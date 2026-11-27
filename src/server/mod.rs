mod agents;
mod ai_settings;
mod assets;
mod commands;
mod mcp;
mod models;
mod pi;
mod projects;
mod runtime;
mod sessions;
mod skills;

use std::{net::Ipv4Addr, path::PathBuf, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, watch},
};

use crate::{Options, benchmark, ollama, ram, run, telemetry};

struct App {
    host: String,
    lan_host: Option<String>,
    origin: String,
    task: watch::Sender<run::Snapshot>,
    metrics: Mutex<telemetry::Cache>,
    ram: Mutex<ram::Cache>,
    startup_log: Option<PathBuf>,
    pi_agent_dir: Option<PathBuf>,
    benchmarks: benchmark::Store,
    sessions: std::sync::Mutex<sessions::History>,
    commands: std::sync::Mutex<commands::State>,
    skills: skills::Store,
    project_models: ai_settings::ProjectModelStore,
    agents_file: PathBuf,
    projects: projects::Store,
    folder_picker: Mutex<()>,
    command_slot: Mutex<()>,
}

pub(super) async fn serve(options: Options) -> std::io::Result<()> {
    std::sync::LazyLock::force(&ai_settings::AI_SETTINGS);
    if options.lan_ip.is_some_and(|ip| !ip.is_private()) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "LAN IP must be a private IPv4 address"));
    }
    let listener = TcpListener::bind((
        if options.lan_ip.is_some() { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST },
        options.port,
    )).await?;
    let lan_host = options.lan_ip.map(|ip| format!("{ip}:{}", listener.local_addr().expect("bound listener").port()));
    let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
    let data = std::path::absolute(crate::data_directory())?;
    let skills = skills::Store::new(data.join("skills"));
    let agents_file = data.join("AGENTS.md");
    agents::install_default(&agents_file)?;
    skills.install_defaults()?;
    let catalog = tokio::task::spawn_blocking(ollama::installed_models)
        .await
        .ok()
        .flatten();
    let model = match catalog.as_ref() {
        Some(names) => names
            .iter()
            .find(|name| {
                options.model.as_deref().is_some_and(|preferred| {
                    name.eq_ignore_ascii_case(preferred)
                        || name.eq_ignore_ascii_case(&format!("{preferred}:latest"))
                })
            })
            .or_else(|| names.first())
            .cloned(),
        None => options.model,
    };
    let app = Arc::new(App {
        origin: format!("http://{host}"),
        host,
        lan_host,
        task: watch::channel(run::Snapshot::new(model)).0,
        metrics: Mutex::default(),
        ram: Mutex::default(),
        startup_log: options.startup_log,
        pi_agent_dir: options.pi_agent_dir,
        benchmarks: benchmark::Store::new(options.benchmark_file),
        sessions: std::sync::Mutex::default(),
        commands: std::sync::Mutex::new(commands::State::new(
            crate::data_directory().join("command-mode.json"),
        )),
        skills,
        project_models: ai_settings::ProjectModelStore::load(data.join("project-models.json"))?,
        agents_file,
        projects: projects::Store::new(data.join("projects")),
        folder_picker: Mutex::default(),
        command_slot: Mutex::default(),
    });
    if let Some(names) = catalog {
        runtime::register_benchmarks(app.clone(), names).await;
    }
    if let Err((_, error)) = app.projects.recover_tasks() {
        eprintln!("Could not recover project tasks: {error}");
    }
    let router = assets::routes()
        .merge(runtime::routes())
        .merge(models::routes())
        .merge(ai_settings::routes())
        .merge(mcp::routes())
        .merge(sessions::routes())
        .merge(commands::routes())
        .merge(skills::routes())
        .merge(agents::routes())
        .merge(projects::routes())
        .layer(middleware::from_fn_with_state(app.clone(), local_only))
        .with_state(app.clone());

    println!("Dashboard: {}", app.origin);
    if let Some(host) = &app.lan_host {
        println!("WARNING: unauthenticated LAN access enabled: http://{host}/");
    }
    let dispatch = tokio::spawn(projects::queue::dispatch(app.clone()));
    let cleanup = tokio::spawn(models::clean_context_profiles(app));
    let result = tokio::select! {
        result = axum::serve(listener, router) => result,
        result = tokio::signal::ctrl_c() => result,
    };
    cleanup.abort();
    dispatch.abort();
    result
}

async fn local_only(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
    let valid_host = host.is_some_and(|host| host == app.host || app.lan_host.as_deref() == Some(host));
    let expected_origin = host.map(|host| format!("http://{host}"));
    let origin = request.headers().get(header::ORIGIN);
    let valid_origin = if request.uri().path() == "/mcp" {
        origin.is_none() || origin.is_some_and(|value| expected_origin.as_deref().is_some_and(|expected| value == expected))
    } else {
        request.method() != axum::http::Method::POST
            || origin.is_some_and(|value| expected_origin.as_deref().is_some_and(|expected| value == expected))
    };
    if !valid_host || !valid_origin {
        return StatusCode::FORBIDDEN.into_response();
    }
    if request.method() == axum::http::Method::POST
        && request.uri().path() != "/shutdown"
        && app.task.borrow().closed()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}
