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
