//! Analog comparators (COMP) — the Configuration tab's second peripheral.
//!
//! # Why it lives here and not on the Pins canvas
//!
//! A comparator is half pin-less. Its non-inverting input IS a pin, and that
//! pin is chosen the ordinary way (`COMP{n}_INP` on the Pins tab). Everything
//! else — the reference it compares against, hysteresis, power mode, output
//! polarity — is a register with no pin at all, exactly like a watchdog's
//! period. So the pin comes from the canvas and the settings from a card here,
//! and the card refuses to generate anything until the pin exists.
//!
//! # Why STM32G4 only
//!
//! `embassy_stm32::comp` implements `Instance` under `#[cfg(comp_v2)]` and
//! `#[cfg(comp_u5)]` and nowhere else. Counted across `stm32-metapac 21`, that
//! is 93 chips (the whole G4 family) and 105 (U5 + WBA) out of the 1522 that
//! have a comparator at all — L4's `v3`, H7's `h7_b`, G0's `v1` and the rest
//! have registers in the PAC but no driver.
//!
//! Worse for the F3 that raised the question: `STM32F303`/`F358` list COMP1..7
//! as peripherals in metapac with `registers: None` — no register block at all,
//! so nothing can be generated for them without new data upstream.
//!
//! Both generations are supported here, and they are NOT the same peripheral:
//!
//! | | `comp_v2` (G4) | `comp_u5` (U5, WBA) |
//! |---|---|---|
//! | instances | COMP1..7 (COMP1..4 on a G431) | COMP1, COMP2 |
//! | hysteresis | 8 steps, 10..70 mV | 4 levels, None/Low/Medium/High |
//! | `[-]` pin | INM1 and INM2 | INM1 only |
//! | speed / power | **not written by embassy** | written |
//! | interrupt | `COMP1_2_3` / `COMP4_5_6` / `COMP7` | one vector, `COMP` |
//!
//! The power-mode row is the one worth pausing on: `Config::power_mode` exists
//! on both, but `configure_raw` only writes it under `#[cfg(comp_u5)]`. Showing
//! it on a G4 would be a setting the driver silently drops, so it is hidden
//! there — the same rule that keeps Trigger Mode and Output Internal Selection
//! off the card.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which comparator the chip has — the two embassy implements, which differ in
/// more than a version number (see the module doc).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Generation {
    /// `comp_v2`: STM32G4.
    V2,
    /// `comp_u5`: STM32U5 and STM32WBA.
    U5,
}

impl Generation {
    /// The generation this family has, or `None` when embassy has no driver
    /// for it — which is most families, see the module doc.
    pub fn of(family: &str) -> Option<Generation> {
        match family {
            "stm32g4" => Some(Generation::V2),
            "stm32u5" | "stm32wba" => Some(Generation::U5),
            _ => None,
        }
    }

    /// Does embassy write `Config::power_mode` on this generation?
    ///
    /// Only on U5. `configure_raw` computes `pwrmode` under `#[cfg(comp_u5)]`
    /// and nowhere else, so on a G4 the field is inert.
    pub fn has_power_mode(self) -> bool {
        self == Generation::U5
    }
}

/// What the comparator's inverting input is wired to.
///
/// The names and the order are `embassy_stm32::comp::InvertingInput`'s; the
/// labels are CubeMX's, so the same choice reads the same in both tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum InvertingInput {
    /// 1/4 VREFINT.
    QuarterVref,
    /// 1/2 VREFINT — embassy's own default.
    #[default]
    HalfVref,
    /// 3/4 VREFINT.
    ThreeQuarterVref,
    /// VREFINT.
    Vref,
    /// DAC channel 1 output.
    Dac1,
    /// DAC channel 2 output.
    Dac2,
    /// The external `COMP{n}_INM` pin.
    InputPin,
    /// The second external pin. `comp_v2` only — U5 has one INM pin.
    InputPin2,
}

