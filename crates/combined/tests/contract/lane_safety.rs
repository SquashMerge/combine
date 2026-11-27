//! The rules that keep the herdr-core lane away from every core but its own.
//!
//! The lane (`sh tools/verify-herdr-core`) starts one core under a scratch root, pins its id and
//! runs the contract suite against it. Everything here is a pure function of strings and
//! numbers, so `./tools/verify` tests it without a core and without ever running `hc`. The shell
//! copies of the checks live in `tools/herdr-core-lane.sh`; the tests below run both on the same
//! table.
//!
//! Signals: the only two places anything is ever signalled are the crash procedure's SIGKILL of
//! the lane's own core (validated by [`check_kill_target`]) and the script's teardown of the
//! lane's own test binary. The lane never signals a group and never signals a pid it hasn't just
//! validated.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// What a path is, from `lstat`: a symlink is never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathKind {
    Directory,
    Socket,
    Symlink,
    File,
    Missing,
    Other,
}

pub fn kind_of(path: &Path) -> PathKind {
    use std::os::unix::fs::FileTypeExt;
    match std::fs::symlink_metadata(path) {
        Err(_) => PathKind::Missing,
        Ok(meta) => {
            let kind = meta.file_type();
            if kind.is_symlink() {
                PathKind::Symlink
            } else if kind.is_dir() {
                PathKind::Directory
            } else if kind.is_socket() {
                PathKind::Socket
            } else if kind.is_file() {
                PathKind::File
            } else {
                PathKind::Other
            }
        }
    }
}

/// The longest socket path macOS accepts is 103 bytes.
pub const SOCKET_PATH_MAX: usize = 103;

/// The longest the lane's test binary may run, in seconds.
pub const DEADLINE_MAX_SECONDS: u64 = 1_320;

/// Names the lane core's process may carry: herdr-core, or the `hc` link launchd starts it by.
const CORE_COMMANDS: [&str; 2] = ["herdr-core", "hc"];

/// The suffix of `/tmp/chc.XXXXXX` (or its `/private` form), six letters and digits.
pub fn check_root(root: &str) -> Result<String, String> {
    let name = root
        .strip_prefix("/private/tmp/chc.")
        .or_else(|| root.strip_prefix("/tmp/chc."))
        .ok_or_else(|| format!("{root} is not a lane scratch root (/tmp/chc.XXXXXX)"))?;
    if name.len() == 6 && name.chars().all(|c| c.is_ascii_alphanumeric()) {
        Ok(name.to_string())
    } else {
        Err(format!(
            "{root} is not a lane scratch root (/tmp/chc.XXXXXX)"
        ))
    }
}

/// The root is a real directory (not a symlink) that only its owner can enter.
pub fn check_root_kind(kind: PathKind, mode: u32) -> Result<(), String> {
    if kind != PathKind::Directory {
        return Err(format!("the lane root is {kind:?}, not a directory"));
    }
    if mode & 0o777 != 0o700 {
        return Err(format!(
            "the lane root has mode {:o}, not 700",
            mode & 0o777
        ));
    }
    Ok(())
}

/// The session is `c` plus the root's suffix: one per lane, never the default's.
pub fn check_session(session: &str, root: &str) -> Result<(), String> {
    let suffix = check_root(root)?;
    if session == format!("c{suffix}") {
        Ok(())
    } else {
        Err(format!(
            "session {session} is not this lane's own (c{suffix})"
        ))
    }
}

pub fn expected_socket(root: &str, session: &str) -> String {
    format!("{root}/run/herdr-core/{session}.sock")
}

