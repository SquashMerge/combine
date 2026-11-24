//! The `open_url` host command: opens an http(s) link from rendered content in the system browser.

use std::io;
use std::process::{Command, Stdio};

use url::Url;

/// Why a URL was not opened. The texts are for logs and tests; the UI never sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenUrlRefusal {
    Empty,
    Whitespace,
    ControlCharacter,
    Malformed,
    Scheme,
    Spawn,
}

impl std::fmt::Display for OpenUrlRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "refused: empty URL",
            Self::Whitespace => "refused: leading or trailing whitespace",
            Self::ControlCharacter => "refused: control character",
            Self::Malformed => "refused: not a valid URL",
            Self::Scheme => "refused: only https and http are allowed",
            Self::Spawn => "failed to start /usr/bin/open",
        })
    }
}

/// Starts the system handler for a URL that already passed `check_url`.
trait Opener {
    fn open(&self, url: &Url) -> io::Result<()>;
}

/// Opens the link in the default browser.
#[tauri::command]
pub fn open_url(url: String) -> Result<(), String> {
    open_with(&url, &SystemOpener).map_err(|r| r.to_string())
}

fn open_with(url: &str, opener: &dyn Opener) -> Result<(), OpenUrlRefusal> {
    let url = check_url(url)?;
    opener.open(&url).map_err(|_| OpenUrlRefusal::Spawn)
}

/// Accepts only an absolute https or http URL. Nothing is trimmed: the string is judged as given.
fn check_url(url: &str) -> Result<Url, OpenUrlRefusal> {
    if url.is_empty() {
        return Err(OpenUrlRefusal::Empty);
    }
    if url.starts_with(char::is_whitespace) || url.ends_with(char::is_whitespace) {
        return Err(OpenUrlRefusal::Whitespace);
    }
    if url.chars().any(|c| ('\u{0}'..='\u{1f}').contains(&c)) {
        return Err(OpenUrlRefusal::ControlCharacter);
    }
    let parsed = Url::parse(url).map_err(|_| OpenUrlRefusal::Malformed)?;
    match parsed.scheme() {
        "https" | "http" => Ok(parsed),
        _ => Err(OpenUrlRefusal::Scheme),
    }
}

/// One argv item, the serialized URL, with no shell. It always starts with `http`, so `open`
/// can't read it as an option.
fn open_command(url: &Url) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

struct SystemOpener;

