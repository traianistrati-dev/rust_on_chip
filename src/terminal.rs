//! Built-in command console (bottom-panel "Terminal" tab).
//!
//! A **streaming command runner** — NOT a full PTY. Each command is a fresh
//! shell spawn (`powershell -NoProfile -NonInteractive -Command <cmd>` on
//! Windows, `$SHELL -c <cmd>` on macOS / Linux) whose stdout+stderr are read
//! line-by-line on background threads and appended to a scrollback buffer, so
//! long-running commands (`cargo build`, `probe-rs`, `git`) show output live.
//! A Stop button kills the child; Up/Down recall history; ANSI SGR colour codes
//! are parsed for display. It can't answer interactive prompts mid-run (no TTY).
//!
//! The command text is the user's own, in their own shell's dialect — the
//! console does not translate between them.
//!
//! Mirrors the reader-thread + throttled-repaint + `AtomicBool` stop pattern of
//! [`crate::serial`].

use eframe::egui;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Max scrollback lines kept; older lines are dropped.
const MAX_LINES: usize = 5_000;
const REPAINT_EVERY: Duration = Duration::from_millis(33);

/// Where a scrollback line came from — tints it in the view.
#[derive(Clone, Copy, PartialEq)]
pub enum LineKind {
    /// A command the user ran (echoed with a `> ` prompt).
    Input,
    Stdout,
    Stderr,
    /// IDE-generated notice (exit code, killed, cwd change, errors).
    Notice,
}

/// One scrollback line + its already-parsed colour spans (from ANSI SGR).
pub struct TermLine {
    pub kind: LineKind,
    /// `(text, colour)` runs. `colour == None` → use the kind's default colour.
    pub spans: Vec<(String, Option<egui::Color32>)>,
}

#[derive(Default)]
pub struct TerminalState {
    pub lines: Vec<TermLine>,
    /// `true` while a command's child process is still running.
    pub running: bool,
    /// When set, every pushed line is prefixed with the time ELAPSED since this
    /// instant — the ESP monitor sets it when a session attaches, so device
    /// output reads like `[  1.234] tick`.
    ///
    /// Elapsed, not wall-clock: the crate carries no `chrono`/`time`, and the
    /// hand-rolled [`crate::activity::fmt_clock`] is UTC-based (it says so) —
    /// stamping firmware output with an hour that is off by the timezone would
    /// be worse than no stamp. Time SINCE THE BOARD STARTED TALKING is also the
    /// number you actually want when watching a loop.
    pub stamp_from: Option<Instant>,
    /// egui pass number of the last frame that DREW these lines — set by the
    /// renderer. Readers repaint only while it is recent, see
    /// [`drawn_recently`].
    pub drawn_pass: u64,
}

/// Is a stream's view on screen, going by the pass that last drew it?
///
/// A reader thread cannot ask whether its tab is visible, and each of its
/// repaints runs a whole app frame. Asked unconditionally, a serial port, an
/// RTT session or a monitor streaming into a HIDDEN tab kept the app redrawing
/// at 30 fps for nothing. The renderer stamps the pass it drew in; two passes of
/// slack cover egui's discard-and-redraw passes.
///
/// While the app is idle the pass counter does not move, so a view that is
/// shown stays "recent" and keeps getting its repaints. Showing a hidden one is
/// a click, which is a frame of its own.
pub(crate) fn drawn_recently(drawn_pass: u64, now_pass: u64) -> bool {
    now_pass <= drawn_pass.saturating_add(2)
}

/// Should a reader that just appended to a stream wake the UI for it?
///
/// Only while its view was [`drawn_recently`] AND the window is not minimized.
/// A minimized window runs no egui pass (eframe 0.36 calls only `App::logic`,
/// through `Context::run_logic`), so the pass counter stands still and the view
/// that was on screen when the window went down stays "recent": every burst
/// woke the event loop for a logic tick with nothing to show. The window state
/// is the one input `run_logic` keeps fresh. Restoring is a frame of its own,
/// and it draws whatever arrived meanwhile.
pub(crate) fn stream_wants_repaint(ctx: &egui::Context, drawn_pass: u64) -> bool {
    let root = egui::ViewportId::ROOT;
    let minimized = ctx.input_for(root, |i| {
        i.raw.viewports.get(&root).and_then(|v| v.minimized)
    });
    minimized != Some(true) && drawn_recently(drawn_pass, ctx.cumulative_pass_nr_for(root))
}

