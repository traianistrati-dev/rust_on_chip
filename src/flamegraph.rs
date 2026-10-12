//! On-target statistical profiler → flamegraph (the Profile tab's "Runtime"
//! mode).
//!
//! There is no `perf` on an MCU and probe-rs ships no profiler command, so we
//! **halt-sample the call stack**: with the firmware already flashed + running,
//! open a `probe-rs dap-server`, attach, then repeatedly `pause → stackTrace →
//! continue`. Each stackTrace (function names and source lines, resolved by
//! probe-rs from the ELF's DWARF) is one sample; folding them builds a flame
//! tree, and counting them per source line builds the "By line" table
//! ([`by_line`]).
//!
//! CAVEAT: halt-sampling is INTRUSIVE — every pause stops the CPU for ~ms, so
//! this is a STATISTICAL view (where time is spent), not cycle-accurate, and it
//! perturbs timing. A true non-intrusive profiler would need SWO/ITM or DWT
//! PC-sampling (chip- + probe-specific). Not hardware-tested in this repo.

use crate::build::no_window;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

// ── Flame tree (pure, testable) ───────────────────────────────────────────────

/// One node of the flamegraph: a function frame + how many samples passed
/// through it, and the callees below it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlameNode {
    pub name: String,
    pub count: usize,
    pub children: Vec<FlameNode>,
}

impl FlameNode {
    fn child_mut(&mut self, name: &str) -> &mut FlameNode {
        if let Some(i) = self.children.iter().position(|c| c.name == name) {
            &mut self.children[i]
        } else {
            self.children.push(FlameNode {
                name: name.to_owned(),
                count: 0,
                children: Vec::new(),
            });
            self.children.last_mut().unwrap()
        }
    }
}

/// Fold samples into a flame tree. Each sample is a call stack **outermost
/// first** (`["main", "loop", "read"]`), so the root's direct children are the
/// entry points. The synthetic root is named "all"; its count = sample total.
/// Children are sorted by descending count so the hot path is leftmost.
pub fn build_tree(samples: &[Vec<String>]) -> FlameNode {
    let mut root = FlameNode {
        name: "all".to_owned(),
        count: 0,
        children: Vec::new(),
    };
    for stack in samples {
        root.count += 1;
        let mut node = &mut root;
        for frame in stack {
            node = node.child_mut(frame);
            node.count += 1;
        }
    }
    sort_desc(&mut root);
    root
}

fn sort_desc(node: &mut FlameNode) {
    node.children
        .sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    for c in &mut node.children {
        sort_desc(c);
    }
}

// ── Per-line statistics (pure, testable) ──────────────────────────────────────

/// One frame of a sampled stack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// The function as probe-rs names it (an inlined one ends in
    /// ` #[inline]`), minus the PC it appends to a frame it found only in the
    /// symbol table: one function is one name, not one per instruction.
    pub name: String,
    /// The frame's source lies inside the project, whether or not the DWARF
    /// gives it a line.
    pub project: bool,
    /// `(project-relative path, 1-based line)` when the frame is in the
    /// project AND has a line (`src/main.rs`, `mylib/src/lib.rs`).
    pub at: Option<(String, u32)>,
    /// The core sleeps here, waiting for an interrupt: a `wfi`/`wfe`/`waiti`
    /// outside the project, or the embassy executor's own sleep.
    pub waits: bool,
    /// probe-rs's marker where its Cortex-M unwinder crossed an exception
    /// entry: every frame outside it is the code the interrupt preempted.
    pub exception: bool,
}

/// What one source line cost over a sampling run.
#[derive(Clone, Debug, PartialEq)]
pub struct LineStat {
    pub file: String,
    pub line: u32,
    /// The function the line was sampled in (its frame's name).
    pub func: String,
    /// The line as this run compiled it, trimmed. Empty when unknown.
    pub code: String,
    /// Samples whose innermost PROJECT frame stood on this line: the line's own
    /// time, the library code it calls included. A HAL `write` it waits in is
    /// this line's cost; another line of the project that it calls is not.
    pub self_n: usize,
    /// Samples with this line anywhere on the stack, once per sample however
    /// often a recursion repeats it.
    pub total_n: usize,
}

/// A run's samples, counted per source line.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ByLine {
    /// Hottest first: by `self_n`, then `total_n`.
    pub lines: Vec<LineStat>,
    /// Samples that caught the core waiting for an interrupt.
    pub idle: usize,
    /// Samples with no project frame on the stack, idle excluded: an
    /// interrupt handler inside the HAL, the executor, the ROM, a radio blob.
    pub outside: usize,
    /// The innermost function of those `outside` samples, most frequent
    /// first, at most [`OUTSIDE_FNS`] of them.
    pub outside_fns: Vec<(String, usize)>,
    /// Samples whose innermost project frame has no line: optimised code on
    /// a DWARF "line 0" row. Never the CALLER's time, so counted apart.
    pub no_line: usize,
    /// The functions of those `no_line` samples, like `outside_fns`.
    pub no_line_fns: Vec<(String, usize)>,
    pub samples: usize,
}

/// How many functions [`ByLine::outside_fns`] and [`ByLine::no_line_fns`]
/// keep.
pub const OUTSIDE_FNS: usize = 8;

/// One standard error of a share of `k` in `n` samples, in percentage points:
/// `√(p(1−p)/n)`. About two runs in three land within it of the true share;
/// twice it covers ~95%.
pub fn std_err_pct(k: usize, n: usize) -> f32 {
    if n == 0 {
        return 0.0;
    }
    let p = k as f32 / n as f32;
    (p * (1.0 - p) / n as f32).sqrt() * 100.0
}

/// A frame's function for counting: without probe-rs's ` #[inline]` mark, so
/// an inlined copy and the real one are one row.
fn fn_key(name: &str) -> &str {
    name.strip_suffix(" #[inline]").unwrap_or(name)
}

/// The bare function name: no ` #[inline]`, no generics, no path. probe-rs
/// sends the DWARF name (`wfi`, `run<F>`), not `cortex_m::asm::wfi`; the
/// path is cut anyway in case a name ever carries one.
fn base_name(name: &str) -> &str {
    let base = fn_key(name).split('<').next().unwrap_or_default();
    base.rsplit("::").next().unwrap_or_default().trim()
}

/// The core waiting for an interrupt, by the function's own name. Only ever
/// asked of a frame outside the project: a function of the user's that
/// happens to be called `wfi` is real work.
fn waits_by_name(name: &str) -> bool {
    matches!(
        base_name(name),
        "wfi" | "__wfi" | "wfe" | "__wfe" | "waiti" | "wait_for_interrupt"
    )
}

/// The embassy executor's sleep. On Cortex-M and RISC-V its `run` loop
/// executes `wfe` / `wfi` as INLINE assembly, so there is no wait function to
/// recognise: the PC rests in `run` itself, in the executor's arch file.
fn executor_sleeps(name: &str, path: Option<&str>) -> bool {
    let Some(path) = path else {
        return false;
    };
    let p = path.replace('\\', "/");
    base_name(name) == "run"
        && p.contains("/embassy-executor-")
        && (p.ends_with("/cortex_m.rs") || p.ends_with("/riscv32.rs"))
}

/// probe-rs's name for the frame where its Cortex-M unwinder crossed an
/// exception entry (`exception_handling::*::expanded_description`). That frame
/// has no source and no PC suffix; the frames outside it belong to the
/// preempted code.
fn is_exception_entry(raw_name: &str) -> bool {
    raw_name.starts_with("External interrupt #")
        || matches!(
            raw_name,
            "SysTick" | "PendSV" | "SVC" | "NMI" | "DebugMonitor" | "Reset"
        )
        || [
            "HardFault",
            "MemManage Fault",
            "BusFault",
            "UsageFault",
            "SecureFault",
        ]
        .iter()
        .any(|p| raw_name.starts_with(p))
}

fn line_stat(file: &str, line: u32, func: &str) -> LineStat {
    LineStat {
        file: file.to_owned(),
        line,
        func: func.to_owned(),
        code: String::new(),
        self_n: 0,
        total_n: 0,
    }
}

/// The most frequent functions of a count, at most [`OUTSIDE_FNS`].
fn top_fns(counts: HashMap<&str, usize>) -> Vec<(String, usize)> {
    let mut fns: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(name, n)| (name.to_owned(), n))
        .collect();
    fns.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    fns.truncate(OUTSIDE_FNS);
    fns
}

