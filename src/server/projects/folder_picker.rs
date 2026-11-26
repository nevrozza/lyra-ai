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
    let script = r#"
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = 'Lyra: choose a project folder'
$dialog.ShowNewFolderButton = $false
try {
    if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
        [Console]::Write($dialog.SelectedPath)
    }
} finally {
    $dialog.Dispose()
}
"#;
    let output = Command::new(powershell)
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-STA",
            "-Command",
            script,
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
