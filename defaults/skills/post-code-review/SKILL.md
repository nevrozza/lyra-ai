---
name: post-code-review
description: Review completed code changes in any language. Use after implementation or when asked to verify a change for correctness, readable design, and focused tests.
---

# Review the finished change

Read the task, the diff, and the nearby code that calls or depends on it. Check the change through its normal entry point.

- **Correctness.** Verify the expected behavior and relevant edge cases. Check failures and retries when they matter. Preserve behavior that the task did not ask to change.
- **Readability.** Make names describe their purpose. Prefer clear control flow and a straightforward solution. Remove dead code and unnecessary layers.
- **Boundaries.** Keep each module's responsibilities clear. Hide internal state behind a useful interface. Look for dependency cycles and callers that have to coordinate details the module could handle itself.
- **Reuse.** Use existing project logic when it fits. Combine duplicated decisions that must stay in sync. Add an abstraction when it makes callers or invariants simpler.
- **Verification.** Run the smallest relevant checks the project already uses. In Lyra Work, use `run_command`. Follow project conventions and any applicable language-specific skill.

Fix clear issues within the task scope, then rerun the affected checks. Describe broader issues only if they affect the change. Report the outcome, any remaining findings with file and line references, and the checks you ran. When reporting to an orchestrator, keep source excerpts and raw command output local.
