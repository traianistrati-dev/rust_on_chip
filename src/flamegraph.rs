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
use serde::{Deserialize, Serialize};
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
    /// An idle loop: everything inside it is the core idling, wherever the
    /// halt caught it. esp-rtos's `idle_hook` - with a debugger attached it
    /// SPINS instead of sleeping, because esp-hal's `wait_for_interrupt`
    /// returns at once then, so most of its samples are not in a `wfi` at all.
    pub idle_loop: bool,
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

/// esp-rtos's idle loop: `idle_hook`, from the esp-rtos crate.
fn runs_idle_loop(name: &str, path: Option<&str>) -> bool {
    let Some(path) = path else {
        return false;
    };
    base_name(name) == "idle_hook" && path.replace('\\', "/").contains("/esp-rtos-")
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
        // The idle loop only when it is the innermost PHYSICAL frame - all
        // that is inside it inlined. An interrupt that preempts it is work.
        let in_idle_loop = own
            .iter()
            .rev()
            .find(|f| !f.name.ends_with(" #[inline]"))
            .is_some_and(|f| f.idle_loop);
        if stack.last().is_some_and(|f| f.waits) || in_idle_loop {
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

/// One halt as probe-rs reported it - kept raw, so a saved run is counted
/// again by whatever this code has learned since.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RawSample {
    /// The `stackFrames` of the `stackTrace` response, innermost first.
    pub frames: Value,
    /// The registers at the halt (RISC-V), read when this code unwinds the
    /// stack itself - see [`ElfLines::unwind_halt`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regs: Option<RiscvRegs>,
    /// The stack words that unwinding read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<StackCopy>,
}

/// The RISC-V registers unwinding starts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiscvRegs {
    pub ra: u64,
    pub sp: u64,
    /// `s0`, the frame pointer some CFI measures the CFA from.
    pub s0: u64,
}

/// Target memory the unwinder read, in the 8-byte chunks it asked for: only
/// the words it needed, wherever they were - a task's frame can be 3 KB.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackCopy {
    pub chunks: Vec<(u64, Vec<u8>)>,
}

impl StackCopy {
    /// The little-endian 32-bit word at `addr`, when a chunk holds it.
    fn word(&self, addr: u64) -> Option<u64> {
        self.chunks.iter().find_map(|(base, bytes)| {
            let off = usize::try_from(addr.checked_sub(*base)?).ok()?;
            let b = bytes.get(off..off.checked_add(4)?)?;
            Some(u64::from(u32::from_le_bytes([b[0], b[1], b[2], b[3]])))
        })
    }
}

/// The format tag of a saved run; a file with another one is refused.
pub const RUN_FORMAT: &str = "rust_on_chip-profile-run-1";

/// FNV-1a of the ELF: tells the build a run was sampled with from a later one
/// at the same path - every Flash, Size and Sample rebuilds it there, and an
/// esp-hal ELF carries no build-id. Written out rather than `DefaultHasher`,
/// whose algorithm may change between Rust versions and orphan saved files.
fn elf_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Everything a sampling run produced - what "Save…" writes and "Load…" reads
/// back. Counting happens in [`process`], for a live run and a loaded one
/// alike.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SampleRun {
    pub format: String,
    pub chip: String,
    pub target: String,
    /// The project the ELF was built in: frames whose source lies under it
    /// are the project's own.
    pub project_dir: PathBuf,
    /// The release ELF the run attached with, read again by every
    /// [`process`] - and used only while it still matches `elf_hash`.
    pub elf: PathBuf,
    /// [`elf_fingerprint`] of `elf` at sampling time.
    #[serde(default)]
    pub elf_hash: Option<u64>,
    /// [`reports_return_address`] as decided at sampling time.
    pub fix_callers: bool,
    /// Every project file's text as this run compiled it, keyed like the
    /// sampled lines (`src/main.rs`). A file whose live text differs has been
    /// edited since, and its line numbers may no longer point at the code
    /// they were sampled on.
    pub texts: HashMap<String, String>,
    pub samples: Vec<RawSample>,
}

#[derive(Clone, Debug)]
pub struct FlameResult {
    pub root: FlameNode,
    pub by_line: ByLine,
    /// The run itself: Save writes it, the editor reads its `texts`. Shared,
    /// because the UI clones the state every frame.
    pub run: Arc<SampleRun>,
    /// Usable samples (a halt with no frames at all is dropped).
    pub samples: usize,
    /// The file this result was loaded from, `None` for a live run.
    pub loaded_from: Option<PathBuf>,
    /// The run's ELF is gone or was rebuilt since: probe-rs's frames were
    /// counted as they came, without anything read from the ELF.
    pub elf_changed: bool,
}

/// Fold counted stacks into both views.
fn result_of(
    samples: &[Vec<Frame>],
    run: SampleRun,
    loaded_from: Option<PathBuf>,
    elf_changed: bool,
) -> FlameResult {
    let names: Vec<Vec<String>> = samples
        .iter()
        .map(|s| s.iter().map(|f| f.name.clone()).collect())
        .collect();
    let mut by = by_line(samples);
    fill_code(&mut by, &run.texts);
    FlameResult {
        root: build_tree(&names),
        by_line: by,
        run: Arc::new(run),
        samples: samples.len(),
        loaded_from,
        elf_changed,
    }
}

/// The halt PC: the innermost frame's `instructionPointerReference`.
fn halt_pc(frames: &Value) -> Option<u64> {
    frames.as_array()?.first()?["instructionPointerReference"]
        .as_str()
        .and_then(parse_pc)
}

/// Count a run - live or loaded - into both views.
pub fn process(run: SampleRun, loaded_from: Option<PathBuf>) -> FlameResult {
    let elf_bytes = std::fs::read(&run.elf).ok();
    // Only the ELF the run was sampled with: a rebuilt one would put the old
    // addresses on other code.
    let elf_changed = match &elf_bytes {
        Some(b) => run.elf_hash != Some(elf_fingerprint(b)),
        None => true,
    };
    let mut elf = elf_bytes
        .as_deref()
        .filter(|_| !elf_changed)
        .and_then(ElfLines::new);
    let root = run.project_dir.clone();
    let mut stacks = Vec::with_capacity(run.samples.len());
    for s in &run.samples {
        let resp = json!({"body": {"stackFrames": s.frames}});
        // RISC-V: the stack as the ELF's CFI unwinds it from the halt, out of
        // the words the sampler read.
        let unwound = match (s.regs, elf.as_ref(), halt_pc(&s.frames)) {
            (Some(regs), Some(e), Some(pc)) => {
                let mut word = |a: u64| s.stack.as_ref().and_then(|st| st.word(a));
                e.unwind_halt(pc, regs, &mut word, 64).map(|u| (pc, u))
            }
            _ => None,
        };
        let mut lookup = |a: u64| elf.as_mut().map_or_else(Chain::new, |l| l.chain(a));
        let frames = if run.fix_callers {
            frames_of(&resp, &root, &mut lookup)
        } else {
            frames_of(&resp, &root, &mut |_| Chain::new())
        };
        let Some(mut frames) = frames.filter(|f| !f.is_empty()) else {
            continue;
        };
        if let Some((pc, u)) = unwound {
            frames = from_unwind(frames, pc, u, &root, &mut lookup);
        }
        stacks.push(frames);
    }
    result_of(&stacks, run, loaded_from, elf_changed)
}

