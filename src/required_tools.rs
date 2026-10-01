//! Tool availability checker and one-click installer.
//!
//! Each [`RequiredTool`] entry knows how to verify and (where possible) install
//! a dependency on the host machine.  All blocking operations run in background
//! threads and write back through the shared [`Arc<Mutex<ToolsState>>`], then
//! call `ctx.request_repaint()` so the UI stays in sync without polling.
//!
//! # Tool catalog
//!
//! | Tool                        | Toolchain    | Auto-install | Platforms |
//! |-----------------------------|--------------|--------------|-----------|
//! | rustup                      | All          | No (manual)  | all       |
//! | rustc                       | All          | Yes          | all       |
//! | git                         | All          | No (manual)  | all       |
//! | cargo-bloat                 | All          | Yes (cargo)  | all       |
//! | host C toolchain            | All          | Win only     | all †     |
//! | serial port access          | All          | No (manual)  | Linux     |
//! | Cortex-M targets (4, gated) | RustEmbedded | Yes          | all       |
//! | probe-rs                    | RustEmbedded | Yes (cargo)  | all       |
//! | dfu-util                    | RustEmbedded | Win + macOS  | all       |
//! | openocd                     | RustEmbedded | Win + macOS  | all       |
//! | objcopy                     | RustEmbedded | Yes (cargo)  | all       |
//! | USB probe udev rules        | RustEmbedded | No (needs root) | Linux  |
//! | USB probe driver (Zadig)    | All          | No (manual)  | Windows   |
//! | riscv32imc-unknown-none-elf | EspRust      | Yes          | all       |
//! | rust-src component          | EspRust      | Yes          | all       |
//! | espflash                    | EspRust      | Yes (cargo)  | all       |
//!
//! † The host C toolchain is a different beast per platform — MSVC on Windows
//! (file-probed, see [`MSVC_CHECK`]), `cc` from the Xcode Command Line Tools on
//! macOS, `cc` from build-essential on Linux — so it is ONE catalog entry whose
//! name, check and installer are chosen for the host.
//!
//! # Per-platform policy
//!
//! Auto-install is offered only where it can succeed **without root**: `cargo` /
//! `rustup` everywhere, `winget` on Windows, `brew` on macOS. Linux package
//! managers need root AND differ per distro (apt / dnf / pacman), so those
//! entries are deliberately manual — a button that always fails with a sudo
//! prompt the IDE can't answer is worse than a link that tells you the command.

use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use eframe::egui;
use std::{
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

// ── ToolStatus ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default, PartialEq)]
pub enum ToolStatus {
    #[default]
    Unknown,
    Checking,
    /// Tool found; inner string is the version reported by the tool.
    Ok(String),
    Missing,
    Installing,
    /// Last check or install failed; inner string is a short error message.
    Failed(String),
    /// Present, but older than the version this IDE needs. Deliberately NOT the
    /// same as missing: the tool may well still work, so it warns rather than
    /// disabling the features that use it.
    Outdated {
        found: String,
        min: &'static str,
    },
}

impl ToolStatus {
    pub fn is_busy(&self) -> bool {
        matches!(self, ToolStatus::Checking | ToolStatus::Installing)
    }

    pub fn label(&self) -> &str {
        match self {
            ToolStatus::Unknown => "—",
            ToolStatus::Checking => "Checking…",
            ToolStatus::Ok(_) => "OK",
            ToolStatus::Missing => "Missing",
            ToolStatus::Installing => "Installing…",
            ToolStatus::Failed(_) => "Failed",
            ToolStatus::Outdated { .. } => "Outdated",
        }
    }

    pub fn color(&self) -> egui::Color32 {
        match self {
            ToolStatus::Ok(_) => egui::Color32::from_rgb(80, 200, 100),
            ToolStatus::Missing => egui::Color32::from_rgb(230, 160, 50),
            ToolStatus::Failed(_) => egui::Color32::from_rgb(220, 70, 60),
            ToolStatus::Checking | ToolStatus::Installing => egui::Color32::from_rgb(180, 180, 80),
            ToolStatus::Outdated { .. } => egui::Color32::from_rgb(215, 165, 70),
            ToolStatus::Unknown => egui::Color32::GRAY,
        }
    }
}

// ── RequiredTool ───────────────────────────────────────────────────────────────

/// How badly a missing tool hurts — drives the startup banner (which only
/// reports [`Severity::Blocking`]) and the ordering in the Tools tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    /// Loud enough for the startup banner. Usually "nothing builds without it"
    /// — but also the rarer case of a problem that breaks only one runtime and
    /// is undiagnosable from the error it produces (see the `CARGO_FEATURE_*`
    /// entry). The distinction the variant really draws is how hard it is to
    /// find, not how much it takes down.
    Blocking,
    /// One feature / tab stops working; the rest of the IDE is fine.
    Feature,
    /// Convenience only.
    Optional,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Blocking => "required",
            Severity::Feature => "feature",
            Severity::Optional => "optional",
        }
    }
    pub fn color(self) -> egui::Color32 {
        match self {
            Severity::Blocking => egui::Color32::from_rgb(230, 110, 100),
            Severity::Feature => egui::Color32::from_rgb(220, 180, 90),
            Severity::Optional => egui::Color32::from_gray(150),
        }
    }
}

pub struct RequiredTool {
    pub name: &'static str,
    pub description: &'static str,
    /// `None` = required for all toolchains.
    pub toolchain: Option<ToolchainKind>,
    /// Narrower than [`RequiredTool::toolchain`]: the project's target triple
    /// must START WITH this, or the tool is not relevant.
    ///
    /// `ToolchainKind::EspRust` covers six RISC-V chips AND three Xtensa ones,
    /// which need entirely different things — `rustup target add` versus
    /// Espressif's rustc fork. Without this, an ESP32-C6 user was told to
    /// install `riscv32imc` (which their chip does not use) and never told about
    /// `riscv32imac` (which it does).
    pub only_for_target: Option<&'static str>,
    /// How much breaks without it.
    pub severity: Severity,
    /// What the user LOSES when it's missing, in plain words — the answer to
    /// "why do I need this?" (shown in the Tools tab + the startup banner).
    pub impact: &'static str,
    // ── Check ────────────────────────────────────────────────────────────────
    pub check_cmd: &'static str,
    pub check_args: &'static [&'static str],
    /// If non-empty: stdout+stderr must contain this substring after a
    /// successful exit code for the tool to be considered present.
    /// Used for `rustup target list --installed` pattern checks.
    pub check_pattern: &'static str,
    /// Lowest version this IDE is known to need, e.g. `"1.74"`. `None` = don't
    /// version-check — the honest default: an invented minimum would nag users
    /// whose older build works fine. Only set it where the requirement is real
    /// and documented.
    pub min_version: Option<&'static str>,
    // ── Install ──────────────────────────────────────────────────────────────
    /// `None` = cannot be auto-installed; direct the user to `manual_url`.
    pub install_cmd: Option<&'static str>,
    pub install_args: &'static [&'static str],
    pub manual_url: &'static str,
    // ── Runtime state (mutated by background threads) ─────────────────────────
    pub status: ToolStatus,
}

// ── ToolsState ─────────────────────────────────────────────────────────────────

pub struct ToolsState {
    pub tools: Vec<RequiredTool>,
    pub log: Vec<String>,
}

impl ToolsState {
    fn push_log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
    }

    /// Same, for the Tools TAB — the udev "Generate rules…" action reports
    /// through the shared log rather than a dialog, so its output stays
    /// selectable for the rest of the session.
    pub fn push_log_public(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
    }

    pub fn any_busy(&self) -> bool {
        self.tools.iter().any(|t| t.status.is_busy())
    }

    /// Every tool that is confirmed broken (Missing / Failed / Outdated) —
    /// `Unknown` and the busy states don't count, so an unchecked catalog
    /// reports nothing. `toolchain` filters to the chip in use (`None` = don't
    /// filter). `Outdated` warns here but never disables a feature (see
    /// [`Self::unavailable`]).
    pub fn problems(
        &self,
        toolchain: Option<&ToolchainKind>,
    ) -> Vec<(&'static str, Severity, &'static str)> {
        self.problems_for(toolchain, None)
    }

    /// [`Self::problems`], narrowed to a project's target triple.
    ///
    /// `None` keeps every target-specific tool, which is what an unopened
    /// project should see: it is better to list a target the user may not need
    /// than to hide the one they do.
    pub fn problems_for(
        &self,
        toolchain: Option<&ToolchainKind>,
        target: Option<&str>,
    ) -> Vec<(&'static str, Severity, &'static str)> {
        self.tools
            .iter()
            .filter(|t| {
                matches!(
                    t.status,
                    ToolStatus::Missing | ToolStatus::Failed(_) | ToolStatus::Outdated { .. }
                )
            })
            .filter(|t| match (&t.toolchain, toolchain) {
                (Some(tc), Some(sel)) => tc == sel,
                (Some(_), None) => false, // toolchain-specific, no chip selected
                (None, _) => true,        // needed by everything
            })
            .filter(|t| match (t.only_for_target, target) {
                (Some(prefix), Some(sel)) => target_gate_matches(prefix, sel),
                (Some(_), None) => true, // no project open — show it anyway
                (None, _) => true,
            })
            .map(|t| (t.name, t.severity, t.impact))
            .collect()
    }

    /// Blocking problems only — what the startup banner reports.
    pub fn blocking_problems(
        &self,
        toolchain: Option<&ToolchainKind>,
    ) -> Vec<(&'static str, Severity, &'static str)> {
        self.problems(toolchain)
            .into_iter()
            .filter(|(_, s, _)| *s == Severity::Blocking)
            .collect()
    }

    /// The specific finding behind an entry's status, when its check produced
    /// one — what `impact` cannot say because it is a fixed string written
    /// before anything was probed.
    ///
    /// `Missing` has none by definition (the tool simply was not there), which
    /// is why this returns an `Option` rather than a string for every entry.
    pub fn status_detail(&self, name: &str) -> Option<String> {
        self.tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| match &t.status {
                ToolStatus::Failed(msg) => Some(msg.clone()),
                ToolStatus::Outdated { found, min } => {
                    Some(format!("found {found}, needs {min} or newer"))
                }
                _ => None,
            })
    }

    /// Is any blocking problem a tool that simply is not installed?
    ///
    /// The banner's "installed it just now? re-check in Tools" hint is only
    /// true for those. An entry that failed for another reason — a stray
    /// environment variable, an incomplete MSVC toolchain — is not fixed by
    /// installing anything, and re-checking a variable this process inherited
    /// at startup cannot clear it either: that needs a restart, which is the
    /// opposite of what the hint suggests.
    pub fn any_blocking_missing(&self, toolchain: Option<&ToolchainKind>) -> bool {
        self.any_blocking_missing_for(toolchain, None)
    }

    /// [`Self::any_blocking_missing`], narrowed to a project's target triple.
    pub fn any_blocking_missing_for(
        &self,
        toolchain: Option<&ToolchainKind>,
        target: Option<&str>,
    ) -> bool {
        self.tools
            .iter()
            .filter(|t| t.severity == Severity::Blocking)
            .filter(|t| matches!(t.status, ToolStatus::Missing))
            .filter(|t| match (t.only_for_target, target) {
                (Some(prefix), Some(sel)) => target_gate_matches(prefix, sel),
                _ => true,
            })
            .any(|t| match (&t.toolchain, toolchain) {
                (Some(tc), Some(sel)) => tc == sel,
                (Some(_), None) => false,
                (None, _) => true,
            })
    }

    /// Names of the tools CONFIRMED unusable (`Missing` / `Failed`). Deliberately
    /// excludes `Unknown` and the busy states: a feature must never be greyed out
    /// just because the check hasn't run (or couldn't run) yet — the UI stays
    /// permissive until there is proof of a problem. Used to gate the buttons
    /// that shell out to that tool.
    pub fn unavailable(&self) -> Vec<&'static str> {
        self.tools
            .iter()
            .filter(|t| matches!(t.status, ToolStatus::Missing | ToolStatus::Failed(_)))
            .map(|t| t.name)
            .collect()
    }

    /// Count tools that are Missing or Failed AND have an auto-installer.
    pub fn missing_installable_count(&self) -> usize {
        self.tools
            .iter()
            .filter(|t| {
                matches!(t.status, ToolStatus::Missing | ToolStatus::Failed(_))
                    && t.install_cmd.is_some()
            })
            .count()
    }
}

