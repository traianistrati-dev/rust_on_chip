//! Raspberry Pi Pico / Pico 2 — `rp2040-hal` and `rp235x-hal`, blocking.
//!
//! These are BOARDS, not bare chips: the pin numbers are the 40-pin header's,
//! because that is what is silkscreened and what a user counts to. GP23/24/25/29
//! are not on the header but are on the board, and GP25 drives the LED.
//!
//! Two things make this family unlike every other one here.
//!
//! **The chip cannot boot from flash on its own.** On RP2040 the boot ROM copies
//! 256 bytes from the start of flash into RAM and runs them; that stage sets up
//! the QSPI flash for execute-in-place. It is a linked artifact, not code the
//! user writes, so it is generated inside the marked block. RP2350 replaced it
//! with an image block the HAL supplies.
//!
//! **There are two cores.** That is why the generated `Cargo.toml` does not
//! enable `cortex-m/critical-section-single-core` — see `cargo_toml_rp`.

use super::common::AUTOGEN_BANNER;
use super::common::{
    ASYNC_USER_TAIL, GEN_BEGIN, GEN_END, USER_TAIL, mcu_id_marker_line, retarget_pristine_tail,
    var_suffix,
};
use super::family::FamilyBackend;
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::UsartModuleConfig;
use crate::panels::mcu_module::pins::PinFunction;
use crate::panels::mcu_module::pins::logic::pin::GpioMode;
use crate::panels::mcu_module::pins::logic::pin::model::Edge;

pub struct RpBackend;

/// The two families this backend serves.
pub fn is_rp(family: &str) -> bool {
    matches!(family, "rp2040" | "rp235x")
}

/// `rp2040_hal` / `rp235x_hal` — the crate name as it is written in Rust.
fn hal_crate(family: &str) -> &'static str {
    if family == "rp2040" {
        "rp2040_hal"
    } else {
        "rp235x_hal"
    }
}

/// `GP13` -> `13`. The definition names header pins after the GPIO they carry,
/// so the number in the name IS the GPIO index; power and ground pins have no
/// digits and are reserved anyway.
/// The GP number a pad name carries, e.g. `GP23 (SMPS mode)` -> 23.
///
/// `pub(crate)` because the module panel resolves a two-pads-one-channel
/// clash by the same rule this file's emitter does, and it has to be the same
/// parse - a panel that ordered pads differently would name a different
/// winner than the generated code.
pub(crate) fn gpio_index(name: &str) -> Option<u8> {
    name.strip_prefix("GP")?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

/// The GPIO bindings, in header order.
/// A pad the user wired but has not written code for yet is not a mistake -
/// it is a pad they are about to use. Same words the F1 backend uses.
const ALLOW: &str = "    #[allow(unused_mut, unused_variables)]
";

fn gpio_lines(mcu: &Mcu) -> String {
    // One entry per pin, joined with a blank line between — see
    // `common::blank_separated`. The interrupt note below belongs to ITS pin, so
    // it goes in the same entry rather than becoming an orphan paragraph.
    let mut pins_out: Vec<String> = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(n) = gpio_index(&p.name) else {
            continue;
        };
        let sfx = var_suffix(&p.selected_function);
        match p.selected_function {
            PinFunction::GpioOutput => pins_out.push(format!(
                "{ALLOW}    let mut gp{n}{sfx} = pins.gpio{n}.into_push_pull_output();\n"
            )),
            PinFunction::GpioInput => {
                let mut entry =
                    format!("{ALLOW}    let gp{n}{sfx} = pins.gpio{n}.into_pull_up_input();\n");
                // The edge is set in the pin panel on every runtime, but only
                // Async can act on it here. Saying so beats dropping it.
                if p.irq.is_some() {
                    entry.push_str(&format!(
                        "    // GP{n} is armed for an interrupt, which this Blocking project\n    // does not generate. Switch Runtime to Async in the System tab and\n    // the pin becomes a task that awaits the edge.\n"
                    ));
                }
                pins_out.push(entry);
            }
            _ => {}
        }
    }
    super::common::blank_separated(pins_out)
}

/// The boot stage, which differs between the two chips and is not optional on
/// either.
fn boot_block(family: &str) -> String {
    if family == "rp2040" {
        "/// Second-stage bootloader. The boot ROM copies these 256 bytes from the\n\
         /// start of flash into RAM and runs them, and they set up the QSPI flash\n\
         /// for execute-in-place. Without it the chip boots into nothing.\n\
         #[link_section = \".boot2\"]\n\
         #[used]\n\
         pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;\n"
            .to_owned()
    } else {
        "/// The image block the RP2350 boot ROM looks for. It replaces RP2040's\n\
         /// second-stage bootloader and is supplied by the HAL.\n\
         #[link_section = \".start_block\"]\n\
         #[used]\n\
         pub static IMAGE_DEF: rp235x_hal::block::ImageDef = rp235x_hal::block::ImageDef::secure_exe();\n"
            .to_owned()
    }
}

/// One PLL's three numbers, straight off the tree.
///
/// `PLLConfig` is exactly FBDIV / POSTDIV1 / POSTDIV2 plus the VCO those imply,
/// which is why the graph models them separately rather than as one opaque
/// "PLL": every field here is a node the user can see and change.
struct Pll {
    vco_mhz: u32,
    pd1: u32,
    pd2: u32,
}

/// The post-divider options, in the order the graph lists them.
const PD: [u32; 7] = [1, 2, 3, 4, 5, 6, 7];

fn pll_from(mcu: &Mcu, prefix: &str, xtal_hz: u32) -> Pll {
    use crate::panels::mcu_module::clock::graph::model::NodeState;
    use crate::panels::mcu_module::clock::model::ClockConfig;
    let ClockConfig::Graph(gc) = &mcu.clock else {
        // A chip with no tree cannot answer; the caller's defaults stand.
        return Pll {
            vco_mhz: 1500,
            pd1: 6,
            pd2: 2,
        };
    };
    let value = |id: String| match gc.graph.node(&id).map(|n| &n.state) {
        Some(NodeState::Value(v)) => Some(*v),
        Some(NodeState::Index(i)) => PD.get(*i).copied(),
        _ => None,
    };
    let fb = value(format!("{prefix}_fb")).unwrap_or(125);
    Pll {
        vco_mhz: xtal_hz / 1_000_000 * fb,
        pd1: value(format!("{prefix}_pd1")).unwrap_or(6),
        pd2: value(format!("{prefix}_pd2")).unwrap_or(2),
    }
}

/// The crystal, as the tree states it.
fn xtal_hz(mcu: &Mcu) -> u32 {
    use crate::panels::mcu_module::clock::graph::model::NodeState;
    use crate::panels::mcu_module::clock::model::ClockConfig;
    let ClockConfig::Graph(gc) = &mcu.clock else {
        return 12_000_000;
    };
    match gc.graph.node("xosc").map(|n| &n.state) {
        Some(NodeState::Source { hz, .. }) => *hz,
        _ => 12_000_000,
    }
}

/// Which GPIO carries each role of one bus instance.
///
/// The definition already says it — every header pin lists the SPI/UART/I2C
/// role its FUNCSEL table gives it — so the backend never has to know the table
/// itself, only how to read the wiring back out.
fn bus_pins(
    mcu: &Mcu,
    want: impl Fn(&PinFunction) -> Option<(u8, &'static str)>,
) -> Vec<(u8, &'static str, u8)> {
    let mut out = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(n) = gpio_index(&p.name) else {
            continue;
        };
        if let Some((inst, role)) = want(&p.selected_function) {
            out.push((inst, role, n));
        }
    }
    out.sort_unstable();
    out
}

