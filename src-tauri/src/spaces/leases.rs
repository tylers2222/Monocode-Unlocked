//! Checkout leases (`checkout_leases`): each working copy has one owner Space.

use super::SpawnTarget;
use crate::session_store::SessionStore;

/// The folder a child starts in. Today: the one its caller passed.
pub(super) fn claim(
    _store: &SessionStore,
    _session_id: &str,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
