//! Database encryption (ADE-42), off unless `<app data>/db-encryption-on`
//! exists. Off, `monocode.db` opens as it always did and no key is read or
//! made. On, a new database is made with SQLCipher under a random 32-byte key
//! that the app keeps in the login keychain (service `MonoCode DB key`,
//! account the data folder's name), and an encrypted database opens only with
//! that key. On never opens a plaintext database (moving one over is ADE-43)
//! and never makes a new key for a database that exists: a wrong or missing
//! key stops the app.

use std::io::{ErrorKind, Read};
use std::path::Path;

use rusqlite::{Connection, OptionalExtension};

const ON_MARKER: &str = "db-encryption-on";
const PLAIN_HEADER: &[u8; 16] = b"SQLite format 3\0";

#[cfg(any(test, feature = "test-db-key"))]
use key_file as store;
#[cfg(not(any(test, feature = "test-db-key")))]
use keychain as store;

/// A 32-byte key as the 64 hex digits `PRAGMA key` takes. No `Debug`, so it
/// never reaches a log.
pub(crate) struct DbKey(String);

impl DbKey {
    fn random() -> Result<Self, String> {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| format!("Database encryption: no random key: {e}"))?;
        let hex = bytes
            .iter()
            .flat_map(|byte| [byte >> 4, byte & 0xf])
            .map(|digit| char::from(DIGITS[usize::from(digit)]))
            .collect();
        Ok(Self(hex))
    }

    fn parse(text: String) -> Result<Self, String> {
        if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(text))
        } else {
            Err(format!(
                "Database encryption: the key in {} is not 64 hex digits",
                store::NAME
            ))
        }
    }
}

enum Header {
    /// No file, or an empty one: SQLite has not written it yet.
    New,
    Plain,
    Encrypted,
}

fn header(db: &Path) -> Result<Header, String> {
    let mut first = Vec::with_capacity(PLAIN_HEADER.len());
    match std::fs::File::open(db) {
        Ok(file) => file.take(PLAIN_HEADER.len() as u64).read_to_end(&mut first),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Header::New),
        Err(e) => Err(e),
    }
    .map_err(|e| format!("Database encryption: {}: {e}", db.display()))?;
    Ok(match first.as_slice() {
        [] => Header::New,
        header if header == PLAIN_HEADER => Header::Plain,
        _ => Header::Encrypted,
    })
}

/// The key `monocode.db` at `db` opens with; `None` while the marker is
/// absent, so the database opens as it always did.
pub(crate) fn key_for(db: &Path) -> Result<Option<DbKey>, String> {
    let marker = db.with_file_name(ON_MARKER);
    let on = std::fs::exists(&marker)
        .map_err(|e| format!("Database encryption: {}: {e}", marker.display()))?;
    if !on {
        return Ok(None);
    }
    require_sqlcipher()?;
    match header(db)? {
        Header::Plain => Err(format!(
            "Database encryption is on, but {} is a plaintext database, and moving it over is not built yet (ADE-43). Remove {ON_MARKER} from the app data folder to open it.",
            db.display()
        )),
        Header::New => match stored_key(db)? {
            Some(key) => Ok(Some(key)),
            None => {
                let key = DbKey::random()?;
                store::write(db, &key.0)?;
                Ok(Some(key))
            }
        },
        Header::Encrypted => stored_key(db)?.map(Some).ok_or_else(|| {
            format!(
                "Database encryption: {} is encrypted, but its key is not in {}. The app never makes a new key for a database that exists.",
                db.display(),
                store::NAME
            )
        }),
    }
}

fn stored_key(db: &Path) -> Result<Option<DbKey>, String> {
    store::read(db)?.map(DbKey::parse).transpose()
}

/// Plain SQLite takes `PRAGMA key` and writes plaintext anyway, so on stops
/// unless the linked library is SQLCipher (the macOS build).
fn require_sqlcipher() -> Result<(), String> {
    let version: Option<String> = Connection::open_in_memory()
        .and_then(|conn| {
            conn.query_row("PRAGMA cipher_version", [], |row| row.get(0))
                .optional()
        })
        .map_err(|e| e.to_string())?;
    if version.is_some() {
        Ok(())
    } else {
        Err(format!("Database encryption needs SQLCipher, which only the macOS build has. Remove {ON_MARKER} from the app data folder to open the database."))
    }
}

/// Opens `db`, keyed when `key` is set; a key that does not open it is an
/// error, never a plaintext connection.
pub(crate) fn connect(db: &Path, key: Option<&DbKey>) -> Result<Connection, String> {
    let conn = Connection::open(db).map_err(|e| e.to_string())?;
    let Some(DbKey(hex)) = key else {
        return Ok(conn);
    };
    conn.execute_batch(&format!("PRAGMA key = \"x'{hex}'\";"))
        .map_err(|e| format!("Database encryption: {e}"))?;
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
        .map_err(|e| {
            format!(
                "Database encryption: the key in {} does not open {}: {e}",
                store::NAME,
                db.display()
            )
        })?;
    Ok(conn)
}

#[cfg(any(test, not(feature = "test-db-key")))]
pub(crate) mod keychain {
    use std::path::Path;

    pub(crate) const NAME: &str = "the login keychain";
    pub(crate) const SERVICE: &str = "MonoCode DB key";

    /// One item per data folder, so each MonoCode build keeps its own key.
    pub(crate) fn entry(db: &Path) -> Result<keyring::Entry, String> {
        let account = db
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .ok_or("Database encryption: the data folder has no name")?;
        keyring::Entry::new(SERVICE, account)
            .map_err(|e| format!("Database encryption: {NAME}: {e}"))
    }

    pub(crate) fn read(db: &Path) -> Result<Option<String>, String> {
        match entry(db)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(format!("Database encryption: reading {NAME}: {e}")),
        }
    }

    pub(crate) fn write(db: &Path, key: &str) -> Result<(), String> {
        entry(db)?
            .set_password(key)
            .map_err(|e| format!("Database encryption: writing {NAME}: {e}"))
    }
}

#[cfg(any(test, feature = "test-db-key"))]
mod key_file {
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};

    pub(super) const NAME: &str = "test-db-key";

    fn path(db: &Path) -> PathBuf {
        db.with_file_name(NAME)
    }

    pub(super) fn read(db: &Path) -> Result<Option<String>, String> {
        match std::fs::read_to_string(path(db)) {
            Ok(key) => Ok(Some(key.trim_end().to_owned())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("Database encryption: {NAME}: {e}")),
        }
    }

    pub(super) fn write(db: &Path, key: &str) -> Result<(), String> {
        std::fs::write(path(db), key).map_err(|e| format!("Database encryption: {NAME}: {e}"))
    }
}