/// Stamp `state` as on screen this pass — see [`drawn_recently`].
///
/// Every view of a stream calls it, INCLUDING its empty placeholder ("Nothing
/// from the device yet"): a view that shows the hint and returns is still on
/// screen, and without the stamp the first line after a Clear never repainted.
pub(crate) fn mark_drawn(state: &Mutex<TerminalState>, ctx: &egui::Context) {
    let pass = ctx.cumulative_pass_nr_for(egui::ViewportId::ROOT);
    if let Ok(mut st) = state.lock() {
        st.drawn_pass = pass;
    }
}

/// Colour of the `[  1.234]` prefix — dim enough to read past, present enough
/// to scan down.
const STAMP_COLOR: egui::Color32 = egui::Color32::from_rgb(110, 120, 135);

impl TerminalState {
    pub(crate) fn push(&mut self, kind: LineKind, spans: Vec<(String, Option<egui::Color32>)>) {
        let mut spans = spans;
        if let Some(t0) = self.stamp_from {
            // Width 7 keeps the column steady from `  0.000` to `9999.999`.
            spans.insert(
                0,
                (
                    format!("[{:>7.3}] ", t0.elapsed().as_secs_f64()),
                    Some(STAMP_COLOR),
                ),
            );
        }
        self.lines.push(TermLine { kind, spans });
        if self.lines.len() > MAX_LINES {
            let excess = self.lines.len() - MAX_LINES;
            self.lines.drain(..excess);
        }
    }

    pub(crate) fn push_plain(&mut self, kind: LineKind, text: impl Into<String>) {
        self.push(kind, vec![(text.into(), None)]);
    }

