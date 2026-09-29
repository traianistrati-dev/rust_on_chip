//! What a UART really does with the baud rate its module asks for.
//!
//! The rate is a plain `u32` all the way into the generated code, which writes
//! it verbatim. So a rate the peripheral cannot make still COMPILES, and then
//! fails on the board - and how it fails is each HAL's own choice:
//!
//! | HAL | too fast | too slow |
//! |---|---|---|
//! | stm32f1xx-hal | `assert!`: panics at boot | BRR overflows its 16 bits: wrong rate, no error |
//! | embassy-stm32 on the F1 | `Err`, unwrapped: panics at boot | `Err`, unwrapped: panics at boot |
//! | rp2040-hal / rp235x-hal / embassy-rp | clamped silently | clamped silently |
//! | esp-hal | `Err` above 5 000 000, unwrapped: panics at boot | `assert!` in the divider: panics at boot |
//! | nRF UARTE | 18 fixed rates, the nearest one is taken | same |
//!
//! None of those limits belongs to the CHIP. Every one is a fraction of the
//! clock feeding the peripheral, which the Clock tab moves: "4.5 Mbit/s" is
//! USART1 on an F103 at 72 MHz and nothing else. On the same chip left on its
//! 8 MHz HSI, 921600 - one of the presets - is an assert. So the answer is
//! worked out at the CURRENT clock, by running the HAL's own divider arithmetic
//! ([`outcome`]). The range shown beside the field comes from the same function
//! ([`range`]), searched for where it stops programming the rate as asked, so
//! the two can never disagree.
//!
//! "The current clock" is the one the HAL will REALLY run, not the Clock tab's
//! drawing of it. stm32f1xx-hal derives its own dividers from the frequencies
//! it is handed (`stm32::f1_hal_pclks`), and rp-hal refuses some PLLs the tab
//! offers - both are followed here, so a verdict cannot be right about a clock
//! the board never has.
//!
//! Imported STM32 parts on embassy-stm32 are not checked yet, the F1 on Async
//! aside: nothing in the repo says which APB bus each USART instance sits on,
//! and on the newer families a per-instance clock mux decides it. They get
//! [`Plan::Unchecked`] rather than a guess.

use super::codegen::{family::is_esp, nrf, rp, stm32};
use super::mcu::{Mcu, Runtime};
use super::pins::logic::pin_function::PinFunction;

/// Within this much of the rate asked for, the link is simply right.
pub const OK_PCT: f64 = 1.0;
/// Past this, a UART loses step even with an exact peer: the receiver samples
/// each bit at its middle, and a whole frame's drift has to stay inside half a
/// bit. Between the two it works only if the other end's clock is accurate.
pub const MARGINAL_PCT: f64 = 2.5;

/// esp-hal's `Config::validate` refuses anything above this (and 0).
const ESP_MAX: u32 = 5_000_000;
/// The first and last of the nRF UARTE's fixed rates - the codegen's own table.
const NRF_RANGE: (u32, u32) = (nrf::BAUDS[0].0, nrf::BAUDS[nrf::BAUDS.len() - 1].0);

/// The divider that turns the clock into the rate, one per HAL family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divider {
    /// stm32f1xx-hal 0.10: `brr = pclk / baud` (truncating), 16x oversampling
    /// only, `assert!(brr >= 16)`, then written RAW into a 16-bit field.
    F1 { pclk: u32 },
    /// embassy-stm32 0.6 on the F1's USART (usart v1, no OVER8): `brr` is
    /// `pclk / baud` ROUNDED to nearest, and outside `16..0x10000` `configure`
    /// returns `BaudrateTooHigh` / `BaudrateTooLow`, which the generated code
    /// unwraps. Same silicon as [`Divider::F1`], different arithmetic.
    EmbassyV1 { pclk: u32 },
    /// The RP's PL011, as rp2040-hal 0.12, rp235x-hal 0.4 and embassy-rp 0.10
    /// all program it: `div = 8 * clk / baud`, 16-bit integer part plus a
    /// 6-bit fraction, clamped silently at both ends.
    Pl011 { clk: u32 },
    /// esp-hal 1.1 on the parts with a per-UART sclk divider (C2, C3, C5, C6,
    /// C61, H2, S3): a 12.4 baud divider behind an 8-bit sclk divider.
    EspSclk { clk: u32 },
    /// esp-hal 1.1 on the ESP32 and ESP32-S2: a 20.4 divider straight off APB.
    EspApb { clk: u32 },
    /// The nRF UARTE: 18 fixed rates and nothing in between.
    NrfTable,
}

/// A divider together with the clock's name, for the text beside the field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Source {
    pub divider: Divider,
    /// The clock as the reader knows it from the Clock tab: `PCLK2`, `XTAL`.
    pub clock: &'static str,
}

