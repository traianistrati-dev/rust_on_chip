//! STM32 code generation — HAL code building, section splicing, pin parsing.

use super::super::clock::frequencies;
use super::super::clock::model::{ClockConfig, PllSrc, Stm32f1Clock, SysclkSrc};
use super::super::modules::model::BlockingDma;
use super::super::modules::{
    ApiStyle, CanModuleConfig, I2cModuleConfig, Parity, SpiModuleConfig, StopBits,
    TimerModuleConfig, UsartModuleConfig, UsbModuleConfig,
};
use super::super::pins::logic::pin::{GpioMode, Pin};
use super::super::pins::logic::pin_function::PinFunction;
use super::common::AUTOGEN_BANNER;
use super::common::duty_percent_str;
use super::common::retarget_pristine_tail;
use super::{GEN_BEGIN, GEN_END, USER_TAIL, mcu_id_marker_line, pin_binding, sanitize_label};
use std::collections::{BTreeMap, BTreeSet};

/// `_<sanitized label>` suffix for a module's generated handle variable, or ""
/// when the module has no user label. So a GI_SPI labelled "imu" turns `_spi1`
/// into `_spi1_imu` — the module analogue of the per-pin `<pin>_<type>_<label>`.
fn module_label_sfx(label: &str) -> String {
    let s = sanitize_label(label);
    if s.is_empty() {
        String::new()
    } else {
        format!("_{s}")
    }
}

/// Variable (binding) name for a pin: `<pin>_<type>[_<label>]`, e.g. `pc13_out`,
/// `pb9_i2c1_sda`, or `pc13_out_led` when the pin carries a user label. Only the
/// `let` binding and later references carry the suffix — the HAL field access
/// stays the bare pin (`meta.var`, e.g. `gpioc.pc13`).
fn binding_of(pin: &Pin, meta: &PinMeta) -> String {
    pin_binding(&meta.var, &pin.selected_function, &pin.custom_label)
}

// ── Section splicing ──────────────────────────────────────────────────────────

pub fn splice_section(existing: &str, new_section: &str, mcu_name: &str, mcu_id: &str) -> String {
    if let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END)) {
        let end = end_start + GEN_END.len();
        // Strip ALL leading newlines after GEN_END, then re-add exactly one
        // blank line.  This makes splice idempotent: running it N times always
        // produces the same result instead of accumulating newlines.
        let after = retarget_pristine_tail(existing[end..].trim_start_matches('\n'), false);

        // Detect old format (had fn custom_config / fn loop_code after GEN_END).
        // Rebuild from scratch so the user tail is the new flat loop{} style.
        if after.contains("fn custom_config()") || after.contains("fn loop_code()") {
            return format!(
                "{}{}\n{}",
                invariant_header(mcu_name, mcu_id),
                new_section,
                USER_TAIL,
            );
        }

        // new_section already ends with "// <<< GENERATED END >>>\n"
        // + one more "\n" gives a single blank line before the user tail.
        format!("{}{}\n{}", &existing[..begin], new_section, after)
    } else {
        // Markers not found — rebuild from scratch.
        format!(
            "{}{}\n{}",
            invariant_header(mcu_name, mcu_id),
            new_section,
            USER_TAIL,
        )
    }
}

// ── Invariant file header ─────────────────────────────────────────────────────

pub fn invariant_header(mcu_name: &str, mcu_id: &str) -> String {
    format!(
        "{AUTOGEN_BANNER}\n\
         // MCU: {mcu_name} | HAL: stm32f1xx-hal\n\
         {id}\n\
         #![no_std]\n\
         #![no_main]\n\n\
         pub mod pins;\n\n\
         use panic_halt as _;\n\
         use cortex_m_rt::entry;\n\n",
        id = mcu_id_marker_line(mcu_id),
    )
}

// ── Clock setup (rcc.cfgr chain) ──────────────────────────────────────────────

/// Format a Hz value as a stm32f1xx-hal rate literal (`72.MHz()`, `48.kHz()`…).
fn freq_lit(hz: u32) -> String {
    if hz % 1_000_000 == 0 {
        format!("{}.MHz()", hz / 1_000_000)
    } else if hz % 1_000 == 0 {
        format!("{}.kHz()", hz / 1_000)
    } else {
        format!("{hz}.Hz()")
    }
}

/// Build the `rcc.cfgr … .freeze(&mut flash.acr)` chain from the clock config.
///
/// Only knobs that deviate from the HAL's natural defaults are emitted, so the
/// default 72 MHz config produces exactly the original
/// `use_hse(8).sysclk(72).pclk1(36)` chain (no spurious diffs).
pub fn clock_setup_chain(clock: &ClockConfig) -> String {
    let a = cfgr_args(&typed_f1_clock(clock));
    // Newline + 17 spaces → lines up the `.method()` calls under `rcc.cfgr`.
    const IND: &str = "\n                 ";

    let mut s = String::from("rcc.cfgr");
    if let Some(hse) = a.hse {
        s.push_str(&format!("{IND}.use_hse({})", freq_lit(hse)));
    }
    s.push_str(&format!("{IND}.sysclk({})", freq_lit(a.sysclk)));
    s.push_str(&format!("{IND}.pclk1({})", freq_lit(a.pclk1)));
    if let Some(hclk) = a.hclk {
        s.push_str(&format!("{IND}.hclk({})", freq_lit(hclk)));
    }
    if let Some(pclk2) = a.pclk2 {
        s.push_str(&format!("{IND}.pclk2({})", freq_lit(pclk2)));
    }
    if let Some(adcclk) = a.adcclk {
        s.push_str(&format!("{IND}.adcclk({})", freq_lit(adcclk)));
    }
    s.push_str(&format!("{IND}.freeze(&mut flash.acr)"));
    s
}

/// The graph read back into the typed codegen intermediate. The graph is the
/// only clock model; a chip without one gets the Blue Pill default.
fn typed_f1_clock(clock: &ClockConfig) -> Stm32f1Clock {
    match clock {
        ClockConfig::Graph(gc) => {
            crate::panels::mcu_module::clock::graph::graph_to_stm32f1(&gc.for_codegen())
        }
        ClockConfig::None => Stm32f1Clock::default(),
    }
}

/// What [`clock_setup_chain`] hands the `rcc.cfgr` builder, `None` where it
/// leaves the HAL's own default. One struct for the emitter AND for
/// [`f1_hal_pclks`], so the clocks the UART check assumes are always the ones
/// the HAL is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CfgrArgs {
    hse: Option<u32>,
    sysclk: u32,
    pclk1: u32,
    hclk: Option<u32>,
    pclk2: Option<u32>,
    adcclk: Option<u32>,
}

fn cfgr_args(c: &Stm32f1Clock) -> CfgrArgs {
    let f = frequencies(c);
    let use_hse = c.hse_enabled
        && (c.sysclk_src == SysclkSrc::Hse
            || (c.sysclk_src == SysclkSrc::Pll
                && matches!(c.pll_src, PllSrc::Hse | PllSrc::HseDiv2)));
    CfgrArgs {
        hse: use_hse.then_some(c.hse_hz),
        sysclk: f.sysclk,
        pclk1: f.pclk1,
        hclk: (c.ahb_pre != 1).then_some(f.hclk),
        pclk2: (c.apb2_pre != 1).then_some(f.pclk2),
        adcclk: (c.adc_pre != 6).then_some(f.adcclk),
    }
}

/// PCLK1 and PCLK2 as stm32f1xx-hal 0.10 will REALLY program them from the
/// emitted chain - or `None` when its `freeze` asserts, and the chip panics
/// before any peripheral exists.
///
/// Not the Clock tab's numbers. The chain only hands the HAL target
/// frequencies, and the HAL derives its own dividers from them, which differs
/// from the tab in three places:
///
/// * it cannot program PLLXTPRE, so HSE/2 into the PLL becomes HSE, and the
///   multiplier is re-derived from `sysclk / hse` (truncating) - HSE 8 MHz,
///   /2, x9 asks for 36 MHz and gets 32;
/// * it rounds the APB1 ratio UP, so an odd HCLK asked to halve lands on /4;
/// * it asserts its own ceilings (PCLK1 36 MHz, ADCCLK 14 MHz, ...) in
///   `get_clocks`, which the tab only flags.
///
/// A mirror of `Config::from_cfgr` + `get_clocks` (rcc.rs 550-720), arm for
/// arm, including where each prescaler match rounds.
pub fn f1_hal_pclks(clock: &ClockConfig) -> Option<(u32, u32)> {
    hal_pclks(&cfgr_args(&typed_f1_clock(clock)))
}

fn hal_pclks(a: &CfgrArgs) -> Option<(u32, u32)> {
    const HSI: u32 = 8_000_000;
    let pllsrc = a.hse.unwrap_or(HSI / 2);
    let sysclk = match a.sysclk / pllsrc {
        1 => a.hse.unwrap_or(HSI),
        // Clamped to 1, then `pllmul as u8 - 2`: an overflow in the HAL itself.
        0 => return None,
        m => pllsrc * m.min(16),
    };
    let hpre = match a.hclk {
        Some(h) if h > 0 => match sysclk / h {
            0..=1 => 1,
            2 => 2,
            3..=5 => 4,
            6..=11 => 8,
            12..=39 => 16,
            40..=95 => 64,
            96..=191 => 128,
            192..=383 => 256,
            _ => 512,
        },
        Some(_) => return None,
        None => 1,
    };
    let hclk = sysclk / hpre;
    if a.pclk1 == 0 {
        return None;
    }
    let ppre = |ratio: u32| match ratio {
        0..=1 => 1,
        2 => 2,
        3..=5 => 4,
        6..=11 => 8,
        _ => 16,
    };
    // APB1 rounds its ratio UP; APB2 below does not.
    let pclk1 = hclk / ppre(hclk.div_ceil(a.pclk1));
    let pclk2 = match a.pclk2 {
        Some(p) if p > 0 => hclk / ppre(hclk / p),
        Some(_) => return None,
        None => hclk,
    };
    let apre = match a.adcclk {
        Some(ad) if ad > 0 => match pclk2 / ad {
            0..=2 => 2,
            3..=4 => 4,
            5..=7 => 6,
            _ => 8,
        },
        Some(_) => return None,
        None => 8,
    };
    let adcclk = pclk2 / apre;
    let ok = sysclk <= 72_000_000
        && hclk <= 72_000_000
        && pclk1 <= 36_000_000
        && pclk2 <= 72_000_000
        && adcclk <= 14_000_000;
    ok.then_some((pclk1, pclk2))
}

// ── Generated section builder ─────────────────────────────────────────────────

/// The pieces of an STM32F1 init sequence, before they are arranged into a
/// program.
///
/// Extracted so the RTIC backend can lay the SAME init out inside `#[init]`
/// instead of `fn main`. Two copies of this logic would drift the first time a
/// peripheral is added, and only one of them would be the one under test.
pub(super) struct GenParts {
    /// Items for the `use stm32f1xx_hal::{ ... };` block, already indented.
    pub use_block: String,
    /// Top-level `use`s that cannot live inside the HAL block (USB).
    pub extra_uses: &'static str,
    /// `let mut afio = ...;` or empty.
    pub afio_line: &'static str,
    /// Right-hand side of `let clocks = ...`.
    pub clock_chain: String,
    /// `let mut gpioX = dp.GPIOX.split();` lines.
    pub port_splits: String,
    /// Per-pin `let` bindings, grouped by port.
    pub pin_section: String,
    /// Peripheral init calls (`pins::configs::usart1::init(...)`, ADC, CAN...).
    pub fn_calls: String,
    /// `(binding, concrete type, written through during init)` for the
    /// peripherals that have NO config module to name a `Handle` for them.
    ///
    /// Just the ADC now: the PWM timers moved into `pins/configs/pwm{N}.rs`
    /// and got a `Handle` alias with the rest, so they promote through
    /// `BUS_HANDLES` like every other peripheral.
    ///
    /// The third field decides whether the promoted binding keeps its `mut`.
    ///
    /// The bare-metal path ignores this: `fn main` runs forever, so a value it
    /// keeps is a value that lives. RTIC's `#[init]` RETURNS, and everything it
    /// did not hand to the framework is dropped right there — which is what
    /// `rtic::promote_bus_handles` fixes for the buses, using the `Handle`
    /// alias their config modules expose. These two have no such module, so the
    /// type is spelled out here, where the timer, the remap and the pin types
    /// are all already known.
    pub inline_handles: Vec<(String, String, bool)>,
    /// Module-level items for the bare-metal EXTI path: the statics that park
    /// each armed input, and one `#[interrupt]` per vector. Empty on the RTIC
    /// path, which turns the same pins into tasks instead.
    pub irq_items: String,
}

/// Build the init pieces. `None` when no pin is configured — the caller decides
/// what an empty project looks like.
#[allow(clippy::too_many_arguments)]
/// How a pin's `let` binding is produced.
///
/// The bare-metal path takes `&mut` of the freshly-configured pin because
/// everything stays inside one `fn main` and nothing outlives it. RTIC cannot:
/// each pin is MOVED into a `Local` resource that outlives `#[init]`, and a
/// `&mut` to a temporary would not even compile. Same lines, one prefix apart.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Binding {
    /// `let p = &mut gpioa.pa0.into_...();` — the historical form.
    MutRef,
    /// `let p = gpioa.pa0.into_...();` — movable into an RTIC resource.
    Owned,
}

