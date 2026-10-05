//! On-target debugger — a DAP client over `probe-rs dap-server` (TCP).
//!
//! probe-rs ships a Debug Adapter Protocol server (the same one its VS Code
//! extension uses). Speaking DAP gets source-level debugging — breakpoints by
//! file:line, step over/in/out, stack traces with source locations, locals and
//! registers — without linking probe-rs as a library dependency.
//!
//! Session pipeline (`Debugger::start`):
//!  1. `cargo build --release` (streamed into the console — shared with RTT).
//!  2. Spawn `probe-rs dap-server --port <free port>`, connect over TCP.
//!  3. DAP handshake: `initialize` → `launch` (flash + reset) → on the
//!     `initialized` event send every breakpoint + `configurationDone`.
//!  4. Event-driven from there: a `stopped` event chains `threads` →
//!     `stackTrace` → `scopes` → `variables`, filling [`DebugState`]; the UI
//!     issues `continue`/`next`/`stepIn`/`stepOut`/`pause` and breakpoint
//!     updates directly over the same socket.
//!
//! Framing is LSP-style `Content-Length: N\r\n\r\n{json}` — see `read_message`.
//! Requests are correlated to responses by `seq` via the `pending` map.

use crate::build::no_window;
use crate::rtt::cargo_build_streamed;
use crate::terminal::{LineKind, TerminalState, spawn_reader};
use eframe::egui;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ── Shared state (UI ↔ reader thread) ─────────────────────────────────────────

#[derive(Clone, Default, PartialEq)]
pub enum DebugPhase {
    #[default]
    Idle,
    /// `cargo build --release` runs.
    Building,
    /// Server spawned; initialize/launch (incl. flashing) in progress.
    Launching,
    /// The target is executing.
    Running,
    /// Halted — the inner string is the DAP stop reason ("breakpoint", "step").
    Stopped(String),
    /// Disconnecting: the server is finishing and letting go of the probe.
    /// A phase of its own because `is_busy` must stay TRUE through it — the
    /// probe is not free yet, and a Flash started here fails to open it.
    Stopping,
    Error(String),
}

/// One stack frame (top-first). `file_rel` is workspace-relative (`src/…`)
/// when the frame's source lives in the generated project; frames without
/// source (HAL internals, asm) keep `None` and are shown greyed.
#[derive(Clone)]
pub struct Frame {
    pub id: i64,
    pub name: String,
    pub file_rel: Option<String>,
    pub line: u32,
}

/// One row of the Locals / Registers panes.
#[derive(Clone)]
pub struct VarRow {
    pub name: String,
    pub value: String,
    pub ty: Option<String>,
}

/// One watch expression + its most recent evaluation against the selected
/// frame. `error` = the last `evaluate` failed (out of scope / unsupported);
/// `value` then holds the reason. Expressions persist across halts; the value
/// refreshes on each stop and on frame selection.
#[derive(Clone)]
pub struct WatchRow {
    pub expr: String,
    pub value: String,
    pub ty: Option<String>,
    pub error: bool,
    /// The last value read LIVE (see [`Debugger::poll_live_watches`]) and when
    /// it last changed. A counter the firmware bumps in its main loop stops
    /// moving the moment that loop is stuck — that "steady for Ns" is the whole
    /// point of watching while the target RUNS.
    pub raw: Option<u64>,
    pub changed_at: Option<std::time::Instant>,
}

impl WatchRow {
    /// A fresh row for `expr`, with no value yet.
    fn new(expr: String) -> Self {
        Self {
            expr,
            value: String::new(),
            ty: None,
            error: false,
            raw: None,
            changed_at: None,
        }
    }
}

/// The current hover-to-evaluate request: the identifier under the pointer and
/// its value once the target answers. `generation` discards stale responses
/// when the pointer moved to another identifier before the reply arrived.
/// `value: None` = still awaiting the `evaluate` response (no tooltip yet).
#[derive(Clone)]
pub struct HoverEval {
    pub generation: u64,
    pub expr: String,
    pub value: Option<String>,
    pub ty: Option<String>,
}

#[derive(Default)]
pub struct DebugState {
    pub phase: DebugPhase,
    pub thread_id: Option<i64>,
    pub stack: Vec<Frame>,
    pub locals: Vec<VarRow>,
    pub registers: Vec<VarRow>,
    /// User watch expressions + their latest values (see [`WatchRow`]). Owned
    /// here (not on `Debugger`) so the reader thread can re-evaluate them on a
    /// halt. NOT cleared when the target runs — only the values go stale.
    pub watches: Vec<WatchRow>,
    /// Hover-to-evaluate for the identifier under the editor pointer (see
    /// [`HoverEval`]); `None` when not hovering an identifier / not halted.
    pub hover: Option<HoverEval>,
    /// Set by the reader when the target halts somewhere navigable; the UI
    /// consumes it (opens the file, scrolls, tints the line).
    pub nav: Option<(String, u32)>,
    /// The frame whose scopes are currently shown (highlighted in the list).
    pub sel_frame: Option<i64>,
    /// What the adapter is doing right now, from its DAP progress events
    /// ("Erasing sectors 40%", "Loading debug info", …). Shown next to the phase
    /// badge, because `launch` — flash + parse the ELF's debug info — is by far
    /// the longest step and otherwise reports nothing at all.
    pub progress: Option<String>,
    /// What probe-rs answered for each requested breakpoint, per file:
    /// `rel path → requested line → status`. Filled from every `setBreakpoints`
    /// response; empty outside a session (nothing has been asked yet).
    pub bp_status: BTreeMap<String, BTreeMap<u32, BpStatus>>,
}

/// probe-rs's verdict on ONE requested breakpoint. A red dot in the gutter only
/// means "the IDE asked for it" — this is whether the target actually got it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BpStatus {
    /// The debugger armed it. `false` = the line has no code of its own (very
    /// common in the optimised `--release` build the Debug tab produces), or the
    /// core ran out of hardware breakpoint comparators.
    pub verified: bool,
    /// Where it ACTUALLY landed, when probe-rs moved it to the nearest line that
    /// has code. `None` when it stayed put.
    pub moved_to: Option<u32>,
    /// probe-rs's own explanation, when the response carried one.
    pub message: Option<String>,
}

// ── Wire (writer half + request bookkeeping) ──────────────────────────────────

/// What a pending request's response should be routed to.
///
/// NOT `Copy` since `Breakpoints` carries the request's path + lines: the DAP
/// response lists results positionally, with no source path of its own, so the
/// question has to travel with the answer.
#[derive(Clone, PartialEq)]
enum Pending {
    Initialize,
    Launch,
    Threads,
    StackTrace,
    Scopes,
    VarsLocals,
    VarsRegisters,
    /// `evaluate` for watch expression `i` (index into `DebugState::watches`).
    Watch(usize),
    /// `evaluate` for a hover tooltip; the `u64` is the hover generation so a
    /// stale reply (pointer already moved on) is dropped.
    Hover(u64),
    /// `readMemory` for live watch row `i` — the one request that answers while
    /// the target is RUNNING (probe-rs reads over the AHB-AP; it only halts
    /// when the core is asleep).
    MemRead(usize),
    /// `setBreakpoints` for one file: `(rel path, the lines we asked for, in
    /// request order)`. The response's array is positional against that list.
    Breakpoints(String, Vec<u32>),
    Other,
}

/// The write half of the DAP socket + seq/pending bookkeeping. Cloned into the
/// reader thread so it can fire follow-up requests (event-driven chains).
#[derive(Clone)]
struct Wire {
    writer: Arc<Mutex<Option<TcpStream>>>,
    seq: Arc<AtomicI64>,
    pending: Arc<Mutex<HashMap<i64, Pending>>>,
}

