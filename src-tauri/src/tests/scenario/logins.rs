//! Logins (ADE-171). Part 1/3: a login folder registered through a symlink,
//! a link at `provider-accounts/<provider>/<id>` to an existing login folder,
//! e.g. `~/.claude-2`. The scenario starts a harness child on such a login in
//! a process whose home folder is a scratch folder, so no link or login it
//! makes is near a real one. Part 2/3: the login list `providerAccounts.ts`
//! imports from browser storage at boot and then reads and writes, sent as
//! the JSON it sends through the Spaces storage commands on a real store.
//! Part 4/4: the project choices it imports into Space 1 and then reads and
//! writes for the active Space, the same way. ADE-224: under srt, a CLI on a
//! linked login writes its folder and reaches the Keychain.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use tauri::Manager;

use super::agent_sandbox::{mock_app, script, stand_in_srt, wait_for};
use super::world::World;
use crate::spaces::{self, account, Provider, FIRST_SPACE};

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
        self.start(id, "claude", &self.fx, "fx", "acp", None)
    }

    /// `harness_spawn` of `cli`, found as `name`, with `arg` on a `provider`
    /// login; what the CLI wrote to `ran` in its folder.
    fn start(
        &self,
        id: &str,
        provider: &str,
        cli: &Path,
        name: &str,
        arg: &str,
        codex_store: Option<&str>,
    ) -> Result<String, String> {
        let workdir = self.root.join(id);
        std::fs::create_dir_all(&workdir).unwrap();
        let account = serde_json::from_value(json!({ "provider": provider, "id": id })).unwrap();
        crate::harness::harness_spawn(
            self.app.handle().clone(),
            crate::scenario::agent_sandbox::main_window(&self.app),
            self.app.state::<crate::harness::HarnessHost>(),
            id.into(),
            text(cli),
            vec![arg.into()],
            text(&workdir),
            Some(account),
            Some(name.into()),
            Some(text(cli)),
            codex_store.map(String::from),
            None,
            None,
            None,
        )?;
        Ok(wait_for(&workdir.join("ran")))
    }
}

/// Defect: a symlinked login starts its CLI with the link, not the target
/// (Claude then misses the Keychain login made for the target), a plain
/// login's folder changes, srt's settings leave a linked login's folder
/// unwritable, its hooks or `notify` config writable, or open a plain one's in
/// the app data, Claude Code's deny rules name the link instead of the folder
/// its CLI gets, a broken, looping or
/// out-of-bounds link falls back to a folder instead of failing, or removing
/// the login follows the link and deletes the target.
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

    sandbox_rules_follow_the_login(&rig, &target);

    let other_app = home.join("apps/com.monocode.second/provider-accounts/claude/x");
    std::fs::create_dir_all(&other_app).unwrap();
    // Outside the home folder, and not a folder that holds the app data.
    let outside = rig.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let refused: [(&str, PathBuf, &str); 12] = [
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
        ("outside-home", outside, "must link"),
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

/// srt's rules and Claude Code's deny rules for a linked login: its folder
/// writable but not its hooks or `notify` config, a plain login's folder in
/// the app data not opened, and Claude Code's rules on the folder its CLI gets.
fn sandbox_rules_follow_the_login(rig: &Rig, target: &Path) {
    let marker = rig
        .app
        .path()
        .app_data_dir()
        .unwrap()
        .join("agent-sandbox-on");
    std::fs::write(&marker, b"").unwrap();
    stand_in_srt(&rig.root.join("srt"));
    rig.link("sandboxed", target);
    for id in ["sandboxed", "plain-sandboxed"] {
        rig.spawn(id).unwrap();
    }
    let rules = |id: &str, key: &str| {
        let settings = std::fs::read_to_string(rig.root.join(id).join("srt-settings")).unwrap();
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(settings).unwrap()).unwrap();
        settings["filesystem"][key].clone()
    };
    let has = |list: &Value, dir: &Path| list.as_array().unwrap().contains(&json!(text(dir)));
    assert!(
        has(&rules("sandboxed", "allowWrite"), target),
        "srt keeps the CLI from writing its linked login's folder"
    );
    for file in ["settings.json", "config.toml"] {
        assert!(
            has(&rules("sandboxed", "denyWrite"), &target.join(file)),
            "the CLI may write {file}, which sessions outside srt run"
        );
    }
    assert!(
        !has(
            &rules("plain-sandboxed", "allowWrite"),
            &rig.folder("plain-sandboxed")
        ),
        "srt lets the CLI write a login folder in the app data"
    );
    std::fs::remove_file(marker).unwrap();

    let data = rig.app.path().app_data_dir().unwrap();
    std::fs::write(data.join("claude-deny-rules-on"), b"").unwrap();
    let account =
        serde_json::from_value(json!({ "provider": "claude", "id": "sandboxed" })).unwrap();
    let login = crate::harness::LoginDir::of(rig.app.handle(), Some(&account)).unwrap();
    let deny = crate::claude_deny::srt_deny(
        rig.app.handle(),
        &rig.root.join("sandboxed"),
        login.as_ref(),
    )
    .unwrap();
    assert!(
        deny.contains(&text(&target.join("settings.json"))),
        "Claude Code's deny rules miss the linked login's settings: {deny:?}"
    );
    assert!(
        !deny.iter().any(|entry| Path::new(entry).starts_with(&data)),
        "Claude Code's deny rules name the link, not the folder its CLI gets: {deny:?}"
    );
    std::fs::remove_file(data.join("claude-deny-rules-on")).unwrap();
}

