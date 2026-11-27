use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
    sync::Arc,
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{StatusCode, Uri, header::HeaderName},
    routing::get,
};
use serde::{Deserialize, Serialize};

use super::App;

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(super) struct Config {
    mcp_servers: BTreeMap<String, Server>,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Default, Deserialize, Serialize)]
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

pub(super) struct Store(PathBuf);

impl Store {
    pub(super) fn new(path: PathBuf) -> Self {
        Self(path)
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

fn storage_error(_: io::Error) -> Error {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        "Не удалось прочитать или сохранить настройки MCP.".into(),
    )
}

async fn get_settings(State(app): State<Arc<App>>) -> Result<Json<Config>, Error> {
    require_local(&app)?;
    app.mcp_servers
        .load()
        .await
        .map(Json)
        .map_err(storage_error)
}

async fn save_settings(
    State(app): State<Arc<App>>,
    Json(config): Json<Config>,
) -> Result<Json<Config>, Error> {
    require_local(&app)?;
    config
        .validate()
        .map_err(|error| (StatusCode::BAD_REQUEST, error))?;
    app.mcp_servers
        .save(config)
        .await
        .map(Json)
        .map_err(storage_error)
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
