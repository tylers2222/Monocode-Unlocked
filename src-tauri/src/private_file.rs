//! Config files the app writes for a harness child to read at startup.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Writes `<dir>/<prefix>-<content hash>.json`, owner-only. Named by content
/// so a running child's file is never rewritten in place.
pub(crate) fn write_once(dir: &Path, prefix: &str, contents: &str) -> Result<PathBuf, String> {
    let mut hasher = DefaultHasher::new();
    contents.hash(&mut hasher);
    let path = dir.join(format!("{prefix}-{:016x}.json", hasher.finish()));
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let temp = dir.join(format!(".{}-{stamp}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&temp)
        .and_then(|mut file| file.write_all(contents.as_bytes()))
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    if let Err(error) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        // Another spawn wrote the same content first.
        if !path.is_file() {
            return Err(format!("{}: {error}", path.display()));
        }
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_each_config_once_by_content() {
        let dir =
            std::env::temp_dir().join(format!("monocode-private-file-{}", std::process::id()));
        let first = write_once(&dir, "a", "{}").unwrap();
        assert_eq!(write_once(&dir, "a", "{}").unwrap(), first);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "{}");
        assert_ne!(write_once(&dir, "a", "{\"a\":1}").unwrap(), first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
