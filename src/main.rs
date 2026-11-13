use brachio::model::{plain, valid_id};
use brachio::process::Runner;
use brachio::rex::Rex;
use brachio::tui::{Action, App};
use brachio::worker::{Update, Worker};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, MouseEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, IsTerminal};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        TERMINAL_ACTIVE.store(true, Ordering::Relaxed);
        let guard = Self;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // The owner restores after all drawing has stopped, even if its panic
        // hook already made a best-effort early restoration.
        TERMINAL_ACTIVE.store(false, Ordering::Relaxed);
        cleanup_terminal();
    }
}

fn restore_terminal() {
    if TERMINAL_ACTIVE.swap(false, Ordering::Relaxed) {
        cleanup_terminal();
    }
}

fn cleanup_terminal() {
    // Attempt every operation, even if one output operation fails.
    let _ = execute!(io::stdout(), DisableMouseCapture);
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = execute!(io::stdout(), Show);
    let _ = disable_raw_mode();
}

fn main() {
    if let Err(error) = run() {
        eprintln!("brachio: {}", plain(&error));
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut client = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!(
                    "brachio — Rex coding agent status\n\nUsage: brachio [--client client:<uuid>]\n\nSelects the only connected app by default; refuses ambiguous clients.\nKeys: Up/Down select, Enter or left click navigate, q/Esc/Ctrl-C quit.\nNavigation requires Rex app Remote Control. It can change shared session focus."
                );
                return Ok(());
            }
            "--version" | "-V" => {
                println!("brachio {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--client" if client.is_none() => {
                let value = args
                    .next()
                    .ok_or("--client requires a full client:<uuid> ID")?;
                if !valid_id(&value, "client") {
                    return Err(
                        "--client requires a full client:<uuid> ID from rex client ls --json"
                            .into(),
                    );
                }
                client = Some(value);
            }
            _ => return Err(format!("Unknown or repeated argument: {arg}. Use --help.")),
        }
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("stdin and stdout must be terminals; use --help for usage".into());
    }
    let stop = Arc::new(AtomicBool::new(false));
    for signal in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(signal, stop.clone()).map_err(|error| error.to_string())?;
    }
    let runner = Runner::new("rex", stop.clone());
    let worker = Worker::start(Rex::new(runner), client, stop.clone())?;
    let guard = TerminalGuard::enter().map_err(|error| error.to_string())?;
    let previous_hook = std::panic::take_hook();
    let ui_thread = std::thread::current().id();
    let panic_stop = stop.clone();
    let panicked = Arc::new(AtomicBool::new(false));
    let panic_flag = panicked.clone();
    std::panic::set_hook(Box::new(move |info| {
        panic_flag.store(true, Ordering::Relaxed);
        panic_stop.store(true, Ordering::Relaxed);
        // A worker must not leave the alternate screen while the UI is drawing.
        // The UI owner restores after it observes stop; its own panic cannot draw again.
        if std::thread::current().id() == ui_thread {
            restore_terminal();
        }
        previous_hook(info);
    }));
    let mut terminal =
        Terminal::new(CrosstermBackend::new(io::stdout())).map_err(|error| error.to_string())?;
    let mut app = App::default();
    app.message = "Connecting to Rex…".into();
    let mut client_error: Option<String> = None;
    let mut notice = String::new();
    terminal
        .draw(|frame| app.draw(frame))
        .map_err(|error| error.to_string())?;
    let mut discard_queued_mouse = false;
    while !stop.load(Ordering::Relaxed) {
        // Resolve queued input against the last displayed frame, before snapshots
        // can reorder rows. Never rebuild hit regions before consuming old clicks.
        if matches!(
            process_input(
                &mut app,
                &worker,
                &client_error,
                Duration::from_millis(50),
                discard_queued_mouse,
                &stop,
            )?,
            InputState::Quit
        ) {
            break;
        }
        loop {
            let update = match worker.updates.try_recv() {
                Ok(update) => update,
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    app.connected = false;
                    app.message = "Background worker stopped; restart Brachio".into();
                    break;
                }
            };
            match update {
                Update::Snapshot(Ok(snapshot)) => {
                    app.message = client_error.as_ref().unwrap_or(&notice).clone();
                    app.replace_snapshot(snapshot);
                    app.connected = true;
                }
                Update::Snapshot(Err(error)) => {
                    app.connected = false;
                    app.message = format!(
                        "Rex disconnected; entries are stale; navigation disabled: {error}"
                    );
                }
                Update::Navigation(result) => {
                    app.navigating = false;
                    notice = match result {
                        Ok(()) => {
                            "Rex accepted app selection; native display is not verified by Brachio"
                                .into()
                        }
                        Err(error) => error,
                    };
                    app.message = notice.clone();
                }
                Update::Client(result) => match result {
                    Ok(id) => {
                        client_error = None;
                        notice =
                            format!("Target app {id}; session focus is shared with other clients");
                        app.message = notice.clone();
                    }
                    Err(error) => {
                        app.message = error.clone();
                        client_error = Some(error);
                    }
                },
            }
        }
        terminal
            .draw(|frame| app.draw(frame))
            .map_err(|error| error.to_string())?;
        // A mouse event queued during a redraw has no frame-generation token.
        // Discard such mouse-down events instead of guessing which row it meant.
        discard_queued_mouse = match process_input(
            &mut app,
            &worker,
            &client_error,
            Duration::ZERO,
            true,
            &stop,
        )? {
            InputState::Quit => break,
            InputState::Drained => false,
            InputState::Pending => true,
        };
    }
    stop.store(true, Ordering::Relaxed);
    drop(guard);
    drop(worker);
    if panicked.load(Ordering::Relaxed) {
        return Err(
            "A background worker panicked; Brachio stopped and restored the terminal".into(),
        );
    }
    Ok(())
}

