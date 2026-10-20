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

/// `get_config()`, below the block: the numbers mapped back. One per
/// generation, so a Configuration-tab edit never changes it.
pub(super) const COMP_GET_CONFIG_TMPL: &str =
    include_str!("../templates/embassy_stm32/comp_get_config.rs.tmpl");

/// Render [`COMP_TMPL`] for one instance.
///
/// The block holds plain numbers - places in the `comparator` lists, see
/// [`comparator::Hysteresis::index`] - and [`comp_get_config`] below it maps
/// them back to embassy's enums. That map depends on the generation alone, so
/// an edit on the Configuration tab rewrites the block and nothing else.
///
/// `POWER_MODE` is emitted only where embassy WRITES it. `Config::power_mode`
/// exists on both generations, but `configure_raw` computes it under
/// `#[cfg(comp_u5)]` alone — a number in a G4 project would be a setting that
/// changes nothing, which is worse than leaving it out.
pub(super) fn comp_config_file(
    n: u8,
    cfg: &comparator::CompConfig,
    g: comparator::Generation,
) -> String {
    use comparator::{BlankingSource, Hysteresis, InvertingInput, OutputPolarity, PowerMode};
    let (ctor, inm_param, inm_arg) = if cfg.inverting_input.needs_pin() {
        (
            "new_with_input_minus_pin",
            format!("\n    inm: Peri<'d, impl InputMinusPin<peripherals::COMP{n}> + Pin>,"),
            " inm,".to_owned(),
        )
    } else {
        ("new", String::new(), String::new())
    };
    let power_const = if g.has_power_mode() {
        format!(
            "// Power mode: {}.\npub const POWER_MODE: u8 = {};\n",
            legend(PowerMode::ALL.iter().map(|p| p.label())),
            cfg.power_mode.index()
        )
    } else {
        String::new()
    };
    let inverted = cfg.output_polarity == OutputPolarity::Inverted;
    tmpl::render(
        COMP_TMPL,
        &[
            // A level from the other generation is not one of `g`'s options:
            // `index` gives 0, none - the belt for a `@comp` line hand-edited
            // or carried over from another chip. The card offers only `g`'s.
            (
                "{HYST_LEGEND}",
                &legend(Hysteresis::options(g).iter().map(|h| h.label())),
            ),
            ("{HYST_N}", &cfg.hysteresis.index(g).to_string()),
            ("{INVERTED}", if inverted { "true" } else { "false" }),
            (
                "{INM_LEGEND}",
                &legend(InvertingInput::options(g).iter().map(|v| v.label())),
            ),
            ("{INM_N}", &cfg.inverting_input.index().to_string()),
            (
                "{BLANK_LEGEND}",
                &legend(BlankingSource::ALL.iter().map(|b| b.label())),
            ),
            ("{BLANK_N}", &cfg.blanking_source.index().to_string()),
            ("{POWER_CONST}", &power_const),
            ("{N}", &n.to_string()),
            ("{HANDLE}", &format!("_comp{n}")),
            ("{CTOR}", ctor),
            (
                "{INM_USE}", // Only the pin form names that trait; importing it always is an
                // `unused_imports` warning in the USER's project, which is worse
                // than in ours - they cannot fix it without editing generated code.
                if cfg.inverting_input.needs_pin() {
                    "
use embassy_stm32::comp::InputMinusPin;"
                } else {
                    ""
                },
            ),
            ("{INM_PARAM}", &inm_param),
            ("{INM_ARG}", &inm_arg),
            ("{GET_CONFIG}", &comp_get_config(g)),
        ],
    )
}

