import { Type } from "@sinclair/typebox";

export default function (pi) {
    const textOnly = process.env.LYRA_PI_TEXT_ONLY === "1";
    const origin = process.env.LYRA_MESSAGE_ORIGIN;
    let earlierMessages = [];
    let promptTimestamp;
    if (!textOnly) {
        const shell = process.platform === "win32" ? "Windows PowerShell" : "/bin/sh";
        pi.registerTool({
            name: "run_command",
            label: "Run project command",
            description: `Request a non-interactive ${shell} command in the project root. Lyra may wait for user approval. Returns bounded stdout, stderr, and the exit code. Do not retry denied or stopped commands without asking the user.`,
            promptSnippet: "Run a project command such as a test or build through Lyra.",
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
    }
    pi.on("session_start", (_event, ctx) => {
        pi.setActiveTools(textOnly ? [] : ["read", "grep", "find", "ls", "write", "edit", "run_command"]);
        if (origin) earlierMessages = recentMessages(ctx.sessionManager);
        ctx.ui.notify(ctx.mode === "rpc" ? "lyra:commands-ready" : "lyra:commands-ready-legacy");
    });
    if (origin === "dashboard" || origin === "orchestrator") {
        pi.on("message_end", ({ message }) => {
            if (message.role === "user") {
                promptTimestamp = message.timestamp;
                pi.appendEntry("lyra-message-origin", {
                    origin,
                    messageTimestamp: message.timestamp,
                    requestId: process.env.LYRA_MESSAGE_REQUEST_ID,
                    model: process.env.LYRA_CHAT_MODEL,
                });
            }
        });
        pi.on("context", ({ messages }) => {
            const currentPromptPresent = messages.some(message => message.role === "user" &&
                message.timestamp === promptTimestamp);
            const historyPresent = messages.some(message => message.role === "compactionSummary" ||
                earlierMessages.some(earlier => earlier.role === message.role &&
                    earlier.timestamp === message.timestamp));
            if (currentPromptPresent && !historyPresent) messages = [...earlierMessages, ...messages];
            if (textOnly) {
                messages = messages.flatMap(message => {
                    if (["system", "compactionSummary", "branchSummary"].includes(message.role)) return [message];
                    const text = textMessage(message);
                    return text ? [text] : [];
                });
            }
            return { messages };
        });
    }
    pi.on("agent_end", (_event, ctx) => ctx.shutdown());
}

function textMessage(message, limit = Infinity) {
    if (message.role !== "user" && message.role !== "assistant") return;
    const text = (typeof message.content === "string" ? message.content
        : message.content?.filter(block => block.type === "text").map(block => block.text).join("\n"))
        ?.slice(0, limit);
    if (!text?.trim()) return;
    const content = [{ type: "text", text }];
    return message.role === "assistant" ? { ...message, content, stopReason: "stop" } : { ...message, content };
}

function recentMessages(sessionManager) {
    const branch = sessionManager?.getBranch?.() ?? [];
    // Honor a saved compaction or explicit context edit instead of restoring raw entries.
    if (branch.some(entry => entry.type === "compaction" || entry.type === "context_edit")) return [];
    const messages = [];
    for (let index = branch.length - 1; index >= 0 && messages.length < 8; index--) {
        const entry = branch[index];
        if (entry.type !== "message") continue;
        const message = textMessage(entry.message, 512);
        if (message) messages.push(message);
    }
    return messages.reverse();
}
