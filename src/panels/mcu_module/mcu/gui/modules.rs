//! Render virtual modules (e.g. _USART) and their wires beside the chip on the
//! Pins canvas — a simplified schematic. Read-only (add/remove is in the Pins
//! tab toolbar; config is the Module panel).

use super::super::model::{Mcu, PIN_HEIGHT};
use super::module_docs::{self as docs, ConfigOut};
use super::rotate::Rot;
use crate::panels::mcu_module::codegen;
use crate::panels::mcu_module::codegen::dma_map;
use crate::panels::mcu_module::codegen::embassy_async::is_advanced_timer;
use crate::panels::mcu_module::codegen::sanitize_label;
use crate::panels::mcu_module::modules::model::BlockingDma;
use crate::panels::mcu_module::modules::model::hz_label;
use crate::panels::mcu_module::modules::{
    ApiStyle, AsyncBusMode, BREAK_FILTERS, BreakPolarity, CanMode, HspiMode, I2cDevice,
    I2sClockPolarity, I2sDirection, I2sFormat, I2sMode, I2sStandard, LcdCamMode, ModuleConfig,
    ModuleKind, ModuleSignal, OspiMemoryType, OspiMode, Parity, ParlIoBitOrder, ParlIoDirection,
    ParlIoWidth, PcntCtrlMode, PcntEdgeMode, PwmCounting, PwmMode, PwmOutput, PwmPolarity,
    QSPI_MEMORY_SIZES, QspiAddressSize, RmtDirection, SaiDataSize, SaiMode, SaiStereoMono, SaiTxRx,
    SpiBitOrder, SpiRole, StopBits, TouchScan, TouchThreshold, UsartDirection, UsartFlow,
    UsartMode, UsartModuleConfig, UsbRole, VirtualModule, XspiMemoryType, XspiMode,
};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// The two lines under a module's title: its config summary and the variable
/// name it binds.
///
/// One colour for both. They were two slightly different greys (150,150,160 and
/// 140,140,150) - a distinction too small to read as meaning, which only made
/// the lower line look faded rather than deliberate.
const SUB_COLOUR: egui::Color32 = egui::Color32::from_rgb(220, 220, 220);
/// Sizes of those two lines. They keep their 10:9 ratio to each other, so the
/// summary still leads and the variable name still follows.
///
/// At this size the three rows nearly fill the space above the box's midline:
/// with the title at 13 they leave about 3px of air, and roughly 1px when the
/// box is SELECTED and every text grows by `SELECTED_TEXT_SCALE`. There is room
/// below (the lowest sits at ~54 of 98) if they ever need to breathe.
const SUB_SIZE: f32 = 15.0;
const HANDLE_SIZE: f32 = 13.5;

const BOX_W: f32 = 170.0;
/// Tall enough for the name, the config summary, and the rename field at the
/// bottom.
const BOX_H: f32 = 98.0;
pub(super) const BOX_GAP: f32 = 14.0;
/// Height of one pin row inside a Custom module's box (its rename field).
const CUSTOM_ROW_H: f32 = 21.0;

/// Height of a module's box. A Custom module grows by one row per pin, because
/// its pins' rename fields live INSIDE the box, grouped under the module name,
/// instead of floating separately beside the chip.
/// The six families the canvas draws, over the twenty-four kinds.
///
/// # Six and not twenty-four
///
/// A reader separates five to seven silhouettes at a glance, and the outline
/// budget here is smaller still: [`facing_terminal`] pins a wire to a rect EDGE,
/// so only the corners of the end facing AWAY from the chip may be cut, and
/// `PathShape::fill` is convex-only, which rules out notches, tabs and stepped
/// edges. What is left is a chamfer, a bevel and a rounded end - three motifs.
///
/// Grouping is not a compromise for that budget, though. The ESP-only kinds
/// (PARL_IO, LCD_CAM, Touch, MCPWM, PCNT, RMT) never share a canvas with the
/// STM32-only ones (LPUART, SAI, SDMMC, the four external-memory ports), so no
/// real project shows more than four or five of these at once. A vocabulary of
/// twenty-four marks is one nobody could see side by side long enough to learn.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum BoxShape {
    /// A bus with a handful of named lines. The plain rounded box everything
    /// else reads as "not that".
    Serial,
    /// External memory and cards: one back corner cut at 45°, the pin-1 bevel
    /// of a memory package.
    Memory,
    /// A parallel port. Taller, because it is: eight to twenty-three signals
    /// have to share one edge, and `facing_terminal` gives them the box height
    /// minus sixteen pixels to do it in.
    Parallel,
    /// Timing and drive: one pad, one waveform. A keystone - wide at the chip,
    /// narrow at the back.
    Driver,
    /// A link that leaves the board. Rounded at the back, the only non-angular
    /// member of the set.
    OffBoard,
    /// A module the user authored. Square corners, as before.
    Custom,
}

impl BoxShape {
    /// Whether this silhouette is a plain rectangle - no bevelled corner on
    /// any side.
    ///
    /// Paired with the `depth` match in [`silhouette`]: these are exactly the
    /// shapes whose depth is zero, and `a_square_shape_is_the_one_with_four_
    /// corners` holds the two together. A square box has all four corners to
    /// spend, which is why its legend can take one.
    pub fn is_square(self) -> bool {
        matches!(self, Self::Serial | Self::Custom)
    }

    pub fn of(kind: ModuleKind) -> Self {
        use ModuleKind as K;
        match kind {
            K::GenericInterfaceUsart
            | K::GenericInterfaceLpuart
            | K::GenericInterfaceSpi
            | K::GenericInterfaceI2c
            | K::GenericInterfaceI2s
            | K::GenericInterfaceSai => Self::Serial,

            K::GenericInterfaceQspi
            | K::GenericInterfaceOspi
            | K::GenericInterfaceXspi
            | K::GenericInterfaceHspi
            | K::GenericInterfaceSdmmc => Self::Memory,

            K::GenericInterfaceParlIo
            | K::GenericInterfaceParlIoRx
            | K::GenericInterfaceLcdCam
            | K::GenericInterfaceCamera => Self::Parallel,

            // Touch and PCNT sit here as the other one-pad waveform things: a
            // touch channel and an edge counter are read the same way a PWM
            // output is written.
            K::GenericInterfaceTimer
            | K::GenericInterfaceMcpwm
            | K::GenericInterfaceRmt
            | K::GenericInterfaceDac
            | K::GenericInterfaceTouch
            | K::GenericInterfacePcnt => Self::Driver,

            K::GenericInterfaceCan | K::GenericInterfaceUsb => Self::OffBoard,

            K::Custom => Self::Custom,
        }
    }

    /// Is this family drawn as a plain rectangle?
    ///
    /// The two that are keep `rect_filled` / `rect_stroke`, and with them corner
    /// ROUNDING, which is a `RectShape` property no polygon can carry.
    fn is_rect(self) -> bool {
        matches!(self, Self::Serial | Self::Custom)
    }

    /// The box height this family gets.
    ///
    /// By KIND and never by live wire count: `box_h` only sees a
    /// `&VirtualModule`, `dragged_half_extent` cannot recompute connections at
    /// all - and a box that changes size while you are wiring it is worse than
    /// no cue.
    fn height(self) -> f32 {
        match self {
            // Room for the signals, and the tallest thing on the canvas.
            Self::Parallel => 130.0,
            // One pad and a short summary; the keystone needs the height gone
            // anyway, or the cut eats the text.
            Self::Driver | Self::OffBoard => 78.0,
            Self::Serial | Self::Memory | Self::Custom => BOX_H,
        }
    }
}

fn box_h(m: &VirtualModule) -> f32 {
    if m.kind.is_custom() {
        let n = match &m.config {
            ModuleConfig::Custom(c) => c.pins.len(),
            _ => 0,
        };
        BoxShape::Custom.height() + n as f32 * CUSTOM_ROW_H
    } else {
        BoxShape::of(m.kind).height()
    }
}

/// The tallest box any module can be, before a Custom module's pin rows.
///
/// What [`MARGIN_Y`] has to reserve: it used to name `BOX_H`, which was every
/// box's height and is now only the middle tier.
const BOX_H_MAX: f32 = 130.0;

/// Every pin whose NAME belongs to a virtual module, so `io_arrows` must not
/// also float a rename field for it beside the chip.
///
/// Two different modules land a pin here, for two different reasons:
///
/// * a **Custom** module draws its pins' rename fields INSIDE its own box,
///   grouped under the module name;
/// * a **Timer** module owns the whole PWM handle (`_pwm1`), and its channel
///   pads are handed straight to that handle's `init`. On embassy they never
///   become a binding at all (`consumed` in the async backend), and on the F1
///   they name a variable `pwm_hz` moves out on the next line — so a field
///   beside the chip would edit a name the user cannot use either way, next to
///   the module's own field, which is the one that counts.
pub fn module_owned_pins(mcu: &Mcu) -> std::collections::HashSet<usize> {
    let mut owned: std::collections::HashSet<usize> = mcu
        .modules
        .iter()
        .filter(|m| m.kind.is_custom())
        .flat_map(|m| match &m.config {
            ModuleConfig::Custom(c) => c.pins.clone(),
            _ => Vec::new(),
        })
        .collect();
    owned.extend(
        mcu.modules
            .iter()
            .filter(|m| m.kind == ModuleKind::GenericInterfaceTimer)
            .flat_map(|m| m.connections.iter().map(|c| c.mcu_pin)),
    );
    owned
}

/// Module path stem of a Custom module's generated file — `custom_<name>` at
/// revision 0, `custom_<name>_<n>` after the n-th **Update**. Every Update lands
/// in a FRESH file; only the current stem is declared in `configs/mod.rs` and
/// called from main.rs, so older revisions sit on disk as history without ever
/// producing a duplicate struct in the build.
pub fn custom_file_stem(m: &VirtualModule) -> String {
    let var = custom_var_name(m);
    match &m.config {
        ModuleConfig::Custom(c) if c.revision > 0 => format!("custom_{var}_{}", c.revision),
        _ => format!("custom_{var}"),
    }
}

/// The prefix every revision of a Custom module's file shares — used to keep the
/// older ones when the project tree prunes config files.
pub fn custom_file_prefix(m: &VirtualModule) -> String {
    format!("custom_{}", custom_var_name(m))
}

/// Fingerprint of a Custom module's pin list — each pin's number plus its
/// `function|label`. Compared against `applied_sig` to decide whether **Update**
/// has anything to do: renaming a pin or flipping it In→Out changes the
/// generated field names just as much as adding one does.
pub fn custom_pins_sig(pins: &[usize], pin_sigs: &HashMap<usize, String>) -> String {
    pins.iter()
        .map(|n| {
            let s = pin_sigs.get(n).map(String::as_str).unwrap_or("?");
            format!("{n}:{s}")
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Width reserved for a Custom module's field labels ("Name:", "Struct"), so the
/// boxes beside them start on one vertical line. Wide enough for the longest of
/// them in bold.
const CUSTOM_LABEL_W: f32 = 52.0;
/// Width of those fields — the same for Name and Struct, so they also END on one
/// line.
pub const CUSTOM_FIELD_W: f32 = 160.0;

/// One pin edit a module's config panel asks the caller to make.
///
/// A plain `(pin, function)` could only ever SET, and half of what this panel
/// needs is a MOVE - "this signal is on the wrong pad, put it on that one".
/// Expressed as one value rather than two out-parameters so the caller cannot
/// be handed both in a frame and silently apply one.
#[derive(Clone, Debug, PartialEq)]
pub enum PinEdit {
    /// Give this pad this function, through `apply_pin_function` - partners and
    /// labels follow, exactly as if the pad had been clicked on the canvas.
    Set(usize, PinFunction),
    /// Carry the function on `from` over to `to`, through `move_pin_function`.
    /// Not two `Set`s: see that method for why either order breaks.
    Move { from: usize, to: usize },
}

/// Why an RP shows no DMA channel picker.
///
/// On this chip the channel is not a decision anyone can get right or wrong:
/// every channel serves every peripheral (the driver writes TREQ_SEL into
/// whichever one it was handed) and they all raise DMA_IRQ_0. The picker could
/// not be SET here in any case — a Pico carries no vendor `DmaDef`, so
/// `channels_for` returns nothing and the combo drew itself disabled with a
/// hover telling a Raspberry Pi owner to "re-import it from the STM32Cube
/// database". That was the most actively wrong string on the panel.
///
/// A remark, not a control, so it belongs in the details pane rather than in a
/// grid row wide enough to hold the config column open. ONE source line: a
/// `\`-continued literal comes back joined to its own indentation, and the
/// panel would draw the run of spaces.
pub const RP_DMA_NOTE: &str = "embassy-rp allocates DMA itself - any channel serves any peripheral, and the Configuration tab's DMA card shows which ones were taken";

/// Why an RP shows no I2C transport choice.
///
/// Not "unimplemented": embassy-rp's `i2c.rs` contains no DMA at all, and
/// `new_async` takes no channel. Offering Blocking / Async-DMA here would teach
/// a hardware model that is not true of this chip.
pub const RP_I2C_NOTE: &str = "embassy-rp's async I2C is interrupt driven and takes no DMA channel, so there is no transport to choose";

/// The SPI init style, locked on an RP, for the same reason.
///
/// `Spi::new_blocking` exists in embassy-rp; this backend only emits the
/// DMA form. The choice is real and unbuilt, not absent.
fn rp_spi_init_locked(ui: &mut egui::Ui, out: &mut ConfigOut) {
    out.field("Async init", docs::ASYNC_INIT_LOCKED_RP);
    ui.label("Async init");
    let resp = ui.add_enabled_ui(false, |ui| {
        egui::ComboBox::from_id_salt("rp_spi_init_locked")
            .selected_text("Async-DMA (embedded-hal-async)")
            .show_ui(ui, |_ui| {});
    });
    resp.response.on_hover_text(
        "embassy-rp has Spi::new_blocking, but this backend emits only the DMA form - so choosing Blocking here would change nothing.",
    );
    ui.end_row();
}

/// A SPIM and a TWIM on one nRF52 block: the row in the grid and the standing
/// note, on whichever of the two is being drawn.
///
/// `partner` is the other module's peripheral (`TWIM0` under a SPI, `SPIM0`
/// under an I2C), found by `nrf::shared_block_partner` in the caller: this
/// panel sees one module, so the pair is a fact it has to be told, like
/// `line_extras`. What `main.rs` does with the pair differs by runtime, and
/// the sentence says which; both readings are the nRF backend's own
/// (`async_bus_lines` skips the TWIM, `bus_lines` builds both).
///
/// The grid gets the SHORT line, with the full sentence on hover and in the
/// details pane: nothing in this grid wraps, and the full sentence was
/// clipped at the panel's edge before the part that says which bus loses.
fn nrf_shared_block_row(
    ui: &mut egui::Ui,
    out: &mut ConfigOut,
    me: &str,
    partner: &str,
    instance: u8,
    is_async: bool,
) {
    let (short, full) = nrf_shared_block_text(me, partner, instance, is_async);
    out.note(full.clone());
    ui.label("");
    ui.label(
        egui::RichText::new(short)
            .size(10.5)
            .color(egui::Color32::from_rgb(220, 160, 70)),
    )
    .on_hover_text(full);
    ui.end_row();
}

/// `(grid line, full sentence)` for [`nrf_shared_block_row`]. Apart from the
/// drawing so the tests can hold the grid line to a width and to the runtime.
fn nrf_shared_block_text(
    me: &str,
    partner: &str,
    instance: u8,
    is_async: bool,
) -> (String, String) {
    let (spim, twim) = if me.starts_with("SPIM") {
        (me, partner)
    } else {
        (partner, me)
    };
    let lost = if is_async {
        format!("{twim} is not built")
    } else {
        format!("{spim} is switched off")
    };
    let short = format!("Shares TWISPI{instance} with {partner}: {lost}");
    let what = if is_async {
        format!("main.rs builds {spim} and leaves {twim} a comment where its init would be")
    } else {
        format!(
            "main.rs builds both, and {twim}'s init, which runs second, writes the block's \
             ENABLE register and switches {spim} off"
        )
    };
    let text = format!(
        "{me} and {partner} are one hardware block on the nRF52 (embassy-nrf calls it \
         TWISPI{instance}), and the chip runs one of them at a time. Both modules are on \
         it: {what}. Move one bus to another instance on the Pins canvas; SPIM2 has no \
         TWIM twin."
    );
    (short, text)
}

/// The left column of a Custom module's `label + field` row: bold, left-aligned,
/// fixed width. `Name:` lives in the module list (`mcu_panel`) and `Struct` in
/// the config grid here — two different containers, so only a shared width keeps
/// their fields aligned.
pub fn custom_field_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(CUSTOM_LABEL_W, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(egui::RichText::new(text).strong());
        },
    );
}

/// Rect of the rename field for the `i`-th pin row inside a Custom box.
fn custom_pin_row(box_rect: egui::Rect, i: usize) -> egui::Rect {
    let top = box_rect.top() + 44.0 + i as f32 * CUSTOM_ROW_H;
    egui::Rect::from_min_max(
        egui::pos2(box_rect.left() + 52.0, top),
        egui::pos2(box_rect.right() - 8.0, top + CUSTOM_ROW_H - 4.0),
    )
}
/// Gap between the pin tips and a module box.
pub(super) const PIN_GAP: f32 = 18.0;
/// Canvas margin reserved around the chip for modules (so boxes + wires sit
/// beyond the pins without overlapping the chip). Horizontal fits a box's width,
/// vertical its height.
pub const MARGIN_X: f32 = PIN_HEIGHT + PIN_GAP + BOX_W + 24.0;
pub const MARGIN_Y: f32 = PIN_HEIGHT + PIN_GAP + BOX_H_MAX + 24.0;

/// Which side of the chip a pin sits on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Side {
    Right,
    Left,
    Top,
    Bottom,
}

/// Half-extents (from the chip centre) needed to keep every DRAGGED module box
/// fully on-canvas — so `Mcu::draw` can grow the painter and the Scene's
/// auto-fit won't clip a box pulled far from the chip. `(0,0)` when nothing is
/// manually placed. A module `pos` is the box top-left offset from the centre.
pub fn dragged_half_extent(mcu: &Mcu) -> egui::Vec2 {
    let mut hx = 0.0_f32;
    let mut hy = 0.0_f32;
    for m in &mcu.modules {
        if m.pos != (0.0, 0.0) {
            hx = hx.max(m.pos.0.abs()).max((m.pos.0 + BOX_W).abs());
            hy = hy.max(m.pos.1.abs()).max((m.pos.1 + box_h(m)).abs());
        }
    }
    egui::vec2(hx, hy)
}

/// Outer connection point of an MCU pin (rotation applied). `None` if the pin
/// isn't on this chip. `chip_rect` is the LOCAL (un-rotated) body; `rot` is the
/// diagram rotation.
pub fn pin_anchor(
    mcu: &Mcu,
    chip_rect: egui::Rect,
    rot: Rot,
    pin_num: usize,
) -> Option<egui::Pos2> {
    pin_anchor_side(mcu, chip_rect, rot, pin_num).map(|(p, _)| p)
}

/// Outer connection point + the unit vector pointing **away** from the chip
/// (rotation applied — a true 45° direction on a diamond). Used to draw in/out
/// arrows straight out past the pin.
pub fn pin_anchor_dir(
    mcu: &Mcu,
    chip_rect: egui::Rect,
    rot: Rot,
    pin_num: usize,
) -> Option<(egui::Pos2, egui::Vec2)> {
    pin_anchor_local(mcu, chip_rect, pin_num)
        .map(|(p, out)| (rot.apply(p), rot.vec(out).normalized()))
}

/// Outer connection point + its **screen** side — the rotated outward direction
/// snapped to the nearest axis, used to place module boxes on a chip edge.
fn pin_anchor_side(
    mcu: &Mcu,
    chip_rect: egui::Rect,
    rot: Rot,
    pin_num: usize,
) -> Option<(egui::Pos2, Side)> {
    pin_anchor_local(mcu, chip_rect, pin_num)
        .map(|(p, out)| (rot.apply(p), side_from_outward(rot.vec(out))))
}

/// LOCAL (un-rotated) outer point + unit outward vector of a pin. `None` if the
/// pin isn't on this chip.
fn pin_anchor_local(
    mcu: &Mcu,
    chip_rect: egui::Rect,
    pin_num: usize,
) -> Option<(egui::Pos2, egui::Vec2)> {
    // The same placement the chip is drawn from — a wire that computed its own
    // would land beside the pin the moment either formula moved.
    super::geometry::pin_geom(mcu, chip_rect, pin_num).map(|g| (g.anchor(), g.outward))
}

/// A fixed-size arrowhead at `to`, pointing along `from → to`. Used on custom
/// module wires to show which way the data flows (the line itself is drawn by
/// the caller). Fixed size — a long wire must not grow a huge head.
fn arrow_head(painter: &egui::Painter, from: egui::Pos2, to: egui::Pos2, color: egui::Color32) {
    let v = to - from;
    if v.length() < 1.0 {
        return;
    }
    let dir = v.normalized();
    let rot = egui::emath::Rot2::from_angle(std::f32::consts::TAU / 12.0);
    let stroke = egui::Stroke::new(1.6_f32, color);
    painter.line_segment([to, to - 7.0 * (rot * dir)], stroke);
    painter.line_segment([to, to - 7.0 * (rot.inverse() * dir)], stroke);
}

/// Snap a (rotated) outward vector to the nearest chip side.
fn side_from_outward(v: egui::Vec2) -> Side {
    if v.x.abs() >= v.y.abs() {
        if v.x >= 0.0 { Side::Right } else { Side::Left }
    } else if v.y >= 0.0 {
        Side::Bottom
    } else {
        Side::Top
    }
}

/// Wire/terminal colour for a module signal — the **same colour as the MCU pin**
/// it connects to (so the schematic matches the pin colours on the chip).
fn signal_color(sig: ModuleSignal, instance: u8) -> egui::Color32 {
    sig.pin_function(instance).color()
}

/// The module's representative colour = the peripheral's pin colour (USART/SPI/
/// I2C category — instance-independent), used for the box border + title so it
/// The RMT block's source clock, in Hz.
///
/// Not from the Clock tab: esp-hal takes the rate as an argument to `Rmt::new`
/// and every example passes the same one per chip — 32 MHz on the ESP32-H2,
/// 80 MHz everywhere else. It is here so the module can show what one tick
/// actually lasts, which is the number a pulse train is written in.
pub fn rmt_source_hz(family: &str) -> u32 {
    if family == "esp32h2" {
        32_000_000
    } else {
        80_000_000
    }
}

/// matches its pins, plus the list-entry name + the "already added" palette
/// buttons.
pub fn module_color(kind: ModuleKind, instance: u8) -> egui::Color32 {
    let f = match kind {
        ModuleKind::GenericInterfaceUsart => PinFunction::UsartTx(instance),
        ModuleKind::GenericInterfaceLpuart => PinFunction::LpuartTx(instance),
        ModuleKind::GenericInterfaceSpi => PinFunction::SpiSck(instance),
        ModuleKind::GenericInterfaceI2c => PinFunction::I2cScl(instance),
        ModuleKind::GenericInterfaceI2s => PinFunction::I2sCk(instance),
        ModuleKind::GenericInterfaceRmt => PinFunction::RmtChannel(instance),
        ModuleKind::GenericInterfacePcnt => PinFunction::PcntEdge {
            unit: instance,
            channel: 0,
        },
        ModuleKind::GenericInterfaceParlIo => PinFunction::ParlData { lane: 0 },
        ModuleKind::GenericInterfaceParlIoRx => PinFunction::ParlRxData { lane: 0 },
        ModuleKind::GenericInterfaceLcdCam => PinFunction::LcdCamData { lane: 0 },
        ModuleKind::GenericInterfaceCamera => PinFunction::CamData { lane: 0 },
        ModuleKind::GenericInterfaceTouch => PinFunction::TouchPad(0),
        ModuleKind::GenericInterfaceMcpwm => PinFunction::McpwmA {
            unit: instance,
            operator: 0,
        },
        ModuleKind::GenericInterfaceDac => PinFunction::DacOut {
            dac: instance,
            channel: 1,
        },
        ModuleKind::GenericInterfaceSai => PinFunction::SaiSck {
            sai: instance,
            block: 1,
        },
        ModuleKind::GenericInterfaceSdmmc => PinFunction::SdmmcCk { unit: instance },
        ModuleKind::GenericInterfaceQspi => PinFunction::QspiClk,
        ModuleKind::GenericInterfaceOspi => PinFunction::OspiClk { port: instance },
        ModuleKind::GenericInterfaceXspi => PinFunction::XspiClk { port: instance },
        ModuleKind::GenericInterfaceHspi => PinFunction::HspiClk { unit: instance },
        ModuleKind::GenericInterfaceTimer => PinFunction::TimerPwm {
            timer: instance,
            channel: 1,
        },
        ModuleKind::GenericInterfaceCan => PinFunction::CanTx,
        ModuleKind::GenericInterfaceUsb => PinFunction::UsbDp,
        // A custom module has no peripheral colour of its own — a neutral slate
        // reads as "user-authored", matching its square corners.
        ModuleKind::Custom => return egui::Color32::from_rgb(150, 160, 180),
    };
    f.color()
}

/// Place a box just beyond `side`, centred on `along` (the pins' centroid along
/// the side axis) but **nudged forward** past `cursor` so same-side boxes never
/// overlap. `cursor` tracks the trailing edge of the previously placed box on
/// this side and is advanced here. Call with boxes pre-sorted by `along`.
fn packed_rect(
    chip_rect: egui::Rect,
    side: Side,
    along: f32,
    cursor: &mut f32,
    h: f32,
) -> egui::Rect {
    match side {
        Side::Right | Side::Left => {
            let half = h / 2.0;
            let mut cy = along;
            if cy - half < *cursor + BOX_GAP {
                cy = *cursor + BOX_GAP + half;
            }
            *cursor = cy + half;
            let x = if side == Side::Right {
                chip_rect.right() + PIN_HEIGHT + PIN_GAP
            } else {
                chip_rect.left() - PIN_HEIGHT - PIN_GAP - BOX_W
            };
            egui::Rect::from_min_size(egui::pos2(x, cy - half), egui::vec2(BOX_W, h))
        }
        Side::Top | Side::Bottom => {
            let half = BOX_W / 2.0;
            let mut cx = along;
            if cx - half < *cursor + BOX_GAP {
                cx = *cursor + BOX_GAP + half;
            }
            *cursor = cx + half;
            let y = if side == Side::Top {
                chip_rect.top() - PIN_HEIGHT - PIN_GAP - h
            } else {
                chip_rect.bottom() + PIN_HEIGHT + PIN_GAP
            };
            egui::Rect::from_min_size(egui::pos2(cx - half, y), egui::vec2(BOX_W, h))
        }
    }
}

/// A terminal point on the box edge that faces the chip, aligned to `anchor`
/// (clamped to the edge), so the wire to the pin is short and doesn't cross the
/// chip.
/// How far a wire terminal is kept from a box's corners.
///
/// `facing_terminal` has always had this margin; `nearest_edge` had none, which
/// was harmless while every box was a plain rectangle and is not once a corner
/// can be cut away. A terminal landing in the eight pixels a chamfer removes
/// would sit in empty canvas with its wire pointing at nothing.
const CORNER_INSET: f32 = 8.0;

fn facing_terminal(box_rect: egui::Rect, side: Side, anchor: egui::Pos2) -> egui::Pos2 {
    match side {
        Side::Right => egui::pos2(
            box_rect.left(),
            anchor.y.clamp(
                box_rect.top() + CORNER_INSET,
                box_rect.bottom() - CORNER_INSET,
            ),
        ),
        Side::Left => egui::pos2(
            box_rect.right(),
            anchor.y.clamp(
                box_rect.top() + CORNER_INSET,
                box_rect.bottom() - CORNER_INSET,
            ),
        ),
        Side::Top => egui::pos2(
            anchor.x.clamp(
                box_rect.left() + CORNER_INSET,
                box_rect.right() - CORNER_INSET,
            ),
            box_rect.bottom(),
        ),
        Side::Bottom => egui::pos2(
            anchor.x.clamp(
                box_rect.left() + CORNER_INSET,
                box_rect.right() - CORNER_INSET,
            ),
            box_rect.top(),
        ),
    }
}

/// Nearest point on a box's edge to `target` — the wire terminal for a
/// user-dragged (manually placed) box, whose original `side` no longer implies
/// which edge faces the chip. Clamps `target` into the rect: an anchor outside
/// the box lands on its boundary.
pub fn nearest_edge(rect: egui::Rect, target: egui::Pos2) -> egui::Pos2 {
    egui::pos2(
        target.x.clamp(rect.left(), rect.right()),
        target.y.clamp(rect.top(), rect.bottom()),
    )
}

/// Nearest point on a closed polygon's boundary to `target`.
///
/// What a dragged module box needs, and what a rect clamp cannot give once a
/// corner is cut away: [`nearest_edge`] would happily return a point inside the
/// chamfer, leaving the terminal dot and its wire in empty canvas beside the
/// outline. An inset would have been an approximation - the cut is up to 42 % of
/// an edge, not eight pixels - so this walks the real outline instead.
///
/// [`nearest_edge`] stays for the pin rename fields in `io_arrows`, which are
/// plain rectangles and always will be.
pub fn nearest_on_outline(poly: &[egui::Pos2], target: egui::Pos2) -> egui::Pos2 {
    let mut best = poly[0];
    let mut best_d2 = f32::INFINITY;
    for (a, b) in poly
        .iter()
        .zip(poly.iter().cycle().skip(1))
        .take(poly.len())
    {
        let seg = *b - *a;
        let len2 = seg.length_sq();
        // A degenerate edge (two identical points) would divide by zero; its
        // endpoint is the answer anyway.
        let t = if len2 > f32::EPSILON {
            ((target - *a).dot(seg) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let p = *a + seg * t;
        let d2 = (target - p).length_sq();
        if d2 < best_d2 {
            best_d2 = d2;
            best = p;
        }
    }
    best
}

/// The outline of a module box, clockwise from the top-left corner.
///
/// # The one rule
///
/// The edge FACING THE CHIP is never touched. [`facing_terminal`] pins every
/// auto-placed wire onto that edge, so cutting it would strand terminals; every
/// shape in [`BoxShape`] therefore spends its whole budget on the two corners of
/// the opposite edge. That is also why the vocabulary is chamfers and rounds and
/// nothing else: `PathShape::fill` is convex-only, so a notch or a step would
/// stroke as a line floating inside an intact square fill.
///
/// Depth is a fraction of the BACK EDGE's length, capped at 45 % of the
/// perpendicular dimension so a shape reads the same whichever side of the chip
/// its module landed on.
fn silhouette(rect: egui::Rect, shape: BoxShape, side: Side) -> Vec<egui::Pos2> {
    let (tl, tr) = (rect.left_top(), rect.right_top());
    let (br, bl) = (rect.right_bottom(), rect.left_bottom());
    let corners = [tl, tr, br, bl];

    // Which two corners belong to the edge pointing AWAY from the chip. `side`
    // is the chip side the box sits on, so `Side::Right` means the box is to the
    // right and its LEFT edge faces the chip.
    let back: [usize; 2] = match side {
        Side::Right => [1, 2],
        Side::Left => [3, 0],
        Side::Top => [0, 1],
        Side::Bottom => [2, 3],
    };

    let along = match side {
        Side::Right | Side::Left => rect.height(),
        Side::Top | Side::Bottom => rect.width(),
    };
    let across = match side {
        Side::Right | Side::Left => rect.width(),
        Side::Top | Side::Bottom => rect.height(),
    };
    let cut = |frac: f32| (frac * along).min(0.45 * across);

    let (depth, both, round) = match shape {
        BoxShape::Serial | BoxShape::Custom => (0.0, false, false),
        BoxShape::Memory => (cut(0.40), false, false),
        BoxShape::Parallel => (cut(0.20), true, false),
        BoxShape::Driver => (cut(0.42), true, false),
        BoxShape::OffBoard => (cut(0.35), true, true),
    };
    if depth <= 0.0 {
        return corners.to_vec();
    }

    let mut out = Vec::with_capacity(12);
    for (i, c) in corners.iter().enumerate() {
        let cut_here = i == back[0] || (both && i == back[1]);
        if !cut_here {
            out.push(*c);
            continue;
        }
        // Walk `depth` back along the edge that arrives, and `depth` forward
        // along the one that leaves.
        let prev = corners[(i + 3) % 4];
        let next = corners[(i + 1) % 4];
        let a = *c + (prev - *c).normalized() * depth;
        let b = *c + (next - *c).normalized() * depth;
        if round {
            // A quarter arc, as five segments. Enough that it reads as a curve
            // at 400 % and cheap enough not to matter at any zoom.
            const STEPS: usize = 5;
            for k in 0..=STEPS {
                let t = k as f32 / STEPS as f32;
                // Quadratic Bezier with the corner as its control point - the
                // standard rounded corner, and it stays convex.
                let u = 1.0 - t;
                out.push(
                    ((a.to_vec2() * (u * u))
                        + (c.to_vec2() * (2.0 * u * t))
                        + (b.to_vec2() * (t * t)))
                        .to_pos2(),
                );
            }
        } else {
            out.push(a);
            out.push(b);
        }
    }
    out
}

/// The module name without the `_` prefix (e.g. `_I2C1` → `I2C1`).
pub fn module_base_name(m: &VirtualModule) -> &str {
    m.name.strip_prefix("_").unwrap_or(&m.name)
}

/// Display title for the module list/box: the base name plus the user's **raw**
/// label, verbatim — e.g. `_I2C1` + "128x32 display" → `I2C1 - 128x32 display`.
pub fn module_title(m: &VirtualModule) -> String {
    let base = module_base_name(m);
    let label = m.config.custom_label();
    if label.trim().is_empty() {
        base.to_owned()
    } else {
        format!("{base} - {label}")
    }
}

/// The binding name(s) this module generates, as a list.
///
/// [`handle_preview`] renders them for the box, where one string is what a
/// painter wants. Callers that need to FIND those bindings in the source want
/// them apart: a Native USART is two handles (`_tx0, _rx0`), and searching for
/// the joined string would match neither line.
pub(crate) fn handle_names(m: &VirtualModule, native_forced: bool) -> Vec<String> {
    handle_preview(m, native_forced)
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Live preview of the generated handle variable name(s), with the user's
/// label appended — the module analogue of the pin's `pc13_out_board_led`
/// caption. SPI/I2C have one handle; USART has two (`_txN` / `_rxN`).
fn handle_preview(m: &VirtualModule, native_forced: bool) -> String {
    let n = m.instance();
    let lbl = sanitize_label(m.config.custom_label());
    let sfx = if lbl.is_empty() {
        String::new()
    } else {
        format!("_{lbl}")
    };
    match m.kind {
        // USART: Native returns the split `(Tx, Rx)` handles → two names; Portable
        // (and the async embedded-io bridge) returns one value → `_serialN`.
        // `native_forced` is the project-level Native runtime (all peripherals
        // Native regardless of the per-module `api_style`).
        ModuleKind::GenericInterfaceUsart => {
            let native = native_forced
                || matches!(&m.config, ModuleConfig::Usart(c) if c.api_style == ApiStyle::Native);
            if native {
                format!("_tx{n}{sfx}, _rx{n}{sfx}")
            } else {
                format!("_serial{n}{sfx}")
            }
        }
        // `_lpserial`, NOT `_serial`: a chip with both USART1 and LPUART1 would
        // otherwise generate the same binding name twice.
        ModuleKind::GenericInterfaceLpuart => format!("_lpserial{n}{sfx}"),
        ModuleKind::GenericInterfaceSpi => format!("_spi{n}{sfx}"),
        ModuleKind::GenericInterfaceI2c => format!("_i2c{n}{sfx}"),
        ModuleKind::GenericInterfaceI2s => format!("_i2s{n}{sfx}"),
        ModuleKind::GenericInterfaceRmt => format!("_rmt{n}{sfx}"),
        ModuleKind::GenericInterfacePcnt => format!("_pcnt{n}{sfx}"),
        // The port is one driver, whatever the bus is wide.
        ModuleKind::GenericInterfaceParlIo => format!("_parl{sfx}"),
        ModuleKind::GenericInterfaceParlIoRx => format!("_parl_rx{sfx}"),
        ModuleKind::GenericInterfaceLcdCam => format!("_lcd{sfx}"),
        ModuleKind::GenericInterfaceCamera => format!("_cam{sfx}"),
        ModuleKind::GenericInterfaceTouch => format!("_touch{sfx}"),
        // One handle per OUTPUT: each is its own `PwmPin`, and they are set
        // independently. The list is built from what is wired, so an
        // operator with only A gives one name.
        ModuleKind::GenericInterfaceMcpwm => {
            let mut names: Vec<String> = m
                .connections
                .iter()
                .filter_map(|c| match c.signal {
                    ModuleSignal::McpwmA0 => Some(format!("_mcpwm{n}_op0a{sfx}")),
                    ModuleSignal::McpwmB0 => Some(format!("_mcpwm{n}_op0b{sfx}")),
                    ModuleSignal::McpwmA1 => Some(format!("_mcpwm{n}_op1a{sfx}")),
                    ModuleSignal::McpwmB1 => Some(format!("_mcpwm{n}_op1b{sfx}")),
                    ModuleSignal::McpwmA2 => Some(format!("_mcpwm{n}_op2a{sfx}")),
                    ModuleSignal::McpwmB2 => Some(format!("_mcpwm{n}_op2b{sfx}")),
                    _ => None,
                })
                .collect();
            names.sort();
            names.join(", ")
        }
        ModuleKind::GenericInterfaceDac => format!("_dac{n}{sfx}"),
        // One handle per SUB-BLOCK: they are independent streams.
        ModuleKind::GenericInterfaceSai => format!("_sai{n}a{sfx}, _sai{n}b{sfx}"),
        ModuleKind::GenericInterfaceSdmmc => format!("_sd{n}{sfx}"),
        // Single-instance peripheral, so no number in the handle.
        ModuleKind::GenericInterfaceQspi => format!("_qspi{sfx}"),
        ModuleKind::GenericInterfaceOspi => format!("_ospi{n}{sfx}"),
        ModuleKind::GenericInterfaceXspi => format!("_xspi{n}{sfx}"),
        ModuleKind::GenericInterfaceHspi => format!("_hspi{n}{sfx}"),
        ModuleKind::GenericInterfaceTimer => format!("_pwm{n}{sfx}"),
        ModuleKind::GenericInterfaceCan => format!("_can{n}{sfx}"),
        ModuleKind::GenericInterfaceUsb => format!("usb_dev{sfx}, serial{sfx}"),
        // The generated struct is named after the module, so the preview shows
        // the handle the user will bind: `let my_mod = MyMod::new(…)`.
        ModuleKind::Custom => {
            let name = custom_struct_name(m);
            format!("{}: {name}", custom_var_name(m))
        }
    }
}

/// Sanitised snake_case identifier for a custom module's variable, derived from
/// its name (empty / invalid → `custom{instance}`).
pub fn custom_var_name(m: &VirtualModule) -> String {
    let lbl = sanitize_label(m.config.custom_label());
    if lbl.is_empty() {
        format!("custom{}", m.instance())
    } else {
        lbl
    }
}

/// `CamelCase` struct name implied by a module label — the hint shown in the
/// Struct field while the user hasn't typed one.
pub fn derived_struct_name(label: &str, instance: u8) -> String {
    let lbl = sanitize_label(label);
    let base = if lbl.is_empty() {
        format!("custom{instance}")
    } else {
        lbl
    };
    base.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Struct name for a custom module: the user's explicit choice when set,
/// otherwise `CamelCase` of the module name (`temp sensor` → `TempSensor`).
pub fn custom_struct_name(m: &VirtualModule) -> String {
    // An explicit name wins, so renaming the module never renames the struct
    // (and the user's own `impl` blocks keep compiling).
    if let ModuleConfig::Custom(c) = &m.config {
        let explicit = sanitize_label(&c.struct_name);
        if !explicit.is_empty() {
            let mut s = String::new();
            for w in explicit.split('_').filter(|w| !w.is_empty()) {
                let mut ch = w.chars();
                if let Some(f) = ch.next() {
                    s.push_str(&f.to_uppercase().collect::<String>());
                    s.push_str(ch.as_str());
                }
            }
            return s;
        }
    }
    let var = custom_var_name(m);
    var.split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// `base` moved `t` of the way towards `towards`.
///
/// Not `Color32::lerp` — egui has none — and deliberately not blending the
/// alpha: both ends are opaque here and a premultiplied blend would darken the
/// result instead of colouring it.
fn tint(base: egui::Color32, towards: egui::Color32, t: f32) -> egui::Color32 {
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    egui::Color32::from_rgb(
        mix(base.r(), towards.r()),
        mix(base.g(), towards.g()),
        mix(base.b(), towards.b()),
    )
}

fn draw_box(
    painter: &egui::Painter,
    rect: egui::Rect,
    m: &VirtualModule,
    // Which side of the chip this box sits on. Needed only for the outline: the
    // edge facing the chip is the one that must stay square, and it is the
    // opposite of this.
    side: Side,
    connected: bool,
    color: egui::Color32,
    native_forced: bool,
    // `Some(t)` (t in 0..1) while this module is pending deletion — its box
    // background pulses red so it's obvious on the diagram which one is being
    // removed. `None` = normal.
    removing_blink: Option<f32>,
    // The box the user clicked last: white title and a border twice as thick,
    // matching how a selected pin is called out on the chip. EVERY text in the
    // box grows by `SELECTED_TEXT_SCALE`, not just the title.
    selected: bool,
) -> egui::Rect {
    // Background: the panel dark, tinted towards the module's own colour, or a
    // red pulse while the remove-confirm for this module is open.
    //
    // # Why the fill and not something cleverer
    //
    // The fill was `rgb(38, 42, 50)` for all twenty-four kinds, so the LARGEST
    // area of the box carried no information at all: the colour lived only on
    // the 1.4 px border and the title. Both of those die on zoom-out — a stroke
    // is scaled before tessellation, so at 25 % the border is a third of a pixel
    // and the 13 px title is three. Area is the one channel that survives, and
    // it was the one going unused.
    //
    // Sixteen percent, not more: the box must still read as part of the diagram
    // rather than as a coloured card, and the borders of two modules of the same
    // kind still have to be told apart from their fills.
    //
    // Only when CONNECTED. A module with no pins has no peripheral colour to
    // claim — it is drawn in the muted red the border and title already use.
    let fill = match removing_blink {
        Some(t) => {
            let lerp = |a: u8, c: u8| (a as f32 + (c as f32 - a as f32) * t).round() as u8;
            egui::Color32::from_rgb(lerp(38, 190), lerp(42, 45), lerp(50, 45))
        }
        None if connected => tint(egui::Color32::from_rgb(38, 42, 50), color, 0.16),
        None => egui::Color32::from_rgb(38, 42, 50),
    };
    // SQUARE corners still mark a user-authored (Custom) module; the peripheral
    // families stay rounded, and four of the six also cut the corners of the
    // edge pointing away from the chip - see `BoxShape` and `silhouette`.
    let shape = BoxShape::of(m.kind);
    let radius = if m.kind.is_custom() { 0.0 } else { 6.0 };
    let outline = silhouette(rect, shape, side);
    // A rectangle keeps `rect_filled`, which a path cannot match: corner
    // ROUNDING is a `RectShape` property and there is no rounded-polygon shape.
    // The cut families have no rounding left to lose.
    if shape.is_rect() {
        painter.rect_filled(rect, radius, fill);
    } else {
        painter.add(egui::Shape::convex_polygon(
            outline.clone(),
            fill,
            egui::Stroke::NONE,
        ));
    }
    // A pending removal outranks the selection: the box is about to disappear,
    // which is the more urgent thing to say about it.
    let stroke = if removing_blink.is_some() {
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(235, 70, 70)) // pending removal
    } else if selected {
        egui::Stroke::new(2.8_f32, egui::Color32::WHITE) // 2× the connected border
    } else if connected {
        egui::Stroke::new(1.4_f32, color) // border matches the pin colour
    } else {
        egui::Stroke::new(1.2_f32, egui::Color32::from_rgb(120, 90, 90)) // disconnected
    };
    if shape.is_rect() {
        painter.rect_stroke(rect, radius, stroke, egui::StrokeKind::Middle);
    } else {
        // `closed_line` strokes centred on the path, which is what
        // `StrokeKind::Middle` does for the rect - so the border keeps its
        // weight and the two families sit at the same visual depth.
        painter.add(egui::Shape::closed_line(outline, stroke));
    }

    const TITLE_SIZE: f32 = 13.0;
    let scale = text_scale(selected);
    let title_color = if selected {
        egui::Color32::WHITE
    } else if connected {
        color
    } else {
        egui::Color32::from_rgb(175, 150, 150)
    };
    // A PWM module carries a thumbnail of what it makes, in the corner - the
    // same shape the config panel draws, at a size that fits beside the title.
    // Only this kind: every other module's output is a bus, and a bus has no
    // one waveform to draw.
    // Keyed on the KIND, not on the config: Camera and LCD_CAM share one config
    // variant, and so do PARL_IO and PARL RX, so a `matches!(m.config, ..)` gate
    // would silently give one kind its sibling's picture.
    //
    // The title is measured rather than assumed. It is centred on the box, and
    // `PWM0` happens to be short - but `LPUART1` and `SDMMC1` are not, and a
    // legend pinned to the corner would print through them. When there is no
    // room the box simply goes without; the config card still carries it.
    if let Some(r) = box_legend_rect(rect, shape)
        && let Some(l) = legend_of(m.kind, r)
    {
        let grey = if connected {
            LEGEND_GREY
        } else {
            LEGEND_GREY_OFF
        };
        paint_legend(&painter.with_clip_rect(rect), &l, grey);
    }
    let title_size = TITLE_SIZE * scale;
    // Kept: returned, so the caller can put the notes glyph beside it.
    let title_rect = painter.text(
        rect.center_top() + egui::vec2(0.0, 13.0),
        egui::Align2::CENTER_CENTER,
        module_base_name(m),
        egui::FontId::proportional(title_size),
        title_color,
    );
    let summary = if connected {
        m.config.summary()
    } else {
        "disconnected".to_owned()
    };
    painter.text(
        rect.center_top() + egui::vec2(0.0, 30.0),
        egui::Align2::CENTER_CENTER,
        summary,
        egui::FontId::proportional(SUB_SIZE * scale),
        SUB_COLOUR,
    );
    // The resulting variable name(s), and ONLY those.
    //
    // The box used to carry the user's own name for the module on a second
    // line below this one. It was the same string twice: the label is already
    // inside the variable name (`_pwm0_power_led` IS "power led"), so the row
    // repeated it in prose and cost the box a line to do it. The name is edited
    // and read in the module's `Name:` row in the Virtual-modules panel, which
    // has the room for it.
    //
    // Centred with the title and the summary, so the box reads as one column
    // rather than two things stacked left and centre. Clipped to the box, so a
    // long label cannot spill over the border.
    painter.with_clip_rect(rect).text(
        handle_caption_pos(m, rect),
        egui::Align2::CENTER_CENTER,
        handle_preview(m, native_forced),
        egui::FontId::proportional(HANDLE_SIZE * scale),
        SUB_COLOUR,
    );
    title_rect
}

/// Where a box's variable-name caption sits.
///
/// Under the summary for every ordinary module - which is where the eye goes
/// after the title, and the box's lower half is empty now that the name row is
/// gone.
///
/// A CUSTOM module is the exception: its pin rows start at `top + 44`
/// ([`custom_pin_row`]) and grow the box downwards, so the same offset would
/// print the caption straight through the first of them. There it stays at the
/// bottom, below the rows.
fn handle_caption_pos(m: &VirtualModule, rect: egui::Rect) -> egui::Pos2 {
    if m.kind.is_custom() {
        egui::pos2(rect.center().x, rect.bottom() - 14.0)
    } else {
        rect.center_top() + egui::vec2(0.0, 47.0)
    }
}

/// One module kind's signal legend: what the peripheral puts on the wire.
///
/// Two colour groups and nothing else. `signal` is the waveform itself, drawn
/// in the same grey wherever it appears; `accent` is the ONE thing the picture
/// exists to point at - the average a PWM holds, the threshold a touch pad
/// crosses, the count an edge counter keeps. A third colour would stop the set
/// reading as one vocabulary, and using the module's own colour was already
/// tried and reverted.
///
/// Each row is its OWN polyline. Joining two rows into one vector strokes a
/// spurious vertical connector between them, which is the two-row version of
/// the bug that turned the first PWM wave into sawteeth.
struct Legend {
    signal: Vec<Vec<egui::Pos2>>,
    accent: Vec<Vec<egui::Pos2>>,
}

impl Legend {
    fn new() -> Self {
        Self {
            signal: Vec::new(),
            accent: Vec::new(),
        }
    }
    fn signal(mut self, row: Vec<egui::Pos2>) -> Self {
        self.signal.push(row);
        self
    }
    fn accent(mut self, row: Vec<egui::Pos2>) -> Self {
        self.accent.push(row);
        self
    }
}

/// Split `r` into `n` rows, each with a little air around it.
///
/// Two rows is the ceiling the box copy can carry: at 17 px a 1 px stroke needs
/// roughly a 4 px pitch to read as a line of its own, so three rows are already
/// at the edge and four are not drawable. That budget is why no clock-plus-lanes
/// picture is in the set - it is also the honest reason QSPI and its four
/// siblings get none.
fn legend_rows(r: egui::Rect, n: usize) -> Vec<egui::Rect> {
    let gap = 3.0;
    let h = (r.height() - gap * (n as f32 - 1.0)) / n as f32;
    (0..n)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(r.left(), r.top() + i as f32 * (h + gap)),
                egui::vec2(r.width(), h),
            )
        })
        .collect()
}

/// A square wave across `r` from a list of `(high fraction of the slot)` duties,
/// one slot per entry.
///
/// FOUR points per pulse: baseline, rise, top, fall. Dropping the one that
/// returns to the baseline before the next rising edge leaves the polyline
/// climbing diagonally out of each pulse, and the picture comes out as a row of
/// sawteeth - a different signal.
fn square_wave(r: egui::Rect, duties: &[f32]) -> Vec<egui::Pos2> {
    let slot = r.width() / duties.len() as f32;
    let (lo, hi) = (r.bottom(), r.top());
    let mut pts = Vec::with_capacity(duties.len() * 4 + 1);
    for (i, d) in duties.iter().enumerate() {
        let x0 = r.left() + i as f32 * slot;
        let x1 = x0 + slot * d;
        pts.push(egui::pos2(x0, lo));
        pts.push(egui::pos2(x0, hi));
        pts.push(egui::pos2(x1, hi));
        pts.push(egui::pos2(x1, lo));
    }
    pts.push(egui::pos2(r.right(), lo));
    pts
}

/// `n` evenly spaced 50 % pulses - a plain clock.
fn clock(r: egui::Rect, n: usize) -> Vec<egui::Pos2> {
    square_wave(r, &vec![0.5; n])
}

/// The signal legend for `kind`, or `None` where a picture would say nothing
/// true.
///
/// Ten kinds get nothing, for three reasons that are worth keeping written
/// down, because each looks like an omission and is not:
///
/// * QSPI, OSPI, XSPI, HSPI and SDMMC differ from each other ONLY in how many
///   data lanes they carry, and the row budget above holds three at best - so
///   all five would draw the same picture. Worse, the lane count is the WIRING,
///   not a setting: the same module is one, four or eight lanes wide at
///   different moments, and any drawn count is wrong at some of them.
/// * PARL_IO and PARL RX differ only in DIRECTION, which no waveform shows. One
///   drawing on both boxes would assert they are the same peripheral - the
///   exact claim the two kinds exist to deny.
/// * LCD_CAM and Camera each carry three unrelated waveform families behind one
///   `mode` (an i8080 strobe, a free-running RGB pixel clock, a camera whose
///   SENSOR drives the clock). One static picture would be wrong in two cases
///   out of three, and keying it to `mode` would break the rule that a legend
///   is not a reading of this module.
///
/// Custom has no protocol at all.
fn legend_of(kind: ModuleKind, r: egui::Rect) -> Option<Legend> {
    use ModuleKind as K;
    Some(match kind {
        K::GenericInterfaceTimer => pwm_legend_shape(r),
        K::GenericInterfaceUsb => usb_legend_shape(r),
        K::GenericInterfaceTouch => touch_legend_shape(r),
        K::GenericInterfaceDac => dac_legend_shape(r),
        K::GenericInterfacePcnt => pcnt_legend_shape(r),
        K::GenericInterfaceSpi => spi_legend_shape(r),
        K::GenericInterfaceI2c => i2c_legend_shape(r),
        // SAI draws the I2S picture, and that is the honest answer rather than
        // a shortcut: the signals ARE the same bit-clock / frame-sync / data
        // triple, and SAI's one real difference - a frame split into up to
        // sixteen TDM slots instead of a stereo pair - is not exclusive to it,
        // because I2S can be set to PCM short sync and wear the same shape.
        // Inventing a difference here would be the misleading option.
        K::GenericInterfaceI2s | K::GenericInterfaceSai => i2s_legend_shape(r),
        K::GenericInterfaceMcpwm => mcpwm_legend_shape(r),
        // LPUART draws the USART picture for the same reason SAI draws I2S's:
        // what goes on the pad IS a UART frame. Its real difference - it clocks
        // from a low-speed source and can wake the part from Stop - has no
        // scope picture at all, and inventing one would be the misleading
        // option.
        K::GenericInterfaceUsart | K::GenericInterfaceLpuart => uart_legend_shape(r),
        K::GenericInterfaceCan => can_legend_shape(r),
        K::GenericInterfaceRmt => rmt_legend_shape(r),
        // The five external-memory ports and SDMMC. One drawing for all of
        // them, and that is the honest answer rather than a shortcut: what
        // separates them is how many data lanes they carry, the bus notation
        // below deliberately does not claim a number, and the count is the
        // WIRING anyway - the same module is one, four or eight lanes wide at
        // different moments.
        K::GenericInterfaceQspi
        | K::GenericInterfaceOspi
        | K::GenericInterfaceXspi
        | K::GenericInterfaceHspi
        | K::GenericInterfaceSdmmc => memory_legend_shape(r),
        // Both directions of the parallel port share a drawing for the same
        // reason LPUART shares USART's: what goes on the wire is identical, and
        // direction is not something a waveform shows. An earlier pass refused
        // them on the grounds that one picture would claim the two kinds are
        // the same peripheral - which was inconsistent with LPUART and SAI,
        // where exactly that trade was already made and is right.
        K::GenericInterfaceParlIo | K::GenericInterfaceParlIoRx => parallel_legend_shape(r),
        K::GenericInterfaceLcdCam | K::GenericInterfaceCamera => lcd_cam_legend_shape(r),
        _ => return None,
    })
}

/// PWM: nine pulses widening in three steps, with the average they hold.
fn pwm_legend_shape(r: egui::Rect) -> Legend {
    let duties: Vec<f32> = PWM_LEGEND_DUTIES
        .iter()
        .flat_map(|d| std::iter::repeat_n(*d, PWM_LEGEND_PULSES))
        .collect();
    Legend::new()
        .signal(square_wave(r, &duties))
        .accent(pwm_average(r))
}

/// USB: a differential pair, and the one place it stops being one.
///
/// Two rows that mirror each other exactly, plus a short stretch where BOTH sit
/// low - SE0, the end-of-packet state. That stretch is what makes the drawing
/// say "differential" rather than just "two lines", and it is honest here
/// because this module's pads ARE the pair (unlike CAN's, which are the
/// controller-side TX/RX).
fn usb_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    // Six slots; the last two are the packet's end, where both lines idle low.
    let bits = [true, false, true, true, false, false];
    let line = |row: egui::Rect, invert: bool| {
        let slot = row.width() / bits.len() as f32;
        let mut pts = Vec::new();
        for (i, b) in bits.iter().enumerate() {
            let se0 = i >= 4;
            let high = if se0 { false } else { *b != invert };
            let y = if high { row.top() } else { row.bottom() };
            let x0 = row.left() + i as f32 * slot;
            pts.push(egui::pos2(x0, y));
            pts.push(egui::pos2(x0 + slot, y));
        }
        square_edges(pts)
    };
    let se0_x = r.left() + r.width() * 4.0 / bits.len() as f32;
    Legend::new()
        .signal(line(rows[0], false))
        .signal(line(rows[1], true))
        .accent(vec![
            egui::pos2(se0_x, rows[0].bottom()),
            egui::pos2(r.right(), rows[0].bottom()),
        ])
        .accent(vec![
            egui::pos2(se0_x, rows[1].bottom()),
            egui::pos2(r.right(), rows[1].bottom()),
        ])
}

/// Insert the vertical connectors a level-per-slot list implies.
///
/// The points come in pairs (slot start, slot end) at one level; between two
/// slots at different levels the line has to go straight up or down, or the
/// stroke cuts the corner and the square wave becomes a ramp.
fn square_edges(pts: Vec<egui::Pos2>) -> Vec<egui::Pos2> {
    let mut out = Vec::with_capacity(pts.len() + pts.len() / 2);
    for (i, p) in pts.iter().enumerate() {
        let changes_level = i > 0 && (p.y - pts[i - 1].y).abs() > f32::EPSILON;
        let same_x = i > 0 && (p.x - pts[i - 1].x).abs() < 0.01;
        if changes_level && !same_x {
            // HOLD the old level to the new x, then jump - not jump first and
            // then run along at the new level. The two are mirror images and
            // only one of them is the signal: the wrong side makes a line that
            // is meant to stay high until a transition drop the moment the
            // previous point ends.
            out.push(egui::pos2(p.x, pts[i - 1].y));
        }
        out.push(*p);
    }
    out
}

/// Touch: a capacitance reading dipping past its threshold.
///
/// The only picture in the set that is not a logic level, which is why it can
/// never be mistaken for a neighbour. The dip direction is the real one: a
/// finger ADDS capacitance, the count falls.
fn touch_legend_shape(r: egui::Rect) -> Legend {
    let base = r.top() + r.height() * 0.25;
    let floor = r.bottom();
    let (a, b) = (r.left() + r.width() * 0.3, r.left() + r.width() * 0.7);
    Legend::new()
        .signal(vec![
            egui::pos2(r.left(), base),
            egui::pos2(a, base),
            egui::pos2(a + r.width() * 0.08, floor),
            egui::pos2(b - r.width() * 0.08, floor),
            egui::pos2(b, base),
            egui::pos2(r.right(), base),
        ])
        .accent(vec![
            egui::pos2(r.left(), r.center().y),
            egui::pos2(r.right(), r.center().y),
        ])
}

/// DAC: the code it is given, and the level the pad holds.
///
/// A staircase rather than a clean ramp, because quantisation is what a DAC IS.
fn dac_legend_shape(r: egui::Rect) -> Legend {
    let steps = [0.15_f32, 0.45, 0.75, 1.0, 0.75, 0.45];
    let y = |v: f32| r.bottom() - v * (r.height() - 1.0);
    let slot = r.width() / steps.len() as f32;
    let mut stair = Vec::new();
    let mut smooth = Vec::new();
    for (i, v) in steps.iter().enumerate() {
        let x0 = r.left() + i as f32 * slot;
        stair.push(egui::pos2(x0, y(*v)));
        stair.push(egui::pos2(x0 + slot, y(*v)));
        smooth.push(egui::pos2(x0 + slot * 0.5, y(*v)));
    }
    Legend::new().signal(square_edges(stair)).accent(smooth)
}

/// PCNT: equal edges, and the count they keep.
///
/// The staircase only ever climbs, and it is the only monotone line in the
/// vocabulary - nothing else counts.
fn pcnt_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let n = 5;
    let wave = clock(rows[1], n);
    let slot = rows[0].width() / n as f32;
    let mut count = vec![egui::pos2(rows[0].left(), rows[0].bottom())];
    for i in 0..n {
        let x = rows[0].left() + i as f32 * slot;
        let y = rows[0].bottom() - (i as f32 + 1.0) / n as f32 * rows[0].height();
        count.push(egui::pos2(x, y));
        count.push(egui::pos2(x + slot, y));
    }
    Legend::new().signal(wave).accent(square_edges(count))
}

/// The accents a device group is drawn in.
///
/// Chosen to sit OUTSIDE two vocabularies already spoken on this canvas. White
/// at 2.8 px is selection, shared by pins, boxes and io fields; every saturated
/// hue belongs to `PinFunction::color` and is reused for wires, borders, titles
/// and list rows. These are desaturated and light, which reads as "a label on
/// top of" rather than "another kind of signal".
const GROUP_COLOURS: [egui::Color32; 6] = [
    egui::Color32::from_rgb(196, 168, 120), // sand
    egui::Color32::from_rgb(150, 176, 190), // slate blue
    egui::Color32::from_rgb(178, 152, 186), // mauve
    egui::Color32::from_rgb(150, 186, 158), // sage
    egui::Color32::from_rgb(200, 152, 148), // clay
    egui::Color32::from_rgb(168, 172, 200), // periwinkle
];

/// The path ONE wire is drawn along, and the terminal it actually leaves from.
///
/// Extracted from the paint loop so the decision can be tested with real box
/// geometry — the silhouette, the chamfers, both candidate edges and the
/// obstacles — instead of only as arithmetic. The caller does nothing with the
/// result but draw it.
///
/// Returns the path TERMINAL-FIRST, so `path[0]` is always the returned
/// terminal. The endpoint dot is painted from that, never from the candidate the
/// caller happened to compute first: when the second candidate wins, a dot
/// placed early is left stranded on the wrong edge with no wire reaching it.
#[allow(clippy::too_many_arguments)]
fn wire_path_for_box(
    ring: egui::Rect,
    blocked: super::wire::Blocked<'_>,
    rect: egui::Rect,
    shape: BoxShape,
    face: Side,
    manual: bool,
    anchor: egui::Pos2,
    adir: egui::Vec2,
) -> (Vec<egui::Pos2>, egui::Pos2) {
    let sil = silhouette(rect, shape, face);
    // A dragged box's stored side no longer implies an edge, so aim at the
    // outline point nearest the pin; an auto box keeps its side.
    let facing = if manual {
        nearest_on_outline(&sil, anchor)
    } else {
        facing_terminal(rect, face, anchor)
    };
    // WHICH EDGE the wire leaves by. Two candidates, and the one that costs
    // fewer corners wins:
    //
    //  * the edge facing the chip — right for a pad across the channel, which
    //    is most of them, and the only one for a pad on the far side;
    //  * the edge facing the PAD's own side — a box on the chip's left wired to
    //    a pad on its top leaves by its own top edge, and the two rays then run
    //    the same way, which turns a wire that had to crawl along the pin row
    //    into two corners over a lane well clear of it.
    //
    // Ties go to the facing edge, so the everyday straight wire is untouched.
    let port = nearest_on_outline(&sil, edge_terminal(rect, adir, anchor));
    super::wire::best_route(
        ring,
        blocked,
        anchor,
        adir,
        &[(facing, facing_normal(face)), (port, adir)],
    )
    .unwrap_or_else(|| (vec![facing, anchor], facing))
}

/// A terminal on the box edge whose outward normal is `normal`, lined up with
/// `anchor`.
///
/// The counterpart of [`facing_terminal`] for a wire that does NOT leave by the
/// edge facing the chip. A module on the chip's left wired to a pad on its top
/// leaves by its own TOP edge, and the two rays then run the same way — which is
/// what turns a wire that had to crawl along the pin row into two corners over a
/// lane well clear of it.
fn edge_terminal(rect: egui::Rect, normal: egui::Vec2, anchor: egui::Pos2) -> egui::Pos2 {
    if normal.x.abs() > normal.y.abs() {
        let x = if normal.x > 0.0 {
            rect.right()
        } else {
            rect.left()
        };
        egui::pos2(
            x,
            anchor
                .y
                .clamp(rect.top() + CORNER_INSET, rect.bottom() - CORNER_INSET),
        )
    } else {
        let y = if normal.y > 0.0 {
            rect.bottom()
        } else {
            rect.top()
        };
        egui::pos2(
            anchor
                .x
                .clamp(rect.left() + CORNER_INSET, rect.right() - CORNER_INSET),
            y,
        )
    }
}

/// The outward normal of the box edge that faces the chip.
///
/// `Side` is which side of the CHIP the box sits on, so a box on the right faces
/// left. This is the direction its wires leave by, and the one the router walks
/// to reach the corridor.
fn facing_normal(side: Side) -> egui::Vec2 {
    match side {
        Side::Right => egui::vec2(-1.0, 0.0),
        Side::Left => egui::vec2(1.0, 0.0),
        Side::Bottom => egui::vec2(0.0, -1.0),
        Side::Top => egui::vec2(0.0, 1.0),
    }
}

/// A stored offset that can never be mistaken for "auto-packed".
///
/// `VirtualModule.pos == (0, 0)` is the sentinel for "let the packer place it".
/// A device drag computes each part's offset ARITHMETICALLY rather than from the
/// pointer, so it can land exactly on that sentinel and silently un-pin a box in
/// the middle of a gesture — the box would jump back to its packed slot while
/// the rest of the device kept moving.
pub fn nudge(v: egui::Vec2) -> (f32, f32) {
    if v.x == 0.0 && v.y == 0.0 {
        (0.0, f32::EPSILON)
    } else {
        (v.x, v.y)
    }
}

/// Whether a wire ending on `pad` belongs to the device the canvas is pointing
/// at.
///
/// Keyed on the PAD and never on the wire's box. A box can wire two devices'
/// pads, and `group_of_module` answers with the first one it finds — keyed on
/// the box, such a box would light both its wires or neither, and the mat under
/// it would be telling the truth while its wires lied.
pub fn wire_lit<'a>(active: Option<&'a str>, mcu: &Mcu, pad: usize) -> Option<&'a str> {
    active.filter(|d| mcu.group_of_pin(pad).is_some_and(|g| g.name.trim() == *d))
}

/// One wire, lit or not, as shapes rather than paint calls.
///
/// A POLYLINE and not two points: a routed wire has corners, and both places
/// that draw a wire have to grow the same way.
///
/// A lit wire KEEPS its signal colour — that is what says which line this is —
/// and gains a halo in the device's colour underneath. Nothing is dimmed to make
/// it stand out: every selection mark on this canvas is additive, so five wires
/// scattered over three sides of the package read as one bundle without the rest
/// of the diagram going quiet.
pub fn wire_shapes(
    path: &[egui::Pos2],
    color: egui::Color32,
    w: f32,
    lit: Option<&str>,
) -> (Option<egui::Shape>, egui::Shape) {
    let halo = lit.map(|dev| {
        let c = group_color(dev);
        egui::Shape::line(
            path.to_vec(),
            egui::Stroke::new(
                w + 4.0,
                egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 90),
            ),
        )
    });
    let wire = egui::Shape::line(
        path.to_vec(),
        egui::Stroke::new(if lit.is_some() { w * 1.35 } else { w }, color),
    );
    (halo, wire)
}

/// The colour of the group called `name`.
///
/// Derived from the NAME rather than stored, so it needs no field, no picker
/// and no migration - and a project reopened years later draws its devices the
/// same colour it did before. Two names can land on one colour; renaming either
/// moves it, which is a smaller price than a colour field nobody wants to fill
/// in.
pub fn group_color(name: &str) -> egui::Color32 {
    let h = name
        .trim()
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    GROUP_COLOURS[h as usize % GROUP_COLOURS.len()]
}

/// Height of the bar that marks a grouped PAD.
///
/// A module box wears no such bar. It used to, under its title, and it was one
/// mark too many: the box already sits on the device's tinted mat and inside its
/// named tab, so a third statement of the same fact only crowded the title. A
/// PAD has no mat under it — its mat is the tip square out on the pin row — so
/// there the bar is the mark that says which device the pad belongs to.
pub(super) const GROUP_BAR_H: f32 = 3.0;

/// The one grey every legend's signal is drawn in, connected and not.
///
/// Two colours across the whole set, and no more: a third would stop them
/// reading as one vocabulary. The module's own colour was tried here and
/// reverted - it made one drawing look like two different things depending on
/// where you met it.
const LEGEND_GREY: egui::Color32 = egui::Color32::from_gray(165);
const LEGEND_GREY_OFF: egui::Color32 = egui::Color32::from_gray(120);
/// The accent: the one thing each picture exists to point at.
const LEGEND_ACCENT: egui::Color32 = egui::Color32::from_rgb(205, 85, 70);

/// Paint a legend into `r`.
fn paint_legend(painter: &egui::Painter, l: &Legend, grey: egui::Color32) {
    for row in &l.signal {
        painter.line(row.clone(), egui::Stroke::new(LEGEND_SIGNAL_STROKE, grey));
    }
    for row in &l.accent {
        painter.line(row.clone(), egui::Stroke::new(LEGEND_STROKE, LEGEND_ACCENT));
    }
}

/// How much of the legend the RMT train occupies before it stops.
///
/// The rest is the idle tail, and the tail is the whole separator from PWM -
/// see [`rmt_legend_shape`]. A third is enough to read as "and then nothing"
/// at 52 px.
const RMT_TRAIN: f32 = 2.0 / 3.0;

/// RMT: a train of arbitrary timings that ENDS.
///
/// Not "pulses of unequal width" - the PWM legend already draws three different
/// widths, so nobody would read that as the difference. What no PWM ever does
/// is STOP: a PWM output runs until you turn it off, and its legend carries an
/// average line across the full width. An RMT sends a finite list of durations
/// and then rests, which is why the picture is a burst in the first two thirds
/// and a flat tail in the last one, with the tail as the accent.
///
/// Both the high AND the low runs vary, which a PWM's cannot: a PWM's period is
/// fixed and only the duty moves inside it.
fn rmt_legend_shape(r: egui::Rect) -> Legend {
    // Alternating high/low runs, starting high. Deliberately without a repeat.
    let runs = [3.0_f32, 2.0, 1.0, 4.0, 5.0, 1.0, 2.0, 3.0, 1.0, 2.0];
    let total: f32 = runs.iter().sum();
    let train_w = r.width() * RMT_TRAIN;
    let idle_x = r.left() + train_w;
    let mut pts = vec![egui::pos2(r.left(), r.bottom())];
    let mut x = r.left();
    for (i, run) in runs.iter().enumerate() {
        let y = if i % 2 == 0 { r.top() } else { r.bottom() };
        pts.push(egui::pos2(x, y));
        x += run / total * train_w;
        pts.push(egui::pos2(x, y));
    }
    // ...and then it rests. The level it rests at is a setting, so the picture
    // says only THAT it rests, by running flat along the baseline.
    pts.push(egui::pos2(idle_x, r.bottom()));
    pts.push(egui::pos2(r.right(), r.bottom()));
    Legend::new().signal(square_edges(pts)).accent(vec![
        egui::pos2(idle_x, r.bottom()),
        egui::pos2(r.right(), r.bottom()),
    ])
}

/// A parallel bus, in the notation that exists precisely so a picture does not
/// have to say how wide it is.
///
/// Two lines that meet at the middle between windows and part to the rails
/// inside one - the elongated hexagons every timing diagram draws a bus with.
/// It says "several lines, carrying a value" and claims no lane count, which is
/// what makes it usable for ports whose width is the wiring rather than a
/// setting.
///
/// Returns the two lines as separate polylines, because they cross: joined,
/// the stroke would run back along itself between windows.
fn bus_envelope(row: egui::Rect, windows: usize, gap: f32) -> (Vec<egui::Pos2>, Vec<egui::Pos2>) {
    let w = row.width() / windows as f32;
    let mid = row.center().y;
    let (mut hi, mut lo) = (Vec::new(), Vec::new());
    for i in 0..windows {
        let x0 = row.left() + i as f32 * w;
        let x1 = x0 + w;
        hi.push(egui::pos2(x0, mid));
        hi.push(egui::pos2(x0 + gap, row.top()));
        hi.push(egui::pos2(x1 - gap, row.top()));
        hi.push(egui::pos2(x1, mid));
        lo.push(egui::pos2(x0, mid));
        lo.push(egui::pos2(x0 + gap, row.bottom()));
        lo.push(egui::pos2(x1 - gap, row.bottom()));
        lo.push(egui::pos2(x1, mid));
    }
    (hi, lo)
}

/// QSPI, OSPI, XSPI, HSPI, SDMMC: a clock and a bus that turns around.
///
/// The bus being BIDIRECTIONAL is what separates a memory port from a plain
/// parallel one: the same lines carry the command out and the data back, and
/// the gap where nobody drives them is the accent. True of all five, including
/// SDMMC, whose CMD and DAT lines both turn.
fn memory_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let gap = rows[1].width() * 0.03;
    // Three windows with the middle one absent: out, turnaround, back.
    let (hi, lo) = bus_envelope(rows[1], 3, gap);
    let per = hi.len() / 3;
    let keep = |v: &Vec<egui::Pos2>, i: usize| v[i * per..(i + 1) * per].to_vec();
    let mid_y = rows[1].center().y;
    let (t0, t1) = (
        rows[1].left() + rows[1].width() / 3.0,
        rows[1].left() + rows[1].width() * 2.0 / 3.0,
    );
    Legend::new()
        .signal(clock(rows[0], 6))
        .signal(keep(&hi, 0))
        .signal(keep(&lo, 0))
        .signal(keep(&hi, 2))
        .signal(keep(&lo, 2))
        .accent(vec![egui::pos2(t0, mid_y), egui::pos2(t1, mid_y)])
}

/// PARL_IO, in either direction: a clock and a bus, every window valid.
///
/// No turnaround - a parallel port drives one way for as long as it runs, which
/// is the difference from a memory port and the only one this size can hold.
/// The accent is the clock edge the bus is latched on, because that is what the
/// peripheral is: bits made to move together on an edge.
fn parallel_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let gap = rows[1].width() * 0.03;
    let (hi, lo) = bus_envelope(rows[1], 4, gap);
    let x = rows[0].left() + rows[0].width() / 4.0;
    Legend::new()
        .signal(clock(rows[0], 4))
        .signal(hi)
        .signal(lo)
        .accent(vec![
            egui::pos2(x, rows[0].bottom()),
            egui::pos2(x, rows[0].top()),
        ])
}

/// LCD_CAM and Camera: a pixel bus, and the sync that frames it.
///
/// The one thing true of all three modes this peripheral wears - an i8080 panel
/// strobed by WR, an RGB panel on a free-running pixel clock, and a DVP camera
/// whose SENSOR drives the clock - is that a clocked parallel bus is framed by
/// a sync line. That sync is the accent; the mode is not drawn, because a
/// legend is not a reading of the module.
fn lcd_cam_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let gap = rows[1].width() * 0.03;
    let (hi, lo) = bus_envelope(rows[1], 4, gap);
    // The sync pulse: low across the first quarter, then high for the data.
    let sync_x = rows[0].left() + rows[0].width() / 4.0;
    Legend::new()
        .signal(clock(rows[0], 6))
        .signal(hi)
        .signal(lo)
        .accent(vec![
            egui::pos2(rows[0].left(), rows[0].bottom()),
            egui::pos2(sync_x, rows[0].bottom()),
            egui::pos2(sync_x, rows[0].top()),
        ])
}

/// USART and LPUART: one asynchronous frame.
///
/// Idle high, a start bit down, data, a stop bit back up. The two framing edges
/// are the accent because they ARE the peripheral - there is no clock line, and
/// the receiver has only those two edges to find the byte with. It is also what
/// separates this from SPI without reading any detail: a UART picture has no
/// second row, because the peripheral has no clock pin.
fn uart_legend_shape(r: egui::Rect) -> Legend {
    // A WHOLE frame, in the order a scope shows it: idle high, one start bit
    // down, eight data bits, one stop bit back up, idle again. Eight and not
    // six - the count is the thing everybody knows about a UART, and a picture
    // that showed a different number would be teaching a frame nobody sends.
    //
    // The last data bit is low so the stop bit is a real RISING edge. With it
    // high the line would already be where the stop bit puts it, and the mark
    // would be a red tick standing on a flat run - the same spurious-edge
    // artefact that had to be taken out of the I2C picture.
    let bits = [
        true,  // idle
        false, // start
        false, true, false, false, false, true, false, false, // eight data bits
        true,  // stop
        true,  // idle again
    ];
    let slot = r.width() / bits.len() as f32;
    let start_x = r.left() + slot;
    let stop_x = r.left() + slot * 10.0;
    Legend::new()
        .signal(levels(r, &bits))
        .accent(vec![
            egui::pos2(start_x, r.top()),
            egui::pos2(start_x, r.bottom()),
        ])
        .accent(vec![
            egui::pos2(stop_x, r.bottom()),
            egui::pos2(stop_x, r.top()),
        ])
}

/// CAN: the one bit somebody else drives.
///
/// Recessive high at rest, a dominant start, a stretch of bits, and then the
/// ACK slot - the single bit the transmitter sends recessive and every
/// listening node pulls down. That bit is the accent because it is the only
/// thing on this wire that is not the sender's, and it is what makes the
/// picture CAN rather than a UART frame.
///
/// Drawn as a HORIZONTAL run in the middle, against the UART's two vertical
/// ticks at the ends: the two are the closest pair in the whole set - one row,
/// idle high, opening low bit - so the accent has to differ in count, place AND
/// orientation, not only in place.
///
/// Deliberately NOT the mirrored CAN_H / CAN_L pair a scope shows, even though
/// that is the canonical picture: this module's pads are the controller-side
/// RX/TX, and its config can turn the transceiver off entirely, in which case
/// the differential pair does not physically exist.
fn can_legend_shape(r: egui::Rect) -> Legend {
    let bits = [
        true, false, true, false, false, true, false, true, false, true,
    ];
    let slot = r.width() / bits.len() as f32;
    // The ACK slot: one dominant bit, near the middle.
    let ack = 5;
    let (x0, x1) = (
        r.left() + slot * ack as f32,
        r.left() + slot * (ack as f32 + 1.0),
    );
    let mut shown = bits;
    shown[ack] = false;
    Legend::new()
        .signal(levels(r, &shown))
        .accent(vec![egui::pos2(x0, r.bottom()), egui::pos2(x1, r.bottom())])
}

/// A row's level per slot, turned into a square wave with its vertical edges.
fn levels(row: egui::Rect, hi_lo: &[bool]) -> Vec<egui::Pos2> {
    let slot = row.width() / hi_lo.len() as f32;
    let mut pts = Vec::new();
    for (i, high) in hi_lo.iter().enumerate() {
        let y = if *high { row.top() } else { row.bottom() };
        let x0 = row.left() + i as f32 * slot;
        pts.push(egui::pos2(x0, y));
        pts.push(egui::pos2(x0 + slot, y));
    }
    square_edges(pts)
}

/// SPI: a clock, and data that only ever moves on its edges.
///
/// The clock runs edge to edge - which is the ONE thing separating this picture
/// from I2C's at 52 px, where SCL has flat high shoulders because START and
/// STOP happen while it is high. CPOL/CPHA are deliberately not drawn: they are
/// a per-module setting, and at this width a half-period shift is under three
/// pixels anyway.
fn spi_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let n = 5;
    // Data holds through each clock period and changes only between them.
    let bits = [true, false, true, true, false];
    let slot = rows[0].width() / n as f32;
    // The edge that samples, marked on the clock row.
    let x = rows[0].left() + slot * 2.0;
    Legend::new()
        .signal(clock(rows[0], n))
        .signal(levels(rows[1], &bits))
        .accent(vec![
            egui::pos2(x, rows[0].bottom()),
            egui::pos2(x, rows[0].top()),
        ])
}

/// I2C: the two conditions that frame every transfer.
///
/// SDA falling while SCL is high is START, SDA rising while SCL is high is
/// STOP, and they are the only two moments in the protocol where SDA may move
/// at all with the clock high - which is exactly why they can serve as
/// delimiters. The eight data bits and the ACK cannot be read at this size and
/// are not attempted; the shoulders are.
fn i2c_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let shoulder = r.width() / 6.0;
    // The clock burst, and - INSIDE the flat shoulders rather than at their
    // edge - where the two conditions happen. SCL has to be unambiguously high
    // at both, which is the whole reason they can serve as delimiters; putting
    // them level with the first and last clock edge would draw a START that is
    // not one.
    let (a, b) = (r.left() + shoulder, r.right() - shoulder);
    let (start_x, stop_x) = (r.left() + shoulder * 0.5, r.right() - shoulder * 0.5);
    let burst = egui::Rect::from_min_max(
        egui::pos2(a, rows[0].top()),
        egui::pos2(b, rows[0].bottom()),
    );
    let mut scl = vec![egui::pos2(rows[0].left(), rows[0].top())];
    // `.skip(1)`: `square_wave` opens on the BASELINE, and appending that to a
    // run that is already high made `square_edges` draw down and straight back
    // up at the same x - a full-height tick inside the flat shoulder, reading
    // as a clock edge exactly where the picture is claiming there is none. The
    // burst starts with SCL falling, which is the second point.
    scl.extend(clock(burst, 4).into_iter().skip(1));
    // `square_wave` ends on the baseline, and SCL has to be HIGH again for the
    // STOP to be one - so it rises at the end of the burst, not at the far edge
    // of the box.
    scl.push(egui::pos2(b, rows[0].top()));
    scl.push(egui::pos2(rows[0].right(), rows[0].top()));
    // SDA: high, down at START, bits, up at STOP, high again. The last bit is
    // low so the STOP is a real rising edge rather than a line that was already
    // there.
    let sda = &rows[1];
    let bits = [false, true, false, false];
    let bit_w = (b - a) / bits.len() as f32;
    let mut d = vec![
        egui::pos2(sda.left(), sda.top()),
        egui::pos2(start_x, sda.top()),
        egui::pos2(start_x, sda.bottom()),
    ];
    for (i, high) in bits.iter().enumerate() {
        let y = if *high { sda.top() } else { sda.bottom() };
        let x0 = a + i as f32 * bit_w;
        d.push(egui::pos2(x0, y));
        d.push(egui::pos2(x0 + bit_w, y));
    }
    d.push(egui::pos2(stop_x, sda.bottom()));
    d.push(egui::pos2(stop_x, sda.top()));
    d.push(egui::pos2(sda.right(), sda.top()));
    Legend::new()
        .signal(square_edges(scl))
        .signal(square_edges(d))
        .accent(vec![
            egui::pos2(start_x, sda.top()),
            egui::pos2(start_x, sda.bottom()),
        ])
        .accent(vec![
            egui::pos2(stop_x, sda.bottom()),
            egui::pos2(stop_x, sda.top()),
        ])
}

/// I2S and SAI: a bit clock, and the word select that is far slower than it.
///
/// The RATE RATIO is the identity, and it is what separates this from SPI,
/// where both lines move at comparable rates. Drawn at roughly 8:1 rather than
/// the real 32:1 of a 16-bit stereo frame, because 32 transitions do not fit
/// in 52 px.
fn i2s_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let ws = square_wave(rows[1], &[0.5]);
    Legend::new()
        .signal(clock(rows[0], 8))
        .signal(ws)
        // The frame boundary: where the word changes, which is what WS is for.
        .accent(vec![
            egui::pos2(rows[1].center().x, rows[1].bottom()),
            egui::pos2(rows[1].center().x, rows[1].top()),
        ])
}

/// MCPWM: a complementary pair, and the gap that keeps them from overlapping.
///
/// The dead time is drawn as a caricature - a real one is a fraction of a
/// percent of the period, and at this width that is invisible. It is the red
/// mark because it is the whole reason the peripheral is not just two PWMs: for
/// that window BOTH outputs are off, and a bridge that skips it shoots through.
fn mcpwm_legend_shape(r: egui::Rect) -> Legend {
    let rows = legend_rows(r, 2);
    let cycles = 3;
    let slot = r.width() / cycles as f32;
    let (duty, dead) = (0.45_f32, 0.12_f32);
    let mut hi = Vec::new();
    let mut lo = Vec::new();
    let mut gaps = Vec::new();
    for i in 0..cycles {
        let x0 = r.left() + i as f32 * slot;
        let fall = x0 + slot * duty;
        let rise = x0 + slot;
        // Upper output: high for its duty, then low.
        hi.push(egui::pos2(x0, rows[0].top()));
        hi.push(egui::pos2(fall, rows[0].top()));
        hi.push(egui::pos2(fall, rows[0].bottom()));
        hi.push(egui::pos2(rise, rows[0].bottom()));
        // Lower output: the complement, minus a dead window at each edge.
        let on = fall + slot * dead;
        let off = rise - slot * dead;
        lo.push(egui::pos2(x0, rows[1].bottom()));
        lo.push(egui::pos2(on, rows[1].bottom()));
        lo.push(egui::pos2(on, rows[1].top()));
        lo.push(egui::pos2(off, rows[1].top()));
        lo.push(egui::pos2(off, rows[1].bottom()));
        lo.push(egui::pos2(rise, rows[1].bottom()));
        gaps.push(vec![
            egui::pos2(fall, r.center().y),
            egui::pos2(on, r.center().y),
        ]);
    }
    let mut l = Legend::new().signal(hi).signal(lo);
    for g in gaps {
        l = l.accent(g);
    }
    l
}

/// The three duty levels the little waveform legend draws, low to high.
///
/// A LEGEND, not a reading of this module: it says what the peripheral does -
/// a wider pulse holds the output high for longer, and the average the load
/// actually sees follows it up - and it says the same thing whatever the
/// module is set to. The module's own duty is a number in the rows below, and
/// with several channels there is no single one to draw.
const PWM_LEGEND_DUTIES: [f32; 3] = [0.2, 0.5, 0.8];

/// Pulses drawn per level. Three is enough to read as "a repeating square
/// wave" and short enough to sit in a corner.
const PWM_LEGEND_PULSES: usize = 3;

/// Size of the legend. Wide enough for nine pulses to stay distinguishable at
/// this height, small enough not to push the first config row down.
const PWM_LEGEND_SIZE: egui::Vec2 = egui::vec2(150.0, 38.0);

/// The signal, and the accent that has to stand out from it.
const LEGEND_SIGNAL_STROKE: f32 = 1.0;
/// The WIDEST stroke `paint_legend` draws - named as a constant, and used both
/// to draw with and to reserve room for, so the two cannot drift apart.
///
/// It matters because egui centres a stroke ON its path: a line drawn along the
/// bottom of a rect puts half its width BELOW that rect, and a clip at the
/// rect's edge takes that half away. `the_widest_stroke_is_the_one_reserved_for`
/// keeps this the maximum.
const LEGEND_STROKE: f32 = 1.2;

/// Where the legend goes inside the strip reserved for it: hard right, and half
/// a stroke clear of the top and bottom.
///
/// The inset is not padding. A square wave's low level is drawn exactly ON
/// `r.bottom()` and its high level exactly ON `r.top()` - that is what makes the
/// two read as levels rather than as a band. With the legend filling its strip
/// edge to edge and the painter clipped to that strip, half of the bottom line
/// and half of the top one were cut away: the USB legend, whose two rows BOTH
/// end on a row bottom, showed it worst.
///
/// The canvas box never had the fault because `box_legend_rect` already sits
/// 6 px inside the box.
fn pwm_legend_rect(strip: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(
        egui::pos2(
            strip.right() - PWM_LEGEND_SIZE.x,
            strip.top() + LEGEND_STROKE / 2.0,
        ),
        PWM_LEGEND_SIZE,
    )
}

/// The average the load sees: flat across each level, ramping between them.
///
/// The red line in the picture, and the reason the legend is worth drawing at
/// all - the square wave alone does not say what a duty cycle is FOR.
fn pwm_average(r: egui::Rect) -> Vec<egui::Pos2> {
    let span = r.height() - 6.0;
    let y = |d: f32| r.bottom() - 3.0 - d * span;
    let group = r.width() / PWM_LEGEND_DUTIES.len() as f32;
    // The ramp is drawn inside the group it climbs INTO, so each level is flat
    // for most of its own pulses - as it is on a real filter.
    let ramp = group * 0.35;
    let mut pts = vec![egui::pos2(r.left(), y(PWM_LEGEND_DUTIES[0]))];
    for (i, d) in PWM_LEGEND_DUTIES.iter().enumerate() {
        let x0 = r.left() + i as f32 * group;
        if i > 0 {
            pts.push(egui::pos2(x0 + ramp, y(*d)));
        }
        pts.push(egui::pos2(x0 + group, y(*d)));
    }
    pts
}

/// The legend as it appears on the CANVAS box: same shape, corner-sized.
///
/// Unscaled size. The box is [`BOX_W`] wide and its title sits centred at the
/// top, so this has to be narrow enough to clear a title of a few characters -
/// which is what a module base name is (`PWM0`, `TIM3`).
const BOX_LEGEND_SIZE: egui::Vec2 = egui::vec2(52.0, 17.0);

/// How far below the box's top the legend sits: under the three text rows.
///
/// The title is at 13, the summary at 30 and the variable name at 47, so 56
/// clears the last of them with a little air.
const BOX_LEGEND_TOP: f32 = 56.0;

/// Where it sits inside a module box: centred, under the three text rows.
///
/// A CORNER was the first answer and it was the wrong one twice over. The
/// silhouette bevels the corners of the edge facing away from the chip - both
/// of them on a `Driver` box, 35 px deep - so the corner had to follow the
/// side; and the title is centred at the top, so a wide name like `USART1` or
/// `MCPWM0` collided with the top-right one and the picture was dropped
/// altogether. Boxes whose names happened to be short kept their thumbnail and
/// the rest silently did not.
///
/// Under the texts there is neither problem: the middle of a box is never
/// bevelled on any of the five silhouettes, and nothing else is drawn there
/// since the name row moved to the panel. It also reads better - the box is one
/// centred column, and the picture is the last thing in it.
/// Size of the notes glyph on a box, in the box's own unscaled units.
const NOTE_GLYPH: f32 = 12.0;

/// Where a box's notes glyph goes: just right of the title, on its line.
///
/// `None` when any corner of it would leave the box's REAL outline. The corners
/// facing away from the chip are bevelled, and how deep depends on the box's own
/// size (see [`silhouette`]) - on a box above the chip both TOP corners are cut,
/// which is exactly the title's row. So the test is against that same polygon,
/// not a guessed margin. A long title on a bevelled box simply gets no glyph;
/// the module's row in the panel list still shows one.
fn note_glyph_rect(
    title: egui::Rect,
    rect: egui::Rect,
    shape: BoxShape,
    side: Side,
) -> Option<egui::Rect> {
    const GAP: f32 = 4.0;
    let g = egui::Rect::from_min_size(
        egui::pos2(title.right() + GAP, title.center().y - NOTE_GLYPH / 2.0),
        egui::vec2(NOTE_GLYPH, NOTE_GLYPH),
    );
    let outline = silhouette(rect, shape, side);
    [
        g.left_top(),
        g.right_top(),
        g.left_bottom(),
        g.right_bottom(),
    ]
    .into_iter()
    .all(|p| inside_convex(&outline, p))
    .then_some(g)
}

/// Whether `p` lies inside, or on, a convex polygon given in either winding.
///
/// Every [`silhouette`] is convex - a rectangle with corners cut straight or
/// rounded - so the point is inside exactly when it is on the same side of
/// every edge. Zero-length edges are skipped rather than trusted for a sign.
fn inside_convex(poly: &[egui::Pos2], p: egui::Pos2) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut sign = 0.0_f32;
    for (i, a) in poly.iter().enumerate() {
        let b = poly[(i + 1) % poly.len()];
        let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
        if cross.abs() < 1e-3 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

fn box_legend_rect(rect: egui::Rect, shape: BoxShape) -> Option<egui::Rect> {
    // NOT scaled with the box texts. The box itself does not grow when
    // selected, so a legend that did would move under a title that also grew.
    let size = BOX_LEGEND_SIZE;
    let out = if shape.is_square() {
        // A plain rectangle keeps all four corners, so the picture takes the
        // far one - out of the way of the three centred text rows entirely,
        // and in the empty quarter of the box.
        egui::Rect::from_min_size(
            egui::pos2(rect.right() - 10.0 - size.x, rect.bottom() - 8.0 - size.y),
            size,
        )
    } else {
        // Every other silhouette bevels the corners of the edge facing away
        // from the chip - both of them on a keystone, 35 px deep - and which
        // two depends on the side the box sits on. The middle is never cut on
        // any of them, so a shape with corners to lose keeps the picture
        // centred under its texts.
        egui::Rect::from_min_size(
            egui::pos2(rect.center().x - size.x / 2.0, rect.top() + BOX_LEGEND_TOP),
            size,
        )
    };
    // Two conditions, and the second is the one that matters on a short box:
    // inside the outline, AND never riding up into the three text rows. A
    // corner is free of the TEXTS only while the box is tall enough to have a
    // corner left below them.
    let fits =
        rect.contains_rect(out.expand(4.0)) && out.top() >= rect.top() + BOX_LEGEND_TOP - 4.0;
    fits.then_some(out)
}

/// Draw a module's signal legend at the top right of its config card.
///
/// Nothing at all for the kinds `legend_of` refuses - and no reserved strip
/// either, so a card without a picture does not carry a blank band where one
/// would have been.
fn signal_legend(ui: &mut egui::Ui, kind: ModuleKind) {
    let probe = egui::Rect::from_min_size(egui::Pos2::ZERO, PWM_LEGEND_SIZE);
    if legend_of(kind, probe).is_none() {
        return;
    }
    // The picture is right-aligned in the strip, so a column narrower than it
    // does not shrink it - it slices the LEFT off, which is where the start
    // bit, the START condition and the first pulses live. Nothing at all is
    // better than a picture with its subject cut away; the panel can be
    // dragged wider, and the box on the canvas still carries one.
    if ui.available_width() < PWM_LEGEND_SIZE.x + LEGEND_STROKE {
        return;
    }
    // A stroke taller than the picture, so the lines drawn on its top and
    // bottom edges have their full width inside the clip - see
    // `pwm_legend_rect`.
    let (strip, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), PWM_LEGEND_SIZE.y + LEGEND_STROKE),
        egui::Sense::hover(),
    );
    let r = pwm_legend_rect(strip);
    if let Some(l) = legend_of(kind, r) {
        paint_legend(&ui.painter().with_clip_rect(strip), &l, LEGEND_GREY);
    }
    ui.interact(r, ui.id().with("signal_legend"), egui::Sense::hover())
        .on_hover_text(docs::legend_hover(kind));
}

/// Font multiplier for a module box's texts: 1 normally, `SELECTED_TEXT_SCALE`
/// while the box is selected. Shared by the box painter and the (separate)
/// mutable pass that puts the rename fields, so the two never drift apart.
fn text_scale(selected: bool) -> f32 {
    if selected {
        crate::panels::mcu_module::pins::gui::draw::SELECTED_TEXT_SCALE
    } else {
        1.0
    }
}

/// Draw each module beside the chip, on the side of the pins it connects to,
/// with short direct wires (no crossing the chip). Fully-disconnected modules
/// float in the right margin. Each box carries a rename field whose text is
/// appended to the module's generated handle variable (e.g. `_spi1_imu`).
pub fn draw_modules(
    mcu: &mut Mcu,
    painter: &egui::Painter,
    local_chip: egui::Rect,
    display_chip: egui::Rect,
    rot: Rot,
    ui: &mut egui::Ui,
    // Out-param, the same idiom as `field_pass` below: every box's rect and the
    // pads it speaks for, for the device mats painted into the slot reserved
    // before the chip body. Collected here rather than recomputed, so a mat is
    // drawn around what was actually painted.
    members: &mut Vec<super::device_frame::Member>,
) {
    // Boxes are placed around the DISPLAY rect (what the user sees); pin anchors
    // are computed on the LOCAL rect then rotated. For an un-rotated chip the two
    // are identical and `rot` is the identity.
    let chip_rect = display_chip;
    // ── 1. Classify each module: connected (side + along-axis centroid) or
    //       floating (no wired pins). `conns` keeps the wire endpoints.
    struct Sided {
        idx: usize,
        conns: Vec<(ModuleSignal, egui::Pos2, usize)>,
        side: Side,
        along: f32,
    }
    let chip_center = chip_rect.center();
    let mut sided: Vec<Sided> = Vec::new();
    let mut floating_idx: Vec<usize> = Vec::new();
    // Modules the user has dragged to a manual position (`pos != (0,0)`, stored
    // as an offset from the chip centre) are placed there directly, skipping the
    // auto-packing. Their wire conns are kept so the wires still track the pins.
    let mut manual_mods: Vec<(usize, Vec<(ModuleSignal, egui::Pos2, usize)>)> = Vec::new();

    for (i, m) in mcu.modules.iter().enumerate() {
        let conns: Vec<(ModuleSignal, egui::Pos2, Side, usize)> = m
            .connections
            .iter()
            .filter_map(|c| {
                pin_anchor_side(mcu, local_chip, rot, c.mcu_pin)
                    .map(|(p, s)| (c.signal, p, s, c.mcu_pin))
            })
            .collect();
        if m.pos != (0.0, 0.0) {
            let conns2 = conns.iter().map(|(sig, p, _, n)| (*sig, *p, *n)).collect();
            manual_mods.push((i, conns2));
            continue;
        }
        if conns.is_empty() {
            floating_idx.push(i);
            continue;
        }
        let side = dominant_side(&conns);
        // Centroid along the side axis, from the pins actually on that side.
        let on_side: Vec<f32> = conns
            .iter()
            .filter(|(_, _, s, _)| *s == side)
            .map(|(_, p, _, _)| match side {
                Side::Top | Side::Bottom => p.x,
                Side::Left | Side::Right => p.y,
            })
            .collect();
        let along = on_side.iter().sum::<f32>() / on_side.len().max(1) as f32;
        let conns2 = conns.iter().map(|(sig, p, _, n)| (*sig, *p, *n)).collect();
        sided.push(Sided {
            idx: i,
            conns: conns2,
            side,
            along,
        });
    }

    // ── 2. Pack each side independently so same-side boxes never overlap. ──────
    // (module index, rect, conns, side, connected, manual)
    let mut boxes: Vec<(
        usize,
        egui::Rect,
        Vec<(ModuleSignal, egui::Pos2, usize)>,
        Side,
        bool,
        bool,
    )> = Vec::new();
    for target in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
        let mut group: Vec<&Sided> = sided.iter().filter(|e| e.side == target).collect();
        group.sort_by(|a, b| a.along.total_cmp(&b.along));
        let mut cursor = f32::MIN;
        for e in group {
            let rect = packed_rect(
                chip_rect,
                e.side,
                e.along,
                &mut cursor,
                box_h(&mcu.modules[e.idx]),
            );
            boxes.push((e.idx, rect, e.conns.clone(), e.side, true, false));
        }
    }
    // Which way a box faces when its PINS cannot say.
    //
    // `dominant_side` is a majority vote over a module's wired pins, so a
    // floating module has no vote and a dragged one may have been pulled to the
    // other side of the chip entirely. Both used to be told `Side::Right`
    // outright, which was invisible while the box was a symmetric rectangle -
    // and stops being invisible the moment one end of it is shaped.
    let facing = |rect: egui::Rect| {
        let d = rect.center() - chip_center;
        if d.x.abs() >= d.y.abs() {
            if d.x >= 0.0 { Side::Right } else { Side::Left }
        } else if d.y >= 0.0 {
            Side::Bottom
        } else {
            Side::Top
        }
    };

    // Disconnected modules stack in the right margin.
    let mut fy = chip_rect.top();
    for i in floating_idx {
        let min = egui::pos2(chip_rect.right() + PIN_HEIGHT + PIN_GAP, fy);
        let h = box_h(&mcu.modules[i]);
        fy += h + BOX_GAP;
        let rect = egui::Rect::from_min_size(min, egui::vec2(BOX_W, h));
        boxes.push((i, rect, Vec::new(), facing(rect), false, false));
    }
    // Manually-dragged boxes: placed at chip centre + stored offset.
    for (i, conns) in manual_mods {
        let p = mcu.modules[i].pos;
        let rect = egui::Rect::from_min_size(
            chip_center + egui::vec2(p.0, p.1),
            egui::vec2(BOX_W, box_h(&mcu.modules[i])),
        );
        let connected = !conns.is_empty();
        boxes.push((i, rect, conns, facing(rect), connected, true));
    }

    // ── 3. Draw boxes + wires; detect a header click to expand the list entry. ─
    // Native runtime → the handle preview shows the split (Tx, Rx) for USART.
    let native_forced = mcu.is_native();
    // While a remove-confirm is open, pulse the target module's box red so it's
    // obvious on the diagram which module is about to be deleted. Computed once
    // per frame; only the matching box uses it. Repaint keeps the pulse alive.
    let removing_id = mcu.module_remove_confirm.clone();
    let blink = if removing_id.is_some() {
        ui.ctx().request_repaint();
        let phase = (ui.input(|i| i.time) * std::f64::consts::TAU * 1.8).sin();
        0.5 + 0.5 * phase as f32
    } else {
        0.0
    };
    let mut clicked_id: Option<String> = None;
    // Applied after the loop (can't mutate `mcu.modules` while it's borrowed by
    // the box `m`): dragged offsets, and right-click "reset to auto" requests.
    let mut drag_updates: Vec<(usize, (f32, f32))> = Vec::new();
    let mut reset_updates: Vec<usize> = Vec::new();
    let mut field_pass: Vec<(usize, egui::Rect)> = Vec::new();
    // Which device each module belongs to. Read BEFORE the loop, because
    // `group_of_module` borrows the whole `Mcu` and the loop already holds a
    // module out of it.
    // The device the canvas is pointing at, as an OWNED name: the loop below
    // holds `mcu` and the tail of this function takes it mutably.
    let active = mcu.active_device().map(str::to_owned);
    // The corridor every wire is routed down, computed once for the canvas.
    let wire_ring = super::wire::ring(chip_rect);
    // Every box, as an obstacle. A wire drawn across a box reads as though it
    // connects to it, which is the one thing a schematic line may never suggest
    // wrongly.
    let box_rects: Vec<egui::Rect> = boxes.iter().map(|(_, r, ..)| *r).collect();
    // ONE slot for every wire on the canvas, reserved before the boxes are
    // drawn and filled after. Wires used to be painted per box, inside the loop,
    // so a box packed later covered the wires of a box packed earlier - and a
    // device's highlight, which is the whole point of lighting them, was the
    // first thing to be cut in half by it.
    let wire_slot = painter.add(egui::Shape::Noop);
    // Halos first, wires after, so one wire's halo can never lie over its
    // neighbour's line.
    let mut wire_halos: Vec<egui::Shape> = Vec::new();
    let mut wire_lines: Vec<egui::Shape> = Vec::new();
    let box_groups: Vec<Option<String>> = boxes
        .iter()
        .map(|(i, ..)| {
            mcu.group_of_module(&mcu.modules[*i])
                .map(|g| g.name.clone())
        })
        .collect();
    // A box speaks for every pad it wires, including one another device holds:
    // `group_of_module` answers with the first pad's group, so without `covers`
    // the second device would draw a competing mat over the same box.
    members.extend(boxes.iter().zip(&box_groups).map(|((_, r, conns, ..), g)| {
        super::device_frame::Member {
            group: g.clone(),
            rect: *r,
            covers: conns.iter().map(|(_, _, n)| *n).collect(),
        }
    }));
    // A device being dragged moves every one of its parts. Seeded here, BEFORE
    // the box loop, so a box-header drag pushed later into the same vec still
    // wins for that box under last-write-wins - dragging one box out of a device
    // keeps working exactly as it did.
    //
    // An auto-packed box is converted to a manual position AT the slot the packer
    // just gave it, so nothing jumps on the first frame of the gesture and the
    // arrangement the user was looking at is what starts moving.
    if let Some((dev, (dx, dy))) = mcu.device_drag.clone() {
        for ((i, rect, ..), g) in boxes.iter().zip(&box_groups) {
            if g.as_deref().map(str::trim) == Some(dev.trim()) {
                let off = rect.min - chip_center + egui::vec2(dx, dy);
                drag_updates.push((*i, nudge(off)));
            }
        }
    }
    for (bi, (i, rect, conns, side, connected, manual)) in boxes.iter().enumerate() {
        let m = &mcu.modules[*i];
        let inst = m.instance();
        let removing = removing_id.as_deref() == Some(m.id.as_str());
        let title_rect = draw_box(
            painter,
            *rect,
            m,
            *side,
            *connected,
            module_color(m.kind, inst),
            native_forced,
            removing.then_some(blink),
            mcu.selected_module.as_deref() == Some(m.id.as_str()),
        );

        for (sig, anchor, anchor_pin) in conns {
            let color = signal_color(*sig, inst);
            // A dragged box's stored side no longer implies an edge, so aim the
            // wire at the box edge nearest the pin; auto boxes keep their side.
            let face = if *manual { facing(*rect) } else { *side };
            let adir = pin_anchor_dir(mcu, local_chip, rot, *anchor_pin)
                .map(|(_, d)| d)
                .unwrap_or_default();
            let (pts, term) = wire_path_for_box(
                wire_ring,
                super::wire::Blocked {
                    body: chip_rect,
                    boxes: &box_rects,
                    own: bi,
                },
                *rect,
                BoxShape::of(m.kind),
                face,
                *manual,
                *anchor,
                adir,
            );
            let lit = wire_lit(active.as_deref(), mcu, *anchor_pin);
            // Painted from the terminal the route actually chose, and only after
            // it has chosen.
            let dot = if lit.is_some() { 4.5 } else { 3.5 };
            painter.circle_filled(term, dot, color);
            painter.circle_filled(*anchor, dot, color);
            let (halo, line) = wire_shapes(
                &crate::panels::structure_map::gui::rounded_path(&pts, super::wire::WIRE_R),
                color,
                1.6,
                lit,
            );
            wire_halos.extend(halo);
            wire_lines.push(line);
            // Custom modules show the DATA DIRECTION: an MCU input is driven by
            // the device (module → pin), an output is driven by the MCU
            // (pin → module). Peripheral buses are bidirectional, so no head.
            if m.kind.is_custom() {
                let dir = mcu
                    .find_pin(*anchor_pin)
                    .map(|p| p.selected_function.clone());
                // The LAST leg of the route, not the two endpoints: on a wire
                // that turns, an arrow aimed straight from one end at the other
                // points off into the middle of the diagram.
                let n = pts.len();
                let (from, to) = match dir {
                    Some(PinFunction::GpioInput) => (pts[n - 2], pts[n - 1]),
                    Some(PinFunction::GpioOutput) | Some(PinFunction::TimerPwm { .. }) => {
                        (pts[1], pts[0])
                    }
                    _ => (term, term), // unknown direction → plain wire
                };
                if from != to {
                    arrow_head(painter, from, to, color);
                }
            }
        }

        // Drag the header area (above the rename field) to MOVE the box; a plain
        // click still expands the list entry. Right-click a moved box to reset.
        let header_rect =
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), rect.bottom() - 30.0));
        let resp = ui
            .interact(
                header_rect,
                ui.id().with(("vmod_box", *i)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        // The box has notes. The tooltip rides on the HEADER's own response
        // rather than a second widget over it: one on top would be the one the
        // pointer lands on, and the box has to stay draggable through the glyph.
        if let Some(preview) = mcu.notes_for(&mcu.modules[*i]).and_then(|n| n.preview()) {
            let shape = BoxShape::of(mcu.modules[*i].kind);
            if let Some(g) = note_glyph_rect(title_rect, *rect, shape, *side) {
                painter.text(
                    g.center(),
                    egui::Align2::CENTER_CENTER,
                    ph::NOTE,
                    egui::FontId::proportional(NOTE_GLYPH),
                    SUB_COLOUR,
                );
                if resp.hover_pos().is_some_and(|p| g.contains(p)) {
                    resp.clone().on_hover_text(preview);
                }
            }
        }
        if resp.dragged() && crate::panels::drag_decided(ui) {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            // drag_delta is already in scene coords (the Scene layer transform is
            // applied by egui), so it's correct at any zoom.
            let new_min = rect.min + resp.drag_delta();
            let off = new_min - chip_center;
            drag_updates.push((*i, (off.x, off.y)));
        }
        if resp.hovered() || resp.dragged() {
            // The SAME outline the box was drawn with. A rectangle here over a
            // chamfered box would read as a second, wrong border.
            let ring = egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(50));
            let shape = BoxShape::of(mcu.modules[*i].kind);
            if shape.is_rect() {
                painter.rect_stroke(*rect, 6.0, ring, egui::StrokeKind::Middle);
            } else {
                painter.add(egui::Shape::closed_line(
                    silhouette(*rect, shape, *side),
                    ring,
                ));
            }
        }
        if resp.clicked() {
            clicked_id = Some(m.id.clone());
        }
        if *manual {
            resp.context_menu(|ui| {
                if ui.button("Reset to auto position").clicked() {
                    reset_updates.push(*i);
                    ui.close();
                }
            });
        }
        field_pass.push((*i, *rect));
    }
    wire_halos.append(&mut wire_lines);
    painter.set(wire_slot, egui::Shape::Vec(wire_halos));

    // Apply drag / reset now that the box borrow of `mcu.modules` has ended.
    for (i, off) in drag_updates {
        mcu.modules[i].pos = off;
    }
    for i in reset_updates {
        mcu.modules[i].pos = (0.0, 0.0);
    }

    // ── 4. A Custom module's PIN names (mutable pass) ─────────────────────────
    // The only editable text left on a module box. It names a PIN, not the
    // module: the module's own name is drawn read-only by `draw_box`, and edited
    // in the Virtual-modules panel where there is room for it.
    //
    // Still a second pass over the same boxes because these need `&mut Mcu` to
    // reach `pin.custom_label`, and the painter pass above holds `&Mcu`.
    for (i, box_rect) in field_pass {
        // Same selection state the box was painted with, so its fields grow with
        // the rest of it. Read BEFORE the mutable `find_pin_mut` borrows below.
        let selected = mcu.selected_module.as_deref() == Some(mcu.modules[i].id.as_str());
        let scale = text_scale(selected);
        let selected_pin = mcu.selected_pin;
        // A Custom module groups its PINS' rename fields inside the box, under
        // the module name — instead of each pin floating separately beside the
        // chip (io_arrows skips them, see `module_owned_pins`).
        if mcu.modules[i].kind.is_custom() {
            let pins: Vec<usize> = match &mcu.modules[i].config {
                ModuleConfig::Custom(c) => c.pins.clone(),
                _ => Vec::new(),
            };
            for (row, num) in pins.iter().enumerate() {
                let r = custom_pin_row(box_rect, row);
                let name = mcu
                    .find_pin(*num)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| format!("pin{num}"));
                // A pin selected on the chip is called out HERE too — its row
                // (pin name + rename field) gets the same white border a lone
                // pin's field group gets out in the margin.
                let pin_sel = selected_pin == Some(*num);
                let row_scale = scale.max(text_scale(pin_sel));
                painter.text(
                    egui::pos2(box_rect.left() + 8.0, r.center().y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    egui::FontId::monospace(9.5 * row_scale),
                    if pin_sel {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_rgb(170, 175, 190)
                    },
                );
                if let Some(pin) = mcu.find_pin_mut(*num) {
                    ui.push_id(("custom_pin_label", i, num), |ui| {
                        ui.put(
                            r,
                            egui::TextEdit::singleline(&mut pin.custom_label)
                                .hint_text("name")
                                .font(egui::FontId::proportional(9.5 * row_scale)),
                        );
                    });
                }
                if pin_sel {
                    painter.rect_stroke(
                        egui::Rect::from_min_max(
                            egui::pos2(box_rect.left() + 4.0, r.top() - 2.0),
                            egui::pos2(r.right() + 2.0, r.bottom() + 2.0),
                        ),
                        4.0,
                        egui::Stroke::new(2.8_f32, egui::Color32::WHITE),
                        egui::StrokeKind::Middle,
                    );
                }
            }
        }
    }

    if let Some(id) = clicked_id {
        // One click does three things: select the box (click again to deselect,
        // like a pin), expand the module's entry in the list below, and light up
        // every line its pins bind in the code.
        mcu.selected_module = if mcu.selected_module.as_deref() == Some(id.as_str()) {
            None
        } else {
            Some(id.clone())
        };
        mcu.expand_module = Some(id.clone());
        mcu.module_goto = Some(id);
    }
}

fn dominant_side(conns: &[(ModuleSignal, egui::Pos2, Side, usize)]) -> Side {
    let mut count: HashMap<Side, usize> = HashMap::new();
    for (_, _, s, _) in conns {
        *count.entry(*s).or_insert(0) += 1;
    }
    // Most common; ties resolved by the first connection's side.
    conns
        .iter()
        .map(|(_, _, s, _)| *s)
        .max_by_key(|s| count[s])
        .unwrap_or(Side::Right)
}

fn parity_label(p: Parity) -> &'static str {
    match p {
        Parity::None => "None",
        Parity::Even => "Even",
        Parity::Odd => "Odd",
    }
}

fn stop_label(s: StopBits) -> &'static str {
    match s {
        StopBits::One => "1",
        StopBits::Two => "2",
    }
}

/// Configuration panel for one module: USART comm settings, the wired TX/RX
/// pins, and the user's RX/TX data model. `pin_names` maps pin number → name.
/// The channels of `timer` this chip still has a free pad for: every channel
/// any pin can serve, minus the ones already wired to the module.
///
/// Derived, never a fixed "CH2/3/4": TIM14 has one channel and TIM9 two, and a
/// package bonds only some of the pads a die carries — the truth is in the
/// pins' own function lists, which is also where the codegen reads the channel
/// set from (`pwm_wires`).
/// What a module's INSTANCE number means on this chip.
///
/// The one shared field whose meaning really moves: an STM32 timer module is a
/// `TIM`, an ESP one is an LEDC timer whose channels are not welded to pads at
/// all, and a Pico one is a PWM SLICE, which is welded to them. Saying "the
/// peripheral instance" on all three would be true and useless.
///
/// Chosen here — inside the function that already knows the family — and not in
/// the pane. The pane must not learn to branch on the family: that is how the
/// two ends of this start disagreeing.
fn shared_instance_doc(kind: ModuleKind, family: &str) -> &'static str {
    if kind.is_custom() {
        return docs::SHARED_INSTANCE_CUSTOM;
    }
    if kind != ModuleKind::GenericInterfaceTimer {
        return docs::SHARED_INSTANCE;
    }
    if crate::panels::mcu_module::codegen::rp::is_rp(family) {
        docs::SHARED_INSTANCE_TIMER_RP
    } else if crate::panels::mcu_module::codegen::family::is_esp(family) {
        docs::SHARED_INSTANCE_TIMER_ESP
    } else {
        docs::SHARED_INSTANCE_TIMER_STM32
    }
}

/// `"CH3"` -> `Some(3)`; `None` for `CH3N`, `BKIN` and every non-PWM signal.
///
/// The complementary pad is deliberately excluded: it is welded to its channel
/// on every part that has one, so there is nothing to choose.
fn pwm_plain_channel(sig: &str) -> Option<u8> {
    let rest = sig.strip_prefix("CH")?;
    rest.parse::<u8>().ok()
}

/// Which channels of `timer` this PAD could drive, the one it drives now
/// included.
///
/// # Why this is a question worth asking at all
///
/// On an STM32 it almost always answers with one: a pad carries one channel per
/// timer, so the pad IS the choice and there is nothing to pick. On an ESP it
/// answers with all of them — `LEDC` reaches the pins through the GPIO matrix,
/// so `esp_gen` gives every pad every channel.
///
/// Read from the pad's own function list rather than from a per-family table,
/// so the answer is the chip's and a part that breaks the rule breaks it here
/// too. `taken` drops the channels another pad of this same timer is already
/// driving: two pads on one channel is not something the generator can write.
fn pwm_channel_choices(
    timer: u8,
    pin: usize,
    pin_funcs: &HashMap<usize, Vec<PinFunction>>,
    taken: &BTreeSet<u8>,
) -> Vec<u8> {
    let mut out: Vec<u8> = pin_funcs
        .get(&pin)
        .into_iter()
        .flatten()
        .filter_map(|f| match f {
            PinFunction::TimerPwm { timer: t, channel } if *t == timer => Some(*channel),
            _ => None,
        })
        .filter(|c| !taken.contains(c))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The widest LEDC duty resolution `freq_hz` leaves room for, in bits.
///
/// Delegates to the GENERATOR rather than restating its formula: the picker must
/// not offer a width the generated file would refuse, and two copies of the same
/// arithmetic is exactly how that stops being true.
fn esp_ledc_max_bits(freq_hz: u32) -> u8 {
    crate::panels::mcu_module::codegen_esp_configs::ledc_duty_bits(freq_hz) as u8
}

/// The NARROWEST LEDC duty resolution `freq_hz` allows, in bits.
///
/// esp-hal bounds the divisor at BOTH ends, and only the upper one used to be
/// checked. At 50 Hz a 1-bit resolution leaves a divisor of 409_600 — over the
/// `0x3FFFF` ceiling — so `configure` returns Err(Divisor) and the generated
/// `unwrap` panics on the board.
fn esp_ledc_min_bits(freq_hz: u32) -> u8 {
    crate::panels::mcu_module::codegen_esp_configs::ledc_min_duty_bits(freq_hz) as u8
}

/// What a Pico owner can actually decide about a PWM slice.
///
/// Not the channel. On RP2040 and RP2350 the (slice, channel) pair falls out of
/// the GPIO NUMBER - slice `(n / 2) % 8`, channel A for an even pad and B for an
/// odd one - and the HALs enforce it in the type system: embassy-rp emits one
/// `impl ChannelAPin<PWM_SLICE0> for PIN_0` per pad, rp-hal one
/// `impl ValidPwmOutputPin<Pwm0, A> for Gpio0`. All 48 impls name 48 distinct
/// pads, and the four shipped RP definitions carry exactly one `TimerPwm` entry
/// per pin to match. A channel dropdown here could only ever hold one value, and
/// widening the pin data to give it a second would generate code the compiler
/// rejects - which is the ESP's freedom (`Ledc::channel(number, pad)` routes
/// through the GPIO matrix) read as if it were universal.
///
/// What IS a choice is the PAD: each (slice, channel) is reachable from two
/// GPIOs sixteen apart, and the slice's other channel has its own two.
fn rp_pwm_notes(
    slice: u8,
    conn_rows: &[(&'static str, String, usize)],
    pin_funcs: &HashMap<usize, Vec<PinFunction>>,
    pin_names: &HashMap<usize, String>,
) -> Vec<String> {
    // Every pad this chip could route to (slice, channel), in pin order. Read
    // from the chip's own table rather than from the arithmetic: slice 7 is
    // half-populated on an RP2040, and a W board loses four pads to the radio.
    let pads_for = |channel: u8| -> Vec<String> {
        let mut v: Vec<(usize, String)> = pin_funcs
            .iter()
            .filter(|(_, fns)| {
                fns.iter().any(|f| {
                    matches!(f, PinFunction::TimerPwm { timer, channel: c }
                             if *timer == slice && *c == channel)
                })
            })
            .map(|(n, _)| {
                (
                    *n,
                    pin_names
                        .get(n)
                        .cloned()
                        .unwrap_or_else(|| format!("pin{n}")),
                )
            })
            .collect();
        v.sort_unstable();
        v.into_iter().map(|(_, name)| name).collect()
    };
    let letter = |c: u8| if c == 1 { 'A' } else { 'B' };

    // Each of these is ONE source line on purpose. A `\`-continued literal is
    // rejoined with its own indentation, and the details pane would draw the run
    // of spaces - the same trap `RP_DMA_NOTE` above is written around.
    let mut out = vec![
        "On this chip the pad decides the channel - slice (GPIO / 2) mod 8 for GP0..31 and 8 + ((GPIO - 32) / 2) mod 4 for GP32..47 on an RP2350B, channel A for an even GPIO and B for an odd one - and both HALs make that a compile-time bound, so the channel is not selectable here."
            .to_owned(),
    ];
    // Which pad each channel actually GETS. Two pads sixteen apart share every
    // (slice, channel) here, so a canvas really can wire both - and the emitter
    // resolves that by sorting on (slice, channel, GPIO) and keeping the first
    // (`codegen::rp`, the `pads` map). The panel has to resolve it the same way
    // and by the same rule, or these notes claim a pad is driving an output the
    // generated file left out.
    let mut driver: BTreeMap<u8, (u8, String)> = BTreeMap::new();
    let mut lost: Vec<(u8, String, String)> = Vec::new();
    for (sig, pin, _) in conn_rows {
        let Some(ch) = pwm_plain_channel(sig) else {
            continue;
        };
        let gp = crate::panels::mcu_module::codegen::rp::gpio_index(pin).unwrap_or(u8::MAX);
        match driver.get(&ch) {
            Some((held, holder)) if *held <= gp => {
                lost.push((ch, pin.clone(), holder.clone()));
            }
            Some((_, holder)) => {
                lost.push((ch, holder.clone(), pin.clone()));
                driver.insert(ch, (gp, pin.clone()));
            }
            None => {
                driver.insert(ch, (gp, pin.clone()));
            }
        }
    }
    // A pad that lost the clash is not "an alternative" - it is wired and
    // silent, which is the one thing worth saying before the board is soldered.
    for (ch, out_pad, holder) in &lost {
        out.push(format!(
            "{out_pad} is wired to slice {slice} channel {} as well, but one channel reaches one pad - {holder} drives it and {out_pad} gets no code.",
            letter(*ch),
        ));
    }
    for (ch, (_, pin)) in &driver {
        // Only pads that are FREE are alternatives; a wired one is covered by
        // the clash line above.
        let others: Vec<String> = pads_for(*ch)
            .into_iter()
            .filter(|p| p != pin && !conn_rows.iter().any(|(_, w, _)| w == p))
            .collect();
        if !others.is_empty() {
            out.push(format!(
                "{pin} drives slice {slice} channel {} - and so can {}, if that pad suits the board better.",
                letter(*ch),
                others.join(" / "),
            ));
        }
    }
    // The sibling channel, named in this chip's own vocabulary. The generic note
    // says "assign TIM3 CH2", which is STM32 wording the Pico canvas never uses.
    for ch in [1u8, 2] {
        if conn_rows
            .iter()
            .any(|(sig, _, _)| pwm_plain_channel(sig) == Some(ch))
        {
            continue;
        }
        let pads = pads_for(ch);
        if !pads.is_empty() {
            out.push(format!(
                "Slice {slice} channel {} is free - wire {} on the Pins canvas and it joins this module, sharing its frequency.",
                letter(ch),
                pads.join(" / "),
            ));
        }
    }
    out
}

/// The pads the AUTOMATIC pick would take, as a short label for the palette.
///
/// Shown on the "Auto" entry so the choice is informed before it is made -
/// seeing `Auto - GP24 / GP25` next to `Choose pins...` is what tells a Pico
/// owner that the automatic wiring is about to take the on-board LED. `None`
/// when nothing is free, which the palette already renders as a greyed entry.
///
/// This runs the same `pick_pins` the add itself will run, so the label cannot
/// promise pads the add then declines to use.
pub fn auto_wiring_summary(
    mcu: &crate::panels::mcu_module::mcu::Mcu,
    kind: ModuleKind,
) -> Option<String> {
    use crate::panels::mcu_module::modules::autowire;
    // A Custom module claims no peripheral and wires nothing, so there is never
    // a wiring to preview.
    if kind.is_custom() {
        return None;
    }
    let (required, optional) = kind.signals();
    let used: std::collections::HashSet<usize> = mcu
        .modules
        .iter()
        .flat_map(|m| m.connections.iter().map(|c| c.mcu_pin))
        .collect();
    let used_instances: std::collections::HashSet<u8> = mcu
        .modules
        .iter()
        .filter(|m| m.kind == kind)
        .map(|m| m.instance())
        .collect();
    let (_, chosen) = autowire::pick_pins(mcu, &used, &used_instances, required, optional)?;
    let names: Vec<String> = chosen
        .iter()
        .filter_map(|(_, n)| mcu.find_pin(*n))
        .map(|p| p.name.clone())
        .collect();
    (!names.is_empty()).then(|| names.join(" / "))
}

/// Whether this family moves a peripheral's pads as ONE group, so a single
/// signal cannot be re-pointed on its own.
///
/// True only on stm32f1, and only for a bus that has more than one pad. There
/// one AFIO bit remaps ALL of a peripheral's signals together: SPI1 is
/// PA5/PA6/PA7 or PB3/PB4/PB5, I2C1 is PB6/PB7 or PB8/PB9, and nothing mixed.
/// `stm32f1xx_hal` encodes that in its `Pins` impls, so a mixed set is not an
/// odd choice - it is a project that does not compile, which is the failure
/// `auto_assign_partners` was rewritten to stop producing (see its doc).
///
/// The chip definitions carry NO remap-group data - `autowire` only ever
/// approximated it with the `ports` score, and that approximation cannot even
/// see the I2C case, where both groups live on port B. So the honest move is to
/// not offer the choice here rather than to offer it with a warning: a pad
/// picker that generates an unbuildable project is worse than no pad picker.
/// Single-pad signals (a timer channel, an ADC input) are unaffected.
fn f1_moves_as_a_group(family: &str, module_pads: usize) -> bool {
    family == "stm32f1" && module_pads > 1
}

/// Pads that could carry `want` and are FREE, other than the one holding it.
///
/// The chip's own answer, uncapped. `autowire`'s search looks at the first
/// `MAX_PER_SIGNAL` pads per signal because it has a combination budget to live
/// within; a menu has none, and on a GPIO-matrix part the two numbers are far
/// apart - an ESP32-C3 offers 21 pads for a UART TX. Capping the menu the way
/// the search is capped would hide exactly the pad the user opened it to find.
///
/// "Free" is `Unset`, not "not blocked": a pad already carrying another
/// peripheral cannot take this one without silently unwiring that, which is a
/// second edit the user did not ask for.
fn free_pads_for(
    want: &PinFunction,
    holder: usize,
    pin_funcs: &HashMap<usize, Vec<PinFunction>>,
    pin_funcs_current: &HashMap<usize, PinFunction>,
    // Number of pads the module has. On stm32f1 a multi-pad peripheral cannot
    // move ONE of them: see `f1_moves_as_a_group`.
    family: &str,
    module_pads: usize,
) -> Vec<usize> {
    if f1_moves_as_a_group(family, module_pads) {
        return Vec::new();
    }
    let mut v: Vec<usize> = pin_funcs
        .iter()
        .filter(|(n, fns)| {
            **n != holder
                && fns.contains(want)
                && pin_funcs_current
                    .get(n)
                    .is_none_or(|f| *f == PinFunction::Unset)
        })
        .map(|(n, _)| *n)
        .collect();
    v.sort_unstable();
    v
}

fn free_pwm_channels(
    timer: u8,
    wired: &BTreeSet<String>,
    pin_funcs: &HashMap<usize, Vec<PinFunction>>,
) -> Vec<String> {
    let on_chip: BTreeSet<String> = pin_funcs
        .values()
        .flatten()
        .filter_map(|f| match f {
            PinFunction::TimerPwm { timer: t, channel } if *t == timer => {
                Some(format!("CH{channel}"))
            }
            // A complementary pad is offered the same way — it is one more
            // signal to assign on the canvas — but only where embassy can
            // actually drive it.
            PinFunction::TimerPwmN { timer: t, channel }
                if *t == timer && is_advanced_timer(*t) =>
            {
                Some(format!("CH{channel}N"))
            }
            // Break lives on the same driver, so it is offered under the same
            // rule: only where embassy can actually reach it.
            PinFunction::TimerBreak { timer: t, input } if *t == timer && is_advanced_timer(*t) => {
                Some(if *input == 1 {
                    "BKIN".to_owned()
                } else {
                    format!("BKIN{input}")
                })
            }
            _ => None,
        })
        .collect();
    on_chip.difference(wired).cloned().collect()
}

/// The colour a baud verdict is drawn in: dim when fine or unjudged, then the
/// Configuration tab's amber and the Serial tab's red.
fn baud_color(s: Option<crate::panels::mcu_module::uart_baud::Severity>) -> egui::Color32 {
    use crate::panels::mcu_module::uart_baud::Severity;
    match s {
        None | Some(Severity::Ok) => egui::Color32::from_gray(140),
        Some(Severity::Marginal) => egui::Color32::from_rgb(235, 150, 90),
        Some(Severity::Broken) => egui::Color32::from_rgb(220, 90, 80),
    }
}

/// The USART/LPUART "Baud rate" cell: the shared picker (the Serial tab's, so a
/// rate set here stays selectable there), then what the chip's UART makes of
/// the rate at the current clock.
///
/// Two short lines rather than one wrapped sentence, because a grid cell does
/// not wrap; the full explanation is on the first line's hover. The presets
/// carry their own error in the dropdown, so a bad one is visible before it is
/// picked.
fn baud_row(ui: &mut egui::Ui, baud: &mut u32, plan: &crate::panels::mcu_module::uart_baud::Plan) {
    use crate::panels::mcu_module::uart_baud as ub;
    ui.vertical(|ui| {
        crate::serial::baud_picker(ui, "baud", baud, ub::typed_range(plan), &|b| {
            ub::preset_tag(plan, b).map(|(text, sev, hover)| crate::serial::BaudTag {
                text,
                color: baud_color(Some(sev)),
                hover,
            })
        });
        let h = ub::hint(plan, *baud);
        ui.label(
            egui::RichText::new(&h.verdict)
                .size(10.5)
                .color(baud_color(h.severity)),
        )
        .on_hover_text(&h.why);
        if let Some(range) = &h.range {
            ui.label(
                egui::RichText::new(range)
                    .size(10.5)
                    .color(egui::Color32::from_gray(120)),
            );
        }
    });
}

pub fn module_config_ui(
    ui: &mut egui::Ui,
    m: &mut VirtualModule,
    pin_names: &HashMap<usize, String>,
    // Per-pin `function|label` fingerprint — a Custom module compares it with
    // what its generated code was built from (see `has_pending_pins`).
    pin_sigs: &HashMap<usize, String>,
    // Pins a Custom module may not take: reserved, assigned, or owned elsewhere.
    pin_blocked: &std::collections::HashSet<usize>,
    // Editable per-pin labels — a Custom module's rows mirror the name field
    // inside its box on the canvas; the caller writes the changes back.
    pin_labels: &mut HashMap<usize, String>,
    // Each pin's CURRENT function — a Custom module needs it to tell a configured
    // pin from one still waiting for In / Out / ADC / PWM.
    pin_funcs_current: &HashMap<usize, PinFunction>,
    // Functions selectable per pin, behind the pin-name buttons.
    pin_funcs: &HashMap<usize, Vec<PinFunction>>,
    // Set when a function is picked from a pin button; the caller applies it.
    pin_fn_choice: &mut Option<PinEdit>,
    // EMBASSY async. The ESP has an async runtime too, but none of its rows
    // turn on it: `with_dma`, `UartTx::new` and `.with_cts()` are all on the
    // blocking drivers, so what decides there is the FAMILY (`esp` below).
    is_async: bool,
    is_native: bool,
    // The chip's family key — the blocking DMA transport is stm32f1xx-hal's,
    // so only the F1 backend can emit it.
    family: &str,
    // The STAGED `(api_style, async_mode)` for this module — the api/async row
    // edit THIS, not `m.config`, so nothing regenerates until "Apply".
    pending: &mut (ApiStyle, AsyncBusMode),
    // The chip's DMA facts, for the channel picker. Cloned by the caller rather
    // than borrowed, because `m` is a mutable borrow out of the same `Mcu`.
    dma: Option<&crate::panels::mcu_module::mcu_def::DmaDef>,
    // Whether this chip's USART has the swap / invert bits. Passed in rather
    // than derived here, because it is a fact about the CHIP and this function
    // only sees one module.
    line_extras: bool,
    // On an nRF, the peripheral sharing this module's block with it (`TWIM0`
    // under SPIM0, and the reverse), from `nrf::shared_block_partner`. A fact
    // about the OTHER modules, so it comes from the caller like `line_extras`.
    block_partner: Option<&str>,
    // Everything this module has to say that is NOT a control: the standing
    // remarks ("duty is taken in whole percent"), and what each row it draws
    // MEANS. Both are drawn by the details pane, not inside the config grid.
    //
    // Out-parameter rather than a second function, because working them out
    // means the same reading of `m.config` and the pin map the controls above
    // already did — recomputing it elsewhere is how the note and the control
    // end up disagreeing. That is not a style preference: nine gate mechanisms,
    // some forty `if`s and three early `return`s decide which rows exist, and a
    // second pass would have to mirror every one of them.
    out: &mut ConfigOut,
    // What the chip's UART does with a baud rate at the CURRENT clock. Read by
    // the caller before `mcu.modules` is borrowed, like `dma`.
    baud_chip: &crate::panels::mcu_module::uart_baud::Chip,
) {
    // Read what we need off `m` BEFORE `m.config` is borrowed mutably below.
    let is_custom = m.kind.is_custom();
    // Which DMA request table the serial arm below asks. Read from the KIND here,
    // because inside the match the two share one binding (`UsartModuleConfig`).
    let uart_bus = if m.kind == ModuleKind::GenericInterfaceLpuart {
        dma_map::Bus::Lpuart
    } else {
        dma_map::Bus::Usart
    };
    let m_id = m.id.clone();
    // Read alongside `is_custom`, for the same reason: the rows at the bottom
    // are drawn long after `m.config` is borrowed mutably.
    let m_kind = m.kind;
    let m_inst = m.instance();
    // Connection rows (generic over kind), computed before borrowing config.
    //
    // The pin NUMBER rides along with the name: the timer rows below turn it
    // into a channel picker, and `pin_funcs` is keyed by number.
    let conn_rows: Vec<(&'static str, String, usize)> = m
        .connections
        .iter()
        .map(|c| {
            let pin = pin_names
                .get(&c.mcu_pin)
                .cloned()
                .unwrap_or_else(|| format!("pin{}", c.mcu_pin));
            (c.signal.label(), pin, c.mcu_pin)
        })
        .collect();
    // Whether an SPI module has a receive line at all. Read from its own wiring
    // for the same reason as `wired_flow` below.
    let has_miso = m.connections.iter().any(|c| c.signal == ModuleSignal::Miso);
    // Which pad of each PAIR this module actually has. On the F1 both are
    // required — see `f1_half_bus_note`.
    let has_sig = |want: ModuleSignal| m.connections.iter().any(|c| c.signal == want);
    let wired_serial = (
        has_sig(ModuleSignal::Tx) || has_sig(ModuleSignal::LpTx),
        has_sig(ModuleSignal::Rx) || has_sig(ModuleSignal::LpRx),
    );
    let wired_i2c = (has_sig(ModuleSignal::Scl), has_sig(ModuleSignal::Sda));
    let wired_can = (has_sig(ModuleSignal::CanTx), has_sig(ModuleSignal::CanRx));
    let wired_usb = (has_sig(ModuleSignal::UsbDm), has_sig(ModuleSignal::UsbDp));
    // Which flow-control pads this module actually has, read from its own
    // wiring — the flow selector warns against THIS, not against a guess.
    let wired_flow = (
        m.connections
            .iter()
            .any(|c| matches!(c.signal, ModuleSignal::Cts | ModuleSignal::LpCts)),
        m.connections
            .iter()
            .any(|c| matches!(c.signal, ModuleSignal::Rts | ModuleSignal::LpRts)),
    );

    // Several rows turn on the FAMILY rather than the runtime: the ESP's UART
    // direction and flow exist on both its runtimes, and its Init API exists on
    // neither.
    let esp = crate::panels::mcu_module::codegen::family::is_esp(family);

    // Portable (embedded-io/hal) vs native (concrete HAL) init — shown for the
    // bus modules (USART/SPI/I2C) that generate a `pins/configs/*.rs` init.
    //
    // On an ESP it is shown LOCKED. The choice is an STM32 one: the bridge it
    // selects is `embedded-io`/`embedded-hal` over a stm32f1xx-hal type, and no
    // ESP emitter reads `api_style` at all — `codegen_esp_configs` always
    // returns the esp-hal driver. Left editable it was a control that changed
    // nothing, which is worse than one that is visibly fixed.
    let api_row = |ui: &mut egui::Ui, out: &mut ConfigOut, style: &mut ApiStyle| {
        if esp {
            out.field("Init API", docs::INIT_API_ESP);
            ui.label("Init API");
            let resp = ui.add_enabled_ui(false, |ui| {
                egui::ComboBox::from_id_salt("api_style_esp")
                    .selected_text("Native (esp-hal type)")
                    .show_ui(ui, |_ui| {});
            });
            resp.response.on_hover_text(
                "esp-hal only. `init` hands back the concrete esp-hal driver - `Uart`, \
                 `Spi`, `I2c` - because that is what its own traits are implemented on. \
                 The Portable bridge is an STM32F1 choice.",
            );
            ui.end_row();
            return;
        }
        out.field("Init API", docs::INIT_API);
        ui.label("Init API");
        egui::ComboBox::from_id_salt("api_style")
            .selected_text(match style {
                ApiStyle::Portable => "Portable (embedded-io/hal)",
                ApiStyle::Native => "Native (HAL type)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(style, ApiStyle::Portable, "Portable (embedded-io/hal)")
                    .on_hover_text(
                        "init returns a STANDARD embedded-io / embedded-hal 1.0 value — \
                         portable driver code across HALs. Its `.0` still gives the raw \
                         HAL object back.",
                    );
                ui.selectable_value(style, ApiStyle::Native, "Native (HAL type)")
                    .on_hover_text(
                        "init returns the concrete stm32f1xx-hal type — no bridge, no extra \
                         trait crates, max HAL features.",
                    );
            });
        ui.end_row();
    };

    // Native RUNTIME forces every peripheral to the concrete HAL — show the Init
    // API row DISABLED + locked on Native (instead of hiding it), so it's clear
    // the choice is fixed here, not missing.
    let api_row_locked = |ui: &mut egui::Ui, out: &mut ConfigOut| {
        out.field("Init API", docs::INIT_API_NATIVE);
        ui.label("Init API");
        let resp = ui.add_enabled_ui(false, |ui| {
            egui::ComboBox::from_id_salt("api_style_locked")
                .selected_text("Native (HAL type)")
                .show_ui(ui, |_ui| {});
        });
        resp.response.on_hover_text(
            "Locked by the Native runtime — every peripheral uses the concrete HAL type. \
             Switch the Runtime (System tab) to choose per module.",
        );
        ui.end_row();
    };

    // Blocking runtime on the F1: poll each byte, or hand the bus to DMA.
    //
    // A checkbox rather than a channel picker, because there is nothing to pick
    // — stm32f1xx-hal fixes the channel per peripheral in its types. The label
    // names them anyway, so the choice is not a black box.
    //
    // `rx_ok` is false for an SPI with no MISO: there is no receive line, so
    // the two receiving choices are dropped and a stored one is narrowed on
    // sight — the same clamp codegen applies, done where the user can see it.
    let transport_row =
        |ui: &mut egui::Ui, out: &mut ConfigOut, on: &mut BlockingDma, chans: &str, rx_ok: bool| {
            out.field("Transport", docs::BLOCKING_TRANSPORT);
            if !rx_ok {
                *on = on.without_rx();
            }
            ui.label("Transport");
            egui::ComboBox::from_id_salt("blocking_dma")
                .selected_text(on.label())
                .show_ui(ui, |ui| {
                    for v in BlockingDma::ALL.into_iter().filter(|v| rx_ok || !v.rx()) {
                        ui.selectable_value(on, v, v.label())
                            .on_hover_text(match v {
                                BlockingDma::Off => {
                                    "The CPU moves every byte, both ways.".to_owned()
                                }
                                BlockingDma::Both => format!(
                                    "Both directions on DMA ({chans}), and both channels reserved."
                                ),
                                // The half-and-half cases are the interesting ones,
                                // so each says what it buys.
                                BlockingDma::Tx => format!(
                                    "TX on DMA, RX polled. Frees the RX channel; `init` returns a \
                                 DMA transmitter and the HAL's ordinary `nb` receiver ({chans})."
                                ),
                                BlockingDma::Rx => format!(
                                    "RX on DMA, TX written by the CPU - the usual choice when \
                                 receiving must not drop bytes but sending is short bursts. \
                                 Frees the TX channel ({chans})."
                                ),
                            });
                    }
                });
            ui.end_row();
        };

    // With DMA on, the Init API choice has no object: the handles are the HAL's
    // own DMA types either way. Shown locked rather than hidden, same as under
    // the Native runtime.
    let api_row_locked_dma = |ui: &mut egui::Ui, out: &mut ConfigOut| {
        out.field("Init API", docs::INIT_API_DMA);
        ui.label("Init API");
        let resp = ui.add_enabled_ui(false, |ui| {
            egui::ComboBox::from_id_salt("api_style_locked_dma")
                .selected_text("DMA handles (HAL type)")
                .show_ui(ui, |_ui| {});
        });
        resp.response.on_hover_text(
            "Locked by the DMA transport - `init` returns TxDma/RxDma (USART) or \
             SpiRxTxDma (SPI), which are stm32f1xx-hal's own types. There is no portable \
             bus to bridge them to; turn DMA off to choose again.",
        );
        ui.end_row();
    };

    // Async runtime only (SPI/I2C): blocking driver vs async-DMA.
    //
    // The wording is deliberately HAL-neutral. It named embassy's constructors
    // outright, on a row an Espressif chip sees too — where the driver is
    // esp-hal's and the DMA is the GDMA. Whatever the chip, the choice is the
    // same one: a standard blocking bus, or an .await-able one on DMA.
    let async_row = |ui: &mut egui::Ui, out: &mut ConfigOut, mode: &mut AsyncBusMode| {
        out.field("Async init", docs::ASYNC_INIT);
        ui.label("Async init");
        egui::ComboBox::from_id_salt("async_mode")
            .selected_text(match mode {
                AsyncBusMode::Blocking => "Blocking (embedded-hal 1.0)",
                AsyncBusMode::AsyncDma => "Async-DMA (embedded-hal-async)",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(mode, AsyncBusMode::Blocking, "Blocking (embedded-hal 1.0)")
                    .on_hover_text(
                        "A STANDARD blocking embedded-hal 1.0 bus. \
                         No DMA — compiles out of the box. Fine inside an async project.",
                    );
                ui.selectable_value(
                    mode,
                    AsyncBusMode::AsyncDma,
                    "Async-DMA (embedded-hal-async)",
                )
                .on_hover_text(
                    "An .await-able bus whose bytes are moved by DMA. On an STM32 the 
                         channels are allocated for you (or pinned below); on an 
                         ESP32 it is the one GDMA channel esp-hal takes, passed 
                         from main.rs. Either way the Configuration tab's DMA card 
                         shows what was taken.",
                );
            });
        ui.end_row();
    };

    // Async runtime only (USART): interrupt ring buffer vs DMA. A separate
    // enum from `AsyncBusMode` because "blocking" is not one of the options —
    // an async USART is never blocking, only differently non-blocking.
    // The ESP's own transport row. Same CHOICE as embassy's `async_row` and a
    // different mechanism behind it, so it says the mechanism: esp-hal takes a
    // channel in `with_dma`, and the driver that comes back is a different type
    // with different methods.
    let esp_dma_row = |ui: &mut egui::Ui, out: &mut ConfigOut, mode: &mut AsyncBusMode| {
        out.field("Transfers", docs::ESP_TRANSFERS);
        ui.label("Transfers");
        egui::ComboBox::from_id_salt("esp_dma")
            .selected_text(match mode {
                AsyncBusMode::Blocking => "CPU (blocking)",
                AsyncBusMode::AsyncDma => "DMA",
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(mode, AsyncBusMode::Blocking, "CPU (blocking)")
                    .on_hover_text(
                        "The CPU moves every byte. Simplest, and fine for short \
                         transfers.",
                    );
                ui.selectable_value(mode, AsyncBusMode::AsyncDma, "DMA")
                    .on_hover_text(
                        "esp-hal's `with_dma`: the peripheral moves the bytes itself. \
                         Takes one of the chip's channels - the Configuration tab \
                         shows which one it got.",
                    );
            });
        ui.end_row();
    };

    // The ESP channel picker. `dma_plan` reserves a hand-pinned channel BEFORE
    // it allocates the rest, so naming one here takes it out of the pool rather
    // than fighting over it.
    //
    // The list is the same one the allocator would draw from: every channel on
    // a pooled DMA, and only the ones the request table names on a bolted one
    // (the original ESP32 and the S2, whose `DMA_SPI2` serves SPI2 alone).
    let esp_dma_channel_row = |ui: &mut egui::Ui,
                               out: &mut ConfigOut,
                               dma: Option<&crate::panels::mcu_module::mcu_def::DmaDef>,
                               request: &str,
                               chan: &mut String| {
        out.field("DMA channel", docs::ESP_DMA_CHANNEL);
        ui.label("DMA channel");
        // The allocator's own list, not a second one built to match it.
        let options = crate::panels::mcu_module::codegen_esp::dma_candidates(dma, request);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(format!("esp_dma_ch_{request}"))
                .selected_text(if chan.is_empty() {
                    "Automatic".to_owned()
                } else {
                    chan.clone()
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(chan, String::new(), "Automatic");
                    for c in &options {
                        ui.selectable_value(chan, c.clone(), c);
                    }
                });
            if options.is_empty() {
                ui.label(
                    egui::RichText::new("no channel data for this chip")
                        .size(10.5)
                        .color(egui::Color32::from_rgb(220, 160, 70)),
                );
            }
        })
        .response
        .on_hover_text(
            "Automatic lets the generator pick a free one. Naming a channel pins \
             it: it is reserved before anything else is handed out, which is how \
             two buses are kept off each other.",
        );
        ui.end_row();
    };

    // The two transports, and the DMA half means something DIFFERENT per HAL.
    // On embassy-stm32 it is `RingBufferedUartRx`, a circular buffer the
    // controller keeps filling between reads. embassy-rp has no such type at
    // all: `UartRx<Async>::read` starts one transfer per call, so between two
    // reads the only thing holding bytes is the PL011's 32-byte FIFO. Naming
    // the STM32 mechanism on a Pico promised a protection the chip does not
    // give.
    let usart_mode_row = |ui: &mut egui::Ui,
                          out: &mut ConfigOut,
                          mode: &mut UsartMode,
                          family: &str| {
        out.field("Async transport", docs::USART_ASYNC_TRANSPORT);
        let rp = crate::panels::mcu_module::codegen::rp::is_rp(family);
        let dma_text = if rp {
            "DMA (one transfer per read)"
        } else {
            "DMA (ring buffer)"
        };
        ui.label("Async transport");
        egui::ComboBox::from_id_salt("usart_mode")
            .selected_text(match mode {
                UsartMode::Buffered => "Buffered (interrupt)",
                UsartMode::Dma => dma_text,
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(mode, UsartMode::Buffered, "Buffered (interrupt)")
                    .on_hover_text(if rp {
                        "embassy-rp BufferedUart - one interrupt per byte into two software rings. Takes NO DMA channel, and it is the only RP uart type implementing embedded-io-async Read: the DMA Uart implements no embedded-io trait at all. Each ring is the RX/TX buffer size set above."
                    } else {
                        "embassy BufferedUart -> embedded-io-async Read + Write, one interrupt per byte into a software ring buffer. Needs no DMA channel, so it compiles out of the box."
                    });
                ui.selectable_value(mode, UsartMode::Dma, dma_text)
                    .on_hover_text(if rp {
                        "embassy-rp Uart::new - the peripheral talks to DMA directly and takes TWO channels, one per direction. There is no RingBufferedUartRx on this chip, so reception does not continue between your reads: what arrives in the gap is held only by the 32-byte hardware FIFO. This type implements no embedded-io trait, only inherent async read/write."
                    } else {
                        "UartTx + RingBufferedUartRx -> the same embedded-io-async traits, but the peripheral talks to DMA directly and RX keeps filling a circular buffer between your reads, so bytes are not dropped in the gaps. Takes TWO channels on a bidirectional UART - embassy's constructor requires both. To spend one, set Data Direction to RX only or TX only; to keep both directions but send from the CPU, the generated file shows `blocking_write` on the same handle."
                    });
            });
        ui.end_row();
    };

    // What the BLOCKING stm32f1xx-hal cannot do, said out loud.
    //
    // Direction, flow control, half duplex and swap/invert are all async-only
    // rows above. On the F1 they are not "not implemented yet": `Pins<USART>` is
    // implemented ONLY for the (TX alternate, RX input) pair, `serial.rs` has no
    // hdsel / rtse / ctse at all, and the SWAP / TXINV / RXINV bits do not exist
    // in F1 silicon. Leaving the rows out silently makes a user who just used
    // them on a G0 project hunt for them; this says why.
    let f1_serial_note = |ui: &mut egui::Ui, out: &mut ConfigOut| {
        for row in ["Data direction", "Line", "Hardware flow control"] {
            out.skip(row, docs::SKIP_F1_SERIAL);
        }
        ui.label("");
        ui.label(
            egui::RichText::new(
                "stm32f1xx-hal has no flow control, half duplex or one-way UART (its Serial takes the TX+RX pair), and the F1 USART has no swap/invert bits",
            )
            .size(10.5)
            .color(egui::Color32::from_gray(140)),
        );
        ui.end_row();
    };

    // Half a USART — or half an I2C — generates NOTHING on the F1: both HALs
    // take the PAIR (`serial::Pins`, `i2c::Pins`) and have no placeholder for
    // the pad that is missing. main.rs says so where the init would have gone;
    // this says so before the user gets there, since a peripheral quietly
    // absent from main.rs is easy to miss.
    //
    // `pads` is (first, second) in the order the pair is named, `wired` says
    // which of them the module actually has, and `why` is the reason clause —
    // three of these are a HAL `Pins` bound, the USB one is not.
    let f1_half_bus_note =
        |ui: &mut egui::Ui, bus: &str, pads: (&str, &str), why: &str, wired: (bool, bool)| {
            let missing = match wired {
                (true, false) => pads.1,
                (false, true) => pads.0,
                _ => return,
            };
            ui.label("");
            ui.label(
                egui::RichText::new(format!(
                    "{missing} is not wired, so this {bus} is not initialised at all — \
                 {why}. Assign the {missing} pad on the canvas."
                ))
                .size(10.5)
                .color(egui::Color32::from_rgb(220, 160, 70)),
            );
            ui.end_row();
        };

    // Data direction + hardware flow control. Both lists come from
    // `UsartDirection::options_for` / `UsartFlow::options_for`, i.e. from what
    // THIS FAMILY's backend has a CONSTRUCTOR for — never from what the silicon
    // could in principle do. The three backends differ a lot: embassy builds
    // every shape, the ESP builds a single direction and RTS/CTS, and the F1
    // builds one TX+RX port and nothing else.
    // `wired` says which of CTS/RTS the module actually has a pin for, so a
    // choice that needs one it hasn't got is called out instead of silently
    // generating code that won't build.
    let direction_row = |ui: &mut egui::Ui, out: &mut ConfigOut, cfg: &mut UsartModuleConfig| {
        let opts = UsartDirection::options_for(cfg.mode, family);
        // The locked form is a different fact, so it is a different
        // sentence — chosen HERE, by the branch that knows which one it
        // drew, and not re-derived from the family somewhere else.
        out.field(
            "Data direction",
            if opts.len() > 1 {
                docs::USART_DIRECTION
            } else if crate::panels::mcu_module::codegen::rp::is_rp(family) {
                // The same three-way choice the hover below makes. They read
                // ONE const each, so a fourth form cannot reach one and not the
                // other.
                docs::USART_DIRECTION_LOCKED_RP
            } else {
                docs::USART_DIRECTION_LOCKED
            },
        );
        ui.label("Data direction");
        if opts.len() == 1 {
            // Locked rather than hidden: the reason is the useful part.
            ui.add_enabled_ui(false, |ui| {
                egui::ComboBox::from_id_salt("usart_dir_locked")
                    .selected_text(UsartDirection::TxRx.label())
                    .show_ui(ui, |_ui| {});
            })
            .response
            .on_hover_text(if crate::panels::mcu_module::codegen::rp::is_rp(family) {
                docs::USART_DIRECTION_LOCKED_RP
            } else {
                docs::USART_DIRECTION_LOCKED
            });
        } else {
            egui::ComboBox::from_id_salt("usart_dir")
                .selected_text(cfg.direction.label())
                .show_ui(ui, |ui| {
                    for d in opts {
                        ui.selectable_value(&mut cfg.direction, *d, d.label());
                    }
                });
        }
        ui.end_row();
        // Line-level extras. On an older USART the register bits do not exist —
        // and neither do embassy's `Config` fields — so the row says that
        // instead of offering a switch that cannot be generated.
        out.field(
            "Line",
            if line_extras {
                docs::USART_LINE
            } else {
                docs::USART_LINE_ABSENT
            },
        );
        ui.label("Line");
        ui.vertical(|ui| {
            if line_extras {
                ui.checkbox(&mut cfg.swap_rx_tx, "Swap RX/TX pads")
                    .on_hover_text(
                        "The peripheral crosses the two itself — for a cable or a board that is wired the other way round, with no rework.",
                    );
                ui.checkbox(&mut cfg.invert_tx, "Invert TX")
                    .on_hover_text("Idle low instead of idle high, for an inverting transceiver.");
                ui.checkbox(&mut cfg.invert_rx, "Invert RX");
            } else {
                ui.label(
                    egui::RichText::new("this USART has no swap / invert bits")
                        .size(10.5)
                        .italics()
                        .color(egui::Color32::from_gray(140)),
                );
            }
        });
        ui.end_row();
        // Readback is a half-duplex-only argument, so it appears only there.
        if cfg.direction.is_half_duplex() {
            out.field("Read back own TX", docs::USART_HALF_DUPLEX_READBACK);
            ui.label("Read back own TX");
            ui.checkbox(&mut cfg.half_duplex_readback, "")
                .on_hover_text(
                    "One wire carries both directions, so everything this node sends is also on its receiver. OFF (the default) disables the receiver while transmitting, which is what a bus with other talkers wants; ON keeps the echo, which is how you verify a driver that can be shouted down.",
                );
            ui.end_row();
        }
    };

    let flow_row = |ui: &mut egui::Ui,
                    out: &mut ConfigOut,
                    cfg: &mut UsartModuleConfig,
                    wired: (bool, bool)| {
        out.field("Hardware flow control", docs::USART_FLOW);
        ui.label("Hardware flow control");
        ui.vertical(|ui| {
            egui::ComboBox::from_id_salt("usart_flow")
                .selected_text(cfg.flow.label())
                .show_ui(ui, |ui| {
                    for f in UsartFlow::options_for(cfg.mode, cfg.direction, family) {
                        ui.selectable_value(&mut cfg.flow, *f, f.label());
                    }
                });
            let (has_cts, has_rts) = wired;
            let missing = match (
                cfg.flow.needs_cts() && !has_cts,
                cfg.flow.needs_rts() && !has_rts,
            ) {
                (true, true) => "CTS and RTS pins",
                (true, false) => "a CTS pin",
                (false, true) => "an RTS pin",
                (false, false) => "",
            };
            if !missing.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{} needs {missing} — assign it on the canvas",
                        cfg.flow.label()
                    ))
                    .size(10.5)
                    .color(egui::Color32::from_rgb(220, 160, 70)),
                );
            }
        });
        ui.end_row();
    };

    // Which DMA channels this peripheral gets. Automatic is right almost
    // always — the row exists for the board that needs a SPECIFIC channel (to
    // leave a high-priority one free, to match an existing driver, to dodge an
    // erratum), which is not something the IDE can infer.
    // ONE direction's picker. Split out of `dma_row` because a peripheral can
    // legitimately use a single channel — an I2S transmits or receives, never
    // both — and offering the other one would invite a choice that moves no
    // bytes.
    let dma_one = |ui: &mut egui::Ui,
                   bus: dma_map::Bus,
                   inst: u8,
                   dir: dma_map::Dir,
                   label: &str,
                   chosen: &mut String| {
        {
            let options = dma_map::channels_for(dma, bus, inst, dir);
            ui.label(label);
            if options.is_empty() {
                // No vendor data for this chip: the IDE cannot say which
                // channels are valid, so it offers none rather than a free-text
                // box that invites a name which moves the wrong bytes.
                ui.add_enabled_ui(false, |ui| {
                    egui::ComboBox::from_id_salt(format!("dma_{label}_none"))
                        .selected_text("Automatic")
                        .show_ui(ui, |_ui| {});
                })
                .response
                .on_hover_text(
                    "This chip carries no DMA channel data - re-import it from the STM32Cube database to choose a channel by hand.",
                );
                ui.end_row();
                return;
            }
            egui::ComboBox::from_id_salt(format!("dma_{label}"))
                .selected_text(if chosen.is_empty() {
                    "Automatic".to_owned()
                } else {
                    chosen.clone()
                })
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(chosen.is_empty(), "Automatic")
                        .on_hover_text("The IDE takes the first channel this peripheral can use and nothing else has taken.")
                        .clicked()
                    {
                        chosen.clear();
                    }
                    for c in &options {
                        if ui.selectable_label(chosen == c, c).clicked() {
                            *chosen = c.clone();
                        }
                    }
                });
            ui.end_row();
        }
    };
    let dma_row = |ui: &mut egui::Ui,
                   out: &mut ConfigOut,
                   bus: dma_map::Bus,
                   inst: u8,
                   tx: &mut String,
                   rx: &mut String| {
        out.field("DMA TX", docs::DMA_CHANNEL);
        out.field("DMA RX", docs::DMA_CHANNEL);
        dma_one(ui, bus, inst, dma_map::Dir::Tx, "DMA TX", tx);
        dma_one(ui, bus, inst, dma_map::Dir::Rx, "DMA RX", rx);
    };

    // ── Settings this module HAS that the grid below does not draw ──
    //
    // Four fields on every config, none of them gated by anything: the instance,
    // the name, and the two data models. They are pushed once here rather than
    // per arm because there is no arm that could get them wrong — and because
    // the family, which decides what an INSTANCE even is, is known here and
    // must not be re-derived in the pane.
    out.elsewhere("Instance", shared_instance_doc(m_kind, family));
    out.elsewhere(
        "Name",
        if crate::panels::mcu_module::codegen::rp::is_rp(family) {
            docs::SHARED_NAME_RP
        } else {
            docs::SHARED_NAME
        },
    );
    out.elsewhere("Data models", docs::SHARED_DATA_MODELS);

    // A picture of what this peripheral puts on the wire, top right - see
    // `signal_legend`. Outside the grid on purpose: inside it the drawing would
    // be a cell, and a cell is either the label column or the control column.
    signal_legend(ui, m_kind);

    egui::Grid::new("module_cfg")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            // A LABELLED block, because three arms below finish early. A plain
            // `return` there left the whole closure, taking the pin rows with it -
            // an SPI slave and every ESP USB role lost their pad list, and with it
            // the only place a signal can be moved to another pad.
            'cfg: {
            match &mut m.config {
                ModuleConfig::Touch(cfg) => {
                    out.field("Scan", docs::TOUCH_SCAN);
                    ui.label("Scan");
                    egui::ComboBox::from_id_salt("touchscan")
                        .selected_text(cfg.scan.label())
                        .show_ui(ui, |ui| {
                            for v in TouchScan::ALL {
                                ui.selectable_value(&mut cfg.scan, v, v.label())
                                    .on_hover_text(v.hint());
                            }
                        })
                        .response
                        .on_hover_text(
                            "The two are different TYPES in esp-hal, with different \
                             methods - not a speed setting.",
                        );
                    ui.end_row();

                    out.field("Touched when", docs::TOUCH_THRESHOLD_MODE);
                    ui.label("Touched when");
                    egui::ComboBox::from_id_salt("touchthr")
                        .selected_text(cfg.threshold_mode.label())
                        .show_ui(ui, |ui| {
                            for v in TouchThreshold::ALL {
                                ui.selectable_value(&mut cfg.threshold_mode, v, v.label());
                            }
                        })
                        .response
                        .on_hover_text(
                            "A finger adds capacitance, so the pad charges slower and the \
                             count usually FALLS. Below-threshold is the normal choice.",
                        );
                    ui.end_row();

                    out.field("Threshold", docs::TOUCH_THRESHOLD);
                    ui.label("Threshold");
                    ui.add(crate::panels::drag_value(ui, &mut cfg.threshold).range(1..=65535))
                        .on_hover_text(
                            "There is no right number here: read your own pad untouched, \
                             then take a margin off it. One value for every pad - the \
                             generated file takes it per pad if you need to differ.",
                        );
                    ui.end_row();

                    out.field("Measurement", docs::TOUCH_MEASUREMENT);
                    ui.label("Measurement")
                        .on_hover_text("Cycles of the 8 MHz touch clock, per measurement.");
                    ui.add(
                        crate::panels::drag_value(ui, &mut cfg.measurement_duration).range(1..=0x7fff),
                    );
                    ui.end_row();

                    // The sleep timer only exists in continuous mode; showing it
                    // in one-shot would be a control that changes nothing.
                    if !cfg.scan.is_continuous() {
                        out.skip("Sleep cycles", docs::SKIP_TOUCH_SLEEP);
                    }
                    if cfg.scan.is_continuous() {
                        out.field("Sleep cycles", docs::TOUCH_SLEEP_CYCLES);
                        ui.label("Sleep cycles")
                            .on_hover_text("Idle time between background measurements.");
                        ui.add(crate::panels::drag_value(ui, &mut cfg.sleep_cycles).range(1..=0xffff));
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                ModuleConfig::LcdCam(cfg) => {
                    // The camera is the OTHER half and its own module, so this
                    // one chooses only between the two display shapes.
                    let camera = cfg.mode.is_camera();
                    if !camera {
                        out.field("Mode", docs::LCDCAM_MODE);
                        ui.label("Mode");
                        egui::ComboBox::from_id_salt("lcd_mode")
                            .selected_text(cfg.mode.label())
                            .show_ui(ui, |ui| {
                                for v in [LcdCamMode::I8080, LcdCamMode::Dpi] {
                                    ui.selectable_value(&mut cfg.mode, v, v.label())
                                        .on_hover_text(v.hint());
                                }
                            })
                            .response
                            .on_hover_text(
                                "The display half is one or the other, never both - they \
                                 are two drivers over the same pads. The camera half runs \
                                 alongside either, as its own module.",
                            );
                        ui.end_row();
                    }

                    out.field(
                        "Bus width",
                        if camera {
                            docs::LCDCAM_WIDTH_CAM
                        } else {
                            docs::LCDCAM_WIDTH
                        },
                    );
                    ui.label("Bus width");
                    egui::ComboBox::from_id_salt("lcd_width")
                        .selected_text(format!("{}-bit", cfg.width))
                        .show_ui(ui, |ui| {
                            for w in [8u8, 16] {
                                ui.selectable_value(&mut cfg.width, w, format!("{w}-bit"));
                            }
                        })
                        .response
                        .on_hover_text(
                            "How many data pads the driver binds. Assign D0..D15 on the \
                             canvas to match - only D0 and D1 are wired for you.",
                        );
                    ui.end_row();

                    // A camera in SLAVE mode is clocked by the sensor, so the
                    // number below changes nothing. Said, rather than hidden.
                    let slave_cam = cfg.mode.is_camera() && !cfg.master_clock;
                    out.field(
                        "Pixel clock",
                        if slave_cam {
                            docs::LCDCAM_PIXEL_CLOCK_SLAVE
                        } else if camera {
                            docs::LCDCAM_PIXEL_CLOCK_CAM
                        } else if cfg.mode == LcdCamMode::Dpi {
                            docs::LCDCAM_PIXEL_CLOCK_DPI
                        } else {
                            docs::LCDCAM_PIXEL_CLOCK_I8080
                        },
                    );
                    ui.label("Pixel clock");
                    ui.horizontal(|ui| {
                        ui.add_enabled(
                            !slave_cam,
                            crate::panels::drag_value(ui, &mut cfg.clock_hz)
                                .range(100_000..=80_000_000)
                                .speed(100_000.0)
                                .custom_formatter(|v, _| hz_label(v as u32))
                                .suffix(""),
                        );
                        if slave_cam {
                            ui.label(
                                egui::RichText::new("the sensor supplies it")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    });
                    ui.end_row();

                    if cfg.mode.is_camera() {
                        out.field("Master clock", docs::LCDCAM_MASTER_CLOCK);
                        ui.label("Master clock");
                        ui.checkbox(&mut cfg.master_clock, "this chip clocks the sensor")
                            .on_hover_text(
                                "On, the MCLK pad drives the camera and the frequency above \
                                 is what it gets. Off is slave mode: the sensor is clocked \
                                 from elsewhere and this chip only reads.",
                            );
                        ui.end_row();
                    }

                    if cfg.mode == LcdCamMode::Dpi {
                        // An RGB panel has no controller: every one of these
                        // numbers comes off its datasheet, and a wrong one is a
                        // rolling picture rather than a build error.
                        out.field("Active area", docs::LCDCAM_ACTIVE_AREA);
                        ui.label("Active area");
                        ui.horizontal(|ui| {
                            ui.add(crate::panels::drag_value(ui, &mut cfg.h_active).range(1..=4095));
                            ui.label("x");
                            ui.add(crate::panels::drag_value(ui, &mut cfg.v_active).range(1..=4095));
                            ui.label(
                                egui::RichText::new("px").size(11.0).color(egui::Color32::GRAY),
                            );
                        });
                        ui.end_row();

                        out.field("Total", docs::LCDCAM_TOTAL);
                        ui.label("Total")
                            .on_hover_text("Active area plus blanking - from the panel datasheet.");
                        ui.horizontal(|ui| {
                            ui.add(crate::panels::drag_value(ui, &mut cfg.h_total).range(1..=4095));
                            ui.label("x");
                            ui.add(crate::panels::drag_value(ui, &mut cfg.v_total).range(1..=4095));
                        });
                        ui.end_row();

                        out.field("Front porch", docs::LCDCAM_FRONT_PORCH);
                        ui.label("Front porch");
                        ui.horizontal(|ui| {
                            ui.add(crate::panels::drag_value(ui, &mut cfg.h_front_porch).range(0..=1023));
                            ui.label("x");
                            ui.add(crate::panels::drag_value(ui, &mut cfg.v_front_porch).range(0..=1023));
                        });
                        ui.end_row();

                        out.field("Sync width", docs::LCDCAM_SYNC_WIDTH);
                        ui.label("Sync width");
                        ui.horizontal(|ui| {
                            ui.add(crate::panels::drag_value(ui, &mut cfg.hsync_width).range(1..=1023));
                            ui.label("x");
                            ui.add(crate::panels::drag_value(ui, &mut cfg.vsync_width).range(1..=1023));
                        });
                        ui.end_row();
                    }

                    out.field("Transfers", docs::LCDCAM_TRANSFERS);
                    ui.label("Transfers");
                    ui.label(
                        egui::RichText::new("DMA, always")
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                    )
                    .on_hover_text(
                        "Every LCD_CAM driver takes a channel in its only constructor: \
                         there is no CPU path. The Configuration tab shows which channel \
                         this one got.",
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                ModuleConfig::ParlIo(cfg) => {
                    out.field("Direction", docs::PARLIO_DIRECTION);
                    ui.label("Direction");
                    egui::ComboBox::from_id_salt("parl_dir")
                        .selected_text(cfg.direction.label())
                        .show_ui(ui, |ui| {
                            for v in ParlIoDirection::ALL {
                                ui.selectable_value(&mut cfg.direction, v, v.label());
                            }
                        })
                        .response
                        .on_hover_text(
                            "The port moves data one way at a time. This also decides whether \
                             the data pads and the clock are outputs or inputs.",
                        );
                    ui.end_row();

                    ui.label("Bus width");
                    let widths = ParlIoWidth::options(family);
                    // After `widths`, which is what decides the form: only the
                    // chips with the pads for it offer 16-bit.
                    out.field(
                        "Bus width",
                        if widths.len() == 5 {
                            docs::PARLIO_WIDTH
                        } else {
                            docs::PARLIO_WIDTH_NO_16
                        },
                    );
                    egui::ComboBox::from_id_salt("parl_width")
                        .selected_text(cfg.width.label())
                        .show_ui(ui, |ui| {
                            for v in widths.iter().copied() {
                                ui.selectable_value(&mut cfg.width, v, v.label());
                            }
                        })
                        .response
                        .on_hover_text(if widths.len() == 5 {
                            "How many data lines move together. Assign D0..D15 on the canvas \
                             to match - only D0 and the clock are wired for you."
                        } else {
                            "How many data lines move together. Sixteen is missing because \
                             esp-hal builds it only for the first generation of this \
                             peripheral, which this chip does not have."
                        });
                    ui.end_row();

                    out.field("Clock", docs::PARLIO_CLOCK);
                    ui.label("Clock");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.freq_hz)
                                .range(1_000..=40_000_000)
                                .suffix(" Hz")
                                .speed(1000.0),
                        );
                        for (lbl, hz) in [("1M", 1_000_000u32), ("10M", 10_000_000)] {
                            if ui.small_button(lbl).clicked() {
                                cfg.freq_hz = hz;
                            }
                        }
                        ui.label(
                            egui::RichText::new(format!(
                                "{} MB/s",
                                (u64::from(cfg.freq_hz) * u64::from(cfg.width.lanes()) / 8)
                                    as f64
                                    / 1_000_000.0
                            ))
                            .size(11.0)
                            .color(egui::Color32::GRAY),
                        );
                    })
                    .response
                    .on_hover_text(
                        "One transfer of the whole bus per clock, so the throughput is the \
                         clock times the width.",
                    );
                    ui.end_row();

                    out.field("Bit order", docs::PARLIO_BIT_ORDER);
                    ui.label("Bit order");
                    egui::ComboBox::from_id_salt("parl_order")
                        .selected_text(cfg.bit_order.label())
                        .show_ui(ui, |ui| {
                            for v in ParlIoBitOrder::ALL {
                                ui.selectable_value(&mut cfg.bit_order, v, v.label());
                            }
                        });
                    ui.end_row();

                    out.field("DMA buffer", docs::PARLIO_DMA_BUFFER);
                    ui.label("DMA buffer");
                    ui.add(
                        crate::panels::drag_value(ui, &mut cfg.buffer_bytes)
                            .range(64..=32_768)
                            .suffix(" bytes"),
                    )
                    .on_hover_text(
                        "The block the DMA moves in one go. A parallel port has no non-DMA \
                         form - its constructor takes a channel.",
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                ModuleConfig::Mcpwm(cfg) => {
                    // One duty row per WIRED output: the six are not all in use,
                    // and a row for a pad nobody assigned sets nothing.
                    let mut wired: Vec<(u8, bool, &'static str)> = m
                        .connections
                        .iter()
                        .filter_map(|c| match c.signal {
                            ModuleSignal::McpwmA0 => Some((0u8, false, "OP0 A")),
                            ModuleSignal::McpwmB0 => Some((0, true, "OP0 B")),
                            ModuleSignal::McpwmA1 => Some((1, false, "OP1 A")),
                            ModuleSignal::McpwmB1 => Some((1, true, "OP1 B")),
                            ModuleSignal::McpwmA2 => Some((2, false, "OP2 A")),
                            ModuleSignal::McpwmB2 => Some((2, true, "OP2 B")),
                            _ => None,
                        })
                        .collect();
                    wired.sort();
                    let mut ops: Vec<u8> = wired.iter().map(|(op, _, _)| *op).collect();
                    ops.dedup();

                    // The unit has three timers and any operator can be on any
                    // of them, so an operator with a pad wired picks its own.
                    // With one operator there is nothing to choose, and the row
                    // would be a control that changes nothing.
                    if ops.len() > 1 {
                        for op in &ops {
                            out.field("Operator timer", docs::MCPWM_OP_TIMER);
                        ui.label(format!("OP{op} timer"));
                            let mut t = cfg.timer_of(*op);
                            egui::ComboBox::from_id_salt(format!("mcpwmtim{op}"))
                                .selected_text(format!("Timer {t}"))
                                .show_ui(ui, |ui| {
                                    for v in 0u8..3 {
                                        ui.selectable_value(&mut t, v, format!("Timer {v}"));
                                    }
                                })
                                .response
                                .on_hover_text(
                                    "Operators on the SAME timer share its frequency and \
                                     period; on different timers they are independent. Two \
                                     motors at two speeds is what the three are for.",
                                );
                            cfg.op_timer[usize::from(*op).min(2)] = t;
                            ui.end_row();
                        }
                    }

                    // A frequency and a resolution per timer IN USE. An unused
                    // timer is never started, so showing its settings would be
                    // two more controls that change nothing.
                    let used = cfg.timers_used(&ops);
                    let one = used.len() == 1;
                    for t in &used {
                        let tag = if one {
                            String::new()
                        } else {
                            format!(" T{t}")
                        };
                        let (freq, period) = cfg.timer_mut(*t);
                        out.field(
                            "Frequency",
                            if one {
                                docs::MCPWM_FREQUENCY
                            } else {
                                docs::MCPWM_FREQUENCY_PER_TIMER
                            },
                        );
                        ui.label(format!("Frequency{tag}"));
                        let mut set: Option<u32> = None;
                        ui.horizontal(|ui| {
                            ui.add(
                                crate::panels::drag_value(ui, freq)
                                    .range(1..=1_000_000)
                                    .suffix(" Hz")
                                    .speed(100.0),
                            );
                            for (lbl, hz) in [("1k", 1_000u32), ("20k", 20_000), ("50k", 50_000)] {
                                if ui.small_button(lbl).clicked() {
                                    set = Some(hz);
                                }
                            }
                        })
                        .response
                        .on_hover_text(
                            "Shared by every operator on THIS timer. 20 kHz and up is above \
                             hearing, which is where a motor drive wants to be.",
                        );
                        if let Some(hz) = set {
                            *freq = hz;
                        }
                        ui.end_row();

                        out.field(
                            "Resolution",
                            if one {
                                docs::MCPWM_RESOLUTION
                            } else {
                                docs::MCPWM_RESOLUTION_PER_TIMER
                            },
                        );
                        ui.label(format!("Resolution{tag}"));
                        let steps = u32::from(*period) + 1;
                        ui.horizontal(|ui| {
                            ui.add(
                                crate::panels::drag_value(ui, period)
                                    .range(1..=65_534)
                                    .prefix("period "),
                            );
                            ui.label(
                                egui::RichText::new(format!("{steps} duty steps"))
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                        })
                        .response
                        .on_hover_text(
                            "The timer's counter top. Duty can only land on one of these \
                             steps, and period x frequency is bounded by the peripheral \
                             clock - so a finer resolution costs frequency.",
                        );
                        ui.end_row();
                    }

                    for (op, is_b, lbl) in wired {
                        out.field("Duty", docs::MCPWM_DUTY);
                        ui.label(format!("Duty {lbl}"));
                        let mut pct = f32::from(cfg.duty_x100_of(op, is_b)) / 100.0;
                        let steps = u32::from(cfg.timer_period(cfg.timer_of(op))) + 1;
                        if ui
                            .add(
                                crate::panels::drag_value(ui, &mut pct)
                                    .range(0.0..=100.0)
                                    .suffix(" %")
                                    .speed(0.5),
                            )
                            .on_hover_text(format!(
                                "Timestamp {} of {steps}, on timer {}",
                                cfg.timestamp_of(op, is_b),
                                cfg.timer_of(op),
                            ))
                            .changed()
                        {
                            cfg.duty_x100
                                .insert((op, is_b), (pct * 100.0).round() as u16);
                        }
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                ModuleConfig::Pcnt(cfg) => {
                    // A unit has two channels and each is wired on its own, so
                    // the rules are shown per WIRED channel. Channel 1 with no
                    // edge pad is not a channel — it gets no rows at all.
                    let wired: Vec<(u8, bool)> = [
                        (0u8, ModuleSignal::PcntEdgeSig, ModuleSignal::PcntCtrlSig),
                        (1, ModuleSignal::PcntEdgeSig1, ModuleSignal::PcntCtrlSig1),
                    ]
                    .into_iter()
                    .filter(|(_, edge, _)| m.connections.iter().any(|c| c.signal == *edge))
                    .map(|(ch, _, ctrl)| {
                        (ch, m.connections.iter().any(|c| c.signal == ctrl))
                    })
                    .collect();
                    // With one channel there is nothing to disambiguate, so the
                    // rows keep the names they always had.
                    let one = wired.len() <= 1;

                    for (ch, has_ctrl) in &wired {
                        let tag = if one {
                            String::new()
                        } else {
                            format!(" ch{ch}")
                        };
                        let mut k = cfg.channel(*ch);

                        out.field("Counts", docs::PCNT_COUNTS);
                        ui.label(format!("Counts{tag}"));
                        ui.horizontal(|ui| {
                            for (lbl, mode, id) in [
                                ("Rising", &mut k.pos_edge, format!("pcnt_pos{ch}")),
                                ("Falling", &mut k.neg_edge, format!("pcnt_neg{ch}")),
                            ] {
                                ui.label(egui::RichText::new(lbl).size(11.0));
                                egui::ComboBox::from_id_salt(id)
                                    .width(96.0)
                                    .selected_text(mode.label())
                                    .show_ui(ui, |ui| {
                                        for v in PcntEdgeMode::ALL {
                                            ui.selectable_value(mode, v, v.label());
                                        }
                                    });
                            }
                        })
                        .response
                        .on_hover_text(
                            "What each edge does to the counter. Counting BOTH edges \
                             doubles the resolution and halves the range. Both channels \
                             add into the SAME counter.",
                        );
                        ui.end_row();

                        out.field(
                            "Control input",
                            if *has_ctrl {
                                docs::PCNT_CTRL
                            } else {
                                docs::PCNT_CTRL_ABSENT
                            },
                        );
                        ui.label(format!("Control input{tag}"));
                        ui.add_enabled_ui(*has_ctrl, |ui| {
                            ui.horizontal(|ui| {
                                for (lbl, mode, id) in [
                                    ("Low", &mut k.ctrl_low, format!("pcnt_cl{ch}")),
                                    ("High", &mut k.ctrl_high, format!("pcnt_ch{ch}")),
                                ] {
                                    ui.label(egui::RichText::new(lbl).size(11.0));
                                    egui::ComboBox::from_id_salt(id)
                                        .width(104.0)
                                        .selected_text(mode.label())
                                        .show_ui(ui, |ui| {
                                            for v in PcntCtrlMode::ALL {
                                                ui.selectable_value(mode, v, v.label());
                                            }
                                        });
                                }
                            });
                        })
                        .response
                        .on_hover_text(if *has_ctrl {
                            "What the control input's level does. Set one side to Reverse \
                             and the unit follows an encoder's direction on its own."
                        } else {
                            "No control pad on this channel. Assign a PCNT CTRL pin on the \
                             canvas - it is what turns the counter into an encoder."
                        });
                        ui.end_row();

                        cfg.set_channel(*ch, k);
                    }

                    // The second channel is the other half of a quadrature
                    // encoder, and nothing else says so.
                    if one {
                        out.field("Second channel", docs::PCNT_SECOND_CHANNEL);
                        ui.label("Second channel");
                        ui.label(
                            egui::RichText::new("assign PCNT EDGE1 on the canvas")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "The unit's other channel, with its own edge and control pads \
                             and its own rules, counting into the same counter. Two \
                             channels with opposite rules is a quadrature encoder.",
                        );
                        ui.end_row();
                    }

                    out.field("Limits", docs::PCNT_LIMITS);
                    ui.label("Limits");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.low_limit)
                                .range(-32_767..=0)
                                .prefix("low "),
                        );
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.high_limit)
                                .range(0..=32_767)
                                .prefix("high "),
                        );
                    })
                    .response
                    .on_hover_text(
                        "The counter is signed 16-bit. Reaching a limit CLEARS it and \
                         raises an event - which is how a count wider than 16 bits is \
                         accumulated.",
                    );
                    ui.end_row();

                    out.field("Glitch filter", docs::PCNT_FILTER);
                    ui.label("Glitch filter");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.filter)
                                .range(0..=1023)
                                .suffix(" APB clocks"),
                        );
                        if cfg.filter == 0 {
                            ui.label(
                                egui::RichText::new("off")
                                    .size(11.0)
                                    .color(egui::Color32::GRAY),
                            );
                        }
                    })
                    .response
                    .on_hover_text(
                        "Pulses shorter than this are ignored - the difference between \
                         counting a contact bounce once and counting it eight times. The \
                         hardware caps it at 1023.",
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                ModuleConfig::Rmt(cfg) => {
                    ui.label("Direction");
                    let dirs = RmtDirection::options(family, cfg.instance);
                    let locked = dirs.len() == 1;
                    out.field(
                        "Direction",
                        if locked {
                            docs::RMT_DIRECTION_LOCKED
                        } else {
                            docs::RMT_DIRECTION
                        },
                    );
                    ui.add_enabled_ui(!locked, |ui| {
                        egui::ComboBox::from_id_salt("rmt_dir")
                            .selected_text(cfg.direction.label())
                            .show_ui(ui, |ui| {
                                for v in dirs.iter().copied() {
                                    ui.selectable_value(&mut cfg.direction, v, v.label());
                                }
                            });
                    })
                    .response
                    .on_hover_text(if locked {
                        "Fixed in silicon on this chip: the low RMT channels transmit and the \
                         high ones receive. Pick a different channel to change direction."
                    } else {
                        "This chip's RMT channels can each do either."
                    });
                    ui.end_row();

                    out.field("Clock divider", docs::RMT_CLK_DIVIDER);
                    ui.label("Clock divider");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.clk_divider)
                                .range(1..=255)
                                .prefix("/"),
                        );
                        // The tick is what every duration in a pulse train is
                        // counted in, so showing it is more useful than the
                        // divider that produced it.
                        let src = rmt_source_hz(family);
                        let tick_ns = 1_000_000_000f64 / (src as f64 / cfg.clk_divider as f64);
                        ui.label(
                            egui::RichText::new(format!("1 tick = {tick_ns:.0} ns"))
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                    })
                    .response
                    .on_hover_text(
                        "Divides the RMT source clock. A smaller divider gives finer \
                         resolution and a shorter longest pulse; the two trade off.",
                    );
                    ui.end_row();

                    if cfg.direction.is_tx() {
                        out.field("Idle level", docs::RMT_IDLE_LEVEL);
                        ui.label("Idle level");
                        egui::ComboBox::from_id_salt("rmt_idle")
                            .selected_text(if cfg.idle_high { "High" } else { "Low" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut cfg.idle_high, false, "Low");
                                ui.selectable_value(&mut cfg.idle_high, true, "High");
                            })
                            .response
                            .on_hover_text("Where the pad rests between trains.");
                        ui.end_row();
                    } else {
                        out.field("Idle threshold", docs::RMT_IDLE_THRESHOLD);
                        ui.label("Idle threshold");
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.idle_threshold)
                                .range(1..=65535)
                                .suffix(" ticks"),
                        )
                        .on_hover_text(
                            "How long the line must rest before the frame counts as over. \
                             Too short and one train is read as several.",
                        );
                        ui.end_row();
                    }

                    out.field("Carrier", docs::RMT_CARRIER);
                    ui.label("Carrier");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut cfg.carrier, "");
                        ui.add_enabled_ui(cfg.carrier, |ui| {
                            ui.add(
                                crate::panels::drag_value(ui, &mut cfg.carrier_hz)
                                    .range(1_000..=1_000_000)
                                    .suffix(" Hz")
                                    .speed(100.0),
                            );
                            if ui.small_button("38k").clicked() {
                                cfg.carrier_hz = 38_000;
                            }
                        });
                    })
                    .response
                    .on_hover_text(
                        "Modulate the pulses onto a carrier — 38 kHz is what an IR receiver \
                         demodulates. Off for WS2812 and 1-Wire, which want the edges raw.",
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                // LPUART reuses the USART settings struct, so it reuses this
                // whole arm — only the DMA request table differs (`uart_bus`).
                ModuleConfig::Usart(cfg) | ModuleConfig::Lpuart(cfg) => {
                    out.field("Baud rate", docs::USART_BAUD);
                    ui.label("Baud rate");
                    baud_row(ui, &mut cfg.baud_rate, &baud_chip.plan(cfg.instance));
                    ui.end_row();
                    // Right after baud rate, because the two are read together:
                    // the buffer is only meaningful as "how many bytes at this
                    // speed". Async only - the blocking paths have no buffer -
                    // and hidden on a TX-only DMA link, which has none either.
                    let tx_only_dma =
                        cfg.mode == UsartMode::Dma && cfg.direction == UsartDirection::TxOnly;
                    if tx_only_dma {
                        out.skip("RX/TX buffer", docs::SKIP_USART_BUF_TX_ONLY);
                    }
                    // `buf_len` reaches the RP backend in exactly one place —
                    // the BUFFERED branch, which allocates the two `StaticCell`
                    // rings. Its DMA `Uart` reads into the caller's slice and
                    // has no ring at all, so the row sized a buffer that was
                    // never allocated while its own hover promised a circular
                    // one that stops bytes being lost.
                    let rp = crate::panels::mcu_module::codegen::rp::is_rp(family);
                    let rp_dma = rp && cfg.mode == UsartMode::Dma;
                    if rp_dma {
                        out.skip("RX DMA buffer", docs::SKIP_USART_BUF_RP_DMA);
                    }
                    // embassy-nrf's `Uarte` has no ring on either transport,
                    // and the nRF backend reads neither `mode` nor `buf_len`.
                    let nrf = crate::panels::mcu_module::codegen::nrf::is_nrf(family);
                    if is_async && nrf && !tx_only_dma {
                        out.skip(
                            if cfg.mode == UsartMode::Dma {
                                "RX DMA buffer"
                            } else {
                                "RX/TX buffer"
                            },
                            docs::SKIP_USART_BUF_NRF,
                        );
                    }
                    if is_async && !tx_only_dma && !rp_dma && !nrf {
                        let dma = cfg.mode == UsartMode::Dma;
                        // The label AND the sentence both turn on the transport
                        // — the field means two different things, and the pane
                        // has to explain the one the reader is looking at.
                        out.field(
                            if dma { "RX DMA buffer" } else { "RX/TX buffer" },
                            if dma {
                                docs::USART_BUF_DMA
                            } else {
                                docs::USART_BUF_BUFFERED
                            },
                        );
                        ui.label(if dma { "RX DMA buffer" } else { "RX/TX buffer" });
                        ui.horizontal(|ui| {
                            // A drag value, not a combo: this is a number the
                            // user sizes against their own read cadence, and a
                            // list of powers of two would only pretend to know
                            // it. Clamped to what the generated code can hold.
                            ui.add(
                                crate::panels::drag_value(ui, &mut cfg.buf_len)
                                    .speed(16.0)
                                    .range(16..=65_536)
                                    .suffix(" B"),
                            );
                            let ms = cfg.buf_len as f32 * 10.0 / cfg.baud_rate.max(1) as f32
                                * 1000.0;
                            ui.label(
                                egui::RichText::new(format!("~{ms:.0} ms at {} baud", cfg.baud_rate))
                                    .size(10.5)
                                    .color(egui::Color32::from_gray(130)),
                            );
                        })
                        .response
                        .on_hover_text(if dma {
                            "The circular buffer the DMA controller fills on its own. Reception never stops, so this only has to cover the longest GAP between your reads - overrun it and the OLDEST bytes are dropped, silently."
                        } else {
                            "Size of both software ring buffers, TX and RX. The CPU copies byte by byte on each interrupt, so this has to cover what arrives between your reads."
                        });
                        ui.end_row();
                    }
                    out.field("Data bits", docs::USART_DATA_BITS);
                    ui.label("Data bits");
                    // Nine bits is an STM32 word length. The RP's PL011 and the
                    // ESP's UART both stop at eight, and the ESP generator was
                    // rounding a 9 down to `_8` in silence - the panel said nine
                    // and the wire carried eight.
                    let widths = crate::panels::mcu_module::modules::usart_data_bits(family);
                    if !widths.contains(&cfg.data_bits) {
                        cfg.data_bits = widths[0];
                    }
                    egui::ComboBox::from_id_salt("databits")
                        .selected_text(cfg.data_bits.to_string())
                        .show_ui(ui, |ui| {
                            for d in widths {
                                ui.selectable_value(&mut cfg.data_bits, *d, d.to_string());
                            }
                        });
                    ui.end_row();
                    out.field("Parity", docs::USART_PARITY);
                    ui.label("Parity");
                    egui::ComboBox::from_id_salt("parity")
                        .selected_text(parity_label(cfg.parity))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cfg.parity, Parity::None, "None");
                            ui.selectable_value(&mut cfg.parity, Parity::Even, "Even");
                            ui.selectable_value(&mut cfg.parity, Parity::Odd, "Odd");
                        });
                    ui.end_row();
                    out.field("Stop bits", docs::USART_STOP_BITS);
                    ui.label("Stop bits");
                    egui::ComboBox::from_id_salt("stop")
                        .selected_text(stop_label(cfg.stop_bits))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cfg.stop_bits, StopBits::One, "1");
                            ui.selectable_value(&mut cfg.stop_bits, StopBits::Two, "2");
                        });
                    ui.end_row();
                    // Blocking → editable Portable|Native; Native runtime → shown
                    // locked on Native; async USART → hidden (always the
                    // embedded-io-async BufferedUart bridge, no choice).
                    if esp {
                        api_row(ui, out, &mut pending.0);
                        // Direction and flow control are NOT async concepts on
                        // an ESP — `UartTx::new` and `.with_cts()` are there on
                        // either runtime — so they show whatever the runtime is.
                        // They were unreachable while the ESP shared embassy's
                        // async gate.
                        direction_row(ui, out, cfg);
                        flow_row(ui, out, cfg, wired_flow);
                        if !UsartDirection::options_for(cfg.mode, family)
                            .contains(&cfg.direction)
                        {
                            cfg.direction = UsartDirection::TxRx;
                        }
                        if !UsartFlow::options_for(cfg.mode, cfg.direction, family)
                            .contains(&cfg.flow)
                        {
                            cfg.flow = UsartFlow::None;
                        }
                        // No transport row: esp-hal's UART DMA is UHCI, a
                        // different driver this generator does not write yet.
                        // Said rather than left blank — the SPI beside it has
                        // the row, and the difference is not obvious.
                        out.field("Transfers", docs::USART_TRANSFERS_ESP);
                        ui.label("Transfers");
                        ui.label(
                            egui::RichText::new("CPU (blocking)")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "esp-hal moves UART bytes over DMA through UHCI, a driver of \
                             its own that this generator does not write yet. The SPI \
                             module's DMA is unrelated and does work.",
                        );
                        ui.end_row();
                    } else if is_native {
                        api_row_locked(ui, out);
                    } else if is_async {
                        // The API style is fixed on async (embedded-io-async
                        // either way); what IS a choice is the transport -
                        // except on an nRF, whose UARTE has EasyDMA built in.
                        if nrf {
                            out.skip("Async transport", docs::SKIP_USART_TRANSPORT_NRF);
                        } else {
                            usart_mode_row(ui, out, &mut cfg.mode, family);
                        }
                        // The transport decides which directions exist, and the
                        // pair decides which flow options do — so this order is
                        // load-bearing, not cosmetic.
                        direction_row(ui, out, cfg);
                        flow_row(ui, out, cfg, wired_flow);
                        // A direction the new transport cannot build, or a flow
                        // option the new pair cannot, would otherwise sit there
                        // as a stale choice that generates nothing.
                        if !UsartDirection::options_for(cfg.mode, family)
                            .contains(&cfg.direction)
                        {
                            cfg.direction = UsartDirection::TxRx;
                        }
                        if !UsartFlow::options_for(cfg.mode, cfg.direction, family)
                            .contains(&cfg.flow)
                        {
                            cfg.flow = UsartFlow::None;
                        }
                        if crate::panels::mcu_module::codegen::rp::is_rp(family) {
                            // Under Buffered no channel is taken at all, so the
                            // note would describe something that is not
                            // happening - and the DMA card agrees, because
                            // `async_bus_lines` reserves nothing for that bus.
                            if cfg.mode == UsartMode::Dma {
                                out.note(RP_DMA_NOTE);
                            }
                        } else if cfg.mode == UsartMode::Dma && !nrf {
                            // Never on an nRF: there are no channels to pick.
                            let inst = cfg.instance;
                            dma_row(ui, out, uart_bus, inst, &mut cfg.dma_tx, &mut cfg.dma_rx);
                        }
                    } else if let Some(chans) =
                        codegen::stm32::blocking_dma_channels(family, uart_bus, cfg.instance)
                    {
                        f1_serial_note(ui, out);
                        f1_half_bus_note(
                            ui,
                            "USART",
                            ("TX", "RX"),
                            "stm32f1xx-hal builds a Serial only from the TX+RX pair",
                            wired_serial,
                        );
                        transport_row(ui, out, &mut cfg.blocking_dma, &chans, true);
                        if cfg.blocking_dma.any() {
                            api_row_locked_dma(ui, out);
                        } else {
                            api_row(ui, out, &mut pending.0);
                        }
                    } else {
                        api_row(ui, out, &mut pending.0);
                    }
                    // Every row this arm can draw is documented above, so the
                    // pane may show the roster. The FIRST arm to say so — the
                    // others show no Fields section until they can too, rather
                    // than a partial list that reads as the whole surface.
                    out.all_fields_documented();
                }
                ModuleConfig::Spi(cfg) => {
                    if let Some(partner) = block_partner {
                        let me = format!("SPIM{}", cfg.instance);
                        nrf_shared_block_row(ui, out, &me, partner, cfg.instance, is_async);
                    }
                    let roles = SpiRole::options(family);
                    if roles.len() == 1 {
                        out.skip("Role", docs::SKIP_SPI_ROLE);
                    }
                    if roles.len() > 1 {
                        out.field("Role", docs::SPI_ROLE);
                        ui.label("Role");
                        egui::ComboBox::from_id_salt("spirole")
                            .selected_text(cfg.role.label())
                            .show_ui(ui, |ui| {
                                for v in roles.iter().copied() {
                                    ui.selectable_value(&mut cfg.role, v, v.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "One peripheral, either end of the bus. As a SLAVE this chip \
                                 does not drive the clock or the chip select - the other side \
                                 does, and nothing moves until it asserts CS.",
                            );
                        ui.end_row();
                    }
                    let slave = cfg.role.is_slave();
                    // The ESP32's slave takes modes 1 and 3 only, and its own
                    // driver says so. Offering 0 and 2 there would be two
                    // settings that configure the peripheral wrong.
                    let modes = cfg.role.modes(family);
                    if !modes.contains(&cfg.mode) {
                        cfg.mode = modes[0];
                    }
                    out.field(
                        "SPI mode",
                        if modes.len() < 4 {
                            docs::SPI_MODE_SLAVE_ESP32
                        } else if slave {
                            docs::SPI_MODE_SLAVE
                        } else {
                            docs::SPI_MODE
                        },
                    );
                    ui.label("SPI mode");
                    egui::ComboBox::from_id_salt("spimode")
                        .selected_text(format!("Mode {}", cfg.mode))
                        .show_ui(ui, |ui| {
                            for md in modes.iter().copied() {
                                ui.selectable_value(&mut cfg.mode, md, format!("Mode {md}"));
                            }
                        })
                        .response
                        .on_hover_text(if modes.len() < 4 {
                            "This chip's SPI slave supports modes 1 and 3 only."
                        } else if slave {
                            "Must match what the master drives."
                        } else {
                            "CPOL/CPHA. The device on the other end states which it wants."
                        });
                    ui.end_row();
                    // Async only: the STM32F1 HAL's SPI takes no bit-order
                    // argument, so the field would be a control that does
                    // nothing there.
                    if !is_async {
                        out.skip("Bit order", docs::SKIP_SPI_BIT_ORDER);
                    }
                    if is_async {
                        out.field("Bit order", docs::SPI_BIT_ORDER);
                        ui.label("Bit order");
                        egui::ComboBox::from_id_salt("spibitorder")
                            .selected_text(match cfg.bit_order {
                                SpiBitOrder::MsbFirst => "MSB first",
                                SpiBitOrder::LsbFirst => "LSB first",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut cfg.bit_order,
                                    SpiBitOrder::MsbFirst,
                                    "MSB first",
                                )
                                .on_hover_text(
                                    "What nearly every device expects, and embassy's default.",
                                );
                                ui.selectable_value(
                                    &mut cfg.bit_order,
                                    SpiBitOrder::LsbFirst,
                                    "LSB first",
                                )
                                .on_hover_text(
                                    "Some sensors and shift registers. Getting this wrong gives bit-reversed data rather than silence, which is why it is worth setting deliberately.",
                                );
                            });
                        ui.end_row();
                    }
                    // A slave has no clock of its own: the master supplies it.
                    // Shown as a line rather than hidden, or the row would just
                    // vanish and leave the reader wondering.
                    if slave {
                        out.field("Clock", docs::SPI_CLOCK_SLAVE);
                        out.field("Transfers", docs::SPI_TRANSFERS_SLAVE);
                        ui.label("Clock");
                        ui.label(
                            egui::RichText::new("driven by the master")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.end_row();
                        ui.label("Transfers");
                        ui.label(
                            egui::RichText::new("DMA, always")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "esp-hal's slave driver has no CPU path and no blocking transfer: \
                             the master decides when bytes move, so a channel is taken \
                             whatever the runtime. The Configuration tab shows which.",
                        );
                        ui.end_row();
                        out.all_fields_documented();
                        break 'cfg;
                    }
                    out.field("Clock", docs::SPI_CLOCK);
                    ui.label("Clock");
                    egui::ComboBox::from_id_salt("spiclk")
                        .selected_text(hz_label(cfg.clock_hz))
                        .show_ui(ui, |ui| {
                            for hz in [
                                125_000u32, 250_000, 500_000, 1_000_000, 2_000_000, 4_000_000,
                                8_000_000,
                            ] {
                                ui.selectable_value(&mut cfg.clock_hz, hz, hz_label(hz));
                            }
                        });
                    ui.end_row();
                    if esp {
                        api_row(ui, out, &mut pending.0);
                        // Not a runtime choice: `with_dma` is on
                        // `impl Spi<'d, Blocking>` and hands back a
                        // `SpiDma<'d, Blocking>`, so a blocking project puts a
                        // master on DMA exactly as an async one does.
                        esp_dma_row(ui, out, &mut pending.1);
                        if pending.1 == AsyncBusMode::AsyncDma {
                            // ONE channel per bus on an ESP: `with_dma` drives
                            // both directions from it, unlike embassy's pair.
                            let req = format!("SPI{}", cfg.instance);
                            esp_dma_channel_row(ui, out, dma, &req, &mut cfg.dma_tx);
                        }
                    } else if is_async {
                        if crate::panels::mcu_module::codegen::rp::is_rp(family) {
                            rp_spi_init_locked(ui, out);
                            out.note(RP_DMA_NOTE);
                        } else if crate::panels::mcu_module::codegen::nrf::is_nrf(family) {
                            out.skip("Async init", docs::SKIP_ASYNC_INIT_NRF);
                        } else {
                            // NOT folded into the chain above: dropping this
                            // call took the Async-init combo off every STM32,
                            // and nothing but reading the screen would say so.
                            async_row(ui, out, &mut pending.1);
                            if pending.1 == AsyncBusMode::AsyncDma {
                                let inst = cfg.instance;
                                dma_row(
                                    ui,
                                    out,
                                    dma_map::Bus::Spi,
                                    inst,
                                    &mut cfg.dma_tx,
                                    &mut cfg.dma_rx,
                                );
                            }
                        }
                    } else if is_native {
                        api_row_locked(ui, out);
                    } else if let Some(chans) = codegen::stm32::blocking_dma_channels(
                        family,
                        dma_map::Bus::Spi,
                        cfg.instance,
                    ) {
                        transport_row(ui, out, &mut cfg.blocking_dma, &chans, has_miso);
                        if cfg.blocking_dma.any() {
                            api_row_locked_dma(ui, out);
                        } else {
                            api_row(ui, out, &mut pending.0);
                        }
                    } else {
                        api_row(ui, out, &mut pending.0);
                    }
                    // The master path ends here; the slave path marked itself
                    // before its early return.
                    out.all_fields_documented();
                }
                // One TIMER: the frequency it shares, then a duty slider per
                // channel actually wired — the channel list comes from the
                // module's own connections, so it mirrors the canvas.
                ModuleConfig::Timer(cfg) => {
                    // A PWM module is a TIM on an STM32, an LEDC timer on an
                    // ESP and a PWM SLICE on a Pico - three different things
                    // wearing one panel. `per_family` picks the sentence once
                    // so no row has to repeat the test.
                    let t_esp = family.starts_with("esp");
                    let t_rp = crate::panels::mcu_module::codegen::rp::is_rp(family);
                    let per_family = |stm32: &'static str,
                                      esp_: &'static str,
                                      rp_: &'static str| {
                        if t_rp {
                            rp_
                        } else if t_esp {
                            esp_
                        } else {
                            stm32
                        }
                    };
                    out.field(
                        "Frequency",
                        per_family(
                            docs::TIMER_FREQUENCY,
                            docs::TIMER_FREQUENCY_ESP,
                            docs::TIMER_FREQUENCY_RP,
                        ),
                    );
                    ui.label("Frequency");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.freq_hz)
                                .range(1..=1_000_000)
                                .suffix(" Hz")
                                .speed(10.0),
                        );
                        for (label, hz) in [("50 Hz", 50u32), ("1 kHz", 1_000), ("20 kHz", 20_000)]
                        {
                            if ui.small_button(label).clicked() {
                                cfg.freq_hz = hz;
                            }
                        }
                    });
                    ui.end_row();
                    // Duty RESOLUTION — ESP only. On an STM32 the resolution is
                    // whatever the reload value gives you; on the LEDC it is a
                    // register field, and one the frequency constrains: esp-hal
                    // refuses a divisor under 256, so 2^bits may not exceed
                    // 80 MHz / frequency. The picker therefore offers only the
                    // widths this frequency can actually carry, and "Auto" —
                    // the widest of them — stays the default.
                    if family.starts_with("esp") {
                        let max = esp_ledc_max_bits(cfg.freq_hz);
                        let min = esp_ledc_min_bits(cfg.freq_hz);
                        out.field("Duty resolution", docs::TIMER_DUTY_RESOLUTION_ESP);
                        ui.label("Duty resolution");
                        ui.horizontal(|ui| {
                            let text = match cfg.duty_res_bits {
                                None => format!("Auto ({max} bit)"),
                                Some(b) => format!("{b} bit"),
                            };
                            egui::ComboBox::from_id_salt("pwm_duty_res")
                                .selected_text(text)
                                .show_ui(ui, |ui| {
                                    ui.selectable_value(
                                        &mut cfg.duty_res_bits,
                                        None,
                                        format!("Auto ({max} bit)"),
                                    );
                                    for b in min..=max {
                                        ui.selectable_value(
                                            &mut cfg.duty_res_bits,
                                            Some(b),
                                            format!("{b} bit  ({} steps)", 1u32 << b),
                                        );
                                    }
                                });
                            // A resolution pinned at one frequency can stop
                            // fitting when the frequency rises. Said here rather
                            // than silently clamped: the number the user chose
                            // is the one they want to see.
                            // A width pinned at one frequency can stop fitting
                            // when the frequency moves — at EITHER end. Said
                            // here rather than silently clamped: the number the
                            // user chose is the one they should see.
                            if let Some(b) = cfg.duty_res_bits.filter(|b| *b > max || *b < min) {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} {b} bit does not fit {} Hz — {min}..={max} does",
                                        ph::WARNING,
                                        cfg.freq_hz
                                    ))
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(220, 180, 90)),
                                );
                            }
                        });
                        ui.end_row();
                    }
                    // The counter belongs to the TIMER, like the frequency —
                    // one counter, one shape. embassy takes it in
                    // `SimplePwm::new`; the note below says why the other
                    // runtimes do not show it.
                    if is_async {
                        out.field(
                            "Counter",
                            if t_rp {
                                docs::TIMER_COUNTING_INERT_RP
                            } else {
                                docs::TIMER_COUNTING
                            },
                        );
                        ui.label("Counter");
                        egui::ComboBox::from_id_salt("pwm_counting")
                            .selected_text(cfg.counting.label())
                            .show_ui(ui, |ui| {
                                for m in PwmCounting::ALL {
                                    ui.selectable_value(&mut cfg.counting, m, m.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "Center-aligned is what motor drive wants: the pulse sits in the middle of the period, so several channels do not all switch at the same instant. The three centred modes differ only in when the compare interrupt fires.",
                            );
                        ui.end_row();
                    }
                    // The pads wired to this module, grouped by CHANNEL: the
                    // duty is the channel's, and a channel can own two pads —
                    // CHx and its complementary CHxN. Both come from the
                    // signals' own labels, so these rows and the hint below
                    // cannot disagree.
                    let wired: BTreeSet<String> =
                        conn_rows.iter().map(|(sig, _, _)| (*sig).to_owned()).collect();
                    let mut pads: BTreeMap<u8, Vec<String>> = BTreeMap::new();
                    for (sig, pin, _) in &conn_rows {
                        let digits = sig.strip_prefix("CH").map(|r| r.trim_end_matches('N'));
                        if let Some(ch) = digits.and_then(|d| d.parse::<u8>().ok()) {
                            pads.entry(ch).or_default().push(format!("{sig} {pin}"));
                        }
                    }
                    if pads.is_empty() {
                        out.field(
                            "Channels",
                            per_family(
                                docs::TIMER_CHANNELS_EMPTY,
                                docs::TIMER_CHANNELS_EMPTY_ESP,
                                docs::TIMER_CHANNELS_EMPTY_RP,
                            ),
                        );
                        ui.label("Channels");
                        ui.label(
                            egui::RichText::new("none wired yet")
                                .size(11.0)
                                .italics()
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    for (ch, pins) in &pads {
                        let (ch, sig) = (*ch, format!("CH{ch}"));
                        let pin = pins.join(", ");
                        out.field(
                            "Channel duty",
                            per_family(docs::TIMER_DUTY, docs::TIMER_DUTY_ESP, docs::TIMER_DUTY_RP),
                        );
                        ui.label(format!("{sig} duty  ({pin})"));
                        // Percent with two decimals, stored as hundredths: a
                        // servo's 1.5 ms of a 20 ms frame is 7.5 %, and whole
                        // percent could not say it. Dragging lands on
                        // hundredths too, so the handle and the typed value
                        // agree.
                        let mut pct = cfg.duty_percent_of(ch);
                        if ui
                            .add(
                                egui::Slider::new(&mut pct, 0.0..=100.0)
                                    .suffix(" %")
                                    .fixed_decimals(2)
                                    .step_by(0.01),
                            )
                            .changed()
                        {
                            cfg.set_duty_x100(ch, (pct * 100.0).round().max(0.0) as u16);
                        }
                        ui.end_row();

                        if is_async {
                            // Edited on a COPY and written back only on a real
                            // change, so opening the panel does not fill the
                            // config with rows of defaults.
                            let before = cfg.channel_of(ch);
                            let mut shape = before;
                            out.field(
                                "Channel output",
                                if t_rp {
                                    docs::TIMER_CHANNEL_OUTPUT_INERT_RP
                                } else if pins.iter().any(|p| p.starts_with(&format!("CH{ch}N"))) {
                                    docs::TIMER_CHANNEL_OUTPUT_COMPLEMENTARY
                                } else {
                                    docs::TIMER_CHANNEL_OUTPUT
                                },
                            );
                            ui.label(format!("{sig} output"));
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt(("pwm_drive", ch))
                                    .width(88.0)
                                    .selected_text(shape.output.label())
                                    .show_ui(ui, |ui| {
                                        for v in PwmOutput::ALL {
                                            ui.selectable_value(&mut shape.output, v, v.label());
                                        }
                                    });
                                egui::ComboBox::from_id_salt(("pwm_pol", ch))
                                    .width(88.0)
                                    .selected_text(shape.polarity.label())
                                    .show_ui(ui, |ui| {
                                        for v in PwmPolarity::ALL {
                                            ui.selectable_value(&mut shape.polarity, v, v.label());
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Active low inverts the pin: 100 % duty then HOLDS IT LOW, which is what a current-sinking driver stage wants.",
                                    );
                                egui::ComboBox::from_id_salt(("pwm_mode", ch))
                                    .width(96.0)
                                    .selected_text(shape.mode.label())
                                    .show_ui(ui, |ui| {
                                        for v in PwmMode::ALL {
                                            ui.selectable_value(&mut shape.mode, v, v.label());
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Mode 2 reverses the comparison — a second route to the same inversion the polarity offers. CubeMX exposes both.",
                                    );
                            });
                            if shape != before {
                                cfg.set_channel(ch, shape);
                            }
                            ui.end_row();
                        }
                    }
                    // Channels are not added from this panel: the codegen
                    // derives them from the PIN functions, so the only way to
                    // gain one is to assign a pad on the canvas. Name the ones
                    // actually left — the old line hardcoded "CH2/3/4", which
                    // pointed at channels that could already be taken, never
                    // mentioned a freed CH1, offered channels the timer does
                    // not have, and kept advertising the action after all of
                    // them were wired.
                    // Dead time only matters once a pair exists, and only the
                    // async backend can emit it — see the note further down.
                    // `starts_with("CH")` matters: BKIN ends in an N too, and it
                    // is a fault input, not half of a pair.
                    let has_comp = wired
                        .iter()
                        .any(|s| s.starts_with("CH") && s.ends_with('N'));
                    if has_comp && is_async {
                        if is_advanced_timer(cfg.instance) {
                            out.field("Dead time", docs::TIMER_DEAD_TIME);
                            ui.label("Dead time");
                            ui.add(
                                crate::panels::drag_value(ui, &mut cfg.dead_time)
                                    .range(0..=u16::MAX)
                                    .suffix(" ticks"),
                            )
                            .on_hover_text(
                                "Ticks on the same scale as the duty compare value; embassy encodes them into the timer's CKD + DTG fields. 0 means the two pads switch at the same instant — fine for independent loads, fatal for a half-bridge.",
                            );
                            ui.end_row();
                        } else {
                            ui.label("");
                            ui.label(
                                egui::RichText::new(format!(
                                    "TIM{} has complementary pads wired, but embassy drives them through `ComplementaryPwm`, which covers the advanced-control timers (TIM1/8/20) only — they will not be initialised",
                                    cfg.instance
                                ))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(200, 140, 60)),
                            );
                            ui.end_row();
                        }
                    }
                    // ── Break inputs ────────────────────────────────────
                    // A fault line the timer watches by itself: when it asserts,
                    // every output goes off in hardware. Only shown for a pad
                    // that is actually wired — break is not something to switch
                    // on in the abstract.
                    let breaks: Vec<u8> = wired
                        .iter()
                        .filter_map(|s| match s.as_str() {
                            "BKIN" => Some(1u8),
                            "BKIN2" => Some(2),
                            _ => None,
                        })
                        .collect();
                    if !breaks.is_empty() && is_async && is_advanced_timer(cfg.instance) {
                        for i in &breaks {
                            let before = cfg.break_of(*i);
                            let mut b = before;
                            let name = if *i == 1 { "BKIN" } else { "BKIN2" };
                            out.field("Break input", docs::TIMER_BREAK_INPUT);
                            ui.label(format!("{name} fault"));
                            ui.horizontal(|ui| {
                                egui::ComboBox::from_id_salt(("brk_pol", i))
                                    .width(96.0)
                                    .selected_text(b.polarity.label())
                                    .show_ui(ui, |ui| {
                                        for v in BreakPolarity::ALL {
                                            ui.selectable_value(&mut b.polarity, v, v.label());
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "Which level on the pad means fault. Active low is the \
                                         usual wiring: the line is released when all is well, so \
                                         a broken wire also reads as a fault.",
                                    );
                                egui::ComboBox::from_id_salt(("brk_filt", i))
                                    .width(110.0)
                                    .selected_text(b.filter_label())
                                    .show_ui(ui, |ui| {
                                        for (code, (_, label)) in BREAK_FILTERS.iter().enumerate() {
                                            ui.selectable_value(&mut b.filter, code as u8, *label);
                                        }
                                    })
                                    .response
                                    .on_hover_text(
                                        "How many consecutive samples must agree before the fault \
                                         is believed, and how fast they are taken. No filter \
                                         reacts fastest and trusts every glitch.",
                                    );
                            });
                            if b != before {
                                cfg.set_break(*i, b);
                            }
                            ui.end_row();
                        }
                        out.field("After a fault", docs::TIMER_AUTO_OUTPUT_ENABLE);
                        ui.label("After a fault");
                        ui.checkbox(
                            &mut cfg.auto_output_enable,
                            "outputs come back on their own",
                        )
                        .on_hover_text(
                            "Off (the reset state, and the safer one): the outputs stay dark \
                             until software turns them back on, so a fault cannot be ridden out \
                             unnoticed. On: they resume at the next update event once the line \
                             releases.",
                        );
                        ui.end_row();
                    }
                    // These two used to be rows in the grid. Neither is a
                    // control, and both are long enough that the column could
                    // not shrink below them — which left the details pane a
                    // strip a word wide.
                    if family.starts_with("esp") {
                        out.note(
                            "esp-hal's LEDC takes duty in WHOLE percent - a fraction is \
                             rounded up in the generated file."
                                .to_owned(),
                        );
                    }
                    // "Assign TIM3 CH2" is STM32 vocabulary, and a Pico canvas
                    // never shows a TIM. On an RP the same facts have to be said
                    // in slices and A/B — and the channel, unlike the ESP's, is
                    // not a choice at all, which is worth saying out loud on the
                    // panel where someone would look for the picker.
                    if crate::panels::mcu_module::codegen::rp::is_rp(family) {
                        for n in rp_pwm_notes(cfg.instance, &conn_rows, pin_funcs, pin_names) {
                            out.note(n);
                        }
                    } else {
                        let free = free_pwm_channels(cfg.instance, &wired, pin_funcs);
                        if !free.is_empty() {
                            let list = free.join(" / ");
                            out.note(format!(
                                "This timer has channels left. Assign TIM{} {list} on the Pins \
                                 canvas and they join this module - they share its frequency.",
                                cfg.instance
                            ));
                        }
                    }
                    // Say why the output controls are absent instead of just
                    // dropping them: the reason differs per backend, and the
                    // second one is worth knowing before wiring a pad.
                    // The ESP is the exception: its LEDC driver is the same on
                    // both runtimes, so there is nothing missing to explain.
                    // The RP is a second exception, and the note was WRONG for it:
                    // `RpBackend::config_files` writes `pins/configs/pwm<slice>.rs`
                    // on Blocking, so telling a Pico owner that nothing is
                    // generated sent them to a runtime they did not need. The gate
                    // was written before the RP backend existed. The nRF is the
                    // same case: `NrfBackend::config_files` writes `pwm<n>.rs`.
                    if !is_async
                        && !family.starts_with("esp")
                        && !crate::panels::mcu_module::codegen::rp::is_rp(family)
                        && !crate::panels::mcu_module::codegen::nrf::is_nrf(family)
                    {
                        ui.label("");
                        let why = if family == "stm32f1" {
                            "counter mode, drive, polarity and PWM mode need the Async runtime — stm32f1xx-hal's `pwm_hz` cannot set them"
                        } else {
                            "this runtime emits no PWM code at all — only Async generates it (System tab)"
                        };
                        ui.label(
                            egui::RichText::new(why)
                                .size(10.5)
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // The HSPI. The smallest panel of the four external-memory
                // controllers, because the driver is: two widths, and the octal
                // one needs its strobe.
                ModuleConfig::Hspi(cfg) => {
                    let lanes = conn_rows
                        .iter()
                        .filter(|(sig, _, _)| sig.starts_with("IO"))
                        .count() as u8;
                    let dqs0 = conn_rows.iter().any(|(sig, _, _)| *sig == "DQS0");

                    ui.label("Mode");
                    let fits: Vec<HspiMode> = HspiMode::ALL
                        .into_iter()
                        .filter(|m| m.lanes() == lanes)
                        .collect();
                    // Which form was drawn is `fits`, and it is only known here -
                    // after the label, because the wired data lines decide it.
                    out.field(
                        "Mode",
                        if fits.is_empty() {
                            docs::HSPI_MODE_NO_FIT
                        } else {
                            docs::HSPI_MODE
                        },
                    );
                    if fits.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{lanes} data line(s) wired — embassy's HSPI builds 2 or 8, and \
                                 nothing between"
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else {
                        egui::ComboBox::from_id_salt("hspi_mode")
                            .selected_text(cfg.mode.label())
                            .show_ui(ui, |ui| {
                                for m in &fits {
                                    ui.selectable_value(&mut cfg.mode, *m, m.label());
                                }
                            });
                        if !fits.contains(&cfg.mode) {
                            cfg.mode = fits[0];
                        }
                    }
                    ui.end_row();

                    if cfg.mode == HspiMode::Octal && !dqs0 {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "the octal call REQUIRES DQS0 — wire it, or nothing is generated",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                        ui.end_row();
                    }

                    out.field("Device", docs::HSPI_DEVICE);
                    ui.label("Device");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("hspi_size")
                            .width(84.0)
                            .selected_text(cfg.size_label())
                            .show_ui(ui, |ui| {
                                for (i, name) in QSPI_MEMORY_SIZES.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut cfg.device_size,
                                        i as u8,
                                        name.trim_start_matches('_'),
                                    );
                                }
                            });
                        egui::ComboBox::from_id_salt("hspi_type")
                            .width(130.0)
                            .selected_text(cfg.memory_type.label())
                            .show_ui(ui, |ui| {
                                for v in OspiMemoryType::ALL {
                                    ui.selectable_value(&mut cfg.memory_type, v, v.label());
                                }
                            });
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.prescaler)
                                .range(0..=255)
                                .prefix("clk / "),
                        );
                    });
                    ui.end_row();

                    ui.label("");
                    ui.label(
                        egui::RichText::new(if is_async {
                            "the silicon carries IO0-IO15 and a second strobe, but embassy's \
                             driver stops at eight lines — the wider pads have no constructor"
                        } else {
                            "only the Async runtime emits HSPI code — the blocking backends \
                             generate GPIO and watchdogs only"
                        })
                        .size(10.5)
                        .color(egui::Color32::from_gray(140)),
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                // One XSPI port — the OCTOSPI panel one step wider, with the
                // strobes derived from the wiring rather than asked for.
                ModuleConfig::Xspi(cfg) => {
                    let lanes = conn_rows
                        .iter()
                        .filter(|(sig, _, _)| sig.starts_with("IO"))
                        .count() as u8;
                    let dqs0 = conn_rows.iter().any(|(sig, _, _)| *sig == "DQS0");
                    let dqs1 = conn_rows.iter().any(|(sig, _, _)| *sig == "DQS1");

                    ui.label("Mode");
                    let fits: Vec<XspiMode> = XspiMode::ALL
                        .into_iter()
                        .filter(|m| m.lanes() == lanes)
                        .collect();
                    // Which form was drawn is `fits`, and it is only known here -
                    // after the label, because the wired data lines decide it.
                    out.field(
                        "Mode",
                        if fits.is_empty() {
                            docs::XSPI_MODE_NO_FIT
                        } else {
                            docs::XSPI_MODE
                        },
                    );
                    if fits.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{lanes} data line(s) wired — the controller takes 2, 4, 8 or 16"
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else {
                        egui::ComboBox::from_id_salt("xspi_mode")
                            .selected_text(cfg.mode.label())
                            .show_ui(ui, |ui| {
                                for m in &fits {
                                    ui.selectable_value(&mut cfg.mode, *m, m.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "Only the modes your wiring can carry. Single and dual share two \
                                 pads, octal and dual-quad share eight — the pins cannot tell \
                                 them apart, so this asks.",
                            );
                        if !fits.contains(&cfg.mode) {
                            cfg.mode = fits[0];
                        }
                    }
                    ui.end_row();

                    out.field("Device", docs::XSPI_DEVICE);
                    ui.label("Device");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("xspi_size")
                            .width(84.0)
                            .selected_text(cfg.size_label())
                            .show_ui(ui, |ui| {
                                for (i, name) in QSPI_MEMORY_SIZES.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut cfg.device_size,
                                        i as u8,
                                        name.trim_start_matches('_'),
                                    );
                                }
                            });
                        egui::ComboBox::from_id_salt("xspi_type")
                            .width(140.0)
                            .selected_text(cfg.memory_type.label())
                            .show_ui(ui, |ui| {
                                for v in XspiMemoryType::ALL {
                                    ui.selectable_value(&mut cfg.memory_type, v, v.label());
                                }
                            });
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.prescaler)
                                .range(0..=255)
                                .prefix("clk / "),
                        );
                    });
                    ui.end_row();

                    if dqs0 {
                        ui.label("Strobe");
                        let text = if !cfg.mode.takes_dqs() {
                            "wired, but only the octal and hexadeca modes read it — it will be \
                             left out of the call"
                                .to_owned()
                        } else if dqs1 && cfg.mode == XspiMode::Hexa {
                            "DQS0 + DQS1 — the dual-strobe hexadeca call".to_owned()
                        } else if dqs1 {
                            "DQS1 needs the hexadeca mode; only DQS0 will be used".to_owned()
                        } else {
                            "DQS0".to_owned()
                        };
                        let warn = !cfg.mode.takes_dqs() || (dqs1 && cfg.mode != XspiMode::Hexa);
                        out.field(
                            "Strobe",
                            if !cfg.mode.takes_dqs() {
                                docs::XSPI_STROBE_IGNORED
                            } else if dqs1 && cfg.mode == XspiMode::Hexa {
                                docs::XSPI_STROBE_DUAL
                            } else if dqs1 {
                                docs::XSPI_STROBE_SECOND_UNUSED
                            } else {
                                docs::XSPI_STROBE
                            },
                        );
                        ui.label(
                            egui::RichText::new(text).size(10.5).color(if warn {
                                egui::Color32::from_rgb(200, 140, 60)
                            } else {
                                egui::Color32::from_gray(160)
                            }),
                        );
                        ui.end_row();
                    }
                    if !is_async {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "only the Async runtime emits XSPI code — the blocking backends \
                                 generate GPIO and watchdogs only",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // One OCTOSPI port. The width narrows the mode but does not
                // decide it: single and dual share two pads, octal and dual-quad
                // share eight — so the mode is asked for, and only the modes the
                // wiring can carry are offered.
                ModuleConfig::Ospi(cfg) => {
                    let lanes = conn_rows
                        .iter()
                        .filter(|(sig, _, _)| sig.starts_with("IO"))
                        .count() as u8;
                    let dqs = conn_rows.iter().any(|(sig, _, _)| *sig == "DQS");

                    ui.label("Mode");
                    let fits: Vec<OspiMode> = OspiMode::ALL
                        .into_iter()
                        .filter(|m| m.lanes() == lanes)
                        .collect();
                    // Which form was drawn is `fits`, and it is only known here -
                    // after the label, because the wired data lines decide it.
                    out.field(
                        "Mode",
                        if fits.is_empty() {
                            docs::OSPI_MODE_NO_FIT
                        } else {
                            docs::OSPI_MODE
                        },
                    );
                    if fits.is_empty() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{lanes} data line(s) wired — the controller takes 2, 4 or 8"
                            ))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                    } else {
                        egui::ComboBox::from_id_salt("ospi_mode")
                            .selected_text(cfg.mode.label())
                            .show_ui(ui, |ui| {
                                for m in &fits {
                                    ui.selectable_value(&mut cfg.mode, *m, m.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "Only the modes your wiring can carry. Single and dual use the \
                                 same two pads, octal and dual-quad the same eight — the pins \
                                 cannot tell them apart, so this asks.",
                            );
                        // Keep the setting inside what is wired: a mode the pads
                        // cannot carry would be refused at generation anyway.
                        if !fits.contains(&cfg.mode) {
                            cfg.mode = fits[0];
                        }
                    }
                    ui.end_row();

                    out.field("Device", docs::OSPI_DEVICE);
                    ui.label("Device");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("ospi_size")
                            .width(84.0)
                            .selected_text(cfg.size_label())
                            .show_ui(ui, |ui| {
                                for (i, name) in QSPI_MEMORY_SIZES.iter().enumerate() {
                                    ui.selectable_value(
                                        &mut cfg.device_size,
                                        i as u8,
                                        name.trim_start_matches('_'),
                                    );
                                }
                            });
                        egui::ComboBox::from_id_salt("ospi_type")
                            .width(130.0)
                            .selected_text(cfg.memory_type.label())
                            .show_ui(ui, |ui| {
                                for v in OspiMemoryType::ALL {
                                    ui.selectable_value(&mut cfg.memory_type, v, v.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "The device family changes how commands are framed. Standard \
                                 covers ordinary NOR flash; HyperBus is a different protocol \
                                 altogether.",
                            );
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.prescaler)
                                .range(0..=255)
                                .prefix("clk / "),
                        );
                    });
                    ui.end_row();

                    if dqs && cfg.mode != OspiMode::Octal {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "DQS is wired but only the octal mode reads it — the pad will \
                                 be left out of the call",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_rgb(200, 140, 60)),
                        );
                        ui.end_row();
                    }
                    if !is_async {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "only the Async runtime emits OCTOSPI code — the blocking \
                                 backends generate GPIO and watchdogs only",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // The external-flash controller. Which BANKS are wired is
                // which constructor embassy gets, so the panel reports the shape
                // and asks only for what the flash chip dictates.
                ModuleConfig::Qspi(cfg) => {
                    let bank = |b: u8| {
                        let tag = format!("BK{b} ");
                        let ios = conn_rows
                            .iter()
                            .filter(|(sig, _, _)| sig.starts_with(&tag) && sig.contains("IO"))
                            .count();
                        let ncs = conn_rows.iter().any(|(sig, _, _)| *sig == format!("BK{b} NCS"));
                        (ios, ncs)
                    };
                    let (io1, ncs1) = bank(1);
                    let (io2, ncs2) = bank(2);
                    let ok1 = io1 == 4 && ncs1;
                    let ok2 = io2 == 4 && ncs2;
                    let clk = conn_rows.iter().any(|(sig, _, _)| *sig == "CLK");

                    out.field(
                        "Wiring",
                        if clk && (ok1 || ok2) {
                            docs::QSPI_WIRING
                        } else {
                            docs::QSPI_WIRING_INCOMPLETE
                        },
                    );
                    ui.label("Wiring");
                    let (text, colour) = match (clk, ok1, ok2) {
                        (true, true, true) => (
                            "both banks — dual flash, 8 lines wide".to_owned(),
                            egui::Color32::from_gray(200),
                        ),
                        (true, true, false) => {
                            ("bank 1".to_owned(), egui::Color32::from_gray(200))
                        }
                        (true, false, true) => {
                            ("bank 2".to_owned(), egui::Color32::from_gray(200))
                        }
                        (false, _, _) => (
                            "no CLK wired".to_owned(),
                            egui::Color32::from_rgb(200, 140, 60),
                        ),
                        _ => (
                            format!(
                                "incomplete — a bank needs NCS and all four IO ({io1}/4 on BK1, \
                                 {io2}/4 on BK2)"
                            ),
                            egui::Color32::from_rgb(200, 140, 60),
                        ),
                    };
                    ui.label(egui::RichText::new(text).size(11.0).color(colour));
                    ui.end_row();

                    out.field("Flash size", docs::QSPI_FLASH_SIZE);
                    ui.label("Flash size");
                    egui::ComboBox::from_id_salt("qspi_size")
                        .selected_text(cfg.memory_size_label())
                        .show_ui(ui, |ui| {
                            for (i, name) in QSPI_MEMORY_SIZES.iter().enumerate() {
                                ui.selectable_value(
                                    &mut cfg.memory_size,
                                    i as u8,
                                    name.trim_start_matches('_'),
                                );
                            }
                        })
                        .response
                        .on_hover_text(
                            "The size of the chip on the board. The controller needs it to know \
                             where the memory-mapped window ends.",
                        );
                    ui.end_row();

                    out.field("Address", docs::QSPI_ADDRESS);
                    ui.label("Address");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("qspi_addr")
                            .width(88.0)
                            .selected_text(cfg.address_size.label())
                            .show_ui(ui, |ui| {
                                for v in QspiAddressSize::ALL {
                                    ui.selectable_value(&mut cfg.address_size, v, v.label());
                                }
                            })
                            .response
                            .on_hover_text(
                                "How many address bytes the chip expects. 24 bit covers up to \
                                 16 MiB; bigger flash needs 32.",
                            );
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.prescaler)
                                .range(0..=255)
                                .prefix("clk / "),
                        )
                        .on_hover_text(
                            "The bus runs at kernel clock / (prescaler + 1). 0 is the fastest \
                             the chip can do and often too fast for the flash.",
                        );
                    });
                    ui.end_row();

                    if !is_async {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "only the Async runtime emits QUADSPI code — the blocking \
                                 backends generate GPIO and watchdogs only",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // The SD-card controller. The bus WIDTH is not a setting: how
                // many data lanes are wired is the width, and each width is a
                // different embassy constructor — so the panel reports it
                // instead of asking.
                ModuleConfig::Sdmmc(cfg) => {
                    let lanes: Vec<u8> = conn_rows
                        .iter()
                        .filter_map(|(sig, _, _)| sig.strip_prefix("D")?.parse::<u8>().ok())
                        .collect();
                    let width = match lanes.len() {
                        1 => Some(1u8),
                        4 => Some(4),
                        8 => Some(8),
                        _ => None,
                    };
                    out.field(
                        "Bus width",
                        if width.is_some() {
                            docs::SDMMC_WIDTH
                        } else {
                            docs::SDMMC_WIDTH_UNSUPPORTED
                        },
                    );
                    ui.label("Bus width");
                    match width {
                        Some(w) => {
                            ui.label(
                                egui::RichText::new(format!("{w}-bit  ({} lanes wired)", lanes.len()))
                                    .size(11.0),
                            );
                        }
                        None => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} lanes wired — needs 1, 4 or 8",
                                    lanes.len()
                                ))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(200, 140, 60)),
                            );
                        }
                    }
                    ui.end_row();

                    out.field("Data timeout", docs::SDMMC_DATA_TIMEOUT);
                    ui.label("Data timeout");
                    ui.add(
                        crate::panels::drag_value(ui, &mut cfg.data_timeout)
                            .range(1_000..=100_000_000)
                            .speed(10_000.0),
                    )
                    .on_hover_text(
                        "In CARD bus clock periods, not microseconds — embassy's `Config` \
                         counts them. The default 5 000 000 is a few seconds on a slow card.",
                    );
                    ui.end_row();

                    let free: Vec<String> = (0..8u8)
                        .filter(|l| !lanes.contains(l))
                        .map(|l| format!("D{l}"))
                        .collect();
                    if !free.is_empty() {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(format!(
                                "widen the bus by assigning {} on the canvas",
                                free.join(" / ")
                            ))
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    if !is_async {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "only the Async runtime emits SD-card code — the blocking \
                                 backends generate GPIO and watchdogs only",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // One SAI unit, two independent sub-blocks. Each one that has
                // its three clock/data pads wired gets its own rows — the module
                // is the unit because `split_subblocks` happens once.
                ModuleConfig::Sai(cfg) => {
                    let wired: Vec<u8> = [1u8, 2]
                        .into_iter()
                        .filter(|b| {
                            let tag = if *b == 1 { "A " } else { "B " };
                            conn_rows.iter().any(|(sig, _, _)| sig.starts_with(tag))
                        })
                        .collect();
                    if wired.is_empty() {
                        out.field("Sub-blocks", docs::SAI_SUBBLOCKS);
                        ui.label("Sub-blocks");
                        ui.label(
                            egui::RichText::new("none wired yet")
                                .size(11.0)
                                .italics()
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    for b in &wired {
                        let letter = if *b == 1 { "A" } else { "B" };
                        let before = cfg.block_of(*b);
                        let mut blk = before;
                        out.field("Stream", docs::SAI_STREAM);
                        ui.label(format!("{letter} stream"));
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("sai_dir", b))
                                .width(84.0)
                                .selected_text(blk.tx_rx.label())
                                .show_ui(ui, |ui| {
                                    for v in SaiTxRx::ALL {
                                        ui.selectable_value(&mut blk.tx_rx, v, v.label());
                                    }
                                });
                            egui::ComboBox::from_id_salt(("sai_mode", b))
                                .width(120.0)
                                .selected_text(blk.mode.label())
                                .show_ui(ui, |ui| {
                                    for v in SaiMode::ALL {
                                        ui.selectable_value(&mut blk.mode, v, v.label());
                                    }
                                });
                            egui::ComboBox::from_id_salt(("sai_size", b))
                                .width(74.0)
                                .selected_text(blk.data_size.label())
                                .show_ui(ui, |ui| {
                                    for v in SaiDataSize::ALL {
                                        ui.selectable_value(&mut blk.data_size, v, v.label());
                                    }
                                });
                        });
                        ui.end_row();

                        out.field("Frame", docs::SAI_FRAME);
                        ui.label(format!("{letter} frame"));
                        ui.horizontal(|ui| {
                            egui::ComboBox::from_id_salt(("sai_sm", b))
                                .width(74.0)
                                .selected_text(blk.stereo_mono.label())
                                .show_ui(ui, |ui| {
                                    for v in SaiStereoMono::ALL {
                                        ui.selectable_value(&mut blk.stereo_mono, v, v.label());
                                    }
                                });
                            ui.add(
                                crate::panels::drag_value(ui, &mut blk.slot_count)
                                    .range(1..=16)
                                    .prefix("slots "),
                            );
                            ui.add(
                                crate::panels::drag_value(ui, &mut blk.frame_length)
                                    .range(8..=256)
                                    .suffix(" bit"),
                            )
                            .on_hover_text(
                                "Frame length in BITS — slots x slot size. 32 is one 16-bit \
                                 stereo frame.",
                            );
                            ui.add(
                                crate::panels::drag_value(ui, &mut blk.buffer_len)
                                    .range(32..=8192)
                                    .prefix("buf "),
                            );
                        });
                        if blk != before {
                            cfg.set_block(*b, blk);
                        }
                        ui.end_row();

                        if is_async {
                            let (dir, field) = if *b == 1 {
                                (dma_map::Dir::Tx, &mut cfg.dma_a)
                            } else {
                                (dma_map::Dir::Rx, &mut cfg.dma_b)
                            };
                            out.field("DMA", docs::SAI_DMA);
                            // SAI has no entry in the hand-written DMA tables, so
                            // the picker offers nothing and the allocator does the
                            // choosing. The row is here for the day it does.
                            dma_one(
                                ui,
                                dma_map::Bus::Spi,
                                cfg.instance,
                                dir,
                                &format!("{letter} DMA"),
                                field,
                            );
                        }
                    }
                    let free: Vec<&str> = [(1u8, "A"), (2, "B")]
                        .into_iter()
                        .filter(|(b, _)| !wired.contains(b))
                        .map(|(_, l)| l)
                        .collect();
                    if !free.is_empty() {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(format!(
                                "add a stream by assigning SAI{} {} SCK/SD/FS on the canvas",
                                cfg.instance,
                                free.join(" / ")
                            ))
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    ui.label("");
                    ui.label(
                        egui::RichText::new(if is_async {
                            "asynchronous mode only: a sub-block slaved to the other one \
                             (`new_synchronous`) needs its clock pads left unwired, which is a \
                             wiring rule of its own"
                        } else {
                            "only the Async runtime emits SAI — embassy drives it from a DMA \
                             ring buffer per sub-block"
                        })
                        .size(10.5)
                        .color(egui::Color32::from_gray(140)),
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                // One DAC block. The channel rows come from the module's own
                // connections, so they mirror the canvas — same shape as the
                // PWM module's duty rows, for the same reason.
                ModuleConfig::Dac(cfg) => {
                    let chans: Vec<(u8, String)> = conn_rows
                        .iter()
                        .filter_map(|(sig, pin, _)| {
                            Some((sig.strip_prefix("OUT")?.parse::<u8>().ok()?, pin.clone()))
                        })
                        .collect();
                    if chans.is_empty() {
                        out.field("Channels", docs::DAC_CHANNELS);
                        ui.label("Channels");
                        ui.label(
                            egui::RichText::new("none wired yet")
                                .size(11.0)
                                .italics()
                                .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    // An ESP's DAC is EIGHT bits (`Dac::write` takes a u8);
                    // the STM32's is twelve. The field stores twelve either
                    // way and the ESP codegen scales, so the slider only has
                    // to stop offering steps the hardware cannot land on.
                    let esp = crate::panels::mcu_module::codegen::family::is_esp(family);
                    let (top, mid) = if esp { (255u16, 128u16) } else { (4095, 2048) };
                    for (ch, pin) in &chans {
                        out.field(
                            "Start level",
                            if esp { docs::DAC_START_ESP } else { docs::DAC_START },
                        );
                        ui.label(format!("OUT{ch} start  ({pin})"));
                        let mut v = cfg.value_of(*ch).min(top);
                        ui.horizontal(|ui| {
                            if ui.add(egui::Slider::new(&mut v, 0..=top)).changed() {
                                cfg.set_value(*ch, v);
                            }
                            if ui.small_button("mid").clicked() {
                                cfg.set_value(*ch, mid);
                            }
                            if esp {
                                ui.label(
                                    egui::RichText::new("8-bit")
                                        .size(10.5)
                                        .color(egui::Color32::GRAY),
                                );
                            }
                        })
                        .response
                        .on_hover_text(
                            "The value the pad holds once `init` returns. There is no unset: the channel drives the pin the moment it is enabled, so the only honest choice is to say what it drives.",
                        );
                        ui.end_row();
                    }
                    let free: Vec<u8> = (1..=2u8)
                        .filter(|c| !chans.iter().any(|(w, _)| w == c))
                        .collect();
                    if !free.is_empty() {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(format!(
                                "add a channel by assigning DAC{} OUT{} on the canvas",
                                cfg.instance,
                                free.iter()
                                    .map(|c| c.to_string())
                                    .collect::<Vec<_>>()
                                    .join(" / ")
                            ))
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    if !is_async {
                        ui.label("");
                        ui.label(
                            egui::RichText::new(
                                "only the Async runtime emits DAC code today — the blocking backends generate GPIO and watchdogs only",
                            )
                            .size(10.5)
                            .color(egui::Color32::from_gray(140)),
                        );
                        ui.end_row();
                    }
                    out.all_fields_documented();
                }
                // One SPI block running as audio. Every setting here is a
                // field of embassy's `i2s::Config`, plus the ring buffer the
                // DMA owns — there is no blocking I2S to fall back on.
                ModuleConfig::I2s(cfg) => {
                    let is_esp =
                        crate::panels::mcu_module::codegen::family::is_esp(family);
                    out.field("Sample rate", docs::I2S_SAMPLE_RATE);
                    ui.label("Sample rate");
                    ui.horizontal(|ui| {
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.sample_rate_hz)
                                .range(8_000..=192_000)
                                .suffix(" Hz")
                                .speed(100.0),
                        );
                        for (label, hz) in [("44.1k", 44_100u32), ("48k", 48_000), ("96k", 96_000)]
                        {
                            if ui.small_button(label).clicked() {
                                cfg.sample_rate_hz = hz;
                            }
                        }
                    });
                    ui.end_row();

                    out.field("Direction", docs::I2S_DIRECTION);
                    ui.label("Direction");
                    egui::ComboBox::from_id_salt("i2s_dir")
                        .selected_text(cfg.direction.label())
                        .show_ui(ui, |ui| {
                            for v in I2sDirection::ALL {
                                ui.selectable_value(&mut cfg.direction, v, v.label());
                            }
                        })
                        .response
                        .on_hover_text(
                            "embassy has a constructor per direction. Full duplex needs the \
                             newer SPI IP (spi_v4/v5) and a second data pad, neither of which \
                             the IDE can check yet.",
                        );
                    ui.end_row();

                    out.field(
                        "Role",
                        if is_esp { docs::I2S_ROLE_ESP } else { docs::I2S_ROLE },
                    );
                    ui.label("Role");
                    egui::ComboBox::from_id_salt("i2s_mode")
                        .selected_text(cfg.mode.label())
                        .show_ui(ui, |ui| {
                            for v in I2sMode::options(family).iter().copied() {
                                ui.selectable_value(&mut cfg.mode, v, v.label());
                            }
                        });
                    ui.end_row();

                    out.field(
                        "Standard",
                        if is_esp {
                            docs::I2S_STANDARD_ESP
                        } else {
                            docs::I2S_STANDARD
                        },
                    );
                    ui.label("Standard");
                    egui::ComboBox::from_id_salt("i2s_std")
                        .selected_text(cfg.standard.label())
                        .show_ui(ui, |ui| {
                            for v in I2sStandard::options(family).iter().copied() {
                                ui.selectable_value(&mut cfg.standard, v, v.label());
                            }
                        });
                    ui.end_row();

                    out.field(
                        "Format",
                        if is_esp { docs::I2S_FORMAT_ESP } else { docs::I2S_FORMAT },
                    );
                    ui.label("Format");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("i2s_fmt")
                            .selected_text(cfg.format.label())
                            .show_ui(ui, |ui| {
                                for v in I2sFormat::options(family).iter().copied() {
                                    ui.selectable_value(&mut cfg.format, v, v.label());
                                }
                            });
                        // esp-hal's master config has no bit-clock polarity
                        // knob — only a word-select one, which is a different
                        // signal. Hidden rather than shown doing nothing.
                        if !is_esp {
                            egui::ComboBox::from_id_salt("i2s_pol")
                                .selected_text(cfg.clock_polarity.label())
                                .show_ui(ui, |ui| {
                                    for v in I2sClockPolarity::ALL {
                                        ui.selectable_value(
                                            &mut cfg.clock_polarity,
                                            v,
                                            v.label(),
                                        );
                                    }
                                });
                        }
                    });
                    ui.end_row();

                    out.field("Ring buffer", docs::I2S_BUFFER);
                    ui.label("Ring buffer");
                    ui.add(
                        crate::panels::drag_value(ui, &mut cfg.buffer_len)
                            .range(32..=8192)
                            .suffix(" samples"),
                    )
                    .on_hover_text(
                        "The DMA owns this buffer for the whole program. Too short and the \
                         audio breaks up on any scheduling hiccup; the samples are \
                         `format`-wide words.",
                    );
                    ui.end_row();

                    if is_async {
                        let inst = cfg.instance;
                        // Drawn by `dma_one`, which takes its label as an argument, so
                        // the row cannot document itself the way the others do.
                        out.field("DMA", docs::I2S_DMA);
                        // The DMA requests are the SPI block's — same silicon,
                        // same request lines.
                        if cfg.direction.is_tx() {
                            dma_one(ui, dma_map::Bus::Spi, inst, dma_map::Dir::Tx, "DMA", &mut cfg.dma_tx);
                        } else {
                            dma_one(ui, dma_map::Bus::Spi, inst, dma_map::Dir::Rx, "DMA", &mut cfg.dma_rx);
                        }
                    }

                    ui.label("");
                    ui.label(
                        egui::RichText::new(if is_async {
                            format!(
                                "runs on SPI{} — an SPI module on the same instance describes \
                                 the same block, and only one of the two is built",
                                cfg.instance
                            )
                        } else {
                            "only the Async runtime emits I2S — embassy drives it from a DMA \
                             ring buffer, and there is no blocking form"
                                .to_owned()
                        })
                        .size(10.5)
                        .color(egui::Color32::from_gray(140)),
                    );
                    ui.end_row();
                    out.all_fields_documented();
                }
                ModuleConfig::I2c(cfg) => {
                    if let Some(partner) = block_partner {
                        let me = format!("TWIM{}", cfg.instance);
                        nrf_shared_block_row(ui, out, &me, partner, cfg.instance, is_async);
                    }
                    out.field("Clock", docs::I2C_CLOCK);
                    ui.label("Clock");
                    egui::ComboBox::from_id_salt("i2cclk")
                        .selected_text(hz_label(cfg.clock_hz))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut cfg.clock_hz, 100_000, "100 kHz");
                            ui.selectable_value(&mut cfg.clock_hz, 400_000, "400 kHz");
                        });
                    ui.end_row();
                    // Async only: `embassy_time::Duration` needs embassy-stm32's
                    // `time` feature, which only the async dependency line pulls
                    // in (through `time-driver-any`).
                    //
                    // ...and embassy-STM32's, specifically. The gate was a bare
                    // `is_async`, so it fired on every async chip that is not an
                    // ESP — the RP included, where `embassy_rp::i2c::Config`
                    // carries the frequency and nothing else. `timeout_ms` is
                    // read by one emitter, and it is embassy-stm32's.
                    let rp_i2c = crate::panels::mcu_module::codegen::rp::is_rp(family);
                    let nrf_i2c = crate::panels::mcu_module::codegen::nrf::is_nrf(family);
                    if is_async && rp_i2c {
                        out.skip("Timeout", docs::SKIP_I2C_TIMEOUT_RP);
                    }
                    if is_async && nrf_i2c {
                        out.skip("Timeout", docs::SKIP_I2C_TIMEOUT_NRF);
                    }
                    if is_async && !rp_i2c && !nrf_i2c {
                        out.field("Timeout", docs::I2C_TIMEOUT);
                        ui.label("Timeout");
                        ui.horizontal(|ui| {
                            ui.add(
                                crate::panels::drag_value(ui, &mut cfg.timeout_ms)
                                    .speed(10.0)
                                    .range(0..=60_000)
                                    .suffix(" ms"),
                            );
                            ui.label(
                                egui::RichText::new(if cfg.timeout_ms == 0 {
                                    "0 = embassy default (1000 ms)".to_string()
                                } else {
                                    String::new()
                                })
                                .size(10.5)
                                .color(egui::Color32::from_gray(130)),
                            );
                        })
                        .response
                        .on_hover_text(
                            "How long a transfer may take before it gives up. I2C hangs are a real failure mode - a device that stretches the clock forever, or a bus with no pull-ups, blocks for as long as this allows.",
                        );
                        ui.end_row();
                    }
                    // One device is one row, as it always was. Several is a
                    // list — and the switch is the user's, never inferred: a
                    // project that has only ever had one address keeps the
                    // simpler panel AND the simpler generated output.
                    if cfg.devices.is_empty() {
                        out.field("Address (7-bit)", docs::I2C_ADDRESS);
                        ui.label("Address (7-bit)");
                        ui.horizontal(|ui| {
                            ui.add(
                                crate::panels::drag_value(ui, &mut cfg.address)
                                    .range(0..=127)
                                    .hexadecimal(2, false, true),
                            );
                            if ui
                                .small_button("+ device")
                                .on_hover_text(docs::I2C_DEVICES)
                                .clicked()
                            {
                                // The address already set IS the first device.
                                // Starting the list without it would read as the
                                // setting having been thrown away.
                                cfg.devices = vec![
                                    I2cDevice {
                                        name: String::new(),
                                        address: cfg.address,
                                    },
                                    I2cDevice::default(),
                                ];
                            }
                        });
                        ui.end_row();
                    } else {
                        out.field("Devices", docs::I2C_DEVICES);
                        ui.label("Devices");
                        let mut remove: Option<usize> = None;
                        let mut add = false;
                        ui.vertical(|ui| {
                            for (i, d) in cfg.devices.iter_mut().enumerate() {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut d.name)
                                            .desired_width(110.0)
                                            .hint_text(format!("device {}", i + 1)),
                                    );
                                    ui.add(
                                        crate::panels::drag_value(ui, &mut d.address)
                                            .range(0..=127)
                                            .hexadecimal(2, false, true),
                                    );
                                    if ui
                                        .small_button("x")
                                        .on_hover_text("Remove this device")
                                        .clicked()
                                    {
                                        remove = Some(i);
                                    }
                                });
                            }
                            add = ui.small_button("+ device").clicked();
                        });
                        if add {
                            cfg.devices.push(I2cDevice::default());
                        }
                        if let Some(i) = remove {
                            cfg.devices.remove(i);
                        }
                        // Back down to one device is the single-address case, so
                        // collapse to it: a one-row list means exactly what the
                        // Address row means, and leaving it would keep emitting a
                        // per-device file for a bus that no longer needs one.
                        if cfg.devices.len() == 1 {
                            cfg.address = cfg.devices[0].address;
                            cfg.devices.clear();
                        }
                        ui.end_row();
                    }
                    if is_async && rp_i2c {
                        out.note(RP_I2C_NOTE);
                    } else if is_async && nrf_i2c {
                        out.skip("Async init", docs::SKIP_ASYNC_INIT_NRF);
                    } else if is_async {
                        async_row(ui, out, &mut pending.1);
                        if pending.1 == AsyncBusMode::AsyncDma {
                            let inst = cfg.instance;
                            dma_row(
                                ui,
                                out,
                                dma_map::Bus::I2c,
                                inst,
                                &mut cfg.dma_tx,
                                &mut cfg.dma_rx,
                            );
                        }
                    } else if is_native {
                        api_row_locked(ui, out);
                    } else {
                        // Same rule as the USART above, and the only backend
                        // whose half-bus behaviour this turn verified.
                        if family == "stm32f1" {
                            f1_half_bus_note(
                                ui,
                                "I2C",
                                ("SCL", "SDA"),
                                "stm32f1xx-hal takes the SCL+SDA pair",
                                wired_i2c,
                            );
                        }
                        api_row(ui, out, &mut pending.0);
                    }
                    out.all_fields_documented();
                }
                ModuleConfig::Can(cfg) => {
                    let esp = crate::panels::mcu_module::codegen::family::is_esp(family);
                    out.field(
                        "Bit rate",
                        if esp { docs::CAN_BITRATE_ESP } else { docs::CAN_BITRATE },
                    );
                    ui.label("Bit rate");
                    egui::ComboBox::from_id_salt("canbr")
                        .selected_text(format!("{} kbit", cfg.bitrate / 1_000))
                        .show_ui(ui, |ui| {
                            for br in [125_000u32, 250_000, 500_000, 1_000_000] {
                                ui.selectable_value(
                                    &mut cfg.bitrate,
                                    br,
                                    format!("{} kbit", br / 1_000),
                                );
                            }
                        })
                        .response
                        .on_hover_text(if esp {
                            "esp-hal ships timings for these four only. Anything else \
                             needs a hand-computed `BaudRate::Custom` in the generated \
                             file."
                        } else {
                            "Every node on the bus must agree on this."
                        });
                    ui.end_row();

                    let modes = CanMode::options(family);
                    if modes.len() == 1 {
                        out.skip("Mode", docs::SKIP_CAN_MODE);
                    }
                    if modes.len() > 1 {
                        out.field("Mode", docs::CAN_MODE);
                        ui.label("Mode");
                        egui::ComboBox::from_id_salt("canmode")
                            .selected_text(cfg.mode.label())
                            .show_ui(ui, |ui| {
                                for v in modes.iter().copied() {
                                    ui.selectable_value(&mut cfg.mode, v, v.label())
                                        .on_hover_text(v.hint());
                                }
                            });
                        ui.end_row();

                        // Only the ESP has the second constructor. Everywhere
                        // else this row would be a switch that changes nothing.
                        out.field("Transceiver", docs::CAN_TRANSCEIVER);
                        ui.label("Transceiver");
                        ui.checkbox(&mut cfg.transceiver, "on the pads")
                            .on_hover_text(
                                "A real CAN bus needs one. Clear this only for two boards \
                                 wired TX-to-RX directly, which esp-hal builds with a \
                                 different constructor.",
                            );
                        ui.end_row();
                    }
                    // Third pair on this family — see the USART and I2C above.
                    if family == "stm32f1" {
                        f1_half_bus_note(
                            ui,
                            "CAN",
                            ("TX", "RX"),
                            "stm32f1xx-hal assigns the CAN pads as a TX+RX pair",
                            wired_can,
                        );
                    }
                    out.all_fields_documented();
                }
                ModuleConfig::Usb(cfg) => {
                    // Two controllers on one pad pair: which one the pads go to
                    // is the first question, and it decides every row below.
                    let roles = UsbRole::options(family);
                    if roles.len() > 1 {
                        out.field("Controller", docs::USB_CONTROLLER);
                        ui.label("Controller");
                        egui::ComboBox::from_id_salt("usbrole")
                            .selected_text(cfg.role.label())
                            .show_ui(ui, |ui| {
                                for v in roles.iter().copied() {
                                    ui.selectable_value(&mut cfg.role, v, v.label())
                                        .on_hover_text(v.hint());
                                }
                            })
                            .response
                            .on_hover_text(
                                "Both land on the same two pads, so only one can have them. \
                                 Serial/JTAG is the built-in console; OTG is a device of \
                                 your own design.",
                            );
                        ui.end_row();
                    } else if !roles.is_empty() && cfg.role != roles[0] {
                        // Carried over from a chip that HAD the other controller.
                        // Put it back rather than offer one this chip lacks.
                        cfg.role = roles[0];
                    }
                    // The three descriptor fields belong to a `usb-device` stack
                    // — the STM32 path's, or the ESP's own OTG. Serial/JTAG
                    // enumerates as Espressif's fixed `303a:1001` and has no
                    // descriptors, so there they would change nothing.
                    if crate::panels::mcu_module::codegen::family::is_esp(family)
                        && !cfg.role.is_otg()
                    {
                        out.field("Identity", docs::USB_IDENTITY);
                        ui.label("Identity");
                        ui.label(
                            egui::RichText::new("303a:1001  ·  fixed in silicon")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "The USB Serial/JTAG peripheral enumerates with Espressif's own VID:PID and a fixed descriptor set. Nothing here can change it - a board that needs its own identity uses a USB stack over the OTG controller instead, which this chip may not have.",
                        );
                        ui.end_row();
                        out.field("Port", docs::USB_PORT);
                        ui.label("Port");
                        ui.label(
                            egui::RichText::new("CDC serial, on the chip's own pads")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "A board with a USB-UART bridge chip shows that as well; the two are different devices to the host.",
                        );
                        ui.end_row();
                        // This path draws fewer rows and leaves here, so it
                        // marks its own roster complete.
                        out.all_fields_documented();
                        break 'cfg;
                    }
                    out.field("Product", docs::USB_PRODUCT);
                    ui.label("Product");
                    ui.add(
                        egui::TextEdit::singleline(&mut cfg.product)
                            .desired_width(140.0)
                            .hint_text("device name shown to host"),
                    );
                    ui.end_row();
                    out.field("Vendor ID", docs::USB_VID);
                    ui.label("Vendor ID");
                    ui.add(crate::panels::drag_value(ui, &mut cfg.vid).hexadecimal(4, false, true));
                    ui.end_row();
                    out.field("Product ID", docs::USB_PID);
                    ui.label("Product ID");
                    ui.add(crate::panels::drag_value(ui, &mut cfg.pid).hexadecimal(4, false, true));
                    ui.end_row();
                    if cfg.role.is_otg() {
                        out.field("Stack", docs::USB_STACK);
                        ui.label("Stack");
                        ui.label(
                            egui::RichText::new("usb-device + usbd-serial")
                                .size(11.0)
                                .color(egui::Color32::GRAY),
                        )
                        .on_hover_text(
                            "Added to Cargo.toml for you. `main.rs` builds a CDC serial \
                             device on the bus; poll it in your loop, or swap the class \
                             for any other the crate offers.",
                        );
                        ui.end_row();
                        // This path draws fewer rows and leaves here, so it
                        // marks its own roster complete.
                        out.all_fields_documented();
                        break 'cfg;
                    }
                    // Not a HAL constraint like the four above — the USB init
                    // takes PA11/PA12 directly, so one pad would spend the other
                    // uninvited.
                    if family == "stm32f1" {
                        f1_half_bus_note(
                            ui,
                            "USB",
                            ("D-", "D+"),
                            "the generated init takes PA11/PA12 directly, so one pad \
                             would spend the other uninvited",
                            wired_usb,
                        );
                    }
                    out.all_fields_documented();
                }
                // ── Custom: a hand-picked pin list ────────────────────────
                // No auto-wiring and no peripheral config — just the pins, in
                // the order they were added (that order is the generated
                // struct's field order and `new()`'s parameter order).
                ModuleConfig::Custom(cfg) => {
                    // Every label in a Custom module's panel is BOLD (user's ask).
                    // Struct name — pre-filled from the module name, then the
                    // user's own (so renaming the module can't break their impls).
                    //
                    // Label AND field share ONE grid cell: put the field in the
                    // second column and the pin rows below (which are wide, and
                    // live in the first) stretch that column, shoving this box to
                    // the far right, away from the Name field it belongs beside.
                    ui.horizontal(|ui| {
                        out.field("Struct", docs::CUSTOM_STRUCT);
                        custom_field_label(ui, "Struct");
                        let hint = derived_struct_name(&cfg.custom_label, cfg.instance);
                        ui.add(
                            egui::TextEdit::singleline(&mut cfg.struct_name)
                                .desired_width(CUSTOM_FIELD_W)
                                .hint_text(hint)
                                .font(egui::FontId::proportional(11.0)),
                        )
                        .on_hover_text(
                            "Name of the generated struct. Empty = follow the module name.",
                        );
                    });
                    ui.end_row();

                    // "Pins" sits on its OWN row right under Struct, and the
                    // rows below start at the far left — the panel then reads as
                    // a list instead of a label with a block hanging off it.
                    out.field("Pins", docs::CUSTOM_PINS);
                    ui.label(egui::RichText::new("Pins").strong());
                    ui.end_row();

                    let mut remove: Option<usize> = None;
                    for (i, pin) in cfg.pins.iter().enumerate() {
                        let num = *pin;
                        let name = pin_names
                            .get(&num)
                            .cloned()
                            .unwrap_or_else(|| format!("pin{num}"));
                        let cur = pin_funcs_current.get(&num);
                        let configured = cur.is_some_and(|f| *f != PinFunction::Unset);
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{num}"))
                                    .size(10.0)
                                    .color(egui::Color32::from_gray(140)),
                            );
                            // The pin NAME is a button opening the same function
                            // list the chip shows, so a pin can be configured
                            // without hunting for it on the canvas. Amber while
                            // it still has no function.
                            let btn_col = if configured {
                                egui::Color32::from_rgb(120, 180, 235)
                            } else {
                                egui::Color32::from_rgb(230, 170, 70)
                            };
                            ui.menu_button(
                                egui::RichText::new(&name)
                                    .size(10.5)
                                    .strong()
                                    .color(btn_col),
                                |ui| {
                                    ui.set_min_width(210.0);
                                    ui.label(
                                        egui::RichText::new(format!("{name} - function"))
                                            .size(10.0)
                                            .color(egui::Color32::GRAY),
                                    );
                                    ui.separator();
                                    for f in pin_funcs.get(&num).into_iter().flatten() {
                                        let selected = cur == Some(f);
                                        if ui
                                            .selectable_label(
                                                selected,
                                                // Same rule as the in-chip
                                                // function list, so the two
                                                // cannot disagree about how
                                                // a signal is named.
                                                egui::RichText::new(f.list_label())
                                                    .size(10.5)
                                                    .color(f.color()),
                                            )
                                            .clicked()
                                        {
                                            *pin_fn_choice = Some(PinEdit::Set(num, f.clone()));
                                            ui.close();
                                        }
                                    }
                                },
                            )
                            .response
                            .on_hover_text(if configured {
                                "Change this pin's function"
                            } else {
                                "This pin has NO function yet - pick one before Update"
                            });
                            if ui
                                .small_button(egui::RichText::new(ph::X).size(10.0))
                                .on_hover_text("Remove this pin from the module")
                                .clicked()
                            {
                                remove = Some(i);
                            }
                            // Mirror of the field inside the module's box.
                            let label = pin_labels.entry(num).or_default();
                            ui.add(
                                egui::TextEdit::singleline(label)
                                    .desired_width(110.0)
                                    .hint_text("name")
                                    .font(egui::FontId::proportional(10.0)),
                            )
                            .on_hover_text(
                                "Name appended to this pin's generated variable (and to its \
                                 field in the struct).",
                            );
                        });
                        ui.end_row();
                    }
                    if let Some(i) = remove {
                        cfg.pins.remove(i);
                    }
                    if cfg.pins.is_empty() {
                        ui.label(
                            egui::RichText::new("no pins yet - add at least one")
                                .size(10.0)
                                .italics()
                                .color(egui::Color32::from_rgb(220, 180, 90)),
                        );
                        ui.end_row();
                    }

                    out.field("Add pin", docs::CUSTOM_ADD_PIN);
                    // "+ Add pin" - only FREE pins (see `pin_blocked`).
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("custom_add_pin", m_id.clone()))
                            .selected_text(
                                egui::RichText::new(format!("{} Add pin", ph::PLUS)).size(10.5),
                            )
                            .show_ui(ui, |ui| {
                                let mut nums: Vec<usize> = pin_names.keys().copied().collect();
                                nums.sort_unstable();
                                let mut any = false;
                                for n in nums {
                                    if cfg.pins.contains(&n) || pin_blocked.contains(&n) {
                                        continue;
                                    }
                                    any = true;
                                    let nm = &pin_names[&n];
                                    if ui
                                        .selectable_label(
                                            false,
                                            egui::RichText::new(format!("{n}  {nm}"))
                                                .size(10.5)
                                                .monospace(),
                                        )
                                        .clicked()
                                    {
                                        cfg.pins.push(n);
                                    }
                                }
                                if !any {
                                    ui.label(
                                        egui::RichText::new(
                                            "no free pin left - only unassigned, non-reserved \
                                             pins can be added",
                                        )
                                        .size(10.0)
                                        .italics(),
                                    );
                                }
                            });
                    });
                    ui.end_row();

                    // Generating with an unconfigured pin would declare a field
                    // for a variable main.rs never binds, so Update turns AMBER
                    // and reports instead, naming the pins to fix.
                    let unconfigured: Vec<String> = cfg
                        .pins
                        .iter()
                        .filter(|n| {
                            pin_funcs_current
                                .get(n)
                                .map(|f| *f == PinFunction::Unset)
                                .unwrap_or(true)
                        })
                        .map(|n| {
                            pin_names
                                .get(n)
                                .cloned()
                                .unwrap_or_else(|| format!("pin{n}"))
                        })
                        .collect();
                    let incomplete = !unconfigured.is_empty();
                    let current_sig = custom_pins_sig(&cfg.pins, pin_sigs);
                    let pending = cfg.has_pending_pins(&current_sig);
                    out.field(
                        "Update",
                        if !pending {
                            docs::CUSTOM_UPDATE_DISABLED
                        } else if incomplete {
                            docs::CUSTOM_UPDATE_INCOMPLETE
                        } else {
                            docs::CUSTOM_UPDATE
                        },
                    );
                    out.field(
                        "Pin function",
                        if incomplete {
                            docs::CUSTOM_PIN_UNSET
                        } else {
                            docs::CUSTOM_PIN_FUNCTION
                        },
                    );
                    let warn_id = egui::Id::new(("custom_update_warn", m_id.clone()));
                    ui.horizontal(|ui| {
                        let btn = ui.add_enabled(
                            pending && !cfg.pins.is_empty(),
                            egui::Button::new(
                                egui::RichText::new(format!("{} Update", ph::ARROWS_CLOCKWISE))
                                    .size(10.5)
                                    .color(if !pending {
                                        egui::Color32::GRAY
                                    } else if incomplete {
                                        egui::Color32::from_rgb(240, 165, 60)
                                    } else {
                                        egui::Color32::from_rgb(120, 210, 140)
                                    }),
                            ),
                        );
                        if btn
                            .on_hover_text(
                                "Generate the struct from the pins above into a NEW file \
                                 (`configs/custom_<name>_<n>.rs`) and point main.rs at it. \
                                 Earlier revisions stay on disk, uncompiled.",
                            )
                            .on_disabled_hover_text("No pin changes to apply.")
                            .clicked()
                        {
                            if incomplete {
                                ui.ctx()
                                    .data_mut(|d| d.insert_temp(warn_id, unconfigured.join(", ")));
                            } else {
                                if !cfg.applied_pins.is_empty() {
                                    cfg.revision += 1;
                                }
                                cfg.applied_pins = cfg.pins.clone();
                                cfg.applied_sig = current_sig.clone();
                            }
                        }
                        if pending {
                            ui.label(
                                egui::RichText::new("pin changes not generated yet")
                                    .size(9.5)
                                    .italics()
                                    .color(egui::Color32::from_rgb(220, 180, 90)),
                            );
                        }
                    });
                    ui.end_row();

                    // The "Not all pins configured" dialog raised above.
                    if let Some(list) = ui.ctx().data(|d| d.get_temp::<String>(warn_id)) {
                        let mut open = true;
                        let mut dismiss = false;
                        egui::Window::new("Not all pins configured")
                            .collapsible(false)
                            .resizable(false)
                            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                            .open(&mut open)
                            .show(ui.ctx(), |ui| {
                                ui.label(
                                    egui::RichText::new(
                                        "These pins have no function yet, so the struct can't \
                                         be generated for them:",
                                    )
                                    .size(11.0),
                                );
                                ui.label(
                                    egui::RichText::new(&list)
                                        .strong()
                                        .monospace()
                                        .color(egui::Color32::from_rgb(240, 165, 60)),
                                );
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Click a pin's name above (or the pin on the chip) and \
                                         choose In / Out / ADC / PWM...",
                                    )
                                    .size(10.5)
                                    .color(egui::Color32::GRAY),
                                );
                                ui.add_space(6.0);
                                if ui.button("OK").clicked() {
                                    dismiss = true;
                                }
                            });
                        if !open || dismiss {
                            ui.ctx().data_mut(|d| d.remove::<String>(warn_id));
                        }
                    }
                    // Every row this arm draws is documented above; its pin list
                    // is the pane's own Pins section, not a config row.
                    out.all_fields_documented();
                }
            }
            }

            // Peripheral modules list their wired pins here; a custom module
            // already shows (and edits) its own pins above.
            if !is_custom {
                // Channels of this timer another pad already drives — they
                // are not on offer here, whatever the pad could do.
                let taken: BTreeSet<u8> = if m_kind == ModuleKind::GenericInterfaceTimer {
                    conn_rows
                        .iter()
                        .filter_map(|(sig, _, _)| pwm_plain_channel(sig))
                        .collect()
                } else {
                    BTreeSet::new()
                };
                for (sig, pin, num) in &conn_rows {
                    // A timer channel is the one signal whose NUMBER a pad can
                    // sometimes choose, so its label is a picker rather than a
                    // caption — but only where the pad really offers more than
                    // one. Elsewhere it stays the plain text it always was.
                    let cur = if m_kind == ModuleKind::GenericInterfaceTimer {
                        pwm_plain_channel(sig)
                    } else {
                        None
                    };
                    let choices = match cur {
                        Some(c) => {
                            let mut t = taken.clone();
                            t.remove(&c);
                            pwm_channel_choices(m_inst, *num, pin_funcs, &t)
                        }
                        None => Vec::new(),
                    };
                    if let Some(c) = cur
                        && choices.len() > 1
                    {
                        ui.horizontal(|ui| {
                            ui.menu_button(
                                egui::RichText::new(format!("{sig} {}", ph::CARET_DOWN))
                                    .size(11.0),
                                |ui| {
                                    ui.set_min_width(120.0);
                                    ui.label(
                                        egui::RichText::new("this pad drives")
                                            .size(10.0)
                                            .color(egui::Color32::GRAY),
                                    );
                                    ui.separator();
                                    for ch in &choices {
                                        if ui
                                            .selectable_label(
                                                *ch == c,
                                                egui::RichText::new(format!("CH{ch}")).size(10.5),
                                            )
                                            .clicked()
                                        {
                                            // Through the SAME door the canvas
                                            // uses, so the duty follows the
                                            // channel (`carry_pwm_channel`) and
                                            // the module re-wires itself.
                                            *pin_fn_choice = Some(PinEdit::Set(
                                                *num,
                                                PinFunction::TimerPwm {
                                                    timer: m_inst,
                                                    channel: *ch,
                                                },
                                            ));
                                            ui.close();
                                        }
                                    }
                                },
                            )
                            .response
                            .on_hover_text(
                                "Which channel of this timer the pad drives. The duty you set \
                                 moves with it.",
                            );
                            ui.label(format!("{} pin", ph::ARROW_RIGHT));
                        });
                    } else {
                        ui.label(format!("{sig} {} pin", ph::ARROW_RIGHT));
                    }
                    // The pad is a PICKER, not a caption. Auto-wiring chooses
                    // one legal wiring out of many and cannot know which pads
                    // the board needs free; this is the repair for that, and it
                    // is a MOVE - the two-step form either wipes the bus
                    // (`deselect_partners`) or leaves two pads on one signal.
                    let want = pin_funcs_current.get(num).cloned();
                    let others: Vec<usize> = want
                        .as_ref()
                        .map(|w| {
                            free_pads_for(
                                w,
                                *num,
                                pin_funcs,
                                pin_funcs_current,
                                family,
                                conn_rows.len(),
                            )
                        })
                        .unwrap_or_default();
                    if others.is_empty() {
                        ui.label(pin).on_hover_text(
                            if f1_moves_as_a_group(family, conn_rows.len()) {
                                "On an STM32F1 one AFIO bit remaps ALL of a peripheral's pads together - SPI1 is PA5/PA6/PA7 or PB3/PB4/PB5, and nothing mixed - so a single signal cannot move on its own. Re-wire the whole bus on the Pins canvas instead."
                            } else {
                                "No other pad on this chip can carry this signal while staying free."
                            },
                        );
                    } else {
                        ui.menu_button(
                            egui::RichText::new(format!("{pin} {}", ph::CARET_DOWN)).size(11.0),
                            |ui| {
                                ui.set_min_width(140.0);
                                ui.label(
                                    egui::RichText::new("move this signal to")
                                        .size(10.0)
                                        .color(egui::Color32::GRAY),
                                );
                                ui.separator();
                                // Every pad the CHIP offers, not the eight the
                                // automatic search looks at - a menu capped the
                                // way the search is would hide exactly the pad
                                // being hunted for.
                                egui::ScrollArea::vertical().max_height(220.0).show(
                                    ui,
                                    |ui| {
                                        for n in &others {
                                            let name = pin_names
                                                .get(n)
                                                .cloned()
                                                .unwrap_or_else(|| format!("pin{n}"));
                                            if ui
                                                .selectable_label(
                                                    false,
                                                    egui::RichText::new(name).size(10.5),
                                                )
                                                .clicked()
                                            {
                                                *pin_fn_choice = Some(PinEdit::Move {
                                                    from: *num,
                                                    to: *n,
                                                });
                                                ui.close();
                                            }
                                        }
                                    },
                                );
                            },
                        )
                        .response
                        .on_hover_text(
                            "Which pad carries this signal. Moving it here keeps the module and \
                             everything configured on it - only the wire moves.",
                        );
                    }
                    ui.end_row();
                }
            }
        });

    ui.add_space(4.0);
    // let label = |ui: &mut egui::Ui, t: &str| {
    //     ui.label(
    //         egui::RichText::new(t)
    //             .size(11.0)
    //             .color(egui::Color32::from_rgb(150, 150, 160)),
    //     );
    // };
    // label(ui, "RX data model");
    // ui.add(
    //     egui::TextEdit::multiline(m.config.rx_model_mut())
    //         .desired_rows(3)
    //         .desired_width(f32::INFINITY)
    //         .code_editor()
    //         .hint_text("data the device sends — e.g. struct Reading { temp: f32, .. }"),
    // );
    // label(ui, "TX data model");
    // ui.add(
    //     egui::TextEdit::multiline(m.config.tx_model_mut())
    //         .desired_rows(3)
    //         .desired_width(f32::INFINITY)
    //         .code_editor()
    //         .hint_text("data you send to the device — e.g. command frames"),
    // );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive the real `module_config_ui` headless and collect what it said.
    ///
    /// `egui::__run_test_ui` builds a `Context` with no fonts, so this costs
    /// microseconds and needs no window. Everything else is a default: the test
    /// is about which ROWS the arm draws for a given chip and runtime, and the
    /// pin map does not decide that for a USART.
    fn drive_usart(
        family: &str,
        is_async: bool,
        is_native: bool,
        line_extras: bool,
        edit: impl FnOnce(&mut UsartModuleConfig),
    ) -> ConfigOut {
        use crate::panels::mcu_module::modules::ModuleConfig;

        let mut cfg = UsartModuleConfig::new(1);
        edit(&mut cfg);
        drive(
            ModuleKind::GenericInterfaceUsart,
            ModuleConfig::Usart(cfg),
            family,
            is_async,
            is_native,
            line_extras,
        )
    }

    /// Drive one arm of the real `module_config_ui` headless.
    ///
    /// `egui::__run_test_ui` builds a `Context` with no fonts, so this costs
    /// microseconds and needs no window. Everything else is a default: the test
    /// is about which ROWS an arm draws for a given chip and runtime, and an
    /// empty pin map is a legitimate state for every one of them.
    fn drive(
        kind: ModuleKind,
        config: crate::panels::mcu_module::modules::ModuleConfig,
        family: &str,
        is_async: bool,
        is_native: bool,
        line_extras: bool,
    ) -> ConfigOut {
        drive_beside(kind, config, family, is_async, is_native, line_extras, None)
    }

    /// `drive`, with the peripheral sharing this module's nRF block, if any.
    fn drive_beside(
        kind: ModuleKind,
        config: crate::panels::mcu_module::modules::ModuleConfig,
        family: &str,
        is_async: bool,
        is_native: bool,
        line_extras: bool,
        block_partner: Option<&str>,
    ) -> ConfigOut {
        use crate::panels::mcu_module::modules::VirtualModule;

        let mut m = VirtualModule {
            id: format!("{}_1", kind.short().to_ascii_lowercase()),
            kind,
            name: kind.short().to_owned(),
            pos: (0.0, 0.0),
            config,
            connections: Vec::new(),
        };
        let mut out = ConfigOut::default();
        let mut labels = HashMap::new();
        let mut choice = None;
        let mut pending = (ApiStyle::Portable, AsyncBusMode::Blocking);
        egui::__run_test_ui(|ui| {
            module_config_ui(
                ui,
                &mut m,
                &HashMap::new(),
                &HashMap::new(),
                &std::collections::HashSet::new(),
                &mut labels,
                &HashMap::new(),
                &HashMap::new(),
                &mut choice,
                is_async,
                is_native,
                family,
                &mut pending,
                None,
                line_extras,
                block_partner,
                &mut out,
                &crate::panels::mcu_module::uart_baud::Chip::bare(
                    family,
                    if is_async {
                        crate::panels::mcu_module::mcu::Runtime::Async
                    } else {
                        crate::panels::mcu_module::mcu::Runtime::Blocking
                    },
                ),
            );
        });
        out
    }

    fn labels_of(out: &ConfigOut) -> Vec<&str> {
        out.fields()
            .expect("the USART arm marks itself documented")
            .iter()
            .map(|f| f.label.as_str())
            .collect()
    }

    /// EVERY kind says where its four un-drawn settings live.
    ///
    /// The instance, the name and the two data models are on every config and
    /// no arm draws them, so a reader comparing the pane with what the project
    /// saves would otherwise find four fields the IDE never mentions. This runs
    /// the real panel for all of `ModuleKind::ALL` on four chips.
    #[test]
    fn every_kind_says_where_its_shared_settings_live() {
        for kind in ModuleKind::ALL {
            for family in ["stm32f1", "stm32g0", "esp32c3", "rp2040", "nrf52833"] {
                let out = drive(kind, kind.default_config(1), family, false, false, false);
                let got: Vec<&str> = out
                    .elsewhere_fields()
                    .iter()
                    .map(|f| f.label.as_str())
                    .collect();
                assert_eq!(
                    got,
                    ["Instance", "Name", "Data models"],
                    "{kind:?} on {family}"
                );
                for f in out.elsewhere_fields() {
                    assert!(!f.doc.trim().is_empty(), "{kind:?}: {} is blank", f.label);
                }
            }
        }
    }

    /// The instance is the one shared field whose MEANING moves with the chip,
    /// so it gets the chip's own sentence and not a true-but-useless one.
    #[test]
    fn a_timers_instance_is_named_in_the_chips_own_words() {
        let doc = |family: &str| {
            drive(
                ModuleKind::GenericInterfaceTimer,
                ModuleKind::GenericInterfaceTimer.default_config(1),
                family,
                false,
                false,
                false,
            )
            .elsewhere_fields()[0]
                .doc
                .clone()
        };
        assert_eq!(doc("stm32g0"), docs::SHARED_INSTANCE_TIMER_STM32);
        assert_eq!(doc("esp32c3"), docs::SHARED_INSTANCE_TIMER_ESP);
        assert_eq!(doc("rp2040"), docs::SHARED_INSTANCE_TIMER_RP);

        // A bus module is a bus module everywhere; only the timer diverges.
        let spi = drive(
            ModuleKind::GenericInterfaceSpi,
            ModuleKind::GenericInterfaceSpi.default_config(1),
            "rp2040",
            false,
            false,
            false,
        );
        assert_eq!(spi.elsewhere_fields()[0].doc, docs::SHARED_INSTANCE);

        // A custom module drives no peripheral, and says so rather than
        // pretending the number means something.
        let custom = drive(
            ModuleKind::Custom,
            ModuleKind::Custom.default_config(1),
            "stm32g0",
            false,
            false,
            false,
        );
        assert_eq!(
            custom.elsewhere_fields()[0].doc,
            docs::SHARED_INSTANCE_CUSTOM
        );
    }

    /// The Pico backend never reads `custom_label`, so the pane must not
    /// promise that a name reaches the generated handles there.
    #[test]
    fn the_name_row_admits_the_pico_ignores_it() {
        let rp = drive(
            ModuleKind::GenericInterfaceSpi,
            ModuleKind::GenericInterfaceSpi.default_config(1),
            "rp2040",
            false,
            false,
            false,
        );
        assert_eq!(rp.elsewhere_fields()[1].doc, docs::SHARED_NAME_RP);

        let stm = drive(
            ModuleKind::GenericInterfaceSpi,
            ModuleKind::GenericInterfaceSpi.default_config(1),
            "stm32g0",
            false,
            false,
            false,
        );
        assert_eq!(stm.elsewhere_fields()[1].doc, docs::SHARED_NAME);
    }

    /// Every doc ANY arm hands out is a NAMED const from `module_docs`.
    ///
    /// This is the invariant the whole design rests on: the hover and the pane
    /// read one `&'static str`, so they cannot drift. An inline literal at a row
    /// site would still compile and still look right in the pane - and would be
    /// the second copy this module exists to prevent. Here it fails.
    ///
    /// Runs every kind against four chips and three runtimes, which is also the
    /// only thing that exercises most of these arms at all.
    #[test]
    fn every_field_doc_is_a_named_const() {
        for kind in ModuleKind::ALL {
            for family in ["stm32f1", "stm32g0", "esp32c3", "rp2040", "nrf52833"] {
                for (is_async, is_native) in [(false, false), (true, false), (false, true)] {
                    for extras in [false, true] {
                        let out = drive(
                            kind,
                            kind.default_config(1),
                            family,
                            is_async,
                            is_native,
                            extras,
                        );
                        let seen = out.fields().into_iter().flatten();
                        for f in seen.chain(out.elsewhere_fields()) {
                            assert!(
                                docs::ALL_DOCS.iter().any(|(_, d)| *d == f.doc.as_ref()),
                                "{kind:?} on {family}: {:?} carries a doc that is not a                                  module_docs const:
{}",
                                f.label,
                                f.doc
                            );
                        }
                    }
                }
            }
        }
    }

    /// Every arm says it documents its rows, so no kind shows an empty pane.
    ///
    /// The gate exists so a half-rolled-out kind shows nothing rather than a
    /// partial roster; now that every arm is done, a kind that goes quiet is a
    /// regression rather than work in progress.
    #[test]
    fn every_kind_has_a_finished_roster() {
        for kind in ModuleKind::ALL {
            let out = drive(kind, kind.default_config(1), "esp32c3", false, false, false);
            assert!(
                out.fields().is_some(),
                "{kind:?} never called all_fields_documented()"
            );
        }
    }

    /// The config STATES worth driving for a kind, not just its default.
    ///
    /// Several rows are gated by the module's own settings rather than by the
    /// chip - Touch's sleep interval belongs to the continuous scan, and the
    /// default scan is one-shot. A matrix that only varies the chip never draws
    /// them, so it cannot tell "this row is gated" from "this row does not
    /// exist", which is exactly what the skip tests below have to distinguish.
    fn config_variants(kind: ModuleKind) -> Vec<crate::panels::mcu_module::modules::ModuleConfig> {
        use crate::panels::mcu_module::modules::{I2cDevice, ModuleConfig, TouchScan};
        let mut out = vec![kind.default_config(1)];
        if let ModuleConfig::Touch(mut c) = kind.default_config(1) {
            c.scan = TouchScan::Continuous;
            out.push(ModuleConfig::Touch(c));
        }
        // An I2C bus carrying several devices draws a list where one device draws
        // a single Address row. Without this variant the list is a row the roster
        // never sees, so it could not be documented without the guard calling it
        // a row nothing draws.
        if let ModuleConfig::I2c(mut c) = kind.default_config(1) {
            c.devices = vec![
                I2cDevice {
                    name: "display".into(),
                    address: 0x3C,
                },
                I2cDevice {
                    name: "imu".into(),
                    address: 0x68,
                },
            ];
            out.push(ModuleConfig::I2c(c));
        }
        out
    }

    /// Every cell of the matrix, as (kind, family, is_async, is_native).
    ///
    /// Wider than the USART cells below: the arms differ by family far more than
    /// by runtime, and several are drawn on one chip only.
    fn every_cell() -> Vec<(ModuleKind, &'static str, bool, bool)> {
        let mut out = Vec::new();
        for kind in ModuleKind::ALL {
            for family in [
                "stm32f1", "stm32g0", "esp32", "esp32c3", "rp2040", "nrf52833",
            ] {
                for (a, n) in [(false, false), (true, false), (false, true)] {
                    out.push((kind, family, a, n));
                }
            }
        }
        out
    }

    /// Walk every cell and collect, per kind, every row label it ever draws and
    /// the doc each label was given.
    ///
    /// The one source for both the roster table and the tests that pin it, so
    /// the table cannot be generated from one reading and checked against
    /// another.
    fn matrix_labels() -> std::collections::BTreeMap<
        &'static str,
        std::collections::BTreeMap<String, std::collections::BTreeSet<&'static str>>,
    > {
        let mut out: std::collections::BTreeMap<
            &'static str,
            std::collections::BTreeMap<String, std::collections::BTreeSet<&'static str>>,
        > = std::collections::BTreeMap::new();
        for (kind, family, is_async, is_native) in every_cell() {
            for extras in [false, true] {
                for cfg in config_variants(kind) {
                    let o = drive(kind, cfg, family, is_async, is_native, extras);
                    for f in o.fields().into_iter().flatten() {
                        let named = docs::ALL_DOCS
                            .iter()
                            .find(|(_, d)| *d == f.doc.as_ref())
                            .map(|(n, _)| *n);
                        let e = out
                            .entry(kind.short())
                            .or_default()
                            .entry(f.label.clone())
                            .or_default();
                        if let Some(n) = named {
                            e.insert(n);
                        }
                    }
                }
            }
        }
        out
    }

    /// Print `ROSTER` for pasting into `module_docs.rs`.
    ///
    /// ```text
    /// cargo test regenerate_the_roster -- --ignored --nocapture
    /// ```
    ///
    /// The table is DERIVED, never hand-written: it is the union of every label
    /// the panel draws across every chip, runtime and config state the matrix
    /// reaches. `the_roster_matches_the_matrix` then pins it from both sides, so
    /// a row added without regenerating fails rather than going quietly missing.
    ///
    /// A label whose doc is the same in every cell carries that const's NAME; a
    /// label whose meaning changes with the chip carries none, because there is
    /// no single sentence to show for a row this chip did not draw.
    #[test]
    #[ignore]
    fn regenerate_the_roster() {
        println!("pub const ROSTER: &[(&str, &[(&str, Option<&str>)])] = &[");
        for (kind, rows) in matrix_labels() {
            println!("    (\"{kind}\", &[");
            for (label, named) in rows {
                let doc = if named.len() == 1 {
                    format!("Some({})", named.iter().next().unwrap())
                } else {
                    "None".to_owned()
                };
                println!("        (\"{label}\", {doc}),");
            }
            println!("    ]),");
        }
        println!("];");
    }

    /// The roster is exactly what the matrix draws - no more, no less.
    ///
    /// Pinned from BOTH sides on purpose. Missing an entry means the pane will
    /// not mention a setting this module has, which is the silence the roster
    /// exists to end; a spare entry means it names a row nothing can draw any
    /// more, which is worse - the reader goes looking for a control that was
    /// deleted.
    ///
    /// Regenerate rather than patch by hand:
    /// `cargo test regenerate_the_roster -- --ignored --nocapture`.
    #[test]
    fn the_roster_matches_the_matrix() {
        let seen = matrix_labels();
        let listed: std::collections::BTreeMap<&str, Vec<&str>> = docs::ROSTER
            .iter()
            .map(|(k, rows)| (*k, rows.iter().map(|(l, _)| *l).collect()))
            .collect();

        for (kind, rows) in &seen {
            let have = listed
                .get(kind)
                .unwrap_or_else(|| panic!("{kind} draws rows but is absent from ROSTER"));
            for label in rows.keys() {
                assert!(
                    have.contains(&label.as_str()),
                    "{kind} draws {label:?}, which ROSTER does not list - regenerate it"
                );
            }
        }
        for (kind, rows) in &listed {
            let drawn = seen
                .get(kind)
                .unwrap_or_else(|| panic!("ROSTER lists {kind}, which draws nothing"));
            for label in rows {
                assert!(
                    drawn.contains_key(*label),
                    "ROSTER lists {kind} {label:?}, which no chip or runtime draws -                      regenerate it"
                );
            }
        }
    }

    /// The roster is keyed by the kind's short name, so two kinds sharing one
    /// would silently merge their rows.
    #[test]
    fn every_kind_has_its_own_short_name() {
        let mut seen: Vec<&str> = ModuleKind::ALL.iter().map(|k| k.short()).collect();
        let before = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(before, seen.len(), "two kinds share a short name");
    }

    /// A row explained as ABSENT must be a row that exists somewhere.
    ///
    /// "Not offered here" is the pane saying why you cannot find a setting. If
    /// the label it names is one no cell ever draws, the pane is explaining the
    /// absence of something that does not exist - a sentence about nothing,
    /// which is worse than silence. This is also what catches a row being
    /// renamed while its skip keeps the old label.
    #[test]
    fn every_skipped_row_is_a_row_some_chip_draws() {
        let mut drawn: std::collections::HashSet<(ModuleKind, String)> =
            std::collections::HashSet::new();
        let mut skipped: std::collections::HashSet<(ModuleKind, String)> =
            std::collections::HashSet::new();
        for (kind, family, is_async, is_native) in every_cell() {
            for extras in [false, true] {
                for cfg in config_variants(kind) {
                    let out = drive(kind, cfg, family, is_async, is_native, extras);
                    for f in out.fields().into_iter().flatten() {
                        drawn.insert((kind, f.label.clone()));
                    }
                    for f in out.skipped_fields() {
                        skipped.insert((kind, f.label.clone()));
                    }
                }
            }
        }
        assert!(!skipped.is_empty(), "the matrix reached no skip at all");
        for (kind, label) in &skipped {
            assert!(
                drawn.contains(&(*kind, label.clone())),
                "{kind:?} explains why {label:?} is absent, but no chip or runtime ever \n                 draws a row by that name"
            );
        }
    }

    /// A row is never both drawn and explained as absent in the SAME cell.
    ///
    /// The two answer opposite questions, so a cell that says both has a gate
    /// whose two halves disagree - and the pane would show the row's meaning
    /// under Fields and its absence under Not offered here, at once.
    #[test]
    fn no_cell_both_draws_a_row_and_explains_its_absence() {
        for (kind, family, is_async, is_native) in every_cell() {
            for extras in [false, true] {
                for cfg in config_variants(kind) {
                    let out = drive(kind, cfg, family, is_async, is_native, extras);
                    let here: Vec<&str> = out
                        .fields()
                        .into_iter()
                        .flatten()
                        .map(|f| f.label.as_str())
                        .collect();
                    for f in out.skipped_fields() {
                        assert!(
                            !here.contains(&f.label.as_str()),
                            "{kind:?} on {family} (async={is_async}, native={is_native}) both draws \n                         {:?} and says it is not offered",
                            f.label
                        );
                    }
                }
            }
        }
    }

    /// The chip/runtime cells this arm is expected to serve.
    const CELLS: &[(&str, bool, bool)] = &[
        ("stm32f1", false, false),
        ("stm32f1", false, true),
        ("stm32f1", true, false),
        ("stm32g0", false, false),
        ("stm32g0", true, false),
        ("esp32c3", false, false),
        ("esp32c3", true, false),
        ("rp2040", true, false),
        ("nrf52833", false, false),
        ("nrf52833", true, false),
    ];

    /// A row is drawn only where the backend can pass its value to something.
    ///
    /// Three rows failed that on an RP: the wire frame (drawn everywhere,
    /// consumed only by the two STM32 emitters), the RX DMA buffer (`buf_len`
    /// reaches the RP through its BUFFERED branch alone) and the I2C timeout
    /// (read by exactly one emitter, embassy-stm32's).
    ///
    /// The frame was fixed by making the RP emit it; the other two by saying
    /// they are not there, with the reason.
    mod a_row_the_backend_cannot_use {
        use super::*;
        use crate::panels::mcu_module::modules::{I2cModuleConfig, ModuleConfig, UsartMode};

        fn skipped_of(out: &ConfigOut) -> Vec<&str> {
            out.skipped_fields()
                .iter()
                .map(|f| f.label.as_str())
                .collect()
        }

        /// The DMA transport has no buffer on an RP, and the pane says so
        /// instead of offering a size for one.
        #[test]
        fn the_rx_dma_buffer_is_absent_on_an_rp_and_explains_itself() {
            let out = drive_usart("rp2040", true, false, false, |c| c.mode = UsartMode::Dma);
            assert!(
                !labels_of(&out).contains(&"RX DMA buffer"),
                "no row: {:?}",
                labels_of(&out)
            );
            assert!(
                skipped_of(&out).contains(&"RX DMA buffer"),
                "and it is explained: {:?}",
                skipped_of(&out)
            );
        }

        /// The buffered transport DOES allocate two rings there, so the row is
        /// real and stays.
        #[test]
        fn the_buffered_transport_keeps_its_buffer_row_on_an_rp() {
            let out = drive_usart("rp2040", true, false, false, |c| {
                c.mode = UsartMode::Buffered
            });
            assert!(labels_of(&out).contains(&"RX/TX buffer"));
        }

        /// And an STM32 keeps the DMA row, which its emitter reads.
        #[test]
        fn an_stm32_keeps_the_rx_dma_buffer_row() {
            let out = drive_usart("stm32g0", true, false, false, |c| c.mode = UsartMode::Dma);
            assert!(labels_of(&out).contains(&"RX DMA buffer"));
        }

        fn i2c_labels(family: &str, is_async: bool) -> (Vec<String>, Vec<String>) {
            let out = drive(
                ModuleKind::GenericInterfaceI2c,
                ModuleConfig::I2c(I2cModuleConfig::new(1)),
                family,
                is_async,
                false,
                false,
            );
            (
                out.fields()
                    .expect("the I2C arm marks itself documented")
                    .iter()
                    .map(|f| f.label.clone())
                    .collect(),
                out.skipped_fields()
                    .iter()
                    .map(|f| f.label.clone())
                    .collect(),
            )
        }

        /// `embassy_rp::i2c::Config` carries the frequency and nothing else.
        #[test]
        fn the_i2c_timeout_is_absent_on_an_rp_and_explains_itself() {
            let (drawn, skipped) = i2c_labels("rp2040", true);
            assert!(!drawn.iter().any(|l| l == "Timeout"), "no row: {drawn:?}");
            assert!(
                skipped.iter().any(|l| l == "Timeout"),
                "and it is explained: {skipped:?}"
            );
        }

        /// An async STM32 keeps it — `config.timeout` is embassy-stm32's own.
        #[test]
        fn an_async_stm32_keeps_the_i2c_timeout() {
            let (drawn, _) = i2c_labels("stm32g0", true);
            assert!(drawn.iter().any(|l| l == "Timeout"), "{drawn:?}");
        }

        /// On an async nRF the UARTE has EasyDMA built in: no transport to
        /// pick, no ring to size and no channels, on either `mode` a project
        /// might carry. Direction and flow control stay, because the
        /// constructor reads them.
        #[test]
        fn an_async_nrf_uart_offers_no_transport_buffer_or_channels() {
            for mode in [UsartMode::Buffered, UsartMode::Dma] {
                let out = drive_usart("nrf52833", true, false, false, |c| c.mode = mode);
                let drawn = labels_of(&out);
                let skipped = skipped_of(&out);
                for gone in ["Async transport", "RX/TX buffer", "RX DMA buffer"] {
                    assert!(!drawn.contains(&gone), "{mode:?}: {gone} drawn: {drawn:?}");
                }
                assert!(skipped.contains(&"Async transport"), "{skipped:?}");
                assert!(
                    skipped.contains(&"RX/TX buffer") || skipped.contains(&"RX DMA buffer"),
                    "{skipped:?}"
                );
                assert!(drawn.contains(&"Data direction"), "{drawn:?}");
                assert!(drawn.contains(&"Hardware flow control"), "{drawn:?}");
                assert!(
                    !drawn.iter().any(|l| l.contains("DMA")),
                    "no DMA row of any kind: {drawn:?}"
                );
            }
        }

        /// embassy-nrf's TWIM has no timeout and one init form, and says so.
        #[test]
        fn an_async_nrf_i2c_has_no_timeout_and_no_init_choice() {
            let (drawn, skipped) = i2c_labels("nrf52833", true);
            for gone in ["Timeout", "Async init"] {
                assert!(!drawn.iter().any(|l| l == gone), "{gone}: {drawn:?}");
                assert!(skipped.iter().any(|l| l == gone), "{gone}: {skipped:?}");
            }
        }

        /// The SPIM likewise, while bit order stays: embassy-nrf's `Config`
        /// carries it and the backend writes it.
        #[test]
        fn an_async_nrf_spi_has_no_init_choice_but_keeps_bit_order() {
            let out = drive(
                ModuleKind::GenericInterfaceSpi,
                ModuleKind::GenericInterfaceSpi.default_config(1),
                "nrf52833",
                true,
                false,
                false,
            );
            let drawn: Vec<&str> = out
                .fields()
                .expect("the SPI arm marks itself documented")
                .iter()
                .map(|f| f.label.as_str())
                .collect();
            assert!(!drawn.contains(&"Async init"), "{drawn:?}");
            assert!(skipped_of(&out).contains(&"Async init"));
            assert!(drawn.contains(&"Bit order"), "{drawn:?}");
        }

        /// A SPIM and a TWIM on one nRF block: each of the two says so, in the
        /// runtime's own terms, and the sentence reaches the details pane as
        /// a note. Without a partner there is nothing to say.
        #[test]
        fn a_shared_nrf_block_is_called_out_on_both_modules() {
            // The grid line: short enough for the config column (about 70
            // characters at this size before the panel's edge clips it), and
            // naming the bus that loses on this runtime.
            for (me, partner) in [("SPIM0", "TWIM0"), ("TWIM1", "SPIM1")] {
                let n = &me[4..];
                for is_async in [true, false] {
                    let (short, full) = super::nrf_shared_block_text(
                        me,
                        partner,
                        n.parse().expect("a digit"),
                        is_async,
                    );
                    assert!(short.len() <= 60, "{} chars: {short}", short.len());
                    assert!(
                        short.starts_with(&format!("Shares TWISPI{n} with {partner}: ")),
                        "{short}"
                    );
                    let lost = if is_async {
                        format!("TWIM{n} is not built")
                    } else {
                        format!("SPIM{n} is switched off")
                    };
                    assert!(short.ends_with(&lost), "{short}");
                    assert!(full.len() > short.len() && full.starts_with(me), "{full}");
                }
            }

            let spi = drive_beside(
                ModuleKind::GenericInterfaceSpi,
                ModuleKind::GenericInterfaceSpi.default_config(0),
                "nrf52833",
                true,
                false,
                false,
                Some("TWIM0"),
            );
            let note = spi
                .notes()
                .iter()
                .find(|n| n.starts_with("SPIM0 and TWIM0 are one hardware block"))
                .expect("the SPI names the block it shares");
            assert!(note.contains("TWISPI0"), "{note}");
            assert!(
                note.contains("builds SPIM0 and leaves TWIM0 a comment"),
                "{note}"
            );

            let i2c = drive_beside(
                ModuleKind::GenericInterfaceI2c,
                ModuleKind::GenericInterfaceI2c.default_config(1),
                "nrf52833",
                false,
                false,
                false,
                Some("SPIM1"),
            );
            let note = i2c
                .notes()
                .iter()
                .find(|n| n.starts_with("TWIM1 and SPIM1 are one hardware block"))
                .expect("the I2C names the block it shares");
            assert!(note.contains("TWISPI1"), "{note}");
            assert!(
                note.contains("TWIM1's init, which runs second")
                    && note.contains("switches SPIM1 off"),
                "{note}"
            );

            for kind in [
                ModuleKind::GenericInterfaceSpi,
                ModuleKind::GenericInterfaceI2c,
            ] {
                let alone = drive(kind, kind.default_config(0), "nrf52833", true, false, false);
                assert!(
                    alone
                        .notes()
                        .iter()
                        .all(|n| !n.contains("one hardware block")),
                    "{:?}",
                    alone.notes()
                );
            }
        }

        /// Nine data bits is an STM32 word length. Neither the RP's PL011 nor
        /// the ESP's UART has one, and the ESP generator was rounding a 9 down
        /// to `_8` without saying so.
        #[test]
        fn nine_data_bits_is_offered_only_where_the_chip_has_it() {
            use crate::panels::mcu_module::modules::usart_data_bits;
            assert_eq!(usart_data_bits("stm32g0"), &[8, 9]);
            assert_eq!(usart_data_bits("stm32f1"), &[8, 9]);
            assert_eq!(usart_data_bits("rp2040"), &[8]);
            assert_eq!(usart_data_bits("esp32c3"), &[8]);
        }
    }

    /// The four wire settings exist on every chip and every runtime — they are
    /// the UART itself, not a HAL feature — so no cell may lose them.
    #[test]
    fn the_wire_settings_are_documented_on_every_chip() {
        for (family, is_async, is_native) in CELLS {
            let out = drive_usart(family, *is_async, *is_native, false, |_| {});
            let got = labels_of(&out);
            for want in ["Baud rate", "Data bits", "Parity", "Stop bits"] {
                assert!(got.contains(&want), "{family}: {want} missing from {got:?}");
            }
            // And nothing is documented twice, whatever the branch drew.
            let mut sorted = got.clone();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(before, sorted.len(), "{family}: duplicate label in {got:?}");
        }
    }

    /// The roster follows the ROWS, so it changes with the chip — which is the
    /// reason the collector runs inside the arm rather than beside it.
    #[test]
    fn the_roster_follows_what_the_chip_actually_shows() {
        // An ESP has direction and flow on either runtime, and its Transfers
        // row is locked because esp-hal's UART DMA is UHCI.
        let esp = drive_usart("esp32c3", false, false, false, |_| {});
        let l = labels_of(&esp);
        assert!(l.contains(&"Data direction"), "{l:?}");
        assert!(l.contains(&"Hardware flow control"), "{l:?}");
        assert!(l.contains(&"Transfers"), "{l:?}");
        assert!(!l.contains(&"Async transport"), "{l:?}");

        // A blocking STM32G0 has none of them: no F1 DMA table, no async rows.
        let g0 = drive_usart("stm32g0", false, false, false, |_| {});
        let l = labels_of(&g0);
        assert!(!l.contains(&"Data direction"), "{l:?}");
        assert!(!l.contains(&"Transfers"), "{l:?}");
        assert!(l.contains(&"Init API"), "{l:?}");

        // The Native runtime locks the Init API, and says so in its own words.
        let native = drive_usart("stm32f1", false, true, false, |_| {});
        let f = native.fields().unwrap();
        let api = f
            .iter()
            .find(|f| f.label == "Init API")
            .expect("locked row");
        assert_eq!(api.doc, docs::INIT_API_NATIVE);

        // On an ESP the same row is locked for a different reason.
        let f = esp.fields().unwrap();
        let api = f.iter().find(|f| f.label == "Init API").expect("esp row");
        assert_eq!(api.doc, docs::INIT_API_ESP);
    }

    /// `buf_len` is the field whose MEANING moves: the label and the sentence
    /// both turn on the transport, and the pane must show the pair the reader
    /// is looking at.
    #[test]
    fn the_buffer_row_changes_with_the_transport() {
        let buffered = drive_usart("stm32g0", true, false, false, |c| {
            c.mode = UsartMode::Buffered;
        });
        let f = buffered.fields().unwrap();
        let b = f
            .iter()
            .find(|f| f.label == "RX/TX buffer")
            .expect("buffered label");
        assert_eq!(b.doc, docs::USART_BUF_BUFFERED);
        assert!(f.iter().all(|f| f.label != "RX DMA buffer"));

        let dma = drive_usart("stm32g0", true, false, false, |c| {
            c.mode = UsartMode::Dma;
        });
        let f = dma.fields().unwrap();
        let b = f
            .iter()
            .find(|f| f.label == "RX DMA buffer")
            .expect("dma label");
        assert_eq!(b.doc, docs::USART_BUF_DMA);

        // A TX-only DMA link has no buffer at all, so the row is not drawn and
        // the pane must not claim it exists.
        let tx_only = drive_usart("stm32g0", true, false, false, |c| {
            c.mode = UsartMode::Dma;
            c.direction = UsartDirection::TxOnly;
        });
        let l = labels_of(&tx_only);
        assert!(!l.contains(&"RX DMA buffer"), "{l:?}");
        assert!(!l.contains(&"RX/TX buffer"), "{l:?}");
    }

    /// A chip whose USART has no swap/invert bits gets the sentence that says
    /// so, not the one describing controls it does not have.
    #[test]
    fn the_line_row_says_when_the_bits_are_absent() {
        let with = drive_usart("stm32g0", true, false, true, |_| {});
        let f = with.fields().unwrap();
        assert_eq!(
            f.iter().find(|f| f.label == "Line").unwrap().doc,
            docs::USART_LINE
        );

        let without = drive_usart("stm32g0", true, false, false, |_| {});
        let f = without.fields().unwrap();
        assert_eq!(
            f.iter().find(|f| f.label == "Line").unwrap().doc,
            docs::USART_LINE_ABSENT
        );
    }

    /// Every row this panel draws with a plain label is documented in the same
    /// arm that drew it.
    ///
    /// # Why this reads the source instead of running the panel
    ///
    /// The failure it exists to catch is someone adding a row and not adding the
    /// `out.field(...)` beside it. No amount of driving the panel finds that: the
    /// new row simply is not in the roster, and a roster the test does not know
    /// about looks exactly like a roster that is complete. The only place the
    /// omission is visible is the source.
    ///
    /// Scope, stated rather than assumed:
    ///
    /// * Only `ui.label("literal")`. In this file a ROW label is a plain string
    ///   and a value or caption is a `RichText`, so that one rule separates them.
    /// * Only inside `module_config_ui`, and per ARM: "Mode" means one thing in
    ///   the CAN arm and another in the OCTOSPI one, so documenting it once must
    ///   not excuse the other.
    /// * `ui.label(format!(...))` rows are NOT checked here. Their label changes
    ///   per wired channel, so the roster carries one generic entry instead -
    ///   which the per-kind roster tests cover.
    #[test]
    fn every_drawn_row_is_documented_by_the_arm_that_drew_it() {
        const SRC: &str = include_str!("modules.rs");

        // Labels that are not rows. Each one is here for a stated reason, so a
        // new exception has to argue for itself.
        const NOT_A_ROW: &[&str] = &[
            // A two-character separator between the two number fields of
            // LCD_CAM's "Active area" row, not a row of its own.
            "x",
        ];

        let body = {
            let a = SRC.find("pub fn module_config_ui(").expect("the panel");
            let b = SRC[a..].find("\n#[cfg(test)]").expect("its end") + a;
            &SRC[a..b]
        };

        // Split into the shared prologue (the row closures) and one chunk per
        // `ModuleConfig::` arm. Both draw rows; both must document them.
        let mut chunks: Vec<(&str, &str)> = Vec::new();
        let mut starts: Vec<usize> = body
            .match_indices("                ModuleConfig::")
            .map(|(i, _)| i)
            .collect();
        starts.push(body.len());
        chunks.push(("<shared row closures>", &body[..starts[0]]));
        for w in starts.windows(2) {
            let chunk = &body[w[0]..w[1]];
            let name = chunk
                .split_once('(')
                .map_or("?", |(h, _)| h.trim())
                .trim_start_matches("ModuleConfig::");
            chunks.push((name, chunk));
        }

        let mut missing: Vec<String> = Vec::new();
        for (arm, chunk) in chunks {
            let documented: Vec<&str> = collect_between(chunk, "out.field(", '"');
            for label in collect_between(chunk, "ui.label(\"", '"') {
                if NOT_A_ROW.contains(&label) || documented.contains(&label) {
                    continue;
                }
                missing.push(format!("{arm}: {label:?}"));
            }
        }
        assert!(
            missing.is_empty(),
            "these rows are drawn but not documented - add `out.field(<label>, docs::…)` \
             beside each, and a const in module_docs.rs:\n  {}",
            missing.join("\n  ")
        );
    }

    /// Every string that follows `needle` up to the next `close`, deduplicated.
    ///
    /// Deliberately not a parser: it only has to find `ui.label("X")` and
    /// `out.field("X"` in one hand-written file, and a real parse would be more
    /// machinery than the rule is worth.
    fn collect_between<'a>(hay: &'a str, needle: &str, close: char) -> Vec<&'a str> {
        let mut out: Vec<&str> = Vec::new();
        let mut rest = hay;
        while let Some(i) = rest.find(needle) {
            let after = &rest[i + needle.len()..];
            // `out.field(` may be followed by a newline and indentation before
            // the string, which rustfmt does whenever the line is long.
            let after = after.trim_start();
            let after = after.strip_prefix('"').unwrap_or(after);
            if let Some(j) = after.find(close) {
                let found = &after[..j];
                if !found.is_empty() && !found.contains('\n') && !out.contains(&found) {
                    out.push(found);
                }
            }
            rest = &rest[i + needle.len()..];
        }
        out
    }

    /// Every kind has a family, and the six families are all reachable.
    ///
    /// A `match` with a catch-all would silently drop a new kind into whatever
    /// arm came last; there is no catch-all, so this only has to prove the
    /// grouping is not lopsided.
    #[test]
    fn every_kind_lands_in_a_family() {
        use std::collections::BTreeSet;
        let seen: BTreeSet<BoxShape> = ModuleKind::ALL.iter().map(|k| BoxShape::of(*k)).collect();
        assert_eq!(seen.len(), 6, "all six families are used: {seen:?}");
        for kind in ModuleKind::ALL {
            // Custom is the only kind allowed in its own family.
            if BoxShape::of(kind) == BoxShape::Custom {
                assert!(kind.is_custom(), "{kind:?} is not a Custom module");
            }
        }
    }

    /// The edge facing the chip is never cut.
    ///
    /// This is the rule the whole shape vocabulary is built around:
    /// `facing_terminal` pins every auto-placed wire onto that edge, so a shape
    /// that shortened it would strand terminals in empty canvas. Checked for
    /// every family on all four sides.
    #[test]
    fn the_chip_facing_edge_stays_whole() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(170.0, 98.0));
        for shape in [
            BoxShape::Serial,
            BoxShape::Memory,
            BoxShape::Parallel,
            BoxShape::Driver,
            BoxShape::OffBoard,
            BoxShape::Custom,
        ] {
            for side in [Side::Right, Side::Left, Side::Top, Side::Bottom] {
                let poly = silhouette(rect, shape, side);
                // The two corners of the facing edge must still be corners of
                // the bounding rect - nothing has moved them inward.
                let (a, b) = match side {
                    Side::Right => (rect.left_top(), rect.left_bottom()),
                    Side::Left => (rect.right_top(), rect.right_bottom()),
                    Side::Top => (rect.left_bottom(), rect.right_bottom()),
                    Side::Bottom => (rect.left_top(), rect.right_top()),
                };
                for want in [a, b] {
                    assert!(
                        poly.iter().any(|p| (*p - want).length() < 0.01),
                        "{shape:?} on {side:?} moved the facing corner {want:?}"
                    );
                }
                // And every auto-placed terminal lands ON that outline.
                for t in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                    let anchor = a + (b - a) * t;
                    let term = facing_terminal(rect, side, anchor);
                    let on = nearest_on_outline(&poly, term);
                    assert!(
                        (term - on).length() < 0.01,
                        "{shape:?} on {side:?}: terminal {term:?} is off the outline"
                    );
                }
            }
        }
    }

    /// A silhouette never leaves its bounding rect.
    ///
    /// The rect is what `packed_rect` reserves, what the painter is grown to,
    /// and what egui hit-tests. A shape spilling past it would be clipped on one
    /// side of the canvas and unclickable on the other.
    #[test]
    fn a_silhouette_stays_inside_its_rect() {
        for (w, h) in [
            (170.0_f32, 98.0_f32),
            (170.0, 78.0),
            (170.0, 130.0),
            (40.0, 20.0),
        ] {
            let rect = egui::Rect::from_min_size(egui::pos2(-30.0, 12.0), egui::vec2(w, h));
            for shape in [
                BoxShape::Memory,
                BoxShape::Parallel,
                BoxShape::Driver,
                BoxShape::OffBoard,
            ] {
                for side in [Side::Right, Side::Left, Side::Top, Side::Bottom] {
                    for p in silhouette(rect, shape, side) {
                        assert!(
                            rect.expand(0.01).contains(p),
                            "{shape:?} on {side:?} at {w}x{h}: {p:?} escapes {rect:?}"
                        );
                    }
                }
            }
        }
    }

    /// A cut really removes area - otherwise the shape is a rectangle wearing a
    /// different name, and the canvas gains nothing.
    #[test]
    fn a_cut_family_is_not_just_a_rectangle() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(170.0, 98.0));
        for shape in [
            BoxShape::Memory,
            BoxShape::Parallel,
            BoxShape::Driver,
            BoxShape::OffBoard,
        ] {
            let poly = silhouette(rect, shape, Side::Right);
            assert!(poly.len() > 4, "{shape:?} produced a plain quad");
            // A point just inside the cut corner is outside the outline.
            let corner = rect.right_top() + egui::vec2(-4.0, 4.0);
            let on = nearest_on_outline(&poly, corner);
            assert!(
                (on - corner).length() > 0.5,
                "{shape:?}: the top-right corner was not cut"
            );
        }
        for shape in [BoxShape::Serial, BoxShape::Custom] {
            assert_eq!(silhouette(rect, shape, Side::Right).len(), 4, "{shape:?}");
        }
    }

    /// The picker offers what the PAD offers, and nothing else.
    ///
    /// This is the whole per-family answer in one function: an STM32 pad
    /// carries one channel of a given timer, so it comes back with a single
    /// entry and the row stays plain text; an ESP pad carries every LEDC
    /// channel, because the GPIO matrix routes them, so the row becomes a
    /// choice. Neither case is written down anywhere — both fall out of the
    /// pad's own function list.
    #[test]
    fn a_pad_is_offered_only_the_channels_it_can_reach() {
        let mut funcs: HashMap<usize, Vec<PinFunction>> = HashMap::new();
        // ESP-shaped pad: every LEDC channel on timer 0.
        funcs.insert(
            10,
            (0u8..6)
                .map(|channel| PinFunction::TimerPwm { timer: 0, channel })
                .collect(),
        );
        // STM32-shaped pad: one channel of TIM3, and one of another timer.
        funcs.insert(
            20,
            vec![
                PinFunction::TimerPwm {
                    timer: 3,
                    channel: 2,
                },
                PinFunction::TimerPwm {
                    timer: 4,
                    channel: 1,
                },
            ],
        );

        let none = BTreeSet::new();
        assert_eq!(
            pwm_channel_choices(0, 10, &funcs, &none),
            vec![0, 1, 2, 3, 4, 5],
            "an LEDC pad reaches every channel"
        );
        // One entry -> the caller keeps the plain label, because there is
        // nothing to choose.
        assert_eq!(pwm_channel_choices(3, 20, &funcs, &none), vec![2]);
        // A timer this pad does not serve at all.
        assert!(pwm_channel_choices(7, 20, &funcs, &none).is_empty());

        // Channels another pad already drives are withheld: two pads on one
        // channel is not something the generator can write.
        let taken: BTreeSet<u8> = [0u8, 1, 5].into_iter().collect();
        assert_eq!(pwm_channel_choices(0, 10, &funcs, &taken), vec![2, 3, 4]);
    }

    /// The Pico's PWM remarks name PADS and SLICES, never a TIM.
    ///
    /// The generic note tells the user to "assign TIM3 CH2 on the Pins canvas",
    /// which is STM32 wording for a control the Pico canvas does not have. What
    /// an RP owner can actually decide is the pad: each (slice, channel) is
    /// reachable from two GPIOs sixteen apart.
    #[test]
    fn an_rp_pwm_note_talks_about_pads_not_timers() {
        let mut funcs: HashMap<usize, Vec<PinFunction>> = HashMap::new();
        let mut names: HashMap<usize, String> = HashMap::new();
        // Slice 1: channel A on GP2 / GP18, channel B on GP3 / GP19 - the real
        // RP2040 map, where slice = (n / 2) % 8 and A is the even pad.
        for (num, gp, channel) in [(2usize, 2u8, 1u8), (18, 18, 1), (3, 3, 2), (19, 19, 2)] {
            funcs.insert(num, vec![PinFunction::TimerPwm { timer: 1, channel }]);
            names.insert(num, format!("GP{gp}"));
        }
        let conn: Vec<(&'static str, String, usize)> = vec![("CH1", "GP2".to_owned(), 2)];
        let notes = rp_pwm_notes(1, &conn, &funcs, &names);

        let all = notes.join("\n");
        assert!(!all.contains("TIM"), "no STM32 vocabulary: {all}");
        // The other pad on the SAME channel - a re-point that compiles.
        assert!(all.contains("GP18"), "names the sibling pad: {all}");
        // The slice's free half, and both pads that reach it.
        assert!(all.contains("channel B is free"), "{all}");
        assert!(all.contains("GP3") && all.contains("GP19"), "{all}");
        // And it says outright that the channel is not the choice, because this
        // is the panel where someone would look for the ESP's picker.
        assert!(all.contains("not selectable"), "{all}");
    }

    /// The panel names the same winner the emitter does.
    ///
    /// GP2 and GP18 are both slice 1 channel A, and a Custom module's pin button
    /// will hand out an already-taken function, so both can end up wired. The
    /// generator keeps the lower GPIO and writes a comment naming the loser; the
    /// panel used to congratulate BOTH pads on driving the channel, so the two
    /// halves of one change described the same pad in opposite ways.
    #[test]
    fn two_pads_on_one_channel_agree_with_what_the_generator_emits() {
        let mut funcs: HashMap<usize, Vec<PinFunction>> = HashMap::new();
        let mut names: HashMap<usize, String> = HashMap::new();
        for (num, channel) in [(2usize, 1u8), (18, 1), (3, 2), (19, 2)] {
            funcs.insert(num, vec![PinFunction::TimerPwm { timer: 1, channel }]);
            names.insert(num, format!("GP{num}"));
        }
        // Deliberately the HIGHER pad first: the winner is decided by GPIO
        // number, exactly as `pwm.sort_unstable()` decides it in the emitter,
        // not by the order the connections happen to sit in.
        let conn: Vec<(&'static str, String, usize)> =
            vec![("CH1", "GP18".to_owned(), 18), ("CH1", "GP2".to_owned(), 2)];
        let all = rp_pwm_notes(1, &conn, &funcs, &names).join("\n");

        // The winner is named as the driver. With no FREE pad left on this
        // channel there is no "and so can ..." line, so the clash line is where
        // it has to be said - and it is.
        assert!(all.contains("GP2 drives it"), "{all}");
        assert!(
            all.contains("GP18 is wired to slice 1 channel A as well"),
            "the pad that gets no code is named as such: {all}"
        );
        assert!(
            !all.contains("GP18 drives"),
            "and is never described as driving it: {all}"
        );
        // A pad that is wired-and-silent is not offered as an alternative
        // either - that line is for FREE pads.
        assert!(!all.contains("and so can GP18"), "{all}");
    }

    /// No note carries a run of spaces.
    ///
    /// A `\`-continued literal comes back joined to its own source indentation,
    /// and the details pane draws every one of those spaces. These strings are
    /// long enough to invite the continuation, so the rule is asserted.
    #[test]
    fn no_note_string_carries_a_run_of_spaces() {
        let mut funcs: HashMap<usize, Vec<PinFunction>> = HashMap::new();
        let mut names: HashMap<usize, String> = HashMap::new();
        for (num, channel) in [(2usize, 1u8), (18, 1), (3, 2)] {
            funcs.insert(num, vec![PinFunction::TimerPwm { timer: 1, channel }]);
            names.insert(num, format!("GP{num}"));
        }
        let conn: Vec<(&'static str, String, usize)> = vec![("CH1", "GP2".to_owned(), 2)];
        let mut all = rp_pwm_notes(1, &conn, &funcs, &names);
        all.push(RP_DMA_NOTE.to_owned());
        all.push(RP_I2C_NOTE.to_owned());
        for n in &all {
            assert!(
                !n.contains("  "),
                "double space in a details-pane note: {n:?}"
            );
        }
    }

    /// `CH3` is a channel; `CH3N` and `BKIN` are not.
    ///
    /// The complementary pad is welded to its channel on every part that has
    /// one, so offering to move it would be offering something the chip cannot
    /// do.
    #[test]
    fn only_a_plain_channel_row_becomes_a_picker() {
        assert_eq!(pwm_plain_channel("CH0"), Some(0));
        assert_eq!(pwm_plain_channel("CH7"), Some(7));
        assert_eq!(pwm_plain_channel("CH3N"), None);
        assert_eq!(pwm_plain_channel("BKIN"), None);
        assert_eq!(pwm_plain_channel("TX"), None);
    }

    /// A chip whose pins can serve `channels` of `timer`, one pad each, plus a
    /// decoy pad on another timer.
    fn chip(timer: u8, channels: &[u8]) -> HashMap<usize, Vec<PinFunction>> {
        let mut out: HashMap<usize, Vec<PinFunction>> = HashMap::new();
        for (i, ch) in channels.iter().enumerate() {
            out.insert(
                i,
                vec![PinFunction::TimerPwm {
                    timer,
                    channel: *ch,
                }],
            );
        }
        out.insert(
            99,
            vec![PinFunction::TimerPwm {
                timer: timer + 1,
                channel: 4,
            }],
        );
        out
    }

    /// A chip whose pins can serve the complementary pads `comp` of `timer`.
    fn chip_with_comp(timer: u8, channels: &[u8], comp: &[u8]) -> HashMap<usize, Vec<PinFunction>> {
        let mut out = chip(timer, channels);
        for (i, ch) in comp.iter().enumerate() {
            out.insert(
                1000 + i,
                vec![PinFunction::TimerPwmN {
                    timer,
                    channel: *ch,
                }],
            );
        }
        out
    }

    fn wired(labels: &[&str]) -> BTreeSet<String> {
        labels.iter().map(|s| (*s).to_owned()).collect()
    }

    /// The hint names the pads actually left on THIS timer.
    #[test]
    fn free_channels_are_what_the_chip_has_minus_what_is_wired() {
        let four = chip(2, &[1, 2, 3, 4]);
        assert_eq!(
            free_pwm_channels(2, &wired(&["CH1"]), &four),
            ["CH2", "CH3", "CH4"],
            "the common case the old fixed text got right"
        );
        // …and the one it got wrong: CH1 freed, CH2 taken.
        assert_eq!(
            free_pwm_channels(2, &wired(&["CH2"]), &four),
            ["CH1", "CH3", "CH4"],
            "CH1 is free and must be offered"
        );
    }

    /// Nothing left to offer — the row disappears instead of advertising an
    /// action that cannot be taken.
    #[test]
    fn a_fully_wired_timer_has_no_free_channel() {
        let four = chip(2, &[1, 2, 3, 4]);
        assert!(free_pwm_channels(2, &wired(&["CH1", "CH2", "CH3", "CH4"]), &four).is_empty());
    }

    /// Not every timer has four channels, and not every package bonds the pads
    /// it does have. TIM14 offers CH1 and nothing else.
    #[test]
    fn a_one_channel_timer_never_offers_channels_it_lacks() {
        let tim14 = chip(14, &[1]);
        assert!(
            free_pwm_channels(14, &wired(&["CH1"]), &tim14).is_empty(),
            "CH2/3/4 do not exist on TIM14"
        );
        assert_eq!(free_pwm_channels(14, &BTreeSet::new(), &tim14), ["CH1"]);
    }

    /// Another timer's pads are not this module's business.
    #[test]
    fn other_timers_do_not_leak_into_the_hint() {
        let tim2 = chip(2, &[1]);
        assert_eq!(free_pwm_channels(2, &BTreeSet::new(), &tim2), ["CH1"]);
        assert_eq!(free_pwm_channels(3, &BTreeSet::new(), &tim2), ["CH4"]);
    }

    /// A complementary pad is one more thing to assign on the canvas, so the
    /// hint offers it — on an advanced timer, where embassy can drive it.
    #[test]
    fn complementary_pads_are_offered_on_an_advanced_timer() {
        let tim1 = chip_with_comp(1, &[1, 2], &[1, 2]);
        assert_eq!(
            free_pwm_channels(1, &wired(&["CH1"]), &tim1),
            ["CH1N", "CH2", "CH2N"],
        );
        // Already wired ones drop out, exactly like the plain channels.
        assert_eq!(
            free_pwm_channels(1, &wired(&["CH1", "CH1N", "CH2", "CH2N"]), &tim1),
            Vec::<String>::new(),
        );
    }

    /// …and never on a timer whose complementary pads embassy cannot drive:
    /// TIM15/16/17 have a CH1N pad, but `ComplementaryPwm` covers TIM1/8/20.
    #[test]
    fn complementary_pads_are_not_offered_where_embassy_cannot_drive_them() {
        let tim16 = chip_with_comp(16, &[1], &[1]);
        assert_eq!(
            free_pwm_channels(16, &BTreeSet::new(), &tim16),
            ["CH1"],
            "CH1N exists on the chip but has no driver here"
        );
        assert!(is_advanced_timer(1) && is_advanced_timer(8) && is_advanced_timer(20));
        assert!(!is_advanced_timer(16) && !is_advanced_timer(2));
    }
}

#[cfg(test)]
mod the_baud_row_reports_the_wire {
    use super::baud_row;
    use crate::panels::mcu_module::uart_baud::{Divider, Plan, Source};
    use eframe::egui;

    fn painted(baud: u32, plan: &Plan) -> Vec<String> {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            ..Default::default()
        };
        let mut baud = baud;
        let shapes =
            crate::headless::run_ui(&ctx, input, |ui| baud_row(ui, &mut baud, plan)).shapes;
        let mut out = Vec::new();
        for s in &shapes {
            walk(&s.shape, &mut out);
        }
        out
    }

    /// The row prints what the HAL will do and the range at the current clock
    /// - here the F1 left on its 8 MHz HSI, where the top preset is an assert.
    #[test]
    fn a_preset_the_clock_cannot_make_says_so_under_the_field() {
        let plan = Plan::Checked(Source {
            divider: Divider::F1 { pclk: 8_000_000 },
            clock: "PCLK2",
        });
        let texts = painted(921_600, &plan);
        assert!(texts.iter().any(|t| t == "921600"), "{texts:?}");
        assert!(texts.iter().any(|t| t == "panics at boot"), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t == "range 123 – 500 000 at PCLK2 8 MHz"),
            "{texts:?}"
        );
    }

    /// A custom rate gets the typed field AND the verdict under it.
    #[test]
    fn a_custom_rate_is_judged_too() {
        let plan = Plan::Checked(Source {
            divider: Divider::F1 { pclk: 72_000_000 },
            clock: "PCLK2",
        });
        let texts = painted(250_000, &plan);
        // The typed field: its number and suffix are painted apart.
        assert!(texts.iter().any(|t| t == "250000"), "{texts:?}");
        assert!(texts.iter().any(|t| t == " baud"), "{texts:?}");
        assert!(
            texts.iter().any(|t| t == "runs at 250 000 baud · exact"),
            "{texts:?}"
        );
    }

    /// Where the rate goes nowhere, the row says that instead of a range.
    #[test]
    fn a_runtime_without_a_driver_says_the_rate_is_unused() {
        let texts = painted(115_200, &Plan::NotUsed("no driver"));
        assert!(
            texts.iter().any(|t| t == "not used by the generated code"),
            "{texts:?}"
        );
        assert!(!texts.iter().any(|t| t.starts_with("range")), "{texts:?}");
    }
}

#[cfg(test)]
mod the_palette_agrees_with_the_model {
    use super::auto_wiring_summary;
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::modules::ModuleKind;

    /// The label names the pads the add will really take.
    ///
    /// Two searches now answer the palette - the cheap `any_wiring` behind
    /// `can_add_module` for whether an entry is enabled, and `pick_pins` behind
    /// this label for what it will take. They must not disagree.
    #[test]
    fn the_preview_is_the_wiring_that_gets_committed() {
        for id in ["rp2040_pico", "stm32f103c8t6", "esp32c3"] {
            let mut mcu = builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu();
            let kind = ModuleKind::GenericInterfaceSpi;
            let Some(preview) = auto_wiring_summary(&mcu, kind) else {
                continue;
            };
            assert!(mcu.add_module(kind));
            let mut got: Vec<String> = mcu.modules[0]
                .connections
                .iter()
                .filter_map(|c| mcu.find_pin(c.mcu_pin))
                .map(|p| p.name.clone())
                .collect();
            got.sort();
            let mut want: Vec<String> = preview.split(" / ").map(str::to_owned).collect();
            want.sort();
            assert_eq!(got, want, "{id}: the label promised what the add did");
        }
    }

    /// An entry that is enabled has a wiring to show, and one that is not has
    /// none. The two searches must not disagree about that either.
    #[test]
    fn an_enabled_entry_always_has_a_preview() {
        for d in builtin_definitions() {
            let mut mcu = d.build_mcu();
            for kind in ModuleKind::ALL {
                if !mcu.supports_module(kind) || kind.is_custom() {
                    continue;
                }
                // ...all the way to exhaustion, where the answers get
                // interesting.
                for _ in 0..4 {
                    let can = mcu.can_add_module(kind);
                    assert_eq!(
                        can,
                        auto_wiring_summary(&mcu, kind).is_some(),
                        "{} {kind:?}: enabled iff there is a wiring",
                        d.id
                    );
                    if !can {
                        break;
                    }
                    mcu.add_module(kind);
                }
            }
        }
    }
}

#[cfg(test)]
mod wire_tests {
    use super::{BoxShape, Mcu, Side, wire_lit, wire_shapes};
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::model::PIN_HEIGHT;
    use eframe::egui;

    fn pico() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu()
    }

    /// The chip, and a pad on one of its edges.
    fn die() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(300.0, 300.0))
    }

    fn blocked_by<'a>(die: egui::Rect, boxes: &'a [egui::Rect]) -> super::super::wire::Blocked<'a> {
        super::super::wire::Blocked {
            body: die,
            boxes,
            own: 0,
        }
    }

    /// Whether `p` lies on one of `r`'s four edges.
    fn on_outline(p: egui::Pos2, r: egui::Rect) -> bool {
        let e = 0.01;
        let inx = p.x >= r.left() - e && p.x <= r.right() + e;
        let iny = p.y >= r.top() - e && p.y <= r.bottom() + e;
        (inx && ((p.y - r.top()).abs() < e || (p.y - r.bottom()).abs() < e))
            || (iny && ((p.x - r.left()).abs() < e || (p.x - r.right()).abs() < e))
    }

    /// The scene from the report: an I2C module dragged far up and to the LEFT of
    /// the chip, wired to a pad on the chip's TOP edge, with the box straddling
    /// the pad tip's own level.
    ///
    /// The wire leaves by the box's TOP edge, because the edge facing the chip
    /// would need a corner behind the pad. The DOT has to be drawn where the wire
    /// actually ends — painted from the first candidate instead, it was left
    /// sitting on the box's right edge with no wire reaching it, which is exactly
    /// what "the connection is not resolved" looked like.
    #[test]
    fn the_endpoint_dot_is_where_the_wire_really_ends() {
        let c = die();
        let r = super::super::wire::ring(c);
        let anchor = egui::pos2(250.0, c.top() - PIN_HEIGHT);
        let bx = egui::Rect::from_min_max(egui::pos2(-400.0, -120.0), egui::pos2(-100.0, 40.0));
        let boxes = [bx];

        let (pts, term) = super::wire_path_for_box(
            r,
            blocked_by(c, &boxes),
            bx,
            BoxShape::Serial,
            Side::Left,
            true,
            anchor,
            egui::vec2(0.0, -1.0),
        );

        assert_eq!(pts[0], term, "the path starts AT the terminal it reports");
        assert_eq!(*pts.last().expect("an end"), anchor);
        assert!(
            on_outline(term, bx),
            "and the terminal is on the box: {term:?}"
        );
    }

    /// The other scene from the report: a PWM module below and to the RIGHT,
    /// wired to a pad on the chip's BOTTOM edge.
    #[test]
    fn the_endpoint_dot_is_right_for_a_box_below_the_chip_too() {
        let c = die();
        let r = super::super::wire::ring(c);
        let anchor = egui::pos2(220.0, c.bottom() + PIN_HEIGHT);
        let bx = egui::Rect::from_min_max(egui::pos2(400.0, 330.0), egui::pos2(700.0, 460.0));
        let boxes = [bx];

        let (pts, term) = super::wire_path_for_box(
            r,
            blocked_by(c, &boxes),
            bx,
            BoxShape::Driver,
            Side::Right,
            true,
            anchor,
            egui::vec2(0.0, 1.0),
        );

        assert_eq!(pts[0], term, "the path starts AT the terminal it reports");
        assert!(on_outline(term, bx), "{term:?} is not on {bx:?}");
    }

    /// A wire may touch its own box only where it ENDS. Running the length of its
    /// own box's interior on the way there looks exactly as wrong as crossing
    /// somebody else's.
    #[test]
    fn a_wire_crosses_its_own_box_only_on_the_segment_that_ends_there() {
        let c = die();
        let r = super::super::wire::ring(c);
        for (anchor, adir, bx, face) in [
            (
                egui::pos2(250.0, c.top() - PIN_HEIGHT),
                egui::vec2(0.0, -1.0),
                egui::Rect::from_min_max(egui::pos2(-400.0, -120.0), egui::pos2(-100.0, 40.0)),
                Side::Left,
            ),
            (
                egui::pos2(220.0, c.bottom() + PIN_HEIGHT),
                egui::vec2(0.0, 1.0),
                egui::Rect::from_min_max(egui::pos2(400.0, 330.0), egui::pos2(700.0, 460.0)),
                Side::Right,
            ),
        ] {
            let boxes = [bx];
            let (pts, _) = super::wire_path_for_box(
                r,
                blocked_by(c, &boxes),
                bx,
                BoxShape::Serial,
                face,
                true,
                anchor,
                adir,
            );
            // `pts` is terminal-first, so the FIRST window is the one entitled to
            // touch the box.
            for (k, w) in pts.windows(2).enumerate().skip(1) {
                assert!(
                    !crate::panels::structure_map::layout::seg_hits_rect(
                        (w[0].x, w[0].y),
                        (w[1].x, w[1].y),
                        bx.left(),
                        bx.top(),
                        bx.width(),
                        bx.height(),
                    ),
                    "segment {k} of {pts:?} runs through its own box {bx:?}"
                );
            }
        }
    }

    /// The terminal on a box edge OTHER than the one facing the chip — the
    /// second way out that lets a wire leave towards its pad's own side.
    #[test]
    fn a_terminal_can_be_placed_on_any_edge_of_the_box() {
        let r = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(170.0, 98.0));
        let far = egui::pos2(1000.0, -1000.0);
        // Up: on the top edge, lined up with the pad but never in the corner.
        let t = super::edge_terminal(r, egui::vec2(0.0, -1.0), far);
        assert_eq!(t.y, r.top());
        assert!(
            t.x < r.right() && t.x > r.left(),
            "clear of the corners: {t:?}"
        );
        // Right: on the right edge.
        let t = super::edge_terminal(r, egui::vec2(1.0, 0.0), far);
        assert_eq!(t.x, r.right());
        assert!(t.y > r.top() && t.y < r.bottom());
        // Down and left, for completeness — an edge each.
        assert_eq!(
            super::edge_terminal(r, egui::vec2(0.0, 1.0), far).y,
            r.bottom()
        );
        assert_eq!(
            super::edge_terminal(r, egui::vec2(-1.0, 0.0), far).x,
            r.left()
        );
    }

    /// A device drag computes each part's offset arithmetically, so it can land
    /// exactly on `(0, 0)` — the sentinel that means "let the packer place this".
    /// A box that hit it would jump back to its packed slot while the rest of the
    /// device kept moving.
    #[test]
    fn a_dragged_offset_never_lands_on_the_auto_sentinel() {
        assert_ne!(super::nudge(egui::Vec2::ZERO), (0.0, 0.0));
        // …and every other offset is passed through untouched.
        assert_eq!(super::nudge(egui::vec2(3.0, -4.0)), (3.0, -4.0));
        assert_eq!(super::nudge(egui::vec2(0.0, 1.0)), (0.0, 1.0));
    }

    /// One box can wire two devices' pads. Each WIRE belongs to the device of
    /// the pad it ends on — keyed on the box instead, such a box would light
    /// both its wires or neither.
    #[test]
    fn a_wire_is_lit_by_the_pad_it_ends_on_not_by_its_box() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.join_group(8, "display");
        assert_eq!(wire_lit(Some("radar"), &mcu, 7), Some("radar"));
        assert_eq!(
            wire_lit(Some("radar"), &mcu, 8),
            None,
            "the other device's pad"
        );
        assert_eq!(wire_lit(None, &mcu, 7), None, "nothing is selected");
    }

    /// A padded spelling is the same device here too.
    #[test]
    fn a_wire_is_lit_through_a_padded_spelling() {
        let mut mcu = pico();
        mcu.groups = vec![crate::panels::mcu_module::mcu_config::PinGroup {
            name: "radar ".into(),
            pins: [7].into_iter().collect(),
        }];
        assert_eq!(wire_lit(Some("radar"), &mcu, 7), Some("radar"));
    }

    /// An unlit wire is ONE shape. A halo painted at full strength on every wire
    /// would recolour the whole diagram.
    #[test]
    fn an_unlit_wire_emits_exactly_one_shape() {
        let path = [egui::pos2(0.0, 0.0), egui::pos2(10.0, 0.0)];
        let (halo, _) = wire_shapes(&path, egui::Color32::RED, 1.6, None);
        assert!(halo.is_none());
        let (halo, _) = wire_shapes(&path, egui::Color32::RED, 1.6, Some("radar"));
        assert!(halo.is_some(), "and a lit one gains exactly one more");
    }

    /// The halo goes UNDER, wider, in the DEVICE's colour; the wire keeps its
    /// SIGNAL colour, which is what says which line it is.
    #[test]
    fn a_lit_wire_keeps_its_signal_colour_and_gains_a_wider_halo() {
        let path = [egui::pos2(0.0, 0.0), egui::pos2(10.0, 0.0)];
        let (halo, wire) = wire_shapes(&path, egui::Color32::RED, 1.6, Some("radar"));
        let hw = match halo.expect("a halo") {
            egui::Shape::Path(p) => p.stroke.width,
            _ => panic!("a path"),
        };
        let (ww, wc) = match wire {
            egui::Shape::Path(p) => (p.stroke.width, p.stroke.color),
            _ => panic!("a path"),
        };
        assert!(hw > ww, "the halo is wider: {hw} vs {ww}");
        assert_eq!(
            wc,
            egui::epaint::ColorMode::Solid(egui::Color32::RED),
            "the wire keeps its signal colour"
        );
    }
}

#[cfg(test)]
mod the_move_picker_respects_the_silicon {
    use super::free_pads_for;
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::modules::ModuleKind;
    use crate::panels::mcu_module::pins::PinFunction;
    use std::collections::HashMap;

    /// On an STM32F1 a bus pad may NOT be re-pointed on its own.
    ///
    /// One AFIO bit remaps a peripheral's whole set: SPI1 is PA5/PA6/PA7 or
    /// PB3/PB4/PB5, I2C1 is PB6/PB7 or PB8/PB9, and `stm32f1xx-hal` has a
    /// `Pins` impl for neither mixture. Offering PB3 for the SCK of a bus whose
    /// MISO/MOSI are PA6/PA7 produced a project that does not compile - the
    /// exact failure `auto_assign_partners` was rewritten to stop producing,
    /// arriving through a new door.
    #[test]
    fn an_f1_bus_pad_is_not_offered_a_partner_of_the_other_group() {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        for kind in [
            ModuleKind::GenericInterfaceSpi,
            ModuleKind::GenericInterfaceI2c,
        ] {
            let mut fresh = mcu.clone();
            assert!(fresh.add_module(kind));
            let pin_funcs: HashMap<usize, Vec<PinFunction>> = fresh
                .iter_all_pins()
                .filter(|p| !p.reserved)
                .map(|p| (p.number, p.available_functions.clone()))
                .collect();
            let current: HashMap<usize, PinFunction> = fresh
                .iter_all_pins()
                .map(|p| (p.number, p.selected_function.clone()))
                .collect();
            let pads = fresh.modules[0].connections.len();
            for c in &fresh.modules[0].connections {
                let want = current.get(&c.mcu_pin).cloned().expect("wired");
                let offered =
                    free_pads_for(&want, c.mcu_pin, &pin_funcs, &current, "stm32f1", pads);
                assert!(
                    offered.is_empty(),
                    "{kind:?}: {want:?} must not be movable on its own, got {offered:?}"
                );
            }
        }
        // ...and the guard is about the FAMILY, not about this being hard: the
        // same bus on a Pico moves freely.
        mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        let pin_funcs: HashMap<usize, Vec<PinFunction>> = mcu
            .iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| (p.number, p.available_functions.clone()))
            .collect();
        let current: HashMap<usize, PinFunction> = mcu
            .iter_all_pins()
            .map(|p| (p.number, p.selected_function.clone()))
            .collect();
        let pads = mcu.modules[0].connections.len();
        let c = &mcu.modules[0].connections[0];
        let want = current.get(&c.mcu_pin).cloned().expect("wired");
        assert!(
            !free_pads_for(&want, c.mcu_pin, &pin_funcs, &current, "rp2040", pads).is_empty(),
            "an RP pad moves freely - no remap group there"
        );
    }

    /// A single-pad signal has no group to break, so the F1 guard leaves it be.
    #[test]
    fn an_f1_single_pad_signal_still_moves() {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceTimer));
        assert_eq!(mcu.modules[0].connections.len(), 1, "one channel wired");
        let pin_funcs: HashMap<usize, Vec<PinFunction>> = mcu
            .iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| (p.number, p.available_functions.clone()))
            .collect();
        let current: HashMap<usize, PinFunction> = mcu
            .iter_all_pins()
            .map(|p| (p.number, p.selected_function.clone()))
            .collect();
        let c = &mcu.modules[0].connections[0];
        let want = current.get(&c.mcu_pin).cloned().expect("wired");
        // Not asserted non-empty (the chip may offer only one pad for it) -
        // asserted only that the GROUP guard did not fire.
        assert!(!super::f1_moves_as_a_group("stm32f1", 1));
        let _ = free_pads_for(&want, c.mcu_pin, &pin_funcs, &current, "stm32f1", 1);
    }
}

#[cfg(test)]
mod the_box_shows_one_caption {
    use super::{BOX_H, CUSTOM_ROW_H, box_h, custom_pin_row, handle_caption_pos};
    use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, VirtualModule};
    use eframe::egui;

    fn module(kind: ModuleKind, pins: Vec<usize>) -> VirtualModule {
        let mut config = kind.default_config(1);
        if let ModuleConfig::Custom(c) = &mut config {
            c.pins = pins;
        }
        VirtualModule {
            id: "m1".to_owned(),
            kind,
            name: kind.short().to_owned(),
            pos: (0.0, 0.0),
            config,
            connections: Vec::new(),
        }
    }

    fn rect_for(m: &VirtualModule) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, box_h(m)))
    }

    /// The caption is centred on the box, like the title and the summary above
    /// it - the box reads as one column now that the name row is gone.
    #[test]
    fn the_caption_is_centred_on_every_kind() {
        for kind in ModuleKind::ALL {
            let m = module(kind, vec![1, 2]);
            let rect = rect_for(&m);
            let pos = handle_caption_pos(&m, rect);
            assert!(
                (pos.x - rect.center().x).abs() < f32::EPSILON,
                "{kind:?}: centred horizontally"
            );
            assert!(
                rect.y_range().contains(pos.y),
                "{kind:?}: inside the box, got {} in {:?}",
                pos.y,
                rect.y_range()
            );
        }
    }

    /// On an ordinary module it sits under the summary, in the space the name
    /// row used to take.
    #[test]
    fn an_ordinary_module_puts_it_under_the_summary() {
        let m = module(ModuleKind::GenericInterfaceUsart, Vec::new());
        let rect = rect_for(&m);
        let y = handle_caption_pos(&m, rect).y;
        // The summary is painted at `center_top + 30`.
        assert!(y > rect.top() + 30.0, "below the summary: {y}");
        assert!(
            y < rect.top() + BOX_H * 0.75,
            "and not down at the floor: {y}"
        );
    }

    /// A Custom module is the exception: its pin rows own the middle of the
    /// box, so the caption stays at the bottom. The same offset would print it
    /// straight through the first row.
    #[test]
    fn a_custom_module_keeps_it_below_the_pin_rows() {
        for n in 1..4usize {
            let m = module(ModuleKind::Custom, (1..=n).collect());
            let rect = rect_for(&m);
            let y = handle_caption_pos(&m, rect).y;
            let last_row = custom_pin_row(rect, n - 1);
            assert!(
                y > last_row.bottom(),
                "{n} pins: caption at {y} must clear the last row at {}",
                last_row.bottom()
            );
            assert!(y < rect.bottom(), "{n} pins: still inside the box");
        }
        // ...and the rows really do grow the box, so the case is not vacuous.
        let one = module(ModuleKind::Custom, vec![1]);
        let three = module(ModuleKind::Custom, vec![1, 2, 3]);
        assert!((box_h(&three) - box_h(&one) - 2.0 * CUSTOM_ROW_H).abs() < f32::EPSILON);
    }
}

#[cfg(test)]
mod the_signal_legends {
    use super::{
        BOX_LEGEND_SIZE, BOX_W, BoxShape, LEGEND_ACCENT, LEGEND_GREY, LEGEND_GREY_OFF,
        LEGEND_SIGNAL_STROKE, LEGEND_STROKE, PWM_LEGEND_SIZE, Side, box_legend_rect, docs,
        legend_of, pwm_legend_rect, silhouette,
    };
    use crate::panels::mcu_module::modules::ModuleKind;
    use eframe::egui;

    /// The kinds that get a picture. Ten others deliberately do not - see
    /// `legend_of` for the three reasons.
    const WITH: [ModuleKind; 23] = [
        ModuleKind::GenericInterfaceQspi,
        ModuleKind::GenericInterfaceOspi,
        ModuleKind::GenericInterfaceXspi,
        ModuleKind::GenericInterfaceHspi,
        ModuleKind::GenericInterfaceSdmmc,
        ModuleKind::GenericInterfaceParlIo,
        ModuleKind::GenericInterfaceParlIoRx,
        ModuleKind::GenericInterfaceLcdCam,
        ModuleKind::GenericInterfaceCamera,
        ModuleKind::GenericInterfaceRmt,
        ModuleKind::GenericInterfaceUsart,
        ModuleKind::GenericInterfaceLpuart,
        ModuleKind::GenericInterfaceCan,
        ModuleKind::GenericInterfaceTimer,
        ModuleKind::GenericInterfaceUsb,
        ModuleKind::GenericInterfaceTouch,
        ModuleKind::GenericInterfaceDac,
        ModuleKind::GenericInterfacePcnt,
        ModuleKind::GenericInterfaceSpi,
        ModuleKind::GenericInterfaceI2c,
        ModuleKind::GenericInterfaceI2s,
        ModuleKind::GenericInterfaceSai,
        ModuleKind::GenericInterfaceMcpwm,
    ];

    /// The pictures that are pure logic levels.
    ///
    /// Touch and DAC are exempt because they are not levels at all. The nine
    /// bus kinds are exempt for a different reason: their CLOCK row is square,
    /// but the bus row is the hexagon notation, whose crossings are diagonal on
    /// purpose - that is what says "a value, not a level".
    const SQUARE: [ModuleKind; 12] = [
        ModuleKind::GenericInterfaceRmt,
        ModuleKind::GenericInterfaceUsart,
        ModuleKind::GenericInterfaceLpuart,
        ModuleKind::GenericInterfaceCan,
        ModuleKind::GenericInterfaceTimer,
        ModuleKind::GenericInterfaceUsb,
        ModuleKind::GenericInterfacePcnt,
        ModuleKind::GenericInterfaceSpi,
        ModuleKind::GenericInterfaceI2c,
        ModuleKind::GenericInterfaceI2s,
        ModuleKind::GenericInterfaceSai,
        ModuleKind::GenericInterfaceMcpwm,
    ];

    /// The pictures built from two rows.
    const TWO_ROW: [ModuleKind; 10] = [
        ModuleKind::GenericInterfaceQspi,
        ModuleKind::GenericInterfaceParlIo,
        ModuleKind::GenericInterfaceLcdCam,
        ModuleKind::GenericInterfaceUsb,
        ModuleKind::GenericInterfacePcnt,
        ModuleKind::GenericInterfaceSpi,
        ModuleKind::GenericInterfaceI2c,
        ModuleKind::GenericInterfaceI2s,
        ModuleKind::GenericInterfaceSai,
        ModuleKind::GenericInterfaceMcpwm,
    ];

    /// A legend never has half a line clipped off its own strip.
    ///
    /// The picture is drawn ON its rect's edges - a square wave's low level sits
    /// exactly at the bottom, its high level exactly at the top, and that is
    /// what makes the two read as levels. egui centres a stroke on its path, so
    /// each of those lines puts half its width OUTSIDE the rect. When the strip
    /// and the rect were the same box and the painter was clipped to the strip,
    /// that half was cut: the USB legend lost the bottom of both its rows, which
    /// is what the bug report was about.
    #[test]
    fn the_legend_strip_has_room_for_the_stroke() {
        // The strip `signal_legend` allocates, at a plausible card width.
        let strip = egui::Rect::from_min_size(
            egui::pos2(12.0, 40.0),
            egui::vec2(300.0, PWM_LEGEND_SIZE.y + LEGEND_STROKE),
        );
        let r = pwm_legend_rect(strip);
        let half = LEGEND_STROKE / 2.0;
        assert!(
            r.top() - strip.top() >= half - 0.001,
            "only {} above the picture, need {half}",
            r.top() - strip.top()
        );
        assert!(
            strip.bottom() - r.bottom() >= half - 0.001,
            "only {} below the picture, need {half}",
            strip.bottom() - r.bottom()
        );
        // The picture itself keeps its full size - the room came from the strip.
        assert_eq!(r.size(), PWM_LEGEND_SIZE);

        // And every point of every legend, at that rect, is inside the strip
        // once its stroke is accounted for.
        for kind in ModuleKind::ALL {
            let Some(l) = legend_of(kind, r) else {
                continue;
            };
            for row in l.signal.iter().chain(l.accent.iter()) {
                for p in row {
                    assert!(
                        strip.contains(egui::pos2(p.x, p.y - half))
                            && strip.contains(egui::pos2(p.x, p.y + half)),
                        "{kind:?}: {p:?} is drawn half outside the clip"
                    );
                }
            }
        }
    }

    fn sizes() -> [egui::Rect; 2] {
        [
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), PWM_LEGEND_SIZE),
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), BOX_LEGEND_SIZE),
        ]
    }

    /// Every kind but one has something true to draw.
    ///
    /// Custom is the only refusal left, and it is the only one that was never
    /// arguable: a user-made module has no protocol to legend - it is whatever
    /// pins were put on it.
    ///
    /// The nine bus kinds were refused once, on the grounds that a picture
    /// cannot show a lane count or a direction. Both are true and neither is a
    /// reason for nothing: the bus notation says "several lines carrying a
    /// value" WITHOUT claiming a number, which is exactly the case it exists
    /// for, and sharing one drawing between two directions is the same trade
    /// already made for LPUART and SAI.
    #[test]
    fn only_a_custom_module_has_nothing_to_draw() {
        for kind in ModuleKind::ALL {
            let has = legend_of(kind, sizes()[0]).is_some();
            assert_eq!(has, WITH.contains(&kind), "{kind:?}");
            assert_eq!(has, !kind.is_custom(), "{kind:?}: only Custom goes without");
        }
    }

    /// Nothing escapes the rect it was given, at either size.
    #[test]
    fn every_stroke_stays_in_its_box() {
        for kind in WITH {
            for r in sizes() {
                let l = legend_of(kind, r).expect("has a legend");
                for row in l.signal.iter().chain(l.accent.iter()) {
                    assert!(row.len() >= 2, "{kind:?}: a row with nothing in it");
                    for p in row {
                        assert!(r.expand(0.51).contains(*p), "{kind:?}: {p:?} escaped {r:?}");
                    }
                }
            }
        }
    }

    /// A logic-level picture has no diagonals.
    ///
    /// Touch and DAC are exempt BY DESIGN - a capacitance trace and a smoothed
    /// DAC output are the two pictures in the set that are not logic levels,
    /// which is exactly why they cannot be mistaken for a neighbour.
    #[test]
    fn the_square_waves_are_square() {
        for kind in SQUARE {
            let r = sizes()[0];
            let l = legend_of(kind, r).expect("has a legend");
            for row in &l.signal {
                for p in row.windows(2) {
                    let (dx, dy) = ((p[1].x - p[0].x).abs(), (p[1].y - p[0].y).abs());
                    assert!(
                        dx < 0.01 || dy < 0.01,
                        "{kind:?}: diagonal {:?} -> {:?} in a square wave",
                        p[0],
                        p[1]
                    );
                }
            }
        }
    }

    /// Two-row pictures really are two rows, and the rows never touch.
    ///
    /// Joining them into one polyline would stroke a vertical connector between
    /// them - the two-row form of the sawtooth bug.
    #[test]
    fn two_row_pictures_keep_their_rows_apart() {
        for kind in TWO_ROW {
            let r = sizes()[0];
            let l = legend_of(kind, r).expect("has a legend");
            let rows: Vec<(f32, f32)> = l
                .signal
                .iter()
                .chain(l.accent.iter())
                .map(|row| {
                    let ys: Vec<f32> = row.iter().map(|p| p.y).collect();
                    (
                        ys.iter().copied().fold(f32::INFINITY, f32::min),
                        ys.iter().copied().fold(f32::NEG_INFINITY, f32::max),
                    )
                })
                .collect();
            assert!(rows.len() >= 2, "{kind:?}: more than one row");
            let top = rows.iter().filter(|(_, hi)| *hi < r.center().y).count();
            let bottom = rows.iter().filter(|(lo, _)| *lo > r.center().y).count();
            assert!(
                top > 0 && bottom > 0,
                "{kind:?}: one row above the middle and one below, got {rows:?}"
            );
        }
    }

    /// On the canvas box, every picture stays inside its module OWN silhouette
    /// - which bevels different corners per shape and per side.
    ///
    /// The old version of this fixed the Driver keystone and varied only the
    /// side. Four other silhouettes now carry legends.
    #[test]
    fn every_kind_fits_the_outline_of_its_own_box() {
        fn inside(poly: &[egui::Pos2], p: egui::Pos2) -> bool {
            let mut wind = 0i32;
            for i in 0..poly.len() {
                let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
                let cross = (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x);
                if a.y <= p.y {
                    if b.y > p.y && cross > 0.0 {
                        wind += 1;
                    }
                } else if b.y <= p.y && cross < 0.0 {
                    wind -= 1;
                }
            }
            wind != 0
        }
        for kind in WITH {
            let shape = BoxShape::of(kind);
            for h in [78.0_f32, 98.0, 130.0] {
                let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(BOX_W, h));
                for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
                    for scale in [1.0_f32, 1.15] {
                        let _ = scale;
                        // `expect`, not `continue`: a kind that quietly stopped
                        // being placed would skip every assertion below and the
                        // test would still pass - which is exactly how USART1
                        // came to have no picture at all without anything
                        // saying so.
                        let r = box_legend_rect(rect, shape)
                            .unwrap_or_else(|| panic!("{kind:?} at h={h} gets a legend"));
                        let poly = silhouette(rect, shape, side);
                        let l = legend_of(kind, r).expect("has a legend");
                        for row in l.signal.iter().chain(l.accent.iter()) {
                            for p in row {
                                assert!(
                                    inside(&poly, *p),
                                    "{kind:?} h={h} {side:?} scale={scale}: {p:?} outside the outline"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    /// Where the picture sits, per silhouette.
    ///
    /// A plain rectangle - the shape USART, SPI, I2C, I2S and SAI wear - keeps
    /// all four corners, so the picture takes the far one and leaves the three
    /// centred text rows alone. Every other silhouette bevels the corners of
    /// the edge facing away from the chip, and which two depends on the side
    /// the box sits on, so there the picture stays centred under the texts
    /// where nothing is ever cut.
    ///
    /// It used to take the TOP-right corner on every shape, and that was wrong
    /// twice over: outside the outline on any bevelled box, and colliding with
    /// the centred title for any name longer than about five characters - so
    /// `USART1`, `LPUART1` and `MCPWM0` silently had no picture while `PWM0`
    /// did.
    #[test]
    fn the_box_legend_takes_a_corner_only_where_there_is_one() {
        for h in [78.0_f32, 98.0, 130.0] {
            let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(BOX_W, h));

            let square = box_legend_rect(rect, BoxShape::Serial).expect("a rectangle has room");
            assert!(
                square.right() > rect.center().x && square.bottom() > rect.center().y,
                "h={h}: a square box puts it in the far corner, got {square:?}"
            );

            let keystone = box_legend_rect(rect, BoxShape::Driver).expect("a keystone has room");
            assert!(
                (keystone.center().x - rect.center().x).abs() < 0.01,
                "h={h}: a bevelled box keeps it centred"
            );
            assert!(
                keystone.top() > rect.top() + 50.0,
                "h={h}: and below the last text row"
            );

            for r in [square, keystone] {
                assert!(rect.contains_rect(r), "h={h}: inside the box");
            }
        }
        // A box too short for it goes without rather than overflowing.
        let squat = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(BOX_W, 60.0));
        for shape in [BoxShape::Serial, BoxShape::Driver] {
            assert!(
                box_legend_rect(squat, shape).is_none(),
                "{shape:?}: a box with no room below its texts goes without"
            );
        }
    }

    /// `is_square` and the silhouette agree about which shapes have corners.
    ///
    /// They are two statements of one fact - the `depth` match inside
    /// `silhouette` and the list in `is_square` - and the placement above trusts
    /// the second to predict the first.
    #[test]
    fn a_square_shape_is_the_one_with_four_corners() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(BOX_W, 98.0));
        for shape in [
            BoxShape::Serial,
            BoxShape::Custom,
            BoxShape::Memory,
            BoxShape::Parallel,
            BoxShape::Driver,
            BoxShape::OffBoard,
        ] {
            for side in [Side::Top, Side::Bottom, Side::Left, Side::Right] {
                let corners = silhouette(rect, shape, side).len();
                assert_eq!(
                    shape.is_square(),
                    corners == 4,
                    "{shape:?} on {side:?}: is_square says {}, the outline has {corners} points",
                    shape.is_square()
                );
            }
        }
    }

    /// Every picture has its sentence, and the kinds without a picture have
    /// none.
    #[test]
    fn each_picture_says_what_it_means() {
        for kind in ModuleKind::ALL {
            let hover = docs::legend_hover(kind);
            assert_eq!(
                !hover.is_empty(),
                WITH.contains(&kind),
                "{kind:?}: hover follows the picture"
            );
            if !hover.is_empty() {
                assert!(
                    hover.ends_with("which is in the rows below."),
                    "{kind:?}: every one repeats the same warning, because the same misreading is available for all of them"
                );
            }
        }
    }

    /// The one thing that tells SPI and I2C apart at 52 px.
    ///
    /// Both are two rows, both are clock-on-top and data-below, and neither
    /// has a detail legible at that width. The separator is the SHOULDERS:
    /// I2C's clock is flat HIGH at both ends because START and STOP happen with
    /// SCL high, while SPI's runs edge to edge. Lose that and the two become
    /// one picture - so it is asserted rather than left to a reviewer's eye.
    #[test]
    fn i2c_has_the_shoulders_that_spi_does_not() {
        let r = sizes()[0];
        let clock_of = |kind: ModuleKind| -> Vec<egui::Pos2> {
            legend_of(kind, r).expect("has a legend").signal[0].clone()
        };
        // The level a row holds at a given x, read off its polyline.
        let level_at = |row: &[egui::Pos2], x: f32| -> f32 {
            let mut y = row[0].y;
            for p in row {
                if p.x <= x + 0.01 {
                    y = p.y;
                }
            }
            y
        };
        // The WHOLE shoulder, up to and including where the burst begins.
        // Half of it missed a full-height tick sitting exactly on that
        // junction: `square_wave` opens on the baseline, so appending it to a
        // run that was already high drew an edge where the picture claims
        // there is none - and the shoulder is the one thing separating this
        // from SPI.
        let shoulder = r.width() / 6.0;
        let i2c = clock_of(ModuleKind::GenericInterfaceI2c);
        let spi = clock_of(ModuleKind::GenericInterfaceSpi);
        let rows = super::legend_rows(r, 2);
        let high = rows[0].top();

        // I2C: high at both ends, and flat there - no edge inside a shoulder.
        for x in [r.left() + shoulder, r.right() - shoulder] {
            assert!(
                (level_at(&i2c, x) - high).abs() < 0.01,
                "I2C SCL is high at {x}, which is what makes START and STOP possible"
            );
        }
        let edges_in_shoulder = i2c
            .windows(2)
            .filter(|p| (p[0].x - p[1].x).abs() < 0.01)
            // The INTERIOR of each shoulder. The junctions themselves carry
            // the burst's own first and last edge, which belong there - the
            // artefact this pair of tests guards against is a spike, and
            // `no_picture_doubles_back_on_itself` is what catches that.
            .filter(|p| {
                (p[0].x > r.left() + 0.01 && p[0].x < r.left() + shoulder - 0.01)
                    || (p[0].x > r.right() - shoulder + 0.01 && p[0].x < r.right() - 0.01)
            })
            .count();
        assert_eq!(
            edges_in_shoulder, 0,
            "I2C SCL holds its level across both shoulders"
        );

        // SPI: the clock is already toggling inside the same margins.
        let spi_edges_in_shoulder = spi
            .windows(2)
            .filter(|p| (p[0].x - p[1].x).abs() < 0.01)
            .filter(|p| {
                (p[0].x > r.left() + 0.01 && p[0].x < r.left() + shoulder - 0.01)
                    || (p[0].x > r.right() - shoulder + 0.01 && p[0].x < r.right() - 0.01)
            })
            .count();
        assert!(
            spi_edges_in_shoulder > 0,
            "SPI's clock runs edge to edge - that is the whole difference"
        );
    }

    /// MCPWM's two outputs are never high together.
    ///
    /// The dead time is the reason the peripheral is not just two PWMs: for
    /// that window both are off, and a bridge that skips it shoots through. A
    /// picture that showed them overlapping would teach the opposite.
    #[test]
    fn the_complementary_pair_is_never_on_together() {
        let r = sizes()[0];
        let l = legend_of(ModuleKind::GenericInterfaceMcpwm, r).expect("has a legend");
        let rows = super::legend_rows(r, 2);
        let level_at = |row: &[egui::Pos2], x: f32| -> f32 {
            let mut y = row[0].y;
            for p in row {
                if p.x <= x + 0.01 {
                    y = p.y;
                }
            }
            y
        };
        let mut both_off = 0;
        let steps = 200;
        for i in 0..steps {
            let x = r.left() + r.width() * (i as f32 + 0.5) / steps as f32;
            let up = (level_at(&l.signal[0], x) - rows[0].top()).abs() < 0.01;
            let down = (level_at(&l.signal[1], x) - rows[1].top()).abs() < 0.01;
            assert!(!(up && down), "both outputs high at x={x}");
            if !up && !down {
                both_off += 1;
            }
        }
        assert!(
            both_off > 0,
            "and there IS a window where neither is on - that window is the point"
        );
    }

    /// The room reserved really is the widest stroke drawn.
    ///
    /// Two constants that must stay ordered: reserve less than you draw and the
    /// clip eats half a line, which is the defect this pair was introduced to
    /// close.
    #[test]
    fn the_widest_stroke_is_the_one_reserved_for() {
        assert!(
            LEGEND_STROKE >= LEGEND_SIGNAL_STROKE,
            "the reserved width must cover every stroke a legend draws"
        );
    }

    /// The UART and CAN pictures must not read as the same thing.
    ///
    /// They are the closest pair in the set: one row each, both idle high, both
    /// opening with a low bit, and no detail legible at 52 px. The separation
    /// is carried entirely by the accent, so it has to differ in COUNT, PLACE
    /// and ORIENTATION - two vertical ticks at the extremes against one
    /// horizontal run in the middle. Any one of those three alone would be too
    /// fine to see at that size.
    ///
    /// If this pair still does not read on a real canvas, the answer is to drop
    /// the CAN picture - not to weaken the UART one, which has no neighbour.
    #[test]
    fn the_uart_and_can_accents_differ_three_ways() {
        let r = sizes()[0];
        let uart = legend_of(ModuleKind::GenericInterfaceUsart, r).expect("has a legend");
        let can = legend_of(ModuleKind::GenericInterfaceCan, r).expect("has a legend");

        let vertical = |row: &Vec<egui::Pos2>| row.iter().all(|p| (p.x - row[0].x).abs() < 0.01);
        // COUNT.
        assert_eq!(
            uart.accent.len(),
            2,
            "the UART frame is marked at both ends"
        );
        assert_eq!(can.accent.len(), 1, "CAN has one ACK slot");
        // ORIENTATION.
        assert!(
            uart.accent.iter().all(vertical),
            "the UART marks are edges, drawn as vertical ticks"
        );
        assert!(
            !vertical(&can.accent[0]),
            "the CAN mark is a BIT, drawn as a horizontal run"
        );
        // PLACE: the UART marks sit in the outer thirds, the CAN one straddles
        // the middle.
        let third = r.width() / 3.0;
        for a in &uart.accent {
            let x = a[0].x;
            assert!(
                x < r.left() + third || x > r.right() - third,
                "a UART mark at {x} is not near an end"
            );
        }
        let (lo, hi) = (can.accent[0][0].x, can.accent[0][1].x);
        assert!(
            lo > r.left() + third * 0.5 && hi < r.right() - third * 0.5,
            "the CAN mark runs {lo}..{hi}, which should be around the middle"
        );
    }

    /// LPUART draws the USART picture, exactly.
    ///
    /// The pad carries a UART frame; the peripheral's real difference has no
    /// waveform at all. Two drawings here would be a difference the wire does
    /// not have.
    #[test]
    fn lpuart_and_sai_borrow_their_siblings_picture() {
        let r = sizes()[0];
        for (a, b) in [
            (
                ModuleKind::GenericInterfaceUsart,
                ModuleKind::GenericInterfaceLpuart,
            ),
            (
                ModuleKind::GenericInterfaceI2s,
                ModuleKind::GenericInterfaceSai,
            ),
        ] {
            let (x, y) = (
                legend_of(a, r).expect("has a legend"),
                legend_of(b, r).expect("has a legend"),
            );
            assert_eq!(x.signal, y.signal, "{a:?} and {b:?} draw the same signal");
            assert_eq!(x.accent, y.accent, "{a:?} and {b:?} mark the same thing");
        }
    }

    /// RMT and PWM must not read as the same thing either.
    ///
    /// They share the Driver silhouette and both are one row of square pulses.
    /// "Unequal widths" is NOT the separator - the PWM legend varies its duty
    /// in three steps already, so anyone reading that would find both pictures
    /// alike. The separator is that an RMT train STOPS: its pulses end before
    /// the right edge and it rests, while a PWM runs to the edge and carries an
    /// average line the whole way.
    #[test]
    fn the_rmt_train_stops_and_the_pwm_one_does_not() {
        let r = sizes()[0];
        let rmt = legend_of(ModuleKind::GenericInterfaceRmt, r).expect("has a legend");
        let pwm = legend_of(ModuleKind::GenericInterfaceTimer, r).expect("has a legend");

        // Where each row last leaves the baseline.
        let last_high = |row: &Vec<egui::Pos2>| {
            row.iter()
                .filter(|p| (p.y - r.top()).abs() < 0.01)
                .map(|p| p.x)
                .fold(r.left(), f32::max)
        };
        let rmt_end = last_high(&rmt.signal[0]);
        let pwm_end = last_high(&pwm.signal[0]);
        assert!(
            rmt_end < r.left() + r.width() * 0.7,
            "the RMT train ends at {rmt_end}, well before the edge"
        );
        assert!(
            pwm_end > r.left() + r.width() * 0.85,
            "the PWM train runs to the edge, got {pwm_end}"
        );

        // And the accents differ in kind: PWM's spans the width and climbs,
        // RMT's is a flat tail confined to the end.
        let span = |row: &Vec<egui::Pos2>| {
            let xs: Vec<f32> = row.iter().map(|p| p.x).collect();
            xs.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - xs.iter().copied().fold(f32::INFINITY, f32::min)
        };
        let climbs = |row: &Vec<egui::Pos2>| {
            let ys: Vec<f32> = row.iter().map(|p| p.y).collect();
            ys.iter().copied().fold(f32::NEG_INFINITY, f32::max)
                - ys.iter().copied().fold(f32::INFINITY, f32::min)
                > 1.0
        };
        assert!(
            span(&pwm.accent[0]) > r.width() * 0.9,
            "the PWM average spans it"
        );
        assert!(climbs(&pwm.accent[0]), "and it rises");
        assert!(
            span(&rmt.accent[0]) < r.width() * 0.5,
            "the RMT tail does not"
        );
        assert!(!climbs(&rmt.accent[0]), "and it is flat");
    }

    /// No picture doubles back on itself.
    ///
    /// A run that goes to a level and returns at the SAME x is a zero-width
    /// spike: rendered, a full-height hairline standing in the middle of a flat
    /// stretch, which on a logic drawing reads as an edge. I2C had one exactly
    /// where its shoulder meets its clock burst - the one feature that
    /// separates that picture from SPI - because `square_wave` opens on the
    /// baseline and it was appended to a run that was already high.
    ///
    /// Checked over every picture rather than that one, because the helper that
    /// produced it is shared.
    #[test]
    fn no_picture_doubles_back_on_itself() {
        for kind in WITH {
            for r in sizes() {
                let l = legend_of(kind, r).expect("has a legend");
                for row in l.signal.iter().chain(l.accent.iter()) {
                    for w in row.windows(3) {
                        let same_x =
                            (w[0].x - w[1].x).abs() < 0.01 && (w[1].x - w[2].x).abs() < 0.01;
                        let returns =
                            (w[0].y - w[2].y).abs() < 0.01 && (w[0].y - w[1].y).abs() > 0.01;
                        assert!(
                            !(same_x && returns),
                            "{kind:?}: a spike at x={} - {:?} -> {:?} -> {:?}",
                            w[0].x,
                            w[0],
                            w[1],
                            w[2]
                        );
                    }
                }
            }
        }
    }

    /// The bus notation claims no lane count.
    ///
    /// That is the whole reason it can serve ports whose width is the wiring:
    /// an envelope says "a value, several bits wide" without saying how many.
    /// The property that guarantees it is that the envelope only ever uses
    /// THREE levels - the two rails and the middle they cross at. A drawing
    /// with a level in between would be a separate lane, and would start being
    /// wrong for every module wired differently from it.
    #[test]
    fn the_bus_notation_says_a_value_not_a_width() {
        let r = sizes()[0];
        for kind in [
            ModuleKind::GenericInterfaceQspi,
            ModuleKind::GenericInterfaceParlIo,
            ModuleKind::GenericInterfaceLcdCam,
        ] {
            let l = legend_of(kind, r).expect("has a legend");
            let bus = super::legend_rows(r, 2)[1];
            let rails = [bus.top(), bus.center().y, bus.bottom()];
            let envelope: Vec<&Vec<egui::Pos2>> = l
                .signal
                .iter()
                .filter(|row| row.iter().all(|p| p.y >= bus.top() - 0.01))
                .collect();
            assert!(
                envelope.len() >= 2 && envelope.len() % 2 == 0,
                "{kind:?}: the envelope comes in mirrored pairs, got {}",
                envelope.len()
            );
            for row in &envelope {
                for p in row.iter() {
                    assert!(
                        rails.iter().any(|y| (p.y - y).abs() < 0.01),
                        "{kind:?}: {p:?} is neither a rail nor the middle - that would be a lane"
                    );
                }
                assert!(
                    row.iter().any(|p| (p.y - rails[1]).abs() < 0.01),
                    "{kind:?}: every envelope line crosses at the middle"
                );
            }
        }
    }

    /// The three bus families are told apart by what the bus DOES, not by how
    /// wide it is drawn.
    #[test]
    fn the_three_bus_families_differ_in_the_accent() {
        let r = sizes()[0];
        let rows = super::legend_rows(r, 2);
        let mem = legend_of(ModuleKind::GenericInterfaceQspi, r).expect("has a legend");
        let par = legend_of(ModuleKind::GenericInterfaceParlIo, r).expect("has a legend");
        let lcd = legend_of(ModuleKind::GenericInterfaceLcdCam, r).expect("has a legend");

        // A memory port's bus turns around: its accent is a horizontal run on
        // the bus row's mid line.
        let a = &mem.accent[0];
        assert!(
            a.iter().all(|p| (p.y - rows[1].center().y).abs() < 0.01),
            "the turnaround sits on the bus, flat: {a:?}"
        );
        assert!(
            a[1].x > a[0].x,
            "and it has width - it is a gap, not an edge"
        );

        // A parallel port latches on an edge: a vertical tick on the clock.
        let b = &par.accent[0];
        assert!(
            (b[0].x - b[1].x).abs() < 0.01,
            "the latch is an edge, drawn vertical: {b:?}"
        );

        // LCD/CAM is framed by a sync: an L, low then up.
        let c = &lcd.accent[0];
        assert_eq!(c.len(), 3, "the sync is a pulse, not a tick: {c:?}");
        assert!(c[2].y < c[0].y, "and it ends high");

        // ...and no two of the three are the same shape.
        assert_ne!(mem.accent, par.accent);
        assert_ne!(par.accent, lcd.accent);
        assert_ne!(mem.accent, lcd.accent);
    }

    /// The five memory ports share one drawing, and so do the two parallel
    /// directions - deliberately, and identically.
    #[test]
    fn the_bus_families_are_internally_identical() {
        let r = sizes()[0];
        let same = |a: ModuleKind, b: ModuleKind| {
            let (x, y) = (
                legend_of(a, r).expect("has a legend"),
                legend_of(b, r).expect("has a legend"),
            );
            assert_eq!(x.signal, y.signal, "{a:?} and {b:?} draw the same signal");
            assert_eq!(x.accent, y.accent, "{a:?} and {b:?} mark the same thing");
        };
        for k in [
            ModuleKind::GenericInterfaceOspi,
            ModuleKind::GenericInterfaceXspi,
            ModuleKind::GenericInterfaceHspi,
            ModuleKind::GenericInterfaceSdmmc,
        ] {
            same(ModuleKind::GenericInterfaceQspi, k);
        }
        same(
            ModuleKind::GenericInterfaceParlIo,
            ModuleKind::GenericInterfaceParlIoRx,
        );
        same(
            ModuleKind::GenericInterfaceLcdCam,
            ModuleKind::GenericInterfaceCamera,
        );
    }

    /// Two colours, and the same two everywhere.
    #[test]
    fn the_palette_is_exactly_two_colours() {
        assert_ne!(LEGEND_GREY, LEGEND_ACCENT);
        assert_ne!(LEGEND_GREY_OFF, LEGEND_ACCENT);
        assert_ne!(LEGEND_GREY, LEGEND_GREY_OFF);
    }
}

#[cfg(test)]
mod the_note_glyph {
    use super::*;

    const SHAPES: [BoxShape; 6] = [
        BoxShape::Serial,
        BoxShape::Memory,
        BoxShape::Parallel,
        BoxShape::Driver,
        BoxShape::OffBoard,
        BoxShape::Custom,
    ];
    const SIDES: [Side; 4] = [Side::Right, Side::Left, Side::Top, Side::Bottom];

    fn boxr() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(170.0, 98.0))
    }

    /// A title of ordinary width, where `draw_box` paints it.
    fn title(width: f32) -> egui::Rect {
        egui::Rect::from_center_size(
            boxr().center_top() + egui::vec2(0.0, 13.0),
            egui::vec2(width, 14.0),
        )
    }

    #[test]
    fn the_note_glyph_sits_right_of_the_title_inside_the_box() {
        let t = title(40.0);
        for shape in SHAPES {
            for side in SIDES {
                let g = note_glyph_rect(t, boxr(), shape, side)
                    .unwrap_or_else(|| panic!("{shape:?} on {side:?}: no room for the glyph"));
                assert!(g.left() > t.right(), "right of the title");
                assert!((g.center().y - t.center().y).abs() < 0.5, "on its line");
            }
        }
    }

    /// Past the box - and, the part a margin would get wrong, inside the
    /// rectangle but in a CUT corner.
    #[test]
    fn a_title_that_fills_the_box_gets_no_glyph() {
        assert!(note_glyph_rect(title(160.0), boxr(), BoxShape::Serial, Side::Right).is_none());

        let t = title(120.0);
        assert!(
            note_glyph_rect(t, boxr(), BoxShape::Serial, Side::Top).is_some(),
            "a square box has the room"
        );
        assert!(
            note_glyph_rect(t, boxr(), BoxShape::Driver, Side::Top).is_none(),
            "a box above the chip has both TOP corners cut, on the title row"
        );
    }

    #[test]
    fn inside_convex_is_inside() {
        let sq = [
            egui::pos2(0.0, 0.0),
            egui::pos2(10.0, 0.0),
            egui::pos2(10.0, 10.0),
            egui::pos2(0.0, 10.0),
        ];
        assert!(inside_convex(&sq, egui::pos2(5.0, 5.0)));
        assert!(
            inside_convex(&sq, egui::pos2(10.0, 5.0)),
            "on an edge counts"
        );
        assert!(!inside_convex(&sq, egui::pos2(11.0, 5.0)));
        let rev: Vec<_> = sq.iter().rev().copied().collect();
        assert!(inside_convex(&rev, egui::pos2(5.0, 5.0)), "either winding");
    }
}
