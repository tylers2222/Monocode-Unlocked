//! ADE-224 r1 M1, r2: a Mono's private Codex home is in its spawn's srt
//! `allowWrite`, so a sandboxed agent may plant links, FIFOs and folders in it,
//! and the app copies a thread into it and restores its agent state outside srt.
//! The app must never follow a link there, never block on a FIFO it finds, and
//! leave what a link names as it was. Accepted (ADE-225): a FIFO swapped in
//! after the check still blocks, as SQLite opens without `O_NONBLOCK`. At the
//! state database's name it blocks the read-only open; at `-journal` it blocks
//! both opens, since the first read opens a hot journal read-only in either.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::codex_agent_state::{closed_agent_edges, open_state, restore_agent_edges, AgentEdge};
use crate::codex_mono_store::copy_rollouts_with_edges;

const ROLLOUT: &str = "rollout-2026-10-08T10-00-00-root.jsonl";
const SNAPSHOT: &str = ".monocode-agent-state/root.json";
const EDGES: &str = "CREATE TABLE thread_spawn_edges(parent_thread_id TEXT NOT NULL, child_thread_id TEXT PRIMARY KEY, status TEXT NOT NULL)";

/// A login with one saved thread, a Mono home, and a folder outside both
/// that holds one precious file.
struct Scratch {
    root: PathBuf,
    source: PathBuf,
    home: PathBuf,
    outside: PathBuf,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("ade224-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let (source, home, outside) = (root.join("login"), root.join("home"), root.join("outside"));
        let day = source.join("sessions/2026/10/08");
        for dir in [&day, &home, &outside] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let meta = r#"{"type":"session_meta","payload":{"id":"root"}}"#;
        std::fs::write(day.join(ROLLOUT), format!("{meta}\n")).unwrap();
        std::fs::write(outside.join("precious"), "precious").unwrap();
        Scratch {
            root,
            source,
            home,
            outside,
        }
    }

    /// A link at `at` in the home to `to`.
    fn plant(&self, at: &str, to: &Path) -> PathBuf {
        let link = self.home.join(at);
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(to, &link).unwrap();
        link
    }

    /// The thread copied into `home`, with `edges` closed agents.
    fn copy_into(&self, home: &Path, edges: &[AgentEdge]) -> Result<(), String> {
        let rollout = self.source.join("sessions/2026/10/08").join(ROLLOUT);
        copy_rollouts_with_edges(&self.source, home, "root", &[rollout], edges)
    }

    fn copy(&self) -> Result<(), String> {
        self.copy_into(&self.home, &[])
    }

    /// A saved closure of one closed agent, as a copy leaves it.
    fn save_snapshot(&self) {
        let snapshot = self.home.join(SNAPSHOT);
        std::fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        std::fs::write(
            snapshot,
            r#"[{"parent":"root","child":"agent","status":"closed"}]"#,
        )
        .unwrap();
    }

    /// Every entry outside the home, with its content.
    fn outside(&self) -> Vec<(String, String)> {
        let mut entries: Vec<(String, String)> = std::fs::read_dir(&self.outside)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read_to_string(&path).unwrap_or_default())
            })
            .collect();
        entries.sort();
        entries
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn untouched() -> Vec<(String, String)> {
    vec![("precious".to_owned(), "precious".to_owned())]
}

fn edge() -> AgentEdge {
    AgentEdge {
        parent: "root".into(),
        child: "agent".into(),
        status: "closed".into(),
    }
}

