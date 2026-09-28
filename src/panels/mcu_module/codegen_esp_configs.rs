//! Per-peripheral init modules for ESP projects — `src/pins/configs/*.rs`.
//!
//! The ESP twin of [`super::codegen::stm32::config_files`]. Same deal as on
//! STM32: `main.rs` binds the pins and hands them to a small module that owns
//! the peripheral's settings and its `init(...)`, so the bus setup is one
//! readable place the user can edit — instead of a builder chain wedged between
//! the GEN markers where every edit is overwritten.
//!
//! Layout of a generated file:
//!
//! ```text
//! use esp_hal::…;                  ← the consts below name these types
//!
//! // <<< GENERATED>>>
//! const BAUDRATE: u32 = 115200;    ← from the Virtual Module, re-written on change
//! // <<< GENERATED END >>>
//!
//! pub fn init<'d>(…) -> Uart<'d, Blocking> { … }
//! pub fn init_async<'d>(…) -> Uart<'d, Async> { … }   ← Async runtime only
//! // ── Using UART0 … ──               ← commented example, matches the runtime
//! ```
//!
//! `init` is GENERIC over the pins (`impl PeripheralInput<'d>` /
//! `PeripheralOutput<'d>`, the bounds esp-hal's own builders take), so the file
//! is identical for every chip in the family — only the instance number differs,
//! and the concrete pins are chosen at the call site in `main.rs`. That is also
//! what keeps the pins VISIBLE in `main.rs`: they stay `let gpio5_… =
//! peripherals.GPIO5; // USART1  RX` lines, which is how a reopened project
//! restores them (see [`super::codegen::parse_main_rs`]).
//!
//! The signature mirrors what the canvas WIRED: an SPI with no MISO gets an
//! `init` without a `miso` parameter, rather than one that ignores it. A
//! parameter nobody passes is a warning in the user's build and a lie about
//! which pins the peripheral uses.
//!
//! Real-compile verified on ESP32-C3 / esp-hal 1.1, both runtimes.

use super::codegen_esp::EspRuntime;
use super::modules::{
    AsyncBusMode, CanModuleConfig, DacModuleConfig, I2cModuleConfig, I2sDirection, I2sFormat,
    I2sModuleConfig, I2sStandard, LcdCamMode, LcdCamModuleConfig, McpwmModuleConfig, Parity,
    ParlIoModuleConfig, PcntModuleConfig, RmtModuleConfig, SpiModuleConfig, StopBits,
    TimerModuleConfig, TouchModuleConfig, UsartDirection, UsartModuleConfig, UsbModuleConfig,
};
use std::collections::BTreeMap;

/// Marker pair bounding the auto-updated half of a config file. Identical to the
/// STM32 templates so `sync_config_files` splices both the same way.
const GEN_BEGIN: &str = "// <<< GENERATED>>>";
const GEN_END: &str = "// <<< GENERATED END >>>";

/// `use` items, then the marker block around `consts`, then the user's `body`,
/// then a commented usage `example` at the very end of the file.
///
/// The example is COMMENTED on purpose: it names a handle (`_uart0`) that only
/// exists in `main.rs`, so as code it would not compile here — it is there to be
/// read and copied into the loop. It also tracks the runtime: a Runtime /
/// Init-API Apply rewrites config files wholesale (`sync_config_files(force)`),
/// so switching Blocking ⇄ Async replaces the example along with the `init` it
/// describes.
fn file(uses: &str, consts: &str, body: &str, example: &str) -> String {
    format!(
        "{uses}\n\
         {GEN_BEGIN}\n\
         // Peripheral config (from the Virtual Module) — auto-updated; edit in the module.\n\
         {consts}{GEN_END}\n\
         \n\
         {body}\
         \n\
         {example}"
    )
}

/// Frame a usage snippet as a comment block. Each line gets `// ` so the whole
/// thing is inert until the user copies it out.
fn example_block(title: &str, lines: &[String]) -> String {
    let mut s = format!("// ── {title} ──\n");
    for l in lines {
        if l.is_empty() {
            s.push_str("//\n");
        } else {
            s.push_str(&format!("// {l}\n"));
        }
    }
    s
}

/// Pick the example for the runtime and substitute the handle name.
fn example_for(
    title: &str,
    handle: &str,
    blocking: &[&str],
    asyncs: &[&str],
    rt: EspRuntime,
) -> String {
    let src = if rt == EspRuntime::Async {
        asyncs
    } else {
        blocking
    };
    let lines: Vec<String> = src.iter().map(|l| l.replace("{H}", handle)).collect();
    example_block(
        &format!(
            "{title} ({})",
            if rt == EspRuntime::Async {
                "async"
            } else {
                "blocking"
            }
        ),
        &lines,
    )
}

/// One `    <name>: <bound>,` line per wired signal, in `spec` order.
fn params_for(sigs: &[&str], spec: &[(&str, &str)]) -> String {
    spec.iter()
        .filter(|(s, _)| sigs.contains(s))
        .map(|(name, bound)| format!("    {name}: {bound},\n"))
        .collect()
}

/// One `        .<method>(<name>)` link per wired signal, in `spec` order.
fn chain_for(sigs: &[&str], spec: &[(&str, &str)]) -> String {
    spec.iter()
        .filter(|(s, _)| sigs.contains(s))
        .map(|(name, method)| format!("        .{method}({name})\n"))
        .collect()
}

/// The `init_async` twin of a blocking `init`, emitted ONLY on the Async
/// runtime — an EXTRA function, never a replacement: one bus can stay blocking
/// while another awaits, and dropping `init` would break code already calling it.
///
/// esp-hal builds both the same way; `.into_async()` at the end swaps the mode
/// type (`Uart<'d, Blocking>` → `Uart<'d, Async>`) and with it the method set
/// (`write` → `write_async`, …). No DMA channels, no extra parameters and no new
/// dependency — `Async` is esp-hal's own marker. Verified under `#[esp_rtos::main]`.
fn async_twin(doc: &str, params: &str, ty: &str, ctor: &str, chain: &str) -> String {
    format!(
        "\n\
         /// {doc}\n\
         ///\n\
         /// Same construction as `init`, then `.into_async()` — the methods become\n\
         /// `*_async` and `.await`-able on the embassy executor.\n\
         pub fn init_async<'d>(\n\
         {params}) -> {ty}<'d, Async> {{\n\
         {ctor}\
         {chain}\x20       .into_async()\n\
         }}\n"
    )
}

/// The `use esp_hal::…;` mode import — both markers are needed once `init_async`
/// exists beside `init`.
fn mode_import(rt: EspRuntime) -> &'static str {
    if rt == EspRuntime::Async {
        "use esp_hal::{Async, Blocking};\n"
    } else {
        "use esp_hal::Blocking;\n"
    }
}

// ── UART ─────────────────────────────────────────────────────────────────────

fn data_bits_variant(bits: u8) -> &'static str {
    match bits {
        5 => "_5",
        6 => "_6",
        7 => "_7",
        _ => "_8",
    }
}

fn parity_variant(p: Parity) -> &'static str {
    match p {
        Parity::None => "None",
        Parity::Even => "Even",
        Parity::Odd => "Odd",
    }
}

fn stop_bits_variant(s: StopBits) -> &'static str {
    match s {
        StopBits::One => "_1",
        StopBits::Two => "_2",
    }
}

/// One UART instance, in the direction and with the flow control the module
/// asked for.
///
/// # A direction is a different TYPE
///
/// `Uart` is the two-way port. One direction is `UartTx` or `UartRx`, built by
/// its own constructor — not a `Uart` with a pad left off. That is why the
/// return type changes with the direction rather than the pad list shrinking.
///
/// # Flow control is a config AND a pad
///
/// `HwFlowControl` in the config turns the hardware on; `.with_cts()` and
/// `.with_rts()` route the pads. Setting one without the other is half a
/// feature, so both are emitted together or neither is.
///
/// A half of the port has only the pad it drives: `UartTx` takes RTS and
/// `UartRx` takes CTS. There is no single-wire mode in esp-hal at all — see
/// `UsartDirection::options_for`.
fn uart_file(n: u8, sigs: &[&str], cfg: Option<&UsartModuleConfig>, rt: EspRuntime) -> String {
    let d = UsartModuleConfig::new(n);
    let c = cfg.unwrap_or(&d);
    let dir = c.direction;
    let (cts, rts) = (c.flow.needs_cts(), c.flow.needs_rts());

    let mut consts = format!(
        "pub const BAUDRATE: u32 = {};\n\
         pub const DATA_BITS: DataBits = DataBits::{};\n\
         pub const PARITY: Parity = Parity::{};\n\
         pub const STOP_BITS: StopBits = StopBits::{};\n",
        c.baud_rate,
        data_bits_variant(c.data_bits),
        parity_variant(c.parity),
        stop_bits_variant(c.stop_bits),
    );
    if cts || rts {
        consts.push_str(&format!(
            "// Hardware flow control. The RTS threshold is how many bytes may\n\
             // still arrive after it is asserted - the sender needs the slack.\n\
             pub const FLOW: HwFlowControl = HwFlowControl {{\n\
             \x20   cts: CtsConfig::{},\n\
             \x20   rts: RtsConfig::{},\n\
             }};\n",
            if cts { "Enabled" } else { "Disabled" },
            if rts { "Enabled(8)" } else { "Disabled" },
        ));
    }

    // `UartTx` needs only the TX pad, `UartRx` only RX — and each takes at most
    // the one flow pad it drives.
    let (driver, want): (&str, Vec<(&str, &str)>) = match dir {
        UsartDirection::TxOnly => (
            "UartTx",
            [("tx", "impl PeripheralOutput<'d>")]
                .into_iter()
                .chain(rts.then_some(("rts", "impl PeripheralOutput<'d>")))
                .collect(),
        ),
        UsartDirection::RxOnly => (
            "UartRx",
            [("rx", "impl PeripheralInput<'d>")]
                .into_iter()
                .chain(cts.then_some(("cts", "impl PeripheralInput<'d>")))
                .collect(),
        ),
        _ => (
            "Uart",
            [
                ("rx", "impl PeripheralInput<'d>"),
                ("tx", "impl PeripheralOutput<'d>"),
            ]
            .into_iter()
            .chain(cts.then_some(("cts", "impl PeripheralInput<'d>")))
            .chain(rts.then_some(("rts", "impl PeripheralOutput<'d>")))
            .collect(),
        ),
    };
    let params = format!("    uart: impl Instance + 'd,\n{}", params_for(sigs, &want));
    let chain_spec: Vec<(&str, &str)> = want
        .iter()
        .map(|(name, _)| {
            (
                *name,
                match *name {
                    "rx" => "with_rx",
                    "tx" => "with_tx",
                    "cts" => "with_cts",
                    _ => "with_rts",
                },
            )
        })
        .collect();
    let chain = chain_for(sigs, &chain_spec);

    let flow_line = if cts || rts {
        "\x20       .with_hw_flow_ctrl(FLOW)\n"
    } else {
        ""
    };
    let ctor = format!(
        "    let config = Config::default()\n\
         \x20       .with_baudrate(BAUDRATE)\n\
         \x20       .with_data_bits(DATA_BITS)\n\
         \x20       .with_parity(PARITY)\n\
         {flow_line}\
         \x20       .with_stop_bits(STOP_BITS);\n\
         \x20   // `unwrap`: these values come from the Virtual Module's UI, which\n\
         \x20   // range-limits them - a failure here is a bug in the generator,\n\
         \x20   // not a runtime condition the firmware could recover from.\n\
         \x20   {driver}::new(uart, config)\n\
         \x20       .unwrap()\n"
    );

    let what = match dir {
        UsartDirection::TxOnly => "transmit only",
        UsartDirection::RxOnly => "receive only",
        _ => "blocking driver",
    };
    let mut body = format!(
        "/// UART{n} — {what}.\n\
         ///\n\
         /// Generic over the pins so this file never names a GPIO: `main.rs`\n\
         /// passes the ones wired on the Pins canvas.\n\
         pub fn init<'d>(\n\
         {params}) -> {driver}<'d, Blocking> {{\n\
         {ctor}\
         {chain}}}\n"
    );
    if rt == EspRuntime::Async {
        body.push_str(&async_twin(
            &format!("UART{n} — async driver."),
            &params,
            driver,
            &ctor,
            &chain,
        ));
    }

    let h = format!("_uart{n}");
    let send = vec![
        "In main.rs, after the init above:".to_owned(),
        String::new(),
        format!("    {h}.write(b\"hello\\r\\n\").ok();"),
        format!("    {h}.flush().ok();"),
    ];
    let recv = vec![
        "In main.rs, after the init above:".to_owned(),
        String::new(),
        "    let mut buf = [0u8; 32];".to_owned(),
        format!("    if let Ok(len) = {h}.read_buffered(&mut buf) {{"),
        "        let _ = &buf[..len];".to_owned(),
        "    }".to_owned(),
    ];
    let both = {
        let mut v = send.clone();
        v.push(String::new());
        v.extend(recv[2..].iter().cloned());
        v
    };
    let lines = match dir {
        UsartDirection::TxOnly => send,
        UsartDirection::RxOnly => recv,
        _ => both,
    };
    let mut lines: Vec<String> = lines;
    if cts || rts {
        lines.push(String::new());
        lines.push("// Flow control is on: the peripheral holds off by itself,".to_owned());
        lines.push("// so nothing here changes - that is the point of it.".to_owned());
    }
    let example = example_block(&format!("Using UART{n}"), &lines);

    file(
        &format!(
            "{}\n\
             use esp_hal::gpio::interconnect::{};\n\
             use esp_hal::uart::{{Config, DataBits, Instance, Parity, StopBits, {driver}{}}};\n",
            // One combined import when both markers appear; a single item
            // takes no braces, which is the shape every other file uses.
            if rt == EspRuntime::Async {
                "use esp_hal::{Async, Blocking};"
            } else {
                "use esp_hal::Blocking;"
            },
            // Derived from the parameters actually emitted: a transmit-only
            // port binds no input, and a receive-only one no output.
            match (
                params.contains("PeripheralInput"),
                params.contains("PeripheralOutput"),
            ) {
                (true, true) => "{PeripheralInput, PeripheralOutput}",
                (true, false) => "PeripheralInput",
                _ => "PeripheralOutput",
            },
            if cts || rts {
                ", CtsConfig, HwFlowControl, RtsConfig"
            } else {
                ""
            },
        ),
        &consts,
        &body,
        &example,
    )
}

// ── SPI (slave) ─────────────────────────────────────────────────────────────

/// One SPI instance as the SLAVE end of the bus.
///
/// # A different driver, not a flag
///
/// `esp_hal::spi::slave::Spi` is its own type in its own module. It takes no
/// frequency — the master supplies the clock — and its pin directions are the
/// mirror of the master's: SCK, MOSI and CS come IN, MISO goes out.
///
/// # DMA is not optional here
///
/// esp-hal's slave "can only be used with DMA": there is no CPU path and no
/// blocking transfer, because the master decides when bytes move. So this file
/// always takes a channel, and a project with none left gets no slave at all.
///
/// # No async twin
///
/// The driver has no `into_async`. Waiting is done on the TRANSFER — `wait()`
/// or `is_done()` — which is the same on either runtime.
fn spi_slave_file(n: u8, sigs: &[&str], cfg: Option<&SpiModuleConfig>, rt: EspRuntime) -> String {
    let mode = cfg.map_or(1, |c| c.mode).min(3);
    let consts = format!(
        "pub const MODE: Mode = Mode::_{mode}; // CPOL/CPHA - the MASTER's choice\n\
         // Both directions move at once on a full-duplex bus, so one size.\n\
         pub const BUFFER_BYTES: usize = 4096;\n"
    );
    // The mirror image of the master's bounds: only MISO is driven.
    let params = format!(
        "    spi: impl Instance + 'd,\n\
         {}\
         \x20   dma: impl DmaChannelFor<AnySpi<'d>>,\n",
        params_for(
            sigs,
            &[
                ("sck", "impl PeripheralInput<'d>"),
                ("mosi", "impl PeripheralInput<'d>"),
                ("miso", "impl PeripheralOutput<'d>"),
                ("cs", "impl PeripheralInput<'d>"),
            ],
        )
    );
    let chain = chain_for(
        sigs,
        &[
            ("sck", "with_sck"),
            ("mosi", "with_mosi"),
            ("miso", "with_miso"),
            ("cs", "with_cs"),
        ],
    );

    let body = format!(
        "/// SPI{n} — the SLAVE end, on DMA.\n\
         ///\n\
         /// No frequency: the master clocks this bus. Nothing moves until the\n\
         /// master asserts CS, which is why there is no blocking transfer.\n\
         ///\n\
         /// The BUFFERS come back with the driver: unlike the master's, this\n\
         /// driver takes them per transfer rather than holding them, so they\n\
         /// have to live somewhere the caller can reach.\n\
         pub fn init<'d>(\n\
         {params}) -> (SpiDma<'d, Blocking>, DmaRxBuf, DmaTxBuf) {{\n\
         \x20   let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) =\n\
         \x20       dma_buffers!(BUFFER_BYTES);\n\
         \x20   let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();\n\
         \x20   let dma_tx = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();\n\
         \x20   let spi = Spi::new(spi, MODE)\n\
         {chain}\x20       .with_dma(dma);\n\
         \x20   (spi, dma_rx, dma_tx)\n\
         }}\n"
    );

    let example = example_block(
        &format!("Using SPI{n} (slave)"),
        &[
            "The master drives it. `transfer` takes BOTH buffers and hands them".to_owned(),
            "back with the driver when the master has clocked the bytes through:".to_owned(),
            String::new(),
            format!("    let (spi, rx, mut tx) = _spi{n};   // as generated above"),
            "    tx.as_mut_slice().fill(0xA5);".to_owned(),
            "    let transfer = spi.transfer(8, rx, 8, tx).unwrap();".to_owned(),
            "    let (spi, (rx, tx)) = transfer.wait();".to_owned(),
            String::new(),
            "// Nothing happens at all until the master asserts CS.".to_owned(),
        ],
    );

    let _ = rt; // the slave driver has no async twin - see the note above.
    file(
        "use esp_hal::Blocking;\n\
         use esp_hal::dma::{DmaChannelFor, DmaRxBuf, DmaTxBuf};\n\
         use esp_hal::dma_buffers;\n\
         use esp_hal::gpio::interconnect::{PeripheralInput, PeripheralOutput};\n\
         use esp_hal::spi::Mode;\n\
         use esp_hal::spi::slave::dma::SpiDma;\n\
         use esp_hal::spi::slave::{AnySpi, Instance, Spi};\n",
        &consts,
        &body,
        &example,
    )
}

// ── SPI ──────────────────────────────────────────────────────────────────────

