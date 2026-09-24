---
"luchta": minor
---

# Per-worktree dev sessions

Add `luchta session -- <command>`, which gives each concurrent worktree its own
set of ports from a new `sessions` config block and refuses to start a second
session in the same worktree, and `luchta sessions` to list live sessions and
their URLs.
