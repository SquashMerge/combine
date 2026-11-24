//! The diagram-check port's spellings and shape. It needs no test support.

use std::collections::HashSet;

use combined::ports::{
    DiagramCheck, DiagramCheckError, DiagramFailure, DiagramVerdict, FailureCode,
};

#[test]
fn failure_codes_have_stable_spellings() {
    let spellings: Vec<&str> = FailureCode::ALL.iter().map(|code| code.as_str()).collect();
    assert_eq!(
        spellings,
        [
            "unsupported_diagram",
            "syntax",
            "semantic",
            "limit",
            "too_large",
            "time_limit",
            "memory_limit"
        ]
    );
    assert_eq!(FailureCode::ALL.iter().collect::<HashSet<_>>().len(), 7);
    for code in FailureCode::ALL {
        assert_eq!(code.to_string(), code.as_str());
    }
}

#[test]
fn verdicts_have_stable_spellings() {
    assert_eq!(DiagramVerdict::Validated.as_str(), "validated");
    assert_eq!(
        DiagramVerdict::ValidatedParserOnly.as_str(),
        "validated_parser_only"
    );
    let failed = DiagramVerdict::Failed(DiagramFailure {
        code: FailureCode::Syntax,
        message: "bad".into(),
        line: None,
        column: None,
        snippet: None,
    });
    assert_eq!(failed.as_str(), "failed");
}

#[test]
fn unavailable_names_its_reason() {
    let error = DiagramCheckError::Unavailable("the engine thread has stopped".into());
    assert_eq!(
        error.to_string(),
        "diagram validation is unavailable: the engine thread has stopped"
    );
}

#[test]
fn no_verdict_names_what_pele_threw() {
    let error = DiagramCheckError::NoVerdict("TypeError: boom".into());
    assert_eq!(
        error.to_string(),
        "the diagram validator gave no verdict: TypeError: boom"
    );
}

#[test]
fn diagram_check_is_object_safe() {
    fn _check(_: &dyn DiagramCheck) {}
}