impl InvertingInput {
    pub const ALL: [InvertingInput; 8] = [
        InvertingInput::QuarterVref,
        InvertingInput::HalfVref,
        InvertingInput::ThreeQuarterVref,
        InvertingInput::Vref,
        InvertingInput::Dac1,
        InvertingInput::Dac2,
        InvertingInput::InputPin,
        InvertingInput::InputPin2,
    ];

    /// The ones this generation can express.
    pub fn options(g: Generation) -> &'static [InvertingInput] {
        match g {
            Generation::V2 => &Self::ALL,
            // `InvertingInput::InputPin2` is `#[cfg(comp_v2)]`: naming it in a
            // U5 project would not compile.
            Generation::U5 => &Self::ALL[..7],
        }
    }

    /// CubeMX's wording for the dropdown.
    pub fn label(self) -> &'static str {
        match self {
            InvertingInput::QuarterVref => "1/4 Internal VRef",
            InvertingInput::HalfVref => "1/2 Internal VRef",
            InvertingInput::ThreeQuarterVref => "3/4 Internal VRef",
            InvertingInput::Vref => "Internal VRef",
            InvertingInput::Dac1 => "DAC OUT1",
            InvertingInput::Dac2 => "DAC OUT2",
            InvertingInput::InputPin => "INM pin",
            InvertingInput::InputPin2 => "INM pin 2",
        }
    }

    /// The `embassy_stm32::comp::InvertingInput` variant.
    pub fn token(self) -> &'static str {
        match self {
            InvertingInput::QuarterVref => "OneQuarterVref",
            InvertingInput::HalfVref => "HalfVref",
            InvertingInput::ThreeQuarterVref => "ThreeQuarterVref",
            InvertingInput::Vref => "Vref",
            InvertingInput::Dac1 => "Dac1",
            InvertingInput::Dac2 => "Dac2",
            InvertingInput::InputPin => "InputPin",
            InvertingInput::InputPin2 => "InputPin2",
        }
    }

    /// Does this choice need a `COMP{n}_INM` pin wired on the Pins tab?
    ///
    /// Only the two pin options do. Everything else is an on-chip reference, so
    /// a comparator against VREFINT needs exactly one pin — which is the point
    /// of the internal references existing.
    pub fn needs_pin(self) -> bool {
        matches!(self, InvertingInput::InputPin | InvertingInput::InputPin2)
    }
}

/// Speed / power trade-off — CubeMX's "Speed / Power Mode".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PowerMode {
    #[default]
    HighSpeed,
    MediumSpeed,
    UltraLowPower,
}

impl PowerMode {
    pub const ALL: [PowerMode; 3] = [
        PowerMode::HighSpeed,
        PowerMode::MediumSpeed,
        PowerMode::UltraLowPower,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PowerMode::HighSpeed => "High speed / full power",
            PowerMode::MediumSpeed => "Medium speed / medium power",
            PowerMode::UltraLowPower => "Ultra-low power / very low speed",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            PowerMode::HighSpeed => "HighSpeed",
            PowerMode::MediumSpeed => "MediumSpeed",
            PowerMode::UltraLowPower => "UltraLowPower",
        }
    }
}

/// Hysteresis. The two generations do not agree on what the steps ARE, so both
/// vocabularies live here and [`Hysteresis::options`] hands out the right one.
///
/// One enum rather than two because it is one persisted field, and the tokens
/// are disjoint (`Hyst20M` can only be a G4, `Low` only a U5), so a round trip
/// through `@comp` is unambiguous either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Hysteresis {
    /// The only level both generations share.
    #[default]
    None,
    // `comp_v2` — eight steps in millivolts.
    Mv10,
    Mv20,
    Mv30,
    Mv40,
    Mv50,
    Mv60,
    Mv70,
    // `comp_u5` — three named levels the reference manual does not put a
    // voltage on.
    Low,
    Medium,
    High,
}

