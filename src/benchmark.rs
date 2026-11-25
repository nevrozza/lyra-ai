use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, Write},
    path::PathBuf,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{context, telemetry};

const PROMPT: &str = "Explain how a hash table works to a beginner. Cover hashing, collisions, and resizing. Give a short example and then describe the time complexity.";
const TOKEN_LIMIT: u32 = 128;
const SEED: u32 = 42;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    description: String,
    pub context_tokens: Option<u32>,
    latest: Option<Measurement>,
    last_cold_load: Option<ColdLoad>,
}

#[derive(Serialize, Deserialize)]
struct ColdLoad {
    measured_at: String,
    load_ms: f64,
    #[serde(default)]
    context_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct Measurement {
    measured_at: String,
    was_loaded: bool,
    #[serde(default)]
    context_tokens: Option<u32>,
    load_ms: f64,
    total_ms: f64,
    prompt_ms: f64,
    generation_ms: f64,
    prompt_tokens: u64,
    cached_prompt_tokens: Option<u64>,
    generated_tokens: u64,
    prompt_tokens_per_second: Option<f64>,
    generated_tokens_per_second: Option<f64>,
    prompt: String,
    token_limit: u32,
    seed: u32,
    temperature: f64,
}

type Records = BTreeMap<String, Record>;

pub struct Store {
    path: PathBuf,
    registered: Mutex<BTreeSet<String>>,
}

impl Store {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path: path.unwrap_or_else(default_path),
            registered: Mutex::default(),
        }
    }

    pub fn register(&self, names: &[String]) -> io::Result<()> {
        let mut registered = self.registered.lock().expect("benchmark registry lock");
        if names.iter().all(|name| registered.contains(name)) {
            return Ok(());
        }
        self.update(|records| {
            let before = records.len();
            for name in names {
                records.entry(name.clone()).or_default();
            }
            records.len() != before
        })?;
        registered.extend(names.iter().cloned());
        Ok(())
    }

    pub fn get(&self, model: &str) -> io::Result<Record> {
        Ok(self.read()?.remove(model).unwrap_or_default())
    }

    pub fn describe(&self, model: String, description: String) -> io::Result<()> {
        self.update(|records| {
            records.entry(model).or_default().description = description;
            true
        })
    }

    pub fn set_context(&self, model: String, tokens: Option<u32>) -> io::Result<()> {
        self.update(|records| {
            let record = records.entry(model).or_default();
            let changed = record.context_tokens != tokens;
            record.context_tokens = tokens;
            changed
        })
    }

    pub fn save(&self, model: String, measurement: Measurement) -> io::Result<()> {
        self.update(|records| {
            let entry = records.entry(model).or_default();
            if !measurement.was_loaded {
                entry.last_cold_load = Some(ColdLoad {
                    measured_at: measurement.measured_at.clone(),
                    load_ms: measurement.load_ms,
                    context_tokens: measurement.context_tokens,
                });
            }
            entry.latest = Some(measurement);
            true
        })
    }

    fn read(&self) -> io::Result<Records> {
        match fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Records::new()),
            Err(error) => Err(error),
        }
    }

    fn update(&self, edit: impl FnOnce(&mut Records) -> bool) -> io::Result<()> {
        if let Some(directory) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(directory)?;
        }
        // Lock a separate file so replacing the JSON keeps the lock valid for other Lyra processes.
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.path.with_extension("json.lock"))?;
        lock.lock()?;

        let mut records = self.read()?;
        if edit(&mut records) {
            let temp = self.path.with_extension("json.tmp");
            let mut file = fs::File::create(&temp)?;
            serde_json::to_writer_pretty(&mut file, &records).map_err(io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            drop(file);
            fs::rename(temp, &self.path)?;
        }
        Ok(())
    }
}

fn default_path() -> PathBuf {
    let directory = if cfg!(windows) {
        env::var_os("LOCALAPPDATA")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(env::temp_dir)
            .join("LyraAI")
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
            .unwrap_or_else(env::temp_dir)
            .join("lyra-ai")
    };
    directory.join("model-benchmarks.json")
}

