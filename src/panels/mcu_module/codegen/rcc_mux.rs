//! Per-peripheral kernel clock selection — the Clock tab's mux nodes, emitted.
//!
//! A modern STM32 gives most peripherals their own clock selector: USART1SEL,
//! I2C1SEL, ADCSEL, LPTIM1SEL. The Clock tab has ALWAYS shown these — a CubeMX
//! import of a WBA55 carries 16 of them, an H5 thirty — and the user could pick
//! a source and watch the frequency change. None of it reached `main.rs`:
//! [`codegen_node_ids`](super::rcc::codegen_node_ids) listed only bus and PLL
//! nodes, so nothing bound them and nothing read them. A diagram that computed
//! an answer nobody asked the compiler.
//!
//! # Nothing here is guessed
//!
//! Emitting one line needs three names:
//!
//! ```ignore
//! config.rcc.mux.usart1sel = mux::Usart1sel::HSI;
//! //             ^field           ^enum      ^variant
//! ```
//!
//! and the enum is NOT the field capitalised: on WBA, `usart2sel` and
//! `usart3sel` both use `Usartsel`, while `i2c2sel` and `i2c4sel` both use
//! `I2c1sel`. So the table is harvested from `stm32-metapac`'s register IR —
//! the same data embassy's own build script reads — by an ignored test,
//! `rcc_mux_harvest`, into [`rcc_mux_data`](super::rcc_mux_data).
//!
//! # Proposed by name, CONFIRMED by sources
//!
//! Matching a node to a selector by name alone would be a guess of the kind
//! this module exists to avoid, so a proposal only survives if the tree agrees
//! with the register: input *i* of the mux must be fed by the clock metapac
//! names for register value *i*. `USART1Mult` is fed by `APB2Prescaler`,
//! `SysCLKOutput`, `HSIRC`, `LSEOSC`; `Usart1sel` reads `PCLK2`, `SYS`, `HSI`,
//! `LSE`. Four for four, in order, so that binding is real.
//!
//! When they disagree — a vendor tree that orders its inputs differently, a
//! selector the tree does not model — the node is DROPPED. A missing line
//! leaves the peripheral on embassy's default - register value 0, since
//! `ClockMux` starts zeroed and `init` writes every field - which is what
//! happened before this module existed. A wrong line silently runs a
//! peripheral off the wrong clock.

use super::super::clock::graph::model::{ClockGraph, NodeKind, NodeState};

/// One selector embassy exposes on `config.rcc.mux`.
pub struct MuxField {
    /// The `ClockMux` field, e.g. `usart1sel`.
    pub field: &'static str,
    /// The PAC enum it takes, e.g. `Usart1sel` — not derivable from `field`.
    pub enum_name: &'static str,
    /// `(register value, variant name)`. The value is carried because it is not
    /// always 0-based: `rtcsel`'s first variant is 1, since 0 means "no clock".
    pub variants: &'static [(u32, &'static str)],
}

/// Which RCC register version a part number uses.
///
/// By PREFIX, longest match first: metapac's chip names (`stm32f410t8`) are a
/// prefix of the part numbers the IDE carries (`stm32f410t8yx`), and the extra
/// letters are the package, which no register cares about.
pub fn rcc_version(chip: &str) -> Option<&'static str> {
    let chip = chip.to_ascii_lowercase();
    super::rcc_mux_data::CHIP_RCC
        .iter()
        .filter(|(prefix, _)| chip.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, version)| *version)
}

/// The selectors a chip can emit.
///
/// A family is NOT one register block, which is why this takes the part number:
/// STM32H7 has four RCC versions, F0 and F3 four each, and even F4 has two —
/// 143 parts on `f4` and six F410s on `f410`. Emitting a family's usual table
/// for the odd part out would name a field that chip does not have.
///
/// `family` is the fallback for a part metapac has never heard of, and only
/// answers for families whose chips all share one version. An unknown chip in
/// an ambiguous family gets nothing, which costs a missing line rather than a
/// wrong one.
///
/// A version's table is every one of its chips' selectors, so the ones THIS
/// part lacks are taken out ([`lacks`]): its CubeMX tree is the whole line's
/// and may carry a mux for a peripheral the part does not have.
pub fn fields_for_chip(chip: &str, family: &str) -> Vec<&'static MuxField> {
    let version = rcc_version(chip).or_else(|| {
        super::rcc_mux_data::FAMILY_RCC
            .iter()
            .find(|(f, _)| *f == family)
            .map(|(_, v)| *v)
    });
    let Some(version) = version else {
        return Vec::new();
    };
    let lacks = lacks(chip);
    super::rcc_mux_data::VERSIONS
        .iter()
        .find(|(v, _)| *v == version)
        .map(|(_, fields)| *fields)
        .unwrap_or(&[])
        .iter()
        .filter(|f| !lacks.contains(&f.field))
        .collect()
}

/// The selectors of its RCC version a part does not have - an STM32L010's
/// `usart1sel`: no USART1, so no field in embassy's `config.rcc.mux`. By
/// prefix, longest match first, as [`rcc_version`].
pub fn lacks(chip: &str) -> &'static [&'static str] {
    let chip = chip.to_ascii_lowercase();
    super::rcc_mux_data::CHIP_LACKS
        .iter()
        .filter(|(prefix, _)| chip.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, fields)| *fields)
        .unwrap_or(&[])
}

/// The selectors a family can emit with no part number to go on.
pub fn fields_for(family: &str) -> Vec<&'static MuxField> {
    fields_for_chip("", family)
}

/// One confirmed pairing of a graph node with a selector.
pub struct Bound {
    pub node: String,
    pub field: &'static MuxField,
}

/// Strip the decoration a vendor puts around a selector's name.
///
/// `USART1Mult` / `USART1CLockSelection` / `usart1sel` all name the same thing;
/// this reduces them to `usart1`.
fn core_name(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    for suffix in [
        "clocksource",
        "clockselection",
        "clksource",
        "selection",
        "source",
        "mult",
        "mux",
        "sel",
    ] {
        if let Some(rest) = s.strip_suffix(suffix)
            && !rest.is_empty()
        {
            s = rest.to_owned();
            break;
        }
    }
    s
}

/// Reduce a clock's name to a token both vocabularies agree on.
///
/// The tree and the register describe the same silicon in different words:
/// `APB7Output` / `PCLK7`, `AHBOutput` / `HCLK4`, `LSIOut` / `LSI`,
/// `PLL1P` / `PLL1_P`. Strip the decoration each side adds, then rename the two
/// bus families onto one spelling.
fn clock_token(raw: &str) -> String {
    let mut s: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    for suffix in ["output", "out", "osc", "rc", "prescaler"] {
        if let Some(rest) = s.strip_suffix(suffix)
            && !rest.is_empty()
        {
            s = rest.to_owned();
            break;
        }
    }
    // The register says PCLK/HCLK where the tree says APB/AHB. The bus NUMBER
    // matters on PCLK — apb1 is not apb7 — and does not on HCLK: `HCLK4` names
    // the AHB4 domain, which these trees draw as a single AHB node.
    if let Some(n) = s.strip_prefix("pclk") {
        return format!("apb{n}");
    }
    if s.starts_with("hclk") || s.starts_with("ahb") {
        return "ahb".into();
    }
    if s == "sys" || s == "system" || s == "sysclk" {
        return "sysclk".into();
    }
    s
}

