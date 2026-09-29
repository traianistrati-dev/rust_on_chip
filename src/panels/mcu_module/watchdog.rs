//! Watchdog configuration (IWDG + WWDG) — the Configuration tab's model.
//!
//! # Why this is not a grid of register fields
//!
//! CubeMX exposes the registers: prescaler, window value, free-running
//! downcounter. embassy exposes DURATIONS —
//! `WindowWatchdog::new(peri, timeout_us, window_us)` — and derives the
//! registers itself, at runtime, from the live PCLK1. There is no API that
//! accepts register values, so the IDE's surface has to be time.
//!
//! # What that costs, and what this module buys back
//!
//! Time is the honest surface but it is not self-validating: a duration the
//! hardware cannot express is not a compile error, it is a **panic at boot** —
//! `assert!(window_us < timeout_us)`, or the `unwrap!` behind "WWDG: timeout_us
//! is out of range for all prescalers". Neither shows up in `cargo check`.
//!
//! So this module reproduces embassy's arithmetic exactly (integer division and
//! all) to answer, before any code is generated: what range can this chip
//! actually reach, and is the value the user typed inside it.
//!
//! # Three facts that are per-family, and were checked rather than assumed
//!
//! * **LSI** — the IWDG's clock — is 40 kHz on F0/F1/F3 and 32 kHz everywhere
//!   else (embassy `rcc/bd.rs`).
//! * **IWDG prescaler** tops out at 256, except on `iwdg_v3` (H5/U5/WBA) where
//!   it reaches 1024.
//! * **WWDG prescaler** tops out at 8 on `wwdg_v1` and 128 on `wwdg_v2`. v1
//!   covers F1/F2/F3/F4/F7 **and L4**, which is the one that would have been
//!   guessed wrong.

use serde::{Deserialize, Serialize};

/// The IWDG counter is 12-bit; its maximum reload value (embassy `MAX_RL`).
const MAX_RL: u32 = 0xFFF;
/// The WWDG counter divides PCLK1 by 4096 before the prescaler.
const WWDG_DIV: u64 = 4096;

/// Which HAL computes the IWDG timing.
///
/// NOT a cosmetic distinction: the two truncate in different places and
/// disagree about the maximum by 42 ms. embassy divides the LSI by the
/// prescaler first (`40000/256 = 156 Hz`) and reaches 26.256 s;
/// `stm32f1xx-hal` multiplies before dividing (`4096 * 256 / 40 kHz`) and
/// stops at 26.214 s - and asking it for more is not a clamp but a
/// **panic**, because its prescaler search runs one step past the table it
/// then indexes. Offering the embassy range on an F1 would hand the user
/// 42 ms of values that crash at boot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IwdgHal {
    /// `embassy-stm32`: microseconds.
    Embassy,
    /// `stm32f1xx-hal`: MILLISECONDS, and its own arithmetic.
    Stm32f1,
}

/// What one chip family's watchdog hardware can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchdogLimits {
    /// The IWDG's clock. NOT configurable — that is the point of the
    /// "independent" watchdog, and why its range never moves with the Clock tab.
    pub lsi_hz: u32,
    /// Largest IWDG prescaler divisor.
    pub iwdg_max_prescaler: u32,
    /// Largest WWDG prescaler multiplier.
    pub wwdg_max_prescaler: u64,
    /// `false` on families whose HAL has no window watchdog at all — see
    /// [`wwdg_supported`].
    pub wwdg_available: bool,
    /// Whose arithmetic decides the IWDG range — see [`IwdgHal`].
    pub iwdg_hal: IwdgHal,
}

/// Does this project's HAL expose a WWDG driver?
///
/// `stm32f1xx-hal` 0.10 — the HAL the F1 uses on every runtime but Async — has
/// only `IndependentWatchdog`. embassy-stm32 has both, on the F1 too (compiled
/// on an STM32F103C8). So on an F1 off Async the Configuration group is HALF
/// live, which is exactly why the WWDG card is shown disabled with the reason
/// rather than hidden: a control that silently vanishes on one chip is harder
/// to understand than one that explains itself.
pub fn wwdg_supported(family: &str, runtime: crate::panels::mcu_module::mcu::Runtime) -> bool {
    !crate::panels::mcu_module::codegen::family::uses_stm32f1xx_hal(family, runtime)
}

/// Is watchdog code actually GENERATED for this family yet?
///
/// Separate from [`wwdg_supported`], which is about the HAL. This is about
/// the IDE: every STM32 backend calls `watchdog_gen`, and so do both ESP, both
/// RP and both nRF ones.
///
/// Kept as a function rather than folded away because it is what stops the
/// tab offering controls that reach no generated file, and the next family
/// added to the IDE starts out on the wrong side of it.
pub fn codegen_supported(family: &str) -> bool {
    family.starts_with("stm32") || is_esp(family) || is_rp(family) || is_nrf(family)
}

/// An nRF52 family key (`nrf52833`) - the nRF backend's own test.
pub fn is_nrf(family: &str) -> bool {
    crate::panels::mcu_module::codegen::nrf::is_nrf(family)
}

