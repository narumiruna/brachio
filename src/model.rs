use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Status {
    Blocked,
    Error,
    Done,
    Working,
    Idle,
}

impl Status {
    pub fn parse(value: &str) -> Self {
        match value {
            "blocked" => Self::Blocked,
            "error" => Self::Error,
            "done" => Self::Done,
            "working" => Self::Working,
            _ => Self::Idle,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Blocked => "Blocked",
            Self::Error => "Error",
            Self::Done => "Done",
            Self::Working => "Working",
            Self::Idle => "Idle",
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct EntryId {
    pub session: String,
    pub block: String,
    pub agent: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub session: String,
    pub window: String,
    pub block: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Location {
    pub session: Arc<str>,
    pub tab: Arc<str>,
    pub pane: Arc<str>,
}

impl std::fmt::Display for Location {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} / {} / {}", self.session, self.tab, self.pane)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    pub id: EntryId,
    pub target: Target,
    pub name: String,
    pub status: Status,
    pub location: Location,
    pub working_dir: Option<String>,
    pub records: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
    pub warnings: Vec<String>,
}

impl Snapshot {
    pub fn sort(&mut self) {
        self.entries
            .sort_by(|a, b| (a.status, &a.location, &a.id).cmp(&(b.status, &b.location, &b.id)));
    }
}

/// Exact full resource IDs only. Never allow Rex's label/suffix/direction fallback.
pub fn valid_id(value: &str, kind: &str) -> bool {
    let Some(uuid) = value.strip_prefix(&format!("{kind}:")) else {
        return false;
    };
    let parts: Vec<_> = uuid.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(part, len)| part.len() == len && part.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn display_chars(value: &str) -> impl Iterator<Item = char> + '_ {
    value.chars().filter(|c| {
        !c.is_control()
            && !matches!(*c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
    })
}

/// Bounded single-line text for status messages and errors.
pub fn plain(value: &str) -> String {
    display_chars(value).take(256).collect()
}

/// Complete display text without terminal controls or bidi overrides.
pub fn display_text(value: &str) -> String {
    display_chars(value).collect()
}

pub fn agent_name(value: &str) -> Option<&'static str> {
    match value {
        "pi" => Some("pi"),
        "claude" | "claude-code" => Some("claude-code"),
        "codex" => Some("codex"),
        _ => None,
    }
}

/// Aggregate related reports into one entry per recognized app and pane.
pub fn aggregate(records: &serde_json::Value) -> Result<BTreeMap<String, (Status, usize)>, String> {
    let records = records
        .get("records")
        .and_then(|v| v.as_array())
        .ok_or("program_status is missing a records array")?;
    if records.len() > 256 {
        return Err("program_status exceeds the protocol limit of 256 records".into());
    }
    let mut parents: BTreeMap<&str, Option<&serde_json::Value>> = BTreeMap::new();
    for record in records {
        let id = record.get("id").and_then(|v| v.as_str()).unwrap_or("");
        parents
            .entry(id)
            .and_modify(|parent| *parent = None)
            .or_insert(Some(record));
    }
    let mut result: BTreeMap<String, (Status, usize)> = BTreeMap::new();
    for record in records {
        if record.get("state").and_then(|v| v.as_str()) == Some("clear") {
            continue;
        }
        let inherited = || {
            let mut id = record.get("id").and_then(|v| v.as_str()).unwrap_or("");
            if id.len() > 128 {
                return None;
            }
            while !id.is_empty() {
                id = id.rsplit_once('/').map_or("", |(parent, _)| parent);
                match parents.get(id) {
                    Some(None) => return None, // An ambiguous parent cannot establish identity.
                    Some(Some(parent)) => {
                        if let Some(app) = parent.get("app").and_then(|v| v.as_str()) {
                            return Some(app);
                        }
                    }
                    None => {}
                }
            }
            None
        };
        let app = record
            .get("effective_app")
            .and_then(|v| v.as_str())
            .or_else(|| record.get("app").and_then(|v| v.as_str()))
            .or_else(inherited);
        let Some(app) = app.and_then(agent_name) else {
            continue;
        };
        let status = Status::parse(record.get("state").and_then(|v| v.as_str()).unwrap_or(""));
        let entry = result.entry(app.to_string()).or_insert((status, 0));
        entry.0 = entry.0.min(status);
        entry.1 += 1;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn related_records_share_one_row_but_different_agents_do_not() {
        let result = aggregate(&json!({"records": [
            {"app":"pi","state":"working"},
            {"id":"worker","state":"idle"},
            {"id":"worker/test","state":"blocked"},
            {"id":"other","app":"codex","state":"error"},
            {"id":"gone","app":"pi","state":"clear"},
            {"id":"tool","app":"cargo","state":"working"}
        ]}))
        .unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result["pi"], (Status::Blocked, 3));
        assert_eq!(result["codex"], (Status::Error, 1));
    }

    #[test]
    fn missing_and_future_states_use_the_schema_idle_fallback() {
        let result = aggregate(&json!({"records": [
            {"app":"pi"},
            {"app":"codex","state":"sleeping"},
            {"state":"working"}
        ]}))
        .unwrap();
        assert_eq!(result["pi"].0, Status::Idle);
        assert_eq!(result["codex"].0, Status::Idle);
        assert!(aggregate(&json!({})).is_err());
        assert!(aggregate(&json!({"records":[]})).unwrap().is_empty());
    }

    #[test]
    fn protocol_states_and_nested_app_inheritance_are_explicit() {
        for (value, status) in [
            ("blocked", Status::Blocked),
            ("error", Status::Error),
            ("done", Status::Done),
            ("working", Status::Working),
            ("idle", Status::Idle),
            ("future", Status::Idle),
            ("unknown", Status::Idle),
            ("", Status::Idle),
        ] {
            assert_eq!(Status::parse(value), status);
        }
        let result = aggregate(&json!({"records": [
            {"app":"cargo","state":"working"},
            {"id":"worker","app":"pi","state":"idle"},
            {"id":"worker/test","state":"error"},
            {"id":"other","effective_app":"codex","state":"done"}
        ]}))
        .unwrap();
        assert_eq!(result["pi"], (Status::Error, 2));
        assert_eq!(result["codex"], (Status::Done, 1));
        assert!(
            aggregate(&json!({"records": vec![json!({"app":"pi","state":"idle"}); 257]})).is_err()
        );
    }

    #[test]
    fn idle_fallback_does_not_override_a_working_report() {
        let mut records = vec![
            json!({"app":"pi","state":"future"}),
            json!({"app":"pi","state":"working"}),
        ];
        for _ in 0..2 {
            let result = aggregate(&json!({"records": records})).unwrap();
            assert_eq!(result["pi"], (Status::Working, 2));
            records.reverse();
        }
    }

    #[test]
    fn duplicate_parent_ids_never_make_inheritance_depend_on_record_order() {
        for parent in ["", "worker"] {
            let mut records = vec![
                json!({"id":parent,"app":"pi","state":"idle"}),
                json!({"id":parent,"app":"codex","state":"idle"}),
                json!({"id":if parent.is_empty() { "child" } else { "worker/child" },"state":"blocked"}),
            ];
            for _ in 0..2 {
                let result = aggregate(&json!({"records": records})).unwrap();
                assert_eq!(result["pi"], (Status::Idle, 1));
                assert_eq!(result["codex"], (Status::Idle, 1));
                records.swap(0, 1);
            }
        }
    }

    #[test]
    fn unknown_apps_and_interpreters_are_not_agents() {
        assert_eq!(agent_name("node"), None);
        assert_eq!(agent_name("python"), None);
        assert_eq!(agent_name("claude"), Some("claude-code"));
    }

    #[test]
    fn ids_cannot_be_labels_flags_or_shell_syntax() {
        assert!(valid_id(
            "block:00000000-0000-0000-0000-000000000001",
            "block"
        ));
        for value in [
            "right",
            "--help",
            "shell",
            "block:123",
            "window:00000000-0000-0000-0000-000000000001",
        ] {
            assert!(!valid_id(value, "block"));
        }
    }

    #[test]
    fn display_text_removes_controls_and_direction_overrides() {
        assert_eq!(plain("a\x1b\n\u{202e}b"), "ab");
        assert_eq!(plain(&"x".repeat(500)).len(), 256);
        assert_eq!(display_text(&"x".repeat(500)).len(), 500);
        assert_eq!(display_text("a\x1b\n\u{202e}b"), "ab");
    }
}
