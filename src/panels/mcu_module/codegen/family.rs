//! Per-family code-generation backends.
//!
//! A [`FamilyBackend`] captures the *family-specific* part of code generation —
//! the HAL idioms and `main.rs` skeleton that differ between chip families
//! (STM32F1 vs ESP32-C3 vs future RP2040/nRF…). Dispatch is keyed on
//! [`Mcu::family`](crate::panels::mcu_module::mcu::Mcu), which is finer-grained
//! than [`ToolchainKind`](crate::panels::mcu_module::mcu_catalog::ToolchainKind)
//! — all ARM HALs share `RustEmbedded` yet generate different code.
//!
//! **Adding a new family** = implement this trait in a new `struct` and add it
//! to [`BACKENDS`]. New *chips* inside an already-supported family need no code
//! here — they are pure data (a `.ron` definition).

use super::common::{EdgeHook, USER_TAIL};
use super::{embassy_async, embassy_common, rcc, rtic, stm32, wba};
use crate::panels::mcu_module::codegen_esp::{self, EspRuntime};
use crate::panels::mcu_module::comparator;
use crate::panels::mcu_module::mcu::{Mcu, Runtime};
use crate::panels::mcu_module::modules::{
    self, ApiStyle, I2cModuleConfig, LcdCamMode, LcdCamModuleConfig, SpiModuleConfig, SpiRole,
    UsartModuleConfig, UsbModuleConfig, UsbRole,
};
use crate::panels::mcu_module::pins::logic::pin::{GpioMode, Pin};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use std::collections::BTreeMap;

/// The USART/SPI/I2C configs for `mcu`'s blocking codegen, with `api_style`
/// forced to Native when the project Runtime is Native (an "all concrete HAL"
/// project). On Blocking each module keeps its own Portable/Native choice.
fn resolve_bus_configs(
    mcu: &Mcu,
) -> (
    BTreeMap<u8, UsartModuleConfig>,
    BTreeMap<u8, SpiModuleConfig>,
    BTreeMap<u8, I2cModuleConfig>,
) {
    let mut usart = modules::usart_configs(&mcu.modules);
    let mut spi = modules::spi_configs(&mcu.modules);
    let mut i2c = modules::i2c_configs(&mcu.modules);
    // The ESP ignores `api_style` outright — `codegen_esp_configs` always hands
    // back the esp-hal driver — so the panel shows the row locked there. Pinning
    // the value too keeps a saved project from CLAIMING a style it never had.
    if mcu.is_native() || is_esp(&mcu.family) {
        for c in usart.values_mut() {
            c.api_style = ApiStyle::Native;
        }
        for c in spi.values_mut() {
            c.api_style = ApiStyle::Native;
        }
        for c in i2c.values_mut() {
            c.api_style = ApiStyle::Native;
        }
    }
    (usart, spi, i2c)
}

/// Family-specific `main.rs` generation. One implementor per chip family.
pub trait FamilyBackend {
    /// Family key this backend handles (matches `Mcu::family`). For a backend
    /// that spans several families (see [`StmEmbassyBackend`]) this is only a
    /// label — [`handles`](FamilyBackend::handles) decides the actual match.
    fn family_id(&self) -> &'static str;

    /// Whether this backend generates code for `family`. Default: an exact
    /// [`family_id`](FamilyBackend::family_id) match; multi-family backends
    /// override it.
    fn handles(&self, family: &str) -> bool {
        family == self.family_id()
    }

    /// Build a brand-new `src/main.rs` from the MCU's pins + clock.
    fn fresh_main_rs(&self, mcu: &Mcu) -> String;

    /// Re-splice only the generated section of an existing `main.rs`,
    /// preserving user-editable code outside the markers.
    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String;

    /// Per-peripheral init module bodies for `src/pins/configs/` — `(file_name,
    /// generated_body)`. Default: none (families without separate config files).
    fn config_files(&self, _mcu: &Mcu) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The hooks the generated handlers call, one per armed input, so `Mcu`
    /// can seed each below the user tail (`common::ensure_edge_hooks`).
    /// Default: none, for a backend whose handler still carries its body
    /// inline between the markers - it reports nothing, and no hook it does
    /// not call is ever seeded.
    fn edge_hooks(&self, _mcu: &Mcu) -> Vec<EdgeHook> {
        Vec::new()
    }

    /// The drive / pull modes this backend can generate for `func` — what the
    /// mode list under the selected function offers. The FIRST entry is the
    /// default, i.e. what a pin with `io_mode: None` generates.
    ///
    /// The default implementation is the full set, which the `into_*` HALs
    /// (stm32f1xx-hal & friends) all provide. A backend whose HAL spells one of
    /// them differently — embassy's open-drain output is a different TYPE, not
    /// an argument — overrides this to offer only what it actually emits, so the
    /// list can never promise code the generator can't write.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull, GpioMode::OpenDrain],
            _ => &[],
        }
    }
}

/// The modes offered for `pin` on `mcu`'s current backend — empty when the pin
/// is not a GPIO In/Out, or the family has no backend. The single entry point
/// the UI uses, so it can't drift from what codegen supports.
pub fn gpio_modes_for(mcu: &Mcu, func: &PinFunction) -> &'static [GpioMode] {
    backend_for_runtime(&mcu.family, mcu.runtime).map_or(&[], |b| b.gpio_modes(func))
}

/// All four sides of the chip, in the canonical order codegen expects
/// (top, bottom, left, right — same as the old private `Mcu::all_pins`).
fn pins_of(mcu: &Mcu) -> Vec<&Pin> {
    mcu.iter_all_pins().collect()
}

// ── STM32F1 (stm32f1xx-hal) ─────────────────────────────────────────────────
struct Stm32f1Backend;

impl FamilyBackend for Stm32f1Backend {
    fn family_id(&self) -> &'static str {
        "stm32f1"
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        let all = pins_of(mcu);
        let (usart, spi, i2c) = resolve_bus_configs(mcu);
        let can = modules::can_configs(&mcu.modules);
        let usb = modules::usb_configs(&mcu.modules);
        let timer = modules::timer_configs(&mcu.modules);
        let gen_ = stm32::make_generated_section(
            &mcu.name,
            &all,
            &mcu.clock,
            &usart,
            &spi,
            &i2c,
            &can,
            &usb,
            &timer,
            mcu.gpio_native(),
            &mcu.watchdog_and_custom_inits(),
        );
        let base = format!(
            "{header}{gen_}\n{tail}",
            header = stm32::invariant_header(&mcu.name, &mcu.id),
            tail = USER_TAIL,
        );
        // Nothing lives after `fn main` any more: USART/SPI/I2C init are in
        // `src/pins/configs/` and the ADC is one line inside the GEN block.
        stm32::strip_obsolete_helpers(base)
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let all = pins_of(mcu);
        let (usart, spi, i2c) = resolve_bus_configs(mcu);
        let can = modules::can_configs(&mcu.modules);
        let usb = modules::usb_configs(&mcu.modules);
        let timer = modules::timer_configs(&mcu.modules);
        let new_section = stm32::make_generated_section(
            &mcu.name,
            &all,
            &mcu.clock,
            &usart,
            &spi,
            &i2c,
            &can,
            &usb,
            &timer,
            mcu.gpio_native(),
            &mcu.watchdog_and_custom_inits(),
        );
        let spliced = stm32::splice_section(existing, &new_section, &mcu.name, &mcu.id);
        // Clean up the init helpers older versions appended after `fn main`.
        stm32::strip_obsolete_helpers(spliced)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        let all = pins_of(mcu);
        let (usart, spi, i2c) = resolve_bus_configs(mcu);
        let can = modules::can_configs(&mcu.modules);
        let mut files = stm32::config_files(
            &all,
            &usart,
            &spi,
            &i2c,
            &can,
            &modules::timer_configs(&mcu.modules),
            &mcu.clock,
            mcu.gpio_native(),
        );
        // Pin-less, from the Configuration tab rather than the pins. The F1
        // gets only the IWDG - `stm32f1xx-hal` has no window watchdog - and
        // `watchdog_gen` enforces that itself.
        files.extend(super::watchdog_gen::config_files(
            &mcu.watchdog,
            &mcu.family,
            mcu.runtime,
        ));
        files
    }
}

// ── ESP32-C3 (esp-hal) ──────────────────────────────────────────────────────
struct Esp32Backend;

/// Whether `family` is one of Espressif's parts.
///
/// The two ESP backends are chip-agnostic: everything that differs between an
/// ESP32-C3 and an ESP32-C6 — the `esp-hal` feature, the target triple, the
/// `espflash --chip` name — travels in `ProjectDef`, not in the generator. So
/// they dispatch on the vendor rather than on one chip, and adding a part is a
/// definition file rather than a backend.
///
/// Only RISC-V parts ever reach here: the Xtensa ones are refused at generation
/// time, because they need Espressif's rustc fork — see
/// [`esp_gen::definition`](crate::panels::mcu_module::esp_gen::definition).
pub fn is_esp(family: &str) -> bool {
    family.starts_with("esp32")
}

impl FamilyBackend for Esp32Backend {
    /// A LABEL, not the dispatch key — see the `handles` below.
    fn family_id(&self) -> &'static str {
        "esp32c3"
    }

    fn handles(&self, family: &str) -> bool {
        is_esp(family)
    }

    /// The three input pulls esp-hal's `InputConfig` carries, and push-pull
    /// only for outputs.
    ///
    /// `OutputConfig` has THREE independent fields - `drive_mode`,
    /// `drive_strength` and `pull` - which a flat [`GpioMode`] cannot express,
    /// so open-drain is left out rather than half-answered. Same shape as
    /// [`WbaBackend`], for a different reason.
    ///
    /// Offered on every pad: nothing in the chip data says which pads have pull
    /// resistors, and esp-hal writes `fun_wpu`/`fun_wpd` unconditionally, so
    /// blocking any pad here would be asserting a datasheet fact this code does
    /// not have.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull],
            _ => &[],
        }
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        esp_fresh_main_rs(mcu, EspRuntime::Blocking)
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        esp_update_main_rs(mcu, existing, EspRuntime::Blocking)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        esp_config_files(mcu, EspRuntime::Blocking)
    }
}

/// The SPI modules, with any role or mode this chip cannot build put back.
///
/// The panel already refuses both — but a project moved from an ESP32-C6 to a
/// C5 carries its `.config` with it and never passes through the panel again.
/// Without this, that project would emit a slave file for a chip whose esp-hal
/// has no `with_dma`, and it would not compile.
fn esp_spi_configs(mcu: &Mcu) -> BTreeMap<u8, SpiModuleConfig> {
    let mut spi = modules::spi_configs(&mcu.modules);
    for cfg in spi.values_mut() {
        if !SpiRole::options(&mcu.family).contains(&cfg.role) {
            cfg.role = SpiRole::Master;
        }
        let modes = cfg.role.modes(&mcu.family);
        if !modes.contains(&cfg.mode) {
            cfg.mode = modes[0];
        }
    }
    spi
}

/// The video-port modules, with each half's mode forced to what that half is.
///
/// The halves used to be ONE module with a three-way mode, so a project saved
/// before the split can hold `mode: Camera` on the DISPLAY half — which would
/// build a `Camera` on `lcd_cam.lcd` and not compile. Keyed by instance: 0 is
/// the display, 1 is the camera. The same net as [`esp_spi_configs`].
fn esp_lcd_configs(mcu: &Mcu) -> BTreeMap<u8, LcdCamModuleConfig> {
    let mut v = modules::lcd_cam_configs(&mcu.modules);
    for (instance, cfg) in v.iter_mut() {
        if *instance == 1 {
            cfg.mode = LcdCamMode::Camera;
        } else if cfg.mode == LcdCamMode::Camera {
            cfg.mode = LcdCamMode::I8080;
        }
    }
    v
}

/// The USB module, with a controller this chip does not have put back.
///
/// The same hole as [`esp_spi_configs`], and it bites hardest on the ESP32-S2:
/// the role DEFAULTS to Serial/JTAG, which the S2 has no driver for, so a
/// project that never opened the panel would emit "pads only" for a chip whose
/// OTG controller is sitting right there.
fn esp_usb_configs(mcu: &Mcu) -> BTreeMap<u8, UsbModuleConfig> {
    let roles = UsbRole::options(&mcu.family);
    let mut usb = modules::usb_configs(&mcu.modules);
    for cfg in usb.values_mut() {
        if !roles.is_empty() && !roles.contains(&cfg.role) {
            cfg.role = roles[0];
        }
    }
    usb
}

/// A fresh ESP `main.rs` on `runtime` — shared by the blocking and async ESP
/// backends, which differ only in that argument.
fn esp_fresh_main_rs(mcu: &Mcu, runtime: EspRuntime) -> String {
    let usart = modules::usart_configs(&mcu.modules);
    let spi = esp_spi_configs(mcu);
    let i2c = modules::i2c_configs(&mcu.modules);
    let i2s = modules::i2s_configs(&mcu.modules);
    let rmt = modules::rmt_configs(&mcu.modules);
    let pcnt = modules::pcnt_configs(&mcu.modules);
    let mcpwm = modules::mcpwm_configs(&mcu.modules);
    let parl_io = modules::parl_io_configs(&mcu.modules);
    let lcd_cam = esp_lcd_configs(mcu);
    let usb = esp_usb_configs(mcu);
    let touch = modules::touch_configs(&mcu.modules);
    let dac = modules::dac_configs(&mcu.modules);
    let can = modules::can_configs(&mcu.modules);
    let timer = modules::timer_configs(&mcu.modules);
    codegen_esp::fresh_esp32c3_main_rs(
        &pins_of(mcu),
        &mcu.clock,
        &mcu.name,
        &mcu.id,
        &usart,
        &spi,
        &i2c,
        &i2s,
        &rmt,
        &pcnt,
        &mcpwm,
        &parl_io,
        &lcd_cam,
        &usb,
        &touch,
        &dac,
        &can,
        &timer,
        &mcu.custom_module_inits(),
        // NOT `watchdog_and_custom_inits()`, which the STM32 backends use: on
        // an ESP the two land in different places in `main`, so they travel as
        // two arguments.
        &esp_tab_inits(mcu),
        // On an ESP the family key IS the chip - `esp32h2`, not a series.
        &mcu.family,
        runtime,
        mcu.dma.as_ref(),
    )
}

/// The Configuration tab's `main.rs` lines on an ESP: the watchdogs, then the
/// flash store. One slot, because both belong right after the scheduler
/// start, ahead of every peripheral - a store read at boot may decide how the
/// rest is set up.
fn esp_tab_inits(mcu: &Mcu) -> String {
    format!(
        "{}{}",
        super::watchdog_gen::init_lines(&mcu.watchdog, &mcu.family, mcu.runtime),
        super::flash_store_gen::init_lines_for(mcu),
    )
}

/// The `src/pins/configs/*.rs` an ESP project needs — one per wired bus
/// instance. Shared by both ESP backends: the config modules are the same
/// blocking esp-hal drivers on either runtime (esp-rtos does not change how a
/// `Uart` is built), so Async gets exactly the same files.
fn esp_config_files(mcu: &Mcu, runtime: EspRuntime) -> Vec<(String, String)> {
    // The watchdogs come from a TAB, not from the Pins canvas, so they are
    // collected here rather than threaded through the pin-driven builder. The
    // flash store's file is added for every family in `Mcu::config_files`.
    let mut out = super::watchdog_gen::config_files(&mcu.watchdog, &mcu.family, mcu.runtime);
    let all = pins_of(mcu);
    let configured: Vec<&Pin> = all
        .iter()
        .copied()
        .filter(|p| !p.reserved && p.selected_function != PinFunction::Unset)
        .collect();
    let (uart, spi_n, i2c_n, i2s_n) = codegen_esp::bus_instances(&configured);
    // The same filter `main.rs` uses, so the call and the signature agree.
    let usart_cfgs = modules::usart_configs(&mcu.modules);
    let uart: Vec<(u8, Vec<&'static str>)> = uart
        .into_iter()
        .map(|(n, sigs)| (n, codegen_esp::uart_sigs(usart_cfgs.get(&n), &sigs)))
        .collect();
    // The config file needs the duty per channel, not the pin: the pins stay in
    // `main.rs` (they are the only record of the wiring) and arrive as `init`
    // arguments.
    let timers = modules::timer_configs(&mcu.modules);
    // `(channel, duty, pad)`. The PAD rides along because it is the one identity
    // that does NOT move when a channel is re-pointed — which is exactly what
    // the generated constant names key on when a timer drives several pads.
    let pwm: Vec<(u8, Vec<(u8, u16, String)>)> = codegen_esp::pwm_channels(&configured)
        .into_iter()
        .map(|(t, chans)| {
            let duties = chans
                .iter()
                .map(|(c, pin)| {
                    (
                        *c,
                        timers.get(&t).map_or(0, |cfg| cfg.duty_x100_of(*c)),
                        pin.name.clone(),
                    )
                })
                .collect();
            (t, duties)
        })
        .collect();
    let can_cfg = modules::can_configs(&mcu.modules);
    let usb_cfg = esp_usb_configs(mcu);
    let touch_cfg = modules::touch_configs(&mcu.modules);
    let lcd_cam_cfg = esp_lcd_configs(mcu);
    let parl_cfg = modules::parl_io_configs(&mcu.modules);
    out.extend(
        crate::panels::mcu_module::codegen_esp_configs::config_files(
            &uart,
            &spi_n,
            &i2c_n,
            &i2s_n,
            &modules::usart_configs(&mcu.modules),
            &esp_spi_configs(mcu),
            &modules::i2c_configs(&mcu.modules),
            &modules::i2s_configs(&mcu.modules),
            // The RMT channels the canvas wires, by number.
            &codegen_esp::rmt_channels_wired(&configured),
            &modules::rmt_configs(&mcu.modules),
            crate::panels::mcu_module::mcu::gui::modules::rmt_source_hz(&mcu.family),
            &codegen_esp::pcnt_units_wired(&configured),
            &modules::pcnt_configs(&mcu.modules),
            // The USB pads wired, and a chip whose esp-hal can drive them AS THE
            // ROLE ASKS: the two controllers share the pads and not the support, so
            // which one is wanted decides whether there is a file to write.
            configured
                .iter()
                .any(|p| matches!(p.selected_function, PinFunction::UsbDm | PinFunction::UsbDp))
                && if usb_cfg.get(&1).is_some_and(|c| c.role.is_otg()) {
                    codegen_esp::has_usb_otg(&mcu.family)
                } else {
                    codegen_esp::has_usb_serial_jtag(&mcu.family)
                },
            usb_cfg.get(&1),
            // The touch channels wired, on a chip whose esp-hal has the driver:
            // the S2 and S3 have the sensors and no `esp_hal::touch`.
            &if codegen_esp::has_touch(&mcu.family) {
                codegen_esp::touch_pads_wired(&configured)
            } else {
                Vec::new()
            },
            touch_cfg.get(&0),
            // BOTH TWAI pads, on a chip whose esp-hal has the driver. The pads are
            // offered only where it does, so this is belt and braces — but the C5
            // has TWAI silicon and no driver, which is the shape of trap it catches.
            codegen_esp::twai_wired(&configured) && codegen_esp::has_twai(&mcu.family),
            can_cfg.get(&1),
            // The MCPWM outputs wired, their unit's settings, and the peripheral
            // clock esp-hal's own examples pass - 32 MHz on the H2, 40 elsewhere.
            &codegen_esp::mcpwm_outputs_wired(&configured),
            &modules::mcpwm_configs(&mcu.modules),
            if mcu.family == "esp32h2" { 32 } else { 40 },
            // The parallel port, and whether a VALID pad went with it.
            &codegen_esp::parl_io_wired(&configured),
            parl_cfg.get(&0),
            // The receiving half, wired and configured on its own.
            &codegen_esp::parl_io_rx_wired(&configured),
            parl_cfg.get(&1),
            &codegen_esp::lcd_wired(&configured),
            lcd_cam_cfg.get(&0),
            // The camera half, wired and configured independently of the display.
            &codegen_esp::cam_wired(&configured),
            lcd_cam_cfg.get(&1),
            // The DAC channels wired, and the module that holds their levels.
            &codegen_esp::dac_channels_wired(&configured),
            modules::dac_configs(&mcu.modules).get(&1),
            mcu.family == "esp32",
            &pwm,
            &timers,
            // Read off the CHIP's own pin table rather than a per-part list:
            // the highest LEDC channel any pad offers is the highest the part
            // has, and that is what the generated mapping may name.
            all.iter()
                .filter(|p| !p.reserved)
                .flat_map(|p| p.available_functions.iter())
                .filter_map(|f| match f {
                    PinFunction::TimerPwm { channel, .. } => Some(*channel),
                    _ => None,
                })
                .max()
                .unwrap_or(0),
            runtime,
        ),
    );
    out
}

/// Re-splice an existing ESP `main.rs` on `runtime` (see [`esp_fresh_main_rs`]).
fn esp_update_main_rs(mcu: &Mcu, existing: &str, runtime: EspRuntime) -> String {
    let usart = modules::usart_configs(&mcu.modules);
    let spi = esp_spi_configs(mcu);
    let i2c = modules::i2c_configs(&mcu.modules);
    let i2s = modules::i2s_configs(&mcu.modules);
    let rmt = modules::rmt_configs(&mcu.modules);
    let pcnt = modules::pcnt_configs(&mcu.modules);
    let mcpwm = modules::mcpwm_configs(&mcu.modules);
    let parl_io = modules::parl_io_configs(&mcu.modules);
    let lcd_cam = esp_lcd_configs(mcu);
    let usb = esp_usb_configs(mcu);
    let touch = modules::touch_configs(&mcu.modules);
    let dac = modules::dac_configs(&mcu.modules);
    let can = modules::can_configs(&mcu.modules);
    let timer = modules::timer_configs(&mcu.modules);
    codegen_esp::update_esp32c3_main_rs(
        existing,
        &pins_of(mcu),
        &mcu.clock,
        &mcu.name,
        &mcu.id,
        &usart,
        &spi,
        &i2c,
        &i2s,
        &rmt,
        &pcnt,
        &mcpwm,
        &parl_io,
        &lcd_cam,
        &usb,
        &touch,
        &dac,
        &can,
        &timer,
        &mcu.custom_module_inits(),
        // NOT `watchdog_and_custom_inits()`, which the STM32 backends use: on
        // an ESP the two land in different places in `main`, so they travel as
        // two arguments.
        &esp_tab_inits(mcu),
        // On an ESP the family key IS the chip - `esp32h2`, not a series.
        &mcu.family,
        runtime,
        mcu.dma.as_ref(),
    )
}

// ── Async ESP32-C3 (esp-rtos + embassy-executor) ────────────────────────────
// The [`Runtime::Async`] counterpart of [`Esp32Backend`]: the SAME esp-hal
// bindings, entered through `#[esp_rtos::main] async fn main(Spawner)` with the
// esp-rtos scheduler driving the executor. Like [`AsyncEmbassyBackend`] it is
// selected by [`backend_for_runtime`], never listed in [`BACKENDS`].
//
// `esp-rtos`, not `esp-hal-embassy`: the latter needs esp-hal's private
// `__esp_hal_embassy` feature, which esp-hal 1.1 (the version the project
// template pins) dropped — cargo fails to resolve before compiling anything.
struct AsyncEspBackend;

impl FamilyBackend for AsyncEspBackend {
    fn family_id(&self) -> &'static str {
        "esp32c3-async" // label only — dispatch is via `backend_for_runtime`
    }

    /// The same three pulls as [`Esp32Backend`] - both runtimes emit the same
    /// `Input::new(.., InputConfig::default().with_pull(..))` from one builder.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull],
            _ => &[],
        }
    }

    fn handles(&self, family: &str) -> bool {
        is_esp(family)
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        esp_fresh_main_rs(mcu, EspRuntime::Async)
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        esp_update_main_rs(mcu, existing, EspRuntime::Async)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        esp_config_files(mcu, EspRuntime::Async)
    }
}

/// Chosen by runtime rather than family, so — like [`ASYNC_EMBASSY_BACKEND`] —
/// it lives outside [`BACKENDS`].
static ASYNC_ESP_BACKEND: AsyncEspBackend = AsyncEspBackend;

// ── STM32WBA (embassy-stm32, blocking) ──────────────────────────────────────
struct WbaBackend;

impl FamilyBackend for WbaBackend {
    fn family_id(&self) -> &'static str {
        "stm32wba"
    }

    /// embassy takes the pull as an argument (`Input::new(p.PB5, Pull::Up)`) but
    /// spells an open-drain output as a different TYPE (`OutputOpenDrain`), which
    /// this backend does not emit — so outputs offer push-pull only.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull],
            _ => &[],
        }
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        let all = pins_of(mcu);
        format!(
            "{header}{section}\n{tail}",
            header = wba::invariant_header(&mcu.name, &mcu.id),
            section = wba::make_generated_section(
                &mcu.name,
                &all,
                &mcu.clock,
                &mcu.watchdog_and_custom_inits(),
                mcu.clock_manual,
            ),
            tail = USER_TAIL,
        )
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let all = pins_of(mcu);
        let section = wba::make_generated_section(
            &mcu.name,
            &all,
            &mcu.clock,
            &mcu.watchdog_and_custom_inits(),
            mcu.clock_manual,
        );
        let section = super::common::keep_manual_clock(existing, section, mcu.clock_manual);
        wba::splice_section(existing, &section, &mcu.name, &mcu.id)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        // Like the generic embassy backend: bus inits stay inline in
        // main.rs, so `pins/configs/` here holds only the pin-less
        // peripherals the Configuration tab owns.
        super::watchdog_gen::config_files(&mcu.watchdog, &mcu.family, mcu.runtime)
    }
    // No per-peripheral config files yet — bus init is documented inline (v1).
}

// ── Generic STM32 (embassy-stm32, blocking) ─────────────────────────────────
// Covers every STM32 family embassy supports that has no dedicated backend
// (all but `stm32f1`, which uses stm32f1xx-hal, and `stm32wba`, which adds RCC
// clock mapping). The GPIO codegen is uniform across families — see
// `embassy_common`; the clock is left at embassy's reset default (per-family
// RCC graphs are a later step). This is what turns the many STM32 chips the
// pin-data XML importer adds from data-only into buildable projects.
struct StmEmbassyBackend;

