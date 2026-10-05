//! Async `embassy-stm32` code generation — the [`Runtime::Async`] path.
//!
//! Structurally the twin of [`super::embassy_common`] (blocking), differing only
//! in the *entry point*: instead of `#[cortex_m_rt::entry] fn main() -> !`, the
//! async runtime uses `#[embassy_executor::main] async fn main(Spawner)` and the
//! executor drives the future. GPIO bindings are byte-for-byte identical
//! (`Output::new` / `Input::new` are the same embassy types in both modes), so
//! the per-pin body comes straight from [`embassy_common::gpio_bindings`] and
//! round-trips through [`super::parse_main_rs`] the same way.
//!
//! Selected for every STM32 family that runs on `embassy-stm32` when the project
//! Runtime is Async (see [`super::family::AsyncEmbassyBackend`]). The clock block
//! is shared with the blocking backend ([`super::rcc::graph_clock_block`]) —
//! `embassy_stm32::init` is runtime-agnostic.
//!
//! [`Runtime::Async`]: crate::panels::mcu_module::mcu::Runtime

use super::common::{ASYNC_USER_TAIL, retarget_pristine_tail};
use super::common::{duty_percent_str, pin_binding, sanitize_label};
use super::dma_map;
use super::embassy_common::{NO_PINS_PLACEHOLDER, gpio_bindings_exti};
use super::nvic;
use super::{AUTOGEN_BANNER, GEN_BEGIN, GEN_END, mcu_id_marker_line};
use crate::panels::mcu_module::comparator;
use crate::panels::mcu_module::modules::{
    AsyncBusMode, DacModuleConfig, HspiModuleConfig, I2cModuleConfig, I2sModuleConfig,
    OspiModuleConfig, Parity, PwmMode, PwmPolarity, QspiModuleConfig, SaiModuleConfig,
    SdmmcModuleConfig, SpiBitOrder, SpiModuleConfig, StopBits, TimerModuleConfig, UsartDirection,
    UsartFlow, UsartMode, UsartModuleConfig, XspiModuleConfig,
};
use crate::panels::mcu_module::pins::logic::pin::{Edge, Pin};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use crate::panels::mcu_module::stm32_pin_data;
use std::collections::BTreeMap;

/// Invariant file header for the async runtime (above `GEN_BEGIN`, rebuilt on
/// every re-splice). Imports `Spawner` (the entry-fn argument) and drops the
/// blocking `cortex_m_rt::entry` — the `#[embassy_executor::main]` macro owns the
/// entry point.
///
/// The `unused_*` lints are allowed at **crate** level here, not on `fn main`:
/// `#[embassy_executor::main]` rewrites the fn into a spawned task and does NOT
/// carry a function-level `#[allow]` onto the moved body, so an unwired pin (or
/// the `let p` on a chip with no pins yet) would otherwise warn. The blocking
/// backend can scope the allow to `fn main`; async needs the crate attribute.
pub fn invariant_header(mcu_name: &str, mcu_id: &str) -> String {
    format!(
        "{AUTOGEN_BANNER}\n\
         // MCU: {mcu_name} | HAL: embassy-stm32 (async)\n\
         {id}\n\
         #![no_std]\n\
         #![no_main]\n\
         #![allow(unused_variables, unused_mut)]\n\n\
         pub mod pins;\n\n\
         use embassy_executor::Spawner;\n\
         use panic_halt as _;\n\n",
        id = mcu_id_marker_line(mcu_id),
    )
}

/// The generated section for the async runtime: gpio `use` items (only when
/// needed), the `#[embassy_executor::main]` entry, the caller-supplied
/// `clock_block` (which must define `let p = …` and end in `\n`), one `let`
/// binding per configured GPIO/raw pin, then `periph_calls` — the peripheral init
/// lines (e.g. USART `init(...)`) whose pins are NOT in `pins` (they are moved
/// into the driver). Opens `async fn main(_spawner: Spawner)` — `USER_TAIL`
/// closes it with the editable loop.
pub fn make_generated_section(
    mcu_name: &str,
    pins: &[&Pin],
    clock_block: &str,
    periph_calls: &str,
    // Module-level `bind_interrupts!` for DMA-backed buses ("" when none). Goes
    // ABOVE the entry point - it is an item, not a statement.
    dma_irqs: &str,
    // Custom-module `let x = Foo::new(…);` lines — last, after every binding
    // and peripheral init they consume (see `Mcu::custom_module_inits`).
    custom_inits: &str,
    // The inputs given an EXTI line: each binds as an `ExtiInput` and gets a
    // task above the entry that awaits its edge.
    exti: &[ExtiPin],
) -> String {
    let lines: Vec<(String, u8)> = exti.iter().map(|e| (e.singleton.clone(), e.line)).collect();
    let (use_line, mut body) = gpio_bindings_exti(pins, &lines);
    if !periph_calls.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(periph_calls);
    }
    if body.is_empty() {
        body.push_str(NO_PINS_PLACEHOLDER);
    }
    // The Configuration tab's lines and the Custom modules, each part under
    // its own header (`Mcu::watchdog_and_custom_inits`).
    body.push_str(custom_inits);
    body.push_str(&exti_spawns(exti));
    // No fn-level `#[allow]` — the macro would drop it; the crate attribute in
    // `invariant_header` covers the unused-pin / unused-`p` cases instead.
    format!(
        "{GEN_BEGIN}\n\
         {use_line}\n\
         {dma_irqs}\n{tasks}#[embassy_executor::main]\n\
         async fn main(_spawner: Spawner) {{\n\
         \x20   // {mcu_name}\n\
         {clock_block}\
         \n\
         {body}\
         {GEN_END}\n",
        tasks = exti_tasks(exti),
    )
}

/// Re-splice the generated section of an existing `main.rs`, preserving the user
/// tail. Unlike the blocking splice, the *header* is rebuilt too (not kept from
/// `existing[..begin]`): this is what lets a Blocking→Async runtime switch swap
/// the header's imports/entry over an existing file. Rebuilds from scratch when
/// the markers are gone.
pub fn splice_section(existing: &str, new_section: &str, mcu_name: &str, mcu_id: &str) -> String {
    let header = invariant_header(mcu_name, mcu_id);
    if let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END)) {
        let end = end_start + GEN_END.len();
        // A Blocking project switched to Async keeps its tail — including,
        // while it is still untouched, the seed that has no `.await` warning
        // in it. Exchange that one; anything the user wrote is left alone.
        //
        // An RTIC file's tail closes nothing (`mod app` is all generated), so
        // leaving RTIC it gets a seed that closes `async fn main` first.
        let after = existing[end..].trim_start_matches('\n');
        let after = if super::common::section_is_rtic(existing) {
            super::common::tail_leaving_rtic(after, ASYNC_USER_TAIL)
        } else {
            retarget_pristine_tail(after, true)
        };
        // Preserve only the user code AFTER the markers; the header above them is
        // regenerated so a runtime switch updates the imports + entry.
        let _ = begin; // header replaces everything before the markers
        format!("{header}{new_section}\n{after}")
    } else {
        format!("{header}{new_section}\n{ASYNC_USER_TAIL}")
    }
}

// ── Async USART config file (embassy BufferedUart → embedded-io-async) ─────────

/// `src/pins/configs/usart{N}.rs` for the async runtime: an embassy
/// `BufferedUart` exposed through the STANDARD `embedded-io-async` traits, so app
/// code written generic over `embedded_io_async::{Read, Write}` is portable. The
/// interrupt-driven driver needs an ISR binding (`bind_interrupts!`, module-local
/// `Irqs`) and `'static` TX/RX buffers (`static_cell::StaticCell`). `init` is
/// GENERIC over the pin types (`impl RxPin/TxPin`) so the file is the same for
/// every chip — only the instance number differs; the concrete pins are chosen at
/// the call site in `main.rs`. Real-compile verified on STM32F411RE.
const ASYNC_USART_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 7, 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
{EXTRA_CONSTS}// Byte capacity of the interrupt-driven TX/RX ring buffers.
pub const BUF_LEN: usize = {BUF};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns a value implementing the STANDARD `embedded-io-async` traits
// (`Read` + `Write`), so your application code stays portable across chips/HALs:
//
//     async fn app<S: embedded_io_async::Read + embedded_io_async::Write>(s: &mut S) { /* … */ }
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::usart::{
    BufferedInterruptHandler, BufferedUart, Config, DataBits, Instance, Parity, RxPin, StopBits,
    TxPin,
};
{FLOW_USE}
use embassy_stm32::{peripherals, Peri};
use static_cell::StaticCell;

fn get_config() -> Config {
    let mut config = Config::default();
    config.baudrate = BAUDRATE;
    config.data_bits = match DATA_BITS {
        7 => DataBits::DataBits7,
        9 => DataBits::DataBits9,
        _ => DataBits::DataBits8,
    };
    config.parity = match PARITY {
        'O' => Parity::ParityOdd,
        'E' => Parity::ParityEven,
        _ => Parity::ParityNone,
    };
    config.stop_bits = match STOP_BITS {
        2 => StopBits::STOP2,
        _ => StopBits::STOP1,
    };
{EXTRA_CONFIG}    config
}

/// Initialise {PERI} as an async `embedded-io-async` Read + Write value.
///
/// No `bind_interrupts!` here: on some families several peripherals SHARE one
/// NVIC vector (an STM32G0 routes USART3, USART4 and LPUART1 through
/// `USART3_4_LPUART1`), and a vector may only be bound ONCE in the whole
/// program. `main.rs` owns the single `Irqs` and passes it in.
pub fn init<'d>(
    usart: Peri<'d, peripherals::{PERI}>,
{PIN_PARAMS}{FLOW_PARAMS}    irqs: impl Binding<
        <peripherals::{PERI} as Instance>::Interrupt,
        BufferedInterruptHandler<peripherals::{PERI}>,
    > + 'd,
) -> impl embedded_io_async::Read + embedded_io_async::Write + 'd {
    static TX_BUF: StaticCell<[u8; BUF_LEN]> = StaticCell::new();
    static RX_BUF: StaticCell<[u8; BUF_LEN]> = StaticCell::new();
    let tx_buf = TX_BUF.init([0; BUF_LEN]);
    let rx_buf = RX_BUF.init([0; BUF_LEN]);
    {CTOR}.unwrap()
}

// ── Using {PERI} ──
// The handle is an `embedded-io-async` Read + Write over embassy's BufferedUart.
// In main.rs, inside the async fn after the init above:
//
//     use embedded_io_async::{Read, Write};
//
//     // Send — yields to the executor instead of spinning
//     {HANDLE}.write_all(b"hello\r\n").await.ok();
//     {HANDLE}.flush().await.ok();
//
//     // Receive exactly N bytes
//     let mut buf = [0u8; 4];
//     {HANDLE}.read_exact(&mut buf).await.ok();
//
//     // Or take whatever has arrived (returns as soon as there is >= 1 byte)
//     let n = {HANDLE}.read(&mut buf).await.unwrap_or(0);
//
//     // Time out a read — the whole point of doing this on an executor:
//     use embassy_time::{with_timeout, Duration};
//     match with_timeout(Duration::from_millis(500), {HANDLE}.read(&mut buf)).await {
//         Ok(Ok(n)) => { /* n bytes */ }
//         Ok(Err(_)) => { /* UART error */ }
//         Err(_) => { /* timed out */ }
//     }

"#;

/// The USART peripheral instances that have BOTH a TX and an RX pin configured —
/// the ones an async `BufferedUart` (bidirectional) can be built for. Returns
/// `(instance, tx_pin_name, rx_pin_name)` sorted by instance. A one-sided UART
/// (only TX or only RX) is skipped in v1: `BufferedUart::new` needs both.
fn usart_wires(pins: &[&Pin]) -> Vec<SerialWire> {
    serial_wires(
        pins,
        PinFunction::UsartTx,
        PinFunction::UsartRx,
        PinFunction::UsartCts,
        PinFunction::UsartRts,
    )
}

/// [`usart_wires`] for the LPUART: the same rule over its own pin functions.
fn lpuart_wires(pins: &[&Pin]) -> Vec<SerialWire> {
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
struct SerialWire {
    instance: u8,
    tx: Option<String>,
    rx: Option<String>,
    cts: Option<String>,
    rts: Option<String>,
}

/// Shared body of [`usart_wires`] / [`lpuart_wires`]: the instances of one
/// serial peripheral that have BOTH halves wired, as `(instance, tx, rx)`.
fn serial_wires(
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

/// SPI instances with at least SCK + MOSI wired. Returns
/// `(instance, sck, mosi, miso)` sorted by instance, `miso` absent for a
/// TRANSMIT-ONLY bus.
///
/// MISO used to be required, so wiring only SCK and MOSI generated nothing at
/// all — no code and no complaint. embassy has `new_txonly` for exactly that
/// shape, and it takes one DMA channel instead of two.
fn spi_wires(pins: &[&Pin]) -> Vec<(u8, String, String, Option<String>)> {
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

/// I2C instances with both SCL + SDA wired. Returns `(instance, scl, sda)`.
fn i2c_wires(pins: &[&Pin]) -> Vec<(u8, String, String)> {
    let find = |want: PinFunction| -> Option<String> {
        pins.iter()
            .find(|p| !p.reserved && p.selected_function == want)
            .map(|p| p.gpio().to_owned())
    };
    let mut instances: Vec<u8> = pins
        .iter()
        .filter_map(|p| match p.selected_function {
            PinFunction::I2cScl(n) | PinFunction::I2cSda(n) => Some(n),
            _ => None,
        })
        .collect();
    instances.sort_unstable();
    instances.dedup();
    instances
        .into_iter()
        .filter_map(|n| {
            let scl = find(PinFunction::I2cScl(n))?;
            let sda = find(PinFunction::I2cSda(n))?;
            Some((n, scl, sda))
        })
        .collect()
}

/// The label suffix (`_imu`) for a module's generated handle, or "" when unset.
fn label_sfx(label: &str) -> String {
    let s = sanitize_label(label);
    if s.is_empty() {
        String::new()
    } else {
        format!("_{s}")
    }
}

/// Everything the async backend derives from the wired bus peripherals: the pins
/// they consume (to drop from raw bindings), the `main.rs` init call lines, and
/// the `src/pins/configs/*.rs` bodies. One pass over USART + SPI + I2C.
pub struct AsyncPeriphs {
    /// Pin names moved into a driver → excluded from the raw GPIO bindings.
    pub consumed_pins: Vec<String>,
    /// The `main.rs` peripheral init lines (with a section header). "" if none.
    pub init_calls: String,
    /// `(file_name, generated_body)` per configured bus peripheral.
    pub config_files: Vec<(String, String)>,
    /// True if any SPI/I2C uses async-DMA (drives the `embedded-hal-async` dep).
    pub any_async_dma: bool,
    /// The module-level `bind_interrupts!` the DMA inits need, or "" when no bus
    /// runs on DMA. Emitted OUTSIDE `async fn main` - see [`dma_irqs_block`].
    pub dma_irqs: String,
    /// True if any SPI/I2C exists at all (drives the `embedded-hal` 1.0 dep).
    pub any_spi_i2c: bool,
    /// Every channel this project uses, in allocation order — the Configuration
    /// tab's list. Recorded here rather than recomputed there, so the two can
    /// never disagree.
    pub dma_uses: Vec<dma_map::DmaUse>,
    /// The inputs armed with an interrupt edge, resolved to their EXTI line and
    /// vector. Each becomes a task that awaits the edge.
    pub exti: Vec<ExtiPin>,
}

/// One input the user armed with an edge, resolved to its EXTI line + vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtiPin {
    /// The `let` binding in `main.rs` (`pb5_in_button`).
    pub binding: String,
    /// The embassy singleton (`PB5`).
    pub singleton: String,
    /// EXTI line — the pin NUMBER, which is what makes it a limited resource.
    pub line: u8,
    pub edge: Edge,
    /// The NVIC vector serving that line (`EXTI9_5`).
    pub vector: String,
}

/// The EXTI line a pad sits on: `PB5` → 5. Lines are numbered by PIN, not by
/// port, which is the whole reason two pads can collide on one.
fn exti_line(singleton: &str) -> Option<u8> {
    let rest = singleton.strip_prefix(['P', 'p'])?;
    let mut cs = rest.chars();
    if !cs.next()?.is_ascii_alphabetic() {
        return None;
    }
    let digits: String = cs.take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| *n <= 15)
}

/// The vector serving EXTI line `n`.
///
/// [`nvic::vector_for`] is not enough here. It reads `EXTI15_10` as covering 15
/// and 10 and nothing between — right for a peripheral list (`TIM6_DAC` really
/// is just those two) and wrong for an EXTI name, which states a RANGE: lines
/// 11 to 14 are on that vector too. So the numbers in the name are read as
/// bounds, and the NARROWEST vector containing the line wins (a G0 lists both
/// `EXTI0_1` and `EXTI4_15`; line 1 belongs to the first).
fn exti_vector(vectors: &[String], n: u8) -> Option<&str> {
    let bounds = |v: &str| -> Option<(u8, u8)> {
        let rest = v.strip_prefix("EXTI")?;
        let nums: Vec<u8> = rest
            .split('_')
            .filter_map(|seg| {
                let d: String = seg.chars().take_while(char::is_ascii_digit).collect();
                d.parse().ok()
            })
            .collect();
        let lo = *nums.iter().min()?;
        let hi = *nums.iter().max()?;
        Some((lo, hi))
    };
    vectors
        .iter()
        .filter_map(|v| bounds(v).map(|b| (v, b)))
        .filter(|(_, (lo, hi))| *lo <= n && n <= *hi)
        .min_by_key(|(_, (lo, hi))| hi - lo)
        .map(|(v, _)| v.as_str())
}

/// The armed inputs, resolved — plus a comment line per input that could NOT be
/// given an EXTI, which is the honest half of this.
///
/// Two ways to lose: the line is already taken (PA5 and PB5 are both line 5, and
/// embassy hands the channel to exactly one), or the chip's vector list does not
/// name a vector for it. Both are stated in `main.rs` rather than dropped.
fn exti_plan(pins: &[&Pin], vectors: &[String]) -> (Vec<ExtiPin>, String) {
    let mut out: Vec<ExtiPin> = Vec::new();
    let mut notes = String::new();
    // Sorted by singleton so the winner of a line clash is stable across runs —
    // an arbitrary but FIXED choice beats one that moves with pin order.
    let mut armed: Vec<&&Pin> = pins
        .iter()
        .filter(|p| !p.reserved && p.selected_function == PinFunction::GpioInput)
        .filter(|p| p.irq.is_some())
        .collect();
    armed.sort_by_key(|p| p.gpio().to_owned());

    for p in armed {
        let singleton = p.gpio().to_owned();
        let edge = p.irq.expect("filtered above");
        let Some(line) = exti_line(&singleton) else {
            continue; // not a P<port><n> pad — no EXTI to speak of
        };
        if let Some(prev) = out.iter().find(|e| e.line == line) {
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: EXTI line {line} is already taken by
                     // {}. One pad per line — the channel is a peripheral, and embassy hands
                     // it to exactly one.
",
                prev.singleton
            ));
            continue;
        }
        let Some(vector) = exti_vector(vectors, line) else {
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: this chip's vector list names none for
                     // EXTI line {line}, so there is nothing to bind the handler to.
"
            ));
            continue;
        };
        out.push(ExtiPin {
            binding: pin_binding(
                &singleton.to_ascii_lowercase(),
                &p.selected_function,
                &p.custom_label,
            ),
            singleton,
            line,
            edge,
            vector: vector.to_owned(),
        });
    }
    (out, notes)
}

/// The `Input` method that waits for `edge`.
fn exti_wait(edge: Edge) -> &'static str {
    match edge {
        Edge::Rising => "wait_for_rising_edge",
        Edge::Falling => "wait_for_falling_edge",
        Edge::Both => "wait_for_any_edge",
    }
}

/// One `#[embassy_executor::task]` per armed input, placed above the entry —
/// a task cannot live inside `async fn main`.
///
/// The task OWNS its pin: `wait_for_*` takes `&mut self`, so the pin cannot also
/// be read from `main` without a mutex.
pub fn exti_tasks(armed: &[ExtiPin]) -> String {
    let mut out = String::new();
    for e in armed {
        out.push_str(&format!(
            "/// {sing} — wakes on a {label} edge. The task owns the pin.\n\
             #[embassy_executor::task]\n\
             async fn {b}_irq(mut pin: ExtiInput<'static, Async>) {{\n\
             \x20   loop {{\n\
             \x20       pin.{wait}().await;\n\
             \x20       // {sing} {label} edge: your handler code here.\n\
             \x20   }}\n\
             }}\n\n",
            sing = e.singleton,
            label = e.edge.label().to_ascii_lowercase(),
            b = e.binding,
            wait = exti_wait(e.edge),
        ));
    }
    out
}

/// The `spawner.spawn(...)` lines for the armed inputs.
///
/// NOTE the shape: on this stack (embassy-executor 0.9 / macros 0.7) the TASK
/// returns a `SpawnToken` and `spawn` returns a `Result`. The ESP backend runs
/// executor 0.10, where it is exactly the other way round — see
/// `codegen_esp::irq_inputs`. Do not copy one call into the other.
pub fn exti_spawns(armed: &[ExtiPin]) -> String {
    if armed.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "
    // ── GPIO interrupts ──
",
    );
    for e in armed {
        out.push_str(&format!(
            "    _spawner.spawn({b}_irq({b})).ok();\n",
            b = e.binding
        ));
    }
    out
}

/// Whether a SPI/I2C module runs in async-DMA mode (else blocking).
fn is_dma(mode: AsyncBusMode) -> bool {
    mode == AsyncBusMode::AsyncDma
}

/// The DMA-channel placeholders for an async-DMA init call. Deliberately
/// undefined fields (`p.DMA_TX_TODO`) so the project fails to compile at exactly
/// this line until the user supplies channels valid for the peripheral on their
/// chip — the IDE can't know them (it doesn't model DMA).
///
/// `Irqs` is the LAST argument: embassy 0.6 makes the caller bind the DMA
/// channels' interrupts, and only this file knows which channels those are (see
/// `dma_irqs_block`).
const DMA_TODO: &str = "p.DMA_TX_TODO, p.DMA_RX_TODO, Irqs";

/// The DMA arguments for one peripheral: real channels when the family has a
/// table (see [`dma_map`]), the `TODO` placeholder otherwise.
///
/// Pushes the matching `bind_interrupts!` lines into `binds`, because the two
/// have to agree and deriving them apart is how they drift.
fn dma_args(
    alloc: &mut dma_map::DmaAllocator,
    binds: &mut Vec<(String, String)>,
    uses: &mut Vec<dma_map::DmaUse>,
    bus: dma_map::Bus,
    n: u8,
    label: &str,
    manual: (&str, &str),
    // Which halves this peripheral really uses. A TX-only UART must not take an
    // RX channel out of circulation for a transfer that never happens.
    wants: (bool, bool),
) -> (String, String) {
    // A channel the user pinned in the Virtual Module wins over allocation; it
    // was already reserved, so `take_named` only has to resolve its interrupt.
    // `clash` records the one refusal the user can act on without looking
    // anything up - the same channel pinned to two Virtual Modules - so the
    // TODO below can say that instead of a generic "pass valid channels".
    let mut clash: Option<String> = None;
    let mut pick = |alloc: &mut dma_map::DmaAllocator, named: &str, dir| {
        if named.is_empty() {
            return alloc.take(bus, n, dir);
        }
        match alloc.take_named_or(named) {
            Ok(p) => Some(p),
            Err(dma_map::NamedRefusal::AlreadyTaken) => {
                clash = Some(named.to_owned());
                None
            }
            Err(dma_map::NamedRefusal::UnknownIrq) => None,
        }
    };
    // Both or neither: a TX reserved next to a failed RX would take a channel
    // out of circulation for a peripheral that still ends up on the TODO path.
    let tx = wants
        .0
        .then(|| pick(alloc, manual.0, dma_map::Dir::Tx))
        .unwrap_or(Some(dma_map::DmaPick::default()));
    let rx = wants
        .1
        .then(|| pick(alloc, manual.1, dma_map::Dir::Rx))
        .unwrap_or(Some(dma_map::DmaPick::default()));
    let (Some(tx), Some(rx)) = (tx, rx) else {
        // Say exactly what is missing, at the line that needs it. Two
        // different sentences, because they ask for two different actions.
        //
        // Built line by line rather than as one continued literal: a backslash
        // continuation carries the source indentation into the emitted file the
        // moment rustfmt reflows it, and this text lands in the user's main.rs.
        let mut note = String::new();
        if let Some(ch) = &clash {
            note.push_str(&format!(
                "    // TODO(async DMA): `{ch}` is already taken by another peripheral.\n"
            ));
            note.push_str("    //   Two Virtual Modules are pinned to it - give one of them a\n");
            note.push_str("    //   different channel, or set it back to Automatic.\n");
        } else {
            note.push_str(&format!(
                "    // TODO(async DMA): pass DMA channels valid for {label} on this chip,\n"
            ));
            note.push_str("    //   and bind their interrupts in the `Irqs` block above.\n");
        }
        return (DMA_TODO.to_owned(), note);
    };
    // Kept as a PAIR, not a finished line: two channels can share one
    // interrupt (STM32G0's `DMA1_Channel2_3`), and `bind_interrupts!` wants
    // those as two handlers on one key, not the key twice. Only
    // `dma_irqs_block` sees them all, so only it can group them.
    for c in [&tx, &rx].into_iter().filter(|c| !c.peri.is_empty()) {
        binds.push((c.irq.clone(), c.peri.clone()));
    }
    for (c, dir, pinned) in [
        (&tx, dma_map::Dir::Tx, manual.0),
        (&rx, dma_map::Dir::Rx, manual.1),
    ]
    .into_iter()
    .filter(|(c, _, _)| !c.peri.is_empty())
    {
        uses.push(dma_map::DmaUse {
            peri: c.peri.clone(),
            irq: c.irq.clone(),
            user: format!("{label} {}", dir.label()),
            manual: !pinned.is_empty(),
        });
    }
    // Resolved — no note. A TODO telling the user to do work already done is
    // worse than none: it makes correct output look unfinished.
    let chans: Vec<String> = [&tx, &rx]
        .into_iter()
        .filter(|c| !c.peri.is_empty())
        .map(|c| format!("p.{}", c.peri))
        .collect();
    (format!("{}, Irqs", chans.join(", ")), String::new())
}

