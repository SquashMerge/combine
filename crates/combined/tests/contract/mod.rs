#![allow(dead_code)]
//! The contract suite: the harness, a simulated client and the runner. A test crate includes
//! this module and the case files it runs; see `contract!`.

/// Expands to `all_cases()`, a test that every case has a test, and one test per case.
///
/// `areas` are the case modules in scope (each has `cases::<T>()`), and `cases` lists every
/// case name of those areas, in order.
///
/// The optional `lane:` / `lane_target:` pair gates every case on a preflight. `lane` is a
/// closure returning `Result<(), String>`, run at the start of each case's test before the
/// target is constructed; on `Err(reason)` the case records `Verdict::Skip(reason)` under the
/// `lane_target` label in the contract report and returns without building a target. The
/// real-core lane uses it so cases skip, with their reason recorded, when the lane is off.
/// Without the pair the preflight always passes.
#[allow(unused_macros)]
macro_rules! contract {
    (
        target: $target:ty,
        make: $make:expr,
        areas: [$($area:ident),* $(,)?],
        cases: [$($case:ident),* $(,)?] $(,)?
    ) => {
        contract!(
            target: $target,
            make: $make,
            lane: || Ok::<(), String>(()),
            lane_target: "unused",
            areas: [$($area),*],
            cases: [$($case),*],
        );
    };
    (
        target: $target:ty,
        make: $make:expr,
        lane: $lane:expr,
        lane_target: $label:literal,
        areas: [$($area:ident),* $(,)?],
        cases: [$($case:ident),* $(,)?] $(,)?
    ) => {
        fn all_cases() -> Vec<$crate::contract::Case<$target>> {
            let mut all = Vec::new();
            $( all.extend($area::cases::<$target>()); )*
            all
        }

        #[test]
        fn every_case_has_a_test() {
            let named: Vec<&str> = vec![$(stringify!($case)),*];
            let defined: Vec<&str> = all_cases().iter().map(|case| case.name).collect();
            assert_eq!(defined, named, "every case needs a test, in order");
        }

        $(
            #[test]
            fn $case() {
                if let Err(reason) = $lane() {
                    $crate::contract::report::record(
                        stringify!($case),
                        $label,
                        &$crate::contract::report::Verdict::Skip(reason),
                    );
                    return;
                }
                $crate::contract::run_named::<$target>($make, &all_cases(), stringify!($case));
            }
        )*
    };
}

#[macro_use]
pub mod harness;
pub mod client;
pub mod fake_target;
pub mod generator;
pub mod report;
pub mod target;

#[allow(unused_imports)]
pub use client::ClientSim;
#[allow(unused_imports)]
pub use harness::{Failure, Harness, Outcome};
#[allow(unused_imports)]
pub use report::{run_named, run_quietly, Case};
#[allow(unused_imports)]
pub use target::{Fault, Target};
