use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::telemetry;

pub const PRESETS: [u32; 7] = [2048, 4096, 8192, 16384, 32768, 65536, 131072];
const ALIAS_PREFIX: &str = "lyra-context/model:";

pub fn is_alias(model: &str) -> bool {
    model
        .strip_prefix(ALIAS_PREFIX)
        .is_some_and(|tag| tag.len() == 16 && tag.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

pub fn alias(model: &str) -> String {
    // FNV-1a gives one stable, short Ollama tag per source model across Lyra builds.
    let identity = model.strip_suffix(":latest").unwrap_or(model);
    let hash = identity.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte.to_ascii_lowercase())).wrapping_mul(0x100000001b3)
    });
    format!("{ALIAS_PREFIX}{hash:016x}")
}

pub fn backend_model(model: &str, tokens: Option<u32>) -> Result<String, String> {
    let Some(tokens) = tokens else {
        return Ok(model.to_owned());
    };
    // Pi uses the OpenAI-compatible API, where Ollama takes num_ctx from the model.
    let alias = alias(model);
    #[derive(Deserialize)]
    struct Created {
        status: String,
    }
    let result = telemetry::ollama_agent(Duration::from_secs(90))
        .post("http://127.0.0.1:11434/api/create")
        .send_json(json!({
            "model": alias,
            "from": model,
            "parameters": { "num_ctx": tokens },
            "stream": false
        }))
        .map_err(|error| format!("Не удалось настроить контекст в Ollama: {error}"))?
        .into_json::<Created>()
        .map_err(|error| format!("Не удалось прочитать ответ Ollama: {error}"))?;
    if result.status == "success" {
        Ok(alias)
    } else {
        Err(format!("Ollama не подготовила модель: {}", result.status))
    }
}
