use crate::model::plain;
use std::ffi::OsString;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const RUN_TIMEOUT: Duration = Duration::from_secs(4);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const STDOUT_LIMIT: usize = 4 * 1024 * 1024;
const STDERR_LIMIT: usize = 16 * 1024;
const LINE_LIMIT: usize = 64 * 1024;

/// Runs commands without a shell, in isolated Unix process groups.
/// Output limits fail the command rather than silently truncate its output.
#[derive(Clone)]
pub struct Runner {
    executable: OsString,
    timeout: Duration,
    stop: Arc<AtomicBool>,
}

impl Runner {
    pub fn new(executable: impl Into<OsString>, stop: Arc<AtomicBool>) -> Self {
        Self {
            executable: executable.into(),
            timeout: RUN_TIMEOUT,
            stop,
        }
    }

    /// Captures UTF-8 stdout with a four-second wall timeout.
    /// The stdout and stderr limits are 4 MiB and 16 KiB, respectively.
    pub fn run(&self, args: &[String]) -> Result<String, String> {
        let mut bytes = Vec::new();
        let mut output = String::new();
        self.execute(args, Some(self.timeout), |chunk| {
            if let Some(chunk) = chunk {
                append_bounded(&mut bytes, chunk, STDOUT_LIMIT, "stdout exceeds 4 MiB")
            } else {
                output = String::from_utf8(std::mem::take(&mut bytes))
                    .map_err(|_| "stdout is not valid UTF-8".to_string())?;
                Ok(())
            }
        })?;
        Ok(output)
    }

    /// Calls `on_line` synchronously, without the newline or a trailing CR.
    /// An unterminated final line is also delivered. There is no wall timeout.
    /// Lines are limited to 64 KiB; stderr is limited to 16 KiB.
    /// Callbacks must return promptly to permit cancellation checks.
    pub fn stream(&self, args: &[String], mut on_line: impl FnMut(&str)) -> Result<(), String> {
        let mut line = Vec::new();
        self.execute(args, None, |chunk| {
            if self.stop.load(Ordering::Acquire) {
                return Err("command cancelled".to_string());
            }
            if let Some(chunk) = chunk {
                for part in chunk.split_inclusive(|byte| *byte == b'\n') {
                    if self.stop.load(Ordering::Acquire) {
                        return Err("command cancelled".to_string());
                    }
                    let newline = part.last() == Some(&b'\n');
                    let bytes = if newline {
                        &part[..part.len() - 1]
                    } else {
                        part
                    };
                    append_bounded(&mut line, bytes, LINE_LIMIT, "stream line exceeds 64 KiB")?;
                    if newline {
                        emit_line(&mut line, &mut on_line)?;
                    }
                }
            } else if !line.is_empty() {
                emit_line(&mut line, &mut on_line)?;
            }
            Ok(())
        })
    }

