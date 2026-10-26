//! D2 (ADE-42) on real temp data folders, through `SessionStore::open_at`, the
//! one place the app opens `monocode.db`: encryption off, on, and every way on
//! must refuse to open. Test builds keep the key in `test-db-key` beside the
//! database, as GigaEval builds do; `the_login_keychain_keeps_a_key` checks the
//! keychain store itself. `scripts/scenarios/app.sh d2` runs it in the real app.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use sha2::Digest;

use crate::session_store::SessionStore;

const PLAIN_HEADER: &[u8] = b"SQLite format 3\0";

fn data_folder() -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("monocode-scenario-d2-{}", uuid::Uuid::new_v4()))
        .join("com.monocode.desktop");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn open(db: &Path) -> Result<SessionStore, String> {
    SessionStore::open_at(db.to_path_buf(), 1_700_000_000_000)
}

fn add_session(store: &SessionStore, id: &str) {
    store
        .lock_conn()
        .unwrap()
        .execute(
            "INSERT INTO sessions (id, cwd, harness, model, runtime_mode, title, created_at, updated_at)
             VALUES (?1, '/tmp/d2', 'fx', 'fx:model', 'auto', 'D2 chat', 1, 1)",
            [id],
        )
        .unwrap();
}

fn sessions(store: &SessionStore) -> Vec<String> {
    let conn = store.lock_conn().unwrap();
    let mut query = conn.prepare("SELECT id FROM sessions ORDER BY id").unwrap();
    let ids = query.query_map([], |row| row.get(0)).unwrap();
    ids.map(Result::unwrap).collect()
}

fn header(db: &Path) -> Vec<u8> {
    std::fs::read(db).unwrap().into_iter().take(16).collect()
}

/// What plain SQLite, with no key, reads from the file.
fn plain_read(db: &Path) -> Result<i64, String> {
    Connection::open(db)
        .and_then(|conn| conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0)))
        .map_err(|e| e.to_string())
}

fn turn_on(data: &Path) {
    std::fs::write(data.join("db-encryption-on"), "").unwrap();
}

fn key_file(data: &Path) -> PathBuf {
    data.join("test-db-key")
}

/// Size and SHA-256 of `monocode.db`, its `-wal` and its `-shm`.
fn disk(db: &Path) -> [String; 3] {
    ["", "-wal", "-shm"].map(
        |suffix| match std::fs::read(format!("{}{suffix}", db.display())) {
            Ok(bytes) => format!("{} bytes {:x}", bytes.len(), sha2::Sha256::digest(&bytes)),
            Err(_) => "absent".into(),
        },
    )
}

/// An encrypted data folder as a killed app leaves it: the files are copied
/// while the store is open, so the last commit is only in `-wal`.
fn crash_image() -> PathBuf {
    let live = data_folder();
    turn_on(&live);
    let store = open(&live.join("monocode.db")).unwrap();
    add_session(&store, "crash-chat");
    let image = data_folder();
    for name in [
        "db-encryption-on",
        "test-db-key",
        "monocode.db",
        "monocode.db-wal",
        "monocode.db-shm",
    ] {
        std::fs::copy(live.join(name), image.join(name)).unwrap();
    }
    drop(store);
    image
}

#[test]
fn d2_the_marker_decides_and_on_never_opens_without_its_key() {
    let off = data_folder();
    let db = off.join("monocode.db");
    let store = open(&db).unwrap();
    add_session(&store, "off-chat");
    drop(store);
    assert_eq!(
        header(&db),
        PLAIN_HEADER,
        "off: the file is plaintext, as today"
    );
    assert!(
        plain_read(&db).unwrap() > 0,
        "off: plain SQLite reads the tables"
    );
    assert!(!key_file(&off).exists(), "off: no key is made");
    assert_eq!(
        sessions(&open(&db).unwrap()),
        ["off-chat"],
        "off: a reopen reads the row"
    );
    assert!(!key_file(&off).exists(), "off: a reopen makes no key");

    if cfg!(windows) {
        on_refuses_without_sqlcipher();
    } else {
        on_encrypts_and_fails_closed();
        on_encrypts_an_empty_file();
        on_refuses_a_plaintext_database(&off, &db);
        a_refused_open_leaves_the_files_as_they_were();
        #[cfg(unix)]
        on_refuses_an_unreadable_file();
    }
}