/// What the ELF's call frame information makes of a RISC-V halt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Unwound {
    /// The return addresses outward from the halt, innermost first.
    returns: Vec<u64>,
    /// `Some` when the halt is in ROM: the routine the call went to, named
    /// when the call's target is a known symbol.
    rom: Option<Option<String>>,
}

/// The stack rebuilt from an unwind: the halt's own frames - the ELF's inline
/// chain at the PC, or probe-rs's ROM frame renamed - then one physical frame
/// per return address, each looked up one byte back, inside its call.
///
/// probe-rs 0.29 reads only `.debug_frame`, and an esp-hal ELF has only an
/// `.eh_frame`: every frame after the first comes from its no-CFI guess (`ra`,
/// then two stale words below `sp`), so the stack rarely reached the user's
/// own lines. ROM made it worse - no DWARF, no symbol, and a C3 without an FPU
/// runs every `f32` operation there (`__mulsf3`, …), as it does `memcpy` and
/// `__udivdi3`. probe-rs's frames past the first are dropped.
fn from_unwind(
    frames: Vec<Frame>,
    pc: u64,
    u: Unwound,
    root: &Path,
    lookup: &mut dyn FnMut(u64) -> Chain,
) -> Vec<Frame> {
    let inner = match u.rom {
        Some(callee) => {
            let Some(mut rom) = frames.last().cloned() else {
                return frames;
            };
            // probe-rs names a ROM PC after the nearest symbol BELOW it - a
            // `.bss` pool, say - which says nothing.
            rom.name = callee.unwrap_or_else(|| "<unknown function>".to_owned());
            vec![rom]
        }
        None => {
            let mut inner = rebuilt(&lookup(pc), root);
            if inner.is_empty() {
                // No DWARF at the PC (hand-written trap code): nothing to
                // build on.
                return frames;
            }
            inner.reverse();
            inner
        }
    };
    let mut out = Vec::new();
    for &ret in u.returns.iter().rev() {
        let mut caller = rebuilt(&lookup(ret.wrapping_sub(1)), root);
        caller.reverse();
        out.extend(caller);
    }
    out.extend(inner);
    out
}

/// Whether probe-rs lost this stack in ROM: the innermost frame has no source
/// and its PC lies OUTSIDE the ELF's image - not hand-written code of the image
/// (a trap entry). Whatever name probe-rs put on it, and whatever frames it
/// guessed past it, mean nothing. Returns that frame's DAP id.
fn lost_in_rom(frames: &Value, in_image: impl Fn(u64) -> bool) -> Option<i64> {
    let first = frames.as_array()?.first()?;
    if first["source"]["path"].is_string() || in_image(halt_pc(frames)?) {
        return None;
    }
    first["id"].as_i64()
}

/// `ra`, `sp` and `s0` from the registers of a `variables` response. probe-rs
/// names a RISC-V register "x1/…", its role after the slash.
fn regs_of(vars: &Value) -> Option<RiscvRegs> {
    let list = vars["body"]["variables"].as_array()?;
    let reg = |names: &[&str]| {
        list.iter().find(|v| {
            v["name"].as_str().is_some_and(|n| {
                n.split('/')
                    .any(|p| names.contains(&p.trim().to_ascii_lowercase().as_str()))
            })
        })?["value"]
            .as_str()
            .and_then(parse_pc)
    };
    Some(RiscvRegs {
        ra: reg(&["x1", "ra", "returnaddress"])?,
        sp: reg(&["x2", "sp", "stackpointer"])?,
        s0: reg(&["x8", "s0", "fp", "framepointer"]).unwrap_or(0),
    })
}

/// Standard base64, what DAP's `readMemory` returns its bytes in.
pub(crate) fn from_base64(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text.as_bytes() {
        if c == b'=' {
            break;
        }
        if c.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | u32::from(value(c)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// What the Profile tab asks of a saved run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlameFileOp {
    Save,
    Load,
}

/// Write `run` to `path`, as JSON.
pub fn save_run(run: &SampleRun, path: &Path) -> Result<(), String> {
    let bytes = serde_json::to_vec(run).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())
}

/// Read a saved run, refusing anything that is not one. Called BEFORE the run
/// on screen is given up, so a wrong file leaves it there.
pub fn read_run(path: &Path) -> Result<SampleRun, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let run: SampleRun =
        serde_json::from_slice(&bytes).map_err(|e| format!("not a saved profile run: {e}"))?;
    if run.format != RUN_FORMAT {
        return Err(format!(
            "not a saved profile run of this IDE (format {:?}, expected {RUN_FORMAT:?})",
            run.format
        ));
    }
    Ok(run)
}

/// Count a run read by [`read_run`] on a background thread, and show it.
pub fn start_load(
    run: SampleRun,
    path: PathBuf,
    state: Arc<Mutex<FlameState>>,
    ctx: eframe::egui::Context,
) {
    if state.lock().unwrap().is_busy() {
        return;
    }
    *state.lock().unwrap() = FlameState::Loading;
    ctx.request_repaint();
    thread::spawn(move || {
        let next = FlameState::Done(process(run, Some(path)));
        *state.lock().unwrap() = next;
        ctx.request_repaint();
    });
}

#[derive(Clone, Debug, Default)]
pub enum FlameState {
    #[default]
    Idle,
    /// `cargo build --release` (to get the ELF + symbols).
    Building,
    /// Sampling in progress: `(collected, target)`.
    Sampling(usize, usize),
    /// A saved run being read and counted.
    Loading,
    Done(FlameResult),
    Failed(String),
}

impl FlameState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            FlameState::Building | FlameState::Sampling(..) | FlameState::Loading
        )
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
    // The build this session attaches with, read and fingerprinted before
    // anything else (Size, Analyze) can relink it at the same path.
    let elf_bytes = std::fs::read(&elf).ok();
    let elf_hash = elf_bytes.as_deref().map(elf_fingerprint);

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
                    // On a char boundary: a split one panics, with the lock
                    // held.
                    let mut cut = l.len() - 8192;
                    while !l.is_char_boundary(cut) {
                        cut += 1;
                    }
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
        elf_bytes.as_deref(),
        target,
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
        // Sample attaches: it never flashes.
        explain_dap_failure(
            e,
            &console.lock().unwrap(),
            &log.lock().unwrap(),
            &probe,
            chip,
            false,
        )
    })?;
    let run = SampleRun {
        format: RUN_FORMAT.to_owned(),
        chip: chip.to_owned(),
        target: target.to_owned(),
        project_dir: project_dir.to_path_buf(),
        elf,
        elf_hash,
        fix_callers,
        texts,
        samples,
    };
    Ok(process(run, None))
}

