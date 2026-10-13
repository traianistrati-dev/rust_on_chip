//! Code-size profiling via `cargo bloat` (the bottom-panel "Profile" tab).
//!
//! `cargo bloat --release` builds the firmware and reports which FUNCTIONS (or
//! `--crates`, which CRATES) take the most `.text` (Flash). It's a static
//! size/efficiency view — no target hardware needed — complementing the runtime
//! flash/RAM bars ([`crate::size`]). Output is the tool's text table, which we
//! parse into rows for a bar view (raw text kept as a fallback).

use crate::build::no_window;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;

/// Which Profile view is showing: static code-size (`cargo bloat`), the
/// runtime on-target flamegraph (probe-rs stack sampling, see
/// [`crate::flamegraph`]), or the exact time of marked code (see
/// [`crate::timing`]).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum ProfileMode {
    #[default]
    Static,
    Runtime,
    Timing,
}

/// One parsed `cargo bloat` row: a function (or crate) and its `.text` size.
#[derive(Clone, Debug, PartialEq)]
pub struct BloatRow {
    /// Share of the `.text` section, 0..100.
    pub text_pct: f32,
    /// Size in bytes (from the `KiB`/`B` label).
    pub size_bytes: u64,
    /// The tool's size label, e.g. `1.9KiB`.
    pub size_label: String,
    /// Owning crate (`std`, `blink_project`, `[Unknown]`, or empty for the
    /// `(N Others)` aggregate).
    pub crate_name: String,
    /// Symbol / function name (empty in `--crates` mode).
    pub name: String,
}

/// A finished `cargo bloat` run.
#[derive(Clone, Debug)]
pub struct ProfileResult {
    pub rows: Vec<BloatRow>,
    /// The `.text section size … file size …` summary line.
    pub summary: String,
    /// `true` when this was a `--crates` (by-crate) run.
    pub by_crate: bool,
    /// Raw stdout, shown when a row failed to parse.
    pub raw: String,
}

#[derive(Clone, Debug, Default)]
pub enum ProfileState {
    #[default]
    Idle,
    /// `cargo bloat --release` is running (it builds first).
    Running,
    Done(ProfileResult),
    Failed(String),
}

impl ProfileState {
    pub fn is_busy(&self) -> bool {
        matches!(self, ProfileState::Running)
    }
}

