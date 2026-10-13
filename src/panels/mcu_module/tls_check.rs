//! The IoT tab's "Test from this PC": does the broker pass the board's own
//! certificate checks, and does it take the CONNECT, the subscriptions and a
//! test publication - what an AWS IoT policy allows or refuses?
//!
//! The checks are the firmware's - `iot_templates/tls_verify.rs`, the text
//! every TLS `mqtt_link.rs` carries - compiled into a small host program
//! (`iot_gen::tls_check_files`) beside this instance's workspace, built once with
//! the user's own cargo (about a minute; seconds after) and run with the
//! request on stdin. Nothing of it is in the IDE's own dependencies.

use std::hash::{Hash as _, Hasher as _};
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::panels::mcu_module::codegen::iot_gen;

/// What to test: the tab's settings, the CAs of `mqtt_ca.rs`, and the
/// credentials of `secrets.rs` (sent only after the certificate checked out).
/// No `Debug`: it carries the password and the client key.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Request {
    pub host: String,
    pub port: u16,
    pub server_name: String,
    pub verify: bool,
    pub ca: Vec<u8>,
    pub client_id: String,
    pub user: String,
    pub password: String,
    /// AWS IoT's ALPN (port 443).
    pub alpn: bool,
    /// The client certificate and its key, as PEM - empty: none. The checker
    /// reads them with the board's own code.
    pub client_cert: String,
    pub client_key: String,
    /// The keep-alive the board asks for, and the filters it subscribes to.
    pub keep_alive: u16,
    pub subscribe: Vec<String>,
    /// One QoS 1 message goes there - under the board's own name, where the
    /// AWS IoT policy the tab writes allows it. Empty: none.
    pub test_topic: String,
}

impl Request {
    /// Which settings a result was for: shown beside it once they change.
    pub fn key(&self) -> String {
        format!("{}:{} as {}", self.host, self.port, self.server_name)
    }

    /// What a result remembers of its request: every field counts as a
    /// change (the CA, the check, the login), the password only as a hash.
    pub fn tested(&self) -> Tested {
        let mut h = std::hash::DefaultHasher::new();
        self.hash(&mut h);
        Tested {
            key: self.key(),
            fingerprint: h.finish(),
            verify: self.verify,
        }
    }
}

/// The request a result was for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tested {
    /// `Request::key`, to show.
    pub key: String,
    /// A hash of the whole request: anything changed makes the result stale.
    pub fingerprint: u64,
    /// The certificate was checked (the dates note depends on it).
    pub verify: bool,
}

/// Where a test stands.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    /// Building the checker - about a minute the first time.
    Building,
    /// Connecting to the broker.
    Connecting,
    Done {
        ok: bool,
        /// The one line that says how it went.
        summary: String,
        /// What happened on the way (resolved, connected, checked).
        steps: Vec<String>,
        /// The broker's chain is signed with P-384 somewhere.
        p384: bool,
        /// What was tested.
        tested: Tested,
    },
}

pub type Shared = Arc<Mutex<State>>;

/// Run a test in the background; `ctx` repaints the tab as it moves on.
pub fn start(request: Request, ctx: egui::Context) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(State::Building));
    let out = Arc::clone(&shared);
    std::thread::spawn(move || {
        let done = run(&request, &out, &ctx);
        if let Ok(mut s) = out.lock() {
            *s = done;
        }
        ctx.request_repaint();
    });
    shared
}

/// The checker's folder: BESIDE this instance's workspace, never in it.
/// Inside, every firmware write prunes a crate it does not know and
/// rust-analyzer takes it for a detached library - the checker would be
/// deleted mid-build and rebuilt cold every time.
fn dir() -> PathBuf {
    let name = match crate::workspace::slot() {
        0 | 1 => "rustonchip_tls_check".to_owned(),
        s => format!("rustonchip_tls_check_{s}"),
    };
    crate::workspace::dir().with_file_name(name)
}

