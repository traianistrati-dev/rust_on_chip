//! OCTOSPI: `ospi{N}.rs`, the blocking external-memory interface.

use super::*;

/// Everything wired to one OCTOSPI port.
pub struct OspiWiring {
    pub clk: String,
    pub ncs: String,
    pub dqs: Option<String>,
    /// Lane → pin, ascending.
    pub io: BTreeMap<u8, String>,
}

/// The OCTOSPI ports with a clock, a chip select and at least one data line.
pub(super) fn ospi_wires(pins: &[&Pin]) -> Vec<(u8, OspiWiring)> {
    type Pads = (
        Option<String>,
        Option<String>,
        Option<String>,
        BTreeMap<u8, String>,
    );
    let mut by_port: BTreeMap<u8, Pads> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let name = || p.gpio().to_owned();
        match p.selected_function {
            PinFunction::OspiClk { port } => {
                by_port.entry(port).or_default().0.get_or_insert_with(name);
            }
            PinFunction::OspiNcs { port } => {
                by_port.entry(port).or_default().1.get_or_insert_with(name);
            }
            PinFunction::OspiDqs { port } => {
                by_port.entry(port).or_default().2.get_or_insert_with(name);
            }
            PinFunction::OspiIo { port, lane } => {
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
                OspiWiring {
                    clk: clk?,
                    ncs: ncs?,
                    dqs,
                    io,
                },
            ))
        })
        .collect()
}

/// The OCTOSPI config file, rendered by [`tmpl::fill`].
pub(super) const OSPI_TMPL: &str = include_str!("../templates/embassy_stm32/ospi.rs.tmpl");

/// `src/pins/configs/ospi{n}.rs`: embassy's blocking `Ospi` in the module's mode.
pub fn ospi_config_file(n: u8, cfg: &OspiModuleConfig, with_dqs: bool) -> String {
    let lanes = cfg.mode.lanes();
    let mut params = String::from("    sck: Peri<'d, impl SckPin<peripherals::OCTOSPI{N}>>,\n");
    let mut args = vec!["sck".to_owned()];
    let mut pin_items: Vec<String> = vec!["SckPin".into(), "NSSPin".into()];
    for l in 0..lanes {
        params.push_str(&format!(
            "    d{l}: Peri<'d, impl D{l}Pin<peripherals::OCTOSPI{{N}}>>,\n"
        ));
        args.push(format!("d{l}"));
        pin_items.push(format!("D{l}Pin"));
    }
    params.push_str("    nss: Peri<'d, impl NSSPin<peripherals::OCTOSPI{N}>>,\n");
    args.push("nss".to_owned());
    if with_dqs {
        params.push_str("    dqs: Peri<'d, impl DQSPin<peripherals::OCTOSPI{N}>>,\n");
        args.push("dqs".to_owned());
        pin_items.push("DQSPin".into());
    }
    pin_items.sort();

    let ctor = format!(
        "new_blocking_{}{}",
        cfg.mode.embassy(),
        if with_dqs { "_with_dqs" } else { "" }
    );

    tmpl::render(
        &tmpl::fill(
            OSPI_TMPL,
            &[
                ("{PRESCALER}", &cfg.prescaler.to_string()),
                ("{MODE}", &cfg.mode.label().to_string()),
                ("{PINS}", &pin_items.join(", ")),
                ("{SIZE}", &cfg.size_embassy().to_string()),
                ("{MTYPE}", &cfg.memory_type.embassy().to_string()),
                ("{MODE_SHORT}", &cfg.mode.label().to_lowercase()),
                ("{PARAMS}", &params),
                ("{CTOR}", &ctor),
                ("{ARGS}", &args.join(", ")),
                (
                    "{HANDLE}",
                    &format!("_ospi{n}{}", label_sfx(&cfg.custom_label)),
                ),
                ("{N}", &n.to_string()),
            ],
        ),
        &[("{N}", &n.to_string())],
    )
}