/// An ESP32 family key. They are CHIP ids (`esp32c3`), not series.
pub fn is_esp(family: &str) -> bool {
    family.starts_with("esp32")
}

/// An RP family key (`rp2040`, `rp235x`) - the RP backend's own test, so the
/// two cannot disagree about which boards are Picos.
pub fn is_rp(family: &str) -> bool {
    crate::panels::mcu_module::codegen::rp::is_rp(family)
}

/// The watchdog limits for an IDE family key (`stm32f4`, `stm32g0`, …).
///
/// An unknown family gets the conservative common case (32 kHz LSI, /256, WWDG
/// /8): every value it then offers is valid on any STM32, so a chip this does
/// not know about is under-served rather than mis-served.
///
/// `runtime` decides the HAL, which is what the last two fields are about: an
/// F1 on Async is on embassy-stm32, with its window watchdog and its IWDG
/// arithmetic. The LSI and the prescalers are the silicon's either way.
pub fn limits_for(
    family: &str,
    runtime: crate::panels::mcu_module::mcu::Runtime,
) -> WatchdogLimits {
    WatchdogLimits {
        // 40 kHz only on the three oldest families.
        lsi_hz: match family {
            "stm32f0" | "stm32f1" | "stm32f3" => 40_000,
            _ => 32_000,
        },
        // `iwdg_v3` — H5, U5, WBA — reaches /1024.
        iwdg_max_prescaler: match family {
            "stm32h5" | "stm32u5" | "stm32wba" => 1024,
            _ => 256,
        },
        // `wwdg_v2` reaches /128. Note L4 is v1, unlike its neighbours.
        wwdg_max_prescaler: match family {
            "stm32g0" | "stm32g4" | "stm32c0" | "stm32h5" | "stm32u5" | "stm32wba" => 128,
            _ => 8,
        },
        wwdg_available: wwdg_supported(family, runtime),
        iwdg_hal: if crate::panels::mcu_module::codegen::family::uses_stm32f1xx_hal(family, runtime)
        {
            IwdgHal::Stm32f1
        } else {
            IwdgHal::Embassy
        },
    }
}

/// embassy's `get_timeout_us`, integer division included.
///
/// Reproduced rather than approximated: `lsi_hz / prescaler` truncates, and at
/// /1024 on a 32 kHz LSI that is 31 Hz, not 31.25 — a 1 % difference in the
/// advertised maximum. Showing a range the driver would reject is the failure
/// this whole module exists to avoid.
fn iwdg_timeout_us(lsi_hz: u32, prescaler: u32, reload: u32) -> u32 {
    let ticks_hz = (lsi_hz / prescaler).max(1);
    (1_000_000u64 * (reload as u64 + 1) / ticks_hz as u64).min(u32::MAX as u64) as u32
}

/// The IWDG timeouts this chip can express, in microseconds.
///
/// The floor is one tick at the smallest prescaler (/4); below it embassy's
/// `reload_value` computes `0 - 1` on a `u16` and panics. The ceiling is a full
/// 12-bit counter at the largest prescaler.
pub fn iwdg_range_us(l: &WatchdogLimits) -> (u32, u32) {
    match l.iwdg_hal {
        IwdgHal::Embassy => (
            iwdg_timeout_us(l.lsi_hz, 4, 0),
            iwdg_timeout_us(l.lsi_hz, l.iwdg_max_prescaler, MAX_RL),
        ),
        // `(rl + 1) * divider / LSI_kHz`, in MILLISECONDS - so the floor
        // is one whole millisecond, not one LSI tick, and the ceiling is
        // the largest divider the HAL will actually index (256).
        IwdgHal::Stm32f1 => {
            let lsi_khz = (l.lsi_hz / 1000).max(1);
            let max_ms = (MAX_RL + 1) * 256 / lsi_khz;
            (1_000, max_ms.saturating_mul(1_000))
        }
    }
}

/// The WWDG timeouts this chip can express at the given PCLK1, in microseconds.
///
/// `None` when PCLK1 is unknown (no clock model, or a graph that evaluates to
/// zero): a range computed from a guessed clock would be worse than none.
///
/// The floor is one counter tick — embassy rounds UP, so a shorter request is
/// silently stretched to it, which is not an error but is a surprise worth
/// showing. The ceiling is the 6-bit counter (64 ticks) at the top prescaler.
pub fn wwdg_range_us(l: &WatchdogLimits, pclk1_hz: u32) -> Option<(u32, u32)> {
    if pclk1_hz == 0 {
        return None;
    }
    let tick_us = WWDG_DIV * 1_000_000 / pclk1_hz as u64;
    let tick_us = tick_us.max(1);
    let max = (tick_us * 64 * l.wwdg_max_prescaler).min(u32::MAX as u64);
    Some((tick_us as u32, max as u32))
}

/// IWDG settings. `timeout_us` is the period; the watchdog resets the chip
/// unless petted within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IwdgConfig {
    pub timeout_us: u32,
}

/// WWDG settings.
///
/// `window_us` is the CLOSED window at the start of the period: petting during
/// it resets the chip just as surely as petting too late. `0` disables the
/// restriction, which is the safe default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WwdgConfig {
    pub timeout_us: u32,
    pub window_us: u32,
}

