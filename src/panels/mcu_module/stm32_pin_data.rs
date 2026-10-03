//! Converter: STMicroelectronics **STM32_open_pin_data** XML → [`McuForm`]s.
//!
//! ST publishes one XML per MCU in
//! <https://github.com/STMicroelectronics/STM32_open_pin_data> (`mcu/*.xml`):
//! vendor pin / alternate-function data, the same source CubeMX uses. This is
//! the deterministic bulk-import path (no AI, no guessing) for adding many
//! STM32 chips at once — the biggest lever for growing the chip catalog.
//!
//! One file can describe a **range** of flash variants — `RefName` looks like
//! `STM32F103C(8-B)Tx` with one `<Flash>` element per code — so a single XML
//! expands into several concrete chips. Each becomes a [`McuForm`] (reusing its
//! token grammar, validation and clock model); the caller saves the ones whose
//! [`McuForm::errors`] is empty as `.ron`, exactly like the New MCU form.
//!
//! Signals the IDE has no dedicated `PinFunction` for (SDMMC, FMC, `TIMx_CHyN`,
//! RTC tamper/timestamp, the trace pins, oscillator pins, …) are NOT dropped:
//! they are carried as generic `af:<name>` tokens and shown on the pin with
//! their datasheet name. The only exclusions are the `ADCx_EXTIn` / `DACx_EXTIn`
//! trigger lines and `EVENTOUT` — see `is_noise_signal`.

use super::mcu_catalog::ToolchainKind;
use super::mcu_def::{GridCellDef, PinDef, PinGridDef};
use super::mcu_form::{ClockChoice, McuForm, PinRow, parse_functions};

/// One chip produced from the XML (a range file yields several), plus any
/// per-file advisories to surface after the import.
pub struct ConvertedChip {
    pub form: McuForm,
    pub warnings: Vec<String>,
}

/// Parse one STM32 open-pin-data `<Mcu>` document into one form per flash
/// variant. `Err` only on unusable XML (not a `<Mcu>` / no `RefName`).
///
/// Without the companion GPIO IP table — see [`convert_xml_with_af`].
pub fn convert_xml(xml: &str) -> Result<Vec<ConvertedChip>, String> {
    convert_xml_with_af(xml, None)
}