pub(super) fn gen_parts(
    binding_style: Binding,
    all_pins: &[&Pin],
    clock: &ClockConfig,
    usart: &BTreeMap<u8, UsartModuleConfig>,
    spi: &BTreeMap<u8, SpiModuleConfig>,
    i2c: &BTreeMap<u8, I2cModuleConfig>,
    can: &BTreeMap<u8, CanModuleConfig>,
    usb: &BTreeMap<u8, UsbModuleConfig>,
    timer: &BTreeMap<u8, TimerModuleConfig>,
    gpio_native: bool,
    // `let x = Foo::new(pa0_out, …);` lines for the Custom modules — appended
    // last, after every binding/init they consume (see `Mcu::custom_module_inits`).
    custom_inits: &str,
) -> Option<GenParts> {
    let configured: Vec<(&Pin, PinMeta)> = all_pins
        .iter()
        .filter(|p| !p.reserved && p.selected_function != PinFunction::Unset)
        .filter_map(|p| parse_pin(&p.name).map(|m| (*p, m)))
        .collect();

    if configured.is_empty() {
        return None;
    }

    // ── Ports used ───────────────────────────────────────────────────────────
    // A port whose pins are configured is split `let mut` — every `into_*` takes
    // `&mut gpioX.crl`. A port pulled in only to free the JTAG pins below is not
    // written through at all, so it must NOT be `mut`.
    let mut ports_written: BTreeSet<char> = BTreeSet::new();
    for (_, meta) in &configured {
        ports_written.insert(meta.port);
    }
    let mut ports_used: BTreeSet<char> = ports_written.clone();

    // ── JTAG pins ────────────────────────────────────────────────────────────
    // PA15, PB3 and PB4 come out of reset as the JTAG port, typed
    // `Pin<'B', 3, Debugger>` — a state with no `into_*` methods at all, so a
    // project using any of them did not compile. `afio.mapr.disable_jtag` is the
    // way out, and it takes ALL THREE at once (used or not), which is why the
    // ports of the other two get split here too.
    let jtag_used = configured
        .iter()
        .any(|(p, _)| JTAG_PINS.contains(&p.name.as_str()));
    // Two of three cannot be handed to the call, so a chip missing one is left
    // exactly as it was rather than given code that won't build either way.
    let jtag_pins_exist = JTAG_PINS
        .iter()
        .all(|n| all_pins.iter().any(|p| p.name == *n));
    let free_jtag = jtag_used && jtag_pins_exist;
    if free_jtag {
        ports_used.insert('A');
        ports_used.insert('B');
    }

    // ── Feature flags ────────────────────────────────────────────────────────
    let has_serial = configured.iter().any(|(p, _)| {
        matches!(
            p.selected_function,
            PinFunction::UsartTx(_) | PinFunction::UsartRx(_)
        )
    });
    let has_spi = configured.iter().any(|(p, _)| {
        matches!(
            p.selected_function,
            PinFunction::SpiSck(_) | PinFunction::SpiMosi(_)
        )
    });
    let has_i2c = configured.iter().any(|(p, _)| {
        matches!(
            p.selected_function,
            PinFunction::I2cScl(_) | PinFunction::I2cSda(_)
        )
    });
    let has_adc = configured
        .iter()
        .any(|(p, _)| matches!(p.selected_function, PinFunction::AdcChannel { .. }));
    let has_timer = configured
        .iter()
        .any(|(p, _)| matches!(p.selected_function, PinFunction::TimerPwm { .. }));
    let has_can = configured
        .iter()
        .any(|(p, _)| matches!(p.selected_function, PinFunction::CanRx | PinFunction::CanTx));
    // USB is the odd one out: its init does not READ the wired pins, it takes
    // `gpioa.pa11` / `gpioa.pa12` directly, because those are the only USB pads
    // on this family. So one wired pad used to produce a whole two-pad
    // peripheral — silently spending a pad the user had not given it, and
    // failing to compile outright (E0382, moved value) when they had spent it
    // on something else. Both pads or nothing, like every other bus here.
    let usb_dm = configured
        .iter()
        .any(|(p, _)| p.selected_function == PinFunction::UsbDm);
    let usb_dp = configured
        .iter()
        .any(|(p, _)| p.selected_function == PinFunction::UsbDp);
    let has_usb = usb_dm && usb_dp;
    let usb_half = (usb_dm || usb_dp) && !has_usb;
    // CAN's `assign_pins` needs AFIO too (it may remap the pins), and so does
    // `disable_jtag` — it lives on `afio.mapr`.
    let needs_afio = has_serial || has_spi || has_i2c || has_timer || has_can || free_jtag;
    let _has_periph_fns = has_serial || has_spi || has_i2c || has_adc || has_can;

    // ── SPI instances ────────────────────────────────────────────────────────
    let mut spi_instances: BTreeSet<u8> = BTreeSet::new();
    for (pin, _) in &configured {
        match pin.selected_function {
            PinFunction::SpiSck(n)
            | PinFunction::SpiMosi(n)
            | PinFunction::SpiMiso(n)
            | PinFunction::SpiNss(n) => {
                spi_instances.insert(n);
            }
            _ => {}
        }
    }

    // ── HAL use block ────────────────────────────────────────────────────────
    let mut use_items: Vec<String> = vec!["pac".into(), "prelude::*".into()];
    // if has_periph_fns || needs_afio {
    //     if needs_afio {
    //         use_items.push("afio".into());
    //     }
    //     use_items.push("rcc::Clocks".into());
    // }
    if has_serial {
        // use_items.push("serial::{self, Config, Serial}".into());
    }
    if has_spi {
        // let remaps = spi_instances
        //     .iter()
        //     .map(|n| format!("Spi{n}NoRemap"))
        //     .collect::<Vec<_>>()
        //     .join(", ");
        // use_items.push(format!(
        //     "spi::{{self, Mode, Phase, Polarity, Spi, {remaps}}}"
        // ));
    }
    if has_i2c {
        //  use_items.push("i2c::{self, BlockingI2c, Mode as I2cMode}".into());
    }
    if has_adc {
        use_items.push("adc".into());
    }
    if has_usb {
        use_items.push("usb::{Peripheral, UsbBus}".into());
    }

    // USB needs top-level `use`s from the external usb-device / usbd-serial
    // crates (auto-added to Cargo.toml) — these can't live in the HAL `use {}`.
    let extra_uses = if has_usb {
        "use usb_device::prelude::*;\nuse usbd_serial::{SerialPort, USB_CLASS_CDC};\n"
    } else {
        ""
    };

    // ── Pin declaration lines ────────────────────────────────────────────────
    let mut port_groups: BTreeMap<char, Vec<String>> = BTreeMap::new();
    for (pin, meta) in &configured {
        let expr = into_expr(
            &pin.selected_function,
            pin.io_mode,
            &meta.port_var,
            meta.crx,
        );
        let comment = pin.selected_function.label();
        let line = if is_comment_expr(&expr) {
            format!(
                "    // {}: {}",
                pin.name,
                expr.trim_start_matches("//").trim()
            )
        } else {
            // GPIO In/Out binding shape follows the project's GPIO api
            // (`gpio_native`):
            //  · Portable (default) → wrap in the `pins::configs::io` bridge so
            //    the binding is a STANDARD `embedded-hal` 1.0 pin — portable to
            //    any HAL. The wrapper is transparent (`.0` gives the raw HAL pin
            //    back).
            //  · Native → bind the raw HAL pin (no io.rs, no embedded-hal dep).
            //
            // A GPIO pin is bound BY VALUE (no `&mut` on the right-hand side):
            // owning it is what lets the user move it into a driver or a struct,
            // which a `&mut` to a temporary never allowed. Peripheral pins keep
            // the old shape — they are consumed by an `init_*` call.
            let binding = binding_of(pin, meta);
            let (prefix, open, close) = match pin.selected_function {
                // RTIC: the pin is moved into a `Local`, so it must be owned.
                _ if binding_style == Binding::Owned => ("", "", ""),
                PinFunction::GpioOutput if !gpio_native => {
                    ("", "pins::configs::io::DigitalOut(", ")")
                }
                // An ARMED input is raw even on the Portable API: `ExtiPin` is
                // implemented for the HAL's pin, not for the `DigitalIn`
                // wrapper — and the wrapper would buy nothing here anyway,
                // because the pin is moved into a static and belongs to the
                // interrupt handler, never to the reader's loop.
                PinFunction::GpioInput if !gpio_native && pin.irq.is_none() => {
                    ("", "pins::configs::io::DigitalIn(", ")")
                }
                PinFunction::GpioOutput | PinFunction::GpioInput => ("", "", ""),
                ref f if needs_mut_ref(f) => ("&mut ", "", ""),
                _ => ("", "", ""),
            };
            // A freed JTAG pin is no longer a field of the port: `disable_jtag`
            // moved it out and handed it back as a plain binding.
            let src = if free_jtag && JTAG_PINS.contains(&pin.name.as_str()) {
                meta.var.clone()
            } else {
                format!("{}.{}", meta.port_var, meta.var)
            };
            // A GPIO pin — or an analog one — is declared here and used in the
            // reader's own loop, which does not exist yet in a fresh project.
            // So rustc warns about every pin they have not got to
            // (`unused_variables`, plus `unused_mut` on an output). These lines
            // live INSIDE the generated block, so that is a warning they cannot
            // answer by editing the line. The allow is scoped to the one
            // statement and goes inert the moment the pin is used.
            //
            // An analog pad belongs here for the same reason a GPIO does: the
            // ADC itself IS generated, and the pin is simply waiting for the
            // `_adc1.read(&mut …)` the block shows commented out below.
            //
            // Peripheral pins deliberately get NO allow: they are consumed by
            // an `init_*` call, so a warning on one means the bus it belongs to
            // was not generated — which is exactly what the reader should see.
            let allow = match pin.selected_function {
                PinFunction::GpioInput
                | PinFunction::GpioOutput
                | PinFunction::GpioAnalog
                | PinFunction::AdcChannel { .. } => "    #[allow(unused_mut, unused_variables)]\n",
                // NOT `TimerPwm`: `pwm_hz` consumes those pads, so one left
                // unused means its timer did not generate (mixed remap sets) —
                // the same signal an orphaned bus pad carries.
                _ => "",
            };
            format!(
                "{allow}    let {mut_}{binding} = {prefix}{open}{src}.{expr}{close}; // {comment}",
                mut_ = if needs_mut_binding(pin, gpio_native, binding_style, &binding, custom_inits)
                {
                    "mut "
                } else {
                    ""
                },
            )
        };
        port_groups.entry(meta.port).or_default().push(line);
    }

    let mut pin_section = String::new();
    for (port, lines) in &port_groups {
        pin_section.push_str(&format!("    // ── Port {port} ──\n"));
        // Blank-line separated, like every other backend: an `#[allow(…)]` sat
        // directly under the previous pin's `let` reads as one wall of text.
        pin_section.push_str(&super::common::blank_separated(
            lines.iter().map(|l| format!("{l}\n")),
        ));
        pin_section.push('\n');
    }

    // Peripheral helper functions (init_usartN, init_spiN, init_i2cN) are NOT
    // emitted here: USART/SPI/I2C init lives in `src/pins/configs/` and the ADC
    // is constructed inline below (one plain `Adc::adc1` line — no helper fn).

    // ── Peripheral init calls (inside fn main) ───────────────────────────────
    let mut fn_calls = String::new();
    let mut any_call = false;
    // See `GenParts::inline_handles`.
    let mut inline_handles: Vec<(String, String, bool)> = Vec::new();

    macro_rules! header {
        () => {
            if !any_call {
                fn_calls.push_str("    // ── Peripheral initialisation ──\n");
                any_call = true;
            }
        };
    }
    // `DMA1.split()` hands out the channel singletons the DMA inits take. Once
    // per project, before the first of them: the channels are moved out of the
    // returned struct one field at a time, so a second split would be a second
    // owner of hardware that is already spoken for.
    let mut dma1_split = false;
    macro_rules! dma1 {
        () => {
            if !dma1_split {
                fn_calls.push_str("    let dma1 = dp.DMA1.split();\n");
                dma1_split = true;
            }
        };
    }

    for n in 1u8..=3 {
        let tx = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::UsartTx(n));
        let rx = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::UsartRx(n));
        if tx.is_none() && rx.is_none() {
            continue;
        }
        header!();
        // Unlike SPI, this HAL has NO placeholder for a missing serial pad:
        // `serial::Pins<USART>` is implemented for the (TX alternate, RX input)
        // PAIR and for nothing else, so a one-way UART cannot be built at all.
        // Say that where the init would have gone — the old code emitted the
        // call anyway, naming a `_rx1` binding nothing ever declared, and the
        // project did not compile.
        let (Some(tx), Some(rx)) = (tx, rx) else {
            let missing = if tx.is_none() { "TX" } else { "RX" };
            fn_calls.push_str(&format!(
                "    // USART{n} is NOT initialised: {missing} is not wired, and stm32f1xx-hal\n    \
                 // builds a Serial only from the TX+RX pair (no one-way UART on this HAL).\n"
            ));
            continue;
        };
        let (tx_v, rx_v) = {
            let (p, m) = tx;
            let (q, o) = rx;
            (binding_of(p, m), binding_of(q, o))
        };
        let sfx = usart
            .get(&n)
            .map(|c| module_label_sfx(&c.custom_label))
            .unwrap_or_default();
        // The binding shape follows the config's return type (see `api_style`):
        //  · Native  → the split `(Tx, Rx)` handles     → `let (mut _txN, mut _rxN)`
        //  · Portable → one `embedded_io::{Read,Write}`  → `let mut _serialN`
        let native = matches!(usart.get(&n).map(|c| c.api_style), Some(ApiStyle::Native));
        // DMA also returns a PAIR, but of DMA halves rather than `nb` ones, and
        // it takes two more arguments: the channels the HAL fixes to this
        // instance (see `usart_dma_channels`).
        let dma = usart
            .get(&n)
            .map(|c| c.blocking_dma)
            .filter(|_| usart_dma_channels(n).is_some())
            .unwrap_or_default();
        // Both DMA and Native hand back a PAIR; only the types differ, and on
        // DMA they differ per half.
        let binding = if native || dma.any() {
            format!("let (mut _tx{n}{sfx}, mut _rx{n}{sfx})")
        } else {
            format!("let mut _serial{n}{sfx}")
        };
        let chans = match usart_dma_channels(n) {
            Some((tx, rx)) if dma.any() => {
                dma1!();
                // Order follows `init`'s signature: TX first, and a half that
                // is not on DMA contributes no argument at all.
                let mut a = String::new();
                if dma.tx() {
                    a.push_str(&format!(", {}", channel_field(tx)));
                }
                if dma.rx() {
                    a.push_str(&format!(", {}", channel_field(rx)));
                }
                a
            }
            _ => String::new(),
        };
        fn_calls.push_str(&format!(
            "    {binding} = \
             pins::configs::usart{n}::init(dp.USART{n}, ({tx_v}, {rx_v}), &mut afio, &clocks{chans});\n"
        ));
    }

    for n in 1u8..=2 {
        let sck = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::SpiSck(n));
        let miso = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::SpiMiso(n));
        let mosi = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::SpiMosi(n));
        if sck.is_none() && mosi.is_none() {
            continue;
        }
        header!();
        // A signal the user left unwired gets the HAL's own placeholder — the
        // same type `SpiPins` in the config module names for it, and a value
        // that exists (the old `_miso{n}` was a binding nothing ever declared).
        let sck_v = sck
            .map(|(p, m)| binding_of(p, m))
            .unwrap_or_else(|| SPI_NO_SCK.to_string());
        let miso_v = miso
            .map(|(p, m)| binding_of(p, m))
            .unwrap_or_else(|| SPI_NO_MISO.to_string());
        let mosi_v = mosi
            .map(|(p, m)| binding_of(p, m))
            .unwrap_or_else(|| SPI_NO_MOSI.to_string());
        let sfx = spi
            .get(&n)
            .map(|c| module_label_sfx(&c.custom_label))
            .unwrap_or_default();
        let spi_dma = spi
            .get(&n)
            .map(|c| c.blocking_dma)
            .filter(|_| spi_dma_channels(n).is_some())
            // No MISO, no receive half — see `BlockingDma::without_rx`.
            .map(|d| if miso.is_some() { d } else { d.without_rx() })
            .unwrap_or_default();
        let chans = match spi_dma_channels(n) {
            Some((rx, tx)) if spi_dma.any() => {
                dma1!();
                // RX first here, matching `with_rx_tx_dma`'s own order.
                let mut a = String::new();
                if spi_dma.rx() {
                    a.push_str(&format!(", {}", channel_field(rx)));
                }
                if spi_dma.tx() {
                    a.push_str(&format!(", {}", channel_field(tx)));
                }
                a
            }
            _ => String::new(),
        };
        // A DMA handle is CONSUMED by each transfer and handed back from
        // `wait()`, so it has to be rebindable — the file's own example ends
        // `_spi1 = spi;`. The polled shapes keep the plain binding, and their
        // templates say to add `mut` if you call anything.
        let binding = if spi_dma.any() {
            format!("let mut _spi{n}{sfx}")
        } else {
            format!("let _spi{n}{sfx}")
        };
        fn_calls.push_str(&format!(
            "    {binding} = \
             pins::configs::spi{n}::init(dp.SPI{n}, ({sck_v}, {miso_v}, {mosi_v}), &mut afio, &clocks{chans});\n"
        ));
    }

    for n in 1u8..=2 {
        let scl = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::I2cScl(n));
        let sda = configured
            .iter()
            .find(|(p, _)| p.selected_function == PinFunction::I2cSda(n));
        if scl.is_none() && sda.is_none() {
            continue;
        }
        header!();
        // Both wires or nothing — `i2c::Pins<I2C1>` is implemented for the
        // (SCL, SDA) pair (PB6/PB7, or PB8/PB9 remapped) and for no single pad.
        // This arm always DID refuse to build half a bus; what it did not do was
        // say so, leaving a configured pad and no peripheral to explain it.
        let (Some(scl), Some(sda)) = (scl, sda) else {
            let missing = if scl.is_none() { "SCL" } else { "SDA" };
            fn_calls.push_str(&format!(
                "    // I2C{n} is NOT initialised: {missing} is not wired, and an I2C bus needs\n    \
                 // both wires (stm32f1xx-hal takes the SCL+SDA pair, and there is no one-wire I2C).\n"
            ));
            continue;
        };
        let scl_v = {
            let (p, m) = scl;
            binding_of(p, m)
        };
        let sda_v = {
            let (p, m) = sda;
            binding_of(p, m)
        };
        let sfx = i2c
            .get(&n)
            .map(|c| module_label_sfx(&c.custom_label))
            .unwrap_or_default();
        fn_calls.push_str(&format!(
            "    let _i2c{n}{sfx} = \
             pins::configs::i2c{n}::init(dp.I2C{n}, ({scl_v}, {sda_v}), &mut afio, &clocks);\n"
        ));
    }

    if has_adc {
        header!();
        // Plain one-liner — `Adc::adc1` takes `Clocks` BY VALUE (Clocks is Copy).
        fn_calls.push_str("    let mut _adc1 = adc::Adc::adc1(dp.ADC1, clocks);\n");
        // The ADC is only MOVED in init - the read the block shows is the
        // reader's to write, so no `mut` survives the promotion.
        inline_handles.push(("_adc1".into(), "adc::Adc<pac::ADC1>".into(), false));
        for (p, meta) in configured
            .iter()
            .filter(|(p, _)| matches!(p.selected_function, PinFunction::AdcChannel { .. }))
        {
            fn_calls.push_str(&format!(
                "    // let val: u16 = _adc1.read(&mut {}).unwrap();\n",
                binding_of(p, meta)
            ));
        }
    }

    if has_timer {
        header!();
        fn_calls.push_str("    // ── Timers / PWM ──\n");
        // One block per timer actually wired, and now ONE LINE per block: the
        // frequency, the duty and the remap all moved into
        // `pins/configs/pwm{N}.rs`, next to the consts that drive them. What
        // stays here is what has to stay here — the pins, which are the record
        // the reopened project is rebuilt from.
        let mut timers: BTreeMap<u8, Vec<(u8, String)>> = BTreeMap::new();
        for (p, meta) in &configured {
            if let PinFunction::TimerPwm { timer, channel } = p.selected_function {
                timers
                    .entry(timer)
                    .or_default()
                    .push((channel, binding_of(p, meta)));
            }
        }
        for (tim, mut chans) in timers {
            chans.sort_by_key(|(c, _)| *c);
            let pads: Vec<(u8, &str)> = configured
                .iter()
                .filter_map(|(p, _)| match p.selected_function {
                    PinFunction::TimerPwm { timer, channel } if timer == tim => {
                        Some((channel, p.name.as_str()))
                    }
                    _ => None,
                })
                .collect();
            let list = chans
                .iter()
                .map(|(c, _)| format!("CH{c}"))
                .collect::<Vec<_>>()
                .join("+");
            if pwm_remap(tim, &pads).is_none() {
                // Nothing generated rather than something wrong — and the pads
                // stay bound and unused, so the compiler names them too.
                fn_calls.push_str(&format!(
                    "    // TIM{tim} {list} is NOT initialised: these pads are not one of the\n    \
                     // remap sets stm32f1xx-hal implements for this timer, so `pwm_hz` has no\n    \
                     // type-state to take. Move the channels onto one set on the canvas.\n"
                ));
                continue;
            }
            // A single channel is NOT a 1-tuple: the HAL's `Pins` impl for one
            // pin is `(P1)`, i.e. the pin itself.
            let pins_expr = if chans.len() == 1 {
                chans[0].1.clone()
            } else {
                format!(
                    "({})",
                    chans
                        .iter()
                        .map(|(_, b)| b.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            let sfx = timer
                .get(&tim)
                .map(|c| module_label_sfx(&c.custom_label))
                .unwrap_or_default();
            fn_calls.push_str(&format!(
                "    let mut _pwm{tim}{sfx} = \
                 pins::configs::pwm{tim}::init(dp.TIM{tim}, {pins_expr}, &mut afio, &clocks);\n"
            ));
        }
    }

    let can_rx = configured
        .iter()
        .find(|(p, _)| p.selected_function == PinFunction::CanRx);
    let can_tx_pin = configured
        .iter()
        .find(|(p, _)| p.selected_function == PinFunction::CanTx);
    if can_rx.is_some() || can_tx_pin.is_some() {
        header!();
        // The third pair on this family: `can::Pins` is implemented for
        // (PA12 alternate, PA11 input) and for PB9/PB8 remapped — never for one
        // pad. The old code filled the gap with `_can_rx` / `_can_tx`, bindings
        // nothing ever declared, and the project did not compile.
        if let (Some((tp, tm)), Some((rp, rm))) = (can_tx_pin, can_rx) {
            let (tx_v, rx_v) = (binding_of(tp, tm), binding_of(rp, rm));
            let sfx = can
                .get(&1)
                .map(|c| module_label_sfx(&c.custom_label))
                .unwrap_or_default();
            // `assign_pins` (and bxcan) expect the pins as `(TX, RX)`. `dp.USB`
            // goes with them: bxCAN and USB share SRAM, so the HAL takes the USB
            // token to prove it is free — see the config module's own note.
            fn_calls.push_str(&format!(
                "    let mut _can{sfx} = \
                 pins::configs::can1::init(dp.CAN1, dp.USB, ({tx_v}, {rx_v}), &mut afio);\n"
            ));
        } else {
            let missing = if can_tx_pin.is_none() { "TX" } else { "RX" };
            fn_calls.push_str(&format!(
                "    // CAN1 is NOT initialised: {missing} is not wired, and stm32f1xx-hal\n    \
                 // assigns the CAN pads as a TX+RX pair (there is no one-way CAN).\n"
            ));
        }
    }

    // ── USB (CDC ACM serial) — full init in main's scope (option A) ───────────
    // USB needs `usb_dev`/`serial` to live where the user's loop polls them, and
    // its bus allocator borrows them, so it can't go in a `pins::configs` init().
    // Pins PA11(D-)/PA12(D+) are emitted as comments (not generic bindings), so
    // this block owns their configuration.
    if has_usb {
        header!();
        let cfg = usb.get(&1);
        let vid = cfg.map(|c| c.vid).unwrap_or(0x16c0);
        let pid = cfg.map(|c| c.pid).unwrap_or(0x27dd);
        let product = cfg.map(|c| c.product.as_str()).unwrap_or("Serial port");
        let sfx = cfg
            .map(|c| module_label_sfx(&c.custom_label))
            .unwrap_or_default();
        fn_calls.push_str(&format!(
            "    // ── USB (CDC ACM serial) ──\n\
             // Needs the `usb-device` + `usbd-serial` crates and the HAL `stm32-usbd`\n\
             // feature (auto-added to Cargo.toml). Poll in your loop:\n\
             //     if usb_dev{sfx}.poll(&mut [&mut serial{sfx}]) {{ /* read/write serial */ }}\n\
             // Pull D+ low briefly so the host re-enumerates after a reset.\n\
             let mut usb_dp = gpioa.pa12.into_push_pull_output(&mut gpioa.crh);\n\
             usb_dp.set_low();\n\
             cortex_m::asm::delay(clocks.sysclk().raw() / 100);\n\
             let usb_periph = Peripheral {{\n\
                 usb: dp.USB,\n\
                 pin_dm: gpioa.pa11,\n\
                 pin_dp: usb_dp.into_floating_input(&mut gpioa.crh),\n\
             }};\n\
             let usb_bus = UsbBus::new(usb_periph);\n\
             // These two are the device: they must be `mut` for `poll`, and they\n\
             // stay unused until you write that poll into your loop. The `allow`\n\
             // keeps a fresh project warning-free; it goes inert the moment you\n\
             // do, and it cannot hide anything beyond its own statement.\n\
             #[allow(unused_mut, unused_variables)]\n\
             let mut serial{sfx} = SerialPort::new(&usb_bus);\n\
             #[allow(unused_mut, unused_variables)]\n\
             let mut usb_dev{sfx} = UsbDeviceBuilder::new(&usb_bus, UsbVidPid(0x{vid:04x}, 0x{pid:04x}))\n\
                 .product(\"{product}\")\n\
                 .device_class(USB_CLASS_CDC)\n\
                 .build();\n"
        ));
    } else if usb_half {
        header!();
        let missing = if usb_dm { "D+ (PA12)" } else { "D- (PA11)" };
        fn_calls.push_str(&format!(
            "    // USB is NOT initialised: {missing} is not wired. A USB device needs both\n    \
             // data pads, and this init takes PA11/PA12 directly - generating it from one\n    \
             // would spend the other pad without being asked.\n"
        ));
    }

    if any_call {
        fn_calls.push('\n');
    }

    // ── The HAL `use {}` block ───────────────────────────────────────────────
    // No timer items to add: PWM names `Timer`, `Channel` and its remap inside
    // its own config module now, so main.rs imports none of them.
    let use_block = use_items
        .iter()
        .map(|s| format!("    {s},"))
        .collect::<Vec<_>>()
        .join("\n");

    // ── Custom modules ───────────────────────────────────────────────────────
    // Last, so every pin binding and peripheral init they consume already
    // exists above.
    if !custom_inits.is_empty() {
        fn_calls.push_str("    // ── Custom modules ──\n");
        fn_calls.push_str(custom_inits);
        fn_calls.push('\n');
    }

    // ── Assemble port splits ─────────────────────────────────────────────────
    let mut port_splits = ports_used
        .iter()
        .map(|p| {
            let lc = p.to_ascii_lowercase();
            let mut_ = if ports_written.contains(p) {
                "mut "
            } else {
                ""
            };
            format!("    let {mut_}gpio{lc} = dp.GPIO{p}.split();")
        })
        .collect::<Vec<_>>()
        .join("\n");
    if free_jtag {
        port_splits.push_str(&jtag_release_line(&configured));
    }

    // ── GPIO interrupts ──────────────────────────────────────────────────────
    // Bare-metal only: the RTIC path turns the same pins into hardware TASKS,
    // and emitting both would arm every line twice.
    let (irq_items, irq_arming) = if binding_style == Binding::Owned {
        (String::new(), String::new())
    } else {
        (f1_irq_items(all_pins), f1_irq_arming(all_pins))
    };
    if !irq_arming.is_empty() {
        fn_calls.push('\n');
        fn_calls.push_str(&irq_arming);
    }

    // `make_interrupt_source` needs the AFIO — the interrupt source multiplexer
    // lives there — so an armed pin pulls it in even when no bus does.
    let afio_line = if needs_afio || !irq_items.is_empty() {
        "let mut afio = dp.AFIO.constrain();\n"
    } else {
        ""
    };

    // ── Clock setup chain (from the Clock tab config) ────────────────────────
    let clock_chain = clock_setup_chain(clock);

    // Peripheral config constants now live in the per-peripheral modules under
    // `src/pins/configs/` (e.g. `pins::configs::usart1`), seeded from the wired
    // Virtual Module — no longer emitted in main.rs.

    Some(GenParts {
        use_block,
        extra_uses,
        afio_line,
        clock_chain,
        port_splits,
        pin_section,
        fn_calls,
        inline_handles,
        irq_items,
    })
}

/// The classic bare-metal `main.rs` GEN block: `#[entry] fn main() -> !` with
/// the whole init inlined.
///
/// Byte-for-byte what it always produced — the parts moved out, the arrangement
/// did not (guarded by `blocking_output_is_unchanged`).
#[allow(clippy::too_many_arguments)]
pub fn make_generated_section(
    mcu_name: &str,
    all_pins: &[&Pin],
    clock: &ClockConfig,
    usart: &BTreeMap<u8, UsartModuleConfig>,
    spi: &BTreeMap<u8, SpiModuleConfig>,
    i2c: &BTreeMap<u8, I2cModuleConfig>,
    can: &BTreeMap<u8, CanModuleConfig>,
    usb: &BTreeMap<u8, UsbModuleConfig>,
    timer: &BTreeMap<u8, TimerModuleConfig>,
    gpio_native: bool,
    custom_inits: &str,
) -> String {
    let Some(parts) = gen_parts(
        Binding::MutRef,
        all_pins,
        clock,
        usart,
        spi,
        i2c,
        can,
        usb,
        timer,
        gpio_native,
        custom_inits,
    ) else {
        return make_default_gen_section(mcu_name, clock);
    };
    let GenParts {
        // `inline_handles` is an RTIC concern: `fn main` never returns here, so
        // a peripheral it keeps in scope simply lives.
        inline_handles: _,
        use_block,
        extra_uses,
        afio_line,
        clock_chain,
        port_splits,
        pin_section,
        fn_calls,
        irq_items,
    } = parts;
    // `gpio::{self, Edge, ExtiPin}` only when a pin is armed: `ExtiPin` is NOT
    // in the HAL's prelude, and an unconditional import would warn on every
    // project without an interrupt.
    let use_block = if irq_items.is_empty() {
        use_block
    } else {
        format!("{use_block}\n    gpio::{{self, Edge, ExtiPin}},")
    };
    // `&mut dp.EXTI` — `trigger_on_edge` and `enable_interrupt` both take the
    // controller mutably. Only when a pin is armed: an unused `mut` there would
    // warn on a line inside the generated block, which the reader cannot edit.
    let dp_mut = if irq_items.is_empty() { "" } else { "mut " };
    // Nothing is appended after `fn main` any more: USART/SPI/I2C init live in
    // `src/pins/configs/` and the ADC is a single line inside the GEN block.
    format!(
        "{GEN_BEGIN}\n\
         use stm32f1xx_hal::{{\n\
         {use_block}\n\
         }};\n\
         {extra_uses}\
         \n\
         {irq_items}\
         #[entry]\n\
         fn main() -> ! {{\n\
             let {dp_mut}dp = pac::Peripherals::take().unwrap();\n\n\
             let mut flash = dp.FLASH.constrain();\n\
             let rcc = dp.RCC.constrain();\n\
         {afio_line}\
             let clocks = {clock_chain};\n\n\
         {port_splits}\n\n\
         {pin_section}\
         {fn_calls}\
         {GEN_END}\n"
    )
}

// ── Default generated section (no pins configured yet) ────────────────────────

fn make_default_gen_section(mcu_name: &str, clock: &ClockConfig) -> String {
    let clock_chain = clock_setup_chain(clock);
    format!(
        "{GEN_BEGIN}\n\
         // MCU: {mcu_name}\n\
         use stm32f1xx_hal::{{pac, prelude::*}};\n\n\
         #[entry]\n\
         fn main() -> ! {{\n\
             let dp = pac::Peripherals::take().unwrap();\n\n\
             let mut flash = dp.FLASH.constrain();\n\
             let rcc = dp.RCC.constrain();\n\
             let clocks = {clock_chain};\n\n\
             // Select pins in the MCU Configurator to generate code here.\n\
         {GEN_END}\n"
    )
}

// ── Per-peripheral config files (src/pins/configs/) ───────────────────────────

/// The body of each `src/pins/configs/<periph>.rs` init module: config
/// constants (seeded from the wired Virtual Module) + a `use` block + a config
/// helper + `pub fn init`. Returns `(file_name, generated_body)` per configured
/// USART/SPI/I2C. `project_tree::sync_config_files` wraps the body in GENERATED
/// markers and splices it (so user code outside the markers survives). main.rs
/// calls `pins::configs::<periph>::init(...)`.
pub fn config_files(
    all_pins: &[&Pin],
    usart: &BTreeMap<u8, UsartModuleConfig>,
    spi: &BTreeMap<u8, SpiModuleConfig>,
    i2c: &BTreeMap<u8, I2cModuleConfig>,
    can: &BTreeMap<u8, CanModuleConfig>,
    timer: &BTreeMap<u8, TimerModuleConfig>,
    clock: &ClockConfig,
    gpio_native: bool,
) -> Vec<(String, String)> {
    let funcs: Vec<&PinFunction> = all_pins
        .iter()
        .filter(|p| !p.reserved && p.selected_function != PinFunction::Unset)
        .map(|p| &p.selected_function)
        .collect();
    let has = |want: PinFunction| funcs.iter().any(|f| **f == want);

    let mut out: Vec<(String, String)> = Vec::new();
    for n in 1u8..=3 {
        // BOTH pads, like I2C and unlike SPI: `Serial::new` takes the pair, so
        // half a USART has no `init` to offer. main.rs says why in its place.
        if has(PinFunction::UsartTx(n)) && has(PinFunction::UsartRx(n)) {
            out.push((format!("usart{n}.rs"), usart_config_file(n, usart.get(&n))));
        }
    }
    for n in 1u8..=2 {
        if has(PinFunction::SpiSck(n)) || has(PinFunction::SpiMosi(n)) {
            out.push((
                format!("spi{n}.rs"),
                spi_config_file(
                    n,
                    spi.get(&n),
                    &spi_pin_tys(n, all_pins),
                    has(PinFunction::SpiMiso(n)),
                ),
            ));
        }
    }
    for n in 1u8..=2 {
        if has(PinFunction::I2cScl(n)) && has(PinFunction::I2cSda(n)) {
            out.push((
                format!("i2c{n}.rs"),
                i2c_config_file(n, i2c.get(&n), &i2c_pin_tys(n, all_pins)),
            ));
            out.extend(super::common::i2c_device_config_files(
                &format!("i2c{n}"),
                i2c.get(&n),
            ));
        }
    }
    // CAN — single instance on STM32F1; the bit-timing register depends on the
    // APB1 (PCLK1) clock, so the clock config is read here. Both pads, like the
    // USART and the I2C: `can::Pins` is a pair, so half a CAN has no `init`.
    if has(PinFunction::CanRx) && has(PinFunction::CanTx) {
        out.push((
            "can1.rs".to_string(),
            can_config_file(can.get(&1), pclk1_of(clock)),
        ));
    }
    // PWM — one file per TIMER, not per channel: the frequency is the timer's
    // and the duties hang off it. A wiring `pwm_hz` has no type-state for gets
    // no file at all; main.rs says why in its place, the same as half a USART.
    for tim in pwm_timers(all_pins) {
        let chans = pwm_chans(all_pins, tim);
        let pads: Vec<(u8, &str)> = chans.iter().map(|(c, p)| (*c, p.name.as_str())).collect();
        if let Some(remap) = pwm_remap(tim, &pads) {
            out.push((
                format!("pwm{tim}.rs"),
                pwm_config_file(tim, timer.get(&tim), &chans, remap),
            ));
        }
    }
    // The GPIO/Delay → embedded-hal 1.0 bridge module — ONLY on the Portable
    // GPIO api (`!gpio_native`), when any GPIO in/out pin is configured (the pin
    // bindings wrap it). On the Native GPIO api the pins bind raw, so io.rs would
    // be dead code + pull an unused `embedded-hal` — skip it.
    if !gpio_native && (has(PinFunction::GpioOutput) || has(PinFunction::GpioInput)) {
        out.push(("io.rs".to_string(), GPIO_IO_FILE.to_string()));
    }
    out
}

/// The pins the JTAG port holds at reset. `disable_jtag` frees exactly these
/// three, together — the order is the one its signature takes.
const JTAG_PINS: [&str; 3] = ["PA15", "PB3", "PB4"];

/// The `let (…) = afio.mapr.disable_jtag(…)` line, with the pins the project
/// does not use underscored so they don't read as forgotten bindings.
fn jtag_release_line(configured: &[(&Pin, PinMeta)]) -> String {
    let names = JTAG_PINS
        .iter()
        .map(|n| {
            let var = n.to_ascii_lowercase();
            if configured.iter().any(|(p, _)| p.name == *n) {
                var
            } else {
                format!("_{var}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\n    // PA15/PB3/PB4 are the JTAG port at reset and have no usable pin\n\
         \x20   // mode until it is given up. SWD keeps working; JTAG debugging does not.\n\
         \x20   let ({names}) = afio.mapr.disable_jtag(gpioa.pa15, gpiob.pb3, gpiob.pb4);"
    )
}

// ── Concrete pin types for the bus config modules ─────────────────────────────
//
// SPI and I2C keep their pins IN the handle type (`Spi<SPI1, REMAP, PINS, u8>`,
// `BlockingI2c<I2C1, PINS>`), so a config module cannot name what `init` returns
// without naming the pins. USART could get away with `Handle` alone — its
// `split()` erases them — these two cannot, and a named handle is what an RTIC
// `Local` resource (or any struct field) needs.
//
// The aliases therefore live INSIDE each file's GENERATED block: re-wiring a
// peripheral must rewrite them, and only that block is re-spliced on save.

/// The HAL placeholders for an unwired SPI signal. main.rs passes the VALUE (by
/// full path — its `use` block does not import `spi`); the config module's
/// `SpiPins` names the TYPE through the `hal_spi` alias it imports itself.
const SPI_NO_SCK: &str = "stm32f1xx_hal::spi::NoSck";
const SPI_NO_MISO: &str = "stm32f1xx_hal::spi::NoMiso";
const SPI_NO_MOSI: &str = "stm32f1xx_hal::spi::NoMosi";

/// The pin carrying `want`, or `None` when the user has not wired that signal.
fn wired<'a>(all_pins: &[&'a Pin], want: PinFunction) -> Option<&'a Pin> {
    all_pins
        .iter()
        .copied()
        .find(|p| !p.reserved && p.selected_function == want)
}

/// The `stm32f1xx-hal` type a configured bus pin ends up with — the value
/// [`into_expr`] builds, named. `hal_gpio` is the alias the GENERATED block of
/// each config file imports for itself, so the type keeps resolving whatever the
/// user does to the editable `use` block below it.
fn hal_pin_ty(pin: &Pin) -> Option<String> {
    let mode = match pin.selected_function {
        // `into_alternate_push_pull` → `Alternate<PushPull>`, and `PushPull` is
        // the alias' default parameter.
        PinFunction::SpiSck(_) | PinFunction::SpiMosi(_) => "hal_gpio::Alternate",
        PinFunction::SpiMiso(_) => "hal_gpio::Input<hal_gpio::Floating>",
        PinFunction::I2cScl(_) | PinFunction::I2cSda(_) => {
            "hal_gpio::Alternate<hal_gpio::OpenDrain>"
        }
        _ => return None,
    };
    let m = parse_pin(&pin.name)?;
    Some(format!("hal_gpio::P{}{}<{mode}>", m.port, m.pin_num))
}

/// One line per element of a generated tuple type, indented for the alias.
fn tuple_body(items: &[String]) -> String {
    items
        .iter()
        .map(|t| format!("\n    {t},"))
        .collect::<String>()
        + "\n"
}

/// `(SpiPins tuple body, SpiRemap type)` for SPI`n`, as its config module
/// spells them.
fn spi_pin_tys(n: u8, all_pins: &[&Pin]) -> (String, String) {
    let ty = |f: PinFunction, filler: &str| {
        wired(all_pins, f)
            .and_then(hal_pin_ty)
            // Same placeholder main.rs passes for the missing signal, with the
            // full path swapped for the file's own `hal_spi` alias.
            .unwrap_or_else(|| filler.replace("stm32f1xx_hal::spi::", "hal_spi::"))
    };
    let pins = tuple_body(&[
        ty(PinFunction::SpiSck(n), SPI_NO_SCK),
        ty(PinFunction::SpiMiso(n), SPI_NO_MISO),
        ty(PinFunction::SpiMosi(n), SPI_NO_MOSI),
    ]);
    (pins, spi_remap_ty(n, all_pins))
}

/// The remap type-state for SPI`n`. Only SPI1 has an alternate pin set on this
/// family (PB3/PB4/PB5 instead of PA5/PA6/PA7) and the HAL takes the remap
/// REGISTER BIT from this type, so naming the wrong one drives the wrong pads.
/// SPI2's own pins are on port B as well — hence the instance check.
///
/// The clock pin decides, falling back to MOSI then MISO: the HAL's remap is
/// all-or-nothing, so a wiring that mixes the two sets has no right answer here
/// — it is rejected by the `Pins` impls, at the pin that does not belong.
fn spi_remap_ty(n: u8, all_pins: &[&Pin]) -> String {
    let remapped = n == 1
        && [
            PinFunction::SpiSck(n),
            PinFunction::SpiMosi(n),
            PinFunction::SpiMiso(n),
        ]
        .into_iter()
        .find_map(|f| wired(all_pins, f).and_then(|p| parse_pin(&p.name)))
        .is_some_and(|m| m.port == 'B');
    if remapped {
        "hal_spi::Spi1Remap".to_string()
    } else {
        format!("hal_spi::Spi{n}NoRemap")
    }
}

/// The `I2cPins` tuple body for I2C`n` — (SCL, SDA), the order the HAL's `Pins`
/// impls and main.rs's call both use. Both signals are required for the file to
/// be generated at all, so there is no placeholder case.
fn i2c_pin_tys(n: u8, all_pins: &[&Pin]) -> String {
    let ty = |f: PinFunction| {
        wired(all_pins, f)
            .and_then(hal_pin_ty)
            .unwrap_or_else(|| "()".to_string())
    };
    tuple_body(&[ty(PinFunction::I2cScl(n)), ty(PinFunction::I2cSda(n))])
}

/// `src/pins/configs/io.rs` — bridges the HAL's embedded-hal 0.2 GPIO + blocking
/// delay to the STANDARD `embedded-hal` 1.0 traits (`OutputPin`/`InputPin`/
/// `DelayNs`), so driver/app code stays portable across HALs. Real-compile
/// verified on thumbv7m. The leading GENERATED marker block is required by
/// `sync_config_files` (there is no per-chip config here — it stays a comment).
const GPIO_IO_FILE: &str = r#"// <<< GENERATED>>>
// Portable GPIO/Delay bridges — no per-chip config; this marker just frames the
// regenerated region. Edit the bridges below freely.
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// Wrap a HAL pin/delay to make it a STANDARD `embedded-hal` 1.0 value, so your
// driver/app code is portable across chips/HALs:
//
//     fn app<P: embedded_hal::digital::OutputPin>(led: &mut P) { /* … */ }
#[derive(Debug)]
pub struct IoError;
impl embedded_hal::digital::Error for IoError {
    fn kind(&self) -> embedded_hal::digital::ErrorKind {
        embedded_hal::digital::ErrorKind::Other
    }
}

/// A HAL output pin (embedded-hal 0.2) as an `embedded-hal` 1.0 pin.
pub struct DigitalOut<P>(pub P);
impl<P> embedded_hal::digital::ErrorType for DigitalOut<P> {
    type Error = IoError;
}
impl<P: embedded_hal_0_2::digital::v2::OutputPin> embedded_hal::digital::OutputPin for DigitalOut<P> {
    fn set_low(&mut self) -> Result<(), Self::Error> { self.0.set_low().map_err(|_| IoError) }
    fn set_high(&mut self) -> Result<(), Self::Error> { self.0.set_high().map_err(|_| IoError) }
}
impl<P> embedded_hal::digital::StatefulOutputPin for DigitalOut<P>
where
    P: embedded_hal_0_2::digital::v2::OutputPin + embedded_hal_0_2::digital::v2::StatefulOutputPin,
{
    fn is_set_high(&mut self) -> Result<bool, Self::Error> { self.0.is_set_high().map_err(|_| IoError) }
    fn is_set_low(&mut self) -> Result<bool, Self::Error> { self.0.is_set_low().map_err(|_| IoError) }
}

/// A HAL input pin (embedded-hal 0.2) as an `embedded-hal` 1.0 pin.
pub struct DigitalIn<P>(pub P);
impl<P> embedded_hal::digital::ErrorType for DigitalIn<P> {
    type Error = IoError;
}
impl<P: embedded_hal_0_2::digital::v2::InputPin> embedded_hal::digital::InputPin for DigitalIn<P> {
    fn is_high(&mut self) -> Result<bool, Self::Error> { self.0.is_high().map_err(|_| IoError) }
    fn is_low(&mut self) -> Result<bool, Self::Error> { self.0.is_low().map_err(|_| IoError) }
}

/// A HAL blocking delay (embedded-hal 0.2) as an `embedded-hal` 1.0 `DelayNs`.
/// Build one from the SysTick: `let mut d = pins::configs::io::Delay(cp.SYST.delay(&clocks));`
#[allow(dead_code)]
pub struct Delay<D>(pub D);
impl<D: embedded_hal_0_2::blocking::delay::DelayUs<u32>> embedded_hal::delay::DelayNs for Delay<D> {
    fn delay_ns(&mut self, ns: u32) { self.0.delay_us(ns.div_ceil(1000)); }
    fn delay_us(&mut self, us: u32) { self.0.delay_us(us); }
}
"#;

/// APB1 (PCLK1) frequency in Hz from the clock config — needed for the CAN bit
/// timing. Mirrors [`clock_setup_chain`]'s graph→typed→frequencies path.
fn pclk1_of(clock: &ClockConfig) -> u32 {
    let c: Stm32f1Clock = match clock {
        ClockConfig::Graph(gc) => {
            crate::panels::mcu_module::clock::graph::graph_to_stm32f1(&gc.for_codegen())
        }
        ClockConfig::None => Stm32f1Clock::default(),
    };
    frequencies(&c).pclk1
}

/// STM32 `CAN_BTR` register value for `bitrate` at `pclk1`, aiming for a ~87.5%
/// sample point. Searches total time-quanta 8..=20 for an exact prescaler.
/// Returns 0 when none fits (so the user notices and fixes the bit timing).
fn can_btr(bitrate: u32, pclk1: u32) -> u32 {
    if bitrate == 0 || pclk1 == 0 {
        return 0;
    }
    for ntq in (8u32..=20).rev() {
        let denom = bitrate * ntq;
        if pclk1 % denom != 0 {
            continue;
        }
        let brp = pclk1 / denom;
        if !(1..=1024).contains(&brp) {
            continue;
        }
        // total = 1 (sync) + ts1 + ts2 = ntq; sample point ≈ ts1/ntq.
        let ts1 = ((ntq * 7) / 8).clamp(2, 16);
        let ts2 = (ntq - 1).saturating_sub(ts1);
        if !(1..=8).contains(&ts2) {
            continue;
        }
        let sjw = 1u32;
        return ((sjw - 1) << 24) | ((ts2 - 1) << 20) | ((ts1 - 1) << 16) | (brp - 1);
    }
    0
}

const CAN_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
// Kept for the reader (and rewritten when you change the bit rate in the
// module): `init` programs BTR, which is what the hardware actually takes.
#[allow(dead_code)]
pub const BITRATE: u32 = {BITRATE}; // bits/s
// CAN_BTR register value, computed from BITRATE and the APB1 clock ({PCLK1} Hz).
pub const BTR: u32 = {BTR};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
// CAN on STM32F1 needs the `bxcan` + `nb` crates (auto-added to Cargo.toml when a
// CAN module is present). Verify the bit timing / filters for your bus.
use stm32f1xx_hal::{
    pac,
    afio,
    can::Can,
};

/// The concrete type [`init`] hands back.
///
/// CAN already returned a named type; the alias exists so every config module
/// spells its handle the same way, which is what the RTIC generator looks for.
pub type Handle = bxcan::Can<Can<pac::CAN1>>;

/// `usb` is not a typo: bxCAN and USB share the same 512 bytes of SRAM on this
/// family, so `Can::new` takes the USB peripheral to prove nothing else holds
/// it. The two cannot be used together, and on these pads (PA11/PA12) they
/// cannot even be wired together.
pub fn init<PINS>(
    can: pac::CAN1,
    usb: pac::USB,
    pins: PINS,
    afio: &mut afio::Parts,
) -> Handle
where
    PINS: stm32f1xx_hal::can::Pins<Instance = pac::CAN1>,
{
    let hal_can = Can::new(can, usb);
    hal_can.assign_pins(pins, &mut afio.mapr);

    let mut bx = bxcan::Can::builder(hal_can)
        .set_bit_timing(BTR)
        .leave_disabled();

    // Accept every frame by default — tighten the filter for your application.
    // The FIFO here is the one `receive()` reads below.
    bx.modify_filters()
        .enable_bank(0, bxcan::Fifo::Fifo0, bxcan::filter::Mask32::accept_all());

    nb::block!(bx.enable_non_blocking()).ok();
    bx
}

// ── Using CAN1 ──
// In main.rs, after the init above:
//
//     use bxcan::{Frame, StandardId};
//
//     // Send
//     let id = StandardId::new(0x123).unwrap();
//     let frame = Frame::new_data(id, [1u8, 2, 3, 4]);
//     nb::block!({HANDLE}.transmit(&frame)).ok();
//
//     // Receive (nb: Err(WouldBlock) while the mailboxes are empty)
//     if let Ok(rx) = {HANDLE}.receive() {
//         if let Some(data) = rx.data() {
//             let _payload = &data[..]; // the frame's bytes
//         }
//     }

"#;

fn can_config_file(cfg: Option<&CanModuleConfig>, pclk1: u32) -> String {
    let bitrate = cfg.map(|c| c.bitrate).unwrap_or(500_000);
    let btr = can_btr(bitrate, pclk1);
    let sfx = cfg
        .map(|c| module_label_sfx(&c.custom_label))
        .unwrap_or_default();
    CAN_TMPL
        .replace("{HANDLE}", &format!("_can{sfx}"))
        .replace("{BITRATE}", &bitrate.to_string())
        .replace("{PCLK1}", &pclk1.to_string())
        .replace("{BTR}", &format!("0x{btr:08X}"))
}

const USART_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
//
// `init` returns a value implementing the STANDARD `embedded-io` traits
// (`Read` + `Write`), so your application code stays portable across chips/HALs:
//
//     fn app<S: embedded_io::Read + embedded_io::Write>(serial: &mut S) { /* … */ }
//
// stm32f1xx-hal 0.10 still speaks embedded-hal 0.2 (`nb`), so `SerialIo` below
// bridges its split `Tx`/`Rx` to `embedded-io`.
use stm32f1xx_hal::{
    pac,
    prelude::*,       // brings the embedded-hal 0.2 `nb` serial methods into scope
    afio,
    rcc::Clocks,
    serial::{self, Config, Serial, StopBits},
};

fn get_config() -> serial::Config {
    let mut config = Config::default().baudrate(BAUDRATE.bps());

    if DATA_BITS == 8 {
        config = config.wordlength_8bits();
    } else if DATA_BITS == 9 {
        config = config.wordlength_9bits();
    }

    if PARITY == 'N' {
        config = config.parity_none();
    } else if PARITY == 'O' {
        config = config.parity_odd();
    } else if PARITY == 'E' {
        config = config.parity_even();
    }

    if STOP_BITS == 1 {
        config = config.stopbits(StopBits::STOP1);
    } else if STOP_BITS == 2 {
        config = config.stopbits(StopBits::STOP2);
    }

    config
}

/// Bridges the HAL's `nb` serial (embedded-hal 0.2) to blocking `embedded-io`.
pub struct SerialIo<TX, RX>(pub TX, pub RX);

#[derive(Debug)]
pub struct IoError;
// embedded-io 0.7 requires `Error: core::error::Error`, which in turn wants
// Display. Two tiny impls, and the bridge works on 0.6 and 0.7 alike.
impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("serial I/O error")
    }
}
impl core::error::Error for IoError {}
impl embedded_io::Error for IoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        embedded_io::ErrorKind::Other
    }
}
impl<TX, RX> embedded_io::ErrorType for SerialIo<TX, RX> {
    type Error = IoError;
}
impl<TX: embedded_hal_0_2::serial::Write<u8>, RX> embedded_io::Write for SerialIo<TX, RX> {
    fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        for &b in buf {
            nb::block!(self.0.write(b)).map_err(|_| IoError)?;
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        nb::block!(self.0.flush()).map_err(|_| IoError)
    }
}
impl<TX, RX: embedded_hal_0_2::serial::Read<u8>> embedded_io::Read for SerialIo<TX, RX> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        // `embedded-io::Read` blocks until at least one byte is available.
        buf[0] = nb::block!(self.1.read()).map_err(|_| IoError)?;
        Ok(1)
    }
}

