use brachio::model::{Entry, Snapshot, Status};
use brachio::process::Runner;
use brachio::rex::Rex;
use brachio::worker::{Update, Worker};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tempfile::TempDir;

const CLIENT: &str = "client:00000000-0000-0000-0000-000000000001";

// Avoid cold executable launch saturation on macOS. Worker threads within
// each fixture still run concurrently, including the slow-scan navigation test.
static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    dir: TempDir,
    rex: Rex,
    stop: Arc<AtomicBool>,
    _guard: MutexGuard<'static, ()>,
}

impl Fixture {
    fn new() -> Self {
        let guard = FIXTURE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        for file in fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures")).unwrap() {
            let file = file.unwrap();
            fs::copy(file.path(), dir.path().join(file.file_name())).unwrap();
        }
        let executable = dir.path().join("rex.sh");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let rex = Rex::new(Runner::new(executable.into_os_string(), stop.clone()));
        Self {
            _guard: guard,
            dir,
            rex,
            stop,
        }
    }

    fn mode(&self, mode: &str) {
        // Atomic replacement avoids partial reads by the event fixture.
        let path = self.dir.path().join("next-mode");
        fs::write(&path, format!("{mode}\n")).unwrap();
        fs::rename(path, self.dir.path().join("mode")).unwrap();
    }

    fn calls(&self) -> Vec<Vec<String>> {
        fs::read_to_string(self.dir.path().join("log"))
            .unwrap_or_default()
            .split("--call--\n")
            .filter(|part| !part.is_empty())
            .map(|part| part.lines().map(str::to_string).collect())
            .collect()
    }

    fn clear_log(&self) {
        fs::write(self.dir.path().join("log"), "").unwrap();
    }

    fn pi(&self) -> Entry {
        self.rex
            .snapshot()
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.id.agent == "pi" && entry.target.block.ends_with("000000000001"))
            .unwrap()
    }
}

fn mutations(calls: &[Vec<String>]) -> Vec<&Vec<String>> {
    calls
        .iter()
        .filter(|args| args.iter().any(|arg| arg == "focus" || arg == "do"))
        .collect()
}

#[test]
fn multi_session_snapshot_preserves_exact_locations_and_record_rules() {
    let fixture = Fixture::new();
    let snapshot = fixture.rex.snapshot().unwrap();
    assert_eq!(snapshot.entries.len(), 4);
    let pi = snapshot
        .entries
        .iter()
        .find(|entry| entry.records == 2)
        .unwrap();
    assert_eq!(pi.status, Status::Blocked);
    assert_eq!(pi.id.agent, "pi");
    assert_eq!(
        pi.target.session,
        "session:00000000-0000-0000-0000-000000000001"
    );
    assert_eq!(
        pi.target.window,
        "window:00000000-0000-0000-0000-000000000001"
    );
    assert!(
        pi.location.session.as_ref() == "Work"
            && pi.location.tab.as_ref() == "1: Code"
            && pi.location.pane.as_ref() == "Pane 1"
    );
    assert_eq!(
        pi.working_dir.as_deref(),
        Some("/Users/test/projects/brachio")
    );
    assert!(
        snapshot
            .entries
            .iter()
            .filter(|entry| entry.status == Status::Done || entry.id.agent == "codex")
            .all(|entry| entry.working_dir.is_none())
    );
    assert_eq!(
        snapshot
            .entries
            .iter()
            .find(|entry| entry.id.agent == "claude-code")
            .unwrap()
            .working_dir
            .as_deref(),
        Some("/work/other-project")
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.id.agent == "claude-code"
                && entry.status == Status::Unknown
                && entry.records == 0)
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.id.agent == "codex" && entry.status == Status::Unknown)
    );
    assert!(
        snapshot
            .entries
            .iter()
            .any(|entry| entry.id.agent == "pi" && entry.status == Status::Done)
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("Detached"))
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("without a full ID"))
    );
    let calls = fixture.calls();
    for number in [3, 6, 7] {
        let id = format!("block:00000000-0000-0000-0000-{number:012x}");
        assert!(
            !calls.iter().any(|args| args.contains(&id)),
            "must not query nonterminal/detached/unknown-placement blocks"
        );
    }
    assert!(mutations(&calls).is_empty());
}