/// `0 None, 1 10 mV, ...` - what each number means, for the block's comments.
fn legend<'a>(labels: impl Iterator<Item = &'a str>) -> String {
    labels
        .enumerate()
        .map(|(i, l)| format!("{i} {l}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `get_config()` for generation `g`: an arm per option `g` has, naming only
/// those - U5 has no `Hyst*M` and no `InputPin2`, V2 no `Low` - with the
/// default under `_`, which also catches a number no option has.
fn comp_get_config(g: comparator::Generation) -> String {
    use comparator::{BlankingSource, Hysteresis, InvertingInput, PowerMode};
    fn arms<T: Copy + PartialEq + Default>(
        ty: &str,
        opts: &[T],
        index: impl Fn(T) -> u8,
        token: impl Fn(T) -> &'static str,
    ) -> String {
        let mut out = String::new();
        for &o in opts.iter().filter(|o| **o != T::default()) {
            out.push_str(&format!("        {} => {ty}::{},\n", index(o), token(o)));
        }
        out.push_str(&format!("        _ => {ty}::{},\n", token(T::default())));
        out
    }
    let (power_use, power_match) = if g.has_power_mode() {
        (
            "    use embassy_stm32::comp::PowerMode;\n".to_owned(),
            format!(
                "    config.power_mode = match POWER_MODE {{\n{}    }};\n",
                arms(
                    "PowerMode",
                    &PowerMode::ALL,
                    PowerMode::index,
                    PowerMode::token
                )
            ),
        )
    } else {
        (String::new(), String::new())
    };
    tmpl::render(
        COMP_GET_CONFIG_TMPL,
        &[
            ("{POWER_USE}", &power_use),
            ("{POWER_MATCH}", &power_match),
            (
                "{HYST_ARMS}",
                &arms(
                    "Hysteresis",
                    Hysteresis::options(g),
                    |h| h.index(g),
                    Hysteresis::token,
                ),
            ),
            (
                "{INM_ARMS}",
                &arms(
                    "InvertingInput",
                    InvertingInput::options(g),
                    InvertingInput::index,
                    InvertingInput::token,
                ),
            ),
            (
                "{BLANK_ARMS}",
                &arms(
                    "BlankingSource",
                    &BlankingSource::ALL,
                    BlankingSource::index,
                    BlankingSource::token,
                ),
            ),
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

/// COMP1 as the "imported chip, comparators" row has it (G4, 30 mV,
/// 3/4 VREF): for the sync tests in `project_tree`.
#[cfg(test)]
pub fn comp_file_for_tests() -> String {
    comp_config_file(
        1,
        &comparator::CompConfig {
            hysteresis: comparator::Hysteresis::Mv30,
            inverting_input: comparator::InvertingInput::ThreeQuarterVref,
            ..comparator::CompConfig::default()
        },
        comparator::Generation::V2,
    )
}

/// `body` (COMP1 from [`comp_file_for_tests`]) as the IDE wrote it before
/// 1B.4: `get_config` and its `use` in the block, nothing of it below.
#[cfg(test)]
pub fn legacy_comp_file(body: &str) -> String {
    let end = body.find("// <<< GENERATED END >>>").unwrap();
    let tail = &body[end..];
    let fn_at = tail
        .find("/// The `Config` the numbers above describe.")
        .unwrap();
    let fn_end = fn_at + tail[fn_at..].find("\n}\n").unwrap() + 4;
    let tail = format!("{}{}", &tail[..fn_at], &tail[fn_end..]);
    let block = concat!(
        "// <<< GENERATED>>>\n",
        "// Peripheral config (from the Configuration tab) - auto-updated; edit it there.\n",
        "use embassy_stm32::comp::{BlankingSource, Config, Hysteresis, InvertingInput, OutputPolarity};\n",
        "\n",
        "fn get_config() -> Config {\n",
        "    let mut config = Config::default();\n",
        "    config.hysteresis = Hysteresis::Hyst30M;\n",
        "    config.output_polarity = OutputPolarity::NotInverted;\n",
        "    config.inverting_input = InvertingInput::ThreeQuarterVref;\n",
        "    config.blanking_source = BlankingSource::None;\n",
        "    config\n",
        "}\n",
    );
    format!("{block}{tail}")
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
            !f.contains("PowerMode") && !f.contains("POWER_MODE"),
            "inert on comp_v2:
{f}"
        );
        // Every setting reaches the block, as its number.
        for want in [
            "pub const HYSTERESIS: u8 = 7;",
            "pub const INVERTED: bool = true;",
            "pub const INVERTING_INPUT: u8 = 2;",
            "pub const BLANKING_SOURCE: u8 = 2;",
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
        assert!(f.contains("pub const INVERTING_INPUT: u8 = 6;"), "{f}");
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
        assert!(f.contains("pub const POWER_MODE: u8 = 2;"), "{f}");
        assert!(f.contains("config.power_mode = match POWER_MODE {"), "{f}");
        assert!(f.contains("    use embassy_stm32::comp::PowerMode;"), "{f}");
        assert!(f.contains("pub const HYSTERESIS: u8 = 2;"), "{f}");
        assert!(f.contains("2 => Hysteresis::Medium,"), "{f}");
        assert!(
            !f.contains("Hysteresis::Hyst") && !f.contains("InputPin2"),
            "no millivolt steps and no second INM pin on this generation:
{f}"
        );

        // A level carried over from a G4 cannot be named here; it falls back to
        // the one both generations have rather than emitting nonsense.
        let stale = CompConfig {
            hysteresis: Hysteresis::Mv40,
            ..CompConfig::default()
        };
        let f = comp_config_file(2, &stale, Generation::U5);
        assert!(f.contains("pub const HYSTERESIS: u8 = 0;"), "{f}");
        assert!(!f.contains("Hysteresis::Hyst40M"), "{f}");
    }

    /// Every pairing the card can make, on both generations, with the pin
    /// form and without.
    fn every_shape() -> Vec<(Generation, CompConfig)> {
        let mut out = Vec::new();
        for g in [Generation::V2, Generation::U5] {
            for inm in [InvertingInput::ThreeQuarterVref, InvertingInput::InputPin] {
                out.push((
                    g,
                    CompConfig {
                        inverting_input: inm,
                        hysteresis: *Hysteresis::options(g).last().unwrap(),
                        output_polarity: OutputPolarity::Inverted,
                        blanking_source: BlankingSource::Blank3,
                        power_mode: PowerMode::MediumSpeed,
                    },
                ));
            }
        }
        out
    }

    fn halves(f: &str) -> (&str, &str) {
        let end = f.find("// <<< GENERATED END >>>").expect("the block");
        (&f[..end], &f[end..])
    }

    /// The block is numbers with fixed names and nothing else - no `use`, no
    /// `fn`: the rule every config file keeps, and the reason `get_config`
    /// left it.
    #[test]
    fn the_comp_block_holds_only_numbers() {
        for (g, cfg) in every_shape() {
            let f = comp_config_file(1, &cfg, g);
            let (block, _) = halves(&f);
            for line in block.lines().filter(|l| !l.trim().is_empty()) {
                let t = line.trim();
                if t.starts_with("//") {
                    continue;
                }
                let ok = t.starts_with("pub const ")
                    && (t.contains(": u8 = ") || t.contains(": bool = "))
                    && t.ends_with(';');
                assert!(ok, "{g:?}: not a plain number:\n{line}\n\n{f}");
            }
        }
    }

    /// Every setting the card has, changed at once, leaves the editable half
    /// byte for byte as it was - so editing the tab never touches the user's
    /// code, only the numbers.
    #[test]
    fn a_settings_edit_never_touches_the_editable_half() {
        for g in [Generation::V2, Generation::U5] {
            let a = CompConfig::default();
            let b = CompConfig {
                power_mode: PowerMode::UltraLowPower,
                hysteresis: Hysteresis::options(g)[1],
                output_polarity: OutputPolarity::Inverted,
                inverting_input: InvertingInput::Dac1,
                blanking_source: BlankingSource::Blank1,
            };
            let (fa, fb) = (comp_config_file(4, &a, g), comp_config_file(4, &b, g));
            assert_ne!(halves(&fa).0, halves(&fb).0, "{g:?}: the numbers moved");
            assert_eq!(halves(&fa).1, halves(&fb).1, "{g:?}");
        }
    }

    /// Each number is mapped back to the very option it stands for: the arm
    /// for `o.index()` names `o`, the default under `_`.
    #[test]
    fn each_number_maps_to_the_variant_it_means() {
        fn check<T: Copy + PartialEq + Default + std::fmt::Debug>(
            f: &str,
            ty: &str,
            opts: &[T],
            index: impl Fn(T) -> u8,
            token: impl Fn(T) -> &'static str,
        ) {
            for &o in opts {
                let arm = if o == T::default() {
                    format!("        _ => {ty}::{},\n", token(o))
                } else {
                    format!("        {} => {ty}::{},\n", index(o), token(o))
                };
                assert!(f.contains(&arm), "{o:?}: no `{}`:\n{f}", arm.trim());
            }
        }
        for g in [Generation::V2, Generation::U5] {
            let f = comp_config_file(1, &CompConfig::default(), g);
            check(
                &f,
                "Hysteresis",
                Hysteresis::options(g),
                |h| h.index(g),
                Hysteresis::token,
            );
            check(
                &f,
                "InvertingInput",
                InvertingInput::options(g),
                InvertingInput::index,
                InvertingInput::token,
            );
            check(
                &f,
                "BlankingSource",
                &BlankingSource::ALL,
                BlankingSource::index,
                BlankingSource::token,
            );
            if g.has_power_mode() {
                check(
                    &f,
                    "PowerMode",
                    &PowerMode::ALL,
                    PowerMode::index,
                    PowerMode::token,
                );
            }
            // And the numbers count the same lists the legends print.
            for (i, h) in Hysteresis::options(g).iter().enumerate() {
                assert_eq!(usize::from(h.index(g)), i);
            }
        }
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
