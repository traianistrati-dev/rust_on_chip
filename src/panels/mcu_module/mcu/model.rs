//! MCU data model — struct definition and rendering constants.

use crate::panels::mcu_module::clock::graph::ClockGraph;
use crate::panels::mcu_module::clock::{ClockConfig, ClockLimits, ClockPreset};
use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
use crate::panels::mcu_module::pins::logic::pin::Pin;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

// ── Rendering constants ───────────────────────────────────────────────────────

pub const PIN_HEIGHT: f32 = 50.0;
pub const PIN_WIDTH: f32 = 30.0;
pub const PIN_SPACING: f32 = 3.0;

// ── Runtime ────────────────────────────────────────────────────────────────

/// Project-level execution model — which code-generation runtime the firmware
/// targets.
///
/// - [`Runtime::Blocking`] (default): the classic bare-metal path — a
///   `#[cortex_m_rt::entry] fn main() -> !` with PORTABLE blocking APIs
///   (`embedded-io` / `embedded-hal` 1.0 bridges), so driver code is portable
///   across HALs. Per-module the peripheral can opt into Native.
/// - [`Runtime::Native`]: also bare-metal `#[entry] fn main()`, but the
///   peripherals expose the CONCRETE HAL types (`Serial`/`Spi`/`BlockingI2c` on
///   stm32f1xx-hal) — no portable bridges, no extra trait crates, max HAL
///   features. A project-wide "all native" shortcut; the per-module Portable/
///   Native selector is subsumed. Only where the concrete-HAL templates exist
///   ([`super::super::codegen::family::native_supported`] = STM32F1).
/// - [`Runtime::Async`]: an embassy async project — `#[embassy_executor::main]
///   async fn main(Spawner)` on `embassy-stm32`, with `.await`-able drivers
///   (`embedded-io-async` / `embedded-hal-async`). Selected in the System tab;
///   for STM32 it re-targets code generation to the async embassy backend.
///
/// - [`Runtime::Rtic`]: an RTIC 2.x project — `#[rtic::app]` with `Shared`,
///   `Local`, `#[init]`, `#[idle]`, and one hardware task per interrupt-enabled
///   input pin. Still bare-metal and still the concrete HAL, but the scheduler
///   owns `main`: clock + peripheral init moves into `#[init]`, and an interrupt
///   is a `#[task(binds = ...)]` rather than an `#[interrupt] fn` the user has
///   to write. Only where a cortex-m PAC + the `ExtiPin` trait exist
///   ([`super::super::codegen::family::rtic_supported`] = STM32F1).
///
/// Persisted in `mcu.config` (`@runtime`); old projects (no section) load as
/// `Blocking`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Runtime {
    #[default]
    Blocking,
    Native,
    Async,
    Rtic,
}

impl Runtime {
    /// The token written to / read from the `mcu.config` `@runtime` section.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Blocking => "Blocking",
            Self::Native => "Native",
            Self::Async => "Async",
            Self::Rtic => "Rtic",
        }
    }

    /// Parse the `@runtime` token; anything unrecognised (incl. a missing
    /// section) is the safe [`Runtime::Blocking`] default.
    pub fn from_token(s: &str) -> Self {
        match s.trim() {
            "Async" => Self::Async,
            "Native" => Self::Native,
            "Rtic" => Self::Rtic,
            _ => Self::Blocking,
        }
    }
}

// ── Auto-build on Save ───────────────────────────────────────────────────────

/// What the IDE does on Project Save when a library changed in `Cargo.toml`.
/// A workflow preference (chosen in the System tab), NOT a codegen choice — it
/// doesn't regenerate anything, so it's applied immediately (no staged Apply)
/// and kept out of the codegen state hash. Persisted in `mcu.config` (`@autobuild`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum AutoBuild {
    /// Never auto-build; the user runs Build/Check manually.
    Off,
    /// Run `cargo check` (fast; catches errors + resolves new deps). Default.
    #[default]
    Check,
    /// Run `cargo build --release` (slower; a full optimized build).
    Release,
}

impl AutoBuild {
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Check => "Check",
            Self::Release => "Release",
        }
    }
    pub fn from_token(s: &str) -> Self {
        match s.trim() {
            "Off" => Self::Off,
            "Release" => Self::Release,
            _ => Self::Check,
        }
    }
}

// ── Mcu struct ───────────────────────────────────────────────────────────────

