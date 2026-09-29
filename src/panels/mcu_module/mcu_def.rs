//! Data-driven MCU definition (Phase 1 foundation).
//!
//! A serializable description of a chip — pins, clock, project params — that can
//! be loaded from a RON file.  [`McuDefinition::build_mcu`] turns it into the
//! runtime [`Mcu`] the configurator draws.
//!
//! This runs **in parallel** with the existing hardcoded factories
//! (`mock_mcu.rs`, `mock_esp32c3.rs`); later phases make the definition the
//! single source of truth and add the registry + family backends.

use serde::{Deserialize, Serialize};

use super::clock::graph::GraphClock;
use super::clock::{ClockConfig, ClockLimits, ClockPreset, Stm32f1Clock};
use super::mcu::Mcu;
use super::mcu::model::{GridCell, PinGrid};
use super::mcu_catalog::ToolchainKind;
use super::pins::logic::pin::Pin;
use super::pins::logic::pin_function::PinFunction;

/// One pin in a chip definition — the data form of [`Pin`] without runtime state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinDef {
    pub number: usize,
    pub name: String,
    #[serde(default)]
    pub reserved: bool,
    /// Complete list of selectable functions (e.g. `[GpioInput, GpioOutput, …]`).
    /// Empty for reserved pins (VDD, VSS, …).
    #[serde(default)]
    pub functions: Vec<PinFunction>,
    /// `(vendor signal name, alternate-function index)` for the signals whose AF
    /// number the vendor publishes — `[("TIM1_CH1N", 4), ("USART1_TX", 7)]`.
    ///
    /// Captured at import from the GPIO IP file (see
    /// [`crate::panels::mcu_module::stm32_pin_data::GpioAf`]) and stored so the
    /// data is in the project rather than in a folder the user may not keep. It
    /// is not consumed yet: configuring a pin to an arbitrary alternate function
    /// needs it, and that is the next step — capturing it now means the chips
    /// imported today will not have to be imported again then.
    ///
    /// Absent for STM32F1, which has no per-pin AF mux at all (it remaps whole
    /// peripherals through AFIO), and for any pin whose signals are "additional
    /// functions" (ADC inputs, RTC tamper, …) rather than alternate ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub af: Vec<(String, u8)>,
    /// `(function, GPIO)` for the functions this package pin gets from a GPIO
    /// OTHER than [`name`](Self::name).
    ///
    /// Small packages bond two die pads to one package pin - an STM32G030F6P's
    /// pin 1 is both PB7 and PB8, each with its own signals. The pin is one pin
    /// (one position, one thing you can solder to), so it is one `PinDef`; but
    /// picking `I2C1_SCL` there means `p.PB8` while `USART1_RX` means `p.PB7`,
    /// and only this says which.
    ///
    /// SPARSE by design: a function both GPIOs provide - plain input/output -
    /// is absent, and resolves to `name`. Absent entirely on the overwhelming
    /// majority of pins, so no existing definition changes on disk.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fn_owner: Vec<(PinFunction, String)>,
}

impl PinDef {
    /// Extract a `PinDef` from a runtime [`Pin`] — used by round-trip tests and
    /// to export the current hardcoded factories to RON.
    pub fn from_pin(p: &Pin) -> Self {
        Self {
            number: p.number,
            name: p.name.clone(),
            reserved: p.reserved,
            functions: p.available_functions.clone(),
            // The runtime `Pin` does not carry it (nothing reads it yet); a
            // round-trip through a live chip therefore drops it.
            af: Vec::new(),
            fn_owner: p.fn_owner.clone(),
        }
    }

    /// Build a runtime [`Pin`] (the selected function starts `Unset`).
    pub fn to_pin(&self) -> Pin {
        Pin {
            number: self.number,
            name: self.name.clone(),
            reserved: self.reserved,
            available_functions: self.functions.clone(),
            selected_function: PinFunction::Unset,
            custom_label: String::new(),
            irq: None,
            irq_priority: crate::panels::mcu_module::pins::logic::pin::model::TaskPriority::default(
            ),
            io_mode: None,
            af: self.af.clone(),
            fn_owner: self.fn_owner.clone(),
        }
    }
}

/// What a chip's DMA controller can do, as captured from the vendor database.
///
/// `None` for a definition imported before this existed, or from a source that
/// carries no DMA data (the public open-pin-data repo has none) - codegen then
/// falls back to the hand-harvested family tables in
/// [`crate::panels::mcu_module::codegen::dma_map`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DmaDef {
    /// `true` when any channel can serve any peripheral (DMAMUX / GPDMA), which
    /// is 1367 of the database's 2123 parts. Then no request table is needed at
    /// all: the allocator just hands out free channels.
    pub mux: bool,
    /// Every channel the chip has, with the interrupt each is served by.
    pub channels: Vec<crate::panels::mcu_module::codegen::dma_data::DmaChannel>,
    /// `("USART1_TX", ["DMA2_CH7"])` — which channels each request may use, on
    /// a chip where that is FIXED in silicon. Empty when `mux` (any channel
    /// serves any request) and when the chip predates this field, in which case
    /// codegen falls back to the hand-written family tables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<(String, Vec<String>)>,
}

/// One pad of a ball grid: where it sits, and the pin itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GridCellDef {
    /// 0-based row from the top (0 = "A"), column from the left (0 = "1").
    pub row: usize,
    pub col: usize,
    pub pin: PinDef,
}

