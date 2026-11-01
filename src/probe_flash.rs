//! Flash the firmware over a debug probe with probe-rs' `cargo flash`, using the
//! SAME probe the Debug / RTT / Runtime(flamegraph) tabs use (`selected_probe`).
//! This is the Flash tab's probe-rs path — one shared probe across all four (the
//! existing DFU / OpenOCD-SWD / espflash paths stay as they are).

use eframe::egui::{Color32, Context};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

/// Status of a `cargo flash` run (own state so it never clashes with the DFU
/// detector or the OpenOCD/SWD flash status).
#[derive(Clone, PartialEq)]
pub enum ProbeFlashState {
    Idle,
    Flashing,
    Success,
    Error(String),
}

impl ProbeFlashState {
    pub fn is_busy(&self) -> bool {
        matches!(self, ProbeFlashState::Flashing)
    }
    pub fn label(&self) -> &str {
        match self {
            ProbeFlashState::Idle => "—",
            ProbeFlashState::Flashing => "Flashing (probe-rs)…",
            ProbeFlashState::Success => "probe-rs Flash OK",
            ProbeFlashState::Error(_) => "probe-rs Error",
        }
    }
    pub fn color(&self) -> Color32 {
        match self {
            ProbeFlashState::Idle => Color32::GRAY,
            ProbeFlashState::Success => Color32::from_rgb(80, 220, 100),
            ProbeFlashState::Error(_) => Color32::from_rgb(230, 80, 60),
            ProbeFlashState::Flashing => Color32::from_rgb(220, 180, 60),
        }
    }
}

/// Stop a running flash. See [`crate::flash_stop`] — the same mechanism serves
/// the OpenOCD and espflash paths.
pub fn stop_probe_flash(child: &crate::flash_stop::FlashHandle, log: &Arc<Mutex<Vec<String>>>) {
    crate::flash_stop::request_stop(child, log, "cargo flash");
}

