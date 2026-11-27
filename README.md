# Lyra

Lyra is a local Ollama/Pi worker runtime and dashboard. Codex owns planning and acceptance; Lyra runs queued local tasks, dependencies, guarded file changes, review and fixes through MCP.

## Connect Codex

Start Lyra (on Windows, the repository includes `Start-Lyra.cmd` / `Start-Lyra.ps1`). The default dashboard and MCP server listen on local port 8787. Use the address of your actual Lyra instance if you changed the port.

Open the existing Codex configuration, `~/.codex/config.toml` (on Windows, `%USERPROFILE%\.codex\config.toml`, unless `CODEX_HOME` is configured). Add or update the existing Lyra entry, without duplicating its table:

```toml
[mcp_servers.lyra]
url = "http://127.0.0.1:8787/mcp"
tool_timeout_sec = 3600
```

If Lyra is already configured with another server name, URL, or headers, keep those settings and add only `tool_timeout_sec = 3600` to that server's existing table. If Codex has an explicit `enabled_tools` allowlist, include `enqueue_and_wait` and `await_project_tasks`.

Restart/reconnect Codex after the change and verify the server with `codex mcp list` or the MCP tool list in the client. Use the Lyra orchestrator skill at [.agents/skills/lyra/SKILL.md](.agents/skills/lyra/SKILL.md); open this repository as the Codex workspace or install/copy the updated skill into the skill directory of the workspace you use.

The MCP timeout defaults to 60 seconds in Codex. Lyra's new waits default to 3300 seconds (55 minutes), leaving room for the enqueue and response within the recommended 3600-second client timeout. This does not change the timeout of a single local Pi run. If an HTTP proxy or client has a shorter request timeout, configure it accordingly or use an explicit `timeout_seconds` below the effective limit. The tools require a client that permits long-running MCP requests.

See [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli#other-configuration-options). Lyra defaults to loopback; its optional LAN mode is unauthenticated and should only be used on a trusted network.

## Automatic context sizing

The saved model context setting **Auto** (or an omitted `context_tokens` when no manual setting is saved) chooses a context at dispatch from measured free GPU memory, model weight size, estimated KV cache, the model's context ceiling, and a compute/driver reserve. It is not written back as a manual context. A lone worker can use the available budget; a task marked `parallel: true` reserves a share for configured concurrent tasks and models. Active compatible context profiles are reused; new automatic runners wait when competing allocations are not yet visible or there is insufficient measured headroom. Manual context choices remain supported.

`inspect` exposes `auto_context.solo_tokens` and `parallel_tokens` as estimates. Codex should normally omit `context_tokens` and decide whether concurrency is worthwhile based on those estimates and the task dependencies, rather than impose a small window. The dashboard Auto setting also shows an estimate for a single worker; ordinary Chat/Work and load/pin use the same policy. Actual context is chosen again at launch, not from the preview.

The estimate is not an Ollama allocation guarantee: GPU telemetry and model metadata must be available, and a solo run falls back to the normal provider setting when the estimate cannot be made. Idle unpinned capacity of the same model may be counted on a single GPU (Ollama handles replacement); pinned and active runners are not treated as reclaimable. Multi-GPU fitting is conservative (one GPU's available space, not the sum). FP16 cache is estimated even if a quantized cache is enabled. If Ollama is configured with multiple parallel request slots, set `OLLAMA_NUM_PARALLEL` in Lyra's environment to match the server so sizing includes all slots; the configured Lyra queue concurrency is also accounted for. Benchmarks are unchanged.

## Local generation and task completion

Lyra no longer exposes `max_output_tokens` in its normal MCP calls, queued tasks or AI settings. Direct Ollama generation uses `num_predict = -1`; the Pi extension overrides the Ollama-compatible request's finite generation cap. Old saved task prompts with a cap remain readable, but the cap is ignored. The fixed benchmark budget remains separate so benchmark measurements stay comparable. Model context windows, natural provider limits and existing execution timeouts still apply.

Work agents are instructed to call `finish_task` before their final answer, reporting completed or blocked and their actual checks and remaining work. Lyra records that outcome in `work_progress` but never sends automatic completion-check or continuation prompts. If a worker stops normally without the tool, Lyra returns the original handoff with `confirmation_status: unconfirmed`; an explicit blocked report is still treated as a blocker. A normal stop, even with self-confirmation, is not proof of task acceptance. Provider errors, context/output exhaustion, guarded file conflicts, cancellation and recorded file evidence remain observable. Codex must inspect the result and decide whether to continue; premature stopping is intentionally not solved by a new heuristic.

## Queue work without polling

- `enqueue_and_wait`: submit a settled batch and wait until its work and automatic review/fix rounds complete or require attention. Its `batch` object uses the usual `enqueue_project_tasks` schema and requires a stable `batch_id`. Reuse the same ID and payload only to recover a lost response.
- `enqueue_project_tasks`: submit without waiting if the orchestrator has independent work to do.
- `await_project_tasks`: wait on existing task IDs or a group; follows automatic rounds and unresolved dependencies, including ready external steps. It can omit completed handoffs already read by passing `known_completed_ids`.
- `get_project_tasks`: inspect status or saved results once. Existing clients can still use its legacy 25-second wait, but the orchestrator skill no longer polls it.

Example combined submission (replace project ID and model tag with values returned by Lyra):

```json
{
  "batch": {
    "project_id": 12,
    "batch_id": "feature-design-v1",
    "group_name": "Feature",
    "tasks": [
      {
        "name": "Design contract",
        "model": "your-installed-model",
        "prompt": "Propose a concrete design contract and report constraints."
      }
    ]
  }
}
```

Example waiting on existing tasks:

```json
{
  "project_id": 12,
  "task_ids": [31, 32],
  "known_completed_ids": [31]
}
```

Wait responses contain `project_id`, the selected `task_ids`, a compact `tasks` list, `reason`, and `attention` entries identifying actions by task ID. `tasks` includes automatic review/fix rounds and unresolved prerequisites relevant to the wait; it does not repeat original worker prompts or workflow configuration (except the prompt for a ready external task).

| reason | What to do |
| --- | --- |
| `completed` | Inspect the completed handoffs; the orchestrator still owns acceptance. |
| `attention_required` | Inspect the listed context/output pause, failure, cancellation, interruption, blocker, or ready external step. Handle it before awaiting the same scope again. |
| `timeout` | Await the same IDs again; no tasks are cancelled or resubmitted. |
| `stopped` | Lyra is stopping; do not assume the task succeeded or automatically resume a user-stopped task. |

The dashboard still shows live progress and handles user command approvals. Waiting for a task does not hold a workspace or model slot; the queue runs independently. Closing the MCP connection does not cancel a submitted task. If the combined submission loses its response before you know the IDs, repeat the exact same batch ID and payload or recover the saved IDs with a one-off inspection. Do not create a new batch just because a wait timed out.

Keep important design/architecture acceptance checkpoints separate. Submit a design batch, inspect its result, approve or revise the contract, and only then submit implementation. Automatic waiting does not delegate those decisions to local workers.
