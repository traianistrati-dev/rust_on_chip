//! What the generated code builds of each I2C: the pads its constructor is
//! handed and the clock it asks for - for the Board tab, whose wires are the
//! firmware's, not the module's. The I2C counterpart of [`super::uart_built`]
//! and [`super::spi_built`].
//!
//! Every backend builds a controller, so there is no role to read. What the
//! module can store beyond the firmware is a pad wired twice (one is taken), a
//! bus short of a line (refused everywhere but on esp-hal, which ties the
//! missing one High), a block the part lacks or another bus runs (the nRF),
//! and a clock the code does not ask for as such (the F1's whole kHz, the
//! nRF's fixed TWIM rates). The clock is the one main.rs asks for, not what a
//! HAL's register maths rounds it to. Each vendor answers from the function
//! its generator decides with:
//!
//! - embassy-stm32 (every STM32 on Async, and every one but the F1 on
//!   Blocking): the pads its peripheral pass moves into a driver
//!   (`FamilyBackend::driver_pins`) - the SCL+SDA pair - at the stored clock.
//! - stm32f1xx-hal (the F1 off Async): `stm32::f1_i2c_built`.
//! - esp-hal: `codegen_esp::i2c_built`, every line wired.
//! - rp-hal and embassy-rp: `rp::i2c_built`.
//! - nrf-hal and embassy-nrf: `nrf::twim_built`.

use std::collections::{BTreeMap, BTreeSet};

use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::{ModuleConfig, ModuleSignal};

/// One I2C module as its chip's generated code builds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum I2cBuilt {
    /// No I2C at all, and why.
    Not(String),
    /// Built, as a controller: from these pads, at this clock.
    Built(I2cShape),
}

/// The shape a built I2C has on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct I2cShape {
    /// The pins (`Pin::number`) its constructor takes.
    pub pins: BTreeSet<usize>,
    /// The SCL clock main.rs asks for, in Hz.
    pub clock_hz: u32,
}

