use std::{
    io,
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

const PAGE_BYTES: usize = 3_500;

#[derive(Deserialize)]
struct Query {
    view: View,
    path: Option<PathBuf>,
    #[serde(default)]
    offset: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum View {
    Changed,
    Untracked,
    Diff,
}

pub(super) async fn read(workspace: &Path, request: &str) -> Value {
    let query = match serde_json::from_str::<Query>(request) {
        Ok(query)
            if query.offset <= 16 * 1024 * 1024
                && query.path.as_ref().is_none_or(|path| {
                    !path.as_os_str().is_empty()
                        && path
                            .components()
                            .all(|part| matches!(part, Component::Normal(_)))
                })
                && (!matches!(query.view, View::Diff) || query.path.is_some()) =>
        {
            query
        }
        _ => {
            return json!({"status": "error", "message": "Give a Git view, a relative file path for diff, and a valid byte offset."});
        }
    };
    match timeout(Duration::from_secs(15), page(workspace, query)).await {
        Ok(Ok((output, next_offset))) => {
            json!({"status": "ok", "output": output, "next_offset": next_offset})
        }
        Ok(Err(error)) => {
            json!({"status": "error", "message": format!("Could not read the Git diff: {error}")})
        }
        Err(_) => json!({"status": "error", "message": "Reading the Git diff timed out."}),
    }
}

async fn page(workspace: &Path, query: Query) -> io::Result<(String, Option<u64>)> {
    let mut command = Command::new("git");
    command
        .current_dir(workspace)
        .args([
            "--no-pager",
            "--literal-pathspecs",
            "-c",
            "core.fsmonitor=false",
        ])
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    match query.view {
        View::Untracked => {
            command.args(["ls-files", "--others", "--exclude-standard", "--"]);
        }
        View::Changed | View::Diff => {
            command.args([
                "diff",
                "--relative",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
            ]);
            if matches!(query.view, View::Changed) {
                command.arg("--name-status");
            }
            command.args(["HEAD", "--"]);
        }
    }
    if let Some(path) = query.path {
        command.arg(path);
    }
    let mut child = command.spawn()?;
    let mut output = child.stdout.take().expect("piped Git output");
    tokio::io::copy(
        &mut (&mut output).take(query.offset),
        &mut tokio::io::sink(),
    )
    .await?;
    let mut bytes = Vec::new();
    (&mut output)
        .take((PAGE_BYTES + 4) as u64)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() > PAGE_BYTES {
        let mut end = PAGE_BYTES;
        while end > 0 && bytes[end] & 0b1100_0000 == 0b1000_0000 {
            end -= 1;
        }
        let end = if end == 0 { PAGE_BYTES } else { end };
        return Ok((
            String::from_utf8_lossy(&bytes[..end]).into_owned(),
            Some(query.offset + end as u64),
        ));
    }
    if !child.wait().await?.success() {
        return Err(io::Error::other(
            "Git did not return a diff. Check that the folder has a repository and a HEAD commit.",
        ));
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), None))
}
