use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context};

use crate::text::sanitize_multiline_text;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[derive(Debug)]
pub struct BoundedOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

pub fn run_bounded(
    command: &mut Command,
    operation: &str,
    deadline: Duration,
    maximum_stream_bytes: usize,
) -> anyhow::Result<BoundedOutput> {
    #[cfg(unix)]
    {
        run_bounded_unix(command, operation, deadline, maximum_stream_bytes)
    }
    #[cfg(not(unix))]
    {
        run_bounded_portable(command, operation, deadline, maximum_stream_bytes)
    }
}

#[cfg(unix)]
fn run_bounded_unix(
    command: &mut Command,
    operation: &str,
    deadline: Duration,
    maximum_stream_bytes: usize,
) -> anyhow::Result<BoundedOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    command.process_group(0);
    let command_debug = format!("{command:?}");
    let child = command
        .spawn()
        .with_context(|| format!("failed to start {operation}: {command_debug}"))?;
    let mut child = ChildGuard::new(child);

    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for {operation}"))?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for {operation}"))?;
    set_nonblocking(&stdout)?;
    set_nonblocking(&stderr)?;
    let mut stdout = NonblockingCapture::new(stdout, maximum_stream_bytes);
    let mut stderr = NonblockingCapture::new(stderr, maximum_stream_bytes);

    let started = Instant::now();
    loop {
        let status = child
            .try_wait()
            .with_context(|| format!("failed to wait for {operation}: {command_debug}"))?;
        stdout
            .drain()
            .with_context(|| format!("failed to read stdout for {operation}"))?;
        stderr
            .drain()
            .with_context(|| format!("failed to read stderr for {operation}"))?;

        if stdout.closed && stderr.closed {
            if let Some(status) = status {
                let (stdout, stdout_truncated) = stdout.finish();
                let (stderr, stderr_truncated) = stderr.finish();
                return Ok(BoundedOutput {
                    status,
                    stdout,
                    stderr,
                    stdout_truncated,
                    stderr_truncated,
                });
            }
        }

        if started.elapsed() >= deadline {
            let cleanup = child.terminate_and_reap(Duration::from_secs(1));
            // Closing these descriptors is the cancellation mechanism for
            // descendants that escaped the process group while retaining a
            // pipe. There are no reader threads left blocked on them.
            drop(stdout);
            drop(stderr);
            cleanup.with_context(|| {
                format!("{operation} exceeded {deadline:?}; termination failed for {command_debug}")
            })?;
            return Err(anyhow!(
                "{operation} exceeded its {deadline:?} deadline; process group was terminated and reaped; command: {command_debug}"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(not(unix))]
fn run_bounded_portable(
    command: &mut Command,
    operation: &str,
    deadline: Duration,
    maximum_stream_bytes: usize,
) -> anyhow::Result<BoundedOutput> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let command_debug = format!("{command:?}");
    let child = command
        .spawn()
        .with_context(|| format!("failed to start {operation}: {command_debug}"))?;
    let mut child = ChildGuard::new(child);
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("failed to capture stdout for {operation}"))?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("failed to capture stderr for {operation}"))?;
    let stdout_handle = thread::spawn(move || retain_bounded(stdout, maximum_stream_bytes));
    let stderr_handle = thread::spawn(move || retain_bounded(stderr, maximum_stream_bytes));

    let started = Instant::now();
    loop {
        let status = child
            .try_wait()
            .with_context(|| format!("failed to wait for {operation}: {command_debug}"))?;
        if status.is_some() && stdout_handle.is_finished() && stderr_handle.is_finished() {
            let (stdout, stdout_truncated) = stdout_handle
                .join()
                .map_err(|_| anyhow!("stdout reader panicked for {operation}"))??;
            let (stderr, stderr_truncated) = stderr_handle
                .join()
                .map_err(|_| anyhow!("stderr reader panicked for {operation}"))??;
            return Ok(BoundedOutput {
                status: status.expect("checked above"),
                stdout,
                stderr,
                stdout_truncated,
                stderr_truncated,
            });
        }
        if started.elapsed() >= deadline {
            let cleanup = child.terminate_and_reap(Duration::from_secs(1));
            let stdout_join = stdout_handle
                .join()
                .map_err(|_| anyhow!("stdout reader panicked for {operation}"));
            let stderr_join = stderr_handle
                .join()
                .map_err(|_| anyhow!("stderr reader panicked for {operation}"));
            cleanup.with_context(|| {
                format!("{operation} exceeded {deadline:?}; termination failed for {command_debug}")
            })?;
            stdout_join?;
            stderr_join?;
            return Err(anyhow!(
                "{operation} exceeded its {deadline:?} deadline; process was terminated and reaped; command: {command_debug}"
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

struct ChildGuard {
    child: Child,
    reaped: bool,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.reaped = true;
        }
        Ok(status)
    }

    fn terminate_and_reap(&mut self, grace: Duration) -> std::io::Result<()> {
        if self.reaped {
            return Ok(());
        }
        terminate_child(&mut self.child)?;
        let deadline = Instant::now() + grace;
        loop {
            if self.try_wait()?.is_some() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }

        // A process-group ESRCH can race with the direct child still being
        // between exit and reaping. Kill the direct child as a final fallback,
        // then wait unconditionally so this guard cannot leave a zombie.
        let _ = self.child.kill();
        self.child.wait()?;
        self.reaped = true;
        Ok(())
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = terminate_child(&mut self.child);
        let _ = self.child.wait();
        self.reaped = true;
    }
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    let process_group = i32::try_from(child.id()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "child process id does not fit in a Unix pid_t",
        )
    })?;
    // The child is placed in its own process group before spawn, so this
    // also terminates helpers that inherited its stdout/stderr pipes.
    // SAFETY: `process_group` is the positive id returned for the child we
    // placed in its own process group; negating it targets that group.
    let result = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    if result == 0 {
        Ok(())
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) -> std::io::Result<()> {
    child.kill()
}

#[cfg(unix)]
fn set_nonblocking(reader: &impl std::os::unix::io::AsRawFd) -> std::io::Result<()> {
    let fd = reader.as_raw_fd();
    // SAFETY: fcntl operates on the valid descriptor owned by `reader`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fcntl operates on the valid descriptor owned by `reader`.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
struct NonblockingCapture<R> {
    reader: R,
    retained: Vec<u8>,
    maximum_bytes: usize,
    truncated: bool,
    closed: bool,
}

#[cfg(unix)]
impl<R: Read> NonblockingCapture<R> {
    fn new(reader: R, maximum_bytes: usize) -> Self {
        Self {
            reader,
            retained: Vec::with_capacity(maximum_bytes.min(8192)),
            maximum_bytes,
            truncated: false,
            closed: false,
        }
    }

    fn drain(&mut self) -> std::io::Result<()> {
        let mut buffer = [0_u8; 8192];
        loop {
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    self.closed = true;
                    return Ok(());
                }
                Ok(count) => {
                    let remaining = self.maximum_bytes.saturating_sub(self.retained.len());
                    self.retained
                        .extend_from_slice(&buffer[..count.min(remaining)]);
                    self.truncated |= count > remaining;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }

    fn finish(self) -> (Vec<u8>, bool) {
        (self.retained, self.truncated)
    }
}

#[cfg(not(unix))]
fn retain_bounded(mut reader: impl Read, maximum_bytes: usize) -> anyhow::Result<(Vec<u8>, bool)> {
    let mut retained = Vec::with_capacity(maximum_bytes.min(8192));
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let remaining = maximum_bytes.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..count.min(remaining)]);
        truncated |= count > remaining;
    }
    Ok((retained, truncated))
}

pub fn output_details(output: &BoundedOutput) -> String {
    let stderr = sanitize_multiline_text(&String::from_utf8_lossy(&output.stderr));
    let stdout = sanitize_multiline_text(&String::from_utf8_lossy(&output.stdout));
    let mut details = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if output.stderr_truncated || output.stdout_truncated {
        if !details.is_empty() {
            details.push('\n');
        }
        details.push_str("[command output truncated]");
    }
    details
}
