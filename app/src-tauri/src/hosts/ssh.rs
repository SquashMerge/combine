//! The SSH bridge: `ssh -T` with piped stdio, no PTY.
//!
//! Host-key checking is never weakened: the argv holds nothing but the six elements of
//! [`ssh_argv`].

use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::link::{Closer, ConnectError, Connector, ExitInfo, ExitReport, FailureText, Link};

/// The macOS system ssh. It is not looked up on `PATH`, because a GUI app's `PATH` is minimal.
pub const SYSTEM_SSH: &str = "/usr/bin/ssh";

/// How much of the end of stderr is kept.
pub const STDERR_TAIL_BYTES: usize = 65536;

const EXIT_WAIT: Duration = Duration::from_secs(5);
const EXIT_POLL: Duration = Duration::from_millis(20);
const STDERR_JOIN_WAIT: Duration = Duration::from_secs(1);

pub fn ssh_argv(destination: &str, combined_path: &str) -> Vec<String> {
    vec![
        "-T".to_string(),
        "-o".to_string(),
        "BatchMode=yes".to_string(),
        destination.to_string(),
        combined_path.to_string(),
        "connect".to_string(),
    ]
}

pub struct SshConnector {
    program: PathBuf,
    destination: String,
    combined_path: String,
}

impl SshConnector {
    pub fn new(
        program: impl Into<PathBuf>,
        destination: impl Into<String>,
        combined_path: impl Into<String>,
    ) -> Self {
        SshConnector {
            program: program.into(),
            destination: destination.into(),
            combined_path: combined_path.into(),
        }
    }

