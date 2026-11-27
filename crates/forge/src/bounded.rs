//! Runs a command that may hang (git over the network, a forge CLI, an editor's launcher) with a deadline, so a stalled one
//! never holds a lock for good.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// How long to wait for what a finished command's pipes still hold: a grandchild (ssh under git) may keep them open.
const DRAIN: Duration = Duration::from_secs(2);

/// What a pipe has produced so far, and a signal for when it closed. The bytes read before a timeout are kept.
struct Drain {
    bytes: Arc<Mutex<Vec<u8>>>,
    closed: mpsc::Receiver<()>,
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> Drain {
    let (bytes, (tx, closed)) = (Arc::new(Mutex::new(Vec::new())), mpsc::channel());
    let shared = Arc::clone(&bytes);
    std::thread::spawn(move || {
        if let Some(mut pipe) = pipe {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if let Ok(mut all) = shared.lock() {
                    all.extend_from_slice(&chunk[..n]);
                }
            }
        }
        let _ = tx.send(());
    });
    Drain { bytes, closed }
}

impl Drain {
    /// Everything read once the pipe closed, or within [`DRAIN`] of the command ending.
    fn finish(self) -> Vec<u8> {
        let _ = self.closed.recv_timeout(DRAIN);
        self.bytes.lock().map(|all| all.clone()).unwrap_or_default()
    }
}