// ── ToolRow – lock-free snapshot for rendering ─────────────────────────────────

/// A cheap snapshot of one tool row used for lock-free egui rendering.
pub struct ToolRow {
    pub name: &'static str,
    pub description: &'static str,
    pub toolchain: Option<ToolchainKind>,
    pub severity: Severity,
    pub impact: &'static str,
    pub status: ToolStatus,
    pub can_auto_install: bool,
    pub manual_url: &'static str,
}

impl ToolsState {
    pub fn snapshot(&self) -> Vec<ToolRow> {
        self.tools
            .iter()
            .map(|t| ToolRow {
                name: t.name,
                description: t.description,
                toolchain: t.toolchain.clone(),
                severity: t.severity,
                impact: t.impact,
                status: t.status.clone(),
                can_auto_install: t.install_cmd.is_some(),
                manual_url: t.manual_url,
            })
            .collect()
    }
}

// ── Per-platform selection ─────────────────────────────────────────────────────

/// Catalog name of the Linux udev-rules entry. A constant because the Tools tab
/// matches on it to offer "Generate rules…" — a typo there would silently drop
/// the only action that entry has.
pub const UDEV_RULES_TOOL: &str = "USB probe udev rules";

/// Catalog name of the Windows USB-driver entry - the counterpart of
/// [`UDEV_RULES_TOOL`], since the two answer the same question ("can this
/// machine OPEN a debug probe?") through completely different machinery.
pub const PROBE_DRIVER_TOOL: &str = "USB probe driver (Zadig)";

/// Pick the value for the host OS. Everything that is not Windows or macOS is
/// treated as Linux — the other unixes this could run on use the same package
/// managers and the same udev/serial-group story.
///
/// A plain runtime `if` rather than `#[cfg]` on every field: the catalog is
/// built once at startup, and keeping all three variants **visible in one place**
/// is what stops a platform from quietly losing an entry.
fn per_os<T>(windows: T, macos: T, linux: T) -> T {
    if cfg!(target_os = "windows") {
        windows
    } else if cfg!(target_os = "macos") {
        macos
    } else {
        linux
    }
}

// ── Tool catalog ───────────────────────────────────────────────────────────────

