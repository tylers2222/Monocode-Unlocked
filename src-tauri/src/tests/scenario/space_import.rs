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

    /// The import stops with `code` and a message holding `says`.
    fn refused(&self, extra: &[&str], code: i32, says: &str) {
        let out = self.import(extra);
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

fn space_one(data: &Path) {
    app_folder(
        data,
        &format!(
            "INSERT OR REPLACE INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms) VALUES (1, 5000, 0, 0);
             INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('m1', 'Made-up One', 'mono', '{{\"sessionId\":\"s1-a\"}}', 0, 1, 1);
             {SESSION} VALUES ('s1-a', '/p', 'claude', 'm', 'auto', 'Made-up one', 1, 1, NULL, NULL);
             {APPROVAL} VALUES ('aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaa1', '/p', 'Made-up', 'S',
               '{{\"pros\":[],\"cons\":[]}}', '[]', 'blocking', '[]', 'm1', 'One', 1, 'open');"
        ),
    );
    write(&data.join("monos/m1/MEMORY.md"), "Space 1's made-up memory");
}

/// MonoCode 2 today: its own clock and downtime, a Mono with its chat, a
/// worker in a project only it used, an open ask with its event and outbox
/// row on its clock, an approval numbered as Space 1's, and a note.
fn space_two(data: &Path) {
    app_folder(
        data,
        &format!(
            "INSERT OR REPLACE INTO mono_clock (id, active_ms, last_wall_ms, boot_wall_ms) VALUES (1, 1000, 0, 0);
             INSERT INTO mono_downtime (id, start_wall_ms, end_wall_ms, cause, active_ms) VALUES (1, 1, 2, 'sleep', 3);
             INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at)
               VALUES ('m2', 'Made-up Two', 'mono', '{{\"sessionId\":\"s2-a\"}}', 0, 1, 1);
             {SESSION} VALUES ('s2-a', '/p', 'claude', 'm', 'auto', 'Made-up two', 1, 1, 'default', NULL);
             {SESSION} VALUES ('s2-b', '/q/', 'claude', 'm', 'auto', 'Made-up worker', 1, 1, NULL, 's2-a');
             INSERT INTO mono_transcripts (session_id) VALUES ('s2-a');
             INSERT INTO mono_blocks VALUES ('s2-a', 0, 'b0', '{{}}', 'made-up', 1, 0), ('s2-a', 1, 'b1', '{{}}', 'reply', 0, 0);
             INSERT INTO mono_asks (id, parent_actor, child_actor, parent_level, child_level, state, due_active_ms, next_due_active_ms, created_wall_ms)
               VALUES ('k2', 'mono:m2', 'session:s2-b', 2, 3, 'open', 1500, 1600, 1);
             INSERT INTO mono_ask_events (ask_id, kind, seq, at_active_ms, at_wall_ms) VALUES ('k2', 'update', 1, 1200, 1);
             INSERT INTO mono_outbox (key, ask_id, stage, recipient, at_active_ms) VALUES ('o2', 'k2', 't5', 'mono:m2', 1400);
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
    write(&data.join("monos/m2/MEMORY.md"), "Space 2's made-up memory");
    write(&data.join("checkpoints/s2-a/0.json"), "{}");
}

/// Defect: a dry run writes, or predicts other counts than an apply makes;
/// an apply writes without a complete backup, with an app running, on a
/// stale report, over a Space 1 row or folder that shares a Space 2 key, on a
/// table it has no rule for or Spaces rows MonoCode 2 made itself, or on a
/// path in the real home without `--real`; it copies the login folder
/// instead of linking it, touches the first app's `~/.codex`, leaves a Space
/// 2 row in Space 1, a project only Space 2 used in Space 1, an ask on
/// MonoCode 2's clock or an approval on its number, copies MonoCode 2's own
/// clock state, or a second apply changes anything.
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
    let (one, before) = a_failed_backup_writes_nothing(&rig);
    apply_makes_the_prediction(&rig, &plan, &one, &before);
    second_apply_changes_nothing(&rig);
    app_reads_the_rows(&rig);
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
            "space_projects /q"
        ])
    );
    assert_eq!(plan["changes"], 18);
    plan
}