/// One instance's pin for a role, if it is wired.
fn role_of(pins: &[(u8, &'static str, u8)], inst: u8, role: &str) -> Option<u8> {
    pins.iter()
        .find(|(i, r, _)| *i == inst && *r == role)
        .map(|(_, _, n)| *n)
}

/// The instances that have any pin at all, ascending.
fn instances(pins: &[(u8, &'static str, u8)]) -> Vec<u8> {
    let mut v: Vec<u8> = pins.iter().map(|(i, _, _)| *i).collect();
    v.dedup();
    v
}

/// The signal a pin carries, as one name: `"UART0 TX"`, `"PWM3 A"`.
///
/// Two pads CAN claim the same signal on this chip — GP0 and GP16 are both
/// UART0 TX, GP4 and GP20 are both UART1 TX, and so on the whole way up. That
/// is the FUNCSEL table, not a mistake in the definition.
fn signal_name(f: &PinFunction) -> Option<String> {
    Some(match f {
        PinFunction::UsartTx(i) => format!("UART{i} TX"),
        PinFunction::UsartRx(i) => format!("UART{i} RX"),
        PinFunction::SpiSck(i) => format!("SPI{i} SCK"),
        PinFunction::SpiMosi(i) => format!("SPI{i} TX"),
        PinFunction::SpiMiso(i) => format!("SPI{i} RX"),
        PinFunction::I2cSda(i) => format!("I2C{i} SDA"),
        PinFunction::I2cScl(i) => format!("I2C{i} SCL"),
        PinFunction::TimerPwm { timer, channel } => {
            format!("PWM{timer} {}", if *channel == 1 { "A" } else { "B" })
        }
        _ => return None,
    })
}

/// Say so when two pads claim one signal.
///
/// rp-hal takes ONE pin per role, so only the lowest-numbered pad can be
/// configured — and the code that did so simply used the first it found and
/// dropped the rest without a word. The project then built, ran, and left a pad
/// the user had deliberately wired doing nothing, with nothing anywhere saying
/// why.
///
/// The generator does not choose between them: it takes the lowest so the output
/// is stable, and names both so the person who wired them can decide.
fn ambiguity_notes(mcu: &Mcu) -> String {
    let mut by_signal: std::collections::BTreeMap<String, Vec<u8>> =
        std::collections::BTreeMap::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let (Some(n), Some(sig)) = (gpio_index(&p.name), signal_name(&p.selected_function)) else {
            continue;
        };
        by_signal.entry(sig).or_default().push(n);
    }
    let mut o = String::new();
    for (sig, mut pads) in by_signal {
        if pads.len() < 2 {
            continue;
        }
        pads.sort_unstable();
        let used = pads[0];
        let rest: Vec<String> = pads[1..].iter().map(|n| format!("GP{n}")).collect();
        o.push_str(&format!(
            "    // {sig} is wired to GP{used} and {}. Only GP{used} is configured:\n",
            rest.join(" and ")
        ));
        o.push_str(
            "    // the HAL takes one pin per role. Unassign the other on the Pins canvas.\n",
        );
    }
    o
}

fn uart_pins(mcu: &Mcu) -> Vec<(u8, &'static str, u8)> {
    bus_pins(mcu, |f| match f {
        PinFunction::UsartTx(i) => Some((*i, "tx")),
        PinFunction::UsartRx(i) => Some((*i, "rx")),
        _ => None,
    })
}

fn spi_pins(mcu: &Mcu) -> Vec<(u8, &'static str, u8)> {
    bus_pins(mcu, |f| match f {
        PinFunction::SpiSck(i) => Some((*i, "sck")),
        PinFunction::SpiMosi(i) => Some((*i, "mosi")),
        PinFunction::SpiMiso(i) => Some((*i, "miso")),
        _ => None,
    })
}

fn i2c_pins(mcu: &Mcu) -> Vec<(u8, &'static str, u8)> {
    bus_pins(mcu, |f| match f {
        PinFunction::I2cSda(i) => Some((*i, "sda")),
        PinFunction::I2cScl(i) => Some((*i, "scl")),
        _ => None,
    })
}

/// UART, SPI and I2C, in that order.
///
/// Each is emitted only when BOTH of its required pads are wired. rp-hal's
/// constructors take the pins by value in a fixed order and there is no
/// `NoPin` — so half a bus is not a smaller bus here, it is a type error.
fn bus_lines(mcu: &Mcu, _hal: &str) -> String {
    let mut o = ambiguity_notes(mcu);

    let uart = uart_pins(mcu);
    for i in instances(&uart) {
        let (Some(tx), Some(rx)) = (role_of(&uart, i, "tx"), role_of(&uart, i, "rx")) else {
            o.push_str(&format!(
                "    // UART{i}: only one of TX/RX is wired, and the constructor takes the\n    // pair. Wire the other pad on the Pins canvas.\n"
            ));
            continue;
        };
        o.push_str(&format!(
            "    let uart{i} = pins::configs::uart{i}::init(\n        pac.UART{i},\n        pins.gpio{tx},\n        pins.gpio{rx},\n        &mut pac.RESETS,\n        clocks.peripheral_clock.freq(),\n    );\n    let _ = &uart{i};\n"
        ));
    }

    let spi = spi_pins(mcu);
    for i in instances(&spi) {
        let (Some(sck), Some(mosi), Some(miso)) = (
            role_of(&spi, i, "sck"),
            role_of(&spi, i, "mosi"),
            role_of(&spi, i, "miso"),
        ) else {
            o.push_str(&format!(
                "    // SPI{i}: the constructor takes (MOSI, MISO, SCK) together, so all\n    // three pads have to be wired before anything can be built.\n"
            ));
            continue;
        };
        o.push_str(&format!(
            "    let spi{i} = pins::configs::spi{i}::init(\n        pac.SPI{i},\n        pins.gpio{mosi},\n        pins.gpio{miso},\n        pins.gpio{sck},\n        &mut pac.RESETS,\n        clocks.peripheral_clock.freq(),\n    );\n    let _ = &spi{i};\n"
        ));
    }

    let i2c = i2c_pins(mcu);
    for i in instances(&i2c) {
        let (Some(sda), Some(scl)) = (role_of(&i2c, i, "sda"), role_of(&i2c, i, "scl")) else {
            o.push_str(&format!(
                "    // I2C{i}: SDA and SCL are taken together; wire the missing one.\n"
            ));
            continue;
        };
        o.push_str(&format!(
            "    let i2c{i} = pins::configs::i2c{i}::init(\n        pac.I2C{i},\n        pins.gpio{sda},\n        pins.gpio{scl},\n        &mut pac.RESETS,\n        &clocks.system_clock,\n    );\n    let _ = &i2c{i};\n"
        ));
    }
    o
}

/// PWM slices and ADC inputs.
///
/// A slice is not indexable on rp-hal — `Slices` exposes `pwm0`..`pwm7` as
/// FIELDS — so the generated code names each one. Channel A is the even GPIO of
/// the pair and B the odd one, which is why the definition stores the slice as
/// the timer and 1/2 as the channel.
fn pwm_adc_lines(mcu: &Mcu, hal: &str) -> String {
    let mut o = String::new();

    let mut pwm: Vec<(u8, u8, u8)> = Vec::new();
    let mut adc: Vec<(u8, u8)> = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(n) = gpio_index(&p.name) else {
            continue;
        };
        match p.selected_function {
            PinFunction::TimerPwm { timer, channel } => pwm.push((timer, channel, n)),
            PinFunction::AdcChannel { channel, .. } => adc.push((channel, n)),
            _ => {}
        }
    }
    pwm.sort_unstable();
    adc.sort_unstable();

    if !pwm.is_empty() {
        o.push_str(
            "    // Every slice comes from one PWM peripheral, so main.rs owns
",
        );
        o.push_str(
            "    // the set and lends each wired one to its config module.
",
        );
        o.push_str(&format!(
            "    let mut pwm_slices = {hal}::pwm::Slices::new(pac.PWM, &mut pac.RESETS);
"
        ));
        let mut by_slice: std::collections::BTreeMap<u8, Vec<(u8, u8)>> =
            std::collections::BTreeMap::new();
        for (slice, channel, n) in &pwm {
            by_slice.entry(*slice).or_default().push((*channel, *n));
        }
        for (slice, chans) in &by_slice {
            let args: Vec<String> = chans.iter().map(|(_, n)| format!("pins.gpio{n}")).collect();
            o.push_str(&format!(
                "    pins::configs::pwm{slice}::init(&mut pwm_slices.pwm{slice}, {});
",
                args.join(", ")
            ));
        }
    }

    if !adc.is_empty() {
        o.push_str(&format!(
            "    let mut adc = {hal}::adc::Adc::new(pac.ADC, &mut pac.RESETS);\n"
        ));
        o.push_str("    let _ = &mut adc;\n");
        for (channel, n) in &adc {
            o.push_str(&format!(
                "    // ADC{channel}, on GP{n}. Read it with `adc.read(&mut adc{channel})`.\n"
            ));
            o.push_str(&format!(
                "    let mut adc{channel} = {hal}::adc::AdcPin::new(pins.gpio{n}).unwrap();\n"
            ));
            o.push_str(&format!("    let _ = &mut adc{channel};\n"));
        }
    }
    o
}

/// The generated region: boot stage, clocks, the GPIO bank, and the pins.
///
/// Built line by line rather than as one continued literal: rustfmt joins a
/// `\`-continued string back onto one physical line and the continuation turns
/// into a run of spaces inside the generated file.
fn section(mcu: &Mcu) -> String {
    let hal = hal_crate(&mcu.family);
    let xtal = xtal_hz(mcu);
    let sys = pll_from(mcu, "pll_sys", xtal);
    let usb = pll_from(mcu, "pll_usb", xtal);
    let mut o = String::new();
    o.push_str(GEN_BEGIN);
    o.push('\n');
    o.push_str(&boot_block(&mcu.family));
    o.push('\n');

    o.push_str("/// The crystal on the board, and each PLL as the Clock tab has it.\n");
    o.push_str(&format!("pub const XTAL_FREQ_HZ: u32 = {xtal};\n\n"));
    for (name, cfg, what) in [
        ("PLL_SYS_CFG", &sys, "the system clock"),
        ("PLL_USB_CFG", &usb, "USB, which needs exactly 48 MHz"),
    ] {
        o.push_str(&format!(
            "/// {what}: {} MHz VCO / {} / {} = {} MHz.\n",
            cfg.vco_mhz,
            cfg.pd1,
            cfg.pd2,
            cfg.vco_mhz / cfg.pd1 / cfg.pd2
        ));
        o.push_str(&format!(
            "pub const {name}: {hal}::pll::PLLConfig = {hal}::pll::PLLConfig {{\n"
        ));
        o.push_str(&format!(
            "    vco_freq: {hal}::fugit::HertzU32::MHz({}),\n",
            cfg.vco_mhz
        ));
        o.push_str("    refdiv: 1,\n");
        o.push_str(&format!("    post_div1: {},\n", cfg.pd1));
        o.push_str(&format!("    post_div2: {},\n", cfg.pd2));
        o.push_str("};\n\n");
    }

    let fpga = fpga_loader(mcu);
    if fpga {
        o.push_str(&fpga_items());
    }
    o.push_str(&format!("#[{hal}::entry]\n"));
    o.push_str("fn main() -> ! {\n");
    o.push_str(&format!(
        "    let mut pac = {hal}::pac::Peripherals::take().unwrap();\n"
    ));
    o.push_str(&format!(
        "    let mut watchdog = {hal}::Watchdog::new(pac.WATCHDOG);\n"
    ));
    o.push_str("\n");
    o.push_str("    // Built from the Clock tab, not from the HAL's fixed default: the two\n");
    o.push_str("    // PLLConfigs above are this tree's FBDIV and POSTDIV values.\n");
    o.push_str("    //\n");
    o.push_str("    // `map_err(|_| false)` because these error types carry no Debug, so\n");
    o.push_str("    // `.unwrap()` alone cannot name them.\n");
    o.push_str(&format!(
        "    let xosc = {hal}::xosc::setup_xosc_blocking(\n"
    ));
    o.push_str("        pac.XOSC,\n");
    o.push_str(&format!(
        "        {hal}::fugit::HertzU32::Hz(XTAL_FREQ_HZ),\n"
    ));
    o.push_str("    )\n    .map_err(|_| false)\n    .unwrap();\n");
    o.push_str(&format!(
        "    let mut clocks = {hal}::clocks::ClocksManager::new(pac.CLOCKS);\n"
    ));
    for (var, peri, cfg) in [
        ("pll_sys", "PLL_SYS", "PLL_SYS_CFG"),
        ("pll_usb", "PLL_USB", "PLL_USB_CFG"),
    ] {
        o.push_str(&format!(
            "    let {var} = {hal}::pll::setup_pll_blocking(\n"
        ));
        o.push_str(&format!("        pac.{peri},\n"));
        o.push_str("        xosc.operating_frequency(),\n");
        o.push_str(&format!("        {cfg},\n"));
        o.push_str("        &mut clocks,\n        &mut pac.RESETS,\n    )\n");
        o.push_str("    .map_err(|_| false)\n    .unwrap();\n");
    }
    o.push_str("    clocks\n        .init_default(&xosc, &pll_sys, &pll_usb)\n");
    o.push_str("        .map_err(|_| false)\n        .unwrap();\n");
    // The HAL's own `init_clocks_and_plls` starts this tick; the manual
    // bring-up above is the sequence its docs spell out, and that sequence
    // starts it too. It was missing here, so the watchdog - and on the RP2040
    // the timer - counted whatever the boot ROM had left in TICK.
    o.push_str("    // The 1 us tick the watchdog and the timer count, divided down from the\n");
    o.push_str("    // crystal. `init_clocks_and_plls` would start it; the manual bring-up\n");
    o.push_str("    // above has to do it itself.\n");
    o.push_str(&format!(
        "    watchdog.enable_tick_generation((XTAL_FREQ_HZ / 1_000_000) as {});\n\n",
        // rp2040-hal takes the cycle count as a u8, rp235x-hal as a u16.
        if mcu.family == "rp2040" { "u8" } else { "u16" }
    ));
    o.push_str(&super::watchdog_gen::rp_init_lines(&mcu.watchdog, false));

    o.push_str("    // Every GPIO comes from one bank, taken once.\n");
    o.push_str(&format!("    let sio = {hal}::Sio::new(pac.SIO);\n"));
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str(&format!("    let pins = {hal}::gpio::Pins::new(\n"));
    o.push_str("        pac.IO_BANK0,\n        pac.PADS_BANK0,\n        sio.gpio_bank0,\n        &mut pac.RESETS,\n    );\n\n");
    // The FPGA before any other pin: see `FPGA_BODY_HEAD`.
    if fpga {
        o.push_str(FPGA_BODY_HEAD);
        o.push_str(&blocking_fpga_body(hal, usb.vco_mhz / usb.pd1 / usb.pd2));
    }
    o.push_str(&gpio_lines(mcu));
    o.push_str(&bus_lines(mcu, hal));
    if radio_led(mcu) {
        // Not a gap in this backend — there IS no blocking path. `cyw43` is
        // async to the bottom: embassy-sync, embassy-time, embedded-hal-async
        // and a spawned runner task. Saying so beats emitting nothing.
        o.push_str(
            "    // WL_LED is driven, but this project is Blocking.
",
        );
        o.push_str(
            "    //
",
        );
        o.push_str(
            "    // The LED hangs off the CYW43 radio, and the radio's driver is async
",
        );
        o.push_str(
            "    // only - there is no blocking version of it to call. Switch Runtime to
",
        );
        o.push_str(
            "    // Async in the System tab and this becomes the wireless bring-up.
",
        );
    }
    o.push_str(&pwm_adc_lines(mcu, hal));
    o.push_str(GEN_END);
    o.push('\n');
    o
}

fn header(mcu: &Mcu) -> String {
    let hal = if mcu.family == "rp2040" {
        "rp2040-hal"
    } else {
        "rp235x-hal"
    };
    format!(
        "{AUTOGEN_BANNER}\n\
         // MCU: {} | HAL: {hal} (blocking)\n\
         {}\n\
         #![no_std]\n\
         #![no_main]\n\
         \n\
         pub mod pins;\n\
         \n\
         use panic_halt as _;\n\
         #[allow(unused_imports)]\n\
         use embedded_hal::digital::{{InputPin, OutputPin}};\n\
         // `Clock` carries `.freq()`, which every bus constructor asks the\n\
         // peripheral clock for.\n\
         #[allow(unused_imports)]\n\
         use {hal_crate}::Clock;\n\
         // `SetDutyCycle` carries `max_duty_cycle` and `set_duty_cycle`; they\n\
         // are embedded-hal's, not rp-hal's own.\n\
         #[allow(unused_imports)]\n\
         use embedded_hal::pwm::SetDutyCycle;\n\
         \n",
        mcu.name,
        mcu_id_marker_line(&mcu.id),
        hal_crate = hal_crate(&mcu.family),
    )
}

/// `src/pins/configs/uart{n}.rs`, `spi{n}.rs`, `i2c{n}.rs`.
///
/// Each owns its peripheral outright — unlike PWM, where every slice comes
/// from one block — so `init` takes it by value and hands back a `Handle` the
/// caller keeps.
/// The speed this bus runs at, as the Virtual Module has it.
///
/// The generated block says "from the Virtual Module — auto-updated", and it
/// used to be a hard-coded literal: a UART set to 9600 in the panel came out
/// at 115200 in the firmware, which the user meets as garbage on a terminal
/// rather than as a message from the IDE.
fn bus_speed(mcu: &Mcu, kind: &str, n: u8) -> u32 {
    use crate::panels::mcu_module::modules;
    match kind {
        "uart" => modules::usart_configs(&mcu.modules)
            .get(&n)
            .map_or(115_200, |c| c.baud_rate),
        "spi" => modules::spi_configs(&mcu.modules)
            .get(&n)
            .map_or(1_000_000, |c| c.clock_hz),
        _ => modules::i2c_configs(&mcu.modules)
            .get(&n)
            .map_or(400_000, |c| c.clock_hz),
    }
}

/// Whether UART `i` runs on the interrupt ring buffer rather than on DMA.
///
/// Read from the Virtual Module through the same door [`bus_speed`] opens.
/// `UsartMode` defaults to `Buffered` and the field is `#[serde(default)]`, so a
/// project saved while the transport combo was LOCKED loads as `Buffered` -
/// which is what the model always said, even while the greyed combo displayed
/// "DMA" at the user.
fn uart_is_buffered(
    cfgs: &std::collections::BTreeMap<u8, crate::panels::mcu_module::modules::UsartModuleConfig>,
    i: u8,
) -> bool {
    use crate::panels::mcu_module::modules::UsartMode;
    cfgs.get(&i).is_none_or(|c| c.mode == UsartMode::Buffered)
}

/// Whether this project needs `static_cell` and `embedded-io-async` in its
/// manifest.
///
/// The app gates those two crates on `has_cfg("usart")` over the generated
/// config files, and `AsyncRpBackend` writes none at all - so a `BufferedUart`
/// emitted into `main.rs` would reference `static_cell::StaticCell` against a
/// Cargo.toml that never received the line, and the failure would read as a
/// codegen bug rather than a manifest one. A Pico W hides it, because the radio
/// path adds `static_cell` for its own reasons.
///
/// ONE computation, called by the app AND by the cross-compile harness. Naming
/// the answer twice in two places is exactly how a matrix went green on a
/// project the application could not build.
pub fn needs_async_usart(mcu: &Mcu) -> bool {
    use crate::panels::mcu_module::mcu::model::Runtime;
    if !is_rp(&mcu.family) || !matches!(mcu.runtime, Runtime::Async) {
        return false;
    }
    let uart = uart_pins(mcu);
    let cfgs = crate::panels::mcu_module::modules::usart_configs(&mcu.modules);
    instances(&uart).into_iter().any(|i| {
        role_of(&uart, i, "tx").is_some()
            && role_of(&uart, i, "rx").is_some()
            && uart_is_buffered(&cfgs, i)
    })
}

/// The wire frame from the Virtual Module, as `rp2040-hal` spells it.
///
/// `UartConfig::new(baudrate, data_bits, parity, stop_bits)` — note the parity
/// is an `Option`, which is why "None" here is the absence of a parity bit and
/// not a variant called None.
///
/// The whole frame used to be hardcoded `DataBits::Eight, None, StopBits::One`,
/// so the three combos in the Virtual Module changed nothing on this chip: set
/// 8-E-2 in the panel, save, and the board still sent 8-N-1 while the panel went
/// on showing Even/2. The peer sees framing errors and nothing in the IDE
/// disagrees with it.
///
/// Nine data bits is not reachable: the PL011 has no 9-bit word length, and the
/// panel no longer offers one here (`usart_data_bits`). It maps to eight rather
/// than being refused, because a project saved on an STM32 and re-targeted at a
/// Pico has to open.
fn blocking_frame(hal: &str, cfg: Option<&UsartModuleConfig>) -> (String, String, String) {
    use crate::panels::mcu_module::modules::{Parity, StopBits};
    let d = UsartModuleConfig::new(0);
    let c = cfg.unwrap_or(&d);
    let bits = match c.data_bits {
        5 => "Five",
        6 => "Six",
        7 => "Seven",
        _ => "Eight",
    };
    let parity = match c.parity {
        Parity::None => "None".to_owned(),
        Parity::Even => format!("Some({hal}::uart::Parity::Even)"),
        Parity::Odd => format!("Some({hal}::uart::Parity::Odd)"),
    };
    let stop = match c.stop_bits {
        StopBits::One => "One",
        StopBits::Two => "Two",
    };
    (
        format!("{hal}::uart::DataBits::{bits}"),
        parity,
        format!("{hal}::uart::StopBits::{stop}"),
    )
}

/// The same frame as `embassy_rp` spells it — a plain `Parity` enum rather than
/// an `Option`, and the variants carry their own prefixes.
fn async_frame(cfg: Option<&UsartModuleConfig>) -> (String, String, String) {
    use crate::panels::mcu_module::modules::{Parity, StopBits};
    let d = UsartModuleConfig::new(0);
    let c = cfg.unwrap_or(&d);
    let bits = match c.data_bits {
        5 => "DataBits5",
        6 => "DataBits6",
        7 => "DataBits7",
        _ => "DataBits8",
    };
    let parity = match c.parity {
        Parity::None => "ParityNone",
        Parity::Even => "ParityEven",
        Parity::Odd => "ParityOdd",
    };
    let stop = match c.stop_bits {
        StopBits::One => "STOP1",
        StopBits::Two => "STOP2",
    };
    (
        format!("embassy_rp::uart::DataBits::{bits}"),
        format!("embassy_rp::uart::Parity::{parity}"),
        format!("embassy_rp::uart::StopBits::{stop}"),
    )
}

fn bus_config_file(
    hal: &str,
    kind: &str,
    n: u8,
    pads: &[(&str, u8)],
    hz: u32,
    frame: Option<&UsartModuleConfig>,
    // The I2C bus's device modules (`common::i2c_device_mods`), empty for the
    // other two kinds. Resolved by the caller the way `hz` is, so this function
    // stays a pure formatter.
    i2c_mods: &str,
) -> String {
    let mut o = String::new();
    o.push_str("// <<< GENERATED>>>\n");
    o.push_str(
        "// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.\n",
    );
    match kind {
        "uart" => {
            let (bits, parity, stop) = blocking_frame(hal, frame);
            o.push_str(&format!("pub const BAUDRATE: u32 = {hz};\n"));
            // In the GENERATED block, so `init` below stays the user's to edit
            // while these keep following the Virtual Module.
            o.push_str(&format!(
                "pub const DATA_BITS: {hal}::uart::DataBits = {bits};\npub const PARITY: Option<{hal}::uart::Parity> = {parity};\npub const STOP_BITS: {hal}::uart::StopBits = {stop};\n"
            ));
        }
        "spi" => o.push_str(&format!("pub const SPI_HZ: u32 = {hz};\n")),
        _ => {
            o.push_str(&format!("pub const I2C_HZ: u32 = {hz};\n"));
            o.push_str(i2c_mods);
        }
    }
    o.push_str("// <<< GENERATED END >>>\n\n");
    o.push_str("// Everything below is editable — your changes are preserved on regeneration.\n");
    // No `use Clock` here: main.rs asks the clock for its frequency and passes
    // the value in, so these modules never touch the trait.
    let gpio = |n: u8| {
        format!(
            "{hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{n}, {hal}::gpio::Function{}, {hal}::gpio::PullDown>",
            match kind {
                "uart" => "Uart",
                "spi" => "Spi",
                _ => "I2c",
            }
        )
    };

    match kind {
        "uart" => {
            let tx = pads.iter().find(|(r, _)| *r == "tx").unwrap().1;
            let rx = pads.iter().find(|(r, _)| *r == "rx").unwrap().1;
            o.push_str(&format!(
                "\n/// The concrete type `init` hands back, so it can be a struct field.\npub type Handle = {hal}::uart::UartPeripheral<\n    {hal}::uart::Enabled,\n    {hal}::pac::UART{n},\n    ({}, {}),\n>;\n\n",
                gpio(tx), gpio(rx)
            ));
            o.push_str(&format!(
                "/// UART{n} on GP{tx} (TX) and GP{rx} (RX), at BAUDRATE.\npub fn init(\n    uart: {hal}::pac::UART{n},\n    tx: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{tx}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    rx: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{rx}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    resets: &mut {hal}::pac::RESETS,\n    peri_freq: {hal}::fugit::HertzU32,\n) -> Handle {{\n"
            ));
            o.push_str(&format!(
                "    {hal}::uart::UartPeripheral::new(uart, (tx.into_function(), rx.into_function()), resets)\n        .enable(\n            {hal}::uart::UartConfig::new(\n                {hal}::fugit::HertzU32::Hz(BAUDRATE),\n                DATA_BITS,\n                PARITY,\n                STOP_BITS,\n            ),\n            peri_freq,\n        )\n        .unwrap()\n}}\n"
            ));
        }
        "spi" => {
            let sck = pads.iter().find(|(r, _)| *r == "sck").unwrap().1;
            let mosi = pads.iter().find(|(r, _)| *r == "mosi").unwrap().1;
            let miso = pads.iter().find(|(r, _)| *r == "miso").unwrap().1;
            o.push_str(&format!(
                "\n/// The concrete type `init` hands back.\npub type Handle = {hal}::spi::Spi<\n    {hal}::spi::Enabled,\n    {hal}::pac::SPI{n},\n    ({}, {}, {}),\n    8,\n>;\n\n",
                gpio(mosi), gpio(miso), gpio(sck)
            ));
            o.push_str(&format!(
                "/// SPI{n}: SCK GP{sck}, TX GP{mosi}, RX GP{miso}. `Spi::new` takes the three\n/// together and there is no `NoPin`, so a half-wired bus cannot be built.\npub fn init(\n    spi: {hal}::pac::SPI{n},\n    mosi: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{mosi}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    miso: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{miso}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    sck: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{sck}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    resets: &mut {hal}::pac::RESETS,\n    peri_freq: {hal}::fugit::HertzU32,\n) -> Handle {{\n"
            ));
            o.push_str(&format!(
                "    {hal}::spi::Spi::<_, _, _, 8>::new(\n        spi,\n        (mosi.into_function(), miso.into_function(), sck.into_function()),\n    )\n    .init(\n        resets,\n        peri_freq,\n        {hal}::fugit::HertzU32::Hz(SPI_HZ),\n        embedded_hal::spi::MODE_0,\n    )\n}}\n"
            ));
        }
        _ => {
            let sda = pads.iter().find(|(r, _)| *r == "sda").unwrap().1;
            let scl = pads.iter().find(|(r, _)| *r == "scl").unwrap().1;
            let p = |n: u8| {
                format!(
                    "{hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{n}, {hal}::gpio::FunctionI2c, {hal}::gpio::PullUp>"
                )
            };
            o.push_str(&format!(
                "\n/// The concrete type `init` hands back.\npub type Handle = {hal}::i2c::I2C<{hal}::pac::I2C{n}, ({}, {})>;\n\n",
                p(sda), p(scl)
            ));
            o.push_str(&format!(
                "/// I2C{n} on GP{sda} (SDA) and GP{scl} (SCL), at I2C_HZ.\npub fn init(\n    i2c: {hal}::pac::I2C{n},\n    sda: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{sda}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    scl: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{scl}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    resets: &mut {hal}::pac::RESETS,\n    sys_clock: &{hal}::clocks::SystemClock,\n) -> Handle {{\n"
            ));
            o.push_str(&format!(
                "    {hal}::i2c::I2C::i2c{n}(\n        i2c,\n        sda.reconfigure(),\n        scl.reconfigure(),\n        {hal}::fugit::HertzU32::Hz(I2C_HZ),\n        resets,\n        sys_clock,\n    )\n}}\n"
            ));
        }
    }
    o
}

/// `src/pins/configs/pwm{slice}.rs` — one PWM slice, its frequency and the duty
/// of each channel it drives.
///
/// The slice itself is NOT taken by value: on this chip `Slices::new` hands out
/// every slice at once from one `PWM` peripheral, so `main.rs` owns the set and
/// lends one out. That is the opposite of STM32, where each timer is its own
/// peripheral and the config file can own it outright.
fn pwm_config_file(mcu: &Mcu, slice: u8, chans: &[(u8, u8)]) -> String {
    let hal = hal_crate(&mcu.family);
    let cfg = crate::panels::mcu_module::modules::timer_configs(&mcu.modules);
    let cfg = cfg.get(&slice);
    let mut o = String::new();

    o.push_str("// <<< GENERATED>>>\n");
    o.push_str(
        "// Peripheral config (from the Virtual Module) — auto-updated; edit in the module.\n",
    );
    o.push_str("// Duty per channel, in HUNDREDTHS of a percent — 750 is 7.5 %, which is what a\n");
    o.push_str("// hobby servo wants and what whole percent cannot say.\n");
    for (channel, _) in chans {
        let x100 = cfg.map_or(0, |c| c.duty_x100_of(*channel));
        let name = if *channel == 1 { "A" } else { "B" };
        o.push_str(&format!(
            "pub const DUTY_{name}_X100: u32 = {x100}; // {} %\n",
            super::common::duty_percent_str(x100)
        ));
    }
    o.push_str("// <<< GENERATED END >>>\n\n");

    o.push_str("// Everything below is editable — your changes are preserved on regeneration.\n");
    o.push_str(&format!(
        "use {hal}::pwm::{{FreeRunning, Pwm{slice}, Slice}};\n"
    ));
    o.push_str("use embedded_hal::pwm::SetDutyCycle;\n\n");

    o.push_str(&format!(
        "/// The slice this module drives. `main.rs` owns the whole set and lends\n/// this one out, because every slice comes from one `PWM` peripheral.\npub type Handle = Slice<Pwm{slice}, FreeRunning>;\n\n"
    ));

    // init
    let params: Vec<String> = chans
        .iter()
        .map(|(channel, n)| {
            let name = if *channel == 1 { "a" } else { "b" };
            format!(
                "    gp{n}: {hal}::gpio::Pin<{hal}::gpio::bank0::Gpio{n}, {hal}::gpio::FunctionNull, {hal}::gpio::PullDown>,\n    // channel {}\n",
                name.to_uppercase()
            )
        })
        .collect();
    o.push_str(&format!(
        "/// Enable the slice and route each wired channel to its pad.\npub fn init(\n    slice: &mut Handle,\n{}) {{\n",
        params.join("")
    ));
    o.push_str("    slice.set_ph_correct();\n");
    // The frequency from the Virtual Module. It used to be dropped here: the
    // blocking emitter set only `ph_correct` and `enable`, so the slice ran at
    // the HAL's default divider whatever the module said, while the async
    // emitter honoured the very same field. Two runtimes, one setting, two
    // answers.
    //
    // Programmed against the clock the Clock tab actually builds, not embassy's
    // default - this backend calls `init_clocks_and_plls` with `PLL_SYS_CFG`.
    //
    // `set_ph_correct` above halves the output: phase-correct counts up AND
    // back down, so one period is 2*(top+1) ticks. The pair is derived for the
    // counter, then the comment says what the pad really sees.
    let want = cfg.map_or(0, |c| c.freq_hz);
    if want > 0 {
        let sys = blocking_sys_hz(mcu);
        // HALF the clock, because `set_ph_correct` above counts up AND back
        // down: one period is 2*(top+1) ticks, not (top+1). Deriving `top` from
        // the whole clock would put the pad at half the frequency asked for -
        // which is what a first cut of this did, arithmetically right and
        // wrong on the wire. The async emitter leaves embassy's
        // `phase_correct: false`, so it needs no such halving; the two runtimes
        // now land on the same output frequency by different routes.
        let (div, top) = pwm_div_top_at(sys / 2, want);
        let got = pwm_actual_hz_at(sys / 2, div, top);
        o.push_str(&format!(
            "    // {want} Hz asked for; phase-correct, so the counter runs at\n"
        ));
        o.push_str(&format!(
            "    // 2 x {want} Hz and the pad sees {got} Hz.\n"
        ));
        o.push_str(&format!("    slice.set_div_int({div});\n"));
        o.push_str("    slice.set_div_frac(0);\n");
        o.push_str(&format!("    slice.set_top({top});\n"));
    }
    o.push_str("    slice.enable();\n");
    for (channel, n) in chans {
        let ch = if *channel == 1 {
            "channel_a"
        } else {
            "channel_b"
        };
        let name = if *channel == 1 { "A" } else { "B" };
        o.push_str(&format!("    slice.{ch}.output_to(gp{n});\n"));
        o.push_str(&format!(
            "    let max = slice.{ch}.max_duty_cycle() as u32;\n"
        ));
        o.push_str(&format!(
            "    slice.{ch}.set_duty_cycle((max * DUTY_{name}_X100 / 10_000) as u16).unwrap();\n"
        ));
    }
    o.push_str("}\n\n");

    // DutyHandle, the same shape as every other backend.
    let first = chans.first().map_or(1, |(c, _)| *c);
    let first_name = if first == 1 { "a" } else { "b" };
    o.push_str("/// Set a channel's duty in the same units the `DUTY_*` constants above use —\n");
    o.push_str("/// HUNDREDTHS of a percent, so `10_000` is 100 % and `750` is 7.5 %.\n///\n");
    o.push_str("/// A trait rather than an inherent method because `Handle` is rp-hal's own\n");
    o.push_str("/// type, which this crate does not own. One method per WIRED channel: the\n");
    o.push_str("/// channel is part of the NAME rather than an argument, so a channel this\n");
    o.push_str("/// slice has no pad for cannot be asked for at all.\n");
    o.push_str("pub trait DutyHandle {\n");
    o.push_str(&format!(
        "    /// Channel {}, the first one wired to this slice.\n    fn set_duty_pwm_{slice}(&mut self, value: u32);\n",
        first_name.to_uppercase()
    ));
    for (channel, _) in chans {
        let name = if *channel == 1 { "a" } else { "b" };
        o.push_str(&format!(
            "\n    /// Channel {}.\n    fn set_duty_pwm_{slice}_{name}(&mut self, value: u32);\n",
            name.to_uppercase()
        ));
    }
    o.push_str("}\n\nimpl DutyHandle for Handle {\n");
    o.push_str(&format!(
        "    fn set_duty_pwm_{slice}(&mut self, value: u32) {{\n        self.set_duty_pwm_{slice}_{first_name}(value);\n    }}\n"
    ));
    for (channel, _) in chans {
        let name = if *channel == 1 { "a" } else { "b" };
        let ch = if *channel == 1 {
            "channel_a"
        } else {
            "channel_b"
        };
        o.push_str(&format!(
            "\n    fn set_duty_pwm_{slice}_{name}(&mut self, value: u32) {{\n"
        ));
        o.push_str(&format!(
            "        let max = self.{ch}.max_duty_cycle() as u32;\n"
        ));
        o.push_str(&format!(
            "        self.{ch}.set_duty_cycle((max * value / 10_000) as u16).unwrap();\n    }}\n"
        ));
    }
    o.push_str("}\n");
    o
}

impl FamilyBackend for RpBackend {
    fn family_id(&self) -> &'static str {
        "rp2040"
    }

    fn handles(&self, family: &str) -> bool {
        is_rp(family)
    }

    /// rp-hal picks pulls through `into_*_input` / `into_push_pull_output`,
    /// which this backend chooses for the user; offering modes it would ignore
    /// would be a lie.
    fn gpio_modes(&self, _func: &PinFunction) -> &'static [GpioMode] {
        &[]
    }

    /// One file per wired PWM slice.
    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        let mut by_slice: std::collections::BTreeMap<u8, Vec<(u8, u8)>> =
            std::collections::BTreeMap::new();
        for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
            let Some(n) = gpio_index(&p.name) else {
                continue;
            };
            if let PinFunction::TimerPwm { timer, channel } = p.selected_function {
                by_slice.entry(timer).or_default().push((channel, n));
            }
        }
        let mut out: Vec<(String, String)> = by_slice
            .into_iter()
            .map(|(slice, mut chans)| {
                chans.sort_unstable();
                (
                    format!("pwm{slice}.rs"),
                    pwm_config_file(mcu, slice, &chans),
                )
            })
            .collect();

        // The three buses that own their peripheral. Only fully wired ones get a
        // file: `init` names every pad in its signature, so half a bus has no
        // signature to write.
        let hal = hal_crate(&mcu.family);
        // The USART modules, so each uart file can carry its own wire frame.
        let ucfgs = crate::panels::mcu_module::modules::usart_configs(&mcu.modules);
        // The I2C modules, so each i2c file can carry its own device address.
        let icfgs = crate::panels::mcu_module::modules::i2c_configs(&mcu.modules);
        for (kind, roles, pins) in [
            ("uart", &["tx", "rx"][..], uart_pins(mcu)),
            ("spi", &["sck", "mosi", "miso"][..], spi_pins(mcu)),
            ("i2c", &["sda", "scl"][..], i2c_pins(mcu)),
        ] {
            for i in instances(&pins) {
                let pads: Vec<(&str, u8)> = roles
                    .iter()
                    .filter_map(|r| role_of(&pins, i, r).map(|n| (*r, n)))
                    .collect();
                if pads.len() == roles.len() {
                    let frame = (kind == "uart").then(|| ucfgs.get(&i)).flatten();
                    // Only the I2C bus is a folder - its devices each get a
                    // file beside its `mod.rs`.
                    let mods = if kind == "i2c" {
                        super::common::i2c_device_mods(icfgs.get(&i))
                    } else {
                        String::new()
                    };
                    let body =
                        bus_config_file(hal, kind, i, &pads, bus_speed(mcu, kind, i), frame, &mods);
                    if kind == "i2c" {
                        out.extend(super::common::i2c_bus_files(
                            &format!("i2c{i}"),
                            body,
                            icfgs.get(&i),
                        ));
                    } else {
                        out.push((format!("{kind}{i}.rs"), body));
                    }
                }
            }
        }
        // Last, and extended INTO this list rather than returned beside it:
        // an empty list drops the whole `configs/` subtree, so a watchdog alone
        // has to count as a config file like any bus.
        out.extend(super::watchdog_gen::rp_config_files(
            &mcu.watchdog,
            &mcu.family,
            false,
        ));
        out
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        format!("{}{}{USER_TAIL}", header(mcu), section(mcu))
    }

    /// Replace ONLY the marked block, keeping what the user wrote on either
    /// side of it.
    ///
    /// Not `embassy_async::splice_section`: that one regenerates everything
    /// ABOVE the markers too, so a runtime switch can rewrite the imports. It is
    /// right for a backend with three runtimes and wrong for this one — reusing
    /// it here rebuilt the file with embassy-stm32's header on a Pico, and took
    /// the user's own `use` lines with it.
    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END))
        else {
            // No block to replace - the file is not ours, so start over rather
            // than splice into something unrecognised.
            return self.fresh_main_rs(mcu);
        };
        let end = end_start + GEN_END.len();
        format!(
            "{}{}{}",
            &existing[..begin],
            section(mcu).trim_end_matches('\n'),
            retarget_pristine_tail(&existing[end..], false)
        )
    }
}

#[cfg(test)]
mod clock_authoring {
    use crate::panels::mcu_module::clock::graph::auto_layout::auto_layout;
    use crate::panels::mcu_module::clock::graph::config::GraphClock;
    use crate::panels::mcu_module::clock::graph::model::{
        ClockGraph, Edge, Node, NodeKind, NodeState,
    };

    /// The Pico clock tree, straight from the datasheet.
    ///
    /// `XOSC` is 12 MHz on both boards. Each PLL multiplies it into a VCO and
    /// then divides twice — that is genuinely how the silicon is arranged, and
    /// modelling it as one opaque "PLL" would make the numbers unexplainable in
    /// the Clock tab.
    ///
    /// `fb` / `pd1` / `pd2` are the datasheet's FBDIV, POSTDIV1 and POSTDIV2.
    fn rp_graph(sys_fb: u32, sys_pd1: usize, sys_pd2: usize) -> ClockGraph {
        let div = |opts: &[u32]| NodeKind::Divider {
            options: opts.to_vec(),
        };
        const PD: [u32; 7] = [1, 2, 3, 4, 5, 6, 7];
        ClockGraph {
            nodes: vec![
                Node {
                    id: "xosc".into(),
                    kind: NodeKind::Source {
                        min_hz: 12_000_000,
                        max_hz: 12_000_000,
                        gated: false,
                    },
                    state: NodeState::Source {
                        enabled: true,
                        hz: 12_000_000,
                    },
                    limit: None,
                },
                Node {
                    id: "pll_sys_fb".into(),
                    kind: NodeKind::Multiplier { min: 16, max: 320 },
                    state: NodeState::Value(sys_fb),
                    limit: None,
                },
                Node {
                    id: "pll_sys_pd1".into(),
                    kind: div(&PD),
                    state: NodeState::Index(sys_pd1),
                    limit: None,
                },
                Node {
                    id: "pll_sys_pd2".into(),
                    kind: div(&PD),
                    state: NodeState::Index(sys_pd2),
                    limit: None,
                },
                Node {
                    id: "clk_sys".into(),
                    kind: NodeKind::Output,
                    state: NodeState::Fixed,
                    limit: None,
                },
                Node {
                    id: "clk_peri".into(),
                    kind: NodeKind::Output,
                    state: NodeState::Fixed,
                    limit: None,
                },
                Node {
                    id: "pll_usb_fb".into(),
                    kind: NodeKind::Multiplier { min: 16, max: 320 },
                    state: NodeState::Value(100),
                    limit: None,
                },
                Node {
                    id: "pll_usb_pd1".into(),
                    kind: div(&PD),
                    state: NodeState::Index(4),
                    limit: None,
                },
                Node {
                    id: "pll_usb_pd2".into(),
                    kind: div(&PD),
                    state: NodeState::Index(4),
                    limit: None,
                },
                Node {
                    id: "clk_usb".into(),
                    kind: NodeKind::Output,
                    state: NodeState::Fixed,
                    limit: None,
                },
                Node {
                    id: "clk_ref".into(),
                    kind: NodeKind::Output,
                    state: NodeState::Fixed,
                    limit: None,
                },
            ],
            edges: vec![
                Edge {
                    from: "xosc".into(),
                    to: "pll_sys_fb".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_sys_fb".into(),
                    to: "pll_sys_pd1".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_sys_pd1".into(),
                    to: "pll_sys_pd2".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_sys_pd2".into(),
                    to: "clk_sys".into(),
                    input: 0,
                },
                Edge {
                    from: "clk_sys".into(),
                    to: "clk_peri".into(),
                    input: 0,
                },
                Edge {
                    from: "xosc".into(),
                    to: "pll_usb_fb".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_usb_fb".into(),
                    to: "pll_usb_pd1".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_usb_pd1".into(),
                    to: "pll_usb_pd2".into(),
                    input: 0,
                },
                Edge {
                    from: "pll_usb_pd2".into(),
                    to: "clk_usb".into(),
                    input: 0,
                },
                Edge {
                    from: "xosc".into(),
                    to: "clk_ref".into(),
                    input: 0,
                },
            ],
        }
    }

