//! I2S: `i2s{N}.rs` on an SPI block.

use super::*;

/// The pads of one I2S: the three it needs, and the master clock it may have.
pub struct I2sWiring {
    pub sd: String,
    pub ws: String,
    pub ck: String,
    pub mck: Option<String>,
}

/// The I2S instances whose three required pads are all wired.
///
/// A half-wired I2S generates NOTHING rather than a call with a missing
/// argument: embassy takes CK, WS and SD together or not at all.
pub(super) fn i2s_wires(pins: &[&Pin]) -> Vec<(u8, I2sWiring)> {
    let mut by_inst: BTreeMap<
        u8,
        (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ),
    > = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let e = match p.selected_function {
            PinFunction::I2sCk(n) => &mut by_inst.entry(n).or_default().0,
            PinFunction::I2sWs(n) => &mut by_inst.entry(n).or_default().1,
            PinFunction::I2sSd(n) => &mut by_inst.entry(n).or_default().2,
            PinFunction::I2sMck(n) => &mut by_inst.entry(n).or_default().3,
            _ => continue,
        };
        e.get_or_insert_with(|| p.gpio().to_owned());
    }
    by_inst
        .into_iter()
        .filter_map(|(n, (ck, ws, sd, mck))| {
            Some((
                n,
                I2sWiring {
                    sd: sd?,
                    ws: ws?,
                    ck: ck?,
                    mck,
                },
            ))
        })
        .collect()
}

/// `src/pins/configs/i2s{N}.rs`: embassy's `I2S` over the SPI{N} block.
pub(super) const ASYNC_I2S_TMPL: &str = include_str!("../templates/embassy_stm32/i2s.rs.tmpl");

/// Render [`ASYNC_I2S_TMPL`] for the I2S on SPI`n`.
pub fn i2s_config_file(n: u8, cfg: &I2sModuleConfig, w: &I2sWiring, handle: &str) -> String {
    let tx = cfg.direction.is_tx();
    let has_mck = w.mck.is_some();
    // embassy has a constructor per (direction × master clock): the pad is a
    // parameter, not an option, so its absence changes the name.
    let ctor = match (tx, has_mck) {
        (true, true) => "new_txonly",
        (true, false) => "new_txonly_nomck",
        (false, true) => "new_rxonly",
        (false, false) => "new_rxonly_nomck",
    };
    // Built line by line: a `\`-continued literal keeps its source indentation
    // once rustfmt joins it, and that indentation lands in the user's file.
    let usage = if tx {
        let mut u = String::from("//\n");
        u.push_str(&format!("//     let mut w = {handle}.writer();\n"));
        u.push_str("//     w.write(&samples).await.ok();");
        u
    } else {
        let mut u = String::from("//\n");
        u.push_str(&format!("//     let mut r = {handle}.reader();\n"));
        u.push_str("//     r.read(&mut samples).await.ok();");
        u
    };
    tmpl::render(
        ASYNC_I2S_TMPL,
        &[
            ("{RATE}", &cfg.sample_rate_hz.to_string()),
            ("{BUF}", &cfg.buffer_len.to_string()),
            ("{DMA_TRAIT}", if tx { "TxDma" } else { "RxDma" }),
            ("{MCK_USE}", if has_mck { ", MckPin" } else { "" }),
            (
                "{MCK_PARAM}",
                if has_mck {
                    "    mck: Peri<'d, impl MckPin<peripherals::SPI{N}>>,\n"
                } else {
                    ""
                },
            ),
            ("{MCK_ARG}", if has_mck { "        mck,\n" } else { "" }),
            ("{MODE}", cfg.mode.embassy()),
            ("{STANDARD}", cfg.standard.embassy()),
            ("{FORMAT}", cfg.format.embassy()),
            ("{POLARITY}", cfg.clock_polarity.embassy()),
            ("{MASTER_CLOCK}", if has_mck { "true" } else { "false" }),
            ("{WORD}", cfg.format.word()),
            ("{CTOR}", ctor),
            ("{DIRWORD}", if tx { "transmitting" } else { "receiving" }),
            ("{USAGE}", &usage),
            ("{HANDLE}", handle),
            ("{N}", &n.to_string()),
        ],
    )
}

