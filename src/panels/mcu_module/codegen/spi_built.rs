//! What the generated code builds of each SPI: the pads its constructor is
//! handed, the role and the mode it is built in - for the Board tab, whose
//! wires are the firmware's, not the module's. The SPI counterpart of
//! [`super::uart_built`].
//!
//! A module can store more than its chip's generator builds: a role the HAL
//! is never asked for (every backend but esp-hal builds a master), an NSS pad
//! no constructor takes (embassy-stm32 and the RP leave it alone, the nRF has
//! none), a shape a HAL does not build, or an SPI the generator refuses. Each
//! vendor answers from the very function its generator decides with:
//!
//! - embassy-stm32 (every STM32 on Async, and every one but the F1 on
//!   Blocking): the pads its peripheral pass moves into a driver
//!   (`FamilyBackend::driver_pins`) - SCK and MOSI, MISO when wired.
//! - stm32f1xx-hal (the F1 off Async): `stm32::f1_spi_built`.
//! - esp-hal: `codegen_esp::spi_built`, every wired line, with the role and
//!   mode `family::esp_spi_configs` builds.
//! - rp-hal and embassy-rp: `rp::spi_built`, `spi_pads`.
//! - nrf-hal and embassy-nrf: `nrf::spim_built`, its SPIM gates and blocks.

use std::collections::{BTreeMap, BTreeSet};

use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::{ModuleConfig, ModuleSignal, SpiRole};

/// One SPI module as its chip's generated code builds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpiBuilt {
    /// No SPI at all, and why.
    Not(String),
    /// Built, then switched off at run time, and why: nrf-hal builds every
    /// bus on a shared block, and the last init takes the block over.
    Off(String),
    /// Built: from these pads, as this role and mode.
    Built(SpiShape),
}

/// The shape a built SPI has on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpiShape {
    /// The pins (`Pin::number`) the firmware drives or reads as this SPI's
    /// lines: the ones its constructor takes, and a chip-select it drives
    /// itself as a GPIO (stm32f1xx-hal).
    pub pins: BTreeSet<usize>,
    pub role: SpiRole,
    /// The SPI mode (CPOL/CPHA) it is built with.
    pub mode: u8,
}