impl FamilyBackend for StmEmbassyBackend {
    fn family_id(&self) -> &'static str {
        "stm32" // label only — `handles` does the real matching
    }

    /// embassy takes the pull as an argument (`Input::new(p.PB5, Pull::Up)`) but
    /// spells an open-drain output as a different TYPE (`OutputOpenDrain`), which
    /// this backend does not emit — so outputs offer push-pull only.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull],
            // A generic alternate function: the mode is how the user states the
            // DIRECTION, which is what turns the vendor's AF index into a real
            // `set_as_af_unchecked` line (see `embassy_common::pin_binding_line`).
            // Both directions are offered because an AF signal can be either —
            // `SAI1_SD_A` is an input in one configuration and an output in
            // another — and guessing would silently mis-wire the pad.
            PinFunction::Other(_) => &[
                GpioMode::Floating,
                GpioMode::PullUp,
                GpioMode::PullDown,
                GpioMode::PushPull,
                GpioMode::OpenDrain,
            ],
            _ => &[],
        }
    }

    fn handles(&self, family: &str) -> bool {
        // Every STM32 family except the two with their own backends.
        family.starts_with("stm32") && family != "stm32f1" && family != "stm32wba"
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        let all = pins_of(mcu);
        format!(
            "{header}{section}\n{tail}",
            header = embassy_common::invariant_header(&mcu.name, &mcu.id),
            // The RCC block is selected by FAMILY (data), not by sniffing the
            // graph's shape — `rcc::rcc_recipe` maps f4/wba (and any future
            // family) to its ReadSpec + descriptor; others get the reset default.
            section = embassy_common::make_generated_section(
                &mcu.name,
                &all,
                &rcc::graph_clock_block_for(&mcu.id, &mcu.family, &mcu.clock, mcu.clock_manual),
                &mcu.watchdog_and_custom_inits(),
            ),
            tail = USER_TAIL,
        )
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let all = pins_of(mcu);
        let section = embassy_common::make_generated_section(
            &mcu.name,
            &all,
            &rcc::graph_clock_block_for(&mcu.id, &mcu.family, &mcu.clock, mcu.clock_manual),
            &mcu.watchdog_and_custom_inits(),
        );
        let section = super::common::keep_manual_clock(existing, section, mcu.clock_manual);
        embassy_common::splice_section(existing, &section, &mcu.name, &mcu.id)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        // Only the watchdogs: this backend keeps every bus init inline in
        // main.rs, so `pins/configs/` exists here purely for the tab-driven,
        // pin-less peripherals.
        super::watchdog_gen::config_files(&mcu.watchdog, &mcu.family, mcu.runtime)
    }
}
// ── Async STM32 (embassy-stm32 + embassy-executor) ──────────────────────────
// The [`Runtime::Async`] counterpart of [`StmEmbassyBackend`]: the SAME
// embassy-stm32 GPIO codegen, but the entry point is `#[embassy_executor::main]
// async fn main(Spawner)` driven by the executor instead of `#[entry] fn main()
// -> !`. Selected by [`backend_for_runtime`] — never listed in [`BACKENDS`],
// since it is chosen by runtime, not family. Handles every STM32 family,
// `stm32f1` included: on Blocking, Native and RTIC the F1 is on stm32f1xx-hal,
// and on Async it swaps that crate for the embassy-stm32 the others use (its
// line is derived in `ProjectDef::for_async`), with its AFIO remap threaded
// through the pin bounds (`embassy_async::with_afio_remap`).
struct AsyncEmbassyBackend;

impl FamilyBackend for AsyncEmbassyBackend {
    fn family_id(&self) -> &'static str {
        "stm32-async" // label only — dispatch is via `backend_for_runtime`
    }

    /// embassy takes the pull as an argument (`Input::new(p.PB5, Pull::Up)`) but
    /// spells an open-drain output as a different TYPE (`OutputOpenDrain`), which
    /// this backend does not emit — so outputs offer push-pull only.
    fn gpio_modes(&self, func: &PinFunction) -> &'static [GpioMode] {
        match func {
            PinFunction::GpioInput => &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
            PinFunction::GpioOutput => &[GpioMode::PushPull],
            _ => &[],
        }
    }

    fn handles(&self, family: &str) -> bool {
        family.starts_with("stm32")
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        format!(
            "{header}{section}\n{tail}",
            header = embassy_async::invariant_header(&mcu.name, &mcu.id),
            section = async_section(mcu),
            tail = super::common::ASYNC_USER_TAIL,
        )
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let section = async_section(mcu);
        let section = super::common::keep_manual_clock(existing, section, mcu.clock_manual);
        embassy_async::splice_section(existing, &section, &mcu.name, &mcu.id)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        // Async bus peripherals: USART (BufferedUart → embedded-io-async), SPI +
        // I2C (blocking `embedded-hal` 1.0 or async-DMA `embedded-hal-async`, per
        // the module's AsyncBusMode). GPIO stays inline in main.rs (no io.rs).
        // Plus the watchdogs, whose `init` calls `async_section` writes.
        let mut files = async_periphs(mcu).config_files;
        files.extend(super::watchdog_gen::config_files(
            &mcu.watchdog,
            &mcu.family,
            mcu.runtime,
        ));
        files
    }
}

/// The async peripherals (USART/SPI/I2C) derived from `mcu`'s modules + pins.
fn async_periphs(mcu: &Mcu) -> embassy_async::AsyncPeriphs {
    let all = pins_of(mcu);
    let usart = modules::usart_configs(&mcu.modules);
    let spi = modules::spi_configs(&mcu.modules);
    let i2c = modules::i2c_configs(&mcu.modules);
    let lpuart = modules::lpuart_configs(&mcu.modules);
    let timer = modules::timer_configs(&mcu.modules);
    let i2s = modules::i2s_configs(&mcu.modules);
    let dac = modules::dac_configs(&mcu.modules);
    let sai = modules::sai_configs(&mcu.modules);
    let sdmmc = modules::sdmmc_configs(&mcu.modules);
    let qspi = modules::qspi_config(&mcu.modules);
    let ospi = modules::ospi_configs(&mcu.modules);
    let xspi = modules::xspi_configs(&mcu.modules);
    let hspi = modules::hspi_configs(&mcu.modules);
    let comp_instances = comparator::instances(mcu);
    let comp_pins: Vec<(u8, String, Option<String>)> = mcu
        .comp
        .keys()
        .filter_map(|n| {
            comparator::wired_pin(mcu, *n, "INP")
                .map(|inp| (*n, inp, comparator::wired_pin(mcu, *n, "INM")))
        })
        .collect();
    embassy_async::async_peripherals(
        &mcu.family,
        embassy_async::ChipData {
            dma: mcu.dma.as_ref(),
            irq_vectors: &mcu.irq_vectors,
            usart_ip: mcu.usart_ip.as_deref(),
            sdmmc_ip: mcu.sdmmc_ip.as_deref(),
        },
        embassy_async::CompInputs {
            settings: &mcu.comp,
            instances: &comp_instances,
            pins: &comp_pins,
        },
        &all,
        &usart,
        &spi,
        &i2c,
        &lpuart,
        &timer,
        &i2s,
        &dac,
        &sai,
        &sdmmc,
        qspi.as_ref(),
        &ospi,
        &xspi,
        &hspi,
    )
}

/// The async generated section for `mcu`: the GPIO/raw pin bindings (minus the
/// pins a bus driver consumes) followed by the peripheral `init(...)` calls.
fn async_section(mcu: &Mcu) -> String {
    let all = pins_of(mcu);
    let periphs = async_periphs(mcu);
    // Pins a bus driver moves into its peripheral must NOT also be bound raw.
    // Compared on `gpio()`, not `name`: that list holds the singletons the
    // drivers took, and on a package pin with two bonded pads the singleton is
    // whichever GPIO the chosen function belongs to.
    let gpio_pins: Vec<&Pin> = all
        .iter()
        .copied()
        .filter(|p| !periphs.consumed_pins.iter().any(|c| c == p.gpio()))
        .collect();
    let mut clock = rcc::graph_clock_block_for(&mcu.id, &mcu.family, &mcu.clock, mcu.clock_manual);
    // PA15, PB3 and PB4 stay the JTAG port on an F1 unless released - and a
    // project that wires one compiles either way, with a pad that does nothing.
    if mcu.family == "stm32f1"
        && all.iter().any(|p| {
            !p.reserved
                && p.selected_function != PinFunction::Unset
                && super::stm32::JTAG_PINS.contains(&p.gpio())
        })
    {
        clock = rcc::with_swd_only(&clock);
    }
    embassy_async::make_generated_section(
        &mcu.name,
        &gpio_pins,
        &clock,
        &periphs.init_calls,
        &periphs.dma_irqs,
        &mcu.watchdog_and_custom_inits(),
        &periphs.exti,
    )
}

/// Registry of every known family backend. Add new families here. Order
/// matters: the first backend whose [`handles`](FamilyBackend::handles) returns
/// true wins, so the multi-family [`StmEmbassyBackend`] must come LAST.
const BACKENDS: &[&dyn FamilyBackend] = &[
    &Stm32f1Backend,
    &Esp32Backend,
    &WbaBackend,
    // Before the generic STM32 one, which matches on a `stm32` prefix and would
    // never see these anyway — kept adjacent so the list reads as "the specific
    // ones, then the catch-all".
    &super::rp::RpBackend,
    &super::nrf::NrfBackend,
    &StmEmbassyBackend,
];

/// The async backend is chosen by runtime (not family), so it lives outside
/// [`BACKENDS`]; a `static` gives the `&'static` the dispatch returns.
static ASYNC_EMBASSY_BACKEND: AsyncEmbassyBackend = AsyncEmbassyBackend;

/// Chosen by runtime like the other two, and outside [`BACKENDS`] for the same
/// reason: a Pico on Async is a different HAL, not a different feature set.
static ASYNC_RP_BACKEND: super::rp::AsyncRpBackend = super::rp::AsyncRpBackend;

/// The micro:bit on embassy-nrf. Outside [`BACKENDS`] like the other async
/// backends, and for the same reason as the RP one: it is a different HAL crate.
static ASYNC_NRF_BACKEND: super::nrf::AsyncNrfBackend = super::nrf::AsyncNrfBackend;

// ── RTIC (STM32F1) ──────────────────────────────────────────────────────────
/// Same chip, same HAL, same init as the blocking backend — only the program
/// shape differs (see [`super::rtic`]). It therefore reuses `Stm32f1Backend`'s
/// `config_files`: the `pins/configs/*.rs` are called from `#[init]` unchanged.
struct RticBackend;

impl FamilyBackend for RticBackend {
    fn family_id(&self) -> &'static str {
        "stm32f1"
    }

    fn fresh_main_rs(&self, mcu: &Mcu) -> String {
        let all = pins_of(mcu);
        let (usart, spi, i2c) = resolve_bus_configs(mcu);
        let can = modules::can_configs(&mcu.modules);
        let usb = modules::usb_configs(&mcu.modules);
        let timer = modules::timer_configs(&mcu.modules);
        let section = rtic::make_generated_section(
            &mcu.name,
            &all,
            &mcu.clock,
            &usart,
            &spi,
            &i2c,
            &can,
            &usb,
            &timer,
            mcu.gpio_native(),
            &mcu.custom_module_inits(),
        );
        format!(
            "{}{section}
{}",
            rtic::invariant_header(&mcu.name, &mcu.id),
            rtic::RTIC_USER_TAIL
        )
    }

    fn update_main_rs(&self, mcu: &Mcu, existing: &str) -> String {
        let all = pins_of(mcu);
        let (usart, spi, i2c) = resolve_bus_configs(mcu);
        let can = modules::can_configs(&mcu.modules);
        let usb = modules::usb_configs(&mcu.modules);
        let timer = modules::timer_configs(&mcu.modules);
        let section = rtic::make_generated_section(
            &mcu.name,
            &all,
            &mcu.clock,
            &usart,
            &spi,
            &i2c,
            &can,
            &usb,
            &timer,
            mcu.gpio_native(),
            &mcu.custom_module_inits(),
        );
        rtic::splice_rtic_section(existing, &section, &mcu.name, &mcu.id)
    }

    fn config_files(&self, mcu: &Mcu) -> Vec<(String, String)> {
        Stm32f1Backend.config_files(mcu)
    }
}

static RTIC_BACKEND: RticBackend = RticBackend;

/// Whether an Async runtime has a backend for `family` — an embassy-capable
/// STM32 family (embassy-stm32), an ESP part (esp-rtos), a Pico (embassy-rp) or
/// an nRF52 (embassy-nrf). Drives both codegen dispatch and the System-tab
/// toggle's enabled state.
pub fn async_supported(family: &str) -> bool {
    ASYNC_EMBASSY_BACKEND.handles(family)
        || ASYNC_ESP_BACKEND.handles(family)
        || ASYNC_RP_BACKEND.handles(family)
        || ASYNC_NRF_BACKEND.handles(family)
}

/// Whether `family`'s async path is the ESP one (esp-rtos + embassy-executor)
/// rather than embassy-stm32. The two need DIFFERENT `[dependencies]` — same
/// crate names, different versions and features — so the Cargo.toml sync has to
/// tell them apart (see `project_gen::AsyncFlavor`).
/// Whether `family`'s async path is `embassy-rp` — which needs its own executor
/// version AND its own HAL crate, unlike every other family here.
pub fn async_is_rp(family: &str) -> bool {
    ASYNC_RP_BACKEND.handles(family)
}

pub fn async_is_esp(family: &str) -> bool {
    ASYNC_ESP_BACKEND.handles(family)
}

/// Whether `family`'s async path is `embassy-nrf`: its own HAL crate, on the
/// same executor version as embassy-rp.
pub fn async_is_nrf(family: &str) -> bool {
    ASYNC_NRF_BACKEND.handles(family)
}

/// The crate `family`'s async runtime is built on, for the System tab's Async
/// card. Named per family, so a card never names another vendor's HAL.
pub fn async_stack_name(family: &str) -> &'static str {
    if async_is_esp(family) {
        "esp-rtos"
    } else if async_is_rp(family) {
        "embassy-rp"
    } else if async_is_nrf(family) {
        "embassy-nrf"
    } else {
        "embassy-stm32"
    }
}

/// What the System tab's Async card "Details" pane says for one family: the
/// `(label, body)` rows and the code example under them.
///
/// Every body is ONE string literal, never a `\`-continuation: the
/// continuation keeps the source indentation, and this text is read by more
/// than the panel's word splitter. `async_details_rows_have_no_double_space`
/// holds it.
pub struct AsyncDetails {
    pub rows: &'static [(&'static str, &'static str)],
    pub example: &'static str,
}

/// The Async pane for `family`. Keyed with the same `esp -> rp -> nrf ->
/// stm32` chain as [`async_stack_name`] and `project_gen::async_flavor_for`,
/// so the card's subtitle, its pane and the manifest lines cannot disagree
/// about which family a chip is.
///
/// It exists because the panel had two panes, an ESP one and one written for
/// embassy-stm32, and showed the STM32 one to every other chip: a Pico read
/// "embassy-rp" in the subtitle and, under it, that its buses live in
/// `src/pins/configs/*.rs` (they are built inline in `main.rs`), that its USART
/// is always `BufferedUart` (a transport row chooses) and that DMA channels
/// are a `TODO` for the user (the generator hands them out). The micro:bit got
/// the same text and matches none of it: EasyDMA, no channels, no choice.
pub fn async_details(family: &str) -> AsyncDetails {
    if async_is_esp(family) {
        ESP_ASYNC_DETAILS
    } else if async_is_rp(family) {
        RP_ASYNC_DETAILS
    } else if async_is_nrf(family) {
        NRF_ASYNC_DETAILS
    } else {
        STM32_ASYNC_DETAILS
    }
}

/// Each row is a claim about `embassy_async.rs` and `project_gen.rs`; the USART
/// and DMA rows say what the module rows offer today (a Buffered | DMA
/// transport, channels from the family's table with a per-module picker), not
/// what they offered when the pane was first written.
const STM32_ASYNC_DETAILS: AsyncDetails = AsyncDetails {
    rows: &[
        (
            "On Apply:",
            "Regenerates main.rs with the embassy entry + Spawner, rewrites every src/pins/configs/*.rs to the async init, toggles the embassy Cargo.toml deps, then builds.",
        ),
        (
            "Entry:",
            "#[embassy_executor::main] async fn main(Spawner); the embassy executor drives the task, use .await inside the loop.",
        ),
        (
            "USART:",
            "BufferedUart on an interrupt ring (no DMA channel), or UartTx + RingBufferedUartRx on two DMA channels; the module's Async transport row chooses. Both expose embedded-io-async.",
        ),
        (
            "SPI / I2C:",
            "Each module gets a Blocking (embedded-hal 1.0) or Async-DMA (embedded-hal-async) selector on the Pins tab; the Portable/Native Init API is not used under Async.",
        ),
        (
            "DMA:",
            "Channels come from this family's table, allocated per module or pinned in the module's channel picker, and the Configuration tab's DMA card shows what was taken. A chip with no table, or two modules pinned to one channel, gets a TODO(async DMA) line in main.rs at exactly the init that needs it.",
        ),
        (
            "Cargo.toml:",
            "Adds embassy-executor 0.9 + embassy-time and toggles time-driver-any on embassy-stm32; on an F1 it first swaps stm32f1xx-hal for embassy-stm32. An async USART adds embedded-io-async + static_cell; SPI/I2C add embedded-hal (Blocking) or embedded-hal-async (Async-DMA). Leaving Async removes them again.",
        ),
        (
            "Applies to:",
            "Every STM32. The F1 is on stm32f1xx-hal on every other runtime, so for it Async is also a HAL swap: USB, CAN and SDIO are not generated there, and embassy-time takes one timer (TIM4 on an F103C8) that PWM can no longer use. The Pico, the micro:bit and the ESP parts have panes of their own.",
        ),
    ],
    // `Irqs` last: the config file takes the binding rather than declaring
    // its own, since a vector may only be bound once in the program.
    example: "let mut _serial1 = pins::configs::usart1::init(p.USART1, p.PA10, p.PA9, Irqs);\n_serial1.write_all(b\"hi\").await.ok();",
};

/// Each row is a claim about `rp.rs`: no bus config files (`config_files`
/// writes the watchdog's only), `BufferedUart` or DMA `Uart` per the transport
/// row, SPI on two channels with its init locked, I2C on interrupts, channels
/// handed out in order by `async_bus_lines`, and the manifest lines from
/// `AsyncFlavor::Rp`, `needs_async_usart` and `ensure_cyw43_deps`.
const RP_ASYNC_DETAILS: AsyncDetails = AsyncDetails {
    rows: &[
        (
            "On Apply:",
            "Regenerates main.rs with the embassy entry + Spawner and every bus built inline against one bind_interrupts! struct, swaps the Cargo.toml deps, then builds. No per-bus src/pins/configs/*.rs: the watchdog's is the only config file.",
        ),
        (
            "Entry:",
            "#[embassy_executor::main] async fn main(spawner: Spawner) on embassy-rp's Peripherals (p.PIN_0, p.UART0, p.DMA_CH0).",
        ),
        (
            "USART:",
            "BufferedUart on an interrupt ring (no DMA channel; the only RP uart type with embedded-io-async), or Uart on two DMA channels with one transfer per read and only the 32-byte FIFO between reads; the module's Async transport row chooses.",
        ),
        (
            "SPI / I2C:",
            "SPI is always async on two DMA channels (the Init selector is shown locked). I2C is interrupt driven and takes no channel, so it has no transport row.",
        ),
        (
            "DMA:",
            "Twelve channels on the RP2040, sixteen on the RP2350, handed out in order to the buses that need them (and, on a Pico W, the radio) and reported to the Configuration tab's DMA card; nothing to fill in by hand.",
        ),
        (
            "Cargo.toml:",
            "The embassy-rp line is the board's own (rp2040 or rp235xa, with its time-driver feature); adds embassy-executor 0.10 (platform-cortex-m) + embassy-time. A BufferedUart adds embedded-io-async + static_cell. Leaving Async removes them again. On a Pico W, taking the on-board LED adds cyw43 + cyw43-pio + static_cell: the LED hangs off the radio, whose driver is async only, so a Blocking project gets a comment instead.",
        ),
        (
            "Applies to:",
            "RP2040 and RP2350 boards (Pico, Pico W, Pico 2, Pico 2 W).",
        ),
    ],
    // The Pico's UART0 (GP0 TX, GP1 RX) in embassy-rp's tx-then-rx order. The
    // second line is illustrative, as the STM32 one is: the generator emits
    // nothing past the init. `BufferedUart` is embedded-io-async `Write`.
    example: "let mut uart0 = embassy_rp::uart::BufferedUart::new(p.UART0, p.PIN_0, p.PIN_1, Irqs, tx_buf, rx_buf, ucfg0);\nuart0.write_all(b\"hi\").await.ok();",
};

/// Each row is a claim about `nrf.rs`: no bus config files, `Uarte` / `Spim` /
/// `Twim` on EasyDMA with the module rows skipped (`SKIP_USART_TRANSPORT_NRF`,
/// `SKIP_ASYNC_INIT_NRF`), the shared TWISPI blocks, an armed input as a task,
/// and the manifest lines from `AsyncFlavor::Nrf` and `needs_static_cell`.
const NRF_ASYNC_DETAILS: AsyncDetails = AsyncDetails {
    rows: &[
        (
            "On Apply:",
            "Regenerates main.rs with the embassy entry + Spawner and every bus built inline against one bind_interrupts! struct, swaps the Cargo.toml deps, then builds. No per-bus src/pins/configs/*.rs: the watchdog's is the only config file.",
        ),
        (
            "Entry:",
            "#[embassy_executor::main] async fn main(spawner: Spawner); embassy_nrf::init(config) starts both clocks itself, so the Clock tab is two fields of Config.",
        ),
        (
            "USART:",
            "Always Uarte: the UARTE moves bytes by EasyDMA, which is part of the peripheral, so there is no transport to choose and no channel to take.",
        ),
        (
            "SPI / I2C:",
            "One form each, Spim and Twim on EasyDMA; the same handle has blocking_* methods for when you want to wait. SPIM0/TWIM0 and SPIM1/TWIM1 are one block each (TWISPI0/1): put a SPIM and a TWIM on the same one and the TWIM is not built (the module says so).",
        ),
        (
            "Inputs:",
            "An armed input becomes a task that awaits the edge and calls your hook, spawned on the main executor; the task priority selector has no effect on this chip.",
        ),
        (
            "Cargo.toml:",
            "The embassy-nrf line is the chip's own (the part's feature, time-driver-rtc1, gpiote, plus nfc-pins-as-gpio on a part with NFC); adds embassy-executor 0.10 (platform-cortex-m) + embassy-time. A TWIM's RAM buffer adds static_cell. Leaving Async removes them again.",
        ),
        ("Applies to:", "nRF52 (the BBC micro:bit v2 today)."),
    ],
    // `write` is embassy-nrf's own async method on `Uarte` (the type is not
    // embedded-io-async), which is why it is not `write_all`. Illustrative:
    // the generator emits nothing past the init.
    example: "let mut uarte0 = embassy_nrf::uarte::Uarte::new(p.UARTE0, p.P1_08, p.P0_06, Irqs, uarte0_cfg);\nuarte0.write(b\"hi\").await.ok();",
};

/// Moved from the panel as it was, apart from the last row, which named
/// embassy-stm32 as THE other async path when there are three.
const ESP_ASYNC_DETAILS: AsyncDetails = AsyncDetails {
    rows: &[
        (
            "On Apply:",
            "Regenerates main.rs with the esp-rtos entry + Spawner and adds the async Cargo.toml deps, then builds. The pin bindings themselves do NOT change: esp-hal's Output/Input/Uart/Spi/I2c are the same types in both runtimes.",
        ),
        (
            "Entry:",
            "#[esp_rtos::main] async fn main(_spawner: Spawner); esp_rtos::start(...) hands TIMG0 + software interrupt 0 to the scheduler, then embassy_time::Timer and .await work in the loop.",
        ),
        (
            "TIMG0:",
            "The scheduler takes peripherals.TIMG0, so your own code cannot also claim it. That is the cost of having embassy-time on this chip.",
        ),
        (
            "Why esp-rtos:",
            "NOT esp-hal-embassy: that crate needs esp-hal's private __esp_hal_embassy feature, dropped in esp-hal 1.1 (the version this template pins), so cargo cannot even resolve it. esp-rtos is its replacement, same maintainers.",
        ),
        (
            "Cargo.toml:",
            "Adds esp-rtos (chip + embassy features), embassy-executor 0.10 and embassy-time 0.5. Leaving Async removes them again.",
        ),
        (
            "Not yet:",
            "USART/SPI/I2C stay the blocking esp-hal drivers written inline in main.rs; ESP has no src/pins/configs/*.rs, so there is no async bus driver to select.",
        ),
        (
            "Applies to:",
            "Every Espressif part, RISC-V and Xtensa alike. Each other family has its own async stack, named in the card's subtitle.",
        ),
    ],
    example: "let timg0 = TimerGroup::new(peripherals.TIMG0);\nesp_rtos::start(timg0.timer0, sw_int.software_interrupt0);\n// then: embassy_time::Timer::after_millis(500).await;",
};

/// Whether an RTIC project can be generated for `family`.
///
/// Narrow on purpose, for two reasons — neither of which is "RTIC cannot do it".
///
/// This used to claim RTIC 2 had cortex-m backends only, which is false: rtic
/// 2.3 ships `riscv-esp32c3-backend` and `riscv-esp32c6-backend` alongside the
/// four thumbv ones. The C3 is one of exactly two chips RTIC names outright.
///
/// The real obstacle on ESP is WHOSE peripherals they are. That backend is
/// built on the PAC — it pulls `dep:esp32c3` and works with
/// `esp32c3::{Interrupt, Peripherals}` — while every line this IDE generates for
/// ESP is `esp-hal`. Both want to own the same silicon, and `#[rtic::app(device
/// = esp32c3)]` wants the PAC's `Peripherals`. Reconciling that is the work, not
/// flipping this flag.
///
/// On the embassy-stm32 families the obstacle is different: the generated
/// interrupt tasks are written against `stm32f1xx-hal`'s `ExtiPin` trait
/// (`make_interrupt_source`, `trigger_on_edge`, `clear_interrupt_pending_bit`),
/// which those families do not expose — so widening there means writing the task
/// bodies for another HAL.
///
/// For preemptive tasks on ESP specifically there is already a cheaper answer
/// that needs none of this: a raised [`TaskPriority`] puts the task on its own
/// `esp_rtos::embassy::InterruptExecutor`.
///
/// [`TaskPriority`]: crate::panels::mcu_module::pins::logic::pin::TaskPriority
pub fn rtic_supported(family: &str) -> bool {
    family == "stm32f1"
}

/// Whether a Native runtime is meaningful for `family` — i.e. the backend has
/// concrete-HAL (Portable/Native) peripheral templates. Only `stm32f1`
/// (stm32f1xx-hal) does; other STM32 families run on embassy-stm32 whose blocking
/// bindings are already concrete (no separate Native form), and ESP has its own
/// scheme. Drives the System-tab card's enabled state + the codegen forcing.
pub fn native_supported(family: &str) -> bool {
    family == "stm32f1"
}

/// What the **Blocking** runtime actually generates for `family` — the HAL crate
/// and the driver style.
///
/// Family-dependent because the answer is: `stm32f1xx-hal` with the portable
/// bridges on F1, `embassy-stm32` used SYNCHRONOUSLY on every other STM32, and
/// `esp-hal` on the ESP parts. A single fixed sentence described only the F1
/// case, so a user on an F2/F4/H5 read "portable blocking drivers" in the UI and
/// then found `embassy_stm32::init(...)` in `main.rs` — which reads as the
/// Runtime choice having been ignored. It has not: Blocking emits
/// `#[entry] fn main() -> !` with no executor and no `.await`; embassy-stm32 is
/// simply that family's HAL, and it is a perfectly ordinary blocking one.
pub fn blocking_hal_note(family: &str) -> &'static str {
    if family == "stm32f1" {
        "stm32f1xx-hal  ·  portable blocking drivers (embedded-io / \
         embedded-hal 1.0); per-module Native opt-in"
    } else if family == "stm32wba" || (family.starts_with("stm32") && family != "stm32f1") {
        "embassy-stm32 used SYNCHRONOUSLY — it is this family's HAL; no \
         executor, no .await"
    } else if family.starts_with("esp") {
        "esp-hal  ·  blocking drivers"
    } else if super::nrf::blocking_on_embassy(family) {
        "embassy-nrf used SYNCHRONOUSLY — nrf-hal is not used for this part; \
         no executor, no .await"
    } else if family == "nrf52833" {
        "nrf52833-hal  ·  blocking drivers (embedded-hal 1.0)"
    } else if family == "nrf52840" {
        "nrf52840-hal  ·  blocking drivers (embedded-hal 1.0)"
    } else if family == "nrf52832" {
        "nrf52832-hal  ·  blocking drivers (embedded-hal 1.0)"
    } else if super::nrf::is_nrf(family) {
        "nrf-hal  ·  blocking drivers (embedded-hal 1.0)"
    } else {
        "the family's HAL, blocking"
    }
}