/// The same, plus the alternate-function indices from the chip's GPIO IP file.
///
/// Split so the converter itself stays PURE: it reads no files, and the caller
/// (which already has the MCU file's path) resolves the sibling IP file. `None`
/// simply means no AF numbers are recorded, which is also the right answer for
/// STM32F1 and for a lone XML copied out of the vendor repo.
pub fn convert_xml_with_af(xml: &str, af: Option<&GpioAf>) -> Result<Vec<ConvertedChip>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("XML parse error: {e}"))?;
    let mcu = doc.root_element();
    // The document uses a default namespace (`xmlns="http://dummy.com"`), so we
    // match by LOCAL name throughout.
    if mcu.tag_name().name() != "Mcu" {
        return Err("not an STM32 open-pin-data file (root is not <Mcu>)".into());
    }
    let ref_name = mcu
        .attribute("RefName")
        .ok_or("XML has no RefName attribute")?
        .trim();
    let family = mcu
        .attribute("Family")
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let package = mcu.attribute("Package").unwrap_or("").trim().to_string();
    let line = mcu.attribute("Line").unwrap_or("").trim().to_string();

    let mut core = String::new();
    let mut max_mhz: Option<u32> = None;
    let mut rams: Vec<u64> = Vec::new();
    let mut flashes: Vec<u64> = Vec::new();
    let mut pin_rows: Vec<PinRow> = Vec::new();
    // Balls of a grid package, with the cell their designator resolves to.
    let mut ball_rows: Vec<(usize, usize, PinRow)> = Vec::new();
    let mut skipped_positions = 0usize;

    for ch in mcu.children().filter(|n| n.is_element()) {
        match ch.tag_name().name() {
            "Core" => core = ch.text().unwrap_or("").trim().to_string(),
            // A DISPLAY fact only: the clock editor's ceilings come from
            // `ClockLimits`, which is a per-family table. Roughly a third of
            // the database states no <Frequency> at all (the whole C0 series
            // among them), so this stays an Option and an absent number is
            // shown as nothing rather than guessed from the family.
            "Frequency" => max_mhz = ch.text().and_then(|t| t.trim().parse::<u32>().ok()),
            "Ram" => {
                if let Some(v) = ch.text().and_then(|t| t.trim().parse::<u64>().ok()) {
                    rams.push(v);
                }
            }
            "Flash" => {
                if let Some(v) = ch.text().and_then(|t| t.trim().parse::<u64>().ok()) {
                    flashes.push(v);
                }
            }
            "Pin" => {
                let position = ch.attribute("Position").unwrap_or("").trim();
                let name_raw = ch.attribute("Name").unwrap_or("").trim();
                let ptype = ch.attribute("Type").unwrap_or("").trim();
                // The exposed thermal pad is not a pin (no package position).
                if is_exposed_pad(name_raw) {
                    continue;
                }
                let reserved = !(ptype == "I/O" || ptype == "MonoIO");
                let mut tokens: Vec<String> = Vec::new();
                // AF indices for this pin's signals, from the GPIO IP file. The
                // pin name is cleaned the same way on both sides so
                // "PC14-OSC32_IN" matches "PC14".
                let mut af_pairs: Vec<(String, u8)> = Vec::new();
                let clean_name = clean_pin_name(name_raw);
                if !reserved {
                    for sig in ch
                        .children()
                        .filter(|n| n.is_element() && n.tag_name().name() == "Signal")
                    {
                        let name = sig.attribute("Name").unwrap_or("");
                        if let Some(n) = af.and_then(|t| t.af(&clean_name, name)) {
                            af_pairs.push((name.to_owned(), n));
                        }
                        // The GPIO signal carries the pin's I/O MODES in an
                        // attribute — `IOModes="Input,Output,Analog,EXTI"`. It is
                        // where CubeMX's `GPIO_Analog` row comes from, and it was
                        // ignored: `GPIO` mapped to a flat "in out" for every pin,
                        // analog-capable or not.
                        let mut mapped = sig.attribute("Name").and_then(map_signal);
                        if name == "GPIO" {
                            if let Some(extra) = gpio_mode_tokens(sig.attribute("IOModes")) {
                                mapped = Some(match mapped {
                                    Some(m) => format!("{m} {extra}"),
                                    None => extra,
                                });
                            }
                        }
                        if let Some(tok) = mapped {
                            for t in tok.split_whitespace() {
                                if !tokens.iter().any(|x| x == t) {
                                    tokens.push(t.to_string());
                                }
                            }
                        }
                    }
                }
                let row = PinRow {
                    number: position.to_string(),
                    name: clean_pin_name(name_raw),
                    reserved,
                    functions: tokens.join(" "),
                    imported: false,
                    af: af_pairs,
                    fn_owner: Vec::new(),
                    note: String::new(),
                };
                // A package position is either a NUMBER (QFP, DIP: pins along
                // the edges) or a DESIGNATOR like "A2" (WLCSP, BGA: balls under
                // the die). The two are different layouts, so they go into
                // different buckets here — designators used to be dropped with a
                // "BGA package?" warning, which is what made those chips
                // unimportable.
                match crate::panels::mcu_module::mcu::model::parse_designator(position) {
                    Some((r, c)) => ball_rows.push((r, c, row)),
                    None if position.parse::<usize>().is_ok() => pin_rows.push(row),
                    None => {
                        if !position.is_empty() {
                            skipped_positions += 1;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Small packages bond two die pads to one package pin, and ST's XML says so
    // by giving two <Pin> elements the same Position. One pin, so one row.
    let merged_positions = merge_bonded_pins(&mut pin_rows);
    pin_rows.sort_by_key(|r| r.number.parse::<usize>().unwrap_or(usize::MAX));
    // Dual-in-line packages (SO8N, TSSOP, …) lay out on LEFT+RIGHT only.
    let sides = if is_two_row_package(&package) {
        distribute_sides_2row(&pin_rows)
    } else {
        distribute_sides(&pin_rows)
    };

    let clock = clock_for_family(&family);
    let target = core_to_target(&core).to_string();
    let cpu = core.trim_start_matches("Arm ").trim().to_string();

    // Balls become a grid layout; the four sides stay empty for such a package,
    // because a WLCSP/BGA genuinely has no edge pins.
    let grid = build_grid(&ball_rows);

    let mut base_warnings = Vec::new();
    if merged_positions > 0 {
        base_warnings.push(format!(
            "{merged_positions} package pin(s) carry two GPIOs bonded together (common on G0/C0 in small packages). Each is one pin here, offering both GPIOs' functions; the generated code names whichever one the function you pick belongs to."
        ));
    }
    if skipped_positions > 0 {
        base_warnings.push(format!(
            "{skipped_positions} pin(s) had a position that is neither a number nor a \
             package designator, and were skipped."
        ));
    }
    if let Some(g) = &grid {
        base_warnings.push(format!(
            "{} ball(s) laid out on a {}x{} grid ({package}).",
            g.cells.len(),
            g.rows,
            g.cols
        ));
        if !pin_rows.is_empty() {
            base_warnings.push(format!(
                "{} pin(s) also had plain numeric positions and were placed on the \
                 edges — check the result.",
                pin_rows.len()
            ));
        }
    }
    if pin_rows.is_empty() && grid.is_none() {
        base_warnings.push("No usable pins were found.".into());
    }
    // Say whether the AF indices came in. Silence would be ambiguous: "no
    // numbers" is correct for STM32F1 (no per-pin mux) but a missing sibling
    // file elsewhere, and the two need telling apart.
    match af {
        Some(t) if !t.is_empty() => base_warnings.push(format!(
            "Alternate-function indices read for {} pin/signal pair(s).",
            t.len()
        )),
        Some(_) if !family.starts_with("stm32f1") => base_warnings.push(
            "The GPIO IP file carried no alternate-function indices for this chip.".into(),
        ),
        Some(_) => {}
        None => base_warnings.push(
            "No GPIO IP file was read, so no alternate-function indices were recorded. Import from a full STM32_open_pin_data checkout (mcu/ next to mcu/IP/) to capture them."
                .into(),
        ),
    }

    let mut chips = Vec::new();
    for (name, idx) in expand_variants(ref_name) {
        // Range codes pair 1:1, in order, with the <Flash> entries.
        let flash_k = flashes.get(idx).or_else(|| flashes.last()).copied();
        let ram_k = rams.get(idx).or_else(|| rams.last()).copied();

        let mut form = McuForm::blank();
        form.id = slugify(&name);
        form.display_name = name.clone();
        form.family = family.clone();
        form.cpu = cpu.clone();
        form.package = package.clone();
        form.max_mhz = max_mhz;
        form.toolchain = ToolchainKind::RustEmbedded;
        form.target = target.clone();
        form.flash_origin = "0x08000000".into();
        form.flash_size = flash_k.map(|k| format!("{k}K")).unwrap_or_default();
        form.ram_origin = "0x20000000".into();
        form.ram_size = ram_k.map(|k| format!("{k}K")).unwrap_or_default();
        form.probe_chip = name.clone();
        form.hal_dep = hal_dep_for(&family, &line, &name);
        form.memory_comment = format!("Imported from STM32 open-pin-data ({ref_name})");
        form.clock = clock;
        form.pins = [
            sides[0].clone(),
            sides[1].clone(),
            sides[2].clone(),
            sides[3].clone(),
        ];
        form.grid = grid.clone();
        chips.push(ConvertedChip {
            form,
            warnings: base_warnings.clone(),
        });
    }
    Ok(chips)
}

/// Turn the collected balls into a [`PinGridDef`], or `None` when the package
/// has none.
///
/// Pin NUMBERS are assigned here, 1..N in reading order (row, then column),
/// because a grid package has none of its own: the board knows a ball by its
/// designator ("A2"), which the IDE derives back from `(row, col)`. The number
/// is purely our internal key — codegen, `mcu.config` and jump-to-code all use
/// it, so it must be stable and dense.
fn build_grid(balls: &[(usize, usize, PinRow)]) -> Option<PinGridDef> {
    if balls.is_empty() {
        return None;
    }
    let mut sorted: Vec<&(usize, usize, PinRow)> = balls.iter().collect();
    sorted.sort_by_key(|(r, c, _)| (*r, *c));
    let rows = sorted.iter().map(|(r, ..)| *r).max().unwrap_or(0) + 1;
    let cols = sorted.iter().map(|(_, c, _)| *c).max().unwrap_or(0) + 1;
    let cells = sorted
        .iter()
        .enumerate()
        .map(|(i, (r, c, row))| GridCellDef {
            row: *r,
            col: *c,
            pin: PinDef {
                number: i + 1,
                name: row.name.clone(),
                reserved: row.reserved,
                functions: parse_functions(&row.functions),
                af: row.af.clone(),
                fn_owner: crate::panels::mcu_module::mcu_form::owners_to_functions(&row.fn_owner),
                note: row.note.clone(),
            },
        })
        .collect();
    Some(PinGridDef { rows, cols, cells })
}

// ── Alternate-function numbers (the GPIO IP file) ────────────────────────────
// The `mcu/*.xml` file says WHICH signals a pin can carry; it never says under
// which alternate-function INDEX. That lives in a second vendor file, one per
// GPIO IP version, referenced from the MCU file:
//
//     <IP Name="GPIO" Version="STM32F303_gpio_v1_0" .../>
//     -> mcu/IP/GPIO-STM32F303_gpio_v1_0_Modes.xml
//
//     <GPIO_Pin Name="PC13">
//         <PinSignal Name="TIM1_CH1N">
//             <SpecificParameter Name="GPIO_AF">
//                 <PossibleValue>GPIO_AF4_TIM1</PossibleValue>   <- AF4
//
// Checked against the whole corpus: all 2240 MCU files resolve to one of the 98
// IP files. Note the key is `Version`, NOT `ConfigFile` - the latter is a
// coarser label ("GPIO-STM32F3xx") that matches no file at all.

/// The `Version` of the MCU file's GPIO IP block - the key to its modes file.
pub fn gpio_ip_version(mcu_xml: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(mcu_xml).ok()?;
    doc.root_element()
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "IP")
        .find(|n| n.attribute("Name") == Some("GPIO"))
        .and_then(|n| n.attribute("Version"))
        .map(|v| v.trim().to_owned())
}

/// The chip's USART IP version, as the vendor database names it
/// (`sci2_v1_1_Cube`, `sci3_v2_1_Cube`, …).
///
/// Captured at import for one reason: it is the only data we have that says
/// whether the USART has the SWAP / TXINV / RXINV bits — see
/// [`usart_has_swap_invert`].
pub fn usart_ip_version(mcu_xml: &str) -> Option<String> {
    ip_version(mcu_xml, "USART")
}

/// The vendor's version string for the `<IP Name="…">` block, if the chip has
/// one.
///
/// The IP version is the only thing in the vendor data that says WHICH
/// generation of a peripheral a part carries, and embassy gates real API
/// differences on exactly that (`usart_v3`, `sdmmc_v2`, …). Reading it per
/// chip rather than guessing per family is not pedantry: the L4 series ships
/// both SDMMC generations, so a family rule would be wrong on 64 of its 144
/// parts.
pub fn ip_version(mcu_xml: &str, name: &str) -> Option<String> {
    let doc = roxmltree::Document::parse(mcu_xml).ok()?;
    doc.root_element()
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "IP")
        .find(|n| n.attribute("Name") == Some(name))
        .and_then(|n| n.attribute("Version"))
        .map(|v| v.trim().to_owned())
}

/// The SDMMC IP version, under either of the two names the vendor uses.
///
/// F1/F2/F4/L1 call the block `SDIO` and everything since calls it `SDMMC`;
/// the version string is the same shape either way.
pub fn sdmmc_ip_version(mcu_xml: &str) -> Option<String> {
    ip_version(mcu_xml, "SDMMC").or_else(|| ip_version(mcu_xml, "SDIO"))
}

/// Which shape embassy's SDMMC constructors take on this chip.
///
/// This is NOT a cosmetic gate like the USART's swap/invert: the two versions
/// take different ARGUMENT LISTS. `sdmmc_v1` is fed a DMA channel and has to
/// bind its interrupt as well as the peripheral's; `sdmmc_v2` has its own
/// controller inside and takes neither. Generating one for the other does not
/// compile, so a chip with no captured IP version generates nothing at all.
///
/// The whole vendor database splits cleanly on the version PREFIX:
///
/// | version | families | this |
/// |---|---|---|
/// | `sdmmc_v1_2_Cube`, `sdmmc_v1_3_Cube` | F1, F2, F4, L1, F7, some L4 | [`SdmmcKind::V1`] |
/// | `sdmmc2_…` (incl. `STM32MP2_sdmmc2_…`) | other L4, L5, H5, H7, U3, U5, N6 | [`SdmmcKind::V2`] |
pub fn sdmmc_kind(ip_version: &str) -> Option<SdmmcKind> {
    if ip_version.contains("sdmmc2") {
        Some(SdmmcKind::V2)
    } else if ip_version.contains("sdmmc_v1") {
        Some(SdmmcKind::V1)
    } else {
        None
    }
}

/// The two shapes of embassy's SDMMC driver — see [`sdmmc_kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SdmmcKind {
    /// Takes a DMA channel, and binds both the peripheral's interrupt and the
    /// channel's.
    V1,
    /// Has its own DMA controller: no channel, no channel interrupt.
    V2,
}

/// Whether this chip's USART can swap RX/TX and invert the lines.
///
/// embassy exposes `Config::{swap_rx_tx, invert_tx, invert_rx}` only under
/// `#[cfg(any(usart_v3, usart_v4))]` — on an older peripheral the FIELD does not
/// exist, so generating an assignment for it does not compile. The IDE cannot
/// read embassy's cfgs, but the vendor's IP version answers the same question:
///
/// | CubeMX IP | embassy | swap/invert |
/// |---|---|---|
/// | `sci2_v1_1` (F103) | v1 | no |
/// | `sci2_v1_2` (F411) | v2 | no |
/// | `sci2_v2_1` (F303), `sci2_v2_2` (F030) | v3 | yes |
/// | `sci3_v1_1` (L432) | v3 | yes |
/// | `sci3_v2_0` (H743/WBA/U5), `sci3_v2_1` (G071/WLE5) | v4 | yes |
///
/// So the rule is "everything except `sci2_v1_*`", checked against
/// `stm32-metapac`'s own metadata for those nine families (see the test).
/// `None` — a chip imported before this was captured, or a built-in — answers
/// NO: refusing an option is recoverable, emitting a field that isn't there is
/// a compile error in the user's project.
pub fn usart_has_swap_invert(ip_version: Option<&str>) -> bool {
    ip_version.is_some_and(|v| !v.starts_with("sci2_v1"))
}

/// The file name `version` maps to inside the vendor repo's `mcu/IP/` folder.
pub fn gpio_ip_file_name(version: &str) -> String {
    format!("GPIO-{version}_Modes.xml")
}

/// Alternate-function indices for one GPIO IP version: `(pin, signal) -> AF`.
#[derive(Default)]
pub struct GpioAf {
    map: std::collections::HashMap<(String, String), u8>,
}

impl GpioAf {
    /// Parse a `GPIO-*_Modes.xml`. Pure; an unparseable file yields an EMPTY
    /// table, which simply means "no AF numbers known" - never an import error.
    pub fn parse(xml: &str) -> Self {
        let mut map = std::collections::HashMap::new();
        let Ok(doc) = roxmltree::Document::parse(xml) else {
            return Self { map };
        };
        for pin in doc
            .descendants()
            .filter(|n| n.is_element() && n.tag_name().name() == "GPIO_Pin")
        {
            let Some(pin_name) = pin.attribute("Name") else {
                continue;
            };
            // The IP file spells a pin the way the MCU file does, suffix included
            // ("PC14-OSC32_IN"), so both sides go through `clean_pin_name`.
            let pin_key = clean_pin_name(pin_name);
            for sig in pin
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "PinSignal")
            {
                let Some(sig_name) = sig.attribute("Name") else {
                    continue;
                };
                if let Some(af) = af_index_of(sig) {
                    map.insert((pin_key.clone(), sig_name.trim().to_owned()), af);
                }
            }
        }
        Self { map }
    }

    /// The AF index of one signal on one pin, if the vendor publishes one.
    pub fn af(&self, pin: &str, signal: &str) -> Option<u8> {
        self.map.get(&(pin.to_owned(), signal.to_owned())).copied()
    }

    /// How many `(pin, signal)` pairs carry an index. The import reports it, and
    /// ZERO is the honest answer for STM32F1: it has no per-pin AF mux, it
    /// remaps whole peripherals through AFIO.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// The `GPIO_AF` parameter of one `<PinSignal>`, as a number.
///
/// Values look like `GPIO_AF4_TIM1`: the digits between `GPIO_AF` and the next
/// `_` are the index. Anything else - STM32F1's `__HAL_AFIO_REMAP_*`, or a speed
/// / pull value belonging to a different parameter - yields `None`.
fn af_index_of(pin_signal: roxmltree::Node<'_, '_>) -> Option<u8> {
    let param = pin_signal
        .children()
        .filter(|n| n.is_element() && n.tag_name().name() == "SpecificParameter")
        .find(|n| n.attribute("Name") == Some("GPIO_AF"))?;
    let value = param
        .children()
        .find(|n| n.is_element() && n.tag_name().name() == "PossibleValue")?
        .text()?
        .trim();
    let digits = value.strip_prefix("GPIO_AF")?;
    let end = digits.find('_').unwrap_or(digits.len());
    digits[..end].parse().ok()
}

/// `true` for the exposed thermal / ground pad under QFN-style packages —
/// drawn INSIDE the package outline as "exposed pad VSS", "EPAD" or "thermal
/// pad". It carries no pin number, so it must never enter the pin list.
/// A normal numbered `VSS` pin is NOT one of these. `pub(crate)` — shared with
/// the AI datasheet import.
pub(crate) fn is_exposed_pad(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    let squashed = n.replace(['-', '_'], " ");
    squashed.contains("EXPOSED PAD")
        || squashed.contains("EXPOSEDPAD")
        || squashed.contains("THERMAL PAD")
        || squashed == "EPAD"
        || squashed == "PAD"
}

/// Signals that are not a pin FUNCTION at all. These are the only ones dropped;
/// everything else survives, natively or as a generic `af:` token.
///
/// It used to drop by prefix — `RCC_`, `RTC_`, `SYS_`, `DEBUG`, anything with
/// `EXTI` or `WKUP` — which swallowed whole peripherals: `RTC_TS`, `RTC_TAMP1`,
/// `RTC_OUT_ALARM`, `SYS_WKUP2`, `RCC_OSC_IN`, `SYS_PVD_IN`, the trace pins…
/// all real functions a datasheet lists, and all things CubeMX offers on the
/// pin. Across the vendor corpus those prefixes cover 2237 distinct signal
/// names, of which only the EXTI trigger lines are genuinely not pin functions.
///
/// `pub(crate)` — shared with the AI datasheet import, so both paths agree.
pub(crate) fn is_noise_signal(sig: &str) -> bool {
    let s = sig.trim();
    s.is_empty() || s == "EVENTOUT" || is_exti_trigger(s)
}

/// `ADC1_EXTI15`, `DAC1_EXTI9` — the EXTI line a peripheral can be TRIGGERED
/// from. It says something about the peripheral's wiring, not about what this
/// pin can be configured as, and the IDE models a pin's interrupt as an EDGE on
/// a GPIO input (`Pin.irq`) rather than as a function.
///
/// Matched by SHAPE (`_EXTI` + digits, nothing after) rather than by
/// `contains("EXTI")`: in the vendor corpus that shape is exclusively
/// `ADCx_EXTIn` / `DACx_EXTIn`.
fn is_exti_trigger(s: &str) -> bool {
    s.split_once("_EXTI").is_some_and(|(head, n)| {
        !head.is_empty() && !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
    })
}

/// Map one STM32 signal name to the IDE's function token(s).
///
/// Order matters: a NATIVE token wins first (so `GPIO`, `SYS_JTMS-SWDIO` and
/// `RCC_MCO` map even though the last two look like "noise"); then true noise
/// is dropped; **everything else becomes a generic `af:<name>` token** so no
/// pin function is ever silently lost — SAI, FMC, DCMI, QUADSPI, LTDC, ETH,
/// SDMMC, `TIMx_CHyN`, `I2Cx_SMBA`, … all survive with their datasheet name.
/// `pub(crate)` so the AI datasheet import maps signals the SAME way.
pub(crate) fn map_signal(sig: &str) -> Option<String> {
    if let Some(tok) = native_token(sig) {
        return Some(tok);
    }
    if is_noise_signal(sig) {
        return None;
    }
    Some(format!("af:{}", sig.trim().to_ascii_lowercase()))
}

/// Extra tokens implied by the GPIO signal's `IOModes` attribute.
///
/// `Input` / `Output` are already covered by mapping `GPIO` itself, and `EXTI` is
/// modelled as an edge on a GPIO input (`Pin.irq`), not as a function — so the
/// only mode that adds anything here is **Analog**, which is exactly the
/// `GPIO_Analog` entry CubeMX shows and the IDE used to lack.
fn gpio_mode_tokens(io_modes: Option<&str>) -> Option<String> {
    let modes = io_modes?;
    modes
        .split(',')
        .any(|m| m.trim().eq_ignore_ascii_case("Analog"))
        .then(|| "analog".to_owned())
}

/// The natively-modelled subset — `None` when the IDE has no dedicated
/// [`PinFunction`] for this signal.
fn native_token(sig: &str) -> Option<String> {
    // Debug: signals look like `SYS_JTMS-SWDIO` / `SYS_JTCK-SWCLK`.
    if sig.contains("SWDIO") {
        return Some("swdio".into());
    }
    if sig.contains("SWCLK") {
        return Some("swclk".into());
    }
    // Generic-IO capability → both directions.
    if sig == "GPIO" {
        return Some("in out".into());
    }
    // USB data lines (`USB_DM`/`USB_DP`, `USB_OTG_FS_DM`/`_DP`, …).
    if sig.starts_with("USB") {
        if sig.ends_with("_DM") {
            return Some("usb_dm".into());
        }
        if sig.ends_with("_DP") {
            return Some("usb_dp".into());
        }
    }
    // CAN / FDCAN — the IDE token carries no instance number.
    if sig.starts_with("CAN") || sig.starts_with("FDCAN") {
        if sig.ends_with("_RX") {
            return Some("can_rx".into());
        }
        if sig.ends_with("_TX") {
            return Some("can_tx".into());
        }
    }
    // `XSPIM_P1_IO12`. `NCLK` is the inverted clock and no constructor takes
    // it, so it stays a generic AF signal — same as the OCTOSPI's.
    if let Some(rest) = sig.strip_prefix("XSPIM_") {
        let (p, role) = rest.split_once('_')?;
        let port = p.strip_prefix('P')?.parse::<u8>().ok()?;
        if role == "CLK" {
            return Some(format!("xspi_p{port}_clk"));
        }
        if let Some(cs) = role.strip_prefix("NCS").and_then(|c| c.parse::<u8>().ok()) {
            return (cs == 1 || cs == 2).then(|| format!("xspi_p{port}_ncs{cs}"));
        }
        if let Some(i) = role.strip_prefix("DQS").and_then(|c| c.parse::<u8>().ok()) {
            return (i < 2).then(|| format!("xspi_p{port}_dqs{i}"));
        }
        return role
            .strip_prefix("IO")
            .and_then(|l| l.parse::<u8>().ok())
            .filter(|l| *l < 16)
            .map(|l| format!("xspi_p{port}_io{l}"));
    }
    // `OCTOSPIM_P1_IO3` — the vendor names the pads after the IO manager's
    // PORT. `NCLK` is the inverted clock of the DTR modes and no embassy
    // constructor takes it, so it stays a generic AF signal.
    if let Some(rest) = sig.strip_prefix("OCTOSPIM_") {
        let (p, role) = rest.split_once('_')?;
        let port = p.strip_prefix('P')?.parse::<u8>().ok()?;
        return match role {
            "CLK" => Some(format!("ospi_p{port}_clk")),
            "NCS" => Some(format!("ospi_p{port}_ncs")),
            "DQS" => Some(format!("ospi_p{port}_dqs")),
            _ => role
                .strip_prefix("IO")
                .and_then(|l| l.parse::<u8>().ok())
                .filter(|l| *l < 8)
                .map(|l| format!("ospi_p{port}_io{l}")),
        };
    }
    // QUADSPI, which has no instance number: what varies is the BANK. A chip
    // with one bank drops the tag entirely (`QUADSPI_NCS`), so that spelling
    // means bank 1.
    if let Some(role) = sig.strip_prefix("QUADSPI_") {
        return match role {
            "CLK" => Some("qspi_clk".to_owned()),
            "NCS" => Some("qspi_b1_ncs".to_owned()),
            _ => {
                let (bk, tail) = role.split_once('_')?;
                let bank = bk.strip_prefix("BK")?.parse::<u8>().ok()?;
                match tail {
                    "NCS" => Some(format!("qspi_b{bank}_ncs")),
                    _ => tail
                        .strip_prefix("IO")
                        .and_then(|l| l.parse::<u8>().ok())
                        .filter(|l| *l < 4)
                        .map(|l| format!("qspi_b{bank}_io{l}")),
                }
            }
        };
    }
    // The UN-NUMBERED `SDIO` of F1/F2/F4/L1 — the same block later families
    // call SDMMC1. Handled here because the instance split below needs a
    // number, and this one has none.
    if let Some(role) = sig.strip_prefix("SDIO_") {
        return sdmmc_role(role).map(|r| format!("sdmmc0_{r}"));
    }
    // Main clock output (`RCC_MCO`, `RCC_MCO_1`).
    if sig.contains("_MCO") {
        return Some("mco".into());
    }
    // Instance peripherals: `<WORD><n>_<ROLE>`.
    let (head, tail) = sig.split_once('_')?;
    let split = head.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let (word, n) = head.split_at(split);
    if n.is_empty() {
        return None; // needs an instance number
    }
    match word {
        // Plain UART maps onto usart; LPUART is its own peripheral (below).
        "USART" | "UART" => {
            let role = match tail {
                "TX" => "tx",
                "RX" => "rx",
                "CTS" => "cts",
                // RTS_DE is the same pin as RTS (it doubles as the RS485
                // driver-enable), so both spellings map to RTS.
                "RTS" | "RTS_DE" | "DE" => "rts",
                "CK" => "ck",
                _ => return None,
            };
            Some(format!("usart{n}_{role}"))
        }
        "LPUART" => {
            let role = match tail {
                "TX" => "tx",
                "RX" => "rx",
                "CTS" => "cts",
                "RTS" | "RTS_DE" | "DE" => "rts",
                _ => return None,
            };
            Some(format!("lpuart{n}_{role}"))
        }
        "SPI" => {
            let role = match tail {
                "NSS" => "nss",
                "SCK" => "sck",
                "MISO" => "miso",
                "MOSI" => "mosi",
                "RDY" => "rdy",
                _ => return None,
            };
            Some(format!("spi{n}_{role}"))
        }
        // `SAI1_SCK_A`. The PDM pads (`SAI1_CK1`, `SAI1_D2`) are a different
        // interface with no embassy driver here, so they stay generic AF.
        // `SDMMC1_D3`. The voltage-translator pads (`CKIN`, `CDIR`, `D0DIR`,
        // `D123DIR`) are not arguments to any embassy constructor, so they stay
        // generic AF signals.
        // `HSPI1_IO3`. `NCLK` is the inverted clock and no constructor takes
        // it, so it stays a generic AF signal.
        "HSPI" => match tail {
            "CLK" => Some(format!("hspi{n}_clk")),
            "NCS" => Some(format!("hspi{n}_ncs")),
            _ => {
                if let Some(i) = tail.strip_prefix("DQS").and_then(|c| c.parse::<u8>().ok()) {
                    return (i < 2).then(|| format!("hspi{n}_dqs{i}"));
                }
                tail.strip_prefix("IO")
                    .and_then(|l| l.parse::<u8>().ok())
                    .filter(|l| *l < 16)
                    .map(|l| format!("hspi{n}_io{l}"))
            }
        },
        "SDMMC" => sdmmc_role(tail).map(|r| format!("sdmmc{n}_{r}")),
        "SAI" => {
            let (role, letter) = tail.rsplit_once('_')?;
            let b = match letter {
                "A" => "a",
                "B" => "b",
                _ => return None,
            };
            let r = match role {
                "SCK" => "sck",
                "SD" => "sd",
                "FS" => "fs",
                "MCLK" => "mclk",
                _ => return None,
            };
            Some(format!("sai{n}_{b}_{r}"))
        }
        // `DAC1_OUT2`. `DAC1_EXTI9` is a trigger line, not a pad we drive, so
        // it falls through to the generic AF path.
        "DAC" => {
            let ch: u32 = tail.strip_prefix("OUT")?.parse().ok()?;
            Some(format!("dac{n}_out{ch}"))
        }
        // `I2S2_CK`. `I2S_CKIN` has no instance and is dropped by the check
        // above, which is right: it is a chip-level clock input, not a bus pad.
        "I2S" => {
            let role = match tail {
                "CK" => "ck",
                "WS" => "ws",
                "SD" => "sd",
                "MCK" => "mck",
                // `I2S2_ext_SD`, the F4 full-duplex second data pad, stays a
                // generic AF signal: embassy has no `I2Sext` driver.
                _ => return None,
            };
            Some(format!("i2s{n}_{role}"))
        }
        "I2C" => {
            let role = match tail {
                "SCL" => "scl",
                "SDA" => "sda",
                _ => return None, // e.g. I2Cx_SMBA
            };
            Some(format!("i2c{n}_{role}"))
        }
        "ADC" => {
            let ch: u32 = tail.strip_prefix("IN")?.parse().ok()?; // drops ADCx_EXTIy
            // Combined instances (`ADC12_IN5`) → the first instance only, so the
            // token stays valid (`adc1_5`, never a non-existent `adc12`).
            let inst = n.chars().next()?;
            Some(format!("adc{inst}_{ch}"))
        }
        "TIM" => {
            // The break inputs, named per index so BKIN and BKIN2 stay apart.
            // `BKIN_COMP1` and friends are a DIFFERENT thing — a comparator
            // routed to break, with its own pin trait — so they fall through to
            // the generic AF path rather than being folded in here.
            if tail == "BKIN" {
                return Some(format!("tim{n}_bkin1"));
            }
            if tail == "BKIN2" {
                return Some(format!("tim{n}_bkin2"));
            }
            let ch = tail.strip_prefix("CH")?;
            // Channels and their complementary outputs; `_ETR`, `_BKIN` and the
            // rest stay generic AF signals.
            let (digits, suffix) = match ch.strip_suffix('N') {
                Some(head) => (head, "n"),
                None => (ch, ""),
            };
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let ch: u32 = digits.parse().ok()?;
            Some(format!("tim{n}_{ch}{suffix}"))
        }
        _ => None,
    }
}

/// The token role of an SD-card signal, or `None` for a pad embassy's
/// constructors do not take.
fn sdmmc_role(tail: &str) -> Option<String> {
    match tail {
        "CK" => Some("ck".to_owned()),
        "CMD" => Some("cmd".to_owned()),
        _ => tail
            .strip_prefix('D')
            .and_then(|l| l.parse::<u8>().ok())
            .filter(|l| *l < 8)
            .map(|l| format!("d{l}")),
    }
}

/// Strip the extra tags STM32 pin names carry (`PC13-TAMPER-RTC` → `PC13`,
/// `PA0-WKUP` → `PA0`, `PB3 (JTDO-TRACESWO)` → `PB3`), but leave non-port names
/// (`VBAT`, `NRST`) untouched.
///
/// The parenthesised form is the STM32Cube database's; the dashed one is the
/// open-pin-data repo's. A pin whose tag survived became `p.PB3 (JTDO-TRACESWO)`
/// in the generated `main.rs` — code no compiler will accept.
fn clean_pin_name(raw: &str) -> String {
    let head = raw.split(['-', ' ', '(']).next().unwrap_or(raw);
    let b = head.as_bytes();
    let looks_like_port = b.len() >= 3
        && b[0] == b'P'
        && b[1].is_ascii_uppercase()
        && b[2..].iter().all(u8::is_ascii_digit);
    if looks_like_port {
        head.to_string()
    } else {
        raw.to_string()
    }
}

/// Expand a (possibly range) `RefName` into `(concrete name, flash index)`
/// pairs. `STM32F103C(8-B)Tx` → `[(…C8Tx, 0), (…CBTx, 1)]`; a plain name →
/// one entry at index 0.
pub fn expand_variants(ref_name: &str) -> Vec<(String, usize)> {
    if let (Some(open), Some(close)) = (ref_name.find('('), ref_name.find(')')) {
        if open < close {
            let prefix = &ref_name[..open];
            let inside = &ref_name[open + 1..close];
            let suffix = &ref_name[close + 1..];
            return inside
                .split('-')
                .enumerate()
                .map(|(i, code)| (format!("{prefix}{code}{suffix}"), i))
                .collect();
        }
    }
    vec![(ref_name.to_string(), 0)]
}

/// Split pins (sorted by number) across the four sides QFP-style, matching the
/// bundled F103 layout: left = first quarter, then bottom, then right, and top
/// last **reversed** (physical counter-clockwise numbering). Returns them in
/// `McuForm::pins` order `[top, bottom, left, right]`. `pub(crate)` so the AI
/// datasheet import lays pins out the SAME way (never "all on one side").
pub(crate) fn distribute_sides(rows: &[PinRow]) -> [Vec<PinRow>; 4] {
    let n = rows.len();
    let base = n / 4;
    let rem = n % 4;
    let mut sizes = [base; 4];
    for s in sizes.iter_mut().take(rem) {
        *s += 1;
    }
    let mut it = rows.iter().cloned();
    let left: Vec<_> = it.by_ref().take(sizes[0]).collect();
    let bottom: Vec<_> = it.by_ref().take(sizes[1]).collect();
    let mut right: Vec<_> = it.by_ref().take(sizes[2]).collect();
    let mut top: Vec<_> = it.by_ref().take(sizes[3]).collect();
    // Both of the "return" edges are numbered against the drawing order.
    // A QFP is numbered counter-clockwise from pin 1 at the top left: DOWN
    // the left edge, RIGHT along the bottom, then UP the right edge and LEFT
    // along the top. Each side is drawn top-to-bottom (or left-to-right), so
    // the two that run backwards have to be reversed.
    //
    // `top` always was. `right` was not, which put pin 25 at the top of an
    // LQFP48 where the datasheet and CubeMX both put pin 36 - the whole edge
    // upside down.
    right.reverse();
    top.reverse();
    [top, bottom, left, right]
}

/// A dual-in-line package (SOIC / TSSOP / SSOP / MSOP / SO8N / DIP): pins on two
/// opposite edges only, not four. Matched by the package NAME — the pin table
/// carries no shape info. Conservative substrings that no QFP/QFN/BGA hits.
pub(crate) fn is_two_row_package(package: &str) -> bool {
    let p = package.trim().to_ascii_uppercase();
    p.contains("SOP")      // SOP / SSOP / TSSOP / MSOP
        || p.contains("SOIC")
        || p.contains("DIP") // DIP / PDIP
        || p.contains("DIL")
        || p.starts_with("SO") // SO8N, SOT23
}

/// Lay pins out DIP/SOIC-style on the LEFT and RIGHT edges only (top/bottom
/// empty), with the real chip numbering: pin 1 top-left, counting DOWN the left
/// edge, then UP the right edge (so the highest number sits top-right). `rows`
/// are pre-sorted by pin number. Returns `[top, bottom, left, right]`.
pub(crate) fn distribute_sides_2row(rows: &[PinRow]) -> [Vec<PinRow>; 4] {
    let half = rows.len().div_ceil(2); // left keeps the extra pin for odd counts
    let left = rows[..half].to_vec();
    let mut right = rows[half..].to_vec();
    right.reverse(); // right edge counts UP from the bottom → top-to-bottom is reversed
    [Vec::new(), Vec::new(), left, right]
}

/// Cortex core string → Rust target triple. `pub(crate)` so [`mcu_identity`]
/// reuses the same mapping.
pub(crate) fn core_to_target(core: &str) -> &'static str {
    let c = core.to_ascii_lowercase();
    if c.contains("cortex-m0") {
        "thumbv6m-none-eabi"
    } else if c.contains("cortex-m33") {
        "thumbv8m.main-none-eabihf"
    // Armv8.1-M (M55 on STM32N6, M85) has no target of its own in stable rustc
    // — `rustup target add thumbv8.1m.main-none-eabihf` is refused. v8-M Main
    // is the triple these are built with, and the extra instructions are opt-in
    // anyway. Without this arm they fell through to the M3 default, which is
    // both the wrong architecture and the wrong float ABI.
    } else if c.contains("cortex-m55") || c.contains("cortex-m85") {
        "thumbv8m.main-none-eabihf"
    } else if c.contains("cortex-m23") {
        "thumbv8m.base-none-eabi"
    } else if c.contains("cortex-m4") || c.contains("cortex-m7") {
        "thumbv7em-none-eabihf"
    } else if c.contains("cortex-m3") {
        "thumbv7m-none-eabi"
    } else {
        "thumbv7m-none-eabi" // safe default
    }
}

/// Which built-in clock model fits a family (others get a plain reset clock).
/// Delegates to the single source of truth so XML import and AI import agree.
fn clock_for_family(family: &str) -> ClockChoice {
    ClockChoice::for_family(family)
}

/// Per-chip F4 clock ceilings (embassy's `max` table): SYSCLK varies by model
/// and the two PCLK ceilings follow the chip's bus-split rule. Applied by the
/// import handler over the form's F411-class default.
pub fn f4_limits_for_chip(id: &str) -> crate::panels::mcu_module::clock::model::ClockLimits {
    use crate::panels::mcu_module::clock::graph::stm32f4_limits;
    let m = 1_000_000;
    // `id` is the slug, e.g. "stm32f411re" → model "f411".
    let model = id.get(5..9).unwrap_or("");
    let (sysclk, high_split) = match model {
        "f401" => (84 * m, false),
        "f405" | "f407" | "f415" | "f417" => (168 * m, true),
        "f427" | "f429" | "f437" | "f439" | "f446" | "f469" | "f479" => (180 * m, true),
        // f410/f411/f412/f413/f423 and any unrecognised F4 → the 100 MHz class.
        _ => (100 * m, false),
    };
    if high_split {
        stm32f4_limits(sysclk, sysclk / 4, sysclk / 2) // PCLK1 = HCLK/4, PCLK2 = HCLK/2
    } else {
        stm32f4_limits(sysclk, sysclk / 2, sysclk) // PCLK1 = HCLK/2, PCLK2 = HCLK
    }
}

/// Fold rows that share a package position into one, returning how many
/// positions were folded.
///
/// STM32G0 and C0 in small packages bond two GPIO pads to a single package pin:
/// an STM32G030F6Px's pin 1 is PB7 *and* PB8, each `<Pin>` carrying its own
/// signals. Left as two rows they collide on the pin number, which the form
/// rejects ("Pin number 1 is used more than once") - 171 of the 2240 published
/// chips could not be imported at all.
///
/// The **richer** GPIO keeps the name, because that is the one most projects
/// will use and the one whose bindings read naturally. Everything the other one
/// adds is kept, tagged with its owner in [`PinRow::fn_owner`] so codegen can
/// name the right singleton later. A function BOTH provide (plain input/output)
/// gets no tag: either GPIO drives the same package pin, so the primary is as
/// correct as its sibling and needs no override.
fn merge_bonded_pins(rows: &mut Vec<PinRow>) -> usize {
    use std::collections::BTreeMap;
    let mut by_pos: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        by_pos.entry(r.number.clone()).or_default().push(i);
    }
    let groups: Vec<Vec<usize>> = by_pos.into_values().filter(|g| g.len() > 1).collect();
    if groups.is_empty() {
        return 0;
    }
    let merged = groups.len();
    let mut drop: Vec<usize> = Vec::new();
    for g in groups {
        // Richest first; ties keep the order ST published, so the result is
        // stable across runs rather than depending on a sort's tie-breaking.
        let mut order = g.clone();
        order.sort_by_key(|&i| std::cmp::Reverse(rows[i].functions.split_whitespace().count()));
        let (&primary, rest) = order.split_first().expect("group is non-empty");
        for &other in rest {
            let sibling = rows[other].clone();
            let base = &mut rows[primary];
            for tok in sibling.functions.split_whitespace() {
                if base.functions.split_whitespace().any(|t| t == tok) {
                    continue; // both GPIOs offer it - the primary answers for it
                }
                base.functions.push(' ');
                base.functions.push_str(tok);
                base.fn_owner.push((tok.to_string(), sibling.name.clone()));
            }
            for pair in sibling.af {
                if !base.af.iter().any(|(sig, _)| *sig == pair.0) {
                    base.af.push(pair);
                }
            }
            // A package pin is only reserved if NEITHER pad is usable.
            base.reserved = base.reserved && sibling.reserved;
            drop.push(other);
        }
    }
    drop.sort_unstable();
    for i in drop.into_iter().rev() {
        rows.remove(i);
    }
    merged
}

