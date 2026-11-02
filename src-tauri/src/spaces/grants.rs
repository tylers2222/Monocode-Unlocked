//! Usage-limit routing grants (`account_route_grants`): a session moved to
//! another login until the grant expires.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::{Manager, Runtime, State};

use super::{with_conn, Provider, SpawnTarget};
use crate::harness::HarnessHost;
use crate::session_store::{now_millis, SessionStore};

const HOUR_MS: i64 = 60 * 60 * 1000;

/// The lengths the usage-limit card offers, the only ones a grant can have.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum GrantLength {
    #[serde(rename = "1h")]
    OneHour,
    #[serde(rename = "3h")]
    ThreeHours,
    #[serde(rename = "6h")]
    SixHours,
    #[serde(rename = "12h")]
    TwelveHours,
    #[serde(rename = "24h")]
    TwentyFourHours,
    /// Until the login the session leaves resets. No cap: a monthly limit
    /// can be weeks away.
    #[serde(rename = "untilReset")]
    UntilReset,
}

impl GrantLength {
    /// `None` for "until the first login resets".
    fn hours(self) -> Option<i64> {
        match self {
            Self::OneHour => Some(1),
            Self::ThreeHours => Some(3),
            Self::SixHours => Some(6),
            Self::TwelveHours => Some(12),
            Self::TwentyFourHours => Some(24),
            Self::UntilReset => None,
        }
    }

    /// `first_login_resets_at` is the reset the provider itself reported for
    /// the login the session leaves; without one there is nothing to wait for.
    fn expires_at(self, now: i64, first_login_resets_at: Option<i64>) -> Result<i64, String> {
        match (self.hours(), first_login_resets_at) {
            (Some(hours), _) => Ok(now + hours * HOUR_MS),
            (None, Some(at)) if at > now => Ok(at),
            (None, Some(_)) => Err("The first login has already reset; pick a length".into()),
            (None, None) => {
                Err("The provider reported no reset time for the first login; pick a length".into())
            }
        }
    }
}

/// What the usage-limit card's Yes sends.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrantRequest {
    pub(crate) session_id: String,
    pub(crate) provider: Provider,
    pub(crate) from_account_id: String,
    pub(crate) to_account_id: String,
    pub(crate) length: GrantLength,
    pub(crate) first_login_resets_at: Option<i64>,
}

/// The schema refuses a grant that ends before it starts and one whose
/// logins do not exist; `write_grant` decides how long it lasts.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteGrant {
    pub(crate) session_id: String,
    pub(crate) provider: Provider,
    pub(crate) from_account_id: String,
    pub(crate) to_account_id: String,
    pub(crate) granted_at: i64,
    pub(crate) expires_at: i64,
}

impl RouteGrant {
    /// The one expiry boundary: a turn at `expires_at` is already refused.
    fn ended(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

pub(crate) fn grant(store: &SessionStore, session_id: &str) -> Result<Option<RouteGrant>, String> {
    with_conn(store, |conn| {
        conn.query_row(
            "SELECT session_id, provider, from_account_id, to_account_id, granted_at, expires_at
               FROM account_route_grants WHERE session_id = ?1",
            [session_id],
            |row| {
                Ok(RouteGrant {
                    session_id: row.get(0)?,
                    provider: row.get(1)?,
                    from_account_id: row.get(2)?,
                    to_account_id: row.get(3)?,
                    granted_at: row.get(4)?,
                    expires_at: row.get(5)?,
                })
            },
        )
        .optional()
    })
}

pub(crate) fn save_grant(store: &SessionStore, grant: &RouteGrant) -> Result<(), String> {
    with_conn(store, |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO account_route_grants
               (session_id, provider, from_account_id, to_account_id, granted_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                grant.session_id,
                grant.provider,
                grant.from_account_id,
                grant.to_account_id,
                grant.granted_at,
                grant.expires_at
            ],
        )
    })?;
    Ok(())
}

