# Sessions (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `luchta session -- <cmd>` (per-worktree port allocation with duplicate-session refusal) and `luchta sessions [--json]` (list live sessions and URLs).

**Architecture:** A new `luchta-sessions` library crate owns the port plan (validated from a new `sessions` config block in `luchta-types`), a machine-wide file registry whose liveness is an OS file lock per slot, and slot allocation. `luchta-cli` adds two subcommands: a wrapper that allocates, sets port env vars, spawns the command, forwards signals and propagates the exit code; and a listing command.

**Tech Stack:** Rust 1.96 (`std::fs::File::lock`/`try_lock`), tokio (process + signals), serde/serde_json, `indexmap` (declared port order), `dirs`, thiserror (library), miette (CLI), clap derive, assert_cmd/assert_fs + nextest.

**Spec:** `docs/superpowers/specs/2026-09-24-sessions-and-port-routing-design.md` (sections 1–3, 6–9 as they apply to phase 1). Read it before starting.

## Global Constraints

- **No commits.** The user's standing rule: never run `git commit` unless the user explicitly asks in the current turn. Each task ends at a checkpoint (`git status`), not a commit.
- `luchta-sessions` uses `thiserror`; `miette` only in `luchta-cli` (AGENTS.md).
- Config keys are camelCase with a snake_case `alias` (existing `luchta-types` convention, e.g. `maxWeight`/`max_weight`).
- Defaults: `slotStride` = 1000, `maxSlots` = 20. Port for slot N = `default + N * slotStride`.
- Registry dir: `LUCHTA_SESSIONS_DIR` if set and non-empty, else `dirs::runtime_dir()/luchta/sessions`, else `dirs::cache_dir()/luchta/sessions`.
- Registry files: `slot-<N>.lock`, `slot-<N>.json`, `alloc.lock`, `last-slots.json`. Lock files are never unlinked.
- Liveness is the slot lock only — never pid or timestamp checks.
- Child env: each declared port var, `LUCHTA_SESSION_NAME`, `LUCHTA_SESSION_SLOT`, `LUCHTA_SESSION_ID`.
- Session record JSON uses snake_case field names (it is also the `luchta sessions --json` output).
- Run tests with `cargo nextest run`, never `cargo test` (AGENTS.md).
- New workspace dependency allowed: `indexmap = { version = "2", features = ["serde"] }`. No other new third-party crates.

**Deliberate deviations from the spec (tell the reviewer):**
1. Config *validation* lives in `luchta_sessions::PortPlan::from_config` (thiserror), not `luchta-types`: `luchta-types` has no thiserror and only validates inside serde. Consequence: an invalid `sessions` block is reported by `luchta session`, not by `luchta run`.
2. `LUCHTA_SESSIONS_DIR` is **not** added to the nextest hermetic allowlists. Tests pass it explicitly to spawned `luchta` processes via `Command::env`, which the wrapper does not filter; allowlisting would only let a developer's ambient value leak into tests.
3. Ports are held in an `IndexMap` so "first declared `http` port" (the default service) is well-defined.
4. `Session` removes its `slot-<N>.json` on drop (clean exit) in addition to ignoring stale records.

## Review Focus

1. **Same worktree via a symlink or different path spelling** → must be refused as a duplicate (roots are canonicalized). Tests: Task 4 `same_worktree_via_symlink_is_refused`, Task 5 `symlinked_workspace_path_counts_as_the_same_worktree`.
2. **A non-Luchta process already listening on a slot's port** (including wildcard/IPv6 listeners on macOS, where std's `SO_REUSEADDR` lets a loopback bind succeed) → that slot is skipped. Test: Task 4 `allocate_skips_a_slot_whose_port_is_really_bound`.
3. **Wrapped command fails to start** (typo, not on PATH) → clear diagnostic and the slot is freed at once. Test: Task 5 `failed_spawn_releases_the_slot`.
4. **Ctrl-C in the session terminal** (SIGINT to the whole foreground group) → wrapper must not die first; it waits for the child's own shutdown and exits with the child's code. Test: Task 5 `ctrl_c_to_the_process_group_lets_the_child_finish`.
5. **Shell already exports a declared port var** (old `.envrc`, stray `export`) → session value wins with a warning. Test: Task 5 `preset_port_variable_is_overridden_with_a_warning`.

## File Structure

| Path | Responsibility |
| --- | --- |
| `Cargo.toml` (modify) | Add `crates/luchta-sessions` member; `indexmap` workspace dep. |
| `crates/luchta-types/Cargo.toml` (modify) | Depend on `indexmap`. |
| `crates/luchta-types/src/sessions.rs` (create) | `SessionsConfig`, `SessionPortSpec` serde schema. |
| `crates/luchta-types/src/lib.rs`, `src/config.rs` (modify) | Export schema; `LuchtaConfig.sessions`. |
| `crates/luchta-sessions/Cargo.toml` (create) | New crate manifest. |
| `crates/luchta-sessions/src/lib.rs` (create) | Module wiring, re-exports, `unix_now`. |
| `crates/luchta-sessions/src/name.rs` (create) | DNS-label sanitizing and de-duplication. |
| `crates/luchta-sessions/src/plan.rs` (create) | `PortPlan` validation and per-slot resolution; `ResolvedPort`. |
| `crates/luchta-sessions/src/registry.rs` (create) | Registry dir, slot locks, records, liveness scan, sticky slots. |
| `crates/luchta-sessions/src/alloc.rs` (create) | `allocate`, `Session`, `PortProbe`/`TcpProbe`. |
| `crates/luchta-cli/Cargo.toml` (modify) | Depend on `luchta-sessions`. |
| `crates/luchta-cli/src/cli.rs` (modify) | `Session`, `Sessions` variants. |
| `crates/luchta-cli/src/main.rs` (modify) | `mod` lines and dispatch arms. |
| `crates/luchta-cli/src/session.rs` (create) | Wrapper: config → allocate → banner → spawn → signals → exit code; shared URL/age formatting. |
| `crates/luchta-cli/src/sessions_cmd.rs` (create) | `luchta sessions` table / JSON. |
| `crates/luchta-cli/tests/session_e2e.rs` (create) | Process-spawning end-to-end tests. |
| `.config/nextest.toml` (modify) | Put `session_e2e` in the `watch-e2e` group. |
| `README.md` (modify) | Crate layout entry and `### Sessions` section. |
| `.changeset/add-luchta-sessions.md` (create) | `minor` changeset. |

---

### Task 1: `sessions` config schema in `luchta-types`

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]`)
- Modify: `crates/luchta-types/Cargo.toml`
- Create: `crates/luchta-types/src/sessions.rs`
- Modify: `crates/luchta-types/src/lib.rs` (module list near line 6, re-exports near line 10)
- Modify: `crates/luchta-types/src/config.rs:58-80` (`LuchtaConfig`)

**Interfaces:**
- Produces: `luchta_types::SessionsConfig { slot_stride: u32, max_slots: u32, ports: IndexMap<String, SessionPortSpec> }`; `luchta_types::SessionPortSpec { default: u16, service: Option<String>, http: bool, default_service: bool }`; `LuchtaConfig.sessions: Option<SessionsConfig>`.

- [ ] **Step 1: Add the dependency**

In root `Cargo.toml` `[workspace.dependencies]` (alphabetical position), add:

```toml
indexmap = { version = "2", features = ["serde"] }
```

In `crates/luchta-types/Cargo.toml` `[dependencies]`:

```toml
indexmap = { workspace = true }
```

- [ ] **Step 2: Write the failing tests**

Create `crates/luchta-types/src/sessions.rs` containing only the tests for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuchtaConfig;

    #[test]
    fn parses_camel_case_with_defaults_in_declared_order() {
        let config: SessionsConfig = serde_json::from_str(
            r#"{"ports":{
                "Z_PORT":{"default":8090},
                "A_PORT":{"default":8081,"service":"web","http":true,"defaultService":true}
            }}"#,
        )
        .unwrap();

        assert_eq!(config.slot_stride, 1000);
        assert_eq!(config.max_slots, 20);
        assert_eq!(
            config.ports.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Z_PORT", "A_PORT"]
        );
        assert_eq!(
            config.ports["A_PORT"],
            SessionPortSpec {
                default: 8081,
                service: Some("web".to_string()),
                http: true,
                default_service: true,
            }
        );
        let api = &config.ports["Z_PORT"];
        assert!(!api.http && api.service.is_none() && !api.default_service);
    }

    #[test]
    fn accepts_snake_case_aliases() {
        let config: SessionsConfig = serde_json::from_str(
            r#"{"slot_stride":500,"max_slots":4,
                "ports":{"P":{"default":1,"http":true,"default_service":true}}}"#,
        )
        .unwrap();

        assert_eq!(config.slot_stride, 500);
        assert_eq!(config.max_slots, 4);
        assert!(config.ports["P"].default_service);
    }

    #[test]
    fn rejects_a_default_port_above_65535() {
        let result: Result<SessionsConfig, _> =
            serde_json::from_str(r#"{"ports":{"P":{"default":70000}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn luchta_config_sessions_block_is_optional() {
        let without: LuchtaConfig = serde_json::from_str("{}").unwrap();
        assert!(without.sessions.is_none());

        let with: LuchtaConfig =
            serde_json::from_str(r#"{"sessions":{"ports":{"P":{"default":1}}}}"#).unwrap();
        assert_eq!(with.sessions.unwrap().ports["P"].default, 1);
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-types sessions`
Expected: compile error — `SessionsConfig`, `SessionPortSpec`, `LuchtaConfig.sessions` not defined.

- [ ] **Step 4: Implement the schema**

Prepend to `crates/luchta-types/src/sessions.rs` (above the tests):

```rust
//! `sessions` block of the luchta config: the ports `luchta session` allocates
//! per worktree. Semantic validation lives in `luchta-sessions` (`PortPlan`).

use indexmap::IndexMap;
use serde::Deserialize;

/// Ports that `luchta session` gives each concurrent worktree its own copy of.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionsConfig {
    /// Offset between one slot's ports and the next slot's.
    #[serde(default = "default_slot_stride", rename = "slotStride", alias = "slot_stride")]
    pub slot_stride: u32,
    /// Number of slots (concurrent sessions) available.
    #[serde(default = "default_max_slots", rename = "maxSlots", alias = "max_slots")]
    pub max_slots: u32,
    /// Env var name → port declaration, in declared order.
    #[serde(default)]
    pub ports: IndexMap<String, SessionPortSpec>,
}

/// One port the app reads from an environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionPortSpec {
    /// Port used by slot 0.
    pub default: u16,
    /// Display and routing name (a DNS label).
    #[serde(default)]
    pub service: Option<String>,
    /// Whether the port serves HTTP that a browser can open.
    #[serde(default)]
    pub http: bool,
    /// Whether this is the service a bare `<session>.localhost` routes to.
    #[serde(default, rename = "defaultService", alias = "default_service")]
    pub default_service: bool,
}

fn default_slot_stride() -> u32 {
    1000
}

fn default_max_slots() -> u32 {
    20
}
```