/// The HAL dependency line. STM32F1 keeps its dedicated `stm32f1xx-hal`; every
/// other STM32 family uses `embassy-stm32` (the generic embassy backend) with
/// the per-chip feature; non-STM32 families get a TODO (Cargo.toml is editable).
fn hal_dep_for(family: &str, line: &str, name: &str) -> String {
    if family == "stm32f1" {
        return format!(
            "stm32f1xx-hal = {{ version = \"0.10\", features = [\"{}\", \"rt\"] }}",
            line.to_ascii_lowercase()
        );
    }
    if family.starts_with("stm32") {
        // Generic embassy backend. The chip feature is the part number, plus a
        // flash-bank feature on the parts that need one (see `needs_bank_feature`).
        let mut feats = format!("\"{}\"", embassy_chip_feature(name));
        if needs_bank_feature(name) {
            feats.push_str(", \"single-bank\"");
        }
        return format!(
            "embassy-stm32 = {{ version = \"{EMBASSY_VERSION}\", features = [{feats}] }}"
        );
    }
    format!("# TODO: add the HAL / PAC dependency for family {family}")
}

/// Does this part need an explicit `single-bank` / `dual-bank` Cargo feature?
///
/// embassy's build script PANICS on a chip whose metapac metadata carries more
/// than one memory configuration and neither feature is enabled:
///
/// > Chip supports single and dual bank configuration. No Cargo feature to
/// > select one is enabled.
///
/// So the project fails before compiling a single line of generated code. The
/// affected set is 194 parts, derived by counting memory configurations across
/// every chip in `stm32-metapac` 21 - the same data embassy's build script
/// reads. Six families, and within a family it is per-PART, not per-line:
///
/// * **F42x/43x/46x/47x, 1 MB only** (`…g`). The 512 KB parts are single-bank
///   and the 2 MB parts are always dual, so only the middle size is a choice.
/// * **F76x/77x**, every size.
/// * **G0B1 / G0C1, 512 KB only** (`…c`); the 128 KB and 256 KB parts are not.
/// * **G471 / G473 / G474 / G483 / G484**, every size.
/// * **L4+ (L4P5/Q5/R5/R7/R9, L4S5/S7/S9)**, every size.
/// * **L552 512 KB only** (`…e`), **L562** every size.
///
/// This started as F4 + F7 alone, which is why an STM32G474 project would not
/// build at all until the other four families were added.
///
/// `single-bank` rather than `dual-bank` on all of them: it is the factory
/// option-byte state on F4 and F7, and on the rest it is the choice that
/// matches the `memory.x` this IDE writes, which describes flash as ONE
/// contiguous region of the full size. The feature only steers embassy's own
/// flash driver, so a user who flips the option bytes flips the feature next to
/// it in the editable `Cargo.toml`.
fn needs_bank_feature(name: &str) -> bool {
    let slug = slugify(name);
    let Some(line) = slug.get(..9) else {
        return false;
    };
    // The size code is the character after the package letter, i.e. the 11th of
    // `stm32f429zg…`; absent on a truncated name, which then needs nothing.
    let size = slug.as_bytes().get(10).copied().map(char::from);
    match line {
        // One size per line is the configurable one; the others are fixed.
        "stm32f427" | "stm32f429" | "stm32f437" | "stm32f439" | "stm32f469" | "stm32f479" => {
            size == Some('g')
        }
        "stm32g0b1" | "stm32g0c1" => size == Some('c'),
        "stm32l552" => size == Some('e'),
        // Whole lines, every size: metapac gives all of their parts two memory
        // configurations.
        "stm32f765" | "stm32f767" | "stm32f768" | "stm32f769" | "stm32f777" | "stm32f778"
        | "stm32f779" => true,
        "stm32g471" | "stm32g473" | "stm32g474" | "stm32g483" | "stm32g484" => true,
        "stm32l4p5" | "stm32l4q5" | "stm32l4r5" | "stm32l4r7" | "stm32l4r9" | "stm32l4s5"
        | "stm32l4s7" | "stm32l4s9" => true,
        "stm32l562" => true,
        _ => false,
    }
}