/// Count `samples` (each a call stack OUTERMOST first) per source line.
///
/// Every sample lands in exactly one of: a line's `self_n`, `idle`,
/// `outside`, `no_line` - so those always add up to `samples`. `total_n`
/// overlaps by design: the `loop` line of `main` is on nearly every stack.
///
/// Only the innermost CONTEXT of a sample counts. On Cortex-M probe-rs unwinds
/// through an exception entry into the code the interrupt preempted; none of
/// the handler's time is that code's, for `self_n` or `total_n`.
pub fn by_line(samples: &[Vec<Frame>]) -> ByLine {
    let mut out = ByLine {
        samples: samples.len(),
        ..ByLine::default()
    };
    let mut stats: HashMap<(&str, u32), LineStat> = HashMap::new();
    let mut outside: HashMap<&str, usize> = HashMap::new();
    let mut no_line: HashMap<&str, usize> = HashMap::new();
    for stack in samples {
        let own = match stack.iter().rposition(|f| f.exception) {
            Some(i) => &stack[i + 1..],
            None => &stack[..],
        };
        let mut seen: HashSet<(&str, u32)> = HashSet::new();
        for f in own {
            if let Some((file, line)) = &f.at
                && seen.insert((file.as_str(), *line))
            {
                stats
                    .entry((file.as_str(), *line))
                    .or_insert_with(|| line_stat(file, *line, &f.name))
                    .total_n += 1;
            }
        }
        // Idle first: the sleep is reached from `main`'s loop, or from the
        // line of `#[embassy_executor::main]`, which would otherwise look like
        // the hottest code in the firmware.
        if stack.last().is_some_and(|f| f.waits) {
            out.idle += 1;
            continue;
        }
        match own.iter().rev().find(|f| f.project) {
            Some(f) => match &f.at {
                Some((file, line)) => {
                    stats
                        .entry((file.as_str(), *line))
                        .or_insert_with(|| line_stat(file, *line, &f.name))
                        .self_n += 1;
                }
                None => {
                    out.no_line += 1;
                    *no_line.entry(fn_key(&f.name)).or_default() += 1;
                }
            },
            None => {
                out.outside += 1;
                if let Some(f) = stack.last() {
                    *outside.entry(fn_key(&f.name)).or_default() += 1;
                }
            }
        }
    }
    let mut lines: Vec<LineStat> = stats.into_values().collect();
    lines.sort_by(|a, b| {
        b.self_n
            .cmp(&a.self_n)
            .then(b.total_n.cmp(&a.total_n))
            .then_with(|| a.file.cmp(&b.file))
            .then(a.line.cmp(&b.line))
    });
    out.lines = lines;
    out.outside_fns = top_fns(outside);
    out.no_line_fns = top_fns(no_line);
    out
}

/// Give each line its text, from the files as the run compiled them.
fn fill_code(by: &mut ByLine, texts: &HashMap<String, String>) {
    for l in &mut by.lines {
        if let Some(text) = texts.get(&l.file) {
            l.code = text
                .lines()
                .nth(l.line as usize - 1)
                .unwrap_or_default()
                .trim()
                .to_owned();
        }
    }
}

/// `path` — a frame's source as probe-rs reports it — relative to the project
/// `root`, `/`-separated, when the file belongs to the project. Build output
/// under `target/` (a build script's generated code) does not.
fn project_rel(path: &str, root: &Path) -> Option<String> {
    let norm = |s: &str| s.replace('\\', "/").trim_start_matches("//?/").to_owned();
    let p = norm(path);
    let absolute = p.starts_with('/') || p.get(1..2) == Some(":");
    let rel = if absolute {
        let root = norm(&root.to_string_lossy());
        let prefix = format!("{}/", root.trim_end_matches('/'));
        // Case-insensitive: Windows drive letters and temp dirs differ in case
        // between the path we wrote to and the one the compiler recorded.
        let head = p.get(..prefix.len())?;
        if !head.eq_ignore_ascii_case(&prefix) {
            return None;
        }
        p[prefix.len()..].to_owned()
    } else {
        // cargo records a workspace crate's files relative to the workspace.
        p
    };
    let rel = rel.trim_start_matches("./");
    (!rel.is_empty() && !rel.starts_with("../") && !rel.starts_with("target/"))
        .then(|| rel.to_owned())
}

// ── State ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct FlameResult {
    pub root: FlameNode,
    pub by_line: ByLine,
    /// Every `.rs` file's text as this run compiled it, keyed like the sampled
    /// lines (`src/main.rs`). A file whose live text differs has been edited
    /// since, and its line numbers may no longer point at the code they were
    /// sampled on. Shared, because the UI clones the state every frame.
    pub texts: Arc<HashMap<String, String>>,
    pub samples: usize,
}

/// Fold a run's samples into both views.
fn result_of(samples: &[Vec<Frame>], texts: HashMap<String, String>) -> FlameResult {
    let names: Vec<Vec<String>> = samples
        .iter()
        .map(|s| s.iter().map(|f| f.name.clone()).collect())
        .collect();
    let mut by = by_line(samples);
    fill_code(&mut by, &texts);
    FlameResult {
        root: build_tree(&names),
        by_line: by,
        texts: Arc::new(texts),
        samples: samples.len(),
    }
}

#[derive(Clone, Debug, Default)]
pub enum FlameState {
    #[default]
    Idle,
    /// `cargo build --release` (to get the ELF + symbols).
    Building,
    /// Sampling in progress: `(collected, target)`.
    Sampling(usize, usize),
    Done(FlameResult),
    Failed(String),
}

impl FlameState {
    pub fn is_busy(&self) -> bool {
        matches!(self, FlameState::Building | FlameState::Sampling(..))
    }
}

// ── Runner ────────────────────────────────────────────────────────────────────

/// Build `--release`, attach via `probe-rs dap-server`, and collect `n_samples`
/// stack samples on a background thread. The firmware must already be running on
/// the target (attach, no flash). Result / progress land in `state`.
#[allow(clippy::too_many_arguments)]
pub fn start_flame(
    project_dir: PathBuf,
    target: String,
    chip: String,
    probe: Option<String>,
    // Which probes could drive this chip at all - see `probe::pick_probe`.
    toolchain: crate::panels::mcu_module::ToolchainKind,
    n_samples: usize,
    // Every `.rs` file's text as written for this build (`src/main.rs` →
    // text): the code column of the by-line table, and the reference the UI
    // compares the live files against.
    texts: HashMap<String, String>,
    state: Arc<Mutex<FlameState>>,
    ctx: eframe::egui::Context,
) {
    if state.lock().unwrap().is_busy() {
        return;
    }
    *state.lock().unwrap() = FlameState::Building;
    ctx.request_repaint();
    thread::spawn(move || {
        let next = run(
            &project_dir,
            &target,
            &chip,
            probe.as_deref(),
            &toolchain,
            n_samples,
            texts,
            &state,
            &ctx,
        );
        if let Err(e) = &next {
            *state.lock().unwrap() = FlameState::Failed(e.clone());
        } else if let Ok(res) = next {
            *state.lock().unwrap() = FlameState::Done(res);
        }
        ctx.request_repaint();
    });
}

