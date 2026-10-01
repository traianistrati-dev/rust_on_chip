//! Editable form model for authoring a new [`McuDefinition`] in the UI.
//!
//! This is the PURE half (no egui): the field buffers, validation, and the
//! `McuForm ⇄ McuDefinition` conversions. The dialog in `app::mcu_form_dialog`
//! renders it and, on Save, writes the RON to the user `mcus/` folder and
//! merges it into the live registry — the same path an imported `.ron` takes,
//! so a form-authored chip is indistinguishable from an imported one.
//!
//! Scope: a chip in an ALREADY-SUPPORTED family (STM32F1 / ESP32-C3) is pure
//! data and fully authorable here. A brand-new family still needs a codegen
//! `FamilyBackend` in code — the form warns when the family is unknown.

use serde::{Deserialize, Serialize};

use super::mcu_catalog::ToolchainKind;
use super::mcu_def::{ClockDef, McuDefinition, PinDef, PinLayout, ProjectDef};
use super::pins::logic::pin_function::PinFunction;

/// The four sides. This is the STORAGE order of `McuForm::pins` (and of
/// `PinLayout` in the definition) — do not reorder it.
pub const SIDES: [&str; 4] = ["Top", "Bottom", "Left", "Right"];

/// The order the GUI presents the sides in: **Left → Bottom → Right → Top**.
/// That mirrors QFP/QFN numbering (pin 1 sits at the top of the left side and
/// the count runs counter-clockwise), so reading the editors top-to-bottom
/// follows the pin numbers — the same walk [`crate::panels::mcu_module::stm32_pin_data`]
/// uses when it distributes an imported pinout. Values are indices into
/// [`SIDES`] / `McuForm::pins`; the storage order above is unchanged.
pub const SIDE_DISPLAY_ORDER: [usize; 4] = [2, 1, 3, 0];

/// One editable pin row (data form of [`PinDef`]). Numbers and functions are
/// STRINGS so a half-typed value never snaps back: the number stays as typed,
/// and functions are a space/comma list of tokens ([`parse_functions`]) — far
/// lighter to edit than a per-pin multi-select of a dozen parameterized
/// variants. `in out usart1_tx spi2_sck i2c1_scl adc1_5 tim2_1 swdio` etc.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PinRow {
    pub number: String,
    pub name: String,
    pub reserved: bool,
    pub functions: String,
    /// UI-only provenance flag: `true` for a row that an AI datasheet import
    /// created, so the pin editor can tag it as "review me". Never persisted
    /// (dropped by [`McuForm::to_definition`]).
    pub imported: bool,
    /// `(signal, alternate-function index)` captured from the vendor GPIO IP
    /// file. Carried through the form untouched — it is data ABOUT the chip, not
    /// something to author by hand — and written to `PinDef::af`.
    pub af: Vec<(String, u8)>,
    /// `(function token, GPIO)` for tokens contributed by a GPIO bonded to the
    /// same package pin - see
    /// [`PinDef::fn_owner`](crate::panels::mcu_module::mcu_def::PinDef::fn_owner).
    /// Carried through the form untouched, like [`af`](Self::af): it is data
    /// about the package, not something to author by hand.
    pub fn_owner: Vec<(String, String)>,
}

/// The clock model offered by the form. A full graph editor is out of scope,
/// so the choices are the built-in family models plus "none"; importing a
/// `.ron` remains the way to carry a hand-authored [`ClockDef::Graph`] (the
/// form PRESERVES such a graph — see [`McuForm::imported_clock`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClockChoice {
    None,
    Stm32f1,
    Esp32c3,
    /// STM32WBA tree (data-driven graph — ships the 100 MHz PLL preset).
    Stm32wba,
    /// STM32F4 tree (data-driven graph — ships the 100 MHz HSI→PLL preset).
    Stm32f4,
    /// STM32F2 tree. The same topology as [`ClockChoice::Stm32f4`] (one embassy
    /// RCC module covers F2/F4/F7), kept SEPARATE because the PLLN window is
    /// not the same: F2's PAC has `MUL192..=MUL432` where F4 has `MUL2..`, so an
    /// N the F4 accepts can name a variant the F2 does not have. Also its own
    /// ceilings — 120 MHz HCLK, 30/60 MHz APB — versus F4's 100/50/100.
    Stm32f2,
    /// STM32G4 tree (data-driven graph — ships the 150 MHz HSI→PLL preset). No
    /// hand-authored layout: the diagram is auto-generated from the topology.
    Stm32g4,
    /// STM32G0 tree (data-driven graph — ships the 64 MHz HSI→PLL preset,
    /// single APB bus). Auto-generated layout.
    Stm32g0,
    /// STM32L4 tree (data-driven graph — ships the 80 MHz HSI→PLL preset; MSI
    /// shown but HSI-PLL codegen). Auto-generated layout.
    Stm32l4,
}

