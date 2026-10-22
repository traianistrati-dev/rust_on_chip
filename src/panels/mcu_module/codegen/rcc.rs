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
    // Whether HSI must be switched on is the FAMILY's (embassy's default),
    // decided in `generic_recipe_for`; the tree cannot say it.
    // The internal RC is 8 MHz on some families and 16 on others, and the tree
    // knows which. It only reaches the header comment — embassy computes the
    // real frequency itself — but a comment that says 16 MHz on an 8 MHz part
    // is a comment that will be believed.
    // A CubeMX H5/H7 binds its HSIDiv as `hsi`: the frequency is the source's
    // - when that source IS the HSI. On a U5 the node bound as `hsi` can be
    // `HSI48DivToRNG`, and its source the 48 MHz RC.
    let hsi_src = match g.node("hsi").map(|n| &n.state) {
        Some(NodeState::Source { hz, .. }) => Some(*hz),
        _ => g
            .edges
            .iter()
            .find(|e| e.to == "hsi")
            .filter(|e| classify_source(&e.from) == MuxSource::Hsi)
            .and_then(|e| g.node(&e.from))
            .and_then(|n| match &n.state {
                NodeState::Source { hz, .. } => Some(*hz),
                _ => None,
            }),
    };
    if let Some(hz) = hsi_src.as_ref() {
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
    graph_clock_block_with(chip, family, clock, manual, 0)
}

/// The same, told which SPI blocks the project runs as I2S (bit *n* for
/// SPI*n*): their kernel clock is an audio clock, so a stopped one gets no
/// stand-in ([`rcc_mux::Running::i2s`](super::rcc_mux::Running::i2s)).
pub fn graph_clock_block_with(
    chip: &str,
    family: &str,
    clock: &ClockConfig,
    manual: bool,
    i2s: u8,
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
    // What embassy's own `Config::default()` - the skeleton's active code -
    // needs on this part: the selectors whose default is a stopped clock. On
    // a WBA with SAI1, `init(Default::default())` itself panics.
    let floor = super::rcc_mux::lines_without_a_tree(
        chip,
        family,
        &super::rcc_mux::Running {
            i2s,
            ..super::rcc_mux::Running::defaults(family)
        },
    );
    // A family that reads no tree at all (U3, C0) is that, whatever the tree
    // selects - blaming the source would suggest a fix that does not exist.
    if rcc_recipe(family).is_none() && !generic_allowed(family) {
        return wrap(
            hand_written_skeleton(family, Skeleton::NoRecipe, manual, &floor),
            manual,
        );
    }
    // A tree that selects a source no emitter writes gets the skeleton.
    if graph.is_some_and(|g| !tree_sources_supported(g)) {
        return wrap(
            hand_written_skeleton(family, Skeleton::Source, manual, &floor),
            manual,
        );
    }
    // The family's verified recipe first; the tree's own shape only as a
    // fallback, so no existing family's output can change.
    let (spec, desc, verified) = match rcc_recipe(family) {
        Some((spec, desc)) => (spec, desc, true),
        None => match graph.and_then(|g| generic_recipe_for(family, g)) {
            Some((spec, desc)) => (spec, desc, false),
            None => {
                let why = match graph.and_then(|g| generic_refusal_for(family, g)) {
                    Some(refusal) => Skeleton::Shape(refusal),
                    None => Skeleton::NoRecipe,
                };
                return wrap(hand_written_skeleton(family, why, manual, &floor), manual);
            }
        },
    };
    let values = match graph {
        Some(g) => read_rcc_values(g, &spec),
        None => spec.reset.clone(),
    };
    // Per-peripheral kernel clocks: USART1SEL, I2C1SEL, ADCSEL and the rest.
    // The Clock tab has always shown these; until now nothing read them. Only
    // onto a source this block leaves running (`emit_rcc_block` below): the
    // HSI it switches on, the HSE it starts, the PLL1 output it enables.
    let hsi_used =
        values.sys == SysSource::Hsi || (values.sys == SysSource::Pll && !values.pll_src_hse);
    let running = super::rcc_mux::Running {
        hsi: super::rcc_mux::hsi_by_default(family) || (desc.hsi_needs_enable && hsi_used),
        // What the block STARTS (`emit_rcc_block`), not the tree's tick: an
        // imported tree has its HSE ticked whatever SYSCLK runs on.
        hse: hse_used(&values),
        pll1: (values.sys == SysSource::Pll).then_some(match desc.pll_out_field {
            "divq" => 'q',
            "divr" => 'r',
            // `divp`, or none at all: F0/F1/F3 report their PLL as `pll1_p`.
            _ => 'p',
        }),
        i2s,
        sys_hz: values.sysclk_hz,
    };
    let mux = match graph {
        Some(g) => super::rcc_mux::emit_lines(g, chip, family, &running),
        None => super::rcc_mux::lines_without_a_tree(chip, family, &running),
    };
    // The reset shortcut only applies when the chip's HW default equals this
    // reset (HSI). L4/L5/U5 default to MSI, so a reset-equivalent graph must
    // still emit an explicit HSI block, not `init(Default::default())`.
    //
    // A peripheral selection also disqualifies it: the buses may be at reset
    // while USART1 has been moved to HSI, and `init(Default::default())` has
    // nowhere to say so. So does a WBA with SAI1, which panics in `init` on
    // the default `sai1sel`.
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
/// The F1's SYSCLK as [`f1_embassy_block`] programs it: the crystal's
/// frequency even where the tree leaves the HSE node off, because the block
/// switches HSE on whenever SYSCLK or the PLL runs from it.
pub fn f1_embassy_sysclk_hz(c: &super::super::clock::model::Stm32f1Clock) -> u32 {
    f1_embassy_sys(c).4
}

/// `(source, PLL from HSE, pll_m, pll_n, SYSCLK Hz)` of the F1 on embassy.
fn f1_embassy_sys(
    c: &super::super::clock::model::Stm32f1Clock,
) -> (SysSource, bool, u32, u32, u32) {
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
    (sys, pll_src_hse, pll_m, pll_n, sysclk_hz)
}

fn f1_embassy_block(chip: &str, c: &super::super::clock::model::Stm32f1Clock) -> String {
    let (sys, pll_src_hse, pll_m, pll_n, sysclk_hz) = f1_embassy_sys(c);
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
        "    // chip. If it doesn't, tick \"Write the clock by hand\" in the Clock tab",
        "    // first, then fix it here: until then this block is regenerated.",
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

/// `block` for a dual-core part built for one core, whose chip feature is
/// `feature` (`stm32wl55jc-cm4`).
///
/// embassy-stm32 under `_dual-core` has no `init` (`lib.rs`:
/// `cfg(not(feature = "_dual-core"))`), only `init_primary`, which keeps the
/// clock state in a `SharedData` the other core could read. So every `init`
/// the block writes, the commented example included, becomes that call, and
/// the static it needs comes first. Outside the manual markers: a hand-written
/// clock then needs only the call, and the static is regenerated with the
/// rest. Plain `.bss`, zeroed by cortex-m-rt - a fixed shared address only
/// matters once a firmware for the other core exists, and this IDE writes one.
pub fn for_dual_core(block: &str, feature: &str) -> String {
    let body = block
        .replace(
            "embassy_stm32::init(config)",
            "embassy_stm32::init_primary(config, &SHARED_DATA)",
        )
        .replace(
            "embassy_stm32::init(Default::default())",
            "embassy_stm32::init_primary(Default::default(), &SHARED_DATA)",
        );
    debug_assert!(!body.contains("embassy_stm32::init("), "{body}");
    format!(
        concat!(
            "    // Dual-core part, built for one core (`{feature}`): embassy has no\n",
            "    // `init` there, only `init_primary`. SHARED_DATA is what it would share\n",
            "    // with the other core; this firmware programs one core only.\n",
            "{shared}{body}"
        ),
        feature = feature,
        shared = SHARED_DATA_STATIC,
        body = body,
    )
}

/// The static [`for_dual_core`] declares, as it writes it - so a caller can
/// take it out again when the user's own code already declares one.
pub const SHARED_DATA_STATIC: &str = concat!(
    "    static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData> =\n",
    "        core::mem::MaybeUninit::uninit();\n"
);

/// What a chip whose family has no RCC recipe gets: a WORKING default plus the
/// shape of the real thing, commented out.
///
/// The active line keeps the project compiling and warning-free; the comment
/// above it is the code to uncomment, in the same idiom the generated blocks
/// use, so setting the clock by hand is filling in numbers rather than looking
/// up embassy's API. Both are inside the manual markers, so the moment it is
/// edited the edit survives.
/// Why a block is the hand-written skeleton.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skeleton {
    /// The family has no recipe and the tree no shape the generic one reads.
    NoRecipe,
    /// The tree selects a source no emitter writes (MSI, CSI, …).
    Source,
    /// The family has no recipe, and what the tree selects is one the
    /// generic one does not write, for this reason.
    Shape(GenericRefusal),
}

/// What embassy's `Config::default()` clocks a family from - what the active
/// line of the skeleton really runs. On the MSI families it is NOT the HSI.
fn default_clock_of(family: &str) -> &'static str {
    match family {
        "stm32l0" | "stm32l1" | "stm32l4" | "stm32l4+" | "stm32l5" | "stm32wb" | "stm32wl"
        | "stm32u0" => "MSI",
        "stm32u5" | "stm32u3" => "MSIS",
        "stm32c0" => "HSISYS",
        _ => "HSI",
    }
}

