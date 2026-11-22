//! Classifies `ssh` stderr and exit status into failures that retrying cannot fix.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshFailure {
    UnknownHostKey,
    ChangedHostKey,
    PassphraseNotInAgent,
    AuthenticationFailed,
    MissingRemoteBinary,
}

const PASSPHRASE_PATTERNS: [&str; 4] = [
    "incorrect passphrase supplied to decrypt private key",
    "sign_and_send_pubkey: signing failed",
    "agent refused operation",
    "Enter passphrase for key",
];

/// The first match wins. The stderr patterns are ssh's own errors, so they count only with
/// ssh's exit status 255; `None` means the failure is transient.
pub fn classify(code: Option<i32>, stderr: &str) -> Option<SshFailure> {
    if code == Some(127) {
        return Some(SshFailure::MissingRemoteBinary);
    }
    if code != Some(255) {
        return None;
    }
    if stderr.contains("REMOTE HOST IDENTIFICATION HAS CHANGED") {
        Some(SshFailure::ChangedHostKey)
    } else if stderr.contains("Host key verification failed.") {
        Some(SshFailure::UnknownHostKey)
    } else if PASSPHRASE_PATTERNS.iter().any(|p| stderr.contains(p)) {
        Some(SshFailure::PassphraseNotInAgent)
    } else if stderr.contains("Permission denied (")
        || stderr.contains("Too many authentication failures")
    {
        Some(SshFailure::AuthenticationFailed)
    } else {
        None
    }
}