    /// Write the `clock:` block for both boards, for splicing into their `.ron`.
    ///
    /// The FIGURE is not hand-placed: `auto_layout` derives it from the graph,
    /// the same way an imported CubeMX tree gets one.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_rp_clock_blocks -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authoring tool: writes the clock blocks to the temp dir"]
    fn emit_rp_clock_blocks() {
        for (name, fb, pd1, pd2, want_hz) in [
            ("rp2040", 125u32, 5usize, 1usize, 125_000_000u32),
            ("rp2350", 125, 4, 1, 150_000_000),
        ] {
            let graph = rp_graph(fb, pd1, pd2);
            // Prove the defaults before writing them down: 12 MHz x FBDIV,
            // divided by POSTDIV1 then POSTDIV2.
            let pd = |i: usize| [1u32, 2, 3, 4, 5, 6, 7][i];
            let got = 12_000_000 * fb / pd(pd1) / pd(pd2);
            assert_eq!(got, want_hz, "{name} clk_sys");
            let usb = 12_000_000 * 100 / 5 / 5;
            assert_eq!(usb, 48_000_000, "clk_usb must be exactly 48 MHz");

            let gc = GraphClock {
                layout: auto_layout(&graph),
                graph,
                bindings: Default::default(),
            };
            let text =
                ron::ser::to_string_pretty(&gc, ron::ser::PrettyConfig::new()).expect("serialise");
            let path = std::env::temp_dir().join(format!("eide_{name}_clock.ron"));
            std::fs::write(&path, text).expect("write");
            println!("wrote {} ({} nodes)", path.display(), gc.graph.nodes.len());
        }
    }
}

#[cfg(test)]
mod emit_for_manual_compile {
    use crate::panels::mcu_module::{builtins, pins::PinFunction, project_gen};

    /// A Pico / Pico 2 project on disk, for a real cross-compile.
    ///
    /// Nothing about this backend is believable until a compiler has seen it:
    /// the boot stage, the PLL sequence and the GPIO bank are all APIs read from
    /// documentation, and documentation is where this session's every wrong
    /// guess came from.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_rp_project -- --ignored --nocapture
    /// cd %TEMP%\eide_rp2040_check && cargo check --target thumbv6m-none-eabi
    /// ```
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_rp_project() {
        for (id, dir_name) in [
            ("rp2040_pico", "eide_rp2040_check"),
            ("rp2350_pico2", "eide_rp2350_check"),
            // The wireless boards generate through the same backend; what
            // differs is that GP23/24/25/29 are the radio's, so the LED cannot
            // be wired and the emitter has to skip it without complaining.
            ("rp2040_pico_w", "eide_rp2040w_check"),
            ("rp2350_pico2_w", "eide_rp2350w_check"),
        ] {
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"));
            let mut mcu = def.build_mcu();
            // The on-board LED and one input, so the GPIO half is exercised.
            for p in mcu.iter_all_pins_mut() {
                // One of each bus, on the pads their FUNCSEL table gives them:
                // UART0 on GP0/1, I2C0 on GP4/5, SPI0 on GP18/19/16. Anything
                // else would be a wiring the chip cannot make.
                match p.name.as_str() {
                    n if n.starts_with("GP25") => p.selected_function = PinFunction::GpioOutput,
                    "GP0" => p.selected_function = PinFunction::UsartTx(0),
                    "GP1" => p.selected_function = PinFunction::UsartRx(0),
                    "GP4" => p.selected_function = PinFunction::I2cSda(0),
                    "GP5" => p.selected_function = PinFunction::I2cScl(0),
                    "GP18" => p.selected_function = PinFunction::SpiSck(0),
                    "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                    "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                    // PWM slice 3 (both channels) and one ADC input.
                    "GP6" => {
                        p.selected_function = PinFunction::TimerPwm {
                            timer: 3,
                            channel: 1,
                        }
                    }
                    "GP7" => {
                        p.selected_function = PinFunction::TimerPwm {
                            timer: 3,
                            channel: 2,
                        }
                    }
                    "GP26" => p.selected_function = PinFunction::AdcChannel { adc: 0, channel: 0 },
                    _ => {}
                }
            }
            // The modules the wiring implies, then non-default duties on the
            // PWM slice: 7.5 % and 10 %, so the generated code cannot pass by
            // accident on the module's default of zero.
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let crate::panels::mcu_module::modules::ModuleConfig::Timer(c) = &mut m.config {
                    c.freq_hz = 20_000;
                    c.set_duty_x100(1, 750);
                    c.set_duty_x100(2, 1_000);
                }
            }
            // The watchdog at the LAST period its driver accepts, so the
            // generated `const` assert is compiled at its boundary - one
            // microsecond more is rp2040-hal's / rp235x-hal's boot panic.
            let (_, max) = crate::panels::mcu_module::watchdog::rp_range_us(&mcu.family, false);
            mcu.watchdog.rp =
                Some(crate::panels::mcu_module::watchdog::RpWdtConfig { timeout_us: max });
            // Two devices on I2C0, one of them unnamed: `device1_oled.rs` and
            // `device2.rs` beside the bus's `mod.rs`.
            assert!(mcu.with_i2c_devices(&[("oled", 0x3C), ("", 0x68)]));
            let main_rs = mcu.fresh_main_rs();
            assert!(
                main_rs.contains("pins::configs::watchdog::init(&mut watchdog);"),
                "{id}: no watchdog in main.rs:\n{main_rs}"
            );
            let files = project_gen::build_project_files(&def.project, &def.toolchain, &main_rs);
            // `sync_pin_files` keeps `src/pins/mod.rs` in a real project, and the
            // generated header declares `pub mod pins;` — so the harness has to
            // supply it too, or it compiles a project shape the app never
            // produces. Which is exactly what happened: the invariant test went
            // green while `cargo check` said "file not found for module `pins`".
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            let _ = std::fs::remove_dir_all(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write rp project");
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        }
    }
}

#[cfg(test)]
mod regeneration {
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// Re-generating must replace the marked block and keep everything else.
    ///
    /// This is the one failure a compiler cannot catch: a splice that drops the
    /// user's loop still produces a file that builds, and the loss is only
    /// noticed later, by the person who wrote it.
    #[test]
    fn regeneration_keeps_the_users_code() {
        let def = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico");
        let mut mcu = def.build_mcu();
        for p in mcu.iter_all_pins_mut() {
            if p.name.starts_with("GP25") {
                p.selected_function = PinFunction::GpioOutput;
            }
        }
        let first = mcu.fresh_main_rs();

        // What a user would add: an import above the block and code below it.
        let edited = first
            .replace(
                "use panic_halt as _;",
                "use panic_halt as _;\nuse my_crate::Thing;",
            )
            .replace(
                "        // Your main loop code here.",
                "        gp25.set_high().unwrap();\n        my_own_helper();",
            );

        // Wire a second pad, so the block genuinely has to change.
        for p in mcu.iter_all_pins_mut() {
            if p.name == "GP16" {
                p.selected_function = PinFunction::GpioInput;
            }
        }
        let again = mcu.update_main_rs(&edited);

        assert!(
            again.contains("use my_crate::Thing;"),
            "import above the block:\n{again}"
        );
        assert!(
            again.contains("my_own_helper();"),
            "code below the block:\n{again}"
        );
        assert!(
            again.contains("pins.gpio16.into_pull_up_input()"),
            "the new pad:\n{again}"
        );
        assert!(
            again.contains("pins.gpio25.into_push_pull_output()"),
            "the old pad:\n{again}"
        );
        // And it must not have grown a second copy of the block.
        assert_eq!(
            again.matches("#[rp2040_hal::entry]").count(),
            1,
            "the generated block was duplicated:\n{again}"
        );
    }
}

#[cfg(test)]
mod header_layout {
    use crate::panels::mcu_module::builtins;

    /// The 40-pin header is numbered like a DIP: down the left side, then UP the
    /// right. So pin 21 sits at the BOTTOM right, facing pin 20 — and since the
    /// canvas draws each side top-to-bottom in list order, the right column has
    /// to be stored descending.
    ///
    /// Generated ascending the first time, which put GP16 at the top right and
    /// VBUS at the bottom: a board that matches no photograph and no silkscreen.
    #[test]
    fn the_header_is_numbered_like_the_silkscreen() {
        for id in ["rp2040_pico", "rp2350_pico2"] {
            let mcu = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu();

            let left: Vec<usize> = mcu.left_pins.iter().map(|p| p.number).collect();
            let right: Vec<usize> = mcu.right_pins.iter().map(|p| p.number).collect();

            assert_eq!(left, (1..=20).collect::<Vec<_>>(), "{id}: left side");
            assert_eq!(
                right,
                (21..=40).rev().collect::<Vec<_>>(),
                "{id}: the right side runs UP the board"
            );
            // The two that face each other at the bottom.
            assert_eq!(*left.last().unwrap(), 20, "{id}");
            assert_eq!(*right.last().unwrap(), 21, "{id}");

            // And the pin those two carry, because the numbers alone would pass
            // on a board whose names were shuffled.
            let name_of = |n: usize| {
                mcu.iter_all_pins()
                    .find(|p| p.number == n)
                    .map(|p| p.name.clone())
                    .unwrap_or_default()
            };
            assert_eq!(name_of(1), "GP0", "{id}");
            assert_eq!(name_of(20), "GP15", "{id}");
            assert_eq!(name_of(21), "GP16", "{id}");
            assert_eq!(name_of(40), "VBUS", "{id}");

            // The four that are on the BOARD but not on the header. They sit on
            // the top edge because that is where they are: the USB connector is
            // at the pin-1 end, and the LED is beside it.
            let top: Vec<String> = mcu.top_pins.iter().map(|p| p.name.clone()).collect();
            assert!(
                top.iter().any(|n| n.starts_with("GP25")),
                "{id}: the LED must be reachable, or the first thing anyone tries cannot be done: {top:?}"
            );
            assert!(
                mcu.bottom_pins.is_empty(),
                "{id}: nothing belongs on the bottom edge"
            );
        }
    }
}

#[cfg(test)]
mod ambiguous_wiring {
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// Two pads claiming one signal must be NAMED, not silently reduced to one.
    ///
    /// GP0 and GP16 are both UART0 TX on this chip — that is the FUNCSEL table,
    /// so a user can wire both without doing anything wrong. rp-hal takes one
    /// pin per role, and the code that chose silently left the other pad
    /// unconfigured with nothing to explain it.
    #[test]
    fn two_pads_on_one_signal_are_both_named() {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                // Both are UART0 TX. Both are legal. Only one can be built.
                "GP0" | "GP16" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                _ => {}
            }
        }
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains("UART0 TX is wired to GP0 and GP16"),
            "the clash must be named:\n{code}"
        );
        assert!(code.contains("Only GP0 is configured"), "{code}");
        // The lowest pad is the one built, so the output does not depend on
        // which order the canvas happened to hand them over.
        // main.rs hands the pad to the config module now, rather than
        // reconfiguring it in place.
        assert!(code.contains("pins.gpio0,"), "{code}");
        assert!(!code.contains("pins.gpio16,"), "{code}");
    }

    /// And an unambiguous project says nothing at all.
    #[test]
    fn a_clean_wiring_gets_no_note() {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                _ => {}
            }
        }
        let code = mcu.fresh_main_rs();
        assert!(
            !code.contains("is wired to GP"),
            "no clash, no note:\n{code}"
        );
    }
}

#[cfg(test)]
mod config_file_shape {
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// The editable half of a config file must sit OUTSIDE the markers.
    ///
    /// `sync_config_files` replaces everything between `<<< GENERATED>>>` and
    /// `<<< GENERATED END >>>` whenever a Virtual Module changes. So whatever
    /// lands inside is regenerated, and whatever the user wrote there is gone —
    /// silently, with no error, on a change as small as nudging a duty slider.
    ///
    /// Constants belong inside. `init`, `Handle` and `DutyHandle` do not: they
    /// are the parts a user rewrites.
    #[test]
    fn only_the_constants_are_regenerated() {
        const BEGIN: &str = "// <<< GENERATED>>>";
        const END: &str = "// <<< GENERATED END >>>";

        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                "GP4" => p.selected_function = PinFunction::I2cSda(0),
                "GP5" => p.selected_function = PinFunction::I2cScl(0),
                "GP18" => p.selected_function = PinFunction::SpiSck(0),
                "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                "GP6" => {
                    p.selected_function = PinFunction::TimerPwm {
                        timer: 3,
                        channel: 1,
                    }
                }
                _ => {}
            }
        }
        let files = mcu.config_files();
        assert_eq!(
            files.len(),
            4,
            "one per peripheral: {:?}",
            files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );

        for (name, body) in &files {
            assert_eq!(body.matches(BEGIN).count(), 1, "{name}: one begin marker");
            assert_eq!(body.matches(END).count(), 1, "{name}: one end marker");
            let b = body.find(BEGIN).unwrap();
            let e = body.find(END).unwrap();
            assert!(b < e, "{name}: markers out of order");

            let inside = &body[b..e];
            let outside = &body[e..];
            for forbidden in ["pub fn init", "pub trait", "pub type Handle"] {
                assert!(
                    !inside.contains(forbidden),
                    "{name}: `{forbidden}` is inside the regenerated block, so a user's \
                     edit to it would be wiped by the next duty change:\n{body}"
                );
            }
            assert!(
                outside.contains("pub fn init"),
                "{name}: init must survive:\n{body}"
            );
            assert!(
                outside.contains("pub type Handle"),
                "{name}: Handle must survive"
            );
            // And the constants are where they belong.
            assert!(
                inside.contains("const "),
                "{name}: nothing regenerated at all?\n{body}"
            );
        }
    }
}

#[cfg(test)]
mod wireless_boards {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::pins::logic::pin::colors::reserved_role;

    /// On a W board the LED is NOT a GPIO, and the board has to say so.
    ///
    /// GP25 drives the on-board LED on a Pico and the CYW43's chip select on a
    /// Pico W. Someone coming from the non-W board will reach for it first, so
    /// the pad is reserved and its explanation names the surprise rather than
    /// leaving them to find it with a meter.
    #[test]
    fn the_led_is_not_a_gpio_on_a_wireless_board() {
        for id in ["rp2040_pico_w", "rp2350_pico2_w"] {
            let mcu = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu();

            let radio: Vec<&str> = mcu
                .iter_all_pins()
                .filter(|p| p.reserved && p.name.starts_with("WL_"))
                .map(|p| p.name.as_str())
                .collect();
            assert_eq!(radio.len(), 4, "{id}: the radio's four lines: {radio:?}");

            // Nothing on a W board may offer GP25 as a pin to configure.
            assert!(
                !mcu.iter_all_pins().any(|p| p.name.starts_with("GP25")),
                "{id}: GP25 is the radio's chip select here, not the LED"
            );
            // And the explanation has to say the thing that surprises people.
            let why = reserved_role("WL_CS");
            assert!(
                why.contains("LED is NOT here"),
                "{id}: the pad must name the surprise, not just its function: {why}"
            );
        }

        // The non-W boards keep their LED, or the change went too far.
        for id in ["rp2040_pico", "rp2350_pico2"] {
            let mcu = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu();
            assert!(
                mcu.iter_all_pins().any(|p| p.name.starts_with("GP25")),
                "{id}: the LED is still a plain GPIO here"
            );
        }
    }
}

/// What the compiler taught me about embassy-rp's async constructors.
///
/// Both facts here compiled to nothing visible in a test that only looked at
/// the emitted text: a project whose buses were never emitted at all still
/// "compiled", and the wrong argument order only shows up as a trait bound.
#[cfg(test)]
mod async_dma_bindings {
    use super::async_bus_lines;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico_with_every_bus() -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = Runtime::Async;
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                "GP4" => p.selected_function = PinFunction::I2cSda(0),
                "GP5" => p.selected_function = PinFunction::I2cScl(0),
                "GP18" => p.selected_function = PinFunction::SpiSck(0),
                "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                _ => {}
            }
        }
        pin_uart_to_dma(&mut mcu);
        mcu
    }

    /// Pin the transport, so this stays a test of the DMA ALLOCATOR.
    ///
    /// `UsartMode` defaults to `Buffered`, and a buffered UART takes no channel
    /// at all - which is the whole point of that transport, and would quietly
    /// leave these assertions measuring an allocator nobody called.
    fn pin_uart_to_dma(mcu: &mut super::Mcu) {
        use crate::panels::mcu_module::modules::{ModuleConfig, UsartMode};
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Usart(c) = &mut m.config {
                c.mode = UsartMode::Dma;
            }
        }
    }

    /// A DMA channel handed to a driver needs its OWN handler bound.
    ///
    /// Not the peripheral's — the channel's. All sixteen drain through the one
    /// DMA_IRQ_0, so the handlers stack up under a single entry, which the
    /// `bind_interrupts!` grammar allows and nothing else in this repo uses.
    #[test]
    fn every_dma_channel_gets_a_handler() {
        let (binding, body, _, _) = async_bus_lines(&pico_with_every_bus());
        // UART takes two channels and SPI two more.
        for ch in 0..4 {
            assert!(
                body.contains(&format!("p.DMA_CH{ch},")),
                "channel {ch} is handed out:
{body}"
            );
            assert!(
                binding.contains(&format!(
                    "dma::InterruptHandler<embassy_rp::peripherals::DMA_CH{ch}>"
                )),
                "channel {ch} is bound:
{binding}"
            );
        }
        assert_eq!(
            binding.matches("DMA_IRQ_0").count(),
            1,
            "one entry, not four:
{binding}"
        );
    }

    /// UART takes the binding BEFORE its channels, SPI after. Same crate.
    #[test]
    fn the_irq_argument_sits_where_each_driver_wants_it() {
        let (_, body, _, _) = async_bus_lines(&pico_with_every_bus());
        let uart = body.split("Uart::new").nth(1).expect("a uart");
        let uart = uart.split("let _").next().unwrap();
        assert!(
            uart.find("Irqs,").unwrap() < uart.find("p.DMA_CH").unwrap(),
            "uart: irq then channels:\n{uart}"
        );
        let spi = body.split("Spi::new").nth(1).expect("a spi");
        let spi = spi.split("let _").next().unwrap();
        assert!(
            spi.find("Irqs,").unwrap() > spi.find("p.DMA_CH").unwrap(),
            "spi: channels then irq:\n{spi}"
        );
    }
}

#[cfg(test)]
mod blocking_pwm_is_generated {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// The panel used to tell a Blocking Pico owner that "this runtime emits no
    /// PWM code at all". It does. This is the proof the note was false.
    #[test]
    fn a_blocking_pico_writes_its_pwm_config_file() {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = Runtime::Blocking;
        for p in mcu.iter_all_pins_mut() {
            if p.name == "GP6" {
                p.selected_function = PinFunction::TimerPwm {
                    timer: 3,
                    channel: 1,
                };
            }
        }
        let files = mcu.config_files();
        assert!(
            files.iter().any(|(name, _)| name == "pwm3.rs"),
            "Blocking writes pwm3.rs: {:?}",
            files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        // ...and main.rs calls into it, so the file is not written to be unused.
        let code = mcu.fresh_main_rs();
        assert!(code.contains("pins::configs::pwm3::init("), "{code}");
    }

    /// The frequency set in the Virtual Module has to reach the BLOCKING file.
    ///
    /// It did not: the blocking emitter wrote `set_ph_correct()` and
    /// `enable()` and nothing else, so a slice asked for 20 kHz ran at the
    /// HAL's default divider - while the async emitter, reading the very same
    /// `freq_hz`, programmed it. One setting, two runtimes, two answers, and
    /// nothing on screen to say so.
    #[test]
    fn a_blocking_slice_is_programmed_to_the_frequency_it_was_given() {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = Runtime::Blocking;
        for p in mcu.iter_all_pins_mut() {
            if p.name == "GP6" {
                p.selected_function = PinFunction::TimerPwm {
                    timer: 3,
                    channel: 1,
                };
            }
        }
        mcu.reconcile_modules();
        let want = 20_000;
        let mut found = false;
        for m in &mut mcu.modules {
            if let crate::panels::mcu_module::modules::ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = want;
                found = true;
            }
        }
        assert!(found, "no Timer module was reconciled for the wired pad");

        let files = mcu.config_files();
        let pwm = &files
            .iter()
            .find(|(n, _)| n == "pwm3.rs")
            .expect("pwm3.rs")
            .1;
        // Not "does it mention a divider" - what does the PAD actually see.
        // Phase-correct counts up and back down, so one period is 2*(top+1)
        // ticks; deriving `top` from the whole clock is arithmetically fine and
        // puts the output at HALF the request, which is the mistake this
        // guards against.
        assert!(
            pwm.contains("set_ph_correct()"),
            "the halving below assumes phase-correct is still on:\n{pwm}"
        );
        let num = |key: &str| -> u32 {
            let at = pwm
                .find(key)
                .unwrap_or_else(|| panic!("no {key} in:\n{pwm}"));
            pwm[at + key.len()..]
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .expect("a number")
        };
        let (div, top) = (num("set_div_int("), num("set_top("));
        let out = 125_000_000 / (2 * div * (top + 1));
        assert_eq!(
            out, want,
            "asked for {want} Hz, the pad gets {out} Hz (div {div}, top {top})"
        );
    }

    /// Against the clock the project really builds, not embassy's default.
    ///
    /// The blocking backend hands `init_clocks_and_plls` the Clock tab's
    /// `PLL_SYS_CFG`, so a project running at something other than 125 MHz
    /// would get a divider computed for a clock it does not have.
    #[test]
    fn the_blocking_divider_follows_the_clock_tab_not_embassys_default() {
        // 125 MHz and 150 MHz are the two the async path assumes; asking the
        // arithmetic for a third shows it is the input, not a constant.
        let (d125, t125) = super::pwm_div_top_at(125_000_000, 20_000);
        let (d100, t100) = super::pwm_div_top_at(100_000_000, 20_000);
        assert_ne!(
            (d125, t125),
            (d100, t100),
            "the same request on a different clock must not give the same pair"
        );
        for (sys, div, top) in [(125_000_000, d125, t125), (100_000_000, d100, t100)] {
            let got = super::pwm_actual_hz_at(sys, div, top);
            let err = (got as i64 - 20_000).abs();
            assert!(err * 1000 <= 20_000, "{sys} Hz sys came out {got} Hz");
        }
    }
}

#[cfg(test)]
mod pwm_frequency {
    use super::{pwm_actual_hz, pwm_div_top};

    /// The pair actually reaches the asked-for frequency, on both chips.
    ///
    /// The two differ: `embassy_rp::init(Default::default())` leaves the RP2040
    /// at 125 MHz and the RP2350 at 150 MHz, so the same request needs a
    /// different `top` on each. One table for both would be wrong on one.
    #[test]
    fn common_frequencies_land_on_the_number_asked_for() {
        for family in ["rp2040", "rp235x"] {
            for want in [50, 1_000, 20_000, 100_000] {
                let (div, top) = pwm_div_top(family, want);
                let got = pwm_actual_hz(family, div, top);
                let err = (got as i64 - want as i64).abs();
                assert!(
                    err * 1000 <= want as i64,
                    "{family} at {want} Hz came out {got} Hz (div {div}, top {top})"
                );
            }
        }
    }

    /// Below what the hardware can reach, it clamps and does not wrap.
    ///
    /// `top` is 16 bits and the divider's integer part is 8. Asking for 1 Hz is
    /// out of range on both chips, and the arithmetic must saturate rather than
    /// produce a `top` of 0 - which is a slice that never counts.
    #[test]
    fn an_unreachable_frequency_clamps() {
        for family in ["rp2040", "rp235x"] {
            let (div, top) = pwm_div_top(family, 1);
            assert!(div <= 255, "{family}: divider fits its 8 bits: {div}");
            assert!(top >= 1, "{family}: a top of 0 never counts");
            // ...and 0 Hz cannot divide by zero.
            let (d0, t0) = pwm_div_top(family, 0);
            assert!(d0 >= 1 && t0 >= 1, "{family}: {d0}/{t0}");
        }
    }

    /// A high frequency needs no divider at all.
    #[test]
    fn a_fast_slice_keeps_the_divider_at_one() {
        assert_eq!(pwm_div_top("rp2040", 100_000).0, 1);
    }
}

#[cfg(test)]
mod bus_speeds {
    use super::bus_speed;
    use crate::panels::mcu_module::builtins;

    /// With no module configured, each bus keeps a sane default.
    ///
    /// The point of the change was that these USED to be the only value - the
    /// generated block claimed "from the Virtual Module" over a literal.
    #[test]
    fn an_unconfigured_bus_falls_back() {
        let mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        assert_eq!(bus_speed(&mcu, "uart", 0), 115_200);
        assert_eq!(bus_speed(&mcu, "spi", 0), 1_000_000);
        assert_eq!(bus_speed(&mcu, "i2c", 0), 400_000);
    }
}

