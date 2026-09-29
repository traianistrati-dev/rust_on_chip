//! Data-driven RCC clock-block emitter (Layer 2).
//!
//! `f4.rs` and `wba.rs` emitted almost the same `embassy_stm32::Config` block —
//! ~85% identical text, differing only in a handful of family quirks (which PLL
//! output, the HSE syntax, the extra APB bus, the voltage-scale line). This
//! captures those quirks as a [`RccDescriptor`] and emits from ONE generic
//! function, so adding a family is a descriptor, not another copy of the string
//! builder.
//!
//! The GRAPH → values step stays per-family (`graph_to_f4` / `graph_to_wba`):
//! it is already identical across families (same node ids, same readers), and
//! keeping it lets both funnel through [`RccValues`] here. Correctness is pinned
//! by the existing `f4`/`wba` codegen tests — this must reproduce their exact
//! output byte-for-byte.

use super::super::clock::graph::model::{ClockGraph, NodeKind, NodeState};
use super::super::clock::model::ClockConfig;
use super::family::is_esp;

/// embassy's reset default (HSI, everything /1) — the clock line for a family
/// with no RCC recipe, or whose graph selections happen to equal the reset.
const EMBASSY_RESET_INIT: &str = "    let p = embassy_stm32::init(Default::default()); // reset clock (HSI). \
     Set embassy_stm32::Config for RCC if needed.\n";

/// The RCC recipe (how to READ the graph + how to EMIT the config) for an STM32
/// `family` key, if one exists. **This is the data that replaced the old
/// `is_f4_graph` / `is_wba_graph` topology sniffing**: the chip's FAMILY selects
/// the codegen — which is physically true (every STM32F4 shares the F4 RCC) —
/// so an unusual graph shape can't mis-route it and a new family is just one arm
/// here plus its [`ReadSpec`] / [`RccDescriptor`] constructors.
pub fn rcc_recipe(family: &str) -> Option<(ReadSpec, RccDescriptor)> {
    match family {
        // F2 / F4 / F7 share embassy's `rcc/f247.rs` (same Config/Pll/Sysclk),
        // so they share ONE recipe — families on a common RCC module cost only
        // an extra pattern here.
        //
        // What they do NOT share is the PLLN window, and an earlier note here
        // claimed otherwise ("the shipped 50-432 window is valid for all three,
        // verified vs metapac rcc_f2/f7"). It is not: metapac's `rcc_f2` block
        // has `MUL192..=MUL432` where `rcc_f4`/`rcc_f7` have `MUL2..=MUL432`,
        // and `PllMul` IS that PAC enum. An F2 project with N=144 therefore
        // failed to compile on a name that does not exist.
        "stm32f2" => Some((
            ReadSpec::f4(),
            RccDescriptor::f4().with_pll_n(super::super::clock::graph::F2_PLL_N),
        )),
        "stm32f4" | "stm32f7" => Some((ReadSpec::f4(), RccDescriptor::f4())),
        "stm32g0" => Some((ReadSpec::g0(), RccDescriptor::g0())),
        "stm32g4" => Some((ReadSpec::g4(), RccDescriptor::g4())),
        "stm32l4" => Some((ReadSpec::l4(), RccDescriptor::l4())),
        "stm32wba" => Some((ReadSpec::wba(), RccDescriptor::wba())),
        _ => None,
    }
}

/// The graph node ids this family's code generation READS, by id.
///
/// The clock editor needs this: nodes are addressed by id everywhere, so
/// renaming or deleting one of these silently changes (or defaults) what lands
/// in `main.rs` — a failure with no error message anywhere. The editor marks
/// them and reports the ones that have gone missing.
///
/// Kept next to the readers that consume them, so a new family's ids arrive with
/// its [`ReadSpec`] instead of drifting in a list elsewhere.
pub fn codegen_node_ids(family: &str) -> Vec<&'static str> {
    match family {
        // Its own HAL: `graph_to_stm32f1` reads the whole F103 tree.
        // (`pll_input` is NOT here: it is a field of the computed frequencies,
        // not a node `graph_to_stm32f1` looks up — listing it made the editor
        // demand a binding for something code generation never reads.)
        "stm32f1" => vec![
            "hse", "pllsrc", "pllxtpre", "pllmul", "sw", "ahb", "apb1", "apb2", "adc", "usb",
            "systick", "rtc", "mco",
        ],
        // esp-hal exposes only the CPU clock — on every Espressif part, not
        // just the C3 that was once the only one here. Left as a single name,
        // the eight parts added later fell through to the STM32 branch below,
        // which is gated on `starts_with("stm32")` and so gave them NOTHING:
        // no id to bind, so nothing renamed their `cpu` node, so the editor
        // protected a name code generation actually reads.
        f if is_esp(f) => vec!["cpu"],
        // The Pico tree, as the backend actually reads it: the crystal, and each
        // PLL's FBDIV / POSTDIV1 / POSTDIV2. Those three are what a `PLLConfig`
        // is made of, so renaming any of them in the editor would silently
        // change the frequency the chip boots at.
        // The nRF tree as `nrf::clock_choice` reads it: each mux's selected
        // edge is followed back to its source, so the two muxes AND the
        // sources they can land on are all names code generation depends on.
        f if crate::panels::mcu_module::codegen::nrf::is_nrf(f) => vec![
            "hfclk_src",
            "lfclk_src",
            "hfint",
            "hfxo",
            "hfxo_pll",
            "lfrc",
            "lfsynth",
        ],
        f if crate::panels::mcu_module::codegen::rp::is_rp(f) => vec![
            "xosc",
            "pll_sys_fb",
            "pll_sys_pd1",
            "pll_sys_pd2",
            "pll_usb_fb",
            "pll_usb_pd1",
            "pll_usb_pd2",
        ],
        _ => match rcc_recipe(family) {
            Some((spec, _)) => {
                let mut ids = vec![
                    "hse",
                    "sw",
                    "pllsrc",
                    "pllm",
                    "plln",
                    "ahb",
                    spec.pll_out_node,
                ];
                ids.extend(spec.apb.iter().map(|(_, node)| *node));
                ids
            }
            // No family recipe — but [`generic_recipe`] reads a tree that
            // carries the canonical spine, so these ids are exactly as live as
            // a recipe's.
            //
            // Returning nothing here used to make that unreachable in the one
            // case it was written for: an imported vendor tree carries names
            // like `SysClkSource`, and with no ids to bind, nothing renamed
            // them, so the generic recipe found no `sw` and the chip generated
            // no clock code at all. A 157-node diagram that drove nothing.
            //
            // Both PLL output dividers are listed because the tree decides the
            // embassy spelling: whichever of `pllr` / `pllp` the vendor tree
            // actually has is the one that binds, and `generic_recipe` reads the
            // shape off that.
            //
            // STM32 only: what the generic recipe emits is `embassy_stm32`
            // config, so offering these ids for a family that will never take
            // that path would ask the editor to protect names nothing reads.
            None if family.starts_with("stm32") => vec![
                "hsi", "hse", "sw", "pllsrc", "pllm", "plln", "pllp", "pllr", "ahb", "apb1", "apb2",
            ],
            None => Vec::new(),
        },
    }
}

// The APB bus sets a generic tree can have. `ReadSpec::apb` is `&'static`, so
// the choice is between fixed slices rather than a built vector.
const APB_1: &[(&str, &str)] = &[("apb1_pre", "apb1")];
const APB_12: &[(&str, &str)] = &[("apb1_pre", "apb1"), ("apb2_pre", "apb2")];
const APB_127: &[(&str, &str)] = &[
    ("apb1_pre", "apb1"),
    ("apb2_pre", "apb2"),
    ("apb7_pre", "apb7"),
];

/// A recipe derived from the TREE, for a family this IDE has no verified one
/// for.
///
/// The reason it can exist at all: every embassy STM32 `Config::rcc` is built
/// the same way — an HSE, a `Pll { prediv, mul, div* }`, a `Sysclk`, an
/// `ahb_pre` and one APB prescaler per bus. What differs between families is
/// which of two spellings they use, and the tree says which: a graph that names
/// its PLL output `pllr` is the modern nested-`Pll { source, … }` shape
/// (G0/G4/L4/L5/U5/H5/C0/WBA…), one that names it `pllp` is the F2/F4/F7 shape
/// with a separate `config.rcc.pll_src`. The bus count comes from the tree too.
///
/// Returns `None` unless the tree really carries that spine — an empty or
/// foreign graph has nothing to read, and guessing at it would emit a clock the
/// user never described. That case keeps [`hand_written_skeleton`].
///
/// This is a GUESS at the API surface, not at the user's intent: the numbers are
/// exactly what the Clock tab shows. Where the guess is wrong the project fails
/// to compile on a field name, which the emitted note says how to fix — versus
/// the previous behaviour, where a configured tree produced no clock code at all
/// and said nothing.
pub fn generic_recipe(g: &ClockGraph) -> Option<(ReadSpec, RccDescriptor)> {
    let has = |id: &str| g.node(id).is_some();
    if !(has("sw") && has("ahb")) {
        return None;
    }
    // Which PLL output divider the tree names decides the embassy shape — and
    // NAMING NONE is itself an answer, not a failure: on F0/F1/F3 the PLL
    // multiplies and stops. Treating that as "no match" is what left an
    // imported STM32F358 tree generating nothing at all.
    let (mut spec, desc) = if has("pllr") {
        (ReadSpec::g4(), RccDescriptor::g4())
    } else if has("pllp") {
        (ReadSpec::f4(), RccDescriptor::f4())
    } else {
        (ReadSpec::f013(), RccDescriptor::f013())
    };
    // The internal RC is 8 MHz on some families and 16 on others, and the tree
    // knows which. It only reaches the header comment — embassy computes the
    // real frequency itself — but a comment that says 16 MHz on an 8 MHz part
    // is a comment that will be believed.
    if let Some(NodeState::Source { hz, .. }) = g.node("hsi").map(|n| &n.state) {
        spec.hsi_hz = *hz;
        if spec.reset.sys == SysSource::Hsi {
            spec.reset.sysclk_hz = *hz;
        }
    }
    spec.apb = if has("apb7") {
        APB_127
    } else if has("apb2") {
        APB_12
    } else {
        APB_1
    };
    spec.reset.apb = spec.apb.iter().map(|(field, _)| (*field, 1)).collect();
    // Nothing here knows the family's power-on default, so the
    // `init(Default::default())` shortcut is never taken: an explicit block
    // states what the tree says instead of assuming the silicon agrees.
    spec.reset_is_hw_default = false;
    Some((spec, desc))
}

/// Emit the RCC clock block for an embassy STM32 `family` from its Clock-tab
/// graph. A family with no recipe falls back to [`generic_recipe`] read off the
/// tree, and only a tree that cannot be read at all gets the hand-written
/// skeleton. Replaces the per-family `f4::clock_block` / `wba::clock_block` and
/// the `stm_clock_block` sniffing with one dispatch.
pub fn graph_clock_block(family: &str, clock: &ClockConfig, manual: bool) -> String {
    graph_clock_block_for("", family, clock, manual)
}