pub fn make_tools_state() -> Arc<Mutex<ToolsState>> {
    #[allow(unused_mut)]
    let mut tools = vec![
        // ── Common to all toolchains ─────────────────────────────────────
        RequiredTool {
            name: "rustup",
            description: "Rust toolchain installer — manages Rust versions and targets",
            toolchain: None,
            only_for_target: None,
            severity: Severity::Blocking,
            impact: "No Rust toolchain management: targets can't be installed and nothing builds.",
            check_cmd: "rustup",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: None, // must be installed manually from rustup.rs
            install_args: &[],
            manual_url: "https://rustup.rs",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "rustc",
            description: "Rust compiler (stable toolchain)",
            toolchain: None,
            only_for_target: None,
            severity: Severity::Blocking,
            impact: "No Rust compiler: Build, Check, Clippy and Flash all fail.",
            check_cmd: "rustc",
            check_args: &["--version"],
            check_pattern: "",
            min_version: Some("1.74"), // Cargo `[lints]` table (strict-lints)
            install_cmd: Some("rustup"),
            install_args: &["install", "stable"],
            manual_url: "https://www.rust-lang.org/tools/install",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "git",
            description: "Version control — powers the Git tab (commit / push / pull)",
            toolchain: None,
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The Git tab (commit / push / pull) and library cloning are unavailable.",
            check_cmd: "git",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: None, // installed manually from git-scm.com
            install_args: &[],
            manual_url: "https://git-scm.com",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "cargo-bloat",
            description: "Code-size profiler — powers the Profile tab (.text/Flash per function)",
            toolchain: None,
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The Profile tab's Static (size) view can't run; the rest of the IDE is fine.",
            check_cmd: "cargo",
            check_args: &["bloat", "--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: Some("cargo"),
            install_args: &["install", "cargo-bloat"],
            manual_url: "https://github.com/RazrFalcon/cargo-bloat",
            status: ToolStatus::Unknown,
        },
        // ── RustEmbedded (STM32 / ARM Cortex-M) ─────────────────────────
        // FOUR targets, one per Cortex-M architecture the catalog spans, each
        // gated on the project's own triple like the RISC-V pair below. There
        // used to be a single ungated `thumbv7m-none-eabi` entry — the F103's
        // target — marked Blocking for every RustEmbedded chip: a Pico user was
        // told the M3 target blocked their build (it does not), and never told
        // about `thumbv6m-none-eabi` (which does). Every STM32 outside F1 had
        // the same wrong advice.
        //
        // The gates are whole triples, matched up to a `-` boundary
        // (`target_gate_matches`): `thumbv7m-` is not a prefix of `thumbv7em-`,
        // and the soft-float `thumbv7em-none-eabi` does NOT match the
        // hard-float `thumbv7em-none-eabihf`, which a plain prefix test would.
        // The installed check is line-exact for the same reason.
        RequiredTool {
            name: "thumbv6m-none-eabi",
            description: "Rust target for ARM Cortex-M0 / M0+ (STM32C0 / F0 / G0 / L0 / U0, RP2040)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: Some("thumbv6m-none-eabi"),
            severity: Severity::Blocking,
            impact: "This chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "thumbv6m-none-eabi",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "thumbv6m-none-eabi"],
            manual_url: "https://docs.rust-embedded.org/book/intro/install.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "thumbv7m-none-eabi",
            description: "Rust target for ARM Cortex-M3 (STM32F1 / F2 / L1)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: Some("thumbv7m-none-eabi"),
            severity: Severity::Blocking,
            impact: "This chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "thumbv7m-none-eabi",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "thumbv7m-none-eabi"],
            manual_url: "https://docs.rust-embedded.org/book/intro/install.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "thumbv7em-none-eabi",
            description: "Rust target for ARM Cortex-M4 without an FPU (nRF52805 / 52810 / 52811 / 52820)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: Some("thumbv7em-none-eabi"),
            severity: Severity::Blocking,
            impact: "This chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "thumbv7em-none-eabi",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "thumbv7em-none-eabi"],
            manual_url: "https://docs.rust-embedded.org/book/intro/install.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "thumbv7em-none-eabihf",
            description: "Rust target for ARM Cortex-M4F / M7 (STM32F3 / F4 / G4 / L4 / WB / F7 / H7, nRF52)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: Some("thumbv7em-none-eabihf"),
            severity: Severity::Blocking,
            impact: "This chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "thumbv7em-none-eabihf",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "thumbv7em-none-eabihf"],
            manual_url: "https://docs.rust-embedded.org/book/intro/install.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "thumbv8m.main-none-eabihf",
            description: "Rust target for ARM Cortex-M33 (STM32H5 / L5 / U5 / WBA, RP2350)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: Some("thumbv8m.main-none-eabihf"),
            severity: Severity::Blocking,
            impact: "This chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "thumbv8m.main-none-eabihf",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "thumbv8m.main-none-eabihf"],
            manual_url: "https://docs.rust-embedded.org/book/intro/install.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "probe-rs",
            description: "Debug probe runner — powers the Debug + RTT tabs (breakpoints, defmt logs) and SWD/JTAG flashing",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: None,
            severity: Severity::Feature,
            // The version advice is WINDOWS-ONLY: both known-bad releases fail on
            // Windows driver binding (WinUSB), which has no equivalent elsewhere.
            // Repeating it on Linux/macOS would pin those users to an old release
            // for a bug they cannot hit.
            impact: per_os(
                "No RTT logs, on-target Debug, runtime flamegraph or probe-rs flashing. \
                 NEWER IS NOT ALWAYS BETTER here: 0.31.0 panics inside its own USB probe \
                 enumeration, and 0.32.0 can't open an ST-Link bound to the WinUSB driver \
                 (\"reset not supported by WinUSB\"). 0.29.0 is the version verified to work \
                 on such a setup: cargo install probe-rs-tools --locked --version 0.29.0",
                "No RTT logs, on-target Debug, runtime flamegraph or probe-rs flashing.",
                "No RTT logs, on-target Debug, runtime flamegraph or probe-rs flashing. \
                 If it reports no probe while one is plugged in, the tool is fine — \
                 see \"USB probe udev rules\" below.",
            ),
            check_cmd: "probe-rs",
            check_args: &["--version"],
            check_pattern: "",
            // Deliberately NONE. A minimum here would mark a WORKING install
            // (0.29.0) "Outdated" and offer an upgrade that breaks debugging on
            // a WinUSB-bound ST-Link — the failures are version RANGES, not a
            // floor. `failure_hint`'s [PROBE_RS_PANIC] / [PROBE_OPEN_FAILED]
            // name the actual problem when it happens, which is the honest way
            // to handle "some releases are broken for some hardware".
            min_version: None,
            install_cmd: Some("cargo"),
            install_args: &["install", "probe-rs-tools", "--locked"],
            manual_url: "https://probe.rs/docs/getting-started/installation/",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "dfu-util",
            description: "USB DFU flasher — programs an STM32 held in its ROM bootloader",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The Flash tab's DFU (USB bootloader) path can't run; SWD flashing via \
                     probe-rs is unaffected.",
            check_cmd: "dfu-util",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: per_os(Some("winget"), Some("brew"), None),
            install_args: per_os(
                &[
                    "install",
                    "--id",
                    "dfu-util.dfu-util",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ][..],
                &["install", "dfu-util"][..],
                &[][..],
            ),
            manual_url: "https://dfu-util.sourceforge.net/",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "openocd",
            description: "On-chip debugger — the alternative SWD/JTAG flash path",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The Flash tab's OpenOCD path can't run; probe-rs flashing is unaffected.",
            check_cmd: "openocd",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: per_os(Some("winget"), Some("brew"), None),
            install_args: per_os(
                &[
                    "install",
                    "--id",
                    "OpenOCD.OpenOCD",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ][..],
                &["install", "open-ocd"][..],
                &[][..],
            ),
            manual_url: "https://openocd.org/pages/getting-openocd.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "objcopy",
            description: "ELF -> raw binary converter - the step between `cargo build` and a DFU flash",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: None,
            severity: Severity::Feature,
            impact: "DFU flashing stops right after the build: firmware.bin can't be produced. \
                     Any ONE of llvm-objcopy, arm-none-eabi-objcopy or `cargo objcopy` is enough \
                     (`cargo objcopy` also needs `rustup component add llvm-tools`).",
            // Sentinel: the flash path tries three different binaries in order,
            // so a single-command check would report Missing whenever the user
            // happens to have one of the other two. See `OBJCOPY_CHECK`.
            check_cmd: OBJCOPY_CHECK,
            check_args: &[],
            check_pattern: "",
            min_version: None,
            // cargo-binutils provides fallback #3 and needs no root anywhere.
            install_cmd: Some("cargo"),
            install_args: &["install", "cargo-binutils"],
            manual_url: "https://github.com/rust-embedded/cargo-binutils",
            status: ToolStatus::Unknown,
        },
        // ── EspRust, RISC-V ──────────────────────────────────────────────
        // TWO targets, not one: the C2 and C3 cores have no atomics extension
        // and use `imc`; the C5, C6, C61 and H2 use `imac`. Each is gated on the
        // project's own triple, so a C6 user is told about the one their chip
        // needs and not about the one it does not.
        RequiredTool {
            name: "riscv32imc-unknown-none-elf",
            description: "Rust target for the ESP32-C2 / C3 (RISC-V, no atomics)",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: Some("riscv32imc"),
            severity: Severity::Blocking,
            impact: "This ESP32 chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "riscv32imc-unknown-none-elf",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "riscv32imc-unknown-none-elf"],
            manual_url: "https://esp-rs.github.io/book/installation/riscv.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "riscv32imac-unknown-none-elf",
            description: "Rust target for the ESP32-C5 / C6 / C61 / H2 (RISC-V with atomics)",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: Some("riscv32imac"),
            severity: Severity::Blocking,
            impact: "This ESP32 chip cannot be compiled at all until the target is installed.",
            check_cmd: "rustup",
            check_args: &["target", "list", "--installed"],
            check_pattern: "riscv32imac-unknown-none-elf",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["target", "add", "riscv32imac-unknown-none-elf"],
            manual_url: "https://esp-rs.github.io/book/installation/riscv.html",
            status: ToolStatus::Unknown,
        },
        // ── EspRust, Xtensa ──────────────────────────────────────────────
        // A different compiler, not a different target. `rustup target add`
        // cannot help here: stock rustc knows the triple but its LLVM has a
        // different data layout, so even nightly with `-Z build-std` fails
        // inside `core`. `espup` installs a fork that carries a patched LLVM,
        // as a toolchain named `esp`.
        RequiredTool {
            name: "espup",
            description: "Installer for Espressif's Rust fork — the only way to build Xtensa",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: Some("xtensa"),
            severity: Severity::Blocking,
            impact: "ESP32 / S2 / S3 cannot be compiled: Xtensa needs Espressif's rustc fork.",
            check_cmd: "espup",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: Some("cargo"),
            install_args: &["install", "espup"],
            manual_url: "https://docs.esp-rs.org/book/installation/riscv-and-xtensa.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "esp toolchain",
            description: "The `esp` Rust toolchain itself — installed by `espup install`",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: Some("xtensa"),
            severity: Severity::Blocking,
            impact: "Xtensa builds fail: `cargo +esp` has no toolchain to run.",
            check_cmd: "rustup",
            check_args: &["toolchain", "list"],
            check_pattern: "esp",
            min_version: None,
            // `espup install` writes the toolchain; there is no rustup channel
            // for it, so this is the one install that is not a rustup command.
            install_cmd: Some("espup"),
            install_args: &["install"],
            manual_url: "https://docs.esp-rs.org/book/installation/riscv-and-xtensa.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "rust-src",
            description: "Rust source component — required by build-std for ESP32-C3",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: None,
            severity: Severity::Blocking,
            impact: "ESP32-C3 builds fail: build-std needs the Rust source component.",
            check_cmd: "rustup",
            check_args: &["component", "list", "--installed"],
            check_pattern: "rust-src",
            min_version: None,
            install_cmd: Some("rustup"),
            install_args: &["component", "add", "rust-src"],
            manual_url: "https://esp-rs.github.io/book/installation/riscv.html",
            status: ToolStatus::Unknown,
        },
        RequiredTool {
            name: "espflash",
            description: "ESP32 USB flash tool — programs the chip over the built-in USB serial",
            toolchain: Some(ToolchainKind::EspRust),
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The ESP32 cannot be flashed from the Flash tab.",
            check_cmd: "espflash",
            check_args: &["--version"],
            check_pattern: "",
            min_version: None,
            install_cmd: Some("cargo"),
            install_args: &["install", "espflash"],
            manual_url: "https://github.com/esp-rs/espflash",
            status: ToolStatus::Unknown,
        },
    ];

    // ── Host C toolchain ─────────────────────────────────────────────────────
    // Not an embedded tool: Rust links every build-script / proc-macro for the
    // HOST, so without a working C toolchain NOTHING builds — on any platform.
    // Only the shape differs. Windows is probed BY FILE, not by binary presence:
    // a half-installed Visual Studio has `cl.exe` but no libs/headers, which is
    // exactly the failure that must be caught (see `MSVC_CHECK`). The unixes just
    // need `cc` to answer.
    tools.push(RequiredTool {
        name: per_os("MSVC build tools", "Xcode Command Line Tools", "C build tools"),
        description: per_os(
            "Microsoft C++ x64 toolchain (msvcrt.lib + headers) — required to link Rust build-scripts on Windows",
            "Apple clang + linker — required to link Rust build-scripts on macOS",
            "gcc + ld (build-essential) — required to link Rust build-scripts on Linux",
        ),
        toolchain: None,
        only_for_target: None,
        severity: Severity::Blocking,
        impact: per_os(
            "NOTHING builds: every build-script fails to link (LNK1104 msvcrt.lib / C1083 vcruntime.h).",
            "NOTHING builds: every build-script fails to link (no linker / missing SDK headers). \
             Run `xcode-select --install`.",
            "NOTHING builds: every build-script fails to link (`cc` not found). \
             Debian/Ubuntu: build-essential · Fedora: @development-tools · Arch: base-devel.",
        ),
        check_cmd: per_os(MSVC_CHECK, "cc", "cc"),
        check_args: per_os(&[][..], &["--version"][..], &["--version"][..]),
        check_pattern: "",
        min_version: None,
        // macOS: `xcode-select --install` opens a GUI installer and returns
        // immediately, so it is NOT an auto-install we can report on — manual.
        // Linux: needs root and the package name differs per distro — manual.
        install_cmd: per_os(Some("winget"), None, None),
        install_args: per_os(
            &[
                "install",
                "--id",
                "Microsoft.VisualStudio.2022.BuildTools",
                "--accept-package-agreements",
                "--accept-source-agreements",
                "--override",
                "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended",
            ][..],
            &[][..],
            &[][..],
        ),
        manual_url: per_os(
            "https://visualstudio.microsoft.com/visual-cpp-build-tools/",
            "https://developer.apple.com/xcode/resources/",
            "https://doc.rust-lang.org/book/ch01-01-installation.html#installing-rustup-on-linux-or-macos",
        ),
        status: ToolStatus::Unknown,
    });

    // ── Serial-tab Bridge (MITM) prerequisite ────────────────────────────────
    // A virtual serial pair, which is a completely different kind of thing per
    // platform: a spawnable CLI on unix, a kernel driver on Windows. Hence one
    // entry whose name, probe and installer are all per-OS.
    tools.push(RequiredTool {
        name: per_os("com0com", "socat", "socat"),
        description: per_os(
            "Virtual serial-port pair driver — required by the Serial tab's Bridge (MITM) mode",
            "Multipurpose relay — the Serial tab's Bridge (MITM) mode uses it to create a PTY pair",
            "Multipurpose relay — the Serial tab's Bridge (MITM) mode uses it to create a PTY pair",
        ),
        toolchain: None,
        only_for_target: None,
        severity: Severity::Optional,
        impact: per_os(
            "The Serial tab's Bridge (MITM) mode can't run: there is no way to make a virtual \
             port pair. Everything else in the Serial tab is unaffected.",
            "The Serial tab's Bridge (MITM) mode can't create its PTY pair. Everything else in \
             the Serial tab is unaffected.",
            "The Serial tab's Bridge (MITM) mode can't create its PTY pair. Everything else in \
             the Serial tab is unaffected.",
        ),
        // com0com is a driver with no CLI on PATH, so it is probed by registry
        // key rather than by running anything — see `COM0COM_CHECK`.
        check_cmd: per_os(COM0COM_CHECK, "socat", "socat"),
        check_args: per_os(&[][..], &["-V"][..], &["-V"][..]),
        check_pattern: "",
        min_version: None,
        install_cmd: per_os(None, Some("brew"), None),
        install_args: per_os(&[][..], &["install", "socat"][..], &[][..]),
        manual_url: per_os(
            "https://com0com.sourceforge.net/",
            "http://www.dest-unreach.org/socat/",
            "http://www.dest-unreach.org/socat/",
        ),
        status: ToolStatus::Unknown,
    });

    // Not a tool at all: a property of the ENVIRONMENT the IDE was started in,
    // which every cargo it launches inherits. It lives in this catalog because
    // this is the one place that already answers "why does the build fail for a
    // reason that has nothing to do with my code?" — and because the failure it
    // causes is otherwise undiagnosable (see `check_cargo_feature_env`).
    tools.push(RequiredTool {
        name: "CARGO_FEATURE_* env",
        description: "Cargo's private feature namespace must be clear in the environment — \
                      build scripts cannot tell a stray variable from a real feature",
        toolchain: None,
        only_for_target: None,
        // Blocking, i.e. it reaches the STARTUP BANNER, even though projects
        // other than RTIC build fine with the variable set. The severity here is
        // about how loud the warning has to be, not about how much breaks: the
        // failure it causes names a crate the user did not add, blames a
        // Cargo.toml that is correct, and gives no hint that the environment is
        // involved. Finding that unaided costs an afternoon.
        severity: Severity::Blocking,
        impact: per_os(
            "RTIC projects fail to build with \"More than one backend selected\", pointing at a \
             Cargo.toml that is correct. Other projects are unaffected. Remove the variable: \
             reg delete \"HKCU\\Environment\" /v <NAME> /f — then restart the IDE so it stops \
             inheriting it.",
            "RTIC projects fail to build with \"More than one backend selected\", pointing at a \
             Cargo.toml that is correct. Other projects are unaffected. Unset the variable in \
             your shell profile (~/.zprofile or ~/.zshrc), then restart the IDE so it stops \
             inheriting it.",
            "RTIC projects fail to build with \"More than one backend selected\", pointing at a \
             Cargo.toml that is correct. Other projects are unaffected. Unset the variable in \
             your shell profile (~/.profile or ~/.bashrc), then restart the IDE so it stops \
             inheriting it.",
        ),
        check_cmd: CARGO_FEATURE_ENV_CHECK,
        check_args: &[],
        check_pattern: "",
        min_version: None,
        // Nothing to install, and nothing this IDE should silently remove: a
        // persistent environment variable belongs to the user's machine, not to
        // a project. Naming it and saying what it breaks is the whole job.
        install_cmd: None,
        install_args: &[],
        manual_url: "https://doc.rust-lang.org/cargo/reference/environment-variables.html",
        status: ToolStatus::Unknown,
    });

    // ── Access to the hardware ───────────────────────────────────────────────
    // None of these is a program to install: they are PERMISSIONS on Linux and a
    // DRIVER REGISTRATION on Windows, and they are the number-one reason
    // flashing "doesn't work" on a machine that already has every tool present.
    // macOS asks for neither, so it gets no entry here at all.
    if cfg!(target_os = "linux") {
        tools.push(RequiredTool {
            name: UDEV_RULES_TOOL,
            description:
                "udev rules granting non-root access to debug probes (ST-Link, J-Link, CMSIS-DAP, DFU)",
            toolchain: Some(ToolchainKind::RustEmbedded),
            only_for_target: None,
            severity: Severity::Feature,
            impact:
                "Debug probes are visible but cannot be OPENED: probe-rs / OpenOCD / dfu-util fail \
                 with \"Permission denied\" or find no probe unless run with sudo. Install \
                 probe-rs' 69-probe-rs.rules (and 60-openocd.rules), then `sudo udevadm control \
                 --reload && sudo udevadm trigger`.",
            check_cmd: UDEV_CHECK,
            check_args: &[],
            check_pattern: "",
            min_version: None,
            install_cmd: None, // writing to /etc/udev/rules.d needs root
            install_args: &[],
            manual_url: "https://probe.rs/docs/getting-started/probe-setup/#linux%3A-udev-rules",
            status: ToolStatus::Unknown,
        });
        tools.push(RequiredTool {
            name: SERIAL_ACCESS_TOOL,
            description: "Permission to open /dev/ttyUSB* and /dev/ttyACM*",
            toolchain: None,
            only_for_target: None,
            severity: Severity::Feature,
            impact: "The Serial tab and espflash can't open the port (\"Permission denied\"). \
                 The owning group differs by distro — `dialout` on Debian/Ubuntu/Fedora, `uucp` \
                 on Arch and openSUSE — so use the Tools tab's \"Fix access…\", which reads the \
                 group off the device actually plugged in. The change takes effect at the next \
                 LOGIN, not immediately.",
            // Sentinel: not a program, and "am I in a group called dialout?" is
            // the wrong question anyway — see `SERIAL_ACCESS_CHECK`.
            check_cmd: SERIAL_ACCESS_CHECK,
            check_args: &[],
            check_pattern: "",
            min_version: None,
            install_cmd: None, // needs root, and takes effect only after re-login
            install_args: &[],
            manual_url: "https://wiki.archlinux.org/title/Users_and_groups",
            status: ToolStatus::Unknown,
        });
    }

    if cfg!(target_os = "windows") {
        tools.push(RequiredTool {
            name: PROBE_DRIVER_TOOL,
            description:
                "WinUSB driver + device-interface GUID for debug probes (what udev rules are on Linux)",
            // Not toolchain-gated: every chip family in this IDE is flashed and
            // debugged through a probe, and the ESP built-in JTAG is the one
            // that most often arrives unregistered.
            toolchain: None,
            only_for_target: None,
            severity: Severity::Feature,
            impact:
                "Debug probes are LISTED but cannot be OPENED: RTT, Debug, Profile-Runtime and \
                 `cargo flash` all fail with \"The selected USB device could not be opened\", \
                 while the Probe list keeps showing the probe (enumeration never opens anything). \
                 Zadig is a small free Windows tool that installs the generic WinUSB driver on ONE \
                 USB interface and registers the device-interface GUID that probe-rs opens the \
                 probe through — the binding Windows makes on its own can leave that GUID out. Run \
                 it once per probe: Options -> List All Devices, pick the probe's DEBUG interface \
                 (on an ESP built-in JTAG that is \"USB JTAG/serial debug unit (Interface 2)\" — \
                 NEVER Interface 0, which is the COM port that flashing and the monitor use), \
                 choose WinUSB, Replace Driver, then unplug and replug the board. It is undone \
                 from Device Manager -> Update driver.",
            // Sentinel: see `PROBE_DRIVER_CHECK` for why "is Zadig installed?"
            // is not the question.
            check_cmd: PROBE_DRIVER_CHECK,
            check_args: &[],
            check_pattern: "",
            min_version: None,
            // Never automatic. This replaces a device driver, needs elevation,
            // and picking the wrong interface takes the serial port away with
            // it — a button that could do that unattended has no business here.
            install_cmd: None,
            install_args: &[],
            manual_url: "https://zadig.akeo.ie",
            status: ToolStatus::Unknown,
        });
    }

    Arc::new(Mutex::new(ToolsState {
        log: Vec::new(),
        tools,
    }))
}

// ── Public API ─────────────────────────────────────────────────────────────────

/// Asynchronously check one tool; updates its status from a background thread.
pub fn start_check(idx: usize, state: Arc<Mutex<ToolsState>>, ctx: egui::Context) {
    {
        let mut s = state.lock().unwrap();
        if s.tools[idx].status.is_busy() {
            return;
        }
        s.tools[idx].status = ToolStatus::Checking;
    }
    ctx.request_repaint();
    thread::spawn(move || {
        // Extract check parameters without holding the lock
        let (cmd, args, pat, minv) = {
            let s = state.lock().unwrap();
            let t = &s.tools[idx];
            (t.check_cmd, t.check_args, t.check_pattern, t.min_version)
        };
        let result = run_check_blocking(cmd, args, pat, minv);
        {
            let mut s = state.lock().unwrap();
            let name = s.tools[idx].name; // &'static str — copy out before mut borrow
            s.push_log(format!("[check] {} -> {}", name, result.label()));
            s.tools[idx].status = result;
        }
        ctx.request_repaint();
    });
}

/// Check all tools sequentially in one background thread.
pub fn start_check_all(state: Arc<Mutex<ToolsState>>, ctx: egui::Context) {
    let count = state.lock().unwrap().tools.len();
    thread::spawn(move || {
        {
            state.lock().unwrap().push_log("> Checking all tools…");
        }
        ctx.request_repaint();

        for idx in 0..count {
            // Skip tools currently being operated on by another thread
            {
                let mut s = state.lock().unwrap();
                if s.tools[idx].status.is_busy() {
                    continue;
                }
                s.tools[idx].status = ToolStatus::Checking;
            }
            ctx.request_repaint();

            // Perform check without holding the lock
            let (cmd, args, pat, minv) = {
                let s = state.lock().unwrap();
                let t = &s.tools[idx];
                (t.check_cmd, t.check_args, t.check_pattern, t.min_version)
            };
            let result = run_check_blocking(cmd, args, pat, minv);

            {
                let mut s = state.lock().unwrap();
                let name = s.tools[idx].name; // &'static str — copy out before mut borrow
                s.push_log(format!("  {} -> {}", name, result.label()));
                s.tools[idx].status = result;
            }
            ctx.request_repaint();
        }

        {
            state.lock().unwrap().push_log("[OK] Check complete");
        }
        ctx.request_repaint();
    });
}

/// Asynchronously install one tool (then re-checks it).
pub fn start_install(idx: usize, state: Arc<Mutex<ToolsState>>, ctx: egui::Context) {
    {
        let mut s = state.lock().unwrap();
        if s.tools[idx].status.is_busy() {
            return;
        }
        if s.tools[idx].install_cmd.is_none() {
            return; // manual-only — caller should show the URL instead
        }
        s.tools[idx].status = ToolStatus::Installing;
    }
    ctx.request_repaint();
    thread::spawn(move || {
        do_install_blocking(idx, &state, &ctx);
    });
}

/// Install all tools that are Missing or Failed (with auto-installers), sequentially.
pub fn start_install_missing(state: Arc<Mutex<ToolsState>>, ctx: egui::Context) {
    let count = state.lock().unwrap().tools.len();
    thread::spawn(move || {
        {
            state
                .lock()
                .unwrap()
                .push_log("> Installing missing tools…");
        }
        ctx.request_repaint();

        let mut installed_any = false;
        for idx in 0..count {
            let should_install = {
                let s = state.lock().unwrap();
                let t = &s.tools[idx];
                matches!(t.status, ToolStatus::Missing | ToolStatus::Failed(_))
                    && t.install_cmd.is_some()
                    && !t.status.is_busy()
            };
            if should_install {
                installed_any = true;
                {
                    state.lock().unwrap().tools[idx].status = ToolStatus::Installing;
                }
                ctx.request_repaint();
                do_install_blocking(idx, &state, &ctx);
            }
        }

        if !installed_any {
            state.lock().unwrap().push_log("  (nothing to install)");
        }
        {
            state.lock().unwrap().push_log("[OK] Install pass complete");
        }
        ctx.request_repaint();
    });
}

// ── Internal helpers ───────────────────────────────────────────────────────────

/// Run the check command synchronously and return the resulting `ToolStatus`.
/// Does **not** hold any mutex while running the external command.
/// Sentinel [`RequiredTool::check_cmd`] for the MSVC toolchain: it is NOT a CLI
/// on PATH, and "the binary exists" is exactly the wrong test (a half-installed
/// Visual Studio has `cl.exe` but no libs/headers), so it gets a file-based probe
/// instead of a spawned command. See [`crate::msvc`].
pub const MSVC_CHECK: &str = "@msvc-toolchain";

/// Sentinel for the ELF→bin converter: the DFU flash path tries `llvm-objcopy`,
/// then `arm-none-eabi-objcopy`, then `cargo objcopy` ([`crate::dfu`]), so the
/// catalog must answer the same question — "is ANY of the three here?" — instead
/// of picking one and calling the other two setups broken.
pub const OBJCOPY_CHECK: &str = "@objcopy-any";

/// Sentinel for the Linux udev rules that grant non-root access to debug probes.
/// Not a program, so there is nothing to run: it is answered by looking for rules
/// files on disk.
pub const UDEV_CHECK: &str = "@udev-rules";

/// Sentinel for the WinUSB registration of debug probes (Windows).
///
/// Nothing to spawn, and - unlike every other entry - nothing to INSTALL either:
/// Zadig is a single portable .exe, usually run once out of a Downloads folder
/// and then deleted. "Is Zadig present?" is therefore both unanswerable and the
/// wrong question; whether the probes attached right now can be opened is the
/// one that matters. See [`check_usb_probe_driver`].
pub const PROBE_DRIVER_CHECK: &str = "@usb-probe-driver";

/// Sentinel for the com0com virtual-pair driver (Windows). It installs no CLI on
/// PATH, so "run it and see" is impossible; the driver's service key is the
/// evidence, the same thing its own docs tell you to look for.
pub const COM0COM_CHECK: &str = "@com0com";

/// Sentinel for the `CARGO_FEATURE_*` environment check. Nothing to spawn: the
/// evidence is the environment this process was started with, which is the same
/// one every `cargo` we launch will pass on to its build scripts.
pub const CARGO_FEATURE_ENV_CHECK: &str = "@cargo-feature-env";

/// Stray `CARGO_FEATURE_*` variables in the inherited environment.
///
/// `CARGO_FEATURE_<NAME>` is cargo's own namespace: it sets one per enabled
/// feature when it runs a build script. A build script cannot tell cargo's from
/// an inherited one — it just reads `std::env::vars()` — so anything left in the
/// user's environment is silently counted as an extra enabled feature.
///
/// `rtic-macros` is the case that bites, because it counts rather than matches:
/// one stray variable makes it report "More than one backend selected" and fail
/// the build, pointing at a `Cargo.toml` that is perfectly correct. That is a
/// diagnosis nobody makes unaided, which is the whole reason this check exists.
fn check_cargo_feature_env() -> ToolStatus {
    let mut leaked: Vec<String> = std::env::vars()
        .map(|(k, _)| k)
        .filter(|k| k.starts_with("CARGO_FEATURE_"))
        .collect();
    if leaked.is_empty() {
        return ToolStatus::Ok("no CARGO_FEATURE_* set".to_string());
    }
    leaked.sort();
    // `Failed`, not `Missing`: the check ran and found something wrong. Missing
    // would read backwards here — the problem is a variable being PRESENT.
    //
    // Short on purpose: the consequence and the fix live in the entry's
    // `impact`, which the banner and the Tools tab both already print. What
    // only the CHECK can know is WHICH variable — and that is the one thing
    // `impact` cannot say, being a `&'static str` with a `<NAME>` placeholder.
    // So this message carries exactly that, and the banner prints it underneath.
    ToolStatus::Failed(format!("set in this environment: {}", leaked.join(", ")))
}

#[cfg(windows)]
fn check_com0com() -> ToolStatus {
    // `reg query` rather than a registry crate: one spawn, no dependency, and
    // the exit code alone answers "is the driver there?".
    let installed = crate::build::no_window(&mut Command::new("reg"))
        .args(["query", r"HKLM\SYSTEM\CurrentControlSet\Services\com0com"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !installed {
        return ToolStatus::Missing;
    }
    // "Driver installed" is NOT the same as "Bridge can work", and reporting Ok
    // for it was a false pass found on this very machine: the service key is
    // present with a pair configured as `COM#` (auto-assign) and NO virtual port
    // is actually enumerated. Same lesson as the half-installed Visual Studio in
    // `check_msvc_toolchain` — probe the capability, not the installation.
    let live: Vec<String> = serialport::available_ports()
        .map(|ps| ps.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    let pairs = crate::serial_bridge::com0com_pairs(&live);
    if pairs.is_empty() {
        return ToolStatus::Failed(
            "com0com is installed but no pair has two live ports — create one in its setup \
             (a pair left on the `COM#` placeholder doesn't count until Windows assigns \
             numbers). Bridge mode stays unavailable until then."
                .to_string(),
        );
    }
    ToolStatus::Ok(
        pairs
            .iter()
            .map(|(a, b)| format!("{a} <-> {b}"))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

#[cfg(not(windows))]
fn check_com0com() -> ToolStatus {
    ToolStatus::Ok("n/a (not Windows)".to_string())
}

/// Can the debug probes attached RIGHT NOW actually be opened?
///
/// The Windows counterpart of [`check_udev_rules`], and the same lesson as
/// `check_com0com`: probe the capability, not the installation. probe-rs opens a
/// probe through a device-interface GUID, and a WinUSB binding Windows made by
/// itself can carry no such GUID - the probe then LISTS but never opens, which
/// reads as "no probe found" or "device could not be opened" everywhere it is
/// used (see [`crate::probe::missing_device_interface_guid`]). Zadig is the fix,
/// exactly as probe-rs' own `69-probe-rs.rules` is the fix on Linux; neither
/// entry is named after a program you keep installed.
fn check_usb_probe_driver() -> ToolStatus {
    // No answer from probe-rs (not installed, crashed) means we know nothing,
    // and `Unknown` is the one status that reports no problem - the honest
    // result. The missing probe-rs itself has its own catalog entry.
    let Ok(probes) = crate::probe::list_probes() else {
        return ToolStatus::Unknown;
    };
    if probes.is_empty() {
        return ToolStatus::Ok("n/a (no debug probe attached)".to_string());
    }
    // A serial interface taken over by a WinUSB install comes FIRST: it is the
    // more damaging of the two mistakes (the board loses its COM port, so
    // flashing and the monitor stop) and the one the user did not intend.
    let hijacked: Vec<crate::win_driver::HijackedPort> = probes
        .iter()
        .flat_map(|p| crate::win_driver::hijacked_ports(&p.selector))
        .collect();
    if let Some(h) = hijacked.first() {
        return ToolStatus::Failed(format!(
            "{} — Zadig was pointed at the serial interface instead of the debug one. \
             Flashing and the monitor have nothing to open. Use \"Restore COM port\" on this \
             row: Windows still has the name on file, so the port comes back as {}.",
            h.summary(),
            h.port_name
        ));
    }
    let stuck: Vec<String> = probes
        .iter()
        .filter(|p| crate::probe::missing_device_interface_guid(Some(&p.selector)))
        .map(|p| format!("{} ({})", p.name, p.selector))
        .collect();
    if stuck.is_empty() {
        return ToolStatus::Ok(format!(
            "{} probe(s) openable: {}",
            probes.len(),
            probes
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // Named, because on a bench with two probes only one of them is usually
    // broken and the other keeps working - which is exactly what makes this
    // failure look random.
    ToolStatus::Failed(format!(
        "no device-interface GUID for {}: probe-rs can LIST it but not open it. Reinstall its \
         driver with Zadig, picking that probe's DEBUG interface - on an ESP built-in JTAG that \
         is \"USB JTAG/serial debug unit (Interface 2)\", never Interface 0, which is the COM \
         port used for flashing and the monitor.",
        stuck.join(", ")
    ))
}

/// Sentinel for "can this user open a serial port?".
///
/// The obvious check — `id -nG | grep dialout` — answers the WRONG question in
/// two directions: Arch and openSUSE call the group `uucp`, and on a systemd
/// machine the logged-in user gets an ACL on the device through `uaccess` with no
/// group membership at all. So when a port is actually present the check asks the
/// kernel directly (`access(R_OK|W_OK)`, which does NOT open the device — opening
/// a tty asserts DTR and would reset the attached board), and only falls back to
/// group membership when there is nothing plugged in to test.
pub const SERIAL_ACCESS_CHECK: &str = "@serial-access";

/// Catalog name of that entry — the Tools tab matches on it to offer "Fix
/// access…", same arrangement as [`UDEV_RULES_TOOL`].
pub const SERIAL_ACCESS_TOOL: &str = "serial port access";

/// Groups that conventionally own serial devices. Only consulted when no device
/// is plugged in; with one present its REAL group is read off the node.
const SERIAL_GROUP_CANDIDATES: [&str; 3] = ["dialout", "uucp", "plugdev"];

/// Exact-token membership test over `id -nG` output (space-separated names).
/// A substring test would pass on a group merely CONTAINING the name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn groups_contain(id_output: &str, group: &str) -> bool {
    id_output.split_whitespace().any(|g| g == group)
}

/// Serial device nodes present right now, sorted. `/dev/ttyUSB*` for USB-serial
/// bridges (CH340, CP210x, FTDI), `/dev/ttyACM*` for CDC devices (ESP32-S3/C3
/// native USB, many dev boards).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn serial_device_nodes() -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev") {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("ttyUSB") || name.starts_with("ttyACM") {
                out.push(e.path());
            }
        }
    }
    out.sort();
    out
}

/// The group that owns `path`, by name. `None` when the gid has no entry.
#[cfg(target_os = "linux")]
fn owning_group(path: &std::path::Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let gid = std::fs::metadata(path).ok()?.gid();
    // SAFETY: getgrgid returns a pointer into a static buffer, valid until the
    // next call; the name is copied out immediately, before anything else can
    // call it. Null = no such group.
    unsafe {
        let grp = libc::getgrgid(gid);
        if grp.is_null() {
            return None;
        }
        let name = (*grp).gr_name;
        if name.is_null() {
            return None;
        }
        Some(
            std::ffi::CStr::from_ptr(name)
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// Can we read AND write `path` right now? Answers the real question, and unlike
/// opening the device it has no side effects on the attached board.
#[cfg(target_os = "linux")]
fn can_use_device(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c` is a valid NUL-terminated string for the duration of the call.
    unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

/// The group the user should be added to, and the command that does it — used by
/// the Tools tab's "Fix access…". Reads the group off a plugged-in device when
/// there is one, so the suggestion is right on Arch (`uucp`) as well as Debian.
#[cfg(target_os = "linux")]
pub fn serial_access_fix() -> (String, String) {
    let group = serial_device_nodes()
        .iter()
        .find_map(|d| owning_group(d))
        .unwrap_or_else(|| SERIAL_GROUP_CANDIDATES[0].to_string());
    let cmd = format!("sudo usermod -aG {group} $USER");
    (group, cmd)
}

#[cfg(not(target_os = "linux"))]
pub fn serial_access_fix() -> (String, String) {
    let group = SERIAL_GROUP_CANDIDATES[0].to_string();
    let cmd = format!("sudo usermod -aG {group} $USER");
    (group, cmd)
}

#[cfg(target_os = "linux")]
fn check_serial_access() -> ToolStatus {
    let devices = serial_device_nodes();

    // A device is plugged in: ask the kernel, which accounts for uaccess ACLs,
    // group membership and plain permissions all at once.
    if let Some(usable) = devices.iter().find(|d| can_use_device(d)) {
        return ToolStatus::Ok(format!("can open {}", usable.display()));
    }
    if let Some(blocked) = devices.first() {
        let group = owning_group(blocked).unwrap_or_else(|| "?".to_string());
        return ToolStatus::Failed(format!(
            "{} is present but not writable by you (owned by group `{group}`). \
             Add yourself with `sudo usermod -aG {group} $USER`, then LOG OUT and back in.",
            blocked.display()
        ));
    }

    // Nothing plugged in — the honest answer is "can't tell", so fall back to
    // the conventional groups and say which one matched.
    let Ok(out) = Command::new("id").arg("-nG").output() else {
        return ToolStatus::Unknown;
    };
    let mine = String::from_utf8_lossy(&out.stdout);
    match SERIAL_GROUP_CANDIDATES
        .iter()
        .find(|g| groups_contain(&mine, g))
    {
        Some(g) => ToolStatus::Ok(format!("in group `{g}` (no port plugged in to verify)")),
        None => ToolStatus::Missing,
    }
}

#[cfg(not(target_os = "linux"))]
fn check_serial_access() -> ToolStatus {
    ToolStatus::Ok("n/a (not Linux)".to_string())
}

/// The three ELF→bin converters, in the order [`crate::dfu::objcopy`] tries them.
/// Kept next to the sentinel so the two lists can be compared at a glance.
const OBJCOPY_CANDIDATES: [(&str, &[&str]); 3] = [
    ("llvm-objcopy", &["--version"]),
    ("arm-none-eabi-objcopy", &["--version"]),
    ("cargo", &["objcopy", "--version"]),
];

/// `Ok` naming the first converter found, `Missing` when none of the three is.
fn check_objcopy_any() -> ToolStatus {
    for (cmd, args) in OBJCOPY_CANDIDATES {
        let mut c = Command::new(cmd);
        c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        crate::build::no_window_raw(&mut c);
        if matches!(c.output(), Ok(out) if out.status.success()) {
            return ToolStatus::Ok(format!("{cmd} ({args:?} ok)").replace('"', ""));
        }
    }
    ToolStatus::Missing
}

/// Look for udev rules that mention a debug-probe tool. Both the system
/// directories and the admin one are searched, because packages install into
/// `/usr/lib` (or `/lib`) while a hand-installed rule lands in `/etc`.
///
/// Deliberately a NAME match, not a parse: rule files are matched by vendor/
/// product id in a syntax we have no business interpreting, and "a file called
/// 69-probe-rs.rules exists" is the same thing every setup guide tells the user
/// to check.
/// Where udev rules live. Both the system directories and the admin one, because
/// packages install into `/usr/lib` (or `/lib`) while a hand-installed rule lands
/// in `/etc`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const UDEV_DIRS: [&str; 3] = [
    "/etc/udev/rules.d",
    "/usr/lib/udev/rules.d",
    "/lib/udev/rules.d",
];

/// Does this rules-file name look like a debug-probe rule? Pure, so the matching
/// is testable on any host — only the directory walk around it is Linux-only.
/// (Compiled everywhere for exactly that reason; only Linux CALLS it.)
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn udev_rule_matches(file_name: &str) -> bool {
    // The last two are this app's own file, before and after the rename: it
    // was never recognised here, so the check reported it missing.
    const MARKERS: [&str; 6] = [
        "probe-rs",
        "openocd",
        "stlink",
        "dfu",
        "rust_on_chip",
        "embedded-ide",
    ];
    let lower = file_name.to_ascii_lowercase();
    lower.ends_with(".rules") && MARKERS.iter().any(|m| lower.contains(m))
}

/// Probe-related rules files present in `dirs`, sorted and deduplicated.
///
/// Compiled on EVERY platform — only the call is Linux-only. A Linux-only body
/// is a body nobody here can compile, let alone test; keeping the whole walk
/// portable means a mistake in it fails the build on this machine too.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn scan_udev_dirs(dirs: &[&str]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // an absent directory is normal, not an error
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if udev_rule_matches(&name) {
                found.push(name);
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

#[cfg(target_os = "linux")]
fn check_udev_rules() -> ToolStatus {
    let found = scan_udev_dirs(&UDEV_DIRS);
    if found.is_empty() {
        ToolStatus::Missing
    } else {
        ToolStatus::Ok(found.join(", "))
    }
}

#[cfg(not(target_os = "linux"))]
fn check_udev_rules() -> ToolStatus {
    ToolStatus::Ok("n/a (not Linux)".to_string())
}

/// First dotted number in `text`, e.g. `"rustc 1.89.0 (abc 2026-01-01)"` →
/// `"1.89.0"`. Tools print their version in wildly different shapes, so we scan
/// rather than assume a position. `None` when there is no number at all.
pub fn parse_version(text: &str) -> Option<String> {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            let mut seen_dot = false;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == '.') {
                // A trailing dot ("1.2." / end of sentence) isn't part of it.
                if bytes[i] == '.' {
                    if i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_digit() {
                        break;
                    }
                    seen_dot = true;
                }
                i += 1;
            }
            if seen_dot {
                return Some(bytes[start..i].iter().collect());
            }
        } else {
            i += 1;
        }
    }
    None
}

/// `found < min`, comparing dotted components NUMERICALLY (so 1.10 > 1.9, which
/// a string compare gets wrong). Missing components count as 0 — `"1.74"` and
/// `"1.74.0"` are equal. Unparsable input answers `false`: never cry "outdated"
/// over something we failed to read.
pub fn version_lt(found: &str, min: &str) -> bool {
    // Strict: EVERY component must be a number. A string we can't read (empty,
    // "abc", "1.x") yields None → answer `false`, so we never accuse a tool of
    // being outdated on the strength of output we didn't understand.
    let part = |s: &str| -> Option<Vec<u64>> {
        let v: Vec<&str> = s.trim().split('.').collect();
        if v.iter().any(|c| c.trim().is_empty()) {
            return None;
        }
        v.iter().map(|c| c.trim().parse::<u64>().ok()).collect()
    };
    let (Some(f), Some(m)) = (part(found), part(min)) else {
        return false;
    };
    for i in 0..f.len().max(m.len()) {
        let a = f.get(i).copied().unwrap_or(0);
        let b = m.get(i).copied().unwrap_or(0);
        if a != b {
            return a < b;
        }
    }
    false
}

/// File-based probe of the MSVC host toolchain: `Ok` when some install has BOTH
/// `lib\x64\msvcrt.lib` and `include\vcruntime.h`; `Failed` (with the reason)
/// when installs exist but are all incomplete — the case that silently breaks
/// every build; `Missing` when there is none at all.
#[cfg(windows)]
fn check_msvc_toolchain() -> ToolStatus {
    let installs = crate::msvc::installs();
    if let Some(ok) = installs.iter().find(|i| i.is_complete()) {
        // Name the broken ones too: they are why builds can still fail if the
        // env injection is ever bypassed.
        let broken = installs.iter().filter(|i| !i.is_complete()).count();
        return ToolStatus::Ok(if broken > 0 {
            format!("{} (+{broken} incomplete)", ok.label())
        } else {
            ok.label()
        });
    }
    if installs.is_empty() {
        return ToolStatus::Missing;
    }
    let detail: Vec<String> = installs
        .iter()
        .map(|i| {
            let mut miss = Vec::new();
            if !i.has_libs {
                miss.push("libs");
            }
            if !i.has_headers {
                miss.push("headers");
            }
            format!("{} missing {}", i.label(), miss.join("+"))
        })
        .collect();
    ToolStatus::Failed(format!(
        "Visual Studio found but its C++ x64 toolchain is incomplete ({}). \
         Install the \"Desktop development with C++\" workload / Build Tools.",
        detail.join("; ")
    ))
}

#[cfg(not(windows))]
fn check_msvc_toolchain() -> ToolStatus {
    ToolStatus::Ok("n/a (not Windows)".to_string())
}

/// Whether a tool gated on `gate` applies to a project built for `target`.
///
/// A prefix match that must end at a `-` (or at the end): `riscv32imc` gates
/// `riscv32imc-unknown-none-elf` and `xtensa` gates every Xtensa triple, but
/// `thumbv7em-none-eabi` does NOT gate `thumbv7em-none-eabihf`.
fn target_gate_matches(gate: &str, target: &str) -> bool {
    target
        .strip_prefix(gate)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('-'))
}

/// Whether `pattern` is present in a check's output. A rustup target is one
/// WHOLE line of `rustup target list --installed`: by substring,
/// `thumbv7em-none-eabi` would be found inside `thumbv7em-none-eabihf`.
fn output_has(args: &[&str], output: &str, pattern: &str) -> bool {
    if args == ["target", "list", "--installed"] {
        output.lines().any(|l| l.trim() == pattern)
    } else {
        output.contains(pattern)
    }
}

fn run_check_blocking(
    cmd: &str,
    args: &[&str],
    pattern: &str,
    min_version: Option<&'static str>,
) -> ToolStatus {
    // Sentinels: not programs on PATH, so they never reach the spawn below.
    if cmd == MSVC_CHECK {
        return check_msvc_toolchain();
    }
    if cmd == OBJCOPY_CHECK {
        return check_objcopy_any();
    }
    if cmd == UDEV_CHECK {
        return check_udev_rules();
    }
    if cmd == SERIAL_ACCESS_CHECK {
        return check_serial_access();
    }
    if cmd == COM0COM_CHECK {
        return check_com0com();
    }
    if cmd == PROBE_DRIVER_CHECK {
        return check_usb_probe_driver();
    }
    if cmd == CARGO_FEATURE_ENV_CHECK {
        return check_cargo_feature_env();
    }
    let mut c = Command::new(cmd);
    c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    match c.output() {
        Err(_) => ToolStatus::Missing,
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = format!("{stdout}{stderr}");

            if !out.status.success() {
                return ToolStatus::Missing;
            }
            if !pattern.is_empty() && !output_has(args, &combined, pattern) {
                return ToolStatus::Missing;
            }

            // Version string:
            // • Pattern-based checks (e.g. `rustup target list --installed`) →
            //   show "installed" since the first stdout line is a random target name.
            // • Direct version checks (e.g. `rustc --version`) →
            //   show first non-empty line of stdout.
            let version = if !pattern.is_empty() {
                "installed".to_string()
            } else {
                stdout
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim()
                    .to_string()
            };

            // Present — but is it new enough? Only when a real minimum is
            // declared AND a version could actually be read.
            if let (Some(min), Some(found)) = (min_version, parse_version(&version)) {
                if version_lt(&found, min) {
                    return ToolStatus::Outdated { found, min };
                }
            }
            ToolStatus::Ok(version)
        }
    }
}

/// Install `tools[idx]` synchronously, stream output to the log, then re-check.
/// Caller must have already set the tool's status to `Installing` and released
/// the lock before calling this function.
fn do_install_blocking(idx: usize, state: &Arc<Mutex<ToolsState>>, ctx: &egui::Context) {
    // Extract all needed data while holding the lock briefly
    let (cmd, args_owned, name) = {
        let s = state.lock().unwrap();
        let t = &s.tools[idx];
        let args: Vec<String> = t.install_args.iter().map(|a| a.to_string()).collect();
        (t.install_cmd.unwrap_or(""), args, t.name.to_string())
    };

    {
        state
            .lock()
            .unwrap()
            .push_log(format!("> Installing {name}…"));
    }
    ctx.request_repaint();

    let mut c = Command::new(cmd);
    c.args(&args_owned)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }

    let result = match c.output() {
        Err(e) => {
            let msg = format!("Cannot run `{cmd}`: {e}");
            state.lock().unwrap().push_log(format!("  [X] {msg}"));
            ctx.request_repaint();
            ToolStatus::Failed(msg)
        }
        Ok(out) => {
            // Append combined stdout + stderr to the log
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            {
                let mut s = state.lock().unwrap();
                for line in combined.lines() {
                    if !line.trim().is_empty() {
                        s.log.push(format!("  {line}"));
                    }
                }
            }
            ctx.request_repaint();

            if !out.status.success() {
                let msg = format!("{cmd} exited with {}", out.status);
                state.lock().unwrap().push_log(format!("  [X] {msg}"));
                ctx.request_repaint();
                ToolStatus::Failed(msg)
            } else {
                state
                    .lock()
                    .unwrap()
                    .push_log(format!("  [OK] {name} installed OK"));
                ctx.request_repaint();

                // Re-check to confirm installation and capture the version string
                let (check_cmd, check_args, check_pattern, min_version) = {
                    let s = state.lock().unwrap();
                    let t = &s.tools[idx];
                    (t.check_cmd, t.check_args, t.check_pattern, t.min_version)
                };
                run_check_blocking(check_cmd, check_args, check_pattern, min_version)
            }
        }
    };

    state.lock().unwrap().tools[idx].status = result;
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bundled chip is offered exactly the target-gated tools it needs.
    ///
    /// `only_for_target` is a prefix match up to a `-`, and the two RISC-V targets differ by
    /// one letter in the middle - `riscv32imc` against `riscv32imac`. If either
    /// were a prefix of the other, a C3 would be told to install the C6's target
    /// (or worse, silently not told to install its own). They are not, and this
    /// is where that stays true.
    ///
    /// The Xtensa parts are the other half: espup and the `esp` toolchain are
    /// Blocking, so getting their gate wrong either blocks an STM32 user on a
    /// tool they will never use, or lets an ESP32-S3 build fail with
    /// `'esp32s3' is not a recognized processor` and no tool listed as missing.
    #[test]
    fn each_chip_is_offered_the_target_tools_it_needs_and_no_others() {
        use crate::panels::mcu_module::builtins::builtin_definitions;

        let state = make_tools_state();
        let mut s = state.lock().unwrap();
        // Pretend everything is absent, so the filter - not the probe - decides.
        for t in &mut s.tools {
            t.status = ToolStatus::Missing;
        }

        for d in builtin_definitions() {
            let target = d.project.target.as_str();
            let names: Vec<&str> = s
                .problems_for(Some(&d.toolchain), Some(target))
                .into_iter()
                .map(|(n, _, _)| n)
                .collect();

            let xtensa = target.starts_with("xtensa");
            for t in ["espup", "esp toolchain"] {
                assert_eq!(
                    names.contains(&t),
                    xtensa,
                    "{} ({target}): `{t}` offered={}, wanted={xtensa}",
                    d.id,
                    names.contains(&t)
                );
            }

            // Each rustup target entry appears for its OWN chips only. The
            // assertion is written from the chip's target, so it cannot drift
            // with the catalogue.
            for t in RUSTUP_TARGETS {
                assert_eq!(
                    names.contains(&t),
                    target == t,
                    "{} ({target}): `{t}` offered={}",
                    d.id,
                    names.contains(&t)
                );
            }
        }
    }

    /// Every rustup target the catalog knows, ARM and RISC-V alike.
    const RUSTUP_TARGETS: [&str; 7] = [
        "thumbv6m-none-eabi",
        "thumbv7m-none-eabi",
        "thumbv7em-none-eabi",
        "thumbv7em-none-eabihf",
        "thumbv8m.main-none-eabihf",
        "riscv32imc-unknown-none-elf",
        "riscv32imac-unknown-none-elf",
    ];

    /// The soft-float M4 target is a string prefix of the hard-float one, so
    /// both the gate and the installed check must stop at a `-` / a line.
    #[test]
    fn the_soft_float_target_is_not_the_hard_float_one() {
        assert!(target_gate_matches("thumbv7em-none-eabi", "thumbv7em-none-eabi"));
        assert!(!target_gate_matches("thumbv7em-none-eabi", "thumbv7em-none-eabihf"));
        assert!(target_gate_matches("riscv32imc", "riscv32imc-unknown-none-elf"));
        assert!(!target_gate_matches("riscv32imc", "riscv32imac-unknown-none-elf"));
        assert!(target_gate_matches("xtensa", "xtensa-esp32s3-none-elf"));

        let args = ["target", "list", "--installed"];
        let only_hf = "thumbv6m-none-eabi\nthumbv7em-none-eabihf\n";
        assert!(!output_has(&args, only_hf, "thumbv7em-none-eabi"));
        assert!(output_has(&args, only_hf, "thumbv7em-none-eabihf"));
        assert!(output_has(&args, "thumbv7em-none-eabi\r\n", "thumbv7em-none-eabi"));
        // Other checks keep the substring rule: `esp (default)` is the esp toolchain.
        assert!(output_has(&["toolchain", "list"], "esp (default)\n", "esp"));
    }

    /// The bundled chips cover only three of the four ARM targets - no built-in
    /// uses `thumbv7em-none-eabihf`, yet it is what every imported STM32F4 / G4 /
    /// L4 / H7 builds for. So the sweep above cannot see the gate on that entry;
    /// this does, for every ARM triple the IDE hands out.
    #[test]
    fn each_arm_target_is_offered_to_its_own_projects_only() {
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;

        let state = make_tools_state();
        let mut s = state.lock().unwrap();
        for t in &mut s.tools {
            t.status = ToolStatus::Missing;
        }

        for target in &RUSTUP_TARGETS[..5] {
            let names: Vec<&str> = s
                .problems_for(Some(&ToolchainKind::RustEmbedded), Some(target))
                .into_iter()
                .map(|(n, _, _)| n)
                .collect();
            for t in RUSTUP_TARGETS {
                assert_eq!(
                    names.contains(&t),
                    *target == t,
                    "{target}: `{t}` offered={}",
                    names.contains(&t)
                );
            }
        }

        // The gate reaches the startup banner too, not just the list. This is
        // the Pico regression in one line: only the M3 target is missing, and
        // a Cortex-M0+ project must not be blocked by it.
        for t in &mut s.tools {
            t.status = if t.name == "thumbv7m-none-eabi" {
                ToolStatus::Missing
            } else {
                ToolStatus::Ok(String::new())
            };
        }
        let arm = Some(&ToolchainKind::RustEmbedded);
        assert!(
            s.any_blocking_missing_for(arm, Some("thumbv7m-none-eabi")),
            "an F1 project with its own target missing must block"
        );
        assert!(
            !s.any_blocking_missing_for(arm, Some("thumbv6m-none-eabi")),
            "a Pico project was blocked by the M3 target it never uses"
        );
    }

    /// With no project open, a toolchain-specific tool must not fire the
    /// BLOCKING banner - "espup is missing" on a machine that only builds STM32
    /// is a false alarm, and the banner is the one thing that must not cry wolf.
    #[test]
    fn no_project_open_does_not_block_on_a_toolchain_specific_tool() {
        let state = make_tools_state();
        let mut s = state.lock().unwrap();
        for t in &mut s.tools {
            t.status = ToolStatus::Missing;
        }
        // The Tools LIST still shows them (you may be browsing the catalogue)…
        let listed: Vec<&str> = s
            .problems_for(None, None)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert!(
            !listed.contains(&"espup"),
            "toolchain-specific, no chip open"
        );

        // …and an STM32 project is not blocked by an Espressif tool either.
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        let arm = s.problems_for(
            Some(&ToolchainKind::RustEmbedded),
            Some("thumbv7em-none-eabihf"),
        );
        let arm: Vec<&str> = arm.into_iter().map(|(n, _, _)| n).collect();
        for t in ["espup", "esp toolchain", "riscv32imc-unknown-none-elf"] {
            assert!(!arm.contains(&t), "an ARM project was offered `{t}`");
        }
    }

    /// The failure mode this per-platform table invites: picking the command
    /// with `per_os` but forgetting to switch the ARGS with it, leaving a bare
    /// `winget` / `brew` that installs nothing and reports success.
    #[test]
    fn an_auto_installer_always_has_arguments() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        for t in &s.tools {
            if t.install_cmd.is_some() {
                assert!(
                    !t.install_args.is_empty(),
                    "{} has an install command but no arguments",
                    t.name
                );
            }
        }
    }

    /// The other half: an entry the host can't auto-install MUST tell the user
    /// where to get it, or the Tools tab has nothing to offer but "Missing".
    #[test]
    fn a_manual_tool_always_has_a_url() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        for t in &s.tools {
            if t.install_cmd.is_none() {
                assert!(
                    t.manual_url.starts_with("http"),
                    "{} can't be auto-installed and has no manual URL",
                    t.name
                );
            }
        }
    }

    /// The host C toolchain is Blocking on every platform — it is the entry that
    /// explains "nothing builds", and losing it on a platform is exactly the gap
    /// this table was made to close.
    #[test]
    fn the_host_toolchain_entry_exists_everywhere() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        let host = s
            .tools
            .iter()
            .find(|t| t.check_cmd == MSVC_CHECK || (t.check_cmd == "cc" && t.toolchain.is_none()))
            .expect("no host C toolchain entry for this platform");
        assert_eq!(host.severity, Severity::Blocking);
    }

    /// The directory walk itself, exercised on THIS host: a missing directory is
    /// skipped rather than treated as an error, and only rule files count.
    #[test]
    fn udev_scan_reads_real_directories() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["69-probe-rs.rules", "60-openocd.rules", "README.md"] {
            std::fs::write(dir.path().join(f), "").unwrap();
        }
        let p = dir.path().to_string_lossy().to_string();
        let found = scan_udev_dirs(&[&p, "/definitely/not/here"]);
        assert_eq!(found, vec!["60-openocd.rules", "69-probe-rs.rules"]);
        assert!(scan_udev_dirs(&["/definitely/not/here"]).is_empty());
    }

    /// `id -nG` prints space-separated names. A SUBSTRING test — the obvious
    /// implementation, and what this entry used to do — passes on any group that
    /// merely contains the word, and on Arch it fails a working setup outright
    /// because the group there is `uucp`.
    #[test]
    fn group_membership_matches_whole_names_only() {
        let out = "istrati wheel uucp video\n";
        assert!(groups_contain(out, "uucp"));
        assert!(groups_contain(out, "wheel"));
        assert!(!groups_contain(out, "dialout"));
        // The substring trap: `dialout-admin` must not read as `dialout`.
        assert!(!groups_contain("me dialout-admin\n", "dialout"));
        assert!(!groups_contain("", "dialout"));
    }

    /// The fix must be a runnable command naming a real group, on any host —
    /// with no device plugged in it falls back to the conventional one.
    #[test]
    fn serial_fix_is_a_runnable_command() {
        let (group, cmd) = serial_access_fix();
        assert!(!group.trim().is_empty());
        assert_eq!(cmd, format!("sudo usermod -aG {group} $USER"));
        assert!(SERIAL_GROUP_CANDIDATES.contains(&group.as_str()) || cfg!(target_os = "linux"));
    }

    #[test]
    fn udev_rule_names_are_recognised() {
        assert!(udev_rule_matches("69-probe-rs.rules"));
        assert!(udev_rule_matches("60-openocd.rules"));
        assert!(udev_rule_matches("49-stlinkv2.rules"));
        // The app's own rules file, in both spellings.
        assert!(udev_rule_matches(crate::udev::RULES_FILE_NAME));
        assert!(udev_rule_matches(crate::udev::LEGACY_RULES_FILE_NAME));
        // Not a rules file, and not about a probe.
        assert!(!udev_rule_matches("70-probe-rs.txt"));
        assert!(!udev_rule_matches("99-systemd.rules"));
        assert!(!udev_rule_matches(""));
    }

    /// Every catalog entry must carry a non-empty, user-facing `impact` — it is
    /// the answer to "why does the IDE need this?" shown in the banner + Tools.
    #[test]
    fn every_tool_explains_its_impact() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        assert!(!s.tools.is_empty());
        for t in &s.tools {
            assert!(!t.impact.trim().is_empty(), "{} has no impact text", t.name);
            assert!(
                t.impact.len() > 20,
                "{} impact too terse: {:?}",
                t.name,
                t.impact
            );
        }
    }

    /// The core toolchain must be classed Blocking, feature tools must not be —
    /// otherwise the startup banner either misses a fatal gap or cries wolf.
    #[test]
    fn severity_matches_reality() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        let sev = |n: &str| s.tools.iter().find(|t| t.name == n).map(|t| t.severity);
        assert_eq!(sev("rustup"), Some(Severity::Blocking));
        assert_eq!(sev("rustc"), Some(Severity::Blocking));
        assert_eq!(sev("cargo-bloat"), Some(Severity::Feature));
        assert_eq!(sev("git"), Some(Severity::Feature));
        assert_eq!(sev("probe-rs"), Some(Severity::Feature));
        // Blocking on purpose, and not because everything stops: see the entry.
        assert_eq!(sev("CARGO_FEATURE_* env"), Some(Severity::Blocking));
    }

    /// Held by every test that writes a `CARGO_FEATURE_*` variable. The
    /// environment is one per process and tests run in parallel: without this,
    /// one test's variable appears and vanishes between two reads of another.
    static CARGO_FEATURE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A panic while holding it poisons the lock, not the environment (each
    /// holder removes its variable before it can assert), so the poison is
    /// ignored rather than failing the other test with an unrelated message.
    fn cargo_feature_env_lock() -> std::sync::MutexGuard<'static, ()> {
        CARGO_FEATURE_ENV
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// End to end, the way the startup self-check actually runs it: the entry's
    /// OWN `check_cmd` through `run_check_blocking`, then into the list the
    /// banner reads. The two halves were already tested separately, which is
    /// precisely what would hide a typo in the sentinel — the checker would
    /// still work, the banner would still render whatever it was given, and the
    /// entry would spawn `@cargo-feature-env` as a program, come back `Missing`,
    /// and never say why.
    #[test]
    fn the_catalog_entry_is_wired_to_the_check() {
        const VAR: &str = "CARGO_FEATURE_EIDE_WIRING";
        const NAME: &str = "CARGO_FEATURE_* env";
        let _env = cargo_feature_env_lock();

        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        let idx = s
            .tools
            .iter()
            .position(|t| t.name == NAME)
            .expect("entry is in the catalog");
        let (cmd, args, pat, minv) = {
            let t = &s.tools[idx];
            (t.check_cmd, t.check_args, t.check_pattern, t.min_version)
        };
        assert_eq!(cmd, CARGO_FEATURE_ENV_CHECK, "sentinel must match");

        // SAFETY: restored immediately below.
        unsafe { std::env::set_var(VAR, "1") };
        let status = run_check_blocking(cmd, args, pat, minv);
        unsafe { std::env::remove_var(VAR) };

        match &status {
            ToolStatus::Failed(msg) => assert!(msg.contains(VAR), "{msg}"),
            other => panic!("the sentinel did not reach the checker: {other:?}"),
        }

        // …and that status is what puts it in front of the user at startup.
        s.tools[idx].status = status;
        let names: Vec<&str> = s
            .blocking_problems(None)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert!(names.contains(&NAME), "banner would not show it: {names:?}");
    }

    /// The Windows driver entry, wired the way the startup self-check runs it.
    /// A typo in the sentinel would spawn `@usb-probe-driver` as a PROGRAM, come
    /// back `Missing`, and look in the UI exactly like a real finding - the same
    /// trap `the_catalog_entry_is_wired_to_the_check` guards for its own entry.
    ///
    /// The verdict itself depends on what is plugged into this machine, so the
    /// assertion is the one thing that does not: the sentinel must REACH the
    /// checker, whose every answer (Ok / Failed / Unknown) differs from what a
    /// failed spawn produces.
    #[test]
    fn the_usb_probe_driver_entry_is_wired_to_its_check() {
        // The dispatch is compiled on every platform, so this half runs anywhere.
        let status = run_check_blocking(PROBE_DRIVER_CHECK, &[], "", None);
        assert_ne!(
            status,
            ToolStatus::Missing,
            "the sentinel was spawned as a program instead of reaching the checker"
        );

        #[cfg(target_os = "windows")]
        {
            let s = make_tools_state();
            let s = s.lock().unwrap();
            let t = s
                .tools
                .iter()
                .find(|t| t.name == PROBE_DRIVER_TOOL)
                .expect("the entry is in the catalog on Windows");
            assert_eq!(t.check_cmd, PROBE_DRIVER_CHECK, "sentinel must match");
            assert!(
                t.install_cmd.is_none(),
                "replacing a device driver is never a one-click action"
            );
            assert!(
                !t.manual_url.is_empty(),
                "manual entries need somewhere to go"
            );
            // Not toolchain-gated: an ESP project must see it, and that is the
            // family it bites most often.
            assert!(t.toolchain.is_none(), "every chip here is probed over USB");
            // The one instruction that is expensive to get wrong: Interface 0 is
            // the COM port, and replacing ITS driver takes flashing away.
            assert!(t.impact.contains("Interface 2"), "{}", t.impact);
            assert!(t.impact.contains("Interface 0"), "{}", t.impact);
        }
    }

    /// The stray-variable entry has to reach the STARTUP BANNER, on any chip —
    /// that is the entire point of it being Blocking rather than Feature. It is
    /// also the only entry whose problem is something being present, so it goes
    /// in as `Failed`, not `Missing`.
    #[test]
    fn a_stray_cargo_feature_variable_reaches_the_startup_banner() {
        const NAME: &str = "CARGO_FEATURE_* env";
        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        let t = s
            .tools
            .iter_mut()
            .find(|t| t.name == NAME)
            .expect("entry is in the catalog");
        assert!(t.toolchain.is_none(), "it breaks builds on every toolchain");
        t.status = ToolStatus::Failed("CARGO_FEATURE_RT in this environment".into());

        for tc in [
            None,
            Some(&ToolchainKind::RustEmbedded),
            Some(&ToolchainKind::EspRust),
        ] {
            let names: Vec<&str> = s
                .blocking_problems(tc)
                .into_iter()
                .map(|(n, _, _)| n)
                .collect();
            assert!(names.contains(&NAME), "missing from the banner: {names:?}");
        }

        // The banner prints this under the bullet — it is the only place the
        // variable's NAME appears, since `impact` is fixed text with a
        // `<NAME>` placeholder in the command it tells you to run.
        assert_eq!(
            s.status_detail(NAME).as_deref(),
            Some("CARGO_FEATURE_RT in this environment")
        );

        // And the "installed it just now? re-check in Tools" hint must NOT be
        // shown for it: nothing was installed, and a re-check reads the same
        // inherited environment, so it would point at the one action that
        // cannot clear the problem. Only a genuinely absent tool earns it.
        assert!(
            !s.any_blocking_missing(None),
            "a Failed entry is not a missing tool"
        );
        let rustup = s
            .tools
            .iter_mut()
            .find(|t| t.name == "rustup")
            .expect("rustup is in the catalog");
        rustup.status = ToolStatus::Missing;
        assert!(
            s.any_blocking_missing(None),
            "a genuinely missing tool must still get the hint"
        );
    }

    /// One `ToolchainKind` covers nine chips that need three different things.
    ///
    /// `EspRust` is the toolchain of every Espressif part, but a C3 needs
    /// `riscv32imc`, a C6 needs `riscv32imac`, and an ESP32-S3 needs Espressif's
    /// entire rustc fork. Before the target gate, a C6 user was told to install
    /// the target their chip does NOT use and never told about the one it does.
    #[test]
    fn esp_tools_are_gated_on_the_project_target() {
        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        // Pretend everything is missing, so the filter is the only thing
        // deciding what gets reported.
        for t in &mut s.tools {
            t.status = ToolStatus::Missing;
        }
        let named = |target: &str| -> Vec<&'static str> {
            s.problems_for(Some(&ToolchainKind::EspRust), Some(target))
                .into_iter()
                .map(|(n, _, _)| n)
                .collect()
        };

        let imc = named("riscv32imc-unknown-none-elf");
        assert!(imc.contains(&"riscv32imc-unknown-none-elf"), "{imc:?}");
        assert!(!imc.contains(&"riscv32imac-unknown-none-elf"), "{imc:?}");
        assert!(!imc.contains(&"espup"), "{imc:?}");

        let imac = named("riscv32imac-unknown-none-elf");
        assert!(imac.contains(&"riscv32imac-unknown-none-elf"), "{imac:?}");
        // `riscv32imc` is a PREFIX of `riscv32imac`… but the other way round, so
        // the imac project must not be told about the imc target.
        assert!(!imac.contains(&"riscv32imc-unknown-none-elf"), "{imac:?}");
        assert!(!imac.contains(&"espup"), "{imac:?}");

        let xt = named("xtensa-esp32s3-none-elf");
        assert!(xt.contains(&"espup"), "{xt:?}");
        assert!(xt.contains(&"esp toolchain"), "{xt:?}");
        // Xtensa is not a rustup target; offering to `rustup target add` one
        // would send someone down an hour of the wrong road.
        assert!(!xt.iter().any(|n| n.starts_with("riscv32")), "{xt:?}");

        // Every ESP project still needs these, whatever its target.
        for target in [
            "riscv32imc-unknown-none-elf",
            "riscv32imac-unknown-none-elf",
            "xtensa-esp32s3-none-elf",
        ] {
            let all = named(target);
            assert!(all.contains(&"espflash"), "{target}: {all:?}");
            assert!(all.contains(&"rust-src"), "{target}: {all:?}");
        }

        // With no project open, nothing target-specific is hidden.
        let none: Vec<&str> = s
            .problems_for(Some(&ToolchainKind::EspRust), None)
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        for t in [
            "riscv32imc-unknown-none-elf",
            "riscv32imac-unknown-none-elf",
            "espup",
        ] {
            assert!(none.contains(&t), "hidden with no project: {none:?}");
        }
    }

    /// The environment check has to fire on a variable being THERE, which is
    /// the opposite of every other entry in this catalog — so both directions
    /// are pinned, and the message has to name the variable (that name is the
    /// entire diagnosis) and the error the user will otherwise be staring at.
    ///
    /// Serialized with the other test that writes such a variable, and restores
    /// the environment before it returns.
    #[test]
    fn a_stray_cargo_feature_variable_is_reported() {
        const NAME: &str = "CARGO_FEATURE_EIDE_SELFTEST";
        let _env = cargo_feature_env_lock();
        let before = std::env::var(NAME).ok();
        assert!(before.is_none(), "{NAME} is not something anyone sets");

        // SAFETY: single-threaded test, and the variable is restored below.
        unsafe { std::env::set_var(NAME, "1") };
        let hit = check_cargo_feature_env();
        unsafe { std::env::remove_var(NAME) };

        // The two halves live in two places on purpose, and the banner prints
        // them one under the other: the STATUS carries what only a probe can
        // know (which variable), the ENTRY's `impact` carries the consequence
        // and the fix, which are the same whatever the variable is called.
        match &hit {
            ToolStatus::Failed(msg) => {
                assert!(msg.contains(NAME), "the name IS the diagnosis: {msg}")
            }
            other => panic!("a stray variable must be reported, got {other:?}"),
        }
        let s = make_tools_state();
        let s = s.lock().unwrap();
        let impact = s
            .tools
            .iter()
            .find(|t| t.name == "CARGO_FEATURE_* env")
            .map(|t| t.impact)
            .expect("entry is in the catalog");
        assert!(
            impact.contains("More than one backend selected"),
            "must name the error the user actually sees: {impact}"
        );
        drop(s);

        // And it must be able to turn green — a check that never passes reads
        // as broken. Conditional on purpose: this very test exists because a
        // machine CAN carry a stray one, and a test that fails on the user's
        // environment rather than on the code is a test nobody trusts.
        let still_set: Vec<String> = std::env::vars()
            .map(|(k, _)| k)
            .filter(|k| k.starts_with("CARGO_FEATURE_"))
            .collect();
        if still_set.is_empty() {
            assert!(
                matches!(check_cargo_feature_env(), ToolStatus::Ok(_)),
                "clean environment must pass"
            );
        } else {
            // Not a failure: the detector is doing its job on a real leak.
            eprintln!("note: this environment really does carry {still_set:?}");
            assert!(matches!(check_cargo_feature_env(), ToolStatus::Failed(_)));
        }
    }

    /// An UNCHECKED catalog must report no problems — the banner may never fire
    /// on `Unknown`, or it would accuse the user before anything was verified.
    #[test]
    fn unchecked_catalog_reports_no_problems() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        assert!(s.problems(None).is_empty());
        assert!(s.blocking_problems(None).is_empty());
    }

    #[test]
    fn version_is_scanned_out_of_any_banner() {
        assert_eq!(
            parse_version("rustc 1.89.0 (abc 2026-01-01)").as_deref(),
            Some("1.89.0")
        );
        assert_eq!(parse_version("probe-rs 0.31.0").as_deref(), Some("0.31.0"));
        assert_eq!(
            parse_version("git version 2.45.1.windows.1").as_deref(),
            Some("2.45.1")
        );
        // A trailing dot is punctuation, not part of the number.
        assert_eq!(parse_version("v1.2. done").as_deref(), Some("1.2"));
        // Nothing dotted → nothing claimed.
        assert_eq!(parse_version("installed"), None);
        assert_eq!(parse_version("version 7"), None);
    }

    #[test]
    fn versions_compare_numerically_not_as_strings() {
        assert!(
            version_lt("1.9.0", "1.10.0"),
            "1.9 < 1.10 (string compare gets this wrong)"
        );
        assert!(!version_lt("1.10.0", "1.9.0"));
        assert!(version_lt("1.73.0", "1.74"));
        assert!(
            !version_lt("1.74.0", "1.74"),
            "missing components count as 0"
        );
        assert!(!version_lt("1.74", "1.74.0"));
        assert!(!version_lt("2.0", "1.99"));
        // Unreadable input must never be called outdated.
        assert!(!version_lt("", "1.74"));
        assert!(!version_lt("abc", "1.74"));
    }

    /// `Outdated` warns (it shows up in `problems`) but must NOT disable the
    /// features that use the tool — it may well still work.
    #[test]
    fn outdated_warns_but_never_disables() {
        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        if let Some(t) = s.tools.iter_mut().find(|t| t.name == "rustc") {
            t.status = ToolStatus::Outdated {
                found: "1.70.0".into(),
                min: "1.74",
            };
        }
        let names: Vec<&str> = s.problems(None).into_iter().map(|(n, _, _)| n).collect();
        assert!(
            names.contains(&"rustc"),
            "outdated must be reported: {names:?}"
        );
        assert!(
            !s.unavailable().contains(&"rustc"),
            "outdated must NOT gate features"
        );
    }

    /// Only requirements we can actually justify carry a minimum — an invented
    /// one would nag users whose older build works fine.
    #[test]
    fn min_versions_are_declared_sparingly() {
        let s = make_tools_state();
        let s = s.lock().unwrap();
        let with_min: Vec<&str> = s
            .tools
            .iter()
            .filter(|t| t.min_version.is_some())
            .map(|t| t.name)
            .collect();
        assert_eq!(
            with_min,
            vec!["rustc"],
            "unexpected min_version set: {with_min:?}"
        );
        // And the one we declare must itself be parseable by our comparator.
        assert!(!version_lt("1.74.0", "1.74"));
        // probe-rs deliberately carries NONE: its breakages are version RANGES
        // (0.31.0 panics enumerating, 0.32.0 can't open a WinUSB-bound ST-Link),
        // so a floor would mark the WORKING 0.29.0 outdated and push an upgrade
        // that breaks debugging. The failure hints name the problem instead.
        assert!(
            s.tools
                .iter()
                .find(|t| t.name == "probe-rs")
                .is_some_and(|t| t.min_version.is_none()),
            "probe-rs must not carry a minimum — see the comment in the catalog"
        );
    }

    /// A tool must be gated ONLY on proof of absence: `Unknown` (before the
    /// startup check) and the busy states must never grey a button out.
    #[test]
    fn unavailable_needs_proof_not_ignorance() {
        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        assert!(s.unavailable().is_empty(), "Unknown must not gate anything");

        for t in s.tools.iter_mut() {
            t.status = ToolStatus::Checking;
        }
        assert!(s.unavailable().is_empty(), "a running check must not gate");

        for t in s.tools.iter_mut() {
            t.status = ToolStatus::Ok("1.0".into());
        }
        assert!(s.unavailable().is_empty());

        if let Some(t) = s.tools.iter_mut().find(|t| t.name == "probe-rs") {
            t.status = ToolStatus::Missing;
        }
        assert_eq!(s.unavailable(), vec!["probe-rs"]);
    }

    /// Problems are filtered by the selected chip's toolchain: an ESP-only tool
    /// must not be reported while an STM32 chip is selected (and vice-versa).
    #[test]
    fn problems_are_filtered_by_toolchain() {
        let s = make_tools_state();
        let mut s = s.lock().unwrap();
        for t in s.tools.iter_mut() {
            t.status = ToolStatus::Missing;
        }
        let stm: Vec<&str> = s
            .problems(Some(&ToolchainKind::RustEmbedded))
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert!(
            stm.contains(&"rustup"),
            "common tools always count: {stm:?}"
        );
        assert!(stm.contains(&"probe-rs"), "{stm:?}");
        assert!(
            !stm.contains(&"espflash"),
            "ESP tool leaked into STM32: {stm:?}"
        );

        let esp: Vec<&str> = s
            .problems(Some(&ToolchainKind::EspRust))
            .into_iter()
            .map(|(n, _, _)| n)
            .collect();
        assert!(esp.contains(&"espflash"), "{esp:?}");
        assert!(
            !esp.contains(&"probe-rs"),
            "STM32 tool leaked into ESP: {esp:?}"
        );

        // Blocking is a strict subset of all problems.
        let all = s.problems(Some(&ToolchainKind::RustEmbedded)).len();
        let blocking = s
            .blocking_problems(Some(&ToolchainKind::RustEmbedded))
            .len();
        assert!(
            blocking > 0 && blocking < all,
            "all={all} blocking={blocking}"
        );
    }
}
