//! DAC: `dac{N}.rs`, one file per converter with its wired channels.

use super::*;

/// The DACs with at least one output pad wired, as `(dac, [(channel, pin)])`.
pub(super) fn dac_wires(pins: &[&Pin]) -> Vec<(u8, Vec<(u8, String)>)> {
    let mut by_dac: BTreeMap<u8, Vec<(u8, String)>> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        if let PinFunction::DacOut { dac, channel } = p.selected_function {
            by_dac
                .entry(dac)
                .or_default()
                .push((channel, p.gpio().to_owned()));
        }
    }
    for chans in by_dac.values_mut() {
        chans.sort_unstable();
        // One pad per channel: a second one would become a duplicate argument.
        chans.dedup_by_key(|(c, _)| *c);
    }
    by_dac.into_iter().collect()
}

/// `src/pins/configs/dac{N}.rs`: embassy's blocking DAC over DAC{N}.
pub(super) const ASYNC_DAC_TMPL: &str = include_str!("../templates/embassy_stm32/dac.rs.tmpl");

/// Render [`ASYNC_DAC_TMPL`] for DAC `n` with the channels `chans`.
pub fn dac_config_file(
    n: u8,
    cfg: &DacModuleConfig,
    chans: &[(u8, String)],
    handle: &str,
) -> String {
    let both = chans.len() == 2;
    let mut start_consts = String::new();
    let mut params = String::new();
    for (ch, _) in chans {
        start_consts.push_str(&format!(
            "pub const START_CH{ch}: u16 = {}; // 12 bit, right-aligned (0..=4095)\n",
            cfg.value_of(*ch)
        ));
        params.push_str(&format!(
            "    out{ch}: Peri<'d, impl DacPin<peripherals::DAC{n}, Ch{ch}>>,\n"
        ));
    }
    let arg_list: String = chans.iter().map(|(ch, _)| format!(", out{ch}")).collect();

    // Both pads wired is ONE `Dac` covering the block; a single pad is a
    // `DacChannel`, and the channel it stands for cannot be inferred from the
    // argument alone — hence the turbofish.
    let (ret, ctor, sets, chs) = if both {
        (
            "Dac<'d, Blocking>".to_owned(),
            format!("Dac::new_blocking(dac{arg_list})"),
            "    dac.set(DualValue::Bit12Right(START_CH1, START_CH2));\n".to_owned(),
            "Ch1, Ch2".to_owned(),
        )
    } else {
        let ch = chans.first().map(|(c, _)| *c).unwrap_or(1);
        (
            "DacChannel<'d, Blocking>".to_owned(),
            format!("DacChannel::new_blocking::<peripherals::DAC{n}, Ch{ch}>(dac{arg_list})"),
            format!("    dac.set(Value::Bit12Right(START_CH{ch}));\n"),
            format!("Ch{ch}"),
        )
    };
    let value_ty = if both { "DualValue" } else { "Value" };
    let ty = if both { "Dac" } else { "DacChannel" };
    let use_line = format!("use embassy_stm32::dac::{{{chs}, {ty}, DacPin, {value_ty}}};");

    let mut usage =
        String::from("// Write a new level at any time; the pin follows immediately.\n//\n");
    if both {
        usage.push_str(&format!(
            "//     {handle}.set(DualValue::Bit12Right(2048, 0));\n"
        ));
        usage.push_str("//\n// Or take the channels apart and drive them independently:\n//\n");
        usage.push_str(&format!("//     let (mut a, mut b) = {handle}.split();\n"));
        usage.push_str("//     a.set(Value::Bit12Right(4095));");
    } else {
        usage.push_str(&format!(
            "//     {handle}.set(Value::Bit12Right(2048)); // mid-scale\n"
        ));
        usage.push_str(&format!(
            "//     {handle}.set(Value::Bit8(255));        // or 8 bit"
        ));
    }

    tmpl::render(
        ASYNC_DAC_TMPL,
        &[
            ("{START_CONSTS}", &start_consts),
            ("{USE}", &use_line),
            ("{PARAMS}", &params),
            ("{RET}", &ret),
            ("{CTOR}", &ctor),
            ("{SETS}", &sets),
            ("{USAGE}", &usage),
            ("{N}", &n.to_string()),
        ],
    )
}

#[cfg(test)]
mod dac_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, dac: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::DacOut { dac, channel };
        p
    }

    fn run(pins: &[Pin], dac: BTreeMap<u8, DacModuleConfig>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32g4",
            ChipData {
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci3_v2_1_Cube"),
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
            },
            &refs,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &dac,
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// Both pads of one block are ONE `Dac`, set together — the module is the
    /// peripheral, exactly as it is for a timer.
    #[test]
    fn both_channels_are_one_peripheral() {
        let pins = [mk("PA4", 1, 1), mk("PA5", 1, 2)];
        let mut cfg = DacModuleConfig::new(1);
        cfg.set_value(1, 2048);
        cfg.set_value(2, 4095);
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());

        assert_eq!(out.config_files.len(), 1, "one file per block");
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "dac1.rs");
        assert!(body.contains("pub const START_CH1: u16 = 2048;"), "{body}");
        assert!(body.contains("pub const START_CH2: u16 = 4095;"), "{body}");
        assert!(
            body.contains("Dac::new_blocking(dac, out1, out2)"),
            "{body}"
        );
        assert!(
            body.contains("dac.set(DualValue::Bit12Right(START_CH1, START_CH2));"),
            "{body}"
        );
        assert!(body.contains("-> Dac<'d, Blocking>"), "{body}");
        assert!(
            out.init_calls
                .contains("let mut _dac1 = pins::configs::dac1::init(p.DAC1, p.PA4, p.PA5);"),
            "{}",
            out.init_calls
        );
    }

    /// One pad is a `DacChannel`, and WHICH channel cannot be read off the
    /// argument — so the constructor names it.
    #[test]
    fn a_single_channel_names_itself_in_the_constructor() {
        let out = run(&[mk("PA5", 1, 2)], Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("DacChannel::new_blocking::<peripherals::DAC1, Ch2>(dac, out2)"),
            "{body}"
        );
        assert!(body.contains("-> DacChannel<'d, Blocking>"), "{body}");
        assert!(
            body.contains("dac.set(Value::Bit12Right(START_CH2));"),
            "{body}"
        );
        // Only the wired channel exists: no phantom CH1 const or parameter.
        assert!(!body.contains("START_CH1"), "{body}");
        assert!(!body.contains("out1"), "{body}");
        // …and the import names exactly what the file uses.
        assert!(
            body.contains("use embassy_stm32::dac::{Ch2, DacChannel, DacPin, Value};"),
            "{body}"
        );
    }

    /// A channel nobody set starts at zero, and that is written down rather
    /// than left implicit: the pad drives the moment it is enabled.
    #[test]
    fn an_untouched_channel_starts_at_zero_explicitly() {
        let out = run(&[mk("PA4", 1, 1)], Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("pub const START_CH1: u16 = 0;"), "{body}");
        assert!(
            body.contains("dac.set(Value::Bit12Right(START_CH1));"),
            "{body}"
        );
    }

    /// The DAC needs no DMA and no interrupt — `new_blocking` writes the
    /// register — so it must not drag an `Irqs` argument along.
    #[test]
    fn the_dac_binds_no_interrupts() {
        let out = run(&[mk("PA4", 1, 1)], Default::default());
        assert!(!out.init_calls.contains("Irqs"), "{}", out.init_calls);
        let (_, body) = &out.config_files[0];
        assert!(!body.contains("Binding"), "{body}");
        assert!(!body.contains("Dma"), "{body}");
    }
}