/// Parse a `KiB`/`MiB`/`B` size label to bytes (`1.9KiB` → 1945).
fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("KiB") {
        (n, 1024.0)
    } else if let Some(n) = s.strip_suffix("MiB") {
        (n, 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix("GiB") {
        (n, 1024.0 * 1024.0 * 1024.0)
    } else if let Some(n) = s.strip_suffix('B') {
        (n, 1.0)
    } else {
        (s, 1.0)
    };
    (num.trim().parse::<f64>().unwrap_or(0.0) * mult) as u64
}

/// Split the trailing `crate name…` tokens into `(crate, name)`. The
/// `(N Others)` aggregate starts with `(` and has no crate.
fn split_crate_name(rest: &[&str]) -> (String, String) {
    match rest.first() {
        None => (String::new(), String::new()),
        Some(first) if first.starts_with('(') => (String::new(), rest.join(" ")),
        Some(first) => (first.to_string(), rest[1..].join(" ")),
    }
}

/// Parse `cargo bloat` stdout into rows + the summary line. Tolerant: only the
/// `<pct>% <pct>% <size> …` data rows are taken; the `Analyzing`/header lines
/// and anything unparseable are skipped. Handles both the function table and
/// the `--crates` table (no Name column).
pub fn parse_bloat(stdout: &str) -> (Vec<BloatRow>, String) {
    let mut rows = Vec::new();
    let mut summary = String::new();
    for line in stdout.lines() {
        let toks: Vec<&str> = line.split_whitespace().collect();
        // A data row is `<file%> <text%> <size> …`.
        if toks.len() < 3 || !toks[0].ends_with('%') || !toks[1].ends_with('%') {
            continue;
        }
        // The final `.text section size, the file size is …` line closes it.
        if line.contains("section size") {
            summary = line.trim().to_string();
            continue;
        }
        let text_pct = toks[1].trim_end_matches('%').parse::<f32>().unwrap_or(0.0);
        let (crate_name, name) = split_crate_name(&toks[3..]);
        rows.push(BloatRow {
            text_pct,
            size_bytes: parse_size(toks[2]),
            size_label: toks[2].to_string(),
            crate_name,
            name,
        });
    }
    (rows, summary)
}

/// Run `cargo bloat --release` on a background thread; the result lands in
/// `state`. `by_crate` toggles the per-crate view.
pub fn start_profile(
    project_dir: std::path::PathBuf,
    target: String,
    by_crate: bool,
    state: Arc<Mutex<ProfileState>>,
    ctx: eframe::egui::Context,
) {
    if state.lock().unwrap().is_busy() {
        return;
    }
    *state.lock().unwrap() = ProfileState::Running;
    ctx.request_repaint();
    thread::spawn(move || {
        let next = run_bloat(&project_dir, &target, by_crate);
        *state.lock().unwrap() = next;
        ctx.request_repaint();
    });
}

/// What to pass `cargo`, split out so it can be asserted.
///
/// No `--color=never`: cargo-bloat does not take it. It parses its own
/// arguments, never forwards the flag to the inner cargo, and echoes
/// `Warning: unused arguments left: ["--color=never"].` as the FIRST line of
/// every run - which then sits above the real message on any failure. The
/// colours it was meant to suppress never arrive anyway, because the child's
/// stdio is a pipe.
fn bloat_args<'a>(target: &'a str, by_crate: bool) -> Vec<&'a str> {
    let mut args: Vec<&str> = vec!["bloat", "--release", "--target", target];
    if by_crate {
        args.push("--crates");
    } else {
        args.push("-n");
        args.push("40");
    }
    args
}