/// One ESP watchdog: a period, and nothing else.
///
/// No window (the ESP has none) and no expiry action. The action is left at
/// what `enable()` writes — stage 0 resets the system, stages 1..3 off — for
/// two different reasons per watchdog:
///
/// * MWDT: `set_stage_action` is documented as taking effect **only under a
///   custom bootloader** with `ESP_TASK_WDT_EN` and `ESP_INT_WDT` disabled. A
///   control that silently does nothing on a stock build is worse than none.
/// * RWDT: it works, but the useful non-reset action is `Interrupt`, which
///   needs a handler bound — a Pins-canvas concern, not a duration field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EspWdtConfig {
    pub timeout_us: u32,
}

/// What one ESP chip's watchdogs can express.
///
/// Far less than [`WatchdogLimits`] carries, and deliberately: on the STM32 an
/// out-of-range duration is a **boot panic**, so that struct exists to prevent
/// one. esp-hal's `set_timeout` cannot fail — it computes ticks and writes
/// them. So the only question left here is the FLOOR, which is one tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EspWatchdogLimits {
    /// Nominal RTC slow clock. NOT a fixed number in silicon: it is an RC
    /// oscillator that esp-hal CALIBRATES at boot (`rtc_slow_cal_period`), so
    /// this is what the part is specified at, not what a given die will run at.
    /// It is used for one thing — the shortest RWDT period worth offering.
    pub rtc_slow_hz: u32,
    /// `Rwdt::set_timeout` shifts the tick count right by `1 + efuse
    /// multiplier` on every part except the original ESP32. The efuse half is
    /// read at runtime and is 0 on stock parts, so this is the shift alone.
    pub rwdt_shift: u32,
    /// The ESP32-C2 has one timer group; every other part has two.
    pub has_mwdt1: bool,
}

/// Nominal RC_SLOW per bundled chip, from `rc_slow_clk_frequency` in
/// `esp-metadata-generated` 0.4.0 — the version esp-hal 1.1.2 pins.
///
/// Read, not assumed: until 2026-09-21 this was "150 kHz on the ESP32, 136
/// everywhere else", and three parts are not 136. The S2 runs at 90 kHz, so its
/// RWDT floor was offered at 16 us where one written tick needs 24.
const RC_SLOW_HZ: &[(&str, u32)] = &[
    ("esp32", 150_000),
    ("esp32c2", 136_000),
    ("esp32c3", 136_000),
    ("esp32c5", 130_000),
    ("esp32c6", 136_000),
    ("esp32c61", 136_000),
    ("esp32h2", 130_000),
    ("esp32s2", 90_000),
    ("esp32s3", 136_000),
];

/// This chip's RC_SLOW. A chip missing from [`RC_SLOW_HZ`] gets the SLOWEST
/// one, whose RWDT floor is the highest: every period it then offers is
/// writable on any part in the table.
fn rc_slow_hz(chip: &str) -> u32 {
    RC_SLOW_HZ
        .iter()
        .find(|&&(c, _)| c == chip)
        .or_else(|| RC_SLOW_HZ.iter().min_by_key(|&&(_, hz)| hz))
        .map_or(90_000, |&(_, hz)| hz)
}

/// The watchdog limits for an ESP chip id (`esp32c3`, `esp32s3`, …).
pub fn esp_limits_for(chip: &str) -> EspWatchdogLimits {
    EspWatchdogLimits {
        rtc_slow_hz: rc_slow_hz(chip),
        rwdt_shift: u32::from(chip != "esp32"),
        has_mwdt1: chip != "esp32c2",
    }
}

/// The RWDT periods worth offering, in microseconds.
///
/// The floor is one RTC tick AFTER the shift: below it the `hold` register
/// takes 0 and the stage is degenerate. The ceiling is `u32::MAX` because that
/// is the tab's own unit running out, not the chip: at 136 kHz a full u32 of
/// microseconds is 5.8e8 ticks, and the register holds 32 bits of them.
pub fn rwdt_range_us(l: &EspWatchdogLimits) -> (u32, u32) {
    let per_tick_us = 1_000_000u64.div_ceil(l.rtc_slow_hz.max(1) as u64);
    ((per_tick_us << l.rwdt_shift).max(1) as u32, u32::MAX)
}

/// The RP watchdog: a period, and nothing else.
///
/// No window and no expiry action - the RP2040 and RP2350 watchdog does one
/// thing when it runs out, a reset.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpWdtConfig {
    pub timeout_us: u32,
}

impl RpWdtConfig {
    /// One second. NOT the longest period, unlike [`IwdgConfig::default_for`]:
    /// the ceiling here moves with the RUNTIME, so a longest-for-Async default
    /// would become a boot panic the moment the project switched to Blocking.
    /// One second is inside every chip's range on every runtime.
    pub fn default_for() -> Self {
        Self {
            timeout_us: 1_000_000,
        }
    }
}

