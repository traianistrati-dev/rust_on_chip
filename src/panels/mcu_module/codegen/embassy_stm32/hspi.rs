//! HSPI: `hspi{N}.rs`, the blocking 16-lane memory interface.

use super::*;

/// Everything wired to one HSPI controller.
pub struct HspiWiring {
    pub clk: String,
    /// The single chip select.
    pub ncs: Option<String>,
    /// Strobe index → pin. Only DQS0 has a constructor that takes it.
    pub dqs: BTreeMap<u8, String>,
    /// Lane → pin, ascending.
    pub io: BTreeMap<u8, String>,
}

/// The HSPI controllers with a clock and at least one data line.
pub(super) fn hspi_wires(pins: &[&Pin]) -> Vec<(u8, HspiWiring)> {
    type Pads = (
        Option<String>,
        Option<String>,
        BTreeMap<u8, String>,
        BTreeMap<u8, String>,
    );
    let mut by_unit: BTreeMap<u8, Pads> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let name = || p.gpio().to_owned();
        match p.selected_function {
            PinFunction::HspiClk { unit } => {
                by_unit.entry(unit).or_default().0.get_or_insert_with(name);
            }
            PinFunction::HspiNcs { unit } => {
                by_unit.entry(unit).or_default().1.get_or_insert_with(name);
            }
            PinFunction::HspiDqs { unit, index } => {
                by_unit
                    .entry(unit)
                    .or_default()
                    .2
                    .entry(index)
                    .or_insert_with(name);
            }
            PinFunction::HspiIo { unit, lane } => {
                by_unit
                    .entry(unit)
                    .or_default()
                    .3
                    .entry(lane)
                    .or_insert_with(name);
            }
            _ => continue,
        }
    }
    by_unit
        .into_iter()
        .filter_map(|(unit, (clk, ncs, dqs, io))| {
            (!io.is_empty()).then_some(())?;
            Some((
                unit,
                HspiWiring {
                    clk: clk?,
                    ncs,
                    dqs,
                    io,
                },
            ))
        })
        .collect()
}

/// The HSPI config file, rendered by [`tmpl::fill`].
pub(super) const HSPI_TMPL: &str = include_str!("../templates/embassy_stm32/hspi.rs.tmpl");

/// `src/pins/configs/hspi{n}.rs`: embassy's blocking `Hspi` in the module's mode.
pub fn hspi_config_file(n: u8, cfg: &HspiModuleConfig) -> String {
    let lanes = cfg.mode.lanes();
    let octal = cfg.mode == crate::panels::mcu_module::modules::HspiMode::Octal;
    let mut params = format!("    sck: Peri<'d, impl SckPin<peripherals::HSPI{n}>>,\n");
    let mut args = vec!["sck".to_owned()];
    // The HSPI names its clock `Sck` and its chip select `NSS` — the OCTOSPI's
    // `CLKPin`/`NCSPin` spelling does not carry over.
    let mut pin_items: Vec<String> = vec!["SckPin".into(), "NSSPin".into()];

    for l in 0..lanes {
        params.push_str(&format!(
            "    d{l}: Peri<'d, impl D{l}Pin<peripherals::HSPI{n}>>,\n"
        ));
        args.push(format!("d{l}"));
        pin_items.push(format!("D{l}Pin"));
    }
    params.push_str(&format!(
        "    nss: Peri<'d, impl NSSPin<peripherals::HSPI{n}>>,\n"
    ));
    args.push("nss".to_owned());
    if octal {
        params.push_str(&format!(
            "    dqs0: Peri<'d, impl DQS0Pin<peripherals::HSPI{n}>>,\n"
        ));
        args.push("dqs0".to_owned());
        pin_items.push("DQS0Pin".into());
    }
    pin_items.sort();

    // Built line by line on purpose: a `\`-continued literal is joined by
    // rustfmt with the SOURCE indentation, which would drop the `//` off the
    // second line of a generated comment.
    let handle = format!("_hspi{n}{}", label_sfx(&cfg.custom_label));
    let mut usage = String::new();
    usage.push_str(&format!("// ── Using HSPI{n} ──\n"));
    usage.push_str("// A command is a `TransferConfig`, the same shape as the OCTOSPI's:\n");
    usage.push_str("//\n");
    usage.push_str("//     use embassy_stm32::hspi::TransferConfig;\n");
    usage.push_str("//\n");
    usage.push_str("//     let mut id = [0u8; 3];\n");
    usage.push_str(&format!(
        "//     {handle}.blocking_read(&mut id, TransferConfig {{\n"
    ));
    usage.push_str("//         instruction: Some(0x9F), // read JEDEC id\n");
    usage.push_str("//         ..Default::default()\n");
    usage.push_str("//     }).ok();\n");

    tmpl::fill(
        HSPI_TMPL,
        &[
            ("{PRESCALER}", &cfg.prescaler.to_string()),
            ("{MODE}", &cfg.mode.label().to_string()),
            ("{N}", &n.to_string()),
            ("{PINS}", &pin_items.join(", ")),
            ("{SIZE}", &cfg.size_embassy().to_string()),
            ("{MTYPE}", &cfg.memory_type.embassy().to_string()),
            ("{MODE_SHORT}", &cfg.mode.label().to_lowercase()),
            ("{PARAMS}", &params),
            ("{CTOR}", &cfg.mode.embassy().to_string()),
            ("{ARGS}", &args.join(", ")),
            ("{USAGE}", &usage),
        ],
    )
}