/// A ball grid — WLCSP / BGA, where the pads sit under the die rather than
/// along its edges. Sparse: list only the cells that carry a ball, which is how
/// a staggered pattern like WLCSP12's is described.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PinGridDef {
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<GridCellDef>,
}

/// Where a chip's pins are: along the four sides, in a ball grid, or both.
///
/// `grid` is an ADDITIVE option rather than `PinLayout` becoming an enum. An
/// enum would change the serialized shape of `layout:` and break every `.ron`
/// already on disk — the bundled ones and, worse, every chip a user imported
/// from ST's XML. With a defaulted field, an old file parses unchanged and an
/// edge-packaged chip is exactly what it always was.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct PinLayout {
    #[serde(default)]
    pub top: Vec<PinDef>,
    #[serde(default)]
    pub bottom: Vec<PinDef>,
    #[serde(default)]
    pub left: Vec<PinDef>,
    #[serde(default)]
    pub right: Vec<PinDef>,
    #[serde(default)]
    pub grid: Option<PinGridDef>,
}

/// Project-generation parameters: target triple, HAL dependency line, memory
/// layout and probe/flash chip — everything `project_gen` needs to emit a
/// buildable Cargo project. Consumed alongside the chip's `ToolchainKind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectDef {
    pub pkg_name: String,
    pub target: String,
    #[serde(default)]
    pub flash_origin: String,
    #[serde(default)]
    pub flash_size: String,
    #[serde(default)]
    pub ram_origin: String,
    #[serde(default)]
    pub ram_size: String,
    pub hal_dep: String,
    /// The HAL line to use INSTEAD on the Async runtime, when the family changes
    /// crates rather than features.
    ///
    /// RP is the first family where it does: `rp2040-hal` drives the chip
    /// blocking, `embassy-rp` drives it async, and they are different crates.
    /// ESP and every STM32 but the F1 keep one HAL for both runtimes, so they
    /// leave this `None`. The F1 swaps `stm32f1xx-hal` for embassy-stm32, but
    /// its line is derived when this is `None` - see [`ProjectDef::for_async`].
    ///
    /// It lives on the CHIP rather than on the backend because the feature is
    /// chip-specific: an RP2350A wants `rp235xa` and an RP2350B `rp235xb`, and a
    /// backend deriving it from the family alone would get one of them wrong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hal_dep_async: Option<String>,
    pub probe_chip: String,
    #[serde(default)]
    pub memory_comment: String,
}

/// Clock model + defaults for this chip, keyed per clock family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClockDef {
    /// Compact STM32F1 authoring form — upgraded to a full graph at load.
    Stm32f1(Stm32f1Clock),
    /// Built-in ESP32-C3 clock graph (topology + diagram are code).
    Esp32c3,
    /// Data-driven clock tree + diagram, importable for any family.
    Graph(GraphClock),
    /// No modelled clock tree yet (STM8, …).
    None,
}

impl ClockDef {
    /// Build the runtime clock. The graph is the only runtime model, so the
    /// compact family forms are upgraded to a full graph here (topology +
    /// diagram from the family's `*_graph` / `*_layout`; `limits` only feeds the
    /// STM32F1 HSE-range label).
    ///
    /// Public because the AI clock importer needs the same upgrade: its second
    /// pass merges onto whatever tree the form currently holds, family template
    /// included.
    pub fn to_config(&self, limits: &ClockLimits) -> ClockConfig {
        use super::clock::graph::{
            GraphClock, esp32c3_graph, esp32c3_layout, layout::stm32f1_layout, stm32f1_graph,
        };
        match self {
            ClockDef::Stm32f1(c) => ClockConfig::Graph(GraphClock {
                graph: stm32f1_graph(c),
                layout: stm32f1_layout(limits),
                bindings: Default::default(),
            }),
            ClockDef::Esp32c3 => ClockConfig::Graph(GraphClock {
                graph: esp32c3_graph(),
                layout: esp32c3_layout(),
                bindings: Default::default(),
            }),
            ClockDef::Graph(g) => ClockConfig::Graph(g.clone()),
            ClockDef::None => ClockConfig::None,
        }
    }

    /// Same clock family? Used to filter presets that don't fit the chip.
    fn same_family(&self, other: &ClockDef) -> bool {
        matches!(
            (self, other),
            (ClockDef::Stm32f1(_), ClockDef::Stm32f1(_))
                | (ClockDef::Esp32c3, ClockDef::Esp32c3)
                | (ClockDef::Graph(_), ClockDef::Graph(_))
                | (ClockDef::None, ClockDef::None)
        )
    }
}

/// One named clock preset shipped with a definition. `config` reuses the
/// family-tagged [`ClockDef`], so presets stay extensible per family; presets
/// whose family doesn't match the chip's `clock` are dropped at build time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClockPresetDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub config: ClockDef,
}

/// `skip_serializing_if` helper — omit `clock_limits` from generated RON when
/// it carries only the family defaults.
fn limits_are_default(l: &ClockLimits) -> bool {
    *l == ClockLimits::default()
}

