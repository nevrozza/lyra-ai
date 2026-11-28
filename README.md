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

Restart/reconnect Codex after the change and verify the server with `codex mcp list` or the MCP tool list in the client. Lyra now serves the current orchestrator guide through MCP. The server's initialize instructions tell the client to call `get_orchestrator_skill` before delegating a new task; omit `path` for SKILL.md and read references by their relative path as needed. The same files are available via standard `resources/list` and `resources/read` with `lyra://orchestrator/...` URIs. The guide and references are embedded from [.agents/skills/lyra/](.agents/skills/lyra/) at build time, so there is one source of truth per running Lyra version. This is on-demand instructions delivery, not automatic installation of a native Codex skill. Clients that do not honor server instructions may need a short local AGENTS.md rule: "When delegating through Lyra, call get_orchestrator_skill and follow the current guide." The local skill remains available as a fallback. If using a tool allowlist, include `get_orchestrator_skill`. Restart/reconnect after upgrading Lyra.

### Connect MCP servers to Lyra Work

Open **Серверы MCP** in the local dashboard and add a server using the form or JSON editor. Codex can also manage the same configuration using `list_mcp_servers`, `save_mcp_server`, `set_mcp_server_enabled`, `remove_mcp_server`, and `test_mcp_server`. Saving a partial server object patches existing fields; omitted credentials are preserved. Use the tools only for user-authorized connections. The test performs MCP initialization and tool discovery without model inference or invoking any tool; HTTP(S) testing requires Node.js on PATH. It tests the protocol connection, not Pi's final exposed tool allowlist. Settings apply only to future Work runs. Pi takes a fresh snapshot for each Work run, including queued Work. Compatible Pi builds must provide the built-in `builtin:mcp` extension.

```json
{
  "mcpServers": {
    "time": { "command": "uvx", "args": ["mcp-server-time"], "enabled": false },
    "docs": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer ${DOCS_TOKEN}" },
      "toolExposure": { "write_document": "hidden" },
      "enabled": false
    }
  }
}
```

Set `enabled` to `true` for servers you want Pi to connect to, or remove an entry to delete it. Local servers use an executable and separate arguments. Remote servers use Streamable HTTP. Put secrets in the Lyra process environment and reference them as `${NAME}`. The dashboard supports header-based authentication. Tool exposure defaults to `direct`; use `hidden` with selected `direct` tool overrides for an allowlist. Check Pi activity for discovery and connection errors. Chat, read-only Review, and the unauthenticated LAN mode do not load these servers.

The MCP timeout defaults to 60 seconds in Codex. Lyra's new waits default to 3300 seconds (55 minutes), leaving room for the enqueue and response within the recommended 3600-second client timeout. This does not change the timeout of a single local Pi run. If an HTTP proxy or client has a shorter request timeout, configure it accordingly or use an explicit `timeout_seconds` below the effective limit. The tools require a client that permits long-running MCP requests.

