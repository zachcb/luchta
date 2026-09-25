# Sessions, Port Routing, and Pause — Design

Date: 2026-09-24
Status: Approved design, pending spec review

## Problem

Developers run several git worktrees of the same Luchta-managed app (e.g.
formative) side by side, each in its own terminal. Every worktree's dev servers
bind the same default ports (8081, 8011, …), so a second `yarn start` collides
with a forgotten first one, and there is no way to see which worktree owns which
port or to open several worktrees' sites at once.

## Goals

1. Detect and refuse a second dev session in the same worktree, naming the
   running one.
2. Give each concurrent worktree session its own non-colliding set of app ports.
3. List live sessions and their URLs.
4. (Phase 2) Reach each session's services by name, e.g.
   `http://web.feature-x.localhost:1355`, including hot-reload websockets.
5. (Phase 3) Pause a session — every process it started, including ones that
   daemonize — and resume it instantly, on Linux and macOS, without depending
   on the wrapped command being overmind.

## Non-goals

- Isolating shared infrastructure (docker-compose mongo, redis, smtp4dev,
  sockudo, localstack). All worktrees share one set on standard ports. The
  known cost — cross-worktree crosstalk through shared redis queues and DB
  state — is accepted.
- Replacing overmind or adding a long-lived "service" task type. Luchta wraps
  whatever command starts the dev servers.
- Different Luchta repos concurrently (works incidentally, not designed for).
- HTTPS / HTTP/2 in the proxy; auto-starting the proxy; rewriting absolute URLs
  the app generates.
- Pause: freeing memory or ports (paused processes stay resident and keep their
  sockets bound); Windows support; the Linux cgroup freezer; preventing
  shared-redis job crosstalk while a worker is paused.

## Context

- Today formative runs `yarn start` → `overmind s` over a `Procfile`. Luchta
  only runs `luchta watch` and `luchta await` gates inside it; it never sees the
  dev-server processes or their ports.
- Formative's servers already read ports from specific env vars
  (`DEVSERVER_HTTP_PORT`, `AUTH_DEV_HTTP_PORT`, `REPORT_SERVER_HTTP_PORT`,
  `*_METRICS_PORT`, …), falling back to `PORT` / `METRICS_PORT`. Overmind sets
  `PORT` per process identically in every worktree.
- The workspace already depends on `fd-lock` (used by `build_lock.rs`),
  `dirs`, and `hyper`.

## Phasing

- **Phase 1 — Sessions:** config schema, registry, allocation, `luchta session`
  wrapper, `luchta sessions` listing. Useful on its own.
- **Phase 2 — Proxy:** `luchta proxy`, consuming the phase-1 registry.
- **Phase 3 — Pause/resume:** `luchta sessions pause|resume`, via a per-session
  control socket owned by the wrapper.

Each phase gets its own implementation plan.

## 1. Configuration

New optional top-level key in the `luchta-config` JSON:

```jsonc
{
  "sessions": {
    "slotStride": 1000,   // optional, default 1000
    "maxSlots": 20,       // optional, default 20
    "ports": {
      "DEVSERVER_HTTP_PORT":     { "default": 8081, "service": "web",  "http": true, "defaultService": true },
      "AUTH_DEV_HTTP_PORT":      { "default": 8011, "service": "auth", "http": true },
      "REPORT_SERVER_HTTP_PORT": { "default": 8090 }
    }
  }
}
```

- Keys are env var names; they are the whole interface to the app. The port for
  slot `N` is `default + N * slotStride`.
- `service` (optional) is a display/routing name and must be a DNS label.
  `http` (default `false`) marks the port as a browsable HTTP service: it gets
  a URL in listings and is routable by the proxy. `defaultService`
  (default `false`, at most one) picks the target for bare
  `<session>.localhost`; otherwise the first declared `http` port is used.
- Validation (`luchta-types`, `thiserror`):
  - `slotStride` must exceed `max(default) - min(default)` across declared
    ports, so slots cannot overlap.
  - `default + (maxSlots - 1) * slotStride` must be ≤ 65535 for every port.
  - `service` names unique and DNS-label-valid; at most one `defaultService`.
- Formative must declare **every** port its servers bind, metrics ports
  included; otherwise the `PORT`/`METRICS_PORT` fallbacks collide across
  worktrees.