struct Remove(PathBuf);

impl Drop for Remove {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A scratch folder in the home folder, removed on drop, and a rig in it: a
/// login must link inside the home folder, and every spawn may write the temp dir.
fn home_rig() -> (Remove, Rig) {
    let home = PathBuf::from(crate::dirs_home().unwrap());
    let root = home.join(format!(".ade224-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let rig = Rig::new(std::fs::canonicalize(&root).unwrap());
    (Remove(root), rig)
}

/// Defect: a Codex Mono's `CODEX_HOME` is not its own home beside the app
/// data folder (ADE-224), its home keeps a `config.toml` an agent wrote there
/// (r1 M2), or private storage starts for a spawn that is not Codex's
/// app-server on a Codex login.
#[test]
fn a_codex_mono_runs_in_its_own_home_beside_the_app_data() {
    let (_remove, rig) = home_rig();
    let data = rig.app.path().app_data_dir().unwrap();
    std::fs::create_dir_all(rig.root.join("codex-cli")).unwrap();
    let codex = script(
        &rig.root.join("codex-cli/codex"),
        r#"case "$1" in
  --help) echo "codex app-server" ;;
  --version) echo "codex 0.0.1" ;;
  *) printf '%s\n' "$CODEX_HOME" > ran.part && mv ran.part ran ;;
esac"#,
    );
    let target = rig.root.join("home/.codex-2");
    std::fs::create_dir_all(&target).unwrap();
    let link = data.join("provider-accounts/codex/linked");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, link).unwrap();
    let home = crate::codex_mono_store::homes(&data)
        .unwrap()
        .join("com.monocode.desktop/linked");
    // An agent's `notify` from an earlier run; the login has no config.toml.
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), "notify = [\"planted\"]").unwrap();
    assert_eq!(
        rig.start(
            "linked",
            "codex",
            &codex,
            "codex",
            "app-server",
            Some("mono")
        ),
        Ok(format!("{}\n", text(&home))),
        "the Mono's CODEX_HOME is not its own home beside the app data"
    );
    assert_eq!(
        std::fs::read_link(home.join("config.toml")).ok(),
        Some(target.join("config.toml")),
        "the Mono home kept a config.toml that is not its login's"
    );
    let refused = "Private Mono storage is only supported for Codex app-server";
    for (id, provider, cli, name, arg) in [
        ("exec", "codex", &codex, "codex", "exec"),
        ("claude-login", "claude", &codex, "codex", "app-server"),
        ("other-cli", "codex", &rig.fx, "fx", "app-server"),
    ] {
        assert_eq!(
            rig.start(id, provider, cli, name, arg, Some("mono")),
            Err(refused.into()),
            "{id}: private storage started"
        );
    }
}