/// Whether, and how, a module's rate can be checked on this chip.
///
/// Every variant but `Checked` carries the sentence the hover shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plan {
    Checked(Source),
    /// The rate never reaches the generated code: no driver is built for it.
    NotUsed(&'static str),
    /// The chip panics before the UART is set up, whatever the rate, because
    /// the HAL refuses the clock setup itself.
    Doomed(&'static str),
    /// The rate is used, but nothing here can say what the peripheral makes of
    /// it.
    Unchecked(&'static str),
}

/// What the HAL does with one rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fate {
    /// Programmed as asked; `actual` differs only by the divider's rounding.
    Runs { actual: f64 },
    /// RP: inside the divider's range, but the fraction rounds up to 64/64 and
    /// FBRD holds only 0..=63, so the carry into the integer part is lost.
    /// A hole in the range, not an edge of it.
    CarryLost { actual: f64 },
    /// nRF: the nearest table rate is programmed instead.
    Snapped { setting: u32, actual: f64 },
    /// Outside the divider, clamped silently to its end (RP).
    Clamped { actual: f64, too_fast: bool },
    /// F1, too slow: the divider overflows its 16 bits and the top is dropped.
    /// `None` when what is left is 0 and the port does not run at all.
    Wraps { actual: Option<f64> },
    /// The HAL asserts, divides by zero, or returns an `Err` the generated code
    /// unwraps: the chip panics at boot.
    Panics { why: &'static str, too_fast: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Ok,
    Marginal,
    Broken,
}

impl Fate {
    /// The rate on the wire, when there is one.
    pub fn actual(self) -> Option<f64> {
        match self {
            Fate::Runs { actual }
            | Fate::CarryLost { actual }
            | Fate::Snapped { actual, .. }
            | Fate::Clamped { actual, .. } => Some(actual),
            Fate::Wraps { actual } => actual,
            Fate::Panics { .. } => None,
        }
    }

    /// How far the wire is from `asked`, in percent.
    pub fn error_pct(self, asked: u32) -> Option<f64> {
        let actual = self.actual()?;
        // Multiplied first, so a round figure lands exactly on a threshold:
        // 2500 / 100000 * 100 is 2.4999..., 2500 * 100 / 100000 is 2.5.
        (asked > 0).then(|| (actual - asked as f64).abs() * 100.0 / asked as f64)
    }

    /// Judged by the error alone, whatever produced it: a clamp that lands
    /// close is as good as a rounding that does, and a panic has no error.
    pub fn severity(self, asked: u32) -> Severity {
        match self.error_pct(asked) {
            None => Severity::Broken,
            Some(e) if e < OK_PCT => Severity::Ok,
            Some(e) if e <= MARGINAL_PCT => Severity::Marginal,
            Some(_) => Severity::Broken,
        }
    }

    fn too_fast(self) -> bool {
        matches!(
            self,
            Fate::Clamped { too_fast: true, .. } | Fate::Panics { too_fast: true, .. }
        )
    }

    fn too_slow(self) -> bool {
        matches!(
            self,
            Fate::Wraps { .. }
                | Fate::Clamped {
                    too_fast: false,
                    ..
                }
                | Fate::Panics {
                    too_fast: false,
                    ..
                }
        )
    }
}

// Each is the first half of "..., so the generated code panics when it sets the
// UART up." - a clause, not a noun phrase.
const WHY_ZERO: &str = "A rate of 0 divides by zero in the HAL";
const WHY_F1_FAST: &str =
    "This rate is above the clock / 16, and stm32f1xx-hal asserts against that";
const WHY_EMBASSY_FAST: &str =
    "This rate needs a divider below 16, and embassy-stm32 refuses it with BaudrateTooHigh";
const WHY_EMBASSY_SLOW: &str =
    "This rate needs a divider above 65535, and embassy-stm32 refuses it with BaudrateTooLow";
const WHY_ESP_REFUSED: &str = "esp-hal's Config::validate refuses 0 and anything above 5 000 000";
const WHY_ESP_SLOW: &str =
    "This rate is below the divider's range, and esp-hal asserts against that";
const WHY_PL011_OVERFLOW: &str =
    "rp-hal multiplies clk_peri by 8 in 32 bits, and at this clock that overflows";

/// Run `baud` through the divider exactly as its HAL does.
pub fn outcome(d: Divider, baud: u32) -> Fate {
    match d {
        Divider::F1 { pclk } => {
            if baud == 0 {
                return Fate::Panics {
                    why: WHY_ZERO,
                    too_fast: false,
                };
            }
            let brr = pclk / baud;
            if brr < 16 {
                Fate::Panics {
                    why: WHY_F1_FAST,
                    too_fast: true,
                }
            } else if brr > 0xFFFF {
                // `bits(brr)` is raw: DIV_Mantissa is 12 bits and DIV_Fraction
                // 4, so everything above bit 15 falls into reserved bits.
                let low = brr & 0xFFFF;
                Fate::Wraps {
                    actual: (low != 0).then(|| pclk as f64 / low as f64),
                }
            } else {
                Fate::Runs {
                    actual: pclk as f64 / brr as f64,
                }
            }
        }
        Divider::EmbassyV1 { pclk } => {
            if baud == 0 {
                return Fate::Panics {
                    why: WHY_ZERO,
                    too_fast: false,
                };
            }
            // `calculate_brr` with mul = 1 and no prescaler: the truncated
            // quotient plus the remainder rounded to nearest.
            let brr = pclk / baud + (pclk % baud + baud / 2) / baud;
            if brr < 16 {
                Fate::Panics {
                    why: WHY_EMBASSY_FAST,
                    too_fast: true,
                }
            } else if brr >= 0x1_0000 {
                Fate::Panics {
                    why: WHY_EMBASSY_SLOW,
                    too_fast: false,
                }
            } else {
                Fate::Runs {
                    actual: pclk as f64 / brr as f64,
                }
            }
        }
        Divider::Pl011 { clk } => {
            // rp-hal returns `BadArgument` for 0 or an overflowing `8 * clk`
            // (the generated `.unwrap()` panics); embassy-rp divides by 0.
            let Some(div) = clk.checked_mul(8).and_then(|x| x.checked_div(baud)) else {
                return Fate::Panics {
                    why: if baud == 0 {
                        WHY_ZERO
                    } else {
                        WHY_PL011_OVERFLOW
                    },
                    too_fast: false,
                };
            };
            let (ibrd, fbrd, clamp) = match (div >> 7, (div & 0x7F).div_ceil(2)) {
                (0, _) => (1, 0, Some(true)),
                (i, _) if i >= 65_535 => (65_535, 0, Some(false)),
                (i, f) => (i, f, None),
            };
            // FBRD is 6 bits wide. A fraction that rounds up to 64 is written
            // anyway - rp-hal's field writer masks it, embassy-rp's raw write
            // lands in reserved bits - so the hardware sees 0, not a carry.
            let carry_lost = fbrd == 64;
            let fbrd = fbrd & 0x3F;
            let actual = 4.0 * clk as f64 / (64 * ibrd + fbrd) as f64;
            match clamp {
                Some(too_fast) => Fate::Clamped { actual, too_fast },
                None if carry_lost => Fate::CarryLost { actual },
                None => Fate::Runs { actual },
            }
        }
        Divider::EspSclk { clk } | Divider::EspApb { clk } => {
            if baud == 0 || baud > ESP_MAX {
                return Fate::Panics {
                    why: WHY_ESP_REFUSED,
                    too_fast: baud > 0,
                };
            }
            let clk = clk as u64;
            let baud = baud as u64;
            let (sclk_div, divider) = if let Divider::EspSclk { .. } = d {
                // `clk.div_ceil(4095).div_ceil(baud)`, then
                // `ClockConfig::new(src, clk_div - 1)` asserts `div_num <= 255`.
                let sclk_div = clk.div_ceil(4095).div_ceil(baud);
                if sclk_div - 1 > 255 {
                    return Fate::Panics {
                        why: WHY_ESP_SLOW,
                        too_fast: false,
                    };
                }
                (sclk_div, (clk << 4) / (baud * sclk_div))
            } else {
                // `BaudRateConfig::new` asserts the 20-bit integral part.
                let divider = (clk << 4) / baud;
                if divider >> 4 > 1_048_575 {
                    return Fate::Panics {
                        why: WHY_ESP_SLOW,
                        too_fast: false,
                    };
                }
                (1, divider)
            };
            Fate::Runs {
                actual: clk as f64 * 16.0 / (sclk_div * divider) as f64,
            }
        }
        Divider::NrfTable => {
            // The codegen's own pick, so the panel names the rate it emits.
            let (setting, _) = nrf::baud_variant(baud);
            let actual = nrf_actual(setting) as f64;
            if setting == baud {
                Fate::Runs { actual }
            } else {
                Fate::Snapped { setting, actual }
            }
        }
    }
}

/// What each UARTE setting really runs at.
///
/// The UARTE divides 16 MHz by an integer, so every setting runs at 16 MHz / N
/// (921600 is 16 MHz / 17), not at the fraction of 16 MHz its register value
/// spells out. These are the actual rates the PAC docs carry, identically in
/// nrf52833-pac 0.12 (nrf-hal) and nrf-pac 0.4 (embassy-nrf).
const NRF_ACTUAL: [(u32, u32); 18] = [
    (1_200, 1_205),
    (2_400, 2_396),
    (4_800, 4_808),
    (9_600, 9_598),
    (14_400, 14_401),
    (19_200, 19_208),
    (28_800, 28_777),
    (31_250, 31_250),
    (38_400, 38_369),
    (56_000, 55_944),
    (57_600, 57_554),
    (76_800, 76_923),
    (115_200, 115_108),
    (230_400, 231_884),
    (250_000, 250_000),
    (460_800, 457_143),
    (921_600, 941_176),
    (1_000_000, 1_000_000),
];

fn nrf_actual(setting: u32) -> u32 {
    NRF_ACTUAL
        .iter()
        .find(|(s, _)| *s == setting)
        .map_or(setting, |(_, a)| *a)
}

/// Neither too fast nor too slow: the divider takes the rate as asked, up to
/// its rounding (and, on the RP, a lost carry).
fn in_range(f: Fate) -> bool {
    !f.too_fast() && !f.too_slow()
}

/// The slowest and fastest rates the divider programs AS ASKED - no panic, no
/// clamp, no overflow, no snapping - or `None` when there are none (an RP
/// clock whose `8 * clk` no longer fits 32 bits). Rounding still applies inside
/// it, and on the RP so do the lost-carry holes ([`Fate::CarryLost`]).
///
/// Searched, not derived: [`outcome`] is the one statement of each HAL's rules,
/// and a second, closed-form statement of the same limits is exactly the kind
/// of copy that drifts. Both edges are monotone - once a rate is too fast, every
/// faster one is too.
pub fn range(d: Divider) -> Option<(u32, u32)> {
    if d == Divider::NrfTable {
        return Some(NRF_RANGE);
    }
    const TOP: u32 = 200_000_000;
    // Fastest rate that is not too fast.
    let hi = if !outcome(d, TOP).too_fast() {
        TOP
    } else {
        let (mut ok, mut bad) = (1_u32, TOP);
        while bad - ok > 1 {
            let mid = ok + (bad - ok) / 2;
            if outcome(d, mid).too_fast() {
                bad = mid;
            } else {
                ok = mid;
            }
        }
        ok
    };
    // Slowest rate that is not too slow.
    let lo = if !outcome(d, 1).too_slow() {
        1
    } else {
        let (mut bad, mut ok) = (1_u32, hi.max(1));
        while ok - bad > 1 {
            let mid = bad + (ok - bad) / 2;
            if outcome(d, mid).too_slow() {
                bad = mid;
            } else {
                ok = mid;
            }
        }
        ok
    };
    (lo <= hi && in_range(outcome(d, lo)) && in_range(outcome(d, hi))).then_some((lo, hi))
}

/// The F1's (PCLK1, PCLK2) as the Clock tab computes them - what embassy-stm32
/// programs on the Async runtime. `None` without a tree, or with a bus at zero.
fn f1_tab_pclks(clock: &crate::panels::mcu_module::clock::ClockConfig) -> Option<(u32, u32)> {
    let crate::panels::mcu_module::clock::ClockConfig::Graph(gc) = clock else {
        return None;
    };
    let f = crate::panels::mcu_module::clock::graph::evaluate(&gc.graph);
    let (p1, p2) = (*f.get("pclk1")?, *f.get("pclk2")?);
    (p1 > 0 && p2 > 0).then_some((p1, p2))
}

/// What [`Chip::plan`] needs from the MCU, read once.
///
/// Owned rather than borrowed: the module panel draws while `mcu.modules` is
/// borrowed mutably out of the same `Mcu`, so the clock has to be read first.
/// Each clock is `None` when not known at all ([`Chip::bare`]) and `Some(Err)`
/// when the HAL refuses the clock setup.
#[derive(Clone, Debug, PartialEq)]
pub struct Chip {
    family: String,
    runtime: Runtime,
    /// F1: (PCLK1, PCLK2) as the runtime's HAL will program them - the chain on
    /// stm32f1xx-hal, the tab itself on embassy-stm32.
    f1_pclks: Option<Result<(u32, u32), ()>>,
    /// RP, blocking backend: clk_peri from the Clock tab's PLL.
    rp_blocking_hz: Option<Result<u32, ()>>,
    /// ESP: the crystal the Clock tab states, snapped to one esp-hal has.
    xtal_hz: Option<u32>,
}

impl Chip {
    pub fn of(mcu: &Mcu) -> Self {
        let family = mcu.family.as_str();
        // On Async the F1 is on embassy-stm32, which programs exactly the Clock
        // tab's prescalers - so the tab's own PCLKs are the answer there, and
        // stm32f1xx-hal's chain (with its `freeze` refusals) only off Async.
        // embassy has refusals of its own: `rcc_assert!`s at init, checked on
        // the clock the Async block is emitted from.
        let f1_pclks = if family != "stm32f1" {
            None
        } else if mcu.pending_runtime == Runtime::Async {
            let boots = super::codegen::rcc::f1_embassy_clock_ok(
                &super::codegen::rcc::f1_embassy_clock(&mcu.clock),
            );
            f1_tab_pclks(&mcu.clock).map(|p| if boots { Ok(p) } else { Err(()) })
        } else {
            Some(stm32::f1_hal_pclks(&mcu.clock).ok_or(()))
        };
        let rp_blocking_hz = rp::is_rp(family).then(|| {
            if rp::blocking_plls_ok(mcu) {
                Ok(rp::blocking_peri_hz(mcu))
            } else {
                Err(())
            }
        });
        Self {
            family: mcu.family.clone(),
            // The STAGED runtime, like every other row of the module panel
            // (`pending_is_async`): the panel previews what Apply will build.
            runtime: mcu.pending_runtime,
            f1_pclks,
            rp_blocking_hz,
            xtal_hz: esp_xtal_hz(mcu),
        }
    }

    /// A chip known only by family and runtime - no clock at all.
    pub fn bare(family: &str, runtime: Runtime) -> Self {
        Self {
            family: family.to_owned(),
            runtime,
            f1_pclks: None,
            rp_blocking_hz: None,
            xtal_hz: None,
        }
    }

    /// How the module on USART/UART `instance` can be checked.
    ///
    /// The runtime matters only where it swaps the HAL: an Async RP is
    /// embassy-rp on embassy's own clocks, and an STM32 other than the F1 builds
    /// a driver only on Async. The F1 builds one on every runtime - on
    /// stm32f1xx-hal off Async, on embassy-stm32 on it, with the PCLKs each
    /// programs (see [`Chip::of`]) - and an ESP or an nRF divides the same way
    /// on both.
    pub fn plan(&self, instance: u8) -> Plan {
        let f = self.family.as_str();
        if f == "stm32f1" {
            let (pclk1, pclk2) = match self.f1_pclks {
                Some(Ok(p)) => p,
                Some(Err(())) if self.runtime == Runtime::Async => {
                    return Plan::Doomed(F1_EMBASSY_INIT);
                }
                Some(Err(())) => return Plan::Doomed(F1_FREEZE),
                None => return Plan::Unchecked(NO_CLOCK),
            };
            // stm32f1xx-hal's `enable.rs`: USART1 on APB2, USART2/3 on APB1.
            // UART4/5 sit on APB1 too, but that HAL has no `Serial` for them;
            // embassy-stm32 drives them like the others.
            let embassy = self.runtime == Runtime::Async;
            let divider = |pclk| {
                if embassy {
                    Divider::EmbassyV1 { pclk }
                } else {
                    Divider::F1 { pclk }
                }
            };
            return match instance {
                1 => Plan::Checked(Source {
                    divider: divider(pclk2),
                    clock: "PCLK2",
                }),
                2 | 3 => Plan::Checked(Source {
                    divider: divider(pclk1),
                    clock: "PCLK1",
                }),
                4 | 5 if embassy => Plan::Checked(Source {
                    divider: divider(pclk1),
                    clock: "PCLK1",
                }),
                _ => Plan::NotUsed(F1_NO_UART45),
            };
        }
        if f.starts_with("stm32") {
            return if self.runtime == Runtime::Async {
                Plan::Unchecked(NO_BUS_MAP)
            } else {
                Plan::NotUsed(NOT_USED_RAW)
            };
        }
        if rp::is_rp(f) {
            // clk_peri follows clk_sys on both HALs. Async code calls
            // `embassy_rp::init(Default::default())`, which ignores the tab.
            let clk = if self.runtime == Runtime::Async {
                rp::async_sys_hz(f)
            } else {
                match self.rp_blocking_hz {
                    Some(Ok(hz)) => hz,
                    Some(Err(())) => return Plan::Doomed(RP_PLL_REFUSED),
                    None => return Plan::Unchecked(NO_CLOCK),
                }
            };
            return Plan::Checked(Source {
                divider: Divider::Pl011 { clk },
                clock: "clk_peri",
            });
        }
        if is_esp(f) {
            // The ESP32 and S2 default the UART to APB, which esp-hal holds at
            // 80 MHz for every CPU clock they offer (80/160/240). The rest
            // default to the crystal.
            if f == "esp32" || f == "esp32s2" {
                return Plan::Checked(Source {
                    divider: Divider::EspApb { clk: 80_000_000 },
                    clock: "APB",
                });
            }
            return match self.xtal_hz {
                Some(clk) => Plan::Checked(Source {
                    divider: Divider::EspSclk { clk },
                    clock: "XTAL",
                }),
                None => Plan::Unchecked(NO_CLOCK),
            };
        }
        if nrf::is_nrf(f) {
            return Plan::Checked(Source {
                divider: Divider::NrfTable,
                clock: "UARTE",
            });
        }
        Plan::Unchecked(NO_FAMILY)
    }
}

/// The ESP's crystal as the Clock tab states it, snapped to one esp-hal runs.
///
/// esp-hal never sees this number - it knows only discrete crystals (26 or 40
/// MHz on the C2, 40 or 48 on the C5) - but the tab edits it as a free value
/// between the first and last of them. The node carries only those two ends,
/// and no shipped part has more than two crystals, so the nearer end is the
/// crystal the board can really have.
fn esp_xtal_hz(mcu: &Mcu) -> Option<u32> {
    use super::clock::graph::model::{NodeKind, NodeState};
    use super::clock::model::ClockConfig;
    if !is_esp(&mcu.family) {
        return None;
    }
    let ClockConfig::Graph(gc) = &mcu.clock else {
        return None;
    };
    let node = gc.graph.node("xtal")?;
    let (NodeKind::Source { min_hz, max_hz, .. }, NodeState::Source { hz, .. }) =
        (&node.kind, &node.state)
    else {
        return None;
    };
    let (lo, hi) = (*min_hz.min(max_hz), *min_hz.max(max_hz));
    let snapped = if (*hz as u64) * 2 < lo as u64 + hi as u64 {
        lo
    } else {
        hi
    };
    (snapped > 0).then_some(snapped)
}

const NO_CLOCK: &str = "This chip has no clock tree to check the rate against.";
const NO_BUS_MAP: &str = "Which clock feeds each USART is not modelled for this family yet, so the rate can't be checked against it.";
const NO_FAMILY: &str = "Rates are not checked for this chip family.";
const NOT_USED_RAW: &str = "This runtime binds the USART pins raw and builds no driver, so the baud rate never reaches the generated code.";
const F1_NO_UART45: &str = "stm32f1xx-hal 0.10 has no Serial for UART4 or UART5, so no driver is generated for it and the baud rate never reaches the code.";
const F1_FREEZE: &str = "stm32f1xx-hal's freeze asserts on this clock setup (the Clock tab flags the limit it breaks), so the chip panics before the UART is set up.";
const F1_EMBASSY_INIT: &str = "embassy-stm32's clock init asserts on this clock setup (the Clock tab flags the limit it breaks), so the chip panics before the UART is set up.";
const RP_PLL_REFUSED: &str = "rp-hal refuses one of the Clock tab's PLLs (post dividers 1–6, VCO 750–1600 MHz on the RP2040 or 400–1600 MHz on the RP2350), so the board panics before the UART is set up.";

/// The typed rates the field accepts under this plan. Fixed ends only - the
/// clock-dependent ones are REPORTED, not enforced, because clamping to them
/// would rewrite the module's rate every time the Clock tab moved.
pub fn typed_range(plan: &Plan) -> std::ops::RangeInclusive<u32> {
    use crate::serial::{BAUD_MAX, BAUD_MIN};
    match plan {
        Plan::Checked(Source {
            divider: Divider::EspSclk { .. } | Divider::EspApb { .. },
            ..
        }) => BAUD_MIN..=ESP_MAX,
        Plan::Checked(Source {
            divider: Divider::NrfTable,
            ..
        }) => NRF_RANGE.0..=NRF_RANGE.1,
        _ => BAUD_MIN..=BAUD_MAX,
    }
}

/// What the panel prints under the field.
#[derive(Clone, Debug, PartialEq)]
pub struct Hint {
    /// First line, coloured by `severity`: what the wire gets.
    pub verdict: String,
    /// Second line, dim: the range at the current clock.
    pub range: Option<String>,
    /// The full sentence, for the hover.
    pub why: String,
    /// `None` for a plan that cannot judge the rate.
    pub severity: Option<Severity>,
}

/// The hint for `baud` under `plan`.
pub fn hint(plan: &Plan, baud: u32) -> Hint {
    let src = match plan {
        Plan::Checked(src) => *src,
        Plan::NotUsed(why) => {
            return Hint {
                verdict: "not used by the generated code".to_owned(),
                range: None,
                why: (*why).to_owned(),
                severity: None,
            };
        }
        Plan::Doomed(why) => {
            return Hint {
                verdict: "panics at boot · clock setup".to_owned(),
                range: None,
                why: (*why).to_owned(),
                severity: Some(Severity::Broken),
            };
        }
        Plan::Unchecked(why) => {
            return Hint {
                verdict: "not checked on this chip".to_owned(),
                range: None,
                why: (*why).to_owned(),
                severity: None,
            };
        }
    };
    let fate = outcome(src.divider, baud);
    let severity = fate.severity(baud);
    let clock = clock_label(src);
    let range = range(src.divider).map(|(lo, hi)| match src.divider {
        Divider::NrfTable => format!("fixed rates {} – {}", grouped(lo), grouped(hi)),
        _ => format!("range {} – {} at {clock}", grouped(lo), grouped(hi)),
    });
    let n = |a: f64| grouped(a.round() as u64);
    let off = |a: f64| {
        let e = (a - baud as f64).abs() * 100.0 / baud.max(1) as f64;
        if e < 0.005 {
            "exact".to_owned()
        } else {
            format!("{e:.2} % off")
        }
    };
    let judgement = match severity {
        Severity::Ok => "",
        Severity::Marginal => {
            " That is close to the edge: it works only if the other end's clock is accurate."
        }
        Severity::Broken => " That is too far for a UART to stay in step with the other end.",
    };
    let (verdict, why) = match fate {
        Fate::Runs { actual } => (
            format!("runs at {} baud · {}", n(actual), off(actual)),
            if src.divider == Divider::NrfTable {
                format!(
                    "The UARTE's {baud} setting really runs at {} baud.{judgement}",
                    n(actual)
                )
            } else {
                format!(
                    "The divider at {clock} makes {} baud from {}.{judgement}",
                    n(actual),
                    grouped(baud)
                )
            },
        ),
        Fate::CarryLost { actual } => (
            format!("runs at {} baud · fraction lost", n(actual)),
            format!(
                "At {clock} the HAL rounds the divider's fraction up to 64/64, but FBRD holds only 0–63, so the carry is lost and the port runs at {} baud.{judgement}",
                n(actual)
            ),
        ),
        Fate::Snapped { setting, actual } => (
            format!(
                "runs at {} baud · {setting} setting, {}",
                n(actual),
                off(actual)
            ),
            format!(
                "The nRF UARTE has fixed rates only. The nearest to {} is its {setting} setting, which really runs at {} baud.{judgement}",
                grouped(baud),
                n(actual)
            ),
        ),
        Fate::Clamped { actual, too_fast } => (
            format!("runs at {} baud · clamped by the HAL", n(actual)),
            format!(
                "{} for the divider at {clock}. The HAL clamps it to its {} end without an error, so the port runs at {} baud.",
                if too_fast { "Too fast" } else { "Too slow" },
                if too_fast { "fastest" } else { "slowest" },
                n(actual)
            ),
        ),
        Fate::Wraps { actual } => (
            match actual {
                Some(a) => format!("runs at {} baud · divider overflows", n(a)),
                None => "does not run · divider overflows".to_owned(),
            },
            format!(
                "Too slow for the 16-bit divider at {clock}: stm32f1xx-hal writes the value without a check, the top bits are dropped, and {}.",
                match actual {
                    Some(a) => format!("the port runs at {} baud", n(a)),
                    None => "the divider left is 0, so the port does not run".to_owned(),
                }
            ),
        ),
        Fate::Panics { why, .. } => (
            "panics at boot".to_owned(),
            format!("{why}, so the generated code panics when it sets the UART up."),
        ),
    };
    Hint {
        verdict,
        range,
        why,
        severity: Some(severity),
    }
}

/// The short tag a preset carries in the dropdown, its severity, and the hover.
pub fn preset_tag(plan: &Plan, baud: u32) -> Option<(String, Severity, String)> {
    let Plan::Checked(src) = plan else {
        return None;
    };
    let fate = outcome(src.divider, baud);
    let severity = fate.severity(baud);
    let tag = match fate {
        Fate::Panics { .. } => "panics".to_owned(),
        Fate::Wraps { .. } => "overflows".to_owned(),
        Fate::Clamped { .. } => "clamped".to_owned(),
        Fate::CarryLost { .. } => "fraction lost".to_owned(),
        Fate::Runs { .. } | Fate::Snapped { .. } => match fate.error_pct(baud) {
            Some(e) if e < 0.005 => "exact".to_owned(),
            Some(e) => format!("{e:.2} %"),
            None => String::new(),
        },
    };
    Some((tag, severity, hint(plan, baud).why))
}

/// The instance a USART/LPUART pin function belongs to, for the ⓘ popup.
pub fn instance_of(f: &PinFunction) -> Option<u8> {
    match f {
        PinFunction::UsartTx(n)
        | PinFunction::UsartRx(n)
        | PinFunction::UsartCts(n)
        | PinFunction::UsartRts(n)
        | PinFunction::UsartCk(n)
        | PinFunction::LpuartTx(n)
        | PinFunction::LpuartRx(n)
        | PinFunction::LpuartCts(n)
        | PinFunction::LpuartRts(n) => Some(*n),
        _ => None,
    }
}

/// The ⓘ popup's "Max baud rate" value at the current clock, or `None` when
/// this chip cannot say (the popup then keeps its generic line).
///
/// An LPUART is its own peripheral with its own clock; the F1 has none, and
/// every family that does is one [`Chip::plan`] does not check yet.
pub fn max_baud_text(chip: &Chip, f: &PinFunction) -> Option<String> {
    let lpuart = matches!(
        f,
        PinFunction::LpuartTx(_)
            | PinFunction::LpuartRx(_)
            | PinFunction::LpuartCts(_)
            | PinFunction::LpuartRts(_)
    );
    if lpuart {
        return None;
    }
    let Plan::Checked(src) = chip.plan(instance_of(f)?) else {
        return None;
    };
    let (_, hi) = range(src.divider)?;
    Some(match src.divider {
        Divider::NrfTable => format!("{} baud (fixed rates)", grouped(hi)),
        _ => format!("{} baud at {}", grouped(hi), clock_label(src)),
    })
}

/// `PCLK2 72 MHz` - the clock's name and its current frequency.
fn clock_label(src: Source) -> String {
    let hz = match src.divider {
        Divider::F1 { pclk: hz }
        | Divider::EmbassyV1 { pclk: hz }
        | Divider::Pl011 { clk: hz }
        | Divider::EspSclk { clk: hz }
        | Divider::EspApb { clk: hz } => hz,
        Divider::NrfTable => return src.clock.to_owned(),
    };
    if hz % 1_000_000 == 0 {
        format!("{} {} MHz", src.clock, hz / 1_000_000)
    } else {
        let mhz = format!("{:.3}", hz as f64 / 1e6);
        format!("{} {} MHz", src.clock, mhz.trim_end_matches('0'))
    }
}

/// `4500000` → `4 500 000`.
fn grouped(n: impl Into<u64>) -> String {
    let digits = n.into().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pct(d: Divider, baud: u32) -> f64 {
        outcome(d, baud).error_pct(baud).expect("a rate")
    }

    const F1_72: Divider = Divider::F1 { pclk: 72_000_000 };
    const F1_36: Divider = Divider::F1 { pclk: 36_000_000 };
    const F1_8: Divider = Divider::F1 { pclk: 8_000_000 };

    /// The F1's ends are PCLK/16 and "BRR still fits 16 bits". Checked against
    /// the closed forms, which the search must reproduce: a search that stopped
    /// one step early would print a range the divider does not keep.
    #[test]
    fn the_f1_range_is_pclk_over_16_down_to_the_16_bit_divider() {
        assert_eq!(range(F1_72), Some((1_099, 4_500_000)));
        assert_eq!(range(F1_36), Some((550, 2_250_000)));
        for pclk in [
            8_000_000, 24_000_000, 36_000_000, 48_000_000, 64_000_000, 72_000_000,
        ] {
            let want = (pclk / 65_536 + 1, pclk / 16);
            assert_eq!(range(Divider::F1 { pclk }), Some(want), "pclk {pclk}");
        }
    }

    /// The failures the fixed list already had: on the 8 MHz HSI, 921600 is an
    /// assert and the two below it are 2.12 % off. At the default 72 MHz
    /// every preset is fine.
    #[test]
    fn the_f1_on_hsi_breaks_the_top_presets() {
        assert!(matches!(
            outcome(F1_8, 921_600),
            Fate::Panics { too_fast: true, .. }
        ));
        for b in [230_400, 460_800] {
            assert!((pct(F1_8, b) - 2.124).abs() < 0.01, "{b}: {}", pct(F1_8, b));
            assert_eq!(outcome(F1_8, b).severity(b), Severity::Marginal);
        }
        for b in crate::serial::BAUDS {
            assert_eq!(outcome(F1_72, b).severity(b), Severity::Ok, "{b} at 72 MHz");
            assert_eq!(outcome(F1_36, b).severity(b), Severity::Ok, "{b} at 36 MHz");
        }
        // 921600 at 72 MHz: BRR 78, 923 077 baud.
        assert!((pct(F1_72, 921_600) - 0.160).abs() < 0.001);
    }

    /// Below the range the F1 does not fail, it lies: 36 MHz / 500 is 72000,
    /// whose top bit is dropped, leaving 6464 - 5569 baud.
    #[test]
    fn a_too_slow_f1_rate_wraps_instead_of_failing() {
        match outcome(F1_36, 500) {
            Fate::Wraps { actual: Some(a) } => assert!((a - 5_569.3).abs() < 0.1, "{a}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(outcome(F1_36, 500).severity(500), Severity::Broken);
        // Exactly 65536: nothing is left, and the port does not run.
        assert_eq!(
            outcome(Divider::F1 { pclk: 65_536 * 300 }, 300),
            Fate::Wraps { actual: None }
        );
    }

    const RP_125: Divider = Divider::Pl011 { clk: 125_000_000 };

    /// 125 MHz / (16 × 921600) is 8.477; the HAL rounds the fraction up to
    /// 31/64, and 4 × 125e6 / (64 × 8 + 31) is 920 810 baud.
    #[test]
    fn the_pl011_divides_in_sixty_fourths() {
        let a = outcome(RP_125, 921_600).actual().unwrap();
        assert!((a - 920_810.3).abs() < 0.5, "{a}");
        for b in crate::serial::BAUDS {
            assert_eq!(outcome(RP_125, b).severity(b), Severity::Ok, "{b}");
        }
    }

    /// Both ends clamp without a word: too fast runs at clk/16, too slow at
    /// clk/(16 × 65535).
    #[test]
    fn the_pl011_clamps_silently_at_both_ends() {
        assert_eq!(
            outcome(RP_125, 8_000_000),
            Fate::Clamped {
                actual: 7_812_500.0,
                too_fast: true
            }
        );
        match outcome(RP_125, 100) {
            Fate::Clamped {
                actual,
                too_fast: false,
            } => assert!((actual - 125e6 / (16.0 * 65_535.0)).abs() < 0.01),
            other => panic!("{other:?}"),
        }
        // 8 × 125e6 / (65535 × 128) is 119.2; the first rate over it is 120.
        assert_eq!(range(RP_125), Some((120, 7_812_500)));
    }

    /// A fraction that rounds up to 64 does not carry into the integer part:
    /// FBRD is 6 bits, so the hardware is left with a zero fraction. Near the
    /// top that is not rounding any more - 3 920 000 at 125 MHz is div 255,
    /// integer 1, fraction 64, and the port runs at 7 812 500.
    #[test]
    fn a_fraction_rounding_up_to_64_is_dropped_not_carried() {
        let clk = 125_000_000_u32;
        let baud = (300..2_000_000)
            .find(|b| (8 * clk / b) & 0x7F == 127)
            .expect("some rate lands on the edge");
        let ibrd = (8 * clk / baud) >> 7;
        let want = 4.0 * clk as f64 / (64 * ibrd) as f64;
        assert_eq!(outcome(RP_125, baud), Fate::CarryLost { actual: want });

        assert_eq!(
            outcome(RP_125, 3_920_000),
            Fate::CarryLost {
                actual: 7_812_500.0
            }
        );
        assert_eq!(
            outcome(RP_125, 3_920_000).severity(3_920_000),
            Severity::Broken
        );
        // A hole, not an edge: the range around it is unchanged.
        assert_eq!(range(RP_125), Some((120, 7_812_500)));
    }

    /// rp-hal clamps at `int_part >= 65535`, so an integer part of exactly
    /// 65535 loses its fraction too and counts as too slow. No whole rate
    /// lands there at 125 MHz; at 314.57 MHz, 300 baud is div 8 388 533 -
    /// integer 65535 - while 301 is inside the range.
    #[test]
    fn an_integer_part_of_exactly_65535_is_clamped() {
        let d = Divider::Pl011 { clk: 314_570_000 };
        assert_eq!(
            outcome(d, 300),
            Fate::Clamped {
                actual: 4.0 * 314_570_000.0 / (64.0 * 65_535.0),
                too_fast: false
            }
        );
        assert_eq!(range(d).unwrap().0, 301);
    }

    /// A clk_peri past 536 MHz overflows `8 * clk` in rp-hal: every rate
    /// panics, and there is no range to print.
    #[test]
    fn an_overflowing_pl011_clock_has_no_range() {
        let d = Divider::Pl011 { clk: 600_000_000 };
        assert_eq!(
            outcome(d, 115_200),
            Fate::Panics {
                why: WHY_PL011_OVERFLOW,
                too_fast: false
            }
        );
        assert_eq!(range(d), None);
    }

    /// esp-hal's own numbers: `validate` refuses above 5 000 000, and the
    /// asserts sit at 77 baud on APB 80 MHz, 39 on a 40 MHz crystal, 31 on the
    /// H2's 32 MHz and 25 on the C2's 26 MHz.
    #[test]
    fn the_esp_limits_are_esp_hals() {
        assert_eq!(
            range(Divider::EspApb { clk: 80_000_000 }),
            Some((77, 5_000_000))
        );
        assert_eq!(
            range(Divider::EspSclk { clk: 40_000_000 }),
            Some((39, 5_000_000))
        );
        assert_eq!(range(Divider::EspSclk { clk: 32_000_000 }).unwrap().0, 31);
        assert_eq!(range(Divider::EspSclk { clk: 26_000_000 }).unwrap().0, 25);
        assert!(matches!(
            outcome(Divider::EspSclk { clk: 40_000_000 }, 5_000_001),
            Fate::Panics { too_fast: true, .. }
        ));
        assert!(matches!(
            outcome(Divider::EspApb { clk: 80_000_000 }, 0),
            Fate::Panics {
                too_fast: false,
                ..
            }
        ));
    }

    /// `ClockConfig::new` asserts `div_num <= 255`, and div_num is sclk_div - 1,
    /// so an sclk divider of exactly 256 is still legal. No real crystal lands
    /// on it; a clock of 4095 x 10240 Hz does, at 40 baud.
    #[test]
    fn an_sclk_divider_of_256_is_still_legal() {
        let d = Divider::EspSclk {
            clk: 4_095 * 10_240,
        };
        assert!(
            matches!(outcome(d, 40), Fate::Runs { .. }),
            "{:?}",
            outcome(d, 40)
        );
        assert_eq!(range(d).unwrap().0, 40);
    }

    /// 115200 off a 40 MHz crystal: sclk undivided, divider 640e6 / 115200 =
    /// 5555 sixteenths, which is 115 211.5 baud.
    #[test]
    fn the_esp_divider_counts_sixteenths() {
        let a = outcome(Divider::EspSclk { clk: 40_000_000 }, 115_200)
            .actual()
            .unwrap();
        assert!((a - 640e6 / 5_555.0).abs() < 0.01, "{a}");
        // Low rates go through the sclk divider: 300 baud needs 33 of it.
        let a = outcome(Divider::EspSclk { clk: 40_000_000 }, 300)
            .actual()
            .unwrap();
        assert!((a - 300.0).abs() / 300.0 < 0.001, "{a}");
    }

    /// The nRF runs the rate the codegen picks, at what it really is: 74880
    /// becomes the 76800 setting, which is 76 923 baud - 2.73 % out.
    #[test]
    fn the_nrf_snaps_and_says_the_real_rate() {
        assert_eq!(
            outcome(Divider::NrfTable, 74_880),
            Fate::Snapped {
                setting: 76_800,
                actual: 76_923.0
            }
        );
        assert_eq!(
            outcome(Divider::NrfTable, 74_880).severity(74_880),
            Severity::Broken
        );
        // In the table and still marginal: 921600 is 16 MHz / 17.
        assert_eq!(
            outcome(Divider::NrfTable, 921_600).severity(921_600),
            Severity::Marginal
        );
        assert_eq!(
            outcome(Divider::NrfTable, 115_200).severity(115_200),
            Severity::Ok
        );
    }

    /// The real-rate table covers exactly the settings the codegen can emit,
    /// every real rate is 16 MHz over a whole divider, and the range is the
    /// table's two ends.
    #[test]
    fn every_nrf_setting_has_a_real_rate() {
        let settings: Vec<u32> = nrf::BAUDS.iter().map(|(b, _)| *b).collect();
        let actuals: Vec<u32> = NRF_ACTUAL.iter().map(|(b, _)| *b).collect();
        assert_eq!(settings, actuals);
        for (s, a) in NRF_ACTUAL {
            let n = (16e6 / a as f64).round();
            assert_eq!((16e6 / n).round() as u32, a, "{s} -> {a} is not 16 MHz / N");
        }
        assert_eq!(
            NRF_RANGE,
            (*settings.first().unwrap(), *settings.last().unwrap())
        );
    }

    /// The thresholds are inclusive where the doc says so.
    #[test]
    fn severity_follows_the_two_thresholds() {
        // Whole-baud rates against 100 000, so each lands EXACTLY on its
        // percentage: a float like 100 000 x 1.025 is 2.4999... % and would
        // test nothing at the edge.
        let at = |actual: f64| Fate::Runs { actual }.severity(100_000);
        assert_eq!(at(100_999.0), Severity::Ok);
        assert_eq!(at(101_000.0), Severity::Marginal);
        assert_eq!(at(102_500.0), Severity::Marginal);
        assert_eq!(at(102_501.0), Severity::Broken);
        assert_eq!(at(97_500.0), Severity::Marginal, "below counts the same");
        assert_eq!(
            Fate::Panics {
                why: "",
                too_fast: true
            }
            .severity(9_600),
            Severity::Broken
        );
    }

    fn builtin(id: &str) -> Mcu {
        crate::panels::mcu_module::builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("{id} is bundled"))
            .build_mcu()
    }

    /// Applied and staged alike, as after an Apply.
    fn set_runtime(mcu: &mut Mcu, rt: Runtime) {
        mcu.runtime = rt;
        mcu.pending_runtime = rt;
    }

    /// The F103 as shipped: USART1 on the 72 MHz APB2, USART2/3 on 36 MHz, on
    /// every runtime. The DIVIDER follows the HAL: stm32f1xx-hal's off Async,
    /// embassy-stm32's on it - which also drives UART4/5, where stm32f1xx-hal
    /// builds no driver at all.
    #[test]
    fn the_f103_checks_each_usart_on_its_own_bus() {
        let mut mcu = builtin("stm32f103c8t6");
        for rt in [Runtime::Blocking, Runtime::Async, Runtime::Rtic] {
            set_runtime(&mut mcu, rt);
            let chip = Chip::of(&mcu);
            let embassy = rt == Runtime::Async;
            let on = |pclk, clock| {
                Plan::Checked(Source {
                    divider: if embassy {
                        Divider::EmbassyV1 { pclk }
                    } else {
                        Divider::F1 { pclk }
                    },
                    clock,
                })
            };
            assert_eq!(chip.plan(1), on(72_000_000, "PCLK2"), "{rt:?}");
            assert_eq!(chip.plan(2), on(36_000_000, "PCLK1"), "{rt:?}");
            assert_eq!(chip.plan(3), on(36_000_000, "PCLK1"), "{rt:?}");
            if embassy {
                assert_eq!(chip.plan(4), on(36_000_000, "PCLK1"), "{rt:?}");
            } else {
                assert_eq!(chip.plan(4), Plan::NotUsed(F1_NO_UART45), "{rt:?}");
            }
        }
    }

    /// A clock embassy-stm32's init asserts on dooms every rate on Async, with
    /// embassy's reason - the verdict stm32f1xx-hal's `freeze` gets on Blocking.
    #[test]
    fn an_f1_clock_embassy_refuses_dooms_every_rate_on_async() {
        use crate::panels::mcu_module::clock::Stm32f1Clock;
        let mut mcu = builtin("stm32f103c8t6");
        mcu.apply_saved_clock(Stm32f1Clock {
            apb1_pre: 1, // PCLK1 72 MHz, over the 36 MHz ceiling
            ..Stm32f1Clock::default()
        });
        set_runtime(&mut mcu, Runtime::Async);
        let chip = Chip::of(&mcu);
        for n in [1, 2, 4] {
            assert_eq!(chip.plan(n), Plan::Doomed(F1_EMBASSY_INIT), "USART{n}");
        }
        set_runtime(&mut mcu, Runtime::Blocking);
        assert_eq!(Chip::of(&mcu).plan(2), Plan::Doomed(F1_FREEZE));
    }

    /// embassy-stm32 ROUNDS the F1's divider and refuses both ends with an
    /// error the generated code unwraps: too slow is a panic here, where
    /// stm32f1xx-hal wraps silently.
    #[test]
    fn embassy_on_the_f1_rounds_and_refuses_both_ends() {
        let e72 = Divider::EmbassyV1 { pclk: 72_000_000 };
        let e36 = Divider::EmbassyV1 { pclk: 36_000_000 };
        // Rounding moves the top: a divider of 15.5 already rounds to 16, so
        // the fastest rate is PCLK / 15.5 rather than stm32f1xx-hal's PCLK / 16.
        // The bottom stays: 72 MHz / 1099 rounds to 65514, / 1098 to 65574.
        assert_eq!(range(e72), Some((1_099, 4_645_161)));
        assert_eq!(range(e36), Some((550, 2_322_580)));
        assert!(matches!(outcome(e72, 4_500_001), Fate::Runs { .. }));
        assert!(matches!(
            outcome(e72, 4_645_162),
            Fate::Panics { too_fast: true, why } if why == WHY_EMBASSY_FAST
        ));
        assert!(matches!(
            outcome(e36, 500),
            Fate::Panics { too_fast: false, why } if why == WHY_EMBASSY_SLOW
        ));
        assert!(matches!(outcome(e72, 0), Fate::Panics { why, .. } if why == WHY_ZERO));
        // Rounding, not truncating: 8 MHz / 230400 is 34.72, so 35 here and 34
        // on stm32f1xx-hal - 0.79 % off instead of 2.12 %.
        let e8 = Divider::EmbassyV1 { pclk: 8_000_000 };
        match outcome(e8, 230_400) {
            Fate::Runs { actual } => assert!((actual - 228_571.4).abs() < 0.1, "{actual}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(outcome(e8, 230_400).severity(230_400), Severity::Ok);
        assert_eq!(outcome(F1_8, 230_400).severity(230_400), Severity::Marginal);
        // The hover names the clock the same way as for the other HAL.
        let h = hint(
            &Plan::Checked(Source {
                divider: e72,
                clock: "PCLK2",
            }),
            115_200,
        );
        assert!(
            h.range
                .as_deref()
                .is_some_and(|r| r.contains("PCLK2 72 MHz")),
            "{h:?}"
        );
    }

    /// A clock stm32f1xx-hal's `freeze` asserts on dooms every rate.
    #[test]
    fn an_f1_clock_the_hal_refuses_dooms_every_rate() {
        let chip = Chip {
            f1_pclks: Some(Err(())),
            ..Chip::bare("stm32f1", Runtime::Blocking)
        };
        assert_eq!(chip.plan(1), Plan::Doomed(F1_FREEZE));
        let h = hint(&chip.plan(1), 115_200);
        assert_eq!(h.verdict, "panics at boot · clock setup");
        assert_eq!(h.severity, Some(Severity::Broken));
        assert_eq!(h.range, None);
    }

    /// Every chip the IDE ships gets a real check, on every runtime it has.
    #[test]
    fn every_bundled_chip_is_checked() {
        for d in crate::panels::mcu_module::builtins::builtin_definitions() {
            let mut mcu = d.build_mcu();
            for rt in [Runtime::Blocking, Runtime::Async] {
                set_runtime(&mut mcu, rt);
                let plan = Chip::of(&mcu).plan(1);
                assert!(
                    matches!(plan, Plan::Checked(_)),
                    "{} {rt:?}: {plan:?}",
                    d.id
                );
            }
        }
    }

    /// The RP's blocking backend follows the Clock tab; the async one is
    /// embassy's default whatever the tab says. And it is the STAGED runtime
    /// that counts, like every other row of the panel.
    #[test]
    fn the_rp_clock_depends_on_the_runtime() {
        let async_on = |clk| {
            Plan::Checked(Source {
                divider: Divider::Pl011 { clk },
                clock: "clk_peri",
            })
        };
        let mut mcu = builtin("rp2040_pico");
        set_runtime(&mut mcu, Runtime::Async);
        assert_eq!(Chip::of(&mcu).plan(0), async_on(125_000_000));
        set_runtime(&mut mcu, Runtime::Blocking);
        assert_eq!(Chip::of(&mcu).plan(0), async_on(rp::blocking_peri_hz(&mcu)));
        // Async staged, not yet applied: the panel already shows Async. The
        // tab's PLL is moved to 1500 / 5 / 2 = 150 MHz first, so the applied
        // Blocking and the staged Async give different answers.
        use crate::panels::mcu_module::clock::graph::model::NodeState;
        set_node(&mut mcu, "pll_sys_pd1", NodeState::Index(4));
        assert_eq!(Chip::of(&mcu).plan(0), async_on(150_000_000));
        mcu.pending_runtime = Runtime::Async;
        assert_eq!(Chip::of(&mcu).plan(0), async_on(125_000_000));

        let mut pico2 = builtin("rp2350_pico2");
        set_runtime(&mut pico2, Runtime::Async);
        assert_eq!(Chip::of(&pico2).plan(0), async_on(150_000_000));
    }

    /// A PLL rp-hal refuses panics the board before the UART exists.
    #[test]
    fn an_rp_pll_the_hal_refuses_dooms_every_rate() {
        let chip = Chip {
            rp_blocking_hz: Some(Err(())),
            ..Chip::bare("rp2040", Runtime::Blocking)
        };
        assert_eq!(chip.plan(0), Plan::Doomed(RP_PLL_REFUSED));
        // Async ignores the tab's PLLs, so it is not doomed by them.
        let chip = Chip {
            runtime: Runtime::Async,
            ..chip
        };
        assert!(matches!(chip.plan(0), Plan::Checked(_)));
    }

    /// Set one node of the Clock tab's tree, as the user would.
    fn set_node(
        mcu: &mut Mcu,
        id: &str,
        state: crate::panels::mcu_module::clock::graph::model::NodeState,
    ) {
        use crate::panels::mcu_module::clock::model::ClockConfig;
        let ClockConfig::Graph(gc) = &mut mcu.clock else {
            panic!("a tree")
        };
        gc.graph.node_mut(id).expect(id).state = state;
    }

    /// The tab offers a post divider of 7; rp-hal takes 1..=6, and the
    /// generated `setup_pll_blocking(..).unwrap()` panics on it.
    #[test]
    fn a_pico_on_post_divider_7_is_doomed() {
        use crate::panels::mcu_module::clock::graph::model::NodeState;
        let mut mcu = builtin("rp2040_pico");
        set_runtime(&mut mcu, Runtime::Blocking);
        assert!(matches!(Chip::of(&mcu).plan(0), Plan::Checked(_)));
        set_node(&mut mcu, "pll_sys_pd1", NodeState::Index(6));
        assert_eq!(Chip::of(&mcu).plan(0), Plan::Doomed(RP_PLL_REFUSED));
    }

    /// A 600 MHz VCO: under the RP2040's 750 MHz floor, above the RP2350's 400.
    #[test]
    fn the_vco_floor_is_per_chip() {
        use crate::panels::mcu_module::clock::graph::model::NodeState;
        for (id, ok) in [("rp2040_pico", false), ("rp2350_pico2", true)] {
            let mut mcu = builtin(id);
            set_runtime(&mut mcu, Runtime::Blocking);
            set_node(&mut mcu, "pll_sys_fb", NodeState::Value(50));
            let plan = Chip::of(&mcu).plan(0);
            assert_eq!(matches!(plan, Plan::Checked(_)), ok, "{id}: {plan:?}");
        }
    }

    /// ESP32 and S2 run the UART off APB; the others off the crystal the Clock
    /// tab states.
    #[test]
    fn the_esp_uart_clock_is_apb_or_the_crystal() {
        let on = |divider, clock| Plan::Checked(Source { divider, clock });
        assert_eq!(
            Chip::of(&builtin("esp32")).plan(0),
            on(Divider::EspApb { clk: 80_000_000 }, "APB")
        );
        assert_eq!(
            Chip::of(&builtin("esp32c3")).plan(0),
            on(Divider::EspSclk { clk: 40_000_000 }, "XTAL")
        );
        assert_eq!(
            Chip::of(&builtin("esp32h2")).plan(0),
            on(Divider::EspSclk { clk: 32_000_000 }, "XTAL")
        );
    }

    /// The C2's crystal is edited as a free value between 26 and 40 MHz, but
    /// only those two exist: a value in between is taken as the nearer one.
    #[test]
    fn a_free_crystal_value_snaps_to_a_real_crystal() {
        use crate::panels::mcu_module::clock::graph::model::NodeState;
        use crate::panels::mcu_module::clock::model::ClockConfig;
        let mut mcu = builtin("esp32c2");
        let mut at = |mhz: u32| {
            if let ClockConfig::Graph(gc) = &mut mcu.clock
                && let Some(n) = gc.graph.node_mut("xtal")
                && let NodeState::Source { hz, .. } = &mut n.state
            {
                *hz = mhz * 1_000_000;
            }
            esp_xtal_hz(&mcu)
        };
        assert_eq!(at(26), Some(26_000_000));
        assert_eq!(at(30), Some(26_000_000));
        assert_eq!(at(33), Some(40_000_000));
        assert_eq!(at(40), Some(40_000_000));
    }

    /// Imported STM32s: Blocking builds no driver, Async is not checked yet.
    #[test]
    fn other_stm32_families_say_why_they_are_not_checked() {
        assert_eq!(
            Chip::bare("stm32g4", Runtime::Blocking).plan(1),
            Plan::NotUsed(NOT_USED_RAW)
        );
        assert_eq!(
            Chip::bare("stm32wba", Runtime::Blocking).plan(1),
            Plan::NotUsed(NOT_USED_RAW)
        );
        assert!(matches!(
            Chip::bare("stm32l4", Runtime::Async).plan(1),
            Plan::Unchecked(_)
        ));
        // No clock known at all is "unchecked", not "doomed".
        assert!(matches!(
            Chip::bare("stm32f1", Runtime::Blocking).plan(1),
            Plan::Unchecked(_)
        ));
    }

    /// What the panel prints, for the cases a user meets.
    #[test]
    fn the_hint_says_what_the_wire_gets() {
        let f1_8 = Plan::Checked(Source {
            divider: F1_8,
            clock: "PCLK2",
        });
        let h = hint(&f1_8, 921_600);
        assert_eq!(h.verdict, "panics at boot");
        assert_eq!(h.severity, Some(Severity::Broken));
        assert_eq!(
            h.why,
            "This rate is above the clock / 16, and stm32f1xx-hal asserts against that, so the generated code panics when it sets the UART up."
        );
        assert_eq!(
            h.range.as_deref(),
            Some("range 123 – 500 000 at PCLK2 8 MHz")
        );

        let h = hint(&f1_8, 460_800);
        assert_eq!(h.verdict, "runs at 470 588 baud · 2.12 % off");
        assert!(h.why.contains("close to the edge"), "{}", h.why);

        let nrf = Plan::Checked(Source {
            divider: Divider::NrfTable,
            clock: "UARTE",
        });
        let h = hint(&nrf, 74_880);
        assert_eq!(h.verdict, "runs at 76 923 baud · 76800 setting, 2.73 % off");
        assert_eq!(h.range.as_deref(), Some("fixed rates 1 200 – 1 000 000"));
        // An exact setting names the setting, not a divider.
        assert!(
            hint(&nrf, 921_600)
                .why
                .starts_with("The UARTE's 921600 setting really runs at 941 176 baud."),
            "{}",
            hint(&nrf, 921_600).why
        );

        let h = hint(&Plan::NotUsed(NOT_USED_RAW), 115_200);
        assert_eq!(h.verdict, "not used by the generated code");
        assert_eq!(h.severity, None);
        assert!(h.why.contains("raw"), "{}", h.why);
    }

    /// An exact rate says so instead of "0.00 % off".
    #[test]
    fn an_exact_rate_reads_exact() {
        let esp = Plan::Checked(Source {
            divider: Divider::EspSclk { clk: 40_000_000 },
            clock: "XTAL",
        });
        assert_eq!(hint(&esp, 250_000).verdict, "runs at 250 000 baud · exact");
        assert_eq!(preset_tag(&esp, 250_000).unwrap().0, "exact");
    }

    /// The popup's max follows the clock, and says nothing for an LPUART.
    #[test]
    fn the_popup_max_is_the_range_top() {
        let chip = Chip::of(&builtin("stm32f103c8t6"));
        assert_eq!(
            max_baud_text(&chip, &PinFunction::UsartTx(1)).as_deref(),
            Some("4 500 000 baud at PCLK2 72 MHz")
        );
        assert_eq!(
            max_baud_text(&chip, &PinFunction::UsartRx(3)).as_deref(),
            Some("2 250 000 baud at PCLK1 36 MHz")
        );
        assert_eq!(max_baud_text(&chip, &PinFunction::LpuartTx(1)), None);
        // No driver for UART4: no max either.
        assert_eq!(max_baud_text(&chip, &PinFunction::UsartTx(4)), None);
    }

    #[test]
    fn numbers_are_grouped_by_thousands() {
        assert_eq!(grouped(0_u64), "0");
        assert_eq!(grouped(999_u64), "999");
        assert_eq!(grouped(1_000_u64), "1 000");
        assert_eq!(grouped(4_500_000_u64), "4 500 000");
    }
}
