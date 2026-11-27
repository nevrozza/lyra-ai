# Project pipelines

## Required pipelines

Choose one pipeline before dispatching any delegated code change. Record each worker's model and scope so the final report can say who implemented, reviewed, and fixed the work.

| Pipeline | Use when | Sequence |
| --- | --- | --- |
| Fine-grained implementation | Contracts are clear and the change divides into small mechanical or otherwise independently verifiable steps. | Send each implementation leaf to Medium or High in its own chat. Low is reserved for trivial mechanical fixes. Overlap independent changes when possible. Assemble the result, then run the required final review with the strongest eligible independent local reviewer. Delegate confirmed fixes and repeat the review with a model that did not make those edits. |
| Coherent implementation | The change requires coupled design or edits that benefit from one owner per coherent slice. | Assign a capable medium or stronger local model directly to each slice. Start a fresh chat for the next slice when it can proceed from a clean handoff. Run the required final review on the assembled result with the strongest eligible independent local reviewer. Delegate confirmed fixes and repeat the review with a model that did not make those edits. |

Final review is required in both pipelines. Select the strongest locally installed model that can review the full assembled change in the available context and memory, is eligible for Pi, and did not implement or fix the changes under review. Reserve that reviewer before dispatch, including when several workers contribute. Schedule it after the writers if the models cannot coexist in memory. If a suitable independent reviewer is unavailable, report the limitation and use a fallback only when the user authorizes it. The original worker can run focused checks, and the orchestrator can inspect the final outcome in addition to this review.

Plan fresh chats early where a step can work from a short handoff. Use shared project files as the source of truth. Pass the contract, changed paths, acceptance checks, and blockers, and let the next worker inspect what it needs. Long transcripts and compaction are fallback mechanisms for work that genuinely depends on shared history.

## Delegated delivery and exact file paths

When the user delegates creation or explicitly forbids the orchestrator from writing code, the orchestrator learns the requirements, schedules work, and verifies results. It does not author the deliverable or silently finish a worker's partial edit. Give each worker the requested behavior and acceptance checks. Leave room for the worker to design details the user delegated. Preserve the requested file layout and restrictions on deployment or external actions independently of model choice.

1. **Resolve the destination.** If the user names a chat, find it with `list_projects` and verify the registered working folder and shared Chat or Work mode. Use `read_project_session` for the relevant history when the user refers to earlier work. For sequential work in that chat, use `run_pi` with its project and chat IDs and `mode: work` as the expected mode. If concurrent execution is requested and separate step chats are acceptable, queue the steps in the same project. Scope all steps to the user's requested folder.
2. **Coordinate file changes.** Use the project's default folder or select a registered folder with `workspace`. Specify intended files in the prompt. Use plain file paths, not Markdown syntax. Lyra locks each path on its first successful write/edit and checks that existing files match the version the worker read. A new file needs no prior reservation. On FILE_CHANGED reread and reconcile; on FILE_LOCKED continue independent work or report the dependency. Never use shell commands to bypass locks/version checks or mutate shared source/configuration files.
3. **Run the independent review.** Choose an eligible local reviewer according to the required pipeline. Use `run_pi` with `role: review` in a fresh chat for direct work, unless the user explicitly requires all turns in one chat. In the queue, make the review depend on every implementation step, or attach `follow_up.review` to the final implementation step. Pass the original acceptance criteria. Ask the reviewer to inspect the actual directory and read the exact expected files. A worker's file path or completion claim alone is insufficient.
4. **Close the loop.** If an expected file is missing, misnamed, or fails a check, delegate a bounded fix with a focused task and the exact intended paths and repeat the relevant review. If an unexpected file already exists, report it and preserve it unless removal is authorized. State which checks actually ran. Maintain the no-deploy rule during verification.

### Example with parallel implementation

For a static page with a settled HTML contract and where separate files fit the request, run styling (`styles.css`) and behavior (`timer.js`) in parallel. Give them shared IDs or interface details, then queue an integration step for `index.html` with `after` pointing to both. Review the assembled page against the user's requirements. When the deliverable must stay in one HTML file, use the sequential workflow below.

## Split work on a single file

