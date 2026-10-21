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

use super::tmpl;

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

mod afio;
mod comp;
mod dac;
mod dma;
mod exti;
mod hspi;
mod i2c;
mod i2s;
mod irqs;
mod ospi;
mod pwm;

/// The timer `time-driver-any` takes on this chip - [`pwm::time_driver_timer`]
/// for the Clock tab, which checks embassy-time can run on it.
pub fn time_driver_timer_of(
    pins: &[&crate::panels::mcu_module::pins::logic::pin::Pin],
) -> Option<u8> {
    pwm::time_driver_timer(pins)
}
mod qspi;
mod sai;
mod sdmmc;
mod spi;
mod usart;
mod xspi;

use afio::*;
pub(crate) use comp::*;
pub(crate) use dac::*;
use dma::*;
pub(crate) use exti::*;
pub(crate) use hspi::*;
pub(crate) use i2c::*;
pub(crate) use i2s::*;
use irqs::*;
pub(crate) use ospi::*;
pub(crate) use pwm::*;
pub(crate) use qspi::*;
pub(crate) use sai::*;
pub(crate) use sdmmc::*;
pub(crate) use spi::*;
pub(crate) use usart::*;
pub(crate) use xspi::*;

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
