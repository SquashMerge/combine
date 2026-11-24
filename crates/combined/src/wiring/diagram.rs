//! Builds the headless diagram check the daemon runs with (SPEC.md, Feedback).

use std::sync::Arc;

use crate::adapters::loader::{DiagramLimits, PeleDiagramCheck};
use crate::ports::DiagramCheck;

/// Starts the loader adapter's diagram check; its warm runtime builds beside the rest of the
/// start. An engine that can't start is logged once and the daemon starts anyway: every check
/// answers `DiagramCheckError::Unavailable` until a rebuild works. Call inside the tokio runtime.
pub fn build(limits: DiagramLimits) -> Arc<dyn DiagramCheck> {
    let (check, warmup) = PeleDiagramCheck::start(limits);
    tokio::spawn(async move {
        if let Err(error) = warmup.finished().await {
            eprintln!("combined: {error}");
        }
    });
    Arc::new(check)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{DiagramCheckError, DiagramVerdict};

    const TINY_CLASS: &str = "classDiagram\n  class Animal\n  class Dog\n  Animal <|-- Dog\n";

    #[tokio::test]
    async fn build_gives_a_checker_that_validates() {
        let check = build(DiagramLimits::default());
        assert_eq!(check.check(TINY_CLASS).await, Ok(DiagramVerdict::Validated));
    }

    #[tokio::test]
    async fn build_keeps_the_daemon_going_when_the_engine_cannot_start() {
        let check = build(DiagramLimits {
            memory_limit: 1024 * 1024,
            ..DiagramLimits::default()
        });
        assert!(matches!(
            check.check(TINY_CLASS).await,
            Err(DiagramCheckError::Unavailable(_))
        ));
    }
}