In `crates/luchta-types/src/lib.rs`, next to `mod config;` add `mod sessions;`, and next to `pub use config::*;` add:

```rust
pub use sessions::{SessionPortSpec, SessionsConfig};
```

In `crates/luchta-types/src/config.rs`, add to `LuchtaConfig` after the `cache` field (import `crate::SessionsConfig` the same way `EnvSpec`/`CacheConfig` are imported in that file):

```rust
    /// Per-worktree port allocation for `luchta session`.
    #[serde(default)]
    pub sessions: Option<SessionsConfig>,
```

Then find every struct-literal construction and add `sessions: None`:

Run: `grep -rn "LuchtaConfig {" crates --include='*.rs'`
For each hit that builds the struct with named fields (not a pattern or type position), add `sessions: None,`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-types && cargo build --workspace`
Expected: all luchta-types tests PASS; workspace builds (catches missed struct literals).

- [ ] **Step 6: Checkpoint**

Run: `git status` — expect the five files above modified/created. Do not commit.

---

### Task 2: `luchta-sessions` crate — names and port plan

**Files:**
- Modify: `Cargo.toml` (`[workspace] members`: add `"crates/luchta-sessions"`)
- Create: `crates/luchta-sessions/Cargo.toml`
- Create: `crates/luchta-sessions/src/lib.rs`
- Create: `crates/luchta-sessions/src/name.rs`
- Create: `crates/luchta-sessions/src/plan.rs`

**Interfaces:**
- Consumes: `luchta_types::{SessionsConfig, SessionPortSpec}` (Task 1).
- Produces:
  - `pub fn sanitize_label(raw: &str) -> String`
  - `pub fn is_dns_label(value: &str) -> bool`
  - `pub fn dedupe_name(base: &str, taken: &[&str]) -> String`
  - `pub struct ResolvedPort { pub env: String, pub port: u16, pub service: Option<String>, pub http: bool, pub default_service: bool }` (Serialize/Deserialize, Clone, PartialEq, Eq, Debug)
  - `pub struct PortPlan` with `pub fn from_config(&SessionsConfig) -> Result<PortPlan, PlanError>`, `pub fn max_slots(&self) -> u32`, `pub fn ports_for_slot(&self, slot: u32) -> Vec<ResolvedPort>`
  - `pub enum PlanError` (variants below)
  - `pub fn unix_now() -> u64`

- [ ] **Step 1: Create the crate skeleton**

`crates/luchta-sessions/Cargo.toml`:

```toml
[package]
name = "luchta-sessions"
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
rust-version.workspace = true
repository.workspace = true
homepage.workspace = true
description.workspace = true

[dependencies]
dirs = { workspace = true }
luchta-types = { path = "../luchta-types" }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
```

`crates/luchta-sessions/src/lib.rs`:

```rust
//! Per-worktree dev sessions: port plans, the machine-wide session registry,
//! and slot allocation used by `luchta session` and `luchta sessions`.

mod name;
mod plan;

pub use name::{dedupe_name, is_dns_label, sanitize_label};
pub use plan::{PlanError, PortPlan, ResolvedPort};

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
```

Add `"crates/luchta-sessions"` to the explicit `members` list in root `Cargo.toml`.

- [ ] **Step 2: Write failing name tests**

`crates/luchta-sessions/src/name.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_to_a_lowercase_dns_label() {
        assert_eq!(sanitize_label("Feature_X"), "feature-x");
        assert_eq!(sanitize_label("  --My  Branch!!--"), "my-branch");
        assert_eq!(sanitize_label("ünïcode"), "n-code");
        assert_eq!(sanitize_label("___"), "session");
        let long = sanitize_label(&"a".repeat(100));
        assert_eq!(long.len(), 63);
        assert!(is_dns_label(&long));
    }

    #[test]
    fn truncation_never_leaves_a_trailing_hyphen() {
        let raw = format!("{}-b", "a".repeat(62));
        let label = sanitize_label(&raw);
        assert_eq!(label, "a".repeat(62));
    }

    #[test]
    fn recognizes_dns_labels() {
        assert!(is_dns_label("web"));
        assert!(is_dns_label("web-2"));
        assert!(!is_dns_label(""));
        assert!(!is_dns_label("-web"));
        assert!(!is_dns_label("web-"));
        assert!(!is_dns_label("Web"));
        assert!(!is_dns_label("web_2"));
        assert!(!is_dns_label(&"a".repeat(64)));
    }

    #[test]
    fn dedupes_with_numeric_suffixes() {
        assert_eq!(dedupe_name("app", &[]), "app");
        assert_eq!(dedupe_name("app", &["app"]), "app-2");
        assert_eq!(dedupe_name("app", &["app", "app-2"]), "app-3");
    }

    #[test]
    fn dedupe_keeps_long_names_within_63_characters() {
        let base = "a".repeat(63);
        let name = dedupe_name(&base, &[base.as_str()]);
        assert_eq!(name.len(), 63);
        assert!(name.ends_with("-2"));
        assert!(is_dns_label(&name));
    }
}
```

In `lib.rs` temporarily keep the `pub use name::...` line; the build fails until Step 4.

- [ ] **Step 3: Write failing plan tests**

`crates/luchta-sessions/src/plan.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use luchta_types::SessionsConfig;

    fn config(json: &str) -> SessionsConfig {
        serde_json::from_str(json).unwrap()
    }

    fn plan(json: &str) -> Result<PortPlan, PlanError> {
        PortPlan::from_config(&config(json))
    }

    const TWO_PORTS: &str = r#"{"ports":{
        "WEB":{"default":8081,"service":"web","http":true},
        "API":{"default":8090}
    }}"#;

    #[test]
    fn resolves_ports_for_a_slot_in_declared_order() {
        let plan = plan(TWO_PORTS).unwrap();
        let slot1 = plan.ports_for_slot(1);
        assert_eq!(
            slot1.iter().map(|p| (p.env.as_str(), p.port)).collect::<Vec<_>>(),
            [("WEB", 9081), ("API", 9090)]
        );
        assert_eq!(slot1[0].service.as_deref(), Some("web"));
        assert!(slot1[0].http);
        assert!(!slot1[1].http);
        assert_eq!(plan.max_slots(), 20);
    }

    #[test]
    fn slot_zero_uses_the_defaults() {
        let ports = plan(TWO_PORTS).unwrap().ports_for_slot(0);
        assert_eq!(ports[0].port, 8081);
        assert_eq!(ports[1].port, 8090);
    }

    #[test]
    fn rejects_a_stride_that_does_not_exceed_the_port_spread() {
        let json = r#"{"slotStride":9,"ports":{"A":{"default":8081},"B":{"default":8090}}}"#;
        assert_eq!(plan(json), Err(PlanError::StrideTooSmall { stride: 9, spread: 9 }));
    }

    #[test]
    fn accepts_a_stride_just_above_the_spread() {
        let json = r#"{"slotStride":10,"ports":{"A":{"default":8081},"B":{"default":8090}}}"#;
        assert!(plan(json).is_ok());
    }

    #[test]
    fn rejects_ports_past_65535_in_the_last_slot() {
        let json = r#"{"ports":{"WEB":{"default":60000}}}"#;
        assert_eq!(
            plan(json),
            Err(PlanError::PortOutOfRange { env: "WEB".to_string(), slot: 19, port: 79000 })
        );
    }

    #[test]
    fn rejects_empty_ports_and_zero_limits() {
        assert_eq!(plan(r#"{"ports":{}}"#), Err(PlanError::NoPorts));
        assert_eq!(
            plan(r#"{"slotStride":0,"ports":{"A":{"default":1}}}"#),
            Err(PlanError::ZeroStride)
        );
        assert_eq!(
            plan(r#"{"maxSlots":0,"ports":{"A":{"default":1}}}"#),
            Err(PlanError::ZeroMaxSlots)
        );
    }

    #[test]
    fn rejects_bad_env_and_service_names() {
        assert_eq!(
            plan(r#"{"ports":{"A=B":{"default":1}}}"#),
            Err(PlanError::InvalidEnvName { env: "A=B".to_string() })
        );
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":1,"service":"Web_1"}}}"#),
            Err(PlanError::InvalidServiceName { env: "A".to_string(), service: "Web_1".to_string() })
        );
    }

    #[test]
    fn rejects_duplicate_services_and_multiple_defaults() {
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":1,"service":"web"},"B":{"default":2,"service":"web"}}}"#),
            Err(PlanError::DuplicateService { service: "web".to_string() })
        );
        assert_eq!(
            plan(r#"{"ports":{
                "A":{"default":1,"http":true,"defaultService":true},
                "B":{"default":2,"http":true,"defaultService":true}}}"#),
            Err(PlanError::MultipleDefaultServices)
        );
    }

    #[test]
    fn rejects_a_default_service_that_is_not_http() {
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":1,"defaultService":true}}}"#),
            Err(PlanError::DefaultServiceNotHttp { env: "A".to_string() })
        );
    }
}
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-sessions`
Expected: compile errors — `sanitize_label`, `PortPlan`, etc. not defined.

- [ ] **Step 5: Implement `name.rs`**

Prepend to `crates/luchta-sessions/src/name.rs`:

```rust
//! Session and service names are DNS labels so phase 2 can route
//! `<service>.<session>.localhost`.

const MAX_LABEL_LEN: usize = 63;

/// Lowercases `raw` and collapses every run of non-`[a-z0-9]` characters into
/// one hyphen, trimming hyphens at both ends and capping at 63 characters.
/// Returns `"session"` when nothing usable remains.
pub fn sanitize_label(raw: &str) -> String {
    let mut label = String::new();
    let mut pending_hyphen = false;
    for ch in raw.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            if pending_hyphen && !label.is_empty() {
                label.push('-');
            }
            pending_hyphen = false;
            label.push(ch);
        } else {
            pending_hyphen = true;
        }
    }
    label.truncate(MAX_LABEL_LEN);
    let label = label.trim_end_matches('-');
    if label.is_empty() {
        "session".to_string()
    } else {
        label.to_string()
    }
}

/// Whether `value` is a lowercase DNS label: `[a-z0-9-]`, 1–63 characters,
/// no leading or trailing hyphen.
pub fn is_dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_LABEL_LEN
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Returns `base`, or `base-2`, `base-3`, … — the first not in `taken`, kept
/// within 63 characters. `base` must already be a sanitized (ASCII) label.
pub fn dedupe_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_string();
    }
    (2u32..)
        .map(|n| {
            let suffix = format!("-{n}");
            let keep = MAX_LABEL_LEN - suffix.len();
            let stem = base[..base.len().min(keep)].trim_end_matches('-');
            format!("{stem}{suffix}")
        })
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .expect("unbounded numeric suffixes always yield a free name")
}
```

- [ ] **Step 6: Implement `plan.rs`**

Prepend to `crates/luchta-sessions/src/plan.rs`:

```rust
//! Validated port plan: which env vars get which port in each slot.