    // `None` is stdout EOF. Each read has a fixed budget, so a busy pipe cannot
    // prevent checks for cancellation, exit, or the wall timeout.
    fn execute(
        &self,
        args: &[String],
        timeout: Option<Duration>,
        mut receive: impl FnMut(Option<&[u8]>) -> Result<(), String>,
    ) -> Result<(), String> {
        let started = Instant::now();
        if self.stop.load(Ordering::Acquire) {
            return Err(self.error(args, "command cancelled", &[]));
        }
        let child = Command::new(&self.executable)
            .args(args)
            .env_remove("REX_CLIENT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|error| self.error(args, &format!("cannot start command: {error}"), &[]))?;
        // Install the guard before any fallible pipe setup.
        let mut group = ChildGroup::new(child);
        let mut stdout = group
            .child
            .stdout
            .take()
            .ok_or_else(|| self.error(args, "stdout pipe is missing", &[]))?;
        let mut stderr = group
            .child
            .stderr
            .take()
            .ok_or_else(|| self.error(args, "stderr pipe is missing", &[]))?;
        nonblocking(&stdout)
            .and_then(|()| nonblocking(&stderr))
            .map_err(|error| {
                self.error(args, &format!("cannot set nonblocking pipes: {error}"), &[])
            })?;

        let mut diagnostics = Vec::new();
        let result = (|| {
            let mut stdout_finished = false;
            let mut stderr_finished = false;
            loop {
                if self.stop.load(Ordering::Acquire) {
                    return Err("command cancelled".to_string());
                }
                if let Some(timeout) = timeout
                    && started.elapsed() >= timeout
                {
                    return Err(format!("command timed out after {timeout:?}"));
                }
                let status = group
                    .poll_exit()
                    .map_err(|error| format!("cannot poll command: {error}"))?;
                let err_read = if stderr_finished {
                    Drain::closed()
                } else {
                    drain(&mut stderr, "stderr", |chunk| {
                        append_bounded(
                            &mut diagnostics,
                            chunk,
                            STDERR_LIMIT,
                            "stderr exceeds 16 KiB",
                        )
                    })?
                };
                stderr_finished = err_read.eof;
                let out_read = if stdout_finished {
                    Drain::closed()
                } else {
                    drain(&mut stdout, "stdout", |chunk| receive(Some(chunk)))?
                };
                if out_read.eof && !stdout_finished {
                    receive(None)?;
                    stdout_finished = true;
                }
                if self.stop.load(Ordering::Acquire) {
                    return Err("command cancelled".to_string());
                }

                if timeout.is_none() && stdout_finished {
                    let forced = status.is_none();
                    let status = group
                        .terminate()
                        .map_err(|error| format!("cannot stop command: {error}"))?;
                    // A stream may close stdout but keep its process or stderr
                    // open. Kill it rather than wait for those handles to close.
                    drain(&mut stderr, "stderr", |chunk| {
                        append_bounded(
                            &mut diagnostics,
                            chunk,
                            STDERR_LIMIT,
                            "stderr exceeds 16 KiB",
                        )
                    })?;
                    return if status.success() || (forced && status.signal() == Some(libc::SIGKILL))
                    {
                        Ok(())
                    } else {
                        Err(format!("command exited with {status}"))
                    };
                }
                if let Some(status) = status
                    && out_read.quiet
                    && err_read.quiet
                {
                    // The leader has exited and its group has been killed.
                    // Do not wait for EOF from a descendant's inherited pipe.
                    if !stdout_finished {
                        receive(None)?;
                    }
                    return if status.success() {
                        Ok(())
                    } else {
                        Err(format!("command exited with {status}"))
                    };
                }
                if !out_read.progress && !err_read.progress {
                    thread::sleep(POLL_INTERVAL);
                }
            }
        })();
        result.map_err(|reason| {
            // Stop writers first, then collect only currently available stderr.
            // Neither this path nor Drop waits for pipe EOF or joins a reader.
            let _ = group.terminate();
            let _ = drain(&mut stderr, "stderr", |chunk| {
                append_bounded(
                    &mut diagnostics,
                    chunk,
                    STDERR_LIMIT,
                    "stderr exceeds 16 KiB",
                )
            });
            self.error(args, &reason, &diagnostics)
        })
    }

    fn error(&self, args: &[String], reason: &str, stderr: &[u8]) -> String {
        let command = plain(&format!("{:?} {:?}", self.executable, args));
        let reason = plain(reason);
        let stderr = plain(&String::from_utf8_lossy(stderr));
        // Put the cause first: display callers may truncate the whole error.
        if stderr.is_empty() {
            format!("{reason}; command: {command}")
        } else {
            format!("{reason}; stderr: {stderr}; command: {command}")
        }
    }
}

fn emit_line(line: &mut Vec<u8>, on_line: &mut impl FnMut(&str)) -> Result<(), String> {
    let text =
        std::str::from_utf8(line).map_err(|_| "stream line is not valid UTF-8".to_string())?;
    on_line(text.strip_suffix('\r').unwrap_or(text));
    line.clear();
    Ok(())
}

fn append_bounded(
    buffer: &mut Vec<u8>,
    bytes: &[u8],
    limit: usize,
    message: &str,
) -> Result<(), String> {
    let available = limit - buffer.len();
    buffer.extend_from_slice(&bytes[..bytes.len().min(available)]);
    if bytes.len() > available {
        Err(message.to_string())
    } else {
        Ok(())
    }
}

fn nonblocking(pipe: &impl AsRawFd) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: The borrowed pipe owns a live descriptor. Neither fcntl closes it.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Drain {
    progress: bool,
    quiet: bool,
    eof: bool,
}

impl Drain {
    fn closed() -> Self {
        Self {
            progress: false,
            quiet: true,
            eof: true,
        }
    }
}

fn drain(
    pipe: &mut impl Read,
    name: &str,
    mut receive: impl FnMut(&[u8]) -> Result<(), String>,
) -> Result<Drain, String> {
    let mut buffer = [0; 8192];
    let mut result = Drain {
        progress: false,
        quiet: false,
        eof: false,
    };
    for _ in 0..8 {
        match pipe.read(&mut buffer) {
            Ok(0) => {
                result.quiet = true;
                result.eof = true;
                break;
            }
            Ok(count) => {
                result.progress = true;
                receive(&buffer[..count])?;
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                result.quiet = true;
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("cannot read {name}: {error}")),
        }
    }
    Ok(result)
}

struct ChildGroup {
    child: Child,
    pgid: libc::pid_t,
    armed: bool,
    status: Option<ExitStatus>,
}

impl ChildGroup {
    fn new(child: Child) -> Self {
        Self {
            pgid: child.id() as libc::pid_t,
            child,
            armed: true,
            status: None,
        }
    }

