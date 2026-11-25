//! The packaged-app CSP probe: compiled only with the csp-probe feature, never in a default build.
//!
//! The probe build runs the real app against an isolated data directory the harness names. A
//! script injected at document start records every `securitypolicyviolation`, the webview's
//! probe module reports the renders and violations once, and this module writes them to a
//! file and exits. The webview gets no file or shell access for that: the file is written
//! here.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tauri::plugin::{Builder, TauriPlugin};
use tauri::{AppHandle, Runtime};

/// Where the harness wants the result file. It must be an absolute path.
pub const OUT_ENV: &str = "COMBINE_CSP_PROBE_OUT";
/// The isolated data directory the probe app uses instead of the user's. It must be absolute.
pub const DATA_ENV: &str = "COMBINE_CSP_PROBE_DATA";
/// When set, the init script plants a blocked image after the webview is ready, so the harness
/// can prove the listener sees a violation.
pub const PLANT_ENV: &str = "COMBINE_CSP_PROBE_PLANT";
/// How many distinct panels must have a render status before the probe reports.
pub const PROBE_PANELS: usize = 2;
/// How many violations the init script keeps; the count goes on past it.
pub const VIOLATIONS_KEPT: usize = 200;

const VERSION: u32 = 1;

fn absolute(name: &str, value: Option<OsString>) -> Result<PathBuf, String> {
    let value = value.ok_or_else(|| format!("{name} is not set"))?;
    let path = PathBuf::from(value);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(format!("{name} must be an absolute path"))
    }
}

fn data_dir_from(out: Option<OsString>, data: Option<OsString>) -> Result<PathBuf, String> {
    absolute(OUT_ENV, out)?;
    absolute(DATA_ENV, data)
}

/// Both `OUT_ENV` and `DATA_ENV` set and absolute; else the reason, and the app fails to start.
pub fn data_dir() -> Result<PathBuf, String> {
    data_dir_from(std::env::var_os(OUT_ENV), std::env::var_os(DATA_ENV))
}

/// The script's text. It is pure so a test can read it.
pub fn init_script(plant: bool) -> String {
    let planted = if plant {
        r#"
  window.addEventListener('combine-csp-probe-ready', function () {
    var i = document.createElement('img');
    i.src = 'https://example.invalid/planted.png';
    document.body.appendChild(i);
  }, { once: true });"#
    } else {
        ""
    };
    format!(
        r#"(function () {{
  var probe = {{ panels: {PROBE_PANELS}, violations: [], count: 0, notify: null }};
  Object.defineProperty(window, '__COMBINE_CSP_PROBE__', {{ value: probe }});
  document.addEventListener('securitypolicyviolation', function (e) {{
    probe.count += 1;
    if (probe.violations.length < {VIOLATIONS_KEPT}) probe.violations.push({{
      directive: String(e.violatedDirective), effectiveDirective: String(e.effectiveDirective),
      blockedUri: String(e.blockedURI), sourceFile: String(e.sourceFile || ''),
      line: e.lineNumber | 0, column: e.columnNumber | 0,
      sample: String(e.sample || '').slice(0, 256), disposition: String(e.disposition) }});
    if (typeof probe.notify === 'function') probe.notify();
  }}, true);{planted}
}})();
"#
    )
}

/// The plugin that injects the init script. It has no commands and no permissions.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    let plant = std::env::var_os(PLANT_ENV).is_some();
    Builder::new("csp-probe")
        .js_init_script(init_script(plant))
        .build()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeRender {
    pub space: String,
    pub panel: String,
    pub revision: String,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeViolation {
    pub directive: String,
    pub effective_directive: String,
    pub blocked_uri: String,
    pub source_file: String,
    pub line: u32,
    pub column: u32,
    pub sample: String,
    pub disposition: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProbeReport {
    pub renders: Vec<ProbeRender>,
    pub violations: Vec<ProbeViolation>,
    pub violation_count: u32,
    pub timed_out: bool,
}

#[derive(Serialize)]
struct ResultFile<'a> {
    version: u32,
    #[serde(flatten)]
    report: &'a ProbeReport,
}

/// Writes `{"version":1, ...report}`, pretty-printed, to the absolute `path`.
pub fn write_result(path: &Path, report: &ProbeReport) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the result path must be absolute",
        ));
    }
    let mut text = serde_json::to_string_pretty(&ResultFile {
        version: VERSION,
        report,
    })
    .map_err(io::Error::other)?;
    text.push('\n');
    std::fs::write(path, text)
}

