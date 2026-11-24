//! Headless validation of Mermaid source: how core asks whether a diagram is valid, with no
//! client attached (SPEC.md, Feedback).

use std::fmt;

use async_trait::async_trait;

/// Why a diagram failed. [`FailureCode::as_str`] is its stable spelling on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FailureCode {
    /// `unsupported_diagram`: Pele has no renderer for the diagram's type.
    UnsupportedDiagram,
    /// `syntax`: Pele could not parse the source.
    Syntax,
    /// `semantic`: the source parsed, but Pele can't draw what it says.
    Semantic,
    /// `limit`: a limit inside the validator refused the source: one of Pele's own (edges,
    /// output size) or the engine's stack depth.
    Limit,
    /// `too_large`: the source is over the size cap; no engine ran.
    TooLarge,
    /// `time_limit`: the check ran past its time cap and was stopped.
    TimeLimit,
    /// `memory_limit`: the check needed more than the engine's memory cap and was stopped.
    MemoryLimit,
}

impl FailureCode {
    /// Every code, in declaration order.
    pub const ALL: [FailureCode; 7] = [
        FailureCode::UnsupportedDiagram,
        FailureCode::Syntax,
        FailureCode::Semantic,
        FailureCode::Limit,
        FailureCode::TooLarge,
        FailureCode::TimeLimit,
        FailureCode::MemoryLimit,
    ];

    /// The stable snake_case spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            FailureCode::UnsupportedDiagram => "unsupported_diagram",
            FailureCode::Syntax => "syntax",
            FailureCode::Semantic => "semantic",
            FailureCode::Limit => "limit",
            FailureCode::TooLarge => "too_large",
            FailureCode::TimeLimit => "time_limit",
            FailureCode::MemoryLimit => "memory_limit",
        }
    }
}

impl fmt::Display for FailureCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A diagram that did not validate: what an agent needs to repair it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagramFailure {
    pub code: FailureCode,
    /// What is wrong, in Pele's words or Combine's. Never empty; at most 1,024 bytes.
    pub message: String,
    /// 1-based line of the error: Pele's, or the adapter's anchor when Pele gives none for an
    /// unknown diagram type or a front-matter error. `None` for `too_large`, `time_limit` and
    /// `memory_limit`, and when neither gives a position.
    pub line: Option<u32>,
    /// 1-based column within `line`, in UTF-16 code units (recorded in docs/m2-s2-spike.md,
    /// P5). `None` whenever `line` is.
    pub column: Option<u32>,
    /// The source text around the error, as Pele quotes it, or the anchored line; at most
    /// 1,024 bytes.
    pub snippet: Option<String>,
}

/// The answer to a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramVerdict {
    /// `validated`: Pele's full `render()` drew the diagram.
    Validated,
    /// `validated_parser_only`: only Pele's `parse()` ran, and it accepted the source.
    ValidatedParserOnly,
    /// `failed`.
    Failed(DiagramFailure),
}

impl DiagramVerdict {
    /// `validated`, `validated_parser_only` or `failed`.
    pub fn as_str(&self) -> &'static str {
        match self {
            DiagramVerdict::Validated => "validated",
            DiagramVerdict::ValidatedParserOnly => "validated_parser_only",
            DiagramVerdict::Failed(_) => "failed",
        }
    }
}

/// A check that produced no verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagramCheckError {
    /// The engine is down: it did not start, could not be rebuilt, or its thread has stopped.
    /// Every source fails the same way until a rebuild works. The text says which.
    Unavailable(String),
    /// The engine ran this source, but Pele threw something that is neither a Pele error nor a
    /// cap. The engine has been rebuilt, so other sources may still validate. The text is what
    /// was thrown, as "name: message".
    NoVerdict(String),
}

impl fmt::Display for DiagramCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiagramCheckError::Unavailable(reason) => {
                write!(f, "diagram validation is unavailable: {reason}")
            }
            DiagramCheckError::NoVerdict(detail) => {
                write!(f, "the diagram validator gave no verdict: {detail}")
            }
        }
    }
}

impl std::error::Error for DiagramCheckError {}

/// Checks Mermaid source headlessly. The loader adapter implements it; `testing::FakeDiagramCheck`
/// scripts it for tests.
#[async_trait]
pub trait DiagramCheck: Send + Sync {
    /// Checks `source`. A source over the implementation's size cap is `Failed` with
    /// [`FailureCode::TooLarge`] before any engine runs. The calling thread is never blocked:
    /// the engine runs elsewhere and the future waits for it.
    async fn check(&self, source: &str) -> Result<DiagramVerdict, DiagramCheckError>;
}