/// The concrete type [`init`] hands back.
///
/// A NAMED type, not `impl Trait`: a struct field has to name its type, and on
/// the RTIC runtime this handle becomes a `Local` resource. Returning `impl
/// Trait` made the peripheral unusable there — `#[init]` returned and dropped
/// it. Still `embedded-io` Read + Write for every caller.
pub type Handle = SerialIo<serial::Tx<pac::USART{N}>, serial::Rx<pac::USART{N}>>;

/// Initialise USART{N} and expose it as an `embedded-io` Read + Write value.
pub fn init(
    usart: pac::USART{N},
    pins: impl serial::Pins<pac::USART{N}>,
    afio: &mut afio::Parts,
    clocks: &Clocks,
) -> Handle {
    let (tx, rx) = Serial::new(usart, pins, &mut afio.mapr, get_config(), clocks).split();
    SerialIo(tx, rx)
}

// ── Using USART{N} ──
// Portable init — the handle is an `embedded-io` Read + Write, so this code
// works unchanged on any HAL. In main.rs, after the init above:
//
//     use embedded_io::{Read, Write};
//
//     // Send
//     {HANDLE}.write_all(b"hello\r\n").ok();
//     {HANDLE}.flush().ok();
//
//     // Receive exactly N bytes (blocks until they arrive)
//     let mut buf = [0u8; 4];
//     {HANDLE}.read_exact(&mut buf).ok();
//
//     // Or take whatever is available right now
//     let n = {HANDLE}.read(&mut buf).unwrap_or(0);

"#;

fn usart_config_file(n: u8, cfg: Option<&UsartModuleConfig>) -> String {
    let baud = cfg.map(|c| c.baud_rate).unwrap_or(115_200);
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
    // DMA is a third shape, not a third API style: the handles it returns are
    // the HAL's own DMA types, so `api_style` has nothing left to choose.
    let channels = usart_dma_channels(n);
    let dma = cfg
        .map(|c| c.blocking_dma)
        .filter(|_| channels.is_some())
        .unwrap_or_default();
    let tmpl = if dma.any() {
        USART_TMPL_DMA
    } else {
        match cfg.map(|c| c.api_style).unwrap_or_default() {
            ApiStyle::Portable => USART_TMPL,
            ApiStyle::Native => USART_TMPL_NATIVE,
        }
    };
    let (txch, rxch) = channels.unwrap_or(("dma1::C4", "dma1::C5"));
    // The handle the example names is the one `main.rs` actually binds —
    // module label included (`_serial1_mw_radar`), or the split pair on Native.
    let sfx = cfg
        .map(|c| module_label_sfx(&c.custom_label))
        .unwrap_or_default();
    let tx = format!("_tx{n}{sfx}");
    let rx = format!("_rx{n}{sfx}");
    tmpl.replace("{DMA_WHICH}", dma_which(dma))
        .replace("{TX_TY}", &usart_half_ty(n, "Tx", dma.tx()))
        .replace("{RX_TY}", &usart_half_ty(n, "Rx", dma.rx()))
        .replace("{TXCH_PARAM}", &channel_param("tx_ch", txch, dma.tx()))
        .replace("{RXCH_PARAM}", &channel_param("rx_ch", rxch, dma.rx()))
        .replace(
            "{TX_EXPR}",
            if dma.tx() { "tx.with_dma(tx_ch)" } else { "tx" },
        )
        .replace(
            "{RX_EXPR}",
            if dma.rx() { "rx.with_dma(rx_ch)" } else { "rx" },
        )
        .replace("{USART_EXAMPLE}", &usart_example(n, dma, &tx, &rx))
        .replace("{HANDLE}", &format!("_serial{n}{sfx}"))
        .replace("{TXCH}", txch)
        .replace("{RXCH}", rxch)
        .replace("{TX}", &format!("_tx{n}{sfx}"))
        .replace("{RX}", &format!("_rx{n}{sfx}"))
        .replace("{N}", &n.to_string())
        .replace("{BAUD}", &baud.to_string())
        .replace("{DATA}", &data.to_string())
        .replace("{PARITY}", &parity.to_string())
        .replace("{STOP}", &stop.to_string())
}

