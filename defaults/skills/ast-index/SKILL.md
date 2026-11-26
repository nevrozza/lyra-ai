---
name: ast-index
description: Navigate project source with ast-index. Use for definitions, symbols, references, callers, implementations, and outlines when working in a local codebase.
---

# Search source with ast-index

Use `run_command` for CLI calls in Lyra Work. Run the commands from the relevant working folder. For another folder selected in the chat, change to its absolute path in the same command. Use `Set-Location -LiteralPath '…' -ErrorAction Stop; ast-index …` in PowerShell or `cd '…' && ast-index …` in sh.

1. Check the index with `ast-index stats`. If ast-index reports that the index is missing, run `ast-index rebuild` in that folder. Use `ast-index update` when the index is stale after source changes.
2. Choose the query by what you need:
   - `ast-index symbol "Name" --limit 10` for a definition by name.
   - `ast-index search "Name" --limit 10` for related symbols or paths when the name is uncertain.
   - `ast-index usages "Name"`, `ast-index callers "Name"`, or `ast-index implementations "Name"` for relationships.
   - `ast-index outline "path/to/file"` for a file overview.
3. Open the relevant source at the returned path and line before editing or drawing conclusions. For multiple working folders, search the relevant indexes separately. A result in one folder covers only its index.

Use the available text search tools for literal text, regexes, and files outside the index. If the executable is missing, the command is denied, or the index misses relevant code, use the available search tools and mention the limitation when it affects the result.

[ast-index command reference](https://github.com/defendend/Claude-ast-index-search/blob/main/USER_GUIDE.md)