impl ClockChoice {
    pub const ALL: [ClockChoice; 8] = [
        ClockChoice::None,
        ClockChoice::Stm32f1,
        ClockChoice::Esp32c3,
        ClockChoice::Stm32wba,
        ClockChoice::Stm32f4,
        ClockChoice::Stm32g4,
        ClockChoice::Stm32g0,
        ClockChoice::Stm32l4,
    ];
    /// The clock tree a chip FAMILY defaults to — so an imported chip (XML or
    /// AI datasheet) whose family has a modelled tree gets a working Clock tab
    /// and real RCC codegen without the user picking one by hand. `None` for
    /// families with no tree yet (the reset-default clock still compiles).
    pub fn for_family(family: &str) -> ClockChoice {
        match family {
            "stm32f1" => ClockChoice::Stm32f1,
            "stm32wba" => ClockChoice::Stm32wba,
            // F2/F4/F7 share embassy's f247 RCC, but NOT the PLLN window or the
            // clock ceilings — see `ClockChoice::Stm32f2`.
            "stm32f2" => ClockChoice::Stm32f2,
            "stm32f4" | "stm32f7" => ClockChoice::Stm32f4,
            "stm32g4" => ClockChoice::Stm32g4,
            "stm32g0" => ClockChoice::Stm32g0,
            "stm32l4" => ClockChoice::Stm32l4,
            "esp32c3" => ClockChoice::Esp32c3,
            _ => ClockChoice::None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ClockChoice::None => "None",
            ClockChoice::Stm32f1 => "STM32F1 tree",
            ClockChoice::Esp32c3 => "ESP32-C3 tree",
            ClockChoice::Stm32wba => "STM32WBA tree",
            ClockChoice::Stm32f4 => "STM32F4/F7 tree",
            ClockChoice::Stm32f2 => "STM32F2 tree",
            ClockChoice::Stm32g4 => "STM32G4 tree",
            ClockChoice::Stm32g0 => "STM32G0 tree",
            ClockChoice::Stm32l4 => "STM32L4 tree",
        }
    }
    /// Public so a definition can fall back to its family's tree when it
    /// declares no clock of its own (see `McuDefinition::effective_clock`).
    pub fn to_def(self) -> ClockDef {
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, stm32f2_graph, stm32f2_layout, stm32f4_graph, stm32f4_layout,
            stm32g0_graph, stm32g4_graph, stm32l4_graph, stm32wba_graph, stm32wba_layout,
        };
        match self {
            ClockChoice::None => ClockDef::None,
            ClockChoice::Stm32f1 => ClockDef::Stm32f1(Default::default()),
            ClockChoice::Esp32c3 => ClockDef::Esp32c3,
            ClockChoice::Stm32wba => ClockDef::Graph(GraphClock {
                graph: stm32wba_graph(),
                layout: stm32wba_layout(),
                bindings: Default::default(),
            }),
            ClockChoice::Stm32f4 => ClockDef::Graph(GraphClock {
                graph: stm32f4_graph(),
                layout: stm32f4_layout(),
                bindings: Default::default(),
            }),
            ClockChoice::Stm32f2 => ClockDef::Graph(GraphClock {
                graph: stm32f2_graph(),
                layout: stm32f2_layout(),
                bindings: Default::default(),
            }),
            // Empty layout on purpose — `auto_layout` draws the diagram from the
            // graph topology, so a new family needs no hand-tuned positions.
            ClockChoice::Stm32g4 => ClockDef::Graph(GraphClock {
                graph: stm32g4_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
            ClockChoice::Stm32g0 => ClockDef::Graph(GraphClock {
                graph: stm32g0_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
            ClockChoice::Stm32l4 => ClockDef::Graph(GraphClock {
                graph: stm32l4_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
        }
    }
    fn from_def(d: &ClockDef) -> ClockChoice {
        use crate::panels::mcu_module::clock::graph::{
            is_f4_graph, is_g0_graph, is_g4_graph, is_l4_graph, is_wba_graph,
        };
        match d {
            ClockDef::Stm32f1(_) => ClockChoice::Stm32f1,
            ClockDef::Esp32c3 => ClockChoice::Esp32c3,
            ClockDef::Graph(gc) if is_wba_graph(&gc.graph) => ClockChoice::Stm32wba,
            ClockDef::Graph(gc) if is_f4_graph(&gc.graph) => ClockChoice::Stm32f4,
            ClockDef::Graph(gc) if is_g4_graph(&gc.graph) => ClockChoice::Stm32g4,
            ClockDef::Graph(gc) if is_g0_graph(&gc.graph) => ClockChoice::Stm32g0,
            ClockDef::Graph(gc) if is_l4_graph(&gc.graph) => ClockChoice::Stm32l4,
            // A foreign graph maps to None here but is PRESERVED via
            // `McuForm::imported_clock`; plain none stays none.
            ClockDef::Graph(_) | ClockDef::None => ClockChoice::None,
        }
    }
}

/// All editable fields of a new / cloned MCU definition.
#[derive(Clone, Debug, PartialEq)]
pub struct McuForm {
    // Identity
    pub id: String,
    pub display_name: String,
    pub family: String,
    pub cpu: String,
    pub package: String,
    /// Datasheet maximum core frequency in MHz. `None` when the vendor file
    /// states none — shown as nothing, never as a family guess.
    pub max_mhz: Option<u32>,
    /// The chip on this board, carried through untouched like `dma`: the form
    /// has no field for it, and dropping it turns a board back into a bare part.
    pub board_chip: Option<String>,
    /// On-die RAM in KiB, carried through like `dma` for as long as the RAM
    /// size below is the one it was loaded with — see [`Self::sram_kb_kept`].
    ///
    /// Set together with `loaded_ram_size`, or not at all: a value whose
    /// `loaded_ram_size` does not match `ram_size` is dropped on save.
    pub sram_kb: Option<u32>,
    /// `ram_size` as the definition had it, which is what `sram_kb` describes.
    pub loaded_ram_size: String,
    /// The chip's DMA channels, carried through untouched: imported from the
    /// vendor database, not authorable here (see [`super::mcu_def::DmaDef`]).
    /// Editing a chip in this form must not silently drop them.
    pub dma: Option<super::mcu_def::DmaDef>,
    /// The chip's interrupt vectors, carried through untouched like `dma`.
    pub irq_vectors: Vec<String>,
    /// The chip's USART IP version, carried verbatim from the vendor data —
    /// the IDE never edits it, it only decides whether swap/invert are offered.
    pub usart_ip: Option<String>,
    pub sdmmc_ip: Option<String>,
    // Toolchain + target
    pub toolchain: ToolchainKind,
    pub target: String,
    // Memory (RustEmbedded only — ESP owns its layout)
    pub flash_origin: String,
    pub flash_size: String,
    pub ram_origin: String,
    pub ram_size: String,
    pub memory_comment: String,
    // Probe / flash + dependency line
    pub probe_chip: String,
    pub hal_dep: String,
    /// The dependency line an Async project gets INSTEAD of `hal_dep`, for a
    /// family that swaps HAL crates with the runtime (RP, nRF). Empty for a
    /// chip that keeps one crate. Shown in the form, because a line that
    /// replaces the visible one must not be invisible itself.
    pub hal_dep_async: String,
    // Clock model
    pub clock: ClockChoice,
    /// A hand-imported [`ClockDef::Graph`] the form cannot re-author: carried
    /// through Edit → Save verbatim while the choice stays `None`, so editing
    /// an imported chip never silently drops its clock tree.
    pub imported_clock: Option<ClockDef>,
    /// Where `imported_clock` came from: `true` when [`Self::from_definition`]
    /// carried it in with the chip the form was opened on, `false` when
    /// [`Self::set_imported_clock`] attached it to THIS form. Only the first
    /// kind goes stale when Auto-fill moves the form to another family. Read
    /// only while `imported_clock` is `Some`.
    pub clock_carried_in: bool,
    // Pins, per side
    pub pins: [Vec<PinRow>; 4],
    /// A ball grid (WLCSP / BGA) the form cannot re-author yet: carried through
    /// Edit -> Save verbatim, for the same reason as `imported_clock`. Editing a
    /// grid chip must not silently drop its balls.
    pub grid: Option<crate::panels::mcu_module::mcu_def::PinGridDef>,
    /// True when the form was opened to EDIT/clone an existing chip (so the
    /// dialog can warn before an id collision silently overrides a built-in).
    pub editing: bool,
}

impl Default for McuForm {
    fn default() -> Self {
        Self::blank()
    }
}

impl McuForm {
    /// What the New MCU form opens with: nothing.
    ///
    /// It used to open on [`Self::blank`], and every one of those STM32F1
    /// values is VALID, so none of them was ever flagged: an nRF52840 typed
    /// into it kept Cortex-M3, `thumbv7m-none-eabi`, flash at `0x08000000` and
    /// the F1 clock tree, and saved without an error. Empty fields are ones
    /// [`Self::errors`] asks for.
    ///
    /// The toolchain is the one field that cannot be empty; it stays on the
    /// ARM one, which is what decides that the memory fields are required.
    ///
    /// Every field is written out rather than spread from [`Self::blank`]: a
    /// field added there with an STM32 value would otherwise reach this form
    /// too, silently, which is the shape of the bug this all started from.
    pub fn empty() -> Self {
        Self {
            id: String::new(),
            display_name: String::new(),
            family: String::new(),
            cpu: String::new(),
            package: String::new(),
            max_mhz: None,
            board_chip: None,
            sram_kb: None,
            loaded_ram_size: String::new(),
            dma: None,
            irq_vectors: Vec::new(),
            usart_ip: None,
            sdmmc_ip: None,
            toolchain: ToolchainKind::RustEmbedded,
            target: String::new(),
            flash_origin: String::new(),
            flash_size: String::new(),
            ram_origin: String::new(),
            ram_size: String::new(),
            memory_comment: String::new(),
            probe_chip: String::new(),
            hal_dep: String::new(),
            hal_dep_async: String::new(),
            clock: ClockChoice::None,
            imported_clock: None,
            clock_carried_in: false,
            pins: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            grid: None,
            editing: false,
        }
    }

    /// An STM32F1-flavored form: the base the importers and the tests fill in.
    /// Not what the dialog opens with — see [`Self::empty`].
    pub fn blank() -> Self {
        Self {
            grid: None,
            id: String::new(),
            display_name: String::new(),
            family: "stm32f1".into(),
            cpu: "Cortex-M3".into(),
            package: String::new(),
            max_mhz: None,
            board_chip: None,
            sram_kb: None,
            loaded_ram_size: String::new(),
            dma: None,
            irq_vectors: Vec::new(),
            usart_ip: None,
            sdmmc_ip: None,
            toolchain: ToolchainKind::RustEmbedded,
            target: "thumbv7m-none-eabi".into(),
            flash_origin: "0x08000000".into(),
            flash_size: "64K".into(),
            ram_origin: "0x20000000".into(),
            ram_size: "20K".into(),
            memory_comment: String::new(),
            probe_chip: String::new(),
            hal_dep: "stm32f1xx-hal = { version = \"0.10\", features = [\"rt\"] }".into(),
            hal_dep_async: String::new(),
            clock: ClockChoice::Stm32f1,
            imported_clock: None,
            clock_carried_in: false,
            pins: [Vec::new(), Vec::new(), Vec::new(), Vec::new()],
            editing: false,
        }
    }

    /// Fill family / CPU / toolchain / target deterministically from the chip
    /// NAME (display_name, else id) — the "Auto-fill from name" button. Also
    /// seeds probe_chip when empty. No-op for a non-STM32 / unrecognised name;
    /// returns true when it recognised the name. See [`super::mcu_identity`].
    pub fn auto_fill_identity(&mut self) -> bool {
        let name = if !self.display_name.trim().is_empty() {
            self.display_name.trim().to_string()
        } else {
            self.id.trim().to_string()
        };
        if let Some(c) = nrf52_in_name(&name) {
            self.auto_fill_nrf(c);
            return true;
        }
        let Some((family, cpu, toolchain, target)) = super::mcu_identity::identity_from_name(&name)
        else {
            return false;
        };
        // Whether this form is being moved to ANOTHER family decides what else
        // below is stale. A re-fill on the same family changes nothing but the
        // derived fields, so a value typed into this chip survives it.
        //
        // Both sides go through the SAME derivation. A chip imported from the
        // vendor data carries the vendor's family key, and that can be finer
        // than a name yields: `stm32l4+` and `stm32wb0` are both real keys,
        // and compared raw against `stm32l4` / `stm32wb` an unrenamed L4+ part
        // read as a move - and lost its own clock tree to the L4 template.
        let new_family = super::mcu_identity::family_from_name(self.family.trim()).as_deref()
            != Some(family.as_str());
        self.family = family;
        self.cpu = cpu.to_string();
        self.toolchain = toolchain;
        self.target = target.to_string();
        // The HAL/PAC dependency line, too — same derivation the XML importer
        // uses. Without this, "Auto-fill" (and the AI import that calls it) left
        // a non-F1 STM32 on the blank form's `stm32f1xx-hal` default, so the
        // generated project wouldn't compile until the line was hand-edited.
        self.hal_dep = super::stm32_pin_data::hal_dep_for_name(&self.family, &name);
        // Only STM32 names are recognized, and an STM32 keeps one crate for
        // both runtimes. A line left over from the chip this form was cloned
        // from would replace the one just written in every Async project.
        self.hal_dep_async.clear();
        // The memory map and the clock tree are per-FAMILY, so they follow the
        // family across. Cloning a board and renaming it is how a form ends up
        // describing another part, and it arrives carrying the old family's
        // answers: a micro:bit cloned to an STM32 kept flash at `0x00000000`
        // and an nRF clock graph, under `family = stm32f4`.
        //
        // Only when the family actually moved. A re-fill on the same family
        // leaves a bootloader offset, or a clock tree imported from CubeMX,
        // exactly where the user put it.
        if new_family {
            // Every STM32 maps flash and SRAM at these two — the same pair the
            // XML importer writes for all ~2800 parts it knows.
            self.flash_origin = "0x08000000".into();
            self.ram_origin = "0x20000000".into();
            // A graph that came in with the old chip describes clocks this one
            // does not have. One ATTACHED to this form is the user's answer for
            // this chip, and is never dropped: the AI import attaches its tree
            // from a request that can land before the one that gets here, and
            // dropping it made the result depend on which reply came first.
            if self.clock_carried_in {
                self.imported_clock = None;
                self.clock_carried_in = false;
            }
            // The dropdown follows the family - unless an attached graph is
            // what is in effect, which a family template would shadow.
            let attached_in_effect =
                self.imported_clock.is_some() && self.clock == ClockChoice::None;
            if !attached_in_effect {
                self.clock = ClockChoice::for_family(&self.family);
            }
        }
        if self.probe_chip.trim().is_empty() {
            self.probe_chip = name;
        }
        true
    }

    /// Auto-fill for an nRF52 part: every field the built-in kits carry, from
    /// the same table and the same methods `nrf_boards::apply_chip` uses.
    ///
    /// The clock is the tree WITHOUT the 32.768 kHz crystal: on a board that
    /// has none, an LFXO choice generates a `start_lfclk` that never returns,
    /// while the RC works on every board. A board with the crystal can import
    /// a tree that has it.
    fn auto_fill_nrf(&mut self, c: &'static crate::panels::mcu_module::codegen::nrf::NrfChip) {
        let new_family = self.family.trim() != c.family;
        self.family = c.family.into();
        self.cpu = c.cpu().into();
        self.toolchain = ToolchainKind::RustEmbedded;
        self.target = c.target().into();
        self.max_mhz = Some(64);
        self.flash_origin = "0x00000000".into();
        self.flash_size = format!("{}K", c.flash_kb);
        self.ram_origin = "0x20000000".into();
        self.ram_size = format!("{}K", c.ram_kb);
        self.sram_kb = Some(c.ram_kb);
        self.loaded_ram_size = self.ram_size.clone();
        self.hal_dep = c.hal_dep();
        self.hal_dep_async = c.hal_dep_async();
        if self.probe_chip.trim().is_empty() || new_family {
            self.probe_chip = c.probe_chip();
        }
        // Only on a move, for the reason `auto_fill_identity` gives: a tree
        // the user attached to this nRF form is their answer for it.
        if new_family {
            self.imported_clock = Some(ClockDef::Graph(
                crate::panels::mcu_module::codegen::nrf_boards::clock_graph(false),
            ));
            self.clock_carried_in = true;
            self.clock = ClockChoice::None;
        }
    }

    /// Move pin `idx` from side `from` to the END of side `to`, keeping the row
    /// intact. The pin NUMBER is untouched — which side a pin is drawn on is
    /// layout, not identity. `false` (no-op) for a same-side move or any
    /// out-of-range index/side.
    pub fn move_pin(&mut self, from: usize, idx: usize, to: usize) -> bool {
        if from == to || from >= self.pins.len() || to >= self.pins.len() {
            return false;
        }
        if idx >= self.pins[from].len() {
            return false;
        }
        let row = self.pins[from].remove(idx);
        self.pins[to].push(row);
        true
    }

    /// Move pin `idx` by `delta` positions within its own side (−1 = earlier,
    /// +1 = later). Order along a side IS the physical position, so this is how
    /// a pin gets placed after being moved across. `false` (no-op) at the ends.
    pub fn reorder_pin(&mut self, side: usize, idx: usize, delta: isize) -> bool {
        let Some(rows) = self.pins.get_mut(side) else {
            return false;
        };
        if idx >= rows.len() {
            return false;
        }
        let target = idx as isize + delta;
        if target < 0 || target as usize >= rows.len() {
            return false;
        }
        let row = rows.remove(idx);
        rows.insert(target as usize, row);
        true
    }

    /// Seed the form from an existing definition (the "Clone / Edit" path).
    pub fn from_definition(def: &McuDefinition) -> Self {
        let side = |ds: &[PinDef]| -> Vec<PinRow> {
            ds.iter()
                .map(|d| PinRow {
                    number: d.number.to_string(),
                    name: d.name.clone(),
                    reserved: d.reserved,
                    functions: functions_to_string(&d.functions),
                    imported: false,
                    af: d.af.clone(),
                    fn_owner: Vec::new(),
                })
                .collect()
        };
        Self {
            grid: def.pins.grid.clone(),
            id: def.id.clone(),
            display_name: def.display_name.clone(),
            family: def.family.clone(),
            cpu: def.cpu.clone(),
            package: def.package.clone(),
            max_mhz: def.max_mhz,
            board_chip: def.board_chip.clone(),
            sram_kb: def.sram_kb,
            loaded_ram_size: def.project.ram_size.clone(),
            dma: def.dma.clone(),
            irq_vectors: def.irq_vectors.clone(),
            usart_ip: def.usart_ip.clone(),
            sdmmc_ip: def.sdmmc_ip.clone(),
            toolchain: def.toolchain.clone(),
            target: def.project.target.clone(),
            flash_origin: def.project.flash_origin.clone(),
            flash_size: def.project.flash_size.clone(),
            ram_origin: def.project.ram_origin.clone(),
            ram_size: def.project.ram_size.clone(),
            memory_comment: def.project.memory_comment.clone(),
            probe_chip: def.project.probe_chip.clone(),
            hal_dep: def.project.hal_dep.clone(),
            hal_dep_async: def.project.hal_dep_async.clone().unwrap_or_default(),
            clock: ClockChoice::from_def(&def.clock),
            imported_clock: match (&def.clock, ClockChoice::from_def(&def.clock)) {
                // A graph the form can't re-author (not the WBA one).
                (ClockDef::Graph(_), ClockChoice::None) => Some(def.clock.clone()),
                _ => None,
            },
            clock_carried_in: true,
            pins: [
                side(&def.pins.top),
                side(&def.pins.bottom),
                side(&def.pins.left),
                side(&def.pins.right),
            ],
            editing: true,
        }
    }

    /// Collected blocking errors — empty means [`to_definition`] will succeed.
    /// Order matches the form top-to-bottom so the first message points at the
    /// first offending field.
    pub fn errors(&self) -> Vec<String> {
        let mut e = Vec::new();
        if !is_valid_id(&self.id) {
            e.push(
                "Id must be non-empty and use only a–z, 0–9 and _ (it becomes the \
                 file name and the registry key)."
                    .into(),
            );
        }
        if self.display_name.trim().is_empty() {
            e.push("Display name is required.".into());
        }
        if self.family.trim().is_empty() {
            e.push("Family is required (selects the codegen + clock backend).".into());
        }
        if self.target.trim().is_empty() {
            e.push("Target triple is required (e.g. thumbv7m-none-eabi).".into());
        }
        // Memory + probe only matter for the ARM / probe-rs toolchain; ESP owns
        // its own memory layout and flashes over serial.
        if self.toolchain == ToolchainKind::RustEmbedded {
            if self.probe_chip.trim().is_empty() {
                e.push("Probe chip is required for the ARM toolchain (used by probe-rs).".into());
            }
            for (label, v) in [
                ("Flash origin", &self.flash_origin),
                ("Flash size", &self.flash_size),
                ("RAM origin", &self.ram_origin),
                ("RAM size", &self.ram_size),
            ] {
                // An empty field is missing, not malformed: `('')` is not a
                // value anyone typed.
                if v.trim().is_empty() {
                    e.push(format!(
                        "{label} is required — hex (0x…), decimal, or a K/M suffix (e.g. 64K)."
                    ));
                } else if parse_ld_number(v).is_none() {
                    e.push(format!(
                        "{label} ('{v}') is not a valid value — use hex (0x…), \
                         decimal, or a K/M suffix (e.g. 64K)."
                    ));
                }
            }
        }
        // Pin numbers, when present, must be positive and unique across sides;
        // every function token must be recognised.
        let mut seen = std::collections::HashSet::new();
        for row in self.pins.iter().flatten() {
            let n = row.number.trim();
            if n.is_empty() && row.name.trim().is_empty() && row.functions.trim().is_empty() {
                continue; // a wholly blank scratch row is ignored, not an error
            }
            match n.parse::<usize>() {
                Ok(num) if num >= 1 => {
                    if !seen.insert(num) {
                        e.push(format!("Pin number {num} is used more than once."));
                    }
                }
                _ => e.push(format!(
                    "Pin '{}' has an invalid number ('{n}') — use a positive integer.",
                    row.name.trim()
                )),
            }
            for bad in unknown_function_tokens(&row.functions) {
                e.push(format!(
                    "Pin '{}' has an unknown function token '{bad}'.",
                    row.name.trim()
                ));
            }
        }
        e
    }

    /// Non-blocking advisories (shown amber; do not prevent Save).
    pub fn warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        // Warn exactly when no codegen backend claims this family — so a family
        // handled by the generic STM32 (embassy) backend never flags.
        // Not for an empty family: `errors` already asks for one, and "Family ''
        // has no backend" says the same thing a second time, worse.
        if !self.family.trim().is_empty()
            && crate::panels::mcu_module::codegen::family::backend_for(self.family.trim()).is_none()
        {
            w.push(format!(
                "Family '{}' has no codegen backend yet — the chip loads and its \
                 pins/clock show, but configuring a peripheral won't generate init \
                 code until a backend is added.",
                self.family.trim()
            ));
        }
        // ARM only: most Espressif parts leave this empty, because the ESP
        // template writes the esp-hal line itself.
        if self.toolchain == ToolchainKind::RustEmbedded && self.hal_dep.trim().is_empty() {
            w.push(
                "HAL dependency line is empty — the generated Cargo.toml will name no HAL \
                 crate, and the project will not build."
                    .into(),
            );
        }
        // Asked of `async_flavor_for`, the one place that decides which async
        // stack a family gets, so this cannot disagree with the generator. The
        // RP and nRF stacks take their HAL line from the chip, and with none an
        // Async project pairs embassy code with the blocking crate's manifest.
        use crate::panels::mcu_module::project_gen::{AsyncFlavor, async_flavor_for};
        if self.hal_dep_async.trim().is_empty()
            && matches!(
                async_flavor_for(self.family.trim(), ""),
                AsyncFlavor::Rp | AsyncFlavor::Nrf
            )
        {
            w.push(format!(
                "Family '{}' uses a different HAL crate on the Async runtime, and the \
                 async dependency line is empty — an Async project will not build. Add \
                 the embassy line (see a built-in board of this family for its shape).",
                self.family.trim()
            ));
        }
        // A ball-grid chip legitimately has NO edge pins — its pads are in the
        // grid, which the form carries but does not edit.
        if self.pins.iter().all(|s| s.is_empty()) && self.grid.is_none() {
            w.push("No pins defined — the Pins canvas will be empty.".into());
        }
        if let Some(g) = &self.grid {
            w.push(format!(
                "{} ball(s) on a {}x{} grid. The form edits edge pins only, so the grid is carried through unchanged — edit it in the .ron.",
                g.cells.len(),
                g.rows,
                g.cols
            ));
        }
        if self.package.trim().is_empty() {
            w.push(
                "Package is empty — set it (e.g. UFQFPN48 / LQFP64) before importing pins from a \
                 datasheet, so the right pin-count column is read."
                    .into(),
            );
        }
        w
    }

    /// The clock the form currently resolves to — an imported graph (kept while
    /// the family dropdown is "None"), otherwise the chosen family's tree.
    ///
    /// Shared by `to_definition` (what gets saved) and the dialog's "Export
    /// clock" button (what gets written to a `.ron`), so the two never diverge.
    pub fn effective_clock(&self) -> ClockDef {
        match (&self.imported_clock, self.clock) {
            // The preserved imported graph, unless the user actively switched
            // to a family model.
            (Some(g), ClockChoice::None) => g.clone(),
            _ => self.clock.to_def(),
        }
    }

    /// Attach a hand/AI-authored clock graph (from a `.ron` import). Stored as
    /// `imported_clock` with the family dropdown reset to None, so
    /// `effective_clock` returns it — the exact path a foreign graph already
    /// travels when a chip `.ron` is loaded.
    pub fn set_imported_clock(&mut self, gc: crate::panels::mcu_module::clock::graph::GraphClock) {
        self.imported_clock = Some(ClockDef::Graph(gc));
        self.clock_carried_in = false;
        self.clock = ClockChoice::None;
    }

    /// `sram_kb`, unless the RAM size was changed to another figure.
    ///
    /// The catalogue reads `sram_kb` before `ram_size`, so a chip edited from
    /// 128K to 256K would go on being listed with 128. Dropping it lets the
    /// new `ram_size` answer. An EMPTY `ram_size` is not another figure: an
    /// Espressif part has none, and `sram_kb` is the only RAM size it states.
    fn sram_kb_kept(&self) -> Option<u32> {
        let now = self.ram_size.trim();
        self.sram_kb
            .filter(|_| now.is_empty() || now == self.loaded_ram_size.trim())
    }

    /// Build the [`McuDefinition`]. Call only when [`errors`] is empty; blank
    /// scratch pin rows are dropped and numbers are parsed here.
    pub fn to_definition(&self) -> McuDefinition {
        let side = |rows: &[PinRow]| -> Vec<PinDef> {
            rows.iter()
                .filter(|r| {
                    !(r.number.trim().is_empty()
                        && r.name.trim().is_empty()
                        && r.functions.trim().is_empty())
                })
                .map(|r| PinDef {
                    number: r.number.trim().parse().unwrap_or(0),
                    name: r.name.trim().to_string(),
                    reserved: r.reserved,
                    functions: parse_functions(&r.functions),
                    af: r.af.clone(),
                    fn_owner: owners_to_functions(&r.fn_owner),
                })
                .collect()
        };
        McuDefinition {
            board_chip: self.board_chip.clone(),
            id: self.id.trim().to_string(),
            display_name: self.display_name.trim().to_string(),
            family: self.family.trim().to_string(),
            package: self.package.trim().to_string(),
            max_mhz: self.max_mhz,
            // The form has no field for this. An edited chip keeps the value
            // from its definition.
            sram_kb: self.sram_kb_kept(),
            dma: self.dma.clone(),
            irq_vectors: self.irq_vectors.clone(),
            usart_ip: self.usart_ip.clone(),
            sdmmc_ip: self.sdmmc_ip.clone(),
            cpu: self.cpu.trim().to_string(),
            toolchain: self.toolchain.clone(),
            project: ProjectDef {
                pkg_name: self.id.trim().to_string(),
                target: self.target.trim().to_string(),
                flash_origin: self.flash_origin.trim().to_string(),
                flash_size: self.flash_size.trim().to_string(),
                ram_origin: self.ram_origin.trim().to_string(),
                ram_size: self.ram_size.trim().to_string(),
                hal_dep: self.hal_dep.trim().to_string(),
                hal_dep_async: Some(self.hal_dep_async.trim().to_string())
                    .filter(|l| !l.is_empty()),
                probe_chip: self.probe_chip.trim().to_string(),
                memory_comment: self.memory_comment.trim().to_string(),
            },
            pins: PinLayout {
                top: side(&self.pins[0]),
                bottom: side(&self.pins[1]),
                left: side(&self.pins[2]),
                right: side(&self.pins[3]),
                // The form edits the four sides only; a ball grid is authored in
                // the `.ron` for now (the grid editor is a later phase). Editing
                // a grid chip here would silently drop its balls, which is why
                // `mcu_form_dialog` refuses to open one — see `from_definition`.
                grid: self.grid.clone(),
            },
            clock: self.effective_clock(),
            // Each graph family ships its own ceilings so its preset isn't
            // flagged against the F103 defaults. (F4's real per-chip ceiling is
            // set by the XML converter; this is the F411-class default.)
            clock_limits: match self.clock {
                ClockChoice::Stm32wba => crate::panels::mcu_module::clock::graph::stm32wba_limits(),
                ClockChoice::Stm32f4 => {
                    crate::panels::mcu_module::clock::graph::stm32f4_limits_default()
                }
                _ => Default::default(),
            },
            clock_presets: Vec::new(),
        }
    }

    /// Serialize the built definition to pretty RON (what gets written to disk).
    pub fn to_ron(&self) -> Result<String, String> {
        ron::ser::to_string_pretty(&self.to_definition(), ron::ser::PrettyConfig::default())
            .map(|t| super::ron_text::bare_none(&t))
            .map_err(|e| format!("RON serialize error: {e}"))
    }
}

/// A valid registry id / file stem: non-empty, ASCII `a–z 0–9 _` only.
/// The nRF52 part a chip name names - `nRF52840`, `nrf52810-qcaa`,
/// `My nRF52833 board` - or `None`. The LONGEST family that appears wins, so
/// no key can be read off a longer one.
fn nrf52_in_name(name: &str) -> Option<&'static crate::panels::mcu_module::codegen::nrf::NrfChip> {
    let lower = name.to_ascii_lowercase();
    crate::panels::mcu_module::codegen::nrf::NRF52_CHIPS
        .iter()
        .filter(|c| lower.contains(c.family))
        .max_by_key(|c| c.family.len())
}

pub fn is_valid_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Parse an ld-style number: hex (`0x…`), decimal, optional `K`/`M` suffix.
/// `None` on anything else. (Mirrors the private helper in `crate::size`; kept
/// here so the form has no dependency on that module.)
pub fn parse_ld_number(tok: &str) -> Option<u64> {
    let tok = tok.trim();
    if tok.is_empty() {
        return None;
    }
    let (body, mult) = match tok.chars().last() {
        Some('K') | Some('k') => (&tok[..tok.len() - 1], 1024u64),
        Some('M') | Some('m') => (&tok[..tok.len() - 1], 1024 * 1024),
        _ => (tok, 1),
    };
    let body = body.trim();
    let value = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()?
    } else {
        body.parse::<u64>().ok()?
    };
    Some(value * mult)
}

/// Fill one side with `count` sequential general-purpose pins named `prefix{n}`
/// (e.g. `PA0…`), each offering GPIO in/out — a fast start for a fresh chip.
pub fn gpio_bank(prefix: &str, start_number: usize, count: usize) -> Vec<PinRow> {
    (0..count)
        .map(|i| PinRow {
            number: (start_number + i).to_string(),
            name: format!("{prefix}{i}"),
            reserved: false,
            functions: "in out".to_string(),
            imported: false,
            af: Vec::new(),
            fn_owner: Vec::new(),
        })
        .collect()
}

/// The function-token cheatsheet shown under the pin editor.
pub const FUNCTION_TOKEN_HELP: &str = "in out · usart{n}_tx/rx/cts/rts/ck · \
    lpuart{n}_tx/rx/cts/rts · spi{n}_nss/sck/miso/mosi/rdy · i2c{n}_scl/sda · \
    adc{a}_{ch} · tim{t}_{ch} · swdio swclk · usb_dm usb_dp · can_rx can_tx · mco · \
    ESP: rmt{n} · touch{n} · mcpwm{u}_op{o}a/b · pcnt{u}_edge{c}/ctrl{c} · \
    lcd_d{n} lcd_dc/wr/cs/pclk/vsync/hsync/de · cam_d{n} cam_pclk/vsync/hsync/href/mclk · \
    parl_d{n} parl_clk/valid · parl_rx_d{n} parl_rx_clk/valid · \
    af:{signal} for anything else (e.g. af:sai1_sd_a, af:fmc_a0)";

/// Resolve `(token, gpio)` pairs into `(function, gpio)`.
///
/// A token the parser does not recognise is DROPPED rather than defaulted: an
/// owner attached to the wrong function would send generated code at the wrong
/// GPIO, which is worse than losing the override and falling back to the pin's
/// own name.
pub fn owners_to_functions(owners: &[(String, String)]) -> Vec<(PinFunction, String)> {
    owners
        .iter()
        .filter_map(|(tok, gpio)| {
            parse_functions(tok)
                .into_iter()
                .next()
                .map(|f| (f, gpio.clone()))
        })
        .collect()
}

/// Parse a space/comma-separated function token list into [`PinFunction`]s.
/// Unrecognised tokens are skipped here (validation lists them separately).
pub fn parse_functions(s: &str) -> Vec<PinFunction> {
    s.split([' ', ',', '\t', '\n'])
        .filter(|t| !t.is_empty())
        .filter_map(token_to_function)
        .collect()
}

/// Tokens that don't map to any [`PinFunction`] — surfaced as validation
/// errors so a typo (`uart1_tx`) is caught before Save.
pub fn unknown_function_tokens(s: &str) -> Vec<String> {
    s.split([' ', ',', '\t', '\n'])
        .filter(|t| !t.is_empty())
        .filter(|t| token_to_function(t).is_none())
        .map(str::to_string)
        .collect()
}

/// A number inside an ESP token, in the one spelling the writer uses: ASCII
/// digits, no sign, no leading zero. `u8::from_str` alone also takes `+3` and
/// `007`, which would give one function several tokens - and let a typo save
/// as something else. Only the ESP tokens go through it: the older ones have
/// always read the lenient way, and files may rely on that.
fn token_number(s: &str) -> Option<u8> {
    let plain = s.bytes().all(|b| b.is_ascii_digit()) && (s == "0" || !s.starts_with('0'));
    plain.then(|| s.parse().ok()).flatten()
}

/// One token → one [`PinFunction`]. Case-insensitive; the inverse of
/// [`function_to_token`].
fn token_to_function(tok: &str) -> Option<PinFunction> {
    let t = tok.trim().to_ascii_lowercase();
    // `af:<name>` — a generic alternate function the IDE doesn't model
    // natively (SAI / FMC / DCMI / …). Explicit prefix so a TYPO still gets
    // flagged by `unknown_function_tokens` instead of silently becoming one.
    if let Some(name) = t.strip_prefix("af:") {
        let name = name.trim();
        return (!name.is_empty()).then(|| PinFunction::Other(name.to_ascii_uppercase()));
    }
    // Fixed tokens first.
    let simple = match t.as_str() {
        "in" | "gpioinput" => Some(PinFunction::GpioInput),
        "out" | "gpiooutput" => Some(PinFunction::GpioOutput),
        "analog" | "gpioanalog" => Some(PinFunction::GpioAnalog),
        "swdio" => Some(PinFunction::SwdIo),
        "swclk" => Some(PinFunction::SwdClk),
        "usb_dm" => Some(PinFunction::UsbDm),
        "usb_dp" => Some(PinFunction::UsbDp),
        "can_rx" => Some(PinFunction::CanRx),
        "can_tx" => Some(PinFunction::CanTx),
        "mco" => Some(PinFunction::Mco),
        "qspi_clk" => Some(PinFunction::QspiClk),
        "lcd_dc" => Some(PinFunction::LcdCamDc),
        "lcd_wr" => Some(PinFunction::LcdCamWr),
        "lcd_cs" => Some(PinFunction::LcdCamCs),
        "lcd_pclk" => Some(PinFunction::LcdCamPclk),
        "lcd_vsync" => Some(PinFunction::LcdCamVsync),
        "lcd_hsync" => Some(PinFunction::LcdCamHsync),
        "lcd_de" => Some(PinFunction::LcdCamDe),
        "cam_pclk" => Some(PinFunction::CamPclk),
        "cam_vsync" => Some(PinFunction::CamVsync),
        "cam_hsync" => Some(PinFunction::CamHsync),
        "cam_href" => Some(PinFunction::CamHenable),
        "cam_mclk" => Some(PinFunction::CamMclk),
        "parl_clk" => Some(PinFunction::ParlClk),
        "parl_valid" => Some(PinFunction::ParlValid),
        "parl_rx_clk" => Some(PinFunction::ParlRxClk),
        "parl_rx_valid" => Some(PinFunction::ParlRxValid),
        _ => None,
    };
    if simple.is_some() {
        return simple;
    }
    // The Espressif tokens `function_to_token` writes. None of them was read
    // back, so Edit on any ESP part opened on several hundred "unknown function
    // token" errors, one per function per pad - the GPIO matrix offers every
    // one of these on every pad - and Save stayed disabled.
    //
    // `<word><n>` with no underscore at all, which the instance split further
    // down requires: `rmt0`, `touch7`.
    let numbered = |word: &str| token_number(t.strip_prefix(word)?);
    if let Some(n) = numbered("rmt") {
        return Some(PinFunction::RmtChannel(n));
    }
    if let Some(n) = numbered("touch") {
        return Some(PinFunction::TouchPad(n));
    }
    // A data lane of a port the chip has one of: `lcd_d3`, `cam_d7`, `parl_d0`,
    // `parl_rx_d15`. After the fixed names above, so `lcd_dc` and `lcd_de` are
    // taken as themselves and not as a lane that fails to parse.
    let lane_of = |prefix: &str| token_number(t.strip_prefix(prefix)?);
    if let Some(lane) = lane_of("lcd_d") {
        return Some(PinFunction::LcdCamData { lane });
    }
    if let Some(lane) = lane_of("cam_d") {
        return Some(PinFunction::CamData { lane });
    }
    if let Some(lane) = lane_of("parl_rx_d") {
        return Some(PinFunction::ParlRxData { lane });
    }
    if let Some(lane) = lane_of("parl_d") {
        return Some(PinFunction::ParlData { lane });
    }
    // `xspi_p1_io12` → XSPI port 1 data line 12. Same shape as the OCTOSPI
    // tokens below, and lifted for the same reason.
    if let Some(rest) = t.strip_prefix("xspi_") {
        let (p, role) = rest.split_once('_')?;
        let port = p.strip_prefix('p')?.parse().ok()?;
        return match role {
            "clk" => Some(PinFunction::XspiClk { port }),
            _ => {
                if let Some(cs) = role.strip_prefix("ncs").and_then(|c| c.parse().ok()) {
                    return (cs == 1 || cs == 2).then_some(PinFunction::XspiNcs { port, cs });
                }
                if let Some(index) = role.strip_prefix("dqs").and_then(|c| c.parse().ok()) {
                    return (index < 2).then_some(PinFunction::XspiDqs { port, index });
                }
                role.strip_prefix("io")
                    .and_then(|l| l.parse().ok())
                    .filter(|l| *l < 16)
                    .map(|lane| PinFunction::XspiIo { port, lane })
            }
        };
    }
    // `ospi_p1_io3` → OCTOSPI port 1 data line 3. Lifted for the same reason as
    // the QUADSPI tokens below: what varies is the PORT, not an instance.
    if let Some(rest) = t.strip_prefix("ospi_") {
        let (p, role) = rest.split_once('_')?;
        let port = p.strip_prefix('p')?.parse().ok()?;
        return match role {
            "clk" => Some(PinFunction::OspiClk { port }),
            "ncs" => Some(PinFunction::OspiNcs { port }),
            "dqs" => Some(PinFunction::OspiDqs { port }),
            _ => role
                .strip_prefix("io")
                .and_then(|l| l.parse().ok())
                .filter(|l| *l < 8)
                .map(|lane| PinFunction::OspiIo { port, lane }),
        };
    }
    // `qspi_b1_io2` → QUADSPI bank 1 data line 2. Lifted ABOVE the instance
    // split below, which needs a peripheral number: the chip has at most one
    // QUADSPI, and what varies is the BANK.
    if let Some(rest) = t.strip_prefix("qspi_") {
        let (b, role) = rest.split_once('_')?;
        let bank = b.strip_prefix('b')?.parse().ok()?;
        return match role {
            "ncs" => Some(PinFunction::QspiNcs { bank }),
            _ => role
                .strip_prefix("io")
                .and_then(|l| l.parse().ok())
                .filter(|l| *l < 4)
                .map(|lane| PinFunction::QspiIo { bank, lane }),
        };
    }
    // Parameterized `<peripheral><n>_<role>` and `<kind><a>_<b>`.
    let (head, tail) = t.split_once('_')?;
    // The instance number is the head's TRAILING digit run — NOT the first
    // digit: `i2c1` has a `2` inside the peripheral word, so `usart1` → 1 and
    // `i2c1` → 1 both need the suffix, not `find`.
    let split = head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (word, n_str) = head.split_at(split);
    let n: u8 = n_str.parse().ok()?;
    match word {
        "usart" => match tail {
            "tx" => Some(PinFunction::UsartTx(n)),
            "rx" => Some(PinFunction::UsartRx(n)),
            "cts" => Some(PinFunction::UsartCts(n)),
            // `rts_de` is the same physical pin as RTS (it doubles as the RS485
            // driver-enable), so both spellings land on RTS.
            "rts" | "rts_de" => Some(PinFunction::UsartRts(n)),
            "ck" => Some(PinFunction::UsartCk(n)),
            _ => None,
        },
        "lpuart" => match tail {
            "tx" => Some(PinFunction::LpuartTx(n)),
            "rx" => Some(PinFunction::LpuartRx(n)),
            "cts" => Some(PinFunction::LpuartCts(n)),
            "rts" | "rts_de" => Some(PinFunction::LpuartRts(n)),
            _ => None,
        },
        "spi" => match tail {
            "nss" => Some(PinFunction::SpiNss(n)),
            "sck" => Some(PinFunction::SpiSck(n)),
            "miso" => Some(PinFunction::SpiMiso(n)),
            "mosi" => Some(PinFunction::SpiMosi(n)),
            "rdy" => Some(PinFunction::SpiRdy(n)),
            _ => None,
        },
        // `sdmmc1_d3` → SDMMC1 data line 3; `sdmmc0_ck` is the un-numbered SDIO.
        // `hspi1_io3` → HSPI1 data line 3. Instance-numbered, so it goes
        // through the ordinary split — unlike the OCTOSPI's and XSPI's, which
        // are named after an IO-manager port.
        "hspi" => match tail {
            "clk" => Some(PinFunction::HspiClk { unit: n }),
            "ncs" => Some(PinFunction::HspiNcs { unit: n }),
            _ => {
                if let Some(index) = tail.strip_prefix("dqs").and_then(|c| c.parse().ok()) {
                    return (index < 2).then_some(PinFunction::HspiDqs { unit: n, index });
                }
                tail.strip_prefix("io")
                    .and_then(|l| l.parse().ok())
                    .filter(|l| *l < 16)
                    .map(|lane| PinFunction::HspiIo { unit: n, lane })
            }
        },
        "sdmmc" => match tail {
            "ck" => Some(PinFunction::SdmmcCk { unit: n }),
            "cmd" => Some(PinFunction::SdmmcCmd { unit: n }),
            _ => tail
                .strip_prefix('d')
                .and_then(|l| l.parse().ok())
                .filter(|l| *l < 8)
                .map(|lane| PinFunction::SdmmcD { unit: n, lane }),
        },
        // `sai1_a_sck` → SAI1 sub-block A bit clock.
        "sai" => {
            let (letter, role) = tail.split_once('_')?;
            let block = match letter {
                "a" => 1u8,
                "b" => 2,
                _ => return None,
            };
            let sai = n;
            match role {
                "sck" => Some(PinFunction::SaiSck { sai, block }),
                "sd" => Some(PinFunction::SaiSd { sai, block }),
                "fs" => Some(PinFunction::SaiFs { sai, block }),
                "mclk" => Some(PinFunction::SaiMclk { sai, block }),
                _ => None,
            }
        }
        // `dac1_out2` → DAC1 channel 2.
        "dac" => tail
            .strip_prefix("out")
            .and_then(|c| c.parse().ok())
            .map(|channel| PinFunction::DacOut { dac: n, channel }),
        // `mcpwm0_op2b` → MCPWM0 operator 2, output B.
        "mcpwm" => {
            // By suffix, never by byte index: this is typed text, and the
            // last byte of it need not be a whole character.
            let op = tail.strip_prefix("op")?;
            if let Some(operator) = op.strip_suffix('a').and_then(token_number) {
                return Some(PinFunction::McpwmA { unit: n, operator });
            }
            op.strip_suffix('b')
                .and_then(token_number)
                .map(|operator| PinFunction::McpwmB { unit: n, operator })
        }
        // `pcnt1_edge0` / `pcnt1_ctrl0` → PCNT unit 1, channel 0.
        "pcnt" => {
            if let Some(channel) = tail.strip_prefix("edge").and_then(token_number) {
                return Some(PinFunction::PcntEdge { unit: n, channel });
            }
            tail.strip_prefix("ctrl")
                .and_then(token_number)
                .map(|channel| PinFunction::PcntCtrl { unit: n, channel })
        }
        // `i2s0_rmt` and `i2s0_pcnt_edge` used to be read here. Nothing ever
        // wrote them - the writer's spellings are `rmt0` and `pcnt0_edge0` -
        // so they matched no file, and read a typo as a real function.
        "i2s" => match tail {
            "ck" => Some(PinFunction::I2sCk(n)),
            "ws" => Some(PinFunction::I2sWs(n)),
            "sd" => Some(PinFunction::I2sSd(n)),
            "mck" => Some(PinFunction::I2sMck(n)),
            _ => None,
        },
        "i2c" => match tail {
            "scl" => Some(PinFunction::I2cScl(n)),
            "sda" => Some(PinFunction::I2cSda(n)),
            _ => None,
        },
        // `adc1_5` → ADC1 channel 5; `tim2_1` → TIM2 CH1.
        "adc" => tail.parse().ok().map(|ch| PinFunction::AdcChannel {
            adc: n,
            channel: ch,
        }),
        // `tim1_2` → TIM1 CH2; the `n` suffix is the complementary output,
        // `tim1_2n` → TIM1 CH2N; `tim1_bkin1` / `tim1_bkin2` are the break inputs.
        "tim" if tail.starts_with("bkin") => tail
            .strip_prefix("bkin")
            .and_then(|i| i.parse::<u8>().ok())
            .filter(|i| *i == 1 || *i == 2)
            .map(|input| PinFunction::TimerBreak { timer: n, input }),
        "tim" => match tail.strip_suffix('n') {
            Some(ch) => ch.parse().ok().map(|ch| PinFunction::TimerPwmN {
                timer: n,
                channel: ch,
            }),
            None => tail.parse().ok().map(|ch| PinFunction::TimerPwm {
                timer: n,
                channel: ch,
            }),
        },
        _ => None,
    }
}

/// The lowercase sub-block letter used inside a SAI token.
fn sai_letter(block: u8) -> &'static str {
    if block == 1 { "a" } else { "b" }
}

/// One [`PinFunction`] → its canonical token (inverse of [`token_to_function`]).
fn function_to_token(f: &PinFunction) -> Option<String> {
    Some(match f {
        PinFunction::Unset => return None,
        PinFunction::GpioInput => "in".into(),
        PinFunction::GpioOutput => "out".into(),
        PinFunction::GpioAnalog => "analog".into(),
        PinFunction::SwdIo => "swdio".into(),
        PinFunction::SwdClk => "swclk".into(),
        PinFunction::UsbDm => "usb_dm".into(),
        PinFunction::UsbDp => "usb_dp".into(),
        PinFunction::CanRx => "can_rx".into(),
        PinFunction::CanTx => "can_tx".into(),
        PinFunction::Mco => "mco".into(),
        PinFunction::UsartTx(n) => format!("usart{n}_tx"),
        PinFunction::UsartRx(n) => format!("usart{n}_rx"),
        PinFunction::UsartCts(n) => format!("usart{n}_cts"),
        PinFunction::UsartRts(n) => format!("usart{n}_rts"),
        PinFunction::UsartCk(n) => format!("usart{n}_ck"),
        PinFunction::LpuartTx(n) => format!("lpuart{n}_tx"),
        PinFunction::LpuartRx(n) => format!("lpuart{n}_rx"),
        PinFunction::LpuartCts(n) => format!("lpuart{n}_cts"),
        PinFunction::LpuartRts(n) => format!("lpuart{n}_rts"),
        PinFunction::SpiNss(n) => format!("spi{n}_nss"),
        PinFunction::SpiSck(n) => format!("spi{n}_sck"),
        PinFunction::SpiMiso(n) => format!("spi{n}_miso"),
        PinFunction::SpiMosi(n) => format!("spi{n}_mosi"),
        PinFunction::SpiRdy(n) => format!("spi{n}_rdy"),
        PinFunction::DacOut { dac, channel } => format!("dac{dac}_out{channel}"),
        PinFunction::HspiClk { unit } => format!("hspi{unit}_clk"),
        PinFunction::HspiNcs { unit } => format!("hspi{unit}_ncs"),
        PinFunction::HspiDqs { unit, index } => format!("hspi{unit}_dqs{index}"),
        PinFunction::HspiIo { unit, lane } => format!("hspi{unit}_io{lane}"),
        PinFunction::XspiClk { port } => format!("xspi_p{port}_clk"),
        PinFunction::XspiNcs { port, cs } => format!("xspi_p{port}_ncs{cs}"),
        PinFunction::XspiDqs { port, index } => format!("xspi_p{port}_dqs{index}"),
        PinFunction::XspiIo { port, lane } => format!("xspi_p{port}_io{lane}"),
        PinFunction::OspiClk { port } => format!("ospi_p{port}_clk"),
        PinFunction::OspiNcs { port } => format!("ospi_p{port}_ncs"),
        PinFunction::OspiDqs { port } => format!("ospi_p{port}_dqs"),
        PinFunction::OspiIo { port, lane } => format!("ospi_p{port}_io{lane}"),
        PinFunction::QspiClk => "qspi_clk".into(),
        PinFunction::QspiNcs { bank } => format!("qspi_b{bank}_ncs"),
        PinFunction::QspiIo { bank, lane } => format!("qspi_b{bank}_io{lane}"),
        PinFunction::SdmmcCk { unit } => format!("sdmmc{unit}_ck"),
        PinFunction::SdmmcCmd { unit } => format!("sdmmc{unit}_cmd"),
        PinFunction::SdmmcD { unit, lane } => format!("sdmmc{unit}_d{lane}"),
        PinFunction::SaiSck { sai, block } => format!("sai{sai}_{}_sck", sai_letter(*block)),
        PinFunction::SaiSd { sai, block } => format!("sai{sai}_{}_sd", sai_letter(*block)),
        PinFunction::SaiFs { sai, block } => format!("sai{sai}_{}_fs", sai_letter(*block)),
        PinFunction::SaiMclk { sai, block } => format!("sai{sai}_{}_mclk", sai_letter(*block)),
        PinFunction::RmtChannel(n) => format!("rmt{n}"),
        PinFunction::TouchPad(n) => format!("touch{n}"),
        PinFunction::LcdCamData { lane } => format!("lcd_d{lane}"),
        PinFunction::LcdCamDc => "lcd_dc".to_owned(),
        PinFunction::LcdCamWr => "lcd_wr".to_owned(),
        PinFunction::LcdCamCs => "lcd_cs".to_owned(),
        PinFunction::LcdCamPclk => "lcd_pclk".to_owned(),
        PinFunction::LcdCamVsync => "lcd_vsync".to_owned(),
        PinFunction::LcdCamHsync => "lcd_hsync".to_owned(),
        PinFunction::LcdCamDe => "lcd_de".to_owned(),
        PinFunction::CamData { lane } => format!("cam_d{lane}"),
        PinFunction::CamPclk => "cam_pclk".to_owned(),
        PinFunction::CamVsync => "cam_vsync".to_owned(),
        PinFunction::CamHsync => "cam_hsync".to_owned(),
        PinFunction::CamHenable => "cam_href".to_owned(),
        PinFunction::CamMclk => "cam_mclk".to_owned(),
        PinFunction::ParlData { lane } => format!("parl_d{lane}"),
        PinFunction::ParlClk => "parl_clk".to_owned(),
        PinFunction::ParlValid => "parl_valid".to_owned(),
        PinFunction::ParlRxData { lane } => format!("parl_rx_d{lane}"),
        PinFunction::ParlRxClk => "parl_rx_clk".to_owned(),
        PinFunction::ParlRxValid => "parl_rx_valid".to_owned(),
        PinFunction::McpwmA { unit, operator } => format!("mcpwm{unit}_op{operator}a"),
        PinFunction::McpwmB { unit, operator } => format!("mcpwm{unit}_op{operator}b"),
        PinFunction::PcntEdge { unit, channel } => format!("pcnt{unit}_edge{channel}"),
        PinFunction::PcntCtrl { unit, channel } => format!("pcnt{unit}_ctrl{channel}"),
        PinFunction::I2sCk(n) => format!("i2s{n}_ck"),
        PinFunction::I2sWs(n) => format!("i2s{n}_ws"),
        PinFunction::I2sSd(n) => format!("i2s{n}_sd"),
        PinFunction::I2sMck(n) => format!("i2s{n}_mck"),
        PinFunction::I2cScl(n) => format!("i2c{n}_scl"),
        PinFunction::I2cSda(n) => format!("i2c{n}_sda"),
        PinFunction::AdcChannel { adc, channel } => format!("adc{adc}_{channel}"),
        PinFunction::TimerPwm { timer, channel } => format!("tim{timer}_{channel}"),
        PinFunction::TimerPwmN { timer, channel } => format!("tim{timer}_{channel}n"),
        PinFunction::TimerBreak { timer, input } => format!("tim{timer}_bkin{input}"),
        PinFunction::Other(name) => format!("af:{}", name.to_ascii_lowercase()),
    })
}

/// Format a function list back into an editable token string (space-joined).
pub fn functions_to_string(fns: &[PinFunction]) -> String {
    fns.iter()
        .filter_map(function_to_token)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins::builtin_for;

    #[test]
    fn ld_number_parses_hex_dec_and_suffixes() {
        assert_eq!(parse_ld_number("0x08000000"), Some(0x0800_0000));
        assert_eq!(parse_ld_number("64K"), Some(64 * 1024));
        assert_eq!(parse_ld_number("1M"), Some(1024 * 1024));
        assert_eq!(parse_ld_number("131072"), Some(131072));
        assert_eq!(parse_ld_number(""), None);
        assert_eq!(parse_ld_number("garbage"), None);
    }

    #[test]
    fn id_validation() {
        assert!(is_valid_id("stm32f103rb"));
        assert!(is_valid_id("esp32_c6"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id("STM32")); // uppercase
        assert!(!is_valid_id("a b")); // space
        assert!(!is_valid_id("a-b")); // dash
    }

    #[test]
    fn blank_form_reports_the_missing_essentials() {
        let f = McuForm::blank();
        let errs = f.errors();
        // id, display_name, probe_chip empty on a blank ARM form.
        assert!(errs.iter().any(|e| e.contains("Id must")));
        assert!(errs.iter().any(|e| e.contains("Display name")));
        assert!(errs.iter().any(|e| e.contains("Probe chip")));
    }

    #[test]
    fn a_minimal_valid_form_builds_and_round_trips() {
        let mut f = McuForm::blank();
        f.id = "stm32f103rb".into();
        f.display_name = "STM32F103RB".into();
        f.probe_chip = "STM32F103RB".into();
        f.pins[2] = gpio_bank("PA", 1, 4);
        assert!(f.errors().is_empty(), "{:?}", f.errors());

        let def = f.to_definition();
        assert_eq!(def.id, "stm32f103rb");
        assert_eq!(def.pins.left.len(), 4);
        assert_eq!(def.pins.left[0].name, "PA0");
        // Builds into a runtime Mcu.
        assert!(def.build_mcu().iter_all_pins().count() >= 4);

        // The RON it writes parses back to an equal definition.
        let ron = f.to_ron().unwrap();
        let parsed: McuDefinition = ron::from_str(&ron).unwrap();
        assert_eq!(parsed, def);
    }

    #[test]
    fn duplicate_and_bad_pin_numbers_are_errors() {
        let mut f = McuForm::blank();
        f.id = "x".into();
        f.display_name = "X".into();
        f.probe_chip = "X".into();
        f.pins[0] = vec![
            PinRow {
                number: "1".into(),
                name: "PA0".into(),
                ..Default::default()
            },
            PinRow {
                number: "1".into(),
                name: "PA1".into(),
                ..Default::default()
            },
            PinRow {
                number: "abc".into(),
                name: "PA2".into(),
                ..Default::default()
            },
        ];
        let errs = f.errors();
        assert!(errs.iter().any(|e| e.contains("used more than once")));
        assert!(errs.iter().any(|e| e.contains("invalid number")));
    }

    #[test]
    fn function_tokens_round_trip_and_reject_typos() {
        let src = "in out usart1_tx spi2_sck i2c1_scl adc1_5 tim2_1 swdio usb_dp can_tx mco";
        let fns = parse_functions(src);
        assert_eq!(fns.len(), 11);
        assert_eq!(fns[2], PinFunction::UsartTx(1));
        assert_eq!(fns[5], PinFunction::AdcChannel { adc: 1, channel: 5 });
        assert_eq!(
            fns[6],
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1
            }
        );
        // Round-trips through the canonical string form.
        assert_eq!(parse_functions(&functions_to_string(&fns)), fns);
        // Commas and case are accepted; a typo is reported, others still parse.
        assert_eq!(parse_functions("IN, USART2_RX").len(), 2);
        assert_eq!(
            unknown_function_tokens("in uart1_tx spi9_bad out"),
            vec!["uart1_tx", "spi9_bad"]
        );
        // A bad token on a pin row surfaces as a validation error.
        let mut f = McuForm::blank();
        f.id = "x".into();
        f.display_name = "X".into();
        f.probe_chip = "X".into();
        f.pins[0] = vec![PinRow {
            number: "1".into(),
            name: "PA0".into(),
            reserved: false,
            functions: "in wat".into(),
            imported: false,
            af: Vec::new(),
            fn_owner: Vec::new(),
        }];
        assert!(
            f.errors()
                .iter()
                .any(|e| e.contains("unknown function token 'wat'"))
        );
    }

    #[test]
    fn esp_form_skips_memory_and_probe_requirements() {
        let mut f = McuForm::blank();
        f.id = "esp32c6".into();
        f.display_name = "ESP32-C6".into();
        f.family = "esp32c3".into(); // reuse the backed family for the test
        f.toolchain = ToolchainKind::EspRust;
        f.target = "riscv32imac-unknown-none-elf".into();
        f.clock = ClockChoice::Esp32c3;
        // No probe_chip, no memory — must still be valid for ESP.
        f.probe_chip.clear();
        f.flash_origin.clear();
        assert!(f.errors().is_empty(), "{:?}", f.errors());
    }

    #[test]
    fn from_definition_seeds_every_field() {
        let def = builtin_for("stm32f103c8t6").unwrap();
        let f = McuForm::from_definition(&def);
        assert_eq!(f.id, def.id);
        assert!(f.editing);
        // Round-trip through the form preserves the definition (clock graphs
        // collapse to their choice, but stm32f1 defaults reconstruct equal).
        let rebuilt = f.to_definition();
        assert_eq!(rebuilt.id, def.id);
        assert_eq!(rebuilt.pins, def.pins);
        assert_eq!(rebuilt.project, def.project);
    }

    /// Edit then Save, with nothing touched, must write back what it read.
    /// The form has no field for `board_chip`, `sram_kb` or `hal_dep_async`, and
    /// once rebuilt all three as `None`: the saved file overrides the built-in,
    /// so a board lost its chip square and its Async manifest named the
    /// blocking HAL.
    ///
    /// Whole definitions, pin functions included. They were left out of both
    /// sides once, because the ESP tokens did not read back and that defect
    /// would have hidden this one.
    #[test]
    fn every_builtin_survives_an_untouched_edit() {
        for def in crate::panels::mcu_module::builtins::builtin_definitions() {
            let rebuilt = McuForm::from_definition(&def).to_definition();
            assert_eq!(rebuilt, def, "{}", def.id);
        }
    }

    /// Every function any bundled chip offers reads back from the token it is
    /// written as, and a bundled chip opens in the form with no error at all.
    /// The ESP ones did not: `rmt0`, `touch3`, `lcd_d0`, `cam_d0`, `parl_d0`,
    /// `mcpwm0_op0a` and `pcnt0_edge0` were written and never read, so Edit on
    /// an ESP32 listed several hundred errors and could not be saved.
    #[test]
    fn every_function_a_builtin_offers_reads_back_from_its_token() {
        for def in crate::panels::mcu_module::builtins::builtin_definitions() {
            let p = &def.pins;
            for pin in [&p.top, &p.bottom, &p.left, &p.right].into_iter().flatten() {
                for f in &pin.functions {
                    let Some(tok) = function_to_token(f) else {
                        continue;
                    };
                    assert_eq!(
                        token_to_function(&tok).as_ref(),
                        Some(f),
                        "{}: {} writes '{tok}'",
                        def.id,
                        pin.name
                    );
                }
            }
            let errs = McuForm::from_definition(&def).errors();
            assert!(errs.is_empty(), "{}: {errs:?}", def.id);
        }
    }

    /// The ESP spellings one by one, since a bundled chip need not offer all
    /// of them - and the near misses, which must stay typos.
    #[test]
    fn the_esp_tokens_read_back_and_their_near_misses_do_not() {
        for f in [
            PinFunction::RmtChannel(3),
            PinFunction::TouchPad(14),
            PinFunction::LcdCamData { lane: 15 },
            PinFunction::LcdCamDc,
            PinFunction::LcdCamDe,
            PinFunction::CamData { lane: 7 },
            PinFunction::CamHenable,
            PinFunction::CamMclk,
            PinFunction::ParlData { lane: 15 },
            PinFunction::ParlClk,
            PinFunction::ParlValid,
            PinFunction::ParlRxData { lane: 15 },
            PinFunction::ParlRxClk,
            PinFunction::ParlRxValid,
            PinFunction::McpwmA {
                unit: 1,
                operator: 2,
            },
            PinFunction::McpwmB {
                unit: 0,
                operator: 0,
            },
            PinFunction::PcntEdge {
                unit: 3,
                channel: 1,
            },
            PinFunction::PcntCtrl {
                unit: 0,
                channel: 0,
            },
        ] {
            let tok = function_to_token(&f).unwrap();
            assert_eq!(token_to_function(&tok), Some(f), "{tok}");
        }
        for typo in [
            "rmt",
            "rmtx",
            "rmt_0",
            "touch",
            "lcd_d",
            "lcd_dx",
            "cam_d",
            "parl_d",
            "parl_rx_d",
            "parl_rx",
            "mcpwm0_op",
            "mcpwm0_opa",
            "mcpwm0_op0c",
            "mcpwm_op0a",
            "pcnt0_edge",
            "pcnt0_edgex",
            "pcnt_edge0",
            "i2s0_rmt",
            "i2s0_pcnt_edge",
        ] {
            assert_eq!(token_to_function(typo), None, "{typo}");
        }
    }

    /// The box is free text, and `errors()` reads it on every frame: whatever
    /// can be typed must come back as "unknown", never as a panic. Splitting
    /// the last BYTE off `op0\u{e9}` lands inside the character.
    #[test]
    fn a_non_ascii_token_is_unknown_and_does_not_panic() {
        for typed in [
            "mcpwm0_op0\u{e9}",
            "mcpwm0_op\u{e9}",
            "rmt\u{e9}",
            "touch\u{663}",
            "lcd_d\u{e9}",
            "pcnt0_edge\u{e9}",
            "\u{e9}",
        ] {
            assert_eq!(token_to_function(typed), None, "{typed}");
            assert_eq!(unknown_function_tokens(typed), vec![typed.to_owned()]);
        }
    }

    /// The legend under the pin editor is where these spellings are learned,
    /// so each ESP form it names is one the reader takes.
    #[test]
    fn the_legend_names_esp_tokens_that_read() {
        let esp = FUNCTION_TOKEN_HELP
            .split("ESP:")
            .nth(1)
            .expect("an ESP part");
        for shown in [
            "rmt{n}",
            "touch{n}",
            "mcpwm{u}_op{o}a/b",
            "pcnt{u}_edge{c}/ctrl{c}",
            "lcd_d{n}",
            "cam_d{n}",
            "cam_pclk/vsync/hsync/href/mclk",
            "parl_d{n}",
            "parl_rx_d{n}",
        ] {
            assert!(esp.contains(shown), "{shown}");
        }
        for token in [
            "rmt0",
            "touch1",
            "mcpwm0_op1a",
            "mcpwm0_op1b",
            "pcnt0_edge1",
            "pcnt0_ctrl1",
            "lcd_d0",
            "lcd_dc",
            "lcd_wr",
            "lcd_cs",
            "lcd_pclk",
            "lcd_vsync",
            "lcd_hsync",
            "lcd_de",
            "cam_d0",
            "cam_pclk",
            "cam_vsync",
            "cam_hsync",
            "cam_href",
            "cam_mclk",
            "parl_d0",
            "parl_clk",
            "parl_valid",
            "parl_rx_d0",
            "parl_rx_clk",
            "parl_rx_valid",
        ] {
            assert!(token_to_function(token).is_some(), "{token}");
        }
    }

    /// A number in an ESP token is written one way, and only that way reads:
    /// `u8::from_str` alone also takes `+3` and `007`, which would make two
    /// spellings of one function - and a typo that saves as something else.
    #[test]
    fn an_esp_token_number_has_one_spelling() {
        for typo in [
            "rmt+3",
            "rmt007",
            "rmt256",
            "touch+1",
            "lcd_d+0",
            "lcd_d00",
            "parl_rx_d+1",
            "mcpwm0_op+1a",
            "mcpwm0_op01b",
            "pcnt0_edge+0",
            "pcnt0_ctrl00",
        ] {
            assert_eq!(token_to_function(typo), None, "{typo}");
        }
        assert_eq!(token_to_function("rmt0"), Some(PinFunction::RmtChannel(0)));
        assert_eq!(
            token_to_function("RMT10"),
            Some(PinFunction::RmtChannel(10))
        );
    }

    /// The async line REPLACES `hal_dep` in every Async project, so one left
    /// over from the chip a form was cloned from must not outlive Auto-fill
    /// rewriting `hal_dep` for a different part.
    #[test]
    fn auto_fill_drops_the_async_line_of_the_chip_it_was_cloned_from() {
        let mut f = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        assert!(f.hal_dep_async.starts_with("embassy-nrf"));
        f.display_name = "STM32F411RETx".into();
        assert!(f.auto_fill_identity());
        assert_eq!(f.to_definition().project.hal_dep_async, None);
    }

    /// The New MCU form opens empty, and says what it needs. It opened on F1
    /// values before, which are all valid and so were never questioned.
    #[test]
    fn the_empty_form_asks_for_everything_it_used_to_assume() {
        let f = McuForm::empty();
        let errs = f.errors();
        for needed in [
            "Family is required",
            "Target triple is required",
            "Flash origin",
            "Flash size",
            "RAM origin",
            "RAM size",
        ] {
            assert!(
                errs.iter().any(|e| e.contains(needed)),
                "{needed}: {errs:?}"
            );
        }
        assert!(f.warnings().iter().any(|w| w.contains("HAL dependency")));
        // Asked for once each, as missing: not as a malformed `('')`, and not a
        // second time as a family with no backend.
        assert!(!errs.iter().any(|e| e.contains("('')")), "{errs:?}");
        assert!(
            errs.iter()
                .any(|e| e.starts_with("Flash origin is required"))
        );
        assert!(!f.warnings().iter().any(|w| w.contains("codegen backend")));
        assert_eq!(f.clock, ClockChoice::None);
        assert!(f.cpu.is_empty());
    }

    /// Auto-fill makes an empty form a complete STM32 one, short of the two
    /// sizes, which are per-part. It does not overwrite a typed origin.
    #[test]
    fn auto_fill_completes_an_empty_form_for_an_stm32() {
        let mut f = McuForm::empty();
        f.id = "stm32f411re".into();
        f.display_name = "STM32F411RETx".into();
        assert!(f.auto_fill_identity());
        assert_eq!(f.flash_origin, "0x08000000");
        assert_eq!(f.ram_origin, "0x20000000");
        assert_eq!(f.clock, ClockChoice::Stm32f4);
        assert!(!f.hal_dep.is_empty());
        f.flash_size = "512K".into();
        f.ram_size = "128K".into();
        assert!(f.errors().is_empty(), "{:?}", f.errors());

        // A re-fill on the family the form is already on leaves what was typed
        // into it: this is where a bootloader offset lives.
        let mut same = McuForm::empty();
        same.family = "stm32f4".into();
        same.display_name = "STM32F411RETx".into();
        same.flash_origin = "0x08004000".into();
        assert!(same.auto_fill_identity());
        assert_eq!(same.flash_origin, "0x08004000");
    }

    /// Cloning a board is how a form comes to describe another part, and it
    /// arrives holding the old family's memory map and clock graph. Auto-fill
    /// is where it is told which part it now is.
    #[test]
    fn auto_fill_moves_the_memory_map_and_clock_to_the_new_family() {
        let mut f = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        assert_eq!(f.flash_origin, "0x00000000", "the nRF map");
        assert!(f.imported_clock.is_some(), "the nRF graph");

        f.id = "stm32f411re".into();
        f.display_name = "STM32F411RETx".into();
        assert!(f.auto_fill_identity());
        assert_eq!(f.flash_origin, "0x08000000");
        assert_eq!(f.ram_origin, "0x20000000");
        assert_eq!(f.clock, ClockChoice::Stm32f4);
        assert!(f.imported_clock.is_none(), "the nRF graph is gone");

        // What gets saved is the F4 tree, not the nRF one.
        match f.to_definition().clock {
            ClockDef::Graph(gc) => assert!(crate::panels::mcu_module::clock::graph::is_f4_graph(
                &gc.graph
            )),
            other => panic!("expected the F4 graph, got {other:?}"),
        }
    }

    /// A tree ATTACHED to this form is the user's answer for this chip. The AI
    /// import attaches one from a request of its own, which can land before
    /// the pins request that runs Auto-fill, so dropping it on a family move
    /// made the saved clock depend on which reply arrived first.
    #[test]
    fn auto_fill_keeps_a_tree_attached_to_this_form() {
        let attached = || match builtin_for("nrf52833_microbit_v2").unwrap().clock {
            ClockDef::Graph(gc) => gc,
            other => panic!("expected a graph, got {other:?}"),
        };

        let mut f = McuForm::empty();
        f.set_imported_clock(attached());
        f.display_name = "STM32F411RETx".into();
        assert!(f.auto_fill_identity());
        assert_eq!(f.to_definition().clock, ClockDef::Graph(attached()));

        // On a cloned form too: attaching replaces the carried-in graph, and
        // what replaced it is not the old chip's.
        let mut cloned = McuForm::from_definition(&builtin_for("rp2040_pico").unwrap());
        cloned.set_imported_clock(attached());
        cloned.display_name = "STM32F411RETx".into();
        assert!(cloned.auto_fill_identity());
        assert_eq!(cloned.to_definition().clock, ClockDef::Graph(attached()));

        // Shadowed by a dropdown model the user picked over it: the dropdown
        // follows the family, and the attached tree is still held.
        let mut shadowed = McuForm::empty();
        shadowed.set_imported_clock(attached());
        shadowed.clock = ClockChoice::Stm32f1;
        shadowed.display_name = "STM32G431KBTx".into();
        assert!(shadowed.auto_fill_identity());
        assert_eq!(shadowed.clock, ClockChoice::Stm32g4);
        assert!(shadowed.imported_clock.is_some());
    }

    /// The vendor's family key can be finer than a part name yields. An
    /// unrenamed L4+ chip is not a move, and keeps its own tree and origins.
    #[test]
    fn a_finer_vendor_family_key_is_not_a_family_move() {
        let mut def = builtin_for("nrf52833_microbit_v2").unwrap();
        def.family = "stm32l4+".into();
        def.display_name = "STM32L4R5ZITx".into();
        def.project.flash_origin = "0x08004000".into();
        let mut f = McuForm::from_definition(&def);
        assert!(f.imported_clock.is_some());

        assert!(f.auto_fill_identity());
        assert!(f.imported_clock.is_some(), "its own tree");
        assert_eq!(f.clock, ClockChoice::None);
        assert_eq!(f.flash_origin, "0x08004000");

        // Typed in capitals is the same family as well.
        let mut typed = McuForm::empty();
        typed.family = "STM32F4".into();
        typed.display_name = "STM32F411RETx".into();
        typed.flash_origin = "0x08004000".into();
        assert!(typed.auto_fill_identity());
        assert_eq!(typed.flash_origin, "0x08004000");
    }

    /// A family with no modelled tree takes none, rather than the old one.
    #[test]
    fn a_family_without_a_tree_does_not_inherit_the_old_one() {
        let mut f = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        f.display_name = "STM32H563ZITx".into();
        assert!(f.auto_fill_identity());
        assert_ne!(f.family, "nrf52833");
        assert_eq!(f.clock, ClockChoice::None);
        assert!(f.imported_clock.is_none());
    }

    /// A family whose Async stack names another crate needs the line that says
    /// which. The built-ins are held to that by their own tests; a chip
    /// authored here is held to it by nothing but this warning.
    #[test]
    fn a_missing_async_line_warns_only_where_async_swaps_crates() {
        let warns = |f: &McuForm| f.warnings().iter().any(|w| w.contains("async dependency"));

        let mut nrf = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        assert!(!warns(&nrf), "the built-in has its line");
        nrf.hal_dep_async = " ".into();
        assert!(warns(&nrf));

        let mut rp = McuForm::from_definition(&builtin_for("rp2040_pico").unwrap());
        rp.hal_dep_async.clear();
        assert!(warns(&rp));

        // One crate for both runtimes: nothing is missing.
        assert!(!warns(&McuForm::blank()), "stm32f1");
        assert!(!warns(&McuForm::from_definition(
            &builtin_for("esp32c3").unwrap()
        )));
    }

    /// An emptied box is "this chip keeps one crate", not an empty line in the
    /// saved file.
    #[test]
    fn a_cleared_async_line_is_saved_as_none() {
        let mut f = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        f.hal_dep_async = "  ".into();
        assert_eq!(f.to_definition().project.hal_dep_async, None);
    }

    /// `sram_kb` describes the RAM size the chip was loaded with.
    #[test]
    fn sram_kb_follows_the_ram_size_it_was_loaded_with() {
        let mut f = McuForm::from_definition(&builtin_for("nrf52833_microbit_v2").unwrap());
        assert_eq!(f.to_definition().sram_kb, Some(128));
        f.ram_size = "256K".into();
        assert_eq!(f.to_definition().sram_kb, None, "stale beside 256K");
        f.ram_size = " 128K ".into();
        assert_eq!(f.to_definition().sram_kb, Some(128), "changed back");

        // An ESP part has no `ram_size` at all: nothing was changed.
        let esp = McuForm::from_definition(&builtin_for("esp32c3").unwrap());
        assert!(esp.ram_size.is_empty());
        assert!(esp.to_definition().sram_kb.is_some());
    }

    /// The WBA clock choice: the def carries the WBA graph + the WBA ceilings,
    /// Edit detects it back, and a FOREIGN imported graph survives Edit→Save.
    #[test]
    fn wba_clock_choice_round_trips_and_foreign_graphs_survive() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, is_wba_graph};