- `env` (optional; added after phase-1 review, for apps that pass ports
  around inside URLs rather than reading a port variable directly): a map of
  extra env var name → template string, resolved after allocation. A
  `${NAME}` placeholder must be a declared `ports` key, `LUCHTA_SESSION_NAME`,
  or `LUCHTA_SESSION_SLOT`; any other `$` is literal. For example:
  ```jsonc
  "env": { "API_ROOT_URL": "http://localhost:${DEVSERVER_HTTP_PORT}" }
  ```

## 2. Session registry and liveness

**Location:** `LUCHTA_SESSIONS_DIR` if set, overriding the registry directory
(tests use it for isolation); otherwise
`dirs::runtime_dir()/luchta/sessions` (Linux `$XDG_RUNTIME_DIR`), falling back
to `dirs::cache_dir()/luchta/sessions` (macOS, Windows). The registry is
per-user and machine-wide, since ports are machine-wide.

**Files:**

| File | Purpose |
| --- | --- |
| `slot-<N>.lock` | Exclusive `fd-lock` held by the owning session for its lifetime. Never unlinked. |
| `slot-<N>.json` | Session record, written atomically (temp file + rename). |
| `alloc.lock` | Held briefly during allocation to serialize concurrent starts. |
| `last-slots.json` | Map of canonical workspace root → last slot used (sticky slots). |

**Record fields:** `slot`, `id` (random uuid, per session), `name`, `pid`,
`workspace_root` (canonicalized), `branch` (best effort, may be null),
`command` (argv), `started_at`, `ports: [{ env, port, service?, http }]`,
`paused_at` (phase 3; null when running).

**Liveness is the lock, nothing else.** A slot is live iff `try_lock` on
`slot-<N>.lock` fails. A record whose lock is acquirable is stale and ignored
or overwritten. The OS releases the lock on any process death, so no cleanup
path is required.

**Allocation** (under `alloc.lock`):

1. If a live record has the same `workspace_root`, fail with
   `SessionAlreadyRunning` naming pid, start time, slot, and URLs.
2. Try the slot in `last-slots.json` for this workspace root.
3. Otherwise try slots `0..maxSlots` in order.
4. A slot is taken when its lock is acquired **and** every declared port for
   that slot passes a probe bind on `127.0.0.1` (catching non-Luchta
   processes). If the probe fails, release the lock and try the next slot.
5. None free → `NoFreeSlot`, listing live sessions.
6. On success: write the record, update `last-slots.json`, release
   `alloc.lock`, keep the slot lock.

**Session name:** `--name` if given, else the workspace root's basename,
lowercased and sanitized to a DNS label (`[a-z0-9-]`, no leading/trailing
hyphen, ≤ 63 chars). If a live session already has the name, append `-2`,
`-3`, ….

## 3. `luchta session` wrapper

`luchta session [--name <n>] [--quiet] -- <cmd> [args...]`

1. Evaluate `luchta-config` as `run` does. Missing `sessions` block →
   diagnostic pointing at the key.
2. Allocate (section 2).
3. Print a banner to stderr unless `--quiet`: session name, slot, and one
   `service  http://localhost:<port>` line per `http` port; plus the named URL
   when a live proxy is detected (phase 2).
4. Spawn the child with inherited stdio and env plus:
   - each declared port var set to its slot value;
   - `LUCHTA_SESSION_NAME`, `LUCHTA_SESSION_SLOT`;
   - `LUCHTA_SESSION_ID` (the record's `id`; phase 1 sets it so phase 3 can
     find descendants).

   A declared var already present in the environment with a different value is
   overridden, with a one-line warning.
5. Signals: ignore SIGINT and SIGQUIT (the terminal delivers both to the whole
   foreground group, including the child); forward SIGTERM and SIGHUP to the
   child. Either signal sent to the wrapper alone (not its process group) is
   likewise absorbed, not forwarded. On Windows, only wait (console Ctrl-C
   reaches the group).
6. Wait for the child; exit with its exit code (or 128+signal). Dropping the
   lock frees the slot.

Port env vars pass through to nested `luchta watch`/`run` but do not affect
cache keys, because Luchta hashes only env vars a task declares.

`luchta sessions [--json]` lists live sessions (name, slot, branch, pid,
uptime, URLs, and `paused <duration>` in phase 3), marking the current
worktree's. `--json` prints the records.
Stale records are omitted.

