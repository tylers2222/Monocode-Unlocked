//! ADE-273: where a harness child's startup spends its time. When
//! `MONOCODE_HARNESS_TIMING` names a file, each step of a child's process I/O
//! appends one JSON line to it. The scenario instance sets it, to the file the
//! puppet stamps too, so one file holds the whole timeline. Unset, this does
//! nothing. No line holds what the child sent or received.

use std::fs::OpenOptions;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

const FILE_VAR: &str = "MONOCODE_HARNESS_TIMING";

/// `at` is real wall time in ms, as the puppet's `Date.now()`: the scenario
/// build's fake clock does not move it.
pub(crate) fn stamp(event: &str, session_id: &str, pid: Option<u32>) {
    let Some(path) = std::env::var_os(FILE_VAR) else {
        return;
    };
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| u64::try_from(since.as_millis()).unwrap_or(0));
    let line = serde_json::json!({ "event": event, "at": at, "session": session_id, "pid": pid });
    // A diagnostic: a failed write must not change how the child runs.
    if let Ok(mut file) = OpenOptions::new().append(true).create(true).open(path) {
        let _ = file.write_all(format!("{line}\n").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stamp_appends_one_line_only_when_the_variable_names_a_file() {
        let path =
            std::env::temp_dir().join(format!("harness-timing-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let session = "harness-timing-test";
        let mine = || -> Vec<serde_json::Value> {
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .filter(|line: &serde_json::Value| line["session"] == session)
                .collect()
        };

        std::env::remove_var(FILE_VAR);
        stamp("before", session, None);
        assert!(mine().is_empty());

        std::env::set_var(FILE_VAR, &path);
        let earliest = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis();
        stamp("spawned", session, Some(7));
        stamp("write_called", session, None);
        std::env::remove_var(FILE_VAR);

        let lines = mine();
        let _ = std::fs::remove_file(&path);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "spawned");
        assert_eq!(lines[0]["pid"], 7);
        assert!(u128::from(lines[0]["at"].as_u64().unwrap()) >= earliest);
        assert_eq!(lines[1]["event"], "write_called");
        assert!(lines[1]["pid"].is_null());
    }
}