/// One line of prose naming which halves are on DMA, for the file's header.
fn dma_which(dma: BlockingDma) -> &'static str {
    match dma {
        BlockingDma::Both => "both directions",
        BlockingDma::Tx => "TX only, RX polled by the CPU",
        BlockingDma::Rx => "RX only, TX written by the CPU",
        BlockingDma::Off => "off",
    }
}

/// `serial::TxDma1` when that half is on DMA, `serial::Tx<pac::USART1>` when it
/// is not — the two are different types, which is the whole reason the halves
/// can be chosen separately.
fn usart_half_ty(n: u8, half: &str, on_dma: bool) -> String {
    if on_dma {
        format!("serial::{half}Dma{n}")
    } else {
        format!("serial::{half}<pac::USART{n}>")
    }
}

/// A channel parameter for `init`, or nothing when that half is not on DMA.
///
/// Leading newline and indentation included, so the signature closes cleanly
/// whichever combination is emitted.
fn channel_param(name: &str, ty: &str, on_dma: bool) -> String {
    if on_dma {
        format!("\n    {name}: {ty},")
    } else {
        String::new()
    }
}

/// The SPI handle's type. Three shapes, one per combination — the HAL puts the
/// channels in the type, so `Spi1TxDma` and `Spi1RxTxDma` are different values
/// with different methods.
fn spi_handle_ty(n: u8, dma: BlockingDma) -> String {
    let kind = match dma {
        BlockingDma::Both => "RxTxDma",
        BlockingDma::Tx => "TxDma",
        _ => "RxDma",
    };
    format!("hal_spi::Spi{n}{kind}<SpiRemap, SpiPins, hal_spi::Master>")
}

/// The builder call that turns a plain `Spi` into the chosen DMA shape.
fn spi_with(dma: BlockingDma) -> &'static str {
    match dma {
        BlockingDma::Both => "with_rx_tx_dma(rx_ch, tx_ch)",
        BlockingDma::Tx => "with_tx_dma(tx_ch)",
        _ => "with_rx_dma(rx_ch)",
    }
}

/// The worked example for a USART, written for the halves this file actually
/// has. A DMA half consumes its handle and hands it back from `wait()`; a
/// polled half is the HAL's `nb` one, and they are used nothing alike — showing
/// the wrong pair would be worse than showing none.
///
/// Built line by line rather than as one continued literal: a `\`-continuation
/// keeps its source indentation once rustfmt joins the lines, and that
/// indentation lands in the user's file.
fn usart_example(n: u8, dma: BlockingDma, tx: &str, rx: &str) -> String {
    let mut l: Vec<String> = vec![format!("// -- Using USART{n} --")];
    let traits = match (dma.tx(), dma.rx()) {
        (true, true) => Some("{ReadDma, WriteDma}"),
        (true, false) => Some("WriteDma"),
        (false, true) => Some("ReadDma"),
        (false, false) => None,
    };
    if let Some(t) = traits {
        l.push("// A DMA transfer CONSUMES its handle and gives it back from `wait()`, so".into());
        l.push("// rebind it every time, and the buffer must outlive the transfer.".into());
        l.push("//".into());
        l.push(format!("//     use stm32f1xx_hal::dma::{t};"));
    }
    l.push("//".into());
    l.push("// In main.rs, after the init above:".into());
    l.push("//".into());
    if dma.tx() {
        l.push("//     // Send - the DMA reads straight out of the slice.".into());
        l.push(format!(
            "//     let (_, tx) = {tx}.write(&b\"hello\"[..]).wait();"
        ));
        l.push(format!("//     {tx} = tx;"));
    } else {
        l.push("//     // TX is not on DMA: the CPU writes each byte.".into());
        l.push("//     use core::fmt::Write;".into());
        l.push(format!("//     writeln!({tx}, \"hello\").ok();"));
    }
    l.push("//".into());
    if dma.rx() {
        l.push("//     // Receive 8 bytes into a 'static buffer.".into());
        l.push("//     static mut RXBUF: [u8; 8] = [0; 8];".into());
        l.push(
            "//     let buf: &'static mut [u8; 8] = unsafe { &mut *core::ptr::addr_of_mut!(RXBUF) };"
                .into(),
        );
        l.push(format!("//     let (buf, rx) = {rx}.read(buf).wait();"));
        l.push(format!("//     {rx} = rx;"));
        l.push("//".into());
        l.push("// For a receiver that never misses bytes between reads, use the circular".into());
        l.push(format!(
            "// form instead: `let mut circ = {rx}.circ_read(two_buffers);`."
        ));
    } else {
        l.push("//     // RX is not on DMA: one byte at a time, blocking.".into());
        l.push("//     use nb::block;".into());
        l.push(format!(
            "//     let byte = block!({rx}.read()).unwrap_or(0);"
        ));
    }
    l.join("\n")
}

/// The worked example for an SPI. Each shape has ONE trait and one method:
/// `write` for TX-only, `read` for RX-only, `read_write` for both.
fn spi_example(n: u8, dma: BlockingDma, handle: &str) -> String {
    let tr = match dma {
        BlockingDma::Both => "ReadWriteDma",
        BlockingDma::Tx => "WriteDma",
        _ => "ReadDma",
    };
    let mut l: Vec<String> = vec![
        format!("// -- Using SPI{n} on DMA --"),
        "// A transfer CONSUMES the handle and gives it back from `wait()`, so rebind".into(),
        "// it every time. The buffers must be 'static. In main.rs, after the init:".into(),
        "//".into(),
        format!("//     use stm32f1xx_hal::dma::{tr};"),
        "//".into(),
    ];
    let decl = |name: &str, init: &str| {
        vec![
            format!("//     static mut {name}: [u8; 4] = {init};"),
            format!(
                "//     let {} : &'static mut [u8; 4] = unsafe {{ &mut *core::ptr::addr_of_mut!({name}) }};",
                name.to_lowercase()
            ),
        ]
    };
    match dma {
        BlockingDma::Both => {
            l.extend(decl("RXBUF", "[0; 4]"));
            l.extend(decl("TXBUF", "[0x9F, 0, 0, 0]"));
            l.push(format!(
                "//     let ((rxbuf, _txbuf), spi) = {handle}.read_write(rxbuf, txbuf).wait();"
            ));
            l.push(format!("//     {handle} = spi;"));
            l.push("//     // rxbuf now holds the reply.".into());
        }
        BlockingDma::Tx => {
            l.extend(decl("TXBUF", "[0x9F, 0, 0, 0]"));
            l.push(format!(
                "//     let (_txbuf, spi) = {handle}.write(txbuf).wait();"
            ));
            l.push(format!("//     {handle} = spi;"));
            l.push("//     // Send only - nothing is captured from MISO.".into());
        }
        _ => {
            l.extend(decl("RXBUF", "[0; 4]"));
            l.push(format!(
                "//     let (rxbuf, spi) = {handle}.read(rxbuf).wait();"
            ));
            l.push(format!("//     {handle} = spi;"));
            l.push("//     // rxbuf holds what MISO clocked in.".into());
        }
    }
    l.join("\n")
}

/// The `(tx, rx)` DMA channels `stm32f1xx-hal` binds to `USART{n}`.
///
/// Not a choice: the HAL puts the channel in the TYPE (`serial::TxDma1` is
/// `TxDma<Tx<USART1>, dma1::C4>` and nothing else), so `init` has to name the
/// same one. Taken from `serialdma!` in the HAL, which matches RM0008's fixed
/// request map. `None` for an instance the HAL gives no DMA — only USART1..3
/// have it, UART4/5 do not.
fn usart_dma_channels(n: u8) -> Option<(&'static str, &'static str)> {
    match n {
        1 => Some(("dma1::C4", "dma1::C5")),
        2 => Some(("dma1::C7", "dma1::C6")),
        3 => Some(("dma1::C2", "dma1::C3")),
        _ => None,
    }
}

/// Every channel the STM32F1 BLOCKING path uses, for the Configuration tab.
///
/// Reads the same two tables the templates do, so it cannot report a channel
/// the generated code does not take. The interrupt is left empty on purpose:
/// on this path the HAL owns it and no generated line names it, unlike the
/// embassy one where `bind_interrupts!` spells it out.
pub fn blocking_dma_uses(mcu: &crate::panels::mcu_module::Mcu) -> Vec<super::dma_map::DmaUse> {
    use super::dma_map::{Bus, DmaUse};
    use crate::panels::mcu_module::modules::ModuleConfig;

    /// Is `want` wired? Asked of the PIN MAP, the same place `config_files`
    /// asks, not of the module — a module survives losing a pin.
    fn wired_fn(mcu: &crate::panels::mcu_module::Mcu, want: PinFunction) -> bool {
        mcu.iter_all_pins()
            .any(|p| !p.reserved && p.selected_function == want)
    }
    let has_miso = |mcu: &_, n| wired_fn(mcu, PinFunction::SpiMiso(n));
    // A USART missing either pad is never initialised (see `gen_parts`), so it
    // takes no channel however its module is configured.
    let usart_built = |mcu: &_, n| {
        wired_fn(mcu, PinFunction::UsartTx(n)) && wired_fn(mcu, PinFunction::UsartRx(n))
    };

    let mut out = Vec::new();
    let mut push = |peri: &str, bus: Bus, n: u8, dir: &str| {
        out.push(DmaUse {
            // `dma1::C4` is how the config's `init` declares it; the card
            // shows the singleton spelling every other family uses.
            peri: match peri.split_once("::C") {
                Some((bank, n)) => format!("{}_CH{n}", bank.to_uppercase()),
                None => peri.to_owned(),
            },
            irq: String::new(),
            user: format!("{}{n} {dir}", bus.label()),
            manual: false,
        });
    };
    for m in &mcu.modules {
        match &m.config {
            ModuleConfig::Usart(c) if c.blocking_dma.any() && usart_built(mcu, c.instance) => {
                if let Some((tx, rx)) = usart_dma_channels(c.instance) {
                    if c.blocking_dma.tx() {
                        push(tx, Bus::Usart, c.instance, "TX");
                    }
                    if c.blocking_dma.rx() {
                        push(rx, Bus::Usart, c.instance, "RX");
                    }
                }
            }
            ModuleConfig::Spi(c) if c.blocking_dma.any() => {
                if let Some((rx, tx)) = spi_dma_channels(c.instance) {
                    // The same narrowing the templates apply, so the card
                    // cannot claim a channel `init` never asks for.
                    let dma = if has_miso(mcu, c.instance) {
                        c.blocking_dma
                    } else {
                        c.blocking_dma.without_rx()
                    };
                    if dma.tx() {
                        push(tx, Bus::Spi, c.instance, "TX");
                    }
                    if dma.rx() {
                        push(rx, Bus::Spi, c.instance, "RX");
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The DMA channels this bus instance would get, as a label for the UI — or
/// `None` when it cannot run on DMA at all.
///
/// Answers from the SAME tables codegen uses, so the checkbox can never offer a
/// transport the templates would then decline to emit. I2C is always `None`:
/// `stm32f1xx-hal` has no DMA for it, unlike USART and SPI.
pub fn blocking_dma_channels(
    family: &str,
    bus: super::dma_map::Bus,
    instance: u8,
) -> Option<String> {
    if family != "stm32f1" {
        return None;
    }
    match bus {
        // The F1 backend has no SD-card codegen; the arm exists so the match
        // stays exhaustive.
        super::dma_map::Bus::Sdmmc => None,
        super::dma_map::Bus::Usart => {
            usart_dma_channels(instance).map(|(tx, rx)| format!("{tx} TX / {rx} RX"))
        }
        super::dma_map::Bus::Spi => {
            spi_dma_channels(instance).map(|(rx, tx)| format!("{tx} TX / {rx} RX"))
        }
        super::dma_map::Bus::I2c => None,
        // STM32F1 has no LPUART at all, so this is unreachable in practice —
        // `None` keeps it harmless if a future family reuses this helper.
        super::dma_map::Bus::Lpuart => None,
    }
}

/// `dma1::C4` (the TYPE, as a config's `init` names it) -> `dma1.4` (the VALUE
/// main.rs passes). `DmaExt::split` returns a tuple struct whose fields ARE the
/// channels, so the index is the channel number.
fn channel_field(ty: &str) -> String {
    match ty.rsplit_once("::C") {
        Some((bank, n)) => format!("{bank}.{n}"),
        None => ty.to_owned(),
    }
}

/// The `(rx, tx)` DMA channels for `SPI{n}` — same story, from `spi_dma!`.
///
/// SPI3 is deliberately absent: its channels live on DMA2, which only the
/// connectivity-line parts have, and the HAL gates them behind a feature the
/// generated manifest does not set.
fn spi_dma_channels(n: u8) -> Option<(&'static str, &'static str)> {
    match n {
        1 => Some(("dma1::C2", "dma1::C3")),
        2 => Some(("dma1::C4", "dma1::C5")),
        _ => None,
    }
}

/// Blocking `stm32f1xx-hal` USART on DMA. Returns the two DMA halves.
///
/// Neither the portable nor the native template can be reused: the DMA handles
/// are `TxDma`/`RxDma`, whose transfer methods CONSUME the handle and hand it
/// back from `wait()`. That is a different shape from both `embedded-io` and
/// the `nb` halves, so it gets its own file and its own worked example.
const USART_TMPL_DMA: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) - auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
// <<< GENERATED END >>>

// Everything below is editable - your changes are preserved on regeneration.
// DMA transport (chosen in the Virtual Module): {DMA_WHICH}. A half on DMA
// moves bytes without the CPU polling for each one; a half left off is the
// ordinary `nb` handle, which is often the better trade for short bursts - and
// it leaves that channel free for another peripheral.
//
// The channels are NOT a choice on this chip: stm32f1xx-hal puts them in the
// type, fixing USART{N} to {TXCH} for TX and {RXCH} for RX.
use stm32f1xx_hal::{
    afio,
    dma::dma1,
    pac,
    prelude::*,
    rcc::Clocks,
    serial::{self, Config, Serial, StopBits},
};

/// The handles [`init`] hands back. Named so they can be struct fields.
pub type TxHandle = {TX_TY};
pub type RxHandle = {RX_TY};

fn get_config() -> serial::Config {
    let mut config = Config::default().baudrate(BAUDRATE.bps());
    if DATA_BITS == 8 {
        config = config.wordlength_8bits();
    } else if DATA_BITS == 9 {
        config = config.wordlength_9bits();
    }
    if PARITY == 'N' {
        config = config.parity_none();
    } else if PARITY == 'O' {
        config = config.parity_odd();
    } else if PARITY == 'E' {
        config = config.parity_even();
    }
    if STOP_BITS == 1 {
        config = config.stopbits(StopBits::STOP1);
    } else if STOP_BITS == 2 {
        config = config.stopbits(StopBits::STOP2);
    }
    config
}

pub fn init<PINS: serial::Pins<pac::USART{N}>>(
    usart: pac::USART{N},
    pins: PINS,
    afio: &mut afio::Parts,
    clocks: &Clocks,{TXCH_PARAM}{RXCH_PARAM}
) -> (TxHandle, RxHandle) {
    let (tx, rx) = Serial::new(usart, pins, &mut afio.mapr, get_config(), clocks).split();
    ({TX_EXPR}, {RX_EXPR})
}

{USART_EXAMPLE}
"#;

/// Blocking `stm32f1xx-hal` SPI on DMA. Returns the combined RX+TX handle.
const SPI_TMPL_DMA: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) - auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_KHZ: u32 = {KHZ};

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the peripheral has to update them; the `use` is
// here too, so the aliases hold whatever you do to the `use` block below.
use stm32f1xx_hal::{gpio as hal_gpio, spi as hal_spi};

/// The pins SPI{N} is wired to, in HAL order: (SCK, MISO, MOSI). A signal you
/// left unwired is the HAL's `No...` placeholder.
#[allow(dead_code)]
pub type SpiPins = ({PINS});

/// Whether those are the peripheral's default pins or its remapped set.
#[allow(dead_code)]
pub type SpiRemap = {REMAP};
// <<< GENERATED END >>>

// Everything below is editable - your changes are preserved on regeneration.
// DMA transport (chosen in the Virtual Module): {DMA_WHICH}. The channels are
// NOT a choice on this chip: stm32f1xx-hal fixes SPI{N} to {RXCH} for RX and
// {TXCH} for TX.
use stm32f1xx_hal::{
    afio,
    dma::dma1,
    pac,
    prelude::*,
    rcc::Clocks,
    spi::{Mode, Phase, Polarity, Spi},
};

fn get_mode() -> Mode {
    match SPI_MODE {
        1 => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnSecondTransition },
        2 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnFirstTransition },
        3 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnSecondTransition },
        _ => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnFirstTransition },
    }
}

/// The DMA handle [`init`] hands back - its TYPE says which halves are on DMA.
pub type Handle = {HANDLE_TY};

pub fn init(
    spi: pac::SPI{N},
    pins: SpiPins,
    {AFIO_PARAM}: &mut afio::Parts,
    clocks: &Clocks,{RXCH_PARAM}{TXCH_PARAM}
) -> Handle {
    Spi::spi{N}(spi, pins, {AFIO_ARG}get_mode(), CLOCK_KHZ.kHz(), *clocks)
        .{WITH}
}

{SPI_EXAMPLE}
"#;

/// Native `stm32f1xx-hal` USART init (no embedded-io bridge). Returns the split
/// `(Tx, Rx)` halves — the idiomatic stm32f1xx-hal handles (TX for `writeln!`, RX
/// for reading). Selected via `ApiStyle`; the `main.rs` binding destructures the
/// tuple (`let (mut _txN, mut _rxN) = …`), unlike the single-value Portable form.
const USART_TMPL_NATIVE: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const BAUDRATE: u32 = {BAUD};
pub const DATA_BITS: u8 = {DATA}; // 8, 9
pub const PARITY: char = '{PARITY}'; // 'N' None, 'O' Odd, 'E' Even
pub const STOP_BITS: u8 = {STOP}; // 1, 2
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
// NATIVE stm32f1xx-hal API (chosen in the Virtual Module). `init` returns the
// split `(Tx, Rx)` handles — use `Tx` with `writeln!` / `Rx` with `.read()`.
// Switch to the portable `embedded-io` API in the Virtual Module.
use stm32f1xx_hal::{
    pac,
    prelude::*,
    afio,
    rcc::Clocks,
    serial::{self, Config, Serial, StopBits},
};

fn get_config() -> serial::Config {
    let mut config = Config::default().baudrate(BAUDRATE.bps());
    if DATA_BITS == 8 {
        config = config.wordlength_8bits();
    } else if DATA_BITS == 9 {
        config = config.wordlength_9bits();
    }
    if PARITY == 'N' {
        config = config.parity_none();
    } else if PARITY == 'O' {
        config = config.parity_odd();
    } else if PARITY == 'E' {
        config = config.parity_even();
    }
    if STOP_BITS == 1 {
        config = config.stopbits(StopBits::STOP1);
    } else if STOP_BITS == 2 {
        config = config.stopbits(StopBits::STOP2);
    }
    config
}

pub fn init<PINS: serial::Pins<pac::USART{N}>>(
    usart: pac::USART{N},
    pins: PINS,
    afio: &mut afio::Parts,
    clocks: &Clocks,
) -> (serial::Tx<pac::USART{N}>, serial::Rx<pac::USART{N}>) {
    Serial::new(usart, pins, &mut afio.mapr, get_config(), clocks).split()
}

// ── Using USART{N} ──
// Native init — `init` returns the HAL's own split halves, which are `nb`
// (non-blocking) based. In main.rs, after the init above:
//
//     use nb::block;
//
//     // Send one byte at a time
//     for b in b"hello\r\n" {
//         block!({TX}.write(*b)).ok();
//     }
//     block!({TX}.flush()).ok();
//
//     // Receive one byte, blocking until it arrives
//     let byte = block!({RX}.read()).unwrap_or(0);
//
//     // Non-blocking poll: Err(nb::Error::WouldBlock) means "nothing yet"
//     match {RX}.read() {
//         Ok(b) => { /* got b */ }
//         Err(nb::Error::WouldBlock) => { /* try again later */ }
//         Err(_) => { /* framing / overrun error */ }
//     }

"#;

const SPI_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_KHZ: u32 = {KHZ};

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the peripheral has to update them; the `use` is
// here too, so the aliases hold whatever you do to the `use` block below.
use stm32f1xx_hal::{gpio as hal_gpio, spi as hal_spi};

/// The pins SPI{N} is wired to, in HAL order: (SCK, MISO, MOSI). A signal you
/// left unwired is the HAL's `No…` placeholder.
///
/// `allow(dead_code)`: `init` below is yours to edit and may stop naming it.
#[allow(dead_code)]
pub type SpiPins = ({PINS});

/// Whether those are the peripheral's default pins or its remapped set — the
/// HAL takes the remap register bit from this type.
#[allow(dead_code)]
pub type SpiRemap = {REMAP};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
use stm32f1xx_hal::{
    pac,
    prelude::*,
    afio,
    rcc::Clocks,
    spi::{Mode, Phase, Polarity, Spi},
};

fn get_mode() -> Mode {
    match SPI_MODE {
        1 => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnSecondTransition },
        2 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnFirstTransition },
        3 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnSecondTransition },
        _ => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnFirstTransition },
    }
}

/// Bridges the HAL's blocking/nb SPI (embedded-hal 0.2) to the STANDARD
/// `embedded-hal` 1.0 `SpiBus`, so driver/app code stays portable across HALs:
///
///     fn app<S: embedded_hal::spi::SpiBus>(spi: &mut S) { /* … */ }
pub struct SpiBusIo<SPI>(pub SPI);

