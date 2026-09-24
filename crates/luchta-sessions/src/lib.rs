//! Per-worktree dev sessions: port plans, the machine-wide session registry,
//! and slot allocation used by `luchta session` and `luchta sessions`.

mod alloc;
mod name;
mod plan;
mod registry;

pub use alloc::{allocate, AllocError, PortProbe, Session, SessionRequest, TcpProbe};
pub use name::{dedupe_name, is_dns_label, sanitize_label};
pub use plan::{PlanError, PortPlan, ResolvedPort};
pub use registry::{
    LiveSession, Registry, RegistryError, SessionRecord, SlotLock, SESSIONS_DIR_ENV,
};

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