/// The example is HSE straight to SYSCLK: `Sysclk::HSE` exists on every
/// family and needs no PLL - `PLL1_P` is not a variant on L1/WL/U5, and on the
/// H7 it is an `unwrap!` at boot with no `pll1` set. The WB and WL `Hse` takes
/// a prescaler as well.
///
/// "Kept across regeneration" only inside the manual markers. A tree that
/// moves to a selection no recipe writes reaches here with the switch still
/// off, and that block is regenerated - so it says how to keep one instead.
///
/// `floor` is what embassy's default clock needs written on this part
/// ([`lines_without_a_tree`](super::rcc_mux::lines_without_a_tree)): with
/// any, the active code is a `Config` carrying them, not `Default::default()`.
fn hand_written_skeleton(family: &str, why: Skeleton, manual: bool, floor: &[String]) -> String {
    let mut head: Vec<String> = match why {
        // Word for word what every existing hand-written project carries.
        Skeleton::NoRecipe if manual => vec![
            "    // Clock: this chip's family has no generated RCC recipe yet, so the".into(),
            "    // Clock tab's tree cannot be turned into code automatically. Set it".into(),
            "    // here — this block is kept across regeneration.".into(),
        ],
        Skeleton::NoRecipe => vec![
            "    // Clock: this chip's family has no generated RCC recipe yet, so the".into(),
            "    // Clock tab's tree cannot be turned into code automatically.".into(),
        ],
        Skeleton::Source => vec![
            "    // Clock: the Clock tab selects a clock source the generated recipes do".into(),
            "    // not write (MSI, CSI, HSI48, LSI, LSE, or an HSI or HSE behind a".into(),
            "    // divider), so its tree is not turned into code.".into(),
        ],
        Skeleton::Shape(refusal) => {
            let mut h: Vec<String> = vec![
                "    // Clock: this family has no generated RCC recipe yet, and the Clock tab"
                    .into(),
                "    // selects what only that recipe can write safely:".into(),
            ];
            // Plain comment lines: indented like the example below, a reader
            // uncommenting it would take the reason for code.
            h.extend(refusal.lines().iter().map(|l| format!("    // {l}")));
            h
        }
    };
    // What keeps an edit - honestly: only the markers do.
    if why != Skeleton::NoRecipe || !manual {
        head.push(if manual {
            "    // Set it here — this block is kept across regeneration.".into()
        } else {
            "    // To set it here, tick \"Write the clock by hand\" first: until then this".into()
        });
        if !manual {
            head.push("    // block is regenerated.".into());
        }
    }
    // The example's HSE, as the family's `Hse` spells it and at a frequency
    // the part can run. WBA's is only a prescaler (a fixed 32 MHz crystal),
    // and like the WBA recipe it needs RANGE1 off HSI. WB/WL's HSE32 is a
    // fixed 32 MHz too, with a prescaler. On a U5 or an H7 with OTG_HS (an
    // H7RS) 8 MHz is itself the panic - 16 is one the PHY takes, and every
    // H7 runs it.
    let hse = if family == "stm32wba" {
        concat!(
            "    //       config.rcc.hse = Some(rcc::Hse { prescaler: rcc::HsePrescaler::DIV1 });\n",
            "    //       config.rcc.voltage_scale = rcc::VoltageScale::RANGE1;\n",
        )
        .to_owned()
    } else {
        let mhz = match family {
            "stm32wb" | "stm32wl" => "32",
            "stm32u5" | "stm32h7" => "16",
            _ if why == Skeleton::Shape(GenericRefusal::UsbPhy) => "16",
            _ => "8",
        };
        let prescaler = if matches!(family, "stm32wb" | "stm32wl") {
            "    //           prescaler: rcc::HsePrescaler::DIV1,\n"
        } else {
            ""
        };
        format!(
            concat!(
                "    //       config.rcc.hse = Some(rcc::Hse {{\n",
                "    //           freq: embassy_stm32::time::Hertz({mhz}_000_000),\n",
                "    //           mode: rcc::HseMode::Oscillator,\n",
                "{prescaler}",
                "    //       }});\n",
            ),
            mhz = mhz,
            prescaler = prescaler,
        )
    };
    // The lines embassy's default clock needs on this part
    // (`rcc_mux::lines_without_a_tree`) go in the example AND in the active
    // code: `Default::default()` alone leaves a peripheral on a stopped clock,
    // or panics in `init` (a WBA's `sai1sel`).
    let example_floor: String = floor
        .iter()
        .map(|l| format!("    //       {}\n", l.trim_start()))
        .collect();
    let active = if floor.is_empty() {
        format!(
            concat!(
                "    // Then delete the line below.\n",
                "    let p = embassy_stm32::init(Default::default()); // embassy's default clock ({clock})\n",
            ),
            clock = default_clock_of(family),
        )
    } else {
        format!(
            concat!(
                "    // Then delete the lines below. Not `Default::default()`: on this part\n",
                "    // embassy's default `config.rcc.mux` selects clocks that do not run.\n",
                "    let mut config = embassy_stm32::Config::default();\n",
                "    {{\n",
                "        use embassy_stm32::rcc;\n",
                "{floor}\n",
                "    }}\n",
                "    let p = embassy_stm32::init(config); // embassy's default clock ({clock})\n",
            ),
            floor = floor.join("\n"),
            clock = default_clock_of(family),
        )
    };
    format!(
        concat!(
            "{head}\n",
            "    //\n",
            "    //   let mut config = embassy_stm32::Config::default();\n",
            "    //   {{\n",
            "    //       use embassy_stm32::rcc;\n",
            "{hse}",
            "    //       config.rcc.sys = {sys};\n",
            "{example_floor}",
            "    //   }}\n",
            "    //   let p = embassy_stm32::init(config);\n",
            "    //\n",
            "{active}",
        ),
        head = head.join("\n"),
        hse = hse,
        example_floor = example_floor,
        active = active,
        // N6's SYSCLK is its own `SysClk`, not the `Sysclk` of every other module.
        sys = if family == "stm32n6" {
            "rcc::SysClk::Hse"
        } else {
            "rcc::Sysclk::HSE"
        },
    )
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
    // The skeleton `graph_clock_block_for` falls back to for a tree that
    // selects MSI, CSI, …: no clock code, so the block is the user's.
    if family != "stm32f1"
        && matches!(clock, ClockConfig::Graph(gc) if !tree_sources_supported(&gc.for_codegen()))
    {
        return false;
    }
    generates_clock_code(family)
        || matches!(clock, ClockConfig::Graph(gc) if generic_recipe_for(family, &gc.for_codegen()).is_some())
}

/// Can this tree drive the clock code at SOME selection - the question the
/// hand-written default and the "missing pieces" reports ask, where
/// [`generates_clock_code_for`] answers for the selection it has now.
///
/// An import sits at its vendor default, MSI on the L/U/W families and HSI
/// /2 on an H5: the skeleton until HSI or the PLL is picked. Defaulting
/// those to "written by hand" fenced the block off for good - the switch is
/// saved only when on, so turning it off came back on at every open - and
/// reported the chip as missing a clock it has.
pub fn tree_drives_clock_code(family: &str, clock: &ClockConfig) -> bool {
    if family == "stm32n6" {
        return matches!(clock, ClockConfig::Graph(gc) if super::rcc_n6::block(&gc.for_codegen()).is_some());
    }
    generates_clock_code(family)
        || matches!(clock, ClockConfig::Graph(gc) if generic_allowed(family) && generic_recipe(&gc.for_codegen()).is_some())
}

/// Why THIS tree is the skeleton when its family does generate from trees -
/// what the Clock tab says instead of "no clock generator", which on an H7 at
/// its PLL is false: the same tree at HSI generates. `None` when the clock is
/// generated, or when the family reads no tree at all.
pub fn skeleton_reason(family: &str, clock: &ClockConfig) -> Option<String> {
    let ClockConfig::Graph(gc) = clock else {
        return None;
    };
    if !family.starts_with("stm32") || generates_clock_code_for(family, clock) {
        return None;
    }
    // U3, C0: no generator for any selection, which the tab says as that.
    if rcc_recipe(family).is_none() && !generic_allowed(family) {
        return None;
    }
    let g = gc.for_codegen();
    if family != "stm32f1" && !tree_sources_supported(&g) {
        return Some(
            "the tree selects a source no generated recipe writes (MSI, CSI, HSI48, LSI, LSE, or an HSI or HSE behind a divider)."
                .into(),
        );
    }
    let refusal = generic_refusal_for(family, &g)?;
    Some(format!(
        "the tree selects {} That needs this family's own recipe, not written yet.",
        refusal.lines().join(" ")
    ))
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
    /// `Pll { source, mul, div }`). Through the generic recipe no family gets
    /// its PLL written at all ([`generic_writes`]); the shape still decides
    /// the HSI label and frequency its other blocks say.
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
/// What a clock mux's SELECTED input really is, read from the edge that feeds
/// it rather than from the bare index.
///
/// The built-in trees put HSI, HSE and the PLL on inputs 0, 1 and 2. A tree
/// imported from CubeMX lists the sources in ST's own order, which on the MSI
/// families (L0/L1/L4/L5/U0/U5/WB/WL) starts with MSI - read by index, that
/// MSI became `Sysclk::HSI` with HSI never switched on, and embassy's
/// `hsi.unwrap()` panicked at boot. `None` when the mux is not in the tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MuxSource {
    Hsi,
    Hse,
    Pll,
    /// MSI, CSI, HSI48, HSISYS, LSI…: nothing an emitter here writes.
    Other,
}

fn mux_source(g: &ClockGraph, mux: &str) -> Option<MuxSource> {
    let NodeState::Index(i) = g.node(mux)?.state else {
        return None;
    };
    let Some(edge) = g.edges.iter().find(|e| e.to == mux && e.input == i) else {
        // No edge to ask: the index is all there is, in the built-in order.
        return Some(match i {
            0 => MuxSource::Hsi,
            1 => MuxSource::Hse,
            _ => MuxSource::Pll,
        });
    };
    // Walk back through dividers to the oscillator that feeds this input: a
    // CubeMX F0/F3 binds the HSE predivider as `pllm`, so the PLL source's
    // input is that divider, not `HSE`; a G0 feeds SYSCLK through HSISYS.
    let mut id = edge.from.clone();
    // Any division on the way, and a SELECTABLE one other than the PLL's own
    // input prescaler (`pllm`, which the block writes).
    let (mut divided, mut unwritten) = (false, false);
    for _ in 0..4 {
        if mux == "sw" && id.to_ascii_lowercase().contains("pll") {
            return Some(MuxSource::Pll);
        }
        let Some(node) = g.node(&id) else { break };
        match (&node.kind, &node.state) {
            (NodeKind::Divider { options }, NodeState::Index(k)) => {
                let by_more = options.get(*k).is_some_and(|d| *d != 1);
                divided |= by_more;
                unwritten |= by_more && id != "pllm";
            }
            (NodeKind::FixedDiv { by }, _) => divided |= *by != 1,
            _ => break,
        }
        match g.edges.iter().find(|e| e.to == id) {
            Some(up) => id = up.from.clone(),
            None => break,
        }
    }
    let class = classify_source(&id);
    // SYSCLK from a DIVIDED HSI (H5/H7 HSIDiv, G0 HSISYS, L0 HSIRCDiv): no
    // emitter writes that divider and embassy runs HSI undivided, so the block
    // would run a clock the tree does not show. Behind the PLL a divider is
    // the PLL's own input prescaler, which the block does write.
    // Behind the PLL a fixed divider is the HAL's (F0/F3 HSI/2) and `pllm` is
    // written; a selectable HSIDiv there is neither.
    if class == MuxSource::Hsi && ((mux == "sw" && divided) || unwritten) {
        return Some(MuxSource::Other);
    }
    // Nor does any write an HSE divider (WBA's HseDiv, an F0/F3 PLLDiv the
    // binding never names `pllm`): the firmware runs the HSE undivided.
    if class == MuxSource::Hse && unwritten {
        return Some(MuxSource::Other);
    }
    Some(class)
}

