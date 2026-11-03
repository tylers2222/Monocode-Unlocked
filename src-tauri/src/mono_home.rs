//! The app's file access in a Mono's private Codex home (ADE-224). That home
//! is in its Mono's srt `allowWrite`, so an agent may plant a link, a FIFO or a
//! folder anywhere in it, and the app works there outside srt. So the home and
//! each folder below it are opened without following a link (`O_NOFOLLOW`,
//! then `openat` from the folder above), a file is written to a new file beside
//! it and renamed over its name (which replaces a link there, never follows
//! it), a read takes only a regular file, and a link at a folder is refused.
//! Codex's state database is opened with `SQLITE_OPEN_NOFOLLOW`
//! (`codex_mono_store`). Windows runs no srt, so no agent is sandboxed there
//! and plain paths are used: the non-unix lines are single `std::fs` calls.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The largest file `Dir::read` takes: an agent closure snapshot is far smaller.
const READ_CAP: u64 = 1_048_576;

/// A folder in a Mono home, reached without following a link.
pub(crate) struct Dir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

impl Dir {
    /// `home`, then each of `parts` in turn, each made (0700) where missing.
    pub(crate) fn create(home: &Path, parts: &[&str]) -> Result<Self, String> {
        // The folders above the home are the app's own.
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder
            .recursive(true)
            .create(home)
            .map_err(|e| failed(home, e))?;
        Self::walk(home, parts, true).map_err(|e| e.to_string())
    }

    /// `home`, then each of `parts` in turn, `None` when one is missing. It
    /// makes nothing: a read must not leave folders behind.
    pub(crate) fn open(home: &Path, parts: &[&str]) -> Result<Option<Self>, String> {
        match Self::walk(home, parts, false) {
            Ok(dir) => Ok(Some(dir)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    /// Errors keep their kind and name the folder they are about.
    fn walk(home: &Path, parts: &[&str], make: bool) -> io::Result<Self> {
        #[cfg(not(unix))]
        std::fs::read_dir(home).map_err(|e| located(home, e))?;
        let mut dir = Dir {
            path: home.to_path_buf(),
            #[cfg(unix)]
            fd: sys::open_dir(home).map_err(|e| located(home, e))?,
        };
        for part in parts {
            let path = dir.path.join(part);
            checked(part).map_err(|e| located(&path, e))?;
            #[cfg(unix)]
            let fd = sys::child_dir(&dir.fd, part, make).map_err(|e| located(&path, e))?;
            #[cfg(not(unix))]
            if make {
                std::fs::create_dir_all(&path).map_err(|e| located(&path, e))?;
            }
            #[cfg(not(unix))]
            std::fs::read_dir(&path).map_err(|e| located(&path, e))?;
            dir = Dir {
                path,
                #[cfg(unix)]
                fd,
            };
        }
        Ok(dir)
    }

    /// `bytes` as the file `name`.
    pub(crate) fn write(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.replace(name, |file| file.write_all(bytes))
    }

    /// A copy of the file `from`, which is outside the home, as `name`.
    pub(crate) fn copy(&self, name: &str, from: &Path) -> Result<(), String> {
        let mut source = File::open(from).map_err(|e| failed(from, e))?;
        self.replace(name, |file| io::copy(&mut source, file).map(drop))
    }

    /// The regular file `name`, `None` when there is none. A link, a FIFO, a
    /// folder or a file over `READ_CAP` is refused, and opening never blocks.
    pub(crate) fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path.join(name);
        checked(name).map_err(|e| failed(&path, e))?;
        #[cfg(unix)]
        let opened = sys::open_file(&self.fd, name);
        #[cfg(not(unix))]
        let opened = File::open(&path);
        let file = match opened {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failed(&path, e)),
        };
        let meta = file.metadata().map_err(|e| failed(&path, e))?;
        if !meta.is_file() || meta.len() > READ_CAP {
            return Err(format!(
                "Mono Codex storage at {}: not a file of at most {READ_CAP} bytes",
                path.display()
            ));
        }
        let mut bytes = Vec::new();
        file.take(READ_CAP)
            .read_to_end(&mut bytes)
            .map_err(|e| failed(&path, e))?;
        Ok(Some(bytes))
    }

    /// Removes the entry `name` itself, never what a link names. None there is fine.
    pub(crate) fn remove(&self, name: &str) -> Result<(), String> {
        let path = self.path.join(name);
        checked(name).map_err(|e| failed(&path, e))?;
        #[cfg(unix)]
        let removed = sys::remove(&self.fd, name);
        #[cfg(not(unix))]
        let removed = std::fs::remove_file(&path);
        match removed {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(failed(&path, e)),
            _ => Ok(()),
        }
    }

    /// A new file beside `name`, filled, synced, then renamed over `name`. A
    /// link there is replaced, not followed, so a planted one cannot block it.
    fn replace(
        &self,
        name: &str,
        fill: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> Result<(), String> {
        let path = self.path.join(name);
        checked(name).map_err(|e| failed(&path, e))?;
        let temp = format!(".{name}.{}.tmp", uuid::Uuid::new_v4());
        #[cfg(unix)]
        let created = sys::create_new(&self.fd, &temp);
        #[cfg(not(unix))]
        let created = File::create_new(self.path.join(&temp));
        let mut file = created.map_err(|e| failed(&path, e))?;
        let written = fill(&mut file).and_then(|()| file.sync_all());
        drop(file);
        #[cfg(unix)]
        let renamed = written.and_then(|()| sys::rename(&self.fd, &temp, name));
        #[cfg(not(unix))]
        let renamed = written.and_then(|()| std::fs::rename(self.path.join(&temp), &path));
        if renamed.is_err() {
            let _ = self.remove(&temp);
        }
        renamed.map_err(|e| failed(&path, e))
    }
}

/// One entry's name: empty, `.`, `..` or a `/` would leave the folder.
fn checked(name: &str) -> io::Result<()> {
    if matches!(name, "" | "." | "..") || name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a name in its folder",
        ));
    }
    Ok(())
}

