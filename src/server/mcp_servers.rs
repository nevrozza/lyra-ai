use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
    sync::Arc,
    hash::{Hash, Hasher},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header::{self, HeaderName}},
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Mutex;

mod probe;

use super::App;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Config {
    mcp_servers: BTreeMap<String, Server>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Server {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    env: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout: Option<u32>,
    #[serde(default = "enabled")]
    enabled: bool,
    #[serde(default)]
    exposure: Exposure,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tool_exposure: BTreeMap<String, Exposure>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Transport {
    Stdio,
    Http,
    StreamableHttp,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Exposure {
    #[default]
    Direct,
    Hidden,
}

fn enabled() -> bool {
    true
}

impl Config {
    fn validate(&self) -> Result<(), String> {
        let mut names = BTreeSet::new();
        for (name, server) in &self.mcp_servers {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
                || !names.insert(name.replace('-', "_").to_ascii_lowercase())
            {
                return Err(format!(
                    "Недопустимое или повторяющееся имя MCP-сервера: {name:?}."
                ));
            }
            server
                .validate()
                .map_err(|error| format!("{name}: {error}"))?;
        }
        Ok(())
    }
}

impl Server {
    fn validate(&self) -> Result<(), &'static str> {
        match (self.command.as_deref(), self.url.as_deref(), self.transport) {
            (Some(command), None, None | Some(Transport::Stdio)) => {
                if command.trim().is_empty()
                    || command.chars().any(char::is_control)
                    || !self.headers.is_empty()
                    || self.args.iter().any(|argument| argument.contains('\0'))
                    || self
                        .cwd
                        .as_ref()
                        .is_some_and(|cwd| cwd.trim().is_empty() || cwd.contains('\0'))
                    || self
                        .env
                        .keys()
                        .any(|key| key.is_empty() || key.contains(['=', '\0']))
                {
                    return Err("Проверьте command, args, cwd и env для stdio.");
                }
            }
            (None, Some(url), None | Some(Transport::Http | Transport::StreamableHttp)) => {
                let uri = url
                    .parse::<Uri>()
                    .map_err(|_| "Укажите абсолютный HTTP(S) URL.")?;
                if !matches!(uri.scheme_str(), Some("http" | "https"))
                    || uri
                        .authority()
                        .is_none_or(|host| host.as_str().contains('@'))
                    || url.contains('#')
                {
                    return Err("Укажите абсолютный HTTP(S) URL без учётных данных или фрагмента.");
                }
                if self.cwd.is_some()
                    || !self.args.is_empty()
                    || !self.env.is_empty()
                    || self
                        .headers
                        .keys()
                        .any(|key| key.parse::<HeaderName>().is_err())
                {
                    return Err(
                        "Проверьте headers для Streamable HTTP. Поля cwd, args и env относятся к stdio.",
                    );
                }
            }
            _ => return Err("Укажите либо command для stdio, либо url для Streamable HTTP."),
        }
        if self.timeout == Some(0) {
            return Err("Таймаут должен быть больше нуля.");
        }
        if self
            .env
            .values()
            .chain(self.headers.values())
            .any(|value| value.trim_start().starts_with('!') || value.contains(['\0', '\r', '\n']))
        {
            return Err("В env и headers используйте обычные значения или ${NAME}.");
        }
        Ok(())
    }
}

pub(super) struct Store(PathBuf, Mutex<()>);

impl Store {
    pub(super) fn new(path: PathBuf) -> Self {
        Self(path, Mutex::new(()))
    }

    async fn load(&self) -> io::Result<Config> {
        match tokio::fs::read(&self.0).await {
            Ok(bytes) => {
                let config: Config = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
                config.validate().map_err(io::Error::other)?;
                Ok(config)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
            Err(error) => Err(error),
        }
    }

    async fn save(&self, config: Config) -> io::Result<Config> {
        let path = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let directory = path.parent().expect("MCP settings directory");
            std::fs::create_dir_all(directory)?;
            let file = tempfile::NamedTempFile::new_in(directory)?;
            serde_json::to_writer_pretty(file.as_file(), &config)?;
            file.as_file().sync_all()?;
            file.persist(path)?;
            Ok(config)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(super) async fn enabled_snapshot(&self) -> io::Result<Option<Vec<u8>>> {
        let mut config = self.load().await?;
        config.mcp_servers.retain(|_, server| server.enabled);
        if config.mcp_servers.is_empty() {
            return Ok(None);
        }
        serde_json::to_vec(&config)
            .map(Some)
            .map_err(io::Error::other)
    }

    async fn update(&self, operation: impl FnOnce(&mut Config) -> Result<Value, Error>) -> Result<Value, Error> {
        let _guard = self.1.lock().await;
        let mut config = self.load().await.map_err(storage_error)?;
        let result = operation(&mut config)?;
        config.validate().map_err(bad_request)?;
        self.save(config).await.map_err(storage_error)?;
        Ok(result)
    }
}

type Error = (StatusCode, String);

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(
            "/settings/mcp-servers",
            get(get_settings).post(save_settings),
        )
        .layer(DefaultBodyLimit::max(64 * 1024))
}

fn require_local(app: &App) -> Result<(), Error> {
    if app.lan_host.is_none() {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        "Подключения MCP доступны в локальном режиме Lyra.".into(),
    ))
}

fn bad_request(message: String) -> Error { (StatusCode::BAD_REQUEST, message) }

fn missing_server(name: &str) -> Error { (StatusCode::NOT_FOUND, format!("MCP server not found: {name}")) }

fn revision(config: &Config) -> String {
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_vec(config).expect("serializable MCP settings").hash(&mut hash);
    format!("\"{:016x}\"", hash.finish())
}

fn response(config: Config) -> (HeaderMap, Json<Config>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, HeaderValue::from_str(&revision(&config)).expect("valid ETag"));
    (headers, Json(config))
}

fn storage_error(_: io::Error) -> Error {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Не удалось прочитать или сохранить настройки MCP.".into(),
    )
}