impl Wire {
    fn request(&self, command: &str, arguments: Value, kind: Pending) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed);
        let msg = json!({
            "seq": seq,
            "type": "request",
            "command": command,
            "arguments": arguments,
        });
        self.pending.lock().unwrap().insert(seq, kind);
        if let Some(stream) = self.writer.lock().unwrap().as_mut() {
            let body = msg.to_string();
            // ONE `write_all` for header + body, never `write!` with a format
            // string: that issues a syscall per piece ("Content-Length: ", the
            // number, "\r\n\r\n", the body), so the header can reach the server
            // split across TCP segments. probe-rs's dap-server reads its two
            // header lines with `read_line` on a NON-blocking socket (0.29's
            // `receive_data`), and a partial line desyncs that state machine for
            // good — the next read returns a bare "\n" and the session dies with
            // `Failed to read content length from header '\n'`.
            let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
            let _ = stream.write_all(frame.as_bytes());
            let _ = stream.flush();
        }
    }
}

/// Read one `Content-Length`-framed DAP message; `None` on EOF / bad frame.
fn read_message(stream: &mut TcpStream) -> Option<Value> {
    // Headers: byte-by-byte until the blank line (no BufReader — a buffered
    // reader would eat bytes of the next message between calls).
    let mut header = Vec::new();
    let mut b = [0u8; 1];
    while !header.ends_with(b"\r\n\r\n") {
        match stream.read(&mut b) {
            Ok(1) => header.push(b[0]),
            _ => return None,
        }
        if header.len() > 4096 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&header);
    let len: usize = text
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())?;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

// ── Session config ────────────────────────────────────────────────────────────

/// Everything the reader thread needs to drive the handshake.
struct SessionCfg {
    project_dir: PathBuf,
    chip: String,
    /// The `--probe VID:PID[:Serial]` selector for the DAP `launch` `probe`
    /// field, or `None` to let probe-rs auto-pick (see [`crate::probe`]).
    probe: Option<String>,
    elf: PathBuf,
    /// Breakpoints at session start (rel path → 1-based lines). Later edits
    /// go over the wire directly (`Debugger::sync_breakpoints`).
    breakpoints: BTreeMap<String, Vec<u32>>,
}

// ── The debugger ──────────────────────────────────────────────────────────────

/// Owned by `AppIde.debugger`. All methods are UI-thread safe; the heavy
/// lifting happens on the orchestrator + reader threads.
pub struct Debugger {
    pub state: Arc<Mutex<DebugState>>,
    /// Build progress + DAP `output` events (defmt/RTT prints land here too).
    pub console: Arc<Mutex<TerminalState>>,
    wire: Wire,
    /// The `probe-rs dap-server` child (killed on Stop / app exit).
    server: Arc<Mutex<Option<Child>>>,
    /// Cargo child during the build phase (killable) — reuses the RTT helper.
    build_child: Arc<Mutex<Option<Child>>>,
    stop: Option<Arc<AtomicBool>>,
    cfg: Arc<Mutex<Option<Arc<SessionCfg>>>>,
    /// Debug-tab pane split boundaries as fractions of the row width (four
    /// separators → five panes: Console | Breakpoints | Call stack | Variables |
    /// Watch). UI-only, single-threaded. Draggable; see
    /// `debug_tab::split_widths`.
    pub pane_splits: [f32; 4],
    /// The Watch pane's "add expression" input text. UI-only.
    pub watch_draft: String,
    /// Watch pane "Live": poll ADDRESS rows out of memory while the target runs
    /// (see [`Debugger::poll_live_watches`]). UI-only, session-persistent.
    pub watch_live: bool,
    /// Rate limit for the poll above.
    last_live_poll: std::time::Instant,
    /// Monotonic generation for hover-evaluate requests (drops stale replies).
    hover_gen: AtomicU64,
}

impl Default for Debugger {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(DebugState::default())),
            console: Arc::new(Mutex::new(TerminalState::default())),
            wire: Wire {
                writer: Arc::new(Mutex::new(None)),
                seq: Arc::new(AtomicI64::new(1)),
                pending: Arc::new(Mutex::new(HashMap::new())),
            },
            server: Arc::new(Mutex::new(None)),
            build_child: Arc::new(Mutex::new(None)),
            stop: None,
            cfg: Arc::new(Mutex::new(None)),
            // Console widest; breakpoints / stack / variables / watch share it.
            pane_splits: [0.28, 0.45, 0.62, 0.81],
            watch_draft: String::new(),
            watch_live: false,
            last_live_poll: std::time::Instant::now(),
            hover_gen: AtomicU64::new(0),
        }
    }
}

impl Debugger {
    pub fn phase(&self) -> DebugPhase {
        self.state.lock().unwrap().phase.clone()
    }

    pub fn is_busy(&self) -> bool {
        !matches!(self.phase(), DebugPhase::Idle | DebugPhase::Error(_))
    }

    /// What the adapter says it is doing right now (DAP progress events), for
    /// the phase badge. `None` outside a reported operation.
    pub fn progress(&self) -> Option<String> {
        self.state.lock().unwrap().progress.clone()
    }

    /// Take the pending halt-location navigation (UI consumes it once).
    pub fn take_nav(&self) -> Option<(String, u32)> {
        self.state.lock().unwrap().nav.take()
    }

    pub fn clear_console(&mut self) {
        self.console.lock().unwrap().lines.clear();
    }

    /// Start a session: build, flash and halt-ready debug. `breakpoints` is
    /// the current rel-path → lines map. No-op while a session is active.
    pub fn start(
        &mut self,
        project_dir: PathBuf,
        target: String,
        chip: String,
        // The `--probe VID:PID[:Serial]` selector, or `None` for auto-select.
        probe: Option<String>,
        breakpoints: BTreeMap<String, Vec<u32>>,
        ctx: egui::Context,
    ) {
        if self.is_busy() {
            return;
        }
        {
            let mut st = self.state.lock().unwrap();
            // Watch EXPRESSIONS survive a restart (user intent); their values
            // reset and refill on the first halt of the new session.
            let watches = st
                .watches
                .iter()
                .map(|w| WatchRow::new(w.expr.clone()))
                .collect();
            *st = DebugState {
                phase: DebugPhase::Building,
                watches,
                ..Default::default()
            };
        }
        self.wire.pending.lock().unwrap().clear();
        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(Arc::clone(&stop));

        let console = Arc::clone(&self.console);
        let state = Arc::clone(&self.state);
        let wire = self.wire.clone();
        let server_slot = Arc::clone(&self.server);
        let build_slot = Arc::clone(&self.build_child);
        let cfg_slot = Arc::clone(&self.cfg);
        thread::spawn(move || {
            let end = run_session(
                &project_dir,
                &target,
                &chip,
                probe,
                breakpoints,
                &console,
                &state,
                &wire,
                &server_slot,
                &build_slot,
                &cfg_slot,
                &stop,
                &ctx,
            );
            if let Err(e) = end {
                if !stop.load(Ordering::Relaxed) {
                    console
                        .lock()
                        .unwrap()
                        .push_plain(LineKind::Notice, format!("[error] {e}"));
                    state.lock().unwrap().phase = DebugPhase::Error(e);
                }
                kill_server(&server_slot);
                *wire.writer.lock().unwrap() = None;
            }
            ctx.request_repaint();
        });
    }