#[derive(Debug)]
pub struct IoError;
impl embedded_hal::spi::Error for IoError {
    fn kind(&self) -> embedded_hal::spi::ErrorKind {
        embedded_hal::spi::ErrorKind::Other
    }
}
impl<SPI> embedded_hal::spi::ErrorType for SpiBusIo<SPI> {
    type Error = IoError;
}
impl<SPI> embedded_hal::spi::SpiBus<u8> for SpiBusIo<SPI>
where
    SPI: embedded_hal_0_2::blocking::spi::Write<u8>
        + embedded_hal_0_2::blocking::spi::Transfer<u8>
        + embedded_hal_0_2::spi::FullDuplex<u8>,
{
    fn write(&mut self, words: &[u8]) -> Result<(), Self::Error> {
        embedded_hal_0_2::blocking::spi::Write::write(&mut self.0, words).map_err(|_| IoError)
    }
    fn read(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        for w in words.iter_mut() {
            nb::block!(embedded_hal_0_2::spi::FullDuplex::send(&mut self.0, 0)).map_err(|_| IoError)?;
            *w = nb::block!(embedded_hal_0_2::spi::FullDuplex::read(&mut self.0)).map_err(|_| IoError)?;
        }
        Ok(())
    }
    fn transfer_in_place(&mut self, words: &mut [u8]) -> Result<(), Self::Error> {
        embedded_hal_0_2::blocking::spi::Transfer::transfer(&mut self.0, words)
            .map(|_| ())
            .map_err(|_| IoError)
    }
    fn transfer(&mut self, read: &mut [u8], write: &[u8]) -> Result<(), Self::Error> {
        // Clock max(read, write) bytes; pad the write with 0, ignore reads past `read`.
        for i in 0..read.len().max(write.len()) {
            let b = write.get(i).copied().unwrap_or(0);
            nb::block!(embedded_hal_0_2::spi::FullDuplex::send(&mut self.0, b)).map_err(|_| IoError)?;
            let r = nb::block!(embedded_hal_0_2::spi::FullDuplex::read(&mut self.0)).map_err(|_| IoError)?;
            if let Some(slot) = read.get_mut(i) {
                *slot = r;
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// The concrete type [`init`] hands back.
///
/// A NAMED type, not `impl Trait`: a struct field — an RTIC `Local` resource,
/// say — has to name what it holds, and an opaque type cannot be named. The pins
/// stay inside the HAL's `Spi<…>`, so `SpiPins`/`SpiRemap` from the GENERATED
/// block above are part of it; re-wiring SPI{N} updates this alias with them.
/// It is still an `embedded-hal` 1.0 `SpiBus` for every caller.
pub type Handle = SpiBusIo<Spi<pac::SPI{N}, SpiRemap, SpiPins, u8>>;

/// Initialise SPI{N} and expose it as an `embedded-hal` 1.0 `SpiBus`.
pub fn init(
    spi: pac::SPI{N},
    pins: SpiPins,
    {AFIO_PARAM}: &mut afio::Parts,
    clocks: &Clocks,
) -> Handle {
    let bus = Spi::spi{N}(spi, pins, {AFIO_ARG}get_mode(), CLOCK_KHZ.kHz(), *clocks);
    SpiBusIo(bus)
}

// ── Using SPI{N} ──
// Portable init — the handle is an `embedded-hal` 1.0 `SpiBus`. In main.rs,
// after the init above:
//
// NOTE: main.rs binds this handle as `let {HANDLE} = …` — add `mut`
// there before calling anything below (bus methods take `&mut self`).
//
//     use embedded_hal::spi::SpiBus;
//
//     // Write only
//     {HANDLE}.write(&[0x9F]).ok();
//
//     // Read only (clocks out zeros)
//     let mut rx = [0u8; 3];
//     {HANDLE}.read(&mut rx).ok();
//
//     // Full duplex, separate buffers
//     {HANDLE}.transfer(&mut rx, &[0x9F, 0x00, 0x00]).ok();
//
//     // Full duplex in place: `buf` is sent, then overwritten by the reply
//     let mut buf = [0x9F, 0x00, 0x00];
//     {HANDLE}.transfer_in_place(&mut buf).ok();
//     {HANDLE}.flush().ok();
//
//     // NSS/CS is a plain GPIO here — drive it low around a transaction.

"#;

/// `has_miso` is what makes this a transmitter rather than a bus: with the pin
/// unwired the HAL's `NoMiso` placeholder takes its slot in `SpiPins`, and the
/// receive half of any DMA choice is dropped — see `BlockingDma::without_rx`.
fn spi_config_file(
    n: u8,
    cfg: Option<&SpiModuleConfig>,
    pin_tys: &(String, String),
    has_miso: bool,
) -> String {
    let mode = cfg.map(|c| c.mode).unwrap_or(0);
    let khz = cfg.map(|c| c.clock_hz).unwrap_or(1_000_000) / 1_000;
    let channels = spi_dma_channels(n);
    let dma = cfg
        .map(|c| c.blocking_dma)
        .filter(|_| channels.is_some())
        .map(|d| if has_miso { d } else { d.without_rx() })
        .unwrap_or_default();
    let tmpl = if dma.any() {
        SPI_TMPL_DMA
    } else {
        match cfg.map(|c| c.api_style).unwrap_or_default() {
            ApiStyle::Portable => SPI_TMPL,
            ApiStyle::Native => SPI_TMPL_NATIVE,
        }
    };
    // Without MISO the bus can only SEND, and the polled examples are the ones
    // that would mislead: `read`/`transfer` still COMPILE (the HAL takes
    // `NoMiso` as a type-state placeholder, not as a narrower type), they just
    // return whatever the unconfigured pad happens to read. The DMA template
    // needs no such note — `without_rx` already left it with the send example.
    let tmpl = if has_miso {
        tmpl.to_owned()
    } else {
        tmpl.replace(
            "// ── Using SPI{N} ──\n",
            "// ── Using SPI{N} ──\n\
             // MISO is not wired: this bus can only SEND. `read` / `transfer` still\n\
             // compile, but they return whatever the unconnected pad reads.\n",
        )
    };
    let (rxch, txch) = channels.unwrap_or(("dma1::C2", "dma1::C3"));
    let (pins, remap) = pin_tys;
    // Only SPI1 can be remapped, so only `Spi::spi1` takes the AFIO register.
    let (afio_param, afio_arg) = afio_subst(n == 1);
    let sfx = cfg
        .map(|c| module_label_sfx(&c.custom_label))
        .unwrap_or_default();
    tmpl.replace("{HANDLE}", &format!("_spi{n}{sfx}"))
        .replace("{N}", &n.to_string())
        .replace("{MODE}", &mode.to_string())
        .replace("{KHZ}", &khz.to_string())
        .replace("{PINS}", pins)
        .replace("{REMAP}", remap)
        .replace("{AFIO_PARAM}", afio_param)
        .replace("{AFIO_ARG}", afio_arg)
        .replace("{RXCH}", rxch)
        .replace("{TXCH}", txch)
        .replace("{DMA_WHICH}", dma_which(dma))
        .replace("{HANDLE_TY}", &spi_handle_ty(n, dma))
        .replace("{RXCH_PARAM}", &channel_param("rx_ch", rxch, dma.rx()))
        .replace("{TXCH_PARAM}", &channel_param("tx_ch", txch, dma.tx()))
        .replace("{WITH}", spi_with(dma))
        .replace(
            "{SPI_EXAMPLE}",
            &spi_example(n, dma, &format!("_spi{n}{sfx}")),
        )
}

/// The channels wired on TIM`tim`, ascending. One place, because `gen_parts`
/// (which needs the BINDINGS) and `config_files` (which needs the pin TYPES)
/// have to agree on the order — it is `pwm_hz`'s tuple order.
fn pwm_chans<'a>(all_pins: &[&'a Pin], tim: u8) -> Vec<(u8, &'a Pin)> {
    let mut v: Vec<(u8, &Pin)> = all_pins
        .iter()
        .copied()
        .filter(|p| !p.reserved)
        .filter_map(|p| match p.selected_function {
            PinFunction::TimerPwm { timer, channel } if timer == tim => Some((channel, p)),
            _ => None,
        })
        .collect();
    v.sort_by_key(|(c, _)| *c);
    v
}

/// The timers with at least one channel wired, ascending.
fn pwm_timers(all_pins: &[&Pin]) -> Vec<u8> {
    let mut v: Vec<u8> = all_pins
        .iter()
        .filter(|p| !p.reserved)
        .filter_map(|p| match p.selected_function {
            PinFunction::TimerPwm { timer, .. } => Some(timer),
            _ => None,
        })
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// `src/pins/configs/pwm{tim}.rs` — one timer, its frequency, and a duty per
/// channel.
///
/// The duty is applied HERE rather than in `main.rs` because that is what makes
/// the numbers editable: `FREQUENCY_HZ` and `DUTY_CH*_X100` are consts in the
/// generated block, so the Virtual Module rewrites them and nothing else in the
/// file moves.
fn pwm_config_file(
    tim: u8,
    cfg: Option<&TimerModuleConfig>,
    chans: &[(u8, &Pin)],
    remap: &str,
) -> String {
    let hz = cfg.map(|c| c.freq_hz).unwrap_or(1_000);
    let mut duties = String::new();
    let mut sets = String::new();
    for (c, _) in chans {
        let x100 = cfg.map(|t| t.duty_x100_of(*c)).unwrap_or(0);
        duties.push_str(&format!(
            "pub const DUTY_CH{c}_X100: u32 = {x100}; // {} %\n",
            duty_percent_str(x100)
        ));
        // u32 all the way through the multiply: a 16-bit reload times 10_000
        // needs 30 bits, so doing it in u16 would wrap on any duty above ~0.1 %.
        sets.push_str(&format!(
            "    pwm.set_duty(Channel::C{c}, (max as u32 * DUTY_CH{c}_X100 / 10_000) as u16);\n"
        ));
        sets.push_str(&format!("    pwm.enable(Channel::C{c});\n"));
    }
    // One channel is NOT a 1-tuple: the HAL implements `Pins` for the bare pin,
    // so both the pin type and the channel marker stay bare.
    let join = |v: Vec<String>| {
        if v.len() == 1 {
            v[0].clone()
        } else {
            format!("({})", v.join(", "))
        }
    };
    let pins_ty = join(
        chans
            .iter()
            .map(|(_, p)| {
                parse_pin(&p.name)
                    .map(|m| format!("hal_gpio::P{}{}<hal_gpio::Alternate>", m.port, m.pin_num))
                    .unwrap_or_else(|| "hal_gpio::PA0<hal_gpio::Alternate>".into())
            })
            .collect(),
    );
    let ch_markers = join(
        chans
            .iter()
            .map(|(c, _)| format!("Ch<{}>", c - 1))
            .collect(),
    );
    let list = chans
        .iter()
        .map(|(c, _)| format!("CH{c}"))
        .collect::<Vec<_>>()
        .join("+");
    let sfx = cfg
        .map(|c| module_label_sfx(&c.custom_label))
        .unwrap_or_default();

    // The channel goes in the METHOD NAME, never in a parameter: `set_duty`
    // panics on a channel the timer has no pin for (`PINS::check_used` —
    // "Unused channel"), so a setter that took one could be handed a panic.
    // Naming them after what is wired makes that unreachable.
    let mut duty_decls = String::new();
    let mut duty_impls = String::new();
    for (c, p) in chans {
        duty_decls.push_str(&format!(
            "
    /// CH{c}, on {}.
    fn set_duty_tim_{tim}_ch{c}(&mut self, value: u32);
",
            p.name
        ));
        duty_impls.push_str(&format!(
            "
    fn set_duty_tim_{tim}_ch{c}(&mut self, value: u32) {{
        self.set_duty(Channel::C{c}, (self.get_max_duty() as u32 * value / 10_000) as u16);
    }}
"
        ));
    }
    // `pwm_timers` only yields timers with a channel, so this fallback is for a
    // caller that does not exist yet — but indexing here would panic the IDE.
    let first = chans.first().map_or(1, |(c, _)| *c);
    PWM_TMPL
        .replace("{HANDLE}", &format!("_pwm{tim}{sfx}"))
        .replace("{N}", &tim.to_string())
        .replace("{HZ}", &hz.to_string())
        .replace("{DUTIES}", duties.trim_end_matches('\n'))
        .replace("{SETS}", &sets)
        .replace("{PINS}", &pins_ty)
        .replace("{CHS}", &ch_markers)
        .replace("{REMAP}", remap)
        .replace("{LIST}", &list)
        .replace("{DUTY_DECLS}", &duty_decls)
        .replace("{DUTY_IMPLS}", &duty_impls)
        .replace("{CH1ST}", &format!("C{first}"))
        .replace("{CH1N}", &first.to_string())
}

/// One `stm32f1xx-hal` timer in PWM mode. `pwm_hz` takes the pins BY VALUE and
/// reads the pads off the remap type-state, so both live in the generated block.
const PWM_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const FREQUENCY_HZ: u32 = {HZ}; // one frequency for the whole timer
// Duty per channel, in HUNDREDTHS of a percent — 750 is 7.5 %, which is what a
// hobby servo wants and what whole percent cannot say.
{DUTIES}

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the timer has to update them; the `use` is here
// too, so the aliases hold whatever you do to the `use` block below.
use stm32f1xx_hal::gpio as hal_gpio;

/// The pins TIM{N} drives, in `pwm_hz` order: {LIST}.
///
/// `allow(dead_code)`: `init` below is yours to edit and may stop naming it.
#[allow(dead_code)]
pub type PwmPins = {PINS};

/// Which pads the timer comes out on. NOT decoration: `pwm_hz` reads the remap
/// REGISTER BIT from this type, so the wrong one drives the wrong pins.
pub type PwmRemap = stm32f1xx_hal::timer::{REMAP};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
use stm32f1xx_hal::{
    afio, pac,
    prelude::*,
    rcc::Clocks,
    timer::{Ch, Channel, PwmHz, Timer},
};

/// The concrete type [`init`] hands back. The HAL keeps the channel markers AND
/// the pins inside it, so this alias moves with the wiring — which is what lets
/// it be a struct field or an RTIC `Local`.
pub type Handle = PwmHz<pac::TIM{N}, PwmRemap, {CHS}, PwmPins>;

/// Initialise TIM{N} {LIST} at `FREQUENCY_HZ`, each channel at its own duty,
/// and enable them.
pub fn init(tim: pac::TIM{N}, pins: PwmPins, afio: &mut afio::Parts, clocks: &Clocks) -> Handle {
    let mut pwm =
        Timer::new(tim, clocks).pwm_hz::<PwmRemap, _, _>(pins, &mut afio.mapr, FREQUENCY_HZ.Hz());
    let max = pwm.get_max_duty();
{SETS}    pwm
}

/// Set a channel's duty in the same units the `DUTY_*` constants above use —
/// HUNDREDTHS of a percent, so `10_000` is 100 % and `750` is 7.5 %.
///
/// A trait rather than an inherent method because `Handle` is an alias for the
/// HAL's own `PwmHz`, which this crate does not own.
///
/// One method per WIRED channel ({LIST}). The channel is part of the NAME
/// rather than an argument on purpose: `set_duty` PANICS on a channel this
/// timer has no pin for — `PINS::check_used` says "Unused channel" — and a duty
/// setter should not be able to reach a panic. `set_duty_tim_{N}` drives
/// {CH1ST} — the lowest channel you wired.
pub trait DutyHandle {
    /// {CH1ST}, the lowest channel wired to TIM{N}.
    fn set_duty_tim_{N}(&mut self, value: u32);
{DUTY_DECLS}}

impl DutyHandle for Handle {
    fn set_duty_tim_{N}(&mut self, value: u32) {
        self.set_duty_tim_{N}_ch{CH1N}(value);
    }
{DUTY_IMPLS}}

// ── Using TIM{N} ──
// The duty is already set and the channels are already running. To change one
// from your loop:
//
//     use pins::configs::pwm{N}::DutyHandle;
//     {HANDLE}.set_duty_tim_{N}(2_500); // 25 %
//
// or through the HAL directly, which is what the trait does — except that the
// channel is then yours to get right: {LIST} are wired, and any other panics.
//
//     use stm32f1xx_hal::timer::Channel;
//
//     let max = {HANDLE}.get_max_duty();
//     {HANDLE}.set_duty(Channel::{CH1ST}, max / 2); // 50 %
//
//     // …and to stop driving a pad without tearing the timer down:
//     {HANDLE}.disable(Channel::{CH1ST});
"#;

/// `({AFIO_PARAM}, {AFIO_ARG})` for a config template.
///
/// In stm32f1xx-hal 0.10 only the FIRST instance of SPI and I2C takes the AFIO
/// remap register — `Spi::spi2` / `BlockingI2c::i2c2` have no `mapr` argument,
/// because those peripherals have no alternate pin set to select. main.rs passes
/// `&mut afio` to every instance all the same (one call shape for all of them),
/// so the parameter stays and is underscored where the HAL cannot use it.
fn afio_subst(takes_mapr: bool) -> (&'static str, &'static str) {
    if takes_mapr {
        ("afio", "&mut afio.mapr, ")
    } else {
        ("_afio", "")
    }
}

/// Native `stm32f1xx-hal` SPI init (no eh-1.0 bridge). Returns `Spi<…>`.
const SPI_TMPL_NATIVE: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const SPI_MODE: u8 = {MODE}; // 0..=3 (CPOL/CPHA)
pub const CLOCK_KHZ: u32 = {KHZ};

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the peripheral has to update them; the `use` is
// here too, so the aliases hold whatever you do to the `use` block below.
use stm32f1xx_hal::{gpio as hal_gpio, spi as hal_spi};

/// The pins SPI{N} is wired to, in HAL order: (SCK, MISO, MOSI). A signal you
/// left unwired is the HAL's `No…` placeholder.
///
/// `allow(dead_code)`: `init` below is yours to edit and may stop naming it.
#[allow(dead_code)]
pub type SpiPins = ({PINS});

/// Whether those are the peripheral's default pins or its remapped set — the
/// HAL takes the remap register bit from this type.
#[allow(dead_code)]
pub type SpiRemap = {REMAP};
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
// NATIVE stm32f1xx-hal API — returns `Spi<…>`. Switch to the portable
// `embedded-hal` 1.0 `SpiBus` API in the Virtual Module.
use stm32f1xx_hal::{
    pac,
    prelude::*,
    afio,
    rcc::Clocks,
    spi::{Mode, Phase, Polarity, Spi},
};

fn get_mode() -> Mode {
    match SPI_MODE {
        1 => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnSecondTransition },
        2 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnFirstTransition },
        3 => Mode { polarity: Polarity::IdleHigh, phase: Phase::CaptureOnSecondTransition },
        _ => Mode { polarity: Polarity::IdleLow, phase: Phase::CaptureOnFirstTransition },
    }
}

/// The concrete type [`init`] hands back — the HAL's own `Spi`, with the pins
/// and the remap state it keeps in its type. Named so it can be a struct field
/// (an RTIC `Local` resource, say); the two aliases come from the GENERATED
/// block above, so re-wiring SPI{N} updates this one with them.
pub type Handle = Spi<pac::SPI{N}, SpiRemap, SpiPins, u8>;

pub fn init(
    spi: pac::SPI{N},
    pins: SpiPins,
    {AFIO_PARAM}: &mut afio::Parts,
    clocks: &Clocks,
) -> Handle {
    Spi::spi{N}(spi, pins, {AFIO_ARG}get_mode(), CLOCK_KHZ.kHz(), *clocks)
}

// ── Using SPI{N} ──
// Native init — the concrete `stm32f1xx-hal` Spi, whose traits are
// `embedded-hal` 0.2. In main.rs, after the init above:
//
// NOTE: the Native path needs the 0.2 traits, which the IDE only adds to
// Cargo.toml for PORTABLE modules. Add them yourself:
//   embedded-hal-0-2 = { package = "embedded-hal", version = "0.2.7", features = ["unproven"] }
// — or switch this module's Init API to Portable, where the handle is an
// embedded-hal 1.0 bus and no extra dependency is needed.
//
// NOTE: main.rs binds this handle as `let {HANDLE} = …` — add `mut`
// there before calling anything below (bus methods take `&mut self`).
//
//     use embedded_hal_0_2::blocking::spi::{Transfer, Write};
//
//     // Write only
//     {HANDLE}.write(&[0x9F]).ok();
//
//     // Full duplex in place: the buffer is sent, then holds the reply
//     let mut buf = [0x9F, 0x00, 0x00];
//     {HANDLE}.transfer(&mut buf).ok();
//
//     // NSS/CS is a plain GPIO here — drive it low around a transaction.

"#;

const I2C_TMPL: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const CLOCK_KHZ: u32 = {KHZ}; // <=100 Standard, >100 Fast
{ADDR}

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the peripheral has to update them; the `use` is
// here too, so the alias holds whatever you do to the `use` block below.
use stm32f1xx_hal::gpio as hal_gpio;

/// The pins I2C{N} is wired to, in HAL order: (SCL, SDA).
///
/// `allow(dead_code)`: `init` below is yours to edit and may stop naming it.
#[allow(dead_code)]
pub type I2cPins = ({PINS});
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
use stm32f1xx_hal::{
    pac,
    prelude::*,
    afio,
    rcc::Clocks,
    i2c::{self, BlockingI2c, Mode as I2cMode},
};

fn get_mode() -> I2cMode {
    if CLOCK_KHZ <= 100 {
        I2cMode::Standard { frequency: CLOCK_KHZ.kHz() }
    } else {
        I2cMode::Fast { frequency: CLOCK_KHZ.kHz(), duty_cycle: i2c::DutyCycle::Ratio2to1 }
    }
}

/// Bridges the HAL's blocking I2C (embedded-hal 0.2) to the STANDARD
/// `embedded-hal` 1.0 `I2c`, so driver/app code stays portable across HALs:
///
///     fn app<I: embedded_hal::i2c::I2c>(i2c: &mut I) { /* … */ }
pub struct I2cIo<I2C>(pub I2C);

#[derive(Debug)]
pub struct IoError;
impl embedded_hal::i2c::Error for IoError {
    fn kind(&self) -> embedded_hal::i2c::ErrorKind {
        embedded_hal::i2c::ErrorKind::Other
    }
}
impl<I2C> embedded_hal::i2c::ErrorType for I2cIo<I2C> {
    type Error = IoError;
}
impl<I2C> embedded_hal::i2c::I2c<embedded_hal::i2c::SevenBitAddress> for I2cIo<I2C>
where
    I2C: embedded_hal_0_2::blocking::i2c::Read
        + embedded_hal_0_2::blocking::i2c::Write
        + embedded_hal_0_2::blocking::i2c::WriteRead,
{
    fn transaction(
        &mut self,
        addr: u8,
        ops: &mut [embedded_hal::i2c::Operation<'_>],
    ) -> Result<(), Self::Error> {
        use embedded_hal::i2c::Operation;
        // Optimise the common `[Write, Read]` (register read) into one
        // repeated-start `write_read`; single ops map directly. Other multi-op
        // sequences fall back to per-op (each with its own START/STOP).
        match ops {
            [Operation::Write(w), Operation::Read(r)] => {
                embedded_hal_0_2::blocking::i2c::WriteRead::write_read(&mut self.0, addr, w, r)
                    .map_err(|_| IoError)
            }
            _ => {
                for op in ops.iter_mut() {
                    match op {
                        Operation::Read(r) => {
                            embedded_hal_0_2::blocking::i2c::Read::read(&mut self.0, addr, r)
                                .map_err(|_| IoError)?
                        }
                        Operation::Write(w) => {
                            embedded_hal_0_2::blocking::i2c::Write::write(&mut self.0, addr, w)
                                .map_err(|_| IoError)?
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

/// The concrete type [`init`] hands back.
///
/// A NAMED type, not `impl Trait`: a struct field — an RTIC `Local` resource,
/// say — has to name what it holds, and an opaque type cannot be named. The pins
/// stay inside the HAL's `BlockingI2c<…>`, so `I2cPins` from the GENERATED block
/// above is part of it; re-wiring I2C{N} updates this alias with it. It is still
/// an `embedded-hal` 1.0 `I2c` for every caller.
pub type Handle = I2cIo<BlockingI2c<pac::I2C{N}, I2cPins>>;

/// Initialise I2C{N} and expose it as an `embedded-hal` 1.0 `I2c`.
pub fn init(
    i2c: pac::I2C{N},
    pins: I2cPins,
    {AFIO_PARAM}: &mut afio::Parts,
    clocks: &Clocks,
) -> Handle {
    let bus = BlockingI2c::i2c{N}(i2c, pins, {AFIO_ARG}get_mode(), *clocks, 1000, 10, 1000, 1000);
    I2cIo(bus)
}

// ── Using I2C{N} ──
// Portable init — the handle is an `embedded-hal` 1.0 `I2c`. In main.rs,
// after the init above:
//
// NOTE: main.rs binds this handle as `let {HANDLE} = …` — add `mut`
// there before calling anything below (bus methods take `&mut self`).
//
//     use embedded_hal::i2c::I2c;
//
//     // Write to a register
//     {HANDLE}.write(DEVICE_ADDRESS, &[0x10, 0x42]).ok();
//
//     // Read bytes
//     let mut rx = [0u8; 2];
//     {HANDLE}.read(DEVICE_ADDRESS, &mut rx).ok();
//
//     // Register read: write the address, then read WITHOUT releasing the bus
//     // (repeated START) — what most sensors expect.
//     {HANDLE}.write_read(DEVICE_ADDRESS, &[0x10], &mut rx).ok();

"#;

fn i2c_config_file(n: u8, cfg: Option<&I2cModuleConfig>, pins: &str) -> String {
    let khz = cfg.map(|c| c.clock_hz).unwrap_or(100_000) / 1_000;
    let tmpl = match cfg.map(|c| c.api_style).unwrap_or_default() {
        ApiStyle::Portable => I2C_TMPL,
        ApiStyle::Native => I2C_TMPL_NATIVE,
    };
    // Only I2C1 can be remapped (PB8/PB9), so only `i2c1` takes the AFIO.
    let (afio_param, afio_arg) = afio_subst(n == 1);
    let sfx = cfg
        .map(|c| module_label_sfx(&c.custom_label))
        .unwrap_or_default();
    tmpl.replace("{HANDLE}", &format!("_i2c{n}{sfx}"))
        .replace("{N}", &n.to_string())
        .replace("{KHZ}", &khz.to_string())
        .replace("{PINS}", pins)
        .replace("{AFIO_PARAM}", afio_param)
        .replace("{AFIO_ARG}", afio_arg)
        .replace(
            "{ADDR}",
            &super::common::device_address_const(None, cfg.map_or(0, |c| c.primary_address())),
        )
}

/// Native `stm32f1xx-hal` I2C init (no eh-1.0 bridge). Returns `BlockingI2c<…>`.
const I2C_TMPL_NATIVE: &str = r#"// <<< GENERATED>>>
// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.
pub const CLOCK_KHZ: u32 = {KHZ}; // <=100 Standard, >100 Fast
{ADDR}

// The wired pins, straight from the MCU Configurator's pin map. They live in
// this block because re-wiring the peripheral has to update them; the `use` is
// here too, so the alias holds whatever you do to the `use` block below.
use stm32f1xx_hal::gpio as hal_gpio;

/// The pins I2C{N} is wired to, in HAL order: (SCL, SDA).
///
/// `allow(dead_code)`: `init` below is yours to edit and may stop naming it.
#[allow(dead_code)]
pub type I2cPins = ({PINS});
// <<< GENERATED END >>>

// Everything below is editable — your changes are preserved on regeneration.
// NATIVE stm32f1xx-hal API — returns `BlockingI2c<…>`. Switch to the portable
// `embedded-hal` 1.0 `I2c` API in the Virtual Module.
use stm32f1xx_hal::{
    pac,
    prelude::*,
    afio,
    rcc::Clocks,
    i2c::{self, BlockingI2c, Mode as I2cMode},
};

fn get_mode() -> I2cMode {
    if CLOCK_KHZ <= 100 {
        I2cMode::Standard { frequency: CLOCK_KHZ.kHz() }
    } else {
        I2cMode::Fast { frequency: CLOCK_KHZ.kHz(), duty_cycle: i2c::DutyCycle::Ratio2to1 }
    }
}

/// The concrete type [`init`] hands back — the HAL's own `BlockingI2c`, with the
/// pins it keeps in its type. Named so it can be a struct field (an RTIC `Local`
/// resource, say); `I2cPins` comes from the GENERATED block above, so re-wiring
/// I2C{N} updates this one with it.
pub type Handle = BlockingI2c<pac::I2C{N}, I2cPins>;

pub fn init(
    i2c: pac::I2C{N},
    pins: I2cPins,
    {AFIO_PARAM}: &mut afio::Parts,
    clocks: &Clocks,
) -> Handle {
    BlockingI2c::i2c{N}(i2c, pins, {AFIO_ARG}get_mode(), *clocks, 1000, 10, 1000, 1000)
}

// ── Using I2C{N} ──
// Native init — the concrete `stm32f1xx-hal` BlockingI2c, whose traits are
// `embedded-hal` 0.2. In main.rs, after the init above:
//
// NOTE: the Native path needs the 0.2 traits, which the IDE only adds to
// Cargo.toml for PORTABLE modules. Add them yourself:
//   embedded-hal-0-2 = { package = "embedded-hal", version = "0.2.7", features = ["unproven"] }
// — or switch this module's Init API to Portable, where the handle is an
// embedded-hal 1.0 bus and no extra dependency is needed.
//
// NOTE: main.rs binds this handle as `let {HANDLE} = …` — add `mut`
// there before calling anything below (bus methods take `&mut self`).
//
//     use embedded_hal_0_2::blocking::i2c::{Read, Write, WriteRead};
//
//     {HANDLE}.write(DEVICE_ADDRESS, &[0x10, 0x42]).ok();
//
//     let mut rx = [0u8; 2];
//     {HANDLE}.read(DEVICE_ADDRESS, &mut rx).ok();
//     {HANDLE}.write_read(DEVICE_ADDRESS, &[0x10], &mut rx).ok();

"#;

/// The section header older versions inserted before the appended helpers.
const HELPERS_HEADER: &str = "── Peripheral init helpers";

/// Helper fns older versions appended after `fn main`. All of them are dead code
/// now: USART/SPI/I2C init moved to `src/pins/configs/` and the ADC is built by
/// a plain `adc::Adc::adc1(dp.ADC1, clocks)` line inside the GENERATED block.
const OBSOLETE_HELPERS: &[&str] = &["init_usart", "init_spi", "init_i2c", "init_adc"];

/// Strip the obsolete peripheral init helpers (and their now-empty section
/// header) from an existing `main.rs`. These were generated code, so they are
/// removed whole — body included — rather than left behind as dead fns the
/// project can no longer compile (their `Clocks` parameter has no import).
pub fn strip_obsolete_helpers(file: String) -> String {
    let is_obsolete_fn = |line: &str| {
        line.trim_start().strip_prefix("fn ").is_some_and(|rest| {
            OBSOLETE_HELPERS.iter().any(|name| {
                rest.starts_with(name)
                    && rest[name.len()..].starts_with(|c: char| c.is_ascii_digit())
            })
        })
    };
    if !file.lines().any(is_obsolete_fn) {
        return file;
    }

    // Drop each helper from its `fn` line through its closing brace.
    let mut kept: Vec<&str> = Vec::new();
    let mut depth = 0i32;
    let mut in_helper = false;
    for line in file.lines() {
        if !in_helper && is_obsolete_fn(line) {
            in_helper = true;
            depth = 0;
        }
        if in_helper {
            depth += line.matches('{').count() as i32;
            depth -= line.matches('}').count() as i32;
            // `depth == 0` before the opening brace too (multi-line signature),
            // so only a line that actually closed a brace ends the helper.
            if depth <= 0 && line.contains('}') {
                in_helper = false;
            }
            continue;
        }
        kept.push(line);
    }

    // The section header is only there to introduce the helpers — drop it (and
    // the blank lines around it) once nothing follows it any more.
    if let Some(hdr) = kept.iter().position(|l| l.contains(HELPERS_HEADER)) {
        if kept[hdr + 1..].iter().all(|l| l.trim().is_empty()) {
            kept.truncate(hdr);
        }
    }
    while kept.last().is_some_and(|l| l.trim().is_empty()) {
        kept.pop();
    }

    let mut out = kept.join("\n");
    out.push('\n');
    out
}

// ── Pin helpers ───────────────────────────────────────────────────────────────

struct PinMeta {
    port: char,
    pin_num: u8,
    var: String,
    port_var: String,
    crx: &'static str,
}

fn parse_pin(name: &str) -> Option<PinMeta> {
    let bytes = name.as_bytes();
    if bytes.len() < 3 || bytes[0] != b'P' {
        return None;
    }
    let port = bytes[1] as char;
    if !port.is_ascii_uppercase() {
        return None;
    }
    let pin_num: u8 = name[2..].parse().ok()?;
    let lc = port.to_ascii_lowercase();
    Some(PinMeta {
        port,
        pin_num,
        var: format!("p{}{}", lc, pin_num),
        port_var: format!("gpio{}", lc),
        crx: if pin_num < 8 { "crl" } else { "crh" },
    })
}

/// The `.into_*(…)` call for a pin. `mode` is the user's GPIO drive/pull choice
/// (`None` = this backend's default: floating in, push-pull out) and only
/// applies to GPIO In/Out — a peripheral pin's mode is dictated by the
/// peripheral, not by the user.
fn into_expr(func: &PinFunction, mode: Option<GpioMode>, pv: &str, crx: &str) -> String {
    match func {
        // `for_input` / `for_output` and not `unwrap_or`: a mode stored while
        // the pad was the OTHER direction is still there, and `into_method`
        // would turn it into a call for that other direction under a comment
        // naming this one. The embassy backend has always matched variants for
        // the same reason (see its `GpioInput` arm); this one defaulted.
        PinFunction::GpioInput => {
            let m = GpioMode::for_input(mode).into_method();
            format!("{m}(&mut {pv}.{crx})")
        }
        PinFunction::GpioOutput => {
            let m = GpioMode::for_output(mode).into_method();
            format!("{m}(&mut {pv}.{crx})")
        }
        // Same call as an ADC channel: analog mode IS `into_analog`, it just
        // doesn't name which analog block reads the pin.
        PinFunction::GpioAnalog | PinFunction::AdcChannel { .. } => {
            format!("into_analog(&mut {pv}.{crx})")
        }
        PinFunction::TimerPwm { .. } | PinFunction::TimerPwmN { .. } => {
            format!("into_alternate_push_pull(&mut {pv}.{crx})")
        }
        PinFunction::HspiClk { .. }
        | PinFunction::HspiNcs { .. }
        | PinFunction::HspiDqs { .. }
        | PinFunction::HspiIo { .. }
        | PinFunction::XspiClk { .. }
        | PinFunction::XspiNcs { .. }
        | PinFunction::XspiDqs { .. }
        | PinFunction::XspiIo { .. }
        | PinFunction::OspiClk { .. }
        | PinFunction::OspiNcs { .. }
        | PinFunction::OspiDqs { .. }
        | PinFunction::OspiIo { .. }
        | PinFunction::QspiClk
        | PinFunction::QspiNcs { .. }
        | PinFunction::QspiIo { .. }
        | PinFunction::SdmmcCk { .. }
        | PinFunction::SdmmcCmd { .. }
        | PinFunction::SdmmcD { .. }
        | PinFunction::SaiSck { .. }
        | PinFunction::SaiSd { .. }
        | PinFunction::SaiFs { .. }
        | PinFunction::SaiMclk { .. } => {
            format!("into_alternate_push_pull(&mut {pv}.{crx})")
        }
        // An analog OUTPUT pad is still analog mode: the digital buffer is off
        // and the block drives the pin.
        // Touch is Espressif's and no STM32 pad has it. Grouped with the
        // analog pins rather than the outputs: a touch pad SENSES, so if
        // one ever did, this is the mode it would want.
        PinFunction::DacOut { .. } | PinFunction::TouchPad(..) => {
            format!("into_analog(&mut {pv}.{crx})")
        }
        // A break input is read, not driven — floating input, like any other
        // signal coming in from the board.
        PinFunction::TimerBreak { .. } => format!("into_floating_input(&mut {pv}.{crx})"),
        // RMT is Espressif's, and no STM32 pin ever carries it. Grouped with
        // the other push-pull outputs so the match stays total rather than
        // reaching a panic that could never fire.
        // PARL_IO's clock and data reverse with the port's direction, and no
        // STM32 pin carries any of them; push-pull keeps the match total.
        // LCD_CAM is an ESP32-S3 peripheral and nothing else has it, so the
        // same reasoning covers it: totality, not a real STM32 mode.
        PinFunction::LcdCamData { .. }
        | PinFunction::LcdCamDc
        | PinFunction::LcdCamWr
        | PinFunction::LcdCamCs
        | PinFunction::LcdCamPclk
        | PinFunction::LcdCamVsync
        | PinFunction::LcdCamHsync
        | PinFunction::LcdCamDe
        | PinFunction::CamData { .. }
        | PinFunction::CamPclk
        | PinFunction::CamVsync
        | PinFunction::CamHsync
        | PinFunction::CamHenable
        | PinFunction::CamMclk
        | PinFunction::ParlData { .. }
        | PinFunction::ParlClk
        | PinFunction::ParlValid
        | PinFunction::ParlRxData { .. }
        | PinFunction::ParlRxClk
        | PinFunction::ParlRxValid
        | PinFunction::RmtChannel(..)
        | PinFunction::McpwmA { .. }
        | PinFunction::McpwmB { .. } => {
            format!("into_push_pull_output(&mut {pv}.{crx})")
        }
        // PCNT reads, so its two pads are inputs — again unreachable on an
        // STM32, and again grouped rather than left to a panic.
        PinFunction::PcntEdge { .. } | PinFunction::PcntCtrl { .. } => {
            format!("into_floating_input(&mut {pv}.{crx})")
        }
        // The F1 backend has no I2S codegen; the pads still take the alternate
        // function so the pin is at least in the right mode.
        PinFunction::I2sCk(_)
        | PinFunction::I2sWs(_)
        | PinFunction::I2sSd(_)
        | PinFunction::I2sMck(_) => format!("into_alternate_push_pull(&mut {pv}.{crx})"),
        // LPUART / SPI-RDY don't exist on STM32F1; they're grouped with their
        // closest USART/SPI analogue so the mode stays sane if ever selected.
        PinFunction::UsartTx(_) | PinFunction::UsartCk(_) | PinFunction::LpuartTx(_) => {
            format!("into_alternate_push_pull(&mut {pv}.{crx})")
        }
        PinFunction::UsartRx(_)
        | PinFunction::UsartCts(_)
        | PinFunction::LpuartRx(_)
        | PinFunction::LpuartCts(_) => {
            format!("into_floating_input(&mut {pv}.{crx})")
        }
        PinFunction::UsartRts(_) | PinFunction::LpuartRts(_) => {
            format!("into_push_pull_output(&mut {pv}.{crx})")
        }
        PinFunction::SpiSck(_) | PinFunction::SpiMosi(_) => {
            format!("into_alternate_push_pull(&mut {pv}.{crx})")
        }
        PinFunction::SpiNss(_) => format!("into_push_pull_output(&mut {pv}.{crx})"),
        PinFunction::SpiMiso(_) | PinFunction::SpiRdy(_) => {
            format!("into_floating_input(&mut {pv}.{crx})")
        }
        PinFunction::I2cScl(_) | PinFunction::I2cSda(_) => {
            format!("into_alternate_open_drain(&mut {pv}.{crx})")
        }
        PinFunction::Mco => format!("into_alternate_push_pull(&mut {pv}.{crx})"),
        PinFunction::CanTx => format!("into_alternate_push_pull(&mut {pv}.{crx})"),
        PinFunction::CanRx => format!("into_floating_input(&mut {pv}.{crx})"),
        PinFunction::UsbDm | PinFunction::UsbDp => {
            "// USB — configured automatically by the USB peripheral".to_owned()
        }
        PinFunction::SwdIo | PinFunction::SwdClk => {
            "// SWD — active by default, no config needed".to_owned()
        }
        // Generic alternate function — push-pull AF is the safe default; the
        // user refines it if that peripheral needs something else.
        PinFunction::Other(_) => format!("into_alternate_push_pull(&mut {pv}.{crx})"),
        PinFunction::Unset => unreachable!(),
    }
}

fn is_comment_expr(expr: &str) -> bool {
    expr.trim_start().starts_with("//")
}

/// The remap type-state `pwm_hz` needs for TIM`timer` driving `pads`, or `None`
/// when that pin set is not one the HAL implements.
///
/// Transcribed from `remap!` in `stm32f1xx-hal`'s `timer/pins.rs`: each row is
/// `(type-state, [CH1, CH2, CH3, CH4])`. The remap is a TYPE, and it is what
/// programs the AFIO bits, so naming the wrong one drives the wrong pads — the
/// same trap `spi_remap_ty` documents for SPI1.
///
/// TIM4's rows exist only behind the HAL's `medium` feature; they are listed
/// here because a chip that has TIM4 is a chip built with that feature.
fn pwm_remap(timer: u8, pads: &[(u8, &str)]) -> Option<&'static str> {
    let rows: &[(&str, [&str; 4])] = match timer {
        1 => &[
            ("Tim1NoRemap", ["PA8", "PA9", "PA10", "PA11"]),
            ("Tim1FullRemap", ["PE9", "PE11", "PE13", "PE14"]),
        ],
        2 => &[
            ("Tim2NoRemap", ["PA0", "PA1", "PA2", "PA3"]),
            ("Tim2PartialRemap1", ["PA15", "PB3", "PA2", "PA3"]),
            ("Tim2PartialRemap2", ["PA0", "PA1", "PB10", "PB11"]),
            ("Tim2FullRemap", ["PA15", "PB3", "PB10", "PB11"]),
        ],
        3 => &[
            ("Tim3NoRemap", ["PA6", "PA7", "PB0", "PB1"]),
            ("Tim3PartialRemap", ["PB4", "PB5", "PB0", "PB1"]),
            ("Tim3FullRemap", ["PC6", "PC7", "PC8", "PC9"]),
        ],
        4 => &[
            ("Tim4NoRemap", ["PB6", "PB7", "PB8", "PB9"]),
            ("Tim4Remap", ["PD12", "PD13", "PD14", "PD15"]),
        ],
        _ => return None,
    };
    if pads.is_empty() {
        return None;
    }
    // The FIRST row every wired pad agrees with. Several can match when the
    // wiring only uses channels the rows share (TIM2 CH3/CH4 are the same pads
    // in NoRemap and PartialRemap1), and then the difference is invisible to
    // the timer — the AFIO bits still differ, so the earliest row wins for
    // being the one that leaves the other channels on their default pads.
    rows.iter()
        .find(|(_, pins)| {
            pads.iter().all(|(ch, name)| {
                (1..=4).contains(ch) && pins[usize::from(*ch) - 1].eq_ignore_ascii_case(name)
            })
        })
        .map(|(ty, _)| *ty)
}

/// Returns `true` for pins that are driven (output / alternate-output).
/// These get a `&mut` prefix so the binding is immediately usable as a
/// mutable reference without a later `&mut var` at each call site.
/// Whether an OWNED GPIO binding has to be `let mut`.
///
/// Owning the pin (instead of taking `&mut` of a temporary) means the `mut` now
/// has to be spelled out wherever the pin is written through:
/// * an **output** always — `set_high` takes `&mut self` in both `embedded-hal`
///   0.2 and 1.0;
/// * an **input** only on the Portable path — `embedded-hal` 1.0's `InputPin`
///   reads through `&mut self`, while the raw `stm32f1xx-hal` (0.2) input reads
///   through `&self`, where a `mut` would just earn an unused-mut warning;
/// * an **ADC channel** always — `OneShot::read` takes `&mut PIN`, and the very
///   line the ADC block shows commented out (`_adc1.read(&mut pa0_adc1_in0)`)
///   did not compile without it (E0596, cannot borrow as mutable);
/// * never when the pin is **moved into a Custom module** below, since the
///   binding is then consumed, never written through.
///
/// The "moved" test reads the already-rendered `custom_inits` text rather than
/// re-deriving which modules get built: that text is what actually passes the
/// binding, so the two can't disagree.
fn needs_mut_binding(
    pin: &Pin,
    gpio_native: bool,
    binding_style: Binding,
    binding: &str,
    custom_inits: &str,
) -> bool {
    if binding_style == Binding::Owned {
        return false; // RTIC moves it into a `Local`
    }
    let write_through = match pin.selected_function {
        PinFunction::GpioOutput => true,
        // An armed input is written through even on the Native API:
        // `make_interrupt_source` / `trigger_on_edge` / `enable_interrupt` all
        // take `&mut self`.
        PinFunction::GpioInput => !gpio_native || pin.irq.is_some(),
        PinFunction::AdcChannel { .. } => true,
        _ => false,
    };
    if !write_through {
        return false;
    }
    let moved = custom_inits.contains(&format!("{binding},"))
        || custom_inits.contains(&format!("{binding})"));
    !moved
}

/// The concrete `stm32f1xx-hal` type of an armed input, for the static that
/// parks it.
///
/// The MODE is part of the type: a pull-up input is `Input<PullUp>`, and naming
/// `Floating` there is a type error rather than a cosmetic slip. (The RTIC path
/// hardcodes `Floating` — that is a separate bug, not a precedent.)
fn f1_input_ty(pin: &Pin) -> Option<String> {
    let m = parse_pin(&pin.name)?;
    let mode = match pin.io_mode.unwrap_or(GpioMode::Floating) {
        GpioMode::PullUp => "PullUp",
        GpioMode::PullDown => "PullDown",
        _ => "Floating",
    };
    Some(format!(
        "gpio::gpio{lc}::P{port}{n}<gpio::Input<gpio::{mode}>>",
        lc = m.port.to_ascii_lowercase(),
        port = m.port,
        n = m.pin_num,
    ))
}

/// The module-level items a bare-metal EXTI needs: one static per armed input,
/// and one `#[interrupt]` per VECTOR.
///
/// Statics because an interrupt handler takes no arguments and cannot borrow a
/// local — the same reason the ESP blocking path needs them. One handler per
/// vector rather than per pin because that is what the NVIC gives: `EXTI9_5`
/// carries five lines, so its handler has to ask which pin fired.
///
/// `cortex_m::interrupt::Mutex`, not `critical_section::Mutex`: `cortex-m` is
/// already a dependency of every generated project here, with the
/// `critical-section-single-core` feature, so this adds nothing to Cargo.toml.
fn f1_irq_items(all_pins: &[&Pin]) -> String {
    let armed = super::rtic::irq_pins(all_pins);
    if armed.is_empty() {
        return String::new();
    }
    let ty_of = |name: &str| {
        all_pins
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| f1_input_ty(p))
            .unwrap_or_else(|| "gpio::Input<gpio::Floating>".to_owned())
    };
    let mut out = String::new();
    out.push_str("use core::cell::RefCell;\n");
    out.push_str("use cortex_m::interrupt::Mutex;\n");
    out.push_str("use stm32f1xx_hal::pac::interrupt;\n\n");
    for p in &armed {
        out.push_str(&format!(
            "/// {} — parked here so the interrupt handler can reach it.\n",
            p.name
        ));
        out.push_str(&format!(
            "static {up}: Mutex<RefCell<Option<{ty}>>> = Mutex::new(RefCell::new(None));\n",
            up = p.binding.to_ascii_uppercase(),
            ty = ty_of(&p.name),
        ));
    }
    out.push('\n');
    for (vector, group) in super::rtic::by_vector(&armed) {
        if group.len() == 1 {
            out.push_str(&format!(
                "/// {} (EXTI line {}).\n",
                group[0].name, group[0].line
            ));
        } else {
            out.push_str(&format!(
                "/// {vector} is shared by {} lines — each pin is asked in turn.\n",
                group.len()
            ));
        }
        out.push_str("#[interrupt]\n");
        out.push_str(&format!("fn {vector}() {{\n"));
        out.push_str("    cortex_m::interrupt::free(|cs| {\n");
        for p in group {
            let up = p.binding.to_ascii_uppercase();
            out.push_str(&format!(
                "        if let Some(pin) = {up}.borrow(cs).borrow_mut().as_mut() {{\n"
            ));
            // A private vector still asks: `check_interrupt` costs one register
            // read, and it keeps the two shapes identical to read.
            out.push_str("            if pin.check_interrupt() {\n");
            out.push_str("                // Cleared FIRST: leave the pending bit set and the\n");
            out.push_str("                // handler re-enters the moment it returns.\n");
            out.push_str("                pin.clear_interrupt_pending_bit();\n");
            out.push_str(&format!(
                "                // {} {} edge: your handler code here.\n",
                p.name,
                p.edge.label().to_ascii_lowercase()
            ));
            out.push_str("            }\n");
            out.push_str("        }\n");
        }
        out.push_str("    });\n}\n\n");
    }
    out
}

/// The `fn main` lines that arm each input and hand it to its static.
fn f1_irq_arming(all_pins: &[&Pin]) -> String {
    let armed = super::rtic::irq_pins(all_pins);
    if armed.is_empty() {
        return String::new();
    }
    let mut out = String::from("    // ── GPIO interrupts ──\n");
    for p in &armed {
        let b = &p.binding;
        out.push_str(&format!("    {b}.make_interrupt_source(&mut afio);\n"));
        out.push_str(&format!(
            "    {b}.trigger_on_edge(&mut dp.EXTI, {});\n",
            p.edge.hal_variant()
        ));
        out.push_str(&format!("    {b}.enable_interrupt(&mut dp.EXTI);\n"));
        // Arming the line is not enough — the NVIC still masks the vector.
        out.push_str(&format!(
            "    unsafe {{ pac::NVIC::unmask(pac::Interrupt::{}) }};\n",
            super::rtic::exti_vector(p.line)
        ));
        out.push_str(&format!(
            "    cortex_m::interrupt::free(|cs| {up}.borrow(cs).replace(Some({b})));\n",
            up = b.to_ascii_uppercase(),
        ));
    }
    out
}

/// Whether main.rs passes this pin as `&mut …` instead of by value.
///
/// The bus arms below are commented out on purpose (see the `//cd` marks): every
/// `pins::configs::*::init` takes its pins BY VALUE, because that is how the
/// HAL's `Pins` impls are written. `CanTx` used to be the exception and it was
/// simply wrong — `can::Pins` is implemented for the owned `(PA12<Alternate>,
/// PA11<Input>)` pair, so the reference did not satisfy the bound and the CAN
/// project did not compile.
fn needs_mut_ref(func: &PinFunction) -> bool {
    matches!(
        func,
        PinFunction::GpioOutput
          //  | PinFunction::UsartTx(_)
            | PinFunction::UsartCk(_)
            | PinFunction::UsartRts(_)
           //cd  | PinFunction::SpiSck(_)
           // | PinFunction::SpiMosi(_)
           // | PinFunction::SpiNss(_)
           // | PinFunction::I2cScl(_)
           // | PinFunction::I2cSda(_)
            | PinFunction::Mco //cd  | PinFunction::CanTx
                               //cd  | PinFunction::TimerPwm { .. }
    )
}

#[cfg(test)]
mod blocking_dma_tests {
    use super::*;
    use crate::panels::mcu_module::codegen::dma_map::Bus;

    /// The channel tables are transcribed from `serialdma!` / `spi_dma!` in
    /// stm32f1xx-hal, which puts the channel in the TYPE. Get one wrong and the
    /// generated `init` signature stops matching the alias it returns, so pin
    /// them: this is the fact the feature rests on, and it is not derivable.
    #[test]
    fn the_channels_are_the_ones_the_hal_fixes() {
        assert_eq!(usart_dma_channels(1), Some(("dma1::C4", "dma1::C5")));
        assert_eq!(usart_dma_channels(2), Some(("dma1::C7", "dma1::C6")));
        assert_eq!(usart_dma_channels(3), Some(("dma1::C2", "dma1::C3")));
        assert_eq!(usart_dma_channels(4), None, "UART4 has no DMA in this HAL");
        assert_eq!(spi_dma_channels(1), Some(("dma1::C2", "dma1::C3")));
        assert_eq!(spi_dma_channels(2), Some(("dma1::C4", "dma1::C5")));
        assert_eq!(
            spi_dma_channels(3),
            None,
            "SPI3 is on DMA2, connectivity only"
        );
    }

    /// `dma1::C4` is what `init` declares; `dma1.4` is what main.rs passes.
    #[test]
    fn the_type_and_the_value_are_spelled_differently() {
        assert_eq!(channel_field("dma1::C4"), "dma1.4");
        assert_eq!(channel_field("dma2::C1"), "dma2.1");
    }

    /// The picker answers from the same tables, so it can never offer a
    /// transport the templates would decline to emit.
    #[test]
    fn only_f1_usart_and_spi_offer_the_transport() {
        assert!(blocking_dma_channels("stm32f1", Bus::Usart, 1).is_some());
        assert!(blocking_dma_channels("stm32f1", Bus::Spi, 2).is_some());
        // stm32f1xx-hal has no I2C DMA at all.
        assert!(blocking_dma_channels("stm32f1", Bus::I2c, 1).is_none());
        // Instances the HAL leaves out.
        assert!(blocking_dma_channels("stm32f1", Bus::Usart, 4).is_none());
        assert!(blocking_dma_channels("stm32f1", Bus::Spi, 3).is_none());
        // Other families run on embassy - a different DMA story entirely.
        for family in ["stm32f4", "stm32g0", "esp32c3"] {
            assert!(
                blocking_dma_channels(family, Bus::Usart, 1).is_none(),
                "{family}"
            );
        }
        // And the label names the channels, so the checkbox is not a black box.
        assert_eq!(
            blocking_dma_channels("stm32f1", Bus::Usart, 1).unwrap(),
            "dma1::C4 TX / dma1::C5 RX"
        );
    }

    /// The flag selects a THIRD template, not a third API style: the handles
    /// are the HAL's own DMA types, so Portable/Native has nothing to choose.
    #[test]
    fn the_flag_switches_the_template_whatever_the_api_style() {
        let dma_cfg = |style| UsartModuleConfig {
            blocking_dma: BlockingDma::Both,
            api_style: style,
            ..UsartModuleConfig::new(1)
        };
        for style in [ApiStyle::Portable, ApiStyle::Native] {
            let f = usart_config_file(1, Some(&dma_cfg(style)));
            assert!(f.contains("pub type TxHandle = serial::TxDma1;"), "{f}");
            assert!(
                f.contains("tx_ch: dma1::C4,") && f.contains("rx_ch: dma1::C5,"),
                "{f}"
            );
            assert!(f.contains("tx.with_dma(tx_ch)"), "{f}");
            // Neither of the two non-DMA shapes leaks in.
            assert!(!f.contains("embedded_io"), "{f}");
        }
        // Off: exactly what it generated before.
        let off = usart_config_file(1, Some(&UsartModuleConfig::new(1)));
        assert!(!off.contains("with_dma"), "{off}");

        let spi = spi_config_file(
            2,
            Some(&SpiModuleConfig {
                blocking_dma: BlockingDma::Both,
                ..SpiModuleConfig::new(2)
            }),
            &("()".into(), "Remap".into()),
            true,
        );
        assert!(
            spi.contains("Spi2RxTxDma<SpiRemap, SpiPins, hal_spi::Master>"),
            "{spi}"
        );
        assert!(spi.contains("with_rx_tx_dma(rx_ch, tx_ch)"), "{spi}");
        assert!(
            spi.contains("rx_ch: dma1::C4,") && spi.contains("tx_ch: dma1::C5,"),
            "{spi}"
        );
    }

    /// The four states are four different sets of HAL types, and the file has
    /// to match the one it claims: `TxDma1` has `write`, `Tx<USART1>` does not.
    #[test]
    fn each_half_gets_its_own_type_and_its_own_parameter() {
        let cfg = |dma| UsartModuleConfig {
            blocking_dma: dma,
            ..UsartModuleConfig::new(1)
        };

        let both = usart_config_file(1, Some(&cfg(BlockingDma::Both)));
        assert!(
            both.contains("pub type TxHandle = serial::TxDma1;"),
            "{both}"
        );
        assert!(
            both.contains("pub type RxHandle = serial::RxDma1;"),
            "{both}"
        );
        assert!(
            both.contains("tx_ch: dma1::C4,") && both.contains("rx_ch: dma1::C5,"),
            "{both}"
        );
        assert!(
            both.contains("(tx.with_dma(tx_ch), rx.with_dma(rx_ch))"),
            "{both}"
        );

        // RX only: the TX half is the HAL's plain `nb` one, and `init` takes no
        // TX channel at all - that channel stays free for another peripheral.
        let rx = usart_config_file(1, Some(&cfg(BlockingDma::Rx)));
        assert!(
            rx.contains("pub type TxHandle = serial::Tx<pac::USART1>;"),
            "{rx}"
        );
        assert!(rx.contains("pub type RxHandle = serial::RxDma1;"), "{rx}");
        assert!(!rx.contains("tx_ch"), "no TX channel anywhere:\n{rx}");
        assert!(rx.contains("rx_ch: dma1::C5,"), "{rx}");
        assert!(rx.contains("(tx, rx.with_dma(rx_ch))"), "{rx}");
        // ...and the example teaches the halves it really has.
        assert!(rx.contains("writeln!(_tx1, \"hello\").ok();"), "{rx}");
        assert!(rx.contains("use stm32f1xx_hal::dma::ReadDma;"), "{rx}");
        assert!(!rx.contains("WriteDma"), "{rx}");

        let tx = usart_config_file(1, Some(&cfg(BlockingDma::Tx)));
        assert!(tx.contains("pub type TxHandle = serial::TxDma1;"), "{tx}");
        assert!(
            tx.contains("pub type RxHandle = serial::Rx<pac::USART1>;"),
            "{tx}"
        );
        assert!(!tx.contains("rx_ch"), "{tx}");
        assert!(tx.contains("(tx.with_dma(tx_ch), rx)"), "{tx}");
        assert!(tx.contains("block!(_rx1.read())"), "{tx}");

        // Off keeps the old template entirely.
        let off = usart_config_file(1, Some(&cfg(BlockingDma::Off)));
        assert!(!off.contains("with_dma"), "{off}");
    }

    /// SPI's three DMA shapes are three TYPES, each with one method.
    #[test]
    fn the_spi_handle_type_says_which_halves_are_on_dma() {
        let cfg = |dma| SpiModuleConfig {
            blocking_dma: dma,
            ..SpiModuleConfig::new(1)
        };
        let pins = ("()".to_owned(), "Remap".to_owned());

        let both = spi_config_file(1, Some(&cfg(BlockingDma::Both)), &pins, true);
        assert!(
            both.contains("Spi1RxTxDma<SpiRemap, SpiPins, hal_spi::Master>"),
            "{both}"
        );
        assert!(both.contains(".with_rx_tx_dma(rx_ch, tx_ch)"), "{both}");
        assert!(both.contains("read_write(rxbuf, txbuf)"), "{both}");

        let tx = spi_config_file(1, Some(&cfg(BlockingDma::Tx)), &pins, true);
        assert!(
            tx.contains("Spi1TxDma<SpiRemap, SpiPins, hal_spi::Master>"),
            "{tx}"
        );
        assert!(tx.contains(".with_tx_dma(tx_ch)"), "{tx}");
        assert!(!tx.contains("rx_ch"), "{tx}");
        assert!(tx.contains("use stm32f1xx_hal::dma::WriteDma;"), "{tx}");

        let rx = spi_config_file(1, Some(&cfg(BlockingDma::Rx)), &pins, true);
        assert!(
            rx.contains("Spi1RxDma<SpiRemap, SpiPins, hal_spi::Master>"),
            "{rx}"
        );
        assert!(rx.contains(".with_rx_dma(rx_ch)"), "{rx}");
        assert!(!rx.contains("tx_ch"), "{rx}");
        assert!(rx.contains("use stm32f1xx_hal::dma::ReadDma;"), "{rx}");
    }

    /// An SPI wired SCK+MOSI is a TRANSMITTER. `stm32f1xx-hal` would happily
    /// build `with_rx_dma` on it — `NoMiso` is a type-state placeholder, not a
    /// narrower type — and burn a channel receiving an unconfigured pad.
    #[test]
    fn without_miso_the_receive_half_is_dropped() {
        let cfg = |dma| SpiModuleConfig {
            blocking_dma: dma,
            ..SpiModuleConfig::new(1)
        };
        let pins = ("()".to_owned(), "Remap".to_owned());

        // Both narrows to TX: one channel, one method, and the example no
        // longer promises a reply.
        let both = spi_config_file(1, Some(&cfg(BlockingDma::Both)), &pins, false);
        assert!(
            both.contains("Spi1TxDma<SpiRemap, SpiPins, hal_spi::Master>"),
            "{both}"
        );
        assert!(both.contains(".with_tx_dma(tx_ch)"), "{both}");
        assert!(!both.contains("rx_ch"), "{both}");
        assert!(!both.contains("read_write"), "{both}");

        // RX alone has nothing left, so the file is the polled one again —
        // carrying the note that says why `read` would lie.
        let rx = spi_config_file(1, Some(&cfg(BlockingDma::Rx)), &pins, false);
        assert!(!rx.contains("with_rx_dma"), "{rx}");
        assert!(!rx.contains("dma1::C2"), "{rx}");
        assert!(
            rx.contains("MISO is not wired: this bus can only SEND"),
            "{rx}"
        );

        // With MISO wired, the note stays out of the way.
        let full = spi_config_file(1, Some(&cfg(BlockingDma::Off)), &pins, true);
        assert!(!full.contains("MISO is not wired"), "{full}");
    }

    /// Neither the USART nor the I2C can be built from one pad on this HAL:
    /// `serial::Pins<USART>` and `i2c::Pins<I2C>` are implemented for their
    /// PAIRS alone. The USART used to be built anyway, naming a `_rx1` binding
    /// nothing declared — the project did not compile. The I2C already refused,
    /// but silently.
    #[test]
    fn half_a_paired_bus_is_not_initialised_and_says_so() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::modules::ModuleConfig;

        // `wired` names the pads to configure; the other one stays Unset.
        let build = |wired: &[(&str, PinFunction)], dma| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            for (name, func) in wired {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| p.name == *name)
                    .map(|p| p.number);
                if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                    p.selected_function = func.clone();
                }
            }
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let ModuleConfig::Usart(c) = &mut m.config {
                    c.blocking_dma = dma;
                }
            }
            mcu
        };
        let tx = ("PA9", PinFunction::UsartTx(1));
        let rx = ("PA10", PinFunction::UsartRx(1));

        for (wired, missing) in [(vec![tx.clone()], "RX"), (vec![rx.clone()], "TX")] {
            // Even with both halves asked for, an uninitialised USART takes no
            // channel and leaves no config file behind.
            let mcu = build(&wired, BlockingDma::Both);
            // The module SURVIVES one pad (`reconcile_modules` keeps a module
            // while its peripheral has any pin), so it really can be sitting
            // there asking for DMA — the guard is not decoration.
            assert!(
                mcu.modules
                    .iter()
                    .any(|m| matches!(&m.config, ModuleConfig::Usart(c) if c.blocking_dma.any())),
                "half a USART still has a module"
            );
            let main_rs = mcu.fresh_main_rs();
            assert!(!main_rs.contains("configs::usart1::init"), "{main_rs}");
            assert!(
                main_rs.contains(&format!(
                    "USART1 is NOT initialised: {missing} is not wired"
                )),
                "{main_rs}"
            );
            assert!(!main_rs.contains("dp.DMA1.split()"), "{main_rs}");
            assert!(blocking_dma_uses(&mcu).is_empty());
            assert!(
                !mcu.config_files().iter().any(|(f, _)| f == "usart1.rs"),
                "no init to offer, so no file"
            );
        }

        // Both pads: the ordinary USART comes back, file and channels included.
        let mcu = build(&[tx, rx], BlockingDma::Both);
        let main_rs = mcu.fresh_main_rs();
        assert!(main_rs.contains("configs::usart1::init"), "{main_rs}");
        assert!(!main_rs.contains("is NOT initialised"), "{main_rs}");
        assert_eq!(blocking_dma_uses(&mcu).len(), 2);
        assert!(mcu.config_files().iter().any(|(f, _)| f == "usart1.rs"));

        // Half an I2C already refused to build — `i2c::Pins` is the (SCL, SDA)
        // pair too, and one wire is not a bus in any reading. What it lacked was
        // the reason: a configured pad and no peripheral to account for it.
        let scl = ("PB6", PinFunction::I2cScl(1));
        let sda = ("PB7", PinFunction::I2cSda(1));
        for (wired, missing) in [(vec![scl.clone()], "SDA"), (vec![sda.clone()], "SCL")] {
            let mcu = build(&wired, BlockingDma::Off);
            let main_rs = mcu.fresh_main_rs();
            assert!(!main_rs.contains("configs::i2c1::init"), "{main_rs}");
            assert!(
                main_rs.contains(&format!("I2C1 is NOT initialised: {missing} is not wired")),
                "{main_rs}"
            );
            assert!(
                !mcu.config_files().iter().any(|(f, _)| f == "i2c1.rs"),
                "no init to offer, so no file"
            );
        }

        let mcu = build(&[scl, sda], BlockingDma::Off);
        let main_rs = mcu.fresh_main_rs();
        assert!(main_rs.contains("configs::i2c1::init"), "{main_rs}");
        assert!(!main_rs.contains("is NOT initialised"), "{main_rs}");
        assert!(mcu.config_files().iter().any(|(f, _)| f == "i2c1.rs"));

        // And the CAN, the third pair — `can::Pins` is (PA12, PA11) or the
        // PB9/PB8 remap. This one had the USART's bug: `_can_rx` / `_can_tx`.
        let can_tx = ("PA12", PinFunction::CanTx);
        let can_rx = ("PA11", PinFunction::CanRx);
        for (wired, missing) in [(vec![can_tx.clone()], "RX"), (vec![can_rx.clone()], "TX")] {
            let mcu = build(&wired, BlockingDma::Off);
            let main_rs = mcu.fresh_main_rs();
            assert!(!main_rs.contains("configs::can1::init"), "{main_rs}");
            assert!(
                main_rs.contains(&format!("CAN1 is NOT initialised: {missing} is not wired")),
                "{main_rs}"
            );
            // The phantom bindings, as they appeared in the argument list —
            // `pa11_can_rx` is a REAL binding and ends the same way, so match
            // the punctuation around them.
            assert!(
                !main_rs.contains(", _can_rx)") && !main_rs.contains("(_can_tx,"),
                "{main_rs}"
            );
            assert!(!mcu.config_files().iter().any(|(f, _)| f == "can1.rs"));
        }

        let mcu = build(&[can_tx, can_rx], BlockingDma::Off);
        let main_rs = mcu.fresh_main_rs();
        // The USB token is not optional: bxCAN and USB share SRAM, so
        // `Can::new` takes it to prove nothing else holds it.
        assert!(
            main_rs.contains("configs::can1::init(dp.CAN1, dp.USB, (pa12_can_tx, pa11_can_rx)"),
            "{main_rs}"
        );
        // And the pins go BY VALUE — `can::Pins` is implemented for the owned
        // pair, so a `&mut` here does not satisfy the bound.
        assert!(!main_rs.contains("&mut gpioa.pa12"), "{main_rs}");
        assert!(mcu.config_files().iter().any(|(f, _)| f == "can1.rs"));

        // USB, on the SAME two pads, is the odd one out: its init never read
        // the pin map, it took `gpioa.pa11`/`gpioa.pa12` outright. One wired pad
        // therefore produced a whole two-pad device — and if the user had spent
        // the other pad, a moved-value error.
        let dm = ("PA11", PinFunction::UsbDm);
        let dp = ("PA12", PinFunction::UsbDp);
        for (wired, missing) in [
            (vec![dm.clone()], "D+ (PA12)"),
            (vec![dp.clone()], "D- (PA11)"),
        ] {
            let mcu = build(&wired, BlockingDma::Off);
            let main_rs = mcu.fresh_main_rs();
            assert!(!main_rs.contains("UsbBus::new"), "{main_rs}");
            assert!(
                main_rs.contains(&format!("USB is NOT initialised: {missing} is not wired")),
                "{main_rs}"
            );
            // Neither pad is touched by a USB block that is not there.
            assert!(
                !main_rs.contains("let mut usb_dp = gpioa.pa12"),
                "{main_rs}"
            );
            assert!(!main_rs.contains("pin_dm: gpioa.pa11"), "{main_rs}");
            // …and the `use`s that only the USB block needs stay out too.
            assert!(!main_rs.contains("usbd_serial"), "{main_rs}");
        }

        let mcu = build(&[dm, dp], BlockingDma::Off);
        let main_rs = mcu.fresh_main_rs();
        assert!(main_rs.contains("UsbBus::new(usb_periph)"), "{main_rs}");
        assert!(!main_rs.contains("is NOT initialised"), "{main_rs}");
        assert!(main_rs.contains("usbd_serial"), "{main_rs}");
        // The device is `mut` for `poll` and unused until the reader writes
        // that poll — and these two lines are INSIDE the generated block, so a
        // warning there is one the reader cannot answer. Scoped to the
        // statement, so it goes inert rather than hiding anything later.
        assert_eq!(
            main_rs
                .matches("#[allow(unused_mut, unused_variables)]")
                .count(),
            2,
            "{main_rs}"
        );
    }

    /// The remap table is transcribed from `remap!` in the HAL's
    /// `timer/pins.rs`, and it is what the generated example turns on: the
    /// type-state programs the AFIO bits, so the wrong one drives the wrong
    /// pads. Not derivable — pin it.
    #[test]
    fn the_pwm_remap_is_the_one_the_hal_implements() {
        // The default sets.
        assert_eq!(pwm_remap(2, &[(1, "PA0")]), Some("Tim2NoRemap"));
        assert_eq!(pwm_remap(3, &[(1, "PA6"), (2, "PA7")]), Some("Tim3NoRemap"));
        assert_eq!(pwm_remap(1, &[(4, "PA11")]), Some("Tim1NoRemap"));
        assert_eq!(pwm_remap(4, &[(3, "PB8")]), Some("Tim4NoRemap"));

        // A remapped set is a DIFFERENT type-state, chosen by the pads.
        assert_eq!(pwm_remap(3, &[(1, "PB4")]), Some("Tim3PartialRemap"));
        assert_eq!(
            pwm_remap(3, &[(1, "PC6"), (4, "PC9")]),
            Some("Tim3FullRemap")
        );
        assert_eq!(
            pwm_remap(2, &[(1, "PA15"), (3, "PB10")]),
            Some("Tim2FullRemap")
        );
        // CH3/CH4 alone are the same pads in NoRemap and PartialRemap1, and the
        // earliest row wins — it leaves CH1/CH2 on their default pads.
        assert_eq!(pwm_remap(2, &[(3, "PA2"), (4, "PA3")]), Some("Tim2NoRemap"));

        // CH1 on its default pad with CH3 remapped is a row of its own — the
        // four TIM2 rows are combinations, not a NoRemap/FullRemap pair.
        assert_eq!(
            pwm_remap(2, &[(1, "PA0"), (3, "PB10")]),
            Some("Tim2PartialRemap2")
        );

        // A wiring that mixes two sets has no answer — PA0 is CH1 only in
        // NoRemap/PartialRemap2, PB3 is CH2 only in PartialRemap1/FullRemap,
        // and no row holds both. So does a timer the HAL has no remap for.
        assert_eq!(pwm_remap(2, &[(1, "PA0"), (2, "PB3")]), None);
        assert_eq!(pwm_remap(5, &[(1, "PA0")]), None);
        assert_eq!(pwm_remap(2, &[]), None);
    }

    /// An armed input off the RTIC path becomes a bare-metal `#[interrupt]` over
    /// a static — a different shape from the hardware task, same HAL sequence.
    #[test]
    fn an_armed_input_becomes_an_interrupt_over_a_static() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::pins::logic::pin::Edge;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        let num = mcu
            .iter_all_pins()
            .find(|p| p.name == "PB1")
            .map(|p| p.number);
        if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
            p.selected_function = PinFunction::GpioInput;
            p.irq = Some(Edge::Rising);
        }
        let out = mcu.fresh_main_rs();

        // `ExtiPin` is not in the HAL's prelude — the import has to be earned.
        assert!(out.contains("gpio::{self, Edge, ExtiPin},"), "{out}");
        assert!(out.contains("use cortex_m::interrupt::Mutex;"), "{out}");
        // The static carries the pin's REAL type, mode included.
        assert!(
            out.contains(
                "static PB1_IN: Mutex<RefCell<Option<gpio::gpiob::PB1<gpio::Input<gpio::Floating>>>>>"
            ),
            "{out}"
        );
        assert!(out.contains("#[interrupt]\nfn EXTI1() {"), "{out}");
        assert!(out.contains("pin.clear_interrupt_pending_bit();"), "{out}");
        // The whole arming sequence, in order — and the NVIC, which arming the
        // line alone does not touch.
        assert!(
            out.contains("pb1_in.make_interrupt_source(&mut afio);"),
            "{out}"
        );
        assert!(
            out.contains("pb1_in.trigger_on_edge(&mut dp.EXTI, Edge::Rising);"),
            "{out}"
        );
        assert!(
            out.contains("pb1_in.enable_interrupt(&mut dp.EXTI);"),
            "{out}"
        );
        assert!(
            out.contains("unsafe { pac::NVIC::unmask(pac::Interrupt::EXTI1) };"),
            "{out}"
        );
        assert!(
            out.contains(
                "cortex_m::interrupt::free(|cs| PB1_IN.borrow(cs).replace(Some(pb1_in)));"
            ),
            "{out}"
        );
        // `&mut dp.EXTI` needs a mutable `dp`, and the pin needs a mutable
        // binding — every `ExtiPin` method takes `&mut self`.
        assert!(
            out.contains("let mut dp = pac::Peripherals::take()"),
            "{out}"
        );
        assert!(out.contains("let mut pb1_in = "), "{out}");
        // AFIO is pulled in by the arming even with no bus wired: the interrupt
        // source multiplexer lives there.
        assert!(out.contains("let mut afio = dp.AFIO.constrain();"), "{out}");
    }

    /// Lines that share a vector share a handler, which asks each pin in turn —
    /// the NVIC gives one `EXTI9_5` for five lines and there is no dividing it.
    #[test]
    fn lines_sharing_a_vector_share_the_handler() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::pins::logic::pin::Edge;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        for (name, edge) in [("PB5", Edge::Rising), ("PB6", Edge::Falling)] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = PinFunction::GpioInput;
                p.irq = Some(edge);
            }
        }
        let out = mcu.fresh_main_rs();

        assert_eq!(out.matches("#[interrupt]").count(), 1, "{out}");
        assert!(out.contains("fn EXTI9_5() {"), "{out}");
        assert!(out.contains("PB5_IN.borrow(cs)"), "{out}");
        assert!(out.contains("PB6_IN.borrow(cs)"), "{out}");
        // Each keeps its own edge.
        assert!(
            out.contains("pb5_in.trigger_on_edge(&mut dp.EXTI, Edge::Rising);"),
            "{out}"
        );
        assert!(
            out.contains("pb6_in.trigger_on_edge(&mut dp.EXTI, Edge::Falling);"),
            "{out}"
        );
    }

    /// An input with no edge is one you poll: nothing armed, nothing imported,
    /// and `dp` stays immutable.
    #[test]
    fn an_unarmed_input_arms_nothing_on_f1() {
        use crate::panels::mcu_module::builtins::builtin_for;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        let num = mcu
            .iter_all_pins()
            .find(|p| p.name == "PB1")
            .map(|p| p.number);
        if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
            p.selected_function = PinFunction::GpioInput;
        }
        let out = mcu.fresh_main_rs();
        assert!(!out.contains("#[interrupt]"), "{out}");
        assert!(!out.contains("ExtiPin"), "{out}");
        assert!(!out.contains("cortex_m::interrupt::Mutex"), "{out}");
        assert!(out.contains("let dp = pac::Peripherals::take()"), "{out}");
    }

    /// The Virtual Module owns ONE frequency (it is one prescaler in silicon)
    /// and a duty PER CHANNEL. Both have to reach the code, or the sliders are
    /// decoration — for a long time on this family they were.
    ///
    /// They reach `pins/configs/pwm{N}.rs` now, not `main.rs`: the numbers are
    /// consts in the generated block, so they are editable in the one place the
    /// module rewrites.
    #[test]
    fn the_timer_module_drives_the_generated_pwm() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::modules::ModuleConfig;

        let build = |pins: &[(&str, PinFunction)], set: bool| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            for (name, func) in pins {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| p.name == *name)
                    .map(|p| p.number);
                if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                    p.selected_function = func.clone();
                }
            }
            mcu.reconcile_modules();
            if set {
                for m in &mut mcu.modules {
                    if let ModuleConfig::Timer(c) = &mut m.config {
                        c.freq_hz = 20_000;
                        c.set_duty_x100(3, 7_550);
                    }
                }
            }
            let file = mcu
                .config_files()
                .into_iter()
                .find(|(n, _)| n == "pwm2.rs")
                .map(|(_, b)| b)
                .unwrap_or_default();
            (mcu.fresh_main_rs(), file)
        };
        let ch3 = (
            "PA2",
            PinFunction::TimerPwm {
                timer: 2,
                channel: 3,
            },
        );
        let ch4 = (
            "PA3",
            PinFunction::TimerPwm {
                timer: 2,
                channel: 4,
            },
        );

        let (main_rs, pwm2) = build(&[ch3.clone(), ch4.clone()], true);
        // main.rs binds the pads and calls init — that is all it does now.
        assert!(
            main_rs.contains(
                "let mut _pwm2 = pins::configs::pwm2::init(dp.TIM2, \
                 (pa2_tim2_ch3, pa3_tim2_ch4), &mut afio, &clocks);"
            ),
            "{main_rs}"
        );
        // …and it no longer imports the timer items it used to name.
        assert!(!main_rs.contains("timer::{"), "{main_rs}");
        assert!(!main_rs.contains(".pwm_hz::<"), "{main_rs}");

        // The config file carries the settings, as editable consts.
        assert!(
            pwm2.contains("pub const FREQUENCY_HZ: u32 = 20000;"),
            "{pwm2}"
        );
        // A channel the user set, and one they never touched — 0 %, enabled,
        // pin low, which is the safe state the model documents. The duty is a
        // ratio out of 10_000, so a fraction of a percent reaches the pin
        // instead of being rounded to the nearest whole one.
        assert!(
            pwm2.contains("pub const DUTY_CH3_X100: u32 = 7550; // 75.5 %"),
            "{pwm2}"
        );
        assert!(
            pwm2.contains("pub const DUTY_CH4_X100: u32 = 0; // 0 %"),
            "{pwm2}"
        );
        assert!(
            pwm2.contains(
                "pwm.set_duty(Channel::C3, (max as u32 * DUTY_CH3_X100 / 10_000) as u16);"
            ),
            "{pwm2}"
        );
        assert!(pwm2.contains("pwm.enable(Channel::C4);"), "{pwm2}");
        // The remap and the pins are type ALIASES in the generated block, so
        // re-wiring the timer rewrites them and the `Handle` moves with them.
        assert!(
            pwm2.contains("pub type PwmRemap = stm32f1xx_hal::timer::Tim2NoRemap;"),
            "{pwm2}"
        );
        assert!(
            pwm2.contains(
                "pub type PwmPins = (hal_gpio::PA2<hal_gpio::Alternate>, \
                 hal_gpio::PA3<hal_gpio::Alternate>);"
            ),
            "{pwm2}"
        );
        assert!(
            pwm2.contains("pub type Handle = PwmHz<pac::TIM2, PwmRemap, (Ch<2>, Ch<3>), PwmPins>;"),
            "{pwm2}"
        );

        // Defaults when the module has not been touched: 1 kHz, every duty 0.
        let (main_rs, pwm2) = build(&[ch3.clone()], false);
        assert!(
            pwm2.contains("pub const FREQUENCY_HZ: u32 = 1000;"),
            "{pwm2}"
        );
        // ONE channel is not a 1-tuple — the HAL's `Pins` impl for a single pin
        // is the pin itself, and the channel marker follows.
        assert!(
            main_rs.contains("pins::configs::pwm2::init(dp.TIM2, pa2_tim2_ch3,"),
            "{main_rs}"
        );
        assert!(
            pwm2.contains("pub type PwmPins = hal_gpio::PA2<hal_gpio::Alternate>;"),
            "{pwm2}"
        );
        assert!(
            pwm2.contains("pub type Handle = PwmHz<pac::TIM2, PwmRemap, Ch<2>, PwmPins>;"),
            "{pwm2}"
        );

        // Pads from two different remap sets have no type-state, so nothing is
        // generated — not a call and not a file — and the pads stay bound, so
        // the compiler names them too.
        let (main_rs, pwm2) = build(
            &[
                (
                    "PA0",
                    PinFunction::TimerPwm {
                        timer: 2,
                        channel: 1,
                    },
                ),
                (
                    "PB3",
                    PinFunction::TimerPwm {
                        timer: 2,
                        channel: 2,
                    },
                ),
            ],
            false,
        );
        assert!(pwm2.is_empty(), "no type-state, no file:\n{pwm2}");
        assert!(!main_rs.contains("pins::configs::pwm2::init"), "{main_rs}");
        assert!(
            main_rs.contains("TIM2 CH1+CH2 is NOT initialised"),
            "{main_rs}"
        );
    }

    /// A GPIO pin is declared for the reader's loop, which a fresh project does
    /// not have yet — so it warns twice, on a line inside the generated block
    /// that the reader cannot edit. A peripheral pin is NOT given the same
    /// allow: a warning on one of those means its bus was never generated, and
    /// that is worth seeing.
    #[test]
    fn a_gpio_pin_is_allowed_to_be_unused_but_an_orphaned_bus_pad_is_not() {
        use crate::panels::mcu_module::builtins::builtin_for;

        let wire = |pins: &[(&str, PinFunction)]| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            for (name, func) in pins {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| p.name == *name)
                    .map(|p| p.number);
                if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                    p.selected_function = func.clone();
                }
            }
            mcu.reconcile_modules();
            mcu.fresh_main_rs()
        };
        const ALLOW: &str = "#[allow(unused_mut, unused_variables)]";

        let main_rs = wire(&[
            ("PC13", PinFunction::GpioOutput),
            ("PB1", PinFunction::GpioInput),
        ]);
        assert_eq!(main_rs.matches(ALLOW).count(), 2, "{main_rs}");

        // Analog pins are the same case: the ADC itself IS generated, and the
        // pin waits for the read the block shows commented out. That read takes
        // `&mut PIN`, so the channel binding must be `mut` — without it the
        // advertised line does not compile (E0596).
        let main_rs = wire(&[
            ("PA0", PinFunction::AdcChannel { adc: 1, channel: 0 }),
            ("PA1", PinFunction::GpioAnalog),
        ]);
        assert_eq!(main_rs.matches(ALLOW).count(), 2, "{main_rs}");
        assert!(main_rs.contains("let mut pa0_adc1_in0"), "{main_rs}");
        assert!(
            main_rs.contains("_adc1.read(&mut pa0_adc1_in0)"),
            "{main_rs}"
        );

        // A half-wired USART leaves its pad bound and unused — and says so
        // twice over: the generated comment, and the warning the reader gets.
        let main_rs = wire(&[("PA9", PinFunction::UsartTx(1))]);
        assert!(main_rs.contains("let pa9_usart1_tx"), "{main_rs}");
        assert!(!main_rs.contains(ALLOW), "{main_rs}");

        // A COMPLETE bus consumes its pads, so nothing is unused either way.
        let main_rs = wire(&[
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
        ]);
        assert!(!main_rs.contains(ALLOW), "{main_rs}");
    }

    /// The Configuration tab's list must show the halves actually taken -
    /// reporting a channel the project left free is the drift the list exists
    /// to avoid.
    #[test]
    fn only_the_halves_in_use_are_reported() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::modules::ModuleConfig;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        mcu.reconcile_modules();
        let set = |mcu: &mut crate::panels::mcu_module::Mcu, d| {
            for m in &mut mcu.modules {
                if let ModuleConfig::Usart(c) = &mut m.config {
                    c.blocking_dma = d;
                }
            }
        };

        set(&mut mcu, BlockingDma::Rx);
        let rows: Vec<(String, String)> = blocking_dma_uses(&mcu)
            .into_iter()
            .map(|u| (u.peri, u.user))
            .collect();
        assert_eq!(rows, [("DMA1_CH5".to_owned(), "USART1 RX".to_owned())]);

        set(&mut mcu, BlockingDma::Tx);
        assert_eq!(
            blocking_dma_uses(&mcu)
                .into_iter()
                .map(|u| u.peri)
                .collect::<Vec<_>>(),
            ["DMA1_CH4"]
        );

        set(&mut mcu, BlockingDma::Off);
        assert!(blocking_dma_uses(&mcu).is_empty());
    }

    /// The Configuration tab's list, on the F1 blocking path: the same two
    /// tables the templates read, spelled the way every other family spells a
    /// channel. The interrupt column is empty because nothing generated names
    /// one here - the HAL owns it.
    #[test]
    fn the_blocking_path_reports_what_it_takes() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::modules::ModuleConfig;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        mcu.reconcile_modules();
        // Nothing on DMA yet: an empty list, not a wrong one.
        assert!(blocking_dma_uses(&mcu).is_empty());

        for m in &mut mcu.modules {
            match &mut m.config {
                ModuleConfig::Usart(c) => c.blocking_dma = BlockingDma::Both,
                ModuleConfig::Spi(c) => c.blocking_dma = BlockingDma::Both,
                _ => {}
            }
        }
        let uses = blocking_dma_uses(&mcu);
        let rows: Vec<(&str, &str)> = uses
            .iter()
            .map(|u| (u.peri.as_str(), u.user.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("DMA1_CH4", "USART1 TX"),
                ("DMA1_CH5", "USART1 RX"),
                ("DMA1_CH3", "SPI1 TX"),
                ("DMA1_CH2", "SPI1 RX"),
            ]
        );
        assert!(uses.iter().all(|u| u.irq.is_empty() && !u.manual));

        // And they are the channels main.rs really passes.
        let main_rs = mcu.fresh_main_rs();
        for (peri, _) in &rows {
            let value = peri.replace("DMA", "dma").replace("_CH", ".");
            assert!(
                main_rs.contains(&value),
                "{value} missing from:\\n{main_rs}"
            );
        }
        // A DMA handle is rebound after every transfer (`_spi1 = spi;` is the
        // last line of the file's own example), so the binding must be `mut`.
        assert!(main_rs.contains("let mut _spi1 ="), "{main_rs}");

        // Unwire MISO and SPI1's receive channel must go with it, in the list
        // and in main.rs alike — the module keeps its `Both`, but a bus with no
        // receive line does not get to reserve DMA1_CH2.
        let miso = mcu
            .iter_all_pins()
            .find(|p| p.selected_function == PinFunction::SpiMiso(1))
            .map(|p| p.number)
            .expect("PA6 was wired above");
        if let Some(p) = mcu.find_pin_mut(miso) {
            p.selected_function = PinFunction::Unset;
        }
        let uses = blocking_dma_uses(&mcu);
        let rows: Vec<(&str, &str)> = uses
            .iter()
            .map(|u| (u.peri.as_str(), u.user.as_str()))
            .collect();
        assert_eq!(
            rows,
            [
                ("DMA1_CH4", "USART1 TX"),
                ("DMA1_CH5", "USART1 RX"),
                ("DMA1_CH3", "SPI1 TX"),
            ]
        );
        let main_rs = mcu.fresh_main_rs();
        assert!(!main_rs.contains("dma1.2"), "{main_rs}");
        assert!(main_rs.contains("stm32f1xx_hal::spi::NoMiso"), "{main_rs}");
        // Still a transmitter on DMA, so still rebindable.
        assert!(main_rs.contains("let mut _spi1 ="), "{main_rs}");

        // Turn the transport off and the plain binding comes back — the polled
        // templates are the ones that tell the user to add `mut` themselves.
        for m in &mut mcu.modules {
            if let ModuleConfig::Spi(c) = &mut m.config {
                c.blocking_dma = BlockingDma::Off;
            }
        }
        let main_rs = mcu.fresh_main_rs();
        assert!(main_rs.contains("let _spi1 ="), "{main_rs}");
    }
}