impl SshFailure {
    /// Combine's own text, published whole. Each variant says what to run.
    pub fn message(&self, dest: &str, combined_path: &str) -> String {
        match self {
            SshFailure::UnknownHostKey => format!(
                "The host key for {dest} is not in known_hosts. Run `ssh {dest}` once in a terminal, check the fingerprint and accept it, then retry."
            ),
            SshFailure::ChangedHostKey => format!(
                "The host key for {dest} has changed since it was saved in known_hosts: the machine may have been reinstalled, or someone may be intercepting the connection. Check with the machine, fix known_hosts, run `ssh {dest}` once in a terminal, then retry."
            ),
            SshFailure::PassphraseNotInAgent => format!(
                "Your SSH key for {dest} has a passphrase and isn't loaded in the agent. Run `ssh-add` in a terminal, then retry."
            ),
            SshFailure::AuthenticationFailed => format!(
                "SSH authentication to {dest} failed. Run `ssh {dest}` once in a terminal to check your login; if your key has a passphrase, run `ssh-add` first. Then retry."
            ),
            SshFailure::MissingRemoteBinary => format!(
                "combined was not found at {combined_path} on {dest} (exit 127). Check with `ssh {dest} ls -l {combined_path}`, install Combine on {dest}, or set \"combined_path\" for this host in hosts.json, then retry."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNKNOWN: &str = "No ED25519 host key is known for mini and you have requested strict checking.\nHost key verification failed.\n";
    const CHANGED: &str = "@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\n@    WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!     @\n@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@@\nIT IS POSSIBLE THAT SOMEONE IS DOING SOMETHING NASTY!\nOffending ED25519 key in /Users/example/.ssh/known_hosts:3\nHost key for mini has changed and you have requested strict checking.\nHost key verification failed.\n";
    const PASSPHRASE_A: &str = "Load key \"/Users/example/.ssh/id_ed25519\": incorrect passphrase supplied to decrypt private key\nexample@mini: Permission denied (publickey).\n";
    const PASSPHRASE_B: &str = "sign_and_send_pubkey: signing failed for ED25519 \"/Users/example/.ssh/id_ed25519\" from agent: agent refused operation\nexample@mini: Permission denied (publickey).\n";
    const AUTH: &str =
        "example@mini: Permission denied (publickey,password,keyboard-interactive).\n";
    const AUTH_TOO_MANY: &str =
        "Received disconnect from 192.0.2.10 port 22:2: Too many authentication failures\n";
    const MISSING: &str = "zsh:1: no such file or directory: /opt/homebrew/bin/combined\n";
    const NETWORK: &str = "ssh: connect to host mini port 22: Operation timed out\n";
    const REMOTE_NOISE: &str = "example@mini: Permission denied (publickey).\n";

    #[test]
    fn classifies_unknown_host_key() {
        assert_eq!(
            classify(Some(255), UNKNOWN),
            Some(SshFailure::UnknownHostKey)
        );
    }

    #[test]
    fn classifies_changed_host_key_before_unknown() {
        assert_eq!(
            classify(Some(255), CHANGED),
            Some(SshFailure::ChangedHostKey)
        );
    }

    #[test]
    fn classifies_passphrase_not_in_agent_a() {
        assert_eq!(
            classify(Some(255), PASSPHRASE_A),
            Some(SshFailure::PassphraseNotInAgent)
        );
    }

    #[test]
    fn classifies_passphrase_not_in_agent_b() {
        assert_eq!(
            classify(Some(255), PASSPHRASE_B),
            Some(SshFailure::PassphraseNotInAgent)
        );
    }

    #[test]
    fn classifies_failed_authentication() {
        assert_eq!(
            classify(Some(255), AUTH),
            Some(SshFailure::AuthenticationFailed)
        );
    }

    #[test]
    fn classifies_too_many_authentication_failures() {
        assert_eq!(
            classify(Some(255), AUTH_TOO_MANY),
            Some(SshFailure::AuthenticationFailed)
        );
    }

    #[test]
    fn classifies_missing_remote_binary_by_exit_127() {
        assert_eq!(
            classify(Some(127), MISSING),
            Some(SshFailure::MissingRemoteBinary)
        );
        assert_eq!(
            classify(Some(127), ""),
            Some(SshFailure::MissingRemoteBinary)
        );
    }

    #[test]
    fn network_error_is_not_a_failure() {
        assert_eq!(classify(Some(255), NETWORK), None);
    }

    #[test]
    fn patterns_count_only_with_exit_255() {
        for stderr in [
            UNKNOWN,
            CHANGED,
            PASSPHRASE_A,
            PASSPHRASE_B,
            AUTH,
            AUTH_TOO_MANY,
        ] {
            assert_eq!(classify(Some(1), stderr), None);
            assert_eq!(classify(Some(0), stderr), None);
            assert_eq!(classify(None, stderr), None);
        }
        assert_eq!(classify(Some(1), REMOTE_NOISE), None);
    }

    #[test]
    fn messages_say_what_to_run() {
        let dest = "mini";
        let path = "/opt/homebrew/bin/combined";
        assert_eq!(
            SshFailure::UnknownHostKey.message(dest, path),
            "The host key for mini is not in known_hosts. Run `ssh mini` once in a terminal, check the fingerprint and accept it, then retry."
        );
        assert_eq!(
            SshFailure::ChangedHostKey.message(dest, path),
            "The host key for mini has changed since it was saved in known_hosts: the machine may have been reinstalled, or someone may be intercepting the connection. Check with the machine, fix known_hosts, run `ssh mini` once in a terminal, then retry."
        );
        assert_eq!(
            SshFailure::PassphraseNotInAgent.message(dest, path),
            "Your SSH key for mini has a passphrase and isn't loaded in the agent. Run `ssh-add` in a terminal, then retry."
        );
        assert_eq!(
            SshFailure::AuthenticationFailed.message(dest, path),
            "SSH authentication to mini failed. Run `ssh mini` once in a terminal to check your login; if your key has a passphrase, run `ssh-add` first. Then retry."
        );
        assert_eq!(
            SshFailure::MissingRemoteBinary.message(dest, path),
            "combined was not found at /opt/homebrew/bin/combined on mini (exit 127). Check with `ssh mini ls -l /opt/homebrew/bin/combined`, install Combine on mini, or set \"combined_path\" for this host in hosts.json, then retry."
        );
    }
}