/// The same, told WHICH PART it is generating for.
///
/// The part number only matters to the peripheral clock selectors, whose
/// register block varies inside a family — see
/// [`rcc_mux::fields_for_chip`](super::rcc_mux::fields_for_chip). Everything
/// else is family-wide, which is why the shorter form above still exists.
pub fn graph_clock_block_for(
    chip: &str,
    family: &str,
    clock: &ClockConfig,
    manual: bool,
) -> String {
    // `for_codegen` applies the chip's id bindings; with none declared it
    // borrows the graph unchanged, so the emitted block is byte-identical.
    let graph = match clock {
        ClockConfig::Graph(gc) => Some(gc.for_codegen()),
        _ => None,
    };
    let graph = graph.as_deref();
    // The F1 reaches this function only on the Async runtime - Blocking, Native
    // and RTIC write stm32f1xx-hal's `rcc.cfgr` chain instead. Its tree goes
    // through the F1's own reader, the one that chain uses.
    if family == "stm32f1" {
        return wrap(f1_embassy_block(chip, &f1_embassy_clock(clock)), manual);
    }
    // N6 before either: it has its own emitter because four PLLs, twenty IC
    // dividers and a separate CPU clock do not fit the single-PLL descriptor,
    // and the generic recipe cannot read its tree at all (no `sw`, no `ahb`).
    if family == "stm32n6"
        && let Some(g) = graph
        && let Some(block) = super::rcc_n6::block(g)
    {
        return wrap(block, manual);
    }
    // The family's verified recipe first; the tree's own shape only as a
    // fallback, so no existing family's output can change.
    let (spec, desc, verified) = match rcc_recipe(family) {
        Some((spec, desc)) => (spec, desc, true),
        None => match graph.and_then(generic_recipe) {
            Some((spec, desc)) => (spec, desc, false),
            None => return wrap(hand_written_skeleton(), manual),
        },
    };
    let values = match graph {
        Some(g) => read_rcc_values(g, &spec),
        None => spec.reset.clone(),
    };
    // Per-peripheral kernel clocks: USART1SEL, I2C1SEL, ADCSEL and the rest.
    // The Clock tab has always shown these; until now nothing read them.
    let mux = match graph {
        Some(g) => super::rcc_mux::emit_lines(g, chip, family),
        None => Vec::new(),
    };
    // The reset shortcut only applies when the chip's HW default equals this
    // reset (HSI). L4/L5/U5 default to MSI, so a reset-equivalent graph must
    // still emit an explicit HSI block, not `init(Default::default())`.
    //
    // A peripheral selection also disqualifies it: the buses may be at reset
    // while USART1 has been moved to HSI, and `init(Default::default())` has
    // nowhere to say so.
    if spec.reset_is_hw_default && values == spec.reset && mux.is_empty() {
        return wrap(EMBASSY_RESET_INIT.to_string(), manual);
    }
    let mut block = emit_rcc_block(&desc, &values);
    if !mux.is_empty() {
        // Spliced INSIDE the `{ use embassy_stm32::rcc; … }` scope, after the
        // prescalers — the lines say `rcc::mux::…` and that `use` is what makes
        // them resolve.
        let tail = "    }\n    let p = embassy_stm32::init(config);\n";
        if let Some(cut) = block.rfind(tail) {
            let lines = format!("{}\n", mux.join("\n"));
            block.insert_str(cut, &lines);
        }
    }
    wrap(
        if verified {
            block
        } else {
            format!("{}{block}", unverified_note())
        },
        manual,
    )
}

/// The F1's clock on embassy-stm32, from the typed F1 clock.
///
/// Read with the F1's own reader rather than a [`ReadSpec`], because the PLL
/// input is TWO nodes - the PLLSRC mux and the PLLXTPRE divider - and the HSI
/// branch is always halved: embassy panics at init on an HSI PLL whose `prediv`
/// is not `DIV2`. Emitted in the [`RccDescriptor::f013`] shape, compiled against
/// embassy-stm32 0.6 on an STM32F103C8. PLLXTPRE /2, which stm32f1xx-hal cannot
/// express, is just `prediv: DIV2` here.
///
/// Not written: the USB prescaler, which embassy picks from the PLL output
/// itself (/1 at 48 MHz, /1.5 at 72), and the ADC prescaler at embassy's own
/// default of /6 - so the default tree's block is exactly the verified one.
///
/// `chip` matters for one fact: the connectivity line (F105/F107, metapac's
/// `rcc_f1cl`) has a `PllMul` of only MUL4..MUL9, where the rest of the F1 goes
/// to MUL16. A multiplier outside that window is flagged in the block.
fn f1_embassy_block(chip: &str, c: &super::super::clock::model::Stm32f1Clock) -> String {
    use super::super::clock::model::{PllSrc, SysclkSrc};
    const HSI_HZ: u32 = 8_000_000;
    let sys = match c.sysclk_src {
        SysclkSrc::Hsi => SysSource::Hsi,
        SysclkSrc::Hse => SysSource::Hse,
        SysclkSrc::Pll => SysSource::Pll,
    };
    let pll_src_hse = c.pll_src != PllSrc::HsiDiv2;
    let pll_m = if c.pll_src == PllSrc::Hse { 1 } else { 2 };
    let pll_n = u32::from(c.pll_mul);
    let sysclk_hz = match sys {
        SysSource::Hsi => HSI_HZ,
        SysSource::Hse => c.hse_hz,
        SysSource::Pll => (if pll_src_hse { c.hse_hz } else { HSI_HZ }) / pll_m * pll_n,
    };
    let values = RccValues {
        sys,
        hse_on: c.hse_enabled || sys == SysSource::Hse || (sys == SysSource::Pll && pll_src_hse),
        hse_hz: c.hse_hz,
        pll_src_hse,
        pll_m,
        pll_n,
        pll_out: 1,
        ahb: u32::from(c.ahb_pre),
        sysclk_hz,
        apb: vec![
            ("apb1_pre", u32::from(c.apb1_pre)),
            ("apb2_pre", u32::from(c.apb2_pre)),
        ],
    };
    let desc = match super::rcc_mux::rcc_version(chip) {
        Some("f1cl") => RccDescriptor::f013().with_pll_n((4, 9)),
        _ => RccDescriptor::f013(),
    };
    let mut block = emit_rcc_block(&desc, &values);
    if c.adc_pre != 6 {
        let tail = "    }\n    let p = embassy_stm32::init(config);\n";
        if let Some(cut) = block.rfind(tail) {
            block.insert_str(
                cut,
                &format!(
                    "        config.rcc.adc_pre = rcc::ADCPrescaler::DIV{};\n",
                    c.adc_pre
                ),
            );
        }
    }
    block
}

/// The typed F1 clock the Async block is emitted from - the same reading
/// [`graph_clock_block_for`] does, so a check on it is a check on the code.
pub fn f1_embassy_clock(clock: &ClockConfig) -> super::super::clock::model::Stm32f1Clock {
    match clock {
        ClockConfig::Graph(gc) => super::super::clock::graph::graph_to_stm32f1(&gc.for_codegen()),
        _ => Default::default(),
    }
}

/// Would embassy-stm32 0.6's F1 clock init accept what [`f1_embassy_block`]
/// emits from `c`? Its `rcc_assert!`s are real asserts (the crate's
/// `unchecked-overclocking` feature is never enabled here), so `false` means
/// `embassy_stm32::init` panics at boot, before any peripheral exists.
///
/// The limits are `rcc/f013.rs`'s `max` module for the F1: HCLK 72, PCLK1 36,
/// PCLK2 72 and ADC 14 MHz, plus - only when SYSCLK is the PLL, the one case
/// the block configures it - PLL input 1..=25 and output 16..=72 MHz. The HSE
/// is taken as the block writes it, switched on whenever the PLL needs it. Two
/// asserts cannot fire from this block and are not repeated: the HSI PLL is
/// always `DIV2`, and the Clock tab keeps the crystal in 4..=16 MHz.
pub fn f1_embassy_clock_ok(c: &super::super::clock::model::Stm32f1Clock) -> bool {
    use super::super::clock::model::{PllSrc, SysclkSrc};
    const MHZ: u32 = 1_000_000;
    const HSI_HZ: u32 = 8 * MHZ;
    let pll_in = match c.pll_src {
        PllSrc::HsiDiv2 => HSI_HZ / 2,
        PllSrc::Hse => c.hse_hz,
        PllSrc::HseDiv2 => c.hse_hz / 2,
    };
    let pll_out = pll_in.saturating_mul(u32::from(c.pll_mul));
    let (sys, pll_ok) = match c.sysclk_src {
        SysclkSrc::Hsi => (HSI_HZ, true),
        SysclkSrc::Hse => (c.hse_hz, true),
        SysclkSrc::Pll => (
            pll_out,
            (MHZ..=25 * MHZ).contains(&pll_in) && (16 * MHZ..=72 * MHZ).contains(&pll_out),
        ),
    };
    let hclk = sys / u32::from(c.ahb_pre.max(1));
    let pclk1 = hclk / u32::from(c.apb1_pre.max(1));
    let pclk2 = hclk / u32::from(c.apb2_pre.max(1));
    let adc = pclk2 / u32::from(c.adc_pre.max(1));
    pll_ok && hclk <= 72 * MHZ && pclk1 <= 36 * MHZ && pclk2 <= 72 * MHZ && adc <= 14 * MHZ
}

/// Release the JTAG-only pads on an F1 clock block: PA15, PB3 and PB4 come out
/// of reset as the JTAG port, and embassy leaves them there unless told
/// otherwise - so a project that wires one compiles and the pad does nothing.
/// `SwdOnly` keeps the SWD pins a probe needs. The line goes before the
/// `init(config)` that consumes the config, and only when one of the three is
/// in use, as on Blocking (`disable_jtag`).
pub fn with_swd_only(block: &str) -> String {
    let init = "    let p = embassy_stm32::init(config);\n";
    match block.rfind(init) {
        Some(cut) => {
            let mut out = block.to_owned();
            out.insert_str(
                cut,
                "    // PA15, PB3 and PB4 are JTAG at reset; free them, keep SWD.\n    \
                 config.swj = embassy_stm32::gpio::SwjCfg::SwdOnly;\n",
            );
            out
        }
        None => block.to_owned(),
    }
}

/// The header on a block emitted from [`generic_recipe`]. It says the one thing
/// the user cannot see from the code itself: the field NAMES are a guess, the
/// numbers are not.
fn unverified_note() -> String {
    [
        "    // NOTE: this IDE has no verified RCC recipe for this family, so the",
        "    // block below uses embassy's most common shape. The numbers come from",
        "    // the Clock tab and are correct; a field name might not exist for this",
        "    // chip. If it doesn't, fix it here and tick \"Write the clock by hand\"",
        "    // in the Clock tab to keep the fix across regeneration.",
        "",
    ]
    .join("\n")
}

/// Fence the block off so a regeneration leaves it alone — only in manual mode,
/// so a generated project's `main.rs` is unchanged.
fn wrap(block: String, manual: bool) -> String {
    use super::common::{CLOCK_BEGIN, CLOCK_END};
    if manual {
        format!("{CLOCK_BEGIN}\n{block}{CLOCK_END}\n")
    } else {
        block
    }
}

/// What a chip whose family has no RCC recipe gets: a WORKING default plus the
/// shape of the real thing, commented out.
///
/// The active line keeps the project compiling and warning-free; the comment
/// above it is the code to uncomment, in the same idiom the generated blocks
/// use, so setting the clock by hand is filling in numbers rather than looking
/// up embassy's API. Both are inside the manual markers, so the moment it is
/// edited the edit survives.
fn hand_written_skeleton() -> String {
    [
        "    // Clock: this chip's family has no generated RCC recipe yet, so the",
        "    // Clock tab's tree cannot be turned into code automatically. Set it",
        "    // here — this block is kept across regeneration.",
        "    //",
        "    //   let mut config = embassy_stm32::Config::default();",
        "    //   {",
        "    //       use embassy_stm32::rcc;",
        "    //       config.rcc.hse = Some(rcc::Hse {",
        "    //           freq: embassy_stm32::time::Hertz(8_000_000),",
        "    //           mode: rcc::HseMode::Oscillator,",
        "    //       });",
        "    //       config.rcc.sys = rcc::Sysclk::PLL1_P;",
        "    //   }",
        "    //   let p = embassy_stm32::init(config);",
        "    //",
        "    // Then delete the line below.",
        "    let p = embassy_stm32::init(Default::default()); // reset clock (HSI)",
        "",
    ]
    .join("\n")
}

