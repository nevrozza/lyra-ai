use std::{
    collections::VecDeque,
};

use serde::{Deserialize, Serialize};

use super::{
    App, Job, JobResult, MessageOrigin, Prompt, Result, Status, invalid, mcp,
};
use crate::{
    server::{ai_settings::AI_SETTINGS, pi::Role},
};

const MAX_REVIEWS: u8 = 3;

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Config {
    review: Step,
    launch: Option<Step>,
    max_fixes: Option<usize>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    model: String,
    prompt: String,
}

impl Config {
    pub(super) fn for_dashboard(model: String, prompt: &str) -> Self {
        Self {
            review: Step {
                model,
                prompt: prompt[..prompt.floor_char_boundary(4_000)].to_owned(),
            },
            launch: None,
            max_fixes: None,
        }
    }

    pub(super) fn validate(&mut self, installed: &[String]) -> Result<()> {
        for model in [
            Some(&mut self.review.model),
            self.launch.as_mut().map(|launch| &mut launch.model),
        ]
        .into_iter()
        .flatten()
        {
            let name = crate::model_name(model).map_err(invalid)?;
            *model = mcp::resolve_model(installed, &name).map_err(invalid)?;
        }
        for step in std::iter::once(&self.review).chain(self.launch.as_ref()) {
            if step.prompt.trim().is_empty() || step.prompt.len() > 4_000 {
                return Err(invalid("Укажите цель проверки и запуска до 4000 байт."));
            }
        }
        if self.max_fixes.is_some_and(|limit| limit > 8) {
            return Err(invalid("max_fixes должен быть от 0 до 8."));
        }
        Ok(())
    }
}

#[derive(Serialize)]
pub(super) struct Candidate {
    model: String,
    pi_checked: bool,
    memory: serde_json::Value,
}

#[cfg(test)]
pub(super) fn test_candidate(model: &str) -> Candidate {
    Candidate {
        model: model.into(),
        pi_checked: true,
        memory: serde_json::Value::Null,
    }
}

pub(super) fn candidates(
    app: &App,
    reviewer: &str,
    metrics: &crate::telemetry::Metrics,
) -> Vec<Candidate> {
    metrics
        .installed_models()
        .unwrap_or_default()
        .iter()
        .filter_map(|model| {
            let record = app.benchmarks.get(model).ok()?;
            let pi_check = record.pi_check;
            if pi_check == Some(false) {
                return None;
            }
            let estimate = crate::context::estimate(model).unwrap_or_default();
            let advertised = estimate.supports_agent_tools();
            if advertised == Some(false)
                || (advertised.is_none() && pi_check.is_none() && model != reviewer)
            {
                return None;
            }
            let resident = metrics
                .models
                .as_ref()
                .and_then(|models| models.iter().find(|entry| entry.name == *model));
            let memory = mcp::memory_estimate(metrics, model, &record, &estimate, resident, None);
            Some(Candidate {
                model: model.clone(),
                pi_checked: pi_check == Some(true),
                memory: serde_json::json!({
                    "weights_bytes": memory["weights_bytes"],
                    "context_tokens": memory["context_tokens"],
                    "kv_included": memory["kv_included"],
                    "estimated_required_bytes": memory["total_bytes_excluding_unknown_kv"],
                    "available_vram_bytes": memory["best_gpu_vram_available_bytes"],
                    "estimated_ram_shortfall_bytes": memory["ram_shortfall_bytes"]
                }),
            })
        })
        .collect()
}

pub(super) fn release_reviewer(
    app: &App,
    metrics: &crate::telemetry::Metrics,
    reviewer: &str,
    successor: &str,
) -> std::result::Result<(), String> {
    let Some(loaded) = &metrics.models else {
        return Ok(());
    };
    let review_models: Vec<_> = loaded
        .iter()
        .filter(|model| model.name == reviewer)
        .collect();
    if review_models.is_empty()
        || review_models.iter().any(|model| model.is_pinned())
        || !review_models
            .iter()
            .any(|model| model.size_vram.is_some_and(|bytes| bytes > 0))
    {
        return Ok(());
    }
    let resident = loaded.iter().find(|model| model.name == successor);
    let record = crate::server::models::model_settings(app, successor)?;
    let estimate = crate::context::estimate(successor).unwrap_or_default();
    let memory = mcp::memory_estimate(metrics, successor, &record, &estimate, resident, None);
    let fits = memory["total_bytes_excluding_unknown_kv"]
        .as_u64()
        .zip(memory["best_gpu_vram_available_bytes"].as_u64())
        .map_or_else(
            || resident.is_some(),
            |(required, available)| required <= available,
        );
    if !fits {
        crate::ollama::change_model(reviewer, crate::ollama::ModelAction::Unload, None)
            .map_err(|error| format!("Не удалось выгрузить модель проверки {reviewer}: {error}"))?;
    }
    Ok(())
}