    /// End the session: polite `disconnect`, then wait for the server to let go
    /// of the probe — and only kill it if it won't.
    ///
    /// The phase stays [`DebugPhase::Stopping`] (so `is_busy` is true, so every
    /// flasher stays blocked) until the server process is GONE. It used to flip
    /// to `Idle` immediately while a kill was scheduled 400 ms later: a Flash
    /// clicked in that window found the probe still held, and the kill itself
    /// left the ST-Link in debug mode — "The debug probe could not be opened"
    /// until the user physically replugged it.
    pub fn stop(&mut self, ctx: &egui::Context) {
        // ── Leave the CHIP the way we found it ───────────────────────────────
        // probe-rs's `disconnect` does neither of these (0.29
        // `adapter.rs::disconnect` only halts, and only when the client asks to
        // terminate/suspend). Whatever we leave behind outlives the session:
        // breakpoints live in the core's FPB comparators, and a core halted at
        // one stays halted with nobody to resume it. The next flash then boots
        // into a firmware that freezes on the first armed line — which is
        // exactly what it looks like from the outside.
        if let Some(cfg) = self.cfg.lock().unwrap().clone() {
            let files: std::collections::BTreeSet<String> = {
                let st = self.state.lock().unwrap();
                cfg.breakpoints
                    .keys()
                    .chain(st.bp_status.keys())
                    .cloned()
                    .collect()
            };
            for rel in files {
                send_breakpoints(&self.wire, &cfg.project_dir, &rel, &[]);
            }
        }
        // Resume a halted target, so the firmware runs on after we let go. Sent
        // raw rather than through `continue_run` — the phase is about to become
        // `Stopping`, not `Running`.
        if matches!(self.phase(), DebugPhase::Stopped(_)) {
            self.wire.request(
                "continue",
                json!({"threadId": self.thread_id()}),
                Pending::Other,
            );
        }
        // The reader must stay alive to carry the disconnect handshake; only
        // the shutdown thread flips the flag, once the server is really gone.
        self.wire.request(
            "disconnect",
            json!({"terminateDebuggee": false}),
            Pending::Other,
        );
        if let Some(child) = self.build_child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        {
            let mut st = self.state.lock().unwrap();
            st.phase = DebugPhase::Stopping;
            st.progress = None;
            st.stack.clear();
            st.locals.clear();
            st.registers.clear();
        }
        self.console
            .lock()
            .unwrap()
            .push_plain(LineKind::Notice, "[disconnecting — releasing the probe…]");

        let server = Arc::clone(&self.server);
        let writer = Arc::clone(&self.wire.writer);
        let state = Arc::clone(&self.state);
        let console = Arc::clone(&self.console);
        let stop = self.stop.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let clean = shutdown_server(&server, Duration::from_secs(3));
            // Some ST-Links need a breath after the handle closes before they
            // answer an open again.
            thread::sleep(Duration::from_millis(150));
            if let Some(s) = stop {
                s.store(true, Ordering::Relaxed);
            }
            *writer.lock().unwrap() = None;
            {
                let mut st = state.lock().unwrap();
                st.phase = DebugPhase::Idle;
                st.bp_status.clear();
            }
            console.lock().unwrap().push_plain(
                LineKind::Notice,
                if clean {
                    "[debug session ended — probe released]"
                } else {
                    "[debug session ended — server killed; if the next Flash can't open \
                     the probe, unplug it and plug it back in]"
                },
            );
            ctx.request_repaint();
        });
    }

    /// Synchronous teardown for app exit — an orphaned dap-server would keep
    /// the probe locked for the next start (no polite disconnect, just kill).
    pub fn kill_now(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(child) = self.build_child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        kill_server(&self.server);
        *self.wire.writer.lock().unwrap() = None;
    }

    // ── Execution controls (enabled by the UI per phase) ─────────────────────

    fn thread_id(&self) -> i64 {
        self.state.lock().unwrap().thread_id.unwrap_or(0)
    }

    pub fn continue_run(&self) {
        self.wire.request(
            "continue",
            json!({"threadId": self.thread_id()}),
            Pending::Other,
        );
        self.mark_running();
    }

    pub fn pause(&self) {
        self.wire.request(
            "pause",
            json!({"threadId": self.thread_id()}),
            Pending::Other,
        );
    }

    pub fn step_over(&self) {
        self.wire.request(
            "next",
            json!({"threadId": self.thread_id()}),
            Pending::Other,
        );
        self.mark_running();
    }

    pub fn step_in(&self) {
        self.wire.request(
            "stepIn",
            json!({"threadId": self.thread_id()}),
            Pending::Other,
        );
        self.mark_running();
    }

    pub fn step_out(&self) {
        self.wire.request(
            "stepOut",
            json!({"threadId": self.thread_id()}),
            Pending::Other,
        );
        self.mark_running();
    }

    /// Optimistic phase flip — the next `stopped` event corrects it.
    fn mark_running(&self) {
        let mut st = self.state.lock().unwrap();
        st.phase = DebugPhase::Running;
        st.stack.clear();
        st.locals.clear();
        st.registers.clear();
        st.sel_frame = None;
        st.hover = None;
    }

    /// Show another frame's variables (stack-row click). Also raises the nav
    /// so the editor jumps to that frame's source line.
    pub fn select_frame(&self, frame: &Frame) {
        {
            let mut st = self.state.lock().unwrap();
            st.sel_frame = Some(frame.id);
            if let Some(rel) = &frame.file_rel {
                st.nav = Some((rel.clone(), frame.line));
            }
        }
        self.wire
            .request("scopes", json!({"frameId": frame.id}), Pending::Scopes);
        // The new frame's scope changes what every watch resolves to.
        eval_watches(&self.wire, &self.state, frame.id);
    }

    /// Add a watch expression (from the editor's "Add to Watch" or the Watch
    /// pane's input). Deduplicated; evaluated immediately when halted on a frame.
    pub fn add_watch(&self, expr: String) {
        let expr = expr.trim().to_string();
        if expr.is_empty() {
            return;
        }
        let (idx, frame) = {
            let mut st = self.state.lock().unwrap();
            if st.watches.iter().any(|w| w.expr == expr) {
                return; // already watching it
            }
            st.watches.push(WatchRow::new(expr.clone()));
            (st.watches.len() - 1, st.sel_frame)
        };
        // Evaluate now if a session is halted on a frame; otherwise it fills in
        // at the next stop.
        if let Some(fid) = frame {
            if self.wire.writer.lock().unwrap().is_some() {
                self.wire.request(
                    "evaluate",
                    json!({"expression": expr, "frameId": fid, "context": "watch"}),
                    Pending::Watch(idx),
                );
            }
        }
    }

    /// Re-read every ADDRESS watch straight from memory, whether the target is
    /// halted or running. Call once per frame; it rate-limits itself.
    ///
    /// This is the only way to see a value MOVE: `evaluate` needs a stack frame,
    /// so the ordinary watch path is dead while the firmware runs — which is
    /// exactly when you want to know whether the main loop is still turning.
    /// Point a row at a counter's address and its "steady for Ns" badge is a
    /// hang detector that costs the target nothing but a few bus cycles.
    pub fn poll_live_watches(&mut self) {
        const EVERY: Duration = Duration::from_millis(400);
        if !self.watch_live || self.wire.writer.lock().unwrap().is_none() {
            return;
        }
        if self.last_live_poll.elapsed() < EVERY {
            return;
        }
        self.last_live_poll = std::time::Instant::now();
        let addrs: Vec<(usize, u64)> = {
            let st = self.state.lock().unwrap();
            st.watches
                .iter()
                .enumerate()
                .filter_map(|(i, w)| parse_addr(&w.expr).map(|a| (i, a)))
                .collect()
        };
        for (i, addr) in addrs {
            self.wire.request(
                "readMemory",
                json!({"memoryReference": format!("0x{addr:X}"), "count": 4}),
                Pending::MemRead(i),
            );
        }
    }

    pub fn remove_watch(&self, i: usize) {
        let mut st = self.state.lock().unwrap();
        if i < st.watches.len() {
            st.watches.remove(i);
        }
    }

    /// Evaluate `expr` for a hover tooltip (`context:"hover"`) against the
    /// selected frame. Debounced: a no-op while the same expression is already
    /// the current hover, so at most one request fires per identifier hovered.
    /// Only meaningful while halted on a frame with a live session.
    pub fn hover_eval(&self, expr: String) {
        let expr = expr.trim().to_string();
        if expr.is_empty() {
            self.clear_hover();
            return;
        }
        let fid = {
            let st = self.state.lock().unwrap();
            if !matches!(st.phase, DebugPhase::Stopped(_)) {
                return;
            }
            // Same expression already shown / in flight → nothing to do.
            if st.hover.as_ref().map(|h| h.expr.as_str()) == Some(expr.as_str()) {
                return;
            }
            st.sel_frame
        };
        let Some(fid) = fid else {
            return;
        };
        if self.wire.writer.lock().unwrap().is_none() {
            return;
        }
        let generation = self.hover_gen.fetch_add(1, Ordering::Relaxed) + 1;
        self.state.lock().unwrap().hover = Some(HoverEval {
            generation,
            expr: expr.clone(),
            value: None,
            ty: None,
        });
        self.wire.request(
            "evaluate",
            json!({"expression": expr, "frameId": fid, "context": "hover"}),
            Pending::Hover(generation),
        );
    }

    /// Drop any hover tooltip (pointer left an identifier / the editor, or the
    /// session isn't halted).
    pub fn clear_hover(&self) {
        let mut st = self.state.lock().unwrap();
        if st.hover.is_some() {
            st.hover = None;
        }
    }

    /// Push the current breakpoint set of one file to the live session (call
    /// on every gutter toggle; no-op when no session is up).
    pub fn sync_breakpoints(&self, rel_path: &str, lines: &[u32]) {
        let Some(cfg) = self.cfg.lock().unwrap().clone() else {
            return;
        };
        if self.wire.writer.lock().unwrap().is_none() {
            return;
        }
        send_breakpoints(&self.wire, &cfg.project_dir, rel_path, lines);
    }
}

