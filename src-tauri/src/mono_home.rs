//! The app's writes in a Mono's private Codex home (ADE-224). That home is in
//! its Mono's srt `allowWrite`, so an agent may plant a link anywhere in it,
//! and the app writes there outside srt. So the home and each folder below it
//! are opened without following a link (`O_NOFOLLOW`, then `openat` from the
//! folder above), a file is written to a new file beside it and renamed over
//! its name, and a link where the app writes is refused: never followed, and
//! its target never touched. Windows runs no srt, so no agent is fenced there
//! and plain paths are used.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A folder in a Mono home, reached without following a link.
pub(crate) struct Dir {
    path: PathBuf,
    #[cfg(unix)]
    fd: std::os::fd::OwnedFd,
}

impl Dir {
    /// `home`, then each of `parts` in turn, made where missing.
    pub(crate) fn open(home: &Path, parts: &[&str]) -> Result<Self, String> {
        // The folders above the home are the app's own.
        std::fs::create_dir_all(home).map_err(|e| failed(home, e))?;
        let mut dir = Dir {
            path: home.to_path_buf(),
            #[cfg(unix)]
            fd: sys::open_dir(home).map_err(|e| failed(home, e))?,
        };
        for part in parts {
            dir = dir.child(part)?;
        }
        Ok(dir)
    }

    fn child(&self, part: &str) -> Result<Self, String> {
        let path = self.path.join(checked(part)?);
        #[cfg(unix)]
        let fd = sys::child_dir(&self.fd, part).map_err(|e| failed(&path, e))?;
        #[cfg(not(unix))]
        std::fs::create_dir_all(&path).map_err(|e| failed(&path, e))?;
        Ok(Dir {
            path,
            #[cfg(unix)]
            fd,
        })
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

    /// The file `name`, `None` when there is none.
    pub(crate) fn read(&self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.path.join(checked(name)?);
        #[cfg(unix)]
        let opened = sys::open_file(&self.fd, name);
        #[cfg(not(unix))]
        let opened = File::open(&path);
        let mut file = match opened {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failed(&path, e)),
        };
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut file, &mut bytes).map_err(|e| failed(&path, e))?;
        Ok(Some(bytes))
    }

    /// Removes the entry `name` itself, never what a link names.
    pub(crate) fn remove(&self, name: &str) -> Result<(), String> {
        let path = self.path.join(checked(name)?);
        #[cfg(unix)]
        let removed = sys::remove(&self.fd, name);
        #[cfg(not(unix))]
        let removed = std::fs::remove_file(&path);
        removed.map_err(|e| failed(&path, e))
    }

    /// A new file beside `name`, filled, synced, then renamed over `name`,
    /// unless `name` is a link.
    fn replace(
        &self,
        name: &str,
        fill: impl FnOnce(&mut File) -> io::Result<()>,
    ) -> Result<(), String> {
        let path = self.path.join(checked(name)?);
        let temp = format!(".{name}.{}.tmp", uuid::Uuid::new_v4());
        #[cfg(unix)]
        let created = sys::create_new(&self.fd, &temp);
        #[cfg(not(unix))]
        let created = File::options()
            .write(true)
            .create_new(true)
            .open(self.path.join(&temp));
        let mut file = created.map_err(|e| failed(&path, e))?;
        let written = fill(&mut file).and_then(|()| file.sync_all());
        let result = written.map_err(|e| failed(&path, e)).and_then(|()| {
            if self.is_link(name)? {
                return Err(format!(
                    "Mono Codex storage has a link at {}: the app does not write through it",
                    path.display()
                ));
            }
            self.rename(&temp, name).map_err(|e| failed(&path, e))
        });
        if result.is_err() {
            let _ = self.remove(&temp);
        }
        result
    }

    fn is_link(&self, name: &str) -> Result<bool, String> {
        #[cfg(unix)]
        let found = sys::is_link(&self.fd, name);
        #[cfg(not(unix))]
        let found = match std::fs::symlink_metadata(self.path.join(name)) {
            Ok(meta) => Ok(meta.file_type().is_symlink()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        };
        found.map_err(|e| failed(&self.path.join(name), e))
    }

    #[cfg(unix)]
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        sys::rename(&self.fd, from, to)
    }

    #[cfg(not(unix))]
    fn rename(&self, from: &str, to: &str) -> io::Result<()> {
        std::fs::rename(self.path.join(from), self.path.join(to))
    }
}

/// One entry's name: another folder or the one above would leave the home.
fn checked(name: &str) -> Result<&str, String> {
    if name.contains('/') || name == ".." {
        return Err(format!("Not a name in Mono Codex storage: {name}"));
    }
    Ok(name)
}

fn failed(path: &Path, error: io::Error) -> String {
    format!("Mono Codex storage at {}: {error}", path.display())
}

#[cfg(unix)]
mod sys {
    use std::ffi::CString;
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// A folder, not a link to one.
    const DIR: libc::c_int =
        libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    /// A file the call makes: it fails on any entry already there, a link included.
    const NEW: libc::c_int =
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    /// A file, not a link to one.
    const READ: libc::c_int = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC;

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
        owned(unsafe { libc::open(path.as_ptr(), DIR) })
    }

    pub(super) fn child_dir(parent: &OwnedFd, name: &str) -> io::Result<OwnedFd> {
        let name = c(name.as_bytes())?;
        // SAFETY: an open folder and a NUL-terminated name; results are checked.
        let open = || owned(unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), DIR) });
        match open() {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // SAFETY: as above. mkdirat never follows a link at `name`; when
                // it fails, the open after it says why.
                unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) };
                open()
            }
            opened => opened,
        }
    }

    pub(super) fn create_new(dir: &OwnedFd, name: &str) -> io::Result<File> {
        let name = c(name.as_bytes())?;
        // SAFETY: an open folder and a NUL-terminated name; the result is checked.
        let fd =
            unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), NEW, 0o600 as libc::c_uint) };
        owned(fd).map(File::from)
    }

    pub(super) fn open_file(dir: &OwnedFd, name: &str) -> io::Result<File> {
        let name = c(name.as_bytes())?;
        // SAFETY: as above.
        owned(unsafe { libc::openat(dir.as_raw_fd(), name.as_ptr(), READ) }).map(File::from)
    }

    pub(super) fn is_link(dir: &OwnedFd, name: &str) -> io::Result<bool> {
        let name = c(name.as_bytes())?;
        // SAFETY: zeroed is a valid `stat`, which fstatat fills.
        let mut stat = unsafe { std::mem::zeroed::<libc::stat>() };
        // SAFETY: an open folder, a NUL-terminated name and a `stat` to fill.
        let found = done(unsafe {
            libc::fstatat(
                dir.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        });
        match found {
            Ok(()) => Ok((stat.st_mode & libc::S_IFMT) == libc::S_IFLNK),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
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

    /// Defect: a name leaves the folder it is written in.
    #[test]
    fn a_name_stays_in_its_folder() {
        for name in ["a/b", ".."] {
            assert!(checked(name).is_err(), "{name} is a name");
        }
        assert_eq!(checked("rollout-1.jsonl"), Ok("rollout-1.jsonl"));
    }
}
