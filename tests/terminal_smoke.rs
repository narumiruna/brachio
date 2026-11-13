#![cfg(unix)]

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const DEADLINE: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const OUTPUT_LIMIT: usize = 2 * 1024 * 1024;
const ALT_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_LEAVE: &[u8] = b"\x1b[?1049l";
const MOUSE_ENABLE: &[u8] = b"\x1b[?1000h";
const MOUSE_DISABLE: &[u8] = b"\x1b[?1000l";
const SGR_MOUSE_ENABLE: &[u8] = b"\x1b[?1006h";
const SGR_MOUSE_DISABLE: &[u8] = b"\x1b[?1006l";
const PASTE_ENABLE: &[u8] = b"\x1b[?2004h";
const PASTE_DISABLE: &[u8] = b"\x1b[?2004l";
const CURSOR_HIDE: &[u8] = b"\x1b[?25l";
const CURSOR_SHOW: &[u8] = b"\x1b[?25h";

struct MockRex {
    root: TempDir,
    bin: PathBuf,
    executable: PathBuf,
}

impl MockRex {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("create isolated Rex directory");
        let bin = root.path().join("bin");
        fs::create_dir(&bin).expect("create isolated PATH directory");
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        for entry in fs::read_dir(fixtures).expect("read Rex fixtures") {
            let entry = entry.expect("read fixture entry");
            if entry.file_type().expect("read fixture type").is_file() {
                fs::copy(entry.path(), bin.join(entry.file_name())).expect("copy Rex fixture");
            }
        }
        // Shell builtins only: PATH contains no host tools or real Rex executable.
        // Keep the copied rex.sh unchanged and use a separate, offline-only mock.
        let rex = bin.join("rex");
        fs::write(
            &rex,
            b"#!/bin/sh\nprintf '%s\\n' '--call--' \"$@\" >> \"${0%/*}/smoke-rex.log\"\nprintf '%s\\n' 'offline smoke fixture' >&2\nexit 1\n",
        )
        .expect("write offline Rex mock");
        fs::set_permissions(&rex, fs::Permissions::from_mode(0o700))
            .expect("make offline Rex mock executable");
        Self {
            root,
            bin,
            executable: rex,
        }
    }

    #[cfg(target_os = "macos")]
    fn managed_installation() -> Self {
        let mut mock = Self::new();
        let installed = mock.root.path().join(
            "Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex",
        );
        fs::create_dir_all(installed.parent().unwrap()).expect("create app-managed CLI directory");
        fs::rename(&mock.executable, &installed).expect("move CLI outside PATH");
        mock.executable = installed;
        mock
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_brachio"));
        // Do not inherit Rex endpoints, app IDs, or the user's configuration.
        command
            .env_clear()
            .env("PATH", &self.bin)
            .env("HOME", self.root.path())
            .env("TERM", "xterm-256color")
            .env("LC_ALL", "C")
            .current_dir(self.root.path());
        command
    }

    fn was_called(&self) -> bool {
        self.executable.with_file_name("smoke-rex.log").is_file()
    }
}

