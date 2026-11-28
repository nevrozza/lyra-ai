import { Type } from "@sinclair/typebox";

export default function (pi) {
    const textOnly = process.env.LYRA_PI_TEXT_ONLY === "1";
    const review = process.env.LYRA_PI_REVIEW === "1" && !textOnly;
    const guardedWrite = process.env.LYRA_PI_GUARDED_WRITE === "1" && !textOnly && !review;
    const origin = process.env.LYRA_MESSAGE_ORIGIN;
    const effort = process.env.LYRA_THINKING_EFFORT;
    const mcpEnabled = process.env.LYRA_PI_MCP === "1" && !textOnly && !review;
    const mcpResources = new Set(["list_mcp_resources", "list_mcp_resource_templates", "read_mcp_resource"]);
    const isMcpTool = (name) => mcpEnabled && (name.startsWith("mcp__") || mcpResources.has(name));
    let reportedMcpTools = "";
    pi.on("before_provider_request", ({ payload }, ctx) => {
        if (mcpEnabled) {
            const active = pi.getActiveTools().filter(isMcpTool).sort().join(", ");
            if (active && active !== reportedMcpTools) ctx.ui.notify("lyra:mcp-tools:" + active);
            reportedMcpTools = active;
        }
        // Lyra only registers the Ollama OpenAI-compatible provider. Override
        // Pi's provider-generated max token fields even for default thinking.
        // Ollama maps max_tokens=-1 to unlimited num_predict (no finite cap).
        const request = { ...payload, max_tokens: -1 };
        delete request.max_completion_tokens;
        if (effort) {
            request.reasoning_effort = effort;
            if (request.reasoning) request.reasoning = { ...request.reasoning, effort };
        }
        // Never log prompt content or credentials. This is the payload returned
        // by the last Lyra hook, not a capture of the downstream HTTP transport.
        ctx?.ui?.notify("lyra:provider-request:" + JSON.stringify({
            original_max_tokens: payload?.max_tokens ?? null,
            original_max_completion_tokens: payload?.max_completion_tokens ?? null,
            max_tokens: request.max_tokens,
            max_completion_tokens: request.max_completion_tokens ?? null,
            context_window: ctx?.model?.contextWindow ?? null,
        }));
        return request;
    });
    let earlierMessages = [];
    let promptTimestamp;
    const guardedDescriptions = new Map();
    if (guardedWrite) {
        const path = Type.String({ description: "Exact file path, relative to the primary working folder or absolute" });
        const definitions = [
            ["read", "Read the current file and record its version. Optional 1-based offset and line limit.", Type.Object({
                path, offset: Type.Optional(Type.Integer({ minimum: 1 })), limit: Type.Optional(Type.Integer({ minimum: 1 })),
            }), "Read current file contents before editing. Use offset and limit for large files."],
            ["write", "Create a new file (missing parent directories are created automatically) or replace an existing file after a complete current read. Acquires a dynamic file lock.", Type.Object({ path, content: Type.String() }), "Create a new file or replace a fully read file."],
            ["copy_skill_resource", "Copy a resource from an available Lyra skill package to a new file in the project. Preserves binary bytes, including fonts and images. Never overwrites existing files; source is relative to the skill package root.", Type.Object({
                skill: Type.String({ description: "Exact name of an available skill" }),
                source: Type.String({ description: "Relative resource path within the package, e.g. assets/fonts/Example.ttf" }),
                path,
            }), "Copy a skill asset to a new project file without reading its binary contents."],
            ["mkdir", "Create a directory and missing parent directories within the registered workspace. Existing directories are accepted.", Type.Object({ path }), "Create an empty or nested directory; write already creates parents for files."],
            ["edit", "Replace exact, unique, non-overlapping text after reading the current file. Acquires a dynamic file lock.", Type.Object({
                path,
                edits: Type.Array(Type.Object({ oldText: Type.String(), newText: Type.String() }), { minItems: 1 }),
            }), "Apply exact oldText/newText replacements to a file you have read."],
        ];
        for (const [name, description, parameters, snippet] of definitions) {
            guardedDescriptions.set(name, description);
            pi.registerTool({
                name, label: name, description, parameters, executionMode: "sequential",
                promptSnippet: snippet,
                promptGuidelines: [
                    "Use the exact path and advertised argument schema. Existing files require a current read; new files can be created directly.",
                    "FILE_CHANGED: reread and reconcile. FILE_LOCKED: work elsewhere or report the dependency. Never bypass via run_command."
                ],
                async execute(_id, args, signal, _onUpdate, ctx) {
                    const request = JSON.stringify({ ...args, tool: name });
                    const value = await ctx.ui.input("lyra:guarded-file", request, { signal });
                    if (value === undefined) throw new Error("Lyra did not return a file result");
                    const result = JSON.parse(value);
                    if (!result.ok) throw new Error(result.message);
                    if (name === "read") {
                        const content = result.image
                            ? [{ type: "image", data: result.image, mimeType: result.mime }]
                            : [{ type: "text", text: result.text }];
                        return { content, details: {} };
                    }
                    return { content: [{ type: "text", text: name === "mkdir" ? `Directory ready: ${args.path}` : `Updated ${args.path}` }], details: result };
                },
            });
        }
    }
    if (!textOnly) {
        const completionDescription = "Record the actual outcome of the assignment: completed or blocked. Call before the final answer.";
        guardedDescriptions.set("finish_task", completionDescription);
        pi.registerTool({
            name: "finish_task", label: "Report task outcome", description: completionDescription,
            parameters: Type.Object({
                status: Type.Union([Type.Literal("completed"), Type.Literal("blocked")]),
                summary: Type.String({ minLength: 1, maxLength: 2000, description: "Actual changes, checks and unresolved requirements." }),
            }),
            promptSnippet: "Record your task outcome before the final answer.",
            executionMode: "sequential",
            async execute(_id, args, signal, _onUpdate, ctx) {
                const value = await ctx.ui.input("lyra:finish-task", JSON.stringify(args), { signal });
                if (value === undefined) throw new Error("Lyra did not return a task outcome result");
                const result = JSON.parse(value);
                if (!result.ok) throw new Error(result.message);
                return { content: [{ type: "text", text: "Outcome recorded." }], details: result };
            },
        });
        const description = "Load the instructions of an available Lyra skill by exact name, without guessing its file path. Returns SKILL.md text and the real package directory for referenced resources.";
        guardedDescriptions.set("load_skill", description);
        pi.registerTool({
            name: "load_skill", label: "Load skill", description,
            parameters: Type.Object({
                name: Type.String({ description: "Exact skill name from the available Lyra skills catalog, e.g. canvas-design" }),
                offset: Type.Optional(Type.Integer({ minimum: 1 })),
                limit: Type.Optional(Type.Integer({ minimum: 1 })),
            }),
            promptSnippet: "Load a relevant skill by name from the available catalog.",
            promptGuidelines: ["When a requested or relevant skill is listed, call load_skill with its name, not a guessed path. Follow any read continuation marker; resolve referenced files relative to the returned package directory."],
            executionMode: "sequential",
            async execute(_id, args, signal, _onUpdate, ctx) {
                const value = await ctx.ui.input("lyra:load-skill", JSON.stringify(args), { signal });
                if (value === undefined) throw new Error("Lyra did not return a skill result");
                const result = JSON.parse(value);
                if (!result.ok) throw new Error(result.message);
                return {
                    content: [{ type: "text", text: `Skill: ${result.name}\nManifest: ${result.path}\nPackage directory: ${result.directory}\n\n${result.text}` }],
                    details: { name: result.name, path: result.path, directory: result.directory },
                };
            },
        });
        const shell = process.platform === "win32" ? "Windows PowerShell" : "/bin/sh";
        pi.registerTool({
            name: "run_command",
            label: "Run project command",
            description: `Request a non-interactive ${shell} command in the project root for inspection, tests or builds. NEVER modify shared source/configuration files through run_command, including via shell redirection, scripts, generators, formatters, git or subprocesses. Never bypass read/write/edit version checks, file locks or read-only review restrictions, even after a tool error. Avoid conflicting build outputs. If a required mutation is blocked, report it. Lyra may wait for user approval. Returns bounded stdout, stderr, and the exit code. Do not retry denied or stopped commands without asking the user.`,
            promptSnippet: "Run a project check or build through Lyra. Never use shell commands to bypass guarded file changes.",
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
                "Submit one background build or check through Lyra's standard command approval. NEVER modify source/configuration files or bypass the read-only review or guarded file rules through this command. Continue inspecting files or the Git diff while it runs.",
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
    const tools = ["read", "grep", "find", "ls", "run_command", "load_skill", "finish_task"];
    const expected = textOnly ? [] : review ? [...tools, ...reviewTools] : [...tools, "write", "edit", ...(guardedWrite ? ["mkdir", "copy_skill_resource"] : [])];
    async function verifyTools() {
        await pi.setActiveTools([...expected, ...pi.getActiveTools().filter(isMcpTool)]);
        const active = pi.getActiveTools();
        const missing = expected.filter(name => !active.includes(name));
        const unexpected = active.filter(name => !expected.includes(name) && !isMcpTool(name));
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
    pi.on("agent_end", (_event, ctx) => {
        // Finish the original agent run. No completion-check message or extra generation.
        ctx.shutdown();
    });
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