/// The socket is exactly `<root>/run/herdr-core/<session>.sock`, short enough for macOS, and
/// nowhere near herdr-core's own state directory.
pub fn check_socket_path(
    socket: &str,
    root: &str,
    session: &str,
    home: &str,
) -> Result<(), String> {
    check_session(session, root)?;
    let state = format!("{home}/.local/state/herdr-core");
    let default = format!("{state}/default/default.sock");
    if socket == default || socket == state || socket.starts_with(&format!("{state}/")) {
        return Err(format!(
            "{socket} is under herdr-core's own state directory"
        ));
    }
    if socket != expected_socket(root, session) {
        return Err(format!(
            "{socket} is not the lane socket {}",
            expected_socket(root, session)
        ));
    }
    // Unreachable today: the equality check above admits only
    // `<root>/run/herdr-core/<session>.sock`, and `check_root` and `check_session` hold that to
    // 51 bytes at most (`longest_valid_lane_socket_fits_the_103_byte_limit` pins it). Kept as
    // defence in depth, so a later change that loosens the root or session rules still can't
    // name a socket path macOS would refuse.
    if socket.len() > SOCKET_PATH_MAX {
        return Err(format!(
            "{socket} is {} bytes; a socket path is at most {SOCKET_PATH_MAX}",
            socket.len()
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Socket,
    Directory,
}

/// A socket must be a socket and a root a directory, neither a symlink to one.
pub fn check_kind(kind: PathKind, expect: Expect, what: &str) -> Result<(), String> {
    let wanted = match expect {
        Expect::Socket => PathKind::Socket,
        Expect::Directory => PathKind::Directory,
    };
    if kind == wanted {
        Ok(())
    } else {
        Err(format!("{what} is {kind:?}, expected {wanted:?}"))
    }
}

/// The core answering is the one the lane started: both ids present and equal.
pub fn check_core_id(reported: &str, pinned: &str) -> Result<(), String> {
    if reported.is_empty() || pinned.is_empty() {
        return Err("a core id is empty".to_string());
    }
    if reported == pinned {
        Ok(())
    } else {
        Err(format!("core id {reported} is not the pinned id {pinned}"))
    }
}

/// What one `ps -o ppid=,pgid=,comm=` observation of the target says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observed {
    pub ppid: u32,
    pub pgid: u32,
    pub comm: String,
}

/// Reads the single line `ps -o ppid=,pgid=,comm= -p <pid>` prints.
pub fn parse_ps(output: &str) -> Option<Observed> {
    let mut lines = output.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let line = line.trim_start();
    let (ppid, rest) = line.split_once(char::is_whitespace)?;
    let (pgid, comm) = rest.trim_start().split_once(char::is_whitespace)?;
    Some(Observed {
        ppid: ppid.parse().ok()?,
        pgid: pgid.parse().ok()?,
        comm: comm.trim().to_string(),
    })
}

/// Whether the crash procedure may SIGKILL `pid`. `pid` is exactly what the lane core's
/// `server.status` reply said (after `check_core_id` passed on that reply), `observed` is one
/// `ps` look at that pid, and `own`, `parent` and `group` are this test process's own pid, parent
/// and process group. Returns the pid to signal.
pub fn check_kill_target(
    pid: &Value,
    holders: &[u32],
    own: u32,
    parent: u32,
    group: u32,
    observed: &Observed,
) -> Result<u32, String> {
    let pid = match pid {
        Value::Number(number) if number.is_u64() => number.as_u64().unwrap_or(0),
        other => return Err(format!("the pid {other} is not a plain integer")),
    };
    let pid = u32::try_from(pid)
        .ok()
        .filter(|pid| *pid > 1 && i32::try_from(*pid).is_ok())
        .ok_or_else(|| format!("the pid {pid} is not above 1 and within i32"))?;
    if [own, parent, group].contains(&pid) {
        return Err(format!("the pid {pid} is ours, our parent or our group"));
    }
    if holders.contains(&pid) {
        return Err(format!("the pid {pid} is a holder"));
    }
    let name = observed.comm.rsplit('/').next().unwrap_or("");
    if !CORE_COMMANDS.contains(&name) {
        return Err(format!(
            "the pid {pid} runs {}, not herdr-core",
            observed.comm
        ));
    }
    if observed.pgid == group {
        return Err(format!("the pid {pid} shares our process group {group}"));
    }
    if observed.ppid == own {
        return Err(format!("the pid {pid} is our child"));
    }
    Ok(pid)
}

/// No `HC_*` variable may reach the lane's test binary: they point `hc` at some core.
pub fn check_no_hc_vars<I: IntoIterator<Item = (String, String)>>(vars: I) -> Result<(), String> {
    for (name, _) in vars {
        if name.starts_with("HC_") {
            return Err(format!(
                "{name} is set; the lane binary runs without HC_* variables"
            ));
        }
    }
    Ok(())
}

/// The lane deadline: 1,320 s unless a shorter valid number is given.
pub fn deadline_seconds(raw: Option<&str>) -> u64 {
    raw.and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(DEADLINE_MAX_SECONDS, |seconds| {
            seconds.min(DEADLINE_MAX_SECONDS)
        })
}

/// The lane, as the script describes it to the test binary.
#[derive(Debug, Clone)]
pub struct Lane {
    pub root: String,
    pub session: String,
    pub socket: PathBuf,
    pub core_id: String,
    pub hc_bin: PathBuf,
    pub python: PathBuf,
}

/// What the caller's environment supplies to every `hc` call.
#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub home: String,
    pub user: String,
    pub logname: String,
    pub tmpdir: String,
}

impl Caller {
    pub fn from_process() -> Caller {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        Caller {
            home: var("HOME"),
            user: var("USER"),
            logname: var("LOGNAME"),
            tmpdir: std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string()),
        }
    }
}