/// Spawn `cargo flash --release --chip <chip> --target <target> [--probe <sel>]`
/// and stream its output into `log`. `probe` is the exact `VID:PID[:Serial]`
/// selector shared with the other probe-rs tabs (None = let probe-rs pick).
#[allow(clippy::too_many_arguments)]
pub fn start_probe_flash(
    project_dir: PathBuf,
    target: String,
    chip: String,
    probe: Option<String>,
    state: Arc<Mutex<ProbeFlashState>>,
    log: Arc<Mutex<Vec<String>>>,
    // The running child, so the UI can stop it. `cargo flash` can sit for
    // minutes with nothing to show — waiting on an ambiguous probe, on a target
    // that won't halt — and without this the only way out was killing the IDE.
    child: crate::flash_stop::FlashHandle,
    ctx: Context,
    // The Activity tab's log. The other three flash paths - espflash,
    // OpenOCD and DFU - have always recorded themselves; this one did not,
    // so the one operation an STM32 or RP user cares most about was the
    // one the timing breakdown had nothing to say about.
    activity: Arc<Mutex<crate::activity::ActivityLog>>,
) {
    if state.lock().unwrap().is_busy() {
        return;
    }
    *state.lock().unwrap() = ProbeFlashState::Flashing;
    log.lock().unwrap().clear();
    crate::flash_stop::arm(&child);
    ctx.request_repaint();

    thread::spawn(move || {
        // Commits on drop, so the entry appears even when the spawn fails
        // outright (no probe-rs installed) - the same reason the other three
        // paths use it.
        let mut act = crate::activity::Committing::new("Flash (probe-rs)", activity);
        let t_flash = std::time::Instant::now();
        let mut args: Vec<String> = vec![
            "flash".into(),
            "--release".into(),
            "--chip".into(),
            // The definition's name, like the generated runner: cargo-flash
            // need not be the probe-rs `probe::probe_rs_chip` asks, and its
            // final reset holds the nRF5340's network core anyway.
            chip,
            "--target".into(),
            target,
        ];
        if let Some(p) = crate::probe::selector(probe.as_deref()) {
            args.push("--probe".into());
            args.push(p);
        }
        log.lock()
            .unwrap()
            .push(format!("> cargo {}", args.join(" ")));
        ctx.request_repaint();

        let mut command = Command::new("cargo");
        crate::build::no_window(&mut command);
        command
            .current_dir(&project_dir)
            .args(&args)
            // The log renders plain strings: without this, cargo's colours
            // arrive as ANSI escapes and read as garbage around every word.
            // Covers the build `cargo flash` runs internally too.
            .env("CARGO_TERM_COLOR", "never")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut cargo = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                *state.lock().unwrap() = ProbeFlashState::Error(format!(
                    "cannot run `cargo flash`: {e}\n\
                     Install it with: cargo install probe-rs-tools"
                ));
                ctx.request_repaint();
                return;
            }
        };

        // Publish the pid before reading a single line: a flash that hangs does
        // so at the very first step (opening the probe), which is exactly when
        // Stop has to work.
        if !crate::flash_stop::publish(&child, cargo.id()) {
            // Stopped while this one was being spawned — nothing else can reach
            // it, so kill it here.
            crate::lsp::kill_process_tree(cargo.id());
            let _ = cargo.wait();
            *state.lock().unwrap() = ProbeFlashState::Error(crate::flash_stop::STOPPED.into());
            ctx.request_repaint();
            return;
        }

        // Stream stdout on a helper thread so we never block on a full stderr pipe.
        let out_log = Arc::clone(&log);
        let out_ctx = ctx.clone();
        let out_h = cargo.stdout.take().map(|o| {
            thread::spawn(move || {
                for line in BufReader::new(o).lines().map_while(Result::ok) {
                    out_log.lock().unwrap().push(line);
                    out_ctx.request_repaint();
                }
            })
        });
        if let Some(err) = cargo.stderr.take() {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                log.lock().unwrap().push(line);
                ctx.request_repaint();
            }
        }
        if let Some(h) = out_h {
            let _ = h.join();
        }

        let status = cargo.wait();
        let ok = status.as_ref().map(|s| s.success()).unwrap_or(false);
        // The CODE, not just success: "exit 1" and "exit 101" send the
        // reader to different places, and the tab exists for the failures.
        act.rec().cmd_phase(
            "cargo flash",
            format!("cargo {}", args.join(" ")),
            t_flash.elapsed(),
            status.as_ref().ok().and_then(|s| s.code()),
        );
        // Whatever happened, nobody may kill this pid any more — the OS reuses
        // them, and a stale one aimed at a stranger is the worst kind of bug.
        let stopped = crate::flash_stop::finished(&child);
        *state.lock().unwrap() = if ok {
            ProbeFlashState::Success
        } else if stopped {
            // `stop_probe_flash` got here first: this is the user's own doing,
            // not a failure to explain.
            ProbeFlashState::Error(crate::flash_stop::STOPPED.into())
        } else {
            // A probe that won't open (or a probe-rs crash) has an explanation
            // worth more than "see the log above" — the log is where the raw
            // cause is buried.
            let tail = {
                let l = log.lock().unwrap();
                let start = l.len().saturating_sub(60);
                l[start..].join("\n")
            };
            let explained = crate::failure_hint::probe_rs_panic(&tail)
                .map(|d| crate::failure_hint::probe_rs_panic_message(&d))
                .or_else(|| {
                    crate::failure_hint::probe_open_failure(&tail).map(|d| {
                        crate::failure_hint::probe_open_message(
                            &d,
                            crate::probe::missing_device_interface_guid(probe.as_deref()),
                        )
                    })
                });
            ProbeFlashState::Error(
                explained.unwrap_or_else(|| "cargo flash failed — see the log above".into()),
            )
        };
        ctx.request_repaint();
    });
}