/// Does this family's clock code come from the Clock tab at all?
///
/// `false` means the tree cannot be turned into code — the block is the
/// hand-written skeleton, and manual mode is the only way to configure it.
///
/// STM32F1 and the Espressif parts are here even though they have no
/// [`rcc_recipe`]: they generate through their own HALs (`graph_to_stm32f1`,
/// `esp_init_line`). Left out, every F1 project would have opened defaulted to
/// hand-written — with the generator still overwriting the block, since those
/// paths carry no markers.
///
/// `is_esp`, not `"esp32c3"`. Naming one chip was right when there was one; the
/// eight added later then reported *"no clock code (its tree cannot be turned
/// into an RCC config)"* in the New Project dialog while their `main.rs` carried
/// `with_cpu_clock(CpuClock::_160MHz)` all along. The tree indeed becomes no RCC
/// config — an ESP has no RCC — but the question this answers is whether clock
/// code is generated, and it is.
pub fn generates_clock_code(family: &str) -> bool {
    // The Pico families join F1 and ESP for the same reason those are here: their
    // backend emits real clock setup through the chip's own HAL
    // (`init_clocks_and_plls`), not through an RCC recipe. The Clock tab cannot
    // retune it yet, but `main.rs` is NOT left with a commented skeleton — which
    // is the question this predicate is actually asked.
    family == "stm32f1"
        || is_esp(family)
        || crate::panels::mcu_module::codegen::rp::is_rp(family)
        // nRF: `Clocks::new(..).enable_ext_hfosc().set_lfclk_src_*()`, read off
        // the tree's two muxes.
        || crate::panels::mcu_module::codegen::nrf::is_nrf(family)
        || rcc_recipe(family).is_some()
}

/// Does THIS chip's clock reach `main.rs` — family recipe or tree?
///
/// The per-family answer above is no longer the whole story: a family with no
/// recipe still generates real code once its tree carries the canonical spine
/// (see [`generic_recipe`]). Which is exactly the case that was broken — a chip
/// given a tree in the Clock tab kept the commented skeleton forever, because
/// "no recipe" had already decided, at `Mcu::new`, that its clock was written by
/// hand and must therefore be PRESERVED across regeneration.
///
/// So this is what the manual default and the tab's warning must ask.
pub fn generates_clock_code_for(family: &str, clock: &ClockConfig) -> bool {
    // Asked with the SAME condition `graph_clock_block` dispatches on. A family
    // whose emitter runs while this says no would put "no clock code" in the
    // preflight, the System tab and the Clock tab while `main.rs` quietly had
    // some — and would default that chip to hand-written, preserving a block it
    // was about to generate anyway.
    if family == "stm32n6"
        && matches!(clock, ClockConfig::Graph(gc) if super::rcc_n6::block(&gc.for_codegen()).is_some())
    {
        return true;
    }
    generates_clock_code(family)
        || matches!(clock, ClockConfig::Graph(gc) if generic_recipe(&gc.for_codegen()).is_some())
}

/// Can the clock block be hand-written and PRESERVED for this family?
///
/// Only the embassy path fences the block off. F1 and ESP generate real clock
/// code through their own HALs and are not marker-wrapped, so offering the
/// switch there would promise a preservation that does not happen.
///
/// Which is what the eight Espressif parts added after the C3 were offered:
/// the switch appeared, and their hand-written clock would have been overwritten
/// on the first regeneration.
pub fn supports_manual_clock(family: &str) -> bool {
    // nRF joins them: its clock block is two `Clocks` calls inside the one
    // marker-wrapped `main`, and `NrfBackend` never reads `clock_manual`, so
    // the switch would promise a preservation that does not happen.
    !(family == "stm32f1"
        || is_esp(family)
        || crate::panels::mcu_module::codegen::nrf::is_nrf(family))
}

/// The clock selections, family-neutral — what the generic emitter consumes and
/// the generic reader ([`read_rcc_values`]) produces.
#[derive(Clone, Debug, PartialEq)]
pub struct RccValues {
    /// `true` = PLL drives SYSCLK; `false` with `hse_on` = HSE; else HSI.
    pub sys: SysSource,
    pub hse_on: bool,
    pub hse_hz: u32,
    pub pll_src_hse: bool,
    pub pll_m: u32,
    pub pll_n: u32,
    /// The single PLL output divider in use (F4 = P, WBA = R).
    pub pll_out: u32,
    pub ahb: u32,
    /// SYSCLK produced, in Hz — for the header comment.
    pub sysclk_hz: u32,
    /// `(embassy field suffix, divisor)` per APB bus, in emit order.
    pub apb: Vec<(&'static str, u32)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SysSource {
    Hsi,
    Hse,
    Pll,
}

/// How a family writes the HSE oscillator into `config.rcc.hse`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HseStyle {
    /// `Hse { freq: Hertz(hz), mode: HseMode::Oscillator }` — the crystal
    /// frequency is a field (F4).
    Freq,
    /// `Hse { prescaler: HsePrescaler::DIV1 }` — a fixed crystal, no freq field
    /// (WBA's 32 MHz radio crystal).
    FixedPrescaler,
}

/// How the HSE appears in the human-readable header comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HseLabel {
    /// `HSE {mhz} MHz` (F4 — any crystal).
    WithMhz,
    /// A fixed string, e.g. `HSE32` (WBA).
    Fixed(&'static str),
}

/// Everything family-specific about emitting the RCC block. The ~15 fields that
/// differ between F4 and WBA — a new family is one of these.
#[derive(Clone, Debug, PartialEq)]
pub struct RccDescriptor {
    /// The `config.rcc.<field>` that holds the PLL: `pll` (F4) / `pll1` (WBA).
    pub pll_field: &'static str,
    /// `true`: PLL source is a nested line inside the `Pll { … }` struct (WBA).
    /// `false`: a separate `config.rcc.pll_src` line (F4).
    pub pll_source_nested: bool,
    /// What that nested line is CALLED: `source` on the modern families,
    /// `src` on F0/F1/F3. Verified against embassy-stm32 0.6 `rcc/g4.rs` and
    /// `rcc/f013.rs` — the two spellings are not interchangeable, and picking
    /// the wrong one is a compile error on a field name.
    pub pll_source_field: &'static str,
    /// embassy divider TYPE for the used PLL output: `PllPDiv` (F4) / `PllDiv`
    /// (WBA).
    pub pll_out_div_type: &'static str,
    /// Which `Pll { … }` field the used output fills: `divp` (F4) / `divr`
    /// (WBA). The other two outputs are emitted as `None`.
    ///
    /// **Empty means the family's `Pll` has no output dividers at all** — F0/F1/F3
    /// multiply and stop (`Pll { src, prediv, mul }`), so emitting `divp: None`
    /// there names a field that does not exist.
    pub pll_out_field: &'static str,
    /// `Sysclk::<variant>` when SYSCLK is the PLL: `PLL1_P` (F4) / `PLL1_R`
    /// (WBA).
    pub sys_pll_variant: &'static str,
    /// Emit a `frac: None` line in the `Pll { … }` (WBA yes, F4 no).
    pub pll_has_frac: bool,
    /// HSE write style.
    pub hse_style: HseStyle,
    /// Emit `config.rcc.voltage_scale = rcc::VoltageScale::RANGE1;` whenever
    /// SYSCLK is not plain HSI (WBA's PLL panics in range 2). Empty = never.
    pub voltage_scale_non_hsi: Option<&'static str>,
    /// Header-comment label for the HSI source (always `HSI16` today).
    pub hsi_label: &'static str,
    /// Header-comment label for the HSE source.
    pub hse_label: HseLabel,
    /// The `via <X>` suffix in the PLL description: `PLLP` (F4) / `PLL1R` (WBA).
    pub pll_desc_via: &'static str,
    /// Emit `config.rcc.hsi = true;` when HSI drives SYSCLK or the PLL. Needed
    /// by families whose `Config::default()` leaves HSI OFF (L4/L5/U5:
    /// `hsi: bool = false`); false where HSI is on by default (F4/G4/G0) or
    /// unused (WBA fixed HSE).
    pub hsi_needs_enable: bool,
    /// Inclusive `PllMul::MUL<n>` range this family's PAC actually defines.
    ///
    /// `PllMul` is not a shared embassy type — it is `pac::rcc::vals::Plln`,
    /// generated per chip. So the same recipe can serve several families whose
    /// legal N differs, and emitting a value outside this window produces code
    /// that names a variant which does not exist. `None` = no window known for
    /// the family, so nothing is checked (the pre-existing behaviour).
    pub pll_n: Option<(u32, u32)>,
}

impl RccDescriptor {
    /// Narrow this family's PLLN window — see [`RccDescriptor::pll_n`].
    pub fn with_pll_n(mut self, range: (u32, u32)) -> Self {
        self.pll_n = Some(range);
        self
    }

    /// STM32F4: separate `pll_src`, P output, no voltage scale, APB1/2.
    pub fn f4() -> Self {
        Self {
            pll_field: "pll",
            pll_source_nested: false,
            pll_source_field: "source",
            pll_out_div_type: "PllPDiv",
            pll_out_field: "divp",
            sys_pll_variant: "PLL1_P",
            pll_has_frac: false,
            hse_style: HseStyle::Freq,
            voltage_scale_non_hsi: None,
            hsi_label: "HSI16",
            hse_label: HseLabel::WithMhz,
            pll_desc_via: "PLLP",
            hsi_needs_enable: false,
            // No window known for this family; `with_pll_n` narrows it where
            // one is (see `rcc_recipe`).
            pll_n: None,
        }
    }

    /// STM32G4: `pll` with nested source (no frac), R output, a real HSE
    /// crystal, APB1/2. Verified against embassy-stm32 v0.4.0 `src/rcc/g4.rs`:
    /// `Config { hsi: true (default), hse, sys: Sysclk, pll: Option<Pll>,
    /// ahb_pre, apb1_pre, apb2_pre, boost }`; `Pll { source, prediv, mul, divp,
    /// divq, divr }`; SYSCLK-from-PLL variant `Sysclk::PLL1_R`. No `voltage_
    /// scale` field (G4 uses `boost`, which defaults false — fine ≤150 MHz, the
    /// shipped preset's ceiling); embassy sets flash latency itself.
    pub fn g4() -> Self {
        Self {
            pll_field: "pll",
            pll_source_nested: true,
            pll_source_field: "source",
            pll_out_div_type: "PllRDiv",
            pll_out_field: "divr",
            sys_pll_variant: "PLL1_R",
            pll_has_frac: false,
            hse_style: HseStyle::Freq,
            voltage_scale_non_hsi: None,
            hsi_label: "HSI16",
            hse_label: HseLabel::WithMhz,
            pll_desc_via: "PLLR",
            hsi_needs_enable: false,
            // No window known for this family; `with_pll_n` narrows it where
            // one is (see `rcc_recipe`).
            pll_n: None,
        }
    }

    /// STM32L4: same emit shape as G4 (nested source, R output, real HSE, no
    /// frac, no voltage_scale line) EXCEPT `hsi_needs_enable` — L4's
    /// `Config::default()` leaves `hsi: false`, so an HSI-sourced clock must
    /// switch it on explicitly. Verified against embassy `l.rs` + metapac
    /// `rcc_l4` (Sysclk::PLL1_R, Plln MUL8..127, Pllr {2,4,6,8}).
    pub fn l4() -> Self {
        Self {
            hsi_needs_enable: true,
            // No window known for this family; `with_pll_n` narrows it where
            // one is (see `rcc_recipe`).
            pll_n: None,
            ..Self::g4()
        }
    }

    /// STM32G0: identical emit shape to G4 (nested source, R output, real HSE,
    /// no frac, no voltage_scale — G0 uses `voltage_range`, default Range1 is
    /// fine ≤64 MHz). The only difference from G4 is the single APB bus, which
    /// lives in [`ReadSpec::g0`], not here. Verified against embassy `g0.rs`.
    pub fn g0() -> Self {
        Self::g4() // same descriptor; the bus count is a ReadSpec concern
    }