/// Defect: copying a thread into a Mono home writes through a link planted at
/// a file (the pending marker, the rollout, the agent snapshot, written or
/// removed), so a file outside the home changes, or the link blocks the copy;
/// or it goes through a link at a folder (the markers' folder, a sessions
/// folder), or succeeds without naming it.
#[test]
fn a_copy_into_a_mono_home_never_follows_a_planted_link() {
    let clean = Scratch::new();
    clean.copy().unwrap();
    assert!(clean
        .home
        .join("sessions/2026/10/08")
        .join(ROLLOUT)
        .is_file());

    let rollout = format!("sessions/2026/10/08/{ROLLOUT}");
    let files = [
        (".monocode-migrations/root", None),
        (rollout.as_str(), Some("session_meta")),
        (SNAPSHOT, None),
    ];
    for (at, kept) in files {
        let scratch = Scratch::new();
        let link = scratch.plant(at, &scratch.outside.join("precious"));
        scratch.copy().unwrap_or_else(|e| panic!("{at}: {e}"));
        assert_eq!(
            scratch.outside(),
            untouched(),
            "{at}: the copy wrote through the link"
        );
        assert!(!link.is_symlink(), "{at}: the link blocks the copy");
        match kept {
            Some(text) => assert!(std::fs::read_to_string(&link).unwrap().contains(text)),
            None => assert!(!link.exists(), "{at}: the copy left it"),
        }
    }
    for at in [".monocode-migrations", "sessions/2026"] {
        let scratch = Scratch::new();
        let link = scratch.plant(at, &scratch.outside);
        let error = scratch.copy().expect_err(at);
        assert!(error.contains(&link.display().to_string()), "{at}: {error}");
        assert_eq!(
            scratch.outside(),
            untouched(),
            "{at}: the copy wrote through the link"
        );
    }
}

/// Defect: a copy with closed agents writes their snapshot through a link
/// planted at it, or the link blocks the copy.
#[test]
fn a_snapshot_write_replaces_a_planted_link() {
    let scratch = Scratch::new();
    let link = scratch.plant(SNAPSHOT, &scratch.outside.join("precious"));
    scratch.copy_into(&scratch.home, &[edge()]).unwrap();
    assert_eq!(scratch.outside(), untouched());
    assert!(!link.is_symlink(), "the link blocks the snapshot");
    assert!(std::fs::read_to_string(&link).unwrap().contains("agent"));
}

/// Defect: a copy with no closed agents leaves a folder planted at the
/// snapshot as if it were removed, makes the snapshot folder, or fails when
/// that folder is there with no snapshot (a copy and restore before it).
#[test]
fn a_copy_without_agents_removes_only_a_snapshot_file() {
    let scratch = Scratch::new();
    scratch.copy().unwrap();
    assert!(
        !scratch.home.join(".monocode-agent-state").exists(),
        "a copy with no closed agents made their folder"
    );
    std::fs::create_dir_all(scratch.home.join(".monocode-agent-state")).unwrap();
    scratch.copy().unwrap();
    std::fs::create_dir_all(scratch.home.join(SNAPSHOT)).unwrap();
    let error = scratch.copy().expect_err("a folder at the snapshot");
    assert!(error.contains(SNAPSHOT), "{error}");
}

/// Defect: a copy into a home it makes leaves the home open to other users.
#[test]
fn a_copy_makes_a_private_home() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    let home = scratch.root.join("homes/app/login");
    scratch.copy_into(&home, &[]).unwrap();
    for dir in [
        scratch.root.join("homes"),
        home.clone(),
        home.join("sessions"),
    ] {
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "{}", dir.display());
    }
}

/// Defect: restoring agent state reads a snapshot through a link, or through a
/// link at its folder, or makes folders in a home it only reads.
#[test]
fn a_restore_never_follows_a_planted_link() {
    for at in [SNAPSHOT, ".monocode-agent-state"] {
        let scratch = Scratch::new();
        let to = match at {
            SNAPSHOT => scratch.outside.join("precious"),
            _ => scratch.outside.clone(),
        };
        let link = scratch.plant(at, &to);
        let error = restore_agent_edges(&scratch.home, "root").expect_err(at);
        assert!(error.contains(&link.display().to_string()), "{at}: {error}");
        assert_eq!(scratch.outside(), untouched(), "{at}");
        assert!(link.is_symlink(), "{at}: the restore removed the link");
    }

    let scratch = Scratch::new();
    restore_agent_edges(&scratch.home, "root").unwrap();
    assert!(
        !scratch.home.join(".monocode-agent-state").exists(),
        "a restore with nothing saved made the snapshot folder"
    );
    let missing = scratch.root.join("never-prepared");
    restore_agent_edges(&missing, "root").unwrap();
    assert!(!missing.exists(), "a restore made the home");
}

/// Defect: a FIFO an agent made at the snapshot hangs the restore on `open`,
/// or is read as a snapshot.
#[test]
fn a_restore_refuses_a_planted_fifo_without_blocking() {
    let scratch = Scratch::new();
    let fifo = scratch.home.join(SNAPSHOT);
    std::fs::create_dir_all(fifo.parent().unwrap()).unwrap();
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    // SAFETY: a NUL-terminated path.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let home = scratch.home.clone();
    let (send, done) = std::sync::mpsc::channel();
    std::thread::spawn(move || send.send(restore_agent_edges(&home, "root")));
    let error = done
        .recv_timeout(Duration::from_secs(20))
        .expect("the restore blocks on a FIFO")
        .expect_err("a FIFO read as a snapshot");
    assert!(error.contains("not a file"), "{error}");
}

