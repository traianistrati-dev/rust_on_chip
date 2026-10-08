//! The IoT tab's "Test from this PC": does the broker pass the board's own
//! certificate checks, and does it take the CONNECT?
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

/// What to test: the tab's settings, the CA of `mqtt_ca.rs`, and the
/// credentials of `secrets.rs` (sent only after the certificate checked out).
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
        if r.verify { b"1".as_slice() } else { b"0".as_slice() },
        &r.ca,
        r.client_id.as_bytes(),
        r.user.as_bytes(),
        r.password.as_bytes(),
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
    let (ok, summary) = result.unwrap_or((false, "the checker stopped without an answer".to_owned()));
    State::Done { ok, summary, steps, p384, tested }
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

fn run(request: &Request, shared: &Shared, ctx: &egui::Context) -> State {
    let tested = request.tested();
    let dir = dir();
    // The sources, rewritten only when the IDE's differ: cargo then has
    // nothing to rebuild.
    for (name, body) in iot_gen::tls_check_files() {
        let path = dir.join(name);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(body.as_str()) {
            continue;
        }
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, &body));
        if let Err(e) = written {
            return failed(format!("the checker could not be written to {}: {e}", dir.display()), &tested);
        }
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
        Err(e) => return failed(format!("cargo could not be started: {e}"), &tested),
        Ok(out) if !out.status.success() => {
            let err = String::from_utf8_lossy(&out.stderr);
            let tail: Vec<&str> = err.lines().filter(|l| !l.trim().is_empty()).rev().take(3).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            return failed(format!("the checker did not build: {}", tail.join(" / ")), &tested);
        }
        Ok(_) => {}
    }
    if let Ok(mut s) = shared.lock() {
        *s = State::Connecting;
    }
    ctx.request_repaint();

    let exe = dir
        .join("target")
        .join(host.as_deref().unwrap_or(""))
        .join("release")
        .join("rustonchip-tls-check")
        .with_extension(std::env::consts::EXE_EXTENSION);
    let mut check = Command::new(&exe);
    crate::build::no_window_raw(&mut check)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = match check.spawn() {
        Ok(c) => c,
        Err(e) => return failed(format!("the checker could not be started: {e}"), &tested),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(stdin_lines(request).as_bytes());
    }
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.read_to_string(&mut s);
        }
        s
    });
    // A broker that never answers must not leave the tab waiting for ever.
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return failed("no answer within a minute - the broker or the network is silent", &tested);
            }
        }
    }
    let stdout = reader.join().unwrap_or_default();
    parse_output(&stdout, tested)
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
        };
        let text = stdin_lines(&r);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 8);
        assert_eq!(lines[1], "38383833");
        assert_eq!(lines[3], "31");
        assert_eq!(lines[4], "3082");
        assert_eq!(lines[7], "610a62", "the newline stays inside its line");
        assert_eq!(r.key(), "broker.example.org:8883 as broker.example.org");
        // The check, the CA or the login changed: the result is stale even
        // though the broker is the same.
        let before = r.tested();
        for changed in [
            Request { verify: false, ..r.clone() },
            Request { ca: vec![0x30, 0x83], ..r.clone() },
            Request { password: "c".into(), ..r.clone() },
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
        let k = Tested { key: "k".into(), ..Tested::default() };
        let State::Done { ok, summary, steps, p384, tested } = parse_output(out, k.clone()) else {
            panic!("done");
        };
        assert!(ok && p384);
        assert_eq!(summary, "the broker accepted");
        assert_eq!(steps, ["resolved 1.2.3.4:8886", "TCP connected"]);
        assert_eq!(tested, k);
        let State::Done { ok, summary, .. } = parse_output("RESULT fail bad CA\n", Tested::default()) else {
            panic!("done");
        };
        assert!(!ok && summary == "bad CA");
        assert!(matches!(parse_output("", Tested::default()), State::Done { ok: false, .. }));
    }

    /// The checker compiles the firmware's own certificate checks.
    #[test]
    fn the_checker_carries_the_firmwares_checks() {
        let files = iot_gen::tls_check_files();
        let get = |n: &str| files.iter().find(|(f, _)| *f == n).map(|(_, b)| b.as_str()).unwrap();
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
        assert!(!d.starts_with(&ws), "{} is inside {}", d.display(), ws.display());
        assert_eq!(d.parent(), ws.parent());
        let name = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(!name.starts_with(crate::names::LEGACY_SLOT_PREFIX), "the slot sweep would delete {name}");
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
        let ca = crate::panels::mcu_module::iot::pem_der(iot_gen::ca_pem_of(&pem).unwrap()).unwrap();
        let request = |port: u16, name: &str| Request {
            host: "test.mosquitto.org".into(),
            port,
            server_name: name.into(),
            verify: true,
            ca: ca.clone(),
            client_id: "rustonchip-ide-test".into(),
            user: String::new(),
            password: String::new(),
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
            assert!(matches!(done, State::Done { ok, .. } if ok == want), "{port} {name}: {done:?}");
        }
    }
}
