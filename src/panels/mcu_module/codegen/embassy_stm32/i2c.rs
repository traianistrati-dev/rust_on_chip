//! I2C: `i2c{N}/mod.rs`, blocking or async-DMA.

use super::*;

/// I2C instances with both SCL + SDA wired. Returns `(instance, scl, sda)`.
pub(super) fn i2c_wires(pins: &[&Pin]) -> Vec<(u8, String, String)> {
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

// ── Async I2C config file (blocking | async-DMA) ──────────────────────────────

/// Blocking I2C (`I2c::new_blocking`) exposed as the STANDARD blocking
/// `embedded-hal` 1.0 `I2c`. No DMA/interrupts — compiles out of the box.
pub(super) const ASYNC_I2C_TMPL_BLOCKING: &str =
    include_str!("../templates/embassy_stm32/i2c_blocking.rs.tmpl");

/// Async DMA I2C (`I2c::new`) exposed as `embedded-hal-async` `I2c`
/// (`.await`-able). Needs an interrupt binding (event + error) and two DMA
/// channels (passed by `main.rs`).
pub(super) const ASYNC_I2C_TMPL_DMA: &str =
    include_str!("../templates/embassy_stm32/i2c_dma.rs.tmpl");

/// The blocking I2C template's runtime text - see `SPI_ASYNC_INTRO`.
const I2C_ASYNC_INTRO: &str = " A blocking bus in
// an async project is a common, valid pattern; for `.await`-able DMA I2C switch
// this module to \"Async-DMA\" in the IDE.";
const I2C_ASYNC_USAGE: &str =
    "// Blocking init inside an async project: the handle is an `embedded-hal` 1.0
// `I2c` — no `.await`. Switch the module to Async-DMA for long transfers.
";
const I2C_BLOCKING_USAGE: &str =
    "// The handle is an `embedded-hal` 1.0 `I2c`: each transfer busy-waits until the
// device has answered.
";

/// Render the I2C config file for instance `n` on the Async runtime, picking
/// the blocking or async-DMA template from the module's [`AsyncBusMode`].
#[cfg(test)]
pub fn i2c_config_file(n: u8, cfg: Option<&I2cModuleConfig>) -> String {
    i2c_config_file_for(PeriphMode::Async, n, cfg)
}

/// [`i2c_config_file`] for either runtime. On Blocking the bus is always the
/// blocking one, and the module's timeout is left out: embassy's I2C has the
/// field only with embassy-time, which the Blocking runtime does not link.
pub fn i2c_config_file_for(runtime: PeriphMode, n: u8, cfg: Option<&I2cModuleConfig>) -> String {
    let clk = cfg.map(|c| c.clock_hz).unwrap_or(100_000);
    // 0 means "embassy's default", so nothing is written and the existing
    // output is unchanged. Clamped otherwise: a 0 ms timeout would fail every
    // transfer instantly, which reads as broken wiring.
    let tmo = cfg.map(|c| c.timeout_ms).unwrap_or(0);
    let extra = match (tmo, runtime) {
        (0, _) => String::new(),
        (_, PeriphMode::Blocking) => format!(
            "    // The module's {tmo} ms timeout is not applied: embassy's I2C has a timeout
    // only with embassy-time, which the Blocking runtime does not run.
"
        ),
        (_, PeriphMode::Async) => format!(
            "    config.timeout = embassy_time::Duration::from_millis({});
",
            tmo.clamp(1, 60_000)
        ),
    };
    let (intro, usage) = match runtime {
        PeriphMode::Async => (I2C_ASYNC_INTRO, I2C_ASYNC_USAGE),
        PeriphMode::Blocking => ("", I2C_BLOCKING_USAGE),
    };
    let bus_mode = match runtime {
        PeriphMode::Async => cfg.map(|c| c.async_mode).unwrap_or_default(),
        PeriphMode::Blocking => AsyncBusMode::Blocking,
    };
    let template = match bus_mode {
        AsyncBusMode::Blocking => ASYNC_I2C_TMPL_BLOCKING,
        AsyncBusMode::AsyncDma => ASYNC_I2C_TMPL_DMA,
    };
    let sfx = cfg
        .map(|c| sanitize_label(&c.custom_label))
        .filter(|s| !s.is_empty())
        .map(|s| format!("_{s}"))
        .unwrap_or_default();
    tmpl::render(
        template,
        &[
            ("{HANDLE}", &format!("_i2c{n}{sfx}")),
            ("{N}", &n.to_string()),
            ("{EXTRA_CFG}", &extra),
            ("{CLK}", &clk.to_string()),
            ("{DEVICES}", &super::super::common::i2c_device_mods(cfg)),
            ("{INTRO}", intro),
            ("{USAGE}", usage),
        ],
    )
}
