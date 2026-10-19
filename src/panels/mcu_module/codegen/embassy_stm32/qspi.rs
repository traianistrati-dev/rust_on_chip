//! QUADSPI: `qspi.rs`, one or two flash banks.

use super::*;

/// One QUADSPI bank: its chip select and its four data lines.
pub struct QspiBank {
    pub ncs: String,
    /// Lane → pin, ascending. A bank counts only with all four.
    pub io: BTreeMap<u8, String>,
}

/// Everything wired to the QUADSPI: the shared clock and the complete banks.
pub struct QspiWiring {
    pub clk: String,
    pub bank1: Option<QspiBank>,
    pub bank2: Option<QspiBank>,
}

/// The QUADSPI wiring, or `None` when the clock is missing.
///
/// The clock is shared, so without it neither bank can run and there is nothing
/// to report per bank. A bank counts only when its chip select AND all four
/// data lines are wired — three lines is not a narrower flash, it is an
/// unfinished one.
pub(super) fn qspi_wires(pins: &[&Pin]) -> Option<QspiWiring> {
    let mut clk = None;
    let mut ncs: BTreeMap<u8, String> = BTreeMap::new();
    let mut io: BTreeMap<u8, BTreeMap<u8, String>> = BTreeMap::new();
    let mut seen = false;
    for p in pins.iter().filter(|p| !p.reserved) {
        match p.selected_function {
            PinFunction::QspiClk => {
                seen = true;
                clk.get_or_insert_with(|| p.gpio().to_owned());
            }
            PinFunction::QspiNcs { bank } => {
                seen = true;
                ncs.entry(bank).or_insert_with(|| p.gpio().to_owned());
            }
            PinFunction::QspiIo { bank, lane } => {
                seen = true;
                io.entry(bank)
                    .or_default()
                    .entry(lane)
                    .or_insert_with(|| p.gpio().to_owned());
            }
            _ => continue,
        }
    }
    if !seen {
        return None;
    }
    let bank = |b: u8| -> Option<QspiBank> {
        let io = io.get(&b)?;
        if io.len() != 4 {
            return None;
        }
        Some(QspiBank {
            ncs: ncs.get(&b)?.clone(),
            io: io.clone(),
        })
    };
    Some(QspiWiring {
        clk: clk?,
        bank1: bank(1),
        bank2: bank(2),
    })
}

/// The QUADSPI config file, rendered by [`tmpl::fill`].
pub(super) const QSPI_TMPL: &str = include_str!("../templates/embassy_stm32/qspi.rs.tmpl");

/// `src/pins/configs/qspi.rs`: embassy's blocking `Qspi` over the wired bank(s).
pub fn qspi_config_file(cfg: &QspiModuleConfig, b1: bool, b2: bool, handle: &str) -> String {
    let dual = b1 && b2;
    let mut params = String::new();
    let mut args = Vec::new();
    let mut pin_items: Vec<String> = vec!["SckPin".into()];

    let mut data = |b: u8, prefix: &str| {
        for l in 0..4u8 {
            params.push_str(&format!(
                "    {prefix}d{l}: Peri<'d, impl BK{b}D{l}Pin<peripherals::QUADSPI>>,\n"
            ));
            args.push(format!("{prefix}d{l}"));
            pin_items.push(format!("BK{b}D{l}Pin"));
        }
    };
    if b1 {
        data(1, if dual { "bk1" } else { "" });
    }
    if b2 {
        data(2, if dual { "bk2" } else { "" });
    }
    params.push_str("    sck: Peri<'d, impl SckPin<peripherals::QUADSPI>>,\n");
    args.push("sck".to_owned());
    for (b, on) in [(1u8, b1), (2, b2)] {
        if on {
            let name = if dual {
                format!("bk{b}nss")
            } else {
                "nss".to_owned()
            };
            params.push_str(&format!(
                "    {name}: Peri<'d, impl BK{b}NSSPin<peripherals::QUADSPI>>,\n"
            ));
            args.push(name);
            pin_items.push(format!("BK{b}NSSPin"));
        }
    }
    pin_items.sort();

    let ctor = if dual {
        "new_blocking_dual_bank"
    } else if b1 {
        "new_blocking_bank1"
    } else {
        "new_blocking_bank2"
    };
    let shape = if dual {
        "both banks, as one 8-line dual flash"
    } else if b1 {
        "bank 1"
    } else {
        "bank 2"
    };

    tmpl::fill(
        QSPI_TMPL,
        &[
            ("{PRESCALER}", &(cfg.prescaler).to_string()),
            ("{SHAPE}", &shape),
            ("{PINS}", &pin_items.join(", ")),
            ("{SIZE}", &(cfg.memory_size_embassy()).to_string()),
            ("{ADDR}", &(cfg.address_size.embassy()).to_string()),
            ("{DUAL}", &dual.to_string()),
            ("{PARAMS}", &params),
            ("{CTOR}", &ctor),
            ("{ARGS}", &args.join(", ")),
            ("{HANDLE}", &handle),
        ],
    )
}

