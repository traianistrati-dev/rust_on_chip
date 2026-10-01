//! MCU "Peripherals" tab — the inverse of the Pins tab.
//!
//! Lists every peripheral category the chip exposes and, under each, the pins
//! that can serve it — **one chip per pin**. A pin that can take several signals
//! of the same peripheral (e.g. an ESP32-C3 GPIO that the GPIO matrix can route
//! to SPI2 SCK/MOSI/MISO/NSS) shows a single chip with a ▾ menu to pick the
//! signal, instead of repeating the pin once per signal. This is the mirror of
//! choosing a function on a pin in the Pins tab.
//!
//! Two rules shape the layout, both about spending pins wisely:
//!
//! * Categories are split over two columns by [`Complexity`] — single-pin
//!   functions on the left (where you *spend* pins), multi-pin protocols on the
//!   right (where you *invest* them) — and every group starts collapsed, so the
//!   whole chip fits on one screen as a list of group headers.
//! * Inside a group the pins are ordered by [`pin_cost`]: a pin that can do
//!   nothing but GPIO is the cheapest one to burn on an LED, while a pin that
//!   also carries USART/SPI/I2C is a scarce resource. Cheap pins come first and
//!   are outlined; expensive ones are faded.

use crate::panels::mcu_module::Mcu;
use crate::panels::mcu_module::Pin;
use crate::panels::mcu_module::PinFunction;
use crate::panels::mcu_module::pins::logic::pin::colors::switch_role;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

/// Outline of a pin that carries no peripheral beyond GPIO — the one to spend.
const FREE_PIN: egui::Color32 = egui::Color32::from_rgb(80, 195, 120);
/// Text colour of a chip by cost tier: free, shared, scarce.
const TXT_FREE: egui::Color32 = egui::Color32::from_rgb(225, 225, 235);
const TXT_SHARED: egui::Color32 = egui::Color32::from_rgb(170, 170, 180);
const TXT_SCARCE: egui::Color32 = egui::Color32::from_rgb(118, 118, 130);
/// A pin serving this many other categories is treated as scarce.
const SCARCE_AT: usize = 3;

/// How much of the chip a peripheral ties up, and therefore which column it
/// lives in: a single-pin function you can spend freely, or a multi-pin
/// protocol whose pins have to be picked together.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Complexity {
    /// One pin, one job — GPIO, ADC, PWM, MCO.
    Simple,
    /// A protocol spread over several pins — USART, SPI, I2C, USB, CAN, SWD.
    Complex,
}

/// One row of the ordered category table.
struct CategoryDef {
    /// Owned, not `&'static str`: the peripherals derived from the chip's own
    /// signals (see [`other_categories`]) have names nobody could hardcode.
    name: String,
    /// One line saying what the peripheral IS. Shown under the group name, so
    /// a row nobody recognises still says what picking it would buy. Owned for
    /// the same reason as `name`: a derived row builds its own.
    blurb: String,
    rgb: (u8, u8, u8),
    complexity: Complexity,
    /// "Is this function mine?" Boxed for the same reason - a derived
    /// category's predicate closes over the prefix it was built from.
    pred: Box<dyn Fn(&PinFunction) -> bool>,
}

impl CategoryDef {
    /// `true` for the two plain GPIO rows. They never count towards a pin's
    /// cost: every pin does GPIO, so GPIO is never the capability given up.
    /// Derived from `pred` so adding a category can't forget to update this.
    fn is_gpio(&self) -> bool {
        (self.pred)(&PinFunction::GpioInput) || (self.pred)(&PinFunction::GpioOutput)
    }
}

/// One signal a pin can take inside a category (e.g. "SPI2  SCK").
struct PinOption {
    func: PinFunction,
    label: String,
    /// `true` when the pin currently has exactly this function selected.
    assigned: bool,
}

/// A pin and every signal it can serve within one category (deduped — the pin
/// appears once even when it supports several signals of the peripheral).
struct PinEntry {
    pin_num: usize,
    pin_name: String,
    /// How many *other* peripheral categories this pin could serve — the
    /// versatility given up by spending it here. 0 = GPIO-only, see [`pin_cost`].
    cost: usize,
    options: Vec<PinOption>,
}

impl PinEntry {
    /// The signal currently selected on this pin within the category, if any.
    fn assigned(&self) -> Option<&PinOption> {
        self.options.iter().find(|o| o.assigned)
    }
}

/// A peripheral category and the pins that can serve it on this chip.
struct CategoryView {
    name: String,
    blurb: String,
    color: egui::Color32,
    complexity: Complexity,
    pins: Vec<PinEntry>,
}

/// The peripheral a raw signal name belongs to: everything before the first
/// underscore.
///
/// `COMP1_INP` -> `COMP1`, `FMC_D0` -> `FMC`, `SAI1_SD_A` -> `SAI1`. Per
/// INSTANCE, not per family, because that is the choice being made: a chip
/// with seven comparators offers seven of them, exactly as CubeMX lists them.
/// A name with no underscore is its own peripheral (`RTC`).
fn signal_peripheral(signal: &str) -> &str {
    signal.split('_').next().unwrap_or(signal)
}

/// One derived category per peripheral found among the chip's `Other`
/// signals, with how many distinct signals it has.
///
/// The Peripherals tab is a fixed table of categories, each keyed on
/// a `PinFunction` variant. Everything the XML importer does not recognise
/// becomes `PinFunction::Other`, which no category matches - so COMP, OPAMP,
/// DAC, RTC, FMC, LTDC and DCMIPP were visible on the pins and nowhere in the
/// tab. These fill that hole without inventing an enum variant per peripheral.
///
/// **They are visible, not generatable.** Codegen still knows nothing about
/// them beyond binding the pin raw; assigning one here configures the pin and
/// nothing more.
fn other_peripherals(pins: &[&Pin]) -> Vec<(String, usize)> {
    let mut by_name: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for pin in pins {
        for f in &pin.available_functions {
            if let PinFunction::Other(sig) = f {
                by_name
                    .entry(signal_peripheral(sig).to_owned())
                    .or_default()
                    .insert(sig.clone());
            }
        }
    }
    by_name.into_iter().map(|(k, v)| (k, v.len())).collect()
}

/// What a derived peripheral is, plus the one thing that sets these rows
/// apart: the IDE lists them, it does not generate them.
///
/// Keyed on the family, not the instance — `COMP1` and `COMP7` are one entry —
/// and a peripheral nobody here has heard of still gets the second half, which
/// is the half that changes what you do next.
fn derived_blurb(name: &str) -> String {
    let family = name.trim_end_matches(|c: char| c.is_ascii_digit());
    let what = match family {
        "COMP" => "analog comparator",
        "OPAMP" => "on-chip operational amplifier",
        "VREF" | "VREFBUF" => "voltage reference for the analog blocks",
        "RTC" => "calendar and alarms, off the slow clock",
        "TAMP" => "tamper detection on the backup domain",
        "FMC" | "FSMC" => "parallel bus to SRAM, NOR or SDRAM",
        "LTDC" => "RGB TFT display controller",
        "DSIHOST" | "DSI" => "MIPI DSI display link",
        "DCMI" | "DCMIPP" => "parallel camera input",
        "PSSI" => "parallel synchronous slave interface",
        "ETH" => "Ethernet MAC (MII or RMII)",
        "FDCAN" => "CAN FD — the faster CAN",
        "SDIO" => "SD card interface, SDMMC under its old name",
        "DFSDM" | "MDF" | "ADF" => "digital filter for sigma-delta mics",
        "SPDIFRX" => "S/PDIF digital audio receiver",
        "LPTIM" => "timer that keeps running in low-power modes",
        "TIM" => "timer",
        "UCPD" => "USB Type-C power delivery",
        "CEC" => "HDMI consumer-electronics control",
        "SWPMI" => "single-wire protocol master (SIM cards)",
        "MDIOS" => "MDIO slave, for Ethernet PHY registers",
        "TSC" => "capacitive touch sensing",
        "CRS" => "clock recovery from an external reference",
        "RCC" => "clock control",
        "PWR" => "power control",
        "SYS" | "DEBUG" => "system pins: debug, boot, wake",
        "OTG_FS" | "OTG_HS" | "USB_OTG" => "USB on-the-go",
        "HDMI" => "HDMI-CEC",
        "OCTOSPIM" | "XSPIM" | "HSPIM" => "IO manager for the memory controller",
        _ => "",
    };
    // The second half is the point of these rows and is never dropped: they
    // come from the chip's own signal list, so assigning one wires the pad and
    // stops there.
    if what.is_empty() {
        "from this chip's signal list — wires the pad, no code".into()
    } else {
        format!("{what} — wires the pad, no code")
    }
}

