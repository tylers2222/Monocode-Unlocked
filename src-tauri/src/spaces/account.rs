//! Logins (`provider_accounts`) and the login each Space uses per provider
//! (`space_accounts`).

use super::{Provider, SpawnTarget};
use crate::session_store::SessionStore;

/// The login a child starts with. Today: the one its caller passed.
pub(super) fn resolve(
    _store: &SessionStore,
    _session_id: &str,
    _provider: Option<Provider>,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    Ok(target)
}
