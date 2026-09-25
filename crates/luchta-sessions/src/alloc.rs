//! Choosing a slot for a new session.

use std::{
    fs, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    dedupe_name, sanitize_label, unix_now, PortPlan, Registry, RegistryError, ResolvedPort,
    SessionEnvVar, SessionRecord, SlotLock,
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
    // No `{source}` here: it is preserved via `#[source]` and rendered by
    // callers that show the error chain (e.g. miette's `{:?}`); repeating it
    // in Display would print the cause twice.
    #[error("failed to resolve workspace root {}", .path.display())]
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
        let path = self.registry.record_path(self.record.slot);
        if let Err(error) = fs::remove_file(&path) {
            // Best-effort and must never panic from a destructor, but a
            // failure here (other than the record already being gone) means
            // a stale record may linger and confuse the next `luchta
            // sessions` listing; `luchta-sessions` is a library crate with no
            // diagnostics channel of its own, so stderr is the only way to
            // surface this.
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "luchta session: could not remove session record {}: {error}",
                    path.display()
                );
            }
        }
    }
}

pub fn allocate(
    registry: &Registry,
    plan: &PortPlan,
    request: SessionRequest<'_>,
    probe: &dyn PortProbe,
) -> Result<Session, AllocError> {
    let workspace_root =
        fs::canonicalize(request.workspace_root).map_err(|source| AllocError::WorkspaceRoot {
            path: request.workspace_root.to_path_buf(),
            source,
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
    let name = session_name(request.name, &workspace_root, &records);
    let env = resolved_env(plan, &ports, &name, lock.slot());
    let record = SessionRecord {
        slot: lock.slot(),
        id: session_id(lock.slot(), request.pid),
        name,
        pid: request.pid,
        workspace_root,
        branch: request.branch,
        command: request.command,
        started_at: unix_now(),
        ports,
        env,
        paused_at: None,
    };
    registry.write_record(&record)?;
    // Sticky slots are a convenience; failing to remember one must not fail
    // the session.
    let _ = registry.remember_slot(&record.workspace_root, record.slot);
    Ok(Session {
        registry: registry.clone(),
        record,
        _lock: lock,
    })
}

fn resolved_env(
    plan: &PortPlan,
    ports: &[ResolvedPort],
    name: &str,
    slot: u32,
) -> Vec<SessionEnvVar> {
    plan.env_for_slot(ports, name, slot)
        .into_iter()
        .map(|(name, value)| SessionEnvVar { name, value })
        .collect()
}

fn claim_slot(
    registry: &Registry,
    plan: &PortPlan,
    preferred: Option<u32>,
    probe: &dyn PortProbe,
) -> Result<Option<(SlotLock, Vec<ResolvedPort>)>, RegistryError> {
    let preferred = preferred.filter(|slot| *slot < plan.max_slots());
    let rest = (0..plan.max_slots()).filter(move |slot| Some(*slot) != preferred);
    for slot in preferred.into_iter().chain(rest) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LiveSession, SessionEnvVar};
    use luchta_types::SessionsConfig;
    use std::{
        cell::RefCell,
        collections::{BTreeMap, BTreeSet},
        fs,
        net::TcpListener,
        sync::{Arc, Barrier},
        thread,
    };
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
    fn the_allocated_record_carries_resolved_env_for_its_slot_and_name() {
        let fx = Fixture::new();
        let (a, b) = (fx.workspace("a"), fx.workspace("b"));
        let plan = plan_from(
            r#"{"maxSlots":3,"ports":{
                "WEB":{"default":41081},
                "API":{"default":41090}
            },"env":{
                "API_ROOT_URL":"http://localhost:${API}",
                "SESSION_LABEL":"${LUCHTA_SESSION_NAME}-${LUCHTA_SESSION_SLOT}"
            }}"#,
        );
        let _first = allocate(&fx.registry, &plan, request(&a), &all_free()).unwrap();
        let second = allocate(&fx.registry, &plan, request(&b), &all_free()).unwrap();

        assert_eq!(second.record().slot, 1);
        assert_eq!(
            second.record().env,
            vec![
                SessionEnvVar {
                    name: "API_ROOT_URL".to_string(),
                    value: "http://localhost:42090".to_string(),
                },
                SessionEnvVar {
                    name: "SESSION_LABEL".to_string(),
                    value: "b-1".to_string(),
                },
            ]
        );
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
            vec![LiveSession {
                slot: 0,
                record: Some(record.clone())
            }]
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
    fn the_preferred_slots_port_is_probed_only_once_when_busy() {
        struct CountingProbe {
            busy: u16,
            counts: RefCell<BTreeMap<u16, u32>>,
        }
        impl PortProbe for CountingProbe {
            fn is_free(&self, port: u16) -> bool {
                *self.counts.borrow_mut().entry(port).or_insert(0) += 1;
                port != self.busy
            }
        }

        let fx = Fixture::new();
        let root = fx.workspace("app");
        // Slot 0's own port is preferred (as if this worktree last ran in
        // slot 0), but that port is busy, forcing a fall back to the next
        // free slot.
        let canonical = fs::canonicalize(&root).unwrap();
        fx.registry.ensure_dir().unwrap();
        fx.registry.remember_slot(&canonical, 0).unwrap();
        let probe = CountingProbe {
            busy: 41081,
            counts: RefCell::new(BTreeMap::new()),
        };

        let session = allocate(&fx.registry, &plan(3), request(&root), &probe).unwrap();

        assert_eq!(session.record().slot, 1);
        assert_eq!(
            probe.counts.borrow().get(&41081),
            Some(&1),
            "the preferred slot's busy port must be probed exactly once, not once per scan pass"
        );
    }

    #[test]
    fn workspace_root_error_display_does_not_duplicate_the_preserved_source() {
        let source = io::Error::new(io::ErrorKind::NotFound, "boom");
        let error = AllocError::WorkspaceRoot {
            path: "/missing".into(),
            source,
        };
        // The source is preserved via `#[source]` (and rendered by callers
        // that show the error chain, e.g. miette's `{:?}`); the Display
        // string must not repeat it, or chain-aware renderers show it twice.
        assert_eq!(
            error.to_string(),
            "failed to resolve workspace root /missing"
        );
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            "boom"
        );
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
