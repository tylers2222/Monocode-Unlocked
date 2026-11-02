//! Usage-limit routing grants (`account_route_grants`): a session moved to
//! another login until the grant expires.

use super::SpawnTarget;
use crate::session_store::SessionStore;

/// The login a session's grant routes it to at `now`. Today: no routing.
pub(super) fn route(
    _store: &SessionStore,
    _session_id: &str,
    _now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
