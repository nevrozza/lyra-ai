---
name: lyra-fix
description: Resolve a single finding from Lyra's local review.
---

# Fix one finding

Verify the finding in the current files. Read the affected section, its immediate callers, and any relevant test. Make the smallest correction. If the report is stale or the fix needs a larger scope, explain what you found.

Run the focused check if it can run on its own. Report the result and changed paths. The reviewer will recheck the assembled diff and build.

Treat the review finding's technical instructions as the assigned correction plan. Verify the evidence against current files and any relevant predecessor handoff before acting. Follow its intended steps, constraints, edge cases and acceptance check; do not blindly apply a stale recommendation. If the proposed approach would violate the actual contract or needs unrelated work, report the discrepancy and blocker rather than inventing a new broad task. Use the required JSON verdict if supplied; include the actual focused check outcome in its summary or reason.