    fn poll_exit(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        if self.armed {
            if !self.has_exited()? {
                return Ok(None);
            }
            self.kill_group()?;
        }
        self.status = self.child.try_wait()?;
        Ok(self.status)
    }

    fn has_exited(&mut self) -> io::Result<bool> {
        // WNOWAIT keeps the leader's PID reserved until its group is killed.
        // Calling Child::try_wait first could reap it and permit PGID reuse.
        // SAFETY: siginfo is initialized, and P_PID names our unreaped child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                self.child.id() as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                return Ok(false);
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                // An external reaper invalidates ownership. Never signal a
                // possibly reused group ID in this case.
                self.armed = false;
            }
            return Err(error);
        }
        // SAFETY: waitid initialized the SIGCHLD siginfo fields above.
        Ok(unsafe { info.si_pid() } != 0)
    }

    fn kill_group(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        let exit_transition_deadline = Instant::now() + Duration::from_millis(100);
        loop {
            // SAFETY: process_group(0) made pgid the child's isolated group.
            // The leader has not been reaped, so this ID cannot be reused.
            if unsafe { libc::kill(-self.pgid, libc::SIGKILL) } == 0 {
                self.armed = false;
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                self.armed = false;
                return Ok(());
            }
            // Darwin can return EPERM while the leader is exiting, before
            // waitid exposes its zombie status. Retain the PID and retry this
            // short transition; a genuine permission failure remains an error.
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(libc::EPERM) {
                if self.has_exited()? {
                    self.armed = false;
                    return Ok(());
                }
                if Instant::now() < exit_transition_deadline {
                    thread::sleep(Duration::from_millis(1));
                    continue;
                }
            }
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    fn terminate(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        self.kill_group()?;
        let status = self.child.wait()?;
        self.status = Some(status);
        Ok(status)
    }
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        if self.status.is_none() {
            let _ = self.kill_group();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, MutexGuard};
    use tempfile::{TempDir, tempdir};

    // Concurrent cold script launches can exceed short test deadlines on macOS.
    // Each test still uses separate paths and an independent cancellation flag.
    static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

    fn fixture_lock() -> MutexGuard<'static, ()> {
        FIXTURE_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    struct Fixture {
        directory: TempDir,
        executable: PathBuf,
        stop: Arc<AtomicBool>,
    }

    impl Fixture {
        fn new(body: &str) -> Self {
            let directory = tempdir().unwrap();
            let executable = directory.path().join("mock rex ' ; executable");
            fs::write(&executable, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            Self {
                directory,
                executable,
                stop: Arc::new(AtomicBool::new(false)),
            }
        }

        fn runner(&self) -> Runner {
            Runner::new(self.executable.clone(), Arc::clone(&self.stop))
        }

        fn path(&self, name: &str) -> PathBuf {
            self.directory.path().join(name)
        }

        fn pid_args(&self) -> Vec<String> {
            [self.path("parent.pid"), self.path("descendant.pid")]
                .iter()
                .map(|path| path.to_str().unwrap().to_string())
                .collect()
        }

        fn assert_cleaned(&self) {
            let parent = read_pid(&self.path("parent.pid"));
            let descendant = read_pid(&self.path("descendant.pid"));
            let mut status = 0;
            // SAFETY: waitpid only checks the fixture's recorded child PID.
            assert_eq!(
                unsafe { libc::waitpid(parent, &mut status, libc::WNOHANG) },
                -1
            );
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
            assert_stopped(descendant);
        }
    }

    fn read_pid(path: &Path) -> libc::pid_t {
        fs::read_to_string(path).unwrap().trim().parse().unwrap()
    }

    fn wait_for_pid(path: &Path) -> libc::pid_t {
        let started = Instant::now();
        loop {
            if let Ok(contents) = fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse()
            {
                return pid;
            }
            assert!(
                started.elapsed() < Duration::from_secs(8),
                "missing {path:?}"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    fn assert_stopped(pid: libc::pid_t) {
        let started = Instant::now();
        loop {
            // SAFETY: signal 0 only checks whether the fixture process exists.
            if unsafe { libc::kill(pid, 0) } == -1 {
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
                return;
            }
            // A grandchild is reaped by init, not by this runner. Some Linux
            // containers leave adopted zombies in /proc for an extended time.
            #[cfg(target_os = "linux")]
            if fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .map(|(_, rest)| rest.starts_with(" Z "))
                })
                == Some(true)
            {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "process {pid} survived"
            );
            thread::sleep(POLL_INTERVAL);
        }
    }

    const WAITING_GROUP: &str = "\
printf '%s\\n' \"$$\" > \"$1\"
sleep 30 &
printf '%s\\n' \"$!\" > \"$2\"
printf 'ready\\n'
wait";

    #[test]
    fn argv_is_separate_and_stdin_is_closed() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(
            "if IFS= read -r value; then exit 8; fi\nfor value in \"$@\"; do printf '%s\\000' \"$value\"; done",
        );
        let marker = fixture.path("must-not-exist");
        let args = vec![
            "space separated".to_string(),
            "'\"\\$HOME*?".to_string(),
            format!("; touch {}; #", marker.display()),
            "$(exit 9)".to_string(),
            "line\nbreak".to_string(),
            "--client=a=b".to_string(),
            "é".to_string(),
            String::new(),
        ];
        let result = fixture.runner().run(&args).unwrap();
        assert_eq!(
            result,
            args.iter()
                .map(|arg| format!("{arg}\0"))
                .collect::<String>()
        );
        assert!(!marker.exists());
    }

    #[test]
    fn only_client_fallback_is_removed() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(
            "printf '%s\\n' \"${REX_CLIENT-unset}\" \"${REX_SESSION-unset}\" \"${REX_BLOCK-unset}\" \"${REX_SOCKET-unset}\"",
        );
        let expected = format!(
            "unset\n{}\n{}\n{}\n",
            std::env::var("REX_SESSION").unwrap_or_else(|_| "unset".to_string()),
            std::env::var("REX_BLOCK").unwrap_or_else(|_| "unset".to_string()),
            std::env::var("REX_SOCKET").unwrap_or_else(|_| "unset".to_string()),
        );
        assert_eq!(fixture.runner().run(&[]).unwrap(), expected);
    }

    #[test]
    fn nonzero_errors_include_sanitized_stderr_and_command() {
        let _lock = fixture_lock();
        let fixture =
            Fixture::new("printf 'permission\\033[31m\\n denied\u{202e}\\377' >&2\nexit 7");
        let error = fixture.runner().run(&["argument".to_string()]).unwrap_err();
        assert!(error.contains("mock rex"));
        assert!(error.contains("argument"));
        assert!(error.contains("exit status: 7"));
        assert!(error.contains("permission"));
        assert!(error.contains("denied"));
        assert!(!error.chars().any(char::is_control));
        assert!(!error.contains('\u{202e}'));
    }

    #[test]
    fn long_command_context_does_not_hide_the_error_cause() {
        let runner = Runner::new("x".repeat(4096), Arc::new(AtomicBool::new(false)));
        let error = runner.error(
            &["y".repeat(4096)],
            "command timed out after 4s",
            b"diagnostic\nmessage",
        );
        assert!(plain(&error).starts_with("command timed out after 4s; stderr: diagnosticmessage"));
        assert!(error.contains("command: "));
        assert!(error.chars().count() < 600);
        assert!(!error.chars().any(char::is_control));
    }

    #[test]
    fn missing_executable_has_command_context() {
        let _lock = fixture_lock();
        let directory = tempdir().unwrap();
        let runner = Runner::new(
            directory.path().join("missing"),
            Arc::new(AtomicBool::new(false)),
        );
        let error = runner.run(&["arg".to_string()]).unwrap_err();
        assert!(error.contains("missing"));
        assert!(error.contains("arg"));
        assert!(error.contains("cannot start"));
    }

    #[test]
    fn run_accepts_exact_stdout_limit_and_rejects_more() {
        let _lock = fixture_lock();
        let exact = Fixture::new("dd if=/dev/zero bs=1024 count=4096 2>/dev/null");
        assert_eq!(exact.runner().run(&[]).unwrap().len(), STDOUT_LIMIT);
        let excess = Fixture::new("dd if=/dev/zero bs=1024 count=4097 2>/dev/null");
        assert!(
            excess
                .runner()
                .run(&[])
                .unwrap_err()
                .contains("stdout exceeds 4 MiB")
        );
    }

    #[test]
    fn stderr_is_bounded_in_both_modes() {
        let _lock = fixture_lock();
        let exact = Fixture::new("dd if=/dev/zero bs=1024 count=16 >&2 2>/dev/null");
        assert_eq!(exact.runner().run(&[]).unwrap(), "");
        let excess = Fixture::new(
            "printf 'diagnostic-prefix' >&2\ndd if=/dev/zero bs=1024 count=17 >&2 2>/dev/null",
        );
        for streaming in [false, true] {
            let error = if streaming {
                excess.runner().stream(&[], |_| {}).unwrap_err()
            } else {
                excess.runner().run(&[]).unwrap_err()
            };
            assert!(error.contains("stderr exceeds 16 KiB"));
            assert!(error.contains("diagnostic-prefix"));
            assert!(error.len() < 1024);
        }
    }

    #[test]
    fn run_rejects_invalid_utf8() {
        let _lock = fixture_lock();
        let fixture = Fixture::new("printf '\\377'");
        assert!(
            fixture
                .runner()
                .run(&[])
                .unwrap_err()
                .contains("not valid UTF-8")
        );
    }

    #[test]
    fn timeout_kills_and_reaps_the_group_with_inherited_pipes() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(WAITING_GROUP);
        let mut runner = fixture.runner();
        // Allow cold script startup during parallel tests, but stay below the
        // public four-second timeout.
        runner.timeout = Duration::from_secs(1);
        let started = Instant::now();
        assert!(
            runner
                .run(&fixture.pid_args())
                .unwrap_err()
                .contains("timed out after 1s")
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        fixture.assert_cleaned();
    }

    #[test]
    fn run_cancellation_kills_and_reaps_the_group() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(WAITING_GROUP);
        let runner = fixture.runner();
        let args = fixture.pid_args();
        let task = thread::spawn(move || runner.run(&args));
        wait_for_pid(&fixture.path("descendant.pid"));
        fixture.stop.store(true, Ordering::Release);
        let started = Instant::now();
        assert!(task.join().unwrap().unwrap_err().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(2));
        fixture.assert_cleaned();
    }

    #[test]
    fn cancellation_before_spawn_does_not_run_a_fixture() {
        let _lock = fixture_lock();
        let fixture = Fixture::new("printf 'spawned' > \"$1\"");
        let marker = fixture.path("spawned");
        let args = [marker.to_str().unwrap().to_string()];
        fixture.stop.store(true, Ordering::Release);
        assert!(
            fixture
                .runner()
                .run(&args)
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(
            fixture
                .runner()
                .stream(&args, |_| {})
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(!marker.exists());
    }

    #[test]
    fn successful_parent_exit_still_kills_descendants_in_both_modes() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(
            "printf '%s\\n' \"$$\" > \"$1\"\nsleep 30 &\nprintf '%s\\n' \"$!\" > \"$2\"\nprintf 'complete\\n'\nexit 0",
        );
        for streaming in [false, true] {
            let started = Instant::now();
            if streaming {
                let mut lines = Vec::new();
                fixture
                    .runner()
                    .stream(&fixture.pid_args(), |line| lines.push(line.to_string()))
                    .unwrap();
                assert_eq!(lines, ["complete"]);
            } else {
                assert_eq!(
                    fixture.runner().run(&fixture.pid_args()).unwrap(),
                    "complete\n"
                );
            }
            assert!(started.elapsed() < Duration::from_secs(2));
            fixture.assert_cleaned();
        }
    }

    #[test]
    fn stream_delivers_lines_before_exit_and_has_no_run_timeout() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(
            "printf 'first\\n'\nn=0\nwhile [ ! -f \"$1\" ]; do\n n=$((n + 1))\n [ \"$n\" -lt 100 ] || exit 9\n sleep 0.01\ndone\nsleep 0.05\nprintf 'second\\r\\n'",
        );
        let mut runner = fixture.runner();
        runner.timeout = Duration::from_millis(1);
        let ack = fixture.path("acknowledged");
        let mut lines = Vec::new();
        runner
            .stream(&[ack.to_str().unwrap().to_string()], |line| {
                lines.push(line.to_string());
                if line == "first" {
                    fs::write(&ack, b"received").unwrap();
                }
            })
            .unwrap();
        assert_eq!(lines, ["first", "second"]);
    }

    #[test]
    fn stream_eof_delivers_partial_line_and_stops_a_live_group() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(
            "printf '%s\\n' \"$$\" > \"$1\"\nsleep 30 >/dev/null &\nprintf '%s\\n' \"$!\" > \"$2\"\nprintf 'first\\npartial'\nexec 1>&-\nwait",
        );
        let started = Instant::now();
        let mut lines = Vec::new();
        fixture
            .runner()
            .stream(&fixture.pid_args(), |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["first", "partial"]);
        assert!(started.elapsed() < Duration::from_secs(2));
        fixture.assert_cleaned();
    }

    #[test]
    fn short_stream_exit_races_are_not_permission_errors() {
        let _lock = fixture_lock();
        let fixture = Fixture::new("printf 'event\\n'");
        for _ in 0..100 {
            let mut lines = Vec::new();
            fixture
                .runner()
                .stream(&[], |line| lines.push(line.to_string()))
                .unwrap();
            assert_eq!(lines, ["event"]);
        }
    }

    #[test]
    fn empty_stream_does_not_emit_a_line() {
        let _lock = fixture_lock();
        let fixture = Fixture::new("exit 0");
        fixture
            .runner()
            .stream(&[], |_| panic!("unexpected line"))
            .unwrap();
    }

    #[test]
    fn stream_cancellation_stops_only_its_isolated_group() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(WAITING_GROUP);
        let error = fixture
            .runner()
            .stream(&fixture.pid_args(), |line| {
                assert_eq!(line, "ready");
                let parent = read_pid(&fixture.path("parent.pid"));
                // SAFETY: These calls only inspect the fixture and test process.
                assert_eq!(unsafe { libc::getpgid(parent) }, parent);
                assert_ne!(parent, unsafe { libc::getpgrp() });
                fixture.stop.store(true, Ordering::Release);
            })
            .unwrap_err();
        assert!(error.contains("cancelled"));
        fixture.assert_cleaned();
    }

    #[test]
    fn cancelling_one_group_does_not_stop_another() {
        let _lock = fixture_lock();
        let independent = Fixture::new(WAITING_GROUP);
        let mut runner = independent.runner();
        // This test measures isolation, not startup latency or the run deadline.
        runner.timeout = Duration::from_secs(12);
        let args = independent.pid_args();
        let task = thread::spawn(move || runner.run(&args));
        wait_for_pid(&independent.path("descendant.pid"));
        let parent = read_pid(&independent.path("parent.pid"));

        let cancelled = Fixture::new(WAITING_GROUP);
        assert!(
            cancelled
                .runner()
                .stream(&cancelled.pid_args(), |_| {
                    cancelled.stop.store(true, Ordering::Release);
                })
                .unwrap_err()
                .contains("cancelled")
        );
        cancelled.assert_cleaned();
        // SAFETY: Signal 0 only checks the independent fixture's live process.
        assert_eq!(unsafe { libc::kill(parent, 0) }, 0);
        independent.stop.store(true, Ordering::Release);
        assert!(task.join().unwrap().unwrap_err().contains("cancelled"));
        independent.assert_cleaned();
    }

    #[test]
    fn stream_callback_panic_drops_and_reaps_the_group() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(WAITING_GROUP);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = fixture
                .runner()
                .stream(&fixture.pid_args(), |_| panic!("callback panic"));
        }));
        assert!(result.is_err());
        fixture.assert_cleaned();
    }

    #[test]
    fn stream_checks_nonzero_status_and_stderr() {
        let _lock = fixture_lock();
        let fixture = Fixture::new("printf 'event\\n'\nprintf 'stream denied' >&2\nexit 12");
        let mut lines = Vec::new();
        let error = fixture
            .runner()
            .stream(&[], |line| lines.push(line.to_string()))
            .unwrap_err();
        assert_eq!(lines, ["event"]);
        assert!(error.contains("exit status: 12"));
        assert!(error.contains("stream denied"));
    }

    #[test]
    fn stream_line_limit_is_in_bytes_and_includes_partial_lines() {
        let _lock = fixture_lock();
        let exact = Fixture::new("dd if=/dev/zero bs=1024 count=64 2>/dev/null\nprintf '\\n'");
        let mut lengths = Vec::new();
        exact
            .runner()
            .stream(&[], |line| lengths.push(line.len()))
            .unwrap();
        assert_eq!(lengths, [LINE_LIMIT]);
        for suffix in ["", "printf '\\n'"] {
            let excess = Fixture::new(&format!(
                "dd if=/dev/zero bs=1024 count=65 2>/dev/null\n{suffix}"
            ));
            assert!(
                excess
                    .runner()
                    .stream(&[], |_| {})
                    .unwrap_err()
                    .contains("line exceeds 64 KiB")
            );
        }
    }

    #[test]
    fn stream_handles_split_utf8_and_rejects_invalid_final_lines() {
        let _lock = fixture_lock();
        let split = Fixture::new("printf '\\303'\nsleep 0.03\nprintf '\\251\\n'");
        let mut lines = Vec::new();
        split
            .runner()
            .stream(&[], |line| lines.push(line.to_string()))
            .unwrap();
        assert_eq!(lines, ["é"]);
        for body in ["printf '\\377\\n'", "printf '\\377'"] {
            let invalid = Fixture::new(body);
            assert!(
                invalid
                    .runner()
                    .stream(&[], |_| {})
                    .unwrap_err()
                    .contains("not valid UTF-8")
            );
        }
    }

    #[test]
    fn stream_has_no_total_stdout_limit() {
        let _lock = fixture_lock();
        let fixture = Fixture::new(&format!(
            "n=0\nwhile [ \"$n\" -lt 1100 ]; do\n printf '%s\\n' '{}'\n n=$((n + 1))\ndone",
            "x".repeat(4096)
        ));
        let mut count = 0;
        fixture
            .runner()
            .stream(&[], |line| {
                assert_eq!(line.len(), 4096);
                count += 1;
            })
            .unwrap();
        assert_eq!(count, 1100);
    }
}