#[cfg(test)]
mod pin_interrupts {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico(runtime: Runtime, arm: Option<Edge>) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = runtime;
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP15" => {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = arm;
                }
                // A plain input beside it, so the two are proved to differ.
                "GP14" => p.selected_function = PinFunction::GpioInput,
                _ => {}
            }
        }
        mcu
    }

    /// An armed input becomes a task that OWNS the pin.
    ///
    /// It cannot stay a binding in `main`: `wait_for_*` takes `&mut self` for as
    /// long as the program runs, so something has to hold it for `'static`.
    #[test]
    fn an_armed_input_becomes_a_task() {
        for (edge, wait) in [
            (Edge::Rising, "wait_for_rising_edge"),
            (Edge::Falling, "wait_for_falling_edge"),
            (Edge::Both, "wait_for_any_edge"),
        ] {
            let code = pico(Runtime::Async, Some(edge)).fresh_main_rs();
            assert!(code.contains("#[embassy_executor::task]"), "{code}");
            assert!(
                code.contains(&format!("pin.{wait}().await;")),
                "{edge:?} waits with {wait}:\n{code}"
            );
            assert!(
                code.contains("spawner.spawn(gp15in_irq(gp15in).unwrap());"),
                "main hands the pin over:\n{code}"
            );
            // The task is a TOP-LEVEL item; nested in `main` it does not compile.
            assert!(
                code.find("#[embassy_executor::task]") < code.find("async fn main"),
                "the task comes first:\n{code}"
            );
            // ...and the plain input beside it is still just a binding.
            assert!(code.contains("let gp14in = Input::new(p.PIN_14"), "{code}");
            assert!(!code.contains("gp14in_irq"), "{code}");
        }
    }

    /// An UNARMED input is untouched — arming is opt-in, not implied.
    #[test]
    fn an_unarmed_input_stays_a_binding() {
        let code = pico(Runtime::Async, None).fresh_main_rs();
        assert!(!code.contains("_irq("), "no task without an edge:\n{code}");
        assert!(code.contains("async fn main(_spawner: Spawner)"), "{code}");
    }

    /// Blocking cannot generate it, and says so rather than dropping it.
    ///
    /// The pin panel offers the edge on every runtime. Before this, setting one
    /// on a Blocking Pico persisted the value and generated nothing at all.
    #[test]
    fn blocking_says_why_rather_than_dropping_it() {
        let code = pico(Runtime::Blocking, Some(Edge::Rising)).fresh_main_rs();
        assert!(!code.contains("embassy_executor::task"), "{code}");
        assert!(
            code.contains("armed for an interrupt"),
            "it says so:\n{code}"
        );
        assert!(code.contains("Switch Runtime to Async"), "{code}");
    }
}

#[cfg(test)]
mod dma_reporting {
    use crate::panels::mcu_module::codegen::family;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico_with_buses(runtime: Runtime) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = runtime;
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                "GP18" => p.selected_function = PinFunction::SpiSck(0),
                "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                _ => {}
            }
        }
        pin_uart_to_dma(&mut mcu);
        mcu
    }

    /// Pin the transport, so this stays a test of the DMA ALLOCATOR.
    ///
    /// `UsartMode` defaults to `Buffered`, and a buffered UART takes no channel
    /// at all - which is the whole point of that transport, and would quietly
    /// leave these assertions measuring an allocator nobody called.
    fn pin_uart_to_dma(mcu: &mut super::Mcu) {
        use crate::panels::mcu_module::modules::{ModuleConfig, UsartMode};
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Usart(c) = &mut m.config {
                c.mode = UsartMode::Dma;
            }
        }
    }

    /// The card reports the channels the code ACTUALLY takes.
    ///
    /// The Pico has no vendor DMA database, so nothing else can answer this —
    /// and before, the card said "nothing used" while four channels were gone.
    #[test]
    fn every_reported_channel_appears_in_the_code() {
        let mcu = pico_with_buses(Runtime::Async);
        let uses = family::dma_uses(&mcu);
        assert_eq!(uses.len(), 4, "UART takes two and SPI two: {uses:?}");
        let code = mcu.fresh_main_rs();
        for u in &uses {
            assert!(
                code.contains(&format!("p.{},", u.peri)),
                "{} is handed out in the code:\n{code}",
                u.peri
            );
            assert_eq!(u.irq, "DMA_IRQ_0");
        }
        // And they are DISTINCT - two drivers on one channel would compile.
        let mut peris: Vec<&str> = uses.iter().map(|u| u.peri.as_str()).collect();
        peris.sort_unstable();
        peris.dedup();
        assert_eq!(peris.len(), 4, "no channel handed out twice");
    }

    /// Blocking takes none, because this backend's buses are polled.
    #[test]
    fn blocking_reports_no_channel() {
        assert!(family::dma_uses(&pico_with_buses(Runtime::Blocking)).is_empty());
    }
}

#[cfg(test)]
mod pio_accounting {
    use super::{PIO_SMS, pio_blocks, pio_uses};
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico(id: &str, runtime: Runtime, take_led: bool) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu();
        mcu.runtime = runtime;
        if take_led {
            for p in mcu.iter_all_pins_mut() {
                if p.name == "WL_LED" {
                    p.selected_function = PinFunction::GpioOutput;
                }
            }
        }
        mcu
    }

    /// THE point of the card: what it lists is what the code actually takes.
    ///
    /// A resource list maintained beside the generator instead of BY it drifts,
    /// and a wrong answer with a confident face is worse than no answer. So the
    /// claim is checked against the emitted text, not against a second table.
    #[test]
    fn the_list_agrees_with_the_generated_code() {
        let mcu = pico("rp2040_pico_w", Runtime::Async, true);
        let code = mcu.fresh_main_rs();
        let uses = pio_uses(&mcu);
        assert_eq!(uses.len(), 1, "the radio takes exactly one state machine");
        let u = &uses[0];
        // Each field has to be findable in the code it claims to describe.
        assert!(
            code.contains(&format!("Pio::new(p.{}", u.block)),
            "{} is the block the code opens:
{code}",
            u.block
        );
        assert!(
            code.contains(&format!("pio.{}", u.sm)),
            "{} is the state machine the code moves out:
{code}",
            u.sm
        );
        assert!(
            code.contains(&format!("{} =>", u.irq)),
            "{} is bound:
{code}",
            u.irq
        );
    }

    /// Blocking takes nothing, because there is nothing to take.
    ///
    /// The codegen emits an explanation there rather than a driver, so a card
    /// claiming PIO0 was busy would be describing code that does not exist.
    #[test]
    fn a_blocking_project_holds_no_state_machine() {
        let mcu = pico("rp2040_pico_w", Runtime::Blocking, true);
        assert!(pio_uses(&mcu).is_empty());
        assert!(!mcu.fresh_main_rs().contains("Pio::new"));
    }

    /// And an untouched pad holds nothing either, on either board.
    #[test]
    fn an_untouched_pad_holds_nothing() {
        for id in ["rp2040_pico_w", "rp2350_pico2_w", "rp2040_pico"] {
            let mcu = pico(id, Runtime::Async, false);
            assert!(pio_uses(&mcu).is_empty(), "{id}");
        }
    }

    /// The third block is the RP2350's headline difference for anyone counting.
    #[test]
    fn the_rp2350_has_a_third_block() {
        assert_eq!(pio_blocks("rp2040") * PIO_SMS, 8);
        assert_eq!(pio_blocks("rp235x") * PIO_SMS, 12);
    }

    /// A chip with no PIO reports none, so the tab can hide the card rather
    /// than print "0 of 0" for hardware that was never there.
    #[test]
    fn a_chip_without_pio_reports_none() {
        use crate::panels::mcu_module::codegen::family;
        let f1 = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F1")
            .build_mcu();
        assert!(family::pio_uses(&f1).is_empty());
        // ...while the Pico reaches the RP producer at all.
        assert_eq!(
            family::pio_uses(&pico("rp2040_pico_w", Runtime::Async, true)).len(),
            1
        );
    }
}

/// Exactly ONE IMAGE_DEF per RP2350 image, on either runtime.
///
/// Blocking's comes from the generated `boot_block`. On Async embassy-rp
/// brings its own, so the generated code must not add a second: it did, and
/// `.start_block` held two. Where the one that remains lands is memory.x's job.
#[cfg(test)]
mod image_def_count {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;

    const IMAGE_SECTION: &str = "#[link_section = \".start_block\"]";

    fn main_rs(id: &str, runtime: Runtime) -> String {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu();
        mcu.runtime = runtime;
        mcu.fresh_main_rs()
    }

    #[test]
    fn one_image_block_per_rp2350_image() {
        for id in ["rp2350_pico2", "rp2350_pico2_w"] {
            let blocking = main_rs(id, Runtime::Blocking);
            assert_eq!(
                blocking.matches(IMAGE_SECTION).count(),
                1,
                "{id} Blocking:\n{blocking}"
            );
            let asynchronous = main_rs(id, Runtime::Async);
            assert_eq!(
                asynchronous.matches(IMAGE_SECTION).count(),
                0,
                "{id} Async:\n{asynchronous}"
            );
        }
        // Which only holds while embassy-rp's own block is left switched on.
        for d in builtins::builtin_definitions() {
            if let Some(line) = &d.project.hal_dep_async {
                assert!(!line.contains("imagedef-none"), "{}: {line}", d.id);
            }
        }
    }

    /// The RP2040 boots through `.boot2` and has no image block at all.
    #[test]
    fn the_rp2040_has_none() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let code = main_rs("rp2040_pico", runtime);
            assert!(!code.contains(".start_block"), "{code}");
        }
    }
}

/// The tinyVision pico2-ice (RP2350B + iCE40UP5K), pad by pad.
///
/// Taken from the Rev2 fabrication netlist (IPC-D-356), NOT from the vendor's
/// `pico2_ice.h`, which is wrong three times over: it names GP22 as the FPGA
/// clock (GP22 is a clock INPUT; the clock is GP21 = GPOUT0), puts I2C on
/// GP12/13 (HSTX lanes that only reach the FFC) and calls GP32..35 SPI1 (the
/// FUNCSEL table says SPI0).
///
/// The table is the single source: `emit_pico2_ice_definition` writes
/// `assets/mcus/rp2350_pico2_ice.ron` from it, and the committed file is
/// checked against it. Everything but the pins - the clock tree, the memory
/// map, the probe - is the Pico 2's, because it is the same die.
///
/// Numbering: J2 is pads 1..40 and J3 is 41..80, each in header order (pin 1
/// is the end away from the USB-C connector); the pads that are on the board
/// but on no 0.1" header number from 81.
#[cfg(test)]
pub(super) mod pico2_ice_board {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::mcu_def::{McuDefinition, PinDef, PinLayout};
    use crate::panels::mcu_module::pins::PinFunction;

    /// What a position carries: a GPIO the user may take, a GPIO that only
    /// lights an LED, or something the board has spoken for.
    #[derive(Clone, Copy)]
    enum Pad {
        /// `Gp(n, note)`: GPIO n with its whole FUNCSEL row, named `GPn (note)`.
        Gp(u8, &'static str),
        /// A GPIO wired only to an on-board LED: an output, or PWM to dim it.
        Led(u8, &'static str),
        /// A line the board owns that the user may switch ON as GPIO Output -
        /// the FPGA's CRESET, which turns the loader on. Named without a `GP`
        /// prefix, so `gpio_index` never binds it as an ordinary pin.
        Switch(&'static str),
        /// Reserved, explained by `colors::reserved_role`.
        Fixed(&'static str),
    }
    use Pad::{Fixed, Gp, Led, Switch};

    /// J2, pins 1..40. Most of it is the FPGA's own I/O: an `ICEn` pad has no
    /// wire to the RP2350 at all.
    const J2: [Pad; 40] = [
        Fixed("ICE12 (flash IO2)"),  // 1
        Switch("ICE_CRESET (GP31)"), // 2: GPIO Output = load the FPGA
        Fixed("ICE13 (flash IO3)"),  // 3
        Fixed("GND"),                // 4
        Fixed("3V3_FPGA"),           // 5
        Fixed("3V3_FPGA"),           // 6
        Fixed("GND"),                // 7
        Fixed("GND"),                // 8
        Fixed("ICE28"),              // 9
        Fixed("ICE31"),              // 10
        Fixed("ICE32"),              // 11
        Fixed("ICE34"),              // 12
        Fixed("ICE36"),              // 13
        Fixed("ICE38"),              // 14
        Fixed("ICE42"),              // 15
        Fixed("ICE43"),              // 16
        Fixed("ICE37"),              // 17
        Fixed("ICE_CLK (GP21)"),     // 18: GP21 through R6, 27 R
        Gp(29, "ICE11"),             // 19
        Fixed("ICE6"),               // 20
        Fixed("ICE10 (SW2)"),        // 21
        Gp(28, "ICE9"),              // 22
        Fixed("VIO_BANK2"),          // 23
        Fixed("VIO_BANK2"),          // 24
        Fixed("GND"),                // 25
        Fixed("GND"),                // 26
        Fixed("ICE48"),              // 27
        Fixed("ICE47"),              // 28
        Fixed("ICE46"),              // 29
        Fixed("ICE45"),              // 30
        Fixed("ICE44 (G6)"),         // 31
        Fixed("ICE2"),               // 32
        Fixed("ICE3"),               // 33
        Fixed("ICE4"),               // 34
        Gp(2, "SDA"),                // 35: 10k pull-up
        Fixed("RUN"),                // 36
        Gp(3, "SCL"),                // 37: 10k pull-up
        Fixed("BOOTSEL"),            // 38
        Fixed("ADC7 (GP47)"),        // 39: through a 2.2k / 10k divider
        Fixed("GND"),                // 40
    ];

    /// J3, pins 1..40: the FPGA's configuration port, the shared RP-ICE PMOD
    /// (5..12), the FPGA's RGB LED, and the RP-only PMOD (23..30).
    const J3: [Pad; 40] = [
        Fixed("ICE_SS (GP5)"),     // 1
        Fixed("ICE_SO (GP7)"),     // 2
        Fixed("ICE_SI (GP4)"),     // 3
        Fixed("ICE_SCK (GP6)"),    // 4: GP6 through R9, 27 R
        Gp(30, "ICE25"),           // 5
        Gp(25, "ICE23"),           // 6
        Gp(23, "ICE19"),           // 7
        Gp(27, "ICE18"),           // 8
        Gp(20, "ICE27"),           // 9
        Gp(24, "ICE26"),           // 10
        Gp(26, "ICE21"),           // 11
        Gp(22, "ICE20"),           // 12: through R34, 27 R
        Fixed("GND"),              // 13
        Fixed("GND"),              // 14
        Fixed("3V3_FPGA"),         // 15
        Fixed("3V3_FPGA"),         // 16
        Fixed("ICE_DONE (GP40)"),  // 17
        Fixed("ICE_LED_R"),        // 18
        Gp(41, "ADC1"),            // 19
        Fixed("ICE_LED_G"),        // 20
        Gp(42, "ADC2/SW1"),        // 21: also reads SW1 through 5.1k
        Fixed("ICE_LED_B"),        // 22
        Gp(33, ""),                // 23
        Gp(37, ""),                // 24
        Gp(35, ""),                // 25
        Gp(39, ""),                // 26
        Gp(32, ""),                // 27
        Gp(36, ""),                // 28
        Gp(34, ""),                // 29
        Gp(38, ""),                // 30
        Fixed("GND"),              // 31
        Fixed("GND"),              // 32
        Fixed("3V3"),              // 33
        Fixed("3V3"),              // 34
        Fixed("VBUS"),             // 35
        Fixed("VIN"),              // 36
        Gp(43, "ADC3"),            // 37
        Gp(44, "ADC4"),            // 38
        Fixed("ADC5/VREF (GP45)"), // 39: R30 ties it to the TL431 shunt
        Gp(46, "ADC6/VREF_EN"),    // 40: powers the TL431 through 1k
    ];

    /// On the board, on no 0.1" header: the RP's own RGB LED and the PSRAM
    /// select, drawn along the top. The LED is common-anode, so LOW lights it -
    /// the reverse of the Pico 2's GP25 - and the name has to say so, because
    /// a non-reserved pad shows nothing else.
    const TOP: [Pad; 4] = [
        Led(0, "LED G, active LOW"),
        Led(1, "LED R, active LOW"),
        Led(9, "LED B, active LOW"),
        Fixed("PSRAM_CS (GP8)"),
    ];

    /// GP10..19 reach only the 22-pin FFC (J6), the HSTX connector.
    const BOTTOM: [Pad; 10] = [
        Gp(10, "FFC"),
        Gp(11, "FFC"),
        Gp(12, "FFC"),
        Gp(13, "FFC"),
        Gp(14, "FFC"),
        Gp(15, "FFC"),
        Gp(16, "FFC"),
        Gp(17, "FFC"),
        Gp(18, "FFC"),
        Gp(19, "FFC"),
    ];

    /// GPIO n's row of the RP2350 FUNCSEL table, in the order every Pico
    /// definition lists it. The same formulas the Pico boards were generated
    /// from, extended past GP29: PWM slices 8..11 serve GP32..47, and there is
    /// no `AdcChannel` because the ADC inputs of a B part are GP40..47, which
    /// rp235x-hal 0.4 does not offer.
    fn gp_functions(n: u8) -> Vec<PinFunction> {
        use PinFunction::*;
        let spi = (n / 8) % 2;
        let uart = ((n / 4 + 1) / 2) % 2;
        let i2c = (n / 2) % 2;
        let slice = if n < 32 {
            (n / 2) % 8
        } else {
            8 + ((n - 32) / 2) % 4
        };
        vec![
            GpioInput,
            GpioOutput,
            match n % 4 {
                0 => SpiMiso(spi),
                1 => SpiNss(spi),
                2 => SpiSck(spi),
                _ => SpiMosi(spi),
            },
            match n % 4 {
                0 => UsartTx(uart),
                1 => UsartRx(uart),
                2 => UsartCts(uart),
                _ => UsartRts(uart),
            },
            if n % 2 == 0 { I2cSda(i2c) } else { I2cScl(i2c) },
            TimerPwm {
                timer: slice,
                channel: 1 + n % 2,
            },
        ]
    }

    fn pin_def(number: usize, pad: Pad) -> PinDef {
        let (name, reserved, functions) = match pad {
            Gp(n, "") => (format!("GP{n}"), false, gp_functions(n)),
            Gp(n, note) => (format!("GP{n} ({note})"), false, gp_functions(n)),
            Led(n, note) => (
                format!("GP{n} ({note})"),
                false,
                gp_functions(n)
                    .into_iter()
                    .filter(|f| matches!(f, PinFunction::GpioOutput | PinFunction::TimerPwm { .. }))
                    .collect(),
            ),
            Switch(name) => (name.to_owned(), false, vec![PinFunction::GpioOutput]),
            Fixed(name) => (name.to_owned(), true, Vec::new()),
        };
        PinDef {
            number,
            name,
            reserved,
            functions,
            af: Vec::new(),
            fn_owner: Vec::new(),
        }
    }

    /// The whole definition: the Pico 2's, with this board's identity and pins.
    pub(in crate::panels::mcu_module::codegen) fn definition() -> McuDefinition {
        let mut d = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2350_pico2")
            .expect("built-in rp2350_pico2");
        d.id = "rp2350_pico2_ice".into();
        d.display_name = "tinyVision pico2-ice (RP2350B + iCE40UP5K)".into();
        d.package = "2x 2x20 headers".into();
        d.board_chip = Some("RP2350B".into());
        d.project.pkg_name = "rp2350_pico2_ice".into();
        // The one HAL difference: the B package's 48 GPIOs. On `rp235xa`
        // embassy-rp has no `PIN_30`..`PIN_47` at all.
        d.project.hal_dep_async = d
            .project
            .hal_dep_async
            .map(|l| l.replace("\"rp235xa\"", "\"rp235xb\""));
        d.project.memory_comment =
            "tinyVision pico2-ice (RP2350B)  -  4 MiB Flash / 520 KiB RAM".into();
        let row = |pads: &[Pad], first: usize| -> Vec<PinDef> {
            pads.iter()
                .enumerate()
                .map(|(i, p)| pin_def(first + i, *p))
                .collect()
        };
        d.pins = PinLayout {
            left: row(&J2, 1),
            right: row(&J3, 41),
            top: row(&TOP, 81),
            bottom: row(&BOTTOM, 81 + TOP.len()),
            grid: None,
        };
        d
    }

    /// Writes the definition for `assets/mcus/`.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_pico2_ice_definition -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "authoring tool: writes rp2350_pico2_ice.ron to the temp dir"]
    fn emit_pico2_ice_definition() {
        let text = ron::ser::to_string_pretty(
            &definition(),
            ron::ser::PrettyConfig::default().struct_names(true),
        )
        .expect("serialise");
        let text = crate::panels::mcu_module::ron_text::bare_none(&text);
        let path = std::env::temp_dir().join("rp2350_pico2_ice.ron");
        std::fs::write(&path, text).expect("write");
        println!("wrote {}", path.display());
    }

    fn board() -> McuDefinition {
        builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2350_pico2_ice")
            .expect("built-in rp2350_pico2_ice")
    }

    /// The committed file is exactly what the table says - pins, identity and
    /// the Pico 2's clock tree alike. A hand edit to the .ron, or a change to
    /// the Pico 2 it borrows from, fails here until the file is regenerated.
    #[test]
    fn the_committed_definition_is_the_netlist() {
        assert!(
            board() == definition(),
            "assets/mcus/rp2350_pico2_ice.ron is stale: run emit_pico2_ice_definition"
        );
    }

    /// The formulas past GP29 are new, so check them where they are not: on
    /// every GPIO the two boards share, the pico2-ice offers exactly what the
    /// hand-checked Pico 2 does - minus the ADC, which an RP2350B has on
    /// GP40..47 instead of GP26..29.
    #[test]
    fn below_gp30_the_functions_are_the_pico2s() {
        let pico2 = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2350_pico2")
            .expect("built-in rp2350_pico2")
            .build_mcu();
        let ice = board().build_mcu();
        let mut compared = 0;
        for p in ice.iter_all_pins().filter(|p| !p.reserved) {
            let Some(n) = super::gpio_index(&p.name) else {
                continue;
            };
            let Some(q) = pico2
                .iter_all_pins()
                .find(|q| super::gpio_index(&q.name) == Some(n) && q.available_functions.len() > 2)
            else {
                continue;
            };
            let theirs: Vec<_> = q
                .available_functions
                .iter()
                .filter(|f| !matches!(f, PinFunction::AdcChannel { .. }))
                .cloned()
                .collect();
            if p.name.contains("(LED") {
                // An LED pad offers a subset - output and PWM - of the same row.
                for f in &p.available_functions {
                    assert!(theirs.contains(f), "GP{n} offers {f:?}");
                }
                assert!(p.available_functions.contains(&PinFunction::GpioOutput));
                continue;
            }
            assert_eq!(p.available_functions, theirs, "GP{n}");
            compared += 1;
        }
        assert!(compared >= 20, "only {compared} pads compared");
    }

    /// The FPGA's configuration lines are the board's, and none of them can be
    /// bound as a GPIO: reserved, and named so `gpio_index` never parses them.
    /// CRESET is the one the user may touch - GPIO Output, and nothing else,
    /// is the loader's switch - and it is not a GPIO either.
    #[test]
    fn the_fpga_lines_are_reserved_and_never_bound() {
        let mcu = board().build_mcu();
        let creset = mcu.iter_all_pins().find(|p| p.number == 2).expect("pad 2");
        assert_eq!(creset.name, "ICE_CRESET (GP31)");
        assert!(!creset.reserved);
        assert_eq!(creset.available_functions, [PinFunction::GpioOutput]);
        assert_eq!(super::gpio_index(&creset.name), None);
        for (number, name) in [
            (18, "ICE_CLK (GP21)"),
            (41, "ICE_SS (GP5)"),
            (42, "ICE_SO (GP7)"),
            (43, "ICE_SI (GP4)"),
            (44, "ICE_SCK (GP6)"),
            (57, "ICE_DONE (GP40)"),
        ] {
            let p = mcu
                .iter_all_pins()
                .find(|p| p.number == number)
                .unwrap_or_else(|| panic!("pad {number}"));
            assert_eq!(p.name, name);
            assert!(p.reserved, "{name}");
            assert_eq!(super::gpio_index(&p.name), None, "{name}");
        }
        // And GP22 is an ordinary pad: it is the vendor header's wrong clock.
        let gp22 = mcu
            .iter_all_pins()
            .find(|p| p.name == "GP22 (ICE20)")
            .expect("GP22");
        assert!(!gp22.reserved);
    }

    /// Each GPIO appears on exactly one pad, and GPIO 0..47 are all accounted
    /// for - as a pad of their own or inside a reserved pad's name.
    #[test]
    fn every_gpio_is_on_exactly_one_pad() {
        let mcu = board().build_mcu();
        let mut seen = [0u8; 48];
        for p in mcu.iter_all_pins() {
            let n = super::gpio_index(&p.name).or_else(|| {
                let (_, rest) = p.name.split_once("(GP")?;
                rest.trim_end_matches(')').parse().ok()
            });
            if let Some(n) = n {
                seen[n as usize] += 1;
            }
        }
        for (n, count) in seen.iter().enumerate() {
            assert_eq!(*count, 1, "GP{n} is on {count} pads");
        }
    }

    /// An untouched board names no FPGA line in its generated code, on either
    /// runtime: nothing takes GP4..7, 21, 31 or 40 until the loader does.
    #[test]
    fn an_untouched_board_leaves_the_fpga_alone() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mut mcu = board().build_mcu();
            mcu.runtime = runtime;
            let code = mcu.fresh_main_rs();
            for n in [4, 5, 6, 7, 21, 31, 40] {
                for name in [format!("gpio{n}"), format!("PIN_{n},"), format!("PIN_{n})")] {
                    assert!(!code.contains(&name), "{runtime:?} names {name}:\n{code}");
                }
            }
        }
    }
}

/// What the FPGA loader puts in main.rs, on both runtimes.
#[cfg(test)]
mod fpga_loader_codegen {
    use super::{dma_uses, fpga_loader, pio_uses};
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    use crate::panels::mcu_module::project_gen::FPGA_BITSTREAM_INCLUDE;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// The pico2-ice, with the loader switched on or not, plus a UART and an
    /// armed input - things that must come AFTER the load.
    fn ice(runtime: Runtime, loader: bool) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2350_pico2_ice")
            .expect("built-in rp2350_pico2_ice")
            .build_mcu();
        mcu.runtime = runtime;
        for p in mcu.iter_all_pins_mut() {
            match super::gpio_index(&p.name) {
                Some(36) => p.selected_function = PinFunction::UsartTx(1),
                Some(37) => p.selected_function = PinFunction::UsartRx(1),
                Some(41) => {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = Some(Edge::Rising);
                }
                _ if loader && p.name.starts_with("ICE_CRESET") => {
                    p.selected_function = PinFunction::GpioOutput
                }
                _ => {}
            }
        }
        mcu.reconcile_modules();
        mcu
    }

