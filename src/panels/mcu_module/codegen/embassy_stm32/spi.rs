//! SPI: `spi{N}.rs`, blocking or async-DMA, two-way or transmit-only.

use super::*;

/// SPI instances with at least SCK + MOSI wired. Returns
/// `(instance, sck, mosi, miso)` sorted by instance, `miso` absent for a
/// TRANSMIT-ONLY bus.
///
/// MISO used to be required, so wiring only SCK and MOSI generated nothing at
/// all — no code and no complaint. embassy has `new_txonly` for exactly that
/// shape, and it takes one DMA channel instead of two.
pub(super) fn spi_wires(pins: &[&Pin]) -> Vec<(u8, String, String, Option<String>)> {
    let find = |want: PinFunction| -> Option<String> {
        pins.iter()
            .find(|p| !p.reserved && p.selected_function == want)
            .map(|p| p.gpio().to_owned())
    };
    let mut instances: Vec<u8> = pins
        .iter()
        .filter_map(|p| match p.selected_function {
            PinFunction::SpiSck(n) | PinFunction::SpiMosi(n) | PinFunction::SpiMiso(n) => Some(n),
            _ => None,
        })
        .collect();
    instances.sort_unstable();
    instances.dedup();
    instances
        .into_iter()
        .filter_map(|n| {
            let sck = find(PinFunction::SpiSck(n))?;
            let mosi = find(PinFunction::SpiMosi(n))?;
            Some((n, sck, mosi, find(PinFunction::SpiMiso(n))))
        })
        .collect()
}

// ── SPI config file (blocking on both runtimes, async-DMA on Async) ───────────

/// Blocking SPI (`Spi::new_blocking`) exposed as the STANDARD blocking
/// `embedded-hal` 1.0 `SpiBus<u8>`. No DMA — compiles out of the box. The
/// Blocking runtime's only SPI, and the Async one's "Blocking" choice.
pub(super) const SPI_TMPL_BLOCKING: &str =
    include_str!("../templates/embassy_stm32/spi_blocking.rs.tmpl");

/// Async DMA SPI (`Spi::new`) exposed as `embedded-hal-async` `SpiBus<u8>`
/// (`.await`-able). Needs two DMA channels, passed by `main.rs`.
pub(super) const ASYNC_SPI_TMPL_DMA: &str =
    include_str!("../templates/embassy_stm32/spi_dma.rs.tmpl");

/// The blocking SPI template's runtime text, on the Async runtime: why a
/// blocking bus is there at all, and how to get the DMA one.
const SPI_ASYNC_INTRO: &str = " A blocking
// bus in an async project is a common, valid pattern; for `.await`-able DMA SPI
// switch this module to \"Async-DMA\" in the IDE.";
const SPI_ASYNC_USAGE: &str =
    "// Blocking init inside an async project: the handle is an `embedded-hal` 1.0
// `SpiBus` — no `.await`, the transfer busy-waits. Fine for short bursts;
// switch the module to Async-DMA when a transfer is long enough to matter.
";
/// …and on the Blocking runtime, where it is the only bus there is.
const SPI_BLOCKING_USAGE: &str =
    "// The handle is an `embedded-hal` 1.0 `SpiBus`: each transfer busy-waits until
// its last byte is clocked out.
";

/// Render the SPI config file for instance `n` on the Async runtime, picking
/// the blocking or async-DMA template from the module's [`AsyncBusMode`].
#[cfg(test)]
pub fn spi_config_file(n: u8, cfg: Option<&SpiModuleConfig>, tx_only: bool) -> String {
    spi_config_file_for(PeriphMode::Async, n, cfg, tx_only)
}

/// [`spi_config_file`] for either runtime. On Blocking the bus is always the
/// blocking one: the DMA template's `.await` has no executor to run on.
pub fn spi_config_file_for(
    runtime: PeriphMode,
    n: u8,
    cfg: Option<&SpiModuleConfig>,
    tx_only: bool,
) -> String {
    let mode = cfg.map(|c| c.mode).unwrap_or(0);
    let clk = cfg.map(|c| c.clock_hz).unwrap_or(1_000_000);
    let order = cfg.map(|c| c.bit_order).unwrap_or_default();
    // Only written when it is NOT the default: an explicit `MsbFirst` line adds
    // nothing and would move the output of every project that already exists.
    let extra = if order == SpiBitOrder::MsbFirst {
        String::new()
    } else {
        format!(
            "    config.bit_order = embassy_stm32::spi::BitOrder::{};
",
            order.embassy()
        )
    };
    let bus_mode = match runtime {
        PeriphMode::Async => cfg.map(|c| c.async_mode).unwrap_or_default(),
        PeriphMode::Blocking => AsyncBusMode::Blocking,
    };
    let (intro, usage) = match runtime {
        PeriphMode::Async => (SPI_ASYNC_INTRO, SPI_ASYNC_USAGE),
        PeriphMode::Blocking => ("", SPI_BLOCKING_USAGE),
    };
    let template = match (bus_mode, tx_only) {
        (AsyncBusMode::Blocking, false) => SPI_TMPL_BLOCKING,
        (AsyncBusMode::AsyncDma, false) => ASYNC_SPI_TMPL_DMA,
        (AsyncBusMode::Blocking, true) => SPI_TMPL_BLOCKING_TX,
        (AsyncBusMode::AsyncDma, true) => ASYNC_SPI_TMPL_DMA_TX,
    };
    let sfx = cfg
        .map(|c| sanitize_label(&c.custom_label))
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();
    tmpl::render(
        template,
        &[
            ("{HANDLE}", &format!("_spi{n}{sfx}")),
            ("{N}", &n.to_string()),
            ("{MODE}", &mode.to_string()),
            ("{EXTRA_CFG}", &extra),
            ("{CLK}", &clk.to_string()),
            ("{INTRO}", intro),
            ("{USAGE}", usage),
        ],
    )
}