## 4. `luchta proxy` (phase 2)

`luchta proxy [--port <p>]`, default port 1355, run explicitly (own terminal
or a systemd/launchd user unit). Port 80 is possible with `--port 80` where the
OS permits (macOS; Linux with `setcap` or
`net.ipv4.ip_unprivileged_port_start`) — documented, not automated.

**Liveness:** holds `proxy.lock` and writes `proxy.json` (`port`, `pid`) in the
registry directory, same model as sessions. The session banner and
`luchta sessions` print named URLs only when the proxy is live.

**Routing by `Host`** (port suffix ignored, case-insensitive):

- `<service>.<session>.localhost` → `127.0.0.1:<port>` of that session's
  `http` port with that `service`.
- `<session>.localhost` → the session's default service.
- `localhost` or any unmatched host → an HTML index of live sessions and
  links (unmatched hosts get status 404).
- Registered but refusing connections → 502 page naming session, service, and
  port.
- Session paused (phase 3) → 503 page "`<session>` is paused" with the resume
  command, instead of forwarding into a stopped process.

**Registry reads:** cached snapshot, refreshed on lookup miss and at most once
per second otherwise. No filesystem watcher.

**Protocol:** HTTP/1.1 via `hyper`. Requests with `Upgrade` are proxied by
completing the upgrade on both sides and splicing with
`tokio::io::copy_bidirectional` (webpack/rspack HMR websockets). `Host` is
preserved; `X-Forwarded-Host`, `X-Forwarded-Proto`, `X-Forwarded-For` are
added.

**Formative follow-ups (not Luchta work):** set dev-server
`allowedHosts: ['.localhost']`; optionally derive public/callback URLs from
`LUCHTA_SESSION_NAME` so absolute links stay on the named host. Until then
those links point at `localhost:<port>` and still work.

## 5. Pause and resume (phase 3)

`luchta sessions pause [<name>]` / `luchta sessions resume [<name>]`; no name
means the current worktree's session. Windows → "unsupported on this platform".

**Session membership** (union of):