/// The RP watchdog periods the driver accepts, in microseconds.
///
/// The ceiling belongs to the DRIVER, not the 24-bit counter, and one step past
/// it is a **panic at boot** in all three:
///
/// * `rp2040-hal` 0.12 and `rp235x-hal` 0.4: `start()` refuses anything above
///   `0xFFFFFF / 2`. The halving is the RP2040's count-by-two erratum
///   (RP2040-E1), and rp235x-hal kept the check although it loads the period
///   undoubled - so an RP2350 on Blocking stops at 8.39 s as well.
/// * `embassy-rp` 0.10: `feed()`, which `start()` calls, stops at
///   `0xFFFFFF / 2` on the RP2040 and at `0xFFFFFF` on the RP2350.
///
/// So the same chip has two ceilings, and a Runtime switch can put a stored
/// period out of range. The floor is one tick of the 1 us watchdog clock.
pub fn rp_range_us(family: &str, is_async: bool) -> (u32, u32) {
    let max = if is_async && family == "rp235x" {
        0xFF_FFFF
    } else {
        0xFF_FFFF / 2
    };
    (1, max)
}

/// The crate whose limit [`rp_range_us`] reproduces, for messages that point
/// at the code that would panic.
pub fn rp_driver(family: &str, is_async: bool) -> &'static str {
    match (is_async, family == "rp2040") {
        (true, _) => "embassy-rp",
        (false, true) => "rp2040-hal",
        (false, false) => "rp235x-hal",
    }
}

/// Why an RP period cannot be built, or `None` when it is fine.
///
/// The driver would panic at boot; the generated file's `const` assert stops
/// the BUILD first. Typically reached through a Runtime switch, which lowers
/// the RP2350's ceiling under a period that was valid on Async.
pub fn rp_wdt_problem(cfg: &RpWdtConfig, family: &str, is_async: bool) -> Option<String> {
    let (lo, hi) = rp_range_us(family, is_async);
    (!(lo..=hi).contains(&cfg.timeout_us)).then(|| {
        format!(
            "{} is outside the range {}..={} us that {} accepts on this runtime - the build \
             will stop on it, since the driver would panic at boot",
            cfg.timeout_us,
            lo,
            hi,
            rp_driver(family, is_async)
        )
    })
}

/// The nRF52 WDT: a period, and nothing else.
///
/// It counts the 32.768 kHz LFCLK and, once started, cannot be stopped by
/// anything short of a reset that clears it - which a soft reset is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NrfWdtConfig {
    pub timeout_us: u32,
}

impl NrfWdtConfig {
    /// One second, like the ESP and RP ones: every value in the range is
    /// expressible, so the default can be the useful one.
    pub fn default_for() -> Self {
        Self {
            timeout_us: 1_000_000,
        }
    }
}

/// The clock the nRF52 WDT counts, on every part: the LFCLK.
pub const NRF_LFCLK_HZ: u64 = 32_768;

/// A period in microseconds as the WDT's CRV takes it: LFCLK ticks, rounded
/// UP, so the watchdog never bites sooner than the period asked for. Both
/// nrf-hal's `set_lfosc_ticks` and embassy-nrf's `timeout_ticks` take this.
pub fn nrf_ticks(timeout_us: u32) -> u32 {
    (timeout_us as u64 * NRF_LFCLK_HZ).div_ceil(1_000_000) as u32
}

/// The fewest ticks the WDT is given: both HALs raise anything shorter to it,
/// SILENTLY (`max(15)` in nrf-hal's `set_lfosc_ticks`, `MIN_TICKS` in
/// embassy-nrf).
pub const NRF_MIN_TICKS: u32 = 15;

/// The nRF52 WDT periods worth offering, in microseconds.
///
/// Unlike the RP and STM32 ranges, neither end is a panic. The floor is the
/// shortest period that rounds UP to [`NRF_MIN_TICKS`] - 428 us, since 15
/// ticks are 457.8 us and [`nrf_ticks`] rounds up. Below it the HAL would run
/// 15 ticks while the tab showed less. (It was 458 us - the HALs' own number -
/// which [`nrf_ticks`] turns into 16 ticks, so the real minimum could not be
/// set at all.) The ceiling is the tab's `u32` of microseconds: 71.6 minutes
/// is 1.4e8 ticks, and the CRV register holds 32 bits of them (36 hours).
pub fn nrf_range_us() -> (u32, u32) {
    // us * HZ / 1e6 must exceed MIN - 1 ticks for the ceiling to reach MIN.
    let floor = (NRF_MIN_TICKS as u64 - 1) * 1_000_000 / NRF_LFCLK_HZ + 1;
    (floor as u32, u32::MAX)
}

/// Why an nRF period would not be the one the chip runs, or `None`.
pub fn nrf_wdt_problem(cfg: &NrfWdtConfig) -> Option<String> {
    let (lo, hi) = nrf_range_us();
    (!(lo..=hi).contains(&cfg.timeout_us)).then(|| {
        format!(
            "{} us is below the WDT's {NRF_MIN_TICKS}-tick minimum (anything under {lo} us) - \
             the HAL would quietly run {NRF_MIN_TICKS} ticks, 457.8 us, instead",
            cfg.timeout_us
        )
    })
}