impl Mcu {
    /// Every I2C module, keyed by instance, as this chip's generated code
    /// builds it. `None` for a family no backend here generates I2Cs for - its
    /// modules are read as they are stored.
    pub fn i2cs_built(&self) -> Option<BTreeMap<u8, I2cBuilt>> {
        use super::{family, nrf, rp, stm32};
        use crate::panels::mcu_module::modules::{I2cModuleConfig, VirtualModule};
        let i2cs = self.modules.iter().filter_map(|m| match &m.config {
            ModuleConfig::I2c(c) => Some((m, c)),
            _ => None,
        });
        // A vendor that answers one module at a time.
        let each = |f: &dyn Fn(&VirtualModule, &I2cModuleConfig) -> I2cBuilt| {
            Some(i2cs.clone().map(|(m, c)| (m.instance(), f(m, c))).collect())
        };
        if rp::is_rp(&self.family) {
            return each(&|m, _| rp::i2c_built(self, m.instance()));
        }
        if nrf::is_nrf(&self.family) {
            return each(&|m, _| nrf::twim_built(self, m));
        }
        if family::is_esp(&self.family) {
            let pins: Vec<_> = self.iter_all_pins().collect();
            return each(&|m, c| {
                crate::panels::mcu_module::codegen_esp::i2c_built(&pins, c.clock_hz, m.instance())
            });
        }
        if self.family == "stm32f1" && !self.is_async() {
            return each(&|m, c| stm32::f1_i2c_built(self, m.instance(), c.clock_hz));
        }
        if !self.family.starts_with("stm32") {
            return None;
        }
        // embassy-stm32: what its peripheral pass moved into a driver - once,
        // for every I2C.
        let consumed: BTreeSet<String> = self.backend()?.driver_pins(self).into_iter().collect();
        Some(
            i2cs
                // "Generate driver" off: the user's own code builds it from
                // the raw pads, so it is read as stored.
                .filter(|(m, _)| {
                    !(family::embassy_drivers(&self.family, self.is_async())
                        && self.raw_modules.contains(&(m.kind, m.instance())))
                })
                .map(|(m, c)| {
                    let pins: BTreeSet<usize> = m
                        .connections
                        .iter()
                        .map(|x| x.mcu_pin)
                        .filter(|n| {
                            self.find_pin(*n)
                                .is_some_and(|p| consumed.contains(p.gpio()))
                        })
                        .collect();
                    let has = |s: ModuleSignal| {
                        m.connections
                            .iter()
                            .any(|x| x.signal == s && pins.contains(&x.mcu_pin))
                    };
                    let built = if has(ModuleSignal::Scl) && has(ModuleSignal::Sda) {
                        I2cBuilt::Built(I2cShape {
                            pins,
                            clock_hz: c.clock_hz,
                        })
                    } else {
                        I2cBuilt::Not(
                            "embassy-stm32 is built here from the SCL+SDA pair, and one is not \
                             wired - its pad stays a raw binding"
                                .to_owned(),
                        )
                    };
                    (m.instance(), built)
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins::builtin_for;
    use crate::panels::mcu_module::mcu::Runtime;
    use crate::panels::mcu_module::pins::PinFunction;

    /// A pad's name without a board's note (`P0.21 (row1)` is `P0.21`).
    fn pad(name: &str) -> &str {
        name.split_whitespace().next().unwrap_or(name)
    }

    /// Built-in `id` on `runtime`, exactly `wired` on its pads, every I2C
    /// module's clock `clock_hz`.
    fn chip(id: &str, runtime: Runtime, wired: &[(&str, PinFunction)], clock_hz: u32) -> Mcu {
        let mut mcu = builtin_for(id).expect(id).build_mcu();
        mcu.runtime = runtime;
        mcu.pending_runtime = runtime;
        for (name, f) in wired {
            mcu.iter_all_pins_mut()
                .find(|p| pad(&p.name) == *name)
                .unwrap_or_else(|| panic!("{id} has no {name}"))
                .selected_function = f.clone();
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::I2c(c) = &mut m.config {
                c.clock_hz = clock_hz;
            }
        }
        mcu
    }

    /// I2C `n`'s pads and clock as built - `Err(why)` when it is not built.
    fn built(mcu: &Mcu, n: u8) -> Result<(Vec<String>, u32), String> {
        match mcu
            .i2cs_built()
            .expect("a family with a rule")
            .remove(&n)
            .expect("the I2C")
        {
            I2cBuilt::Not(why) => Err(why),
            I2cBuilt::Built(s) => {
                let mut pads: Vec<String> = s
                    .pins
                    .iter()
                    .map(|n| pad(&mcu.find_pin(*n).unwrap().name).to_owned())
                    .collect();
                pads.sort();
                Ok((pads, s.clock_hz))
            }
        }
    }

    fn names(pads: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = pads.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    /// The RP builds a controller from the pair, at the clock it is given;
    /// half a pair is refused.
    #[test]
    fn an_rp_i2c_needs_the_pair() {
        let pair = [
            ("GP4", PinFunction::I2cSda(0)),
            ("GP5", PinFunction::I2cScl(0)),
        ];
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let rp = chip("rp2040_pico", runtime, &pair, 100_000);
            assert_eq!(
                built(&rp, 0),
                Ok((names(&["GP4", "GP5"]), 100_000)),
                "{runtime:?}"
            );
            let half = chip("rp2040_pico", runtime, &pair[..1], 100_000);
            assert_eq!(
                built(&half, 0),
                Err("SDA and SCL are taken together; wire the missing one".to_owned()),
                "{runtime:?}"
            );
        }
    }

    /// The nRF builds a TWIM at the fixed rate its clock floors to; on
    /// embassy-nrf a SPIM wired on the same TWISPI0 takes the block first,
    /// on nrf-hal the TWIM's init runs last and keeps it.
    #[test]
    fn an_nrf_twim_runs_at_its_rate_on_its_block() {
        let pair = [
            ("P0.26", PinFunction::I2cSda(0)),
            ("P0.27", PinFunction::I2cScl(0)),
        ];
        let twim = chip("nrf52840_dk", Runtime::Blocking, &pair, 300_000);
        assert_eq!(built(&twim, 0), Ok((names(&["P0.26", "P0.27"]), 250_000)));
        let shared = [
            pair[0].clone(),
            pair[1].clone(),
            ("P0.02", PinFunction::SpiSck(0)),
            ("P0.03", PinFunction::SpiMosi(0)),
        ];
        let embassy = chip("nrf52840_dk", Runtime::Async, &shared, 100_000);
        assert_eq!(
            built(&embassy, 0),
            Err("TWISPI0 runs SPIM0 here - the block runs one bus at a time".to_owned())
        );
        let nrf_hal = chip("nrf52840_dk", Runtime::Blocking, &shared, 100_000);
        assert!(built(&nrf_hal, 0).is_ok(), "{:?}", built(&nrf_hal, 0));
    }

    /// esp-hal builds even one line alone, the other tied High.
    #[test]
    fn an_esp_i2c_takes_every_line_it_has() {
        let pair = [
            ("GPIO8", PinFunction::I2cSda(0)),
            ("GPIO9", PinFunction::I2cScl(0)),
        ];
        let esp = chip("esp32s3", Runtime::Blocking, &pair, 400_000);
        assert_eq!(built(&esp, 0), Ok((names(&["GPIO8", "GPIO9"]), 400_000)));
        let one = chip("esp32s3", Runtime::Async, &pair[..1], 400_000);
        assert_eq!(built(&one, 0), Ok((names(&["GPIO8"]), 400_000)));
    }

    /// The F1: stm32f1xx-hal at the whole kHz, and refusing half a pair;
    /// embassy refusing it too, silently in main.rs; a raw module read as
    /// stored.
    #[test]
    fn an_f1_i2c_is_built_as_its_runtime_builds_it() {
        let pair = [
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ];
        let hal = chip("stm32f103c8t6", Runtime::Blocking, &pair, 100_500);
        assert_eq!(built(&hal, 1), Ok((names(&["PB6", "PB7"]), 100_000)));
        let half = chip("stm32f103c8t6", Runtime::Blocking, &pair[..1], 100_000);
        assert_eq!(
            built(&half, 1),
            Err("SDA is not wired, and an I2C bus needs both wires".to_owned())
        );
        let embassy = chip("stm32f103c8t6", Runtime::Async, &pair, 100_500);
        assert_eq!(built(&embassy, 1), Ok((names(&["PB6", "PB7"]), 100_500)));
        let half = chip("stm32f103c8t6", Runtime::Async, &pair[..1], 100_000);
        assert!(built(&half, 1).is_err());
        let mut raw = chip("stm32f103c8t6", Runtime::Async, &pair, 100_000);
        raw.raw_modules.insert((
            crate::panels::mcu_module::modules::ModuleKind::GenericInterfaceI2c,
            1,
        ));
        assert_eq!(raw.i2cs_built().map(|m| m.len()), Some(0));
    }
}
