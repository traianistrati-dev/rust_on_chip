//! RTT / defmt live console via the probe-rs CLI (bottom-panel "RTT" tab).
//!
//! RTT (Real-Time Transfer) streams logs through the debug probe — no USART
//! pin, ~100× faster than a UART. probe-rs decodes `defmt` frames itself when
//! the ELF carries a defmt table; plain `rtt_target` strings pass through.
//!
//! Pipeline (one orchestrator thread per session, killable at every phase):
//!  1. `cargo build --release --message-format=json` — human progress from
//!     stderr streams into the console, the ELF path comes from the JSON
//!     `compiler-artifact` messages (same trick as `crate::size`).
//!  2. `probe-rs run --chip <chip> <elf>` (flash + reset + RTT) or
//!     `probe-rs attach --chip <chip> <elf>` (RTT only, no flash), stdout +
//!     stderr streamed live until the session is stopped or the probe drops.
//!
//! Reuses the Terminal tab's scrollback machinery ([`TerminalState`],
//! [`spawn_reader`]) so ANSI colours and throttled repaints come for free.

use crate::build::no_window;
use crate::terminal::{LineKind, TerminalState, spawn_reader};
use eframe::egui;
use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// What the session does before streaming: flash first, or attach only.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RttMode {
    /// `probe-rs run` — flash `--release`, reset, then stream RTT.
    Run,
    /// `probe-rs attach` — stream RTT from the already-running firmware.
    Attach,
}

#[derive(Clone, Default, PartialEq)]
pub enum RttPhase {
    #[default]
    Idle,
    /// `cargo build --release` is running.
    Building,
    /// probe-rs is attached and streaming.
    Streaming,
    /// The last session ended with an error (shown in the tab header row).
    Error(String),
}

/// The RTT console (owned by `AppIde.rtt`): scrollback + phase + the running
/// child (cargo, then probe-rs) so Stop can kill whichever is active.
pub struct RttConsole {
    pub state: Arc<Mutex<TerminalState>>,
    pub phase: Arc<Mutex<RttPhase>>,
    /// Per-session stop flag (readers + orchestrator bail out).
    stop: Option<Arc<AtomicBool>>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Default for RttConsole {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(TerminalState::default())),
            phase: Arc::new(Mutex::new(RttPhase::Idle)),
            stop: None,
            child: Arc::new(Mutex::new(None)),
        }
    }
}

impl RttConsole {
    pub fn phase(&self) -> RttPhase {
        self.phase.lock().unwrap().clone()
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.phase(), RttPhase::Building | RttPhase::Streaming)
    }

    pub fn clear(&mut self) {
        self.state.lock().unwrap().lines.clear();
    }

    /// Kill the running child (cargo or probe-rs) and end the session.
    pub fn stop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        if self.is_busy() {
            self.state
                .lock()
                .unwrap()
                .push_plain(LineKind::Notice, "[stopped]");
        }
        *self.phase.lock().unwrap() = RttPhase::Idle;
    }

    /// Start a session: build `--release` in `project_dir`, then launch
    /// probe-rs on the produced ELF. No-op while one is already running.
    pub fn start(
        &mut self,
        mode: RttMode,
        project_dir: PathBuf,
        target: String,
        chip: String,
        // The `--probe VID:PID[:Serial]` selector, or `None` to let probe-rs
        // auto-pick the only attached probe (see [`crate::probe`]).
        probe: Option<String>,
        ctx: egui::Context,
    ) {
        if self.is_busy() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(Arc::clone(&stop));
        *self.phase.lock().unwrap() = RttPhase::Building;

        let state = Arc::clone(&self.state);
        let phase = Arc::clone(&self.phase);
        let child_slot = Arc::clone(&self.child);
        thread::spawn(move || {
            let end = run_session(
                mode,
                &project_dir,
                &target,
                &chip,
                probe.as_deref(),
                &state,
                &phase,
                &child_slot,
                &stop,
                &ctx,
            );
            *child_slot.lock().unwrap() = None;
            // A user Stop already set Idle + logged; don't overwrite it.
            if !stop.load(Ordering::Relaxed) {
                *phase.lock().unwrap() = match end {
                    Ok(()) => RttPhase::Idle,
                    Err(e) => {
                        state
                            .lock()
                            .unwrap()
                            .push_plain(LineKind::Notice, format!("[error] {e}"));
                        RttPhase::Error(e)
                    }
                };
            }
            ctx.request_repaint();
        });
    }
}