impl Lane {
    /// Reads and checks the lane from the variables the script sets. Every failure is a
    /// refusal.
    pub fn from_vars(
        vars: &[(String, String)],
        get: impl Fn(&str) -> Option<String>,
        home: &str,
    ) -> Result<Lane, String> {
        check_no_hc_vars(vars.iter().cloned())?;
        let need = |name: &str| {
            get(name)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| format!("{name} is not set"))
        };
        let root = need("COMBINE_HC_ROOT")?;
        let session = need("COMBINE_HC_SESSION")?;
        let socket = need("COMBINE_HC_SOCKET")?;
        let core_id = need("COMBINE_HC_CORE_ID")?;
        check_root(&root)?;
        check_session(&session, &root)?;
        check_socket_path(&socket, &root, &session, home)?;
        if core_id.is_empty() {
            return Err("COMBINE_HC_CORE_ID is empty".to_string());
        }
        Ok(Lane {
            root,
            session,
            socket: PathBuf::from(socket),
            core_id,
            hc_bin: PathBuf::from(need("COMBINE_HC_BIN")?),
            python: PathBuf::from(need("COMBINE_HC_PYTHON")?),
        })
    }
}

/// The one environment every `hc` the lane runs gets: nothing but these.
pub fn hc_env(lane: &Lane, caller: &Caller, with_socket: bool) -> Vec<(String, String)> {
    let path = |sub: &str| format!("{}/{sub}", lane.root);
    let mut env = vec![
        (
            "PATH".to_string(),
            "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
        ),
        ("HOME".to_string(), caller.home.clone()),
        ("USER".to_string(), caller.user.clone()),
        ("LOGNAME".to_string(), caller.logname.clone()),
        ("TMPDIR".to_string(), caller.tmpdir.clone()),
        ("HC_SESSION".to_string(), lane.session.clone()),
        ("XDG_STATE_HOME".to_string(), path("state")),
        ("XDG_CONFIG_HOME".to_string(), path("config")),
        ("XDG_RUNTIME_DIR".to_string(), path("run")),
    ];
    if with_socket {
        env.push(("HC_SOCKET".to_string(), lane.socket.display().to_string()));
    }
    env
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use combined::testing::fresh_dir;
    use serde_json::json;

    use super::*;

    const HOME: &str = "/Users/someone";

    fn lib() -> String {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tools/herdr-core-lane.sh"
        )
        .to_string()
    }

    /// Runs `script` in `sh` with the lane library sourced and `args` as `$1…`, with an
    /// environment that holds only what `env` names.
    fn sh(script: &str, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!(". '{}'; {script}", lib()))
            .arg("sh")
            .args(args)
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        for (name, value) in env {
            command.env(name, value);
        }
        output_unless_killed(&mut command)
    }

    /// Runs `command` and returns its output, one child at a time across these tests.
    ///
    /// #27: when several of these tests spawned `sh` at the same instant, the kernel sometimes
    /// SIGKILLed a child before it ran `sh` at all. The kernel log names the killed process by
    /// the test binary's own name ("exiting with signal 9, force exiting the process"), and the
    /// kills came in pairs a millisecond apart. Spawning one at a time stops that.
    ///
    /// A child a signal killed never ran to an answer, so it says nothing about the lane library.
    /// It is run once more, and a second kill fails the test, naming the signal. A kill must
    /// never decide a check: a `!sh_ok(..)` assertion would otherwise pass on a killed child.
    fn output_unless_killed(command: &mut Command) -> std::process::Output {
        use std::os::unix::process::ExitStatusExt;
        static SPAWN: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let run = |command: &mut Command| {
            let _one_at_a_time = SPAWN.lock().unwrap_or_else(|e| e.into_inner());
            command.output().expect("sh runs")
        };
        let output = run(command);
        if output.status.code().is_some() {
            return output;
        }
        let first = output.status.signal();
        let output = run(command);
        assert!(
            output.status.code().is_some(),
            "sh was killed twice, by signal {first:?} then {:?}; stderr {:?}",
            output.status.signal(),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn sh_ok(script: &str, args: &[&str]) -> bool {
        sh(script, args, &[]).status.success()
    }

    fn lane() -> Lane {
        Lane {
            root: "/tmp/chc.Ab12Cd".to_string(),
            session: "cAb12Cd".to_string(),
            socket: PathBuf::from("/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock"),
            core_id: "abc".to_string(),
            hc_bin: PathBuf::from("/usr/bin/true"),
            python: PathBuf::from("/usr/bin/python3"),
        }
    }

    fn core(ppid: u32, pgid: u32, comm: &str) -> Observed {
        Observed {
            ppid,
            pgid,
            comm: comm.to_string(),
        }
    }

    #[test]
    fn root_pattern_accepts_only_this_lanes_scratch_roots() {
        for (root, ok) in [
            ("/tmp/chc.Ab12Cd", true),
            ("/private/tmp/chc.Ab12Cd", true),
            ("/tmp/chc.Ab12C", false),
            ("/tmp/chc.Ab12CdE", false),
            ("/tmp/chc.Ab12C!", false),
            ("/tmp/chc.Ab12Cd/", false),
            ("/tmp/chc.Ab12Cd/x", false),
            ("/tmp/chx.Ab12Cd", false),
            ("/var/tmp/chc.Ab12Cd", false),
            ("/home/me/chc.Ab12Cd", false),
            ("tmp/chc.Ab12Cd", false),
            ("", false),
        ] {
            assert_eq!(check_root(root).is_ok(), ok, "{root}");
        }
        assert_eq!(check_root("/tmp/chc.Ab12Cd"), Ok("Ab12Cd".to_string()));
        assert!(check_root_kind(PathKind::Directory, 0o40700).is_ok());
        assert!(check_root_kind(PathKind::Directory, 0o755).is_err());
        assert!(check_root_kind(PathKind::Symlink, 0o700).is_err());
    }

    #[test]
    fn session_must_be_the_lanes_own() {
        let root = "/tmp/chc.Ab12Cd";
        assert!(check_session("cAb12Cd", root).is_ok());
        for session in ["default", "", "Ab12Cd", "cAb12CD", "cXXXXXX", "m0"] {
            assert!(check_session(session, root).is_err(), "{session}");
        }
        assert!(check_session("cAb12Cd", "/tmp/other").is_err());
    }

    #[test]
    fn socket_must_be_the_expected_path_inside_the_root() {
        let root = "/tmp/chc.Ab12Cd";
        let session = "cAb12Cd";
        let good = expected_socket(root, session);
        assert_eq!(good, "/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock");
        assert!(check_socket_path(&good, root, session, HOME).is_ok());
        for other in [
            "/tmp/chc.Ab12Cd/run/herdr-core/other.sock",
            "/tmp/chc.Ab12Cd/run/cAb12Cd.sock",
            "/tmp/chc.Ab12Cd/run/herdr-core/../herdr-core/cAb12Cd.sock",
            "/tmp/chc.Zz99Zz/run/herdr-core/cAb12Cd.sock",
            "/tmp/cAb12Cd.sock",
            "",
        ] {
            assert!(
                check_socket_path(other, root, session, HOME).is_err(),
                "{other}"
            );
        }
    }

    #[test]
    fn default_socket_is_refused() {
        let default = format!("{HOME}/.local/state/herdr-core/default/default.sock");
        assert!(check_socket_path(&default, "/tmp/chc.Ab12Cd", "cAb12Cd", HOME).is_err());
        assert!(check_socket_path(&default, "/tmp/chc.Ab12Cd", "default", HOME).is_err());
    }

    #[test]
    fn herdr_core_state_paths_are_refused() {
        let state = format!("{HOME}/.local/state/herdr-core");
        for path in [
            state.clone(),
            format!("{state}/c/x.sock"),
            format!("{state}/"),
        ] {
            assert!(check_socket_path(&path, "/tmp/chc.Ab12Cd", "cAb12Cd", HOME).is_err());
        }
        // A lane whose root somehow sat under the state directory still can't pass.
        assert!(check_root(&format!("{state}/chc.Ab12Cd")).is_err());
    }

    #[test]
    fn socket_over_103_bytes_is_refused() {
        let long_root = format!("/tmp/chc.Ab12Cd/{}", "x".repeat(100));
        assert!(check_socket_path(
            &format!("{long_root}/s.sock"),
            "/tmp/chc.Ab12Cd",
            "cAb12Cd",
            HOME
        )
        .is_err());
    }

    /// The 103-byte length branch of `check_socket_path` cannot be reached by any input that
    /// passes the checks before it: a root is `/tmp/chc.XXXXXX` or `/private/tmp/chc.XXXXXX`,
    /// the session is `c` + the six-character suffix, and anything but the exact expected path
    /// is refused first. So the branch is defence in depth, and the property it guards is
    /// pinned here instead: the longest socket a valid lane can name fits, with room to spare.
    #[test]
    fn longest_valid_lane_socket_fits_the_103_byte_limit() {
        let root = "/private/tmp/chc.Zz99Zz";
        let socket = expected_socket(root, "cZz99Zz");
        assert_eq!(socket.len(), 51, "{socket}");
        assert!(socket.len() <= SOCKET_PATH_MAX, "{}", socket.len());
        assert!(check_socket_path(&socket, root, "cZz99Zz", HOME).is_ok());
        // Anything longer than the limit is refused by the equality check, whichever branch
        // comes first: no over-long path is ever accepted.
        let long = format!("{root}/run/herdr-core/{}.sock", "c".repeat(SOCKET_PATH_MAX));
        assert!(long.len() > SOCKET_PATH_MAX);
        let error = check_socket_path(&long, root, "cZz99Zz", HOME).expect_err("too long");
        assert!(error.contains("not the lane socket"), "{error}");
    }

    /// The state-path branch on its own. The state check runs before the equality check, so an
    /// error naming the state directory proves this branch refused the path, not the equality
    /// check that would also have refused it. The default socket is one of those paths.
    #[test]
    fn state_path_branch_alone_refuses_paths_under_the_state_directory() {
        let state = format!("{HOME}/.local/state/herdr-core");
        for path in [
            format!("{state}/default/default.sock"),
            format!("{state}/cAb12Cd/x.sock"),
            state.clone(),
        ] {
            let error = check_socket_path(&path, "/tmp/chc.Ab12Cd", "cAb12Cd", HOME)
                .expect_err("state path");
            assert!(error.contains("state directory"), "{path}: {error}");
        }
        // A path outside it is refused by the equality check instead, so the message tells
        // the two branches apart.
        let error = check_socket_path("/tmp/x.sock", "/tmp/chc.Ab12Cd", "cAb12Cd", HOME)
            .expect_err("other path");
        assert!(error.contains("not the lane socket"), "{error}");
    }

    #[test]
    fn symlinks_and_non_sockets_are_refused() {
        assert!(check_kind(PathKind::Socket, Expect::Socket, "socket").is_ok());
        for kind in [
            PathKind::Symlink,
            PathKind::File,
            PathKind::Directory,
            PathKind::Missing,
        ] {
            assert!(
                check_kind(kind, Expect::Socket, "socket").is_err(),
                "{kind:?}"
            );
        }
        assert!(check_kind(PathKind::Directory, Expect::Directory, "root").is_ok());
        assert!(check_kind(PathKind::Symlink, Expect::Directory, "root").is_err());
        let dir = fresh_dir(&std::env::temp_dir(), "lane-kinds");
        std::fs::write(dir.join("file"), b"x").expect("file");
        std::os::unix::fs::symlink(&dir, dir.join("link")).expect("link");
        assert_eq!(kind_of(&dir.join("file")), PathKind::File);
        assert_eq!(kind_of(&dir.join("link")), PathKind::Symlink);
        assert_eq!(kind_of(&dir), PathKind::Directory);
        assert_eq!(kind_of(&dir.join("none")), PathKind::Missing);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn core_id_must_equal_the_pinned_id() {
        assert!(check_core_id("abc", "abc").is_ok());
        for (reported, pinned) in [("abc", "abd"), ("", "abc"), ("abc", ""), ("", "")] {
            assert!(
                check_core_id(reported, pinned).is_err(),
                "{reported:?} {pinned:?}"
            );
        }
    }

    #[test]
    fn kill_pid_must_be_a_plain_integer_above_1() {
        let ok = core(1, 900, "/Users/x/.local/bin/hc");
        let check = |pid: Value| check_kill_target(&pid, &[], 10, 11, 12, &ok);
        assert_eq!(check(json!(4242)), Ok(4242));
        assert_eq!(check(json!(2)), Ok(2));
        for bad in [
            json!(0),
            json!(1),
            json!(-5),
            json!("4242"),
            json!(4242.0),
            json!(2.5),
            json!(null),
            json!(true),
            json!([4242]),
            json!(2_147_483_648u64),
            json!(u64::MAX),
        ] {
            assert!(check(bad.clone()).is_err(), "{bad}");
        }
    }

    #[test]
    fn kill_pid_must_not_be_ours_our_parent_our_group_or_a_holder() {
        let ok = core(1, 900, "herdr-core");
        let check =
            |pid: u32, holders: &[u32]| check_kill_target(&json!(pid), holders, 10, 11, 12, &ok);
        assert!(check(4242, &[77]).is_ok());
        for pid in [10, 11, 12] {
            assert!(check(pid, &[]).is_err(), "{pid}");
        }
        assert!(check(77, &[77]).is_err());
        assert!(check(4242, &[4242, 5]).is_err());
    }

    #[test]
    fn kill_target_must_be_herdr_core_outside_our_process_tree() {
        let check =
            |observed: Observed| check_kill_target(&json!(4242), &[], 10, 11, 12, &observed);
        assert!(check(core(
            1,
            4242,
            "/Users/x/.local/share/herdr-core/current/Contents/MacOS/herdr-core"
        ))
        .is_ok());
        assert!(check(core(1, 4242, "/Users/x/.local/bin/hc")).is_ok());
        for comm in [
            "bash",
            "/bin/zsh",
            "claude",
            "python3",
            "herdr-core-helper",
            "",
            "/bin/hc2",
        ] {
            assert!(check(core(1, 4242, comm)).is_err(), "{comm}");
        }
        assert!(
            check(core(1, 12, "hc")).is_err(),
            "shares our process group"
        );
        assert!(check(core(10, 4242, "hc")).is_err(), "our child");
        assert_eq!(
            parse_ps(" 1 4242 /Users/x/.local/bin/hc\n"),
            Some(core(1, 4242, "/Users/x/.local/bin/hc"))
        );
        assert_eq!(parse_ps(""), None);
        assert_eq!(parse_ps("1 2 a\n3 4 b\n"), None);
        assert_eq!(parse_ps("x 2 a\n"), None);
    }

    #[test]
    fn hc_vars_in_the_lane_binary_are_refused() {
        let vars = |names: &[&str]| -> Vec<(String, String)> {
            names
                .iter()
                .map(|n| (n.to_string(), "1".to_string()))
                .collect()
        };
        assert!(check_no_hc_vars(vars(&["PATH", "COMBINE_HC_LANE"])).is_ok());
        for name in ["HC_SESSION", "HC_SOCKET", "HC_TERMINAL_ID", "HC_"] {
            assert!(check_no_hc_vars(vars(&["PATH", name])).is_err(), "{name}");
        }
        let lane = lane();
        let set: Vec<(String, String)> = vec![
            ("COMBINE_HC_ROOT".into(), lane.root.clone()),
            ("COMBINE_HC_SESSION".into(), lane.session.clone()),
            (
                "COMBINE_HC_SOCKET".into(),
                lane.socket.display().to_string(),
            ),
            ("COMBINE_HC_CORE_ID".into(), "abc".into()),
            ("COMBINE_HC_BIN".into(), "/usr/bin/true".into()),
            ("COMBINE_HC_PYTHON".into(), "/usr/bin/python3".into()),
        ];
        let get = |name: &str| set.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
        assert!(Lane::from_vars(&set, get, HOME).is_ok());
        let mut with_hc = set.clone();
        with_hc.push(("HC_SOCKET".into(), "/x".into()));
        assert!(Lane::from_vars(&with_hc, get, HOME).is_err());
        let missing = |name: &str| {
            if name == "COMBINE_HC_CORE_ID" {
                None
            } else {
                get(name)
            }
        };
        assert!(Lane::from_vars(&set, missing, HOME).is_err());
    }

    #[test]
    fn deadline_defaults_to_1320_and_never_exceeds_it() {
        assert_eq!(deadline_seconds(None), 1_320);
        assert_eq!(deadline_seconds(Some("")), 1_320);
        assert_eq!(deadline_seconds(Some("abc")), 1_320);
        assert_eq!(deadline_seconds(Some("0")), 1_320);
        assert_eq!(deadline_seconds(Some("-4")), 1_320);
        assert_eq!(deadline_seconds(Some("99999")), 1_320);
        assert_eq!(deadline_seconds(Some("1320")), 1_320);
        assert_eq!(deadline_seconds(Some("60")), 60);
        assert_eq!(deadline_seconds(Some(" 5 ")), 5);
    }

    #[test]
    fn hc_env_is_the_allowlist_only() {
        let caller = Caller {
            home: HOME.to_string(),
            user: "me".to_string(),
            logname: "me".to_string(),
            tmpdir: "/tmp".to_string(),
        };
        let without = hc_env(&lane(), &caller, false);
        let names: Vec<&str> = without.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "HOME",
                "USER",
                "LOGNAME",
                "TMPDIR",
                "HC_SESSION",
                "XDG_STATE_HOME",
                "XDG_CONFIG_HOME",
                "XDG_RUNTIME_DIR"
            ]
        );
        let with = hc_env(&lane(), &caller, true);
        assert_eq!(with.len(), without.len() + 1);
        assert_eq!(
            with.last(),
            Some(&(
                "HC_SOCKET".to_string(),
                "/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock".to_string()
            ))
        );
    }

    #[test]
    fn shell_checks_agree_with_the_rust_checks() {
        let home = HOME;
        for root in [
            "/tmp/chc.Ab12Cd",
            "/private/tmp/chc.Ab12Cd",
            "/tmp/chc.Ab12C",
            "/tmp/chc.Ab12C!",
            "/tmp/chc.Ab12Cd/",
            "/var/tmp/chc.Ab12Cd",
            "",
        ] {
            assert_eq!(
                sh_ok("lane_check_root \"$1\"", &[root]),
                check_root(root).is_ok(),
                "root {root:?}"
            );
        }
        let root = "/tmp/chc.Ab12Cd";
        for session in ["cAb12Cd", "default", "", "cXXXXXX", "Ab12Cd"] {
            assert_eq!(
                sh_ok("lane_check_session \"$1\" \"$2\"", &[session, root]),
                check_session(session, root).is_ok(),
                "session {session:?}"
            );
        }
        let default = format!("{home}/.local/state/herdr-core/default/default.sock");
        let state_path = format!("{home}/.local/state/herdr-core/c/x.sock");
        let long = "a".repeat(120);
        let good = expected_socket(root, "cAb12Cd");
        for socket in [
            good.as_str(),
            default.as_str(),
            state_path.as_str(),
            long.as_str(),
            "/tmp/chc.Ab12Cd/run/herdr-core/other.sock",
            "",
        ] {
            assert_eq!(
                sh_ok(
                    "lane_check_socket_path \"$1\" \"$2\" \"$3\" \"$4\"",
                    &[socket, root, "cAb12Cd", home]
                ),
                check_socket_path(socket, root, "cAb12Cd", home).is_ok(),
                "socket {socket:?}"
            );
        }
        let printed = sh(
            "lane_expected_socket \"$1\" \"$2\"",
            &[root, "cAb12Cd"],
            &[],
        );
        assert_eq!(String::from_utf8_lossy(&printed.stdout), good);
        for (reported, pinned) in [
            ("abc", "abc"),
            ("abc", "abd"),
            ("", "abc"),
            ("abc", ""),
            ("", ""),
        ] {
            assert_eq!(
                sh_ok("lane_check_core_id \"$1\" \"$2\"", &[reported, pinned]),
                check_core_id(reported, pinned).is_ok(),
                "ids {reported:?} {pinned:?}"
            );
        }
    }

    #[test]
    fn shell_own_child_check_refuses_strangers() {
        let check = |pid: &str, exe: &str, ppid: &str, comm: &str, me: &str| {
            sh_ok(
                "lane_check_own_child \"$1\" \"$2\" \"$3\" \"$4\" \"$5\"",
                &[pid, exe, ppid, comm, me],
            )
        };
        let exe = "/work/target/debug/deps/contract_herdr_core-1a2b";
        assert!(check("4242", exe, "900", exe, "900"));
        assert!(check("4242", exe, "900", "contract_herdr_core-1a2b", "900"));
        assert!(!check("4242", exe, "901", exe, "900"), "wrong parent");
        assert!(
            !check("4242", exe, "900", "/bin/zsh", "900"),
            "wrong command"
        );
        assert!(!check(
            "4242",
            exe,
            "900",
            "/bin/contract_herdr_core-1a2c",
            "900"
        ));
        assert!(!check("", exe, "900", exe, "900"), "empty pid");
        for pid in ["1", "0", "-3", "12a", "4 2", "0x10", "+5"] {
            assert!(!check(pid, exe, "900", exe, "900"), "pid {pid:?}");
        }
        assert!(!check("4242", "", "900", "", "900"), "no executable");
        assert!(!check("4242", exe, "", exe, "900"), "no parent");
    }

    /// #27: the lane refuses a core with the hard-coded default id or with the live one the
    /// default environment reported at start, and refuses outright when either id is missing.
    #[test]
    fn shell_refuses_the_default_core_by_either_id() {
        let hard = sh("lane_default_core_id", &[], &[]);
        let hard = String::from_utf8_lossy(&hard.stdout).into_owned();
        assert_eq!(hard.len(), 32, "{hard:?}");
        let check =
            |id: &str, live: &str| sh_ok("lane_check_not_default \"$1\" \"$2\"", &[id, live]);
        assert!(check("abc", "def"));
        assert!(!check(&hard, "def"), "the hard-coded id");
        assert!(!check("def", "def"), "the live id");
        assert!(!check(&hard, &hard));
        assert!(!check("", "def"), "no lane id");
        assert!(!check("abc", ""), "no live id");
        assert!(!check("", ""));
    }

    /// The default-environment read gets no HC_* variable and none of the lane's paths: only
    /// what `lane_hc` passes from the caller, plus the caller's own XDG directories when set.
    #[test]
    fn shell_default_environment_is_the_callers_without_hc_or_lane_vars() {
        let base = [
            ("HOME", HOME),
            ("USER", "me"),
            ("LOGNAME", "me2"),
            ("TMPDIR", "/var/folders/x/T"),
            ("LANE_HC", "/usr/bin/env"),
            ("LANE_ROOT", "/tmp/chc.Ab12Cd"),
            ("LANE_SESSION", "cAb12Cd"),
            ("LANE_SOCKET", "/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock"),
            ("HC_SESSION", "cAb12Cd"),
            ("HC_SOCKET", "/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock"),
            ("SECRET_FROM_CALLER", "must not leak"),
        ];
        let lines = |extra: &[(&str, &str)]| {
            let environment: Vec<(&str, &str)> = base.iter().chain(extra).copied().collect();
            let output = sh("lane_hc_default", &[], &environment);
            assert!(output.status.success(), "lane_hc_default failed");
            let mut got: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect();
            got.sort();
            got
        };
        let mut want = vec![
            "PATH=/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
            format!("HOME={HOME}"),
            "USER=me".to_string(),
            "LOGNAME=me2".to_string(),
            "TMPDIR=/var/folders/x/T".to_string(),
        ];
        want.sort();
        assert_eq!(lines(&[]), want);
        let xdg = [
            ("XDG_STATE_HOME", "/Users/someone/state"),
            ("XDG_CONFIG_HOME", "/Users/someone/config"),
            ("XDG_RUNTIME_DIR", "/Users/someone/run"),
        ];
        want.extend(xdg.iter().map(|(name, value)| format!("{name}={value}")));
        want.sort();
        assert_eq!(lines(&xdg), want);
    }

    #[test]
    fn shell_and_rust_hc_environments_match() {
        let caller = Caller {
            home: HOME.to_string(),
            user: "me".to_string(),
            logname: "me2".to_string(),
            tmpdir: "/var/folders/x/T".to_string(),
        };
        let lane = lane();
        let environment = [
            ("HOME", caller.home.as_str()),
            ("USER", caller.user.as_str()),
            ("LOGNAME", caller.logname.as_str()),
            ("TMPDIR", caller.tmpdir.as_str()),
            ("LANE_HC", "/usr/bin/env"),
            ("LANE_ROOT", lane.root.as_str()),
            ("LANE_SESSION", lane.session.as_str()),
            ("LANE_SOCKET", "/tmp/chc.Ab12Cd/run/herdr-core/cAb12Cd.sock"),
            ("SECRET_FROM_CALLER", "must not leak"),
            ("HC_SESSION", "default"),
        ];
        for (function, with_socket) in [("lane_hc", false), ("lane_hc_socket", true)] {
            let output = sh(function, &[], &environment);
            assert!(output.status.success(), "{function} failed");
            let mut got: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string)
                .collect();
            let mut want: Vec<String> = hc_env(&lane, &caller, with_socket)
                .into_iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            got.sort();
            want.sort();
            assert_eq!(got, want, "{function}");
        }
    }

    #[test]
    fn shell_kind_checks_refuse_symlinks_and_regular_files() {
        let dir = fresh_dir(&std::env::temp_dir(), "lane-sh-kinds");
        let file = dir.join("file");
        std::fs::write(&file, b"x").expect("file");
        let link = dir.join("link");
        std::os::unix::fs::symlink(&dir, &link).expect("link");
        let socket = dir.join("s.sock");
        let _bound = std::os::unix::net::UnixListener::bind(&socket).expect("a socket node");
        let path = |p: &Path| p.display().to_string();
        assert!(sh_ok("lane_check_kind \"$1\" socket", &[&path(&socket)]));
        assert!(!sh_ok("lane_check_kind \"$1\" socket", &[&path(&file)]));
        assert!(!sh_ok("lane_check_kind \"$1\" socket", &[&path(&dir)]));
        assert!(!sh_ok(
            "lane_check_kind \"$1\" socket",
            &[&path(&dir.join("none"))]
        ));
        assert!(sh_ok("lane_check_kind \"$1\" dir", &[&path(&dir)]));
        assert!(!sh_ok("lane_check_kind \"$1\" dir", &[&path(&link)]));
        assert!(!sh_ok("lane_check_kind \"$1\" dir", &[&path(&file)]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sourcing_the_lane_library_runs_nothing() {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(format!(
                "before=$(env | sort); . '{}'; after=$(env | sort); \
                 [ \"$before\" = \"$after\" ] || exit 3; \
                 [ -z \"${{LANE_HC+x}}${{LANE_ROOT+x}}${{LANE_SESSION+x}}${{LANE_SOCKET+x}}\" ] || exit 4",
                lib()
            ))
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        // A kill is retried once inside; exit 3 or 4 (the library ran something) fails at once.
        let output = output_unless_killed(&mut command);
        assert!(
            output.status.success(),
            "exit {:?}, stderr {:?}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stdout.is_empty() && output.stderr.is_empty());
    }
}