#[allow(clippy::too_many_arguments)]
fn run(
    project_dir: &std::path::Path,
    target: &str,
    chip: &str,
    probe: Option<&str>,
    toolchain: &crate::panels::mcu_module::ToolchainKind,
    n_samples: usize,
    texts: HashMap<String, String>,
    state: &Arc<Mutex<FlameState>>,
    ctx: &eframe::egui::Context,
) -> Result<FlameResult, String> {
    // Resolve the probe FIRST: `probe-rs dap-server` is non-interactive, so
    // with two probes attached and no `probe` field it refuses to choose and
    // the attach fails - as the bare word "cancelled", after a full release
    // build has already been paid for.
    let probe = match crate::probe::selector(probe) {
        Some(sel) => sel,
        None => crate::probe::pick_probe(&crate::probe::list_probes()?, toolchain)?,
    };

    let elf = build_elf(project_dir, target)?;

    // Cortex-M caller frames need their lines looked up again only where this
    // probe-rs reports them at the return address - see `relocate_callers`.
    let fix_callers = target.starts_with("thumb")
        && no_window(&mut Command::new("probe-rs"))
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| crate::required_tools::parse_version(&String::from_utf8_lossy(&o.stdout)))
            .is_some_and(|v| reports_return_address(&v));

    // Spawn the DAP server.
    let port = free_port();
    let mut server = no_window(&mut Command::new("probe-rs"))
        .current_dir(project_dir)
        // `--single-session`: without it probe-rs stays in multi-session mode
        // and keeps listening after the client disconnects, so it never exits
        // on its own and the only way out is a kill - and a killed probe-rs
        // never detaches the probe. `debugger.rs` learned this on the ST-Link,
        // which stayed in debug mode until a replug; the ESP32-C3 answers the
        // same treatment with "Timeout during DMI access" on every attach
        // afterwards.
        .args([
            "dap-server",
            "--single-session",
            "--port",
            &port.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Capture stderr: probe-rs logs the REAL reason an attach fails here
        // (no probe, probe busy, unknown chip, target not responding …).
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "probe-rs not found in PATH (cargo install probe-rs-tools)".to_string()
            } else {
                format!("could not launch probe-rs dap-server: {e}")
            }
        })?;
    // Drain the dap-server's stderr on a background thread (capped) so a chatty
    // server can never fill the pipe buffer and block the sampling loop; the tail
    // is surfaced if the run fails.
    let log = Arc::new(Mutex::new(String::new()));
    let drain = server.stderr.take().map(|mut se| {
        let log = log.clone();
        thread::spawn(move || {
            let mut buf = [0u8; 1024];
            while let Ok(n) = se.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut l = log.lock().unwrap();
                l.push_str(&String::from_utf8_lossy(&buf[..n]));
                if l.len() > 8192 {
                    let cut = l.len() - 8192;
                    *l = l.split_off(cut);
                }
            }
        })
    });
    // Kill the server whatever happens next.
    // probe-rs writes its own diagnostics to the DAP console, not to stderr
    // ("Log output ... will be written to the Debug Console"), so the session
    // collects them here - that is where the reason for a failure lives.
    let console = Arc::new(Mutex::new(Vec::<String>::new()));
    let result = sample_over_dap(
        port,
        &elf,
        project_dir,
        fix_callers,
        chip,
        &probe,
        &console,
        n_samples,
        state,
        ctx,
    );
    // `sample_over_dap` has dropped its socket by the time it returns, which
    // ends the session on the server's side too - including the failure paths,
    // which never get as far as sending `disconnect`. Let it unwind before the
    // kill rather than cutting it off mid-teardown: see `wait_or_kill`.
    wait_or_kill(&mut server, Duration::from_secs(2));
    if let Some(h) = drain {
        let _ = h.join(); // kill → stderr EOF → drain exits; ensures full capture
    }
    // On failure, enrich the terse DAP error with the dap-server's own log tail
    // (probe-rs logs the real reason there).
    let samples = result.map_err(|e| {
        let log = log.lock().unwrap();
        let console = console.lock().unwrap();
        // The console first: it carries the sentence, stderr only the banner.
        let console_tail = tail_of(console.iter().map(String::as_str), 6);
        let server_tail = tail_of(log.lines(), 4);
        // A probe that enumerates but won't open, or a probe-rs crash, has an
        // explanation card with the two or three things that actually fix it —
        // worth far more than the raw chain. Everything probe-rs said is fed to
        // the taggers, because the sentence arrives on the console, not in `e`.
        let everything = format!(
            "{e}\n{}\n{}",
            console_tail.join("\n"),
            server_tail.join("\n")
        );
        if let Some(tagged) = crate::failure_hint::probe_rs_panic(&everything)
            .map(|d| crate::failure_hint::probe_rs_panic_message(&d))
            .or_else(|| {
                crate::failure_hint::probe_open_failure(&everything).map(|d| {
                    crate::failure_hint::probe_open_message(
                        &d,
                        crate::probe::missing_device_interface_guid(Some(&probe)),
                    )
                })
            })
        {
            return tagged;
        }
        let mut out = e;
        for (title, lines) in [
            ("probe-rs console:", console_tail),
            ("probe-rs dap-server:", server_tail),
        ] {
            if !lines.is_empty() {
                out = format!("{out}\n\n{title}\n{}", lines.join("\n"));
            }
        }
        out
    })?;
    Ok(result_of(&samples, texts))
}

/// Wait up to `grace` for the dap-server to exit by itself, and only then kill
/// it. A debugger killed mid-teardown leaves the target's debug module still
/// configured - on a RISC-V ESP32-C3 that shows up as probe-rs warning "Could
/// not clear all hardware breakpoints", after which EVERY later attach dies
/// with "Timeout during DMI access" until the board is power-cycled. The wait
/// costs nothing in the normal case, where the server is already gone.
fn wait_or_kill(server: &mut std::process::Child, grace: Duration) {
    // The server is started with `--single-session`, so a finished session is
    // what makes it leave; this only has to outlast probe-rs's own shutdown.
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        match server.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => break,
        }
    }
    let _ = server.kill();
    let _ = server.wait();
}

/// The last `n` non-empty lines, trimmed and back in order — both failure
/// tails want this and neither wants the blank lines probe-rs pads with.
fn tail_of<'a>(lines: impl Iterator<Item = &'a str>, n: usize) -> Vec<&'a str> {
    let mut kept: Vec<&str> = lines
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take(n)
        .collect();
    kept.reverse();
    kept
}

/// `cargo build --release --message-format=json`, return the ELF path.
fn build_elf(project_dir: &std::path::Path, target: &str) -> Result<PathBuf, String> {
    let out = no_window(&mut Command::new("cargo"))
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
        .output()
        .map_err(|e| format!("could not launch cargo: {e}"))?;
    let mut elf: Option<PathBuf> = None;
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"] == "compiler-artifact" {
            if let Some(exe) = v["executable"].as_str() {
                elf = Some(PathBuf::from(exe));
            }
        }
    }
    if !out.status.success() {
        return Err(format!(
            "release build failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    elf.ok_or_else(|| "build produced no executable (nothing to profile)".to_string())
}

/// Connect to the DAP server, attach, and run the pause/stackTrace/continue
/// sampling loop.
#[allow(clippy::too_many_arguments)]
fn sample_over_dap(
    port: u16,
    elf: &std::path::Path,
    // The project the ELF was built from: frames whose source lies under it
    // are the project's own lines.
    project_dir: &Path,
    // Whether Cortex-M caller frames are re-resolved one byte back - see
    // `relocate_callers` and `reports_return_address`.
    fix_callers: bool,
    chip: &str,
    // Already resolved to ONE probe by `run` - never "let probe-rs choose".
    probe: &str,
    console: &Arc<Mutex<Vec<String>>>,
    n_samples: usize,
    state: &Arc<Mutex<FlameState>>,
    ctx: &eframe::egui::Context,
) -> Result<Vec<Vec<Frame>>, String> {
    // Read before the session starts. Without it (not Cortex-M, a probe-rs
    // that already steps back, no DWARF, unreadable ELF) the frames keep
    // probe-rs's lines.
    let elf_bytes = fix_callers.then(|| std::fs::read(elf).ok()).flatten();
    let mut elf_lines = elf_bytes.as_deref().and_then(ElfLines::new);
    let mut lookup = |addr: u64| {
        elf_lines
            .as_mut()
            .map_or_else(Chain::new, |l| l.chain(addr))
    };

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = None;
    for _ in 0..50 {
        if let Ok(s) = TcpStream::connect_timeout(&addr, Duration::from_millis(200)) {
            stream = Some(s);
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let mut stream = stream.ok_or("could not connect to probe-rs dap-server")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(6)))
        .map_err(|e| e.to_string())?;
    let mut seq = 1i64;

    // Handshake: initialize → launch (ATTACH: no flash) → configurationDone.
    send(
        &mut stream,
        &mut seq,
        "initialize",
        json!({"adapterID": "probe-rs"}),
    )?;
    wait_response(&mut stream, "initialize", console)?;
    let launch = json!({
        "chip": chip,
        "probe": probe,
        "connectUnderReset": false,
        "flashingConfig": { "flashingEnabled": false },
        "coreConfigs": [{ "coreIndex": 0, "programBinary": elf.to_string_lossy() }],
        // Without this probe-rs stays at its default level and says less about
        // why an attach failed.
        "consoleLogLevel": "Console",
    });
    send(&mut stream, &mut seq, "attach", launch)?;
    // The `initialized` event tells us to finish configuration.
    wait_event(&mut stream, "initialized", console)?;
    send(&mut stream, &mut seq, "configurationDone", json!({}))?;

    *state.lock().unwrap() = FlameState::Sampling(0, n_samples);
    ctx.request_repaint();

    let mut samples: Vec<Vec<Frame>> = Vec::with_capacity(n_samples);
    let mut thread_id = 0i64;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    for i in 0..n_samples {
        // Give the target a moment to run between samples (spreads the samples,
        // reduces "always halted in the same spot" bias from back-to-back halts).
        thread::sleep(gap(i, seed));
        send(
            &mut stream,
            &mut seq,
            "pause",
            json!({"threadId": thread_id.max(1)}),
        )?;
        let stopped = wait_event(&mut stream, "stopped", console)?;
        if let Some(t) = stopped["body"]["threadId"].as_i64() {
            thread_id = t;
        }
        send(
            &mut stream,
            &mut seq,
            "stackTrace",
            // `levels: 0` = every frame. probe-rs has unwound the whole stack
            // at the halt anyway, and a cap drops the OUTERMOST frames - the
            // `main` loop line, or every project frame of a deep interrupt.
            json!({"threadId": thread_id.max(1), "startFrame": 0, "levels": 0}),
        )?;
        let st = wait_response(&mut stream, "stackTrace", console)?;
        if let Some(stack) = frames_of(&st, project_dir, &mut lookup)
            && !stack.is_empty()
        {
            samples.push(stack);
        }
        send(
            &mut stream,
            &mut seq,
            "continue",
            json!({"threadId": thread_id.max(1)}),
        )?;

        if i % 4 == 0 {
            *state.lock().unwrap() = FlameState::Sampling(samples.len(), n_samples);
            ctx.request_repaint();
        }
    }
    let _ = send(&mut stream, &mut seq, "disconnect", json!({}));
    // Wait for the answer instead of firing and forgetting: probe-rs
    // deconfigures the target while it handles `disconnect` - clears the
    // hardware breakpoints it set and lets the core run again. The process is
    // killed immediately after this returns, so not waiting here is what makes
    // the kill land in the middle of that.
    let _ = wait_response(&mut stream, "disconnect", console);

    if samples.is_empty() {
        return Err(
            "no stack samples — is the firmware flashed and running, and the ELF matching?"
                .to_string(),
        );
    }
    Ok(samples)
}

/// The spacing before pause number `i`: 4..=20 ms, different every time. A
/// constant gap can fall into step with a periodic task and catch it at the
/// same phase on every sample - always inside its handler, or never. The
/// round trip wanders by milliseconds on a slow probe, which hides that there;
/// a fast probe would not.
fn gap(i: usize, seed: u64) -> Duration {
    // splitmix64: no `rand` dependency for seventeen values.
    let mut x = seed.wrapping_add((i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    Duration::from_millis(4 + x % 17)
}

/// One function of an inline chain at an address, as the ELF's DWARF has it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Site {
    path: Option<String>,
    line: Option<u32>,
    /// The function, demangled and cut to its bare name the way probe-rs
    /// names a frame (`panic_fmt`, `{closure#0}`).
    func: Option<String>,
}

/// An inline chain, innermost first, as [`ElfLines::chain`] returns it.
type Chain = Vec<Site>;

/// Whether this probe-rs places a Cortex-M caller frame at the return
/// address itself: up to 0.29 it does (LR & !1). 0.32 already steps back into
/// the call ((LR - 2) & !1), and a second step back would land BEFORE a 16-bit
/// `blx` - on whatever set up its target. 0.30 and 0.31 are unchecked and
/// treated like 0.32, and so is a version that does not parse.
fn reports_return_address(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    matches!(
        (parts.next().flatten(), parts.next().flatten()),
        (Some(0), Some(minor)) if minor <= 29
    )
}

/// `core::panicking::panic_fmt` -> `panic_fmt`,
/// `<Radar<D> as Sensor>::read::<u8>` -> `read`: generic arguments dropped
/// (nested ones included), then the last path segment.
fn bare_fn(demangled: &str) -> String {
    let mut depth = 0usize;
    let mut flat = String::with_capacity(demangled.len());
    for c in demangled.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => flat.push(c),
            _ => {}
        }
    }
    flat.trim_end_matches(':')
        .rsplit("::")
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// One DAP stack frame, as sent, before it is classified.
struct RawFrame<'a> {
    name: &'a str,
    /// probe-rs marks an inlined function's frame with a ` #[inline]` suffix.
    inline: bool,
    path: Option<&'a str>,
    /// 1-based; DAP's 0 ("no line") is `None`.
    line: Option<u32>,
    /// `instructionPointerReference`, which probe-rs writes as `0x08000abc`.
    pc: Option<u64>,
}

fn parse_pc(s: &str) -> Option<u64> {
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None => s.parse().ok(),
    }
}

