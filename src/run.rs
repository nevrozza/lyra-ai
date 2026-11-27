use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

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
    chat_errors: BTreeSet<u64>,
    chat_completed: BTreeSet<u64>,
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
    skills_revision: u64,
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
    history_revision: u64,
    #[serde(skip)]
    project: bool,
    #[serde(skip)]
    exclusive: bool,
    #[serde(skip)]
    writes: Vec<PathBuf>,
    #[serde(skip)]
    parallel: bool,
    #[serde(skip)]
    context_tokens: Option<u32>,
}

#[derive(Clone, Copy, Deserialize, PartialEq, Serialize)]
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
    Project {
        session_id: Option<u64>,
        work: bool,
    },
    Queued {
        session_id: u64,
        parallel: bool,
        context_tokens: Option<u32>,
        max_tasks: usize,
        max_models: usize,
    },
}

pub fn path_key(path: &Path) -> PathBuf {
    // Be conservative on case- and normalization-insensitive volumes.
    let folded = path.to_string_lossy().to_uppercase();
    PathBuf::from(
        icu_normalizer::ComposingNormalizerBorrowed::new_nfc()
            .normalize(&folded)
            .as_ref(),
    )
}

pub fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

impl Snapshot {
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            closed: false,
            delegated_model: None,
            delegations: BTreeMap::new(),
            chat_context: BTreeMap::new(),
            chat_errors: BTreeSet::new(),
            chat_completed: BTreeSet::new(),
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
            skills_revision: 0,
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
        let (project, project_session_id, exclusive, parallel, context_tokens) = match scope {
            DelegationScope::Standalone if self.delegations.is_empty() => {
                (false, None, false, false, None)
            }
            DelegationScope::Standalone => return None,
            DelegationScope::Project { session_id, work } => {
                (true, session_id, work, false, None)
            }
            DelegationScope::Queued {
                session_id,
                parallel,
                context_tokens,
                max_tasks,
                max_models,
            } => {
                let exclusive = !parallel;
                if !exclusive {
                    let mut models = BTreeSet::from([(model, context_tokens)]);
                    let mut active_tasks = 0;
                    for active in self
                        .delegations
                        .values()
                        .filter(|active| active.parallel)
                    {
                        active_tasks += 1;
                        models.insert((active.model.as_str(), active.context_tokens));
                    }
                    if active_tasks >= max_tasks || models.len() > max_models {
                        return None;
                    }
                }
                (true, Some(session_id), exclusive, parallel, context_tokens)
            }
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
            self.chat_errors.remove(&session);
            self.chat_completed.remove(&session);
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
                history_revision: 0,
                project,
                exclusive,
                writes: Vec::new(),
                parallel,
                context_tokens,
            },
        );
        Some(self.next_delegation)
    }

    /// Called before a guarded mutation. Locks are held until the delegation ends.
    /// Never wait for another owner: returning a conflict avoids agent deadlocks.
    pub fn claim_file(&mut self, id: u64, path: &Path) -> Result<bool, String> {
        let key = path_key(path);
        let own = self.delegations.get(&id).ok_or("FILE_LOCKED: run is no longer active")?;
        if self.closed || own.stopping {
            return Err("FILE_LOCKED: run is stopping".into());
        }
        if own.writes.contains(&key) { return Ok(false); }
        for (other_id, active) in &self.delegations {
            if *other_id != id && active.writes.iter().any(|other| paths_overlap(other, &key)) {
                let owner = active.project_session_id.map_or_else(
                    || format!("run #{other_id}"), |session| format!("task/chat #{session}"));
                return Err(format!("FILE_LOCKED: {} is held by {owner}. Continue independent work or report a dependency. Do not wait in a loop or bypass through run_command.", path.display()));
            }
        }
        self.delegations.get_mut(&id).expect("active delegation").writes.push(key);
        Ok(true)
    }

    pub fn release_file(&mut self, id: u64, path: &Path) -> bool {
        let Some(active) = self.delegations.get_mut(&id) else { return false; };
        let key = path_key(path);
        let before = active.writes.len();
        active.writes.retain(|other| *other != key);
        before != active.writes.len()
    }

    pub fn stop_project_delegation(&mut self, id: u64, session_id: u64) -> bool {
        let Some(active) = self
            .delegations
            .get_mut(&id)
            .filter(|active| active.project_session_id == Some(session_id) && !active.stopping)
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

    pub fn project_chat_active(&self, session: u64) -> bool {
        self.delegations.values().any(|active| active.project_session_id == Some(session))
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
                if succeeded && !active.stopping {
                    self.chat_completed.insert(session);
                } else if !active.stopping {
                    self.chat_errors.insert(session);
                }
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

    pub fn project_context_notice(&self, session_id: u64) -> Option<ContextNotice> {
        self.delegations
            .values()
            .find(|active| active.project_session_id == Some(session_id))?
            .context_notice
    }

    pub fn context_notice(&self, id: u64) -> Option<ContextNotice> {
        self.delegations.get(&id)?.context_notice
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

    pub fn chat_history_changed(&mut self, id: u64) -> bool {
        let Some(active) = self
            .delegations
            .get_mut(&id)
            .filter(|active| active.project_session_id.is_some())
        else {
            return false;
        };
        active.history_revision += 1;
        true
    }

    pub fn clear_chat_thinking(&mut self, id: u64) -> bool {
        let Some(active) = self.delegations.get_mut(&id) else {
            return false;
        };
        if active.thinking.is_empty() {
            return false;
        }
        active.thinking.clear();
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

    pub fn skills_changed(&mut self) {
        self.skills_revision += 1;
    }

    pub fn projects_changed(&mut self) {
        self.projects_revision += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{DelegationScope, Snapshot};

    #[test]
    fn chat_history_revisions_are_scoped_to_the_delegation() {
        let mut snapshot = Snapshot::new(None);
        let id = snapshot
            .begin_delegation("one", DelegationScope::Project {
                session_id: Some(1),
                work: true,
            })
            .unwrap();
        assert_eq!(snapshot.delegations[&id].history_revision, 0);
        assert!(snapshot.chat_history_changed(id));
        assert!(snapshot.chat_history_changed(id));
        assert_eq!(snapshot.delegations[&id].history_revision, 2);
        assert!(!snapshot.chat_history_changed(id + 1));
    }

    #[test]
    fn dynamic_file_locks_and_parallel_limits() {
        let mut snapshot = Snapshot::new(None);
        let scoped = |session_id| DelegationScope::Queued {
            session_id, parallel: true, context_tokens: None, max_tasks: 2, max_models: 1,
        };
        let a = snapshot.begin_delegation("one", scoped(1)).unwrap();
        assert!(snapshot.begin_delegation("two", scoped(2)).is_none());
        let b = snapshot.begin_delegation("one", scoped(2)).unwrap();
        assert!(snapshot.begin_delegation("one", scoped(3)).is_none());
        let file = super::Path::new("src/café.rs");
        let alias = super::Path::new("src/CAFE\u{301}.rs");
        assert_eq!(snapshot.claim_file(a, file), Ok(true));
        assert_eq!(snapshot.claim_file(a, file), Ok(false));
        assert!(snapshot.claim_file(b, alias).unwrap_err().contains("FILE_LOCKED"));
        assert!(snapshot.claim_file(b, super::Path::new("src/other.rs")).is_ok());
        assert!(snapshot.begin_delegation("one", DelegationScope::Project {
            session_id: Some(10), work: true,
        }).is_none());
        snapshot.finish_delegation(a, true);
        assert_eq!(snapshot.claim_file(b, alias), Ok(true));
        snapshot.finish_delegation(b, true);
    }

    #[test]
    fn independent_project_chats_run_in_parallel_without_overlapping_one_session() {
        let mut snapshot = Snapshot::new(None);
        let chat = |id| DelegationScope::Project {
            session_id: Some(id),
            work: false,
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