#[test]
fn directory_changes_do_not_change_agent_identity_or_navigation_target() {
    let fixture = Fixture::new();
    let before = fixture.pi();
    let path = fixture.dir.path().join("process-pi.json");
    let mut process: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    process["foreground"]["cwd"] = "/work/another-project".into();
    fs::write(&path, serde_json::to_vec(&process).unwrap()).unwrap();
    let after = fixture.pi();
    assert_eq!(after.id, before.id);
    assert_eq!(after.target, before.target);
    assert_eq!(after.working_dir.as_deref(), Some("/work/another-project"));
    assert!(mutations(&fixture.calls()).is_empty());
}

#[test]
fn missing_or_unrelated_process_directories_remain_unknown() {
    for cwd in [
        serde_json::Value::Null,
        serde_json::json!(""),
        serde_json::json!("relative/path"),
        serde_json::json!(format!("/{}", "x".repeat(4096))),
        serde_json::json!("/work/\0invalid"),
    ] {
        let fixture = Fixture::new();
        let path = fixture.dir.path().join("process-pi.json");
        let mut process: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        process["foreground"]["cwd"] = cwd;
        fs::write(path, serde_json::to_vec(&process).unwrap()).unwrap();
        assert!(fixture.pi().working_dir.is_none());
    }
    let fixture = Fixture::new();
    fs::copy(
        fixture.dir.path().join("process-node.json"),
        fixture.dir.path().join("process-pi.json"),
    )
    .unwrap();
    assert!(fixture.pi().working_dir.is_none());
}

#[test]
fn missing_and_generated_location_labels_use_readable_positions_not_ids() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("view-a.json");
    let mut view: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    view["label"] = "".into();
    view["windows"][0]["label"] = "Window 00000001".into();
    fs::write(path, serde_json::to_vec(&view).unwrap()).unwrap();
    let path = fixture.dir.path().join("blocks-a.json");
    let mut blocks: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    blocks["blocks"][0]["label"] = "".into();
    fs::write(path, serde_json::to_vec(&blocks).unwrap()).unwrap();
    let pi = fixture.pi();
    assert_eq!(pi.location.session.as_ref(), "Session 1");
    assert_eq!(pi.location.tab.as_ref(), "Tab 1");
    assert_eq!(pi.location.pane.as_ref(), "Pane 1");
    assert_eq!(
        pi.target.block,
        "block:00000000-0000-0000-0000-000000000001"
    );
}

#[test]
fn long_location_names_are_preserved_and_shared_between_pane_entries() {
    let fixture = Fixture::new();
    let label = "human-readable-label/".repeat(30);
    let path = fixture.dir.path().join("view-a.json");
    let mut view: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    view["label"] = label.clone().into();
    view["windows"][0]["label"] = label.clone().into();
    fs::write(path, serde_json::to_vec(&view).unwrap()).unwrap();
    let path = fixture.dir.path().join("blocks-a.json");
    let mut blocks: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    blocks["blocks"][0]["label"] = label.clone().into();
    fs::write(path, serde_json::to_vec(&blocks).unwrap()).unwrap();
    let snapshot = fixture.rex.snapshot().unwrap();
    let pi = snapshot
        .entries
        .iter()
        .find(|entry| entry.records == 2)
        .unwrap();
    assert_eq!(pi.location.session.as_ref(), label);
    assert_eq!(pi.location.tab.as_ref(), format!("1: {label}"));
    assert_eq!(pi.location.pane.as_ref(), format!("1: {label}"));
    let codex = snapshot
        .entries
        .iter()
        .find(|entry| entry.id.agent == "codex")
        .unwrap();
    assert!(Arc::ptr_eq(&pi.location.session, &codex.location.session));
    assert!(Arc::ptr_eq(&pi.location.tab, &codex.location.tab));
}

