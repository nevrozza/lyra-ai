use super::Server;
use crate::server::commands::process_tree::Tree;
use serde_json::{Value, json};
use std::{collections::BTreeMap, process::Stdio, time::Duration};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader}, process::{Child, Command}};

const VERSION: &str = "2025-03-26";
const MAX_RESPONSE: usize = 1024 * 1024;
const MAX_TOOLS: usize = 500;

fn expand(value: &str) -> Result<String, String> {
    let mut result = String::new();
    let mut rest = value;
    while let Some(index) = rest.find("${") {
        result.push_str(&rest[..index]);
        let tail = &rest[index + 2..];
        let end = tail.find('}').ok_or("Invalid environment variable reference")?;
        let name = &tail[..end];
        if name.is_empty() { return Err("Invalid environment variable reference".into()); }
        result.push_str(&std::env::var(name).map_err(|_| format!("Missing environment variable: {name}"))?);
        rest = &tail[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

fn expanded(values: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    values.iter().map(|(key, value)| Ok((key.clone(), expand(value)?))).collect()
}

fn prepare(command: &mut Command) {
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
}

fn spawn_stdio(server: &Server) -> Result<Child, String> {
    let program = server.command.as_deref().ok_or("Missing command")?;
    let env = expanded(&server.env)?;
    let spawn = |program: &str| {
        let mut command = Command::new(program);
        command.args(&server.args).envs(&env);
        if let Some(cwd) = &server.cwd { command.current_dir(cwd); }
        prepare(&mut command);
        command.spawn()
    };
    let child = spawn(program);
    #[cfg(windows)]
    let child = if child.as_ref().is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        && std::path::Path::new(program).extension().is_none()
    {
        spawn(&format!("{program}.cmd"))
    } else { child };
    child.map_err(|_| "Could not start MCP server process; check command and working directory".into())
}

struct StdioConnection {
    input: tokio::process::ChildStdin,
    output: BufReader<tokio::process::ChildStdout>,
}

impl StdioConnection {
    async fn send(&mut self, value: Value) -> Result<(), String> {
        self.input.write_all(format!("{value}\n").as_bytes()).await.map_err(|_| "MCP server input closed".into())
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<Value, String> {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})).await?;
        loop {
            let mut line = Vec::new();
            let length = (&mut self.output).take((MAX_RESPONSE + 1) as u64).read_until(b'\n', &mut line).await
                .map_err(|_| "Could not read MCP server response")?;
            if length == 0 { return Err("MCP server closed before responding".into()); }
            if length > MAX_RESPONSE { return Err("MCP response exceeds the probe limit".into()); }
            let Ok(message) = serde_json::from_slice::<Value>(&line) else { continue };
            if message["id"].as_u64() == Some(id) && message.get("method").is_none() {
                if message.get("error").is_some() { return Err(format!("MCP {method} failed")); }
                return message.get("result").cloned().ok_or_else(|| "Invalid MCP result".into());
            }
            // Servers may ping while discovering tools. Reject other client
            // requests without executing sampling, elicitation or tool calls.
            if let Some(request_id) = message.get("id").filter(|_| message.get("method").is_some()) {
                let reply = if message["method"] == "ping" {
                    json!({"jsonrpc":"2.0","id":request_id,"result":{}})
                } else {
                    json!({"jsonrpc":"2.0","id":request_id,"error":{"code":-32601,"message":"Unsupported probe client request"}})
                };
                self.send(reply).await?;
            }
        }
    }
}

async fn stdio(server: &Server) -> Result<Vec<String>, String> {
    let mut child = spawn_stdio(server)?;
    let _tree = Tree::attach(&child).map_err(|_| "Could not supervise MCP server process")?;
    let mut connection = StdioConnection {
        input: child.stdin.take().expect("piped stdin"),
        output: BufReader::new(child.stdout.take().expect("piped stdout")),
    };
    let init = connection.request(1, "initialize", json!({
        "protocolVersion":VERSION,"capabilities":{},"clientInfo":{"name":"lyra-probe","version":"1"}
    })).await?;
    if !matches!(init["protocolVersion"].as_str(), Some("2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25")) {
        return Err("Unsupported MCP protocol version".into());
    }
    connection.send(json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
    if init["capabilities"].get("tools").is_none() { return Ok(Vec::new()); }
    let mut tools = Vec::new();
    let mut cursor = None::<String>;
    for id in 2..=101 {
        let params = cursor.as_ref().map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
        let result = connection.request(id, "tools/list", params).await?;
        let page = result["tools"].as_array().ok_or("Invalid tools/list result")?;
        for tool in page {
            tools.push(tool["name"].as_str().ok_or("Invalid MCP tool name")?.to_owned());
            if tools.len() > MAX_TOOLS { return Err("MCP server exceeds probe tool limit".into()); }
        }
        match result["nextCursor"].as_str() {
            Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_owned()),
            Some(_) => return Err("Repeated MCP pagination cursor".into()),
            None => return Ok(tools),
        }
    }
    Err("MCP server exceeds probe pagination limit".into())
}

async fn http(server: &Server) -> Result<Vec<String>, String> {
    // Node provides HTTPS and streaming fetch without a new Rust TLS dependency.
    // Pass credentials only via stdin, never CLI arguments or diagnostic output.
    let headers = expanded(&server.headers)?;
    let payload = json!({"url":server.url,"headers":headers});
    let mut command = Command::new("node");
    command.args(["--input-type=module", "-e", include_str!("probe-http.mjs")]);
    prepare(&mut command);
    let mut child = command.spawn().map_err(|_| "HTTP MCP probe requires Node.js on PATH")?;
    let _tree = Tree::attach(&child).map_err(|_| "Could not supervise HTTP MCP probe")?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(payload.to_string().as_bytes()).await.map_err(|_| "Could not send probe configuration")?;
    drop(stdin);
    let mut output = Vec::new();
    child.stdout.take().expect("piped stdout").take((MAX_RESPONSE + 1) as u64)
        .read_to_end(&mut output).await.map_err(|_| "Could not read HTTP probe result")?;
    if output.len() > MAX_RESPONSE { return Err("MCP response exceeds probe limit".into()); }
    let _ = child.wait().await;
    let result: Value = serde_json::from_slice(&output).map_err(|_| "HTTP MCP probe failed to return a result")?;
    if let Some(error) = result["error"].as_str() { return Err(error.into()); }
    serde_json::from_value(result["tools"].clone()).map_err(|_| "Invalid HTTP MCP probe result".into())
}

pub(super) async fn test(server: &Server) -> Result<Vec<String>, String> {
    // Bound the whole handshake, not each page separately. Dropping the future
    // kills the child/process tree. No local model or tool execution is needed.
    let seconds = u64::from(server.timeout.unwrap_or(30)).clamp(1, 45);
    tokio::time::timeout(Duration::from_secs(seconds), async {
        if server.url.is_some() { http(server).await } else { stdio(server).await }
    }).await.map_err(|_| "MCP connection test timed out".to_owned())?
}