struct ChildGuard {
    child: Child,
    status: Option<ExitStatus>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            status: None,
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
        }
        Ok(self.status)
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.status.is_none() {
            // Every child has its own process group, never the test runner's group.
            unsafe {
                libc::kill(-(self.child.id() as libc::pid_t), libc::SIGKILL);
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

struct Pty {
    master: File,
    slave: File,
}

impl Pty {
    fn new() -> io::Result<Self> {
        let mut master = -1;
        let mut slave = -1;
        let mut size = libc::winsize {
            ws_row: 28,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // openpty initializes both descriptors on success. Files own them below.
        let result = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::addr_of_mut!(size),
            )
        };
        if result == -1 {
            return Err(io::Error::last_os_error());
        }
        let pty = Self {
            master: unsafe { File::from_raw_fd(master) },
            slave: unsafe { File::from_raw_fd(slave) },
        };
        for file in [&pty.master, &pty.slave] {
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
            if flags == -1
                || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                    == -1
            {
                return Err(io::Error::last_os_error());
            }
        }
        set_nonblocking(&pty.master)?;
        let mut initial = pty.termios()?;
        // Make the cooked starting state explicit even on hosts with other defaults.
        initial.c_lflag |= libc::ICANON | libc::ECHO | libc::ISIG;
        if unsafe { libc::tcsetattr(pty.slave.as_raw_fd(), libc::TCSANOW, &initial) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(pty)
    }

    fn termios(&self) -> io::Result<libc::termios> {
        let mut state = MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(self.slave.as_raw_fd(), state.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { state.assume_init() })
    }

    fn spawn(&self, mut command: Command) -> io::Result<ChildGuard> {
        command
            .stdin(Stdio::from(self.slave.try_clone()?))
            .stdout(Stdio::from(self.slave.try_clone()?))
            .stderr(Stdio::from(self.slave.try_clone()?));
        // Detach from the runner's controlling tty. Use the PTY through stdio only:
        // Darwin revokes slave descriptors when a controlling session leader exits.
        // Only async-signal-safe calls run between fork and exec.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn().map(ChildGuard::new)
    }
}

fn set_nonblocking(file: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

// No read can block. Limit each poll and total captured output, including failures.
fn read_available(reader: &mut impl Read, output: &mut Vec<u8>) -> io::Result<usize> {
    let start = output.len();
    let mut buffer = [0; 4096];
    for _ in 0..32 {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                if output.len() + count > OUTPUT_LIMIT {
                    return Err(io::Error::other("smoke-test output limit exceeded"));
                }
                output.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            // Linux can report EIO when a PTY slave closes instead of EOF.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error),
        }
    }
    Ok(output.len() - start)
}

fn drain(reader: &mut impl Read, output: &mut Vec<u8>) {
    while read_available(reader, output).expect("read child output") != 0 {}
}

fn contains(output: &[u8], sequence: &[u8]) -> bool {
    output.windows(sequence.len()).any(|part| part == sequence)
}

fn assert_termios_restored(before: &libc::termios, after: &libc::termios) {
    assert_eq!(after.c_iflag, before.c_iflag, "input flags not restored");
    assert_eq!(after.c_oflag, before.c_oflag, "output flags not restored");
    assert_eq!(after.c_cflag, before.c_cflag, "control flags not restored");
    assert_eq!(after.c_lflag, before.c_lflag, "local flags not restored");
    assert_eq!(after.c_cc, before.c_cc, "control characters not restored");
    #[cfg(any(target_os = "linux", target_os = "android"))]
    assert_eq!(after.c_line, before.c_line, "line discipline not restored");
    unsafe {
        assert_eq!(libc::cfgetispeed(after), libc::cfgetispeed(before));
        assert_eq!(libc::cfgetospeed(after), libc::cfgetospeed(before));
    }
}

#[derive(Clone, Copy, Debug)]
enum Termination {
    Byte(u8),
    FilterPaste,
    Signal(libc::c_int),
    FloodSignal(libc::c_int),
}

struct InputFlood {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl InputFlood {
    fn start(mut master: File) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !flag.load(Ordering::Relaxed) && Instant::now() < deadline {
                match master.write(&[b'x'; 4096]) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for InputFlood {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn assert_terminal_cleanup(termination: Termination) {
    assert_terminal_cleanup_with_mock(termination, MockRex::new());
}

fn assert_terminal_cleanup_with_mock(termination: Termination, mock: MockRex) {
    let mut pty = Pty::new().expect("open owned PTY");
    let initial = pty.termios().expect("read initial terminal state");
    let mut child = pty.spawn(mock.command()).expect("spawn brachio on PTY");
    let mut output = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    loop {
        read_available(&mut pty.master, &mut output).expect("read terminal startup");
        if let Some(status) = child.try_wait().expect("poll brachio startup") {
            panic!(
                "brachio exited before terminal setup: {status}; output: {:?}",
                String::from_utf8_lossy(&output)
            );
        }
        let state = pty.termios().expect("read terminal startup state");
        if state.c_lflag & (libc::ICANON | libc::ECHO) == 0
            && [
                ALT_ENTER,
                MOUSE_ENABLE,
                SGR_MOUSE_ENABLE,
                PASTE_ENABLE,
                CURSOR_HIDE,
            ]
            .iter()
            .all(|sequence| contains(&output, sequence))
            && mock.was_called()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "terminal setup timed out; output: {:?}",
            String::from_utf8_lossy(&output)
        );
        thread::sleep(POLL_INTERVAL);
    }

    let raw = pty.termios().expect("verify raw mode before termination");
    assert_eq!(raw.c_lflag & (libc::ICANON | libc::ECHO), 0);
    // Ctrl-C must reach the key handler as a byte, not as a tty-generated signal.
    assert_eq!(raw.c_lflag & libc::ISIG, 0);
    let cleanup_start = output.len();
    let mut flood = None;
    match termination {
        Termination::Byte(byte) => pty.master.write_all(&[byte]).expect("send quit byte"),
        Termination::FilterPaste => {
            pty.master
                .write_all(b"/\x1b[200~needle\nq\ng?\x1b[201~")
                .expect("paste multiline filter");
            let deadline = Instant::now() + DEADLINE;
            loop {
                read_available(&mut pty.master, &mut output).expect("read pasted query");
                assert!(
                    child.try_wait().expect("poll paste input").is_none(),
                    "pasted q became a quit command"
                );
                // Ratatui can reuse unchanged screen cells within the query.
                // Its unique command-like suffix and edit-mode hint must still be drawn.
                if contains(&output, b"q g?") && contains(&output, b"Enter Apply") {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "pasted query was not kept as filter text: {:?}",
                    String::from_utf8_lossy(&output)
                );
                thread::sleep(POLL_INTERVAL);
            }
            pty.master
                .write_all(b"\rq")
                .expect("apply filter then quit");
        }
        Termination::Signal(signal) | Termination::FloodSignal(signal) => {
            if matches!(termination, Termination::FloodSignal(_)) {
                flood = Some(InputFlood::start(pty.master.try_clone().unwrap()));
                thread::sleep(Duration::from_millis(20));
            }
            assert_eq!(
                unsafe { libc::kill(child.child.id() as libc::pid_t, signal) },
                0,
                "send termination signal: {}",
                io::Error::last_os_error()
            );
        }
    }

    let deadline = Instant::now()
        + if flood.is_some() {
            Duration::from_secs(2)
        } else {
            DEADLINE
        };
    let status = loop {
        read_available(&mut pty.master, &mut output).expect("read terminal cleanup");
        if let Some(status) = child.try_wait().expect("poll brachio exit") {
            drop(flood.take());
            drain(&mut pty.master, &mut output);
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "brachio did not exit after {termination:?}; output: {:?}",
            String::from_utf8_lossy(&output)
        );
        thread::sleep(POLL_INTERVAL);
    };
    assert!(
        status.success(),
        "brachio failed after {termination:?}: {status}; output: {:?}",
        String::from_utf8_lossy(&output)
    );
    assert_termios_restored(
        &initial,
        &pty.termios().expect("read terminal state after exit"),
    );
    for sequence in [
        PASTE_DISABLE,
        SGR_MOUSE_DISABLE,
        MOUSE_DISABLE,
        ALT_LEAVE,
        CURSOR_SHOW,
    ] {
        assert!(
            contains(&output[cleanup_start..], sequence),
            "missing cleanup sequence {:?} after {termination:?}; output: {:?}",
            String::from_utf8_lossy(sequence),
            String::from_utf8_lossy(&output)
        );
    }
}

fn run_cli(args: &[&str]) -> Output {
    let mock = MockRex::new();
    let mut command = mock.command();
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = ChildGuard::new(command.spawn().expect("spawn nonterminal brachio"));
    let mut stdout = child.child.stdout.take().expect("capture stdout");
    let mut stderr = child.child.stderr.take().expect("capture stderr");
    set_nonblocking(&stdout).expect("set nonblocking stdout");
    set_nonblocking(&stderr).expect("set nonblocking stderr");
    let mut out = Vec::new();
    let mut err = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        read_available(&mut stdout, &mut out).expect("read stdout");
        read_available(&mut stderr, &mut err).expect("read stderr");
        if let Some(status) = child.try_wait().expect("poll nonterminal brachio") {
            drain(&mut stdout, &mut out);
            drain(&mut stderr, &mut err);
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "brachio {args:?} timed out; stdout: {:?}; stderr: {:?}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        thread::sleep(POLL_INTERVAL);
    };
    assert!(!mock.was_called(), "CLI-only paths must not invoke Rex");
    assert!(!out.contains(&0x1b), "terminal escapes in CLI stdout");
    assert!(!err.contains(&0x1b), "terminal escapes in CLI stderr");
    Output {
        status,
        stdout: out,
        stderr: err,
    }
}

#[test]
fn help_without_a_terminal() {
    let output = run_cli(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8_lossy(&output.stdout);
    assert!(help.contains("Usage: brachio"));
    for controls in [
        "Up/Down or j/k select",
        "g/G cycle grouping (None/Agent/Status/CWD/Session; default None)",
        "Tab/Shift+Tab select groups",
        "Left/Right collapse/expand",
        "/ filter, ? help",
        "a/A select Blocked/Error agents",
        "Enter or left click navigate, w details",
        "q/Esc close the current view; Ctrl-C quits",
        "Enter on a collapsed group expands it",
    ] {
        assert!(help.contains(controls), "missing CLI help: {controls}");
    }
    assert!(output.stderr.is_empty());
}

#[test]
fn version_without_a_terminal() {
    let output = run_cli(&["--version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        format!("brachio {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn nonterminal_is_rejected() {
    let output = run_cli(&[]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be terminals"));
}

#[test]
fn invalid_client_is_rejected_before_terminal_setup() {
    for args in [&["--client"][..], &["--client", "client:not-a-uuid"][..]] {
        let output = run_cli(args);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("--client requires a full client:<uuid> ID")
        );
    }
}

#[test]
fn missing_rex_is_rejected_before_terminal_setup() {
    let mock = MockRex::new();
    fs::remove_file(&mock.executable).expect("remove Rex from isolated PATH");
    let mut pty = Pty::new().expect("open owned PTY");
    let initial = pty.termios().unwrap();
    let mut child = pty.spawn(mock.command()).expect("spawn brachio on PTY");
    let mut output = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    let status = loop {
        read_available(&mut pty.master, &mut output).expect("read missing CLI error");
        if let Some(status) = child.try_wait().expect("poll brachio exit") {
            drain(&mut pty.master, &mut output);
            break status;
        }
        assert!(Instant::now() < deadline, "missing CLI check timed out");
        thread::sleep(POLL_INTERVAL);
    };
    assert_eq!(status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output).contains("Rex CLI not found"));
    assert!(!output.contains(&0x1b), "terminal setup before CLI check");
    assert!(!mock.was_called());
    assert_termios_restored(&initial, &pty.termios().unwrap());
}

#[cfg(target_os = "macos")]
#[test]
fn managed_rex_outside_path_starts_and_restores_terminal() {
    let mock = MockRex::managed_installation();
    assert!(!mock.bin.join("rex").exists());
    assert_terminal_cleanup_with_mock(Termination::Byte(b'q'), mock);
}

#[test]
fn bracketed_multiline_filter_paste_is_text_and_restores_terminal() {
    assert_terminal_cleanup(Termination::FilterPaste);
}

#[test]
fn q_restores_terminal() {
    assert_terminal_cleanup(Termination::Byte(b'q'));
}

#[test]
fn ctrl_c_byte_restores_terminal() {
    assert_terminal_cleanup(Termination::Byte(0x03));
}

#[test]
fn sigterm_restores_terminal() {
    assert_terminal_cleanup(Termination::Signal(libc::SIGTERM));
}

#[test]
fn sighup_restores_terminal() {
    assert_terminal_cleanup(Termination::Signal(libc::SIGHUP));
}

#[test]
fn sigterm_during_input_flood_restores_terminal_without_waiting_for_input_eof() {
    assert_terminal_cleanup(Termination::FloodSignal(libc::SIGTERM));
}

#[test]
fn sigint_restores_terminal() {
    assert_terminal_cleanup(Termination::Signal(libc::SIGINT));
}