/// The channels a module pinned by hand, as `(tx, rx)`; `("", "")` when it
/// pinned none, which is the normal case.
fn manual_channels<'a, C>(
    cfg: Option<&'a C>,
    get: impl Fn(&'a C) -> (&'a str, &'a str),
) -> (&'a str, &'a str) {
    cfg.map(get).unwrap_or(("", ""))
}

/// What the VENDOR DATABASE told us about this chip, as one argument.
///
/// Both halves are `Option`-shaped in practice — a chip imported before the
/// import read them, or from a source that has neither, carries empty ones —
/// and both are consulted for the same kind of question: what this particular
/// part has, rather than what its family usually has.
#[derive(Clone, Copy)]
pub struct ChipData<'a> {
    /// The chip's USART IP version, which decides whether the swap/invert
    /// `Config` fields exist — see `stm32_pin_data::usart_has_swap_invert`.
    pub usart_ip: Option<&'a str>,
    /// The chip's SDMMC IP version, which decides which constructor shape the
    /// SDMMC block may emit — see `stm32_pin_data::sdmmc_kind`.
    pub sdmmc_ip: Option<&'a str>,
    /// DMA channels + request table, or `None` when the chip carries neither.
    pub dma: Option<&'a crate::panels::mcu_module::mcu_def::DmaDef>,
    /// The chip's interrupt vector names; empty when it carries none.
    pub irq_vectors: &'a [String],
}

/// Everything the comparator pass needs, as one argument.
///
/// Three values that only ever travel together — and `async_peripherals` had
/// nine parameters with them spread out, which is where a caller starts passing
/// them in the wrong order.
pub struct CompInputs<'a> {
    /// The instances the Configuration tab switched on, with their settings.
    pub settings: &'a comparator::CompSettings,
    /// Every instance the CHIP has — decides which shared vector is an
    /// instance's, see [`comp_irq`].
    pub instances: &'a [u8],
    /// `(instance, INP pin, INM pin)` for the ones whose pins are wired.
    pub pins: &'a [(u8, String, Option<String>)],
}

/// One comparator, on the Async runtime. STM32G4 only — see
/// [`crate::panels::mcu_module::comparator`] for why.
///
/// `{INM_PARAM}` / `{INM_ARG}` are empty unless the inverting input is a PIN:
/// embassy has two constructors, and the one that takes an `inm` ignores
/// `config.inverting_input` entirely. Emitting the pin argument and the config
/// field together would show a choice the driver then drops.
const COMP_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Configuration tab) - auto-updated; edit it there.
use embassy_stm32::comp::{BlankingSource, Config, Hysteresis, InvertingInput, OutputPolarity};{POWER_USE}

fn get_config() -> Config {
    let mut config = Config::default();
{POWER_LINE}    config.hysteresis = Hysteresis::{HYST};
    config.output_polarity = OutputPolarity::{POLARITY};
    config.inverting_input = InvertingInput::{INM};
    config.blanking_source = BlankingSource::{BLANK};
    config
}
// <<< GENERATED END >>>

// Everything below is editable - your changes are preserved on regeneration.
//
// `init` returns embassy's `Comp`, already enabled. The comparator then runs on
// its own: no CPU involvement until you ask, either by polling `output_level()`
// or by awaiting an edge.
use embassy_stm32::comp::{Comp, InputPlusPin, Instance, InterruptHandler};{INM_USE}
use embassy_stm32::gpio::Pin;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::{peripherals, Peri};

/// The concrete type [`init`] hands back, named so it can be a struct field.
pub type Handle<'d> = Comp<'d, peripherals::COMP{N}>;

/// Initialise COMP{N} and start it.
pub fn init<'d>(
    comp: Peri<'d, peripherals::COMP{N}>,
    inp: Peri<'d, impl InputPlusPin<peripherals::COMP{N}> + Pin>,{INM_PARAM}
    irqs: impl Binding<
            <peripherals::COMP{N} as Instance>::Interrupt,
            InterruptHandler<peripherals::COMP{N}>,
        > + 'd,
) -> Handle<'d> {
    let mut comp = Comp::{CTOR}(comp, inp,{INM_ARG} irqs, get_config());
    // embassy leaves a new comparator OFF; nothing would ever compare without
    // this. Drop the line if you want to start it later yourself.
    comp.enable();
    comp
}

// -- Using COMP{N} --
// In main.rs, after the init above:
//
//     // Read the current result - no waiting.
//     let above = {HANDLE}.output_level();
//
//     // Or wait for the input to cross the threshold. WHICH edge you wait for
//     // is chosen here, not in the config: embassy arms EXTI per call, so
//     // there is no "trigger mode" field to set.
//     {HANDLE}.wait_for_rising_edge().await;
//     {HANDLE}.wait_for_falling_edge().await;
//     {HANDLE}.wait_for_any_edge().await;

"#;

/// Render [`COMP_TMPL`] for one instance.
///
/// `power_mode` is emitted only where embassy WRITES it. `Config::power_mode`
/// exists on both generations, but `configure_raw` computes it under
/// `#[cfg(comp_u5)]` alone — assigning it in a G4 project would put a line in
/// the user's file that changes nothing, which is worse than leaving it out.
fn comp_config_file(n: u8, cfg: &comparator::CompConfig, g: comparator::Generation) -> String {
    let (ctor, inm_param, inm_arg) = if cfg.inverting_input.needs_pin() {
        (
            "new_with_input_minus_pin",
            format!("\n    inm: Peri<'d, impl InputMinusPin<peripherals::COMP{n}> + Pin>,"),
            " inm,".to_owned(),
        )
    } else {
        ("new", String::new(), String::new())
    };
    let (power_use, power_line) = if g.has_power_mode() {
        (
            "
use embassy_stm32::comp::PowerMode;",
            format!(
                "    config.power_mode = PowerMode::{};
",
                cfg.power_mode.token()
            ),
        )
    } else {
        ("", String::new())
    };
    // A level from the other generation cannot be named here - `Hyst20M` does
    // not exist under `comp_u5`. The card offers only the right ones; this is
    // the belt for a `@comp` line hand-edited or carried over from another chip.
    let hyst = if cfg.hysteresis.fits(g) {
        cfg.hysteresis
    } else {
        comparator::Hysteresis::None
    };
    COMP_TMPL
        .replace("{POWER_USE}", power_use)
        .replace("{POWER_LINE}", &power_line)
        .replace("{N}", &n.to_string())
        .replace("{HANDLE}", &format!("_comp{n}"))
        .replace("{POWER}", cfg.power_mode.token())
        .replace("{HYST}", hyst.token())
        .replace("{POLARITY}", cfg.output_polarity.token())
        .replace("{INM}", cfg.inverting_input.token())
        .replace("{BLANK}", cfg.blanking_source.token())
        .replace("{CTOR}", ctor)
        .replace(
            "{INM_USE}",
            // Only the pin form names that trait; importing it always is an
            // `unused_imports` warning in the USER's project, which is worse
            // than in ours - they cannot fix it without editing generated code.
            if cfg.inverting_input.needs_pin() {
                "
use embassy_stm32::comp::InputMinusPin;"
            } else {
                ""
            },
        )
        .replace("{INM_PARAM}", &inm_param)
        .replace("{INM_ARG}", &inm_arg)
}

