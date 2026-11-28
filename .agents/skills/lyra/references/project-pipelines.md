# Project pipelines

## Required pipelines

Choose one pipeline before dispatching any delegated code change. The default for both is one autonomous batch of all settled steps, using `after` for sequential or information dependencies and `follow_up.review` for automatic independent review, fixes and repeat review. Record each worker's model and scope for final acceptance. Do not await individual leaves simply to approve their handoffs.

| Pipeline | Use when | Sequence |
| --- | --- | --- |
| Fine-grained implementation | Contracts are clear and the change divides into small mechanical or otherwise independently verifiable steps. | Send each implementation leaf to Medium or High in its own chat. Low is reserved for trivial mechanical fixes. Overlap independent changes when shared memory and expected end-to-end speed justify it. Assemble the result, then run the required final review with the strongest eligible independent local reviewer. Delegate confirmed fixes and repeat the review with a model that did not make those edits. |
| Coherent implementation | The change requires coupled design or edits that benefit from one owner per coherent slice. | Assign a capable medium or stronger local model to each coherent slice in the queued graph. Use a fresh step chat where a compact handoff suffices. Run the required final review on the assembled result with the strongest eligible independent local reviewer. Delegate confirmed fixes and repeat the review with a model that did not make those edits. |

Final review is required in both pipelines. Select the strongest locally installed model that can review the full assembled change in the available context and memory, is eligible for Pi, and did not implement or fix the changes under review. Reserve that reviewer before dispatch, including when several workers contribute. Schedule it after the writers if the models cannot coexist in memory. If a suitable independent reviewer is unavailable, report the limitation and use a fallback only when the user authorizes it. The original worker can run focused checks, and the orchestrator can inspect the final outcome in addition to this review.

Plan fresh chats where a short handoff is more useful than retaining the existing working history. Use shared project files as the source of truth. Pass the contract, changed paths, acceptance checks, and blockers, and let the next worker inspect what it needs. Long transcripts and compaction are fallback mechanisms for work that genuinely depends on shared history.

## Delegated delivery and exact file paths

When the user delegates creation or explicitly forbids the orchestrator from writing code, the orchestrator learns the requirements, schedules work, and verifies results. It does not author the deliverable or silently finish a worker's partial edit. Give each worker the requested behavior and acceptance checks. Leave room for the worker to design details the user delegated. Preserve the requested file layout and restrictions on deployment or external actions independently of model choice.

1. **Resolve the destination.** If the user names a chat, find it with `list_projects` and verify the registered working folder and shared Chat or Work mode. Use `read_project_session` for the relevant history when the user refers to earlier work. For sequential work in that chat, use `run_pi` with its project and chat IDs and `mode: work` as the expected mode. Unless the user specifically needs the existing chat's history or direct execution, queue the full graph in the same project; sequential work does not require separate orchestrator calls. Scope all steps to the user's requested folder. Every direct run requires a saved project and chat; queued runs create their own chats. A new folderless project gets an internal workspace on its first chat or task. When a new destination is requested, prepare it in a saved Work chat before queueing implementation: the first guarded mkdir can create a new folder at any authorized absolute path under an existing parent and automatically register it as the only project workspace, provided the internal workspace is empty and no queued work exists. The worker must finish after binding; later runs use the new root. Existing destinations are registered directly. Do not register the broader parent or use shell mkdir. Resolve host-specific names using authorized read-only inspection if the path is unknown.
2. **Coordinate file changes.** Use the project's default folder or select a registered folder with `workspace`. Specify intended files in the prompt. Use plain file paths, not Markdown syntax. Lyra locks each path on its first successful write/edit and checks that existing files match the version the worker read. A new file needs no prior reservation. On FILE_CHANGED reread and reconcile; on FILE_LOCKED continue independent work or report the dependency. Never use shell commands to bypass locks/version checks or mutate shared source/configuration files.
3. **Run the independent review.** Choose an eligible local reviewer according to the required pipeline. Use `run_pi` with `role: review` in a fresh chat for direct work, unless the user explicitly requires all turns in one chat. In the queue, prefer `follow_up.review` on a real final implementation/integration step that depends on all contributions. Use a standalone `role: review` only when the automatic attachment is unsuitable; submit it in the original graph when feasible. Pass the original acceptance criteria. Ask the reviewer to inspect the actual directory and read the exact expected files. A worker's file path or completion claim alone is insufficient.
4. **Close the loop.** If an expected file is missing, misnamed, or fails a check, the automatic reviewer should prescribe a bounded fix and repeat review. Schedule fixes yourself only when the automatic cycle cannot handle the finding or the user required direct control. If an unexpected file already exists, report it and preserve it unless removal is authorized. State which checks actually ran. Maintain the no-deploy rule during verification.

