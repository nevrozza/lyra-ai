use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{context, ollama};

const PROMPT: &str = "Explain how a hash table works to a beginner. Cover hashing, collisions, and resizing. Give a short example and then describe the time complexity.";
const TOKEN_LIMIT: u32 = 128;
const SEED: u32 = 42;

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preparing,
    CheckingPi,
    LoadingAndPrefill,
    Prefill,
    Generating,
    MeasuringMemory,
    Saving,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    description: String,
    pi_check: Option<bool>,
    pub context_tokens: Option<u32>,
    pub latest: Option<Measurement>,
    last_cold_load: Option<ColdLoad>,
    average_run: Option<TimingAverage>,
    average_launch: Option<TimingAverage>,
}

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
struct TimingAverage {
    samples: u64,
    mean_ms: f64,
}

impl TimingAverage {
    fn with_sample(self, milliseconds: f64) -> Self {
        let samples = self.samples + 1;
        Self {
            samples,
            mean_ms: self.mean_ms + (milliseconds - self.mean_ms) / samples as f64,
        }
    }
}

impl Record {
    fn average_run(&self) -> Option<TimingAverage> {
        // Earlier files only retained the latest measurement.
        self.average_run.or_else(|| {
            self.latest
                .as_ref()
                .map(|latest| TimingAverage::default().with_sample(latest.total_ms))
        })
    }

    fn average_launch(&self) -> Option<TimingAverage> {
        self.average_launch.or_else(|| {
            self.last_cold_load
                .as_ref()
                .map(|latest| TimingAverage::default().with_sample(latest.load_ms))
        })
    }
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
    #[serde(default)]
    measured_context_tokens: Option<u64>,
    #[serde(default)]
    model_ram_bytes: Option<u64>,
    #[serde(default)]
    model_vram_bytes: Option<u64>,
    #[serde(default)]
    token_memory_bytes_f16: Option<u64>,
    #[serde(default)]
    context_memory_bytes_f16: Option<u64>,
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

impl Measurement {
    pub fn auto_context_tokens(&self) -> Option<u64> {
        self.context_tokens.is_none().then_some(self.measured_context_tokens).flatten()
    }
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
        let mut record = self.read()?.remove(model).unwrap_or_default();
        record.average_run = record.average_run();
        record.average_launch = record.average_launch();
        Ok(record)
    }

    pub fn summaries(&self, names: &[String]) -> io::Result<Vec<(Option<f64>, Option<f64>)>> {
        let records = self.read()?;
        Ok(names
            .iter()
            .map(|name| {
                records.get(name).map_or((None, None), |record| {
                    (
                        record.average_launch().map(|timing| timing.mean_ms),
                        record.latest.as_ref().and_then(|run| run.generated_tokens_per_second),
                    )
                })
            })
            .collect())
    }