/// The `bind_interrupts!` key for `COMP{n}` on this chip.
///
/// `COMP1_2_3` on a G474, but plain `COMP4` on a G431 — the family's table
/// carries both and only the instance list decides. See
/// [`nvic::vector_for_within`]. Falls back to `COMP{n}` when the chip carries
/// no vector list, which at least fails loudly rather than silently binding the
/// wrong one.
fn comp_irq(irqs: &[String], n: u8, existing: &[u8]) -> String {
    nvic::vector_for_within(irqs, "COMP", n, existing)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("COMP{n}"))
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
/// USART); without one, only the F1's UART4/5 are known to be UARTs.
fn serial_word(word: &'static str, family: &str, irqs: &[String], n: u8) -> &'static str {
    if word != "USART" {
        return word;
    }
    let uart = if irqs.is_empty() {
        family == "stm32f1" && n >= 4
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
fn serial_irq(irqs: &[String], peri: &str, n: u8) -> String {
    nvic::vector_for(irqs, peri, n)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{peri}{n}"))
}

/// The `bind_interrupts!` block `main.rs` needs when any bus runs on DMA.
///
/// It has to live here rather than in each `configs/*.rs`, because embassy takes
/// ONE value that must satisfy every binding at once — the peripheral's own
/// interrupts and both DMA channels' — and the channels are chosen at this call
/// site. `i2c_instances` are the I2C peripherals whose event/error interrupts
/// belong in the same struct.
fn dma_irqs_block(
    i2c_instances: &[u8],
    // `(peripheral word, instance, buffered)` — "USART"/"LPUART", its number,
    // and which of the two interrupt handlers its `init` takes.
    usart_instances: &[(&str, u8, bool)],
    binds: &[(String, String)],
    irqs: &[String],
    comp_binds: &[(String, u8)],
    // `(vector, handler type)` bound VERBATIM — for a peripheral whose handler
    // is neither a DMA channel's nor one of the shapes above. The SD-card
    // controller is the first: it brings its own interrupt into the same
    // `Irqs`, because its `init` takes one value for that and the channel both.
    extra_binds: &[(String, String)],
    // Whether some `init` below was left with the `DMA_TODO` placeholder: the
    // header asks for channels exactly then - not merely because none were
    // bound, which is also the state of a project with nothing on DMA at all
    // (a buffered USART, an armed input).
    unresolved_dma: bool,
) -> String {
    let head = if unresolved_dma {
        r#"use embassy_stm32::{bind_interrupts, peripherals};

// Interrupt bindings for the DMA-backed peripherals below.
// TODO(async DMA): add one line per DMA channel you pass to an `init`, e.g.
//     DMA2_STREAM3 => embassy_stm32::dma::InterruptHandler<peripherals::DMA2_CH3>;
// The INTERRUPT name (left) and the CHANNEL name (right) differ on most
// families - check your chip's `embassy_stm32::interrupt` list.
bind_interrupts!(struct Irqs {
"#
    } else {
        r#"use embassy_stm32::{bind_interrupts, peripherals};

// Interrupt bindings for the DMA-backed peripherals below.
bind_interrupts!(struct Irqs {
"#
    };

    // One entry per INTERRUPT, listing every handler it carries. Grouping is
    // not a nicety: `bind_interrupts!` rejects a repeated key, and repeats are
    // normal — an STM32G0's `DMA1_Channel2_3` covers two channels, its `I2C1`
    // carries both I2C halves, its `USART3_4_LPUART1` two USARTs.
    let mut by_irq: Vec<(String, Vec<String>)> = Vec::new();
    let mut bind = |irq: String, handler: String| match by_irq.iter_mut().find(|(k, _)| *k == irq) {
        Some((_, hs)) => {
            if !hs.contains(&handler) {
                hs.push(handler);
            }
        }
        None => by_irq.push((irq, vec![handler])),
    };

    for n in i2c_instances {
        let ev = format!("embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C{n}>");
        let er = format!("embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C{n}>");
        // Two vectors on most families, ONE on G0/C0/L0/U0 - where the split
        // names do not exist and the macro cannot compile them.
        match nvic::i2c_irqs(irqs, *n) {
            Some(nvic::I2cIrqs::Combined(v)) => {
                bind(v.clone(), ev);
                bind(v, er);
            }
            Some(nvic::I2cIrqs::Split { ev: e, er: r }) => {
                bind(e, ev);
                bind(r, er);
            }
            None => {
                bind(format!("I2C{n}_EV"), ev);
                bind(format!("I2C{n}_ER"), er);
            }
        }
    }
    for (irq, peri) in binds {
        bind(
            irq.clone(),
            format!("embassy_stm32::dma::InterruptHandler<peripherals::{peri}>"),
        );
    }
    for (irq, handler) in extra_binds {
        bind(irq.clone(), handler.clone());
    }
    for (vector, n) in comp_binds {
        // Several comparators share one vector on the G4, so this goes through
        // the same grouping as everything else.
        bind(
            vector.clone(),
            format!("embassy_stm32::comp::InterruptHandler<peripherals::COMP{n}>"),
        );
    }
    for (peri, n, buffered) in usart_instances {
        // EVERY async serial binds here, buffered or DMA — a vector may be bound
        // only once in the program, and on an STM32G0 USART3, USART4 and LPUART1
        // share `USART3_4_LPUART1`, so the grouping above is the only thing that
        // lets two of them coexist. The handler type is the one its `init` asks
        // for: `BufferedInterruptHandler` for the ring-buffer driver,
        // `InterruptHandler` for the DMA one.
        let handler = if *buffered {
            "BufferedInterruptHandler"
        } else {
            "InterruptHandler"
        };
        bind(
            serial_irq(irqs, peri, *n),
            format!("embassy_stm32::usart::{handler}<peripherals::{peri}{n}>"),
        );
    }

    let mut b = String::from(head);
    for (irq, handlers) in &by_irq {
        b.push_str(&format!(
            "    {irq} => {};
",
            handlers.join(", ")
        ));
    }
    b.push_str(
        "});

",
    );
    b
}

pub fn async_peripherals(
    family: &str,
    chip: ChipData<'_>,
    comp: CompInputs<'_>,
    pins: &[&Pin],
    usart: &BTreeMap<u8, UsartModuleConfig>,
    spi: &BTreeMap<u8, SpiModuleConfig>,
    i2c: &BTreeMap<u8, I2cModuleConfig>,
    // LPUART is its OWN map, not folded into `usart`: LPUART1 and USART1 are
    // different peripherals that share the instance number.
    lpuart: &BTreeMap<u8, UsartModuleConfig>,
    // PWM, keyed by TIMER — one module per timer, whatever its channel count.
    timer: &BTreeMap<u8, TimerModuleConfig>,
    // I2S, keyed by the SPI block it runs on — I2S2 IS SPI2.
    i2s: &BTreeMap<u8, I2sModuleConfig>,
    // DAC, keyed by peripheral — one module per block, one or two channels.
    dac: &BTreeMap<u8, DacModuleConfig>,
    // SAI, keyed by UNIT — the two sub-blocks are inside it.
    sai: &BTreeMap<u8, SaiModuleConfig>,
    // SD card / eMMC, keyed by controller. 0 is the un-numbered SDIO.
    sdmmc: &BTreeMap<u8, SdmmcModuleConfig>,
    // External flash. Single-instance peripheral, so a single config.
    qspi: Option<&QspiModuleConfig>,
    // OCTOSPI, keyed by the IO-manager PORT.
    ospi: &BTreeMap<u8, OspiModuleConfig>,
    // XSPI, keyed by its own IO-manager PORT.
    xspi: &BTreeMap<u8, XspiModuleConfig>,
    // HSPI, keyed by controller instance — these pads are instance-numbered.
    hspi: &BTreeMap<u8, HspiModuleConfig>,
) -> AsyncPeriphs {
    let mut consumed = Vec::new();
    let mut calls = String::new();
    let mut files = Vec::new();
    let mut any_async_dma = false;
    let mut any_spi_i2c = false;
    // I2C peripherals on DMA: their event/error interrupts go in the SAME
    // `Irqs` struct as the channels', because `I2c::new` takes one value.
    let mut dma_i2c_instances: Vec<u8> = Vec::new();
    // Every async serial, DMA-backed or buffered: they all need their vector
    // bound in main.rs's single `Irqs`.
    let mut serial_instances: Vec<(&str, u8, bool)> = Vec::new();
    let mut dma_binds: Vec<(String, String)> = Vec::new();
    // Handlers that are not a DMA channel's — see `dma_irqs_block`.
    let mut extra_binds: Vec<(String, String)> = Vec::new();
    // ── GPIO interrupts ──────────────────────────────────────────────────────
    // `ExtiInput::new` takes a BINDING, not just the channel, so an armed pin
    // adds its vector to the same `Irqs` the DMA peripherals use. The handler is
    // generic over the interrupt, not over a peripheral — unlike every other
    // entry in that block.
    //
    // A chip with no vector list of its own - the built-in F103, and any F1
    // imported without vendor data - still has the F1's seven EXTI vectors,
    // the same set the Blocking and RTIC paths bind (`rtic::exti_vector`).
    let f1_vectors: Vec<String>;
    let vectors: &[String] = if chip.irq_vectors.is_empty() && family == "stm32f1" {
        f1_vectors = (0..=15u8)
            .map(|line| super::rtic::exti_vector(line).to_owned())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        &f1_vectors
    } else {
        chip.irq_vectors
    };
    let (exti, exti_notes) = exti_plan(pins, vectors);
    for e in &exti {
        extra_binds.push((
            e.vector.clone(),
            format!(
                "embassy_stm32::exti::InterruptHandler<embassy_stm32::interrupt::typelevel::{}>",
                e.vector
            ),
        ));
    }
    let mut dma_uses: Vec<dma_map::DmaUse> = Vec::new();
    let mut alloc = dma_map::DmaAllocator::for_chip(family, chip.dma);
    // Hand-picked channels come out of circulation FIRST, so that whichever
    // peripheral happens to be emitted earlier cannot take one.
    //
    // EVERY map that carries a hand-pinned pair has to be in this chain.
    // `lpuart` was missing, and the failure was exactly the arbitrariness the
    // manual field exists to remove: a channel pinned on an LPUART module was
    // still free when USART/SPI/I2C/I2S/SAI/SDMMC were served, so whoever came
    // first took it and the LPUART fell through to `AlreadyTaken` and a clash
    // TODO. It is easy to miss because LPUART is its own map — the emission
    // loop below pairs it with USART deliberately for that reason.
    for (tx, rx) in usart
        .values()
        .map(|c| (&c.dma_tx, &c.dma_rx))
        .chain(lpuart.values().map(|c| (&c.dma_tx, &c.dma_rx)))
        .chain(spi.values().map(|c| (&c.dma_tx, &c.dma_rx)))
        .chain(i2c.values().map(|c| (&c.dma_tx, &c.dma_rx)))
        .chain(i2s.values().map(|c| (&c.dma_tx, &c.dma_rx)))
        .chain(sai.values().map(|c| (&c.dma_a, &c.dma_b)))
        .chain(sdmmc.values().map(|c| (&c.dma_tx, &c.dma_rx)))
    {
        alloc.reserve(tx);
        alloc.reserve(rx);
    }

    // USART and LPUART are the SAME emission: embassy drives both through
    // `usart::{BufferedUart, Uart}`, so only the peripheral word, the config
    // module's name and the DMA request table differ. Looping over the two
    // instead of copying the block is what keeps a fix to one from missing the
    // other.
    for (peri, stem, bus, wires, cfgs) in [
        (
            "USART",
            "usart",
            dma_map::Bus::Usart,
            usart_wires(pins),
            usart,
        ),
        (
            "LPUART",
            "lpuart",
            dma_map::Bus::Lpuart,
            lpuart_wires(pins),
            lpuart,
        ),
    ] {
        for w in wires {
            let n = w.instance;
            // `USART4` or `UART4`: the pin function is the same, the singleton
            // embassy generates is not.
            let peri = serial_word(peri, family, chip.irq_vectors, n);
            let cfg = cfgs.get(&n);
            let dir = cfg.map(|c| c.direction).unwrap_or_default();
            let flow = cfg.map(|c| c.flow).unwrap_or_default();
            // The direction decides which pins are REQUIRED — a TX-only UART is
            // complete without an RX pad. A half the direction needs but the
            // canvas has not wired yet is simply not ready to generate.
            let (tx, rx) = (w.tx.as_deref(), w.rx.as_deref());
            if (dir.needs_tx() && tx.is_none()) || (dir.needs_rx() && rx.is_none()) {
                continue;
            }
            // Same for flow control: the option is only honoured once its pad is
            // wired, so a half-finished choice generates the plain form instead
            // of code that names a pin that isn't there.
            let flow = match (
                flow.needs_cts() && w.cts.is_none(),
                flow.needs_rts() && w.rts.is_none(),
            ) {
                (false, false) => flow,
                _ => UsartFlow::None,
            };
            let mut cfg_owned;
            let cfg = match cfg {
                Some(c) if c.flow != flow => {
                    cfg_owned = c.clone();
                    cfg_owned.flow = flow;
                    Some(&cfg_owned)
                }
                other => other,
            };
            let sfx = cfg.map(|c| label_sfx(&c.custom_label)).unwrap_or_default();
            let handle = serial_handle(peri, n, &sfx);
            // The pin arguments, in the order every template declares them:
            // rx, tx (whichever the direction uses), then the flow pads.
            let mut pin_args = String::new();
            if dir.needs_rx() {
                let pin = rx.unwrap_or_default();
                consumed.push(pin.to_owned());
                pin_args.push_str(&format!(", p.{pin}"));
            }
            if dir.needs_tx() {
                let pin = tx.unwrap_or_default();
                consumed.push(pin.to_owned());
                pin_args.push_str(&format!(", p.{pin}"));
            }
            // RTS/DE first, then CTS — `serial_shape` lists the parameters in
            // that order because embassy's constructors do.
            if flow.needs_rts()
                && let Some(pin) = &w.rts
            {
                consumed.push(pin.clone());
                pin_args.push_str(&format!(", p.{pin}"));
            }
            if flow.needs_cts()
                && let Some(pin) = &w.cts
            {
                consumed.push(pin.clone());
                pin_args.push_str(&format!(", p.{pin}"));
            }
            if cfg.map(|c| c.mode) == Some(UsartMode::Dma) {
                any_async_dma = true;
                serial_instances.push((peri, n, false));
                let (args, note) = dma_args(
                    &mut alloc,
                    &mut dma_binds,
                    &mut dma_uses,
                    bus,
                    n,
                    &format!("{peri}{n}"),
                    manual_channels(cfg, |c| (&c.dma_tx, &c.dma_rx)),
                    dir.dma_halves(),
                );
                calls.push_str(&note);
                // Full duplex hands back a PAIR (the halves have different
                // types); one-way hands back the single half.
                let bind = if dir == UsartDirection::TxRx {
                    format!("let (mut {handle}_tx, mut {handle}_rx)")
                } else {
                    format!("let mut {handle}")
                };
                calls.push_str(&format!(
                    "    {bind} = pins::configs::{stem}{n}::init(p.{peri}{n}{pin_args}, {args});
"
                ));
            } else {
                serial_instances.push((peri, n, true));
                calls.push_str(&format!(
                    "    let mut {handle} = pins::configs::{stem}{n}::init(p.{peri}{n}{pin_args}, Irqs);
"
                ));
            }
            files.push((
                format!("{stem}{n}.rs"),
                serial_config_file(
                    peri,
                    n,
                    cfg,
                    &serial_irq(chip.irq_vectors, peri, n),
                    stm32_pin_data::usart_has_swap_invert(chip.usart_ip),
                ),
            ));
        }
    }

    // ── PWM ──────────────────────────────────────────────────────────────
    // One config module per TIMER, taking exactly the channels wired on the
    // canvas. No interrupts and no DMA: `SimplePwm` writes the compare
    // registers directly, so there is nothing to bind.
    let time_driver = time_driver_timer(pins);
    for (n, mut wiring) in pwm_wires(pins) {
        // embassy-time runs on a timer of its own, and the one it takes is no
        // longer a field of `Peripherals`: `p.TIM4` on an STM32F103C8 is E0609.
        // Its pads stay bound raw, and main.rs says why.
        if Some(n) == time_driver {
            calls.push_str(&format!(
                "    // TIM{n} is NOT initialised: embassy-time runs on it. The \"time-driver-any\"
    // feature takes TIM{n} on this chip, so `p.TIM{n}` does not exist. Move the PWM
    // to another timer.
"
            ));
            continue;
        }
        // A timer with pads but no module entry generates at the module's own
        // defaults, so the defaults live in exactly one place.
        let cfg = timer
            .get(&n)
            .cloned()
            .unwrap_or_else(|| TimerModuleConfig::new(n));
        let handle = format!("_pwm{n}{}", label_sfx(&cfg.custom_label));

        // The F1's timer pads have no alternate-function number - AFIO remaps
        // them in groups - and a pad can sit in two groups at once (TIM3 CH3 is
        // PB0 unremapped AND partly remapped), so the remap has to be NAMED:
        // rustc does not infer one from pads that fit two (E0283).
        let f1_remap = if family == "stm32f1" {
            if !wiring.comp.is_empty() || !wiring.breaks.is_empty() {
                calls.push_str(&format!(
                    "    // TIM{n}'s complementary and break pads are left unconfigured: on the F1
    // they are remapped in groups of their own, which Async does not generate yet.
    // The plain channels below are unaffected.
"
                ));
                wiring.comp.clear();
                wiring.breaks.clear();
            }
            if wiring.chans.is_empty() {
                // Only complementary or break pads: the note above covers them.
                continue;
            }
            let pads: Vec<(u8, &str)> = wiring
                .chans
                .iter()
                .map(|(c, pin)| (*c, pin.as_str()))
                .collect();
            let remap = match n {
                // No AFIO field for these two: embassy types their pads
                // `AfioRemapNotApplicable` (compiled on an STM32F103RE).
                5 | 8 => Some("AfioRemapNotApplicable"),
                1..=4 => super::stm32::pwm_remap(n, &pads).and_then(embassy_afio_remap),
                _ => {
                    calls.push_str(&format!(
                        "    // TIM{n} is NOT initialised: its F1 remap lives in AFIO_MAPR2, which the
    // Async runtime does not generate yet. The pads are bound raw.
"
                    ));
                    continue;
                }
            };
            match remap {
                Some(remap) => Some(remap),
                None => {
                    calls.push_str(&format!(
                        "    // TIM{n} is NOT initialised: its pads are not one AFIO remap set, and the
    // F1 remaps a timer's channels together. Keep them on one row of the
    // reference manual's TIM{n} remap table.
"
                    ));
                    continue;
                }
            }
        } else {
            None
        };

        // Complementary outputs reach embassy through `ComplementaryPwm`, whose
        // bound is `AdvancedInstance4Channel` — the advanced-control timers. A
        // CHxN pad on TIM15/16/17 is real silicon, but there is no driver for it
        // here, so say that instead of emitting code that will not compile.
        if wiring.needs_complementary() && !is_advanced_timer(n) {
            let pads: Vec<String> = wiring
                .comp
                .iter()
                .map(|(c, pin)| format!("CH{c}N ({pin})"))
                .chain(wiring.breaks.iter().map(|(i, pin)| {
                    let sfx = if *i == 1 {
                        String::new()
                    } else {
                        i.to_string()
                    };
                    format!("BKIN{sfx} ({pin})")
                }))
                .collect();
            calls.push_str(&format!(
                "    // TIM{n} {} left unconfigured: embassy drives complementary outputs and
    // break inputs through `ComplementaryPwm`, which covers the advanced-control
    // timers (TIM1/8/20) only. The plain channels below are unaffected.
",
                pads.join(", ")
            ));
            wiring.comp.clear();
            wiring.breaks.clear();
        }

        let params = wiring.params();
        let args: String = params
            .iter()
            .map(|(_, _, pin, _)| format!(", p.{pin}"))
            .collect();
        for (_, _, pin, _) in &params {
            consumed.push((*pin).to_owned());
        }
        // With a break pad the config module also hands back the pads it put
        // into alternate-function mode; they have to stay alive, so they get a
        // binding of their own rather than being dropped on the spot.
        let lhs = if wiring.breaks.is_empty() {
            format!("let mut {handle}")
        } else {
            format!("let (mut {handle}, {handle}_break_pads)")
        };
        calls.push_str(&format!(
            "    {lhs} = pins::configs::pwm{n}::init(p.TIM{n}{args});
"
        ));
        let body = pwm_config_file(n, &cfg, &wiring, &handle);
        let body = match f1_remap {
            Some(remap) => with_timer_remap(&body, n, remap),
            None => body,
        };
        files.push((format!("pwm{n}.rs"), body));
    }

    // ── HSPI ─────────────────────────────────────────────────────────────
    // The narrowest of the four memory controllers to generate, because the
    // driver is: embassy has two constructors and nothing between them, and the
    // octal one REQUIRES the strobe. Sixteen data pads exist in silicon; none of
    // the ones past IO7 has a call that takes it.
    for (n, w) in hspi_wires(pins) {
        let cfg = hspi
            .get(&n)
            .cloned()
            .unwrap_or_else(|| HspiModuleConfig::new(n));
        let handle = format!("_hspi{n}{}", label_sfx(&cfg.custom_label));
        let want = cfg.mode.lanes();
        if w.io.len() as u8 != want {
            calls.push_str(&format!(
                "    // HSPI{n} is NOT initialised: the module is set to {} and that needs
    // {want} data lines, but {} are wired. embassy builds 2 or 8, nothing between.
",
                cfg.mode.label(),
                w.io.len()
            ));
            continue;
        }
        let Some(ncs) = w.ncs.clone() else {
            calls.push_str(&format!(
                "    // HSPI{n} is NOT initialised: no chip select is wired.
"
            ));
            continue;
        };
        // The octal call takes DQS0 as an ORDINARY argument, not an Option: with
        // no strobe wired there is no constructor to call at all.
        let dqs0 = w.dqs.get(&0).cloned();
        let octal = cfg.mode == crate::panels::mcu_module::modules::HspiMode::Octal;
        if octal && dqs0.is_none() {
            calls.push_str(&format!(
                "    // HSPI{n} is NOT initialised: the octal call requires DQS0, and no data
    // strobe is wired. Assign HSPI{n}_DQS0 to a pad, or drop to the single width.
"
            ));
            continue;
        }

        let mut pins_used = vec![w.clk.clone()];
        pins_used.extend(w.io.values().cloned());
        pins_used.push(ncs.clone());
        if octal {
            pins_used.push(dqs0.clone().expect("checked above"));
        }
        for p in &pins_used {
            consumed.push(p.clone());
        }
        let args: String = pins_used.iter().map(|p| format!(", p.{p}")).collect();
        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::hspi{n}::init(p.HSPI{n}{args});\n"
        ));
        files.push((format!("hspi{n}.rs"), hspi_config_file(n, &cfg)));
    }

    // ── XSPI ─────────────────────────────────────────────────────────────
    // The OCTOSPI block one step wider. Same rule: the wiring narrows the mode,
    // the mode names the constructor — and here the STROBES pick the suffix.
    for (n, w) in xspi_wires(pins) {
        let cfg = xspi
            .get(&n)
            .cloned()
            .unwrap_or_else(|| XspiModuleConfig::new(n));
        let handle = format!("_xspi{n}{}", label_sfx(&cfg.custom_label));
        let want = cfg.mode.lanes();
        if w.io.len() as u8 != want {
            calls.push_str(&format!(
                "    // XSPI{n} is NOT initialised: the module is set to {} and that needs
    // {want} data lines, but {} are wired.
",
                cfg.mode.label(),
                w.io.len()
            ));
            continue;
        }
        let Some(ncs) = w.ncs.values().next().cloned() else {
            calls.push_str(&format!(
                "    // XSPI{n} is NOT initialised: no chip select is wired. Either NCS1 or NCS2
    // will do - the controller is told which one it got.
"
            ));
            continue;
        };
        // The strobes pick the suffix, and only the wide modes have one.
        let dqs: Vec<String> = if cfg.mode.takes_dqs() {
            w.dqs.values().cloned().collect()
        } else {
            Vec::new()
        };
        let dual_dqs =
            dqs.len() == 2 && cfg.mode == crate::panels::mcu_module::modules::XspiMode::Hexa;
        let used_dqs = if dual_dqs {
            2
        } else {
            usize::from(!dqs.is_empty())
        };

        let mut pins_used = vec![w.clk.clone()];
        pins_used.extend(w.io.values().cloned());
        pins_used.push(ncs.clone());
        pins_used.extend(dqs.iter().take(used_dqs).cloned());
        for p in &pins_used {
            consumed.push(p.clone());
        }
        let args: String = pins_used.iter().map(|p| format!(", p.{p}")).collect();
        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::xspi{n}::init(p.XSPI{n}{args});\n"
        ));
        files.push((format!("xspi{n}.rs"), xspi_config_file(n, &cfg, used_dqs)));
    }

    // ── OCTOSPI ──────────────────────────────────────────────────────────
    // The width narrows the mode but does not decide it — single and dual share
    // two pads, octal and dual-quad share eight — so the module says which, and
    // the wiring has to be able to carry it.
    for (n, w) in ospi_wires(pins) {
        let cfg = ospi
            .get(&n)
            .cloned()
            .unwrap_or_else(|| OspiModuleConfig::new(n));
        let handle = format!("_ospi{n}{}", label_sfx(&cfg.custom_label));
        let want = cfg.mode.lanes();
        if w.io.len() as u8 != want {
            calls.push_str(&format!(
                "    // OCTOSPI{n} is NOT initialised: the module is set to {} and that needs
    // {want} data lines, but {} are wired.
",
                cfg.mode.label(),
                w.io.len()
            ));
            continue;
        }
        // DQS is read only by the octal mode; anywhere else the pad stays free
        // rather than becoming an argument no constructor takes.
        let with_dqs =
            cfg.mode == crate::panels::mcu_module::modules::OspiMode::Octal && w.dqs.is_some();

        let mut pins_used = vec![w.clk.clone()];
        pins_used.extend(w.io.values().cloned());
        pins_used.push(w.ncs.clone());
        if with_dqs {
            pins_used.push(w.dqs.clone().unwrap());
        }
        for p in &pins_used {
            consumed.push(p.clone());
        }

        // embassy's order: clock, then the data lines, then the chip select,
        // then the strobe.
        let mut args = format!(", p.{}", w.clk);
        for pin in w.io.values() {
            args.push_str(&format!(", p.{pin}"));
        }
        args.push_str(&format!(", p.{}", w.ncs));
        if with_dqs {
            args.push_str(&format!(", p.{}", w.dqs.clone().unwrap()));
        }
        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::ospi{n}::init(p.OCTOSPI{n}{args});\n"
        ));
        files.push((format!("ospi{n}.rs"), ospi_config_file(n, &cfg, with_dqs)));
    }

    // ── QUADSPI ──────────────────────────────────────────────────────────
    // Which BANKS are fully wired is which constructor: a bank is a chip
    // select and four data lines, and half of one drives nothing.
    if let Some(w) = qspi_wires(pins) {
        let owned;
        let cfg = match qspi {
            Some(c) => c,
            None => {
                owned = QspiModuleConfig::new(1);
                &owned
            }
        };
        let handle = format!("_qspi{}", label_sfx(&cfg.custom_label));
        match (w.bank1.as_ref(), w.bank2.as_ref()) {
            (None, None) => {
                calls.push_str(
                    "    // QUADSPI is NOT initialised: neither bank is complete. A bank is its
    // chip select AND all four data lines - half of one drives nothing.
",
                );
            }
            (b1, b2) => {
                let mut pin_args = vec![w.clk.clone()];
                for b in [b1, b2].into_iter().flatten() {
                    pin_args.extend(b.io.values().cloned());
                    pin_args.push(b.ncs.clone());
                }
                for p in &pin_args {
                    consumed.push(p.clone());
                }
                // The order embassy declares: data lines first, then the clock,
                // then the chip select(s) — and dual bank interleaves both
                // banks' data before them.
                let mut args = String::new();
                for b in [b1, b2].into_iter().flatten() {
                    for pin in b.io.values() {
                        args.push_str(&format!(", p.{pin}"));
                    }
                }
                args.push_str(&format!(", p.{}", w.clk));
                for b in [b1, b2].into_iter().flatten() {
                    args.push_str(&format!(", p.{}", b.ncs));
                }
                calls.push_str(&format!(
                    "    let mut {handle} = pins::configs::qspi::init(p.QUADSPI{args});\n"
                ));
                files.push((
                    "qspi.rs".to_owned(),
                    qspi_config_file(cfg, b1.is_some(), b2.is_some(), &handle),
                ));
            }
        }
    }

    // ── SDMMC ────────────────────────────────────────────────────────────
    // The one peripheral whose IP version changes the ARGUMENT LIST rather than
    // a setting: the older controller is fed a DMA channel and has to bind its
    // interrupt too, the newer one has its own inside. With no captured version
    // the IDE cannot know which, so it says so and emits nothing.
    for (n, w) in sdmmc_wires(pins) {
        let cfg = sdmmc
            .get(&n)
            .cloned()
            .unwrap_or_else(|| SdmmcModuleConfig::new(n));
        let peri = sdmmc_peri(n);
        let handle = format!("_sd{n}{}", label_sfx(&cfg.custom_label));

        // The F1's SDIO is the older controller, which takes a DMA channel -
        // and embassy-stm32 0.6 routes none to it on the F1 (no `SdmmcDma`
        // impl in its F103 build), so no call could ever be completed.
        if family == "stm32f1" {
            calls.push_str(&format!(
                "    // {peri} is NOT initialised: embassy-stm32 gives the F1's SDIO no DMA
    // channel, and its driver needs one. The pads are bound raw.
"
            ));
            continue;
        }

        let Some(width) = sd_bus_width(&w.lanes) else {
            calls.push_str(&format!(
                "    // {peri} is NOT initialised: {} data line(s) are wired, and the controller
    // takes 1, 4 or 8 — wire D0 alone, D0-D3, or D0-D7.
",
                w.lanes.len()
            ));
            continue;
        };
        let Some(kind) = chip.sdmmc_ip.and_then(stm32_pin_data::sdmmc_kind) else {
            calls.push_str(&format!(
                "    // {peri} is NOT initialised: this chip carries no SDMMC IP version, and the
    // two versions take DIFFERENT arguments (the older one a DMA channel, the
    // newer none). Re-import the chip from the STM32Cube database.
"
            ));
            continue;
        };

        for pin in [&w.ck, &w.cmd].into_iter().chain(w.lanes.values()) {
            consumed.push(pin.clone());
        }
        let pin_args: String = [&w.ck, &w.cmd]
            .into_iter()
            .chain(w.lanes.values())
            .map(|p| format!(", p.{p}"))
            .collect();

        // The peripheral's own interrupt goes in the SAME `Irqs` as any DMA
        // channel's, because `init` takes one value for both.
        extra_binds.push((
            peri.clone(),
            format!("embassy_stm32::sdmmc::InterruptHandler<peripherals::{peri}>"),
        ));
        any_async_dma = true;

        let dma_arg = if kind == stm32_pin_data::SdmmcKind::V1 {
            let (arg, note) = dma_args(
                &mut alloc,
                &mut dma_binds,
                &mut dma_uses,
                dma_map::Bus::Sdmmc,
                n,
                &peri,
                manual_channels(Some(&cfg), |c| (&c.dma_tx, &c.dma_rx)),
                (true, false),
            );
            calls.push_str(&note);
            // `dma_args` ends with the shared `Irqs`; here it has to sit AFTER
            // the channel and before the pins, so it is re-placed by hand.
            format!("{}, ", arg.replace(", Irqs", ""))
        } else {
            String::new()
        };

        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::sd{n}::init(p.{peri}, {dma_arg}Irqs{pin_args});\n"
        ));
        files.push((
            format!("sd{n}.rs"),
            sdmmc_config_file(n, &cfg, &w, width, kind),
        ));
    }

    // ── SAI ──────────────────────────────────────────────────────────────
    // One unit, up to two independent sub-blocks. `split_subblocks` happens
    // once inside the config module, which is why the module is the unit.
    for (n, blocks) in sai_wires(pins) {
        let cfg = sai
            .get(&n)
            .cloned()
            .unwrap_or_else(|| SaiModuleConfig::new(n));
        let sfx = label_sfx(&cfg.custom_label);
        let mut args = String::new();
        let mut handles = Vec::new();
        for (b, w) in &blocks {
            let letter = if *b == 1 { "a" } else { "b" };
            handles.push(format!("mut _sai{n}{letter}{sfx}"));
            for pin in [Some(&w.sck), Some(&w.sd), Some(&w.fs), w.mclk.as_ref()]
                .into_iter()
                .flatten()
            {
                args.push_str(&format!(", p.{pin}"));
                consumed.push(pin.clone());
            }
            let (dma_arg, note) = dma_args(
                &mut alloc,
                &mut dma_binds,
                &mut dma_uses,
                dma_map::Bus::Spi,
                n,
                &format!("SAI{n}{}", letter.to_uppercase()),
                manual_channels(Some(&cfg), |c| (&c.dma_a, &c.dma_b)),
                (*b == 1, *b != 1),
            );
            calls.push_str(&note);
            args.push_str(&format!(", {dma_arg}"));
        }
        // `dma_args` already appended the shared `Irqs`; only the last one is
        // wanted, since `init` takes a single binding value.
        let args = args.replace(", Irqs", "");
        any_async_dma = true;
        let lhs = if handles.len() == 1 {
            format!("let {}", handles[0])
        } else {
            format!("let ({})", handles.join(", "))
        };
        calls.push_str(&format!(
            "    {lhs} = pins::configs::sai{n}::init(p.SAI{n}{args}, Irqs);\n"
        ));
        files.push((format!("sai{n}.rs"), sai_config_file(n, &cfg, &blocks)));
    }

    // ── DAC ──────────────────────────────────────────────────────────────
    // No DMA and no interrupt: `new_blocking` writes the data register, which
    // is the whole peripheral for a set-point or a bias.
    for (n, chans) in dac_wires(pins) {
        let cfg = dac
            .get(&n)
            .cloned()
            .unwrap_or_else(|| DacModuleConfig::new(n));
        let handle = format!("_dac{n}{}", label_sfx(&cfg.custom_label));
        let args: String = chans.iter().map(|(_, pin)| format!(", p.{pin}")).collect();
        for (_, pin) in &chans {
            consumed.push(pin.clone());
        }
        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::dac{n}::init(p.DAC{n}{args});\n"
        ));
        files.push((
            format!("dac{n}.rs"),
            dac_config_file(n, &cfg, &chans, &handle),
        ));
    }

    // ── I2S ──────────────────────────────────────────────────────────────
    // One SPI block driven as audio, always through DMA: embassy has no
    // blocking I2S at all.
    for (n, w) in i2s_wires(pins) {
        let cfg = i2s
            .get(&n)
            .cloned()
            .unwrap_or_else(|| I2sModuleConfig::new(n));
        let handle = format!("_i2s{n}{}", label_sfx(&cfg.custom_label));

        // I2S{n} and SPI{n} are the same silicon. Both wired means the user
        // described one block twice; SPI keeps it, because that is what the
        // block is unless told otherwise, and the audio side says so out loud
        // rather than emitting a second `p.SPI{n}` that will not compile.
        if spi_wires(pins).iter().any(|(i, ..)| *i == n) {
            calls.push_str(&format!(
                "    // I2S{n} is NOT initialised: it runs on SPI{n}, which a SPI module already
    // claims. One block cannot be both — remove one of the two modules.
"
            ));
            continue;
        }

        let tx = cfg.direction.is_tx();
        let (dma_arg, note) = dma_args(
            &mut alloc,
            &mut dma_binds,
            &mut dma_uses,
            dma_map::Bus::Spi,
            n,
            &format!("I2S{n}"),
            manual_channels(Some(&cfg), |c| (&c.dma_tx, &c.dma_rx)),
            (tx, !tx),
        );
        any_async_dma = true;
        calls.push_str(&note);

        let mck_arg = w
            .mck
            .as_ref()
            .map(|p| format!(", p.{p}"))
            .unwrap_or_default();
        for pin in [Some(&w.sd), Some(&w.ws), Some(&w.ck), w.mck.as_ref()]
            .into_iter()
            .flatten()
        {
            consumed.push(pin.clone());
        }
        calls.push_str(&format!(
            "    let mut {handle} = pins::configs::i2s{n}::init(p.SPI{n}, p.{}, p.{}, p.{}{mck_arg}, {dma_arg});\n",
            w.sd, w.ws, w.ck
        ));
        files.push((format!("i2s{n}.rs"), i2s_config_file(n, &cfg, &w, &handle)));
    }

    for (n, sck, mosi, miso) in spi_wires(pins) {
        any_spi_i2c = true;
        consumed.push(sck.clone());
        consumed.push(mosi.clone());
        if let Some(p) = &miso {
            consumed.push(p.clone());
        }
        // No MISO pin, no receiver: embassy's `new_txonly` takes one channel,
        // and `read`/`transfer` on such a bus would panic (`rx_dma.unwrap()`),
        // which is why this returns a different value - see `spi_config_file`.
        let tx_only = miso.is_none();
        let miso_arg = miso
            .as_ref()
            .map(|p| format!(", p.{p}"))
            .unwrap_or_default();
        let cfg = spi.get(&n);
        let sfx = cfg.map(|c| label_sfx(&c.custom_label)).unwrap_or_default();
        let dma = cfg.map(|c| is_dma(c.async_mode)).unwrap_or(false);
        any_async_dma |= dma;
        if dma {
            let (args, note) = dma_args(
                &mut alloc,
                &mut dma_binds,
                &mut dma_uses,
                dma_map::Bus::Spi,
                n,
                &format!("SPI{n}"),
                manual_channels(cfg, |c| (&c.dma_tx, &c.dma_rx)),
                (true, !tx_only),
            );
            calls.push_str(&note);
            calls.push_str(&format!(
                "    let mut _spi{n}{sfx} = \
                 pins::configs::spi{n}::init(p.SPI{n}, p.{sck}, p.{mosi}{miso_arg}, {args});\n"
            ));
        } else {
            calls.push_str(&format!(
                "    let mut _spi{n}{sfx} = \
                 pins::configs::spi{n}::init(p.SPI{n}, p.{sck}, p.{mosi}{miso_arg});\n"
            ));
        }
        files.push((format!("spi{n}.rs"), spi_config_file(n, cfg, tx_only)));
    }

    for (n, scl, sda) in i2c_wires(pins) {
        any_spi_i2c = true;
        consumed.push(scl.clone());
        consumed.push(sda.clone());
        let cfg = i2c.get(&n);
        let sfx = cfg.map(|c| label_sfx(&c.custom_label)).unwrap_or_default();
        let dma = cfg.map(|c| is_dma(c.async_mode)).unwrap_or(false);
        any_async_dma |= dma;
        if dma {
            dma_i2c_instances.push(n);
            let (args, note) = dma_args(
                &mut alloc,
                &mut dma_binds,
                &mut dma_uses,
                dma_map::Bus::I2c,
                n,
                &format!("I2C{n}"),
                manual_channels(cfg, |c| (&c.dma_tx, &c.dma_rx)),
                (true, true),
            );
            calls.push_str(&note);
            calls.push_str(&format!(
                "    let mut _i2c{n}{sfx} = \
                 pins::configs::i2c{n}::init(p.I2C{n}, p.{scl}, p.{sda}, {args});\n"
            ));
        } else {
            calls.push_str(&format!(
                "    let mut _i2c{n}{sfx} = \
                 pins::configs::i2c{n}::init(p.I2C{n}, p.{scl}, p.{sda});\n"
            ));
        }
        files.extend(super::common::i2c_bus_files(
            &format!("i2c{n}"),
            i2c_config_file(n, cfg),
            cfg,
        ));
    }

    // ── Comparators ────────────────────────────────────────────────────
    // Not derived from pins the way the buses are: the settings come from the
    // Configuration tab, and only the INP pin comes from the canvas. A
    // comparator whose pin is missing is skipped here rather than emitted
    // half-wired - the card says so.
    let mut comp_binds: Vec<(String, u8)> = Vec::new();
    if let Some(generation) = comparator::Generation::of(family) {
        for (n, cfg) in comp.settings {
            let Some((_, inp, inm)) = comp.pins.iter().find(|(i, _, _)| i == n) else {
                continue;
            };
            if cfg.inverting_input.needs_pin() && inm.is_none() {
                continue;
            }
            consumed.push(inp.clone());
            let inm_arg = match (cfg.inverting_input.needs_pin(), inm) {
                (true, Some(p)) => {
                    consumed.push(p.clone());
                    format!(", p.{p}")
                }
                _ => String::new(),
            };
            comp_binds.push((comp_irq(chip.irq_vectors, *n, comp.instances), *n));
            calls.push_str(&format!(
                "    let mut _comp{n} = pins::configs::comp{n}::init(p.COMP{n}, p.{inp}{inm_arg}, Irqs);\n"
            ));
            files.push((format!("comp{n}.rs"), comp_config_file(*n, cfg, generation)));
        }
    }

    // The struct is needed as soon as ANYTHING binds an interrupt, which is no
    // longer only the DMA path.
    let dma_irqs = if any_async_dma
        || !comp_binds.is_empty()
        || !serial_instances.is_empty()
        || !extra_binds.is_empty()
    {
        dma_irqs_block(
            &dma_i2c_instances,
            &serial_instances,
            &dma_binds,
            chip.irq_vectors,
            &comp_binds,
            &extra_binds,
            calls.contains("DMA_TX_TODO"),
        )
    } else {
        String::new()
    };
    // An F1 project switched to Async keeps its USB and CAN modules on the
    // canvas, but no async template exists for either: their pads are bound
    // raw below, and this says so rather than letting them vanish.
    if family == "stm32f1" {
        let wired = |fs: [PinFunction; 2]| {
            pins.iter()
                .any(|p| !p.reserved && fs.contains(&p.selected_function))
        };
        if wired([PinFunction::UsbDm, PinFunction::UsbDp]) {
            calls.push_str(
                "    // USB is NOT initialised: the Async runtime has no USB template for the F1
    // yet (embassy-stm32 has the driver). The Blocking runtime generates it.
",
            );
        }
        if wired([PinFunction::CanRx, PinFunction::CanTx]) {
            calls.push_str(
                "    // CAN is NOT initialised: the Async runtime has no CAN template for the F1
    // yet (embassy-stm32 has bxCAN). The Blocking runtime generates it.
",
            );
        }
    }
    let init_calls = if calls.is_empty() {
        String::new()
    } else {
        format!("    // ── Peripheral initialisation ──\n{calls}")
    };
    // Said where the pin is, not swallowed: an armed input that could not be
    // given an EXTI is a wiring problem, and the canvas is where it is fixed.
    let init_calls = format!("{init_calls}{exti_notes}");
    // The F1's pin traits carry its AFIO remap as one more parameter; see
    // `with_afio_remap`. The PWM files already got theirs in the loop above.
    if family == "stm32f1" {
        for (_, body) in files.iter_mut() {
            *body = with_afio_remap(body);
        }
    }
    AsyncPeriphs {
        consumed_pins: consumed,
        init_calls,
        config_files: files,
        any_async_dma,
        dma_irqs,
        any_spi_i2c,
        dma_uses,
        exti,
    }
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
const ASYNC_USART_TMPL_DMA: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 7, 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
{EXTRA_CONSTS}// Bytes the DMA controller can receive without the CPU touching them.
pub const RX_DMA_BUF: usize = {BUF};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns (TX, RX). Both implement the STANDARD `embedded-io-async`
// traits, so your application code stays portable:
//
//     async fn send<W: embedded_io_async::Write>(w: &mut W) { /* … */ }
//     async fn recv<R: embedded_io_async::Read>(r: &mut R) { /* … */ }
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::mode::Async;
use embassy_stm32::usart::{
    Config, DataBits, Instance, InterruptHandler, Parity, RingBufferedUartRx, RxDma, RxPin,
    StopBits, TxDma, TxPin, Uart, UartTx,
};
{FLOW_USE}
use embassy_stm32::{peripherals, Peri};
use static_cell::StaticCell;