#[test]
fn status_not_supported_is_explicit_and_process_presence_never_means_working() {
    let fixture = Fixture::new();
    fixture.mode("no-status");
    let snapshot = fixture.rex.snapshot().unwrap();
    assert_eq!(snapshot.entries.len(), 2);
    assert!(
        snapshot
            .entries
            .iter()
            .all(|entry| entry.status == Status::Unknown)
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("not_supported"))
    );
}

#[test]
fn malformed_or_unavailable_server_is_an_error_not_an_empty_success() {
    let fixture = Fixture::new();
    fixture.mode("badjson");
    assert!(
        fixture
            .rex
            .snapshot()
            .unwrap_err()
            .contains("Invalid Rex JSON")
    );
    fixture.mode("offline");
    assert!(fixture.rex.snapshot().unwrap_err().contains("disconnected"));
}

#[test]
fn duplicate_or_incomplete_location_data_cannot_make_navigation_targets() {
    let fixture = Fixture::new();
    let path = fixture.dir.path().join("view-a.json");
    let mut view: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let duplicate = view["windows"][0]["layers"][0]["blocks"][0].clone();
    view["windows"][1]["layers"][0]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    fs::write(path, serde_json::to_vec(&view).unwrap()).unwrap();
    let snapshot = fixture.rex.snapshot().unwrap();
    assert!(
        !snapshot
            .entries
            .iter()
            .any(|entry| entry.target.block.ends_with("000000000001"))
    );
    assert!(
        snapshot
            .warnings
            .iter()
            .any(|warning| warning.contains("ambiguous"))
    );
}

#[test]
fn navigation_checks_identity_then_focuses_before_selecting_only_the_pinned_app() {
    let fixture = Fixture::new();
    let pi = fixture.pi();
    fixture.clear_log();
    fixture.rex.navigate(&pi, CLIENT).unwrap();
    let calls = fixture.calls();
    let actions = mutations(&calls);
    assert_eq!(actions.len(), 2);
    assert_eq!(
        &actions[0][5..],
        ["focus", "-s", &pi.target.session, &pi.target.block]
    );
    assert_eq!(
        &actions[1][5..],
        [
            "-C",
            CLIENT,
            "do",
            "session.select",
            &format!("session_id={}", pi.target.session),
            &format!("window_id={}", pi.target.window)
        ]
    );
    assert!(calls.iter().all(|args| args[0] == "--autostart=false"));
    assert!(calls.iter().all(|args| {
        !args
            .iter()
            .any(|arg| ["send", "send-key", "run", "kill", "close"].contains(&arg.as_str()))
    }));
}

#[test]
fn stale_moved_invalid_and_unsupported_targets_send_no_focus() {
    for mode in ["removed", "moved", "unsupported", "no-client"] {
        let fixture = Fixture::new();
        let pi = fixture.pi();
        fixture.mode(mode);
        fixture.clear_log();
        assert!(fixture.rex.navigate(&pi, CLIENT).is_err(), "{mode}");
        assert!(mutations(&fixture.calls()).is_empty(), "{mode}");
    }
    let fixture = Fixture::new();
    let mut pi = fixture.pi();
    pi.target.block = "right".into();
    fixture.clear_log();
    assert!(fixture.rex.navigate(&pi, CLIENT).is_err());
    assert!(fixture.calls().is_empty());
}

#[test]
fn focus_failure_never_selects_app_and_permission_failure_reports_partial_effect() {
    let fixture = Fixture::new();
    let pi = fixture.pi();
    fixture.mode("focus-fail");
    fixture.clear_log();
    assert!(
        fixture
            .rex
            .navigate(&pi, CLIENT)
            .unwrap_err()
            .contains("pane not found")
    );
    assert_eq!(mutations(&fixture.calls()).len(), 1);
    fixture.mode("permission");
    fixture.clear_log();
    let error = fixture.rex.navigate(&pi, CLIENT).unwrap_err();
    assert!(error.contains("Server focus changed") && error.contains("Remote Control"));
    assert_eq!(mutations(&fixture.calls()).len(), 2);
}

