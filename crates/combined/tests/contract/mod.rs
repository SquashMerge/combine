#![allow(dead_code)]
//! The contract suite: the harness, a simulated client and the runner. A test crate includes
//! this module and the case files it runs; see `contract!`.

/// Expands to `all_cases()`, a test that every case has a test, and one test per case.
///
/// `areas` are the case modules in scope (each has `cases::<T>()`), and `cases` lists every
/// case name of those areas, in order.
#[allow(unused_macros)]
macro_rules! contract {
    (
        target: $target:ty,
        make: $make:expr,
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