fn spi_file(n: u8, sigs: &[&str], cfg: Option<&SpiModuleConfig>, rt: EspRuntime) -> String {
    if cfg.is_some_and(|c| c.role.is_slave()) {
        return spi_slave_file(n, sigs, cfg, rt);
    }
    let mut consts = format!(
        "pub const FREQUENCY_HZ: u32 = {};\npub const MODE: Mode = Mode::_{}; // CPOL/CPHA, 0..=3\n",
        cfg.map_or(1_000_000, |c| c.clock_hz),
        cfg.map_or(0, |c| c.mode).min(3),
    );
    let params = format!(
        "    spi: impl Instance + 'd,\n{}",
        params_for(
            sigs,
            &[
                ("sck", "impl PeripheralOutput<'d>"),
                ("mosi", "impl PeripheralOutput<'d>"),
                ("miso", "impl PeripheralInput<'d>"),
                ("cs", "impl PeripheralOutput<'d>"),
            ],
        )
    );
    let chain = chain_for(
        sigs,
        &[
            ("sck", "with_sck"),
            ("mosi", "with_mosi"),
            ("miso", "with_miso"),
            ("cs", "with_cs"),
        ],
    );
    // Only meaningful on the DMA path, and only emitted there: an unused
    // constant in a generated file is a warning in the user's project.
    if cfg.is_some_and(|c| c.async_mode == AsyncBusMode::AsyncDma) {
        consts.push_str(
            "pub const DMA_BUFFER_BYTES: usize = 4096; // per direction, static-backed
",
        );
    }
    let ctor = "    let config = Config::default()\n\
                \x20       .with_frequency(Rate::from_hz(FREQUENCY_HZ))\n\
                \x20       .with_mode(MODE);\n\
                \x20   Spi::new(spi, config)\n\
                \x20       .unwrap()\n";
    // The MODULE decides, not the runtime: `with_dma` is on the blocking
    // driver too, so `init` itself is the DMA form when one was asked for.
    let on_dma = cfg.is_some_and(|c| c.async_mode == AsyncBusMode::AsyncDma);
    let mut body = if on_dma {
        spi_dma_fn(n, &params, ctor, &chain, "init", "Blocking", "")
    } else {
        format!(
            "/// SPI{n} master — blocking driver.\n\
             ///\n\
             /// Takes exactly the lines wired on the Pins canvas: no MISO wired\n\
             /// means no `miso` parameter here.\n\
             pub fn init<'d>(\n\
             {params}) -> Spi<'d, Blocking> {{\n\
             {ctor}\
             {chain}}}\n"
        )
    };
    if rt == EspRuntime::Async {
        body.push('\n');
        if on_dma {
            body.push_str(&spi_dma_fn(
                n,
                &params,
                ctor,
                &chain,
                "init_async",
                "Async",
                "\n\x20       .into_async()",
            ));
        } else {
            body.push_str(&async_twin(
                &format!("SPI{n} master — async driver."),
                &params,
                "Spi",
                ctor,
                &chain,
            ));
        }
    }
    let example = if on_dma {
        example_for(
            &format!("Using SPI{n}"),
            &format!("_spi{n}"),
            &[
                "In main.rs, after the init above. The handle is a `SpiDmaBus`, not",
                "an `Spi`: the GDMA moves the bytes, and each call returns once it",
                "has finished.",
                "",
                "    // Write only",
                "    {H}.write(&[0x9F]).ok();",
                "",
                "    // Read only",
                "    let mut rx = [0u8; 3];",
                "    {H}.read(&mut rx).ok();",
                "",
                "    // Full duplex in place: `buf` is sent, then overwritten",
                "    let mut buf = [0x9F, 0x00, 0x00, 0x00];",
                "    {H}.transfer_in_place(&mut buf).ok();",
                "",
                "    // Or two buffers, which need NOT be the same length. Note the",
                "    // order: the one you read into comes first.",
                "    //     {H}.transfer(&mut rx, &[0x9F]).ok();",
                "",
                "    // Transfers longer than DMA_BUFFER_BYTES are chunked for you.",
                "    // CS is NOT toggled for you unless it was wired on the canvas.",
            ],
            &[
                "main.rs calls `init_async`, so the handle is a `SpiDmaBus<'_, Async>`.",
                "There is no flush to call: every one of these waits for the GDMA.",
                "",
                "    // Write only",
                "    {H}.write_async(&[0x9F]).await.ok();",
                "",
                "    // Full duplex in place: `buf` is sent, then overwritten",
                "    let mut buf = [0x9F, 0x00, 0x00, 0x00];",
                "    {H}.transfer_in_place_async(&mut buf).await.ok();",
                "",
                "    // Two buffers, the one you read into first:",
                "    //     let mut rx = [0u8; 3];",
                "    //     {H}.transfer_async(&mut rx, &[0x9F]).await.ok();",
                "",
                "    // CS is NOT toggled for you unless it was wired on the canvas.",
            ],
            rt,
        )
    } else {
        example_for(
            &format!("Using SPI{n}"),
            &format!("_spi{n}"),
            &[
                "In main.rs, after the init above:",
                "",
                "    // Write only",
                "    {H}.write(&[0x9F]).ok();",
                "",
                "    // Read only",
                "    let mut rx = [0u8; 3];",
                "    {H}.read(&mut rx).ok();",
                "",
                "    // Full duplex: `buf` is sent, and is overwritten by what comes back",
                "    let mut buf = [0x9F, 0x00, 0x00, 0x00];",
                "    {H}.transfer(&mut buf).ok();",
                "",
                "    // CS is NOT toggled for you unless it was wired on the canvas —",
                "    // drive it yourself around a transaction if you kept it as a GPIO.",
            ],
            &[
                "main.rs calls `init_async`, so the handle is a `Spi<'_, Async>`.",
                "esp-hal's async SPI surface is narrower than the blocking one: it is",
                "full-duplex in place, plus a flush. Use `init` for write-only/read-only.",
                "",
                "    // `buf` is sent, then overwritten by the reply",
                "    let mut buf = [0x9F, 0x00, 0x00, 0x00];",
                "    {H}.transfer_in_place_async(&mut buf).await.ok();",
                "    {H}.flush_async().await.ok();",
                "",
                "    // CS is NOT toggled for you unless it was wired on the canvas.",
            ],
            rt,
        )
    };
    file(
        &format!(
            "{}\
             {}\
             use esp_hal::gpio::interconnect::{{PeripheralInput, PeripheralOutput}};\n\
             use esp_hal::spi::Mode;\n\
             use esp_hal::spi::master::{{Config, Instance, Spi}};\n\
             use esp_hal::time::Rate;\n",
            mode_import(rt),
            if on_dma {
                "use esp_hal::dma::{DmaChannelFor, DmaRxBuf, DmaTxBuf};\n\
                 use esp_hal::dma_buffers;\n\
                 use esp_hal::spi::master::{AnySpi, SpiDmaBus};\n"
            } else {
                ""
            },
        ),
        &consts,
        &body,
        &example,
    )
}

/// The bus moved by the GDMA instead of by the CPU.
///
/// # Not a flag on [`async_twin`], and not async-only
///
/// `.into_async()` alone gives an `Spi<'_, Async>` that still copies every byte
/// through the CPU. Going to DMA changes the RETURN TYPE — `SpiDmaBus` — and
/// needs two owned descriptor buffers built before the bus exists, so there is
/// no line to append: the whole body differs.
///
/// And it is NOT the async driver's privilege. `with_dma` is on
/// `impl Spi<'d, Blocking>` and hands back a `SpiDma<'d, Blocking>`, so this
/// emits `init` on a blocking project and `init_async` on an async one — same
/// body, one `.into_async()` apart. It used to be reachable only on async,
/// which quietly refused a blocking project the channel it had asked for.
///
/// The buffers are `static`-backed by `dma_buffers!`, which is why the size is a
/// constant here rather than a parameter: they must outlive the transfer, and a
/// caller-supplied slice could not.
fn spi_dma_fn(
    n: u8,
    params: &str,
    ctor: &str,
    chain: &str,
    fname: &str,
    dm: &str,
    tail: &str,
) -> String {
    format!(
        "/// SPI{n} master — {} driver on DMA.\n\
         ///\n\
         /// Same construction as the CPU form, then `.with_dma()` and a pair of\n\
         /// DMA buffers. The GDMA moves the bytes, so a transfer costs the CPU\n\
         /// one wait rather than one interrupt per word.\n\
         ///\n\
         /// The channel comes from main.rs. On this chip any free channel serves\n\
         /// any peripheral, so which one you get is the IDE's choice — see the\n\
         /// DMA card in the Configuration tab, or pin one by hand in the SPI\n\
         /// module.\n\
         pub fn {fname}<'d>(\n\
         {params}\x20   dma: impl DmaChannelFor<AnySpi<'d>>,\n\
         ) -> SpiDmaBus<'d, {dm}> {{\n\
         \x20   let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) =\n\
         \x20       dma_buffers!(DMA_BUFFER_BYTES);\n\
         \x20   let dma_rx = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();\n\
         \x20   let dma_tx = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();\n\
         {ctor}\
         {chain}\x20       .with_dma(dma)\n\
         \x20       .with_buffers(dma_rx, dma_tx){tail}\n\
         }}\n",
        if dm == "Async" { "async" } else { "blocking" },
    )
}

// ── I2S ──────────────────────────────────────────────────────────────────────

/// esp-hal's `Config` constructor for the standard the module asked for.
///
/// `LsbFirst` has no constructor at all: esp-hal builds Philips, MSB-first and
/// the two PCM sync widths, and nothing else. The UI does not offer it on an
/// ESP — see `I2sStandard::options` — so reaching the fallback here means the
/// project carries a setting made for another chip, and Philips is the one to
/// land on rather than silently emitting something that will not compile.
fn i2s_standard_ctor(std: I2sStandard) -> &'static str {
    match std {
        I2sStandard::Philips | I2sStandard::LsbFirst => "new_tdm_philips",
        I2sStandard::MsbFirst => "new_tdm_msb",
        I2sStandard::PcmShortSync => "new_tdm_pcm_short",
        I2sStandard::PcmLongSync => "new_tdm_pcm_long",
    }
}

/// The `DataFormat` variant, for the two widths esp-hal and the IDE both have.
///
/// esp-hal's list is `Data{8,16,32}Channel{8,16,24,32}`-ish and the IDE's is the
/// STM32's four; the overlap is 16-in-16 and 32-in-32. The other two are not
/// offered on an ESP, so this fallback is the same "setting from another chip"
/// case as [`i2s_standard_ctor`].
fn i2s_data_format(fmt: I2sFormat) -> &'static str {
    match fmt {
        I2sFormat::Data32Channel32 | I2sFormat::Data24Channel32 => "Data32Channel32",
        _ => "Data16Channel16",
    }
}

/// One I2S instance as an esp-hal driver, in the direction the module asked for.
///
/// # It is a DMA peripheral, and only a DMA peripheral
///
/// Unlike the UART/SPI/I2C files beside it, there is no non-DMA form to fall
/// back to: `I2s::new` TAKES a channel. So this file always needs one, and
/// `main.rs` always passes one — a project with no channel left does not get a
/// half-working I2S, it gets none (see `dma_plan`).
///
/// # Why `init` hands back the buffer
///
/// `dma_buffers!` makes a `&'static mut [u8]` and a descriptor list. The
/// descriptors go into `.build()` and disappear into the driver; the BUFFER is
/// the one the caller passes to `write_dma_circular` on every transfer, so it
/// has to come back out. Returning it is what keeps the whole allocation in one
/// place instead of asking the user to declare a matching `static` by hand.
fn i2s_file(
    n: u8,
    sigs: &[&str],
    cfg: Option<&I2sModuleConfig>,
    rt: EspRuntime,
    // The ESP32 alone restricts MCLK to three pads, and says so in the TYPE:
    // its `with_mclk` takes `impl ClkPin`, not `impl PeripheralOutput`. The
    // wrong bound is a trait error in the USER's project, not here - which is
    // how it survived until a DAC pushed the harness onto an Xtensa part.
    esp32_mclk: bool,
) -> String {
    let tx = cfg.is_none_or(|c| c.direction == I2sDirection::Transmit);
    let (unit, data_method, data_bound) = if tx {
        ("i2s_tx", "with_dout", "impl PeripheralOutput<'d>")
    } else {
        ("i2s_rx", "with_din", "impl PeripheralInput<'d>")
    };
    let ty = if tx { "I2sTx" } else { "I2sRx" };
    let samples = cfg.map_or(256, |c| c.buffer_len) as usize;
    let consts = format!(
        "pub const SAMPLE_RATE_HZ: u32 = {};\n\
         // Samples are 32-bit at the widest, and the ring holds both channels.\n\
         pub const BUFFER_BYTES: usize = {} * 4 * 2;\n",
        cfg.map_or(48_000, |c| c.sample_rate_hz),
        samples,
    );
    // The signal names the canvas uses are the STM32's (ck/ws/sd/mck); esp-hal
    // calls the first two bclk/ws and the third dout or din by direction.
    let params = format!(
        "    i2s: impl Instance + 'd,\n\
         \x20   dma: impl DmaChannelFor<AnyI2s<'d>>,\n\
         {}",
        params_for(
            sigs,
            &[
                ("ck", "impl PeripheralOutput<'d>"),
                ("ws", "impl PeripheralOutput<'d>"),
                ("sd", data_bound),
                (
                    "mck",
                    if esp32_mclk {
                        "impl ClkPin<'d>"
                    } else {
                        "impl PeripheralOutput<'d>"
                    },
                ),
            ],
        )
    );
    let chain = chain_for(
        sigs,
        &[("ck", "with_bclk"), ("ws", "with_ws"), ("sd", data_method)],
    );
    // MCLK is set on the I2S itself, before it is split into its two halves —
    // it belongs to the block, not to a direction.
    let mclk = if sigs.contains(&"mck") {
        "\n\x20       .with_mclk(mck)"
    } else {
        ""
    };
    let (buffers, descriptors) = if tx {
        ("(_, _, buffer, descriptors)", "0, BUFFER_BYTES")
    } else {
        ("(buffer, descriptors, _, _)", "BUFFER_BYTES, 0")
    };
    let body_for = |name: &str, mode: &str, extra: &str| {
        format!(
            "pub fn {name}<'d>(\n\
             {params}) -> ({ty}<'d, {mode}>, &'static mut [u8]) {{\n\
             \x20   let {buffers} = dma_buffers!({descriptors});\n\
             \x20   let config = Config::{ctor}()\n\
             \x20       .with_sample_rate(Rate::from_hz(SAMPLE_RATE_HZ))\n\
             \x20       .with_data_format(DataFormat::{fmt})\n\
             \x20       .with_channels(Channels::STEREO);\n\
             \x20   let i2s = I2s::new(i2s, dma, config)\n\
             \x20       .unwrap(){mclk}{extra};\n\
             \x20   let driver = i2s\n\
             \x20       .{unit}\n\
             {chain}\x20       .build(descriptors);\n\
             \x20   (driver, buffer)\n\
             }}\n",
            ctor = i2s_standard_ctor(cfg.map_or(I2sStandard::Philips, |c| c.standard)),
            fmt = i2s_data_format(cfg.map_or(I2sFormat::Data16Channel16, |c| c.format)),
        )
    };
    let mut body = format!(
        "/// I2S{n} — blocking driver, {} on DMA.\n\
         ///\n\
         /// Returns the driver AND the ring buffer it moves: the descriptors go\n\
         /// into the driver, the buffer is what every transfer reads or writes.\n\
         {}",
        if tx { "transmitting" } else { "receiving" },
        body_for("init", "Blocking", ""),
    );
    if rt == EspRuntime::Async {
        body.push_str(&format!(
            "\n/// I2S{n} — async driver.\n\
             ///\n\
             /// Same construction, then `.into_async()`: the transfer methods\n\
             /// become `*_async` and `.await`-able on the executor.\n\
             {}",
            body_for("init_async", "Async", "\n\x20       .into_async()"),
        ));
    }
    let example = example_for(
        &format!("Using I2S{n}"),
        &format!("_i2s{n}"),
        &[
            "main.rs hands back the driver and its ring buffer:",
            "",
            "    let (mut {H}, buf) = …;   // as generated above",
            if tx {
                "    let mut transfer = {H}.write_dma_circular(buf).unwrap();"
            } else {
                "    let mut transfer = {H}.read_dma_circular(buf).unwrap();"
            },
            "    loop {",
            "        let n = transfer.available().unwrap();",
            if tx {
                "        if n > 0 { transfer.push(&samples[..n]).ok(); }"
            } else {
                "        if n > 0 { transfer.pop(&mut samples[..n]).ok(); }"
            },
            "    }",
        ],
        &[
            "main.rs calls `init_async`, so the transfer is .await-able:",
            "",
            "    let (mut {H}, buf) = …;   // as generated above",
            if tx {
                "    let mut transfer = {H}.write_dma_circular_async(buf).unwrap();"
            } else {
                "    let mut transfer = {H}.read_dma_circular_async(buf).unwrap();"
            },
            "    transfer.available().await.ok();",
            "",
            "    // `init` is still there if you want this one blocking instead.",
        ],
        rt,
    );
    file(
        &format!(
            "{}\
             use esp_hal::dma::DmaChannelFor;\n\
             use esp_hal::dma_buffers;\n\
             use esp_hal::gpio::interconnect::{{{pads}}};\n\
             use esp_hal::i2s::AnyI2s;\n\
             use esp_hal::i2s::master::{{Channels, {clk_pin}Config, DataFormat, I2s, {ty}, Instance}};\n\
             use esp_hal::time::Rate;\n",
            mode_import(rt),
            // Imported only where it is both needed and used.
            clk_pin = if esp32_mclk && sigs.contains(&"mck") {
                "ClkPin, "
            } else {
                ""
            },
            // Only the direction actually used: an unused import is a
            // warning in the user's project, and a generated file must
            // not raise one.
            pads = if tx {
                "PeripheralOutput"
            } else {
                "PeripheralInput, PeripheralOutput"
            },
        ),
        &consts,
        &body,
        &example,
    )
}

// ── DAC ─────────────────────────────────────────────────────────────────────