fn get_config() -> Config {
    let mut config = Config::default();
    config.baudrate = BAUDRATE;
    config.data_bits = match DATA_BITS {
        7 => DataBits::DataBits7,
        9 => DataBits::DataBits9,
        _ => DataBits::DataBits8,
    };
    config.parity = match PARITY {
        'O' => Parity::ParityOdd,
        'E' => Parity::ParityEven,
        _ => Parity::ParityNone,
    };
    config.stop_bits = match STOP_BITS {
        2 => StopBits::STOP2,
        _ => StopBits::STOP1,
    };
{EXTRA_CONFIG}    config
}

/// Initialise {PERI} on DMA, as (TX, RX).
///
/// No `bind_interrupts!` here: embassy takes ONE value that must bind this
/// peripheral's interrupt AND both DMA channels', and only `main.rs` knows which
/// channels those are — so the whole `Irqs` lives there.
pub fn init<'d, TxD: TxDma<peripherals::{PERI}>, RxD: RxDma<peripherals::{PERI}>>(
    usart: Peri<'d, peripherals::{PERI}>,
{PIN_PARAMS}{FLOW_PARAMS}    tx_dma: Peri<'d, TxD>,
    rx_dma: Peri<'d, RxD>,
    irqs: impl Binding<
            <peripherals::{PERI} as Instance>::Interrupt,
            InterruptHandler<peripherals::{PERI}>,
        > + Binding<TxD::Interrupt, DmaInterruptHandler<TxD>>
        + Binding<RxD::Interrupt, DmaInterruptHandler<RxD>>
        + 'd,
) -> (UartTx<'d, Async>, RingBufferedUartRx<'d>) {
    let uart = {CTOR}.unwrap();
    let (tx, rx) = uart.split();
    // `'static` so the DMA controller can own it for the program's lifetime.
    static RX_BUF: StaticCell<[u8; RX_DMA_BUF]> = StaticCell::new();
    (tx, rx.into_ring_buffered(RX_BUF.init([0; RX_DMA_BUF])))
}

// ── Using {PERI} ──
// `init` gives you (tx, rx). Reception runs in the background from the moment
// it returns, so a slow reader loses nothing as long as RX_DMA_BUF holds.
//
//     use embedded_io_async::{Read, Write};
//
//     let (mut tx, mut rx) = ({HANDLE}_tx, {HANDLE}_rx);
//     tx.write_all(b"hello\r\n").await.ok();
//
//     let mut buf = [0u8; 32];
//     let n = rx.read(&mut buf).await.unwrap(); // as much as has arrived
//
// ── Sending without the DMA ──
// The same handle also writes straight from the CPU, which is often the better
// trade for a short burst: a DMA write costs a descriptor setup and an
// interrupt, and it yields to the executor, so a handful of bytes can leave
// sooner this way.
//
//     {HANDLE}_tx.blocking_write(b"hi\r\n").ok();
//
// It does NOT free the channel: embassy takes both when it builds a
// bidirectional UART, and this handle keeps its TX one either way. To spend one
// channel instead of two, set the module's Data Direction to RX only (or TX
// only) — that builds half a UART, and half a UART takes half the channels.
"#;

/// `configs/usart{n}.rs` — or `configs/lpuart{n}.rs`, which is the same file
/// with a different peripheral: embassy drives an LPUART through the very same
/// `usart::{Uart, BufferedUart}` API, so `peri` ("USART" / "LPUART") is the only
/// difference between the two.
/// `configs/{PERI}.rs` for a **TX-only** DMA UART: one pin, one channel.
///
/// A separate template rather than a flag on the full-duplex one because the
/// return type is the point — half a UART is a different value, not the same
/// value with a `None` in it.
const ASYNC_USART_TMPL_DMA_TX: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 7, 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
{EXTRA_CONSTS}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// TRANSMIT ONLY: the RX pad is free for something else. `init` returns a value
// implementing `embedded_io_async::Write`.
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::mode::Async;
use embassy_stm32::usart::{Config, DataBits, Parity, StopBits, TxDma, TxPin, UartTx};
use embassy_stm32::{peripherals, Peri};
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
{FLOW_USE}

fn get_config() -> Config {
    let mut config = Config::default();
    config.baudrate = BAUDRATE;
    config.data_bits = match DATA_BITS {
        7 => DataBits::DataBits7,
        9 => DataBits::DataBits9,
        _ => DataBits::DataBits8,
    };
    config.parity = match PARITY {
        'O' => Parity::ParityOdd,
        'E' => Parity::ParityEven,
        _ => Parity::ParityNone,
    };
    config.stop_bits = match STOP_BITS {
        2 => StopBits::STOP2,
        _ => StopBits::STOP1,
    };
{EXTRA_CONFIG}    config
}

/// Initialise {PERI} as a TX-only DMA UART.
pub fn init<'d, TxD: TxDma<peripherals::{PERI}>>(
    usart: Peri<'d, peripherals::{PERI}>,
    tx: Peri<'d, impl TxPin<peripherals::{PERI}>>,
{FLOW_PARAMS}    tx_dma: Peri<'d, TxD>,
    irqs: impl Binding<TxD::Interrupt, DmaInterruptHandler<TxD>> + 'd,
) -> UartTx<'d, Async> {
    {CTOR}.unwrap()
}

// ── Using {PERI} ──
//     use embedded_io_async::Write;
//     {HANDLE}.write_all(b"hello\r\n").await.ok();
"#;

/// `configs/{PERI}.rs` for a **RX-only** DMA UART. The receiver is ring-buffered
/// for the same reason the full-duplex one is: a bare `UartRx` has no
/// `embedded_io_async::Read`, and reception must not stop between reads.
const ASYNC_USART_TMPL_DMA_RX: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 7, 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
{EXTRA_CONSTS}// Bytes the DMA controller can receive without the CPU touching them.
pub const RX_DMA_BUF: usize = {BUF};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// RECEIVE ONLY: the TX pad is free for something else. `init` returns a value
// implementing `embedded_io_async::Read`.
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::usart::{
    Config, DataBits, Instance, InterruptHandler, Parity, RingBufferedUartRx, RxDma, RxPin,
    StopBits, UartRx,
};
use embassy_stm32::{peripherals, Peri};
use static_cell::StaticCell;
{FLOW_USE}

fn get_config() -> Config {
    let mut config = Config::default();
    config.baudrate = BAUDRATE;
    config.data_bits = match DATA_BITS {
        7 => DataBits::DataBits7,
        9 => DataBits::DataBits9,
        _ => DataBits::DataBits8,
    };
    config.parity = match PARITY {
        'O' => Parity::ParityOdd,
        'E' => Parity::ParityEven,
        _ => Parity::ParityNone,
    };
    config.stop_bits = match STOP_BITS {
        2 => StopBits::STOP2,
        _ => StopBits::STOP1,
    };
{EXTRA_CONFIG}    config
}

/// Initialise {PERI} as an RX-only DMA UART with a ring buffer.
pub fn init<'d, RxD: RxDma<peripherals::{PERI}>>(
    usart: Peri<'d, peripherals::{PERI}>,
    rx: Peri<'d, impl RxPin<peripherals::{PERI}>>,
{FLOW_PARAMS}    rx_dma: Peri<'d, RxD>,
    irqs: impl Binding<
            <peripherals::{PERI} as Instance>::Interrupt,
            InterruptHandler<peripherals::{PERI}>,
        > + Binding<RxD::Interrupt, DmaInterruptHandler<RxD>>
        + 'd,
) -> RingBufferedUartRx<'d> {
    let rx = {CTOR}.unwrap();
    // `'static` so the DMA controller can own it for the program's lifetime.
    static RX_BUF: StaticCell<[u8; RX_DMA_BUF]> = StaticCell::new();
    rx.into_ring_buffered(RX_BUF.init([0; RX_DMA_BUF]))
}

// ── Using {PERI} ──
//     use embedded_io_async::Read;
//     let mut buf = [0u8; 32];
//     let n = {HANDLE}.read(&mut buf).await.unwrap();
"#;

/// The extra pin parameters, the `use` they need and the constructor call for
/// one `(transport, direction, flow)` combination.
///
/// Every string here mirrors an embassy signature EXACTLY, and the signatures
/// are not uniform: `BufferedUart::new` takes the buffers before the interrupt
/// binding, while its `new_with_*` variants take the binding first. Getting that
/// wrong compiles nowhere and reads fine — which is why the combinations are
/// listed rather than assembled from rules.
fn serial_shape(peri_ty: &str, cfg: Option<&UsartModuleConfig>) -> SerialShape {
    let transport = cfg.map(|c| c.mode).unwrap_or_default();
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let flow = cfg.map(|c| c.flow).unwrap_or_default();
    let readback = if cfg.is_some_and(|c| c.half_duplex_readback) {
        "HalfDuplexReadback::Readback"
    } else {
        "HalfDuplexReadback::NoReadback"
    };
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

    let ctor = match (transport, direction, flow) {
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
    SerialShape {
        uses,
        pins,
        flow_params: params,
        ctor,
    }
}

/// The generated pieces of one `init`: the extra `use`, the data-pad parameters,
/// the flow-pad parameters and the constructor call.
struct SerialShape {
    uses: String,
    pins: String,
    flow_params: String,
    ctor: String,
}

/// The line-level extras (`swap_rx_tx`, `invert_tx`, `invert_rx`) as a pair of
/// `(consts, assignments)` — EMPTY unless the user turned one on, so a project
/// that uses none generates exactly what it did before these existed.
///
/// `supported` is the chip gate: embassy declares these `Config` fields only
/// under `#[cfg(any(usart_v3, usart_v4))]`, so on an older USART the assignment
/// would not compile. Codegen re-checks it rather than trusting the UI — a
/// project file can outlive the chip it was written for.
fn serial_line_extras(cfg: Option<&UsartModuleConfig>, supported: bool) -> (String, String) {
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
        if on {
            consts.push_str(&format!(
                "pub const {konst}: bool = true;
"
            ));
            body.push_str(&format!(
                "    config.{field} = {konst};
"
            ));
        }
    }
    (consts, body)
}

pub fn serial_config_file(
    peri: &str,
    n: u8,
    cfg: Option<&UsartModuleConfig>,
    irq: &str,
    // Whether this CHIP's USART has the swap/invert bits at all.
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
    let direction = cfg.map(|c| c.direction).unwrap_or_default();
    let tmpl = match (cfg.map(|c| c.mode).unwrap_or_default(), direction) {
        (UsartMode::Dma, UsartDirection::TxOnly) => ASYNC_USART_TMPL_DMA_TX,
        (UsartMode::Dma, UsartDirection::RxOnly) => ASYNC_USART_TMPL_DMA_RX,
        // Half duplex returns a FULL `Uart` / `BufferedUart` — one pad, both
        // directions — so it reuses the two-way templates; only the pin
        // parameters and the constructor differ, and `serial_shape` owns both.
        (UsartMode::Dma, _) => ASYNC_USART_TMPL_DMA,
        (UsartMode::Buffered, _) => ASYNC_USART_TMPL,
    };
    let peri_ty = format!("{peri}{n}");
    let shape = serial_shape(&peri_ty, cfg);
    let (extra_consts, extra_config) = serial_line_extras(cfg, line_extras);
    tmpl.replace("{EXTRA_CONSTS}", &extra_consts)
        .replace("{EXTRA_CONFIG}", &extra_config)
        .replace("{FLOW_USE}", shape.uses.trim_end())
        .replace("{PIN_PARAMS}", &shape.pins)
        .replace("{FLOW_PARAMS}", &shape.flow_params)
        .replace("{CTOR}", &shape.ctor)
        .replace("{HANDLE}", &serial_handle(peri, n, &sfx))
        .replace("{PERI}", &format!("{peri}{n}"))
        .replace("{UIRQ}", irq)
        .replace("{N}", &n.to_string())
        .replace("{BAUD}", &baud.to_string())
        .replace("{DATA}", &data.to_string())
        .replace("{PARITY}", &parity.to_string())
        .replace("{STOP}", &stop.to_string())
        .replace("{BUF}", &buf.to_string())
}

// ── Async PWM config file (embassy SimplePwm) ─────────────────────────────────

/// `src/pins/configs/pwm{N}.rs`: embassy's `SimplePwm` over ONE timer.
///
/// The whole file is built per timer rather than per channel because the timer
/// is what owns the frequency — one prescaler and one reload value serve all
/// four channels, so a per-channel frequency could not be honoured. Only the
/// channels actually wired on the canvas become parameters; the rest are `None`.
const ASYNC_PWM_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const FREQ_HZ: u32 = {FREQ};
{DUTY_CONSTS}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
{INTRO}
{GPIO_USE}
use embassy_stm32::time::Hertz;
{LOW_LEVEL_USE}
{DRIVER_USE}
{TIMER_USE}
use embassy_stm32::{peripherals, Peri};
{BREAK_STRUCT}
/// Initialise TIM{N} as PWM on the wired pad(s), enabled at the configured duty.
pub fn init<'d>(
    tim: Peri<'d, peripherals::TIM{N}>,
{PARAMS}) -> {RET} {
{PRELUDE}    let mut pwm = {DRIVER}::new(
        tim,
{CH_ARGS}        Hertz(FREQ_HZ),
        CountingMode::{COUNTING},
    );
{ENABLES}    {RET_EXPR}
}
{DUTY_TRAIT}
// ── Using TIM{N} ──
{USAGE}
"#;
/// The advanced-control timers, the only ones embassy's `ComplementaryPwm`
/// accepts: its bound is `AdvancedInstance4Channel`. TIM15/16/17 carry a CH1N
/// pad on real silicon but are 1- and 2-channel advanced timers, which that
/// driver does not cover.
pub fn is_advanced_timer(n: u8) -> bool {
    matches!(n, 1 | 8 | 20)
}

/// Render [`ASYNC_PWM_TMPL`] for timer `n` from its wiring and its module
/// config.
///
/// Two shapes come out of one template. With plain channels only it is
/// `SimplePwm`, exactly as before. As soon as one CHxN pad is wired it becomes
/// `ComplementaryPwm`, whose API is NOT the same: duty is a raw compare value
/// against `get_max_duty()` rather than a ratio, channels are addressed by the
/// `Channel` enum rather than `.chN()`, and there is no output-compare-mode
/// setter at all.
pub fn pwm_config_file(n: u8, cfg: &TimerModuleConfig, wiring: &PwmWiring, handle: &str) -> String {
    let complementary = wiring.needs_complementary();
    let param_list = wiring.params();
    let has_plain = !wiring.chans.is_empty();

    let mut duty_consts = String::new();
    let mut params = String::new();
    let mut ch_args = String::new();
    let mut enables = String::new();
    // Only the `low_level` items the emitted code actually names — an unused
    // import in a generated file is a warning the user cannot fix.
    let mut low_level: Vec<String> = vec!["CountingMode".to_owned()];
    let mut timer_items: Vec<String> = Vec::new();
    let mut gpio_items: Vec<String> = vec!["OutputType".to_owned()];
    let mut prelude = String::new();

    if complementary {
        duty_consts.push_str(&format!(
            "pub const DEAD_TIME: u16 = {}; // timer ticks, same scale as the duty compare value\n",
            cfg.dead_time
        ));
    }
    for ch in wiring.active_channels() {
        let duty = cfg.duty_x100_of(ch);
        duty_consts.push_str(&format!(
            "pub const DUTY_CH{ch}: u32 = {duty}; // {} %, in hundredths (0..=10_000)\n",
            duty_percent_str(duty)
        ));
    }

    for (ch, name, _, comp) in &param_list {
        if name.starts_with("bkin") {
            // The break pad is bounded by `BreakInputPin`, which embassy
            // declares but no driver consumes — see `prelude` below.
            timer_items.push("BreakInputPin".to_owned());
            timer_items.push(format!("BkIn{ch}"));
            params.push_str(&format!(
                "    {name}: Peri<'d, impl BreakInputPin<peripherals::TIM{n}, BkIn{ch}>>,\n"
            ));
            continue;
        }
        let bound = if *comp {
            "TimerComplementaryPin"
        } else {
            "TimerPin"
        };
        timer_items.push(format!("Ch{ch}"));
        timer_items.push(bound.to_owned());
        params.push_str(&format!(
            "    {name}: Peri<'d, impl {bound}<peripherals::TIM{n}, Ch{ch}>>,\n"
        ));
    }

    // The slots, in the order the driver declares them: ch1..ch4 for
    // `SimplePwm`, ch1/ch1n/ch2/ch2n/… for `ComplementaryPwm`. A pad that is
    // not wired is still a slot.
    for ch in 1..=4u8 {
        let out = cfg.channel_of(ch);
        if wiring.chans.iter().any(|(c, _)| *c == ch) {
            ch_args.push_str(&format!(
                "        Some(PwmPin::new(ch{ch}, OutputType::{})),\n",
                out.output.embassy()
            ));
        } else {
            ch_args.push_str("        None,\n");
        }
        if complementary {
            if wiring.comp.iter().any(|(c, _)| *c == ch) {
                ch_args.push_str(&format!(
                    "        Some(ComplementaryPwmPin::new(ch{ch}n, OutputType::{})),\n",
                    out.output.embassy()
                ));
            } else {
                ch_args.push_str("        None,\n");
            }
        }
    }

    if complementary {
        // Dead time first: it has to be in the register before an output can
        // turn on, or the first edges go out with both sides live.
        enables.push_str("    pwm.set_dead_time(DEAD_TIME);\n");
        enables.push_str("    let max = pwm.get_max_duty();\n");
        let mut mode_asked = false;
        for ch in wiring.active_channels() {
            let out = cfg.channel_of(ch);
            enables.push_str(&format!(
                "    pwm.set_duty(Channel::Ch{ch}, max * DUTY_CH{ch} / 10_000);\n"
            ));
            if out.polarity != PwmPolarity::default() {
                low_level.push("OutputPolarity".to_owned());
                // The MAIN side only: inverting both would undo the pairing the
                // complementary output exists for.
                enables.push_str(&format!(
                    "    pwm.set_main_polarity(Channel::Ch{ch}, OutputPolarity::{});\n",
                    out.polarity.embassy()
                ));
            }
            mode_asked |= out.mode != PwmMode::default();
            enables.push_str(&format!("    pwm.enable(Channel::Ch{ch});\n"));
        }
        for (i, _) in &wiring.breaks {
            let b = cfg.break_of(*i);
            let sfx = if *i == 1 {
                String::new()
            } else {
                i.to_string()
            };
            low_level.push("BreakInputPolarity".to_owned());
            low_level.push("FilterValue".to_owned());
            enables.push_str(&format!(
                "    pwm.set_break{sfx}_polarity(BreakInputPolarity::{});\n",
                b.polarity.embassy()
            ));
            enables.push_str(&format!(
                "    pwm.set_break{sfx}_filter(FilterValue::{});\n",
                b.filter_embassy()
            ));
            enables.push_str(&format!("    pwm.set_break{sfx}_input_pin_enable(true);\n"));
            enables.push_str(&format!("    pwm.set_break{sfx}_enable(true);\n"));
        }
        if !wiring.breaks.is_empty() {
            enables.push_str(&format!(
                "    pwm.set_automatic_output_enable({});\n",
                cfg.auto_output_enable
            ));
        }
        if mode_asked {
            // Three pushes, not one `\`-continued literal: rustfmt joins those
            // back into a single source line and the continuation loses its
            // `//`, which lands as a bare statement in the user's file.
            enables.push_str("    // PWM mode 2 is not applied: `ComplementaryPwm` has no\n");
            enables.push_str("    // output-compare-mode setter. The channel polarity above\n");
            enables.push_str("    // gives the same inversion.\n");
        }
        timer_items.push("Channel".to_owned());
    } else {
        for ch in wiring.active_channels() {
            let out = cfg.channel_of(ch);
            enables.push_str(&format!(
                "    pwm.ch{ch}().enable();\n    pwm.ch{ch}().set_duty_cycle_fraction(DUTY_CH{ch}, 10_000);\n"
            ));
            // Only what DIFFERS from the timer's reset state gets a line, so a
            // project that never opened these settings generates what it always did.
            if out.polarity != PwmPolarity::default() {
                low_level.push("OutputPolarity".to_owned());
                enables.push_str(&format!(
                    "    pwm.ch{ch}().set_polarity(OutputPolarity::{});\n",
                    out.polarity.embassy()
                ));
            }
            if out.mode != PwmMode::default() {
                low_level.push("OutputCompareMode".to_owned());
                enables.push_str(&format!(
                    "    pwm.ch{ch}().set_output_compare_mode(OutputCompareMode::{});\n",
                    out.mode.embassy()
                ));
            }
        }
    }

    if !wiring.breaks.is_empty() {
        gpio_items.push("AfType".to_owned());
        gpio_items.push("Flex".to_owned());
        gpio_items.push("Pull".to_owned());
        prelude.push_str("    // The break pads are put into alternate-function mode by hand:\n");
        prelude.push_str("    // embassy declares `BreakInputPin` but no driver takes one, so\n");
        prelude.push_str("    // nothing else does it.\n");
        for (i, _) in &wiring.breaks {
            prelude.push_str(&format!("    let bkin{i}_af = bkin{i}.af_num();\n"));
            prelude.push_str(&format!("    let mut bkin{i} = Flex::new(bkin{i});\n"));
            prelude.push_str(&format!(
                "    bkin{i}.set_as_af_unchecked(bkin{i}_af, AfType::input(Pull::None));\n"
            ));
        }
        prelude.push('\n');
    }

    let use_line = |path: &str, items: &[String]| -> String {
        if items.len() == 1 {
            format!("use embassy_stm32::{path}::{};", items[0])
        } else {
            format!("use embassy_stm32::{path}::{{{}}};", items.join(", "))
        }
    };
    let dedup = |mut v: Vec<String>| -> Vec<String> {
        v.sort();
        v.dedup();
        v
    };

    let low_level_use = use_line("timer::low_level", &dedup(low_level));
    let timer_use = use_line("timer", &dedup(timer_items));
    let driver = if complementary {
        "ComplementaryPwm"
    } else {
        "SimplePwm"
    };
    let driver_use = if complementary {
        let mut l =
            "use embassy_stm32::timer::complementary_pwm::{ComplementaryPwm, ComplementaryPwmPin};"
                .to_owned();
        if has_plain {
            l.push_str("\nuse embassy_stm32::timer::simple_pwm::PwmPin;");
        }
        l
    } else {
        "use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};".to_owned()
    };

    // The pads have to outlive `init`: a dropped `Flex` disconnects its pin,
    // and a disconnected break pad is a fault line nobody is listening to.
    let (break_struct, ret, ret_expr) = if wiring.breaks.is_empty() {
        (
            String::new(),
            format!("{driver}<'d, peripherals::TIM{n}>"),
            "pwm".to_owned(),
        )
    } else {
        let fields: String = wiring
            .breaks
            .iter()
            .map(|(i, _)| format!("    pub bkin{i}: Flex<'d>,\n"))
            .collect();
        let inits: String = wiring
            .breaks
            .iter()
            .map(|(i, _)| format!("bkin{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(
                "\n/// The break pads, handed back so their alternate-function setting outlives\n\
                 /// `init`. Dropping one disconnects the pin, and the fault line goes deaf.\n\
                 pub struct BreakPads<'d> {{\n{fields}}}\n"
            ),
            format!("({driver}<'d, peripherals::TIM{n}>, BreakPads<'d>)"),
            format!("(pwm, BreakPads {{ {inits} }})"),
        )
    };

    let intro = if complementary {
        format!(
            "// `init` returns embassy's `ComplementaryPwm` for TIM{n}. Each channel drives a\n\
             // PAIR of pads — CHx and its inverse CHxN — with DEAD_TIME between the edges, so\n\
             // the two sides of a half-bridge are never on at once. The frequency belongs to\n\
             // the timer; the duty is what each channel owns."
        )
    } else {
        format!(
            "// `init` returns embassy's `SimplePwm` for TIM{n}. Every channel of a timer\n\
             // shares its frequency (one prescaler, one reload value) — the duty cycle is\n\
             // what each channel owns."
        )
    };
    let usage = if complementary {
        format!(
            "// The handle addresses channels by name, and a channel means BOTH its pads:\n\
             //\n\
             //     let max = {handle}.get_max_duty();\n\
             //     {handle}.set_duty(Channel::Ch1, max / 2); // 50 %\n\
             //     {handle}.disable(Channel::Ch1);           // both pads idle\n\
             //\n\
             // DEAD_TIME is in timer ticks on the same scale as the compare value; embassy\n\
             // encodes it into the CKD + DTG fields. Zero means no dead time at all, which a\n\
             // half-bridge will not survive."
        )
    } else {
        format!(
            "// The handle owns every channel; each one is reached by name:\n\
             //\n\
             //     {handle}.ch1().set_duty_cycle_percent(75);\n\
             //     {handle}.ch1().disable();\n\
             //\n\
             // The generated duty is a ratio out of 10_000, so a value like 7.5 % lands\n\
             // exactly. Any other ratio works the same way — a third of the period:\n\
             //\n\
             //     {handle}.ch1().set_duty_cycle_fraction(1, 3);"
        )
    };

    // The same duty trait the F1 backend generates, so switching runtime does
    // not rename the call sites. The channel is part of the METHOD NAME rather
    // than an argument for the same reason it is on F1: there, asking for a
    // channel with no pad PANICS (`PINS::check_used`); here the driver accepts
    // it and silently drives nothing, which is quieter and no more useful.
    let active = wiring.active_channels();
    let duty_trait = if active.is_empty() {
        String::new()
    } else {
        let first = active[0];
        let list = active
            .iter()
            .map(|c| format!("CH{c}"))
            .collect::<Vec<_>>()
            .join("+");
        let mut t = String::new();
        t.push_str("\n/// Set a channel's duty in the same units the `DUTY_*` constants above\n");
        t.push_str(
            "/// use \u{2014} HUNDREDTHS of a percent, so `10_000` is 100 % and `750` is 7.5 %.\n",
        );
        t.push_str("///\n");
        t.push_str("/// A trait rather than an inherent method because the handle is embassy's\n");
        t.push_str("/// own type, which this crate does not own. One method per WIRED channel\n");
        t.push_str(&format!(
            "/// ({list}); a channel this timer has no pad for cannot be named at all.\n"
        ));
        if !wiring.breaks.is_empty() {
            t.push_str("///\n");
            t.push_str(
                "/// The impl is on the DRIVER, not on the tuple `init` returns here \u{2014}\n",
            );
            t.push_str("/// the break pads ride along in it. `main.rs` destructures that tuple,\n");
            t.push_str(&format!("/// so `{handle}` is already the driver.\n"));
        }
        t.push_str("pub trait DutyHandle {\n");
        t.push_str(&format!(
            "    /// Ch{first}, the lowest channel wired to TIM{n}.\n"
        ));
        t.push_str(&format!(
            "    fn set_duty_tim_{n}(&mut self, value: u32);\n"
        ));
        for ch in &active {
            let both = if complementary {
                format!(" and CH{ch}N")
            } else {
                String::new()
            };
            t.push_str(&format!("\n    /// CH{ch}{both}.\n"));
            t.push_str(&format!(
                "    fn set_duty_tim_{n}_ch{ch}(&mut self, value: u32);\n"
            ));
        }
        t.push_str("}\n\n");
        t.push_str(&format!(
            "impl<'d> DutyHandle for {driver}<'d, peripherals::TIM{n}> {{\n"
        ));
        t.push_str(&format!(
            "    fn set_duty_tim_{n}(&mut self, value: u32) {{\n"
        ));
        t.push_str(&format!(
            "        self.set_duty_tim_{n}_ch{first}(value);\n"
        ));
        t.push_str("    }\n");
        for ch in &active {
            t.push_str(&format!(
                "\n    fn set_duty_tim_{n}_ch{ch}(&mut self, value: u32) {{\n"
            ));
            // Each body is copied from the `enables` line above it, which is
            // already proven to compile against that driver's API: the two
            // drivers do NOT share one.
            if complementary {
                t.push_str(&format!(
                    "        self.set_duty(Channel::Ch{ch}, self.get_max_duty() * value / 10_000);\n"
                ));
            } else {
                t.push_str(&format!(
                    "        self.ch{ch}().set_duty_cycle_fraction(value, 10_000);\n"
                ));
            }
            t.push_str("    }\n");
        }
        t.push_str("}\n");
        t
    };

    ASYNC_PWM_TMPL
        .replace("{GPIO_USE}", &use_line("gpio", &dedup(gpio_items)))
        .replace("{BREAK_STRUCT}", &break_struct)
        .replace("{PRELUDE}", &prelude)
        .replace("{RET_EXPR}", &ret_expr)
        .replace("{RET}", &ret)
        .replace("{FREQ}", &cfg.freq_hz.to_string())
        .replace("{COUNTING}", cfg.counting.embassy())
        .replace("{INTRO}", &intro)
        .replace("{USAGE}", &usage)
        .replace("{LOW_LEVEL_USE}", &low_level_use)
        .replace("{DRIVER_USE}", &driver_use)
        .replace("{TIMER_USE}", &timer_use)
        .replace("{DRIVER}", driver)
        .replace("{DUTY_TRAIT}", &duty_trait)
        .replace("{DUTY_CONSTS}", &duty_consts)
        .replace("{PARAMS}", &params)
        .replace("{CH_ARGS}", &ch_args)
        .replace("{ENABLES}", &enables)
        .replace("{HANDLE}", handle)
        .replace("{N}", &n.to_string())
}

