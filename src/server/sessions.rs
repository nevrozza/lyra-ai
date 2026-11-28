use std::{
    collections::{BTreeMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::{Deserialize, Serialize};

use super::App;

const SESSION_LIMIT: usize = 24;
const EVENT_LIMIT: usize = 120;
const DETAIL_LIMIT: usize = 4_000;
const CALL_LIMIT: usize = 120;
const RECENT_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Default)]
pub(super) struct History {
    next_session: u64,
    next_call: u64,
    sessions: VecDeque<Session>,
    recent_calls: VecDeque<(Instant, String)>,
}

struct Session {
    group: Option<String>,
    summary: Summary,
    started: Instant,
    next_event: u64,
    events: VecDeque<Event>,
    calls: VecDeque<CallSummary>,
}

#[derive(Clone, Serialize)]
struct CallSummary {
    id: u64,
    tool: String,
    model: Option<String>,
    started_at: u64,
    duration_ms: Option<u64>,
    status: &'static str,
}

#[derive(Serialize)]
struct RecentCalls {
    total: usize,
    by_tool: BTreeMap<String, usize>,
}

fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

#[derive(Clone, Serialize)]
struct Summary {
    id: u64,
    title: String,
    started_at: u64,
    calls: u64,
    active: u64,
    errors: u64,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Mcp,
    Pi,
    Tool,
    Thinking,
    Answer,
}

#[derive(Clone, Serialize)]
struct Event {
    id: u64,
    call: u64,
    at_ms: u64,
    kind: Kind,
    title: String,
    text: String,
    status: Option<&'static str>,
}

#[derive(Deserialize)]
struct Selection {
    selected: Option<u64>,
}

#[derive(Serialize)]
struct View {
    sessions: Vec<Summary>,
    selected: Option<u64>,
    events: VecDeque<Event>,
    omitted_events: bool,
    calls: Vec<CallSummary>,
    omitted_calls: bool,
    recent: RecentCalls,
}

pub(super) fn routes() -> Router<Arc<App>> {
    Router::new().route("/sessions", get(view))
}

async fn view(State(app): State<Arc<App>>, Query(query): Query<Selection>) -> Json<View> {
    let mut history = app.sessions.lock().expect("session history lock");
    let recent = history.recent_counts(Instant::now());
    let selected = query
        .selected
        .and_then(|id| {
            history
                .sessions
                .iter()
                .find(|session| session.summary.id == id)
        })
        .or_else(|| history.sessions.back());
    Json(View {
        sessions: history
            .sessions
            .iter()
            .rev()
            .map(|session| session.summary.clone())
            .collect(),
        selected: selected.map(|session| session.summary.id),
        events: selected
            .map(|session| session.events.clone())
            .unwrap_or_default(),
        omitted_events: selected
            .is_some_and(|session| session.events.front().is_some_and(|event| event.id > 1)),
        calls: selected.map(|session| session.calls.iter().rev().cloned().collect()).unwrap_or_default(),
        omitted_calls: selected.is_some_and(|session| session.summary.calls > session.calls.len() as u64),
        recent,
    })
}

impl Session {
    fn push(
        &mut self,
        call: u64,
        kind: Kind,
        title: &str,
        text: &str,
        status: Option<&'static str>,
    ) -> bool {
        if kind == Kind::Thinking
            && let Some(previous) = self.events.back_mut()
            && previous.kind == kind
            && previous.call == call
            && previous.title == title
        {
            let remaining = DETAIL_LIMIT.saturating_sub(previous.text.len());
            if remaining == 0 {
                return false;
            }
            let end = text.floor_char_boundary(remaining);
            previous.text.push_str(&text[..end]);
            return end != 0;
        }
        self.next_event += 1;
        if self.events.len() == EVENT_LIMIT {
            self.events.pop_front();
        }
        self.events.push_back(Event {
            id: self.next_event,
            call,
            at_ms: self.started.elapsed().as_millis() as u64,
            kind,
            title: title[..title.floor_char_boundary(180)].to_owned(),
            text: text[..text.floor_char_boundary(DETAIL_LIMIT)].to_owned(),
            status,
        });
        true
    }
}

pub(super) struct Call {
    app: Arc<App>,
    tracked: Option<TrackedCall>,
}

struct TrackedCall {
    session: u64,
    id: u64,
    tool: String,
    started: Instant,
    // The counters belong to this call, even when it shares a session with a retry.
    token_usage: Option<String>,
}

impl Call {
    pub(super) fn app(&self) -> &App {
        &self.app
    }

    pub(super) fn app_handle(&self) -> Arc<App> {
        self.app.clone()
    }

    pub(super) fn untracked(app: Arc<App>) -> Self {
        Self { app, tracked: None }
    }