fn write_to_out(report: &ProbeReport) -> Result<(), String> {
    let path = absolute(OUT_ENV, std::env::var_os(OUT_ENV))?;
    write_result(&path, report).map_err(|err| format!("could not write {}: {err}", path.display()))
}

/// Receives the probe's one report, writes it and ends the run: exit 0 when it was written, 1
/// when it was not.
#[tauri::command]
pub fn csp_probe_report(app: AppHandle, report: ProbeReport) -> Result<(), String> {
    match write_to_out(&report) {
        Ok(()) => {
            app.exit(0);
            Ok(())
        }
        Err(err) => {
            eprintln!("csp probe: {err}");
            app.exit(1);
            Err(err)
        }
    }
}

#[cfg(all(test, feature = "csp-probe"))]
mod tests {
    use std::fs;

    use serde_json::{json, Value};

    use super::*;

    fn report() -> ProbeReport {
        ProbeReport {
            renders: vec![ProbeRender {
                space: "/work".to_string(),
                panel: "notes".to_string(),
                revision: "12".to_string(),
                status: "rendered".to_string(),
                error: None,
            }],
            violations: vec![ProbeViolation {
                directive: "img-src".to_string(),
                effective_directive: "img-src".to_string(),
                blocked_uri: "https://example.invalid/planted.png".to_string(),
                source_file: String::new(),
                line: 0,
                column: 0,
                sample: String::new(),
                disposition: "enforce".to_string(),
            }],
            violation_count: 1,
            timed_out: false,
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(format!("/tmp/c48b-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn ipc_contract_names_match_fixture() {
        let path = format!(
            "{}/../web/test/fixtures/ipc-contract.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let contract: Value =
            serde_json::from_str(&fs::read_to_string(path).expect("fixture")).expect("JSON");
        assert_eq!(
            contract["commands"]["cspProbeReport"],
            stringify!(csp_probe_report)
        );
    }

    #[test]
    fn write_result_writes_the_versioned_report() {
        let dir = scratch("write");
        let path = dir.join("result.json");
        write_result(&path, &report()).expect("written");
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&path).expect("file")).expect("JSON");
        assert_eq!(
            written,
            json!({
                "version": 1,
                "renders": [{
                    "space": "/work", "panel": "notes", "revision": "12",
                    "status": "rendered", "error": null
                }],
                "violations": [{
                    "directive": "img-src", "effectiveDirective": "img-src",
                    "blockedUri": "https://example.invalid/planted.png", "sourceFile": "",
                    "line": 0, "column": 0, "sample": "", "disposition": "enforce"
                }],
                "violationCount": 1,
                "timedOut": false
            })
        );
        assert!(fs::read_to_string(&path).expect("file").contains("\n  "));
        let decoded: ProbeReport = serde_json::from_value(json!({
            "renders": [], "violations": [], "violationCount": 0, "timedOut": true
        }))
        .expect("decodes");
        assert!(decoded.timed_out);
        assert!(serde_json::from_value::<ProbeReport>(json!({
            "renders": [], "violations": [], "violationCount": 0, "timedOut": true, "x": 1
        }))
        .is_err());
        fs::remove_dir_all(dir).expect("cleanup");
    }

    #[test]
    fn relative_or_missing_paths_are_refused() {
        let abs = || Some(OsString::from("/tmp/x"));
        assert_eq!(data_dir_from(abs(), abs()), Ok(PathBuf::from("/tmp/x")));
        assert!(data_dir_from(None, abs()).is_err());
        assert!(data_dir_from(abs(), None).is_err());
        assert!(data_dir_from(Some(OsString::from("out.json")), abs()).is_err());
        assert!(data_dir_from(abs(), Some(OsString::from("data"))).is_err());
        let err = write_result(Path::new("relative.json"), &report()).expect_err("refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(!Path::new("relative.json").exists());
    }

    #[test]
    fn the_init_script_listens_from_document_start() {
        let script = init_script(false);
        assert!(script.contains("securitypolicyviolation"));
        assert!(script.contains("__COMBINE_CSP_PROBE__"));
        assert!(script.contains(&format!("violations.length < {VIOLATIONS_KEPT}")));
        assert!(script.contains(&format!("panels: {PROBE_PANELS}")));
        for late in ["DOMContentLoaded", "'load'", "readystatechange"] {
            assert!(!script.contains(late), "waits for {late}");
        }
        assert!(!script.contains("example.invalid"));
        let planted = init_script(true);
        assert!(planted.contains("combine-csp-probe-ready"));
        assert!(planted.contains("https://example.invalid/planted.png"));
        let listener = planted.find("securitypolicyviolation").expect("listener");
        assert!(listener < planted.find("planted.png").expect("plant"));
    }
}
