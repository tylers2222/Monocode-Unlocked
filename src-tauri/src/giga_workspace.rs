//! The shared Giga workspace at `$HOME/MonoCode`: a git repo every Giga Mono
//! reads, and writes only inside its own `gigas/<slug>/`.
//!
//! Every operation here converges: it can run twice, or after a crash at any
//! step, and ends in the same state. Nothing here may fail a Giga's turn, so
//! the commands report problems in their result instead of returning `Err`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(crate) const MANAGED_START: &str = "<!-- monocode:managed:start -->";
pub(crate) const MANAGED_END: &str = "<!-- monocode:managed:end -->";
const HOOK_MARKER: &str = "# monocode:managed pre-commit";
const LOCK_FILE: &str = "monocode-giga.lock";
const APP_SLUG: &str = "__app__";
const LOCK_WAIT: Duration = Duration::from_secs(30);
/// The root refresh runs before a turn, so it never waits long: a busy lock
/// leaves the commit to the next run.
const ROOT_LOCK_WAIT: Duration = Duration::from_secs(2);
/// Git leaves `index.lock` behind when it is killed mid-commit. Older than
/// this, no git process can still own it.
const STALE_GIT_INDEX_LOCK: Duration = Duration::from_secs(120);
const GIGA_DIRS: [&str; 6] = [
    "briefs", "runbooks", "projects", "monos", "state", "scratch",
];
/// Folders git would drop when empty get a `.keep` when they are created.
const KEPT_DIRS: [&str; 4] = ["briefs", "runbooks", "projects", "monos"];

const ROOT_AGENTS: &str = r#"# ~/MonoCode — the shared Giga workspace

Every Giga Mono works here. A Giga is a Mono that can create and configure other Monos. This folder is where Gigas keep what they need between turns, and where they can see what other Gigas did.

## Layout
```
AGENTS.md               this file
shared/                 conventions every Giga follows (add files; do not edit another Giga's notes)
index/                  derived, app-maintained: a search index (git-ignored)
gigas/<giga-slug>/      one folder per Giga
  AGENTS.md             who this Giga is and what it is working on now
  log.jsonl             receipts, one line per turn, written by the app
  briefs/               every brief it sent to a Mono or session
  runbooks/             scripts it reuses
  projects/<slug>/      notes per project it coordinates
  monos/                templates and a register of Monos it created
  state/                JSON / SQLite working state (git-ignored)
  scratch/              throwaway (git-ignored)
```

## Rules
1. Read anything. Write only inside your own `gigas/<your-slug>/`.
2. Before you redo work, search `index/` and the other Gigas' `log.jsonl` and `briefs/`.
3. Never write secrets (keys, tokens, passwords) into any file here. This folder is a git repo.
4. A coordinator edits few files. Its record is its receipts: who it asked, and the ID that came back (message, session, commit).
5. Long work goes to a visible session or to the Mono that owns the project. Do not run it inside your own turn.

"#;

const MANAGED_BODY: &str = r#"(The app keeps this block up to date. Text outside it is never overwritten.)

## What the app does
- Before every Giga turn and habit run, the app creates any missing folder or file of this layout and of your `gigas/<slug>/`. It never overwrites a file, except the text in this block.
- After every Giga turn and habit run, the app appends one receipt line to `gigas/<slug>/log.jsonl`:
  `{"v":1,"at":<ms>,"giga":{"monoId":"…","slug":"…"},"sessionId":"…","kind":"chat|habit","asked":"<first 200 chars>","outcome":"completed|failed|cancelled","sent":[{"requestId":"app-…","toMonoId":"…"}],"started":[{"requestId":"app-…","sessionId":"…"}],"files":["gigas/<slug>/briefs/…"]}`
  `files` lists the files in `gigas/<slug>/` that the turn added or changed, except `log.jsonl`, `state/` and `scratch/`.
- When the turn changed a file other than `log.jsonl`, the app commits `gigas/<slug>/` as `giga(<slug>): <outcome> turn <session>`, without secret-looking files. A turn that only added its receipt makes no commit: its line waits in `log.jsonl` and goes into the next commit. After every turn the app rebuilds `index/index.db` from every `log.jsonl` and `briefs/` file, so waiting receipts can be searched too.
- The pre-commit hook lets a Giga commit only its own `gigas/<slug>/` and new files in `shared/`, and refuses secret-looking files. The app sets `MONOCODE_GIGA_SLUG` for each Giga, so your own commits follow the same rules.

## Messages between Monos
- `app monos.send` is removed. Message another Mono with `app messages.send` (`message`, `send_to`, `wanting_reply`, `intent`; `intent:"steer"` with `reply_to` replaces `whenBusy`, and `action_type` is gone) and answer a message that wants a reply with `app messages.reply`. `intent:"steer"` with `"scope":"turn"` instead of `reply_to` (a Giga to a Mono only) changes everything a Mono is doing now, in its own running turn only; never pass both. Where `shared/CONVENTIONS.md` still names `monos.send` or `action_type`, this line wins.

## Search before you redo work
`index/index.db` has one SQLite FTS5 table, `entries(source, giga, path, at, text)`. `source` is `log` or `brief`.
```sh
sqlite3 ~/MonoCode/index/index.db "SELECT giga, path, snippet(entries, 4, '[', ']', '…', 12) FROM entries WHERE entries MATCH 'release notes' ORDER BY rank LIMIT 10"
```
"#;

const GITIGNORE: &str = "index/\ngigas/*/state/\ngigas/*/scratch/\n.DS_Store\n";

const CONVENTIONS: &str = r#"# Conventions for Gigas

- Every spin-up names its harness and model (exact IDs from `app models.list`).
- Messages to other Monos: `app messages.send` with `message`, `send_to`, `wanting_reply` (true only when you need an answer) and `intent`. `intent:"new"` starts new work and waits for a busy recipient's turn to end; `intent:"steer"` with `reply_to` changes the running work that message started, and `intent:"steer"` with `"scope":"turn"` (a Giga to a Mono only) changes everything a Mono is doing now. Answer a message that wants a reply with `app messages.reply`.
- Briefs: `# Target`, `# Change`, `# Acceptance`, written plainly. Save each one in your `briefs/`.
- To add a convention, add a new file in `shared/`. Do not edit a file another Giga wrote.
"#;

pub(crate) const PRE_COMMIT: &str = r#"#!/bin/sh
# monocode:managed pre-commit
# MonoCode installs and updates this hook. Replace this file with your own
# hook to stop MonoCode updating it.
slug="${MONOCODE_GIGA_SLUG-}"
tab=$(printf '\t')
fail=0
reject() {
  printf 'monocode pre-commit: %s: %s\n' "$1" "$2" >&2
  fail=1
}
case "$slug" in
  "" | __app__) ;;
  *[!a-z0-9-]* | -*)
    reject "$slug" "MONOCODE_GIGA_SLUG is not a Giga slug"
    exit 1
    ;;
