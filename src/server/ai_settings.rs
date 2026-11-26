use std::sync::LazyLock;

use serde::Deserialize;

#[derive(Deserialize)]
pub(super) struct AiSettings {
    pub(super) minimum_output_tokens: u32,
    pub(super) default_output_tokens: u32,
    pub(super) maximum_output_tokens: u32,
    pub(super) orchestrator_instructions: String,
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
        !settings.orchestrator_instructions.trim().is_empty(),
        "В ai-settings.yaml нужны инструкции оркестратора"
    );
    settings
});
