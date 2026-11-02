//! Usage-limit routing grants (`account_route_grants`): a session moved to
//! another login until the grant expires.

use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::State;

use super::{with_conn, Provider, SpawnTarget};
use crate::harness::HarnessAccount;
use crate::session_store::{now_millis, SessionStore};

/// How a turn's error starts once its grant has ended. The front end reads it
/// as a usage-limit stop (`usageLimitFromError`).
const ROUTE_GRANT_EXPIRED: &str = "route_grant_expired";

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
    /// `first_login_resets_at` is the reset the provider itself reported for
    /// the login the session leaves; without one there is nothing to wait for.
    fn expires_at(self, now: i64, first_login_resets_at: Option<i64>) -> Result<i64, String> {
        let hours =
            match self {
                Self::OneHour => 1,
                Self::ThreeHours => 3,
                Self::SixHours => 6,
                Self::TwelveHours => 12,
                Self::TwentyFourHours => 24,
                Self::UntilReset => return match first_login_resets_at {
                    Some(at) if at > now => Ok(at),
                    Some(_) => Err("The first login has already reset; pick a length".into()),
                    None => Err(
                        "The provider reported no reset time for the first login; pick a length"
                            .into(),
                    ),
                },
            };
        Ok(now + hours * HOUR_MS)
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

/// The login a session's grant routes its child to at `now`. While the grant
/// lasts, a child sent to its first login starts on the granted one. Once it
/// ends, a child on the granted login stops before its turn; one sent back to
/// the first login starts there.
pub(super) fn route(
    store: &SessionStore,
    session_id: &str,
    now: i64,
    mut target: SpawnTarget,
) -> Result<SpawnTarget, String> {
    let Some(account) = &target.account else {
        return Ok(target);
    };
    let Some(grant) = grant(store, session_id)? else {
        return Ok(target);
    };
    let on = |id: &str| account.provider == grant.provider.as_str() && account.id == id;
    if now >= grant.expires_at {
        if on(&grant.to_account_id) {
            return Err(format!(
                "{ROUTE_GRANT_EXPIRED}: the grant to the {} login {} ended at {}",
                grant.provider.as_str(),
                grant.to_account_id,
                grant.expires_at
            ));
        }
    } else if on(&grant.from_account_id) {
        target.account = Some(HarnessAccount {
            provider: account.provider.clone(),
            id: grant.to_account_id,
        });
    }
    Ok(target)
}
