//! `luchta session`: run a long-lived command with ports allocated for this
//! worktree, published in the machine-wide session registry.

use std::{fmt::Write as _, path::Path, process::ExitStatus};

use luchta_sessions::{
    allocate, unix_now, AllocError, PortPlan, Registry, Session, SessionRecord, SessionRequest,
    TcpProbe,
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
    let sessions = config
        .sessions
        .as_ref()
        .ok_or_else(missing_sessions_block)?;
    let plan = PortPlan::from_config(sessions).into_diagnostic()?;
    let registry = Registry::from_env().into_diagnostic()?;
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
    let status = run_in_session(session, &command).await?;
    std::process::exit(exit_code(status))
}

/// Runs `command` inside `session`, releasing its slot before returning on
/// both the success and error paths. `process::exit` skips destructors, so
/// the caller must not exit while still holding `session`; going through an
/// owned `Session` here (rather than exiting inside this function) makes that
/// impossible to get wrong, and lets this be tested in-process without
/// spawning a real `luchta` binary.
async fn run_in_session(session: Session, command: &[String]) -> Result<ExitStatus> {
    let result = run_child(session.record(), command).await;
    drop(session);
    result
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
        other => miette::Report::from_err(other),
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
        .map(|port| {
            (
                port.service.as_deref().unwrap_or(port.env.as_str()),
                port.port,
            )
        })
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

/// Warning lines for declared port vars whose current value (as `lookup`
/// reports it) differs from what this session will set. Only `record.ports`
/// is considered — never `LUCHTA_SESSION_*` — so a session started inside
/// another session doesn't warn about identity vars the outer session set.
fn overridden_ports(
    record: &SessionRecord,
    lookup: impl Fn(&str) -> Option<String>,
) -> Vec<String> {
    record
        .ports
        .iter()
        .filter_map(|port| {
            let existing = lookup(&port.env)?;
            let value = port.port.to_string();
            (existing != value).then(|| {
                format!(
                    "luchta session: overriding {}={existing} with {value}",
                    port.env
                )
            })
        })
        .collect()
}

fn warn_about_overrides(record: &SessionRecord) {
    for line in overridden_ports(record, |key| std::env::var(key).ok()) {
        eprintln!("{line}");
    }
}

async fn run_child(record: &SessionRecord, command: &[String]) -> Result<ExitStatus> {
    let (program, args) = command
        .split_first()
        .ok_or_else(|| miette!("luchta session needs a command after `--`"))?;
    let env = child_env(record);
    warn_about_overrides(record);
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
    quit: tokio::signal::unix::Signal,
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
            quit: install(SignalKind::quit(), "SIGQUIT")?,
            terminate: install(SignalKind::terminate(), "SIGTERM")?,
            hangup: install(SignalKind::hangup(), "SIGHUP")?,
        })
    }

    async fn wait_forwarding(&mut self, child: &mut tokio::process::Child) -> Result<ExitStatus> {
        loop {
            tokio::select! {
                status = child.wait() => return status.into_diagnostic(),
                // The terminal already delivers SIGINT and SIGQUIT to the
                // child's process group (Ctrl-C / Ctrl-\); keep waiting so the
                // child can shut down on its own instead of us also dying.
                _ = self.interrupt.recv() => {}
                _ = self.quit.recv() => {}
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
    use luchta_sessions::{PortPlan, PortProbe, Registry, ResolvedPort};
    use luchta_types::SessionsConfig;
    use tempfile::TempDir;

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
                ResolvedPort {
                    env: "WEB".into(),
                    port: 9081,
                    service: Some("web".into()),
                    http: true,
                    default_service: false,
                },
                ResolvedPort {
                    env: "AUTH_PORT".into(),
                    port: 9011,
                    service: None,
                    http: true,
                    default_service: false,
                },
                ResolvedPort {
                    env: "METRICS".into(),
                    port: 9500,
                    service: None,
                    http: false,
                    default_service: false,
                },
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

    #[test]
    fn overridden_ports_warns_only_for_declared_port_vars() {
        let lookup = |key: &str| match key {
            "WEB" => Some("1234".to_string()),
            // A session-inside-a-session would have these preset by the
            // outer session; they must never trigger a warning.
            "LUCHTA_SESSION_NAME" => Some("outer".to_string()),
            "LUCHTA_SESSION_SLOT" => Some("9".to_string()),
            "LUCHTA_SESSION_ID" => Some("9-1-abc".to_string()),
            _ => None,
        };
        assert_eq!(
            overridden_ports(&record(), lookup),
            vec!["luchta session: overriding WEB=1234 with 9081".to_string()]
        );
    }

    #[test]
    fn overridden_ports_is_silent_when_values_match_or_are_unset() {
        let lookup = |key: &str| match key {
            "AUTH_PORT" => Some("9011".to_string()),
            _ => None,
        };
        assert!(overridden_ports(&record(), lookup).is_empty());
    }

    #[tokio::test]
    async fn run_in_session_releases_the_slot_in_process_when_spawn_fails() {
        let temp = TempDir::new().unwrap();
        let registry = Registry::new(temp.path().join("registry"));
        let workspace = temp.path().join("ws");
        std::fs::create_dir_all(&workspace).unwrap();
        let config: SessionsConfig =
            serde_json::from_str(r#"{"ports":{"WEB":{"default":23081}}}"#).unwrap();
        let plan = PortPlan::from_config(&config).unwrap();
        let request = SessionRequest {
            workspace_root: &workspace,
            name: None,
            branch: None,
            command: vec!["/nonexistent/luchta-followup-cmd".to_string()],
            pid: std::process::id(),
        };
        // A fake, always-free probe: this test only cares that spawn failure
        // releases the slot, not port availability, and a real `TcpProbe`
        // would make the test's outcome depend on whether some unrelated
        // process on the host happens to be bound to port 23081.
        struct AlwaysFree;
        impl PortProbe for AlwaysFree {
            fn is_free(&self, _port: u16) -> bool {
                true
            }
        }
        let session = allocate(&registry, &plan, request, &AlwaysFree).unwrap();

        let result =
            run_in_session(session, &["/nonexistent/luchta-followup-cmd".to_string()]).await;

        assert!(result.is_err());
        // If the slot lock were only released at process exit, this would
        // still see it held, since we are in the same process.
        assert!(registry.live_sessions().unwrap().is_empty());
    }

    #[test]
    fn render_alloc_error_keeps_the_source_error_for_unmatched_variants() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "no such workspace");
        let error = AllocError::WorkspaceRoot {
            path: "/missing".into(),
            source,
        };
        let report = render_alloc_error(error);
        let source = std::error::Error::source(&*report).expect("source error preserved");
        assert_eq!(source.to_string(), "no such workspace");
    }
}