#[derive(Deserialize)]
struct Generation {
    created_at: String,
    done: bool,
    load_duration: u64,
    total_duration: u64,
    prompt_eval_count: Option<u64>,
    prompt_eval_cached_count: Option<u64>,
    prompt_eval_duration: Option<u64>,
    eval_count: u64,
    eval_duration: u64,
}

pub fn measure(model: &str, context_tokens: Option<u32>) -> Result<Measurement, String> {
    let backend = context::backend_model(model, context_tokens)?;
    let loaded = telemetry::ollama().ok_or("Не удалось проверить память Ollama.")?;
    let resident = loaded
        .iter()
        .find(|entry| telemetry::same_model(&entry.name, &backend));
    let was_loaded = resident.is_some_and(|entry| {
        context_tokens
            .zip(entry.context_length)
            .is_none_or(|(expected, actual)| u64::from(expected) == actual)
    });
    let mut request = json!({
        "model": backend,
        "prompt": PROMPT,
        "stream": false,
        "options": { "num_predict": TOKEN_LIMIT, "seed": SEED, "temperature": 0.0 }
    });
    if let Some(tokens) = context_tokens {
        request["options"]["num_ctx"] = json!(tokens);
    }
    if let Some(keep_alive) = retention(resident) {
        request["keep_alive"] = keep_alive;
    }
    let response = telemetry::ollama_agent(Duration::from_secs(300))
        .post("http://127.0.0.1:11434/api/generate")
        .send_json(request)
        .map_err(|error| match error {
            ureq::Error::Status(status, response) => {
                let reason = response.into_json::<Value>().ok();
                let detail = reason
                    .as_ref()
                    .and_then(|body| body["error"].as_str())
                    .unwrap_or("Проверьте, поддерживает ли модель генерацию текста.");
                format!("Ollama (HTTP {status}): {detail}")
            }
            error => format!("Не удалось выполнить бенчмарк Ollama: {error}"),
        })?
        .into_json::<Generation>()
        .map_err(|error| format!("Не удалось прочитать замеры Ollama: {error}"))?;

    if !response.done || response.eval_count == 0 {
        return Err("Ollama не сгенерировала токены для замера.".into());
    }
    let prompt_tokens = response.prompt_eval_count.unwrap_or_default();
    let prompt_duration = response.prompt_eval_duration.unwrap_or_default();
    let uncached =
        prompt_tokens.saturating_sub(response.prompt_eval_cached_count.unwrap_or_default());
    let ms = |nanos| nanos as f64 / 1_000_000.0;
    let rate = |tokens, nanos| {
        (tokens > 0 && nanos > 0).then(|| tokens as f64 * 1_000_000_000.0 / nanos as f64)
    };
    Ok(Measurement {
        measured_at: response.created_at,
        was_loaded,
        context_tokens,
        load_ms: ms(response.load_duration),
        total_ms: ms(response.total_duration),
        prompt_ms: ms(prompt_duration),
        generation_ms: ms(response.eval_duration),
        prompt_tokens,
        cached_prompt_tokens: response.prompt_eval_cached_count,
        generated_tokens: response.eval_count,
        prompt_tokens_per_second: rate(uncached, prompt_duration),
        generated_tokens_per_second: rate(response.eval_count, response.eval_duration),
        prompt: PROMPT.into(),
        token_limit: TOKEN_LIMIT,
        seed: SEED,
        temperature: 0.0,
    })
}

fn retention(resident: Option<&telemetry::Model>) -> Option<Value> {
    let Some(model) = resident else {
        // A cold benchmark returns the model to its previous unloaded state.
        return Some(json!(0));
    };
    let expires = DateTime::parse_from_rfc3339(model.expires_at.as_deref()?).ok()?;
    let remaining = expires.signed_duration_since(DateTime::<Utc>::from(SystemTime::now()));
    if remaining.num_days() > 36_500 {
        return Some(json!(-1));
    }
    Some(json!(format!("{}ms", remaining.num_milliseconds().max(1))))
}
