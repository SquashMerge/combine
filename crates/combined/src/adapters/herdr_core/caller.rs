//! [`CallerIdentity`] for herdr-core: an agent's `HERDR_TERMINAL_ID` is a terminal of this
//! daemon's backend when its `HERDR_SOCKET_PATH` names the socket the daemon connects to. No
//! herdr-core call is made.

use std::path::{Path, PathBuf};

use combine_protocol::TermId;

use super::jsonrpc::{is_remote, term_id};
use crate::ports::CallerIdentity;

/// The longest terminal address accepted from an agent's environment.
const MAX_ADDRESS: usize = 256;

pub struct HerdrCoreCallerIdentity {
    socket: PathBuf,
    /// The socket with symlinks resolved, when that differs from `socket`. Resolved once here;
    /// the caller's spelling is never resolved.
    canonical: Option<PathBuf>,
}

impl HerdrCoreCallerIdentity {
    pub fn new(socket: PathBuf) -> HerdrCoreCallerIdentity {
        let canonical = std::fs::canonicalize(&socket)
            .ok()
            .filter(|canonical| *canonical != socket);
        HerdrCoreCallerIdentity { socket, canonical }
    }
}

impl CallerIdentity for HerdrCoreCallerIdentity {
    fn terminal(&self, terminal_id: &str, terminal_socket: &str) -> Option<TermId> {
        let valid = !terminal_id.is_empty()
            && terminal_id.len() <= MAX_ADDRESS
            && !is_remote(terminal_id)
            && !terminal_id.chars().any(char::is_control);
        let theirs = Path::new(terminal_socket);
        let ours = theirs == self.socket || self.canonical.as_deref() == Some(theirs);
        (valid && ours).then(|| term_id(terminal_id))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let path = PathBuf::from(format!("/tmp/c47b-{}-{n}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Scratch(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const SOCKET: &str = "/tmp/c47b-scratch/herdr.sock";

    fn identity() -> HerdrCoreCallerIdentity {
        HerdrCoreCallerIdentity::new(PathBuf::from(SOCKET))
    }

    #[test]
    fn accepts_its_own_socket_and_makes_an_hc_id() {
        assert_eq!(
            identity().terminal("t1", SOCKET),
            Some(TermId("hc:t1".to_string()))
        );
    }

    #[test]
    fn refuses_another_socket_and_relative_paths() {
        let identity = identity();
        assert_eq!(
            identity.terminal("t1", "/tmp/c47b-scratch/other.sock"),
            None
        );
        assert_eq!(identity.terminal("t1", "herdr.sock"), None);
        assert_eq!(identity.terminal("t1", "./herdr.sock"), None);
        assert_eq!(identity.terminal("t1", ""), None);
    }

    #[test]
    fn refuses_remote_empty_and_control_addresses() {
        let identity = identity();
        assert_eq!(identity.terminal("work/t1", SOCKET), None);
        assert_eq!(identity.terminal("", SOCKET), None);
        assert_eq!(identity.terminal("t\n1", SOCKET), None);
        assert_eq!(identity.terminal("t\u{7f}", SOCKET), None);
        let longest = "a".repeat(MAX_ADDRESS);
        assert!(identity.terminal(&longest, SOCKET).is_some());
        assert_eq!(identity.terminal(&format!("{longest}a"), SOCKET), None);
    }

    #[test]
    fn accepts_the_canonical_form_of_a_symlinked_socket_dir() {
        let scratch = Scratch::new();
        let real = scratch.0.join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("herdr.sock"), b"").unwrap();
        let link = scratch.0.join("link");
        symlink(&real, &link).unwrap();
        let canonical = fs::canonicalize(real.join("herdr.sock")).unwrap();

        let identity = HerdrCoreCallerIdentity::new(link.join("herdr.sock"));
        assert!(identity
            .terminal("t1", link.join("herdr.sock").to_str().unwrap())
            .is_some());
        assert!(identity
            .terminal("t1", canonical.to_str().unwrap())
            .is_some());
    }

    #[test]
    fn does_not_resolve_the_callers_symlinked_spelling() {
        let scratch = Scratch::new();
        let real = scratch.0.join("real");
        fs::create_dir(&real).unwrap();
        fs::write(real.join("herdr.sock"), b"").unwrap();
        let link = scratch.0.join("link");
        symlink(&real, &link).unwrap();
        let canonical = fs::canonicalize(real.join("herdr.sock")).unwrap();

        let identity = HerdrCoreCallerIdentity::new(canonical);
        assert_eq!(
            identity.terminal("t1", link.join("herdr.sock").to_str().unwrap()),
            None
        );
    }
}