pub(super) fn add_candidates(
    prompt: &mut String,
    candidates: &[Candidate],
    metrics: &crate::telemetry::Metrics,
    reviewer: &str,
    intelligence: &crate::server::ai_settings::ProjectModels,
) {
    use std::fmt::Write;
    let models = serde_json::to_string(candidates).expect("serialize model shortlist");
    let levels = serde_json::to_string(intelligence).expect("serialize intelligence levels");
    let _ = write!(prompt, "\nConfigured model intelligence (low/medium/high): {levels}. Low is only for trivial, fully specified mechanical fixes (typos, literal replacements, formatting); never delegate implementation or fixes requiring reasoning to Low. Medium is the minimum for writing code, debugging and refactoring. High is preferred for complex implementation, architectural or subtle behavioral fixes and review. Escalate after a failed attempt; never downgrade merely to save memory. Unclassified models require evidence of sufficient capability.\n");
    let system = serde_json::json!({
        "cpu_percent": metrics.cpu_percent,
        "ram": metrics.ram,
        "gpus": metrics.gpus,
        "reviewer_resident": metrics.models.as_ref().into_iter().flatten()
            .filter(|model| model.name == reviewer).collect::<Vec<_>>(),
    });
    let _ = write!(
        prompt,
        "\n\nLyra can run up to {} independent fixes with up to {} distinct models at once. Reuse one model for independent fixes when equally suitable.\nLocal hardware snapshot before starting this review (bytes, null means unknown): {system}\nUse inspect_review_resources for current headroom before deciding to overlap a build with diff review. Treat unknown estimates conservatively. Lyra may unload this reviewer before loading a different successor.\nInstalled models eligible for a fix (weights are estimates):\n{models}",
        AI_SETTINGS.parallel_project_tasks, AI_SETTINGS.parallel_project_models,
    );
}

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Stage {
    Implementation,
    Review,
    Fix,
    BatchFix,
    Launch,
}

#[derive(Clone, Deserialize, Serialize)]
pub(super) struct Workflow {
    root_id: u64,
    stage: Stage,
    fixes: usize,
    #[serde(default = "first_round")]
    round: u8,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pending_fixes: VecDeque<Finding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    config: Option<Config>,
}

fn first_round() -> u8 {
    1
}

// Ignore legacy reservation metadata in persisted pending findings.
#[derive(Clone, Deserialize, Serialize)]
struct Finding {
    location: String,
    issue: String,
    check: String,
    model: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    after: Vec<usize>,
}

pub(super) struct NextStep {
    pub(super) name: String,
    pub(super) job: Job,
    // Zero-based indices of previous steps in this group. The store assigns session IDs.
    pub(super) after: Vec<usize>,
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Review {
    Pass { summary: String },
    Fixes { findings: VecDeque<Finding> },
    Blocked { reason: String },
}

impl Workflow {
    pub(super) fn new(root_id: u64, config: Config) -> Self {
        Self {
            root_id,
            stage: Stage::Implementation,
            fixes: 0,
            round: first_round(),
            pending_fixes: VecDeque::new(),
            config: Some(config),
        }
    }

    pub(super) fn root_id(&self) -> u64 {
        self.root_id
    }

    pub(super) fn is_follow_up(&self) -> bool {
        self.stage != Stage::Implementation
    }

    pub(super) fn is_review(&self) -> bool {
        self.stage == Stage::Review
    }

    pub(super) fn reviewer_to_release(&self, successor: &str) -> Option<&str> {
        let reviewer = self.config.as_ref()?.review.model.as_str();
        (matches!(self.stage, Stage::Fix | Stage::BatchFix | Stage::Launch)
            && reviewer != successor)
            .then_some(reviewer)
    }

    pub(super) fn role(&self, requested: Role) -> Option<Role> {
        match self.stage {
            Stage::Implementation => Some(requested),
            Stage::Fix | Stage::BatchFix => Some(Role::Fix),
            Stage::Review => Some(Role::Review),
            Stage::Launch => None,
        }
    }

    pub(super) fn omit_config(&mut self) {
        self.config = None;
        self.pending_fixes.clear();
    }