1. **Env tag** — processes whose initial environment contains
   `LUCHTA_SESSION_ID=<record id>`. The environment survives fork, `setsid`,
   and daemonizing, so detached servers (e.g. overmind's tmux server) are
   covered without knowing what the wrapped command is. Read via
   `/proc/<pid>/environ` (Linux) and `sysctl(KERN_PROCARGS2)` (macOS);
   unreadable processes are skipped.
2. **Descendants** — the wrapper's process tree by ppid walk, covering
   children that scrubbed their environment but did not detach.

Always excluded: the wrapper, and the requesting CLI process and its
ancestors (so running `pause` from a pane inside the session does not freeze
the caller). Only same-user processes are considered.

The only platform-specific code is a small `ProcessTable` interface (list
pids with ppid and state; read a pid's initial environment) with Linux and
macOS implementations.

**Freeze to fixpoint:** enumerate → SIGSTOP every member → re-enumerate;
repeat until a pass finds no new running member, capped at 10 passes
(`PauseDidNotConverge` if exceeded, after resuming everything already
stopped). Resume re-enumerates and sends SIGCONT to every member.

**Wrapper owns the state.** Each wrapper serves `slot-<N>.sock` (Unix domain
socket in the registry dir, mode 0600) accepting line-delimited JSON commands
`pause`, `resume`, `status`. The wrapper performs the freeze/thaw and writes
`paused_at` into its record. Because it owns the state it can:

- on SIGINT while paused: resume members first, then let the interrupt
  proceed normally (otherwise stopped children never see it and the terminal
  hangs);
- on any exit path while paused (child exit, SIGTERM, SIGHUP): resume before
  releasing the slot, so no stopped orphans remain.

**Stale recovery:** if the wrapper is `kill -9`'d while paused, its members
stay stopped. `luchta sessions resume --stale` scans for stopped processes
tagged with the `id` of any stale record and SIGCONTs them. `luchta sessions`
warns when it finds stopped processes tagged with a stale record's `id`.

**Interactions:**

- Build lock: when `luchta run` waits on a build lock and a paused session
  exists for the same workspace root, it prints "build lock is held by paused
  session `<name>` — run `luchta sessions resume <name>`" instead of waiting
  silently.
- Watchers: filesystem events queue while paused and arrive in a burst on
  resume. `luchta watch` already rescans on internal channel overflow; phase 3
  must verify it also rescans on OS-level overflow (notify `Flag::Rescan` —
  inotify `IN_Q_OVERFLOW`, FSEvents `MustScanSubDirs`) and add it if missing.
- Documented only: a paused Bull worker's in-flight jobs can be treated as
  stalled and picked up by another worktree's worker via the shared redis;
  DB/redis connections may reconnect on resume; pause saves CPU/battery, not
  memory or ports.

## 6. Crate layout

- `crates/luchta-types`: `SessionsConfig` schema + validation.
- `crates/luchta-sessions` (new, `thiserror`): port-plan resolution, registry
  paths, record I/O, locks, allocation, name sanitizing, proxy-liveness
  lookup; phase 3 adds the `ProcessTable` (Linux/macOS), membership
  enumeration, freeze/thaw, and the control-socket protocol.
- `crates/luchta-session-proxy` (new, `thiserror`, tokio + hyper server): the
  proxy; depends on `luchta-sessions`.
- `crates/luchta-cli`: `session`, `sessions` (incl. `pause`/`resume`),
  `proxy` subcommands; the wrapper's control-socket server; `miette`
  diagnostics for `SessionAlreadyRunning`, `NoFreeSlot`, invalid config,
  missing `sessions` block, and pause errors.

## 7. Error handling

| Condition | Behavior |
| --- | --- |
| No `sessions` config | Diagnostic with an example block; exit non-zero before spawning. |
| Invalid stride / range / names | Config diagnostic identifying the offending key. |
| Same worktree already live | `SessionAlreadyRunning` with pid, start time, slot, URLs. |
| All slots busy | `NoFreeSlot` listing live sessions. |
| Registry dir not creatable | I/O diagnostic with the path. |
| Corrupt record JSON with lock held | Listed as `<unreadable>`; slot treated as live. |
| Corrupt `last-slots.json` | Ignored; fall back to lowest free slot. |
| Child fails to spawn | Diagnostic; slot released. |
| Pause/resume on Windows | `unsupported on this platform`. |
| Named session not live | `SessionNotFound`, listing live sessions. |
| Control socket unreachable, lock held | `SessionUnresponsive` with pid. |
| Pause already paused / resume running | No-op, reports current state, exit 0. |
| Freeze does not converge in 10 passes | Resume all stopped members; `PauseDidNotConverge`. |

## 8. Testing

- **Unit:** config validation (stride overlap, 65535 bound, service names,
  single default); slot selection (sticky, lowest free, skip on probe-bind
  failure); name sanitizing and de-duplication; `Host` parsing and routing
  table.
- **Registry (temp `LUCHTA_SESSIONS_DIR`):** a lock held by a child process →
  live; child killed → stale and reusable; concurrent allocations never share
  a slot. Tests mutating process env call `require_nextest()`.
- **E2E (`watch-e2e` group):** two `luchta session` runs in different temp
  workspaces receive distinct ports in the child env; a second run in the same
  workspace is refused with the first's pid; exit code propagates;
  `luchta sessions --json` reflects live sessions and drops killed ones; the
  proxy routes by `Host` to a toy HTTP server, serves the index, returns 502
  for a dead upstream, and round-trips a websocket upgrade.
- **Pause (Linux and macOS CI):** a fixture that spawns a child which
  detaches (`setsid` / double fork) while keeping the env tag — pause stops it,
  resume continues it (state via `/proc/<pid>/stat` on Linux, `kinfo_proc` on
  macOS); a forking child still converges; pause issued from inside the
  session does not stop the caller; SIGINT to a paused wrapper exits cleanly
  with no stopped members left; after `kill -9` of a paused wrapper,
  `resume --stale` recovers members.
- **Hermetic env:** add `LUCHTA_SESSIONS_DIR` to both nextest wrapper
  allowlists, with a test covering that it passes through.

## 9. User-visible changes

Changeset `luchta: minor` per phase: phase 1 "Add `luchta session` and
`luchta sessions` for per-worktree port allocation"; phase 2 "Add
`luchta proxy` for named `*.localhost` routing to sessions"; phase 3 "Add
`luchta sessions pause`/`resume` to freeze and thaw a session's processes".
