//! Comparators: `comp{N}.rs` and their interrupt.

use super::*;

/// Everything the comparator pass needs, as one argument.
///
/// Three values that only ever travel together — and `async_peripherals` had
/// nine parameters with them spread out, which is where a caller starts passing
/// them in the wrong order.
pub struct CompInputs<'a> {
    /// The instances the Configuration tab switched on, with their settings.
    pub settings: &'a comparator::CompSettings,
    /// Every instance the CHIP has — decides which shared vector is an
    /// instance's, see [`comp_irq`].
    pub instances: &'a [u8],
    /// `(instance, INP pin, INM pin)` for the ones whose pins are wired.
    pub pins: &'a [(u8, String, Option<String>)],
}

/// One comparator, on the Async runtime. STM32G4 only — see
/// [`crate::panels::mcu_module::comparator`] for why.
///
/// `{INM_PARAM}` / `{INM_ARG}` are empty unless the inverting input is a PIN:
/// embassy has two constructors, and the one that takes an `inm` ignores
/// `config.inverting_input` entirely. Emitting the pin argument and the config
/// field together would show a choice the driver then drops.
pub(super) const COMP_TMPL: &str = include_str!("../templates/embassy_stm32/comp.rs.tmpl");

/// Render [`COMP_TMPL`] for one instance.
///
/// `power_mode` is emitted only where embassy WRITES it. `Config::power_mode`
/// exists on both generations, but `configure_raw` computes it under
/// `#[cfg(comp_u5)]` alone — assigning it in a G4 project would put a line in
/// the user's file that changes nothing, which is worse than leaving it out.
pub(super) fn comp_config_file(n: u8, cfg: &comparator::CompConfig, g: comparator::Generation) -> String {
    let (ctor, inm_param, inm_arg) = if cfg.inverting_input.needs_pin() {
        (
            "new_with_input_minus_pin",
            format!("\n    inm: Peri<'d, impl InputMinusPin<peripherals::COMP{n}> + Pin>,"),
            " inm,".to_owned(),
        )
    } else {
        ("new", String::new(), String::new())
    };
    let (power_use, power_line) = if g.has_power_mode() {
        (
            "
use embassy_stm32::comp::PowerMode;",
            format!(
                "    config.power_mode = PowerMode::{};
",
                cfg.power_mode.token()
            ),
        )
    } else {
        ("", String::new())
    };
    // A level from the other generation cannot be named here - `Hyst20M` does
    // not exist under `comp_u5`. The card offers only the right ones; this is
    // the belt for a `@comp` line hand-edited or carried over from another chip.
    let hyst = if cfg.hysteresis.fits(g) {
        cfg.hysteresis
    } else {
        comparator::Hysteresis::None
    };
    tmpl::render(
        COMP_TMPL,
        &[
            ("{POWER_USE}", power_use),
            ("{POWER_LINE}", &power_line),
            ("{N}", &n.to_string()),
            ("{HANDLE}", &format!("_comp{n}")),
            ("{POWER}", cfg.power_mode.token()),
            ("{HYST}", hyst.token()),
            ("{POLARITY}", cfg.output_polarity.token()),
            ("{INM}", cfg.inverting_input.token()),
            ("{BLANK}", cfg.blanking_source.token()),
            ("{CTOR}", ctor),
            ("{INM_USE}", // Only the pin form names that trait; importing it always is an
            // `unused_imports` warning in the USER's project, which is worse
            // than in ours - they cannot fix it without editing generated code.
            if cfg.inverting_input.needs_pin() {
                "
use embassy_stm32::comp::InputMinusPin;"
            } else {
                ""
            }),
            ("{INM_PARAM}", &inm_param),
            ("{INM_ARG}", &inm_arg),
        ],
    )
}

/// The `bind_interrupts!` key for `COMP{n}` on this chip.
///
/// `COMP1_2_3` on a G474, but plain `COMP4` on a G431 — the family's table
/// carries both and only the instance list decides. See
/// [`nvic::vector_for_within`]. Falls back to `COMP{n}` when the chip carries
/// no vector list, which at least fails loudly rather than silently binding the
/// wrong one.
pub(super) fn comp_irq(irqs: &[String], n: u8, existing: &[u8]) -> String {
    nvic::vector_for_within(irqs, "COMP", n, existing)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("COMP{n}"))
}

#[cfg(test)]
mod comp_tests {
    use super::*;
    use crate::panels::mcu_module::comparator::{
        BlankingSource, CompConfig, Generation, Hysteresis, InvertingInput, OutputPolarity,
        PowerMode,
    };