/// A pad at one cell of the package's ball grid. Sparse: only the cells that
/// carry a ball are listed, which is what reproduces the staggered patterns
/// small WLCSP parts use.
#[derive(Clone)]
pub struct GridCell {
    /// 0-based, from the top — row 0 is "A".
    pub row: usize,
    /// 0-based, from the left — column 0 is "1".
    pub col: usize,
    pub pin: Pin,
}

impl GridCell {
    /// The datasheet designator for this cell — "A2", "F3".
    ///
    /// Row letters follow JEDEC: **I, O, Q, S, X and Z are skipped**, because
    /// they read as 1, 0, and each other on a package photo. Column numbers
    /// start at 1.
    pub fn designator(&self) -> String {
        format!("{}{}", row_letter(self.row), self.col + 1)
    }
}

/// The JEDEC row alphabet: I, O, Q, S, X and Z are absent because they read as
/// 1, 0, and each other on a package photo.
const ROW_LETTERS: &[u8] = b"ABCDEFGHJKLMNPRTUVWY";

/// JEDEC row letter for a 0-based row index.
pub fn row_letter(row: usize) -> String {
    let n = ROW_LETTERS.len();
    // Past the alphabet, JEDEC doubles the letter: AA, AB, … Rare, but a 21-row
    // BGA must not silently label two rows the same.
    if row < n {
        (ROW_LETTERS[row] as char).to_string()
    } else {
        let (hi, lo) = (row / n - 1, row % n);
        format!("{}{}", ROW_LETTERS[hi] as char, ROW_LETTERS[lo] as char)
    }
}

/// 0-based row index for a JEDEC row letter — the inverse of [`row_letter`].
/// `None` for a letter the standard doesn't use.
pub fn row_index(letters: &str) -> Option<usize> {
    let n = ROW_LETTERS.len();
    let idx = |c: char| ROW_LETTERS.iter().position(|l| *l as char == c);
    let mut chars = letters.chars();
    let first = idx(chars.next()?)?;
    match chars.next() {
        None => Some(first),
        Some(c) => {
            let second = idx(c)?;
            chars.next().is_none().then_some((first + 1) * n + second)
        }
    }
}

/// Split a package designator ("A2", "AB13") into 0-based `(row, col)`.
///
/// `None` for anything that isn't letters-then-digits — a plain pin number, an
/// empty string, a thermal pad's blank position.
pub fn parse_designator(pos: &str) -> Option<(usize, usize)> {
    let pos = pos.trim();
    let split = pos.find(|c: char| c.is_ascii_digit())?;
    if split == 0 {
        return None; // starts with a digit: a pin NUMBER, not a designator
    }
    let (letters, digits) = pos.split_at(split);
    let col: usize = digits.parse().ok()?;
    // Columns are 1-based on a package; 0 would be a malformed designator.
    Some((row_index(letters)?, col.checked_sub(1)?))
}

/// The package's ball grid.
#[derive(Clone)]
pub struct PinGrid {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<GridCell>,
}