use std::collections::BTreeSet;

use luchta_types::SessionsConfig;
use serde::{Deserialize, Serialize};

use crate::name::is_dns_label;

/// One port as handed to a session's child process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPort {
    pub env: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub http: bool,
    #[serde(default)]
    pub default_service: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("sessions.ports must declare at least one port")]
    NoPorts,
    #[error("sessions.slotStride must be greater than 0")]
    ZeroStride,
    #[error("sessions.maxSlots must be greater than 0")]
    ZeroMaxSlots,
    #[error(
        "sessions.slotStride ({stride}) must be greater than the spread of the declared default ports ({spread}), otherwise slots overlap"
    )]
    StrideTooSmall { stride: u32, spread: u32 },
    #[error(
        "sessions.ports.{env}: slot {slot} would use port {port}, above 65535; lower sessions.maxSlots or sessions.slotStride"
    )]
    PortOutOfRange { env: String, slot: u32, port: u64 },
    #[error("sessions.ports: `{env}` is not a valid environment variable name")]
    InvalidEnvName { env: String },
    #[error(
        "sessions.ports.{env}.service: `{service}` must be a lowercase DNS label ([a-z0-9-], no leading or trailing hyphen, at most 63 characters)"
    )]
    InvalidServiceName { env: String, service: String },
    #[error("sessions.ports: service `{service}` is declared more than once")]
    DuplicateService { service: String },
    #[error("sessions.ports: at most one port may set defaultService")]
    MultipleDefaultServices,
    #[error("sessions.ports.{env}: defaultService requires http: true")]
    DefaultServiceNotHttp { env: String },
}

/// A validated `sessions` config. Construct with [`PortPlan::from_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortPlan {
    stride: u32,
    max_slots: u32,
    /// Declared ports as resolved for slot 0.
    base: Vec<ResolvedPort>,
}

impl PortPlan {
    pub fn from_config(config: &SessionsConfig) -> Result<Self, PlanError> {
        validate_limits(config)?;
        validate_names(config)?;
        validate_ranges(config)?;
        let base = config
            .ports
            .iter()
            .map(|(env, spec)| ResolvedPort {
                env: env.clone(),
                port: spec.default,
                service: spec.service.clone(),
                http: spec.http,
                default_service: spec.default_service,
            })
            .collect();
        Ok(Self { stride: config.slot_stride, max_slots: config.max_slots, base })
    }

    pub fn max_slots(&self) -> u32 {
        self.max_slots
    }

    /// Ports for `slot`. `slot` must be below [`PortPlan::max_slots`]; the
    /// range check in `from_config` guarantees those ports fit in a `u16`.
    pub fn ports_for_slot(&self, slot: u32) -> Vec<ResolvedPort> {
        let offset = slot * self.stride;
        self.base
            .iter()
            .map(|base| ResolvedPort {
                port: u16::try_from(u32::from(base.port) + offset)
                    .expect("slot below max_slots stays within the validated port range"),
                ..base.clone()
            })
            .collect()
    }
}

fn validate_limits(config: &SessionsConfig) -> Result<(), PlanError> {
    if config.ports.is_empty() {
        return Err(PlanError::NoPorts);
    }
    if config.slot_stride == 0 {
        return Err(PlanError::ZeroStride);
    }
    if config.max_slots == 0 {
        return Err(PlanError::ZeroMaxSlots);
    }
    Ok(())
}

fn validate_names(config: &SessionsConfig) -> Result<(), PlanError> {
    let mut services = BTreeSet::new();
    let mut default_services = 0;
    for (env, spec) in &config.ports {
        if env.is_empty() || env.contains(['=', '\0']) {
            return Err(PlanError::InvalidEnvName { env: env.clone() });
        }
        if let Some(service) = &spec.service {
            if !is_dns_label(service) {
                return Err(PlanError::InvalidServiceName {
                    env: env.clone(),
                    service: service.clone(),
                });
            }
            if !services.insert(service.as_str()) {
                return Err(PlanError::DuplicateService { service: service.clone() });
            }
        }
        if spec.default_service {
            if !spec.http {
                return Err(PlanError::DefaultServiceNotHttp { env: env.clone() });
            }
            default_services += 1;
        }
    }
    if default_services > 1 {
        return Err(PlanError::MultipleDefaultServices);
    }
    Ok(())
}

fn validate_ranges(config: &SessionsConfig) -> Result<(), PlanError> {
    let defaults = config.ports.values().map(|spec| u32::from(spec.default));
    let min = defaults.clone().min().unwrap_or(0);
    let max = defaults.max().unwrap_or(0);
    let spread = max - min;
    if config.slot_stride <= spread {
        return Err(PlanError::StrideTooSmall { stride: config.slot_stride, spread });
    }
    let last = config.max_slots - 1;
    for (env, spec) in &config.ports {
        let port = u64::from(spec.default) + u64::from(last) * u64::from(config.slot_stride);
        if port > u64::from(u16::MAX) {
            return Err(PlanError::PortOutOfRange { env: env.clone(), slot: last, port });
        }
    }
    Ok(())
}
```

- [ ] **Step 7: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-sessions`
Expected: all name and plan tests PASS.

- [ ] **Step 8: Checkpoint**

Run: `cargo clippy -p luchta-sessions --all-targets -- -D warnings && git status`. Do not commit.

---

### Task 3: Session registry and liveness

**Files:**
- Create: `crates/luchta-sessions/src/registry.rs`
- Modify: `crates/luchta-sessions/src/lib.rs`

**Interfaces:**
- Consumes: `ResolvedPort` (Task 2).
- Produces:
  - `pub const SESSIONS_DIR_ENV: &str = "LUCHTA_SESSIONS_DIR"`
  - `#[derive(Debug, Clone)] pub struct Registry` with `pub fn new(dir: impl Into<PathBuf>) -> Self`, `pub fn from_env() -> Result<Self, RegistryError>`, `pub fn dir(&self) -> &Path`, `pub fn ensure_dir(&self) -> Result<(), RegistryError>`, `pub fn try_lock_slot(&self, slot: u32) -> Result<Option<SlotLock>, RegistryError>`, `pub fn write_record(&self, &SessionRecord) -> Result<(), RegistryError>`, `pub fn live_sessions(&self) -> Result<Vec<LiveSession>, RegistryError>`; crate-private `lock_alloc`, `live_sessions_unlocked`, `record_path`, `last_slot`, `remember_slot`.
  - `pub struct SessionRecord { pub slot: u32, pub id: String, pub name: String, pub pid: u32, pub workspace_root: PathBuf, pub branch: Option<String>, pub command: Vec<String>, pub started_at: u64, pub ports: Vec<ResolvedPort>, pub paused_at: Option<u64> }`
  - `pub struct LiveSession { pub slot: u32, pub record: Option<SessionRecord> }` (`None` = lock held but record unreadable)
  - `pub struct SlotLock` with `pub fn slot(&self) -> u32`
  - `pub enum RegistryError { NoRegistryDir, Io { action, path, source } }`

Locks use `std::fs::File::lock` / `try_lock` (stable since Rust 1.89; `flock` on Unix, `LockFileEx` on Windows). Two `File`s opened separately conflict even inside one process, which the tests rely on. std opens files with `O_CLOEXEC`, so spawned children never inherit a slot lock.

- [ ] **Step 1: Write the failing tests**

`crates/luchta-sessions/src/registry.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn registry(temp: &TempDir) -> Registry {
        let registry = Registry::new(temp.path().join("sessions"));
        registry.ensure_dir().unwrap();
        registry
    }

    fn record(slot: u32, root: &Path) -> SessionRecord {
        SessionRecord {
            slot,
            id: format!("{slot}-1-0"),
            name: format!("s{slot}"),
            pid: 1,
            workspace_root: root.to_path_buf(),
            branch: None,
            command: vec!["true".to_string()],
            started_at: 0,
            ports: Vec::new(),
            paused_at: None,
        }
    }

    #[test]
    fn a_missing_registry_dir_lists_nothing() {
        let temp = TempDir::new().unwrap();
        let registry = Registry::new(temp.path().join("absent"));
        assert!(registry.live_sessions().unwrap().is_empty());
        assert!(!temp.path().join("absent").exists());
    }

    #[test]
    fn a_locked_slot_with_a_record_is_live_until_the_lock_drops() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let lock = registry.try_lock_slot(2).unwrap().expect("slot 2 is free");
        let rec = record(2, temp.path());
        registry.write_record(&rec).unwrap();

        assert_eq!(
            registry.live_sessions().unwrap(),
            vec![LiveSession { slot: 2, record: Some(rec) }]
        );

        drop(lock);
        assert!(registry.live_sessions().unwrap().is_empty());
    }

    #[test]
    fn a_held_slot_cannot_be_locked_again() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let _held = registry.try_lock_slot(0).unwrap().unwrap();
        assert!(registry.try_lock_slot(0).unwrap().is_none());
    }

    #[test]
    fn a_corrupt_record_under_a_held_lock_is_live_but_unreadable() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let _held = registry.try_lock_slot(1).unwrap().unwrap();
        fs::write(registry.record_path(1), "{not json").unwrap();

        assert_eq!(
            registry.live_sessions().unwrap(),
            vec![LiveSession { slot: 1, record: None }]
        );
    }

    #[test]
    fn a_record_without_a_held_lock_is_stale_and_ignored() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        drop(registry.try_lock_slot(3).unwrap().unwrap());
        registry.write_record(&record(3, temp.path())).unwrap();

        assert!(registry.live_sessions().unwrap().is_empty());
    }

    #[test]
    fn writing_a_record_leaves_no_temp_file() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        registry.write_record(&record(0, temp.path())).unwrap();

        let names: Vec<String> = fs::read_dir(registry.dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["slot-0.json"]);
    }

    #[test]
    fn sticky_slots_round_trip_and_tolerate_corruption() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let root = temp.path().join("ws");

        assert_eq!(registry.last_slot(&root), None);
        registry.remember_slot(&root, 4).unwrap();
        assert_eq!(registry.last_slot(&root), Some(4));

        fs::write(registry.dir().join("last-slots.json"), "garbage").unwrap();
        assert_eq!(registry.last_slot(&root), None);
        registry.remember_slot(&root, 5).unwrap();
        assert_eq!(registry.last_slot(&root), Some(5));
    }

    #[test]
    fn only_slot_lock_files_are_scanned() {
        assert_eq!(parse_slot_lock_name("slot-12.lock"), Some(12));
        assert_eq!(parse_slot_lock_name("slot-12.json"), None);
        assert_eq!(parse_slot_lock_name("alloc.lock"), None);
        assert_eq!(parse_slot_lock_name("slot-x.lock"), None);
    }

    #[test]
    fn the_override_dir_wins_when_set_and_non_empty() {
        assert_eq!(
            resolve_dir(Some("/tmp/luchta-test-sessions".into())),
            Some(PathBuf::from("/tmp/luchta-test-sessions"))
        );
        let fallback = resolve_dir(Some("".into()));
        assert_ne!(fallback, Some(PathBuf::new()));
        if let Some(dir) = fallback {
            assert!(dir.ends_with("luchta/sessions"));
        }
    }
}
```

