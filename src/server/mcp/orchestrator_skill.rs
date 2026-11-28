//! Read-only distribution of the version of the orchestrator guide shipped
//! with this server. Never read arbitrary paths or locally installed worker skills.
use serde::Deserialize;
use serde_json::{Value, json};
use std::{hash::{DefaultHasher, Hash, Hasher}, sync::LazyLock};

pub(super) const INSTRUCTIONS: &str = "Lyra runs local workers; you own planning and final acceptance. Default to one autonomous enqueue_and_wait graph of all settled implementation and integration steps, with after dependencies and follow_up.review for automatic independent review, fixes and repeat review. Do not supervise individual handoffs or routine fixes. Before delegating a new task through Lyra, call get_orchestrator_skill (no arguments) to read the current orchestration guide, unless already loaded from this server for this task. Read its references on demand with the same tool's path argument or MCP resources. Pause only a branch needing a genuinely unresolved consequential decision or an actionable blocker; otherwise let the full pipeline finish. Respect user permissions and cancellation. Do not load the guide for unrelated work or repeat it for every tool call.";

const FILES: &[(&str, &str, &str)] = &[
    ("SKILL.md", "Lyra orchestrator guide", include_str!("../../../.agents/skills/lyra/SKILL.md")),
    ("references/project-pipelines.md", "Planning, delegation and project pipelines", include_str!("../../../.agents/skills/lyra/references/project-pipelines.md")),
    ("references/models-and-context.md", "Model selection and context recovery", include_str!("../../../.agents/skills/lyra/references/models-and-context.md")),
];

static REVISION: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = DefaultHasher::new();
    FILES.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
});

fn uri(path: &str) -> String { format!("lyra://orchestrator/{path}") }

pub(super) fn list() -> Value {
    json!({"resources": FILES.iter().map(|(path, description, _)| json!({
        "uri": uri(path), "name": path, "description": description,
        "mimeType": "text/markdown"
    })).collect::<Vec<_>>()})
}

pub(super) fn read(requested: &str) -> Option<Value> {
    let (_, _, text) = FILES.iter().find(|(path, _, _)| uri(path) == requested)?;
    Some(json!({"contents": [{"uri": requested, "mimeType": "text/markdown", "text": text}]}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    path: Option<String>,
}

pub(super) fn get(request: Request) -> Result<String, String> {
    let path = request.path.as_deref().unwrap_or("SKILL.md");
    let (_, _, text) = FILES.iter().find(|(candidate, _, _)| *candidate == path)
        .ok_or_else(|| "Unknown guide path. Use SKILL.md, references/project-pipelines.md or references/models-and-context.md.".to_owned())?;
    Ok(format!("Lyra orchestrator guide · revision {} · {}\n\n{}", &*REVISION, path, text))
}

pub(super) fn tool() -> Value {
    json!({
        "name": "get_orchestrator_skill",
        "description": "Read the current Lyra orchestrator instructions before delegating a new task, unless already loaded for this task. Default returns SKILL.md; use path to read a linked reference on demand. No model inference, disk access, or project mutation.",
        "inputSchema": {"type": "object", "properties": {
            "path": {"type": "string", "enum": ["SKILL.md", "references/project-pipelines.md", "references/models-and-context.md"], "default": "SKILL.md"}
        }, "additionalProperties": false},
        "annotations": {"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resources_and_tool_use_the_same_embedded_guide() {
        let resources = list();
        assert_eq!(resources["resources"].as_array().unwrap().len(), FILES.len());
        for &(path, _, text) in FILES {
            let resource = read(&uri(path)).unwrap();
            assert_eq!(resource["contents"][0]["text"], text);
            let result = get(Request { path: Some(path.to_owned()) }).unwrap();
            assert!(result.ends_with(text));
            assert!(result.contains(REVISION.as_str()));
        }
        assert!(get(Request { path: None }).unwrap().ends_with(FILES[0].2));
    }

    #[test]
    fn arbitrary_paths_are_not_resources() {
        assert!(get(Request { path: Some("../AGENTS.md".into()) }).is_err());
        assert!(read("file:///C:/Users/example/.codex/config.toml").is_none());
        assert!(read("lyra://orchestrator/../AGENTS.md").is_none());
    }
}