/// Represents a microcontroller with four sides of pins and UI state.
#[derive(Clone)]
pub struct Mcu {
    /// Stable definition id (e.g. "esp32c3-graph"). Written into the generated
    /// `main.rs` header so reopening restores the exact chip. Empty when built
    /// outside the registry (e.g. unit tests via [`Mcu::new`]).
    pub id: String,
    pub name: String,
    /// Family / backend key (e.g. "stm32f1", "esp32c3") — selects the
    /// [`FamilyBackend`](crate::panels::mcu_module::codegen::family::FamilyBackend)
    /// used for code generation. Distinguishes chips that share a toolchain
    /// (e.g. all ARM HALs are `RustEmbedded` but differ per family).
    pub family: String,
    /// Toolchain family — governs which build/flash pipeline is used.
    pub toolchain: ToolchainKind,
    pub top_pins: Vec<Pin>,
    pub bottom_pins: Vec<Pin>,
    pub left_pins: Vec<Pin>,
    pub right_pins: Vec<Pin>,
    /// Ball grid for packages whose pads sit UNDER the die instead of along its
    /// edges — WLCSP, BGA, and the sparse staggered arrays small parts use
    /// (STM32C011D6Yx WLCSP12 has 12 balls in a 4×6 grid).
    ///
    /// Additive on purpose: the four side vectors stay, empty, and everything
    /// downstream reaches pins through [`Mcu::iter_all_pins`], which chains this
    /// in. So autowire, codegen, `mcu.config` persistence and jump-to-code work
    /// on a ball-grid chip without knowing it exists — only the LAYOUT differs.
    pub grid: Option<PinGrid>,
    /// The chip's DMA channels, as the vendor database describes them — the
    /// only way async codegen can name a channel on a family without a
    /// hand-written table. `None` for a built-in chip or one imported before
    /// this existed; see [`crate::panels::mcu_module::mcu_def::DmaDef`].
    pub dma: Option<crate::panels::mcu_module::mcu_def::DmaDef>,
    /// The chip's interrupt vector names — see
    /// [`crate::panels::mcu_module::mcu_def::McuDefinition::irq_vectors`].
    pub irq_vectors: Vec<String>,
    /// The chip's USART IP version — the gate on the swap/invert options, see
    /// [`crate::panels::mcu_module::stm32_pin_data::usart_has_swap_invert`].
    /// The chip on this board — see [`McuDefinition::board_chip`]. `None` for a
    /// bare part, which is what makes the diagram draw a chip instead of a PCB.
    pub board_chip: Option<String>,
    /// The flash in bytes, from the definition's `flash_size` - the same
    /// figure memory.x is written from. Where the flash is off-chip (the
    /// Raspberry Pi boards) nothing else knows it: the flash store's range is
    /// counted from it, and a range past the real chip would wrap onto the
    /// boot block. `None` when the definition does not state it.
    pub board_flash: Option<u32>,
    pub usart_ip: Option<String>,
    /// The chip's SDMMC IP version — the gate on WHICH constructor shape the
    /// SDMMC codegen may emit, see
    /// [`crate::panels::mcu_module::stm32_pin_data::sdmmc_kind`].
    pub sdmmc_ip: Option<String>,
    /// Currently selected pin number (None = no pin selected)
    pub selected_pin: Option<usize>,
    /// Pins-toolbar search box (next to "Rotate"). Pins that match stay bright,
    /// the rest are painted at half opacity. Transient view state: not persisted,
    /// not part of the codegen hash. See [`Mcu::pin_search_hits`].
    pub pin_search: String,
    /// Function whose ⓘ info window is open (None = closed)
    pub show_info: Option<PinFunction>,
    /// Vertical scroll offset (pixels) of the function list inside the chip.
    /// Fed by the wheel in `mcu_panel.rs` (which owns the canvas' wheel), clamped
    /// while the list is drawn.
    pub fn_scroll_offset: f32,
    /// Clock-tree configuration shown/edited in the "Clock" tab.
    /// `ClockConfig::None` for MCUs without a modelled clock tree yet.
    pub clock: ClockConfig,
    /// Per-chip datasheet frequency ceilings (validation + red diagram tags).
    /// Defaults to the STM32F103 values; imported `.ron` chips may override.
    pub clock_limits: ClockLimits,
    /// Chip-specific clock presets from the definition; empty → the family's
    /// built-in presets are shown in the Clock tab.
    pub clock_presets: Vec<ClockPreset>,
    /// Pristine clock-graph node states, exactly as the chip definition ships
    /// them — the target of the Clock tab's "Reset" button. Captured when the
    /// MCU is built, BEFORE any saved `@clock` state is adopted, so a reset
    /// really returns to the factory tree and not to whatever the project was
    /// opened with. `None` for chips with no modelled clock tree.
    pub clock_defaults: Option<ClockGraph>,
    /// Write the clock setup by HAND: the generated `main.rs` fences the clock
    /// block off and preserves whatever is inside it across regenerations.
    ///
    /// Defaults to ON exactly when the chip's family has no RCC recipe — there
    /// the Clock tab cannot produce code at all, so hand-writing it is the only
    /// way, and leaving the block generated would silently discard the edit.
    /// Persisted in `mcu.config` (`@clockmanual`), because it changes what is
    /// generated and so belongs in the project's reviewable config.
    pub clock_manual: bool,
    /// Virtual electronic modules (e.g. _USART) wired to the chip's pins and
    /// drawn beside it on the Pins canvas.
    pub modules: Vec<crate::panels::mcu_module::modules::VirtualModule>,
    /// Execution model — Blocking (bare-metal) or Async (embassy). Chosen in the
    /// System tab; drives which family backend generates `main.rs` and the
    /// embassy runtime deps. Persisted in `mcu.config`.
    pub runtime: Runtime,
    /// GPIO In/Out init style (STM32F1 blocking path): `Portable` wraps pins in
    /// the `pins/configs/io.rs` embedded-hal 1.0 bridge (`DigitalOut`/`DigitalIn`
    /// + `Delay`); `Native` binds the raw HAL pin (no io.rs, no embedded-hal for
    /// GPIO). Chosen in the System tab. Forced to raw on the Native runtime;
    /// irrelevant on Async (embassy `Output`/`Input`). Persisted in `mcu.config`.
    pub gpio_api: crate::panels::mcu_module::modules::ApiStyle,