    pub(super) fn start(
        app: Arc<App>,
        tool: &str,
        model: Option<&str>,
        detail: &str,
        group: Option<String>,
    ) -> Self {
        let (session, id) = {
            let mut history = app.sessions.lock().expect("session history lock");
            history.next_call += 1;
            let call = history.next_call;
            let started = Instant::now();
            let started_at = unix_ms();
            history.record_call(started, tool);
            let existing = group.as_ref().and_then(|group| {
                history
                    .sessions
                    .iter()
                    .position(|session| session.group.as_ref() == Some(group))
            });
            let title = model.map_or_else(|| tool.to_owned(), |model| format!("{tool} · {model}"));
            let mut session = existing
                .and_then(|index| history.sessions.remove(index))
                .unwrap_or_else(|| {
                    history.next_session += 1;
                    Session {
                        group,
                        summary: Summary {
                            id: history.next_session,
                            title: title.clone(),
                            started_at,
                            calls: 0,
                            active: 0,
                            errors: 0,
                        },
                        started: Instant::now(),
                        next_event: 0,
                        events: VecDeque::new(),
                        calls: VecDeque::new(),
                    }
                });
            session.summary.calls += 1;
            session.summary.active += 1;
            if matches!(tool, "run_model" | "run_pi") {
                session.summary.title = title;
            }
            session.calls.push_back(CallSummary {
                id: call,
                tool: tool[..tool.floor_char_boundary(180)].to_owned(),
                model: model.map(|model| model[..model.floor_char_boundary(180)].to_owned()),
                started_at,
                duration_ms: None,
                status: "running",
            });
            session.trim_calls();
            session.push(call, Kind::Mcp, tool, detail, Some("running"));
            let id = session.summary.id;
            history.sessions.push_back(session);
            history.trim();
            (id, call)
        };
        app.task.send_modify(crate::run::Snapshot::sessions_changed);
        Self {
            app,
            tracked: Some(TrackedCall {
                session,
                id,
                tool: tool.into(),
                started: Instant::now(),
                token_usage: None,
            }),
        }
    }

    pub(super) fn record_token_usage(
        &mut self,
        prompt: Option<u64>,
        generated: Option<u64>,
        stop_reason: Option<&str>,
    ) {
        let Some(call) = self.tracked.as_mut() else {
            return;
        };
        let count =
            |count: Option<u64>| count.map_or_else(|| "—".into(), |count| count.to_string());
        call.token_usage = Some(format!(
            "Токены · вход {} · генерация {} · остановка {}",
            count(prompt),
            count(generated),
            stop_reason
                .filter(|reason| !reason.is_empty())
                .unwrap_or("—"),
        ));
    }

    pub(super) fn event(&self, kind: Kind, title: &str, text: &str, status: Option<&'static str>) {
        self.update(|call, session| session.push(call.id, kind, title, text, status));
    }

    pub(super) fn finish(self, error: Option<&str>) {
        self.update(|call, session| {
            session.summary.active -= 1;
            let (status, mut detail) = match error {
                None => ("ok", format!("{} мс", call.started.elapsed().as_millis())),
                Some(error) => {
                    session.summary.errors += 1;
                    ("error", error.to_owned())
                }
            };
            if let Some(usage) = &call.token_usage {
                detail.insert(0, '\n');
                detail.insert_str(0, usage);
            }
            if let Some(summary) = session.calls.iter_mut().find(|summary| summary.id == call.id) {
                summary.status = status;
                summary.duration_ms = Some(call.started.elapsed().as_millis() as u64);
            }
            session.trim_calls();
            session.push(call.id, Kind::Mcp, &call.tool, &detail, Some(status))
        });
    }

    fn update(&self, update: impl FnOnce(&TrackedCall, &mut Session) -> bool) {
        let Some(call) = &self.tracked else { return };
        let changed = {
            let mut history = self.app.sessions.lock().expect("session history lock");
            let changed = history
                .sessions
                .iter_mut()
                .find(|session| session.summary.id == call.session)
                .is_some_and(|session| update(call, session));
            history.trim();
            changed
        };
        if changed {
            self.app
                .task
                .send_modify(crate::run::Snapshot::sessions_changed);
        }
    }
}

impl Session {
    fn trim_calls(&mut self) {
        while self.calls.len() > CALL_LIMIT {
            let Some(index) = self.calls.iter().position(|call| call.status != "running") else {
                break;
            };
            self.calls.remove(index);
        }
    }
}

impl History {
    fn prune_recent(&mut self, now: Instant) {
        while self.recent_calls.front().is_some_and(|(started, _)| now.saturating_duration_since(*started) >= RECENT_WINDOW) {
            self.recent_calls.pop_front();
        }
    }

    fn record_call(&mut self, now: Instant, tool: &str) {
        self.prune_recent(now);
        self.recent_calls.push_back((now, tool[..tool.floor_char_boundary(180)].to_owned()));
    }

    fn recent_counts(&mut self, now: Instant) -> RecentCalls {
        self.prune_recent(now);
        let mut by_tool = BTreeMap::new();
        for (_, tool) in &self.recent_calls {
            *by_tool.entry(tool.clone()).or_insert(0) += 1;
        }
        RecentCalls { total: self.recent_calls.len(), by_tool }
    }

    fn trim(&mut self) {
        while self.sessions.len() > SESSION_LIMIT {
            let Some(index) = self
                .sessions
                .iter()
                .position(|session| session.summary.active == 0)
            else {
                break;
            };
            self.sessions.remove(index);
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_counts_count_starts_once_and_expire_independently_of_session_history() {
        let mut history = History::default();
        let start = Instant::now();
        history.record_call(start, "run_pi");
        history.record_call(start + Duration::from_secs(60), "run_pi");
        history.record_call(start + Duration::from_secs(120), "list_projects");
        let counts = history.recent_counts(start + Duration::from_secs(120));
        assert_eq!(counts.total, 3);
        assert_eq!(counts.by_tool["run_pi"], 2);
        assert_eq!(counts.by_tool["list_projects"], 1);
        history.trim();
        let counts = history.recent_counts(start + RECENT_WINDOW);
        assert_eq!(counts.total, 2);
        assert_eq!(counts.by_tool["run_pi"], 1);
        assert_eq!(history.recent_counts(start + RECENT_WINDOW + Duration::from_secs(120)).total, 0);
    }
}