/// The timers with at least one PWM channel wired, as
/// the shape `pwm_config_file` and the `main.rs` call both need.
#[derive(Default)]
pub struct PwmWiring {
    /// `(channel, pin name)` for the plain outputs, ascending.
    pub chans: Vec<(u8, String)>,
    /// `(channel, pin name)` for the complementary `CHxN` pads, ascending.
    pub comp: Vec<(u8, String)>,
    /// `(input index, pin name)` for the break pads — 1 is BKIN, 2 is BKIN2.
    pub breaks: Vec<(u8, String)>,
}

impl PwmWiring {
    /// The `init` parameters in call order: channel by channel, the plain pad
    /// before its complementary one. `(parameter name, pin, complementary)`.
    ///
    /// One list drives the signature, the arguments in `main.rs` and the slots
    /// handed to embassy, so the three cannot fall out of step.
    fn params(&self) -> Vec<(u8, String, &str, bool)> {
        let mut out = Vec::new();
        for ch in 1..=4u8 {
            if let Some((_, pin)) = self.chans.iter().find(|(c, _)| *c == ch) {
                out.push((ch, format!("ch{ch}"), pin.as_str(), false));
            }
            if let Some((_, pin)) = self.comp.iter().find(|(c, _)| *c == ch) {
                out.push((ch, format!("ch{ch}n"), pin.as_str(), true));
            }
        }
        // Break pads last, so adding one never renumbers the channel arguments.
        for (i, pin) in &self.breaks {
            out.push((*i, format!("bkin{i}"), pin.as_str(), false));
        }
        out
    }

    /// `true` when this timer needs `ComplementaryPwm` rather than `SimplePwm`:
    /// a complementary pad, or a break input, both of which live only there.
    fn needs_complementary(&self) -> bool {
        !self.comp.is_empty() || !self.breaks.is_empty()
    }

    /// Channels with at least one pad wired — the ones that need a duty. A
    /// channel whose only pad is the complementary one still has a compare
    /// value; it is the channel that carries the duty, not the pin.
    fn active_channels(&self) -> Vec<u8> {
        (1..=4u8)
            .filter(|ch| {
                self.chans.iter().any(|(c, _)| c == ch) || self.comp.iter().any(|(c, _)| c == ch)
            })
            .collect()
    }
}

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
fn sai_wires(pins: &[&Pin]) -> Vec<(u8, Vec<(u8, SaiBlockWiring)>)> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
{consts}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// SAI{n} carries TWO independent sub-blocks. `split_subblocks` hands them out
// once, here, which is why one config module covers the whole unit even though
// each sub-block gets its own driver, its own direction and its own DMA.
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::sai::{{{items}}};
use embassy_stm32::{{peripherals, Peri}};
use static_cell::StaticCell;

{configs}/// Initialise the wired sub-block(s) of SAI{n}.
pub fn init<'d, {generics}>(
    sai: Peri<'d, peripherals::SAI{n}>,
{params}    // One binding value covers every channel, so it travels with them —
    // same shape as the DMA-backed SPI and I2S next door.
    irqs: impl {bounds} + 'd,
) -> {ret} {{
{split}    // `'static` so the DMA controller can own them for the program's lifetime.
{statics}{body}}}
"#,
        items = sai_items.join(", "),
        generics = generics.join(", "),
        bounds = bounds.join(" + "),
    )
}

/// The DACs with at least one output pad wired, as `(dac, [(channel, pin)])`.
fn dac_wires(pins: &[&Pin]) -> Vec<(u8, Vec<(u8, String)>)> {
    let mut by_dac: BTreeMap<u8, Vec<(u8, String)>> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        if let PinFunction::DacOut { dac, channel } = p.selected_function {
            by_dac
                .entry(dac)
                .or_default()
                .push((channel, p.gpio().to_owned()));
        }
    }
    for chans in by_dac.values_mut() {
        chans.sort_unstable();
        // One pad per channel: a second one would become a duplicate argument.
        chans.dedup_by_key(|(c, _)| *c);
    }
    by_dac.into_iter().collect()
}

/// `src/pins/configs/dac{N}.rs`: embassy's blocking DAC over DAC{N}.
const ASYNC_DAC_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
{START_CONSTS}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// The analog mirror of a GPIO output: write a number, the pin holds the
// matching voltage. `new_blocking` writes the data register directly — no DMA
// and no interrupt, which is the whole peripheral for a set-point or a bias.
// Waveform streaming is the other API, and it needs a timer trigger.
{USE}
use embassy_stm32::mode::Blocking;
use embassy_stm32::{peripherals, Peri};

/// Initialise DAC{N}. The channel(s) are enabled and driving before this
/// returns, at the value(s) above.
pub fn init<'d>(
    dac: Peri<'d, peripherals::DAC{N}>,
{PARAMS}) -> {RET} {
    let mut dac = {CTOR};
{SETS}    dac
}

// ── Using DAC{N} ──
{USAGE}
"#;

/// Render [`ASYNC_DAC_TMPL`] for DAC `n` with the channels `chans`.
pub fn dac_config_file(
    n: u8,
    cfg: &DacModuleConfig,
    chans: &[(u8, String)],
    handle: &str,
) -> String {
    let both = chans.len() == 2;
    let mut start_consts = String::new();
    let mut params = String::new();
    for (ch, _) in chans {
        start_consts.push_str(&format!(
            "pub const START_CH{ch}: u16 = {}; // 12 bit, right-aligned (0..=4095)\n",
            cfg.value_of(*ch)
        ));
        params.push_str(&format!(
            "    out{ch}: Peri<'d, impl DacPin<peripherals::DAC{n}, Ch{ch}>>,\n"
        ));
    }
    let arg_list: String = chans.iter().map(|(ch, _)| format!(", out{ch}")).collect();

    // Both pads wired is ONE `Dac` covering the block; a single pad is a
    // `DacChannel`, and the channel it stands for cannot be inferred from the
    // argument alone — hence the turbofish.
    let (ret, ctor, sets, chs) = if both {
        (
            "Dac<'d, Blocking>".to_owned(),
            format!("Dac::new_blocking(dac{arg_list})"),
            "    dac.set(DualValue::Bit12Right(START_CH1, START_CH2));\n".to_owned(),
            "Ch1, Ch2".to_owned(),
        )
    } else {
        let ch = chans.first().map(|(c, _)| *c).unwrap_or(1);
        (
            "DacChannel<'d, Blocking>".to_owned(),
            format!("DacChannel::new_blocking::<peripherals::DAC{n}, Ch{ch}>(dac{arg_list})"),
            format!("    dac.set(Value::Bit12Right(START_CH{ch}));\n"),
            format!("Ch{ch}"),
        )
    };
    let value_ty = if both { "DualValue" } else { "Value" };
    let ty = if both { "Dac" } else { "DacChannel" };
    let use_line = format!("use embassy_stm32::dac::{{{chs}, {ty}, DacPin, {value_ty}}};");

    let mut usage =
        String::from("// Write a new level at any time; the pin follows immediately.\n//\n");
    if both {
        usage.push_str(&format!(
            "//     {handle}.set(DualValue::Bit12Right(2048, 0));\n"
        ));
        usage.push_str("//\n// Or take the channels apart and drive them independently:\n//\n");
        usage.push_str(&format!("//     let (mut a, mut b) = {handle}.split();\n"));
        usage.push_str("//     a.set(Value::Bit12Right(4095));");
    } else {
        usage.push_str(&format!(
            "//     {handle}.set(Value::Bit12Right(2048)); // mid-scale\n"
        ));
        usage.push_str(&format!(
            "//     {handle}.set(Value::Bit8(255));        // or 8 bit"
        ));
    }

    ASYNC_DAC_TMPL
        .replace("{START_CONSTS}", &start_consts)
        .replace("{USE}", &use_line)
        .replace("{PARAMS}", &params)
        .replace("{RET}", &ret)
        .replace("{CTOR}", &ctor)
        .replace("{SETS}", &sets)
        .replace("{USAGE}", &usage)
        .replace("{N}", &n.to_string())
}

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
fn hspi_wires(pins: &[&Pin]) -> Vec<(u8, HspiWiring)> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const PRESCALER: u8 = {prescaler}; // bus = kernel clock / (PRESCALER + 1)
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// {mode} on HSPI{n}. The high-speed memory controller at the top of the U5 line.
// The pads go up to IO15, but embassy's driver has exactly two constructors —
// single and octal — so those are the two widths this file can be generated in.
// This is the BLOCKING driver: a memory command is a round trip anyway.
use embassy_stm32::hspi::enums::{{MemorySize, MemoryType}};
use embassy_stm32::hspi::{{{pins}, Config, Hspi}};
use embassy_stm32::mode::Blocking;
use embassy_stm32::{{peripherals, Peri}};

fn get_config() -> Config {{
    let mut config = Config::default();
    config.device_size = MemorySize::{size};
    config.memory_type = MemoryType::{mtype};
    config.clock_prescaler = PRESCALER;
    config
}}

/// Initialise HSPI{n} in {mode_short}.
pub fn init<'d>(
    hspi: Peri<'d, peripherals::HSPI{n}>,
{params}) -> Hspi<'d, peripherals::HSPI{n}, Blocking> {{
    Hspi::new_blocking_{ctor}(hspi, {args}, get_config())
}}

{usage}"#,
        prescaler = cfg.prescaler,
        mode = cfg.mode.label(),
        mode_short = cfg.mode.label().to_lowercase(),
        pins = pin_items.join(", "),
        size = cfg.size_embassy(),
        mtype = cfg.memory_type.embassy(),
        ctor = cfg.mode.embassy(),
        args = args.join(", "),
    )
}

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
fn xspi_wires(pins: &[&Pin]) -> Vec<(u8, XspiWiring)> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const PRESCALER: u8 = {prescaler}; // bus = kernel clock / (PRESCALER + 1)
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// {mode} on XSPI{n}. The OCTOSPI's successor: the same block again, now up to
// sixteen data lines, with two chip selects and two strobes to choose from.
// This is the BLOCKING driver — a memory command is a round trip anyway.
use embassy_stm32::mode::Blocking;
use embassy_stm32::xspi::enums::{{MemorySize, MemoryType}};
use embassy_stm32::xspi::{{{pins}, Config, Xspi}};
use embassy_stm32::{{peripherals, Peri}};

fn get_config() -> Config {{
    let mut config = Config::default();
    config.device_size = MemorySize::{size};
    config.memory_type = MemoryType::{mtype};
    config.clock_prescaler = PRESCALER;
    config
}}

/// Initialise XSPI{n} in {mode_short}.
pub fn init<'d>(
    xspi: Peri<'d, peripherals::XSPI{n}>,
{params}) -> Xspi<'d, peripherals::XSPI{n}, Blocking> {{
    Xspi::{ctor}(xspi, {args}, get_config())
}}

// ── Using XSPI{n} ──
// Commands go out as a `TransferConfig`, the same shape as the OCTOSPI's:
//
//     use embassy_stm32::xspi::TransferConfig;
//
//     let mut id = [0u8; 3];
//     {handle}.blocking_read(&mut id, TransferConfig {{
//         instruction: Some(0x9F), // read JEDEC id
//         ..Default::default()
//     }}).ok();
"#,
        prescaler = cfg.prescaler,
        mode = cfg.mode.label(),
        mode_short = cfg.mode.label().to_lowercase(),
        pins = pin_items.join(", "),
        size = cfg.size_embassy(),
        mtype = cfg.memory_type.embassy(),
        args = args.join(", "),
        handle = format!("_xspi{n}{}", label_sfx(&cfg.custom_label)),
    )
}

/// Everything wired to one OCTOSPI port.
pub struct OspiWiring {
    pub clk: String,
    pub ncs: String,
    pub dqs: Option<String>,
    /// Lane → pin, ascending.
    pub io: BTreeMap<u8, String>,
}

/// The OCTOSPI ports with a clock, a chip select and at least one data line.
fn ospi_wires(pins: &[&Pin]) -> Vec<(u8, OspiWiring)> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const PRESCALER: u8 = {prescaler}; // bus = kernel clock / (PRESCALER + 1)
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// {mode} on OCTOSPI{{N}}. QUADSPI's successor: the same idea with up to eight
// data lines, and it speaks single, dual, quad and octal SPI from one block.
// This is the BLOCKING driver — a flash command is a round trip anyway.
use embassy_stm32::mode::Blocking;
use embassy_stm32::ospi::enums::{{MemorySize, MemoryType}};
use embassy_stm32::ospi::{{{pins}, Config, Ospi}};
use embassy_stm32::{{peripherals, Peri}};

fn get_config() -> Config {{
    let mut config = Config::default();
    config.device_size = MemorySize::{size};
    config.memory_type = MemoryType::{mtype};
    config.clock_prescaler = PRESCALER;
    config
}}

/// Initialise OCTOSPI{{N}} in {mode_short}.
pub fn init<'d>(
    ospi: Peri<'d, peripherals::OCTOSPI{{N}}>,
{params}) -> Ospi<'d, peripherals::OCTOSPI{{N}}, Blocking> {{
    Ospi::{ctor}(ospi, {args}, get_config())
}}

// ── Using OCTOSPI{{N}} ──
// Commands go out as a `TransferConfig`, the same shape as the QUADSPI's:
//
//     use embassy_stm32::ospi::TransferConfig;
//
//     let mut id = [0u8; 3];
//     {handle}.blocking_read(&mut id, TransferConfig {{
//         instruction: Some(0x9F), // read JEDEC id
//         ..Default::default()
//     }}).ok();
"#,
        prescaler = cfg.prescaler,
        mode = cfg.mode.label(),
        mode_short = cfg.mode.label().to_lowercase(),
        pins = pin_items.join(", "),
        size = cfg.size_embassy(),
        mtype = cfg.memory_type.embassy(),
        args = args.join(", "),
        handle = format!("_ospi{n}{}", label_sfx(&cfg.custom_label)),
    )
    .replace("{N}", &n.to_string())
}

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
fn qspi_wires(pins: &[&Pin]) -> Option<QspiWiring> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const PRESCALER: u8 = {prescaler}; // bus = kernel clock / (PRESCALER + 1)
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// External flash on {shape}. Four data lines instead of one, so a read runs
// about four times faster than over plain SPI. This is the BLOCKING driver: it
// waits for each command, which is what a flash command is anyway.
use embassy_stm32::mode::Blocking;
use embassy_stm32::qspi::enums::{{AddressSize, MemorySize}};
use embassy_stm32::qspi::{{{pins}, Config, Qspi}};
use embassy_stm32::{{peripherals, Peri}};

fn get_config() -> Config {{
    let mut config = Config::default();
    config.memory_size = MemorySize::{size};
    config.address_size = AddressSize::{addr};
    config.prescaler = PRESCALER;
    config.dual_flash = {dual};
    config
}}

/// Initialise the QUADSPI over {shape}.
pub fn init<'d>(
    qspi: Peri<'d, peripherals::QUADSPI>,
{params}) -> Qspi<'d, peripherals::QUADSPI, Blocking> {{
    Qspi::{ctor}(qspi, {args}, get_config())
}}

// ── Using the QUADSPI ──
// Commands go out as a `TransferConfig`; the driver waits for each one.
//
//     use embassy_stm32::qspi::TransferConfig;
//     use embassy_stm32::qspi::enums::{{QspiWidth, DummyCycles}};
//
//     let mut id = [0u8; 3];
//     {handle}.blocking_read(
//         &mut id,
//         TransferConfig {{
//             iwidth: QspiWidth::SING,
//             instruction: Some(0x9F), // read JEDEC id
//             dummy: DummyCycles::_0,
//             ..Default::default()
//         }},
//     );
"#,
        prescaler = cfg.prescaler,
        pins = pin_items.join(", "),
        size = cfg.memory_size_embassy(),
        addr = cfg.address_size.embassy(),
        args = args.join(", "),
    )
}

/// What this chip calls its SD-card block: 0 is the un-numbered `SDIO`.
fn sdmmc_peri(unit: u8) -> String {
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
fn sd_bus_width(lanes: &BTreeMap<u8, String>) -> Option<u8> {
    let have: Vec<u8> = lanes.keys().copied().collect();
    match have.len() {
        1 if have == [0] => Some(1),
        4 if have == [0, 1, 2, 3] => Some(4),
        8 if have == [0, 1, 2, 3, 4, 5, 6, 7] => Some(8),
        _ => None,
    }
}

/// The SD-card controllers with a clock, a command line and at least one lane.
fn sdmmc_wires(pins: &[&Pin]) -> Vec<(u8, SdmmcWiring)> {
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

    format!(
        r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const DATA_TIMEOUT: u32 = {timeout}; // card bus clock periods
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// {peri} at {width}-bit width — the width IS the wiring: one, four or eight data
// lines, one constructor each.
//
// This chip carries the {ver} controller.{dma_note}
{dma_use}use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::sdmmc::{{{items}}};
use embassy_stm32::{{peripherals, Peri}};

fn get_config() -> Config {{
    let mut config = Config::default();
    config.data_transfer_timeout = DATA_TIMEOUT;
    config
}}

/// Initialise {peri} as a {width}-bit SD/eMMC host.
pub fn init<'d{generic}>(
    sdmmc: Peri<'d, peripherals::{peri}>,
{params}) -> Sdmmc<'d> {{
    {ctor}
}}

// ── Using {peri} ──
// The handle talks to the card directly; it has to be initialised once the
// card is in the socket:
//
//     {handle}.init_card(embassy_stm32::time::mhz(25)).await.unwrap();
//     let card = {handle}.card().unwrap();
//     defmt::info!("{{}} blocks", card.csd.block_count());
"#,
        timeout = cfg.data_timeout,
        ver = if v1 { "older (v1)" } else { "newer (v2)" },
        dma_note = if v1 {
            " It is fed a DMA channel, and binds
// that channel's interrupt as well as its own."
        } else {
            " It has its own DMA controller
// inside, so it takes no channel."
        },
        dma_use = if v1 {
            "use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
"
        } else {
            ""
        },
        items = sd_items.join(", "),
        generic = if v1 {
            format!(", D: SdmmcDma<peripherals::{peri}>")
        } else {
            String::new()
        },
        handle = format!("_sd{n}{}", label_sfx(&cfg.custom_label)),
    )
}

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
fn i2s_wires(pins: &[&Pin]) -> Vec<(u8, I2sWiring)> {
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
const ASYNC_I2S_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SAMPLE_RATE_HZ: u32 = {RATE};
pub const BUF_LEN: usize = {BUF};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// I2S{N} IS the SPI{N} block: the same silicon, told to speak audio instead. It
// runs from a ring buffer the DMA owns for the whole program — there is no
// blocking I2S in embassy, so `init` always takes a channel.
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::i2s::{ClockPolarity, Config, Format, I2S, Mode, Standard};
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::spi::{CkPin, I2sSdPin, {DMA_TRAIT}, WsPin{MCK_USE}};
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};
use static_cell::StaticCell;

fn get_config() -> Config {
    // `Config` is `#[non_exhaustive]`, so it is built by assignment rather than
    // as a literal.
    let mut config = Config::default();
    config.frequency = Hertz(SAMPLE_RATE_HZ);
    config.mode = Mode::{MODE};
    config.standard = Standard::{STANDARD};
    config.format = Format::{FORMAT};
    config.clock_polarity = ClockPolarity::{POLARITY};
    config.master_clock = {MASTER_CLOCK};
    config
}

/// Initialise SPI{N} as I2S{N}, {DIRWORD}.
pub fn init<'d, D: {DMA_TRAIT}<peripherals::SPI{N}>>(
    spi: Peri<'d, peripherals::SPI{N}>,
    sd: Peri<'d, impl I2sSdPin<peripherals::SPI{N}>>,
    ws: Peri<'d, impl WsPin<peripherals::SPI{N}>>,
    ck: Peri<'d, impl CkPin<peripherals::SPI{N}>>,
{MCK_PARAM}    dma: Peri<'d, D>,
    // Only `main.rs` knows WHICH channel this is, so the binding travels with
    // it — same shape as the DMA-backed SPI next door.
    irqs: impl Binding<D::Interrupt, DmaInterruptHandler<D>> + 'd,
) -> I2S<'d, {WORD}> {
    // `'static` so the DMA controller can own it for the program's lifetime.
    static BUF: StaticCell<[{WORD}; BUF_LEN]> = StaticCell::new();
    I2S::{CTOR}(
        spi,
        sd,
        ws,
        ck,
{MCK_ARG}        dma,
        BUF.init([0; BUF_LEN]),
        irqs,
        get_config(),
    )
}