/// Stops `child` and everything it started: it leads its own process group, so one `kill` of the group reaches the `git` a `gh`
/// spawned too. The CLI `kill` does it without `unsafe`; the child alone is killed as well in case that did not run.
fn stop(child: &mut Child) {
    let group = format!("-{}", child.id());
    let _ = Command::new("kill").args(["-KILL", "--", &group]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = child.kill();
    let _ = child.wait();
}

/// [`Command::output`] with stdin closed and a deadline. The command leads its own process group; on timeout the whole group is
/// killed and the error says so.
pub fn run(command: &mut Command, limit: Duration) -> io::Result<Output> {
    run_with(command, None, limit)
}

/// [`run`] with `input` written to the command's stdin, then closed.
pub fn run_with_input(command: &mut Command, input: &[u8], limit: Duration) -> io::Result<Output> {
    run_with(command, Some(input), limit)
}

fn run_with(command: &mut Command, input: Option<&[u8]>, limit: Duration) -> io::Result<Output> {
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    let mut child = command.process_group(0).stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    if let (Some(data), Some(mut pipe)) = (input, child.stdin.take()) {
        // On its own thread: a command that does not read must not hold the deadline up. The pipe closes when it is written.
        let data = data.to_vec();
        std::thread::spawn(move || {
            let _ = pipe.write_all(&data);
        });
    }
    let (out, err) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= limit {
            stop(&mut child);
            return Err(io::Error::new(io::ErrorKind::TimedOut, format!("it did not finish in {} seconds and was stopped", limit.as_secs())));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Output { status, stdout: out.finish(), stderr: err.finish() })
}

/// A deadline over a child whose output the caller reads itself: when `limit` passes before the guard drops, the child's process
/// group is killed, which closes its pipes and ends the caller's read. The child must have been started with `process_group(0)`.
pub struct Deadline {
    done: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
}

/// Starts the clock on `child`.
pub fn watch(child: &Child, limit: Duration) -> Deadline {
    let (done, fired) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
    let (flag, ours, group) = (Arc::clone(&done), Arc::clone(&fired), format!("-{}", child.id()));
    std::thread::spawn(move || {
        let started = Instant::now();
        while !flag.load(Ordering::SeqCst) {
            if started.elapsed() >= limit {
                ours.store(true, Ordering::SeqCst);
                let _ = Command::new("kill").args(["-KILL", "--", &group]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    Deadline { done, fired }
}

impl Deadline {
    /// Whether the deadline ran out and the child was killed for it.
    pub fn fired(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }
}

impl Drop for Deadline {
    fn drop(&mut self) {
        self.done.store(true, Ordering::SeqCst);
    }
}

/// What became of a launcher started and not waited for.
#[derive(Debug, PartialEq, Eq)]
pub enum Launched {
    /// It exited successfully within the wait.
    Done,
    /// It was still running when the wait ended (an editor's own process); it is left to run.
    Running,
}

/// Starts `command` with no input and no output attached, and waits up to `wait` for it to exit. A launcher that exits with an
/// error in that time is an error; one still running is left running and reaped on its own thread.
pub fn launch(command: &mut Command, wait: Duration) -> io::Result<Launched> {
    let mut child: Child = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() { Ok(Launched::Done) } else { Err(io::Error::other(format!("it exited with {status}"))) };
        }
        if started.elapsed() >= wait {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return Ok(Launched::Running);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_command_that_does_not_finish_is_stopped_and_one_that_does_keeps_its_output() {
        let started = Instant::now();
        let error = run(&mut sh("sleep 30"), Duration::from_millis(200)).expect_err("timed out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(10), "the wait ended with the deadline");
        let out = run(&mut sh("echo out; echo err >&2; exit 3"), Duration::from_secs(10)).expect("ran");
        assert_eq!(
            (out.status.code(), String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim()),
            (Some(3), "out", "err")
        );
    }

    #[test]
    fn a_timeout_stops_what_the_command_started_and_a_pipe_a_grandchild_holds_keeps_what_was_read() {
        let pidfile = std::env::temp_dir().join(format!("pr-craft-bounded-{}", std::process::id()));
        let script = format!("sleep 30 & echo $! > {}; wait", pidfile.display());
        run(&mut sh(&script), Duration::from_millis(500)).expect_err("timed out");
        let pid = std::fs::read_to_string(&pidfile).expect("the child wrote its helper's pid");
        let _ = std::fs::remove_file(&pidfile);
        std::thread::sleep(Duration::from_millis(200));
        let alive = Command::new("kill").args(["-0", pid.trim()]).stderr(Stdio::null()).status().expect("kill runs").success();
        assert!(!alive, "the helper the command started was stopped with it");
        let out = run(&mut sh("echo partial >&2; sleep 4 &"), Duration::from_secs(10)).expect("the shell itself exits at once");
        assert!(String::from_utf8_lossy(&out.stderr).contains("partial"), "what was read before the pipe was given up on is kept");
    }

    #[test]
    fn input_reaches_the_command_and_a_command_that_never_reads_or_answers_is_still_stopped() {
        let out = run_with_input(&mut sh("cat"), b"hello", Duration::from_secs(10)).expect("ran");
        assert_eq!(String::from_utf8_lossy(&out.stdout), "hello");
        let big = vec![b'x'; 1 << 20];
        let error = run_with_input(&mut sh("sleep 30"), &big, Duration::from_millis(300)).expect_err("it never read its input");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn a_watched_child_is_killed_when_the_deadline_passes_and_left_alone_when_it_ends_first() {
        let mut slow = sh("sleep 30");
        let child = slow.process_group(0).spawn().expect("started");
        let guard = watch(&child, Duration::from_millis(200));
        let mut child = child;
        let status = child.wait().expect("waited");
        assert!(!status.success() && guard.fired(), "the deadline killed it");
        let mut quick = sh("exit 0");
        let mut child = quick.process_group(0).spawn().expect("started");
        let guard = watch(&child, Duration::from_secs(30));
        assert!(child.wait().expect("waited").success() && !guard.fired());
    }

    #[test]
    fn a_launcher_that_keeps_running_is_not_waited_for() {
        let started = Instant::now();
        assert_eq!(launch(&mut sh("sleep 5"), Duration::from_millis(100)).expect("started"), Launched::Running);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(launch(&mut sh("exit 0"), Duration::from_secs(5)).expect("ran"), Launched::Done);
        assert!(launch(&mut sh("exit 2"), Duration::from_secs(5)).is_err());
    }
}
