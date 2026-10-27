//! XSPI: `xspi{N}.rs`, the blocking external-memory interface.

use super::*;

/// Everything wired to one XSPI port.
pub struct XspiWiring {
    pub clk: String,
    /// Chip select index (1 or 2) → pin. Either one drives the device.
    pub ncs: BTreeMap<u8, String>,
    /// Strobe index → pin.
    pub dqs: BTreeMap<u8, String>,
    /// Lane → pin, ascending.
    pub io: BTreeMap<u8, String>,
}

/// The XSPI ports with a clock and at least one data line.
pub(super) fn xspi_wires(pins: &[&Pin]) -> Vec<(u8, XspiWiring)> {
    type Pads = (
        Option<String>,
        BTreeMap<u8, String>,
        BTreeMap<u8, String>,
        BTreeMap<u8, String>,
    );
    let mut by_port: BTreeMap<u8, Pads> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let name = || p.gpio().to_owned();
        match p.selected_function {
            PinFunction::XspiClk { port } => {
                by_port.entry(port).or_default().0.get_or_insert_with(name);
            }
            PinFunction::XspiNcs { port, cs } => {
                by_port
                    .entry(port)
                    .or_default()
                    .1
                    .entry(cs)
                    .or_insert_with(name);
            }
            PinFunction::XspiDqs { port, index } => {
                by_port
                    .entry(port)
                    .or_default()
                    .2
                    .entry(index)
                    .or_insert_with(name);
            }
            PinFunction::XspiIo { port, lane } => {
                by_port
                    .entry(port)
                    .or_default()
                    .3
                    .entry(lane)
                    .or_insert_with(name);
            }
            _ => continue,
        }
    }
    by_port
        .into_iter()
        .filter_map(|(port, (clk, ncs, dqs, io))| {
            (!io.is_empty()).then_some(())?;
            Some((
                port,
                XspiWiring {
                    clk: clk?,
                    ncs,
                    dqs,
                    io,
                },
            ))
        })
        .collect()
}

/// The XSPI config file, rendered by [`tmpl::fill`].
pub(super) const XSPI_TMPL: &str = include_str!("../templates/embassy_stm32/xspi.rs.tmpl");

/// `src/pins/configs/xspi{n}.rs`: embassy's blocking `Xspi` in the module's mode.
pub fn xspi_config_file(n: u8, cfg: &XspiModuleConfig, dqs: usize) -> String {
    let lanes = cfg.mode.lanes();
    let mut params = String::new();
    let mut args = vec!["clk".to_owned()];
    let mut pin_items: Vec<String> = vec!["CLKPin".into(), "NCSEither".into()];

    params.push_str(&format!(
        "    clk: Peri<'d, impl CLKPin<peripherals::XSPI{n}>>,\n"
    ));
    for l in 0..lanes {
        params.push_str(&format!(
            "    d{l}: Peri<'d, impl D{l}Pin<peripherals::XSPI{n}>>,\n"
        ));
        args.push(format!("d{l}"));
        pin_items.push(format!("D{l}Pin"));
    }
    // `NCSEither`, not `NCSPin`: both chip selects satisfy it, and the driver
    // reads which one it got off the pin itself.
    params.push_str(&format!(
        "    ncs: Peri<'d, impl NCSEither<peripherals::XSPI{n}>>,\n"
    ));
    args.push("ncs".to_owned());
    for i in 0..dqs {
        params.push_str(&format!(
            "    dqs{i}: Peri<'d, impl DQS{i}Pin<peripherals::XSPI{n}>>,\n"
        ));
        args.push(format!("dqs{i}"));
        pin_items.push(format!("DQS{i}Pin"));
    }
    pin_items.sort();

    let ctor = format!(
        "new_blocking_{}{}",
        cfg.mode.embassy(),
        match dqs {
            2 => "_dqs_dual",
            1 => "_dqs",
            _ => "",
        }
    );

    tmpl::fill(
        XSPI_TMPL,
        &[
            ("{PRESCALER}", &cfg.prescaler.to_string()),
            ("{MODE}", &cfg.mode.label().to_string()),
            ("{N}", &n.to_string()),
            ("{PINS}", &pin_items.join(", ")),
            ("{SIZE}", &cfg.size_embassy().to_string()),
            ("{MTYPE}", &cfg.memory_type.embassy().to_string()),
            ("{MODE_SHORT}", &cfg.mode.label().to_lowercase()),
            ("{PARAMS}", &params),
            ("{CTOR}", &ctor),
            ("{ARGS}", &args.join(", ")),
            (
                "{HANDLE}",
                &format!("_xspi{n}{}", label_sfx(&cfg.custom_label)),
            ),
        ],
    )
}

