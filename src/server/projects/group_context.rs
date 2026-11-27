//! Group handoffs are derived from committed queue results and per-chat notes.
//! The catalog is the source of truth; no workers share a writable Markdown file.
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use serde::{Deserialize, Serialize};

use super::{Catalog, ChatGroup, Project, Result, Store, invalid, missing};

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

struct Section {
    heading: String,
    body: String,
    start_line: usize,
    end_line: usize,
    required: bool,
    has_result: bool,
}

impl Snapshot {
    fn sections(&self) -> (String, Vec<Section>) {
        let mut full = format!("# {}\n", one_line(&self.name));
        let mut next_line = full.lines().count() + 1;
        let mut sections = Vec::new();
        let mut append = |heading: String, body: String, required: bool, has_result: bool| {
            let start_line = next_line;
            let block = format!("{heading}{body}\n");
            next_line += block.lines().count();
            full.push_str(&block);
            sections.push(Section {
                heading, body, start_line, end_line: next_line - 1, required, has_result,
            });
        };
        append(
            "\n## Общая цель и ограничения\n\n".into(),
            if self.brief.trim().is_empty() { "_Пока не задана._".into() } else { self.brief.clone() },
            true, !self.brief.trim().is_empty(),
        );
        append("\n## Результаты и состояния чатов\n".into(), String::new(), false, false);
        for entry in &self.entries {
            let mut body = String::new();
            if entry.text.is_empty() {
                body.push_str("_Нет сохранённого результата._");
            } else {
                quote(&mut body, &entry.text);
            }
            append(entry.heading(), body, entry.required, !entry.text.is_empty());
        }
        (full, sections)
    }

    pub(in crate::server) fn markdown(&self) -> String {
        self.sections().0
    }

    // The budget is bytes, not a tokenizer estimate. Every displayed fragment
    // identifies its source range in the exact Markdown snapshot.
    pub(in crate::server) fn excerpt(&self, context_tokens: u32) -> String {
        let budget = (context_tokens as usize / 2).clamp(512, 12_000);
        let (_, mut sections) = self.sections();
        let brief = sections.remove(0);
        sections.remove(0); // structural heading, not a separate handoff
        // Dependencies first; then completed answers, then status-only entries.
        // Keep newer non-prerequisite answers ahead of older ones.
        sections.reverse();
        sections.sort_by_key(|section| (!section.required, !section.has_result));
        let mut text = format!("# {}\nExcerpt from CONTEXT.md; not an AI summary.\n", clip(&one_line(&self.name), 120));
        const FOOTER_RESERVE: usize = 160;
        let mut omitted = 0;
        let mut shortened = 0;
        let mut skipped_ranges = Vec::new();

        // Do not let a long brief consume all of the dependency budget.
        let brief_budget = if sections.is_empty() {
            budget.saturating_sub(text.len() + FOOTER_RESERVE)
        } else {
            budget / 3
        };
        if !append_excerpt(&mut text, &brief, brief_budget, &mut shortened) {
            omitted += 1;
            skipped_ranges.push((brief.start_line, brief.end_line));
        }
        let mut remaining = sections.len();
        let mut required_remaining = sections.iter().filter(|section| section.required).count();
        for section in sections {
            let available = budget.saturating_sub(text.len() + FOOTER_RESERVE);
            let share = if section.required {
                available / required_remaining.max(1)
            } else {
                available / remaining.max(1)
            };
            // Small shares may omit an entire result. Never silently cut off its
            // heading or provenance marker to make the budget fit.
            if !append_excerpt(&mut text, &section, share, &mut shortened) {
                omitted += 1;
                skipped_ranges.push((section.start_line, section.end_line));
            }
            if section.required { required_remaining -= 1; }
            remaining -= 1;
        }

        // Give the agent exact locations for omitted sections when room permits.
        // A count remains visible even if there are too many ranges to list.
        let footer = format!("\nEXCERPT COVERAGE: {shortened} shortened, {omitted} omitted sections. Read CONTEXT.md for missing details if needed.\n");
        if !skipped_ranges.is_empty() {
            let mut index = String::from("\nOmitted source lines:");
            for (start, end) in skipped_ranges {
                let range = format!(" {start}-{end};");
                if text.len() + index.len() + range.len() + footer.len() + 1 > budget { break; }
                index.push_str(&range);
            }
            if index != "\nOmitted source lines:" {
                text.push_str(&index);
                text.push('\n');
            }
        }
        text.push_str(&footer);
        debug_assert!(text.len() <= budget);
        text
    }
}

fn append_excerpt(out: &mut String, section: &Section, budget: usize, shortened: &mut usize) -> bool {
    let source = format!("Source: CONTEXT.md lines {}-{}.\n", section.start_line, section.end_line);
    let overhead = section.heading.len() + source.len() + 1;
    if overhead + section.body.len() <= budget {
        out.push_str(&section.heading);
        out.push_str(&source);
        out.push_str(&section.body);
        out.push('\n');
        return true;
    }
    const MORE: &str = "\n[EXCERPT SHORTENED: more text in the source range above.]\n";
    if budget < overhead + MORE.len() + 16 { return false; }
    out.push_str(&section.heading);
    out.push_str(&source);
    out.push_str(&clip(&section.body, budget - overhead - MORE.len()));
    out.push_str(MORE);
    *shortened += 1;
    true
}

