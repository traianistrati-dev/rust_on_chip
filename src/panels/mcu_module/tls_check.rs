//! The IoT tab's "Test from this PC": does the broker pass the board's own
//! certificate checks, and does it take the CONNECT, the subscriptions and a
//! test publication - what an AWS IoT policy allows or refuses?
//!
//! The checks are the firmware's - `iot_templates/tls_verify.rs`, the text
//! every `tls.rs` carries, for MQTT and HTTP - compiled into a small host program
//! (`iot_gen::tls_check_files`) beside this instance's workspace, built once with
//! the user's own cargo (about a minute; seconds after) and run with the
//! request on stdin. Nothing of it is in the IDE's own dependencies.
//!
//! The same program probes a URL for the HTTP client (`http` mode): the
//! handshake and checks of `http.rs`, then one GET, its head and body held
//! against the board's buffers.

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
#[derive(Clone, Default, PartialEq, Eq, Hash)]
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
    /// The Device Shadow over MQTT: the answer topics the board subscribes
    /// first (SHADOW_SUBSCRIBE, 7 a shadow) - empty: none. Each shadow's get
    /// is asked as the board would, its answer held to the board's
    /// SHADOW_MAX and to the packet it takes (WORK).
    pub shadow_filters: Vec<String>,
    pub shadow_max: usize,
    pub shadow_work: usize,
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

/// What the HTTP probe tests: a URL, by the board's own checks (the CAs of
/// `http_ca.rs`) and against its buffers. No `Debug`: a query may carry an
/// API key, and the client key is a secret.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct HttpRequest {
    /// `http://` or `https://`, a host name or an IPv4 address, a port, the
    /// path and the query - as the firmware's `get()` takes it.
    pub url: String,
    pub verify: bool,
    pub ca: Vec<u8>,
    /// "P-384 chains" is on: without it the board fails a chain signed with
    /// P-384, which this PC checks either way.
    pub p384: bool,
    /// The board's buffer for the response head (HEAD_MAX), and for the
    /// body (BODY_MAX).
    pub head_max: usize,
    pub body_max: usize,
    /// What the board calls itself (some APIs refuse a request without).
    pub user_agent: String,
    /// The board's buffer for the request head (REQUEST_HEAD_MAX).
    pub request_head_max: usize,
    /// A POST of `payload` - as `aws_publish()` sends it - instead of a GET.
    pub post: Option<HttpPost>,
    /// The board's certificate and key as PEM (secrets.rs), for a server
    /// that asks: empty for any host but the tab's certificate host.
    pub client_cert: String,
    pub client_key: String,
    /// The ALPN the board's hello names: x-amzn-http-ca to AWS IoT on 443.
    pub alpn: Option<String>,
    /// `host:port` to dial instead of the URL's: a look-alike on this PC,
    /// still named and checked as the URL's host.
    pub connect_to: Option<String>,
    /// The URL is AWS IoT's HTTPS publish: refusals said in its terms.
    pub aws: bool,
    /// More headers, after Content-Length as the board writes them: AWS's
    /// x-amz-mqtt5-* for an `AwsMessage`'s properties.
    pub headers: Vec<(String, String)>,
}

/// What a POST carries.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
pub struct HttpPost {
    /// "" = no Content-Type header (`aws_publish()` sends none).
    pub content_type: String,
    pub payload: Vec<u8>,
}

impl HttpRequest {
    /// Which URL a result was for: shown beside it once it changes.
    pub fn key(&self) -> String {
        self.url.clone()
    }

    /// What a result remembers of its request: every field counts as a
    /// change - the CA, the check, the switch and the limits as much as the
    /// URL.
    pub fn tested(&self) -> Tested {
        let mut h = std::hash::DefaultHasher::new();
        self.hash(&mut h);
        Tested {
            key: self.key(),
            fingerprint: h.finish(),
            // Plain http:// checks no certificate, and so no dates either.
            verify: self.verify && https(&self.url),
        }
    }
}