Add to `lib.rs`: `mod registry;` and
`pub use registry::{LiveSession, Registry, RegistryError, SessionRecord, SlotLock, SESSIONS_DIR_ENV};`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-sessions registry`
Expected: compile errors — `Registry`, `SessionRecord`, etc. not defined.

- [ ] **Step 3: Implement `registry.rs`**

Prepend to `crates/luchta-sessions/src/registry.rs`:

```rust
//! Machine-wide registry of live sessions. A slot is live exactly while some
//! process holds the OS lock on `slot-<N>.lock`; the OS drops that lock when
//! the process dies, so crashed sessions never need cleaning up. Lock files
//! are never unlinked: the lock guards the inode, not the path.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::ResolvedPort;

/// Overrides the registry directory (used by tests).
pub const SESSIONS_DIR_ENV: &str = "LUCHTA_SESSIONS_DIR";

const ALLOC_LOCK: &str = "alloc.lock";
const LAST_SLOTS: &str = "last-slots.json";

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("could not determine a directory for session records; set {SESSIONS_DIR_ENV}")]
    NoRegistryDir,
    #[error("failed to {action} {}: {source}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> RegistryError {
    let path = path.to_path_buf();
    move |source| RegistryError::Io { action, path, source }
}

/// What a live session publishes about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub slot: u32,
    /// Unique per session run; exported to the child as `LUCHTA_SESSION_ID`.
    pub id: String,
    pub name: String,
    pub pid: u32,
    /// Canonicalized workspace root.
    pub workspace_root: PathBuf,
    pub branch: Option<String>,
    pub command: Vec<String>,
    /// Unix seconds.
    pub started_at: u64,
    pub ports: Vec<ResolvedPort>,
    /// Unix seconds when paused (phase 3); `None` while running.
    #[serde(default)]
    pub paused_at: Option<u64>,
}

/// A slot whose lock is held. `record` is `None` when the record file is
/// missing or unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub slot: u32,
    pub record: Option<SessionRecord>,
}

/// Exclusive ownership of one slot; released when dropped.
#[derive(Debug)]
pub struct SlotLock {
    slot: u32,
    _file: File,
}

impl SlotLock {
    pub fn slot(&self) -> u32 {
        self.slot
    }
}

#[derive(Debug, Clone)]
pub struct Registry {
    dir: PathBuf,
}

impl Registry {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The registry named by `LUCHTA_SESSIONS_DIR`, else the per-user default.
    pub fn from_env() -> Result<Self, RegistryError> {
        resolve_dir(std::env::var_os(SESSIONS_DIR_ENV))
            .map(Self::new)
            .ok_or(RegistryError::NoRegistryDir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ensure_dir(&self) -> Result<(), RegistryError> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&self.dir)
            .map_err(io_error("create session registry", &self.dir))
    }

    /// Claims `slot` if no live process holds it.
    pub fn try_lock_slot(&self, slot: u32) -> Result<Option<SlotLock>, RegistryError> {
        let path = self.dir.join(format!("slot-{slot}.lock"));
        let file = open_lock_file(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SlotLock { slot, _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(source)) => Err(io_error("lock", &path)(source)),
        }
    }

    /// Serializes allocation and listing across processes. Blocks briefly.
    pub(crate) fn lock_alloc(&self) -> Result<File, RegistryError> {
        self.ensure_dir()?;
        let path = self.dir.join(ALLOC_LOCK);
        let file = open_lock_file(&path)?;
        file.lock().map_err(io_error("lock", &path))?;
        Ok(file)
    }

    pub fn write_record(&self, record: &SessionRecord) -> Result<(), RegistryError> {
        let json = serde_json::to_vec_pretty(record).expect("session records always serialize");
        write_atomically(&self.record_path(record.slot), &json)
    }

    /// Live sessions, sorted by slot.
    pub fn live_sessions(&self) -> Result<Vec<LiveSession>, RegistryError> {
        if !self.dir.exists() {
            return Ok(Vec::new());
        }
        let _alloc = self.lock_alloc()?;
        self.live_sessions_unlocked()
    }

    /// Like [`Registry::live_sessions`], for callers already holding `alloc.lock`.
    pub(crate) fn live_sessions_unlocked(&self) -> Result<Vec<LiveSession>, RegistryError> {
        let mut live = Vec::new();
        for slot in self.slots_on_disk()? {
            if self.try_lock_slot(slot)?.is_none() {
                live.push(LiveSession { slot, record: self.read_record(slot) });
            }
        }
        Ok(live)
    }

    pub(crate) fn record_path(&self, slot: u32) -> PathBuf {
        self.dir.join(format!("slot-{slot}.json"))
    }

    /// Slot this workspace root used last, if remembered.
    pub(crate) fn last_slot(&self, workspace_root: &Path) -> Option<u32> {
        self.read_last_slots().get(&root_key(workspace_root)).copied()
    }

    pub(crate) fn remember_slot(&self, workspace_root: &Path, slot: u32) -> Result<(), RegistryError> {
        let mut slots = self.read_last_slots();
        slots.insert(root_key(workspace_root), slot);
        let json = serde_json::to_vec_pretty(&slots).expect("slot map always serializes");
        write_atomically(&self.dir.join(LAST_SLOTS), &json)
    }

    fn read_last_slots(&self) -> BTreeMap<String, u32> {
        fs::read(self.dir.join(LAST_SLOTS))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn read_record(&self, slot: u32) -> Option<SessionRecord> {
        let bytes = fs::read(self.record_path(slot)).ok()?;
        serde_json::from_slice::<SessionRecord>(&bytes)
            .ok()
            .filter(|record| record.slot == slot)
    }

    fn slots_on_disk(&self) -> Result<Vec<u32>, RegistryError> {
        let entries = fs::read_dir(&self.dir).map_err(io_error("read session registry", &self.dir))?;
        let mut slots: Vec<u32> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| parse_slot_lock_name(&entry.file_name().to_string_lossy()))
            .collect();
        slots.sort_unstable();
        Ok(slots)
    }
}

fn resolve_dir(override_dir: Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = override_dir.filter(|dir| !dir.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    dirs::runtime_dir()
        .or_else(dirs::cache_dir)
        .map(|base| base.join("luchta").join("sessions"))
}

fn parse_slot_lock_name(name: &str) -> Option<u32> {
    name.strip_prefix("slot-")?.strip_suffix(".lock")?.parse().ok()
}

fn root_key(workspace_root: &Path) -> String {
    workspace_root.to_string_lossy().into_owned()
}

fn open_lock_file(path: &Path) -> Result<File, RegistryError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(io_error("open lock file", path))
}

fn write_atomically(path: &Path, contents: &[u8]) -> Result<(), RegistryError> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, contents).map_err(io_error("write", &tmp))?;
    fs::rename(&tmp, path).map_err(io_error("replace", path))
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-sessions`
Expected: all tests PASS.

- [ ] **Step 5: Checkpoint**

Run: `cargo clippy -p luchta-sessions --all-targets -- -D warnings && git status`. Do not commit.

---

### Task 4: Slot allocation

**Files:**
- Create: `crates/luchta-sessions/src/alloc.rs`
- Modify: `crates/luchta-sessions/src/lib.rs`

**Interfaces:**
- Consumes: `Registry` (incl. crate-private `lock_alloc`, `live_sessions_unlocked`, `record_path`, `last_slot`, `remember_slot`), `SlotLock`, `SessionRecord`, `PortPlan`, `ResolvedPort`, `sanitize_label`, `dedupe_name`, `unix_now`.
- Produces:
  - `pub trait PortProbe { fn is_free(&self, port: u16) -> bool; }`
  - `#[derive(Debug, Default, Clone, Copy)] pub struct TcpProbe;`
  - `pub struct SessionRequest<'a> { pub workspace_root: &'a Path, pub name: Option<&'a str>, pub branch: Option<String>, pub command: Vec<String>, pub pid: u32 }`
  - `pub struct Session` with `pub fn record(&self) -> &SessionRecord`; dropping it removes the record file and releases the slot.
  - `pub fn allocate(registry: &Registry, plan: &PortPlan, request: SessionRequest<'_>, probe: &dyn PortProbe) -> Result<Session, AllocError>`
  - `pub enum AllocError { Registry(RegistryError), WorkspaceRoot { path, source }, AlreadyRunning(Box<SessionRecord>), NoFreeSlot { live: Vec<SessionRecord> } }`

- [ ] **Step 1: Write the failing tests**