/// The MWDT periods worth offering, in microseconds.
///
/// One flat range for every part, and that is not a simplification: the tick
/// count is `micros * clock_MHz`, so any clock of at least 1 MHz reaches one
/// tick within one microsecond — and the MWDT is clocked from APB or the
/// crystal, never below 32 MHz on these parts. The ceiling is `u32::MAX` for
/// the same reason as the RWDT's: a full u32 of microseconds at 80 MHz needs
/// prescaler 80 out of the 65535 available.
pub fn mwdt_range_us() -> (u32, u32) {
    (1, u32::MAX)
}

/// The STM32 pair, the ESP three, the RP one and the nRF one. Each `None` until
/// switched on.
///
/// One struct for every family rather than an enum: the tab reaches for the
/// two fields its family uses and the generator ignores the rest, so carrying
/// a configuration across a chip change loses nothing that can be kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchdogSettings {
    pub iwdg: Option<IwdgConfig>,
    pub wwdg: Option<WwdgConfig>,
    /// ESP: the RTC watchdog, in the RTC power domain.
    #[serde(default)]
    pub rwdt: Option<EspWdtConfig>,
    /// ESP: timer group 0's watchdog.
    #[serde(default)]
    pub mwdt0: Option<EspWdtConfig>,
    /// ESP: timer group 1's watchdog — absent on the ESP32-C2.
    #[serde(default)]
    pub mwdt1: Option<EspWdtConfig>,
    /// RP2040 / RP2350: the chip's one watchdog.
    #[serde(default)]
    pub rp: Option<RpWdtConfig>,
    /// nRF52: the WDT.
    #[serde(default)]
    pub nrf: Option<NrfWdtConfig>,
}

impl EspWdtConfig {
    /// The default a freshly ticked box gets: one second.
    ///
    /// NOT the longest period, unlike [`IwdgConfig::default_for`]. That default
    /// exists because on an STM32 the longest value is the only one certain to
    /// be in range; here every value in the range is expressible, so the
    /// default can be the one that is actually useful.
    pub fn default_for() -> Self {
        Self {
            timeout_us: 1_000_000,
        }
    }
}

/// Why an ESP period would not do what it says, or `None` when it is fine.
pub fn esp_wdt_problem(cfg: &EspWdtConfig, range: (u32, u32), what: &str) -> Option<String> {
    let (lo, hi) = range;
    (!(lo..=hi).contains(&cfg.timeout_us)).then(|| {
        format!(
            "{} is outside the {what} range {}..={} us",
            cfg.timeout_us, lo, hi
        )
    })
}

impl IwdgConfig {
    /// The default the Reset button restores: the LONGEST period the chip can
    /// express.
    ///
    /// Longest, not some middle value, because it is the least aggressive
    /// setting and the only one guaranteed to be in range on every chip — a
    /// "restore defaults" that produced a boot panic would be its own bug.
    pub fn default_for(l: &WatchdogLimits) -> Self {
        Self {
            timeout_us: iwdg_range_us(l).1,
        }
    }
}

impl WwdgConfig {
    /// Longest period, window disabled — see [`IwdgConfig::default_for`].
    ///
    /// `None` when PCLK1 is unknown, because every WWDG duration is relative to
    /// it. The tab then offers no default rather than a fabricated one.
    pub fn default_for(l: &WatchdogLimits, pclk1_hz: u32) -> Option<Self> {
        let (_, max) = wwdg_range_us(l, pclk1_hz)?;
        Some(Self {
            timeout_us: max,
            window_us: 0,
        })
    }
}

/// Why a setting would not survive to run, or `None` when it is fine.
///
/// Every message names the value AND the bound, because "out of range" without
/// the range is a dead end for the person reading it.
pub fn iwdg_problem(cfg: &IwdgConfig, l: &WatchdogLimits) -> Option<String> {
    let (lo, hi) = iwdg_range_us(l);
    (!(lo..=hi).contains(&cfg.timeout_us)).then(|| {
        format!(
            "{} is outside this chip's IWDG range {}..={} us (LSI {} kHz, prescaler up to /{})",
            cfg.timeout_us,
            lo,
            hi,
            l.lsi_hz / 1000,
            l.iwdg_max_prescaler
        )
    })
}

