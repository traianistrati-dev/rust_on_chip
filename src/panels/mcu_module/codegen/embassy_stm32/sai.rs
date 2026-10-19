//! SAI: `sai{N}.rs`, both sub-blocks of one unit.

use super::*;

/// The pads of ONE SAI sub-block.
pub struct SaiBlockWiring {
    pub sck: String,
    pub sd: String,
    pub fs: String,
    pub mclk: Option<String>,
}

/// The SAI units with at least one fully wired sub-block, as
/// `(unit, [(sub-block, pads)])` with 1 = A and 2 = B.
///
/// "Fully wired" is SCK + SD + FS: a sub-block missing one of the three
/// generates nothing rather than a call with an argument short. The
/// synchronous mode, where a sub-block borrows the other one's clocks and needs
/// only SD, is a wiring rule of its own and is not emitted yet.
pub(super) fn sai_wires(pins: &[&Pin]) -> Vec<(u8, Vec<(u8, SaiBlockWiring)>)> {
    type Pads = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let mut by_unit: BTreeMap<u8, BTreeMap<u8, Pads>> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let (unit, block, slot) = match p.selected_function {
            PinFunction::SaiSck { sai, block } => (sai, block, 0),
            PinFunction::SaiSd { sai, block } => (sai, block, 1),
            PinFunction::SaiFs { sai, block } => (sai, block, 2),
            PinFunction::SaiMclk { sai, block } => (sai, block, 3),
            _ => continue,
        };
        let e = by_unit.entry(unit).or_default().entry(block).or_default();
        let field = match slot {
            0 => &mut e.0,
            1 => &mut e.1,
            2 => &mut e.2,
            _ => &mut e.3,
        };
        field.get_or_insert_with(|| p.gpio().to_owned());
    }
    by_unit
        .into_iter()
        .filter_map(|(unit, blocks)| {
            let wired: Vec<(u8, SaiBlockWiring)> = blocks
                .into_iter()
                .filter_map(|(b, (sck, sd, fs, mclk))| {
                    Some((
                        b,
                        SaiBlockWiring {
                            sck: sck?,
                            sd: sd?,
                            fs: fs?,
                            mclk,
                        },
                    ))
                })
                .collect();
            (!wired.is_empty()).then_some((unit, wired))
        })
        .collect()
}

/// The SAI config file, rendered by [`tmpl::fill`].
pub(super) const SAI_TMPL: &str = include_str!("../templates/embassy_stm32/sai.rs.tmpl");