/// A stable colour for a derived category.
///
/// Hashed from the name rather than allocated in order: the set of
/// peripherals changes with the chip, and a positional palette would repaint
/// every row whenever a different part was loaded. Kept dark and desaturated
/// so the hand-picked colours stay the loudest thing on screen -
/// these are the peripherals the IDE understands least.
fn derived_color(name: &str) -> (u8, u8, u8) {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    // Six hues, evenly spaced, at a fixed muted lightness.
    match h % 6 {
        0 => (120, 100, 150),
        1 => (100, 130, 150),
        2 => (150, 120, 95),
        3 => (110, 140, 110),
        4 => (150, 105, 125),
        _ => (105, 115, 135),
    }
}
/// Keep only what matches `query`, or everything when it is blank.
///
/// A row can match three different ways, because there are three different
/// things a person is holding when they come here: the peripheral ("usart"),
/// the signal ("mosi", "usart1 tx"), or the pin ("pa9"). Matching all three
/// with one box means never having to know which kind of question the box
/// wanted.
///
/// When the CATEGORY name matches, its pins are kept whole - "usart" means
/// "show me the USART", not "show me the pins whose label contains usart".
/// When it does not, only the pins that matched survive, so a search for a
/// pin lands on the one row that mentions it.
fn filter_categories(cats: Vec<CategoryView>, query: &str) -> Vec<CategoryView> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return cats;
    }
    // Space-separated terms, all of which must match somewhere in the row:
    // "usart 1" and "spi mosi" are the natural way to narrow a long list.
    let terms: Vec<&str> = q.split_whitespace().collect();
    cats.into_iter()
        .filter_map(|mut c| {
            let name = c.name.to_ascii_lowercase();
            let hits_name = terms.iter().all(|t| name.contains(t));
            if !hits_name {
                c.pins.retain(|p| {
                    let hay = format!(
                        "{} {} {}",
                        name,
                        p.pin_name.to_ascii_lowercase(),
                        p.options
                            .iter()
                            .map(|o| o.label.to_ascii_lowercase())
                            .collect::<Vec<_>>()
                            .join(" ")
                    );
                    terms.iter().all(|t| hay.contains(t))
                });
                if c.pins.is_empty() {
                    return None;
                }
            }
            Some(c)
        })
        .collect()
}

/// Render the Peripherals tab. Returns the `(num, name, func)` change when the
/// user assigns or clears a function, so the caller can re-sync `pins/` files.
pub fn show_peripherals_tab(
    ui: &mut egui::Ui,
    mcu_opt: &mut Option<Mcu>,
    // The search box's text, owned by the caller so it survives a repaint.
    query: &mut String,
) -> Option<(usize, String, PinFunction)> {
    let Some(mcu) = mcu_opt.as_mut() else {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("No chip selected.").color(egui::Color32::GRAY));
        });
        return None;
    };

    // Owned snapshot — no borrow of `mcu` is held across the UI pass below.
    let categories = build_categories(mcu);
    if categories.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("This chip exposes no selectable functions.")
                    .size(14.0)
                    .color(egui::Color32::GRAY),
            );
        });
        return None;
    }

    // Filtered once, before the UI pass: the count in the search row and
    // the columns below have to agree, and re-filtering per column would
    // be two chances to disagree.
    // Before the filter consumes it, and from the UNFILTERED set: whether
    // the chip has a cheap pin at all is a fact about the chip, not about
    // what the search box is showing.
    let tiering_informative = cost_tiering_is_informative(&categories);
    let filtered = filter_categories(categories, query);

    // (pin_num, func) to apply after the UI pass (Unset = clear the pin).
    let mut pending: Option<(usize, PinFunction)> = None;

    // Every group starts closed each time the app opens. egui persists the
    // open/closed state to disk, so without this a session that ended with ten
    // groups expanded would come back as a wall of chips. Only the FIRST render
    // of the tab in this run resets — reopening the tab later keeps whatever the
    // user expanded.
    static FIRST_RENDER: AtomicBool = AtomicBool::new(true);
    let collapse_all = FIRST_RENDER.swap(false, Ordering::Relaxed);

    egui::ScrollArea::vertical().show(ui, |ui| {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(
                "Every peripheral this chip can use, with the pins that can serve it. \
                 Click a pin to assign it; pins with several roles open a dropdown menu. \
                 This is the inverse of the Pins tab.",
            )
            .size(11.0)
            .color(egui::Color32::from_rgb(130, 130, 145)),
        );
        ui.add_space(3.0);
        legend(ui, tiering_informative);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("{} Find:", ph::MAGNIFYING_GLASS))
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
            ui.add(
                egui::TextEdit::singleline(query)
                    .desired_width(220.0)
                    .hint_text("usart, mosi, pa9 …"),
            );
            if !query.is_empty() && ui.small_button(ph::X).clicked() {
                query.clear();
            }
            if !query.is_empty() {
                let shown: usize = filtered.iter().map(|c| c.pins.len()).sum();
                ui.label(
                    egui::RichText::new(format!(
                        "{} peripheral{}, {shown} pin{}",
                        filtered.len(),
                        if filtered.len() == 1 { "" } else { "s" },
                        if shown == 1 { "" } else { "s" }
                    ))
                    .size(10.5)
                    .color(egui::Color32::from_rgb(130, 130, 145)),
                );
            }
        });
        ui.add_space(6.0);

        // Simple functions left, protocols right. Each column is its own
        // vertical stack, so expanding a group grows only that side instead of
        // shifting everything below it.
        ui.columns(2, |cols| {
            for (col, kind) in cols
                .iter_mut()
                .zip([Complexity::Simple, Complexity::Complex])
            {
                column_title(col, kind);
                for cat in filtered.iter().filter(|c| c.complexity == kind) {
                    category_row(col, cat, collapse_all, &mut pending);
                }
            }
        });
        ui.add_space(6.0);
    });

    if let Some((pin_num, func)) = pending {
        return mcu.apply_pin_function(pin_num, func);
    }
    None
}

/// The heading above each of the two category columns.
fn column_title(ui: &mut egui::Ui, kind: Complexity) {
    let text = match kind {
        Complexity::Simple => "SIMPLE  ·  one pin, one job",
        Complexity::Complex => "PROTOCOLS  ·  pins picked together",
    };
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .color(egui::Color32::from_rgb(120, 120, 135)),
    );
}

/// Does the cost colouring tell this chip's pins apart at all?
///
/// `SCARCE_AT` is an ABSOLUTE threshold, and it was calibrated on an STM32,
/// where a pad serves a handful of peripherals and 35% of them land above it.
/// On a part with a GPIO matrix it does not sort anything: measured over the
/// shipped definitions, every ESP and every RP board has 100% of its pins at or
/// above the line - the ESP32-C6 runs 11 to 12, the S3 11 to 13. Fading all of
/// them and advising "keep it free" offers a choice the chip does not have.
///
/// So the legend says which of the two situations the user is in, rather than
/// implying a ranking that is not there. No relative re-tiering: on a spread of
/// 11 to 12 any ordering would be noise dressed as advice.
fn cost_tiering_is_informative(categories: &[CategoryView]) -> bool {
    categories
        .iter()
        .flat_map(|c| c.pins.iter())
        .any(|p| p.cost < SCARCE_AT)
}

/// One-line key for the pin-cost colouring, so the outline and the fading are
/// readable without hovering a chip.
fn legend(ui: &mut egui::Ui, informative: bool) {
    let dim = egui::Color32::from_rgb(120, 120, 135);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        // Painted, not a glyph: a box character would come out as tofu in the
        // UI font. This is the same outline the free chips wear.
        let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
        ui.painter().rect_stroke(
            rect,
            2.0,
            egui::Stroke::new(1.0_f32, FREE_PIN),
            egui::StrokeKind::Inside,
        );
        ui.label(
            egui::RichText::new("GPIO-only pin — cheapest to spend")
                .size(10.0)
                .color(dim),
        );
        ui.label(egui::RichText::new("·").size(10.0).color(dim));
        ui.label(
            egui::RichText::new(if informative {
                format!("faded = also serves {SCARCE_AT}+ other peripherals — keep it free")
            } else {
                "every pin here serves several peripherals — this chip has no cheap ones".to_owned()
            })
            .size(10.0)
            .color(TXT_SCARCE),
        );
    });
}