/// A complete, importable MCU definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct McuDefinition {
    /// Stable identifier, e.g. "stm32f103c8t6".
    pub id: String,
    /// Display name shown in the chip selector.
    pub display_name: String,
    /// Family / backend key (e.g. "stm32f1", "esp32c3") — selects the codegen
    /// + clock backend in later phases.
    pub family: String,
    #[serde(default)]
    pub package: String,
    /// The chip on this BOARD, when the definition describes a board rather
    /// than a bare part — `Some("RP2350")` for a Raspberry Pi Pico 2 W.
    ///
    /// A board is not a chip with a nicer name: its pin numbers are header
    /// positions, some of its pads are not on the header at all, and the thing
    /// the diagram draws is a PCB with a chip on it. Everything that follows —
    /// the green board fill, the two blank slots that keep the off-header pads
    /// clear of the edge, the chip square, the radio can — reads this one field
    /// rather than testing the family, so an ESP or STM32 dev board gets the
    /// same treatment by saying so.
    ///
    /// `None` for every bare part, which is nearly all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board_chip: Option<String>,
    /// Datasheet maximum core frequency in MHz, captured at import from
    /// the vendor file's `<Frequency>`. A display fact only — the clock
    /// editor's ceilings live in [`ClockLimits`], which is a per-FAMILY
    /// table and would say 72 MHz for any chip without its own graph.
    /// `None` when the vendor states none (the whole C0 series does not).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_mhz: Option<u32>,
    /// On-die RAM in KiB, for the catalogue and its filters.
    ///
    /// NOT [`ProjectDef::ram_size`], which is the linker script's RAM region and
    /// is empty on Espressif parts because `esp-hal` writes `memory.x` itself.
    /// Putting the SRAM figure there would state a linker value nothing sets —
    /// the same shape of mistake as the `hal_dep` that went on claiming
    /// "esp-hal 0.23" long after the template had moved on.
    ///
    /// `#[serde(default)]`, so every definition written before this field
    /// existed still parses; those fall back to the `memory.x` size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sram_kb: Option<u32>,
    /// DMA channels + whether they are muxed. See [`DmaDef`].
    #[serde(default)]
    pub dma: Option<DmaDef>,
    /// The chip's interrupt vector names, as the vendor's NVIC table lists them
    /// (`I2C1_EV`, `I2C1`, `USART3_4_LPUART1`). `bind_interrupts!` is keyed by
    /// vector, and which peripherals share one is per-chip — see
    /// [`crate::panels::mcu_module::codegen::nvic`]. Empty when the chip was not
    /// imported from the vendor database.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub irq_vectors: Vec<String>,
    /// The chip's USART IP version as the vendor names it (`sci3_v2_1_Cube`).
    /// The only thing that says whether the USART can swap/invert its lines —
    /// see [`crate::panels::mcu_module::stm32_pin_data::usart_has_swap_invert`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usart_ip: Option<String>,
    /// The chip's SDMMC (or SDIO) IP version as the vendor names it. Decides
    /// which of embassy's two constructor shapes the codegen may emit — see
    /// [`crate::panels::mcu_module::stm32_pin_data::sdmmc_kind`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sdmmc_ip: Option<String>,
    #[serde(default)]
    pub cpu: String,
    pub toolchain: ToolchainKind,
    pub project: ProjectDef,
    #[serde(default)]
    pub pins: PinLayout,
    pub clock: ClockDef,
    /// Per-chip datasheet frequency ceilings. Omitted (or partial) fields fall
    /// back to the family defaults (STM32F103 values), so existing `.ron`
    /// files keep parsing unchanged. Example override in RON:
    /// `clock_limits: (sysclk_max: 24000000, hclk_max: 24000000)`.
    #[serde(default, skip_serializing_if = "limits_are_default")]
    pub clock_limits: ClockLimits,
    /// Chip-specific one-click presets for the Clock tab; when empty the
    /// family's built-in presets are shown instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clock_presets: Vec<ClockPresetDef>,
}

impl McuDefinition {
    /// This chip's clock, falling back to its FAMILY's tree when the definition
    /// declares none.
    ///
    /// `clock: None` means "no tree in this file", which is not the same as "no
    /// tree exists". A definition saved before its family had one — or by a form
    /// where the dropdown was left at None — otherwise shows *"Clock
    /// configuration is not modelled yet"* for a chip the IDE models perfectly
    /// well. That is what a stale user `mcus/esp32c3.ron` did: it overrode the
    /// bundled definition (same id) and took the ESP32-C3's clock tab with it.
    ///
    /// [`ClockChoice::for_family`] is the single source of truth for that
    /// mapping — the same one the XML and datasheet importers use — so a family
    /// with no modelled tree (STM8, …) still yields `None` and the message is
    /// then true.
    pub fn effective_clock(&self) -> ClockDef {
        use crate::panels::mcu_module::mcu_form::ClockChoice;
        match &self.clock {
            ClockDef::None => ClockChoice::for_family(&self.family).to_def(),
            declared => declared.clone(),
        }
    }

