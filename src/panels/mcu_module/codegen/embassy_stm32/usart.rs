//! USART / UART / LPUART: the `usart{N}.rs` / `lpuart{N}.rs` config files (buffered, DMA,
//! one-way), their pads, `Irqs` lines and handles.

use super::*;

// ── Async USART config file (embassy BufferedUart → embedded-io-async) ─────────

/// `src/pins/configs/usart{N}.rs` for the async runtime: an embassy
/// `BufferedUart` exposed through the STANDARD `embedded-io-async` traits, so app
/// code written generic over `embedded_io_async::{Read, Write}` is portable. The
/// interrupt-driven driver needs an ISR binding (`bind_interrupts!`, module-local
/// `Irqs`) and `'static` TX/RX buffers (`static_cell::StaticCell`). `init` is
/// GENERIC over the pin types (`impl RxPin/TxPin`) so the file is the same for
/// every chip — only the instance number differs; the concrete pins are chosen at
/// the call site in `main.rs`. Real-compile verified on STM32F411RE.
pub(super) const ASYNC_USART_TMPL: &str =
    include_str!("../templates/embassy_stm32/usart_buffered.rs.tmpl");

/// The USART instances with at least one pad wired, sorted by instance, each
/// with whichever of its TX/RX/CTS/RTS pads are (see [`serial_wires`]).
pub(super) fn usart_wires(pins: &[&Pin]) -> Vec<SerialWire> {
    serial_wires(
        pins,
        PinFunction::UsartTx,
        PinFunction::UsartRx,
        PinFunction::UsartCts,
        PinFunction::UsartRts,
    )
}

/// [`usart_wires`] for the LPUART: the same rule over its own pin functions.
pub(super) fn lpuart_wires(pins: &[&Pin]) -> Vec<SerialWire> {
    serial_wires(
        pins,
        PinFunction::LpuartTx,
        PinFunction::LpuartRx,
        PinFunction::LpuartCts,
        PinFunction::LpuartRts,
    )
}

/// The pads one serial instance has wired. Every field is optional because the
/// direction decides which are REQUIRED (a TX-only UART needs no RX pad) and
/// flow control is opt-in.
pub(super) struct SerialWire {
    pub(super) instance: u8,
    pub(super) tx: Option<String>,
    pub(super) rx: Option<String>,
    pub(super) cts: Option<String>,
    pub(super) rts: Option<String>,
}

/// Shared body of [`usart_wires`] / [`lpuart_wires`]: every instance of one
/// serial peripheral with at least one of its pads wired - TX, RX or a flow
/// pad - and the pads it has, sorted by instance.
///
/// Nothing is filtered here. Whether an instance is built is decided in
/// [`peripherals`] by the module's direction, which says which pads are
/// REQUIRED: both for full duplex, the one pad of a half duplex, and one side
/// for a one-way UART (on DMA on Async, whatever the transport on Blocking).
pub(super) fn serial_wires(
    pins: &[&Pin],
    tx_of: fn(u8) -> PinFunction,
    rx_of: fn(u8) -> PinFunction,
    cts_of: fn(u8) -> PinFunction,
    rts_of: fn(u8) -> PinFunction,
) -> Vec<SerialWire> {
    let find = |want: PinFunction| -> Option<String> {
        pins.iter()
            .find(|p| !p.reserved && p.selected_function == want)
            .map(|p| p.gpio().to_owned())
    };
    // Instances present on the wired pins (embassy chips have USART1/2/3/6/…).
    // A flow-control pad counts too: it is the whole instance's, so an RTS pin
    // on its own still names the peripheral.
    let mut instances: Vec<u8> = pins
        .iter()
        .filter_map(|p| {
            (0u8..=9).find(|&n| {
                [tx_of(n), rx_of(n), cts_of(n), rts_of(n)].contains(&p.selected_function)
            })
        })
        .collect();
    instances.sort_unstable();
    instances.dedup();

    instances
        .into_iter()
        .map(|n| SerialWire {
            instance: n,
            tx: find(tx_of(n)),
            rx: find(rx_of(n)),
            cts: find(cts_of(n)),
            rts: find(rts_of(n)),
        })
        .collect()
}

/// The binding a serial config's `init` is assigned to.
///
/// `_serial1` for USART1, `_lpserial1` for LPUART1: the two are different
/// peripherals sharing the instance number, so a chip carrying both would
/// otherwise generate the same variable name twice.
pub fn serial_handle(peri: &str, n: u8, sfx: &str) -> String {
    let prefix = if peri.eq_ignore_ascii_case("LPUART") {
        "_lpserial"
    } else {
        "_serial"
    };
    format!("{prefix}{n}{sfx}")
}

/// The peripheral word of serial instance `n`: the loop's own (`USART`,
/// `LPUART`), except for a USART-loop instance this chip names `UART{n}`.
///
/// A USART and a UART share the pin function (`UsartTx(4)` either way), the
/// config module (`usart4.rs`) and embassy's driver - but not the singleton:
/// an STM32F103RC has `p.UART4`, and `p.USART4` is E0609. Read from the vector
/// list where the chip carries one (a G0's `USART3_4_LPUART1` keeps USART4 a
/// USART); without one, only the F1's and the F3's UART4/5 are known to be
/// UARTs - no part of either family names a USART4 or USART5 (CubeMX database,
/// 2026-10-27).
pub(super) fn serial_word(
    word: &'static str,
    family: &str,
    irqs: &[String],
    n: u8,
) -> &'static str {
    if word != "USART" {
        return word;
    }
    let uart = if irqs.is_empty() {
        matches!(family, "stm32f1" | "stm32f3") && n >= 4
    } else {
        nvic::vector_for(irqs, "USART", n).is_none() && nvic::vector_for(irqs, "UART", n).is_some()
    };
    if uart { "UART" } else { "USART" }
}

/// The `bind_interrupts!` key for `USART{n}` on this chip.
///
/// Usually `USART{n}`, but an STM32G0 routes USART3, USART4 and LPUART1 through
/// one `USART3_4_LPUART1` vector and has no `USART3` at all. Falls back to the
/// plain name when the chip carries no vector list — which is what every chip
/// imported before the list existed does, and what its family really uses.
pub(super) fn serial_irq(irqs: &[String], peri: &str, n: u8) -> String {
    nvic::vector_for(irqs, peri, n)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{peri}{n}"))
}

/// Render [`ASYNC_USART_TMPL`] for instance `n` with the Virtual Module's config
/// (baud / data bits / parity / stop bits), or embassy defaults when unset.
/// `src/pins/configs/usart{N}.rs` for the async runtime on DMA.
///
/// Returns the two HALVES rather than a `Uart`, because embassy's DMA `Uart`
/// implements `embedded_io_async::Write` but NOT `Read` — handing one back whole
/// would silently lose half the portable API. `RingBufferedUartRx` restores
/// `Read`, and is the actual reason to put a UART on DMA: it keeps receiving
/// into a circular DMA buffer between your reads, so bytes are not dropped in
/// the gaps.
///
/// Concrete embassy types, not `impl Trait`: a tuple return cannot carry
/// `impl Trait`, and both types implement the standard traits anyway.
pub(super) const ASYNC_USART_TMPL_DMA: &str =
    include_str!("../templates/embassy_stm32/usart_dma.rs.tmpl");