/// probe-rs answers an unresolved `evaluate` with a `success:true` result whose
/// text is a placeholder marker — `<invalid expression "x">` (name isn't a
/// register / in-scope local / static / SVD peripheral on THIS probe-rs),
/// `<not found …>`, or `<optimized out>` (release build dropped it). Detect
/// those so the UI can grey them out with a short reason instead of showing the
/// raw marker. Returns `(display_text, is_unresolved)`.
fn classify_eval_result(value: &str) -> (String, bool) {
    let v = value.trim();
    if v.starts_with("<invalid expression") {
        ("not in scope / unsupported".to_string(), true)
    } else if v.starts_with("<not found") {
        ("not found in this frame".to_string(), true)
    } else if v.contains("optimized out") {
        ("optimized out".to_string(), true)
    } else {
        (value.to_string(), false)
    }
}

/// Fire a DAP `evaluate` (context "watch") for every watch expression against
/// `frame_id`; each response routes to `Pending::Watch(i)` → fills
/// `DebugState::watches[i]`. Called on every halt and on frame selection.
fn eval_watches(wire: &Wire, state: &Arc<Mutex<DebugState>>, frame_id: i64) {
    let exprs: Vec<String> = state
        .lock()
        .unwrap()
        .watches
        .iter()
        .map(|w| w.expr.clone())
        .collect();
    for (i, expr) in exprs.iter().enumerate() {
        wire.request(
            "evaluate",
            json!({"expression": expr, "frameId": frame_id, "context": "watch"}),
            Pending::Watch(i),
        );
    }
}

/// `setBreakpoints` for one source file (abs path = workspace + rel).
fn send_breakpoints(wire: &Wire, project_dir: &Path, rel_path: &str, lines: &[u32]) {
    let abs = project_dir.join(rel_path);
    let bps: Vec<Value> = lines.iter().map(|l| json!({"line": l})).collect();
    wire.request(
        "setBreakpoints",
        json!({
            "source": { "path": abs.to_string_lossy() },
            "breakpoints": bps,
        }),
        Pending::Breakpoints(rel_path.to_owned(), lines.to_vec()),
    );
}

/// Wait for the dap-server to exit on its own (it does, after a `disconnect`,
/// because we start it with `--single-session`), killing it only if it outstays
/// `grace`. Returns true when it left by itself — the case where probe-rs ran
/// its own shutdown and DETACHED the probe. A kill skips all of that, which is
/// what used to leave the ST-Link wedged until a replug.
fn shutdown_server(server: &Arc<Mutex<Option<Child>>>, grace: Duration) -> bool {
    let Some(mut child) = server.lock().unwrap().take() else {
        return true; // nothing running
    };
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => thread::sleep(Duration::from_millis(40)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

fn kill_server(server: &Arc<Mutex<Option<Child>>>) {
    if let Some(mut child) = server.lock().unwrap().take() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

// ── Orchestrator ──────────────────────────────────────────────────────────────

/// Build, spawn the server, connect, start the reader, send `initialize`.
#[allow(clippy::too_many_arguments)]
fn run_session(
    project_dir: &Path,
    target: &str,
    chip: &str,
    probe: Option<String>,
    breakpoints: BTreeMap<String, Vec<u32>>,
    console: &Arc<Mutex<TerminalState>>,
    state: &Arc<Mutex<DebugState>>,
    wire: &Wire,
    server_slot: &Arc<Mutex<Option<Child>>>,
    build_slot: &Arc<Mutex<Option<Child>>>,
    cfg_slot: &Arc<Mutex<Option<Arc<SessionCfg>>>>,
    stop: &Arc<AtomicBool>,
    ctx: &egui::Context,
) -> Result<(), String> {
    // ── 1. Build ──────────────────────────────────────────────────────────────
    let Some(elf) = cargo_build_streamed(project_dir, target, console, build_slot, stop, ctx)?
    else {
        return Ok(()); // user stopped mid-build
    };
    state.lock().unwrap().phase = DebugPhase::Launching;

    // ── 2. Server ─────────────────────────────────────────────────────────────
    let port = free_port();
    console.lock().unwrap().push_plain(
        LineKind::Input,
        format!("> probe-rs dap-server --port {port}"),
    );
    ctx.request_repaint();
    let mut server = no_window(&mut Command::new("probe-rs"))
        .current_dir(project_dir)
        // `--single-session` (the flag its VS Code extension uses): the server
        // finishes after OUR session and exits, instead of going back to
        // listening. That exit is what releases the probe cleanly — with the
        // server still alive we had to kill it, and a killed probe-rs never
        // detaches, leaving the ST-Link in debug mode until it is replugged.
        .args([
            "dap-server",
            "--single-session",
            "--port",
            &port.to_string(),
        ])
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
                format!("could not launch probe-rs dap-server: {e}")
            }
        })?;
    // The server's own log lines → console (its errors are the best clue when
    // a probe / chip problem aborts the session).
    let done = Arc::new(AtomicUsize::new(0));
    if let Some(out) = server.stdout.take() {
        spawn_reader(
            out,
            LineKind::Notice,
            Arc::clone(console),
            Arc::clone(stop),
            ctx.clone(),
            Arc::clone(&done),
        );
    }
    if let Some(err) = server.stderr.take() {
        spawn_reader(
            err,
            LineKind::Stderr,
            Arc::clone(console),
            Arc::clone(stop),
            ctx.clone(),
            Arc::clone(&done),
        );
    }
    *server_slot.lock().unwrap() = Some(server);

    // ── 3. Connect (the server needs a moment to listen) ──────────────────────
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut socket = None;
    for _ in 0..50 {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            Ok(s) => {
                socket = Some(s);
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(100)),
        }
    }
    let socket = socket.ok_or("could not connect to probe-rs dap-server (port timeout)")?;
    let read_half = socket
        .try_clone()
        .map_err(|e| format!("socket clone failed: {e}"))?;
    *wire.writer.lock().unwrap() = Some(socket);

    let cfg = Arc::new(SessionCfg {
        project_dir: project_dir.to_path_buf(),
        chip: chip.to_string(),
        probe: crate::probe::selector(probe.as_deref()),
        elf,
        breakpoints,
    });
    *cfg_slot.lock().unwrap() = Some(Arc::clone(&cfg));

    // Bounds the one step that can hang silently (see the fn's note).
    spawn_launch_watchdog(
        Arc::clone(state),
        Arc::clone(console),
        Arc::clone(stop),
        cfg.elf.clone(),
        ctx.clone(),
    );

    // ── 4. Reader thread drives the handshake from here ──────────────────────
    spawn_dap_reader(
        read_half,
        cfg,
        Arc::clone(state),
        Arc::clone(console),
        wire.clone(),
        Arc::clone(server_slot),
        Arc::clone(stop),
        ctx.clone(),
    );
    wire.request(
        "initialize",
        json!({
            "clientID": env!("CARGO_PKG_NAME"),
            "clientName": crate::names::APP_DISPLAY_NAME,
            "adapterID": "probe-rs",
            "linesStartAt1": true,
            "columnsStartAt1": true,
            "pathFormat": "path",
            // Ask for progress events. probe-rs only sends `progressStart` /
            // `progressUpdate` / `progressEnd` when the CLIENT advertises this,
            // and they are the only detail there is while `launch` runs: its
            // own log goes to the "Debug Console" (DAP `output` events) and
            // stays silent at the default `probe_rs=warn`. Without this the
            // whole flash + debug-info load is a spinner and nothing else.
            "supportsProgressReporting": true,
        }),
        Pending::Initialize,
    );
    Ok(())
}