Split a substantial single-file deliverable into implementation passes when the requirements have clear handoffs. Use separate task keys and chats for those passes. The file constraint determines which paths may be written and which passes must run in sequence. As a dispatch check, verify that each implementation pass has one outcome and its own task ID or chat ID. A request that hands all the requirements to one worker and then queues a review has one implementation pass.

Queue same-file writers with explicit `after` dependencies. For a new file, the first writer creates it and establishes the shared IDs and structure. Each successor reads the current file, makes targeted edits for its pass, and reads the saved result. Pass the contract and short acceptance checks to each chat. Keep earlier contributions in place. Use the strongest eligible independent model for final review and repeat the review after any fixes.

### Worked example with one self-contained landing page

Register the requested folder and identify `landing.html` in each writer's task. Carry the requested limits on file writes, external resources, deployment, and servers into every step. In this example the page works as a directly opened file, including useful content and navigation with JavaScript disabled.

| Step key | Deliverable for this chat | `after` | Write paths |
| --- | --- | --- | --- |
| `landing-structure` | Establish the concept, Russian copy, semantic sections, CTA and anchors. Create the HTML and define IDs or hooks for later passes. | — | `landing.html` |
| `landing-visual` | Read the HTML. Add the original hero visual and embedded CSS. Verify layout down to 320 px, focus states, and reduced-motion styles. Preserve the anchor and interaction hooks. | `landing-structure` | `landing.html` |
| `landing-motion` | Read the saved page. Add embedded JavaScript for scroll reveal and interaction feedback. Keep anchors and content usable without JavaScript and honor reduced motion. | `landing-visual` | `landing.html` |
| `landing-review` | Read the assembled file and verify the concept, layout, internal navigation, accessibility, and motion against the original request. Report which checks were possible with the available tools. | `landing-motion` | Read-only file tools with `role: review` |

Omit `parallel` for the shared-file writes. Check that the queue returns three implementation task IDs and a separate review task ID in this example. Report the actual stages, checks, and any blockers after completion.

## Plan and submit