/// `configs/usart{n}.rs` — or `configs/lpuart{n}.rs`, which is the same file
/// with a different peripheral: embassy drives an LPUART through the very same
/// `usart::{Uart, BufferedUart}` API, so `peri` ("USART" / "LPUART") is the only
/// difference between the two.
/// `configs/{PERI}.rs` for a **TX-only** DMA UART: one pin, one channel.
///
/// A separate template rather than a flag on the full-duplex one because the
/// return type is the point — half a UART is a different value, not the same
/// value with a `None` in it.
pub(super) const ASYNC_USART_TMPL_DMA_TX: &str =
    include_str!("../templates/embassy_stm32/usart_dma_tx.rs.tmpl");

/// `configs/{PERI}.rs` for a **RX-only** DMA UART. The receiver is ring-buffered
/// for the same reason the full-duplex one is: a bare `UartRx` has no
/// `embedded_io_async::Read`, and reception must not stop between reads.
pub(super) const ASYNC_USART_TMPL_DMA_RX: &str =
    include_str!("../templates/embassy_stm32/usart_dma_rx.rs.tmpl");

/// The extra pin parameters, the `use` they need and the constructor call for
/// one `(transport, direction, flow)` combination.
///
/// Every string here mirrors an embassy signature EXACTLY, and the signatures
/// are not uniform: `BufferedUart::new` takes the buffers before the interrupt
/// binding, while its `new_with_*` variants take the binding first. Getting that
/// wrong compiles nowhere and reads fine — which is why the combinations are
/// listed rather than assembled from rules.
pub(super) fn serial_shape(peri_ty: &str, cfg: Option<&UsartModuleConfig>) -> SerialShape {
    let transport = cfg.map(|c| c.mode).unwrap_or_default();
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let flow = cfg.map(|c| c.flow).unwrap_or_default();
    let readback = serial_readback(cfg);
    let mut shape = serial_pads(peri_ty, cfg, false);
    shape.ctor = match (transport, direction, flow) {
        // ── Half duplex: one pad, both directions, no flow pads ─────────────
        (UsartMode::Buffered, UsartDirection::HalfDuplexOnTx, _) => format!(
            "BufferedUart::new_half_duplex(usart, tx, irqs, tx_buf, rx_buf, get_config(), {readback})"
        ),
        (UsartMode::Buffered, UsartDirection::HalfDuplexOnRx, _) => format!(
            "BufferedUart::new_half_duplex_on_rx(usart, rx, irqs, tx_buf, rx_buf, get_config(), {readback})"
        ),
        (UsartMode::Dma, UsartDirection::HalfDuplexOnTx, _) => format!(
            "Uart::new_half_duplex(usart, tx, tx_dma, rx_dma, irqs, get_config(), {readback})"
        ),
        (UsartMode::Dma, UsartDirection::HalfDuplexOnRx, _) => format!(
            "Uart::new_half_duplex_on_rx(usart, rx, tx_dma, rx_dma, irqs, get_config(), {readback})"
        ),
        // ── Buffered: always both pins (embassy has no buffered half) ────────
        (UsartMode::Buffered, _, UsartFlow::None) => {
            "BufferedUart::new(usart, rx, tx, tx_buf, rx_buf, irqs, get_config())".to_owned()
        }
        (UsartMode::Buffered, _, UsartFlow::Rts) => {
            "BufferedUart::new_with_rts(usart, rx, tx, rts, irqs, tx_buf, rx_buf, get_config())"
                .to_owned()
        }
        (UsartMode::Buffered, _, UsartFlow::CtsRts) => {
            "BufferedUart::new_with_rtscts(usart, rx, tx, rts, cts, irqs, tx_buf, rx_buf, get_config())"
                .to_owned()
        }
        (UsartMode::Buffered, _, UsartFlow::De) => {
            "BufferedUart::new_with_de(usart, rx, tx, de, irqs, tx_buf, rx_buf, get_config())"
                .to_owned()
        }
        // CTS-only has no buffered constructor; `UsartFlow::options` never
        // offers it there, and this keeps the fallback honest.
        (UsartMode::Buffered, _, UsartFlow::Cts) => {
            "BufferedUart::new(usart, rx, tx, tx_buf, rx_buf, irqs, get_config())".to_owned()
        }
        // ── DMA, full duplex ────────────────────────────────────────────────
        (UsartMode::Dma, UsartDirection::TxRx, UsartFlow::CtsRts) => {
            "Uart::new_with_rtscts(usart, rx, tx, rts, cts, tx_dma, rx_dma, irqs, get_config())"
                .to_owned()
        }
        (UsartMode::Dma, UsartDirection::TxRx, UsartFlow::De) => {
            "Uart::new_with_de(usart, rx, tx, de, tx_dma, rx_dma, irqs, get_config())".to_owned()
        }
        (UsartMode::Dma, UsartDirection::TxRx, _) => {
            "Uart::new(usart, rx, tx, tx_dma, rx_dma, irqs, get_config())".to_owned()
        }
        // ── DMA, one way: the real reason "direction" exists — one pin ──────
        (UsartMode::Dma, UsartDirection::TxOnly, UsartFlow::Cts) => {
            "UartTx::new_with_cts(usart, tx, cts, tx_dma, irqs, get_config())".to_owned()
        }
        (UsartMode::Dma, UsartDirection::TxOnly, _) => {
            "UartTx::new(usart, tx, tx_dma, irqs, get_config())".to_owned()
        }
        (UsartMode::Dma, UsartDirection::RxOnly, UsartFlow::Rts) => {
            "UartRx::new_with_rts(usart, rx, rts, rx_dma, irqs, get_config())".to_owned()
        }
        (UsartMode::Dma, UsartDirection::RxOnly, _) => {
            "UartRx::new(usart, rx, rx_dma, irqs, get_config())".to_owned()
        }
    };
    shape
}

/// [`serial_shape`] on the Blocking runtime: embassy's `new_blocking*` calls,
/// which take no interrupt binding, no buffer and no channel. Their flow forms
/// are the DMA `Uart`'s - `UsartFlow::options(UsartMode::Dma, …)` - and
/// [`blocking_serial_flow`] narrows any other choice to none before this.
pub(super) fn serial_shape_blocking(peri_ty: &str, cfg: Option<&UsartModuleConfig>) -> SerialShape {
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let flow = cfg.map(|c| c.flow).unwrap_or_default();
    let readback = serial_readback(cfg);
    // The two-way template names no pad trait itself: half duplex takes one
    // of the two, and importing both would leave one unused.
    let two_way = !matches!(direction, UsartDirection::TxOnly | UsartDirection::RxOnly);
    let mut shape = serial_pads(peri_ty, cfg, two_way);
    shape.ctor = match (direction, flow) {
        (UsartDirection::HalfDuplexOnTx, _) => {
            format!("Uart::new_blocking_half_duplex(usart, tx, get_config(), {readback})")
        }
        (UsartDirection::HalfDuplexOnRx, _) => {
            format!("Uart::new_blocking_half_duplex_on_rx(usart, rx, get_config(), {readback})")
        }
        (UsartDirection::TxRx, UsartFlow::CtsRts) => {
            "Uart::new_blocking_with_rtscts(usart, rx, tx, rts, cts, get_config())".to_owned()
        }
        (UsartDirection::TxRx, UsartFlow::De) => {
            "Uart::new_blocking_with_de(usart, rx, tx, de, get_config())".to_owned()
        }
        (UsartDirection::TxRx, _) => "Uart::new_blocking(usart, rx, tx, get_config())".to_owned(),
        (UsartDirection::TxOnly, UsartFlow::Cts) => {
            "UartTx::new_blocking_with_cts(usart, tx, cts, get_config())".to_owned()
        }
        (UsartDirection::TxOnly, _) => "UartTx::new_blocking(usart, tx, get_config())".to_owned(),
        (UsartDirection::RxOnly, UsartFlow::Rts) => {
            "UartRx::new_blocking_with_rts(usart, rx, rts, get_config())".to_owned()
        }
        (UsartDirection::RxOnly, _) => "UartRx::new_blocking(usart, rx, get_config())".to_owned(),
    };
    shape
}