/// The chip's DAC channels — an 8-bit level per pad.
///
/// # Two channels, two peripherals
///
/// esp-hal names them `DAC1` and `DAC2`, each with a `T::Pin` fixed by type:
/// `GPIO25`/`GPIO26` on the ESP32, `GPIO17`/`GPIO18` on the S2. So `init` takes
/// the pad, but passing a different one does not compile — which is the whole
/// reason the canvas offers `DacOut` on those two pads and nowhere else.
///
/// # Eight bits, not twelve
///
/// `Dac::write` takes a `u8`. The module stores its resting value the way the
/// STM32's DAC needs it — twelve bits — so it is scaled here rather than
/// clamped, or every value above 255 would land at full scale.
fn dac_file(channels: &[u8], cfg: Option<&DacModuleConfig>, rt: EspRuntime) -> String {
    let d = DacModuleConfig::new(1);
    let c = cfg.unwrap_or(&d);
    let mut consts = String::new();
    for ch in channels {
        consts.push_str(&format!(
            "// The level the pad holds once `init` returns.\n\
             pub const START_OUT{ch}: u8 = {};\n",
            esp_dac_value(c.value_of(*ch)),
        ));
    }

    let params: String = channels
        .iter()
        .map(|ch| {
            format!("    dac{ch}: DAC{ch}<'d>,\n    pin{ch}: <DAC{ch}<'d> as Instance>::Pin,\n")
        })
        .collect();
    let ret: String = channels
        .iter()
        .map(|ch| format!("Dac<'d, DAC{ch}<'d>>"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut steps = String::new();
    for ch in channels {
        steps.push_str(&format!(
            "\x20   let mut out{ch} = Dac::new(dac{ch}, pin{ch});\n\
             \x20   out{ch}.write(START_OUT{ch});\n"
        ));
    }
    let handles = channels
        .iter()
        .map(|ch| format!("out{ch}"))
        .collect::<Vec<_>>()
        .join(", ");
    let ret = if channels.len() == 1 {
        ret
    } else {
        format!("({ret})")
    };
    let handles = if channels.len() == 1 {
        handles
    } else {
        format!("({handles})")
    };

    let body = format!(
        "/// The DAC — {} channel{}, eight bits each.\n\
         ///\n\
         /// Each pad is fixed in silicon; `Dac::new` takes no other.\n\
         pub fn init<'d>(\n\
         {params}) -> {ret} {{\n\
         {steps}\x20   {handles}\n\
         }}\n",
        channels.len(),
        if channels.len() == 1 { "" } else { "s" },
    );

    let first = channels.first().copied().unwrap_or(1);
    let example = example_block(
        "Using the DAC",
        &[
            "One byte in, one voltage out — 0 is ground, 255 is the supply:".to_owned(),
            String::new(),
            format!("    _dac_out{first}.write(128);   // half scale"),
            String::new(),
            "// There is no ramp and no buffer: the pad follows the last value".to_owned(),
            "// written, and holds it.".to_owned(),
        ],
    );

    let _ = rt; // the DAC has no async surface: `write` is a register store.
    file(
        &format!(
            "use esp_hal::analog::dac::{{Dac, Instance}};\n\
             use esp_hal::peripherals::{{{}}};\n",
            channels
                .iter()
                .map(|ch| format!("DAC{ch}"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        &consts,
        &body,
        &example,
    )
}

/// The module's 12-bit resting value, as the eight bits an ESP DAC takes.
///
/// Scaled, not truncated: the field is shared with the STM32 path, where the
/// converter really is twelve bits, so a mid-scale 2048 has to come out as 128
/// rather than as the low byte of 2048 (which is 0).
fn esp_dac_value(v12: u16) -> u8 {
    (u32::from(v12.min(4095)) * 255 / 4095) as u8
}

// ── PARL_IO ─────────────────────────────────────────────────────────────────

/// The parallel port: the sending half, the receiving half, or BOTH.
///
/// # One constructor, one channel, two halves
///
/// `ParlIo::new` consumes the peripheral and takes a SINGLE DMA channel, which
/// it splits into a tx and an rx half itself. That is the one thing that
/// differs from LCD_CAM, whose halves want a channel each — and it is why this
/// file has one `init` rather than two.
///
/// # The halves never share a wire
///
/// The GPIO matrix has separate `PARL_TX_*` and `PARL_RX_*` signals, so the two
/// halves have their own pads, their own width, their own frequency. The
/// constants are prefixed for that reason.
///
/// # Buffers come back with the drivers
///
/// The descriptors go into the driver and the buffer is what each transfer
/// moves, so `init` hands both back for every half it builds.
fn parl_io_file(
    tx: Option<(&ParlIoModuleConfig, bool)>,
    rx: Option<(&ParlIoModuleConfig, bool)>,
    rt: EspRuntime,
) -> String {
    // `(prefix, config, has a valid pad, is the receiving half)`, sending
    // first — the order `main.rs` passes the pads in.
    let halves: Vec<(&str, &ParlIoModuleConfig, bool, bool)> = [
        tx.map(|(c, v)| ("TX", c, v, false)),
        rx.map(|(c, v)| ("RX", c, v, true)),
    ]
    .into_iter()
    .flatten()
    .collect();

    let mut consts = String::new();
    let mut params =
        String::from("    parl_io: PARL_IO<'d>,\n    dma: impl DmaChannelFor<PARL_IO<'d>>,\n");
    let mut rets: Vec<String> = Vec::new();
    let mut gives: Vec<String> = Vec::new();
    let mut descs: Vec<String> = Vec::new();

    for (prefix, c, has_valid, is_rx) in &halves {
        let lanes = c.width.lanes();
        // At sixteen bits the valid signal IS one of the data lines.
        let valid = *has_valid && lanes < 16;
        let lower = prefix.to_lowercase();
        let pad = if *is_rx { "r" } else { "" };
        consts.push_str(&format!(
            "pub const {prefix}_FREQUENCY_HZ: u32 = {};\n\
             pub const {prefix}_BUFFER_BYTES: usize = {};\n",
            c.freq_hz, c.buffer_bytes,
        ));
        let pad_bound = if *is_rx {
            "impl PeripheralInput<'d>"
        } else {
            "impl PeripheralOutput<'d>"
        };
        for lane in 0..lanes {
            params.push_str(&format!("    {pad}d{lane}: {pad_bound},\n"));
        }
        params.push_str(&format!(
            "    {pad}clk: impl Peripheral{}<'d>,\n",
            if *is_rx { "Input" } else { "Output" }
        ));
        if valid {
            params.push_str(&format!(
                "    {pad}valid: impl Peripheral{}<'d>,\n",
                if *is_rx { "Input" } else { "Output" }
            ));
        }

        let data_args: Vec<String> = (0..lanes).map(|l| format!("{pad}d{l}")).collect();
        descs.push(format!(
            "\x20   let {lower}_pins = {}::new({});\n",
            c.width.esp_hal(!*is_rx),
            data_args.join(", "),
        ));
        if valid {
            descs.push(format!(
                "\x20   let {lower}_pins = {}PinConfigWithValidPin::new({lower}_pins, {pad}valid);\n",
                if *is_rx { "Rx" } else { "Tx" },
            ));
        }
        // `ClkInPin` is a TxClkPin — a transmitter clocked from OUTSIDE.
        // The receiver wants `RxClkInPin`, which also names the edge it
        // samples on. Sampling on the wrong one reads the bus
        // mid-transition: garbage that looks like noise, not an error.
        descs.push(if *is_rx {
            format!(
                "\x20   let {lower}_clk = RxClkInPin::new({pad}clk, SampleEdge::{});\n",
                c.sample_edge.esp_hal(),
            )
        } else {
            format!("\x20   let {lower}_clk = ClkOutPin::new({pad}clk);\n")
        });

        let (buffers, sizes, buf_ty) = if *is_rx {
            (
                format!("({lower}_buffer, {lower}_desc, _, _)"),
                format!("{prefix}_BUFFER_BYTES, 0"),
                "DmaRxBuf",
            )
        } else {
            (
                format!("(_, _, {lower}_buffer, {lower}_desc)"),
                format!("0, {prefix}_BUFFER_BYTES"),
                "DmaTxBuf",
            )
        };
        descs.push(format!(
            "\x20   let {buffers} = dma_buffers!({sizes});\n\
             \x20   let {lower}_buf = {buf_ty}::new({lower}_desc, {lower}_buffer).unwrap();\n",
        ));

        rets.push(format!(
            "ParlIo{}<'d, {{DM}}>",
            if *is_rx { "Rx" } else { "Tx" }
        ));
        rets.push(buf_ty.to_owned());
        gives.push(format!("{lower}_driver"));
        gives.push(format!("{lower}_buf"));
    }

    let body_for = |name: &str, mode: &str, into_async: &str| {
        let mut build = String::new();
        for (prefix, c, has_valid, is_rx) in &halves {
            let lower = prefix.to_lowercase();
            let valid = *has_valid && c.width.lanes() < 16;
            let _ = valid;
            // The receiver needs a timeout or a frame never ends; the
            // transmitter has no such knob.
            let extra = if *is_rx {
                "\n\x20       .with_timeout_ticks(0xfff)"
            } else {
                ""
            };
            build.push_str(&format!(
                "\x20   let {lower}_config = {}Config::default()\n\
                 \x20       .with_frequency(Rate::from_hz({prefix}_FREQUENCY_HZ))\n\
                 \x20       .with_bit_order(BitPackOrder::{}){extra};\n",
                if *is_rx { "Rx" } else { "Tx" },
                c.bit_order.esp_hal(),
            ));
        }
        let mut take = String::new();
        for (prefix, _, _, is_rx) in &halves {
            let lower = prefix.to_lowercase();
            take.push_str(&format!(
                "\x20   let {lower}_driver = port.{}.with_config({lower}_pins, {lower}_clk, {lower}_config).unwrap();\n",
                if *is_rx { "rx" } else { "tx" },
            ));
        }
        format!(
            "pub fn {name}<'d>(\n\
             {params}) -> ({}) {{\n\
             {}{build}\
             \x20   let port = ParlIo::new(parl_io, dma).unwrap(){into_async};\n\
             {take}\
             \x20   ({})\n\
             }}\n",
            rets.join(", ").replace("{DM}", mode),
            descs.concat(),
            gives.join(", "),
        )
    };

    let mut body = format!(
        "/// The parallel port — {}.\n\
         ///\n\
         /// Returns each driver AND its DMA buffer: the descriptors go into the\n\
         /// driver, the buffer is what each transfer moves.\n\
         {}",
        halves
            .iter()
            .map(|(_, c, _, is_rx)| format!(
                "{} {} lines at {} Hz",
                if *is_rx { "receiving" } else { "transmitting" },
                c.width.lanes(),
                c.freq_hz
            ))
            .collect::<Vec<_>>()
            .join(" + "),
        body_for("init", "Blocking", ""),
    );
    if rt == EspRuntime::Async {
        body.push_str(&format!(
            "\n/// The same, async.\n{}",
            body_for("init_async", "Async", ".into_async()"),
        ));
    }

    // The handles `main.rs` binds, so the snippet is pasteable rather than a
    // sketch with a `…` in it. A `write`/`read` takes the buffer BY VALUE and
    // the transfer hands it back, which is why they are reassigned.
    let sending = halves.iter().any(|(_, _, _, is_rx)| !*is_rx);
    let (handle, buf, verb, len) = if sending {
        ("_parl", "_parl_buf", "write", "_parl_buf.len()")
    } else {
        (
            "_parl_rx",
            "_parl_rx_buf",
            "read",
            "Some(_parl_rx_buf.len())",
        )
    };
    let fill = if sending {
        format!("    {buf}.as_mut_slice().fill(0xAA);\n")
    } else {
        String::new()
    };
    let sync = vec![
        if sending {
            "Fill the buffer, hand it over, and take it back when done:".to_owned()
        } else {
            "Hand the buffer over and take it back full:".to_owned()
        },
        String::new(),
        fill.trim_end().to_owned(),
        format!("    let t = {handle}.{verb}({len}, {buf}).unwrap();"),
        "    let (result, p, b) = t.wait();".to_owned(),
        format!("    ({handle}, {buf}) = (p, b);"),
        "    result.ok();".to_owned(),
        String::new(),
        "// With BOTH halves built, each has its own handle and its own buffer -".to_owned(),
        "// they share only the peripheral and the ONE DMA channel it splits.".to_owned(),
    ];
    let asyn = [
        "The same, awaited:".to_owned(),
        String::new(),
        format!("    let mut t = {handle}.{verb}({len}, {buf}).unwrap();"),
        "    t.wait_for_done().await;".to_owned(),
        "    let (result, p, b) = t.wait();".to_owned(),
        format!("    ({handle}, {buf}) = (p, b);"),
        "    result.ok();".to_owned(),
    ];
    let sync: Vec<&str> = sync.iter().map(String::as_str).collect();
    let asyn: Vec<&str> = asyn.iter().map(String::as_str).collect();
    let example = example_for("Using the parallel port", handle, &sync, &asyn, rt);

    let mut uses = vec![
        // The mode markers name the return type. `Async` only appears when the
        // async twin is emitted, so it is added with that.
        "use esp_hal::Blocking;".to_owned(),
        "use esp_hal::dma::DmaChannelFor;".to_owned(),
        "use esp_hal::dma_buffers;".to_owned(),
        "use esp_hal::peripherals::PARL_IO;".to_owned(),
        "use esp_hal::time::Rate;".to_owned(),
    ];
    let mut items: Vec<String> = vec!["BitPackOrder".to_owned(), "ParlIo".to_owned()];
    for (_, c, has_valid, is_rx) in &halves {
        let valid = *has_valid && c.width.lanes() < 16;
        items.push(c.width.esp_hal(!*is_rx).to_owned());
        if *is_rx {
            items.extend([
                "RxClkInPin".to_owned(),
                "SampleEdge".to_owned(),
                "ParlIoRx".to_owned(),
                "RxConfig".to_owned(),
            ]);
            uses.push("use esp_hal::dma::DmaRxBuf;".to_owned());
            if valid {
                items.push("RxPinConfigWithValidPin".to_owned());
            }
        } else {
            items.extend([
                "ClkOutPin".to_owned(),
                "ParlIoTx".to_owned(),
                "TxConfig".to_owned(),
            ]);
            uses.push("use esp_hal::dma::DmaTxBuf;".to_owned());
            if valid {
                items.push("TxPinConfigWithValidPin".to_owned());
            }
        }
    }
    let ins = halves.iter().any(|(_, _, _, is_rx)| *is_rx);
    let outs = halves.iter().any(|(_, _, _, is_rx)| !*is_rx);
    uses.push(format!(
        "use esp_hal::gpio::interconnect::{};",
        match (ins, outs) {
            (true, true) => "{PeripheralInput, PeripheralOutput}".to_owned(),
            (true, false) => "PeripheralInput".to_owned(),
            _ => "PeripheralOutput".to_owned(),
        }
    ));
    if rt == EspRuntime::Async {
        uses.push("use esp_hal::Async;".to_owned());
    }
    items.sort();
    items.dedup();
    uses.push(format!("use esp_hal::parl_io::{{{}}};", items.join(", ")));
    uses.sort();
    uses.dedup();

    file(&format!("{}\n", uses.join("\n")), &consts, &body, &example)
}

// ── MCPWM ───────────────────────────────────────────────────────────────────

/// One MCPWM unit: its timers, its operators, and the pads they drive.
///
/// # A timer per operator
///
/// The unit has THREE timers and three operators, and `Operator::set_timer`
/// takes the timer index as a const parameter — any operator can be pointed at
/// any timer. That is what puts two motors at two frequencies on one unit, and
/// it is why the frequency and the period are per TIMER here rather than per
/// unit. Only the timers something actually runs on are started.
///
/// # The timers come back
///
/// A `Timer` owns the guard that holds the MCPWM clock on; a `PwmPin` does not.
/// Returning only the pins compiles and silently kills every output, so `init`
/// hands the started timers back and `main.rs` keeps them.
///
/// # `with_pin_a` consumes the operator
///
/// An operator whose two outputs are both wired has to build them together with
/// `with_pins`; reaching for `with_pin_a` and then `with_pin_b` does not
/// compile, because the first call moved the operator.
fn mcpwm_file(
    unit: u8,
    outputs: &[(u8, bool)],
    cfg: Option<&McpwmModuleConfig>,
    source_mhz: u32,
) -> String {
    let d = McpwmModuleConfig::new(unit);
    let c = cfg.unwrap_or(&d);
    let peri = format!("MCPWM{unit}");

    let mut ops: Vec<u8> = outputs.iter().map(|(op, _)| *op).collect();
    ops.sort_unstable();
    ops.dedup();
    let timers = c.timers_used(&ops);

    let mut consts = String::new();
    for t in &timers {
        consts.push_str(&format!(
            "pub const FREQUENCY_HZ_T{t}: u32 = {};\n\
             // Timer {t} counts 0..=PERIOD_T{t}, so a duty lands on one of PERIOD_T{t}+1 steps. Public: main.rs sets duty in terms of it.\n\
             pub const PERIOD_T{t}: u16 = {};\n",
            c.timer_freq_hz(*t),
            c.timer_period(*t),
        ));
    }
    for (op, b) in outputs {
        consts.push_str(&format!(
            "pub const TIMESTAMP_OP{op}{}: u16 = {}; // {:.2} % of timer {}\n",
            if *b { "B" } else { "A" },
            c.timestamp_of(*op, *b),
            f64::from(c.duty_x100_of(*op, *b)) / 100.0,
            c.timer_of(*op),
        ));
    }

    let name = |op: u8, b: bool| format!("op{op}{}", if b { "b" } else { "a" });
    let params: String = outputs
        .iter()
        .map(|(op, b)| format!("    {}: impl PeripheralOutput<'d>,\n", name(*op, *b)))
        .collect();
    // The const parameter is IS_A, so it is the NEGATION of "this is the B
    // output" — inverting it silently swaps which pad each handle drives.
    let ret: String = timers
        .iter()
        .map(|t| format!("Timer<{t}, {peri}<'d>>, "))
        .chain(
            outputs
                .iter()
                .map(|(op, b)| format!("PwmPin<'d, {peri}<'d>, {op}, {}>, ", !b)),
        )
        .collect::<String>()
        .trim_end_matches(", ")
        .to_owned();

    // Each operator is pointed at ITS timer once, however many of its two
    // outputs are wired.
    let links: String = ops
        .iter()
        .map(|op| {
            format!(
                "\x20   mcpwm.operator{op}.set_timer(&mcpwm.timer{});\n",
                c.timer_of(*op)
            )
        })
        .collect();

    let mut pins = String::new();
    for op in &ops {
        let a = outputs.contains(&(*op, false));
        let b = outputs.contains(&(*op, true));
        match (a, b) {
            (true, true) => pins.push_str(&format!(
                "\x20   let (mut {a_n}, mut {b_n}) = mcpwm.operator{op}.with_pins(\n\
                 \x20       {a_n},\n\
                 \x20       PwmPinConfig::UP_ACTIVE_HIGH,\n\
                 \x20       {b_n},\n\
                 \x20       PwmPinConfig::UP_ACTIVE_HIGH,\n\
                 \x20   );\n",
                a_n = name(*op, false),
                b_n = name(*op, true),
            )),
            _ => {
                let is_b = b;
                pins.push_str(&format!(
                    "\x20   let mut {n} = mcpwm\n\
                     \x20       .operator{op}\n\
                     \x20       .with_pin_{ab}({n}, PwmPinConfig::UP_ACTIVE_HIGH);\n",
                    n = name(*op, is_b),
                    ab = if is_b { "b" } else { "a" },
                ));
            }
        }
    }

    // One config and one `start` per timer IN USE. Starting an unused one would
    // hold the peripheral clock for an output that does not exist.
    let starts: String = timers
        .iter()
        .map(|t| {
            format!(
                "\x20   let timer_cfg_t{t} = clock_cfg\n\
                 \x20       .timer_clock_with_frequency(\n\
                 \x20           PERIOD_T{t},\n\
                 \x20           PwmWorkingMode::Increase,\n\
                 \x20           Rate::from_hz(FREQUENCY_HZ_T{t}),\n\
                 \x20       )\n\
                 \x20       .unwrap();\n\
                 \x20   mcpwm.timer{t}.start(timer_cfg_t{t});\n"
            )
        })
        .collect();

    let stamps: String = outputs
        .iter()
        .map(|(op, b)| {
            format!(
                "\x20   {}.set_timestamp(TIMESTAMP_OP{op}{});\n",
                name(*op, *b),
                if *b { "B" } else { "A" },
            )
        })
        .collect();
    let handles: String = timers
        .iter()
        .map(|t| format!("mcpwm.timer{t}, "))
        .chain(outputs.iter().map(|(op, b)| format!("{}, ", name(*op, *b))))
        .collect::<String>()
        .trim_end_matches(", ")
        .to_owned();

    let body = format!(
        "/// MCPWM{unit} — motor-control PWM on {} timer{}.\n\
         ///\n\
         /// Returns the TIMERS as well as the pins, and `main.rs` keeps them: a\n\
         /// timer owns the guard that holds the MCPWM clock on. Drop one and\n\
         /// every output on it goes quiet.\n\
         pub fn init<'d>(\n\
         \x20   mcpwm: {peri}<'d>,\n\
         {params}) -> ({ret}) {{\n\
         \x20   let clock_cfg = PeripheralClockConfig::with_frequency(Rate::from_mhz({source_mhz}))\n\
         \x20       .unwrap();\n\
         \x20   let mut mcpwm = McPwm::new(mcpwm, clock_cfg);\n\
         {links}{pins}\n\
         {starts}{stamps}\n\
         \x20   ({handles})\n\
         }}\n",
        timers.len(),
        if timers.len() == 1 { "" } else { "s" },
    );

    let first = outputs.first().copied().unwrap_or((0, false));
    let ft = c.timer_of(first.0);
    let example = example_block(
        &format!("Using MCPWM{unit}"),
        &[
            format!(
                "Duty is a TIMESTAMP: the count the output flips at, out of                  PERIOD_T{ft} + 1."
            ),
            String::new(),
            format!(
                // The handle main.rs binds, which carries the unit in its name.
                "    _mcpwm{unit}_{}.set_timestamp(pins::configs::mcpwm{unit}::PERIOD_T{ft} / 2);   // 50 %",
                name(first.0, first.1)
            ),
            String::new(),
            "// Each timer has its own PERIOD, so a duty computed against one".to_owned(),
            "// means nothing on another. The constant is named for its timer.".to_owned(),
        ],
    );

    file(
        &format!(
            "use esp_hal::gpio::interconnect::PeripheralOutput;\n\
             use esp_hal::mcpwm::operator::{{PwmPin, PwmPinConfig}};\n\
             use esp_hal::mcpwm::timer::{{PwmWorkingMode, Timer}};\n\
             use esp_hal::mcpwm::{{McPwm, PeripheralClockConfig}};\n\
             use esp_hal::peripherals::{peri};\n\
             use esp_hal::time::Rate;\n"
        ),
        &consts,
        &body,
        &example,
    )
}

// ── USB Serial/JTAG ─────────────────────────────────────────────────────────

/// The OTG full-speed controller — a USB device of the project's own design.
///
/// # Not the same peripheral as Serial/JTAG
///
/// They share the pads and nothing else. Serial/JTAG is a fixed console with
/// Espressif's identity; this is a controller you hang a `usb-device` stack on,
/// which is why the VID, PID and product string mean something here and mean
/// nothing there. Only the S2 and S3 have it.
///
/// # DP comes first
///
/// `Usb::new` takes D+ BEFORE D-, the opposite of how the pads are usually
/// listed. Swapping them compiles and enumerates nothing.
///
/// # The allocator cannot come back with its classes
///
/// `UsbBus::new` hands back a `UsbBusAllocator` that everything else BORROWS —
/// the serial port and the device both hold a reference to it. A function
/// cannot return a value and a borrow of that value, so this one returns the
/// allocator alone and `main.rs` builds the rest around it, in one scope.
fn usb_otg_file(cfg: Option<&UsbModuleConfig>) -> String {
    let d = UsbModuleConfig::new(1);
    let c = cfg.unwrap_or(&d);
    let consts = format!(
        "// The host sees these. `0x16c0:0x27dd` is pid.codes' test pair - fine on\n\
         // a bench, not for anything shipped.\n\
         pub const VID: u16 = 0x{:04x};\n\
         pub const PID: u16 = 0x{:04x};\n\
         pub const PRODUCT: &str = {:?};\n\
         // Endpoint scratch, in 32-bit words. 256 is enough for a CDC serial\n\
         // port; a composite device with more endpoints needs more.\n\
         pub const EP_WORDS: usize = 256;\n",
        c.vid, c.pid, c.product,
    );

    let body = "/// The OTG bus, ready for `usb-device` classes.\n\
         ///\n\
         /// D+ FIRST: `Usb::new` takes them in that order. The wrong way round\n\
         /// still compiles and enumerates nothing.\n\
         pub fn init<'d>(\n\
         \x20   usb0: USB0<'d>,\n\
         \x20   dp: impl UsbDp + 'd,\n\
         \x20   dm: impl UsbDm + 'd,\n\
         ) -> UsbBusAllocator<UsbBus<Usb<'d>>> {\n\
         \x20   // The endpoint scratch has to be `'static`, and `init` can only\n\
         \x20   // run once because it takes the USB0 singleton BY VALUE - which\n\
         \x20   // is what makes this handout sound.\n\
         \x20   static mut EP_MEMORY: [u32; EP_WORDS] = [0; EP_WORDS];\n\
         \x20   // SAFETY: see above - one call, one borrow, no other name for it.\n\
         \x20   let ep = unsafe { &mut *(&raw mut EP_MEMORY) };\n\
         \x20   UsbBus::new(Usb::new(usb0, dp, dm), ep)\n\
         }\n"
    .to_owned();

    let example = example_block(
        "Using the USB OTG port",
        &[
            "`main.rs` already builds the device around this bus. Poll it in".to_owned(),
            "your loop - nothing enumerates until you do:".to_owned(),
            String::new(),
            "    if _usb_dev.poll(&mut [&mut _usb_serial]) {".to_owned(),
            "        let mut buf = [0u8; 64];".to_owned(),
            "        if let Ok(n) = _usb_serial.read(&mut buf) {".to_owned(),
            "            _usb_serial.write(&buf[..n]).ok();".to_owned(),
            "        }".to_owned(),
            "    }".to_owned(),
            String::new(),
            "// `poll` must be called often - faster than once a millisecond, or".to_owned(),
            "// the host gives up on the device.".to_owned(),
        ],
    );

    file(
        "use esp_hal::otg_fs::{Usb, UsbBus, UsbDm, UsbDp};\n\
         use esp_hal::peripherals::USB0;\n\
         use usb_device::bus::UsbBusAllocator;\n",
        &consts,
        &body,
        &example,
    )
}

/// The USB Serial/JTAG peripheral: a CDC serial port over the chip's own USB.
///
/// # No pins, and no VID/PID either
///
/// The two pads are fixed in silicon, so `UsbSerialJtag::new` takes the
/// peripheral and nothing else — this is the one config file here whose `init`
/// has no parameters at all.
///
/// The descriptors are fixed too: the device always enumerates as Espressif's
/// `303a:1001`, which is why the module's Vendor ID, Product ID and product
/// string are not offered on an ESP. They belong to the `usb-device` stack the
/// STM32 path builds, where they really are a choice.
///
/// # It is not the flashing port
///
/// A board with a USB-UART bridge chip exposes that instead; this is the port
/// the chip provides itself, on parts that have one. Both can be present, and
/// they are different devices to the host.
fn usb_file(rt: EspRuntime, cfg: Option<&UsbModuleConfig>) -> String {
    if cfg.is_some_and(|c| c.role.is_otg()) {
        return usb_otg_file(cfg);
    }
    let body_for = |name: &str, mode: &str, extra: &str| {
        format!(
            "pub fn {name}<'d>(usb: USB_DEVICE<'d>) -> UsbSerialJtag<'d, {mode}> {{\n\
             \x20   UsbSerialJtag::new(usb){extra}\n\
             }}\n",
        )
    };
    let mut body = format!(
        "/// The chip's own USB CDC serial port — blocking driver.\n\
         ///\n\
         /// Enumerates as Espressif's `303a:1001`. On a board that also has a\n\
         /// USB-UART bridge, this is the SECOND serial port the host sees.\n\
         {}",
        body_for("init", "Blocking", ""),
    );
    if rt == EspRuntime::Async {
        body.push_str(&format!(
            "\n/// The same, async.\n\
             ///\n\
             /// `.into_async()` makes the reads and writes `.await`-able on the\n\
             /// executor instead of spinning.\n\
             {}",
            body_for("init_async", "Async", "\n\x20       .into_async()"),
        ));
    }

    let example = example_for(
        "Using the USB serial port",
        "_usb",
        &[
            "Writing works on the handle, which implements `core::fmt::Write`:",
            "",
            "    use core::fmt::Write;",
            "    writeln!({H}, \"hello over USB\").ok();",
            "",
            "Reading needs the two halves apart, and `split` CONSUMES the handle:",
            "",
            "    let (mut rx, mut tx) = {H}.split();",
            "    let mut buf = [0u8; 64];",
            "    let n = rx.drain_rx_fifo(&mut buf);   // non-blocking",
            "    tx.write(&buf[..n]).ok();             // echo it back",
            "",
            "// Nothing is sent until a host opens the port.",
        ],
        &[
            "`init_async` swaps in the interrupt-driven handler. The .await-able",
            "surface is the embedded-io-async traits, which this project does not",
            "depend on yet — add `embedded-io-async` to Cargo.toml to use them.",
            "The calls below work on either driver:",
            "",
            "    use core::fmt::Write;",
            "    writeln!({H}, \"hello over USB\").ok();",
            "",
            "    let (mut rx, mut tx) = {H}.split();",
            "    let mut buf = [0u8; 64];",
            "    let n = rx.drain_rx_fifo(&mut buf);",
            "    tx.write(&buf[..n]).ok();",
        ],
        rt,
    );

    file(
        &format!(
            "{}\
             use esp_hal::peripherals::USB_DEVICE;\n\
             use esp_hal::usb_serial_jtag::UsbSerialJtag;\n",
            mode_import(rt)
        ),
        // Nothing to configure: the peripheral has no settings the driver
        // exposes, so the generated block is empty rather than decorative.
        "",
        &body,
        &example,
    )
}

// ── LCD_CAM ─────────────────────────────────────────────────────────────

/// The parallel video port, in whichever of its three shapes the module picked.
///
/// # One peripheral, three drivers
///
/// esp-hal splits LCD_CAM into `lcd::i8080`, `lcd::dpi` and `cam`. They share
/// `LcdCam::new`, which hands back two halves — `.lcd` and `.cam` — and each
/// driver consumes one of them. This file builds exactly one, because the
/// module has exactly one mode.
///
/// # DMA is not optional
///
/// All three take a channel in their only constructor, so a project with none
/// left gets no video port at all rather than a slower one. The LCD half wants
/// a TX channel and the camera an RX channel; the same pool serves both.
///
/// # Async is on the PERIPHERAL, not the driver
///
/// `into_async` sits on `LcdCam`, before either half is taken. That is why it
/// appears in the middle of `init` rather than at the end of the chain, and why
/// the camera's type does not change with it: `Camera` has no mode parameter,
/// only its transfer has an `await`.
/// What one LCD_CAM half needs, so the arms below read side by side instead of
/// as a five-place tuple.
struct LcdCamShape<'a> {
    /// The control pads, as `(name, bound)` — the data lanes are appended after.
    ctl_params: Vec<(&'a str, &'a str)>,
    /// The same pads as `(name, setter)`.
    ctl_chain: Vec<(&'a str, &'a str)>,
    /// Which half of `LcdCam` this driver consumes: `lcd` or `cam`.
    half: &'a str,
    /// The return type, written out.
    ty: String,
    /// The esp-hal module under `lcd_cam::` that holds it.
    driver_mod: &'a str,
    /// The data pads' prefix. The two halves need DIFFERENT names in one
    /// signature, because both can appear in it.
    data_prefix: &'a str,
    /// The bound every data pad takes.
    data_dir: &'a str,
    /// `Tx` or `Rx` — which kind of DMA channel this half asks for.
    dma_dir: &'a str,
}

/// The shape of one half, given its mode.
fn lcd_cam_shape<'a>(mode: LcdCamMode, dm: &str) -> LcdCamShape<'a> {
    match mode {
        LcdCamMode::I8080 => LcdCamShape {
            ctl_params: vec![
                ("dc", "impl PeripheralOutput<'d>"),
                ("wr", "impl PeripheralOutput<'d>"),
                ("cs", "impl PeripheralOutput<'d>"),
            ],
            ctl_chain: vec![("dc", "with_dc"), ("wr", "with_wrx"), ("cs", "with_cs")],
            half: "lcd",
            ty: format!("I8080<'d, {dm}>"),
            driver_mod: "lcd::i8080",
            data_prefix: "d",
            data_dir: "impl PeripheralOutput<'d>",
            dma_dir: "Tx",
        },
        LcdCamMode::Dpi => LcdCamShape {
            ctl_params: vec![
                ("vsync", "impl PeripheralOutput<'d>"),
                ("hsync", "impl PeripheralOutput<'d>"),
                ("de", "impl PeripheralOutput<'d>"),
                ("pclk", "impl PeripheralOutput<'d>"),
            ],
            ctl_chain: vec![
                ("vsync", "with_vsync"),
                ("hsync", "with_hsync"),
                ("de", "with_de"),
                ("pclk", "with_pclk"),
            ],
            half: "lcd",
            ty: format!("Dpi<'d, {dm}>"),
            driver_mod: "lcd::dpi",
            data_prefix: "d",
            data_dir: "impl PeripheralOutput<'d>",
            dma_dir: "Tx",
        },
        // The camera READS: every pad is an input except the master clock,
        // which is the one thing this chip gives the sensor. Its control pads
        // are prefixed too, so a display's `pclk` and a sensor's never collide.
        LcdCamMode::Camera => LcdCamShape {
            ctl_params: vec![
                ("mclk", "impl PeripheralOutput<'d>"),
                ("cam_pclk", "impl PeripheralInput<'d>"),
                ("cam_vsync", "impl PeripheralInput<'d>"),
                ("cam_hsync", "impl PeripheralInput<'d>"),
                ("href", "impl PeripheralInput<'d>"),
            ],
            ctl_chain: vec![
                ("mclk", "with_master_clock"),
                ("cam_pclk", "with_pixel_clock"),
                ("cam_vsync", "with_vsync"),
                ("cam_hsync", "with_hsync"),
                ("href", "with_h_enable"),
            ],
            half: "cam",
            // `Camera` carries no driver mode at all.
            ty: "Camera<'d>".to_owned(),
            driver_mod: "cam",
            data_prefix: "cd",
            data_dir: "impl PeripheralInput<'d>",
            dma_dir: "Rx",
        },
    }
}

/// The parallel video port: the display half, the camera half, or BOTH.
///
/// # One constructor, two halves
///
/// `LcdCam::new` consumes the peripheral once and hands back `.lcd` and `.cam`
/// together. They then run independently — an ESP32-S3 driving a display while
/// reading a sensor is what the block is for — so this file has ONE `init` that
/// builds whichever halves the canvas wired, rather than two that would each
/// want the same peripheral.
///
/// # Two channels when both are live
///
/// The display half takes a TX channel and the camera an RX one. Neither has a
/// non-DMA form, so a project short of channels gets no video port at all
/// rather than a slower one.
///
/// # Async is on the PERIPHERAL
///
/// `into_async` sits on `LcdCam`, before either half is taken. That is why it
/// appears in the middle of `init` rather than at the end of a chain, and why
/// the camera's type does not change with it: `Camera` has no mode parameter.
fn lcd_cam_file(
    lcd_sigs: &[&str],
    lcd_cfg: Option<&LcdCamModuleConfig>,
    cam_sigs: &[&str],
    cam_cfg: Option<&LcdCamModuleConfig>,
    rt: EspRuntime,
) -> String {
    let asyn = rt == EspRuntime::Async;
    let dm = if asyn { "Async" } else { "Blocking" };
    let into_async = if asyn { ".into_async()" } else { "" };

    // `(prefix, shape, config, the signal names wired)` per half, display first
    // — the order `main.rs` passes them in.
    let mut halves: Vec<(&str, LcdCamShape, &LcdCamModuleConfig, &[&str])> = Vec::new();
    if !lcd_sigs.is_empty() {
        let c = lcd_cfg.expect("a wired display half has a module");
        halves.push(("LCD", lcd_cam_shape(c.mode, dm), c, lcd_sigs));
    }
    if !cam_sigs.is_empty() {
        let c = cam_cfg.expect("a wired camera half has a module");
        halves.push(("CAM", lcd_cam_shape(LcdCamMode::Camera, dm), c, cam_sigs));
    }

    let mut consts = String::new();
    let mut params = String::new();
    let mut dma_params = String::new();
    let mut body_lines = String::new();
    let mut rets: Vec<String> = Vec::new();
    let mut handles: Vec<String> = Vec::new();
    let mut driver_mods: Vec<String> = Vec::new();
    let mut dma_dirs: Vec<&str> = Vec::new();

    for (prefix, shape, c, sigs) in &halves {
        let lower = prefix.to_lowercase();
        consts.push_str(&lcd_cam_consts(prefix, c));
        dma_params.push_str(&format!(
            "\x20   dma_{lower}: impl {}ChannelFor<LCD_CAM<'d>>,\n",
            shape.dma_dir
        ));
        dma_dirs.push(shape.dma_dir);
        driver_mods.push(shape.driver_mod.to_owned());

        let lanes = usize::from(c.width.min(16)).max(8);
        let data: Vec<(String, String)> = (0..lanes)
            .map(|n| (format!("{}{n}", shape.data_prefix), format!("with_data{n}")))
            .collect();
        let mut param_spec: Vec<(&str, &str)> = shape.ctl_params.clone();
        let mut chain_spec: Vec<(&str, &str)> = shape.ctl_chain.clone();
        for (name, method) in &data {
            param_spec.push((name.as_str(), shape.data_dir));
            chain_spec.push((name.as_str(), method.as_str()));
        }
        params.push_str(&params_for(sigs, &param_spec));

        body_lines.push_str(&lcd_cam_config_expr(prefix, c));
        // The chain must END the statement, not sit above a lone semicolon.
        let mut chain = chain_for(sigs, &chain_spec);
        if chain.ends_with('\n') {
            chain.pop();
        }
        chain.push_str(";\n");
        body_lines.push_str(&format!(
            "\x20   let {lower} = {}::new(lcd_cam.{}, dma_{lower}, {lower}_config)\n\
             \x20       .unwrap()\n\
             {chain}",
            c.mode.driver(),
            shape.half,
        ));
        rets.push(shape.ty.clone());
        handles.push(lower);
    }

    // A single half is returned bare; two come back as a pair.
    let ret = if rets.len() == 1 {
        rets[0].clone()
    } else {
        format!("({})", rets.join(", "))
    };
    let give = if handles.len() == 1 {
        handles[0].clone()
    } else {
        format!("({})", handles.join(", "))
    };

    let body = format!(
        "/// The video port: {}.\n\
         ///\n\
         /// Every pad here is routed through the GPIO matrix, so which one it\n\
         /// is was decided on the canvas rather than by the silicon.\n\
         pub fn {fname}<'d>(\n\
         \x20   lcd_cam: LCD_CAM<'d>,\n\
         {dma_params}{params}) -> {ret} {{\n\
         \x20   let lcd_cam = LcdCam::new(lcd_cam){into_async};\n\
         {body_lines}\
         \x20   {give}\n\
         }}\n",
        halves
            .iter()
            .map(|(_, _, c, _)| c.mode.label())
            .collect::<Vec<_>>()
            .join(" + "),
        fname = if asyn { "init_async" } else { "init" },
    );

    let example = lcd_cam_example(&halves, rt);

    // Which imports a file needs is decided by what the code below actually
    // says, not by the modes: a camera with no master clock binds no output,
    // a display never binds an input, and `Camera` carries no mode marker.
    let mut traits = Vec::new();
    if params.contains("PeripheralInput") {
        traits.push("PeripheralInput");
    }
    if params.contains("PeripheralOutput") {
        traits.push("PeripheralOutput");
    }
    let mode_use = if ret.contains(dm) {
        format!("use esp_hal::{dm};\n")
    } else {
        String::new()
    };
    let mut dma_use: Vec<String> = dma_dirs
        .iter()
        .map(|d| format!("use esp_hal::dma::{d}ChannelFor;\n"))
        .collect();
    dma_use.sort();
    dma_use.dedup();

    let mut driver_use: Vec<String> = halves
        .iter()
        .map(|(_, shape, c, _)| {
            format!(
                "use esp_hal::lcd_cam::{}::{{Config as {}Config, {}{}}};\n",
                shape.driver_mod,
                c.mode.driver(),
                c.mode.driver(),
                if c.mode == LcdCamMode::Dpi {
                    ", Format, FrameTiming"
                } else {
                    ""
                },
            )
        })
        .collect();
    driver_use.sort();
    driver_use.dedup();
    let _ = driver_mods;

    file(
        &format!(
            "{mode_use}{}\
             use esp_hal::gpio::interconnect::{};\n\
             use esp_hal::lcd_cam::LcdCam;\n\
             {}\
             use esp_hal::peripherals::LCD_CAM;\n\
             use esp_hal::time::Rate;\n",
            dma_use.join(""),
            if traits.len() == 1 {
                traits[0].to_owned()
            } else {
                format!("{{{}}}", traits.join(", "))
            },
            driver_use.join(""),
        ),
        &consts,
        &body,
        &example,
    )
}

/// The editable constants for ONE half, namespaced by its prefix.
///
/// `LCD_` and `CAM_`: both halves can be in one file, and both have a
/// frequency and a width, so unprefixed names would collide.
///
/// The RGB timings are constants rather than literals buried in a builder
/// chain: they come off a panel datasheet, they are the numbers most likely to
/// need one more nudge, and a rolling picture is what a wrong one looks like.
fn lcd_cam_consts(prefix: &str, cfg: &LcdCamModuleConfig) -> String {
    let mut s = format!(
        "pub const {prefix}_FREQUENCY: Rate = Rate::from_hz({});\n",
        cfg.clock_hz.max(1)
    );
    if cfg.mode == LcdCamMode::Dpi {
        s.push_str(&format!(
            "// Straight off the panel's datasheet. Total = active + blanking.\n\
             pub const {prefix}_H_ACTIVE: usize = {};\n\
             pub const {prefix}_V_ACTIVE: usize = {};\n\
             pub const {prefix}_H_TOTAL: usize = {};\n\
             pub const {prefix}_V_TOTAL: usize = {};\n\
             pub const {prefix}_H_FRONT_PORCH: usize = {};\n\
             pub const {prefix}_V_FRONT_PORCH: usize = {};\n\
             pub const {prefix}_HSYNC_WIDTH: usize = {};\n\
             pub const {prefix}_VSYNC_WIDTH: usize = {};\n",
            cfg.h_active,
            cfg.v_active,
            cfg.h_total,
            cfg.v_total,
            cfg.h_front_porch,
            cfg.v_front_porch,
            cfg.hsync_width,
            cfg.vsync_width,
        ));
    }
    if cfg.mode != LcdCamMode::I8080 {
        s.push_str(&format!(
            "pub const {prefix}_TWO_BYTE_MODE: bool = {};\n",
            cfg.width >= 16
        ));
    }
    s
}

/// The `let <half>_config = ...;` line, which differs enough per mode to be
/// worth its own function rather than three branches inside the body format.
fn lcd_cam_config_expr(prefix: &str, cfg: &LcdCamModuleConfig) -> String {
    let lower = prefix.to_lowercase();
    let ty = format!("{}Config", cfg.mode.driver());
    match cfg.mode {
        // The i8080 driver takes its width from how many data pads are bound,
        // so there is nothing to set here beyond the clock.
        LcdCamMode::I8080 => format!(
            "\x20   let {lower}_config = {ty}::default().with_frequency({prefix}_FREQUENCY);\n"
        ),
        LcdCamMode::Camera => format!(
            "\x20   let {lower}_config = {ty}::default()\n\
             \x20       .with_frequency({prefix}_FREQUENCY)\n\
             \x20       .with_enable_2byte_mode({prefix}_TWO_BYTE_MODE);\n"
        ),
        LcdCamMode::Dpi => format!(
            "\x20   let {lower}_config = {ty}::default()\n\
             \x20       .with_frequency({prefix}_FREQUENCY)\n\
             \x20       .with_format(Format {{\n\
             \x20           enable_2byte_mode: {prefix}_TWO_BYTE_MODE,\n\
             \x20           ..Default::default()\n\
             \x20       }})\n\
             \x20       .with_timing(FrameTiming {{\n\
             \x20           horizontal_active_width: {prefix}_H_ACTIVE,\n\
             \x20           horizontal_total_width: {prefix}_H_TOTAL,\n\
             \x20           horizontal_blank_front_porch: {prefix}_H_FRONT_PORCH,\n\
             \x20           vertical_active_height: {prefix}_V_ACTIVE,\n\
             \x20           vertical_total_height: {prefix}_V_TOTAL,\n\
             \x20           vertical_blank_front_porch: {prefix}_V_FRONT_PORCH,\n\
             \x20           hsync_width: {prefix}_HSYNC_WIDTH,\n\
             \x20           vsync_width: {prefix}_VSYNC_WIDTH,\n\
             \x20           hsync_position: 0,\n\
             \x20       }});\n"
        ),
    }
}

/// What to do with each handle. The three modes move data three different ways,
/// and none of them looks like a bus.
///
/// Two things here are not symmetric, and both come from esp-hal rather than
/// from choice: every `send`/`receive` reports failure as a TUPLE that carries
/// the driver and the buffer back (so `.unwrap()` alone will not compile), and
/// `wait_for_done` exists on the i8080 transfer ALONE — the RGB and camera
/// transfers have `is_done`, `stop` and a blocking `wait`, on either runtime.
fn lcd_cam_example(
    halves: &[(&str, LcdCamShape, &LcdCamModuleConfig, &[&str])],
    rt: EspRuntime,
) -> String {
    let asyn = rt == EspRuntime::Async;
    let mut lines: Vec<String> = Vec::new();
    for (i, (_, _, c, _)) in halves.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
            lines.push("// …and the other half, at the same time:".to_owned());
            lines.push(String::new());
        }
        match c.mode {
            LcdCamMode::I8080 => {
                lines.push("A command, then its pixels, out of a DMA buffer:".to_owned());
                lines.push(String::new());
                lines.push("    use esp_hal::dma_tx_buffer;".to_owned());
                lines.push("    let mut buf = dma_tx_buffer!(32768).unwrap();".to_owned());
                lines.push("    buf.fill(&[0x55; 32]);".to_owned());
                lines.push(
                    "    // 0x3A is MIPI's COLMOD; the 0 is the dummy-cycle count.".to_owned(),
                );
                lines.push(
                    "    let mut t = _lcd.send(0x3Au8, 0, buf).map_err(|e| e.0).unwrap();"
                        .to_owned(),
                );
                if asyn {
                    lines.push("    t.wait_for_done().await;".to_owned());
                }
                lines.push("    let (result, _lcd, _buf) = t.wait();".to_owned());
                lines.push("    result.unwrap();".to_owned());
            }
            LcdCamMode::Dpi => {
                lines.push("An RGB panel is fed forever, so the buffer LOOPS:".to_owned());
                lines.push(String::new());
                lines.push("    use esp_hal::dma_loop_buffer;".to_owned());
                lines.push("    let mut buf = dma_loop_buffer!(32);".to_owned());
                lines.push("    buf.fill(0xFF);   // a solid frame".to_owned());
                lines
                    .push("    let t = _lcd.send(true, buf).map_err(|e| e.0).unwrap();".to_owned());
                lines.push("    let (_lcd, _buf) = t.stop();".to_owned());
                lines.push(
                    "    // `true` loops it: stopping the transfer blanks the screen.".to_owned(),
                );
            }
            LcdCamMode::Camera => {
                lines
                    .push("The sensor streams; this side supplies somewhere to put it:".to_owned());
                lines.push(String::new());
                lines.push("    use esp_hal::dma_rx_stream_buffer;".to_owned());
                lines.push("    let buf = dma_rx_stream_buffer!(20 * 1000, 1000);".to_owned());
                lines.push("    let t = _cam.receive(buf).map_err(|e| e.0).unwrap();".to_owned());
                lines.push("    while !t.is_done() {}".to_owned());
                lines.push("    let (_cam, _buf) = t.stop();".to_owned());
                lines.push("    // No `await` even on async: esp-hal gives the camera".to_owned());
                lines.push("    // transfer `is_done` and `stop`, nothing to wait on.".to_owned());
            }
        }
    }
    let title = if halves.len() > 1 {
        "Using LCD_CAM (both halves)".to_owned()
    } else {
        format!(
            "Using LCD_CAM ({})",
            halves
                .first()
                .map(|(_, _, c, _)| c.mode.label())
                .unwrap_or("")
        )
    };
    example_block(&title, &lines)
}

// ── Touch ───────────────────────────────────────────────────────────────

/// The capacitive touch controller and the pads wired to it.
///
/// # It hands the controller back too
///
/// `TouchPad::new` takes a `&Touch` and does not keep it — but the `Touch` owns
/// the peripheral singleton, and dropping it would put the controller away
/// while the pads are still being read. So `init` returns it alongside them,
/// for the same reason the MCPWM one returns its timer.
///
/// # The pads are generic
///
/// A `TouchPad`'s type names its GPIO, so ten wired pads would be ten different
/// concrete types to write out. One type parameter each says the same thing and
/// lets `main.rs` pass whatever pads the canvas chose.
///
/// # Async is continuous-only
///
/// esp-hal has `Touch<Continuous, Async>` and no one-shot equivalent: waiting
/// for a touch needs something measuring while you wait. A one-shot module on
/// the async runtime therefore gets `init` alone, and this file says so.
fn touch_file(pads: &[u8], cfg: &TouchModuleConfig, rt: EspRuntime) -> String {
    let consts = format!(
        "pub const THRESHOLD_MODE: ThresholdMode = ThresholdMode::{};\n\
         pub const MEASUREMENT_DURATION: u16 = {};\n\
         {}// The count that means \"touched\". There is no right value: read your\n\
         // own pad untouched and take a margin off it.\n\
         pub const THRESHOLD: u16 = {};\n\
         // What the driver is handed. `None` anywhere here means esp-hal's own\n\
         // default for that field.\n\
         pub const CONFIG: TouchConfig = TouchConfig {{\n\
         \x20   threshold_mode: Some(THRESHOLD_MODE),\n\
         \x20   measurement_duration: Some(MEASUREMENT_DURATION),\n\
         \x20   sleep_cycles: {},\n\
         }};\n",
        cfg.threshold_mode.token(),
        cfg.measurement_duration.max(1),
        if cfg.scan.is_continuous() {
            format!(
                "pub const SLEEP_CYCLES: u16 = {};\n",
                cfg.sleep_cycles.max(1)
            )
        } else {
            String::new()
        },
        cfg.threshold.max(1),
        // The sleep timer only exists in continuous mode.
        if cfg.scan.is_continuous() {
            "Some(SLEEP_CYCLES)"
        } else {
            "None"
        },
    );

    // One type parameter and one argument per wired pad, in channel order.
    let generics: String = pads
        .iter()
        .map(|n| format!("P{n}: TouchPin + 'd, "))
        .collect();
    let params: String = pads
        .iter()
        .map(|n| format!("\x20   pad{n}: P{n},\n"))
        .collect();
    let marker = cfg.scan.marker();

    let build = |fname: &str, dm: &str, ctor: &str, extra_arg: &str, extra_param: &str| {
        let ret: String = pads
            .iter()
            .map(|n| format!(", TouchPad<P{n}, {marker}, {dm}>"))
            .collect();
        let lets: String = pads
            .iter()
            .map(|n| format!("\x20   let touch{n} = TouchPad::new(pad{n}, &touch);\n"))
            .collect();
        let names: String = pads.iter().map(|n| format!(", touch{n}")).collect();
        format!(
            "pub fn {fname}<'d, {generics}>(\n\
             \x20   touch: TOUCH<'d>,\n\
             {extra_param}{params}) -> (Touch<'d, {marker}, {dm}>{ret}) {{\n\
             \x20   let touch = Touch::{ctor}(touch{extra_arg}, Some(CONFIG));\n\
             {lets}\
             \x20   (touch{names})\n\
             }}\n"
        )
    };

    let mut body = format!(
        "/// The touch controller, and one handle per pad.\n\
         ///\n\
         /// The CONTROLLER comes back with the pads on purpose: it owns the\n\
         /// peripheral, and dropping it would stop the pads reading.\n\
         {}",
        build("init", "Blocking", cfg.scan.ctor(), "", ""),
    );
    if rt == EspRuntime::Async {
        if cfg.scan.is_continuous() {
            body.push_str(&format!(
                "\n/// The same, async: the pads gain `wait_for_touch(THRESHOLD).await`.\n\
                 ///\n\
                 /// Takes the RTC because the driver hangs its interrupt off it.\n\
                 /// Anything else that installs an RTC handler breaks this.\n\
                 {}",
                build(
                    "init_async",
                    "Async",
                    "async_mode",
                    ", rtc",
                    "\x20   rtc: &mut Rtc<'_>,\n",
                ),
            ));
        } else {
            body.push_str(
                "\n// No `init_async`: esp-hal has `Touch<Continuous, Async>` and no\n\
                 // one-shot twin - waiting for a touch needs something measuring\n\
                 // while you wait. Switch the module to Continuous for that.\n",
            );
        }
    }

    let first = pads.first().copied().unwrap_or(0);
    let mut lines = vec!["A reading is a 16-bit COUNT, not a yes/no:".to_owned()];
    lines.push(String::new());
    if cfg.scan.is_continuous() && rt == EspRuntime::Async {
        lines.push(format!(
            "    _touch{first}.wait_for_touch(pins::configs::touch::THRESHOLD).await;"
        ));
        lines.push("    // No `read()` here: esp-hal puts it on the BLOCKING pad".to_owned());
        lines.push("    // only, so an async pad waits rather than polls.".to_owned());
    } else {
        if !cfg.scan.is_continuous() {
            lines.push(format!(
                "    _touch{first}.start_measurement();   // one-shot: nothing without this"
            ));
        }
        lines.push(format!("    let n = _touch{first}.read();"));
        lines.push(
            "    let _touched = n < pins::configs::touch::THRESHOLD;   // see THRESHOLD_MODE"
                .to_owned(),
        );
    }
    lines.push(String::new());
    lines.push("// Calibrate against YOUR pad: read it untouched, take a margin".to_owned());
    lines.push("// off, and put that in the module.".to_owned());
    let example = example_block("Using touch", &lines);

    let rtc_use = if rt == EspRuntime::Async && cfg.scan.is_continuous() {
        "use esp_hal::rtc_cntl::Rtc;\n"
    } else {
        ""
    };
    let async_use = if rt == EspRuntime::Async && cfg.scan.is_continuous() {
        "use esp_hal::Async;\n"
    } else {
        ""
    };
    file(
        &format!(
            "use esp_hal::Blocking;\n\
             {async_use}\
             use esp_hal::gpio::TouchPin;\n\
             use esp_hal::peripherals::TOUCH;\n\
             {rtc_use}\
             use esp_hal::touch::{{{marker}, ThresholdMode, Touch, TouchConfig, TouchPad}};\n"
        ),
        &consts,
        &body,
        &example,
    )
}

// ── TWAI (CAN) ──────────────────────────────────────────────────────────

/// The TWAI controller - Espressif's name for CAN 2.0.
///
/// # Two steps, not one
///
/// `TwaiConfiguration::new` does not give you a driver: it gives you a
/// CONFIGURATION, which `start()` then turns into a `Twai`. Everything that can
/// only be set while the controller is off the bus - the acceptance filter
/// above all - happens in between, which is why `init` is two statements.
///
/// # RX comes first
///
/// The constructor takes `(rx, tx)` in that order, the opposite of how the pads
/// are usually spoken about. Swapping them compiles perfectly and produces a
/// node that never hears anything, so both ends name their parameters.
///
/// # Only four bit rates
///
/// esp-hal ships timings for 125k, 250k, 500k and 1M. `BaudRate::Custom` takes
/// a five-field `TimingConfig` computed against the APB clock - real work, and
/// wrong in a way that shows up only as bus errors, so the module offers the
/// four presets and leaves the rest to whoever edits this file.
fn twai_file(n: u8, cfg: Option<&CanModuleConfig>, rt: EspRuntime) -> String {
    let d = CanModuleConfig::new(1);
    let c = cfg.unwrap_or(&d);
    let baud = match c.bitrate {
        125_000 => "B125K",
        250_000 => "B250K",
        1_000_000 => "B1000K",
        // 500k is the default, and the fall-back for anything the presets do
        // not name - see the note above.
        _ => "B500K",
    };
    let consts = format!(
        "pub const BAUD: BaudRate = BaudRate::{baud};\n\
         pub const MODE: TwaiMode = TwaiMode::{};\n",
        c.mode.esp_token(),
    );

    // Two boards wired pad-to-pad have no transceiver to hold the recessive
    // level, and esp-hal has a separate constructor that accounts for it.
    let (ctor, ctor_note) = if c.transceiver {
        (
            "TwaiConfiguration::new",
            "    // A transceiver sits between these pads and the bus.\n",
        )
    } else {
        (
            "TwaiConfiguration::new_no_transceiver",
            "    // No transceiver: two boards wired TX-to-RX directly.\n",
        )
    };
    // `into_async` sits on the CONFIGURATION, so it goes in before `start`.
    let (fname, dm, tail) = match rt {
        EspRuntime::Async => ("init_async", "Async", ".into_async()"),
        EspRuntime::Blocking => ("init", "Blocking", ""),
    };

    let body = format!(
        "/// TWAI{n} - CAN 2.0 on two pads.\n\
         ///\n\
         /// RX FIRST: the constructor takes the receiving pad before the\n\
         /// transmitting one. The wrong way round still compiles.\n\
         ///\n\
         /// The filter is left at accept-all, which is what `new` installs. To\n\
         /// narrow it, call `cfg.set_filter(..)` below before `start()` - it\n\
         /// cannot be changed once the controller is on the bus.\n\
         pub fn {fname}<'d>(\n\
         \x20   twai: impl Instance + 'd,\n\
         \x20   rx: impl PeripheralInput<'d>,\n\
         \x20   tx: impl PeripheralOutput<'d>,\n\
         ) -> Twai<'d, {dm}> {{\n\
         {ctor_note}\
         \x20   let cfg = {ctor}(twai, rx, tx, BAUD, MODE){tail};\n\
         \x20   cfg.start()\n\
         }}\n"
    );

    let traffic = match rt {
        EspRuntime::Async => vec![
            format!("    _twai{n}.transmit_async(&frame).await.unwrap();"),
            format!("    let _got = _twai{n}.receive_async().await.unwrap();"),
        ],
        EspRuntime::Blocking => vec![
            "    // Both are `nb`: Err(WouldBlock) rather than a wait.".to_owned(),
            format!("    while _twai{n}.transmit(&frame).is_err() {{}}"),
            format!("    if let Ok(_got) = _twai{n}.receive() {{ /* a frame arrived */ }}"),
        ],
    };
    let mut lines = vec![
        "A frame carries an id and up to 8 bytes:".to_owned(),
        String::new(),
        "    use esp_hal::twai::{EspTwaiFrame, StandardId};".to_owned(),
        "    let id = StandardId::new(0x123).unwrap();".to_owned(),
        "    let frame = EspTwaiFrame::new(id, &[1, 2, 3]).unwrap();".to_owned(),
    ];
    lines.extend(traffic);
    lines.push(String::new());
    lines.push("// Nothing leaves the controller until another node acknowledges".to_owned());
    lines.push("// it - one board alone on the bus needs Self-test mode instead.".to_owned());
    let example = example_block(&format!("Using TWAI{n}"), &lines);

    file(
        &format!(
            "use esp_hal::{dm};\n\
             use esp_hal::gpio::interconnect::{{PeripheralInput, PeripheralOutput}};\n\
             use esp_hal::twai::{{BaudRate, Instance, Twai, TwaiConfiguration, TwaiMode}};\n"
        ),
        &consts,
        &body,
        &example,
    )
}

// ── PCNT ────────────────────────────────────────────────────────────────────

/// One PCNT unit: its limits, its filter, and what each edge means on each of
/// its two channels.
///
/// # It hands the unit back
///
/// Unlike the buses, there is no driver object to keep — the unit IS the
/// counter, and `main.rs` reads `.counter` off it. So `init` takes the unit,
/// configures it, and returns it.
///
/// # Two channels, one counter
///
/// A unit has `channel0` and `channel1`, each with its own edge pad, its own
/// optional control pad, and its own answer to what an edge means at each
/// control level. They add into the SAME counter, which is what makes a
/// quadrature encoder possible: one channel per phase, with opposite rules.
fn pcnt_file(
    n: u8,
    chans: &[(u8, bool)],
    cfg: Option<&PcntModuleConfig>,
    rt: EspRuntime,
) -> String {
    let d = PcntModuleConfig::new(n);
    let c = cfg.unwrap_or(&d);
    let any_ctrl = chans.iter().any(|(_, has_ctrl)| *has_ctrl);
    let consts = format!(
        "pub const LOW_LIMIT: i16 = {};\n\
         pub const HIGH_LIMIT: i16 = {};\n\
         {}",
        c.low_limit,
        c.high_limit,
        if c.filter > 0 {
            format!(
                "// Pulses shorter than this many APB clocks are ignored.\n\
                 pub const FILTER: u16 = {};\n",
                c.filter,
            )
        } else {
            String::new()
        },
    );

    // One edge parameter per wired channel, each followed by its control pad
    // when there is one — the order `main.rs` passes them in.
    let params: String = chans
        .iter()
        .map(|(ch, has_ctrl)| {
            let mut s = format!("\x20   edge{ch}: impl PeripheralInput<'d>,\n");
            if *has_ctrl {
                s.push_str(&format!("\x20   ctrl{ch}: impl PeripheralInput<'d>,\n"));
            }
            s
        })
        .collect();

    let mut steps = String::from(
        "\x20   unit.set_low_limit(Some(LOW_LIMIT)).unwrap();\n\
         \x20   unit.set_high_limit(Some(HIGH_LIMIT)).unwrap();\n",
    );
    if c.filter > 0 {
        steps.push_str("\x20   unit.set_filter(Some(FILTER)).unwrap();\n");
    }
    steps.push_str("\x20   unit.clear();\n");

    for (ch, has_ctrl) in chans {
        let k = c.channel(*ch);
        steps.push_str(&format!(
            "\n\x20   let channel{ch} = &unit.channel{ch};\n\
             \x20   channel{ch}.set_edge_signal(edge{ch});\n"
        ));
        if *has_ctrl {
            steps.push_str(&format!("\x20   channel{ch}.set_ctrl_signal(ctrl{ch});\n"));
        }
        // The vendor's own argument order, and it is NOT the obvious one:
        // `set_input_mode` takes the FALLING edge first.
        steps.push_str(&format!(
            "\x20   channel{ch}.set_input_mode(EdgeMode::{}, EdgeMode::{}); // (falling, rising)\n",
            k.neg_edge.esp_hal(),
            k.pos_edge.esp_hal(),
        ));
        if *has_ctrl {
            steps.push_str(&format!(
                "\x20   channel{ch}.set_ctrl_mode(CtrlMode::{}, CtrlMode::{}); // (low, high)\n",
                k.ctrl_low.esp_hal(),
                k.ctrl_high.esp_hal(),
            ));
        }
    }

    let body = format!(
        "/// PCNT unit {n} — a hardware pulse counter on {} channel{}.\n\
         ///\n\
         /// `main.rs` builds the one `Pcnt` and lends this unit in; it comes back\n\
         /// configured, and the count is read off `.counter`. Both channels add\n\
         /// into that ONE counter.\n\
         pub fn init<'d, const U: usize>(\n\
         \x20   unit: Unit<'d, U>,\n\
         {params}) -> Unit<'d, U> {{\n\
         {steps}\n\x20   unit\n\
         }}\n",
        chans.len(),
        if chans.len() == 1 { "" } else { "s" },
    );

    let example = example_for(
        &format!("Using PCNT unit {n}"),
        &format!("_pcnt{n}"),
        &[
            "The counter runs on its own; read it whenever you like:",
            "",
            "    let _count = {H}.counter.get();",
            "",
            "    // Start again from zero - `clear` is on the UNIT, so it",
            "    // resets what BOTH channels have added.",
            "    {H}.clear();",
            "",
            "// Reaching a limit CLEARS the counter and raises an event, so a",
            "// total wider than 16 bits is accumulated by listening for it.",
        ],
        &[
            "The counter runs on its own; read it between .awaits:",
            "",
            "    let _count = {H}.counter.get();",
            "    {H}.clear();",
        ],
        rt,
    );

    file(
        &format!(
            "use esp_hal::gpio::interconnect::PeripheralInput;\n\
             use esp_hal::pcnt::channel::{{{}}};\n\
             use esp_hal::pcnt::unit::Unit;\n",
            if any_ctrl {
                "CtrlMode, EdgeMode"
            } else {
                "EdgeMode"
            },
        ),
        &consts,
        &body,
        &example,
    )
}

// ── RMT ─────────────────────────────────────────────────────────────────────

/// One RMT channel, in the direction that channel has.
///
/// # Not a bus, so not shaped like one
///
/// The other files here build a peripheral that moves BYTES. An RMT channel
/// moves EDGES: you hand it `(level, ticks)` pairs and it clocks each one out.
/// So there is no baud rate and no data format — the two numbers that matter
/// are the tick length (the clock divider) and, if the far end is an IR
/// receiver, the carrier to modulate onto.
///
/// # The channel arrives from main.rs
///
/// `Rmt::new` takes the whole block and hands back a struct whose channels are
/// fields. `main.rs` builds it once and lends `rmt.channel0` here, the same way
/// it lends a LEDC timer to a PWM channel — which is why `init` takes a
/// `TxChannelCreator` rather than a `peripherals.*` singleton.
fn rmt_file(n: u8, cfg: Option<&RmtModuleConfig>, rt: EspRuntime, source_hz: u32) -> String {
    let tx = cfg.is_none_or(|c| c.direction.is_tx());
    let divider = cfg.map_or(1, |c| c.clk_divider).max(1);
    let carrier = cfg.is_some_and(|c| c.carrier);
    let carrier_hz = cfg.map_or(38_000, |c| c.carrier_hz).max(1);
    // The carrier is counted in the channel's own ticks, so the divider is
    // already in play. Split evenly: a 50% duty is what an IR receiver expects.
    let period = (source_hz / u32::from(divider) / carrier_hz).max(2);
    let (high, low) = (period / 2, period - period / 2);

    let mut consts = format!(
        "pub const CLK_DIVIDER: u8 = {divider}; // 1 tick = {} ns\n",
        (1_000_000_000f64 / (source_hz as f64 / f64::from(divider))).round() as u64,
    );
    if tx {
        consts.push_str(&format!(
            "pub const IDLE_HIGH: bool = {}; // where the pad rests between trains\n",
            cfg.is_some_and(|c| c.idle_high),
        ));
    } else {
        consts.push_str(&format!(
            "pub const IDLE_THRESHOLD: u16 = {}; // ticks of silence that end a frame\n",
            cfg.map_or(10_000, |c| c.idle_threshold),
        ));
    }
    if carrier {
        consts.push_str(&format!(
            "// Carrier {carrier_hz} Hz at 50%: source / CLK_DIVIDER / carrier, split in two.\n\
             pub const CARRIER_HIGH: u16 = {high};\n\
             pub const CARRIER_LOW: u16 = {low};\n",
        ));
    }

    let (ty, creator, ctor, pad_bound, pad_use) = if tx {
        (
            "Tx",
            "TxChannelCreator",
            "configure_tx",
            "impl PeripheralOutput<'d>",
            "PeripheralOutput",
        )
    } else {
        (
            "Rx",
            "RxChannelCreator",
            "configure_rx",
            "impl PeripheralInput<'d>",
            "PeripheralInput",
        )
    };
    let cfg_ty = if tx {
        "TxChannelConfig"
    } else {
        "RxChannelConfig"
    };
    let mut chain = String::from("\x20       .with_clk_divider(CLK_DIVIDER)\n");
    if tx {
        chain.push_str(
            "\x20       .with_idle_output(true)\n\
             \x20       .with_idle_output_level(if IDLE_HIGH { Level::High } else { Level::Low })\n",
        );
    } else {
        chain.push_str("\x20       .with_idle_threshold(IDLE_THRESHOLD)\n");
    }
    chain.push_str(&if carrier {
        "\x20       .with_carrier_modulation(true)\n\
         \x20       .with_carrier_high(CARRIER_HIGH)\n\
         \x20       .with_carrier_low(CARRIER_LOW)\n\
         \x20       .with_carrier_level(Level::High)\n"
            .to_owned()
    } else {
        "\x20       .with_carrier_modulation(false)\n".to_owned()
    });

    // The `;` belongs to the last builder call, not to a line of its own.
    let chain = chain.trim_end_matches('\n');

    let body_for = |name: &str, mode: &str| {
        format!(
            "pub fn {name}<'d>(\n\
             \x20   channel: impl {creator}<'d, {mode}>,\n\
             \x20   line: {pad_bound},\n\
             ) -> Channel<'d, {mode}, {ty}> {{\n\
             \x20   let config = {cfg_ty}::default()\n\
             {chain};\n\
             \x20   channel.{ctor}(&config).unwrap().with_pin(line)\n\
             }}\n"
        )
    };
    let mut body = format!(
        "/// RMT channel {n} — {}, blocking driver.\n\
         ///\n\
         /// `main.rs` builds the one `Rmt` and lends this channel in.\n\
         {}",
        if tx { "transmitting" } else { "receiving" },
        body_for("init", "Blocking"),
    );
    if rt == EspRuntime::Async {
        body.push_str(&format!(
            "\n/// RMT channel {n} — async driver.\n\
             ///\n\
             /// Identical, but built from the `Rmt` main.rs turned async: the\n\
             /// transmit and receive calls become `.await`-able.\n\
             {}",
            body_for("init_async", "Async"),
        ));
    }

    let example = example_for(
        &format!("Using RMT channel {n}"),
        &format!("_rmt{n}"),
        &if tx {
            vec![
                "A pulse train is a list of (level, ticks) pairs, ended by a zero:",
                "",
                "    use esp_hal::rmt::{PulseCode, TxChannel};",
                "    let data = [",
                "        PulseCode::new(Level::High, 200, Level::Low, 100),",
                "        PulseCode::empty(),   // the terminator",
                "    ];",
                "    let tx = {H}.transmit(&data).unwrap();",
                "    {H} = tx.wait().unwrap();",
            ]
        } else {
            vec![
                "Receiving fills a buffer with the pairs the line carried:",
                "",
                "    use esp_hal::rmt::RxChannel;",
                "    let mut data = [esp_hal::rmt::PulseCode::empty(); 48];",
                "    let rx = {H}.receive(&mut data).unwrap();",
                "    {H} = rx.wait().unwrap();",
            ]
        },
        &if tx {
            vec![
                "main.rs calls `init_async`, so the transmit is .await-able:",
                "",
                "    use esp_hal::rmt::{PulseCode, TxChannelAsync};",
                "    let data = [",
                "        PulseCode::new(Level::High, 200, Level::Low, 100),",
                "        PulseCode::empty(),",
                "    ];",
                "    {H}.transmit(&data).await.unwrap();",
            ]
        } else {
            vec![
                "main.rs calls `init_async`, so the receive is .await-able:",
                "",
                "    use esp_hal::rmt::RxChannelAsync;",
                "    let mut data = [esp_hal::rmt::PulseCode::empty(); 48];",
                "    {H}.receive(&mut data).await.unwrap();",
            ]
        },
        rt,
    );

    file(
        &format!(
            "{}\
             {level_use}\
             use esp_hal::gpio::interconnect::{pad_use};\n\
             use esp_hal::rmt::{{Channel, {ty}, {cfg_ty}, {creator}}};\n",
            mode_import(rt),
            // `Level` names the idle output and the carrier level, and a
            // receive channel with no carrier uses neither — importing it
            // there is an unused-import warning in the user's project.
            level_use = if tx || carrier {
                "use esp_hal::gpio::Level;
"
            } else {
                ""
            },
        ),
        &consts,
        &body,
        &example,
    )
}

// ── I2C ──────────────────────────────────────────────────────────────────────

fn i2c_file(n: u8, sigs: &[&str], cfg: Option<&I2cModuleConfig>, rt: EspRuntime) -> String {
    // The address line now comes from the shared emitter rather than being
    // spelled out here: this file was the ONLY one that wrote it, so the text
    // that explains it had nowhere to be reused from and the other families
    // dropped the setting entirely. Wording moved with it — "an I2C master",
    // not "an esp-hal I2C master", because it is true of every HAL here.
    let consts = format!(
        "pub const FREQUENCY_HZ: u32 = {};\n{}",
        cfg.map_or(100_000, |c| c.clock_hz),
        super::codegen::common::device_address_const(None, cfg.map_or(0, |c| c.address)),
    );
    let bound = "impl PeripheralInput<'d> + PeripheralOutput<'d>";
    let params = format!(
        "    i2c: impl Instance + 'd,\n{}",
        params_for(sigs, &[("scl", bound), ("sda", bound)])
    );
    let chain = chain_for(sigs, &[("scl", "with_scl"), ("sda", "with_sda")]);
    let ctor = "    let config = Config::default().with_frequency(Rate::from_hz(FREQUENCY_HZ));\n\
                \x20   I2c::new(i2c, config)\n\
                \x20       .unwrap()\n";
    let mut body = format!(
        "/// I2C{n} master — blocking driver.\n\
         pub fn init<'d>(\n\
         {params}) -> I2c<'d, Blocking> {{\n\
         {ctor}\
         {chain}}}\n"
    );
    if rt == EspRuntime::Async {
        body.push_str(&async_twin(
            &format!("I2C{n} master — async driver."),
            &params,
            "I2c",
            ctor,
            &chain,
        ));
    }
    let example = example_for(
        &format!("Using I2C{n}"),
        &format!("_i2c{n}"),
        &[
            "In main.rs, after the init above:",
            "",
            "    use pins::configs::i2c{N}::DEVICE_ADDRESS;",
            "",
            "    // Write to a register",
            "    {H}.write(DEVICE_ADDRESS, &[0x10, 0x42]).ok();",
            "",
            "    // Read bytes",
            "    let mut rx = [0u8; 2];",
            "    {H}.read(DEVICE_ADDRESS, &mut rx).ok();",
            "",
            "    // Register read: write the address, then read WITHOUT releasing",
            "    // the bus (repeated START) — what most sensors expect.",
            "    {H}.write_read(DEVICE_ADDRESS, &[0x10], &mut rx).ok();",
        ],
        &[
            "main.rs calls `init_async`, so the handle is an `I2c<'_, Async>`:",
            "",
            "    use pins::configs::i2c{N}::DEVICE_ADDRESS;",
            "",
            "    {H}.write_async(DEVICE_ADDRESS, &[0x10, 0x42]).await.ok();",
            "",
            "    let mut rx = [0u8; 2];",
            "    {H}.read_async(DEVICE_ADDRESS, &mut rx).await.ok();",
            "",
            "    // Register read (repeated START) — what most sensors expect.",
            "    {H}.write_read_async(DEVICE_ADDRESS, &[0x10], &mut rx).await.ok();",
            "",
            "    // `init` is still there if you want this bus blocking instead.",
        ],
        rt,
    )
    .replace("{N}", &n.to_string());
    file(
        &format!(
            "{}\
             use esp_hal::gpio::interconnect::{{PeripheralInput, PeripheralOutput}};\n\
             use esp_hal::i2c::master::{{Config, I2c, Instance}};\n\
             use esp_hal::time::Rate;\n",
            mode_import(rt)
        ),
        &consts,
        &body,
        &example,
    )
}

/// The `(file_name, body)` pairs for every bus instance the pins wire, given
/// `(instance, wired signals)` from
/// [`super::codegen_esp::bus_instances`].
///
/// Keyed on what the CANVAS wires, not on the Virtual Modules: a bus can be
/// wired without a module (the module only carries the settings), and the
/// generated `init` then uses esp-hal's defaults. `rt` decides whether an
/// `init_async` twin joins each `init`.
#[allow(clippy::too_many_arguments)]
/// The LEDC source clock. Fixed at 80 MHz on the ESP32-C3 — `APB_CLK` does not
/// follow `CpuClock`, so this is a constant and not something read off the
/// clock config.
const LEDC_APB_HZ: u32 = 80_000_000;

/// The widest duty resolution `freq_hz` leaves room for, in bits.
///
/// Not a free choice: esp-hal computes `divisor = (apb << 8) / freq / 2^bits`
/// and REFUSES a divisor under 256, so `2^bits` may not exceed `apb / freq`. At
/// 20 kHz that is 4000, so 11 bits fit and 12 do not — and picking 12 anyway
/// makes `configure` return `Err(Divisor)` at boot rather than at build time.
/// Fourteen is the ceiling the ESP32-C3's enum offers.
pub fn ledc_duty_bits(freq_hz: u32) -> u32 {
    let ratio = LEDC_APB_HZ / freq_hz.max(1);
    if ratio == 0 {
        return 1;
    }
    (u32::BITS - ratio.leading_zeros() - 1).clamp(ledc_min_duty_bits(freq_hz), 14)
}

/// The NARROWEST duty resolution `freq_hz` allows, in bits.
///
/// The other half of the bound, and it was missing. esp-hal computes
/// `divisor = (apb << 8) / freq / 2^bits` and refuses it OUTSIDE `256..=0x3FFFF`
/// — too MANY bits push it under 256, too FEW push it over the ceiling. Only the
/// first half was checked, which is right at 20 kHz and wrong at 50 Hz: there a
/// 1-bit resolution leaves a divisor of 409_600 and `configure` returns
/// `Err(Divisor)`, so the generated `unwrap` panics on the board.
pub fn ledc_min_duty_bits(freq_hz: u32) -> u32 {
    const MAX_DIV: u64 = 0x3FFFF;
    let num = (u64::from(LEDC_APB_HZ) << 8) / u64::from(freq_hz.max(1));
    (1..=14).find(|b| num >> b <= MAX_DIV).unwrap_or(14)
}

/// `src/pins/configs/pwm{n}.rs`: one LEDC timer and the channels wired to it.
///
/// Two functions, not one, and the hardware is the reason: a `Channel` BORROWS
/// its timer for as long as it lives (`Config { timer: &dyn TimerIFace }`), so
/// a single `init` returning both would be a self-referential struct. `timer()`
/// hands the timer back to `main.rs`, which owns it and lends it to `init()`.
///
/// # Why the constants carry no channel number
///
/// Only the block between the GENERATED markers is rewritten; everything below
/// it is the user's, and is written ONCE. So a constant whose NAME moves with
/// the wiring breaks the file the moment the wiring changes: re-point a pad from
/// CH1 to CH2 and `DUTY_CH1_PCT` becomes `DUTY_CH2_PCT` in the block, while the
/// `init` below still says `DUTY_CH1_PCT` and no longer compiles.
///
/// Hence: every generated name is FIXED (`CHANNEL`, `DUTY`, `DUTY_RESOLUTION_BIT`),
/// and the hardware choice they encode is a plain number. The `u8` → enum
/// mapping lives below the markers, where the user can read and edit it, and
/// where a rewrite never reaches.
///
/// With SEVERAL pads on one timer the names take a PAD suffix, because several
/// constants cannot all be called `DUTY`. That suffix moves only when the pad
/// does — never when a channel is re-pointed, and never when a lower channel is
/// wired later (which a positional suffix would not survive).
///
/// Crossing between one pad and two DOES rename `DUTY` to `DUTY_<pad>`, in
/// both directions. That is not a regression: adding OR removing a pad already
/// changes `init`'s signature and its return type, both of which live in the
/// editable half, so that edit was unavoidable either way. Re-pointing a channel
/// changed nothing there — and that is the case this exists to fix.
///
/// Moving the PWM to a different GPIO renames that pad's constants too. There
/// is no wiring-independent identity to key on; the pad is simply the one that
/// survives the edit people actually make.
fn pwm_file(
    n: u8,
    // `(channel, duty in hundredths of a percent, pad)`.
    chans: &[(u8, u16, String)],
    cfg: Option<&TimerModuleConfig>,
    // The highest LEDC channel this CHIP has. `channel::Number::Channel6` is
    // `#[cfg]`-ed out on the C2/C3/C6/H2, so a match arm naming it there does
    // not compile — the mapping can only offer what the part carries.
    max_ch: u8,
) -> String {
    let freq = cfg.map_or(1_000, |c| c.freq_hz);
    // The module's choice when it has one; otherwise as wide as the frequency
    // allows, which is what this generated on its own before it was settable.
    // The window THIS frequency allows. A pinned resolution is clamped into it
    // rather than emitted as chosen: the frequency can be raised long after the
    // width was picked, and `configure(...).unwrap()` would then panic on the
    // board — a clean build that dies at boot is the worst of the three
    // outcomes.
    let widest = ledc_duty_bits(freq) as u8;
    let narrowest = ledc_min_duty_bits(freq) as u8;
    let wanted = cfg.and_then(|c| c.duty_res_bits).unwrap_or(widest);
    let bits = wanted.clamp(narrowest, widest);

    // ── the GENERATED block: fixed names, plain numbers ─────────────────────
    let mut consts = format!("pub const FREQUENCY_HZ: u32 = {freq};\n");
    // One `push_str` per line on purpose: rustfmt joins a `\`-continued literal
    // and keeps the SOURCE indentation, which drops the `//` off every line but
    // the first — inside a generated file that is a syntax error, not a typo.
    consts.push_str("// Duty resolution in BITS. Tied to FREQUENCY_HZ: 2^bits must stay\n");
    consts.push_str("// under 80_000_000 / FREQUENCY_HZ (the LEDC runs off the 80 MHz APB\n");
    consts.push_str("// clock), or `configure` returns Err(Divisor). Change one, check the\n");
    consts.push_str("// other. `get_duty_resolution_bit()` below turns it into the enum.\n");
    consts.push_str(&format!("pub const DUTY_RESOLUTION_BIT: u8 = {bits};"));
    // Reduced in the open, never in silence.
    if bits != wanted {
        consts.push_str(&format!(
            " // {wanted} does not fit {freq} Hz - {narrowest}..={widest} does"
        ));
    }
    consts.push('\n');

    let one = chans.len() == 1;
    // With one pad there is no suffix at all — which is the whole point: the
    // name cannot move because there is nothing in it to move.
    //
    // With several, the suffix is the PAD, never the channel and never the
    // position. The pad is the one identity that survives the edit this exists
    // to fix (re-pointing a pad at another channel), and it also survives a
    // LOWER channel being wired later, which would shift every position.
    let sfx = |pad: &str| {
        if one {
            String::new()
        } else {
            format!("_{}", pad.to_ascii_uppercase())
        }
    };
    let lsfx = |pad: &str| {
        if one {
            String::new()
        } else {
            format!("_{}", pad.to_ascii_lowercase())
        }
    };

    for (ch, x100, pad) in chans {
        consts.push_str(&format!(
            "// Which LEDC channel this pad drives, 0..={max_ch}.\n"
        ));
        consts.push_str(&format!("pub const CHANNEL{}: u8 = {ch};\n", sfx(pad)));
        let pct = (*x100 as u32).div_ceil(100).min(100);
        consts.push_str(&format!("pub const DUTY{}: u8 = {pct};", sfx(pad)));
        // esp-hal takes duty in WHOLE percent, so a module set to 7.5 % cannot
        // be carried across as-is. Say so in the file rather than rounding in
        // silence.
        if x100 % 100 != 0 {
            consts.push_str(&format!(
                " // {}.{:02} % rounded up — esp-hal's LEDC takes whole percent",
                x100 / 100,
                x100 % 100
            ));
        }
        consts.push('\n');
    }

    // ── the editable zone ───────────────────────────────────────────────────
    let mut func = String::new();
    func.push_str("/// The `Duty` variant `DUTY_RESOLUTION_BIT` names.\n");
    func.push_str("///\n");
    func.push_str("/// The constant is a plain `u8` so the Virtual Module can rewrite it\n");
    func.push_str("/// without touching this mapping, which is yours.\n");
    func.push_str("const fn get_duty_resolution_bit() -> timer::config::Duty {\n");
    func.push_str("    match DUTY_RESOLUTION_BIT {\n");
    for b in 1..=14u8 {
        // 8 is the fallback arm, so it is not listed twice.
        if b == 8 {
            continue;
        }
        func.push_str(&format!(
            "        {b} => timer::config::Duty::Duty{b}Bit,\n"
        ));
    }
    func.push_str("        _ => timer::config::Duty::Duty8Bit,\n");
    func.push_str("    }\n}\n\n");

    for (_, _, pad) in chans {
        func.push_str(&format!(
            "/// The `channel::Number` `CHANNEL{}` names.\n",
            sfx(pad)
        ));
        func.push_str(&format!(
            "fn get_channel{}() -> channel::Number {{\n    match CHANNEL{} {{\n",
            lsfx(pad),
            sfx(pad)
        ));
        for c in 1..=max_ch {
            func.push_str(&format!("        {c} => channel::Number::Channel{c},\n"));
        }
        func.push_str("        _ => channel::Number::Channel0,\n");
        func.push_str("    }\n}\n\n");
    }

    // The handle + duty helper, the same shape the STM32 backends expose so a
    // call site reads alike. THREE things differ, and esp-hal forces all of
    // them: `set_duty` takes `&self`, it returns a `Result` (a duty the
    // resolution cannot express is a real failure, not something to swallow),
    // and the channel is part of the METHOD NAME rather than an argument,
    // because one `Channel` value owns exactly one channel.
    let one_ty = "channel::Channel<'d, LowSpeed>";
    let handle_ty = if one {
        one_ty.to_owned()
    } else {
        format!("({})", vec![one_ty; chans.len()].join(", "))
    };
    func.push_str("/// What `init` hands back.\n");
    func.push_str(&format!("pub type Handle<'d> = {handle_ty};\n\n"));
    func.push_str("/// Hundredths of a percent into the whole percent esp-hal's LEDC takes,\n");
    func.push_str("/// rounded UP and clamped — the same rounding the `DUTY` constants show.\n");
    func.push_str("fn whole_percent(x100: u32) -> u8 {\n");
    func.push_str("    x100.div_ceil(100).min(100) as u8\n");
    func.push_str("}\n\n");
    func.push_str("/// Set the duty in the units the Virtual Module uses — HUNDREDTHS of a\n");
    func.push_str("/// percent — so a value read off its slider means the same thing here.\n");
    func.push_str("pub trait DutyHandle {\n");
    for (_, _, pad) in chans {
        func.push_str(&format!(
            "    fn set_duty_x100{}(&self, value: u32) -> Result<(), channel::Error>;\n",
            lsfx(pad)
        ));
    }
    func.push_str("}\n\n");
    func.push_str("impl DutyHandle for Handle<'_> {\n");
    for (i, (_, _, pad)) in chans.iter().enumerate() {
        // The tuple FIELD is positional even though the method name is not: the
        // return type is a tuple, and that is how a tuple is indexed.
        let this = if one {
            "self".to_owned()
        } else {
            format!("self.{i}")
        };
        if i > 0 {
            func.push('\n');
        }
        func.push_str(&format!(
            "    fn set_duty_x100{}(&self, value: u32) -> Result<(), channel::Error> {{\n",
            lsfx(pad)
        ));
        func.push_str(&format!(
            "        {this}.set_duty(whole_percent(value))\n    }}\n"
        ));
    }
    func.push_str("}\n\n");

    // The PADS, not the channels: this line sits in the editable half, and a
    // channel number here would move with the wiring exactly like the constant
    // names used to.
    let list = chans
        .iter()
        .map(|(_, _, pad)| pad.clone())
        .collect::<Vec<_>>()
        .join("+");
    func.push_str(&format!(
        "/// The LEDC timer PWM{n} runs on. One frequency for every channel on it.\n"
    ));
    func.push_str("///\n");
    func.push_str("/// `main.rs` keeps the value alive and lends it to `init` — the channels\n");
    func.push_str("/// hold a reference to it for as long as they exist.\n");
    func.push_str("pub fn timer<'d>(ledc: &Ledc<'d>) -> timer::Timer<'d, LowSpeed> {\n");
    func.push_str(&format!(
        "    let mut t = ledc.timer::<LowSpeed>(timer::Number::Timer{n});\n"
    ));
    func.push_str("    t.configure(timer::config::Config {\n");
    func.push_str("        duty: get_duty_resolution_bit(),\n");
    func.push_str("        clock_source: timer::LSClockSource::APBClk,\n");
    func.push_str("        frequency: Rate::from_hz(FREQUENCY_HZ),\n");
    func.push_str("    })\n    .unwrap();\n    t\n}\n\n");

    func.push_str(&format!(
        "/// PWM{n} {list} — the pads wired on the canvas, each on the channel\n"
    ));
    func.push_str("/// its own `CHANNEL` names, at its own `DUTY`.\n");
    func.push_str("pub fn init<'d>(\n");
    func.push_str("    ledc: &Ledc<'d>,\n");
    func.push_str("    timer: &'d timer::Timer<'d, LowSpeed>,\n");
    for (_, _, pad) in chans {
        func.push_str(&format!(
            "    out_pin{}: impl PeripheralOutput<'d>,\n",
            lsfx(pad)
        ));
    }
    func.push_str(") -> Handle<'d> {\n");
    let mut rets = Vec::new();
    for (_, _, pad) in chans {
        let v = format!("ch{}", lsfx(pad));
        func.push_str(&format!(
            "    let mut {v} = ledc.channel(get_channel{}(), out_pin{});\n",
            lsfx(pad),
            lsfx(pad)
        ));
        func.push_str(&format!("    {v}\n"));
        func.push_str("        .configure(channel::config::Config {\n");
        func.push_str("            timer,\n");
        func.push_str(&format!("            duty_pct: DUTY{},\n", sfx(pad)));
        func.push_str("            drive_mode: DriveMode::PushPull,\n");
        func.push_str("        })\n        .unwrap();\n");
        rets.push(v);
    }
    func.push_str(&format!(
        "    {}\n}}\n",
        if one {
            rets[0].clone()
        } else {
            format!("({})", rets.join(", "))
        }
    ));

    let handle = format!("_pwm{n}");
    let mut usage = vec![
        "In main.rs, after the init above:".to_owned(),
        String::new(),
        "    use pins::configs::pwm{N}::DutyHandle as _;".to_owned(),
        "    use esp_hal::ledc::channel::ChannelIFace as _; // for start_duty_fade".to_owned(),
        String::new(),
        "    // Hundredths of a percent — the Virtual Module's own scale.".to_owned(),
    ];
    if one {
        usage.push("    {H}.set_duty_x100(5_000).ok(); // 50 %".to_owned());
    } else {
        usage.push(format!(
            "    {{H}}.set_duty_x100{}(5_000).ok(); // 50 % on that pad",
            lsfx(&chans[0].2)
        ));
    }
    usage.extend([
        String::new(),
        "    // Or let the hardware fade for you — no CPU involved:".to_owned(),
        if one {
            "    {H}.start_duty_fade(0, 100, 1000).ok();".to_owned()
        } else {
            "    {H}.0.start_duty_fade(0, 100, 1000).ok();".to_owned()
        },
    ]);
    let usage: Vec<&str> = usage.iter().map(String::as_str).collect();
    // The LEDC has no async driver in esp-hal, so both runtimes read the same.
    let example = example_for(
        &format!("Using PWM{n}"),
        &handle,
        &usage,
        &usage,
        EspRuntime::Blocking,
    )
    .replace("{N}", &n.to_string());

    file(
        "use esp_hal::gpio::DriveMode;\n\
         use esp_hal::gpio::interconnect::PeripheralOutput;\n\
         use esp_hal::ledc::channel::{self, ChannelIFace};\n\
         use esp_hal::ledc::timer::{self, TimerIFace};\n\
         use esp_hal::ledc::{Ledc, LowSpeed};\n\
         use esp_hal::time::Rate;\n",
        &consts,
        &func,
        &example,
    )
}

pub fn config_files(
    uart: &[(u8, Vec<&'static str>)],
    spi: &[(u8, Vec<&'static str>)],
    i2c: &[(u8, Vec<&'static str>)],
    i2s: &[(u8, Vec<&'static str>)],
    usart_cfg: &BTreeMap<u8, UsartModuleConfig>,
    spi_cfg: &BTreeMap<u8, SpiModuleConfig>,
    i2c_cfg: &BTreeMap<u8, I2cModuleConfig>,
    i2s_cfg: &BTreeMap<u8, I2sModuleConfig>,
    // Just the channel numbers: an RMT channel has one wire, so there is no
    // signal list to carry.
    rmt: &[u8],
    rmt_cfg: &BTreeMap<u8, RmtModuleConfig>,
    rmt_hz: u32,
    // `(unit, its wired channels as (channel, has a control pad))` — a unit has
    // two, and which of them are wired decides the generated signature.
    pcnt: &[(u8, Vec<(u8, bool)>)],
    pcnt_cfg: &BTreeMap<u8, PcntModuleConfig>,
    // True when the USB pads are wired AND this chip's esp-hal has the driver
    // — see `codegen_esp::has_usb_serial_jtag`.
    usb: bool,
    // The USB module, whose ROLE decides which of the two controllers the pads
    // are routed to — see `modules::UsbRole`.
    usb_cfg: Option<&UsbModuleConfig>,
    // The touch channels wired, and the module that configures them. Empty
    // means no touch controller on this canvas.
    touch: &[u8],
    touch_cfg: Option<&TouchModuleConfig>,
    // True when BOTH TWAI pads are wired and the chip has the driver — a CAN
    // node with one wire is not a node, so a half-wired bus emits nothing.
    twai: bool,
    can_cfg: Option<&CanModuleConfig>,
    // `(unit, its wired outputs as (operator, is B))`.
    mcpwm: &[(u8, Vec<(u8, bool)>)],
    mcpwm_cfg: &BTreeMap<u8, McpwmModuleConfig>,
    // The MCPWM peripheral clock, in MHz. 40 everywhere but the H2, which is 32.
    mcpwm_source_mhz: u32,
    // `Some(has a valid pad)` when the SENDING half of the parallel port is
    // wired, and the module that configures it.
    parl_io: &Option<bool>,
    parl_io_cfg: Option<&ParlIoModuleConfig>,
    // The RECEIVING half, wired and configured on its own. Both halves run at
    // once, off one peripheral and one DMA channel.
    parl_rx: &Option<bool>,
    parl_rx_cfg: Option<&ParlIoModuleConfig>,
    // The DISPLAY half's signal names wired, and the module that gives them
    // meaning. Empty means no display on this canvas.
    lcd_cam: &[String],
    lcd_cam_cfg: Option<&LcdCamModuleConfig>,
    // The CAMERA half, wired and configured on its own: the two halves run at
    // the same time, so one file has to build whichever of them exist.
    cam: &[String],
    cam_cfg: Option<&LcdCamModuleConfig>,
    // The DAC channels wired, and the module they belong to.
    dac: &[u8],
    dac_cfg: Option<&DacModuleConfig>,
    // True on the ESP32 alone, whose I2S restricts MCLK to three pads and says
    // so in the type — see `i2s_file`.
    esp32_mclk: bool,
    // LEDC timer → the channels wired on it, with each channel duty in
    // hundredths of a percent.
    pwm: &[(u8, Vec<(u8, u16, String)>)],
    timer_cfg: &BTreeMap<u8, TimerModuleConfig>,
    // The highest LEDC channel THIS chip carries: `channel::Number::Channel6`
    // does not exist on a C2/C3/C6/H2, so the generated mapping can only offer
    // what the part has.
    pwm_max_ch: u8,
    rt: EspRuntime,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (n, sigs) in uart {
        out.push((
            format!("uart{n}.rs"),
            uart_file(*n, sigs, usart_cfg.get(n), rt),
        ));
    }
    for (n, sigs) in spi {
        out.push((format!("spi{n}.rs"), spi_file(*n, sigs, spi_cfg.get(n), rt)));
    }
    for (n, sigs) in i2c {
        out.push((format!("i2c{n}.rs"), i2c_file(*n, sigs, i2c_cfg.get(n), rt)));
    }
    for (n, sigs) in i2s {
        out.push((
            format!("i2s{n}.rs"),
            i2s_file(*n, sigs, i2s_cfg.get(n), rt, esp32_mclk),
        ));
    }
    for n in rmt {
        out.push((
            format!("rmt{n}.rs"),
            rmt_file(*n, rmt_cfg.get(n), rt, rmt_hz),
        ));
    }
    for (n, chans) in pcnt {
        out.push((
            format!("pcnt{n}.rs"),
            pcnt_file(*n, chans, pcnt_cfg.get(n), rt),
        ));
    }
    if usb {
        out.push(("usb.rs".to_owned(), usb_file(rt, usb_cfg)));
    }
    // One file for both halves: `LcdCam::new` consumes the peripheral once, so
    // two independent `init`s could not exist even if they read better.
    if !lcd_cam.is_empty() || !cam.is_empty() {
        let lcd_default = LcdCamModuleConfig::new(0);
        let cam_default = LcdCamModuleConfig::new_camera();
        let lcd_sigs: Vec<&str> = lcd_cam.iter().map(String::as_str).collect();
        let cam_sigs: Vec<&str> = cam.iter().map(String::as_str).collect();
        out.push((
            "lcd_cam.rs".to_owned(),
            lcd_cam_file(
                &lcd_sigs,
                Some(lcd_cam_cfg.unwrap_or(&lcd_default)),
                &cam_sigs,
                Some(cam_cfg.unwrap_or(&cam_default)),
                rt,
            ),
        ));
    }
    // TWAI0 alone: `PinFunction::CanTx`/`CanRx` carry no instance number, so
    // the C6's second controller has no way to be wired on the canvas.
    if !touch.is_empty() {
        let d = TouchModuleConfig::new(0);
        out.push((
            "touch.rs".to_owned(),
            touch_file(touch, touch_cfg.unwrap_or(&d), rt),
        ));
    }
    if twai {
        out.push(("twai0.rs".to_owned(), twai_file(0, can_cfg, rt)));
    }
    if !dac.is_empty() {
        out.push(("dac.rs".to_owned(), dac_file(dac, dac_cfg, rt)));
    }
    if parl_io.is_some() || parl_rx.is_some() {
        let d = ParlIoModuleConfig::new(0);
        let dr = ParlIoModuleConfig::new_rx();
        out.push((
            "parl_io.rs".to_owned(),
            parl_io_file(
                parl_io.map(|v| (parl_io_cfg.unwrap_or(&d), v)),
                parl_rx.map(|v| (parl_rx_cfg.unwrap_or(&dr), v)),
                rt,
            ),
        ));
    }
    for (unit, outputs) in mcpwm {
        out.push((
            format!("mcpwm{unit}.rs"),
            mcpwm_file(*unit, outputs, mcpwm_cfg.get(unit), mcpwm_source_mhz),
        ));
    }
    for (n, chans) in pwm {
        out.push((
            format!("pwm{n}.rs"),
            pwm_file(*n, chans, timer_cfg.get(n), pwm_max_ch),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Virtual Module's settings reach the generated consts — that is the
    /// whole point of the auto-updated block.
    #[test]
    fn module_settings_land_inside_the_generated_block() {
        let mut cfg = UsartModuleConfig::new(1);
        cfg.baud_rate = 9600;
        cfg.data_bits = 7;
        cfg.parity = Parity::Even;
        cfg.stop_bits = StopBits::Two;
        let f = uart_file(1, &["rx", "tx"], Some(&cfg), EspRuntime::Blocking);

        assert!(f.contains("pub const BAUDRATE: u32 = 9600;"), "{f}");
        assert!(f.contains("DataBits::_7"), "{f}");
        assert!(f.contains("Parity::Even"), "{f}");
        assert!(f.contains("StopBits::_2"), "{f}");

        let (begin, end) = (f.find(GEN_BEGIN).unwrap(), f.find(GEN_END).unwrap());
        let baud = f.find("pub const BAUDRATE").unwrap();
        assert!(begin < baud && baud < end, "consts inside the block:\n{f}");
        // The `init` the user edits is BELOW the markers, and the `use` items
        // the consts need are ABOVE them.
        assert!(f.find("pub fn init").unwrap() > end, "{f}");
        assert!(f.find("use esp_hal::uart").unwrap() < begin, "{f}");
    }

    /// A DMA master gets the DMA `init` on EITHER runtime, and the entry point
    /// main.rs calls is the one that takes the channel.
    ///
    /// This is the shape that was broken: the DMA form was emitted only as an
    /// `init_async`, so a BLOCKING project called `init` — which took no
    /// channel — with the channel main.rs had already allocated for it, and
    /// failed to compile with an "unexpected argument #5".
    #[test]
    fn a_dma_master_takes_its_channel_in_the_entry_point_main_calls() {
        let mut cfg = SpiModuleConfig::new(2);
        cfg.async_mode = AsyncBusMode::AsyncDma;
        let sigs = ["sck", "mosi", "miso"];

        let blocking = spi_file(2, &sigs, Some(&cfg), EspRuntime::Blocking);
        assert!(
            blocking.contains("pub fn init<'d>(") && blocking.contains("dma: impl DmaChannelFor"),
            "blocking `init` takes the channel:\n{blocking}"
        );
        assert!(
            blocking.contains("-> SpiDmaBus<'d, Blocking>"),
            "and hands back the DMA bus:\n{blocking}"
        );
        // `.into_async()` on a blocking project would not even name a type it
        // has imported.
        assert!(!blocking.contains("into_async"), "{blocking}");
        assert!(!blocking.contains("init_async"), "{blocking}");

        // On async, main.rs calls `init_async` — so THAT one takes the channel.
        let asy = spi_file(2, &sigs, Some(&cfg), EspRuntime::Async);
        let at = asy.find("pub fn init_async<'d>(").expect("async twin");
        assert!(
            asy[at..].contains("dma: impl DmaChannelFor"),
            "async twin takes the channel:\n{asy}"
        );
        assert!(asy.contains("-> SpiDmaBus<'d, Async>"), "{asy}");
        assert!(asy.contains(".into_async()"), "{asy}");

        // A master that did NOT ask keeps the plain driver on both runtimes,
        // and never mentions a channel it was not given.
        for rt in [EspRuntime::Blocking, EspRuntime::Async] {
            let cpu = spi_file(2, &sigs, Some(&SpiModuleConfig::new(2)), rt);
            assert!(cpu.contains("-> Spi<'d, Blocking>"), "{rt:?}:\n{cpu}");
            assert!(!cpu.contains("DmaChannelFor"), "{rt:?}:\n{cpu}");
            assert!(!cpu.contains("DMA_BUFFER_BYTES"), "{rt:?}:\n{cpu}");
        }
    }

    /// A bus with no module still gets a complete file, on esp-hal's defaults.
    #[test]
    fn a_bus_without_a_module_falls_back_to_defaults() {
        let f = uart_file(0, &["rx", "tx"], None, EspRuntime::Blocking);
        assert!(f.contains("pub const BAUDRATE: u32 = 115200;"), "{f}");
        assert!(f.contains("pub fn init"), "{f}");
    }

    /// `init` declares a parameter per WIRED line and nothing more — an unused
    /// parameter would warn in the user's build and misdescribe the peripheral.
    #[test]
    fn the_signature_mirrors_the_wiring() {
        let full = spi_file(
            2,
            &["sck", "mosi", "miso", "cs"],
            None,
            EspRuntime::Blocking,
        );
        for (param, method) in [
            ("sck:", ".with_sck(sck)"),
            ("mosi:", ".with_mosi(mosi)"),
            ("miso:", ".with_miso(miso)"),
            ("cs:", ".with_cs(cs)"),
        ] {
            assert!(full.contains(param), "{param} declared:\n{full}");
            assert!(full.contains(method), "{method} chained:\n{full}");
        }

        // Three-wire bus: no CS anywhere.
        let no_cs = spi_file(2, &["sck", "mosi", "miso"], None, EspRuntime::Blocking);
        assert!(!no_cs.contains("cs:"), "no cs parameter:\n{no_cs}");
        assert!(!no_cs.contains("with_cs"), "no cs link:\n{no_cs}");
        assert!(no_cs.contains("miso:"), "{no_cs}");

        // TX-only UART: no rx parameter, no `.with_rx`.
        let tx_only = uart_file(1, &["tx"], None, EspRuntime::Blocking);
        assert!(!tx_only.contains("rx:"), "{tx_only}");
        assert!(!tx_only.contains("with_rx"), "{tx_only}");
        assert!(tx_only.contains(".with_tx(tx)"), "{tx_only}");
    }

    /// On the Async runtime each file gains an `init_async` BESIDE `init` — an
    /// extra function, so one bus can await while another stays blocking, and
    /// code already calling `init` keeps working.
    #[test]
    fn async_runtime_adds_init_async_without_removing_init() {
        for (name, blocking, asyncs) in [
            (
                "uart",
                uart_file(0, &["rx", "tx"], None, EspRuntime::Blocking),
                uart_file(0, &["rx", "tx"], None, EspRuntime::Async),
            ),
            (
                "spi",
                spi_file(2, &["sck", "mosi"], None, EspRuntime::Blocking),
                spi_file(2, &["sck", "mosi"], None, EspRuntime::Async),
            ),
            (
                "i2c",
                i2c_file(0, &["scl", "sda"], None, EspRuntime::Blocking),
                i2c_file(0, &["scl", "sda"], None, EspRuntime::Async),
            ),
        ] {
            assert!(
                !blocking.contains("init_async"),
                "{name}: no async twin on the blocking runtime:\n{blocking}"
            );
            assert!(
                blocking.contains("use esp_hal::Blocking;"),
                "{name}: {blocking}"
            );

            assert!(asyncs.contains("pub fn init_async"), "{name}:\n{asyncs}");
            assert!(
                asyncs.contains("pub fn init<'d>"),
                "{name}: `init` SURVIVES — the twin is additive:\n{asyncs}"
            );
            assert!(asyncs.contains(".into_async()"), "{name}:\n{asyncs}");
            assert!(
                asyncs.contains("use esp_hal::{Async, Blocking};"),
                "{name}: both mode markers imported:\n{asyncs}"
            );
            // Both functions take the same parameters — only the tail differs.
            assert_eq!(
                asyncs.matches("impl Instance + 'd").count(),
                2,
                "{name}: same signature twice:\n{asyncs}"
            );
        }
    }

    /// The example follows the runtime: `.await` methods only where they exist.
    #[test]
    fn the_example_matches_the_runtime() {
        let blocking = i2c_file(0, &["scl", "sda"], None, EspRuntime::Blocking);
        assert!(blocking.contains("write_read(DEVICE_ADDRESS"), "{blocking}");
        assert!(!blocking.contains(".await"), "{blocking}");

        let asyncs = i2c_file(0, &["scl", "sda"], None, EspRuntime::Async);
        assert!(
            asyncs.contains("write_read_async(DEVICE_ADDRESS, &[0x10], &mut rx).await"),
            "{asyncs}"
        );

        // esp-hal's async SPI is transfer-in-place only — the example must not
        // promise a `write_async` that does not exist.
        let spi = spi_file(2, &["sck", "mosi"], None, EspRuntime::Async);
        assert!(spi.contains("transfer_in_place_async"), "{spi}");
        assert!(!spi.contains("{H}.write_async"), "{spi}");
    }

    /// The duty resolution is not a taste question: too many bits and the
    /// divisor falls under 256, which esp-hal refuses AT BOOT.
    #[test]
    fn the_duty_resolution_follows_the_frequency() {
        // 80 MHz / 20 kHz = 4000, so 2^11 fits and 2^12 does not.
        assert_eq!(ledc_duty_bits(20_000), 11);
        // Plenty of room at 1 kHz — capped at what the enum offers.
        assert_eq!(ledc_duty_bits(1_000), 14);
        assert_eq!(ledc_duty_bits(50), 14);
        // And no panic at the far end, where the ratio collapses to nothing.
        assert_eq!(ledc_duty_bits(40_000_000), 1);
        assert_eq!(ledc_duty_bits(100_000_000), 1);
        // Zero is nonsense, not a crash: it reads as "as slow as possible",
        // so it lands on the widest resolution rather than dividing by zero.
        assert_eq!(ledc_duty_bits(0), 14);
    }

    /// The LEDC's shape decides the file's: a `Channel` borrows its timer, so
    /// the timer cannot be built and returned in the same call.
    #[test]
    fn the_timer_and_the_channels_are_separate_functions() {
        let mut cfg = TimerModuleConfig::new(0);
        cfg.freq_hz = 20_000;
        cfg.set_duty_x100(1, 2_000);
        let f = pwm_file(0, &[(1, 2_000, "GPIO19".into())], Some(&cfg), 5);

        assert!(f.contains("pub const FREQUENCY_HZ: u32 = 20000;"), "{f}");
        assert!(
            f.contains("pub fn timer<'d>(ledc: &Ledc<'d>) -> timer::Timer<'d, LowSpeed>"),
            "{f}"
        );
        assert!(f.contains("timer::Number::Timer0"), "{f}");
        // The channel takes the timer BY REFERENCE, for its own lifetime.
        assert!(f.contains("timer: &'d timer::Timer<'d, LowSpeed>,"), "{f}");
        // One channel is a bare value, not a 1-tuple.
        assert!(
            f.contains("pub type Handle<'d> = channel::Channel<'d, LowSpeed>;"),
            "{f}"
        );
        assert!(f.contains(") -> Handle<'d> {"), "{f}");
    }

    /// The names in the GENERATED block carry NO channel number, because only
    /// that block is rewritten: a name that moved with the wiring would leave
    /// the user's own `init` below pointing at a constant that no longer exists.
    #[test]
    fn the_generated_names_do_not_move_with_the_channel() {
        let one = |ch: u8| pwm_file(0, &[(ch, 2_000, "GPIO19".into())], None, 5);
        let a = one(1);
        let b = one(2);

        for f in [&a, &b] {
            assert!(f.contains("pub const DUTY: u8 = 20;"), "{f}");
            assert!(f.contains("pub const DUTY_RESOLUTION_BIT: u8 = "), "{f}");
            // The old, channel-bearing spellings are gone for good.
            assert!(!f.contains("DUTY_CH"), "{f}");
            assert!(!f.contains("pub const DUTY_RESOLUTION:"), "{f}");
        }
        // Only the VALUE moves.
        assert!(a.contains("pub const CHANNEL: u8 = 1;"), "{a}");
        assert!(b.contains("pub const CHANNEL: u8 = 2;"), "{b}");

        // Everything outside the marker block is byte-identical between the two
        // — which is the property that keeps a re-wire from breaking the file.
        let tail = |f: &str| {
            f.split("// <<< GENERATED END >>>")
                .nth(1)
                .unwrap()
                .to_owned()
        };
        assert_eq!(tail(&a), tail(&b), "the editable half must not move");
    }

    /// The `u8` → enum mappings live BELOW the markers, where a rewrite never
    /// reaches, and only offer what the chip has.
    #[test]
    fn the_mappings_are_editable_and_chip_bounded() {
        let f = pwm_file(0, &[(1, 2_000, "GPIO19".into())], None, 5);
        let editable = f.split("// <<< GENERATED END >>>").nth(1).unwrap();

        assert!(editable.contains("const fn get_duty_resolution_bit() -> timer::config::Duty {"));
        assert!(editable.contains("11 => timer::config::Duty::Duty11Bit,"));
        // 8 is the fallback arm, so it is never listed twice.
        assert_eq!(editable.matches("Duty8Bit").count(), 1, "{editable}");
        assert!(editable.contains("_ => timer::config::Duty::Duty8Bit,"));
        // 15..=20 exist only on the esp32, so they are never named.
        assert!(!f.contains("Duty15Bit"), "{f}");

        assert!(editable.contains("fn get_channel() -> channel::Number {"));
        assert!(editable.contains("5 => channel::Number::Channel5,"));
        assert!(editable.contains("_ => channel::Number::Channel0,"));
        // A C3 has no Channel6 — naming it would not compile there.
        assert!(!f.contains("Channel6"), "{f}");
        assert!(f.contains("ledc.channel(get_channel(), out_pin)"), "{f}");

        // A part that HAS more channels gets the wider mapping.
        let wide = pwm_file(0, &[(1, 0, "GPIO19".into())], None, 7);
        assert!(wide.contains("7 => channel::Number::Channel7,"), "{wide}");
    }

    /// The module's own resolution wins when it has one; otherwise the widest
    /// the frequency allows, which is what this computed before it was settable.
    #[test]
    fn the_module_can_pin_the_duty_resolution() {
        let mut cfg = TimerModuleConfig::new(0);
        cfg.freq_hz = 20_000;
        let derived = pwm_file(0, &[(1, 0, "GPIO19".into())], Some(&cfg), 5);
        assert!(
            derived.contains("pub const DUTY_RESOLUTION_BIT: u8 = 11;"),
            "{derived}"
        );

        cfg.duty_res_bits = Some(8);
        let pinned = pwm_file(0, &[(1, 0, "GPIO19".into())], Some(&cfg), 5);
        assert!(
            pinned.contains("pub const DUTY_RESOLUTION_BIT: u8 = 8;"),
            "{pinned}"
        );
    }

    /// esp-hal's LEDC takes WHOLE percent. A module set to 7.5 % cannot be
    /// carried across, so the file says what it did instead of pretending.
    #[test]
    fn a_fractional_duty_is_rounded_and_says_so() {
        let f = pwm_file(0, &[(0, 750, "GPIO19".into())], None, 5);
        assert!(f.contains("pub const DUTY: u8 = 8;"), "{f}");
        assert!(
            f.contains("7.50 % rounded up — esp-hal's LEDC takes whole percent"),
            "{f}"
        );
        // A whole percent gets no note — there is nothing to explain.
        let f = pwm_file(0, &[(0, 2_000, "GPIO19".into())], None, 5);
        assert!(f.contains("pub const DUTY: u8 = 20;\n"), "{f}");
        assert!(!f.contains("rounded up"), "{f}");
    }

    /// Several pads on one timer come back as a tuple, and each name is keyed
    /// on its PAD — the one identity that survives a channel change.
    #[test]
    fn several_channels_are_keyed_on_their_pad() {
        let chans = vec![
            (0u8, 1_000u16, "GPIO4".to_owned()),
            (1, 5_000, "GPIO5".to_owned()),
            (2, 0, "GPIO6".to_owned()),
        ];
        let f = pwm_file(0, &chans, None, 5);
        assert!(
            f.contains(
                "pub type Handle<'d> = (channel::Channel<'d, LowSpeed>, \
                 channel::Channel<'d, LowSpeed>, channel::Channel<'d, LowSpeed>);"
            ),
            "{f}"
        );
        for pad in ["GPIO4", "GPIO5", "GPIO6"] {
            // Constants shout; functions and parameters are snake_case, or the
            // generated file warns on every one of them.
            let low = pad.to_ascii_lowercase();
            assert!(
                f.contains(&format!("pub const CHANNEL_{pad}: u8 = ")),
                "{f}"
            );
            assert!(f.contains(&format!("pub const DUTY_{pad}: u8 = ")), "{f}");
            assert!(
                f.contains(&format!("fn get_channel_{low}() -> channel::Number")),
                "{f}"
            );
            assert!(
                f.contains(&format!("out_pin_{low}: impl PeripheralOutput<'d>,")),
                "{f}"
            );
        }
        // No SHOUTING identifier anywhere outside a `const` — `non_snake_case`
        // is a warning the reader cannot answer inside a generated file.
        for line in f.lines() {
            let t = line.trim_start();
            if t.starts_with("const ") {
                continue;
            }
            assert!(!t.contains("GPIO4("), "shouting fn name: {line}");
            assert!(!t.contains("out_pin_GPIO"), "shouting parameter: {line}");
        }
        assert!(f.contains("    (ch_gpio4, ch_gpio5, ch_gpio6)\n"), "{f}");
        // Re-pointing GPIO5 from CH1 to CH4 moves the VALUE and nothing else.
        let mut moved = chans.clone();
        moved[1].0 = 4;
        let g = pwm_file(0, &moved, None, 5);
        let tail = |f: &str| {
            f.split("// <<< GENERATED END >>>")
                .nth(1)
                .unwrap()
                .to_owned()
        };
        assert_eq!(tail(&f), tail(&g), "the editable half must not move");
        assert!(g.contains("pub const CHANNEL_GPIO5: u8 = 4;"), "{g}");
    }

    /// The duty helper speaks the Virtual Module's units, and `set_duty` on ESP
    /// takes `&self` and returns a `Result` — both unlike the STM32 twin.
    #[test]
    fn the_duty_helper_speaks_the_modules_units() {
        let f = pwm_file(0, &[(1, 2_000, "GPIO19".into())], None, 5);
        assert!(f.contains("pub trait DutyHandle {"), "{f}");
        assert!(
            f.contains("fn set_duty_x100(&self, value: u32) -> Result<(), channel::Error>;"),
            "{f}"
        );
        assert!(f.contains("impl DutyHandle for Handle<'_> {"), "{f}");
        assert!(f.contains("self.set_duty(whole_percent(value))"), "{f}");
    }

    /// Every line of the generated file is a line: no `\`-continued literal has
    /// swallowed a comment marker on its way through rustfmt.
    #[test]
    fn no_generated_comment_lost_its_marker() {
        let f = pwm_file(0, &[(1, 2_000, "GPIO19".into())], None, 5);
        for line in f.lines() {
            assert!(
                !line.starts_with(' ') || line.starts_with("    ") || line.starts_with("     "),
                "stray indentation: {line:?}"
            );
            // A line that starts with whitespace then `//` inside the GENERATED
            // block is exactly the joined-literal bug.
            let t = line.trim_start();
            assert!(
                !(line != t && t.starts_with("// ") && t.contains("FREQUENCY_HZ")),
                "comment lost its column: {line:?}"
            );
        }
    }

    #[test]
    fn one_file_per_wired_instance() {
        let files = config_files(
            &[(1, vec!["rx", "tx"])],
            &[(2, vec!["sck", "mosi"])],
            &[(0, vec!["scl", "sda"])],
            &[(0, vec!["ck", "ws", "sd"])],
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &[2],
            &BTreeMap::new(),
            80_000_000,
            // Unit 1: channel 0 with a control pad, channel 1 without.
            &[(1, vec![(0, true), (1, false)])],
            &BTreeMap::new(),
            true,
            // The USB module left at its defaults — Serial/JTAG, not OTG.
            None,
            // Two touch pads wired, module at its defaults.
            &[0, 5],
            None,
            // TWAI wired, with the module left at its defaults.
            true,
            None,
            &[(0, vec![(0, false), (0, true)])],
            &BTreeMap::new(),
            40,
            &Some(false),
            None,
            // …and the receiving half, unwired: one file, one half.
            &None,
            None,
            // An 8-bit i8080 display: DC, WR and D0..D7 wired.
            &[
                "dc".to_owned(),
                "wr".to_owned(),
                "d0".to_owned(),
                "d1".to_owned(),
                "d2".to_owned(),
                "d3".to_owned(),
                "d4".to_owned(),
                "d5".to_owned(),
                "d6".to_owned(),
                "d7".to_owned(),
            ],
            None,
            // …and a camera on the other half at the same time, which is the
            // arrangement this peripheral exists for.
            &["cd0".to_owned(), "cd1".to_owned(), "href".to_owned()],
            None,
            &[1, 2],
            None,
            false,
            &[(0, vec![(1, 2_000, "GPIO19".to_owned())])],
            &BTreeMap::new(),
            5,
            EspRuntime::Blocking,
        );
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "uart1.rs",
                "spi2.rs",
                "i2c0.rs",
                "i2s0.rs",
                "rmt2.rs",
                "pcnt1.rs",
                "usb.rs",
                "lcd_cam.rs",
                "touch.rs",
                "twai0.rs",
                "dac.rs",
                "parl_io.rs",
                "mcpwm0.rs",
                "pwm0.rs"
            ]
        );
    }
}

#[cfg(test)]
mod esp_duty_handle_tests {
    use super::*;

    /// The trap this test exists for: the TUPLE POSITION is not the channel
    /// number, and it is not the pad either. CH0 on GPIO4 + CH2 on GPIO6 means
    /// `self.0` drives GPIO4 and `self.1` drives GPIO6 — while the METHOD names
    /// stay keyed on the pad, so neither moves when a channel is re-pointed.
    #[test]
    fn the_tuple_index_is_the_position_not_the_pad() {
        let f = pwm_file(
            0,
            &[
                (0, 1_000, "GPIO4".to_owned()),
                (2, 5_000, "GPIO6".to_owned()),
            ],
            None,
            5,
        );
        assert!(
            f.contains(
                "_gpio4(&self, value: u32) -> Result<(), channel::Error> {
        self.0.set_duty("
            ),
            "{f}"
        );
        assert!(
            f.contains(
                "_gpio6(&self, value: u32) -> Result<(), channel::Error> {
        self.1.set_duty("
            ),
            "{f}"
        );
        assert!(
            !f.contains("self.2."),
            "there is no third channel:
{f}"
        );
    }

    /// esp-hal takes WHOLE percent, so the trait rounds up rather than losing
    /// the hundredths silently, and hands the `Result` back rather than eating
    /// a duty the resolution cannot hold.
    #[test]
    fn the_duty_is_rounded_up_and_the_result_survives() {
        let f = pwm_file(0, &[(0, 750, "GPIO19".into())], None, 5);
        assert!(f.contains("    x100.div_ceil(100).min(100) as u8"), "{f}");
        assert!(
            f.contains("fn set_duty_x100(&self, value: u32) -> Result<(), channel::Error>;"),
            "{f}"
        );
    }
}
