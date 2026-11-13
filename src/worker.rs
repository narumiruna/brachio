use crate::model::{Entry, Snapshot};
use crate::rex::{ClientSelection, Rex, event_invalidates};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub enum Update {
    Snapshot(Result<Snapshot, String>),
    Navigation(Result<(), String>),
    Client(Result<String, String>),
}

pub struct Worker {
    pub updates: Receiver<Update>,
    commands: SyncSender<Entry>,
    stop: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
}

impl Worker {
    pub fn start(
        rex: Rex,
        requested_client: Option<String>,
        stop: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (updates_tx, updates) = mpsc::sync_channel(4);
        let (commands, commands_rx) = mpsc::sync_channel(1);
        let dirty = Arc::new(AtomicBool::new(true));
        let stream_error = Arc::new(Mutex::new(None::<String>));
        let client = Arc::new(Mutex::new(ClientSelection::Retry(
            "Discovering Rex app".into(),
        )));
        // Own threads as soon as they start, including partial startup failure.
        let mut worker = Self {
            updates,
            commands,
            stop: stop.clone(),
            threads: Vec::new(),
        };
        let event_thread = {
            let rex = rex.clone();
            let stop = stop.clone();
            let dirty = dirty.clone();
            let stream_error = stream_error.clone();
            thread::Builder::new()
                .name("rex-events".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let result = rex.events(|line| {
                            if let Ok(mut error) = stream_error.lock() {
                                *error = None;
                            }
                            if event_invalidates(line) {
                                dirty.store(true, Ordering::Relaxed);
                            }
                        });
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        if let Ok(mut error) = stream_error.lock() {
                            *error = Some(format!(
                                "Event stream disconnected: {}. Periodic snapshots continue.",
                                result.err().unwrap_or_else(|| "EOF".into())
                            ));
                        }
                        dirty.store(true, Ordering::Relaxed);
                        pause(&stop, Duration::from_secs(1));
                    }
                })
                .map_err(|error| format!("Cannot start event worker: {error}"))?
        };
        worker.threads.push(event_thread);
        let navigation_thread = {
            let rex = rex.clone();
            let stop = stop.clone();
            let dirty = dirty.clone();
            let client = client.clone();
            let updates_tx = updates_tx.clone();
            thread::Builder::new()
                .name("rex-navigation".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let entry = match commands_rx.recv_timeout(Duration::from_millis(50)) {
                            Ok(entry) => entry,
                            Err(mpsc::RecvTimeoutError::Timeout) => continue,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        };
                        // Only this thread mutates Rex focus. Snapshot/event threads are read-only.
                        let selected = client
                            .lock()
                            .map_err(|_| "Client selection state is unavailable".to_string())
                            .and_then(|selection| selection.result());
                        let result = selected.and_then(|id| rex.navigate(&entry, &id));
                        if !publish(&updates_tx, Update::Navigation(result), &stop) {
                            break;
                        }
                        dirty.store(true, Ordering::Relaxed);
                    }
                })
                .map_err(|error| format!("Cannot start navigation worker: {error}"))?
        };
        worker.threads.push(navigation_thread);
        let refresh_thread = {
            let stop = stop.clone();
            thread::Builder::new()
                .name("rex-refresh".into())
                .spawn(move || {
                    let mut next_refresh = Instant::now();
                    let mut next_event_refresh = Instant::now();
                    let mut reported_client = false;
                    while !stop.load(Ordering::Relaxed) {
                        let now = Instant::now();
                        if now >= next_refresh
                            || (now >= next_event_refresh && dirty.swap(false, Ordering::Relaxed))
                        {
                            let retry = client.lock().is_ok_and(|selection| {
                                matches!(*selection, ClientSelection::Retry(_))
                            });
                            if retry {
                                let selection = rex.discover_client(requested_client.as_deref());
                                let changed = match client.lock() {
                                    Ok(mut current) => {
                                        let changed = *current != selection || !reported_client;
                                        *current = selection.clone();
                                        changed
                                    }
                                    Err(_) => return,
                                };
                                if changed
                                    && !publish(
                                        &updates_tx,
                                        Update::Client(selection.result()),
                                        &stop,
                                    )
                                {
                                    break;
                                }
                                reported_client = true;
                            }
                            let mut result = rex.snapshot();
                            if let Ok(snapshot) = &mut result
                                && let Ok(error) = stream_error.lock()
                                && let Some(error) = &*error
                            {
                                snapshot.warnings.push(error.clone());
                            }
                            if !publish(&updates_tx, Update::Snapshot(result), &stop) {
                                break;
                            }
                            // Reconciliation repairs missed/out-of-order events. It never focuses.
                            next_refresh = Instant::now() + Duration::from_secs(2);
                            next_event_refresh = Instant::now() + Duration::from_millis(250);
                        }
                        pause(&stop, Duration::from_millis(50));
                    }
                })
                .map_err(|error| format!("Cannot start refresh worker: {error}"))?
        };
        worker.threads.push(refresh_thread);
        Ok(worker)
    }

    pub fn navigate(&self, entry: Entry) -> Result<(), String> {
        self.commands
            .try_send(entry)
            .map_err(|_| "Navigation worker is busy or stopped; no request queued".into())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn pause(stop: &AtomicBool, duration: Duration) {
    let end = Instant::now() + duration;
    while Instant::now() < end && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(20));
    }
}

fn publish(tx: &SyncSender<Update>, mut update: Update, stop: &AtomicBool) -> bool {
    while !stop.load(Ordering::Relaxed) {
        match tx.try_send(update) {
            Ok(()) => return true,
            Err(TrySendError::Full(value)) => {
                update = value;
                thread::sleep(Duration::from_millis(20));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_update_queue_does_not_block_shutdown() {
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.send(Update::Client(Ok("pinned".into()))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let handle = thread::spawn(move || publish(&tx, Update::Navigation(Ok(())), &flag));
        thread::sleep(Duration::from_millis(40));
        stop.store(true, Ordering::Relaxed);
        assert!(!handle.join().unwrap());
    }
}