/// The host's target triple: a `build.target` in the user's own cargo config
/// (a thumb target, set once for embedded work) must not turn the checker
/// into firmware. `None` = leave the target to cargo.
fn host_triple(dir: &Path) -> Option<String> {
    let mut rustc = Command::new("rustc");
    crate::build::no_window(&mut rustc)
        .arg("-vV")
        .current_dir(dir)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let out = rustc.output().ok().filter(|o| o.status.success())?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
}

/// The request as the checker reads it: one hex line per value.
pub fn stdin_lines(r: &Request) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let mut o = String::new();
    for field in [
        r.host.as_bytes(),
        r.port.to_string().as_bytes(),
        r.server_name.as_bytes(),
        if r.verify {
            b"1".as_slice()
        } else {
            b"0".as_slice()
        },
        &r.ca,
        r.client_id.as_bytes(),
        r.user.as_bytes(),
        r.password.as_bytes(),
        if r.alpn {
            b"1".as_slice()
        } else {
            b"0".as_slice()
        },
        r.client_cert.as_bytes(),
        r.client_key.as_bytes(),
        r.keep_alive.to_string().as_bytes(),
        r.subscribe.join("
").as_bytes(),
        r.test_topic.as_bytes(),
    ] {
        o.push_str(&hex(field));
        o.push('\n');
    }
    o
}

/// The checker's answer, read out of what it printed.
pub fn parse_output(stdout: &str, tested: Tested) -> State {
    let mut steps = Vec::new();
    let mut p384 = false;
    let mut result = None;
    for line in stdout.lines() {
        if let Some(info) = line.strip_prefix("INFO ") {
            if info == "p384" {
                p384 = true;
            } else {
                steps.push(info.to_owned());
            }
        } else if let Some(r) = line.strip_prefix("RESULT ok ") {
            result = Some((true, r.to_owned()));
        } else if let Some(r) = line.strip_prefix("RESULT fail ") {
            result = Some((false, r.to_owned()));
        }
    }
    let (ok, summary) =
        result.unwrap_or((false, "the checker stopped without an answer".to_owned()));
    State::Done {
        ok,
        summary,
        steps,
        p384,
        tested,
    }
}

fn failed(summary: impl Into<String>, tested: &Tested) -> State {
    State::Done {
        ok: false,
        summary: summary.into(),
        steps: Vec::new(),
        p384: false,
        tested: tested.clone(),
    }
}

/// The checker, written (only what changed: cargo then has nothing to
/// rebuild) and built: its executable.
fn build_checker() -> Result<PathBuf, String> {
    let dir = dir();
    for (name, body) in iot_gen::tls_check_files() {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
            continue;
        }
        path.parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &body))
            .map_err(|e| format!("the checker could not be written to {}: {e}", dir.display()))?;
    }
    // Run FROM the checker's folder: cargo and rustup look for their config
    // and `rust-toolchain.toml` from the working directory, and a firmware
    // project's (thumb target, build-std) must not apply here.
    let host = host_triple(&dir);
    let mut build = Command::new("cargo");
    crate::build::no_window(&mut build)
        .args(["build", "--release", "--quiet", "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .args(host.iter().flat_map(|h| ["--target", h.as_str()]))
        .current_dir(&dir)
        .env("CARGO_TARGET_DIR", dir.join("target"))
        .env_remove("CARGO_BUILD_TARGET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    match build.output() {
        Err(e) => return Err(format!("cargo could not be started: {e}")),
        Ok(out) if !out.status.success() => {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err
                .lines()
                .filter(|l| !l.trim().is_empty())
                .rev()
                .take(3)
                .collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            return Err(format!("the checker did not build: {}", tail.join(" / ")));
        }
        Ok(_) => {}
    }
    Ok(dir
        .join("target")
        .join(host.as_deref().unwrap_or(""))
        .join("release")
        .join("rustonchip-tls-check")
        .with_extension(std::env::consts::EXE_EXTENSION))
}

/// Runs the checker with `args`, `input` on its stdin; what it printed. A
/// minute at most: a broker that never answers must not leave the tab
/// waiting for ever.
fn run_checker(exe: &Path, args: &[&str], input: &str) -> Result<String, String> {
    let mut check = Command::new(exe);
    crate::build::no_window_raw(&mut check)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = check
        .spawn()
        .map_err(|e| format!("the checker could not be started: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut s);
        }
        s
    });
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "no answer within a minute - the broker or the network is silent".into(),
                );
            }
        }
    }
    Ok(reader.join().unwrap_or_default())
}

