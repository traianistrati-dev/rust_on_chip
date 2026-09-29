//! Validate a clock configuration against the chip's datasheet limits.
//!
//! Produces human-readable errors (hard limits that would hang/brick the chip)
//! and warnings (works, but probably not intended — e.g. USB clock ≠ 48 MHz).
//! The ceilings come from a per-chip [`ClockLimits`] (defaults = STM32F103);
//! the three footnotes under Figure 2 are encoded here.

use super::compute::ClockFrequencies;
use super::model::{ClockLimits, PllSrc, Stm32f1Clock, SysclkSrc};

const MHZ: u32 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClockWarning {
    pub severity: Severity,
    pub msg: String,
}

impl ClockWarning {
    fn error(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            msg: msg.into(),
        }
    }
    fn warn(msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            msg: msg.into(),
        }
    }
}

/// Round Hz to one decimal of MHz for messages, e.g. 36_000_000 → "36.0".
fn mhz(hz: u32) -> String {
    format!("{:.1}", hz as f64 / MHZ as f64)
}

/// Collect all validation findings for `c` / `f` against the chip limits `l`.
pub fn warnings(c: &Stm32f1Clock, f: &ClockFrequencies, l: &ClockLimits) -> Vec<ClockWarning> {
    let mut out = Vec::new();

    // ── HSE crystal range ────────────────────────────────────────────────────
    if c.hse_enabled && (c.hse_hz < l.hse_min_hz || c.hse_hz > l.hse_max_hz) {
        out.push(ClockWarning::warn(format!(
            "HSE crystal {} MHz is outside the {}–{} MHz range.",
            mhz(c.hse_hz),
            mhz(l.hse_min_hz),
            mhz(l.hse_max_hz)
        )));
    }

    // ── Selected source actually running? ────────────────────────────────────
    let uses_pll = c.sysclk_src == SysclkSrc::Pll;
    if (c.sysclk_src == SysclkSrc::Hse || uses_pll && pll_needs_hse(c)) && !c.hse_enabled {
        out.push(ClockWarning::error(
            "Selected clock path needs HSE, but HSE is disabled.".to_string(),
        ));
    }

    // ── Hard frequency ceilings ──────────────────────────────────────────────
    if f.sysclk > l.sysclk_max {
        out.push(ClockWarning::error(format!(
            "SYSCLK {} MHz exceeds the {} MHz maximum.",
            mhz(f.sysclk),
            mhz(l.sysclk_max)
        )));
    }
    // Footnote 1: HSI→PLL caps the system clock below the HSE-fed maximum.
    if uses_pll && c.pll_src == PllSrc::HsiDiv2 && f.sysclk > l.sysclk_max_hsi_pll {
        out.push(ClockWarning::error(format!(
            "With HSI as PLL input, SYSCLK is limited to {} MHz (got {} MHz).",
            mhz(l.sysclk_max_hsi_pll),
            mhz(f.sysclk)
        )));
    }
    if f.pllclk > l.sysclk_max {
        out.push(ClockWarning::error(format!(
            "PLL output {} MHz exceeds the {} MHz maximum.",
            mhz(f.pllclk),
            mhz(l.sysclk_max)
        )));
    }
    // The floor of the same range (datasheet: PLL output 16..72 MHz). Not
    // academic: embassy-stm32 asserts it at init, so on the Async runtime a
    // slower PLL is a panic at boot. A floor, not a ceiling - so it does not
    // start "PLL output", the prefix that marks the ceiling above.
    if uses_pll && f.pllclk < 16_000_000 {
        out.push(ClockWarning::error(format!(
            "The PLL runs at {} MHz, below its 16 MHz minimum.",
            mhz(f.pllclk)
        )));
    }
    if f.hclk > l.hclk_max {
        out.push(ClockWarning::error(format!(
            "HCLK {} MHz exceeds the {} MHz maximum.",
            mhz(f.hclk),
            mhz(l.hclk_max)
        )));
    }
    if f.pclk1 > l.pclk1_max {
        out.push(ClockWarning::error(format!(
            "PCLK1 {} MHz exceeds the {} MHz APB1 maximum.",
            mhz(f.pclk1),
            mhz(l.pclk1_max)
        )));
    }
    if f.pclk2 > l.pclk2_max {
        out.push(ClockWarning::error(format!(
            "PCLK2 {} MHz exceeds the {} MHz APB2 maximum.",
            mhz(f.pclk2),
            mhz(l.pclk2_max)
        )));
    }
    if f.adcclk > l.adcclk_max {
        out.push(ClockWarning::error(format!(
            "ADCCLK {} MHz exceeds the {} MHz maximum.",
            mhz(f.adcclk),
            mhz(l.adcclk_max)
        )));
    }

    // ── Footnote 3: 1 µs ADC conversion needs APB2 ∈ {14, 28, 56} MHz ─────────
    if f.pclk2 != 14 * MHZ && f.pclk2 != 28 * MHZ && f.pclk2 != 56 * MHZ {
        out.push(ClockWarning::warn(format!(
            "For a 1 µs ADC conversion, APB2 (PCLK2) should be 14, 28 or 56 MHz (got {} MHz).",
            mhz(f.pclk2)
        )));
    }

    // ── Footnote 2: USB requires HSE + PLL with USBCLK = 48 MHz ───────────────
    if !uses_pll {
        out.push(ClockWarning::warn(
            "USB is unavailable: it requires the PLL to be the SYSCLK source.".to_string(),
        ));
    } else if f.usbclk != l.usbclk_hz {
        out.push(ClockWarning::warn(format!(
            "USBCLK is {} MHz; USB needs exactly {} MHz (adjust PLL or USB prescaler).",
            mhz(f.usbclk),
            mhz(l.usbclk_hz)
        )));
    }

    out
}