impl Mcu {
    /// Every SPI module, keyed by instance, as this chip's generated code
    /// builds it. `None` for a family no backend here generates SPIs for - its
    /// modules are read as they are stored.
    pub fn spis_built(&self) -> Option<BTreeMap<u8, SpiBuilt>> {
        use super::{family, nrf, rp, stm32};
        use crate::panels::mcu_module::modules::{SpiModuleConfig, VirtualModule};
        let spis = self.modules.iter().filter_map(|m| match &m.config {
            ModuleConfig::Spi(c) => Some((m, c)),
            _ => None,
        });
        // A vendor that answers one module at a time.
        let each = |f: &dyn Fn(&VirtualModule, &SpiModuleConfig) -> SpiBuilt| {
            Some(spis.clone().map(|(m, c)| (m.instance(), f(m, c))).collect())
        };
        if rp::is_rp(&self.family) {
            return each(&|m, _| rp::spi_built(self, m.instance()));
        }
        if nrf::is_nrf(&self.family) {
            return each(&|m, c| nrf::spim_built(self, m, c.mode));
        }
        if family::is_esp(&self.family) {
            let pins: Vec<_> = self.iter_all_pins().collect();
            let built = family::esp_spi_configs(self);
            return each(&|m, c| {
                let c = built.get(&m.instance()).unwrap_or(c);
                crate::panels::mcu_module::codegen_esp::spi_built(&pins, c, m.instance())
            });
        }
        if self.family == "stm32f1" && !self.is_async() {
            return each(&|m, c| stm32::f1_spi_built(self, m.instance(), c.mode));
        }
        if !self.family.starts_with("stm32") {
            return None;
        }
        // embassy-stm32: what its peripheral pass moved into a driver - once,
        // for every SPI.
        let consumed: BTreeSet<String> = self.backend()?.driver_pins(self).into_iter().collect();
        Some(
            spis
                // "Generate driver" off: the user's own code builds it from
                // the raw pads, so it is read as stored.
                .filter(|(m, _)| {
                    !(family::embassy_drivers(&self.family, self.is_async())
                        && self.raw_modules.contains(&(m.kind, m.instance())))
                })
                .map(|(m, c)| {
                    use ModuleSignal::*;
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
                    let built = if has(Sck) && has(Mosi) {
                        SpiBuilt::Built(SpiShape {
                            pins,
                            role: SpiRole::Master,
                            mode: c.mode,
                        })
                    } else {
                        SpiBuilt::Not(
                            "embassy-stm32 is built here from SCK and MOSI, and one is not \
                             wired - its pads stay raw bindings"
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
    use crate::panels::mcu_module::modules::SpiModuleConfig;
    use crate::panels::mcu_module::pins::PinFunction;

    /// A pad's name without a board's note (`P0.21 (row1)` is `P0.21`).
    fn pad(name: &str) -> &str {
        name.split_whitespace().next().unwrap_or(name)
    }

    /// Built-in `id` on `runtime`, exactly `wired` on its pads, its SPI set by
    /// `set`.
    pub(crate) fn chip(
        id: &str,
        runtime: Runtime,
        wired: &[(&str, PinFunction)],
        set: impl Fn(&mut SpiModuleConfig),
    ) -> Mcu {
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
            if let ModuleConfig::Spi(c) = &mut m.config {
                set(c);
            }
        }
        mcu
    }

    /// The pads, role and mode the only SPI is built with - `Err(why)` when it
    /// is not built.
    fn built(mcu: &Mcu) -> Result<(Vec<String>, SpiRole, u8), String> {
        let all = mcu.spis_built().expect("a family with a rule");
        assert_eq!(all.len(), 1, "{all:?}");
        match all.into_values().next().unwrap() {
            SpiBuilt::Not(why) | SpiBuilt::Off(why) => Err(why),
            SpiBuilt::Built(s) => {
                let mut pads: Vec<String> = s
                    .pins
                    .iter()
                    .map(|n| pad(&mcu.find_pin(*n).unwrap().name).to_owned())
                    .collect();
                pads.sort();
                Ok((pads, s.role, s.mode))
            }
        }
    }

    fn names(pads: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = pads.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    fn slave_mode3(c: &mut SpiModuleConfig) {
        c.role = SpiRole::Slave;
        c.mode = 3;
    }

    /// The RP builds a master in mode 0 from its lines and never an NSS;
    /// rp-hal has no receive-only SPI, embassy-rp has.
    #[test]
    fn an_rp_spi_is_a_mode_0_master_without_nss() {
        let all = [
            ("GP2", PinFunction::SpiSck(0)),
            ("GP3", PinFunction::SpiMosi(0)),
            ("GP4", PinFunction::SpiMiso(0)),
            ("GP5", PinFunction::SpiNss(0)),
        ];
        let rp = chip("rp2040_pico", Runtime::Blocking, &all, slave_mode3);
        assert_eq!(
            built(&rp),
            Ok((names(&["GP2", "GP3", "GP4"]), SpiRole::Master, 0))
        );
        let rx = [all[0].clone(), all[2].clone()];
        let blocking = chip("rp2040_pico", Runtime::Blocking, &rx, |_| {});
        let why = built(&blocking).expect_err("rp-hal has no receive-only SPI");
        assert!(
            why.starts_with("rp-hal builds every SPI from at least MOSI and SCK"),
            "{why}"
        );
        let on_async = chip("rp2040_pico", Runtime::Async, &rx, |_| {});
        assert_eq!(
            built(&on_async),
            Ok((names(&["GP2", "GP4"]), SpiRole::Master, 0))
        );
    }

    /// The nRF builds a SPIM master in the stored mode from its lines; it
    /// needs SCK, and embassy-nrf more than SCK alone.
    #[test]
    fn an_nrf_spim_is_built_through_its_gates() {
        let all = [
            ("P0.02", PinFunction::SpiSck(2)),
            ("P0.03", PinFunction::SpiMosi(2)),
            ("P0.09", PinFunction::SpiMiso(2)),
        ];
        let nrf_hal = chip("nrf52840_dk", Runtime::Blocking, &all, slave_mode3);
        assert_eq!(
            built(&nrf_hal),
            Ok((names(&["P0.02", "P0.03", "P0.09"]), SpiRole::Master, 3))
        );
        let no_sck = chip("nrf52840_dk", Runtime::Blocking, &all[1..], |_| {});
        assert_eq!(
            built(&no_sck),
            Err("SCK is not wired, and a SPIM without a clock is nothing".to_owned())
        );
        let sck_only = chip("nrf52840_dk", Runtime::Async, &all[..1], |_| {});
        assert_eq!(
            built(&sck_only),
            Err(
                "only SCK is wired, and embassy-nrf builds a SPIM with MOSI, MISO or both"
                    .to_owned()
            )
        );
        // SPIM0 beside TWIM0 on their one TWISPI0: nrf-hal builds both and
        // the TWIM's init, which runs last, switches the SPIM off; embassy-nrf
        // builds the SPIM, which comes first, and leaves the TWIM out.
        let shared = [
            ("P0.02", PinFunction::SpiSck(0)),
            ("P0.03", PinFunction::SpiMosi(0)),
            ("P0.26", PinFunction::I2cSda(0)),
            ("P0.27", PinFunction::I2cScl(0)),
        ];
        let spim0 = |mcu: &Mcu| mcu.spis_built().expect("an nRF").remove(&0).expect("SPIM0");
        let nrf_hal = chip("nrf52840_dk", Runtime::Blocking, &shared, |_| {});
        assert_eq!(
            spim0(&nrf_hal),
            SpiBuilt::Off(
                "TWIM0's init runs after it on nrf-hal and takes TWISPI0 over - the block runs \
                 one bus at a time"
                    .to_owned()
            )
        );
        let embassy = chip("nrf52840_dk", Runtime::Async, &shared, |_| {});
        assert!(
            matches!(spim0(&embassy), SpiBuilt::Built(_)),
            "{:?}",
            spim0(&embassy)
        );
    }

    /// esp-hal takes every line it is wired with, CS included, in the role
    /// the module asks for.
    #[test]
    fn an_esp_spi_takes_every_line_in_its_role() {
        let all = [
            ("GPIO12", PinFunction::SpiSck(2)),
            ("GPIO11", PinFunction::SpiMosi(2)),
            ("GPIO13", PinFunction::SpiMiso(2)),
            ("GPIO10", PinFunction::SpiNss(2)),
        ];
        let slave = chip("esp32s3", Runtime::Blocking, &all, |c| {
            c.role = SpiRole::Slave
        });
        let (pads, role, _) = built(&slave).expect("built");
        assert_eq!(pads, names(&["GPIO12", "GPIO11", "GPIO13", "GPIO10"]));
        assert_eq!(role, SpiRole::Slave);
    }

    /// The F1: stm32f1xx-hal builds a master once SCK or MOSI is there, its
    /// NSS a GPIO it drives; embassy builds one from SCK and MOSI, and never
    /// takes the NSS.
    #[test]
    fn an_f1_spi_is_built_as_its_runtime_builds_it() {
        let all = [
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PA4", PinFunction::SpiNss(1)),
        ];
        let tx = [all[0].clone(), all[1].clone(), all[3].clone()];
        let hal = chip("stm32f103c8t6", Runtime::Blocking, &tx, slave_mode3);
        assert_eq!(
            built(&hal),
            Ok((names(&["PA5", "PA7", "PA4"]), SpiRole::Master, 3))
        );
        let miso_only = chip("stm32f103c8t6", Runtime::Blocking, &all[2..3], |_| {});
        assert!(built(&miso_only).is_err());
        let embassy = chip("stm32f103c8t6", Runtime::Async, &all, slave_mode3);
        assert_eq!(
            built(&embassy),
            Ok((names(&["PA5", "PA7", "PA6"]), SpiRole::Master, 3))
        );
        let rx = [all[0].clone(), all[2].clone()];
        let receive_only = chip("stm32f103c8t6", Runtime::Async, &rx, |_| {});
        assert!(built(&receive_only).is_err());
        // "Generate driver" off: the user's own code builds it - as stored.
        let mut raw = chip("stm32f103c8t6", Runtime::Async, &all, |_| {});
        raw.raw_modules.insert((
            crate::panels::mcu_module::modules::ModuleKind::GenericInterfaceSpi,
            1,
        ));
        assert_eq!(raw.spis_built().map(|m| m.len()), Some(0));
    }
}
