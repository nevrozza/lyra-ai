use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

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

pub fn cache_bytes_per_token(model: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct Show {
        model_info: Value,
    }
    let info = telemetry::ollama_agent(Duration::from_secs(2))
        .post("http://127.0.0.1:11434/api/show")
        .send_json(json!({ "model": model, "verbose": false }))
        .ok()?
        .into_json::<Show>()
        .ok()?;
    estimate_f16(&info.model_info)
}

fn estimate_f16(info: &Value) -> Option<u64> {
    let architecture = info["general.architecture"].as_str()?;
    if !matches!(
        architecture,
        "llama" | "mistral" | "qwen2" | "qwen3" | "qwen3moe"
    ) {
        return None;
    }
    let field = |key| &info[format!("{architecture}.{key}")];
    let number = |key| field(key).as_u64().filter(|&value| value > 0);
    let sliding = field("attention.sliding_window");
    if !sliding.is_null() && sliding.as_u64() != Some(0) {
        return None;
    }

    let dimension = |key| {
        if !field(key).is_null() {
            return number(key);
        }
        let heads = number("attention.head_count")?;
        let embedding = number("embedding_length")?;
        (embedding >= heads && embedding % heads == 0).then_some(embedding / heads)
    };
    // Standard attention stores one key and one value per KV head, layer and token.
    let head_bytes = dimension("attention.key_length")?
        .checked_add(dimension("attention.value_length")?)?
        .checked_mul(2)?; // f16
    let kv_heads = if field("attention.head_count_kv").is_null() {
        number("attention.head_count")?
    } else {
        number("attention.head_count_kv")?
    };
    head_bytes
        .checked_mul(kv_heads)?
        .checked_mul(number("block_count")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_grouped_attention_and_explicit_key_value_sizes() {
        let mut info = json!({
            "general.architecture": "qwen2",
            "qwen2.block_count": 28,
            "qwen2.embedding_length": 3584,
            "qwen2.attention.head_count": 28,
            "qwen2.attention.head_count_kv": 4,
        });
        assert_eq!(estimate_f16(&info), Some(57_344));
        info["qwen2.attention.key_length"] = json!(256);
        info["qwen2.attention.value_length"] = json!(128);
        assert_eq!(estimate_f16(&info), Some(86_016));
        info["qwen2.attention.sliding_window"] = json!(4096);
        assert_eq!(estimate_f16(&info), None);
        info["general.architecture"] = json!("qwen35");
        assert_eq!(estimate_f16(&info), None);
    }
}
