import { Type } from "@sinclair/typebox";

export default function (pi) {
    const shell = process.platform === "win32" ? "Windows PowerShell" : "/bin/sh";
    pi.registerTool({
        name: "run_command",
        label: "Run project command",
        description: `Request a non-interactive ${shell} command in the project root. Lyra may wait for user approval. Returns bounded stdout, stderr, and the exit code. Do not retry denied or stopped commands without asking the user.`,
        promptSnippet: "Run a project command such as a test or build with approval in Lyra.",
        parameters: Type.Object({ command: Type.String({ minLength: 1, maxLength: 4000 }) }),
        executionMode: "sequential",
        async execute(_id, { command }, signal, _onUpdate, ctx) {
            const result = await ctx.ui.input("lyra:run-command", command, { signal });
            if (result === undefined) throw new Error("Lyra did not return a command result");
            const details = JSON.parse(result);
            if (details.status !== "exited" || details.exit_code !== 0) throw new Error(result);
            return { content: [{ type: "text", text: result }], details };
        },
    });
    pi.on("session_start", (_event, ctx) => {
        pi.setActiveTools(["read", "grep", "find", "ls", "write", "edit", "run_command"]);
        ctx.ui.notify(ctx.mode === "rpc" ? "lyra:commands-ready" : "lyra:commands-ready-legacy");
    });
    pi.on("agent_end", (_event, ctx) => ctx.shutdown());
}