/// The ordered category table. Built per call (fn-pointer predicates), then
/// shared by [`build_categories`] and [`pin_cost`] so the cost metric and the
/// grouping can never drift apart.
/// The ordered category table: the hand-written rows, then one per
/// peripheral derived from this chip's unrecognised signals.
fn category_defs(pins: &[&Pin]) -> Vec<CategoryDef> {
    use Complexity::{Complex, Simple};
    // Rule of thumb for `complexity`: it mirrors `PinFunction::is_bus()` — a
    // protocol whose pins must be chosen as a set — plus SWD, which is equally
    // multi-pin but isn't a data bus.
    let mut defs: Vec<CategoryDef> = vec![
        CategoryDef {
            name: "GPIO Output".into(),
            blurb: "drive the pin high or low".into(),
            rgb: (200, 120, 50),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::GpioOutput)),
        },
        CategoryDef {
            name: "GPIO Input".into(),
            blurb: "read the level on the pin".into(),
            rgb: (70, 160, 70),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::GpioInput)),
        },
        CategoryDef {
            name: "ADC".into(),
            blurb: "measures a voltage, gives you a number".into(),
            rgb: (150, 70, 200),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::AdcChannel { .. })),
        },
        CategoryDef {
            name: "DAC".into(),
            blurb: "takes a number, puts out a voltage".into(),
            rgb: (180, 90, 160),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::DacOut { .. })),
        },
        // An analog sense, which is why it sits with the ADC and DAC rather than
        // with the digital rows: the pad reports a CAPACITANCE count, and the
        // threshold that turns it into a press is the user's to pick.
        // Original ESP32 only - esp-hal ships a touch driver for no other part.
        CategoryDef {
            name: "TOUCH".into(),
            blurb: "a pad that senses a finger — a count, not a yes/no".into(),
            rgb: (125, 195, 80),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::TouchPad(_))),
        },
        CategoryDef {
            name: "Timers / PWM".into(),
            blurb: "a square wave: frequency, duty, dead time".into(),
            rgb: (190, 170, 30),
            complexity: Simple,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::TimerPwm { .. }
                        | PinFunction::TimerPwmN { .. }
                        | PinFunction::TimerBreak { .. }
                )
            }),
        },
        // Directly under Timers / PWM, because the pair is the whole point: that
        // row dims an LED, this one drives a half-bridge. Simple by the rule -
        // one operator output is a working instance - even though a bridge
        // usually takes A and B together.
        // Its (205, 85, 45) deviates from the canvas, which paints MCPWM in the
        // PWM orange deliberately (colors.rs). A canvas shows one pad at a time
        // and can afford the shared hue; this column shows every row at once and
        // cannot - it would be byte-identical to GPIO Output.
        CategoryDef {
            name: "MCPWM".into(),
            blurb: "PWM for switching a motor bridge, not dimming an LED".into(),
            rgb: (205, 85, 45),
            complexity: Simple,
            pred: Box::new(|f| {
                matches!(f, PinFunction::McpwmA { .. } | PinFunction::McpwmB { .. })
            }),
        },
        // One edge pad is a working counter; the control pad is what makes it an
        // encoder and the second channel is what makes it quadrature. Both are
        // choices, so the required set is one pin and the row is Simple.
        CategoryDef {
            name: "PCNT".into(),
            blurb: "counts pulses in hardware — tachometer, flow, encoder".into(),
            rgb: (70, 190, 155),
            complexity: Simple,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::PcntEdge { .. } | PinFunction::PcntCtrl { .. }
                )
            }),
        },
        // One pad per channel, and the direction is fixed in silicon per channel
        // on every part after the original ESP32 - which this row has no way to
        // show. The Virtual Modules panel is where that appears.
        CategoryDef {
            name: "RMT".into(),
            blurb: "sends or reads pulse trains — IR, WS2812, 1-Wire".into(),
            rgb: (220, 100, 115),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::RmtChannel(_))),
        },
        CategoryDef {
            name: "MCO / Clock".into(),
            blurb: "an internal clock, out on a pin".into(),
            rgb: (150, 150, 160),
            complexity: Simple,
            pred: Box::new(|f| matches!(f, PinFunction::Mco)),
        },
        CategoryDef {
            name: "USART".into(),
            blurb: "asynchronous serial — console, GPS, modem".into(),
            rgb: (50, 110, 200),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::UsartTx(_)
                        | PinFunction::UsartRx(_)
                        | PinFunction::UsartCts(_)
                        | PinFunction::UsartRts(_)
                        | PinFunction::UsartCk(_)
                )
            }),
        },
        CategoryDef {
            name: "SPI".into(),
            blurb: "fast four-wire bus, one device per chip select".into(),
            rgb: (30, 170, 170),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::SpiNss(_)
                        | PinFunction::SpiSck(_)
                        | PinFunction::SpiMiso(_)
                        | PinFunction::SpiMosi(_)
                )
            }),
        },
        CategoryDef {
            name: "I2S".into(),
            blurb: "stereo audio, carried by the SPI block".into(),
            rgb: (90, 140, 200),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::I2sCk(_)
                        | PinFunction::I2sWs(_)
                        | PinFunction::I2sSd(_)
                        | PinFunction::I2sMck(_)
                )
            }),
        },
        CategoryDef {
            name: "HSPI".into(),
            blurb: "external memory, 1 or 8 lines (high-end U5)".into(),
            rgb: (205, 185, 60),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::HspiClk { .. }
                        | PinFunction::HspiNcs { .. }
                        | PinFunction::HspiDqs { .. }
                        | PinFunction::HspiIo { .. }
                )
            }),
        },
        CategoryDef {
            name: "XSPI".into(),
            blurb: "external memory, up to 16 lines".into(),
            rgb: (195, 200, 70),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::XspiClk { .. }
                        | PinFunction::XspiNcs { .. }
                        | PinFunction::XspiDqs { .. }
                        | PinFunction::XspiIo { .. }
                )
            }),
        },
        CategoryDef {
            name: "OCTOSPI".into(),
            blurb: "external flash or RAM, up to 8 lines".into(),
            rgb: (175, 190, 80),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::OspiClk { .. }
                        | PinFunction::OspiNcs { .. }
                        | PinFunction::OspiDqs { .. }
                        | PinFunction::OspiIo { .. }
                )
            }),
        },
        CategoryDef {
            name: "QUADSPI".into(),
            blurb: "external flash on four lines".into(),
            rgb: (150, 175, 95),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::QspiClk | PinFunction::QspiNcs { .. } | PinFunction::QspiIo { .. }
                )
            }),
        },
        CategoryDef {
            name: "SDMMC".into(),
            blurb: "SD card or eMMC, 1, 4 or 8 bits wide".into(),
            rgb: (120, 155, 110),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::SdmmcCk { .. }
                        | PinFunction::SdmmcCmd { .. }
                        | PinFunction::SdmmcD { .. }
                )
            }),
        },
        // Data plus clock are both required, so the pads are picked as a set.
        // Deliberately NOT "up to 16 bits": sixteen data lines exist only on the
        // C6, and the UI offers 1/2/4/8 everywhere else.
        CategoryDef {
            name: "PARL".into(),
            blurb: "parallel port out — a whole byte per clock, not a bit".into(),
            rgb: (100, 110, 215),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::ParlData { .. } | PinFunction::ParlClk | PinFunction::ParlValid
                )
            }),
        },
        // The same peripheral by direction, so the only word that differs from
        // the row above is out/in. Two rows because the Virtual Modules panel
        // names the two halves separately, and the two lists must agree.
        // The pin canvas paints all six Parl signals one colour, so these two
        // row swatches cannot both match it. Splitting the canvas arm instead
        // would repaint pads to fix a legend, which is the wrong way round.
        CategoryDef {
            name: "PARL RX".into(),
            blurb: "parallel port in — a whole byte per clock, not a bit".into(),
            rgb: (155, 150, 230),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::ParlRxData { .. }
                        | PinFunction::ParlRxClk
                        | PinFunction::ParlRxValid
                )
            }),
        },
        // The display half of LCD_CAM, ESP32-S3 only. Named "LCD" to match
        // ModuleKind::short(), so the tab and the Virtual Modules panel say the
        // same word.
        //
        // KNOWN COLLISION, accepted: an imported STM32 with a segment LCD keeps
        // `LCD_SEG0` as an unrecognised signal, and `signal_peripheral` derives
        // the name "LCD" from it too - so the fold below merges those pads into
        // this row and labels them "i8080 or RGB", which a segment LCD is not.
        // No bundled chip is affected. Guarding the fold on "does this row match
        // anything on this chip" was tried and is WRONG: an imported part whose
        // ADC signals the importer left alone derives "ADC" with no typed
        // AdcChannel beside it, and folding there is exactly right
        // (`category_names_are_unique` covers that case). The two situations are
        // mechanically identical; only the vendors' choice of a three-letter
        // name differs.
        CategoryDef {
            name: "LCD".into(),
            blurb: "parallel bus that drives a display — i8080 or RGB".into(),
            rgb: (130, 105, 215),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::LcdCamData { .. }
                        | PinFunction::LcdCamDc
                        | PinFunction::LcdCamWr
                        | PinFunction::LcdCamCs
                        | PinFunction::LcdCamPclk
                        | PinFunction::LcdCamVsync
                        | PinFunction::LcdCamHsync
                        | PinFunction::LcdCamDe
                )
            }),
        },
        // The camera half of the same block, and a separate variant family in
        // the enum - so the two preds cannot both claim a pad.
        CategoryDef {
            name: "CAM".into(),
            blurb: "pixels in from a camera sensor, on a parallel bus".into(),
            rgb: (200, 115, 215),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::CamData { .. }
                        | PinFunction::CamPclk
                        | PinFunction::CamVsync
                        | PinFunction::CamHsync
                        | PinFunction::CamHenable
                        | PinFunction::CamMclk
                )
            }),
        },
        CategoryDef {
            name: "SAI".into(),
            blurb: "multi-channel audio: I2S, TDM, PDM, AC'97".into(),
            rgb: (60, 110, 175),
            complexity: Complex,
            pred: Box::new(|f| {
                matches!(
                    f,
                    PinFunction::SaiSck { .. }
                        | PinFunction::SaiSd { .. }
                        | PinFunction::SaiFs { .. }
                        | PinFunction::SaiMclk { .. }
                )
            }),
        },
        CategoryDef {
            name: "I2C".into(),
            blurb: "two wires, many devices, each with an address".into(),
            rgb: (60, 180, 100),
            complexity: Complex,
            pred: Box::new(|f| matches!(f, PinFunction::I2cScl(_) | PinFunction::I2cSda(_))),
        },
        CategoryDef {
            name: "USB".into(),
            blurb: "device or host, on the D+/D- pair".into(),
            rgb: (190, 50, 160),
            complexity: Complex,
            pred: Box::new(|f| matches!(f, PinFunction::UsbDm | PinFunction::UsbDp)),
        },
        CategoryDef {
            name: "CAN".into(),
            blurb: "differential bus for vehicles and industry".into(),
            rgb: (200, 130, 20),
            complexity: Complex,
            pred: Box::new(|f| matches!(f, PinFunction::CanRx | PinFunction::CanTx)),
        },
        CategoryDef {
            name: "SWD / Debug".into(),
            blurb: "flash and debug the chip — usually keep it".into(),
            rgb: (190, 50, 50),
            complexity: Complex,
            pred: Box::new(|f| matches!(f, PinFunction::SwdIo | PinFunction::SwdClk)),
        },
    ];
    // Then the peripherals only this chip knows about. Appended, so the
    // hand-written rows keep their order and their place at the top.
    for (name, signals) in other_peripherals(pins) {
        let prefix = name.clone();
        let derived: Box<dyn Fn(&PinFunction) -> bool> =
            Box::new(move |f| matches!(f, PinFunction::Other(s) if signal_peripheral(s) == prefix));

        // A leftover signal group can carry the name of a hand-written row: an
        // STM32 whose USB_OTG_* signals the importer left alone derives "USB",
        // and "USB" is already the UsbDm/UsbDp row. Two rows sharing a name
        // share their CollapsingHeader id, and egui answers that by painting an
        // "ID clash" banner across the tab. Fold the leftovers into the row that
        // already owns the name instead of adding a second one - one peripheral
        // is one row, whether or not the importer recognised each of its
        // signals.
        if let Some(row) = defs.iter_mut().find(|d| d.name == name) {
            let known = std::mem::replace(&mut row.pred, Box::new(|_| false));
            row.pred = Box::new(move |f| known(f) || derived(f));
            continue;
        }

        defs.push(CategoryDef {
            blurb: derived_blurb(&name),
            rgb: derived_color(&name),
            // A peripheral whose pins must be chosen as a set belongs on the
            // Complex side, and "more than one signal on this chip" is the only
            // evidence available for that: COMP1 has INP and INM, DAC1 has one
            // output. Derived rather than guessed, and stable for a given part.
            complexity: if signals > 1 { Complex } else { Simple },
            pred: derived,
            name,
        });
    }
    defs
}

