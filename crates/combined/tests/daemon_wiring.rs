#![cfg(feature = "test-support")]
//! The daemon's wiring pieces: the clock, the ID reservation and the test-safety helpers.

mod common;
mod daemon_support;

use std::fs;
use std::path::{Path, PathBuf};

use combine_protocol::{Attach, ClientMessage, HostId, RequestId, TermId};
use combined::adapters::fs::config::{BackendName, DaemonConfigFile};
use combined::adapters::fs::ids_file;
use combined::core::{BackendCommand, Core, CoreConfig, Effect};
use combined::model::Millis;
use combined::ports::Capabilities;
use combined::testing::{FakeBackend, FakeConfig};
use combined::wiring;
use combined::wiring::backend::{self, BackendChoice};
use combined::wiring::clock::MonotonicClock;
use combined::wiring::config::{self, DaemonConfigError, DaemonEnv, HerdrCoreSource};
use combined::wiring::ids::{IdReservation, ID_BLOCK};
use common::request_options;
use daemon_support::{
    default_herdr_core_sockets, refuse_default_herdr_core_in, refuse_non_lane_herdr_core_socket,
    write_daemon_config, DaemonPaths, ShortTempDir,
};

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
            instance: None,
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

/// `server_parts` hands the reservation's base to core: a second incarnation's first
/// attachment ID is the first ID above the first incarnation's block.
#[test]
fn server_parts_issues_the_first_attachment_id_at_the_reservation_base() {
    let scratch = ShortTempDir::new();
    let path = scratch.path().join("ids.json");
    drop(IdReservation::start(&path).unwrap());
    let (ids, id_base) = IdReservation::start(&path).unwrap();
    assert_eq!(id_base, ID_BLOCK + 1);
    assert_eq!(id_base, 1_048_577);
    let (fake, _control) = FakeBackend::new(FakeConfig::default());
    let mut parts = wiring::server_parts(fake, id_base, HostId("host-a".into()), ids).unwrap();
    let core = parts.driver.core_mut();
    let connection = core.connect();
    core.client_message(
        connection,
        ClientMessage::Attach(Attach {
            request: RequestId(1),
            terminal: TermId("fake:1".to_string()),
            options: request_options(80, 24, false),
            instance: None,
        }),
        Millis(0),
    );
    let attachments: Vec<u64> = core
        .take_effects()
        .into_iter()
        .filter_map(|effect| match effect {
            Effect::Backend(BackendCommand::Attach { attachment, .. }) => Some(attachment.0),
            _ => None,
        })
        .collect();
    assert_eq!(attachments, [id_base]);
}

#[test]
#[should_panic(expected = "herdr-core's default socket")]
fn test_helpers_refuse_the_default_herdr_core_socket() {
    let scratch = ShortTempDir::new();
    // The scratch path stands in for the default, through a synthetic environment.
    let default = scratch.path().join("default.sock");
    let env = DaemonEnv {
        hc_socket: Some(default.clone()),
        ..DaemonEnv::default()
    };
    refuse_default_herdr_core_in(&default, &scratch, &env);
}

#[test]
fn lane_socket_check_accepts_the_lane_shape() {
    let scratch = ShortTempDir::new();
    for socket in [
        "/tmp/chc.ab12/run/herdr-core/cab12.sock",
        "/private/tmp/chc.ab12/run/herdr-core/cab12.sock",
    ] {
        let socket = PathBuf::from(socket);
        refuse_non_lane_herdr_core_socket(&socket);
        let paths =
            DaemonPaths::in_scratch(&scratch, "d").with_lane_herdr_core_socket(socket.clone());
        assert!(paths.lane_herdr_core);
        let config = write_daemon_config(&scratch, &paths, "herdr-core");
        let written: serde_json::Value =
            serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
        assert_eq!(written["herdr_core_socket"], socket.to_str().unwrap());
        let reset = paths.with_herdr_core_socket(&scratch, scratch.path().join("x.sock"));
        assert!(!reset.lane_herdr_core);
    }
}

#[test]
#[should_panic(expected = "is not absolute")]
fn lane_socket_check_refuses_a_relative_path() {
    refuse_non_lane_herdr_core_socket(Path::new("chc.ab12/run/herdr-core/c.sock"));
}