    #[test]
    fn an_untouched_creset_emits_nothing() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mcu = ice(runtime, false);
            assert!(!fpga_loader(&mcu));
            let code = mcu.fresh_main_rs();
            assert!(!code.contains("Ice40Cram"), "{runtime:?}:\n{code}");
            assert!(!code.contains("FPGA_BITSTREAM"), "{runtime:?}:\n{code}");
        }
    }

    #[test]
    fn the_loader_is_emitted_on_both_runtimes() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mcu = ice(runtime, true);
            assert!(fpga_loader(&mcu));
            let code = mcu.fresh_main_rs();
            for needle in [
                FPGA_BITSTREAM_INCLUDE,
                "struct Ice40Cram",
                "let fpga_ok = fpga_cram.ice40_load(",
                "fpga_cram.ice40_release();",
                // GP7 is driven for the flash's sleep command only.
                "fpga_cram.ice40_sleep_flash(&mut fpga_so",
            ] {
                assert!(code.contains(needle), "{runtime:?} lacks {needle}:\n{code}");
            }
            // GP31 is the loader's, never an ordinary output binding.
            assert!(!code.contains("let mut gp31"), "{runtime:?}:\n{code}");
        }
    }

    /// The load blocks, and the executor does not poll while it runs - so on
    /// Async it has to come before every spawn, every bus and every pin.
    #[test]
    fn async_loads_before_anything_else_runs() {
        let code = ice(Runtime::Async, true).fresh_main_rs();
        let at = |needle: &str| {
            code.find(needle)
                .unwrap_or_else(|| panic!("no {needle}:\n{code}"))
        };
        let load = at("p.PIN_31");
        assert!(at("embassy_rp::init(") < load);
        assert!(load < at("spawner.spawn"), "{code}");
        assert!(load < at("p.UART1"), "{code}");
        assert!(load < at("p.PIN_41"), "{code}");
    }

    /// A Gpout stops its clock when dropped, so the binding must be a NAMED one
    /// that lives on - `let _ =` would stop the FPGA's clock on the same line.
    #[test]
    fn async_keeps_the_clock_running() {
        let code = ice(Runtime::Async, true).fresh_main_rs();
        assert!(code.contains("let fpga_clk = embassy_rp::clocks::Gpout::new(p.PIN_21);"));
        assert!(code.contains("GpoutSrc::PllUsb"));
        assert!(!code.contains("let _ = embassy_rp::clocks::Gpout"));
        // GP22 is the vendor header's clock pin, and it cannot output one.
        assert!(!code.contains("PIN_22"), "{code}");
    }

    /// Blocking: the pins come out of the bank before any other, and GP21 is
    /// fed from PLL_USB at the frequency the Clock tab gives it.
    #[test]
    fn blocking_takes_the_fpga_pins_first() {
        let code = ice(Runtime::Blocking, true).fresh_main_rs();
        let at = |needle: &str| {
            code.find(needle)
                .unwrap_or_else(|| panic!("no {needle}:\n{code}"))
        };
        assert!(at("let pins = ") < at("pins.gpio31"));
        assert!(at("pins.gpio31") < at("pins.gpio36"), "{code}");
        assert!(code.contains("pins.gpio21.into_function::<rp235x_hal::gpio::FunctionClock>()"));
        assert!(code.contains("// 48 MHz on GP21 (GPOUT0)"), "{code}");
        assert!(!code.contains("gpio22"), "{code}");
        // Released as inputs: `into_floating_disabled` sets the pad's isolation
        // latch while the output is still on, and may keep the RP driving the
        // FPGA's now-user pins.
        assert!(!code.contains("into_floating_disabled"), "{code}");
        assert!(code.contains("fpga_si.into_floating_input()"), "{code}");
    }

    /// The two waits `fpga_load_waveform` replays, pinned as generated.
    ///
    /// `fpga_wait` paces SCK with a third of its count: `delay` counts loop
    /// TURNS, about three cycles each on the M33, and cycles passed straight
    /// through made the configuration clock fall to about 1 MHz. `fpga_settle`
    /// carries the FPGA's minimums and hands `delay` the whole count: "at least
    /// `n` cycles" is all every cortex-m 0.7 promises, and 0.7.7 runs half the
    /// turns 0.7.9 does.
    #[test]
    fn the_waits_are_the_ones_the_replay_models() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let code = ice(runtime, true).fresh_main_rs();
            for needle in [
                "let fpga_mhz = fpga_sys_hz.div_ceil(1_000_000);",
                "let mut fpga_wait = |cycles: u32| cortex_m::asm::delay(cycles / 3);",
                "let mut fpga_settle = |us: u32| cortex_m::asm::delay(us.saturating_mul(fpga_mhz));",
                "fpga_cram.ice40_sleep_flash(&mut fpga_so, fpga_half, &mut fpga_wait, &mut fpga_settle);",
                "fpga_cram.ice40_load(fpga_half, &mut fpga_wait, &mut fpga_settle, FPGA_BITSTREAM);",
            ] {
                assert!(code.contains(needle), "{runtime:?} lacks {needle}:\n{code}");
            }
        }
    }

    /// No PIO, no DMA: the loader is plain pin writes, so it takes nothing the
    /// Configuration tab's cards count.
    #[test]
    fn the_loader_takes_no_pio_and_no_dma() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let with = ice(runtime, true);
            let without = ice(runtime, false);
            assert!(pio_uses(&with).is_empty(), "{runtime:?}");
            assert_eq!(dma_uses(&with), dma_uses(&without), "{runtime:?}");
        }
    }

    /// Only a board with a CRESET pad can switch the loader on.
    #[test]
    fn other_boards_never_emit_it() {
        for id in ["rp2040_pico", "rp2350_pico2", "rp2350_pico2_w"] {
            let mut mcu = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu();
            for p in mcu.iter_all_pins_mut() {
                if p.available_functions.contains(&PinFunction::GpioOutput) {
                    p.selected_function = PinFunction::GpioOutput;
                }
            }
            assert!(!fpga_loader(&mcu), "{id}");
            assert!(!mcu.fresh_main_rs().contains("Ice40Cram"), "{id}");
        }
    }
}

/// An RP project comes back from disk as it was saved.
///
/// The RP backend writes no `// label` on a binding, so `parse_main_rs` reads
/// nothing back and the diagram used to open empty - and the next regeneration
/// wrote a main.rs without the user's pins. On the pico2-ice that silently
/// dropped the FPGA loader, which is a pad function like any other. The store
/// is `@pins` in `mcu.config`, as on nRF; this walks the open path in
/// `project_io`'s order on both runtimes.
#[cfg(test)]
mod pin_restore_rp {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::mcu_config;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn board(id: &str) -> super::Mcu {
        builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu()
    }

    #[test]
    fn a_wired_pico2_ice_comes_back_identical_on_reopen() {
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mut mcu = board("rp2350_pico2_ice");
            mcu.runtime = runtime;
            for p in mcu.iter_all_pins_mut() {
                match super::gpio_index(&p.name) {
                    Some(36) => p.selected_function = PinFunction::UsartTx(1),
                    Some(37) => p.selected_function = PinFunction::UsartRx(1),
                    Some(30) => {
                        p.selected_function = PinFunction::GpioOutput;
                        p.custom_label = "Heartbeat".into();
                    }
                    None if p.name.starts_with("ICE_CRESET") => {
                        p.selected_function = PinFunction::GpioOutput
                    }
                    _ => {}
                }
            }
            mcu.reconcile_modules();
            let code = mcu.fresh_main_rs();
            let cfg = mcu.mcu_config_text();
            assert!(cfg.contains("@pins\n"), "{runtime:?}: written\n{cfg}");

            let mut reopened = board("rp2350_pico2_ice");
            reopened.apply_mcu_config(&cfg);
            reopened.apply_saved_pins_by_number(&mcu_config::parse_pins(&cfg));
            reopened.apply_config_pin_labels(&cfg);

            assert!(
                super::fpga_loader(&reopened),
                "{runtime:?}: the loader survives"
            );
            assert_eq!(
                reopened.fresh_main_rs(),
                code,
                "{runtime:?}: reload changed the generated file"
            );
        }
    }

    /// And on a plain Pico, where the same hole emptied every diagram.
    #[test]
    fn a_wired_pico_comes_back_identical_on_reopen() {
        let mut mcu = board("rp2040_pico");
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                n if n.starts_with("GP25") => p.selected_function = PinFunction::GpioOutput,
                _ => {}
            }
        }
        mcu.reconcile_modules();
        let code = mcu.fresh_main_rs();
        let cfg = mcu.mcu_config_text();
        let mut reopened = board("rp2040_pico");
        reopened.apply_mcu_config(&cfg);
        reopened.apply_saved_pins_by_number(&mcu_config::parse_pins(&cfg));
        assert_eq!(reopened.fresh_main_rs(), code);
    }
}

/// The loader's waveform, replayed against mock pins on a virtual clock - the
/// strongest check there is without a board. `include!` compiles the very text
/// the generator emits, so this tests what users get.
#[cfg(test)]
mod fpga_load_waveform {
    use std::cell::RefCell;
    use std::rc::Rc;

    /// embedded-hal 1.0's two pin traits - the part the loader calls. The IDE
    /// does not depend on the crate; the generated projects compile against
    /// the real one in the verify matrix.
    mod embedded_hal {
        pub mod digital {
            pub trait ErrorType {
                type Error;
            }
            pub trait OutputPin: ErrorType {
                fn set_low(&mut self) -> Result<(), Self::Error>;
                fn set_high(&mut self) -> Result<(), Self::Error>;
            }
            pub trait InputPin: ErrorType {
                fn is_high(&mut self) -> Result<bool, Self::Error>;
            }
        }
    }
    use embedded_hal::digital::{ErrorType, InputPin, OutputPin};

    include!("rp_fpga_load.rs");

    /// The CPU clock the replay runs at: the RP2350's default.
    const MHZ: u32 = 150;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Line {
        Creset,
        Ss,
        Sck,
        Si,
        So,
    }

    /// Every level change on every line, stamped in CPU cycles.
    #[derive(Default)]
    struct Bus {
        now: u64,
        level: [bool; 5],
        events: Vec<(u64, Line, bool)>,
        /// Rising SCK edges after SS goes high before CDONE rises, or never.
        cdone_after: Option<usize>,
    }

    impl Bus {
        fn level_at(&self, line: Line, t: u64) -> bool {
            self.events
                .iter()
                .filter(|(at, l, _)| *at <= t && *l == line)
                .last()
                .is_some_and(|(_, _, v)| *v)
        }
        fn rises(&self, line: Line) -> impl Iterator<Item = u64> + '_ {
            self.events
                .iter()
                .filter(move |(_, l, v)| *l == line && *v)
                .map(|(t, _, _)| *t)
        }
    }

    struct Pin(Rc<RefCell<Bus>>, Line);
    struct Done(Rc<RefCell<Bus>>);

    impl ErrorType for Pin {
        type Error = core::convert::Infallible;
    }
    impl ErrorType for Done {
        type Error = core::convert::Infallible;
    }
    impl Pin {
        fn drive(&mut self, v: bool) {
            let mut bus = self.0.borrow_mut();
            bus.now += 1; // a register write is not free
            if bus.level[self.1 as usize] != v {
                bus.level[self.1 as usize] = v;
                let now = bus.now;
                bus.events.push((now, self.1, v));
            }
        }
    }
    impl OutputPin for Pin {
        fn set_low(&mut self) -> Result<(), Self::Error> {
            self.drive(false);
            Ok(())
        }
        fn set_high(&mut self) -> Result<(), Self::Error> {
            self.drive(true);
            Ok(())
        }
    }
    impl InputPin for Done {
        fn is_high(&mut self) -> Result<bool, Self::Error> {
            let bus = self.0.borrow();
            let Some(after) = bus.cdone_after else {
                return Ok(false);
            };
            let ss_up = bus.rises(Line::Ss).last().unwrap_or(0);
            Ok(bus.rises(Line::Sck).filter(|t| *t > ss_up).count() >= after)
        }
    }

    /// Runs the loader exactly as the generated code calls it, on a `delay`
    /// that spins three cycles per count - cortex-m 0.7.9 on the M33.
    fn replay(image: &[u8], cdone_after: Option<usize>) -> (bool, Bus) {
        replay_with(image, cdone_after, |n| 3 * u64::from(n))
    }

    /// [`replay`] on a `cortex_m::asm::delay(n)` that spins `delay(n)` CPU
    /// cycles. The waits are the generated closures: `fpga_wait` is
    /// `delay(cycles / 3)` and `fpga_settle` is `delay(us * MHz)`
    /// (`the_waits_are_the_ones_the_replay_models`).
    fn replay_with(image: &[u8], cdone_after: Option<usize>, delay: fn(u32) -> u64) -> (bool, Bus) {
        let bus = Rc::new(RefCell::new(Bus {
            cdone_after,
            ..Default::default()
        }));
        let pin = |line| Pin(bus.clone(), line);
        let mut cram = Ice40Cram {
            creset: pin(Line::Creset),
            ss: pin(Line::Ss),
            sck: pin(Line::Sck),
            si: pin(Line::Si),
            cdone: Done(bus.clone()),
        };
        let mut so = pin(Line::So);
        let sys_hz = MHZ * 1_000_000;
        let (mhz, half) = (
            sys_hz.div_ceil(1_000_000),
            (sys_hz / (2 * 4_000_000)).max(1),
        );
        let clock = bus.clone();
        let mut wait = |cycles: u32| clock.borrow_mut().now += delay(cycles / 3);
        let mut settle = |us: u32| clock.borrow_mut().now += delay(us.saturating_mul(mhz));
        cram.ice40_sleep_flash(&mut so, half, &mut wait, &mut settle);
        // The generated code releases GP7 here: from now on nothing drives it.
        drop(so);
        let ok = cram.ice40_load(half, &mut wait, &mut settle, image);
        let _ = cram.ice40_release();
        drop(clock);
        let bus = Rc::try_unwrap(bus).ok().expect("sole owner").into_inner();
        (ok, bus)
    }

    /// What the rising SCK edges carried on `data` while SS was low, in
    /// `[from, to)`, packed MSB first.
    fn shifted(bus: &Bus, data: Line, from: u64, to: u64) -> Vec<u8> {
        let bits: Vec<bool> = bus
            .rises(Line::Sck)
            .filter(|t| *t >= from && *t < to && !bus.level_at(Line::Ss, *t))
            .map(|t| bus.level_at(data, t))
            .collect();
        bits.chunks(8)
            .map(|b| b.iter().fold(0u8, |acc, bit| (acc << 1) | u8::from(*bit)))
            .collect()
    }

    const IMAGE: [u8; 12] = [
        0xFF, 0x00, 0x00, 0xFF, 0x7E, 0xAA, 0x99, 0x7E, 0x01, 0x80, 0x5A, 0xC3,
    ];

    fn creset_rise(bus: &Bus) -> u64 {
        bus.rises(Line::Creset).next().expect("CRESET rises")
    }

    #[test]
    fn slave_mode_is_selected() {
        let (ok, bus) = replay(&IMAGE, Some(20));
        assert!(ok);
        // SS low while CRESET rises is what selects slave mode.
        assert!(!bus.level_at(Line::Ss, creset_rise(&bus)));
        assert_eq!(bus.rises(Line::Creset).count(), 1);
    }

    /// 0xB9 to the FPGA flash on its DI (SO) before the FPGA wakes, and SO
    /// never moves after that.
    #[test]
    fn the_flash_is_put_to_sleep_first() {
        let (_, bus) = replay(&IMAGE, Some(20));
        let rise = creset_rise(&bus);
        assert_eq!(shifted(&bus, Line::So, 0, rise), [0xB9]);
        assert!(
            !bus.events
                .iter()
                .any(|(t, l, _)| *l == Line::So && *t > rise),
            "SO moved while the FPGA loaded"
        );
    }

    /// The FPGA clears its memory for at least 1200 us after CRESET rises;
    /// no clock may reach it before that.
    #[test]
    fn the_fpga_gets_its_clear_time() {
        let (_, bus) = replay(&IMAGE, Some(20));
        let rise = creset_rise(&bus);
        let first = bus.rises(Line::Sck).find(|t| *t > rise).expect("a clock");
        assert!(
            first - rise >= 1200 * u64::from(MHZ),
            "{} cycles",
            first - rise
        );
    }

    /// Mode 3, MSB first: the rising edges with SS low carry exactly the image
    /// - no dummy byte, no trailing byte, which both go out with SS high.
    #[test]
    fn the_image_arrives_whole_and_in_order() {
        let (_, bus) = replay(&IMAGE, Some(20));
        let rise = creset_rise(&bus);
        assert_eq!(shifted(&bus, Line::Si, rise, u64::MAX), IMAGE);
        // Data only ever changes while SCK is low.
        for (t, l, _) in &bus.events {
            if *l == Line::Si {
                assert!(
                    !bus.level_at(Line::Sck, *t),
                    "SI moved with SCK high at {t}"
                );
            }
        }
    }

    /// Clocks after the image, with SS high: until CDONE rises, then at least
    /// 49 more before the FPGA's pins are its design's.
    #[test]
    fn cdone_gets_its_trailing_clocks() {
        for after in [1, 20, 100] {
            let (ok, bus) = replay(&IMAGE, Some(after));
            assert!(ok, "CDONE after {after}");
            let ss_up = bus.rises(Line::Ss).last().unwrap();
            let trailing = bus.rises(Line::Sck).filter(|t| *t > ss_up).count();
            assert!(
                trailing >= after + 49,
                "{trailing} clocks for CDONE at {after}"
            );
            assert!(bus.level[Line::Creset as usize], "CRESET stays high");
        }
    }

    /// No CDONE: the loader gives up after 104 clocks, says so, and puts the
    /// FPGA back into reset - never releasing it to boot its flash image.
    #[test]
    fn a_failed_load_holds_the_fpga_in_reset() {
        for cdone in [None, Some(105)] {
            let (ok, bus) = replay(&IMAGE, cdone);
            assert!(!ok, "{cdone:?}");
            assert!(!bus.level[Line::Creset as usize], "CRESET left high");
            let ss_up = bus.rises(Line::Ss).last().unwrap();
            assert_eq!(bus.rises(Line::Sck).filter(|t| *t > ss_up).count(), 104);
        }
    }

    /// The FPGA's slave SPI takes up to 25 MHz: 20 ns per half period, which is
    /// 3 cycles at 150 MHz. The loader must never be that fast.
    #[test]
    fn sck_never_outruns_the_fpga() {
        let (_, bus) = replay(&IMAGE, Some(20));
        let edges: Vec<u64> = bus
            .events
            .iter()
            .filter(|(_, l, _)| *l == Line::Sck)
            .map(|(t, _, _)| *t)
            .collect();
        let tightest = edges.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        assert!(tightest >= 3, "{tightest} cycles between SCK edges");
    }

    /// All `cortex_m::asm::delay(n)` promises: at least `n` CPU cycles. How
    /// many loop turns it runs changed in 0.7.8 - 0.7.7, still a legal lock
    /// for `cortex-m = "0.7"`, runs half as many - and how long a turn takes
    /// is the core's.
    fn delay_floor(n: u32) -> u64 {
        u64::from(n)
    }

    /// Found by review: the 1300 us clear time went through the same
    /// `delay(cycles / 3)` as the SCK half-periods, which on cortex-m 0.7.7 is
    /// about 650 us - under the FPGA's 1200 us. Replayed at what `delay`
    /// guarantees, not at what one version of it happens to do.
    #[test]
    fn the_clear_time_holds_at_the_delays_floor() {
        let (_, bus) = replay_with(&IMAGE, Some(20), delay_floor);
        let rise = creset_rise(&bus);
        let first = bus.rises(Line::Sck).find(|t| *t > rise).expect("a clock");
        assert!(
            first - rise >= 1200 * u64::from(MHZ),
            "{} us",
            (first - rise) / u64::from(MHZ)
        );
    }

    /// At that floor the SCK half-periods run short - they only set how fast
    /// the image goes in - but never past the FPGA's 25 MHz.
    #[test]
    fn sck_holds_at_the_delays_floor() {
        let (ok, bus) = replay_with(&IMAGE, Some(20), delay_floor);
        assert!(ok);
        let edges: Vec<u64> = bus
            .events
            .iter()
            .filter(|(_, l, _)| *l == Line::Sck)
            .map(|(t, _, _)| *t)
            .collect();
        let tightest = edges.windows(2).map(|w| w[1] - w[0]).min().unwrap();
        assert!(tightest >= 3, "{tightest} cycles between SCK edges");
    }
}

#[cfg(test)]
mod radio_led {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico_w(runtime: Runtime, take_the_led: bool) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico_w")
            .expect("built-in Pico W")
            .build_mcu();
        mcu.runtime = runtime;
        if take_the_led {
            for p in mcu.iter_all_pins_mut() {
                if p.name == "WL_LED" {
                    p.selected_function = PinFunction::GpioOutput;
                }
            }
        }
        mcu
    }

    /// A W board carries no wifi stack until someone asks for the LED.
    ///
    /// The deps are gated on the PAD, not on the board: `cyw43` pulls in a
    /// whole wireless driver, and a Pico W project that only blinks a GPIO has
    /// no business linking one.
    #[test]
    fn an_untouched_pad_emits_nothing() {
        let code = pico_w(Runtime::Async, false).fresh_main_rs();
        assert!(!code.contains("cyw43"), "no radio until asked:\n{code}");
        // And the spawner stays underscored, or the project warns.
        assert!(code.contains("async fn main(_spawner: Spawner)"), "{code}");
    }

    /// Taking it brings up the radio AND wakes the spawner.
    #[test]
    fn taking_the_pad_brings_up_the_radio() {
        let code = pico_w(Runtime::Async, true).fresh_main_rs();
        for want in [
            "cyw43::new(",
            "cyw43_pio::PioSpi::new(",
            "cyw43_pio::RM2_CLOCK_DIVIDER",
            "PIO0_IRQ_0 =>",
            "control.init(clm).await;",
            // The task is a TOP-LEVEL item; inside `main` it does not compile.
            "#[embassy_executor::task]",
        ] {
            assert!(code.contains(want), "missing {want}:\n{code}");
        }
        assert!(
            code.contains("async fn main(spawner: Spawner)"),
            "spawning needs a live spawner:\n{code}"
        );
    }

    /// On Blocking there is no radio code to emit, and no pretending otherwise.
    ///
    /// `cyw43` is async to the bottom. A blocking project that silently dropped
    /// the LED would look like a codegen bug; one that emitted a blocking call
    /// would be fiction. It says what to do instead.
    #[test]
    fn blocking_says_why_rather_than_emitting_fiction() {
        let code = pico_w(Runtime::Blocking, true).fresh_main_rs();
        assert!(
            !code.contains("cyw43"),
            "no async driver on blocking:\n{code}"
        );
        assert!(
            code.contains("Switch Runtime to"),
            "it says what to do:\n{code}"
        );
    }
}

#[cfg(test)]
mod async_hal_line {
    use crate::panels::mcu_module::builtins;

    /// RP is the first family whose HAL CRATE changes with the runtime.
    ///
    /// `rp2040-hal` drives the chip blocking and `embassy-rp` drives it async —
    /// two different crates, not two feature sets. Everything before this had
    /// one HAL per chip, so the model had nowhere to say it.
    ///
    /// The feature is chip-specific, which is why the line lives on the chip and
    /// not in the backend: an RP2350**A** wants `rp235xa`, a **B** wants
    /// `rp235xb`, and a backend deriving it from the family would get one wrong.
    #[test]
    fn the_pico_boards_swap_hal_crate_on_async() {
        for (id, feature) in [
            ("rp2040_pico", "rp2040"),
            ("rp2040_pico_w", "rp2040"),
            ("rp2350_pico2", "rp235xa"),
            ("rp2350_pico2_w", "rp235xa"),
            ("rp2350_pico2_ice", "rp235xb"),
        ] {
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"));

            let blocking = def.project.for_async(false);
            assert!(
                blocking.hal_dep.starts_with("rp2040-hal")
                    || blocking.hal_dep.starts_with("rp235x-hal"),
                "{id}: blocking keeps its own HAL: {}",
                blocking.hal_dep
            );

            let asynchronous = def.project.for_async(true);
            assert!(
                asynchronous.hal_dep.starts_with("embassy-rp"),
                "{id}: async swaps to embassy-rp: {}",
                asynchronous.hal_dep
            );
            assert!(
                asynchronous.hal_dep.contains(feature),
                "{id}: the chip feature has to be this board's: {}",
                asynchronous.hal_dep
            );
        }

        // And a family that does NOT swap is untouched, whichever runtime. (Not
        // the F103 any more: on Async it swaps stm32f1xx-hal for embassy-stm32.)
        let esp = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "esp32c3")
            .expect("built-in ESP32-C3");
        assert_eq!(
            esp.project.for_async(true).hal_dep,
            esp.project.hal_dep,
            "the ESP keeps one HAL for both runtimes"
        );
    }
}

// ── The async runtime, on `embassy-rp` ────────────────────────────────────────
//
// A DIFFERENT HAL from the blocking backend above — `embassy-rp`, not
// `rp2040-hal` — which is why the chip carries a second dependency line. It is
// also the only way to reach the CYW43 radio on a W board, and with it the LED:
// `cyw43` is async to the bone.
//
// Much shorter than the blocking template, because embassy-rp does for you what
// rp-hal makes explicit: `init` sets the clocks up and, on RP2040, supplies the
// second-stage bootloader itself.

pub struct AsyncRpBackend;

/// The GPIO bindings, in header order. embassy-rp names pads `PIN_25`, not
/// `gpio25`, and hands them over from one `Peripherals` struct.
/// What `(divider, top)` actually produces — the number the comment prints.
///
/// Printing the ASKED-FOR frequency beside a divider that cannot reach it is
/// how a generated comment starts lying quietly.
fn pwm_actual_hz(family: &str, div: u32, top: u16) -> u32 {
    pwm_actual_hz_at(async_sys_hz(family), div, top)
}

