use crate::model::{
    Entry, EntryId, Snapshot, Status, Target, agent_name, aggregate, plain, valid_id,
};
use crate::process::Runner;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct Rex {
    runner: Runner,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClientSelection {
    Pinned(String),
    Retry(String),
    Refused(String),
}

impl ClientSelection {
    pub fn result(&self) -> Result<String, String> {
        match self {
            Self::Pinned(id) => Ok(id.clone()),
            Self::Retry(error) | Self::Refused(error) => Err(error.clone()),
        }
    }
}

#[derive(Deserialize)]
struct Sessions {
    sessions: Vec<Session>,
}

#[derive(Deserialize)]
struct Session {
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct View {
    session_id: Option<String>,
    #[serde(default)]
    label: String,
    windows: Option<Vec<Window>>,
    active_window_id: Option<String>,
}

#[derive(Deserialize)]
struct Window {
    window_id: Option<String>,
    focused_block_id: Option<String>,
    #[serde(default)]
    label: String,
    layers: Option<Vec<Layer>>,
}

#[derive(Deserialize)]
struct Layer {
    blocks: Option<Vec<PlacedBlock>>,
}

#[derive(Deserialize)]
struct PlacedBlock {
    block_id: Option<String>,
}

#[derive(Deserialize)]
struct Blocks {
    blocks: Vec<Block>,
}

#[derive(Deserialize)]
struct Block {
    block_id: Option<String>,
    window_id: Option<String>,
    #[serde(default)]
    label: String,
    #[serde(default)]
    creator_name: String,
    #[serde(default)]
    placement: String,
}

#[derive(Clone, Debug)]
struct Pane {
    target: Target,
    location: String,
}

#[derive(Deserialize)]
struct Clients {
    clients: Vec<Client>,
}

#[derive(Deserialize)]
struct Client {
    client_id: Option<String>,
    connection_state: Option<String>,
    info: Option<ClientInfo>,
}

#[derive(Deserialize)]
struct ClientInfo {
    kind: Option<String>,
}

impl Rex {
    pub fn new(runner: Runner) -> Self {
        Self { runner }
    }

    fn args(args: &[&str]) -> Vec<String> {
        ["--autostart=false", "--timeout", "3s", "--color", "never"]
            .into_iter()
            .chain(args.iter().copied())
            .map(str::to_string)
            .collect()
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.runner.run(&Self::args(args))
    }

    fn json<T: serde::de::DeserializeOwned>(&self, args: &[&str]) -> Result<T, String> {
        let text = self.run(args)?;
        serde_json::from_str(&text)
            .map_err(|error| format!("Invalid Rex JSON: {error}; command: {}", args.join(" ")))
    }

    fn panes(&self, session: &str, warnings: &mut Vec<String>) -> Result<Vec<Pane>, String> {
        let view: View = self.json(&["session", "inspect", session, "--json"])?;
        if view.session_id.as_deref() != Some(session) {
            return Err("Rex returned a different or missing session ID".into());
        }
        let blocks: Blocks = self.json(&["block", "ls", "-s", session, "--json"])?;
        let mut windows = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for window in view.windows.unwrap_or_default() {
            let Some(id) = window.window_id.filter(|id| valid_id(id, "window")) else {
                warnings.push("Window without a full ID ignored".into());
                continue;
            };
            for layer in window.layers.unwrap_or_default() {
                for block in layer.blocks.unwrap_or_default() {
                    if let Some(block) = block.block_id.filter(|id| valid_id(id, "block"))
                        && windows
                            .insert(block.clone(), (id.clone(), window.label.clone()))
                            .is_some()
                    {
                        ambiguous.insert(block);
                    }
                }
            }
        }
        let mut seen = BTreeSet::new();
        for block in &blocks.blocks {
            if let Some(id) = &block.block_id
                && !seen.insert(id.clone())
            {
                ambiguous.insert(id.clone());
            }
        }
        let mut panes = Vec::new();
        for block in blocks.blocks {
            if block.creator_name != "com.superlogical.terminal" {
                continue;
            }
            let Some(id) = block.block_id.filter(|id| valid_id(id, "block")) else {
                warnings.push("Terminal pane without a full ID ignored".into());
                continue;
            };
            let location = windows.get(&id);
            if block.placement != "placed"
                || ambiguous.contains(&id)
                || location.is_none()
                || location.is_some_and(|(window, _)| block.window_id.as_ref() != Some(window))
            {
                warnings.push(format!("Detached, ambiguous, or moved pane {id} ignored"));
                continue;
            }
            let (window, label) = location.expect("checked above");
            panes.push(Pane {
                target: Target {
                    session: session.into(),
                    window: window.clone(),
                    block: id.clone(),
                },
                location: format!(
                    "{} / {} / {}",
                    named(&view.label, session),
                    named(label, window),
                    named(&block.label, &id)
                ),
            });
        }
        Ok(panes)
    }

    fn reports(
        &self,
        pane: &Pane,
        warnings: &mut Vec<String>,
    ) -> BTreeMap<String, (Status, usize)> {
        let target = &pane.target;
        let status: Result<Value, String> = self.json(&[
            "block",
            "call",
            "-s",
            &target.session,
            "-b",
            &target.block,
            "program_status",
        ]);
        let mut reports = match status.and_then(|value| aggregate(&value)) {
            Ok(reports) => reports,
            Err(error) => {
                warnings.push(format!("Status unavailable for {}: {error}", target.block));
                BTreeMap::new()
            }
        };
        let process: Result<Value, String> = self.json(&[
            "block",
            "call",
            "-s",
            &target.session,
            "-b",
            &target.block,
            "process",
        ]);
        match process {
            Ok(value) => {
                if let Some(app) = process_agent(&value) {
                    reports.entry(app.into()).or_insert((Status::Unknown, 0));
                }
            }
            Err(error) => warnings.push(format!(
                "Process identity unavailable for {}: {error}",
                target.block
            )),
        }
        reports
    }

    pub fn snapshot(&self) -> Result<Snapshot, String> {
        let sessions: Sessions = self.json(&["ls", "--json"])?;
        let mut snapshot = Snapshot::default();
        let mut seen = BTreeSet::new();
        for session in sessions.sessions {
            let Some(id) = session.session_id.filter(|id| valid_id(id, "session")) else {
                snapshot
                    .warnings
                    .push("Session without a full ID ignored".into());
                continue;
            };
            if !seen.insert(id.clone()) {
                return Err("Duplicate session IDs in Rex snapshot".into());
            }
            let panes = match self.panes(&id, &mut snapshot.warnings) {
                Ok(panes) => panes,
                Err(error) => {
                    snapshot
                        .warnings
                        .push(format!("Session {id} unavailable: {error}"));
                    continue;
                }
            };
            for pane in panes {
                for (agent, (status, records)) in self.reports(&pane, &mut snapshot.warnings) {
                    snapshot.entries.push(Entry {
                        id: EntryId {
                            session: id.clone(),
                            block: pane.target.block.clone(),
                            agent: agent.clone(),
                        },
                        target: pane.target.clone(),
                        name: agent,
                        status,
                        location: pane.location.clone(),
                        records,
                    });
                }
            }
        }
        snapshot.sort();
        Ok(snapshot)
    }

    /// Retry connection/no-app failures only until a full client ID is pinned.
    /// Ambiguity is a selection decision, not a transient connection failure.
    pub fn discover_client(&self, requested: Option<&str>) -> ClientSelection {
        if requested.is_some_and(|id| !valid_id(id, "client")) {
            return ClientSelection::Refused(
                "--client requires a full client:<uuid> ID; use rex client ls --json".into(),
            );
        }
        let clients: Clients = match self.json(&["client", "ls", "--json"]) {
            Ok(clients) => clients,
            Err(error) => return ClientSelection::Retry(error),
        };
        let apps: Vec<_> = clients
            .clients
            .into_iter()
            .filter(|client| {
                client.info.as_ref().and_then(|info| info.kind.as_deref()) == Some("app")
                    && client.connection_state.as_deref() == Some("connected")
            })
            .filter_map(|client| client.client_id.filter(|id| valid_id(id, "client")))
            .collect();
        match requested {
            Some(id) if apps.iter().any(|app| app == id) => ClientSelection::Pinned(id.into()),
            Some(_) => ClientSelection::Retry(
                "The selected Rex app is not connected; waiting for its exact ID. No other client will be used".into(),
            ),
            None if apps.len() == 1 => ClientSelection::Pinned(apps[0].clone()),
            None if apps.is_empty() => ClientSelection::Retry("No connected Rex app; waiting for an app".into()),
            None => ClientSelection::Refused(
                "Multiple Rex apps: restart with --client client:<uuid> from rex client ls --json".into(),
            ),
        }
    }

    pub fn select_client(&self, requested: Option<&str>) -> Result<String, String> {
        self.discover_client(requested).result()
    }

    pub fn navigate(&self, entry: &Entry, client: &str) -> Result<(), String> {
        let target = &entry.target;
        if !valid_id(&target.session, "session")
            || !valid_id(&target.window, "window")
            || !valid_id(&target.block, "block")
            || entry.id.session != target.session
            || entry.id.block != target.block
            || agent_name(&entry.id.agent).is_none()
        {
            return Err("Invalid navigation identity; no focus command sent".into());
        }
        self.select_client(Some(client))?;
        let actions: Value = self.json(&["-C", client, "actions", "--json"])?;
        let supported = actions
            .get("actions")
            .and_then(Value::as_array)
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    action.get("name").and_then(Value::as_str) == Some("session.select")
                        && action
                            .pointer("/args/properties/session_id/type")
                            .and_then(Value::as_str)
                            == Some("string")
                        && action
                            .pointer("/args/properties/window_id/type")
                            .and_then(Value::as_str)
                            == Some("string")
                })
            });
        if !supported {
            return Err(
                "The selected Rex app does not support session.select with window_id".into(),
            );
        }
        let mut warnings = Vec::new();
        let pane = self
            .panes(&target.session, &mut warnings)?
            .into_iter()
            .find(|pane| pane.target == *target)
            .ok_or("Pane closed or moved; no focus command sent")?;
        if !self
            .reports(&pane, &mut warnings)
            .contains_key(&entry.id.agent)
        {
            return Err("Agent is no longer reported in this pane; no focus command sent".into());
        }
        self.run(&["focus", "-s", &target.session, &target.block])
            .map_err(|error| format!("Focus request failed; server focus may have changed. App selection was not sent; no automatic retry: {error}"))?;
        let focused: View = self.json(&["session", "inspect", &target.session, "--json"])
            .map_err(|error| format!("Server focus changed, but its result could not be verified; app selection was not sent: {error}"))?;
        if focused.session_id.as_deref() != Some(&target.session)
            || focused.active_window_id.as_deref() != Some(&target.window)
            || !focused.windows.unwrap_or_default().iter().any(|window| {
                window.window_id.as_deref() == Some(&target.window)
                    && window.focused_block_id.as_deref() == Some(&target.block)
            })
        {
            return Err(
                "Server focus changed concurrently; app selection was not sent. Refresh and retry."
                    .into(),
            );
        }
        self.run(&[
            "-C", client, "do", "session.select",
            &format!("session_id={}", target.session), &format!("window_id={}", target.window),
        ]).map_err(|error| format!("Server focus changed, but app selection failed: {error}. Check Remote Control. App action may complete after a timeout; no automatic retry."))?;
        Ok(())
    }

    pub fn events(&self, on_line: impl FnMut(&str)) -> Result<(), String> {
        self.runner
            .stream(&Self::args(&["events", "--all", "--json"]), on_line)
    }
}