/// The flow control a Blocking serial is built with: the module's, when one of
/// the `new_blocking*` calls takes it, and none otherwise - the pads of a
/// choice no call takes stay bound raw instead of becoming `init` parameters
/// nothing reads.
pub(super) fn blocking_serial_flow(direction: UsartDirection, flow: UsartFlow) -> UsartFlow {
    if UsartFlow::options(UsartMode::Dma, direction).contains(&flow) {
        flow
    } else {
        UsartFlow::None
    }
}

/// The readback argument of the half-duplex constructors.
fn serial_readback(cfg: Option<&UsartModuleConfig>) -> &'static str {
    if cfg.is_some_and(|c| c.half_duplex_readback) {
        "HalfDuplexReadback::Readback"
    } else {
        "HalfDuplexReadback::NoReadback"
    }
}

/// Everything of a [`SerialShape`] but the constructor: the pad parameters
/// and the `use` their traits need, the same on both runtimes. `data_traits`
/// puts the data pads' `RxPin` / `TxPin` in that `use` too, for a template
/// that does not import them itself.
fn serial_pads(peri_ty: &str, cfg: Option<&UsartModuleConfig>, data_traits: bool) -> SerialShape {
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let flow = cfg.map(|c| c.flow).unwrap_or_default();
    let pin_param = |name: &str, tr: &str| {
        format!(
            "    {name}: Peri<'d, impl {tr}<peripherals::{peri_ty}>>,
"
        )
    };
    // The data pads, in the order every constructor declares them: rx before tx
    // for the two-pad forms, and the single pad on its own for half duplex.
    let mut pins = String::new();
    if direction.needs_rx() {
        pins.push_str(&pin_param("rx", "RxPin"));
    }
    if direction.needs_tx() {
        pins.push_str(&pin_param("tx", "TxPin"));
    }
    let (mut params, mut uses) = (String::new(), String::new());
    let mut pin_traits: Vec<&str> = Vec::new();
    if data_traits && direction.needs_rx() {
        pin_traits.push("RxPin");
    }
    if data_traits && direction.needs_tx() {
        pin_traits.push("TxPin");
    }
    // RTS and DE are the same pad; the trait differs, so the parameter is named
    // after the ROLE the constructor gives it.
    if flow.needs_rts() {
        let (name, tr) = if flow == UsartFlow::De {
            ("de", "DePin")
        } else {
            ("rts", "RtsPin")
        };
        params.push_str(&pin_param(name, tr));
        pin_traits.push(tr);
    }
    if flow.needs_cts() {
        params.push_str(&pin_param("cts", "CtsPin"));
        pin_traits.push("CtsPin");
    }
    if direction.is_half_duplex() {
        pin_traits.push("HalfDuplexReadback");
    }
    if !pin_traits.is_empty() {
        pin_traits.sort_unstable();
        uses = format!(
            "use embassy_stm32::usart::{{{}}};
",
            pin_traits.join(", ")
        );
    }

    SerialShape {
        uses,
        pins,
        flow_params: params,
        ctor: String::new(),
    }
}

/// The generated pieces of one `init`: the extra `use`, the data-pad parameters,
/// the flow-pad parameters and the constructor call.
pub(super) struct SerialShape {
    uses: String,
    pins: String,
    flow_params: String,
    ctor: String,
}

/// The line-level extras (`swap_rx_tx`, `invert_tx`, `invert_rx`) as a pair of
/// `(consts, assignments)` — all three, `true` or `false`, wherever the chip
/// has them. Named only while switched on, a constant vanished from the block
/// the moment the box was cleared, and an edited file whose `get_config` still
/// assigned it stopped compiling; a block whose names never move cannot do
/// that (the rule in `peripheral-config-files`).
///
/// `supported` is the chip gate: embassy declares these `Config` fields only
/// under `#[cfg(any(usart_v3, usart_v4))]`, so on an older USART the assignment
/// would not compile. Codegen re-checks it rather than trusting the UI — a
/// project file can outlive the chip it was written for.
pub(super) fn serial_line_extras(
    cfg: Option<&UsartModuleConfig>,
    supported: bool,
) -> (String, String) {
    let (mut consts, mut body) = (String::new(), String::new());
    if !supported {
        return (consts, body);
    }
    for (on, konst, field) in [
        (
            cfg.is_some_and(|c| c.swap_rx_tx),
            "SWAP_RX_TX",
            "swap_rx_tx",
        ),
        (cfg.is_some_and(|c| c.invert_tx), "INVERT_TX", "invert_tx"),
        (cfg.is_some_and(|c| c.invert_rx), "INVERT_RX", "invert_rx"),
    ] {
        consts.push_str(&format!("pub const {konst}: bool = {on};\n"));
        body.push_str(&format!("    config.{field} = {konst};\n"));
    }
    consts.insert_str(
        0,
        "// Line options: TX and RX pads swapped, TX inverted, RX inverted.\n",
    );
    (consts, body)
}

/// USART1 on a chip with the line options (`usart_v4`), swap on or off: for
/// the sync tests in `project_tree`.
#[cfg(test)]
pub fn usart_file_for_tests(swap: bool) -> String {
    let cfg = UsartModuleConfig {
        swap_rx_tx: swap,
        ..UsartModuleConfig::new(1)
    };
    serial_config_file("USART", 1, Some(&cfg), "USART1", true)
}

/// `body` as the IDE wrote it before 1B.4: an option named - in the block and
/// in `get_config` - only while it was on.
#[cfg(test)]
pub fn legacy_usart_file(body: &str) -> String {
    let mut out = body.replace(
        "// Line options: TX and RX pads swapped, TX inverted, RX inverted.\n",
        "",
    );
    for (konst, field) in [
        ("SWAP_RX_TX", "swap_rx_tx"),
        ("INVERT_TX", "invert_tx"),
        ("INVERT_RX", "invert_rx"),
    ] {
        let off = format!("pub const {konst}: bool = false;\n");
        if out.contains(&off) {
            out = out
                .replace(&off, "")
                .replace(&format!("    config.{field} = {konst};\n"), "");
        }
    }
    out
}

