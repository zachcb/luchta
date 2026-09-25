//! `luchta sessions`: list live sessions from the machine-wide registry.

use std::{fmt::Write as _, path::Path};

use luchta_sessions::{LiveSession, Registry, SessionRecord};
use miette::{IntoDiagnostic, Result};

use crate::session::{format_age, url_lines};

pub fn dispatch_sessions(workspace_root: &Path, json: bool) -> Result<()> {
    let registry = Registry::from_env().into_diagnostic()?;
    let live = registry.live_sessions().into_diagnostic()?;
    if json {
        for warning in unreadable_slot_warnings(&live) {
            eprintln!("{warning}");
        }
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

/// Stderr warning for each live slot whose record could not be read, so a
/// `--json` consumer parsing stdout still learns a slot is being silently
/// dropped from the listing.
fn unreadable_slot_warnings(live: &[LiveSession]) -> Vec<String> {
    live.iter()
        .filter(|session| session.record.is_none())
        .map(|session| {
            format!(
                "luchta sessions: slot {} is held but its record is unreadable",
                session.slot
            )
        })
        .collect()
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

    #[test]
    fn unreadable_slot_warnings_names_each_slot_missing_a_record() {
        let live = vec![
            LiveSession {
                slot: 0,
                record: Some(record(0, "app", "/ws/app")),
            },
            LiveSession {
                slot: 2,
                record: None,
            },
            LiveSession {
                slot: 5,
                record: None,
            },
        ];
        assert_eq!(
            unreadable_slot_warnings(&live),
            vec![
                "luchta sessions: slot 2 is held but its record is unreadable".to_string(),
                "luchta sessions: slot 5 is held but its record is unreadable".to_string(),
            ]
        );
    }

    #[test]
    fn unreadable_slot_warnings_is_empty_when_every_record_reads() {
        let live = vec![LiveSession {
            slot: 0,
            record: Some(record(0, "app", "/ws/app")),
        }];
        assert!(unreadable_slot_warnings(&live).is_empty());
    }
}