fn named(label: &str, id: &str) -> String {
    let label: String = plain(label).chars().take(24).collect();
    let suffix = id.get(id.len().saturating_sub(8)..).unwrap_or(id);
    if label.is_empty() {
        id.into()
    } else {
        format!("{label} [{suffix}]")
    }
}

fn process_agent(value: &Value) -> Option<&'static str> {
    let process = value.get("foreground")?.as_object()?;
    ["script", "argv0", "name"]
        .iter()
        .filter_map(|key| process.get(*key)?.as_str())
        .find_map(|value| {
            let name = value.rsplit('/').next().unwrap_or(value);
            agent_name(name)
        })
}

/// CLI events have server, session_event, and block_event envelopes.
/// Events only invalidate snapshots. Unknown or malformed events also resync.
/// Ignore CLI connection churn caused by our own snapshot commands.
pub fn event_invalidates(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return true;
    };
    !matches!(
        value.get("type").and_then(Value::as_str),
        Some("client_joined" | "client_left" | "client_updated" | "client_performed")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn interpreter_alone_is_not_an_agent() {
        assert_eq!(
            process_agent(&json!({"foreground":{"name":"node","script":"/tmp/not-an-agent.js"}})),
            None
        );
        assert_eq!(
            process_agent(&json!({"foreground":{"name":"node","argv0":"pi"}})),
            Some("pi")
        );
        assert_eq!(process_agent(&json!({"last_exit":{"exit_code":0}})), None);
    }

    #[test]
    fn status_and_structure_events_resync_but_cli_churn_does_not() {
        for line in [
            r#"{"type":"session_created","payload":{}}"#,
            r#"{"type":"session_destroyed","payload":{}}"#,
            r#"{"type":"session_event","payload":{"event":{"type":"block_event","payload":{"name":"program_status_removed"}}}}"#,
            r#"{"type":"future_event"}"#,
            "malformed",
        ] {
            assert!(event_invalidates(line));
        }
        assert!(!event_invalidates(r#"{"type":"client_joined"}"#));
        assert!(!event_invalidates(r#"{"type":"client_left"}"#));
    }
}
