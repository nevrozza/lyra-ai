# Project pipelines

## Plan and submit

Start from the acceptance criteria and existing code boundaries. Give each shallow implementation or fix step a specific file set and a checkable result. Group coupled work when an interface still needs to be designed, a change spans both producer and consumer, or each checkpoint must build. Respect the user's preferred task granularity.

For scoped steps, list every file that may change, including a fixture or lockfile when it genuinely belongs to the step. Paths are relative to the first working folder and use forward slashes. Unscoped steps run exclusively and can request shell commands under Lyra's global Pi command mode. Put builds, formatting, or generators that require shell into a step with shell access. If a worker needs to edit outside its assigned scope, inspect the new requirement before scheduling the remaining work.

Use `after` for prerequisite results and stable ordering on shared files. A prerequisite is ready only after its work and automatic follow-up succeed. Use earlier keys in the same batch or existing task IDs from the same project. Each entry gets its own saved chat. Use a stable `batch_id` when a submission might need to be retried after a lost response. Reuse it only for the same payload.

Read the current `parallel_project_tasks` and `parallel_project_models` values from `inspect`. The first limits active parallel steps. The second limits active model and context combinations. Prefer a common model for comparably suitable parallel steps. Lyra controls task dispatch and file reservations. Ollama independently controls residency and the number of requests it can generate for at once.

### Example batch

Replace model placeholders with exact inspected catalog tags and choose the project's actual ID and file paths.

```json
{
  "project_id": 12,
  "tasks": [
    {
      "key": "api",
      "name": "Implement the agreed API",
      "role": "implement",
      "model": "<worker-tag>",
      "prompt": "Implement the agreed GET /status contract in src/api.rs. Return {ready: boolean}. Report changed paths.",
      "parallel": true,
      "write_paths": ["src/api.rs"]
    },
    {
      "key": "client",
      "name": "Fix status rendering",
      "role": "fix",
      "model": "<worker-tag>",
      "prompt": "Render the ready flag in web/status.js using the agreed GET /status response {ready: boolean}. Report changed paths.",
      "parallel": true,
      "write_paths": ["web/status.js"]
    },
    {
      "key": "review",
      "name": "Verify the assembled change",
      "role": "review",
      "model": "<review-tag>",
      "prompt": "Build the project, check both changes against GET /status returning {ready: boolean}, and report findings and check results.",
      "after": ["api", "client"]
    }
  ]
}
```

The example assumes the response contract is settled and both steps can use the same worker model within Lyra's limits. Add `"after": ["api"]` to the client step when the client needs the API implementation first.

## Review and fix

Use an exclusive `role: review` step for a standalone verification. For Lyra's automatic review and fix loop, attach `follow_up.review` to the implementation or integration step that completes the slice. Give that step `after` dependencies for all other contributions. Include the full intended change and acceptance criteria in the review prompt. Supply `follow_up.launch` when startup verification is part of the goal.

Lyra reviews the assembled diff, schedules small fixes, and waits for all fixes to succeed before a new round, up to three reviews. Independent fixes with known disjoint scopes may overlap. A fix with unknown scope or a shell requirement runs exclusively. If the user asked for a coarse slice and wants stronger fix models, give the reviewer the eligible tags to choose from. For mechanical fixes, permit a suitable lightweight model. Inspect the final diff and checks before delivering the result.

## Large tasks and group cycles

Use groups when the requested work has several coherent outcomes, or when one implementation and review cycle would have to carry too much context. Keep the user's overall acceptance criteria. For each group, record its deliverable, file or component boundaries, prerequisites, and checks. Agree on interfaces before parallelizing across them. Split each group again into work a local model can implement or fix and verify. Stop splitting when a step has a concrete scope and result. Give a leaf its own review cycle when it needs to be independently reliable or unblocks another step before the group is complete.

Lyra accepts a flat list of steps with dependencies. Translate the group plan into named keys such as `storage-schema` and `ui-bind`. List prerequisites first and use `after` to link the steps.

1. **Implement the group.** Run independent leaves together when their `write_paths` are disjoint and the model limits allow it. Order leaves that share files or depend on each other's results.
2. **Integrate and review the group.** Use the real step that assembles the group, such as wiring its components or adding its integration checks. Give it `after` dependencies for every group branch it needs and attach `follow_up.review`. The review prompt identifies the group's complete change, shared interfaces, and acceptance checks. Ask the reviewer to inspect that scope in the working diff and attribute findings before scheduling fixes. Lyra applies the fixes and repeats the group review within its review limit. If there's no integration edit, either attach the loop to a real final implementation step with the required dependencies or use `role: review` for a standalone check and coordinate its fixes and re-review explicitly.
3. **Release dependent groups after verification.** Point a consumer that needs a finished group at its integration step. Lyra's `after` waits for that step and its automatic follow-up, so the consumer also waits for group fixes and the successful repeat review. For a manually coordinated loop, queue consumers only when the group passes. Independent groups may overlap at the leaf level.
4. **Verify the whole task.** Make the final integration or review depend on every group result. Check the original acceptance criteria, cross-group interfaces, and the relevant build and integration tests or startup. If this round needs cross-group fixes, scope them explicitly and repeat the assembled check. Attach an automatic cycle to a real final integration step when appropriate.

### Worked group plan

| Step key | Work | `after` | Review |
| --- | --- | --- | --- |
| `storage-schema` | Update the storage schema | — | Focused check |
| `storage-adapter` | Implement the adapter against the agreed schema in separate files | — | Focused check |
| `storage-integrate` | Wire the adapter and exercise the migration | `storage-schema`, `storage-adapter` | `follow_up.review` for Storage |
| `ui-view` | Build the view against the agreed interface | — | Focused check |
| `ui-bind` | Connect the view to verified storage and add integration coverage | `ui-view`, `storage-integrate` | `follow_up.review` for UI |
| `whole-task-review` | Check storage and UI together against the original request | `storage-integrate`, `ui-bind` | Standalone `role: review` |

With a stable interface, `ui-view` can start with the storage leaves. `ui-bind` waits through the Storage review and fixes. Apply `write_paths` and the shell rules to each executable step.

A batch accepts up to 16 steps and each step accepts up to 16 direct dependencies. For a larger plan, submit batches in dependency order and keep the returned task IDs. Use those IDs in `after` across batches. Use group integration results to join multiple branches. If a group fails, follow its blocked branches and recover the affected work while independent groups continue.

## External steps

Use `external: true` when a user-authorized external tool must perform a step, such as producing an image for integration. The orchestrator runs the tool after the step's prerequisites succeed. Order its writes relative to any local writers and make its consumers depend on the external step. Save outputs inside the step's primary folder. Call `complete_project_step` with its original ID, outcome, summary, and relative artifact paths on success. Lyra supplies those paths to dependent local tasks.

## Follow the queue

Call `get_project_tasks` with the returned task IDs and a bounded `wait_seconds`. It includes automatic follow-ups attached to those task IDs. Continue through actual completion and inspect outcomes. Use separate short checks while an external step needs action. Read the specific blocked or failed step and follow its dependency chain before reporting an issue.

- `queued` or `running` means the task is pending. Respect tool and shell approvals.
- `context_full` means the step can be recovered and dependent work is waiting. Use the [context recovery procedure](models-and-context.md#recover-an-overflow).
- Use `cancel_project_task` for an authorized cancellation of a queued or context-paused task. A running task can be stopped from its dashboard chat.
- Validate completed results against the goal. Record verification, changed files, and any remaining blockers in the user's language.
