use crate::model::{Entry, Snapshot};
use crate::rex::{Rex, event_invalidates};
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
        let refresh_thread = {
            let stop = stop.clone();
            thread::Builder::new()
                .name("rex-refresh".into())
                .spawn(move || {
                    let client = rex.select_client(requested_client.as_deref());
                    if !publish(&updates_tx, Update::Client(client.clone()), &stop) {
                        return;
                    }
                    let mut next_refresh = Instant::now();
                    let mut next_event_refresh = Instant::now();
                    while !stop.load(Ordering::Relaxed) {
                        if let Ok(entry) = commands_rx.recv_timeout(Duration::from_millis(50)) {
                            let result = match &client {
                                Ok(client) => rex.navigate(&entry, client),
                                Err(error) => Err(error.clone()),
                            };
                            if !publish(&updates_tx, Update::Navigation(result), &stop) {
                                break;
                            }
                            dirty.store(true, Ordering::Relaxed);
                        }
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let now = Instant::now();
                        if now >= next_refresh
                            || (now >= next_event_refresh && dirty.swap(false, Ordering::Relaxed))
                        {
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
                            // Fixed reconciliation repairs missed, malformed, or out-of-order events.
                            next_refresh = Instant::now() + Duration::from_secs(2);
                            next_event_refresh = Instant::now() + Duration::from_millis(250);
                        }
                    }
                })
        };
        let refresh_thread = match refresh_thread {
            Ok(thread) => thread,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                let _ = event_thread.join();
                return Err(format!("Cannot start refresh worker: {error}"));
            }
        };
        Ok(Self {
            updates,
            commands,
            stop,
            threads: vec![event_thread, refresh_thread],
        })
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