#[cfg(test)]
mod duty_handle_tests {
    use super::*;

    fn pwm_pin(name: &str, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerPwm { timer: 2, channel };
        p
    }

    /// `DutyHandle` may never name a channel the timer has no pin for.
    ///
    /// The trait shipped with `Channel::C1` hardcoded, which is a RUNTIME PANIC
    /// on this exact wiring — `PINS::check_used` says "Unused channel" — and it
    /// is the one defect in the F1 chain that a cross-compile cannot catch, so
    /// it gets a test instead. TIM2 on CH3+CH4 is the harness's own wiring:
    /// PA0/PA1 belong to the ADC there, which is why CH1 is free to be wrong.
    #[test]
    fn the_duty_trait_only_reaches_wired_channels() {
        let (p3, p4) = (pwm_pin("PA2", 3), pwm_pin("PA3", 4));
        let file = pwm_config_file(2, None, &[(3, &p3), (4, &p4)], "Tim2NoRemap");

        // The whole point: no unwired channel anywhere in the file.
        for ch in ["C1", "C2"] {
            assert!(
                !file.contains(&format!("Channel::{ch}")),
                "TIM2 has no {ch} pin, so nothing may name it:\n{file}"
            );
        }
        // One method per wired channel, and the bare one delegates rather than
        // repeating the arithmetic.
        assert!(
            file.contains("fn set_duty_tim_2_ch3(&mut self, value: u32) {"),
            "{file}"
        );
        assert!(
            file.contains("fn set_duty_tim_2_ch4(&mut self, value: u32) {"),
            "{file}"
        );
        assert!(
            file.contains("        self.set_duty_tim_2_ch3(value);"),
            "{file}"
        );
        assert!(file.contains("/// CH3, on PA2."), "{file}");
    }