/// The oscillator a node id names. `48` in an HSI's name is the 48 MHz RC
/// (`HSI48`, `HSIRC48`, `HSIUSB48`), which no emitter writes.
fn classify_source(id: &str) -> MuxSource {
    let f = id.to_ascii_lowercase();
    if f.contains("msi") || (f.starts_with("hsi") && f.contains("48")) {
        MuxSource::Other
    } else if f.starts_with("hsi") {
        MuxSource::Hsi
    } else if f.starts_with("hse") || f.contains("xtpre") {
        // `pllxtpre`: the F1's HSE predivider, the PLL's HSE input.
        MuxSource::Hse
    } else if f.contains("pll") {
        MuxSource::Pll
    } else {
        MuxSource::Other
    }
}

/// Families the generic recipe must not write for: its `Sysclk::HSI` is
/// spelled `HSI16` on the U3 and `HSISYS` on the C0, and neither has a PLL.
pub fn generic_allowed(family: &str) -> bool {
    !matches!(family, "stm32u3" | "stm32c0")
}

/// The generic recipe for `family`'s tree, when what the tree selects is a
/// block that compiles AND boots there - the ONE answer
/// [`graph_clock_block_for`], [`generates_clock_code_for`], `cpu_clock` and
/// the Configuration tab's WWDG card all ask.
///
/// What it adds to [`generic_recipe`] is the family, which the tree cannot
/// give: whether embassy's default leaves HSI off, which APB buses its
/// `Config` has, and what [`generic_writes`] must refuse.
pub fn generic_recipe_for(family: &str, g: &ClockGraph) -> Option<(ReadSpec, RccDescriptor)> {
    let (spec, desc) = generic_recipe_adapted(family, g)?;
    generic_refusal(family, g, &read_rcc_values(g, &spec))
        .is_none()
        .then_some((spec, desc))
}

/// Why the generic recipe does not write `family`'s tree as it stands -
/// `None` when it does, or when there is no generic recipe for it at all.
fn generic_refusal_for(family: &str, g: &ClockGraph) -> Option<GenericRefusal> {
    let (spec, _) = generic_recipe_adapted(family, g)?;
    generic_refusal(family, g, &read_rcc_values(g, &spec))
}

/// [`generic_recipe`] with what only the family knows, before
/// [`generic_refusal`] decides.
fn generic_recipe_adapted(family: &str, g: &ClockGraph) -> Option<(ReadSpec, RccDescriptor)> {
    if !generic_allowed(family) {
        return None;
    }
    let (mut spec, mut desc) = generic_recipe(g)?;
    // embassy's own HSI, which no block sets - not the tree's. The minimal
    // tree's 16 MHz is 8 on an F0/F3 and 64 on an H5/H7, and an imported
    // L0/L1 tree (the f013 shape) would have said 8 for a 16 MHz HSI: the
    // header, `cpu_clock` and the Async boot check all read this.
    let (hsi_hz, hsi_label) = match family {
        "stm32f0" | "stm32f3" => (8_000_000, "HSI8"),
        "stm32h5" | "stm32h7" => (64_000_000, "HSI64"),
        _ => (16_000_000, "HSI16"),
    };
    spec.hsi_hz = hsi_hz;
    if spec.reset.sys == SysSource::Hsi {
        spec.reset.sysclk_hz = hsi_hz;
    }
    desc.hsi_label = hsi_label;
    // `rcc/l.rs` and `rcc/u5.rs` default to `hsi: false`: a block that selects
    // HSI must turn it on, or `hsi.unwrap()` panics at boot. (H5/H7's `hsi`
    // is an `Option<HSIPrescaler>`, on by default; F0/F3's is on.)
    desc.hsi_needs_enable = matches!(
        family,
        "stm32l0"
            | "stm32l1"
            | "stm32l4"
            | "stm32l4+"
            | "stm32l5"
            | "stm32wb"
            | "stm32wl"
            | "stm32u0"
            | "stm32u5"
    );
    // F0 and U0 have one APB: embassy's `Config` has no `apb2_pre` there, so a
    // tree with an `apb2` node (the minimal one has) named a missing field.
    if matches!(family, "stm32f0" | "stm32u0") {
        spec.apb = APB_1;
        spec.reset.apb = spec.apb.iter().map(|(field, _)| (*field, 1)).collect();
    }
    Some((spec, desc))
}

/// Why [`generic_refusal`] refused - each a project that did not build or
/// panicked in `embassy_stm32::init`, said in the skeleton and the Clock tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GenericRefusal {
    Pll,
    HsePrescaler,
    UsbPhy,
    AdcAhb,
}

impl GenericRefusal {
    /// What the tree selects and why it needs the family's recipe, as
    /// comment-width lines.
    fn lines(self) -> &'static [&'static str] {
        match self {
            GenericRefusal::Pll => {
                &["the PLL - its fields and limits differ from family to family."]
            }
            GenericRefusal::HsePrescaler => {
                &["the HSE - on this family its `Hse` takes a prescaler as well."]
            }
            GenericRefusal::UsbPhy => &[
                "the HSE - on a part with USB OTG_HS, embassy also derives the USB PHY",
                "clock from it and panics at boot unless it is 16, 19.2, 20, 24, 26 or",
                "32 MHz; the generic block cannot tell whether this part has one.",
            ],
            GenericRefusal::AdcAhb => &[
                "an AHB prescaler - on every F3 but the F37x the ADC runs from HCLK/1 by",
                "default, and embassy panics at boot unless AHB is /1 then.",
            ],
        }
    }
}

/// Why the generic block for `v` would not compile or boot on `family`, read
/// against embassy-stm32 0.6's `rcc` modules - every refusal here is a
/// project that failed to build or panicked in `embassy_stm32::init`.
///
/// Never the PLL. The generic `Pll { … }` is F4's or G4's, and of the
/// families it reaches only F0/F3 and L4+/L5/WB/WL/U0 even have that shape;
/// on those the tree's PLLMUL is a list the reader does not read (F0/F3: the
/// block said MUL2 whatever the tab showed, then failed `PLL_OUT` at boot) or
/// offers a PLLR the PAC lacks (WB/WL `/8`). That is each family's own
/// recipe, Phase 4; until then a PLL is the skeleton, which boots.
fn generic_refusal(family: &str, g: &ClockGraph, v: &RccValues) -> Option<GenericRefusal> {
    if v.sys == SysSource::Pll {
        return Some(GenericRefusal::Pll);
    }
    if v.sys == SysSource::Hse {
        match family {
            // `rcc/l.rs` gives the WB and WL `Hse` a `prescaler` the block does
            // not write.
            "stm32wb" | "stm32wl" => return Some(GenericRefusal::HsePrescaler),
            // `rcc/u5.rs` (U59x/U5Ax/U5Fx/U5Gx) and `rcc/h.rs` (an H7RS) with
            // OTG_HS take the USB PHY reference from `otghssel` / `usbphycsel`,
            // which no block writes (the sweep checks), so from their default,
            // the HSE itself - and panic unless it is one the PHY takes, USB
            // used or not. Nothing here knows the PART, so on a U5 every HSE
            // outside the list is refused: a U575 loses a block it could boot,
            // not a U5A5 one it could not. An H7 is refused only on a tree with
            // a USB PHY node (every H7RS tree): the same rule family-wide would
            // take the 8 and 25 MHz HSE from every H743.
            "stm32u5" if !USB_PHY_REF_HZ.contains(&v.hse_hz) => {
                return Some(GenericRefusal::UsbPhy);
            }
            "stm32h7"
                if g.nodes
                    .iter()
                    .any(|n| n.id.to_ascii_lowercase().contains("usbphy"))
                    && !USB_PHY_REF_HZ.contains(&v.hse_hz) =>
            {
                return Some(GenericRefusal::UsbPhy);
            }
            _ => {}
        }
    }
    // `rcc/f013.rs` on every F3 but the F37x: the ADC's default clock is
    // HCLK/1, and `init` asserts that AHB is then /1 too.
    if family == "stm32f3" && v.ahb != 1 {
        return Some(GenericRefusal::AdcAhb);
    }
    None
}

/// The HSE frequencies embassy can make an OTG_HS PHY reference from
/// (`Usbrefcksel` / `Usbphycsel` on U5 and H7RS).
const USB_PHY_REF_HZ: [u32; 6] = [
    16_000_000, 19_200_000, 20_000_000, 24_000_000, 26_000_000, 32_000_000,
];

/// HCLK and each APB bus's divider once the generated block has set the
/// clock - what `main.rs` runs, which on the generic recipe is embassy's HSI
/// and not the tree's. `None` when the block is the skeleton (embassy's
/// default, which nothing here models) or the clock is not an RCC block; the
/// F1 on Async writes embassy's block from its own reader.
fn block_buses(family: &str, clock: &ClockConfig) -> Option<(u32, Vec<(u8, u32)>)> {
    if family == "stm32f1" {
        let c = f1_embassy_clock(clock);
        if !f1_embassy_clock_ok(&c) {
            return None;
        }
        let hclk = f1_embassy_sysclk_hz(&c) / u32::from(c.ahb_pre).max(1);
        return Some((
            hclk,
            vec![(1, u32::from(c.apb1_pre)), (2, u32::from(c.apb2_pre))],
        ));
    }
    let ClockConfig::Graph(gc) = clock else {
        return None;
    };
    if !family.starts_with("stm32") || family == "stm32n6" {
        return None;
    }
    let g = gc.for_codegen();
    if !tree_sources_supported(&g) {
        return None;
    }
    let spec = rcc_recipe(family)
        .map(|(spec, _)| spec)
        .or_else(|| generic_recipe_for(family, &g).map(|(spec, _)| spec))?;
    let v = read_rcc_values(&g, &spec);
    let buses = v
        .apb
        .iter()
        .filter_map(|(field, d)| {
            let bus = field
                .strip_prefix("apb")?
                .strip_suffix("_pre")?
                .parse()
                .ok()?;
            Some((bus, (*d).max(1)))
        })
        .collect();
    Some((v.sysclk_hz / v.ahb.max(1), buses))
}

/// Each APB bus's clock as `main.rs` runs it - `(bus, Hz)`; empty where
/// [`block_buses`] knows nothing. What the Configuration tab's WWDG card
/// checks against: the tree can show another (an H7's minimal tree at 16 MHz
/// HSI, where embassy runs 64).
pub fn apb_bus_clocks(family: &str, clock: &ClockConfig) -> Vec<(u8, u32)> {
    block_buses(family, clock).map_or_else(Vec::new, |(hclk, buses)| {
        buses.into_iter().map(|(bus, d)| (bus, hclk / d)).collect()
    })
}

