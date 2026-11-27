use std::{collections::BTreeSet, sync::Mutex, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{benchmark::Store, ollama};

pub const PRESETS: [u32; 7] = [2048, 4096, 8192, 16384, 32768, 65536, 131072];
const ALIAS_PREFIX: &str = "lyra-context/model:";
static PROFILES: Mutex<()> = Mutex::new(());

pub fn is_alias(model: &str) -> bool {
    profile_base(model).is_some()
}

pub fn profile_base(model: &str) -> Option<&str> {
    let tag = model.strip_prefix(ALIAS_PREFIX)?;
    let (hash, size) = tag
        .split_once('-')
        .map_or((tag, None), |(hash, size)| (hash, Some(size)));
    (hash.len() == 16
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && size.is_none_or(|size| {
            size.bytes().all(|byte| byte.is_ascii_digit())
                && size.parse::<u32>().is_ok_and(|size| size > 0)
        }))
    .then(|| &model[..ALIAS_PREFIX.len() + hash.len()])
}

pub fn profile(model: &str, tokens: u32) -> String {
    format!("{}-{tokens}", alias(model))
}

pub fn alias(model: &str) -> String {
    // FNV-1a gives one stable, short Ollama identifier per source model across Lyra builds.
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
    let legacy = alias(model);
    let alias = profile(model, tokens);
    if let Some(loaded) = ollama::running_models().and_then(|models| {
        models.into_iter().find(|entry| {
            (entry.name == legacy || entry.name == alias)
                && entry.context_length == Some(u64::from(tokens))
        })
    }) {
        return Ok(loaded.name);
    }
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
                .filter(|source| {
                    !is_alias(source)
                        && profile_base(profile).is_some_and(|base| base == alias(source))
                })
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
    // A separate estimate for auto sizing, including hybrid/SWA cache layouts.
    pub auto_kv_bytes_per_token: Option<u64>,
    pub auto_fixed_cache_bytes: Option<u64>,
    pub configured_context_tokens: Option<u64>,
    pub max_context_tokens: Option<u64>,
    pub thinking: Value,
    pub thinking_options: Vec<ollama::Think>,
    pub capabilities: Option<Vec<String>>,
}

impl Estimate {
    pub(crate) fn supports_agent_tools(&self) -> Option<bool> {
        self.capabilities.as_ref().map(|capabilities| {
            capabilities.iter().any(|item| item == "completion")
                && capabilities.iter().any(|item| item == "tools")
        })
    }
}

pub(crate) fn validate_window(
    model: &str,
    tokens: u32,
    estimate: Option<&Estimate>,
) -> Result<(), String> {
    if let Some(maximum) = estimate.and_then(|info| info.max_context_tokens)
        && u64::from(tokens) > maximum
    {
        return Err(format!(
            "{model} supports up to {maximum} context tokens, requested {tokens}."
        ));
    }
    Ok(())
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
    let mut thinking_options = Vec::new();
    if let Some(values) = info.thinking["values"].as_array() {
        for value in values {
            let option = match value {
                Value::Bool(true) => ollama::Think::Enabled(true),
                Value::String(level) if !level.trim().is_empty() => {
                    ollama::Think::Level(level.clone())
                }
                _ => continue,
            };
            if !thinking_options.contains(&option) {
                thinking_options.push(option);
            }
        }
    } else if info
        .capabilities
        .as_ref()
        .is_some_and(|items| items.iter().any(|item| item == "thinking"))
    {
        thinking_options.push(ollama::Think::Enabled(true));
    }
    if !thinking_options.is_empty() {
        thinking_options.push(ollama::Think::Enabled(false));
    }
    let max_context_tokens = info.model_info["general.architecture"]
        .as_str()
        .and_then(|architecture| info.model_info[format!("{architecture}.context_length")].as_u64())
        .filter(|&tokens| tokens > 0);
    let auto_cache = auto_cache_estimate(&info.model_info).or_else(|| {
        // Non-verbose /api/show can omit per-layer GGUF arrays. Fetch them
        // only when the compact hybrid metadata did not suffice.
        let arch = info.model_info["general.architecture"].as_str()?;
        if !matches!(arch, "gemma4" | "qwen35" | "qwen35moe") { return None; }
        let detailed = ollama::agent(Duration::from_secs(8))
            .post("http://127.0.0.1:11434/api/show")
            .send_json(json!({"model": model, "verbose": true})).ok()?
            .into_json::<Show>().ok()?;
        auto_cache_estimate(&detailed.model_info)
    });
    Some(Estimate {
        auto_kv_bytes_per_token: auto_cache.map(|cache| cache.0),
        auto_fixed_cache_bytes: auto_cache.map(|cache| cache.1),
        max_context_tokens,
        bytes_per_token_f16: estimate_f16(&info.model_info),
        configured_context_tokens,
        thinking: info.thinking,
        thinking_options,
        capabilities: info.capabilities,
    })
}

// FP16 upper estimate for ordinary attention, and metadata-based estimates
// for supported hybrid architectures. Unknown layouts are not guessed.
fn auto_cache_estimate(info: &Value) -> Option<(u64, u64)> {
    if let Some(bytes) = estimate_f16(info) {
        return Some((bytes, 0));
    }
    let architecture = info["general.architecture"].as_str()?;
    let field = |key: &str| &info[format!("{architecture}.{key}")];
    let number = |key: &str| field(key).as_u64().filter(|&value| value > 0);
    let layers = number("block_count")?;
    let pair = |key: &str, value: &str| {
        number(key)?.checked_add(number(value)?)?.checked_mul(2)
    };
    match architecture {
        "qwen35" | "qwen35moe" => {
            let interval = number("full_attention_interval")?;
            let bytes = pair("attention.key_length", "attention.value_length")?
                .checked_mul(number("attention.head_count_kv")?)?
                .checked_mul(layers.div_ceil(interval))?;
            // Recurrent state and non-KV allocations are covered by the
            // additional per-slot workspace reserve in the sizing function.
            Some((bytes, 0))
        }
        "gemma4" => {
            let pattern = field("attention.sliding_window_pattern").as_array()?;
            let window = number("attention.sliding_window")?;
            let heads = field("attention.head_count_kv");
            let global = pair("attention.key_length", "attention.value_length")?;
            let sliding = pair("attention.key_length_swa", "attention.value_length_swa")?;
            if pattern.len() as u64 != layers { return None; }
            let mut linear = 0u64;
            let mut fixed = 0u64;
            for (index, local) in pattern.iter().enumerate() {
                let kv = heads.as_u64().or_else(|| heads.as_array()?.get(index)?.as_u64())
                    .filter(|&heads| heads > 0)?;
                if local.as_bool()? {
                    // Include extra cells for prefill/cache padding. This
                    // assumes Ollama uses a bounded sliding-window cache.
                    fixed = fixed.checked_add(sliding.checked_mul(kv)?
                        .checked_mul(window.checked_add(1024)?)?)?;
                } else {
                    linear = linear.checked_add(global.checked_mul(kv)?)?;
                }
            }
            (linear > 0).then_some((linear, fixed))
        }
        _ => None,
    }
}

// Largest aligned context fitting estimated weights, cache, and compute
// overhead on one GPU. Do not impose a fixed upper preset; model metadata
// provides the ceiling. Round down to 1024-token boundaries for profile reuse.
pub(crate) fn fit_window(
    available: u64, weights: u64, bytes_per_token: u64, fixed_cache: u64,
    maximum: u64, slots: usize, model_slots: usize,
) -> Option<u32> {
    if weights == 0 || bytes_per_token == 0 || maximum == 0 || slots == 0 || model_slots == 0 {
        return None;
    }
    let slots = u64::try_from(slots).ok()?;
    let models = u64::try_from(model_slots).ok()?;
    // Reserve display/driver headroom before dividing among distinct runners.
    let available = available.saturating_sub(256 << 20) / models;
    // The same runner shares weights but needs one cache per request slot.
    let per_slot_workspace = 256u64 << 20;
    let fits = |tokens: u64| {
        let cache = bytes_per_token.saturating_mul(tokens).saturating_add(fixed_cache)
            .saturating_mul(slots);
        let base = weights.saturating_add(cache);
        let compute = (512u64 << 20).max(base.div_ceil(10))
            .saturating_add(per_slot_workspace.saturating_mul(slots));
        base.saturating_add(compute) <= available
    };
    let maximum = maximum.min(u64::from(u32::MAX));
    // Avoid auto-selecting a tiny window that cannot hold an agent's system prompt.
    let minimum = maximum.min(4096);
    if !fits(minimum) { return None; }
    // Monotone binary search, then align down to avoid creating a profile
    // for every tiny telemetry fluctuation.
    let (mut low, mut high) = (minimum, maximum);
    while low < high {
        let mid = low + (high - low).div_ceil(2);
        if fits(mid) { low = mid; } else { high = mid - 1; }
    }
    let selected = if low == maximum { low } else { low / 1024 * 1024 };
    u32::try_from(selected.max(minimum)).ok()
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
        let beta_8k = profile("team/beta:q4", 8192);
        let beta_16k = profile("team/beta:q4", 16384);
        let unrelated = alias("unrelated");
        let installed = vec![
            "registry.ollama.ai/library/Alpha:latest".into(),
            "team/beta:q8".into(),
            alpha.clone(),
            beta.clone(),
            beta_8k.clone(),
            beta_16k.clone(),
            unrelated,
        ];
        assert_eq!(profile_base(&beta_8k), Some(beta.as_str()));
        assert_eq!(
            orphan_profiles(&installed, &known),
            BTreeSet::from([beta, beta_8k, beta_16k])
        );
        assert!(orphan_profiles(&installed, &[]).is_empty());
        assert!(is_alias(&alpha));
    }

    #[test]
    fn auto_window_uses_available_memory_and_model_maximum() {
        let gib = 1u64 << 30;
        let fit = |free, slots, models| fit_window(free, 5*gib, 65_536, 0, 262_144, slots, models);
        let single = fit(12*gib, 1, 1).unwrap();
        let parallel = fit(12*gib, 2, 1).unwrap();
        assert!(single > parallel);
        assert!(single > 32_768);
        assert!(fit(10*gib, 1, 1).unwrap() < single);
        assert!(fit(12*gib, 1, 2).is_none_or(|window| window < parallel));
        assert!(fit(10*gib, 1, 2).is_none());
        assert_eq!(fit_window(12*gib, 5*gib, 65_536, 0, 8192, 1, 1), Some(8192));
        assert_eq!(fit_window(4*gib, 5*gib, 65_536, 0, 262_144, 1, 1), None);
    }

    #[test]
    fn hybrid_cache_uses_model_metadata() {
        let qwen = json!({
            "general.architecture":"qwen35", "qwen35.block_count":32,
            "qwen35.full_attention_interval":4, "qwen35.attention.head_count_kv":4,
            "qwen35.attention.key_length":256, "qwen35.attention.value_length":256
        });
        assert_eq!(auto_cache_estimate(&qwen), Some((32768, 0)));
        let gemma = json!({
            "general.architecture":"gemma4", "gemma4.block_count":2,
            "gemma4.attention.sliding_window_pattern":[true,false],
            "gemma4.attention.sliding_window":1024,
            "gemma4.attention.head_count_kv":[8,1],
            "gemma4.attention.key_length":512, "gemma4.attention.value_length":512,
            "gemma4.attention.key_length_swa":256, "gemma4.attention.value_length_swa":256
        });
        assert_eq!(auto_cache_estimate(&gemma), Some((2048, 16_777_216)));
        assert_eq!(auto_cache_estimate(&json!({"general.architecture":"unknown"})), None);
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