    fn cannot_run(&self, detail: impl ToString) -> ConnectError {
        ConnectError::Permanent(FailureText {
            text: format!("cannot run {}", self.program.display()),
            detail: Some(detail.to_string()),
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

type Tail = Arc<Mutex<Vec<u8>>>;

/// Keeps the last [`STDERR_TAIL_BYTES`] of stderr and drains it to EOF, so the child never
/// blocks on a full pipe. The returned channel yields once the thread is done.
fn drain_stderr(mut stderr: ChildStderr) -> (Tail, Receiver<()>) {
    let tail: Tail = Arc::new(Mutex::new(Vec::new()));
    let (done, finished) = mpsc::channel();
    let shared = Arc::clone(&tail);
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match stderr.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    let mut kept = lock(&shared);
                    kept.extend_from_slice(&chunk[..n]);
                    if kept.len() > STDERR_TAIL_BYTES {
                        let excess = kept.len() - STDERR_TAIL_BYTES;
                        kept.drain(..excess);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        let _ = done.send(());
    });
    (tail, finished)
}

struct SshExit {
    child: Arc<Mutex<Child>>,
    tail: Tail,
    stderr_done: Receiver<()>,
}

impl SshExit {
    /// Polls for the child's exit, then kills it if it is still running after the wait.
    fn wait_for_exit(&self) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + EXIT_WAIT;
        loop {
            let polled = lock(&self.child).try_wait();
            match polled {
                Ok(Some(status)) => return Some(status),
                Ok(None) if Instant::now() < deadline => thread::sleep(EXIT_POLL),
                Ok(None) => {
                    let mut child = lock(&self.child);
                    let _ = child.kill();
                    return child.wait().ok();
                }
                Err(_) => return None,
            }
        }
    }
}

impl ExitReport for SshExit {
    fn finish(self: Box<Self>) -> Option<ExitInfo> {
        let status = self.wait_for_exit();
        let _ = self.stderr_done.recv_timeout(STDERR_JOIN_WAIT);
        let stderr = String::from_utf8_lossy(&lock(&self.tail)).into_owned();
        Some(ExitInfo {
            code: status.and_then(|s| s.code()),
            stderr,
        })
    }
}

impl Connector for SshConnector {
    fn connect(&self) -> Result<Link, ConnectError> {
        let mut child = Command::new(&self.program)
            .args(ssh_argv(&self.destination, &self.combined_path))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| self.cannot_run(e))?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(self.cannot_run("the child's pipes were not created"));
        };
        let (tail, stderr_done) = drain_stderr(stderr);
        let child = Arc::new(Mutex::new(child));
        let killer = Arc::clone(&child);
        let closer: Closer = Arc::new(move || {
            let _ = lock(&killer).kill();
        });
        Ok(Link::new(
            Box::new(stdout),
            Box::new(stdin),
            closer,
            Box::new(SshExit {
                child,
                tail,
                stderr_done,
            }),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::super::failure::{classify, SshFailure};
    use super::super::handshake::run_handshake;
    use super::super::testing::{
        host_id, write_fake_ssh, write_welcome_file, ShortTempDir, HOST_A,
    };
    use super::*;

    fn connect(script: &std::path::Path) -> Link {
        match SshConnector::new(script, "example@mini", "/opt/combined").connect() {
            Ok(link) => link,
            Err(e) => panic!("connect failed: {e:?}"),
        }
    }

    #[test]
    fn argv_is_exact() {
        assert_eq!(
            ssh_argv("example@mini", "/opt/homebrew/bin/combined"),
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "example@mini",
                "/opt/homebrew/bin/combined",
                "connect"
            ]
        );
    }

    #[test]
    fn argv_never_disables_host_key_checking() {
        let argv = ssh_argv("example@mini", "/opt/homebrew/bin/combined");
        assert_eq!(argv.len(), 6);
        for arg in &argv {
            assert!(!arg.contains("StrictHostKeyChecking"), "{arg}");
            assert!(!arg.contains("UserKnownHostsFile"), "{arg}");
            assert_ne!(arg, "-t");
            assert_ne!(arg, "-tt");
        }
        assert_eq!(SYSTEM_SSH, "/usr/bin/ssh");
    }

    /// What `ssh` writes for a host that is not in known_hosts.
    const UNKNOWN_HOST_KEY: &str = "No ED25519 host key is known for mini and you have requested strict checking.\nHost key verification failed.\n";

    #[test]
    fn spawns_program_with_exact_argv() {
        let dir = ShortTempDir::new();
        let argv_file = dir.path().join("argv.txt");
        let script = write_fake_ssh(
            dir.path(),
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\nprintf '%s' '{UNKNOWN_HOST_KEY}' >&2\nexit 255",
                argv_file.display()
            ),
        );
        let mut link = connect(&script);
        assert!(run_handshake(&mut *link.reader, &mut *link.writer).is_err());
        let exit = link.finish().expect("an ssh link reports its exit");
        assert_eq!(exit.code, Some(255));
        assert_eq!(exit.stderr, UNKNOWN_HOST_KEY);
        assert_eq!(
            classify(exit.code, &exit.stderr),
            Some(SshFailure::UnknownHostKey)
        );
        let written = std::fs::read_to_string(&argv_file).expect("the script wrote its argv");
        assert_eq!(
            written.lines().collect::<Vec<_>>(),
            [
                "-T",
                "-o",
                "BatchMode=yes",
                "example@mini",
                "/opt/combined",
                "connect"
            ]
        );
    }

    #[test]
    fn stdout_carries_frames_stderr_is_captured() {
        let dir = ShortTempDir::new();
        let welcome = write_welcome_file(dir.path(), &host_id(HOST_A));
        let script = write_fake_ssh(
            dir.path(),
            &format!(
                "printf 'noise on stderr\\n' >&2\ncat '{}'\nexec cat >/dev/null",
                welcome.display()
            ),
        );
        let mut link = connect(&script);
        let welcome = run_handshake(&mut *link.reader, &mut *link.writer)
            .unwrap_or_else(|e| panic!("handshake failed: {e:?}"));
        assert_eq!(welcome.host_id, host_id(HOST_A));
        let exit = link.finish().expect("an ssh link reports its exit");
        assert_eq!(exit.code, Some(0));
        assert!(exit.stderr.contains("noise on stderr"), "{:?}", exit.stderr);
    }

    #[test]
    fn stderr_keeps_the_last_64_kib() {
        let dir = ShortTempDir::new();
        let script = write_fake_ssh(
            dir.path(),
            "head -c 70000 /dev/zero | tr '\\0' a >&2; printf 'END\\n' >&2; exit 1",
        );
        let link = connect(&script);
        let exit = link.finish().expect("an ssh link reports its exit");
        assert_eq!(exit.code, Some(1));
        assert_eq!(exit.stderr.len(), STDERR_TAIL_BYTES);
        assert!(exit.stderr.ends_with("END\n"));
    }

    #[test]
    fn spawn_error_is_permanent_with_the_os_error_as_detail() {
        let missing = "/nonexistent/cs5-ssh";
        match SshConnector::new(missing, "example@mini", "/opt/combined").connect() {
            Err(ConnectError::Permanent(ft)) => {
                assert_eq!(ft.text, format!("cannot run {missing}"));
                assert!(ft.detail.is_some_and(|d| !d.is_empty()));
            }
            other => panic!("expected a permanent failure, got {:?}", other.err()),
        }
    }
}