#[test]
fn ambiguous_clients_require_full_id_and_never_fall_back_to_another_app() {
    let fixture = Fixture::new();
    assert_eq!(fixture.rex.select_client(None).unwrap(), CLIENT);
    fixture.mode("multiple");
    assert!(
        fixture
            .rex
            .select_client(None)
            .unwrap_err()
            .contains("Multiple Rex apps")
    );
    assert_eq!(fixture.rex.select_client(Some(CLIENT)).unwrap(), CLIENT);
    assert!(fixture.rex.select_client(Some("macbook")).is_err());
    assert!(
        fixture
            .rex
            .select_client(Some("client:00000000-0000-0000-0000-000000000099"))
            .is_err()
    );
    fixture.mode("no-client");
    assert!(fixture.rex.select_client(Some(CLIENT)).is_err());
}

fn await_snapshot(worker: &Worker, accept: impl Fn(&Snapshot) -> bool) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if let Ok(Update::Snapshot(Ok(snapshot))) =
            worker.updates.recv_timeout(Duration::from_millis(100))
            && accept(&snapshot)
        {
            return snapshot;
        }
    }
    panic!("worker did not publish the expected snapshot before deadline");
}

#[test]
fn events_add_update_remove_and_reconnect_to_the_latest_snapshot_without_focus() {
    let fixture = Fixture::new();
    let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
    let first = await_snapshot(&worker, |snapshot| {
        snapshot
            .entries
            .iter()
            .any(|entry| entry.status == Status::Blocked)
    });
    let pi_id = first
        .entries
        .iter()
        .find(|entry| entry.records == 2)
        .unwrap()
        .id
        .clone();
    fixture.mode("updated");
    let updated = await_snapshot(&worker, |snapshot| {
        snapshot
            .entries
            .iter()
            .any(|entry| entry.id == pi_id && entry.status == Status::Idle)
    });
    assert_eq!(updated.entries, fixture.rex.snapshot().unwrap().entries);
    fixture.mode("removed");
    await_snapshot(&worker, |snapshot| {
        !snapshot.entries.iter().any(|entry| entry.id == pi_id)
    });
    fixture.mode("offline");
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut disconnected = false;
    while Instant::now() < deadline {
        if let Ok(Update::Snapshot(Err(error))) =
            worker.updates.recv_timeout(Duration::from_millis(100))
        {
            assert!(error.contains("disconnected"));
            disconnected = true;
            break;
        }
    }
    assert!(disconnected);
    fixture.mode("normal");
    let recovered = await_snapshot(&worker, |snapshot| {
        snapshot.entries.iter().any(|entry| entry.id == pi_id)
    });
    assert_eq!(recovered.entries, fixture.rex.snapshot().unwrap().entries);
    drop(worker);
    assert!(mutations(&fixture.calls()).is_empty());
}

#[test]
fn missed_events_are_repaired_by_periodic_reconciliation() {
    let fixture = Fixture::new();
    let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
    await_snapshot(&worker, |snapshot| {
        snapshot
            .entries
            .iter()
            .any(|entry| entry.status == Status::Blocked)
    });
    fs::write(fixture.dir.path().join("quiet-events"), "").unwrap();
    fixture.mode("updated");
    await_snapshot(&worker, |snapshot| {
        snapshot
            .entries
            .iter()
            .any(|entry| entry.status == Status::Idle)
    });
    drop(worker);
    assert!(mutations(&fixture.calls()).is_empty());
}

#[test]
fn concurrent_focus_change_prevents_app_selection_and_is_not_retried() {
    let fixture = Fixture::new();
    let pi = fixture.pi();
    fixture.mode("concurrent-move");
    fixture.clear_log();
    assert!(
        fixture
            .rex
            .navigate(&pi, CLIENT)
            .unwrap_err()
            .contains("concurrently")
    );
    let calls = fixture.calls();
    assert_eq!(mutations(&calls).len(), 1);
    assert!(calls.iter().all(|args| !args.iter().any(|arg| arg == "do")));
}