    /// STM32F0/F1/F3 — embassy's `rcc/f013.rs`, the families whose PLL just
    /// multiplies: `Pll { src, prediv, mul }`, no output divider at all.
    ///
    /// Verified field-by-field against embassy-stm32 0.6: the nested source is
    /// `src` (not `source`), there are no `divp`/`divq`/`divr` fields to emit,
    /// `Sysclk::PLL1_P` still names the PLL, and HSI is **8 MHz** here, not the
    /// 16 every other recipe assumes.
    ///
    /// This exists because those families were unreachable: [`generic_recipe`]
    /// keyed on a PLL output divider, so a tree without one matched nothing and
    /// generated no clock code — the STM32F358 case, imported complete and
    /// still silent.
    ///
    /// L0/L1 also lack an output divider but are NOT this shape (`rcc/l.rs` has
    /// `Pll { source, mul, div }`), so they land here and will need one field
    /// corrected — which the emitted note says how to make permanent.
    pub fn f013() -> Self {
        Self {
            pll_field: "pll",
            pll_source_nested: true,
            pll_source_field: "src",
            // Unused: there is no output divider to type.
            pll_out_div_type: "",
            pll_out_field: "",
            sys_pll_variant: "PLL1_P",
            pll_has_frac: false,
            hse_style: HseStyle::Freq,
            voltage_scale_non_hsi: None,
            hsi_label: "HSI8",
            hse_label: HseLabel::WithMhz,
            pll_desc_via: "PLL",
            hsi_needs_enable: false,
            pll_n: None,
        }
    }

    /// STM32WBA: `pll1` with nested source + frac, R output, RANGE1 off-HSI,
    /// fixed 32 MHz HSE, APB1/2/7.
    pub fn wba() -> Self {
        Self {
            pll_field: "pll1",
            pll_source_nested: true,
            pll_source_field: "source",
            pll_out_div_type: "PllDiv",
            pll_out_field: "divr",
            sys_pll_variant: "PLL1_R",
            pll_has_frac: true,
            hse_style: HseStyle::FixedPrescaler,
            voltage_scale_non_hsi: Some("RANGE1"),
            hsi_label: "HSI16",
            hse_label: HseLabel::Fixed("HSE32"),
            pll_desc_via: "PLL1R",
            hsi_needs_enable: false,
            // No window known for this family; `with_pll_n` narrows it where
            // one is (see `rcc_recipe`).
            pll_n: None,
        }
    }
}

/// How to read a family's selections out of the clock graph.
///
/// The graph→values logic was duplicated verbatim between `graph_to_f4` and
/// `graph_to_wba` — same node ids, same `index_of`/`divisor_of` readers. The
/// only differences are these fields, so a family's reader is now data.
pub struct ReadSpec {
    /// HSI frequency in Hz — 16 MHz on both F4 and WBA (`HSI16`).
    pub hsi_hz: u32,
    /// `Some(hz)` for a fixed crystal (WBA's 32 MHz radio HSE, read from a
    /// register-less node); `None` reads the live frequency from the `hse`
    /// source node (F4).
    pub hse_fixed_hz: Option<u32>,
    /// The single PLL-output divider node id: `pllp` (F4) / `pllr` (WBA).
    pub pll_out_node: &'static str,
    /// `(embassy field suffix, graph node id)` per APB bus, in emit order.
    pub apb: &'static [(&'static str, &'static str)],
    /// The all-default baseline: values with everything at reset. Read as the
    /// fallback for any node the graph is missing, and compared against the
    /// result to decide the `embassy_stm32::init(Default::default())` path.
    pub reset: RccValues,
    /// `true` when `Config::default()` equals `reset` (F4/G4/G0/WBA: HSI) — then
    /// a reset-equivalent graph emits `init(Default::default())`. `false` for
    /// L4/L5/U5, whose HW default is MSI, not HSI: a reset graph must still emit
    /// an explicit HSI block, so the shortcut is skipped.
    pub reset_is_hw_default: bool,
}

impl ReadSpec {
    pub fn f4() -> Self {
        Self {
            reset_is_hw_default: true,
            hsi_hz: 16_000_000,
            hse_fixed_hz: None,
            pll_out_node: "pllp",
            apb: &[("apb1_pre", "apb1"), ("apb2_pre", "apb2")],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 8_000_000,
                pll_src_hse: false,
                pll_m: 8,
                pll_n: 100,
                pll_out: 2,
                ahb: 1,
                sysclk_hz: 16_000_000,
                apb: vec![("apb1_pre", 1), ("apb2_pre", 1)],
            },
        }
    }

    /// G4: HSI16, a live HSE crystal (read from the `hse` node), PLL output on
    /// `pllr`, APB1/2. `reset` mirrors the shipped graph's default PLL
    /// selections (M=4, N=75, R=2) so a fully-reset graph reads back as reset.
    pub fn g4() -> Self {
        Self {
            reset_is_hw_default: true,
            hsi_hz: 16_000_000,
            hse_fixed_hz: None,
            pll_out_node: "pllr",
            apb: &[("apb1_pre", "apb1"), ("apb2_pre", "apb2")],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 8_000_000,
                pll_src_hse: false,
                pll_m: 4,
                pll_n: 75,
                pll_out: 2,
                ahb: 1,
                sysclk_hz: 16_000_000,
                apb: vec![("apb1_pre", 1), ("apb2_pre", 1)],
            },
        }
    }

    /// G0: HSI16, a live HSE crystal, PLL output on `pllr`, and a SINGLE APB
    /// bus (G0 has no APB2). `reset` mirrors the shipped 64 MHz preset's PLL
    /// selections (M=1, N=8, R=2).
    pub fn g0() -> Self {
        Self {
            reset_is_hw_default: true,
            hsi_hz: 16_000_000,
            hse_fixed_hz: None,
            pll_out_node: "pllr",
            apb: &[("apb1_pre", "apb1")],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 8_000_000,
                pll_src_hse: false,
                pll_m: 1,
                pll_n: 8,
                pll_out: 2,
                ahb: 1,
                sysclk_hz: 16_000_000,
                apb: vec![("apb1_pre", 1)],
            },
        }
    }

    /// L4: HSI16, live HSE crystal, PLL output on `pllr`, APB1/2. `reset` mirrors
    /// the shipped 80 MHz preset (M=1, N=10, R=2); `reset_is_hw_default: false`
    /// because L4 boots on MSI, so a reset graph still emits an explicit HSI
    /// block rather than `init(Default::default())`.
    pub fn l4() -> Self {
        Self {
            reset_is_hw_default: false,
            hsi_hz: 16_000_000,
            hse_fixed_hz: None,
            pll_out_node: "pllr",
            apb: &[("apb1_pre", "apb1"), ("apb2_pre", "apb2")],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 8_000_000,
                pll_src_hse: false,
                pll_m: 1,
                pll_n: 10,
                pll_out: 2,
                ahb: 1,
                sysclk_hz: 16_000_000,
                apb: vec![("apb1_pre", 1), ("apb2_pre", 1)],
            },
        }
    }

    /// F0/F1/F3: HSI is **8 MHz** here, and there is no PLL output divider —
    /// `pll_out_node` names nothing, so the reader falls back to the `/1` below.
    pub fn f013() -> Self {
        Self {
            reset_is_hw_default: false,
            hsi_hz: 8_000_000,
            hse_fixed_hz: None,
            pll_out_node: "",
            apb: &[("apb1_pre", "apb1"), ("apb2_pre", "apb2")],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 8_000_000,
                pll_src_hse: false,
                pll_m: 1,
                pll_n: 2,
                pll_out: 1,
                ahb: 1,
                sysclk_hz: 8_000_000,
                apb: vec![("apb1_pre", 1), ("apb2_pre", 1)],
            },
        }
    }

    pub fn wba() -> Self {
        Self {
            reset_is_hw_default: true,
            hsi_hz: 16_000_000,
            hse_fixed_hz: Some(32_000_000),
            pll_out_node: "pllr",
            apb: &[
                ("apb1_pre", "apb1"),
                ("apb2_pre", "apb2"),
                ("apb7_pre", "apb7"),
            ],
            reset: RccValues {
                sys: SysSource::Hsi,
                hse_on: false,
                hse_hz: 32_000_000,
                pll_src_hse: true,
                pll_m: 2,
                pll_n: 25,
                pll_out: 4,
                ahb: 1,
                sysclk_hz: 16_000_000,
                apb: vec![("apb1_pre", 1), ("apb2_pre", 1), ("apb7_pre", 1)],
            },
        }
    }
}