    /// Build the runtime [`Mcu`] (pin diagram + clock) from this definition.
    pub fn build_mcu(&self) -> Mcu {
        let map = |defs: &[PinDef]| defs.iter().map(PinDef::to_pin).collect::<Vec<_>>();
        let mut mcu = Mcu::new(
            self.display_name.clone(),
            self.family.clone(),
            self.toolchain.clone(),
            map(&self.pins.top),
            map(&self.pins.bottom),
            map(&self.pins.left),
            map(&self.pins.right),
        );
        mcu.id = self.id.clone();
        mcu.dma = self.dma.clone();
        mcu.irq_vectors = self.irq_vectors.clone();
        mcu.board_chip = self.board_chip.clone();
        mcu.usart_ip = self.usart_ip.clone();
        mcu.sdmmc_ip = self.sdmmc_ip.clone();
        mcu.grid = self.pins.grid.as_ref().map(|g| PinGrid {
            rows: g.rows,
            cols: g.cols,
            cells: g
                .cells
                .iter()
                .map(|c| GridCell {
                    row: c.row,
                    col: c.col,
                    pin: c.pin.to_pin(),
                })
                .collect(),
        });
        mcu.clock = self.effective_clock().to_config(&self.clock_limits);
        // `Mcu::new` could only ask the FAMILY whether the clock is generated,
        // and it answered before the tree arrived. Now that it has, ask again:
        // a tree the generic recipe can read generates real code, so defaulting
        // it to "hand-written" would fence off a block nobody wrote and freeze
        // it there. A project's own `@clockmanual` still overrides this later.
        mcu.clock_manual = !crate::panels::mcu_module::codegen::rcc::generates_clock_code_for(
            &mcu.family,
            &mcu.clock,
        );
        // The definition's tree is this chip's factory clock — snapshot it for
        // the Clock tab's "Reset" button before any saved state is applied.
        mcu.capture_clock_defaults();
        mcu.clock_limits = self.clock_limits;
        mcu.clock_presets = self
            .clock_presets
            .iter()
            .filter(|p| p.config.same_family(&self.clock))
            .filter_map(|p| match &p.config {
                ClockDef::Stm32f1(c) => Some(ClockPreset {
                    name: p.name.clone(),
                    description: p.description.clone(),
                    config: c.clone(),
                }),
                // `ClockPreset` is the Stm32f1 runtime preset; other families
                // carry presets in-graph, so they don't convert here.
                ClockDef::Esp32c3 | ClockDef::Graph(_) | ClockDef::None => None,
            })
            .collect();
        mcu
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins::builtin_for;
    use crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx;

    fn stm_def() -> McuDefinition {
        builtin_for("stm32f103c8t6").expect("built-in stm32f103c8t6 definition")
    }

    /// The WLCSP12 example must parse and build — it is the reference for the
    /// ball-grid format, and a `.ron` nobody loads is documentation that rots.
    #[test]
    fn the_ball_grid_example_parses_and_builds() {
        const SRC: &str = include_str!("../../../assets/mcus/examples/stm32c011d6yx_wlcsp12.ron");
        let def: McuDefinition = ron::from_str(SRC).expect("the WLCSP12 example must parse");
        let grid = def.pins.grid.as_ref().expect("it is a ball-grid chip");
        assert_eq!((grid.rows, grid.cols), (6, 4));
        assert_eq!(grid.cells.len(), 12, "WLCSP12 has twelve balls");
        assert!(
            def.pins.top.is_empty() && def.pins.left.is_empty(),
            "a WLCSP has no edge pins at all"
        );

        let mcu = def.build_mcu();
        assert_eq!(
            mcu.iter_all_pins().count(),
            12,
            "balls must reach the normal pin iterator, or codegen and autowire \
             would not see them"
        );
        assert!(mcu.find_pin(1).is_some_and(|p| p.name == "PB6"));
    }

    /// An existing `.ron` predates the `grid` field and must still parse — the
    /// reason the layout gained an optional field instead of becoming an enum.
    #[test]
    fn a_definition_without_a_grid_still_parses() {
        let layout: PinLayout = ron::from_str("(top: [], bottom: [], left: [], right: [])")
            .expect("the pre-grid shape must still load");
        assert!(layout.grid.is_none());
    }

    #[test]
    fn ron_round_trips_definition() {
        let def = stm_def();
        let ron = ron::to_string(&def).expect("serialize to RON");
        let parsed: McuDefinition = ron::from_str(&ron).expect("parse RON");
        assert_eq!(def, parsed, "RON round-trip must be lossless");
    }

    #[test]
    fn pindef_round_trips_a_pin() {
        // A real pin carrying multiple peripheral functions.
        let pin = create_stm32f103c8tx()
            .top_pins
            .into_iter()
            .find(|p| !p.reserved && !p.available_functions.is_empty())
            .unwrap();
        let back = PinDef::from_pin(&pin).to_pin();
        assert_eq!(back.number, pin.number);
        assert_eq!(back.name, pin.name);
        assert_eq!(back.reserved, pin.reserved);
        assert_eq!(back.available_functions, pin.available_functions);
    }

    #[test]
    fn build_mcu_matches_factory() {
        let factory = create_stm32f103c8tx();
        let built = stm_def().build_mcu();

        let same = |a: &[Pin], b: &[Pin]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| {
                    x.number == y.number
                        && x.name == y.name
                        && x.reserved == y.reserved
                        && x.available_functions == y.available_functions
                })
        };
        assert!(same(&built.top_pins, &factory.top_pins), "top pins differ");
        assert!(
            same(&built.bottom_pins, &factory.bottom_pins),
            "bottom pins differ"
        );
        assert!(
            same(&built.left_pins, &factory.left_pins),
            "left pins differ"
        );
        assert!(
            same(&built.right_pins, &factory.right_pins),
            "right pins differ"
        );
        assert_eq!(built.clock, factory.clock, "clock differs");
    }

    /// A definition without `clock_limits` / `clock_presets` (every existing
    /// `.ron`) must keep parsing and fall back to the family defaults.
    #[test]
    fn missing_clock_fields_fall_back_to_defaults() {
        let def = stm_def(); // bundled RON predates the new fields
        assert_eq!(def.clock_limits, ClockLimits::default());
        assert!(def.clock_presets.is_empty());

        let mcu = def.build_mcu();
        assert_eq!(mcu.clock_limits, ClockLimits::default());
        assert!(
            mcu.clock_presets.is_empty(),
            "empty -> family presets in the GUI"
        );
    }

    /// Custom limits and presets declared in a definition reach the runtime Mcu.
    #[test]
    fn clock_limits_and_presets_flow_into_mcu() {
        let mut def = stm_def();
        def.clock_limits = ClockLimits {
            sysclk_max: 24_000_000,
            hclk_max: 24_000_000,
            ..ClockLimits::default()
        };
        def.clock_presets = vec![ClockPresetDef {
            name: "24 MHz max".to_owned(),
            description: "Value-line style cap.".to_owned(),
            config: ClockDef::Stm32f1(Stm32f1Clock::default()),
        }];

        let mcu = def.build_mcu();
        assert_eq!(mcu.clock_limits.sysclk_max, 24_000_000);
        assert_eq!(mcu.clock_presets.len(), 1);
        assert_eq!(mcu.clock_presets[0].name, "24 MHz max");
    }

    /// Presets from a different clock family are dropped, not mis-applied.
    #[test]
    fn foreign_family_presets_are_filtered() {
        let mut def = stm_def(); // chip clock family: Stm32f1
        def.clock_presets = vec![
            ClockPresetDef {
                name: "fits".to_owned(),
                description: String::new(),
                config: ClockDef::Stm32f1(Stm32f1Clock::default()),
            },
            ClockPresetDef {
                name: "foreign".to_owned(),
                description: String::new(),
                config: ClockDef::None,
            },
        ];

        let mcu = def.build_mcu();
        assert_eq!(mcu.clock_presets.len(), 1);
        assert_eq!(mcu.clock_presets[0].name, "fits");
    }

    /// New fields survive a RON round-trip (and stay omitted when default).
    #[test]
    fn clock_fields_round_trip_in_ron() {
        let mut def = stm_def();
        def.clock_limits = ClockLimits {
            adcclk_max: 12_000_000,
            ..ClockLimits::default()
        };
        def.clock_presets = vec![ClockPresetDef {
            name: "p".to_owned(),
            description: "d".to_owned(),
            config: ClockDef::Stm32f1(Stm32f1Clock::default()),
        }];

        let ron = ron::to_string(&def).expect("serialize");
        let parsed: McuDefinition = ron::from_str(&ron).expect("parse");
        assert_eq!(parsed, def);

        // Default limits / empty presets are skipped in the output entirely.
        let plain = ron::to_string(&stm_def()).expect("serialize");
        assert!(!plain.contains("clock_limits"));
        assert!(!plain.contains("clock_presets"));
    }

    /// A chip can ship a data-driven `ClockDef::Graph`: it round-trips through
    /// RON and builds into a runtime `ClockConfig::Graph`.
    #[test]
    fn graph_clock_def_round_trips_and_builds() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::layout::stm32f1_layout;
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32f1_graph};

        let gc = GraphClock {
            graph: stm32f1_graph(&Stm32f1Clock::default()),
            layout: stm32f1_layout(&ClockLimits::default()),
            bindings: Default::default(),
        };
        let mut def = stm_def();
        def.clock = ClockDef::Graph(gc);

        let mcu = def.build_mcu();
        assert!(
            matches!(mcu.clock, ClockConfig::Graph(_)),
            "build_mcu yields a graph clock"
        );

        let ron = ron::to_string(&def).expect("serialize def with graph clock");
        let back: McuDefinition = ron::from_str(&ron).expect("parse def with graph clock");
        assert_eq!(
            def, back,
            "definition with embedded graph + layout must round-trip"
        );
    }

    /// The Clock tab's "Reset" button: an edited tree returns to exactly the
    /// definition's factory states, and resetting twice is a no-op.
    #[test]
    fn reset_clock_restores_the_definition_default() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::NodeState;

        let mut mcu = stm_def().build_mcu();
        assert!(mcu.clock_defaults.is_some(), "defaults captured at build");
        assert!(mcu.clock_is_default(), "a freshly built chip is pristine");
        assert!(!mcu.reset_clock(), "nothing to reset yet");

        // Edit the PLL multiplier the way the diagram widget does.
        let pristine = mcu.clock.clone();
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("stm32f103 has a graph clock");
        };
        let pll = gc.graph.node_mut("pllmul").expect("pllmul node");
        pll.state = NodeState::Value(4);

        assert!(!mcu.clock_is_default(), "edit is detected");
        assert!(mcu.reset_clock(), "reset reports the change");
        assert_eq!(mcu.clock, pristine, "tree is back to the factory config");
        assert!(!mcu.reset_clock(), "second reset is a no-op");
    }

    /// "Save to chip" writes the edited TOPOLOGY into the definition — the part
    /// `mcu.config` cannot carry (it round-trips node states, not the graph). A
    /// definition carrying an edited tree must survive the `.ron` round trip and
    /// rebuild into exactly that tree.
    #[test]
    fn an_edited_clock_tree_survives_the_definition_round_trip() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::edit::{PaletteKind, add_node, connect};
        use crate::panels::mcu_module::clock::graph::{GraphClock, auto_layout};

        // Start from the chip's own tree and add a node, as the editor does.
        let mut mcu = stm_def().build_mcu();
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("graph clock");
        };
        let mut boxes = auto_layout(&gc.graph).nodes;
        let added = add_node(
            &mut gc.graph,
            &mut boxes,
            PaletteKind::Output,
            10.0,
            10.0,
            96.0,
            26.0,
        );
        connect(&mut gc.graph, "hclk", &added).expect("wire the new output");
        let edited = GraphClock {
            graph: gc.graph.clone(),
            layout: gc.layout.clone(),
            bindings: Default::default(),
        };

        let mut def = stm_def();
        def.clock = ClockDef::Graph(edited.clone());
        let text = ron::ser::to_string_pretty(&def, ron::ser::PrettyConfig::default())
            .expect("serialize the edited definition");
        let back: McuDefinition = ron::from_str(&text).expect("parse it back");
        assert_eq!(back, def, "the edited tree round-trips");

        let rebuilt = back.build_mcu();
        let ClockConfig::Graph(out) = &rebuilt.clock else {
            panic!("graph clock");
        };
        assert!(
            out.graph.node(&added).is_some(),
            "the node added in the editor is in the rebuilt chip"
        );
        assert!(
            out.graph.edges.iter().any(|e| e.to == added),
            "and so is its wire"
        );
        // The saved tree is the chip's factory config now, so Reset aims at it.
        assert!(rebuilt.clock_is_default());
    }

    /// A definition that declares no clock still gets its FAMILY's tree.
    ///
    /// The bug: a stale user `mcus/esp32c3.ron` with `clock: None` overrode the
    /// bundled ESP32-C3 (same id) and the Clock tab said "not modelled yet" for
    /// a chip the IDE models fully.
    #[test]
    fn a_definition_without_a_clock_falls_back_to_its_family() {
        use crate::panels::mcu_module::clock::ClockConfig;

        let mut def = stm_def();
        def.clock = ClockDef::None;
        def.family = "esp32c3".into();
        assert!(
            matches!(def.effective_clock(), ClockDef::Esp32c3),
            "esp32c3 has a modelled tree"
        );
        assert!(matches!(def.build_mcu().clock, ClockConfig::Graph(_)));

        // The same for an STM32 family.
        def.family = "stm32f1".into();
        assert!(matches!(def.effective_clock(), ClockDef::Stm32f1(_)));
        assert!(matches!(def.build_mcu().clock, ClockConfig::Graph(_)));

        // A family with no modelled tree keeps None — the message is true there.
        def.family = "stm8".into();
        assert!(matches!(def.effective_clock(), ClockDef::None));
        assert!(matches!(def.build_mcu().clock, ClockConfig::None));
    }

    /// A family with no template still builds — it just arrives with no tree,
    /// which the Clock tab now offers to create instead of refusing.
    #[test]
    fn a_family_without_a_template_builds_without_a_clock() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::mcu_form::ClockChoice;

        let mut def = stm_def();
        def.clock = ClockDef::None;
        // H5 is the case that matters: importable from XML, no template, and
        // its clock code is hand-written.
        def.family = "stm32h5".into();
        assert_eq!(ClockChoice::for_family("stm32h5"), ClockChoice::None);

        let mcu = def.build_mcu();
        assert!(matches!(mcu.clock, ClockConfig::None), "no tree yet");
        assert!(
            mcu.clock_manual,
            "and its clock block is hand-written, since nothing generates it"
        );
    }

    /// …but give that same chip a tree, and its clock is generated after all —
    /// so it must NOT arrive fenced off as hand-written.
    ///
    /// This was the bug: `clock_manual` was decided by the FAMILY in `Mcu::new`,
    /// before `build_mcu` had installed the tree. An H5 with a full clock tree
    /// therefore opened in manual mode, `keep_manual_clock` preserved whatever
    /// `main.rs` already had, and the Clock tab drove nothing for the life of the
    /// project.
    #[test]
    fn a_tree_takes_the_chip_out_of_hand_written_mode() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, minimal_graph};

        let mut def = stm_def();
        def.family = "stm32h5".into();
        def.clock = ClockDef::Graph(GraphClock {
            graph: minimal_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        });

        let mcu = def.build_mcu();
        assert!(
            !mcu.clock_manual,
            "the tree generates the block, so the IDE keeps writing it"
        );

        // And the block really does follow the tree.
        let block = crate::panels::mcu_module::codegen::rcc::graph_clock_block(
            &mcu.family,
            &mcu.clock,
            mcu.clock_manual,
        );
        assert!(block.contains("embassy_stm32::init"), "{block}");
        assert!(
            !block.contains("has no generated RCC recipe yet"),
            "{block}"
        );
    }

    /// The whole point of `@clocknodes`: a NON-F1 chip's clock survives being
    /// saved and reopened.
    ///
    /// Until this existed the save path was gated on `family == "stm32f1"`, so
    /// retuning an H5 and closing the project lost the lot — the tree came back
    /// at the chip definition's defaults with nothing recording that it had ever
    /// changed.
    #[test]
    fn a_non_f1_clock_survives_a_save_and_reopen() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, minimal_graph, model::NodeState,
        };

        let mut def = stm_def();
        def.family = "stm32h5".into();
        def.clock = ClockDef::Graph(GraphClock {
            graph: minimal_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        });

        // Open the project and retune the clock: PLL off HSE, x100, SYSCLK on it.
        let mut mcu = def.build_mcu();
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("the definition carries a graph");
        };
        gc.graph.node_mut("pllsrc").unwrap().state = NodeState::Index(1);
        gc.graph.node_mut("plln").unwrap().state = NodeState::Value(100);
        gc.graph.node_mut("sw").unwrap().state = NodeState::Index(2);
        let tuned = gc.graph.clone();

        let text = mcu.mcu_config_text();
        assert!(
            text.contains("@clocknodes"),
            "the section is written: {text}"
        );
        assert!(
            !text.contains(
                "@clock
"
            ),
            "and NOT the F1 one, which cannot describe this tree: {text}"
        );

        // Reopen: a fresh chip from the same definition, plus the saved file.
        let mut reopened = def.build_mcu();
        reopened.apply_mcu_config(&text);
        let ClockConfig::Graph(back) = &reopened.clock else {
            panic!("still a graph");
        };
        assert_eq!(back.graph.nodes, tuned.nodes, "the tuning came back");

        // And an untouched project leaves the file alone.
        let untouched = def.build_mcu().mcu_config_text();
        assert!(
            !untouched.contains("@clocknodes"),
            "a default clock writes nothing: {untouched}"
        );
    }

    /// A declared clock is never overridden by the family fallback.
    #[test]
    fn a_declared_clock_wins_over_the_family_fallback() {
        let mut def = stm_def();
        def.family = "esp32c3".into(); // family says ESP…
        def.clock = ClockDef::Stm32f1(Stm32f1Clock::default()); // …the file says F1
        assert!(matches!(def.effective_clock(), ClockDef::Stm32f1(_)));
    }

    /// A saved project's clock is adopted AFTER the snapshot, so Reset targets
    /// the chip default — not whatever the project was opened with.
    #[test]
    fn defaults_survive_a_restored_project_clock() {
        use crate::panels::mcu_module::clock::graph::{NodeState, stm32f1_graph};

        let mut mcu = stm_def().build_mcu();
        let pristine = mcu.clock.clone();
        mcu.apply_saved_clock(Stm32f1Clock {
            pll_mul: 4,
            ..Stm32f1Clock::default()
        });
        assert!(
            !mcu.clock_is_default(),
            "restored config differs from factory"
        );

        assert!(mcu.reset_clock());
        assert_eq!(mcu.clock, pristine);

        // Sanity: the snapshot really is the definition's tree, not a clone of
        // the saved one.
        let def_graph = stm32f1_graph(&Stm32f1Clock::default());
        let saved = mcu.clock_defaults.as_ref().unwrap();
        assert!(saved.states_match(&def_graph));
        assert_ne!(
            saved.node("pllmul").map(|n| n.state.clone()),
            Some(NodeState::Value(4))
        );
    }
}