    /// The last `n` lines as plain text (colour spans flattened), oldest first.
    /// For post-mortems on a console a subprocess wrote into — a crash is always
    /// at the END, and a long session's scrollback isn't worth walking.
    pub(crate) fn tail_text(&self, n: usize) -> String {
        let start = self.lines.len().saturating_sub(n);
        self.lines[start..]
            .iter()
            .map(|l| l.spans.iter().map(|(t, _)| t.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The console UI + process state (owned by `AppIde.terminal`).
pub struct TerminalConsole {
    pub input: String,
    /// Working directory commands run in (default: the generated workspace).
    pub cwd: PathBuf,
    /// Previously-run commands (newest last); `history_pos` walks them with Up/Down.
    history: Vec<String>,
    history_pos: Option<usize>,
    pub state: Arc<Mutex<TerminalState>>,
    /// Per-run stop flag; set on Stop / a new run, so an old reader bails out.
    stop: Option<Arc<AtomicBool>>,
    /// The running child, kept so Stop can kill it.
    child: Arc<Mutex<Option<std::process::Child>>>,
}

impl Default for TerminalConsole {
    fn default() -> Self {
        Self {
            input: String::new(),
            cwd: crate::workspace::dir(),
            history: Vec::new(),
            history_pos: None,
            state: Arc::new(Mutex::new(TerminalState::default())),
            stop: None,
            child: Arc::new(Mutex::new(None)),
        }
    }
}

impl TerminalConsole {
    pub fn is_running(&self) -> bool {
        self.state.lock().unwrap().running
    }

    pub fn clear(&mut self) {
        self.state.lock().unwrap().lines.clear();
    }

    /// Recall the previous / next history entry into `input` (Up = older).
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match self.history_pos {
            None => self.history.len() - 1, // first Up → newest entry
            Some(p) => p.saturating_sub(1), // subsequent Ups → older
        };
        self.history_pos = Some(pos);
        self.input = self.history[pos].clone();
    }

    pub fn history_next(&mut self) {
        let Some(pos) = self.history_pos else {
            return;
        };
        if pos + 1 < self.history.len() {
            self.history_pos = Some(pos + 1);
            self.input = self.history[pos + 1].clone();
        } else {
            // Past the newest entry → back to an empty prompt.
            self.history_pos = None;
            self.input.clear();
        }
    }

    /// Run the current `input` as a command. No-op while one is already running
    /// or the input is blank.
    pub fn run(&mut self, ctx: &egui::Context) {
        let cmd = self.input.trim().to_string();
        if cmd.is_empty() || self.is_running() {
            return;
        }
        self.input.clear();
        self.history_pos = None;
        if self.history.last().map(|h| h.as_str()) != Some(cmd.as_str()) {
            self.history.push(cmd.clone());
        }

        // Echo the command with a prompt.
        {
            let mut s = self.state.lock().unwrap();
            s.push_plain(LineKind::Input, format!("> {cmd}"));
        }

        // `cd` / `Set-Location` is handled client-side: each command is a fresh
        // spawn, so a child's `cd` wouldn't persist. Intercept it and move our
        // own `cwd` instead, so the console behaves like a real shell session.
        if let Some(target) = parse_cd(&cmd) {
            self.change_dir(&target);
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        self.stop = Some(Arc::clone(&stop));

        let (shell, shell_args) = shell_invocation();
        let mut command = shell_command(&shell, &shell_args, &cmd, &self.cwd);

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                let mut s = self.state.lock().unwrap();
                s.push_plain(
                    LineKind::Notice,
                    format!("[error] couldn't launch {shell}: {e}"),
                );
                self.stop = None;
                return;
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        {
            let mut s = self.state.lock().unwrap();
            s.running = true;
        }
        *self.child.lock().unwrap() = Some(child);

        // One reader thread per pipe, plus a waiter that clears `running` and
        // prints the exit status once BOTH pipes hit EOF (child exited).
        let done = Arc::new(std::sync::atomic::AtomicUsize::new(0)); // pipes finished
        if let Some(out) = stdout {
            spawn_reader(
                out,
                LineKind::Stdout,
                Arc::clone(&self.state),
                Arc::clone(&stop),
                ctx.clone(),
                Arc::clone(&done),
            );
        } else {
            done.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(err) = stderr {
            spawn_reader(
                err,
                LineKind::Stderr,
                Arc::clone(&self.state),
                Arc::clone(&stop),
                ctx.clone(),
                Arc::clone(&done),
            );
        } else {
            done.fetch_add(1, Ordering::Relaxed);
        }

        // Waiter thread: once both pipes are done, reap the child + report exit.
        let state = Arc::clone(&self.state);
        let child_slot = Arc::clone(&self.child);
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            // Wait for both reader threads to signal EOF.
            while done.load(Ordering::Relaxed) < 2 {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let status = child_slot.lock().unwrap().as_mut().map(|c| c.wait());
            let mut s = state.lock().unwrap();
            match status {
                Some(Ok(st)) if st.success() => s.push_plain(LineKind::Notice, "[exit 0]"),
                Some(Ok(st)) => s.push_plain(
                    LineKind::Notice,
                    format!(
                        "[exit {}]",
                        st.code()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".into())
                    ),
                ),
                _ => {}
            }
            s.running = false;
            *child_slot.lock().unwrap() = None;
            drop(s);
            ctx2.request_repaint();
        });
    }

    /// Kill the running command (Stop button / Ctrl+C).
    pub fn stop(&mut self) {
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        let mut s = self.state.lock().unwrap();
        if s.running {
            s.push_plain(LineKind::Notice, "[stopped]");
            s.running = false;
        }
    }

    /// Apply a client-side `cd <target>` (absolute or relative to `cwd`).
    fn change_dir(&mut self, target: &str) {
        let candidate = if target == "~" {
            dirs_home()
        } else {
            let p = PathBuf::from(target);
            if p.is_absolute() { p } else { self.cwd.join(p) }
        };
        let mut s = self.state.lock().unwrap();
        match candidate.canonicalize() {
            Ok(abs) if abs.is_dir() => {
                // Strip Windows verbatim `\\?\` prefix for a tidy display.
                self.cwd = abs;
                let shown = self.cwd.to_string_lossy().replace(r"\\?\", "");
                s.push_plain(LineKind::Notice, format!("[cwd] {shown}"));
            }
            _ => s.push_plain(
                LineKind::Notice,
                format!("[error] no such directory: {target}"),
            ),
        }
    }
}

/// The shell to run one command in: `(program, flags before the command)`.
///
/// Windows gets PowerShell with `-NoProfile` (a user profile can print banners
/// that pollute the scrollback, and it costs startup time on every command) and
/// `-NonInteractive` (this console has no TTY, so a prompt would hang forever).
///
/// The unixes get `$SHELL -c`, so a zsh / fish user types their own dialect
/// rather than the one this file happens to assume. `/bin/sh` is the fallback,
/// because `$SHELL` is simply absent when the app is launched from a desktop
/// entry rather than a login shell. There is no `-NonInteractive` equivalent —
/// `stdin(Stdio::null())` at the call site is what stops a prompt from hanging.
/// The child process a typed command runs as.
///
/// Split out of `run` so its ENVIRONMENT can be asserted, because that
/// environment is load-bearing and invisible. `build::no_window` is what puts
/// the Xtensa linker on PATH and drops an inherited `RUSTUP_TOOLCHAIN`; without
/// both, a `cargo build` typed into this tab fails on an ESP32/S2/S3 project
/// while the Build button on the same project succeeds. One command, two
/// answers, and nothing on screen to explain the difference.
///
/// `stdin` is null deliberately. This is not a PTY, so a tool that stops to ask
/// something - `espflash` with several boards attached, say - reads EOF and
/// fails with its own message rather than hanging the tab forever waiting for
/// an answer that cannot be typed.
fn shell_command(shell: &str, shell_args: &[String], cmd: &str, cwd: &std::path::Path) -> Command {
    let mut command = Command::new(shell);
    command
        .args(shell_args)
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::build::no_window(&mut command);
    command
}

fn shell_invocation() -> (String, Vec<String>) {
    if cfg!(target_os = "windows") {
        (
            "powershell".to_string(),
            ["-NoProfile", "-NonInteractive", "-Command"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        )
    } else {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "/bin/sh".to_string());
        (shell, vec!["-c".to_string()])
    }
}

/// Display name of the shell commands run in — for the UI, so the user knows
/// which dialect the console expects. Just the file name (`zsh`, not
/// `/bin/zsh`), since the path adds nothing at a glance.
pub fn shell_name() -> String {
    let (shell, _) = shell_invocation();
    std::path::Path::new(&shell)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or(shell)
}

/// Home directory (best-effort, Windows `USERPROFILE` / Unix `HOME`).
fn dirs_home() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Recognise a whole-command `cd <dir>` (no pipes / `;` / `&`) → the target.
/// Returns `None` for anything else.
///
/// PowerShell's aliases are accepted only ON Windows. `sl` in particular MUST
/// NOT be treated as a cd on the unixes: it is a real program there, and
/// swallowing `sl foo` would make the console lie about what it ran.
///
/// A bare `cd` means "go home" on the unixes, so it maps to `~` — which
/// [`TerminalConsole::change_dir`] already resolves. PowerShell's bare `Set-
/// Location` does the same, so the rule is shared.
fn parse_cd(cmd: &str) -> Option<String> {
    let c = cmd.trim();
    if c.contains('|') || c.contains(';') || c.contains('&') {
        return None;
    }
    // Bare "go home" forms.
    if c == "cd" || (cfg!(target_os = "windows") && (c == "Set-Location" || c == "sl")) {
        return Some("~".to_string());
    }
    let mut rest = c.strip_prefix("cd ");
    if cfg!(target_os = "windows") {
        rest = rest
            .or_else(|| c.strip_prefix("Set-Location "))
            .or_else(|| c.strip_prefix("sl "));
    }
    let target = rest?.trim().trim_matches('"').trim_matches('\'').trim();
    if target.is_empty() {
        // `cd ""` — treat as home rather than an error, same as a bare `cd`.
        Some("~".to_string())
    } else {
        Some(target.to_string())
    }
}

/// Background line reader: reads `pipe` to EOF, parsing each line's ANSI SGR
/// colours, appending to the scrollback (throttled repaint). Bumps `done` on
/// EOF so the waiter can reap the child. (Shared with the RTT console.)
pub(crate) fn spawn_reader(
    pipe: impl std::io::Read + Send + 'static,
    kind: LineKind,
    state: Arc<Mutex<TerminalState>>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
    done: Arc<std::sync::atomic::AtomicUsize>,
) {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut last_repaint = Instant::now() - REPAINT_EVERY;
        let mut buf = Vec::new();
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            buf.clear();
            // Read up to a newline (bytes, so invalid UTF-8 doesn't abort).
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => break, // EOF
                Ok(_) => {
                    let raw = String::from_utf8_lossy(&buf);
                    // Strip the line terminator (LF, and the CR of a CRLF pair)
                    // FIRST, then — for progress bars that rewrite the line with
                    // embedded `\r` — keep only the last rewritten segment. Doing
                    // it in this order matters: on Windows every line ends `\r\n`,
                    // so splitting on `\r` before trimming would leave just "\n".
                    let terminated = raw.trim_end_matches('\n').trim_end_matches('\r');
                    let line = terminated.rsplit('\r').next().unwrap_or(terminated);
                    let spans = parse_ansi(line);
                    let drawn_pass = {
                        let mut s = state.lock().unwrap();
                        s.push(kind, spans);
                        s.drawn_pass
                    };
                    // Asked with the state lock released. EOF below repaints
                    // unconditionally: a finished command changes what the
                    // rest of the UI shows.
                    if !stream_wants_repaint(&ctx, drawn_pass) {
                        // Nobody is looking; the lines are in the buffer.
                    } else if last_repaint.elapsed() >= REPAINT_EVERY {
                        ctx.request_repaint();
                        last_repaint = Instant::now();
                    } else {
                        ctx.request_repaint_after(REPAINT_EVERY);
                    }
                }
                Err(_) => break,
            }
        }
        done.fetch_add(1, Ordering::Relaxed);
        ctx.request_repaint();
    });
}

/// The line's visible text, with every ANSI escape sequence removed — for the
/// consoles that render plain `String`s (the Flash tab's log) rather than the
/// parsed spans. A tool that colours its output would otherwise show its escape
/// codes as literal garbage around every word.
pub(crate) fn strip_ansi(line: &str) -> String {
    if !line.contains('\u{1b}') {
        return line.to_owned();
    }
    parse_ansi(line).into_iter().map(|(t, _)| t).collect()
}

/// Split a line into `(text, colour)` runs by parsing ANSI SGR (`\x1b[…m`)
/// sequences. Non-SGR escape sequences (cursor moves, `\x1b[K`, …) are dropped.
/// Only the common colour attributes are honoured; unknown ones reset to default.
fn parse_ansi(line: &str) -> Vec<(String, Option<egui::Color32>)> {
    let mut spans: Vec<(String, Option<egui::Color32>)> = Vec::new();
    let mut cur = String::new();
    let mut color: Option<egui::Color32> = None;
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            // Escape — find the sequence. We only interpret CSI `…m` (SGR).
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // Read until the final byte (a letter).
                let mut j = i + 2;
                while j < bytes.len() && !bytes[j].is_ascii_alphabetic() {
                    j += 1;
                }
                if j < bytes.len() {
                    let final_byte = bytes[j];
                    let params = &line[i + 2..j];
                    if final_byte == b'm' {
                        // Flush the current run before switching colour.
                        if !cur.is_empty() {
                            spans.push((std::mem::take(&mut cur), color));
                        }
                        color = sgr_color(params, color);
                    }
                    // Skip the whole escape sequence (SGR or otherwise).
                    i = j + 1;
                    continue;
                }
            }
            // Lone ESC or unrecognised — skip the ESC byte.
            i += 1;
            continue;
        }
        // Regular UTF-8 char.
        let ch_len = utf8_len(bytes[i]);
        if let Ok(s) = std::str::from_utf8(&bytes[i..(i + ch_len).min(bytes.len())]) {
            cur.push_str(s);
        }
        i += ch_len;
    }
    if !cur.is_empty() {
        spans.push((cur, color));
    }
    if spans.is_empty() {
        spans.push((String::new(), None));
    }
    spans
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

