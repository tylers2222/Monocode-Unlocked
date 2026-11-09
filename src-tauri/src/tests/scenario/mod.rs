//! The scenario harness (Change R plan v2, section 3 layer 1). A [`World`]
//! runs the app's own store on a file database: it opens it through
//! `SessionStore::open_at` (migrate plus `startup`), can kill and restart
//! the app, and makes every call through the functions the Tauri commands
//! call, at a fake wall time. Agents are real rows. `MONOCODE_SCENARIO_DB`
//! starts a World from a copy of a real `monocode.db` under `/tmp`.
//!
//! `scripts/scenarios/repeat.sh <filter>` runs a scenario 20 times.

mod agent_sandbox;
mod agents;
mod alarms_limit;
mod alarms_route;
mod alarms_stall;
mod alarms_text;
mod alarms_text_edges;
mod alarms_turns;
mod alarms_unseen;
mod alarms_wakes;
mod asks;
mod clock;
mod completions;
mod cross_space;
mod d2_db_key;
#[cfg(unix)]
mod d4_deny;
#[cfg(unix)]
mod devin_supervised;
#[cfg(unix)]
mod logins;
#[cfg(unix)]
mod mono_home_links;
#[cfg(unix)]
mod omp_mcp;
#[cfg(unix)]
mod ownerless_space;
mod s0_baseline;
mod s1_asks;
mod s1_levels;
mod s2_approvals;
mod s3_extension;
mod s3_miss;
mod s4_questions;
mod s5_delivery;
mod s5_scheduler;
#[cfg(not(windows))]
mod space_caller;
#[cfg(unix)]
mod space_checkouts;
#[cfg(unix)]
mod space_import;
mod space_login;
#[cfg(not(windows))]
mod space_projects;
#[cfg(not(windows))]
mod space_rows;
mod space_scope;
#[cfg(unix)]
mod space_shared;
#[cfg(unix)]
mod space_worktrees;
mod spaces;
#[cfg(not(windows))]
mod spaces_commands;
#[cfg(unix)]
mod spawn_priority;
#[cfg(not(windows))]
mod wake_turn;
mod world;