        let mut f = McuForm::blank();
        f.id = "stm32wba55cg".into();
        f.display_name = "STM32WBA55CG".into();
        f.family = "stm32wba".into();
        f.probe_chip = "STM32WBA55CGUx".into();
        f.target = "thumbv8m.main-none-eabihf".into();
        f.clock = ClockChoice::Stm32wba;
        let def = f.to_definition();
        match &def.clock {
            ClockDef::Graph(gc) => assert!(is_wba_graph(&gc.graph)),
            other => panic!("expected WBA graph, got {other:?}"),
        }
        assert_eq!(def.clock_limits.sysclk_max, 100_000_000);
        // Edit path detects the WBA tree again.
        assert_eq!(McuForm::from_definition(&def).clock, ClockChoice::Stm32wba);

        // A hand-imported foreign graph: choice shows None but the graph is
        // preserved verbatim through Edit → Save.
        let mut imported = def.clone();
        imported.clock = ClockDef::Graph(GraphClock {
            graph: crate::panels::mcu_module::clock::graph::ClockGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            layout: Default::default(),
            bindings: Default::default(),
        }); // an empty graph — not the WBA tree
        let edited = McuForm::from_definition(&imported);
        assert_eq!(edited.clock, ClockChoice::None);
        assert_eq!(edited.to_definition().clock, imported.clock);
    }

    /// The STM32F4 clock choice drives the whole chain: the def carries the F4
    /// graph, the generated main.rs carries the embassy F4 RCC config, and Edit
    /// detects the choice back.
    #[test]
    fn stm32f4_clock_choice_reaches_generated_code() {
        use crate::panels::mcu_module::clock::graph::is_f4_graph;

        let mut f = McuForm::blank();
        f.id = "stm32f411re".into();
        f.display_name = "STM32F411RE".into();
        f.family = "stm32f4".into();
        f.probe_chip = "STM32F411RE".into();
        f.target = "thumbv7em-none-eabihf".into();
        f.clock = ClockChoice::Stm32f4;

        let def = f.to_definition();
        match &def.clock {
            ClockDef::Graph(gc) => assert!(is_f4_graph(&gc.graph)),
            other => panic!("expected F4 graph, got {other:?}"),
        }
        assert_eq!(def.clock_limits.sysclk_max, 100_000_000);

        // Full chain: build the Mcu and generate main.rs.
        let code = def.build_mcu().fresh_main_rs();
        assert!(
            code.contains("config.rcc.sys = rcc::Sysclk::PLL1_P;"),
            "{code}"
        );
        assert!(code.contains("SYSCLK 100 MHz"), "{code}");
        // Edit round-trips the choice.
        assert_eq!(McuForm::from_definition(&def).clock, ClockChoice::Stm32f4);
    }

    /// LPUART / SPI-RDY are first-class tokens now, and `rts_de` is accepted as
    /// a spelling of `rts` (same physical pin — RS485 driver enable).
    #[test]
    fn lpuart_spi_rdy_and_rts_de_tokens() {
        let fns = parse_functions("lpuart1_tx lpuart1_rx lpuart2_cts lpuart1_rts spi3_rdy");
        assert_eq!(
            fns,
            vec![
                PinFunction::LpuartTx(1),
                PinFunction::LpuartRx(1),
                PinFunction::LpuartCts(2),
                PinFunction::LpuartRts(1),
                PinFunction::SpiRdy(3),
            ]
        );
        // Canonical round-trip.
        assert_eq!(parse_functions(&functions_to_string(&fns)), fns);
        // `rts_de` is an accepted alias for `rts` on both peripherals.
        assert_eq!(
            parse_functions("usart2_rts_de"),
            vec![PinFunction::UsartRts(2)]
        );
        assert_eq!(
            parse_functions("lpuart1_rts_de"),
            vec![PinFunction::LpuartRts(1)]
        );
        // None of them are flagged as unknown any more.
        assert!(
            unknown_function_tokens("lpuart1_tx lpuart1_rts_de spi1_rdy usart2_rts_de").is_empty()
        );
        // The cheatsheet advertises them.
        assert!(FUNCTION_TOKEN_HELP.contains("lpuart"));
        assert!(FUNCTION_TOKEN_HELP.contains("rdy"));
    }

    /// `af:<signal>` carries anything the IDE doesn't model natively, so an
    /// import never loses a pin function — while typos are STILL flagged
    /// (that's why the prefix is explicit rather than a catch-all).
    #[test]
    fn generic_af_tokens_round_trip_and_typos_still_flagged() {
        let fns = parse_functions("in out af:sai1_sd_a af:fmc_a0 af:tim1_ch1n");
        assert_eq!(
            fns,
            vec![
                PinFunction::GpioInput,
                PinFunction::GpioOutput,
                PinFunction::Other("SAI1_SD_A".into()),
                PinFunction::Other("FMC_A0".into()),
                PinFunction::Other("TIM1_CH1N".into()),
            ]
        );
        // Canonical round-trip through the token string.
        assert_eq!(
            functions_to_string(&fns),
            "in out af:sai1_sd_a af:fmc_a0 af:tim1_ch1n"
        );
        assert_eq!(parse_functions(&functions_to_string(&fns)), fns);
        // Generic AFs are never "unknown"…
        assert!(unknown_function_tokens("af:dcmi_d3 af:eth_mdio").is_empty());
        // …but a real typo still is (the prefix keeps validation honest).
        assert_eq!(
            unknown_function_tokens("uart1_tx spi9_bad af:"),
            vec!["uart1_tx", "spi9_bad", "af:"]
        );
        // The label round-trips through codegen comments too.
        let f = PinFunction::Other("SAI1_SD_A".into());
        assert_eq!(f.label(), "SAI1_SD_A");
        assert_eq!(PinFunction::from_label(&f.label()), Some(f));
    }

    /// Moving a pin across sides and positioning it within a side — the pin
    /// keeps its number (the side is layout, not identity).
    #[test]
    fn move_and_reorder_pins() {
        let mut f = McuForm::blank();
        // pins = [top, bottom, left, right]
        f.pins[1] = gpio_bank("PB", 1, 3); // bottom: PB0 PB1 PB2
        f.pins[3] = gpio_bank("PC", 10, 1); // right:  PC0

        // Bottom → Right (the user's case): appended at the end, number kept.
        assert!(f.move_pin(1, 1, 3)); // PB1
        assert_eq!(names(&f.pins[1]), vec!["PB0", "PB2"]);
        assert_eq!(names(&f.pins[3]), vec!["PC0", "PB1"]);
        assert_eq!(f.pins[3][1].number, "2", "package number is untouched");

        // Position it within the side.
        assert!(f.reorder_pin(3, 1, -1));
        assert_eq!(names(&f.pins[3]), vec!["PB1", "PC0"]);
        // Clamped at the ends — no wrap-around, no panic.
        assert!(!f.reorder_pin(3, 0, -1));
        assert!(!f.reorder_pin(3, 1, 1));
        assert_eq!(names(&f.pins[3]), vec!["PB1", "PC0"]);

        // Guards: same side, out-of-range index, out-of-range side.
        assert!(!f.move_pin(1, 0, 1));
        assert!(!f.move_pin(1, 99, 3));
        assert!(!f.move_pin(1, 0, 9));
        assert!(!f.reorder_pin(9, 0, 1));
        assert!(!f.reorder_pin(1, 99, 1));
    }

    fn names(rows: &[PinRow]) -> Vec<&str> {
        rows.iter().map(|r| r.name.as_str()).collect()
    }

    /// A chip's family picks its clock tree, and that tree round-trips to a
    /// graph whose codegen the family dispatch recognises. Guards the "imported
    /// chip gets a working clock automatically" path (recommendation b).
    #[test]
    fn for_family_maps_to_a_dispatchable_clock_tree() {
        use crate::panels::mcu_module::clock::graph::{is_g0_graph, is_g4_graph};
        use crate::panels::mcu_module::mcu_def::ClockDef;

        assert_eq!(ClockChoice::for_family("stm32g4"), ClockChoice::Stm32g4);
        assert_eq!(ClockChoice::for_family("stm32g0"), ClockChoice::Stm32g0);
        // The f247 families share one TOPOLOGY, but the F2 gets its own choice:
        // its PLLN window and clock ceilings differ, and mapping it onto the F4
        // tree is what generated an uncompilable `PllMul::MUL144`.
        assert_eq!(ClockChoice::for_family("stm32f7"), ClockChoice::Stm32f4);
        assert_eq!(ClockChoice::for_family("stm32f2"), ClockChoice::Stm32f2);
        // A family with no tree yet → None (reset-default clock, still compiles).
        assert_eq!(ClockChoice::for_family("stm32h7"), ClockChoice::None);

        // End-to-end: a G4 choice builds a graph the codegen recognises as G4.
        match ClockChoice::Stm32g4.to_def() {
            ClockDef::Graph(gc) => assert!(is_g4_graph(&gc.graph) && !is_g0_graph(&gc.graph)),
            _ => panic!("G4 choice must build a graph clock"),
        }
    }

    /// The GUI order must be a real permutation of the four sides — a typo
    /// would silently hide one editor and show another twice.
    #[test]
    fn side_display_order_is_left_bottom_right_top() {
        let shown: Vec<&str> = SIDE_DISPLAY_ORDER.iter().map(|&i| SIDES[i]).collect();
        assert_eq!(shown, vec!["Left", "Bottom", "Right", "Top"]);
        let mut sorted = SIDE_DISPLAY_ORDER;
        sorted.sort_unstable();
        assert_eq!(sorted, [0, 1, 2, 3], "must cover every side exactly once");
    }

    #[test]
    fn auto_fill_identity_from_name_sets_family_cpu_target() {
        let mut f = McuForm::blank();
        f.display_name = "STM32WBA55CG".into();
        f.family.clear();
        f.cpu.clear();
        f.target.clear();
        f.probe_chip.clear();
        assert!(f.auto_fill_identity());
        assert_eq!(f.family, "stm32wba");
        assert_eq!(f.cpu, "Cortex-M33");
        assert_eq!(f.target, "thumbv8m.main-none-eabihf");
        assert_eq!(f.toolchain, ToolchainKind::RustEmbedded);
        assert_eq!(f.probe_chip, "STM32WBA55CG"); // seeded because it was empty
        // An unrecognised name changes nothing.
        let mut g = McuForm::blank();
        g.display_name = "ESP32-C3".into();
        assert!(!g.auto_fill_identity());
    }

    /// An nRF52 name fills every line the generator reads, from the chip
    /// table: the soft-float target on the small parts, the part's own HAL
    /// crates, no `nfc-pins-as-gpio` where there is no NFC, and a clock tree.
    #[test]
    fn auto_fill_completes_an_nrf52_form() {
        for (name, family, target, hal, nfc) in [
            ("nRF52810-QFAA", "nrf52810", "thumbv7em-none-eabi", "nrf52810-hal", false),
            ("nRF52820", "nrf52820", "thumbv7em-none-eabi", "embassy-nrf", false),
            ("my nrf52840 board", "nrf52840", "thumbv7em-none-eabihf", "nrf52840-hal", true),
        ] {
            let mut f = McuForm::empty();
            f.id = "x".into();
            f.display_name = name.into();
            assert!(f.auto_fill_identity(), "{name}");
            assert_eq!(f.family, family);
            assert_eq!(f.target, target);
            assert!(f.hal_dep.starts_with(hal), "{name}: {}", f.hal_dep);
            assert!(f.hal_dep_async.starts_with("embassy-nrf"), "{name}");
            assert_eq!(f.hal_dep_async.contains("nfc-pins-as-gpio"), nfc, "{name}");
            assert_eq!(f.flash_origin, "0x00000000");
            assert!(f.probe_chip.ends_with("_xxAA"), "{}", f.probe_chip);
            assert!(matches!(f.effective_clock(), ClockDef::Graph(_)), "{name}");
            let w = f.warnings();
            assert!(
                !w.iter().any(|w| w.contains("codegen backend") || w.contains("HAL")),
                "{name}: {w:?}"
            );
        }
    }

    #[test]
    fn empty_package_warns() {
        let mut f = McuForm::blank();
        f.package.clear();
        assert!(f.warnings().iter().any(|w| w.contains("Package is empty")));
        f.package = "UFQFPN48".into();
        assert!(!f.warnings().iter().any(|w| w.contains("Package is empty")));
    }

    /// A typed value that does not parse still gets the message that quotes it.
    #[test]
    fn a_malformed_memory_value_is_quoted_back() {
        let mut f = McuForm::blank();
        f.ram_size = "20 kilobytes".into();
        let errs = f.errors();
        assert!(
            errs.iter()
                .any(|e| e.contains("RAM size ('20 kilobytes') is not a valid value")),
            "{errs:?}"
        );
    }

    #[test]
    fn unknown_family_warns_but_does_not_block() {
        let mut f = McuForm::blank();
        // An nRF51: the nRF52 parts have a backend now, this one does not.
        f.id = "nrf51822".into();
        f.display_name = "nRF51822".into();
        f.probe_chip = "nRF51822_xxAA".into();
        f.family = "nrf51".into();
        assert!(f.errors().is_empty());
        assert!(
            f.warnings()
                .iter()
                .any(|w| w.contains("no codegen backend"))
        );
    }
}