/// The project definition a chip builds under `mcu`'s runtime.
///
/// The pairing of a chip definition with the runtime that is actually selected,
/// in ONE place. It existed only inside `AppIde::selected_build_cfg` and inside
/// each cross-compile harness separately, which is how a Pico on Async came to
/// generate `embassy_rp::init(...)` against a manifest naming `rp2040-hal`:
/// every harness called [`ProjectDef::for_async`] by hand, and the application
/// called it nowhere. A free function so a test can ask the same question the
/// app asks, without standing up an `AppIde`.
pub fn build_cfg(
    def: &McuDefinition,
    mcu: Option<&crate::panels::mcu_module::mcu::Mcu>,
) -> ProjectDef {
    let is_async = mcu.is_some_and(|m| m.is_async());
    def.project.for_async(is_async).into_owned()
}

impl ProjectDef {
    /// This definition as it applies to `runtime` — the same thing, except that
    /// a family which swaps HAL crates gets the other line.
    ///
    /// An STM32F1 on `stm32f1xx-hal` with no `hal_dep_async` of its own gets its
    /// embassy-stm32 line derived from the part number, so the built-in and
    /// every F1 imported before Async existed build on it without re-import.
    /// An explicit `hal_dep_async` still wins. An F1 whose part number cannot
    /// be read still gets an embassy-stm32 line, with the chip feature left to
    /// fill in.
    ///
    /// Off Async the definition is itself, with one exception: an F1 saved
    /// with the old comment-only blocking line (`# TODO: set the stm32f1xx-hal
    /// device feature for …`) gets a real `stm32f1xx-hal` line instead. A
    /// comment names no crate, so a manifest that had swapped to embassy-stm32
    /// on Async could never swap back.
    pub fn for_async(&self, is_async: bool) -> std::borrow::Cow<'_, ProjectDef> {
        use crate::panels::mcu_module::stm32_pin_data::{
            f1_blocking_hal_dep_todo, f1_embassy_hal_dep, f1_embassy_hal_dep_todo,
        };
        let name = || {
            [&self.probe_chip, &self.pkg_name]
                .into_iter()
                .find(|n| !n.trim().is_empty())
                .map_or("this part", |n| n.as_str())
        };
        let line = if is_async {
            self.hal_dep_async.clone().or_else(|| {
                self.hal_dep.contains("stm32f1xx-hal").then(|| {
                    f1_embassy_hal_dep(&self.probe_chip, &self.pkg_name)
                        .unwrap_or_else(|| f1_embassy_hal_dep_todo(name()))
                })
            })
        } else {
            (self.hal_dep.trim_start().starts_with('#') && self.hal_dep.contains("stm32f1xx-hal"))
                .then(|| f1_blocking_hal_dep_todo(name()))
        };
        match line {
            Some(line) => {
                let mut out = self.clone();
                out.hal_dep = line;
                std::borrow::Cow::Owned(out)
            }
            None => std::borrow::Cow::Borrowed(self),
        }
    }
}