#[cfg(test)]
mod qspi_tests {
    use super::*;
    use crate::panels::mcu_module::modules::QspiAddressSize;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// A clock plus `banks`, each complete unless `short` names it.
    fn flash(banks: &[u8], short: Option<u8>) -> Vec<Pin> {
        let mut v = vec![mk("PB2", PinFunction::QspiClk)];
        for b in banks {
            v.push(mk(&format!("PB{b}6"), PinFunction::QspiNcs { bank: *b }));
            let lanes = if short == Some(*b) { 3 } else { 4 };
            for lane in 0..lanes {
                v.push(mk(
                    &format!("P{b}{lane}"),
                    PinFunction::QspiIo { bank: *b, lane },
                ));
            }
        }
        v
    }

    fn run(pins: &[Pin], cfg: Option<&QspiModuleConfig>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32l4",
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
            &Default::default(),
            &Default::default(),
            &Default::default(),
            cfg,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// One bank is one constructor, and the pads go in the order embassy
    /// declares: data lines, then the clock, then the chip select.
    #[test]
    fn one_bank_picks_its_own_constructor() {
        let out = run(&flash(&[1], None), None);
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "qspi.rs");
        assert!(
            body.contains("Qspi::new_blocking_bank1(qspi, d0, d1, d2, d3, sck, nss, get_config())"),
            "{body}"
        );
        assert!(body.contains("config.dual_flash = false;"), "{body}");
        assert!(!body.contains("BK2"), "{body}");

        let out = run(&flash(&[2], None), None);
        let (_, body) = &out.config_files[0];
        assert!(body.contains("Qspi::new_blocking_bank2("), "{body}");
        assert!(body.contains("BK2D0Pin"), "{body}");
        assert!(!body.contains("BK1"), "{body}");
    }

    /// Both banks is the dual-flash shape: twelve pads, one 8-line memory, and
    /// the parameters gain a bank prefix so the two sets stay apart.
    #[test]
    fn both_banks_become_one_dual_flash() {
        let out = run(&flash(&[1, 2], None), None);
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains(
                "Qspi::new_blocking_dual_bank(qspi, bk1d0, bk1d1, bk1d2, bk1d3, bk2d0, bk2d1, bk2d2, bk2d3, sck, bk1nss, bk2nss, get_config())"
            ),
            "{body}"
        );
        assert!(body.contains("config.dual_flash = true;"), "{body}");
        // Eleven pads after the peripheral itself: eight data lines, the
        // shared clock, and a chip select per bank.
        assert_eq!(
            out.init_calls.matches(", p.").count(),
            11,
            "{}",
            out.init_calls
        );
    }

    /// Three data lines is not a narrower flash, it is an unfinished one — and
    /// with no complete bank there is nothing to build.
    #[test]
    fn an_incomplete_bank_is_refused_with_a_reason() {
        let out = run(&flash(&[1], Some(1)), None);
        assert!(
            out.init_calls
                .contains("QUADSPI is NOT initialised: neither bank is complete"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// What the flash chip dictates reaches the file: its size, how many
    /// address bytes it wants, and how fast the bus may run.
    #[test]
    fn the_flash_chip_settings_reach_the_file() {
        let mut cfg = QspiModuleConfig::new(1);
        cfg.memory_size = 18; // _256MiB
        cfg.address_size = QspiAddressSize::Bits32;
        cfg.prescaler = 7;
        let out = run(&flash(&[1], None), Some(&cfg));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("MemorySize::_256MiB"), "{body}");
        // embassy's own spelling is not uniform — `_8Bit` but `_32bit`.
        assert!(body.contains("AddressSize::_32bit"), "{body}");
        assert!(body.contains("pub const PRESCALER: u8 = 7;"), "{body}");
    }

    /// No clock, no bus: the pads of a bank alone drive nothing.
    #[test]
    fn without_a_clock_nothing_is_emitted() {
        let pins: Vec<Pin> = flash(&[1], None)
            .into_iter()
            .filter(|p| p.selected_function != PinFunction::QspiClk)
            .collect();
        let out = run(&pins, None);
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
        assert!(!out.init_calls.contains("qspi"), "{}", out.init_calls);
    }
}