/// `src/pins/configs/sai{n}.rs`: embassy's `Sai`, one driver per sub-block.
pub fn sai_config_file(n: u8, cfg: &SaiModuleConfig, blocks: &[(u8, SaiBlockWiring)]) -> String {
    let letter = |b: u8| if b == 1 { "a" } else { "b" };
    let upper = |b: u8| if b == 1 { "A" } else { "B" };

    let mut consts = String::new();
    let mut configs = String::new();
    let mut params = String::new();
    let mut generics = Vec::new();
    let mut bounds = Vec::new();
    let mut statics = String::new();
    let mut ctors = Vec::new();
    let mut rets = Vec::new();
    let mut any_mclk = false;

    for (b, w) in blocks {
        let (l, u) = (letter(*b), upper(*b));
        let blk = cfg.block_of(*b);
        let word = blk.data_size.word();
        consts.push_str(&format!(
            "pub const {u}_SLOTS: u8 = {}; // slots per frame\npub const {u}_FRAME_BITS: u16 = {}; // frame length, in bits\npub const {u}_BUF_LEN: usize = {}; // ring buffer, in {word} samples\n",
            blk.slot_count, blk.frame_length, blk.buffer_len
        ));
        configs.push_str(&format!(
            "fn config_{l}() -> Config {{\n    let mut config = Config::default();\n    config.mode = Mode::{};\n    config.tx_rx = TxRx::{};\n    config.data_size = DataSize::{};\n    config.stereo_mono = StereoMono::{};\n    config.slot_count = word::U4({u}_SLOTS);\n    config.frame_length = {u}_FRAME_BITS;\n    config\n}}\n\n",
            blk.mode.embassy(),
            blk.tx_rx.embassy(),
            blk.data_size.embassy(),
            blk.stereo_mono.embassy(),
        ));
        params.push_str(&format!(
            "    {l}_sck: Peri<'d, impl SckPin<peripherals::SAI{n}, {u}>>,\n    {l}_sd: Peri<'d, impl SdPin<peripherals::SAI{n}, {u}>>,\n    {l}_fs: Peri<'d, impl FsPin<peripherals::SAI{n}, {u}>>,\n"
        ));
        if w.mclk.is_some() {
            any_mclk = true;
            params.push_str(&format!(
                "    {l}_mclk: Peri<'d, impl MclkPin<peripherals::SAI{n}, {u}>>,\n"
            ));
        }
        params.push_str(&format!("    {l}_dma: Peri<'d, D{u}>,\n"));
        generics.push(format!("D{u}: Dma<peripherals::SAI{n}, {u}>"));
        bounds.push(format!(
            "Binding<D{u}::Interrupt, DmaInterruptHandler<D{u}>>"
        ));
        statics.push_str(&format!(
            "    static {u}_BUF: StaticCell<[{word}; {u}_BUF_LEN]> = StaticCell::new();\n"
        ));
        let ctor = if w.mclk.is_some() {
            "new_asynchronous_with_mclk"
        } else {
            "new_asynchronous"
        };
        let mclk_arg = if w.mclk.is_some() {
            format!(", {l}_mclk")
        } else {
            String::new()
        };
        ctors.push(format!(
            "Sai::{ctor}(sub_{l}, {l}_sck, {l}_sd, {l}_fs{mclk_arg}, {l}_dma, {u}_BUF.init([0; {u}_BUF_LEN]), irqs, config_{l}())"
        ));
        rets.push(format!("Sai<'d, peripherals::SAI{n}, {word}>"));
    }

    // The split yields BOTH sub-blocks whatever is wired; the unused one is
    // dropped on the spot, which is what disables it.
    let has_a = blocks.iter().any(|(b, _)| *b == 1);
    let has_b = blocks.iter().any(|(b, _)| *b == 2);
    let split = format!(
        "    let ({}, {}) = split_subblocks(sai);\n",
        if has_a { "sub_a" } else { "_sub_a" },
        if has_b { "sub_b" } else { "_sub_b" },
    );

    let (ret, body) = if ctors.len() == 1 {
        (rets[0].clone(), format!("    {}\n", ctors[0]))
    } else {
        (
            format!("({})", rets.join(", ")),
            format!("    (\n        {},\n    )\n", ctors.join(",\n        ")),
        )
    };

    let mut sai_items: Vec<&str> = vec![
        "Config",
        "DataSize",
        "Dma",
        "FsPin",
        "Mode",
        "Sai",
        "SckPin",
        "SdPin",
        "StereoMono",
        "TxRx",
        "split_subblocks",
        "word",
    ];
    if any_mclk {
        sai_items.push("MclkPin");
    }
    if has_a {
        sai_items.push("A");
    }
    if has_b {
        sai_items.push("B");
    }
    sai_items.sort_unstable();

    tmpl::fill(
        SAI_TMPL,
        &[
            ("{CONSTS}", &consts),
            ("{N}", &n.to_string()),
            ("{ITEMS}", &sai_items.join(", ")),
            ("{CONFIGS}", &configs),
            ("{GENERICS}", &generics.join(", ")),
            ("{PARAMS}", &params),
            ("{BOUNDS}", &(bounds.join(" + ")).to_string()),
            ("{RET}", &ret),
            ("{SPLIT}", &split),
            ("{STATICS}", &statics),
            ("{BODY}", &body),
        ],
    )
}