/// Does this family have the `pins/configs/io.rs` GPIO bridge (the Portable /
/// Native choice)? Only the F1 backend generates it — elsewhere GPIO binds
/// straight to the HAL's own types, so the choice does not exist and its UI
/// section is hidden rather than shown greyed out.
pub fn gpio_bridge_supported(family: &str) -> bool {
    native_supported(family)
}

/// Why the **Native** runtime is unavailable on `family`, or `None` when it is
/// available. Phrased for the System tab, next to the card it explains.
///
/// The distinction Native draws — concrete HAL types instead of the portable
/// bridges — only exists where the bridges do. On an embassy family the blocking
/// types are ALREADY concrete, so Native would emit byte-for-byte what Blocking
/// emits: not a missing feature, a choice with no object.
pub fn native_unavailable_reason(family: &str) -> Option<String> {
    if native_supported(family) {
        return None;
    }
    Some(if family.starts_with("esp") {
        format!(
            "`{family}` uses the ESP HAL's own scheme — there are no portable bridges to opt out of."
        )
    } else if family.starts_with("stm32") {
        format!(
            "`{family}` runs on embassy-stm32, whose blocking types are already concrete — \
             Blocking generates exactly that code, so Native would be identical to it."
        )
    } else {
        // The Pico and nRF backends bind GPIO straight to their HAL's own
        // types; there is no portable bridge to opt out of, so Blocking already
        // IS the native form. The old else-branch said "embassy-stm32" here,
        // which was wrong for both.
        format!(
            "`{family}` binds every pin to its HAL's own types — there are no portable \
             bridges to opt out of, so Blocking already is the native form."
        )
    })
}

/// Why **RTIC** is unavailable on `family`, or `None` when it is available.
///
/// Unlike Native, this one IS missing work rather than a category error: RTIC 2
/// runs on any Cortex-M, so an F2/F4/H5 would be eligible. What is F1-specific is
/// the generated code.
pub fn rtic_unavailable_reason(family: &str) -> Option<String> {
    if rtic_supported(family) {
        return None;
    }
    Some(if family.starts_with("esp") {
        // The architecture is NOT named per chip on purpose. This used to end
        // "and `{family}` is RISC-V", which is false for the esp32, esp32s2 and
        // esp32s3 - a third of the family is Xtensa, and each of those was told
        // the wrong thing about its own core. Naming both is true of every part
        // and cannot drift when a tenth chip lands; the reason RTIC is
        // unavailable never depended on which of the two it is.
        format!(
            "RTIC 2 ships Cortex-M backends only, so no Espressif part can use it - neither the RISC-V chips nor the Xtensa ones (`{family}`)."
        )
    } else if family.starts_with("stm32") {
        format!(
            "Not written for `{family}` yet: the generated interrupt tasks use \
             stm32f1xx-hal's ExtiPin (make_interrupt_source / trigger_on_edge / \
             clear_interrupt_pending_bit), which embassy-stm32 does not expose."
        )
    } else {
        // Not "nothing equivalent exists": nrf-hal has GPIOTE and rp-hal has
        // pin interrupts. What is missing is templates, not the HAL.
        format!(
            "Not written for `{family}` yet: the RTIC templates exist for the STM32F1 only, \
             whose interrupt tasks use stm32f1xx-hal's ExtiPin. This family would need its \
             own, built on its HAL's pin interrupts."
        )
    })
}

/// Every DMA channel this project uses, and who has it.
///
/// The Configuration tab's list. Comes from the code generators themselves —
/// the async pass records what it allocated, the F1 blocking path reads the
/// tables its templates read — so the list cannot describe an allocation the
/// project does not have. Empty when nothing is on DMA, which is the normal
/// case and reads as "nothing to see" rather than as an error.
/// The PIO state machines a project takes, for the Configuration tab.
///
/// Only the RP parts have a PIO at all. Everything else reports none, and the
/// tab hides the card rather than showing "0 of 0" for a block the chip has
/// never had.
pub fn pio_uses(mcu: &Mcu) -> Vec<super::rp::PioUse> {
    if super::rp::is_rp(&mcu.family) {
        super::rp::pio_uses(mcu)
    } else {
        Vec::new()
    }
}

pub fn dma_uses(mcu: &Mcu) -> Vec<super::dma_map::DmaUse> {
    use crate::panels::mcu_module::mcu::model::Runtime;
    match mcu.runtime {
        // The ESP path is its own: esp-hal's `with_dma` takes ONE channel per
        // bus, not a TX/RX pair, so the plan is keyed by instance rather than
        // by direction.
        // FAMILY, not runtime. `with_dma` is on esp-hal's BLOCKING drivers, so a
        // Blocking ESP project takes channels exactly like an Async one -
        // `codegen_esp::dma_plan` says so itself with a bare `let _ = runtime;`.
        // Gating this arm on Async left the Configuration tab's DMA card empty
        // on the runtime where ESP DMA is most ordinary, while the generated
        // code was quietly holding the channels.
        _ if is_esp(&mcu.family) => codegen_esp::dma_plan(
            mcu.dma.as_ref(),
            if matches!(mcu.runtime, Runtime::Async) {
                EspRuntime::Async
            } else {
                EspRuntime::Blocking
            },
            &modules::spi_configs(&mcu.modules),
            &codegen_esp::i2s_instances_wired(&pins_of(mcu)),
            codegen_esp::parl_io_wired(&pins_of(mcu)).is_some(),
            !codegen_esp::lcd_wired(&pins_of(mcu)).is_empty(),
            !codegen_esp::cam_wired(&pins_of(mcu)).is_empty(),
        )
        .uses(),
        // BEFORE `async_supported`, which already answers true for RP: an
        // arm after it is unreachable, and the DMA card stayed empty on a
        // Pico for exactly that reason. The Pico has no vendor database,
        // so its channels are only knowable by asking the generator what
        // it just allocated.
        Runtime::Async if super::rp::is_rp(&mcu.family) => super::rp::dma_uses(mcu),
        // Before `async_supported` for the same reason as the RP arm, with the
        // opposite answer: an nRF's EasyDMA is inside each peripheral, so there
        // are no channels to take. Falling through ran the embassy-stm32 pass
        // on a micro:bit.
        Runtime::Async if async_is_nrf(&mcu.family) => Vec::new(),
        Runtime::Async if async_supported(&mcu.family) => async_periphs(mcu).dma_uses,
        // Only the F1 backend has a blocking DMA transport; every other family
        // reaches DMA through embassy, i.e. through the async runtime.
        Runtime::Blocking if mcu.family == "stm32f1" => super::stm32::blocking_dma_uses(mcu),
        _ => Vec::new(),
    }
}

/// Why the **Async** runtime is unavailable on `family`, or `None` when it is
/// available. Phrased for the System tab, next to the card it explains.
///
/// Every STM32 has it now, the F1 included, so the only answer left is a
/// family with no async backend at all.
pub fn async_unavailable_reason(family: &str) -> Option<String> {
    if async_supported(family) {
        return None;
    }
    Some(format!(
        "No async backend for `{family}`: the async runtimes here cover the STM32, ESP, Pico and nRF52 families."
    ))
}

/// Whether the project runs `stm32f1xx-hal` - an STM32F1 on any runtime but
/// Async, where it is on embassy-stm32 instead.
///
/// The one predicate for every fact that is about that CRATE rather than the
/// silicon: the io.rs GPIO bridge, the USB and CAN templates, milliseconds on
/// the IWDG, no window watchdog. Silicon facts - the AFIO remap groups, the
/// 40 kHz LSI - stay keyed on the family alone, because they hold on both.
pub fn uses_stm32f1xx_hal(family: &str, runtime: Runtime) -> bool {
    family == "stm32f1" && runtime != Runtime::Async
}

/// Why a module of `kind` is not offered on an F1 on the Async runtime, or
/// `None` when it is.
///
/// USB and CAN are real on that path - embassy-stm32 has a USB device driver
/// and bxCAN for the F103 - but no async template is written for either, on
/// any STM32, so the module would produce nothing. Blocking generates both.
/// The SDIO is the other way round: embassy-stm32 0.6 gives the F1's SDIO no
/// DMA channel, and its driver takes one.
///
/// `concat!` rather than `\`-continued literals: these are read in a hover,
/// and rustfmt rejoins a continuation with its indentation inside.
pub fn f1_async_module_gap(
    family: &str,
    runtime: Runtime,
    kind: crate::panels::mcu_module::modules::ModuleKind,
) -> Option<&'static str> {
    use crate::panels::mcu_module::modules::ModuleKind;
    if family != "stm32f1" || runtime != Runtime::Async {
        return None;
    }
    Some(match kind {
        ModuleKind::GenericInterfaceUsb => concat!(
            "Not generated on the Async runtime yet: embassy-stm32 has the F1's USB device ",
            "driver, but no async USB template is written here. The Blocking runtime ",
            "generates the CDC serial device."
        ),
        ModuleKind::GenericInterfaceCan => concat!(
            "Not generated on the Async runtime yet: embassy-stm32 has the F1's bxCAN ",
            "driver, but no async CAN template is written here. The Blocking runtime ",
            "generates it."
        ),
        ModuleKind::GenericInterfaceSdmmc => concat!(
            "Not generated on the Async runtime: embassy-stm32 gives the F1's SDIO no DMA ",
            "channel, and its driver needs one."
        ),
        _ => return None,
    })
}

/// Does a USB virtual module DO anything on this family?
///
/// - **stm32f1** — yes: the backend emits the whole CDC-ACM device
///   (`usb-device` + `usbd-serial` + the HAL's `stm32-usbd` feature).
/// - **ESP** — yes: the USB Serial/JTAG peripheral is hardware-fixed to two
///   pins, and the generated code says so. Nothing to configure, but the module
///   shows a real thing on the diagram.
/// - **every other STM32** — no. The embassy backends generate no USB code at
///   all, so adding the module produced exactly two stray dependencies and
///   nothing else. Offering a peripheral that cannot be generated is how a
///   configuration silently fails to reach `main.rs`.
///
/// Keyed on the family rather than on the pins, unlike the rest of
/// [`crate::panels::mcu_module::mcu::Mcu::supports_module`]: the pins exist on
/// every chip with USB, so only the BACKEND can answer this.
pub fn usb_supported(family: &str) -> bool {
    family == "stm32f1" || family.starts_with("esp")
}

#[cfg(test)]
mod blocking_note_tests {
    use super::{backend_for, blocking_hal_note, gpio_bridge_supported};

    /// The card must name the HAL the chip will ACTUALLY get — the mismatch
    /// that made a Blocking F2 project look mis-generated.
    #[test]
    fn the_note_matches_the_backend_that_will_run() {
        for family in ["stm32f2", "stm32f4", "stm32g0", "stm32h5", "stm32wba"] {
            let note = blocking_hal_note(family);
            assert!(
                note.contains("embassy-stm32"),
                "{family} generates embassy code on Blocking, so say so: {note}"
            );
            assert!(
                note.contains("no executor"),
                "and say it is the SYNC use of it: {note}"
            );
            assert!(
                backend_for(family).is_some(),
                "fixture: {family} has a backend"
            );
        }
        let f1 = blocking_hal_note("stm32f1");
        assert!(f1.contains("stm32f1xx-hal"), "{f1}");
        assert!(
            !f1.contains("embassy"),
            "F1 does not use embassy on Blocking: {f1}"
        );
        assert!(blocking_hal_note("esp32c3").contains("esp-hal"));
    }

    /// The GPIO Portable/Native choice exists only where `io.rs` is generated.
    #[test]
    fn the_gpio_bridge_is_f1_only() {
        assert!(gpio_bridge_supported("stm32f1"));
        for family in ["stm32f2", "stm32f4", "stm32wba", "esp32c3"] {
            assert!(!gpio_bridge_supported(family), "{family}");
        }
    }

    /// A greyed card must be able to say why — and the two reasons are NOT the
    /// same kind of thing, which is the point of having separate texts.
    /// No chip is told the wrong thing about its own core.
    ///
    /// The RTIC explanation used to end "and `{family}` is RISC-V" for every
    /// Espressif part — false for the esp32, esp32s2 and esp32s3, which are
    /// Xtensa. A third of the family read a statement about its own silicon
    /// that was simply wrong, in the one message meant to explain something.
    ///
    /// Checked against each definition's real TARGET, not a list, so it follows
    /// the chips rather than someone's memory of them.
    #[test]
    fn no_chip_is_told_the_wrong_architecture() {
        use crate::panels::mcu_module::builtins::builtin_definitions;

        for d in builtin_definitions() {
            let Some(why) = super::rtic_unavailable_reason(&d.family) else {
                continue; // RTIC is available here; nothing to explain.
            };
            let xtensa = d.project.target.starts_with("xtensa");
            let riscv = d.project.target.starts_with("riscv");

            if xtensa {
                assert!(
                    !why.contains("is RISC-V"),
                    "{} is Xtensa and was told: {why}",
                    d.id
                );
            }
            if riscv {
                assert!(
                    !why.contains("is Xtensa"),
                    "{} is RISC-V and was told: {why}",
                    d.id
                );
            }
            // And the run-of-spaces scar, which a continued literal leaves the
            // moment rustfmt joins it — this message has already had one.
            assert!(
                !why.contains("  "),
                "{}: a joined continuation left a gap: {why}",
                d.id
            );
        }
    }

    #[test]
    fn every_unavailable_runtime_can_explain_itself() {
        use super::{native_unavailable_reason, rtic_unavailable_reason};
        // Available: nothing to explain.
        assert!(native_unavailable_reason("stm32f1").is_none());
        assert!(rtic_unavailable_reason("stm32f1").is_none());

        for family in ["stm32f2", "stm32f4", "stm32h5"] {
            let native =
                native_unavailable_reason(family).expect("greyed means it must explain itself");
            // Native is a category error: it would generate the same code.
            assert!(native.contains(family), "name the chip's family: {native}");
            assert!(
                native.contains("identical") || native.contains("already concrete"),
                "say it would be the same code, not that it is missing: {native}"
            );

            let rtic =
                rtic_unavailable_reason(family).expect("greyed means it must explain itself");
            // RTIC is missing work — it must NOT read as impossible.
            assert!(
                rtic.contains("ExtiPin") && rtic.contains("yet"),
                "name what is missing: {rtic}"
            );
        }

        // RISC-V is a different answer again.
        let esp = rtic_unavailable_reason("esp32c3").expect("greyed means it must explain itself");
        assert!(esp.contains("RISC-V"), "{esp}");

        // A Cortex-M family outside STM32 is missing templates, not a HAL
        // feature: nrf-hal has GPIOTE and rp-hal has pin interrupts.
        for family in ["nrf52833", "rp2040", "rp235x"] {
            let rtic =
                rtic_unavailable_reason(family).expect("greyed means it must explain itself");
            assert!(rtic.contains(family) && rtic.contains("yet"), "{rtic}");
            assert!(rtic.contains("STM32F1 only"), "{rtic}");
            assert!(!rtic.contains("nothing equivalent"), "{rtic}");
            assert!(
                !rtic.contains("  "),
                "a joined continuation left a gap: {rtic}"
            );
        }
    }

    /// The Async card names the stack each family would actually build on.
    #[test]
    fn the_async_card_names_the_familys_own_stack() {
        use super::async_stack_name;
        assert_eq!(async_stack_name("stm32f4"), "embassy-stm32");
        assert_eq!(async_stack_name("stm32f1"), "embassy-stm32");
        assert_eq!(async_stack_name("rp2040"), "embassy-rp");
        assert_eq!(async_stack_name("rp235x"), "embassy-rp");
        assert_eq!(async_stack_name("nrf52833"), "embassy-nrf");
        assert_eq!(async_stack_name("esp32c3"), "esp-rtos");
    }

    /// One chip per async family, with the Rust path its example must use.
    /// The STM32 path is the config module; the others build inline in
    /// `main.rs` against their HAL crate.
    const ASYNC_FAMILIES: &[(&str, &str)] = &[
        ("stm32f4", "pins::configs::"),
        ("stm32f1", "pins::configs::"),
        ("rp2040", "embassy_rp::"),
        ("rp235x", "embassy_rp::"),
        ("nrf52833", "embassy_nrf::"),
        ("esp32c3", "esp_rtos::"),
    ];

    fn pane_text(family: &str) -> String {
        let d = super::async_details(family);
        let mut s: String = d
            .rows
            .iter()
            .map(|(label, body)| format!("{label} {body}\n"))
            .collect();
        s.push_str(d.example);
        s
    }

    /// The pane under the Async card names the stack the subtitle names, and
    /// its example is written against that stack.
    #[test]
    fn the_async_pane_names_its_own_stack() {
        for (family, path) in ASYNC_FAMILIES {
            let d = super::async_details(family);
            let stack = super::async_stack_name(family);
            let rows = pane_text(family);
            assert!(rows.contains(stack), "{family}: no {stack} in {rows}");
            assert!(d.example.contains(path), "{family}: {}", d.example);
        }
    }

    /// The bug this exists for: the STM32 pane shown to a Pico or a
    /// micro:bit. Nothing in the other three panes describes the STM32 flow.
    #[test]
    fn no_other_pane_describes_the_stm32_flow() {
        for (family, _) in ASYNC_FAMILIES
            .iter()
            .filter(|(f, _)| !f.starts_with("stm32"))
        {
            let rows = pane_text(family);
            for stm32_only in [
                "embassy-stm32",
                "rewrites every src/pins/configs",
                "time-driver-any",
                "TODO",
                "PA10",
            ] {
                assert!(
                    !rows.contains(stm32_only),
                    "{family} pane says {stm32_only:?}: {rows}"
                );
            }
        }
    }

    /// The four panes read alike: the same first, second and last labels, and
    /// a Cargo.toml row in each, so switching chips moves the eye to the same
    /// rows.
    #[test]
    fn every_pane_has_the_same_spine_of_labels() {
        for (family, _) in ASYNC_FAMILIES {
            let labels: Vec<&str> = super::async_details(family)
                .rows
                .iter()
                .map(|(l, _)| *l)
                .collect();
            assert_eq!(labels.first(), Some(&"On Apply:"), "{family}: {labels:?}");
            assert_eq!(labels.get(1), Some(&"Entry:"), "{family}: {labels:?}");
            assert!(labels.contains(&"Cargo.toml:"), "{family}: {labels:?}");
            assert_eq!(labels.last(), Some(&"Applies to:"), "{family}: {labels:?}");
        }
    }

    /// A `\`-continuation carries the source indentation into the text as a
    /// run of spaces. Every body is one literal; this is what notices when
    /// one is not.
    #[test]
    fn async_details_rows_have_no_double_space() {
        for (family, _) in ASYNC_FAMILIES {
            for line in pane_text(family).lines() {
                assert!(!line.contains("  "), "{family}: {line:?}");
            }
        }
    }

    /// Async is greyed only where no async backend exists - since the F1 got
    /// its path, that is no family this IDE ships - and the card says why.
    #[test]
    fn the_async_card_is_greyed_only_without_a_backend() {
        use super::async_unavailable_reason;
        // Every family with a backend: nothing to explain.
        for family in [
            "stm32f1", "stm32f2", "stm32f4", "stm32g0", "stm32h5", "stm32wba", "esp32c3", "rp2040",
            "nrf52833",
        ] {
            assert!(
                async_unavailable_reason(family).is_none(),
                "{family} has an async backend"
            );
        }
        // A family with no backend at all gets the generic answer.
        let other = async_unavailable_reason("stm8").expect("no backend, no async");
        assert!(other.contains("stm8"), "{other}");
    }

    /// USB, CAN and SDIO on an F1 on Async: kept out of the palette but still
    /// in sight, with a reason that says what is missing - never the chip.
    #[test]
    fn f1_async_names_the_modules_it_cannot_generate() {
        use super::{f1_async_module_gap, uses_stm32f1xx_hal};
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::ModuleKind;
        for kind in [
            ModuleKind::GenericInterfaceUsb,
            ModuleKind::GenericInterfaceCan,
            ModuleKind::GenericInterfaceSdmmc,
        ] {
            let why = f1_async_module_gap("stm32f1", Runtime::Async, kind).expect("a gap");
            assert!(why.contains("Async runtime"), "{why}");
            assert!(!why.contains("  "), "a joined continuation: {why}");
            for rt in [Runtime::Blocking, Runtime::Native, Runtime::Rtic] {
                assert!(f1_async_module_gap("stm32f1", rt, kind).is_none(), "{rt:?}");
            }
            assert!(f1_async_module_gap("stm32f4", Runtime::Async, kind).is_none());
        }
        // USB and CAN name the runtime that does generate them.
        for kind in [
            ModuleKind::GenericInterfaceUsb,
            ModuleKind::GenericInterfaceCan,
        ] {
            let why = f1_async_module_gap("stm32f1", Runtime::Async, kind).unwrap_or("");
            assert!(why.contains("yet") && why.contains("Blocking"), "{why}");
        }
        assert!(
            f1_async_module_gap("stm32f1", Runtime::Async, ModuleKind::GenericInterfaceSpi)
                .is_none()
        );
        assert!(uses_stm32f1xx_hal("stm32f1", Runtime::Blocking));
        assert!(uses_stm32f1xx_hal("stm32f1", Runtime::Rtic));
        assert!(!uses_stm32f1xx_hal("stm32f1", Runtime::Async));
        assert!(!uses_stm32f1xx_hal("stm32f4", Runtime::Blocking));
    }

    /// The palette's disabled row with the reason is for a peripheral the chip
    /// HAS. An F1 without CAN pads (the value line) gets no CAN row at all.
    #[test]
    fn the_palette_explains_only_the_f1_modules_the_chip_has() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::ModuleKind;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mut mcu = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        mcu.runtime = Runtime::Async;
        for kind in [
            ModuleKind::GenericInterfaceUsb,
            ModuleKind::GenericInterfaceCan,
        ] {
            assert!(!mcu.supports_module(kind), "{kind:?}");
            assert!(mcu.hardware_only_reason(kind).is_some(), "{kind:?}");
        }
        for p in mcu.iter_all_pins_mut() {
            p.available_functions
                .retain(|f| !matches!(f, PinFunction::CanRx | PinFunction::CanTx));
        }
        assert!(
            mcu.hardware_only_reason(ModuleKind::GenericInterfaceCan)
                .is_none()
        );
        assert!(
            mcu.hardware_only_reason(ModuleKind::GenericInterfaceUsb)
                .is_some()
        );
    }

    /// USB follows what the BACKEND writes, not what the pins allow.
    #[test]
    fn usb_follows_the_backend_not_the_pins() {
        use super::usb_supported;
        // F1 emits the full CDC device; ESP acknowledges its fixed peripheral.
        assert!(usb_supported("stm32f1"));
        assert!(usb_supported("esp32c3"));
        // The embassy families emit nothing — offering it there added two
        // dependencies and no code.
        for family in ["stm32f2", "stm32f4", "stm32g0", "stm32h5", "stm32wba"] {
            assert!(!usb_supported(family), "{family}");
        }
    }
}

/// Look up the backend for a family key, if one is registered.
///
/// Families without a backend yet (e.g. "stm8") return `None`; callers fall
/// back to "no code generation" so an unconfigured chip stays safe.
pub fn backend_for(family: &str) -> Option<&'static dyn FamilyBackend> {
    BACKENDS.iter().copied().find(|b| b.handles(family))
}

