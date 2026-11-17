use crossterm::event::{self, Event};
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::Duration;

struct QueuedEvent {
    event: Event,
    frame: Option<u64>,
}

impl QueuedEvent {
    fn new(event: Event, before: u64, after: u64, batch_frame: u64) -> Self {
        Self {
            event,
            frame: (before == after && before == batch_frame && before.is_multiple_of(2))
                .then_some(before),
        }
    }
}

pub struct Input {
    events: Receiver<Result<QueuedEvent, String>>,
    frame: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    changed: bool,
}

impl Input {
    pub fn start(stop: Arc<AtomicBool>) -> Result<Self, String> {
        let (sender, events) = mpsc::sync_channel(256);
        let frame = Arc::new(AtomicU64::new(0));
        let input = Self {
            events,
            frame: frame.clone(),
            stop: stop.clone(),
            changed: false,
        };
        // Crossterm can block or spin inside poll on a partial input sequence or
        // a revoked terminal. Only this process-lifetime thread calls it. Do not
        // join it: the UI must still restore the terminal and stop Rex workers.
        thread::Builder::new()
            .name("terminal-input".into())
            .spawn(move || {
                read_events(
                    |wait| {
                        event::poll(wait).and_then(|ready| {
                            if ready {
                                event::read().map(Some)
                            } else {
                                Ok(None)
                            }
                        })
                    },
                    &frame,
                    &stop,
                    sender,
                );
            })
            .map_err(|error| format!("Cannot start terminal input: {error}"))?;
        Ok(input)
    }

    pub fn recv(&mut self, wait: Duration) -> Result<Option<(Event, bool)>, String> {
        // Do not depend on Crossterm returning from a read after terminal loss.
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("Terminal disconnected".into());
        }
        match self.events.recv_timeout(wait) {
            Ok(result) => {
                let queued = result?;
                self.changed = true;
                let crossed_redraw = queued.frame != Some(self.frame.load(Ordering::SeqCst));
                Ok(Some((queued.event, crossed_redraw)))
            }
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err("Terminal input reader stopped".into()),
        }
    }

    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    pub fn redraw<T>(&self, draw: impl FnOnce() -> T) -> T {
        // Odd generations identify input read while a frame is being drawn.
        self.frame.fetch_add(1, Ordering::SeqCst);
        let result = draw();
        self.frame.fetch_add(1, Ordering::SeqCst);
        result
    }
}

fn read_events(
    mut read: impl FnMut(Duration) -> io::Result<Option<Event>>,
    frame: &AtomicU64,
    stop: &AtomicBool,
    sender: SyncSender<Result<QueuedEvent, String>>,
) {
    let mut batch_frame = 0;
    let mut wait = Duration::from_millis(50);
    while !stop.load(Ordering::Relaxed) {
        let before = frame.load(Ordering::SeqCst);
        let result = read(wait);
        let after = frame.load(Ordering::SeqCst);
        let message = match result {
            Ok(Some(event)) => Ok(QueuedEvent::new(event, before, after, batch_frame)),
            Ok(None) => {
                // A zero-timeout poll can filter an internal cursor response and
                // return false with public events still in Crossterm's parser.
                // Keep the old batch until a waiting poll also returns no event.
                if !wait.is_zero() {
                    // A poll that spans a redraw cannot certify the new frame.
                    batch_frame = before;
                }
                wait = Duration::from_millis(50);
                continue;
            }
            Err(error) => Err(error.to_string()),
        };
        let failed = message.is_err();
        if stop.load(Ordering::Relaxed) || sender.send(message).is_err() || failed {
            break;
        }
        wait = Duration::ZERO;
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    fn click(before: u64, after: u64, batch_frame: u64) -> QueuedEvent {
        QueuedEvent::new(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 1,
                row: 3,
                modifiers: event::KeyModifiers::NONE,
            }),
            before,
            after,
            batch_frame,
        )
    }

    fn idle_input() -> Input {
        let (_sender, events) = mpsc::sync_channel(1);
        Input {
            events,
            frame: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            changed: false,
        }
    }

    #[test]
    fn stable_frame_click_keeps_its_generation() {
        assert_eq!(click(2, 2, 2).frame, Some(2));
    }

    #[test]
    fn clicks_during_or_across_redraw_have_no_safe_frame() {
        assert_eq!(click(1, 1, 1).frame, None);
        assert_eq!(click(0, 2, 0).frame, None);
    }

    #[test]
    fn unread_old_batch_cannot_acquire_a_new_frame_after_backpressure() {
        assert_eq!(click(4, 4, 2).frame, None);
        // Only an empty Crossterm poll advances the batch to the new frame.
        assert_eq!(click(4, 4, 4).frame, Some(4));
    }

    #[test]
    fn queued_click_keeps_the_old_frame_after_redraw() {
        let queued = click(0, 0, 0);
        let input = idle_input();
        input.redraw(|| assert_eq!(input.frame.load(Ordering::SeqCst), 1));
        assert_ne!(queued.frame, Some(input.frame.load(Ordering::SeqCst)));
        assert_eq!(input.frame.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_draw_also_finishes_the_frame_generation() {
        let input = idle_input();
        let result: Result<(), ()> = input.redraw(|| Err(()));
        assert_eq!(result, Err(()));
        assert_eq!(input.frame.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn filtered_zero_timeout_poll_keeps_buffered_old_clicks_quarantined() {
        let frame = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let (sender, receiver) = mpsc::sync_channel(4);
        let mut step = 0;
        read_events(
            |wait| {
                step += 1;
                match step {
                    1 => {
                        // A key wakes the UI, which redraws before the next poll.
                        frame.store(2, Ordering::SeqCst);
                        Ok(Some(Event::FocusGained))
                    }
                    2 | 4 => {
                        // Crossterm filters a cursor response at zero timeout.
                        // Another old-frame event can remain in its parser.
                        assert!(wait.is_zero());
                        Ok(None)
                    }
                    3 | 6 => Ok(Some(click(0, 0, 0).event)),
                    5 => {
                        assert!(!wait.is_zero());
                        Ok(None)
                    }
                    _ => Err(io::Error::other("reader finished")),
                }
            },
            &frame,
            &stop,
            sender,
        );
        receiver.recv().unwrap().unwrap();
        assert_eq!(receiver.recv().unwrap().unwrap().frame, None);
        assert_eq!(receiver.recv().unwrap().unwrap().frame, Some(2));
        assert!(receiver.recv().unwrap().is_err());
        assert!(receiver.recv().is_err());
    }

    #[test]
    fn an_empty_poll_spanning_redraw_does_not_certify_the_new_frame() {
        let frame = AtomicU64::new(0);
        let stop = AtomicBool::new(false);
        let (sender, receiver) = mpsc::sync_channel(2);
        let mut step = 0;
        read_events(
            |_| {
                step += 1;
                match step {
                    1 => {
                        frame.store(2, Ordering::SeqCst);
                        Ok(None)
                    }
                    2 => Ok(Some(click(0, 0, 0).event)),
                    _ => Err(io::Error::other("reader finished")),
                }
            },
            &frame,
            &stop,
            sender,
        );
        assert_eq!(receiver.recv().unwrap().unwrap().frame, None);
        assert!(receiver.recv().unwrap().is_err());
    }

    #[test]
    fn drop_requests_shutdown_without_waiting_for_the_reader() {
        let input = idle_input();
        let stop = input.stop.clone();
        drop(input);
        assert!(stop.load(Ordering::Relaxed));
    }
}