/// The HAL dependency line derived from a chip NAME alone — the path taken by
/// the AI datasheet import and the form's "Auto-fill from name", which (unlike
/// the XML importer) have no `<Line>` attribute to hand to [`hal_dep_for`].
/// STM32F1 keys `stm32f1xx-hal` on its `stm32f1NN` device feature, recovered
/// here from the part number; every other STM32 family uses the embassy
/// per-chip feature; non-STM32 families get the editable TODO line.
pub fn hal_dep_for_name(family: &str, name: &str) -> String {
    if family == "stm32f1" {
        return match f1_line_from_name(name) {
            Some(line) => hal_dep_for(family, &line, name),
            // An F1 family with a name we can't pin to a device feature — a
            // real line with the feature left out and a TODO beside it, rather
            // than an empty `features = ["", "rt"]`.
            None => f1_blocking_hal_dep_todo(name),
        };
    }
    // `line` is unused outside the F1 branch of `hal_dep_for`.
    hal_dep_for(family, "", name)
}

/// The `embassy-stm32` line an STM32F1 builds with on the Async runtime, or
/// `None` when neither name is a recognisable F1 part number.
///
/// F1 is the one STM32 family with two HALs: `stm32f1xx-hal` on Blocking,
/// Native and RTIC, embassy-stm32 on Async. The line is DERIVED rather than
/// stored because the `.ron` files already on disk - the built-in and every F1
/// part imported before this existed - carry only the blocking line. Every F1
/// feature embassy-stm32 0.6 publishes is the part number's first eleven
/// characters (`stm32f103c8`): all 95 of them, counted in its Cargo.toml.
/// `probe_chip` comes first because it is the bare part number
/// (`STM32F103C8`), with `pkg_name` (`stm32f103c8t6`) as the fallback.
///
/// Both trailing letters are checked against what the F1 parts use, the pin
/// count (`c` 48, `r` 64, `t` 36, `v` 100, `z` 144) and the flash size (`4` to
/// `8`, `b` to `g`), so a generic name such as `STM32F103xB` is refused rather
/// than turned into a feature cargo has never heard of.
pub fn f1_embassy_hal_dep(probe_chip: &str, pkg_name: &str) -> Option<String> {
    [probe_chip, pkg_name].into_iter().find_map(|name| {
        let slug = slugify(name); // lower-case a-z0-9 only, so byte-indexable
        let b = slug.as_bytes();
        let ok = slug.len() >= 11
            && slug.starts_with("stm32f1")
            && b[7].is_ascii_digit()
            && b[8].is_ascii_digit()
            && b"crtvz".contains(&b[9])
            && b"468bcdefg".contains(&b[10]);
        ok.then(|| {
            format!(
                "{EMBASSY_CRATE} = {{ version = \"{EMBASSY_VERSION}\", features = [\"{}\"] }}",
                &slug[..11]
            )
        })
    })
}