/// A failed dap-server session's error, with what probe-rs said about it:
/// its console (`output` events) and its stderr `log`.
pub(crate) fn explain_dap_failure(
    e: String,
    console: &[String],
    log: &str,
    probe: &str,
    // For the stuck-debug-unit card: which pins, and whether this session
    // would have flashed (see `failure_hint::riscv_dmi_timeout`).
    chip: &str,
    flashes: bool,
) -> String {
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
        .or_else(|| crate::failure_hint::riscv_dmi_timeout(&everything, chip, flashes))
        .or_else(|| {
            crate::failure_hint::probe_open_failure(&everything).map(|d| {
                crate::failure_hint::probe_open_message(
                    &d,
                    crate::probe::missing_device_interface_guid(Some(probe)),
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
}

/// Wait up to `grace` for the dap-server to exit by itself, and only then kill
/// it. A debugger killed mid-teardown leaves the target's debug module still
/// configured - on a RISC-V ESP32-C3 that shows up as probe-rs warning "Could
/// not clear all hardware breakpoints", after which EVERY later attach dies
/// with "Timeout during DMI access" until the board is power-cycled. The wait
/// costs nothing in the normal case, where the server is already gone.
pub(crate) fn wait_or_kill(server: &mut std::process::Child, grace: Duration) {
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
pub(crate) fn tail_of<'a>(lines: impl Iterator<Item = &'a str>, n: usize) -> Vec<&'a str> {
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
pub(crate) fn build_elf(project_dir: &std::path::Path, target: &str) -> Result<PathBuf, String> {
    build_elf_until(project_dir, target, None)
        .map(|elf| elf.expect("a build with no stop flag is never stopped"))
}

/// [`build_elf`], ended - cargo and the rustc under it - as soon as `stop`
/// is set: `Ok(None)` then.
pub(crate) fn build_elf_until(
    project_dir: &std::path::Path,
    target: &str,
    stop: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Option<PathBuf>, String> {
    let mut child = crate::lsp::spawn_in_own_group(
        no_window(&mut Command::new("cargo"))
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
            .stderr(Stdio::piped()),
    )
    .spawn()
    .map_err(|e| format!("could not launch cargo: {e}"))?;
    // Both pipes drained as cargo writes, or a full one stops it.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if stop.is_some_and(|s| s.load(std::sync::atomic::Ordering::SeqCst)) {
            crate::lsp::kill_process_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            let _ = (stdout.join(), stderr.join());
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(100));
    };
    let out = std::process::Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    };
    let mut elf: Option<PathBuf> = None;
    // With `--message-format=json` rustc's errors are on stdout, as
    // `compiler-message` records; stderr only says "could not compile".
    let mut errors: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v["reason"] == "compiler-artifact" {
            if let Some(exe) = v["executable"].as_str() {
                elf = Some(PathBuf::from(exe));
            }
        }
        if v["reason"] == "compiler-message"
            && v["message"]["level"] == "error"
            && let Some(text) = v["message"]["rendered"].as_str()
            && errors.len() < 5
        {
            errors.push(text.trim_end().to_owned());
        }
    }
    if !out.status.success() {
        let mut msg = "release build failed:".to_owned();
        for e in &errors {
            msg.push('\n');
            msg.push_str(e);
        }
        msg.push('\n');
        msg.push_str(String::from_utf8_lossy(&out.stderr).trim_end());
        return Err(msg);
    }
    elf.map(Some)
        .ok_or_else(|| "build produced no executable (nothing to profile)".to_string())
}

/// Connect to the DAP server, attach, and run the pause/stackTrace/continue
/// sampling loop.
#[allow(clippy::too_many_arguments)]
fn sample_over_dap(
    port: u16,
    elf: &std::path::Path,
    // That ELF's contents, as fingerprinted for the run.
    elf_bytes: Option<&[u8]>,
    // The Rust target: a RISC-V halt is unwound here - see `unwind_halt`.
    target: &str,
    chip: &str,
    // Already resolved to ONE probe by `run` - never "let probe-rs choose".
    probe: &str,
    console: &Arc<Mutex<Vec<String>>>,
    n_samples: usize,
    state: &Arc<Mutex<FlameState>>,
    ctx: &eframe::egui::Context,
) -> Result<Vec<RawSample>, String> {
    // RISC-V: the image, to tell a stack lost in ROM - see `through_rom`.
    // Read before the session starts.
    let image = elf_bytes
        .filter(|_| target.starts_with("riscv"))
        .and_then(ElfLines::new);

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

    let mut samples: Vec<RawSample> = Vec::with_capacity(n_samples);
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
        let frames = st["body"]["stackFrames"].clone();
        // RISC-V: unwind here, with the ELF's CFI, when probe-rs cannot -
        // every halt of an ELF without `.debug_frame` (esp-hal's), and a halt
        // lost in ROM. The registers, then each stack word the walk asks for:
        // 8-byte reads, ~1 per frame, recorded so a saved run replays the same
        // walk. A refused request costs that sample its callers, not the run.
        let (mut regs, mut stack) = (None, None);
        let frame_id = image.as_ref().and_then(|img| {
            if img.has_debug_frame() {
                lost_in_rom(&frames, |pc| img.in_image(pc))
            } else {
                frames.as_array()?.first()?["id"].as_i64()
            }
        });
        if let (Some(img), Some(frame_id)) = (image.as_ref(), frame_id) {
            send(
                &mut stream,
                &mut seq,
                "variables",
                json!({"variablesReference": frame_id}),
            )?;
            regs = wait_response(&mut stream, "variables", console)
                .ok()
                .and_then(|v| regs_of(&v));
            if let (Some(r), Some(pc)) = (regs, halt_pc(&frames)) {
                let mut copy = StackCopy::default();
                {
                    let mut word = |addr: u64| -> Option<u64> {
                        if let Some(w) = copy.word(addr) {
                            return Some(w);
                        }
                        let base = addr & !7;
                        send(
                            &mut stream,
                            &mut seq,
                            "readMemory",
                            json!({"memoryReference": format!("{base:#x}"), "count": 8}),
                        )
                        .ok()?;
                        // probe-rs 0.29 puts the END address in `address`: the
                        // chunk starts where it was asked to.
                        let bytes = wait_response(&mut stream, "readMemory", console)
                            .ok()
                            .and_then(|m| from_base64(m["body"]["data"].as_str()?))?;
                        copy.chunks.push((base, bytes));
                        copy.word(addr)
                    };
                    let _ = img.unwind_halt(pc, r, &mut word, 64);
                }
                stack = Some(copy);
            }
        }
        if frames.as_array().is_some_and(|a| !a.is_empty()) {
            samples.push(RawSample {
                frames,
                regs,
                stack,
            });
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

/// One CFI row, reduced to what a RISC-V walk needs: the CFA is register
/// `cfa_reg` (2 = `sp`, 8 = `s0`) plus `cfa_off`, and `ra` / `s0` were saved
/// at CFA + `ra_at` / `s0_at`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CfiRow {
    cfa_reg: u16,
    cfa_off: i64,
    ra_at: Option<i64>,
    s0_at: Option<i64>,
}

/// The return addresses OUTSIDE a caller that runs at return address `pc`,
/// innermost first, by call frame information: `row_at` gives the row in force
/// at an address, `word` reads a saved register. Starts from `regs`' `sp` /
/// `s0`. Every address must pass `is_return` (in the image, right after a
/// call) or the walk stops there: a wrong step ends it rather than inventing
/// frames.
fn walk(
    pc: u64,
    regs: RiscvRegs,
    max: usize,
    row_at: impl Fn(u64) -> Option<CfiRow>,
    is_return: impl Fn(u64) -> bool,
    word: &mut dyn FnMut(u64) -> Option<u64>,
) -> Vec<u64> {
    let wrap = |base: u64, off: i64| base.wrapping_add_signed(off) & 0xffff_ffff;
    let (mut pc, mut sp, mut s0) = (pc, regs.sp, regs.s0);
    let mut out = Vec::new();
    while out.len() < max {
        // One byte back, inside the call: the row the caller was in.
        let Some(row) = row_at(pc.wrapping_sub(1)) else {
            break;
        };
        let cfa = match row.cfa_reg {
            2 => wrap(sp, row.cfa_off),
            8 => wrap(s0, row.cfa_off),
            _ => break,
        };
        // The stack grows down: a caller that saved `ra` has a frame, and
        // its CFA lies strictly above. A repeated return address is NOT a
        // loop - direct recursion saves the same `ra` at every level.
        if cfa <= sp {
            break;
        }
        let Some(next) = row.ra_at.and_then(|o| word(wrap(cfa, o))) else {
            break;
        };
        if let Some(v) = row.s0_at.and_then(|o| word(wrap(cfa, o))) {
            s0 = v;
        }
        // RISC-V: the caller's `sp` is the CFA.
        sp = cfa;
        if next == 0 || !is_return(next) {
            break;
        }
        out.push(next);
        pc = next;
    }
    out
}

/// The RISC-V call whose return address is `ra`, from the instructions
/// before it: `Some(Some(target))` for a direct call - `jal`, `auipc`/`lui` +
/// `jalr` (how flash code reaches ROM, 32 MB away), `c.jal` -,
/// `Some(None)` for a call through a register (`jalr`, `c.jalr`), `None` when
/// the instruction before `ra` is no call at all, so `ra` is no caller's.
fn call_into(
    ra: u64,
    word_at: impl Fn(u64) -> Option<u32>,
    half_at: impl Fn(u64) -> Option<u16>,
) -> Option<Option<u64>> {
    // RV32: addresses wrap at 32 bits (`auipc` from 0x4201_xxxx to ROM does).
    let add = |base: u64, off: i64| (base as i64).wrapping_add(off) as u64 & 0xffff_ffff;
    if let Some(i) = ra.checked_sub(4).and_then(&word_at)
        && i & 3 == 3
        && (i >> 7) & 31 == 1
    {
        match i & 0x7f {
            // jal ra, imm
            0x6f => return Some(Some(add(ra - 4, jal_imm(i)))),
            // jalr ra, lo(rs1) - with `auipc`/`lui rs1, hi` right before it
            // the target is known, otherwise it came out of a register.
            0x67 if (i >> 12) & 7 == 0 => {
                let rs1 = (i >> 15) & 31;
                let lo = i64::from((i as i32) >> 20);
                let hi = ra
                    .checked_sub(8)
                    .and_then(&word_at)
                    .filter(|p| p & 3 == 3 && (p >> 7) & 31 == rs1);
                return Some(hi.and_then(|p| {
                    let upper = i64::from((p & 0xffff_f000) as i32);
                    match p & 0x7f {
                        0x17 => Some(add(ra - 8, upper + lo)),
                        0x37 => Some(add(0, upper + lo)),
                        _ => None,
                    }
                }));
            }
            _ => {}
        }
    }
    let h = ra.checked_sub(2).and_then(half_at)?;
    match (h >> 13, h & 3) {
        // c.jal (RV32C only)
        (0b001, 0b01) => Some(Some(add(ra - 2, cj_imm(h)))),
        // c.jalr rs1: funct4 1001, rs1 != 0, rs2 == 0
        (0b100, 0b10) if (h >> 12) & 1 == 1 && (h >> 7) & 31 != 0 && (h >> 2) & 31 == 0 => {
            Some(None)
        }
        _ => None,
    }
}

/// The J-type immediate of `jal`: imm[20|10:1|11|19:12], sign-extended.
fn jal_imm(i: u32) -> i64 {
    let imm = ((i >> 31) & 1) << 20
        | ((i >> 21) & 0x3ff) << 1
        | ((i >> 20) & 1) << 11
        | ((i >> 12) & 0xff) << 12;
    i64::from(((imm << 11) as i32) >> 11)
}

/// The CJ-type immediate of `c.jal`: imm[11|4|9:8|10|6|7|3:1|5],
/// sign-extended.
fn cj_imm(h: u16) -> i64 {
    let h = u32::from(h);
    let imm = ((h >> 12) & 1) << 11
        | ((h >> 11) & 1) << 4
        | ((h >> 9) & 3) << 8
        | ((h >> 8) & 1) << 10
        | ((h >> 7) & 1) << 6
        | ((h >> 6) & 1) << 7
        | ((h >> 3) & 7) << 1
        | ((h >> 2) & 1) << 5;
    i64::from(((imm << 20) as i32) >> 20)
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

pub(crate) fn parse_pc(s: &str) -> Option<u64> {
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
                idle_loop: !project && runs_idle_loop(&name, r.path),
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
            // Every function but the outermost was inlined into it.
            let name = if k + 1 < chain.len() {
                format!("{func} #[inline]")
            } else {
                func
            };
            let project = file.is_some();
            let path = s.path.as_deref();
            Frame {
                waits: !project && (waits_by_name(&name) || executor_sleeps(&name, path)),
                idle_loop: !project && runs_idle_loop(&name, path),
                project,
                at: file.zip(s.line),
                name,
                exception: false,
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
    /// Every allocated section: `(address, size, contents)`; the contents are
    /// empty for one that occupies no file space.
    sections: Vec<(u64, u64, &'a [u8])>,
    /// Code symbols by address, ABSOLUTE ones included: the ROM routines a
    /// linker script PROVIDEs (`memcpy = 0x40000358`), which no DWARF and no
    /// sized symbol names.
    symbols: HashMap<u64, &'a str>,
    /// The call frame information, for [`ElfLines::unwind`].
    cfi: Cfi<'a>,
    bases: gimli::BaseAddresses,
}

type Slice<'a> = gimli::EndianSlice<'a, gimli::RunTimeEndian>;

/// Where an ELF keeps its call frame information. An esp-hal release build
/// has no `.debug_frame` at all, only an `.eh_frame` its linker script leaves
/// unallocated (address 0) - whose pointers are relative to that address.
enum Cfi<'a> {
    Debug(gimli::DebugFrame<Slice<'a>>),
    Eh(gimli::EhFrame<Slice<'a>>),
    Missing,
}

/// The row of `section` in force at `addr`, reduced to a [`CfiRow`].
fn cfi_row_in<'a, S: gimli::UnwindSection<Slice<'a>>>(
    section: &S,
    bases: &gimli::BaseAddresses,
    addr: u64,
) -> Option<CfiRow> {
    let mut ctx = gimli::UnwindContext::new();
    let row = section
        .unwind_info_for_address(bases, &mut ctx, addr, |section, bases, offset| {
            section.cie_from_offset(bases, offset)
        })
        .ok()?;
    let gimli::CfaRule::RegisterAndOffset { register, offset } = row.cfa() else {
        return None;
    };
    let at = |r: u16| match row.register(gimli::Register(r)) {
        gimli::RegisterRule::Offset(o) => Some(o),
        _ => None,
    };
    Some(CfiRow {
        cfa_reg: register.0,
        cfa_off: *offset,
        ra_at: at(1),
        s0_at: at(8),
    })
}

impl<'a> ElfLines<'a> {
    /// `None` for anything that is not an ELF with DWARF in it.
    fn new(data: &'a [u8]) -> Option<Self> {
        use object::{Object, ObjectSection, ObjectSymbol};
        let obj = object::File::parse(data).ok()?;
        let sections = obj
            .sections()
            .filter(|s| s.address() != 0 && s.size() != 0)
            .map(|s| (s.address(), s.size(), s.data().unwrap_or(&[])))
            .collect();
        let mut symbols = HashMap::new();
        for sym in obj.symbols() {
            let Ok(name) = sym.name() else { continue };
            if name.is_empty() || name.starts_with(['$', '.']) {
                continue;
            }
            if matches!(
                sym.kind(),
                object::SymbolKind::Text | object::SymbolKind::Unknown | object::SymbolKind::Label
            ) {
                symbols.entry(sym.address()).or_insert(name);
            }
        }
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
        let address_size = if obj.is_64() { 8 } else { 4 };
        let found = |name: &str| {
            let s = obj.section_by_name(name)?;
            let data = s.data().ok().filter(|d| !d.is_empty())?;
            Some((s.address(), data))
        };
        let mut bases = gimli::BaseAddresses::default();
        let cfi = if let Some((_, data)) = found(".debug_frame") {
            let mut f = gimli::DebugFrame::new(data, endian);
            f.set_address_size(address_size);
            Cfi::Debug(f)
        } else if let Some((addr, data)) = found(".eh_frame") {
            let mut f = gimli::EhFrame::new(data, endian);
            f.set_address_size(address_size);
            bases = bases.set_eh_frame(addr);
            Cfi::Eh(f)
        } else {
            Cfi::Missing
        };
        Some(Self {
            ctx: addr2line::Context::from_dwarf(dwarf).ok()?,
            cache: HashMap::new(),
            sections,
            symbols,
            cfi,
            bases,
        })
    }

    /// Whether the ELF has `.debug_frame` - the only CFI probe-rs 0.29 reads.
    fn has_debug_frame(&self) -> bool {
        matches!(self.cfi, Cfi::Debug(_))
    }

    /// A RISC-V halt at `pc`, unwound by this ELF's call frame information;
    /// `None` where nothing can be said (a ROM PC whose `ra` follows no call,
    /// code without CFI).
    ///
    /// In the image the row AT the PC holds (the halt is exact); `ra` comes
    /// from the stack where the row saved it, else from the register (a leaf,
    /// or a prologue). In ROM the routine is a leaf with no CFI: `ra` is its
    /// caller's return address when the instruction before it is a call, and
    /// the walk goes on from there with `sp` as the halt left it - or a few
    /// 16-byte frames above it, for a ROM routine that keeps one: offset 0
    /// first, the others only when that gets nowhere; each step is checked
    /// against a call instruction, so a wrong size gets nowhere either.
    fn unwind_halt(
        &self,
        pc: u64,
        regs: RiscvRegs,
        word: &mut dyn FnMut(u64) -> Option<u64>,
        max: usize,
    ) -> Option<Unwound> {
        let is_return = |r: u64| self.in_image(r) && self.call_before(r).is_some();
        let max = max.max(1);
        if !self.in_image(pc) {
            let target = self.call_before(regs.ra)?;
            let mut outer = Vec::new();
            for frame in (0..=96).step_by(16) {
                let at = RiscvRegs {
                    sp: regs.sp + frame,
                    ..regs
                };
                let found = walk(
                    regs.ra,
                    at,
                    max - 1,
                    |a| self.cfi_row(a),
                    is_return,
                    &mut *word,
                );
                if found.len() > outer.len() {
                    outer = found;
                }
                if !outer.is_empty() {
                    break;
                }
            }
            let mut returns = vec![regs.ra];
            returns.extend(outer);
            return Some(Unwound {
                returns,
                rom: Some(target.and_then(|t| self.symbol_name(t))),
            });
        }
        let row = self.cfi_row(pc)?;
        let wrap = |base: u64, off: i64| base.wrapping_add_signed(off) & 0xffff_ffff;
        let cfa = match row.cfa_reg {
            2 => wrap(regs.sp, row.cfa_off),
            8 => wrap(regs.s0, row.cfa_off),
            _ => return None,
        };
        let ra = match row.ra_at {
            Some(o) => word(wrap(cfa, o)),
            None => Some(regs.ra),
        };
        let s0 = row
            .s0_at
            .and_then(|o| word(wrap(cfa, o)))
            .unwrap_or(regs.s0);
        let mut returns = Vec::new();
        if let Some(ra) = ra.filter(|&ra| is_return(ra)) {
            returns.push(ra);
            let next = RiscvRegs { ra, sp: cfa, s0 };
            returns.extend(walk(
                ra,
                next,
                max - 1,
                |a| self.cfi_row(a),
                is_return,
                &mut *word,
            ));
        }
        Some(Unwound { returns, rom: None })
    }

    /// The CFI row in force at `addr`, reduced to what [`walk`] reads.
    fn cfi_row(&self, addr: u64) -> Option<CfiRow> {
        match &self.cfi {
            Cfi::Debug(f) => cfi_row_in(f, &self.bases, addr),
            Cfi::Eh(f) => cfi_row_in(f, &self.bases, addr),
            Cfi::Missing => None,
        }
    }

    /// Whether `addr` lies in any section of the image (ROM does not).
    fn in_image(&self, addr: u64) -> bool {
        self.sections
            .iter()
            .any(|&(a, size, _)| addr >= a && addr - a < size)
    }

    /// `len` bytes of the image's contents at `addr`.
    fn bytes(&self, addr: u64, len: usize) -> Option<&'a [u8]> {
        self.sections.iter().find_map(|&(a, _, data)| {
            let off = usize::try_from(addr.checked_sub(a)?).ok()?;
            data.get(off..off.checked_add(len)?)
        })
    }

    /// The call that returns to `ra` - see [`call_into`].
    fn call_before(&self, ra: u64) -> Option<Option<u64>> {
        call_into(
            ra,
            |at| {
                self.bytes(at, 4)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            },
            |at| self.bytes(at, 2).map(|b| u16::from_le_bytes([b[0], b[1]])),
        )
    }

    /// The symbol exactly at `addr`, demangled to a bare name.
    fn symbol_name(&self, addr: u64) -> Option<String> {
        let raw = self.symbols.get(&addr)?;
        let named = addr2line::demangle_auto(std::borrow::Cow::Borrowed(raw), None);
        Some(bare_fn(&named))
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

pub(crate) fn send(
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
pub(crate) fn read_msg(stream: &mut TcpStream) -> Option<Value> {
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
pub(crate) fn wait_response(
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
pub(crate) fn collect_output(msg: &Value, console: &Arc<Mutex<Vec<String>>>) {
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
pub(crate) fn wait_event(
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

pub(crate) fn free_port() -> u16 {
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

    /// Little-endian code at `base`, readable the way `call_into` reads it.
    fn code_at(base: u64, words: &[u32]) -> impl Fn(u64, usize) -> Option<Vec<u8>> {
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        move |at, len| {
            let off = usize::try_from(at.checked_sub(base)?).ok()?;
            bytes.get(off..off + len).map(<[u8]>::to_vec)
        }
    }

    fn call(ra: u64, read: impl Fn(u64, usize) -> Option<Vec<u8>>) -> Option<Option<u64>> {
        call_into(
            ra,
            |at| read(at, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            |at| read(at, 2).map(|b| u16::from_le_bytes([b[0], b[1]])),
        )
    }

    #[test]
    fn a_call_is_read_from_the_instructions_before_ra() {
        // From the real ESP32-C3 build: flash code reaching ROM's `memcpy`.
        //   420106fa: auipc ra, 0xfdff0
        //   420106fe: jalr  -0x3a2(ra)  <memcpy>
        let read = code_at(0x4201_06fa, &[0xfdff_0097, 0xc5e0_80e7]);
        assert_eq!(call(0x4201_0702, &read), Some(Some(0x4000_0358)));
        // jal ra, +0x100 and jal ra, -4.
        assert_eq!(
            call(0x1004, code_at(0x1000, &[0x1000_00ef])),
            Some(Some(0x1100))
        );
        assert_eq!(
            call(0x1004, code_at(0x1000, &[0xffdf_f0ef])),
            Some(Some(0x0ffc))
        );
        // jalr ra, 0(a5) with nothing that set a5 up: a call, target unknown.
        assert_eq!(
            call(0x1008, code_at(0x1000, &[0x0000_0013, 0x0007_80e7])),
            Some(None)
        );
        // c.jal +8 (RV32C) and c.jalr a5, as the low half of a word.
        assert_eq!(
            call(0x1002, code_at(0x1000, &[0x0000_2021])),
            Some(Some(0x1008))
        );
        assert_eq!(call(0x1002, code_at(0x1000, &[0x0000_9782])), Some(None));
        // addi a0, a0, 1: no call, so `ra` is nobody's return address.
        assert_eq!(call(0x1004, code_at(0x1000, &[0x0015_0513])), None);
        // Nothing to read at all.
        assert_eq!(call(0x1004, code_at(0x2000, &[])), None);
    }

    #[test]
    fn an_unwound_stack_reaches_the_users_line() {
        // `__mulsf3` in ROM, called by libm's `log10f`, called from the
        // user's `to_db` on line 262, called from `main`'s line 545.
        let root = Path::new("/ws");
        let mut lookup = |addr: u64| match addr {
            0x4201_0701 => vec![site(&reg("libm/src/math/log10f.rs"), 76, "log10f")],
            0x4200_2001 => vec![
                site("/ws/src/parse_result.rs", 262, "to_db"),
                site("/ws/src/main.rs", 545, "radar_task"),
            ],
            0x4200_3000 => vec![site("/ws/src/parse_result.rs", 270, "to_db")],
            _ => Chain::new(),
        };
        // probe-rs's own frames: the ROM PC named after the nearest symbol
        // below it, then two guesses.
        let probe_rs = vec![
            lib("garbage"),
            lib("guess"),
            lib("esp32c3_project::__main::POOL"),
        ];
        let rom = Unwound {
            returns: vec![0x4201_0702, 0x4200_2002],
            rom: Some(Some("__mulsf3".into())),
        };
        let out = from_unwind(probe_rs.clone(), 0x4000_0368, rom, root, &mut lookup);
        assert_eq!(
            names(&out),
            ["radar_task", "to_db #[inline]", "log10f", "__mulsf3"]
        );
        let by = by_line(&[out]);
        assert_eq!(stat(&by, "src/parse_result.rs", 262).self_n, 1);
        assert_eq!(stat(&by, "src/main.rs", 545).total_n, 1);
        assert_eq!(by.outside, 0);
        // A ROM target with no name: not probe-rs's misleading neighbour.
        let unnamed = Unwound {
            returns: vec![0x4201_0702],
            rom: Some(None),
        };
        let out = from_unwind(probe_rs.clone(), 0x4000_0368, unnamed, root, &mut lookup);
        assert_eq!(names(&out), ["log10f", "<unknown function>"]);
        // A halt in flash: the ELF's chain at the PC itself, then the callers.
        let flash = Unwound {
            returns: vec![0x4200_2002],
            rom: None,
        };
        let out = from_unwind(probe_rs.clone(), 0x4200_3000, flash, root, &mut lookup);
        assert_eq!(names(&out), ["radar_task", "to_db #[inline]", "to_db"]);
        assert_eq!(charged(&out), Some(("src/parse_result.rs".to_owned(), 270)));
        // No DWARF at the PC (trap code): probe-rs's frames stay.
        let trap = Unwound {
            returns: vec![],
            rom: None,
        };
        assert_eq!(
            from_unwind(probe_rs.clone(), 0x4038_0000, trap, root, &mut lookup),
            probe_rs
        );
    }

    #[test]
    fn only_a_stack_that_ends_outside_the_image_is_lost_in_rom() {
        let image = |pc: u64| (0x4200_0000..0x4240_0000).contains(&pc);
        let rom = json!([{"id": 7, "name": "<unknown function @ 0x40000368>",
                          "instructionPointerReference": "0x40000368", "line": 0}]);
        assert_eq!(lost_in_rom(&rom, image), Some(7));
        // Hand-written code of the image (a trap entry): `ra` is not a caller.
        let trap = json!([{"id": 7, "name": "<unknown function @ 0x42000010>",
                           "instructionPointerReference": "0x42000010", "line": 0}]);
        assert_eq!(lost_in_rom(&trap, image), None);
        // probe-rs's guesses past it, with sources, change nothing: they come
        // from stale words below `sp`.
        let guessed = json!([
            {"id": 7, "name": "<unknown function @ 0x40000368>",
             "instructionPointerReference": "0x40000368", "line": 0},
            {"id": 8, "name": "main", "instructionPointerReference": "0x42001000",
             "line": 12, "source": {"path": "/ws/src/main.rs"}}
        ]);
        assert_eq!(lost_in_rom(&guessed, image), Some(7));
        // Nor does the name of the nearest symbol below the ROM PC.
        let named = json!([{"id": 7, "name": "esp32c3_project::__main::POOL @ 0x40000368>",
                            "instructionPointerReference": "0x40000368", "line": 0}]);
        assert_eq!(lost_in_rom(&named, image), Some(7));
        // A frame with a source is not lost.
        let sourced = json!([{"id": 7, "name": "main", "line": 12,
                              "instructionPointerReference": "0x40000368",
                              "source": {"path": "/ws/src/main.rs"}}]);
        assert_eq!(lost_in_rom(&sourced, image), None);
        // The registers out of probe-rs's list.
        let vars = json!({"body": {"variables": [
            {"name": "x0/zero", "value": "0x00000000"},
            {"name": "x1/RA", "value": "0x42010702"},
            {"name": "x2/SP", "value": "0x3fc8f000"},
            {"name": "x8/FP", "value": "0x3fc8f040"}
        ]}});
        assert_eq!(
            regs_of(&vars),
            Some(RiscvRegs {
                ra: 0x4201_0702,
                sp: 0x3fc8_f000,
                s0: 0x3fc8_f040
            })
        );
        assert_eq!(regs_of(&json!({"body": {"variables": []}})), None);
    }

    #[test]
    fn esp_rtos_idle_hook_is_idle_wherever_the_halt_caught_it() {
        // With a debugger attached `wait_for_interrupt` returns at once, so the
        // halt lands anywhere in the loop - here inside `debugger_connected`.
        let root = Path::new("/ws");
        let rtos = reg("esp-rtos-0.3.0/src/task/mod.rs");
        let hal = reg("esp-hal-1.1.1/src/debugger.rs");
        let resp = stack(vec![
            dap("debugger_connected #[inline]", Some(&hal), 9, 0x4202_6c80),
            dap("wait_for_interrupt #[inline]", Some(&hal), 433, 0x4202_6c80),
            dap("idle_hook", Some(&rtos), 38, 0x4202_6c80),
        ]);
        let frames = frames_of(&resp, root, &mut nothing).unwrap();
        assert!(frames[0].idle_loop);
        let by = by_line(&[frames]);
        assert_eq!((by.idle, by.outside), (1, 0));
    }

    #[test]
    fn a_saved_run_loads_back_and_counts_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.json");
        let run = SampleRun {
            format: RUN_FORMAT.to_owned(),
            chip: "esp32c3".into(),
            target: "riscv32imc-unknown-none-elf".into(),
            project_dir: PathBuf::from("/ws"),
            // Gone: the run still loads, without the ELF's help.
            elf: dir.path().join("no-such-elf"),
            elf_hash: Some(1),
            fix_callers: false,
            texts: HashMap::from([("src/main.rs".to_owned(), "a\nb\nlet x = 1;\n".to_owned())]),
            samples: vec![
                RawSample {
                    frames: json!([{"id": 1, "name": "main", "line": 3,
                                    "instructionPointerReference": "0x42000010",
                                    "source": {"path": "/ws/src/main.rs"}}]),
                    regs: Some(RiscvRegs {
                        ra: 0x4201_0702,
                        sp: 0x3fc8_f000,
                        s0: 0,
                    }),
                    stack: Some(StackCopy {
                        chunks: vec![(0x3fc8_f000, vec![1, 2, 3, 4, 5, 6, 7, 8])],
                    }),
                },
                RawSample {
                    frames: json!([]),
                    regs: None,
                    stack: None,
                },
            ],
        };
        save_run(&run, &path).unwrap();
        let path_copy = dir.path().join("copy.json");
        save_run(&run, &path_copy).unwrap();
        let back = read_run(&path).unwrap();
        assert_eq!(back.samples, run.samples);
        assert_eq!(back.texts, run.texts);
        let res = process(back, Some(path.clone()));
        // The empty halt is dropped; the other is counted on its line.
        assert_eq!(res.samples, 1);
        assert_eq!(res.by_line.lines[0].code, "let x = 1;");
        assert_eq!(res.loaded_from.as_deref(), Some(path.as_path()));
        assert!(res.elf_changed);
        // An ELF at the path that is not the one sampled is not used either.
        let other = dir.path().join("other-elf");
        std::fs::write(&other, b"\x7fELF not this build").unwrap();
        let rebuilt_run = SampleRun {
            elf: other.clone(),
            elf_hash: Some(elf_fingerprint(b"\x7fELF the sampled build")),
            ..read_run(&path_copy).unwrap()
        };
        assert!(process(rebuilt_run, None).elf_changed);
        let same = SampleRun {
            elf: other,
            elf_hash: Some(elf_fingerprint(b"\x7fELF not this build")),
            ..read_run(&path_copy).unwrap()
        };
        assert!(!process(same, None).elf_changed);
        // Not a saved run.
        std::fs::write(&path, br#"{"format": "something else"}"#).unwrap();
        assert!(read_run(&path).is_err());
    }

    /// The way out of ROM against a real ESP32-C3 release ELF. Ignored and
    /// path-driven - set `EIDE_RISCV_ELF` to a `riscv32*` release binary:
    ///
    /// `EIDE_RISCV_ELF=… cargo test -- --ignored a_real_riscv_elf`
    ///
    /// Every `auipc ra` + `jalr ra` in the image that lands OUTSIDE it (in ROM)
    /// must decode to a call whose target is a named ROM routine.
    #[test]
    #[ignore]
    fn a_real_riscv_elf_names_its_rom_calls() {
        use object::{Object, ObjectSection};
        let Some(elf) = std::env::var_os("EIDE_RISCV_ELF") else {
            println!("EIDE_RISCV_ELF not set - nothing to check");
            return;
        };
        let data = std::fs::read(&elf).unwrap();
        let mut lines = ElfLines::new(&data).expect("an ELF with DWARF");
        let obj = object::File::parse(&*data).unwrap();
        let text = obj.section_by_name(".text").expect("a .text section");
        let (base, code) = (text.address(), text.data().unwrap());
        let mut found: HashMap<String, usize> = HashMap::new();
        // Who calls what: the innermost line the ELF has for each call site.
        let mut callers: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
            Default::default();
        let mut unnamed = Vec::new();
        let (mut with_row, mut rows_checked) = (0, 0);
        let mut no_row = Vec::new();
        // Function starts, and one call site of each function called directly.
        let mut starts: Vec<u64> = lines
            .symbols
            .keys()
            .copied()
            .filter(|&a| lines.in_image(a))
            .collect();
        starts.sort_unstable();
        let mut called_from: HashMap<u64, u64> = HashMap::new();
        for p in (0..code.len().saturating_sub(4)).step_by(2) {
            let ret = base + p as u64 + 4;
            if let Some(Some(t)) = lines.call_before(ret)
                && lines.in_image(t)
            {
                called_from.entry(t).or_insert(ret);
            }
        }
        let (mut walked, mut flash_walked, mut walk_wrong) = (0, 0, Vec::new());
        let mut i = 0;
        while i + 8 <= code.len() {
            let w = |o: usize| u32::from_le_bytes([code[o], code[o + 1], code[o + 2], code[o + 3]]);
            let (a, b) = (w(i), w(i + 4));
            // auipc ra, hi ; jalr ra, lo(ra)
            if a & 0xfff == 0x097 && b & 0x000f_ffff == 0x0000_80e7 {
                let ret = base + i as u64 + 8;
                if let Some(Some(target)) = lines.call_before(ret)
                    && !lines.in_image(target)
                {
                    // The caller's CFI row, as `walk` will read it - for every
                    // caller with lines; the boot code without DWARF has none.
                    if !lines.chain(ret - 1).is_empty() {
                        rows_checked += 1;
                        match lines.cfi_row(ret - 1) {
                            Some(row) if matches!(row.cfa_reg, 2 | 8) && row.ra_at.is_some() => {
                                with_row += 1;
                                // Halted in that ROM routine: a stack whose only
                                // known word is the caller's saved ra, set to a
                                // real call site of the caller. `unwind_halt`
                                // must find both, through the real CFI row.
                                let f = starts[starts.partition_point(|&a| a <= ret) - 1];
                                if row.cfa_reg == 2
                                    && let Some(&outer) = called_from.get(&f)
                                {
                                    let sp = 0x3fc8_0000u64;
                                    let slot =
                                        sp.wrapping_add_signed(row.cfa_off + row.ra_at.unwrap());
                                    let regs = RiscvRegs { ra: ret, sp, s0: 0 };
                                    let mut word = |a: u64| (a == slot).then_some(outer);
                                    let got = lines.unwind_halt(target, regs, &mut word, 2);
                                    walked += 1;
                                    // Halted in the caller itself, right after
                                    // that call: the row AT the PC, ra from
                                    // the stack, the register ignored.
                                    if let Some(at) = lines.cfi_row(ret)
                                        && at.cfa_reg == 2
                                        && let Some(ra_at) = at.ra_at
                                    {
                                        let slot = sp.wrapping_add_signed(at.cfa_off + ra_at);
                                        let regs = RiscvRegs { ra: 0, sp, s0: 0 };
                                        let mut word = |a: u64| (a == slot).then_some(outer);
                                        let here = lines.unwind_halt(ret, regs, &mut word, 1);
                                        flash_walked += 1;
                                        if here.as_ref().map(|u| u.returns.as_slice())
                                            != Some(&[outer])
                                        {
                                            walk_wrong.push(format!("flash {ret:#x}: {here:?}"));
                                        }
                                    }
                                    if got.as_ref().map(|u| u.returns.as_slice())
                                        != Some(&[ret, outer])
                                    {
                                        walk_wrong.push(format!("{:#x}: {got:?}", ret - 8));
                                    }
                                }
                            }
                            row => no_row.push(format!("{:#x} {row:?}", ret - 8)),
                        }
                    }
                    match lines.symbol_name(target) {
                        Some(name) => {
                            *found.entry(name.clone()).or_default() += 1;
                            let at = lines.chain(ret - 1).into_iter().find_map(|s| {
                                let path = s.path?.replace('\\', "/");
                                let tail: Vec<&str> = path.rsplit('/').take(2).collect();
                                Some(format!("{}/{}:{}", tail[1..].join("/"), tail[0], s.line?))
                            });
                            if let Some(at) = at {
                                callers.entry(name).or_default().insert(at);
                            }
                        }
                        None => unnamed.push(format!("{:#x} -> {target:#x}", ret - 8)),
                    }
                }
            }
            i += 2;
        }
        println!("ROM calls by routine: {found:?}");
        for (name, at) in &callers {
            println!(
                "  {name} <- {}",
                at.iter().cloned().collect::<Vec<_>>().join(", ")
            );
        }
        println!("{with_row} of {rows_checked} ROM callers have a CFI row with a saved ra");
        println!("{walked} ROM halts unwound through a real CFI row to the caller's caller");
        println!("{flash_walked} flash halts unwound through the row at the PC to the caller");
        assert!(flash_walked > 0, "no flash halt to walk from");
        assert!(walked > 0, "no ROM call site to walk from");
        assert!(walk_wrong.is_empty(), "wrong unwinds: {walk_wrong:?}");
        assert!(!found.is_empty(), "no call into ROM found");
        assert!(unnamed.is_empty(), "unnamed ROM targets: {unnamed:?}");
        assert_eq!(
            with_row, rows_checked,
            "callers without a usable CFI row: {no_row:?}"
        );
    }

    #[test]
    fn the_walk_reads_return_addresses_out_of_the_stack_copy() {
        // Two frames: `f` (CFA = sp + 16, ra at CFA - 4) returning into `g`
        // (CFA = s0 + 0, ra at CFA - 4, s0 at CFA - 8), and `g`'s caller
        // without a row - where the walk ends.
        let row_at = |at: u64| match at {
            0x4200_100f => Some(CfiRow {
                cfa_reg: 2,
                cfa_off: 16,
                ra_at: Some(-4),
                s0_at: Some(-8),
            }),
            0x4200_201f => Some(CfiRow {
                cfa_reg: 8,
                cfa_off: 0,
                ra_at: Some(-4),
                s0_at: None,
            }),
            _ => None,
        };
        let sp = 0x3fc8_0000u64;
        let mut bytes = vec![0u8; 64];
        let mut put = |at: u64, v: u32| {
            let o = (at - sp) as usize;
            bytes[o..o + 4].copy_from_slice(&v.to_le_bytes());
        };
        put(sp + 12, 0x4200_2020); // f's saved ra
        put(sp + 8, 0x3fc8_0030); // f's saved s0 = g's frame pointer
        put(0x3fc8_002c, 0x4200_3030); // g's saved ra
        let stack = StackCopy {
            chunks: vec![(sp, bytes)],
        };
        let mut word = |a: u64| stack.word(a);
        let regs = RiscvRegs {
            ra: 0x4200_1010,
            sp,
            s0: 0,
        };
        let out = walk(0x4200_1010, regs, 64, row_at, |_| true, &mut word);
        assert_eq!(out, [0x4200_2020, 0x4200_3030]);
        // A step that does not land right after a call stops the walk.
        let out = walk(
            0x4200_1010,
            regs,
            64,
            row_at,
            |r| r != 0x4200_3030,
            &mut word,
        );
        assert_eq!(out, [0x4200_2020]);
        // A saved register that cannot be read stops it too.
        assert!(walk(0x4200_1010, regs, 64, row_at, |_| true, &mut |_| None).is_empty());
        // The cap.
        assert_eq!(
            walk(0x4200_1010, regs, 1, row_at, |_| true, &mut word).len(),
            1
        );
    }

    #[test]
    fn the_walk_goes_through_direct_recursion() {
        // F calls itself from one call site, so every level saves the same
        // return address R; the outermost level returns into G at R2.
        const R: u64 = 0x4200_1010;
        const R2: u64 = 0x4200_2020;
        let row_at = |at: u64| match at {
            // F: CFA = sp + 16, ra at CFA - 4.
            0x4200_100f => Some(CfiRow {
                cfa_reg: 2,
                cfa_off: 16,
                ra_at: Some(-4),
                s0_at: None,
            }),
            // G: CFA = sp + 16 as well.
            0x4200_201f => Some(CfiRow {
                cfa_reg: 2,
                cfa_off: 16,
                ra_at: Some(-4),
                s0_at: None,
            }),
            _ => None,
        };
        let sp = 0x3fc8_0000u64;
        let saved = HashMap::from([(sp + 12, R), (sp + 28, R2), (sp + 44, 0)]);
        let mut word = |a: u64| saved.get(&a).copied();
        let regs = RiscvRegs { ra: R, sp, s0: 0 };
        assert_eq!(walk(R, regs, 64, row_at, |_| true, &mut word), [R, R2]);
        // A row whose CFA does not rise above `sp` is no caller's frame.
        let flat = |_: u64| {
            Some(CfiRow {
                cfa_reg: 2,
                cfa_off: 0,
                ra_at: Some(-4),
                s0_at: None,
            })
        };
        assert!(walk(R, regs, 64, flat, |_| true, &mut |_| Some(R)).is_empty());
    }

    /// A saved run, counted by this version and summed up. Ignored and
    /// path-driven - set `EIDE_RUN_FILE` to a file the Profile tab saved:
    ///
    /// `EIDE_RUN_FILE=… cargo test -- --ignored a_saved_run_file --nocapture`
    #[test]
    #[ignore]
    fn a_saved_run_file_is_counted() {
        let Some(path) = std::env::var_os("EIDE_RUN_FILE") else {
            println!("EIDE_RUN_FILE not set - nothing to count");
            return;
        };
        let run = read_run(Path::new(&path)).unwrap();
        let res = process(run, None);
        let by = &res.by_line;
        let pct = |k: usize| k as f32 * 100.0 / by.samples.max(1) as f32;
        println!(
            "{} samples · idle {:.1}% · outside {:.1}% · no line {:.1}% · ELF {}",
            by.samples,
            pct(by.idle),
            pct(by.outside),
            pct(by.no_line),
            if res.elf_changed { "not used" } else { "used" }
        );
        for (f, k) in &by.outside_fns {
            println!("  outside {k:>4}  {f}");
        }
        for l in by.lines.iter().take(15) {
            println!(
                "  {:>5.1}% self {:>5.1}% total  {}:{}  {}",
                pct(l.self_n),
                pct(l.total_n),
                l.file,
                l.line,
                l.code
            );
        }
        assert_eq!(
            by.lines.iter().map(|l| l.self_n).sum::<usize>() + by.idle + by.outside + by.no_line,
            by.samples
        );
    }

    #[test]
    fn read_memory_bytes_are_base64() {
        assert_eq!(
            from_base64("AQIDBA==").as_deref(),
            Some(&[1u8, 2, 3, 4][..])
        );
        assert_eq!(from_base64("TWFu").as_deref(), Some(&b"Man"[..]));
        assert_eq!(from_base64("").as_deref(), Some(&[][..]));
        assert_eq!(from_base64("not base64!"), None);
        let copy = StackCopy {
            chunks: vec![
                (0x100, vec![0x78, 0x56, 0x34, 0x12, 0xff]),
                (0x200, vec![1, 0, 0, 0]),
            ],
        };
        assert_eq!(copy.word(0x100), Some(0x1234_5678));
        assert_eq!(copy.word(0x200), Some(1));
        assert_eq!(copy.word(0x102), None);
        assert_eq!(copy.word(0xfc), None);
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
        let run = SampleRun {
            texts,
            ..SampleRun::default()
        };
        let res = result_of(&[vec![at("main", "src/main.rs", 2)]], run, None, false);
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