/// Does this graph node name the clock metapac calls `variant`?
fn same_clock(node_id: &str, variant: &str) -> bool {
    let n = clock_token(node_id);
    !n.is_empty() && n == clock_token(variant)
}

/// Pair this graph's mux nodes with the family's selectors.
///
/// Only pairings the tree CONFIRMS are returned — see the module docs.
pub fn propose(graph: &ClockGraph, chip: &str, family: &str) -> Vec<Bound> {
    let mut out = Vec::new();
    for field in fields_for_chip(chip, family) {
        let target = core_name(field.field);
        let Some(node) = graph
            .nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Mux { .. }) && core_name(&n.id) == target)
        else {
            continue;
        };
        if confirms(graph, &node.id, field) {
            out.push(Bound {
                node: node.id.clone(),
                field,
            });
        }
    }
    out
}

/// Do the tree's inputs and the register's variants describe the same clocks?
///
/// Order is deliberately NOT checked, because it does not hold — see
/// [`variant_of`]. What is checked is that the two are talking about the same
/// selector at all: most of the tree's inputs must name a variant this field
/// really has. A selector whose sources we cannot recognise is one we do not
/// understand well enough to write a line for.
fn confirms(graph: &ClockGraph, node_id: &str, field: &MuxField) -> bool {
    let sources: Vec<&str> = graph
        .edges
        .iter()
        .filter(|e| e.to == node_id)
        .map(|e| e.from.as_str())
        .collect();
    if sources.len() < 2 {
        return false;
    }
    let known = sources
        .iter()
        .filter(|s| field.variants.iter().any(|(_, v)| same_clock(s, v)))
        .count();
    // Two thirds, and never fewer than two. A tree may legitimately offer a
    // source the register calls something this module has not been taught —
    // WBA's `SAI1_EXT` for `AUDIOCLK` — without that making the pairing wrong.
    known >= 2 && known * 3 >= sources.len() * 2
}

/// The variant this node's current selection means, if any.
///
/// Resolved through the SOURCE the selected input is fed by, never through the
/// index itself. Measured on the shipped WBA55 tree, the two disagree for most
/// selectors: `ADCMult` input 2 is `HSEOSC` while `adcsel` value 2 is `PLL1_P`,
/// and `LPTIM1Mult` is rotated by one against `lptim1sel`. Reading the index as
/// a register value would have put peripherals on the wrong clock SILENTLY —
/// the diagram would still show the frequency the user picked.
pub fn variant_of(graph: &ClockGraph, bound: &Bound) -> Option<&'static str> {
    let node = graph.node(&bound.node)?;
    let NodeState::Index(i) = node.state else {
        // `Unset` is a mux with nothing selected — the peripheral keeps
        // embassy's default, which is exactly what emitting nothing gives it.
        return None;
    };
    let source = &graph
        .edges
        .iter()
        .find(|e| e.to == bound.node && e.input == i)?
        .from;
    bound
        .field
        .variants
        .iter()
        .find(|(_, name)| same_clock(source, name))
        .map(|(_, name)| *name)
}

/// The `config.rcc.mux.…` lines for a whole tree, in field order.
///
/// Empty for a family with no table, for a tree with no mux nodes, and for
/// every selector still on its reset value — a project that has not chosen a
/// peripheral clock gets no line for it, so the generated block stays as small
/// as what the user actually configured.
///
/// A pick that would compile and still be wrong is not written as the tree
/// says it:
///
/// - `rtcsel`: embassy sets the RTC source from `config.rcc.ls`, before it
///   applies `config.rcc.mux`, and RTCSEL holds until a backup-domain reset,
///   so the mux line would do nothing;
/// - a source the firmware does not run ([`source_running`]): the peripheral
///   would panic on its first frequency query ("configured to use the 'pll2_q'
///   clock, which is not running");
/// - the RNG on the LSI or LSE ([`too_slow`]).
///
/// Leaving such a line out is not "the reset clock": embassy's `ClockMux`
/// starts zeroed and `init` writes every field, so the selector gets register
/// value 0. Where that is itself a clock the firmware does not run (the WBA's
/// RNG on the LSE, the H7's ADC on PLL2_P), the selector gets a running one
/// instead ([`stand_in`]), with a comment naming the pick it replaces.
///
/// One line is written whatever the tree says: a WBA's `sai1sel = HSI`
/// ([`lines_without_a_tree`]).
///
/// The LSE is the one source this turns on: the RTC on it, or a peripheral on
/// it where the LSE reaches peripherals ([`reports_lse`]), gets
/// `config.rcc.ls = LsConfig::default_lse()`, which starts it and runs the RTC
/// from it. The board needs its 32.768 kHz crystal, so it is written only when
/// the tree asks for the LSE; no vendor tree does by default (their RTC is on
/// the LSI). `default_lse()` also moves the RTC and stops the LSI, so a block
/// that starts the LSE for a peripheral puts the RTC back on the LSI, and the
/// LSI is switched back on wherever `LsConfig::new()` would have left it on.
pub fn emit_lines(graph: &ClockGraph, chip: &str, family: &str, running: &Running) -> Vec<String> {
    let picks: Vec<(Bound, &'static str)> = propose(graph, chip, family)
        .into_iter()
        .filter(|bound| bound.field.field != "rtcsel")
        .filter_map(|bound| {
            let variant = variant_of(graph, &bound)?;
            Some((bound, variant))
        })
        .collect();
    // Every family's `Config` has `ls` but the N6's (which has its own emitter).
    let rtc_lse = family != "stm32n6" && rtc_on_lse(graph);
    let lse = family != "stm32n6"
        && (rtc_lse
            || picks
                .iter()
                .any(|(b, v)| *v == "LSE" && !too_slow(b.field, v) && reports_lse(family)));
    let wants_lsi = reports_lsi(family)
        && picks
            .iter()
            .any(|(b, v)| *v == "LSI" && !too_slow(b.field, v));
    // `LsConfig::new()` is `default_lsi()`, but `off()` on the L5, where an LSI
    // alone crashes it (embassy's own note): the LSI is never turned on there.
    let lsi = family != "stm32l5" && (!lse || !rtc_lse || wants_lsi);
    let mut out = Vec::new();
    if lse {
        out.push(LSE_ON.to_owned());
        if !rtc_lse {
            out.push(RTC_ON_LSI.to_owned());
        }
        if lsi {
            out.push(LSI_ON.to_owned());
        }
    }
    let ok = |field: &MuxField, variant: &str| {
        !too_slow(field, variant) && source_running(family, running, lse, lsi, variant)
    };
    let wba_sai = wba_sai(chip, family);
    let mut sai_written = false;
    for (bound, variant) in &picks {
        let field = bound.field;
        if wba_sai && field.field == "sai1sel" {
            out.push(WBA_SAI_HSI.to_owned());
            sai_written = true;
        } else if ok(field, variant) {
            out.push(mux_line(field, variant));
        } else if let Some(other) = stand_in(field, &ok) {
            let why = if too_slow(field, variant) {
                "too slow for the RNG"
            } else {
                "not running in this firmware"
            };
            out.push(format!(
                "{} // the tree picks {variant}: {why}",
                mux_line(field, other)
            ));
        }
    }
    if wba_sai && !sai_written {
        out.push(WBA_SAI_HSI.to_owned());
    }
    out
}

/// The lines that start the LSE, and undo what `default_lse()` does besides.
pub(super) const LSE_ON: &str = "        config.rcc.ls = rcc::LsConfig::default_lse();";
pub(super) const RTC_ON_LSI: &str = "        config.rcc.ls.rtc = rcc::RtcClockSource::LSI;";
pub(super) const LSI_ON: &str = "        config.rcc.ls.lsi = true;";

const WBA_SAI_HSI: &str = "        config.rcc.mux.sai1sel = rcc::mux::Sai1sel::HSI;";

fn mux_line(field: &MuxField, variant: &str) -> String {
    format!(
        "        config.rcc.mux.{} = rcc::mux::{}::{variant};",
        field.field, field.enum_name
    )
}

/// The RNG on a 32 kHz clock: embassy's driver runs it with clock error
/// detection on, which needs more than HCLK/32, and `Rng::new` never
/// returns. (The WB's vendor default is the LSI.)
fn too_slow(field: &MuxField, variant: &str) -> bool {
    field.field == "rngsel" && matches!(variant, "LSI" | "LSE")
}

/// What a selector gets when the tree's pick cannot be written. Nothing,
/// when register value 0 - what a missing line leaves - is a clock that
/// runs; else the running variant with the lowest value, a 32 kHz one only
/// when nothing else runs. `None` when no variant runs: then a missing line
/// is all there is.
fn stand_in(field: &MuxField, ok: &impl Fn(&MuxField, &str) -> bool) -> Option<&'static str> {
    let zero = field.variants.iter().find(|(value, _)| *value == 0);
    if zero.is_some_and(|(_, name)| ok(field, name)) {
        return None;
    }
    field
        .variants
        .iter()
        .filter(|(_, name)| ok(field, name))
        .min_by_key(|(value, name)| (matches!(*name, "LSI" | "LSE"), *value))
        .map(|(_, name)| *name)
}