`crates/luchta-sessions/src/alloc.rs` (tests only for now):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::LiveSession;
    use std::{collections::BTreeSet, fs, net::TcpListener, sync::{Arc, Barrier}, thread};
    use luchta_types::SessionsConfig;
    use tempfile::TempDir;

    struct FakeProbe(BTreeSet<u16>);
    impl PortProbe for FakeProbe {
        fn is_free(&self, port: u16) -> bool {
            !self.0.contains(&port)
        }
    }
    fn all_free() -> FakeProbe {
        FakeProbe(BTreeSet::new())
    }

    fn plan_from(json: &str) -> PortPlan {
        let config: SessionsConfig = serde_json::from_str(json).unwrap();
        PortPlan::from_config(&config).unwrap()
    }
    fn plan(max_slots: u32) -> PortPlan {
        plan_from(&format!(
            r#"{{"maxSlots":{max_slots},"ports":{{
                "WEB":{{"default":41081,"service":"web","http":true}},
                "API":{{"default":41090}}}}}}"#
        ))
    }

    struct Fixture {
        temp: TempDir,
        registry: Registry,
    }
    impl Fixture {
        fn new() -> Self {
            let temp = TempDir::new().unwrap();
            let registry = Registry::new(temp.path().join("registry"));
            Self { temp, registry }
        }
        fn workspace(&self, relative: &str) -> PathBuf {
            let dir = self.temp.path().join(relative);
            fs::create_dir_all(&dir).unwrap();
            dir
        }
    }

    fn request(root: &Path) -> SessionRequest<'_> {
        SessionRequest {
            workspace_root: root,
            name: None,
            branch: Some("main".to_string()),
            command: vec!["overmind".to_string(), "s".to_string()],
            pid: 4242,
        }
    }

    #[test]
    fn the_first_session_takes_slot_zero_and_publishes_its_record() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let session = allocate(&fx.registry, &plan(3), request(&root), &all_free()).unwrap();

        let record = session.record();
        assert_eq!(record.slot, 0);
        assert_eq!(record.name, "app");
        assert_eq!(record.pid, 4242);
        assert_eq!(record.workspace_root, fs::canonicalize(&root).unwrap());
        assert_eq!(record.ports[0].port, 41081);
        assert!(record.id.starts_with("0-4242-"));
        assert_eq!(
            fx.registry.live_sessions().unwrap(),
            vec![LiveSession { slot: 0, record: Some(record.clone()) }]
        );
    }

    #[test]
    fn a_second_worktree_takes_the_next_slot() {
        let fx = Fixture::new();
        let (a, b) = (fx.workspace("a"), fx.workspace("b"));
        let _first = allocate(&fx.registry, &plan(3), request(&a), &all_free()).unwrap();
        let second = allocate(&fx.registry, &plan(3), request(&b), &all_free()).unwrap();

        assert_eq!(second.record().slot, 1);
        assert_eq!(second.record().ports[0].port, 42081);
        assert_eq!(second.record().ports[1].port, 42090);
    }

    #[test]
    fn the_same_worktree_is_refused_with_the_existing_record() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let _first = allocate(&fx.registry, &plan(3), request(&root), &all_free()).unwrap();

        match allocate(&fx.registry, &plan(3), request(&root), &all_free()) {
            Err(AllocError::AlreadyRunning(existing)) => assert_eq!(existing.slot, 0),
            other => panic!("expected AlreadyRunning, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn same_worktree_via_symlink_is_refused() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let link = fx.temp.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let _first = allocate(&fx.registry, &plan(3), request(&root), &all_free()).unwrap();

        assert!(matches!(
            allocate(&fx.registry, &plan(3), request(&link), &all_free()),
            Err(AllocError::AlreadyRunning(_))
        ));
    }

    #[test]
    fn a_slot_with_a_busy_port_is_skipped() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let probe = FakeProbe(BTreeSet::from([41090]));
        let session = allocate(&fx.registry, &plan(3), request(&root), &probe).unwrap();
        assert_eq!(session.record().slot, 1);
    }

    #[test]
    fn a_worktree_gets_its_previous_slot_back() {
        let fx = Fixture::new();
        let (a, b) = (fx.workspace("a"), fx.workspace("b"));
        let first = allocate(&fx.registry, &plan(3), request(&b), &all_free()).unwrap();
        let second = allocate(&fx.registry, &plan(3), request(&a), &all_free()).unwrap();
        assert_eq!(second.record().slot, 1);
        drop((first, second));

        let again = allocate(&fx.registry, &plan(3), request(&a), &all_free()).unwrap();
        assert_eq!(again.record().slot, 1);
    }

    #[test]
    fn dropping_a_session_frees_the_slot_and_removes_the_record() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let session = allocate(&fx.registry, &plan(3), request(&root), &all_free()).unwrap();
        let record_path = fx.registry.record_path(0);
        assert!(record_path.exists());

        drop(session);
        assert!(!record_path.exists());
        assert!(fx.registry.live_sessions().unwrap().is_empty());
        assert!(allocate(&fx.registry, &plan(3), request(&root), &all_free()).is_ok());
    }

    #[test]
    fn no_free_slot_reports_the_live_sessions() {
        let fx = Fixture::new();
        let held: Vec<Session> = (0..2)
            .map(|i| {
                let root = fx.workspace(&format!("w{i}"));
                allocate(&fx.registry, &plan(2), request(&root), &all_free()).unwrap()
            })
            .collect();
        let extra = fx.workspace("extra");

        match allocate(&fx.registry, &plan(2), request(&extra), &all_free()) {
            Err(AllocError::NoFreeSlot { live }) => assert_eq!(live.len(), 2),
            other => panic!("expected NoFreeSlot, got {other:?}"),
        }
        drop(held);
    }

    #[test]
    fn default_names_come_from_the_directory_and_are_deduplicated() {
        let fx = Fixture::new();
        let (a, b) = (fx.workspace("one/Feature X"), fx.workspace("two/Feature X"));
        let first = allocate(&fx.registry, &plan(3), request(&a), &all_free()).unwrap();
        let second = allocate(&fx.registry, &plan(3), request(&b), &all_free()).unwrap();
        assert_eq!(first.record().name, "feature-x");
        assert_eq!(second.record().name, "feature-x-2");
    }

    #[test]
    fn an_explicit_name_is_sanitized() {
        let fx = Fixture::new();
        let root = fx.workspace("app");
        let mut req = request(&root);
        req.name = Some("My_Branch");
        let session = allocate(&fx.registry, &plan(3), req, &all_free()).unwrap();
        assert_eq!(session.record().name, "my-branch");
    }

    #[test]
    fn concurrent_allocations_never_share_a_slot() {
        let fx = Arc::new(Fixture::new());
        let barrier = Arc::new(Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (fx, barrier) = (Arc::clone(&fx), Arc::clone(&barrier));
                thread::spawn(move || {
                    let root = fx.workspace(&format!("w{i}"));
                    barrier.wait();
                    allocate(&fx.registry, &plan(8), request(&root), &all_free()).unwrap()
                })
            })
            .collect();
        let sessions: Vec<Session> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let slots: BTreeSet<u32> = sessions.iter().map(|s| s.record().slot).collect();
        assert_eq!(slots.len(), 8);
    }

    #[test]
    fn tcp_probe_sees_a_bound_port_as_busy() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(!TcpProbe.is_free(port));
    }

    #[test]
    fn allocate_skips_a_slot_whose_port_is_really_bound() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let max_slots = (u32::from(u16::MAX) - u32::from(port) + 1).min(50);
        assert!(max_slots >= 2, "ephemeral port {port} too close to 65535");
        let plan = plan_from(&format!(
            r#"{{"slotStride":1,"maxSlots":{max_slots},"ports":{{"WEB":{{"default":{port}}}}}}}"#
        ));
        let fx = Fixture::new();
        let root = fx.workspace("app");

        let session = allocate(&fx.registry, &plan, request(&root), &TcpProbe).unwrap();
        assert_ne!(session.record().slot, 0);
        assert_ne!(session.record().ports[0].port, port);
    }
}
```

Add to `lib.rs`: `mod alloc;` and
`pub use alloc::{allocate, AllocError, PortProbe, Session, SessionRequest, TcpProbe};`

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-sessions alloc`
Expected: compile errors — `allocate`, `Session`, etc. not defined.

- [ ] **Step 3: Implement `alloc.rs`**

Prepend to `crates/luchta-sessions/src/alloc.rs`:

```rust
//! Choosing a slot for a new session.

use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    dedupe_name, sanitize_label, unix_now, PortPlan, Registry, RegistryError, ResolvedPort,
    SessionRecord, SlotLock,
};

/// Decides whether a port is free for a new session to use.
pub trait PortProbe {
    fn is_free(&self, port: u16) -> bool;
}

/// Probes real sockets on loopback.
#[derive(Debug, Default, Clone, Copy)]
pub struct TcpProbe;

const CONNECT_TIMEOUT: Duration = Duration::from_millis(100);

impl PortProbe for TcpProbe {
    fn is_free(&self, port: u16) -> bool {
        // A loopback bind can succeed beside an existing wildcard listener on
        // macOS (std sets SO_REUSEADDR), and Node often listens on `::`, so a
        // port that accepts a connection on either loopback also counts as busy.
        let bindable = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
        bindable
            && !accepts(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
            && !accepts(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
    }
}

fn accepts(ip: IpAddr, port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::new(ip, port), CONNECT_TIMEOUT).is_ok()
}

#[derive(Debug, thiserror::Error)]
pub enum AllocError {
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("failed to resolve workspace root {}: {source}", .path.display())]
    WorkspaceRoot {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "a session is already running for this worktree: {} (pid {}, slot {})",
        .0.name, .0.pid, .0.slot
    )]
    AlreadyRunning(Box<SessionRecord>),
    #[error("no free session slot ({} live sessions)", .live.len())]
    NoFreeSlot { live: Vec<SessionRecord> },
}

/// What the caller wants to run.
#[derive(Debug, Clone)]
pub struct SessionRequest<'a> {
    pub workspace_root: &'a Path,
    /// Requested name; sanitized. Defaults to the workspace directory name.
    pub name: Option<&'a str>,
    pub branch: Option<String>,
    pub command: Vec<String>,
    pub pid: u32,
}

/// An allocated, published session. Dropping it removes the record and frees
/// the slot.
#[derive(Debug)]
pub struct Session {
    registry: Registry,
    record: SessionRecord,
    _lock: SlotLock,
}

impl Session {
    pub fn record(&self) -> &SessionRecord {
        &self.record
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Runs before `_lock` is released, so no one reads a half-gone slot.
        let _ = fs::remove_file(self.registry.record_path(self.record.slot));
    }
}

pub fn allocate(
    registry: &Registry,
    plan: &PortPlan,
    request: SessionRequest<'_>,
    probe: &dyn PortProbe,
) -> Result<Session, AllocError> {
    let workspace_root = fs::canonicalize(request.workspace_root).map_err(|source| {
        AllocError::WorkspaceRoot { path: request.workspace_root.to_path_buf(), source }
    })?;
    let _alloc = registry.lock_alloc()?;
    let records: Vec<SessionRecord> = registry
        .live_sessions_unlocked()?
        .into_iter()
        .filter_map(|live| live.record)
        .collect();
    if let Some(existing) = records.iter().find(|r| r.workspace_root == workspace_root) {
        return Err(AllocError::AlreadyRunning(Box::new(existing.clone())));
    }
    let preferred = registry.last_slot(&workspace_root);
    let Some((lock, ports)) = claim_slot(registry, plan, preferred, probe)? else {
        return Err(AllocError::NoFreeSlot { live: records });
    };
    let record = SessionRecord {
        slot: lock.slot(),
        id: session_id(lock.slot(), request.pid),
        name: session_name(request.name, &workspace_root, &records),
        pid: request.pid,
        workspace_root,
        branch: request.branch,
        command: request.command,
        started_at: unix_now(),
        ports,
        paused_at: None,
    };
    registry.write_record(&record)?;
    // Sticky slots are a convenience; failing to remember one must not fail
    // the session.
    let _ = registry.remember_slot(&record.workspace_root, record.slot);
    Ok(Session { registry: registry.clone(), record, _lock: lock })
}

fn claim_slot(
    registry: &Registry,
    plan: &PortPlan,
    preferred: Option<u32>,
    probe: &dyn PortProbe,
) -> Result<Option<(SlotLock, Vec<ResolvedPort>)>, RegistryError> {
    let preferred = preferred.filter(|slot| *slot < plan.max_slots());
    for slot in preferred.into_iter().chain(0..plan.max_slots()) {
        let Some(lock) = registry.try_lock_slot(slot)? else {
            continue;
        };
        let ports = plan.ports_for_slot(slot);
        if ports.iter().all(|port| probe.is_free(port.port)) {
            return Ok(Some((lock, ports)));
        }
    }
    Ok(None)
}

fn session_name(requested: Option<&str>, workspace_root: &Path, live: &[SessionRecord]) -> String {
    let base = match requested {
        Some(name) => sanitize_label(name),
        None => sanitize_label(
            &workspace_root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        ),
    };
    let taken: Vec<&str> = live.iter().map(|record| record.name.as_str()).collect();
    dedupe_name(&base, &taken)
}

fn session_id(slot: u32, pid: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{slot}-{pid}-{nanos:x}")
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-sessions`
Expected: all tests PASS. Then `cargo nextest run -p luchta-sessions --stress-count=5` — the concurrency and real-bind tests must stay green.