#[cfg(test)]
mod ospi_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{OspiMemoryType, OspiMode};
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// A device on `port` with `lanes` data lines, and the strobe if asked.
    fn dev(port: u8, lanes: u8, dqs: bool) -> Vec<Pin> {
        let mut v = vec![
            mk("PA1", PinFunction::OspiClk { port }),
            mk("PA2", PinFunction::OspiNcs { port }),
        ];
        for lane in 0..lanes {
            v.push(mk(&format!("PB{lane}"), PinFunction::OspiIo { port, lane }));
        }
        if dqs {
            v.push(mk("PA3", PinFunction::OspiDqs { port }));
        }
        v
    }

    fn run(pins: &[Pin], cfg: Option<(u8, OspiModuleConfig)>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        let map: BTreeMap<u8, OspiModuleConfig> = cfg.into_iter().collect();
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
            &map,
            &Default::default(),
            &Default::default(),
        )
    }

    fn cfg(mode: OspiMode) -> (u8, OspiModuleConfig) {
        let mut c = OspiModuleConfig::new(1);
        c.mode = mode;
        (1, c)
    }

    /// The mode names the constructor, and the pad count follows from it.
    #[test]
    fn the_mode_picks_the_constructor() {
        for (mode, ctor, lanes) in [
            (OspiMode::Single, "new_blocking_singlespi", 2u8),
            (OspiMode::Dual, "new_blocking_dualspi", 2),
            (OspiMode::Quad, "new_blocking_quadspi", 4),
            (OspiMode::DualQuad, "new_blocking_dualquadspi", 8),
            (OspiMode::Octal, "new_blocking_octospi", 8),
        ] {
            let out = run(&dev(1, lanes, false), Some(cfg(mode)));
            let (name, body) = &out.config_files[0];
            assert_eq!(name, "ospi1.rs");
            assert!(body.contains(&format!("Ospi::{ctor}(")), "{mode:?}: {body}");
            assert!(
                body.contains(&format!("d{}: Peri", lanes - 1)),
                "{mode:?}: {body}"
            );
        }
    }

    /// Single and dual take the SAME two pads — which is exactly why the mode
    /// is a setting and not something the wiring could have told us.
    #[test]
    fn single_and_dual_are_indistinguishable_from_the_pins() {
        let pins = dev(1, 2, false);
        let a = run(&pins, Some(cfg(OspiMode::Single)));
        let b = run(&pins, Some(cfg(OspiMode::Dual)));
        assert_eq!(a.init_calls, b.init_calls, "same pads, same call");
        assert!(a.config_files[0].1.contains("singlespi"));
        assert!(b.config_files[0].1.contains("dualspi"));
    }

    /// Only the octal mode reads the strobe. Wired anywhere else, the pad is
    /// left out rather than passed to a constructor that has no slot for it.
    #[test]
    fn the_strobe_is_octal_only() {
        let out = run(&dev(1, 8, true), Some(cfg(OspiMode::Octal)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("new_blocking_octospi_with_dqs("), "{body}");
        assert!(body.contains("dqs: Peri<'d, impl DQSPin<"), "{body}");

        let out = run(&dev(1, 4, true), Some(cfg(OspiMode::Quad)));
        let (_, body) = &out.config_files[0];
        assert!(body.contains("new_blocking_quadspi("), "{body}");
        assert!(!body.contains("DQSPin"), "{body}");
        assert_eq!(
            out.init_calls.matches(", p.").count(),
            6,
            "clock, four lines, chip select — no strobe: {}",
            out.init_calls
        );
    }

    /// A mode the pads cannot carry is refused, and the message names both
    /// halves of the disagreement.
    #[test]
    fn a_mode_the_wiring_cannot_carry_is_refused() {
        let out = run(&dev(1, 4, false), Some(cfg(OspiMode::Octal)));
        assert!(
            out.init_calls
                .contains("the module is set to Octal (8 lines) and that needs"),
            "{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("but 4 are wired"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// The port is the controller: port 2 builds OCTOSPI2, not a second copy
    /// of the first.
    #[test]
    fn the_port_names_the_controller() {
        let mut c = OspiModuleConfig::new(2);
        c.mode = OspiMode::Quad;
        c.memory_type = OspiMemoryType::HyperBusMemory;
        c.device_size = 16;
        c.prescaler = 3;
        let out = run(&dev(2, 4, false), Some((2, c)));
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "ospi2.rs");
        assert!(body.contains("peripherals::OCTOSPI2"), "{body}");
        assert!(body.contains("MemoryType::HyperBusMemory"), "{body}");
        assert!(body.contains("MemorySize::_64MiB"), "{body}");
        assert!(body.contains("pub const PRESCALER: u8 = 3;"), "{body}");
        assert!(
            out.init_calls.contains("init(p.OCTOSPI2,"),
            "{}",
            out.init_calls
        );
    }
}
