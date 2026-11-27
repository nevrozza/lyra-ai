---
name: lyra-review
description: Review the complete assembled diff in a Lyra review round, run the relevant build, and return a batch of small fixes.
---

# Review the assembled change

Inspect the full task diff every round, including in-scope new files. Read it in small chunks. Run the project's standard build when applicable. Consult the memory estimates and use `inspect_review_resources` to check current headroom. When there is room to overlap, start the build with `start_background_command`, inspect the diff with `read_review_diff` and the file tools, then collect the exit code and logs with `wait_background_command` before giving a verdict. Otherwise use `run_command`.

For each material acceptance criterion, record the verification method, concrete observation, and outcome: confirmed, violated, or unverified. Source inspection can establish structure, rules, and directly provable defects; it does not establish rendered appearance or observed runtime behavior. Use an actual render or execution for those claims. If the required environment or check is unavailable, mark the affected criteria unverified and explain the limitation; do not treat them as passed. Only send evidenced defects to fix workers (an observed effect or a reproducible or directly demonstrable violation). Do not invent a defect just because a check could not run. Include the compact verification evidence and limitations in the existing verdict summary or reason, without adding fields to the required JSON schema.

Use `post-code-review` for quality criteria. Return every confirmed blocker as a small, separately checkable fix using the requested verdict format. Choose an installed model by intelligence: Low only for fully specified mechanical fixes such as typos; Medium or High for code, debugging and reasoning; High for complex or subtle fixes. Escalate after failure. Never downgrade just to save memory. Leave file edits to the fix tasks.

## Prepare the fix workers' technical assignments

In an automatic review round, Lyra schedules the models selected in your JSON findings. You are planning their work, not just reporting bugs. Do not edit files yourself or try to launch a model through shell commands. For each confirmed issue, produce a small complete technical assignment:

- `location`: verified relative file path and symbol or line; identify immediate callers or related files in the instructions when relevant.
- `issue`: observed incorrect behavior, supporting evidence/reproduction, and why it violates the original requirements.
- `instructions`: actionable correction steps, expected interfaces or invariants, relevant edge cases, and behavior to preserve. Distinguish a confirmed cause from a hypothesis. Reference the exact inspected files or prerequisite findings; do not invent symbols, commands, or facts. For Low, specify the exact mechanical before/after and no open decisions; for Medium, give a concrete implementation sequence and contract; for High, state the requirements and allow judgment about the implementation.
- `check`: a specific acceptance/regression check, including the expected result and a known existing test/build command when useful. Do not claim an unrun check passed.
- `model` and `after`: choose the eligible exact model tag and list earlier finding positions for shared-file or information dependencies. Order dependent fixes before their consumers.

Use a short sequence of actions in instructions for a nontrivial fix: inspect the verified location/caller, make the necessary correction, preserve the affected contract and add or run the focused regression check. A mechanical fix may need only one exact operation. Specify the correction where the evidence supports it, and leave incidental implementation choices to the worker. Do not inflate a finding with unrelated improvements, repeated constraints, or full code listings. Do not turn a finding into a vague request like "fix auth" or copy a huge diff into the report. The fix worker receives your instructions and shared group handoff; give it what it needs to start and verify the work. Use the verdict schema and size limits supplied in the review prompt; return every confirmed blocker, compactly, and preserve valid JSON. If required evidence is missing, state the blocker rather than guessing. For a standalone review without an automatic verdict schema, return the same actionable information in readable prose for the orchestrator.

A completed review may legitimately return `fixes`: completing the review does not mean the implementation passed. If essential verification is unavailable and prevents a complete review, use `blocked` rather than a passing verdict. Preserve the required verdict format.
