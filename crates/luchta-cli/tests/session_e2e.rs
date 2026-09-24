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

const CONFIG: &str = r#"{"sessions":{"ports":{"TEST_WEB_PORT":{"default":21081,"service":"web","http":true},"TEST_API_PORT":{"default":21090}}}}"#;
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
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// A session running in the background in its own process group; the whole
/// group is killed if the test ends while it is still running.
struct Background(Child);

impl Background {
    fn spawn(mut cmd: Command) -> Self {
        cmd.process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
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
    assert_eq!(
        wait_for_line(&ws.path().join("env.out")),
        "21081 21090 0 feature-x"
    );
    assert!(
        stderr.contains("luchta session feature-x (slot 0)"),
        "{stderr}"
    );
    assert!(stderr.contains("web  http://localhost:21081"), "{stderr}");
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
    assert_eq!(wait_for_line(&a.path().join("port.out")), "21081");

    let second = session_sh(
        b.path(),
        registry.path(),
        r#"echo "$TEST_WEB_PORT" > port.out"#,
    )
    .output()
    .unwrap();
    assert!(second.status.success(), "{}", stderr_of(&second));
    assert_eq!(wait_for_line(&b.path().join("port.out")), "22081");

    fs::write(a.path().join("stop"), "").unwrap();
    assert!(first.wait().success());
}

#[test]
fn second_session_in_the_same_worktree_is_refused() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let mut first = Background::spawn(session_sh(ws.path(), registry.path(), HOLD));
    wait_for_line(&ws.path().join("port.out"));

    let second = session_sh(ws.path(), registry.path(), "touch second-ran")
        .output()
        .unwrap();
    let stderr = stderr_of(&second);
    assert!(!second.status.success());
    assert!(
        stderr.contains(&format!(
            "already running for this worktree (pid {})",
            first.pid()
        )),
        "{stderr}"
    );
    assert!(stderr.contains("http://localhost:21081"), "{stderr}");
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
    assert!(
        stderr_of(&second).contains("already running"),
        "{}",
        stderr_of(&second)
    );
}

#[test]
fn child_exit_code_is_propagated() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let status = session_sh(ws.path(), registry.path(), "exit 7")
        .status()
        .unwrap();
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
    assert!(
        stderr_of(&failed).contains("failed to start"),
        "{}",
        stderr_of(&failed)
    );

    let retry = session_sh(
        ws.path(),
        registry.path(),
        r#"echo "$LUCHTA_SESSION_SLOT" > slot.out"#,
    )
    .output()
    .unwrap();
    assert!(retry.status.success(), "{}", stderr_of(&retry));
    assert_eq!(wait_for_line(&ws.path().join("slot.out")), "0");
}

#[test]
fn preset_port_variable_is_overridden_with_a_warning() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let output = session_sh(
        ws.path(),
        registry.path(),
        r#"echo "$TEST_WEB_PORT" > port.out"#,
    )
    .env("TEST_WEB_PORT", "1234")
    .output()
    .unwrap();

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert_eq!(wait_for_line(&ws.path().join("port.out")), "21081");
    assert!(
        stderr_of(&output).contains("overriding TEST_WEB_PORT=1234 with 21081"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn missing_sessions_block_is_reported() {
    let ws = workspace_with_config("{}");
    let registry = TempDir::new().unwrap();
    let output = session_sh(ws.path(), registry.path(), "touch ran")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("`sessions` block"),
        "{}",
        stderr_of(&output)
    );
    assert!(!ws.path().join("ran").exists());
}

#[test]
fn invalid_sessions_block_names_the_key() {
    let ws = workspace_with_config(
        r#"{"sessions":{"slotStride":5,"ports":{"A":{"default":21081},"B":{"default":21090}}}}"#,
    );
    let registry = TempDir::new().unwrap();
    let output = session_sh(ws.path(), registry.path(), "true")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("sessions.slotStride"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn sessions_json_lists_live_sessions_and_drops_killed_ones() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let mut first = Background::spawn(session_sh(ws.path(), registry.path(), HOLD));
    wait_for_line(&ws.path().join("port.out"));

    let listed = luchta(ws.path(), registry.path())
        .args(["sessions", "--json"])
        .output()
        .unwrap();
    assert!(listed.status.success(), "{}", stderr_of(&listed));
    let records: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["ports"][0]["port"], 21081);
    assert_eq!(records[0]["pid"], first.0.id());
    assert_eq!(
        records[0]["workspace_root"],
        fs::canonicalize(ws.path()).unwrap().to_str().unwrap()
    );

    first.signal_group(libc::SIGKILL);
    first.wait();
    let after = luchta(ws.path(), registry.path())
        .args(["sessions", "--json"])
        .output()
        .unwrap();
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

    let listed = luchta(ws.path(), registry.path())
        .arg("sessions")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&listed.stdout);
    assert!(listed.status.success(), "{}", stderr_of(&listed));
    assert!(stdout.contains("* alpha  slot 0"), "{stdout}");
    assert!(stdout.contains("web  http://localhost:21081"), "{stdout}");
}

#[test]
fn sessions_without_live_sessions_says_so() {
    let ws = workspace_with_config(CONFIG);
    let registry = TempDir::new().unwrap();
    let listed = luchta(ws.path(), registry.path())
        .arg("sessions")
        .output()
        .unwrap();
    assert!(listed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&listed.stdout),
        "no live sessions\n"
    );
}
