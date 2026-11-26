use std::collections::BTreeMap;

use serde::Serialize;

use crate::{benchmark, ollama::ModelAction};

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
}

impl Snapshot {
    pub fn new(model: Option<String>) -> Self {
        Self {
            model,
            closed: false,
            delegated_model: None,
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

    pub fn begin_delegation(&mut self, model: String) -> bool {
        if self.busy() {
            return false;
        }
        self.delegated_model = Some(model);
        true
    }

    pub fn finish_delegation(&mut self, succeeded: bool) {
        if let Some(model) = self.delegated_model.take().filter(|_| succeeded) {
            self.record_model_action(model, Some(AiAction::Request));
        }
        self.metrics_revision += 1;
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
}