fn run(request: &Request, shared: &Shared, ctx: &egui::Context) -> State {
    let tested = request.tested();
    let exe = match build_checker() {
        Ok(exe) => exe,
        Err(why) => return failed(why, &tested),
    };
    if let Ok(mut s) = shared.lock() {
        *s = State::Connecting;
    }
    ctx.request_repaint();
    match run_checker(&exe, &[], &stdin_lines(request)) {
        Ok(stdout) => parse_output(&stdout, tested),
        Err(why) => failed(why, &tested),
    }
}

// ── A key and its certificate request ────────────────────────────────────

/// Who the certificate is for, and the key: `None` = make a new one.
#[derive(Clone)]
pub struct CsrRequest {
    pub country: String,
    pub organization: String,
    pub common_name: String,
    pub key: Option<String>,
    /// The project it was asked for: an answer for another is dropped.
    pub project: PathBuf,
}

/// What came back. No `Debug`: it carries the key.
pub struct CsrDone {
    /// The key the checker made, as PEM - `None` when it was given.
    pub key: Option<String>,
    /// The request, as PEM.
    pub csr: String,
    pub project: PathBuf,
}

/// Where a request stands.
#[derive(Default)]
pub enum CsrState {
    #[default]
    Idle,
    Building,
    Done(Result<CsrDone, String>),
}

pub type CsrShared = Arc<Mutex<CsrState>>;

/// Makes a key (or takes `request.key`) and its certificate request, in the
/// background.
pub fn start_csr(request: CsrRequest, ctx: egui::Context) -> CsrShared {
    let shared: CsrShared = Arc::new(Mutex::new(CsrState::Building));
    let out = Arc::clone(&shared);
    std::thread::spawn(move || {
        let done = csr(&request);
        if let Ok(mut s) = out.lock() {
            *s = CsrState::Done(done);
        }
        ctx.request_repaint();
    });
    shared
}

/// The `csr` mode's stdin: one hex line per value.
fn csr_lines(r: &CsrRequest) -> String {
    let hex = |s: &str| s.bytes().map(|x| format!("{x:02x}")).collect::<String>();
    [
        &r.country,
        &r.organization,
        &r.common_name,
        r.key.as_deref().unwrap_or(""),
    ]
    .iter()
    .map(|s| hex(s) + "\n")
    .collect()
}

/// The `csr` mode's answer: a key it made (when it was asked to), the
/// request - both or nothing.
pub fn parse_csr(stdout: &str, want_key: bool, project: PathBuf) -> Result<CsrDone, String> {
    let unhex = |h: &str| -> Option<String> {
        let h = h.trim();
        let bytes: Option<Vec<u8>> = (0..h.len() / 2)
            .map(|i| u8::from_str_radix(h.get(2 * i..2 * i + 2)?, 16).ok())
            .collect();
        String::from_utf8(bytes?).ok()
    };
    let (mut key, mut csr, mut result) = (None, None, None);
    for line in stdout.lines() {
        if let Some(h) = line.strip_prefix("KEY ") {
            key = unhex(h);
        } else if let Some(h) = line.strip_prefix("CSR ") {
            csr = unhex(h);
        } else if let Some(r) = line.strip_prefix("RESULT ok ") {
            result = Some(Ok(r.to_owned()));
        } else if let Some(r) = line.strip_prefix("RESULT fail ") {
            result = Some(Err(r.to_owned()));
        }
    }
    match result {
        Some(Ok(_)) => {}
        Some(Err(why)) => return Err(why),
        None => return Err("the checker stopped without an answer".into()),
    }
    let csr = csr
        .filter(|c| c.contains("-----BEGIN CERTIFICATE REQUEST-----"))
        .ok_or("no request came back")?;
    if want_key
        && !key
            .as_deref()
            .is_some_and(|k| k.contains("-----BEGIN EC PRIVATE KEY-----"))
    {
        return Err("no key came back".into());
    }
    Ok(CsrDone {
        key: if want_key { key } else { None },
        csr,
        project,
    })
}