/// Defect: restoring agent state opens Codex's state database through a link
/// an agent planted at its name, and writes the closure into the database it names.
#[test]
fn a_restore_refuses_a_planted_state_database_link() {
    let scratch = Scratch::new();
    scratch.save_snapshot();
    let precious = scratch.outside.join("state.sqlite");
    rusqlite::Connection::open(&precious)
        .unwrap()
        .execute_batch(EDGES)
        .unwrap();
    scratch.plant("state_9.sqlite", &precious);
    let error = restore_agent_edges(&scratch.home, "root").expect_err("a linked database");
    let rows: i64 = rusqlite::Connection::open(&precious)
        .unwrap()
        .query_row("SELECT count(*) FROM thread_spawn_edges", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "the restore wrote through the link: {error}");
}

/// Defect: when a folder an agent made at `config.toml` stops the login's link
/// from replacing it, the link's temporary name stays in the home.
#[test]
fn a_failed_config_share_leaves_no_temporary_link() {
    let scratch = Scratch::new();
    std::fs::create_dir_all(scratch.home.join("config.toml/planted")).unwrap();
    let error = crate::codex_mono_store::prepare_files(&scratch.source, &scratch.home)
        .expect_err("a folder at config.toml");
    assert!(error.contains("Codex configuration"), "{error}");
    let left: Vec<String> = std::fs::read_dir(&scratch.home)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".config.toml."))
        .collect();
    assert_eq!(left, Vec::<String>::new());
}

