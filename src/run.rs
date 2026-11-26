use std::collections::BTreeMap;

use serde::Serialize;

use crate::{benchmark, ollama::ModelAction};

pub(crate) const CHAT_THINKING_LIMIT: usize = 24_000;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AiAction {
    Request,
    Description,
    #[serde(untagged)]
    Model(ModelAction),
}

#[derive(Serialize)]
pub struct Snapshot {
    model: Option<String>,
    closed: bool,
    delegated_model: Option<String>,
    delegations: BTreeMap<u64, Delegation>,
    chat_context: BTreeMap<u64, ContextNotice>,
    #[serde(skip)]
    next_delegation: u64,
    ai_model_actions: BTreeMap<String, AiAction>,
    pinned: Option<bool>,
    model_action: Option<ModelAction>,
    operation_model: Option<String>,
    model_error: Option<String>,
    benchmark_running: bool,
    benchmark_stage: Option<benchmark::Stage>,
    benchmark_revision: u64,
    metrics_revision: u64,
    sessions_revision: u64,
    commands_revision: u64,
    projects_revision: u64,
}

#[derive(Serialize)]
struct Delegation {
    model: String,
    project_session_id: Option<u64>,
    stopping: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    context_notice: Option<ContextNotice>,
    #[serde(skip_serializing_if = "String::is_empty")]
    thinking: String,
    #[serde(skip)]
    project: bool,
    #[serde(skip)]
    exclusive: bool,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextNotice {
    NearLimit,
    Compacting,
    Compacted,
    Full,
}

#[derive(Clone, Copy)]
pub enum DelegationScope {
    Standalone,
    Project { session_id: Option<u64> },
    Queued { session_id: u64, parallel: bool },
}

impl Snapshot {
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            closed: false,
            delegated_model: None,
            delegations: BTreeMap::new(),
            chat_context: BTreeMap::new(),
            next_delegation: 0,
            ai_model_actions: BTreeMap::new(),
            pinned: None,
            model_action: None,
            operation_model: None,
            model_error: None,
            benchmark_running: false,
            benchmark_stage: None,
            benchmark_revision: 0,
            metrics_revision: 0,
            sessions_revision: 0,
            commands_revision: 0,
            projects_revision: 0,
        }
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn closed(&self) -> bool {
        self.closed
    }

    pub fn close(&mut self) {
        self.closed = true;
    }

    pub fn busy(&self) -> bool {
        self.closed
            || self.delegated_model.is_some()
            || self.model_action.is_some()
            || self.benchmark_running
    }

    pub fn select_model(&mut self, model: String) -> bool {
        if self.busy() {
            return false;
        }
        self.model = Some(model);
        self.pinned = None;
        self.model_error = None;
        true
    }

    pub fn begin_delegation(&mut self, model: &str, scope: DelegationScope) -> Option<u64> {
        if self.closed || self.model_action.is_some() || self.benchmark_running {
            return None;
        }
        let (project, project_session_id, exclusive) = match scope {
            DelegationScope::Standalone if self.delegations.is_empty() => (false, None, false),
            DelegationScope::Standalone => return None,
            DelegationScope::Project { session_id } => (true, session_id, false),
            DelegationScope::Queued {
                session_id,
                parallel,
            } => (true, Some(session_id), !parallel),
        };
        if project
            && self.delegations.values().any(|active| {
                exclusive
                    || active.exclusive
                    || !active.project
                    || project_session_id.is_some()
                        && active.project_session_id == project_session_id
            })
        {
            return None;
        }
        self.next_delegation += 1;
        if let Some(session) = project_session_id {
            self.chat_context.remove(&session);
        }
        self.delegated_model.get_or_insert_with(|| model.to_owned());
        self.delegations.insert(
            self.next_delegation,
            Delegation {
                model: model.to_owned(),
                project_session_id,
                stopping: false,
                context_notice: None,
                thinking: String::new(),
                project,
                exclusive,
            },
        );
        Some(self.next_delegation)
    }

    pub fn stop_project_delegation(&mut self, id: u64, session_id: u64) -> bool {
        let Some(active) = self
            .delegations
            .get_mut(&id)
            .filter(|active| active.project_session_id == Some(session_id))
        else {
            return false;
        };
        active.stopping = true;
        true
    }

    pub fn delegation_stopping(&self, id: u64) -> bool {
        self.delegations
            .get(&id)
            .is_some_and(|active| active.stopping)
    }

    pub fn has_delegation(&self, id: u64) -> bool {
        self.delegations.contains_key(&id)
    }

    pub fn finish_delegation(&mut self, id: u64, succeeded: bool) {
        if let Some(active) = self.delegations.remove(&id) {
            self.delegated_model = self
                .delegations
                .values()
                .next()
                .map(|active| active.model.clone());
            if let Some(session) = active.project_session_id {
                if let Some(notice @ (ContextNotice::NearLimit | ContextNotice::Full)) =
                    active.context_notice
                {
                    self.chat_context.insert(session, notice);
                }
                self.projects_changed();
            }
            if succeeded {
                self.record_model_action(active.model, Some(AiAction::Request));
            }
        }
        self.metrics_revision += 1;
    }