#[cfg(test)]
mod the_manifest_follows_the_runtime {
    use super::build_cfg;
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::project_gen;

    /// A chip that swaps HAL crates on Async must get the other crate in its
    /// Cargo.toml, on the path the APPLICATION takes.
    ///
    /// This is the test that was missing. `emit_rp_async_project` and friends
    /// each called `for_async(true)` by hand before generating, so all of them
    /// passed while a Pico saved from the app got `main.rs` full of
    /// `embassy_rp::` and a manifest naming `rp2040-hal` - `E0433: use of
    /// undeclared crate or module embassy_rp` on the first build.
    #[test]
    fn an_async_chip_gets_its_async_hal_crate() {
        let mut checked = 0;
        for d in builtin_definitions() {
            let Some(async_line) = d.project.hal_dep_async.clone() else {
                continue;
            };
            // The crate name is the first word of the dependency line.
            let krate = async_line
                .split_whitespace()
                .next()
                .expect("a dependency line names a crate");
            let blocking = d.project.hal_dep.split_whitespace().next().unwrap_or("");
            assert_ne!(krate, blocking, "{}: the two lines differ", d.id);

            let mut mcu = d.build_mcu();
            mcu.runtime = Runtime::Async;
            assert!(mcu.is_async(), "{}: async is supported here", d.id);

            let cfg = build_cfg(&d, Some(&mcu));
            let toml =
                project_gen::gen_config(project_gen::ConfigFile::CargoToml, &cfg, &d.toolchain);
            assert!(
                toml.contains(krate),
                "{}: Async must name {krate}, got:\n{toml}",
                d.id
            );

            // ...and Blocking keeps the other one, so this is a swap and not an
            // addition.
            let mut blocking_mcu = d.build_mcu();
            blocking_mcu.runtime = Runtime::Blocking;
            let bcfg = build_cfg(&d, Some(&blocking_mcu));
            let btoml =
                project_gen::gen_config(project_gen::ConfigFile::CargoToml, &bcfg, &d.toolchain);
            assert!(
                btoml.contains(blocking),
                "{}: Blocking must name {blocking}, got:\n{btoml}",
                d.id
            );
            checked += 1;
        }
        assert!(
            checked >= 5,
            "the four RP boards and the micro:bit at least, got {checked}"
        );
    }