    // ── Staged codegen-style choices (System tab "Apply") ─────────────────────
    // The APPLIED fields above (`runtime`, `gpio_api`) + each module's
    // `config.api_style`/`async_mode` are what codegen reads and the regen hash
    // gates on. The `pending_*` below are what the UI edits; nothing regenerates
    // until the user clicks **Apply**, which commits pending → applied. All
    // transient (not persisted, not hashed).
    /// Staged runtime (edited by the System-tab cards; applied on "Apply").
    pub pending_runtime: Runtime,
    /// Staged GPIO api (System-tab cards).
    pub pending_gpio_api: crate::panels::mcu_module::modules::ApiStyle,
    /// Staged per-module `(api_style, async_mode)` keyed by module id. Lazily
    /// populated when a module's config panel is opened; a module with no entry
    /// has no staged change.
    pub pending_module_styles: std::collections::BTreeMap<
        String,
        (
            crate::panels::mcu_module::modules::ApiStyle,
            crate::panels::mcu_module::modules::AsyncBusMode,
        ),
    >,
    /// Transient: the inline "Apply — Confirm/Cancel" prompt is showing.
    pub pending_apply_confirm: bool,

    /// Transient one-shot: set by `apply_pending_style` so the NEXT config-file
    /// regeneration rewrites the peripheral `configs/*.rs` templates in FULL
    /// (not just the constants block). A Runtime / Init-API change swaps the
    /// whole `init()` template (blocking ⇄ async ⇄ native), which lives in the
    /// "editable" region a normal splice preserves — so without this the code
    /// would not update on Apply. Cleared after that regen.
    pub config_regen_forced: bool,