/// `configs/spi{n}.rs` for a **transmit-only** SPI on DMA: no MISO pin, ONE
/// channel.
///
/// Returns the concrete `Spi<'d, Async>` rather than `impl SpiBus`, and that is
/// the point: `SpiBus` promises `read` and `transfer`, and on a bus built
/// without an RX channel those panic inside embassy (`rx_dma.unwrap()`).
/// Handing back a trait that cannot keep half its contract would be worse than
/// handing back a narrower type.
pub(super) const ASYNC_SPI_TMPL_DMA_TX: &str =
    include_str!("../templates/embassy_stm32/spi_dma_tx.rs.tmpl");

/// The same shape without DMA: `new_blocking_txonly`, no channels at all.
pub(super) const SPI_TMPL_BLOCKING_TX: &str =
    include_str!("../templates/embassy_stm32/spi_blocking_tx.rs.tmpl");

#[cfg(test)]
mod spi_txonly_tests {
    use super::*;
    use crate::panels::mcu_module::modules::AsyncBusMode;
    use crate::panels::mcu_module::pins::logic::pin::Pin;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    fn pin(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// SCK + MOSI with no MISO is a TRANSMITTER, not an incomplete bus. It used
    /// to generate nothing at all — no code, no complaint.
    #[test]
    fn sck_and_mosi_without_miso_is_a_transmitter() {
        let pins = [
            pin("PA5", PinFunction::SpiSck(1)),
            pin("PA7", PinFunction::SpiMosi(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        assert_eq!(spi_wires(&refs).len(), 1, "the bus is generated");
        assert!(spi_wires(&refs)[0].3.is_none(), "and it has no MISO");

        // With MISO it is the full-duplex bus it always was.
        let mut full = pins.to_vec();
        full.push(pin("PA6", PinFunction::SpiMiso(1)));
        let refs: Vec<&Pin> = full.iter().collect();
        assert_eq!(spi_wires(&refs)[0].3.as_deref(), Some("PA6"));

        // SCK alone is still nothing: a clock with no data line is not a bus.
        let only_sck = [pin("PA5", PinFunction::SpiSck(1))];
        let refs: Vec<&Pin> = only_sck.iter().collect();
        assert!(spi_wires(&refs).is_empty());
    }

    /// A transmit-only bus takes ONE channel, and its `init` is a different
    /// shape: no MISO argument, one channel, and the concrete `Spi` type
    /// instead of `impl SpiBus` — `read` on such a bus panics inside embassy.
    #[test]
    fn a_transmitter_takes_one_channel_and_a_narrower_type() {
        use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
        use crate::panels::mcu_module::mcu_def::DmaDef;
        use crate::panels::mcu_module::modules::SpiModuleConfig;

        let pins = [
            pin("PA5", PinFunction::SpiSck(1)),
            pin("PA7", PinFunction::SpiMosi(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        let chip = DmaDef {
            mux: true,
            channels: (1..=4)
                .map(|i| DmaChannel {
                    peri: format!("DMA1_CH{i}"),
                    irq: format!("DMA1_CHANNEL{i}"),
                })
                .collect(),
            requests: Vec::new(),
        };
        let spi: BTreeMap<u8, SpiModuleConfig> = [(
            1u8,
            SpiModuleConfig {
                async_mode: AsyncBusMode::AsyncDma,
                ..SpiModuleConfig::new(1)
            },
        )]
        .into_iter()
        .collect();

        let out = async_peripherals(
            "stm32g4",
            ChipData {
                i2s: true,
                dma: Some(&chip),
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
            &spi,
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
        );
        assert!(
            out.init_calls
                .contains("init(p.SPI1, pa5_spi1_sck, pa7_spi1_mosi, p.DMA1_CH1, Irqs)"),
            "one channel, no MISO argument:\n{}",
            out.init_calls
        );
        assert_eq!(out.dma_uses.len(), 1, "only a TX channel is taken");
        assert_eq!(out.dma_uses[0].user, "SPI1 TX");

        let f = &out.config_files[0].1;
        assert!(
            f.contains("Spi::new_txonly(spi, sck, mosi, tx_dma, irqs, get_config())"),
            "{f}"
        );
        assert!(f.contains("-> Spi<'d, Async, Master>"), "{f}");
        // The prose mentions `SpiBus` — it says how to get one back. What must
        // not appear is the bus being RETURNED as one.
        assert!(
            !f.contains("impl embedded_hal_async::spi::SpiBus"),
            "the trait promises reads this bus cannot do:\n{f}"
        );
    }

    /// The same shape without DMA takes no channel at all.
    #[test]
    fn a_blocking_transmitter_takes_no_channel() {
        use crate::panels::mcu_module::modules::SpiModuleConfig;
        let f = spi_config_file(2, Some(&SpiModuleConfig::new(2)), true);
        assert!(
            f.contains("Spi::new_blocking_txonly(spi, sck, mosi, get_config())"),
            "{f}"
        );
        assert!(f.contains("-> Spi<'d, Blocking, Master>"), "{f}");
        assert!(f.contains("blocking_write"), "{f}");
    }
}