fn on_encrypts_and_fails_closed() {
    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    let store = open(&db).unwrap();
    add_session(&store, "on-chat");
    drop(store);
    let key = std::fs::read_to_string(key_file(&data)).unwrap();
    assert!(
        key.len() == 64 && key.bytes().all(|b| b.is_ascii_hexdigit()),
        "on: a new database gets a new 32-byte key"
    );
    assert_ne!(header(&db), PLAIN_HEADER, "on: the file is not plaintext");
    assert!(
        plain_read(&db)
            .unwrap_err()
            .contains("file is not a database"),
        "on: plain SQLite cannot read it"
    );
    if let Ok(cli) = std::process::Command::new("sqlite3")
        .arg(&db)
        .arg("SELECT count(*) FROM sessions;")
        .output()
    {
        assert!(
            !cli.status.success()
                && String::from_utf8_lossy(&cli.stderr).contains("file is not a database"),
            "on: the sqlite3 CLI cannot read it"
        );
    }
    assert_eq!(
        sessions(&open(&db).unwrap()),
        ["on-chat"],
        "on: the app reads it with the key"
    );
    assert_eq!(
        std::fs::read_to_string(key_file(&data)).unwrap(),
        key,
        "on: a reopen keeps the key"
    );

    let bytes = std::fs::read(&db).unwrap();
    std::fs::write(key_file(&data), "0".repeat(63) + "1").unwrap();
    let error = open(&db).err().expect("wrong key: the open fails");
    assert!(
        error.contains("does not open"),
        "wrong key: the error says so: {error}"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        bytes,
        "wrong key: nothing is written"
    );

    std::fs::remove_file(key_file(&data)).unwrap();
    let error = open(&db).err().expect("missing key: the open fails");
    assert!(
        error.contains("is encrypted, but its key is not in"),
        "missing key: {error}"
    );
    assert!(
        !key_file(&data).exists(),
        "missing key: no new key for a database that exists"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        bytes,
        "missing key: nothing is written"
    );

    std::fs::remove_file(data.join("db-encryption-on")).unwrap();
    assert!(
        open(&db).is_err(),
        "off on an encrypted database: the open fails"
    );
    assert_eq!(
        std::fs::read(&db).unwrap(),
        bytes,
        "off on an encrypted database: nothing is written"
    );

    on_refuses_a_malformed_key(&data, &db, &bytes);

    let flipped = data_folder();
    turn_on(&flipped);
    std::fs::write(key_file(&flipped), &key).unwrap();
    let mut copy = bytes;
    copy[100] ^= 1;
    std::fs::write(flipped.join("monocode.db"), copy).unwrap();
    let error = open(&flipped.join("monocode.db"))
        .err()
        .expect("one flipped byte: the open fails");
    assert!(
        error.contains("does not open"),
        "one flipped byte: the page HMAC fails: {error}"
    );
}

fn on_refuses_a_malformed_key(data: &Path, db: &Path, bytes: &[u8]) {
    turn_on(data);
    for (bad, case) in [
        ("0".repeat(63), "63 hex digits"),
        (format!("g{}", "0".repeat(63)), "a digit that is not hex"),
    ] {
        std::fs::write(key_file(data), &bad).unwrap();
        let error = open(db).err().expect("a malformed key: the open fails");
        assert!(
            error.contains("is not 64 hex digits"),
            "a malformed key ({case}): {error}"
        );
        assert_eq!(
            std::fs::read_to_string(key_file(data)).unwrap(),
            bad,
            "a malformed key ({case}): the key is not replaced"
        );
        assert_eq!(
            std::fs::read(db).unwrap(),
            bytes,
            "a malformed key ({case}): nothing is written"
        );
    }
}

fn on_encrypts_an_empty_file() {
    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    std::fs::write(&db, "").unwrap();
    let store = open(&db).expect("an empty file: on makes a new database");
    add_session(&store, "empty-chat");
    drop(store);
    assert_eq!(
        std::fs::read_to_string(key_file(&data)).unwrap().len(),
        64,
        "an empty file: it gets a new key"
    );
    assert_ne!(header(&db), PLAIN_HEADER, "an empty file: it is encrypted");
    assert_eq!(
        sessions(&open(&db).unwrap()),
        ["empty-chat"],
        "an empty file: the app reads it back with the key"
    );
}

fn on_refuses_a_plaintext_database(off: &Path, db: &Path) {
    turn_on(off);
    let error = open(db).err().expect("on over plaintext: the open fails");
    assert!(
        error.contains("is a plaintext database"),
        "on over plaintext: {error}"
    );
    assert_eq!(
        header(db),
        PLAIN_HEADER,
        "on over plaintext: the file stays plaintext"
    );
    assert!(!key_file(off).exists(), "on over plaintext: no key is made");
}

fn on_refuses_without_sqlcipher() {
    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    let error = open(&db)
        .err()
        .expect("on without SQLCipher: the open fails");
    assert!(
        error.contains("needs SQLCipher"),
        "on without SQLCipher: {error}"
    );
    assert!(!db.exists(), "on without SQLCipher: no database is made");
    assert!(
        !key_file(&data).exists(),
        "on without SQLCipher: no key is made"
    );
}