    /// What Save does when a Cargo.toml library changed (System-tab preference,
    /// persisted). Default `Check`.
    pub auto_build: AutoBuild,
    /// Strict "deny panics" Clippy profile (System-tab preference, persisted).
    /// When on, a `[lints.clippy]` block (pedantic/nursery + unwrap/panic/
    /// indexing/arithmetic/as… all `deny`) is written into the project
    /// `Cargo.toml`, and the GENERATED code (main.rs entry fn + `configs/*.rs`)
    /// is exempted with `#[allow]` so only the user's own code is linted.
    /// Default `false`. See `project_gen::ensure_strict_lints`.
    pub strict_lints: bool,
    /// Debug-friendly `[profile.release]` (Debug-tab toggle, persisted). When
    /// on, the project's release profile is relaxed to `opt-level = 0`,
    /// `lto = false`, `debug = true` so every source line keeps code of its own
    /// and can hold a breakpoint. Off (default) is the optimised profile that
    /// normally gets flashed — smaller and correctly timed, but breakpoints on
    /// lines the optimiser folded away never arm.
    /// See `project_gen::ensure_debug_build`.
    pub debug_build: bool,
    /// Transient: id of a module the user clicked on the canvas, so the module
    /// list (below the chip) expands its entry next frame. Consumed + cleared by
    /// the panel. Not part of project state.
    pub expand_module: Option<String>,
    /// Undo stack for explicit Virtual-module add/remove (Ctrl+Z on the Pins
    /// tab). Each entry snapshots the modules + pin state before the change so
    /// an unwanted add/remove can be reverted. Transient, capped, not persisted.
    pub module_undo: Vec<ModuleUndo>,
    /// Transient: id of the module whose "Remove" button is armed and awaiting
    /// an inline confirmation (delete resets its pins, so it's confirmed first).
    pub module_remove_confirm: Option<String>,
    /// Transient: the I2C device - (bus instance, which device) - whose removal
    /// is armed, from the panel's row or its box on the canvas. Confirmed
    /// first because the device's config file goes with it, and the user's
    /// code below its markers too. One slot, like `module_remove_confirm`: a
    /// key that no longer names a device simply stops counting.
    pub i2c_remove_confirm: Option<(u8, crate::panels::mcu_module::modules::I2cDeviceKey)>,
    /// Transient: what the Virtual-modules panel's device rows did this frame,
    /// held until the CANVAS has drawn too and applied with what it did, as one
    /// batch. The panel is drawn first; applied there, a first edit that mints
    /// a bus re-keys its devices before the canvas draws them, and a canvas
    /// field still holding typed text is drawn under a new id and loses it.
    pub pending_i2c_acts: Vec<(u8, crate::panels::mcu_module::mcu::gui::i2c_devices::I2cAct)>,
    /// Transient: number of a pin the user just clicked, requesting a jump to the
    /// line that defines its variable in the generated code. Consumed + cleared
    /// by the panel (`AppIde`), which owns the editor. Not part of project state.
    pub pin_goto: Option<usize>,
    /// Transient: id of a module the user just clicked, requesting the same jump
    /// for EVERY pin it wires. Same consumer as [`Mcu::pin_goto`].
    pub module_goto: Option<String>,
    /// Id of the module currently selected on the canvas — drawn white, bigger
    /// and double-bordered, the module counterpart of [`Mcu::selected_pin`].
    /// Clicking it again clears it. View state only; not persisted.
    pub selected_module: Option<String>,
    /// The device of an I2C bus picked on the canvas: (the bus module's id,
    /// which device). Counts only while `selected_module` is that bus - read it
    /// through [`Mcu::selected_i2c_child`], which says so - so every place that
    /// clears the module selection clears this one too, without having to know
    /// it exists. View state only; not persisted.
    pub selected_i2c_child: Option<(String, crate::panels::mcu_module::modules::I2cDeviceKey)>,
    /// How far from the chip centre the I2C device boxes and their SCL/SDA wires
    /// reached LAST frame, `(x, y)`. Their place is only known once the modules
    /// are packed, which is after the canvas has been sized; `Mcu::draw` grows
    /// the canvas to this (one frame late, with a repaint) so the Scene's
    /// auto-fit does not clip them. View state only; not persisted.
    pub bus_reach: (f32, f32),
    /// Name of the device the canvas is pointing at, when the user said so
    /// OUTRIGHT — by clicking its tab. Most of the time nothing is here and
    /// [`Mcu::active_device`] derives the answer from the selected pin or module
    /// instead, so there is no second gesture to learn. View state only; not
    /// persisted.
    pub selected_device: Option<String>,
    /// The device whose row is showing its "remove?" confirm, by NAME — the
    /// identity a device answers to everywhere else.
    ///
    /// One at a time, like `module_remove_confirm`: arming a second row disarms
    /// the first, because a single `Option` cannot hold two. View state only;
    /// not persisted.
    pub device_remove_confirm: Option<String>,
    /// Where each device's tab was drawn LAST frame, as an offset from the chip
    /// centre.
    ///
    /// A tab's rect is only known after both paint passes have run, but its drag
    /// handle has to be registered BEFORE them — above the canvas background so
    /// it takes its own click, below every pad, box and field so they win any
    /// tie. Those cannot both happen in one frame, so the handle is minted
    /// against last frame's rect. Only the PRESS is hit-tested that way: egui
    /// carries a live drag on the id it started on, so a tab moving under its
    /// own drag keeps it. View state only; not persisted.
    pub device_tabs: Vec<crate::panels::mcu_module::mcu::gui::device_frame::TabHandle>,
    /// A device being dragged this frame, and by how much.
    ///
    /// Set before both paint passes and read by each of them, so the boxes and
    /// the io fields move by one value fixed before either ran. Cleared when the
    /// gesture ends.
    pub device_drag: Option<(String, (f32, f32))>,
    /// Transient one-shot: a click on EMPTY canvas (beside the chip, on no
    /// module box and no pin field) cleared the selection, so the module list
    /// below should collapse every entry too — the canvas and the list show the
    /// same "nothing is focused" state. Consumed + cleared by the panel.
    pub collapse_modules: bool,
    /// Diagram rotation toggle (persisted in `mcu.config` `@rotation`). `false`
    /// = default orientation. `true` = rotated one step clockwise: a 4-sided
    /// (QFP) chip and a BALL GRID (BGA, WLCSP) both become a 45° diamond, a
    /// 2-sided (DIP) chip turns 90° (vertical ⇄ horizontal). Purely a
    /// view/layout preference — never affects codegen. See `mcu/gui/rotate.rs`.
    pub rotated: bool,
    /// Manual on-canvas positions for in/out (GPIO In/Out/PWM) pin rename
    /// fields: pin number → field-CENTRE offset from the chip centre. Absent =
    /// auto-placed beside the pin. Draggable like virtual modules; persisted in
    /// `mcu.config` `@iopins`. View-only.
    pub io_pin_pos: std::collections::BTreeMap<usize, (f32, f32)>,
    /// Devices of an I2C bus the user dragged out of their bus's column:
    /// `(bus instance, device uid)` -> the offset of the box's top-left from
    /// the chip centre, like a dragged module's `pos`. Absent = in the column.
    /// A dragged device stays put when its bus moves, and moves with its module
    /// group. Persisted in `mcu.config` `@i2cpos`; view-only, never hashed.
    pub i2c_child_pos: std::collections::BTreeMap<(u8, u32), (f32, f32)>,
    /// Devices on the board: a name over a set of pads.
    ///
    /// A VIEW of the wiring, not part of it: nothing here changes a binding, an
    /// init call or a pad's function. A sensor with a UART and a spare input
    /// line is three pads that belong together, and the diagram had no way to
    /// say so.
    ///
    /// The one thing it DOES reach is `codegen::common::device_comment`, which
    /// is why it feeds `calculate_mcu_state_hash` - a rename has to regenerate
    /// main.rs or the comment would go stale.
    ///
    /// See [`PinGroup`](crate::panels::mcu_module::mcu_config::PinGroup) for
    /// why membership is by pad rather than by module.
    pub groups: Vec<crate::panels::mcu_module::mcu_config::PinGroup>,
    /// Notes, a documentation link and an image per Virtual Module, keyed by
    /// `(kind, instance)` - "USART1" - and NOT by module id.
    ///
    /// A module does not survive what these must: `reconcile_modules` drops a
    /// derived module whenever its peripheral has no pins and rebuilds it with
    /// a NEW id, and `free_module_id` hands a freed id to the next module
    /// created. `(kind, instance)` is what `reconcile_modules` itself matches
    /// on, and nothing that touches `modules` (reconcile, Remove, undo) sees
    /// this map. A key with no live module is an orphan and re-attaches when
    /// that peripheral is wired again. See [`crate::panels::mcu_module::modules::notes`].
    ///
    /// Persisted in `mcu.config` `@modulenotes`. NOT codegen input, so it is
    /// deliberately absent from `calculate_mcu_state_hash`.
    pub module_notes: std::collections::BTreeMap<
        crate::panels::mcu_module::modules::NotesKey,
        crate::panels::mcu_module::modules::ModuleNotes,
    >,
    /// IWDG / WWDG settings from the Configuration tab. Not a `VirtualModule`:
    /// those live on the Pins canvas and own pins, and a watchdog has none.
    /// Persisted in `mcu.config` `@watchdog`; feeds `calculate_mcu_state_hash`
    /// because it is codegen input.
    pub watchdog: crate::panels::mcu_module::watchdog::WatchdogSettings,
    /// Comparator settings from the Configuration tab, keyed by instance —
    /// present only for the ones switched on, so the KEYS are the enabled
    /// comparators. Half pin-less: the `COMP{n}_INP` pin is chosen on the Pins
    /// canvas, everything here is a register with no pin. Persisted in
    /// `mcu.config` `@comp`; feeds `calculate_mcu_state_hash` because it is
    /// codegen input.
    pub comp: crate::panels::mcu_module::comparator::CompSettings,
    /// The flash store from the Configuration tab: `Some` while it is switched
    /// on. Pin-less like the watchdogs. Persisted in `mcu.config`
    /// `@flashstore`; feeds `calculate_mcu_state_hash` because it is codegen
    /// input - main.rs, a config file, Cargo.toml and partitions.csv all follow
    /// it. See [`crate::panels::mcu_module::flash_store`].
    pub flash_store: Option<crate::panels::mcu_module::flash_store::FlashStoreConfig>,
    /// The IoT tab: Wi-Fi, the IP settings, MQTT. Pin-less like the
    /// watchdogs. Persisted in `mcu.config` `@iot` - WITHOUT the SSID and the
    /// passwords, which live in the git-ignored `secrets.rs`. Feeds
    /// `calculate_mcu_state_hash` because it is codegen input: main.rs, four
    /// config files and the Cargo.toml all follow it.
    pub iot: crate::panels::mcu_module::iot::IotConfig,
}

/// One Virtual-module undo snapshot (see [`Mcu::module_undo`]): the modules and
/// the pin state (`(number, function, custom_label)`) as they were BEFORE an
/// add/remove, plus a short label for the Undo button's hover.
#[derive(Clone)]
pub struct ModuleUndo {
    pub modules: Vec<crate::panels::mcu_module::modules::VirtualModule>,
    pub pins: Vec<(usize, PinFunction, String)>,
    pub label: String,
}