// ── Using I2S{N} ──
// The handle streams through its ring buffer; `start()` opens the tap.
//
//     {HANDLE}.start();
{USAGE}
"#;

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
    ASYNC_I2S_TMPL
        .replace("{RATE}", &cfg.sample_rate_hz.to_string())
        .replace("{BUF}", &cfg.buffer_len.to_string())
        .replace("{DMA_TRAIT}", if tx { "TxDma" } else { "RxDma" })
        .replace("{MCK_USE}", if has_mck { ", MckPin" } else { "" })
        .replace(
            "{MCK_PARAM}",
            if has_mck {
                "    mck: Peri<'d, impl MckPin<peripherals::SPI{N}>>,\n"
            } else {
                ""
            },
        )
        .replace("{MCK_ARG}", if has_mck { "        mck,\n" } else { "" })
        .replace("{MODE}", cfg.mode.embassy())
        .replace("{STANDARD}", cfg.standard.embassy())
        .replace("{FORMAT}", cfg.format.embassy())
        .replace("{POLARITY}", cfg.clock_polarity.embassy())
        .replace("{MASTER_CLOCK}", if has_mck { "true" } else { "false" })
        .replace("{WORD}", cfg.format.word())
        .replace("{CTOR}", ctor)
        .replace("{DIRWORD}", if tx { "transmitting" } else { "receiving" })
        .replace("{USAGE}", &usage)
        .replace("{HANDLE}", handle)
        .replace("{N}", &n.to_string())
}

/// The timer `time-driver-any` hands embassy-time on this chip, among the
/// timers its pads offer - `None` when it has none of the candidates.
///
/// embassy's own order (`build.rs`): two-channel timers, then two-channel with
/// complementary outputs, then 16-bit general purpose, 32-bit, advanced; the
/// larger number first inside each. On an STM32F103C8 that is TIM4, which is
/// why `p.TIM4` does not exist in an async F103 project.
fn time_driver_timer(pins: &[&Pin]) -> Option<u8> {
    const ORDER: [u8; 15] = [22, 21, 12, 9, 15, 19, 4, 3, 24, 23, 5, 2, 20, 8, 1];
    let present: std::collections::BTreeSet<u8> = pins
        .iter()
        .flat_map(|p| p.available_functions.iter())
        .filter_map(|f| match f {
            PinFunction::TimerPwm { timer, .. }
            | PinFunction::TimerPwmN { timer, .. }
            | PinFunction::TimerBreak { timer, .. } => Some(*timer),
            _ => None,
        })
        .collect();
    ORDER.into_iter().find(|t| present.contains(t))
}

/// The pin traits of the USART, SPI, I2C and I2S config files, whose bounds
/// carry the F1's AFIO remap as a second parameter (`pin_trait!(.., @A)` in
/// embassy-stm32 0.6). `CkPin` is also the SD host's clock trait, which does
/// NOT take one - see [`with_afio_remap`].
const AFIO_BUS_PIN_TRAITS: [&str; 14] = [
    "RxPin", "TxPin", "CtsPin", "RtsPin", "CkPin", "DePin", "SckPin", "MosiPin", "MisoPin",
    "SclPin", "SdaPin", "I2sSdPin", "WsPin", "MckPin",
];

/// A bus config file as an STM32F1 needs it on embassy-stm32: every pin trait
/// takes the AFIO remap as one more parameter (`RxPin<USART1, A>`), and embassy
/// writes AFIO_MAPR itself from it.
///
/// `init` gets a generic `A` that the pins decide at the call site: each bus
/// pad belongs to one remap set, so rustc infers it, and a pair from two sets
/// does not type-check (E0277) - the rule the F1's remap groups keep in the UI
/// anyway. Compiled on an STM32F103C8 with default and remapped pads, main.rs
/// unchanged. A pass over the text rather than a flag through every template,
/// so no other family's file can change; a file with no such bound is
/// returned as it came.
fn with_afio_remap(body: &str) -> String {
    let mut out = body.to_owned();
    let mut bounds = 0;
    for tr in AFIO_BUS_PIN_TRAITS {
        let needle = format!("impl {tr}<peripherals::");
        let mut s = String::with_capacity(out.len() + 16);
        let mut rest = out.as_str();
        while let Some(at) = rest.find(&needle) {
            let start = at + needle.len();
            // The SD host's `CkPin` has no remap parameter: leave it as it is.
            if tr == "CkPin"
                && (rest[start..].starts_with("SDIO") || rest[start..].starts_with("SDMMC"))
            {
                s.push_str(&rest[..start]);
                rest = &rest[start..];
                continue;
            }
            let Some(close) = rest[start..].find('>') else {
                break;
            };
            s.push_str(&rest[..start + close]);
            s.push_str(", A");
            rest = &rest[start + close..];
            bounds += 1;
        }
        s.push_str(rest);
        out = s;
    }
    if bounds == 0 {
        return body.to_owned();
    }
    // `<'d, ` before `<'d>(`, or the first would match the second's result.
    out.replace("pub fn init<'d, ", "pub fn init<'d, A, ")
        .replace("pub fn init<'d>(", "pub fn init<'d, A>(")
}

/// embassy's AFIO remap type for one of stm32f1xx-hal's timer remap
/// type-states - the rows [`super::stm32::pwm_remap`] picks from. The numbers
/// are the TIMx_REMAP field values: TIM1 has no partial row here because its
/// CH1..4 pads are the same in both, TIM4's field is a single bit.
fn embassy_afio_remap(hal_ty: &str) -> Option<&'static str> {
    Some(match hal_ty {
        "Tim1NoRemap" | "Tim2NoRemap" | "Tim3NoRemap" => "AfioRemap<0>",
        "Tim2PartialRemap1" => "AfioRemap<1>",
        "Tim2PartialRemap2" | "Tim3PartialRemap" => "AfioRemap<2>",
        "Tim1FullRemap" | "Tim2FullRemap" | "Tim3FullRemap" => "AfioRemap<3>",
        "Tim4NoRemap" => "AfioRemapBool<false>",
        "Tim4Remap" => "AfioRemapBool<true>",
        _ => return None,
    })
}

/// An F1 PWM file with its remap NAMED: a `Remap` alias in the generated block,
/// re-spliced when the wiring moves, and every `TimerPin` bound taking it.
fn with_timer_remap(body: &str, n: u8, remap: &str) -> String {
    let alias = format!(
        "// The AFIO remap these pads are on - named, because a timer pad can sit in two.\n\
         pub type Remap = embassy_stm32::gpio::{remap};\n"
    );
    let end = "// <<< GENERATED END >>>";
    let body = match body.find(end) {
        Some(at) => format!("{}{alias}{}", &body[..at], &body[at..]),
        None => body.to_owned(),
    };
    let mut out = body;
    for ch in 1..=4u8 {
        out = out.replace(
            &format!("impl TimerPin<peripherals::TIM{n}, Ch{ch}>>"),
            &format!("impl TimerPin<peripherals::TIM{n}, Ch{ch}, Remap>>"),
        );
    }
    out
}

/// The timers with at least one PWM pad wired, plain or complementary.
fn pwm_wires(pins: &[&Pin]) -> Vec<(u8, PwmWiring)> {
    let mut by_timer: BTreeMap<u8, PwmWiring> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let (timer, channel, which) = match p.selected_function {
            PinFunction::TimerPwm { timer, channel } => (timer, channel, 0),
            PinFunction::TimerPwmN { timer, channel } => (timer, channel, 1),
            PinFunction::TimerBreak { timer, input } => (timer, input, 2),
            _ => continue,
        };
        let w = by_timer.entry(timer).or_default();
        let list = match which {
            0 => &mut w.chans,
            1 => &mut w.comp,
            _ => &mut w.breaks,
        };
        list.push((channel, p.gpio().to_owned()));
    }
    for w in by_timer.values_mut() {
        for list in [&mut w.chans, &mut w.comp, &mut w.breaks] {
            list.sort_unstable();
            // One pin per channel: a second pad claiming the same channel would
            // become a duplicate argument.
            list.dedup_by_key(|(c, _)| *c);
        }
    }
    by_timer.into_iter().collect()
}

// ── Async SPI config file (blocking | async-DMA) ──────────────────────────────

/// Blocking SPI (`Spi::new_blocking`) exposed as the STANDARD blocking
/// `embedded-hal` 1.0 `SpiBus<u8>`. No DMA — compiles out of the box.
const ASYNC_SPI_TMPL_BLOCKING: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_HZ: u32 = {CLK};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns a STANDARD `embedded-hal` 1.0 `SpiBus<u8>` (BLOCKING). A blocking
// bus in an async project is a common, valid pattern; for `.await`-able DMA SPI
// switch this module to "Async-DMA" in the IDE.
//
//     fn app<S: embedded_hal::spi::SpiBus<u8>>(spi: &mut S) { /* … */ }
use embassy_stm32::spi::{Config, MisoPin, MosiPin, SckPin, Spi, MODE_0, MODE_1, MODE_2, MODE_3};
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};

fn get_config() -> Config {
    let mut config = Config::default();
    config.mode = match SPI_MODE {
        1 => MODE_1,
        2 => MODE_2,
        3 => MODE_3,
        _ => MODE_0,
    };
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config
}

/// Initialise SPI{N} as a blocking `embedded-hal` 1.0 SpiBus value.
pub fn init<'d>(
    spi: Peri<'d, peripherals::SPI{N}>,
    sck: Peri<'d, impl SckPin<peripherals::SPI{N}>>,
    mosi: Peri<'d, impl MosiPin<peripherals::SPI{N}>>,
    miso: Peri<'d, impl MisoPin<peripherals::SPI{N}>>,
) -> impl embedded_hal::spi::SpiBus<u8> + 'd {
    Spi::new_blocking(spi, sck, mosi, miso, get_config())
}

// ── Using SPI{N} ──
// Blocking init inside an async project: the handle is an `embedded-hal` 1.0
// `SpiBus` — no `.await`, the transfer busy-waits. Fine for short bursts;
// switch the module to Async-DMA when a transfer is long enough to matter.
//
//     use embedded_hal::spi::SpiBus;
//
//     let mut buf = [0x9F, 0x00, 0x00];
//     {HANDLE}.transfer_in_place(&mut buf).ok();
//     {HANDLE}.flush().ok();

"#;

/// Async DMA SPI (`Spi::new`) exposed as `embedded-hal-async` `SpiBus<u8>`
/// (`.await`-able). Needs two DMA channels, passed by `main.rs`.
const ASYNC_SPI_TMPL_DMA: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_HZ: u32 = {CLK};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns an ASYNC `embedded-hal-async` `SpiBus<u8>` (`.await`-able),
// backed by DMA. The DMA channels are passed in from `main.rs`.
//
//     async fn app<S: embedded_hal_async::spi::SpiBus<u8>>(spi: &mut S) { /* … */ }
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::spi::{
    Config, MisoPin, MosiPin, RxDma, SckPin, Spi, TxDma, MODE_0, MODE_1, MODE_2, MODE_3,
};
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};

fn get_config() -> Config {
    let mut config = Config::default();
    config.mode = match SPI_MODE {
        1 => MODE_1,
        2 => MODE_2,
        3 => MODE_3,
        _ => MODE_0,
    };
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config
}

/// Initialise SPI{N} as an async `embedded-hal-async` SpiBus value (DMA-backed).
pub fn init<'d, TxD: TxDma<peripherals::SPI{N}>, RxD: RxDma<peripherals::SPI{N}>>(
    spi: Peri<'d, peripherals::SPI{N}>,
    sck: Peri<'d, impl SckPin<peripherals::SPI{N}>>,
    mosi: Peri<'d, impl MosiPin<peripherals::SPI{N}>>,
    miso: Peri<'d, impl MisoPin<peripherals::SPI{N}>>,
    tx_dma: Peri<'d, TxD>,
    rx_dma: Peri<'d, RxD>,
    // embassy 0.6 requires the DMA channels' interrupts to be bound, and only
    // `main.rs` knows WHICH channels those are — hence the named type params
    // above instead of `impl TxDma<..>`: the bound below has to name them.
    irqs: impl Binding<TxD::Interrupt, DmaInterruptHandler<TxD>>
        + Binding<RxD::Interrupt, DmaInterruptHandler<RxD>>
        + 'd,
) -> impl embedded_hal_async::spi::SpiBus<u8> + 'd {
    Spi::new(spi, sck, mosi, miso, tx_dma, rx_dma, irqs, get_config())
}

// ── Using SPI{N} ──
// Async-DMA init: the handle is an `embedded-hal-async` `SpiBus`, so a transfer
// hands the bytes to DMA and yields until it finishes.
//
//     use embedded_hal_async::spi::SpiBus;
//
//     {HANDLE}.write(&[0x9F]).await.ok();
//
//     let mut rx = [0u8; 3];
//     {HANDLE}.transfer(&mut rx, &[0x9F, 0x00, 0x00]).await.ok();
//
//     let mut buf = [0x9F, 0x00, 0x00];
//     {HANDLE}.transfer_in_place(&mut buf).await.ok();
//     {HANDLE}.flush().await.ok();

"#;

/// Render the SPI config file for instance `n`, picking the blocking or
/// async-DMA template from the module's [`AsyncBusMode`].
pub fn spi_config_file(n: u8, cfg: Option<&SpiModuleConfig>, tx_only: bool) -> String {
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
    let tmpl = match (cfg.map(|c| c.async_mode).unwrap_or_default(), tx_only) {
        (AsyncBusMode::Blocking, false) => ASYNC_SPI_TMPL_BLOCKING,
        (AsyncBusMode::AsyncDma, false) => ASYNC_SPI_TMPL_DMA,
        (AsyncBusMode::Blocking, true) => ASYNC_SPI_TMPL_BLOCKING_TX,
        (AsyncBusMode::AsyncDma, true) => ASYNC_SPI_TMPL_DMA_TX,
    };
    let sfx = cfg
        .map(|c| sanitize_label(&c.custom_label))
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();
    tmpl.replace("{HANDLE}", &format!("_spi{n}{sfx}"))
        .replace("{N}", &n.to_string())
        .replace("{MODE}", &mode.to_string())
        .replace("{EXTRA_CFG}", &extra)
        .replace("{CLK}", &clk.to_string())
}

/// `configs/spi{n}.rs` for a **transmit-only** SPI on DMA: no MISO pin, ONE
/// channel.
///
/// Returns the concrete `Spi<'d, Async>` rather than `impl SpiBus`, and that is
/// the point: `SpiBus` promises `read` and `transfer`, and on a bus built
/// without an RX channel those panic inside embassy (`rx_dma.unwrap()`).
/// Handing back a trait that cannot keep half its contract would be worse than
/// handing back a narrower type.
const ASYNC_SPI_TMPL_DMA_TX: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_HZ: u32 = {CLK};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// TRANSMIT ONLY: MISO is not wired, so this bus can send and cannot receive.
// It takes one DMA channel instead of two — the other stays free for another
// peripheral. Wire a MISO pin to get the full-duplex `SpiBus` back.
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::mode::Async;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config, MosiPin, SckPin, Spi, TxDma};
use embassy_stm32::time::Hertz;
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::{peripherals, Peri};

fn get_config() -> Config {
    let mut config = Config::default();
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config.mode = match SPI_MODE {
        1 => embassy_stm32::spi::MODE_1,
        2 => embassy_stm32::spi::MODE_2,
        3 => embassy_stm32::spi::MODE_3,
        _ => embassy_stm32::spi::MODE_0,
    };
    config
}

/// Initialise SPI{N} as a transmit-only, DMA-backed bus.
pub fn init<'d, TxD: TxDma<peripherals::SPI{N}>>(
    spi: Peri<'d, peripherals::SPI{N}>,
    sck: Peri<'d, impl SckPin<peripherals::SPI{N}>>,
    mosi: Peri<'d, impl MosiPin<peripherals::SPI{N}>>,
    tx_dma: Peri<'d, TxD>,
    irqs: impl Binding<TxD::Interrupt, DmaInterruptHandler<TxD>> + 'd,
) -> Spi<'d, Async, Master> {
    Spi::new_txonly(spi, sck, mosi, tx_dma, irqs, get_config())
}

// ── Using SPI{N} ──
// Send only. `read` and `transfer` exist on this type but would panic here —
// there is no RX channel behind them.
//
//     {HANDLE}.write(&[0x9Fu8, 0x00]).await.ok();

"#;

/// The same shape without DMA: `new_blocking_txonly`, no channels at all.
const ASYNC_SPI_TMPL_BLOCKING_TX: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_HZ: u32 = {CLK};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// TRANSMIT ONLY: MISO is not wired, so this bus can send and cannot receive.
// Wire a MISO pin to get the full-duplex `SpiBus` back.
use embassy_stm32::mode::Blocking;
use embassy_stm32::spi::mode::Master;
use embassy_stm32::spi::{Config, MosiPin, SckPin, Spi};
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};

fn get_config() -> Config {
    let mut config = Config::default();
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config.mode = match SPI_MODE {
        1 => embassy_stm32::spi::MODE_1,
        2 => embassy_stm32::spi::MODE_2,
        3 => embassy_stm32::spi::MODE_3,
        _ => embassy_stm32::spi::MODE_0,
    };
    config
}

/// Initialise SPI{N} as a transmit-only, blocking bus.
pub fn init<'d>(
    spi: Peri<'d, peripherals::SPI{N}>,
    sck: Peri<'d, impl SckPin<peripherals::SPI{N}>>,
    mosi: Peri<'d, impl MosiPin<peripherals::SPI{N}>>,
) -> Spi<'d, Blocking, Master> {
    Spi::new_blocking_txonly(spi, sck, mosi, get_config())
}

// ── Using SPI{N} ──
// Send only, and blocking — the CPU clocks each byte out.
//
//     {HANDLE}.blocking_write(&[0x9Fu8, 0x00]).ok();

"#;

// ── Async I2C config file (blocking | async-DMA) ──────────────────────────────

/// Blocking I2C (`I2c::new_blocking`) exposed as the STANDARD blocking
/// `embedded-hal` 1.0 `I2c`. No DMA/interrupts — compiles out of the box.
const ASYNC_I2C_TMPL_BLOCKING: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const CLOCK_HZ: u32 = {CLK};
{DEVICES}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns a STANDARD `embedded-hal` 1.0 `I2c` (BLOCKING). A blocking bus in
// an async project is a common, valid pattern; for `.await`-able DMA I2C switch
// this module to "Async-DMA" in the IDE.
//
//     fn app<I: embedded_hal::i2c::I2c>(i2c: &mut I) { /* … */ }
use embassy_stm32::i2c::{Config, I2c, SclPin, SdaPin};
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};

fn get_config() -> Config {
    let mut config = Config::default();
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config
}

/// Initialise I2C{N} as a blocking `embedded-hal` 1.0 I2c value.
pub fn init<'d>(
    i2c: Peri<'d, peripherals::I2C{N}>,
    scl: Peri<'d, impl SclPin<peripherals::I2C{N}>>,
    sda: Peri<'d, impl SdaPin<peripherals::I2C{N}>>,
) -> impl embedded_hal::i2c::I2c + 'd {
    I2c::new_blocking(i2c, scl, sda, get_config())
}

// ── Using I2C{N} ──
// Blocking init inside an async project: the handle is an `embedded-hal` 1.0
// `I2c` — no `.await`. Switch the module to Async-DMA for long transfers.
//
//     use embedded_hal::i2c::I2c;
//     // The address is the device's own, in its file beside this one:
//     use pins::configs::i2c{N}::device1_<name>::DEVICE_ADDRESS;
//
//     let mut rx = [0u8; 2];
//     {HANDLE}.write_read(DEVICE_ADDRESS, &[0x10], &mut rx).ok();

"#;

/// Async DMA I2C (`I2c::new`) exposed as `embedded-hal-async` `I2c`
/// (`.await`-able). Needs an interrupt binding (event + error) and two DMA
/// channels (passed by `main.rs`).
const ASYNC_I2C_TMPL_DMA: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const CLOCK_HZ: u32 = {CLK};
{DEVICES}// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns an ASYNC `embedded-hal-async` `I2c` (`.await`-able), backed by
// DMA. The DMA channels are passed in from `main.rs`.
//
//     async fn app<I: embedded_hal_async::i2c::I2c>(i2c: &mut I) { /* … */ }
use embassy_stm32::dma::InterruptHandler as DmaInterruptHandler;
use embassy_stm32::i2c::{
    Config, ErrorInterruptHandler, EventInterruptHandler, I2c, Instance, RxDma, SclPin, SdaPin,
    TxDma,
};
use embassy_stm32::interrupt::typelevel::Binding;
use embassy_stm32::time::Hertz;
use embassy_stm32::{peripherals, Peri};

// No `bind_interrupts!` here, unlike the non-DMA configs: `I2c::new` takes ONE
// value that must bind the peripheral's two interrupts AND both DMA channels',
// and the channels are only known in `main.rs`. So the whole `Irqs` lives there.

fn get_config() -> Config {
    let mut config = Config::default();
    config.frequency = Hertz(CLOCK_HZ);
{EXTRA_CFG}    config
}

/// Initialise I2C{N} as an async `embedded-hal-async` I2c value (DMA-backed).
pub fn init<'d, TxD: TxDma<peripherals::I2C{N}>, RxD: RxDma<peripherals::I2C{N}>>(
    i2c: Peri<'d, peripherals::I2C{N}>,
    scl: Peri<'d, impl SclPin<peripherals::I2C{N}>>,
    sda: Peri<'d, impl SdaPin<peripherals::I2C{N}>>,
    tx_dma: Peri<'d, TxD>,
    rx_dma: Peri<'d, RxD>,
    irqs: impl Binding<
            <peripherals::I2C{N} as Instance>::EventInterrupt,
            EventInterruptHandler<peripherals::I2C{N}>,
        > + Binding<
            <peripherals::I2C{N} as Instance>::ErrorInterrupt,
            ErrorInterruptHandler<peripherals::I2C{N}>,
        > + Binding<TxD::Interrupt, DmaInterruptHandler<TxD>>
        + Binding<RxD::Interrupt, DmaInterruptHandler<RxD>>
        + 'd,
) -> impl embedded_hal_async::i2c::I2c + 'd {
    // Argument order matters and changed in embassy 0.6: the irq binding comes
    // AFTER both DMA channels, not before them.
    I2c::new(i2c, scl, sda, tx_dma, rx_dma, irqs, get_config())
}

// ── Using I2C{N} ──
// Async-DMA init: the handle is an `embedded-hal-async` `I2c`.
//
//     use embedded_hal_async::i2c::I2c;
//     // The address is the device's own, in its file beside this one:
//     use pins::configs::i2c{N}::device1_<name>::DEVICE_ADDRESS;
//
//     {HANDLE}.write(DEVICE_ADDRESS, &[0x10, 0x42]).await.ok();
//
//     let mut rx = [0u8; 2];
//     {HANDLE}.read(DEVICE_ADDRESS, &mut rx).await.ok();
//     {HANDLE}.write_read(DEVICE_ADDRESS, &[0x10], &mut rx).await.ok();

"#;

/// Render the I2C config file for instance `n`, picking the blocking or
/// async-DMA template from the module's [`AsyncBusMode`].
pub fn i2c_config_file(n: u8, cfg: Option<&I2cModuleConfig>) -> String {
    let clk = cfg.map(|c| c.clock_hz).unwrap_or(100_000);
    // 0 means "embassy's default", so nothing is written and the existing
    // output is unchanged. Clamped otherwise: a 0 ms timeout would fail every
    // transfer instantly, which reads as broken wiring.
    let tmo = cfg.map(|c| c.timeout_ms).unwrap_or(0);
    let extra = if tmo == 0 {
        String::new()
    } else {
        format!(
            "    config.timeout = embassy_time::Duration::from_millis({});
",
            tmo.clamp(1, 60_000)
        )
    };
    let tmpl = match cfg.map(|c| c.async_mode).unwrap_or_default() {
        AsyncBusMode::Blocking => ASYNC_I2C_TMPL_BLOCKING,
        AsyncBusMode::AsyncDma => ASYNC_I2C_TMPL_DMA,
    };
    let sfx = cfg
        .map(|c| sanitize_label(&c.custom_label))
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();
    tmpl.replace("{HANDLE}", &format!("_i2c{n}{sfx}"))
        .replace("{N}", &n.to_string())
        .replace("{EXTRA_CFG}", &extra)
        .replace("{CLK}", &clk.to_string())
        .replace("{DEVICES}", &super::common::i2c_device_mods(cfg))
}