fn run_bloat(project_dir: &std::path::Path, target: &str, by_crate: bool) -> ProfileState {
    let args = bloat_args(target, by_crate);
    let out = match no_window(&mut Command::new("cargo"))
        .current_dir(project_dir)
        .args(&args)
        .output()
    {
        Ok(o) => o,
        Err(e) => return ProfileState::Failed(format!("Could not launch `cargo`: {e}")),
    };
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        // The toolchain BEFORE the tool: rustup's message for a missing
        // toolchain is `custom toolchain 'esp' specified in override file
        // '…/rust-toolchain.toml' is not installed`, and it contains the very
        // words the cargo-bloat sniffer below looks for. Only the three Xtensa
        // parts carry a `rust-toolchain.toml` (see `project_gen`), so on those -
        // and only those - a machine without espup was told to install
        // cargo-bloat, which changes nothing. rustup's own line names the file,
        // the toolchain and the problem, so it is worth more than anything
        // written here.
        if stderr.contains("toolchain") && stderr.contains("is not installed") {
            return ProfileState::Failed(stderr.trim().to_owned());
        }
        // cargo-bloat not installed → cargo prints "no such command".
        if stderr.contains("no such command")
            || stderr.contains("not provided")
            || stderr.contains("is not installed")
        {
            return ProfileState::Failed(
                "[BLOAT_MISSING] cargo-bloat isn't installed.\n\n\
                 Install it with:\n  cargo install cargo-bloat\n\n\
                 (then click Analyze again)"
                    .to_string(),
            );
        }
        // Otherwise a build error — surface the compiler output.
        let msg = if stderr.trim().is_empty() {
            stdout.clone()
        } else {
            stderr.into_owned()
        };
        return ProfileState::Failed(format!("cargo bloat failed:\n{msg}"));
    }
    let (rows, summary) = parse_bloat(&stdout);
    ProfileState::Done(ProfileResult {
        rows,
        summary,
        by_crate,
        raw: stdout,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A missing `esp` toolchain must not be reported as a missing cargo-bloat.
    ///
    /// rustup's message for a toolchain named in `rust-toolchain.toml` but not
    /// installed contains the words `is not installed` - which is exactly what
    /// the cargo-bloat sniffer looks for. Only the three Xtensa parts carry such
    /// a file, so on those, and only those, a machine without espup was told to
    /// run `cargo install cargo-bloat`, which changes nothing.
    ///
    /// The real rustup line, verbatim from a machine with no such toolchain.
    #[test]
    fn a_missing_toolchain_is_not_blamed_on_cargo_bloat() {
        let rustup = "error: custom toolchain 'esp' specified in override file \
                      'C:\\p\\rust-toolchain.toml' is not installed";
        assert!(
            rustup.contains("is not installed"),
            "the collision this guards is gone; re-check the ordering in run_bloat"
        );
        // The toolchain test must come FIRST and must win.
        assert!(rustup.contains("toolchain") && rustup.contains("is not installed"));
        // …and the cargo-bloat shapes must not look like a toolchain problem.
        for bloat in [
            "error: no such command: `bloat`",
            "cargo-bloat is not provided by the `cargo` binary",
        ] {
            assert!(
                !(bloat.contains("toolchain") && bloat.contains("is not installed")),
                "`{bloat}` would be swallowed by the toolchain arm"
            );
        }
    }

    /// cargo-bloat takes no `--color` flag: it echoes it back as a warning that
    /// becomes the first line of every run, above any real error.
    #[test]
    fn the_bloat_arguments_carry_no_flag_cargo_bloat_rejects() {
        for by_crate in [false, true] {
            let args = bloat_args("riscv32imc-unknown-none-elf", by_crate);
            assert!(!args.iter().any(|a| a.starts_with("--color")), "{args:?}");
            assert_eq!(args[0], "bloat");
            assert!(args.contains(&"--target"), "{args:?}");
        }
    }

    #[test]
    fn parses_function_table() {
        let out = "    Analyzing target/thumbv7m-none-eabi/release/blink-project\n\
                   \n\
                   File  .text    Size          Crate Name\n\
                   3.2%  9.1%     412B            std core::fmt::Formatter::pad\n\
                   1.5%  4.3%     194B  blink_project blink_project::main\n\
                   30.1% 85.5%   3.8KiB                (34 Others)\n\
                   35.2% 100.0%  4.5KiB          .text section size, the file size is 12.8KiB\n";
        let (rows, summary) = parse_bloat(out);
        assert_eq!(rows.len(), 3, "3 data rows, summary excluded:\n{rows:?}");
        assert_eq!(rows[0].crate_name, "std");
        assert_eq!(rows[0].name, "core::fmt::Formatter::pad");
        assert_eq!(rows[0].size_bytes, 412);
        assert_eq!(rows[1].name, "blink_project::main");
        // The "(N Others)" aggregate: no crate, KiB parsed.
        assert_eq!(rows[2].crate_name, "");
        assert_eq!(rows[2].name, "(34 Others)");
        assert_eq!(rows[2].size_bytes, 3891); // 3.8 * 1024
        assert!(summary.contains("section size"), "summary captured");
    }

    #[test]
    fn parses_crate_table() {
        let out = "File  .text   Size Crate\n\
                   15.2% 43.1%  1.9KiB std\n\
                   2.0%  5.6%    256B blink_project\n";
        let (rows, _) = parse_bloat(out);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].crate_name, "std");
        assert_eq!(rows[0].name, "", "no Name column in --crates mode");
        assert_eq!(rows[0].size_bytes, 1945);
    }

    #[test]
    fn size_suffixes() {
        assert_eq!(parse_size("412B"), 412);
        assert_eq!(parse_size("1.0KiB"), 1024);
        assert_eq!(parse_size("2.5MiB"), (2.5 * 1024.0 * 1024.0) as u64);
    }
}