impl Hysteresis {
    pub const ALL: [Hysteresis; 11] = [
        Hysteresis::None,
        Hysteresis::Mv10,
        Hysteresis::Mv20,
        Hysteresis::Mv30,
        Hysteresis::Mv40,
        Hysteresis::Mv50,
        Hysteresis::Mv60,
        Hysteresis::Mv70,
        Hysteresis::Low,
        Hysteresis::Medium,
        Hysteresis::High,
    ];
    const V2: [Hysteresis; 8] = [
        Hysteresis::None,
        Hysteresis::Mv10,
        Hysteresis::Mv20,
        Hysteresis::Mv30,
        Hysteresis::Mv40,
        Hysteresis::Mv50,
        Hysteresis::Mv60,
        Hysteresis::Mv70,
    ];
    const U5: [Hysteresis; 4] = [
        Hysteresis::None,
        Hysteresis::Low,
        Hysteresis::Medium,
        Hysteresis::High,
    ];

    /// The levels this generation can express — the two sets are disjoint apart
    /// from `None`.
    pub fn options(g: Generation) -> &'static [Hysteresis] {
        match g {
            Generation::V2 => &Self::V2,
            Generation::U5 => &Self::U5,
        }
    }

    /// Can this generation express this level? A stored value from the other
    /// one cannot be generated, so the card falls back to `None`.
    pub fn fits(self, g: Generation) -> bool {
        Self::options(g).contains(&self)
    }

    pub fn label(self) -> &'static str {
        match self {
            Hysteresis::None => "None",
            Hysteresis::Mv10 => "10 mV",
            Hysteresis::Mv20 => "20 mV",
            Hysteresis::Mv30 => "30 mV",
            Hysteresis::Mv40 => "40 mV",
            Hysteresis::Mv50 => "50 mV",
            Hysteresis::Mv60 => "60 mV",
            Hysteresis::Mv70 => "70 mV",
            Hysteresis::Low => "Low",
            Hysteresis::Medium => "Medium",
            Hysteresis::High => "High",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            Hysteresis::None => "None",
            Hysteresis::Mv10 => "Hyst10M",
            Hysteresis::Mv20 => "Hyst20M",
            Hysteresis::Mv30 => "Hyst30M",
            Hysteresis::Mv40 => "Hyst40M",
            Hysteresis::Mv50 => "Hyst50M",
            Hysteresis::Mv60 => "Hyst60M",
            Hysteresis::Mv70 => "Hyst70M",
            Hysteresis::Low => "Low",
            Hysteresis::Medium => "Medium",
            Hysteresis::High => "High",
        }
    }
}

/// Output polarity — whether the comparator's result is inverted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OutputPolarity {
    #[default]
    NotInverted,
    Inverted,
}

impl OutputPolarity {
    pub const ALL: [OutputPolarity; 2] = [OutputPolarity::NotInverted, OutputPolarity::Inverted];

    pub fn label(self) -> &'static str {
        match self {
            OutputPolarity::NotInverted => "Not inverted",
            OutputPolarity::Inverted => "Inverted",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            OutputPolarity::NotInverted => "NotInverted",
            OutputPolarity::Inverted => "Inverted",
        }
    }
}

/// Timer-driven blanking, to mask the output during switching noise.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BlankingSource {
    #[default]
    None,
    Blank1,
    Blank2,
    Blank3,
}

impl BlankingSource {
    pub const ALL: [BlankingSource; 4] = [
        BlankingSource::None,
        BlankingSource::Blank1,
        BlankingSource::Blank2,
        BlankingSource::Blank3,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlankingSource::None => "None",
            BlankingSource::Blank1 => "Blanking source 1",
            BlankingSource::Blank2 => "Blanking source 2",
            BlankingSource::Blank3 => "Blanking source 3",
        }
    }

    pub fn token(self) -> &'static str {
        match self {
            BlankingSource::None => "None",
            BlankingSource::Blank1 => "Blank1",
            BlankingSource::Blank2 => "Blank2",
            BlankingSource::Blank3 => "Blank3",
        }
    }
}