fn await_client(worker: &Worker) -> Result<String, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(Update::Client(result)) = worker.updates.recv_timeout(Duration::from_millis(50)) {
            return result;
        }
    }
    panic!("client discovery result not received");
}

fn await_navigation(worker: &Worker, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(Update::Navigation(result)) =
            worker.updates.recv_timeout(Duration::from_millis(20))
        {
            return result;
        }
    }
    panic!("navigation did not finish within {timeout:?}");
}

#[test]
fn startup_client_discovery_recovers_before_pinning_but_does_not_replace_a_pin() {
    for initial in ["offline", "no-client"] {
        let fixture = Fixture::new();
        fixture.mode(initial);
        let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
        assert!(await_client(&worker).is_err());
        fixture.mode("normal");
        assert_eq!(await_client(&worker).unwrap(), CLIENT);
        let snapshot = await_snapshot(&worker, |snapshot| {
            snapshot.entries.iter().any(|entry| entry.records == 2)
        });
        let pi = snapshot
            .entries
            .into_iter()
            .find(|entry| entry.records == 2)
            .unwrap();
        // Replacing the only connected app must not replace an established pin.
        let replacement = fs::read_to_string(fixture.dir.path().join("clients.json"))
            .unwrap()
            .replace(CLIENT, "client:00000000-0000-0000-0000-000000000002");
        fs::write(fixture.dir.path().join("clients.json"), replacement).unwrap();
        fixture.clear_log();
        worker.navigate(pi).unwrap();
        assert!(
            await_navigation(&worker, Duration::from_secs(4))
                .unwrap_err()
                .contains("selected Rex app is not connected")
        );
        drop(worker);
        assert!(mutations(&fixture.calls()).is_empty());
    }
}

#[test]
fn ambiguous_startup_client_selection_is_not_retried_into_an_arbitrary_pin() {
    let fixture = Fixture::new();
    fixture.mode("multiple");
    let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
    assert!(
        await_client(&worker)
            .unwrap_err()
            .contains("Multiple Rex apps")
    );
    fixture.mode("normal");
    let snapshot = await_snapshot(&worker, |snapshot| {
        snapshot.entries.iter().any(|entry| entry.records == 2)
    });
    let pi = snapshot
        .entries
        .into_iter()
        .find(|entry| entry.records == 2)
        .unwrap();
    worker.navigate(pi).unwrap();
    assert!(
        await_navigation(&worker, Duration::from_secs(4))
            .unwrap_err()
            .contains("Multiple Rex apps")
    );
    drop(worker);
    assert!(mutations(&fixture.calls()).is_empty());
}

#[test]
fn navigation_does_not_wait_for_a_slow_background_session_scan() {
    let fixture = Fixture::new();
    let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
    let first = await_snapshot(&worker, |snapshot| {
        snapshot.entries.iter().any(|entry| entry.records == 2)
    });
    let pi = first
        .entries
        .into_iter()
        .find(|entry| entry.records == 2)
        .unwrap();
    fixture.mode("slow-scan");
    let deadline = Instant::now() + Duration::from_secs(8);
    while !fixture.dir.path().join("scan-waiting").exists() {
        assert!(
            Instant::now() < deadline,
            "background scan did not reach its slow session"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    worker.navigate(pi).unwrap();
    await_navigation(&worker, Duration::from_millis(1500)).unwrap();
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(worker);
    assert_eq!(mutations(&fixture.calls()).len(), 2);
}

#[test]
fn worker_shutdown_cancels_owned_listener() {
    let fixture = Fixture::new();
    let worker = Worker::start(fixture.rex.clone(), None, fixture.stop.clone()).unwrap();
    await_snapshot(&worker, |_| true);
    let start = Instant::now();
    drop(worker);
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(mutations(&fixture.calls()).is_empty());
}