fn csr(request: &CsrRequest) -> Result<CsrDone, String> {
    let exe = build_checker()?;
    let stdout = run_checker(&exe, &["csr"], &csr_lines(request))?;
    parse_csr(&stdout, request.key.is_none(), request.project.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request goes over as hex lines - a password with a newline or a
    /// non-ASCII host cannot break the framing.
    #[test]
    fn the_request_is_one_hex_line_per_value() {
        let r = Request {
            host: "broker.example.org".into(),
            port: 8883,
            server_name: "broker.example.org".into(),
            verify: true,
            ca: vec![0x30, 0x82],
            client_id: "rustonchip".into(),
            user: "me".into(),
            password: "a\nb".into(),
            alpn: false,
            client_cert: String::new(),
            client_key: String::new(),
            keep_alive: 60,
            subscribe: vec!["a/b".into(), "c/#".into()],
            test_topic: "rustonchip/test".into(),
        };
        let text = stdin_lines(&r);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 14);
        assert_eq!(lines[11], "3630");
        assert_eq!(lines[12], "612f620a632f23", "the filters one per line inside their hex line");
        assert_eq!(lines[1], "38383833");
        assert_eq!(lines[3], "31");
        assert_eq!(lines[4], "3082");
        assert_eq!(lines[7], "610a62", "the newline stays inside its line");
        assert_eq!(r.key(), "broker.example.org:8883 as broker.example.org");
        // The check, the CA or the login changed: the result is stale even
        // though the broker is the same.
        let before = r.tested();
        for changed in [
            Request {
                verify: false,
                ..r.clone()
            },
            Request {
                ca: vec![0x30, 0x83],
                ..r.clone()
            },
            Request {
                password: "c".into(),
                ..r.clone()
            },
            Request {
                subscribe: vec!["a/b".into()],
                ..r.clone()
            },
        ] {
            assert_eq!(changed.key(), before.key);
            assert_ne!(changed.tested().fingerprint, before.fingerprint);
        }
        assert_eq!(r.clone().tested(), before);
    }

    /// The checker's printout read back: the steps, P-384, the verdict.
    #[test]
    fn the_answer_is_read_back() {
        let out = "INFO resolved 1.2.3.4:8886\nINFO TCP connected\nINFO p384\nRESULT ok the broker accepted\n";
        let k = Tested {
            key: "k".into(),
            ..Tested::default()
        };
        let State::Done {
            ok,
            summary,
            steps,
            p384,
            tested,
        } = parse_output(out, k.clone())
        else {
            panic!("done");
        };
        assert!(ok && p384);
        assert_eq!(summary, "the broker accepted");
        assert_eq!(steps, ["resolved 1.2.3.4:8886", "TCP connected"]);
        assert_eq!(tested, k);
        let State::Done { ok, summary, .. } =
            parse_output("RESULT fail bad CA\n", Tested::default())
        else {
            panic!("done");
        };
        assert!(!ok && summary == "bad CA");
        assert!(matches!(
            parse_output("", Tested::default()),
            State::Done { ok: false, .. }
        ));
    }

    /// The checker compiles the firmware's own certificate checks.
    #[test]
    fn the_checker_carries_the_firmwares_checks() {
        let files = iot_gen::tls_check_files();
        let get = |n: &str| {
            files
                .iter()
                .find(|(f, _)| *f == n)
                .map(|(_, b)| b.as_str())
                .unwrap()
        };
        assert!(get("src/main.rs").contains("include!(\"tls_verify.rs\");"));
        assert!(get("Cargo.toml").contains("features = [\"std\", \"rsa\", \"p384\"]"));
        assert!(get("Cargo.toml").contains("[workspace]"));
        let verify = get("src/tls_verify.rs");
        assert!(verify.contains("struct Verifier<'a>") && verify.contains("fn key_is_rsa("));
    }

    /// The checker lives beside the workspace: inside, a firmware write would
    /// prune it as a foreign crate.
    #[test]
    fn the_checker_is_not_in_the_firmware_workspace() {
        let ws = crate::workspace::dir();
        let d = dir();
        assert!(
            !d.starts_with(&ws),
            "{} is inside {}",
            d.display(),
            ws.display()
        );
        assert_eq!(d.parent(), ws.parent());
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(
            !name.starts_with(crate::names::LEGACY_SLOT_PREFIX),
            "the slot sweep would delete {name}"
        );
    }

    /// The helper's `csr` mode, for real: a new key and its request, then a
    /// request for a given key - each checked with the board's own reader.
    /// Writes both to %TEMP%/rustonchip_csr_check.* for `openssl req -verify`.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip csr_mode_makes_a_key_and_its_request -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "builds the checker"]
    fn csr_mode_makes_a_key_and_its_request() {
        use crate::panels::mcu_module::iot;
        let request = |key: Option<String>| CsrRequest {
            country: "RO".into(),
            organization: "RustOnChip".into(),
            common_name: "rustonchip-test-thing".into(),
            key,
            project: PathBuf::from("p"),
        };
        let made = csr(&request(None)).unwrap_or_else(|e| panic!("{e}"));
        let key = made.key.expect("a new key");
        let (_, point) = iot::client_key(&key).unwrap();
        assert!(point.is_some(), "the key carries its public half");
        assert!(
            made.csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"),
            "{}",
            made.csr
        );
        let dir = std::env::temp_dir();
        std::fs::write(dir.join("rustonchip_csr_check.csr"), &made.csr).unwrap();
        std::fs::write(dir.join("rustonchip_csr_check.key"), &key).unwrap();
        // Again for the same key: no new key back.
        let again = csr(&request(Some(key))).unwrap_or_else(|e| panic!("{e}"));
        assert!(again.key.is_none());
        assert!(again.csr.starts_with("-----BEGIN CERTIFICATE REQUEST-----"));
        // A bad subject is the helper's refusal, not a crash.
        let mut bad = request(None);
        bad.country = "ROU".into();
        assert!(csr(&bad).is_err());
        println!("{}", made.csr);
    }

    /// Building and running the real checker against test.mosquitto.org:
    /// network and a minute of cargo, so by hand only.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip tls_check_against_mosquitto -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "builds the checker and connects to test.mosquitto.org"]
    fn tls_check_against_mosquitto() {
        let pem = iot_gen::mqtt_ca_file();
        let ca =
            crate::panels::mcu_module::iot::pem_der(iot_gen::ca_pem_of(&pem).unwrap()).unwrap();
        let request = |port: u16, name: &str| Request {
            host: "test.mosquitto.org".into(),
            port,
            server_name: name.into(),
            verify: true,
            ca: ca.clone(),
            client_id: "rustonchip-ide-test".into(),
            user: String::new(),
            password: String::new(),
            alpn: false,
            client_cert: String::new(),
            client_key: String::new(),
            keep_alive: 60,
            subscribe: vec!["rustonchip-ide-test/#".into()],
            test_topic: "rustonchip-ide-test/test".into(),
        };
        let ctx = egui::Context::default();
        for (port, name, want) in [
            (8886, "test.mosquitto.org", true),
            (8883, "test.mosquitto.org", false),
            (8886, "mosquitto.org", false),
        ] {
            let shared = Shared::default();
            let done = run(&request(port, name), &shared, &ctx);
            println!("{port} {name}: {done:?}");
            assert!(
                matches!(done, State::Done { ok, .. } if ok == want),
                "{port} {name}: {done:?}"
            );
        }
    }
}
