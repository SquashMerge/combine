//! AC13: the packaged app, built with the `csp-probe` feature, draws a Mermaid panel and a
//! Markdown panel served by an in-process daemon on the fake backend, and its webview reports
//! no Content-Security-Policy violation. A second run plants a violation to show the probe can
//! see one. Both tests are `#[ignore]`: they need the GUI session and the bundle that
//! `tools/csp-probe/run.sh` builds, and that script runs them. No herdr-core, no ssh.

mod support;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use combine_protocol::{PanelState, Renderer};
use combined::core::PanelCreate;
use combined::model::Lifetime;
use serde_json::{json, Value};
use support::{write_hosts, PanelDaemon, ScratchDir};

const HOST: &str = "00000000-0000-4000-8000-000000000048";
const MERMAID: &str = "probe-mermaid";
const MARKDOWN: &str = "probe-markdown";
const LAYOUT: &str = r#"{"version":2,"root":{"type":"split","dir":"h","ratio":0.5,"a":{"type":"tile","id":"a","panels":["probe-mermaid"],"active":"probe-mermaid"},"b":{"type":"tile","id":"b","panels":["probe-markdown"],"active":"probe-markdown"}},"groups":[]}"#;

const OUT_ENV: &str = "COMBINE_CSP_PROBE_OUT";
const DATA_ENV: &str = "COMBINE_CSP_PROBE_DATA";
const PLANT_ENV: &str = "COMBINE_CSP_PROBE_PLANT";
const APP_ENV: &str = "COMBINE_CSP_PROBE_APP";
const LAUNCH_ENV: &str = "COMBINE_CSP_PROBE_LAUNCH";

const VALIDATE_DEADLINE: Duration = Duration::from_secs(30);
const RENDER_POLL: Duration = Duration::from_millis(50);
const RENDER_DEADLINE: Duration = Duration::from_secs(240);
const APP_TIMEOUT: Duration = Duration::from_secs(300);
const EARLY_EXIT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Launch {
    Exec,
    Open,
}

/// The probe app while it runs. Dropping it kills what this test launched, and nothing else.
struct Running {
    child: Child,
    exe: PathBuf,
    mode: Launch,
}

