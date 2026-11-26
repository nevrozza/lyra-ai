use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub(super) struct AiSettings {
    pub(super) minimum_output_tokens: u32,
    pub(super) default_output_tokens: u32,
    pub(super) maximum_output_tokens: u32,
    pub(super) parallel_project_tasks: usize,
    pub(super) parallel_project_models: usize,
    #[serde(default)]
    pub(super) project_models: ProjectModels,
    pub(super) orchestrator_instructions: String,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ProjectModels {
    pub(super) implement: Option<String>,
    pub(super) fix: Option<String>,
    pub(super) review: Option<String>,
}

pub(super) static AI_SETTINGS: LazyLock<AiSettings> = LazyLock::new(|| {
    let settings: AiSettings = serde_saphyr::from_str(include_str!("../../ai-settings.yaml"))
        .expect("Некорректный ai-settings.yaml");
    assert!(
        settings.minimum_output_tokens > 0
            && (settings.minimum_output_tokens..=settings.maximum_output_tokens)
                .contains(&settings.default_output_tokens),
        "В ai-settings.yaml требуется 0 < minimum_output_tokens ≤ default_output_tokens ≤ maximum_output_tokens"
    );
    assert!(
        (1..=16).contains(&settings.parallel_project_tasks)
            && (1..=settings.parallel_project_tasks).contains(&settings.parallel_project_models),
        "В ai-settings.yaml требуется 1 ≤ parallel_project_models ≤ parallel_project_tasks ≤ 16"
    );
    assert!(
        !settings.orchestrator_instructions.trim().is_empty(),
        "В ai-settings.yaml нужны инструкции оркестратора"
    );
    settings
});
