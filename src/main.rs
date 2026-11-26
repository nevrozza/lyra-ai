mod benchmark;
mod context;
mod ollama;
mod ram;
mod run;
mod server;
mod telemetry;

use std::{path::PathBuf, time::Duration};

use clap::Parser;

#[derive(Parser)]
#[command(about = "Local dashboard for a Pi agent running on Ollama")]
struct Options {
    /// Preferred local model, as shown by `ollama list`.
    #[arg(long, value_parser = model_name)]
    model: Option<String>,
    /// Pi configuration directory managed by the Lyra launcher.
    #[arg(long)]
    pi_agent_dir: Option<PathBuf>,
    /// Local dashboard port.
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Startup log supplied by the Windows launcher.
    #[arg(long)]
    startup_log: Option<PathBuf>,
    /// Path to the JSON file for model descriptions and benchmark results.
    #[arg(long)]
    benchmark_file: Option<PathBuf>,
}

fn model_name(name: &str) -> Result<String, String> {
    if name
        .as_bytes()
        .first()
        .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._:/".contains(&byte))
    {
        return Ok(name.to_owned());
    }
    Err("use the name from `ollama list`, for example qwen3:8b".into())
}

fn data_directory() -> PathBuf {
    use std::env;

    if cfg!(windows) {
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
    }
}

fn main() -> std::io::Result<()> {
    let options = Options::parse();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(server::serve(options));
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}