/// The URL's scheme is https, in any case.
fn https(url: &str) -> bool {
    url.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"))
}

/// The request a result was for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tested {
    /// `Request::key` (or `HttpRequest::key`), to show.
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
    /// Connecting to the broker (the server, for a URL).
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
    in_background(ctx, move |shared, ctx| run(&request, shared, ctx))
}

/// Probe a URL in the background, as `start` tests a broker.
pub fn start_http(request: HttpRequest, ctx: egui::Context) -> Shared {
    in_background(ctx, move |shared, ctx| run_http(&request, shared, ctx))
}

/// `test` on its own thread, its answer in the returned state at the end.
fn in_background(
    ctx: egui::Context,
    test: impl FnOnce(&Shared, &egui::Context) -> State + Send + 'static,
) -> Shared {
    let shared: Shared = Arc::new(Mutex::new(State::Building));
    let out = Arc::clone(&shared);
    std::thread::spawn(move || {
        let done = test(&out, &ctx);
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
        r.shadow_filters.join("
").as_bytes(),
        r.shadow_max.to_string().as_bytes(),
        r.shadow_work.to_string().as_bytes(),
    ] {
        o.push_str(&hex(field));
        o.push('\n');
    }
    o
}

/// The `http` mode's request as the checker reads it: one hex line per
/// value - the URL, verify, the CAs as DER, "P-384 chains", HEAD_MAX,
/// BODY_MAX, the User-Agent, REQUEST_HEAD_MAX, then the method (POST or a
/// GET), the Content-Type, the payload, the client certificate and key, the
/// ALPN, a host:port to dial instead, AWS IoT ("1"/"0"), and more headers
/// (`Name: value` one a line) - seventeen lines, an empty value still taking
/// its own.
pub fn http_stdin_lines(r: &HttpRequest) -> String {
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let flag = |on: bool| if on { "1" } else { "0" };
    [
        r.url.as_bytes(),
        flag(r.verify).as_bytes(),
        &r.ca,
        flag(r.p384).as_bytes(),
        r.head_max.to_string().as_bytes(),
        r.body_max.to_string().as_bytes(),
        r.user_agent.as_bytes(),
        r.request_head_max.to_string().as_bytes(),
        if r.post.is_some() { b"POST" } else { b"GET" },
        r.post.as_ref().map_or(&b""[..], |p| p.content_type.as_bytes()),
        r.post.as_ref().map_or(&b""[..], |p| &p.payload[..]),
        r.client_cert.as_bytes(),
        r.client_key.as_bytes(),
        r.alpn.as_deref().unwrap_or_default().as_bytes(),
        r.connect_to.as_deref().unwrap_or_default().as_bytes(),
        flag(r.aws).as_bytes(),
        r.headers
            .iter()
            .map(|(name, value)| format!("{name}: {value}\n"))
            .collect::<String>()
            .as_bytes(),
    ]
    .iter()
    .map(|field| hex(field) + "\n")
    .collect()
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
/// minute at most: a broker (`peer`) that never answers must not leave the
/// tab waiting for ever.
fn run_checker(exe: &Path, args: &[&str], input: &str, peer: &str) -> Result<String, String> {
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
                return Err(format!(
                    "no answer within a minute - {peer} or the network is silent"
                ));
            }
        }
    }
    Ok(reader.join().unwrap_or_default())
}

fn run(request: &Request, shared: &Shared, ctx: &egui::Context) -> State {
    let input = stdin_lines(request);
    check(&[], &input, "the broker", request.tested(), shared, ctx)
}

fn run_http(request: &HttpRequest, shared: &Shared, ctx: &egui::Context) -> State {
    let input = http_stdin_lines(request);
    check(&["http"], &input, "the server", request.tested(), shared, ctx)
}