/// Map an SGR parameter list (`"1;32"`) to a display colour. `prev` is kept for
/// unhandled attributes; `0` (reset) / `39` (default fg) clear the colour.
fn sgr_color(params: &str, prev: Option<egui::Color32>) -> Option<egui::Color32> {
    let mut color = prev;
    for p in params.split(';') {
        match p.trim() {
            "" | "0" | "39" => color = None,
            "30" | "90" => color = Some(egui::Color32::from_gray(120)),
            "31" | "91" => color = Some(egui::Color32::from_rgb(230, 100, 90)),
            "32" | "92" => color = Some(egui::Color32::from_rgb(90, 200, 110)),
            "33" | "93" => color = Some(egui::Color32::from_rgb(220, 180, 60)),
            "34" | "94" => color = Some(egui::Color32::from_rgb(90, 150, 235)),
            "35" | "95" => color = Some(egui::Color32::from_rgb(200, 120, 220)),
            "36" | "96" => color = Some(egui::Color32::from_rgb(80, 200, 220)),
            "37" | "97" => color = Some(egui::Color32::from_gray(210)),
            _ => {} // bold/underline/etc. — ignore, keep colour
        }
    }
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_repaints_only_a_view_drawn_lately() {
        // Idle app, view on screen: the pass counter has not moved since.
        assert!(drawn_recently(40, 40));
        // egui's discard-and-redraw passes are allowed for.
        assert!(drawn_recently(40, 42));
        // Another tab has been drawing for a while: hidden.
        assert!(!drawn_recently(40, 43));
        // Never drawn this session (the default 0), app long past startup.
        assert!(!drawn_recently(0, 500));
        assert!(drawn_recently(u64::MAX, u64::MAX), "no overflow");
    }

    /// The root window as eframe reports it to egui: shown, or minimized.
    fn window(minimized: bool) -> egui::RawInput {
        let mut input = egui::RawInput::default();
        input
            .viewports
            .entry(egui::ViewportId::ROOT)
            .or_default()
            .minimized = Some(minimized);
        input
    }

    fn wait_until(what: impl Fn() -> bool, patience: Duration) -> bool {
        let t0 = Instant::now();
        while !what() {
            if t0.elapsed() > patience {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }

    #[test]
    fn a_minimized_window_wants_no_stream_repaint() {
        let ctx = egui::Context::default();
        // No window state yet (startup, a bare context): counts as shown.
        assert!(stream_wants_repaint(&ctx, 0));

        let state = Mutex::new(TerminalState::default());
        crate::headless::run_ui(&ctx, window(false), |ui| mark_drawn(&state, ui.ctx()));
        let drawn = state.lock().unwrap().drawn_pass;
        assert!(stream_wants_repaint(&ctx, drawn), "shown and just drawn");

        // What eframe runs while the window is down: logic only, no pass.
        let _ = ctx.run_logic(&window(true), |_| {});
        assert!(drawn_recently(
            drawn,
            ctx.cumulative_pass_nr_for(egui::ViewportId::ROOT)
        ));
        assert!(!stream_wants_repaint(&ctx, drawn), "minimized");

        // The restore frame.
        crate::headless::run_ui(&ctx, window(false), |_| {});
        assert!(stream_wants_repaint(&ctx, drawn), "restored");

        // Shown, but another tab has been drawing since.
        for _ in 0..2 {
            crate::headless::run_ui(&ctx, window(false), |_| {});
        }
        assert!(!stream_wants_repaint(&ctx, drawn), "view not drawn lately");
    }

    /// A minimized window runs no egui pass on eframe 0.36, only `App::logic`
    /// through `Context::run_logic`. The pass counter stood still, so the
    /// Terminal drawn just before the minimize stayed "recent" and every line
    /// of a running command woke the event loop (the repaint callback IS
    /// eframe's wake-up) for as long as the window was down.
    #[test]
    fn a_stream_does_not_wake_a_minimized_window() {
        use std::io::Write;
        use std::sync::atomic::AtomicUsize;

        let ctx = egui::Context::default();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        ctx.set_request_repaint_callback(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let state = Arc::new(Mutex::new(TerminalState::default()));
        let (stdout, mut child) = std::io::pipe().unwrap();
        let done = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        spawn_reader(
            stdout,
            LineKind::Stdout,
            Arc::clone(&state),
            stop,
            ctx.clone(),
            Arc::clone(&done),
        );

        // The Terminal tab on screen, drawn by its own renderer.
        let shown_frame = || {
            crate::headless::run_ui(&ctx, window(false), |ui| {
                crate::app::tabs::terminal_tab::render_scrollback(ui, &state, "term", 200.0);
            });
        };
        shown_frame();

        // One line from the child, and the wake-ups it caused. Two logic-only
        // ticks first, as eframe runs them between wake-ups: they settle any
        // request still outstanding, so the reader's own reaches the callback.
        let mut sent = 0;
        let mut line = |minimized: bool, patience: Duration| {
            for _ in 0..2 {
                let _ = ctx.run_logic(&window(minimized), |_| {});
            }
            wakes.store(0, Ordering::SeqCst);
            sent += 1;
            writeln!(child, "line {sent}").unwrap();
            let taken = || state.lock().unwrap().lines.len() == sent;
            assert!(wait_until(taken, Duration::from_secs(10)), "reader stalled");
            // The request follows the push, with the lock released.
            wait_until(|| wakes.load(Ordering::SeqCst) > 0, patience);
            wakes.load(Ordering::SeqCst)
        };

        let while_minimized: Vec<usize> = (0..3)
            .map(|_| line(true, Duration::from_millis(200)))
            .collect();
        assert_eq!(
            while_minimized,
            [0, 0, 0],
            "wake-ups per line while minimized"
        );

        // Restored: the lines that came meanwhile are there for its first
        // frame, and the stream wakes the UI again.
        shown_frame();
        assert_eq!(state.lock().unwrap().lines.len(), 3);
        assert!(
            line(false, Duration::from_secs(10)) > 0,
            "restored Terminal not repainted"
        );

        drop(child);
        assert!(wait_until(
            || done.load(Ordering::SeqCst) == 1,
            Duration::from_secs(10)
        ));
    }

    /// `stamp_from` prefixes each line with the time since the session attached,
    /// as its OWN span — so the stamp keeps its dim colour while the line below
    /// it keeps whatever colour its content earned, and the text is untouched.
    #[test]
    fn stamped_lines_get_a_separate_time_span() {
        let mut st = TerminalState::default();

        // Off by default: nothing else in the app wants stamps.
        st.push_plain(LineKind::Stdout, "plain");
        assert_eq!(st.lines[0].spans.len(), 1);
        assert_eq!(st.lines[0].spans[0].0, "plain");

        st.stamp_from = Some(Instant::now());
        st.push_plain(LineKind::Stdout, "tick");
        let spans = &st.lines[1].spans;
        assert_eq!(spans.len(), 2, "stamp is its own span: {spans:?}");
        assert_eq!(spans[1].0, "tick", "the line itself is unchanged");
        assert!(spans[0].1.is_some(), "the stamp carries its own colour");
        let stamp = &spans[0].0;
        assert!(
            stamp.starts_with('[') && stamp.ends_with("] "),
            "bracketed: {stamp:?}"
        );
        // Fixed width keeps the column steady as seconds grow.
        assert_eq!(stamp.len(), "[  0.000] ".len(), "{stamp:?}");
        let secs: f64 = stamp
            .trim_start_matches('[')
            .trim_end_matches("] ")
            .trim()
            .parse()
            .unwrap_or_else(|e| panic!("{stamp:?} is not a number: {e}"));
        assert!((0.0..1.0).contains(&secs), "just attached: {secs}");
    }

    #[test]
    fn parse_cd_variants() {
        assert_eq!(parse_cd("cd src"), Some("src".into()));
        assert_eq!(parse_cd("cd \"my dir\""), Some("my dir".into()));
        assert_eq!(parse_cd("cargo build"), None);
        assert_eq!(parse_cd("cd src && ls"), None); // compound → let the shell run it
        // Bare `cd` = go home, on every platform.
        assert_eq!(parse_cd("cd"), Some("~".into()));
    }

    /// PowerShell's cd aliases are Windows-only. `sl` is a REAL program on the
    /// unixes (and `Set-Location` is not a shell builtin there), so intercepting
    /// either would make the console silently not run what the user typed.
    #[test]
    fn powershell_cd_aliases_are_windows_only() {
        let expected = |v: Option<&str>| v.map(str::to_string);
        if cfg!(target_os = "windows") {
            assert_eq!(parse_cd("Set-Location .."), expected(Some("..")));
            assert_eq!(parse_cd("sl src"), expected(Some("src")));
            assert_eq!(parse_cd("sl"), expected(Some("~")));
        } else {
            assert_eq!(parse_cd("Set-Location .."), None);
            assert_eq!(parse_cd("sl src"), None);
            assert_eq!(parse_cd("sl"), None);
        }
    }

    /// The invocation must always end with a "run this string" flag, or the
    /// command would be taken as the name of a script FILE.
    #[test]
    fn shell_invocation_takes_a_command_string() {
        let (shell, args) = shell_invocation();
        assert!(!shell.trim().is_empty());
        let last = args.last().expect("no flags before the command");
        if cfg!(target_os = "windows") {
            assert_eq!(shell, "powershell");
            assert_eq!(last, "-Command");
            // A profile banner would pollute the scrollback; a prompt would hang.
            assert!(args.iter().any(|a| a == "-NoProfile"));
            assert!(args.iter().any(|a| a == "-NonInteractive"));
        } else {
            assert_eq!(last, "-c");
            assert_eq!(args.len(), 1);
        }
    }

    #[test]
    fn parse_ansi_splits_colours() {
        // "\x1b[32mgreen\x1b[0m plain"
        let spans = parse_ansi("\u{1b}[32mgreen\u{1b}[0m plain");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].0, "green");
        assert!(spans[0].1.is_some());
        assert_eq!(spans[1].0, " plain");
        assert_eq!(spans[1].1, None);
    }

    #[test]
    fn parse_ansi_strips_non_sgr() {
        // A cursor-clear `\x1b[K` must be dropped, text preserved.
        let spans = parse_ansi("abc\u{1b}[Kdef");
        let text: String = spans.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, "abcdef");
    }

    #[test]
    fn parse_ansi_plain_line() {
        let spans = parse_ansi("just text");
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].0, "just text");
        assert_eq!(spans[0].1, None);
    }

    /// The CRLF/progress `\r` handling done in the reader (kept as a free fn
    /// here so it's unit-testable without a live pipe).
    fn strip_line(raw: &str) -> &str {
        let terminated = raw.trim_end_matches('\n').trim_end_matches('\r');
        terminated.rsplit('\r').next().unwrap_or(terminated)
    }

    #[test]
    fn strip_line_handles_crlf_and_progress() {
        // Windows CRLF must keep the text (the bug: `rsplit('\r')` first → "\n").
        assert_eq!(strip_line("Compiling nb v1.1.0\r\n"), "Compiling nb v1.1.0");
        // Plain LF.
        assert_eq!(strip_line("done\n"), "done");
        // Progress bar rewriting the line → last segment wins.
        assert_eq!(strip_line("10%\r50%\r100%\n"), "100%");
    }
}