// ── The numbers `comp{N}.rs` stores ─────────────────────────────────────────
//
// A comparator's GENERATED block holds plain numbers, and `get_config()` below
// it maps them back to embassy's enums. Each number is a POSITION in the lists
// above, so their order is part of every user's `comp{N}.rs` now: never
// reorder `ALL` or `options()`, only append.

impl InvertingInput {
    /// This choice's number in `comp{N}.rs`: its place in [`Self::ALL`].
    pub fn index(self) -> u8 {
        position(&Self::ALL, self)
    }
}

impl PowerMode {
    /// This mode's number in `comp{N}.rs`: its place in [`Self::ALL`].
    pub fn index(self) -> u8 {
        position(&Self::ALL, self)
    }
}

impl Hysteresis {
    /// This level's number in `comp{N}.rs` on generation `g`: its place in
    /// [`Self::options`] - 0, none, for a level `g` cannot express, which is
    /// what the card falls back to as well.
    pub fn index(self, g: Generation) -> u8 {
        position(Self::options(g), self)
    }
}

impl BlankingSource {
    /// This source's number in `comp{N}.rs`: its place in [`Self::ALL`].
    pub fn index(self) -> u8 {
        position(&Self::ALL, self)
    }
}

/// Where `v` stands in `list`, 0 when it is not there.
fn position<T: PartialEq>(list: &[T], v: T) -> u8 {
    list.iter()
        .position(|x| *x == v)
        .and_then(|i| u8::try_from(i).ok())
        .unwrap_or(0)
}

/// One comparator's settings. Present in [`Mcu::comp`] only when the user
/// switched that instance on, so the map's keys ARE the enabled instances.
///
/// [`Mcu::comp`]: crate::panels::mcu_module::mcu::Mcu
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CompConfig {
    pub power_mode: PowerMode,
    pub hysteresis: Hysteresis,
    pub output_polarity: OutputPolarity,
    pub inverting_input: InvertingInput,
    pub blanking_source: BlankingSource,
}

/// Every comparator instance the CHIP has, ascending.
///
/// Derived from the pin signals the vendor published (`COMP3_INP` on some pin
/// means the part has a COMP3), not from a per-family constant: a G431 has four
/// and a G474 seven, and the difference is exactly what the pin data records.
pub fn instances(mcu: &crate::panels::mcu_module::mcu::Mcu) -> Vec<u8> {
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    let mut out: Vec<u8> = Vec::new();
    for pin in mcu.iter_all_pins() {
        for f in &pin.available_functions {
            // A comparator input is an "additional function", so the importer
            // leaves it as the vendor's raw signal name rather than one of the
            // modelled variants.
            let PinFunction::Other(sig) = f else { continue };
            if let Some(n) = instance_of_signal(sig, "INP")
                && !out.contains(&n)
            {
                out.push(n);
            }
        }
    }
    out.sort_unstable();
    out
}

/// `("COMP5_INP", "INP") -> Some(5)`; `None` for anything else.
fn instance_of_signal(signal: &str, suffix: &str) -> Option<u8> {
    let rest = signal.strip_prefix("COMP")?;
    let (num, tail) = rest.split_at(rest.find('_')?);
    (tail == format!("_{suffix}"))
        .then(|| num.parse().ok())
        .flatten()
}

/// The pin currently CONFIGURED as `COMP{n}_{suffix}`, if any.
pub fn wired_pin(mcu: &crate::panels::mcu_module::mcu::Mcu, n: u8, suffix: &str) -> Option<String> {
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    mcu.iter_all_pins()
        .find(|p| match &p.selected_function {
            PinFunction::Other(sig) => instance_of_signal(sig, suffix) == Some(n),
            _ => false,
        })
        .map(|p| p.name.clone())
}