/// Defect: a link in a folder above the login or the home (a linked login,
/// macOS's `/var`) makes SQLite's no-follow open refuse the state database,
/// so closed agents are neither read from the login nor restored in the home.
#[test]
fn closed_agents_survive_a_linked_folder_above_the_login_and_home() {
    let scratch = Scratch::new();
    let alias = scratch.root.join("alias");
    std::os::unix::fs::symlink(&scratch.root, &alias).unwrap();
    let (source, home) = (alias.join("login"), alias.join("home"));
    let closed = "INSERT INTO thread_spawn_edges VALUES ('root', 'agent', 'closed')";
    rusqlite::Connection::open(scratch.source.join("state_5.sqlite"))
        .unwrap()
        .execute_batch(&format!("{EDGES}; {closed};"))
        .unwrap();
    let edges = closed_agent_edges(&source, "root").unwrap();
    assert_eq!(edges.len(), 1, "the closed agent was not read");
    let rollout = source.join("sessions/2026/10/08").join(ROLLOUT);
    copy_rollouts_with_edges(&source, &home, "root", &[rollout], &edges).unwrap();
    let private = rusqlite::Connection::open(scratch.home.join("state_5.sqlite")).unwrap();
    private.execute_batch(EDGES).unwrap();
    restore_agent_edges(&home, "root").unwrap();
    let status: String = private
        .query_row("SELECT status FROM thread_spawn_edges", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(status, "closed");
}

/// What an agent plants at a database name.
#[derive(Clone, Copy, Debug)]
enum Planted {
    Link,
    Folder,
    Fifo,
}

impl Planted {
    fn make(self, at: &Path, outside: &Path) {
        match self {
            Planted::Link => std::os::unix::fs::symlink(outside, at).unwrap(),
            Planted::Folder => std::fs::create_dir(at).unwrap(),
            Planted::Fifo => {
                let path = std::ffi::CString::new(at.to_str().unwrap()).unwrap();
                // SAFETY: a NUL-terminated path.
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            }
        }
    }
}

/// Defect (ADE-225 item 18): a link, folder or FIFO an agent plants as the
/// highest `state_N.sqlite` is picked. In the login, a link makes the app read
/// the database it names into the Mono snapshot, which the agent can read; in
/// the home, every restore for that Mono fails, or one hangs on the FIFO.
/// Or an older regular database is picked over the newest, or Codex's own
/// `-wal` and `-shm` beside it stop the read.
#[test]
fn closed_agents_skip_a_planted_newer_state_database() {
    for planted in [Planted::Link, Planted::Folder, Planted::Fifo] {
        let (send, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let scratch = Scratch::new();
            let secret = scratch.outside.join("state.sqlite");
            let leak = "INSERT INTO thread_spawn_edges VALUES ('root', 'secret', 'closed')";
            rusqlite::Connection::open(&secret)
                .unwrap()
                .execute_batch(&format!("{EDGES}; {leak};"))
                .unwrap();
            let older = "INSERT INTO thread_spawn_edges VALUES ('root', 'older', 'closed')";
            rusqlite::Connection::open(scratch.source.join("state_3.sqlite"))
                .unwrap()
                .execute_batch(&format!("{EDGES}; {older};"))
                .unwrap();
            // Codex running on the login, its `-wal` and `-shm` open beside it.
            let closed = "INSERT INTO thread_spawn_edges VALUES ('root', 'agent', 'closed')";
            let codex = rusqlite::Connection::open(scratch.source.join("state_5.sqlite")).unwrap();
            codex
                .execute_batch(&format!("PRAGMA journal_mode=WAL; {EDGES}; {closed};"))
                .unwrap();
            planted.make(&scratch.source.join("state_99.sqlite"), &secret);
            let edges = closed_agent_edges(&scratch.source, "root");
            let children: Vec<String> = edges.unwrap().into_iter().map(|e| e.child).collect();
            assert_eq!(children, ["agent"], "{planted:?} in the login");

            scratch.copy_into(&scratch.home, &[edge()]).unwrap();
            rusqlite::Connection::open(scratch.home.join("state_3.sqlite"))
                .unwrap()
                .execute_batch(EDGES)
                .unwrap();
            let private = rusqlite::Connection::open(scratch.home.join("state_5.sqlite")).unwrap();
            private.execute_batch(EDGES).unwrap();
            planted.make(&scratch.home.join("state_99.sqlite"), &secret);
            restore_agent_edges(&scratch.home, "root").unwrap();
            let status: String = private
                .query_row("SELECT status FROM thread_spawn_edges", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!(status, "closed", "{planted:?} in the home");
            send.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{planted:?}: failed, or blocked on the planted name"));
    }
}

/// Defect: a link swapped in at the picked database's name, after the pick,
/// is followed by the open.
#[test]
fn a_state_database_open_refuses_a_link_at_its_name() {
    let scratch = Scratch::new();
    let real = scratch.outside.join("state.sqlite");
    rusqlite::Connection::open(&real)
        .unwrap()
        .execute_batch(EDGES)
        .unwrap();
    let link = scratch.plant("state_5.sqlite", &real);
    for access in [
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    ] {
        open_state(&real, access).expect("the real database");
        assert!(
            open_state(&link, access).is_err(),
            "{access:?} followed the link"
        );
    }
}

/// Defect (ADE-225 r1 m1): a FIFO an agent plants beside the state database,
/// where SQLite opens a journal or WAL file, blocks the login read and the
/// home restore every time, or reaches SQLite instead of being refused.
#[test]
fn a_fifo_beside_the_state_database_is_refused_without_blocking() {
    for sidecar in ["-journal", "-wal", "-shm"] {
        let (send, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let scratch = Scratch::new();
            let mode = if sidecar == "-journal" {
                "DELETE"
            } else {
                "WAL"
            };
            for dir in [&scratch.source, &scratch.home] {
                let db = dir.join("state_5.sqlite");
                rusqlite::Connection::open(&db)
                    .unwrap()
                    .execute_batch(&format!("PRAGMA journal_mode={mode}; {EDGES};"))
                    .unwrap();
                // A closed WAL database may keep its `-shm`.
                let fifo = dir.join(format!("state_5.sqlite{sidecar}"));
                let _ = std::fs::remove_file(&fifo);
                Planted::Fifo.make(&fifo, &db);
            }
            scratch.save_snapshot();
            let login = closed_agent_edges(&scratch.source, "root").map(|_| ());
            send.send([login, restore_agent_edges(&scratch.home, "root")])
                .unwrap();
        });
        let results = done
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("{sidecar}: blocked on the FIFO"));
        for (side, result) in ["login", "home"].into_iter().zip(results) {
            let error = result.expect_err(side);
            assert!(
                error.contains("is not a file"),
                "{sidecar} in the {side}: {error}"
            );
        }
    }
}