/// Does `init` read this part's `sai1sel`? embassy-stm32 0.6 does on a WBA
/// with SAI1 (`rcc/wba.rs`, `cfg(sai_v4_2pdm)`): SYS and AUDIOCLK panic there,
/// and so does a PLL1 output it was not given - `ClockMux` starts zeroed, so
/// `Config::default()` itself selects PLL1_P and `init(Default::default())`
/// panics with "PLL1.P not configured". HSI is the one choice that works.
fn wba_sai(chip: &str, family: &str) -> bool {
    family == "stm32wba"
        && fields_for_chip(chip, family)
            .iter()
            .any(|f| f.field == "sai1sel")
}

/// The lines a block needs with no tree to read: a WBA with SAI1 cannot
/// boot on embassy's default `ClockMux` ([`wba_sai`]).
pub fn lines_without_a_tree(chip: &str, family: &str) -> Vec<String> {
    if wba_sai(chip, family) {
        vec![WBA_SAI_HSI.to_owned()]
    } else {
        Vec::new()
    }
}

/// Does the tree run the RTC from the LSE? Read off the RTC mux's selected
/// source rather than an `rtcsel` binding: the binding needs most of the
/// mux's inputs recognised (on the L5, U5 and WL the LSI arrives as
/// `LSIDIV`), and the F0, F2, F3, L0 and L1 have no `rtcsel` in `ClockMux` at
/// all - their RTCSEL sits in BDCR or CSR, which `config.rcc.ls` sets on
/// every family.
fn rtc_on_lse(graph: &ClockGraph) -> bool {
    graph.nodes.iter().any(|n| {
        matches!(n.kind, NodeKind::Mux { .. })
            && core_name(&n.id) == "rtc"
            && matches!(n.state, NodeState::Index(i) if graph
                .edges
                .iter()
                .any(|e| e.to == n.id && e.input == i && same_clock(&e.from, "LSE")))
    })
}

/// What the generated clock block turns on, over the family's embassy
/// defaults - what a peripheral clock selector may point at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Running {
    /// The HSI: on by default ([`hsi_by_default`]) or switched on by the block.
    pub hsi: bool,
    /// The HSE, which only the block starts.
    pub hse: bool,
    /// The PLL1 output the block enables (`'p'`, `'q'`, `'r'`): the one that
    /// drives SYSCLK, the only one it configures.
    pub pll1: Option<char>,
}

impl Running {
    /// Nothing but the family's defaults: a block that configures no clock.
    pub fn defaults(family: &str) -> Self {
        Self {
            hsi: hsi_by_default(family),
            hse: false,
            pll1: None,
        }
    }
}

/// The families of embassy-stm32 0.6's `rcc/l.rs`.
fn l_family(family: &str) -> bool {
    matches!(
        family,
        "stm32l0"
            | "stm32l1"
            | "stm32l4"
            | "stm32l4+"
            | "stm32l5"
            | "stm32wb"
            | "stm32wl"
            | "stm32u0"
    )
}

/// HSI in embassy's `Config::default()`: on, but in the `l.rs` families, U3
/// and U5 (which boot on the MSI).
pub fn hsi_by_default(family: &str) -> bool {
    !(l_family(family) || matches!(family, "stm32u3" | "stm32u5"))
}

/// Does a peripheral on the LSE get a clock once `LsConfig::default_lse()`
/// starts it? Embassy reports the LSE only in `l.rs`, `u3.rs` and `u5.rs`
/// (`lse: None` elsewhere), and on the L5, U3, U5 and WL the LSE reaches
/// peripherals only with `peripherals_clocked`, which `default_lse()` leaves
/// off. That leaves the L0, L1, L4, WB and U0. The RTC on the LSE needs none
/// of this.
fn reports_lse(family: &str) -> bool {
    matches!(
        family,
        "stm32l0" | "stm32l1" | "stm32l4" | "stm32l4+" | "stm32wb" | "stm32u0"
    )
}

/// Does a peripheral on the running LSI get a clock? Embassy reports it in
/// `l.rs`, U3, U5 and WBA; `c0`, `g0`, `g4`, `h.rs` and the F families pass
/// `lsi: None`.
fn reports_lsi(family: &str) -> bool {
    l_family(family) || matches!(family, "stm32u3" | "stm32u5" | "stm32wba")
}