fn route_input(app: &mut App, event: Event, crossed_redraw: bool) -> Option<Action> {
    if crossed_redraw
        && matches!(&event, Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)))
    {
        app.message = "List redrawn; click again to select the displayed row".into();
        return None;
    }
    app.handle_event(event)
}

enum InputState {
    Quit,
    Drained,
    Pending,
}

fn process_input(
    app: &mut App,
    worker: &Worker,
    client_error: &Option<String>,
    mut wait: Duration,
    crossed_redraw: bool,
    stop: &AtomicBool,
) -> Result<InputState, String> {
    // A paste or PTY writer must not starve rendering, refresh, or signal exit.
    for _ in 0..256 {
        if stop.load(Ordering::Relaxed) {
            return Ok(InputState::Quit);
        }
        if !event::poll(wait).map_err(|error| error.to_string())? {
            return Ok(InputState::Drained);
        }
        wait = Duration::ZERO;
        let event = event::read().map_err(|error| error.to_string())?;
        match route_input(app, event, crossed_redraw) {
            Some(Action::Quit) => return Ok(InputState::Quit),
            Some(Action::Navigate(entry)) => {
                if let Some(error) = client_error {
                    app.navigating = false;
                    app.message = error.clone();
                } else if let Err(error) = worker.navigate(*entry) {
                    app.navigating = false;
                    app.message = error;
                }
            }
            None => {}
        }
    }
    Ok(InputState::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;
    use brachio::model::{Entry, EntryId, Snapshot, Status, Target};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};
    use ratatui::backend::TestBackend;

    #[test]
    fn clicks_crossing_a_snapshot_redraw_cannot_navigate_a_replacement_row() {
        let entry = |number: &str, status| Entry {
            id: EntryId {
                session: "session".into(),
                block: number.into(),
                agent: "pi".into(),
            },
            target: Target {
                session: "session".into(),
                window: "window".into(),
                block: number.into(),
            },
            name: "pi".into(),
            status,
            location: number.into(),
            records: 1,
        };
        let mut app = App::default();
        app.connected = true;
        app.replace_snapshot(Snapshot {
            entries: vec![entry("old", Status::Working), entry("new", Status::Idle)],
            warnings: vec![],
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 10)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let click = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        // Input already pending before updates still resolves the old displayed row.
        let Some(Action::Navigate(target)) = route_input(&mut app, click.clone(), false) else {
            panic!("expected navigation");
        };
        assert_eq!(target.target.block, "old");
        app.navigating = false;
        app.replace_snapshot(Snapshot {
            entries: vec![entry("new", Status::Blocked), entry("old", Status::Idle)],
            warnings: vec![],
        });
        terminal.draw(|frame| app.draw(frame)).unwrap();
        // A queued coordinate crossing this redraw must not acquire the new row.
        assert!(route_input(&mut app, click, true).is_none());
        assert!(!app.navigating);
        let quit = Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(
            route_input(&mut app, quit, true),
            Some(Action::Quit)
        ));
    }
}