    /// And the async line carries the feature embassy-time needs, or the project
    /// links to nothing: `undefined symbol: _embassy_time_now`. embassy-rp
    /// spells it `time-driver`, embassy-nrf names the RTC it drives it from.
    #[test]
    fn an_embassy_async_line_enables_the_time_driver() {
        let mut checked = 0;
        for d in builtin_definitions() {
            let Some(line) = &d.project.hal_dep_async else {
                continue;
            };
            if !line.starts_with("embassy-rp") && !line.starts_with("embassy-nrf") {
                continue;
            }
            assert!(line.contains("\"time-driver"), "{}: {line}", d.id);
            checked += 1;
        }
        assert!(
            checked >= 5,
            "the RP boards and the micro:bit, got {checked}"
        );
    }

    /// Without `gpiote`, embassy-nrf has no `Input::wait_for_*`, which is what an
    /// armed input's task awaits.
    #[test]
    fn the_nrf_async_line_enables_gpiote() {
        let mut checked = 0;
        for d in builtin_definitions() {
            let Some(line) = &d.project.hal_dep_async else {
                continue;
            };
            if line.starts_with("embassy-nrf") {
                assert!(line.contains("\"gpiote\""), "{}: {line}", d.id);
                checked += 1;
            }
        }
        assert!(checked >= 1, "the micro:bit at least");
    }