/// A frame name without what probe-rs appends that is not the function: the
/// PC of a frame it found only in the symbol table (`foo @ 0x42001234>`,
/// `<unknown function @ 0x40000054>`) and an unwind error (`foo : ERROR: …`).
/// ` #[inline]` stays: the flamegraph shows it.
fn clean_name(raw: &str) -> String {
    let (body, inline) = match raw.strip_suffix(" #[inline]") {
        Some(b) => (b, " #[inline]"),
        None => (raw, ""),
    };
    let body = body.split(" : ERROR").next().unwrap_or(body);
    let body = if body.starts_with("<unknown function @ ") {
        "<unknown function>"
    } else {
        match body.rfind(" @ 0x") {
            Some(i) if body.ends_with('>') => &body[..i],
            _ => body,
        }
    };
    format!("{body}{inline}")
}

/// The frames of a `stackTrace` response, OUTERMOST first (reversed from the
/// DAP order, which is innermost first), skipping unnamed frames. Each is
/// classified here, where its raw source path is still at hand.
///
/// `lookup` gives the inline chain at an address from the ELF's own line
/// table; it re-resolves Cortex-M caller frames (see [`relocate_callers`]).
/// Elsewhere it answers nothing and the frames keep probe-rs's lines.
fn frames_of(
    resp: &Value,
    project_dir: &Path,
    lookup: &mut dyn FnMut(u64) -> Chain,
) -> Option<Vec<Frame>> {
    let arr = resp["body"]["stackFrames"].as_array()?;
    let raw: Vec<RawFrame> = arr
        .iter()
        .filter_map(|f| {
            let name = f["name"]
                .as_str()
                .filter(|n| !n.is_empty() && *n != "<unknown>")?;
            Some(RawFrame {
                name,
                inline: name.ends_with(" #[inline]"),
                path: f["source"]["path"].as_str(),
                line: f["line"]
                    .as_u64()
                    .and_then(|l| u32::try_from(l).ok())
                    .filter(|&l| l > 0),
                pc: f["instructionPointerReference"].as_str().and_then(parse_pc),
            })
        })
        .collect();
    let frames: Vec<Frame> = raw
        .iter()
        .map(|r| {
            let file = r.path.and_then(|p| project_rel(p, project_dir));
            let project = file.is_some();
            let name = clean_name(r.name);
            Frame {
                waits: !project && (waits_by_name(&name) || executor_sleeps(&name, r.path)),
                exception: r.path.is_none() && !r.inline && is_exception_entry(r.name),
                at: file.zip(r.line),
                project,
                name,
            }
        })
        .collect();
    let mut frames = relocate_callers(&raw, frames, project_dir, lookup);
    frames.reverse();
    Some(frames)
}

/// Put Cortex-M caller frames back on the line of their CALL.
///
/// probe-rs 0.29 gives a caller frame the return address as its PC (LR with
/// bit 0 cleared) and looks the frame's lines and inlined functions up there:
/// on the instruction AFTER the call, which is the next statement, or the loop
/// head on a back edge. The time a line waits inside a HAL call that is not
/// inlined then lands on the line below it - 42 of 71 call sites of a real
/// STM32F103 release build. Each such physical frame is looked up again one
/// byte back, inside the call instruction. Only run where
/// [`reports_return_address`] says so.
///
/// Left alone: the innermost physical frame (its PC is the real one), and the
/// one right outside an exception entry (its PC was stacked by the exception:
/// real as well).
fn relocate_callers(
    raw: &[RawFrame],
    frames: Vec<Frame>,
    root: &Path,
    lookup: &mut dyn FnMut(u64) -> Chain,
) -> Vec<Frame> {
    let mut out = Vec::with_capacity(frames.len());
    let mut frames = frames.into_iter();
    let mut pc_is_real = true;
    let mut start = 0;
    while start < raw.len() {
        // probe-rs lists a PC's inlined functions first, then the function
        // they were inlined into - that one closes the physical frame.
        let end = raw[start..]
            .iter()
            .position(|r| !r.inline)
            .map_or(raw.len(), |k| start + k + 1);
        let mut group: Vec<Frame> = frames.by_ref().take(end - start).collect();
        let exception = group.last().is_some_and(|f| f.exception);
        if !pc_is_real
            && !exception
            && let Some(pc) = raw[start].pc.filter(|&pc| pc > 0)
        {
            let chain = lookup(pc - 1);
            if !chain.is_empty() {
                // The return address is the first instruction of ANOTHER
                // function: the call was the caller's last instruction and
                // never returns, and probe-rs named the frame after whatever
                // the linker put next (cortex-m-rt's `main` trampoline, then
                // `panic_fmt`).
                let outer = |c: &Chain| c.last().and_then(|s| s.func.clone());
                let moved = matches!(
                    (outer(&chain), outer(&lookup(pc))),
                    (Some(call), Some(here)) if call != here
                );
                if chain.iter().all(|s| s.func.is_some()) {
                    // Names AND lines from the call's own chain: probe-rs named
                    // the inlined frames after the code that runs AFTER the
                    // call (`next` of the loop head instead of the closure
                    // that called). The physical function keeps probe-rs's
                    // name, unless the return address lies in another one.
                    let physical = group.pop().map(|f| f.name).filter(|_| !moved);
                    group = rebuilt(&chain, root);
                    if let (Some(name), Some(f)) = (physical, group.last_mut()) {
                        f.name = name;
                    }
                } else {
                    realign(&mut group, &chain, root);
                }
            }
        }
        out.extend(group);
        pc_is_real = exception;
        start = end;
    }
    out
}