pub fn serial_config_file(
    peri: &str,
    n: u8,
    cfg: Option<&UsartModuleConfig>,
    irq: &str,
    // Whether this CHIP's USART has the swap/invert bits at all.
    line_extras: bool,
) -> String {
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let template = match (cfg.map(|c| c.mode).unwrap_or_default(), direction) {
        (UsartMode::Dma, UsartDirection::TxOnly) => ASYNC_USART_TMPL_DMA_TX,
        (UsartMode::Dma, UsartDirection::RxOnly) => ASYNC_USART_TMPL_DMA_RX,
        // Half duplex returns a FULL `Uart` / `BufferedUart` — one pad, both
        // directions — so it reuses the two-way templates; only the pin
        // parameters and the constructor differ, and `serial_shape` owns both.
        (UsartMode::Dma, _) => ASYNC_USART_TMPL_DMA,
        (UsartMode::Buffered, _) => ASYNC_USART_TMPL,
    };
    let shape = serial_shape(&format!("{peri}{n}"), cfg);
    let body = render_serial(template, &shape, peri, n, cfg, irq, line_extras);
    // The two-way templates import both data-pad traits, and half duplex takes
    // one pad: the other would be an `unused_imports` warning in the user's
    // project. Only here, so a full-duplex file stays as it always was.
    match direction {
        UsartDirection::HalfDuplexOnTx => without_usart_use(&body, "RxPin"),
        UsartDirection::HalfDuplexOnRx => without_usart_use(&body, "TxPin"),
        _ => body,
    }
}

/// `body` with `item` taken out of its `use embassy_stm32::usart::{..}` list,
/// the list then written on one line. Unchanged when the list or the item is
/// not there.
fn without_usart_use(body: &str, item: &str) -> String {
    const HEAD: &str = "use embassy_stm32::usart::{";
    let Some(at) = body.find(HEAD) else {
        return body.to_owned();
    };
    let start = at + HEAD.len();
    let Some(len) = body[start..].find("};") else {
        return body.to_owned();
    };
    let items: Vec<&str> = body[start..start + len]
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if !items.contains(&item) {
        return body.to_owned();
    }
    let kept: Vec<&str> = items.into_iter().filter(|s| *s != item).collect();
    format!(
        "{}{HEAD}{}}};{}",
        &body[..at],
        kept.join(", "),
        &body[start + len + 2..]
    )
}

/// The `get_config` lines a single-wire UART needs: its pad open-drain. The
/// USART releases the line between frames - ST's manuals ask for the TX pad
/// as an open-drain alternate function with a pull-up on the line - and
/// embassy leaves `tx_config` at its push-pull default, which would drive
/// against every other talker. Plain `OpenDrain`: the F1's gpio_v1 has no
/// `OpenDrainPullUp`, and a bus wants an external pull-up anyway.
const HALF_DUPLEX_OPEN_DRAIN: &str =
    "    // Single wire: the pad is released between frames, so it is open-drain, and
    // the line needs a pull-up (an external one: a bus has other talkers).
    config.tx_config = embassy_stm32::usart::OutputConfig::OpenDrain;
";

/// The DMA template's closing note on a single wire: the ring-buffered
/// receiver turns the transmitter off without waiting for it.
const HALF_DUPLEX_DMA_USAGE: &str = "//
// ── Single wire ──
// One pad carries both directions, and a read turns the transmitter off
// without waiting for it: `tx.flush().await` after writing, before reading, or
// the last byte is cut off the wire.
";

/// `configs/usart{n}.rs` on the Blocking runtime: a polled UART, whatever the
/// module's transport says - the buffer and the DMA are the Async runtime's.
/// Full and half duplex hand back an `embedded-io` Read + Write, a one-way UART
/// its half.
pub(super) const BLOCKING_USART_TMPL: &str =
    include_str!("../templates/embassy_stm32/usart_blocking.rs.tmpl");

/// [`BLOCKING_USART_TMPL`] for a transmitter: embassy's `UartTx`, which
/// implements `embedded_io::Write` itself.
pub(super) const BLOCKING_USART_TMPL_TX: &str =
    include_str!("../templates/embassy_stm32/usart_blocking_tx.rs.tmpl");

/// [`BLOCKING_USART_TMPL`] for a receiver: `UartRx` behind the `embedded-io`
/// `Read` embassy gives it none of.
pub(super) const BLOCKING_USART_TMPL_RX: &str =
    include_str!("../templates/embassy_stm32/usart_blocking_rx.rs.tmpl");

/// [`serial_config_file`] for the Blocking runtime.
pub fn serial_config_file_blocking(
    peri: &str,
    n: u8,
    cfg: Option<&UsartModuleConfig>,
    line_extras: bool,
) -> String {
    let template = match cfg.map(|c| c.direction).unwrap_or_default() {
        UsartDirection::TxOnly => BLOCKING_USART_TMPL_TX,
        UsartDirection::RxOnly => BLOCKING_USART_TMPL_RX,
        _ => BLOCKING_USART_TMPL,
    };
    let shape = serial_shape_blocking(&format!("{peri}{n}"), cfg);
    render_serial(template, &shape, peri, n, cfg, "", line_extras)
}

/// One serial template filled with the module's settings and `shape`.
fn render_serial(
    template: &str,
    shape: &SerialShape,
    peri: &str,
    n: u8,
    cfg: Option<&UsartModuleConfig>,
    irq: &str,
    line_extras: bool,
) -> String {
    let baud = cfg.map(|c| c.baud_rate).unwrap_or(115_200);
    // Clamped, not trusted: a 0-byte `StaticCell<[u8; 0]>` compiles and then
    // never delivers a byte, which reads as a dead link rather than as a
    // setting. The UI clamps too; this is the backstop for a hand-edited
    // `mcu.config`.
    let buf = cfg.map(|c| c.buf_len).unwrap_or(256).clamp(16, 65_536);
    let data = cfg.map(|c| c.data_bits).unwrap_or(8);
    let parity = match cfg.map(|c| c.parity) {
        Some(Parity::Odd) => 'O',
        Some(Parity::Even) => 'E',
        _ => 'N',
    };
    let stop = match cfg.map(|c| c.stop_bits) {
        Some(StopBits::Two) => 2,
        _ => 1,
    };
    let sfx = cfg
        .map(|c| sanitize_label(&c.custom_label))
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();
    let (extra_consts, mut extra_config) = serial_line_extras(cfg, line_extras);
    let half_duplex = cfg.is_some_and(|c| c.direction.is_half_duplex());
    if half_duplex {
        extra_config.push_str(HALF_DUPLEX_OPEN_DRAIN);
    }
    // main.rs binds a full-duplex DMA pair as `{HANDLE}_tx, {HANDLE}_rx`, and
    // a half-duplex one - the same pair - whole, as `{HANDLE}`, the name it
    // has always had there. Filled first: both name the handle.
    let (pair, tx_half) = if half_duplex {
        ("{HANDLE}", "{HANDLE}.0")
    } else {
        ("({HANDLE}_tx, {HANDLE}_rx)", "{HANDLE}_tx")
    };
    tmpl::render(
        template,
        &[
            ("{PAIR}", pair),
            ("{TX_HALF}", tx_half),
            (
                "{HALF_DUPLEX_USAGE}",
                if half_duplex {
                    HALF_DUPLEX_DMA_USAGE
                } else {
                    ""
                },
            ),
            ("{EXTRA_CONSTS}", &extra_consts),
            ("{EXTRA_CONFIG}", &extra_config),
            ("{FLOW_USE}", shape.uses.trim_end()),
            ("{PIN_PARAMS}", &shape.pins),
            ("{FLOW_PARAMS}", &shape.flow_params),
            ("{CTOR}", &shape.ctor),
            ("{HANDLE}", &serial_handle(peri, n, &sfx)),
            ("{PERI}", &format!("{peri}{n}")),
            ("{UIRQ}", irq),
            ("{N}", &n.to_string()),
            ("{BAUD}", &baud.to_string()),
            ("{DATA}", &data.to_string()),
            ("{PARITY}", &parity.to_string()),
            ("{STOP}", &stop.to_string()),
            ("{BUF}", &buf.to_string()),
        ],
    )
}