/// The blocking line for an F1 whose part number names no `stm32f1xx-hal`
/// device feature: the crate without one, and a TODO saying which to add.
///
/// A dependency line, not a bare `# TODO` comment. A comment names no crate,
/// so after an Async round trip `project_gen::refresh_hal_dependency` had
/// nothing to swap back to, and the manifest kept embassy-stm32 under
/// stm32f1xx-hal code. Without a device feature the crate refuses to build
/// with its own "no device selected" error, which is the right place to learn.
pub fn f1_blocking_hal_dep_todo(name: &str) -> String {
    format!(
        concat!(
            "stm32f1xx-hal = {{ version = \"0.10\", features = [\"rt\"] }} ",
            "# TODO: add the device feature for {name}, such as \"stm32f103\""
        ),
        name = name,
    )
}

/// The async line for an F1 whose part number [`f1_embassy_hal_dep`] cannot
/// read. Still an `embassy-stm32` line, so the runtime switch swaps the crate
/// and the manifest matches the embassy code - with the feature to fill in
/// named where cargo will point. A bare `# TODO` comment would leave the
/// blocking HAL in place, since a comment names no crate to swap to.
pub fn f1_embassy_hal_dep_todo(name: &str) -> String {
    // `concat!`, not a `\`-continued literal: rustfmt rejoins those and leaves
    // the indentation inside the manifest line.
    format!(
        concat!(
            "{krate} = {{ version = \"{version}\", features = [\"stm32f1xxxx\"] }} ",
            "# TODO: the chip feature for {name} - its part number's first 11 ",
            "characters, such as stm32f103c8"
        ),
        krate = EMBASSY_CRATE,
        version = EMBASSY_VERSION,
        name = name,
    )
}

/// The `stm32f1xx-hal` device feature (`stm32f103`) implied by an STM32F1 part
/// name, or `None` when the name isn't a recognisable F1 part number. The HAL
/// keys the feature on the 3-digit line (`stm32f100/101/103/…`) — the
/// `stm32f1` prefix plus the next two digits.
fn f1_line_from_name(name: &str) -> Option<String> {
    let slug = slugify(name); // lower-case a–z0–9 only → ASCII, byte-indexable
    let ok = slug.len() >= 9
        && slug.starts_with("stm32f1")
        && slug.as_bytes()[7].is_ascii_digit()
        && slug.as_bytes()[8].is_ascii_digit();
    ok.then(|| slug[..9].to_string())
}

/// The `embassy-stm32` version every generated STM32 project (except F1) pins.
///
/// One constant because it appears in a generated manifest AND in the
/// import-time feature check — two copies would drift, and the check would then
/// validate a version the project doesn't use.
///
/// Moved 0.4 -> 0.6 on 2026-08-15. What made it safe to move: `embassy-time`
/// stays `^0.5` across both (0.4 wanted ^0.5.0, 0.6 wants ^0.5.1), so the
/// `embassy-executor` 0.9 / `embassy-time` 0.5 pair the async template writes
/// still resolves; the crates that DID move are embassy-stm32's own private
/// deps (embassy-sync 0.7->0.8, embassy-hal-internal 0.3->0.5), which a
/// generated project never names. 0.6 also publishes ~50 more chip features
/// than 0.4.
pub const EMBASSY_VERSION: &str = "0.6";

/// The crate name that goes with it.
pub const EMBASSY_CRATE: &str = "embassy-stm32";

/// The chip feature written in an `embassy-stm32` dependency line, if that is
/// what this line is.
///
/// Pulled back OUT of the generated line rather than threaded through every
/// caller: the line is what actually ends up in `Cargo.toml`, so checking it is
/// checking the real thing — including a line the user has since edited.
pub fn embassy_feature_in(dep_line: &str) -> Option<&str> {
    let line = dep_line.trim();
    if !line.starts_with(EMBASSY_CRATE) {
        return None;
    }
    let features = line.split_once("features")?.1;
    let start = features.find('"')? + 1;
    let rest = &features[start..];
    let end = rest.find('"')?;
    Some(&rest[..end]).filter(|f| !f.is_empty())
}

/// The embassy-stm32 chip feature for a concrete part: the part number without
/// the trailing package + temperature code (open-pin-data names end in a
/// `<PackageLetter>x` pair — `STM32F411RETx` → `stm32f411re`).
fn embassy_chip_feature(name: &str) -> String {
    let slug = slugify(name);
    // `x` stands for the temperature range, and embassy's feature stops just
    // before the package letter that precedes it: STM32F103C8Tx -> stm32f103c8.
    if slug.len() > 2 && slug.ends_with('x') {
        return slug[..slug.len() - 2].to_string();
    }
    // 387 parts carry ONE more character after the `x` — an `N` for the
    // no-crystal variants (STM32C071C8TxN), a `Q` for the secure ones
    // (STM32N645A0HxQ). embassy does not distinguish those, so the same three
    // characters come off: verified against the published feature list, where
    // `stm32c071c8` and `stm32n645a0` exist and `stm32c071c8t` /
    // `stm32n645a0h` do not.
    //
    // Getting this wrong is not a small error: the derived name goes straight
    // into Cargo.toml, and a feature that does not exist makes the whole
    // project unresolvable — which kills rust-analyzer for it entirely.
    if slug.len() > 3 && slug.as_bytes()[slug.len() - 2] == b'x' {
        return slug[..slug.len() - 3].to_string();
    }
    slug
}

