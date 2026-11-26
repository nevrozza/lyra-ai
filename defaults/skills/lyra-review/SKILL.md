---
name: lyra-review
description: Review the complete assembled diff in a Lyra review round, run the relevant build, and return a batch of small fixes.
---

# Review the assembled change

Inspect the full task diff every round, including in-scope new files. Read it in small chunks. Run the project's standard build when applicable. Consult the memory estimates and use `inspect_review_resources` to check current headroom. When there is room to overlap, start the build with `start_background_command`, inspect the diff with `read_review_diff` and the file tools, then collect the exit code and logs with `wait_background_command` before giving a verdict. Otherwise use `run_command`.

Use `post-code-review` for quality criteria. Return every confirmed blocker as a small, separately checkable fix using the requested verdict format. Choose a suitable installed model for each fix. Leave file edits to the fix tasks.
