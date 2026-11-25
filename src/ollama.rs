use std::{
    collections::BTreeMap,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::context;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Model {
    pub(crate) name: String,
    size: Option<u64>,
    pub(crate) size_vram: Option<u64>,
    #[serde(skip_deserializing)]
    pub(crate) context_profile: bool,
    pub(crate) context_length: Option<u64>,
    pub(crate) expires_at: Option<String>,
}

impl Model {
    pub(crate) fn ram_bytes(&self) -> Option<u64> {
        Some(self.size?.saturating_sub(self.size_vram?))
    }

    pub(crate) fn remaining_keep_alive(&self) -> Option<Value> {
        let expires = DateTime::parse_from_rfc3339(self.expires_at.as_deref()?).ok()?;
        let remaining = expires.signed_duration_since(DateTime::<Utc>::from(SystemTime::now()));
        if remaining.num_days() > 36_500 {
            return Some(json!(-1));
        }
        Some(json!(format!("{}ms", remaining.num_milliseconds().max(1))))
    }
}

#[derive(Deserialize)]
pub(crate) struct ChatReply {
    message: ChatMessage,
    done: bool,
    done_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

impl ChatReply {
    pub(crate) fn into_text(self) -> Result<(String, bool), String> {
        if !self.done {
            return Err("Ollama returned an incomplete reply".into());
        }
        let truncated = self.done_reason.as_deref() == Some("length");
        if self.message.content.trim().is_empty() {
            return Err(if truncated {
                "The model reached its output limit before returning text"
            } else {
                "The model returned no text"
            }
            .into());
        }
        Ok((self.message.content, truncated))
    }
}

pub(crate) fn chat(
    model: &str,
    prompt: &str,
    max_output_tokens: u32,
    context_tokens: Option<u32>,
) -> Result<ChatReply, String> {
    let backend = context::backend_model(model, context_tokens)?;
    let resident = running_models().and_then(|models| {
        models
            .into_iter()
            .find(|entry| same_model(&entry.name, &backend))
    });
    let mut request = json!({
        "model": backend,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "options": {"num_predict": max_output_tokens}
    });
    if let Some(tokens) = context_tokens {
        request["options"]["num_ctx"] = json!(tokens);
    }
    if let Some(keep_alive) = resident.and_then(|entry| entry.remaining_keep_alive()) {
        request["keep_alive"] = keep_alive;
    }
    agent(Duration::from_secs(120))
        .post("http://127.0.0.1:11434/api/chat")
        .send_json(request)
        .map_err(|error| match error {
            ureq::Error::Status(status, response) => {
                let detail = response.into_json::<Value>().ok().and_then(|body| {
                    body["error"]
                        .as_str()
                        .map(|error| error.chars().take(300).collect::<String>())
                });
                format!(
                    "Ollama HTTP {status}: {}",
                    detail.as_deref().unwrap_or("Chat request failed")
                )
            }
            ureq::Error::Transport(error) => format!("Ollama chat failed: {error}"),
        })?
        .into_json()
        .map_err(|error| format!("Could not read Ollama reply: {error}"))
}

#[derive(Deserialize)]
struct OllamaPs {
    models: Vec<Model>,
}

pub(crate) fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(timeout)
        .build()
}

pub(crate) fn running_models() -> Option<Vec<Model>> {
    Some(
        agent(Duration::from_millis(800))
            .get("http://127.0.0.1:11434/api/ps")
            .call()
            .ok()?
            .into_json::<OllamaPs>()
            .ok()?
            .models,
    )
}

pub(crate) fn same_model(left: &str, right: &str) -> bool {
    left == right
        || left.strip_suffix(":latest") == Some(right)
        || right.strip_suffix(":latest") == Some(left)
}

pub fn installed_models() -> Option<Vec<String>> {
    Some(model_catalog()?.into_keys().collect())
}

#[derive(Deserialize)]
struct CatalogEntry {
    name: Option<String>,
    model: Option<String>,
    remote_host: Option<String>,
    size: Option<u64>,
}

pub(crate) fn model_catalog() -> Option<BTreeMap<String, Option<u64>>> {
    let models = model_tags()?
        .into_iter()
        .filter(|entry| entry.remote_host.as_deref().unwrap_or_default().is_empty())
        .filter_map(|entry| {
            Some((
                entry.name.or(entry.model)?,
                entry.size.filter(|&size| size > 0),
            ))
        })
        .filter(|(name, _)| {
            !context::is_alias(name)
                && !name.to_ascii_lowercase().ends_with(":cloud")
                && crate::model_name(name).is_ok()
        })
        .collect();
    Some(models)
}

pub(crate) fn model_tag_names() -> Option<Vec<String>> {
    model_tags()?
        .into_iter()
        .map(|entry| entry.name.or(entry.model).filter(|name| !name.is_empty()))
        .collect()
}

fn model_tags() -> Option<Vec<CatalogEntry>> {
    #[derive(Deserialize)]
    struct Catalog {
        models: Vec<CatalogEntry>,
    }
    Some(
        agent(Duration::from_millis(800))
            .get("http://127.0.0.1:11434/api/tags")
            .call()
            .ok()?
            .into_json::<Catalog>()
            .ok()?
            .models,
    )
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelAction {
    Pin,
    Unpin,
    Unload,
}

pub fn change_model(
    model: &str,
    action: ModelAction,
    context_tokens: Option<u32>,
) -> Result<(), String> {
    if matches!(action, ModelAction::Pin) {
        let backend = context::backend_model(model, context_tokens)?;
        return set_keep_alive(&backend, serde_json::json!(-1), Duration::from_secs(180));
    }
    let alias = context::alias(model);
    let loaded = running_models().ok_or("Ollama недоступна")?;
    let keep_alive = if matches!(action, ModelAction::Unload) {
        serde_json::json!(0)
    } else {
        serde_json::json!("5m")
    };
    for entry in loaded
        .iter()
        .filter(|entry| same_model(&entry.name, model) || entry.name == alias)
    {
        set_keep_alive(&entry.name, keep_alive.clone(), Duration::from_secs(180))?;
    }
    Ok(())
}

pub(crate) fn unload_profile(model: &str) -> Result<(), String> {
    set_keep_alive(model, serde_json::json!(0), Duration::from_secs(4))
}

fn set_keep_alive(
    model: &str,
    keep_alive: serde_json::Value,
    timeout: Duration,
) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Acknowledgement {
        done: bool,
    }

    let acknowledgement = agent(timeout)
        .post("http://127.0.0.1:11434/api/generate")
        .send_json(serde_json::json!({ "model": model, "prompt": "", "stream": false, "keep_alive": keep_alive }))
        .map_err(|error| format!("Ollama отклонила команду: {error}"))?
        .into_json::<Acknowledgement>()
        .map_err(|error| format!("Не удалось прочитать ответ Ollama: {error}"))?;
    if acknowledgement.done {
        Ok(())
    } else {
        Err("Ollama ещё не завершила команду.".into())
    }
}