#[cfg(test)]
mod usart_mode_tests {
    use super::*;
    use crate::panels::mcu_module::modules::UsartModuleConfig;

    fn cfg(mode: UsartMode) -> UsartModuleConfig {
        UsartModuleConfig {
            mode,
            ..UsartModuleConfig::new(1)
        }
    }

    /// The size the module carries reaches the generated file - in both modes,
    /// under the name each one uses.
    #[test]
    fn the_module_buffer_size_reaches_the_generated_code() {
        for (mode, name) in [
            (UsartMode::Buffered, "BUF_LEN"),
            (UsartMode::Dma, "RX_DMA_BUF"),
        ] {
            let c = UsartModuleConfig {
                buf_len: 1024,
                ..cfg(mode)
            };
            let f = serial_config_file("USART", 1, Some(&c), "USART1", true);
            assert!(
                f.contains(&format!("pub const {name}: usize = 1024;")),
                "{mode:?} must carry the size:
{f}"
            );
            assert!(
                !f.contains("= 256;"),
                "the old default must be gone:
{f}"
            );
        }
    }

    /// …and it lands INSIDE the generated block. Left below the marker it would
    /// be preserved across regeneration, so the module's value would be written
    /// once and then silently ignored - a field that looks live and is not.
    #[test]
    fn the_size_is_regenerated_not_preserved() {
        let f = serial_config_file(
            "USART",
            1,
            Some(&UsartModuleConfig {
                buf_len: 512,
                ..cfg(UsartMode::Buffered)
            }),
            "USART1",
            true,
        );
        let end = f.find("// <<< GENERATED END >>>").expect("marker");
        let at = f.find("pub const BUF_LEN").expect("the constant");
        assert!(
            at < end,
            "BUF_LEN must be above the END marker:
{f}"
        );
    }

    /// A hand-edited `mcu.config` cannot produce a buffer the code chokes on.
    #[test]
    fn an_absurd_size_is_clamped_rather_than_emitted() {
        let zero = serial_config_file(
            "USART",
            1,
            Some(&UsartModuleConfig {
                buf_len: 0,
                ..cfg(UsartMode::Buffered)
            }),
            "USART1",
            true,
        );
        // A zero-length StaticCell compiles and then never delivers a byte,
        // which reads as broken hardware rather than as a setting.
        assert!(zero.contains("pub const BUF_LEN: usize = 16;"), "{zero}");
        let huge = serial_config_file(
            "USART",
            1,
            Some(&UsartModuleConfig {
                buf_len: 10_000_000,
                ..cfg(UsartMode::Buffered)
            }),
            "USART1",
            true,
        );
        assert!(huge.contains("pub const BUF_LEN: usize = 65536;"), "{huge}");
    }

    #[test]
    fn buffered_is_the_default_and_keeps_the_old_output() {
        assert_eq!(UsartMode::default(), UsartMode::Buffered);
        let f = serial_config_file("USART", 1, Some(&cfg(UsartMode::Buffered)), "USART1", true);
        assert!(f.contains("BufferedUart::new("), "{f}");
        // The interrupt binding is main.rs's, for both forms — see
        // `the_buffered_usart_binds_the_chips_vector_in_main`.
        assert!(!f.contains("bind_interrupts!(struct Irqs"), "{f}");
        assert!(!f.contains("RingBufferedUartRx"), "{f}");
    }

    #[test]
    fn dma_splits_the_uart_so_read_survives() {
        let f = serial_config_file("USART", 1, Some(&cfg(UsartMode::Dma)), "USART1", true);
        // The whole point: a bare `Uart<Async>` has `embedded_io_async::Write`
        // but NOT `Read`, so the RX half must become a ring-buffered receiver.
        assert!(
            f.contains("-> (UartTx<'d, Async>, RingBufferedUartRx<'d>)"),
            "{f}"
        );
        assert!(f.contains("rx.into_ring_buffered("), "{f}");
        assert!(
            f.contains("Uart::new(usart, rx, tx, tx_dma, rx_dma, irqs, get_config())"),
            "{f}"
        );
        // No local `Irqs`: embassy wants one value binding the peripheral AND
        // both channels, and only main.rs knows the channels. Matched on the
        // INVOCATION, not the word - the doc comment above `init` explains why
        // the macro is absent, and naming it there must not fail this.
        assert!(!f.contains("bind_interrupts!(struct"), "{f}");
        assert!(f.contains("irqs: impl Binding<"), "{f}");
    }