/// Warn, then give up, on a `launch` that never answers.
///
/// `launch` (flash + load debug info) is the one step that can take minutes and
/// reports nothing on failure: a wedged probe accepts the TCP connection and
/// then goes silent, which is indistinguishable from a slow start. Without this
/// the tab sits on "flashing + attaching…" forever — the worst failure mode
/// there is, because it never tells the user whether waiting is still worth it.
fn spawn_launch_watchdog(
    state: Arc<Mutex<DebugState>>,
    console: Arc<Mutex<TerminalState>>,
    stop: Arc<AtomicBool>,
    elf: PathBuf,
    ctx: egui::Context,
) {
    /// How long before saying "still working"; how long before calling it dead.
    const NOTE_AFTER: Duration = Duration::from_secs(20);
    const FAIL_AFTER: Duration = Duration::from_secs(120);

    thread::spawn(move || {
        let started = std::time::Instant::now();
        let mut noted = false;
        loop {
            thread::sleep(Duration::from_millis(500));
            if stop.load(Ordering::Relaxed) {
                return;
            }
            // Only watches the launch window; any other phase means it answered.
            if !matches!(state.lock().unwrap().phase, DebugPhase::Launching) {
                return;
            }
            let waited = started.elapsed();
            if !noted && waited >= NOTE_AFTER {
                noted = true;
                console.lock().unwrap().push_plain(
                    LineKind::Notice,
                    format!(
                        "[still launching after {}s — probe-rs is flashing the chip and loading \
                         the ELF's debug info; it reports nothing while it works]",
                        waited.as_secs()
                    ),
                );
                ctx.request_repaint();
            }
            if waited >= FAIL_AFTER {
                let mb = std::fs::metadata(&elf)
                    .map(|m| m.len() as f64 / (1024.0 * 1024.0))
                    .unwrap_or(0.0);
                let msg = crate::failure_hint::launch_stalled_message(waited.as_secs(), mb);
                let mut st = state.lock().unwrap();
                st.progress = None;
                st.phase = DebugPhase::Error(msg);
                drop(st);
                console.lock().unwrap().push_plain(
                    LineKind::Stderr,
                    "[giving up on launch — the adapter never answered]",
                );
                ctx.request_repaint();
                return;
            }
        }
    });
}

/// An unused localhost port (bind to 0, read back, release).
fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(50_999)
}

// ── Reader (event loop) ───────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn spawn_dap_reader(
    mut socket: TcpStream,
    cfg: Arc<SessionCfg>,
    state: Arc<Mutex<DebugState>>,
    console: Arc<Mutex<TerminalState>>,
    wire: Wire,
    server_slot: Arc<Mutex<Option<Child>>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
) {
    thread::spawn(move || {
        while let Some(msg) = read_message(&mut socket) {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match msg["type"].as_str() {
                Some("response") => {
                    handle_response(&msg, &cfg, &state, &console, &wire);
                }
                Some("event") => {
                    handle_event(&msg, &cfg, &state, &console, &wire);
                }
                _ => {}
            }
            ctx.request_repaint();
        }
        // Socket closed: session over (server exit / disconnect / error).
        if !stop.load(Ordering::Relaxed) {
            // A dap-server that PANICKED — or that refused to open the probe —
            // closes the socket exactly like a clean disconnect does. So before
            // calling it "ended", look for the real failure in what the server
            // printed; otherwise the user gets a calm "[debug session ended]"
            // after a wall of `<unknown>` frames or a WinUSB complaint.
            // Copy the tail, THEN classify: the classifier may query the registry,
            // and the Debug tab locks this console every frame to draw it.
            let tail = console.lock().unwrap().tail_text(60);
            let crash = crate::rtt::probe_rs_failure(&tail, cfg.probe.as_deref());
            let mut st = state.lock().unwrap();
            // `Stopping` is excluded: the socket closing is EXPECTED there (the
            // single-session server exits after our disconnect), and `stop`'s
            // own thread owns the ending — it must not be declared over until
            // that thread has seen the process go, or the Flash buttons unblock
            // while the probe is still held.
            if !matches!(
                st.phase,
                DebugPhase::Error(_) | DebugPhase::Idle | DebugPhase::Stopping
            ) {
                match crash {
                    Some(msg) => {
                        st.phase = DebugPhase::Error(msg);
                        console.lock().unwrap().push_plain(
                            LineKind::Stderr,
                            "[probe-rs failed — the session died with it]",
                        );
                    }
                    None => {
                        st.phase = DebugPhase::Idle;
                        console
                            .lock()
                            .unwrap()
                            .push_plain(LineKind::Notice, "[debug session ended]");
                    }
                }
            }
        }
        // The verdicts describe the session that just ended — keeping them would
        // mark rows "not armed" while nothing is even attached.
        state.lock().unwrap().bp_status.clear();
        kill_server(&server_slot);
        *wire.writer.lock().unwrap() = None;
        ctx.request_repaint();
    });
}

/// The human-readable reason a failed DAP response carries.
///
/// probe-rs sets `message` to the literal **"cancelled"** on EVERY failure — it
/// is a predefined token from the DAP spec, not a description — and puts the
/// real sentence in `body.error.format`. That field is itself a TEMPLATE whose
/// `{placeholders}` are filled from `body.error.variables`, and probe-rs uses
/// exactly one: `format: "{response_message}"`. So reading either `message` or
/// a bare `format` gave the user "cancelled" or "{response_message}" — the
/// actual cause ("Multiple probes were found", "no probes were found", "the
/// target is not responding") was on the wire the whole time and thrown away.
pub(crate) fn response_error(msg: &Value) -> String {
    let err = &msg["body"]["error"];
    if let Some(fmt) = err["format"].as_str() {
        let mut out = fmt.to_owned();
        if let Some(vars) = err["variables"].as_object() {
            for (name, value) in vars {
                if let Some(value) = value.as_str() {
                    out = out.replace(&format!("{{{name}}}"), value);
                }
            }
        }
        let out = out.trim();
        // Only if a placeholder was left unfilled is this worse than `message`.
        if !out.is_empty() && !out.starts_with('{') {
            return out.to_owned();
        }
    }
    match msg["message"].as_str() {
        // "cancelled" is the placeholder, never the reason - saying it back to
        // the user reads as "you cancelled this", which nobody did.
        Some(m) if !m.is_empty() && m != "cancelled" => m.to_owned(),
        _ => format!(
            "{} failed without a reason (see the log)",
            msg["command"].as_str().unwrap_or("request")
        ),
    }
}