    pub(super) fn advance(
        &self,
        result: &JobResult,
        candidates: &[Candidate],
    ) -> std::result::Result<Vec<NextStep>, String> {
        let config = self
            .config
            .as_ref()
            .ok_or("Настройки цепочки не найдены.")?;
        if result.truncated {
            return Err("Ответ модели оборвался. Автоматическая цепочка остановлена.".into());
        }
        let mut follow_up = self.clone();
        let next = match self.stage {
            Stage::Implementation => follow_up.review_step(&config.review),
            Stage::Fix => {
                if let Some(fix) = follow_up.next_fix() {
                    return Ok(vec![fix]);
                }
                follow_up.round += 1;
                follow_up.review_step(&config.review)
            }
            Stage::BatchFix => {
                return match read_result(&result.text)? {
                    Review::Pass { summary } if short(&summary, 360) => Ok(Vec::new()),
                    Review::Blocked { reason } if short(&reason, 360) => {
                        Err(format!("Локальный фикс заблокирован: {reason}"))
                    }
                    _ => Err("Локальная модель вернула результат вне формата фикса.".into()),
                };
            }
            Stage::Review | Stage::Launch => match read_result(&result.text)? {
                Review::Pass { summary } if short(&summary, 360) => {
                    if self.stage == Stage::Launch {
                        return Ok(Vec::new());
                    }
                    let Some(launch) = &config.launch else {
                        return Ok(Vec::new());
                    };
                    self.step(
                        Stage::Launch,
                        "Запуск после локальной проверки".into(),
                        &launch.model,
                        launch_prompt(&launch.prompt),
                    )
                }
                Review::Fixes { findings }
                    if self.stage == Stage::Review && !findings.is_empty() =>
                {
                    if self.round >= MAX_REVIEWS {
                        return Err("После третьей проверки остались замечания. Они сохранены в чате проверки.".into());
                    }
                    if findings.len() > 16
                        || config
                            .max_fixes
                            .is_some_and(|limit| self.fixes + findings.len() > limit)
                    {
                        return Err(
                            "Список замечаний превышает оставшийся лимит локальных фиксов.".into(),
                        );
                    }
                    for finding in &findings {
                        if !short(&finding.location, 160)
                            || !short(&finding.issue, 360)
                            || !short(&finding.check, 240)
                        {
                            return Err("В списке фиксов есть замечание без короткого места, описания или критерия проверки.".into());
                        }
                        if !candidates
                            .iter()
                            .any(|candidate| candidate.model == finding.model)
                        {
                            return Err(format!(
                                "Модель для фикса недоступна или не поддерживает Pi: {}",
                                finding.model
                            ));
                        }
                    }
                    return follow_up.fix_batch(&config.review, findings);
                }
                Review::Blocked { reason } if short(&reason, 360) => {
                    return Err(format!("Локальный этап заблокирован: {reason}"));
                }
                _ => return Err("Локальная модель вернула результат вне формата проверки.".into()),
            },
        };
        Ok(vec![next])
    }

    fn fix_batch(
        &mut self,
        review: &Step,
        findings: VecDeque<Finding>,
    ) -> std::result::Result<Vec<NextStep>, String> {
        let mut steps = Vec::with_capacity(findings.len() + 1);
        for (index, finding) in findings.into_iter().enumerate() {
            let mut after = Vec::new();
            for position in &finding.after {
                if !(1..=index).contains(position) {
                    return Err(
                        "Зависимость фикса должна ссылаться на более раннее замечание.".into(),
                    );
                }
                after.push(position - 1);
            }
            after.sort_unstable();
            after.dedup();
            let mut step = self.fix_step(&finding, Stage::BatchFix);
            step.job.parallel = true;
            step.after = after;
            steps.push(step);
        }
        self.round += 1;
        let mut review = self.review_step(review);
        review.after = (0..steps.len()).collect();
        steps.push(review);
        Ok(steps)
    }

    fn review_step(&self, review: &Step) -> NextStep {
        let name = if self.round == 1 {
            "Локальная проверка 1".into()
        } else {
            format!("Проверка {} после локальных фиксов", self.round)
        };
        self.step(
            Stage::Review,
            name,
            &review.model,
            review_prompt(&review.prompt, self.round),
        )
    }

    fn next_fix(&mut self) -> Option<NextStep> {
        let finding = self.pending_fixes.pop_front()?;
        Some(self.fix_step(&finding, Stage::Fix))
    }