/// Give a caller's frames the lines of `chain`, its inline chain one byte
/// back, when the chain lacks a function name to rebuild them from. Lined up
/// from the OUTER end: the physical function is the same one byte back, while
/// the inlined ones need not be.
///
/// - Inlined at the return address but not at the call: code AFTER the call,
///   not running - those frames lose their line.
/// - Inlined at the call but gone at the return address (a call made from
///   inside an inlined wrapper): folded into the innermost frame that is left,
///   its innermost PROJECT line first, so the line the user wrote keeps the
///   time rather than the wrapper's.
fn realign(group: &mut [Frame], chain: &Chain, root: &Path) {
    let (g, c) = (group.len(), chain.len());
    let n = g.min(c);
    for f in &mut group[..g - n] {
        f.project = false;
        f.at = None;
    }
    let in_project = |s: &Site| {
        s.path
            .as_deref()
            .and_then(|p| project_rel(p, root))
            .is_some()
    };
    let fold = chain[..=c - n]
        .iter()
        .find(|s| in_project(s))
        .unwrap_or(&chain[c - n]);
    for (k, f) in group[g - n..].iter_mut().enumerate() {
        let site = if k == 0 { fold } else { &chain[c - n + k] };
        if let Some(path) = &site.path {
            let file = project_rel(path, root);
            f.project = file.is_some();
            f.at = file.zip(site.line);
        }
    }
}

/// The frames of `chain`, names included: what the caller was doing at the
/// call, one frame per function of its inline chain.
fn rebuilt(chain: &Chain, root: &Path) -> Vec<Frame> {
    chain
        .iter()
        .enumerate()
        .map(|(k, s)| {
            let file = s.path.as_deref().and_then(|p| project_rel(p, root));
            let func = s.func.clone().unwrap_or_default();
            Frame {
                // Every function but the outermost was inlined into it.
                name: if k + 1 < chain.len() {
                    format!("{func} #[inline]")
                } else {
                    func
                },
                project: file.is_some(),
                at: file.zip(s.line),
                ..Frame::default()
            }
        })
        .collect()
}

/// The release ELF's line table, for [`relocate_callers`]. Built once per run
/// and only for Cortex-M; answers are cached per address, since the same few
/// call sites come back on every sample.
struct ElfLines<'a> {
    ctx: addr2line::Context<gimli::EndianSlice<'a, gimli::RunTimeEndian>>,
    cache: HashMap<u64, Chain>,
}

impl<'a> ElfLines<'a> {
    /// `None` for anything that is not an ELF with DWARF in it.
    fn new(data: &'a [u8]) -> Option<Self> {
        use object::{Object, ObjectSection};
        let obj = object::File::parse(data).ok()?;
        let endian = if obj.is_little_endian() {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };
        let dwarf = gimli::Dwarf::load(|id| -> Result<_, gimli::Error> {
            let bytes = obj
                .section_by_name(id.name())
                .and_then(|s| s.data().ok())
                .unwrap_or(&[]);
            Ok(gimli::EndianSlice::new(bytes, endian))
        })
        .ok()?;
        Some(Self {
            ctx: addr2line::Context::from_dwarf(dwarf).ok()?,
            cache: HashMap::new(),
        })
    }

    /// The inline chain at `addr`, innermost first; empty where the DWARF
    /// says nothing.
    fn chain(&mut self, addr: u64) -> Chain {
        if let Some(known) = self.cache.get(&addr) {
            return known.clone();
        }
        let mut out = Chain::new();
        if let Ok(mut frames) = self.ctx.find_frames(addr).skip_all_loads() {
            while let Ok(Some(f)) = frames.next() {
                let loc = f.location.as_ref();
                out.push(Site {
                    path: loc.and_then(|l| l.file).map(str::to_owned),
                    line: loc.and_then(|l| l.line),
                    func: f
                        .function
                        .as_ref()
                        .and_then(|n| n.demangle().ok())
                        .map(|d| bare_fn(&d)),
                });
            }
        }
        self.cache.insert(addr, out.clone());
        out
    }
}

// ── Minimal blocking DAP client ───────────────────────────────────────────────

fn send(
    stream: &mut TcpStream,
    seq: &mut i64,
    command: &str,
    arguments: Value,
) -> Result<(), String> {
    let msg = json!({"seq": *seq, "type": "request", "command": command, "arguments": arguments});
    *seq += 1;
    let body = msg.to_string();
    // ONE `write_all` for header + body, never `write!` with a format string:
    // that issues a syscall per piece, so the header can reach the server split
    // across TCP segments. probe-rs's dap-server (0.29) parses its two header
    // lines with `read_line` on a NON-blocking socket, and a partial line
    // desyncs that reader for good — it then drops the connection, which
    // arrives here as `dap-server closed waiting for 'initialized'`. Same fix
    // as `debugger.rs`'s `Wire::request`.
    let frame = format!("Content-Length: {}\r\n\r\n{body}", body.len());
    stream
        .write_all(frame.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())
}

/// Read one `Content-Length`-framed DAP message (`None` on EOF / timeout).
fn read_msg(stream: &mut TcpStream) -> Option<Value> {
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
    let len: usize = String::from_utf8_lossy(&header)
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .and_then(|v| v.trim().parse().ok())?;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Read messages until a successful RESPONSE to `command` arrives, keeping any
/// `output` event seen on the way (that is probe-rs's own log).
fn wait_response(
    stream: &mut TcpStream,
    command: &str,
    console: &Arc<Mutex<Vec<String>>>,
) -> Result<Value, String> {
    for _ in 0..200 {
        let msg =
            read_msg(stream).ok_or_else(|| format!("dap-server closed waiting for {command}"))?;
        collect_output(&msg, console);
        if msg["type"] == "response" && msg["command"] == command {
            if msg["success"].as_bool() == Some(false) {
                return Err(format!(
                    "{command}: {}",
                    crate::debugger::response_error(&msg)
                ));
            }
            return Ok(msg);
        }
    }
    Err(format!("no response to {command}"))
}

/// Park an `output` event's text in the session console.
fn collect_output(msg: &Value, console: &Arc<Mutex<Vec<String>>>) {
    if msg["type"] != "event" || msg["event"] != "output" {
        return;
    }
    let Some(text) = msg["body"]["output"].as_str() else {
        return;
    };
    let mut c = console.lock().unwrap();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        c.push(line.to_owned());
    }
    // A long session must not grow this without bound; only the tail is used.
    let len = c.len();
    if len > 200 {
        c.drain(..len - 200);
    }
}

/// Read messages until EVENT `event` arrives. A FAILED response to a pending
/// request (e.g. `attach` when the probe/target isn't there) is surfaced with
/// its message rather than skipped — otherwise the server just closes and the
/// real reason is lost as a bare "closed waiting for 'initialized'".
fn wait_event(
    stream: &mut TcpStream,
    event: &str,
    console: &Arc<Mutex<Vec<String>>>,
) -> Result<Value, String> {
    for _ in 0..200 {
        let msg =
            read_msg(stream).ok_or_else(|| format!("dap-server closed waiting for '{event}'"))?;
        collect_output(&msg, console);
        if msg["type"] == "event" && msg["event"] == event {
            return Ok(msg);
        }
        if msg["type"] == "response" && msg["success"].as_bool() == Some(false) {
            let cmd = msg["command"].as_str().unwrap_or("request");
            return Err(format!(
                "{cmd} failed: {}",
                crate::debugger::response_error(&msg)
            ));
        }
    }
    Err(format!("no '{event}' event"))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(4712)
}

#[cfg(test)]
mod tests {
    use super::*;