/// How many peripheral categories *other than GPIO* this pin is able to serve.
///
/// Counted per category, not per signal: an ESP32-C3 pad the GPIO matrix can
/// route to SPI2 SCK/MOSI/MISO/NSS costs 1 (one peripheral lost), not 4. `0`
/// means the pin does nothing but GPIO — the one to burn on an LED or a button.
fn pin_cost(pin: &Pin, defs: &[CategoryDef]) -> usize {
    defs.iter()
        .filter(|d| !d.is_gpio())
        .filter(|d| pin.available_functions.iter().any(|f| (d.pred)(f)))
        .count()
}

/// Build the per-category view from the chip's pins (available functions),
/// grouping each pin's matching signals into a single [`PinEntry`].
fn build_categories(mcu: &Mcu) -> Vec<CategoryView> {
    let pins: Vec<&Pin> = mcu.iter_all_pins().filter(|p| !p.reserved).collect();

    let defs = category_defs(&pins);

    // Mirror the Pins panel's visibility rule (mcu/gui/mod.rs): a function that
    // is already selected on a *different* pin is hidden everywhere else, so an
    // exclusive signal (e.g. SPI2 SCK) can't be offered — or assigned — twice.
    // GPIO In/Out are shareable and never hidden. This keeps the Peripherals tab
    // consistent with the Pins tab (a signal taken on one pin disappears from
    // the others in both views).
    let taken_elsewhere = |pin_num: usize, f: &PinFunction| {
        !matches!(f, PinFunction::GpioInput | PinFunction::GpioOutput)
            && pins
                .iter()
                .any(|p| p.number != pin_num && &p.selected_function == f)
    };

    defs.iter()
        .filter_map(|def| {
            let (name, (r, g, b), pred) = (def.name.clone(), def.rgb, &def.pred);
            let blurb = def.blurb.clone();
            let mut pin_entries = Vec::new();
            for pin in &pins {
                // A pin already configured for a function belongs to a single
                // category: once it holds a function, it disappears from every
                // *other* category, so the same pin can never be selected twice
                // with two different functionalities. Within its own category
                // the signal menu still lets the user re-pick or unassign it.
                let configured = pin.selected_function != PinFunction::Unset;
                if configured && !pred(&pin.selected_function) {
                    continue;
                }

                let mut options = Vec::new();
                for f in &pin.available_functions {
                    // Always keep the owning pin's option (so a signal can be
                    // unassigned even if it ended up on two pins); hide it only
                    // on pins that don't currently hold it.
                    let owns = &pin.selected_function == f;
                    if pred(f) && (owns || !taken_elsewhere(pin.number, f)) {
                        options.push(PinOption {
                            func: f.clone(),
                            label: f.label(),
                            assigned: owns,
                        });
                    }
                }
                if !options.is_empty() {
                    pin_entries.push(PinEntry {
                        pin_num: pin.number,
                        pin_name: pin.name.clone(),
                        cost: pin_cost(pin, &defs),
                        options,
                    });
                }
            }
            // Cheapest pins first: the ones you can spend without losing a
            // peripheral. Pin number breaks ties, so the order is stable. A
            // board pad that switches something on goes LAST: it costs no
            // peripheral, but it is not a spare output either.
            pin_entries.sort_by_key(|p| (switch_role(&p.pin_name).is_some(), p.cost, p.pin_num));

            if pin_entries.is_empty() {
                None
            } else {
                Some(CategoryView {
                    name,
                    blurb,
                    color: egui::Color32::from_rgb(r, g, b),
                    complexity: def.complexity,
                    pins: pin_entries,
                })
            }
        })
        .collect()
}

/// The `(pin, func)` to apply when an option is clicked: assigned → clear, else
/// → assign.
fn click_action(pin_num: usize, opt: &PinOption) -> (usize, PinFunction) {
    if opt.assigned {
        (pin_num, PinFunction::Unset)
    } else {
        (pin_num, opt.func.clone())
    }
}

/// Draw one category as a collapsed-by-default section: header (swatch + name +
/// count) always visible, one chip per pin in the body.
fn category_row(
    ui: &mut egui::Ui,
    cat: &CategoryView,
    collapse: bool,
    pending: &mut Option<(usize, PinFunction)>,
) {
    let assigned = cat.pins.iter().filter(|p| p.assigned().is_some()).count();
    let total = cat.pins.len();

    ui.add_space(4.0);
    let id = ui.make_persistent_id(("periph_cat", cat.name.as_str()));
    let mut state =
        egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false);
    if collapse {
        state.set_open(false);
    }
    state
        .show_header(ui, |ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, cat.color);
            // Two lines, not two columns: the tab is already split in half, and
            // the blurb has to survive there without pushing the count off the
            // row.
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(cat.name.clone())
                            .size(13.0)
                            .strong()
                            .color(cat.color),
                    );

                    // With the group closed this badge is the only clue about
                    // what is already wired up, so it takes the category colour
                    // once something is assigned.
                    let (badge, badge_col) = if assigned > 0 {
                        (format!("{assigned}/{total} pins"), cat.color)
                    } else {
                        (
                            format!("{total} pins"),
                            egui::Color32::from_rgb(150, 150, 160),
                        )
                    };
                    ui.label(egui::RichText::new(badge).size(11.0).color(badge_col));
                });
                // Stays visible with the group closed: closed is the default,
                // and a name alone is exactly what someone who does not already
                // know the peripheral cannot read.
                ui.label(
                    egui::RichText::new(cat.blurb.clone())
                        .size(10.0)
                        .color(egui::Color32::from_gray(135)),
                );
            });
        })
        .body(|ui| {
            // ── One chip per pin; assigned ones highlighted ──
            ui.horizontal_wrapped(|ui| {
                for pe in &cat.pins {
                    pin_chip(ui, cat.color, pe, pending);
                }
            });
            ui.add_space(2.0);
        });
    ui.separator();
}

