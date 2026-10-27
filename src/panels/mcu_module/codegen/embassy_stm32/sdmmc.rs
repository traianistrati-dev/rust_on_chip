//! SD card: `sdmmc{N}.rs` over SDMMC / SDIO.

use super::*;

/// What this chip calls its SD-card block: 0 is the un-numbered `SDIO`.
pub(super) fn sdmmc_peri(unit: u8) -> String {
    if unit == 0 {
        "SDIO".to_owned()
    } else {
        format!("SDMMC{unit}")
    }
}

/// The pads of one SD-card controller.
pub struct SdmmcWiring {
    pub ck: String,
    pub cmd: String,
    /// Lane number → pin, ascending.
    pub lanes: BTreeMap<u8, String>,
}

/// The bus width the wired lanes describe, or `None` when they describe none.
///
/// The width is not a setting: the controller has a 1-, a 4- and an 8-line
/// constructor, and which one applies is exactly which lanes are wired. Two
/// lanes is not a bus, it is a half-finished one.
pub(super) fn sd_bus_width(lanes: &BTreeMap<u8, String>) -> Option<u8> {
    let have: Vec<u8> = lanes.keys().copied().collect();
    match have.len() {
        1 if have == [0] => Some(1),
        4 if have == [0, 1, 2, 3] => Some(4),
        8 if have == [0, 1, 2, 3, 4, 5, 6, 7] => Some(8),
        _ => None,
    }
}

/// The SD-card controllers with a clock, a command line and at least one lane.
pub(super) fn sdmmc_wires(pins: &[&Pin]) -> Vec<(u8, SdmmcWiring)> {
    let mut by_unit: BTreeMap<u8, (Option<String>, Option<String>, BTreeMap<u8, String>)> =
        BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        match p.selected_function {
            PinFunction::SdmmcCk { unit } => {
                by_unit
                    .entry(unit)
                    .or_default()
                    .0
                    .get_or_insert_with(|| p.gpio().to_owned());
            }
            PinFunction::SdmmcCmd { unit } => {
                by_unit
                    .entry(unit)
                    .or_default()
                    .1
                    .get_or_insert_with(|| p.gpio().to_owned());
            }
            PinFunction::SdmmcD { unit, lane } => {
                by_unit
                    .entry(unit)
                    .or_default()
                    .2
                    .entry(lane)
                    .or_insert_with(|| p.gpio().to_owned());
            }
            _ => continue,
        }
    }
    by_unit
        .into_iter()
        .filter_map(|(unit, (ck, cmd, lanes))| {
            (!lanes.is_empty()).then_some(())?;
            Some((
                unit,
                SdmmcWiring {
                    ck: ck?,
                    cmd: cmd?,
                    lanes,
                },
            ))
        })
        .collect()
}

/// The SD-card config file, rendered by [`tmpl::fill`].
pub(super) const SDMMC_TMPL: &str = include_str!("../templates/embassy_stm32/sdmmc.rs.tmpl");

/// `src/pins/configs/sd{n}.rs`: embassy's `Sdmmc`, in whichever of its two
/// shapes this chip's controller takes.
pub fn sdmmc_config_file(
    n: u8,
    cfg: &SdmmcModuleConfig,
    w: &SdmmcWiring,
    width: u8,
    kind: stm32_pin_data::SdmmcKind,
) -> String {
    let peri = sdmmc_peri(n);
    let v1 = kind == stm32_pin_data::SdmmcKind::V1;
    let lanes: Vec<u8> = w.lanes.keys().copied().collect();

    let mut sd_items: Vec<String> = vec![
        "CkPin".into(),
        "CmdPin".into(),
        "Config".into(),
        "Instance".into(),
        "InterruptHandler".into(),
        "Sdmmc".into(),
    ];
    if v1 {
        sd_items.push("SdmmcDma".into());
    }
    for l in &lanes {
        sd_items.push(format!("D{l}Pin"));
    }
    sd_items.sort();

    let mut params = String::new();
    if v1 {
        params.push_str("    dma: Peri<'d, D>,\n");
    }
    params.push_str(&format!(
        "    // One binding value covers the peripheral{} — `init` takes a single value.\n    irqs: impl Binding<<peripherals::{peri} as Instance>::Interrupt, InterruptHandler<peripherals::{peri}>>\n{}        + 'd,\n",
        if v1 { " AND the DMA channel" } else { "" },
        if v1 {
            "        + Binding<D::Interrupt, DmaInterruptHandler<D>>\n"
        } else {
            ""
        },
    ));
    params.push_str(&format!(
        "    clk: Peri<'d, impl CkPin<peripherals::{peri}>>,\n    cmd: Peri<'d, impl CmdPin<peripherals::{peri}>>,\n"
    ));
    for l in &lanes {
        params.push_str(&format!(
            "    d{l}: Peri<'d, impl D{l}Pin<peripherals::{peri}>>,\n"
        ));
    }

    let lane_args: String = lanes.iter().map(|l| format!(", d{l}")).collect();
    let ctor = format!(
        "Sdmmc::new_{width}bit(sdmmc, {}irqs, clk, cmd{lane_args}, get_config())",
        if v1 { "dma, " } else { "" }
    );

    tmpl::fill(
        SDMMC_TMPL,
        &[
            ("{TIMEOUT}", &cfg.data_timeout.to_string()),
            ("{PERI}", &peri),
            ("{WIDTH}", &width.to_string()),
            ("{VER}", if v1 { "older (v1)" } else { "newer (v2)" }),
            (
                "{DMA_NOTE}",
                if v1 {
                    " It is fed a DMA channel, and binds
// that channel's interrupt as well as its own."
                } else {
                    " It has its own DMA controller
// inside, so it takes no channel."
                },
            ),
            (
                "{DMA_USE}",
                if v1 {
                    "use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
"
                } else {
                    ""
                },
            ),
            ("{ITEMS}", &sd_items.join(", ")),
            (
                "{GENERIC}",
                &(if v1 {
                    format!(", D: SdmmcDma<peripherals::{peri}>")
                } else {
                    String::new()
                }),
            ),
            ("{PARAMS}", &params),
            ("{CTOR}", &ctor),
            (
                "{HANDLE}",
                &format!("_sd{n}{}", label_sfx(&cfg.custom_label)),
            ),
        ],
    )
}