    // `wait_or_kill` is what keeps a probe-rs teardown from being cut in half;
    // both halves of its contract are worth pinning down.
    #[test]
    #[cfg(target_os = "windows")]
    fn a_server_that_is_already_gone_is_not_waited_for() {
        let mut child = Command::new("cmd")
            .args(["/C", "exit"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let started = std::time::Instant::now();
        wait_or_kill(&mut child, Duration::from_secs(10));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the grace period must end when the process does, not run out"
        );
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn a_server_that_will_not_leave_is_killed() {
        let mut child = Command::new("cmd")
            .args(["/C", "ping -n 20 127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        wait_or_kill(&mut child, Duration::from_millis(200));
        assert!(
            child.try_wait().unwrap().is_some(),
            "a server that outlives its grace period must still be killed"
        );
    }

    #[test]
    fn folds_stacks_into_a_tree_hot_path_first() {
        // main→loop→work (x3), main→loop→idle (x1), main→setup (x1).
        let samples = vec![
            vec!["main".into(), "loop".into(), "work".into()],
            vec!["main".into(), "loop".into(), "work".into()],
            vec!["main".into(), "loop".into(), "work".into()],
            vec!["main".into(), "loop".into(), "idle".into()],
            vec!["main".into(), "setup".into()],
        ];
        let root = build_tree(&samples);
        assert_eq!(root.name, "all");
        assert_eq!(root.count, 5);
        // one entry point: main, with all 5.
        assert_eq!(root.children.len(), 1);
        let main = &root.children[0];
        assert_eq!((main.name.as_str(), main.count), ("main", 5));
        // main's children sorted by count: loop (4) before setup (1).
        assert_eq!(main.children[0].name, "loop");
        assert_eq!(main.children[0].count, 4);
        assert_eq!(main.children[1].name, "setup");
        // loop→work is the hot leaf (3), before idle (1).
        let lp = &main.children[0];
        assert_eq!(
            (lp.children[0].name.as_str(), lp.children[0].count),
            ("work", 3)
        );
        assert_eq!(lp.children[1].name, "idle");
    }

    #[test]
    fn empty_input_is_an_empty_root() {
        let root = build_tree(&[]);
        assert_eq!(root.count, 0);
        assert!(root.children.is_empty());
    }

    fn at(name: &str, file: &str, line: u32) -> Frame {
        Frame {
            name: name.into(),
            project: true,
            at: Some((file.into(), line)),
            ..Frame::default()
        }
    }

    fn lib(name: &str) -> Frame {
        Frame {
            name: name.into(),
            ..Frame::default()
        }
    }

    /// A frame `frames_of` classified as the core waiting for an interrupt.
    fn asleep(name: &str) -> Frame {
        Frame {
            waits: true,
            ..lib(name)
        }
    }

    /// probe-rs's exception-entry frame.
    fn entry(name: &str) -> Frame {
        Frame {
            exception: true,
            ..lib(name)
        }
    }

    /// A project frame on a DWARF "line 0" row.
    fn no_line(name: &str) -> Frame {
        Frame {
            project: true,
            ..lib(name)
        }
    }

    fn stat<'a>(by: &'a ByLine, file: &str, line: u32) -> &'a LineStat {
        by.lines
            .iter()
            .find(|l| l.file == file && l.line == line)
            .unwrap_or_else(|| panic!("no row for {file}:{line}"))
    }

    fn buckets(by: &ByLine) -> usize {
        by.lines.iter().map(|l| l.self_n).sum::<usize>() + by.idle + by.outside + by.no_line
    }

    #[test]
    fn every_sample_lands_in_exactly_one_bucket() {
        let samples = vec![
            // main:10 calls work, which is busy on line 30.
            vec![at("main", "src/main.rs", 10), at("work", "src/main.rs", 30)],
            vec![at("main", "src/main.rs", 10), at("work", "src/main.rs", 30)],
            // main:12 waits inside the HAL: the HAL time is line 12's.
            vec![at("main", "src/main.rs", 12), lib("write")],
            // The idle loop, reached from main:14.
            vec![at("main", "src/main.rs", 14), asleep("wfi #[inline]")],
            // An interrupt with no project frame on the stack.
            vec![lib("handler"), lib("isr")],
            // Project code on a "line 0" row, called from main:16.
            vec![at("main", "src/main.rs", 16), no_line("work")],
        ];
        let by = by_line(&samples);
        assert_eq!(by.samples, 6);
        assert_eq!(buckets(&by), by.samples);
        assert_eq!(stat(&by, "src/main.rs", 30).self_n, 2);
        assert_eq!(stat(&by, "src/main.rs", 12).self_n, 1);
        assert_eq!((by.idle, by.outside, by.no_line), (1, 1, 1));
        // The caller's line is on the stack, but none of the time is its own.
        let ten = stat(&by, "src/main.rs", 10);
        assert_eq!((ten.self_n, ten.total_n), (0, 2));
        // The line the idle loop was reached from carries it as total only.
        let fourteen = stat(&by, "src/main.rs", 14);
        assert_eq!((fourteen.self_n, fourteen.total_n), (0, 1));
        // A project frame without a line is not its caller's time either.
        let sixteen = stat(&by, "src/main.rs", 16);
        assert_eq!((sixteen.self_n, sixteen.total_n), (0, 1));
        assert_eq!(by.outside_fns, vec![("isr".to_owned(), 1)]);
        assert_eq!(by.no_line_fns, vec![("work".to_owned(), 1)]);
        // Hottest first.
        assert_eq!(
            (by.lines[0].file.as_str(), by.lines[0].line),
            ("src/main.rs", 30)
        );
        assert_eq!(by.lines[0].func, "work");
    }

    #[test]
    fn a_recursive_line_counts_once_per_sample() {
        let samples = vec![vec![
            at("main", "src/main.rs", 5),
            at("walk", "src/tree.rs", 8),
            at("walk", "src/tree.rs", 8),
            at("walk", "src/tree.rs", 8),
        ]];
        let by = by_line(&samples);
        let walk = stat(&by, "src/tree.rs", 8);
        assert_eq!((walk.self_n, walk.total_n), (1, 1));
    }

    #[test]
    fn an_interrupt_is_not_the_time_of_the_code_it_preempted() {
        // Cortex-M: probe-rs unwinds through the exception entry into the code
        // the interrupt stopped. A HAL handler is Outside; a project handler
        // keeps its own line; the preempted lines get nothing at all.
        let samples = vec![
            vec![
                at("main", "src/main.rs", 10),
                lib("wfi"),
                entry("External interrupt #28"),
                lib("on_interrupt"),
            ],
            vec![
                at("main", "src/main.rs", 10),
                at("compute", "src/main.rs", 20),
                entry("SysTick"),
                at("SysTick", "src/main.rs", 50),
            ],
        ];
        let by = by_line(&samples);
        assert_eq!(by.outside, 1);
        assert_eq!(by.outside_fns, vec![("on_interrupt".to_owned(), 1)]);
        assert_eq!(stat(&by, "src/main.rs", 50).self_n, 1);
        for preempted in [10, 20] {
            assert!(
                !by.lines.iter().any(|l| l.line == preempted),
                "line {preempted}: {by:?}"
            );
        }
        assert_eq!(buckets(&by), 2);
    }

    #[test]
    fn inlined_copies_and_pcs_are_one_function() {
        let samples = vec![
            vec![lib("ppTask")],
            vec![lib("ppTask")],
            vec![lib("memcpy #[inline]")],
            vec![lib("memcpy")],
        ];
        let by = by_line(&samples);
        assert_eq!(
            by.outside_fns,
            vec![("memcpy".to_owned(), 2), ("ppTask".to_owned(), 2)]
        );
        // probe-rs's suffixes, as `frames_of` takes them off.
        assert_eq!(clean_name("ppTask @ 0x420012a4>"), "ppTask");
        assert_eq!(
            clean_name("<unknown function @ 0x40000054>"),
            "<unknown function>"
        );
        assert_eq!(clean_name("poll : ERROR: no CFA"), "poll");
        assert_eq!(clean_name("poll : ERROR : bad"), "poll");
        assert_eq!(clean_name("wfi #[inline]"), "wfi #[inline]");
        assert_eq!(clean_name("Vec<u8>::push"), "Vec<u8>::push");
    }

    /// One DAP frame as probe-rs 0.29 sends it.
    fn dap(name: &str, path: Option<&str>, line: u64, pc: u64) -> Value {
        let mut f = json!({"name": name, "line": line,
                           "instructionPointerReference": format!("{pc:#010x}")});
        if let Some(p) = path {
            f["source"] = json!({"path": p});
        }
        f
    }

    fn stack(frames: Vec<Value>) -> Value {
        json!({"body": {"stackFrames": frames}})
    }

    fn nothing(_: u64) -> Chain {
        Chain::new()
    }

    /// A file of a registry crate, as DWARF names it.
    fn reg(rel: &str) -> String {
        format!("/home/me/.cargo/registry/src/index.crates.io-1/{rel}")
    }

    #[test]
    fn what_probe_rs_names_a_sleep_is_idle() {
        let root = Path::new("/ws");
        let main = || {
            dap(
                "__cortex_m_rt_main",
                Some("/ws/src/main.rs"),
                21,
                0x0800_0100,
            )
        };
        let sleeps = [
            dap(
                "wfi #[inline]",
                Some(reg("cortex-m-0.7.7/src/asm.rs").as_str()),
                52,
                0x0800_0200,
            ),
            dap("__wfi @ 0x08000abc>", None, 0, 0x0800_0abc),
            dap(
                "wait_for_interrupt #[inline]",
                Some(reg("esp-hal-1.0.0/src/interrupt/riscv.rs").as_str()),
                9,
                0x4200_0010,
            ),
            dap(
                "waiti",
                Some(reg("xtensa-lx-0.11.0/src/lib.rs").as_str()),
                30,
                0x4200_0020,
            ),
            // embassy's executor sleeps in inline `wfe`, inside `run` itself.
            dap(
                "run<embassy_executor::raw::Executor>",
                Some(reg("embassy-executor-0.9.1/src/arch/cortex_m.rs").as_str()),
                106,
                0x0800_0300,
            ),
            dap(
                "run #[inline]",
                Some(
                    r"C:\Users\me\.cargo\registry\src\x\embassy-executor-0.10.0\src\platform\cortex_m.rs",
                ),
                106,
                0x0800_0300,
            ),
        ];
        for s in sleeps {
            let frames = frames_of(&stack(vec![s.clone(), main()]), root, &mut nothing).unwrap();
            assert!(frames.last().unwrap().waits, "{s}");
            assert!(!frames[0].waits);
        }
        let busy = [
            // The user's own `wfi` is work.
            dap("wfi", Some("/ws/src/main.rs"), 40, 0x0800_0400),
            dap(
                "wfi_count",
                Some(reg("x/src/lib.rs").as_str()),
                3,
                0x0800_0500,
            ),
            // Somebody else's `run`.
            dap(
                "run",
                Some(reg("tokio-1.0.0/src/rt.rs").as_str()),
                3,
                0x0800_0600,
            ),
        ];
        for b in busy {
            let frames = frames_of(&stack(vec![b.clone(), main()]), root, &mut nothing).unwrap();
            assert!(!frames.last().unwrap().waits, "{b}");
        }
    }

    #[test]
    fn stack_frames_keep_project_lines_outermost_first() {
        let root = Path::new("/ws");
        let resp = stack(vec![
            dap(
                "write",
                Some(reg("esp-hal/src/uart.rs").as_str()),
                210,
                0x4200_0100,
            ),
            dap("send", Some("/ws/src/main.rs"), 42, 0x4200_0200),
            json!({"name": "<unknown>", "line": 0}),
            dap("main", Some("/ws/src/main.rs"), 0, 0x4200_0300),
            dap("External interrupt #3", None, 0, 0x4200_0300),
        ]);
        let frames = frames_of(&resp, root, &mut nothing).unwrap();
        assert_eq!(
            frames,
            vec![
                entry("External interrupt #3"),
                // Line 0 = no line: still the project's own frame.
                no_line("main"),
                at("send", "src/main.rs", 42),
                lib("write"),
            ]
        );
    }

    fn site(path: &str, line: u32, func: &str) -> Site {
        Site {
            path: Some(path.to_owned()),
            line: Some(line),
            func: Some(func.to_owned()),
        }
    }

    /// What `by_line` charges: the innermost PROJECT frame's line; `None`
    /// when that frame has no line (`no_line`) or there is no project frame.
    fn charged(frames: &[Frame]) -> Option<(String, u32)> {
        frames.iter().rev().find(|f| f.project)?.at.clone()
    }

    /// The bare names of `frames`, outermost first.
    fn names(frames: &[Frame]) -> Vec<&str> {
        frames.iter().map(|f| f.name.as_str()).collect()
    }

    #[test]
    fn a_cortex_m_caller_is_put_back_on_its_call() {
        // probe-rs 0.29 reports `main` at the RETURN address, on line 12 (the
        // statement after the call). One byte back, the ELF says line 11.
        let root = Path::new("/ws");
        let mut asked = Vec::new();
        let mut lookup = |addr: u64| {
            asked.push(addr);
            match addr {
                0x0800_0123 => vec![site("/ws/src/main.rs", 11, "main")],
                // An inlined helper's call, one byte back: same chain of two.
                0x0800_0223 => vec![
                    site("/ws/src/drv.rs", 7, "send"),
                    site("/ws/src/main.rs", 30, "main"),
                ],
                _ => Chain::new(),
            }
        };
        let usart = reg("stm32-hal/src/usart.rs");
        let resp = stack(vec![
            // Innermost: the real PC, never looked up again.
            dap("write", Some(usart.as_str()), 88, 0x0800_0900),
            dap("main", Some("/ws/src/main.rs"), 12, 0x0800_0124),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(frames[0].at, Some(("src/main.rs".to_owned(), 11)));
        assert_eq!(frames[0].name, "main");

        let resp = stack(vec![
            dap("write", Some(usart.as_str()), 88, 0x0800_0900),
            dap("send #[inline]", Some("/ws/src/drv.rs"), 8, 0x0800_0224),
            dap("main", Some("/ws/src/main.rs"), 31, 0x0800_0224),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(frames[0].at, Some(("src/main.rs".to_owned(), 30)));
        assert_eq!(frames[1].at, Some(("src/drv.rs".to_owned(), 7)));

        // Right outside an exception entry the PC was stacked by the hardware:
        // already exact, not looked up.
        let resp = stack(vec![
            dap(
                "on_interrupt",
                Some(reg("stm32-hal/src/time.rs").as_str()),
                5,
                0x0800_0700,
            ),
            dap("SysTick", None, 0, 0x0800_0124),
            dap("main", Some("/ws/src/main.rs"), 12, 0x0800_0124),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(frames[0].at, Some(("src/main.rs".to_owned(), 12)));
        assert!(frames[1].exception);
        // Each relocated frame asks one byte back, then its own address (to
        // tell whether the call was the caller's last instruction).
        assert_eq!(
            asked,
            vec![0x0800_0123, 0x0800_0124, 0x0800_0223, 0x0800_0224]
        );
    }

    #[test]
    fn a_call_from_inside_an_inlined_wrapper_keeps_the_users_line() {
        // `delay_ms(3000)` on line 147 of a closure inlined into main: one
        // byte back the chain is [delay, closure, main], at the return address
        // just [main] on the next statement. The closure's line must win, not
        // cortex-m's `delay` and not main's call of the closure.
        let root = Path::new("/ws");
        let mut lookup = |addr: u64| match addr {
            0x0800_16d3 => vec![
                site(&reg("cortex-m/src/call_asm.rs"), 19, "delay"),
                site("/ws/src/main.rs", 147, "{closure#0}"),
                site("/ws/src/main.rs", 245, "__cortex_m_rt_main"),
            ],
            0x0800_16d4 => vec![site("/ws/src/main.rs", 248, "__cortex_m_rt_main")],
            _ => Chain::new(),
        };
        let resp = stack(vec![
            dap("__delay", None, 0, 0x0800_0400),
            dap(
                "__cortex_m_rt_main",
                Some("/ws/src/main.rs"),
                248,
                0x0800_16d4,
            ),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(charged(&frames), Some(("src/main.rs".to_owned(), 147)));
        assert_eq!(
            names(&frames),
            [
                "__cortex_m_rt_main",
                "{closure#0} #[inline]",
                "delay #[inline]",
                "__delay"
            ]
        );
    }

    #[test]
    fn code_inlined_after_the_call_loses_its_line() {
        // The PWM fade: `for i in 0..10000 { …; delay(…) }`. At the return
        // address the loop head is inlined (Range::next and its helpers, on
        // line 158); one byte back the call sits in a closure on line 152.
        let root = Path::new("/ws");
        let core = "/rustc/abc/library/core/src/iter/range.rs";
        let mut lookup = |addr: u64| match addr {
            0x0800_1621 => vec![
                site(&reg("cortex-m/src/call_asm.rs"), 19, "delay"),
                site("/ws/src/main.rs", 152, "{closure#1}"),
                site("/ws/src/main.rs", 161, "__cortex_m_rt_main"),
            ],
            0x0800_1622 => vec![site("/ws/src/main.rs", 158, "__cortex_m_rt_main")],
            _ => Chain::new(),
        };
        let resp = stack(vec![
            dap("__delay", None, 0, 0x0800_0400),
            dap("unchecked_add #[inline]", Some(core), 10, 0x0800_1622),
            dap("forward_unchecked #[inline]", Some(core), 20, 0x0800_1622),
            dap("spec_next #[inline]", Some(core), 30, 0x0800_1622),
            dap("next #[inline]", Some(core), 40, 0x0800_1622),
            dap(
                "__cortex_m_rt_main",
                Some("/ws/src/main.rs"),
                158,
                0x0800_1622,
            ),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(charged(&frames), Some(("src/main.rs".to_owned(), 152)));
        // The loop head's helpers ran AFTER the call: gone, names and all.
        assert_eq!(
            names(&frames),
            [
                "__cortex_m_rt_main",
                "{closure#1} #[inline]",
                "delay #[inline]",
                "__delay"
            ]
        );
        assert!(
            !frames
                .iter()
                .any(|f| f.at.as_ref().is_some_and(|a| a.1 == 158)),
            "{frames:?}"
        );
        let by = by_line(&[frames]);
        assert_eq!(stat(&by, "src/main.rs", 152).self_n, 1);
        assert_eq!(stat(&by, "src/main.rs", 152).func, "{closure#1} #[inline]");
        assert_eq!(stat(&by, "src/main.rs", 161).total_n, 1);
    }

    #[test]
    fn a_call_that_never_returns_is_renamed_from_the_elf() {
        // cortex-m-rt's `main` trampoline ends in `bl __cortex_m_rt_main`; its
        // return address is the first instruction of `panic_fmt`, so probe-rs
        // names the frame after that. The ELF one byte back says `main`.
        let root = Path::new("/ws");
        let panicking = "/rustc/abc/library/core/src/panicking.rs";
        let mut lookup = |addr: u64| match addr {
            0x0800_2f01 => vec![site("/ws/src/main.rs", 25, "main")],
            0x0800_2f02 => vec![site(panicking, 60, "panic_fmt")],
            _ => Chain::new(),
        };
        let resp = stack(vec![
            dap(
                "__cortex_m_rt_main",
                Some("/ws/src/main.rs"),
                147,
                0x0800_10f8,
            ),
            dap("panic_fmt", Some(panicking), 60, 0x0800_2f02),
        ]);
        let frames = frames_of(&resp, root, &mut lookup).unwrap();
        assert_eq!(frames[0], at("main", "src/main.rs", 25));
        assert_eq!(frames[1], at("__cortex_m_rt_main", "src/main.rs", 147));
    }

    #[test]
    fn names_and_versions_are_read_the_way_probe_rs_writes_them() {
        assert_eq!(bare_fn("core::panicking::panic_fmt"), "panic_fmt");
        assert_eq!(
            bare_fn("<radar::Radar<u8, 4> as radar::Sensor>::read::<u8>"),
            "read"
        );
        assert_eq!(
            bare_fn("app::__cortex_m_rt_main::{closure#0}"),
            "{closure#0}"
        );
        assert_eq!(bare_fn("main"), "main");
        // Only probe-rs up to 0.29 reports the return address itself.
        for (v, back) in [
            ("0.29.0", true),
            ("0.24.1", true),
            ("0.30.0", false),
            ("0.32.0", false),
            ("1.0.0", false),
            ("dev", false),
        ] {
            assert_eq!(reports_return_address(v), back, "{v}");
        }
    }

    #[test]
    fn a_lookup_that_knows_nothing_changes_nothing() {
        let root = Path::new("/ws");
        let resp = stack(vec![
            dap("write", None, 0, 0x0800_0900),
            dap("main", Some("/ws/src/main.rs"), 12, 0x0800_0124),
        ]);
        let frames = frames_of(&resp, root, &mut nothing).unwrap();
        assert_eq!(frames[0].at, Some(("src/main.rs".to_owned(), 12)));
        assert_eq!(parse_pc("0x08000124"), Some(0x0800_0124));
        assert_eq!(parse_pc("291"), Some(291));
        assert_eq!(parse_pc("pc"), None);
        // Not an ELF: no line table, and no panic.
        assert!(ElfLines::new(b"MZ\x90\x00 not an elf").is_none());
    }

    /// The caller fix against a real Cortex-M release ELF. Ignored and
    /// path-driven - set `EIDE_THUMB_ELF` to a `thumbv*` release binary and
    /// `EIDE_THUMB_ROOT` to the project it was built from:
    ///
    /// `EIDE_THUMB_ELF=… EIDE_THUMB_ROOT=… cargo test -- --ignored a_real_thumb_elf`
    ///
    /// Every Thumb-2 `bl` whose call (one byte before the return address) has
    /// a project line is replayed the way probe-rs 0.29 would report it: a
    /// caller frame per function inlined at the RETURN address, all on that
    /// address. After `frames_of`, the line `by_line` charges must be the
    /// call's own innermost project line - at every site, including calls made
    /// from inside an inlined wrapper and calls that never return.
    #[test]
    #[ignore]
    fn a_real_thumb_elf_puts_callers_on_their_call() {
        use object::{Object, ObjectSection};
        let (Some(elf), Some(root)) = (
            std::env::var_os("EIDE_THUMB_ELF"),
            std::env::var_os("EIDE_THUMB_ROOT"),
        ) else {
            println!("EIDE_THUMB_ELF / EIDE_THUMB_ROOT not set - nothing to check");
            return;
        };
        let data = std::fs::read(&elf).unwrap();
        let mut lines = ElfLines::new(&data).expect("an ELF with DWARF");
        let obj = object::File::parse(&*data).unwrap();
        let text = obj.section_by_name(".text").expect("a .text section");
        let (base, code) = (text.address(), text.data().unwrap());
        let root = Path::new(&root);
        let (mut calls, mut fixed, mut wrong_unfixed, mut i) = (0, 0, 0, 0);
        let mut wrong = Vec::new();
        while i + 4 <= code.len() {
            let hw1 = u16::from_le_bytes([code[i], code[i + 1]]);
            let hw2 = u16::from_le_bytes([code[i + 2], code[i + 3]]);
            // A 32-bit Thumb-2 instruction starts 0b11101/0b11110/0b11111;
            // BL is `11110 S imm10` then `11 J1 1 J2 imm11`.
            let wide = hw1 >> 11 >= 0b11101;
            if hw1 & 0xF800 == 0xF000 && hw2 & 0xD000 == 0xD000 {
                let ret = base + i as u64 + 4;
                let call = lines.chain(ret - 1);
                // The call's innermost PROJECT site, and the function it is
                // in. On a DWARF line-0 row its line is None: `by_line` must
                // count that as `no_line`, not as an outer line.
                let want = call.iter().find_map(|s| {
                    let rel = project_rel(s.path.as_deref()?, root)?;
                    Some((s.line.map(|l| (rel, l)), s.func.clone()))
                });
                let here = lines.chain(ret);
                if let Some((want, func)) = want
                    && !here.is_empty()
                {
                    calls += 1;
                    // What probe-rs 0.29 sends for this caller.
                    let mut dap_frames = vec![dap("callee", None, 0, 0x1000)];
                    for (k, s) in here.iter().enumerate() {
                        let func = s.func.clone().unwrap_or_default();
                        let name = if k + 1 < here.len() {
                            format!("{func} #[inline]")
                        } else {
                            func
                        };
                        let line = u64::from(s.line.unwrap_or(0));
                        dap_frames.push(dap(&name, s.path.as_deref(), line, ret));
                    }
                    let resp = stack(dap_frames);
                    if charged(&frames_of(&resp, root, &mut nothing).unwrap()) != want {
                        wrong_unfixed += 1;
                    }
                    let mut lookup = |a: u64| lines.chain(a);
                    let frames = frames_of(&resp, root, &mut lookup).unwrap();
                    let got = charged(&frames);
                    // And the row is named after the function that holds the
                    // line, not one that runs after the call.
                    let named = frames
                        .iter()
                        .rev()
                        .find(|f| f.project)
                        .map(|f| fn_key(&f.name).to_owned());
                    if got == want && named == func {
                        fixed += 1;
                    } else {
                        wrong.push(format!(
                            "{:#x}: {got:?} in {named:?} instead of {want:?} in {func:?}",
                            ret - 4
                        ));
                    }
                }
            }
            i += if wide { 4 } else { 2 };
        }
        println!(
            "{calls} calls with a project site; without the fix {wrong_unfixed} land on another \
             line, with it {fixed} are right, names included"
        );
        assert!(calls > 0, "no call site in the project's code");
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn the_standard_error_matches_the_plan_numbers() {
        // 10% of 120 samples: ±2.7 points; of 1000: ±0.95.
        assert!((std_err_pct(12, 120) - 2.74).abs() < 0.01);
        assert!((std_err_pct(100, 1000) - 0.95).abs() < 0.01);
        assert_eq!(std_err_pct(0, 0), 0.0);
        assert_eq!(std_err_pct(5, 5), 0.0);
    }

    #[test]
    fn project_paths_are_made_relative_and_others_dropped() {
        let root = Path::new(r"C:\Users\me\AppData\Local\Temp\embedded_ide_0_check");
        assert_eq!(
            project_rel(
                r"c:\users\me\appdata\local\temp\embedded_ide_0_check\src\main.rs",
                root
            )
            .as_deref(),
            Some("src/main.rs")
        );
        assert_eq!(
            project_rel(
                r"\\?\C:\Users\me\AppData\Local\Temp\embedded_ide_0_check\mylib\src\lib.rs",
                root
            )
            .as_deref(),
            Some("mylib/src/lib.rs")
        );
        // Relative, as cargo records a workspace crate's own files.
        assert_eq!(
            project_rel("src/pins.rs", root).as_deref(),
            Some("src/pins.rs")
        );
        // Build output, a sibling folder that shares the prefix, the registry,
        // and `core` are not the project.
        for other in [
            r"C:\Users\me\AppData\Local\Temp\embedded_ide_0_check\target\riscv\out\gen.rs",
            r"C:\Users\me\AppData\Local\Temp\embedded_ide_0_check_old\src\main.rs",
            r"C:\Users\me\.cargo\registry\src\esp-hal-1.0.0\src\uart.rs",
            "/rustc/abc123/library/core/src/ptr/mod.rs",
            "../elsewhere/src/main.rs",
        ] {
            assert_eq!(project_rel(other, root), None, "{other}");
        }
    }

    #[test]
    fn the_result_carries_the_compiled_line_text() {
        let texts = HashMap::from([(
            "src/main.rs".to_owned(),
            "fn main() {\n    let x = read();\n}\n".to_owned(),
        )]);
        let res = result_of(&[vec![at("main", "src/main.rs", 2)]], texts);
        assert_eq!(res.by_line.lines[0].code, "let x = read();");
        assert_eq!(res.samples, 1);
        assert_eq!(res.root.children[0].name, "main");
    }

    #[test]
    fn the_gap_between_pauses_varies_within_its_bounds() {
        let gaps: Vec<Duration> = (0..64).map(|i| gap(i, 0x1234_5678)).collect();
        assert!(
            gaps.iter()
                .all(|g| (4..=20).contains(&(g.as_millis() as u64)))
        );
        let distinct: HashSet<Duration> = gaps.iter().copied().collect();
        assert!(distinct.len() > 8, "{distinct:?}");
    }
}
