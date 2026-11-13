use crate::model::{
    Entry, EntryId, Location, Snapshot, Status, Target, agent_name, aggregate, display_text,
    valid_id,
};
use crate::process::Runner;
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const MACOS_REX_EXECUTABLE: &str = "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex";

/// Prefer PATH, then the current app-managed CLI on macOS.
pub fn find_executable() -> Result<PathBuf, String> {
    let path = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_executable_in(path.as_deref(), home.as_deref()).ok_or_else(|| {
        if cfg!(target_os = "macos") {
            "Rex CLI not found in PATH or the macOS Rex installation. Open Rex to install its server, or add rex to PATH."
        } else {
            "Rex CLI not found in PATH. Install the official Rex CLI and add it to PATH."
        }
        .into()
    })
}

fn find_executable_in(path: Option<&OsStr>, home: Option<&Path>) -> Option<PathBuf> {
    let installed = home
        .filter(|home| cfg!(target_os = "macos") && home.is_absolute())
        .map(|home| home.join(MACOS_REX_EXECUTABLE));
    path.into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("rex"))
        .chain(installed)
        .find(|path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        })
}

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
    location: Location,
}

struct Report {
    status: Status,
    records: usize,
    working_dir: Option<String>,
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

    fn panes(
        &self,
        session: &str,
        session_number: usize,
        warnings: &mut Vec<String>,
    ) -> Result<Vec<Pane>, String> {
        let view: View = self.json(&["session", "inspect", session, "--json"])?;
        if view.session_id.as_deref() != Some(session) {
            return Err("Rex returned a different or missing session ID".into());
        }
        let blocks: Blocks = self.json(&["block", "ls", "-s", session, "--json"])?;
        let mut windows = BTreeMap::new();
        let mut ambiguous = BTreeSet::new();
        for (window_index, window) in view.windows.unwrap_or_default().into_iter().enumerate() {
            let Some(id) = window.window_id.filter(|id| valid_id(id, "window")) else {
                warnings.push("Window without a full ID ignored".into());
                continue;
            };
            let default_label = format!("Window {}", &id[id.len() - 8..]);
            let tab = if window.label == default_label {
                format!("Tab {}", window_index + 1).into()
            } else {
                named(&window.label, "Tab", window_index + 1)
            };
            for (pane_index, block) in window
                .layers
                .unwrap_or_default()
                .into_iter()
                .flat_map(|layer| layer.blocks.unwrap_or_default())
                .enumerate()
            {
                if let Some(block) = block.block_id.filter(|id| valid_id(id, "block"))
                    && windows
                        .insert(block.clone(), (id.clone(), tab.clone(), pane_index + 1))
                        .is_some()
                {
                    ambiguous.insert(block);
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
        let session_name = named(&view.label, "Session", session_number);
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
                || location.is_some_and(|(window, _, _)| block.window_id.as_ref() != Some(window))
            {
                warnings.push(format!("Detached, ambiguous, or moved pane {id} ignored"));
                continue;
            }
            let (window, tab, pane_number) = location.expect("checked above");
            panes.push(Pane {
                target: Target {
                    session: session.into(),
                    window: window.clone(),
                    block: id.clone(),
                },
                location: Location {
                    session: session_name.clone(),
                    tab: tab.clone(),
                    pane: named(&block.label, "Pane", *pane_number),
                },
            });
        }
        Ok(panes)
    }

    fn reports(&self, pane: &Pane, warnings: &mut Vec<String>) -> BTreeMap<String, Report> {
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
        let reports = match status.and_then(|value| aggregate(&value)) {
            Ok(reports) => reports,
            Err(error) => {
                warnings.push(format!("Status unavailable for {}: {error}", target.block));
                BTreeMap::new()
            }
        };
        let mut reports: BTreeMap<_, _> = reports
            .into_iter()
            .map(|(app, (status, records))| {
                (
                    app,
                    Report {
                        status,
                        records,
                        working_dir: None,
                    },
                )
            })
            .collect();
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
                    let report = reports.entry(app.into()).or_insert(Report {
                        status: Status::Unknown,
                        records: 0,
                        working_dir: None,
                    });
                    report.working_dir = value
                        .pointer("/foreground/cwd")
                        .and_then(Value::as_str)
                        .filter(|path| {
                            path.len() <= 4096
                                && !path.contains('\0')
                                && std::path::Path::new(path).is_absolute()
                        })
                        .map(str::to_string);
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
        for (session_index, session) in sessions.sessions.into_iter().enumerate() {
            let Some(id) = session.session_id.filter(|id| valid_id(id, "session")) else {
                snapshot
                    .warnings
                    .push("Session without a full ID ignored".into());
                continue;
            };
            if !seen.insert(id.clone()) {
                return Err("Duplicate session IDs in Rex snapshot".into());
            }
            let panes = match self.panes(&id, session_index + 1, &mut snapshot.warnings) {
                Ok(panes) => panes,
                Err(error) => {
                    snapshot
                        .warnings
                        .push(format!("Session {id} unavailable: {error}"));
                    continue;
                }
            };
            for pane in panes {
                for (agent, report) in self.reports(&pane, &mut snapshot.warnings) {
                    snapshot.entries.push(Entry {
                        id: EntryId {
                            session: id.clone(),
                            block: pane.target.block.clone(),
                            agent: agent.clone(),
                        },
                        target: pane.target.clone(),
                        name: agent,
                        status: report.status,
                        location: pane.location.clone(),
                        working_dir: report.working_dir,
                        records: report.records,
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
            .panes(&target.session, 1, &mut warnings)?
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

fn named(label: &str, kind: &str, number: usize) -> Arc<str> {
    let label = display_text(label);
    if label.is_empty() {
        format!("{kind} {number}").into()
    } else if kind != "Session" && label != format!("{kind} {number}") {
        format!("{number}: {label}").into()
    } else {
        label.into()
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
    use std::fs;

    fn write_executable(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn executable_search_preserves_path_order_and_skips_invalid_candidates() {
        let root = tempfile::tempdir().unwrap();
        let directories: Vec<_> = ["missing", "directory", "not-executable", "first", "second"]
            .into_iter()
            .map(|name| root.path().join(name))
            .collect();
        fs::create_dir_all(directories[1].join("rex")).unwrap();
        for directory in &directories[2..] {
            write_executable(&directory.join("rex"));
        }
        fs::set_permissions(
            directories[2].join("rex"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let path = std::env::join_paths(&directories).unwrap();
        assert_eq!(
            find_executable_in(Some(&path), None),
            Some(directories[3].join("rex"))
        );
    }

    #[test]
    fn executable_search_without_path_or_home_finds_nothing() {
        assert_eq!(find_executable_in(None, None), None);
    }

    #[test]
    fn executable_search_ignores_relative_home() {
        assert_eq!(find_executable_in(None, Some(Path::new("relative"))), None);
        assert_eq!(find_executable_in(None, Some(Path::new(""))), None);
    }

    #[test]
    fn executable_search_uses_managed_installation_only_on_macos() {
        let home = tempfile::tempdir().unwrap();
        let installed = home.path().join(MACOS_REX_EXECUTABLE);
        write_executable(&installed);
        assert_eq!(
            find_executable_in(None, Some(home.path())),
            cfg!(target_os = "macos").then_some(installed.clone())
        );
        let bin = home.path().join("custom bin");
        let preferred = bin.join("rex");
        write_executable(&preferred);
        assert_eq!(
            find_executable_in(Some(bin.as_os_str()), Some(home.path())),
            Some(preferred)
        );
        fs::set_permissions(&installed, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(find_executable_in(None, Some(home.path())), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn executable_search_follows_current_installation_symlink() {
        let home = tempfile::tempdir().unwrap();
        let versions = home
            .path()
            .join("Library/Application Support/com.superlogical.rex/RexServer/Versions");
        let installed = versions.join("Current/Rex Server.app/Contents/MacOS/rex");
        let version = versions.join("1956");
        write_executable(&version.join("Rex Server.app/Contents/MacOS/rex"));
        std::os::unix::fs::symlink("1956", versions.join("Current")).unwrap();
        assert_eq!(find_executable_in(None, Some(home.path())), Some(installed));
        fs::remove_dir_all(version).unwrap();
        assert_eq!(find_executable_in(None, Some(home.path())), None);
    }

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