impl Opener for SystemOpener {
    fn open(&self, url: &Url) -> io::Result<()> {
        let mut child = open_command(url).spawn()?;
        let _ = std::thread::Builder::new()
            .name("open-url-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct RecordingOpener(RefCell<Vec<String>>);

    impl RecordingOpener {
        fn new() -> Self {
            Self(RefCell::new(Vec::new()))
        }

        fn recorded(&self) -> Vec<String> {
            self.0.borrow().clone()
        }
    }

    impl Opener for RecordingOpener {
        fn open(&self, url: &Url) -> io::Result<()> {
            self.0.borrow_mut().push(url.as_str().to_string());
            Ok(())
        }
    }

    struct FailingOpener;

    impl Opener for FailingOpener {
        fn open(&self, _url: &Url) -> io::Result<()> {
            Err(io::Error::other("no open"))
        }
    }

    fn fixture(name: &str) -> serde_json::Value {
        let path = format!("{}/../web/test/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture is readable");
        serde_json::from_str(&text).expect("fixture is JSON")
    }

    fn assert_refused(inputs: &[&str], expected: OpenUrlRefusal) {
        for input in inputs {
            let opener = RecordingOpener::new();
            assert_eq!(open_with(input, &opener), Err(expected), "{input:?}");
            assert!(opener.recorded().is_empty(), "{input:?}");
        }
    }

    #[test]
    fn open_url_r1_accepted_urls_are_opened_serialized() {
        let cases = [
            ("https://example.com", "https://example.com/"),
            ("http://example.com/p?q=1#f", "http://example.com/p?q=1#f"),
            ("HTTPS://EXAMPLE.COM", "https://example.com/"),
            ("HtTp://example.com", "http://example.com/"),
            (
                "https://b\u{fc}cher.example/",
                "https://xn--bcher-kva.example/",
            ),
            ("https:example.com", "https://example.com/"),
            ("https://user@example.com", "https://user@example.com/"),
        ];
        for (input, expected) in cases {
            let opener = RecordingOpener::new();
            assert_eq!(open_with(input, &opener), Ok(()), "{input:?}");
            assert_eq!(opener.recorded(), vec![expected.to_string()], "{input:?}");
        }
    }

    #[test]
    fn open_url_r2_other_schemes_are_refused() {
        assert_refused(
            &[
                "file:///etc/passwd",
                "javascript:alert(1)",
                "JaVaScRiPt:alert(1)",
                "vbscript:x",
                "data:text/html,<script>",
                "ftp://example.com",
                "mailto:a@b.c",
                "tel:1",
                "about:blank",
                "blob:https://example.com/x",
                "ws://example.com",
                "ssh://host",
                "tauri://localhost",
                "asset://localhost/x",
                "ipc://localhost",
                "x-combine:x",
                "httpsx://example.com",
            ],
            OpenUrlRefusal::Scheme,
        );
    }

    #[test]
    fn open_url_r3_empty_is_refused() {
        assert_refused(&[""], OpenUrlRefusal::Empty);
    }

    #[test]
    fn open_url_r4_edge_whitespace_is_refused() {
        assert_refused(
            &[
                " https://example.com",
                "https://example.com ",
                "\u{a0}https://example.com",
                "https://example.com\u{3000}",
                "\u{2028}https://example.com",
                "https://example.com/\r",
            ],
            OpenUrlRefusal::Whitespace,
        );
    }

    #[test]
    fn open_url_r5_control_characters_are_refused() {
        assert_refused(
            &[
                "java\tscript:alert(1)",
                "https://exa\nmple.com",
                "\u{0}https://example.com",
                "https://example.com/\u{1b}",
                "\u{1f}x",
            ],
            OpenUrlRefusal::ControlCharacter,
        );
    }

    #[test]
    fn open_url_r6_malformed_urls_are_refused() {
        assert_refused(
            &[
                "example.com",
                "https://",
                "http://[::1",
                "//example.com",
                "/path",
                "\u{feff}https://example.com",
            ],
            OpenUrlRefusal::Malformed,
        );
    }

    #[test]
    fn open_url_r7_spawn_failure_is_reported() {
        assert_eq!(
            open_with("https://example.com", &FailingOpener),
            Err(OpenUrlRefusal::Spawn)
        );
    }

    #[test]
    fn open_url_r8_command_has_one_fixed_argv() {
        let url = check_url("https://example.com").expect("allowed URL");
        let command = open_command(&url);
        assert_eq!(command.get_program(), "/usr/bin/open");
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, [url.as_str()]);
    }

    #[test]
    fn refusal_texts_are_fixed() {
        let texts = [
            (OpenUrlRefusal::Empty, "refused: empty URL"),
            (
                OpenUrlRefusal::Whitespace,
                "refused: leading or trailing whitespace",
            ),
            (
                OpenUrlRefusal::ControlCharacter,
                "refused: control character",
            ),
            (OpenUrlRefusal::Malformed, "refused: not a valid URL"),
            (
                OpenUrlRefusal::Scheme,
                "refused: only https and http are allowed",
            ),
            (OpenUrlRefusal::Spawn, "failed to start /usr/bin/open"),
        ];
        for (refusal, text) in texts {
            assert_eq!(refusal.to_string(), text);
        }
    }

    #[test]
    fn ipc_contract_names_match_fixture() {
        let contract = fixture("ipc-contract.json");
        assert_eq!(contract["commands"]["openUrl"], stringify!(open_url));
    }
}