/// One pin chip. A single-signal pin is a plain toggle button; a multi-signal
/// pin opens a ▾ menu listing its signals so the pin is shown only once.
fn pin_chip(
    ui: &mut egui::Ui,
    color: egui::Color32,
    pe: &PinEntry,
    pending: &mut Option<(usize, PinFunction)>,
) {
    let assigned = pe.assigned();
    // The port name is what you configure against; the physical pin number only
    // matters when soldering, so it lives in the tooltip. That keeps the chip
    // narrow enough for two per row inside a half-width column.
    let base = pe.pin_name.clone();
    // A board pad whose function switches something on is not a free pin,
    // whatever it costs in peripherals: no green outline, and its tooltip says
    // what it switches instead of how cheap it is.
    let switch = switch_role(&pe.pin_name);

    let text_col = chip_text(pe.cost, assigned.is_some());
    let stroke = if switch.is_some() {
        egui::Stroke::NONE
    } else {
        chip_stroke(pe.cost, assigned.is_some())
    };
    let note = switch.map_or_else(|| cost_note(pe.cost), str::to_owned);

    if pe.options.len() == 1 {
        // Single role — direct toggle, with the signal shown inline.
        let opt = &pe.options[0];
        let label = format!("{base} · {}", opt.label);
        let btn = egui::Button::new(egui::RichText::new(label).size(11.0).color(text_col))
            .fill(chip_fill(color, opt.assigned))
            .stroke(stroke);
        let resp = ui.add(btn).on_hover_text(format!(
            "{} (pin {})\n{}\n{}\nclick to {}",
            pe.pin_name,
            pe.pin_num,
            opt.label,
            note,
            if opt.assigned { "unassign" } else { "assign" }
        ));
        if resp.clicked() {
            *pending = Some(click_action(pe.pin_num, opt));
        }
        return;
    }

    // Multiple roles — one chip with a ▾ menu, so the pin is listed once.
    let label = match assigned {
        Some(o) => format!("{base} · {} {}", o.label, ph::CARET_DOWN),
        None => format!("{base} {}", ph::CARET_DOWN),
    };
    let title = egui::RichText::new(label).size(11.0).color(text_col);
    let button = egui::Button::new(title)
        .fill(chip_fill(color, assigned.is_some()))
        .stroke(stroke);
    let (resp, _) = egui::containers::menu::MenuButton::from_button(button).ui(ui, |ui| {
        ui.set_min_width(150.0);
        for opt in &pe.options {
            let mark = if opt.assigned {
                format!("{} ", ph::CHECK)
            } else {
                "    ".to_string()
            };
            let col = if opt.assigned {
                color
            } else {
                egui::Color32::from_rgb(205, 205, 215)
            };
            if ui
                .add(
                    egui::Button::new(
                        egui::RichText::new(format!("{mark}{}", opt.label))
                            .size(11.0)
                            .color(col),
                    )
                    .frame(false),
                )
                .clicked()
            {
                *pending = Some(click_action(pe.pin_num, opt));
                ui.close();
            }
        }
    });
    resp.on_hover_text(format!(
        "{} (pin {})\n{}\npick a signal",
        pe.pin_name, pe.pin_num, note
    ));
}

/// Chip background: category colour when assigned, neutral otherwise.
fn chip_fill(color: egui::Color32, assigned: bool) -> egui::Color32 {
    if assigned {
        color
    } else {
        egui::Color32::from_rgb(45, 45, 52)
    }
}

/// Chip label colour by [`pin_cost`]: bright for a pin that costs nothing to
/// spend, faded for one that would take a scarce peripheral with it. An
/// assigned chip is filled with the category colour, so it stays white.
fn chip_text(cost: usize, assigned: bool) -> egui::Color32 {
    if assigned {
        egui::Color32::WHITE
    } else if cost == 0 {
        TXT_FREE
    } else if cost >= SCARCE_AT {
        TXT_SCARCE
    } else {
        TXT_SHARED
    }
}

/// Outline only the free (GPIO-only) pins, and only while unassigned — an
/// assigned chip is already a solid block of category colour.
fn chip_stroke(cost: usize, assigned: bool) -> egui::Stroke {
    if cost == 0 && !assigned {
        egui::Stroke::new(1.0_f32, FREE_PIN)
    } else {
        egui::Stroke::NONE
    }
}

/// Tooltip line explaining what spending this pin costs.
fn cost_note(cost: usize) -> String {
    match cost {
        0 => "GPIO-only pin — cheapest to spend".to_string(),
        1 => "also serves 1 other peripheral".to_string(),
        n => format!("also serves {n} other peripherals"),
    }
}

#[cfg(test)]
mod tests {

    /// The legend must not promise a choice the chip does not offer.
    ///
    /// `SCARCE_AT` is absolute and was calibrated on an STM32: 35% of an F103's
    /// pins land above it, so fading them says something. Measured over the
    /// shipped definitions, EVERY ESP part and EVERY RP board has 100% of its
    /// pins at or above the line - the C6 runs 11 to 12, the S3 11 to 13. There
    /// the old legend faded all of them and advised "keep it free", which is
    /// advice about a choice that does not exist.
    ///
    /// Deliberately NOT re-tiered relative to the chip: on a spread of 11 to 12
    /// any ranking would be noise dressed as advice. The tab says which of the
    /// two situations the user is in instead.
    #[test]
    fn the_legend_matches_what_the_chip_can_offer() {
        use crate::panels::mcu_module::builtins::builtin_definitions;

        let mut informative = Vec::new();
        for d in builtin_definitions() {
            let mcu = d.build_mcu();
            if cost_tiering_is_informative(&build_categories(&mcu)) {
                informative.push(d.id.clone());
            }
        }
        informative.sort();
        // The micro:bit joins the F103 and the W boards: its on-board nets
        // (MIC_RUN, LOGO, SENSOR_INT) are GPIO-only while every edge pad also
        // serves two UARTEs and sixteen PWM channels, so the spread is real
        // and the tiering says something. The pico2-ice joins for the same
        // reason as the W boards: its RGB LED pads (GP0/1/9) are output-only.
        assert_eq!(
            informative,
            [
                "nrf52832_dk",
                "nrf52833_microbit_v2",
                "nrf52840_dk",
                "rp2040_pico_w",
                "rp2350_pico2_ice",
                "rp2350_pico2_w",
                "stm32f103c8t6"
            ],
            "which chips have a pin below the scarcity line changed - if a chip              joined or left, the legend it shows changed with it"
        );
    }