    /// With CH1 wired the bare method still drives C1 — the fix moved the
    /// channel, it did not make the common case take a detour.
    #[test]
    fn ch1_is_still_the_default_when_it_is_wired() {
        let (p1, p2) = (pwm_pin("PA0", 1), pwm_pin("PA1", 2));
        let file = pwm_config_file(2, None, &[(1, &p1), (2, &p2)], "Tim2NoRemap");
        assert!(
            file.contains("        self.set_duty_tim_2_ch1(value);"),
            "{file}"
        );
        assert!(file.contains("Channel::C1"), "{file}");
    }
}

#[cfg(test)]
mod stale_io_mode_tests {
    use super::{GpioMode, PinFunction, into_expr};

    /// The generated call always matches the comment beside it.
    ///
    /// `io_mode` OUTLIVES the function it was chosen for - `apply_pin_function`
    /// sets `selected_function` and clears `custom_label`, and never touches the
    /// mode - so this sequence, which is three clicks in the Pins tab,
    ///
    ///     GPIO input  ->  Pull-up  ->  GPIO output
    ///
    /// left `Some(PullUp)` on an output. `unwrap_or(PushPull)` kept it, and this
    /// function emitted `into_pull_up_input(&mut gpioa.crl)` on the line the
    /// generator comments `// GPIO Output`. It compiles; the pin is an input.
    ///
    /// Asserted on the DIRECTION rather than on one exact string, so a HAL
    /// rename does not turn a real regression into a green run.
    #[test]
    fn a_mode_from_the_other_direction_never_crosses_over() {
        let cases = [
            // (function, the mode left behind by the OTHER direction)
            (PinFunction::GpioOutput, GpioMode::PullUp, "_output"),
            (PinFunction::GpioOutput, GpioMode::PullDown, "_output"),
            (PinFunction::GpioOutput, GpioMode::Floating, "_output"),
            (PinFunction::GpioInput, GpioMode::PushPull, "_input"),
            (PinFunction::GpioInput, GpioMode::OpenDrain, "_input"),
        ];
        for (func, stale, want) in cases {
            let e = into_expr(&func, Some(stale), "gpioa", "crl");
            let call = e.split('(').next().unwrap_or_default();
            assert!(
                call.ends_with(want),
                "{func:?} with a leftover {stale:?} generated `{e}`,                  which is not a {want} call"
            );
        }
    }