esac
changes=$(git -c core.quotePath=false diff --cached --name-status --no-renames)
while IFS="$tab" read -r status path; do
  [ -n "$status" ] || continue
  case "$path" in
    '"'*)
      reject "$path" "unsupported path name"
      continue
      ;;
  esac
  if [ "$status" != D ]; then
    name=$(printf '%s' "${path##*/}" | tr 'A-Z' 'a-z')
    case "$name" in
      *.pem | *.key | *.p12 | id_rsa* | id_ed25519* | .env | .env.* | *credentials* | *secret* | *.token)
        reject "$path" "secret-looking file name"
        continue
        ;;
    esac
    if git show ":$path" 2>/dev/null | grep -Eq -e '-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----' -e '(^|[^A-Za-z0-9_-])(ghp_|gho_|github_pat_|sk-|xoxb-|AKIA)[A-Za-z0-9_-]{16,}'; then
      reject "$path" "secret-looking content"
      continue
    fi
  fi
  if [ "$slug" = __app__ ]; then
    case "$path" in
      AGENTS.md | .gitignore) ;;
      *) reject "$path" "the app commits only AGENTS.md and .gitignore" ;;
    esac
    continue
  fi
  case "$path" in
    shared/*)
      [ "$status" = A ] || reject "$path" "files under shared/ can be added, not changed or deleted"
      ;;
    gigas/*)
      if [ -z "$slug" ]; then
        reject "$path" "commits under gigas/ need MONOCODE_GIGA_SLUG"
      else
        case "$path" in
          "gigas/$slug/"*) ;;
          *) reject "$path" "a Giga commits only gigas/$slug/" ;;
        esac
      fi
      ;;
    *)
      [ -z "$slug" ] || reject "$path" "a Giga commits only gigas/$slug/ and new files under shared/"
      ;;
  esac
done <<EOF
$changes
EOF
exit "$fail"
"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RootCheck {
    /// `MonoCode` is missing or is a folder (directly or through a link in `$HOME`).
    Usable,
    Blocked(&'static str),
}

/// Whether the app may write at `root`. A file, a dangling link and a link
/// out of `home` are left exactly as they are.
pub(crate) fn check_root(home: &Path, root: &Path) -> RootCheck {
    let Ok(meta) = fs::symlink_metadata(root) else {
        return RootCheck::Usable;
    };
    if meta.file_type().is_symlink() {
        let (Ok(target), Ok(home)) = (fs::canonicalize(root), fs::canonicalize(home)) else {
            return RootCheck::Blocked("~/MonoCode is a link that does not resolve");
        };
        if !target.starts_with(&home) {
            return RootCheck::Blocked("~/MonoCode is a link to a folder outside your home folder");
        }
        if !target.is_dir() {
            return RootCheck::Blocked("~/MonoCode is a link to a file, not a folder");
        }
        return RootCheck::Usable;
    }
    if meta.is_dir() {
        RootCheck::Usable
    } else {
        RootCheck::Blocked("~/MonoCode is a file, not a folder")
    }
}

pub(crate) fn managed_block() -> String {
    format!("{MANAGED_START}\n{MANAGED_BODY}{MANAGED_END}")
}

/// `existing` with only the text between the managed markers replaced. The
/// last start marker and the first end marker after it bound the block, so a
/// stray marker in the user's text never swallows that text. Without a
/// complete block, a new one is appended and every other byte is kept.
pub(crate) fn refresh_managed_block(existing: &str, block: &str) -> String {
    if let Some(start) = existing.rfind(MANAGED_START) {
        if let Some(end) = existing[start..].find(MANAGED_END) {
            let end = start + end + MANAGED_END.len();
            return format!("{}{block}{}", &existing[..start], &existing[end..]);
        }
    }
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(block);
    out.push('\n');
    out
}

/// The pre-commit hook's file-name rule.
pub(crate) fn is_secret_name(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    [".pem", ".key", ".p12", ".token"]
        .iter()
        .any(|ext| name.ends_with(ext))
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name == ".env"
        || name.starts_with(".env.")
        || name.contains("credentials")
        || name.contains("secret")
}

/// The pre-commit hook's content rule.
pub(crate) fn is_secret_content(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .any(|line| has_private_key_header(line) || has_token(line))
}

fn has_private_key_header(line: &str) -> bool {
    let mut rest = line;
    while let Some(at) = rest.find("-----BEGIN ") {
        let after = &rest[at + "-----BEGIN ".len()..];
        let label_len = after
            .find(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' '))
            .unwrap_or(after.len());
        if after[..label_len].ends_with("PRIVATE KEY") && after[label_len..].starts_with("-----") {
            return true;
        }
        rest = after;
    }
    false
}

fn has_token(line: &str) -> bool {
    let bytes = line.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    for prefix in ["ghp_", "gho_", "github_pat_", "sk-", "xoxb-", "AKIA"] {
        let mut from = 0;
        while let Some(at) = line[from..].find(prefix) {
            let at = from + at;
            let tail = &bytes[at + prefix.len()..];
            let run = tail.iter().take_while(|&&b| word(b)).count();
            if (at == 0 || !word(bytes[at - 1])) && run >= 16 {
                return true;
            }
            from = at + 1;
        }
    }
    false
}

/// A slug the app can put in a path: what `gigaSlug` produces.
pub(crate) fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 48
        && !slug.starts_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub(crate) fn commit_subject(slug: &str, outcome: &str, session_id: &str) -> String {
    let short: String = session_id.chars().take(8).collect();
    format!("giga({slug}): {outcome} turn {short}")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GigaFolder {
    pub slug: String,
    pub name: String,
}

#[derive(Debug, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnsureReport {
    pub root: String,
    /// `ready` or `blocked`.
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Paths relative to the root, in the order they were created.
    pub created: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Something the user should know that did not stop the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

pub(crate) fn workspace_root(home: &Path) -> PathBuf {
    home.join("MonoCode")
}

/// Creates whatever is missing of the workspace and of each Giga's folder,
/// refreshes the managed block and the managed hook, and commits the app's
/// own root files. Running it again changes nothing.
pub(crate) fn ensure(home: &Path, gigas: &[GigaFolder]) -> EnsureReport {
    let root = workspace_root(home);
    let mut report = EnsureReport {
        root: crate::fs::path_to_js(&root),
        state: "ready",
        ..EnsureReport::default()
    };
    if let RootCheck::Blocked(reason) = check_root(home, &root) {
        report.state = "blocked";
        report.reason = Some(reason.to_string());
        return report;
    }
    if let Err(error) = ensure_tree(&root, gigas, &mut report) {
        report.error = Some(error);
        return report;
    }
    match install_hook(&root) {
        Ok(HookState::Foreign) => {
            report.warning = Some(
                ".git/hooks/pre-commit is not MonoCode's, so it was left as is and does not check Giga commits"
                    .into(),
            );
        }
        Ok(HookState::Created) => report.created.push(".git/hooks/pre-commit".into()),
        Ok(HookState::Current | HookState::Updated) => {}
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    }
    if let Err(error) = commit_root(&root) {
        report.error = Some(error);
    }
    report
}

fn ensure_tree(root: &Path, gigas: &[GigaFolder], report: &mut EnsureReport) -> Result<(), String> {
    let created = &mut report.created;
    ensure_dir(root, "", created)?;
    if !root.join(".git").exists() {
        git(root, &["init", "--quiet"], &[])?;
        created.push(".git".into());
    }
    let agents = root.join("AGENTS.md");
    match fs::read_to_string(&agents) {
        Ok(existing) => {
            let next = refresh_managed_block(&existing, &managed_block());
            if next != existing {
                write_atomic(&agents, next.as_bytes())?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_new(
                root,
                "AGENTS.md",
                &format!("{ROOT_AGENTS}{}\n", managed_block()),
                created,
            )?;
        }
        Err(error) => return Err(format!("Could not read {}: {error}", agents.display())),
    }
    write_new(root, ".gitignore", GITIGNORE, created)?;
    if ensure_dir(root, "shared", created)? {
        write_new(root, "shared/.keep", "", created)?;
    }
    write_new(root, "shared/CONVENTIONS.md", CONVENTIONS, created)?;
    ensure_dir(root, "index", created)?;
    ensure_dir(root, "gigas", created)?;
    for giga in gigas {
        let base = format!("gigas/{}", giga.slug);
        ensure_dir(root, &base, created)?;
        let name = giga.name.lines().next().unwrap_or("").trim();
        write_new(
            root,
            &format!("{base}/AGENTS.md"),
            &format!(
                "# {name} (Giga)\n\nWho this Giga is and what it is working on now. The Giga keeps this file up to date.\n"
            ),
            created,
        )?;
        write_new(root, &format!("{base}/log.jsonl"), "", created)?;
        for dir in GIGA_DIRS {
            let rel = format!("{base}/{dir}");
            if ensure_dir(root, &rel, created)? && KEPT_DIRS.contains(&dir) {
                write_new(root, &format!("{rel}/.keep"), "", created)?;
            }
        }
    }
    // The rules tell a Giga to query the index in its first turn.
    if !root.join("index/index.db").exists() {
        rebuild_index(root)?;
        created.push("index/index.db".into());
    }
    Ok(())
}

/// Returns whether it created the folder.
fn ensure_dir(root: &Path, rel: &str, created: &mut Vec<String>) -> Result<bool, String> {
    let path = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    if path.is_dir() {
        return Ok(false);
    }
    fs::create_dir_all(&path)
        .map_err(|error| format!("Could not create {}: {error}", path.display()))?;
    if !rel.is_empty() {
        created.push(format!("{rel}/"));
    }
    Ok(true)
}

/// Writes `rel` only when nothing is there.
fn write_new(
    root: &Path,
    rel: &str,
    content: &str,
    created: &mut Vec<String>,
) -> Result<(), String> {
    let path = root.join(rel);
    match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            file.write_all(content.as_bytes())
                .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
            created.push(rel.to_string());
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(format!("Could not write {}: {error}", path.display())),
    }
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension(format!("monocode-{}.tmp", std::process::id()));
    fs::write(&tmp, content)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|error| {
            let _ = fs::remove_file(&tmp);
            format!("Could not write {}: {error}", path.display())
        })
}

#[derive(Debug, PartialEq)]
enum HookState {
    Created,
    Updated,
    Current,
    Foreign,
}

fn install_hook(root: &Path) -> Result<HookState, String> {
    let hooks = root.join(".git/hooks");
    let path = hooks.join("pre-commit");
    let state = match fs::read_to_string(&path) {
        Ok(existing) if existing == PRE_COMMIT && is_executable(&path) => {
            return Ok(HookState::Current)
        }
        Ok(existing) if existing.lines().nth(1) == Some(HOOK_MARKER) => HookState::Updated,
        Ok(_) => return Ok(HookState::Foreign),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => HookState::Created,
        // Not text, so not ours.
        Err(_) if path.exists() => return Ok(HookState::Foreign),
        Err(error) => return Err(format!("Could not read {}: {error}", path.display())),
    };
    fs::create_dir_all(&hooks)
        .map_err(|error| format!("Could not create {}: {error}", hooks.display()))?;
    write_atomic(&path, PRE_COMMIT.as_bytes())?;
    set_executable(&path)?;
    Ok(state)
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 == 0o111)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    true
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("Could not make {} executable: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Commits the app's own root files when they differ from `HEAD`, and new
/// files under `shared/`. Checking git, not what this run wrote, also commits
/// what a run that crashed before its commit left behind.
fn commit_root(root: &Path) -> Result<(), String> {
    let changed = changed_paths(root, &["AGENTS.md", ".gitignore", "shared/"])?;
    let app: Vec<&str> = changed
        .iter()
        .filter(|change| change.path == "AGENTS.md" || change.path == ".gitignore")
        .map(|change| change.path.as_str())
        .collect();
    let shared: Vec<&str> = changed
        .iter()
        .filter(|change| change.untracked && change.path.starts_with("shared/"))
        .map(|change| change.path.as_str())
        .filter(|path| !is_secret_name(path))
        .collect();
    if app.is_empty() && shared.is_empty() {
        return Ok(());
    }
    let _lock = WorkspaceLock::acquire(root, ROOT_LOCK_WAIT)?;
    if !app.is_empty() {
        commit_paths(
            root,
            &app,
            APP_SLUG,
            "MonoCode",
            "app@monocode.local",
            "Refresh workspace root",
        )?;
    }
    if !shared.is_empty() {
        commit_paths(
            root,
            &shared,
            "",
            "MonoCode",
            "app@monocode.local",
            "Add shared workspace files",
        )?;
    }
    Ok(())
}

struct Change {
    path: String,
    untracked: bool,
}

fn changed_paths(root: &Path, pathspecs: &[&str]) -> Result<Vec<Change>, String> {
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
        "--",
    ];
    args.extend_from_slice(pathspecs);
    let out = git(root, &args, &[])?;
    Ok(out
        .split('\0')
        .filter(|entry| entry.len() > 3)
        .map(|entry| Change {
            path: entry[3..].to_string(),
            untracked: entry.starts_with("??"),
        })
        .collect())
}

/// Stages exactly `paths` (additions, changes and deletions) and commits only
/// them, so whatever else is staged stays staged and out of this commit.
fn commit_paths(
    root: &Path,
    paths: &[&str],
    slug: &str,
    author: &str,
    email: &str,
    subject: &str,
) -> Result<Option<String>, String> {
    let env: &[(&str, &str)] = &[("MONOCODE_GIGA_SLUG", slug)];
    let mut add = vec!["add", "-A", "--"];
    add.extend_from_slice(paths);
    git(root, &add, env)?;
    let name = format!("user.name={author}");
    let mail = format!("user.email={email}");
    let mut commit = vec![
        "-c", &name, "-c", &mail, "commit", "--quiet", "-m", subject, "--only", "--",
    ];
    commit.extend_from_slice(paths);
    git(root, &commit, env)?;
    git(root, &["rev-parse", "HEAD"], &[]).map(|sha| Some(sha.trim().to_string()))
}

fn git(root: &Path, args: &[&str], env: &[(&str, &str)]) -> Result<String, String> {
    let mut command = Command::new("git");
    crate::hide_window_console(&mut command);
    command.arg("-C").arg(root).args(args);
    command.env_remove("MONOCODE_GIGA_SLUG");
    for (key, value) in env {
        if !value.is_empty() {
            command.env(key, value);
        }
    }
    let output = command
        .output()
        .map_err(|error| format!("Could not run git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git failed: {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// `.git/monocode-giga.lock`. The OS lock on the file serializes the app's
/// own writers, even two in one process. The PID written inside also keeps
/// out any other writer while that process lives; a dead PID is taken over.
/// The file is emptied, never deleted, on release: deleting it would let a
/// waiter that already opened it and a newcomer both hold a lock.
pub(crate) struct WorkspaceLock {
    file: File,
}

impl WorkspaceLock {
    pub(crate) fn acquire(root: &Path, wait: Duration) -> Result<Self, String> {
        let path = root.join(".git").join(LOCK_FILE);
        let deadline = Instant::now() + wait;
        loop {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
            if file.try_lock().is_ok() {
                let mut owner = String::new();
                let _ = file.read_to_string(&mut owner);
                let held = owner
                    .trim()
                    .parse::<u32>()
                    .is_ok_and(|pid| pid != std::process::id() && pid_alive(pid));
                if !held {
                    file.set_len(0)
                        .and_then(|()| file.seek(SeekFrom::Start(0)))
                        .and_then(|_| file.write_all(std::process::id().to_string().as_bytes()))
                        .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
                    return Ok(Self { file });
                }
                let _ = file.unlock();
            }
            if Instant::now() >= deadline {
                return Err(format!("Timed out waiting for {}", path.display()));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for WorkspaceLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let result = unsafe { libc::kill(pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Without a cheap liveness check, only the OS lock counts.
#[cfg(not(unix))]
fn pid_alive(_: u32) -> bool {
    false
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEnd {
    pub slug: String,
    /// One JSON line, without the newline.
    pub receipt: String,
    pub outcome: String,
    pub session_id: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnEndReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Appends the receipt, commits the Giga's folder when the turn changed more
/// than its log, and rebuilds the index, all under the workspace lock. A
/// receipt alone waits in `log.jsonl` for the next commit.
pub(crate) fn turn_end(home: &Path, turn: &TurnEnd) -> TurnEndReport {
    let root = workspace_root(home);
    let result = (|| {
        if let RootCheck::Blocked(reason) = check_root(home, &root) {
            return Err(reason.to_string());
        }
        if !root.join(".git").is_dir() {
            return Err("~/MonoCode is not a git repo yet".to_string());
        }
        let _lock = WorkspaceLock::acquire(&root, LOCK_WAIT)?;
        append_receipt(
            &root.join("gigas").join(&turn.slug).join("log.jsonl"),
            &turn.receipt,
        )?;
        let commit = commit_giga(&root, &turn.slug, &turn.outcome, &turn.session_id);
        let index = rebuild_index(&root);
        let commit = commit?;
        index?;
        Ok(commit)
    })();
    match result {
        Ok(commit) => TurnEndReport {
            commit,
            error: None,
        },
        Err(error) => TurnEndReport {
            commit: None,
            error: Some(error),
        },
    }
}

/// Appends `line` unless the log already ends with it, so a retried turn end
/// adds its receipt once.
fn append_receipt(log: &Path, line: &str) -> Result<(), String> {
    let line = line.trim_end_matches('\n');
    let mut file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(log)
        .map_err(|error| format!("Could not open {}: {error}", log.display()))?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let tail_len = (line.len() as u64 + 2).min(len);
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(len - tail_len))
        .and_then(|_| file.read_to_end(&mut tail))
        .map_err(|error| format!("Could not read {}: {error}", log.display()))?;
    let tail = String::from_utf8_lossy(&tail);
    if tail.trim_end_matches('\n').ends_with(line)
        && (tail.len() as u64 == len || tail.trim_end_matches('\n').len() > line.len())
    {
        return Ok(());
    }
    let prefix = if len > 0 && !tail.ends_with('\n') {
        "\n"
    } else {
        ""
    };
    file.write_all(format!("{prefix}{line}\n").as_bytes())
        .map_err(|error| format!("Could not write {}: {error}", log.display()))
}

/// A file of the Giga's own work: anything in `gigas/<slug>/` except the
/// receipt log and the git-ignored `state/` and `scratch/`.
pub(crate) fn is_work_file(slug: &str, path: &str) -> bool {
    let Some(rest) = path
        .strip_prefix("gigas/")
        .and_then(|rest| rest.strip_prefix(slug))
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return false;
    };
    rest != "log.jsonl" && !rest.starts_with("state/") && !rest.starts_with("scratch/")
}

/// Content hash by path, relative to `~/MonoCode`, of every work file of a Giga.
pub type Snapshot = BTreeMap<String, String>;

pub(crate) fn work_snapshot(root: &Path, slug: &str) -> Snapshot {
    let base = format!("gigas/{slug}");
    let mut paths = Vec::new();
    collect_files(&root.join(&base), &base, &mut paths);
    paths
        .into_iter()
        .filter(|path| is_work_file(slug, path))
        .filter_map(|path| {
            let bytes = fs::read(root.join(&path)).ok()?;
            Some((path, format!("{:x}", Sha256::digest(bytes))))
        })
        .collect()
}

/// The work files a turn added or changed, sorted: new in `after`, or with
/// other content than in `before`.
pub(crate) fn turn_files(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    after
        .iter()
        .filter(|(path, hash)| before.get(*path) != Some(hash))
        .map(|(path, _)| path.clone())
        .collect()
}

fn commit_giga(
    root: &Path,
    slug: &str,
    outcome: &str,
    session_id: &str,
) -> Result<Option<String>, String> {
    clear_stale_git_index_lock(root);
    let base = format!("gigas/{slug}/");
    let paths: Vec<String> = changed_paths(root, &[&base])?
        .into_iter()
        .map(|change| change.path)
        .filter(|path| {
            !is_secret_name(path)
                && !fs::read(root.join(path)).is_ok_and(|bytes| is_secret_content(&bytes))
        })
        .collect();
    if !paths.iter().any(|path| is_work_file(slug, path)) {
        return Ok(None);
    }
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    commit_paths(
        root,
        &paths,
        slug,
        &format!("MonoCode Giga {slug}"),
        &format!("giga+{slug}@monocode.local"),
        &commit_subject(slug, outcome, session_id),
    )
}

fn clear_stale_git_index_lock(root: &Path) {
    let lock = root.join(".git/index.lock");
    let old = fs::metadata(&lock)
        .and_then(|meta| meta.modified())
        .is_ok_and(|at| at.elapsed().unwrap_or_default() > STALE_GIT_INDEX_LOCK);
    if old {
        let _ = fs::remove_file(lock);
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct IndexEntry {
    pub source: &'static str,
    pub giga: String,
    pub path: String,
    pub at: i64,
    pub text: String,
}

/// Every receipt and every brief, in a fixed order, read from the files only:
/// the same files always give the same entries.
pub(crate) fn index_entries(root: &Path) -> Vec<IndexEntry> {
    let mut entries = Vec::new();
    let mut gigas: Vec<_> = fs::read_dir(root.join("gigas"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    gigas.sort();
    for giga in gigas {
        let base = format!("gigas/{giga}");
        let log_path = format!("{base}/log.jsonl");
        if let Ok(log) = fs::read(root.join(&log_path)) {
            for line in String::from_utf8_lossy(&log)
                .lines()
                .filter(|line| !line.trim().is_empty())
            {
                let at = serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|value| value.get("at").and_then(serde_json::Value::as_i64))
                    .unwrap_or(0);
                entries.push(IndexEntry {
                    source: "log",
                    giga: giga.clone(),
                    path: log_path.clone(),
                    at,
                    text: line.to_string(),
                });
            }
        }
        let mut briefs = Vec::new();
        collect_files(
            &root.join(&base).join("briefs"),
            &format!("{base}/briefs"),
            &mut briefs,
        );
        briefs.sort();
        for path in briefs {
            let full = root.join(&path);
            let Ok(bytes) = fs::read(&full) else { continue };
            let at = fs::metadata(&full)
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map(|time| time.as_millis() as i64)
                .unwrap_or(0);
            entries.push(IndexEntry {
                source: "brief",
                giga: giga.clone(),
                path,
                at,
                text: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
    }
    entries
}

fn collect_files(dir: &Path, rel: &str, out: &mut Vec<String>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".keep" {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = format!("{rel}/{name}");
        if kind.is_dir() {
            collect_files(&entry.path(), &path, out);
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

/// A full rebuild into a new file that then replaces `index/index.db`, so a
/// reader never sees half an index and a crash leaves the old one in place.
pub(crate) fn rebuild_index(root: &Path) -> Result<(), String> {
    let dir = root.join("index");
    fs::create_dir_all(&dir)
        .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".index-{}-{nonce}.db", std::process::id()));
    let result = (|| {
        let mut db = rusqlite::Connection::open(&tmp).map_err(|error| error.to_string())?;
        db.execute_batch(
            "CREATE VIRTUAL TABLE entries USING fts5(source, giga, path, at UNINDEXED, text);",
        )
        .map_err(|error| error.to_string())?;
        let tx = db.transaction().map_err(|error| error.to_string())?;
        {
            let mut insert = tx
                .prepare("INSERT INTO entries (source, giga, path, at, text) VALUES (?1, ?2, ?3, ?4, ?5)")
                .map_err(|error| error.to_string())?;
            for entry in index_entries(root) {
                insert
                    .execute(rusqlite::params![
                        entry.source,
                        entry.giga,
                        entry.path,
                        entry.at,
                        entry.text
                    ])
                    .map_err(|error| error.to_string())?;
            }
        }
        tx.commit().map_err(|error| error.to_string())?;
        db.close().map_err(|(_, error)| error.to_string())?;
        fs::rename(&tmp, dir.join("index.db")).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|error| format!("Could not rebuild index/index.db: {error}"))
}

/// The Giga slug each agent process gets as `MONOCODE_GIGA_SLUG`, by session.
static SESSION_SLUGS: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, String>>,
> = std::sync::LazyLock::new(Default::default);

/// Gives a Giga's agent process its slug, so its own commits follow the hook:
/// the slug bound to the session (the Giga's chat or a habit run), else the
/// slug of the Giga whose chat started the session, when the session runs in
/// that Giga's folder.
pub(crate) fn configure_child(
    store: &crate::session_store::SessionStore,
    home: Option<&Path>,
    session_id: &str,
    workdir: &Path,
    cmd: &mut Command,
) {
    cmd.env_remove("MONOCODE_GIGA_SLUG");
    let bound = SESSION_SLUGS
        .lock()
        .ok()
        .and_then(|slugs| slugs.get(session_id).cloned());
    // A session a Giga's chat started through `sessions.start` has the id
    // `app-<chat>-<request>`.
    let slug = bound.or_else(|| {
        let started = session_id.strip_prefix("app-")?;
        let slug = folder_slug(&workspace_root(home?), workdir)?;
        let conn = store.lock_conn().ok()?;
        let monos = crate::mono_roster::list(&conn).ok()?;
        monos
            .iter()
            .any(|mono| {
                mono.role == crate::mono_roster::MonoRole::Giga
                    && mono.data["gigaSlug"] == slug.as_str()
                    && mono.data["sessionId"]
                        .as_str()
                        .and_then(|chat| started.strip_prefix(chat))
                        .is_some_and(|rest| rest.starts_with('-'))
            })
            .then_some(slug)
    });
    if let Some(slug) = slug {
        cmd.env("MONOCODE_GIGA_SLUG", slug);
    }
}

/// The first folder under `gigas/` that holds `workdir`, if any.
fn folder_slug(root: &Path, workdir: &Path) -> Option<String> {
    let gigas = fs::canonicalize(root.join("gigas")).ok()?;
    let workdir = fs::canonicalize(workdir).ok()?;
    let slug = workdir.strip_prefix(gigas).ok()?.components().next()?;
    slug.as_os_str().to_str().map(str::to_string)
}

#[tauri::command]
pub fn giga_workspace_bind(session_id: String, slug: String) -> Result<(), String> {
    if !is_valid_slug(&slug) {
        return Err(format!("Not a Giga slug: {slug:?}"));
    }
    let mut slugs = SESSION_SLUGS.lock().map_err(|error| error.to_string())?;
    slugs.insert(session_id, slug);
    Ok(())
}

fn home() -> Result<PathBuf, String> {
    crate::dirs_home()
        .map(PathBuf::from)
        .ok_or_else(|| "No home folder".to_string())
}

#[tauri::command(async)]
pub fn giga_workspace_ensure(gigas: Vec<GigaFolder>) -> EnsureReport {
    let home = match home() {
        Ok(home) => home,
        Err(error) => {
            return EnsureReport {
                state: "blocked",
                reason: Some(error),
                ..EnsureReport::default()
            }
        }
    };
    if let Some(bad) = gigas.iter().find(|giga| !is_valid_slug(&giga.slug)) {
        return EnsureReport {
            root: crate::fs::path_to_js(&workspace_root(&home)),
            state: "ready",
            error: Some(format!("Not a Giga slug: {:?}", bad.slug)),
            ..EnsureReport::default()
        };
    }
    ensure(&home, &gigas)
}

#[tauri::command(async)]
pub fn giga_workspace_turn_end(turn: TurnEnd) -> TurnEndReport {
    if !is_valid_slug(&turn.slug) {
        return TurnEndReport {
            commit: None,
            error: Some(format!("Not a Giga slug: {:?}", turn.slug)),
        };
    }
    match home() {
        Ok(home) => turn_end(&home, &turn),
        Err(error) => TurnEndReport {
            commit: None,
            error: Some(error),
        },
    }
}

/// The work files a turn added or changed since `before`, its start. Without
/// a snapshot (the app did not see the turn start), the work files that
/// differ from `HEAD`.
pub(crate) fn files_since(root: &Path, slug: &str, before: Option<&Snapshot>) -> Vec<String> {
    let after = work_snapshot(root, slug);
    match before {
        Some(before) => turn_files(before, &after),
        None => changed_paths(root, &[&format!("gigas/{slug}/")])
            .unwrap_or_default()
            .into_iter()
            .map(|change| change.path)
            .filter(|path| after.contains_key(path))
            .collect(),
    }
}

fn giga_root(slug: &str) -> Option<PathBuf> {
    let home = home().ok()?;
    let root = workspace_root(&home);
    (is_valid_slug(slug) && matches!(check_root(&home, &root), RootCheck::Usable)).then_some(root)
}

#[tauri::command(async)]
pub fn giga_workspace_turn_start(slug: String) -> Option<Snapshot> {
    giga_root(&slug).map(|root| work_snapshot(&root, &slug))
}

#[tauri::command(async)]
pub fn giga_workspace_turn_files(slug: String, before: Option<Snapshot>) -> Vec<String> {
    giga_root(&slug)
        .map(|root| files_since(&root, &slug, before.as_ref()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    struct TempHome(PathBuf);

    impl TempHome {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mc-giga-{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).unwrap();
            Self(fs::canonicalize(dir).unwrap())
        }
        fn root(&self) -> PathBuf {
            workspace_root(&self.0)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn giga(slug: &str) -> GigaFolder {
        GigaFolder {
            slug: slug.into(),
            name: format!("Giga {slug}"),
        }
    }

    /// Path to content (or `<dir>`) for every entry under `root`, except
    /// git's object store, which changes for reasons unrelated to the tree.
    fn snapshot(root: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
            for entry in fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if rel == ".git/objects" || rel == ".git/index" || rel == ".git/logs" {
                    continue;
                }
                if path.is_dir() {
                    out.insert(rel, "<dir>".into());
                    walk(root, &path, out);
                } else {
                    out.insert(
                        rel,
                        String::from_utf8_lossy(&fs::read(&path).unwrap()).into_owned(),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(root, root, &mut out);
        out
    }

    fn log_subjects(root: &Path) -> Vec<String> {
        git(root, &["log", "--format=%s"], &[])
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    // Defect: a missing piece of the layout, or a missing commit of the root.
    #[test]
    fn ensure_creates_the_full_tree_from_nothing() {
        let home = TempHome::new("create");
        let report = ensure(&home.0, &[giga("alpha")]);
        assert_eq!((report.state, &report.error), ("ready", &None));
        let root = home.root();
        for rel in [
            "AGENTS.md",
            ".gitignore",
            "shared/CONVENTIONS.md",
            "shared/.keep",
            "index",
            "gigas/alpha/AGENTS.md",
            "gigas/alpha/log.jsonl",
            "gigas/alpha/briefs/.keep",
            "gigas/alpha/runbooks/.keep",
            "gigas/alpha/projects/.keep",
            "gigas/alpha/monos/.keep",
            "gigas/alpha/state",
            "gigas/alpha/scratch",
            ".git/hooks/pre-commit",
        ] {
            assert!(root.join(rel).exists(), "missing {rel}");
        }
        let entries: i64 = rusqlite::Connection::open(root.join("index/index.db"))
            .unwrap()
            .query_row("SELECT count(*) FROM entries", [], |row| row.get(0))
            .unwrap();
        assert_eq!(entries, 0, "a new workspace's index can be searched");
        let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
        assert!(agents.contains(MANAGED_START) && agents.contains(MANAGED_END));
        assert_eq!(
            fs::read_to_string(root.join(".gitignore")).unwrap(),
            GITIGNORE
        );
        assert_eq!(
            fs::read_to_string(root.join("gigas/alpha/AGENTS.md"))
                .unwrap()
                .lines()
                .next(),
            Some("# Giga alpha (Giga)")
        );
        assert_eq!(
            git(
                &root,
                &[
                    "status",
                    "--porcelain",
                    "--",
                    "AGENTS.md",
                    ".gitignore",
                    "shared/"
                ],
                &[]
            )
            .unwrap(),
            "",
            "the root files are committed"
        );
    }

    // Defect: a second run rewrites, recreates or commits something.
    #[test]
    fn ensure_twice_changes_nothing() {
        let home = TempHome::new("twice");
        ensure(&home.0, &[giga("alpha"), giga("beta")]);
        let before = snapshot(&home.root());
        let commits = log_subjects(&home.root());
        let again = ensure(&home.0, &[giga("alpha"), giga("beta")]);
        assert_eq!(again.created, Vec::<String>::new());
        assert_eq!(again.error, None);
        assert_eq!(snapshot(&home.root()), before);
        assert_eq!(log_subjects(&home.root()), commits);
    }

    // Defect: ensure only builds a tree from nothing, and never heals one.
    #[test]
    fn ensure_restores_deleted_agents_md_and_giga_subtree() {
        let home = TempHome::new("restore");
        ensure(&home.0, &[giga("alpha")]);
        let root = home.root();
        let before = snapshot(&root);
        fs::remove_file(root.join("AGENTS.md")).unwrap();
        fs::remove_dir_all(root.join("gigas/alpha")).unwrap();
        let report = ensure(&home.0, &[giga("alpha")]);
        assert!(report.created.contains(&"AGENTS.md".to_string()));
        assert!(report.created.contains(&"gigas/alpha/".to_string()));
        let after = snapshot(&root);
        let paths = |map: &BTreeMap<String, String>| {
            map.keys()
                .filter(|key| !key.starts_with(".git"))
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&after), paths(&before));
        assert_eq!(after["AGENTS.md"], before["AGENTS.md"]);
    }

    // Defect: the refresh overwrites the user's text, or leaves the block stale.
    #[test]
    fn refresh_keeps_user_text_outside_the_managed_block() {
        let home = TempHome::new("refresh");
        let root = home.root();
        fs::create_dir_all(&root).unwrap();
        let mine = format!("# Mine\n\nmy notes\n\n{MANAGED_START}\nold app text\n{MANAGED_END}\n\n## After\nmore of mine\n");
        fs::write(root.join("AGENTS.md"), &mine).unwrap();
        ensure(&home.0, &[]);
        let refreshed = fs::read_to_string(root.join("AGENTS.md")).unwrap();
        assert_eq!(
            refreshed,
            format!(
                "# Mine\n\nmy notes\n\n{}\n\n## After\nmore of mine\n",
                managed_block()
            )
        );

        fs::write(root.join("AGENTS.md"), "no markers here").unwrap();
        ensure(&home.0, &[]);
        assert_eq!(
            fs::read_to_string(root.join("AGENTS.md")).unwrap(),
            format!("no markers here\n\n{}\n", managed_block())
        );
    }

    // Defect: a stray marker in the user's text makes refreshes eat that text
    // or append a block on every run.
    #[test]
    fn refresh_converges_with_stray_markers() {
        let block = managed_block();
        for start in [
            format!("intro\n{MANAGED_END}\nmine\n"),
            format!("intro\n{MANAGED_START}\nmine\n"),
        ] {
            let once = refresh_managed_block(&start, &block);
            assert!(once.contains("mine"));
            assert_eq!(refresh_managed_block(&once, &block), once);
        }
    }

    // Defect: the app writes into a file, or follows a link out of $HOME.
    #[cfg(unix)]
    #[test]
    fn root_file_or_outside_link_is_blocked_and_untouched() {
        let home = TempHome::new("file");
        fs::write(home.root(), "not a folder").unwrap();
        let report = ensure(&home.0, &[giga("alpha")]);
        assert_eq!(report.state, "blocked");
        assert!(report.reason.is_some());
        assert_eq!(fs::read_to_string(home.root()).unwrap(), "not a folder");

        let home = TempHome::new("link");
        let outside = TempHome::new("outside");
        std::os::unix::fs::symlink(&outside.0, home.root()).unwrap();
        let report = ensure(&home.0, &[giga("alpha")]);
        assert_eq!(report.state, "blocked");
        assert_eq!(
            fs::read_dir(&outside.0).unwrap().count(),
            0,
            "nothing written through the link"
        );
        assert!(turn_end(&home.0, &turn("alpha", "{}")).error.is_some());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    }

    // Defect: a link inside $HOME, a normal dotfiles setup, is refused.
    #[cfg(unix)]
    #[test]
    fn link_inside_home_is_used() {
        let home = TempHome::new("inner-link");
        fs::create_dir_all(home.0.join("Sync/MonoCode")).unwrap();
        std::os::unix::fs::symlink(home.0.join("Sync/MonoCode"), home.root()).unwrap();
        assert_eq!(ensure(&home.0, &[]).state, "ready");
        assert!(home.0.join("Sync/MonoCode/AGENTS.md").exists());
    }

    // Defect: ensure replaces a hook the user installed.
    #[test]
    fn foreign_hook_is_kept_and_reported() {
        let home = TempHome::new("hook");
        let root = home.root();
        fs::create_dir_all(root.join(".git/hooks")).unwrap();
        git(&root, &["init", "--quiet"], &[]).unwrap();
        fs::write(root.join(".git/hooks/pre-commit"), "#!/bin/sh\necho mine\n").unwrap();
        let report = ensure(&home.0, &[]);
        assert!(report.warning.is_some());
        assert_eq!(
            fs::read_to_string(root.join(".git/hooks/pre-commit")).unwrap(),
            "#!/bin/sh\necho mine\n"
        );
        fs::write(
            root.join(".git/hooks/pre-commit"),
            format!("#!/bin/sh\n{HOOK_MARKER}\nold\n"),
        )
        .unwrap();
        assert_eq!(ensure(&home.0, &[]).warning, None);
        assert_eq!(
            fs::read_to_string(root.join(".git/hooks/pre-commit")).unwrap(),
            PRE_COMMIT
        );
    }

    // Defect: an unwritable workspace is reported as ready.
    #[cfg(unix)]
    #[test]
    fn write_error_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let home = TempHome::new("readonly");
        fs::create_dir_all(home.root()).unwrap();
        fs::set_permissions(home.root(), fs::Permissions::from_mode(0o555)).unwrap();
        let report = ensure(&home.0, &[giga("alpha")]);
        fs::set_permissions(home.root(), fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(report.state, "ready");
        assert!(report.error.is_some());
    }

    /// A repo with the hook and one committed file under each folder.
    fn hook_repo(name: &str) -> TempHome {
        let home = TempHome::new(name);
        ensure(&home.0, &[giga("alpha"), giga("beta")]);
        let root = home.root();
        fs::write(root.join("shared/old.md"), "old").unwrap();
        git(&root, &["add", "shared/old.md"], &[]).unwrap();
        git(
            &root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "seed",
                "--no-verify",
            ],
            &[],
        )
        .unwrap();
        home
    }

    fn try_commit(root: &Path, slug: &str, files: &[(&str, &str)]) -> Result<String, String> {
        for (rel, content) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
            git(root, &["add", rel], &[]).unwrap();
        }
        let result = git(
            root,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-qm",
                "try",
            ],
            &[("MONOCODE_GIGA_SLUG", slug)],
        );
        let _ = git(root, &["reset", "-q", "--hard", "HEAD"], &[]);
        let _ = git(root, &["clean", "-qfd", "--", "gigas", "shared"], &[]);
        result
    }

    // Defect: the hook blocks a Giga's own commits.
    #[test]
    fn hook_lets_a_giga_commit_its_own_subtree() {
        let home = hook_repo("hook-own");
        assert_eq!(
            try_commit(
                &home.root(),
                "alpha",
                &[("gigas/alpha/briefs/b.md", "plan")]
            ),
            Ok(String::new())
        );
    }

    // Defect: the hook lets one Giga overwrite another's folder.
    #[test]
    fn hook_refuses_another_gigas_subtree() {
        let home = hook_repo("hook-other");
        let error =
            try_commit(&home.root(), "alpha", &[("gigas/beta/briefs/b.md", "plan")]).unwrap_err();
        assert!(
            error.contains("monocode pre-commit: gigas/beta/briefs/b.md:"),
            "{error}"
        );
    }

    // Defect: a commit without MONOCODE_GIGA_SLUG writes into a Giga's folder.
    #[test]
    fn hook_refuses_gigas_without_a_slug() {
        let home = hook_repo("hook-noslug");
        assert!(try_commit(&home.root(), "", &[("gigas/alpha/briefs/b.md", "plan")]).is_err());
    }

    // Defect: a session a Giga's chat starts in its folder cannot commit
    // there, or another process in a Giga's folder gets that Giga's slug.
    #[test]
    fn only_a_session_its_chat_started_commits_as_that_giga() {
        let home = hook_repo("hook-folder");
        let root = home.root();
        fs::create_dir_all(root.join("gigas/alpha/work")).unwrap();
        let store = crate::session_store::SessionStore::open_in_memory().unwrap();
        {
            let conn = store.lock_conn().unwrap();
            for (id, role, data) in [
                (
                    "ga",
                    "giga",
                    serde_json::json!({"mascot": "skull", "gigaSlug": "alpha", "sessionId": "chat-a"}),
                ),
                (
                    "gb",
                    "giga",
                    serde_json::json!({"mascot": "crab", "gigaSlug": "beta", "sessionId": "chat-b"}),
                ),
                (
                    "m",
                    "mono",
                    serde_json::json!({"mascot": "owl", "sessionId": "chat-m", "projects": [root.join("gigas/beta")]}),
                ),
            ] {
                let mono = serde_json::from_value(
                    serde_json::json!({"id": id, "role": role, "data": data, "sortOrder": 0}),
                )
                .unwrap();
                crate::mono_roster::create(
                    &conn,
                    &crate::mono_roster::validate_create(mono).unwrap(),
                )
                .unwrap();
            }
        }
        let slug_of = |session: &str, dir: &str| {
            let mut cmd = Command::new("true");
            configure_child(&store, Some(&home.0), session, &root.join(dir), &mut cmd);
            cmd.get_envs()
                .find(|(key, _)| *key == "MONOCODE_GIGA_SLUG")
                .and_then(|(_, value)| value)
                .map(|value| value.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        assert_eq!(slug_of("app-chat-a-r1", "gigas/alpha"), "alpha");
        assert_eq!(slug_of("app-chat-a-r1", "gigas/alpha/work"), "alpha");
        assert_eq!(slug_of("app-chat-b-r1", "gigas/alpha/../beta"), "beta");
        assert_eq!(slug_of("app-chat-a-r1", "gigas/alpha/../beta"), "");
        assert_eq!(slug_of("app-chat-a-r1", ""), "");
        assert_eq!(slug_of("user-session", "gigas/alpha"), "");
        assert_eq!(slug_of("app-app-chat-a-r1-r2", "gigas/alpha"), "");
        // Giga A creates a Mono with project gigas/beta; that Mono's session
        // runs there and must not commit as Giga B.
        assert_eq!(slug_of("app-chat-m-r1", "gigas/beta"), "");
        assert_eq!(
            try_commit(
                &root,
                &slug_of("app-chat-a-r1", "gigas/alpha"),
                &[("gigas/alpha/work/a.md", "a")]
            ),
            Ok(String::new())
        );
        assert!(try_commit(
            &root,
            &slug_of("app-chat-a-r1", "gigas/alpha"),
            &[("gigas/beta/briefs/b.md", "b")]
        )
        .is_err());
        assert!(try_commit(
            &root,
            &slug_of("app-chat-m-r1", "gigas/beta"),
            &[("gigas/beta/briefs/m.md", "m")]
        )
        .is_err());
    }

    // Defect: shared/ is either closed to new files or open to edits.
    #[test]
    fn hook_allows_added_shared_files_and_refuses_modified_ones() {
        let home = hook_repo("hook-shared");
        for slug in ["", "alpha"] {
            assert_eq!(
                try_commit(&home.root(), slug, &[("shared/new.md", "new")]),
                Ok(String::new())
            );
            let _ = git(&home.root(), &["reset", "-q", "--hard", "HEAD~1"], &[]);
            let error =
                try_commit(&home.root(), slug, &[("shared/old.md", "changed")]).unwrap_err();
            assert!(
                error.contains("shared/old.md: files under shared/ can be added"),
                "{error}"
            );
        }
    }

    // Defect: the app's own commit can carry a Giga's files.
    #[test]
    fn hook_limits_the_app_to_root_files() {
        let home = hook_repo("hook-app");
        assert!(try_commit(&home.root(), "__app__", &[("gigas/alpha/briefs/b.md", "x")]).is_err());
        assert!(try_commit(&home.root(), "__app__", &[(".gitignore", "index/\n")]).is_ok());
    }

    const SECRET_CASES: [(&str, &str, bool); 10] = [
        (
            "gigas/alpha/briefs/plan.md",
            "a plan that mentions ghp_ and sk- in prose\ntask-runner-without-a-token\n",
            false,
        ),
        ("gigas/alpha/briefs/server.pem", "x", true),
        ("gigas/alpha/briefs/.env", "x", true),
        ("gigas/alpha/briefs/.env.local", "x", true),
        ("gigas/alpha/briefs/My-Credentials.json", "x", true),
        ("gigas/alpha/briefs/id_ed25519.pub", "x", true),
        (
            "gigas/alpha/briefs/key.txt",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n",
            true,
        ),
        (
            "gigas/alpha/briefs/gh.md",
            "token: ghp_abcdefghijklmnopqrstuvwxyz0123\n",
            true,
        ),
        ("gigas/alpha/briefs/aws.md", "AKIAABCDEFGHIJKLMNOP\n", true),
        (
            "gigas/alpha/briefs/oa.md",
            "export KEY=sk-proj-abcdefghijklmnop\n",
            true,
        ),
    ];

    // Defect: a secret reaches the repo, or the app and the hook disagree on
    // what a secret is (then the app's commit fails on a file it staged).
    #[test]
    fn hook_and_app_agree_on_secret_looking_files() {
        let home = hook_repo("hook-secret");
        for (rel, content, secret) in SECRET_CASES {
            let app = is_secret_name(rel) || is_secret_content(content.as_bytes());
            assert_eq!(app, secret, "app on {rel}");
            let hook = try_commit(&home.root(), "alpha", &[(rel, content)]);
            assert_eq!(hook.is_err(), secret, "hook on {rel}: {hook:?}");
            if let Err(error) = hook {
                assert!(
                    error.contains(&format!("monocode pre-commit: {rel}: secret-looking")),
                    "{error}"
                );
            }
        }
    }

    fn turn(slug: &str, receipt: &str) -> TurnEnd {
        TurnEnd {
            slug: slug.into(),
            receipt: receipt.into(),
            outcome: "completed".into(),
            session_id: "0123456789abcdef".into(),
        }
    }

    // Defect: the turn-end commit takes the wrong files, misses the subject
    // or author, or appends a retried receipt twice.
    #[test]
    fn turn_end_commits_only_the_gigas_folder_without_secrets() {
        let home = TempHome::new("turn");
        ensure(&home.0, &[giga("alpha"), giga("beta")]);
        let root = home.root();
        fs::write(root.join("gigas/alpha/briefs/b.md"), "brief").unwrap();
        fs::write(root.join("gigas/alpha/briefs/.env"), "X=1").unwrap();
        fs::write(root.join("gigas/beta/briefs/b.md"), "other").unwrap();
        fs::write(root.join("notes.md"), "staged by the user").unwrap();
        git(&root, &["add", "notes.md"], &[]).unwrap();
        let line = r#"{"v":1,"at":5,"text":"x"}"#;
        let report = turn_end(&home.0, &turn("alpha", line));
        assert_eq!(report.error, None);
        assert!(report.commit.is_some());
        let files = git(
            &root,
            &["show", "--name-only", "--format=%s%n%an <%ae>", "HEAD"],
            &[],
        )
        .unwrap();
        assert_eq!(
            files.trim(),
            "giga(alpha): completed turn 01234567\nMonoCode Giga alpha <giga+alpha@monocode.local>\n\ngigas/alpha/AGENTS.md\ngigas/alpha/briefs/.keep\ngigas/alpha/briefs/b.md\ngigas/alpha/log.jsonl\ngigas/alpha/monos/.keep\ngigas/alpha/projects/.keep\ngigas/alpha/runbooks/.keep"
        );
        assert_eq!(
            git(&root, &["diff", "--cached", "--name-only"], &[]).unwrap(),
            "notes.md\n"
        );
        turn_end(&home.0, &turn("alpha", line));
        assert_eq!(
            fs::read_to_string(root.join("gigas/alpha/log.jsonl")).unwrap(),
            format!("{line}\n")
        );
    }

    // Defect: concurrent turn ends lose a commit or deadlock.
    #[test]
    fn concurrent_turn_ends_for_two_gigas_both_land() {
        let home = Arc::new(TempHome::new("race"));
        ensure(&home.0, &[giga("alpha"), giga("beta")]);
        let handles: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|slug| {
                let home = Arc::clone(&home);
                std::thread::spawn(move || {
                    for round in 0..3 {
                        fs::write(
                            home.root().join(format!("gigas/{slug}/briefs/r{round}.md")),
                            "plan",
                        )
                        .unwrap();
                        let report = turn_end(
                            &home.0,
                            &turn(slug, &format!(r#"{{"v":1,"round":{round}}}"#)),
                        );
                        assert_eq!(report.error, None);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let subjects = log_subjects(&home.root());
        for slug in ["alpha", "beta"] {
            assert_eq!(
                subjects
                    .iter()
                    .filter(|s| s.starts_with(&format!("giga({slug})")))
                    .count(),
                3
            );
            assert_eq!(
                fs::read_to_string(home.root().join(format!("gigas/{slug}/log.jsonl")))
                    .unwrap()
                    .lines()
                    .count(),
                3
            );
        }
    }

    /// A workspace whose Gigas have nothing left to commit.
    fn settled(name: &str, slugs: &[&str]) -> TempHome {
        let home = TempHome::new(name);
        let gigas: Vec<_> = slugs.iter().map(|slug| giga(slug)).collect();
        ensure(&home.0, &gigas);
        for slug in slugs {
            fs::write(
                home.root().join(format!("gigas/{slug}/briefs/seed.md")),
                "seed",
            )
            .unwrap();
            let report = turn_end(&home.0, &turn(slug, &format!(r#"{{"seed":"{slug}"}}"#)));
            assert!(report.commit.is_some(), "{report:?}");
        }
        home
    }

    /// One turn as the app runs it: a snapshot at the start, the agent's
    /// writes, a receipt that names the turn's files, then the turn end.
    fn giga_turn(
        home: &TempHome,
        slug: &str,
        n: u32,
        writes: &[(&str, &str)],
    ) -> (TurnEndReport, Vec<String>, String) {
        let root = home.root();
        let before = work_snapshot(&root, slug);
        for (rel, content) in writes {
            let path = root.join(format!("gigas/{slug}/{rel}"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        let files = files_since(&root, slug, Some(&before));
        let line = serde_json::json!({ "v": 1, "n": n, "files": files }).to_string();
        (turn_end(&home.0, &turn(slug, &line)), files, line)
    }

    fn head(root: &Path) -> String {
        git(root, &["rev-parse", "HEAD"], &[])
            .unwrap()
            .trim()
            .to_string()
    }

    fn head_files(root: &Path) -> Vec<String> {
        git(root, &["show", "--name-only", "--format=", "HEAD"], &[])
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn committed(root: &Path, path: &str) -> String {
        git(root, &["show", &format!("HEAD:{path}")], &[]).unwrap()
    }

    fn staged(root: &Path) -> String {
        git(root, &["diff", "--cached", "--name-only"], &[]).unwrap()
    }

    // Defect: a turn that only appended its receipt still commits, or the
    // receipt is not in log.jsonl.
    #[test]
    fn receipt_only_turn_makes_no_commit_and_keeps_the_line() {
        let home = settled("wait", &["alpha"]);
        let root = home.root();
        let before = head(&root);
        let (report, _, line) = giga_turn(
            &home,
            "alpha",
            1,
            &[("state/cache.json", "{}"), ("scratch/t.txt", "t")],
        );
        assert_eq!((report.commit, report.error), (None, None));
        assert_eq!(head(&root), before);
        let log = fs::read_to_string(root.join("gigas/alpha/log.jsonl")).unwrap();
        assert!(log.ends_with(&format!("{line}\n")), "{log}");
        assert_eq!(staged(&root), "");
    }

    // Defect: the waiting receipt misses the next commit, or that turn makes
    // more than one commit.
    #[test]
    fn next_real_change_commits_the_brief_and_every_waiting_line_once() {
        let home = settled("drain", &["alpha"]);
        let root = home.root();
        let before = head(&root);
        let (_, _, first) = giga_turn(&home, "alpha", 1, &[]);
        let (report, _, second) = giga_turn(&home, "alpha", 2, &[("briefs/plan.md", "plan")]);
        assert_eq!(report.commit, Some(head(&root)));
        assert_eq!(
            git(
                &root,
                &["rev-list", "--count", &format!("{before}..HEAD")],
                &[]
            )
            .unwrap()
            .trim(),
            "1"
        );
        assert_eq!(
            head_files(&root),
            ["gigas/alpha/briefs/plan.md", "gigas/alpha/log.jsonl"]
        );
        let log = committed(&root, "gigas/alpha/log.jsonl");
        assert!(log.ends_with(&format!("{first}\n{second}\n")), "{log}");
        assert_eq!(
            git(&root, &["status", "--porcelain", "--", "gigas/alpha/"], &[]).unwrap(),
            ""
        );
    }

    // Defect: `files` names something that is not this turn's work (the log,
    // state/, scratch/, files from before the turn) or misses a new file.
    #[test]
    fn receipt_files_list_the_turns_work_files() {
        let home = settled("files", &["alpha"]);
        let (_, first, _) = giga_turn(
            &home,
            "alpha",
            1,
            &[("state/cache.json", "{}"), ("scratch/t.txt", "t")],
        );
        assert_eq!(first, Vec::<String>::new());
        let (_, second, _) = giga_turn(
            &home,
            "alpha",
            2,
            &[("runbooks/deep/run.md", "run"), ("briefs/plan.md", "plan")],
        );
        assert_eq!(
            second,
            [
                "gigas/alpha/briefs/plan.md",
                "gigas/alpha/runbooks/deep/run.md"
            ]
        );
    }

    // Defect: `files` compares with HEAD instead of the turn's start, so it
    // names work an earlier turn left uncommitted.
    #[test]
    fn files_skip_work_left_uncommitted_by_an_earlier_turn() {
        let home = settled("earlier", &["alpha"]);
        let root = home.root();
        let git_lock = root.join(".git/index.lock");
        fs::write(&git_lock, "").unwrap();
        let (report, first, _) = giga_turn(&home, "alpha", 1, &[("briefs/a.md", "a")]);
        assert!(report.error.is_some(), "the commit failed on the git lock");
        assert_eq!(first, ["gigas/alpha/briefs/a.md"]);
        fs::remove_file(&git_lock).unwrap();
        let (report, second, _) = giga_turn(&home, "alpha", 2, &[("briefs/b.md", "b")]);
        assert_eq!(second, ["gigas/alpha/briefs/b.md"]);
        assert!(report.commit.is_some());
        assert_eq!(
            head_files(&root),
            [
                "gigas/alpha/briefs/a.md",
                "gigas/alpha/briefs/b.md",
                "gigas/alpha/log.jsonl"
            ]
        );
    }

    const RESTART_HOME: &str = "MC_GIGA_RESTART_HOME";

    #[test]
    #[ignore = "waiting_lines_survive_a_restart runs it in a process of its own"]
    fn restart_child_turn() {
        let home = std::env::var(RESTART_HOME).unwrap();
        let report = turn_end(Path::new(&home), &turn("alpha", r#"{"v":1,"n":1}"#));
        assert_eq!((report.commit, report.error), (None, None));
    }

    // Defect: a waiting receipt lives only in the app's memory, so a restart
    // between turns loses it.
    #[test]
    fn waiting_lines_survive_a_restart() {
        let home = settled("restart", &["alpha"]);
        let root = home.root();
        let before = head(&root);
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "giga_workspace::tests::restart_child_turn",
                "--ignored",
                "--quiet",
            ])
            .env(RESTART_HOME, &home.0)
            .status()
            .unwrap();
        assert!(status.success(), "first turn ran in its own process");
        assert_eq!(head(&root), before);
        let (report, _, second) = giga_turn(&home, "alpha", 2, &[("briefs/plan.md", "plan")]);
        assert!(report.commit.is_some());
        let log = committed(&root, "gigas/alpha/log.jsonl");
        assert!(
            log.ends_with(&format!("{{\"v\":1,\"n\":1}}\n{second}\n")),
            "{log}"
        );
    }

    // Defect: a turn of one Giga commits or stages another Giga's waiting
    // receipts.
    #[test]
    fn receipt_only_turn_leaves_another_gigas_lines_alone() {
        let home = settled("two", &["alpha", "beta"]);
        let root = home.root();
        let before = head(&root);
        giga_turn(&home, "beta", 1, &[]);
        let (report, _, _) = giga_turn(&home, "alpha", 2, &[]);
        assert_eq!((report.commit, report.error), (None, None));
        assert_eq!(head(&root), before);
        assert_eq!(staged(&root), "");
        let (report, _, _) = giga_turn(&home, "alpha", 3, &[("briefs/plan.md", "plan")]);
        assert!(report.commit.is_some());
        assert_eq!(
            head_files(&root),
            ["gigas/alpha/briefs/plan.md", "gigas/alpha/log.jsonl"]
        );
        assert_eq!(
            git(&root, &["status", "--porcelain", "--", "gigas/"], &[]).unwrap(),
            " M gigas/beta/log.jsonl\n"
        );
        assert_eq!(staged(&root), "");
    }

    // Defect: a lock left by a crashed process blocks every later commit, or
    // a live owner's lock is ignored.
    #[cfg(unix)]
    #[test]
    fn stale_lock_is_taken_over_and_live_lock_is_respected() {
        let home = TempHome::new("stale");
        ensure(&home.0, &[giga("alpha")]);
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        let lock = home.root().join(".git").join(LOCK_FILE);
        fs::write(&lock, child.id().to_string()).unwrap();
        let started = Instant::now();
        let waiter = {
            let root = home.root();
            std::thread::spawn(move || {
                WorkspaceLock::acquire(&root, LOCK_WAIT).map(|_| started.elapsed())
            })
        };
        std::thread::sleep(Duration::from_millis(300));
        child.kill().unwrap();
        child.wait().unwrap();
        let waited = waiter.join().unwrap().unwrap();
        assert!(
            waited >= Duration::from_millis(300),
            "waited for the live owner"
        );
        assert!(waited < Duration::from_secs(5), "took over once it died");
        let report = turn_end(&home.0, &turn("alpha", "{}"));
        assert_eq!(report.error, None);
    }

    // Defect: the index misses a source, or an incremental and a full build
    // differ (here: rebuilt twice over the same files).
    #[test]
    fn index_holds_every_log_line_and_brief() {
        let home = TempHome::new("index");
        ensure(&home.0, &[giga("alpha"), giga("beta")]);
        let root = home.root();
        fs::write(
            root.join("gigas/alpha/log.jsonl"),
            "{\"at\":7,\"asked\":\"tracing spans\"}\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("gigas/beta/briefs/old")).unwrap();
        fs::write(
            root.join("gigas/beta/briefs/old/release.md"),
            "release notes plan",
        )
        .unwrap();
        rebuild_index(&root).unwrap();
        let rows = || {
            let db = rusqlite::Connection::open(root.join("index/index.db")).unwrap();
            let mut query = db
                .prepare("SELECT source, giga, path, at FROM entries WHERE entries MATCH ?1")
                .unwrap();
            let mut found = |term: &str| {
                query
                    .query_map([term], |row| {
                        Ok(format!(
                            "{}|{}|{}|{}",
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, i64>(3)?
                        ))
                    })
                    .unwrap()
                    .map(Result::unwrap)
                    .collect::<Vec<_>>()
            };
            (found("tracing"), found("release"))
        };
        let (tracing, release) = rows();
        assert_eq!(tracing, vec!["log|alpha|gigas/alpha/log.jsonl|7"]);
        assert_eq!(release.len(), 1);
        assert!(release[0].starts_with("brief|beta|gigas/beta/briefs/old/release.md|"));
        rebuild_index(&root).unwrap();
        assert_eq!(rows(), (tracing, release));
    }
    // Defect: the workspace keeps teaching the removed `monos.send`, or the
    // fix leaves a change under `shared/` that no one may commit.
    // PR I2 review r2 n1. Defect: a text every Mono reads teaches scope
    // "turn" without saying only a Giga may use it, so a Mono tries it and
    // gets steer_scope_not_allowed.
    #[test]
    fn the_workspace_texts_say_only_a_giga_may_steer_a_whole_turn() {
        for text in [MANAGED_BODY, CONVENTIONS] {
            let line = text
                .lines()
                .find(|line| line.contains(r#"`"scope":"turn"`"#))
                .expect("the texts name scope turn");
            assert!(line.contains("a Giga to a Mono only"), "{line}");
        }
    }

    #[test]
    fn a_workspace_with_the_old_messaging_convention_learns_messages_send_and_stays_clean() {
        let home = TempHome::new("conventions");
        let root = home.root();
        fs::create_dir_all(root.join("shared")).unwrap();
        let old = "# Conventions for Gigas\n\n- Messages to other Monos: `app monos.send`. Use `whenBusy:\"steer\"` to redirect, `\"followUp\"` to wait.\n";
        fs::write(root.join("shared/CONVENTIONS.md"), old).unwrap();
        for _ in 0..2 {
            assert_eq!(ensure(&home.0, &[]).error, None);
        }
        assert_eq!(git(&root, &["status", "--porcelain"], &[]).unwrap(), "");
        assert_eq!(
            fs::read_to_string(root.join("shared/CONVENTIONS.md")).unwrap(),
            old
        );
        let agents = fs::read_to_string(root.join("AGENTS.md")).unwrap();
        let block = &agents[agents.find(MANAGED_START).unwrap()..];
        assert!(
            block.contains("`app monos.send` is removed")
                && block.contains("`app messages.send`")
                && block.contains("`intent:\"steer\"` with `reply_to` replaces `whenBusy`"),
            "{block}"
        );
    }
}