    /// The two constructors are not interchangeable: `new_with_input_minus_pin`
    /// IGNORES `config.inverting_input`, so emitting the pin argument next to a
    /// VREF choice would show a setting the driver then drops.
    #[test]
    fn the_constructor_follows_the_inverting_input() {
        let internal = CompConfig {
            power_mode: PowerMode::UltraLowPower,
            hysteresis: Hysteresis::Mv70,
            output_polarity: OutputPolarity::Inverted,
            inverting_input: InvertingInput::ThreeQuarterVref,
            blanking_source: BlankingSource::Blank2,
        };
        let f = comp_config_file(3, &internal, Generation::V2);
        assert!(
            f.contains("Comp::new(comp, inp, irqs, get_config())"),
            "{f}"
        );
        assert!(!f.contains("inm"), "no second pin anywhere: {f}");
        // Every setting reaches the file, under embassy's own spelling.
        // On a G4 embassy never writes the power mode, so the file must not
        // pretend otherwise.
        assert!(
            !f.contains("PowerMode"),
            "inert on comp_v2:
{f}"
        );
        for want in [
            "Hysteresis::Hyst70M",
            "OutputPolarity::Inverted",
            "InvertingInput::ThreeQuarterVref",
            "BlankingSource::Blank2",
            "peripherals::COMP3",
        ] {
            assert!(f.contains(want), "{want} missing from:\n{f}");
        }

        let pin = CompConfig {
            inverting_input: InvertingInput::InputPin,
            ..CompConfig::default()
        };
        let f = comp_config_file(3, &pin, Generation::V2);
        assert!(
            f.contains("Comp::new_with_input_minus_pin(comp, inp, inm, irqs, get_config())"),
            "{f}"
        );
        assert!(
            f.contains("inm: Peri<'d, impl InputMinusPin<peripherals::COMP3> + Pin>,"),
            "{f}"
        );
        // ...and only this form imports the trait, or the user's project warns.
        assert!(f.contains("use embassy_stm32::comp::InputMinusPin;"), "{f}");
    }

    /// The U5/WBA generation is a different peripheral: named hysteresis
    /// levels, no second INM pin, and a power mode that embassy DOES write.
    #[test]
    fn the_u5_generation_emits_its_own_vocabulary() {
        let cfg = CompConfig {
            power_mode: PowerMode::UltraLowPower,
            hysteresis: Hysteresis::Medium,
            inverting_input: InvertingInput::Vref,
            ..CompConfig::default()
        };
        let f = comp_config_file(2, &cfg, Generation::U5);
        assert!(
            f.contains("config.power_mode = PowerMode::UltraLowPower;"),
            "{f}"
        );
        assert!(f.contains("use embassy_stm32::comp::PowerMode;"), "{f}");
        assert!(f.contains("Hysteresis::Medium"), "{f}");
        assert!(
            !f.contains("Hysteresis::Hyst"),
            "no millivolt steps on this generation:
{f}"
        );

        // A level carried over from a G4 cannot be named here; it falls back to
        // the one both generations have rather than emitting nonsense.
        let stale = CompConfig {
            hysteresis: Hysteresis::Mv40,
            ..CompConfig::default()
        };
        let f = comp_config_file(2, &stale, Generation::U5);
        assert!(f.contains("Hysteresis::None"), "{f}");
        assert!(!f.contains("Hysteresis::Hyst40M"), "{f}");
    }

    /// A comparator reaches `main.rs` only when the chip's family has a driver,
    /// the instance is switched on, AND its pin is wired. Each of the three is
    /// a separate silence, and none of them may half-emit.
    #[test]
    fn a_comparator_needs_a_driver_a_switch_and_a_pin() {
        use crate::panels::mcu_module::comparator::CompSettings;
        use crate::panels::mcu_module::pins::logic::pin::Pin;

        let pins: [Pin; 0] = [];
        let refs: Vec<&Pin> = pins.iter().collect();
        let mut on = CompSettings::new();
        on.insert(1, CompConfig::default());
        let wired = [(1u8, "PA1".to_owned(), None)];
        let irqs = vec!["COMP1_2_3".to_owned()];

        let go = |family: &str, set: &CompSettings, pins: &[(u8, String, Option<String>)]| {
            async_peripherals(
                family,
                ChipData {
                    dma: None,
                    irq_vectors: &irqs,
                    usart_ip: Some("sci3_v2_1_Cube"),
                    sdmmc_ip: None,
                },
                CompInputs {
                    settings: set,
                    instances: &[1, 2, 3],
                    pins,
                },
                &refs,
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                &Default::default(),
                None,
                &Default::default(),
                &Default::default(),
                &Default::default(),
            )
        };

        let out = go("stm32g4", &on, &wired);
        assert!(
            out.init_calls
                .contains("pins::configs::comp1::init(p.COMP1, p.PA1, Irqs)"),
            "{}",
            out.init_calls
        );
        assert!(
            out.dma_irqs.contains(
                "COMP1_2_3 => embassy_stm32::comp::InterruptHandler<peripherals::COMP1>;"
            ),
            "the Irqs struct exists even with no DMA at all:\n{}",
            out.dma_irqs
        );
        assert_eq!(out.config_files.len(), 1);
        assert!(
            out.consumed_pins.contains(&"PA1".to_owned()),
            "the pin is taken"
        );

        // No driver for the family.
        assert!(go("stm32f4", &on, &wired).init_calls.is_empty());
        // Switched off.
        assert!(
            go("stm32g4", &CompSettings::new(), &wired)
                .init_calls
                .is_empty()
        );
        // Switched on, but no INP pin wired.
        assert!(go("stm32g4", &on, &[]).init_calls.is_empty());
        // Wants a pin for [-] and has none.
        let mut needs_pin = CompSettings::new();
        needs_pin.insert(
            1,
            CompConfig {
                inverting_input: InvertingInput::InputPin,
                ..CompConfig::default()
            },
        );
        assert!(go("stm32g4", &needs_pin, &wired).init_calls.is_empty());
        // ...and emits once the INM pin is there too.
        let both = [(1u8, "PA1".to_owned(), Some("PA0".to_owned()))];
        assert!(
            go("stm32g4", &needs_pin, &both)
                .init_calls
                .contains("init(p.COMP1, p.PA1, p.PA0, Irqs)"),
            "{}",
            go("stm32g4", &needs_pin, &both).init_calls
        );
    }
}
