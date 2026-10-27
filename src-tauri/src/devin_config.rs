//! Supervised Devin sessions. Devin's ACP server has no mode that asks before
//! workspace edits, but its permission rules can: the child runs with a copy of
//! the user's config whose `ask` list also covers every write. `--config`
//! replaces the user config rather than layering on it, so the copy keeps the
//! user's own settings.

use std::io::ErrorKind;
use std::path::PathBuf;

use serde_json::{json, Value};
use tauri::{AppHandle, Manager, Runtime};

use crate::{dirs_home, jsonc::strip_jsonc_comments};

pub(crate) const ASK_EDITS_RULE: &str = "Write(**)";

/// The `--config` arguments for a supervised `devin acp` child.
pub(crate) fn supervised_args<R: Runtime>(app: &AppHandle<R>) -> Result<Vec<String>, String> {
    let config = ask_before_edits(read_user_config()?.as_deref())?;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("devin");
    let path = crate::private_file::write_once(&dir, "supervised", &config)?;
    Ok(vec!["--config".into(), path.to_string_lossy().into_owned()])
}

/// The user config file Devin itself reads.
fn user_config_path() -> Option<PathBuf> {
    let env = |key: &str| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let dir = if cfg!(windows) {
        env("APPDATA")
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| dirs_home().map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(dir.join("devin").join("config.json"))
}

fn read_user_config() -> Result<Option<String>, String> {
    let Some(path) = user_config_path() else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

/// The user's config with an `ask` rule for every write. Anything it cannot
/// extend safely is an error: Supervised must not start without the rule.
pub(crate) fn ask_before_edits(user: Option<&str>) -> Result<String, String> {
    const HELP: &str = "Supervised mode needs it to make Devin ask before edits.";
    let mut config = match user.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            let cleaned = strip_jsonc_comments(text).ok_or_else(|| {
                format!("Devin's config.json has an unterminated comment. {HELP}")
            })?;
            serde_json::from_str::<Value>(&cleaned)
                .map_err(|e| format!("Devin's config.json is not valid JSON ({e}). {HELP}"))?
        }
        None => json!({}),
    };
    let ask = config
        .as_object_mut()
        .ok_or_else(|| format!("Devin's config.json is not a JSON object. {HELP}"))?
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("Devin's config.json `permissions` is not an object. {HELP}"))?
        .entry("ask")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("Devin's config.json `permissions.ask` is not a list. {HELP}"))?;
    if !ask.iter().any(|rule| rule.as_str() == Some(ASK_EDITS_RULE)) {
        ask.push(json!(ASK_EDITS_RULE));
    }
    serde_json::to_string_pretty(&config).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask_rules(config: &str) -> Vec<String> {
        let value: Value = serde_json::from_str(config).unwrap();
        value["permissions"]["ask"]
            .as_array()
            .unwrap()
            .iter()
            .map(|rule| rule.as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn adds_the_write_rule_and_keeps_the_users_settings() {
        let merged = ask_before_edits(Some(
            r#"{"theme_mode":"dark","permissions":{"allow":["Exec(python3)"],"ask":["exec"]}}"#,
        ))
        .unwrap();
        let value: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["theme_mode"], "dark");
        assert_eq!(value["permissions"]["allow"], json!(["Exec(python3)"]));
        assert_eq!(ask_rules(&merged), vec!["exec", ASK_EDITS_RULE]);
        // Idempotent: the rule is not added twice.
        assert_eq!(
            ask_rules(&ask_before_edits(Some(&merged)).unwrap()),
            vec!["exec", ASK_EDITS_RULE]
        );
    }

    #[test]
    fn starts_from_an_empty_config() {
        assert_eq!(
            ask_rules(&ask_before_edits(None).unwrap()),
            vec![ASK_EDITS_RULE]
        );
        assert_eq!(
            ask_rules(&ask_before_edits(Some("  ")).unwrap()),
            vec![ASK_EDITS_RULE]
        );
    }

    #[test]
    fn refuses_configs_it_cannot_extend() {
        assert!(ask_before_edits(Some("{} /* unterminated")).is_err());
        assert!(ask_before_edits(Some("[]")).is_err());
        assert!(ask_before_edits(Some(r#"{"permissions":[]}"#)).is_err());
        assert!(ask_before_edits(Some(r#"{"permissions":{"ask":"Write(**)"}}"#)).is_err());
    }

    #[test]
    fn preserves_settings_and_permissions_in_commented_configs() {
        let user = r#"{
            // User-wide model and proxy settings.
            "agent": { "model": "swe-2-high" },
            "proxy": { "url": "https://example.invalid/a//b/*literal*/" },
            "label": "雪 \" // still a string /* not a comment */",
            "permissions": {
                "allow": ["Exec(git status)"], /* Keep existing grants. */
                "deny": ["Write(.env*)"],
                "ask": ["exec"]
            }
        } // A trailing comment is also valid."#;
        let merged = ask_before_edits(Some(user)).unwrap();
        let value: Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(value["agent"]["model"], "swe-2-high");
        assert_eq!(
            value["proxy"]["url"],
            "https://example.invalid/a//b/*literal*/"
        );
        assert_eq!(
            value["label"],
            "雪 \" // still a string /* not a comment */"
        );
        assert_eq!(value["permissions"]["allow"], json!(["Exec(git status)"]));
        assert_eq!(value["permissions"]["deny"], json!(["Write(.env*)"]));
        assert_eq!(ask_rules(&merged), vec!["exec", ASK_EDITS_RULE]);
        assert_eq!(
            ask_rules(&ask_before_edits(Some("{ // comment\r\n}")).unwrap()),
            vec![ASK_EDITS_RULE]
        );
    }

    #[test]
    fn malformed_commented_configs_still_fail_closed() {
        for user in [
            "{} /* unterminated",
            r#"{"a": 1/* a comment cannot join number tokens */2}"#,
            r#"{"permissions": {"ask": /* still the wrong type */ "Write(**)"}}"#,
            r#"{"permissions": [/* still the wrong type */]}"#,
            r#"{"a": "unterminated // comment"#,
        ] {
            assert!(ask_before_edits(Some(user)).is_err(), "Accepted {user}");
        }
    }
}