- [ ] **Step 5: Checkpoint**

Run: `cargo clippy -p luchta-sessions --all-targets -- -D warnings && git status`. Do not commit.

---

### Task 5: `luchta session` wrapper

**Files:**
- Modify: `crates/luchta-cli/Cargo.toml` (`[dependencies]`: `luchta-sessions = { path = "../luchta-sessions" }`)
- Modify: `crates/luchta-cli/src/cli.rs` (`Commands` enum, lines ~50-272)
- Modify: `crates/luchta-cli/src/main.rs` (module list lines 1-23; `run` dispatch lines ~150-212)
- Create: `crates/luchta-cli/src/session.rs`
- Create: `crates/luchta-cli/tests/session_e2e.rs`
- Modify: `.config/nextest.toml` (the `watch-e2e` override filter)

**Interfaces:**
- Consumes: `luchta_sessions::{allocate, AllocError, PortPlan, Registry, SessionRecord, SessionRequest, TcpProbe, unix_now}`; `crate::config::load_config(workspace_root) -> miette::Result<LuchtaConfig>` (`crates/luchta-cli/src/config.rs:39`).
- Produces: `Commands::Session { name: Option<String>, quiet: bool, command: Vec<String> }`; `session::dispatch_session(&Path, Option<String>, bool, Vec<String>) -> miette::Result<()>`; `pub(crate) fn url_lines(record: &SessionRecord, indent: &str) -> String`; `pub(crate) fn format_age(started_at: u64) -> String` (both used by Task 6).

- [ ] **Step 1: Write the failing e2e tests**

Create `crates/luchta-cli/tests/session_e2e.rs`:

```rust
//! End-to-end tests for `luchta session` (and, from Task 6, `luchta sessions`).
#![cfg(unix)]

mod common;

use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::OnceLock,
    thread,
    time::{Duration, Instant},
};

use assert_fs::{prelude::*, TempDir};

const CONFIG: &str = r#"{"sessions":{"ports":{"TEST_WEB_PORT":{"default":41081,"service":"web","http":true},"TEST_API_PORT":{"default":41090}}}}"#;
const HOLD: &str = r#"echo "$TEST_WEB_PORT" > port.out; while [ ! -f stop ]; do sleep 0.05; done"#;
const WAIT: Duration = Duration::from_secs(10);

fn luchta_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| assert_cmd::cargo::cargo_bin("luchta"))
}

fn workspace_with_config(config_json: &str) -> TempDir {
    let temp = TempDir::new().unwrap();
    common::write_root_workspace(&temp);
    common::write_executable(
        temp.child("luchta-config.sh").path(),
        &format!("#!/bin/sh\necho '{config_json}'\n"),
    );
    temp
}

/// A `luchta` invocation against `workspace` with an isolated session registry.
fn luchta(workspace: &Path, registry: &Path) -> Command {
    let mut cmd = Command::new(luchta_bin());
    cmd.env("NO_COLOR", "1")
        .env("LUCHTA_SESSIONS_DIR", registry)
        .arg("--workspace-root")
        .arg(workspace)
        .current_dir(workspace)
        .stdin(Stdio::null());
    cmd
}

fn session_sh(workspace: &Path, registry: &Path, script: &str) -> Command {
    let mut cmd = luchta(workspace, registry);
    cmd.args(["session", "--", "sh", "-c", script]);
    cmd
}

fn stderr_of(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn wait_for_line(path: &Path) -> String {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            if text.ends_with('\n') {
                return text.trim_end().to_string();
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {}", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

/// A session running in the background in its own process group; the whole
/// group is killed if the test ends while it is still running.
struct Background(Child);

impl Background {
    fn spawn(mut cmd: Command) -> Self {
        cmd.process_group(0).stdout(Stdio::null()).stderr(Stdio::null());
        Self(cmd.spawn().unwrap())
    }

    fn pid(&self) -> i32 {
        i32::try_from(self.0.id()).unwrap()
    }

    fn signal(&self, signal: i32) {
        // SAFETY: plain kill(2) on a pid this test spawned.
        unsafe { libc::kill(self.pid(), signal) };
    }

    fn signal_group(&self, signal: i32) {
        // SAFETY: plain kill(2) on the process group this test created.
        unsafe { libc::kill(-self.pid(), signal) };
    }

    fn wait(&mut self) -> ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "session did not exit");
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Background {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            self.signal_group(libc::SIGKILL);
            let _ = self.0.wait();
        }
    }
}

#[test]
fn session_passes_ports_and_identity_to_the_child() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let output = luchta(ws.path(), registry.path())
        .args(["session", "--name", "Feature_X", "--", "sh", "-c"])
        .arg(r#"test -n "$LUCHTA_SESSION_ID" && echo "$TEST_WEB_PORT $TEST_API_PORT $LUCHTA_SESSION_SLOT $LUCHTA_SESSION_NAME" > env.out"#)
        .output()
        .unwrap();

    let stderr = stderr_of(&output);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(wait_for_line(&ws.path().join("env.out")), "41081 41090 0 feature-x");
    assert!(stderr.contains("luchta session feature-x (slot 0)"), "{stderr}");
    assert!(stderr.contains("web  http://localhost:41081"), "{stderr}");
}

#[test]
fn quiet_suppresses_the_banner() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let output = luchta(ws.path(), registry.path())
        .args(["session", "--quiet", "--", "true"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!stderr_of(&output).contains("luchta session"));
}

#[test]
fn concurrent_worktrees_get_distinct_ports() {
    let (a, b) = (workspace_with_config(CONFIG), workspace_with_config(CONFIG));
    let registry = TempDir::new().unwrap();
    let mut first = Background::spawn(session_sh(a.path(), registry.path(), HOLD));
    assert_eq!(wait_for_line(&a.path().join("port.out")), "41081");

    let second = session_sh(b.path(), registry.path(), r#"echo "$TEST_WEB_PORT" > port.out"#)
        .output()
        .unwrap();
    assert!(second.status.success(), "{}", stderr_of(&second));
    assert_eq!(wait_for_line(&b.path().join("port.out")), "42081");

    fs::write(a.path().join("stop"), "").unwrap();
    assert!(first.wait().success());
}

#[test]
fn second_session_in_the_same_worktree_is_refused() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let mut first = Background::spawn(session_sh(ws.path(), registry.path(), HOLD));
    wait_for_line(&ws.path().join("port.out"));

    let second = session_sh(ws.path(), registry.path(), "touch second-ran").output().unwrap();
    let stderr = stderr_of(&second);
    assert!(!second.status.success());
    assert!(
        stderr.contains(&format!("already running for this worktree (pid {})", first.pid())),
        "{stderr}"
    );
    assert!(stderr.contains("http://localhost:41081"), "{stderr}");
    assert!(!ws.path().join("second-ran").exists());

    fs::write(ws.path().join("stop"), "").unwrap();
    assert!(first.wait().success());
}

#[test]
fn symlinked_workspace_path_counts_as_the_same_worktree() {
    let ws = workspace_with_config(CONFIG);
    let links = TempDir::new().unwrap();
    let link = links.path().join("link");
    std::os::unix::fs::symlink(ws.path(), &link).unwrap();
    let registry = TempDir::new().unwrap();
    let _first = Background::spawn(session_sh(ws.path(), registry.path(), HOLD));
    wait_for_line(&ws.path().join("port.out"));

    let second = session_sh(&link, registry.path(), "true").output().unwrap();
    assert!(!second.status.success());
    assert!(stderr_of(&second).contains("already running"), "{}", stderr_of(&second));
}

#[test]
fn child_exit_code_is_propagated() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let status = session_sh(ws.path(), registry.path(), "exit 7").status().unwrap();
    assert_eq!(status.code(), Some(7));
}

#[test]
fn sigterm_is_forwarded_to_the_child() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let script = r#"trap 'echo term > term.out; exit 3' TERM; echo ready > ready.out; while :; do sleep 0.05; done"#;
    let mut session = Background::spawn(session_sh(ws.path(), registry.path(), script));
    wait_for_line(&ws.path().join("ready.out"));

    session.signal(libc::SIGTERM);
    assert_eq!(session.wait().code(), Some(3));
    assert_eq!(wait_for_line(&ws.path().join("term.out")), "term");
}

#[test]
fn ctrl_c_to_the_process_group_lets_the_child_finish() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let script = r#"trap 'echo int > int.out; exit 0' INT; echo ready > ready.out; while :; do sleep 0.05; done"#;
    let mut session = Background::spawn(session_sh(ws.path(), registry.path(), script));
    wait_for_line(&ws.path().join("ready.out"));

    session.signal_group(libc::SIGINT);
    assert_eq!(session.wait().code(), Some(0));
    assert_eq!(wait_for_line(&ws.path().join("int.out")), "int");
}

#[test]
fn failed_spawn_releases_the_slot() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let failed = luchta(ws.path(), registry.path())
        .args(["session", "--", "/nonexistent/luchta-session-test-command"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(stderr_of(&failed).contains("failed to start"), "{}", stderr_of(&failed));

    let retry = session_sh(ws.path(), registry.path(), r#"echo "$LUCHTA_SESSION_SLOT" > slot.out"#)
        .output()
        .unwrap();
    assert!(retry.status.success(), "{}", stderr_of(&retry));
    assert_eq!(wait_for_line(&ws.path().join("slot.out")), "0");
}

#[test]
fn preset_port_variable_is_overridden_with_a_warning() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let output = session_sh(ws.path(), registry.path(), r#"echo "$TEST_WEB_PORT" > port.out"#)
        .env("TEST_WEB_PORT", "1234")
        .output()
        .unwrap();

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(wait_for_line(&ws.path().join("port.out")), "41081");
    assert!(
        stderr_of(&output).contains("overriding TEST_WEB_PORT=1234 with 41081"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn missing_sessions_block_is_reported() {
    let ws = workspace_with_config("{}");
    let registry = TempDir::new().unwrap();
    let output = session_sh(ws.path(), registry.path(), "touch ran").output().unwrap();
    assert!(!output.status.success());
    assert!(stderr_of(&output).contains("`sessions` block"), "{}", stderr_of(&output));
    assert!(!ws.path().join("ran").exists());
}

#[test]
fn invalid_sessions_block_names_the_key() {
    let ws = workspace_with_config(
        r#"{"sessions":{"slotStride":5,"ports":{"A":{"default":41081},"B":{"default":41090}}}}"#,
    );
    let registry = TempDir::new().unwrap();
    let output = session_sh(ws.path(), registry.path(), "true").output().unwrap();
    assert!(!output.status.success());
    assert!(stderr_of(&output).contains("sessions.slotStride"), "{}", stderr_of(&output));
}
```