#[cfg(test)]
mod sai_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{SaiBlockConfig, SaiDataSize, SaiTxRx};
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// The three pads a sub-block needs, plus its optional master clock.
    fn block(sai: u8, b: u8, mclk: bool) -> Vec<Pin> {
        let mut v = vec![
            mk(&format!("P{b}0"), PinFunction::SaiSck { sai, block: b }),
            mk(&format!("P{b}1"), PinFunction::SaiSd { sai, block: b }),
            mk(&format!("P{b}2"), PinFunction::SaiFs { sai, block: b }),
        ];
        if mclk {
            v.push(mk(
                &format!("P{b}3"),
                PinFunction::SaiMclk { sai, block: b },
            ));
        }
        v
    }

    fn run(pins: &[Pin], sai: BTreeMap<u8, SaiModuleConfig>) -> AsyncPeriphs {
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
            &Default::default(),
            &sai,
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// Two sub-blocks are ONE module and ONE file: `split_subblocks` happens
    /// once, so the unit cannot be two modules. They keep separate directions,
    /// separate word widths and separate DMA channels.
    #[test]
    fn two_sub_blocks_are_one_unit_split_once() {
        let mut pins = block(1, 1, false);
        pins.extend(block(1, 2, false));
        let mut cfg = SaiModuleConfig::new(1);
        cfg.set_block(
            1,
            SaiBlockConfig {
                tx_rx: SaiTxRx::Transmitter,
                data_size: SaiDataSize::Data24,
                ..Default::default()
            },
        );
        cfg.set_block(
            2,
            SaiBlockConfig {
                tx_rx: SaiTxRx::Receiver,
                data_size: SaiDataSize::Data16,
                ..Default::default()
            },
        );
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());

        assert_eq!(out.config_files.len(), 1, "one file per UNIT");
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "sai1.rs");
        assert!(
            body.contains("let (sub_a, sub_b) = split_subblocks(sai);"),
            "{body}"
        );
        assert!(body.contains("config.tx_rx = TxRx::Transmitter;"), "{body}");
        assert!(body.contains("config.tx_rx = TxRx::Receiver;"), "{body}");
        // A 24-bit stream rides in u32 words, a 16-bit one in u16 — unlike I2S,
        // the SAI ring buffer is a DMA buffer, so it really does widen.
        assert!(
            body.contains("-> (Sai<'d, peripherals::SAI1, u32>, Sai<'d, peripherals::SAI1, u16>)"),
            "{body}"
        );
        assert!(
            body.contains("static A_BUF: StaticCell<[u32; A_BUF_LEN]>"),
            "{body}"
        );
        assert!(
            body.contains("static B_BUF: StaticCell<[u16; B_BUF_LEN]>"),
            "{body}"
        );
        // Two handles out of one call.
        assert!(
            out.init_calls
                .contains("let (mut _sai1a, mut _sai1b) = pins::configs::sai1::init(p.SAI1,"),
            "{}",
            out.init_calls
        );
    }

    /// One sub-block wired: one driver, one handle, and the other half of the
    /// split is dropped on the spot — which is what leaves it disabled.
    #[test]
    fn one_sub_block_drops_the_other_half_of_the_split() {
        let out = run(&block(1, 1, false), Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("let (sub_a, _sub_b) = split_subblocks(sai);"),
            "{body}"
        );
        assert!(
            body.contains("-> Sai<'d, peripherals::SAI1, u16>"),
            "{body}"
        );
        assert!(!body.contains("B_BUF"), "{body}");
        // …and the import names only what the file uses.
        assert!(body.contains("sai::{A, Config"), "{body}");
        assert!(!body.contains(", B,"), "{body}");
        assert!(
            out.init_calls
                .contains("let mut _sai1a = pins::configs::sai1::init(p.SAI1,"),
            "{}",
            out.init_calls
        );
    }

    /// The master clock pad is a parameter, so its presence changes the
    /// CONSTRUCTOR — the same rule as I2S.
    #[test]
    fn the_master_clock_pad_picks_the_constructor() {
        let out = run(&block(1, 1, true), Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("Sai::new_asynchronous_with_mclk(sub_a,"),
            "{body}"
        );
        assert!(body.contains("a_mclk: Peri<'d, impl MclkPin<"), "{body}");

        let out = run(&block(1, 1, false), Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("Sai::new_asynchronous(sub_a,"), "{body}");
        assert!(!body.contains("MclkPin"), "{body}");
    }

    /// A sub-block missing one of its three pads generates nothing: embassy
    /// takes SCK, SD and FS together or not at all.
    #[test]
    fn a_half_wired_sub_block_generates_nothing() {
        let pins = vec![
            mk("PA1", PinFunction::SaiSck { sai: 1, block: 1 }),
            mk("PA2", PinFunction::SaiSd { sai: 1, block: 1 }),
        ];
        let out = run(&pins, Default::default());
        assert!(out.config_files.is_empty(), "{:?}", out.config_files);
        assert!(!out.init_calls.contains("sai1"), "{}", out.init_calls);
    }
}