#[cfg(test)]
mod irq_key_tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    /// STM32G0: one I2C vector, one shared USART vector, and DMA channels that
    /// share vectors too - every key in the struct has to be unique, because
    /// `bind_interrupts!` rejects a repeat.
    #[test]
    fn a_chip_that_shares_vectors_gets_one_line_per_vector() {
        let irqs = v(&["I2C1", "USART1", "USART3_4_LPUART1", "DMA1_Channel2_3"]);
        let binds = [
            ("DMA1_CHANNEL2_3".to_owned(), "DMA1_CH2".to_owned()),
            ("DMA1_CHANNEL2_3".to_owned(), "DMA1_CH3".to_owned()),
        ];
        let out = dma_irqs_block(&[1], &[("USART", 3, false)], &binds, &irqs, &[], &[], false);
        assert!(
            out.contains(
                "    I2C1 => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>, embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;"
            ),
            "{out}"
        );
        assert!(
            !out.contains("I2C1_EV"),
            "the split names do not exist here:
{out}"
        );
        assert!(
            out.contains("    USART3_4_LPUART1 => embassy_stm32::usart::InterruptHandler<peripherals::USART3>;"),
            "{out}"
        );
        assert!(
            out.contains("DMA1_CHANNEL2_3 => embassy_stm32::dma::InterruptHandler<peripherals::DMA1_CH2>, embassy_stm32::dma::InterruptHandler<peripherals::DMA1_CH3>;"),
            "{out}"
        );
        // No key twice, whatever the source.
        let keys: Vec<&str> = out
            .lines()
            .filter_map(|l| l.split_once(" => ").map(|(k, _)| k.trim()))
            .collect();
        let mut uniq = keys.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(
            keys.len(),
            uniq.len(),
            "duplicate key in:
{out}"
        );
    }

    /// The overwhelming majority of families, and every chip imported before the
    /// vector list existed: the split names stay.
    #[test]
    fn a_chip_with_split_vectors_or_no_list_keeps_ev_and_er() {
        for irqs in [v(&["I2C1_EV", "I2C1_ER", "USART1"]), Vec::new()] {
            let out = dma_irqs_block(&[1], &[("USART", 1, false)], &[], &irqs, &[], &[], false);
            assert!(
                out.contains(
                    "    I2C1_EV => embassy_stm32::i2c::EventInterruptHandler<peripherals::I2C1>;"
                ),
                "{out}"
            );
            assert!(
                out.contains(
                    "    I2C1_ER => embassy_stm32::i2c::ErrorInterruptHandler<peripherals::I2C1>;"
                ),
                "{out}"
            );
            assert!(
                out.contains(
                    "    USART1 => embassy_stm32::usart::InterruptHandler<peripherals::USART1>;"
                ),
                "{out}"
            );
        }
    }

    /// The buffered-USART config file has its OWN `bind_interrupts!`, keyed the
    /// same way.
    #[test]
    fn the_buffered_usart_binds_the_chips_vector_in_main() {
        // The config file no longer binds anything: a vector can be bound ONCE
        // per program, and on this chip USART3 shares one with USART4/LPUART1.
        let f = serial_config_file("USART", 3, None, "USART3_4_LPUART1", true);
        assert!(!f.contains("bind_interrupts!(struct Irqs"), "{f}");
        assert!(f.contains("irqs: impl Binding<"), "{f}");
        // main.rs binds it, under the chip's own vector name.
        let out = dma_irqs_block(
            &[],
            &[("USART", 3, true)],
            &[],
            &["USART3_4_LPUART1".into()],
            &[],
            &[],
            false,
        );
        assert!(
            out.contains(
                "    USART3_4_LPUART1 => embassy_stm32::usart::BufferedInterruptHandler<peripherals::USART3>;"
            ),
            "{out}"
        );
    }
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
                dma: Some(&chip),
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
                dma: Some(&chip),
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
            !out.init_calls.contains("DMA_TX_TODO"),
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
                dma: Some(&chip),
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
                dma: Some(&chip),
                irq_vectors: &[],
                usart_ip: None,
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
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
                .contains("init(p.SPI1, p.PA5, p.PA7, p.DMA1_CH1, Irqs)"),
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

#[cfg(test)]
mod comp_tests {
    use super::*;
    use crate::panels::mcu_module::comparator::{
        BlankingSource, CompConfig, Generation, Hysteresis, InvertingInput, OutputPolarity,
        PowerMode,
    };

    /// The two constructors are not interchangeable: `new_with_input_minus_pin`
    /// IGNORES `config.inverting_input`, so emitting the pin argument next to a
    /// VREF choice would show a setting the driver then drops.
    #[test]
    fn the_constructor_follows_the_inverting_input() {
        let internal = CompConfig {
            power_mode: PowerMode::UltraLowPower,
            hysteresis: Hysteresis::Mv70,
            output_polarity: OutputPolarity::Inverted,
            inverting_input: InvertingInput::ThreeQuarterVref,
            blanking_source: BlankingSource::Blank2,
        };
        let f = comp_config_file(3, &internal, Generation::V2);
        assert!(
            f.contains("Comp::new(comp, inp, irqs, get_config())"),
            "{f}"
        );
        assert!(!f.contains("inm"), "no second pin anywhere: {f}");
        // Every setting reaches the file, under embassy's own spelling.
        // On a G4 embassy never writes the power mode, so the file must not
        // pretend otherwise.
        assert!(
            !f.contains("PowerMode"),
            "inert on comp_v2:
{f}"
        );
        for want in [
            "Hysteresis::Hyst70M",
            "OutputPolarity::Inverted",
            "InvertingInput::ThreeQuarterVref",
            "BlankingSource::Blank2",
            "peripherals::COMP3",
        ] {
            assert!(f.contains(want), "{want} missing from:\n{f}");
        }

        let pin = CompConfig {
            inverting_input: InvertingInput::InputPin,
            ..CompConfig::default()
        };
        let f = comp_config_file(3, &pin, Generation::V2);
        assert!(
            f.contains("Comp::new_with_input_minus_pin(comp, inp, inm, irqs, get_config())"),
            "{f}"
        );
        assert!(
            f.contains("inm: Peri<'d, impl InputMinusPin<peripherals::COMP3> + Pin>,"),
            "{f}"
        );
        // ...and only this form imports the trait, or the user's project warns.
        assert!(f.contains("use embassy_stm32::comp::InputMinusPin;"), "{f}");
    }

    /// The U5/WBA generation is a different peripheral: named hysteresis
    /// levels, no second INM pin, and a power mode that embassy DOES write.
    #[test]
    fn the_u5_generation_emits_its_own_vocabulary() {
        let cfg = CompConfig {
            power_mode: PowerMode::UltraLowPower,
            hysteresis: Hysteresis::Medium,
            inverting_input: InvertingInput::Vref,
            ..CompConfig::default()
        };
        let f = comp_config_file(2, &cfg, Generation::U5);
        assert!(
            f.contains("config.power_mode = PowerMode::UltraLowPower;"),
            "{f}"
        );
        assert!(f.contains("use embassy_stm32::comp::PowerMode;"), "{f}");
        assert!(f.contains("Hysteresis::Medium"), "{f}");
        assert!(
            !f.contains("Hysteresis::Hyst"),
            "no millivolt steps on this generation:
{f}"
        );

        // A level carried over from a G4 cannot be named here; it falls back to
        // the one both generations have rather than emitting nonsense.
        let stale = CompConfig {
            hysteresis: Hysteresis::Mv40,
            ..CompConfig::default()
        };
        let f = comp_config_file(2, &stale, Generation::U5);
        assert!(f.contains("Hysteresis::None"), "{f}");
        assert!(!f.contains("Hysteresis::Hyst40M"), "{f}");
    }

    /// A comparator reaches `main.rs` only when the chip's family has a driver,
    /// the instance is switched on, AND its pin is wired. Each of the three is
    /// a separate silence, and none of them may half-emit.
    #[test]
    fn a_comparator_needs_a_driver_a_switch_and_a_pin() {
        use crate::panels::mcu_module::comparator::CompSettings;
        use crate::panels::mcu_module::pins::logic::pin::Pin;

        let pins: [Pin; 0] = [];
        let refs: Vec<&Pin> = pins.iter().collect();
        let mut on = CompSettings::new();
        on.insert(1, CompConfig::default());
        let wired = [(1u8, "PA1".to_owned(), None)];
        let irqs = vec!["COMP1_2_3".to_owned()];

        let go = |family: &str, set: &CompSettings, pins: &[(u8, String, Option<String>)]| {
            async_peripherals(
                family,
                ChipData {
                    dma: None,
                    irq_vectors: &irqs,
                    usart_ip: Some("sci3_v2_1_Cube"),
                    sdmmc_ip: None,
                },
                CompInputs {
                    settings: set,
                    instances: &[1, 2, 3],
                    pins,
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
        };

        let out = go("stm32g4", &on, &wired);
        assert!(
            out.init_calls
                .contains("pins::configs::comp1::init(p.COMP1, p.PA1, Irqs)"),
            "{}",
            out.init_calls
        );
        assert!(
            out.dma_irqs.contains(
                "COMP1_2_3 => embassy_stm32::comp::InterruptHandler<peripherals::COMP1>;"
            ),
            "the Irqs struct exists even with no DMA at all:\n{}",
            out.dma_irqs
        );
        assert_eq!(out.config_files.len(), 1);
        assert!(
            out.consumed_pins.contains(&"PA1".to_owned()),
            "the pin is taken"
        );

        // No driver for the family.
        assert!(go("stm32f4", &on, &wired).init_calls.is_empty());
        // Switched off.
        assert!(
            go("stm32g4", &CompSettings::new(), &wired)
                .init_calls
                .is_empty()
        );
        // Switched on, but no INP pin wired.
        assert!(go("stm32g4", &on, &[]).init_calls.is_empty());
        // Wants a pin for [-] and has none.
        let mut needs_pin = CompSettings::new();
        needs_pin.insert(
            1,
            CompConfig {
                inverting_input: InvertingInput::InputPin,
                ..CompConfig::default()
            },
        );
        assert!(go("stm32g4", &needs_pin, &wired).init_calls.is_empty());
        // ...and emits once the INM pin is there too.
        let both = [(1u8, "PA1".to_owned(), Some("PA0".to_owned()))];
        assert!(
            go("stm32g4", &needs_pin, &both)
                .init_calls
                .contains("init(p.COMP1, p.PA1, p.PA0, Irqs)"),
            "{}",
            go("stm32g4", &needs_pin, &both).init_calls
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
                dma: None,
                irq_vectors: irqs,
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
            out.init_calls
                .contains("pins::configs::lpuart1::init(p.LPUART1, p.PA3, p.PA2, Irqs)"),
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
#[cfg(test)]
mod exti_tests {
    use super::*;

    fn vectors(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    fn armed(name: &str, edge: Edge) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::GpioInput;
        p.irq = Some(edge);
        p
    }

    /// An EXTI vector name states a RANGE, which is what makes `nvic::covered`
    /// the wrong reader for it: `EXTI15_10` carries lines 11..14 too.
    #[test]
    fn a_vector_name_is_read_as_a_range() {
        let v = vectors(&[
            "EXTI0",
            "EXTI1",
            "EXTI2",
            "EXTI3",
            "EXTI4",
            "EXTI9_5",
            "EXTI15_10",
        ]);
        assert_eq!(exti_vector(&v, 0), Some("EXTI0"));
        assert_eq!(exti_vector(&v, 5), Some("EXTI9_5"));
        // The ones in the middle — the whole point.
        assert_eq!(exti_vector(&v, 7), Some("EXTI9_5"));
        assert_eq!(exti_vector(&v, 12), Some("EXTI15_10"));
        assert_eq!(exti_vector(&v, 15), Some("EXTI15_10"));
        // A line the chip does not carry has no vector.
        assert_eq!(exti_vector(&v, 16), None);

        // A G0-style list: overlapping ranges, and the NARROWEST wins.
        let g0 = vectors(&["EXTI0_1", "EXTI2_3", "EXTI4_15"]);
        assert_eq!(exti_vector(&g0, 1), Some("EXTI0_1"));
        assert_eq!(exti_vector(&g0, 3), Some("EXTI2_3"));
        assert_eq!(exti_vector(&g0, 9), Some("EXTI4_15"));
    }

    /// The line is the PIN NUMBER, and that is what makes it scarce.
    #[test]
    fn the_line_is_the_pin_number() {
        assert_eq!(exti_line("PA0"), Some(0));
        assert_eq!(exti_line("PB15"), Some(15));
        assert_eq!(exti_line("PC7"), Some(7));
        // Not a P<port><n> pad — nothing to sit on.
        assert_eq!(exti_line("VDD"), None);
        assert_eq!(exti_line("PB16"), None);
    }

    /// Two pads on one line: one gets the channel, the other is refused with the
    /// reason — embassy hands the EXTI channel to exactly one.
    #[test]
    fn one_pad_per_line_and_the_other_is_told_why() {
        let pins = vec![armed("PA5", Edge::Rising), armed("PB5", Edge::Falling)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI9_5"]));

        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(plan[0].singleton, "PA5");
        assert_eq!(plan[0].line, 5);
        assert_eq!(plan[0].vector, "EXTI9_5");
        assert!(notes.contains("PB5 is NOT on an interrupt"), "{notes}");
        assert!(notes.contains("already taken by"), "{notes}");
    }

    /// No vector for the line → no binding to hang the handler on, said out loud.
    #[test]
    fn a_line_with_no_vector_is_refused() {
        let pins = vec![armed("PA5", Edge::Rising)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI0", "EXTI1"]));
        assert!(plan.is_empty(), "{plan:?}");
        assert!(notes.contains("names none for"), "{notes}");
    }

    /// An input with no edge is one you poll: no plan, no task, no feature.
    #[test]
    fn an_unarmed_input_is_left_alone() {
        let mut p = Pin::new(1, "PA5");
        p.selected_function = PinFunction::GpioInput;
        let pins = vec![p];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI9_5"]));
        assert!(plan.is_empty());
        assert!(notes.is_empty(), "{notes}");
    }

    /// Each edge picks its own await, and the task owns the pin.
    #[test]
    fn the_task_awaits_the_chosen_edge() {
        for (edge, want) in [
            (Edge::Rising, "wait_for_rising_edge"),
            (Edge::Falling, "wait_for_falling_edge"),
            (Edge::Both, "wait_for_any_edge"),
        ] {
            let pins = vec![armed("PA5", edge)];
            let refs: Vec<&Pin> = pins.iter().collect();
            let (plan, _) = exti_plan(&refs, &vectors(&["EXTI9_5"]));
            let tasks = exti_tasks(&plan);
            assert!(tasks.contains(&format!("pin.{want}().await;")), "{tasks}");
            assert!(
                tasks.contains("async fn pa5_in_irq(mut pin: ExtiInput<'static, Async>)"),
                "{tasks}"
            );
            // The spawn shape is this stack's, NOT the ESP one: here the task
            // returns the token and `spawn` returns the Result.
            assert!(
                exti_spawns(&plan).contains("_spawner.spawn(pa5_in_irq(pa5_in)).ok();"),
                "{}",
                exti_spawns(&plan)
            );
        }
    }
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
            "pins::configs::hspi1::init(p.HSPI1, p.PA1,",
            " p.PB0, p.PB1, p.PB2, p.PB3, p.PB4, p.PB5, p.PB6, p.PB7,",
            " p.PA2, p.PC0);"
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
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci3_v2_1_Cube"),
                sdmmc_ip: ip,
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

#[cfg(test)]
mod dac_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, dac: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::DacOut { dac, channel };
        p
    }

    fn run(pins: &[Pin], dac: BTreeMap<u8, DacModuleConfig>) -> AsyncPeriphs {
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
            &dac,
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// Both pads of one block are ONE `Dac`, set together — the module is the
    /// peripheral, exactly as it is for a timer.
    #[test]
    fn both_channels_are_one_peripheral() {
        let pins = [mk("PA4", 1, 1), mk("PA5", 1, 2)];
        let mut cfg = DacModuleConfig::new(1);
        cfg.set_value(1, 2048);
        cfg.set_value(2, 4095);
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());

        assert_eq!(out.config_files.len(), 1, "one file per block");
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "dac1.rs");
        assert!(body.contains("pub const START_CH1: u16 = 2048;"), "{body}");
        assert!(body.contains("pub const START_CH2: u16 = 4095;"), "{body}");
        assert!(
            body.contains("Dac::new_blocking(dac, out1, out2)"),
            "{body}"
        );
        assert!(
            body.contains("dac.set(DualValue::Bit12Right(START_CH1, START_CH2));"),
            "{body}"
        );
        assert!(body.contains("-> Dac<'d, Blocking>"), "{body}");
        assert!(
            out.init_calls
                .contains("let mut _dac1 = pins::configs::dac1::init(p.DAC1, p.PA4, p.PA5);"),
            "{}",
            out.init_calls
        );
    }

    /// One pad is a `DacChannel`, and WHICH channel cannot be read off the
    /// argument — so the constructor names it.
    #[test]
    fn a_single_channel_names_itself_in_the_constructor() {
        let out = run(&[mk("PA5", 1, 2)], Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("DacChannel::new_blocking::<peripherals::DAC1, Ch2>(dac, out2)"),
            "{body}"
        );
        assert!(body.contains("-> DacChannel<'d, Blocking>"), "{body}");
        assert!(
            body.contains("dac.set(Value::Bit12Right(START_CH2));"),
            "{body}"
        );
        // Only the wired channel exists: no phantom CH1 const or parameter.
        assert!(!body.contains("START_CH1"), "{body}");
        assert!(!body.contains("out1"), "{body}");
        // …and the import names exactly what the file uses.
        assert!(
            body.contains("use embassy_stm32::dac::{Ch2, DacChannel, DacPin, Value};"),
            "{body}"
        );
    }

    /// A channel nobody set starts at zero, and that is written down rather
    /// than left implicit: the pad drives the moment it is enabled.
    #[test]
    fn an_untouched_channel_starts_at_zero_explicitly() {
        let out = run(&[mk("PA4", 1, 1)], Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("pub const START_CH1: u16 = 0;"), "{body}");
        assert!(
            body.contains("dac.set(Value::Bit12Right(START_CH1));"),
            "{body}"
        );
    }

    /// The DAC needs no DMA and no interrupt — `new_blocking` writes the
    /// register — so it must not drag an `Irqs` argument along.
    #[test]
    fn the_dac_binds_no_interrupts() {
        let out = run(&[mk("PA4", 1, 1)], Default::default());
        assert!(!out.init_calls.contains("Irqs"), "{}", out.init_calls);
        let (_, body) = &out.config_files[0];
        assert!(!body.contains("Binding"), "{body}");
        assert!(!body.contains("Dma"), "{body}");
    }
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

#[cfg(test)]
mod pwm_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, timer: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerPwm { timer, channel };
        p
    }

    /// The complementary pad of `timer` CH`channel`.
    fn mkn(name: &str, timer: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerPwmN { timer, channel };
        p
    }

    /// A break pad of `timer` — `input` 1 is BKIN, 2 is BKIN2.
    fn mkb(name: &str, timer: u8, input: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerBreak { timer, input };
        p
    }

    fn run(pins: &[Pin], timer: BTreeMap<u8, TimerModuleConfig>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32g0",
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
            &timer,
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

    /// Two channels of ONE timer are one module, one config file and one `init`
    /// — the whole reason the module is the timer rather than the channel.
    #[test]
    fn channels_of_one_timer_share_a_single_init() {
        let pins = [mk("PA6", 3, 1), mk("PA7", 3, 2)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.freq_hz = 20_000;
        cfg.set_duty_x100(1, 7_500);
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());

        assert_eq!(
            out.config_files.len(),
            1,
            "one file per timer: {:?}",
            out.config_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "pwm3.rs");
        assert!(
            out.init_calls
                .contains("let mut _pwm3 = pins::configs::pwm3::init(p.TIM3, p.PA6, p.PA7);"),
            "{}",
            out.init_calls
        );
        // The frequency is the module's, shared; the duty is per channel, and a
        // channel the user never touched starts at 0 %.
        assert!(body.contains("pub const FREQ_HZ: u32 = 20000;"), "{body}");
        assert!(
            body.contains("pub const DUTY_CH1: u32 = 7500; // 75 %, in hundredths"),
            "{body}"
        );
        assert!(
            body.contains("pub const DUTY_CH2: u32 = 0; // 0 %, in hundredths"),
            "{body}"
        );
        // Wired channels become parameters; the rest stay `None` slots.
        assert!(
            body.contains("ch1: Peri<'d, impl TimerPin<peripherals::TIM3, Ch1>>"),
            "{body}"
        );
        assert!(
            body.contains("Some(PwmPin::new(ch2, OutputType::PushPull)),"),
            "{body}"
        );
        assert_eq!(
            body.matches("        None,").count(),
            2,
            "CH3 + CH4: {body}"
        );
        assert!(
            body.contains("pwm.ch1().set_duty_cycle_fraction(DUTY_CH1, 10_000);"),
            "{body}"
        );
    }

    /// One CHxN pad changes the whole driver: `ComplementaryPwm` instead of
    /// `SimplePwm`, eight slots instead of four, and a dead time between the
    /// two edges — which is the entire reason the pad exists.
    #[test]
    fn a_complementary_pad_switches_the_driver() {
        let pins = [mk("PA8", 1, 1), mkn("PB13", 1, 1)];
        let mut cfg = TimerModuleConfig::new(1);
        cfg.freq_hz = 20_000;
        cfg.set_duty_x100(1, 5_000);
        cfg.dead_time = 40;
        cfg.set_channel(
            1,
            crate::panels::mcu_module::modules::PwmChannelConfig {
                polarity: PwmPolarity::ActiveLow,
                mode: PwmMode::Mode2,
                ..Default::default()
            },
        );
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "pwm1.rs");

        // Both pads are parameters, the plain one before its complement.
        assert!(
            out.init_calls
                .contains("let mut _pwm1 = pins::configs::pwm1::init(p.TIM1, p.PA8, p.PB13);"),
            "{}",
            out.init_calls
        );
        assert!(
            body.contains("ch1: Peri<'d, impl TimerPin<peripherals::TIM1, Ch1>>"),
            "{body}"
        );
        assert!(
            body.contains("ch1n: Peri<'d, impl TimerComplementaryPin<peripherals::TIM1, Ch1>>"),
            "{body}"
        );

        assert!(
            body.contains("ComplementaryPwm<'d, peripherals::TIM1>"),
            "{body}"
        );
        assert!(
            body.contains("Some(ComplementaryPwmPin::new(ch1n, OutputType::PushPull)),"),
            "{body}"
        );
        // Eight slots: four channels, each with its complement.
        assert_eq!(body.matches("        None,").count(), 6, "{body}");

        // Dead time is set BEFORE anything is enabled, and the duty is a
        // compare value here, not a ratio.
        let dead = body.find("set_dead_time").expect("dead time");
        let enable = body.find("pwm.enable(").expect("enable");
        assert!(dead < enable, "dead time must land first:\n{body}");
        assert!(body.contains("pub const DEAD_TIME: u16 = 40;"), "{body}");
        assert!(
            body.contains("pwm.set_duty(Channel::Ch1, max * DUTY_CH1 / 10_000);"),
            "{body}"
        );
        assert!(!body.contains("set_duty_cycle_fraction"), "{body}");

        // Polarity lands on the MAIN side only — inverting both would undo the
        // pairing the complementary pad exists for.
        assert!(
            body.contains("pwm.set_main_polarity(Channel::Ch1, OutputPolarity::ActiveLow);"),
            "{body}"
        );
        assert!(!body.contains("set_complementary_polarity"), "{body}");
        // PWM mode 2 has no setter on this driver, and saying so must not spill
        // a bare line into the user's file: every line of the note is a comment.
        assert!(
            body.contains("    // output-compare-mode setter."),
            "the note must stay commented on every line:
{body}"
        );
        for line in body.lines() {
            let t = line.trim_start();
            assert!(
                !t.starts_with("setter.") && !t.starts_with("gives the same"),
                "a continuation lost its `//`:
{body}"
            );
        }
    }

    /// A channel whose ONLY wired pad is the complementary one still owns a
    /// duty: the compare value belongs to the channel, not to the pin.
    #[test]
    fn a_lone_complementary_pad_still_gets_its_duty() {
        let pins = [mkn("PB13", 1, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("pub const DUTY_CH1: u32 = 0;"), "{body}");
        assert!(
            body.contains("pwm.set_duty(Channel::Ch1, max * DUTY_CH1 / 10_000);"),
            "{body}"
        );
        // No plain pad, so nothing imports `PwmPin` or names `TimerPin`.
        assert!(!body.contains("simple_pwm::PwmPin"), "{body}");
        assert!(!body.contains("TimerPin<"), "{body}");
    }

    /// TIM15/16/17 carry a CH1N pad, but embassy's `ComplementaryPwm` is bound
    /// to the advanced-control timers. Say so and drive the plain channel,
    /// rather than emitting code that cannot compile.
    #[test]
    fn a_complementary_pad_on_a_plain_timer_is_refused_with_a_reason() {
        let pins = [mk("PA6", 16, 1), mkn("PB6", 16, 1)];
        let out = run(&pins, Default::default());
        assert!(
            out.init_calls
                .contains("TIM16 CH1N (PB6) left unconfigured"),
            "{}",
            out.init_calls
        );
        // The plain channel still works, and the refused pad is NOT passed in.
        assert!(
            out.init_calls
                .contains("pins::configs::pwm16::init(p.TIM16, p.PA6);"),
            "{}",
            out.init_calls
        );
        let (_, body) = &out.config_files[0];
        assert!(body.contains("SimplePwm<'d, peripherals::TIM16>"), "{body}");
        assert!(!body.contains("ComplementaryPwm"), "{body}");
    }

    /// A break pad alone — no complementary channel in sight — is still enough
    /// to need `ComplementaryPwm`: that is where every break bit lives.
    #[test]
    fn a_break_pad_alone_switches_the_driver() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("ComplementaryPwm<'d, peripherals::TIM1>"),
            "{body}"
        );
        // No CHxN pad, so all four complementary slots stay `None`.
        assert_eq!(body.matches("        None,").count(), 7, "{body}");
    }

    /// The break pad is the one pin the generated `init` has to put into
    /// alternate-function mode itself, and it must OUTLIVE `init` — a dropped
    /// `Flex` disconnects the pin and the fault line goes deaf.
    #[test]
    fn a_break_pad_is_configured_by_hand_and_handed_back() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let mut cfg = TimerModuleConfig::new(1);
        cfg.set_break(
            1,
            crate::panels::mcu_module::modules::BreakInputConfig {
                polarity: crate::panels::mcu_module::modules::BreakPolarity::ActiveHigh,
                filter: 3,
            },
        );
        cfg.auto_output_enable = true;
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];

        // The pad is a parameter, bounded so a pin that cannot be BKIN for this
        // timer is a compile error rather than a silent no-op.
        assert!(
            body.contains("bkin1: Peri<'d, impl BreakInputPin<peripherals::TIM1, BkIn1>>"),
            "{body}"
        );
        // …put into AF mode by hand, because no embassy driver takes one.
        assert!(body.contains("let bkin1_af = bkin1.af_num();"), "{body}");
        assert!(
            body.contains("bkin1.set_as_af_unchecked(bkin1_af, AfType::input(Pull::None));"),
            "{body}"
        );
        // …and handed back, so the caller keeps it alive.
        assert!(body.contains("pub struct BreakPads<'d> {"), "{body}");
        assert!(body.contains("pub bkin1: Flex<'d>,"), "{body}");
        assert!(body.contains("(pwm, BreakPads { bkin1 })"), "{body}");
        assert!(
            out.init_calls.contains(
                "let (mut _pwm1, _pwm1_break_pads) = pins::configs::pwm1::init(p.TIM1, p.PA8, p.PA6);"
            ),
            "{}",
            out.init_calls
        );

        // Every break setting reaches the file, enable last.
        assert!(
            body.contains("pwm.set_break_polarity(BreakInputPolarity::ACTIVE_HIGH);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.set_break_filter(FilterValue::FCK_INT_N8);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.set_break_input_pin_enable(true);"),
            "{body}"
        );
        assert!(body.contains("pwm.set_break_enable(true);"), "{body}");
        assert!(
            body.contains("pwm.set_automatic_output_enable(true);"),
            "{body}"
        );
        let cfgd = body.find("set_break_polarity").expect("polarity");
        let enabled = body.find("set_break_enable").expect("enable");
        assert!(cfgd < enabled, "configure before enabling:\n{body}");
    }

    /// A break pad is NOT a complementary pad, even though `BKIN` ends in an N.
    /// They land in different lists, and a break-only timer has an empty one.
    #[test]
    fn a_break_pad_is_not_mistaken_for_a_complementary_one() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let wires = pwm_wires(&refs);
        assert_eq!(wires.len(), 1);
        let (timer, w) = &wires[0];
        assert_eq!(*timer, 1);
        assert_eq!(w.chans.len(), 1);
        assert!(w.comp.is_empty(), "BKIN is not half of a pair");
        assert_eq!(w.breaks, vec![(1u8, "PA6".to_owned())]);
    }