/// The one product writer: the grant lasts the picked length from `now`.
/// Settings only preselect the card's choice, so they are not read here. A
/// real-app check that needs a shorter grant writes the row itself.
pub(crate) fn write_grant(
    store: &SessionStore,
    request: &GrantRequest,
    now: i64,
) -> Result<RouteGrant, String> {
    let grant = RouteGrant {
        session_id: request.session_id.clone(),
        provider: request.provider,
        from_account_id: request.from_account_id.clone(),
        to_account_id: request.to_account_id.clone(),
        granted_at: now,
        expires_at: request
            .length
            .expires_at(now, request.first_login_resets_at)?,
    };
    save_grant(store, &grant)?;
    Ok(grant)
}

pub(crate) fn remove_grant(store: &SessionStore, session_id: &str) -> Result<(), String> {
    with_conn(store, |conn| {
        conn.execute(
            "DELETE FROM account_route_grants WHERE session_id = ?1",
            [session_id],
        )
    })?;
    Ok(())
}

#[tauri::command(async)]
pub fn spaces_grant_get(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<Option<RouteGrant>, String> {
    grant(&store, &session_id)
}

#[tauri::command(async)]
pub fn spaces_grant_save(
    store: State<'_, SessionStore>,
    request: GrantRequest,
) -> Result<RouteGrant, String> {
    write_grant(&store, &request, now_millis())
}

#[tauri::command(async)]
pub fn spaces_grant_remove(
    store: State<'_, SessionStore>,
    session_id: String,
) -> Result<(), String> {
    remove_grant(&store, &session_id)
}

/// The error a turn stops with once its grant has ended. It starts with
/// `route_grant_expired`, which the front end reads as a usage-limit stop
/// (`usageLimitFromError`), and names what the expired-grant card shows and
/// the login the session goes back to (`expiredRouteGrant` parses it).
fn expired(grant: &RouteGrant) -> String {
    format!(
        "route_grant_expired: the grant from {} to {} granted at {} ended at {}",
        grant.from_account_id, grant.to_account_id, grant.granted_at, grant.expires_at
    )
}

/// Before every turn the app sends (`control::control_authorize_turn`, which
/// `sendHarnessTurn` calls first), so also a turn on a live child, which
/// reaches no spawn. A running turn is never checked. Once the session's grant has ended,
/// the turn is refused, the grant is removed (so it never blocks a login
/// again), and the child on the granted login is stopped, so the next turn
/// starts a new child on the login the session picks then.
pub(crate) fn check_turn(
    store: &SessionStore,
    children: &HarnessHost,
    session_id: &str,
    now: i64,
) -> Result<(), String> {
    match grant(store, session_id)? {
        Some(grant) if grant.ended(now) => {
            remove_grant(store, session_id)?;
            children.kill_session_tree(session_id);
            Err(expired(&grant))
        }
        _ => Ok(()),
    }
}

/// `check_turn` on the app's clock, the one `spaces_grant_save` writes with.
pub(crate) fn check_app_turn<R: Runtime>(
    app: &impl Manager<R>,
    session_id: &str,
) -> Result<(), String> {
    check_turn(
        &app.state::<SessionStore>(),
        &app.state::<HarnessHost>(),
        session_id,
        now_millis(),
    )
}

/// The spawn hook's grant step, behind `check_turn`. It never changes the
/// login: the transcript handoff moved the session to the granted one. The
/// session's own child (its spawn id without `#n`) on any other login means
/// the user moved it off the grant (Go back, or their own pick), which ends
/// it; a helper child has another id, so it never does. A child on the granted login after
/// the grant has ended is refused once, and the grant is removed.
pub(super) fn route(
    store: &SessionStore,
    session_id: &str,
    now: i64,
    target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    let Some(account) = &target.account else {
        return Ok(target);
    };
    let session_id = crate::harness::child_session(session_id);
    let Some(grant) = grant(store, session_id)? else {
        return Ok(target);
    };
    let on_grant = account.provider == grant.provider.as_str() && account.id == grant.to_account_id;
    let ended = grant.ended(now);
    if ended || !on_grant {
        remove_grant(store, session_id)?;
    }
    if ended && on_grant {
        return Err(expired(&grant));
    }
    Ok(target)
}
