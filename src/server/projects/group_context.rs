//! Group handoffs are derived from committed queue results and per-chat notes.
//! The catalog is the source of truth; no workers share a writable Markdown file.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::{Deserialize, Serialize};

use super::{Catalog, Project, Result, Session, Store, invalid, missing};

const BRIEF_LIMIT: usize = 8_000;
const RESULT_LIMIT: usize = 8_000;

#[derive(Default, Deserialize, Serialize)]
pub(super) struct GroupMemory {
    #[serde(default)]
    brief: String,
    #[serde(default)]
    brief_revision: u64,
    #[serde(default)]
    direct: BTreeMap<u64, DirectResult>,
}

#[derive(Deserialize, Serialize)]
struct DirectResult {
    text: String,
    truncated: bool,
    timestamp: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct GroupKey {
    pub project_id: u64,
    pub group_id: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::server) struct UpdateBrief {
    pub project_id: u64,
    pub group_id: u64,
    pub brief: String,
    pub brief_revision: u64,
}

#[derive(Serialize)]
pub(in crate::server) struct Document {
    project_id: u64,
    group_id: u64,
    name: String,
    brief: String,
    brief_revision: u64,
    editable: bool,
    markdown: String,
}

struct Entry {
    id: u64,
    name: String,
    status: &'static str,
    text: String,
    truncated: bool,
    required: bool,
}

pub(in crate::server) struct Snapshot {
    name: String,
    brief: String,
    entries: Vec<Entry>,
}

impl Snapshot {
    pub(in crate::server) fn markdown(&self) -> String {
        self.render(None)
    }

    // This is deliberately a byte budget, not a claimed tokenizer estimate.
    // The full Markdown is available to Work agents through the normal read tool.
    pub(in crate::server) fn excerpt(&self, context_tokens: u32) -> String {
        self.render(Some((context_tokens as usize / 2).clamp(512, 12_000)))
    }

    fn render(&self, budget: Option<usize>) -> String {
        let mut text = format!("# {}\n", one_line(&self.name));
        if let Some(budget) = budget {
            let brief_limit = if self.entries.iter().any(|entry| !entry.text.is_empty()) {
                budget / 3
            } else { budget.saturating_sub(text.len() + 100) };
            if !self.brief.trim().is_empty() {
                text.push_str("\n## Общая цель и ограничения\n");
                text.push_str(&clip(&self.brief, brief_limit));
                text.push('\n');
            }
            let mut entries = self.entries.iter().filter(|entry| !entry.text.is_empty()).collect::<Vec<_>>();
            entries.sort_by_key(|entry| (!entry.required, std::cmp::Reverse(entry.id)));
            let mut omitted = 0;
            let mut remaining = entries.len();
            for entry in entries {
                let heading = entry.heading();
                let available = budget.saturating_sub(text.len() + heading.len() + 100);
                if available < 100 {
                    omitted += 1;
                } else {
                    // Prioritize dependency results; divide room among remaining entries.
                    let share = if entry.required { available } else { available / remaining.max(1) };
                    let body = clip(&entry.text, share.max(100).min(available));
                    text.push_str(&heading);
                    quote(&mut text, &body);
                }
                remaining -= 1;
            }
            if omitted != 0 || self.entries.iter().any(|entry| entry.text.is_empty()) {
                text.push_str("\n_Некоторые шаги не включены в выдержку. Полный CONTEXT.md содержит все сохранённые результаты и состояния._\n");
            }
            if text.len() > budget {
                text = clip(&text, budget);
            }
        } else {
            text.push_str("\n## Общая цель и ограничения\n\n");
            text.push_str(if self.brief.trim().is_empty() { "_Пока не задана._" } else { &self.brief });
            text.push_str("\n\n## Результаты и состояния чатов\n");
            if self.entries.is_empty() { text.push_str("\n_Пока нет результатов._\n"); }
            for entry in &self.entries {
                text.push_str(&entry.heading());
                if entry.text.is_empty() {
                    text.push_str("\n_Нет сохранённого результата._\n");
                } else {
                    quote(&mut text, &entry.text);
                }
            }
        }
        text
    }
}

impl Entry {
    fn heading(&self) -> String {
        format!("\n### #{} — {} [{}{}{}]\n\n", self.id, one_line(&self.name), self.status,
            if self.required { "; prerequisite" } else { "" },
            if self.truncated { "; partial" } else { "" })
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn quote(out: &mut String, text: &str) {
    for line in text.lines() { let _ = writeln!(out, "> {line}"); }
}

fn clip(text: &str, max: usize) -> String {
    if text.len() <= max { return text.to_owned(); }
    const SUFFIX: &str = "…";
    if max < SUFFIX.len() { return String::new(); }
    format!("{}{SUFFIX}", &text[..text.floor_char_boundary(max - SUFFIX.len())])
}

fn group<'a>(catalog: &'a Catalog, key: &GroupKey) -> Result<(&'a Project, &'a super::ChatGroup)> {
    let project = catalog.projects.iter().find(|project| project.id == key.project_id)
        .ok_or_else(|| missing("Project"))?;
    let group = project.sessions.iter().filter_map(|session| session.group.as_ref())
        .find(|group| group.id == key.group_id).ok_or_else(|| missing("Group"))?;
    Ok((project, group))
}

fn collect(project: &Project, group_id: Option<u64>, memory: Option<&GroupMemory>,
    name: String, excluded: Option<u64>, dependencies: &BTreeSet<u64>) -> Snapshot {
    let mut entries = Vec::new();
    for session in &project.sessions {
        if Some(session.id) == excluded { continue; }
        let required = dependencies.contains(&session.id) || session.job.as_ref()
            .and_then(|job| job.workflow_root()).is_some_and(|id| dependencies.contains(&id));
        let in_group = group_id.is_some() && session.group.as_ref().map(|group| group.id) == group_id;
        if !in_group && !required { continue; }
        // A retry chat is a continuation of its queued owner; show the owner result once.
        if session.job.is_none() && project.sessions.iter().any(|owner| owner.job.as_ref()
            .is_some_and(|job| job.chat_session_id == Some(session.id))) && !required {
            if memory.and_then(|memory| memory.direct.get(&session.id)).is_none() { continue; }
        }
        let direct = memory.and_then(|memory| memory.direct.get(&session.id));
        let (status, mut text, mut truncated) = if let Some(job) = &session.job {
            let (status, result) = job.handoff();
            (status, result.map(|result| clip(&result.text, RESULT_LIMIT)).unwrap_or_default(),
                result.is_some_and(|result| result.truncated))
        } else { ("chat", String::new(), false) };
        let status = if let Some(result) = direct {
            if !text.is_empty() { text.push_str("\n\nLatest direct chat answer:\n"); }
            text.push_str(&result.text);
            truncated |= result.truncated;
            if session.job.is_none() { "answered" } else { status }
        } else { status };
        entries.push(Entry { id: session.id, name: session.name.clone(), status, text, truncated, required });
    }
    entries.sort_by_key(|entry| entry.id);
    Snapshot { name, brief: memory.map(|memory| memory.brief.clone()).unwrap_or_default(), entries }
}

fn document(catalog: &Catalog, key: &GroupKey) -> Result<Document> {
    let (project, group) = group(catalog, key)?;
    let memory = catalog.group_memory.get(&group.id);
    let snapshot = collect(project, Some(group.id), memory, group.name.clone(), None, &BTreeSet::new());
    Ok(Document {
        project_id: project.id, group_id: group.id, name: group.name.clone(),
        brief: snapshot.brief.clone(), brief_revision: memory.map_or(0, |memory| memory.brief_revision),
        editable: !project.archived && project.sessions.iter().any(|session| !session.archived &&
            session.group.as_ref().is_some_and(|group| group.id == key.group_id)),
        markdown: snapshot.markdown(),
    })
}

impl Store {
    pub(in crate::server) fn group_context(&self, key: GroupKey) -> Result<Document> {
        document(&self.list()?, &key)
    }