Add the binary to the throttled group in `.config/nextest.toml` (these tests spawn real processes against a 10s deadline):

```toml
filter = 'binary_id(luchta-worker-watcher::e2e) + binary_id(luchta-cli::worker_integration) + binary_id(luchta-cli::session_e2e) + (package(luchta-cli) & test(/driver_e2e/))'
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-cli --test session_e2e`
Expected: every test FAILS — clap rejects the unknown `session` subcommand.

- [ ] **Step 3: Add the CLI variant and dispatch**

In `crates/luchta-cli/src/cli.rs`, add to `Commands` (after `Await`, following the existing doc-comment-as-help style):

```rust
    /// Run a long-lived command (e.g. dev servers) with ports allocated for
    /// this worktree, refusing to start if this worktree already has one.
    Session {
        /// Session name (defaults to the workspace directory name).
        #[arg(long)]
        name: Option<String>,
        /// Do not print the session banner.
        #[arg(long)]
        quiet: bool,
        /// Command to run, after `--`.
        #[arg(last = true, required = true, value_name = "COMMAND")]
        command: Vec<String>,
    },
```

In `crates/luchta-cli/src/main.rs`, add `mod session;` to the module list, and in `run`'s `match cli.command` add:

```rust
        Commands::Session { name, quiet, command } => {
            session::dispatch_session(&workspace_root, name, quiet, command).await
        }
```

- [ ] **Step 4: Implement `session.rs`**

Create `crates/luchta-cli/src/session.rs`:

```rust
//! `luchta session`: run a long-lived command with ports allocated for this
//! worktree, published in the machine-wide session registry.

use std::{fmt::Write as _, path::Path, process::ExitStatus};

use luchta_sessions::{
    allocate, unix_now, AllocError, PortPlan, Registry, SessionRecord, SessionRequest, TcpProbe,
};
use miette::{miette, IntoDiagnostic, Result, WrapErr};

pub async fn dispatch_session(
    workspace_root: &Path,
    name: Option<String>,
    quiet: bool,
    command: Vec<String>,
) -> Result<()> {
    let config = crate::config::load_config(workspace_root)
        .await
        .wrap_err_with(|| format!("Failed to load config at {}", workspace_root.display()))?;
    let sessions = config.sessions.as_ref().ok_or_else(missing_sessions_block)?;
    let plan = PortPlan::from_config(sessions).map_err(|error| miette!("{error}"))?;
    let registry = Registry::from_env().map_err(|error| miette!("{error}"))?;
    let request = SessionRequest {
        workspace_root,
        name: name.as_deref(),
        branch: current_branch(workspace_root),
        command: command.clone(),
        pid: std::process::id(),
    };
    let session = allocate(&registry, &plan, request, &TcpProbe).map_err(render_alloc_error)?;
    if !quiet {
        eprint!("{}", banner(session.record()));
    }
    let status = run_child(session.record(), &command).await?;
    // Release the slot before exiting: `process::exit` skips destructors.
    drop(session);
    std::process::exit(exit_code(status))
}

fn missing_sessions_block() -> miette::Report {
    miette!(
        help = r#"add for example: "sessions": {"ports": {"DEVSERVER_HTTP_PORT": {"default": 8081, "service": "web", "http": true}}}"#,
        "luchta session needs a `sessions` block in the luchta config"
    )
}

fn render_alloc_error(error: AllocError) -> miette::Report {
    match error {
        AllocError::AlreadyRunning(existing) => miette!(
            help = "stop that session first, or run `luchta sessions` to see every session",
            // Keep the pid on the first line: miette wraps long messages at spaces.
            "a session is already running for this worktree (pid {}): {} in slot {}, started {} ago\n{}",
            existing.pid,
            existing.name,
            existing.slot,
            format_age(existing.started_at),
            url_lines(&existing, "  ")
        ),
        AllocError::NoFreeSlot { live } => {
            let listing: String = live
                .iter()
                .map(|record| format!("  {} (slot {}, pid {})\n", record.name, record.slot, record.pid))
                .collect();
            miette!(
                help = "stop one of these sessions or raise sessions.maxSlots",
                "no free session slot; live sessions:\n{listing}"
            )
        }
        other => miette!("{other}"),
    }
}

fn banner(record: &SessionRecord) -> String {
    format!(
        "luchta session {} (slot {})\n{}",
        record.name,
        record.slot,
        url_lines(record, "  ")
    )
}

/// One `service  http://localhost:<port>` line per HTTP port.
pub(crate) fn url_lines(record: &SessionRecord, indent: &str) -> String {
    let urls: Vec<(&str, u16)> = record
        .ports
        .iter()
        .filter(|port| port.http)
        .map(|port| (port.service.as_deref().unwrap_or(port.env.as_str()), port.port))
        .collect();
    let width = urls.iter().map(|(label, _)| label.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (label, port) in urls {
        let _ = writeln!(out, "{indent}{label:<width$}  http://localhost:{port}");
    }
    out
}

/// Compact age such as `45s`, `12m`, `3h`, `2d`.
pub(crate) fn format_age(started_at: u64) -> String {
    let secs = unix_now().saturating_sub(started_at);
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

fn current_branch(workspace_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let branch = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (output.status.success() && !branch.is_empty() && branch != "HEAD").then_some(branch)
}

fn child_env(record: &SessionRecord) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = record
        .ports
        .iter()
        .map(|port| (port.env.clone(), port.port.to_string()))
        .collect();
    env.push(("LUCHTA_SESSION_NAME".to_string(), record.name.clone()));
    env.push(("LUCHTA_SESSION_SLOT".to_string(), record.slot.to_string()));
    env.push(("LUCHTA_SESSION_ID".to_string(), record.id.clone()));
    env
}

fn warn_about_overrides(env: &[(String, String)]) {
    for (key, value) in env {
        if let Ok(existing) = std::env::var(key) {
            if existing != *value {
                eprintln!("luchta session: overriding {key}={existing} with {value}");
            }
        }
    }
}

async fn run_child(record: &SessionRecord, command: &[String]) -> Result<ExitStatus> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| miette!("luchta session needs a command after `--`"))?;
    let env = child_env(record);
    warn_about_overrides(&env);
    // Install handlers before spawning so no signal slips through the gap.
    let mut signals = Signals::install()?;
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .envs(env)
        .spawn()
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to start `{program}`"))?;
    signals.wait_forwarding(&mut child).await
}

#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl Signals {
    fn install() -> Result<Self> {
        use tokio::signal::unix::{signal, SignalKind};
        let install = |kind: SignalKind, name: &str| {
            signal(kind)
                .into_diagnostic()
                .wrap_err_with(|| format!("failed to install {name} handler"))
        };
        Ok(Self {
            interrupt: install(SignalKind::interrupt(), "SIGINT")?,
            terminate: install(SignalKind::terminate(), "SIGTERM")?,
            hangup: install(SignalKind::hangup(), "SIGHUP")?,
        })
    }

    async fn wait_forwarding(&mut self, child: &mut tokio::process::Child) -> Result<ExitStatus> {
        loop {
            tokio::select! {
                status = child.wait() => return status.into_diagnostic(),
                // The terminal already delivers SIGINT to the child's process
                // group; keep waiting so the child can shut down on its own.
                _ = self.interrupt.recv() => {}
                _ = self.terminate.recv() => forward(child, libc::SIGTERM),
                _ = self.hangup.recv() => forward(child, libc::SIGHUP),
            }
        }
    }
}

#[cfg(unix)]
fn forward(child: &tokio::process::Child, signal: libc::c_int) {
    if let Some(pid) = child.id().and_then(|pid| libc::pid_t::try_from(pid).ok()) {
        // SAFETY: kill(2) on our own child's pid; failure (already exited) is harmless.
        unsafe { libc::kill(pid, signal) };
    }
}

#[cfg(not(unix))]
struct Signals;

#[cfg(not(unix))]
impl Signals {
    fn install() -> Result<Self> {
        Ok(Self)
    }

    async fn wait_forwarding(&mut self, child: &mut tokio::process::Child) -> Result<ExitStatus> {
        loop {
            tokio::select! {
                status = child.wait() => return status.into_diagnostic(),
                // Console Ctrl-C reaches the whole console group; just wait.
                _ = tokio::signal::ctrl_c() => {}
            }
        }
    }
}

fn exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use luchta_sessions::ResolvedPort;

    fn record() -> SessionRecord {
        SessionRecord {
            slot: 1,
            id: "1-9-abc".to_string(),
            name: "feature-x".to_string(),
            pid: 9,
            workspace_root: "/ws".into(),
            branch: None,
            command: vec![],
            started_at: 0,
            ports: vec![
                ResolvedPort { env: "WEB".into(), port: 9081, service: Some("web".into()), http: true, default_service: false },
                ResolvedPort { env: "AUTH_PORT".into(), port: 9011, service: None, http: true, default_service: false },
                ResolvedPort { env: "METRICS".into(), port: 9500, service: None, http: false, default_service: false },
            ],
            paused_at: None,
        }
    }

    #[test]
    fn url_lines_cover_http_ports_aligned_by_label() {
        assert_eq!(
            url_lines(&record(), "  "),
            "  web        http://localhost:9081\n  AUTH_PORT  http://localhost:9011\n"
        );
    }

    #[test]
    fn child_env_has_ports_and_identity() {
        let env = child_env(&record());
        assert!(env.contains(&("WEB".to_string(), "9081".to_string())));
        assert!(env.contains(&("METRICS".to_string(), "9500".to_string())));
        assert!(env.contains(&("LUCHTA_SESSION_NAME".to_string(), "feature-x".to_string())));
        assert!(env.contains(&("LUCHTA_SESSION_SLOT".to_string(), "1".to_string())));
        assert!(env.contains(&("LUCHTA_SESSION_ID".to_string(), "1-9-abc".to_string())));
    }

    #[test]
    fn ages_are_compact() {
        let now = unix_now();
        assert_eq!(format_age(now), "0s");
        assert_eq!(format_age(now - 125), "2m");
        assert_eq!(format_age(now - 7_300), "2h");
        assert_eq!(format_age(now - 200_000), "2d");
    }
}
```

Note for the implementer: if `cargo fmt` reformats the long `ResolvedPort` literals, accept its output.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-cli --test session_e2e && cargo nextest run -p luchta-cli session::`
Expected: all PASS.

- [ ] **Step 6: Checkpoint**

Run: `cargo clippy --workspace --all-targets -- -D warnings && git status`. Do not commit.

---

### Task 6: `luchta sessions` listing

**Files:**
- Modify: `crates/luchta-cli/src/cli.rs`
- Modify: `crates/luchta-cli/src/main.rs`
- Create: `crates/luchta-cli/src/sessions_cmd.rs`
- Modify: `crates/luchta-cli/tests/session_e2e.rs` (append tests)