#[cfg(test)]
mod sdmmc_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// A controller with `lanes` data lines wired.
    fn card(unit: u8, lanes: u8) -> Vec<Pin> {
        let mut v = vec![
            mk("PC12", PinFunction::SdmmcCk { unit }),
            mk("PD2", PinFunction::SdmmcCmd { unit }),
        ];
        for lane in 0..lanes {
            v.push(mk(
                &format!("PC{}", 8 + lane),
                PinFunction::SdmmcD { unit, lane },
            ));
        }
        v
    }

    fn run(pins: &[Pin], ip: Option<&str>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32f7",
            ChipData {
                i2s: true,
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci3_v2_1_Cube"),
                sdmmc_ip: ip,
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

    /// The older controller is fed a DMA channel and binds ITS interrupt as
    /// well as the peripheral's — one `Irqs` value has to satisfy both.
    #[test]
    fn the_older_controller_takes_a_dma_channel() {
        let out = run(&card(1, 4), Some("sdmmc_v1_3_Cube"));
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "sd1.rs");
        assert!(body.contains("D: SdmmcDma<peripherals::SDMMC1>"), "{body}");
        assert!(body.contains("dma: Peri<'d, D>,"), "{body}");
        assert!(
            body.contains("+ Binding<D::Interrupt, DmaInterruptHandler<D>>"),
            "{body}"
        );
        assert!(
            body.contains(
                "Sdmmc::new_4bit(sdmmc, dma, irqs, clk, cmd, d0, d1, d2, d3, get_config())"
            ),
            "{body}"
        );
    }

    /// The newer one has its own DMA inside: no channel, no channel binding.
    /// Same function NAME, different argument list — which is why the IP
    /// version is not optional.
    #[test]
    fn the_newer_controller_takes_none() {
        let out = run(&card(1, 4), Some("sdmmc2_v2_1_U5_Cube"));
        let (_, body) = &out.config_files[0];
        assert!(!body.contains("SdmmcDma"), "{body}");
        assert!(!body.contains("dma: Peri"), "{body}");
        assert!(!body.contains("DmaInterruptHandler"), "{body}");
        assert!(
            body.contains("Sdmmc::new_4bit(sdmmc, irqs, clk, cmd, d0, d1, d2, d3, get_config())"),
            "{body}"
        );
        // …and the peripheral's own interrupt still joins the shared `Irqs`.
        assert!(
            out.init_calls
                .contains("pins::configs::sd1::init(p.SDMMC1, Irqs,"),
            "{}",
            out.init_calls
        );
    }

    /// Width is the WIRING, and each width is its own constructor.
    #[test]
    fn the_wired_lanes_pick_the_constructor() {
        for (lanes, ctor) in [(1u8, "new_1bit"), (4, "new_4bit"), (8, "new_8bit")] {
            let out = run(&card(1, lanes), Some("sdmmc2_v2_1_U5_Cube"));
            let (_, body) = &out.config_files[0];
            assert!(body.contains(&format!("Sdmmc::{ctor}(")), "{lanes}: {body}");
            assert!(
                body.contains(&format!("as a {lanes}-bit SD/eMMC host")),
                "{body}"
            );
        }
    }

    /// Two lanes is not a bus. Say which widths exist rather than emitting a
    /// call the card could never answer.
    #[test]
    fn a_width_the_controller_has_no_constructor_for_is_refused() {
        let out = run(&card(1, 2), Some("sdmmc2_v2_1_U5_Cube"));
        assert!(
            out.init_calls
                .contains("SDMMC1 is NOT initialised: 2 data line(s) are wired"),
            "{}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// With no captured IP version there is no way to know WHICH argument list
    /// applies, so nothing is emitted and the reason names the fix.
    #[test]
    fn a_chip_with_no_ip_version_generates_nothing() {
        let out = run(&card(1, 4), None);
        assert!(
            out.init_calls.contains("carries no SDMMC IP version"),
            "{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("Re-import the chip"),
            "the message has to name the fix: {}",
            out.init_calls
        );
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
    }

    /// The un-numbered `SDIO` of the older families is unit 0, and the
    /// peripheral singleton goes by that name.
    #[test]
    fn the_unnumbered_sdio_keeps_its_name() {
        let out = run(&card(0, 1), Some("sdmmc_v1_2_Cube"));
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "sd0.rs");
        assert!(body.contains("peripherals::SDIO"), "{body}");
        assert!(!body.contains("SDMMC0"), "{body}");
        assert!(
            out.init_calls.contains("init(p.SDIO,"),
            "{}",
            out.init_calls
        );
    }
}