    /// The pico2-ice's CRESET pad costs no peripheral, so by cost alone it
    /// would head the GPIO Output list as the cheapest spare pin - and one click
    /// there switches the FPGA loader on. It goes last, and says what it does.
    #[test]
    fn a_switch_pad_is_offered_last() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        let mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2350_pico2_ice")
            .expect("built-in rp2350_pico2_ice")
            .build_mcu();
        let cats = build_categories(&mcu);
        let holder = cats
            .iter()
            .find(|c| c.pins.iter().any(|p| p.pin_name.starts_with("ICE_CRESET")))
            .expect("a category offers ICE_CRESET");
        let last = holder.pins.last().expect("pins");
        assert!(last.pin_name.starts_with("ICE_CRESET"), "{}", last.pin_name);
        assert!(super::switch_role(&last.pin_name).is_some_and(|r| r.contains("FPGA loader")));
    }

    /// The Peripherals tab, for every chip that can open it.
    ///
    /// The tab is a fixed set of hard-coded categories plus whatever the chip's own
    /// signals derive. What it must never do is offer a peripheral the chip
    /// has not got — the categories come from PIN FUNCTIONS, so the check is
    /// whether those agree with what the part actually carries.
    #[test]
    fn every_bundled_chip_opens_a_truthful_peripherals_tab() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        use crate::panels::mcu_module::codegen::family;

        for d in builtin_definitions() {
            let mcu = d.build_mcu();
            let cats = build_categories(&mcu);
            let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();
            println!("{:<16} {:?}", d.id, names);

            assert!(!cats.is_empty(), "{}: the tab would be blank", d.id);
            for c in &cats {
                // An empty category is a heading over nothing. `build_categories`
                // drops them; this is what keeps that true.
                assert!(!c.pins.is_empty(), "{}: `{}` lists no pin", d.id, c.name);
                assert!(
                    !c.blurb.trim().is_empty(),
                    "{}: `{}` has no blurb",
                    d.id,
                    c.name
                );
                for p in &c.pins {
                    assert!(
                        !p.options.is_empty(),
                        "{}: `{}` pin {} has no signal",
                        d.id,
                        c.name,
                        p.pin_num
                    );
                }
            }
            let mut sorted = names.clone();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(sorted.len(), before, "{}: a category is listed twice", d.id);

            if family::is_esp(&mcu.family) {
                // Cross-checked against the pins, which came from the vendor
                // metadata: a category may appear only where the chip has the
                // signals for it. The C5 and C61 have no LEDC at all, and the
                // ESP32 and C2 no USB — so those tabs must be short.
                use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
                let has = |pred: &dyn Fn(&PinFunction) -> bool| {
                    mcu.iter_all_pins()
                        .any(|p| p.available_functions.iter().any(|f| pred(f)))
                };
                for (cat, pred) in [
                    (
                        "Timers / PWM",
                        &(|f: &PinFunction| matches!(f, PinFunction::TimerPwm { .. }))
                            as &dyn Fn(&PinFunction) -> bool,
                    ),
                    ("USB", &|f: &PinFunction| {
                        matches!(f, PinFunction::UsbDm | PinFunction::UsbDp)
                    }),
                    ("ADC", &|f: &PinFunction| {
                        matches!(f, PinFunction::AdcChannel { .. })
                    }),
                    ("CAN", &|f: &PinFunction| {
                        matches!(f, PinFunction::CanTx | PinFunction::CanRx)
                    }),
                    // The eight that were missing entirely. The assertion is
                    // bidirectional, so this catches a row offered without pins
                    // AND pins with no row - the second is how these went
                    // invisible on seven of the nine Espressif parts while the
                    // Virtual Modules panel offered them all along.
                    ("RMT", &|f: &PinFunction| {
                        matches!(f, PinFunction::RmtChannel(_))
                    }),
                    ("PCNT", &|f: &PinFunction| {
                        matches!(
                            f,
                            PinFunction::PcntEdge { .. } | PinFunction::PcntCtrl { .. }
                        )
                    }),
                    ("MCPWM", &|f: &PinFunction| {
                        matches!(f, PinFunction::McpwmA { .. } | PinFunction::McpwmB { .. })
                    }),
                    ("TOUCH", &|f: &PinFunction| {
                        matches!(f, PinFunction::TouchPad(_))
                    }),
                    ("PARL", &|f: &PinFunction| {
                        matches!(
                            f,
                            PinFunction::ParlData { .. }
                                | PinFunction::ParlClk
                                | PinFunction::ParlValid
                        )
                    }),
                    ("PARL RX", &|f: &PinFunction| {
                        matches!(
                            f,
                            PinFunction::ParlRxData { .. }
                                | PinFunction::ParlRxClk
                                | PinFunction::ParlRxValid
                        )
                    }),
                    ("LCD", &|f: &PinFunction| {
                        matches!(
                            f,
                            PinFunction::LcdCamData { .. }
                                | PinFunction::LcdCamDc
                                | PinFunction::LcdCamWr
                                | PinFunction::LcdCamCs
                                | PinFunction::LcdCamPclk
                                | PinFunction::LcdCamVsync
                                | PinFunction::LcdCamHsync
                                | PinFunction::LcdCamDe
                        )
                    }),
                    ("CAM", &|f: &PinFunction| {
                        matches!(
                            f,
                            PinFunction::CamData { .. }
                                | PinFunction::CamPclk
                                | PinFunction::CamVsync
                                | PinFunction::CamHsync
                                | PinFunction::CamHenable
                                | PinFunction::CamMclk
                        )
                    }),
                ] {
                    assert_eq!(
                        names.contains(&cat),
                        has(pred),
                        "{}: `{cat}` is offered {} the pins for it",
                        d.id,
                        if names.contains(&cat) {
                            "without"
                        } else {
                            "despite"
                        }
                    );
                }
            }
        }
    }
    use super::*;
    use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
    use crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx;

    fn category<'a>(cats: &'a [CategoryView], name: &str) -> &'a CategoryView {
        cats.iter().find(|c| c.name == name).unwrap()
    }

    /// The REVERSE of `every_bundled_chip_opens_a_truthful_peripherals_tab`.
    ///
    /// That test checks a category appears only where the chip has the signals.
    /// This one checks the other direction, which is the one that was missing:
    /// every signal a chip's pins expose must be reachable through SOME
    /// category. A typed `PinFunction` variant with no row is simply invisible
    /// on screen: the pin shows it on the Pins tab and in the Virtual Modules
    /// panel, and
    /// the Peripherals tab, which claims to list "every peripheral this chip can
    /// use", silently omits it.
    ///
    /// That is how RMT, PCNT, MCPWM, PARL, PARL RX, LCD, CAM and TOUCH went
    /// missing on seven of the nine Espressif parts. `PinFunction::Other` is
    /// exempt because `other_peripherals` already derives a row for it.
    #[test]
    fn every_signal_a_chip_exposes_is_reachable_from_the_peripherals_tab() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        use std::collections::BTreeSet;

        for d in builtin_definitions() {
            let mcu = d.build_mcu();
            let pins: Vec<&Pin> = mcu.iter_all_pins().collect();
            let defs = category_defs(&pins);
            let mut orphans: BTreeSet<String> = BTreeSet::new();
            for pin in &pins {
                for f in &pin.available_functions {
                    if matches!(f, PinFunction::Unset | PinFunction::Other(_)) {
                        continue;
                    }
                    if !defs.iter().any(|c| (c.pred)(f)) {
                        // The variant name alone: the payload is an instance
                        // number, and one missing row hides every instance.
                        let s = format!("{f:?}");
                        orphans.insert(s.split(['(', ' ', '{']).next().unwrap_or(&s).to_owned());
                    }
                }
            }
            assert!(
                orphans.is_empty(),
                "{}: the pins offer {orphans:?}, and no category row does",
                d.id
            );
        }
    }
    #[test]
    fn categories_cover_expected_peripherals() {
        let mcu = create_stm32f103c8tx();
        let cats = build_categories(&mcu);
        let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();

        for expected in ["GPIO Output", "GPIO Input", "ADC", "USART", "SPI", "I2C"] {
            assert!(names.contains(&expected), "missing category {expected}");
        }
        // ADC exposes many analog-capable pins.
        assert!(
            category(&cats, "ADC").pins.len() >= 8,
            "expected ≥8 ADC pins"
        );
        // Every option carries a non-empty label, and starts unassigned.
        for c in &cats {
            for p in &c.pins {
                assert!(!p.options.is_empty());
                for o in &p.options {
                    assert!(!o.label.is_empty());
                    assert!(!o.assigned, "nothing is configured on a fresh chip");
                }
            }
        }
    }

    #[test]
    fn assigned_flag_reflects_selection() {
        let mut mcu = create_stm32f103c8tx();
        // PA0 is pin 10 on the C8T6 and supports GPIO Output.
        mcu.apply_pin_function(10, PinFunction::GpioOutput);

        let cats = build_categories(&mcu);
        let pa0 = category(&cats, "GPIO Output")
            .pins
            .iter()
            .find(|p| p.pin_num == 10)
            .unwrap();
        assert!(
            pa0.assigned().is_some(),
            "PA0 must be assigned in GPIO Output"
        );

        // A configured pin disappears from every *other* category, so it can't
        // be selected twice with a different functionality.
        let pa0_in = category(&cats, "GPIO Input")
            .pins
            .iter()
            .find(|p| p.pin_num == 10);
        assert!(
            pa0_in.is_none(),
            "PA0 is configured as GPIO Output -> must not appear under GPIO Input"
        );
    }

    /// Once a pin holds a function it is offered only inside that function's
    /// category — never under any other peripheral it could otherwise serve.
    #[test]
    fn configured_pin_hidden_from_other_categories() {
        let mut mcu = create_stm32f103c8tx();
        // PA0 (pin 10) supports both GPIO and ADC1 IN0 on the C8T6.
        // Before assignment it shows under both GPIO Output and ADC.
        let before = build_categories(&mcu);
        assert!(
            category(&before, "ADC")
                .pins
                .iter()
                .any(|p| p.pin_num == 10)
        );
        assert!(
            category(&before, "GPIO Output")
                .pins
                .iter()
                .any(|p| p.pin_num == 10)
        );

        // Assign ADC → PA0 must vanish from GPIO Output (and every other row).
        mcu.apply_pin_function(10, PinFunction::AdcChannel { adc: 1, channel: 0 });
        let after = build_categories(&mcu);
        assert!(
            category(&after, "ADC").pins.iter().any(|p| p.pin_num == 10),
            "PA0 stays in its own ADC category"
        );
        assert!(
            !category(&after, "GPIO Output")
                .pins
                .iter()
                .any(|p| p.pin_num == 10),
            "PA0 is taken by ADC -> hidden from GPIO Output"
        );
    }

    /// A pin that supports several signals of one peripheral (ESP32-C3 GPIO
    /// matrix) is listed ONCE with every signal as a menu option — not repeated.
    #[test]
    fn multi_signal_pin_is_listed_once() {
        let mcu = Mcu::new(
            "t".into(),
            "esp32c3".into(),
            ToolchainKind::EspRust,
            vec![],
            vec![],
            vec![Pin::new(28, "GPIO21").with_functions(vec![
                PinFunction::SpiSck(2),
                PinFunction::SpiMosi(2),
                PinFunction::SpiMiso(2),
                PinFunction::SpiNss(2),
            ])],
            vec![],
        );
        let cats = build_categories(&mcu);
        let spi = category(&cats, "SPI");
        assert_eq!(spi.pins.len(), 1, "GPIO21 should appear once, not 4×");
        assert_eq!(spi.pins[0].options.len(), 4, "all 4 SPI signals as options");
    }

    /// An exclusive signal assigned to one pin is hidden from the others in the
    /// Peripherals tab (mirrors the Pins panel), so it can't be assigned twice.
    #[test]
    fn taken_signal_is_hidden_on_other_pins() {
        let spi_sck = || vec![PinFunction::SpiSck(1)];
        let mut mcu = Mcu::new(
            "t".into(),
            "stm32f1".into(),
            ToolchainKind::RustEmbedded,
            vec![],
            vec![],
            vec![Pin::new(1, "PA5").with_functions(spi_sck())],
            vec![Pin::new(2, "PB3").with_functions(spi_sck())],
        );

        // Both pins offer SPI1 SCK before anything is assigned.
        assert_eq!(category(&build_categories(&mcu), "SPI").pins.len(), 2);

        // Assign it to pin 1 → it disappears from pin 2 (only the owner remains).
        mcu.apply_pin_function(1, PinFunction::SpiSck(1));
        let cats = build_categories(&mcu);
        let spi = category(&cats, "SPI");
        assert_eq!(spi.pins.len(), 1, "taken signal hidden on the other pin");
        assert_eq!(spi.pins[0].pin_num, 1);
        assert!(spi.pins[0].assigned().is_some());
    }

    /// Every category lands in exactly one column, and the split is the one the
    /// tab promises: single-pin functions left, multi-pin protocols right.
    #[test]
    fn every_category_has_a_column() {
        // No pins, so only the hand-written rows: the derived ones are
        // per-chip and have their own test.
        for def in category_defs(&[]) {
            let expected = match def.name.as_str() {
                "GPIO Output" | "GPIO Input" | "ADC" | "DAC" | "TOUCH" | "Timers / PWM"
                | "MCPWM" | "PCNT" | "RMT" | "MCO / Clock" => Complexity::Simple,
                _ => Complexity::Complex,
            };
            assert_eq!(def.complexity, expected, "wrong column for {}", def.name);
        }
        // The complex column is the buses, SWD, and the ESP wide parallel
        // ports - none of which `is_bus()` knows about, because they predate
        // nothing in it: they are simply pads that have to be picked as a set.
        let complex: Vec<String> = category_defs(&[])
            .into_iter()
            .filter(|d| d.complexity == Complexity::Complex)
            .map(|d| d.name)
            .collect();
        let complex: Vec<&str> = complex.iter().map(String::as_str).collect();
        assert_eq!(
            complex,
            [
                "USART",
                "SPI",
                "I2S",
                "HSPI",
                "XSPI",
                "OCTOSPI",
                "QUADSPI",
                "SDMMC",
                "PARL",
                "PARL RX",
                "LCD",
                "CAM",
                "SAI",
                "I2C",
                "USB",
                "CAN",
                "SWD / Debug"
            ]
        );
    }

    /// Every row says what the peripheral IS, not just its acronym. A name
    /// alone is readable only by someone who already knows the answer.
    #[test]
    fn every_category_says_what_it_is() {
        for def in category_defs(&[]) {
            assert!(
                !def.blurb.trim().is_empty(),
                "{} has no description",
                def.name
            );
            // Short enough to fit under the name in a half-width column - the
            // point is a hint, not a datasheet.
            assert!(
                def.blurb.chars().count() <= 60,
                "{} description is {} chars: {:?}",
                def.name,
                def.blurb.chars().count(),
                def.blurb
            );
            // The description is not the name again.
            assert_ne!(def.blurb, def.name, "{} describes itself", def.name);
        }
    }

    /// A derived row always says the thing that changes what you do next: the
    /// IDE lists it, it does not generate it. True whether or not the
    /// peripheral is one this table has heard of.
    #[test]
    fn a_derived_row_admits_it_generates_nothing() {
        assert!(
            derived_blurb("COMP1").starts_with("analog comparator"),
            "{}",
            derived_blurb("COMP1")
        );
        for name in ["COMP1", "OPAMP2", "FMC", "LTDC", "WHAT_IS_THIS"] {
            assert!(
                derived_blurb(name).contains("no code"),
                "{name}: {}",
                derived_blurb(name)
            );
        }
        // Keyed on the family: every instance of one peripheral reads alike.
        assert_eq!(derived_blurb("COMP1"), derived_blurb("COMP7"));
        // …and still fits under a name in a half-width column.
        for name in ["COMP1", "DFSDM1", "OCTOSPIM", "SDIO", "WHAT_IS_THIS"] {
            let b = derived_blurb(name);
            assert!(
                b.chars().count() <= 72,
                "{name}: {} chars: {b:?}",
                b.chars().count()
            );
        }
    }

    fn other(sig: &str) -> PinFunction {
        PinFunction::Other(sig.to_owned())
    }

    /// The peripherals from the report: visible on the pins, absent from the
    /// tab because nothing matched `PinFunction::Other`.
    #[test]
    fn unrecognised_signals_become_one_category_per_peripheral() {
        let p1 = Pin::new(1, "PA0").with_functions(vec![other("COMP1_INP"), other("DAC1_OUT1")]);
        let p2 = Pin::new(2, "PA1").with_functions(vec![other("COMP1_INM"), other("FMC_D0")]);
        let p3 = Pin::new(3, "PA2").with_functions(vec![other("COMP2_INP")]);
        let pins: Vec<&Pin> = vec![&p1, &p2, &p3];

        let derived: Vec<(String, usize)> = other_peripherals(&pins);
        assert_eq!(
            derived,
            vec![
                ("COMP1".to_owned(), 2),
                ("COMP2".to_owned(), 1),
                ("DAC1".to_owned(), 1),
                ("FMC".to_owned(), 1),
            ],
            "one row per INSTANCE, with its distinct signal count"
        );
    }

    /// A peripheral whose pins must be chosen together belongs on the right;
    /// a single-output one does not. Derived from the chip, not a list.
    #[test]
    fn multi_signal_peripherals_land_in_the_complex_column() {
        let p1 = Pin::new(1, "PA0").with_functions(vec![other("COMP1_INP"), other("DAC1_OUT1")]);
        let p2 = Pin::new(2, "PA1").with_functions(vec![other("COMP1_INM")]);
        let pins: Vec<&Pin> = vec![&p1, &p2];
        let defs = category_defs(&pins);
        let col = |n: &str| {
            defs.iter()
                .find(|d| d.name == n)
                .unwrap_or_else(|| panic!("no category {n}"))
                .complexity
        };
        assert_eq!(col("COMP1"), Complexity::Complex, "INP + INM go together");
        assert_eq!(col("DAC1"), Complexity::Simple, "one output, one pin");
        // …and the hand-written rows keep their place at the top.
        assert_eq!(defs[0].name, "GPIO Output");
    }

    /// A derived peripheral whose name is already taken by a hand-written row
    /// must not become a second row: two categories with one name hash to one
    /// CollapsingHeader id, and egui paints an "ID clash" banner over the tab.
    /// The leftover signals join the row that owns the name.
    #[test]
    fn derived_peripheral_never_duplicates_a_hand_written_row() {
        // An STM32 whose OTG signals the importer left as `Other` derives the
        // name "USB", which the UsbDm/UsbDp row already holds.
        let p1 =
            Pin::new(1, "PA11").with_functions(vec![PinFunction::UsbDm, other("USB_OTG_FS_ID")]);
        let p2 =
            Pin::new(2, "PA12").with_functions(vec![PinFunction::UsbDp, other("USB_OTG_FS_SOF")]);
        let pins: Vec<&Pin> = vec![&p1, &p2];

        let defs = category_defs(&pins);
        let usb: Vec<&CategoryDef> = defs.iter().filter(|d| d.name == "USB").collect();
        assert_eq!(usb.len(), 1, "one USB row, not one per source of signals");

        // …and the merged row answers for both halves of the peripheral.
        let pred = &usb[0].pred;
        assert!(pred(&PinFunction::UsbDm), "keeps the recognised signals");
        assert!(pred(&other("USB_OTG_FS_ID")), "adopts the leftovers");
        assert!(!pred(&other("FMC_D0")), "and nothing else");
        assert_eq!(
            usb[0].complexity,
            Complexity::Complex,
            "row keeps its column"
        );
    }

    /// The header id is hashed from the category name, so a duplicate name is
    /// an id clash. Guard the invariant for every row the tab can build.
    #[test]
    fn category_names_are_unique() {
        let p1 =
            Pin::new(1, "PA11").with_functions(vec![PinFunction::UsbDm, other("USB_OTG_FS_ID")]);
        let p2 = Pin::new(2, "PA0").with_functions(vec![
            other("COMP1_INP"),
            other("ADC_IN0"),
            other("RTC"),
        ]);
        let p3 = Pin::new(3, "PB8").with_functions(vec![PinFunction::CanRx, other("CAN_RX1")]);
        let pins: Vec<&Pin> = vec![&p1, &p2, &p3];

        let names: Vec<String> = category_defs(&pins).into_iter().map(|d| d.name).collect();
        let unique: std::collections::BTreeSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate category name in {names:?}"
        );
    }

    /// The predicate has to catch its own signals and nobody else's - the
    /// prefix is compared whole, so COMP1 must not swallow COMP10.
    #[test]
    fn a_derived_predicate_matches_only_its_own_peripheral() {
        let p = Pin::new(1, "PA0").with_functions(vec![other("COMP1_INP"), other("COMP10_INP")]);
        let pins: Vec<&Pin> = vec![&p];
        let defs = category_defs(&pins);
        let comp1 = defs.iter().find(|d| d.name == "COMP1").unwrap();
        assert!((comp1.pred)(&other("COMP1_INP")));
        assert!(!(comp1.pred)(&other("COMP10_INP")), "COMP1 is not COMP10");
        assert!(!(comp1.pred)(&PinFunction::GpioInput));
    }

    /// A chip with nothing unrecognised gets nothing extra, so the common
    /// case is untouched.
    #[test]
    fn a_fully_recognised_chip_grows_no_categories() {
        let p = Pin::new(1, "PA9").with_functions(vec![PinFunction::UsartTx(1)]);
        assert!(other_peripherals(&[&p]).is_empty());
        assert_eq!(category_defs(&[&p]).len(), category_defs(&[]).len());
    }

    /// Colours follow the NAME, so loading a different part does not repaint
    /// the rows that survived.
    ///
    /// Stability is the guarantee; distinctness is NOT. Six buckets over
    /// arbitrary names collide by design, and an assertion that two chosen
    /// names differ would be a coin flip dressed as a test - the first pair I
    /// picked, COMP1 and FMC, happens to collide. What is worth pinning is
    /// that the palette is actually spread rather than constant.
    /// No two hand-written rows share a colour, and none sits closer to another
    /// than the closest DELIBERATE pair.
    ///
    /// The swatch is how a row is recognised at a glance, and the table has
    /// grown from thirteen to twenty-seven. Eight of those colours arrived at
    /// once, picked against a palette listing rather than against each other -
    /// which is exactly how two rows end up indistinguishable with nothing
    /// failing.
    ///
    /// The floor is 20 in RGB space because HSPI and XSPI sit at 21 ON PURPOSE:
    /// they are one family of external-memory controller and read as one. A new
    /// row closer than that to anything is almost certainly an accident; if it
    /// is not, the pair belongs in the exemption below with a reason.
    #[test]
    fn every_category_colour_is_its_own() {
        let defs = category_defs(&[]);
        let mut seen: std::collections::BTreeMap<(u8, u8, u8), Vec<&str>> =
            std::collections::BTreeMap::new();
        for d in &defs {
            seen.entry(d.rgb).or_default().push(&d.name);
        }
        for (rgb, names) in &seen {
            assert_eq!(names.len(), 1, "{names:?} all draw {rgb:?}");
        }

        // One family, deliberately close.
        const ALLOWED_NEAR: [(&str, &str); 3] = [
            ("HSPI", "XSPI"),
            ("OCTOSPI", "XSPI"),
            ("OCTOSPI", "QUADSPI"),
        ];
        let dist = |a: (u8, u8, u8), b: (u8, u8, u8)| -> f64 {
            let f = |x: u8, y: u8| (f64::from(x) - f64::from(y)).powi(2);
            (f(a.0, b.0) + f(a.1, b.1) + f(a.2, b.2)).sqrt()
        };
        for (i, a) in defs.iter().enumerate() {
            for b in &defs[i + 1..] {
                let near = ALLOWED_NEAR
                    .iter()
                    .any(|(x, y)| (a.name == *x && b.name == *y) || (a.name == *y && b.name == *x));
                if near {
                    continue;
                }
                let d = dist(a.rgb, b.rgb);
                assert!(
                    d >= 20.0,
                    "{} and {} are {d:.0} apart in RGB - one swatch, two rows",
                    a.name,
                    b.name
                );
            }
        }
    }

    #[test]
    fn derived_colours_are_stable_per_name() {
        assert_eq!(derived_color("COMP1"), derived_color("COMP1"));
        assert_eq!(derived_color("FMC"), derived_color("FMC"));
        let names = [
            "COMP1", "COMP2", "COMP3", "DAC1", "DAC2", "FMC", "LTDC", "DCMIPP", "RTC", "OPAMP1",
            "OPAMP2", "SAI1", "QUADSPI", "SDMMC1",
        ];
        let used: std::collections::BTreeSet<_> = names.iter().map(|n| derived_color(n)).collect();
        assert!(used.len() >= 4, "palette barely used: {used:?}");
    }

    /// Cost counts *categories*, not signals: a pad routable to four SPI signals
    /// gives up one peripheral, not four. GPIO never counts.
    #[test]
    fn pin_cost_counts_categories_not_signals() {
        let defs = category_defs(&[]);

        let gpio_only = Pin::new(1, "PC13")
            .with_functions(vec![PinFunction::GpioInput, PinFunction::GpioOutput]);
        assert_eq!(pin_cost(&gpio_only, &defs), 0);

        let spi_matrix = Pin::new(2, "GPIO21").with_functions(vec![
            PinFunction::GpioOutput,
            PinFunction::SpiSck(2),
            PinFunction::SpiMosi(2),
            PinFunction::SpiMiso(2),
            PinFunction::SpiNss(2),
        ]);
        assert_eq!(
            pin_cost(&spi_matrix, &defs),
            1,
            "4 SPI signals = 1 category"
        );

        let busy = Pin::new(3, "PA9").with_functions(vec![
            PinFunction::GpioOutput,
            PinFunction::UsartTx(1),
            PinFunction::TimerPwm {
                timer: 1,
                channel: 2,
            },
            PinFunction::AdcChannel { adc: 1, channel: 9 },
        ]);
        assert_eq!(pin_cost(&busy, &defs), 3, "USART + PWM + ADC");
    }

    /// Inside a group the cheapest pins come first, ties broken by pin number,
    /// so the pin you should burn on an LED is the one you reach first.
    #[test]
    fn pins_sorted_cheapest_first() {
        let mcu = Mcu::new(
            "t".into(),
            "stm32f1".into(),
            ToolchainKind::RustEmbedded,
            vec![],
            vec![],
            vec![
                // Costly: GPIO + USART + timer.
                Pin::new(1, "PA9").with_functions(vec![
                    PinFunction::GpioOutput,
                    PinFunction::UsartTx(1),
                    PinFunction::TimerPwm {
                        timer: 1,
                        channel: 2,
                    },
                ]),
                // Free, but a higher pin number than the other free one.
                Pin::new(9, "PC14").with_functions(vec![PinFunction::GpioOutput]),
                // Free.
                Pin::new(5, "PC13").with_functions(vec![PinFunction::GpioOutput]),
            ],
            vec![],
        );

        let cats = build_categories(&mcu);
        let out = category(&cats, "GPIO Output");
        let order: Vec<_> = out.pins.iter().map(|p| p.pin_num).collect();
        assert_eq!(order, [5, 9, 1], "free pins first, then by pin number");
        assert_eq!(out.pins[0].cost, 0);
        assert_eq!(out.pins[2].cost, 2);
    }

    #[test]
    fn chip_styling_follows_cost() {
        // Free pin: bright text + green outline.
        assert_eq!(chip_text(0, false), TXT_FREE);
        assert_eq!(chip_stroke(0, false).color, FREE_PIN);
        // Scarce pin: faded, no outline.
        assert_eq!(chip_text(SCARCE_AT, false), TXT_SCARCE);
        assert_eq!(chip_stroke(SCARCE_AT, false), egui::Stroke::NONE);
        // Assigned chips are solid category colour — white text, no outline.
        assert_eq!(chip_text(0, true), egui::Color32::WHITE);
        assert_eq!(chip_stroke(0, true), egui::Stroke::NONE);
    }

    #[test]
    fn click_action_assigns_then_clears() {
        let unset = PinOption {
            func: PinFunction::GpioOutput,
            label: "GPIO Output".into(),
            assigned: false,
        };
        assert_eq!(click_action(2, &unset), (2, PinFunction::GpioOutput));

        let set = PinOption {
            assigned: true,
            ..unset
        };
        assert_eq!(click_action(2, &set), (2, PinFunction::Unset));
    }
}

