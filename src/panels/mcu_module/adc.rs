//! The ADC's settings from the Configuration tab - what no pin carries. The
//! pads are wired on the Pins canvas (`PinFunction::AdcChannel`); how long each
//! conversion samples is a register, set here.
//!
//! STM32F1 on its own HAL (Blocking, Native, RTIC) so far: stm32f1xx-hal's
//! `SampleTime`. The index is the SMPx encoding, which embassy-stm32's F1
//! `SampleTime` shares step for step, so an Async ADC can keep the setting.

use std::collections::BTreeMap;

/// Settings per ADC instance (`1` = ADC1). Only the ones that differ from the
/// default are stored, so a project that never touched the card writes no
/// `@adc` section and its `mcu.config` does not move.
pub type AdcSettings = BTreeMap<u8, AdcConfig>;

/// One ADC's settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AdcConfig {
    /// The sample time as the HAL's `SampleTime` index, 0..=7 - see
    /// [`F1_SAMPLE_CYCLES`].
    pub sample_time: u8,
}

/// stm32f1xx-hal's own (`SampleTime::T_28`): what `Adc::adc1` leaves set, and
/// so what a project gets that never touched the card.
pub const F1_SAMPLE_TIME_DEFAULT: u8 = 3;

/// The ADC clock cycles each index samples for, in the HAL's order (`T_1` to
/// `T_239`).
pub const F1_SAMPLE_CYCLES: [&str; 8] = [
    "1.5", "7.5", "13.5", "28.5", "41.5", "55.5", "71.5", "239.5",
];

/// The same, in tenths of a cycle, for arithmetic.
const F1_SAMPLE_TENTHS: [u64; 8] = [15, 75, 135, 285, 415, 555, 715, 2395];

/// The 12.5 cycles the F1 takes to convert, after sampling, in tenths.
const F1_CONVERT_TENTHS: u64 = 125;

impl Default for AdcConfig {
    fn default() -> Self {
        Self {
            sample_time: F1_SAMPLE_TIME_DEFAULT,
        }
    }
}

/// ADC `n`'s sample time index, the default where none is stored - clamped,
/// so a hand-edited `mcu.config` cannot hand the HAL a step it lacks.
pub fn f1_sample_time(settings: &AdcSettings, n: u8) -> u8 {
    settings
        .get(&n)
        .map_or(F1_SAMPLE_TIME_DEFAULT, |c| c.sample_time.min(7))
}

/// Store `sample_time` for ADC `n` - or nothing, at the default, so the
/// section stays absent for a project at the HAL's own setting.
pub fn set_f1_sample_time(settings: &mut AdcSettings, n: u8, sample_time: u8) {
    let cfg = AdcConfig {
        sample_time: sample_time.min(7),
    };
    if cfg == AdcConfig::default() {
        settings.remove(&n);
    } else {
        settings.insert(n, cfg);
    }
}

/// How long one conversion takes, in nanoseconds - the sample time plus the
/// 12.5 cycles of the conversion itself - at `adcclk_hz`. `None` without a
/// clock.
pub fn f1_conversion_ns(sample_time: u8, adcclk_hz: u32) -> Option<u64> {
    if adcclk_hz == 0 {
        return None;
    }
    let tenths = F1_SAMPLE_TENTHS[usize::from(sample_time.min(7))] + F1_CONVERT_TENTHS;
    Some(tenths * 100_000_000 / u64::from(adcclk_hz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_the_hals_and_is_not_stored() {
        let mut s = AdcSettings::new();
        assert_eq!(f1_sample_time(&s, 1), 3);
        assert_eq!(F1_SAMPLE_CYCLES[3], "28.5");
        set_f1_sample_time(&mut s, 1, 7);
        assert_eq!(f1_sample_time(&s, 1), 7);
        set_f1_sample_time(&mut s, 1, F1_SAMPLE_TIME_DEFAULT);
        assert!(s.is_empty(), "back to the default stores nothing");
        set_f1_sample_time(&mut s, 1, 42);
        assert_eq!(f1_sample_time(&s, 1), 7, "clamped to the HAL's last step");
    }

    /// 239.5 + 12.5 = 252 cycles at 12 MHz is 21 us; 28.5 + 12.5 = 41 cycles
    /// at 9 MHz is 4.56 us.
    #[test]
    fn a_conversion_takes_sample_plus_conversion_cycles() {
        assert_eq!(f1_conversion_ns(7, 12_000_000), Some(21_000));
        assert_eq!(f1_conversion_ns(3, 9_000_000), Some(4_555));
        assert_eq!(f1_conversion_ns(3, 0), None);
    }
}