#[test]
#[should_panic(expected = "`..` component")]
fn lane_socket_check_refuses_a_dotdot_path() {
    refuse_non_lane_herdr_core_socket(Path::new("/tmp/chc.ab12/run/../run/herdr-core/c.sock"));
}

#[test]
#[should_panic(expected = "not an isolated lane's herdr-core socket")]
fn lane_socket_check_refuses_a_root_not_named_chc() {
    refuse_non_lane_herdr_core_socket(Path::new("/tmp/x/run/herdr-core/c.sock"));
}

#[test]
#[should_panic(expected = "not an isolated lane's herdr-core socket")]
fn lane_socket_check_refuses_a_socket_outside_run_herdr_core() {
    refuse_non_lane_herdr_core_socket(Path::new("/tmp/chc.ab12/run/other/c.sock"));
}

#[test]
#[should_panic]
fn lane_socket_check_refuses_a_default_socket() {
    let default = default_herdr_core_sockets(&DaemonEnv::from_process())
        .into_iter()
        .next()
        .expect("the process environment has a default socket");
    refuse_non_lane_herdr_core_socket(&default);
}

#[test]
fn test_helpers_know_all_three_spellings_of_the_default_socket() {
    let env = DaemonEnv {
        home: Some("/h".into()),
        xdg_runtime_dir: Some("/r".into()),
        xdg_state_home: Some("/s".into()),
        ..DaemonEnv::default()
    };
    let known = daemon_support::default_herdr_core_sockets(&env);
    for spelling in [
        "/r/herdr-core/default.sock",
        "/s/herdr-core/default/default.sock",
        "/h/.local/state/herdr-core/default/default.sock",
    ] {
        assert!(
            known.contains(&Path::new(spelling).to_path_buf()),
            "{spelling}"
        );
    }
}