/// What `embassy_rp::init(Default::default())` leaves the system clock at.
///
/// Only the ASYNC backend may assume this. The blocking one builds its clocks
/// from the Clock tab, so it has to ask that instead - see [`blocking_sys_hz`].
pub(crate) fn async_sys_hz(family: &str) -> u32 {
    if family == "rp235x" {
        150_000_000
    } else {
        125_000_000
    }
}

/// The system clock the BLOCKING backend actually programs, from the Clock tab.
///
/// `init_clocks_and_plls` is handed `PLL_SYS_CFG`, whose VCO and two post
/// dividers are emitted a few lines above the PWM config - so the frequency is
/// known here exactly, and assuming embassy's default would be wrong for every
/// project that touched the Clock tab.
fn blocking_sys_hz(mcu: &Mcu) -> u32 {
    let cfg = pll_from(mcu, "pll_sys", xtal_hz(mcu));
    let mhz = cfg.vco_mhz / cfg.pd1.max(1) / cfg.pd2.max(1);
    mhz.saturating_mul(1_000_000).max(1)
}

/// [`blocking_sys_hz`] without its whole-MHz rounding: clk_peri, which
/// `ClocksManager::init_default` leaves on clk_sys, as the UART divider sees
/// it - the same `(ref * fbdiv) / (pd1 * pd2)` rp-hal's PLL reports, in Hz.
///
/// The PWM helpers are fine with whole megahertz; a baud error is not - a VCO
/// of 1596 MHz over 5 × 2 is 159.6 MHz, which whole megahertz would call 159,
/// moving every rate the UART check reports.
pub(crate) fn blocking_peri_hz(mcu: &Mcu) -> u32 {
    let cfg = pll_from(mcu, "pll_sys", xtal_hz(mcu));
    let div = (cfg.pd1.max(1) * cfg.pd2.max(1)) as u64;
    (cfg.vco_mhz as u64 * 1_000_000 / div).clamp(1, u32::MAX as u64) as u32
}

/// Whether rp-hal accepts BOTH of the Clock tab's PLLs.
///
/// The generated blocking `main` sets up PLL_SYS and PLL_USB with
/// `setup_pll_blocking(..).map_err(|_| false).unwrap()`, so a PLL rp-hal
/// refuses panics the board before any peripheral exists - and the tab offers
/// some: post divider 7, and VCOs outside rp-hal's window. A mirror of
/// `PhaseLockedLoop::new` (pll.rs, identical in rp2040-hal 0.12 and rp235x-hal
/// 0.4 but for the VCO floor), with `refdiv: 1` as emitted.
pub(crate) fn blocking_plls_ok(mcu: &Mcu) -> bool {
    let xtal = xtal_hz(mcu);
    let vco_min_mhz = if mcu.family == "rp235x" { 400 } else { 750 };
    ["pll_sys", "pll_usb"].iter().all(|prefix| {
        let cfg = pll_from(mcu, prefix, xtal);
        let vco_hz = cfg.vco_mhz as u64 * 1_000_000;
        let fbdiv = vco_hz / (xtal.max(1) as u64);
        (vco_min_mhz..=1_600).contains(&cfg.vco_mhz)
            && (1..7).contains(&cfg.pd1)
            && (1..7).contains(&cfg.pd2)
            && (5_000_000..vco_hz / 16).contains(&(xtal as u64))
            && (16..320).contains(&fbdiv)
    })
}

fn pwm_actual_hz_at(sys: u32, div: u32, top: u16) -> u32 {
    sys / (div * (top as u32 + 1))
}

/// `(divider, top)` for a PWM slice asked to run at `freq_hz`.
///
/// The counter runs at `sys / divider` and wraps at `top + 1`, so the output is
/// `sys / (divider * (top + 1))`. `top` is 16 bits and the divider's integer
/// part is 8, which together reach down to about 7 Hz — below that the pair is
/// clamped and the caller says what was actually programmed rather than
/// pretending the asked-for number was met.
fn pwm_div_top(family: &str, freq_hz: u32) -> (u32, u16) {
    pwm_div_top_at(async_sys_hz(family), freq_hz)
}

/// The same arithmetic against a system clock the caller knows.
fn pwm_div_top_at(sys: u32, freq_hz: u32) -> (u32, u16) {
    let freq = freq_hz.max(1);
    let mut div: u32 = 1;
    while div < 255 && sys / (div * freq) > 65_536 {
        div += 1;
    }
    let top = (sys / (div * freq)).saturating_sub(1).clamp(1, 65_535) as u16;
    (div, top)
}

/// The `Input` method that waits for `edge` — the same three embassy-rp offers
/// as embassy-stm32 and esp-hal, which is why an armed pin looks the same here.
fn wait_fn(edge: Edge) -> &'static str {
    match edge {
        Edge::Rising => "wait_for_rising_edge",
        Edge::Falling => "wait_for_falling_edge",
        Edge::Both => "wait_for_any_edge",
    }
}

/// GPIO on the async runtime. Returns `(top-level tasks, main body)`.
///
/// An ARMED input does not become a binding — it becomes a task that OWNS the
/// pin and awaits the edge, so main never sees it again. The same shape ESP on
/// Async uses, for the same reason: `wait_for_*` takes `&mut self` for as long
/// as the program runs, and a task is the cheapest thing that can hold it.
fn async_gpio_lines(mcu: &Mcu) -> (String, String) {
    let mut tasks = String::new();
    // One entry per pin, blank-line separated (see `common::blank_separated`).
    // An armed input's `let` and its `spawner.spawn` belong together, so they
    // are ONE entry — a blank line between them would split a pair that reads
    // as a unit.
    let mut pins_out: Vec<String> = Vec::new();
    for p in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(n) = gpio_index(&p.name) else {
            continue;
        };
        let sfx = var_suffix(&p.selected_function);
        match p.selected_function {
            PinFunction::GpioOutput => pins_out.push(format!(
                "{ALLOW}    let mut gp{n}{sfx} = Output::new(p.PIN_{n}, Level::Low);\n"
            )),
            PinFunction::GpioInput => {
                let Some(edge) = p.irq else {
                    pins_out.push(format!(
                        "{ALLOW}    let gp{n}{sfx} = Input::new(p.PIN_{n}, Pull::Up);\n"
                    ));
                    continue;
                };
                let wait = wait_fn(edge);
                let what = match edge {
                    Edge::Rising => "A rising edge",
                    Edge::Falling => "A falling edge",
                    Edge::Both => "Either edge",
                };
                let name = format!("gp{n}{sfx}");
                tasks.push_str(&format!("/// {what} on GP{n}. The task owns the pin.\n"));
                tasks.push_str("#[embassy_executor::task]\n");
                tasks.push_str(&format!(
                    "async fn {name}_irq(mut pin: Input<'static>) {{\n"
                ));
                tasks.push_str(&format!("    loop {{\n        pin.{wait}().await;\n"));
                tasks.push_str("        // The edge arrived. Your code here.\n    }\n}\n\n");
                pins_out.push(format!(
                    "    let {name} = Input::new(p.PIN_{n}, Pull::Up);\n    spawner.spawn({name}_irq({name}).unwrap());\n"
                ));
            }
            _ => {}
        }
    }
    (tasks, super::common::blank_separated(pins_out))
}

/// The buses, on embassy-rp.
///
/// Async constructors where they exist, which is why some of these need an
/// interrupt binding and some need DMA channels. The channels are handed out in
/// order: embassy-rp exposes twelve as separate peripherals, and nothing else
/// here claims one.
///
/// Returns `(interrupt binding, body)` — the binding is a top-level item and
/// cannot live inside `main`.
fn async_bus_lines(mcu: &Mcu) -> (String, String, String, Vec<super::dma_map::DmaUse>) {
    let mut irqs: Vec<String> = Vec::new();
    let mut o = String::new();
    let mut dma = 0u8;
    // Reported to the Configuration tab straight from this counter. A second
    // table listing the same channels is exactly how a card starts describing
    // an allocation the code no longer makes.
    let mut uses: Vec<super::dma_map::DmaUse> = Vec::new();
    let take = |ch: u8, user: &str, uses: &mut Vec<super::dma_map::DmaUse>| {
        uses.push(super::dma_map::DmaUse {
            peri: format!("DMA_CH{ch}"),
            irq: "DMA_IRQ_0".to_owned(),
            user: user.to_owned(),
            manual: false,
        });
    };

    let uart = uart_pins(mcu);
    let ucfgs = crate::panels::mcu_module::modules::usart_configs(&mcu.modules);
    for i in instances(&uart) {
        let (Some(tx), Some(rx)) = (role_of(&uart, i, "tx"), role_of(&uart, i, "rx")) else {
            o.push_str(&format!("    // UART{i}: TX and RX are taken together.\n"));
            continue;
        };
        let hz = bus_speed(mcu, "uart", i);
        let (bits, parity, stop) = async_frame(ucfgs.get(&i));
        o.push_str(&format!(
            "    let mut ucfg{i} = embassy_rp::uart::Config::default();\n    // From the Virtual Module, not a default: a bus at the wrong speed\n    // is met as garbage on the wire, never as a message - and neither is one\n    // at the wrong frame. All four used to be left on embassy's defaults, so\n    // the three frame combos in the panel changed nothing here.\n    ucfg{i}.baudrate = {hz};\n    ucfg{i}.data_bits = {bits};\n    ucfg{i}.parity = {parity};\n    ucfg{i}.stop_bits = {stop};\n"
        ));
        // The binding NAME is the same on both transports, and so is the
        // keep-alive: user code below GEN_END refers to `uart{i}` and must not
        // notice which one was built.
        if uart_is_buffered(&ucfgs, i) {
            // No channel is taken here, so nothing is reported to the DMA card
            // and every later peripheral keeps the number it would have had
            // without this bus.
            let len = ucfgs.get(&i).map_or(256, |c| c.buf_len.clamp(16, 65_536));
            o.push_str(&format!(
                "    // `BufferedUart` moves bytes on the UART interrupt into these two\n    // software rings - no DMA channel at all. They must be `'static`, which\n    // is what `StaticCell` buys: one runtime check instead of `static mut`.\n    static UART{i}_TX_BUF: static_cell::StaticCell<[u8; {len}]> = static_cell::StaticCell::new();\n    static UART{i}_RX_BUF: static_cell::StaticCell<[u8; {len}]> = static_cell::StaticCell::new();\n"
            ));
            // Argument order is embassy-rp's own and NOT embassy-stm32's: the
            // pins go tx-then-rx, the binding sits between the pads and the
            // buffers, and `new` returns Self - a `.unwrap()` copied from the
            // STM32 template does not compile.
            o.push_str(&format!(
                "    let uart{i} = embassy_rp::uart::BufferedUart::new(\n        p.UART{i},\n        p.PIN_{tx},\n        p.PIN_{rx},\n        Irqs,\n        UART{i}_TX_BUF.init([0; {len}]),\n        UART{i}_RX_BUF.init([0; {len}]),\n        ucfg{i},\n    );\n    let _ = &uart{i};\n"
            ));
            // A DIFFERENT handler type on the same vector. The two disambiguate
            // on the DMA-enable bit, so binding the wrong one is silent at
            // compile time and dead at run time.
            irqs.push(format!(
                "    UART{i}_IRQ => embassy_rp::uart::BufferedInterruptHandler<embassy_rp::peripherals::UART{i}>;"
            ));
        } else {
            let (tdma, rdma) = (dma, dma + 1);
            dma += 2;
            take(tdma, &format!("UART{i} TX"), &mut uses);
            take(rdma, &format!("UART{i} RX"), &mut uses);
            o.push_str(&format!(
                "    let uart{i} = embassy_rp::uart::Uart::new(\n        p.UART{i},\n        p.PIN_{tx},\n        p.PIN_{rx},\n        Irqs,\n        p.DMA_CH{tdma},\n        p.DMA_CH{rdma},\n        ucfg{i},\n    );\n    let _ = &uart{i};\n"
            ));
            irqs.push(format!(
                "    UART{i}_IRQ => embassy_rp::uart::InterruptHandler<embassy_rp::peripherals::UART{i}>;"
            ));
        }
    }

    let spi = spi_pins(mcu);
    for i in instances(&spi) {
        let (Some(sck), Some(mosi), Some(miso)) = (
            role_of(&spi, i, "sck"),
            role_of(&spi, i, "mosi"),
            role_of(&spi, i, "miso"),
        ) else {
            o.push_str(&format!(
                "    // SPI{i}: all three pads are taken together.\n"
            ));
            continue;
        };
        let (tdma, rdma) = (dma, dma + 1);
        dma += 2;
        take(tdma, &format!("SPI{i} TX"), &mut uses);
        take(rdma, &format!("SPI{i} RX"), &mut uses);
        let hz = bus_speed(mcu, "spi", i);
        o.push_str(&format!(
            "    let mut ucfg{i} = embassy_rp::spi::Config::default();\n    // From the Virtual Module, not a default: a bus at the wrong speed\n    // is met as garbage on the wire, never as a message.\n    ucfg{i}.frequency = {hz};\n"
        ));
        o.push_str(&format!(
            "    let spi{i} = embassy_rp::spi::Spi::new(\n        p.SPI{i},\n        p.PIN_{sck},\n        p.PIN_{mosi},\n        p.PIN_{miso},\n        p.DMA_CH{tdma},\n        p.DMA_CH{rdma},\n        Irqs,\n        ucfg{i},\n    );\n    let _ = &spi{i};\n"
        ));
    }

    let i2c = i2c_pins(mcu);
    for i in instances(&i2c) {
        let (Some(sda), Some(scl)) = (role_of(&i2c, i, "sda"), role_of(&i2c, i, "scl")) else {
            o.push_str(&format!("    // I2C{i}: SDA and SCL are taken together.\n"));
            continue;
        };
        let hz = bus_speed(mcu, "i2c", i);
        o.push_str(&format!(
            "    let mut ucfg{i} = embassy_rp::i2c::Config::default();\n    // From the Virtual Module, not a default: a bus at the wrong speed\n    // is met as garbage on the wire, never as a message.\n    ucfg{i}.frequency = {hz};\n"
        ));
        o.push_str(&format!(
            "    let i2c{i} = embassy_rp::i2c::I2c::new_async(\n        p.I2C{i},\n        p.PIN_{scl},\n        p.PIN_{sda},\n        Irqs,\n        ucfg{i},\n    );\n    let _ = &i2c{i};\n"
        ));
        irqs.push(format!(
            "    I2C{i}_IRQ => embassy_rp::i2c::InterruptHandler<embassy_rp::peripherals::I2C{i}>;"
        ));
    }

    let mut pwm: Vec<(u8, u8, u8)> = Vec::new();
    let mut adc: Vec<(u8, u8)> = Vec::new();
    for pin in mcu.iter_all_pins().filter(|p| !p.reserved) {
        let Some(n) = gpio_index(&pin.name) else {
            continue;
        };
        match pin.selected_function {
            PinFunction::TimerPwm { timer, channel } => pwm.push((timer, channel, n)),
            PinFunction::AdcChannel { channel, .. } => adc.push((channel, n)),
            _ => {}
        }
    }
    pwm.sort_unstable();
    let tcfg = crate::panels::mcu_module::modules::timer_configs(&mcu.modules);
    adc.sort_unstable();
    // Grouped by SLICE, because a slice is one peripheral with two outputs and
    // embassy-rp builds it once. The old loop kept a `done` list and skipped
    // every pad after the first of a slice, so wiring GP2 (slice 1 A) and GP3
    // (slice 1 B) generated GP2 only - the second pad configured on the canvas
    // and absent from main.rs. `new_output_ab` exists for exactly this shape.
    let mut by_slice: std::collections::BTreeMap<u8, Vec<(u8, u8)>> =
        std::collections::BTreeMap::new();
    for (slice, channel, n) in &pwm {
        by_slice.entry(*slice).or_default().push((*channel, *n));
    }
    for (slice, chans) in &by_slice {
        // One channel drives one pad. Two pads sixteen apart share each
        // (slice, channel) on this chip, so a canvas can ask for both - say
        // which one lost rather than dropping it in silence.
        let mut pads: std::collections::BTreeMap<u8, u8> = std::collections::BTreeMap::new();
        let mut clashes: Vec<(u8, u8)> = Vec::new();
        for (channel, n) in chans {
            if pads.contains_key(channel) {
                clashes.push((*channel, *n));
            } else {
                pads.insert(*channel, *n);
            }
        }
        o.push_str(&format!(
            "    let mut cfg{slice} = embassy_rp::pwm::Config::default();\n"
        ));
        let freq = tcfg.get(slice).map_or(0, |c| c.freq_hz);
        if freq > 0 {
            let (div, top) = pwm_div_top(&mcu.family, freq);
            let got = pwm_actual_hz(&mcu.family, div, top);
            o.push_str(&format!(
                "    // {freq} Hz asked for; this divider and top give {got} Hz.\n"
            ));
            o.push_str(&format!("    cfg{slice}.divider = {div}u8.into();\n"));
            o.push_str(&format!("    cfg{slice}.top = {top};\n"));
        }
        for (channel, _) in &pads {
            // The duty the Virtual Module carries. `Config::default()` used to
            // go out here whatever the user had set - a setting that looks
            // applied and is not, the same silent drop an armed input used to
            // be.
            let duty = tcfg
                .get(slice)
                .map_or(0, |c| c.duty_x100_of(*channel))
                .min(10_000);
            let ch = if *channel == 1 { "a" } else { "b" };
            o.push_str(&format!(
                "    // Channel {}: {} % of the period.\n",
                ch.to_ascii_uppercase(),
                super::common::duty_percent_str(duty)
            ));
            o.push_str(&format!(
                "    cfg{slice}.compare_{ch} = ((cfg{slice}.top as u32 * {duty}) / 10_000) as u16;\n"
            ));
        }
        for (channel, n) in &clashes {
            let ch = if *channel == 1 { "A" } else { "B" };
            o.push_str(&format!(
                "    // GP{n} also asks for slice {slice} channel {ch}, which GP{} already drives - one channel reaches one pad.\n",
                pads[channel]
            ));
        }
        let args = match (pads.get(&1), pads.get(&2)) {
            (Some(a), Some(b)) => format!(
                "new_output_ab(\n        p.PWM_SLICE{slice},\n        p.PIN_{a},\n        p.PIN_{b},"
            ),
            (Some(a), None) => {
                format!("new_output_a(\n        p.PWM_SLICE{slice},\n        p.PIN_{a},")
            }
            (None, Some(b)) => {
                format!("new_output_b(\n        p.PWM_SLICE{slice},\n        p.PIN_{b},")
            }
            (None, None) => continue,
        };
        o.push_str(&format!(
            "    let pwm{slice} = embassy_rp::pwm::Pwm::{args}\n        cfg{slice},\n    );\n    let _ = &pwm{slice};\n"
        ));
    }

    if !adc.is_empty() {
        o.push_str("    let mut adc = embassy_rp::adc::Adc::new(p.ADC, Irqs, embassy_rp::adc::Config::default());\n    let _ = &mut adc;\n");
        irqs.push("    ADC_IRQ_FIFO => embassy_rp::adc::InterruptHandler;".to_owned());
        for (channel, n) in &adc {
            o.push_str(&format!(
                "    let mut adc{channel} = embassy_rp::adc::Channel::new_pin(p.PIN_{n}, embassy_rp::gpio::Pull::None);\n    let _ = &mut adc{channel};\n"
            ));
        }
    }

    // The radio takes one more channel, from the same counter — two drivers
    // both handed DMA_CH0 would compile and then fight at run time.
    let radio = if radio_led(mcu) {
        let (mut r_irqs, task, body) = radio_lines(dma);
        take(dma, "CYW43 radio - PIO SPI", &mut uses);
        dma += 1;
        irqs.append(&mut r_irqs);
        o.push_str(&body);
        task
    } else {
        String::new()
    };

    if dma > 0 {
        // Every channel drains through the one DMA interrupt, so the
        // handlers for all of them hang off DMA_IRQ_0 together.
        let handlers: Vec<String> = (0..dma)
            .map(|c| {
                format!(
                    "        embassy_rp::dma::InterruptHandler<embassy_rp::peripherals::DMA_CH{c}>"
                )
            })
            .collect();
        irqs.push(format!("    DMA_IRQ_0 =>\n{};", handlers.join(",\n")));
    }

    let binding = if irqs.is_empty() {
        String::new()
    } else {
        format!(
            "// The handlers embassy needs bound before an async peripheral can run.\nembassy_rp::bind_interrupts!(struct Irqs {{\n{}\n}});\n\n",
            irqs.join("\n")
        )
    };
    (binding, o, radio, uses)
}

/// The DMA channels the generated project takes.
///
/// Produced by RUNNING the allocator and throwing the code away, rather than by
/// a second table that lists what it is believed to do. The Pico carries no
/// `DmaDef` — there is no vendor database for it — so this is the ONLY thing
/// that can answer "which channel is free", and a list built beside the
/// allocator would answer it wrong the first time either one changed.
///
/// Blocking takes none: `rp2040-hal` bus setup in this backend is polled.
pub fn dma_uses(mcu: &Mcu) -> Vec<super::dma_map::DmaUse> {
    use crate::panels::mcu_module::mcu::model::Runtime;
    if !matches!(mcu.runtime, Runtime::Async) {
        return Vec::new();
    }
    async_bus_lines(mcu).3
}

/// The device-address consts for the I2C buses this runtime builds INLINE.
///
/// The blocking runtime puts each address in its device's own file under
/// `pins/configs/i2c{n}/`; this one has no such file — `config_files` here returns watchdogs
/// only, because every bus is constructed in `main.rs` — so the const goes where
/// the bus goes. Without this the address a user set in the panel reached the
/// generated code on the blocking runtime and vanished on the async one, which
/// is the same setting behaving differently for no reason the user can see.
///
/// Module scope, above the entry point, and `pub`: at the crate root that is
/// what keeps an as-yet-unused const out of `dead_code` (a private one warns —
/// checked against rustc, not assumed).
///
/// Half a bus is skipped: `async_bus_lines` builds no driver for it, so there
/// would be nothing to address.
fn async_i2c_address_consts(mcu: &Mcu) -> String {
    let pins = i2c_pins(mcu);
    let cfgs = crate::panels::mcu_module::modules::i2c_configs(&mcu.modules);
    let mut o = String::new();
    for i in instances(&pins) {
        if role_of(&pins, i, "sda").is_none() || role_of(&pins, i, "scl").is_none() {
            continue;
        }
        let cfg = cfgs.get(&i);
        let stems = cfg.map_or_else(Vec::new, |c| {
            super::common::legacy_i2c_device_stems(&format!("i2c{i}"), c)
        });
        if stems.is_empty() {
            o.push_str(&super::common::device_address_const(
                Some(&format!("I2C{i}")),
                cfg.map_or(0, |c| c.primary_address()),
            ));
        } else {
            // Several devices on the bus: one const each, named as the device
            // files were before a bus became a folder. Kept, because the code
            // below the markers names them - and a device number in the name
            // would move whenever an earlier device is removed.
            for (stem, addr, _) in stems {
                o.push_str(&super::common::device_address_const(
                    Some(&stem.to_ascii_uppercase()),
                    addr,
                ));
            }
        }
    }
    o
}

/// How many DMA channels this chip has — 12 on the RP2040, 16 on the RP2350.
pub fn dma_channels(family: &str) -> usize {
    if family == "rp235x" { 16 } else { 12 }
}

/// ONE PIO state machine a project actually uses, as REPORTED BY CODEGEN.
///
/// The twin of [`super::dma_map::DmaUse`], and for the same reason: a list of
/// resources that is maintained separately from the code that takes them drifts,
/// and a wrong answer with a confident face is worse than no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PioUse {
    /// The block, as the HAL spells the peripheral — `PIO0`.
    pub block: String,
    /// The state machine inside it — `sm0`.
    pub sm: String,
    /// Who holds it, in words a person can act on.
    pub user: String,
    /// The `bind_interrupts!` vector it needs, empty when it needs none.
    pub irq: String,
}

/// State machines per PIO block. Four on every RP part so far.
pub const PIO_SMS: usize = 4;

/// How many PIO blocks this chip has.
///
/// Two on the RP2040, three on the RP2350 — the third is the headline
/// difference between them for anyone counting state machines.
pub fn pio_blocks(family: &str) -> usize {
    if family == "rp235x" { 3 } else { 2 }
}

/// The PIO state machines the generated project takes.
///
/// Exactly one today: the CYW43 radio's half-duplex SPI, which no SPI block on
/// the chip can produce. It is async-only, so a Blocking project takes nothing
/// even with the LED pad driven — the same asymmetry the codegen has.
pub fn pio_uses(mcu: &Mcu) -> Vec<PioUse> {
    use crate::panels::mcu_module::mcu::model::Runtime;
    if !matches!(mcu.runtime, Runtime::Async) || !radio_led(mcu) {
        return Vec::new();
    }
    vec![PioUse {
        block: "PIO0".to_owned(),
        sm: "sm0".to_owned(),
        user: "CYW43 radio - the wifi chip's half-duplex SPI".to_owned(),
        irq: "PIO0_IRQ_0".to_owned(),
    }]
}

/// Is the on-board LED wired up on a W board?
///
/// `WL_LED` is not a GPIO on the chip at all — it is pin 0 of the CYW43 radio's
/// own GPIO block, which is why a Pico W cannot blink without a wifi driver
/// running. The pad exists on the canvas so the board can OFFER the LED; this
/// asks whether the user took it.
fn radio_led(mcu: &Mcu) -> bool {
    mcu.iter_all_pins()
        .any(|p| p.name == "WL_LED" && p.selected_function == PinFunction::GpioOutput)
}