/// Lower-case, keep only `a–z 0–9` — a valid registry id / file stem.
fn slugify(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::PinFunction;

    /// A compact but format-accurate fixture (default namespace, range RefName,
    /// two `<Flash>`, and pins exercising every signal-mapping branch — plus a
    /// `<Condition>` child and a reserved power pin, as in the real files).
    const F103: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Mcu Family="STM32F1" Line="STM32F103" Package="LQFP48" RefName="STM32F103C(8-B)Tx" xmlns="http://dummy.com">
    <Core>Arm Cortex-M3</Core>
    <Ram>20</Ram>
    <Flash>64</Flash>
    <Flash>128</Flash>
    <Pin Name="VBAT" Position="1" Type="Power"/>
    <Pin Name="PC13-TAMPER-RTC" Position="2" Type="I/O">
        <Signal Name="RTC_OUT"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PA9" Position="3" Type="I/O">
        <Signal Name="TIM1_CH2"/>
        <Signal Name="USART1_TX"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PA11" Position="4" Type="I/O">
        <Signal Name="ADC1_EXTI11"/>
        <Signal Name="CAN_RX"/>
        <Signal Name="TIM1_CH4"/>
        <Signal Name="USART1_CTS"/>
        <Signal Name="USB_DM"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PB6" Position="5" Type="I/O">
        <Signal Name="I2C1_SCL"/>
        <Signal Name="I2C1_SMBA"/>
        <Signal Name="TIM4_CH1"/>
        <Signal Name="GPIO"/>
        <Condition Diagnostic="BZ#1" Expression="(!x)"/>
    </Pin>
    <Pin Name="PB13" Position="6" Type="I/O">
        <Signal Name="SPI2_SCK"/>
        <Signal Name="TIM1_CH1N"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PA13" Position="7" Type="I/O">
        <Signal Name="SYS_JTMS-SWDIO"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PA5" Position="8" Type="I/O">
        <Signal Name="ADC1_IN5"/>
        <Signal Name="ADC2_IN5"/>
        <Signal Name="SPI1_SCK"/>
        <Signal Name="GPIO"/>
    </Pin>
</Mcu>"#;

    /// PC13 of the STM32F358CCTx, copied verbatim from the vendor XML — the pin
    /// from the report: CubeMX offered eleven entries, the IDE showed three.
    ///
    /// Seven of the eleven come from these `<Signal>`s; the rest CubeMX derives
    /// (Reset_State, and Analog/EXTI from the GPIO `IOModes` attribute, which
    /// this importer still does not read).
    const F358_PC13: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Mcu Family="STM32F3" Line="STM32F358" Package="LQFP48" RefName="STM32F358CCTx" xmlns="http://dummy.com">
    <Core>Arm Cortex-M4</Core>
    <Ram>40</Ram>
    <Flash>256</Flash>
    <Pin Name="PC13" Position="2" Type="I/O">
        <Signal Name="RTC_OUT_ALARM"/>
        <Signal Name="RTC_OUT_CALIB"/>
        <Signal Name="RTC_TAMP1"/>
        <Signal Name="RTC_TS"/>
        <Signal Name="SYS_WKUP2"/>
        <Signal Name="TIM1_CH1N"/>
        <Signal IOModes="Input,Output,Analog,EXTI" Name="GPIO"/>
    </Pin>
    <Pin Name="PA13" Position="34" Type="I/O">
        <Signal Name="SYS_JTMS-SWDIO"/>
        <Signal Name="ADC1_EXTI11"/>
        <Signal IOModes="Input,Output,Analog,EVENTOUT,EXTI" Name="GPIO"/>
    </Pin>
</Mcu>"#;

    /// Every signal the datasheet lists for a pin reaches the pin. The old
    /// prefix filter dropped `RTC_*` and `SYS_WKUP*` wholesale, which is why
    /// PC13 showed 3 functions against CubeMX's 11.
    #[test]
    fn a_pins_datasheet_functions_are_not_dropped_by_prefix() {
        let chips = convert_xml(F358_PC13).expect("parses");
        let form = &chips[0].form;
        let pc13 = find(form, "PC13");

        for expected in [
            "af:rtc_out_alarm",
            "af:rtc_out_calib",
            "af:rtc_tamp1",
            "af:rtc_ts",
            "af:sys_wkup2",
            // Was a generic AF signal until CHxN became a function of its own.
            "tim1_1n",
            "in",
            "out",
            // From the GPIO signal's `IOModes="…,Analog,…"` attribute — CubeMX's
            // `GPIO_Analog` row, which the importer used to ignore entirely.
            "analog",
        ] {
            assert!(
                pc13.functions.split_whitespace().any(|t| t == expected),
                "PC13 must keep {expected}, got: {}",
                pc13.functions
            );
        }
        assert_eq!(
            pc13.functions.split_whitespace().count(),
            9,
            "six af tokens + in + out + analog: {}",
            pc13.functions
        );
        // CubeMX lists ELEVEN rows for this pin. The two we do not produce are
        // deliberate: `Reset_State` is our `Unset`, and `GPIO_EXTI13` is an EDGE
        // on a GPIO input (`Pin.irq`), not a function.

        // The EXTI TRIGGER line is still dropped, and the native mapping still
        // wins over the generic fallback.
        let pa13 = find(form, "PA13");
        assert_eq!(pa13.functions, "swdio in out analog", "{}", pa13.functions);
    }

    /// The GPIO IP file for PC13, verbatim from
    /// `mcu/IP/GPIO-STM32F303_gpio_v1_0_Modes.xml`.
    const F358_GPIO_IP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<IP xmlns="http://dummy.com" Name="GPIO" Version="STM32F303_gpio_v1_0">
    <GPIO_Pin PortName="PC" Name="PC13">
        <SpecificParameter Name="GPIO_Pin">
            <PossibleValue>GPIO_PIN_13</PossibleValue>
        </SpecificParameter>
        <SpecificParameter Name="GPIO_Speed">
            <PossibleValue>GPIO_SPEED_FREQ_LOW</PossibleValue>
        </SpecificParameter>
        <PinSignal Name="TIM1_CH1N">
            <SpecificParameter Name="GPIO_AF">
                <PossibleValue>GPIO_AF4_TIM1</PossibleValue>
            </SpecificParameter>
        </PinSignal>
    </GPIO_Pin>
    <GPIO_Pin PortName="PA" Name="PA13">
        <PinSignal Name="USART3_CTS">
            <SpecificParameter Name="GPIO_AF">
                <PossibleValue>GPIO_AF7_USART3</PossibleValue>
            </SpecificParameter>
        </PinSignal>
    </GPIO_Pin>
    <GPIO_Pin PortName="PC" Name="PC14-OSC32_IN">
        <SpecificParameter Name="GPIO_Speed">
            <PossibleValue>GPIO_SPEED_FREQ_LOW</PossibleValue>
        </SpecificParameter>
    </GPIO_Pin>
</IP>"#;

    /// The MCU file points at its GPIO IP file by `Version` — NOT by
    /// `ConfigFile`, which is a coarser label matching no file on disk.
    #[test]
    fn the_gpio_ip_file_is_found_by_version() {
        let xml = F358_PC13.replace(
            "<Core>",
            "<IP ConfigFile=\"GPIO-STM32F3xx\" InstanceName=\"GPIO\" Name=\"GPIO\"              Version=\"STM32F303_gpio_v1_0\"/>
    <Core>",
        );
        assert_eq!(
            gpio_ip_version(&xml).as_deref(),
            Some("STM32F303_gpio_v1_0")
        );
        assert_eq!(
            gpio_ip_file_name("STM32F303_gpio_v1_0"),
            "GPIO-STM32F303_gpio_v1_0_Modes.xml"
        );
        // A file with no GPIO IP block is not an error.
        assert_eq!(gpio_ip_version(F358_PC13), None);
        assert_eq!(gpio_ip_version("not xml at all"), None);
    }

    /// `GPIO_AF4_TIM1` is AF **4**, and a pin spelled with its suffix in the IP
    /// file must still match the cleaned name the pin list uses.
    #[test]
    fn af_indices_are_read_per_pin_and_signal() {
        let af = GpioAf::parse(F358_GPIO_IP);
        assert_eq!(af.af("PC13", "TIM1_CH1N"), Some(4));
        assert_eq!(af.af("PA13", "USART3_CTS"), Some(7));
        assert_eq!(af.len(), 2);
        // Unknown pairs, and a pin whose only parameters are speed/pin-number.
        assert_eq!(af.af("PC13", "RTC_TS"), None);
        assert_eq!(af.af("PC14", "GPIO"), None);
        // Junk is an empty table, never a failure — the import goes on without.
        assert!(GpioAf::parse("<not-xml").is_empty());
    }

    /// End to end: the indices reach `PinDef::af`, keyed by the vendor signal
    /// name, so a later step can configure the pin without re-importing.
    #[test]
    fn af_indices_are_stored_on_the_imported_pin() {
        let af = GpioAf::parse(F358_GPIO_IP);
        let chips = convert_xml_with_af(F358_PC13, Some(&af)).expect("parses");
        let def = chips[0].form.clone().to_definition();
        let pc13 = def
            .pins
            .top
            .iter()
            .chain(&def.pins.bottom)
            .chain(&def.pins.left)
            .chain(&def.pins.right)
            .find(|p| p.name == "PC13")
            .expect("PC13 imported");
        assert_eq!(pc13.af, vec![("TIM1_CH1N".to_string(), 4)]);

        // Without the table the import still works and records nothing.
        let plain = convert_xml(F358_PC13).expect("parses");
        let def = plain[0].form.clone().to_definition();
        assert!(
            def.pins
                .top
                .iter()
                .chain(&def.pins.bottom)
                .chain(&def.pins.left)
                .chain(&def.pins.right)
                .all(|p| p.af.is_empty())
        );
    }

    /// A WLCSP fixture in the shape ST publishes: `Position` is a package
    /// DESIGNATOR, not a number. Six of the twelve balls of the C011 part, which
    /// is enough to pin down the staggered pattern and the grid extent.
    const WLCSP: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Mcu Family="STM32C0" Line="STM32C011" Package="WLCSP12" RefName="STM32C011D6Yx" xmlns="http://dummy.com">
    <Core>Arm Cortex-M0+</Core>
    <Ram>6</Ram>
    <Flash>32</Flash>
    <Pin Name="PB6" Position="A2" Type="I/O">
        <Signal Name="USART1_TX"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PC15-OSCX_OUT" Position="A4" Type="I/O">
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PA13" Position="B1" Type="I/O">
        <Signal Name="SYS_JTMS-SWDIO"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PC14-OSCX_IN" Position="B3" Type="I/O">
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="VDD" Position="C4" Type="Power"/>
    <Pin Name="PF2-NRST" Position="F3" Type="I/O">
        <Signal Name="GPIO"/>
    </Pin>
</Mcu>"#;

    /// The whole point of the phase: a designator package imports as a GRID
    /// instead of being dropped with a "BGA package?" warning.
    #[test]
    fn a_wlcsp_package_imports_as_a_ball_grid() {
        let chips = convert_xml(WLCSP).expect("parses");
        let form = &chips[0].form;
        let grid = form.grid.as_ref().expect("balls become a grid");

        // F3 is the lowest row and A4 the rightmost column -> 6 rows, 4 columns.
        assert_eq!((grid.rows, grid.cols), (6, 4));
        assert_eq!(grid.cells.len(), 6);
        assert!(
            form.pins.iter().all(|side| side.is_empty()),
            "a WLCSP has no edge pins, so no side may be populated"
        );

        // Cells carry the designator's coordinates, 0-based.
        let cell = |name: &str| {
            grid.cells
                .iter()
                .find(|c| c.pin.name.starts_with(name))
                .unwrap_or_else(|| panic!("{name} missing"))
        };
        assert_eq!((cell("PB6").row, cell("PB6").col), (0, 1), "A2");
        assert_eq!((cell("PA13").row, cell("PA13").col), (1, 0), "B1");
        assert_eq!((cell("PF2").row, cell("PF2").col), (5, 2), "F3");

        // Numbers are ours, dense and in reading order — a WLCSP has none.
        let mut numbers: Vec<usize> = grid.cells.iter().map(|c| c.pin.number).collect();
        numbers.sort_unstable();
        assert_eq!(numbers, (1..=6).collect::<Vec<_>>());
        assert_eq!(cell("PB6").pin.number, 1, "first in reading order");

        // Signals are mapped exactly as they are for edge pins.
        assert!(
            cell("PB6")
                .pin
                .functions
                .iter()
                .any(|f| matches!(f, PinFunction::UsartTx(1))),
            "USART1_TX must survive the grid path"
        );
        assert!(cell("VDD").pin.reserved, "power pins stay reserved");
        assert!(
            chips[0].warnings.iter().any(|w| w.contains("6x4 grid")),
            "the import must say what it did: {:?}",
            chips[0].warnings
        );
    }

    /// The import-time check reads the feature back out of the generated line,
    /// so the two must agree — including after the user edits the line by hand.
    #[test]
    fn the_embassy_feature_is_recoverable_from_the_dependency_line() {
        let line = hal_dep_for("stm32h5", "STM32H5", "STM32H563ZITx");
        assert!(line.contains(EMBASSY_CRATE));
        assert!(
            line.contains(&format!("version = \"{EMBASSY_VERSION}\"")),
            "the version must come from the one constant: {line}"
        );
        assert_eq!(embassy_feature_in(&line), Some("stm32h563zi"));

        // A hand-edited line, with the fields in another order and extra spaces.
        assert_eq!(
            embassy_feature_in(
                "embassy-stm32 = { features = [ \"stm32h563zi\", \"defmt\" ], version = \"0.4\" }"
            ),
            Some("stm32h563zi")
        );
        // Lines that are not an embassy dependency, or carry no feature.
        assert_eq!(
            embassy_feature_in("stm32f1xx-hal = { features = [\"x\"] }"),
            None
        );
        assert_eq!(embassy_feature_in("embassy-stm32 = \"0.4\""), None);
        assert_eq!(
            embassy_feature_in("embassy-stm32 = { features = [\"\"] }"),
            None
        );
    }

    /// An edge-pin package must be completely unaffected by the grid path.
    #[test]
    fn a_numbered_package_still_has_no_grid() {
        let chips = convert_xml(F103).expect("parses");
        assert!(chips[0].form.grid.is_none());
        assert!(chips[0].form.pins.iter().any(|s| !s.is_empty()));
    }

    fn find<'a>(form: &'a McuForm, name: &str) -> &'a PinRow {
        form.pins
            .iter()
            .flatten()
            .find(|p| p.name == name)
            .unwrap_or_else(|| panic!("pin {name} not found"))
    }

    #[test]
    fn range_expands_into_two_flash_variants() {
        let chips = convert_xml(F103).unwrap();
        assert_eq!(chips.len(), 2);
        assert_eq!(chips[0].form.display_name, "STM32F103C8Tx");
        assert_eq!(chips[0].form.id, "stm32f103c8tx");
        assert_eq!(chips[0].form.flash_size, "64K");
        assert_eq!(chips[1].form.display_name, "STM32F103CBTx");
        assert_eq!(chips[1].form.flash_size, "128K");
        // Shared identity across variants.
        for c in &chips {
            assert_eq!(c.form.family, "stm32f1");
            assert_eq!(c.form.cpu, "Cortex-M3");
            assert_eq!(c.form.package, "LQFP48");
            assert_eq!(c.form.target, "thumbv7m-none-eabi");
            assert_eq!(c.form.ram_size, "20K");
            assert_eq!(c.form.flash_origin, "0x08000000");
            assert_eq!(c.form.clock, ClockChoice::Stm32f1);
        }
    }

    /// The datasheet frequency is captured when the vendor states one and left
    /// EMPTY when it does not — a third of the database (the whole C0 series)
    /// has no `<Frequency>`, and the header would rather show nothing than the
    /// family default, which is 72 MHz for every chip without its own graph.
    #[test]
    fn the_max_frequency_is_read_when_stated_and_absent_otherwise() {
        assert_eq!(
            convert_xml(F103).unwrap()[0].form.max_mhz,
            None,
            "this fixture states no frequency"
        );
        let with_freq = F103.replace(
            "<Core>Arm Cortex-M3</Core>",
            "<Core>Arm Cortex-M3</Core>
    <Frequency>72</Frequency>",
        );
        for chip in convert_xml(&with_freq).unwrap() {
            assert_eq!(chip.form.max_mhz, Some(72), "every variant of the range");
        }
    }

    #[test]
    fn signals_map_to_tokens_and_noise_is_dropped() {
        let form = &convert_xml(F103).unwrap()[0].form;
        // GPIO → in out; peripheral signals mapped; EXTI/SMBA/CHyN dropped.
        assert_eq!(find(form, "PA9").functions, "tim1_2 usart1_tx in out");
        assert_eq!(
            find(form, "PA11").functions,
            "can_rx tim1_4 usart1_cts usb_dm in out"
        );
        // Not-natively-modelled signals are CARRIED as generic `af:` tokens.
        assert_eq!(
            find(form, "PB6").functions,
            "i2c1_scl af:i2c1_smba tim4_1 in out"
        );
        assert_eq!(find(form, "PB13").functions, "spi2_sck tim1_1n in out");
        assert_eq!(find(form, "PA13").functions, "swdio in out");
        assert_eq!(find(form, "PA5").functions, "adc1_5 adc2_5 spi1_sck in out");
        // Power pin: reserved, no functions, name kept verbatim.
        let vbat = find(form, "VBAT");
        assert!(vbat.reserved);
        assert!(vbat.functions.is_empty());
        // Suffix stripped on the port pin.
        assert!(form.pins.iter().flatten().any(|p| p.name == "PC13"));
    }

    #[test]
    fn built_form_validates_and_builds() {
        for chip in convert_xml(F103).unwrap() {
            assert!(chip.form.errors().is_empty(), "{:?}", chip.form.errors());
            let def = chip.form.to_definition();
            assert!(def.build_mcu().iter_all_pins().count() >= 7);
        }
    }

    #[test]
    fn qfp_side_distribution_matches_the_bundled_layout() {
        // 8 pins → 2 per side: left 1-2, bottom 3-4, right 6-5, top 8-7.
        // The two reversed edges are the ones the package numbers against
        // the drawing direction - see `distribute_sides`.
        let rows: Vec<PinRow> = (1..=8)
            .map(|i| PinRow {
                number: i.to_string(),
                ..Default::default()
            })
            .collect();
        let [top, bottom, left, right] = distribute_sides(&rows);
        let nums = |s: &[PinRow]| s.iter().map(|r| r.number.clone()).collect::<Vec<_>>();
        assert_eq!(nums(&left), ["1", "2"]);
        assert_eq!(nums(&bottom), ["3", "4"]);
        // Right counts UP the edge, so top-to-bottom is 6 then 5. This
        // used to assert ["5", "6"] and matched an import that drew the
        // edge upside down.
        assert_eq!(nums(&right), ["6", "5"]);
        assert_eq!(nums(&top), ["8", "7"]);
    }

    /// The reported case, at its real size: an LQFP48 must read like the
    /// datasheet drawing, not like the pin table.
    #[test]
    fn an_lqfp48_right_edge_runs_from_36_down_to_25() {
        let rows: Vec<PinRow> = (1..=48)
            .map(|i| PinRow {
                number: i.to_string(),
                ..Default::default()
            })
            .collect();
        let [top, bottom, left, right] = distribute_sides(&rows);
        let first = |s: &[PinRow]| s.first().unwrap().number.clone();
        let last = |s: &[PinRow]| s.last().unwrap().number.clone();
        // Pin 1 top-left, counting down.
        assert_eq!((first(&left), last(&left)), ("1".into(), "12".into()));
        // Along the bottom, left to right.
        assert_eq!((first(&bottom), last(&bottom)), ("13".into(), "24".into()));
        // UP the right edge - so drawn top-to-bottom it is 36 first, 25 last.
        // This is the bug: it used to read 25 at the top.
        assert_eq!((first(&right), last(&right)), ("36".into(), "25".into()));
        // And LEFT along the top, so 48 is the leftmost.
        assert_eq!((first(&top), last(&top)), ("48".into(), "37".into()));
    }

    #[test]
    fn two_row_packages_lay_out_left_and_right_only() {
        // SO8N-style: 8 pins → left 1-4 (top→bottom), right 8-5 (top→bottom,
        // counting UP from the bottom), no top/bottom.
        let rows: Vec<PinRow> = (1..=8)
            .map(|i| PinRow {
                number: i.to_string(),
                ..Default::default()
            })
            .collect();
        let [top, bottom, left, right] = distribute_sides_2row(&rows);
        let nums = |s: &[PinRow]| s.iter().map(|r| r.number.clone()).collect::<Vec<_>>();
        assert!(top.is_empty() && bottom.is_empty());
        assert_eq!(nums(&left), ["1", "2", "3", "4"]);
        assert_eq!(nums(&right), ["8", "7", "6", "5"]);
    }

    #[test]
    fn two_row_package_detection() {
        for p in [
            "SO8N", "TSSOP20", "SOIC8", "SSOP28", "MSOP10", "DIP8", "SOT23-6",
        ] {
            assert!(is_two_row_package(p), "{p} should be two-row");
        }
        for p in ["LQFP64", "UFQFPN48", "UFBGA100", "WLCSP25", "TFBGA216"] {
            assert!(!is_two_row_package(p), "{p} should NOT be two-row");
        }
    }

    /// The two derivations a chip cannot be used without, on the naming ST uses
    /// for its newest parts.
    ///
    /// Both were wrong for STM32N6 and both fail LOUDLY only much later: the
    /// target triple as a wrong architecture, the feature as a Cargo.toml no
    /// resolver can satisfy. Verified against embassy's published list, where
    /// `stm32n645a0` and `stm32c071c8` exist while `stm32n645a0h` and
    /// `stm32c071c8t` do not.
    #[test]
    fn the_newest_part_names_derive_correctly() {
        // `x` + one more character: `N` for no-crystal, `Q` for secure.
        assert_eq!(embassy_chip_feature("STM32N645A0HxQ"), "stm32n645a0");
        assert_eq!(embassy_chip_feature("STM32C071C8TxN"), "stm32c071c8");
        // The plain form is untouched.
        assert_eq!(embassy_chip_feature("STM32F103C8Tx"), "stm32f103c8");
        assert_eq!(embassy_chip_feature("STM32WL30KBVx"), "stm32wl30kb");

        // Armv8.1-M has no stable triple; v8-M Main is what it builds as.
        assert_eq!(
            core_to_target("Arm Cortex-M55"),
            "thumbv8m.main-none-eabihf"
        );
        assert_eq!(
            core_to_target("Arm Cortex-M85"),
            "thumbv8m.main-none-eabihf"
        );
        // And it must not fall through to the M3 default any more.
        assert_ne!(core_to_target("Arm Cortex-M55"), "thumbv7m-none-eabi");
    }

    #[test]
    fn helpers_behave() {
        assert_eq!(clean_pin_name("PC13-TAMPER-RTC"), "PC13");
        // The STM32Cube database's spelling of the same idea.
        assert_eq!(clean_pin_name("PB3 (JTDO-TRACESWO)"), "PB3");
        assert_eq!(clean_pin_name("PB4 (NJTRST)"), "PB4");
        assert_eq!(clean_pin_name("PA13 (JTMS-SWDIO)"), "PA13");
        assert_eq!(clean_pin_name("PA0-WKUP"), "PA0");
        assert_eq!(clean_pin_name("VBAT"), "VBAT");
        assert_eq!(clean_pin_name("NRST"), "NRST");
        // Exposed thermal pad — not a pin; a numbered VSS still is.
        assert!(is_exposed_pad("exposed pad VSS"));
        assert!(is_exposed_pad("EPAD"));
        assert!(is_exposed_pad("Thermal-Pad"));
        assert!(is_exposed_pad("PAD"));
        assert!(!is_exposed_pad("VSS"));
        assert!(!is_exposed_pad("VSSA"));
        assert!(!is_exposed_pad("PA0"));
        assert_eq!(map_signal("USART2_RX").as_deref(), Some("usart2_rx"));
        assert_eq!(map_signal("UART4_TX").as_deref(), Some("usart4_tx")); // UART→usart
        assert_eq!(map_signal("ADC123_IN10").as_deref(), Some("adc1_10")); // combined→first
        // Anything the IDE doesn't model natively is CARRIED as a generic
        // alternate function — never dropped.
        assert_eq!(map_signal("FMC_A0").as_deref(), Some("af:fmc_a0"));
        assert_eq!(map_signal("SAI1_SD_A").as_deref(), Some("sai1_a_sd"));
        assert_eq!(map_signal("DCMI_D3").as_deref(), Some("af:dcmi_d3"));
        assert_eq!(map_signal("QUADSPI_CLK").as_deref(), Some("qspi_clk"));
        assert_eq!(map_signal("TIM1_CH1N").as_deref(), Some("tim1_1n"));
        // The break inputs, per index …
        assert_eq!(map_signal("TIM1_BKIN").as_deref(), Some("tim1_bkin1"));
        assert_eq!(map_signal("TIM1_BKIN2").as_deref(), Some("tim1_bkin2"));
        // …but a comparator routed to break is a different peripheral path and
        // stays a generic AF signal.
        assert_eq!(
            map_signal("TIM1_BKIN_COMP1").as_deref(),
            Some("af:tim1_bkin_comp1")
        );
        assert_eq!(map_signal("TIM1_ETR").as_deref(), Some("af:tim1_etr"));
        assert_eq!(map_signal("I2C1_SMBA").as_deref(), Some("af:i2c1_smba"));
        // Only the EXTI TRIGGER lines and EVENTOUT are dropped…
        assert_eq!(map_signal("EVENTOUT"), None);
        assert_eq!(map_signal("ADC1_EXTI11"), None);
        assert_eq!(map_signal("DAC1_EXTI9"), None);
        // …and "contains EXTI" is not the rule: only `_EXTI<digits>`.
        assert_eq!(
            map_signal("SYS_EXTI_MUX").as_deref(),
            Some("af:sys_exti_mux")
        );
        // Everything a datasheet lists as a pin function SURVIVES. These were
        // dropped by the old prefix filter, which is why a pin showed three
        // functions where CubeMX showed eleven.
        assert_eq!(map_signal("RTC_TS").as_deref(), Some("af:rtc_ts"));
        assert_eq!(map_signal("RTC_TAMP1").as_deref(), Some("af:rtc_tamp1"));
        assert_eq!(
            map_signal("RTC_OUT_ALARM").as_deref(),
            Some("af:rtc_out_alarm")
        );
        assert_eq!(map_signal("SYS_WKUP2").as_deref(), Some("af:sys_wkup2"));
        assert_eq!(map_signal("SYS_PVD_IN").as_deref(), Some("af:sys_pvd_in"));
        assert_eq!(map_signal("SYS_TRACED0").as_deref(), Some("af:sys_traced0"));
        assert_eq!(map_signal("RCC_OSC_IN").as_deref(), Some("af:rcc_osc_in"));
        assert_eq!(map_signal("SYS_JTDI").as_deref(), Some("af:sys_jtdi"));
        // Natives still win over the fallback.
        assert_eq!(map_signal("SYS_JTMS-SWDIO").as_deref(), Some("swdio"));
        assert_eq!(map_signal("DEBUG_JTCK-SWCLK").as_deref(), Some("swclk"));
        assert_eq!(map_signal("RCC_MCO").as_deref(), Some("mco"));
        // Grammar extension: LPUART / RTS_DE / SPI_RDY are no longer dropped.
        assert_eq!(map_signal("LPUART1_TX").as_deref(), Some("lpuart1_tx"));
        assert_eq!(map_signal("LPUART1_RTS_DE").as_deref(), Some("lpuart1_rts"));
        assert_eq!(map_signal("USART2_RTS_DE").as_deref(), Some("usart2_rts"));
        assert_eq!(map_signal("SPI1_RDY").as_deref(), Some("spi1_rdy"));
        assert_eq!(map_signal("SPI3_RDY").as_deref(), Some("spi3_rdy"));
        assert_eq!(
            core_to_target("Arm Cortex-M33"),
            "thumbv8m.main-none-eabihf"
        );
        assert_eq!(core_to_target("Arm Cortex-M0+"), "thumbv6m-none-eabi");
        assert_eq!(
            expand_variants("STM32F103CBTx"),
            vec![("STM32F103CBTx".to_string(), 0)]
        );
        assert_eq!(embassy_chip_feature("STM32F411RETx"), "stm32f411re");
        assert_eq!(embassy_chip_feature("STM32G0B1RETx"), "stm32g0b1re");
    }

    #[test]
    fn hal_dep_picks_the_right_crate_per_family() {
        // F1 keeps stm32f1xx-hal; the F103 fixture proves it end-to-end.
        assert!(
            convert_xml(F103).unwrap()[0]
                .form
                .hal_dep
                .contains("stm32f1xx-hal")
        );
        // Any other STM32 family → embassy-stm32 with the chip feature.
        let g0 = hal_dep_for("stm32g0", "STM32G0B1", "STM32G0B1RETx");
        assert!(g0.contains("embassy-stm32"), "{g0}");
        assert!(g0.contains("\"stm32g0b1re\""), "{g0}");
        // Non-STM32 falls back to a TODO the user completes.
        assert!(hal_dep_for("rp2040", "", "RP2040").contains("TODO"));
    }

    #[test]
    fn hal_dep_from_name_recovers_the_f1_device_feature() {
        // F1: the device feature is the stm32f1NN line pulled from the part
        // number (the AI / auto-fill path has no XML <Line> attribute).
        let f1 = hal_dep_for_name("stm32f1", "STM32F103RBT6");
        assert!(f1.contains("stm32f1xx-hal"), "{f1}");
        assert!(f1.contains("\"stm32f103\""), "{f1}");
        // Other STM32 families → embassy-stm32 with the per-chip feature.
        let g0 = hal_dep_for_name("stm32g0", "STM32G0B1RETx");
        assert!(
            g0.contains("embassy-stm32") && g0.contains("\"stm32g0b1re\""),
            "{g0}"
        );
        // An F1 family with an unusable name → editable TODO, never `["", "rt"]`.
        let bad = hal_dep_for_name("stm32f1", "STM32");
        assert!(bad.contains("TODO") && !bad.contains("\"\""), "{bad}");
        // Still a dependency line naming the crate, so a runtime switch has a
        // crate to swap back to.
        assert!(bad.starts_with("stm32f1xx-hal = {"), "{bad}");
        assert!(!bad.contains("  "), "a joined continuation: {bad}");
    }

    #[test]
    fn rejects_non_mcu_xml() {
        assert!(convert_xml("<Root/>").is_err());
        assert!(convert_xml("not xml at all <<<").is_err());
    }
}