fn handle_response(
    msg: &Value,
    cfg: &Arc<SessionCfg>,
    state: &Arc<Mutex<DebugState>>,
    console: &Arc<Mutex<TerminalState>>,
    wire: &Wire,
) {
    let req_seq = msg["request_seq"].as_i64().unwrap_or(-1);
    let kind = wire
        .pending
        .lock()
        .unwrap()
        .remove(&req_seq)
        .unwrap_or(Pending::Other);
    let ok = msg["success"].as_bool().unwrap_or(false);

    if !ok {
        let err = response_error(msg);
        // A watch that can't be resolved (out of scope, unsupported expression)
        // is normal — show it on the row, don't spam the console.
        if let Pending::Watch(i) = kind {
            let mut st = state.lock().unwrap();
            if let Some(row) = st.watches.get_mut(i) {
                row.value = err;
                row.ty = None;
                row.error = true;
            }
            return;
        }
        // A hover over a non-evaluable token just shows no tooltip.
        if let Pending::Hover(g) = kind {
            let mut st = state.lock().unwrap();
            if st.hover.as_ref().map(|h| h.generation) == Some(g) {
                st.hover = None;
            }
            return;
        }
        console
            .lock()
            .unwrap()
            .push_plain(LineKind::Stderr, format!("[dap] {err}"));
        // A failed launch is fatal; anything else just logs.
        if kind == Pending::Launch {
            // The DAP layer only says "cancelled" — the REASON (a probe that
            // won't open, a probe-rs crash) is in the server's own output that
            // came before it.
            // The lock is released before classifying, as above.
            let tail = console.lock().unwrap().tail_text(60);
            let real = crate::rtt::probe_rs_failure(&tail, cfg.probe.as_deref());
            state.lock().unwrap().phase = DebugPhase::Error(real.unwrap_or(err));
        }
        return;
    }

    match kind {
        Pending::Initialize => {
            // Capabilities received → launch (flash + reset the target).
            // probe-rs's DAP `launch` accepts an optional `probe` selector
            // (VID:PID[:Serial]); omit the key entirely to keep auto-select.
            let mut launch = json!({
                "cwd": cfg.project_dir.to_string_lossy(),
                "chip": cfg.chip,
                "connectUnderReset": false,
                "flashingConfig": {
                    "flashingEnabled": true,
                    "haltAfterReset": false,
                },
                "coreConfigs": [{
                    "coreIndex": 0,
                    "programBinary": cfg.elf.to_string_lossy(),
                    "rttEnabled": true,
                }],
                "consoleLogLevel": "Console",
            });
            if let Some(sel) = &cfg.probe {
                launch["probe"] = json!(sel);
            }
            // An ESP flashed without its partition table gets probe-rs's default
            // one, in which the flash store sits inside the app partition. The
            // build workspace holds the table exactly when THIS project has one
            // (`write_project` deletes a stale copy there). Key shape read from
            // probe-rs 0.29.0: `FlashingConfig.format_options` (camelCase) ->
            // `FormatOptions.idf_options.idf_partition_table`.
            let table = cfg.project_dir.join("partitions.csv");
            if crate::rtt::idf_partition_table(&cfg.chip, &cfg.project_dir) {
                launch["flashingConfig"]["formatOptions"] = json!({
                    "idf_options": { "idf_partition_table": table.to_string_lossy() }
                });
            }
            wire.request("launch", launch, Pending::Launch);
        }
        Pending::Launch => {
            let mut st = state.lock().unwrap();
            if !matches!(st.phase, DebugPhase::Stopped(_)) {
                st.phase = DebugPhase::Running;
            }
            console
                .lock()
                .unwrap()
                .push_plain(LineKind::Notice, "[launched — target running]");
        }
        Pending::Threads => {
            let tid = msg["body"]["threads"]
                .as_array()
                .and_then(|t| t.first())
                .and_then(|t| t["id"].as_i64())
                .unwrap_or(0);
            state.lock().unwrap().thread_id = Some(tid);
            wire.request(
                "stackTrace",
                json!({"threadId": tid, "startFrame": 0, "levels": 24}),
                Pending::StackTrace,
            );
        }
        Pending::StackTrace => {
            let frames: Vec<Frame> = msg["body"]["stackFrames"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|f| Frame {
                            id: f["id"].as_i64().unwrap_or(0),
                            name: f["name"].as_str().unwrap_or("?").to_string(),
                            file_rel: f["source"]["path"]
                                .as_str()
                                .and_then(|p| rel_of(p, &cfg.project_dir)),
                            line: f["line"].as_u64().unwrap_or(0) as u32,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let top = frames.first().cloned();
            {
                let mut st = state.lock().unwrap();
                // Navigate to the topmost frame that has source in the project.
                if let Some(f) = frames.iter().find(|f| f.file_rel.is_some()) {
                    st.nav = Some((f.file_rel.clone().unwrap(), f.line));
                }
                st.sel_frame = top.as_ref().map(|f| f.id);
                st.stack = frames;
            }
            if let Some(f) = top {
                wire.request("scopes", json!({"frameId": f.id}), Pending::Scopes);
                // Refresh every watch against the (new) top frame on each halt.
                eval_watches(wire, state, f.id);
            }
        }
        Pending::Scopes => {
            if let Some(scopes) = msg["body"]["scopes"].as_array() {
                for s in scopes {
                    let name = s["name"].as_str().unwrap_or("");
                    let vref = s["variablesReference"].as_i64().unwrap_or(0);
                    if vref <= 0 {
                        continue;
                    }
                    let kind = if name.to_lowercase().contains("register") {
                        Pending::VarsRegisters
                    } else if name.to_lowercase().contains("local") {
                        Pending::VarsLocals
                    } else {
                        continue; // statics: skipped (often huge)
                    };
                    wire.request("variables", json!({"variablesReference": vref}), kind);
                }
            }
        }
        Pending::VarsLocals | Pending::VarsRegisters => {
            let rows: Vec<VarRow> = msg["body"]["variables"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .map(|v| VarRow {
                            name: v["name"].as_str().unwrap_or("?").to_string(),
                            value: v["value"].as_str().unwrap_or("").to_string(),
                            ty: v["type"].as_str().map(str::to_owned),
                        })
                        .collect()
                })
                .unwrap_or_default();
            let mut st = state.lock().unwrap();
            if kind == Pending::VarsLocals {
                st.locals = rows;
            } else {
                st.registers = rows;
            }
        }
        Pending::Watch(i) => {
            let (value, unresolved) =
                classify_eval_result(msg["body"]["result"].as_str().unwrap_or(""));
            // A placeholder marker has no meaningful type.
            let ty = if unresolved {
                None
            } else {
                msg["body"]["type"].as_str().map(str::to_owned)
            };
            let mut st = state.lock().unwrap();
            if let Some(row) = st.watches.get_mut(i) {
                row.value = value;
                row.ty = ty;
                row.error = unresolved;
            }
        }
        Pending::Hover(g) => {
            let (value, unresolved) =
                classify_eval_result(msg["body"]["result"].as_str().unwrap_or(""));
            let ty = msg["body"]["type"].as_str().map(str::to_owned);
            let mut st = state.lock().unwrap();
            // Only the current hover generation, and never show a tooltip for an
            // unresolved value (hover is for a quick peek at REAL values).
            if st.hover.as_ref().map(|h| h.generation) == Some(g) {
                if unresolved {
                    st.hover = None;
                } else if let Some(h) = &mut st.hover {
                    h.value = Some(value);
                    h.ty = ty;
                }
            }
        }
        Pending::MemRead(i) => {
            // DAP ships memory base64-encoded, little-endian on our targets.
            let Some(bytes) = msg["body"]["data"].as_str().and_then(base64_decode) else {
                return;
            };
            let mut raw: u64 = 0;
            for (k, b) in bytes.iter().take(8).enumerate() {
                raw |= (*b as u64) << (8 * k);
            }
            let mut st = state.lock().unwrap();
            if let Some(row) = st.watches.get_mut(i) {
                // The CHANGE is the signal, so only a different value restarts
                // the clock — a steady counter means a stopped loop.
                if row.raw != Some(raw) {
                    row.changed_at = Some(std::time::Instant::now());
                }
                row.raw = Some(raw);
                row.value = format!("0x{raw:08X}  ({raw})");
                row.ty = Some(format!("{} bytes @ live read", bytes.len()));
                row.error = false;
            }
        }
        Pending::Breakpoints(rel, asked) => {
            let answers = msg["body"]["breakpoints"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let statuses = bp_statuses(&asked, &answers);
            let unarmed: Vec<u32> = statuses
                .iter()
                .filter(|(_, s)| !s.verified)
                .map(|(l, _)| *l)
                .collect();
            // One console line when something did NOT take — otherwise the red
            // dot is the only feedback and it lies (see the Debug tab's list).
            if !unarmed.is_empty() {
                let list = unarmed
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                console.lock().unwrap().push_plain(
                    LineKind::Stderr,
                    format!(
                        "[breakpoints] {rel}: {} of {} armed — line(s) {list} could not be set \
                         (no code there in the optimised --release build, or the core is out of \
                         hardware breakpoints)",
                        asked.len() - unarmed.len(),
                        asked.len()
                    ),
                );
            }
            // This response is the whole truth for that FILE: replace its map so
            // a removed breakpoint can't leave a stale row behind.
            state.lock().unwrap().bp_status.insert(rel, statuses);
        }
        Pending::Other => {}
    }
}

/// Zip the lines we asked for with the DAP `setBreakpoints` answers, which come
/// back positionally (the response carries no line of its own for a breakpoint
/// the debugger refused). A missing answer counts as unverified — silence is
/// not confirmation.
fn bp_statuses(asked: &[u32], answers: &[Value]) -> BTreeMap<u32, BpStatus> {
    asked
        .iter()
        .enumerate()
        .map(|(i, &line)| {
            let a = answers.get(i);
            (
                line,
                BpStatus {
                    verified: a.and_then(|a| a["verified"].as_bool()).unwrap_or(false),
                    // Only a DIFFERENT line is a relocation worth showing.
                    moved_to: a
                        .and_then(|a| a["line"].as_u64())
                        .map(|l| l as u32)
                        .filter(|l| *l != line),
                    message: a
                        .and_then(|a| a["message"].as_str())
                        .filter(|m| !m.trim().is_empty())
                        .map(str::to_owned),
                },
            )
        })
        .collect()
}

fn handle_event(
    msg: &Value,
    cfg: &Arc<SessionCfg>,
    state: &Arc<Mutex<DebugState>>,
    console: &Arc<Mutex<TerminalState>>,
    wire: &Wire,
) {
    match msg["event"].as_str() {
        Some("initialized") => {
            // Configuration window: breakpoints first, then configurationDone.
            for (rel, lines) in &cfg.breakpoints {
                if !lines.is_empty() {
                    send_breakpoints(wire, &cfg.project_dir, rel, lines);
                }
            }
            wire.request("configurationDone", json!({}), Pending::Other);
        }
        Some("stopped") => {
            let reason = msg["body"]["reason"].as_str().unwrap_or("stopped");
            {
                let mut st = state.lock().unwrap();
                st.phase = DebugPhase::Stopped(reason.to_string());
                if let Some(tid) = msg["body"]["threadId"].as_i64() {
                    st.thread_id = Some(tid);
                }
            }
            wire.request("threads", json!({}), Pending::Threads);
        }
        Some("continued") => {
            let mut st = state.lock().unwrap();
            st.phase = DebugPhase::Running;
            st.stack.clear();
            st.locals.clear();
            st.registers.clear();
            st.sel_frame = None;
            st.hover = None;
        }
        Some("output") => {
            let text = msg["body"]["output"].as_str().unwrap_or("");
            let kind = match msg["body"]["category"].as_str() {
                Some("stderr") => LineKind::Stderr,
                Some("console") => LineKind::Notice,
                _ => LineKind::Stdout, // stdout / RTT prints
            };
            let mut c = console.lock().unwrap();
            for line in text.lines().filter(|l| !l.is_empty()) {
                c.push_plain(kind, line);
            }
        }
        // ── Progress (flashing, loading debug info) ──────────────────────────
        // The only running commentary `launch` produces. Start and end go to the
        // console; the updates only refresh the badge, since probe-rs sends one
        // per percent and a console line each would bury everything else.
        Some("progressStart") => {
            let title = msg["body"]["title"].as_str().unwrap_or("working");
            state.lock().unwrap().progress = Some(title.to_owned());
            console
                .lock()
                .unwrap()
                .push_plain(LineKind::Notice, format!("[{title}…]"));
        }
        Some("progressUpdate") => {
            let text = progress_text(msg);
            if let Some(t) = text {
                state.lock().unwrap().progress = Some(t);
            }
        }
        Some("progressEnd") => {
            let mut st = state.lock().unwrap();
            let done = st.progress.take();
            drop(st);
            if let Some(d) = done {
                let msg_text = msg["body"]["message"].as_str().unwrap_or("");
                let line = if msg_text.is_empty() {
                    format!("[{} — done]", d.split(" · ").next().unwrap_or(&d))
                } else {
                    format!("[{msg_text}]")
                };
                console.lock().unwrap().push_plain(LineKind::Notice, line);
            }
        }
        Some("terminated") | Some("exited") => {
            let mut st = state.lock().unwrap();
            if !matches!(st.phase, DebugPhase::Error(_)) {
                st.phase = DebugPhase::Idle;
            }
            st.progress = None;
            drop(st);
            console
                .lock()
                .unwrap()
                .push_plain(LineKind::Notice, "[target terminated]");
        }
        _ => {}
    }
}

/// A watch expression that is a plain ADDRESS (`0x2000_0004`) — the only kind
/// that can be read while the target runs, since there is no frame to resolve a
/// name against. Underscores are allowed, the way they are written in code.
fn parse_addr(expr: &str) -> Option<u64> {
    let t = expr.trim().replace('_', "");
    let hex = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"))?;
    u64::from_str_radix(hex, 16).ok()
}

/// Decode standard base64 (the encoding DAP uses for `readMemory` data).
/// `None` on any character outside the alphabet — a malformed body must not
/// silently produce a plausible-looking value.
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a') as u32 + 26,
            b'0'..=b'9' => (c - b'0') as u32 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        if c == b'=' {
            break; // padding: whatever is buffered is incomplete, drop it
        }
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `"Erasing sectors · 42%"` from a DAP `progressUpdate` body — `None` when it
/// carries neither a message nor a percentage (nothing to show, keep the last).
fn progress_text(msg: &Value) -> Option<String> {
    let message = msg["body"]["message"].as_str().filter(|m| !m.is_empty());
    let pct = msg["body"]["percentage"].as_f64();
    match (message, pct) {
        (Some(m), Some(p)) => Some(format!("{m} · {p:.0}%")),
        (Some(m), None) => Some(m.to_owned()),
        (None, Some(p)) => Some(format!("{p:.0}%")),
        (None, None) => None,
    }
}

/// Map an absolute DWARF/DAP source path back to a workspace-relative one
/// (`src/main.rs`). Case-insensitive on the prefix — Windows reports the same
/// dir as `C:\Users\…\Temp` or `C:\Users\…\temp` depending on the producer.
fn rel_of(path: &str, project_dir: &Path) -> Option<String> {
    let norm = path.replace('\\', "/");
    let prefix = project_dir.to_string_lossy().replace('\\', "/");
    if norm.len() > prefix.len() && norm[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        Some(norm[prefix.len()..].trim_start_matches('/').to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape probe-rs sends on a failure: `message` is the DAP token
    /// "cancelled", and the sentence is a `{response_message}` template filled
    /// from `variables`. Reading either field raw is what turned every failed
    /// attach into the word "cancelled".
    #[test]
    fn a_failed_response_reports_probe_rs_own_sentence() {
        let msg = json!({
            "type": "response", "command": "attach", "success": false,
            "message": "cancelled",
            "body": { "error": {
                "id": 0,
                "format": "{response_message}",
                "variables": { "response_message":
                    "Multiple probes were found. Please specify one with the --probe argument." },
                "showUser": true,
            }},
        });
        assert_eq!(
            response_error(&msg),
            "Multiple probes were found. Please specify one with the --probe argument."
        );
    }

    /// A `format` with no placeholders is already the message.
    #[test]
    fn a_plain_format_is_used_as_it_stands() {
        let msg = json!({
            "type": "response", "command": "launch", "success": false,
            "message": "cancelled",
            "body": { "error": { "format": "The debug probe could not be opened." } },
        });
        assert_eq!(response_error(&msg), "The debug probe could not be opened.");
    }

    /// No error body: fall back to `message` — but never to "cancelled", which
    /// reads as "you cancelled this" when nobody did.
    #[test]
    fn the_cancelled_placeholder_is_never_shown_as_the_reason() {
        let bare = json!({
            "type": "response", "command": "attach", "success": false,
            "message": "cancelled",
        });
        let out = response_error(&bare);
        assert!(!out.contains("cancelled"), "{out}");
        assert!(out.contains("attach"), "{out}");

        // A server that DOES describe the failure in `message` is believed.
        let spoken = json!({
            "type": "response", "command": "pause", "success": false,
            "message": "core is not halted",
        });
        assert_eq!(response_error(&spoken), "core is not halted");
    }

    /// An unfilled placeholder is worse than nothing — don't print braces.
    #[test]
    fn an_unresolved_template_falls_through_to_the_message() {
        let msg = json!({
            "type": "response", "command": "attach", "success": false,
            "message": "cancelled",
            "body": { "error": { "format": "{response_message}" } },
        });
        let out = response_error(&msg);
        assert!(!out.contains('{'), "{out}");
    }

    /// Live watch reads memory over DAP, which ships it base64-encoded, and
    /// only ADDRESS rows can be read while the target runs.
    #[test]
    fn live_watch_parses_addresses_and_decodes_memory() {
        assert_eq!(parse_addr("0x20000004"), Some(0x2000_0004));
        assert_eq!(parse_addr("  0x2000_0004 "), Some(0x2000_0004));
        assert_eq!(parse_addr("0X1FFFF7E8"), Some(0x1FFF_F7E8));
        // A NAME has no address — it needs a frame, so live can't read it.
        assert_eq!(parse_addr("TICK"), None);
        assert_eq!(parse_addr("0xZZ"), None);

        // "AQAAAA==" = 01 00 00 00 → 1 little-endian.
        let bytes = base64_decode("AQAAAA==").expect("valid base64");
        assert_eq!(bytes, vec![1, 0, 0, 0]);
        let mut raw: u64 = 0;
        for (k, b) in bytes.iter().enumerate() {
            raw |= (*b as u64) << (8 * k);
        }
        assert_eq!(raw, 1);
        // Every 6-bit group maps back to its byte, padding and all.
        assert_eq!(base64_decode("/w==").unwrap(), vec![0xFF]);
        assert_eq!(base64_decode("ESIz").unwrap(), vec![0x11, 0x22, 0x33]);
        // Garbage is rejected rather than turned into a plausible number.
        assert!(base64_decode("!!!!").is_none());
    }

    /// The badge text while flashing: a message, a percentage, or both — and
    /// nothing at all when the event carries neither, so the last useful line
    /// stays put instead of blinking away.
    #[test]
    fn progress_updates_render_message_and_percent() {
        let ev = |body: Value| json!({"event": "progressUpdate", "body": body});
        assert_eq!(
            progress_text(&ev(
                json!({"message": "Erasing sectors", "percentage": 41.7})
            )),
            Some("Erasing sectors · 42%".to_owned())
        );
        assert_eq!(
            progress_text(&ev(json!({"message": "Loading debug info"}))),
            Some("Loading debug info".to_owned())
        );
        assert_eq!(
            progress_text(&ev(json!({"percentage": 8.0}))),
            Some("8%".to_owned())
        );
        assert_eq!(progress_text(&ev(json!({}))), None);
        assert_eq!(progress_text(&ev(json!({"message": ""}))), None);
    }

    /// The DAP answer is positional and carries no line of its own for a
    /// breakpoint the debugger refused — so the request's lines drive the
    /// mapping, a shorter answer array leaves the rest unverified, and only a
    /// DIFFERENT reported line counts as a relocation.
    #[test]
    fn breakpoint_verdicts_zip_with_the_request() {
        let asked = [265, 282, 286, 300];
        let answers = vec![
            json!({"verified": false, "message": "no code at this line"}),
            json!({"verified": true, "line": 282}),
            json!({"verified": true, "line": 291}),
            // 300: probe-rs sent nothing back for it.
        ];
        let got = bp_statuses(&asked, &answers);

        assert_eq!(got.len(), 4);
        let b265 = &got[&265];
        assert!(!b265.verified);
        assert_eq!(b265.message.as_deref(), Some("no code at this line"));
        // Reported line == requested line → not a relocation.
        assert!(got[&282].verified);
        assert_eq!(got[&282].moved_to, None);
        // Moved to the nearest line with code.
        assert_eq!(got[&286].moved_to, Some(291));
        // A missing answer is NOT a confirmation.
        assert_eq!(got[&300], BpStatus::default());
        assert!(!got[&300].verified);
    }

    #[test]
    fn eval_result_placeholders_are_flagged_unresolved() {
        // probe-rs's `success:true` placeholder markers → greyed with a reason.
        assert_eq!(
            classify_eval_result("<invalid expression \"buf_a\">"),
            ("not in scope / unsupported".to_string(), true)
        );
        assert_eq!(
            classify_eval_result("<not found: b>"),
            ("not found in this frame".to_string(), true)
        );
        assert_eq!(
            classify_eval_result("<optimized out>"),
            ("optimized out".to_string(), true)
        );
        // A real value is passed through unchanged.
        assert_eq!(classify_eval_result("42"), ("42".to_string(), false));
        assert_eq!(
            classify_eval_result("Some(5)"),
            ("Some(5)".to_string(), false)
        );
    }

    #[test]
    fn rel_of_strips_workspace_prefix_case_insensitively() {
        let dir = PathBuf::from(r"C:\Users\x\AppData\Local\Temp\embedded_ide_0_check");
        assert_eq!(
            rel_of(
                r"C:\Users\x\AppData\Local\temp\embedded_ide_0_check\src\main.rs",
                &dir
            ),
            Some("src/main.rs".to_string())
        );
        // Forward slashes too.
        assert_eq!(
            rel_of(
                "C:/Users/x/AppData/Local/Temp/embedded_ide_0_check/src/pins/mod.rs",
                &dir
            ),
            Some("src/pins/mod.rs".to_string())
        );
        // Outside the workspace (HAL sources in ~/.cargo) → None.
        assert_eq!(
            rel_of(r"C:\Users\x\.cargo\registry\src\stm32f1xx-hal\lib.rs", &dir),
            None
        );
    }

    /// The frame/variable JSON shapes we rely on — parsed like the reader does.
    #[test]
    fn stack_and_variables_parse_from_dap_json() {
        let msg: Value = serde_json::json!({
            "type": "response", "request_seq": 7, "success": true,
            "command": "stackTrace",
            "body": { "stackFrames": [
                { "id": 1001, "name": "main", "line": 42,
                  "source": { "path": "C:/w/src/main.rs" } },
                { "id": 1002, "name": "Reset", "line": 0 }
            ]}
        });
        let dir = PathBuf::from("C:/w");
        let frames: Vec<Frame> = msg["body"]["stackFrames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| Frame {
                id: f["id"].as_i64().unwrap_or(0),
                name: f["name"].as_str().unwrap_or("?").to_string(),
                file_rel: f["source"]["path"].as_str().and_then(|p| rel_of(p, &dir)),
                line: f["line"].as_u64().unwrap_or(0) as u32,
            })
            .collect();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].file_rel.as_deref(), Some("src/main.rs"));
        assert_eq!(frames[0].line, 42);
        assert_eq!(frames[1].file_rel, None); // no source → greyed row
    }
}