#[cfg(test)]
mod xspi_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{XspiMemoryType, XspiMode};
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// A device on `port`: `lanes` data lines, chip select `cs`, and `dqs`
    /// strobes.
    fn dev(port: u8, lanes: u8, cs: u8, dqs: u8) -> Vec<Pin> {
        let mut v = vec![
            mk("PA1", PinFunction::XspiClk { port }),
            mk("PA2", PinFunction::XspiNcs { port, cs }),
        ];
        for lane in 0..lanes {
            v.push(mk(&format!("PB{lane}"), PinFunction::XspiIo { port, lane }));
        }
        for index in 0..dqs {
            v.push(mk(
                &format!("PC{index}"),
                PinFunction::XspiDqs { port, index },
            ));
        }
        v
    }

    fn run(pins: &[Pin], cfg: Option<(u8, XspiModuleConfig)>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        let map: BTreeMap<u8, XspiModuleConfig> = cfg.into_iter().collect();
        async_peripherals(
            "stm32h7rs",
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
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &map,
            &Default::default(),
        )
    }

    fn cfg(mode: XspiMode) -> (u8, XspiModuleConfig) {
        let mut c = XspiModuleConfig::new(1);
        c.mode = mode;
        (1, c)
    }

    /// The mode names the constructor, all the way up to sixteen lines.
    #[test]
    fn the_mode_picks_the_constructor() {
        for (mode, ctor, lanes) in [
            (XspiMode::Single, "new_blocking_singlespi", 2u8),
            (XspiMode::Dual, "new_blocking_dualspi", 2),
            (XspiMode::Quad, "new_blocking_quadspi", 4),
            (XspiMode::DualQuad, "new_blocking_dualquadspi", 8),
            (XspiMode::Octal, "new_blocking_xspi", 8),
            (XspiMode::Hexa, "new_blocking_xspi_hexa", 16),
        ] {
            let out = run(&dev(1, lanes, 1, 0), Some(cfg(mode)));
            let (name, body) = &out.config_files[0];
            assert_eq!(name, "xspi1.rs");
            assert!(
                body.contains(&format!("Xspi::{ctor}(xspi, clk,")),
                "{mode:?}: {body}"
            );
            assert!(
                body.contains(&format!("d{}: Peri", lanes - 1)),
                "{mode:?}: {body}"
            );
        }
    }

    /// The strobes pick the SUFFIX, and only the wide modes have one at all.
    #[test]
    fn the_strobes_pick_the_suffix() {
        let out = run(&dev(1, 8, 1, 1), Some(cfg(XspiMode::Octal)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("Xspi::new_blocking_xspi_dqs("), "{body}");
        assert!(body.contains("dqs0: Peri<'d, impl DQS0Pin<"), "{body}");

        // Two strobes are the hexadeca-only dual-strobe call.
        let out = run(&dev(1, 16, 1, 2), Some(cfg(XspiMode::Hexa)));
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("Xspi::new_blocking_xspi_hexa_dqs_dual("),
            "{body}"
        );
        assert!(body.contains("dqs1: Peri<'d, impl DQS1Pin<"), "{body}");

        // A narrow mode has no strobe variant: the pad is left out entirely.
        let out = run(&dev(1, 4, 1, 1), Some(cfg(XspiMode::Quad)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("Xspi::new_blocking_quadspi("), "{body}");
        assert!(!body.contains("DQS"), "{body}");
    }

    /// Either chip select drives the device — the bound is `NCSEither`, and the
    /// driver reads which one it got off the pin.
    #[test]
    fn either_chip_select_will_do() {
        for cs in [1u8, 2] {
            let out = run(&dev(1, 4, cs, 0), Some(cfg(XspiMode::Quad)));
            let (_, body) = &out.config_files[0];
            assert!(
                body.contains("ncs: Peri<'d, impl NCSEither<peripherals::XSPI1>>"),
                "cs{cs}: {body}"
            );
        }
        // …but SOME chip select is required.
        let pins: Vec<Pin> = dev(1, 4, 1, 0)
            .into_iter()
            .filter(|p| !matches!(p.selected_function, PinFunction::XspiNcs { .. }))
            .collect();
        let out = run(&pins, Some(cfg(XspiMode::Quad)));
        assert!(
            out.init_calls.contains("no chip select is wired"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// A mode the pads cannot carry is refused, with both halves named.
    #[test]
    fn a_mode_the_wiring_cannot_carry_is_refused() {
        let out = run(&dev(1, 8, 1, 0), Some(cfg(XspiMode::Hexa)));
        assert!(
            out.init_calls
                .contains("the module is set to Hexadeca (16 lines) and that needs"),
            "{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("but 8 are wired"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// The device settings reach the file, including the two memory types the
    /// XSPI has that the OCTOSPI does not.
    #[test]
    fn the_device_settings_reach_the_file() {
        let mut c = XspiModuleConfig::new(2);
        c.mode = XspiMode::Quad;
        c.memory_type = XspiMemoryType::ApMemory16Bits;
        c.device_size = 17;
        c.prescaler = 5;
        let out = run(&dev(2, 4, 1, 0), Some((2, c)));
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "xspi2.rs");
        assert!(body.contains("peripherals::XSPI2"), "{body}");
        // embassy spells it `APMemory16Bits`, not `ApMemory16Bits`.
        assert!(body.contains("MemoryType::APMemory16Bits"), "{body}");
        assert!(body.contains("MemorySize::_128MiB"), "{body}");
        assert!(body.contains("pub const PRESCALER: u8 = 5;"), "{body}");
    }
}