    fn fix_step(&mut self, finding: &Finding, stage: Stage) -> NextStep {
        self.fixes += 1;
        let report = serde_json::to_string(finding).expect("serialize fix report");
        let mut prompt = format!(
            "Apply exactly one small local fix for the finding below. Verify that it still applies to the current project files. Read only the smallest relevant section and its immediate callers or tests. Keep other behavior and files intact wherever possible. Run a focused check if the shell is available.\nFinding: {report}"
        );
        if stage == Stage::BatchFix {
            prompt.push_str(r#"

Return one JSON verdict. Use {"status":"pass","summary":"change made or confirmed already fixed"} or {"status":"blocked","reason":"why the fix could not be completed"}. Keep the summary or reason on one line, up to 360 characters."#);
        }
        self.step(
            stage,
            format!("Локальный фикс {}", self.fixes),
            &finding.model,
            prompt,
        )
    }

    fn step(&self, stage: Stage, name: String, model: &str, prompt: String) -> NextStep {
        NextStep {
            name,
            job: Job {
                model: model.to_owned(),
                parallel: false,
                role: None,
                chat_session_id: None,
                origin: MessageOrigin::Lyra,
                status: Status::Queued,
                batch_id: None,
                input: Some(Prompt {
                    prompt,
                    max_output_tokens: None,
                    skills: None,
                    context_tokens: None,
                    retry_prompt: None,
                }),
                result: None,
                workflow: Some(Self {
                    stage,
                    ..self.clone()
                }),
                key: None,
                after: Vec::new(),
                external: false,
                artifacts: Vec::new(),
            },
            after: Vec::new(),
        }
    }
}

fn short(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.chars().count() <= max && !value.contains(['\n', '\r'])
}

fn read_result(text: &str) -> std::result::Result<Review, String> {
    // run_pi prefixes saved results with the model name.
    let answer = text
        .split_once('\n')
        .map_or(text, |(_, answer)| answer)
        .trim();
    let json = answer
        .strip_prefix("```json\n")
        .or_else(|| answer.strip_prefix("```\n"))
        .and_then(|body| body.strip_suffix("\n```"))
        .unwrap_or(answer);
    serde_json::from_str(json)
        .map_err(|_| "Локальная модель не вернула JSON с результатом проверки.".into())
}

fn review_prompt(scope: &str, round: u8) -> String {
    format!(
        r#"Full local review {round} of {MAX_REVIEWS}. Use post-code-review if available. Start from the current full diff for the task, every round. In Git, include staged and unstaged changes against HEAD and new untracked files belonging to the task. List changed paths first, then inspect each relevant file or diff hunk in bounded chunks to fit the context. Verify the whole change against the scope below, including after fixes. Run the project's standard build command for the affected project when a build applies, and run the relevant checks. Treat a confirmed build regression as a blocking finding and continue reviewing the full diff. If the build is required and cannot run, use blocked. Leave code edits to separate fix tasks. Collect all distinct, confirmed blocking findings before replying. Split each finding into one small change with a focused check and order fixes by dependencies.

Return only one JSON object as the final answer:
{{"status":"pass","summary":"checks passed, at most 360 characters"}}
{{"status":"fixes","findings":[{{"location":"file and line or symbol, at most 160 characters","issue":"one concrete problem, at most 360 characters","check":"one focused acceptance check, at most 240 characters","model":"exact installed model tag","after":[]}}]}}
{{"status":"blocked","reason":"why the check cannot be completed, at most 360 characters"}}
Use after for findings touching the same file or requiring earlier results, referring to earlier findings by one-based position. Independent fixes can run in parallel using dynamic file locks and version checks; review runs after all fixes succeed. Never bypass file tools via run_command. Report a blocked verdict if an essential file is locked.
Use pass only after covering the full task diff and confirming the acceptance criteria. For each fix, choose from the provided list and honor any model restrictions in the review scope. Low is only for trivial, fully specified mechanical fixes (typos, literal replacements, formatting); never delegate implementation or fixes requiring reasoning to Low. Medium is the minimum for writing code, debugging and refactoring. High is preferred for complex implementation, architectural or subtle behavioral fixes and review. Escalate after a failed attempt; never downgrade merely to save memory. Unclassified models require evidence of sufficient capability. Prefer a model with a passed Pi check when available. Use blocked if you cannot cover the full change or complete a necessary check. Keep the whole report compact, without source excerpts.

Review scope and acceptance criteria:
{scope}"#
    )
}

fn launch_prompt(scope: &str) -> String {
    format!(
        r#"The local review passed. Perform the requested local startup check through run_command. Limit this task to running and verifying the project. Return only a JSON verdict with one short single-line field and no raw logs:
{{"status":"pass","summary":"observed startup result"}}
{{"status":"blocked","reason":"what prevented startup or verification"}}

Startup goal:
{scope}"#
    )
}
