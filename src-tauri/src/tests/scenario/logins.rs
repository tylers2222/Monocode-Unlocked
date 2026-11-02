//! A login folder registered through a symlink (ADE-171 part 1/3): a link
//! at `provider-accounts/<provider>/<id>` to an existing login folder, e.g.
//! `~/.claude-2`. The scenario starts a harness child on such a login in a
//! process whose home folder is a scratch folder, so no link or login it
//! makes is near a real one.

use std::path::{Path, PathBuf};

use tauri::Manager;

use super::agent_sandbox::{mock_app, script, wait_for};

const ROOT: &str = "MC_ADE171_ROOT";

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Runs the child with its home folder in a scratch folder, so no link
/// or login it makes is near a real one.
#[test]
fn a_login_folder_registered_through_a_symlink_runs_as_its_target() {
    let root = std::env::temp_dir().join(format!("ade171-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "scenario::logins::registered_login_child",
            "--ignored",
            "--quiet",
        ])
        .env(ROOT, &root)
        .env("HOME", root.join("home"))
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        child.status.success(),
        "a symlinked login did not run as its target:\n{}",
        String::from_utf8_lossy(&child.stdout)
    );
}

/// A mock app whose data folder is `$HOME/apps/com.monocode.desktop`, beside
/// another app's as on macOS, and a stand-in CLI that writes its Claude
/// config folder.
struct Rig {
    root: PathBuf,
    fx: PathBuf,
    app: tauri::App<tauri::test::MockRuntime>,
}

impl Rig {
    fn new(root: PathBuf) -> Self {
        let fx = script(
            &root.join("fx"),
            r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) printf '%s\n' "$CLAUDE_CONFIG_DIR" > ran.part && mv ran.part ran ;;
esac"#,
        );
        let data = root.join("home/apps/com.monocode.desktop");
        std::fs::create_dir_all(&data).unwrap();
        let app = mock_app(&data);
        Rig { root, fx, app }
    }

    fn folder(&self, id: &str) -> PathBuf {
        crate::harness::provider_account_path(self.app.handle(), "claude", id).unwrap()
    }

    fn dir(&self, id: &str) -> Result<Option<PathBuf>, String> {
        crate::harness::provider_account_dir(self.app.handle(), "claude", Some(id))
    }

    fn link(&self, id: &str, target: &Path) {
        std::fs::create_dir_all(self.folder(id).parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, self.folder(id)).unwrap();
    }

    /// `harness_spawn` on the login; the folder its CLI got.
    fn spawn(&self, id: &str) -> Result<String, String> {
        let workdir = self.root.join(id);
        std::fs::create_dir_all(&workdir).unwrap();
        let account =
            serde_json::from_value(serde_json::json!({ "provider": "claude", "id": id })).unwrap();
        crate::harness::harness_spawn(
            self.app.handle().clone(),
            self.app.state::<crate::harness::HarnessHost>(),
            id.into(),
            text(&self.fx),
            vec!["acp".into()],
            text(&workdir),
            Some(account),
            Some("fx".into()),
            Some(text(&self.fx)),
            None,
            None,
            None,
        )?;
        Ok(wait_for(&workdir.join("ran")))
    }
}

/// Defect: a symlinked login starts its CLI with the link, not the target
/// (Claude then misses the Keychain login made for the target), a plain
/// login's folder changes, a broken, looping or out-of-bounds link falls
/// back to a folder instead of failing, or removing the login follows the
/// link and deletes the target.
#[test]
#[ignore = "a_login_folder_registered_through_a_symlink_runs_as_its_target runs it in a process of its own"]
fn registered_login_child() {
    let rig = Rig::new(std::fs::canonicalize(std::env::var_os(ROOT).unwrap()).unwrap());
    let home = rig.root.join("home");
    let target = home.join(".claude-2");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("keep"), "the login's own file").unwrap();
    rig.link("linked", &target);
    assert_eq!(
        rig.spawn("linked").unwrap(),
        format!("{}\n", text(&target)),
        "the CLI did not get the link's target as its config folder"
    );
    // The string the user logged in with, not its canonical path.
    let alias = home.join("alias");
    std::os::unix::fs::symlink(&target, &alias).unwrap();
    rig.link("aliased", &alias);
    assert_eq!(
        rig.spawn("aliased").unwrap(),
        format!("{}\n", text(&alias)),
        "the CLI got another string than the link names"
    );
    assert_eq!(
        rig.spawn("plain").unwrap(),
        format!("{}\n", text(&rig.folder("plain"))),
        "a login that is not a link moved"
    );

    let other_app = home.join("apps/com.monocode.second/provider-accounts/claude/x");
    std::fs::create_dir_all(&other_app).unwrap();
    let refused: [(&str, PathBuf, &str); 11] = [
        ("relative", PathBuf::from(".claude-2"), "absolute path"),
        (
            "relative-up",
            PathBuf::from("../../../../.claude-2"),
            "absolute path",
        ),
        ("dotdot", home.join("apps/../.claude-2"), "absolute path"),
        ("dangling", home.join("missing"), "cannot be opened"),
        ("looping", rig.folder("looping"), "cannot be opened"),
        ("root", PathBuf::from("/"), "must link"),
        ("home", home.clone(), "must link"),
        ("file", target.join("keep"), "must link"),
        ("other-login", rig.folder("plain"), "must link"),
        ("data-parent", home.join("apps"), "must link"),
        ("other-app", other_app, "must link"),
    ];
    for (id, to, why) in refused {
        rig.link(id, &to);
        let error = rig.dir(id).unwrap_err();
        assert!(error.contains(why), "{id}: {error}");
        assert_eq!(
            rig.spawn(id).unwrap_err(),
            error,
            "{id}: a child started anyway"
        );
    }
    assert!(
        !home.join("missing").exists(),
        "a broken link made its target"
    );

    crate::harness::provider_account_remove(
        rig.app.handle().clone(),
        rig.app.state::<crate::harness::HarnessHost>(),
        "claude".into(),
        "linked".into(),
    )
    .unwrap();
    assert!(
        std::fs::symlink_metadata(rig.folder("linked")).is_err(),
        "removing the login kept its link"
    );
    assert!(
        target.join("keep").is_file(),
        "removing the login deleted the folder it linked to"
    );
}