/// Why this family's comparators cannot be generated, or `None` when they can.
///
/// Three different answers, and the difference is what tells a reader where the
/// work would have to happen:
///
/// * **Registers, no driver.** The PAC has the comparator (`comp_v3` on L4 and
///   L5, `comp_v1` on G0, `h7_a`/`h7_b` on H7, `u0`, `u3`, `h5`) but
///   `embassy_stm32::comp` is `#[cfg(any(comp_u5, comp_v2))]`, so the module
///   does not even exist there. Writing one is an embassy change, not ours.
/// * **No registers at all.** STM32F303 / F358 list COMP1..7 as peripherals in
///   `stm32-metapac` with `registers: None`. Nothing can be written against
///   them until `stm32-data` gains the register block. (The F373 line is the
///   exception in that family: it has `comp_f3_v1`, still with no driver.)
/// * **No comparators.** Nothing to say.
///
/// Kept as prose per family rather than a generic "unsupported", because the
/// three are days, months and never apart.
pub fn unsupported_reason(family: &str) -> Option<String> {
    if codegen_supported(family) {
        return None;
    }
    let version = match family {
        "stm32l4" | "stm32l5" => Some("comp_v3"),
        "stm32g0" => Some("comp_v1"),
        "stm32h7" => Some("comp_h7_a / comp_h7_b"),
        "stm32u0" => Some("comp_u0"),
        "stm32u3" => Some("comp_u3"),
        "stm32h5" => Some("comp_h5"),
        _ => None,
    };
    Some(match (family, version) {
        (_, Some(v)) => format!(
            "The comparators are in the PAC on {family} ({v}), but embassy's driver is \
             `#[cfg(any(comp_u5, comp_v2))]` - the `comp` module does not exist for this \
             family, so there is nothing to call. Adding it is a change to embassy-stm32.",
        ),
        ("stm32f3", None) => "STM32F303 and F358 list COMP1..7 in stm32-metapac with no \
             register block at all (`registers: None`), so nothing can be generated for \
             them until the vendor data upstream gains one. Only the F373 line has \
             registers, and no driver either."
            .to_owned(),
        _ => format!(
            "embassy-stm32 implements comparators for the G4 (comp_v2) and U5/WBA \
             (comp_u5) register generations only; {family} is neither.",
        ),
    })
}

/// Can this family's comparators be generated?
///
/// A whitelist through [`Generation::of`], not "any STM32": offering a card
/// whose settings reach no generated file is how a configuration silently does
/// nothing.
pub fn codegen_supported(family: &str) -> bool {
    Generation::of(family).is_some()
}