impl Entry {
    fn heading(&self) -> String {
        format!("\n### #{} — {} [{}{}{}]\n\n", self.id, one_line(&self.name), self.status,
            if self.required { "; prerequisite" } else { "" },
            if self.truncated { "; saved result incomplete" } else { "" })
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
                result.is_some_and(|result| result.truncated || result.text.len() > RESULT_LIMIT))
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

pub(super) fn initialize_brief(
    memory: &mut std::collections::HashMap<u64, GroupMemory>,
    group: Option<&ChatGroup>, existing_group: Option<u64>, brief: Option<String>,
) -> Result<()> {
    let Some(brief) = brief else { return Ok(()); };
    if existing_group.is_some() {
        return Err(invalid("Use set_group_context to edit an existing group's brief."));
    }
    let group = group.ok_or_else(|| invalid("group_brief requires a new group"))?;
    if brief.len() > BRIEF_LIMIT {
        return Err(invalid("Общий контекст должен быть не длиннее 8000 байт."));
    }
    memory.insert(group.id, GroupMemory { brief, brief_revision: 1, ..GroupMemory::default() });
    Ok(())
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
            direct.insert(session_id, DirectResult { text: clip(text, RESULT_LIMIT), truncated: truncated || text.len() > RESULT_LIMIT, timestamp });
            Ok(true)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::projects::SaveProject;
    use serde_json::json;

    #[test]
    fn group_brief_and_separate_direct_handoffs_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::new(root.clone());
        let project = store.save(SaveProject {
            id: None, name: "Project".into(), folders: vec![dir.path().to_path_buf()],
        }).unwrap();
        let first = store.create_session(serde_json::from_value(json!({
            "project_id": project.id, "name": "Design", "group_name": "Feature",
            "group_brief": "Preserve API compatibility"
        })).unwrap()).unwrap();
        let group_id = first.group.as_ref().unwrap().id;
        let create = |name: &str| store.create_session(serde_json::from_value(json!({
            "project_id": project.id, "group_id": group_id, "name": name
        })).unwrap()).unwrap();
        let second = create("Backend");
        let third = create("Frontend");
        let key = || GroupKey { project_id: project.id, group_id };
        assert_eq!(store.group_context(key()).unwrap().brief_revision, 1);
        assert!(store.record_group_answer(project.id, first.id, "Use GET /items", false, 100).unwrap());
        assert!(store.record_group_answer(project.id, second.id, "Return JSON", false, 101).unwrap());
        assert!(!store.record_group_answer(project.id, first.id, "Old answer", false, 99).unwrap());
        let store = Store::new(root);
        let snapshot = store.handoff_for(project.id, third.id).unwrap().unwrap();
        let text = snapshot.markdown();
        assert!(text.contains("Preserve API compatibility"));
        assert!(text.contains("Use GET /items"));
        assert!(text.contains("Return JSON"));
        assert!(!text.contains("Old answer"));
        assert!(!text.contains("Frontend"));
        let updated = store.update_group_brief(UpdateBrief {
            project_id: project.id, group_id, brief_revision: 1, brief: "New constraints".into(),
        }).unwrap();
        assert_eq!(updated.brief_revision, 2);
        assert!(updated.markdown.contains("Use GET /items"));
        assert!(store.update_group_brief(UpdateBrief {
            project_id: project.id, group_id, brief_revision: 1, brief: "Stale".into(),
        }).is_err());
        assert!(store.group_context(GroupKey { project_id: project.id + 1000, group_id }).is_err());
    }

    #[test]
    fn excerpt_marks_shortened_sections_and_points_to_exact_source_lines() {
        let snapshot = Snapshot {
            name: "Feature".into(), brief: "A brief".into(),
            entries: vec![
                Entry { id: 1, name: "Design".into(), status: "completed", required: true,
                    text: format!("Contract start\n{}\nContract end", "interface ".repeat(500)), truncated: false },
                Entry { id: 2, name: "Other".into(), status: "completed", required: false,
                    text: "Other result".into(), truncated: false },
            ],
        };
        let (full, sections) = snapshot.sections();
        let lines = full.lines().collect::<Vec<_>>();
        let design = &sections[2];
        let source = lines[design.start_line - 1..design.end_line].join("\n");
        assert!(source.contains("### #1"));
        assert!(source.contains("Contract start"));
        assert!(source.contains("Contract end"));
        assert!(!source.contains("Other result"));
        let excerpt = snapshot.excerpt(4096);
        assert!(excerpt.len() <= 2048);
        assert!(excerpt.contains(&format!("Source: CONTEXT.md lines {}-{}.", design.start_line, design.end_line)));
        assert!(excerpt.contains("EXCERPT SHORTENED"));
        assert!(excerpt.contains("EXCERPT COVERAGE:"));
        assert!(!excerpt.contains("Contract end"));
    }

    #[test]
    fn excerpt_never_silently_drops_sections_at_small_budgets() {
        let snapshot = Snapshot {
            name: "я".repeat(120), brief: "я".repeat(4000),
            entries: (1..=30).map(|id| Entry {
                id, name: "Design".into(), status: "completed", required: true,
                text: "Result ".repeat(100), truncated: false,
            }).collect(),
        };
        let excerpt = snapshot.excerpt(1024);
        assert!(excerpt.len() <= 512);
        assert!(excerpt.contains("EXCERPT COVERAGE:"));
        assert!(excerpt.contains("omitted sections"));
        assert!(excerpt.contains("Read CONTEXT.md"));
    }

    #[test]
    fn excerpt_is_bounded_and_prioritizes_dependencies() {
        let snapshot = Snapshot {
            name: "Feature".into(), brief: "Goal".into(),
            entries: vec![
                Entry { id: 1, name: "Design".into(), status: "completed", required: true,
                    text: "Contract: /items".into(), truncated: false },
                Entry { id: 9, name: "Unrelated".into(), status: "completed", required: false,
                    text: "я".repeat(8000), truncated: false },
            ],
        };
        let excerpt = snapshot.excerpt(4096);
        assert!(excerpt.len() <= 2048);
        assert!(excerpt.contains("Contract: /items"));
        assert!(snapshot.markdown().len() > excerpt.len());
    }
}