#[cfg(test)]
mod hspi_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{HspiMode, OspiMemoryType};
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// A device on `unit`: `lanes` data lines, and `dqs` strobes.
    fn dev(unit: u8, lanes: u8, ncs: bool, dqs: u8) -> Vec<Pin> {
        let mut v = vec![mk("PA1", PinFunction::HspiClk { unit })];
        if ncs {
            v.push(mk("PA2", PinFunction::HspiNcs { unit }));
        }
        for lane in 0..lanes {
            v.push(mk(&format!("PB{lane}"), PinFunction::HspiIo { unit, lane }));
        }
        for index in 0..dqs {
            v.push(mk(
                &format!("PC{index}"),
                PinFunction::HspiDqs { unit, index },
            ));
        }
        v
    }

    fn run(pins: &[Pin], cfg: Option<(u8, HspiModuleConfig)>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        let map: BTreeMap<u8, HspiModuleConfig> = cfg.into_iter().collect();
        async_peripherals(
            "stm32u5",
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
            &map,
        )
    }

    fn cfg(mode: HspiMode) -> (u8, HspiModuleConfig) {
        let mut c = HspiModuleConfig::new(1);
        c.mode = mode;
        (1, c)
    }

    /// Two constructors, and that is the whole driver.
    #[test]
    fn the_mode_picks_the_constructor() {
        let out = run(&dev(1, 2, true, 0), Some(cfg(HspiMode::Single)));
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "hspi1.rs");
        assert!(
            body.contains("Hspi::new_blocking_singlespi(hspi, sck,"),
            "{body}"
        );
        assert!(body.contains("d1: Peri<'d, impl D1Pin<"), "{body}");
        assert!(!body.contains("d2:"), "{body}");
        // The clock is `Sck` here and the chip select `NSS` — the OCTOSPI's
        // `CLKPin`/`NCSPin` spelling is a different peripheral's.
        assert!(body.contains("sck: Peri<'d, impl SckPin<"), "{body}");
        assert!(body.contains("nss: Peri<'d, impl NSSPin<"), "{body}");

        let out = run(&dev(1, 8, true, 1), Some(cfg(HspiMode::Octal)));
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("Hspi::new_blocking_octospi(hspi, sck,"),
            "{body}"
        );
        assert!(body.contains("d7: Peri<'d, impl D7Pin<"), "{body}");
    }

    /// The strobe is not optional in the octal call: without it there is no
    /// constructor to reach for, so nothing is generated and the reason says so.
    #[test]
    fn the_octal_call_demands_the_strobe() {
        let out = run(&dev(1, 8, true, 0), Some(cfg(HspiMode::Octal)));
        assert!(
            out.init_calls.contains("the octal call requires DQS0"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);

        // …and the single call has no strobe argument at all, wired or not.
        let out = run(&dev(1, 2, true, 1), Some(cfg(HspiMode::Single)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("new_blocking_singlespi"), "{body}");
        assert!(!body.contains("DQS"), "{body}");
    }

    /// Sixteen pads exist; eight is where embassy stops. A width in between is
    /// refused with both halves named.
    #[test]
    fn a_width_embassy_cannot_build_is_refused() {
        let out = run(&dev(1, 4, true, 1), Some(cfg(HspiMode::Octal)));
        assert!(
            out.init_calls
                .contains("the module is set to Octal (8 lines + DQS0) and that needs"),
            "{}",
            out.init_calls
        );
        assert!(
            out.init_calls
                .contains("embassy builds 2 or 8, nothing between"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// No chip select, no device.
    #[test]
    fn the_chip_select_is_required() {
        let out = run(&dev(1, 2, false, 0), Some(cfg(HspiMode::Single)));
        assert!(
            out.init_calls.contains("no chip select is wired"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// The device settings reach the file, and the pads reach the call in the
    /// order embassy declares them: clock, data, chip select, strobe.
    #[test]
    fn the_device_settings_and_the_pad_order_reach_the_file() {
        let mut c = HspiModuleConfig::new(1);
        c.mode = HspiMode::Octal;
        c.memory_type = OspiMemoryType::HyperBusMemory;
        c.device_size = 16;
        c.prescaler = 3;
        let out = run(&dev(1, 8, true, 1), Some((1, c)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("MemoryType::HyperBusMemory"), "{body}");
        assert!(body.contains("MemorySize::_64MiB"), "{body}");
        assert!(body.contains("pub const PRESCALER: u8 = 3;"), "{body}");
        assert!(
            body.contains("Hspi<'d, peripherals::HSPI1, Blocking>"),
            "{body}"
        );

        // clock, then IO0..IO7, then the chip select, then the strobe — the
        // order embassy declares the arguments in.
        let want = concat!(
            "pins::configs::hspi1::init(p.HSPI1, pa1_hspi1_clk,",
            " pb0_hspi1_io0, pb1_hspi1_io1, pb2_hspi1_io2, pb3_hspi1_io3,",
            " pb4_hspi1_io4, pb5_hspi1_io5, pb6_hspi1_io6, pb7_hspi1_io7,",
            " pa2_hspi1_ncs, pc0_hspi1_dqs0);"
        );
        assert!(out.init_calls.contains(want), "{}", out.init_calls);
    }

    /// Every generated line is a line: no `\`-continued literal has swallowed a
    /// comment marker on its way through rustfmt.
    #[test]
    fn every_generated_comment_line_is_commented() {
        let out = run(&dev(1, 8, true, 1), Some(cfg(HspiMode::Octal)));
        let (_, body) = &out.config_files[0];
        let mut in_usage = false;
        for line in body.lines() {
            if line.starts_with("// ── Using") {
                in_usage = true;
            }
            if in_usage {
                assert!(
                    line.is_empty() || line.starts_with("//"),
                    "uncommented line in the usage block: {line:?}"
                );
            }
        }
        assert!(in_usage, "no usage block at all: {body}");
    }
}
