use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    sync::{Mutex, mpsc},
    thread,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{context, ollama, telemetry};

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
    pub(crate) pi_check: Option<bool>,
    pub context_tokens: Option<u32>,
    pub(crate) think: Option<ollama::Think>,
    pub latest: Option<Measurement>,
    pub memory_profile: Option<MemoryProfile>,
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
    ollama_total_bytes: Option<u64>,
    #[serde(default)]
    gpu_memory: Vec<GpuMeasurement>,
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

// GPU memory is device-wide, not a per-process counter. A cold isolated
// run supplies a useful allocation delta; a warm run does not.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct GpuMeasurement {
    uuid: String,
    name: String,
    total_bytes: u64,
    baseline_bytes: u64,
    sampled_peak_bytes: u64,
    final_bytes: Option<u64>,
    samples: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ProfileIdentity {
    digest: String,
    ollama_version: String,
    gpu_uuid: String,
    driver_version: String,
    gpu_total_bytes: u64,
    // These are Lyra's visible environment settings, not an introspection
    // of an independently started daemon. See README.
    environment: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
pub struct MemoryProfile {
    identity: ProfileIdentity,
    points: Vec<Measurement>,
    stopped_reason: Option<String>,
}

fn environment() -> BTreeMap<String, String> {
    ["OLLAMA_NUM_PARALLEL", "OLLAMA_KV_CACHE_TYPE", "OLLAMA_FLASH_ATTENTION",
     "OLLAMA_GPU_OVERHEAD", "OLLAMA_CONTEXT_LENGTH", "CUDA_VISIBLE_DEVICES"]
        .into_iter().map(|key| (key.to_owned(), std::env::var(key).unwrap_or_default())).collect()
}

fn identity(digest: String, version: String, gpus: &[telemetry::Gpu]) -> Option<ProfileIdentity> {
    let [gpu] = gpus else { return None };
    if digest.is_empty() || version.is_empty() || gpu.uuid.is_empty() || gpu.driver_version.is_empty() {
        return None;
    }
    Some(ProfileIdentity {
        digest, ollama_version: version, gpu_uuid: gpu.uuid.clone(),
        driver_version: gpu.driver_version.clone(), gpu_total_bytes: gpu.vram.as_ref()?.total,
        environment: environment(),
    })
}

impl MemoryProfile {
    pub(crate) fn matches(&self, metrics: &telemetry::Metrics, model: &str) -> bool {
        let current = metrics.model_digests.as_ref().and_then(|digests| digests.get(model))
            .zip(metrics.ollama_version.as_ref())
            .and_then(|(digest, version)| identity(digest.clone(), version.clone(), &metrics.gpus));
        current.as_ref() == Some(&self.identity)
    }

    // Only use measured windows; no extrapolation beyond a real successful
    // run and no partial CPU offload advertised as a GPU fit.
    pub(crate) fn fit(&self, available: u64, weights: u64, maximum: u64) -> Option<u32> {
        self.points.iter().filter_map(|point| {
            let tokens = point.context_tokens?;
            if u64::from(tokens) > maximum { return None; }
            let bytes = point.verified_gpu_bytes(weights)?;
            let reserve = (512u64 << 20).max(bytes / 10);
            (bytes.saturating_add(reserve) <= available).then_some(tokens)
        }).max()
    }
}

impl Measurement {
    fn verified_gpu_bytes(&self, weights: u64) -> Option<u64> {
        if self.was_loaded || weights == 0 || self.context_tokens.map(u64::from) != self.measured_context_tokens
            || self.measured_context_tokens.is_none() { return None; }
        let total = self.ollama_total_bytes?;
        let vram = self.model_vram_bytes?;
        // Ollama can underreport both total and VRAM for hybrid runners.
        // Require its full-GPU signal AND independent physical memory evidence.
        if total == 0 || vram.saturating_mul(100) < total.saturating_mul(95) { return None; }
        let [gpu] = self.gpu_memory.as_slice() else { return None };
        let physical = gpu.sampled_peak_bytes.saturating_sub(gpu.baseline_bytes);
        let final_delta = gpu.final_bytes?.saturating_sub(gpu.baseline_bytes);
        if gpu.samples < 2 || physical < weights.saturating_mul(95) / 100
            || final_delta < weights.saturating_mul(90) / 100 { return None; }
        Some(physical.max(vram))
    }

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

    pub(crate) fn set_thinking(
        &self,
        model: String,
        think: Option<ollama::Think>,
    ) -> io::Result<()> {
        self.update(|records| {
            let record = records.entry(model).or_default();
            let changed = record.think != think;
            record.think = think;
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

    pub fn save_profile(&self, model: String, profile: MemoryProfile) -> io::Result<()> {
        self.update(|records| {
            records.entry(model).or_default().memory_profile = Some(profile);
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
    crate::data_directory().join("model-benchmarks.json")
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

fn gpu_snapshot() -> Vec<telemetry::Gpu> {
    tokio::runtime::Builder::new_current_thread().enable_all().build()
        .map(|runtime| runtime.block_on(telemetry::nvidia())).unwrap_or_default()
}

struct GpuSampler {
    stop: mpsc::Sender<()>,
    thread: Option<thread::JoinHandle<Vec<GpuMeasurement>>>,
}

impl GpuSampler {
    fn start() -> Self {
        let baseline = gpu_snapshot();
        let (stop, receiver) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut rows: Vec<_> = baseline.into_iter().filter_map(|gpu| {
                let mem = gpu.vram?;
                Some(GpuMeasurement { uuid: gpu.uuid, name: gpu.name, total_bytes: mem.total,
                    baseline_bytes: mem.used, sampled_peak_bytes: mem.used, final_bytes: None, samples: 0 })
            }).collect();
            let sample = |rows: &mut Vec<GpuMeasurement>, final_sample: bool| {
                for gpu in gpu_snapshot() {
                    if let Some(row) = rows.iter_mut().find(|row| row.uuid == gpu.uuid)
                        && let Some(mem) = gpu.vram
                        && mem.total == row.total_bytes
                    {
                        row.sampled_peak_bytes = row.sampled_peak_bytes.max(mem.used);
                        row.samples = row.samples.saturating_add(1);
                        if final_sample { row.final_bytes = Some(mem.used); }
                    }
                }
            };
            if rows.is_empty() { return rows; }
            loop {
                match receiver.recv_timeout(Duration::from_millis(200)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => sample(&mut rows, false),
                    _ => { sample(&mut rows, true); break; }
                }
            }
            rows
        });
        Self { stop, thread: Some(thread) }
    }

    fn finish(mut self) -> Vec<GpuMeasurement> {
        let _ = self.stop.send(());
        self.thread.take().and_then(|thread| thread.join().ok()).unwrap_or_default()
    }
}

impl Drop for GpuSampler {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

fn profile_windows(maximum: u64) -> Vec<u32> {
    let maximum = maximum.min(u64::from(u32::MAX)) as u32;
    if maximum == 0 { return Vec::new(); }
    let mut windows = Vec::new();
    let mut next = 4096u32.min(maximum);
    loop {
        windows.push(next);
        if next == maximum { break; }
        next = next.saturating_mul(2).min(maximum);
    }
    windows
}

pub fn measure_profile(model: &str, mut on_stage: impl FnMut(Stage)) -> Result<MemoryProfile, String> {
    let estimate = context::estimate(model).ok_or("Model metadata unavailable")?;
    let maximum = estimate.max_context_tokens.ok_or("Model context ceiling unknown")?;
    let catalog = ollama::model_catalog_details().ok_or("Ollama catalog unavailable")?;
    let (weights, digest) = catalog.get(model).ok_or("Model not found in catalog")?;
    let weights = weights.ok_or("Model weight size unavailable")?;
    let digest = digest.clone().ok_or("Model digest unavailable")?;
    let version = ollama::server_version().ok_or("Ollama version unavailable")?;
    let identity = identity(digest, version, &gpu_snapshot())
        .ok_or("Full profiling currently requires exactly one measurable NVIDIA GPU")?;
    let loaded = ollama::running_models().ok_or("Cannot inspect loaded models")?;
    let alias = context::alias(model);
    let same = |entry: &ollama::Model| ollama::same_model(&entry.name, model)
        || context::profile_base(&entry.name) == Some(alias.as_str());
    if loaded.iter().any(|entry| !same(entry) || entry.is_pinned()) {
        return Err("Unload other Ollama models and unpin this model before full profiling".into());
    }
    if loaded.iter().any(same) { ollama::change_model(model, ollama::ModelAction::Unload, None)?; }
    let mut profile = MemoryProfile { identity, points: Vec::new(), stopped_reason: None };
    for tokens in profile_windows(maximum) {
        let before = ollama::running_models().ok_or("Cannot verify cold benchmark")?;
        if !before.is_empty() {
            profile.stopped_reason = Some("Another model remains loaded; cold isolation lost".into());
            break;
        }
        match measure(model, Some(tokens), &mut on_stage) {
            Ok(point) => {
                let fits = point.verified_gpu_bytes(weights).is_some();
                profile.points.push(point);
                if !fits {
                    profile.stopped_reason = Some(format!("At {tokens} tokens full GPU residency or physical sampling could not be verified; larger windows were not attempted"));
                    break;
                }
            }
            Err(error) => {
                profile.stopped_reason = Some(format!("{tokens} tokens: {error}"));
                break;
            }
        }
    }
    Ok(profile)
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
    let sampler = GpuSampler::start();
    let generation = (|| {
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

        read_generation(BufReader::new(response.into_reader()), &mut on_stage)
    })();
    on_stage(Stage::MeasuringMemory);
    let measured_model = ollama::running_models().and_then(|models| {
        models
            .into_iter()
            .find(|entry| ollama::same_model(&entry.name, &backend))
    });
    let gpu_memory = sampler.finish();
    if resident.is_none()
        && let Err(error) = ollama::unload_profile(&backend)
    {
        eprintln!("Could not unload benchmark model {backend}: {error}");
    }
    let response = generation?;
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
        ollama_total_bytes: measured_model.as_ref().and_then(|entry| entry.size),
        gpu_memory,
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

#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn windows_follow_model_ceiling() {
        assert_eq!(profile_windows(20_000), vec![4096,8192,16384,20_000]);
        assert_eq!(profile_windows(2048), vec![2048]);
        assert!(profile_windows(0).is_empty());
    }
}