/// The same for the WWDG, which has two ways to fail instead of one.
pub fn wwdg_problem(cfg: &WwdgConfig, l: &WatchdogLimits, pclk1_hz: u32) -> Option<String> {
    // Checked first: it is the one that holds no matter what the clock is, and
    // it is an `assert!` in the driver rather than an `unwrap!`.
    if cfg.window_us >= cfg.timeout_us {
        return Some(format!(
            "the closed window ({} us) must be shorter than the period ({} us)",
            cfg.window_us, cfg.timeout_us
        ));
    }
    let Some((lo, hi)) = wwdg_range_us(l, pclk1_hz) else {
        return Some(
            "PCLK1 is unknown, so the achievable range cannot be checked - set the Clock tab first"
                .to_owned(),
        );
    };
    (!(lo..=hi).contains(&cfg.timeout_us)).then(|| {
        format!(
            "{} is outside this chip's WWDG range {}..={} us at PCLK1 {} MHz (prescaler up to /{})",
            cfg.timeout_us,
            lo,
            hi,
            pclk1_hz / 1_000_000,
            l.wwdg_max_prescaler
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::mcu::Runtime;

    #[test]
    fn the_lsi_frequency_is_per_family() {
        // Getting this wrong scales every IWDG duration by 25 %.
        assert_eq!(limits_for("stm32f1", Runtime::Blocking).lsi_hz, 40_000);
        assert_eq!(limits_for("stm32f3", Runtime::Blocking).lsi_hz, 40_000);
        assert_eq!(limits_for("stm32f4", Runtime::Blocking).lsi_hz, 32_000);
        assert_eq!(limits_for("stm32g4", Runtime::Blocking).lsi_hz, 32_000);
    }

    #[test]
    fn l4_has_the_older_window_watchdog() {
        // The one a glance at the family list would have got wrong: L4 sits
        // among the v2 families but its WWDG is v1, so its longest period is
        // sixteen times shorter than G4's.
        assert_eq!(
            limits_for("stm32l4", Runtime::Blocking).wwdg_max_prescaler,
            8
        );
        assert_eq!(
            limits_for("stm32g4", Runtime::Blocking).wwdg_max_prescaler,
            128
        );
    }

    #[test]
    fn the_iwdg_range_matches_embassys_own_arithmetic() {
        // 32 kHz LSI, /256, full 12-bit reload: 1e6 * 4096 / (32000/256) = 32.768 s.
        let l = limits_for("stm32f4", Runtime::Blocking);
        assert_eq!(iwdg_range_us(&l), (125, 32_768_000));
    }

    /// The F1 does NOT use this formula, and assuming it did was a real
    /// mistake caught only by reading `stm32f1xx-hal`.
    #[test]
    fn f1_uses_its_own_hals_arithmetic_not_embassys() {
        let f1 = limits_for("stm32f1", Runtime::Blocking);
        assert_eq!(f1.iwdg_hal, IwdgHal::Stm32f1);
        // `4096 * 256 / 40 kHz` = 26_214 ms. Applying embassy's formula to
        // the same chip gives 26_256_410 us - 42 ms MORE, every one of
        // which panics, because the HAL's prescaler search steps past the
        // table it then indexes rather than clamping.
        assert_eq!(iwdg_range_us(&f1).1, 26_214_000);
        assert!(
            iwdg_timeout_us(f1.lsi_hz, f1.iwdg_max_prescaler, MAX_RL) > iwdg_range_us(&f1).1,
            "the embassy formula must be the WIDER one - that is the trap",
        );
        // Milliseconds are the unit, so the floor is 1 ms and not a tick.
        assert_eq!(iwdg_range_us(&f1).0, 1_000);
    }

    #[test]
    fn the_v3_prescaler_uses_truncating_division_like_the_driver() {
        // 32000 / 1024 = 31.25, and embassy truncates to 31 - so the real
        // maximum is 132.1 s, not the 134.2 s exact arithmetic would suggest.
        // Advertising the larger number would put the driver's `unwrap!` one
        // step past the end of our own range.
        let l = limits_for("stm32u5", Runtime::Blocking);
        assert_eq!(l.iwdg_max_prescaler, 1024);
        assert_eq!(iwdg_range_us(&l).1, 132_129_032);
    }

    #[test]
    fn the_wwdg_range_moves_with_pclk1() {
        let l = limits_for("stm32f4", Runtime::Blocking);
        // 4096 / 50 MHz = 81.92 us per tick, truncated to 81; 64 ticks x /8.
        let (lo, hi) = wwdg_range_us(&l, 50_000_000).unwrap();
        assert_eq!(lo, 81);
        assert_eq!(hi, 81 * 64 * 8);
        // Halving the clock roughly doubles every duration - which is why a
        // stored value has to be re-checked when the Clock tab changes. Only
        // ROUGHLY: 4096/25 MHz truncates to 163, not 2 x 81, so the range does
        // not scale exactly and cannot be rescaled arithmetically when the
        // clock moves. It has to be recomputed.
        let (lo2, hi2) = wwdg_range_us(&l, 25_000_000).unwrap();
        assert_eq!(lo2, 163);
        assert_eq!(hi2, 163 * 64 * 8);
        assert!(lo2 > lo && lo2 != lo * 2, "truncation, not proportion");
    }

    #[test]
    fn an_unknown_clock_yields_no_range_rather_than_a_guess() {
        assert_eq!(
            wwdg_range_us(&limits_for("stm32f4", Runtime::Blocking), 0),
            None
        );
        assert_eq!(
            WwdgConfig::default_for(&limits_for("stm32f4", Runtime::Blocking), 0),
            None
        );
    }

    #[test]
    fn the_defaults_are_always_inside_the_range() {
        // The property that matters for the Reset button: on every family it
        // can be pressed, it must produce something the driver accepts.
        for fam in [
            "stm32f1",
            "stm32f2",
            "stm32f4",
            "stm32f7",
            "stm32g0",
            "stm32g4",
            "stm32l4",
            "stm32u5",
            "stm32wba",
            "stm32c0",
            "unknown-family",
        ] {
            let l = limits_for(fam, Runtime::Blocking);
            assert_eq!(
                iwdg_problem(&IwdgConfig::default_for(&l), &l),
                None,
                "{fam}"
            );
            for pclk1 in [8_000_000, 50_000_000, 170_000_000] {
                let Some(w) = WwdgConfig::default_for(&l, pclk1) else {
                    continue;
                };
                assert_eq!(wwdg_problem(&w, &l, pclk1), None, "{fam} @ {pclk1}");
            }
        }
    }

    #[test]
    fn the_window_must_be_shorter_than_the_period() {
        let l = limits_for("stm32f4", Runtime::Blocking);
        let bad = WwdgConfig {
            timeout_us: 1000,
            window_us: 1000,
        };
        // Equal is not allowed either - the driver asserts strictly less.
        let msg = wwdg_problem(&bad, &l, 50_000_000).expect("equal must be refused");
        assert!(msg.contains("shorter than"), "{msg}");
    }

    #[test]
    fn problems_name_the_bound_they_broke() {
        let l = limits_for("stm32f4", Runtime::Blocking);
        let msg = iwdg_problem(&IwdgConfig { timeout_us: 1 }, &l).expect("1 us is too short");
        assert!(
            msg.contains("125"),
            "the message must state the range: {msg}"
        );
    }

    #[test]
    fn f1_is_the_family_without_a_window_watchdog() {
        assert!(!wwdg_supported("stm32f1", Runtime::Blocking));
        assert!(!limits_for("stm32f1", Runtime::Blocking).wwdg_available);
        assert!(wwdg_supported("stm32f4", Runtime::Blocking));
        assert!(limits_for("stm32g0", Runtime::Blocking).wwdg_available);
    }

    #[test]
    fn the_codegen_gate_matches_the_backend_that_implements_it() {
        // `StmEmbassyBackend::handles` is the real rule; if it ever widens,
        // this fails rather than letting the tab go quietly dead.
        for fam in [
            "stm32f2", "stm32f4", "stm32f7", "stm32g0", "stm32g4", "stm32l4",
        ] {
            assert!(codegen_supported(fam), "{fam}");
        }
        // F1 and WBA joined once their backends called the generator.
        assert!(codegen_supported("stm32f1"));
        assert!(codegen_supported("stm32wba"));
        // The ESPs joined last, on their own peripherals: RWDT + MWDT, not
        // IWDG + WWDG. This used to assert the opposite.
        for chip in [
            "esp32", "esp32c2", "esp32c3", "esp32c6", "esp32h2", "esp32s3",
        ] {
            assert!(codegen_supported(chip), "{chip}");
            assert!(is_esp(chip), "{chip}");
        }
        assert!(!is_esp("stm32f4"));
        // Both RP keys, on both runtimes' backends. This used to assert the
        // opposite too.
        for fam in ["rp2040", "rp235x"] {
            assert!(codegen_supported(fam), "{fam}");
            assert!(is_rp(fam), "{fam}");
        }
        // And the nRF52s, last. This asserted the opposite until 2026-09-21.
        assert!(codegen_supported("nrf52833") && is_nrf("nrf52833"));
        // A family with no watchdog backend still stays out.
        assert!(!codegen_supported("stm8s"));
    }

    /// The CRV counts LFCLK ticks, and the conversion rounds UP: a watchdog
    /// must never bite sooner than the period the user typed.
    #[test]
    fn the_nrf_period_becomes_lfclk_ticks_rounded_up() {
        assert_eq!(nrf_ticks(1_000_000), 32_768);
        // 15 ticks is 457.76 us; 458 is the first whole microsecond past it.
        assert_eq!(nrf_ticks(457), 15);
        assert_eq!(nrf_ticks(458), 16);
        // One microsecond is a fraction of a tick, and still a whole one.
        assert_eq!(nrf_ticks(1), 1);
        // The tab's ceiling fits the 32-bit CRV with room to spare.
        assert_eq!(nrf_ticks(u32::MAX), 140_737_489);
    }

    /// Below 15 ticks both HALs substitute 15 without a word, so the floor is
    /// where the period stops being the one the chip runs - not a panic.
    #[test]
    fn the_nrf_floor_is_the_hals_silent_minimum() {
        let (lo, hi) = nrf_range_us();
        assert_eq!((lo, hi), (428, u32::MAX));
        // The floor IS the minimum: it rounds up to exactly 15 ticks, and one
        // microsecond less would be the 14 the HAL quietly raises.
        assert_eq!(nrf_ticks(lo), NRF_MIN_TICKS);
        assert_eq!(nrf_ticks(lo - 1), NRF_MIN_TICKS - 1);
        assert_eq!(nrf_wdt_problem(&NrfWdtConfig::default_for()), None);
        assert_eq!(nrf_wdt_problem(&NrfWdtConfig { timeout_us: lo }), None);
        let msg = nrf_wdt_problem(&NrfWdtConfig { timeout_us: 100 }).expect("below 15 ticks");
        assert!(msg.contains("428") && msg.contains("15 ticks"), "{msg}");
    }

    /// The ceiling is the DRIVER's, and the RP2350 has two: rp235x-hal kept the
    /// RP2040's halved check, embassy-rp did not. Each number is the one past
    /// which that crate panics at boot.
    #[test]
    fn the_rp_ceiling_follows_the_driver_not_the_counter() {
        assert_eq!(rp_range_us("rp2040", false), (1, 8_388_607));
        assert_eq!(rp_range_us("rp235x", false), (1, 8_388_607));
        assert_eq!(rp_range_us("rp2040", true), (1, 8_388_607));
        assert_eq!(rp_range_us("rp235x", true), (1, 16_777_215));
        assert_eq!(rp_driver("rp2040", false), "rp2040-hal");
        assert_eq!(rp_driver("rp235x", false), "rp235x-hal");
        assert_eq!(rp_driver("rp235x", true), "embassy-rp");
    }

    /// The default must survive a Runtime switch, and the longest Async period
    /// on an RP2350 must not: that is the trap a longest-period default would
    /// set.
    #[test]
    fn the_rp_default_holds_on_every_runtime_and_the_async_maximum_does_not() {
        for fam in ["rp2040", "rp235x"] {
            for is_async in [false, true] {
                assert_eq!(
                    rp_wdt_problem(&RpWdtConfig::default_for(), fam, is_async),
                    None,
                    "{fam} async={is_async}"
                );
            }
        }
        let long = RpWdtConfig {
            timeout_us: 16_777_215,
        };
        assert_eq!(rp_wdt_problem(&long, "rp235x", true), None);
        let msg = rp_wdt_problem(&long, "rp235x", false).expect("past rp235x-hal's check");
        assert!(
            msg.contains("rp235x-hal") && msg.contains("8388607"),
            "{msg}"
        );
        assert!(rp_wdt_problem(&RpWdtConfig { timeout_us: 0 }, "rp2040", false).is_some());
    }

    /// The ESP32-C2 is the one part with a single timer group, so it is the
    /// one that would generate a `TIMG1` that does not exist.
    #[test]
    fn only_the_c2_lacks_the_second_timer_group() {
        assert!(!esp_limits_for("esp32c2").has_mwdt1);
        for chip in [
            "esp32", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2", "esp32s2", "esp32s3",
        ] {
            assert!(esp_limits_for(chip).has_mwdt1, "{chip}");
        }
    }

    /// The RWDT floor is one tick AFTER the shift esp-hal applies, and the
    /// original ESP32 is the only part without that shift.
    #[test]
    fn the_rwdt_floor_follows_the_shift_and_the_rc_clock() {
        // 150 kHz, no shift: one tick is 7 us.
        assert_eq!(rwdt_range_us(&esp_limits_for("esp32")).0, 7);
        // 136 kHz, shifted by one: 8 us per tick, two ticks per write.
        assert_eq!(rwdt_range_us(&esp_limits_for("esp32c3")).0, 16);
        // 90 kHz: 12 us per tick, so 24. It was offered at 16 while the table
        // said 136 kHz for the S2 - and below 24 the stage is written as 0.
        assert_eq!(rwdt_range_us(&esp_limits_for("esp32s2")).0, 24);
        // 130 kHz still rounds to 8 us per tick: only the card's text moved.
        assert_eq!(rwdt_range_us(&esp_limits_for("esp32c5")).0, 16);
        // The ceiling belongs to the tab's `u32` of microseconds, not to the
        // chip: at 136 kHz a full u32 of us is 5.8e8 ticks in a 32-bit hold.
        assert_eq!(rwdt_range_us(&esp_limits_for("esp32c3")).1, u32::MAX);
        assert_eq!(mwdt_range_us(), (1, u32::MAX));
    }

    /// Each part's RC_SLOW as esp-hal's own metadata states it, written out
    /// here rather than read back from [`RC_SLOW_HZ`] so the two are an
    /// independent record: `rc_slow_clk_frequency` in
    /// `esp-metadata-generated` 0.4.0, the version esp-hal 1.1.2 pins.
    #[test]
    fn the_rtc_slow_clock_is_per_chip() {
        for (chip, hz) in [
            ("esp32", 150_000),
            ("esp32c2", 136_000),
            ("esp32c3", 136_000),
            ("esp32c5", 130_000),
            ("esp32c6", 136_000),
            ("esp32c61", 136_000),
            ("esp32h2", 130_000),
            ("esp32s2", 90_000),
            ("esp32s3", 136_000),
        ] {
            assert_eq!(esp_limits_for(chip).rtc_slow_hz, hz, "{chip}");
        }
        // A part the table does not know gets the slowest clock, so the floor
        // it is offered is the highest any known part needs.
        assert_eq!(esp_limits_for("esp32p4").rtc_slow_hz, 90_000);
    }

    /// A new bundled ESP chip must have its RC_SLOW looked up, not inherit the
    /// fallback: the fallback keeps the floor safe but prints a clock on the
    /// card that is not the chip's.
    #[test]
    fn every_bundled_esp_chip_has_a_checked_rtc_slow_clock() {
        for d in crate::panels::mcu_module::builtins::builtin_definitions() {
            if is_esp(&d.family) {
                assert!(
                    RC_SLOW_HZ.iter().any(|&(c, _)| c == d.family),
                    "{}: add its rc_slow_clk_frequency (esp-metadata-generated) to RC_SLOW_HZ",
                    d.family
                );
            }
        }
    }
}