    /// An F1 whose part number cannot be read still leaves stm32f1xx-hal on
    /// Async: its line becomes an embassy-stm32 one with the feature to fill in,
    /// never the blocking HAL under embassy code - and it comes back again.
    #[test]
    fn an_unreadable_f1_still_swaps_to_embassy() {
        let mut def = builtin_definitions()
            .into_iter()
            .find(|d| d.family == "stm32f1")
            .expect("the built-in F103");
        def.project.probe_chip = "STM32F103xB".into();
        def.project.pkg_name = String::new();
        let line = def.project.for_async(true).hal_dep.clone();
        assert!(line.starts_with("embassy-stm32 = {"), "{line}");
        assert!(
            line.contains("TODO: the chip feature for STM32F103xB"),
            "{line}"
        );
        // The blocking HAL's old comment-only TODO line counts as an F1 too -
        // and off Async it becomes a real line, so the manifest can swap BACK.
        def.project.hal_dep = "# TODO: set the stm32f1xx-hal device feature for X".into();
        let line = def.project.for_async(true).hal_dep.clone();
        assert!(line.starts_with("embassy-stm32 = {"), "{line}");
        let back = def.project.for_async(false).hal_dep.clone();
        assert!(back.starts_with("stm32f1xx-hal = {"), "{back}");

        let tc = def.toolchain.clone();
        let blocking = def.project.for_async(false).into_owned();
        let asynchronous = def.project.for_async(true).into_owned();
        let toml = project_gen::gen_config(project_gen::ConfigFile::CargoToml, &blocking, &tc);
        let there = project_gen::refresh_hal_dependency(&toml, &asynchronous, &tc);
        assert!(there.contains("embassy-stm32 = {"), "{there}");
        let home = project_gen::refresh_hal_dependency(&there, &blocking, &tc);
        assert!(home.contains("stm32f1xx-hal = {"), "{home}");
        assert!(!home.contains("embassy-stm32 = {"), "{home}");

        // A real blocking line is left exactly as it is.
        def.project.hal_dep = "stm32f1xx-hal = { version = \"0.10\" }".into();
        assert_eq!(def.project.for_async(false).hal_dep, def.project.hal_dep);
    }
}