    /// A channel pinned by hand in the Virtual Module reaches BOTH the `init`
    /// call and the interrupt binding, and does not get handed to anyone else.
    #[test]
    fn a_pinned_channel_beats_the_automatic_one() {
        use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
        use crate::panels::mcu_module::mcu_def::DmaDef;
        use crate::panels::mcu_module::modules::{AsyncBusMode, SpiModuleConfig};
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mk = |name: &str, f: PinFunction| {
            let mut p = Pin::new(1, name);
            p.selected_function = f;
            p
        };
        let pins = [
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
            mk("PA5", PinFunction::SpiSck(1)),
            mk("PA7", PinFunction::SpiMosi(1)),
            mk("PA6", PinFunction::SpiMiso(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        // A muxed chip, so automatic allocation would take CH1 and CH2 first.
        let chip = DmaDef {
            mux: true,
            channels: (1..=6)
                .map(|i| DmaChannel {
                    peri: format!("DMA1_CH{i}"),
                    irq: format!("DMA1_CHANNEL{i}"),
                })
                .collect(),
            requests: Vec::new(),
        };
        // The SPI - emitted AFTER the USART - claims CH1 by hand.
        let spi: BTreeMap<u8, SpiModuleConfig> = [(
            1u8,
            SpiModuleConfig {
                async_mode: AsyncBusMode::AsyncDma,
                dma_tx: "DMA1_CH1".into(),
                ..SpiModuleConfig::new(1)
            },
        )]
        .into_iter()
        .collect();
        let usart: BTreeMap<u8, UsartModuleConfig> =
            [(1u8, cfg(UsartMode::Dma))].into_iter().collect();
        let out = async_peripherals(
            "stm32g4",
            ChipData {
                i2s: true,
                dma: Some(&chip),
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
            &usart,
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
            out.init_calls.contains("p.DMA1_CH1, p.DMA1_CH4, Irqs"),
            "the SPI keeps its pinned TX and gets the next free RX:
{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("p.DMA1_CH2, p.DMA1_CH3, Irqs"),
            "the USART skipped the reserved CH1:
{}",
            out.init_calls
        );
        assert!(
            out.dma_irqs.contains(
                "DMA1_CHANNEL1 => embassy_stm32::dma::InterruptHandler<peripherals::DMA1_CH1>;"
            ),
            "the pinned channel still gets its binding:
{}",
            out.dma_irqs
        );
    }

    /// LPUART's hand-pinned channel is reserved like everyone else's.
    ///
    /// It is its OWN module map, and it was the one map missing from the
    /// reserve chain - so its pinned channel stayed free while the USART, which
    /// is emitted FIRST, was allocated automatically. The USART took it and the
    /// LPUART fell through to `AlreadyTaken` and a clash TODO, which is exactly
    /// the emission-order arbitrariness the manual field exists to remove.
    ///
    /// The USART here is the thief on purpose: it is served before the LPUART,
    /// so nothing else in this file would catch the omission.
    #[test]
    fn an_lpuart_keeps_the_channel_it_pinned() {
        use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
        use crate::panels::mcu_module::mcu_def::DmaDef;
        use crate::panels::mcu_module::modules::UsartMode;
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mk = |name: &str, f: PinFunction| {
            let mut p = Pin::new(1, name);
            p.selected_function = f;
            p
        };
        let pins = [
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
            mk("PA2", PinFunction::LpuartTx(1)),
            mk("PA3", PinFunction::LpuartRx(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        // Muxed, so automatic allocation walks CH1, CH2, … in order and the
        // USART would otherwise swallow the LPUART's CH1.
        let chip = DmaDef {
            mux: true,
            channels: (1..=6)
                .map(|i| DmaChannel {
                    peri: format!("DMA1_CH{i}"),
                    irq: format!("DMA1_CHANNEL{i}"),
                })
                .collect(),
            requests: Vec::new(),
        };
        let dma_usart = || UsartModuleConfig {
            mode: UsartMode::Dma,
            ..UsartModuleConfig::new(1)
        };
        let lpuart: BTreeMap<u8, UsartModuleConfig> = [(
            1u8,
            UsartModuleConfig {
                dma_tx: "DMA1_CH1".into(),
                ..dma_usart()
            },
        )]
        .into_iter()
        .collect();
        let out = async_peripherals(
            "stm32g0",
            ChipData {
                i2s: true,
                dma: Some(&chip),
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
            &[(1u8, dma_usart())].into_iter().collect(),
            &Default::default(),
            &Default::default(),
            &lpuart,
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
            !has_dma_todo(&out.init_calls),
            "the LPUART pinned a real channel, so nothing should fall to a TODO:
{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("p.DMA1_CH1,"),
            "the LPUART keeps the channel it pinned:
{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("p.DMA1_CH2, p.DMA1_CH3, Irqs"),
            "the USART, served FIRST, must skip the reserved CH1:
{}",
            out.init_calls
        );
    }

    /// The Configuration tab's list must name EXACTLY the channels `main.rs`
    /// takes - no more, no fewer. This is the property the whole design rests
    /// on: the list is the generator's own record, so a change to allocation
    /// that forgot the record would show up here rather than as a confident
    /// wrong answer on screen.
    #[test]
    fn the_reported_uses_are_the_channels_main_rs_takes() {
        use crate::panels::mcu_module::codegen::dma_data::DmaChannel;
        use crate::panels::mcu_module::mcu_def::DmaDef;
        use crate::panels::mcu_module::modules::{AsyncBusMode, I2cModuleConfig, SpiModuleConfig};
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mk = |name: &str, f: PinFunction| {
            let mut p = Pin::new(1, name);
            p.selected_function = f;
            p
        };
        let pins = [
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
            mk("PA5", PinFunction::SpiSck(1)),
            mk("PA7", PinFunction::SpiMosi(1)),
            mk("PA6", PinFunction::SpiMiso(1)),
            mk("PB6", PinFunction::I2cScl(1)),
            mk("PB7", PinFunction::I2cSda(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        let chip = DmaDef {
            mux: true,
            channels: (1..=8)
                .map(|i| DmaChannel {
                    peri: format!("DMA1_CH{i}"),
                    irq: format!("DMA1_CHANNEL{i}"),
                })
                .collect(),
            requests: Vec::new(),
        };
        let usart: BTreeMap<u8, UsartModuleConfig> =
            [(1u8, cfg(UsartMode::Dma))].into_iter().collect();
        let spi: BTreeMap<u8, SpiModuleConfig> = [(
            1u8,
            SpiModuleConfig {
                async_mode: AsyncBusMode::AsyncDma,
                dma_tx: "DMA1_CH8".into(),
                ..SpiModuleConfig::new(1)
            },
        )]
        .into_iter()
        .collect();
        let i2c: BTreeMap<u8, I2cModuleConfig> = [(
            1u8,
            I2cModuleConfig {
                async_mode: AsyncBusMode::AsyncDma,
                ..I2cModuleConfig::new(1)
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
            &usart,
            &spi,
            &i2c,
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

        // Every channel the code takes, straight out of the emitted text.
        let mut in_code: Vec<String> = out
            .init_calls
            .split("p.DMA")
            .skip(1)
            .filter_map(|t| t.split(&[',', ')'][..]).next())
            .map(|t| format!("DMA{t}"))
            .collect();
        let mut reported: Vec<String> = out.dma_uses.iter().map(|u| u.peri.clone()).collect();
        assert_eq!(
            reported.len(),
            6,
            "three buses, two channels each: {reported:?}"
        );
        in_code.sort();
        reported.sort();
        assert_eq!(reported, in_code, "the list and the code disagree");

        // The pinned one is reported AS pinned, and it really is the SPI's.
        let pinned: Vec<&str> = out
            .dma_uses
            .iter()
            .filter(|u| u.manual)
            .map(|u| u.user.as_str())
            .collect();
        assert_eq!(pinned, ["SPI1 TX"], "{:?}", out.dma_uses);
        let spi_tx = out.dma_uses.iter().find(|u| u.user == "SPI1 TX").unwrap();
        assert_eq!(spi_tx.peri, "DMA1_CH8");
        assert_eq!(
            spi_tx.irq, "DMA1_CHANNEL8",
            "the card shows the binding key"
        );
    }

    #[test]
    fn dma_mode_reaches_main_rs_as_a_pair_plus_bindings() {
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let mk = |name: &str, f: PinFunction| {
            let mut p = Pin::new(1, name);
            p.selected_function = f;
            p
        };
        let pins = [
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        let usart: BTreeMap<u8, UsartModuleConfig> =
            [(1u8, cfg(UsartMode::Dma))].into_iter().collect();
        let out = async_peripherals(
            "stm32f4",
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
            &usart,
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
        );
        // A PAIR, not a single handle - the two halves have different types.
        assert!(
            out.init_calls
                .contains("let (mut _serial1_tx, mut _serial1_rx) ="),
            "{}",
            out.init_calls
        );
        // On a family with a channel table the TODO is already resolved.
        assert!(
            out.init_calls.contains("p.DMA2_CH7, p.DMA2_CH5, Irqs"),
            "{}",
            out.init_calls
        );
        // A family WITHOUT one keeps the placeholder rather than guessing.
        // F2 stood here, then F7; both have their own table now. Whichever
        // family fills this slot is by definition the next one worth
        // harvesting.
        let bare = async_peripherals(
            "stm32l4",
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
            &usart,
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
        );
        assert!(
            bare.init_calls.contains("DMA_TX_TODO"),
            "{}",
            bare.init_calls
        );
        // The USART's own interrupt moved into main.rs's Irqs alongside the
        // channels'; without this the project cannot build at all.
        assert!(
            out.dma_irqs
                .contains("USART1 => embassy_stm32::usart::InterruptHandler"),
            "{}",
            out.dma_irqs
        );
        assert!(
            out.any_async_dma,
            "the DMA USART must pull in the async deps"
        );

        // Buffered takes none of that.
        let usart: BTreeMap<u8, UsartModuleConfig> =
            [(1u8, cfg(UsartMode::Buffered))].into_iter().collect();
        let out = async_peripherals(
            "stm32f4",
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
            &usart,
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
        );
        assert!(
            out.init_calls.contains("let mut _serial1 = "),
            "{}",
            out.init_calls
        );
        // Buffered pulls in no DMA — but it DOES need its vector bound, and the
        // handler is the buffered one.
        assert!(!out.any_async_dma);
        assert!(
            out.dma_irqs
                .contains("USART1 => embassy_stm32::usart::BufferedInterruptHandler"),
            "{}",
            out.dma_irqs
        );
    }
}

#[cfg(test)]
mod lpuart_tests {
    use super::*;
    use crate::panels::mcu_module::modules::UsartModuleConfig;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    fn run(pins: &[Pin], irqs: &[String]) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32g0",
            ChipData {
                i2s: true,
                dma: None,
                irq_vectors: irqs,
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
            &[(1u8, UsartModuleConfig::new(1))].into_iter().collect(),
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

    /// An LPUART generates the SAME shape as a USART, against its own
    /// peripheral: `configs/lpuart1.rs` driving `peripherals::LPUART1`.
    #[test]
    fn lpuart_gets_its_own_config_module_and_peripheral() {
        let pins = [
            mk("PA2", PinFunction::LpuartTx(1)),
            mk("PA3", PinFunction::LpuartRx(1)),
        ];
        let out = run(&pins, &[]);

        let (name, body) = out
            .config_files
            .iter()
            .find(|(n, _)| n == "lpuart1.rs")
            .expect("a config module for the LPUART");
        assert_eq!(name, "lpuart1.rs");
        assert!(
            body.contains("peripherals::LPUART1"),
            "the LPUART peripheral, not a USART: {body}"
        );
        assert!(!body.contains("USART1"), "{body}");
        assert!(
            out.init_calls.contains(
                "pins::configs::lpuart1::init(p.LPUART1, pa3_lpuart1_rx, pa2_lpuart1_tx, Irqs)"
            ),
            "{}",
            out.init_calls
        );
    }

    /// The collision the naming exists to prevent: a chip with USART1 AND
    /// LPUART1 must bind two DIFFERENT variables.
    #[test]
    fn usart1_and_lpuart1_coexist_with_distinct_handles() {
        let pins = [
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
            mk("PA2", PinFunction::LpuartTx(1)),
            mk("PA3", PinFunction::LpuartRx(1)),
        ];
        let refs: Vec<&Pin> = pins.iter().collect();
        let out = async_peripherals(
            "stm32g0",
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
            &[(1u8, UsartModuleConfig::new(1))].into_iter().collect(),
            &Default::default(),
            &Default::default(),
            &[(1u8, UsartModuleConfig::new(1))].into_iter().collect(),
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
            out.init_calls.contains("let mut _serial1 ="),
            "{}",
            out.init_calls
        );
        assert!(
            out.init_calls.contains("let mut _lpserial1 ="),
            "{}",
            out.init_calls
        );
        // Both config modules, neither overwriting the other.
        let names: Vec<&str> = out.config_files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"usart1.rs") && names.contains(&"lpuart1.rs"),
            "{names:?}"
        );
    }

    /// On an STM32G0 the LPUART shares one NVIC vector with USART3/4, and that
    /// shared name is what `bind_interrupts!` must be keyed on.
    #[test]
    fn the_g0_combined_vector_is_used_for_the_lpuart() {
        let irqs = vec!["USART1".to_owned(), "USART3_4_LPUART1".to_owned()];
        let pins = [
            mk("PA2", PinFunction::LpuartTx(1)),
            mk("PA3", PinFunction::LpuartRx(1)),
        ];
        let out = run(&pins, &irqs);
        // ONE binding site, in main.rs, under the shared vector name.
        assert!(
            out.dma_irqs.contains(
                "USART3_4_LPUART1 => embassy_stm32::usart::BufferedInterruptHandler<peripherals::LPUART1>;"
            ),
            "{}",
            out.dma_irqs
        );
    }
}

#[cfg(test)]
mod flow_and_direction_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    /// STM32F4 on purpose: it is the family with a built-in DMA request table,
    /// so a channel really resolves and the TX-only case can assert that only
    /// ONE was taken.
    fn run(pins: &[Pin], cfg: UsartModuleConfig) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32f4",
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
            &[(1u8, cfg)].into_iter().collect(),
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

    fn full_duplex() -> Vec<Pin> {
        vec![
            mk("PA9", PinFunction::UsartTx(1)),
            mk("PA10", PinFunction::UsartRx(1)),
        ]
    }

    /// The list of flow options is embassy's constructor list, not the chip's
    /// capability list — that is the rule the UI leans on.
    #[test]
    fn only_constructible_combinations_are_offered() {
        // Buffered has `new_with_rts` but no `new_with_cts`.
        let buf = UsartFlow::options(UsartMode::Buffered, UsartDirection::TxRx);
        assert!(buf.contains(&UsartFlow::Rts) && buf.contains(&UsartFlow::CtsRts));
        assert!(!buf.contains(&UsartFlow::Cts), "{buf:?}");
        // The DMA full-duplex Uart has neither one-sided form…
        let dma = UsartFlow::options(UsartMode::Dma, UsartDirection::TxRx);
        assert!(!dma.contains(&UsartFlow::Cts) && !dma.contains(&UsartFlow::Rts));
        // …but the one-way halves do, each with the pad that suits it.
        assert_eq!(
            UsartFlow::options(UsartMode::Dma, UsartDirection::TxOnly),
            &[UsartFlow::None, UsartFlow::Cts]
        );
        assert_eq!(
            UsartFlow::options(UsartMode::Dma, UsartDirection::RxOnly),
            &[UsartFlow::None, UsartFlow::Rts]
        );
        // The buffered transport cannot do one-way at all — but it CAN do half
        // duplex, which is a whole `BufferedUart` on one pad, not a half of one.
        let buf_dirs = UsartDirection::options(UsartMode::Buffered);
        assert!(!buf_dirs.contains(&UsartDirection::TxOnly), "{buf_dirs:?}");
        assert!(
            buf_dirs.contains(&UsartDirection::HalfDuplexOnTx),
            "{buf_dirs:?}"
        );
        // No half-duplex constructor takes a flow pad, on either transport.
        for t in [UsartMode::Buffered, UsartMode::Dma] {
            assert_eq!(
                UsartFlow::options(t, UsartDirection::HalfDuplexOnRx),
                &[UsartFlow::None]
            );
        }
    }

    /// CTS/RTS reach the generated code only once their pads are wired, and the
    /// argument order matches the constructor embassy actually exposes.
    #[test]
    fn buffered_rtscts_passes_both_pads() {
        let mut pins = full_duplex();
        pins.push(mk("PA11", PinFunction::UsartCts(1)));
        pins.push(mk("PA12", PinFunction::UsartRts(1)));
        let cfg = UsartModuleConfig {
            flow: UsartFlow::CtsRts,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&pins, cfg);
        assert!(
            out.init_calls
                .contains("init(p.USART1, pa10_usart1_rx, pa9_usart1_tx, pa12_usart1_rts, pa11_usart1_cts, Irqs)"),
            "rx, tx, rts, cts — the order `new_with_rtscts` declares: {}",
            out.init_calls
        );
        let body = &out.config_files[0].1;
        assert!(
            body.contains("BufferedUart::new_with_rtscts(usart, rx, tx, rts, cts, irqs, tx_buf, rx_buf, get_config())"),
            "{body}"
        );
        assert!(
            body.contains("use embassy_stm32::usart::{CtsPin, RtsPin};"),
            "{body}"
        );
    }

    /// A flow option whose pad is NOT wired must not name a pin that isn't
    /// there — it degrades to the plain constructor instead.
    #[test]
    fn flow_without_its_pad_falls_back() {
        let cfg = UsartModuleConfig {
            flow: UsartFlow::CtsRts,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&full_duplex(), cfg);
        let body = &out.config_files[0].1;
        assert!(
            body.contains("BufferedUart::new(usart, rx, tx, tx_buf"),
            "{body}"
        );
        assert!(!body.contains("rtscts"), "{body}");
        assert!(
            out.init_calls
                .contains("init(p.USART1, pa10_usart1_rx, pa9_usart1_tx, Irqs)"),
            "{}",
            out.init_calls
        );
    }

    /// TX-only on DMA is the direction that actually frees a pin: no RX pad, and
    /// no RX channel taken out of circulation either.
    #[test]
    fn tx_only_needs_neither_rx_pin_nor_rx_channel() {
        let pins = vec![mk("PA9", PinFunction::UsartTx(1))];
        let cfg = UsartModuleConfig {
            mode: UsartMode::Dma,
            direction: UsartDirection::TxOnly,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&pins, cfg);
        assert!(
            out.init_calls.contains("let mut _serial1 = "),
            "one handle, not a pair: {}",
            out.init_calls
        );
        assert_eq!(
            out.dma_uses.len(),
            1,
            "only the TX channel: {:?}",
            out.dma_uses.iter().map(|u| &u.user).collect::<Vec<_>>()
        );
        let body = &out.config_files[0].1;
        assert!(
            body.contains("UartTx::new(usart, tx, tx_dma, irqs, get_config())"),
            "{body}"
        );
        assert!(body.contains("-> UartTx<'d, Async>"), "{body}");
    }

    /// Half duplex is ONE pad doing both directions: a single pin parameter, no
    /// flow pads, and the readback argument embassy demands.
    #[test]
    fn half_duplex_takes_one_pad_and_a_readback() {
        let pins = vec![mk("PA9", PinFunction::UsartTx(1))];
        let cfg = UsartModuleConfig {
            direction: UsartDirection::HalfDuplexOnTx,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&pins, cfg);
        let body = &out.config_files[0].1;
        assert!(
            body.contains(
                "BufferedUart::new_half_duplex(usart, tx, irqs, tx_buf, rx_buf, get_config(), HalfDuplexReadback::NoReadback)"
            ),
            "{body}"
        );
        // One data pad in the signature, and no RX one.
        assert!(
            body.contains("    tx: Peri<'d, impl TxPin<peripherals::USART1>>,"),
            "{body}"
        );
        assert!(!body.contains("rx: Peri<"), "{body}");
        assert!(
            body.contains("use embassy_stm32::usart::{HalfDuplexReadback};"),
            "{body}"
        );
        assert!(
            out.init_calls
                .contains("init(p.USART1, pa9_usart1_tx, Irqs)"),
            "{}",
            out.init_calls
        );
    }

    /// On the RX pad it is the other constructor, and the readback flag reaches
    /// the call — the only argument the IDE can get wrong here.
    #[test]
    fn half_duplex_on_rx_with_readback() {
        let pins = vec![mk("PA10", PinFunction::UsartRx(1))];
        let cfg = UsartModuleConfig {
            direction: UsartDirection::HalfDuplexOnRx,
            half_duplex_readback: true,
            mode: UsartMode::Dma,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&pins, cfg);
        let body = &out.config_files[0].1;
        assert!(
            body.contains(
                "Uart::new_half_duplex_on_rx(usart, rx, tx_dma, rx_dma, irqs, get_config(), HalfDuplexReadback::Readback)"
            ),
            "{body}"
        );
        // Still a full-duplex VALUE (both halves), so both DMA channels are
        // taken even though there is one pad.
        assert_eq!(out.dma_uses.len(), 2, "{:?}", out.dma_uses);
    }

    /// Swap / invert reach `get_config` as consts, all three always, where the
    /// chip has them - so clearing a box changes a value, never a name.
    #[test]
    fn line_extras_are_always_named_where_the_chip_has_them() {
        let plain = run(&full_duplex(), UsartModuleConfig::new(1));
        let body = &plain.config_files[0].1;
        for want in [
            "pub const SWAP_RX_TX: bool = false;",
            "pub const INVERT_TX: bool = false;",
            "pub const INVERT_RX: bool = false;",
            "    config.swap_rx_tx = SWAP_RX_TX;",
            "    config.invert_tx = INVERT_TX;",
            "    config.invert_rx = INVERT_RX;",
        ] {
            assert!(body.contains(want), "{want}:\n{body}");
        }
        let end = body.find("// <<< GENERATED END >>>").unwrap();
        assert!(
            body[..end].contains("SWAP_RX_TX"),
            "the names are in the block"
        );

        let cfg = UsartModuleConfig {
            swap_rx_tx: true,
            invert_rx: true,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&full_duplex(), cfg.clone());
        let body = &out.config_files[0].1;
        assert!(
            body.contains("pub const SWAP_RX_TX: bool = true;"),
            "{body}"
        );
        assert!(
            body.contains("    config.swap_rx_tx = SWAP_RX_TX;"),
            "{body}"
        );
        assert!(body.contains("    config.invert_rx = INVERT_RX;"), "{body}");
        assert!(body.contains("pub const INVERT_RX: bool = true;"), "{body}");
        // Only what was asked for: TX inversion was left off.
        assert!(
            body.contains("pub const INVERT_TX: bool = false;"),
            "{body}"
        );
        // The editable half is the plain project's: a box changes a number.
        let tail = |b: &str| b[b.find("// <<< GENERATED END >>>").unwrap()..].to_owned();
        assert_eq!(tail(body), tail(&plain.config_files[0].1));

        // Same config on an OLD USART: the fields do not exist there, so nothing
        // is emitted even though the module still carries the choice.
        let pins = full_duplex();
        let refs: Vec<&Pin> = pins.iter().collect();
        let old = async_peripherals(
            "stm32f4",
            ChipData {
                i2s: true,
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci2_v1_2_Cube"), // F411 — usart_v2
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
                raw: &Default::default(),
            },
            &refs,
            &[(1u8, cfg)].into_iter().collect(),
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
        );
        let body = &old.config_files[0].1;
        assert!(!body.contains("swap_rx_tx"), "{body}");
        assert!(!body.contains("SWAP_RX_TX"), "{body}");
    }

    /// RX-only keeps the ring buffer, because a bare `UartRx` has no
    /// `embedded_io_async::Read`.
    #[test]
    fn rx_only_keeps_the_ring_buffer() {
        let pins = vec![mk("PA10", PinFunction::UsartRx(1))];
        let cfg = UsartModuleConfig {
            mode: UsartMode::Dma,
            direction: UsartDirection::RxOnly,
            ..UsartModuleConfig::new(1)
        };
        let out = run(&pins, cfg);
        let body = &out.config_files[0].1;
        assert!(
            body.contains("UartRx::new(usart, rx, rx_dma, irqs, get_config())"),
            "{body}"
        );
        assert!(body.contains("-> RingBufferedUartRx<'d>"), "{body}");
        assert!(
            out.init_calls
                .contains("init(p.USART1, pa10_usart1_rx, p.DMA"),
            "{}",
            out.init_calls
        );
    }
}