#[cfg(test)]
mod gpio_mode_tests {
    use super::gpio_mode_tokens;

    /// Only `Analog` adds a token: Input/Output already come from mapping the
    /// `GPIO` signal itself, and EXTI/EVENTOUT are not functions here.
    #[test]
    fn only_analog_becomes_a_token() {
        assert_eq!(
            gpio_mode_tokens(Some("Input,Output,Analog,EXTI")).as_deref(),
            Some("analog")
        );
        assert_eq!(
            gpio_mode_tokens(Some("Input,Output,Analog,EVENTOUT,EXTI")).as_deref(),
            Some("analog")
        );
        // A pin that is not analog-capable gains nothing.
        assert_eq!(gpio_mode_tokens(Some("Input,Output,EXTI")), None);
        // Absent attribute (older files, other signals) is not an error.
        assert_eq!(gpio_mode_tokens(None), None);
        // Spacing and case as they might appear in a hand-edited file.
        assert_eq!(
            gpio_mode_tokens(Some("Input, output , ANALOG")).as_deref(),
            Some("analog")
        );
    }
}

#[cfg(test)]
mod f1_async_line_tests {
    use super::{EMBASSY_VERSION, f1_embassy_hal_dep};

    /// The F1's async line is its part number's first eleven characters, from
    /// either name a definition carries - the probe name first.
    #[test]
    fn the_f1_async_line_is_the_part_number() {
        let want = format!(
            "embassy-stm32 = {{ version = \"{EMBASSY_VERSION}\", features = [\"stm32f103c8\"] }}"
        );
        for (probe, pkg) in [
            ("STM32F103C8", "stm32f103c8t6"),
            ("STM32F103C8Tx", ""),
            ("", "stm32f103c8t6"),
            ("not a part", "stm32f103c8t6"),
        ] {
            assert_eq!(
                f1_embassy_hal_dep(probe, pkg).as_deref(),
                Some(want.as_str()),
                "{probe} / {pkg}"
            );
        }
        let cb = f1_embassy_hal_dep("STM32F103CBTx", "").expect("an F1 part");
        assert!(cb.contains("[\"stm32f103cb\"]"), "{cb}");
    }

