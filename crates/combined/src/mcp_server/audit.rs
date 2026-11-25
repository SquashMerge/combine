//! Where the audit line of a mutating tool call goes.

/// Takes one audit line per create, update, show or remove call. The line never holds the
/// source, the title or any other argument.
pub trait McpAudit: Send + Sync {
    fn record(&self, line: &str);
}

/// Prints each line to the daemon's stderr.
pub struct StderrAudit;

impl McpAudit for StderrAudit {
    fn record(&self, line: &str) {
        eprintln!("combined: {line}");
    }
}
