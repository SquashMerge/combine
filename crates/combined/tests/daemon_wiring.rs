#![cfg(feature = "test-support")]
//! The daemon's wiring pieces: the clock, the ID reservation and the test-safety helpers.

mod common;
mod daemon_support;

use std::fs;
use std::path::Path;

use combine_protocol::{Attach, ClientMessage, RequestId, TermId};
use combined::adapters::fs::ids_file;
use combined::core::{Core, CoreConfig};
use combined::model::Millis;
use combined::ports::Capabilities;
use combined::wiring::clock::MonotonicClock;
use combined::wiring::ids::{IdReservation, ID_BLOCK};
use common::request_options;
use daemon_support::{refuse_default_herdr_core, write_daemon_config, DaemonPaths, ShortTempDir};

fn reserved(path: &Path) -> u64 {
    ids_file::load(path).unwrap().unwrap().ids_reserved_through
}

#[test]
fn monotonic_clock_never_goes_backwards() {
    let clock = MonotonicClock::new();
    let mut last = Millis(0);
    for _ in 0..1000 {
        let now = clock.now();
        assert!(now >= last);
        last = now;
    }
    std::thread::sleep(std::time::Duration::from_millis(5));
    let later = clock.now();
    assert!(later >= Millis(5), "{later:?}");
    assert!(
        clock.instant_at(later) <= std::time::Instant::now() + std::time::Duration::from_millis(1)
    );
    assert!(clock.instant_at(Millis(1000)) > clock.instant_at(Millis(10)));
}

#[test]
fn id_reservation_starts_at_1_and_reserves_a_block() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    let (reservation, id_base) = IdReservation::start(&path).unwrap();
    assert_eq!(id_base, 1);
    assert_eq!(reservation.reserved_through(), ID_BLOCK);
    assert_eq!(reserved(&path), ID_BLOCK);
}

#[test]
fn id_reservation_restart_starts_above_the_previous_reservation() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    let (first, _) = IdReservation::start(&path).unwrap();
    let (second, id_base) = IdReservation::start(&path).unwrap();
    assert_eq!(id_base, first.reserved_through() + 1);
    assert_eq!(second.reserved_through(), 2 * ID_BLOCK);
    assert_eq!(reserved(&path), 2 * ID_BLOCK);
}

#[test]
fn id_reservation_extends_when_half_a_block_remains() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    let (mut reservation, _) = IdReservation::start(&path).unwrap();
    reservation.keep_ahead(ID_BLOCK / 2 - 1).unwrap();
    assert_eq!(reserved(&path), ID_BLOCK);
    reservation.keep_ahead(ID_BLOCK / 2).unwrap();
    assert_eq!(reservation.reserved_through(), 2 * ID_BLOCK);
    assert_eq!(reserved(&path), 2 * ID_BLOCK);
    reservation.keep_ahead(ID_BLOCK / 2).unwrap();
    assert_eq!(reserved(&path), 2 * ID_BLOCK);
}

#[test]
fn id_reservation_refuses_an_invalid_file_and_leaves_it() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    fs::write(&path, "garbage").unwrap();
    let error = IdReservation::start(&path).unwrap_err();
    assert_eq!(error.exit_code(), 2);
    assert_eq!(fs::read_to_string(&path).unwrap(), "garbage");
}

#[test]
fn id_reservation_refuses_to_overflow() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    ids_file::store(&path, &ids_file::IdsFile::new(u64::MAX)).unwrap();
    assert_eq!(IdReservation::start(&path).unwrap_err().exit_code(), 2);
}

fn caps() -> Capabilities {
    Capabilities {
        ordered_attach: true,
        byte_credit: false,
        initial_credit: 0,
        display_queries: true,
        automatic_replies: true,
        resync_signal: true,
        agent_state: false,
    }
}

/// Issues two attachment IDs from a core that starts at `id_base`; returns its high water.
fn issue_ids(id_base: u64) -> u64 {
    let config = CoreConfig {
        id_base,
        ..CoreConfig::default()
    };
    let mut core = Core::new(config, caps()).unwrap();
    let connection = core.connect();
    for request in 1..=2 {
        let attach = ClientMessage::Attach(Attach {
            request: RequestId(request),
            terminal: TermId(format!("fake:{request}")),
            options: request_options(80, 24, false),
        });
        core.client_message(connection, attach, Millis(0));
    }
    core.high_water()
}

#[test]
fn core_ids_after_restart_exceed_the_previous_high_water() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    let (_, first_base) = IdReservation::start(&path).unwrap();
    let first_high = issue_ids(first_base);
    assert!(
        first_high >= first_base,
        "the first incarnation issued no ID"
    );
    let (_, second_base) = IdReservation::start(&path).unwrap();
    assert!(second_base > first_high);
    assert!(issue_ids(second_base) > first_high);
}

#[test]
#[should_panic(expected = "herdr-core's default socket")]
fn test_helpers_refuse_the_default_herdr_core_socket() {
    let scratch = ShortTempDir::new();
    let default = scratch.path().join("default.sock");
    // The scratch path stands in for the default: the helper compares against $HC_SOCKET.
    std::env::set_var("HC_SOCKET", &default);
    refuse_default_herdr_core(&default, &scratch);
}

#[test]
#[should_panic(expected = "outside the scratch dir")]
fn test_helpers_refuse_paths_outside_the_scratch_dir() {
    let scratch = ShortTempDir::new();
    let mut paths = DaemonPaths::in_scratch(&scratch, "d");
    paths.data_dir = Path::new("/tmp/cs4-elsewhere/data").to_path_buf();
    write_daemon_config(&scratch, &paths, "fake");
}

#[test]
#[should_panic(expected = "`..` component")]
fn test_helpers_refuse_parent_components_inside_the_scratch_dir() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("../x");
    daemon_support::check_in_scratch(&path, &scratch);
}