#[cfg(test)]
mod filter_tests {
    use super::*;

    fn opt(label: &str) -> PinOption {
        PinOption {
            func: PinFunction::Unset,
            label: label.to_owned(),
            assigned: false,
        }
    }

    fn pin(num: usize, name: &str, labels: &[&str]) -> PinEntry {
        PinEntry {
            pin_num: num,
            pin_name: name.to_owned(),
            cost: 0,
            options: labels.iter().map(|l| opt(l)).collect(),
        }
    }

    fn cats() -> Vec<CategoryView> {
        vec![
            CategoryView {
                name: "USART".into(),
                blurb: String::new(),
                color: egui::Color32::WHITE,
                complexity: Complexity::Complex,
                pins: vec![
                    pin(9, "PA9", &["USART1  TX"]),
                    pin(10, "PA10", &["USART1  RX"]),
                ],
            },
            CategoryView {
                name: "SPI".into(),
                blurb: String::new(),
                color: egui::Color32::WHITE,
                complexity: Complexity::Complex,
                pins: vec![pin(7, "PA7", &["SPI1  MOSI"])],
            },
        ]
    }

    #[test]
    fn a_blank_query_changes_nothing() {
        let out = filter_categories(cats(), "   ");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].pins.len(), 2);
    }

    /// Naming the PERIPHERAL keeps all of its pins: "usart" means show me the
    /// USART, not the pins whose text happens to contain the word.
    #[test]
    fn a_peripheral_name_keeps_the_whole_row() {
        let out = filter_categories(cats(), "usart");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pins.len(), 2, "both pins survive");
    }

    /// Naming a PIN narrows to it, so the answer is one row and one pin.
    #[test]
    fn a_pin_name_narrows_to_that_pin() {
        let out = filter_categories(cats(), "pa10");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pins.len(), 1);
        assert_eq!(out[0].pins[0].pin_name, "PA10");
    }

    #[test]
    fn a_signal_name_works_too() {
        let out = filter_categories(cats(), "mosi");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "SPI");
    }

    /// Several terms all have to match, which is how a long list gets narrowed
    /// without knowing the exact label.
    #[test]
    fn every_term_must_match() {
        assert_eq!(filter_categories(cats(), "usart rx")[0].pins.len(), 1);
        // …and a term that matches nothing removes the row entirely.
        assert!(filter_categories(cats(), "usart i2c").is_empty());
    }

    #[test]
    fn case_and_spacing_do_not_matter() {
        assert_eq!(filter_categories(cats(), "  Pa9 ").len(), 1);
        assert_eq!(filter_categories(cats(), "SpI").len(), 1);
    }
}