/// The bring-up for the CYW43 radio, purely so its GPIO0 can drive the LED.
///
/// Everything here is forced by the hardware, not chosen: the radio speaks a
/// half-duplex SPI no SPI block on the chip can produce, so it goes through a
/// PIO program; the driver is `async` all the way down, so there is no blocking
/// path to this LED at all; and the firmware is three Infineon binaries that
/// cannot ship in a generated project.
///
/// Returns `(irq entries, top-level items, main body)` — the task has to sit
/// outside `main`, and the interrupt entries have to join the shared binding.
fn radio_lines(dma: u8) -> (Vec<String>, String, String) {
    let irqs = vec![
        "    PIO0_IRQ_0 => embassy_rp::pio::InterruptHandler<embassy_rp::peripherals::PIO0>;"
            .to_owned(),
    ];

    let task = "/// Drives the radio. `cyw43` does its own SPI, its own event loop and its own
/// power management, so nothing on the LED path works until this runs.
#[embassy_executor::task]
async fn cyw43_task(
    runner: cyw43::Runner<
        'static,
        cyw43::SpiBus<
            embassy_rp::gpio::Output<'static>,
            cyw43_pio::PioSpi<'static, embassy_rp::peripherals::PIO0, 0>,
        >,
    >,
) -> ! {
    runner.run().await
}

"
    .to_owned();

    let body = format!(
        "    // The radio's firmware, written into `firmware/` with this project. They
    // are Infineon binaries under the Permissive Binary License, which is what
    // lets them ship; the licence text sits beside them. Replacing one with
    // your own build is safe - the IDE never overwrites a file already there,
    // and builds with the one in your project folder.
    //
    // `nvram_rp2040.bin` is right on a Pico 2 W too: the name is the board it
    // was measured on, not the chip it runs on.
    let fw = cyw43::aligned_bytes!(\"../firmware/43439A0.bin\");
    let clm = cyw43::aligned_bytes!(\"../firmware/43439A0_clm.bin\");
    let nvram = cyw43::aligned_bytes!(\"../firmware/nvram_rp2040.bin\");

    // GP23/24/25/29 are the radio's, which is why the canvas reserves them.
    let pwr = embassy_rp::gpio::Output::new(p.PIN_23, Level::Low);
    let cs = embassy_rp::gpio::Output::new(p.PIN_25, Level::High);
    let mut pio = embassy_rp::pio::Pio::new(p.PIO0, Irqs);
    let spi = cyw43_pio::PioSpi::new(
        &mut pio.common,
        pio.sm0,
        // The RM2 divider, not the default one: the module on these boards does
        // not hold the link together at the faster clock.
        cyw43_pio::RM2_CLOCK_DIVIDER,
        pio.irq0,
        cs,
        p.PIN_24,
        p.PIN_29,
        embassy_rp::dma::Channel::new(p.DMA_CH{dma}, Irqs),
    );

    static RADIO_STATE: static_cell::StaticCell<cyw43::State> = static_cell::StaticCell::new();
    let state = RADIO_STATE.init(cyw43::State::new());
    let (_net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw, nvram).await;
    // embassy-executor 0.10: the task FUNCTION returns the Result (the pool can
    // be exhausted), so the `unwrap` goes inside `spawn`, not after it.
    spawner.spawn(cyw43_task(runner).unwrap());
    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    // The LED is GPIO0 ON THE RADIO, so it is driven through `control` rather
    // than through a pin: `wl_led.gpio_set(0, true).await` turns it on.
    #[allow(unused_mut, unused_variables)]
    let mut wl_led = control;
"
    );

    (irqs, task, body)
}

// ── The pico2-ice's FPGA loader ───────────────────────────────────────────────

/// The loader itself, the same on both runtimes: plain generic Rust over
/// embedded-hal's pin traits. It lives in a file of its own - NOT a module of
/// this crate - because two things read it: the generator copies it into
/// main.rs, and `fpga_load_waveform` compiles the very same text against mock
/// pins and checks the waveform it draws.
const FPGA_LOADER: &str = include_str!("rp_fpga_load.rs");

/// Is the FPGA loader switched on?
///
/// The switch is the board's own CRESET pad set to GPIO Output - the line the
/// loader drives. Left alone, the board's pull-down holds the FPGA in reset,
/// which is exactly what "no loader" should mean. `pub` so everything else that
/// needs to know - the harness, the tests - asks the question the emitter does.
pub fn fpga_loader(mcu: &Mcu) -> bool {
    mcu.iter_all_pins()
        .any(|p| p.name.starts_with("ICE_CRESET") && p.selected_function == PinFunction::GpioOutput)
}

/// The loader's top-level items: the bitstream, the clock ceiling, the loader.
fn fpga_items() -> String {
    let mut o = String::new();
    o.push_str("/// The FPGA's bitstream: `fpga/top.bin` in the project folder. The IDE put\n");
    o.push_str("/// its own default design there once; replace the file with yours.\n");
    o.push_str(&format!(
        "static FPGA_BITSTREAM: &[u8] = {};\n\n",
        crate::panels::mcu_module::project_gen::FPGA_BITSTREAM_INCLUDE
    ));
    o.push_str("/// The configuration clock the loader aims for. The FPGA takes up to 25 MHz,\n");
    o.push_str("/// so a `delay` that runs short - cortex-m 0.7.7 spins about half as long -\n");
    o.push_str("/// stays far inside it.\n");
    o.push_str("const FPGA_SPI_HZ: u32 = 4_000_000;\n\n");
    o.push_str(FPGA_LOADER);
    o.push('\n');
    o
}

/// The part every runtime shares: why the load comes first.
const FPGA_BODY_HEAD: &str = "    // ── FPGA (iCE40UP5K) ──
    // Before anything else can touch GP4..7: they are the FPGA's configuration
    // port AND its flash's bus. CRESET low holds the FPGA off them until the
    // load (the board pulls it low too).
";

/// The loader's lines in an Async `main`, right after init and the watchdog.
///
/// It blocks for about half a second at 150 MHz, which is why it runs before
/// anything is spawned: the executor does not poll while it runs.
const ASYNC_FPGA_BODY: &str =
    "    let fpga_creset = embassy_rp::gpio::Output::new(p.PIN_31, embassy_rp::gpio::Level::Low);
    // 48 MHz on GP21 (GPOUT0) into FPGA pin 35: PLL_USB, which embassy-rp
    // starts at 48 MHz, divided by 1. Keep the binding - dropping a Gpout
    // stops the clock.
    let fpga_clk = embassy_rp::clocks::Gpout::new(p.PIN_21);
    fpga_clk.set_src(embassy_rp::clocks::GpoutSrc::PllUsb);
    fpga_clk.set_div(1, 0);
    fpga_clk.enable();
    let mut fpga_cram = Ice40Cram {
        creset: fpga_creset,
        ss: {
            let mut ss = embassy_rp::gpio::Flex::new(p.PIN_5);
            ss.set_high();
            ss.set_as_output();
            ss
        },
        sck: embassy_rp::gpio::Output::new(p.PIN_6, embassy_rp::gpio::Level::High),
        si: embassy_rp::gpio::Output::new(p.PIN_4, embassy_rp::gpio::Level::Low),
        // No pull: the board has its own 2.2k pull-up on CDONE.
        cdone: embassy_rp::gpio::Input::new(p.PIN_40, embassy_rp::gpio::Pull::None),
    };
    let fpga_sys_hz = embassy_rp::clocks::clk_sys_freq();
    let fpga_mhz = fpga_sys_hz.div_ceil(1_000_000);
    let fpga_half = (fpga_sys_hz / (2 * FPGA_SPI_HZ)).max(1);
    // Paces SCK only: `delay` spins about three cycles per turn on the M33, so
    // a count of cycles is a third as many turns.
    let mut fpga_wait = |cycles: u32| cortex_m::asm::delay(cycles / 3);
    // The FPGA's hard minimums, in microseconds. `delay(n)` promises at least
    // `n` cycles and nothing more - how many turns it counts changed between
    // cortex-m versions - so these hand it the whole count.
    let mut fpga_settle = |us: u32| cortex_m::asm::delay(us.saturating_mul(fpga_mhz));
    // The FPGA's flash first: asleep, it ignores the load. GP7, its data in,
    // then stops being driven - it is the FPGA's own SPI_SO from here on.
    let mut fpga_so = embassy_rp::gpio::Flex::new(p.PIN_7);
    fpga_so.set_low();
    fpga_so.set_as_output();
    fpga_cram.ice40_sleep_flash(&mut fpga_so, fpga_half, &mut fpga_wait, &mut fpga_settle);
    fpga_so.set_as_input();
    fpga_so.set_pull(embassy_rp::gpio::Pull::Down);
    // Whether the FPGA took the image (CDONE high). On failure CRESET is
    // already back LOW, holding the FPGA in reset.
    #[allow(unused_variables)]
    let fpga_ok = fpga_cram.ice40_load(fpga_half, &mut fpga_wait, &mut fpga_settle, FPGA_BITSTREAM);
    // SCK, SI and SO go back to no function. SS becomes a pulled-up input, so
    // the flash stays deselected. `fpga_reset` driven LOW stops the FPGA.
    #[allow(unused_variables)]
    let (fpga_reset, mut fpga_ss, fpga_sck, fpga_si, fpga_done) = fpga_cram.ice40_release();
    drop((fpga_sck, fpga_si, fpga_so));
    fpga_ss.set_as_input();
    fpga_ss.set_pull(embassy_rp::gpio::Pull::Up);

";

/// The loader's lines in a Blocking `main`, right after the GPIO bank is
/// taken. `usb_mhz` is what PLL_USB runs at, which is what reaches the FPGA.
fn blocking_fpga_body(hal: &str, usb_mhz: u32) -> String {
    let mut o = String::new();
    o.push_str(&format!(
        "    let fpga_creset = pins.gpio31.into_push_pull_output_in_state({hal}::gpio::PinState::Low);\n"
    ));
    o.push_str(&format!(
        "    // {usb_mhz} MHz on GP21 (GPOUT0) into FPGA pin 35: PLL_USB as the Clock tab\n"
    ));
    o.push_str("    // sets it, divided by 1.\n");
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str(&format!(
        "    let fpga_clk = pins.gpio21.into_function::<{hal}::gpio::FunctionClock>();\n"
    ));
    o.push_str("    clocks\n        .gpio_output0_clock\n");
    o.push_str("        .configure_clock(&pll_usb, pll_usb.operating_frequency())\n");
    o.push_str("        .map_err(|_| false)\n        .unwrap();\n");
    o.push_str("    let mut fpga_cram = Ice40Cram {\n");
    o.push_str("        creset: fpga_creset,\n");
    for (field, gpio, level) in [("ss", 5, "High"), ("sck", 6, "High"), ("si", 4, "Low")] {
        o.push_str(&format!(
            "        {field}: pins.gpio{gpio}.into_push_pull_output_in_state({hal}::gpio::PinState::{level}),\n"
        ));
    }
    o.push_str("        // Floating: the board has its own 2.2k pull-up on CDONE.\n");
    o.push_str("        cdone: pins.gpio40.into_floating_input(),\n");
    o.push_str("    };\n");
    o.push_str("    let fpga_sys_hz = clocks.system_clock.freq().to_Hz();\n");
    o.push_str("    let fpga_mhz = fpga_sys_hz.div_ceil(1_000_000);\n");
    o.push_str("    let fpga_half = (fpga_sys_hz / (2 * FPGA_SPI_HZ)).max(1);\n");
    o.push_str("    // Paces SCK only: `delay` spins about three cycles per turn on the M33, so\n");
    o.push_str("    // a count of cycles is a third as many turns.\n");
    o.push_str("    let mut fpga_wait = |cycles: u32| cortex_m::asm::delay(cycles / 3);\n");
    o.push_str("    // The FPGA's hard minimums, in microseconds. `delay(n)` promises at least\n");
    o.push_str("    // `n` cycles and nothing more - how many turns it counts changed between\n");
    o.push_str("    // cortex-m versions - so these hand it the whole count.\n");
    o.push_str(
        "    let mut fpga_settle = |us: u32| cortex_m::asm::delay(us.saturating_mul(fpga_mhz));\n",
    );
    o.push_str("    // The FPGA's flash first: asleep, it ignores the load. GP7, its data in,\n");
    o.push_str("    // then stops being driven - it is the FPGA's own SPI_SO from here on.\n");
    o.push_str(&format!(
        "    let mut fpga_so = pins.gpio7.into_push_pull_output_in_state({hal}::gpio::PinState::Low);\n"
    ));
    o.push_str(
        "    fpga_cram.ice40_sleep_flash(&mut fpga_so, fpga_half, &mut fpga_wait, &mut fpga_settle);\n",
    );
    o.push_str("    let fpga_so = fpga_so.into_pull_down_input();\n");
    o.push_str("    // Whether the FPGA took the image (CDONE high). On failure CRESET is\n");
    o.push_str("    // already back LOW, holding the FPGA in reset.\n");
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str(
        "    let fpga_ok = fpga_cram.ice40_load(fpga_half, &mut fpga_wait, &mut fpga_settle, FPGA_BITSTREAM);\n",
    );
    o.push_str("    // SCK, SI and SO become plain inputs, so the RP no longer drives them. SS\n");
    o.push_str("    // is pulled up, so the flash stays deselected. `fpga_reset` driven LOW\n");
    o.push_str("    // stops the FPGA.\n");
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str("    let (fpga_reset, fpga_ss, fpga_sck, fpga_si, fpga_done) = fpga_cram.ice40_release();\n");
    o.push_str("    let _ = (\n");
    o.push_str("        fpga_sck.into_floating_input(),\n");
    o.push_str("        fpga_si.into_floating_input(),\n");
    o.push_str("        fpga_so.into_floating_input(),\n");
    o.push_str("    );\n");
    o.push_str("    #[allow(unused_variables)]\n");
    o.push_str("    let fpga_ss = fpga_ss.into_pull_up_input();\n\n");
    o
}

fn async_section(mcu: &Mcu) -> String {
    let (irq_binding, buses, radio_task, _) = async_bus_lines(mcu);
    let (gpio_tasks, gpio_body) = async_gpio_lines(mcu);
    // An armed input needs a live spawner just as much as the radio does.
    let spawner = if radio_task.is_empty() && gpio_tasks.is_empty() {
        "_spawner"
    } else {
        "spawner"
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN);
    o.push('\n');
    // No IMAGE_DEF here on the RP2350, unlike Blocking's `boot_block`:
    // embassy-rp emits its own secure_exe block unless `imagedef-none` is set,
    // and a second copy made `.start_block` hold two. memory.x places the one
    // that remains.
    o.push_str(&irq_binding);
    o.push_str(&async_i2c_address_consts(mcu));
    o.push_str(&radio_task);
    o.push_str(&gpio_tasks);
    let fpga = fpga_loader(mcu);
    if fpga {
        o.push_str(&fpga_items());
    }
    o.push_str("#[embassy_executor::main]\n");
    o.push_str(&format!("async fn main({spawner}: Spawner) {{\n"));
    o.push_str("    // embassy-rp brings up the clocks itself. On RP2040 it also supplies the\n");
    o.push_str("    // second-stage bootloader, which the blocking HAL makes you declare.\n");
    o.push_str("    let p = embassy_rp::init(Default::default());\n\n");
    // First after init, like every other family's watchdog: one meant to catch
    // a hang in start-up is worth having before the code that might hang.
    o.push_str(&super::watchdog_gen::rp_init_lines(&mcu.watchdog, true));
    // Then the FPGA, before any pin, bus or task: it blocks while it loads, and
    // it must own GP4..7 before anything else could drive them.
    if fpga {
        o.push_str(FPGA_BODY_HEAD);
        o.push_str(ASYNC_FPGA_BODY);
    }
    o.push_str(&gpio_body);
    o.push_str(&buses);
    o.push_str(GEN_END);
    o.push('\n');
    o
}

fn async_header(mcu: &Mcu) -> String {
    format!(
        "{AUTOGEN_BANNER}\n\
         // MCU: {} | HAL: embassy-rp (async)\n\
         {}\n\
         #![no_std]\n\
         #![no_main]\n\
         \n\
         pub mod pins;\n\
         \n\
         use embassy_executor::Spawner;\n\
         #[allow(unused_imports)]\n\
         use embassy_rp::gpio::{{Input, Level, Output, Pull}};\n\
         use panic_halt as _;\n\
         \n",
        mcu.name,
        mcu_id_marker_line(&mcu.id),
    )
}

impl FamilyBackend for AsyncRpBackend {
    /// A LABEL — dispatch is by runtime, via `backend_for_runtime`.
    fn family_id(&self) -> &'static str {
        "rp-async"
    }

    fn handles(&self, family: &str) -> bool {
        is_rp(family)
    }

    /// embassy-rp takes the pull in `Input::new`, which this backend chooses.
    fn gpio_modes(&self, _func: &PinFunction) -> &'static [GpioMode] {
        &[]
    }

    /// Only the watchdog: every bus on this runtime is built inline in
    /// main.rs. Without this the file would never be written while main.rs
    /// called `pins::configs::watchdog::init` all the same.
    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        super::watchdog_gen::rp_config_files(&mcu.watchdog, &mcu.family, true)
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        format!(
            "{}{}\n{ASYNC_USER_TAIL}",
            async_header(mcu),
            async_section(mcu),
        )
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END))
        else {
            return self.fresh_main_rs(mcu);
        };
        let end = end_start + GEN_END.len();
        format!(
            "{}{}{}",
            &existing[..begin],
            async_section(mcu).trim_end_matches('\n'),
            retarget_pristine_tail(&existing[end..], true)
        )
    }
}

/// The three wire-frame settings reach the generated project.
///
/// They are the UART itself — data bits, parity, stop bits — so the panel drew
/// them on every chip. This backend read none of them: the blocking template
/// hardcoded `DataBits::Eight, None, StopBits::One` and the async one set the
/// baud rate and left the rest on embassy's defaults. Set 8-E-2 in the Virtual
/// Module, save, and the board sent 8-N-1 while the panel went on showing
/// Even/2 — the peer sees framing errors and nothing in the IDE disagrees.
#[cfg(test)]
mod the_wire_frame_reaches_the_board {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::modules::{ModuleConfig, Parity, StopBits, UsartMode};
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// A Pico with UART0 on GP0/GP1, through the reconciler so the module and
    /// its config really exist.
    fn pico(
        runtime: Runtime,
        edit: impl FnOnce(&mut crate::panels::mcu_module::modules::UsartModuleConfig),
    ) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = runtime;
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                _ => {}
            }
        }
        mcu.reconcile_modules();
        let mut edit = Some(edit);
        for m in &mut mcu.modules {
            if let ModuleConfig::Usart(c) = &mut m.config
                && let Some(f) = edit.take()
            {
                f(c);
            }
        }
        mcu
    }

    fn uart0(mcu: &super::Mcu) -> String {
        mcu.config_files()
            .into_iter()
            .find(|(n, _)| n == "uart0.rs")
            .map(|(_, c)| c)
            .expect("a uart0.rs")
    }

    /// Blocking: the frame lands in the GENERATED consts, so `init` below stays
    /// the user's to edit while the values keep following the module.
    #[test]
    fn the_blocking_config_file_carries_the_frame() {
        let mcu = pico(Runtime::Blocking, |c| {
            c.parity = Parity::Even;
            c.stop_bits = StopBits::Two;
        });
        let src = uart0(&mcu);
        assert!(
            src.contains("pub const PARITY: Option<rp2040_hal::uart::Parity> = Some(rp2040_hal::uart::Parity::Even);"),
            "the parity is emitted:\n{src}"
        );
        assert!(
            src.contains(
                "pub const STOP_BITS: rp2040_hal::uart::StopBits = rp2040_hal::uart::StopBits::Two;"
            ),
            "and the stop bits:\n{src}"
        );
        assert!(
            src.contains("DATA_BITS,\n                PARITY,\n                STOP_BITS,"),
            "and `UartConfig::new` takes them rather than three literals:\n{src}"
        );
        assert!(
            !src.contains("DataBits::Eight,\n                None,"),
            "the hardcoded frame is gone:\n{src}"
        );
    }

    /// No parity is the ABSENCE of the option, not a variant called None —
    /// `UartConfig::new` takes `Option<Parity>`, and rp-hal's `Parity` has two
    /// variants, `Odd` and `Even`. A `Parity::None` would not compile.
    #[test]
    fn no_parity_is_the_absent_option() {
        for (parity, want) in [
            (Parity::None, "None"),
            (Parity::Even, "Some(rp2040_hal::uart::Parity::Even)"),
            (Parity::Odd, "Some(rp2040_hal::uart::Parity::Odd)"),
        ] {
            let src = uart0(&pico(Runtime::Blocking, |c| c.parity = parity));
            assert!(
                src.contains(&format!(
                    "pub const PARITY: Option<rp2040_hal::uart::Parity> = {want};"
                )),
                "{parity:?} is emitted as {want}:\n{src}"
            );
        }
        for (stop, want) in [(StopBits::One, "One"), (StopBits::Two, "Two")] {
            let src = uart0(&pico(Runtime::Blocking, |c| c.stop_bits = stop));
            assert!(
                src.contains(&format!(
                    "pub const STOP_BITS: rp2040_hal::uart::StopBits = rp2040_hal::uart::StopBits::{want};"
                )),
                "{stop:?} is emitted as {want}:\n{src}"
            );
        }
    }

    /// Async: the whole `Config`, not just the baud rate.
    ///
    /// Every variant, not one of them. embassy-rp spells all three enums
    /// differently from rp-hal — `ParityEven` against `Even`, `STOP2` against
    /// `Two`, `DataBits8` against `Eight` — so a name copied across from the
    /// blocking template does not compile, and checking a single arm would let
    /// the other two be copied.
    #[test]
    fn the_async_main_carries_the_frame() {
        for (parity, want) in [
            (Parity::None, "ParityNone"),
            (Parity::Even, "ParityEven"),
            (Parity::Odd, "ParityOdd"),
        ] {
            let src = pico(Runtime::Async, |c| {
                c.mode = UsartMode::Buffered;
                c.parity = parity;
            })
            .fresh_main_rs();
            assert!(
                src.contains(&format!("ucfg0.parity = embassy_rp::uart::Parity::{want};")),
                "{parity:?} is emitted as {want}:\n{src}"
            );
        }
        for (stop, want) in [(StopBits::One, "STOP1"), (StopBits::Two, "STOP2")] {
            let src = pico(Runtime::Async, |c| {
                c.mode = UsartMode::Buffered;
                c.stop_bits = stop;
            })
            .fresh_main_rs();
            assert!(
                src.contains(&format!(
                    "ucfg0.stop_bits = embassy_rp::uart::StopBits::{want};"
                )),
                "{stop:?} is emitted as {want}:\n{src}"
            );
        }
        let src = pico(Runtime::Async, |c| c.mode = UsartMode::Buffered).fresh_main_rs();
        assert!(
            src.contains("ucfg0.data_bits = embassy_rp::uart::DataBits::DataBits8;"),
            "and the word length:\n{src}"
        );
    }

    /// Nine bits is an STM32 word length; the PL011 has none. A project
    /// re-targeted at a Pico emits the widest the chip has rather than a name
    /// that does not exist in either HAL.
    #[test]
    fn a_nine_bit_word_becomes_the_widest_the_chip_has() {
        let blocking = uart0(&pico(Runtime::Blocking, |c| c.data_bits = 9));
        assert!(
            blocking.contains("DataBits::Eight"),
            "no `DataBits::Nine` in rp-hal:\n{blocking}"
        );
        let asynchronous = pico(Runtime::Async, |c| {
            c.mode = UsartMode::Buffered;
            c.data_bits = 9;
        })
        .fresh_main_rs();
        assert!(
            asynchronous.contains("DataBits::DataBits8"),
            "nor in embassy-rp:\n{asynchronous}"
        );
    }
}