Use the technical specification checklist in [the main skill](../SKILL.md#write-actionable-technical-specifications) for every implementation, fix, review and continuation. The orchestrator should investigate relevant code or obtain an explicit discovery handoff before specifying existing APIs. State known contracts, steps, constraints, failure cases, focused verification and handoff; adapt the level of implementation detail to model intelligence. Keep the shared requirements in the group brief, but do not rely on a vague reference when the task needs a precise contract.

Start from the acceptance criteria and existing code boundaries. Give each shallow implementation or fix step a specific file set and a checkable result. Group coupled work when an interface still needs to be designed, a change spans both producer and consumer, or each checkpoint must build. Respect the user's preferred task granularity.

List known intended files in the task prompt, but do not predeclare an allowlist. Use guarded file tools for every source change. Shell commands remain available for inspection and non-conflicting tests/builds, never for bypassing locks, read-version checks, or review restrictions.

Use `after` for prerequisite results and stable ordering on shared files. A prerequisite is ready only after its work and automatic follow-up succeed. Use earlier keys in the same batch or existing task IDs from the same project. Each entry gets its own saved chat. Use a stable `batch_id` when a submission might need to be retried after a lost response. Reuse it only for the same payload.

Read the current `parallel_project_tasks` and `parallel_project_models` values from `inspect`. The first limits active parallel steps. The second limits active model and context combinations. Parallel implementation steps may share a worker model. Keep the reviewer independent. Lyra controls task dispatch and acquires file locks at mutation time. Ollama independently controls residency and the number of requests it can generate for at once.

### Calibrated task briefs

Estimate the expected generated work and verification against the selected model's usable generation capacity, as well as its input window. If uncertain, use a smaller saved milestone and inspect the handoff before assigning the next one. Split along coherent artifacts or sequential checkpoints even within one file; keep independent steps parallel only when contracts are settled. Do not prescribe a universal number of files, tokens or steps. Reserve a short final handoff and schedule substantial integration checks separately when they would overload the implementation worker.

A good brief reduces the decisions a small worker must make without specifying the entire solution. For a nontrivial Medium step, use a handful of ordered actions, usually 4–7; this is a guide, not a quota. For a simple change use fewer, for complex work split by coherent outcomes and explicit dependencies. Do not expand a brief with repeated global constraints or generic best practices.

Illustrative greenfield website (not a claim about an existing project):

**Shared brief:** Russian exhibition landing page, locally opened index.html, no dependencies/network/deploy; preserve the user's accessibility and responsive criteria.

**Design step, read-only.** Goal: produce a buildable visual contract. 1. Verify the registered workspace and skill availability; report which design skill was actually loaded. 2. Choose a coherent visual principle, palette, type hierarchy and composition; define a desktop grid and a single-column mobile interpretation. 3. Decide the hero, program, visit section and CTA hierarchy, including how content remains readable on small screens. 4. Hand off palette hex values, typography, layout rules, relevant contrast calculations if actually performed, and any unresolved decisions. Do not edit files or claim a rendered page was checked.

**Implementation step, after design.** Goal: implement the agreed composition using index.html and styles.css. 1. Read the design handoff and any omitted relevant CONTEXT.md ranges; verify the workspace and existing files. 2. Build semantic header/main/sections/footer, a skip link targeting main, and a primary CTA targeting the visit section; use matching local fragment IDs. 3. Put the agreed palette and typography into CSS tokens, then implement the hero composition and program/visit layouts using the design contract. Choose the exact CSS technique within those constraints; use local system fonts if no local font resource is available. 4. Adapt the layout to 360 px with no fixed-width overflow; preserve visible keyboard focus, meaningful contrast and reduced-motion behavior. Use no JavaScript unless required. 5. Inspect the saved files, resolve relative resources and fragment targets; if a browser is available, render at 360 and 1440 px, compare scrollWidth to viewport width, tab through controls and check the CTA. Otherwise distinguish static inspection from unverified runtime behavior. Return changed files, actual check results, design deviations and blockers.

The example intentionally gives the structure, decisions and checks, not full HTML/CSS or a new visual design overriding the predecessor. In a real batch use the actual user requirements and verified project facts. Do not turn this example into a mandatory template for unrelated tasks.

### Example batch

Replace model placeholders with exact inspected catalog tags and choose the project's actual ID and file paths. The following is the usual batch payload: pass it inside `batch` to `enqueue_and_wait` (with a unique stable `batch_id`), or directly to `enqueue_project_tasks` only when you need to overlap independent work.

```json
{
  "project_id": 12,
  "batch_id": "status-feature-v1",
  "tasks": [
    {
      "key": "api",
      "name": "Implement the agreed API",
      "role": "implement",
      "model": "<worker-tag>",
      "prompt": "Goal: expose GET /status as JSON {ready: boolean}. Inspect src/api.rs and the existing routing/state access first; use the established framework and state source, not a new readiness flag. Register the endpoint and serialize the current boolean. Preserve existing routes and error handling; avoid unrelated edits. Acceptance: both ready=true and ready=false serialize as booleans; add focused coverage using the project test conventions. Run the relevant checks and report route, state source, changed paths, actual results and blockers.",
      "parallel": true
    },
    {
      "key": "client",
      "name": "Fix status rendering",
      "role": "fix",
      "model": "<worker-tag>",
      "prompt": "Goal: render readiness from GET /status returning JSON {ready: boolean}. Inspect web/status.js, its callers and existing status UI. Reuse the current fetch/render flow; handle both boolean values, failed HTTP requests and malformed responses without showing false success. Preserve unrelated controls and markup. Acceptance: both values render correctly and an error is distinguishable from ready. Add or run focused checks using existing conventions. Report changed paths, behavior, checks and blockers.",
      "parallel": true
    },
    {
      "key": "review",
      "name": "Verify the assembled change",
      "role": "review",
      "model": "<review-tag>",
      "prompt": "Review the assembled API and UI against the agreed GET /status contract. Inspect the full relevant diff, including new files; verify routing, true/false serialization, client handling of both booleans, error/malformed-response behavior and preserved existing behavior. Run the standard applicable build and relevant tests. Stay read-only. For each confirmed blocker, specify exact evidence/location, correction steps, preserved contracts, focused acceptance checks and suitable fix model. Report actual check results and blockers; do not claim pass if required checks cannot run.",
      "after": ["api", "client"]
    }
  ]
}
```

The example assumes the response contract is settled and both steps can use the same worker model within Lyra's limits. Add `"after": ["api"]` to the client step when the client needs the API implementation first.

## Review and fix

Use an exclusive `role: review` step for a standalone verification. For Lyra's automatic review and fix loop, attach `follow_up.review` to the implementation or integration step that completes the slice. Give that step `after` dependencies for all other contributions. Include the full intended change and acceptance criteria in the review prompt. For the automatic loop, give the reviewer the eligible fix tags and exclude the reviewer itself, so the repeat review stays independent. Supply `follow_up.launch` when startup verification is part of the goal.

Lyra reviews the assembled diff, schedules small fixes, and waits for all fixes to succeed before a new round, up to three reviews. In an automatic review verdict, each finding should include compact but actionable `instructions` (verified code pointers, correction steps, constraints and edge cases) in addition to `location`, `issue`, `check`, `model` and `after`. Lyra passes those instructions directly to the fix worker. Make Low fixes exact mechanical operations; give Medium concrete steps and contracts; reserve High for subtle fixes that need judgment. Keep the JSON report within the advertised byte budget and omit raw logs or source dumps. For a standalone review, the orchestrator translates the findings into equivalent technical specifications and schedules the workers itself. Independent fixes may overlap. Order fixes that need the same file or each other's results. If the user asked for a coarse slice and wants stronger fix models, give the reviewer the eligible tags to choose from. Low is only for trivial, fully specified mechanical fixes (typos, literal replacements, formatting); never delegate implementation or fixes requiring reasoning to Low. Medium is the minimum for writing code, debugging and refactoring. High is preferred for complex implementation, architectural or subtle behavioral fixes and review. Escalate after a failed attempt; never downgrade merely to save memory. Unclassified models require evidence of sufficient capability. Inspect the final diff and checks before delivering the result.

## Large tasks and group cycles

Use groups when the requested work has several coherent outcomes, or when one implementation and review cycle would have to carry too much context. Keep the user's overall acceptance criteria. For each group, record its deliverable, file or component boundaries, prerequisites, and checks. Agree on interfaces before parallelizing across them. Split each group again into work a local model can implement or fix and verify. Stop splitting when a step has a concrete scope and result. Give a leaf its own review cycle when it needs to be independently reliable or unblocks another step before the group is complete.

Lyra accepts steps with dependencies inside a named chat group. Use `group_name` for the first batch, then pass its returned `group.id` as `group_id` for all later batches and direct chats belonging to the same task. Automatic review and fix chats inherit the group. Use globally unique task IDs, not the display numbering, for dependencies. Translate the group plan into named keys such as `storage-schema` and `ui-bind`. List prerequisites first and use `after` to link the steps.

1. **Implement the group.** Run independent leaves together when their work is independent and the model limits allow it. Order leaves that share files or depend on each other's results.
2. **Integrate and review the group.** Use the real step that assembles the group, such as wiring its components or adding its integration checks. Give it `after` dependencies for every group branch it needs and attach `follow_up.review`. The review prompt identifies the group's complete change, shared interfaces, and acceptance checks. Ask the reviewer to inspect that scope in the working diff and attribute findings before scheduling fixes. Lyra applies the fixes and repeats the group review within its review limit. If there's no integration edit, either attach the loop to a real final implementation step with the required dependencies or use `role: review` for a standalone check and coordinate its fixes and re-review explicitly.
3. **Release dependent groups after verification.** Point a consumer that needs a finished group at its integration step. Lyra's `after` waits for that step and its automatic follow-up, so the consumer also waits for group fixes and the successful repeat review. For a manually coordinated loop, queue consumers only when the group passes. Independent groups may overlap at the leaf level.
4. **Verify the whole task.** Make the final integration or review depend on every group result. Give the combined change to the strongest eligible independent local reviewer. Check the original acceptance criteria, cross-group interfaces, and the relevant build and integration tests or startup. If this round needs cross-group fixes, scope them explicitly and repeat the assembled check. Attach an automatic cycle to a real final integration step when appropriate.

### Worked group plan

| Step key | Work | `after` | Review |
| --- | --- | --- | --- |
| `storage-schema` | Update the storage schema | — | Focused check |
| `storage-adapter` | Implement the adapter against the agreed schema in separate files | — | Focused check |
| `storage-integrate` | Wire the adapter and exercise the migration | `storage-schema`, `storage-adapter` | `follow_up.review` for Storage |
| `ui-view` | Build the view against the agreed interface | — | Focused check |
| `ui-bind` | Connect the view to verified storage and add integration coverage | `ui-view`, `storage-integrate` | `follow_up.review` for UI |
| `whole-task-review` | Check storage and UI together against the original request | `storage-integrate`, `ui-bind` | Standalone `role: review` |

With a stable interface, `ui-view` can start with the storage leaves. `ui-bind` waits through the Storage review and fixes. Use guarded file tools and obey the shell rules for every executable step.

A batch accepts up to 16 steps and each step accepts up to 16 direct dependencies. For a larger plan, submit batches in dependency order and keep the returned task IDs. Use those IDs in `after` across batches. Use group integration results to join multiple branches. If a group fails, follow its blocked branches and recover the affected work while independent groups continue.

## External steps

Use `external: true` when a user-authorized external tool must perform a step, such as producing an image for integration. The orchestrator runs the tool after the step's prerequisites succeed. Order its writes relative to any local writers and make its consumers depend on the external step. Save outputs inside the step's primary folder. Call `complete_project_step` with its original ID, outcome, summary, and relative artifact paths on success. Lyra supplies those paths to dependent local tasks.

## Follow the queue

Prefer `enqueue_and_wait` with `batch` containing the normal enqueue payload and a stable `batch_id`. Use `enqueue_project_tasks` only when independent useful work can overlap or you must submit without waiting; then call `await_project_tasks` with the returned task IDs. Do not use `get_project_tasks` in a polling loop.

The server waits asynchronously for queue changes, follows automatic review/fix rounds and unresolved prerequisites, and returns `reason`, `task_ids`, compact `tasks`, and any `attention` entries. Use the returned IDs as the task scope. On subsequent awaits, pass `known_completed_ids` for completed handoffs you have already read; failures and pauses are never suppressed. The default server wait is 3300 seconds; configure Codex's MCP timeout to 3600 seconds (see the repository README). Do not shorten waits merely to watch progress; the dashboard shows it.

- `completed`: all requested tasks and automatic follow-ups completed; inspect the results and approve the next decision yourself.
- `attention_required`: handle the listed paused, failed, cancelled or interrupted task, or run the ready external step with its returned prompt and complete it using `complete_project_step`. Do not await the unchanged blocked scope repeatedly. Independent tasks may continue; await their IDs separately if needed.
- `timeout`: work may still be running; await the same IDs again, omitting already-read completed results. If the MCP transport lost the combined response before IDs arrived, repeat exactly the same batch ID and payload, or recover IDs with a one-off inspection. Never create a second batch as a timeout recovery.
- `stopped`: the Lyra service is stopping; do not assume success or restart tasks automatically.

User command approvals still happen in the dashboard. Keep acceptance checkpoints between batches: an implementation must not be submitted before Codex approves its unsettled design. Read a specific blocked or failed step and its evidence before deciding recovery.

- `queued` or `running` means the task is pending. Respect tool and shell approvals.
- `context_full` or `output_limit` means the step can be recovered and dependent work is waiting. Use the [context recovery procedure](models-and-context.md#recover-an-overflow).
- Use `cancel_project_task` for an authorized cancellation of a queued or context-paused task. A running task can be stopped from its dashboard chat.
- Validate completed results against the goal. Record verification, changed files, and any remaining blockers in the user's language.

### Intentional user cancellation

Treat `cancelled_by_user` in a Pi result or queue result, and a saved user-stop notice, as a stop instruction. Do not automatically retry, re-enqueue, switch models, or create follow-up reviews/fixes for that work. A later explicit user request may resume it. Other independent tasks are not implicitly cancelled.

## Shared group handoff (CONTEXT.md)

A group is a shared work context, not just a visual folder. Create it with `group_name` and `group_brief` in the first enqueue/create-session call. Include the overall goal, constraints, common interfaces and acceptance criteria; keep task-specific details in the individual prompts. For existing groups, `get_group_context` returns the generated Markdown, shared brief and `brief_revision`. `set_group_context` updates only the brief with that revision; preserve concurrent changes and re-read on conflict.

Declare every data dependency with `after` (task keys in the same batch or task IDs from an earlier batch in the same project). For example, a design step that produces an API contract without editing a file must be a prerequisite of both backend and frontend steps. File scopes being disjoint does not make these steps independent. Do not run a consumer in parallel with its producer. Queue results and external-step summaries are committed before the dependent step becomes ready; Lyra passes their text and existing artifact paths. Group chats additionally receive a bounded excerpt of other results, with a full generated CONTEXT.md snapshot available to Work agents via read. Chat-mode workers receive only the excerpt. A snapshot reflects completed work at dispatch, not a live message stream. Every included excerpt section identifies its source line range in CONTEXT.md; shortened sections and overall omission counts are explicit. Work agents should read relevant missing ranges on demand when the excerpt is insufficient, using read with offset and limit. Chat-mode agents must ask for missing details if needed. A saved result incomplete marker means the stored answer itself was truncated; reading the snapshot cannot recover missing original text.

Ask workers to finish with actionable decisions, interfaces, paths, checks and blockers. The orchestrator must inspect the returned handoffs before the next batch and read shared context when omitted or truncated details are needed. A failed, stopped, or partial result is not a verified decision. Do not write CONTEXT.md into the project repository or ask workers to overwrite shared group memory. Lyra assembles results from separate persisted records.

A queue task with `stop_reason: file_conflict` is blocked, not completed. Re-read current files, inspect the worker's final report and the conflicting task, and schedule only the remaining necessary work after its prerequisites. A resolved conflict does not block the task. Do not use shell to force a change.


## Orchestrator-led checkpoints

Use the existing queue, group context and brief revision APIs; do not invent an approval API or rely on after as a quality gate. The orchestrator is the lead developer, not another autonomous worker.

For an unsettled design/architecture or other consequential decision:
1. Create the group and its shared user requirements. Enqueue only the discovery/design step(s). Require a concrete, inspectable text proposal and evidence, not an implementation.
2. Use enqueue_and_wait or await_project_tasks on the returned IDs. Inspect the returned handoff; use get_group_context or read_project_session only when necessary for missing context/evidence. A Completed status means the worker run ended normally, not that its proposal or actual work is complete. Inspect finish_task confirmation and the actual result yourself. Lyra does not send a follow-up prompt to obtain a missing confirmation.
3. Personally evaluate whether it meets the original goal, has a coherent idea and is feasible. Accept it, make minor textual corrections yourself, or delegate a substantial revision with targeted feedback. Stay within the user's permission; do not edit implementation code when workers are meant to execute it. No extra model run for a small decision you can settle yourself.
4. Persist the approved contract in the shared group brief using its current revision, preserving requirements and stating which earlier candidate it supersedes. Only after that create the consuming implementation tasks, with the approved contract and precise execution plans; use after for the actual prerequisite IDs. If a prior text proposal is inadequate, do not schedule its implementation.
5. Read implementation handoffs and independently verify the assembled result. Use the local review/fix loop for technical checks and settled corrections, or manually coordinate fixes when their proposed approach needs your approval. Personally assess the final result against the user's goal, not merely the review JSON.

For example, a proposed exhibition landing design might specify palette, hierarchy, grid, hero artwork, and mobile treatment. The orchestrator can reject a generic corporate hero, choose an editorial composition, correct the hierarchy and approve a coherent visual contract in CONTEXT.md. Only then should a new chat receive a precise implementation plan (semantic structure, approved visual tokens and layout, anchor behavior, responsive rules, and concrete verification). If a larger conceptual rework is needed, ask the designer for that revision before implementation. This does not authorize writing the implementation instead of the worker.

Do not make every simple task pass through this process. Independent settled tasks can still be batched; meaningful checkpoints, not arbitrary task count, determine the handoff boundaries. A task that returns task_incomplete/task_blocked does not release its dependents. Inspect it before deciding on recovery; never override user cancellation.