    /// A mode of the RIGHT direction still reaches the generated call, and no
    /// mode at all still falls to the family default. The filter must not cost
    /// the user the choice they did make.
    #[test]
    fn the_users_own_choice_still_reaches_the_code() {
        let up = into_expr(
            &PinFunction::GpioInput,
            Some(GpioMode::PullUp),
            "gpioa",
            "crl",
        );
        assert!(up.starts_with("into_pull_up_input"), "{up}");
        let od = into_expr(
            &PinFunction::GpioOutput,
            Some(GpioMode::OpenDrain),
            "gpioa",
            "crl",
        );
        assert!(od.starts_with("into_open_drain_output"), "{od}");
        let none_in = into_expr(&PinFunction::GpioInput, None, "gpioa", "crl");
        assert!(none_in.starts_with("into_floating_input"), "{none_in}");
        let none_out = into_expr(&PinFunction::GpioOutput, None, "gpioa", "crl");
        assert!(none_out.starts_with("into_push_pull_output"), "{none_out}");
    }
}

#[cfg(test)]
mod hal_clock_tests {
    use super::{CfgrArgs, cfgr_args, hal_pclks};
    use crate::panels::mcu_module::clock::model::{PllSrc, Stm32f1Clock, SysclkSrc};

    fn pclks(c: Stm32f1Clock) -> Option<(u32, u32)> {
        hal_pclks(&cfgr_args(&c))
    }

    /// The Blue Pill default comes out as the Clock tab draws it.
    #[test]
    fn the_default_clock_is_what_the_tab_says() {
        assert_eq!(
            pclks(Stm32f1Clock::default()),
            Some((36_000_000, 72_000_000))
        );
    }

    /// stm32f1xx-hal cannot program PLLXTPRE: HSE 8 MHz /2 x9 is drawn as
    /// 36 MHz, but the HAL feeds the PLL the whole 8 MHz and re-derives the
    /// multiplier from 36 / 8 = 4, running at 32 MHz.
    #[test]
    fn hse_halved_into_the_pll_is_not_what_the_hal_runs() {
        let c = Stm32f1Clock {
            pll_src: PllSrc::HseDiv2,
            ..Stm32f1Clock::default()
        };
        assert_eq!(pclks(c), Some((16_000_000, 32_000_000)));
    }

    /// An odd HCLK asked to halve: the HAL rounds the APB1 ratio UP and lands
    /// on /4. 72 MHz / 512 is 140 625; the tab says 70 312, the HAL 35 156.
    #[test]
    fn an_odd_hclk_halved_lands_on_div4() {
        let c = Stm32f1Clock {
            ahb_pre: 512,
            ..Stm32f1Clock::default()
        };
        assert_eq!(pclks(c), Some((35_156, 140_625)));
    }

    /// A clock `get_clocks` asserts on: APB1 left at /1 puts PCLK1 at 72 MHz.
    #[test]
    fn a_clock_over_the_hal_ceilings_panics_in_freeze() {
        let c = Stm32f1Clock {
            apb1_pre: 1,
            ..Stm32f1Clock::default()
        };
        assert_eq!(pclks(c), None);
    }

    /// The HSI with no PLL: 8 MHz everywhere, as the tab says.
    #[test]
    fn the_bare_hsi_runs_at_8_mhz() {
        let c = Stm32f1Clock {
            hse_enabled: false,
            sysclk_src: SysclkSrc::Hsi,
            apb1_pre: 1,
            ..Stm32f1Clock::default()
        };
        assert_eq!(pclks(c), Some((8_000_000, 8_000_000)));
    }

    /// The divide-by-zero guards: a zero target is the HAL dividing by it.
    #[test]
    fn zero_targets_are_refused() {
        let base = cfgr_args(&Stm32f1Clock::default());
        assert_eq!(hal_pclks(&CfgrArgs { pclk1: 0, ..base }), None);
        assert_eq!(
            hal_pclks(&CfgrArgs {
                hclk: Some(0),
                ..base
            }),
            None
        );
    }
}