/// A path in the real home, the first app's Codex login and a stale report.
fn arguments_that_stop_the_run(rig: &Rig) {
    let real = PathBuf::from(std::env::var_os(REAL_HOME).unwrap()).join(format!(
        "Library/Application Support/ade178-missing-{}",
        uuid::Uuid::new_v4()
    ));
    rig.refused(&["--space1", real.to_str().unwrap()], 2, "--real");
    let instance_one = rig.home.join(".codex");
    rig.refused(
        &["--codex-home", instance_one.to_str().unwrap()],
        1,
        "never touched",
    );
    let mut stale: Json =
        serde_json::from_slice(&std::fs::read(rig.root.join("report/merge-dry-run.json")).unwrap())
            .unwrap();
    stale["tables"]["sessions"]["rows"]["space2"] = 1.into();
    let stale_path = rig.root.join("stale.json");
    std::fs::write(&stale_path, stale.to_string()).unwrap();
    rig.refused(
        &["--report", stale_path.to_str().unwrap()],
        1,
        "the report says",
    );
}

/// A shared folder entry or row key, a table with no rule, Spaces rows
/// MonoCode 2 made, and a running app. Each is undone after its refusal.
fn data_that_stops_the_run(rig: &Rig) {
    write(&rig.one.join("monos/m2/MEMORY.md"), "another Mono's memory");
    rig.refused(&[], 1, "in both Spaces with different contents");
    std::fs::remove_dir_all(rig.one.join("monos/m2")).unwrap();
    rig.db(&rig.one)
        .execute_batch("INSERT INTO notes (id, slug, title, created_at, updated_at) VALUES ('n2', 'other', 'Other', 1, 1)")
        .unwrap();
    rig.report();
    rig.refused(&[], 1, "notes: 1 Space 2 rows share a key");
    rig.db(&rig.one)
        .execute_batch("DELETE FROM notes WHERE id = 'n2'")
        .unwrap();
    rig.report();
    for (made, says, undo) in [
        (
            "CREATE TABLE made_up_new (id TEXT PRIMARY KEY)",
            "No import rule for made_up_new",
            "DROP TABLE made_up_new",
        ),
        (
            "CREATE TABLE space_projects (project_path TEXT PRIMARY KEY, space_id TEXT NOT NULL);
             INSERT INTO space_projects VALUES ('/q', 'space-3')",
            "already has its own space_projects rows",
            "DROP TABLE space_projects",
        ),
    ] {
        rig.db(&rig.two).execute_batch(made).unwrap();
        rig.refused(&[], 1, says);
        rig.db(&rig.two).execute_batch(undo).unwrap();
    }
    let lock = std::fs::File::create(rig.one.join("scheduler.lock")).unwrap();
    lock.try_lock().unwrap();
    rig.refused(&["--apply"], 1, "quit its app first");
}

/// Space 1's files and rows as they were before any write.
fn a_failed_backup_writes_nothing(
    rig: &Rig,
) -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let one = fingerprint(&rig.one);
    let before = dump(&rig.one.join("monocode.db"));
    let not_a_folder = rig.root.join("not-a-folder");
    std::fs::write(&not_a_folder, "").unwrap();
    rig.refused(
        &["--apply", "--backup", not_a_folder.to_str().unwrap()],
        1,
        "Not a directory",
    );
    assert_eq!(
        fingerprint(&rig.one),
        one,
        "a failed backup let a write through"
    );
    (one, before)
}

/// The apply makes what the dry run predicted, after a backup that restores
/// Space 1 as it was, and links the login instead of copying it.
fn apply_makes_the_prediction(
    rig: &Rig,
    plan: &Json,
    one: &BTreeMap<String, String>,
    before: &BTreeMap<String, Vec<String>>,
) {
    let done = rig.imported(&["--apply"]);
    for key in [
        "tables",
        "kept",
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
        &dump(&backup.join("monocode.db")),
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
    assert!(backup.join("manifest.json").is_file() && backup.join("result.json").is_file());
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
        ("SELECT active_ms FROM mono_clock WHERE id = 1", 5000),
        ("SELECT count(*) FROM mono_downtime", 0),
    ] {
        assert_eq!(one_value(&conn, sql), want, "{sql}");
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
    assert_eq!(spaces::project_space(&store, "/q").unwrap(), "space-2");
    assert_eq!(spaces::project_space(&store, "/p").unwrap(), "space-1");
    let spawn = |session: &str| {
        let target = SpawnTarget {
            workdir: rig.root.join("no-such-folder"),
            account: Some(HarnessAccount {
                provider: "claude".into(),
                id: "default".into(),
            }),
        };
        spaces::prepare_spawn(&store, session, Some(Provider::Claude), NOW, target)
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