/// The settings of every enabled comparator, keyed by instance.
pub type CompSettings = BTreeMap<u8, CompConfig>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_signal_names_its_instance() {
        assert_eq!(instance_of_signal("COMP5_INP", "INP"), Some(5));
        assert_eq!(instance_of_signal("COMP5_INM", "INM"), Some(5));
        // The suffix has to match exactly, or INP would answer for INM.
        assert_eq!(instance_of_signal("COMP5_INP", "INM"), None);
        // …and neither the output nor a look-alike counts.
        assert_eq!(instance_of_signal("COMP5_OUT", "INP"), None);
        assert_eq!(instance_of_signal("COMP5_INP2", "INP"), None);
        assert_eq!(instance_of_signal("TIM5_CH1", "INP"), None);
        assert_eq!(instance_of_signal("COMP_INP", "INP"), None);
    }

    /// Only the two pin choices need a second pin; the internal references are
    /// the reason a comparator can run on one.
    #[test]
    fn only_the_pin_references_need_a_pin() {
        for i in InvertingInput::ALL {
            assert_eq!(
                i.needs_pin(),
                matches!(i, InvertingInput::InputPin | InvertingInput::InputPin2),
                "{i:?}"
            );
        }
    }

    /// A greyed card must say WHERE the work is, and the three answers are not
    /// the same kind of missing.
    #[test]
    fn each_unsupported_family_says_what_is_actually_missing() {
        assert!(unsupported_reason("stm32g4").is_none());
        assert!(unsupported_reason("stm32u5").is_none());

        // Registers but no driver - name the version, so it can be looked up.
        for (family, version) in [
            ("stm32l4", "comp_v3"),
            ("stm32l5", "comp_v3"),
            ("stm32g0", "comp_v1"),
            ("stm32u0", "comp_u0"),
        ] {
            let why = unsupported_reason(family).expect("greyed means it must explain itself");
            assert!(why.contains(version), "{family}: {why}");
            assert!(why.contains("embassy"), "{family}: {why}");
        }

        // F3 is a different kind of missing, and must not read as "not written
        // yet" - the data itself is absent.
        let f3 = unsupported_reason("stm32f3").expect("greyed");
        assert!(f3.contains("registers: None"), "{f3}");
        assert!(f3.contains("stm32-metapac"), "{f3}");

        // Anything else gets the plain statement, still naming both generations.
        let other = unsupported_reason("stm32f1").expect("greyed");
        assert!(
            other.contains("comp_v2") && other.contains("comp_u5"),
            "{other}"
        );
    }

    /// The two generations are different peripherals, and the card must not
    /// offer one's vocabulary on the other: naming `InputPin2` or `Hyst20M` in
    /// a U5 project does not compile.
    #[test]
    fn each_generation_offers_only_what_it_can_express() {
        assert_eq!(Generation::of("stm32g4"), Some(Generation::V2));
        assert_eq!(Generation::of("stm32u5"), Some(Generation::U5));
        assert_eq!(Generation::of("stm32wba"), Some(Generation::U5));
        assert_eq!(Generation::of("stm32f3"), None);

        let v2 = Hysteresis::options(Generation::V2);
        let u5 = Hysteresis::options(Generation::U5);
        assert_eq!(v2.len(), 8);
        assert_eq!(u5.len(), 4);
        // Disjoint but for `None`, which is why one enum can hold both.
        for h in v2 {
            assert!(*h == Hysteresis::None || !u5.contains(h), "{h:?}");
        }
        assert!(Hysteresis::Mv20.fits(Generation::V2));
        assert!(!Hysteresis::Mv20.fits(Generation::U5));
        assert!(Hysteresis::High.fits(Generation::U5));
        assert!(!Hysteresis::High.fits(Generation::V2));
        assert!(Hysteresis::None.fits(Generation::V2) && Hysteresis::None.fits(Generation::U5));

        // The second INM pin is a G4 thing only.
        assert!(InvertingInput::options(Generation::V2).contains(&InvertingInput::InputPin2));
        assert!(!InvertingInput::options(Generation::U5).contains(&InvertingInput::InputPin2));
        assert!(InvertingInput::options(Generation::U5).contains(&InvertingInput::InputPin));

        // embassy writes the power mode on U5 and drops it on G4.
        assert!(Generation::U5.has_power_mode());
        assert!(!Generation::V2.has_power_mode());
    }

    /// Every token has to be a real `embassy_stm32::comp` variant — a typo here
    /// is a compile error in the USER's project, not in ours.
    #[test]
    fn the_tokens_are_the_embassy_variants() {
        assert_eq!(InvertingInput::HalfVref.token(), "HalfVref");
        assert_eq!(Hysteresis::Mv70.token(), "Hyst70M");
        assert_eq!(PowerMode::UltraLowPower.token(), "UltraLowPower");
        assert_eq!(BlankingSource::Blank3.token(), "Blank3");
        assert_eq!(OutputPolarity::Inverted.token(), "Inverted");
        // No variant may share a token with another, or two settings would
        // generate the same code.
        assert_eq!(Hysteresis::Medium.token(), "Medium");
        let toks: Vec<&str> = InvertingInput::ALL.iter().map(|v| v.token()).collect();
        let mut uniq = toks.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(toks.len(), uniq.len());
    }
}