#[cfg(test)]
mod esp_environment_tests {
    use super::*;

    fn built(cwd: &std::path::Path) -> Command {
        let (shell, args) = shell_invocation();
        shell_command(&shell, &args, "cargo build --release", cwd)
    }

    /// The one thing that makes an Xtensa project buildable from this tab.
    ///
    /// `RUSTUP_TOOLCHAIN` outranks a project's own `rust-toolchain.toml`, and
    /// launching the IDE with `cargo run` leaks one into every child. An ESP32,
    /// S2 or S3 project would then build with stock rustc and fail with
    /// `'esp32s3' is not a recognized processor` - a message that names no
    /// toolchain, from a tab where the Build button works fine.
    #[test]
    fn a_typed_command_does_not_inherit_the_ides_toolchain_pin() {
        let cmd = built(std::path::Path::new("."));
        assert!(
            cmd.get_envs()
                .any(|(k, v)| k == "RUSTUP_TOOLCHAIN" && v.is_none()),
            "the tab would hand its own toolchain pin to the user's cargo"
        );
    }

    /// Where a typed command runs. `cargo build` here has to mean the generated
    /// project, which is also where `rust-toolchain.toml` is written - the two
    /// have to be the same directory or the Xtensa pin is simply not seen.
    #[test]
    fn a_typed_command_runs_where_the_project_was_written() {
        let ws = crate::workspace::dir();
        let t = TerminalConsole::default();
        assert_eq!(t.cwd, ws, "the console does not start in the build dir");
        let cmd = built(&ws);
        assert_eq!(cmd.get_current_dir(), Some(ws.as_path()));
    }