See [Codex MCP configuration](https://learn.chatgpt.com/docs/extend/mcp?surface=cli#other-configuration-options). Lyra defaults to loopback; its optional LAN mode is unauthenticated and should only be used on a trusted network.

## Automatic context sizing

The saved **Auto** setting (or an omitted `context_tokens` without a saved manual setting) prefers the largest compatible verified window from the full benchmark. A lone worker gets the GPU's usable free memory with 256 MiB of device headroom. A measured peak already includes compute buffers. Without a compatible profile, Auto estimates runner memory from model weights, cache, and compute buffers. The dashboard and MCP use the same budget and estimates. When independent parallel work is ready, the queue shares the budget across distinct runners and accounts for the slots that can share a runner. If the first job cannot fit a shared budget, it can start with the full budget and later jobs wait until enough memory is available. Manual choices remain supported.

`inspect` exposes `auto_context.solo_tokens`, `parallel_tokens`, their source and the reason for selection or unavailability, together with the model's benchmark profile. Codex should normally omit `context_tokens` and decide whether concurrency is worthwhile based on those estimates and the task dependencies, rather than impose a small window. Choose task granularity independently of context capacity: use model capability, measured timings and prior outcomes, expected rework, handoff/integration cost, and GPU contention to decide between coherent assignments, smaller sequential steps, or concurrent tasks. No fixed task count or model-specific split rule is imposed. The dashboard Auto setting also shows an estimate for a single worker; ordinary Chat/Work and load/pin use the same policy. Actual context is chosen again at launch, not from the preview.

On an idle model switch, Lyra unloads its unpinned context runners, confirms their removal through Ollama, and samples VRAM again before admission. An already loaded matching window can be reused. Active and pinned runners and external Ollama models are left alone. Idle previews may estimate reclaimed memory; the actual selection is made at launch. If Pi cannot find a window it reports the reason. Native text generation and load/pin can retain Ollama's default with a diagnostic. Multi-GPU estimates use the largest single-device budget. Set `OLLAMA_NUM_PARALLEL` in Lyra's environment to match Ollama's server configuration. An independently started Ollama daemon can have different settings.

## Benchmark profiles

The Dashboard has a single **full context benchmark**. Each point records catalog model size, Ollama's reported runner RAM/VRAM, device-wide NVIDIA VRAM and system-wide RAM baseline, sampled peak and final usage, actual reported context, load time and prefill/generation throughput. System RAM is affected by other processes and OS file caching, so it is not a per-model memory measurement. A cold, isolated run permits a useful peak-minus-baseline estimate; warm-run deltas are not model allocation measurements. Ollama's reported GPU percentage is not independently verified and may be wrong, including for hybrid runners. Old benchmark rows without physical sampling remain readable.

Run the **full context profile** for each model before relying on measured Auto. Lyra unloads its idle unpinned context runners for the benchmark. Unpin resident models, unload unrelated external Ollama runners, and avoid GPU-heavy applications during measurement. The benchmark tests 4K, 8K, 16K and doubling windows up to the model ceiling or the first point that fails verification or exceeds the Auto budget. It refines the boundary with real intermediate windows at roughly 1–2K resolution. The dashboard table shows requested and actual context, the VRAM measurement used by Auto, input/prefill and output/generation throughput, cold launch time, and verification status. The average launch time is computed from completed cold points of the latest profile, not earlier runs. Stop finishes the active measurement and its runner cleanup, skips subsequent contexts, and saves completed points; a stop before the first point preserves the previous profile. A full benchmark then runs the Pi compatibility check automatically, using the freshly saved profile for Auto. Stop skips the Pi stage if it has not begun; an active Pi check finishes and its unpinned runner is released. A failed or unavailable Pi check does not discard the context measurements. Each point uses a cold runner and the same short prompt with a 128-token output budget. GPU samples are taken approximately every 200 ms plus a final sample. A longer workload may require more memory than the measured peak.

Auto uses only successful cold points with matching requested/reported context, credible physical memory evidence and Ollama's near-full-GPU residency signal. It skips verified windows that generate at least 30% slower than the fastest smaller verified window when both runs generated at least 64 tokens. The full benchmark refines this boundary, and existing profiles are evaluated under the same rule. A slowdown is a performance signal and may have causes other than CPU or RAM offload. Profiles are keyed to the installed model digest, Ollama version, GPU UUID/size/driver and visible Ollama environment settings. Full physical profiling currently requires a single measurable NVIDIA GPU. If Ollama is started separately, keep the two environments consistent and rerun the full benchmark after changing KV cache, flash attention, parallelism or hardware settings.

## Local generation and task completion

Lyra no longer exposes `max_output_tokens` in its normal MCP calls, queued tasks or AI settings. Direct Ollama generation uses `num_predict = -1`; the Pi extension overrides the Ollama-compatible request's finite generation cap. Old saved task prompts with a cap remain readable, but the cap is ignored. The fixed benchmark budget remains separate so benchmark measurements stay comparable. Model context windows and natural provider limits still apply. Normal Pi runs no longer have a fixed 15-minute overall timeout; cancellation, startup timeout and separate benchmark/command timeouts remain. A client MCP wait may expire while a queued worker keeps running.

The Pi extension records the original and overridden token-limit fields at the provider hook (not a captured downstream HTTP request), the last generation input/output token counts, requested and observed Ollama context when available, stop reason and elapsed agent time. Work stores these in `work_progress.diagnostics`; direct calls also emit diagnostic events. A `length` stop with input + output reaching the configured window is classified as context exhaustion; otherwise the original provider output-limit reason remains, without pretending that its cause is known.

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

Lyra shows live progress and command approvals in the owning project chat. The chat header opens the global Pi command permission settings (Ask or Allow all); standalone MCP commands without a saved chat appear separately in the Projects panel. The existing single command slot and approval timeout are unchanged. Waiting for a task does not hold a workspace or model slot; the queue runs independently. Closing the MCP connection does not cancel a submitted task. If the combined submission loses its response before you know the IDs, repeat the exact same batch ID and payload or recover the saved IDs with a one-off inspection. Do not create a new batch just because a wait timed out.

Project folders are optional in the dashboard and when creating a project with MCP `save_project`. The first chat or task gets a project-specific internal workspace under Lyra's data directory, where Work can use `mkdir` with a relative path. To work in a specific location such as Desktop, register that location in project settings. Ordinary chats use the current registered folders as soon as they are added.

Keep important design/architecture acceptance checkpoints separate. Submit a design batch, inspect its result, approve or revise the contract, and only then submit implementation. Automatic waiting does not delegate those decisions to local workers.