    /// TIM15/16/17 have a break pad too, and the same driver limit applies.
    #[test]
    fn a_break_pad_on_a_plain_timer_is_refused_with_a_reason() {
        let pins = [mk("PA6", 16, 1), mkb("PB6", 16, 1)];
        let out = run(&pins, Default::default());
        assert!(
            out.init_calls
                .contains("TIM16 BKIN (PB6) left unconfigured"),
            "{}",
            out.init_calls
        );
        // …and the refused pad is not passed in, nor held.
        assert!(
            out.init_calls
                .contains("pins::configs::pwm16::init(p.TIM16, p.PA6);"),
            "{}",
            out.init_calls
        );
        let (_, body) = &out.config_files[0];
        assert!(!body.contains("BreakPads"), "{body}");
    }

    /// Untouched settings generate exactly what they always did: the reset
    /// state of the timer, and not one line explaining that it is the reset
    /// state. Everything below is what a project pays for only if it asks.
    #[test]
    fn default_output_settings_add_nothing_to_the_file() {
        let pins = [mk("PA6", 3, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("CountingMode::EdgeAlignedUp,"), "{body}");
        assert!(
            body.contains("Some(PwmPin::new(ch1, OutputType::PushPull)),"),
            "{body}"
        );
        assert!(!body.contains("set_polarity"), "{body}");
        assert!(!body.contains("set_output_compare_mode"), "{body}");
        // One item, so no braces around the import.
        assert!(
            body.contains("use embassy_stm32::timer::low_level::CountingMode;"),
            "{body}"
        );
    }

    /// …and each of the four settings reaches the generated file, with the
    /// `low_level` import growing to cover exactly what the code names.
    #[test]
    fn the_output_settings_reach_the_generated_file() {
        use crate::panels::mcu_module::modules::{
            PwmChannelConfig, PwmCounting, PwmMode, PwmOutput, PwmPolarity,
        };

        let pins = [mk("PA6", 3, 1), mk("PA7", 3, 2)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.counting = PwmCounting::CenterBothInterrupts;
        cfg.set_channel(
            1,
            PwmChannelConfig {
                output: PwmOutput::OpenDrain,
                polarity: PwmPolarity::ActiveLow,
                mode: PwmMode::Mode2,
            },
        );
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];

        assert!(
            body.contains("CountingMode::CenterAlignedBothInterrupts,"),
            "{body}"
        );
        assert!(
            body.contains("Some(PwmPin::new(ch1, OutputType::OpenDrain)),"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_polarity(OutputPolarity::ActiveLow);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_output_compare_mode(OutputCompareMode::PwmMode2);"),
            "{body}"
        );
        assert!(
            body.contains("low_level::{CountingMode, OutputCompareMode, OutputPolarity};"),
            "{body}"
        );
        // CH2 was never touched, so it keeps the reset state and its silence.
        assert!(
            body.contains("Some(PwmPin::new(ch2, OutputType::PushPull)),"),
            "{body}"
        );
        assert!(!body.contains("pwm.ch2().set_polarity"), "{body}");
    }

    /// The duty everybody needs first: a hobby servo at 1.5 ms of a 20 ms
    /// frame is 7.5 %, which whole percent could only round away. The ratio
    /// out of 10_000 carries it into the generated file untouched.
    #[test]
    fn a_fractional_duty_survives_into_the_generated_file() {
        let pins = [mk("PA6", 3, 1)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.freq_hz = 50;
        cfg.set_duty_x100(1, 750);
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("pub const DUTY_CH1: u32 = 750; // 7.5 %, in hundredths"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_duty_cycle_fraction(DUTY_CH1, 10_000);"),
            "{body}"
        );
    }

    /// Two DIFFERENT timers are two modules, and a timer with no module config
    /// still generates at the defaults rather than being skipped.
    #[test]
    fn each_timer_gets_its_own_module() {
        let pins = [mk("PA6", 3, 1), mk("PA8", 1, 1)];
        let out = run(&pins, Default::default());
        let names: Vec<&str> = out.config_files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"pwm1.rs") && names.contains(&"pwm3.rs"),
            "{names:?}"
        );
        assert!(
            out.init_calls.contains("init(p.TIM1, p.PA8)"),
            "{}",
            out.init_calls
        );
        // Defaults when the module carries no config yet.
        let body = &out
            .config_files
            .iter()
            .find(|(n, _)| n == "pwm1.rs")
            .unwrap()
            .1;
        assert!(body.contains("pub const FREQ_HZ: u32 = 1000;"), "{body}");
    }

    /// PWM needs no interrupt and no DMA — `SimplePwm` writes the compare
    /// registers directly, so nothing may be bound for it.
    #[test]
    fn pwm_binds_no_interrupts() {
        let out = run(&[mk("PA6", 3, 1)], Default::default());
        assert!(!out.any_async_dma);
        assert!(out.dma_irqs.is_empty(), "{}", out.dma_irqs);
        assert!(out.dma_uses.is_empty());
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
                .contains("init(p.USART1, p.PA10, p.PA9, p.PA12, p.PA11, Irqs)"),
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
                .contains("init(p.USART1, p.PA10, p.PA9, Irqs)"),
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
            out.init_calls.contains("init(p.USART1, p.PA9, Irqs)"),
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

    /// Swap / invert reach `get_config`, as consts, and ONLY when switched on —
    /// so a project that uses none generates exactly what it did before.
    #[test]
    fn line_extras_are_opt_in_and_chip_gated() {
        let plain = run(&full_duplex(), UsartModuleConfig::new(1));
        let body = &plain.config_files[0].1;
        assert!(!body.contains("swap_rx_tx"), "{body}");
        assert!(!body.contains("invert"), "{body}");

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
        // Only what was asked for: TX inversion was left off.
        assert!(!body.contains("invert_tx"), "{body}");

        // Same config on an OLD USART: the fields do not exist there, so nothing
        // is emitted even though the module still carries the choice.
        let pins = full_duplex();
        let refs: Vec<&Pin> = pins.iter().collect();
        let old = async_peripherals(
            "stm32f4",
            ChipData {
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci2_v1_2_Cube"), // F411 — usart_v2
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
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
            out.init_calls.contains("init(p.USART1, p.PA10, p."),
            "{}",
            out.init_calls
        );
    }
}

#[cfg(test)]
mod spi_i2c_option_tests {
    use super::*;

    /// The default writes NOTHING, so every project that already exists keeps
    /// byte-identical output. That is the point of the `if default` guard.
    #[test]
    fn the_defaults_add_no_line_at_all() {
        let spi = spi_config_file(1, Some(&SpiModuleConfig::new(1)), false);
        assert!(!spi.contains("bit_order"), "{spi}");
        let i2c = i2c_config_file(1, Some(&I2cModuleConfig::new(1)));
        assert!(!i2c.contains("timeout"), "{i2c}");
    }

    /// BOTH I2C templates declare the bus's devices - each one's address is in
    /// its own file beside this `mod.rs`, so the bus names none - and the usage
    /// example uses that address rather than inventing one of its own.
    ///
    /// Each example used to open with `const ADDR: u8 = 0x3C;` - a literal that
    /// ignored whatever the user had set two panels away, so following the
    /// example talked to the wrong device.
    #[test]
    fn both_i2c_templates_declare_their_devices_and_the_example_uses_their_address() {
        for mode in [AsyncBusMode::Blocking, AsyncBusMode::AsyncDma] {
            let c = I2cModuleConfig {
                address: 0x68,
                async_mode: mode,
                ..I2cModuleConfig::new(1)
            };
            let f = i2c_config_file(1, Some(&c));
            assert!(f.contains("pub mod device1;"), "{f}");
            assert!(!f.contains("pub const DEVICE_ADDRESS"), "{f}");
            assert!(!f.contains("const ADDR: u8"), "{f}");
            assert!(f.contains("DEVICE_ADDRESS, &[0x10]"), "{f}");
            assert!(f.contains("i2c1::device1_<name>::DEVICE_ADDRESS"), "{f}");
        }
    }

    /// The device modules sit INSIDE the generated markers - they follow the
    /// Virtual Module - and the END marker still starts its own line: the
    /// template puts it straight after them.
    #[test]
    fn the_devices_are_in_the_regenerated_half() {
        for address in [0, 0x3C] {
            let c = I2cModuleConfig {
                address,
                ..I2cModuleConfig::new(1)
            };
            let f = i2c_config_file(1, Some(&c));
            let gen_end = f.find("// <<< GENERATED END >>>").expect("markers");
            assert_eq!(&f[gen_end - 1..gen_end], "\n", "END joined a line:\n{f}");
            let devices = f
                .find("each holding its DEVICE_ADDRESS")
                .or(f.find("No device on this bus yet"));
            assert!(devices.is_some_and(|d| d < gen_end), "{f}");
        }
    }

    /// LSB first reaches the config, fully qualified so no template needs a
    /// conditional `use` line for a type it may not mention.
    #[test]
    fn lsb_first_reaches_every_spi_template() {
        for tx_only in [false, true] {
            for mode in [AsyncBusMode::Blocking, AsyncBusMode::AsyncDma] {
                let c = SpiModuleConfig {
                    bit_order: SpiBitOrder::LsbFirst,
                    async_mode: mode,
                    ..SpiModuleConfig::new(1)
                };
                let f = spi_config_file(1, Some(&c), tx_only);
                assert!(
                    f.contains("config.bit_order = embassy_stm32::spi::BitOrder::LsbFirst;"),
                    "{mode:?} tx_only={tx_only}:\n{f}"
                );
            }
        }
    }

    #[test]
    fn a_timeout_reaches_every_i2c_template() {
        for mode in [AsyncBusMode::Blocking, AsyncBusMode::AsyncDma] {
            let c = I2cModuleConfig {
                timeout_ms: 250,
                async_mode: mode,
                ..I2cModuleConfig::new(1)
            };
            let f = i2c_config_file(1, Some(&c));
            assert!(
                f.contains("config.timeout = embassy_time::Duration::from_millis(250);"),
                "{mode:?}:\n{f}"
            );
        }
    }

    /// A hand-edited `mcu.config` cannot produce a timeout that fails every
    /// transfer instantly, which would read as broken wiring rather than as a
    /// setting. (0 is not clamped — it is the "leave embassy's default" value.)
    #[test]
    fn an_absurd_timeout_is_clamped() {
        let huge = i2c_config_file(
            1,
            Some(&I2cModuleConfig {
                timeout_ms: 10_000_000,
                ..I2cModuleConfig::new(1)
            }),
        );
        assert!(huge.contains("from_millis(60000);"), "{huge}");
    }

    /// Neither setting leaves a placeholder behind — a stray `{MSB}` in a
    /// user's file would be a syntax error they did not write.
    #[test]
    fn no_placeholder_survives_substitution() {
        let spi = spi_config_file(2, Some(&SpiModuleConfig::new(2)), false);
        let i2c = i2c_config_file(2, Some(&I2cModuleConfig::new(2)));
        for f in [&spi, &i2c] {
            assert!(!f.contains('{') || !f.contains("_CFG}"), "{f}");
            for ph in ["{MSB}", "{TMO}", "{EXTRA_CFG}", "{DEVICES}"] {
                assert!(!f.contains(ph), "left {ph} behind:\n{f}");
            }
        }
    }
}

#[cfg(test)]
mod async_duty_handle_tests {
    use super::*;

    fn wiring(chans: &[u8], comp: &[u8]) -> PwmWiring {
        PwmWiring {
            chans: chans.iter().map(|c| (*c, format!("pa{c}"))).collect(),
            comp: comp.iter().map(|c| (*c, format!("pb{c}"))).collect(),
            breaks: Vec::new(),
        }
    }

    /// `DutyHandle` may never name a channel the timer has no pad for.
    ///
    /// The same rule as the F1 backend, for a different reason: there, asking
    /// for an unwired channel PANICS; here embassy takes it and drives nothing,
    /// which is quieter and no more useful. Both are answered by putting the
    /// channel in the method NAME.
    #[test]
    fn simple_pwm_only_reaches_wired_channels() {
        let cfg = TimerModuleConfig::new(3);
        let f = pwm_config_file(3, &cfg, &wiring(&[2, 4], &[]), "_pwm3");

        assert!(
            f.contains("impl<'d> DutyHandle for SimplePwm<'d, peripherals::TIM3>"),
            "{f}"
        );
        assert!(
            f.contains("fn set_duty_tim_3_ch2(&mut self, value: u32);"),
            "{f}"
        );
        assert!(
            f.contains("fn set_duty_tim_3_ch4(&mut self, value: u32);"),
            "{f}"
        );
        // CH1 and CH3 have no pad, so nothing may reach them.
        for ch in [1, 3] {
            assert!(
                !f.contains(&format!("set_duty_tim_3_ch{ch}")),
                "TIM3 CH{ch} is unwired:\n{f}"
            );
            assert!(!f.contains(&format!("self.ch{ch}()")), "{f}");
        }
        // The bare method delegates to the lowest wired channel, not to CH1.
        assert!(f.contains("        self.set_duty_tim_3_ch2(value);"), "{f}");
    }

    /// The complementary driver has a DIFFERENT duty API — `set_duty(Channel,
    /// value)` against `get_max_duty()`, not `.chN()` — so the trait body is
    /// generated per driver rather than shared. Getting that backwards compiles
    /// nowhere, which is exactly why it is worth pinning.
    #[test]
    fn complementary_pwm_uses_its_own_duty_api() {
        let cfg = TimerModuleConfig::new(1);
        let f = pwm_config_file(1, &cfg, &wiring(&[1], &[1]), "_pwm1");

        assert!(
            f.contains("impl<'d> DutyHandle for ComplementaryPwm<'d, peripherals::TIM1>"),
            "{f}"
        );
        assert!(
            f.contains(
                "        self.set_duty(Channel::Ch1, self.get_max_duty() * value / 10_000);"
            ),
            "{f}"
        );
        assert!(
            !f.contains("set_duty_cycle_fraction(value"),
            "wrong driver's API:\n{f}"
        );
        // A complementary channel means BOTH its pads, and the doc says so.
        assert!(f.contains("/// CH1 and CH1N."), "{f}");
    }
}

#[cfg(test)]
mod async_tail_on_switch {
    use super::super::common::{ASYNC_USER_TAIL, USER_TAIL};
    use super::{GEN_BEGIN, GEN_END, splice_section};

    fn file_with(tail: &str) -> String {
        format!("// header\n{GEN_BEGIN}\n    let p = init();\n{GEN_END}\n\n{tail}")
    }

    /// A project switched Blocking -> Async is spliced, never freshly generated,
    /// so this is the only place the warning can reach an existing file.
    #[test]
    fn switching_to_async_gains_the_await_warning() {
        let out = splice_section(&file_with(USER_TAIL), "SECTION", "STM32G431", "g431");
        assert!(out.contains("Every iteration must `.await`"), "{out}");
        assert!(out.contains("!!! IMPORTANT !!!"), "{out}");
        assert_eq!(
            out.matches("// Your main loop code here.").count(),
            1,
            "the tail was exchanged, not duplicated:\n{out}"
        );
    }

    /// …but only while the tail is still ours. A loop the user wrote in is not
    /// something a runtime switch gets to rewrite.
    #[test]
    fn a_loop_the_user_wrote_is_not_touched_by_the_switch() {
        let mine = "    loop {\n        led.toggle();\n    }\n}\n";
        let out = splice_section(&file_with(mine), "SECTION", "STM32G431", "g431");
        assert!(out.contains("led.toggle();"), "{out}");
        assert!(
            !out.contains("IMPORTANT"),
            "no warning over user code:\n{out}"
        );
    }

    /// A file with no markers is rebuilt from scratch — it must get the async
    /// tail, not the blocking one.
    #[test]
    fn a_rebuilt_file_gets_the_async_tail() {
        let out = splice_section("not our file", "SECTION", "STM32G431", "g431");
        assert!(out.ends_with(ASYNC_USER_TAIL), "{out}");
    }
}

/// The STM32F1 on embassy-stm32: the AFIO remap every pin trait carries, the
/// peripherals embassy gives the F1 nothing for, and the timer the time driver
/// takes.
#[cfg(test)]
mod f1_async_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, f: PinFunction) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = f;
        p
    }

    fn run(family: &str, pins: &[Pin]) -> AsyncPeriphs {
        run_with(family, &[], pins)
    }

    fn run_with(family: &str, irq_vectors: &[String], pins: &[Pin]) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            family,
            ChipData {
                dma: None,
                irq_vectors,
                usart_ip: Some("sci2_v1_2_Cube"),
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
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    fn file<'a>(out: &'a AsyncPeriphs, name: &str) -> &'a str {
        out.config_files
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, b)| b.as_str())
            .unwrap_or_else(|| panic!("no {name}: {:?}", out.config_files))
    }

    /// I2S runs on an SPI block, and on the F1 its pads carry the remap too -
    /// the master clock's included. `init` takes the `A` ahead of its DMA.
    #[test]
    fn an_f1_i2s_file_takes_the_remap_on_every_pad() {
        let pins = [
            mk("PB13", PinFunction::I2sCk(2)),
            mk("PB12", PinFunction::I2sWs(2)),
            mk("PB15", PinFunction::I2sSd(2)),
            mk("PC6", PinFunction::I2sMck(2)),
        ];
        let out = run("stm32f1", &pins);
        let body = file(&out, "i2s2.rs");
        for bound in [
            "impl I2sSdPin<peripherals::SPI2, A>",
            "impl WsPin<peripherals::SPI2, A>",
            "impl CkPin<peripherals::SPI2, A>",
            "impl MckPin<peripherals::SPI2, A>",
        ] {
            assert!(body.contains(bound), "{bound}\n\n{body}");
        }
        assert!(body.contains("pub fn init<'d, A, D: "), "{body}");
        // Another family's file is untouched by the pass.
        let g4 = run("stm32g4", &pins);
        let g4 = file(&g4, "i2s2.rs");
        assert!(!g4.contains(", A>"), "{g4}");
    }

    /// The SD host's `CkPin` is the one bound WITHOUT a remap parameter, so a
    /// text pass must step over it - and on the F1 the SDIO is not generated at
    /// all: embassy-stm32 gives it no DMA channel.
    #[test]
    fn the_sdio_clock_takes_no_remap_and_the_f1_sdio_is_a_note() {
        let sd = "pub fn init<'d>(\n    ck: Peri<'d, impl CkPin<peripherals::SDIO>>,\n";
        assert_eq!(with_afio_remap(sd), sd, "a file with only the SD clock");
        let mixed = format!("{sd}    ck2: Peri<'d, impl CkPin<peripherals::USART1>>,\n");
        let out = with_afio_remap(&mixed);
        assert!(out.contains("impl CkPin<peripherals::SDIO>>"), "{out}");
        assert!(out.contains("impl CkPin<peripherals::USART1, A>>"), "{out}");
        assert!(out.starts_with("pub fn init<'d, A>("), "{out}");

        let mut pins = vec![
            mk("PC12", PinFunction::SdmmcCk { unit: 0 }),
            mk("PD2", PinFunction::SdmmcCmd { unit: 0 }),
        ];
        for lane in 0..4 {
            pins.push(mk(
                &format!("PC{}", 8 + lane),
                PinFunction::SdmmcD { unit: 0, lane },
            ));
        }
        let out = run("stm32f1", &pins);
        assert!(
            !out.config_files.iter().any(|(n, _)| n.starts_with("sdio")),
            "{:?}",
            out.config_files
        );
        assert!(
            out.init_calls.contains("SDIO is NOT initialised"),
            "{}",
            out.init_calls
        );
    }

    /// TIM5 and TIM8 have no AFIO remap field on the F1, but their pads still
    /// carry the parameter - as `AfioRemapNotApplicable`.
    #[test]
    fn tim8_on_the_f1_names_no_remap() {
        let pins = [mk(
            "PC6",
            PinFunction::TimerPwm {
                timer: 8,
                channel: 1,
            },
        )];
        let out = run("stm32f1", &pins);
        let body = file(&out, "pwm8.rs");
        assert!(
            body.contains("pub type Remap = embassy_stm32::gpio::AfioRemapNotApplicable;"),
            "{body}"
        );
        assert!(
            body.contains("impl TimerPin<peripherals::TIM8, Ch1, Remap>>"),
            "{body}"
        );
    }

    /// `time-driver-any` takes a timer out of `Peripherals` - TIM4 on an F103C8
    /// - so PWM on it is refused with a note, on the F1 and elsewhere alike.
    #[test]
    fn the_time_driver_timer_is_not_offered_to_pwm() {
        let mut tim4 = mk(
            "PB8",
            PinFunction::TimerPwm {
                timer: 4,
                channel: 3,
            },
        );
        let mut tim3 = mk(
            "PA6",
            PinFunction::TimerPwm {
                timer: 3,
                channel: 1,
            },
        );
        tim4.available_functions = vec![tim4.selected_function.clone()];
        tim3.available_functions = vec![tim3.selected_function.clone()];
        let pins = [tim4, tim3];
        let refs: Vec<&Pin> = pins.iter().collect();
        // Embassy's order puts TIM4 before TIM3.
        assert_eq!(time_driver_timer(&refs), Some(4));
        let out = run("stm32f1", &pins);
        assert!(
            out.init_calls
                .contains("TIM4 is NOT initialised: embassy-time runs on it"),
            "{}",
            out.init_calls
        );
        assert!(!out.config_files.iter().any(|(n, _)| n == "pwm4.rs"));
        assert!(out.config_files.iter().any(|(n, _)| n == "pwm3.rs"));
    }

    /// The F1's fourth and fifth serial ports are UARTs, and embassy's
    /// singleton says so: `p.UART4`, never `p.USART4` (E0609). The pin function
    /// and the config module keep the USART spelling.
    #[test]
    fn an_f1_uart4_is_named_uart_in_everything_it_generates() {
        let pins = [
            mk("PC10", PinFunction::UsartTx(4)),
            mk("PC11", PinFunction::UsartRx(4)),
        ];
        // An F103RC-style vector list, and none at all: both name UART4.
        let vectors = vec!["USART1".to_owned(), "UART4".to_owned(), "UART5".to_owned()];
        for out in [run_with("stm32f1", &vectors, &pins), run("stm32f1", &pins)] {
            let body = file(&out, "usart4.rs");
            let all = format!("{}{}{body}", out.init_calls, out.dma_irqs);
            assert!(
                out.init_calls.contains("usart4::init(p.UART4"),
                "{}",
                out.init_calls
            );
            assert!(body.contains("peripherals::UART4"), "{body}");
            assert!(out.dma_irqs.contains("UART4 =>"), "{}", out.dma_irqs);
            // (The template's own comment names the G0's `USART3_4_LPUART1`
            // as an example; only CODE must not say USART4.)
            for code in ["p.USART4", "peripherals::USART4", "USART4 =>"] {
                assert!(!all.contains(code), "{code}\n\n{all}");
            }
        }
        // A chip whose USART4 IS a USART keeps it: a G0 routes it through
        // `USART3_4_LPUART1`.
        let g0 = run_with("stm32g0", &["USART3_4_LPUART1".to_owned()], &pins);
        assert!(g0.init_calls.contains("p.USART4"), "{}", g0.init_calls);
        // And USART1..3 stay USARTs on the F1.
        assert_eq!(serial_word("USART", "stm32f1", &[], 3), "USART");
        assert_eq!(serial_word("LPUART", "stm32f1", &[], 4), "LPUART");
    }

    /// An F1 carries no vector list, so the EXTI plan falls back to the F1's
    /// fixed vectors: lines 5..9 share `EXTI9_5`, 10..15 `EXTI15_10`.
    #[test]
    fn an_armed_f1_input_finds_its_shared_vector() {
        let mut pb5 = mk("PB5", PinFunction::GpioInput);
        pb5.irq = Some(Edge::Both);
        let out = run("stm32f1", &[pb5]);
        assert_eq!(out.exti.len(), 1, "{:?}", out.exti);
        assert_eq!(out.exti[0].vector, "EXTI9_5");
    }
}