    pub fn delegation_context(&mut self, id: u64, notice: ContextNotice) -> bool {
        let Some(active) = self.delegations.get_mut(&id) else {
            return false;
        };
        if active.context_notice == Some(notice) {
            return false;
        }
        active.context_notice = Some(notice);
        true
    }

    pub fn append_chat_thinking(&mut self, id: u64, delta: &str, new_message: bool) -> bool {
        let Some(active) = self
            .delegations
            .get_mut(&id)
            .filter(|active| active.project_session_id.is_some())
        else {
            return false;
        };
        let separator = if new_message && !active.thinking.is_empty() {
            "\n\n"
        } else {
            ""
        };
        let remaining = CHAT_THINKING_LIMIT.saturating_sub(active.thinking.len() + separator.len());
        let end = delta.floor_char_boundary(remaining);
        if end == 0 {
            return false;
        }
        active.thinking.push_str(separator);
        active.thinking.push_str(&delta[..end]);
        true
    }

    pub fn begin_model_action(&mut self, model: String, action: ModelAction) -> bool {
        if self.busy() {
            return false;
        }
        self.model_action = Some(action);
        self.operation_model = Some(model);
        self.model_error = None;
        true
    }

    pub fn finish_model_action(
        &mut self,
        action: ModelAction,
        result: &Result<bool, String>,
        ai_action: Option<AiAction>,
    ) {
        self.model_action = None;
        self.metrics_revision += 1;
        match result {
            Ok(true) => {
                if self.operation_model == self.model && !matches!(action, ModelAction::Load) {
                    self.pinned = Some(matches!(action, ModelAction::Pin));
                }
                if let Some(model) = self.operation_model.clone() {
                    self.record_model_action(model, ai_action);
                }
            }
            Ok(false) => {}
            Err(error) => self.model_error = Some(error.clone()),
        }
    }

    pub fn begin_benchmark(&mut self, model: String) -> bool {
        if self.busy() {
            return false;
        }
        self.benchmark_running = true;
        self.benchmark_stage = Some(benchmark::Stage::Preparing);
        self.operation_model = Some(model);
        self.model_error = None;
        true
    }

    pub fn set_benchmark_stage(&mut self, stage: benchmark::Stage) {
        self.benchmark_stage = Some(stage);
    }

    pub fn finish_benchmark(&mut self, result: Result<(), String>) {
        self.benchmark_running = false;
        self.benchmark_stage = None;
        self.metrics_revision += 1;
        match result {
            Ok(()) => {
                if let Some(model) = self.operation_model.clone() {
                    self.record_model_action(model, None);
                }
                self.model_record_changed();
            }
            Err(error) => self.model_error = Some(error),
        }
    }

    pub fn model_record_changed(&mut self) {
        self.benchmark_revision += 1;
    }

    pub fn record_model_action(&mut self, model: String, ai_action: Option<AiAction>) {
        if let Some(action) = ai_action {
            self.ai_model_actions.insert(model, action);
        } else {
            self.ai_model_actions.remove(&model);
        }
    }

    pub fn sessions_changed(&mut self) {
        self.sessions_revision += 1;
    }

    pub fn commands_changed(&mut self) {
        self.commands_revision += 1;
    }

    pub fn projects_changed(&mut self) {
        self.projects_revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{DelegationScope, Snapshot};

    #[test]
    fn independent_project_chats_run_in_parallel_without_overlapping_one_session() {
        let mut snapshot = Snapshot::new(None);
        let chat = |id| DelegationScope::Project {
            session_id: Some(id),
        };
        let first = snapshot.begin_delegation("one", chat(10)).unwrap();
        assert!(snapshot.begin_delegation("two", chat(10)).is_none());
        assert!(
            snapshot
                .begin_delegation("two", DelegationScope::Standalone)
                .is_none()
        );
        let others: Vec<_> = (20..25)
            .map(|id| snapshot.begin_delegation("two", chat(id)).unwrap())
            .collect();
        assert!(!snapshot.stop_project_delegation(first, 20));
        assert!(snapshot.stop_project_delegation(first, 10));
        assert!(snapshot.delegation_stopping(first));
        assert!(!snapshot.delegation_stopping(others[0]));

        snapshot.finish_delegation(first, true);
        assert!(snapshot.busy());
        assert_eq!(snapshot.delegated_model.as_deref(), Some("two"));
        let resumed = snapshot.begin_delegation("one", chat(10)).unwrap();
        assert!(!snapshot.stop_project_delegation(first, 10));
        assert!(!snapshot.delegation_stopping(resumed));
        for id in others {
            snapshot.finish_delegation(id, false);
        }
        snapshot.finish_delegation(resumed, true);
        assert!(!snapshot.busy());
    }
}
