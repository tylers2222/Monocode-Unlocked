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
mod asks;
mod clock;
mod completions;
#[cfg(unix)]
mod d4_deny;
mod s0_baseline;
mod s1_asks;
mod s1_levels;
mod s2_approvals;
mod s4_questions;
mod s5_scheduler;
mod world;