### Example with parallel implementation

For a static page with a settled HTML contract and where separate files fit the request, run styling (`styles.css`) and behavior (`timer.js`) in parallel. Give them shared IDs or interface details, then queue an integration step for `index.html` with `after` pointing to both. Attach automatic review to the integration step and submit the graph together; review the assembled page against the user's requirements. When the deliverable must stay in one HTML file, consider the sequential workflow below if separate passes improve reliability; a single coherent implementation is also valid.

## Split work on a single file

For a substantial single-file deliverable, choose between one coherent implementation and separate passes. Split when simpler reasoning, saved checkpoints or focused checks are likely to outweigh rereading and editing the same file, prompt setup and integration overhead. If splitting, use separate task keys and chats with one checkable outcome per pass. A single coherent implementation followed by independent review is valid when the chosen worker can handle it reliably.

Queue same-file writers with explicit `after` dependencies. For a new file, the first writer creates it and establishes the shared IDs and structure. Each successor reads the current file, makes targeted edits for its pass, and reads the saved result. Pass the contract and short acceptance checks to each chat. Keep earlier contributions in place. Use the strongest eligible independent model for final review and repeat the review after any fixes.

### Worked example with one self-contained landing page

Register the requested folder and identify `landing.html` in each writer's task. Carry the requested limits on file writes, external resources, deployment, and servers into every step. In this example the page works as a directly opened file, including useful content and navigation with JavaScript disabled.

| Step key | Deliverable for this chat | `after` | Write paths |
| --- | --- | --- | --- |
| `landing-structure` | Establish the concept, Russian copy, semantic sections, CTA and anchors. Create the HTML and define IDs or hooks for later passes. | — | `landing.html` |
| `landing-visual` | Read the HTML. Add the original hero visual and embedded CSS. Verify layout down to 320 px, focus states, and reduced-motion styles. Preserve the anchor and interaction hooks. | `landing-structure` | `landing.html` |
| `landing-motion` | Read the saved page. Add embedded JavaScript for scroll reveal and interaction feedback. Keep anchors and content usable without JavaScript and honor reduced motion. | `landing-visual` | `landing.html` |
| Automatic review/fix | Read the assembled file and verify the concept, layout, internal navigation, accessibility, and motion against the original request. Report which checks were possible with the available tools. | Attached to `landing-motion` via `follow_up.review` | Read-only reviewer; fixes assigned automatically |

This is an illustrative three-pass plan, not a required number of tasks. Submit all three passes with one `enqueue_and_wait` and attach `follow_up.review` to the last pass. Omit `parallel` for shared-file writes. Lyra creates the follow-up chats automatically. Report actual stages, checks and blockers after the batch returns.

## Planning and checkpoints

For a nontrivial change, plan before dispatch, using the least coordination that makes the work reliable:

1. **Ground the goal.** Read the relevant project facts or obtain a bounded discovery handoff. List observable user outcomes, required invariants and failure cases. Distinguish verified contracts from proposed choices and unknowns.
2. **Choose a slice.** Prefer a small working vertical path (the necessary data, logic, interface and checks for one outcome) over disconnected layers when the path can be owned and verified coherently. Component-level work is equally valid when its contract is settled, separate ownership avoids conflicts, or useful parallelism reduces total time. A single coherent worker is often best for coupled work only after its capacity check passes. Coupling is a reason to define contracts and order dependent edits, not a reason to exceed a worker's context.
3. **Resolve risk early.** Test a critical uncertain API, feasibility assumption or shared contract with a focused read-only discovery or minimal authorized implementation/check before scheduling its consumers. The orchestrator approves consequential decisions, not a mechanical queue status. Keep ordinary decisions in the technical brief; no approval ceremony for routine work.
4. **Map dependencies and cost.** Identify real information dependencies, overlapping mutations, shared artifacts, and resource constraints. Reserve an eligible independent reviewer. Estimate time to verified completion from observed capability, benchmark/load timings, expected generation and verification, context, memory and coordination; do not use file-count thresholds or invented timing estimates.
5. **Dispatch and adapt.** Submit the entire settled graph, including automatic review/fix cycles, in one call. Encode dependencies in `after`, use `group_brief` for durable shared goals/contracts, and let Lyra pass handoffs and enforce checkpoints. Inspect handoffs when the graph completes or returns a real blocker or unresolved consequential decision; replan only affected remaining work. Preserve existing IDs, accepted decisions, successful work and cancellation. Verify assembled behavior before accepting it.