fn failed(path: &Path, error: io::Error) -> String {
    format!("Mono Codex storage at {}: {error}", path.display())
}

fn located(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), failed(path, error))
}

#[cfg(unix)]
mod sys {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    use libc::{O_CLOEXEC, O_CREAT, O_DIRECTORY, O_EXCL, O_NOFOLLOW, O_NONBLOCK, O_RDONLY, O_WRONLY};

    /// A folder, not a link to one.
    const DIR: [libc::c_int; 4] = [O_RDONLY, O_DIRECTORY, O_NOFOLLOW, O_CLOEXEC];
    /// A file the call makes: it fails on any entry already there, a link included.
    const NEW: [libc::c_int; 5] = [O_WRONLY, O_CREAT, O_EXCL, O_NOFOLLOW, O_CLOEXEC];
    /// Not a link, and a FIFO opens without waiting for a writer.
    const READ: [libc::c_int; 4] = [O_RDONLY, O_NOFOLLOW, O_NONBLOCK, O_CLOEXEC];

    /// The open flags `set`, together. A fold, not `|`: the flags are disjoint
    /// bits, so a `|` swapped for `^` would be an equivalent mutant.
    fn flags(set: &[libc::c_int]) -> libc::c_int {
        set.iter().copied().fold(0, std::ops::BitOr::bitor)
    }

    fn c(name: &[u8]) -> io::Result<CString> {
        CString::new(name).map_err(|_| io::ErrorKind::InvalidInput.into())
    }

    fn owned(fd: libc::c_int) -> io::Result<OwnedFd> {
        if fd == -1 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a descriptor the call just opened, owned by nothing else.
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }

    fn done(result: libc::c_int) -> io::Result<()> {
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub(super) fn open_dir(path: &Path) -> io::Result<OwnedFd> {
        let path = c(path.as_os_str().as_bytes())?;
        // SAFETY: a NUL-terminated path; the result is checked.
        owned(unsafe { libc::open(path.as_ptr(), flags(&DIR)) })
    }

    /// The folder `name` in `parent`, made (0700) when missing and `make`.
    pub(super) fn child_dir(parent: &OwnedFd, name: &str, make: bool) -> io::Result<OwnedFd> {
        let name = c(name.as_bytes())?;
        // SAFETY: an open folder and a NUL-terminated name; results are checked.
        let open =
            || owned(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags(&DIR)) });
        match open() {
            Err(e) if make && e.kind() == io::ErrorKind::NotFound => {
                // SAFETY: as above. mkdirat never follows a link at `name`.
                let made = done(unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) });
                // Made by another meanwhile, it opens; else mkdirat's error says why.
                open().map_err(|e| made.err().unwrap_or(e))
            }
            opened => opened,
        }
    }

    pub(super) fn create_new(dir: &OwnedFd, name: &str) -> io::Result<File> {
        let name = c(name.as_bytes())?;
        // SAFETY: an open folder and a NUL-terminated name; the result is checked.
        let fd = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                name.as_ptr(),
                flags(&NEW),
                0o600 as libc::c_uint,
            )
        };
        owned(fd).map(File::from)
    }

    pub(super) fn open_file(dir: &OwnedFd, name: &str) -> io::Result<File> {
        let name = c(name.as_bytes())?;
        // SAFETY: as above.
        owned(unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), flags(&READ)) })
            .map(File::from)
    }

    pub(super) fn rename(dir: &OwnedFd, from: &str, to: &str) -> io::Result<()> {
        let (from, to) = (c(from.as_bytes())?, c(to.as_bytes())?);
        let dir = dir.as_raw_fd();
        // SAFETY: an open folder and NUL-terminated names. renameat replaces a
        // link at `to` and never follows it.
        done(unsafe { libc::renameat(dir, from.as_ptr(), dir, to.as_ptr()) })
    }

    pub(super) fn remove(dir: &OwnedFd, name: &str) -> io::Result<()> {
        let name = c(name.as_bytes())?;
        // SAFETY: an open folder and a NUL-terminated name.
        done(unsafe { libc::unlinkat(dir.as_raw_fd(), name.as_ptr(), 0) })
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Defect: a name leaves the folder it is written in, or is the folder.
    #[test]
    fn a_name_stays_in_its_folder() {
        for name in ["a/b", "..", ".", ""] {
            assert!(checked(name).is_err(), "{name:?} is a name");
        }
        assert!(checked("rollout-1.jsonl").is_ok());
    }

    /// Defect: `read` takes a file over `READ_CAP`, or refuses one of exactly it.
    #[test]
    fn a_read_takes_a_file_up_to_its_cap() {
        let root = std::env::temp_dir().join(format!("ade224-cap-{}", uuid::Uuid::new_v4()));
        let dir = Dir::create(&root, &[]).unwrap();
        for (name, len) in [("at-cap", READ_CAP), ("over-cap", READ_CAP + 1)] {
            File::create(root.join(name)).unwrap().set_len(len).unwrap();
        }
        let read = dir.read("at-cap");
        let over = dir.read("over-cap");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(read.unwrap().map(|bytes| bytes.len() as u64), Some(READ_CAP));
        assert!(over.unwrap_err().contains("at most"));
    }
}
