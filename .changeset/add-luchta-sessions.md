---
"luchta": minor
---

# Per-worktree dev sessions

Add `luchta session -- <command>`, which gives each concurrent worktree its own
set of ports from a new `sessions` config block and refuses to start a second
session in the same worktree, and `luchta sessions` to list live sessions and
their URLs. `sessions.env` declares extra env vars whose values are templates
(`"${DEVSERVER_HTTP_PORT}"`, `"${LUCHTA_SESSION_NAME}"`, …) filled in from the
allocated ports and session identity, for apps that pass ports around inside
URLs.