    pub(crate) fn known_models(&self) -> io::Result<Vec<String>> {
        Ok(self.read()?.into_keys().collect())
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

    pub fn save_pi_check(&self, model: String, passed: bool) -> io::Result<()> {
        self.update(|records| {
            records.entry(model).or_default().pi_check = Some(passed);
            true
        })
    }

    pub fn save(&self, model: String, measurement: Measurement) -> io::Result<()> {
        self.update(|records| {
            let entry = records.entry(model).or_default();
            entry.average_run = Some(
                entry
                    .average_run()
                    .unwrap_or_default()
                    .with_sample(measurement.total_ms),
            );
            entry.average_launch = entry.average_launch();
            if !measurement.was_loaded {
                entry.average_launch = Some(
                    entry
                        .average_launch
                        .unwrap_or_default()
                        .with_sample(measurement.load_ms),
                );
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
    load_duration: u64,
    total_duration: u64,
    prompt_eval_count: Option<u64>,
    prompt_eval_cached_count: Option<u64>,
    prompt_eval_duration: Option<u64>,
    eval_count: u64,
    eval_duration: u64,
}

pub fn measure(
    model: &str,
    context_tokens: Option<u32>,
    mut on_stage: impl FnMut(Stage),
) -> Result<Measurement, String> {
    let backend = context::backend_model(model, context_tokens)?;
    let loaded = ollama::running_models().ok_or("Не удалось проверить память Ollama.")?;
    let resident = loaded
        .iter()
        .find(|entry| ollama::same_model(&entry.name, &backend));
    let was_loaded = resident.is_some_and(|entry| {
        context_tokens
            .zip(entry.context_length)
            .is_none_or(|(expected, actual)| u64::from(expected) == actual)
    });
    let mut request = json!({
        "model": backend,
        "prompt": PROMPT,
        "stream": true,
        "options": { "num_predict": TOKEN_LIMIT, "seed": SEED, "temperature": 0.0 }
    });
    if let Some(tokens) = context_tokens {
        request["options"]["num_ctx"] = json!(tokens);
    }
    if let Some(keep_alive) = retention(resident) {
        request["keep_alive"] = keep_alive;
    }
    on_stage(if was_loaded {
        Stage::Prefill
    } else {
        Stage::LoadingAndPrefill
    });
    let response = ollama::agent(Duration::from_secs(300))
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
        })?;

    let response = read_generation(BufReader::new(response.into_reader()), &mut on_stage)?;
    on_stage(Stage::MeasuringMemory);
    let measured_model = ollama::running_models().and_then(|models| {
        models
            .into_iter()
            .find(|entry| ollama::same_model(&entry.name, &backend))
    });
    if resident.is_none()
        && let Err(error) = ollama::unload_profile(&backend)
    {
        eprintln!("Could not unload benchmark model {backend}: {error}");
    }
    if response.eval_count == 0 {
        return Err("Ollama не сгенерировала токены для замера.".into());
    }
    let prompt_tokens = response.prompt_eval_count.unwrap_or_default();
    let prompt_duration = response.prompt_eval_duration.unwrap_or_default();
    let measured_context_tokens = measured_model
        .as_ref()
        .and_then(|entry| entry.context_length);
    let context_capacity = measured_context_tokens.or(context_tokens.map(u64::from));
    let token_count = prompt_tokens.saturating_add(response.eval_count);
    let occupied_tokens =
        context_capacity.map_or(token_count, |capacity| capacity.min(token_count));
    let bytes_per_token = context::cache_bytes_per_token(model);
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
        measured_context_tokens,
        model_ram_bytes: measured_model.as_ref().and_then(ollama::Model::ram_bytes),
        model_vram_bytes: measured_model.as_ref().and_then(|entry| entry.size_vram),
        token_memory_bytes_f16: bytes_per_token
            .and_then(|bytes| bytes.checked_mul(occupied_tokens)),
        context_memory_bytes_f16: context_capacity
            .zip(bytes_per_token)
            .and_then(|(tokens, bytes)| bytes.checked_mul(tokens)),
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

fn read_generation(
    reader: impl BufRead,
    mut on_stage: impl FnMut(Stage),
) -> Result<Generation, String> {
    let mut started_generating = false;
    for line in reader.lines() {
        let line = line.map_err(|error| format!("Не удалось прочитать замеры Ollama: {error}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let chunk: Value = serde_json::from_str(&line)
            .map_err(|error| format!("Не удалось прочитать замеры Ollama: {error}"))?;
        if let Some(error) = chunk["error"].as_str() {
            return Err(format!("Ollama: {error}"));
        }
        if chunk["done"] == true {
            return serde_json::from_value(chunk)
                .map_err(|error| format!("Не удалось прочитать итоговые замеры Ollama: {error}"));
        }
        if !started_generating
            && ["response", "thinking"]
                .iter()
                .any(|field| chunk[*field].as_str().is_some_and(|text| !text.is_empty()))
        {
            started_generating = true;
            on_stage(Stage::Generating);
        }
    }
    Err("Ollama завершила поток без итоговых замеров.".into())
}

fn retention(resident: Option<&ollama::Model>) -> Option<Value> {
    let Some(model) = resident else {
        // Leave time to sample /api/ps before unloading the cold benchmark.
        return Some(json!("10s"));
    };
    model.remaining_keep_alive()
}