/// What a timer on each APB bus runs at - `(bus, Hz)`, by embassy's own rule
/// (`calc_pclk`: the bus clock, or twice it behind a divider).
pub fn apb_timer_clocks(family: &str, clock: &ClockConfig) -> Vec<(u8, u32)> {
    block_buses(family, clock).map_or_else(Vec::new, |(hclk, buses)| {
        buses
            .into_iter()
            .map(|(bus, d)| {
                let pclk = hclk / d;
                (bus, if d > 1 { pclk.saturating_mul(2) } else { pclk })
            })
            .collect()
    })
}

/// What `embassy_stm32::init` refuses at boot on the Async runtime, for the
/// Clock tab: the F1 block's own limits first (counted with the HSE the block
/// switches on, which the tab's warnings count as 0 Hz while it is off), then
/// [`time_driver_issue`].
pub fn async_boot_issue(family: &str, clock: &ClockConfig, timer: Option<u8>) -> Option<String> {
    if family == "stm32f1" && !f1_embassy_clock_ok(&f1_embassy_clock(clock)) {
        return Some(
            "embassy_stm32::init panics at boot on this clock (Async runtime): the PLL, a bus or the ADC is over the F1's limit, counted with the HSE the generated block switches on when the PLL needs it."
                .into(),
        );
    }
    time_driver_issue(family, clock, timer)
}

/// The boot check the Async runtime adds and the datasheet limits cannot see.
///
/// embassy-time ticks at 1 MHz (the IDE asks for no `tick-hz-*`), and
/// `embassy_stm32::init` starts its timer with a prescaler of
/// `clock / 1 MHz - 1`: under 1 MHz that is an overflow, and init panics. `timer` is the one
/// `time-driver-any` takes ([`embassy_stm32::time_driver_timer_of`]), `None`
/// when it is not known - then the slowest bus decides.
///
/// [`embassy_stm32::time_driver_timer_of`]: super::embassy_stm32::time_driver_timer_of
pub fn time_driver_issue(family: &str, clock: &ClockConfig, timer: Option<u8>) -> Option<String> {
    let buses = apb_timer_clocks(family, clock);
    // The timers STM32 puts on APB2; a single-APB part has only bus 1.
    let bus_of = |t: u8| {
        if matches!(t, 1 | 8 | 9 | 10 | 11 | 15 | 16 | 17 | 19 | 20 | 21 | 22) {
            2
        } else {
            1
        }
    };
    let (bus, hz) = match timer {
        Some(t) => buses
            .iter()
            .find(|(b, _)| *b == bus_of(t))
            .or_else(|| buses.first())
            .copied()?,
        None => buses.iter().min_by_key(|(_, hz)| *hz).copied()?,
    };
    if hz >= 1_000_000 {
        return None;
    }
    let who = timer.map_or_else(
        || "embassy-time's timer".to_owned(),
        |t| format!("TIM{t}, embassy-time's timer,"),
    );
    Some(format!(
        "{who} would run at {} kHz from APB{bus} - under the 1 MHz embassy-time ticks at, so embassy_stm32::init panics at boot (Async runtime). Divide less on AHB or APB{bus}.",
        hz / 1000
    ))
}

/// Whether the sources a tree selects are ones the RCC emitters can write:
/// HSI, HSE or the PLL for SYSCLK, and HSI or HSE behind the PLL. A tree that
/// selects anything else (an imported L4/WL/U5 tree at its MSI default) gets
/// the hand-written skeleton, which boots on embassy's own default clock -
/// not a block naming a source the tree did not pick.
pub fn tree_sources_supported(g: &ClockGraph) -> bool {
    match mux_source(g, "sw") {
        Some(MuxSource::Other) => false,
        Some(MuxSource::Pll) => !matches!(
            mux_source(g, "pllsrc"),
            Some(MuxSource::Other | MuxSource::Pll)
        ),
        _ => true,
    }
}