impl Running {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if self.mode == Launch::Open {
            let _ = Command::new("/usr/bin/pkill")
                .arg("-f")
                .arg(&self.exe)
                .status();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

fn app_path() -> PathBuf {
    match std::env::var_os(APP_ENV) {
        Some(app) => PathBuf::from(app),
        None => panic!("set {APP_ENV} to the Combine.app built by tools/csp-probe/run.sh"),
    }
}

fn executable(app: &Path) -> PathBuf {
    let output = Command::new("/usr/bin/plutil")
        .args(["-extract", "CFBundleExecutable", "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .expect("run plutil");
    assert!(output.status.success(), "plutil could not read Info.plist");
    let name = String::from_utf8(output.stdout).expect("the executable name is text");
    app.join("Contents/MacOS").join(name.trim())
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../web/test/fixtures/renderers")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()))
}

fn probe_env(out_dir: &Path, plant: bool) -> Vec<(&'static str, String)> {
    let mut env = vec![
        (OUT_ENV, out_dir.join("result.json").display().to_string()),
        (DATA_ENV, out_dir.join("client").display().to_string()),
    ];
    if plant {
        env.push((PLANT_ENV, "1".to_string()));
    }
    env
}

fn launch(app: &Path, out_dir: &Path, plant: bool, mode: Launch) -> Running {
    let exe = executable(app);
    let stdout = File::create(out_dir.join("app.out")).expect("create app.out");
    let stderr = File::create(out_dir.join("app.err")).expect("create app.err");
    let env = probe_env(out_dir, plant);
    let child = match mode {
        Launch::Exec => {
            let mut command = Command::new(&exe);
            command.env_clear();
            for key in ["HOME", "USER", "LOGNAME"] {
                let value = std::env::var(key).unwrap_or_else(|_| panic!("{key} is not set"));
                command.env(key, value);
            }
            command
                .env(
                    "TMPDIR",
                    std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string()),
                )
                .env("PATH", "/usr/bin:/bin")
                .envs(env);
            command.stdout(stdout).stderr(stderr).spawn()
        }
        Launch::Open => {
            let mut command = Command::new("/usr/bin/open");
            command
                .args(["-W", "-n", "--stdout"])
                .arg(out_dir.join("app.out"))
                .arg("--stderr")
                .arg(out_dir.join("app.err"));
            for (key, value) in env {
                command.arg("--env").arg(format!("{key}={value}"));
            }
            command.arg("-a").arg(app).stdout(Stdio::null()).spawn()
        }
    }
    .unwrap_or_else(|err| panic!("start the probe app ({mode:?}): {err}"));
    Running { child, exe, mode }
}

fn tail(path: &Path) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

fn window_server_error(err: &str) -> bool {
    let err = err.to_lowercase();
    err.contains("window server") || err.contains("windowserver") || err.contains("cgsconnection")
}

struct Outcome {
    status: ExitStatus,
    mode: Launch,
    both_rendered: bool,
    render_lines: Vec<String>,
}

/// Runs the app until it exits, polling the daemon's render status every 50 ms.
fn run_app(daemon: &PanelDaemon, app: &Path, out_dir: &Path, plant: bool, ids: &[&str]) -> Outcome {
    let space = daemon.space();
    let panels = daemon.panels();
    let mut mode = match std::env::var(LAUNCH_ENV).as_deref() {
        Ok("open") => Launch::Open,
        _ => Launch::Exec,
    };
    let mut running = launch(app, out_dir, plant, mode);
    let started = Instant::now();
    let mut seen: Vec<String> = Vec::new();
    let mut rendered: Vec<&str> = Vec::new();
    loop {
        if let Some(status) = running.child.try_wait().expect("poll the app") {
            let err = tail(&out_dir.join("app.err"));
            if mode == Launch::Exec
                && !status.success()
                && started.elapsed() < EARLY_EXIT
                && window_server_error(&err)
            {
                println!("exec launch failed with a window-server error; falling back to open");
                mode = Launch::Open;
                running = launch(app, out_dir, plant, mode);
                continue;
            }
            return Outcome {
                status,
                mode,
                both_rendered: rendered.len() == ids.len(),
                render_lines: seen,
            };
        }
        if started.elapsed() > APP_TIMEOUT {
            running.stop();
            panic!(
                "the probe app did not finish in {APP_TIMEOUT:?}; app.err:\n{}",
                tail(&out_dir.join("app.err"))
            );
        }
        if started.elapsed() < RENDER_DEADLINE {
            for id in ids {
                if rendered.contains(id) {
                    continue;
                }
                let revision = panels.status(&space, id).expect("read the panel").revision;
                let reports = panels
                    .render_status(&space, id)
                    .expect("read render status");
                if let Some(report) = reports.iter().find(|report| {
                    report.revision == revision
                        && report.status == combine_protocol::RenderStatus::Rendered
                }) {
                    let line = format!(
                        "{id}: revision {revision} {:?} after {:?}",
                        report.status,
                        started.elapsed()
                    );
                    println!("render status: {line}");
                    seen.push(line);
                    rendered.push(id);
                }
            }
        }
        thread::sleep(RENDER_POLL);
    }
}

fn wait_validated(daemon: &PanelDaemon, id: &str) {
    let space = daemon.space();
    let deadline = Instant::now() + VALIDATE_DEADLINE;
    loop {
        let view = daemon.panels().status(&space, id).expect("read the panel");
        if matches!(
            view.state,
            PanelState::Validated | PanelState::ValidatedParserOnly
        ) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{id} is {:?}, not validated",
            view.state
        );
        thread::sleep(RENDER_POLL);
    }
}

/// The daemon, the panels and the client's files, then the app, then its result file.
fn probe(plant: bool) -> (Outcome, Value) {
    let app = app_path();
    let scratch = ScratchDir::new();
    let mut daemon = PanelDaemon::new(&scratch, "probe", HOST);
    daemon.seed_layout(LAYOUT);
    daemon.start();
    let space = daemon.space();
    for (id, renderer, title, file) in [
        (MERMAID, Renderer::Mermaid, "Probe Mermaid", "class.mmd"),
        (MARKDOWN, Renderer::Markdown, "Probe Markdown", "probe.md"),
    ] {
        daemon
            .panels()
            .create(
                &space,
                PanelCreate {
                    id: Some(id.to_string()),
                    lifetime: Lifetime::Persistent,
                    renderer,
                    title: Some(title.to_string()),
                    content: fixture(file),
                    idempotency_key: None,
                    created_by: None,
                    placement: None,
                },
            )
            .unwrap_or_else(|err| panic!("create {id}: {err:?}"));
    }
    wait_validated(&daemon, MERMAID);
    wait_validated(&daemon, MARKDOWN);

    let out_dir = scratch.path.join("out");
    fs::create_dir_all(&out_dir).expect("create the output dir");
    let client = out_dir.join("client");
    write_hosts(
        &client,
        json!([{ "name": "probe", "socket": daemon.checked_socket() }]),
    );
    fs::create_dir_all(client.join("hosts")).expect("create the host state dir");
    fs::write(
        client.join("hosts").join(format!("{HOST}.json")),
        serde_json::to_vec(&json!({
            "version": 1,
            "last_space": { "space_id": space.0, "selected_at_ms": 1 },
            "spaces": {},
        }))
        .expect("encode the host state"),
    )
    .expect("write the host state");

    let outcome = run_app(&daemon, &app, &out_dir, plant, &[MERMAID, MARKDOWN]);
    println!("launch mode: {:?}", outcome.mode);
    println!("app exit status: {}", outcome.status);
    let result = fs::read_to_string(out_dir.join("result.json")).unwrap_or_else(|err| {
        panic!(
            "read result.json ({err}); exit {}; app.err:\n{}",
            outcome.status,
            tail(&out_dir.join("app.err"))
        )
    });
    println!("result.json:\n{result}");
    daemon.stop();
    (
        outcome,
        serde_json::from_str(&result).expect("parse result.json"),
    )
}

#[test]
#[ignore = "needs the GUI session and the bundle built by tools/csp-probe/run.sh"]
fn packaged_app_draws_both_panels_with_no_csp_violation() {
    let (outcome, result) = probe(false);
    assert!(outcome.status.success(), "exit status {}", outcome.status);
    assert!(
        outcome.both_rendered,
        "the daemon never showed both panels rendered: {:?}",
        outcome.render_lines
    );
    assert_eq!(result["version"], 1);
    assert_eq!(result["timedOut"], false);
    for id in [MERMAID, MARKDOWN] {
        let renders = result["renders"].as_array().expect("renders is an array");
        assert!(
            renders
                .iter()
                .any(|render| render["panel"] == id && render["status"] == "rendered"),
            "{id} is not rendered in {renders:?}"
        );
    }
    assert_eq!(
        result["violations"],
        json!([]),
        "violations: {}",
        result["violations"]
    );
    assert_eq!(result["violationCount"], 0);
}

#[test]
#[ignore = "needs the GUI session and the bundle built by tools/csp-probe/run.sh"]
fn packaged_app_reports_a_planted_violation() {
    let (outcome, result) = probe(true);
    assert!(outcome.status.success(), "exit status {}", outcome.status);
    assert!(
        result["violationCount"].as_u64().expect("violationCount") >= 1,
        "no violation was reported: {result}"
    );
    let planted = result["violations"]
        .as_array()
        .expect("violations is an array")
        .iter()
        .find(|violation| {
            violation["blockedUri"]
                .as_str()
                .is_some_and(|uri| uri.starts_with("https://example.invalid/planted.png"))
                && violation["effectiveDirective"] == "img-src"
        });
    println!("planted violation: {planted:?}");
    assert!(
        planted.is_some(),
        "no img-src violation for the planted image in {}",
        result["violations"]
    );
}