#[test]
#[should_panic(expected = "herdr-core's default socket")]
fn test_helpers_refuse_the_state_dir_default_even_when_xdg_runtime_dir_is_set() {
    let scratch = ShortTempDir::new();
    let home = scratch.path().join("h");
    let default = home.join(".local/state/herdr-core/default/default.sock");
    let env = DaemonEnv {
        home: Some(home),
        xdg_runtime_dir: Some(scratch.path().join("run")),
        ..DaemonEnv::default()
    };
    refuse_default_herdr_core_in(&default, &scratch, &env);
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

// ---- configuration (a synthetic environment; nothing here reads the real one) ----

fn env_with_home() -> DaemonEnv {
    DaemonEnv {
        home: Some(PathBuf::from("/Users/me")),
        ..DaemonEnv::default()
    }
}

fn config_file() -> DaemonConfigFile {
    DaemonConfigFile {
        version: 1,
        backend: None,
        herdr_core_socket: None,
        data_dir: None,
        socket: None,
    }
}

fn herdr_core(env: &DaemonEnv) -> (PathBuf, HerdrCoreSource) {
    config::herdr_core_socket(None, env).unwrap()
}

#[test]
fn herdr_core_socket_defaults_to_herdr_cores_default_path() {
    let (path, source) = herdr_core(&env_with_home());
    assert_eq!(
        path,
        Path::new("/Users/me/.local/state/herdr-core/default/default.sock")
    );
    assert_eq!(
        source,
        HerdrCoreSource::DefaultStateDir {
            session: "default".to_string()
        }
    );
}

#[test]
fn hc_socket_overrides_the_default() {
    let env = DaemonEnv {
        hc_socket: Some(PathBuf::from("/tmp/hc.sock")),
        hc_session: Some("other".to_string()),
        xdg_runtime_dir: Some(PathBuf::from("/run/user")),
        ..env_with_home()
    };
    let (path, source) = herdr_core(&env);
    assert_eq!(path, Path::new("/tmp/hc.sock"));
    assert_eq!(source, HerdrCoreSource::HcSocket);
}

#[test]
fn hc_session_names_the_socket_under_xdg_state_home() {
    let env = DaemonEnv {
        hc_session: Some("work".to_string()),
        xdg_state_home: Some(PathBuf::from("/Users/me/state")),
        ..env_with_home()
    };
    let (path, _) = herdr_core(&env);
    assert_eq!(path, Path::new("/Users/me/state/herdr-core/work/work.sock"));
    let (path, _) = herdr_core(&DaemonEnv {
        hc_session: Some("work".to_string()),
        ..env_with_home()
    });
    assert_eq!(
        path,
        Path::new("/Users/me/.local/state/herdr-core/work/work.sock")
    );
}

#[test]
fn xdg_runtime_dir_holds_the_session_socket() {
    let env = DaemonEnv {
        xdg_runtime_dir: Some(PathBuf::from("/run/user/501")),
        xdg_state_home: Some(PathBuf::from("/Users/me/state")),
        ..env_with_home()
    };
    let (path, source) = herdr_core(&env);
    assert_eq!(path, Path::new("/run/user/501/herdr-core/default.sock"));
    assert_eq!(
        source,
        HerdrCoreSource::XdgRuntimeDir {
            session: "default".to_string()
        }
    );
}

#[test]
fn config_herdr_core_socket_wins_over_the_environment() {
    let env = DaemonEnv {
        hc_socket: Some(PathBuf::from("/tmp/hc.sock")),
        ..env_with_home()
    };
    let file = DaemonConfigFile {
        herdr_core_socket: Some(PathBuf::from("/tmp/configured.sock")),
        ..config_file()
    };
    let resolved = config::resolve(Some(file), &env).unwrap();
    assert_eq!(
        resolved.backend,
        BackendChoice::HerdrCore {
            socket: PathBuf::from("/tmp/configured.sock"),
            source: HerdrCoreSource::ConfigFile
        }
    );
}

#[test]
fn relative_and_overlong_paths_are_refused() {
    let relative = DaemonEnv {
        hc_socket: Some(PathBuf::from("hc.sock")),
        ..env_with_home()
    };
    assert!(matches!(
        config::herdr_core_socket(None, &relative),
        Err(DaemonConfigError::RelativePath { .. })
    ));
    let file = DaemonConfigFile {
        data_dir: Some(PathBuf::from("data")),
        ..config_file()
    };
    assert!(matches!(
        config::resolve(Some(file), &env_with_home()),
        Err(DaemonConfigError::RelativePath { .. })
    ));

    let long = PathBuf::from(format!("/tmp/{}", "x".repeat(100)));
    let file = DaemonConfigFile {
        socket: Some(long.clone()),
        backend: Some(BackendName::HerdrCore),
        ..config_file()
    };
    assert!(matches!(
        config::resolve(Some(file), &env_with_home()),
        Err(DaemonConfigError::SocketTooLong { .. })
    ));
    assert!(matches!(
        config::herdr_core_socket(Some(long), &env_with_home()),
        Err(DaemonConfigError::SocketTooLong { .. })
    ));
}

#[test]
fn missing_home_without_explicit_paths_is_an_error() {
    let none = DaemonEnv::default();
    assert!(matches!(
        config::herdr_core_socket(None, &none),
        Err(DaemonConfigError::NoHome { .. })
    ));
    assert!(matches!(
        config::resolve(None, &none),
        Err(DaemonConfigError::NoHome { .. })
    ));
    assert!(config::default_config_path(&none).is_none());

    let explicit = DaemonConfigFile {
        herdr_core_socket: Some(PathBuf::from("/tmp/hc.sock")),
        data_dir: Some(PathBuf::from("/tmp/d")),
        ..config_file()
    };
    let resolved = config::resolve(Some(explicit), &none).unwrap();
    assert_eq!(resolved.data_dir, Path::new("/tmp/d"));
}

#[test]
fn socket_defaults_to_combined_sock_in_the_data_dir() {
    let resolved = config::resolve(None, &env_with_home()).unwrap();
    assert_eq!(
        resolved.data_dir,
        Path::new("/Users/me/Library/Application Support/com.squashmerge.combined")
    );
    assert_eq!(resolved.socket, resolved.data_dir.join("combined.sock"));

    let file = DaemonConfigFile {
        data_dir: Some(PathBuf::from("/tmp/d")),
        ..config_file()
    };
    let resolved = config::resolve(Some(file), &env_with_home()).unwrap();
    assert_eq!(resolved.socket, Path::new("/tmp/d/combined.sock"));
}

#[test]
fn connect_uses_socket_flag_then_config_then_default() {
    let env = env_with_home();
    let file = DaemonConfigFile {
        socket: Some(PathBuf::from("/tmp/config.sock")),
        data_dir: Some(PathBuf::from("/tmp/d")),
        ..config_file()
    };
    let flag = Some(PathBuf::from("/tmp/flag.sock"));
    assert_eq!(
        config::connect_socket(flag, Some(&file), &env).unwrap(),
        Path::new("/tmp/flag.sock")
    );
    assert_eq!(
        config::connect_socket(None, Some(&file), &env).unwrap(),
        Path::new("/tmp/config.sock")
    );
    let data_only = DaemonConfigFile {
        socket: None,
        ..file
    };
    assert_eq!(
        config::connect_socket(None, Some(&data_only), &env).unwrap(),
        Path::new("/tmp/d/combined.sock")
    );
    assert_eq!(
        config::connect_socket(None, None, &env).unwrap(),
        Path::new("/Users/me/Library/Application Support/com.squashmerge.combined/combined.sock")
    );
}

#[test]
fn fake_backend_builds_in_test_support_builds() {
    let file = DaemonConfigFile {
        backend: Some(BackendName::Fake),
        ..config_file()
    };
    let resolved = config::resolve(Some(file), &env_with_home()).unwrap();
    assert_eq!(resolved.backend, BackendChoice::Fake);
    let backend = futures::executor::block_on(backend::build(&resolved.backend)).unwrap();
    assert_eq!(backend.id(), "fake");
}

fn herdr_core_choice(socket: PathBuf) -> BackendChoice {
    BackendChoice::HerdrCore {
        socket,
        source: HerdrCoreSource::ConfigFile,
    }
}

#[test]
fn caller_identity_follows_the_backend_choice() {
    let scratch = ShortTempDir::new();
    let socket = scratch.sock("core.sock");
    let identity = backend::caller_identity(&herdr_core_choice(socket.clone())).unwrap();
    assert_eq!(
        identity.terminal("t1", socket.to_str().unwrap()),
        Some(TermId("hc:t1".to_string()))
    );
    assert_eq!(
        identity.terminal("t1", scratch.sock("other.sock").to_str().unwrap()),
        None
    );
    assert!(backend::caller_identity(&BackendChoice::Fake).is_none());
}

fn build_error(choice: &BackendChoice) -> combined::wiring::StartError {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    match runtime.block_on(backend::build(choice)) {
        Ok(_) => panic!("the backend should not build"),
        Err(error) => error,
    }
}

#[test]
fn herdr_core_backend_with_missing_socket_is_unavailable() {
    let scratch = ShortTempDir::new();
    let missing = scratch.path().join("no-hc.sock");
    let error = build_error(&herdr_core_choice(missing.clone()));
    assert_eq!(error.exit_code(), 3);
    let text = error.to_string();
    assert!(text.contains(missing.to_str().unwrap()), "{text}");
    assert!(text.contains("daemon.json"), "{text}");
    assert!(text.contains("unavailable"), "{text}");
}

#[test]
fn herdr_core_backend_that_closes_before_hello_fails_to_start() {
    let scratch = ShortTempDir::new();
    let socket = scratch.path().join("hc.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    // A scoped thread: a failed assertion below still joins the stand-in (within 10 s) rather
    // than leaving it detached.
    std::thread::scope(|scope| {
        let accepter = scope.spawn(move || {
            for _ in 0..1000 {
                if let Ok(stream) = listener.accept() {
                    drop(stream);
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        });
        assert!(backend::HERDR_CORE.is_some());
        let error = build_error(&herdr_core_choice(socket.clone()));
        assert_eq!(error.exit_code(), 3);
        let text = error.to_string();
        let prefix = format!(
            "cannot start the herdr-core backend on {}: ",
            socket.display()
        );
        assert!(text.starts_with(&prefix), "{text}");
        assert!(text.contains("hello"), "{text}");
        assert!(!text.contains("no herdr-core adapter"), "{text}");
        assert!(accepter.join().unwrap(), "nothing connected to the socket");
    });
}
