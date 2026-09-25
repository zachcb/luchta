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

/// Overrides the session registry directory.
pub const SESSIONS_DIR_ENV: &str = "LUCHTA_SESSIONS_DIR";

const ALLOC_LOCK: &str = "alloc.lock";
const LAST_SLOTS: &str = "last-slots.json";

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("could not determine a directory for session records; set {SESSIONS_DIR_ENV}")]
    NoRegistryDir,
    // No `{source}` here: it is preserved via `#[source]` and rendered by
    // callers that show the error chain (e.g. miette's `{:?}`); repeating it
    // in Display would print the cause twice.
    #[error("failed to {action} {}", .path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> RegistryError {
    let path = path.to_path_buf();
    move |source| RegistryError::Io {
        action,
        path,
        source,
    }
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
    /// Resolved `sessions.env` templates for this slot, in declared order.
    #[serde(default)]
    pub env: Vec<SessionEnvVar>,
    /// Unix seconds when paused (phase 3); `None` while running.
    #[serde(default)]
    pub paused_at: Option<u64>,
}

/// One resolved `sessions.env` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEnvVar {
    pub name: String,
    pub value: String,
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
                live.push(LiveSession {
                    slot,
                    record: self.read_record(slot),
                });
            }
        }
        Ok(live)
    }

    pub(crate) fn record_path(&self, slot: u32) -> PathBuf {
        self.dir.join(format!("slot-{slot}.json"))
    }

    /// Slot this workspace root used last, if remembered.
    pub(crate) fn last_slot(&self, workspace_root: &Path) -> Option<u32> {
        self.read_last_slots()
            .get(&root_key(workspace_root))
            .copied()
    }

    pub(crate) fn remember_slot(
        &self,
        workspace_root: &Path,
        slot: u32,
    ) -> Result<(), RegistryError> {
        let mut slots = self.read_last_slots();
        // Worktrees get deleted; without this, entries for roots that no
        // longer exist would accumulate in this file forever.
        slots.retain(|key, _| Path::new(key).exists());
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
        let entries =
            fs::read_dir(&self.dir).map_err(io_error("read session registry", &self.dir))?;
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
    name.strip_prefix("slot-")?
        .strip_suffix(".lock")?
        .parse()
        .ok()
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
            env: Vec::new(),
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
            vec![LiveSession {
                slot: 2,
                record: Some(rec)
            }]
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
            vec![LiveSession {
                slot: 1,
                record: None
            }]
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
    fn remembering_a_slot_prunes_roots_that_no_longer_exist() {
        let temp = TempDir::new().unwrap();
        let registry = registry(&temp);
        let gone = temp.path().join("gone");
        fs::create_dir_all(&gone).unwrap();
        registry.remember_slot(&gone, 3).unwrap();
        assert_eq!(registry.last_slot(&gone), Some(3));

        fs::remove_dir_all(&gone).unwrap();
        let still_here = temp.path().join("still-here");
        fs::create_dir_all(&still_here).unwrap();
        registry.remember_slot(&still_here, 4).unwrap();

        assert_eq!(registry.last_slot(&gone), None);
        assert_eq!(registry.last_slot(&still_here), Some(4));
    }

    #[test]
    fn a_record_without_env_deserializes_with_empty_env() {
        let json = r#"{
            "slot": 0, "id": "0-1-0", "name": "app", "pid": 1,
            "workspace_root": "/ws", "branch": null, "command": [],
            "started_at": 0, "ports": []
        }"#;
        let record: SessionRecord = serde_json::from_str(json).unwrap();
        assert!(record.env.is_empty());
    }

    #[test]
    fn io_error_display_does_not_duplicate_the_preserved_source() {
        let source = io::Error::new(io::ErrorKind::NotFound, "boom");
        let error = RegistryError::Io {
            action: "open lock file",
            path: PathBuf::from("/x"),
            source,
        };
        // The source is preserved via `#[source]` (and rendered by callers
        // that show the error chain, e.g. miette's `{:?}`); the Display
        // string must not repeat it, or chain-aware renderers show it twice.
        assert_eq!(error.to_string(), "failed to open lock file /x");
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            "boom"
        );
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