    /// Neither a truncated F1 name, a generic one, nor another family's part
    /// yields a line.
    #[test]
    fn anything_else_is_no_line() {
        for bad in [
            "",
            "STM32F103",
            "STM32F411RE",
            "STM32F1",
            "ESP32C3",
            "STM32F103xB",  // CMSIS's density name, not a part
            "STM32F103C9",  // no such flash size
            "STM32F103KBU", // no such pin count
        ] {
            assert_eq!(f1_embassy_hal_dep(bad, bad), None, "{bad}");
        }
        // Every pin count and flash size the F1 parts use is accepted.
        for good in ["STM32F100RB", "STM32F101T4", "STM32F105VC", "STM32F103ZG"] {
            assert!(f1_embassy_hal_dep(good, "").is_some(), "{good}");
        }
    }

    /// The fallback is still an embassy-stm32 line - so a runtime switch swaps
    /// the crate - naming the part and what to fill in.
    #[test]
    fn an_unreadable_f1_gets_an_embassy_line_to_finish() {
        let line = super::f1_embassy_hal_dep_todo("STM32F103xB");
        assert!(line.starts_with("embassy-stm32 = {"), "{line}");
        assert!(
            line.contains(&format!("version = \"{EMBASSY_VERSION}\"")),
            "{line}"
        );
        assert!(
            line.contains("# TODO: the chip feature for STM32F103xB"),
            "{line}"
        );
        assert!(!line.contains("  "), "a joined continuation: {line}");
    }
}

#[cfg(test)]
mod bank_feature_tests {
    use super::{hal_dep_for_name, needs_bank_feature};

    /// The three shapes, on real parts checked against `stm32-metapac` 21.
    #[test]
    fn only_the_parts_with_two_memory_configurations_ask_for_one() {
        // F4: the 1 MB parts are the only ones that can be either.
        assert!(needs_bank_feature("STM32F429ZGTx"), "1 MB F429 is a choice");
        assert!(
            !needs_bank_feature("STM32F429ZITx"),
            "2 MB F429 is dual only"
        );
        assert!(
            !needs_bank_feature("STM32F429ZETx"),
            "512 KB F429 is single only"
        );
        // F7: every size of the affected lines.
        assert!(needs_bank_feature("STM32F767ZITx"));
        assert!(needs_bank_feature("STM32F767ZGTx"));
        // Neighbouring lines that are single-bank throughout.
        assert!(!needs_bank_feature("STM32F746ZGTx"));
        assert!(!needs_bank_feature("STM32F411RETx"));
        assert!(!needs_bank_feature("STM32F217ZETx"));
        // A truncated or odd name must not panic or guess.
        assert!(!needs_bank_feature("STM32F4"));
        assert!(!needs_bank_feature(""));
    }

    /// The four families the rule missed until an STM32G474 project turned out
    /// not to build at all. Same three shapes, same source: memory
    /// configurations counted across `stm32-metapac` 21.
    #[test]
    fn the_g0_g4_l4_and_l5_parts_ask_for_one_too() {
        // G4: whole lines, every size — including the 128 KB G474RB that
        // exposed this.
        for part in [
            "STM32G474RBTx",
            "STM32G474RETx",
            "STM32G473CBTx",
            "STM32G484QETx",
        ] {
            assert!(needs_bank_feature(part), "{part}");
        }
        // …but not the G4 lines that are single-bank throughout.
        for part in ["STM32G431CBTx", "STM32G441CBTx", "STM32G491RETx"] {
            assert!(!needs_bank_feature(part), "{part}");
        }
        // G0: only the 512 KB size of two lines.
        assert!(needs_bank_feature("STM32G0B1RCTx"));
        assert!(needs_bank_feature("STM32G0C1RCTx"));
        assert!(!needs_bank_feature("STM32G0B1RETx"), "256 KB is fixed");
        assert!(!needs_bank_feature("STM32G0B1RBTx"), "128 KB is fixed");
        assert!(!needs_bank_feature("STM32G071RBTx"));
        // L4+: whole lines. Plain L4 is untouched.
        assert!(needs_bank_feature("STM32L4R5ZITx"));
        assert!(needs_bank_feature("STM32L4S9ZITx"));
        assert!(!needs_bank_feature("STM32L476RGTx"));
        assert!(!needs_bank_feature("STM32L432KCUx"));
        // L5: L552 only at 512 KB, L562 always.
        assert!(needs_bank_feature("STM32L552ZETx"));
        assert!(!needs_bank_feature("STM32L552CCTx"), "256 KB is fixed");
        assert!(needs_bank_feature("STM32L562QEIx"));
    }

    /// The line an STM32G474 gets — the exact string that was missing it.
    #[test]
    fn the_g474_line_carries_the_feature() {
        let g474 = hal_dep_for_name("stm32g4", "STM32G474RBTx");
        assert!(g474.contains("\"stm32g474rb\", \"single-bank\""), "{g474}");
    }

    #[test]
    fn the_feature_reaches_the_dependency_line() {
        let f767 = hal_dep_for_name("stm32f7", "STM32F767ZITx");
        assert!(f767.contains("\"stm32f767zi\", \"single-bank\""), "{f767}");
        // …and nothing changes for a part that does not need it, so the whole
        // existing corpus of generated projects is untouched.
        let f746 = hal_dep_for_name("stm32f7", "STM32F746ZGTx");
        assert!(f746.contains("features = [\"stm32f746zg\"]"), "{f746}");
        assert!(!f746.contains("bank"), "{f746}");
    }
}

#[cfg(test)]
mod bonded_pin_tests {
    use super::*;

    /// The exact shape ST publishes for an STM32G030F6Px (TSSOP20): pin 1 is
    /// PB7 *and* PB8, pin 2 is PC14 *and* PB9. Two `<Pin>` elements, one
    /// package pin each time.
    const G030: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Mcu Family="STM32G0" Line="STM32G0x0 Value line" Package="TSSOP20" RefName="STM32G030F6Px" xmlns="http://dummy.com">
    <Core>ARM Cortex-M0+</Core>
    <Ram>8</Ram>
    <Flash>32</Flash>
    <Pin Name="PB7" Position="1" Type="I/O">
        <Signal Name="I2C1_SDA"/>
        <Signal Name="USART1_RX"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="PB8" Position="1" Type="I/O">
        <Signal Name="I2C1_SCL"/>
        <Signal Name="GPIO"/>
    </Pin>
    <Pin Name="VDD" Position="4" Type="Power"/>
</Mcu>"#;

    fn g030() -> ConvertedChip {
        convert_xml(G030)
            .expect("the G030 XML must parse")
            .into_iter()
            .next()
            .expect("one variant")
    }

    /// The bug: two rows shared position 1, and the form rejected the chip with
    /// "Pin number 1 is used more than once". 171 of 2240 published chips -
    /// most of the small-package G0 and C0 range - could not be imported.
    #[test]
    fn a_bonded_package_pin_imports_as_one_pin() {
        let chip = g030();
        assert!(
            chip.form.errors().is_empty(),
            "must validate: {:?}",
            chip.form.errors()
        );
        let ones: Vec<_> = chip
            .form
            .pins
            .iter()
            .flatten()
            .filter(|r| r.number == "1")
            .collect();
        assert_eq!(ones.len(), 1, "one package pin, one row");
    }

    /// Nothing is lost in the fold: the pin offers what BOTH pads offer.
    #[test]
    fn it_keeps_every_function_from_both_pads() {
        let chip = g030();
        let row = chip
            .form
            .pins
            .iter()
            .flatten()
            .find(|r| r.number == "1")
            .expect("pin 1");
        // The richer pad keeps the name.
        assert_eq!(row.name, "PB7");
        for want in ["i2c1_sda", "usart1_rx", "i2c1_scl"] {
            assert!(
                row.functions.split_whitespace().any(|t| t == want),
                "missing {want} in {:?}",
                row.functions
            );
        }
    }

    /// …and the sibling's functions carry their owner, which is the whole point:
    /// picking I2C1_SCL on this pin has to generate `p.PB8`, not `p.PB7`.
    #[test]
    fn a_siblings_function_records_which_gpio_provides_it() {
        let chip = g030();
        let row = chip
            .form
            .pins
            .iter()
            .flatten()
            .find(|r| r.number == "1")
            .expect("pin 1");
        assert_eq!(
            row.fn_owner
                .iter()
                .find(|(tok, _): &&(String, String)| tok == "i2c1_scl")
                .map(|(_, g)| g.as_str()),
            Some("PB8")
        );
        // Functions the primary already had are NOT tagged: either pad drives
        // the same package pin, so the primary answers for them.
        assert!(
            !row.fn_owner
                .iter()
                .any(|(tok, _): &(String, String)| tok == "usart1_rx"),
            "the primary's own function must not be overridden: {:?}",
            row.fn_owner
        );
        assert!(
            !row.fn_owner
                .iter()
                .any(|(tok, _): &(String, String)| tok.starts_with("in")),
            "GPIO in/out is on both pads: {:?}",
            row.fn_owner
        );
    }

    /// The user is told, rather than left to wonder why one pin lists two
    /// GPIOs' worth of functions.
    #[test]
    fn the_import_report_mentions_the_bonded_pins() {
        assert!(
            g030()
                .warnings
                .iter()
                .any(|w| w.contains("bonded together")),
            "{:?}",
            g030().warnings
        );
    }
}

#[cfg(test)]
mod bonded_pin_corpus {
    use super::*;

    /// Sweep the real vendor corpus: every chip must import, and no package
    /// position may appear twice. Ignored because it needs the STM32
    /// open-pin-data checkout; point `EIDE_PIN_DATA` at its `mcu/` folder.
    #[test]
    #[ignore = "needs the STM32_open_pin_data checkout"]
    fn every_published_chip_imports() {
        let Ok(dir) = std::env::var("EIDE_PIN_DATA") else {
            eprintln!("set EIDE_PIN_DATA to the open-pin-data mcu/ folder");
            return;
        };
        let (mut ok, mut bad, mut bonded) = (0usize, Vec::new(), 0usize);
        for e in std::fs::read_dir(&dir)
            .expect("read the pin-data folder")
            .flatten()
        {
            let path = e.path();
            if path.extension().and_then(|x| x.to_str()) != Some("xml") {
                continue;
            }
            let Ok(xml) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok(chips) = convert_xml(&xml) else {
                continue;
            };
            for c in chips {
                let errs = c.form.errors();
                if errs.is_empty() {
                    ok += 1;
                } else {
                    bad.push(format!("{}: {}", c.form.display_name, errs[0]));
                }
                if c.warnings.iter().any(|w| w.contains("bonded together")) {
                    bonded += 1;
                }
            }
        }
        println!(
            "imported {ok}, {bonded} with bonded pins, {} rejected",
            bad.len()
        );
        for b in bad.iter().take(20) {
            println!("  {b}");
        }
        // This test is about bonded pins, so that is what it asserts. The rest
        // of the corpus is printed, not enforced: at the time of writing 11
        // STM32H5E4/H5E5 parts are rejected for an EMPTY <Flash> element in
        // ST's own XML - a different defect, and turning it into a failure here
        // would make this test go red for a reason it does not describe.
        let dupes: Vec<_> = bad
            .iter()
            .filter(|b| b.contains("used more than once"))
            .collect();
        assert!(
            dupes.is_empty(),
            "{} chip(s) still rejected for a duplicate pin position: {dupes:?}",
            dupes.len()
        );
    }
}

#[cfg(test)]
mod usart_ip_tests {
    use super::*;

    /// The rule is checked against `stm32-metapac`'s own metadata: the CubeMX IP
    /// version on the left, the `usart_vN` metapac assigns on the right. Note
    /// that the prefix alone is NOT the answer — an F303 is `sci2_v2_1` and has
    /// the bits, an F411 is `sci2_v1_2` and does not.
    #[test]
    fn the_ip_version_says_whether_swap_invert_exist() {
        for (ip, metapac, want) in [
            ("sci2_v1_1_Cube", "v1 (F103)", false),
            ("sci2_v1_2_Cube", "v2 (F411)", false),
            ("sci2_v2_1_Cube", "v3 (F303)", true),
            ("sci2_v2_2_Cube", "v3 (F030)", true),
            ("sci3_v1_1_Cube", "v3 (L432)", true),
            ("sci3_v2_0_Cube", "v4 (H743/WBA/U5)", true),
            ("sci3_v2_1_Cube", "v4 (G071/WLE5)", true),
        ] {
            assert_eq!(usart_has_swap_invert(Some(ip)), want, "{ip} is {metapac}");
        }
        // Unknown provenance answers NO: refusing an option is recoverable,
        // emitting a field that isn't there is a compile error downstream.
        assert!(!usart_has_swap_invert(None));
    }

    #[test]
    fn the_version_is_read_off_the_chip_xml() {
        let xml = r#"<Mcu RefName="STM32G071RBTx" Family="STM32G0">
            <IP InstanceName="GPIO" Name="GPIO" Version="STM32G0xx_gpio_v1_0"/>
            <IP InstanceName="USART1" Name="USART" Version="sci3_v2_1_Cube"/>
        </Mcu>"#;
        assert_eq!(usart_ip_version(xml).as_deref(), Some("sci3_v2_1_Cube"));
        // A chip file without a USART block simply has no answer.
        assert_eq!(usart_ip_version(r#"<Mcu RefName="X"/>"#), None);
    }
}