    pub(in crate::server) fn update_group_brief(&self, input: UpdateBrief) -> Result<Document> {
        if input.brief.len() > BRIEF_LIMIT {
            return Err(invalid("Общий контекст должен быть не длиннее 8000 байт."));
        }
        let key = GroupKey { project_id: input.project_id, group_id: input.group_id };
        self.change(|catalog| {
            if !document(catalog, &key)?.editable {
                return Err(invalid("Восстановите группу из архива перед редактированием."));
            }
            let memory = catalog.group_memory.entry(key.group_id).or_default();
            if memory.brief_revision != input.brief_revision {
                return Err((axum::http::StatusCode::CONFLICT, "Общая цель уже изменена. Обновите контекст и повторите изменение.".into()));
            }
            memory.brief = input.brief;
            memory.brief_revision += 1;
            document(catalog, &key)
        })
    }

    pub(in crate::server) fn handoff_for(&self, project_id: u64, session_id: u64) -> Result<Option<Snapshot>> {
        let catalog = self.list()?;
        let project = catalog.projects.iter().find(|project| project.id == project_id)
            .ok_or_else(|| missing("Project"))?;
        let session = project.session(session_id)?;
        let owner = if session.job.is_some() { session } else {
            project.sessions.iter().find(|owner| owner.job.as_ref()
                .is_some_and(|job| job.chat_session_id == Some(session_id))).unwrap_or(session)
        };
        let dependencies = owner.job.as_ref().map(|job| job.prerequisites().iter().copied().collect())
            .unwrap_or_else(BTreeSet::new);
        let group = owner.group.as_ref().or(session.group.as_ref());
        if group.is_none() && dependencies.is_empty() { return Ok(None); }
        let group_id = group.map(|group| group.id);
        let memory = group_id.and_then(|id| catalog.group_memory.get(&id));
        let mut snapshot = collect(project, group_id, memory,
            group.map(|group| group.name.clone()).unwrap_or_else(|| "Prerequisite handoffs".into()),
            Some(owner.id), &dependencies);
        if session_id != owner.id { snapshot.entries.retain(|entry| entry.id != session_id); }
        Ok(Some(snapshot))
    }

    pub(in crate::server) fn record_group_answer(&self, project_id: u64, session_id: u64, text: &str,
        truncated: bool, timestamp: u64) -> Result<bool> {
        // Avoid catalog writes for normal, ungrouped chats.
        let catalog = self.list()?;
        let project = catalog.projects.iter().find(|project| project.id == project_id)
            .ok_or_else(|| missing("Project"))?;
        if project.session(session_id)?.group.is_none() { return Ok(false); }
        self.change(|catalog| {
            let project = catalog.projects.iter().find(|project| project.id == project_id)
                .ok_or_else(|| missing("Project"))?;
            let Some(group) = &project.session(session_id)?.group else { return Ok(false); };
            let direct = &mut catalog.group_memory.entry(group.id).or_default().direct;
            if direct.get(&session_id).is_some_and(|result| result.timestamp > timestamp) { return Ok(false); }
            direct.insert(session_id, DirectResult { text: clip(text, RESULT_LIMIT), truncated, timestamp });
            Ok(true)
        })
    }
}
