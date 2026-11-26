---
name: lyra-fix
description: Resolve a single finding from Lyra's local review.
---

# Fix one finding

Verify the finding in the current files. Read the affected section, its immediate callers, and any relevant test. Make the smallest correction. If the report is stale or the fix needs a larger scope, explain what you found.

Run the focused check if it can run on its own. Report the result and changed paths. The reviewer will recheck the assembled diff and build.
