use std::{collections::BTreeSet, sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{benchmark::Store, ollama};

pub const PRESETS: [u32; 7] = [2048, 4096, 8192, 16384, 32768, 65536, 131072];
const ALIAS_PREFIX: &str = "lyra-context/model:";
static PROFILES: Mutex<()> = Mutex::new(());

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
    let _profiles = PROFILES.lock().map_err(|error| error.to_string())?;
    let installed =
        ollama::installed_models().ok_or("Не удалось получить список моделей Ollama.")?;
    if !installed.iter().any(|name| same_source(name, model)) {
        return Err(format!("Модель больше не установлена в Ollama: {model}."));
    }
    // Pi uses the OpenAI-compatible API, where Ollama takes num_ctx from the model.
    let alias = alias(model);
    #[derive(Deserialize)]
    struct Created {
        status: String,
    }
    let result = ollama::agent(Duration::from_secs(90))
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

fn same_source(left: &str, right: &str) -> bool {
    fn normalize(name: &str) -> &str {
        let name = name.strip_prefix("registry.ollama.ai/").unwrap_or(name);
        let name = name
            .strip_prefix("library/")
            .filter(|name| !name.contains('/'))
            .unwrap_or(name);
        name.strip_suffix(":latest").unwrap_or(name)
    }
    normalize(left).eq_ignore_ascii_case(normalize(right))
}

fn orphan_profiles(installed: &[String], known: &[String]) -> BTreeSet<String> {
    installed
        .iter()
        .filter(|name| is_alias(name))
        .filter(|profile| {
            let mut sources = known
                .iter()
                .filter(|source| !is_alias(source) && alias(source) == profile.as_str())
                .peekable();
            sources.peek().is_some()
                && sources.all(|source| !installed.iter().any(|name| same_source(name, source)))
        })
        .cloned()
        .collect()
}

pub(crate) fn clean_orphans(store: &Store, previous: BTreeSet<String>) -> BTreeSet<String> {
    let Ok(_profiles) = PROFILES.lock() else {
        return BTreeSet::new();
    };
    let Some(installed) = ollama::model_tag_names() else {
        return BTreeSet::new();
    };
    if !installed.iter().any(|model| is_alias(model)) {
        return BTreeSet::new();
    }
    let Ok(known) = store.known_models() else {
        return BTreeSet::new();
    };
    let mut missing = orphan_profiles(&installed, &known);
    // A profile must be orphaned in two successful, separate Ollama snapshots.
    let confirmed: Vec<_> = missing.intersection(&previous).cloned().collect();
    for profile in confirmed {
        match delete_profile(&profile) {
            Ok(true) => {
                missing.remove(&profile);
            }
            Ok(false) => {}
            Err(error) => eprintln!("Could not remove orphaned Ollama profile {profile}: {error}"),
        }
    }
    missing
}

fn delete_profile(profile: &str) -> Result<bool, String> {
    let is_loaded = || {
        ollama::running_models()
            .map(|models| {
                models
                    .iter()
                    .any(|entry| ollama::same_model(&entry.name, profile))
            })
            .ok_or("could not read Ollama's loaded models")
    };
    if is_loaded()? {
        ollama::unload_profile(profile)?;
        // Ollama can acknowledge an unload before the runner releases the files.
        if is_loaded()? {
            return Ok(false);
        }
    }
    match ollama::agent(Duration::from_secs(4))
        .delete("http://127.0.0.1:11434/api/delete")
        .send_json(json!({ "model": profile }))
    {
        Ok(_) | Err(ureq::Error::Status(404, _)) => Ok(true),
        Err(error) => Err(error.to_string()),
    }
}

pub fn cache_bytes_per_token(model: &str) -> Option<u64> {
    estimate(model)?.bytes_per_token_f16
}

#[derive(Default, Serialize)]
pub(crate) struct Estimate {
    pub bytes_per_token_f16: Option<u64>,
    pub configured_context_tokens: Option<u64>,
    pub thinking: Value,
    pub capabilities: Option<Vec<String>>,
}

pub(crate) fn estimate(model: &str) -> Option<Estimate> {
    #[derive(Deserialize)]
    struct Show {
        #[serde(default)]
        model_info: Value,
        parameters: Option<String>,
        #[serde(default)]
        thinking: Value,
        capabilities: Option<Vec<String>>,
    }
    let info = ollama::agent(Duration::from_secs(2))
        .post("http://127.0.0.1:11434/api/show")
        .send_json(json!({ "model": model, "verbose": false }))
        .ok()?
        .into_json::<Show>()
        .ok()?;
    let configured_context_tokens = info.parameters.as_deref().and_then(|parameters| {
        parameters.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "num_ctx" {
                return None;
            }
            words
                .next()?
                .parse::<u64>()
                .ok()
                .filter(|&tokens| tokens > 0)
        })
    });
    Some(Estimate {
        bytes_per_token_f16: estimate_f16(&info.model_info),
        configured_context_tokens,
        thinking: info.thinking,
        capabilities: info.capabilities,
    })
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
    fn only_selects_known_profiles_with_missing_source_models() {
        let known = vec!["alpha:latest".into(), "team/beta:q4".into()];
        let alpha = alias("alpha");
        let beta = alias("team/beta:q4");
        let unrelated = alias("unrelated");
        let installed = vec![
            "registry.ollama.ai/library/Alpha:latest".into(),
            "team/beta:q8".into(),
            alpha.clone(),
            beta.clone(),
            unrelated,
        ];
        assert_eq!(orphan_profiles(&installed, &known), BTreeSet::from([beta]));
        assert!(orphan_profiles(&installed, &[]).is_empty());
        assert!(is_alias(&alpha));
    }

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