#[cfg(test)]
mod i2s_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{I2sDirection, I2sFormat, I2sStandard};
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    fn run(pins: &[Pin], i2s: BTreeMap<u8, I2sModuleConfig>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32g4",
            ChipData {
                i2s: true,
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci3_v2_1_Cube"),
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
                raw: &Default::default(),
            },
            &refs,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &i2s,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// `run`'s wiring on `mode`'s runtime, for a chip whose `i2s` module
    /// embassy does or does not compile.
    fn run_chip(mode: PeriphMode, chip_i2s: bool, pins: &[Pin]) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        peripherals(
            mode,
            &Default::default(),
            "stm32f3",
            ChipData {
                i2s: chip_i2s,
                dma: None,
                irq_vectors: &[],
                usart_ip: None,
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
                raw: &Default::default(),
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
    }

    /// A chip embassy compiles no `i2s` module for (an STM32F303's CubeMX data
    /// lists the I2S pads anyway): `i2s{n}.rs` would be E0432, so neither
    /// runtime writes it - main.rs says why, the pads stay raw, no DMA channel is
    /// taken, and it is no runtime gap, because the other runtime lacks it too.
    #[test]
    fn a_chip_without_an_i2s_module_generates_none() {
        let said = |out: &AsyncPeriphs| {
            out.init_calls
                .lines()
                .map(|l| l.trim().trim_start_matches("//").trim())
                .collect::<Vec<_>>()
                .join(" ")
        };
        for mode in [PeriphMode::Async, PeriphMode::Blocking] {
            let out = run_chip(mode, false, &three(2));
            let text = said(&out);
            assert!(
                text.contains(&format!("I2S2 is NOT initialised: {I2S_CHIP_GAP}")),
                "{mode:?}: {text}"
            );
            // The chip's reason, not the Blocking runtime's.
            assert!(!text.contains("The Async runtime generates it"), "{text}");
            assert!(out.config_files.is_empty(), "{:?}", out.config_files);
            assert!(out.consumed_pins.is_empty(), "{:?}", out.consumed_pins);
            assert!(out.dma_irqs.is_empty(), "{}", out.dma_irqs);
            assert!(out.runtime_gaps.is_empty(), "{:?}", out.runtime_gaps);
        }
        // The same wiring where embassy has the module: built on Async.
        let out = run_chip(PeriphMode::Async, true, &three(2));
        assert_eq!(out.config_files[0].0, "i2s2.rs");
        // ...and "Generate driver" decides nothing on the chip without it.
        let generated = |i2s| {
            driver_generated(
                PeriphMode::Async,
                "stm32f3",
                ModuleKind::GenericInterfaceI2s,
                2,
                None,
                i2s,
            )
        };
        assert!(generated(true));
        assert!(!generated(false));
    }

    /// A channel pinned on an I2S the chip cannot build is not reserved: on an
    /// F303, I2S2 TX's DMA1_CH5 is USART1 RX's only channel, and the panel no
    /// longer draws the picker that could clear the pin.
    #[test]
    fn a_refused_i2s_keeps_no_dma_channel_from_a_built_bus() {
        use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
        use crate::panels::mcu_module::mcu_def::DmaDef;
        use crate::panels::mcu_module::modules::{UsartMode, UsartModuleConfig};
        let chan = |peri: &str, irq: &str| DmaChannel {
            peri: peri.into(),
            irq: irq.into(),
        };
        let f303 = DmaDef {
            mux: false,
            channels: vec![
                chan("DMA1_CH4", "DMA1_CHANNEL4"),
                chan("DMA1_CH5", "DMA1_CHANNEL5"),
            ],
            requests: vec![
                ("USART1_TX".into(), vec!["DMA1_CH4".into()]),
                ("USART1_RX".into(), vec!["DMA1_CH5".into()]),
                ("SPI2_RX".into(), vec!["DMA1_CH4".into()]),
                ("SPI2_TX".into(), vec!["DMA1_CH5".into()]),
            ],
        };
        let mut pins = three(2);
        pins.push(mk("PA9", PinFunction::UsartTx(1)));
        pins.push(mk("PA10", PinFunction::UsartRx(1)));
        let refs: Vec<&Pin> = pins.iter().collect();
        let usart: BTreeMap<u8, UsartModuleConfig> = [(
            1u8,
            UsartModuleConfig {
                mode: UsartMode::Dma,
                ..UsartModuleConfig::new(1)
            },
        )]
        .into();
        let mut i2s_cfg = I2sModuleConfig::new(2);
        i2s_cfg.dma_tx = "DMA1_CH5".into();
        let i2s: BTreeMap<u8, I2sModuleConfig> = [(2u8, i2s_cfg)].into();
        let out = peripherals(
            PeriphMode::Async,
            &Default::default(),
            "stm32f3",
            ChipData {
                i2s: false,
                dma: Some(&f303),
                irq_vectors: &[],
                usart_ip: None,
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
                raw: &Default::default(),
            },
            &refs,
            &usart,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &i2s,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        assert!(
            out.init_calls.contains("p.DMA1_CH4, p.DMA1_CH5"),
            "{}",
            out.init_calls
        );
        assert!(!has_dma_todo(&out.init_calls), "{}", out.init_calls);
        assert!(
            out.init_calls.contains("I2S2 is NOT initialised"),
            "{}",
            out.init_calls
        );
    }

    fn three(n: u8) -> Vec<Pin> {
        vec![
            mk("PB13", PinFunction::I2sCk(n)),
            mk("PB12", PinFunction::I2sWs(n)),
            mk("PB15", PinFunction::I2sSd(n)),
        ]
    }

    /// The three required pads make an I2S; the master clock is a fourth
    /// parameter, and its absence changes the CONSTRUCTOR, not an argument.
    #[test]
    fn the_master_clock_pad_picks_the_constructor() {
        let out = run(&three(2), Default::default());
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "i2s2.rs");
        assert!(body.contains("I2S::new_txonly_nomck("), "{body}");
        assert!(body.contains("config.master_clock = false;"), "{body}");
        assert!(!body.contains("MckPin"), "{body}");

        let mut pins = three(2);
        pins.push(mk("PC6", PinFunction::I2sMck(2)));
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("I2S::new_txonly("), "{body}");
        assert!(body.contains("config.master_clock = true;"), "{body}");
        assert!(
            body.contains("mck: Peri<'d, impl MckPin<peripherals::SPI2>>"),
            "{body}"
        );
    }

    /// The peripheral is the SPI block, and every setting reaches the config.
    #[test]
    fn the_settings_reach_the_generated_file() {
        let mut cfg = I2sModuleConfig::new(2);
        cfg.sample_rate_hz = 44_100;
        cfg.direction = I2sDirection::Receive;
        cfg.standard = I2sStandard::MsbFirst;
        cfg.format = I2sFormat::Data24Channel32;
        cfg.buffer_len = 512;
        let out = run(&three(2), [(2u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];

        assert!(
            body.contains("pub const SAMPLE_RATE_HZ: u32 = 44100;"),
            "{body}"
        );
        assert!(body.contains("pub const BUF_LEN: usize = 512;"), "{body}");
        assert!(body.contains("Standard::MsbFirst"), "{body}");
        assert!(body.contains("Format::Data24Channel32"), "{body}");
        // Receiving: the other constructor, and the RX half of the DMA.
        assert!(body.contains("I2S::new_rxonly_nomck("), "{body}");
        assert!(body.contains("D: RxDma<peripherals::SPI2>"), "{body}");
        // A 24-bit frame still moves as `u16` halves — `spi::Word` has no u32.
        assert!(body.contains("I2S<'d, u16>"), "{body}");
        // The peripheral handed in is the SPI block itself.
        assert!(
            out.init_calls.contains("pins::configs::i2s2::init(p.SPI2,"),
            "{}",
            out.init_calls
        );
    }

    /// I2S2 and SPI2 are one block. Both wired is one block described twice —
    /// SPI keeps it and the audio side says why, instead of emitting a second
    /// `p.SPI2` that cannot compile.
    #[test]
    fn an_i2s_on_a_claimed_spi_block_is_refused_with_a_reason() {
        let mut pins = three(2);
        pins.push(mk("PA5", PinFunction::SpiSck(2)));
        pins.push(mk("PA7", PinFunction::SpiMosi(2)));
        let out = run(&pins, Default::default());
        assert!(
            out.init_calls
                .contains("I2S2 is NOT initialised: it runs on SPI2"),
            "{}",
            out.init_calls
        );
        assert!(!out.init_calls.contains("i2s2::init"), "{}", out.init_calls);
        assert!(
            !out.config_files.iter().any(|(n, _)| n == "i2s2.rs"),
            "no file for a block it does not own"
        );
    }

    /// Half a bus is not a bus: CK, WS and SD go in together or not at all.
    #[test]
    fn a_half_wired_i2s_generates_nothing() {
        let pins = vec![
            mk("PB13", PinFunction::I2sCk(2)),
            mk("PB12", PinFunction::I2sWs(2)),
        ];
        let out = run(&pins, Default::default());
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
        assert!(!out.init_calls.contains("i2s"), "{}", out.init_calls);
    }
}