A useful task has a named deliverable, intended components or paths, prerequisite results, observable acceptance conditions, a focused verification method, and a concise handoff. Use the existing technical specification checklist; do not duplicate it in a second task tracker. The group brief and persisted task graph are the plan. Do not create `tasks/plan.md`, `tasks/todo.md`, or external tracker items unless the user/project explicitly requires them. Checkpoints belong at risky decisions, dependency boundaries and meaningful integrated results; ordinary dependency and verification checkpoints are automatic within the batch, not manual Codex approvals. Ask the user only for consequential unresolved choices they have not delegated. Required independent final review remains unchanged.

For example, when adding a feature with an uncertain storage contract, first confirm the storage behavior, approve the contract, then choose either one coherent data-to-UI slice or independent API/UI work with an explicit integration dependency. If the next feature builds on this one, queue it in the same graph with `after` pointing to the prerequisite's integration step with automatic review; if unrelated and memory permits, it can overlap. Do not perform a separate design run for an obvious one-line fix.

## Plan and submit

Use the technical specification checklist in [the main skill](../SKILL.md#write-actionable-technical-specifications) for every implementation, fix, review and continuation. The orchestrator should investigate relevant code or obtain an explicit discovery handoff before specifying existing APIs. State known contracts, steps, constraints, failure cases, focused verification and handoff; adapt the level of implementation detail to model intelligence. Keep the shared requirements in the group brief, but do not rely on a vague reference when the task needs a precise contract.

Start from the acceptance criteria and existing code boundaries. Give each shallow implementation or fix step a specific file set and a checkable result. Group coupled work when an interface still needs to be designed, a change spans both producer and consumer, or each checkpoint must build. Respect the user's preferred task granularity.

List known intended files in the task prompt, but do not predeclare an allowlist. Use guarded file tools for every source change. Shell commands remain available for inspection and non-conflicting tests/builds, never for bypassing locks, read-version checks, or review restrictions.

Use `after` for prerequisite results and stable ordering on shared files. A prerequisite is ready only after its work and automatic follow-up succeed. Use earlier keys in the same batch or existing task IDs from the same project. Each entry gets its own saved chat. Use a stable `batch_id` when a submission might need to be retried after a lost response. Reuse it only for the same payload.

Read the current `parallel_project_tasks` and `parallel_project_models` values from `inspect`. The first limits active parallel steps. The second limits active model and context combinations. Parallel implementation steps may share a worker model. Keep the reviewer independent. Lyra controls task dispatch and acquires file locks at mutation time. Ollama independently controls residency and the number of requests it can generate for at once.

### Calibrated task briefs

Perform the mandatory [context-fit planning check](models-and-context.md#context-fit-planning) for each implementation, integration and review step before submitting the graph. Estimate complexity, peak working context and expected generation/checks against the selected model's demonstrated capabilities. Use the main skill's task-granularity policy: compare time to verified completion, expected rework, prompt and load overhead, handoffs, integration and orchestrator cost. A larger Auto context does not improve model intelligence. If capacity is materially uncertain, choose a narrower saved milestone or a capable larger-window worker before dispatch. A known insufficient context is sufficient reason to split, even for High. Do not split merely to meet a quota or combine based only on intelligence. Use existing benchmark speed/load timings and prior observed outcomes as imperfect evidence; do not assume parallel speedup on shared GPU hardware. Reserve a short final handoff and schedule substantial integration checks separately when they would overload the worker. Do not prescribe a universal number of files, tokens or steps. The product goal belongs in the group brief, not as the sole instruction to a worker. Every leaf must specify what to change, what to leave untouched, its expected contract, concrete actions and the check.

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
      "prompt": "Goal: expose GET /status as JSON {ready: boolean}. Inspect src/api.rs and the existing routing/state access first; use the established framework and state source, not a new readiness flag. Register the endpoint and serialize the current boolean. Preserve existing routes and error handling; avoid unrelated edits. Acceptance: both ready=true and ready=false serialize as booleans; add focused coverage using the project test conventions. Run the relevant checks and report route, state source, changed paths, actual results and blockers."
    },
    {
      "key": "client",
      "name": "Fix status rendering",
      "role": "implement",
      "model": "<worker-tag>",
      "prompt": "Goal: render readiness from GET /status returning JSON {ready: boolean}. Inspect web/status.js, its callers and existing status UI. Reuse the current fetch/render flow; handle both boolean values, failed HTTP requests and malformed responses without showing false success. Preserve unrelated controls and markup. Acceptance: both values render correctly and an error is distinguishable from ready. Add or run focused checks using existing conventions. Report changed paths, behavior, checks and blockers.",
      "after": [
        "api"
      ],
      "follow_up": {
        "review": {
          "model": "<review-tag>",
          "prompt": "Review the assembled API and UI against the agreed GET /status contract. Inspect the full relevant diff, including new files; verify routing, true/false serialization, client handling of both booleans, error/malformed-response behavior and preserved existing behavior. Run the standard applicable build and relevant tests. Stay read-only. For each confirmed blocker, specify exact evidence/location, correction steps, preserved contracts, focused acceptance checks and suitable fix model. Report actual check results and blockers; do not claim pass if required checks cannot run."
        }
      }
    }
  ]
}
```

The example assumes the response contract is settled. The API and client steps are submitted together, with the client as the final real integration step and the independent reviewer attached. Lyra runs the review and any necessary fixes before the wait returns. Choose an eligible reviewer tag distinct from the implementation tags. If client and API can truly overlap, add a real integration/check step depending on both and attach the review there.

## Example: context-limited website implementation

Suppose the user wants a polished multi-section responsive site with navigation and interaction. A strong model has a small usable context, and a capable Medium has more room for implementation. Inspect their actual windows, skills and benchmarks before deciding. Do not send the entire site brief to High and expect it to decide the design, write every file, reread all output and test everything in one chat.

One possible graph once the design is settled:

| Step | Bounded assignment | Worker choice | Dependency |
| --- | --- | --- | --- |
| structure | Create semantic HTML shell, agreed section/anchor IDs, navigation and content hierarchy. No CSS/JS implementation. Check local fragment targets. | Capable implementation model with adequate context | — |
| visual | Implement the agreed tokens, hero and section layout in CSS; preserve IDs and semantic HTML. Check narrow/wide layout and focus rules. Split hero/sections into ordered passes if the CSS working set would not fit. | Capable implementation model; stronger model for a specifically difficult visual part only if it fits | structure |
| interaction | Implement only the agreed menu/toggle behavior and accessibility state in JS, using existing IDs. Check keyboard behavior and no-JS fallback. | Capable implementation model | structure |
| integration | Wire missing links/resources and run focused integration checks; correct only integration mismatches. | Context-feasible implementation model | visual, interaction |
| review/fix | Inspect actual relevant files and checks against the original requirements; schedule small precise fixes and repeat review. | Strongest independent context-feasible reviewer | automatic follow_up.review on integration |

If the visual contract is unresolved and consequential, a narrow design proposal and Codex approval may precede this batch. Otherwise Codex specifies it up front. In the example the implementation graph is submitted together; ordering and review/fixes are handled by Lyra. This is not a mandatory website template: a tiny static page might be one coherent step, and a large site may need more slices and scoped review gates. For an existing project, use actual components, not invented file names. Do not force separate files if the user requires one file; order targeted passes with after instead. Do not assign High to routine code if that would consume the only eligible independent reviewer.

## Review and fix

Use Lyra's automatic review and fix loop by default: attach `follow_up.review` to the implementation or integration step that completes the slice in the initial batch. Use an exclusive `role: review` step for standalone verification only when the automatic attachment is unsuitable or the user requests it. Give that step `after` dependencies for all other contributions. Include the full intended change and acceptance criteria in the review prompt. For the automatic loop, give the reviewer the eligible fix tags and exclude the reviewer itself, so the repeat review stays independent. Supply `follow_up.launch` when startup verification is part of the goal.

Lyra reviews the assembled diff, schedules small fixes, and waits for all fixes to succeed before a new round, up to three reviews. In an automatic review verdict, each finding should include compact but actionable `instructions` (verified code pointers, correction steps, constraints and edge cases) in addition to `location`, `issue`, `check`, `model` and `after`. Lyra passes those instructions directly to the fix worker. Make Low fixes exact mechanical operations; give Medium concrete steps and contracts; reserve High for subtle fixes that need judgment. Keep the JSON report within the advertised byte budget and omit raw logs or source dumps. For a standalone review, the orchestrator translates the findings into equivalent technical specifications and schedules the workers itself. Independent fixes may overlap. Order fixes that need the same file or each other's results. If the user asked for a coarse slice and wants stronger fix models, give the reviewer the eligible tags to choose from. Low is only for trivial, fully specified mechanical fixes (typos, literal replacements, formatting); never delegate implementation or fixes requiring reasoning to Low. Medium is the minimum for writing code, debugging and refactoring. High is preferred for complex implementation, architectural or subtle behavioral fixes and review. Escalate after a failed attempt; never downgrade merely to save memory. Unclassified models require evidence of sufficient capability. Inspect the final diff and checks before delivering the result.

## Large tasks and group cycles

Use groups when the requested work has several coherent outcomes, or when one implementation and review cycle would have to carry too much context. Keep the user's overall acceptance criteria. For each group, record its deliverable, file or component boundaries, prerequisites, and checks. Agree on interfaces before parallelizing across them. Subdivide each group only when its chosen worker benefits from narrower, independently verifiable steps. Stop when further splitting is unlikely to improve reliable delivery. Give a leaf its own review cycle when it needs to be independently reliable or unblocks another step before the group is complete.

Lyra accepts steps with dependencies inside a named chat group. Use `group_name` for the first batch, then pass its returned `group.id` as `group_id` for all later batches and direct chats belonging to the same task. Automatic review and fix chats inherit the group. Use globally unique task IDs, not the display numbering, for dependencies. Translate the group plan into named keys such as `storage-schema` and `ui-bind`. List prerequisites first and use `after` to link the steps.

1. **Implement the group.** Run independent leaves together only when the memory/model limits and expected end-to-end speed justify concurrency. Order leaves that share files or depend on each other's results.
2. **Integrate and review the group.** Use the real step that assembles the group, such as wiring its components or adding its integration checks. Give it `after` dependencies for every group branch it needs and attach `follow_up.review`. The review prompt identifies the group's complete change, shared interfaces, and acceptance checks. Ask the reviewer to inspect that scope in the working diff and attribute findings before scheduling fixes. Lyra applies the fixes and repeats the group review within its review limit. If there's no integration edit, either attach the loop to a real final implementation step with the required dependencies or use `role: review` for a standalone check and coordinate its fixes and re-review explicitly.
3. **Release dependent groups after verification.** Point a consumer that needs a finished group at its integration step. Lyra's `after` waits for that step and its automatic follow-up, so the consumer also waits for group fixes and the successful repeat review. Queue settled consumers in the same batch; a manually coordinated loop is exceptional, and its consumers must wait until that group passes. Independent groups may overlap at the leaf level.
4. **Verify the whole task.** Make the final integration or review depend on every group result. Give the combined change to the strongest eligible independent local reviewer. Check the original acceptance criteria, cross-group interfaces, and the relevant build and integration tests or startup. If this round needs cross-group fixes, scope them explicitly and repeat the assembled check. Attach an automatic cycle to a real final integration step when appropriate.

### Worked group plan

| Step key | Work | `after` | Review |
| --- | --- | --- | --- |
| `storage-schema` | Update the storage schema | — | Focused check |
| `storage-adapter` | Implement the adapter against the agreed schema in separate files | — | Focused check |
| `storage-integrate` | Wire the adapter and exercise the migration | `storage-schema`, `storage-adapter` | `follow_up.review` for Storage |
| `ui-view` | Build the view against the agreed interface | — | Focused check |
| `ui-bind` | Connect the view to verified storage and add integration coverage | `ui-view`, `storage-integrate` | `follow_up.review` for UI |
| `whole-task-integrate` | Exercise the integrated storage/UI flow, add the necessary cross-component regression check and verify the original request | `storage-integrate`, `ui-bind` | `follow_up.review` for the assembled task |

Submit this entire settled graph together. With a stable interface, `ui-view` can start with the storage leaves. `ui-bind` waits through the Storage review and fixes. The final integration waits through both group cycles, then runs its own automatic assembled review. If no real integration/check work is necessary, use an explicit standalone final review rather than inventing a ceremonial implementation task. Use guarded file tools and obey the shell rules for every executable step.

A batch accepts up to 16 steps and each step accepts up to 16 direct dependencies. For a larger plan, submit batches in dependency order and keep the returned task IDs. Use those IDs in `after` across batches. Use group integration results to join multiple branches. If a group fails, follow its blocked branches and recover the affected work while independent groups continue.

## External steps

Use `external: true` when a user-authorized external tool must perform a step, such as producing an image for integration. The orchestrator runs the tool after the step's prerequisites succeed. Order its writes relative to any local writers and make its consumers depend on the external step. Save outputs inside the step's primary folder. Call `complete_project_step` with its original ID, outcome, summary, and relative artifact paths on success. Lyra supplies those paths to dependent local tasks.

## Follow the queue

Prefer one `enqueue_and_wait` with `batch` containing the entire settled graph (including its review/fix configuration) and a stable `batch_id`. Use `enqueue_project_tasks` only when independent useful work can overlap or you must submit without waiting; then call `await_project_tasks` with the returned task IDs. Do not use `get_project_tasks` in a polling loop.

The server waits asynchronously for queue changes, follows automatic review/fix rounds and unresolved prerequisites, and returns `reason`, `task_ids`, compact `tasks`, and any `attention` entries. Use the returned IDs as the task scope. On subsequent awaits, pass `known_completed_ids` for completed handoffs you have already read; failures and pauses are never suppressed. The default server wait is 3300 seconds; configure Codex's MCP timeout to 3600 seconds (see the repository README). Do not shorten waits merely to watch progress; the dashboard shows it.

- `completed`: all requested tasks and automatic follow-ups completed; inspect the assembled result for final acceptance, or resolve the specific decision for which this batch was deliberately bounded. Do not insert a new review merely because the batch returned.
- `attention_required`: handle the listed paused, failed, cancelled or interrupted task, or run the ready external step with its returned prompt and complete it using `complete_project_step`. Do not await the unchanged blocked scope repeatedly. Independent tasks may continue; await their IDs separately if needed.
- `timeout`: work may still be running; await the same IDs again, omitting already-read completed results. If the MCP transport lost the combined response before IDs arrived, repeat exactly the same batch ID and payload, or recover IDs with a one-off inspection. Never create a second batch as a timeout recovery.
- `stopped`: the Lyra service is stopping; do not assume success or restart tasks automatically.

User command approvals still happen in the dashboard. Manual checkpoints are exceptional: pause only a branch needing an unresolved consequential decision or actionable attention. Do not interrupt a settled dependency graph for routine checks or findings. Read a specific blocked or failed step and its evidence before deciding recovery.

- `queued` or `running` means the task is pending. Respect tool and shell approvals.
- `context_full` or `output_limit` means the step can be recovered and dependent work is waiting. Use the [context recovery procedure](models-and-context.md#recover-an-overflow).
- Use `cancel_project_task` for an authorized cancellation of a queued or context-paused task. A running task can be stopped from its dashboard chat.
- Validate completed results against the goal. Record verification, changed files, and any remaining blockers in the user's language.

### Intentional user cancellation

Treat `cancelled_by_user` in a Pi result or queue result, and a saved user-stop notice, as a stop instruction. Do not automatically retry, re-enqueue, switch models, or create follow-up reviews/fixes for that work. A later explicit user request may resume it. Other independent tasks are not implicitly cancelled.

## Shared group handoff (CONTEXT.md)

A group is a shared work context, not just a visual folder. Create it with `group_name` and `group_brief` in the first enqueue/create-session call. Include the overall goal, constraints, common interfaces and acceptance criteria; keep task-specific details in the individual prompts. For existing groups, `get_group_context` returns the generated Markdown, shared brief and `brief_revision`. `set_group_context` updates only the brief with that revision; preserve concurrent changes and re-read on conflict.

Declare every data dependency with `after` (task keys in the same batch or task IDs from an earlier batch in the same project). For example, a design step that produces an API contract without editing a file must be a prerequisite of both backend and frontend steps. File scopes being disjoint does not make these steps independent. Do not run a consumer in parallel with its producer. Queue results and external-step summaries are committed before the dependent step becomes ready; Lyra passes their text and existing artifact paths. Group chats additionally receive a bounded excerpt of other results, with a full generated CONTEXT.md snapshot available to Work agents via read. Chat-mode workers receive only the excerpt. A snapshot reflects completed work at dispatch, not a live message stream. Every included excerpt section identifies its source line range in CONTEXT.md; shortened sections and overall omission counts are explicit. Work agents should read relevant missing ranges on demand when the excerpt is insufficient, using read with offset and limit. Chat-mode agents must ask for missing details if needed. A saved result incomplete marker means the stored answer itself was truncated; reading the snapshot cannot recover missing original text.

Ask workers to finish with actionable decisions, interfaces, paths, checks and blockers. The orchestrator inspects returned handoffs at final acceptance or when a genuinely necessary decision or recovery requires another batch. Workers consume ordinary predecessor handoffs directly. Read shared context when omitted or truncated evidence is needed. A failed, stopped, or partial result is not a verified decision. Do not write CONTEXT.md into the project repository or ask workers to overwrite shared group memory. Lyra assembles results from separate persisted records.

A queue task with `stop_reason: file_conflict` is blocked, not completed. Re-read current files, inspect the worker's final report and the conflicting task, and schedule only the remaining necessary work after its prerequisites. A resolved conflict does not block the task. Do not use shell to force a change.


## Orchestrator-led checkpoints

This is an exception to autonomous execution, not the standard pipeline. Use the existing queue, group context and brief revision APIs; `after` waits for attached automatic review/fix cycles but does not constitute approval of an unresolved design. Pause only the dependent branch when a consequential decision genuinely cannot be settled up front. Keep unrelated settled work queued.

For such an unsettled design/architecture or other consequential decision:
1. Create the group and its shared user requirements. Enqueue only the discovery/design step(s). Require a concrete, inspectable text proposal and evidence, not an implementation.
2. Use enqueue_and_wait or await_project_tasks on the returned IDs. Inspect the returned handoff; use get_group_context or read_project_session only when necessary for missing context/evidence. A Completed status means the worker run ended normally, not that its proposal or actual work is complete. Inspect finish_task confirmation and the actual result yourself. Lyra does not send a follow-up prompt to obtain a missing confirmation.
3. Personally evaluate whether it meets the original goal, has a coherent idea and is feasible. Accept it, make minor textual corrections yourself, or delegate a substantial revision with targeted feedback. Stay within the user's permission; do not edit implementation code when workers are meant to execute it. No extra model run for a small decision you can settle yourself.
4. Persist the approved contract in the shared group brief using its current revision, preserving requirements and stating which earlier candidate it supersedes. Then submit the entire remaining settled implementation/integration graph with the approved contract, precise execution plans and automatic review/fix configuration; use after for the actual prerequisite IDs. If a prior text proposal is inadequate, do not schedule its implementation.
5. Let the local review/fix loop handle routine technical checks and settled corrections without intervention. Inspect the returned implementation handoffs and assembled result at completion; intervene only if a blocker or contract-changing correction requires it. Personally assess the final result against the user's goal, not merely the review JSON.

For example, a proposed exhibition landing design might specify palette, hierarchy, grid, hero artwork, and mobile treatment. The orchestrator can reject a generic corporate hero, choose an editorial composition, correct the hierarchy and approve a coherent visual contract in CONTEXT.md. Only then should a new chat receive a precise implementation plan (semantic structure, approved visual tokens and layout, anchor behavior, responsive rules, and concrete verification). If a larger conceptual rework is needed, ask the designer for that revision before implementation. This does not authorize writing the implementation instead of the worker.

Do not make ordinary work pass through this process. Both independent and sequential settled tasks belong in one autonomous graph. A manual decision boundary, not a task count, determines when another orchestrator submission is necessary. A task that returns task_incomplete/task_blocked does not release its dependents. Inspect it before deciding on recovery; never override user cancellation.
