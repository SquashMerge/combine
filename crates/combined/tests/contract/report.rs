//! Cases, verdicts and the one-line result record that carries the revision under test.

use std::fs::OpenOptions;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::Command;
use std::sync::OnceLock;

use futures::future::LocalBoxFuture;

use super::harness::{Failure, Outcome};
use super::target::{Fault, Target};

/// The SPEC.md revision this suite checks.
pub const SPEC_REVISION: u32 = 27;

/// One contract case. `needs` lists the faults the target must be able to produce; without
/// them the case is skipped. A `heavy` case is left out of the mutation runs.
pub struct Case<T> {
    pub name: &'static str,
    pub needs: &'static [Fault],
    pub heavy: bool,
    pub run: fn(&mut T) -> LocalBoxFuture<'_, Outcome>,
}

#[derive(Debug, Clone)]
pub enum Verdict {
    Pass,
    Fail(Failure),
    Skip(String),
}

/// The source revision under test: `COMBINE_REVISION`, else the checkout's `HEAD`, else
/// `unknown`.
pub fn source_revision() -> &'static str {
    static REVISION: OnceLock<String> = OnceLock::new();
    REVISION.get_or_init(|| {
        if let Some(revision) = std::env::var("COMBINE_REVISION")
            .ok()
            .filter(|revision| !revision.trim().is_empty())
        {
            return revision.trim().to_string();
        }
        Command::new("git")
            .arg("-C")
            .arg(env!("CARGO_MANIFEST_DIR"))
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|stdout| stdout.trim().to_string())
            .filter(|revision| !revision.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    })
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

/// Prints the result line, appends it to `contract-results.log` in the target's temp
/// directory, and returns it.
pub fn record(case: &str, target: &str, verdict: &Verdict) -> String {
    let result = match verdict {
        Verdict::Pass => "result=pass".to_string(),
        Verdict::Fail(failure) => format!(
            "result=fail check={} reason={}",
            failure.check,
            first_line(&failure.message)
        ),
        Verdict::Skip(reason) => format!("result=skip reason={}", first_line(reason)),
    };
    let line = format!(
        "contract case={case} target={target} spec_rev={SPEC_REVISION} source_rev={} {result}",
        source_revision()
    );
    println!("{line}");
    let path = concat!(env!("CARGO_TARGET_TMPDIR"), "/contract-results.log");
    if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = log.write_all(format!("{line}\n").as_bytes());
    }
    line
}

fn verdict_of<T: Target>(target: &mut T, case: &Case<T>) -> Verdict {
    if let Some(need) = case.needs.iter().find(|fault| !target.supports(**fault)) {
        return Verdict::Skip(format!("the target can't produce {need:?}"));
    }
    match T::block_on((case.run)(target)) {
        Ok(()) => Verdict::Pass,
        Err(failure) => Verdict::Fail(failure),
    }
}

/// Runs the named case on a fresh target, records the verdict and panics on a failure with the
/// result line and the message.
pub fn run_named<T: Target>(make: impl FnOnce() -> T, cases: &[Case<T>], name: &str) {
    let case = cases
        .iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("no contract case named {name}"));
    let mut target = make();
    let verdict = verdict_of(&mut target, case);
    let line = record(name, &target.describe(), &verdict);
    if let Verdict::Fail(failure) = verdict {
        panic!("{line}\n{}", failure.message);
    }
}

/// Runs a case on a fresh target without recording. A panic counts as a failure with check
/// `panic`.
pub fn run_quietly<T: Target>(make: impl FnOnce() -> T, case: &Case<T>) -> Verdict {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let mut target = make();
        verdict_of(&mut target, case)
    }));
    outcome.unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| panic.downcast_ref::<&str>().map(|text| text.to_string()))
            .unwrap_or_else(|| "the case panicked".to_string());
        Verdict::Fail(Failure {
            check: "panic",
            message,
        })
    })
}
