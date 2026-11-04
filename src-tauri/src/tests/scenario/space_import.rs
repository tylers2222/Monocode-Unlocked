//! The MonoCode 2 -> Space 2 importer (ADE-178, `scripts/spaces/merge-import.py`)
//! on two made-up app data folders whose DBs the app's own `migrate` made:
//! Space 1's with the Spaces tables, MonoCode 2's as it is today, without
//! them. The importer runs as at cut-over, after `merge-dry-run.py`
//! (ADE-169) made its report, and the result is read back through the Spaces
//! storage, the spawn hook and the login-folder rule the app uses. The
//! scenario runs in a process whose home folder is a scratch folder, so no
//! path it passes is inside a real one.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value as Json;

use super::agent_sandbox::mock_app;
use crate::harness::HarnessAccount;
use crate::session_store::SessionStore;
use crate::spaces::account::{self, ProviderAccount, SpaceLogin};
use crate::spaces::{self, Provider, SpawnTarget};

const ROOT: &str = "MC_ADE178_ROOT";
const REAL_HOME: &str = "MC_ADE178_REAL_HOME";
const NOW: i64 = 1_800_000_000_000;
const DELTA: i64 = 4_000;

#[test]
fn monocode_2_imports_as_space_2() {
    let root = std::env::temp_dir().join(format!("ade178-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "scenario::space_import::import_child",
            "--ignored",
            "--quiet",
            "--nocapture",
        ])
        .env(ROOT, &root)
        .env(REAL_HOME, std::env::var_os("HOME").unwrap_or_default())
        .env("HOME", root.join("home"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        child.status.success(),
        "the import scenario failed:\n{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

struct Rig {
    root: PathBuf,
    home: PathBuf,
    one: PathBuf,
    two: PathBuf,
}

impl Rig {
    fn report(&self) {
        let out = python(
            "merge-dry-run.py",
            &[
                "--out".into(),
                self.root.join("report"),
                "--space1".into(),
                self.one.clone(),
                "--space2".into(),
                self.two.clone(),
                "--webkit1".into(),
                self.root.join("webkit1"),
                "--webkit2".into(),
                self.root.join("webkit2"),
            ],
        );
        assert!(out.status.success(), "merge-dry-run: {}", text(&out.stderr));
    }

    /// `merge-import.py --json` with the cut-over arguments plus `extra`;
    /// a later `--report` or `--backup` wins over the default.
    fn import(&self, extra: &[&str]) -> Output {
        let mut args: Vec<PathBuf> = vec![
            "--json".into(),
            "--space1".into(),
            self.one.clone(),
            "--space2".into(),
            self.two.clone(),
            "--report".into(),
            self.root.join("report/merge-dry-run.json"),
            "--claude-home".into(),
            self.home.join(".claude-2"),
            "--backup".into(),
            self.root.join("backups"),
        ];
        args.extend(extra.iter().map(PathBuf::from));
        python("merge-import.py", &args)
    }

    fn imported(&self, extra: &[&str]) -> Json {
        let out = self.import(extra);
        assert!(
            out.status.success(),
            "merge-import {extra:?}: {}",
            text(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// An apply stops with `code` and a message holding `says`, and Space 1
    /// is byte-identical after it.
    fn refused(&self, extra: &[&str], code: i32, says: &str) {
        let one = fingerprint(&self.one);
        let out = self.import(&[extra, &["--apply"][..]].concat());
        assert_eq!(
            out.status.code(),
            Some(code),
            "merge-import {extra:?} did not stop: {}{}",
            text(&out.stdout),
            text(&out.stderr)
        );
        assert!(
            text(&out.stderr).contains(says),
            "merge-import {extra:?} stopped for another reason: {}",
            text(&out.stderr)
        );
        assert_eq!(fingerprint(&self.one), one, "merge-import {extra:?} wrote");
    }

    fn db(&self, data: &Path) -> Connection {
        Connection::open(data.join("monocode.db")).unwrap()
    }
}

fn python(script: &str, args: &[PathBuf]) -> Output {
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../scripts/spaces");
    Command::new("python3")
        .arg("-I")
        .arg(scripts.join(script))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// Every path under `root` with its kind and contents (a link's target).
fn fingerprint(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path.strip_prefix(root).unwrap().display().to_string();
            let meta = std::fs::symlink_metadata(&path).unwrap();
            if meta.file_type().is_symlink() {
                out.insert(
                    rel,
                    format!("link {}", std::fs::read_link(&path).unwrap().display()),
                );
            } else if meta.is_dir() {
                out.insert(rel, "dir".into());
                walk(root, &path, out);
            } else {
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                std::fs::read(&path).unwrap().hash(&mut hasher);
                out.insert(rel, format!("file {:x}", hasher.finish()));
            }
        }
    }
    let mut out = BTreeMap::new();
    if root.exists() {
        walk(root, root, &mut out);
    }
    out
}

/// Every row of every table, in rowid order. With no `-wal` the read is
/// immutable, so it leaves no file behind.
fn dump(db: &Path) -> BTreeMap<String, Vec<String>> {
    let live = db.with_file_name("monocode.db-wal").exists();
    let uri = format!(
        "file:{}?mode=ro{}",
        db.display(),
        if live { "" } else { "&immutable=1" }
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI;
    let conn = Connection::open_with_flags(uri, flags).unwrap();
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let mut stmt = conn
                .prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))
                .unwrap();
            let width = stmt.column_count();
            let rows = stmt
                .query_map([], |row| {
                    (0..width)
                        .map(|i| row.get::<_, Value>(i).map(|value| format!("{value:?}")))
                        .collect::<Result<Vec<_>, _>>()
                        .map(|values| values.join("|"))
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

fn one_value(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn one_text(conn: &Connection, sql: &str) -> String {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

/// A data folder whose DB the app's own `migrate` made, with `rows` in it.
fn app_folder(data: &Path, rows: &str) {
    let store = SessionStore::open_at(data.join("monocode.db"), NOW).unwrap();
    store.lock_conn().unwrap().execute_batch(rows).unwrap();
}

const SESSION: &str =
    "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at, provider_account_id, lead_session_id)";
const APPROVAL: &str = "INSERT INTO approvals (id, project_path, heading, summary, tradeoffs, options, urgency, links, requested_by, requested_by_name, created_at, status)";

/// Space 1: a Mono with its chat in /p and its projects /p and /t.
fn space_one(data: &Path) {
    app_folder(
        data,
        &format!(
            "INSERT OR REPLACE INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms) VALUES (1, 5000, 0, 0);
             INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('m1', 'Made-up One', 'mono', '{{\"sessionId\":\"s1-a\",\"projects\":[\"/p\",\"/t\"]}}', 0, 1, 1);
             {SESSION} VALUES ('s1-a', '/p', 'claude', 'm', 'auto', 'Made-up one', 1, 1, NULL, NULL);
             {APPROVAL} VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaa1', '/p', 'Made-up', 'S',
               '{{\"pros\":[],\"cons\":[]}}', '[]', 'blocking', '[]', 'm1', 'One', 1, 'open');"
        ),
    );
    write(&data.join("monos/m1/MEMORY.md"), "Space 1's made-up memory");
}

/// MonoCode 2 today: its own clock and downtime, a Mono with its chat and
/// projects (one only it lists, two Space 1's Mono lists too), a worker in a
/// project only it used, an open ask with its event and outbox
/// row on its clock, the worker's open stall alarm with its activity, send,
/// outbox row and own N, its closed one, an alarm setting, an approval numbered as Space
/// 1's, and a note.
fn space_two(data: &Path) {
    app_folder(
        data,
        &format!(
            "INSERT OR REPLACE INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms) VALUES (1, 1000, 0, 0);
             INSERT INTO mono_downtime (id, start_wall_ms, end_wall_ms, cause, active_ms) VALUES (1, 1, 2, 'sleep', 3);
             INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('m2', 'Made-up Two', 'mono', '{{\"sessionId\":\"s2-a\",\"projects\":[\"/p\",\"/s/\",\"/t\"]}}', 0, 1, 1);
             {SESSION} VALUES ('s2-a', '/p', 'claude', 'm', 'auto', 'Made-up two', 1, 1, 'default', NULL);
             {SESSION} VALUES ('s2-b', '/q/', 'claude', 'm', 'auto', 'Made-up worker', 1, 1, NULL, 's2-a');
             INSERT INTO mono_transcripts (session_id) VALUES ('s2-a');
             INSERT INTO mono_blocks VALUES ('s2-a', 0, 'b0', '{{}}', 'made-up', 1, 0), ('s2-a', 1, 'b1', '{{}}', 'reply', 0, 0);
             INSERT INTO mono_asks (id, parent_actor, child_actor, parent_level, child_level, state, due_active_ms, next_due_active_ms, created_wall_ms)
               VALUES ('k2', 'mono:m2', 'session:s2-b', 2, 3, 'open', 1500, 1600, 1);
             INSERT INTO mono_ask_events (ask_id, kind, seq, at_active_ms, at_wall_ms) VALUES ('k2', 'update', 1, 1200, 1);
             INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms) VALUES ('o2', 'k2', 't5', 'mono:m2', 1400);
             INSERT INTO agent_activity (session_id, harness, turn_started_active_ms, turn_ended_active_ms,
               last_progress_active_ms, last_progress_wall_ms, background_ended_active_ms, dialog_kind,
               dialog_since_active_ms, wake_pending_since_active_ms, updated_wall_ms)
               VALUES ('s2-b', 'claude', 700, 900, 800, 7, 850, 'plan', 870, 950, 1);
             INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause, open_work_json, stalled_since_active_ms,
               stalled_since_wall_ms, opened_active_ms, opened_wall_ms, next_escalate_active_ms)
               VALUES ('al_2', 'session:s2-b', 'm2', 'mono:m2', 'idle_open_work', '[]', 900, 1, 1300, 1, 2200);
             INSERT INTO alarm_episodes (id, actor, mono_id, provisioner, cause, open_work_json, stalled_since_active_ms,
               stalled_since_wall_ms, opened_active_ms, opened_wall_ms, acted_active_ms, closed_active_ms,
               closed_wall_ms, close_reason)
               VALUES ('al_1', 'session:s2-b', 'm2', 'mono:m2', 'hung', '[]', 300, 3, 400, 4, 450, 500, 5, 'progress');
             INSERT INTO alarm_sends (episode_id, level, recipient, outbox_key, created_active_ms, sent_wall_ms)
               VALUES ('al_2', 1, 'mono:m2', 'alarm-al_2-1', 1300, 6);
             INSERT INTO mono_outbox (key, stage, recipient, at_active_ms) VALUES ('alarm-al_2-1', 'alarm', 'mono:m2', 1300);
             INSERT INTO alarm_prompts (outbox_key, episode_id, text) VALUES ('alarm-al_2-1', 'al_2', '<mono_alarm>');
             INSERT INTO alarm_dialogs (episode_id, request_id) VALUES ('al_2', 'req_1');
             INSERT INTO alarm_acts (episode_id, seq, level, actor, target, how, at_active_ms, at_wall_ms)
               VALUES ('al_2', 1, 1, 'mono:m2', 'session:s2-b', 'dismiss', 1500, 1);
             INSERT INTO agent_alarm_settings (actor, stall_minutes) VALUES ('session:s2-b', 30);
             INSERT INTO alarm_settings (key, value) VALUES ('stall_minutes', '20');
             {APPROVAL} VALUES ('bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbb2', '/q', 'Made-up', 'S',
               '{{\"pros\":[],\"cons\":[]}}', '[]', 'blocking', '[]', 'm2', 'Two', 1, 'open');
             INSERT INTO notes (id, slug, title, created_at, updated_at) VALUES ('n2', 'made-up-plan', 'Made-up plan', 1, 1);"
        ),
    );
    // Today's MonoCode 2 predates Spaces.
    Connection::open(data.join("monocode.db"))
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             DROP TABLE account_route_grants; DROP TABLE space_project_accounts;
             DROP TABLE space_accounts; DROP TABLE checkout_leases; DROP TABLE space_projects;
             DROP TABLE space_settings; DROP TABLE provider_accounts; DROP TABLE spaces;
             ALTER TABLE monos DROP COLUMN space_id; ALTER TABLE sessions DROP COLUMN space_id;",
        )
        .unwrap();
    // A transcript naming a file in MonoCode 2's data folder stays as written.
    let named = data.join("monos/m2/MEMORY.md");
    Connection::open(data.join("monocode.db"))
        .unwrap()
        .execute(
            "UPDATE sessions SET blocks_json = json_array(?1) WHERE id = 's2-a'",
            [named.to_str().unwrap()],
        )
        .unwrap();
    write(&named, "Space 2's made-up memory");
    write(&data.join("checkpoints/s2-a/0.json"), "{}");
    write(&data.join(".window-state.json"), "{}");
}

/// Defect: a dry run writes, or predicts other counts than an apply makes;
/// an apply writes without a complete backup, with an app running, on a
/// stale report, over a Space 1 row or folder that shares a Space 2 key, on a
/// table it has no rule for or Spaces rows MonoCode 2 made itself, or on a
/// path in the real home without `--real`; it copies the login folder
/// instead of linking it, touches the first app's `~/.codex`, leaves a Space
/// 2 row in Space 1, a project only Space 2 used in Space 1, an ask or a
/// stall alarm on MonoCode 2's clock or an approval on its number, copies
/// MonoCode 2's own clock state or alarm settings, or a second apply changes
/// anything.
#[test]
#[ignore = "monocode_2_imports_as_space_2 runs it in a process of its own"]
fn import_child() {
    let root = std::fs::canonicalize(std::env::var_os(ROOT).unwrap()).unwrap();
    let home = root.join("home");
    let rig = Rig {
        one: home.join("apps/com.monocode.desktop"),
        two: home.join("apps/com.monocode.second"),
        home,
        root,
    };
    space_one(&rig.one);
    space_two(&rig.two);
    write(&rig.home.join(".claude-2/keep"), "Space 2's made-up login");
    write(&rig.home.join(".codex/auth.json"), CODEX_LOGIN);
    std::fs::create_dir_all(rig.root.join("webkit1")).unwrap();
    std::fs::create_dir_all(rig.root.join("webkit2")).unwrap();
    let codex_mtime = std::fs::metadata(rig.home.join(".codex/auth.json"))
        .unwrap()
        .modified()
        .unwrap();
    rig.report();

    let plan = dry_run_predicts_and_writes_nothing(&rig);
    arguments_that_stop_the_run(&rig);
    data_that_stops_the_run(&rig);
    let lock = std::fs::File::create(rig.one.join("scheduler.lock")).unwrap();
    lock.try_lock().unwrap();
    rig.refused(&[], 1, "quit its app first");
    drop(lock);
    let resumed = a_stopped_apply_resumes(&rig, &plan);
    apply_makes_the_prediction(&rig, &resumed);
    second_apply_changes_nothing(&rig);
    app_reads_the_rows(&rig);
    every_alarm_clock_column_moves(&rig);
    app_reads_the_spaces(&rig);
    first_apps_codex_login_untouched(&rig, codex_mtime);
}

const CODEX_LOGIN: &str = "the first app's made-up Codex login";

fn dry_run_predicts_and_writes_nothing(rig: &Rig) -> Json {
    let (one, two) = (fingerprint(&rig.one), fingerprint(&rig.two));
    let plan = rig.imported(&[]);
    assert_eq!(fingerprint(&rig.one), one, "the dry run changed Space 1");
    assert_eq!(fingerprint(&rig.two), two, "the dry run changed MonoCode 2");
    assert!(
        !rig.root.join("backups").exists(),
        "the dry run made a backup"
    );
    let row = |table: &str| plan["tables"][table].clone();
    assert_eq!(
        row("sessions"),
        serde_json::json!({"source": 2, "before": 1, "insert": 2, "after": 3})
    );
    assert_eq!(
        row("approvals"),
        serde_json::json!({"source": 1, "before": 1, "insert": 1, "after": 2})
    );
    assert_eq!(row("mono_blocks")["insert"], 2);
    assert_eq!(
        row("mono_downtime"),
        Json::Null,
        "MonoCode 2's downtime is copied"
    );
    assert_eq!(plan["kept"]["mono_downtime"], 1);
    assert_eq!(plan["kept"]["alarm_settings"], 1);
    assert_eq!(plan["clock_delta_ms"], DELTA);
    assert_eq!(plan["folders"]["monos"]["copy"], serde_json::json!(["m2"]));
    assert_eq!(
        plan["folders"]["checkpoints"]["copy"],
        serde_json::json!(["s2-a"])
    );
    assert_eq!(plan["links"], serde_json::json!({"claude": "link"}));
    assert_eq!(
        plan["registered"],
        serde_json::json!([
            "spaces space-2",
            "provider_accounts claude/monocode-2",
            "space_accounts space-2/claude",
            "space_projects /q",
            "space_projects /s"
        ])
    );
    assert_eq!(plan["changes"], 28);
    let left = &plan["not_imported"];
    assert_eq!(left["own_paths"]["count"], 1, "{left}");
    assert_eq!(
        left["own_paths"]["columns"],
        serde_json::json!({"sessions.blocks_json": 1})
    );
    assert_eq!(
        left["other_entries"]["entries"],
        serde_json::json!([".window-state.json"])
    );
    assert_eq!(left["browser_storage"]["count"], 0);
    plan
}

/// Arguments that stop an apply: a path in the real home, the first app's
/// Codex login, a stale report, a backup folder inside an app's data or one
/// that cannot be made, a login folder outside the home folder or inside an
/// app's data, and one folder for both logins.
fn arguments_that_stop_the_run(rig: &Rig) {
    let real = PathBuf::from(std::env::var_os(REAL_HOME).unwrap()).join(format!(
        "Library/Application Support/ade178-missing-{}",
        uuid::Uuid::new_v4()
    ));
    let mut stale: Json =
        serde_json::from_slice(&std::fs::read(rig.root.join("report/merge-dry-run.json")).unwrap())
            .unwrap();
    stale["tables"]["sessions"]["rows"]["space2"] = 1.into();
    std::fs::write(rig.root.join("stale.json"), stale.to_string()).unwrap();
    std::fs::write(rig.root.join("not-a-folder"), "").unwrap();
    for folder in [rig.root.join("outside"), rig.one.join("login")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let path = |p: PathBuf| p.to_str().unwrap().to_owned();
    let rows: [(&str, String, i32, &str); 8] = [
        ("--space1", path(real), 2, "--real"),
        (
            "--codex-home",
            path(rig.home.join(".codex")),
            1,
            "never touched",
        ),
        (
            "--report",
            path(rig.root.join("stale.json")),
            1,
            "the report says",
        ),
        (
            "--backup",
            path(rig.one.join("backups")),
            2,
            "--backup must be outside",
        ),
        (
            "--backup",
            path(rig.root.join("not-a-folder/b")),
            1,
            "Not a directory",
        ),
        (
            "--claude-home",
            path(rig.root.join("outside")),
            1,
            "inside the home folder",
        ),
        (
            "--claude-home",
            path(rig.one.join("login")),
            1,
            "outside every app data folder",
        ),
        (
            "--codex-home",
            path(rig.home.join(".claude-2")),
            1,
            "must be different folders",
        ),
    ];
    for (flag, value, code, says) in rows {
        rig.refused(&[flag, value.as_str()], code, says);
    }
    std::fs::remove_dir(rig.one.join("login")).unwrap();
}

type Change = fn(&Rig);

/// Data that stops an apply, each made, refused against a fresh report and
/// undone: a Space 1 folder entry or row sharing a Space 2 key, a table with
/// no rule, Spaces rows or named logins MonoCode 2 made itself, another
/// login at Space 2's link, Space 2 already on another login, and a
/// `space-2` the user made under another name.
fn data_that_stops_the_run(rig: &Rig) {
    let rows: [(Change, &str, Change); 8] = [
        (
            |rig| write(&rig.one.join("monos/m2/MEMORY.md"), "another Mono's memory"),
            "in both Spaces with different contents",
            |rig| std::fs::remove_dir_all(rig.one.join("monos/m2")).unwrap(),
        ),
        (
            |rig| {
                sql(&rig.one, "INSERT INTO notes (id, slug, title, created_at, updated_at) VALUES ('n2', 'other', 'Other', 1, 1)")
            },
            "notes: 1 Space 2 rows share a key",
            |rig| sql(&rig.one, "DELETE FROM notes WHERE id = 'n2'"),
        ),
        (
            |rig| sql(&rig.two, "CREATE TABLE made_up_new (id TEXT PRIMARY KEY)"),
            "No import rule for made_up_new",
            |rig| sql(&rig.two, "DROP TABLE made_up_new"),
        ),
        (
            |rig| {
                sql(&rig.two, "CREATE TABLE space_projects (project_path TEXT PRIMARY KEY, space_id TEXT NOT NULL);
                 INSERT INTO space_projects VALUES ('/q', 'space-3')")
            },
            "already has its own space_projects rows",
            |rig| sql(&rig.two, "DROP TABLE space_projects"),
        ),
        (
            |rig| std::fs::create_dir_all(rig.two.join("provider-accounts/claude/x")).unwrap(),
            "holds named logins",
            |rig| std::fs::remove_dir_all(rig.two.join("provider-accounts")).unwrap(),
        ),
        (
            |rig| {
                let link = rig.one.join("provider-accounts/claude/monocode-2");
                std::fs::create_dir_all(link.parent().unwrap()).unwrap();
                std::fs::create_dir_all(rig.home.join(".claude-other")).unwrap();
                std::os::unix::fs::symlink(rig.home.join(".claude-other"), link).unwrap();
            },
            "is already another login",
            |rig| std::fs::remove_dir_all(rig.one.join("provider-accounts")).unwrap(),
        ),
        (
            |rig| {
                sql(&rig.one, "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Space 2', 1);
                 INSERT INTO provider_accounts (provider, id, label, created_at) VALUES ('claude', 'other', 'Other', 1);
                 INSERT INTO space_accounts (space_id, provider, account_id) VALUES ('space-2', 'claude', 'other')")
            },
            "Space 2 already uses claude login other",
            |rig| {
                sql(
                    &rig.one,
                    "DELETE FROM space_accounts; DELETE FROM provider_accounts WHERE id = 'other';
                 DELETE FROM spaces WHERE id = 'space-2'",
                )
            },
        ),
        (
            |rig| {
                sql(
                    &rig.one,
                    "INSERT INTO spaces (id, name, created_at) VALUES ('space-2', 'Work', 1)",
                )
            },
            "space-2 already exists as 'Work'",
            |rig| sql(&rig.one, "DELETE FROM spaces WHERE id = 'space-2'"),
        ),
    ];
    for (make, says, undo) in rows {
        make(rig);
        rig.report();
        rig.refused(&[], 1, says);
        undo(rig);
        rig.report();
    }
}

fn sql(data: &Path, batch: &str) {
    Connection::open(data.join("monocode.db"))
        .unwrap()
        .execute_batch(batch)
        .unwrap();
}

/// An apply stopped after its backup, folder copies and login link, before
/// the rows: another connection holds Space 1's write lock. The backup is
/// Space 1 as it was, and the link is the importer's own. A dry run then
/// counts the copied entries and the link as present and predicts every row.
fn a_stopped_apply_resumes(rig: &Rig, plan: &Json) -> Json {
    let one = fingerprint(&rig.one);
    let before = dump(&rig.one.join("monocode.db"));
    let holder = rig.db(&rig.one);
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = rig.import(&["--apply"]);
    drop(holder);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stdout));
    assert!(
        text(&out.stderr).contains("database is locked"),
        "{}",
        text(&out.stderr)
    );
    assert_eq!(
        dump(&rig.one.join("monocode.db")),
        before,
        "rows were imported"
    );
    let backups: Vec<PathBuf> = std::fs::read_dir(rig.root.join("backups"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(backups.len(), 1, "{backups:?}");
    let backup = &backups[0];
    assert_eq!(
        dump(&backup.join("monocode.db")),
        before,
        "the DB backup is not Space 1 as it was"
    );
    let monos: BTreeMap<String, String> = one
        .iter()
        .filter_map(|(path, kind)| {
            let rest = path.strip_prefix("monos/")?;
            Some((rest.to_owned(), kind.clone()))
        })
        .collect();
    assert_eq!(
        fingerprint(&backup.join("folders/monos")),
        monos,
        "the monos/ backup is not Space 1's monos/ as it was"
    );
    assert!(backup.join("manifest.json").is_file());
    let resumed = rig.imported(&[]);
    for key in ["tables", "kept", "registered", "not_imported"] {
        assert_eq!(
            resumed[key], plan[key],
            "after a stop the dry run's {key} moved"
        );
    }
    for folder in ["monos", "checkpoints"] {
        assert_eq!(
            resumed["folders"][folder],
            serde_json::json!({"source": 1, "present": 1, "copy": []})
        );
    }
    assert_eq!(resumed["links"], serde_json::json!({"claude": "present"}));
    assert_eq!(resumed["changes"], plan["changes"].as_i64().unwrap() - 3);
    resumed
}

/// The resumed apply imports what the dry run predicted, after a backup of
/// the DB, and the login stays a link, not a copy.
fn apply_makes_the_prediction(rig: &Rig, plan: &Json) {
    let before = dump(&rig.one.join("monocode.db"));
    let done = rig.imported(&["--apply"]);
    for key in [
        "tables",
        "kept",
        "not_imported",
        "folders",
        "links",
        "registered",
        "changes",
    ] {
        assert_eq!(
            done[key], plan[key],
            "the apply's {key} differ from the dry run's"
        );
    }
    let backup = PathBuf::from(done["backup"].as_str().unwrap());
    assert_eq!(
        dump(&backup.join("monocode.db")),
        before,
        "the DB backup is not Space 1 as it was"
    );
    assert!(backup.join("result.json").is_file());
    let link = rig.one.join("provider-accounts/claude/monocode-2");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink(),
        "Space 2's Claude login is a copy, not a link"
    );
    assert_eq!(
        std::fs::read_link(&link).unwrap(),
        rig.home.join(".claude-2")
    );
    assert!(
        !rig.one.join("provider-accounts/codex").exists(),
        "a Codex login was registered"
    );
    assert_eq!(
        std::fs::read_to_string(rig.one.join("monos/m2/MEMORY.md")).unwrap(),
        "Space 2's made-up memory"
    );
    assert!(rig.one.join("checkpoints/s2-a/0.json").is_file());
}

fn files_but_the_db(root: &Path) -> BTreeMap<String, String> {
    fingerprint(root)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("monocode.db"))
        .collect()
}

fn second_apply_changes_nothing(rig: &Rig) {
    let (files, rows) = (
        files_but_the_db(&rig.one),
        dump(&rig.one.join("monocode.db")),
    );
    let again = rig.imported(&["--apply"]);
    assert_eq!(
        again["changes"], 0,
        "a second apply changed something: {again}"
    );
    assert_eq!(again["imported_before"], Json::Bool(true));
    assert_eq!(
        dump(&rig.one.join("monocode.db")),
        rows,
        "a second apply changed rows"
    );
    assert_eq!(
        files_but_the_db(&rig.one),
        files,
        "a second apply changed files"
    );
}

/// Rows join Space 2, active-clock columns move onto Space 1's clock and
/// approvals take the next number; Space 1's clock state stays its own.
fn app_reads_the_rows(rig: &Rig) {
    let conn = rig.db(&rig.one);
    for (sql, want) in [
        ("SELECT space_id FROM sessions WHERE id = 's1-a'", "space-1"),
        ("SELECT space_id FROM sessions WHERE id = 's2-a'", "space-2"),
        ("SELECT space_id FROM sessions WHERE id = 's2-b'", "space-2"),
        ("SELECT space_id FROM monos WHERE id = 'm1'", "space-1"),
        ("SELECT space_id FROM monos WHERE id = 'm2'", "space-2"),
    ] {
        assert_eq!(one_text(&conn, sql), want, "{sql}");
    }
    for (sql, want) in [
        (
            "SELECT due_active_ms FROM mono_asks WHERE id = 'k2'",
            1500 + DELTA,
        ),
        (
            "SELECT next_due_active_ms FROM mono_asks WHERE id = 'k2'",
            1600 + DELTA,
        ),
        (
            "SELECT at_active_ms FROM mono_ask_events WHERE ask_id = 'k2'",
            1200 + DELTA,
        ),
        (
            "SELECT at_active_ms FROM mono_outbox WHERE key = 'o2'",
            1400 + DELTA,
        ),
        (
            "SELECT number FROM approvals WHERE id = 'aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaa1'",
            1,
        ),
        (
            "SELECT number FROM approvals WHERE id = 'bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbb2'",
            2,
        ),
        (
            "SELECT turn_ended_active_ms FROM agent_activity WHERE session_id = 's2-b'",
            900 + DELTA,
        ),
        (
            "SELECT last_progress_active_ms FROM agent_activity WHERE session_id = 's2-b'",
            800 + DELTA,
        ),
        (
            "SELECT stalled_since_active_ms FROM alarm_episodes WHERE id = 'al_2'",
            900 + DELTA,
        ),
        (
            "SELECT opened_active_ms FROM alarm_episodes WHERE id = 'al_2'",
            1300 + DELTA,
        ),
        (
            "SELECT next_escalate_active_ms FROM alarm_episodes WHERE id = 'al_2'",
            2200 + DELTA,
        ),
        (
            "SELECT created_active_ms FROM alarm_sends WHERE episode_id = 'al_2'",
            1300 + DELTA,
        ),
        (
            "SELECT at_active_ms FROM alarm_acts WHERE episode_id = 'al_2'",
            1500 + DELTA,
        ),
        (
            "SELECT count(*) FROM alarm_prompts WHERE episode_id = 'al_2'",
            1,
        ),
        (
            "SELECT count(*) FROM alarm_dialogs WHERE episode_id = 'al_2'",
            1,
        ),
        (
            "SELECT stall_minutes FROM agent_alarm_settings WHERE actor = 'session:s2-b'",
            30,
        ),
        ("SELECT count(*) FROM alarm_settings", 0),
        ("SELECT active_ms FROM mono_clock WHERE id = 1", 5000),
        ("SELECT count(*) FROM mono_downtime", 0),
    ] {
        assert_eq!(one_value(&conn, sql), want, "{sql}");
    }
}

/// The alarms' tables, each with its key.
const ALARM_TABLES: [(&str, &str); 4] = [
    ("agent_activity", "session_id"),
    ("alarm_episodes", "id"),
    ("alarm_sends", "episode_id || '/' || level"),
    ("mono_outbox", "key"),
];

/// Defect (S1 r4 F1): a column of the alarms' tables on the active clock
/// that the import's list misses is copied raw, so an imported stall
/// counts from Space 1's future (or past) by the clock difference; or a
/// wall time is moved as if it were active time.
fn every_alarm_clock_column_moves(rig: &Rig) {
    let (source, target) = (rig.db(&rig.two), rig.db(&rig.one));
    for (table, key) in ALARM_TABLES {
        let columns: Vec<String> = source
            .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for column in columns {
            let shift = match column {
                ref c if c.ends_with("_active_ms") => DELTA,
                ref c if c.ends_with("_wall_ms") => 0,
                _ => continue,
            };
            let values = |conn: &Connection| -> BTreeMap<String, Option<i64>> {
                conn.prepare(&format!("SELECT {key}, {column} FROM {table}"))
                    .unwrap()
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                    .unwrap()
                    .collect::<Result<_, _>>()
                    .unwrap()
            };
            let (before, after) = (values(&source), values(&target));
            assert!(
                before.values().any(Option::is_some),
                "{table}.{column}: set it in a MonoCode 2 row"
            );
            for (row, value) in before {
                assert_eq!(
                    after.get(&row).copied().flatten(),
                    value.map(|value| value + shift),
                    "{table}.{column} of {row}"
                );
            }
        }
    }
}

/// The Spaces storage, the spawn hook and the login-folder rule read Space 2
/// and its login.
fn app_reads_the_spaces(rig: &Rig) {
    let store = SessionStore::open_at(rig.one.join("monocode.db"), NOW).unwrap();
    let listed: Vec<(String, String)> = spaces::list(&store)
        .unwrap()
        .into_iter()
        .map(|space| (space.id, space.name))
        .collect();
    assert!(
        listed.contains(&("space-2".into(), "Space 2".into())),
        "{listed:?}"
    );
    let login = ProviderAccount {
        provider: Provider::Claude,
        id: "monocode-2".into(),
        label: "MonoCode 2".into(),
    };
    assert!(account::accounts(&store).unwrap().contains(&login));
    let chosen = SpaceLogin {
        space_id: "space-2".into(),
        provider: Provider::Claude,
        account_id: "monocode-2".into(),
    };
    assert!(account::logins(&store).unwrap().contains(&chosen));
    // ADE-229: a project only Space 2 used, by a session (/q) or a Mono
    // (/s), moves to Space 2. One Space 1 uses as well (/p, /t) stays
    // assigned to Space 1 and is in Space 2 through Space 2's Mono.
    let conn = store.lock_conn().unwrap();
    for (project, assigned, every) in [
        ("/q", "space-2", &["space-2"][..]),
        ("/s", "space-2", &["space-2"]),
        ("/p", "space-1", &["space-1", "space-2"]),
        ("/t", "space-1", &["space-1", "space-2"]),
    ] {
        assert_eq!(
            spaces::project_space_in(&conn, project).unwrap(),
            assigned,
            "{project} is assigned to another Space"
        );
        assert_eq!(
            spaces::project_spaces_in(&conn, project).unwrap(),
            every,
            "{project} is in other Spaces"
        );
    }
    drop(conn);
    let spawn = |session: &str| {
        let target = SpawnTarget {
            workdir: rig.root.join("no-such-folder"),
            account: Some(HarnessAccount {
                provider: "claude".into(),
                id: "default".into(),
            }),
        };
        spaces::prepare_spawn(
            &store,
            spaces::Child::Session(session),
            spaces::FIRST_SPACE,
            Some(Provider::Claude),
            NOW,
            target,
        )
        .unwrap()
        .account
        .unwrap()
        .id
    };
    assert_eq!(
        spawn("s2-a"),
        "monocode-2",
        "a Space 2 session starts on another login"
    );
    assert_eq!(
        spawn("s1-a"),
        "default",
        "a Space 1 session left the CLI's own login"
    );
    let app = mock_app(&rig.one);
    assert_eq!(
        crate::harness::provider_account_dir(app.handle(), "claude", Some("monocode-2")).unwrap(),
        Some(rig.home.join(".claude-2")),
        "the app does not run Space 2's Claude login in ~/.claude-2"
    );
    assert_eq!(
        std::fs::read_to_string(rig.home.join(".claude-2/keep")).unwrap(),
        "Space 2's made-up login"
    );
}

fn first_apps_codex_login_untouched(rig: &Rig, mtime: std::time::SystemTime) {
    let codex = rig.home.join(".codex/auth.json");
    assert_eq!(std::fs::read_to_string(&codex).unwrap(), CODEX_LOGIN);
    assert_eq!(
        std::fs::metadata(&codex).unwrap().modified().unwrap(),
        mtime
    );
    assert_eq!(
        fingerprint(&rig.home.join(".codex")).len(),
        1,
        "something was added to ~/.codex"
    );
}