/// An unreadable file stops the open. It never counts as a missing one, so on
/// makes no key for a database that exists and never writes over a stored key.
#[cfg(unix)]
fn on_refuses_an_unreadable_file() {
    use std::os::unix::fs::PermissionsExt;
    let chmod = |path: &Path, mode: u32| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    };

    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    std::fs::write(&db, "an existing database").unwrap();
    chmod(&db, 0o000);
    let error = open(&db)
        .err()
        .expect("an unreadable database: the open fails");
    assert!(
        error.contains("Permission denied"),
        "an unreadable database: {error}"
    );
    assert!(
        !key_file(&data).exists(),
        "an unreadable database: no new key for a database that exists"
    );

    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    let key = "ab".repeat(32);
    std::fs::write(key_file(&data), &key).unwrap();
    chmod(&key_file(&data), 0o200);
    let error = open(&db).err().expect("an unreadable key: the open fails");
    chmod(&key_file(&data), 0o600);
    assert!(
        error.contains("Permission denied"),
        "an unreadable key: {error}"
    );
    assert_eq!(
        std::fs::read_to_string(key_file(&data)).unwrap(),
        key,
        "an unreadable key: the key is not replaced"
    );
    assert!(!db.exists(), "an unreadable key: no database is made");
}

/// A refused open leaves `monocode.db`, `-wal` and `-shm` byte-identical, 20
/// times of 20, from the state a killed app leaves and from a clean close.
/// Off over an encrypted crash image leaves `monocode.db` and `-wal` as they
/// were; its read-write connection rebuilds the `-shm` index from `-wal`, as
/// SQLite does on every first open, and off never reads the file beforehand.
fn a_refused_open_leaves_the_files_as_they_were() {
    let wrong = format!("{:064x}", 1);
    for run in 1..=20 {
        let image = crash_image();
        let db = image.join("monocode.db");
        let key = std::fs::read_to_string(key_file(&image)).unwrap();
        let before = disk(&db);
        assert_ne!(before[1], "absent", "run {run}: the crash image has a -wal");

        std::fs::write(key_file(&image), &wrong).unwrap();
        let error = open(&db).err().expect("a wrong key: the open fails");
        assert!(error.contains("does not open"), "run {run}: {error}");
        assert_eq!(
            disk(&db),
            before,
            "run {run}: a wrong key leaves monocode.db, -wal and -shm as they were"
        );

        std::fs::remove_file(image.join("db-encryption-on")).unwrap();
        assert!(
            open(&db).is_err(),
            "run {run}: off over an encrypted crash image fails"
        );
        let after = disk(&db);
        assert_eq!(
            after[..2],
            before[..2],
            "run {run}: off over an encrypted crash image leaves monocode.db and -wal as they were"
        );
        assert_ne!(
            after[2], "absent",
            "run {run}: off over an encrypted crash image deletes no -shm"
        );

        turn_on(&image);
        std::fs::write(key_file(&image), &key).unwrap();
        assert_eq!(
            sessions(&open(&db).unwrap()),
            ["crash-chat"],
            "run {run}: the right key reads the commit that was only in -wal"
        );
    }

    let data = data_folder();
    let db = data.join("monocode.db");
    turn_on(&data);
    drop(open(&db).unwrap());
    let before = disk(&db);
    assert_eq!(
        before[1..],
        ["absent", "absent"],
        "a clean close leaves no -wal or -shm"
    );
    std::fs::write(key_file(&data), &wrong).unwrap();
    for run in 1..=20 {
        assert!(open(&db).is_err(), "clean run {run}: a wrong key fails");
        assert_eq!(
            disk(&db),
            before,
            "clean run {run}: monocode.db is unchanged and no -wal or -shm appears"
        );
    }
}

/// The keychain store on its own, with a test item it deletes again: the item
/// a test build never writes, since it keeps the key in `test-db-key`. Linux
/// test builds use the kernel keyring (db_key.rs), so it runs there by default.
/// Elsewhere it stays ignored, so no local run writes to a person's login
/// keychain; a CI runner can run it with `--ignored`.
#[test]
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "writes a test item to the login keychain, then deletes it"
)]
fn the_login_keychain_keeps_a_key() {
    use crate::db_key::keychain;

    let db = std::env::temp_dir()
        .join(format!(
            "monocode-d2-keychain-test-{}",
            uuid::Uuid::new_v4()
        ))
        .join("monocode.db");
    assert_eq!(keychain::read(&db).unwrap(), None, "no item before");
    let key = "ab".repeat(32);
    keychain::write(&db, &key).unwrap();
    let read = keychain::read(&db);
    keychain::entry(&db).unwrap().delete_credential().unwrap();
    assert_eq!(read.unwrap(), Some(key), "the item holds the key");
    assert_eq!(keychain::read(&db).unwrap(), None, "no item after");
}