pub fn read_rcc_values(g: &ClockGraph, spec: &ReadSpec) -> RccValues {
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

    // By the source behind the selected input (see [`mux_source`]); on the
    // built-in trees that is the old index order exactly.
    let sys = match mux_source(g, "sw") {
        Some(MuxSource::Hsi) => SysSource::Hsi,
        Some(MuxSource::Hse) => SysSource::Hse,
        _ => SysSource::Pll,
    };
    // A missing node keeps the family default.
    let pll_src_hse = match mux_source(g, "pllsrc") {
        Some(MuxSource::Hse) => true,
        Some(MuxSource::Hsi) => false,
        _ => r.pll_src_hse,
    };

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
            // In u64: a tree at its extremes (64 MHz /1 x512) passes 4 GHz on
            // the way, which overflowed - a panic in a debug build of the IDE.
            ((src / pll_m.max(1)) as u64 * pll_n as u64 / pll_out.max(1) as u64)
                .min(u32::MAX as u64) as u32
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

/// Does the block start the HSE? Only when SYSCLK, directly or through the
/// PLL, runs on it; the tree's HSE tick alone starts nothing.
fn hse_used(v: &RccValues) -> bool {
    v.sys == SysSource::Hse || (v.sys == SysSource::Pll && v.pll_src_hse)
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
    if hse_used(v) {
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
    /// On every built-in tree the source behind each input is exactly what
    /// the old index order said, so no existing block can change.
    #[test]
    fn the_built_in_trees_read_their_sources_as_before() {
        use crate::panels::mcu_module::clock::graph::{
            minimal_graph, stm32f4::stm32f2_graph, stm32f4::stm32f4_graph, stm32g0::stm32g0_graph,
            stm32g4::stm32g4_graph, stm32l4::stm32l4_graph, stm32wba::stm32wba_graph,
        };
        for (name, g) in [
            ("minimal", minimal_graph()),
            ("f2", stm32f2_graph()),
            ("f4", stm32f4_graph()),
            ("g0", stm32g0_graph()),
            ("g4", stm32g4_graph()),
            ("l4", stm32l4_graph()),
            ("wba", stm32wba_graph()),
        ] {
            for (i, want) in [
                (0, MuxSource::Hsi),
                (1, MuxSource::Hse),
                (2, MuxSource::Pll),
            ] {
                let mut g = g.clone();
                g.node_mut("sw").unwrap().state = NodeState::Index(i);
                assert_eq!(mux_source(&g, "sw"), Some(want), "{name} sw {i}");
                for (j, src) in [(0, MuxSource::Hsi), (1, MuxSource::Hse)] {
                    if let Some(n) = g.node_mut("pllsrc") {
                        n.state = NodeState::Index(j);
                        assert_eq!(mux_source(&g, "pllsrc"), Some(src), "{name} pllsrc {j}");
                    }
                    assert!(tree_sources_supported(&g), "{name} {i}/{j}");
                }
            }
        }
    }

    /// A tree that lists MSI first, as ST's do: selecting MSI is not HSI -
    /// the block is the skeleton, which boots on embassy's MSI default; and
    /// selecting HSI on such a family turns HSI on.
    #[test]
    fn an_msi_first_tree_is_not_read_as_hsi() {
        use crate::panels::mcu_module::clock::graph::Edge;
        use crate::panels::mcu_module::clock::graph::minimal_graph;
        let mut g = minimal_graph();
        let mut msi = g.node("hsi").unwrap().clone();
        msi.id = "MSIRC".into();
        g.nodes.push(msi);
        g.edges.retain(|e| e.to != "sw");
        for (from, input) in [("MSIRC", 0), ("hsi", 1), ("hse", 2), ("pllclk", 3)] {
            g.edges.push(Edge {
                from: from.into(),
                to: "sw".into(),
                input,
            });
        }
        if let NodeKind::Mux { inputs } = &mut g.node_mut("sw").unwrap().kind {
            *inputs = 4;
        }
        let gc = |g: &ClockGraph| {
            ClockConfig::Graph(crate::panels::mcu_module::clock::graph::GraphClock {
                graph: g.clone(),
                layout: Default::default(),
                bindings: Default::default(),
            })
        };
        g.node_mut("sw").unwrap().state = NodeState::Index(0);
        assert_eq!(mux_source(&g, "sw"), Some(MuxSource::Other));
        assert!(!generates_clock_code_for("stm32wl", &gc(&g)));
        let block = graph_clock_block_for("stm32wl55jcix", "stm32wl", &gc(&g), false);
        // embassy's default clock - with the RNG off PLL1_Q, which it does
        // not run.
        assert!(
            block.contains("let p = embassy_stm32::init(config); // embassy's default clock (MSI)"),
            "{block}"
        );
        assert!(
            block.contains("        config.rcc.mux.rngsel = rcc::mux::Rngsel::MSI;"),
            "{block}"
        );
        assert!(!block.contains("Sysclk::HSI"), "{block}");

        g.node_mut("sw").unwrap().state = NodeState::Index(1);
        assert_eq!(mux_source(&g, "sw"), Some(MuxSource::Hsi));
        let block = graph_clock_block_for("stm32wl55jcix", "stm32wl", &gc(&g), false);
        assert!(block.contains("Sysclk::HSI"), "{block}");
        assert!(block.contains("config.rcc.hsi = true;"), "{block}");
    }

    /// A WBA55 tree no recipe writes (SYSCLK on the HSE behind its /2) gets
    /// the skeleton - and on a part with SAI1 its active code is NOT
    /// `init(Default::default())`, which panics in `init` there on the
    /// default `sai1sel` (PLL1_P). A WBA52 has no SAI1, but its RNG defaults
    /// to the LSE embassy never reports there: a `Config` on it too.
    #[test]
    fn a_wba_skeleton_boots_on_a_part_with_sai1() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let mut gc = parse_clock_ron(ron).expect("the shipped tree parses");
        gc.graph.node_mut("HseDiv").expect("HseDiv").state = NodeState::Index(1);
        let hse = gc
            .graph
            .edges
            .iter()
            .find(|e| e.to == "SysClkSource" && e.from == "HseDiv")
            .expect("SYSCLK can take the HSE")
            .input;
        gc.graph.node_mut("SysClkSource").expect("sw").state = NodeState::Index(hse);
        let clock = ClockConfig::Graph(gc);
        let active = |block: &str| -> Vec<String> {
            block
                .lines()
                .filter(|l| !l.trim_start().starts_with("//"))
                .map(str::to_owned)
                .collect()
        };

        let block = graph_clock_block_for("stm32wba55cgux", "stm32wba", &clock, false);
        assert!(
            block.contains("selects a clock source the generated recipes do"),
            "the skeleton path: {block}"
        );
        let code = active(&block);
        assert!(
            code.iter().all(|l| !l.contains("Default::default()")),
            "{block}"
        );
        assert!(
            code.iter()
                .any(|l| l == "        config.rcc.mux.sai1sel = rcc::mux::Sai1sel::HSI;"),
            "{block}"
        );
        assert!(
            code.iter()
                .any(|l| l.starts_with("    let p = embassy_stm32::init(config);")),
            "{block}"
        );
        // The example carries it too, so following it keeps the part booting.
        assert!(
            block.contains("    //       config.rcc.mux.sai1sel = rcc::mux::Sai1sel::HSI;"),
            "{block}"
        );

        // The WBA52 has no SAI1 - but an RNG whose default is the LSE.
        let block = graph_clock_block_for("stm32wba52cgux", "stm32wba", &clock, false);
        assert!(!block.contains("sai1sel"), "{block}");
        assert!(
            active(&block).iter().any(|l| l.ends_with("config.rcc.mux.rngsel = rcc::mux::Rngsel::HSI; // embassy's default, LSE, is not running")),
            "{block}"
        );
    }

    /// Every family's real CubeMX tree, at every SYSCLK and every PLL-source
    /// input. Checked against the HAL itself, not against what the reader
    /// meant: each `Sysclk::` / `PllSource::` the block names exists for that
    /// part (stm32-metapac's `Sw` / `Pllsrc`; embassy's own enum on F0/F1/F3);
    /// an HSI it selects on a family where embassy leaves HSI off is turned
    /// on; and a tree whose sources are unsupported gets the skeleton.
    /// Ignored: it needs STM32CubeMX and the cargo registry's metapac.
    ///
    /// `cargo test -- --ignored every_vendor_tree_names_a_source_it_turns_on --nocapture`
    ///
    /// It keeps, for the first and last single-core part of each family, every
    /// distinct block (and the skeleton's example, uncommented) and the HAL line
    /// the importer writes for that part. Two knobs use them:
    ///
    /// - `EIDE_CLOCK_DUMP=<dir>` writes them as `<feature>.rs`, `<feature>.dep`
    ///   and `targets.txt`;
    /// - `EIDE_CLOCK_CHECK=<dir>` `cargo check`s each part's blocks against
    ///   embassy-stm32 - one crate per feature under `<dir>\crates`, one shared
    ///   `<dir>\target` (a few GB: keep it off the repo and off the matrix
    ///   drive) - and fails on any that does not compile. The compiler is the
    ///   only oracle for the field names this sweep cannot see:
    ///   `config.rcc.pll` on an H7 (whose field is `pll1`), an `Hse` without
    ///   the `prescaler` the WB and WL require. `EIDE_CLOCK_LOCK=<Cargo.lock>`
    ///   seeds every crate with a known-good lock file (any generated STM32
    ///   project's), so 40-odd crates resolve once; `EIDE_CLOCK_ONLY` names the
    ///   features to check (comma-separated).
    #[test]
    #[ignore]
    fn every_vendor_tree_names_a_source_it_turns_on() {
        use crate::panels::mcu_module::clock::graph::cubemx;
        use crate::panels::mcu_module::stm32_pin_data::embassy_chip_feature;
        let Some(db) = cubemx::default_db_dir() else {
            println!("skipped: no STM32CubeMX database on this machine");
            return;
        };
        let Some(metapac) = crate::panels::mcu_module::stm32_harvest::metapac() else {
            println!("skipped: no stm32-metapac 21 in the cargo registry");
            return;
        };
        // The variants of `<name>` in a metapac register file: a `pub enum`, or
        // - where the values are sparse, as the F2's PLLN 192..=432 - a newtype
        // whose `impl <name>` holds `pub const MUL192: Self = …`.
        let variants = |text: &str, name: &str| -> Vec<String> {
            let ident = |v: &String| {
                v.starts_with(|c: char| c.is_ascii_uppercase())
                    && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            };
            let block = |head: &str| {
                text.find(head).map(|at| {
                    let body = &text[at..];
                    &body[..body.find("\n    }").unwrap_or(body.len())]
                })
            };
            if let Some(body) = block(&format!("pub enum {name} {{")) {
                return body
                    .lines()
                    .filter_map(|l| l.trim().split_once(" = ").map(|(v, _)| v.trim().to_owned()))
                    .filter(ident)
                    .collect();
            }
            if text.contains(&format!("pub struct {name}("))
                && let Some(body) = block(&format!("impl {name} {{"))
            {
                return body
                    .lines()
                    .filter_map(|l| {
                        let c = l.trim().strip_prefix("pub const ")?;
                        c.split_once(':').map(|(v, _)| v.trim().to_owned())
                    })
                    .filter(ident)
                    .collect();
            }
            Vec::new()
        };
        let named = |block: &str, ty: &str| -> Vec<String> {
            block
                .split(&format!("rcc::{ty}::"))
                .skip(1)
                .map(|r| {
                    r.chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect()
                })
                .collect()
        };
        let mut files: Vec<_> = std::fs::read_dir(db.join("mcu"))
            .expect("the CubeMX mcu folder")
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|e| e == "xml")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("STM32") && !n.starts_with("STM32MP"))
            })
            .collect();
        files.sort();
        let msi_family = |f: &str| {
            matches!(
                f,
                "stm32l0"
                    | "stm32l1"
                    | "stm32l4"
                    | "stm32l4+"
                    | "stm32l5"
                    | "stm32wb"
                    | "stm32wl"
                    | "stm32u0"
                    | "stm32u5"
            )
        };
        // One part per clock-tree file is enough: the tree is per family.
        let mut seen = std::collections::BTreeSet::new();
        // The option sweep once per (family, PAC rcc block, node, options):
        // most of a family's trees offer the same lists.
        let mut swept = std::collections::BTreeSet::new();
        let dump = std::env::var_os("EIDE_CLOCK_DUMP").map(std::path::PathBuf::from);
        // Absolute: each crate's cargo runs in its own folder, and would read a
        // relative CARGO_TARGET_DIR from there - a target per crate.
        let check = std::env::var_os("EIDE_CLOCK_CHECK")
            .map(|d| std::path::absolute(std::path::PathBuf::from(d)).expect("a folder path"));
        // family -> [(feature, target, part, the HAL line, blocks)] for the dump.
        type Dumped = (
            String,
            String,
            String,
            String,
            std::collections::BTreeSet<String>,
        );
        /// One part's blocks as a source file: each in a function of its own.
        fn block_file(
            family: &str,
            part: &str,
            blocks: &std::collections::BTreeSet<String>,
        ) -> String {
            let mut file = format!("// {family}: {part}\n");
            for (n, b) in blocks.iter().enumerate() {
                file.push_str(&format!("#[allow(unused)]\nfn case_{n}() {{\n{b}\n}}\n\n"));
            }
            file
        }
        /// `cargo check` of every kept part's blocks under `work` (see the doc
        /// above): a line per part, and what failed.
        fn cargo_check(
            work: &std::path::Path,
            dumped: &std::collections::BTreeMap<String, Vec<Dumped>>,
        ) -> Vec<String> {
            let only: Vec<String> = std::env::var("EIDE_CLOCK_ONLY")
                .unwrap_or_default()
                .split([',', ' '])
                .filter(|f| !f.is_empty())
                .map(str::to_owned)
                .collect();
            let lock = std::env::var_os("EIDE_CLOCK_LOCK").map(std::path::PathBuf::from);
            let (mut clean, mut failed) = (0usize, Vec::new());
            let mut checked = std::collections::BTreeSet::new();
            for (family, parts) in dumped {
                for (feature, target, part, hal_dep, blocks) in parts {
                    if !only.is_empty() && !only.contains(feature) {
                        continue;
                    }
                    checked.insert(feature.as_str());
                    // The importer's line, so `single-bank` stays where embassy's
                    // build script needs it, plus the time driver the project
                    // adds (the L1's ADC does not compile without embassy_time).
                    let hal = crate::panels::mcu_module::project_gen::toggle_hal_feature(
                        hal_dep.trim(),
                        "time-driver-any",
                        true,
                    );
                    let main = crate::panels::mcu_module::stm32_harvest::check_main(&block_file(
                        family, part, blocks,
                    ));
                    let result = crate::panels::mcu_module::stm32_harvest::cargo_check_crate(
                        work,
                        feature,
                        &hal,
                        target,
                        &main,
                        lock.as_deref(),
                    );
                    println!(
                        "{} {feature} ({} blocks)",
                        if result.is_ok() { "OK  " } else { "FAIL" },
                        blocks.len()
                    );
                    match result {
                        Ok(()) => clean += 1,
                        Err(errors) => {
                            for e in errors.iter().take(12) {
                                println!("     {e}");
                            }
                            failed.push(format!(
                                "{feature}: cargo check fails: {}",
                                errors.first().map_or("(no error line)", String::as_str)
                            ));
                        }
                    }
                }
            }
            println!("\n{clean}/{} features check clean", clean + failed.len());
            // A name that matched nothing compiled nothing: never a pass.
            for f in only.iter().filter(|f| !checked.contains(f.as_str())) {
                failed.push(format!(
                    "{f}: EIDE_CLOCK_ONLY names no part this sweep keeps (the first and last single-core part of each family, lower case)"
                ));
            }
            if checked.is_empty() && only.is_empty() {
                failed
                    .push("EIDE_CLOCK_CHECK: no part was kept, so nothing was checked".to_owned());
            }
            failed
        }
        let mut dumped: std::collections::BTreeMap<String, Vec<Dumped>> = Default::default();
        let (mut blocks, mut skeletons, mut options) = (0usize, 0usize, 0usize);
        let mut wrong = Vec::new();
        for path in &files {
            let xml = std::fs::read_to_string(path).expect("readable");
            let Ok(chips) = crate::panels::mcu_module::stm32_pin_data::convert_xml(&xml) else {
                continue;
            };
            let Some(chip) = chips.first() else { continue };
            let family = chip.form.family.clone();
            if family == "stm32f1" || family == "stm32n6" {
                continue;
            }
            // A part the importer refuses never reaches the generator.
            if crate::panels::mcu_module::stm32_support::refusal(
                &family,
                &chip.form.display_name,
                &chip.cores,
            )
            .is_some()
            {
                continue;
            }
            let Ok(key) = cubemx::clock_key_from_mcu_xml(&xml) else {
                continue;
            };
            if !seen.insert(format!("{family} {key:?}")) {
                continue;
            }
            let Ok((gc, _)) = cubemx::graph_for_chip_xml(&db, &xml, &family) else {
                continue;
            };
            let feature = embassy_chip_feature(&chip.form.display_name);
            let pac = std::fs::read_to_string(
                metapac
                    .join("src")
                    .join("chips")
                    .join(&feature)
                    .join("pac.rs"),
            )
            .unwrap_or_default();
            let Some(rcc_file) = pac
                .split("#[path = \"../../peripherals/rcc_")
                .nth(1)
                .and_then(|r| r.split('"').next())
            else {
                continue; // a part embassy has no chip for
            };
            let regs = std::fs::read_to_string(
                metapac
                    .join("src")
                    .join("peripherals")
                    .join(format!("rcc_{rcc_file}")),
            )
            .unwrap_or_default();
            let sw_ok = variants(&regs, "Sw");
            let src_ok: Vec<String> = if matches!(family.as_str(), "stm32f0" | "stm32f3") {
                ["HSE", "HSI", "HSI48"].map(String::from).to_vec()
            } else {
                variants(&regs, "Pllsrc")
            };
            let dump_here = (dump.is_some() || check.is_some()) && !feature.contains("-cm");
            let mut here = std::collections::BTreeSet::new();
            let mux_inputs = |id: &str| -> usize {
                match gc.for_codegen().node(id).map(|n| &n.kind) {
                    Some(NodeKind::Mux { inputs }) => *inputs,
                    _ => 0,
                }
            };
            let (sw_n, src_n) = (mux_inputs("sw"), mux_inputs("pllsrc").max(1));
            // A copy of the tree with SYSCLK on input `i` and the PLL on `j`.
            let select = |i: usize, j: usize| {
                let mut gc = gc.clone();
                for (canon, k) in [("sw", i), ("pllsrc", j)] {
                    let id = gc
                        .bindings
                        .get(canon)
                        .cloned()
                        .unwrap_or_else(|| canon.into());
                    if let Some(n) = gc.graph.node_mut(&id)
                        && matches!(n.kind, NodeKind::Mux { .. })
                    {
                        n.state = NodeState::Index(k);
                    }
                }
                gc
            };
            // Every `rcc::<Type>::<VARIANT>` a block can name for a prescaler
            // or a PLL factor, and the PAC enum embassy aliases it to.
            let pac_enums: Vec<(&str, Vec<String>)> = [
                ("AHBPrescaler", "Hpre"),
                ("APBPrescaler", "Ppre"),
                ("PllPreDiv", "Pllm"),
                ("PllMul", "Plln"),
                ("PllPDiv", "Pllp"),
                ("PllQDiv", "Pllq"),
                ("PllRDiv", "Pllr"),
                ("PllDiv", "Plldiv"),
            ]
            .into_iter()
            .map(|(ty, pac_name)| (ty, variants(&regs, pac_name)))
            .collect();
            let pac_wrong = |block: &str, at: &str, wrong: &mut Vec<String>| {
                for (ty, ok) in &pac_enums {
                    for v in named(block, ty) {
                        if !ok.contains(&v) {
                            wrong.push(format!("{at}: {ty}::{v} is not one of {ok:?}"));
                        }
                    }
                }
            };
            // (SYSCLK input, PLL source, it names the PLL) of each block written.
            let mut generated: Vec<(usize, usize, bool)> = Vec::new();
            for i in 0..sw_n {
                for j in 0..src_n {
                    let gc = select(i, j);
                    let supported = tree_sources_supported(&gc.for_codegen());
                    let block = graph_clock_block_for(
                        &chip.form.id,
                        &family,
                        &ClockConfig::Graph(gc),
                        false,
                    );
                    // The skeleton runs embassy's default clock - through a
                    // `Config` where its default `config.rcc.mux` needs a
                    // stand-in - and so does the reset shortcut.
                    let skeleton = block.contains("// embassy's default clock (")
                        || block.contains("embassy_stm32::init(Default::default())");
                    if dump_here {
                        // The skeleton's example is code too, once uncommented.
                        let code: String = block
                            .lines()
                            .map(|l| {
                                l.strip_prefix("    //   ")
                                    .map_or(l.to_owned(), |c| format!("    {c}"))
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        here.insert(code);
                    }
                    if skeleton {
                        skeletons += 1;
                    } else {
                        blocks += 1;
                        generated.push((i, j, block.contains("PllMul::")));
                    }
                    let at = format!(
                        "{family} ({}) SYSCLK input {i}, PLL source {j}",
                        chip.form.display_name
                    );
                    pac_wrong(&block, &at, &mut wrong);
                    if !supported && !skeleton {
                        wrong.push(format!("{at}: an unsupported source, and still a block"));
                    }
                    for v in named(&block, "Sysclk") {
                        if !sw_ok.contains(&v) {
                            wrong.push(format!("{at}: Sysclk::{v} is not one of {sw_ok:?}"));
                        }
                    }
                    for v in named(&block, "PllSource") {
                        if !src_ok.contains(&v) {
                            wrong.push(format!("{at}: PllSource::{v} is not one of {src_ok:?}"));
                        }
                    }
                    if msi_family(&family)
                        && block.contains("Sysclk::HSI")
                        && !block.contains("config.rcc.hsi = true;")
                    {
                        wrong.push(format!("{at}: HSI selected, never turned on"));
                    }
                    // `generic_refusal`'s USB PHY rule reads the HSE because no
                    // block moves these off it.
                    if block.contains("mux.otghssel") || block.contains("mux.usbphycsel") {
                        wrong.push(format!("{at}: a block writes the USB PHY selector"));
                    }
                }
            }
            // Then every option the tree offers, not only its default: the
            // prescalers on the first selection that is written, the PLL's
            // factors on the first that writes the PLL. (An imported WB/WL tree
            // offers a PLLR /8 its PAC's `Pllr` does not have.)
            let base = generated.first().copied();
            let with_pll = generated.iter().find(|g| g.2).copied();
            for (combo, nodes) in [
                (base, &["ahb", "apb1", "apb2", "apb7"][..]),
                (with_pll, &["pllm", "plln", "pllp", "pllq", "pllr"][..]),
            ] {
                let Some((i, j, _)) = combo else { continue };
                for canon in nodes {
                    let id = gc
                        .bindings
                        .get(*canon)
                        .cloned()
                        .unwrap_or_else(|| (*canon).to_owned());
                    let states: Vec<NodeState> = match gc.graph.node(&id).map(|n| &n.kind) {
                        Some(NodeKind::Divider { options }) => {
                            (0..options.len()).map(NodeState::Index).collect()
                        }
                        Some(NodeKind::Choice { ratios }) => {
                            (0..ratios.len()).map(NodeState::Index).collect()
                        }
                        Some(NodeKind::Multiplier { min, max }) => {
                            (*min..=*max).map(NodeState::Value).collect()
                        }
                        _ => continue,
                    };
                    let kind = gc.graph.node(&id).map(|n| format!("{:?}", n.kind));
                    if !swept.insert(format!("{family} {rcc_file} {canon} {kind:?} {i} {j}")) {
                        continue;
                    }
                    for state in states {
                        let mut gc = select(i, j);
                        let shown = format!("{state:?}");
                        if let Some(n) = gc.graph.node_mut(&id) {
                            n.state = state;
                        }
                        let block = graph_clock_block_for(
                            &chip.form.id,
                            &family,
                            &ClockConfig::Graph(gc),
                            false,
                        );
                        options += 1;
                        let at = format!(
                            "{family} ({}) SYSCLK input {i}, PLL source {j}, {canon} {shown}",
                            chip.form.display_name
                        );
                        pac_wrong(&block, &at, &mut wrong);
                    }
                }
            }
            if dump_here && !here.is_empty() {
                let entry = (
                    feature.clone(),
                    chip.form.target.clone(),
                    chip.form.display_name.clone(),
                    chip.form.hal_dep.clone(),
                    here,
                );
                let parts = dumped.entry(family.clone()).or_default();
                if parts.len() < 2 {
                    parts.push(entry);
                } else {
                    parts[1] = entry;
                }
            }
        }
        if let Some(dir) = &dump {
            std::fs::create_dir_all(dir).expect("the dump folder");
            let mut targets = String::new();
            for (family, parts) in &dumped {
                for (feature, target, part, hal_dep, blocks) in parts {
                    let file = block_file(family, part, blocks);
                    std::fs::write(dir.join(format!("{feature}.rs")), file).expect("writable");
                    // The line the importer writes: `single-bank`, the time driver.
                    std::fs::write(dir.join(format!("{feature}.dep")), hal_dep).expect("writable");
                    targets.push_str(&format!("{feature} {target} {}\n", blocks.len()));
                }
            }
            std::fs::write(dir.join("targets.txt"), targets).expect("writable");
        }
        // Before the asserts, so a block that names a wrong source is
        // compiled as well and both are reported.
        if let Some(work) = &check {
            wrong.extend(cargo_check(work, &dumped));
        }
        println!(
            "{} trees: {blocks} generated blocks, {skeletons} skeletons, {options} options",
            seen.len()
        );
        wrong.sort();
        wrong.dedup();
        assert!(
            wrong.is_empty(),
            "{} wrong:\n{}",
            wrong.len(),
            wrong.join("\n")
        );
        assert!(seen.len() > 15, "suspiciously few trees: {seen:?}");
        // And not vacuous: a gate that refused everything would pass the rest.
        // Measured 1636 trees, 7976 blocks and 3202 options (2026-10-21).
        assert!(
            blocks > 5000 && options > 2000,
            "suspiciously little generated: {blocks} blocks, {options} options"
        );
    }

    /// The source behind an input, through its dividers: a divided HSI on
    /// SYSCLK is not plain HSI, an HSE predivider bound as `pllm` is still the
    /// HSE, and every spelling of the 48 MHz RC is unwritten.
    #[test]
    fn a_source_is_read_through_its_dividers() {
        use crate::panels::mcu_module::clock::graph::{Edge, Node};
        let node = |id: &str, kind: NodeKind, state: NodeState| Node {
            id: id.into(),
            kind,
            state,
            limit: None,
        };
        let edge = |from: &str, to: &str, input: usize| Edge {
            from: from.into(),
            to: to.into(),
            input,
        };
        let src = NodeState::Source {
            enabled: true,
            hz: 64_000_000,
        };
        let source = || NodeKind::Source {
            min_hz: 0,
            max_hz: 0,
            gated: false,
        };
        // H5: HSIRC -> HSIDiv (/1 /2 /4 /8) -> SYSCLK input 1.
        let mut h5 = ClockGraph {
            nodes: vec![
                node("HSIRC", source(), src.clone()),
                node(
                    "hsi",
                    NodeKind::Divider {
                        options: vec![1, 2, 4, 8],
                    },
                    NodeState::Index(1),
                ),
                node("CSIRC", source(), src.clone()),
                node("sw", NodeKind::Mux { inputs: 2 }, NodeState::Index(1)),
            ],
            edges: vec![
                edge("HSIRC", "hsi", 0),
                edge("CSIRC", "sw", 0),
                edge("hsi", "sw", 1),
            ],
        };
        assert_eq!(mux_source(&h5, "sw"), Some(MuxSource::Other), "HSI /2");
        assert!(!tree_sources_supported(&h5));
        h5.node_mut("hsi").unwrap().state = NodeState::Index(0);
        assert_eq!(mux_source(&h5, "sw"), Some(MuxSource::Hsi), "HSI /1");
        h5.node_mut("sw").unwrap().state = NodeState::Index(0);
        assert_eq!(mux_source(&h5, "sw"), Some(MuxSource::Other), "CSI");

        // F0/F3: HSE -> PREDIV bound as `pllm` -> PLL source input 1.
        let f0 = ClockGraph {
            nodes: vec![
                node("HSE", source(), src.clone()),
                node(
                    "pllm",
                    NodeKind::Divider {
                        options: vec![1, 2, 3],
                    },
                    NodeState::Index(1),
                ),
                node("HSIRC", source(), src.clone()),
                node("pllsrc", NodeKind::Mux { inputs: 2 }, NodeState::Index(1)),
            ],
            edges: vec![
                edge("HSE", "pllm", 0),
                edge("HSIRC", "pllsrc", 0),
                edge("pllm", "pllsrc", 1),
            ],
        };
        assert_eq!(mux_source(&f0, "pllsrc"), Some(MuxSource::Hse));

        for id in ["HSI48", "HSIRC48", "HSIUSB48", "MSIRC", "CSI", "LSE"] {
            assert_eq!(classify_source(id), MuxSource::Other, "{id}");
        }
        assert_eq!(classify_source("HSIRC"), MuxSource::Hsi);
        assert_eq!(classify_source("pllxtpre"), MuxSource::Hse);
        assert!(
            !generic_allowed("stm32u3")
                && !generic_allowed("stm32c0")
                && generic_allowed("stm32h5")
        );
    }

    /// The skeleton's example compiles on the families it is written for: no
    /// `PLL1_P`, and the WB/WL prescaler; its comment names the real default.
    #[test]
    fn the_skeleton_names_the_familys_default_clock() {
        let wl = hand_written_skeleton("stm32wl", Skeleton::NoRecipe, true, &[]);
        assert!(!wl.contains("PLL1_P"), "{wl}");
        assert!(wl.contains("prescaler: rcc::HsePrescaler::DIV1"), "{wl}");
        assert!(wl.contains("(MSI)"), "{wl}");
        let u5 = hand_written_skeleton("stm32u5", Skeleton::NoRecipe, true, &[]);
        assert!(u5.contains("(MSIS)") && !u5.contains("prescaler"), "{u5}");
        assert!(hand_written_skeleton("stm32h7", Skeleton::NoRecipe, true, &[]).contains("(HSI)"));
        assert!(
            hand_written_skeleton("stm32h7", Skeleton::Source, true, &[])
                .contains("selects a clock source")
        );
        // Outside the markers it does not promise to keep an edit.
        for why in [
            Skeleton::NoRecipe,
            Skeleton::Source,
            Skeleton::Shape(GenericRefusal::Pll),
            Skeleton::Shape(GenericRefusal::UsbPhy),
        ] {
            let kept = hand_written_skeleton("stm32h7", why, true, &[]);
            let regenerated = hand_written_skeleton("stm32h7", why, false, &[]);
            assert!(kept.contains("kept across regeneration"), "{kept}");
            assert!(
                !regenerated.contains("kept across regeneration"),
                "{regenerated}"
            );
            assert!(
                regenerated.contains("Write the clock by hand"),
                "{regenerated}"
            );
            // Only the head differs: the example is the same code.
            let example = |s: &str| s.split_once("\n    //\n").map(|(_, rest)| rest.to_owned());
            assert_eq!(example(&kept), example(&regenerated));
            assert!(example(&kept).is_some(), "{kept}");
            // And no line repeats a word across its break ("To set it / it here").
            for s in [&kept, &regenerated] {
                let words: Vec<&str> = s.split_whitespace().filter(|w| *w != "//").collect();
                assert!(!words.windows(2).any(|w| w[0] == w[1]), "{s}");
            }
        }
        // Only the example is indented as code: nothing in the head is.
        for r in [
            GenericRefusal::Pll,
            GenericRefusal::HsePrescaler,
            GenericRefusal::UsbPhy,
            GenericRefusal::AdcAhb,
        ] {
            let s = hand_written_skeleton("stm32wl", Skeleton::Shape(r), false, &[]);
            let head = s.split_once("\n    //\n").map_or("", |(h, _)| h);
            assert!(!head.lines().any(|l| l.starts_with("    //   ")), "{s}");
        }
        // The example compiles on the family it is printed for, and does not
        // panic where the USB PHY rule refused: WBA's `Hse` is a prescaler.
        let wba = hand_written_skeleton("stm32wba", Skeleton::Source, true, &[]);
        assert!(
            wba.contains("Hse { prescaler: rcc::HsePrescaler::DIV1 }"),
            "{wba}"
        );
        assert!(!wba.contains("freq:"), "{wba}");
        assert!(wba.contains("VoltageScale::RANGE1"), "{wba}");
        let wb = hand_written_skeleton("stm32wb", Skeleton::NoRecipe, true, &[]);
        assert!(
            wb.contains("Hertz(32_000_000)") && wb.contains("HsePrescaler::DIV1"),
            "{wb}"
        );
        let n6 = hand_written_skeleton("stm32n6", Skeleton::NoRecipe, true, &[]);
        assert!(
            n6.contains("rcc::SysClk::Hse") && !n6.contains("Sysclk::HSE"),
            "{n6}"
        );
        assert!(
            hand_written_skeleton("stm32f4", Skeleton::Source, true, &[])
                .contains("Hertz(8_000_000)")
        );
        let u5 = hand_written_skeleton("stm32u5", Skeleton::NoRecipe, true, &[]);
        assert!(u5.contains("Hertz(16_000_000)"), "{u5}");
        let rs = hand_written_skeleton(
            "stm32h7",
            Skeleton::Shape(GenericRefusal::UsbPhy),
            false,
            &[],
        );
        assert!(rs.contains("Hertz(16_000_000)"), "{rs}");
        assert!(
            hand_written_skeleton("stm32h7", Skeleton::NoRecipe, true, &[])
                .contains("Hertz(16_000_000)")
        );
        // The reason is the refusal's, word for word.
        let rs = hand_written_skeleton(
            "stm32h7",
            Skeleton::Shape(GenericRefusal::UsbPhy),
            true,
            &[],
        );
        assert!(
            rs.contains("USB OTG_HS") && rs.contains("26 or\n    // 32 MHz"),
            "{rs}"
        );
        // The hand-written projects that exist keep their text byte for byte.
        assert!(
            hand_written_skeleton("stm32h7", Skeleton::NoRecipe, true, &[]).starts_with(concat!(
                "    // Clock: this chip's family has no generated RCC recipe yet, so the\n",
                "    // Clock tab's tree cannot be turned into code automatically. Set it\n",
                "    // here — this block is kept across regeneration.\n    //\n"
            ))
        );
    }

    /// A dual-core skeleton: the static once, OUTSIDE the manual markers,
    /// and no `init` left anywhere, the commented example included.
    #[test]
    fn a_dual_core_block_calls_init_primary() {
        use super::super::common::clock_region;
        let block = graph_clock_block_for("stm32wl55jcix", "stm32wl", &ClockConfig::None, true);
        let out = for_dual_core(&block, "stm32wl55jc-cm4");
        assert_eq!(
            out.matches("static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData>")
                .count(),
            1,
            "{out}"
        );
        let region = clock_region(&out).expect("the manual markers");
        assert!(!region.contains("SHARED_DATA:"), "{out}");
        assert!(
            out.contains(
                "let p = embassy_stm32::init_primary(config, &SHARED_DATA); // embassy's default clock (MSI)"
            ),
            "{out}"
        );
        assert!(
            out.contains("//   let p = embassy_stm32::init_primary(config, &SHARED_DATA);"),
            "{out}"
        );
        assert!(!out.contains("embassy_stm32::init("), "{out}");
        assert!(out.contains("`stm32wl55jc-cm4`"), "{out}");
    }

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
            s.contains("//       config.rcc.sys = rcc::Sysclk::HSE;"),
            "the shape to uncomment is there, in a form every family compiles"
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
        // HSE 8 MHz straight to SYSCLK.
        graph.node_mut("sw").unwrap().state = NodeState::Index(1);
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
        assert!(s.contains("SYSCLK 8 MHz"), "{s}");
        assert!(s.contains("config.rcc.sys = rcc::Sysclk::HSE;"), "{s}");
        assert!(s.contains("Hertz(8000000)"), "{s}");
        // Honest about the one thing it guessed.
        assert!(s.contains("no verified RCC recipe for this family"));
        // Which in turn flips the hand-written default off, so it UPDATES.
        assert!(generates_clock_code_for("stm32h5", &clock));
        assert!(!generates_clock_code_for("stm32h5", &ClockConfig::None));
    }

    /// The generic block is one that compiles AND boots on the family, read
    /// against embassy-stm32 0.6 (`generic_writes`): never a PLL, no HSE on
    /// WB/WL (`Hse` takes a `prescaler`) or U5 (the OTG_HS reference panics),
    /// no AHB prescaler on F3 (the ADC clock assert); HSI switched on where
    /// embassy leaves it off, and no `apb2_pre` where `Config` has none.
    #[test]
    fn the_generic_block_compiles_and_boots_on_the_family() {
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, minimal_graph, model::NodeState, stm32g4_graph,
        };
        let clock = |graph| {
            ClockConfig::Graph(GraphClock {
                graph,
                layout: Default::default(),
                bindings: Default::default(),
            })
        };
        let refused = |family: &str, c: &ClockConfig| {
            let s = graph_clock_block(family, c, false);
            assert!(
                s.contains("only that recipe can write safely"),
                "{family}:\n{s}"
            );
            // embassy's default clock, with whatever stand-ins its default
            // `config.rcc.mux` needs on the family (the U5's DAC1SEL).
            assert!(s.contains("embassy's default clock ("), "{s}");
            assert!(!generates_clock_code_for(family, c), "{family}");
            // The Clock tab says which selection, not "no generator".
            assert!(
                skeleton_reason(family, c).is_some_and(|r| r.contains("own recipe")),
                "{family}"
            );
        };
        let sw = |n: usize| {
            let mut g = minimal_graph();
            g.node_mut("sw").unwrap().state = NodeState::Index(n);
            g
        };

        // A PLL, on every family the generic recipe reaches - L5 with the very
        // G4 shape its `rcc/l.rs` has included.
        let mut pll = sw(2);
        pll.node_mut("pllsrc").unwrap().state = NodeState::Index(1);
        pll.node_mut("plln").unwrap().state = NodeState::Value(60);
        let pll = clock(pll);
        for family in [
            "stm32f0", "stm32f3", "stm32h5", "stm32h7", "stm32l0", "stm32l1", "stm32l5", "stm32u0",
            "stm32u5", "stm32wb", "stm32wl",
        ] {
            refused(family, &pll);
        }
        refused("stm32l5", &clock(stm32g4_graph()));

        let hse = clock(sw(1));
        for family in ["stm32wb", "stm32wl"] {
            refused(family, &hse);
        }
        // An H7 tree with no USB PHY node is not an H7RS: its HSE is written.
        for family in ["stm32h7", "stm32f0", "stm32l1"] {
            assert!(generates_clock_code_for(family, &hse), "{family}");
            assert_eq!(skeleton_reason(family, &hse), None);
        }
        assert_eq!(skeleton_reason("stm32wba", &ClockConfig::None), None);

        // HSI: turned on by the family, not by an MSI node the tree may lack.
        let hsi = clock(sw(0));
        for family in ["stm32l5", "stm32wb", "stm32u0", "stm32u5", "stm32l1"] {
            let s = graph_clock_block(family, &hsi, false);
            assert!(s.contains("config.rcc.hsi = true;"), "{family}:\n{s}");
        }
        for family in ["stm32h7", "stm32f3"] {
            let s = graph_clock_block(family, &hsi, false);
            assert!(s.contains("rcc::Sysclk::HSI"), "{s}");
            assert!(!s.contains("config.rcc.hsi ="), "{family}:\n{s}");
        }

        // One APB on F0/U0.
        for family in ["stm32f0", "stm32u0"] {
            let s = graph_clock_block(family, &hsi, false);
            assert!(s.contains("config.rcc.apb1_pre"), "{s}");
            assert!(!s.contains("apb2_pre"), "{family}:\n{s}");
        }
        assert!(graph_clock_block("stm32f3", &hsi, false).contains("apb2_pre"));

        // AHB /2: the F3's ADC assert refuses it, the F0 has no such check.
        let mut ahb = sw(0);
        ahb.node_mut("ahb").unwrap().state = NodeState::Index(1);
        let ahb = clock(ahb);
        refused("stm32f3", &ahb);
        let s = graph_clock_block("stm32f0", &ahb, false);
        assert!(s.contains("AHBPrescaler::DIV2"), "{s}");

        // An H7RS or U5 with OTG_HS takes its USB PHY reference from the HSE.
        let mut rs = sw(1);
        rs.nodes
            .push(crate::panels::mcu_module::clock::graph::Node {
                id: "USBPHYCLKMux".into(),
                kind: crate::panels::mcu_module::clock::graph::NodeKind::Mux { inputs: 3 },
                state: NodeState::Index(0),
                limit: None,
            });
        refused("stm32h7", &clock(rs.clone()));
        refused("stm32u5", &clock(rs.clone()));
        // A U5 is refused whatever its tree: the part decides, and the generic
        // block does not know it.
        refused("stm32u5", &hse);
        let NodeState::Source { enabled, .. } = rs.node("hse").unwrap().state else {
            panic!("the HSE is a source")
        };
        rs.node_mut("hse").unwrap().state = NodeState::Source {
            enabled,
            hz: 24_000_000,
        };
        assert!(generates_clock_code_for("stm32h7", &clock(rs.clone())));
        assert!(generates_clock_code_for("stm32u5", &clock(rs)));

        // embassy's HSI, not the tree's: the minimal tree's 16 MHz is 8 on an
        // F0 and 64 on an H7 - what the header says and the boot check counts.
        let s = graph_clock_block("stm32f0", &hsi, false);
        assert!(s.contains("SYSCLK 8 MHz (HSI8)"), "{s}");
        let s = graph_clock_block("stm32h7", &hsi, false);
        assert!(s.contains("SYSCLK 64 MHz (HSI64)"), "{s}");
        assert!(graph_clock_block("stm32l1", &hsi, false).contains("SYSCLK 16 MHz (HSI16)"));

        // U3 and C0 read no tree: the skeleton says so whatever is selected,
        // and the tab does not blame the source.
        for family in ["stm32u3", "stm32c0"] {
            let s = graph_clock_block(family, &hsi, false);
            assert!(s.contains("has no generated RCC recipe yet, so the"), "{s}");
            assert_eq!(skeleton_reason(family, &hsi), None);
        }

        // Whether THIS selection generates is one question; whether the tree
        // can drive the code at all is another: an import at a selection the
        // generic block does not write (its MSI default, a PLL) is not
        // "written by hand".
        let unwritten = clock(sw(2));
        assert!(!generates_clock_code_for("stm32u5", &unwritten));
        assert!(tree_drives_clock_code("stm32u5", &unwritten));
        assert!(tree_drives_clock_code("stm32l4", &unwritten));
        assert!(!tree_drives_clock_code("stm32u3", &unwritten));
        assert!(!tree_drives_clock_code("stm32h7", &ClockConfig::None));
    }

    /// The Async runtime's own boot check: embassy-time's timer at 1 MHz or
    /// more, on the bus that timer is on - for a family's recipe as much as
    /// for the generic one.
    #[test]
    fn a_timer_under_a_megahertz_is_a_boot_panic_on_async() {
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, minimal_graph, model::NodeState, stm32f4_graph,
        };
        let clock = |graph| {
            ClockConfig::Graph(GraphClock {
                graph,
                layout: Default::default(),
                bindings: Default::default(),
            })
        };
        // 8 MHz HSI straight to SYSCLK, AHB /16: everything at 500 kHz on an F0.
        let mut slow = minimal_graph();
        slow.node_mut("hsi").unwrap().state = NodeState::Source {
            enabled: true,
            hz: 8_000_000,
        };
        slow.node_mut("ahb").unwrap().state = NodeState::Index(4);
        let slow = clock(slow);
        assert_eq!(apb_timer_clocks("stm32f0", &slow), vec![(1, 500_000)]);
        assert_eq!(apb_bus_clocks("stm32f0", &slow), vec![(1, 500_000)]);
        // The bus clocks are what main.rs runs: an H7's HSI is embassy's 64 MHz
        // whatever the minimal tree says, and a timer behind /2 runs at twice
        // its bus.
        let mut h7 = minimal_graph();
        h7.node_mut("apb1").unwrap().state = NodeState::Index(1);
        let h7 = clock(h7);
        assert_eq!(
            apb_bus_clocks("stm32h7", &h7),
            vec![(1, 32_000_000), (2, 64_000_000)]
        );
        assert_eq!(
            apb_timer_clocks("stm32h7", &h7),
            vec![(1, 64_000_000), (2, 64_000_000)]
        );
        // The F1 on Async: the WWDG card's bus too (its only runtime with one),
        // from the block's own reader - the shipped 72 MHz tree, APB1 /2.
        let f1 = clock(crate::panels::mcu_module::clock::graph::stm32f1_graph(
            &Default::default(),
        ));
        assert_eq!(
            apb_bus_clocks("stm32f1", &f1),
            vec![(1, 36_000_000), (2, 72_000_000)]
        );
        let why = time_driver_issue("stm32f0", &slow, Some(15)).expect("a panic at boot");
        assert!(
            why.contains("TIM15") && why.contains("500 kHz") && why.contains("APB1"),
            "{why}"
        );
        // The shipped F4 tree is well above it, on either bus.
        let f4 = clock(stm32f4_graph());
        assert_eq!(time_driver_issue("stm32f4", &f4, Some(9)), None);
        assert_eq!(time_driver_issue("stm32f4", &f4, None), None);
        // ...until its AHB is divided by 512.
        let mut g = stm32f4_graph();
        let n = g.node_mut("ahb").unwrap();
        let crate::panels::mcu_module::clock::graph::NodeKind::Divider { options } = &n.kind else {
            panic!("AHB is a divider")
        };
        n.state = NodeState::Index(options.iter().position(|d| *d == 512).expect("/512"));
        assert!(time_driver_issue("stm32f4", &clock(g), Some(9)).is_some());
        // The skeleton runs embassy's default: nothing to accuse.
        let mut pll = minimal_graph();
        pll.node_mut("sw").unwrap().state = NodeState::Index(2);
        pll.node_mut("ahb").unwrap().state = NodeState::Index(8);
        assert!(apb_timer_clocks("stm32h7", &clock(pll)).is_empty());
        assert_eq!(time_driver_issue("stm32h7", &ClockConfig::None, None), None);

        // The F1's own refusal: x16 on the 8 MHz crystal is a 128 MHz PLL,
        // counted with the HSE on even where the tree left it off.
        use crate::panels::mcu_module::clock::model::Stm32f1Clock;
        let f1 =
            |c: Stm32f1Clock| clock(crate::panels::mcu_module::clock::graph::stm32f1_graph(&c));
        assert_eq!(
            async_boot_issue("stm32f1", &f1(Stm32f1Clock::default()), Some(4)),
            None
        );
        let fast = Stm32f1Clock {
            hse_enabled: false,
            pll_mul: 16,
            ..Stm32f1Clock::default()
        };
        let why = async_boot_issue("stm32f1", &f1(fast), Some(4)).expect("a panic at boot");
        assert!(why.contains("over the F1's limit"), "{why}");
        // Everywhere else it is the time-driver check.
        assert_eq!(
            async_boot_issue("stm32f0", &slow, Some(15)),
            time_driver_issue("stm32f0", &slow, Some(15))
        );
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
    /// its whole clock tree still generated no clock code. The generic path
    /// writes no PLL now (`generic_writes`); the shape stays, emitted from its
    /// descriptor, for the F0/F3 recipe.
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
        let ClockConfig::Graph(gc) = &clock else {
            unreachable!()
        };
        let block = emit_rcc_block(&desc, &read_rcc_values(&gc.graph, &spec));
        assert!(block.contains("src: rcc::PllSource::HSE,"), "{block}");
        assert!(block.contains("mul: rcc::PllMul::MUL9,"), "{block}");
        assert!(
            !block.contains("divp") && !block.contains("divq") && !block.contains("divr"),
            "those fields do not exist on this family:\n{block}"
        );
        // 8 MHz HSE /1 x9 = 72 MHz, the classic F1/F3 maximum.
        assert!(block.contains("SYSCLK 72 MHz"), "{block}");
        // Through the generic recipe it is the skeleton: an imported tree's
        // PLLMUL is a list its reader does not read.
        assert!(!generates_clock_code_for("stm32f3", &clock));

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
            "let p = embassy_stm32::init(Default::default()); // embassy's default clock (HSI)",
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
        let mut graph = stm32f4_graph();
        // HSI straight to SYSCLK: the H7's `pll1` is not the F4's `pll`.
        graph.node_mut("sw").unwrap().state =
            crate::panels::mcu_module::clock::graph::NodeState::Index(0);
        let g = GraphClock {
            graph,
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
        // Its PLL is the skeleton: the F4 shape names fields the H7 lacks.
        let g = GraphClock {
            graph: stm32f4_graph(),
            layout: Default::default(),
            bindings: Default::default(),
        };
        let s = graph_clock_block("stm32h7", &ClockConfig::Graph(g), false);
        assert!(s.contains("only that recipe can write safely"), "{s}");

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