**Interfaces:**
- Consumes: `Registry::from_env`, `Registry::live_sessions`, `LiveSession`, `SessionRecord` (Task 3); `crate::session::{url_lines, format_age}` (Task 5).
- Produces: `Commands::Sessions { json: bool }`; `sessions_cmd::dispatch_sessions(&Path, bool) -> miette::Result<()>`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/luchta-cli/tests/session_e2e.rs`:

```rust
#[test]
fn sessions_json_lists_live_sessions_and_drops_killed_ones() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let mut first = Background::spawn(session_sh(ws.path(), registry.path(), HOLD));
    wait_for_line(&ws.path().join("port.out"));

    let listed = luchta(ws.path(), registry.path()).args(["sessions", "--json"]).output().unwrap();
    assert!(listed.status.success(), "{}", stderr_of(&listed));
    let records: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["ports"][0]["port"], 41081);
    assert_eq!(records[0]["pid"], first.0.id());
    assert_eq!(
        records[0]["workspace_root"],
        fs::canonicalize(ws.path()).unwrap().to_str().unwrap()
    );

    first.signal_group(libc::SIGKILL);
    first.wait();
    let after = luchta(ws.path(), registry.path()).args(["sessions", "--json"]).output().unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&after.stdout).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn sessions_table_marks_the_current_worktree() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let mut cmd = luchta(ws.path(), registry.path());
    cmd.args(["session", "--name", "alpha", "--", "sh", "-c", HOLD]);
    let _first = Background::spawn(cmd);
    wait_for_line(&ws.path().join("port.out"));

    let listed = luchta(ws.path(), registry.path()).arg("sessions").output().unwrap();
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success(), "{}", stderr_of(&listed));
    assert!(stdout.contains("* alpha  slot 0"), "{stdout}");
    assert!(stdout.contains("web  http://localhost:41081"), "{stdout}");
}

#[test]
fn sessions_without_live_sessions_says_so() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let listed = luchta(ws.path(), registry.path()).arg("sessions").output().unwrap();
    assert!(listed.status.success());
    assert_eq!(String::from_utf8_lossy(&listed.stdout), "no live sessions\n");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p luchta-cli --test session_e2e sessions_`
Expected: the three new tests FAIL — unknown subcommand `sessions`.

- [ ] **Step 3: Add the variant and dispatch**

In `cli.rs` `Commands`, after `Session`:

```rust
    /// List live `luchta session` sessions and their URLs.
    Sessions {
        /// Print the session records as JSON.
        #[arg(long)]
        json: bool,
    },
```

In `main.rs`: add `mod sessions_cmd;` and the arm:

```rust
        Commands::Sessions { json } => sessions_cmd::dispatch_sessions(&workspace_root, json),
```

- [ ] **Step 4: Implement `sessions_cmd.rs`**

Create `crates/luchta-cli/src/sessions_cmd.rs`:

```rust
//! `luchta sessions`: list live sessions from the machine-wide registry.

use std::{fmt::Write as _, path::Path};

use luchta_sessions::{LiveSession, Registry, SessionRecord};
use miette::{miette, IntoDiagnostic, Result};

use crate::session::{format_age, url_lines};

pub fn dispatch_sessions(workspace_root: &Path, json: bool) -> Result<()> {
    let registry = Registry::from_env().map_err(|error| miette!("{error}"))?;
    let live = registry.live_sessions().map_err(|error| miette!("{error}"))?;
    if json {
        let records: Vec<&SessionRecord> = live.iter().filter_map(|s| s.record.as_ref()).collect();
        println!("{}", serde_json::to_string_pretty(&records).into_diagnostic()?);
        return Ok(());
    }
    let current_root = std::fs::canonicalize(workspace_root).ok();
    print!("{}", render_table(&live, current_root.as_deref()));
    Ok(())
}

fn render_table(live: &[LiveSession], current_root: Option<&Path>) -> String {
    if live.is_empty() {
        return "no live sessions\n".to_string();
    }
    let mut out = String::new();
    for session in live {
        let Some(record) = &session.record else {
            let _ = writeln!(out, "  <unreadable>  slot {}", session.slot);
            continue;
        };
        let marker = if Some(record.workspace_root.as_path()) == current_root { '*' } else { ' ' };
        let _ = writeln!(
            out,
            "{marker} {}  slot {}  {}  pid {}  up {}",
            record.name,
            record.slot,
            record.branch.as_deref().unwrap_or("-"),
            record.pid,
            format_age(record.started_at)
        );
        out.push_str(&url_lines(record, "    "));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use luchta_sessions::{unix_now, ResolvedPort};

    fn record(slot: u32, name: &str, root: &str) -> SessionRecord {
        SessionRecord {
            slot,
            id: format!("{slot}-1-0"),
            name: name.to_string(),
            pid: 100 + slot,
            workspace_root: root.into(),
            branch: (slot == 0).then(|| "main".to_string()),
            command: vec![],
            started_at: unix_now(),
            ports: vec![ResolvedPort {
                env: "WEB".into(),
                port: 8081 + 1000 * u16::try_from(slot).unwrap(),
                service: Some("web".into()),
                http: true,
                default_service: false,
            }],
            paused_at: None,
        }
    }

    #[test]
    fn marks_the_current_worktree_and_lists_urls() {
        let live = vec![
            LiveSession { slot: 0, record: Some(record(0, "app", "/ws/app")) },
            LiveSession { slot: 1, record: Some(record(1, "feature-x", "/ws/feature-x")) },
            LiveSession { slot: 2, record: None },
        ];
        assert_eq!(
            render_table(&live, Some(Path::new("/ws/feature-x"))),
            "  app  slot 0  main  pid 100  up 0s\n    web  http://localhost:8081\n\
             * feature-x  slot 1  -  pid 101  up 0s\n    web  http://localhost:9081\n\
             \x20 <unreadable>  slot 2\n"
        );
    }

    #[test]
    fn says_when_nothing_is_live() {
        assert_eq!(render_table(&[], None), "no live sessions\n");
    }
}
```

Note: in the expected string, `\x20 <unreadable>` is two spaces then `<unreadable>` — the `\` line continuation strips leading whitespace, so the first space is written as `\x20`.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo nextest run -p luchta-cli --test session_e2e && cargo nextest run -p luchta-cli sessions_cmd::`
Expected: all PASS.

- [ ] **Step 6: Checkpoint**

Run: `cargo clippy --workspace --all-targets -- -D warnings && git status`. Do not commit.

---

### Task 7: Documentation and changeset

**Files:**
- Modify: `README.md` (`## Crate Layout` near line 86; new `### Sessions` section before `### Build Lock` near line 1444)
- Create: `.changeset/add-luchta-sessions.md`

- [ ] **Step 1: Crate layout entry**

In `README.md` `## Crate Layout`, add a line in the same format as its neighbours:

```markdown
- `crates/luchta-sessions`: Per-worktree dev sessions — port plans, the machine-wide session registry, and slot allocation behind `luchta session` / `luchta sessions`.
```

- [ ] **Step 2: Usage section**

Insert before `### Build Lock`:

````markdown
### Sessions

`luchta session` runs a long-lived command — typically your dev servers — with
its own set of ports, so several worktrees of the same app can run side by
side. Declare the ports your app reads from environment variables:

```jsonc
{
  "sessions": {
    "slotStride": 1000, // optional, default 1000
    "maxSlots": 20,     // optional, default 20
    "ports": {
      "DEVSERVER_HTTP_PORT": { "default": 8081, "service": "web", "http": true },
      "AUTH_DEV_HTTP_PORT":  { "default": 8011, "service": "auth", "http": true },
      "REPORT_SERVER_METRICS_PORT": { "default": 9464 }
    }
  }
}
```

Then wrap the command that starts your servers:

```sh
luchta session -- overmind s
```

Each concurrent session takes a slot; slot N sets every declared variable to
`default + N * slotStride` (slot 0 uses the defaults unchanged), plus
`LUCHTA_SESSION_NAME`, `LUCHTA_SESSION_SLOT`, and `LUCHTA_SESSION_ID`. A
worktree gets its previous slot back when it is free. Slots whose ports are
already in use by anything are skipped.

- Starting a second session in the same worktree is refused and names the
  running one (pid, age, URLs).
- `luchta sessions` lists live sessions and their URLs; `--json` prints the
  records.
- `--name <name>` overrides the session name (default: the workspace
  directory name). `--quiet` hides the startup banner.
- The wrapper exits with the command's exit code, forwards SIGTERM/SIGHUP, and
  lets Ctrl-C reach the command directly.

Declare **every** port your servers bind, including metrics ports: tools such
as overmind set `PORT` identically in every worktree, so any fallback to it
collides. Session records live in `$XDG_RUNTIME_DIR/luchta/sessions` (or the
user cache directory); set `LUCHTA_SESSIONS_DIR` to override.
````

- [ ] **Step 3: Changeset**

Create `.changeset/add-luchta-sessions.md`:

```markdown
---
"luchta": minor
---
# Per-worktree dev sessions

Add `luchta session -- <command>`, which gives each concurrent worktree its own
set of ports from a new `sessions` config block and refuses to start a second
session in the same worktree, and `luchta sessions` to list live sessions and
their URLs.
```

- [ ] **Step 4: Checkpoint**

Run: `git status`. Do not commit.

---

### Task 8: Full verification

- [ ] **Step 1: Run the AGENTS.md pipeline, in order, fixing anything it reports**

```bash
cargo build --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace
cargo nextest run --workspace --stress-count=5
cs delta origin/HEAD
cargo xtask install
cargo xtask build-worker --target "$(rustc -vV | sed -n 's/^host: //p')"
```

Expected: every command succeeds; `cs delta` reports no new code-health problems. If `cs delta` flags a function (likely candidates: `allocate`, `validate_names`, `render_alloc_error`), split it and re-run the pipeline from the top.

- [ ] **Step 2: Manual smoke test**

In two terminals, in two different directories that each hold a `luchta-config.sh` with the CONFIG from Task 5:

```bash
luchta session -- sh -c 'echo $TEST_WEB_PORT; sleep 600'   # terminal 1 → 41081
luchta session -- sh -c 'echo $TEST_WEB_PORT; sleep 600'   # terminal 2 → 42081
luchta sessions                                            # both listed, current one starred
```

Re-run the terminal-1 command in terminal 1's directory from a third terminal → refused with terminal 1's pid.

- [ ] **Step 3: Checkpoint**

Run `git status` and report the changed files to the user. Do not commit.

---

## After this plan (not part of it)

- **Formative adoption** (separate repo): add a `sessions.ports` block to `luchta-config.mts` covering every bound port (including `*_METRICS_PORT`), and change `"start"` to `luchta session -- overmind s`.
- **Phase 2 (proxy)** and **phase 3 (pause/resume)** get their own plans from the same spec.
