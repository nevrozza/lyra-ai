use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, LazyLock},
};

use axum::{Router, http::header, response::Html, routing::get};

use super::App;

const DASHBOARD_TEMPLATE: &str = include_str!("../../web/index.html");
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";
const ASSETS: &[(&str, &str, &str)] = &[
    (
        "/app.css",
        "text/css; charset=utf-8",
        include_str!("../../web/app.css"),
    ),
    (
        "/lyra.svg",
        "image/svg+xml",
        include_str!("../../assets/lyra.svg"),
    ),
    (
        "/lyra.webmanifest",
        "application/manifest+json",
        include_str!("../../web/lyra.webmanifest"),
    ),
    (
        "/startup-log",
        "text/html; charset=utf-8",
        include_str!("../../web/startup-log.html"),
    ),
    ("/app.js", JAVASCRIPT, include_str!("../../web/app.js")),
    (
        "/service-worker.js",
        JAVASCRIPT,
        include_str!("../../web/service-worker.js"),
    ),
    (
        "/js/commands.js",
        JAVASCRIPT,
        include_str!("../../web/js/commands.js"),
    ),
    (
        "/js/sessions.js",
        JAVASCRIPT,
        include_str!("../../web/js/sessions.js"),
    ),
    (
        "/js/benchmark-results.js",
        JAVASCRIPT,
        include_str!("../../web/js/benchmark-results.js"),
    ),
    (
        "/js/context-settings.js",
        JAVASCRIPT,
        include_str!("../../web/js/context-settings.js"),
    ),
    (
        "/js/layout.js",
        JAVASCRIPT,
        include_str!("../../web/js/layout.js"),
    ),
    (
        "/js/memory-budget.js",
        JAVASCRIPT,
        include_str!("../../web/js/memory-budget.js"),
    ),
    (
        "/js/model-api.js",
        JAVASCRIPT,
        include_str!("../../web/js/model-api.js"),
    ),
    (
        "/js/model-list.js",
        JAVASCRIPT,
        include_str!("../../web/js/model-list.js"),
    ),
    (
        "/js/models.js",
        JAVASCRIPT,
        include_str!("../../web/js/models.js"),
    ),
    (
        "/js/telemetry.js",
        JAVASCRIPT,
        include_str!("../../web/js/telemetry.js"),
    ),
    ("/js/ui.js", JAVASCRIPT, include_str!("../../web/js/ui.js")),
    (
        "/vendor/markdown-it.min.js",
        JAVASCRIPT,
        include_str!("../../web/vendor/markdown-it.min.js"),
    ),
    (
        "/js/markdown.js",
        JAVASCRIPT,
        include_str!("../../web/js/markdown.js"),
    ),
    (
        "/js/projects.js",
        JAVASCRIPT,
        include_str!("../../web/js/projects.js"),
    ),
    (
        "/js/chat-thinking.js",
        JAVASCRIPT,
        include_str!("../../web/js/chat-thinking.js"),
    ),
    (
        "/js/skills.js",
        JAVASCRIPT,
        include_str!("../../web/js/skills.js"),
    ),
];

static UI_REVISION: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = DefaultHasher::new();
    (DASHBOARD_TEMPLATE, ASSETS).hash(&mut hasher);
    format!("{:016x}", hasher.finish())
});
static DASHBOARD_HTML: LazyLock<String> =
    LazyLock::new(|| DASHBOARD_TEMPLATE.replace("__LYRA_UI_REVISION__", revision()));

pub(super) fn revision() -> &'static str {
    &UI_REVISION
}

pub(super) fn routes() -> Router<Arc<App>> {
    let mut router = Router::new().route("/", get(|| async { Html(DASHBOARD_HTML.as_str()) }));
    for &(path, mime, content) in ASSETS {
        router = router.route(
            path,
            get(move || async move { ([(header::CONTENT_TYPE, mime)], content) }),
        );
    }
    router
}