/// Look up the backend honouring the project [`Runtime`]. Async re-targets every
/// embassy-capable STM32 family to the async embassy backend and the ESP32-C3 to
/// the esp-rtos one; everything else — and all of Blocking — falls through to
/// [`backend_for`].
///
/// [`Runtime`]: crate::panels::mcu_module::mcu::Runtime
pub fn backend_for_runtime(family: &str, runtime: Runtime) -> Option<&'static dyn FamilyBackend> {
    if runtime == Runtime::Async && ASYNC_ESP_BACKEND.handles(family) {
        return Some(&ASYNC_ESP_BACKEND);
    }
    if runtime == Runtime::Async && ASYNC_RP_BACKEND.handles(family) {
        return Some(&ASYNC_RP_BACKEND);
    }
    if runtime == Runtime::Async && ASYNC_NRF_BACKEND.handles(family) {
        return Some(&ASYNC_NRF_BACKEND);
    }
    if runtime == Runtime::Async && async_supported(family) {
        return Some(&ASYNC_EMBASSY_BACKEND);
    }
    if runtime == Runtime::Rtic && rtic_supported(family) {
        return Some(&RTIC_BACKEND);
    }
    backend_for(family)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_families_resolve() {
        assert_eq!(backend_for("stm32f1").unwrap().family_id(), "stm32f1");
        assert_eq!(backend_for("esp32c3").unwrap().family_id(), "esp32c3");
        assert_eq!(backend_for("stm32wba").unwrap().family_id(), "stm32wba");
    }

    #[test]
    fn unknown_family_is_none() {
        assert!(backend_for("stm8").is_none());
        assert!(backend_for("").is_none());
        // Not a part the nRF table has a row for. The nRF5340's application
        // core has one now; the nRF51 and the 5340's network core do not.
        assert!(backend_for("nrf51").is_none());
        assert!(backend_for("nrf5340-net").is_none());
    }

    #[test]
    fn the_nrf52_parts_resolve_to_the_nrf_backend() {
        for fam in ["nrf52833", "nrf52840", "nrf5340"] {
            assert_eq!(backend_for(fam).unwrap().family_id(), "nrf52833", "{fam}");
        }
    }

    /// Any other STM32 family routes to the generic embassy backend, and it
    /// emits a complete, buildable embassy skeleton.
    #[test]
    fn other_stm32_families_use_the_generic_embassy_backend() {
        for fam in [
            "stm32f4", "stm32g0", "stm32g4", "stm32l4", "stm32h7", "stm32c0",
        ] {
            let b = backend_for(fam).unwrap_or_else(|| panic!("no backend for {fam}"));
            assert_eq!(
                b.family_id(),
                "stm32",
                "{fam} should hit the generic backend"
            );
        }
        // The dedicated backends still win over the generic one.
        assert_eq!(backend_for("stm32f1").unwrap().family_id(), "stm32f1");
        assert_eq!(backend_for("stm32wba").unwrap().family_id(), "stm32wba");

        use crate::panels::mcu_module::mcu_def::McuDefinition;
        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        // A family with no RCC recipe (h7). Its clock is no longer left at the
        // reset default: `generic_recipe` reads the tree's canonical spine and
        // emits an explicit block, which is the point of that path — a chip
        // whose Clock tab is full has no business generating `Default::default`.
        def.family = "stm32h7".into();
        def.id = "stm32h743zi".into();
        let code = def.build_mcu().fresh_main_rs();
        assert!(code.contains("HAL: embassy-stm32 (blocking"));
        assert!(
            code.contains("no executor"),
            "the header must say WHY embassy appears on the blocking runtime"
        );
        assert!(code.contains("fn main() -> !"));
        assert!(
            code.contains("embassy_stm32::init(config)")
                && code.contains("no verified RCC recipe for this family"),
            "the tree drives the clock, and the block says the shape is a guess"
        );
        assert!(code.contains(crate::panels::mcu_module::codegen::GEN_BEGIN));
    }

    /// The Async runtime re-targets every STM32 family - the F1 included - to
    /// the async backend, while Blocking keeps each family's default one.
    #[test]
    fn async_runtime_dispatch() {
        // Async supported on every STM32 and on the ESP parts.
        assert!(async_supported("stm32f4"));
        assert!(async_supported("stm32g0"));
        assert!(async_supported("stm32wba"));
        assert!(async_supported("stm32f1"));
        assert!(async_supported("esp32c3")); // esp-rtos
        assert!(!async_is_esp("stm32f4"));
        assert!(async_is_esp("esp32c3"));
        // The Pico families have an async path of their own now, and it must NOT
        // be the embassy-stm32 one: `embassy-rp` is a different HAL crate and a
        // different executor version.
        assert!(async_supported("rp2040"));
        assert!(async_supported("rp235x"));
        assert!(async_is_rp("rp2040"));
        assert!(!async_is_rp("stm32f4"));
        assert!(!async_is_esp("rp2040"));
        assert_eq!(
            backend_for_runtime("rp2040", Runtime::Async)
                .unwrap()
                .family_id(),
            "rp-async"
        );
        assert_eq!(
            backend_for_runtime("rp2040", Runtime::Blocking)
                .unwrap()
                .family_id(),
            "rp2040"
        );
        // The same for the nRF52 parts: embassy-nrf on Async, nrf-hal otherwise.
        for family in ["nrf52833", "nrf52840"] {
            assert!(async_supported(family), "{family}");
            assert!(async_is_nrf(family), "{family}");
            assert!(!async_is_rp(family) && !async_is_esp(family), "{family}");
            assert_eq!(
                backend_for_runtime(family, Runtime::Async)
                    .unwrap()
                    .family_id(),
                "nrf-async"
            );
            assert_eq!(
                backend_for_runtime(family, Runtime::Blocking)
                    .unwrap()
                    .family_id(),
                "nrf52833"
            );
        }
        assert!(!async_is_nrf("rp2040") && !async_is_nrf("stm32f4"));

        // Blocking always uses the family default.
        assert_eq!(
            backend_for_runtime("stm32f4", Runtime::Blocking)
                .unwrap()
                .family_id(),
            "stm32"
        );
        // Async on an embassy family → the async backend.
        assert_eq!(
            backend_for_runtime("stm32f4", Runtime::Async)
                .unwrap()
                .family_id(),
            "stm32-async"
        );
        // Async on an F1 is embassy too: stm32f1xx-hal is its HAL on every
        // other runtime, and Async swaps it out. (It used to be INERT here - the
        // blocking code under an Async label - until F1 got its async path.)
        assert_eq!(
            backend_for_runtime("stm32f1", Runtime::Async)
                .unwrap()
                .family_id(),
            "stm32-async"
        );
        for rt in [Runtime::Blocking, Runtime::Native, Runtime::Rtic] {
            assert_eq!(
                backend_for_runtime("stm32f1", rt).unwrap().family_id(),
                "stm32f1",
                "{rt:?} stays on stm32f1xx-hal"
            );
        }
        // ...and it really is embassy: the executor's entry, embassy's init, a
        // USART file whose pins carry the AFIO remap, no stm32f1xx-hal anywhere.
        {
            use crate::panels::mcu_module::builtins::builtin_for;
            use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            for (name, func) in [
                ("PA9", PinFunction::UsartTx(1)),
                ("PA10", PinFunction::UsartRx(1)),
                ("PC13", PinFunction::GpioOutput),
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

            mcu.runtime = Runtime::Async;
            assert!(mcu.is_async(), "the F1 has an async path");
            // The two flags `AppIde::save` reads to pick the dependency set.
            assert!(!mcu.is_rtic());
            assert!(!mcu.is_native());
            let main = mcu.fresh_main_rs();
            assert!(main.contains("#[embassy_executor::main]"), "{main}");
            assert!(main.contains("embassy_stm32::init(config)"), "{main}");
            assert!(!main.contains("stm32f1xx_hal"), "{main}");
            let cfgs = mcu.config_files();
            let usart = &cfgs
                .iter()
                .find(|(n, _)| n == "usart1.rs")
                .unwrap_or_else(|| panic!("usart1.rs: {cfgs:?}"))
                .1;
            assert!(usart.contains("BufferedUart"), "{usart}");
            assert!(usart.contains("pub fn init<'d, A>("), "{usart}");
            assert!(
                usart.contains("impl RxPin<peripherals::USART1, A>"),
                "{usart}"
            );
            assert!(!usart.contains("stm32f1xx_hal"), "{usart}");
        }
        // ESP32-C3 async is a DIFFERENT backend (esp-rtos), not the embassy one.
        assert_eq!(
            backend_for_runtime("esp32c3", Runtime::Async)
                .unwrap()
                .family_id(),
            "esp32c3-async"
        );
        assert_eq!(
            backend_for_runtime("esp32c3", Runtime::Blocking)
                .unwrap()
                .family_id(),
            "esp32c3"
        );
        // Native / RTIC are still not offered on ESP — they fall back to blocking.
        for rt in [Runtime::Native, Runtime::Rtic] {
            assert_eq!(
                backend_for_runtime("esp32c3", rt).unwrap().family_id(),
                "esp32c3"
            );
        }
    }

    /// Every runtime switch on the F1 re-splices an EXISTING `main.rs`, and four
    /// runtimes write three shapes: Blocking/Native close `fn main` in the user
    /// tail, Async closes `async fn main` there too, RTIC closes nothing (the
    /// whole `mod app` is generated). A still-pristine file must come out exactly
    /// as a fresh one would, from any runtime to any other.
    #[test]
    fn every_f1_runtime_switch_rewrites_a_pristine_main_rs_whole() {
        use crate::panels::mcu_module::builtins::builtin_for;
        let at = |rt: Runtime| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            mcu.runtime = rt;
            mcu.pending_runtime = rt;
            mcu
        };
        let all = [
            Runtime::Blocking,
            Runtime::Native,
            Runtime::Rtic,
            Runtime::Async,
        ];
        let mut wrong = Vec::new();
        for from in all {
            for to in all {
                let switched = at(to).update_main_rs(&at(from).fresh_main_rs());
                if switched != at(to).fresh_main_rs() {
                    wrong.push(format!("{from:?} -> {to:?}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The same switches over a tail the user has WRITTEN in. It is theirs, so
    /// it stays - but the entry the new runtime opens still has to close. RTIC's
    /// tail holds helpers at module level and closes nothing; the others close
    /// their `fn main`. And whatever wrote the header, it is this runtime's
    /// afterwards: no executor import outside Async.
    #[test]
    fn a_user_tail_survives_an_f1_runtime_switch_and_still_closes_main() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::codegen::{GEN_BEGIN, GEN_END};
        let at = |rt: Runtime| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            mcu.runtime = rt;
            mcu.pending_runtime = rt;
            mcu
        };
        let balance = |s: &str| s.matches('{').count() as i64 - s.matches('}').count() as i64;
        // The user's own code: a line in the loop, where there is one, and a
        // module-level `fn` after `main` (RTIC: after its seed comment). The
        // loop line holds a `'}'`, a `"{"` and a commented `{}`: braces the
        // cut must not count, and that net to zero for `balance` below.
        let edited = |rt: Runtime| {
            let fresh = at(rt).fresh_main_rs();
            let cut = fresh.find(GEN_END).expect("markers") + GEN_END.len();
            let (head, tail) = fresh.split_at(cut);
            let mine = if rt == Runtime::Rtic {
                format!("{tail}\nfn module_helper() {{}}\n")
            } else {
                let looped =
                    tail.replacen("loop {", "loop {\n        loop_line('}', \"{\"); // {}", 1);
                format!("{looped}\nfn module_helper() {{}}\n")
            };
            let head = head.replacen("pub mod pins;\n", "pub mod pins;\nmod my_mod;\n", 1);
            format!("{head}{mine}")
        };
        let all = [Runtime::Blocking, Runtime::Rtic, Runtime::Async];
        let mut wrong = Vec::new();
        for from in all {
            for to in all {
                let old = edited(from);
                let switched = at(to).update_main_rs(&old);
                let pair = format!("{from:?} -> {to:?}");
                // Into RTIC, the loop that closed `fn main` goes
                // (`rtic::splice_rtic_section`): it has no place in an app
                // whose idle task is generated. Everything after `main` stays.
                // (An RTIC file has no loop line to begin with.)
                let loop_kept = from != Runtime::Rtic && to != Runtime::Rtic;
                if switched.contains("loop_line(") != loop_kept {
                    wrong.push(format!("{pair}: the loop line kept = {}", !loop_kept));
                }
                if !switched.contains("fn module_helper()") {
                    wrong.push(format!("{pair}: the fn after main is gone"));
                }
                if balance(&switched) != balance(&at(to).fresh_main_rs()) {
                    wrong.push(format!("{pair}: the braces no longer close"));
                }
                let head = &switched[..switched.find(GEN_BEGIN).unwrap_or(0)];
                if head.contains("embassy_executor") != (to == Runtime::Async)
                    || head.contains("use cortex_m_rt::entry;") != (to == Runtime::Blocking)
                {
                    wrong.push(format!("{pair}: the header is another runtime's"));
                }
                // A `mod` the user declared up there survives a refit; the
                // Async header is rebuilt whole, on the way in and out.
                let refitted = from != Runtime::Async && to != Runtime::Async;
                if head.contains("mod my_mod;") != refitted {
                    wrong.push(format!(
                        "{pair}: the user's header line kept = {}",
                        !refitted
                    ));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// A project with NO pin wired still calls what the Configuration tab
    /// configured. The "select pins" default block used to take no inits at
    /// all: a watchdog-only project got its config file and a main.rs that
    /// never called it - and the flash store's `flash` would have vanished too.
    #[test]
    fn a_project_with_no_pins_still_inits_what_the_configuration_tab_set() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::flash_store::FlashStoreConfig;
        use crate::panels::mcu_module::watchdog::{EspWdtConfig, IwdgConfig};
        for rt in [Runtime::Blocking, Runtime::Async] {
            let mut esp = builtin_for("esp32c3").expect("built-in C3").build_mcu();
            esp.runtime = rt;
            esp.pending_runtime = rt;
            assert!(
                esp.fresh_main_rs().contains("Select pins"),
                "{rt:?}: nothing wired"
            );
            esp.watchdog.rwdt = Some(EspWdtConfig {
                timeout_us: 2_000_000,
            });
            esp.flash_store = Some(FlashStoreConfig::default_for("esp32c3"));
            let main = esp.fresh_main_rs();
            assert!(
                main.contains("pins::configs::rwdt::init(peripherals.LPWR)"),
                "{rt:?}\n{main}"
            );
            assert!(
                main.contains("esp_storage::FlashStorage::new(peripherals.FLASH)"),
                "{rt:?}\n{main}"
            );
        }
        let mut f1 = builtin_for("stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        f1.watchdog.iwdg = Some(IwdgConfig {
            timeout_us: 2_000_000,
        });
        let main = f1.fresh_main_rs();
        assert!(main.contains("Select pins"), "nothing wired:\n{main}");
        assert!(
            main.contains("pins::configs::iwdg::init(dp.IWDG)"),
            "{main}"
        );
    }

    /// The flash store, end to end on the model: on, it writes the config
    /// file, the GEN lines and the line at the head of an untouched tail; off,
    /// all three go. A tail the user wrote in is never touched, and a Runtime
    /// switch still swaps the loop seed under the store line.
    #[test]
    fn the_flash_store_follows_its_toggle_through_main_and_the_config_files() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::codegen::flash_store_gen::{FILE, TAIL_SEED};
        use crate::panels::mcu_module::flash_store::FlashStoreConfig;
        let mut mcu = builtin_for("esp32c3").expect("built-in C3").build_mcu();
        let off = mcu.fresh_main_rs();
        mcu.flash_store = Some(FlashStoreConfig::default_for("esp32c3"));
        let on = mcu.update_main_rs(&off);
        assert!(on.contains(TAIL_SEED), "{on}");
        assert!(
            on.contains("use crate::pins::configs::flash_store;"),
            "{on}"
        );
        assert_eq!(
            on,
            mcu.fresh_main_rs(),
            "a switched file equals a fresh one"
        );
        assert!(mcu.config_files().iter().any(|(n, _)| n == FILE));

        // Async and back, with the line in place: the seed under it follows.
        mcu.runtime = Runtime::Async;
        let on_async = mcu.update_main_rs(&on);
        assert_eq!(on_async, mcu.fresh_main_rs(), "{on_async}");
        mcu.runtime = Runtime::Blocking;
        assert_eq!(mcu.update_main_rs(&on_async), on);

        // Off again: the line, the GEN lines and the file go.
        mcu.flash_store = None;
        assert_eq!(mcu.update_main_rs(&on), off);
        assert!(!mcu.config_files().iter().any(|(n, _)| n == FILE));

        // A loop the user wrote in gets no seed - the card shows the line.
        mcu.flash_store = Some(FlashStoreConfig::default_for("esp32c3"));
        let mine = off.replace("// Your main loop code here.", "led.toggle();");
        let kept = mcu.update_main_rs(&mine);
        assert!(!kept.contains(TAIL_SEED), "{kept}");
        assert!(
            kept.contains("FlashStorage::new(peripherals.FLASH)"),
            "{kept}"
        );

        // Not generated on a chip it is not supported on, even when set.
        let mut c6 = builtin_for("esp32c6").expect("built-in C6").build_mcu();
        c6.flash_store = Some(FlashStoreConfig::default_for("esp32c6"));
        assert!(!c6.fresh_main_rs().contains("FlashStorage"));
        assert!(!c6.config_files().iter().any(|(n, _)| n == FILE));
    }

    /// An STM32F103 store: on its own HAL the adapter takes over `flash` once
    /// the clocks are frozen; on Async embassy-stm32's `Flash` takes
    /// `p.FLASH`; on RTIC nothing (the store would be a Local resource), and
    /// a part whose last pages cost too much (an F411's 128 KiB sectors)
    /// gets nothing either.
    #[test]
    fn the_stm32_flash_store_follows_the_runtime_and_the_part() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::codegen::flash_store_gen::{FILE, HAL_FILE, TAIL_SEED};
        use crate::panels::mcu_module::flash_store::{FlashStoreConfig, geometry};
        let def = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut mcu = def.build_mcu();
        let (geo, _) = geometry(&mcu.name).expect("the F103C8 is in the table");
        mcu.flash_store = Some(FlashStoreConfig::default_stm32(&geo));
        let file = |m: &Mcu, name: &str| {
            m.config_files()
                .into_iter()
                .find(|(n, _)| n == name)
                .map(|(_, b)| b)
        };
        let store_file = |m: &Mcu| file(m, FILE);

        for rt in [Runtime::Blocking, Runtime::Native] {
            mcu.runtime = rt;
            let main = mcu.fresh_main_rs();
            let freeze = main
                .find("freeze(&mut flash.acr)")
                .expect("the clocks freeze");
            let adapter = main
                .find("let mut flash = crate::pins::configs::flash_store_hal::F1Flash::new(flash);")
                .unwrap_or_else(|| panic!("{rt:?}: no adapter line\n{main}"));
            assert!(
                freeze < adapter,
                "{rt:?}: the adapter must come after the clocks"
            );
            assert!(main.contains("use crate::pins::configs::flash_store;"));
            assert!(main.contains(TAIL_SEED), "{rt:?}: {main}");
            let body = store_file(&mcu).expect("the store's file");
            assert!(body.contains("0xF800..0x10000"), "{body}");
            let glue = file(&mcu, HAL_FILE).expect("the glue file");
            assert!(glue.contains("pub struct F1Flash"), "{glue}");
        }

        mcu.runtime = Runtime::Async;
        let main = mcu.fresh_main_rs();
        assert!(
            main.contains("let mut flash = embassy_stm32::flash::Flash::new_blocking(p.FLASH);"),
            "{main}"
        );
        assert!(main.contains(TAIL_SEED), "{main}");
        let glue = file(&mcu, HAL_FILE).expect("the glue file");
        assert!(glue.contains("MAX_ERASE_SIZE") && !glue.contains("F1Flash"));

        mcu.runtime = Runtime::Rtic;
        let main = mcu.fresh_main_rs();
        assert!(!main.contains("flash_store"), "{main}");
        assert!(store_file(&mcu).is_none() && file(&mcu, HAL_FILE).is_none());

        // Off: nothing of it left in a Blocking main.
        mcu.runtime = Runtime::Blocking;
        let on = mcu.fresh_main_rs();
        mcu.flash_store = None;
        let off = mcu.update_main_rs(&on);
        assert!(
            !off.contains("flash_store") && !off.contains("F1Flash"),
            "{off}"
        );
        assert_eq!(off, mcu.fresh_main_rs());

        // An F411 the importer would build: 128 KiB sectors at the end, so the
        // store goes into the 16 KiB ones right after the vector table, through
        // embassy's first flash region.
        let mut f4 = def.clone();
        f4.display_name = "STM32F411RETx".into();
        f4.family = "stm32f4".into();
        let mut f4 = f4.build_mcu();
        let (g4, _) = geometry(&f4.name).expect("the F411RE is in the table");
        f4.flash_store = Some(FlashStoreConfig::default_after_vectors(&g4));
        let main = f4.fresh_main_rs();
        assert!(main.contains(".into_blocking_regions()"), "{main}");
        assert!(main.contains(".bank1_region1;"), "{main}");
        let body = store_file(&f4).expect("the store's file");
        assert!(body.contains("0x4000..0xC000;"), "{body}");
        assert!(body.contains("right after the vector table"), "{body}");
        let glue = file(&f4, HAL_FILE).expect("the glue file");
        assert!(glue.contains("BANK1_REGION1.erase_size"), "{glue}");
    }

    /// RTIC -> X -> RTIC hands back the RTIC tail it started with: leaving
    /// RTIC puts a fresh seed IN FRONT of the helpers, and coming back cuts
    /// exactly that seed off again. Through Async the header is rebuilt, so only
    /// the tail is compared there.
    #[test]
    fn an_rtic_tail_survives_a_round_trip_through_every_runtime() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::codegen::GEN_END;
        let at = |rt: Runtime| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            mcu.runtime = rt;
            mcu.pending_runtime = rt;
            mcu
        };
        let fresh = at(Runtime::Rtic).fresh_main_rs();
        let rtic = format!(
            "{fresh}\nfn module_helper() -> u32 {{\n    if true {{ 1 }} else {{ 2 }}\n}}\n"
        );
        let tail = |s: &str| s[s.find(GEN_END).expect("markers")..].to_owned();
        for middle in [Runtime::Blocking, Runtime::Native, Runtime::Async] {
            let away = at(middle).update_main_rs(&rtic);
            let back = at(Runtime::Rtic).update_main_rs(&away);
            if middle == Runtime::Async {
                assert_eq!(tail(&back), tail(&rtic), "via {middle:?}");
            } else {
                assert_eq!(back, rtic, "via {middle:?}");
            }
        }
    }

    /// A header the user wrote the `entry` import into their own way - with a
    /// fault handler beside it, or a comment after it - is left alone on every
    /// splice, and gets no second import on the way back from RTIC (E0252).
    #[test]
    fn a_user_written_entry_import_is_never_doubled() {
        use crate::panels::mcu_module::builtins::builtin_for;
        use crate::panels::mcu_module::codegen::GEN_BEGIN;
        let at = |rt: Runtime| {
            let mut mcu = builtin_for("stm32f103c8t6")
                .expect("built-in F103")
                .build_mcu();
            mcu.runtime = rt;
            mcu.pending_runtime = rt;
            mcu
        };
        let imports = |s: &str| {
            s[..s.find(GEN_BEGIN).unwrap_or(0)]
                .lines()
                .filter(|l| l.contains("cortex_m_rt::"))
                .count()
        };
        for mine in [
            "use cortex_m_rt::{entry, exception, ExceptionFrame};",
            "use cortex_m_rt::entry; // reset vector",
            "use cortex_m_rt::{\n    entry,\n    exception,\n};",
        ] {
            let blocking =
                at(Runtime::Blocking)
                    .fresh_main_rs()
                    .replacen("use cortex_m_rt::entry;", mine, 1);
            // A plain regeneration keeps the header byte for byte.
            let again = at(Runtime::Blocking).update_main_rs(&blocking);
            assert_eq!(again, blocking, "{mine}");
            // Through RTIC and back: still one import.
            let rtic = at(Runtime::Rtic).update_main_rs(&blocking);
            let back = at(Runtime::Blocking).update_main_rs(&rtic);
            assert_eq!(imports(&back), 1, "{mine}\n\n{back}");
            assert!(back.contains(mine), "{mine}\n\n{back}");
        }
    }

    /// The usage examples at the bottom of every `pins/configs/*.rs` name a
    /// handle. That handle must be the one `main.rs` actually binds — including
    /// the Virtual Module's label (`_serial1_mw_radar`, not `_serial1`) — and no
    /// `{PLACEHOLDER}` may survive into the file.
    ///
    /// An example that names a variable which does not exist is worse than no
    /// example: it sends the reader debugging our documentation.
    #[test]
    fn config_examples_name_handles_that_main_rs_really_binds() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::{ApiStyle, ModuleConfig, ModuleKind};
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        // Pull `_serial1_label`-shaped identifiers out of the comment block.
        let handles_in = |file: &str| -> Vec<String> {
            let mut out = Vec::new();
            for line in file.lines().filter(|l| l.trim_start().starts_with("//")) {
                let mut rest = line;
                while let Some(i) = rest.find('_') {
                    let tail = &rest[i..];
                    let ident: String = tail
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect();
                    let is_handle = ["_serial", "_spi", "_i2c", "_can", "_tx", "_rx", "_uart"]
                        .iter()
                        .any(|p| ident.starts_with(p));
                    // Only when it is CALLED — `_i2c1.write(…)`, not prose.
                    if is_handle && tail[ident.len()..].starts_with('.') {
                        out.push(ident.clone());
                    }
                    rest = &tail[ident.len().max(1)..];
                }
            }
            out.sort();
            out.dedup();
            out
        };

        let stm32 = |family: &str, id: &str| {
            let mut def =
                crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
            def.family = family.into();
            def.id = id.into();
            def.build_mcu()
        };

        // (label, mcu, runtime, per-module api style)
        let cases: Vec<(&str, Mcu, Runtime, Option<ApiStyle>)> = vec![
            (
                "stm32f1 portable",
                stm32("stm32f1", "stm32f103c8t6"),
                Runtime::Blocking,
                Some(ApiStyle::Portable),
            ),
            (
                "stm32f1 native",
                stm32("stm32f1", "stm32f103c8t6"),
                Runtime::Blocking,
                Some(ApiStyle::Native),
            ),
            (
                "stm32f4 async",
                stm32("stm32f4", "stm32f411re"),
                Runtime::Async,
                None,
            ),
            (
                "esp blocking",
                crate::panels::mcu_module::mock_esp32c3::create_esp32c3(),
                Runtime::Blocking,
                None,
            ),
            (
                "esp async",
                crate::panels::mcu_module::mock_esp32c3::create_esp32c3(),
                Runtime::Async,
                None,
            ),
        ];

        for (what, mut mcu, runtime, style) in cases {
            mcu.runtime = runtime;
            // Wire a USART, an SPI and an I2C the way the user would, and give
            // one of them a label — the case that made `{HANDLE}` necessary.
            for kind in [
                ModuleKind::GenericInterfaceUsart,
                ModuleKind::GenericInterfaceSpi,
                ModuleKind::GenericInterfaceI2c,
            ] {
                mcu.add_module(kind);
            }
            for md in &mut mcu.modules {
                match &mut md.config {
                    ModuleConfig::Usart(c) => {
                        c.custom_label = "mw radar".into();
                        if let Some(s) = style {
                            c.api_style = s;
                        }
                    }
                    ModuleConfig::Spi(c) => {
                        if let Some(s) = style {
                            c.api_style = s;
                        }
                    }
                    ModuleConfig::I2c(c) => {
                        if let Some(s) = style {
                            c.api_style = s;
                        }
                    }
                    _ => {}
                }
            }
            let _ = PinFunction::Unset; // (kept in scope for the import above)

            let main_rs = mcu.fresh_main_rs();
            let files = mcu.config_files();
            assert!(!files.is_empty(), "{what}: wired buses produce configs");

            for (name, body) in &files {
                assert!(
                    !body.contains("{HANDLE}")
                        && !body.contains("{TX}")
                        && !body.contains("{RX}")
                        && !body.contains("{ADDR}")
                        && !body.contains("{DEVICES}"),
                    "{what}/{name}: unsubstituted placeholder:\n{body}"
                );
                for h in handles_in(body) {
                    assert!(
                        main_rs.contains(&h),
                        "{what}/{name}: the example calls `{h}`, which main.rs never binds.\n\
                         main.rs:\n{main_rs}"
                    );
                }
            }
        }
    }

    /// Every family + runtime pair that actually BUILDS an I2C bus.
    ///
    /// Shared by both I2C address guards on purpose: a family added to one and
    /// forgotten in the other is a family silently unguarded, which is how the
    /// original hole - one emitter written, four not - lasted as long as it did.
    ///
    /// Generic STM32 embassy blocking and WBA are deliberately absent. They emit
    /// no I2C constructor anywhere in the tree, so there is no bus for an address
    /// to belong to; that is a gap in those backends, not in this test.
    fn i2c_bus_cases() -> Vec<(&'static str, Mcu, Runtime)> {
        let by_id = |id: &str| {
            crate::panels::mcu_module::builtins::builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"))
                .build_mcu()
        };
        let stm32 = |family: &str, id: &str| {
            let mut def =
                crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
            def.family = family.into();
            def.id = id.into();
            def.build_mcu()
        };
        vec![
            (
                "stm32f1 blocking",
                stm32("stm32f1", "stm32f103c8t6"),
                Runtime::Blocking,
            ),
            (
                "stm32f4 async",
                stm32("stm32f4", "stm32f411re"),
                Runtime::Async,
            ),
            (
                "esp32c3 blocking",
                crate::panels::mcu_module::mock_esp32c3::create_esp32c3(),
                Runtime::Blocking,
            ),
            (
                "esp32c3 async",
                crate::panels::mcu_module::mock_esp32c3::create_esp32c3(),
                Runtime::Async,
            ),
            ("rp2040 blocking", by_id("rp2040_pico"), Runtime::Blocking),
            ("rp2040 async", by_id("rp2040_pico"), Runtime::Async),
            (
                "nrf52833 blocking",
                by_id("nrf52833_microbit_v2"),
                Runtime::Blocking,
            ),
            (
                "nrf52833 async",
                by_id("nrf52833_microbit_v2"),
                Runtime::Async,
            ),
        ]
    }

    /// main.rs and every config file, concatenated. Which of the two holds a
    /// const is the backend's business; these guards are about the value getting
    /// out at all.
    fn whole_output(mcu: &Mcu) -> String {
        let mut all = mcu.fresh_main_rs();
        for (name, body) in mcu.config_files() {
            all.push_str(&format!("// ==== {name}\n"));
            all.push_str(&body);
        }
        all
    }

    /// Several devices on ONE bus: every address reaches the generated code.
    ///
    /// A bus is shared by design and the address is the only thing telling its
    /// devices apart, so a model carrying one address per bus cannot describe a
    /// display and a sensor on the same two pads. The give-away for a half-done
    /// job is the SECOND device: the first works by accident, being exactly what
    /// the old single-address field already emitted.
    #[test]
    fn several_devices_on_one_bus_all_reach_the_generated_code() {
        use crate::panels::mcu_module::modules::{I2cDevice, ModuleConfig, ModuleKind};

        for (what, mut mcu, runtime) in i2c_bus_cases() {
            mcu.runtime = runtime;
            mcu.add_module(ModuleKind::GenericInterfaceI2c);
            for md in &mut mcu.modules {
                if let ModuleConfig::I2c(c) = &mut md.config {
                    c.devices = vec![
                        I2cDevice {
                            name: "display".into(),
                            address: 0x3C,
                            ..Default::default()
                        },
                        I2cDevice {
                            name: "imu".into(),
                            address: 0x68,
                            ..Default::default()
                        },
                    ];
                }
            }
            let all = whole_output(&mcu);
            assert!(
                all.contains("0x3C;"),
                "{what}: the first device's address is missing:\n{all}"
            );
            assert!(
                all.contains("0x68;"),
                "{what}: the SECOND device's address never reaches the generated code:\n{all}"
            );
        }
    }

    /// The 7-bit address the panel collects reaches the generated code on
    /// EVERY family and runtime that builds an I2C bus.
    ///
    /// It used to reach exactly ONE of them. `I2cModuleConfig.address` had a
    /// single non-UI reader in the whole tree - the ESP config file - so on
    /// every other family the user set an address, saved, and nothing in the
    /// output mentioned it again. The Address row was not gated and gave no
    /// reason, which is this project's rule backwards: the UI offered what
    /// codegen would not write.
    ///
    /// Where it lands differs, and has to. A family with a `pins/configs/`
    /// file puts it there, beside the clock; the runtimes that build every bus
    /// inline in `main.rs` have no such file, so it goes to module scope above
    /// the entry point - which is also why the name carries the peripheral
    /// there and not in a config file.
    #[test]
    fn the_i2c_address_reaches_every_family_that_builds_a_bus() {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        for (what, mut mcu, runtime) in i2c_bus_cases() {
            mcu.runtime = runtime;
            mcu.add_module(ModuleKind::GenericInterfaceI2c);
            let mut wired = false;
            for md in &mut mcu.modules {
                if let ModuleConfig::I2c(c) = &mut md.config {
                    c.address = 0x3C;
                    wired = true;
                }
            }
            assert!(wired, "{what}: no I2C module to configure");

            let all = whole_output(&mcu);
            assert!(
                all.contains("DEVICE_ADDRESS: u8 = 0x3C;"),
                "{what}: the configured address never reaches the generated code:\n{all}"
            );
        }
    }

    /// `files` (a `src/pins/` tree) is a module tree rustc can read: no
    /// `pub mod` names a path, no `X.rs` sits beside an `X/mod.rs`, every
    /// `pub mod` has its file, and every file is declared by exactly one chain
    /// from `pins/mod.rs`.
    fn assert_module_tree(what: &str, files: &[(String, String)]) {
        let has = |p: &str| files.iter().any(|(q, _)| q == p);
        let mut declared: Vec<String> = Vec::new();
        let mut queue = vec!["src/pins/mod.rs".to_owned()];
        while let Some(file) = queue.pop() {
            let body = &files
                .iter()
                .find(|(p, _)| *p == file)
                .unwrap_or_else(|| panic!("{what}: {file} is declared but not there"))
                .1;
            declared.push(file.clone());
            let dir = file.strip_suffix("mod.rs").expect("only mod.rs declares");
            for name in body
                .lines()
                .filter_map(|l| l.trim().strip_prefix("pub mod "))
                .filter_map(|l| l.strip_suffix(';'))
            {
                assert!(!name.contains('/'), "{what}: `pub mod {name};` in {file}");
                let flat = format!("{dir}{name}.rs");
                let folder = format!("{dir}{name}/mod.rs");
                assert!(
                    has(&flat) != has(&folder),
                    "{what}: `{name}` in {file} is neither or both of {flat} / {folder}"
                );
                if has(&folder) {
                    queue.push(folder);
                } else {
                    declared.push(flat);
                }
            }
        }
        for (path, _) in files {
            assert_eq!(
                declared.iter().filter(|d| *d == path).count(),
                1,
                "{what}: {path} is not declared exactly once:\n{declared:#?}"
            );
        }
    }

    /// The `src/pins/` tree a project with I2C devices gets is a module tree
    /// rustc can read, on every family that builds a bus - and still is after
    /// a device is renamed and the SAME tree is synced again, with the code
    /// the user wrote below the device file's markers moved along. The emit
    /// harnesses compile a fresh tree for real, behind `--ignored`; this runs
    /// on every `cargo test`, and is the one place the real emitters' output
    /// goes through a rename.
    #[test]
    fn the_pin_tree_with_i2c_devices_is_a_module_tree() {
        use crate::panels::mcu_module::modules::{I2cDeviceEdit, ModuleConfig, ModuleKind};
        use crate::project_tree::logic::ProjectTreeState;

        for (what, mut mcu, runtime) in i2c_bus_cases() {
            mcu.runtime = runtime;
            mcu.add_module(ModuleKind::GenericInterfaceI2c);
            assert!(mcu.with_i2c_devices(&[("oled", 0x3C), ("", 0x68)]));
            let files = mcu.pin_tree_files();
            assert_module_tree(what, &files);
            // RP and nRF build their buses inline on Async: no config files,
            // the addresses are consts in main.rs.
            let inline =
                what.ends_with("async") && (what.starts_with("rp") || what.starts_with("nrf"));
            let folder = files.iter().any(|(p, _)| {
                p.ends_with("/mod.rs") && (p.contains("configs/i2c") || p.contains("configs/twim"))
            });
            assert_eq!(
                folder,
                !inline,
                "{what}: {:?}",
                files.iter().map(|(p, _)| p).collect::<Vec<_>>()
            );
            if !folder {
                continue;
            }
            assert_eq!(
                files.iter().filter(|(p, _)| p.contains("/device")).count(),
                2,
                "{what}: two devices, two files: {:?}",
                files.iter().map(|(p, _)| p).collect::<Vec<_>>()
            );

            // One tree, synced the way the app does it, before and after a
            // rename made through the panel's own write path.
            let mut tree = ProjectTreeState::new();
            let sync = |tree: &mut ProjectTreeState, mcu: &Mcu| {
                tree.sync_config_files(&mcu.config_files(), false, &[], &mut None);
                tree.sync_pin_files(&mcu.all_pin_functions());
            };
            sync(&mut tree, &mcu);
            tree.user_src_files
                .iter_mut()
                .find(|(p, _)| p.ends_with("/device1_oled.rs"))
                .unwrap_or_else(|| panic!("{what}: no device1_oled.rs"))
                .1
                .push_str("\n// MINE-OLED\n");
            let (instance, key) = mcu
                .modules
                .iter()
                .find_map(|m| match &m.config {
                    ModuleConfig::I2c(c) => Some((m.instance(), c.rows()[0].key)),
                    _ => None,
                })
                .expect("the I2C bus");
            assert!(mcu.edit_i2c_device(instance, I2cDeviceEdit::Name(key, "display".into())));
            sync(&mut tree, &mcu);
            assert_module_tree(what, &tree.user_src_files);
            let renamed = tree
                .user_src_files
                .iter()
                .find(|(p, _)| p.ends_with("/device1_display.rs"))
                .unwrap_or_else(|| panic!("{what}: no device1_display.rs"));
            assert!(renamed.1.contains("// MINE-OLED"), "{what}:\n{}", renamed.1);
            assert!(
                !tree
                    .user_src_files
                    .iter()
                    .any(|(p, _)| p.ends_with("/device1_oled.rs")),
                "{what}: the old name stayed"
            );
        }
    }

    /// The RP and nRF async runtimes keep each device's address const in
    /// main.rs under the name it had before buses were folders: the user's
    /// code below the markers names them.
    #[test]
    fn the_async_device_consts_keep_their_names() {
        use crate::panels::mcu_module::modules::ModuleKind;

        for (what, mut mcu, runtime) in i2c_bus_cases() {
            let peri = match what {
                w if w.starts_with("rp") => "I2C",
                w if w.starts_with("nrf") => "TWIM",
                _ => continue,
            };
            if runtime != Runtime::Async {
                continue;
            }
            mcu.runtime = runtime;
            mcu.add_module(ModuleKind::GenericInterfaceI2c);
            let consts = |main: &str| -> Vec<String> {
                main.lines()
                    .map(str::trim)
                    .filter(|l| l.starts_with(&format!("pub const {peri}")))
                    .map(str::to_owned)
                    .collect()
            };
            assert!(mcu.with_i2c_devices(&[("oled", 0x3C)]));
            let one = consts(&mcu.fresh_main_rs());
            assert!(
                one.iter()
                    .any(|l| l.ends_with("_DEVICE_ADDRESS: u8 = 0x3C;") && !l.contains("OLED")),
                "{what}: one device keeps the bus's name: {one:?}"
            );
            assert!(mcu.with_i2c_devices(&[("", 0x68)]));
            let two = consts(&mcu.fresh_main_rs());
            assert!(
                two.iter()
                    .any(|l| l.ends_with("_OLED_DEVICE_ADDRESS: u8 = 0x3C;")),
                "{what}: {two:?}"
            );
            assert!(
                two.iter()
                    .any(|l| l.ends_with("_DEVICE2_DEVICE_ADDRESS: u8 = 0x68;")),
                "{what}: {two:?}"
            );
        }
    }

    /// Every pin the ESP backend WRITES must parse back out of the file it
    /// wrote. This is the loop a project actually goes through on reload —
    /// `apply_saved_pins` wipes the diagram and re-applies only what
    /// [`parse_main_rs`] returns — so a pin that does not round-trip comes back
    /// Unset and takes its Virtual Module's wiring with it.
    ///
    /// Regression: moving the bus labels onto their own line (to stop a trailing
    /// comment swallowing the chain's `;`) left the `.with_xxx(...)` lines
    /// unlabelled, and USART/SPI/I2C pins silently stopped surviving a restart —
    /// 1 of 5 pins came back.
    ///
    /// [`parse_main_rs`]: super::super::parse_main_rs
    #[test]
    fn every_esp_pin_survives_a_generate_parse_round_trip() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        use std::collections::BTreeMap;

        // One of everything the ESP backend can emit a pin for.
        let plan = [
            ("GPIO2", PinFunction::GpioOutput),
            ("GPIO3", PinFunction::GpioInput),
            ("GPIO21", PinFunction::UsartTx(0)),
            ("GPIO20", PinFunction::UsartRx(0)),
            ("GPIO4", PinFunction::SpiSck(2)),
            ("GPIO5", PinFunction::SpiMosi(2)),
            ("GPIO6", PinFunction::SpiMiso(2)),
            ("GPIO7", PinFunction::SpiNss(2)),
            ("GPIO8", PinFunction::I2cScl(0)),
            ("GPIO9", PinFunction::I2cSda(0)),
        ];

        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mut mcu = crate::panels::mcu_module::mock_esp32c3::create_esp32c3();
            mcu.runtime = runtime;
            for (name, func) in &plan {
                let pin = mcu
                    .iter_all_pins_mut()
                    .find(|p| p.name == *name)
                    .unwrap_or_else(|| panic!("{name} exists"));
                pin.selected_function = func.clone();
            }
            let code = mcu.fresh_main_rs();
            let parsed: BTreeMap<String, PinFunction> =
                super::super::parse_main_rs(&code).into_iter().collect();

            for (name, func) in &plan {
                assert_eq!(
                    parsed.get(*name),
                    Some(func),
                    "{runtime:?}: {name} did not round-trip\n{code}"
                );
            }
            assert_eq!(parsed.len(), plan.len(), "{runtime:?}: {parsed:?}");

            // ...and re-applying the parse must reproduce the same file, which
            // is what makes a reopened project regenerate identical code.
            let mut reloaded = crate::panels::mcu_module::mock_esp32c3::create_esp32c3();
            reloaded.runtime = runtime;
            reloaded.apply_saved_pins(&super::super::parse_main_rs(&code));
            assert_eq!(
                reloaded.fresh_main_rs(),
                code,
                "{runtime:?}: reload changed the generated file"
            );
        }
    }

    /// A label that trails the call — the shape older projects have on disk —
    /// still parses, so reopening one written before the layout change restores
    /// its pins too.
    #[test]
    fn legacy_trailing_bus_labels_still_parse() {
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let legacy = format!(
            "{}\n\
             #[esp_hal::main]\n\
             fn main() -> ! {{\n\
             \x20   let mut _uart0 = Uart::new(peripherals.UART0, UartConfig::default())\n\
             \x20       .with_rx(peripherals.GPIO20) // USART0  RX\n\
             \x20       .with_tx(peripherals.GPIO21) // USART0  TX;\n\
             {}\n",
            super::super::GEN_BEGIN,
            super::super::GEN_END,
        );
        let parsed = super::super::parse_main_rs(&legacy);
        assert_eq!(
            parsed,
            vec![
                ("GPIO20".to_owned(), PinFunction::UsartRx(0)),
                ("GPIO21".to_owned(), PinFunction::UsartTx(0)),
            ],
            "legacy trailing labels:\n{legacy}"
        );
    }

    /// A section header must NOT leak onto a later builder line — the label
    /// applies to the line immediately below it and nothing else.
    #[test]
    fn section_headers_do_not_label_pins() {
        let src = format!(
            "{}\n\
             \x20   // ── I2C0 ──\n\
             \x20   let mut _i2c0 = I2c::new(peripherals.I2C0, I2cConfig::default())\n\
             \x20       .unwrap()\n\
             \x20       .with_scl(peripherals.GPIO8)\n\
             {}\n",
            super::super::GEN_BEGIN,
            super::super::GEN_END,
        );
        assert!(
            super::super::parse_main_rs(&src).is_empty(),
            "an unlabelled pin must stay unparsed, not inherit the header"
        );
    }

    /// Switching the ESP runtime re-splices ONLY the generated block, both ways,
    /// leaving the user's loop body untouched — the whole entry-point difference
    /// lives inside the markers, so the invariant header never moves.
    #[test]
    fn esp_runtime_switch_reslices_and_keeps_user_tail() {
        use crate::panels::mcu_module::mcu::Runtime;
        const MARK: &str = "        my_own_call(); // USER";
        let mut mcu = crate::panels::mcu_module::mock_esp32c3::create_esp32c3();

        let blocking = mcu.fresh_main_rs().replace(
            "        // Your main loop code here.",
            &format!("        // Your main loop code here.\n{MARK}"),
        );
        assert!(blocking.contains(MARK), "test fixture wrote the user line");

        mcu.runtime = Runtime::Async;
        let to_async = mcu.update_main_rs(&blocking);
        assert!(to_async.contains("#[esp_rtos::main]"), "{to_async}");
        assert!(!to_async.contains("#[esp_hal::main]"), "{to_async}");
        assert!(to_async.contains(MARK), "user tail survives:\n{to_async}");

        mcu.runtime = Runtime::Blocking;
        let back = mcu.update_main_rs(&to_async);
        assert!(back.contains("#[esp_hal::main]"), "{back}");
        assert!(!back.contains("esp_rtos"), "scheduler gone:\n{back}");
        assert!(!back.contains("Spawner"), "async import gone:\n{back}");
        assert!(
            back.contains(MARK),
            "user tail survives the way back:\n{back}"
        );
        // Round-trip is stable — a switch there and back is not a rewrite.
        assert_eq!(back, blocking, "blocking -> async -> blocking is identity");
    }

    /// The Configuration tab's DMA card is fed from the ESP plan too.
    ///
    /// `dma_uses` had two arms — embassy-async and F1-blocking — and an ESP fell
    /// through both to `Vec::new()`, so the card could only ever say "no bus is
    /// on DMA" no matter what the project did. It has four arms now, and the ESP
    /// one is keyed on the FAMILY ahead of them, which is the point this test
    /// pins: on ESP, DMA is NOT an async feature.
    ///
    /// This used to be `#[ignore]`d with its last assertion inverted — it still
    /// claimed "the blocking project takes none", which was the pre-fix rule.
    /// Ignoring it hid a test that contradicted the shipping code, so the
    /// assertion was corrected and the attribute removed rather than left to rot.
    #[test]
    fn an_esp_reports_its_dma_to_the_configuration_card() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::{
            AsyncBusMode, ModuleConfig, ModuleKind, SpiModuleConfig, VirtualModule,
        };
        let def = crate::panels::mcu_module::builtins::builtin_for("esp32c5").expect("c5");
        let mut mcu = def.build_mcu();
        mcu.runtime = Runtime::Async;
        assert!(dma_uses(&mcu).is_empty(), "nothing asked for DMA yet");

        let mut cfg = SpiModuleConfig::new(2);
        cfg.async_mode = AsyncBusMode::AsyncDma;
        mcu.modules.push(VirtualModule {
            id: "spi_2".into(),
            kind: ModuleKind::GenericInterfaceSpi,
            name: "SPI2".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Spi(cfg),
            connections: Vec::new(),
        });
        let uses = dma_uses(&mcu);
        assert_eq!(uses.len(), 1);
        assert_eq!(uses[0].peri, "DMA_CH0");
        assert_eq!(uses[0].user, "SPI2");

        // …and it is NOT an async-runtime feature on ESP: `with_dma` sits on
        // esp-hal's BLOCKING drivers, so the blocking project takes the very
        // same channel. That is why the ESP arm is keyed on the family and has
        // to come BEFORE every runtime arm — keyed on the runtime instead, this
        // is the case that reports nothing.
        mcu.runtime = Runtime::Blocking;
        let blocking = dma_uses(&mcu);
        assert_eq!(
            blocking, uses,
            "an ESP reports the same channel on either runtime"
        );
    }

    /// The same, for an ESP chip whose SPI is on the GDMA.
    ///
    /// The DMA path changes the config file's RETURN TYPE (`SpiDmaBus`), adds
    /// two owned descriptor buffers and takes an extra `init_async` argument
    /// from main.rs. None of that can be checked by reading the string: it
    /// either compiles against esp-hal or it does not.
    ///
    /// ```text
    /// ESP_DMA_OUT=/some/dir EIDE_ESP_CHIP=esp32c5 \
    ///     cargo test write_esp_dma_project -- --ignored
    /// cd /some/dir && cargo build --release
    /// ```
    ///
    /// The chip defaults to the ESP32-C5, the part this was written for; any
    /// GDMA part works, and the pads are chosen by SEARCH because the numbering
    /// differs between them.
    #[test]
    #[ignore]
    fn write_esp_dma_project() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::modules::{
            AsyncBusMode, I2sModuleConfig, ModuleConfig, ModuleKind, SpiModuleConfig, VirtualModule,
        };
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        use crate::panels::mcu_module::project_gen::{self, AsyncFlavor, ConfigFile};
        use std::fs;
        use std::path::PathBuf;

        let out = PathBuf::from(
            std::env::var("ESP_DMA_OUT").expect("set ESP_DMA_OUT to the target folder"),
        );
        let id = std::env::var("EIDE_ESP_CHIP").unwrap_or_else(|_| "esp32c5".into());
        let def =
            crate::panels::mcu_module::builtins::builtin_for(&id).expect("a bundled ESP chip");
        let mut mcu = def.build_mcu();
        // `ESP_ASYNC_RUNTIME=blocking` writes the blocking project instead. It
        // is not the same project with fewer `await`s: a blocking one still has
        // to serve the peripherals that take a DMA channel in their only
        // constructor, so this A/B is the one that covers them.
        mcu.runtime = if std::env::var("ESP_ASYNC_RUNTIME").as_deref() == Ok("blocking") {
            Runtime::Blocking
        } else {
            Runtime::Async
        };

        // All three watchdogs, so the RWDT's `Rtc` and both MWDT files land in
        // the same project. `ESP_WDG=0` leaves them out.
        if std::env::var("ESP_WDG").as_deref() != Ok("0") {
            use crate::panels::mcu_module::watchdog::EspWdtConfig;
            mcu.watchdog.rwdt = Some(EspWdtConfig {
                timeout_us: 2_000_000,
            });
            mcu.watchdog.mwdt0 = Some(EspWdtConfig {
                timeout_us: 1_500_000,
            });
            if crate::panels::mcu_module::watchdog::esp_limits_for(&id).has_mwdt1 {
                mcu.watchdog.mwdt1 = Some(EspWdtConfig {
                    timeout_us: 500_000,
                });
            }
        }

        let rx_chan: u8 = if id == "esp32s3" { 4 } else { 2 };

        // `ESP_LCD_HALF=lcd|cam` wires ONE half of the video port instead of
        // both — the single-half files, which are the common case. A half is
        // present because its PADS are, so that is what this changes.
        let lcd_half = std::env::var("ESP_LCD_HALF").unwrap_or_default();

        // `ESP_PARL_HALF=tx|rx` wires ONE half of the parallel port instead of
        // both. Same reasoning as the video port: a half is present because its
        // pads are.
        let parl_half = std::env::var("ESP_PARL_HALF").unwrap_or_default();

        // By search, not by name: GPIO4 is a JTAG pad on one part and a plain
        // pad on the next, and the pin NUMBERS differ between packages.
        let mut want = vec![
            // A UART, so the direction and flow-control shapes are covered:
            // `ESP_UART_MODE=tx|rx` builds a single direction, `=flow` adds
            // RTS/CTS to the two-way port.
            PinFunction::UsartTx(1),
            PinFunction::UsartRx(1),
            PinFunction::UsartCts(1),
            PinFunction::UsartRts(1),
            PinFunction::SpiSck(2),
            PinFunction::SpiMosi(2),
            PinFunction::SpiMiso(2),
            // The I2S goes on the same project on purpose: it and the SPI draw
            // from one pool of channels, so this also proves they do not both
            // get handed DMA_CH0.
            PinFunction::I2sCk(0),
            PinFunction::I2sWs(0),
            PinFunction::I2sSd(0),
            PinFunction::I2sMck(0),
            // Channel 0 transmits on every part that has an RMT at all.
            PinFunction::RmtChannel(0),
            // …and a receiving one, which is a different config type, a
            // different pad bound and a different `configure_*` call. WHICH
            // channel receives differs: the S3 splits 0-3 TX / 4-7 RX, the
            // RISC-V parts split 0-1 / 2-3, and the esp32 and S2 let any
            // channel go either way. Asking for a channel the panel would
            // refuse tests this harness, not the generator.
            PinFunction::RmtChannel(rx_chan),
            // A PCNT unit with BOTH pads: the encoder shape, which is the one
            // that emits `set_ctrl_signal` and `set_ctrl_mode`.
            PinFunction::PcntEdge {
                unit: 0,
                channel: 0,
            },
            PinFunction::PcntCtrl {
                unit: 0,
                channel: 0,
            },
            // …and the unit's SECOND channel, which is the other half of a
            // quadrature encoder and its own pair of pads.
            PinFunction::PcntEdge {
                unit: 0,
                channel: 1,
            },
            PinFunction::PcntCtrl {
                unit: 0,
                channel: 1,
            },
            // The chip's own USB serial port: no pins are passed to it, but the
            // two pads are what makes it appear at all.
            PinFunction::UsbDm,
            PinFunction::UsbDp,
            // One MCPWM operator with BOTH outputs: the complementary pair, and
            // the shape whose return type is longest. A SECOND operator joins
            // it, so `ESP_MCPWM_TIMERS=split` can put the two on two timers —
            // two motors at two frequencies on one unit.
            PinFunction::McpwmA {
                unit: 0,
                operator: 1,
            },
            PinFunction::McpwmA {
                unit: 0,
                operator: 0,
            },
            PinFunction::McpwmB {
                unit: 0,
                operator: 0,
            },
            // A four-bit parallel port with its clock and a valid pad: the
            // shape that exercises the width table, the clock direction and
            // the `TxPinConfigWithValidPin` wrapper all at once.
            PinFunction::ParlData { lane: 0 },
            PinFunction::ParlData { lane: 1 },
            PinFunction::ParlData { lane: 2 },
            PinFunction::ParlData { lane: 3 },
            PinFunction::ParlClk,
            PinFunction::ParlValid,
            // …and the RECEIVING half's own pads, so the project exercises the
            // two halves running at once off one DMA channel.
            PinFunction::ParlRxData { lane: 0 },
            PinFunction::ParlRxData { lane: 1 },
            PinFunction::ParlRxData { lane: 2 },
            PinFunction::ParlRxData { lane: 3 },
            PinFunction::ParlRxClk,
            // Both TWAI pads. A node with one wire emits nothing, so the pair
            // is what proves the section at all.
            PinFunction::CanRx,
            PinFunction::CanTx,
            PinFunction::TouchPad(0),
            PinFunction::TouchPad(5),
            PinFunction::TouchPad(9),
            // The video port, in whichever mode `ESP_LCD_MODE` names. The S3
            // is the only part with the pads, so everywhere else these are
            // reported skipped and the project is the same as before.
            PinFunction::LcdCamData { lane: 0 },
            PinFunction::LcdCamData { lane: 1 },
            PinFunction::LcdCamData { lane: 2 },
            PinFunction::LcdCamData { lane: 3 },
            PinFunction::LcdCamData { lane: 4 },
            PinFunction::LcdCamData { lane: 5 },
            PinFunction::LcdCamData { lane: 6 },
            PinFunction::LcdCamData { lane: 7 },
            // Every control pad of all three modes: the ones a mode does not
            // name are simply never passed, and wiring them anyway proves it.
            PinFunction::LcdCamDc,
            PinFunction::LcdCamWr,
            PinFunction::LcdCamCs,
            PinFunction::LcdCamPclk,
            PinFunction::LcdCamVsync,
            PinFunction::LcdCamHsync,
            PinFunction::LcdCamDe,
            // …and the CAMERA half's own pads, so the project exercises the two
            // halves running at once — the thing the block exists for.
            PinFunction::CamData { lane: 0 },
            PinFunction::CamData { lane: 1 },
            PinFunction::CamData { lane: 2 },
            PinFunction::CamData { lane: 3 },
            PinFunction::CamData { lane: 4 },
            PinFunction::CamData { lane: 5 },
            PinFunction::CamData { lane: 6 },
            PinFunction::CamData { lane: 7 },
            PinFunction::CamPclk,
            PinFunction::CamVsync,
            PinFunction::CamHsync,
            PinFunction::CamHenable,
            PinFunction::CamMclk,
            // Both DAC channels. Their pads are fixed, so the search below
            // finds them wherever the chip puts them.
            PinFunction::DacOut { dac: 1, channel: 1 },
            PinFunction::DacOut { dac: 1, channel: 2 },
        ];
        want.retain(|f| match parl_half.as_str() {
            "tx" => !matches!(
                f,
                PinFunction::ParlRxData { .. } | PinFunction::ParlRxClk | PinFunction::ParlRxValid
            ),
            "rx" => !matches!(
                f,
                PinFunction::ParlData { .. } | PinFunction::ParlClk | PinFunction::ParlValid
            ),
            _ => true,
        });
        want.retain(|f| match lcd_half.as_str() {
            "lcd" => !matches!(
                f,
                PinFunction::CamData { .. }
                    | PinFunction::CamPclk
                    | PinFunction::CamVsync
                    | PinFunction::CamHsync
                    | PinFunction::CamHenable
                    | PinFunction::CamMclk
            ),
            "cam" => !matches!(
                f,
                PinFunction::LcdCamData { .. }
                    | PinFunction::LcdCamDc
                    | PinFunction::LcdCamWr
                    | PinFunction::LcdCamCs
                    | PinFunction::LcdCamPclk
                    | PinFunction::LcdCamVsync
                    | PinFunction::LcdCamHsync
                    | PinFunction::LcdCamDe
            ),
            _ => true,
        });
        // RAREST FIRST, and the DAC is why. On an ESP the GPIO matrix lets
        // almost any pad be SPI, I2S, RMT or MCPWM, so walking the pins and
        // taking the first function each can do hands those pads out in pin
        // order — and by the time `DacOut` came round, GPIO25 and GPIO26 were
        // already spent on something that could have gone anywhere. The DAC,
        // the ADC and USB are bonded to one pad each and have no second choice.
        while !want.is_empty() {
            let candidates = |f: &PinFunction| {
                mcu.iter_all_pins()
                    .filter(|p| {
                        !p.reserved
                            && p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(f)
                    })
                    .count()
            };
            let Some((ix, _)) = want
                .iter()
                .map(candidates)
                .enumerate()
                .filter(|(_, n)| *n > 0)
                .min_by_key(|(_, n)| *n)
            else {
                break;
            };
            let func = want.remove(ix);
            let pin = mcu
                .iter_all_pins_mut()
                .find(|p| {
                    !p.reserved
                        && p.selected_function == PinFunction::Unset
                        && p.available_functions.contains(&func)
                })
                .expect("just counted one");
            pin.selected_function = func;
        }
        // Not every chip has every peripheral, and that is the POINT of running
        // this against several: an ESP32 has no USB Serial/JTAG and no PARL_IO,
        // so those simply do not get wired. Reported rather than asserted, so
        // the harness works on any part.
        if !want.is_empty() {
            println!("{id}: not offered, skipped - {want:?}");
        }

        let mut spi_cfg = SpiModuleConfig::new(2);
        spi_cfg.async_mode = AsyncBusMode::AsyncDma;
        // `ESP_SPI_SLAVE=1` builds the other end of the bus instead: a
        // different driver module, reversed pin bounds, no frequency, and DMA
        // whether or not the async switch is on.
        // `ESP_SPI_CHAN=DMA_CH2` pins the channel by hand, the way the module
        // panel's picker now can — reserved before anything else is allocated.
        if let Ok(c) = std::env::var("ESP_SPI_CHAN") {
            spi_cfg.dma_tx = c;
        }
        if std::env::var("ESP_SPI_SLAVE").is_ok() {
            spi_cfg.role = crate::panels::mcu_module::modules::SpiRole::Slave;
        }
        mcu.modules.push(VirtualModule {
            id: "spi_2".into(),
            kind: ModuleKind::GenericInterfaceSpi,
            name: "SPI2".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Spi(spi_cfg),
            connections: Vec::new(),
        });
        // `ESP_TWAI_LISTEN=1` builds the listen-only, no-transceiver variant:
        // the other constructor and the other `TwaiMode`.
        mcu.modules.push(VirtualModule {
            id: "can_1".into(),
            kind: ModuleKind::GenericInterfaceCan,
            name: "CAN".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Can({
                use crate::panels::mcu_module::modules::{CanMode, CanModuleConfig};
                let mut c = CanModuleConfig::new(1);
                c.bitrate = 250_000;
                if std::env::var("ESP_TWAI_LISTEN").is_ok() {
                    c.mode = CanMode::ListenOnly;
                    c.transceiver = false;
                }
                c
            }),
            connections: Vec::new(),
        });
        // Three touch pads. Only the original ESP32 has them, so everywhere
        // else they are reported skipped. `ESP_TOUCH=continuous` picks the
        // other scan mode, which is also the only one with an async twin.
        mcu.modules.push(VirtualModule {
            id: "touch".into(),
            kind: ModuleKind::GenericInterfaceTouch,
            name: "TOUCH".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Touch({
                use crate::panels::mcu_module::modules::{TouchModuleConfig, TouchScan};
                let mut c = TouchModuleConfig::new(0);
                if std::env::var("ESP_TOUCH").as_deref() == Ok("continuous") {
                    c.scan = TouchScan::Continuous;
                }
                c
            }),
            connections: Vec::new(),
        });
        // `ESP_USB_OTG=1` routes the USB pads to the OTG controller instead of
        // the built-in console: a different peripheral, two extra crates, and a
        // device built in `main.rs` rather than behind one `init`.
        mcu.modules.push(VirtualModule {
            id: "usb_1".into(),
            kind: ModuleKind::GenericInterfaceUsb,
            name: "USB".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usb({
                use crate::panels::mcu_module::modules::{UsbModuleConfig, UsbRole};
                let mut c = UsbModuleConfig::new(1);
                if std::env::var("ESP_USB_OTG").is_ok() {
                    c.role = UsbRole::Otg;
                    c.product = "IDE test device".into();
                }
                c
            }),
            connections: Vec::new(),
        });
        // `ESP_LCD_MODE=dpi` picks the RGB display instead of the i8080 one.
        // The camera below is a SECOND module on the other half, and both are
        // built by one `init` — see `lcd_cam_file`.
        mcu.modules.push(VirtualModule {
            id: "lcd_cam".into(),
            kind: ModuleKind::GenericInterfaceLcdCam,
            name: "LCD".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::LcdCam({
                use crate::panels::mcu_module::modules::{LcdCamMode, LcdCamModuleConfig};
                let mut c = LcdCamModuleConfig::new(0);
                c.mode = match std::env::var("ESP_LCD_MODE").as_deref() {
                    Ok("dpi") => LcdCamMode::Dpi,
                    _ => LcdCamMode::I8080,
                };
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "usart_1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            name: "UART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart({
                use crate::panels::mcu_module::modules::{
                    UsartDirection, UsartFlow, UsartModuleConfig,
                };
                let mut c = UsartModuleConfig::new(1);
                match std::env::var("ESP_UART_MODE").as_deref() {
                    Ok("tx") => {
                        c.direction = UsartDirection::TxOnly;
                        c.flow = UsartFlow::Rts;
                    }
                    Ok("rx") => {
                        c.direction = UsartDirection::RxOnly;
                        c.flow = UsartFlow::Cts;
                    }
                    Ok("flow") => c.flow = UsartFlow::CtsRts,
                    _ => {}
                }
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "parl_rx".into(),
            kind: ModuleKind::GenericInterfaceParlIoRx,
            name: "PARL RX".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::ParlIo({
                use crate::panels::mcu_module::modules::{ParlIoModuleConfig, ParlIoWidth};
                let mut c = ParlIoModuleConfig::new_rx();
                c.width = ParlIoWidth::Four;
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "camera".into(),
            kind: ModuleKind::GenericInterfaceCamera,
            name: "CAM".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::LcdCam({
                use crate::panels::mcu_module::modules::LcdCamModuleConfig;
                LcdCamModuleConfig::new_camera()
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "dac_1".into(),
            kind: ModuleKind::GenericInterfaceDac,
            name: "DAC1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Dac({
                use crate::panels::mcu_module::modules::DacModuleConfig;
                let mut c = DacModuleConfig::new(1);
                // Mid-scale in the module's TWELVE bits, which the ESP path
                // has to scale down to 128 rather than truncate to 0.
                c.set_value(1, 2048);
                c.set_value(2, 4095);
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "parl_io".into(),
            kind: ModuleKind::GenericInterfaceParlIo,
            name: "PARL".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::ParlIo({
                use crate::panels::mcu_module::modules::{ParlIoModuleConfig, ParlIoWidth};
                let mut c = ParlIoModuleConfig::new(0);
                c.width = ParlIoWidth::Four;
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "mcpwm_0".into(),
            kind: ModuleKind::GenericInterfaceMcpwm,
            name: "MCPWM0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Mcpwm({
                let mut c = crate::panels::mcu_module::modules::McpwmModuleConfig::new(0);
                c.duty_x100.insert((0, false), 2_500);
                c.duty_x100.insert((0, true), 7_500);
                c.duty_x100.insert((1, false), 5_000);
                // `ESP_MCPWM_TIMERS=split` puts operator 1 on its own timer at
                // its own frequency — two timers started, two periods, two
                // handles back.
                if std::env::var("ESP_MCPWM_TIMERS").as_deref() == Ok("split") {
                    c.op_timer[1] = 1;
                    c.extra_timers[0] = (1_000, 999);
                }
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "pcnt_0".into(),
            kind: ModuleKind::GenericInterfacePcnt,
            name: "PCNT0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Pcnt({
                use crate::panels::mcu_module::modules::{
                    PcntCtrlMode, PcntEdgeMode, PcntModuleConfig,
                };
                let mut c = PcntModuleConfig::new(0);
                // Both edges, direction from the control input, and a filter —
                // every optional line the generator can emit.
                c.neg_edge = PcntEdgeMode::Decrement;
                c.ctrl_high = PcntCtrlMode::Reverse;
                c.filter = 100;
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: format!("rmt_{rx_chan}"),
            kind: ModuleKind::GenericInterfaceRmt,
            name: format!("RMT{rx_chan}"),
            pos: (0.0, 0.0),
            config: ModuleConfig::Rmt({
                let mut c = crate::panels::mcu_module::modules::RmtModuleConfig::new(rx_chan);
                c.direction = crate::panels::mcu_module::modules::RmtDirection::Receive;
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "rmt_0".into(),
            kind: ModuleKind::GenericInterfaceRmt,
            name: "RMT0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Rmt({
                let mut c = crate::panels::mcu_module::modules::RmtModuleConfig::new(0);
                // A carrier is the shape with the most arithmetic behind it, so
                // it is the one worth compiling.
                c.carrier = true;
                c
            }),
            connections: Vec::new(),
        });
        mcu.modules.push(VirtualModule {
            id: "i2s_0".into(),
            kind: ModuleKind::GenericInterfaceI2s,
            name: "I2S0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::I2s({
                let mut c = I2sModuleConfig::new(0);
                // `ESP_I2S_RX=1` builds the receiving half instead: a different
                // return type, a different pad bound and a different import.
                if std::env::var("ESP_I2S_RX").is_ok() {
                    c.direction = crate::panels::mcu_module::modules::I2sDirection::Receive;
                }
                c
            }),
            connections: Vec::new(),
        });

        let main_rs = crate::panels::mcu_module::codegen::family::backend_for_runtime(
            &mcu.family,
            mcu.runtime,
        )
        .expect("an ESP backend")
        .fresh_main_rs(&mcu);

        let cargo_toml =
            project_gen::gen_config(ConfigFile::CargoToml, &def.project, &def.toolchain);
        let cargo_toml = project_gen::ensure_async_deps(
            &cargo_toml,
            true,
            AsyncFlavor::Esp(&def.project.probe_chip),
            false,
            false,
            false,
            &[],
        );
        fs::create_dir_all(out.join(".cargo")).unwrap();
        fs::write(out.join("Cargo.toml"), cargo_toml).unwrap();
        fs::write(
            out.join(".cargo/config.toml"),
            project_gen::gen_config(ConfigFile::CargoConfig, &def.project, &def.toolchain),
        )
        .unwrap();
        // Xtensa parts need the pinned fork, and a real project gets one — see
        // `project_gen::rust_toolchain_for`. Without it an ESP32 or S2 here
        // fails inside `core` with an error that names no toolchain.
        let toolchain = project_gen::rust_toolchain_for(&def.project.target);
        if !toolchain.is_empty() {
            fs::write(out.join("rust-toolchain.toml"), toolchain).unwrap();
        }
        fs::write(out.join("src/main.rs"), &main_rs).unwrap();
        write_pin_tree(&out, &mcu);
        assert!(
            main_rs.contains("peripherals.DMA_"),
            "main.rs passes no DMA channel:\n{main_rs}"
        );
        println!("wrote {id} DMA project to {}", out.display());
    }

    /// Writes the EXACT files an Async ESP32-C3 project generates (main.rs,
    /// Cargo.toml with the async deps, .cargo/config.toml) into
    /// `%ESP_ASYNC_OUT%`, so the generated code can be compiled for real against
    /// the riscv target — what "verified" means for a codegen change here:
    ///
    /// ```text
    /// ESP_ASYNC_OUT=/some/dir cargo test write_esp_async_project -- --ignored
    /// cd /some/dir && cargo check --target riscv32imc-unknown-none-elf
    /// ```
    #[test]
    #[ignore]
    fn write_esp_async_project() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        use crate::panels::mcu_module::project_gen::{self, AsyncFlavor, ConfigFile};
        use std::fs;
        use std::path::PathBuf;

        let out = PathBuf::from(
            std::env::var("ESP_ASYNC_OUT").expect("set ESP_ASYNC_OUT to the target folder"),
        );
        let def = crate::panels::mcu_module::builtins::builtin_for("esp32c3").expect("esp32c3 def");

        // GPIO in + out and one of every bus — the whole surface the ESP backend
        // emits, so a compile of this project covers the generator, not just the
        // entry point.
        let mut mcu = crate::panels::mcu_module::mock_esp32c3::create_esp32c3();
        // `ESP_ASYNC_RUNTIME=blocking` writes the blocking project instead — the
        // A/B that tells a codegen error apart from one the async path added.
        let async_rt = std::env::var("ESP_ASYNC_RUNTIME").as_deref() != Ok("blocking");
        mcu.runtime = if async_rt {
            Runtime::Async
        } else {
            Runtime::Blocking
        };
        for (name, func) in [
            ("GPIO2", PinFunction::GpioOutput),
            ("GPIO3", PinFunction::GpioInput),
            ("GPIO21", PinFunction::UsartTx(0)),
            ("GPIO20", PinFunction::UsartRx(0)),
            ("GPIO4", PinFunction::SpiSck(2)),
            ("GPIO5", PinFunction::SpiMosi(2)),
            ("GPIO6", PinFunction::SpiMiso(2)),
            ("GPIO7", PinFunction::SpiNss(2)),
            ("GPIO8", PinFunction::I2cScl(0)),
            ("GPIO9", PinFunction::I2cSda(0)),
        ] {
            let pin = mcu
                .iter_all_pins_mut()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} exists"));
            pin.selected_function = func;
        }

        let cargo_toml =
            project_gen::gen_config(ConfigFile::CargoToml, &def.project, &def.toolchain);
        let cargo_toml = project_gen::ensure_async_deps(
            &cargo_toml,
            async_rt,
            AsyncFlavor::Esp(&def.project.probe_chip),
            false,
            false,
            false,
            &[],
        );
        fs::create_dir_all(out.join("src/pins")).unwrap();
        fs::create_dir_all(out.join(".cargo")).unwrap();
        fs::write(out.join("Cargo.toml"), cargo_toml).unwrap();
        fs::write(
            out.join(".cargo/config.toml"),
            project_gen::gen_config(ConfigFile::CargoConfig, &def.project, &def.toolchain),
        )
        .unwrap();
        // `ESP_ASYNC_IN=<file>` re-splices an EXISTING main.rs instead of writing
        // a fresh one — how a real project (older header, user code around the
        // markers) is put through the generator before compiling it.
        let main_rs = match std::env::var("ESP_ASYNC_IN") {
            Ok(src) => mcu.update_main_rs(&fs::read_to_string(src).expect("ESP_ASYNC_IN readable")),
            Err(_) => mcu.fresh_main_rs(),
        };
        fs::write(out.join("src/main.rs"), main_rs).unwrap();

        // The per-peripheral init modules main.rs calls into, plus the `mod`
        // declarations the real project tree writes.
        write_pin_tree(&out, &mcu);
    }

    /// Writes `src/pins/` into `out` exactly as the project tree holds it for
    /// `mcu` ([`Mcu::pin_tree_files`]). `out` is a folder the caller named,
    /// possibly a real project, so only the fully generated `configs/` goes
    /// first - a leftover `i2c0.rs` beside a new `i2c0/mod.rs` would not
    /// compile - and nothing else the user keeps under `src/pins/` is touched.
    fn write_pin_tree(out: &std::path::Path, mcu: &crate::panels::mcu_module::mcu::Mcu) {
        let _ = std::fs::remove_dir_all(out.join("src/pins/configs"));
        for (rel, body) in mcu.pin_tree_files() {
            let path = out.join(&rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
    }

    /// The ESP async backend swaps the entry point for `#[esp_rtos::main]` and
    /// starts the scheduler, while the pin bindings stay the same esp-hal calls.
    #[test]
    fn esp_async_backend_emits_esp_rtos_main() {
        use crate::panels::mcu_module::mcu::Runtime;
        let mut mcu = crate::panels::mcu_module::mock_esp32c3::create_esp32c3();

        // Blocking — the esp-hal entry, no scheduler.
        let blocking = mcu.fresh_main_rs();
        assert!(blocking.contains("#[esp_hal::main]"), "{blocking}");
        assert!(!blocking.contains("esp_rtos"), "{blocking}");

        mcu.runtime = Runtime::Async;
        let code = mcu.fresh_main_rs();
        assert!(code.contains("#[esp_rtos::main]"), "{code}");
        assert!(code.contains("async fn main(_spawner: Spawner)"), "{code}");
        assert!(code.contains("use embassy_executor::Spawner;"), "{code}");
        assert!(
            code.contains("esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);"),
            "{code}"
        );
        // The scheduler must start BEFORE anything that could await.
        let init = code.find("esp_hal::init").expect("init");
        let start = code.find("esp_rtos::start").expect("start");
        assert!(init < start, "start comes after esp_hal::init:\n{code}");
        // ...and the whole runtime difference stays inside the generated block,
        // so a runtime switch never rewrites the user's tail.
        let end = code.find(super::super::GEN_END).expect("end");
        assert!(start < end, "scheduler start inside GEN:\n{code}");
    }

    /// The async backend emits an `#[embassy_executor::main] async fn main`
    /// skeleton (via `Mcu::fresh_main_rs` honouring `runtime`), distinct from the
    /// blocking `#[entry] fn main() -> !`.
    #[test]
    fn async_backend_emits_embassy_executor_main() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        // Retarget to an embassy family (F4) so async applies.
        def.family = "stm32f4".into();
        def.id = "stm32f411re".into();
        let mut mcu = def.build_mcu();

        // Blocking first — the classic entry.
        assert!(mcu.fresh_main_rs().contains("fn main() -> !"));

        // Flip to async — the executor entry + async header.
        mcu.runtime = Runtime::Async;
        let code = mcu.fresh_main_rs();
        assert!(code.contains("HAL: embassy-stm32 (async)"));
        assert!(code.contains("#[embassy_executor::main]"));
        assert!(code.contains("async fn main(_spawner: Spawner)"));
        assert!(code.contains("use embassy_executor::Spawner;"));
        // Crate-level allow (the executor macro drops a fn-level one — see
        // `embassy_async::invariant_header`).
        assert!(code.contains("#![allow(unused_variables, unused_mut)]"));
        assert!(
            !code.contains("fn main() -> !"),
            "no blocking entry in async"
        );
        assert!(
            !code.contains("use cortex_m_rt::entry;"),
            "no cortex_m_rt entry import"
        );
        assert!(code.contains(crate::panels::mcu_module::codegen::GEN_BEGIN));
    }

    /// Toggling runtime on an EXISTING file re-splices the header + entry both
    /// ways while preserving the user's loop body after the markers.
    #[test]
    fn runtime_switch_reslices_header_both_ways_and_keeps_user_tail() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        def.family = "stm32g0".into();
        def.id = "stm32g0b1re".into();
        let mut mcu = def.build_mcu();

        // A blocking file with a hand-edited loop body.
        let blocking = mcu.fresh_main_rs();
        let user = blocking.replace(
            "// Your main loop code here.",
            "defmt::info!(\"tick\"); // USER EDIT",
        );
        assert!(user.contains("fn main() -> !"));

        // → Async: async header/entry replace the blocking ones; user tail kept.
        mcu.runtime = Runtime::Async;
        let now_async = mcu.update_main_rs(&user);
        assert!(now_async.contains("#[embassy_executor::main]"));
        assert!(now_async.contains("HAL: embassy-stm32 (async)"));
        assert!(!now_async.contains("fn main() -> !"), "blocking entry gone");
        assert!(
            !now_async.contains("use cortex_m_rt::entry;"),
            "blocking import gone"
        );
        assert!(now_async.contains("USER EDIT"), "user tail preserved");
        assert_eq!(
            now_async
                .matches(crate::panels::mcu_module::codegen::GEN_BEGIN)
                .count(),
            1
        );

        // → back to Blocking: blocking header/entry restored, still one GEN block.
        mcu.runtime = Runtime::Blocking;
        let back = mcu.update_main_rs(&now_async);
        assert!(back.contains("fn main() -> !"));
        assert!(back.contains("HAL: embassy-stm32 (blocking"));
        assert!(
            !back.contains("#[embassy_executor::main]"),
            "async entry gone"
        );
        assert!(
            !back.contains("use embassy_executor::Spawner;"),
            "async import gone"
        );
        assert!(back.contains("USER EDIT"), "user tail still preserved");
        assert_eq!(
            back.matches(crate::panels::mcu_module::codegen::GEN_BEGIN)
                .count(),
            1
        );
    }

    /// Found while planning F1 on Async: the async STM32 backend wrote the
    /// watchdogs' `init` calls into main.rs but never their files, so any
    /// async STM32 project with a watchdog named a module that did not exist.
    #[test]
    fn an_async_stm32_writes_the_watchdogs_it_starts() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::watchdog::{IwdgConfig, WwdgConfig};

        let mut def = crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        def.family = "stm32f4".into();
        def.id = "stm32f411re".into();
        let mut mcu = def.build_mcu();
        mcu.runtime = Runtime::Async;
        mcu.watchdog.iwdg = Some(IwdgConfig {
            timeout_us: 2_000_000,
        });
        mcu.watchdog.wwdg = Some(WwdgConfig {
            timeout_us: 20_000,
            window_us: 0,
        });

        let code = mcu.fresh_main_rs();
        let names: Vec<String> = mcu.config_files().into_iter().map(|(n, _)| n).collect();
        for (call, file) in [
            ("pins::configs::iwdg::init(p.IWDG)", "iwdg.rs"),
            ("pins::configs::wwdg::init(p.WWDG)", "wwdg.rs"),
        ] {
            assert!(code.contains(call), "main.rs lacks {call}:\n{code}");
            assert!(
                names.iter().any(|n| n == file),
                "main.rs calls {call} but {file} is not written: {names:?}"
            );
        }
    }

    /// An async USART (both TX+RX wired) emits a `usart{n}.rs` config file with
    /// the embassy `BufferedUart` → embedded-io-async bridge, a call into it from
    /// `main.rs`, and does NOT also bind the TX/RX pins raw (they're moved into
    /// the driver).
    #[test]
    fn async_usart_emits_config_file_and_init_call() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        def.family = "stm32f4".into();
        def.id = "stm32f411re".into();
        let mut mcu = def.build_mcu();
        mcu.runtime = Runtime::Async;

        // Wire USART1 TX + RX on whichever pins offer them.
        let tx = mcu
            .iter_all_pins()
            .find(|p| p.available_functions.contains(&PinFunction::UsartTx(1)))
            .map(|p| p.number)
            .expect("a UsartTx(1) pin");
        let rx = mcu
            .iter_all_pins()
            .find(|p| p.available_functions.contains(&PinFunction::UsartRx(1)))
            .map(|p| p.number)
            .expect("a UsartRx(1) pin");
        mcu.apply_pin_function(tx, PinFunction::UsartTx(1));
        mcu.apply_pin_function(rx, PinFunction::UsartRx(1));

        // main.rs: calls into the config module, no raw binding for the USART pins.
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains("pins::configs::usart1::init(p.USART1,"),
            "USART init call missing:\n{code}"
        );
        assert!(
            !code.contains("usart1_tx = p."),
            "USART TX pin must not be bound raw:\n{code}"
        );

        // The config file is the async BufferedUart bridge.
        let cfgs = mcu.config_files();
        let usart1 = &cfgs
            .iter()
            .find(|(n, _)| n == "usart1.rs")
            .expect("usart1.rs config file")
            .1;
        assert!(usart1.contains("BufferedUart"));
        assert!(usart1.contains("embedded_io_async::Read"));
        assert!(usart1.contains("bind_interrupts!"));
        assert_eq!(usart1.matches("pub fn init").count(), 1);
    }

    /// Async SPI/I2C: a blocking-mode module emits `new_blocking` + embedded-hal
    /// 1.0 with a no-DMA init call; an async-DMA module emits `Spi::new` +
    /// embedded-hal-async with the DMA TODO placeholder in `main.rs`.
    #[test]
    fn async_spi_i2c_blocking_and_dma_codegen() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        use crate::panels::mcu_module::modules::{
            AsyncBusMode, ModuleConfig, ModuleKind, SpiModuleConfig, VirtualModule,
        };
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        def.family = "stm32f4".into();
        def.id = "stm32f411re".into();
        let mut mcu = def.build_mcu();
        mcu.runtime = Runtime::Async;

        // Wire SPI1 (SCK/MOSI/MISO) + I2C1 (SCL/SDA) on whatever pins offer them.
        for f in [
            PinFunction::SpiSck(1),
            PinFunction::SpiMosi(1),
            PinFunction::SpiMiso(1),
            PinFunction::I2cScl(1),
            PinFunction::I2cSda(1),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.available_functions.contains(&f))
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no pin for {f:?}"));
            mcu.apply_pin_function(num, f);
        }

        // Default (no module) → blocking SPI/I2C: new_blocking, embedded-hal 1.0,
        // and init calls WITHOUT the DMA TODO.
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains("pins::configs::spi1::init(p.SPI1,"),
            "SPI call:\n{code}"
        );
        assert!(
            code.contains("pins::configs::i2c1::init(p.I2C1,"),
            "I2C call"
        );
        assert!(
            !code.contains("DMA_TX_TODO"),
            "blocking calls carry no DMA TODO"
        );
        let cfgs = mcu.config_files();
        let spi1 = &cfgs
            .iter()
            .find(|(n, _)| n == "spi1.rs")
            .expect("spi1.rs")
            .1;
        assert!(spi1.contains("Spi::new_blocking"));
        assert!(spi1.contains("embedded_hal::spi::SpiBus"));
        let i2c1 = &cfgs
            .iter()
            .find(|(n, _)| n == "i2c1/mod.rs")
            .expect("i2c1/mod.rs")
            .1;
        assert!(i2c1.contains("I2c::new_blocking"));
        assert!(i2c1.contains("embedded_hal::i2c::I2c"));

        // Switch SPI1 to async-DMA via its module → DMA driver + TODO in main.rs.
        mcu.modules.push(VirtualModule {
            id: "gi_spi_1".into(),
            kind: ModuleKind::GenericInterfaceSpi,
            name: "GI_SPI1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Spi(SpiModuleConfig {
                async_mode: AsyncBusMode::AsyncDma,
                ..SpiModuleConfig::new(1)
            }),
            connections: vec![],
        });
        let code = mcu.fresh_main_rs();
        // This used to assert the DMA TODO. On a family WITH a channel table
        // there is none left to assert: the chip is an F4, so SPI1's channels
        // and their interrupt binding are filled in for the user.
        assert!(
            code.contains("p.DMA2_CH3, p.DMA2_CH0, Irqs"),
            "SPI1 gets its F4 channels:
{code}"
        );
        assert!(
            code.contains("DMA2_STREAM3 => embassy_stm32::dma::InterruptHandler"),
            "and the matching interrupt binding:
{code}"
        );
        assert!(
            !code.contains("DMA_TX_TODO"),
            "no TODO left on F4:
{code}"
        );
        let cfgs = mcu.config_files();
        let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
        assert!(spi1.contains("Spi::new(spi"), "DMA driver: {spi1}");
        assert!(spi1.contains("embedded_hal_async::spi::SpiBus"));
        // The USART/GPIO-less project still keeps exactly one gen block.
        assert_eq!(
            code.matches(crate::panels::mcu_module::codegen::GEN_BEGIN)
                .count(),
            1
        );
    }

    /// The Native runtime forces concrete stm32f1xx-hal codegen for the bus
    /// peripherals even when the module keeps its default `Portable` api_style —
    /// the project-wide "all native" behaviour. `native_supported` gates it to F1.
    #[test]
    fn native_runtime_forces_concrete_hal() {
        use crate::panels::mcu_module::mcu::Runtime;
        use crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx;
        use crate::panels::mcu_module::modules::{ApiStyle, ModuleConfig, ModuleKind};

        assert!(native_supported("stm32f1"));
        assert!(!native_supported("stm32f4"));

        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let n = mcu.modules[0].instance();
        // The module keeps its default Portable api_style throughout.
        assert!(
            matches!(&mcu.modules[0].config, ModuleConfig::Usart(c) if c.api_style == ApiStyle::Portable)
        );

        let usart_body = |mcu: &crate::panels::mcu_module::mcu::Mcu| {
            mcu.config_files()
                .into_iter()
                .find(|(name, _)| name == &format!("usart{n}.rs"))
                .expect("usart config file")
                .1
        };

        // Blocking + Portable module → portable embedded-io init.
        // The Portable USART now returns a NAMED type (see `Handle`), so an RTIC
        // `Local` field can name it; it is still embedded-io Read + Write.
        let body = usart_body(&mcu);
        assert!(body.contains("pub type Handle = SerialIo<"), "{body}");
        assert!(body.contains(") -> Handle {"), "{body}");

        // Native runtime OVERRIDES it → concrete `Serial` split, no embedded-io.
        mcu.runtime = Runtime::Native;
        assert!(mcu.is_native());
        let body = usart_body(&mcu);
        assert!(
            body.contains(&format!(
                "-> (serial::Tx<pac::USART{n}>, serial::Rx<pac::USART{n}>)"
            )),
            "{body}"
        );
        assert!(
            !body.contains("embedded_io"),
            "native forced -> no embedded-io:\n{body}"
        );
        // …and main.rs uses the split `(Tx, Rx)` tuple binding.
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains(&format!("let (mut _tx{n}")),
            "native tuple binding:\n{code}"
        );
        assert!(
            !code.contains(&format!("let mut _serial{n}")),
            "not the single-value form"
        );
    }

    /// The WBA backend produces a complete embassy skeleton with the markers,
    /// entry point and user tail — enough for the project to build.
    #[test]
    fn wba_backend_emits_a_complete_skeleton() {
        use crate::panels::mcu_module::mcu_def::McuDefinition;
        let mut def: McuDefinition =
            crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6").unwrap();
        def.family = "stm32wba".into();
        def.id = "stm32wba55cg".into();
        let mcu = def.build_mcu();
        let code = mcu.fresh_main_rs();
        assert!(code.contains("#![no_std]"));
        assert!(code.contains("fn main() -> !"));
        assert!(code.contains("embassy_stm32::init"));
        assert!(code.contains(crate::panels::mcu_module::codegen::GEN_BEGIN));
        assert!(code.contains(crate::panels::mcu_module::codegen::GEN_END));
    }

    /// Build an ESP `Mcu` with SPI2 wired and a role, and return
    /// `(main.rs, spi2.rs)`.
    #[cfg(test)]
    fn esp_spi_role_project(
        chip: &str,
        role: crate::panels::mcu_module::modules::SpiRole,
        mode: u8,
        runtime: Runtime,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            ModuleConfig, ModuleKind, SpiModuleConfig, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;
        for f in [
            PinFunction::SpiSck(2),
            PinFunction::SpiMosi(2),
            PinFunction::SpiMiso(2),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("{chip}: no pin for {f:?}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = SpiModuleConfig::new(2);
        cfg.role = role;
        cfg.mode = mode;
        mcu.modules.push(VirtualModule {
            id: "spi_2".into(),
            kind: ModuleKind::GenericInterfaceSpi,
            name: "SPI2".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Spi(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "spi2.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// A slave module emits the OTHER driver: `spi::slave`, no frequency, and a
    /// DMA channel even though the project is Blocking — esp-hal's slave has no
    /// CPU path at all, so the channel is not the runtime's choice.
    #[test]
    fn esp_spi_slave_emits_the_slave_driver_on_dma() {
        use crate::panels::mcu_module::modules::SpiRole;
        let (main, file) = esp_spi_role_project("esp32c6", SpiRole::Slave, 0, Runtime::Blocking);
        assert!(file.contains("esp_hal::spi::slave"), "driver:\n{file}");
        assert!(file.contains(".with_dma(dma)"), "dma:\n{file}");
        assert!(
            !file.contains("with_frequency"),
            "a slave sets no frequency:\n{file}"
        );
        // `init`, never `init_async`: there is no async twin to call.
        assert!(
            main.contains("configs::spi2::init(peripherals.SPI2,"),
            "call:\n{main}"
        );
        assert!(
            main.contains("peripherals.DMA_"),
            "the slave takes a channel on Blocking too:\n{main}"
        );
    }

    /// The same wiring as Master is the ordinary master file — the role is what
    /// changes, not the pins.
    #[test]
    fn esp_spi_master_is_unchanged() {
        use crate::panels::mcu_module::modules::SpiRole;
        let (_, file) = esp_spi_role_project("esp32c6", SpiRole::Master, 0, Runtime::Blocking);
        assert!(file.contains("esp_hal::spi::master"), "driver:\n{file}");
        assert!(file.contains("with_frequency"), "master clocks it:\n{file}");
    }

    /// A project carried from a C6 to a C5 keeps `role: Slave` in its `.config`
    /// and never reopens the panel. The C5's esp-hal has no `with_dma` for the
    /// slave, so the emitter puts the role back rather than emit a file that
    /// cannot compile.
    #[test]
    fn esp_spi_slave_falls_back_to_master_where_it_cannot_build() {
        use crate::panels::mcu_module::modules::SpiRole;
        assert!(!SpiRole::options("esp32c5").contains(&SpiRole::Slave));
        assert!(!SpiRole::options("esp32c61").contains(&SpiRole::Slave));
        assert!(!SpiRole::options("stm32f4").contains(&SpiRole::Slave));
        assert!(SpiRole::options("esp32c6").contains(&SpiRole::Slave));

        let (_, file) = esp_spi_role_project("esp32c5", SpiRole::Slave, 0, Runtime::Blocking);
        assert!(
            file.contains("esp_hal::spi::master"),
            "the C5 has no usable slave:\n{file}"
        );
    }

    /// The ESP32's slave takes modes 1 and 3 only, and a mode 0 carried over
    /// from another chip is clamped rather than emitted.
    #[test]
    fn esp32_spi_slave_clamps_the_mode() {
        use crate::panels::mcu_module::modules::SpiRole;
        assert_eq!(SpiRole::Slave.modes("esp32"), &[1, 3]);
        assert_eq!(SpiRole::Master.modes("esp32"), &[0, 1, 2, 3]);
        assert_eq!(SpiRole::Slave.modes("esp32c6"), &[0, 1, 2, 3]);

        let (_, file) = esp_spi_role_project("esp32", SpiRole::Slave, 0, Runtime::Blocking);
        assert!(file.contains("Mode::_1"), "clamped:\n{file}");
        // Mode 2 is out too, and clamps to the same place.
        let (_, two) = esp_spi_role_project("esp32", SpiRole::Slave, 2, Runtime::Blocking);
        assert!(two.contains("Mode::_1"), "clamped:\n{two}");
        // A mode it CAN take survives untouched.
        let (_, three) = esp_spi_role_project("esp32", SpiRole::Slave, 3, Runtime::Blocking);
        assert!(three.contains("Mode::_3"), "kept:\n{three}");
    }

    /// Build an ESP `Mcu` with the given TWAI pads wired and a CAN module, and
    /// return `(main.rs, twai0.rs)` — the file empty when none was emitted.
    #[cfg(test)]
    fn esp_twai_project(
        chip: &str,
        pads: &[PinFunction],
        // `None` drops no module at all — the only way a single pad survives,
        // since dropping one auto-wires its partner.
        edit: Option<impl FnOnce(&mut crate::panels::mcu_module::modules::CanModuleConfig)>,
        runtime: Runtime,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            CanModuleConfig, ModuleConfig, ModuleKind, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;
        for f in pads {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("{chip}: no pin for {f:?}"));
            mcu.apply_pin_function(num, f.clone());
        }
        if let Some(edit) = edit {
            let mut cfg = CanModuleConfig::new(1);
            edit(&mut cfg);
            mcu.modules.push(VirtualModule {
                id: "can_1".into(),
                kind: ModuleKind::GenericInterfaceCan,
                name: "CAN".into(),
                pos: (0.0, 0.0),
                config: ModuleConfig::Can(cfg),
                connections: Vec::new(),
            });
        }
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "twai0.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// Both pads wired emit the real driver — and RX goes in FIRST, which is the
    /// constructor's order and the opposite of how the pads are named.
    #[test]
    fn esp_twai_emits_the_driver_with_rx_first() {
        let (main, file) = esp_twai_project(
            "esp32c6",
            &[PinFunction::CanTx, PinFunction::CanRx],
            Some(|_: &mut crate::panels::mcu_module::modules::CanModuleConfig| {}),
            Runtime::Blocking,
        );
        assert!(file.contains("TwaiConfiguration::new("), "ctor:\n{file}");
        assert!(file.contains("cfg.start()"), "started:\n{file}");
        assert!(
            main.contains("configs::twai0::init(peripherals.TWAI0,"),
            "call:\n{main}"
        );

        // The argument order is the whole point: the wrong way round compiles
        // and produces a node that never hears anything.
        let call = main
            .lines()
            .find(|l| l.contains("configs::twai0::init"))
            .expect("the call");
        let rx = call.find("_can_rx").expect("rx arg");
        let tx = call.find("_can_tx").expect("tx arg");
        assert!(rx < tx, "rx must come first: {call}");
    }

    /// A module wires the partner pad by itself: one pad in, both pads out.
    #[test]
    fn esp_twai_module_wires_the_missing_pad() {
        let (main, _) = esp_twai_project(
            "esp32c6",
            &[PinFunction::CanTx],
            Some(|_: &mut crate::panels::mcu_module::modules::CanModuleConfig| {}),
            Runtime::Blocking,
        );
        assert!(main.contains("// CAN  RX"), "auto-wired:{main}");
        assert!(main.contains("twai0::init("), "and built:{main}");
    }

    /// One pad is not a bus. It used to emit a comment naming the pad, which
    /// read like progress; there is no constructor that takes half a node.
    ///
    /// Getting here takes work, and that is the point: assigning either pad
    /// wires the other, so the way a node loses half of itself is a pad being
    /// taken for something ELSE afterwards.
    #[test]
    fn esp_twai_needs_both_pads() {
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c6")
            .unwrap()
            .build_mcu();
        let num = mcu
            .iter_all_pins()
            .find(|p| p.available_functions.contains(&PinFunction::CanTx))
            .map(|p| p.number)
            .expect("a TWAI pad");
        mcu.apply_pin_function(num, PinFunction::CanTx);

        // …which wired RX too. Take that pad back for a plain output.
        let rx = mcu
            .iter_all_pins()
            .find(|p| p.selected_function == PinFunction::CanRx)
            .map(|p| p.number)
            .expect("the partner pad");
        mcu.apply_pin_function(rx, PinFunction::GpioOutput);
        assert!(
            mcu.iter_all_pins()
                .any(|p| p.selected_function == PinFunction::CanTx),
            "TX must survive its partner being taken"
        );

        let main = mcu.fresh_main_rs();
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "twai0.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        assert!(file.is_empty(), "no file for half a bus:\n{file}");
        assert!(!main.contains("twai0::init"), "no call:\n{main}");
        assert!(
            main.contains("TODO: TWAI needs BOTH pads"),
            "the note:\n{main}"
        );
    }

    /// The mode and the transceiver switch reach the generated file, and the
    /// no-transceiver case is a DIFFERENT constructor rather than an argument.
    #[test]
    fn esp_twai_mode_and_transceiver_reach_the_file() {
        use crate::panels::mcu_module::modules::CanMode;
        let (_, file) = esp_twai_project(
            "esp32c3",
            &[PinFunction::CanTx, PinFunction::CanRx],
            Some(
                |c: &mut crate::panels::mcu_module::modules::CanModuleConfig| {
                    c.mode = CanMode::ListenOnly;
                    c.transceiver = false;
                    c.bitrate = 125_000;
                },
            ),
            Runtime::Async,
        );
        assert!(file.contains("TwaiMode::ListenOnly"), "mode:\n{file}");
        assert!(
            file.contains("TwaiConfiguration::new_no_transceiver("),
            "ctor:\n{file}"
        );
        assert!(file.contains("BaudRate::B125K"), "baud:\n{file}");
        // `into_async` sits on the configuration, so it lands before `start`.
        assert!(
            file.contains(".into_async();"),
            "async goes in before start:\n{file}"
        );
    }

    /// esp-hal ships four sets of timings. A bit rate it does not name falls
    /// back to 500k rather than emitting a `BaudRate` variant that is not there.
    #[test]
    fn esp_twai_bitrate_falls_back_to_a_preset() {
        for (hz, want) in [
            (125_000u32, "B125K"),
            (250_000, "B250K"),
            (500_000, "B500K"),
            (1_000_000, "B1000K"),
            (800_000, "B500K"),
        ] {
            let (_, file) = esp_twai_project(
                "esp32c6",
                &[PinFunction::CanTx, PinFunction::CanRx],
                Some(|c: &mut crate::panels::mcu_module::modules::CanModuleConfig| c.bitrate = hz),
                Runtime::Blocking,
            );
            assert!(
                file.contains(&format!("BaudRate::{want}")),
                "{hz} -> {want}:\n{file}"
            );
        }
    }

    /// The C5 has TWAI silicon and no esp-hal driver, so it gets no pads and no
    /// file — the same trap the I2S hit, answered the same way.
    #[test]
    fn a_chip_without_the_driver_offers_no_twai() {
        use crate::panels::mcu_module::modules::CanMode;
        for chip in ["esp32c2", "esp32c5", "esp32c61"] {
            assert!(!codegen_esp::has_twai(chip), "{chip} has no driver");
            let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                .unwrap()
                .build_mcu();
            assert!(
                !mcu.iter_all_pins()
                    .any(|p| p.available_functions.contains(&PinFunction::CanTx)),
                "{chip} must not offer a TWAI pad"
            );
        }
        for chip in [
            "esp32", "esp32c3", "esp32c6", "esp32h2", "esp32s2", "esp32s3",
        ] {
            assert!(codegen_esp::has_twai(chip), "{chip} has the driver");
        }
        // All three modes on an ESP; Normal alone where the generator builds a
        // plain `Can::new`.
        assert_eq!(CanMode::options("esp32c6").len(), 3);
        assert_eq!(CanMode::options("stm32f1"), &[CanMode::Normal]);
    }

    /// Build an S3 with the video port wired and return `(main.rs, lcd_cam.rs)`.
    #[cfg(test)]
    fn esp_lcd_project(
        mode: crate::panels::mcu_module::modules::LcdCamMode,
        width: u8,
        runtime: Runtime,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            LcdCamModuleConfig, ModuleConfig, ModuleKind, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32s3")
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;

        let mut want: Vec<PinFunction> = (0..width)
            .map(|lane| PinFunction::LcdCamData { lane })
            .collect();
        // Every control pad of every mode. The ones this mode does not name are
        // simply never passed, which is what makes one pad set serve three.
        want.extend([
            PinFunction::LcdCamDc,
            PinFunction::LcdCamWr,
            PinFunction::LcdCamCs,
            PinFunction::LcdCamPclk,
            PinFunction::LcdCamVsync,
            PinFunction::LcdCamHsync,
            PinFunction::LcdCamDe,
        ]);
        if mode == crate::panels::mcu_module::modules::LcdCamMode::Camera {
            // The camera half has its OWN pads now — a display's data lines and
            // a sensor's cannot be the same wires when both run at once.
            want = (0..width)
                .map(|lane| PinFunction::CamData { lane })
                .collect();
            want.extend([
                PinFunction::CamPclk,
                PinFunction::CamVsync,
                PinFunction::CamHsync,
                PinFunction::CamHenable,
                PinFunction::CamMclk,
            ]);
        }
        for f in &want {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no S3 pad for {f:?}"));
            mcu.apply_pin_function(num, f.clone());
        }

        let camera = mode == crate::panels::mcu_module::modules::LcdCamMode::Camera;
        let mut cfg = if camera {
            LcdCamModuleConfig::new_camera()
        } else {
            LcdCamModuleConfig::new(0)
        };
        cfg.mode = mode;
        cfg.width = width;
        mcu.modules.push(VirtualModule {
            id: "lcd_cam".into(),
            kind: if camera {
                ModuleKind::GenericInterfaceCamera
            } else {
                ModuleKind::GenericInterfaceLcdCam
            },
            name: "LCD".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::LcdCam(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "lcd_cam.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// Each mode reaches for a DIFFERENT esp-hal driver, and binds only the
    /// control pads that driver has a setter for.
    #[test]
    fn esp_lcd_cam_builds_the_mode_it_was_given() {
        use crate::panels::mcu_module::modules::LcdCamMode;
        let (_, i8080) = esp_lcd_project(LcdCamMode::I8080, 8, Runtime::Blocking);
        assert!(i8080.contains("lcd::i8080"), "driver:\n{i8080}");
        assert!(i8080.contains(".with_dc(dc)") && i8080.contains(".with_wrx(wr)"));
        // The RGB and camera pads are wired on the canvas and STILL not bound:
        // the i8080 driver has no setter for them.
        assert!(!i8080.contains("with_vsync"), "not an RGB panel:\n{i8080}");
        assert!(
            !i8080.contains("with_master_clock"),
            "not a camera:\n{i8080}"
        );

        let (_, dpi) = esp_lcd_project(LcdCamMode::Dpi, 8, Runtime::Blocking);
        assert!(dpi.contains("lcd::dpi"), "driver:\n{dpi}");
        assert!(dpi.contains("FrameTiming {"), "the timings:\n{dpi}");
        assert!(dpi.contains("horizontal_active_width: LCD_H_ACTIVE"));
        assert!(
            !dpi.contains("with_dc("),
            "no command line on an RGB panel:\n{dpi}"
        );

        let (_, cam) = esp_lcd_project(LcdCamMode::Camera, 8, Runtime::Blocking);
        assert!(
            cam.contains("esp_hal::lcd_cam::cam::"),
            "driver:{}{cam}",
            "\n"
        );
        assert!(cam.contains(".with_master_clock(mclk)"), "mclk:\n{cam}");
        assert!(cam.contains(".with_h_enable(href)"), "href:\n{cam}");
        // A camera READS: its data pads take the input bound.
        assert!(
            cam.contains("d0: impl PeripheralInput<'d>"),
            "data direction:\n{cam}"
        );
        assert!(
            !cam.contains("d0: impl PeripheralOutput<'d>"),
            "data direction:\n{cam}"
        );
    }

    /// The width is how many data pads the driver binds — not a number written
    /// anywhere in the i8080 config.
    #[test]
    fn esp_lcd_cam_width_decides_the_pads_bound() {
        use crate::panels::mcu_module::modules::LcdCamMode;
        let (_, eight) = esp_lcd_project(LcdCamMode::I8080, 8, Runtime::Blocking);
        assert!(eight.contains(".with_data7(d7)"));
        assert!(
            !eight.contains(".with_data8("),
            "8-bit stops at 7:\n{eight}"
        );

        let (main, sixteen) = esp_lcd_project(LcdCamMode::I8080, 16, Runtime::Blocking);
        assert!(sixteen.contains(".with_data15(d15)"), "16-bit:\n{sixteen}");
        // …and main.rs passes exactly those pads, in the same order.
        let call = main
            .lines()
            .find(|l| l.contains("configs::lcd_cam::init"))
            .expect("the call");
        assert!(
            call.contains("_lcd_d15"),
            "the wide bus reaches main:\n{call}"
        );
    }

    /// `into_async` sits on `LcdCam`, before either half is taken — so it lands
    /// in the middle of `init`, not at the end of the chain. And the camera's
    /// type does not change with it, because `Camera` has no mode parameter.
    #[test]
    fn esp_lcd_cam_async_is_on_the_peripheral() {
        use crate::panels::mcu_module::modules::LcdCamMode;
        let (_, blocking) = esp_lcd_project(LcdCamMode::I8080, 8, Runtime::Blocking);
        assert!(
            blocking.contains("LcdCam::new(lcd_cam);"),
            "sync:\n{blocking}"
        );
        assert!(blocking.contains("-> I8080<'d, Blocking>"));

        let (_, asyn) = esp_lcd_project(LcdCamMode::I8080, 8, Runtime::Async);
        assert!(
            asyn.contains("LcdCam::new(lcd_cam).into_async();"),
            "async goes in before the half is taken:\n{asyn}"
        );
        assert!(asyn.contains("-> I8080<'d, Async>"));

        let (_, cam) = esp_lcd_project(LcdCamMode::Camera, 8, Runtime::Async);
        assert!(cam.contains(".into_async();"), "still registers:\n{cam}");
        assert!(cam.contains("-> Camera<'d>"), "no mode on Camera:\n{cam}");
        // …and with no mode in the type, the marker is not imported either.
        assert!(
            !cam.contains("use esp_hal::Async;"),
            "unused import:\n{cam}"
        );
    }

    /// Only the ESP32-S3 has LCD_CAM, and the pads say so — the module is not
    /// offered anywhere else rather than offered and then silent.
    #[test]
    fn only_the_s3_offers_the_video_port() {
        for chip in [
            "esp32", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32s2",
        ] {
            let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                .unwrap()
                .build_mcu();
            assert!(
                !mcu.iter_all_pins()
                    .any(|p| p.available_functions.contains(&PinFunction::LcdCamDc)),
                "{chip} must not offer a video pad"
            );
        }
        let s3 = crate::panels::mcu_module::builtins::builtin_for("esp32s3")
            .unwrap()
            .build_mcu();
        assert!(
            s3.iter_all_pins()
                .any(|p| p.available_functions.contains(&PinFunction::LcdCamDc)),
            "the S3 has it"
        );
    }

    /// Build an ESP with the USB pads wired and a USB module, and return
    /// `(main.rs, usb.rs)` — the file empty when none was emitted.
    #[cfg(test)]
    fn esp_usb_project(
        chip: &str,
        role: crate::panels::mcu_module::modules::UsbRole,
        runtime: Runtime,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, VirtualModule};
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;
        for f in [PinFunction::UsbDp, PinFunction::UsbDm] {
            // Assigning one pad of a fixed pair wires the other, so the second
            // pass usually finds its work already done.
            if mcu.iter_all_pins().any(|p| p.selected_function == f) {
                continue;
            }
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("{chip}: no pad for {f:?}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = UsbModuleConfig::new(1);
        cfg.role = role;
        cfg.product = "Test device".into();
        mcu.modules.push(VirtualModule {
            id: "usb_1".into(),
            kind: ModuleKind::GenericInterfaceUsb,
            name: "USB".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usb(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "usb.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// The OTG role reaches the OTHER controller: a different esp-hal module, a
    /// bus rather than a console, and the descriptors actually used.
    #[test]
    fn esp_usb_otg_builds_a_device_of_its_own() {
        let (main, file) = esp_usb_project("esp32s3", UsbRole::Otg, Runtime::Blocking);
        assert!(file.contains("esp_hal::otg_fs"), "driver:\n{file}");
        assert!(file.contains("UsbBusAllocator"), "the bus:\n{file}");
        assert!(
            file.contains(r#"pub const PRODUCT: &str = "Test device";"#),
            "descriptors:\n{file}"
        );
        assert!(!file.contains("UsbSerialJtag"), "not the console:\n{file}");

        // D+ goes in FIRST — the wrong way round compiles and enumerates nothing.
        let call = main
            .lines()
            .find(|l| l.contains("configs::usb::init(peripherals.USB0"))
            .expect("the call");
        let dp = call.find("_usb_dp").expect("dp arg");
        let dm = call.find("_usb_dm").expect("dm arg");
        assert!(dp < dm, "D+ must come first: {call}");

        // The device is built in main.rs, not behind `init`: the serial port and
        // the device both borrow the bus, so all three live in one scope.
        assert!(
            main.contains("usbd_serial::SerialPort::new(&_usb_bus)"),
            "class:\n{main}"
        );
        assert!(main.contains("UsbDeviceBuilder::new("), "device:\n{main}");
    }

    /// Serial/JTAG is untouched: the same one-line console it always was.
    #[test]
    fn esp_usb_serial_jtag_is_unchanged() {
        let (main, file) = esp_usb_project("esp32c6", UsbRole::SerialJtag, Runtime::Async);
        assert!(file.contains("UsbSerialJtag"), "driver:\n{file}");
        assert!(
            main.contains("configs::usb::init_async(peripherals.USB_DEVICE)"),
            "call:\n{main}"
        );
        assert!(!main.contains("UsbDeviceBuilder"), "no stack:\n{main}");
    }

    /// The two controllers are separate silicon sharing one pad pair, and not
    /// every part has both. A role the chip cannot host is put back before
    /// codegen — which is what makes the S2 work without opening the panel,
    /// since its default role is one it does not have.
    #[test]
    fn esp_usb_role_falls_back_to_what_the_chip_has() {
        assert_eq!(UsbRole::options("esp32s3"), &UsbRole::ALL);
        assert_eq!(UsbRole::options("esp32s2"), &[UsbRole::Otg]);
        assert_eq!(UsbRole::options("esp32c6"), &[UsbRole::SerialJtag]);

        // The S2 asked for the console it does not have…
        let (main, file) = esp_usb_project("esp32s2", UsbRole::SerialJtag, Runtime::Blocking);
        assert!(file.contains("esp_hal::otg_fs"), "…and got OTG:\n{file}");
        assert!(main.contains("peripherals.USB0"), "…and got OTG:\n{main}");

        // …and a C6 asked for OTG it does not have.
        let (main, file) = esp_usb_project("esp32c6", UsbRole::Otg, Runtime::Blocking);
        assert!(
            file.contains("UsbSerialJtag"),
            "…and got the console:\n{file}"
        );
        assert!(
            !main.contains("peripherals.USB0"),
            "no OTG on a C6:\n{main}"
        );
    }

    /// Build an ESP32 with `channels` wired to touch pads, and return
    /// `(main.rs, touch.rs)`.
    #[cfg(test)]
    fn esp_touch_project(
        channels: &[u8],
        scan: crate::panels::mcu_module::modules::TouchScan,
        runtime: Runtime,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            ModuleConfig, ModuleKind, TouchModuleConfig, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32")
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;
        for ch in channels {
            let f = PinFunction::TouchPad(*ch);
            let num = mcu
                .iter_all_pins()
                .find(|p| p.available_functions.contains(&f))
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no pad for touch {ch}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = TouchModuleConfig::new(0);
        cfg.scan = scan;
        mcu.modules.push(VirtualModule {
            id: "touch".into(),
            kind: ModuleKind::GenericInterfaceTouch,
            name: "TOUCH".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Touch(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "touch.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// The channel is welded to the GPIO, so wiring pad 5 gets channel 5 — and
    /// the CONTROLLER comes back with the pads, because it owns the peripheral
    /// and dropping it would stop them reading.
    #[test]
    fn esp_touch_binds_the_channel_its_pad_carries() {
        use crate::panels::mcu_module::modules::TouchScan;
        let (main, file) = esp_touch_project(&[0, 5, 9], TouchScan::OneShot, Runtime::Blocking);
        for ch in [0u8, 5, 9] {
            assert!(
                file.contains(&format!("let touch{ch} = TouchPad::new(pad{ch}, &touch);")),
                "channel {ch}:\n{file}"
            );
        }
        assert!(!file.contains("pad1:"), "only what was wired:\n{file}");
        // GPIO12 is touch 5 on this chip, and nothing else is.
        assert!(
            main.contains("peripherals.GPIO12; // TOUCH5"),
            "pad map:\n{main}"
        );
        assert!(
            main.contains("let (mut _touch_ctrl, mut _touch0, mut _touch5, mut _touch9) ="),
            "the controller is kept:\n{main}"
        );
    }

    /// One-shot and continuous are different TYPES in esp-hal, with different
    /// methods — so the scan mode reaches the signature, not just a register.
    #[test]
    fn esp_touch_scan_mode_picks_the_constructor() {
        use crate::panels::mcu_module::modules::TouchScan;
        let (_, one) = esp_touch_project(&[0], TouchScan::OneShot, Runtime::Blocking);
        assert!(one.contains("Touch::one_shot_mode(touch"), "ctor:\n{one}");
        assert!(one.contains("OneShot, Blocking>"), "marker:\n{one}");
        assert!(
            one.contains("sleep_cycles: None"),
            "no timer in one-shot:\n{one}"
        );

        let (_, cont) = esp_touch_project(&[0], TouchScan::Continuous, Runtime::Blocking);
        assert!(
            cont.contains("Touch::continuous_mode(touch"),
            "ctor:\n{cont}"
        );
        assert!(
            cont.contains("sleep_cycles: Some(SLEEP_CYCLES)"),
            "the timer:\n{cont}"
        );
    }

    /// esp-hal has `Touch<Continuous, Async>` and no one-shot twin: waiting for
    /// a touch needs something measuring while you wait. So the async runtime
    /// gets `init_async` for one scan mode and a note for the other.
    #[test]
    fn esp_touch_async_is_continuous_only() {
        use crate::panels::mcu_module::modules::TouchScan;
        let (main, cont) = esp_touch_project(&[0], TouchScan::Continuous, Runtime::Async);
        assert!(cont.contains("pub fn init_async"), "the twin:\n{cont}");
        assert!(
            cont.contains("Touch::async_mode(touch, rtc"),
            "and it takes the RTC:\n{cont}"
        );
        assert!(
            main.contains("Rtc::new(peripherals.LPWR)"),
            "which main builds:\n{main}"
        );
        assert!(main.contains("touch::init_async("), "call:\n{main}");

        let (main, one) = esp_touch_project(&[0], TouchScan::OneShot, Runtime::Async);
        assert!(
            !one.contains("pub fn init_async"),
            "no async one-shot:\n{one}"
        );
        assert!(one.contains("No `init_async`"), "and it says why:\n{one}");
        assert!(
            main.contains("touch::init("),
            "falls back to blocking:\n{main}"
        );
        assert!(
            !main.contains("Rtc::new"),
            "no RTC without the async twin:\n{main}"
        );
    }

    /// Only the original ESP32 has a touch driver. The S2 and S3 have the
    /// sensors in silicon and no `esp_hal::touch`, so they get no pads at all —
    /// the same trap as the C5's I2S, answered the same way.
    #[test]
    fn only_the_original_esp32_offers_touch() {
        for chip in [
            "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32s2", "esp32s3",
        ] {
            assert!(!codegen_esp::has_touch(chip), "{chip} has no touch driver");
            let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                .unwrap()
                .build_mcu();
            assert!(
                !mcu.iter_all_pins()
                    .any(|p| p.available_functions.contains(&PinFunction::TouchPad(0))),
                "{chip} must not offer a touch pad"
            );
        }
        assert!(codegen_esp::has_touch("esp32"));
        let esp32 = crate::panels::mcu_module::builtins::builtin_for("esp32")
            .unwrap()
            .build_mcu();
        let pads = esp32
            .iter_all_pins()
            .filter(|p| {
                p.available_functions
                    .iter()
                    .any(|f| matches!(f, PinFunction::TouchPad(_)))
            })
            .count();
        assert_eq!(pads, 10, "ten channels, one pad each");
    }

    /// Both halves at once: one `init`, two drivers, two DMA channels — the
    /// arrangement the peripheral exists for and the one a single module with a
    /// single mode could not express.
    #[test]
    fn esp_lcd_cam_runs_both_halves_together() {
        use crate::panels::mcu_module::modules::{
            LcdCamMode, LcdCamModuleConfig, ModuleConfig, ModuleKind, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32s3")
            .unwrap()
            .build_mcu();

        // The display half's pads…
        let mut want: Vec<PinFunction> = (0..8u8)
            .map(|lane| PinFunction::LcdCamData { lane })
            .collect();
        want.extend([PinFunction::LcdCamDc, PinFunction::LcdCamWr]);
        // …and the camera's OWN, which is what makes "at once" possible.
        want.extend((0..8u8).map(|lane| PinFunction::CamData { lane }));
        want.extend([
            PinFunction::CamPclk,
            PinFunction::CamVsync,
            PinFunction::CamHenable,
            PinFunction::CamMclk,
        ]);
        for f in &want {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no S3 pad for {f:?}"));
            mcu.apply_pin_function(num, f.clone());
        }
        for (kind, cfg) in [
            (
                ModuleKind::GenericInterfaceLcdCam,
                LcdCamModuleConfig::new(0),
            ),
            (
                ModuleKind::GenericInterfaceCamera,
                LcdCamModuleConfig::new_camera(),
            ),
        ] {
            mcu.modules.push(VirtualModule {
                id: format!("{kind:?}"),
                kind,
                name: "V".into(),
                pos: (0.0, 0.0),
                config: ModuleConfig::LcdCam(cfg),
                connections: Vec::new(),
            });
        }

        let main = mcu.fresh_main_rs();
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "lcd_cam.rs")
            .map(|(_, c)| c)
            .expect("one file for both halves");

        // ONE file, ONE init, both drivers built from the same `LcdCam`.
        assert_eq!(file.matches("LcdCam::new(lcd_cam)").count(), 1, "{file}");
        assert!(
            file.contains("-> (I8080<'d, Blocking>, Camera<'d>)"),
            "pair:\n{file}"
        );
        assert!(
            file.contains("I8080::new(lcd_cam.lcd,"),
            "display half:\n{file}"
        );
        assert!(
            file.contains("Camera::new(lcd_cam.cam,"),
            "camera half:\n{file}"
        );

        // Two channels: TX for the display, RX for the camera.
        assert!(file.contains("dma_lcd: impl TxChannelFor"), "tx:\n{file}");
        assert!(file.contains("dma_cam: impl RxChannelFor"), "rx:\n{file}");
        let call = main
            .lines()
            .find(|l| l.contains("configs::lcd_cam::init"))
            .expect("the call");
        assert_eq!(
            call.matches("peripherals.DMA_CH").count(),
            2,
            "two channels: {call}"
        );

        // Their settings are namespaced, because both live in one file.
        assert!(file.contains("const LCD_FREQUENCY"), "{file}");
        assert!(file.contains("const CAM_FREQUENCY"), "{file}");
        assert!(
            main.contains("let (mut _lcd, mut _cam) ="),
            "both handles:\n{main}"
        );

        // The display half is i8080 or RGB, never the camera: that is the other
        // half's module now.
        assert_ne!(LcdCamModuleConfig::new(0).mode, LcdCamMode::Camera);
        assert_eq!(LcdCamModuleConfig::new_camera().mode, LcdCamMode::Camera);
    }

    /// Build an S3 with MCPWM0 operators 0 and 1 wired, and return
    /// `(main.rs, mcpwm0.rs)`.
    #[cfg(test)]
    fn esp_mcpwm_project(op1_timer: u8, runtime: Runtime) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            McpwmModuleConfig, ModuleConfig, ModuleKind, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32s3")
            .unwrap()
            .build_mcu();
        mcu.runtime = runtime;
        for f in [
            PinFunction::McpwmA {
                unit: 0,
                operator: 0,
            },
            PinFunction::McpwmB {
                unit: 0,
                operator: 0,
            },
            PinFunction::McpwmA {
                unit: 0,
                operator: 1,
            },
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no pad for {f:?}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = McpwmModuleConfig::new(0);
        cfg.duty_x100.insert((0, false), 2_500);
        cfg.duty_x100.insert((1, false), 5_000);
        cfg.op_timer[1] = op1_timer;
        cfg.extra_timers[0] = (1_000, 999);
        mcu.modules.push(VirtualModule {
            id: "mcpwm_0".into(),
            kind: ModuleKind::GenericInterfaceMcpwm,
            name: "MCPWM0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Mcpwm(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "mcpwm0.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// Two operators on two timers: two configs, two `start`s, two handles —
    /// and each operator pointed at ITS timer.
    #[test]
    fn esp_mcpwm_gives_each_operator_its_own_timer() {
        let (main, file) = esp_mcpwm_project(1, Runtime::Blocking);
        assert!(
            file.contains("mcpwm.operator0.set_timer(&mcpwm.timer0);"),
            "{file}"
        );
        assert!(
            file.contains("mcpwm.operator1.set_timer(&mcpwm.timer1);"),
            "{file}"
        );
        assert!(
            file.contains("const FREQUENCY_HZ_T0: u32 = 20000;"),
            "{file}"
        );
        assert!(
            file.contains("const FREQUENCY_HZ_T1: u32 = 1000;"),
            "{file}"
        );
        assert!(file.contains("mcpwm.timer0.start(timer_cfg_t0);"), "{file}");
        assert!(file.contains("mcpwm.timer1.start(timer_cfg_t1);"), "{file}");
        // Both timers come back: each owns the clock guard for its own outputs.
        assert!(
            main.contains("let (_mcpwm0_timer0, _mcpwm0_timer1, mut _mcpwm0_op0a"),
            "handles:\n{main}"
        );
    }

    /// The duty is a fraction of the OPERATOR's timer, so the same 50 % is a
    /// different timestamp on a different period. Reading it against the wrong
    /// timer is a silently wrong pulse width, which is why this is asserted.
    #[test]
    fn esp_mcpwm_duty_follows_its_own_timer() {
        let (_, split) = esp_mcpwm_project(1, Runtime::Blocking);
        // Timer 1 has period 999, so 50 % is 500.
        assert!(
            split.contains("const TIMESTAMP_OP1A: u16 = 500; // 50.00 % of timer 1"),
            "{split}"
        );
        // Timer 0 has period 99, so 25 % is 25.
        assert!(
            split.contains("const TIMESTAMP_OP0A: u16 = 25; // 25.00 % of timer 0"),
            "{split}"
        );

        // …and with both operators back on timer 0, the same duty is 50 of 100.
        let (main, one) = esp_mcpwm_project(0, Runtime::Blocking);
        assert!(
            one.contains("const TIMESTAMP_OP1A: u16 = 50; // 50.00 % of timer 0"),
            "{one}"
        );
        assert!(
            !one.contains("FREQUENCY_HZ_T1"),
            "one timer, one config:\n{one}"
        );
        assert!(
            !one.contains("timer1.start"),
            "no unused timer started:\n{one}"
        );
        assert!(
            main.contains("let (_mcpwm0_timer0, mut _mcpwm0_op0a"),
            "one handle:\n{main}"
        );
    }

    /// Both PARL_IO halves at once: one `ParlIo::new`, one DMA channel, two
    /// drivers. The single channel is what differs from LCD_CAM — esp-hal
    /// splits it into a tx and an rx half itself.
    #[test]
    fn esp_parl_io_runs_both_halves_off_one_channel() {
        use crate::panels::mcu_module::modules::{
            ModuleConfig, ModuleKind, ParlIoModuleConfig, ParlIoWidth, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c6")
            .unwrap()
            .build_mcu();
        let mut want: Vec<PinFunction> = (0..4u8)
            .map(|lane| PinFunction::ParlData { lane })
            .collect();
        want.push(PinFunction::ParlClk);
        // The receiving half's OWN pads — separate `PARL_RX_*` signals.
        want.extend((0..4u8).map(|lane| PinFunction::ParlRxData { lane }));
        want.push(PinFunction::ParlRxClk);
        for f in &want {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no C6 pad for {f:?}"));
            mcu.apply_pin_function(num, f.clone());
        }
        for (kind, mut cfg) in [
            (
                ModuleKind::GenericInterfaceParlIo,
                ParlIoModuleConfig::new(0),
            ),
            (
                ModuleKind::GenericInterfaceParlIoRx,
                ParlIoModuleConfig::new_rx(),
            ),
        ] {
            cfg.width = ParlIoWidth::Four;
            mcu.modules.push(VirtualModule {
                id: format!("{kind:?}"),
                kind,
                name: "P".into(),
                pos: (0.0, 0.0),
                config: ModuleConfig::ParlIo(cfg),
                connections: Vec::new(),
            });
        }

        let main = mcu.fresh_main_rs();
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "parl_io.rs")
            .map(|(_, c)| c)
            .expect("one file for both halves");

        assert_eq!(
            file.matches("ParlIo::new(parl_io, dma)").count(),
            1,
            "{file}"
        );
        assert!(
            file.contains(
                "-> (ParlIoTx<'d, Blocking>, DmaTxBuf, ParlIoRx<'d, Blocking>, DmaRxBuf)"
            ),
            "both drivers:\n{file}"
        );
        assert!(file.contains("port.tx.with_config("), "{file}");
        assert!(file.contains("port.rx.with_config("), "{file}");
        // ONE channel, not two — the peripheral splits it itself.
        assert_eq!(
            file.matches("impl DmaChannelFor<PARL_IO<'d>>").count(),
            1,
            "one channel:\n{file}"
        );
        let call = main
            .lines()
            .find(|l| l.contains("configs::parl_io::init"))
            .expect("the call");
        assert_eq!(call.matches("peripherals.DMA_CH").count(), 1, "{call}");
        assert!(main.contains("mut _parl,"), "both handles:\n{main}");
        assert!(main.contains("mut _parl_rx,"), "both handles:\n{main}");

        // The receiver's clock is NOT the transmitter's type: `ClkInPin` is a
        // TxClkPin, and using it for the rx half does not compile.
        assert!(file.contains("RxClkInPin::new("), "rx clock:\n{file}");
        assert!(file.contains("ClkOutPin::new("), "tx clock:\n{file}");
        // `RxClkInPin` CONTAINS `ClkInPin`, so the bare one is absent only
        // when every occurrence belongs to the Rx type.
        assert_eq!(
            file.matches("ClkInPin::new(").count(),
            file.matches("RxClkInPin::new(").count(),
            "never the tx-clocked one:\n{file}"
        );
    }

    /// A PCNT unit has two channels adding into one counter, each with its own
    /// pads and its own rules — which is what a quadrature encoder needs.
    #[test]
    fn esp_pcnt_configures_both_channels() {
        use crate::panels::mcu_module::modules::{
            ModuleConfig, ModuleKind, PcntChannelCfg, PcntCtrlMode, PcntEdgeMode, PcntModuleConfig,
            VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c6")
            .unwrap()
            .build_mcu();
        for f in [
            PinFunction::PcntEdge {
                unit: 0,
                channel: 0,
            },
            PinFunction::PcntCtrl {
                unit: 0,
                channel: 0,
            },
            PinFunction::PcntEdge {
                unit: 0,
                channel: 1,
            },
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no pad for {f:?}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = PcntModuleConfig::new(0);
        cfg.set_channel(
            1,
            PcntChannelCfg {
                pos_edge: PcntEdgeMode::Decrement,
                neg_edge: PcntEdgeMode::Hold,
                ctrl_low: PcntCtrlMode::Keep,
                ctrl_high: PcntCtrlMode::Keep,
            },
        );
        mcu.modules.push(VirtualModule {
            id: "pcnt_0".into(),
            kind: ModuleKind::GenericInterfacePcnt,
            name: "PCNT0".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Pcnt(cfg),
            connections: Vec::new(),
        });

        let main = mcu.fresh_main_rs();
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "pcnt0.rs")
            .map(|(_, c)| c)
            .expect("the unit's file");

        assert!(file.contains("let channel0 = &unit.channel0;"), "{file}");
        assert!(file.contains("let channel1 = &unit.channel1;"), "{file}");
        // Channel 0 has a control pad and channel 1 does not, so only one of
        // them takes a ctrl argument or sets a ctrl mode.
        assert!(file.contains("channel0.set_ctrl_signal(ctrl0);"), "{file}");
        assert!(!file.contains("channel1.set_ctrl_signal"), "{file}");
        // Each channel's own rules reach the file.
        assert!(
            file.contains("channel1.set_input_mode(EdgeMode::Hold, EdgeMode::Decrement);"),
            "channel 1's rules:\n{file}"
        );
        assert!(
            main.contains("_pcnt0_edge1"),
            "the second edge pad:\n{main}"
        );
    }

    /// Build a chip with UART1's four pads wired, and return
    /// `(main.rs, uart1.rs)`.
    #[cfg(test)]
    fn esp_uart_project(
        chip: &str,
        direction: crate::panels::mcu_module::modules::UsartDirection,
        flow: crate::panels::mcu_module::modules::UsartFlow,
    ) -> (String, String) {
        use crate::panels::mcu_module::modules::{
            ModuleConfig, ModuleKind, UsartModuleConfig, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
            .unwrap()
            .build_mcu();
        for f in [
            PinFunction::UsartTx(1),
            PinFunction::UsartRx(1),
            PinFunction::UsartCts(1),
            PinFunction::UsartRts(1),
        ] {
            if mcu.iter_all_pins().any(|p| p.selected_function == f) {
                continue;
            }
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("{chip}: no pad for {f:?}"));
            mcu.apply_pin_function(num, f);
        }
        let mut cfg = UsartModuleConfig::new(1);
        cfg.direction = direction;
        cfg.flow = flow;
        mcu.modules.push(VirtualModule {
            id: "usart_1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            name: "UART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart(cfg),
            connections: Vec::new(),
        });
        let file = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "uart1.rs")
            .map(|(_, c)| c)
            .unwrap_or_default();
        (mcu.fresh_main_rs(), file)
    }

    /// A single direction is a different esp-hal TYPE with a shorter signature,
    /// and `main.rs` must pass exactly what that signature declares — a
    /// mismatched argument list is the failure this guards.
    #[test]
    fn esp_uart_direction_picks_the_driver_and_the_pads() {
        use crate::panels::mcu_module::modules::{UsartDirection, UsartFlow};
        for (dir, driver, has, hasnt) in [
            (UsartDirection::TxRx, "Uart", "_usart1_rx", ""),
            (UsartDirection::TxOnly, "UartTx", "_usart1_tx", "_usart1_rx"),
            (UsartDirection::RxOnly, "UartRx", "_usart1_rx", "_usart1_tx"),
        ] {
            let (main, file) = esp_uart_project("esp32c6", dir, UsartFlow::None);
            assert!(
                file.contains(&format!("-> {driver}<'d, Blocking>")),
                "{dir:?}: {file}"
            );
            let call = main
                .lines()
                .find(|l| l.contains("configs::uart1::init"))
                .unwrap_or_else(|| panic!("{dir:?}: no call:\n{main}"));
            assert!(call.contains(has), "{dir:?}: {call}");
            if hasnt != "" {
                assert!(!call.contains(hasnt), "{dir:?}: {call}");
            }
            // No flow asked for, so neither flow pad is passed.
            assert!(!call.contains("_usart1_cts"), "{dir:?}: {call}");
            assert!(!call.contains("_usart1_rts"), "{dir:?}: {call}");
        }
    }

    /// Flow control is a config AND a pad — one without the other is half a
    /// feature. A half of the port takes only the pad it drives.
    #[test]
    fn esp_uart_flow_binds_the_config_and_the_pads() {
        use crate::panels::mcu_module::modules::{UsartDirection, UsartFlow};
        let (main, file) = esp_uart_project("esp32c6", UsartDirection::TxRx, UsartFlow::CtsRts);
        assert!(file.contains("const FLOW: HwFlowControl"), "{file}");
        assert!(file.contains("cts: CtsConfig::Enabled,"), "{file}");
        assert!(file.contains("rts: RtsConfig::Enabled(8),"), "{file}");
        assert!(file.contains(".with_hw_flow_ctrl(FLOW)"), "{file}");
        assert!(
            file.contains(".with_cts(cts)") && file.contains(".with_rts(rts)"),
            "{file}"
        );
        let call = main
            .lines()
            .find(|l| l.contains("configs::uart1::init"))
            .expect("the call");
        assert!(
            call.contains("_usart1_cts") && call.contains("_usart1_rts"),
            "{call}"
        );

        // TX-only takes RTS and never CTS: it drives the flow line, it does not
        // read one.
        let (main, file) = esp_uart_project("esp32c6", UsartDirection::TxOnly, UsartFlow::Rts);
        assert!(file.contains(".with_rts(rts)"), "{file}");
        assert!(!file.contains(".with_cts("), "{file}");
        let call = main
            .lines()
            .find(|l| l.contains("configs::uart1::init"))
            .expect("the call");
        assert!(
            call.contains("_usart1_rts") && !call.contains("_usart1_cts"),
            "{call}"
        );
    }

    /// The panel offers only what THIS family's backend can build. It used to
    /// offer every shape everywhere and silently drop what the F1 and the ESP
    /// could not — a control that changes nothing is worse than an absent one.
    #[test]
    fn usart_modes_are_offered_only_where_they_can_be_built() {
        use crate::panels::mcu_module::modules::{UsartDirection, UsartFlow, UsartMode};
        // The F1 has one shape: `serial::Pins` is implemented for the (TX, RX)
        // PAIR alone, and the HAL has no flow control at all.
        assert_eq!(
            UsartDirection::options_for(UsartMode::Buffered, "stm32f1"),
            &[UsartDirection::TxRx]
        );
        assert_eq!(
            UsartFlow::options_for(UsartMode::Buffered, UsartDirection::TxRx, "stm32f1"),
            &[UsartFlow::None]
        );

        // The ESP has both single directions and RTS/CTS — but no single wire.
        let esp = UsartDirection::options_for(UsartMode::Buffered, "esp32c6");
        assert!(esp.contains(&UsartDirection::TxOnly));
        assert!(esp.contains(&UsartDirection::RxOnly));
        assert!(
            !esp.iter().any(|d| d.is_half_duplex()),
            "no single wire: {esp:?}"
        );
        assert!(
            UsartFlow::options_for(UsartMode::Buffered, UsartDirection::TxRx, "esp32c6")
                .contains(&UsartFlow::CtsRts)
        );
        // …and never the RS485 driver-enable, which esp-hal does not expose.
        assert!(
            !UsartFlow::options_for(UsartMode::Buffered, UsartDirection::TxRx, "esp32c6")
                .contains(&UsartFlow::De)
        );

        // embassy keeps every shape it always had.
        let emb = UsartDirection::options_for(UsartMode::Dma, "stm32f4");
        assert!(emb.iter().any(|d| d.is_half_duplex()), "{emb:?}");
    }

    /// The ESP has no Init API choice: `codegen_esp_configs` always hands back
    /// the concrete esp-hal driver, and no ESP emitter reads `api_style`.
    ///
    /// The panel shows the row locked; this pins the VALUE too, so a project
    /// carried over from an STM32 cannot keep claiming `Portable` on a chip
    /// where nothing would honour it.
    #[test]
    fn the_esp_pins_its_api_style_because_nothing_reads_it() {
        use crate::panels::mcu_module::modules::{
            ApiStyle, ModuleConfig, ModuleKind, UsartModuleConfig, VirtualModule,
        };
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c6")
            .unwrap()
            .build_mcu();
        // Blocking, so the Native-runtime pin is NOT what does the work here.
        mcu.runtime = Runtime::Blocking;
        let mut cfg = UsartModuleConfig::new(1);
        cfg.api_style = ApiStyle::Portable;
        mcu.modules.push(VirtualModule {
            id: "usart_1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            name: "UART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart(cfg),
            connections: Vec::new(),
        });

        let (usart, _, _) = resolve_bus_configs(&mcu);
        assert_eq!(
            usart[&1].api_style,
            ApiStyle::Native,
            "the ESP pins it, whatever the project said"
        );

        // …and an STM32 on the same runtime keeps the user's choice, because
        // there the bridge is real.
        let mut f1 = crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6")
            .unwrap()
            .build_mcu();
        f1.runtime = Runtime::Blocking;
        let mut cfg = UsartModuleConfig::new(1);
        cfg.api_style = ApiStyle::Portable;
        f1.modules.push(VirtualModule {
            id: "usart_1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            name: "USART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart(cfg),
            connections: Vec::new(),
        });
        let (usart, _, _) = resolve_bus_configs(&f1);
        assert_eq!(
            usart[&1].api_style,
            ApiStyle::Portable,
            "the F1 still chooses"
        );
    }
}

#[cfg(test)]
mod esp_gpio_mode_tests {
    use super::{AsyncEspBackend, Esp32Backend, FamilyBackend, GpioMode, PinFunction};

    /// Both ESP runtimes offer the three input pulls, and push-pull only.
    ///
    /// They offered NOTHING until the emitter learned to write
    /// `InputConfig::default().with_pull(..)` - the house rule being that a
    /// backend lists a mode only if it can generate it. This test is the other
    /// half of that rule: now that it CAN, an empty list would be the UI
    /// silently withdrawing a working choice.
    ///
    /// Outputs stay at push-pull because `OutputConfig` carries `drive_mode`,
    /// `drive_strength` and `pull` as three independent fields, which a flat
    /// `GpioMode` cannot express - the same shape `WbaBackend` has, for its own
    /// reason.
    #[test]
    fn both_esp_runtimes_offer_the_three_pulls() {
        let backends: [&dyn FamilyBackend; 2] = [&Esp32Backend, &AsyncEspBackend];
        for b in backends {
            let id = b.family_id();
            assert_eq!(
                b.gpio_modes(&PinFunction::GpioInput),
                &[GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown],
                "{id} does not offer the pulls it can now generate"
            );
            assert_eq!(
                b.gpio_modes(&PinFunction::GpioOutput),
                &[GpioMode::PushPull],
                "{id} offers an output mode it cannot generate"
            );
            // A peripheral pad's mode belongs to the peripheral.
            assert!(
                b.gpio_modes(&PinFunction::UsartTx(1)).is_empty(),
                "{id} offers a GPIO mode on a USART pad"
            );
        }
    }
}