/// Defect: under the real srt, a CLI on a login linked to a folder outside
/// the app data cannot read it (it got the link, in the app data), cannot
/// write it (Claude Code refreshes its login there), or cannot reach the
/// Keychain ("A Module Directory Service error", so Claude Code reports it is
/// not logged in). That last one shows only where `security` must rewrite
/// the module cache: on a CI runner whose cache is current it passes without
/// it, and `on_settings_start_with_the_default_allows` is the check that fails
/// (#512). macOS only, with srt 0.0.81 on the search PATH, as ci.yml installs
/// it. In the home folder, since every spawn may write the temp dir.
///
/// Not in the parallel suite: the session runs under the agents' disk throttle
/// (`IOPOL_THROTTLE`), and the suite's own disk work at normal priority holds
/// it back for as long as it lasts, past the 20 s `wait_for` (ADE-313). ci.yml
/// runs both real-srt tests in a step of their own, after `cargo test`.
#[test]
#[cfg(target_os = "macos")]
#[ignore = "ci.yml runs it after the suite, in a step of its own (ADE-313)"]
fn a_linked_login_signs_in_under_the_real_srt() {
    if crate::harness::resolve_gui_binary("srt").is_none() {
        assert!(std::env::var_os("CI").is_none(), "srt is not installed");
        eprintln!("skipped: srt is not installed");
        return;
    }
    let (_remove, mut rig) = home_rig();
    std::fs::create_dir_all(rig.root.join("login")).unwrap();
    rig.fx = script(
        &rig.root.join("login/fx"),
        r#"case "$1" in
  --help) echo "fx acp: ask over the Agent Client Protocol" ;;
  --version) echo "fx 0.0.1" ;;
  *) {
    cat "$CLAUDE_CONFIG_DIR/login" && echo read
    echo refreshed > "$CLAUDE_CONFIG_DIR/login" && echo wrote
    /usr/bin/security find-generic-password -s ade-224-no-such-item
  } > ran.part 2>&1; mv ran.part ran ;;
esac"#,
    );
    let target = rig.root.join("home/.claude-2");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("login"), "token\n").unwrap();
    rig.link("linked", &target);
    let data = rig.app.path().app_data_dir().unwrap();
    std::fs::write(data.join("agent-sandbox-on"), b"").unwrap();
    let ran = rig.spawn("linked").unwrap();
    assert!(
        ran.starts_with("token\nread\nwrote\n"),
        "the CLI could not read or write its login folder:\n{ran}"
    );
    assert!(
        !ran.contains("Module Directory Service") && ran.contains("could not be found"),
        "the CLI could not reach the Keychain:\n{ran}"
    );
    assert_eq!(
        std::fs::read_to_string(target.join("login")).unwrap(),
        "refreshed\n"
    );
}

