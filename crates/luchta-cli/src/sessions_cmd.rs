//! `luchta sessions`: list live sessions from the machine-wide registry.

use std::{fmt::Write as _, path::Path};

use luchta_sessions::{LiveSession, Registry, SessionRecord};
use miette::{miette, IntoDiagnostic, Result};

use crate::session::{format_age, url_lines};

pub fn dispatch_sessions(workspace_root: &Path, json: bool) -> Result<()> {
    let registry = Registry::from_env().map_err(|error| miette!("{error}"))?;
    let live = registry
        .live_sessions()
        .map_err(|error| miette!("{error}"))?;
    if json {
        let records: Vec<&SessionRecord> = live.iter().filter_map(|s| s.record.as_ref()).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&records).into_diagnostic()?
        );
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
        let marker = if Some(record.workspace_root.as_path()) == current_root {
            '*'
        } else {
            ' '
        };
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
            LiveSession {
                slot: 0,
                record: Some(record(0, "app", "/ws/app")),
            },
            LiveSession {
                slot: 1,
                record: Some(record(1, "feature-x", "/ws/feature-x")),
            },
            LiveSession {
                slot: 2,
                record: None,
            },
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
