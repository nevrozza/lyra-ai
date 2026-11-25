use std::{
    process::Stdio,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::{process::Command, time::timeout};

use crate::context;

#[derive(Default)]
pub struct Cache {
    snapshot: Option<(Instant, Metrics)>,
    previous_cpu: Option<(u64, u64)>,
}

#[derive(Clone, Serialize)]
pub struct Metrics {
    cpu_percent: Option<f64>,
    ram: Option<Memory>,
    gpus: Vec<Gpu>,
    models: Option<Vec<Model>>,
    installed_models: Option<Vec<String>>,
}

#[derive(Clone, Serialize)]
pub(crate) struct Memory {
    pub(crate) used: u64,
    pub(crate) total: u64,
}

#[derive(Clone, Serialize)]
struct Gpu {
    name: String,
    load: Option<u8>,
    vram: Option<Memory>,
    temperature: Option<u8>,
}

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Model {
    pub(crate) name: String,
    size_vram: Option<u64>,
    pub(crate) context_length: Option<u64>,
    pub(crate) expires_at: Option<String>,
}

#[derive(Deserialize)]
struct OllamaPs {
    models: Vec<Model>,
}

impl Metrics {
    pub fn installed_models(&self) -> Option<&[String]> {
        self.installed_models.as_deref()
    }
}

#[derive(Default)]
struct System {
    cpu: Option<(u64, u64)>, // Total and idle ticks, in the same units.
    ram: Option<Memory>,
}

impl Cache {
    pub fn invalidate(&mut self) {
        self.snapshot = None;
    }

    pub async fn sample(&mut self) -> Metrics {
        if let Some((sampled, snapshot)) = &self.snapshot
            && sampled.elapsed() < Duration::from_secs(4)
        {
            return snapshot.clone();
        }

        let local = system();
        let cpu_percent = self.previous_cpu.zip(local.cpu).and_then(
            |((total_before, idle_before), (total, idle))| {
                let total = total.checked_sub(total_before)?;
                let idle = idle.checked_sub(idle_before)?;
                (total > 0).then(|| 100.0 * total.saturating_sub(idle) as f64 / total as f64)
            },
        );
        self.previous_cpu = local.cpu;

        let (models, installed_models, gpus) = tokio::join!(
            tokio::task::spawn_blocking(ollama),
            tokio::task::spawn_blocking(installed_models),
            nvidia(),
        );
        let installed_models = installed_models.ok().flatten();
        let mut models = models.ok().flatten();
        if let (Some(loaded), Some(installed)) = (&mut models, &installed_models) {
            let aliases: std::collections::HashMap<_, _> = installed
                .iter()
                .map(|model| (context::alias(model), model))
                .collect();
            for entry in loaded
                .iter_mut()
                .filter(|entry| context::is_alias(&entry.name))
            {
                if let Some(source) = aliases.get(&entry.name) {
                    entry.name = (*source).clone();
                }
            }
        }
        let metrics = Metrics {
            cpu_percent,
            ram: local.ram,
            gpus,
            models,
            installed_models,
        };
        self.snapshot = Some((Instant::now(), metrics.clone()));
        metrics
    }
}

pub(crate) fn ollama_agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(false)
        .redirects(0)
        .timeout(timeout)
        .build()
}