/// True when the PLL input ultimately comes from HSE.
fn pll_needs_hse(c: &Stm32f1Clock) -> bool {
    matches!(c.pll_src, PllSrc::Hse | PllSrc::HseDiv2)
}

/// Convenience: does the config have any hard error?
pub fn has_errors(c: &Stm32f1Clock, f: &ClockFrequencies, l: &ClockLimits) -> bool {
    warnings(c, f, l)
        .iter()
        .any(|w| w.severity == Severity::Error)
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::compute::frequencies;
    use super::super::model::{Stm32f1Clock, SysclkSrc};
    use super::*;

    fn warns(c: &Stm32f1Clock) -> Vec<ClockWarning> {
        warnings(c, &frequencies(c), &ClockLimits::default())
    }

    #[test]
    fn default_72mhz_has_no_errors() {
        let c = Stm32f1Clock::default();
        assert!(
            !has_errors(&c, &frequencies(&c), &ClockLimits::default()),
            "default config should be valid"
        );
    }

    #[test]
    fn pclk1_over_36_is_error() {
        let mut c = Stm32f1Clock::default();
        c.apb1_pre = 1; // PCLK1 = 72 MHz > 36
        assert!(
            warns(&c)
                .iter()
                .any(|w| w.severity == Severity::Error && w.msg.contains("PCLK1"))
        );
    }

    #[test]
    fn adcclk_over_14_is_error() {
        let mut c = Stm32f1Clock::default();
        c.adc_pre = 2; // 72 / 2 = 36 MHz > 14
        assert!(
            warns(&c)
                .iter()
                .any(|w| w.severity == Severity::Error && w.msg.contains("ADCCLK"))
        );
    }

    #[test]
    fn usb_not_48_is_warning() {
        let mut c = Stm32f1Clock::default();
        c.usb_pre = super::super::model::UsbPre::Div1; // 72 / 1 = 72 ≠ 48
        assert!(
            warns(&c)
                .iter()
                .any(|w| w.severity == Severity::Warning && w.msg.contains("USB"))
        );
    }

    #[test]
    fn hsi_source_warns_usb_unavailable() {
        let mut c = Stm32f1Clock::default();
        c.sysclk_src = SysclkSrc::Hsi;
        assert!(
            warns(&c)
                .iter()
                .any(|w| w.msg.contains("USB is unavailable"))
        );
    }

    #[test]
    fn hsi_pll_over_64_is_error() {
        let mut c = Stm32f1Clock::default();
        c.pll_src = super::super::model::PllSrc::HsiDiv2; // 4 MHz
        c.pll_mul = 16; // 64 MHz — OK
        assert!(!warns(&c).iter().any(|w| w.msg.contains("64 MHz")));
        // Bump APB so nothing else errors, but sysclk stays 64 — still fine.
    }

    /// A PLL under its 16 MHz floor is an error: embassy-stm32 asserts the floor
    /// at init. At exactly 16 it is fine, and a PLL nobody selects is ignored.
    #[test]
    fn a_pll_under_16_mhz_is_an_error() {
        let floor = |c: &Stm32f1Clock| warns(c).iter().any(|w| w.msg.contains("16 MHz minimum"));
        let mut c = Stm32f1Clock {
            pll_src: super::super::model::PllSrc::HsiDiv2, // 4 MHz in
            pll_mul: 2,                                    // 8 MHz out
            ..Stm32f1Clock::default()
        };
        assert!(floor(&c), "{:?}", warns(&c));
        c.pll_mul = 4; // 16 MHz out
        assert!(!floor(&c), "{:?}", warns(&c));
        c.pll_mul = 2;
        c.sysclk_src = SysclkSrc::Hsi;
        assert!(!floor(&c), "the PLL is not used");
    }

    /// Custom per-chip limits change the verdict: the default 72 MHz config is
    /// fine on an F103 but must error on a 24 MHz value-line-style part.
    #[test]
    fn custom_limits_flag_default_config() {
        let c = Stm32f1Clock::default(); // SYSCLK 72 MHz
        let f = frequencies(&c);
        let lim = ClockLimits {
            sysclk_max: 24_000_000,
            hclk_max: 24_000_000,
            pclk1_max: 24_000_000,
            pclk2_max: 24_000_000,
            ..ClockLimits::default()
        };
        assert!(!has_errors(&c, &f, &ClockLimits::default()));
        assert!(has_errors(&c, &f, &lim), "72 MHz must violate a 24 MHz cap");
        assert!(
            warnings(&c, &f, &lim)
                .iter()
                .any(|w| w.msg.contains("24.0 MHz maximum"))
        );
    }
}
