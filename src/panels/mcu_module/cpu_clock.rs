//! The CPU clock a built firmware runs at - what the Profile tab's Timing mode
//! divides its cycle counts by.
//!
//! The clock the HAL really programs, read the way [`super::uart_baud`] reads
//! the bus clocks: the F1's `rcc.cfgr` chain through stm32f1xx-hal's own
//! derivation, embassy-stm32 from the tree the block is emitted from, rp-hal
//! from the Clock tab's PLL, embassy-rp at its fixed default, an ESP from the
//! `CpuClock` its `main.rs` asks for. `None` where nothing says: a hand-written
//! clock block, an STM32 the IDE writes no clock code for, an nRF5340 / nRF54L
//! (their CPU clock is not modelled), a clock the HAL refuses at boot.

use super::codegen::{family::is_esp, nrf, rcc, rp, stm32};
use super::mcu::Mcu;

/// The CPU clock in Hz of the firmware built from `main_rs` for `mcu`.
pub fn cpu_hz(mcu: &Mcu, main_rs: &str) -> Option<u64> {
    // A clock that reads 0 is a tree the model cannot follow, not a chip
    // that runs at 0 Hz: cycles only, rather than infinite times.
    hz_of(mcu, main_rs).filter(|&hz| hz > 0)
}

fn hz_of(mcu: &Mcu, main_rs: &str) -> Option<u64> {
    let family = mcu.family.as_str();
    if is_esp(family) {
        return esp_cpu_mhz(main_rs, super::esp_clocks::max_mhz(family))
            .map(|m| u64::from(m) * 1_000_000);
    }
    if rp::is_rp(family) {
        return if mcu.is_async() {
            Some(u64::from(rp::async_sys_hz(family)))
        } else {
            rp::blocking_plls_ok(mcu).then(|| u64::from(rp::blocking_peri_hz(mcu)))
        };
    }
    if nrf::is_nrf(family) {
        // HFCLK is 64 MHz on HFINT and on HFXO alike, and the nRF52 CPU runs
        // on it. The nRF5340 app core and the nRF54L can run at 128 MHz, which
        // the model does not say.
        return family.starts_with("nrf52").then_some(64_000_000);
    }
    if !family.starts_with("stm32") || mcu.clock_manual {
        return None;
    }
    if family == "stm32f1" {
        if mcu.is_async() {
            // What the emitted block programs, not the tab's own numbers:
            // the block switches on an HSE the tree leaves off.
            let c = rcc::f1_embassy_clock(&mcu.clock);
            return rcc::f1_embassy_clock_ok(&c)
                .then(|| u64::from(rcc::f1_embassy_sysclk_hz(&c) / u32::from(c.ahb_pre).max(1)));
        }
        return stm32::f1_hal_hclk(&mcu.clock).map(u64::from);
    }
    if family == "stm32n6" {
        return None;
    }
    // What `rcc::graph_clock_block_for` emits from: the family's recipe, else
    // the tree's own shape, else a skeleton at an unknown reset clock.
    let graph = match &mcu.clock {
        super::clock::ClockConfig::Graph(gc) => Some(gc.for_codegen()),
        _ => None,
    };
    let graph = graph.as_deref();
    if graph.is_some_and(|g| !rcc::tree_sources_supported(g)) {
        return None;
    }
    let spec = match rcc::rcc_recipe(family) {
        Some((spec, _)) => spec,
        None => rcc::generic_recipe_for(family, graph?)?.0,
    };
    let v = match graph {
        Some(g) => rcc::read_rcc_values(g, &spec),
        None => spec.reset.clone(),
    };
    // An H7's core runs at SYSCLK / D1CPRE, which no block writes (/1): the
    // tree's `ahb` there is HPRE, the buses' divider, not the CPU's.
    let ahb = if family == "stm32h7" { 1 } else { v.ahb.max(1) };
    (v.sysclk_hz > 0).then(|| u64::from(v.sysclk_hz / ahb))
}

/// The MHz an ESP firmware asks esp-hal for: `CpuClock::_160MHz`, or `max`
/// for `CpuClock::max()`. `None` when its `main.rs` names neither.
pub fn esp_cpu_mhz(main_rs: &str, max: Option<u32>) -> Option<u32> {
    let named = main_rs.find("CpuClock::_").and_then(|at| {
        let digits: String = main_rs[at + "CpuClock::_".len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    });
    named.or_else(|| main_rs.contains("CpuClock::max()").then_some(max).flatten())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_esp_clock_comes_from_main_rs() {
        let main =
            "esp_hal::init(Config::default().with_cpu_clock(esp_hal::clock::CpuClock::_160MHz));";
        assert_eq!(esp_cpu_mhz(main, Some(240)), Some(160));
        let main = "esp_hal::init(Config::default().with_cpu_clock(CpuClock::max()));";
        assert_eq!(esp_cpu_mhz(main, Some(160)), Some(160));
        // esp-hal's own default is not something this file says.
        assert_eq!(
            esp_cpu_mhz("esp_hal::init(Config::default())", Some(160)),
            None
        );
    }

    fn mcu(family: &str) -> Mcu {
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        let tc = if is_esp(family) {
            ToolchainKind::EspRust
        } else {
            ToolchainKind::RustEmbedded
        };
        Mcu::new(
            family.into(),
            family.into(),
            tc,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    #[test]
    fn every_chip_family_gets_its_hals_clock() {
        assert_eq!(
            cpu_hz(&mcu("esp32c3"), "CpuClock::max()"),
            Some(160_000_000)
        );
        assert_eq!(
            cpu_hz(&mcu("esp32c3"), "CpuClock::_80MHz"),
            Some(80_000_000)
        );
        assert_eq!(cpu_hz(&mcu("nrf52840"), ""), Some(64_000_000));
        assert_eq!(cpu_hz(&mcu("nrf5340"), ""), None);
        let mut f4 = mcu("stm32f4");
        // No tree: the recipe's reset clock, HSI 16 MHz on the F4.
        assert_eq!(cpu_hz(&f4, ""), Some(16_000_000));
        f4.clock_manual = true;
        assert_eq!(
            cpu_hz(&f4, ""),
            None,
            "a hand-written clock block is unknown"
        );
        assert_eq!(cpu_hz(&mcu("stm32n6"), ""), None);
        // The F1 off Async: stm32f1xx-hal's own derivation of its chain.
        let f1 = mcu("stm32f1");
        let hal = stm32::f1_hal_hclk(&f1.clock).map(u64::from);
        assert!(hal.is_some_and(|h| h > 0));
        assert_eq!(cpu_hz(&f1, ""), hal);
    }
}