/// `X_DIV_n` / `X_MUL_n` are X scaled (embassy's `parse_mul_div`): X decides.
fn unscaled(variant: &str) -> &str {
    for sep in ["_DIV_", "_MUL_"] {
        if let Some((base, n)) = variant.rsplit_once(sep)
            && n.parse::<u32>().is_ok()
        {
            return base;
        }
    }
    variant
}

/// Is `variant` a clock the generated firmware has running on `family`, so
/// that a peripheral on it gets a frequency from embassy? Read off
/// embassy-stm32 0.6's `set_clocks!` per family against what the block
/// writes; anything not known to run is not (a missing line, not a panic):
///
/// - the bus clocks and SYSCLK, always;
/// - HSI (and the C0's HSIKER), HSE, the PLL1 output the block enables, as
///   [`Running`] says; PER, the H5/H7 kernel clock whose CKPERSEL resets to
///   the HSI, with it;
/// - by the family's defaults, which the block leaves alone: the MSI on the
///   `l.rs` families, the MSIK on U3/U5, the HSI48 where `Config::default()`
///   starts it (not F2/F4/F7, not WBA);
/// - the LSI when `lsi` (it runs after the block's `ls` lines) and embassy
///   reports it ([`reports_lsi`]); the LSE when `lse` (the block starts it)
///   and it reaches peripherals ([`reports_lse`]);
/// - never PLL2, PLL3, PLLSAI, PLLI2S, CSI, the external and audio clocks:
///   no block configures them.
fn source_running(family: &str, running: &Running, lse: bool, lsi: bool, variant: &str) -> bool {
    let base = unscaled(variant);
    match base {
        "SYS" => true,
        b if b.starts_with("HCLK") || b.starts_with("PCLK") => true,
        "HSI" | "HSIKER" => running.hsi,
        "HSE" => running.hse,
        "PER" => running.hsi && matches!(family, "stm32h5" | "stm32h7"),
        "MSI" => l_family(family),
        "MSIK" => matches!(family, "stm32u3" | "stm32u5"),
        "HSI48" => {
            matches!(
                family,
                "stm32f0"
                    | "stm32f1"
                    | "stm32f3"
                    | "stm32c0"
                    | "stm32g0"
                    | "stm32g4"
                    | "stm32h5"
                    | "stm32h7"
                    | "stm32u3"
                    | "stm32u5"
            ) || l_family(family)
        }
        "LSI" => lsi && reports_lsi(family),
        "LSE" => lse && reports_lse(family),
        _ => match base.strip_prefix("PLL1_") {
            Some(out) if out.len() == 1 => {
                running.pll1 == out.chars().next().map(|c| c.to_ascii_lowercase())
            }
            _ => false,
        },
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The naming vocabularies this module has to reconcile.
    #[test]
    fn a_selector_is_recognised_however_it_is_spelled() {
        for raw in ["USART1Mult", "usart1sel", "USART1CLockSelection", "USART1"] {
            assert_eq!(core_name(raw), "usart1", "for {raw}");
        }
        assert_eq!(core_name("I2C1Mult"), "i2c1");
        assert_eq!(core_name("ADCMult"), "adc");
        // A name that IS the decoration keeps itself rather than emptying out.
        assert_eq!(core_name("sel"), "sel");
    }

    /// Tree words vs register words, in both directions.
    #[test]
    fn the_two_vocabularies_line_up() {
        assert!(same_clock("APB2Prescaler", "PCLK2"));
        assert!(same_clock("SysCLKOutput", "SYS"));
        assert!(same_clock("HSIRC", "HSI"));
        assert!(same_clock("LSEOSC", "LSE"));
        // PLL legs match structurally rather than from a list.
        assert!(same_clock("PLL1Qoutput", "PLL1_Q"));
        // And a mismatch is a mismatch — this is what keeps a wrong line out.
        assert!(!same_clock("APB1Prescaler", "PCLK2"));
        assert!(!same_clock("HSIRC", "HSE"));
    }

    /// A family is not one register block — the reason this is keyed by chip.
    #[test]
    fn a_part_number_picks_its_own_register_block() {
        // Six F410s sit on their own version while 143 other F4s do not.
        assert_eq!(rcc_version("stm32f410t8yx"), Some("f410"));
        assert_eq!(rcc_version("stm32f411retx"), Some("f4"));
        // Four versions across STM32H7.
        assert_eq!(rcc_version("stm32h743zitx"), Some("h7rm0433"));
        assert_ne!(rcc_version("stm32h7a3zitx"), rcc_version("stm32h743zitx"));
        // The G0 split that kept this family out of the first table.
        assert_eq!(rcc_version("stm32g030f6px"), Some("g0x0"));
        assert_eq!(rcc_version("stm32g031k8tx"), Some("g0x1"));
        // And the H5 one.
        assert_eq!(rcc_version("stm32h503rbtx"), Some("h50"));
        assert_eq!(rcc_version("stm32h563zitx"), Some("h5"));

        // Case does not matter, and the package suffix is ignored.
        assert_eq!(rcc_version("STM32WBA55CGUx"), Some("wba"));
        assert_eq!(rcc_version("stm32wba55"), Some("wba"));

        // A part nobody has heard of falls back to its family, but only when
        // that family has one answer.
        assert!(rcc_version("stm32zz99").is_none());
        assert!(
            !fields_for_chip("stm32zz99", "stm32u5").is_empty(),
            "u5 is single-version"
        );
        assert!(
            fields_for_chip("stm32h799xx", "stm32h7").is_empty(),
            "an unknown H7 gets nothing rather than one of the four guesses"
        );
    }

    /// The families the first table could not serve now do.
    #[test]
    fn the_ambiguous_families_are_covered_per_chip() {
        for (chip, family) in [
            ("stm32g0b1retx", "stm32g0"),
            ("stm32h563zitx", "stm32h5"),
            ("stm32u575zitx", "stm32u5"),
            ("stm32l552zetx", "stm32l5"),
            ("stm32c011f4ux", "stm32c0"),
            ("stm32f303retx", "stm32f3"),
            ("stm32wle5jcix", "stm32wl"),
            ("stm32wb55rgvx", "stm32wb"),
            ("stm32l4p5cetx", "stm32l4+"),
        ] {
            let fields = fields_for_chip(chip, family);
            // How MANY a family exposes is the vendor's business — L5 declares
            // only four kernel-clock muxes (`adcsel`, `clk48sel`, `fdcansel`,
            // `rtcsel`), and that is the right answer for L5, not a gap to
            // paper over.
            assert!(!fields.is_empty(), "{chip} has no selectors at all");
        }
    }

    /// A part keeps only the selectors it has: the L010 has no USART1, while
    /// the L071 on the same RCC version does - and the CubeMX tree of either
    /// carries the L0 line's USART1 mux.
    #[test]
    fn a_part_does_not_get_a_selector_it_lacks() {
        let has = |chip: &str, field: &str| {
            fields_for_chip(chip, "stm32l0")
                .iter()
                .any(|f| f.field == field)
        };
        assert_eq!(rcc_version("stm32l010c6tx"), rcc_version("stm32l071c8tx"));
        assert!(has("stm32l071c8tx", "usart1sel"));
        assert!(
            !has("stm32l010c6tx", "usart1sel"),
            "{:?}",
            lacks("stm32l010c6tx")
        );
        assert!(has("stm32l010c6tx", "usart2sel"), "only what it lacks goes");
        // Every entry names selectors of the version its prefix is on.
        for (prefix, fields) in super::super::rcc_mux_data::CHIP_LACKS {
            assert!(!fields.is_empty(), "{prefix}: an empty entry is no entry");
            let version = rcc_version(prefix).unwrap_or_else(|| panic!("{prefix}: no version"));
            let table = super::super::rcc_mux_data::VERSIONS
                .iter()
                .find(|(v, _)| *v == version)
                .map(|(_, f)| *f)
                .unwrap_or(&[]);
            for f in *fields {
                assert!(
                    table.iter().any(|t| t.field == *f),
                    "{prefix}: {f} is not {version}'s"
                );
            }
        }
    }

    /// Every harvested table is well-formed: no empty variant list, no field
    /// that would emit a line naming nothing.
    #[test]
    fn the_harvested_tables_are_sane() {
        let mut families = 0;
        let mut selectors = 0;
        for (family, fields) in super::super::rcc_mux_data::VERSIONS {
            families += 1;
            assert!(!fields.is_empty(), "{family} has no selectors");
            for f in *fields {
                selectors += 1;
                assert!(!f.field.is_empty() && !f.enum_name.is_empty(), "{family}");
                assert!(!f.variants.is_empty(), "{}::{}", family, f.field);
                assert!(
                    f.enum_name.starts_with(|c: char| c.is_ascii_uppercase()),
                    "an enum name is capitalised: {}",
                    f.enum_name
                );
            }
        }
        assert_eq!(families, 35, "one table per RCC version that has selectors");
        // 477 until 2026-10-21: the first harvester's pattern had dropped 49
        // (C0 USART1SEL, H7 LPUART1SEL, the RTCSELs...) and kept 34 embassy
        // has no field for (DFSDM, SWPMI, ADF, MDF, most of the N6) - see
        // `rcc_mux_harvest`.
        assert_eq!(selectors, 492, "the table is the harvest, unedited");

        // The case that proves the table is not a rule: field != enum.
        let wba = fields_for("stm32wba");
        let u2 = wba.iter().find(|f| f.field == "usart2sel").unwrap();
        assert_eq!(u2.enum_name, "Usartsel", "not `Usart2sel`");

        // And a family with no table asks for nothing.
        assert!(
            fields_for("stm32g0").is_empty(),
            "g0 is deliberately absent"
        );
        assert!(fields_for("esp32c3").is_empty());
    }

    /// Against the shipped WBA55 tree — 81 nodes, 16 of them peripheral muxes.
    ///
    /// This is the measurement that says whether the confirmation rule is too
    /// strict to be useful or loose enough to be wrong, so it PRINTS what it
    /// bound and what it refused.
    #[test]
    fn the_shipped_wba_tree_binds_its_selectors() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let muxes: Vec<&str> = gc
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Mux { .. }))
            .map(|n| n.id.as_str())
            .collect();

        let bound = propose(&gc.graph, "stm32wba55cgux", "stm32wba");
        let names: Vec<&str> = bound.iter().map(|b| b.node.as_str()).collect();
        println!(
            "{} mux nodes, {} bound: {names:?}",
            muxes.len(),
            bound.len()
        );
        let refused: Vec<&&str> = muxes.iter().filter(|m| !names.contains(m)).collect();
        println!("refused: {refused:?}");

        assert!(
            bound.len() >= 8,
            "the rule is too strict to be useful: only {} of {}",
            bound.len(),
            muxes.len()
        );
        // Every binding must name a selector this family really has.
        for b in &bound {
            assert!(
                fields_for("stm32wba")
                    .iter()
                    .any(|f| f.field == b.field.field),
                "{} is not a WBA selector",
                b.field.field
            );
        }

        let lines = emit_lines(
            &gc.graph,
            "stm32wba55cgux",
            "stm32wba",
            &Running::defaults("stm32wba"),
        );
        for l in &lines {
            println!("{l}");
        }
        assert!(!lines.is_empty(), "the tree's selections emit something");
    }

    /// Lines that would compile and be wrong are not written: never `rtcsel`
    /// (embassy takes the RTC source from `config.rcc.ls`), never a source
    /// the firmware does not run. On the WBA, where embassy does not report
    /// the LSE, a selector moved onto its LSE input emits nothing; the RTC
    /// moved onto the LSE starts it through `config.rcc.ls` instead.
    #[test]
    fn no_line_runs_a_peripheral_off_a_clock_that_is_not_running() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let (chip, family) = ("stm32wba55cgux", "stm32wba");
        let running = Running::defaults(family);
        let lines = emit_lines(&gc.graph, chip, family, &running);
        assert!(lines.iter().all(|l| !l.contains("rtcsel")), "{lines:#?}");
        assert!(lines.iter().all(|l| !l.contains("LsConfig")), "{lines:#?}");

        // Every bound selector that can take the LSE, moved onto it.
        let mut moved = 0;
        let mut rtc_on_lse = false;
        for bound in propose(&gc.graph, chip, family) {
            let Some(input) = gc
                .graph
                .edges
                .iter()
                .find(|e| e.to == bound.node && same_clock(&e.from, "LSE"))
            else {
                continue;
            };
            let mut g = gc.graph.clone();
            g.node_mut(&bound.node).expect("the bound node").state = NodeState::Index(input.input);
            let lines = emit_lines(&g, chip, family, &running);
            let field = format!("config.rcc.mux.{} ", bound.field.field);
            // At most a stand-in for a stopped value 0 (the RNG's LSE).
            assert!(
                lines
                    .iter()
                    .filter(|l| l.contains(&field))
                    .all(|l| !l.contains("::LSE;") && l.contains("// the tree picks LSE")),
                "{} on LSE: {lines:#?}",
                bound.field.field
            );
            if bound.field.field == "rtcsel" {
                assert_eq!(
                    lines.first().map(String::as_str),
                    Some(LSE_ON),
                    "the RTC on the LSE starts it"
                );
                rtc_on_lse = true;
            } else {
                assert!(lines.iter().all(|l| !l.contains("LsConfig")), "{lines:#?}");
            }
            moved += 1;
        }
        assert!(
            moved > 1,
            "the WBA tree has selectors that can take the LSE"
        );
        assert!(rtc_on_lse, "and its RTC can");
    }

    /// A selector on the HSE gets its line only when the block STARTS the
    /// HSE - SYSCLK on it or on a PLL fed by it - not because the tree has
    /// it ticked: embassy reports no HSE it was not given (`config.rcc.hse`).
    #[test]
    fn a_selector_on_the_hse_needs_the_block_to_start_it() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;
        use crate::panels::mcu_module::clock::model::ClockConfig;
        use crate::panels::mcu_module::codegen::rcc::graph_clock_block_for;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let mut gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let (chip, family) = ("stm32wba55cgux", "stm32wba");
        let bound = propose(&gc.graph, chip, family)
            .into_iter()
            .find(|b| {
                gc.graph
                    .edges
                    .iter()
                    .any(|e| e.to == b.node && same_clock(&e.from, "HSE"))
            })
            .expect("a WBA selector that can take the HSE");
        let input = gc
            .graph
            .edges
            .iter()
            .find(|e| e.to == bound.node && same_clock(&e.from, "HSE"))
            .expect("its HSE input")
            .input;
        gc.graph
            .node_mut(&bound.node)
            .expect("the bound node")
            .state = NodeState::Index(input);
        if let Some(NodeState::Source { enabled, .. }) =
            gc.graph.node_mut("HSEOSC").map(|n| &mut n.state)
        {
            *enabled = true;
        }
        let line = format!(
            "config.rcc.mux.{} = rcc::mux::{}::HSE;",
            bound.field.field, bound.field.enum_name
        );
        let sw: Vec<usize> = gc
            .graph
            .edges
            .iter()
            .filter(|e| e.to == "SysClkSource")
            .map(|e| e.input)
            .collect();
        let pll: Vec<usize> = gc
            .graph
            .edges
            .iter()
            .filter(|e| e.to == "PLLSource")
            .map(|e| e.input)
            .collect();
        let (mut started, mut not_started) = (0, 0);
        for &s in &sw {
            for &p in &pll {
                let mut g = gc.clone();
                g.graph.node_mut("SysClkSource").expect("sw").state = NodeState::Index(s);
                g.graph.node_mut("PLLSource").expect("pllsrc").state = NodeState::Index(p);
                let block = graph_clock_block_for(chip, family, &ClockConfig::Graph(g), false);
                let hse = block.contains("config.rcc.hse = Some");
                assert_eq!(block.contains(&line), hse, "sw {s} pll {p}:\n{block}");
                if hse {
                    started += 1;
                } else {
                    not_started += 1;
                }
            }
        }
        assert!(started > 0 && not_started > 0, "{started} / {not_started}");
    }

    /// A WBA with SAI1 writes `sai1sel = HSI` whatever its tree selects, and
    /// without a tree: embassy's default selects PLL1_P and `init` panics. A
    /// WBA without SAI1 (WBA50/52) has no such field to write.
    #[test]
    fn a_wba_with_sai1_always_runs_it_on_the_hsi() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let (chip, family) = ("stm32wba55cgux", "stm32wba");
        let running = Running::defaults(family);
        let sai = |lines: &[String]| -> Vec<String> {
            lines
                .iter()
                .filter(|l| l.contains("sai1sel"))
                .cloned()
                .collect()
        };
        let want = vec![WBA_SAI_HSI.to_owned()];
        assert_eq!(sai(&emit_lines(&gc.graph, chip, family, &running)), want);

        // Every input of the SAI1 mux, the panicking ones included.
        let node = propose(&gc.graph, chip, family)
            .into_iter()
            .find(|b| b.field.field == "sai1sel")
            .expect("the WBA55 tree binds its SAI1 mux")
            .node;
        let inputs: Vec<usize> = gc
            .graph
            .edges
            .iter()
            .filter(|e| e.to == node)
            .map(|e| e.input)
            .collect();
        assert!(inputs.len() > 2, "{inputs:?}");
        for i in inputs {
            let mut g = gc.graph.clone();
            g.node_mut(&node).expect("the SAI1 mux").state = NodeState::Index(i);
            assert_eq!(
                sai(&emit_lines(&g, chip, family, &running)),
                want,
                "input {i}"
            );
        }

        assert_eq!(lines_without_a_tree(chip, family), want);
        assert_eq!(lines_without_a_tree("stm32wba65rix", family), want);
        assert!(lines_without_a_tree("stm32wba52cgux", family).is_empty());
        assert!(lines_without_a_tree("stm32g474re", "stm32g4").is_empty());
        // The WBA52 reads the WBA5 tree too; its lines name no SAI1.
        let wba52 = emit_lines(&gc.graph, "stm32wba52cgux", family, &running);
        assert!(sai(&wba52).is_empty(), "{wba52:#?}");
    }

    /// The RNG on a 32 kHz clock hangs embassy's `Rng::new`. Leaving the
    /// line out is no escape on the WBA - its RNGSEL value 0, what a missing
    /// line leaves, IS the LSE - so the RNG gets the HSI there.
    #[test]
    fn the_rng_never_runs_on_a_32_khz_clock() {
        use crate::panels::mcu_module::clock::graph::parse_clock_ron;

        let ron = include_str!("../../../../assets/mcus/examples/stm32wba55_graphclock.ron");
        let gc = parse_clock_ron(ron).expect("the shipped tree parses");
        let (chip, family) = ("stm32wba55cgux", "stm32wba");
        let rng = propose(&gc.graph, chip, family)
            .into_iter()
            .find(|b| b.field.field == "rngsel")
            .expect("the WBA55 tree binds its RNG mux");
        for variant in ["LSI", "LSE"] {
            let edge = gc
                .graph
                .edges
                .iter()
                .find(|e| e.to == rng.node && same_clock(&e.from, variant))
                .expect("the WBA55's RNG mux has both 32 kHz inputs");
            let mut g = gc.graph.clone();
            g.node_mut(&rng.node).expect("the RNG mux").state = NodeState::Index(edge.input);
            // Even where the source runs: the LSI on a WBA does.
            let lines = emit_lines(&g, chip, family, &Running::defaults(family));
            let rng: Vec<&String> = lines.iter().filter(|l| l.contains("rngsel")).collect();
            assert_eq!(
                rng,
                [&format!(
                    "        config.rcc.mux.rngsel = rcc::mux::Rngsel::HSI; // the tree picks {variant}: too slow for the RNG"
                )],
                "{lines:#?}"
            );
            assert!(
                lines.iter().all(|l| !l.contains("LsConfig")),
                "{variant}: {lines:#?}"
            );
        }
    }

    /// Where embassy reports the LSE (the WB), the RNG picked on it does not
    /// start it: the pick is dropped BEFORE the LSE is decided. And where
    /// nothing else the RNG takes runs, no stand-in is invented: the WB's
    /// value 0 is CLK48, which this module does not follow, so the line is
    /// just left out.
    #[test]
    fn an_rng_on_the_lse_does_not_start_it() {
        use crate::panels::mcu_module::clock::graph::Edge;
        use crate::panels::mcu_module::clock::graph::minimal_graph;

        let mut g = minimal_graph();
        let source = g.node("hse").expect("an oscillator").clone();
        for id in ["CLK48Output", "LSIRC", "LSEOSC"] {
            let mut n = source.clone();
            n.id = id.into();
            g.nodes.push(n);
        }
        let mut rng = g.node("sw").expect("a mux").clone();
        rng.id = "RNGMult".into();
        rng.kind = NodeKind::Mux { inputs: 3 };
        g.nodes.push(rng);
        for (from, input) in [("CLK48Output", 0), ("LSIRC", 1), ("LSEOSC", 2)] {
            g.edges.push(Edge {
                from: from.into(),
                to: "RNGMult".into(),
                input,
            });
        }
        let (chip, family) = ("stm32wb55rgvx", "stm32wb");
        assert!(reports_lse(family), "the case this test is for");
        assert!(
            propose(&g, chip, family)
                .iter()
                .any(|b| b.field.field == "rngsel"),
            "the RNG mux binds"
        );
        for (input, variant) in [(1, "LSI"), (2, "LSE")] {
            g.node_mut("RNGMult").expect("the RNG mux").state = NodeState::Index(input);
            let lines = emit_lines(&g, chip, family, &Running::defaults(family));
            assert!(lines.is_empty(), "{variant}: {lines:#?}");
        }
    }

    /// A stand-in only where the missing line would leave a stopped clock:
    /// the WBA's USART on its LSE gets nothing (value 0 is PCLK2, which
    /// runs), the WBA's RNG gets the HSI (value 0 is the LSE).
    #[test]
    fn a_stand_in_only_replaces_a_stopped_default() {
        let wba = fields_for_chip("stm32wba55cgux", "stm32wba");
        let field = |name: &str| *wba.iter().find(|f| f.field == name).expect(name);
        let running = Running::defaults("stm32wba");
        let ok = |f: &MuxField, v: &str| {
            !too_slow(f, v) && source_running("stm32wba", &running, false, true, v)
        };
        assert_eq!(stand_in(field("usart1sel"), &ok), None);
        assert_eq!(stand_in(field("rngsel"), &ok), Some("HSI"));
        // A 32 kHz clock only when nothing faster runs: the LPTIM with no
        // bus clock left.
        let lptim = MuxField {
            field: "lptim1sel",
            enum_name: "Lptimsel",
            variants: &[(0, "PLL2_P"), (1, "LSI"), (2, "HSI")],
        };
        assert_eq!(stand_in(&lptim, &ok), Some("HSI"));
        let off = |f: &MuxField, v: &str| v == "LSI" && ok(f, v);
        assert_eq!(stand_in(&lptim, &off), Some("LSI"));
        assert_eq!(stand_in(&lptim, &|_: &MuxField, _: &str| false), None);
    }

    /// `default_lse()` moves the RTC to the LSE and stops the LSI. Started
    /// for a peripheral (the L4's LPUART), the RTC goes back on the LSI and
    /// the LSI back on; started for the RTC, the LSI comes back only for a
    /// selector that uses it.
    #[test]
    fn starting_the_lse_moves_nothing_else() {
        use crate::panels::mcu_module::clock::graph::Edge;
        use crate::panels::mcu_module::clock::graph::minimal_graph;

        let mut g = minimal_graph();
        let source = g.node("hse").expect("an oscillator").clone();
        for id in ["APB1Output", "SysCLKOutput", "LSIRC", "LSEOSC"] {
            let mut n = source.clone();
            n.id = id.into();
            g.nodes.push(n);
        }
        let mux = g.node("sw").expect("a mux").clone();
        for (id, inputs) in [
            (
                "LPUART1Mult",
                ["APB1Output", "SysCLKOutput", "hsi", "LSEOSC"].as_slice(),
            ),
            (
                "LPTIM1Mult",
                ["APB1Output", "LSIRC", "hsi", "LSEOSC"].as_slice(),
            ),
            ("RTCClkSource", ["LSEOSC", "LSIRC"].as_slice()),
        ] {
            let mut n = mux.clone();
            n.id = id.into();
            n.kind = NodeKind::Mux {
                inputs: inputs.len(),
            };
            n.state = NodeState::Index(0);
            g.nodes.push(n);
            for (input, from) in inputs.iter().enumerate() {
                g.edges.push(Edge {
                    from: (*from).into(),
                    to: id.into(),
                    input,
                });
            }
        }
        let (chip, family) = ("stm32l476rgtx", "stm32l4");
        let bound = propose(&g, chip, family);
        for field in ["lpuart1sel", "lptim1sel"] {
            assert!(
                bound.iter().any(|b| b.field.field == field),
                "{field} binds"
            );
        }
        let running = Running::defaults(family);
        let at = |g: &mut ClockGraph, lpuart: usize, lptim: usize, rtc: usize| {
            for (id, i) in [
                ("LPUART1Mult", lpuart),
                ("LPTIM1Mult", lptim),
                ("RTCClkSource", rtc),
            ] {
                g.node_mut(id).expect(id).state = NodeState::Index(i);
            }
            emit_lines(g, chip, family, &running)
        };
        let lpuart_lse = "        config.rcc.mux.lpuart1sel = rcc::mux::Lpuart1sel::LSE;";
        let lptim_lsi = "        config.rcc.mux.lptim1sel = rcc::mux::Lptim1sel::LSI;";

        // The LPUART on the LSE, the LPTIM and the RTC on the LSI.
        let lines = at(&mut g, 3, 1, 1);
        assert_eq!(lines[..3], [LSE_ON, RTC_ON_LSI, LSI_ON], "{lines:#?}");
        assert!(lines.iter().any(|l| l == lpuart_lse), "{lines:#?}");
        assert!(lines.iter().any(|l| l == lptim_lsi), "{lines:#?}");

        // The RTC on the LSE, the LPTIM on the LSI.
        let lines = at(&mut g, 0, 1, 0);
        assert_eq!(lines[..2], [LSE_ON, LSI_ON], "{lines:#?}");
        assert!(!lines.iter().any(|l| l == RTC_ON_LSI), "{lines:#?}");
        assert!(lines.iter().any(|l| l == lptim_lsi), "{lines:#?}");

        // The RTC on the LSE and nothing on the LSI: `default_lse()` alone.
        let lines = at(&mut g, 0, 0, 0);
        assert_eq!(lines[0], LSE_ON, "{lines:#?}");
        assert!(
            lines.iter().all(|l| l != RTC_ON_LSI && l != LSI_ON),
            "{lines:#?}"
        );

        // No LSE at all: no `ls` line, and the LSI runs as embassy leaves it.
        let lines = at(&mut g, 0, 1, 1);
        assert!(
            lines.iter().all(|l| !l.contains("config.rcc.ls")),
            "{lines:#?}"
        );
        assert!(lines.iter().any(|l| l == lptim_lsi), "{lines:#?}");
    }

    /// The RTC on the LSE is read off the RTC mux itself, so it starts the
    /// LSE even where the `rtcsel` binding is refused - the L5/U5/WL trees
    /// draw their LSI as `LSIDIV`, which names no `Rtcsel` variant - and on
    /// the F0/F2/F3/L0/L1, which have no `rtcsel` in `ClockMux` at all.
    #[test]
    fn the_rtc_on_the_lse_is_read_off_its_own_mux() {
        use crate::panels::mcu_module::clock::graph::Edge;
        use crate::panels::mcu_module::clock::graph::minimal_graph;

        let mut g = minimal_graph();
        let source = g.node("hse").expect("an oscillator").clone();
        for id in ["LSIDIV", "LSEOSC"] {
            let mut n = source.clone();
            n.id = id.into();
            g.nodes.push(n);
        }
        let mut rtc = g.node("sw").expect("a mux").clone();
        rtc.id = "RTCClkSource".into();
        rtc.kind = NodeKind::Mux { inputs: 2 };
        g.nodes.push(rtc);
        for (from, input) in [("LSIDIV", 0), ("LSEOSC", 1)] {
            g.edges.push(Edge {
                from: from.into(),
                to: "RTCClkSource".into(),
                input,
            });
        }
        let (chip, family) = ("stm32l552zetxq", "stm32l5");
        assert!(
            propose(&g, chip, family)
                .iter()
                .all(|b| b.field.field != "rtcsel"),
            "the binding is refused, as on ST's L5 tree"
        );
        let running = Running::defaults(family);

        g.node_mut("RTCClkSource").expect("the RTC mux").state = NodeState::Index(1);
        let lines = emit_lines(&g, chip, family, &running);
        // No `ls.lsi = true` either: an LSI alone crashes the L5.
        assert_eq!(lines, [LSE_ON], "{lines:#?}");
        // A part with no `rtcsel` field.
        let f0 = "stm32f072rbtx";
        assert!(
            fields_for_chip(f0, "stm32f0")
                .iter()
                .all(|f| f.field != "rtcsel")
        );
        let lines = emit_lines(&g, f0, "stm32f0", &Running::defaults("stm32f0"));
        assert_eq!(lines, [LSE_ON], "{lines:#?}");
        // The N6's `Config` has no `ls`.
        assert!(emit_lines(&g, "stm32n657x0", "stm32n6", &Running::defaults("stm32n6")).is_empty());

        g.node_mut("RTCClkSource").expect("the RTC mux").state = NodeState::Index(0);
        let lines = emit_lines(&g, chip, family, &running);
        assert!(lines.iter().all(|l| !l.contains("LsConfig")), "{lines:#?}");
    }

    /// The running clocks, family by family, as embassy-stm32 0.6's
    /// `set_clocks!` reports them under what the block writes.
    #[test]
    fn a_source_runs_only_where_embassy_reports_it() {
        // The LSI as a block with no `ls.lsi` line leaves it: `LsConfig::new()`
        // runs it but on the L5, `default_lse()` stops it.
        let run = |family: &str, r: &Running, lse: bool, v: &str| {
            source_running(family, r, lse, !lse && family != "stm32l5", v)
        };
        let g4 = Running::defaults("stm32g4");
        let l4 = Running::defaults("stm32l4");
        // The buses, always; scaled clocks by their base.
        for v in ["SYS", "PCLK1", "PCLK2_TIM", "HCLK3", "PCLK7"] {
            assert!(run("stm32g4", &g4, false, v), "{v}");
        }
        // The HSI by default where embassy starts it, else when the block does.
        assert!(run("stm32g4", &g4, false, "HSI"));
        assert!(!run("stm32l4", &l4, false, "HSI"));
        let l4_hsi = Running { hsi: true, ..l4 };
        assert!(run("stm32l4", &l4_hsi, false, "HSI"));
        // The HSE only started by the block; its dividers with it.
        assert!(!run("stm32g4", &g4, false, "HSE"));
        let hse = Running { hse: true, ..g4 };
        assert!(run("stm32g4", &hse, false, "HSE") && run("stm32g4", &hse, false, "HSE_DIV_32"));
        // The PLL1 output the block enables, and no other PLL.
        let pll_r = Running {
            pll1: Some('r'),
            ..g4
        };
        assert!(run("stm32g4", &pll_r, false, "PLL1_R"));
        assert!(!run("stm32g4", &pll_r, false, "PLL1_Q"));
        assert!(!run(
            "stm32h7",
            &Running {
                pll1: Some('p'),
                ..Running::defaults("stm32h7")
            },
            false,
            "PLL2_P"
        ));
        let f3 = Running {
            pll1: Some('p'),
            ..Running::defaults("stm32f3")
        };
        assert!(run("stm32f3", &f3, false, "PLL1_P_MUL_2"));
        // The family's defaults.
        assert!(run("stm32l4", &l4, false, "MSI") && !run("stm32g4", &g4, false, "MSI"));
        assert!(run("stm32g4", &g4, false, "HSI48"));
        assert!(!run(
            "stm32f4",
            &Running::defaults("stm32f4"),
            false,
            "HSI48"
        ));
        assert!(
            run("stm32l4", &l4, false, "LSI")
                && run("stm32wba", &Running::defaults("stm32wba"), false, "LSI")
        );
        assert!(!run("stm32g0", &Running::defaults("stm32g0"), false, "LSI"));
        assert!(!run("stm32h7", &Running::defaults("stm32h7"), false, "LSI"));
        // The LSE once the block starts it, where embassy reports it.
        assert!(!run("stm32l4", &l4, false, "LSE"));
        assert!(run("stm32l4", &l4, true, "LSE"));
        assert!(!run("stm32g0", &Running::defaults("stm32g0"), true, "LSE"));
        // ...and reaches peripherals: not without `peripherals_clocked` (U5).
        assert!(!run("stm32u5", &Running::defaults("stm32u5"), true, "LSE"));
        // `default_lse()` switches the LSI off; the L5 never has it on.
        assert!(!run("stm32l4", &l4, true, "LSI"));
        assert!(!run("stm32l5", &Running::defaults("stm32l5"), false, "LSI"));
        // Switched back on (`ls.lsi = true`), where embassy reports it.
        assert!(source_running("stm32l4", &l4, true, true, "LSI"));
        assert!(!source_running(
            "stm32g0",
            &Running::defaults("stm32g0"),
            true,
            true,
            "LSI"
        ));
        // PER resets to the HSI.
        assert!(run("stm32h7", &Running::defaults("stm32h7"), false, "PER"));
        // Nothing the block configures: never.
        for v in [
            "PLLSAI1_R",
            "PLL3_Q",
            "PLLI2S1_Q",
            "CSI",
            "I2S_CKIN",
            "AUDIOCLK",
            "DSI_PHY",
            "CLK48",
            "SAI1_EXTCLK",
        ] {
            assert!(!run("stm32l4", &l4, true, v), "{v}");
        }
    }

    /// Values are carried, not assumed to be 0-based.
    ///
    /// `usbsw` starts at 1 because 0 is a reserved encoding, so indexing the
    /// variant list by position would name the wrong clock.
    #[test]
    fn a_selector_that_does_not_start_at_zero() {
        let odd = super::super::rcc_mux_data::VERSIONS
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .find(|f| f.variants[0].0 != 0)
            .expect("at least one selector skips value 0");
        assert!(
            odd.variants[0].0 > 0,
            "{} starts at {}",
            odd.field,
            odd.variants[0].0
        );
    }
}