/// The checker built, then run in the mode `args` names with `input`: its
/// answer, for `tested`.
fn check(
    args: &[&str],
    input: &str,
    peer: &str,
    tested: Tested,
    shared: &Shared,
    ctx: &egui::Context,
) -> State {
    let exe = match build_checker() {
        Ok(exe) => exe,
        Err(why) => return failed(why, &tested),
    };
    if let Ok(mut s) = shared.lock() {
        *s = State::Connecting;
    }
    ctx.request_repaint();
    match run_checker(&exe, args, input, peer) {
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
    let stdout = run_checker(&exe, &["csr"], &csr_lines(request), "the broker")?;
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
            shadow_filters: vec!["$aws/things/t/shadow/get/accepted".into(), "x".into()],
            shadow_max: 2048,
            shadow_work: 2368,
        };
        let text = stdin_lines(&r);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 17);
        assert_eq!(lines[14], "246177732f7468696e67732f742f736861646f772f6765742f61636365707465640a78");
        assert_eq!((lines[15], lines[16]), ("32303438", "32333638"));
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

    /// The HTTP probe's request goes over as hex lines too, seventeen of them
    /// in the order the checker's `http` mode reads them - an empty CA (plain
    /// http://) still takes its line, and a GET without a certificate leaves
    /// the last nine empty but there.
    #[test]
    fn the_http_request_is_one_hex_line_per_value() {
        let r = HttpRequest {
            url: "https://api.open-meteo.com/v1/forecast?latitude=44.43&current=temperature_2m".into(),
            verify: true,
            ca: vec![0x30, 0x82],
            p384: false,
            head_max: 2048,
            body_max: 1024,
            user_agent: "RustOnChip".into(),
            request_head_max: 1024,
            ..HttpRequest::default()
        };
        let unhex = |h: &str| -> Vec<u8> {
            (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
                .collect()
        };
        let text = http_stdin_lines(&r);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 17);
        assert_eq!(unhex(lines[8]), b"GET");
        assert_eq!(&lines[9..], ["", "", "", "", "", "", "30", ""], "a GET, nothing more");
        assert_eq!(unhex(lines[0]), r.url.as_bytes());
        assert_eq!(lines[1], "31", "verify");
        assert_eq!(lines[2], "3082", "the CAs");
        assert_eq!(lines[3], "30", "P-384 chains");
        assert_eq!(lines[4], "32303438", "HEAD_MAX in decimal");
        assert_eq!(lines[5], "31303234", "BODY_MAX in decimal");
        assert_eq!(unhex(lines[6]), b"RustOnChip");
        assert_eq!(lines[7], "31303234", "REQUEST_HEAD_MAX in decimal");
        let plain = HttpRequest {
            url: "http://192.168.1.10:8123/api/".into(),
            ca: Vec::new(),
            ..r.clone()
        };
        let text = http_stdin_lines(&plain);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 17);
        assert_eq!(lines[2], "");
        assert_eq!(lines[4], "32303438", "the lines after the CA stay in place");
        // AWS IoT's publish: a POST, the board's identity, the ALPN, the
        // look-alike's address - each in its line, an empty payload too.
        let publish = HttpRequest {
            url: "https://a-ats.iot.eu-south-1.amazonaws.com/topics/t?qos=1".into(),
            post: Some(HttpPost {
                content_type: String::new(),
                payload: b"{}".to_vec(),
            }),
            client_cert: "CERT".into(),
            client_key: "KEY".into(),
            alpn: Some("x-amzn-http-ca".into()),
            connect_to: Some("127.0.0.1:18443".into()),
            aws: true,
            headers: vec![
                ("x-amz-mqtt5-user-properties".into(), "W3siYSI6IjEifV0=".into()),
                ("x-amz-mqtt5-payload-format-indicator".into(), "UTF8_DATA".into()),
            ],
            ..r.clone()
        };
        let text = http_stdin_lines(&publish);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 17);
        assert_eq!(unhex(lines[8]), b"POST");
        assert_eq!(lines[9], "", "no Content-Type");
        assert_eq!(unhex(lines[10]), b"{}");
        assert_eq!(unhex(lines[11]), b"CERT");
        assert_eq!(unhex(lines[12]), b"KEY");
        assert_eq!(unhex(lines[13]), b"x-amzn-http-ca");
        assert_eq!(unhex(lines[14]), b"127.0.0.1:18443");
        assert_eq!(lines[15], "31");
        assert_eq!(
            unhex(lines[16]),
            b"x-amz-mqtt5-user-properties: W3siYSI6IjEifV0=\nx-amz-mqtt5-payload-format-indicator: UTF8_DATA\n"
        );
        let empty = HttpRequest {
            post: Some(HttpPost::default()),
            ..publish
        };
        assert_eq!(http_stdin_lines(&empty).lines().count(), 17);
    }

    /// A probe's result is for its URL, and goes stale when anything it was
    /// tested with changes - the check, the CA, the switch, the limits.
    #[test]
    fn an_http_result_goes_stale_with_any_field() {
        let r = HttpRequest {
            url: "https://letsencrypt.org/".into(),
            verify: true,
            ca: vec![0x30, 0x82],
            p384: true,
            head_max: 2048,
            body_max: 1024,
            user_agent: "RustOnChip".into(),
            request_head_max: 1024,
            ..HttpRequest::default()
        };
        assert_eq!(r.key(), "https://letsencrypt.org/");
        let before = r.tested();
        assert!(before.verify);
        for changed in [
            HttpRequest {
                verify: false,
                ..r.clone()
            },
            HttpRequest {
                ca: vec![0x30, 0x83],
                ..r.clone()
            },
            HttpRequest {
                p384: false,
                ..r.clone()
            },
            HttpRequest {
                head_max: 4096,
                ..r.clone()
            },
            HttpRequest {
                body_max: 512,
                ..r.clone()
            },
            HttpRequest {
                user_agent: "curl/8".into(),
                ..r.clone()
            },
        ] {
            assert_eq!(changed.key(), before.key);
            assert_ne!(changed.tested().fingerprint, before.fingerprint);
        }
        let other = HttpRequest {
            url: "https://letsencrypt.org/docs/".into(),
            ..r.clone()
        };
        assert_ne!(other.tested().key, before.key);
        assert_ne!(other.tested().fingerprint, before.fingerprint);
        assert_eq!(r.clone().tested(), before);
        // Plain http:// checks no certificate, so no dates were checked.
        let plain = HttpRequest {
            url: "http://neverssl.com/".into(),
            ..r.clone()
        };
        assert!(!plain.tested().verify);
        let upper = HttpRequest {
            url: "HTTPS://letsencrypt.org/".into(),
            ..r.clone()
        };
        assert!(upper.tested().verify);
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
            ..Request::default()
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

    /// The `http` mode for real: Open-Meteo answers with ISRG Root X1, a
    /// TLS 1.2-only host and a chain no CA of the list ends in do not. Network
    /// and a minute of cargo, so by hand only.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip http_check_against_public_hosts -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "builds the checker and connects to public HTTPS hosts"]
    fn http_check_against_public_hosts() {
        let ca = crate::panels::mcu_module::iot::pem_der(include_str!(
            "codegen/iot_templates/ca/isrg_root_x1.pem"
        ))
        .unwrap();
        let request = |url: &str| HttpRequest {
            url: url.into(),
            verify: true,
            ca: ca.clone(),
            p384: true,
            head_max: 2048,
            body_max: 1024,
            user_agent: "RustOnChip".into(),
            request_head_max: 1024,
            ..HttpRequest::default()
        };
        let ctx = egui::Context::default();
        for (url, want) in [
            (
                "https://api.open-meteo.com/v1/forecast?latitude=44.43&longitude=26.10&current=temperature_2m",
                true,
            ),
            ("https://httpbin.org/get", false),
            ("https://api.thingspeak.com/channels/9/feeds.json?results=1", false),
        ] {
            let shared = Shared::default();
            let done = run_http(&request(url), &shared, &ctx);
            println!("{url}: {done:?}");
            assert!(
                matches!(done, State::Done { ok, .. } if ok == want),
                "{url}: {done:?}"
            );
        }
    }
}