/// Defect: under the real srt, a Codex Mono cannot write its private home, can
/// read another login's, or can add a file to the app data folder (ADE-224:
/// srt cannot open one folder inside a denied one to writes, so the homes are
/// beside it). macOS only, and run after the suite, as
/// `a_linked_login_signs_in_under_the_real_srt`.
#[test]
#[cfg(target_os = "macos")]
#[ignore = "ci.yml runs it after the suite, in a step of its own (ADE-313)"]
fn a_codex_mono_writes_only_its_home_under_the_real_srt() {
    if crate::harness::resolve_gui_binary("srt").is_none() {
        assert!(std::env::var_os("CI").is_none(), "srt is not installed");
        eprintln!("skipped: srt is not installed");
        return;
    }
    let (_remove, rig) = home_rig();
    let data = rig.app.path().app_data_dir().unwrap();
    let homes = crate::codex_mono_store::homes(&data).unwrap();
    let other = homes.join("com.monocode.desktop/other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("secret"), "another login's rollout").unwrap();
    std::fs::create_dir_all(rig.root.join("codex-cli")).unwrap();
    let planted = data.join("planted");
    let codex = script(
        &rig.root.join("codex-cli/codex"),
        &format!(
            r#"case "$1" in
  --help) echo "codex app-server" ;;
  --version) echo "codex 0.0.1" ;;
  *) {{
    echo rollout > "$CODEX_HOME/sessions/r.jsonl" && echo wrote-home
    cat '{}' && echo read-other
    echo x > '{}' && echo wrote-data
  }} > ran.part 2>&1; mv ran.part ran ;;
esac"#,
            other.join("secret").display(),
            planted.display()
        ),
    );
    let target = rig.root.join("home/.codex-2");
    std::fs::create_dir_all(&target).unwrap();
    let link = data.join("provider-accounts/codex/linked");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, link).unwrap();
    std::fs::write(data.join("agent-sandbox-on"), b"").unwrap();
    let ran = rig
        .start(
            "linked",
            "codex",
            &codex,
            "codex",
            "app-server",
            Some("mono"),
        )
        .unwrap();
    assert!(
        ran.contains("wrote-home"),
        "the Mono cannot write its home:\n{ran}"
    );
    assert!(
        !ran.contains("read-other") && !ran.contains("another login's"),
        "the Mono read another login's home:\n{ran}"
    );
    assert!(
        !ran.contains("wrote-data") && !planted.exists(),
        "the Mono added a file to the app data folder:\n{ran}"
    );
    assert!(homes
        .join("com.monocode.desktop/linked/sessions/r.jsonl")
        .is_file());
}

/// What `loadProviderAccounts` sends for each login browser storage holds:
/// `spaces_account_save` twice (it saves, then renames, every login, and
/// `default` only takes the rename), then `spaces_login_set` per CLI.
fn import(world: &World, logins: &[Value]) {
    for login in logins {
        let sends = if login["id"] == "default" { 1 } else { 2 };
        for _ in 0..sends {
            let login = serde_json::from_value(login.clone()).unwrap();
            account::save_account(world.store(), &login, world.now()).unwrap();
        }
    }
    for provider in ["claude", "codex", "devin"] {
        let login = json!({ "spaceId": FIRST_SPACE, "provider": provider, "accountId": "default" });
        account::set_login(world.store(), &serde_json::from_value(login).unwrap()).unwrap();
    }
}

/// What `spaces_accounts_list` answers, as the snapshot reads it.
fn logins(world: &World) -> Value {
    serde_json::to_value(account::accounts(world.store()).unwrap()).unwrap()
}

fn login(provider: &str, id: &str, label: &str) -> Value {
    json!({ "provider": provider, "id": id, "label": label })
}

fn defaults(label: [&str; 3]) -> [Value; 3] {
    [
        login("claude", "default", label[0]),
        login("codex", "default", label[1]),
        login("devin", "default", label[2]),
    ]
}

