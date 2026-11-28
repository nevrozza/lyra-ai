use std::{collections::BTreeMap, time::{Duration, Instant}};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::run::LiveTool;

const PREVIEW_LIMIT: usize = 16 * 1024;
const LIVE_LIMIT: usize = 8;
const UPDATE_INTERVAL: Duration = Duration::from_millis(150);

fn ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Timing {
    name: String,
    generation_ms: Option<u64>,
    argument_bytes: u64,
    execution_ms: Option<u64>,
    bridge_ms: Option<u64>,
    rpc_ms: Option<u64>,
}

struct Pending {
    id: String,
    name: String,
    target: String,
    phase: &'static str,
    started: Option<Instant>,
    generation_ms: Option<u64>,
    execution_started: Option<Instant>,
    argument_bytes: u64,
    preview: String,
    preview_truncated: bool,
    bridge_ms: Option<u64>,
    rpc_ms: Option<u64>,
}

impl Pending {
    fn new(id: String, name: String, started: Option<Instant>) -> Self {
        Self { id, name, target: String::new(), phase: "generating", started,
            generation_ms: None, execution_started: None, argument_bytes: 0,
            preview: String::new(), preview_truncated: false, bridge_ms: None, rpc_ms: None }
    }
    fn finish_generation(&mut self) {
        if self.generation_ms.is_none() {
            self.generation_ms = self.started.map(|started| ms(started.elapsed()));
        }
        self.phase = "queued";
    }
    fn target_from(&mut self, args: &Value) {
        // Never expose arbitrary MCP arguments, credentials, or shell commands.
        let field = match self.name.as_str() {
            "read" | "write" | "edit" | "mkdir" | "ls" | "grep" | "find" | "copy_skill_resource" => "path",
            "load_skill" => "name",
            _ => return,
        };
        if let Some(value) = args[field].as_str() {
            self.target = value[..value.floor_char_boundary(512)].to_owned();
        }
    }
    fn append(&mut self, delta: &str) {
        self.argument_bytes = self.argument_bytes.saturating_add(delta.len() as u64);
        if !matches!(self.name.as_str(), "write" | "edit") { return; }
        let remaining = PREVIEW_LIMIT.saturating_sub(self.preview.len());
        let end = delta.floor_char_boundary(remaining);
        self.preview.push_str(&delta[..end]);
        self.preview_truncated |= end != delta.len();
    }
    fn live(&self) -> LiveTool {
        LiveTool { id: self.id.clone(), name: self.name.clone(), phase: self.phase,
            target: self.target.clone(), argument_bytes: self.argument_bytes,
            generation_ms: self.generation_ms.or_else(|| self.started.map(|start| ms(start.elapsed()))),
            preview: self.preview.clone(), preview_truncated: self.preview_truncated }
    }
}

#[derive(Default)]
pub(super) struct Stream {
    calls: BTreeMap<u64, Pending>,
    last_publish: Option<Instant>,
}

impl Stream {
    pub(super) fn clear(&mut self) {
        self.calls.clear();
        self.last_publish = None;
    }

    pub(super) fn live(&self) -> Vec<LiveTool> {
        // Prefer currently generated/running calls if there is a large batch.
        let mut calls: Vec<_> = self.calls.values().collect();
        calls.sort_by_key(|call| call.phase == "queued");
        calls.into_iter().take(LIVE_LIMIT).map(Pending::live).collect()
    }

    // Supports both cumulative SDK partials and newer linear RPC delta events.
    // Only the bounded prefix of write/edit arguments is retained for UI.
    pub(super) fn update(&mut self, event: &Value, message: &Value) -> bool {
        let kind = event["type"].as_str().unwrap_or_default();
        if !matches!(kind, "toolcall_start" | "toolcall_delta" | "toolcall_end") { return false; }
        let Some(index) = event["contentIndex"].as_u64() else { return false };
        let block = usize::try_from(index).ok().and_then(|index| {
            event["partial"]["content"].get(index).or_else(|| message["content"].get(index))
        }).unwrap_or(&Value::Null);
        let final_call = &event["toolCall"];
        let name = event["toolName"].as_str().or_else(|| final_call["name"].as_str()).or_else(|| block["name"].as_str());
        let id = event["id"].as_str().or_else(|| final_call["id"].as_str()).or_else(|| block["id"].as_str());
        if !self.calls.contains_key(&index) && self.calls.len() >= 128 { return false; }
        let call = self.calls.entry(index).or_insert_with(|| Pending::new(
            id.unwrap_or_default().to_owned(), name.unwrap_or("tool").to_owned(),
            (kind != "toolcall_end").then(Instant::now),
        ));
        if let Some(id) = id.filter(|id| !id.is_empty()) { call.id = id.to_owned(); }
        if let Some(name) = name.filter(|name| !name.is_empty()) { call.name = name.to_owned(); }
        // A synthetic ID is stable within this assistant message until the
        // provider supplies the actual ID.
        if call.id.is_empty() { call.id = format!("stream-{index}"); }
        call.target_from(&block["arguments"]);
        if let Some(delta) = event["delta"].as_str() { call.append(delta); }
        if kind == "toolcall_end" {
            call.target_from(&final_call["arguments"]);
            call.finish_generation();
        }
        let publish = kind != "toolcall_delta" || self.last_publish.is_none_or(|last| last.elapsed() >= UPDATE_INTERVAL);
        if publish { self.last_publish = Some(Instant::now()); }
        publish
    }

