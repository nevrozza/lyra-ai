import { Type } from "@sinclair/typebox";

export default function (pi) {
    const textOnly = process.env.LYRA_PI_TEXT_ONLY === "1";
    const review = process.env.LYRA_PI_REVIEW === "1" && !textOnly;
    const guardedWrite = process.env.LYRA_PI_GUARDED_WRITE === "1" && !textOnly && !review;
    const origin = process.env.LYRA_MESSAGE_ORIGIN;
    const effort = process.env.LYRA_THINKING_EFFORT;
    if (effort) {
        pi.on("before_provider_request", ({ payload }) => {
            // Ollama levels can differ from Pi's built-in thinking presets.
            const request = { ...payload, reasoning_effort: effort };
            if (request.reasoning) request.reasoning = { ...request.reasoning, effort };
            return request;
        });
    }
    let earlierMessages = [];
    let promptTimestamp;
    const guardedDescriptions = new Map();
    if (guardedWrite) {
        const path = Type.String({ description: "Exact file path relative to the primary working folder, or absolute path in a registered folder" });
        for (const [name, description, parameters] of [
            ["write", "Create a new file or replace an existing file after reading its current contents. Lyra locks the file automatically.", Type.Object({ path, content: Type.String() })],
            ["edit", "Replace exact, unique, non-overlapping text in an existing file after reading it. Lyra locks the file automatically.", Type.Object({
                path,
                edits: Type.Array(Type.Object({ oldText: Type.String(), newText: Type.String() }), { minItems: 1 }),
            })],
        ]) {
            guardedDescriptions.set(name, description);
            pi.registerTool({
                name, label: name, description, parameters, executionMode: "sequential",
                promptSnippet: name === "write" ? "Create a file or replace an existing file after reading it." : "Edit a file after reading it, using exact oldText/newText pairs.",
                promptGuidelines: ["Read existing files before changing them. FILE_CHANGED/READ_REQUIRED: read the current contents and adapt. FILE_LOCKED: do independent work or report the conflict. Never bypass with run_command."],
                async execute(_id, args, signal, _onUpdate, ctx) {
                    const request = JSON.stringify({ ...args, tool: name });
                    const value = await ctx.ui.input("lyra:write-locked-file", request, { signal });
                    if (value === undefined) throw new Error("Lyra did not return a file result");
                    const result = JSON.parse(value);
                    if (!result.ok) throw new Error(result.message);
                    return { content: [{ type: "text", text: `Updated ${args.path}` }], details: result };
                },
            });
        }
    }
    if (guardedWrite) {
        // Retain Pi's native read tool, including images, line offsets, skills
        // and external reference files. Observe the workspace version before
        // execution and authorize writes only after a successful stable read.
        pi.on("tool_call", async (event, ctx) => {
            if (event.toolName !== "read") return;
            const path = event.input?.path;
            if (typeof path !== "string") return;
            const response = await ctx.ui.input("lyra:observe-read", JSON.stringify({
                phase: "begin", callId: event.toolCallId, path: path.replace(/^@/, ""),
            }));
            if (response !== "ok") return { block: true, reason: "Lyra could not observe the file version." };
        });
        pi.on("tool_result", async (event, ctx) => {
            if (event.toolName !== "read") return;
            const response = await ctx.ui.input("lyra:observe-read", JSON.stringify({
                phase: "finish", callId: event.toolCallId, success: !event.isError,
            }));
            if (response !== "ok") throw new Error("Lyra could not confirm the read version.");
        });
    }
    if (!textOnly) {
        const shell = process.platform === "win32" ? "Windows PowerShell" : "/bin/sh";
        pi.registerTool({
            name: "run_command",
            label: "Run project command",
            description: `NEVER use shell commands to bypass Lyra file locks or version checks. NEVER create, edit, delete, rename, move, patch, overwrite or restore project source files through shell, scripts, subprocesses, redirects, generators, or Git commands. Use write/edit for all source changes; review must not edit sources. Use commands only for inspection, builds and tests that do not modify source files. Request a non-interactive ${shell} command in the project root. Lyra may wait for user approval. Returns bounded stdout, stderr, and the exit code. Do not retry denied or stopped commands without asking the user.`,
            promptSnippet: "Run inspection, tests or builds through Lyra. Never bypass file locks or write project source files.",
            promptGuidelines: ["No shell source mutations, including indirect scripts or git operations. Never bypass a file lock or stale-version error."],
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
    const reviewTools = [];
    if (review) {
        for (const [name, title, description, parameters, argument] of [
            ["inspect_review_resources", "lyra:review-resources",
                "Get a current CPU, system RAM, GPU VRAM, and Ollama residency sample. Use it to decide whether to overlap a build with diff review.",
                Type.Object({}), () => ""],
            ["start_background_command", "lyra:start-background-command",
                "Submit one background build or check through Lyra's standard command approval. Continue inspecting files or the Git diff while it runs.",
                Type.Object({ command: Type.String({ minLength: 1, maxLength: 4000 }) }), args => args.command],
            ["wait_background_command", "lyra:wait-background-command",
                "Collect the background command's exit code and bounded logs. Collect the result before giving a review verdict.",
                Type.Object({}), () => ""],
            ["read_review_diff", "lyra:read-review-diff",
                "Read the Git diff during a background build. Start with changed and untracked views, then use diff with each tracked path. Continue with the returned next_offset to read more. Read new untracked files with the file reader.",
                Type.Object({
                    view: Type.Union([Type.Literal("changed"), Type.Literal("untracked"), Type.Literal("diff")]),
                    path: Type.Optional(Type.String()),
                    offset: Type.Optional(Type.Integer({ minimum: 0 })),
                }), args => JSON.stringify(args)],
        ]) {
            reviewTools.push(name);
            pi.registerTool({
                name, label: name, description, parameters, executionMode: "sequential",
                async execute(_id, args, signal, _onUpdate, ctx) {
                    const value = await ctx.ui.input(title, argument(args), { signal });
                    if (value === undefined) throw new Error("Lyra did not return a result");
                    return { content: [{ type: "text", text: value }], details: JSON.parse(value) };
                },
            });
        }
    }
    const tools = ["read", "grep", "find", "ls", "run_command"];
    const expected = textOnly ? [] : review ? [...tools, ...reviewTools] : [...tools, "write", "edit"];
    async function verifyTools() {
        await pi.setActiveTools(expected);
        const active = pi.getActiveTools();
        const missing = expected.filter(name => !active.includes(name));
        const unexpected = active.filter(name => !expected.includes(name));
        const all = pi.getAllTools();
        const unguarded = [...guardedDescriptions].filter(([name, description]) =>
            !all.some(tool => tool.name === name && tool.description === description)).map(([name]) => name);
        if (missing.length || unexpected.length || unguarded.length) {
            throw new Error("Pi tool configuration mismatch: missing=" + missing.join(",") +
                "; unexpected=" + unexpected.join(",") + "; unverified guarded=" + unguarded.join(","));
        }
        return active;
    }
    pi.on("session_start", async (_event, ctx) => {
        try {
            const active = await verifyTools();
            if (origin) earlierMessages = savedMessages(ctx.sessionManager);
            ctx.ui.notify("lyra:active-tools:" + active.join(","));
            ctx.ui.notify(ctx.mode === "rpc" ? "lyra:commands-ready" : "lyra:commands-ready-legacy");
        } catch (error) {
            ctx.ui.notify("lyra:tools-error:" + String(error));
            ctx.shutdown();
        }
    });
    if (origin) {
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
        let reportedNearLimit = false;
        pi.on("session_before_compact", (_event, ctx) => ctx.ui.notify("lyra:context-compacting"));
        pi.on("session_compact", (_event, ctx) => ctx.ui.notify("lyra:context-compacted"));
        pi.on("session_compact_failed", ({ aborted }, ctx) => {
            if (!aborted) ctx.ui.notify("lyra:context-near-limit");
        });
        pi.on("context", ({ messages }, ctx) => {
            const usage = ctx.getContextUsage?.();
            if (!reportedNearLimit && usage?.percent >= 90) {
                reportedNearLimit = true;
                ctx.ui.notify("lyra:context-near-limit");
            }
            const promptIndex = messages.findLastIndex(message => message.role === "user" &&
                message.timestamp === promptTimestamp);
            const historyPresent = messages.some(message => message.role === "compactionSummary" ||
                earlierMessages.some(earlier => earlier.role === message.role &&
                    earlier.timestamp === message.timestamp));
            if (promptIndex < 0) return { messages };
            if (!historyPresent) {
                messages = [...messages.slice(0, promptIndex), ...earlierMessages, ...messages.slice(promptIndex)];
            }
            return { messages: conversationContext(messages, promptTimestamp, textOnly) };
        });
    }
    pi.on("agent_end", (_event, ctx) => ctx.shutdown());
}

function conversationContext(messages, promptTimestamp, textOnly) {
    const conversation = textOnly ? messages.flatMap(message => {
        if (["system", "compactionSummary", "branchSummary"].includes(message.role)) return [message];
        const text = textMessage(message);
        return text ? [text] : [];
    }) : messages;
    const promptIndex = conversation.findLastIndex(message => message.role === "user" &&
        message.timestamp === promptTimestamp);
    const hasHistory = conversation.slice(0, Math.max(promptIndex, 0)).some(message =>
        ["user", "assistant", "compactionSummary", "branchSummary"].includes(message.role));
    if (!hasHistory) return conversation;

    const prompt = conversation[promptIndex];
    // Keep every preceding turn in its original role, including native compaction summaries.
    const continuation = {
        type: "text",
        text: "This is the next message in the same Lyra chat. Use the preceding messages and any saved summary when answering, including when asked to recall earlier turns.\n\nLatest user message\n",
    };
    const content = typeof prompt.content === "string"
        ? [{ type: "text", text: prompt.content }] : prompt.content;
    return [...conversation.slice(0, promptIndex), { ...prompt, content: [continuation, ...content] },
        ...conversation.slice(promptIndex + 1)];
}

function textMessage(message) {
    if (message.role !== "user" && message.role !== "assistant") return;
    const text = typeof message.content === "string" ? message.content
        : message.content?.filter(block => block.type === "text").map(block => block.text).join("\n");
    if (!text?.trim()) return;
    const content = [{ type: "text", text }];
    return message.role === "assistant" ? { ...message, content, stopReason: "stop" } : { ...message, content };
}

function savedMessages(sessionManager) {
    const branch = sessionManager?.getBranch?.() ?? [];
    // Honor a saved compaction or explicit context edit instead of restoring raw entries.
    if (branch.some(entry => entry.type === "compaction" || entry.type === "context_edit")) return [];
    return branch.flatMap(entry => entry.type === "message" ? [entry.message] : []);
}