    /// Not a PTY: a tool that stops to ask something must fail, not hang.
    /// `espflash` asking which of several attached boards to flash is the case
    /// that reaches this on an ESP project.
    ///
    /// Spawned for real, because `Stdio` cannot be read back off a `Command` —
    /// the only way to show stdin is at EOF is to let something try to read it.
    /// Polled rather than waited on, so a regression to a piped stdin fails
    /// here in seconds instead of hanging the suite.
    #[test]
    fn a_prompting_tool_reads_eof_instead_of_blocking() {
        let (shell, args) = shell_invocation();
        let read_all_of_stdin = if cfg!(target_os = "windows") {
            "[Console]::In.ReadToEnd().Length"
        } else {
            "wc -c"
        };
        let mut child = shell_command(&shell, &args, read_all_of_stdin, std::path::Path::new("."))
            .spawn()
            .expect("spawn a shell");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            match child.try_wait().expect("try_wait") {
                Some(_) => break,
                None if std::time::Instant::now() > deadline => {
                    let _ = child.kill();
                    panic!("the child is still waiting on stdin — it is no longer null");
                }
                None => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        }
    }

    /// The Xtensa linker directory, when this machine has one. Skipped on a
    /// machine without `espup`, which is every machine that only builds ARM or
    /// RISC-V — `add_xtensa_to_path` is a no-op there and leaves PATH untouched.
    #[test]
    fn the_xtensa_linker_reaches_a_typed_command() {
        let cmd = built(std::path::Path::new("."));
        let Some(path) = cmd
            .get_envs()
            .find(|(k, _)| *k == "PATH")
            .and_then(|(_, v)| v)
        else {
            eprintln!("no Xtensa toolchain on this machine — PATH untouched, as intended");
            return;
        };
        let has_linker = std::env::split_paths(path).any(|d| {
            std::fs::read_dir(&d).is_ok_and(|mut e| {
                e.any(|f| {
                    f.is_ok_and(|f| {
                        let n = f.file_name();
                        let n = n.to_string_lossy();
                        n.starts_with("xtensa-esp") && n.contains("-elf-gcc")
                    })
                })
            })
        });
        assert!(
            has_linker,
            "PATH was rewritten but carries no xtensa-esp*-elf-gcc"
        );
    }
}