    fn index_for(&self, id: &str) -> Option<u64> {
        if id.is_empty() { return None; }
        self.calls.iter().find_map(|(index, call)| (call.id == id).then_some(*index))
    }

    pub(super) fn execution_start(&mut self, event: &Value) {
        let id = event["toolCallId"].as_str().unwrap_or_default();
        let name = event["toolName"].as_str().unwrap_or("tool");
        let index = self.index_for(id).or_else(|| self.calls.iter().find_map(|(index, call)|
            (call.id.starts_with("stream-") && call.name == name && call.execution_started.is_none()).then_some(*index)
        )).unwrap_or_else(|| self.calls.keys().next_back().copied().unwrap_or(0).saturating_add(1));
        if !self.calls.contains_key(&index) && self.calls.len() >= 128 { return; }
        let call = self.calls.entry(index).or_insert_with(|| Pending::new(id.into(), name.into(), None));
        if !id.is_empty() { call.id = id.into(); }
        call.name = name.into();
        call.target_from(&event["args"]);
        call.finish_generation();
        call.phase = "executing";
        call.execution_started = Some(Instant::now());
        // Avoid duplicate live code once the completed call is in history.
        call.preview.clear();
    }

    pub(super) fn record_bridge(&mut self, id: &str, elapsed: Duration) {
        if let Some(index) = self.index_for(id) { self.calls.get_mut(&index).unwrap().bridge_ms = Some(ms(elapsed)); }
    }
    pub(super) fn record_rpc(&mut self, id: &str, elapsed_ms: u64) {
        if let Some(index) = self.index_for(id) { self.calls.get_mut(&index).unwrap().rpc_ms = Some(elapsed_ms); }
    }
    pub(super) fn execution_end(&mut self, event: &Value) -> Option<Timing> {
        let id = event["toolCallId"].as_str().unwrap_or_default();
        let index = self.index_for(id).or_else(|| {
            let name = event["toolName"].as_str()?;
            self.calls.iter().find_map(|(index, call)| (call.name == name && call.phase == "executing").then_some(*index))
        })?;
        let call = self.calls.remove(&index)?;
        Some(Timing { name: call.name, generation_ms: call.generation_ms, argument_bytes: call.argument_bytes,
            execution_ms: call.execution_started.map(|start| ms(start.elapsed())), bridge_ms: call.bridge_ms, rpc_ms: call.rpc_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn modern_stream_counts_arguments_and_correlates_execution() {
        let mut stream = Stream::default();
        assert!(stream.update(&json!({"type":"toolcall_start","contentIndex":1,"id":"a","toolName":"write"}), &Value::Null));
        stream.update(&json!({"type":"toolcall_delta","contentIndex":1,"delta":"{\"path\":\"a\"}"}), &Value::Null);
        let view = stream.live();
        assert_eq!(view[0].argument_bytes, 12);
        assert_eq!(view[0].preview, "{\"path\":\"a\"}");
        stream.update(&json!({"type":"toolcall_end","contentIndex":1,"toolCall":{"id":"a","name":"write","arguments":{"path":"a"}}}), &Value::Null);
        stream.execution_start(&json!({"toolCallId":"a","toolName":"write","args":{"path":"a"}}));
        assert_eq!(stream.live()[0].phase, "executing");
        assert!(stream.live()[0].preview.is_empty());
        stream.record_rpc("a", 5);
        let timing = stream.execution_end(&json!({"toolCallId":"a","toolName":"write"})).unwrap();
        assert_eq!(timing.rpc_ms, Some(5));
        assert!(timing.generation_ms.is_some());
        assert!(stream.live().is_empty());
    }

    #[test]
    fn legacy_partial_and_non_file_arguments_are_safe() {
        let mut stream = Stream::default();
        stream.update(&json!({"type":"toolcall_start","contentIndex":0,"partial":{"content":[{"id":"x","name":"mcp__secret","arguments":{}}]}}), &Value::Null);
        stream.update(&json!({"type":"toolcall_delta","contentIndex":0,"delta":"secret"}), &Value::Null);
        assert_eq!(stream.live()[0].argument_bytes, 6);
        assert!(stream.live()[0].preview.is_empty());
        stream.clear();
        assert!(stream.live().is_empty());
    }
}