/// Read the user's clock selections out of `g` per `spec` — the ONE reader that
/// replaced `graph_to_f4` / `graph_to_wba`. Missing/foreign nodes fall back to
/// `spec.reset`, so a non-matching graph degrades to the reset state.
pub fn read_rcc_values(g: &ClockGraph, spec: &ReadSpec) -> RccValues {
    let index_of = |id: &str| match g.node(id).map(|n| &n.state) {
        Some(NodeState::Index(i)) => Some(*i),
        _ => None,
    };
    let divisor_of = |id: &str| -> Option<u32> {
        let node = g.node(id)?;
        let NodeKind::Divider { options } = &node.kind else {
            return None;
        };
        let NodeState::Index(i) = node.state else {
            return None;
        };
        options.get(i).copied()
    };

    let r = &spec.reset;
    let mut hse_on = false;
    let mut hse_hz = spec.hse_fixed_hz.unwrap_or(r.hse_hz);
    if let Some(NodeState::Source { enabled, hz }) = g.node("hse").map(|n| &n.state) {
        hse_on = *enabled;
        if spec.hse_fixed_hz.is_none() {
            hse_hz = *hz;
        }
    }

    let sys = match index_of("sw") {
        Some(0) => SysSource::Hsi,
        Some(1) => SysSource::Hse,
        _ => SysSource::Pll,
    };
    // The pllsrc mux is 2-input (HSI=0, HSE=1): index != 0 → HSE; a missing
    // node keeps the family default. (F4 used `== Some(1)`, WBA `!= Some(0)`;
    // for the only real indices, 0 and 1, both agree with this.)
    let pll_src_hse = index_of("pllsrc").map(|i| i != 0).unwrap_or(r.pll_src_hse);

    let pll_m = divisor_of("pllm").unwrap_or(r.pll_m);
    let pll_n = match g.node("plln").map(|n| &n.state) {
        Some(NodeState::Value(v)) => *v,
        _ => r.pll_n,
    };
    let pll_out = divisor_of(spec.pll_out_node).unwrap_or(r.pll_out);
    let ahb = divisor_of("ahb").unwrap_or(1);
    let apb: Vec<(&'static str, u32)> = spec
        .apb
        .iter()
        .map(|(field, node)| (*field, divisor_of(node).unwrap_or(1)))
        .collect();

    // Sysclk on HSE / PLL-from-HSE implies the oscillator is in use.
    if sys == SysSource::Hse || (sys == SysSource::Pll && pll_src_hse) {
        hse_on = true;
    }

    let sysclk_hz = match sys {
        SysSource::Hsi => spec.hsi_hz,
        SysSource::Hse => hse_hz,
        SysSource::Pll => {
            let src = if pll_src_hse { hse_hz } else { spec.hsi_hz };
            (src / pll_m.max(1)) * pll_n / pll_out.max(1)
        }
    };

    RccValues {
        sys,
        hse_on,
        hse_hz,
        pll_src_hse,
        pll_m,
        pll_n,
        pll_out,
        ahb,
        sysclk_hz,
        apb,
    }
}

/// Emit the full clock block for `values` using the family's `desc`.
///
/// Byte-for-byte compatible with the previous hand-written `f4`/`wba` emitters —
/// their tests are the acceptance criteria. The caller decides the reset case
/// (all-default → `embassy_stm32::init(Default::default())`) before calling.
pub fn emit_rcc_block(desc: &RccDescriptor, v: &RccValues) -> String {
    let mhz = v.sysclk_hz / 1_000_000;
    let hse_mhz = v.hse_hz / 1_000_000;

    // Source description for the header comment.
    let hse_desc = match &desc.hse_label {
        HseLabel::WithMhz => format!("HSE {hse_mhz} MHz"),
        HseLabel::Fixed(s) => s.to_string(),
    };
    let sys_desc = match v.sys {
        SysSource::Hsi => desc.hsi_label.to_string(),
        SysSource::Hse => hse_desc.clone(),
        SysSource::Pll => format!(
            "{} /{} x{} /{} via {}",
            if v.pll_src_hse {
                hse_desc.clone()
            } else {
                desc.hsi_label.to_string()
            },
            v.pll_m,
            v.pll_n,
            v.pll_out,
            desc.pll_desc_via,
        ),
    };

    let apb_desc = v
        .apb
        .iter()
        .map(|(name, div)| format!("{} /{}", apb_human(name), div))
        .collect::<Vec<_>>()
        .join(" ");

    let mut b = String::new();
    b.push_str(&format!(
        "    // Clock (from the Clock tab): SYSCLK {mhz} MHz ({sys_desc}) · \
         AHB /{} · {apb_desc}\n",
        v.ahb
    ));
    b.push_str("    let mut config = embassy_stm32::Config::default();\n");
    b.push_str("    {\n        use embassy_stm32::rcc;\n");

    // ── HSE ─────────────────────────────────────────────────────────────
    let hse_used = v.sys == SysSource::Hse || (v.sys == SysSource::Pll && v.pll_src_hse);
    if hse_used {
        match desc.hse_style {
            HseStyle::Freq => b.push_str(&format!(
                "        config.rcc.hse = Some(rcc::Hse {{ freq: embassy_stm32::time::Hertz({}), \
                 mode: rcc::HseMode::Oscillator }});\n",
                v.hse_hz
            )),
            HseStyle::FixedPrescaler => b.push_str(
                "        config.rcc.hse = Some(rcc::Hse { prescaler: rcc::HsePrescaler::DIV1 });\n",
            ),
        }
    }

    // ── HSI enable (families whose default leaves HSI off, e.g. L4) ─────
    let hsi_used = v.sys == SysSource::Hsi || (v.sys == SysSource::Pll && !v.pll_src_hse);
    if desc.hsi_needs_enable && hsi_used {
        b.push_str("        config.rcc.hsi = true;\n");
    }

    // ── PLL ─────────────────────────────────────────────────────────────
    if v.sys == SysSource::Pll {
        let src = if v.pll_src_hse { "HSE" } else { "HSI" };
        if !desc.pll_source_nested {
            b.push_str(&format!(
                "        config.rcc.pll_src = rcc::PllSource::{src};\n"
            ));
        }
        b.push_str(&format!(
            "        config.rcc.{field} = Some(rcc::Pll {{\n",
            field = desc.pll_field
        ));
        if desc.pll_source_nested {
            b.push_str(&format!(
                "            {}: rcc::PllSource::{src},\n",
                desc.pll_source_field
            ));
        }
        b.push_str(&format!(
            "            prediv: rcc::PllPreDiv::DIV{},\n",
            v.pll_m
        ));
        // The net. `PllMul` is the chip's own PAC enum, so an N outside the
        // family's window names a variant that does not exist and rustc reports
        // it as "no associated item named `MUL144`" — true, and useless.
        //
        // NOT clamped: silently retuning the user's clock is worse than not
        // building. The value goes out as configured, with the explanation
        // attached where the compiler will point.
        if let Some((lo, hi)) = desc.pll_n
            && !(lo..=hi).contains(&v.pll_n)
        {
            b.push_str(&format!(
                "            // !! PLLN {} is outside this chip's range {}..={} — \
                 embassy's PllMul has no MUL{} for it.\n",
                v.pll_n, lo, hi, v.pll_n
            ));
            // The way out depends on the side: a raised PLLM only helps an N
            // that is too SMALL. Above the window (the F105/F107's MUL9) the
            // input has to get faster instead.
            if v.pll_n < lo {
                b.push_str(&format!(
                    "            // !! Raise PLLM until N lands in range: the same \
                     SYSCLK usually has a legal (M, N) pair (e.g. /{} x{}).\n",
                    v.pll_m * 2,
                    v.pll_n * 2
                ));
            } else {
                b.push_str(
                    "            // !! Feed the PLL a faster input (a smaller \
                     divider or a faster crystal) so a smaller N reaches the \
                     same SYSCLK.\n",
                );
            }
        }
        b.push_str(&format!("            mul: rcc::PllMul::MUL{},\n", v.pll_n));
        // Three outputs; the one this family uses gets the value, the rest None.
        // An empty `pll_out_field` means the family has no such fields at all
        // (F0/F1/F3), where even `divp: None` would not compile.
        if !desc.pll_out_field.is_empty() {
            for out in ["divp", "divq", "divr"] {
                if out == desc.pll_out_field {
                    b.push_str(&format!(
                        "            {out}: Some(rcc::{ty}::DIV{val}),\n",
                        ty = desc.pll_out_div_type,
                        val = v.pll_out,
                    ));
                } else {
                    b.push_str(&format!("            {out}: None,\n"));
                }
            }
        }
        if desc.pll_has_frac {
            b.push_str("            frac: None,\n");
        }
        b.push_str("        });\n");
    }

    // ── SYSCLK source ───────────────────────────────────────────────────
    let sys = match v.sys {
        SysSource::Hsi => "HSI",
        SysSource::Hse => "HSE",
        SysSource::Pll => desc.sys_pll_variant,
    };
    b.push_str(&format!("        config.rcc.sys = rcc::Sysclk::{sys};\n"));

    // ── Voltage scale (family option) ───────────────────────────────────
    if let Some(range) = desc.voltage_scale_non_hsi {
        if v.sys != SysSource::Hsi {
            b.push_str(&format!(
                "        config.rcc.voltage_scale = rcc::VoltageScale::{range};\n"
            ));
        }
    }

    // ── Prescalers ──────────────────────────────────────────────────────
    b.push_str(&format!(
        "        config.rcc.ahb_pre = rcc::AHBPrescaler::DIV{};\n",
        v.ahb
    ));
    for (field, div) in &v.apb {
        b.push_str(&format!(
            "        config.rcc.{field} = rcc::APBPrescaler::DIV{div};\n"
        ));
    }
    b.push_str("    }\n    let p = embassy_stm32::init(config);\n");
    b
}

/// `apb1_pre` → `APB1` for the header comment.
fn apb_human(field: &str) -> String {
    field.trim_end_matches("_pre").to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The F2 recipe must carry a PLLN window; the F4/F7 one shares every other
    /// field with it.
    #[test]
    fn only_the_f2_recipe_narrows_the_pll_n_window() {
        let (_, f2) = rcc_recipe("stm32f2").expect("stm32f2 has a recipe");
        let (_, f4) = rcc_recipe("stm32f4").expect("stm32f4 has a recipe");
        assert_eq!(f2.pll_n, Some((192, 432)));
        assert_eq!(f4.pll_n, None);
        // Same recipe otherwise — the window is the ONLY difference, which is
        // what lets them share `RccDescriptor::f4()`.
        assert_eq!(RccDescriptor { pll_n: None, ..f2 }, f4);
    }

    /// The net: an N the chip cannot encode is emitted AS CONFIGURED, with the
    /// reason attached. Clamping it would silently retune the user's clock,
    /// which is worse than not building.
    #[test]
    fn an_out_of_range_pll_n_is_explained_not_clamped() {
        let (_, desc) = rcc_recipe("stm32f2").unwrap();
        // The exact configuration from the STM32F217 bug report: 144 MHz via
        // HSI16 /8 x144 /2. Legal arithmetic, illegal N for this family.
        let mut v = f4_100mhz();
        v.pll_n = 144;
        v.sysclk_hz = 144_000_000;
        let out = emit_rcc_block(&desc, &v);
        assert!(
            out.contains("mul: rcc::PllMul::MUL144,"),
            "the configured value must survive verbatim:
{out}"
        );
        assert!(
            out.contains("PLLN 144 is outside this chip's range 192..=432"),
            "{out}"
        );
        assert!(out.contains("Raise PLLM"), "{out}");
    }

    /// …and says nothing when the value is fine, on either family.
    #[test]
    fn an_in_range_pll_n_emits_no_warning() {
        for family in ["stm32f2", "stm32f4"] {
            let (_, desc) = rcc_recipe(family).unwrap();
            let mut v = f4_100mhz();
            // Legal on both: 192 is the F2 floor and well above the F4's.
            v.pll_n = 192;
            let out = emit_rcc_block(&desc, &v);
            assert!(
                !out.contains("!!"),
                "{family} warned about a legal N:
{out}"
            );
        }
        // The F4's own default (N=100) is below the F2 floor but fine for F4 —
        // proof the window is per-family and not a global tightening.
        let (_, f4) = rcc_recipe("stm32f4").unwrap();
        assert!(!emit_rcc_block(&f4, &f4_100mhz()).contains("!!"));
    }

    fn f4_100mhz() -> RccValues {
        // The shipped F4 default: HSI /8 ×100 /2 → 100 MHz, APB1 /2.
        RccValues {
            sys: SysSource::Pll,
            hse_on: false,
            hse_hz: 8_000_000,
            pll_src_hse: false,
            pll_m: 8,
            pll_n: 100,
            pll_out: 2,
            ahb: 1,
            sysclk_hz: 100_000_000,
            apb: vec![("apb1_pre", 2), ("apb2_pre", 1)],
        }
    }

    #[test]
    fn f4_descriptor_reproduces_the_hand_written_output() {
        let s = emit_rcc_block(&RccDescriptor::f4(), &f4_100mhz());
        for needle in [
            "config.rcc.pll_src = rcc::PllSource::HSI;",
            "prediv: rcc::PllPreDiv::DIV8,",
            "mul: rcc::PllMul::MUL100,",
            "divp: Some(rcc::PllPDiv::DIV2),",
            "divq: None,",
            "divr: None,",
            "config.rcc.sys = rcc::Sysclk::PLL1_P;",
            "config.rcc.apb1_pre = rcc::APBPrescaler::DIV2;",
            "config.rcc.apb2_pre = rcc::APBPrescaler::DIV1;",
            "let p = embassy_stm32::init(config);",
            "SYSCLK 100 MHz (HSI16 /8 x100 /2 via PLLP)",
            "AHB /1 · APB1 /2 APB2 /1",
        ] {
            assert!(s.contains(needle), "missing: {needle}\n\n{s}");
        }
        // F4 has no voltage scale and no frac line.
        assert!(!s.contains("voltage_scale"));
        assert!(!s.contains("frac:"));
        assert!(!s.contains("config.rcc.hse"), "HSI preset needs no HSE");
    }

    #[test]
    fn wba_descriptor_reproduces_the_hand_written_output() {
        let v = RccValues {
            sys: SysSource::Pll,
            hse_on: true,
            hse_hz: 32_000_000,
            pll_src_hse: true,
            pll_m: 2,
            pll_n: 25,
            pll_out: 4,
            ahb: 1,
            sysclk_hz: 100_000_000,
            apb: vec![("apb1_pre", 1), ("apb2_pre", 1), ("apb7_pre", 1)],
        };
        let s = emit_rcc_block(&RccDescriptor::wba(), &v);
        for needle in [
            "config.rcc.hse = Some(rcc::Hse { prescaler: rcc::HsePrescaler::DIV1 });",
            "source: rcc::PllSource::HSE,",
            "prediv: rcc::PllPreDiv::DIV2,",
            "mul: rcc::PllMul::MUL25,",
            "divr: Some(rcc::PllDiv::DIV4),",
            "frac: None,",
            "config.rcc.sys = rcc::Sysclk::PLL1_R;",
            "config.rcc.voltage_scale = rcc::VoltageScale::RANGE1;",
            "config.rcc.apb7_pre = rcc::APBPrescaler::DIV1;",
            "SYSCLK 100 MHz (HSE32 /2 x25 /4 via PLL1R)",
            "AHB /1 · APB1 /1 APB2 /1 APB7 /1",
        ] {
            assert!(s.contains(needle), "missing: {needle}\n\n{s}");
        }
        // WBA nests the source and has no separate pll_src line.
        assert!(!s.contains("config.rcc.pll_src ="));
    }

    #[test]
    fn generic_reader_matches_the_shipped_f4_and_wba_presets() {
        use crate::panels::mcu_module::clock::graph::{stm32f4_graph, stm32wba_graph};
        // The shipped F4 default = HSI /8 ×100 /2 → 100 MHz SYSCLK, APB1 /2.
        let f4 = read_rcc_values(&stm32f4_graph(), &ReadSpec::f4());
        assert_eq!(f4.sys, SysSource::Pll);
        assert_eq!(f4.sysclk_hz, 100_000_000);
        assert_eq!((f4.pll_m, f4.pll_n, f4.pll_out), (8, 100, 2));
        assert!(!f4.pll_src_hse);
        assert_eq!(f4.apb, vec![("apb1_pre", 2), ("apb2_pre", 1)]);

        // The shipped WBA default = HSE32 /2 ×25 /4 → 100 MHz, all buses /1.
        let wba = read_rcc_values(&stm32wba_graph(), &ReadSpec::wba());
        assert_eq!(wba.sys, SysSource::Pll);
        assert_eq!(wba.sysclk_hz, 100_000_000);
        assert_eq!((wba.pll_m, wba.pll_n, wba.pll_out), (2, 25, 4));
        assert!(wba.pll_src_hse);
        assert_eq!(wba.hse_hz, 32_000_000);
        assert_eq!(wba.apb.len(), 3);
    }

    #[test]
    fn a_fully_reset_graph_reads_back_as_the_reset_baseline() {
        // HSI sysclk + EVERY prescaler /1 must equal `spec.reset` so codegen
        // emits `init(Default::default())`. The shipped 100 MHz preset ships
        // APB1 /2, so resetting `sw` alone is (correctly) NOT the reset state —
        // every divider has to go back to /1 (index 0).
        use crate::panels::mcu_module::clock::graph::{NodeState, stm32f4_graph};
        let mut g = stm32f4_graph();
        g.node_mut("sw").unwrap().state = NodeState::Index(0); // HSI
        g.node_mut("hse").unwrap().state = NodeState::Source {
            enabled: false,
            hz: 8_000_000,
        };
        for div in ["ahb", "apb1", "apb2"] {
            g.node_mut(div).unwrap().state = NodeState::Index(0); // /1
        }
        let spec = ReadSpec::f4();
        assert_eq!(read_rcc_values(&g, &spec), spec.reset);
    }

    /// THE point of the bindings: a tree with the vendor's node names generates
    /// exactly what the same tree named canonically would.
    #[test]
    fn a_bound_vendor_named_tree_generates_the_same_block() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, bind, stm32f4_graph};

        // The shipped F4 tree, renamed the way CubeMX writes it.
        let canonical = stm32f4_graph();
        let renames = [
            ("hse", "HSEOSC"),
            ("sw", "SysClkSource"),
            ("pllsrc", "PLLSource"),
            ("pllm", "PLLM"),
            ("plln", "PLLN"),
            ("pllp", "PLLP"),
            ("ahb", "AHBPrescaler"),
            ("apb1", "APB1Prescaler"),
            ("apb2", "APB2Prescaler"),
        ];
        let mut vendor = canonical.clone();
        for (from, to) in renames {
            let mut boxes = Vec::new();
            crate::panels::mcu_module::clock::graph::edit::rename_node(
                &mut vendor,
                &mut boxes,
                from,
                to,
            )
            .expect("rename");
        }
        assert!(vendor.node("SysClkSource").is_some(), "renamed");
        assert!(vendor.node("sw").is_none());

        let block = |graph: &ClockGraph, bindings: std::collections::BTreeMap<String, String>| {
            graph_clock_block(
                "stm32f4",
                &ClockConfig::Graph(GraphClock {
                    graph: graph.clone(),
                    layout: Default::default(),
                    bindings,
                }),
                false,
            )
        };

        // What the tree is worth: the canonical one emits a real PLL block.
        let want = block(&canonical, Default::default());
        assert!(want.contains("Pll"), "the reference must be a PLL block");

        // UNBOUND, the vendor-named tree reads as nothing at all — every value
        // falls back, which is exactly the silent failure the bindings exist to
        // prevent.
        assert_ne!(
            block(&vendor, Default::default()),
            want,
            "an unbound vendor-named tree must NOT generate the right block"
        );

        // BOUND — proposed from the vendor names themselves — it is identical.
        let bindings = bind::propose(&codegen_node_ids("stm32f4"), &vendor);
        assert_eq!(bindings["sw"], "SysClkSource", "{bindings:?}");
        assert_eq!(block(&vendor, bindings), want);
    }

    /// A family with no RCC recipe gets a real starting point, not a dead end:
    /// a working default plus the shape of the code to write.
    #[test]
    fn a_family_without_a_recipe_gets_an_editable_skeleton() {
        // H5 has no recipe — and that is the chip whose 178-node tree cannot be
        // turned into code, so the skeleton is all it has.
        assert!(rcc_recipe("stm32h5").is_none());
        assert!(!generates_clock_code("stm32h5"));

        let s = graph_clock_block("stm32h5", &ClockConfig::None, false);
        assert!(
            s.contains("let p = embassy_stm32::init(Default::default())"),
            "it must still compile and run: {s}"
        );
        assert!(s.contains("// Clock: this chip's family has no generated RCC recipe"));
        assert!(
            s.contains("//       config.rcc.sys = rcc::Sysclk::PLL1_P;"),
            "the shape to uncomment is there"
        );
        // Nothing active is `mut`, so no project picks up a warning from it.
        assert!(!s.contains(
            "
    let mut config"
        ));
    }

    /// The markers appear ONLY in manual mode — that is what keeps every
    /// generated project's `main.rs` byte-for-byte what it was.
    #[test]
    fn markers_are_absent_unless_the_clock_is_hand_written() {
        use super::super::common::{CLOCK_BEGIN, CLOCK_END};
        let generated = graph_clock_block("stm32h5", &ClockConfig::None, false);
        assert!(!generated.contains(CLOCK_BEGIN) && !generated.contains(CLOCK_END));

        let manual = graph_clock_block("stm32h5", &ClockConfig::None, true);
        assert!(manual.contains(CLOCK_BEGIN) && manual.contains(CLOCK_END));
        assert!(manual.contains("let p = embassy_stm32::init"));

        // A family WITH a recipe is unaffected while generated.
        let f4 = graph_clock_block("stm32f4", &ClockConfig::None, false);
        assert!(!f4.contains(CLOCK_BEGIN));
    }

    /// THE fix: a chip whose family has no recipe still generates real clock
    /// code once it has a tree — and that code follows the tree.
    #[test]
    fn a_tree_generates_even_without_a_family_recipe() {
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, minimal_graph, model::NodeState,
        };
        assert!(rcc_recipe("stm32h5").is_none());

        let mut graph = minimal_graph();
        // HSE 8 MHz /1 x60 /2 = 240 MHz on SYSCLK.
        graph.node_mut("pllsrc").unwrap().state = NodeState::Index(1);
        graph.node_mut("plln").unwrap().state = NodeState::Value(60);
        graph.node_mut("sw").unwrap().state = NodeState::Index(2);
        let clock = ClockConfig::Graph(GraphClock {
            graph,
            layout: Default::default(),
            bindings: Default::default(),
        });

        let s = graph_clock_block("stm32h5", &clock, false);
        // Active code, not a comment to uncomment.
        assert!(s.contains("let mut config = embassy_stm32::Config::default();"));
        assert!(s.contains("let p = embassy_stm32::init(config);"));
        assert!(!s.contains("has no generated RCC recipe yet"));
        // And it says what the tree says.
        assert!(s.contains("SYSCLK 240 MHz"), "{s}");
        assert!(s.contains("mul: rcc::PllMul::MUL60,"), "{s}");
        assert!(s.contains("Hertz(8000000)"), "{s}");
        // Honest about the one thing it guessed.
        assert!(s.contains("no verified RCC recipe for this family"));
        // Which in turn flips the hand-written default off, so it UPDATES.
        assert!(generates_clock_code_for("stm32h5", &clock));
        assert!(!generates_clock_code_for("stm32h5", &ClockConfig::None));
    }

    /// Peripheral kernel clocks reach `main.rs` — the whole of layer 1.
    ///
    /// The shipped WBA55 tree carries 21 mux nodes. Before this, every one of
    /// them was a control the user could move and nothing downstream read.
    #[test]
    fn peripheral_clock_selections_reach_the_generated_block() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let block = graph_clock_block("stm32wba", &ClockConfig::Graph(gc), false);

        // The three names that had to be right, and could not be guessed.
        assert!(
            block.contains("config.rcc.mux.usart1sel = rcc::mux::Usart1sel::PCLK2;"),
            "{block}"
        );
        assert!(
            block.contains("config.rcc.mux.usart2sel = rcc::mux::Usartsel::PCLK1;"),
            "the enum is NOT the field capitalised:
{block}"
        );
        assert!(
            block.contains("config.rcc.mux.adcsel = rcc::mux::Adcsel::HCLK4;"),
            "{block}"
        );

        // Placement: inside the `use embassy_stm32::rcc;` scope, before init.
        let scope = block.find("use embassy_stm32::rcc;").expect("the scope");
        let init = block.find("embassy_stm32::init(config)").expect("the init");
        let mux = block.find("config.rcc.mux.").expect("a mux line");
        assert!(
            scope < mux && mux < init,
            "a mux line lands out of scope:
{block}"
        );

        // The prescalers still come first, so the block reads sources ->
        // buses -> peripherals like the datasheet does.
        let apb = block
            .find("config.rcc.apb1_pre")
            .expect("the APB1 prescaler");
        assert!(
            apb < mux,
            "peripherals come after the buses:
{block}"
        );
    }

    /// A tree that selects a peripheral clock must not take the
    /// `init(Default::default())` shortcut — there is nowhere in it to say so.
    #[test]
    fn a_peripheral_selection_defeats_the_reset_shortcut() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        // With no graph at all the values ARE the reset, and the shortcut is
        // exactly right: nothing has been configured.
        let bare = graph_clock_block("stm32f4", &ClockConfig::None, false);
        assert!(bare.contains("init(Default::default())"), "{bare}");

        // A tree that moves a peripheral cannot use it, whatever the buses do.
        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).unwrap();
        let block = graph_clock_block("stm32wba", &ClockConfig::Graph(gc), false);
        assert!(block.contains("config.rcc.mux."), "{block}");
        assert!(
            !block.contains("init(Default::default())"),
            "a configured peripheral cannot be expressed by the shortcut:
{block}"
        );
    }

    /// A PLL that just multiplies is a SHAPE, not a missing piece.
    ///
    /// F0/F1/F3 have no PLL output divider at all. The generic recipe used to
    /// key on one and refuse the tree, which is how an STM32F358 imported with
    /// its whole clock tree still generated no clock code.
    #[test]
    fn a_pll_with_no_output_divider_is_its_own_shape() {
        use crate::panels::mcu_module::clock::graph::{
            ClockGraph, GraphClock, minimal_graph,
            model::{NodeKind, NodeState},
        };

        // The minimal tree with its PLL output divider removed — F3's topology.
        let mut g = minimal_graph();
        g.nodes.retain(|n| n.id != "pllp");
        for e in &mut g.edges {
            if e.from == "pllp" {
                e.from = "plln".into();
            }
        }
        g.edges.retain(|e| e.to != "pllp");
        // 8 MHz HSI, as on those families.
        g.node_mut("hsi").unwrap().state = NodeState::Source {
            enabled: true,
            hz: 8_000_000,
        };

        let (spec, desc) = generic_recipe(&g).expect("a PLL without a divider still generates");
        assert_eq!(desc.pll_source_field, "src", "f013 spells it `src`");
        assert!(desc.pll_out_field.is_empty(), "there is nothing to divide");
        assert_eq!(spec.hsi_hz, 8_000_000, "read from the tree, not assumed");

        // Select the PLL and check what comes out.
        g.node_mut("pllsrc").unwrap().state = NodeState::Index(1); // HSE
        g.node_mut("plln").unwrap().state = NodeState::Value(9);
        g.node_mut("sw").unwrap().state = NodeState::Index(2);
        let clock = ClockConfig::Graph(GraphClock {
            graph: g,
            layout: Default::default(),
            bindings: Default::default(),
        });
        let block = graph_clock_block("stm32f3", &clock, false);
        assert!(block.contains("src: rcc::PllSource::HSE,"), "{block}");
        assert!(block.contains("mul: rcc::PllMul::MUL9,"), "{block}");
        assert!(
            !block.contains("divp") && !block.contains("divq") && !block.contains("divr"),
            "those fields do not exist on this family:\n{block}"
        );
        // 8 MHz HSE /1 x9 = 72 MHz, the classic F1/F3 maximum.
        assert!(block.contains("SYSCLK 72 MHz"), "{block}");

        // And an empty graph is still refused — the spine is what makes it work.
        let empty = ClockGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        assert!(generic_recipe(&empty).is_none());
        let _ = NodeKind::Tap;
    }

    /// The minimal tree names `pllp`, a CubeMX-style one `pllr` — the tree picks
    /// the embassy spelling, which is the whole basis of the generic recipe.
    #[test]
    fn the_tree_picks_the_embassy_shape() {
        use crate::panels::mcu_module::clock::graph::minimal_graph;

        let p = minimal_graph();
        let (spec, desc) = generic_recipe(&p).expect("the spine is there");
        assert_eq!(spec.pll_out_node, "pllp");
        assert!(!desc.pll_source_nested, "F2/F4/F7 spelling");
        assert_eq!(spec.apb.len(), 2);

        let mut r = minimal_graph();
        r.node_mut("pllp").unwrap().id = "pllr".into();
        for e in &mut r.edges {
            if e.from == "pllp" {
                e.from = "pllr".into();
            }
            if e.to == "pllp" {
                e.to = "pllr".into();
            }
        }
        let (spec, desc) = generic_recipe(&r).unwrap();
        assert_eq!(spec.pll_out_node, "pllr");
        assert!(desc.pll_source_nested, "the modern nested-Pll spelling");

        // A tree with no spine is not guessed at.
        let empty = crate::panels::mcu_module::clock::graph::ClockGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        assert!(generic_recipe(&empty).is_none());
    }

    /// Every Espressif family the IDE ships. Naming only `esp32c3` — which all
    /// three of these predicates once did — is exactly the bug: eight parts
    /// added later were told they had "no clock code" while their `main.rs`
    /// carried `with_cpu_clock(…)`, and were offered a manual-clock switch whose
    /// preservation the ESP path cannot honour.
    const ESP_FAMILIES: [&str; 9] = [
        "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32s2",
        "esp32s3",
    ];

    /// Which families can be hand-written, and which already generate.
    #[test]
    fn the_family_predicates_agree_with_the_paths_that_exist() {
        for f in ["stm32f4", "stm32wba", "stm32g0", "stm32f1"] {
            assert!(generates_clock_code(f), "{f} generates its clock");
        }
        for f in ESP_FAMILIES {
            assert!(generates_clock_code(f), "{f} generates its clock");
        }
        for f in ["stm32h5", "stm32h7", "stm32u5", "stm32f3"] {
            assert!(!generates_clock_code(f), "{f} has no generator yet");
        }
        // The switch is offered only where the block is marker-wrapped.
        assert!(supports_manual_clock("stm32h5"));
        assert!(supports_manual_clock("stm32f4"));
        assert!(!supports_manual_clock("stm32f1"));
        for f in ESP_FAMILIES {
            assert!(!supports_manual_clock(f), "{f} was offered manual clock");
        }
        assert!(generates_clock_code("nrf52833"));
        assert!(
            !supports_manual_clock("nrf52833"),
            "nrf was offered manual clock"
        );
    }

    /// A hand-written block survives regeneration; going back to generated
    /// discards it.
    #[test]
    fn a_hand_written_block_survives_regeneration() {
        use super::super::common::keep_manual_clock;

        let generated = graph_clock_block("stm32h5", &ClockConfig::None, true);
        let edited = generated.replace(
            "let p = embassy_stm32::init(Default::default()); // reset clock (HSI)",
            "let p = embassy_stm32::init(my_config()); // 250 MHz, by hand",
        );
        let existing = format!(
            "// header
{edited}// rest of the file
"
        );

        // Regenerating in manual mode keeps the edit…
        let kept = keep_manual_clock(&existing, generated.clone(), true);
        assert!(kept.contains("my_config()"), "{kept}");
        // …and turning the switch off puts the generated block back.
        let dropped = keep_manual_clock(&existing, generated.clone(), false);
        assert!(!dropped.contains("my_config()"));

        // First time in manual mode there is nothing to keep, so the freshly
        // generated block stays — the seed the user then edits.
        let fresh = keep_manual_clock(
            "// no markers here
",
            generated.clone(),
            true,
        );
        assert_eq!(fresh, generated);
    }

    /// The editor asks which node ids code generation reads, so it can mark them
    /// and report the ones that went missing. They must match what the readers
    /// actually look up.
    #[test]
    fn codegen_node_ids_match_what_the_readers_look_up() {
        let f4 = codegen_node_ids("stm32f4");
        for id in [
            "hse", "sw", "pllsrc", "pllm", "plln", "ahb", "pllp", "apb1", "apb2",
        ] {
            assert!(f4.contains(&id), "F4 reads `{id}`: {f4:?}");
        }
        assert!(!f4.contains(&"pllr"), "that is the WBA/G4 output leg");

        // WBA's PLL output is R, and it has the extra APB7 bus.
        let wba = codegen_node_ids("stm32wba");
        assert!(wba.contains(&"pllr") && wba.contains(&"apb7"), "{wba:?}");

        // G0 has a single APB bus — the list follows the ReadSpec, not a guess.
        let g0 = codegen_node_ids("stm32g0");
        assert!(g0.contains(&"apb1") && !g0.contains(&"apb2"), "{g0:?}");

        // The families on their own generators.
        let f1 = codegen_node_ids("stm32f1");
        assert!(f1.contains(&"pllmul"));
        // `pll_input` is a computed frequency, not a node the bridge reads;
        // listing it would make the editor ask for an impossible binding.
        assert!(!f1.contains(&"pll_input"), "{f1:?}");
        // Every Espressif part, not just the one that was here first: with no
        // id to bind, nothing renames the `cpu` node the generator reads.
        for f in ESP_FAMILIES {
            assert_eq!(codegen_node_ids(f), vec!["cpu"], "{f}");
        }
        // Not an STM32: nothing here would ever emit `embassy_stm32` config for
        // it, so there is nothing to protect.
        assert!(codegen_node_ids("stm8").is_empty());

        // An STM32 family with no recipe still has live ids — `generic_recipe`
        // reads them off the tree. Without them an imported vendor tree keeps
        // its own node names, binds nothing, and generates nothing.
        let h5 = codegen_node_ids("stm32h5");
        for id in ["hse", "sw", "pllsrc", "pllm", "plln", "ahb", "apb1", "apb2"] {
            assert!(h5.contains(&id), "H5 needs `{id}` bound: {h5:?}");
        }
        assert!(
            h5.contains(&"pllp") && h5.contains(&"pllr"),
            "both spellings offered — the tree picks: {h5:?}"
        );
    }

    #[test]
    fn family_dispatch_selects_the_recipe_without_topology_sniffing() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32f4_graph, stm32wba_graph};

        // F4 family + the shipped F4 graph → the F4 100 MHz RCC block.
        let f4 = GraphClock {
            graph: stm32f4_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        };
        let s = graph_clock_block("stm32f4", &ClockConfig::Graph(f4), false);
        assert!(s.contains("config.rcc.sys = rcc::Sysclk::PLL1_P;"), "{s}");
        assert!(
            s.contains("SYSCLK 100 MHz (HSI16 /8 x100 /2 via PLLP)"),
            "{s}"
        );

        // An F4 HSE-PLL config emits the HSE oscillator block + HSE source
        // (was `f4::hse_pll_emits_hse_block_and_source`, now family-dispatched).
        use crate::panels::mcu_module::clock::graph::NodeState;
        let mut hse = stm32f4_graph();
        hse.node_mut("hse").unwrap().state = NodeState::Source {
            enabled: true,
            hz: 25_000_000,
        };
        hse.node_mut("pllsrc").unwrap().state = NodeState::Index(1); // HSE
        hse.node_mut("pllm").unwrap().state = NodeState::Index(5); // /25
        hse.node_mut("plln").unwrap().state = NodeState::Value(160);
        let s = graph_clock_block(
            "stm32f4",
            &ClockConfig::Graph(GraphClock {
                graph: hse,
                layout: Default::default(),
                bindings: Default::default(),
            }),
            false,
        );
        assert!(s.contains("config.rcc.hse = Some(rcc::Hse { freq: embassy_stm32::time::Hertz(25000000), mode: rcc::HseMode::Oscillator });"), "{s}");
        assert!(
            s.contains("config.rcc.pll_src = rcc::PllSource::HSE;"),
            "{s}"
        );
        assert!(s.contains("mul: rcc::PllMul::MUL160,"), "{s}");

        // WBA family + the shipped WBA graph → the WBA 100 MHz RCC block.
        let wba = GraphClock {
            graph: stm32wba_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        };
        let s = graph_clock_block("stm32wba", &ClockConfig::Graph(wba), false);
        assert!(s.contains("config.rcc.sys = rcc::Sysclk::PLL1_R;"), "{s}");
        assert!(s.contains("VoltageScale::RANGE1"), "{s}");

        // A family with no recipe (e.g. h7 until one lands) generates from the
        // tree it carries, via `generic_recipe`. It used to get the reset init
        // no matter what the tree said — which meant a configured Clock tab
        // produced no clock code at all.
        let g = GraphClock {
            graph: stm32f4_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        };
        let s = graph_clock_block("stm32h7", &ClockConfig::Graph(g), false);
        assert!(
            !s.contains("embassy_stm32::init(Default::default())"),
            "{s}"
        );
        assert!(s.contains("no verified RCC recipe for this family"), "{s}");
        assert!(s.contains("let p = embassy_stm32::init(config);"), "{s}");

        // Non-graph clock → reset init.
        assert!(
            graph_clock_block("stm32f4", &ClockConfig::None, false)
                .contains("embassy_stm32::init(Default::default())")
        );
    }

    #[test]
    fn g4_recipe_reads_the_preset_and_emits_valid_rcc() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32g4_graph};

        // The shipped G4 default = HSI16 /4 ×75 /2 → 150 MHz, all buses /1.
        let v = read_rcc_values(&stm32g4_graph(), &ReadSpec::g4());
        assert_eq!(v.sys, SysSource::Pll);
        assert_eq!(v.sysclk_hz, 150_000_000);
        assert_eq!((v.pll_m, v.pll_n, v.pll_out), (4, 75, 2));
        assert!(!v.pll_src_hse);
        assert_eq!(v.apb, vec![("apb1_pre", 1), ("apb2_pre", 1)]);

        // Emitted block: G4 nests the source, uses divr / PLL1_R, no frac, no
        // voltage_scale, and a real init(config).
        let s = graph_clock_block(
            "stm32g4",
            &ClockConfig::Graph(GraphClock {
                graph: stm32g4_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
            false,
        );
        for needle in [
            "source: rcc::PllSource::HSI,",
            "prediv: rcc::PllPreDiv::DIV4,",
            "mul: rcc::PllMul::MUL75,",
            "divr: Some(rcc::PllRDiv::DIV2),",
            "config.rcc.sys = rcc::Sysclk::PLL1_R;",
            "config.rcc.apb1_pre = rcc::APBPrescaler::DIV1;",
            "let p = embassy_stm32::init(config);",
            "SYSCLK 150 MHz (HSI16 /4 x75 /2 via PLLR)",
        ] {
            assert!(s.contains(needle), "missing: {needle}\n\n{s}");
        }
        // G4 has no separate pll_src line (nested), no frac, no voltage scale.
        assert!(!s.contains("config.rcc.pll_src ="));
        assert!(!s.contains("frac:"));
        assert!(!s.contains("voltage_scale"));
    }

    #[test]
    fn f2_and_f7_share_the_f4_recipe_byte_for_byte() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32f4_graph};
        let gc = || {
            ClockConfig::Graph(GraphClock {
                graph: stm32f4_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            })
        };
        // Same embassy rcc module (f247.rs) → identical emitted RCC block.
        let f4 = graph_clock_block("stm32f4", &gc(), false);
        assert_eq!(graph_clock_block("stm32f7", &gc(), false), f4);
        assert!(f4.contains("config.rcc.sys = rcc::Sysclk::PLL1_P;"));

        // The F2 is byte-identical too — but only for an N it can encode. This
        // test used to feed it the F4 preset (N=100) and assert equality, which
        // is what let `PllMul::MUL144` reach a real project: 100 is below the
        // F2's floor of 192 just as 144 is. Its own graph carries a legal one.
        use crate::panels::mcu_module::clock::graph::{stm32f2_graph, stm32f2_layout};
        let f2 = graph_clock_block(
            "stm32f2",
            &ClockConfig::Graph(GraphClock {
                graph: stm32f2_graph(),
                layout: stm32f2_layout(),
                bindings: Default::default(),
            }),
            false,
        );
        assert!(
            !f2.contains("!!"),
            "the F2 default must be encodable:
{f2}"
        );
        // Same shape, same 100 MHz, different (M, N) to get there.
        assert!(f2.contains("config.rcc.sys = rcc::Sysclk::PLL1_P;"));
        assert!(
            f2.contains("SYSCLK 100 MHz (HSI16 /16 x200 /2 via PLLP)"),
            "{f2}"
        );
        assert_eq!(
            f2.replace("DIV16", "DIV8").replace("MUL200", "MUL100"),
            f4.replace("/8 x100", "/16 x200"),
        );
    }

    #[test]
    fn l4_recipe_emits_hsi_enable_and_never_the_msi_reset_default() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, NodeState, stm32l4_graph};

        // Shipped 80 MHz preset: HSI16 /1 ×10 /2 → 80 MHz.
        let s = graph_clock_block(
            "stm32l4",
            &ClockConfig::Graph(GraphClock {
                graph: stm32l4_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
            false,
        );
        for needle in [
            "config.rcc.hsi = true;", // L4 boots with HSI off — must switch it on
            "source: rcc::PllSource::HSI,",
            "mul: rcc::PllMul::MUL10,",
            "divr: Some(rcc::PllRDiv::DIV2),",
            "config.rcc.sys = rcc::Sysclk::PLL1_R;",
            "SYSCLK 80 MHz (HSI16 /1 x10 /2 via PLLR)",
        ] {
            assert!(s.contains(needle), "missing: {needle}\n\n{s}");
        }

        // A reset-equivalent L4 graph (HSI sysclk, all /1) must STILL emit an
        // explicit HSI block — NOT init(Default::default()), which on L4 is MSI.
        let mut g = stm32l4_graph();
        g.node_mut("sw").unwrap().state = NodeState::Index(0); // HSI direct
        let s2 = graph_clock_block(
            "stm32l4",
            &ClockConfig::Graph(GraphClock {
                graph: g,
                layout: Default::default(),
                bindings: Default::default(),
            }),
            false,
        );
        assert!(s2.contains("config.rcc.hsi = true;"), "{s2}");
        assert!(s2.contains("config.rcc.sys = rcc::Sysclk::HSI;"), "{s2}");
        assert!(
            !s2.contains("init(Default::default())"),
            "L4 reset must be explicit, not MSI default\n\n{s2}"
        );
    }

    #[test]
    fn g0_recipe_reads_the_preset_and_emits_a_single_apb_bus() {
        use crate::panels::mcu_module::clock::graph::{GraphClock, stm32g0_graph};

        // The shipped G0 default = HSI16 /1 ×8 /2 → 64 MHz, single APB /1.
        let v = read_rcc_values(&stm32g0_graph(), &ReadSpec::g0());
        assert_eq!(v.sys, SysSource::Pll);
        assert_eq!(v.sysclk_hz, 64_000_000);
        assert_eq!((v.pll_m, v.pll_n, v.pll_out), (1, 8, 2));
        assert_eq!(v.apb, vec![("apb1_pre", 1)]); // ONE bus — no apb2

        let s = graph_clock_block(
            "stm32g0",
            &ClockConfig::Graph(GraphClock {
                graph: stm32g0_graph(),
                layout: Default::default(),
                bindings: Default::default(),
            }),
            false,
        );
        for needle in [
            "source: rcc::PllSource::HSI,",
            "mul: rcc::PllMul::MUL8,",
            "divr: Some(rcc::PllRDiv::DIV2),",
            "config.rcc.sys = rcc::Sysclk::PLL1_R;",
            "config.rcc.apb1_pre = rcc::APBPrescaler::DIV1;",
            "SYSCLK 64 MHz (HSI16 /1 x8 /2 via PLLR)",
        ] {
            assert!(s.contains(needle), "missing: {needle}\n\n{s}");
        }
        // G0 has no APB2 bus.
        assert!(!s.contains("apb2_pre"), "G0 must not emit apb2_pre\n\n{s}");
    }

    #[test]
    fn hse_direct_sysclk_emits_no_pll_but_keeps_voltage_scale() {
        let v = RccValues {
            sys: SysSource::Hse,
            hse_on: true,
            hse_hz: 32_000_000,
            pll_src_hse: false,
            pll_m: 2,
            pll_n: 25,
            pll_out: 4,
            ahb: 1,
            sysclk_hz: 32_000_000,
            apb: vec![("apb1_pre", 1), ("apb2_pre", 1), ("apb7_pre", 1)],
        };
        let s = emit_rcc_block(&RccDescriptor::wba(), &v);
        assert!(s.contains("config.rcc.sys = rcc::Sysclk::HSE;"));
        assert!(!s.contains("config.rcc.pll1"));
        assert!(s.contains("VoltageScale::RANGE1"));
    }

    /// The F1 on the Async runtime, read from the typed clock. The default tree
    /// is the block compiled on the F103C8; an HSI PLL is always halved, since
    /// embassy panics on any other `prediv` there; the ADC prescaler is written
    /// only off embassy's own /6.
    #[test]
    fn the_f1_embassy_block_follows_the_typed_clock() {
        use super::super::super::clock::model::{PllSrc, Stm32f1Clock};
        let blue_pill = f1_embassy_block("STM32F103C8T6", &Stm32f1Clock::default());
        for line in [
            "src: rcc::PllSource::HSE,",
            "prediv: rcc::PllPreDiv::DIV1,",
            "mul: rcc::PllMul::MUL9,",
            "config.rcc.sys = rcc::Sysclk::PLL1_P;",
        ] {
            assert!(blue_pill.contains(line), "{line}\n\n{blue_pill}");
        }
        assert!(!blue_pill.contains("adc_pre"), "{blue_pill}");
        assert!(!blue_pill.contains("!!"), "{blue_pill}");

        let hsi = Stm32f1Clock {
            pll_src: PllSrc::HsiDiv2,
            pll_mul: 16,
            adc_pre: 8,
            ..Stm32f1Clock::default()
        };
        let hsi = f1_embassy_block("STM32F103C8T6", &hsi);
        assert!(hsi.contains("src: rcc::PllSource::HSI,"), "{hsi}");
        assert!(hsi.contains("prediv: rcc::PllPreDiv::DIV2,"), "{hsi}");
        assert!(
            hsi.contains("config.rcc.adc_pre = rcc::ADCPrescaler::DIV8;"),
            "{hsi}"
        );
        // MUL16 is legal on the F103.
        assert!(!hsi.contains("!!"), "{hsi}");
    }

    /// The connectivity line (F105/F107) stops at MUL9, so the F103's MUL16 is
    /// flagged there - with the advice for a TOO BIG N, not the F2's "raise
    /// PLLM", which would only make it bigger.
    #[test]
    fn the_connectivity_line_flags_a_multiplier_above_nine() {
        use super::super::super::clock::model::{PllSrc, Stm32f1Clock};
        let x16 = Stm32f1Clock {
            pll_src: PllSrc::HsiDiv2,
            pll_mul: 16,
            ..Stm32f1Clock::default()
        };
        for chip in ["STM32F105RCT6", "STM32F107VCT6"] {
            let out = f1_embassy_block(chip, &x16);
            assert!(out.contains("mul: rcc::PllMul::MUL16,"), "{out}");
            assert!(out.contains("outside this chip's range 4..=9"), "{out}");
            assert!(out.contains("faster input"), "{out}");
            assert!(!out.contains("Raise PLLM"), "{out}");
            // x9 is the Blue Pill's own multiplier and legal on both lines.
            let x9 = f1_embassy_block(chip, &Stm32f1Clock::default());
            assert!(!x9.contains("!!"), "{chip}\n\n{x9}");
        }
    }

    /// embassy's F1 init asserts, mirrored on the emitted config: the default
    /// tree boots, a bus over its ceiling does not, and a PLL no one selects is
    /// not configured - so it cannot trip anything.
    #[test]
    fn embassy_accepts_the_f1_clocks_its_init_does_not_assert_on() {
        use super::super::super::clock::model::{PllSrc, Stm32f1Clock, SysclkSrc};
        let ok = |c: Stm32f1Clock| f1_embassy_clock_ok(&c);
        assert!(ok(Stm32f1Clock::default()));
        let base = Stm32f1Clock::default;
        assert!(
            !ok(Stm32f1Clock {
                apb1_pre: 1,
                ..base()
            }),
            "PCLK1 72 > 36"
        );
        assert!(
            !ok(Stm32f1Clock {
                adc_pre: 2,
                ..base()
            }),
            "ADC 36 > 14"
        );
        assert!(
            !ok(Stm32f1Clock {
                pll_mul: 10,
                ..base()
            }),
            "PLL 80 > 72"
        );
        assert!(
            !ok(Stm32f1Clock {
                pll_src: PllSrc::HsiDiv2,
                pll_mul: 2,
                ..base()
            }),
            "PLL 8 < 16"
        );
        // The same over-fast PLL, left unselected: the block writes no PLL.
        assert!(ok(Stm32f1Clock {
            pll_mul: 10,
            sysclk_src: SysclkSrc::Hse,
            ..base()
        }));
        // The HSE feeds the PLL even with the tab's HSE switch off: the block
        // turns it on, so the input is still 8 MHz.
        assert!(ok(Stm32f1Clock {
            hse_enabled: false,
            ..base()
        }));
    }

    /// `SwdOnly` lands between the RCC scope and the `init` that consumes the
    /// config, and a block with no such `init` is left alone.
    #[test]
    fn swd_only_goes_before_the_init() {
        use super::super::super::clock::model::Stm32f1Clock;
        let block = f1_embassy_block("STM32F103C8T6", &Stm32f1Clock::default());
        let out = with_swd_only(&block);
        let swj = out.find("config.swj = embassy_stm32::gpio::SwjCfg::SwdOnly;");
        let init = out.find("let p = embassy_stm32::init(config);");
        assert!(matches!((swj, init), (Some(s), Some(i)) if s < i), "{out}");
        assert_eq!(out.matches("SwdOnly").count(), 1, "{out}");
        let untouched = "let p = embassy_stm32::init(Default::default());\n";
        assert_eq!(with_swd_only(untouched), untouched);
    }
}