pub(crate) fn ollama() -> Option<Vec<Model>> {
    Some(
        ollama_agent(Duration::from_millis(800))
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
    #[derive(Deserialize)]
    struct Catalog {
        models: Vec<Entry>,
    }
    #[derive(Deserialize)]
    struct Entry {
        name: Option<String>,
        model: Option<String>,
        remote_host: Option<String>,
    }

    let catalog = ollama_agent(Duration::from_millis(800))
        .get("http://127.0.0.1:11434/api/tags")
        .call()
        .ok()?
        .into_json::<Catalog>()
        .ok()?;
    let mut names: Vec<_> = catalog
        .models
        .into_iter()
        .filter(|entry| entry.remote_host.as_deref().unwrap_or_default().is_empty())
        .filter_map(|entry| entry.name.or(entry.model))
        .filter(|name| {
            !context::is_alias(name)
                && !name.to_ascii_lowercase().ends_with(":cloud")
                && crate::model_name(name).is_ok()
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    Some(names)
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
        return set_keep_alive(&backend, serde_json::json!(-1));
    }
    let alias = context::alias(model);
    let loaded = ollama().ok_or("Ollama недоступна")?;
    let keep_alive = if matches!(action, ModelAction::Unload) {
        serde_json::json!(0)
    } else {
        serde_json::json!("5m")
    };
    for entry in loaded
        .iter()
        .filter(|entry| same_model(&entry.name, model) || entry.name == alias)
    {
        set_keep_alive(&entry.name, keep_alive.clone())?;
    }
    Ok(())
}

fn set_keep_alive(model: &str, keep_alive: serde_json::Value) -> Result<(), String> {
    #[derive(Deserialize)]
    struct Acknowledgement {
        done: bool,
    }

    let acknowledgement = ollama_agent(Duration::from_secs(180))
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

async fn nvidia() -> Vec<Gpu> {
    let mut probe = Command::new("nvidia-smi");
    probe
        .args([
            "--query-gpu=name,utilization.gpu,memory.used,memory.total,temperature.gpu",
            "--format=csv,noheader,nounits",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    probe.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let Ok(Ok(output)) = timeout(Duration::from_secs(2), probe.output()).await else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let [name, load, used, total, temperature] = line
                .split(',')
                .map(str::trim)
                .collect::<Vec<_>>()
                .try_into()
                .ok()?;
            let vram =
                used.parse::<u64>()
                    .ok()
                    .zip(total.parse::<u64>().ok())
                    .map(|(used, total)| Memory {
                        used: used * 1_048_576,
                        total: total * 1_048_576,
                    });
            Some(Gpu {
                name: name.into(),
                load: load.parse().ok(),
                vram,
                temperature: temperature.parse().ok(),
            })
        })
        .collect()
}

#[cfg(windows)]
fn system() -> System {
    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }
    impl FileTime {
        fn ticks(&self) -> u64 {
            (u64::from(self.high) << 32) | u64::from(self.low)
        }
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetSystemTimes(idle: *mut FileTime, kernel: *mut FileTime, user: *mut FileTime) -> i32;
    }

    let (mut idle, mut kernel, mut user) = (
        FileTime::default(),
        FileTime::default(),
        FileTime::default(),
    );
    // The FFI buffers use the Win32 layout and point to initialized, writable values.
    let cpu = (unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0)
        .then(|| (kernel.ticks() + user.ticks(), idle.ticks()));
    System {
        cpu,
        ram: windows_ram(),
    }
}

#[cfg(windows)]
pub(crate) fn windows_ram() -> Option<Memory> {
    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page_file: u64,
        avail_page_file: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended_virtual: u64,
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
    }
    let mut memory = MemoryStatus {
        length: size_of::<MemoryStatus>() as u32,
        ..MemoryStatus::default()
    };
    // Win32 expects the buffer size to be initialized before the call.
    (unsafe { GlobalMemoryStatusEx(&mut memory) } != 0).then(|| Memory {
        used: memory.total_phys.saturating_sub(memory.avail_phys),
        total: memory.total_phys,
    })
}

#[cfg(target_os = "linux")]
fn system() -> System {
    let cpu = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|stats| {
            let ticks: Vec<u64> = stats
                .lines()
                .next()?
                .split_whitespace()
                .skip(1)
                .take(8)
                .flat_map(str::parse)
                .collect();
            Some((ticks.iter().sum(), ticks.get(3)? + ticks.get(4)?))
        });
    let ram = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| {
            let field = |name| {
                info.lines()
                    .find_map(|line| line.strip_prefix(name))
                    .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
            };
            let total = field("MemTotal:")? * 1024;
            Some(Memory {
                used: total.saturating_sub(field("MemAvailable:")? * 1024),
                total,
            })
        });
    System { cpu, ram }
}

#[cfg(target_os = "macos")]
fn system() -> System {
    static HOST: std::sync::OnceLock<libc::mach_port_t> = std::sync::OnceLock::new();
    // Keep the host port for the lifetime of the process. The C calls below fill the supplied buffers.
    #[allow(deprecated)]
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    let mut ticks = std::mem::MaybeUninit::<libc::host_cpu_load_info>::zeroed();
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    let cpu = (unsafe {
        libc::host_statistics(
            host,
            libc::HOST_CPU_LOAD_INFO,
            ticks.as_mut_ptr().cast(),
            &mut count,
        )
    } == 0)
        .then(|| {
            let ticks = unsafe { ticks.assume_init() }.cpu_ticks;
            (
                ticks.into_iter().map(u64::from).sum(),
                u64::from(ticks[libc::CPU_STATE_IDLE as usize]),
            )
        });

    let mut pages = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    let mut total = 0_u64;
    let mut size = size_of::<u64>();
    let ram_status = unsafe {
        libc::sysctlbyname(
            c"hw.memsize".as_ptr(),
            (&mut total as *mut u64).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    let vm_status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            pages.as_mut_ptr().cast(),
            &mut count,
        )
    };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let ram = (ram_status == 0 && vm_status == 0 && page_size > 0).then(|| {
        let pages = unsafe { pages.assume_init() };
        let active = u64::from(pages.active_count)
            + u64::from(pages.wire_count)
            + u64::from(pages.compressor_page_count);
        Memory {
            used: total.min(active * page_size as u64),
            total,
        }
    });
    System { cpu, ram }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn system() -> System {
    System::default()
}
