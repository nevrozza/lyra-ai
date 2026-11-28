use std::{
    io,
    path::{Path, PathBuf},
    process::Output,
    sync::Arc,
};

use axum::{Json, extract::State, http::StatusCode};
use tokio::process::Command;

use super::{App, Result, canonical_folder};

pub(super) async fn pick(State(app): State<Arc<App>>) -> Result<Json<Option<PathBuf>>> {
    let _dialog = app.folder_picker.try_lock().map_err(|_| {
        (
            StatusCode::CONFLICT,
            "Диалог выбора папки уже открыт.".into(),
        )
    })?;
    let folder = choose().await.map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Не удалось открыть диалог выбора папки. {error}"),
        )
    })?;
    folder
        .as_deref()
        .map(canonical_folder)
        .transpose()
        .map(Json)
}

#[cfg(windows)]
async fn choose() -> io::Result<Option<PathBuf>> {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

    let system = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
    let powershell = PathBuf::from(system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -TypeDefinition @'
{}
'@
[Console]::Write([LyraFolderPicker]::Choose())
"#,
        include_str!("FolderPicker.cs")
    );
    let output = Command::new(powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-Command",
            &script,
        ])
        .output()
        .await?;
    selected(output, false)
}

#[cfg(target_os = "macos")]
async fn choose() -> io::Result<Option<PathBuf>> {
    let script = r#"
try
    return POSIX path of (choose folder with prompt "Lyra: choose a project folder")
on error number -128
    return ""
end try
"#;
    let output = Command::new("osascript")
        .kill_on_drop(true)
        .args(["-e", script])
        .output()
        .await?;
    selected(output, false)
}

#[cfg(not(any(windows, target_os = "macos")))]
async fn choose() -> io::Result<Option<PathBuf>> {
    let output = match Command::new("zenity")
        .kill_on_drop(true)
        .args(["--file-selection", "--directory", "--title=Lyra"])
        .output()
        .await
    {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Command::new("kdialog")
                .kill_on_drop(true)
                .args(["--title", "Lyra", "--getexistingdirectory"])
                .arg(std::env::var_os("HOME").unwrap_or_else(|| "/".into()))
                .output()
                .await?
        }
        result => result?,
    };
    selected(output, true)
}

// Resolve the host account's actual Desktop without a model, interactive
// dialog, user-supplied command, or assumptions about the directory name.
#[cfg(windows)]
pub(super) async fn desktop() -> io::Result<PathBuf> {
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    let system = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("SystemRoot is unavailable"))?;
    let powershell = PathBuf::from(system).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let output = Command::new(powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            r#"$ErrorActionPreference = 'Stop'; [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); [Console]::Write([Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory))"#])
        .output().await?;
    desktop_output(output)
}

#[cfg(target_os = "macos")]
pub(super) async fn desktop() -> io::Result<PathBuf> {
    let output = Command::new("/usr/bin/osascript")
        .kill_on_drop(true)
        .args(["-e", "POSIX path of (path to desktop folder)"])
        .output().await?;
    desktop_output(output)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) async fn desktop() -> io::Result<PathBuf> {
    let output = Command::new("xdg-user-dir")
        .kill_on_drop(true)
        .arg("DESKTOP")
        .output().await?;
    let path = desktop_output(output)?;
    // XDG uses HOME when Desktop is disabled. Do not silently authorize
    // the entire home directory as a Desktop workspace.
    if let Some(home) = std::env::var_os("HOME")
        && path.canonicalize().ok().zip(PathBuf::from(home).canonicalize().ok())
            .is_some_and(|(path, home)| path == home)
    {
        return Err(io::Error::other("Desktop is not configured; provide the existing parent directory explicitly"));
    }
    Ok(path)
}

fn desktop_output(output: Output) -> io::Result<PathBuf> {
    let path = selected(output, false)?
        .ok_or_else(|| io::Error::other("Desktop is not configured"))?;
    if !path.is_absolute() {
        return Err(io::Error::other("Desktop lookup did not return an absolute path"));
    }
    Ok(path)
}

fn selected(output: Output, cancel_exit: bool) -> io::Result<Option<PathBuf>> {
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        if cancel_exit && output.status.code() == Some(1) && error.trim().is_empty() {
            return Ok(None);
        }
        return Err(io::Error::other(format!(
            "{} {}",
            output.status,
            error.trim()
        )));
    }
    let path = std::str::from_utf8(&output.stdout).map_err(io::Error::other)?;
    let path = path
        .strip_suffix('\n')
        .map_or(path, |line| line.strip_suffix('\r').unwrap_or(line));
    Ok((!path.is_empty()).then(|| Path::new(path).to_path_buf()))
}