/// `cargo build --release` with live console streaming: human progress
/// (stderr) and rendered compile errors go into `state`; the ELF path comes
/// from the JSON `compiler-artifact` messages. The cargo child sits in
/// `child_slot` while running so Stop can kill a long build (the final wait
/// happens on the child taken OUT of the slot — never while holding the lock).
/// Returns `Ok(None)` when the user stopped the build. Shared with the
/// debugger (`crate::debugger`).
pub(crate) fn cargo_build_streamed(
    project_dir: &std::path::Path,
    target: &str,
    state: &Arc<Mutex<TerminalState>>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    stop: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<Option<PathBuf>, String> {
    state.lock().unwrap().push_plain(
        LineKind::Input,
        format!("> cargo build --release --target {target}"),
    );
    ctx.request_repaint();

    let mut cargo = no_window(&mut Command::new("cargo"))
        .current_dir(project_dir)
        .args([
            "build",
            "--release",
            "--target",
            target,
            "--message-format=json",
            "--color=never",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not launch cargo: {e}"))?;

    // Cargo's human progress ("Compiling …") goes to stderr — stream it live.
    let done = Arc::new(AtomicUsize::new(0));
    if let Some(err) = cargo.stderr.take() {
        spawn_reader(
            err,
            LineKind::Stdout, // progress, not errors — keep it calm grey
            Arc::clone(state),
            Arc::clone(stop),
            ctx.clone(),
            Arc::clone(&done),
        );
    }
    let cargo_stdout = cargo.stdout.take();
    *child_slot.lock().unwrap() = Some(cargo);

    // The JSON stream (this thread): ELF path + rendered compile errors.
    let mut elf: Option<PathBuf> = None;
    let mut success = false;
    // Set when the link overflowed the chip's memory — that failure deserves a
    // real explanation, not "fix the errors above".
    let mut flash_note: Option<String> = None;
    if let Some(out) = cargo_stdout {
        for line in std::io::BufReader::new(out).lines().flatten() {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            match v["reason"].as_str() {
                Some("compiler-artifact") => {
                    if let Some(exe) = v["executable"].as_str() {
                        elf = Some(PathBuf::from(exe));
                    }
                }
                Some("compiler-message") => {
                    if v["message"]["level"].as_str() == Some("error") {
                        if let Some(r) = v["message"]["rendered"].as_str() {
                            // A memory.x overflow hides inside the linker dump —
                            // keep the one line that says so for the failure below.
                            if flash_note.is_none() {
                                flash_note = crate::failure_hint::flash_overflow(r);
                            }
                            let mut s = state.lock().unwrap();
                            for l in r.lines() {
                                s.push_plain(LineKind::Stderr, l);
                            }
                        }
                    }
                }
                Some("build-finished") => {
                    success = v["success"].as_bool().unwrap_or(false);
                }
                _ => {}
            }
        }
    }
    // stdout hit EOF → cargo is done (or was killed); reap it outside the lock.
    let cargo_child = child_slot.lock().unwrap().take();
    let cargo_ok = cargo_child
        .map(|mut c| c.wait().ok().is_some_and(|st| st.success()))
        .unwrap_or(false);
    if stop.load(Ordering::Relaxed) {
        return Ok(None);
    }
    if !(success && cargo_ok) {
        if let Some(detail) = flash_note {
            return Err(crate::failure_hint::flash_full_message(&detail));
        }
        return Err("build failed — fix the errors above and try again".into());
    }
    elf.map(Some)
        .ok_or_else(|| "build produced no executable artifact".to_string())
}

/// The tagged explanation for whatever probe-rs failure is visible in `tail` —
/// the console's last lines (`TerminalState::tail_text(60)`) — a crash, or a
/// probe that enumerates but won't open. `None` when the output shows neither,
/// so the caller keeps its own error text.
///
/// Shared with the Debug tab: probe-rs dying of its own bug looks exactly like
/// an ordinary exit / socket close, so both orchestrators have to go looking for
/// the real reason themselves.
///
/// Takes the TEXT, not the console: the open-failure branch runs registry
/// queries, and the UI thread locks that console every frame to draw it. Called
/// with the guard held, it froze the Debug and RTT tabs for the whole query.
pub(crate) fn probe_rs_failure(tail: &str, probe: Option<&str>) -> Option<String> {
    if let Some(detail) = crate::failure_hint::probe_rs_panic(tail) {
        return Some(crate::failure_hint::probe_rs_panic_message(&detail));
    }
    crate::failure_hint::probe_open_failure(tail).map(|d| {
        crate::failure_hint::probe_open_message(
            &d,
            crate::probe::missing_device_interface_guid(probe),
        )
    })
}

/// The orchestrator body: build, then stream probe-rs until it exits.
#[allow(clippy::too_many_arguments)]
/// The exact `probe-rs` argument list - built once, used twice.
///
/// The console echoes this line before spawning, and that echo is what a user
/// copies to reproduce the session by hand. It used to be assembled separately
/// from the real `Command`, two hand-kept constructions of the same thing; when
/// those drift the command still works, it just stops being the one on screen.
///
/// `--probe VID:PID[:Serial]` pins the session to one probe. Absent, probe-rs
/// auto-selects - and errors when several are attached, which is why the tabs
/// share one selector.
fn probe_rs_args(
    sub: &str,
    chip: &str,
    probe: Option<&str>,
    partition_table: bool,
    elf: &std::path::Path,
) -> Vec<String> {
    let mut args = vec![sub.to_owned(), "--chip".to_owned(), chip.to_owned()];
    if let Some(sel) = crate::probe::selector(probe) {
        args.push("--probe".to_owned());
        args.push(sel.to_owned());
    }
    // Only `run` flashes; `attach` writes nothing.
    if partition_table && sub == "run" {
        args.push("--idf-partition-table".to_owned());
        args.push("partitions.csv".to_owned());
    }
    args.push(elf.display().to_string());
    args
}

/// Does a probe-rs session on `chip` from `project_dir` (the build workspace)
/// have to flash the project's own partition table?
///
/// An ESP flashed without it gets probe-rs's default table, in which the flash
/// store sits inside the app partition. The file's presence is the answer
/// because `write_project` - which every RTT / Debug session runs first -
/// deletes a stale copy from the shared build workspace, so it is there
/// exactly when THIS project has a table.
pub(crate) fn idf_partition_table(chip: &str, project_dir: &std::path::Path) -> bool {
    chip.to_ascii_lowercase().starts_with("esp") && project_dir.join("partitions.csv").is_file()
}

fn run_session(
    mode: RttMode,
    project_dir: &std::path::Path,
    target: &str,
    chip: &str,
    probe: Option<&str>,
    state: &Arc<Mutex<TerminalState>>,
    phase: &Arc<Mutex<RttPhase>>,
    child_slot: &Arc<Mutex<Option<Child>>>,
    stop: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<(), String> {
    // ── Phase 1: cargo build --release ────────────────────────────────────────
    let Some(elf) = cargo_build_streamed(project_dir, target, state, child_slot, stop, ctx)? else {
        return Ok(()); // stopped by the user mid-build
    };
    state
        .lock()
        .unwrap()
        .push_plain(LineKind::Notice, "[OK] build — starting probe-rs…");

    // ── Phase 2: probe-rs run/attach ──────────────────────────────────────────
    let sub = match mode {
        RttMode::Run => "run",
        RttMode::Attach => "attach",
    };
    let args = probe_rs_args(
        sub,
        chip,
        probe,
        idf_partition_table(chip, project_dir),
        &elf,
    );
    state
        .lock()
        .unwrap()
        .push_plain(LineKind::Input, format!("> probe-rs {}", args.join(" ")));
    ctx.request_repaint();

    let mut cmd = Command::new("probe-rs");
    let mut probe_rs = no_window(&mut cmd)
        .current_dir(project_dir)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "probe-rs not found in PATH.\n\
                 Install it with:  cargo install probe-rs-tools\n\
                 (or download from https://probe.rs)"
                    .to_string()
            } else {
                format!("could not launch probe-rs: {e}")
            }
        })?;

    *phase.lock().unwrap() = RttPhase::Streaming;
    ctx.request_repaint();

    let done = Arc::new(AtomicUsize::new(0));
    let mut pipes = 0;
    if let Some(out) = probe_rs.stdout.take() {
        pipes += 1;
        spawn_reader(
            out,
            LineKind::Stdout,
            Arc::clone(state),
            Arc::clone(stop),
            ctx.clone(),
            Arc::clone(&done),
        );
    }
    if let Some(err) = probe_rs.stderr.take() {
        pipes += 1;
        spawn_reader(
            err,
            LineKind::Stderr,
            Arc::clone(state),
            Arc::clone(stop),
            ctx.clone(),
            Arc::clone(&done),
        );
    }
    *child_slot.lock().unwrap() = Some(probe_rs);

    // Wait until both pipes are done (probe-rs exited) or the user stopped.
    while done.load(Ordering::Relaxed) < pipes && !stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(30));
    }
    let status = child_slot.lock().unwrap().take().map(|mut c| c.wait());
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    match status {
        Some(Ok(st)) if st.success() => {
            state
                .lock()
                .unwrap()
                .push_plain(LineKind::Notice, "[probe-rs exited]");
            Ok(())
        }
        Some(Ok(st)) => {
            // probe-rs can die of its own panic, or refuse to open a probe it
            // just listed — neither is "check your wiring", so say what it was.
            // The tail is copied out first: the classifier may query the
            // registry, and the RTT tab locks this state every frame.
            let tail = state.lock().unwrap().tail_text(60);
            if let Some(msg) = probe_rs_failure(&tail, probe) {
                return Err(msg);
            }
            Err(format!(
                "probe-rs exited with {} — check the probe connection / chip name",
                st.code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal".into())
            ))
        }
        _ => Err("probe-rs could not be reaped".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::probe_rs_args;
    use std::path::Path;

    /// `run` flashes, so it takes the project's partition table - or probe-rs
    /// writes its default one, with the flash store inside the app partition.
    /// `attach` writes nothing. The answer is the build workspace's file, and
    /// only on an ESP.
    #[test]
    fn run_flashes_the_projects_partition_table_and_attach_does_not() {
        let elf = Path::new("a.elf");
        let run = probe_rs_args("run", "esp32c3", None, true, elf);
        let at = run
            .iter()
            .position(|a| a == "--idf-partition-table")
            .expect("the flag");
        assert_eq!(run[at + 1], "partitions.csv");
        assert_eq!(run.last().map(String::as_str), Some("a.elf"));
        let attach = probe_rs_args("attach", "esp32c3", None, true, elf);
        assert!(!attach.iter().any(|a| a == "--idf-partition-table"));

        let dir = std::env::temp_dir().join(format!("eide_rtt_pt_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!super::idf_partition_table("esp32c3", &dir));
        std::fs::write(
            dir.join("partitions.csv"),
            "nvs, data, nvs, 0x9000, 0x6000\n",
        )
        .unwrap();
        assert!(super::idf_partition_table("esp32c3", &dir));
        assert!(!super::idf_partition_table("STM32F103C8", &dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The line the console prints IS the command that runs. It used to be
    /// assembled twice, and the printed one is what a user copies to reproduce
    /// the session by hand.
    #[test]
    fn the_echoed_line_is_the_command() {
        let elf = Path::new("target/x/release/app");
        let args = probe_rs_args("run", "esp32c6", None, false, elf);
        assert_eq!(args, ["run", "--chip", "esp32c6", "target/x/release/app"]);
        assert_eq!(
            format!("probe-rs {}", args.join(" ")),
            "probe-rs run --chip esp32c6 target/x/release/app"
        );
    }

    /// `--probe` appears only when a selector was picked - probe-rs auto-selects
    /// otherwise, and passing an empty string would be a parse error rather than
    /// the intended "let it choose".
    #[test]
    fn the_probe_selector_is_added_only_when_there_is_one() {
        let elf = Path::new("app.elf");
        for empty in [None, Some(""), Some("   ")] {
            let args = probe_rs_args("attach", "esp32", empty, false, elf);
            assert!(
                !args.iter().any(|a| a == "--probe"),
                "{empty:?} produced {args:?}"
            );
        }
        let args = probe_rs_args("attach", "esp32", Some("303a:1001"), false, elf);
        assert_eq!(
            args,
            [
                "attach",
                "--chip",
                "esp32",
                "--probe",
                "303a:1001",
                "app.elf"
            ]
        );
    }

    /// The ELF is LAST: probe-rs takes it positionally, so a flag appended after
    /// it would be read as a second path.
    #[test]
    fn the_elf_stays_last() {
        let args = probe_rs_args("run", "esp32s3", Some("1"), false, Path::new("a.elf"));
        assert_eq!(args.last().unwrap(), "a.elf");
    }

    /// Every bundled chip produces a well-formed line, so no definition can
    /// carry a chip name that would come out empty or split.
    #[test]
    fn every_bundled_chip_produces_one_chip_argument() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        for d in builtin_definitions() {
            let args = probe_rs_args(
                "run",
                &d.project.probe_chip,
                None,
                false,
                Path::new("a.elf"),
            );
            let i = args.iter().position(|a| a == "--chip").expect("--chip");
            let name = &args[i + 1];
            assert!(!name.trim().is_empty(), "{}: empty chip name", d.id);
            assert!(
                !name.contains(' '),
                "{}: `{name}` would split into two arguments",
                d.id
            );
        }
    }
}