async fn get_settings(State(app): State<Arc<App>>) -> Result<(HeaderMap, Json<Config>), Error> {
    require_local(&app)?;
    app.mcp_servers.load().await.map(response).map_err(storage_error)
}

async fn save_settings(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(config): Json<Config>,
) -> Result<(HeaderMap, Json<Config>), Error> {
    require_local(&app)?;
    config.validate().map_err(bad_request)?;
    let _guard = app.mcp_servers.1.lock().await;
    if let Some(expected) = headers.get(header::IF_MATCH) {
        let current = app.mcp_servers.load().await.map_err(storage_error)?;
        if expected.to_str().ok() != Some(revision(&current).as_str()) {
            return Err((StatusCode::CONFLICT, "Настройки MCP изменились. Обновите список и повторите изменения.".into()));
        }
    }
    let config = app.mcp_servers.save(config).await.map_err(storage_error)?;
    app.task.send_modify(crate::run::Snapshot::mcp_servers_changed);
    Ok(response(config))
}

// The orchestrator sees configuration metadata only, not credential values,
// arguments, URLs (which may contain tokens), or arbitrary environment data.
fn summary(name: &str, server: &Server) -> Value {
    json!({
        "name": name,
        "type": if server.url.is_some() { "streamable-http" } else { "stdio" },
        "enabled": server.enabled,
        "exposure": server.exposure,
        "toolExposure": server.tool_exposure,
        "timeout": server.timeout,
        "has_env": !server.env.is_empty(),
        "has_headers": !server.headers.is_empty(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveServer {
    name: String,
    server: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NamedServer {
    name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EnableServer {
    name: String,
    enabled: bool,
}

fn upsert(config: &mut Config, input: SaveServer) -> Result<Value, Error> {
    let patch = input.server.as_object().ok_or_else(|| bad_request("server must be an object".into()))?;
    let previous = config.mcp_servers.get(&input.name);
    let mut value = previous.map(|server| serde_json::to_value(server).expect("serializable MCP server"))
        .unwrap_or_else(|| json!({}));
    let fields = value.as_object_mut().expect("server object");
    // Switching transport removes only fields incompatible with the new transport.
    if previous.is_some_and(|server| server.command.is_some()) && patch.get("url").is_some_and(|url| !url.is_null()) && !patch.contains_key("command") {
        for key in ["command", "args", "env", "cwd", "type"] { fields.remove(key); }
    }
    if previous.is_some_and(|server| server.url.is_some()) && patch.get("command").is_some_and(|command| !command.is_null()) && !patch.contains_key("url") {
        for key in ["url", "headers", "type"] { fields.remove(key); }
    }
    for (key, value) in patch {
        // Keep required/defaulted fields valid; optional nulls clear them.
        fields.insert(key.clone(), value.clone());
    }
    let server: Server = serde_json::from_value(value).map_err(|error| bad_request(format!("Invalid MCP server: {error}")))?;
    let result = summary(&input.name, &server);
    config.mcp_servers.insert(input.name, server);
    Ok(result)
}

pub(super) async fn list_for_orchestrator(app: &App) -> Result<Value, Error> {
    require_local(app)?;
    let config = app.mcp_servers.load().await.map_err(storage_error)?;
    Ok(json!({"servers": config.mcp_servers.iter().map(|(name, server)| summary(name, server)).collect::<Vec<_>>(),
        "applies_to": "next Work run"}))
}

pub(super) async fn save_for_orchestrator(app: &App, input: SaveServer) -> Result<Value, Error> {
    require_local(app)?;
    let result = app.mcp_servers.update(|config| upsert(config, input)).await?;
    app.task.send_modify(crate::run::Snapshot::mcp_servers_changed);
    Ok(result)
}

pub(super) async fn enable_for_orchestrator(app: &App, input: EnableServer) -> Result<Value, Error> {
    require_local(app)?;
    let result = app.mcp_servers.update(|config| {
        let server = config.mcp_servers.get_mut(&input.name).ok_or_else(|| missing_server(&input.name))?;
        server.enabled = input.enabled;
        Ok(summary(&input.name, server))
    }).await?;
    app.task.send_modify(crate::run::Snapshot::mcp_servers_changed);
    Ok(result)
}

pub(super) async fn remove_for_orchestrator(app: &App, input: NamedServer) -> Result<Value, Error> {
    require_local(app)?;
    let result = app.mcp_servers.update(|config| {
        let removed = config.mcp_servers.remove(&input.name).is_some();
        Ok(json!({"name": input.name, "removed": removed}))
    }).await?;
    app.task.send_modify(crate::run::Snapshot::mcp_servers_changed);
    Ok(result)
}

pub(super) async fn test_for_orchestrator(app: &App, input: NamedServer) -> Result<Value, Error> {
    require_local(app)?;
    let config = app.mcp_servers.load().await.map_err(storage_error)?;
    let server = config.mcp_servers.get(&input.name).ok_or_else(|| missing_server(&input.name))?;
    let tools = probe::test(server).await.map_err(|message| (StatusCode::BAD_GATEWAY, message))?;
    Ok(json!({"name": input.name, "connected": true, "enabled": server.enabled, "tools": tools,
        "note": "MCP handshake and tools/list succeeded; no tool invoked. Work uses its own Pi connection."}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn stores_servers_and_only_stages_enabled_connections_for_pi() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path().join("mcp-servers.json"));
        assert!(store.enabled_snapshot().await.unwrap().is_none());
        let config: Config = serde_json::from_value(json!({"mcpServers": {
            "local": {"command": "uvx", "args": ["mcp-server-time"]},
            "remote": {"type": "streamable-http", "url": "https://example.com/mcp",
                "headers": {"Authorization": "Bearer ${API_TOKEN}"}, "toolExposure": {"write": "hidden"}},
            "off": {"command": "unused", "enabled": false}
        }})).unwrap();
        config.validate().unwrap();
        store.save(config).await.unwrap();
        let snapshot = store.enabled_snapshot().await.unwrap().unwrap();
        let staged: serde_json::Value = serde_json::from_slice(&snapshot).unwrap();
        let servers = staged["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 2);
        assert_eq!(servers["local"]["exposure"], "direct");
        assert_eq!(servers["remote"]["toolExposure"]["write"], "hidden");
        assert!(!store.load().await.unwrap().mcp_servers["off"].enabled);
    }

    #[test]
    fn orchestrator_patch_preserves_secrets_and_other_servers() {
        let mut config: Config = serde_json::from_value(json!({"mcpServers": {
            "docs": {"url":"https://example.com/mcp", "headers":{"Authorization":"secret"}, "enabled":false},
            "local": {"command":"node"}
        }})).unwrap();
        let result = upsert(&mut config, SaveServer { name:"docs".into(), server:json!({"enabled":true}) }).unwrap();
        assert!(!result.to_string().contains("secret"));
        assert_eq!(config.mcp_servers["docs"].headers["Authorization"], "secret");
        assert!(config.mcp_servers["docs"].enabled);
        assert!(config.mcp_servers.contains_key("local"));
        upsert(&mut config, SaveServer { name:"docs".into(), server:json!({"command":"uvx"}) }).unwrap();
        config.validate().unwrap();
        assert!(config.mcp_servers["docs"].headers.is_empty());
        assert!(config.mcp_servers["docs"].url.is_none());
    }

    #[test]
    fn validates_transport_and_rejects_conflicting_names_and_shell_values() {
        for servers in [
            json!({"mix": {"url": "https://example.com", "command": "node"}}),
            json!({"bad": {"url": "file:///test"}}),
            json!({"bad": {"url": "https://user:pass@example.com"}}),
            json!({"bad": {"url": "https://example.com", "headers": {"X-Key":"!some-command"}}}),
            json!({"a-b": {"command": "node"}, "a_b": {"command": "node"}}),
        ] {
            let config: Config = serde_json::from_value(json!({"mcpServers": servers})).unwrap();
            assert!(config.validate().is_err());
        }
    }
}