/// Defect: an import adds a second `default`, loses a login or its label,
/// leaves Space 1 without each CLI's `default`, keeps more than 48 code
/// points of a label or splits an emoji, a restart changes the list, or a
/// removed login or the `default` one comes back wrong. It also records that
/// two logins saved in the same millisecond list by id, not in the order they
/// were saved, which is why `loadProviderAccounts` keeps the import's order.
#[test]
fn the_login_list_imports_and_round_trips() {
    let empty = World::new("logins-empty");
    import(&empty, &defaults(["Default account"; 3]));
    assert_eq!(
        logins(&empty),
        json!(defaults(["Default account"; 3])),
        "an empty browser storage changed the logins"
    );
    assert_eq!(
        serde_json::to_value(account::logins(empty.store()).unwrap()).unwrap(),
        json!(["claude", "codex", "devin"].map(|provider| {
            json!({ "spaceId": FIRST_SPACE, "provider": provider, "accountId": "default" })
        })),
        "Space 1 does not use each CLI's own login"
    );

    let mut world = World::new("logins");
    let [claude, codex, devin] = defaults(["Primary", "Default account", "Default account"]);
    let emoji = format!("{}\u{1F600}", "a".repeat(47));
    let zulu = login("claude", "account-zulu", &format!("{emoji}bcd"));
    let alpha = login("claude", "account-alpha", "Alpha");
    let x = login("codex", "account-x", "X");
    import(
        &world,
        &[
            claude.clone(),
            zulu,
            alpha.clone(),
            codex.clone(),
            x.clone(),
            devin.clone(),
        ],
    );
    let zulu = login("claude", "account-zulu", &emoji);
    let listed = json!([claude, alpha, zulu, codex, x, devin]);
    assert_eq!(
        logins(&world),
        listed,
        "a login was lost or doubled, a label was cut wrong, or same-ms saves no longer list by id"
    );

    world.down(60_000);
    assert_eq!(logins(&world), listed, "a restart changed the logins");
    account::remove_account(world.store(), Provider::Codex, "account-x").unwrap();
    assert!(
        account::remove_account(world.store(), Provider::Codex, "default").is_err(),
        "the default login was removed"
    );
    assert_eq!(
        logins(&world),
        json!([claude, alpha, zulu, codex, devin]),
        "a removed login stayed, or another went with it"
    );
}

/// The project keys `selectionKey()` writes: a local folder, a Windows
/// drive, a remote project, and `~` for no project.
const KEYS: [&str; 4] = ["/work/acme", "c:/repo", "remote://box/srv/app", "~"];

fn choice(space: &str, project: &str, provider: &str, id: &str) -> Value {
    json!({ "spaceId": space, "projectPath": project, "provider": provider, "accountId": id })
}

/// What the import and `selectProviderAccount` send for one choice.
fn choose(world: &World, choice: &Value) {
    let login = serde_json::from_value(choice.clone()).unwrap();
    spaces::set_project_login(world.store(), &login).unwrap();
}

/// What `spaces_project_logins_list` gives the snapshot for one Space.
fn choices(world: &World, space: &str) -> Value {
    serde_json::to_value(spaces::project_logins(world.store(), space).unwrap()).unwrap()
}

/// Defect: a project key comes back other than `selectionKey()` wrote it,
/// so the snapshot never finds the choice; a Space lists another Space's
/// choices; a restart changes them; or a removed login keeps its choices.
#[test]
fn project_choices_round_trip_per_space() {
    let mut world = World::new("project-logins");
    let work = serde_json::from_value(login("claude", "account-work", "Work")).unwrap();
    account::save_account(world.store(), &work, world.now()).unwrap();
    let mut space_one: Vec<Value> = KEYS
        .iter()
        .map(|key| choice(FIRST_SPACE, key, "claude", "account-work"))
        .collect();
    space_one.push(choice(FIRST_SPACE, "~", "codex", "default"));
    for row in &space_one {
        choose(&world, row);
    }
    assert_eq!(
        choices(&world, FIRST_SPACE),
        json!(space_one),
        "a choice came back under another key, order or Space"
    );

    let two = spaces::create(world.store(), "Client", None, world.now()).unwrap();
    let client = choice(&two.id, KEYS[0], "claude", "default");
    choose(&world, &client);
    assert_eq!(
        choices(&world, FIRST_SPACE),
        json!(space_one),
        "Space 1 lists Space 2's choice"
    );
    assert_eq!(
        choices(&world, &two.id),
        json!([client]),
        "Space 2 lists Space 1's choices"
    );

    world.down(60_000);
    assert_eq!(
        choices(&world, FIRST_SPACE),
        json!(space_one),
        "a restart changed the choices"
    );
    account::remove_account(world.store(), Provider::Claude, "account-work").unwrap();
    assert_eq!(
        choices(&world, FIRST_SPACE),
        json!([choice(FIRST_SPACE, "~", "codex", "default")]),
        "a removed login kept its project choices"
    );
    assert_eq!(
        choices(&world, &two.id),
        json!([client]),
        "Space 2 lost its choice"
    );
}
