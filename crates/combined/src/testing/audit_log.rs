#![cfg(feature = "test-support")]
//! An audit sink for tests.

use std::sync::{Mutex, PoisonError};

use crate::mcp_server::McpAudit;

/// Collects the audit lines of the MCP calls.
#[derive(Default)]
pub struct AuditLog {
    lines: Mutex<Vec<String>>,
}

impl AuditLog {
    pub fn new() -> AuditLog {
        AuditLog::default()
    }

    /// Every line recorded so far, in order.
    pub fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl McpAudit for AuditLog {
    fn record(&self, line: &str) {
        self.lines
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(line.to_string());
    }
}