#[cfg(test)]
mod buffered_uart_transport {
    use super::{async_bus_lines, needs_async_usart};
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::modules::{ModuleConfig, UsartMode};
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    /// A Pico with UART0 on GP0/GP1 and SPI0, through the module reconciler so
    /// the transport field actually exists.
    fn pico(mode: UsartMode) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = Runtime::Async;
        for p in mcu.iter_all_pins_mut() {
            match p.name.as_str() {
                "GP0" => p.selected_function = PinFunction::UsartTx(0),
                "GP1" => p.selected_function = PinFunction::UsartRx(0),
                "GP18" => p.selected_function = PinFunction::SpiSck(0),
                "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                _ => {}
            }
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Usart(c) = &mut m.config {
                c.mode = mode;
            }
        }
        mcu
    }

    /// The buffered transport takes NO DMA channel, and the card agrees.
    ///
    /// Both halves matter: the emitted code must not name a channel, and the
    /// Configuration tab is fed from the same counter, so a `take()` left
    /// behind would have the card listing channels nothing claims.
    #[test]
    fn a_buffered_uart_claims_no_channel_and_shifts_the_rest_down() {
        let (binding, body, _, uses) = async_bus_lines(&pico(UsartMode::Buffered));
        assert!(body.contains("BufferedUart::new("), "{body}");
        // Qualified: "Uart::new(" is a substring of "BufferedUart::new(".
        assert!(!body.contains("uart::Uart::new("), "{body}");
        // Two left, both the SPI's - the UART's pair is gone.
        assert_eq!(uses.len(), 2, "only the SPI takes channels: {uses:?}");
        assert!(uses.iter().all(|u| u.user.starts_with("SPI")), "{uses:?}");
        // And the SPI moved down into the channels the UART used to hold.
        assert!(
            body.contains("p.DMA_CH0,") && body.contains("p.DMA_CH1,"),
            "{body}"
        );
        // The handler on UART0_IRQ is the BUFFERED one. Binding the DMA handler
        // here compiles and is simply dead: the two disambiguate at run time on
        // the DMA-enable bit.
        assert!(binding.contains("BufferedInterruptHandler"), "{binding}");
        assert!(
            !binding.contains("uart::InterruptHandler"),
            "not both on one vector: {binding}"
        );
    }

    /// The DMA transport is untouched by any of this.
    #[test]
    fn the_dma_transport_still_takes_its_pair() {
        let (binding, body, _, uses) = async_bus_lines(&pico(UsartMode::Dma));
        assert!(body.contains("embassy_rp::uart::Uart::new("), "{body}");
        assert!(!body.contains("BufferedUart"), "{body}");
        assert_eq!(uses.len(), 4, "UART two and SPI two: {uses:?}");
        assert!(binding.contains("uart::InterruptHandler"), "{binding}");
        assert!(!binding.contains("BufferedInterruptHandler"), "{binding}");
    }

    /// The binding NAME does not move with the transport.
    ///
    /// It is the one thing user code below `GEN_END` refers to, and the whole
    /// block above is rewritten on every Save.
    #[test]
    fn the_handle_is_called_uart0_either_way() {
        for mode in [UsartMode::Buffered, UsartMode::Dma] {
            let code = pico(mode).fresh_main_rs();
            assert!(code.contains("let uart0 ="), "{mode:?}: {code}");
            assert!(code.contains("let _ = &uart0;"), "{mode:?}: {code}");
        }
    }

    /// The buffers come from the module's own field, not from a literal.
    #[test]
    fn the_ring_size_is_the_one_the_panel_shows() {
        let mut mcu = pico(UsartMode::Buffered);
        for m in &mut mcu.modules {
            if let ModuleConfig::Usart(c) = &mut m.config {
                c.buf_len = 1288;
            }
        }
        let body = async_bus_lines(&mcu).1;
        assert!(body.contains("StaticCell<[u8; 1288]>"), "{body}");
        assert!(body.contains(".init([0; 1288])"), "{body}");
    }

    /// The manifest gate. `has_cfg("usart")` can never fire on an RP async
    /// project - the backend writes no bus config files - so without this the
    /// emitted `static_cell::StaticCell` would have no crate behind it.
    #[test]
    fn the_dependency_gate_follows_the_transport() {
        assert!(needs_async_usart(&pico(UsartMode::Buffered)));
        assert!(!needs_async_usart(&pico(UsartMode::Dma)));
        // Blocking never emits either one.
        let mut blocking = pico(UsartMode::Buffered);
        blocking.runtime = Runtime::Blocking;
        assert!(!needs_async_usart(&blocking));
    }
}

#[cfg(test)]
mod async_pwm_keeps_both_channels {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::{builtins, pins::PinFunction};

    fn pico_with(pads: &[(&str, u8, u8)]) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = Runtime::Async;
        for p in mcu.iter_all_pins_mut() {
            if let Some((_, timer, channel)) = pads.iter().find(|(n, _, _)| *n == p.name) {
                p.selected_function = PinFunction::TimerPwm {
                    timer: *timer,
                    channel: *channel,
                };
            }
        }
        mcu
    }

    /// Both halves of a slice reach main.rs.
    ///
    /// The emitter used to keep a `done` list of slices and `continue` past
    /// every pad after the first, so wiring GP2 (slice 1, channel A) and GP3
    /// (slice 1, channel B) produced code for GP2 alone - the second pad
    /// configured on the canvas and missing from the project, with nothing
    /// saying so.
    #[test]
    fn a_slice_wired_on_both_channels_emits_new_output_ab() {
        let code = pico_with(&[("GP2", 1, 1), ("GP3", 1, 2)]).fresh_main_rs();
        assert!(code.contains("new_output_ab("), "{code}");
        assert!(code.contains("p.PIN_2,"), "channel A pad is there: {code}");
        assert!(code.contains("p.PIN_3,"), "channel B pad is there: {code}");
        assert!(code.contains("cfg1.compare_a ="), "{code}");
        assert!(code.contains("cfg1.compare_b ="), "{code}");
    }

    /// One channel still uses the one-sided constructor - `new_output_ab` needs
    /// both pads and would not compile with one.
    #[test]
    fn a_single_channel_still_uses_the_one_sided_constructor() {
        let a = pico_with(&[("GP2", 1, 1)]).fresh_main_rs();
        assert!(a.contains("new_output_a("), "{a}");
        assert!(!a.contains("new_output_ab("), "{a}");
        let b = pico_with(&[("GP3", 1, 2)]).fresh_main_rs();
        assert!(b.contains("new_output_b("), "{b}");
        assert!(!b.contains("new_output_ab("), "{b}");
    }

    /// %TEMP%\eide_rp2040_pwm_ab — the only proof `new_output_ab` is real.
    ///
    /// Its signature is the whole point of this change: `new_output_ab(slice, a,
    /// b, config)` where `a: impl ChannelAPin<T>` and `b: impl ChannelBPin<T>`,
    /// so passing the pads in the wrong order, or passing two pads of one
    /// channel, does not compile. Asserting on the emitted TEXT cannot see any
    /// of that.
    ///
    /// Through `async_flavor_for`, the same chooser the app uses - naming the
    /// flavour by hand here is what once made a harness green on a project the
    /// application could not build.
    #[test]
    #[ignore]
    fn emit_rp_pwm_ab_project() {
        use crate::panels::mcu_module::project_gen;
        let def = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico");
        let mcu = pico_with(&[("GP2", 1, 1), ("GP3", 1, 2), ("GP6", 3, 1)]);
        let main_rs = mcu.fresh_main_rs();
        // Through `build_cfg`, the SAME pairing the app uses. Calling
        // `for_async` by hand here is what kept every emitted-project test
        // green while the application shipped a manifest with no embassy in it.
        let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
        let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_rp2040_pwm_ab");
        let _ = std::fs::remove_dir_all(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write rp pwm project");
        let toml_path = dir.join("Cargo.toml");
        let toml = std::fs::read_to_string(&toml_path).expect("read Cargo.toml");
        let toml = project_gen::ensure_async_deps(
            &toml,
            true,
            project_gen::async_flavor_for(&mcu.family, ""),
            super::needs_async_usart(&mcu),
            false,
            false,
            &[],
        );
        let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &[]);
        std::fs::write(&toml_path, toml).expect("write Cargo.toml");
        println!("wrote {}", dir.display());
    }

    /// Two pads asking for ONE channel is a real canvas state on this chip -
    /// GP2 and GP18 are both slice 1 channel A. One of them cannot be driven,
    /// and the generated file says which rather than dropping it quietly.
    #[test]
    fn two_pads_on_one_channel_name_the_one_that_lost() {
        let code = pico_with(&[("GP2", 1, 1), ("GP18", 1, 1)]).fresh_main_rs();
        assert!(
            code.contains("GP18 also asks for slice 1 channel A"),
            "{code}"
        );
        assert!(code.contains("p.PIN_2,"), "{code}");
        assert!(!code.contains("p.PIN_18,"), "{code}");
    }
}

#[cfg(test)]
mod emit_async_for_manual_compile {
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    use crate::panels::mcu_module::{builtins, pins::PinFunction, project_gen};

    /// The Pico W's LED, which is not on the chip at all.
    ///
    /// %TEMP%\eide_rp2040w_radio_check + eide_rp2350w_radio_check
    ///
    /// The firmware ships with the IDE (Infineon's Permissive Binary License
    /// allows it), so `write_project` lays it down and this compiles with the
    /// same bytes a real board would run. Nothing here proves the RADIO comes
    /// up - that needs hardware.
    #[test]
    #[ignore]
    fn emit_rp_radio_project() {
        for (id, dir_name) in [
            ("rp2040_pico_w", "eide_rp2040w_radio_check"),
            ("rp2350_pico2_w", "eide_rp2350w_radio_check"),
        ] {
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"));
            let mut mcu = def.build_mcu();
            mcu.runtime = Runtime::Async;
            for p in mcu.iter_all_pins_mut() {
                match p.name.as_str() {
                    "WL_LED" => p.selected_function = PinFunction::GpioOutput,
                    // One ordinary bus too, so the radio's DMA channel is proved
                    // to come AFTER the ones the buses took, not on top of them.
                    "GP0" => p.selected_function = PinFunction::UsartTx(0),
                    "GP1" => p.selected_function = PinFunction::UsartRx(0),
                    _ => {}
                }
            }
            // ...and that bus has to be on the transport that actually TAKES
            // channels. `UsartMode` defaults to Buffered, which takes none - the
            // radio would then get CH0 and this case would be checking an
            // ordering with nothing in front of it.
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let crate::panels::mcu_module::modules::ModuleConfig::Usart(c) = &mut m.config {
                    c.mode = crate::panels::mcu_module::modules::UsartMode::Dma;
                }
            }
            let main_rs = mcu.fresh_main_rs();
            assert!(main_rs.contains("cyw43::new("), "the radio is brought up");
            assert!(
                main_rs.contains("p.DMA_CH2,"),
                "the radio takes the channel after the UART's two:
{main_rs}"
            );
            // Through `build_cfg`, the SAME pairing the app uses. Calling
            // `for_async` by hand here is what kept every emitted-project test
            // green while the application shipped a manifest with no embassy in it.
            let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
            let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            let _ = std::fs::remove_dir_all(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write rp radio project");
            let toml_path = dir.join("Cargo.toml");
            let toml = std::fs::read_to_string(&toml_path).expect("read Cargo.toml");
            let toml = project_gen::ensure_async_deps(
                &toml,
                true,
                // Through the SAME chooser the app uses. Naming the flavour
                // here is what let the app and this harness disagree.
                project_gen::async_flavor_for(&mcu.family, ""),
                // This project wires GP0/GP1 as a UART too, so it needs the
                // same answer - and the radio would MASK a wrong one, because
                // `ensure_cyw43_deps` below adds `static_cell` for its own
                // reasons. That is exactly why the plain Pico is the board to
                // test a BufferedUart on.
                super::needs_async_usart(&mcu),
                false,
                false,
                &[],
            );
            let toml = project_gen::ensure_cyw43_deps(&toml, true, &[]);
            // `static_cell` holds the driver state, and on the Pico's M0 that
            // needs a CAS the core does not have. The app adds this right after
            // the same two calls; without it only the RP2350 half builds.
            let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &[]);
            std::fs::write(&toml_path, toml).expect("write Cargo.toml");

            // `write_project` put the real blobs in `firmware/` already —
            // this only proves it, because a silent miss here would look like
            // a codegen failure two hundred lines away.
            // SIZES, not existence. `write_cyw43_firmware` deliberately never
            // overwrites, so a stub left by an older run survives — and
            // `include_bytes!` resolves just as happily on 27 bytes as on
            // 231 KB, which would take the whole case green on junk firmware.
            for (name, want) in [
                ("43439A0.bin", 231_077),
                ("43439A0_clm.bin", 984),
                ("nvram_rp2040.bin", 742),
                ("LICENSE-permissive-binary-license-1.0.txt", 2_419),
            ] {
                let path = dir.join("firmware").join(name);
                let got = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                assert_eq!(got, want, "{name}: shipped whole, not stubbed");
            }
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        }
    }

    /// A Pico project on the ASYNC runtime — a different HAL from the blocking
    /// one, so nothing about it is believable until a compiler has seen it.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_rp_async_project -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_rp_async_project() {
        // BOTH transports, on BOTH boards. The buffered arm is the one that
        // names `static_cell::StaticCell` in main.rs, and the plain Pico is the
        // only board where a missing manifest line shows: on a W the radio adds
        // that crate for its own reasons and hides the mistake.
        for (id, dir_name, mode) in [
            (
                "rp2040_pico",
                "eide_rp2040_async_check",
                crate::panels::mcu_module::modules::UsartMode::Buffered,
            ),
            (
                "rp2350_pico2",
                "eide_rp2350_async_check",
                crate::panels::mcu_module::modules::UsartMode::Buffered,
            ),
            (
                "rp2040_pico",
                "eide_rp2040_async_dma_check",
                crate::panels::mcu_module::modules::UsartMode::Dma,
            ),
        ] {
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"));
            let mut mcu = def.build_mcu();
            mcu.runtime = Runtime::Async;
            for p in mcu.iter_all_pins_mut() {
                match p.name.as_str() {
                    n if n.starts_with("GP25") => p.selected_function = PinFunction::GpioOutput,
                    "GP0" => p.selected_function = PinFunction::UsartTx(0),
                    "GP1" => p.selected_function = PinFunction::UsartRx(0),
                    "GP4" => p.selected_function = PinFunction::I2cSda(0),
                    "GP5" => p.selected_function = PinFunction::I2cScl(0),
                    "GP18" => p.selected_function = PinFunction::SpiSck(0),
                    "GP19" => p.selected_function = PinFunction::SpiMosi(0),
                    "GP16" => p.selected_function = PinFunction::SpiMiso(0),
                    "GP6" => {
                        p.selected_function = PinFunction::TimerPwm {
                            timer: 3,
                            channel: 1,
                        }
                    }
                    "GP26" => p.selected_function = PinFunction::AdcChannel { adc: 0, channel: 0 },
                    // An ARMED input: the edge has to become a task that owns
                    // the pin, and only the compiler can say whether
                    // embassy-rp agrees with the shape we emit for it.
                    "GP15" => {
                        p.selected_function = PinFunction::GpioInput;
                        p.irq = Some(Edge::Rising);
                    }
                    // ...and a PLAIN input beside it, so the two paths are
                    // proved to differ rather than both collapsing into one.
                    "GP14" => p.selected_function = PinFunction::GpioInput,
                    _ => {}
                }
            }
            // Through the module reconciler, the same door the canvas uses -
            // poking `selected_function` alone leaves `mcu.modules` empty, and
            // then the transport is whatever the fallback happens to be rather
            // than what a user would have.
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let crate::panels::mcu_module::modules::ModuleConfig::Usart(c) = &mut m.config {
                    c.mode = mode;
                }
            }
            // The watchdog at embassy-rp's own ceiling for this chip - twice
            // the Blocking one on the RP2350 - so its `const` assert is
            // compiled at the boundary.
            let (_, max) = crate::panels::mcu_module::watchdog::rp_range_us(&mcu.family, true);
            mcu.watchdog.rp =
                Some(crate::panels::mcu_module::watchdog::RpWdtConfig { timeout_us: max });
            // Two devices on I2C0: this runtime has no config files, so their
            // addresses are the `I2C0_<NAME>_DEVICE_ADDRESS` consts in main.rs.
            assert!(mcu.with_i2c_devices(&[("oled", 0x3C), ("", 0x68)]));
            let main_rs = mcu.fresh_main_rs();
            assert!(
                main_rs.contains("pins::configs::watchdog::init(p.WATCHDOG);"),
                "{id}: no watchdog in main.rs:\n{main_rs}"
            );
            // The chip names a DIFFERENT HAL crate on async; this is where that
            // choice becomes a Cargo.toml.
            // Through `build_cfg`, the SAME pairing the app uses. Calling
            // `for_async` by hand here is what kept every emitted-project test
            // green while the application shipped a manifest with no embassy in it.
            let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
            let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
            // What the app writes: every config file `config_files` returns.
            // This used to be an empty `configs/mod.rs` - true while this
            // backend had no config files, and a project shape the app stopped
            // producing the day the watchdog became one.
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            let _ = std::fs::remove_dir_all(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write rp async project");
            // The async deps the runtime needs, added the way the app adds them.
            let toml_path = dir.join("Cargo.toml");
            let toml = std::fs::read_to_string(&toml_path).expect("read Cargo.toml");
            let toml = project_gen::ensure_async_deps(
                &toml,
                true,
                // Through the SAME chooser the app uses. Naming the flavour
                // here is what let the app and this harness disagree.
                project_gen::async_flavor_for(&mcu.family, ""),
                // And the same computation for `static_cell` + embedded-io-async.
                // Hard-coded `false` here would keep the matrix green on a
                // project whose main.rs names `StaticCell` and whose manifest
                // does not carry it.
                super::needs_async_usart(&mcu),
                false,
                false,
                &[],
            );
            // `static_cell` on the RP2040's M0 needs a CAS the core has not
            // got. The app runs this for every async project; the radio
            // harness already did, and this one did not.
            let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &[]);
            std::fs::write(&toml_path, toml).expect("write Cargo.toml");
            // The other half of the firmware gate: a board with no radio
            // must not carry 231 KB of it. The gate reads the GENERATED
            // CODE, so it is exactly the kind of thing that goes wrong
            // quietly when the emitter changes.
            assert!(
                !dir.join("firmware").exists(),
                "no radio wired, no firmware shipped"
            );
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        }
    }

    /// The pico2-ice on BOTH runtimes: the first RP2350B board, so the first
    /// project that names GPIO 30..47 and the first on embassy-rp's `rp235xb`
    /// feature, where the wrong one is `no field PIN_36 on Peripherals`.
    ///
    /// Every bus sits on a pad past GP29 where it can (UART1 on GP36/37, SPI0
    /// on GP32/34/35, PWM slice 11 on GP38), because those rows of the FUNCSEL
    /// table are the ones no other board exercises.
    ///
    /// %TEMP%\eide_pico2ice_blocking_check + eide_pico2ice_async_check
    #[test]
    #[ignore = "writes projects to disk for a manual cross-compile"]
    fn emit_pico2_ice_project() {
        for (runtime, dir_name) in [
            (Runtime::Blocking, "eide_pico2ice_blocking_check"),
            (Runtime::Async, "eide_pico2ice_async_check"),
        ] {
            let def = builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == "rp2350_pico2_ice")
                .expect("built-in rp2350_pico2_ice");
            let mut mcu = def.build_mcu();
            mcu.runtime = runtime;
            for p in mcu.iter_all_pins_mut() {
                let f = match super::gpio_index(&p.name) {
                    Some(36) => PinFunction::UsartTx(1),
                    Some(37) => PinFunction::UsartRx(1),
                    Some(32) => PinFunction::SpiMiso(0),
                    Some(34) => PinFunction::SpiSck(0),
                    Some(35) => PinFunction::SpiMosi(0),
                    Some(2) => PinFunction::I2cSda(1),
                    Some(3) => PinFunction::I2cScl(1),
                    Some(38) => PinFunction::TimerPwm {
                        timer: 11,
                        channel: 1,
                    },
                    Some(20) => PinFunction::TimerPwm {
                        timer: 2,
                        channel: 1,
                    },
                    // A header GPIO shared with the FPGA, the RP's red LED
                    // (output is all that pad offers) and a plain input.
                    Some(30) | Some(1) => PinFunction::GpioOutput,
                    Some(41) => PinFunction::GpioInput,
                    // CRESET switches the FPGA loader on.
                    None if p.name.starts_with("ICE_CRESET") => PinFunction::GpioOutput,
                    _ => continue,
                };
                assert!(
                    p.available_functions.contains(&f),
                    "{} does not offer {f:?}",
                    p.name
                );
                p.selected_function = f;
            }
            mcu.reconcile_modules();
            for m in &mut mcu.modules {
                if let crate::panels::mcu_module::modules::ModuleConfig::Timer(c) = &mut m.config {
                    c.freq_hz = 20_000;
                    c.set_duty_x100(1, 750);
                }
            }
            let main_rs = mcu.fresh_main_rs();
            let (uart, slice) = match runtime {
                Runtime::Async => ("p.PIN_36", "PWM_SLICE11"),
                _ => ("gpio36", "pwm11"),
            };
            assert!(
                main_rs.contains(uart),
                "{runtime:?}: UART1 on GP36:\n{main_rs}"
            );
            assert!(
                main_rs.contains(slice),
                "{runtime:?}: PWM slice 11:\n{main_rs}"
            );
            assert!(super::fpga_loader(&mcu), "the loader is switched on");

            // Through `build_cfg`, the SAME pairing the app uses.
            let project = crate::panels::mcu_module::mcu_def::build_cfg(&def, Some(&mcu));
            let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
            let user: Vec<(String, String)> = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            let _ = std::fs::remove_dir_all(&dir);
            project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
                .expect("write pico2-ice project");
            if matches!(runtime, Runtime::Async) {
                let toml_path = dir.join("Cargo.toml");
                let toml = std::fs::read_to_string(&toml_path).expect("read Cargo.toml");
                let toml = project_gen::ensure_async_deps(
                    &toml,
                    true,
                    project_gen::async_flavor_for(&mcu.family, ""),
                    super::needs_async_usart(&mcu),
                    false,
                    false,
                    &[],
                );
                let toml = project_gen::ensure_m0_atomics(&toml, true, &project.target, &[]);
                std::fs::write(&toml_path, toml).expect("write Cargo.toml");
            }
            // The gateware `include_bytes!` reaches for: the IDE's own, whole,
            // and an image the FPGA takes. Existence alone proves nothing - a
            // stub compiles just as happily.
            let bin = std::fs::read(dir.join("fpga").join("top.bin")).expect("fpga/top.bin");
            assert_eq!(bin.len(), 104_090, "fpga/top.bin shipped whole");
            let info = crate::panels::mcu_module::fpga_bitstream::inspect(&bin)
                .expect("fpga/top.bin is a UP5K image");
            assert!(info.crc_checked, "its CRC was checked");
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        }
    }
}

#[cfg(test)]
mod async_tail_rp {
    use super::super::common::ASYNC_USER_TAIL;
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;

    fn pico(runtime: Runtime) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.runtime = runtime;
        mcu
    }

    /// embassy-rp runs the same cooperative executor, so a Pico on Async gets
    /// the same warning as an STM32 on Async.
    #[test]
    fn an_async_pico_opens_its_loop_with_the_warning() {
        let code = pico(Runtime::Async).fresh_main_rs();
        assert!(code.contains("Every iteration must `.await`"), "{code}");
        assert!(code.ends_with(ASYNC_USER_TAIL), "{code}");
    }

    /// A Blocking Pico has no executor to starve — no warning.
    #[test]
    fn a_blocking_pico_has_no_warning() {
        let code = pico(Runtime::Blocking).fresh_main_rs();
        assert!(!code.contains("IMPORTANT"), "{code}");
        assert!(code.contains("// Your main loop code here."), "{code}");
    }
}

/// The watchdog on both RP runtimes: the file, the call, and - on Blocking -
/// the tick it counts, without which neither means anything.
#[cfg(test)]
mod watchdog_rp {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::model::Runtime;
    use crate::panels::mcu_module::watchdog::RpWdtConfig;

    fn board(id: &str, runtime: Runtime, wdg: bool) -> super::Mcu {
        let mut mcu = builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu();
        mcu.runtime = runtime;
        if wdg {
            mcu.watchdog.rp = Some(RpWdtConfig {
                timeout_us: 500_000,
            });
        }
        mcu
    }

    /// Started with or without a watchdog: the RP2040's timer counts the same
    /// tick, and `init_clocks_and_plls` - the HAL's own version of this
    /// bring-up - always starts it. Each HAL takes the count at its own width.
    #[test]
    fn blocking_starts_the_tick_at_each_hals_width() {
        let rp2040 = board("rp2040_pico", Runtime::Blocking, false).fresh_main_rs();
        assert!(
            rp2040.contains("watchdog.enable_tick_generation((XTAL_FREQ_HZ / 1_000_000) as u8);"),
            "{rp2040}"
        );
        assert!(!rp2040.contains("let _ = &mut watchdog;"), "{rp2040}");
        let rp2350 = board("rp2350_pico2", Runtime::Blocking, false).fresh_main_rs();
        assert!(
            rp2350.contains("watchdog.enable_tick_generation((XTAL_FREQ_HZ / 1_000_000) as u16);"),
            "{rp2350}"
        );
    }

    #[test]
    fn nothing_is_generated_until_the_watchdog_is_switched_on() {
        for rt in [Runtime::Blocking, Runtime::Async] {
            let mcu = board("rp2040_pico", rt, false);
            assert!(
                !mcu.fresh_main_rs().contains("pins::configs::watchdog"),
                "{rt:?}"
            );
            assert!(
                mcu.config_files().iter().all(|(n, _)| n != "watchdog.rs"),
                "{rt:?}"
            );
        }
    }

    /// Blocking configures the binding main.rs already owns for the clocks,
    /// and only after the tick it counts has been started.
    #[test]
    fn blocking_configures_the_watchdog_main_already_owns() {
        let mcu = board("rp2350_pico2", Runtime::Blocking, true);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("pins::configs::watchdog::init(&mut watchdog);"),
            "{main}"
        );
        let tick = main.find("enable_tick_generation").expect("tick");
        let init = main.find("pins::configs::watchdog::init").expect("init");
        assert!(tick < init, "configured before its tick runs:\n{main}");

        let files = mcu.config_files();
        let (_, body) = files
            .iter()
            .find(|(n, _)| n == "watchdog.rs")
            .expect("watchdog.rs");
        assert!(body.contains("const TIMEOUT_US: u32 = 500000;"), "{body}");
        assert!(body.contains("use rp235x_hal::Watchdog;"), "{body}");
        // rp235x-hal's ceiling - the RP2040's, kept - not the counter's.
        assert!(body.contains("TIMEOUT_US <= 8_388_607"), "{body}");
        assert!(body.contains("rp235x-hal start() panics"), "{body}");
        assert!(body.contains("pause_on_debug(true)"), "{body}");
    }

    /// Async takes the peripheral itself, and on an RP2350 gets embassy-rp's
    /// whole-counter ceiling - twice what the same board has on Blocking.
    #[test]
    fn async_takes_the_peripheral_and_gets_embassys_ceiling() {
        let mcu = board("rp2350_pico2", Runtime::Async, true);
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("let mut watchdog = pins::configs::watchdog::init(p.WATCHDOG);"),
            "{main}"
        );
        let files = mcu.config_files();
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["watchdog.rs"]);
        let body = &files[0].1;
        assert!(body.contains("TIMEOUT_US <= 16_777_215"), "{body}");
        assert!(
            body.contains("pub const PERIOD: Duration = Duration::from_micros(TIMEOUT_US);"),
            "{body}"
        );

        let rp2040 = board("rp2040_pico", Runtime::Async, true).config_files();
        assert!(
            rp2040[0].1.contains("TIMEOUT_US <= 8_388_607"),
            "{}",
            rp2040[0].1
        );
    }
}
