//! What the generated code builds of each UART: the pads its constructor is
//! handed, and the shape it is built in - for the Board tab, whose wires are
//! the firmware's, not the module's.
//!
//! A module can store more than its chip's generator builds: a flow pad no
//! constructor takes with its direction (embassy-stm32's `UartTx` paces with
//! CTS alone, esp-hal's with RTS alone, the RP builds no flow control at all),
//! a one-way shape a HAL does not offer (stm32f1xx-hal and nrf-hal build the
//! pair), or a UART the generator refuses and explains in main.rs instead.
//! Each vendor answers here from the very function its generator decides
//! with, so the Board cannot draw a wire main.rs does not configure:
//!
//! - embassy-stm32 (every STM32 on Async, and every one but the F1 on
//!   Blocking): the pads its peripheral pass moves into a driver
//!   (`FamilyBackend::driver_pins`), after every refusal and narrowing.
//! - stm32f1xx-hal (the F1 off Async): `stm32::f1_usart_built`.
//! - esp-hal: `codegen_esp::uart_built`, `uart_sigs` over the bus wiring.
//! - rp-hal and embassy-rp: `rp::uart_built`, `uart_pads`.
//! - nrf-hal and embassy-nrf: `nrf::uarte_built`, `uarte_pads`.

use std::collections::{BTreeMap, BTreeSet};

use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::modules::{
    ModuleConfig, ModuleKind, ModuleSignal, UsartDirection, UsartFlow,
};

/// One UART module as its chip's generated code builds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UartBuilt {
    /// No UART at all, and why - the generator's own reason, as the clause
    /// main.rs says it with where its constructor would be.
    Not(String),
    /// Built: from these pads, in this shape.
    Built(UartShape),
}

/// The shape a built UART has on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UartShape {
    /// The pins (`Pin::number`) its constructor takes: the data pads its
    /// direction uses and the flow pads it is built with, nothing else.
    pub pins: BTreeSet<usize>,
    /// The direction it is built as - the pair where a HAL builds nothing
    /// else, whatever the module keeps for another runtime.
    pub direction: UsartDirection,
    /// TX and RX swapped inside the USART.
    pub swapped: bool,
    /// Its RTS pad drives an RS-485 transceiver's DE, on this board.
    pub de: bool,
}

impl UartShape {
    /// The flow control it is built with, read from the pads it takes.
    pub fn flow(&self, signals: &[(ModuleSignal, usize)]) -> UsartFlow {
        use ModuleSignal::*;
        if self.de {
            return UsartFlow::De;
        }
        let takes = |wanted: &[ModuleSignal]| {
            signals
                .iter()
                .any(|(s, pin)| wanted.contains(s) && self.pins.contains(pin))
        };
        match (takes(&[Cts, LpCts]), takes(&[Rts, LpRts])) {
            (true, true) => UsartFlow::CtsRts,
            (true, false) => UsartFlow::Cts,
            (false, true) => UsartFlow::Rts,
            (false, false) => UsartFlow::None,
        }
    }
}

impl Mcu {
    /// Every USART, UART and LPUART module, keyed by `(kind, instance)`, as
    /// this chip's generated code builds it. `None` for a family no backend
    /// here generates UARTs for - its modules are read as they are stored.
    pub fn uarts_built(&self) -> Option<BTreeMap<(ModuleKind, u8), UartBuilt>> {
        use super::{family, nrf, rp, stm32};
        use crate::panels::mcu_module::modules::UsartModuleConfig;
        let uarts = self.modules.iter().filter_map(|m| match &m.config {
            ModuleConfig::Usart(c) | ModuleConfig::Lpuart(c) => Some((m, c)),
            _ => None,
        });
        // A vendor that answers one instance at a time.
        let each = |f: &dyn Fn(u8, &UsartModuleConfig) -> UartBuilt| {
            Some(
                uarts
                    .clone()
                    .map(|(m, c)| ((m.kind, m.instance()), f(m.instance(), c)))
                    .collect(),
            )
        };
        if rp::is_rp(&self.family) {
            return each(&|i, _| rp::uart_built(self, i));
        }
        if nrf::is_nrf(&self.family) {
            return each(&|i, _| nrf::uarte_built(self, i));
        }
        if family::is_esp(&self.family) {
            let pins: Vec<_> = self.iter_all_pins().collect();
            return each(&|i, c| crate::panels::mcu_module::codegen_esp::uart_built(&pins, c, i));
        }
        if self.family == "stm32f1" && !self.is_async() {
            return each(&|i, _| stm32::f1_usart_built(self, i));
        }
        if !self.family.starts_with("stm32") {
            return None;
        }
        // embassy-stm32: what its peripheral pass moved into a driver - once,
        // for every UART.
        let consumed: BTreeSet<String> = self.backend()?.driver_pins(self).into_iter().collect();
        // A module whose "Generate driver" is off is the user's own code's:
        // its pads stay raw bindings for it, so it is read as stored.
        let raw = |m: &crate::panels::mcu_module::modules::VirtualModule| {
            family::embassy_drivers(&self.family, self.is_async())
                && self.raw_modules.contains(&(m.kind, m.instance()))
        };
        let line_extras = crate::panels::mcu_module::stm32_pin_data::usart_has_swap_invert(
            self.usart_ip.as_deref(),
        );
        Some(
            uarts
                .filter(|(m, _)| !raw(m))
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
                    let has = |wanted: &[ModuleSignal]| {
                        m.connections
                            .iter()
                            .any(|x| wanted.contains(&x.signal) && pins.contains(&x.mcu_pin))
                    };
                    let built = if has(&[Tx, Rx, LpTx, LpRx]) {
                        UartBuilt::Built(UartShape {
                            direction: c.direction,
                            swapped: c.swap_rx_tx && line_extras,
                            de: c.flow == UsartFlow::De && has(&[Rts, LpRts]),
                            pins,
                        })
                    } else {
                        UartBuilt::Not("its main.rs says why".to_owned())
                    };
                    ((m.kind, m.instance()), built)
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins::builtin_for;
    use crate::panels::mcu_module::codegen::{nrf, rp, stm32};
    use crate::panels::mcu_module::mcu::Runtime;
    use crate::panels::mcu_module::modules::{UsartMode, UsartModuleConfig};
    use crate::panels::mcu_module::pins::PinFunction;

    /// A pad's name without a board's note (`P0.21 (row1)` is `P0.21`).
    fn pad(name: &str) -> &str {
        name.split_whitespace().next().unwrap_or(name)
    }

    /// Built-in `id` on `runtime`, exactly `wired` on its pads - set directly,
    /// so the canvas pairs no pad on its own - its UART set by `set`.
    fn chip(
        id: &str,
        runtime: Runtime,
        wired: &[(&str, PinFunction)],
        set: impl Fn(&mut UsartModuleConfig),
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
            if let ModuleConfig::Usart(c) = &mut m.config {
                set(c);
            }
        }
        mcu
    }

    /// The pads the only UART is built from, sorted - `None` when it is not
    /// built - and the direction it is built as.
    fn built(mcu: &Mcu) -> Option<(Vec<String>, UsartDirection)> {
        let all = mcu.uarts_built().expect("a family with a rule");
        assert_eq!(all.len(), 1, "{all:?}");
        match all.into_values().next().unwrap() {
            UartBuilt::Not(_) => None,
            UartBuilt::Built(s) => {
                let mut pads: Vec<String> = s
                    .pins
                    .iter()
                    .map(|n| pad(&mcu.find_pin(*n).unwrap().name).to_owned())
                    .collect();
                pads.sort();
                Some((pads, s.direction))
            }
        }
    }

    fn names(pads: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = pads.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    /// The RP is handed its data pads alone, on either runtime: neither rp-hal
    /// nor embassy-rp takes CTS or RTS here. A pair missing a pad is refused.
    #[test]
    fn an_rp_uart_takes_no_flow_pad() {
        let all = [
            ("GP0", PinFunction::UsartTx(0)),
            ("GP1", PinFunction::UsartRx(0)),
            ("GP2", PinFunction::UsartCts(0)),
            ("GP3", PinFunction::UsartRts(0)),
        ];
        for runtime in [Runtime::Blocking, Runtime::Async] {
            let mcu = chip("rp2040_pico", runtime, &all, |c| c.flow = UsartFlow::CtsRts);
            assert_eq!(
                built(&mcu),
                Some((names(&["GP0", "GP1"]), UsartDirection::TxRx)),
                "{runtime:?}"
            );
            let one = chip("rp2040_pico", runtime, &all[..1], |_| {});
            assert_eq!(built(&one), None, "{runtime:?}: TX alone is no pair");
            let tx_only = chip("rp2040_pico", runtime, &all, |c| {
                c.direction = UsartDirection::TxOnly
            });
            assert_eq!(
                built(&tx_only),
                Some((names(&["GP0"]), UsartDirection::TxOnly)),
                "{runtime:?}"
            );
        }
    }

    /// embassy-nrf: each half takes the one flow pad it paces with, the pair
    /// both or neither. nrf-hal: the pair, its flow pads as wired.
    #[test]
    fn an_nrf_uarte_takes_the_pads_its_hal_does() {
        let all = [
            ("P0.06", PinFunction::UsartTx(0)),
            ("P1.08", PinFunction::UsartRx(0)),
            ("P0.02", PinFunction::UsartCts(0)),
            ("P0.03", PinFunction::UsartRts(0)),
        ];
        let tx_only = |c: &mut UsartModuleConfig| c.direction = UsartDirection::TxOnly;
        let on_async = chip("nrf52833_microbit_v2", Runtime::Async, &all, tx_only);
        assert_eq!(
            built(&on_async),
            Some((names(&["P0.06", "P0.02"]), UsartDirection::TxOnly))
        );
        let lone_cts = chip("nrf52833_microbit_v2", Runtime::Async, &all[..3], |_| {});
        assert_eq!(
            built(&lone_cts),
            Some((names(&["P0.06", "P1.08"]), UsartDirection::TxRx)),
            "a lone CTS beside the pair is left out"
        );
        let nrf_hal = chip("nrf52840_dk", Runtime::Blocking, &all, tx_only);
        assert_eq!(
            built(&nrf_hal),
            Some((
                names(&["P0.06", "P1.08", "P0.02", "P0.03"]),
                UsartDirection::TxRx
            ))
        );
        // A lone flow pad: nrf-hal connects it but turns no HWFC on.
        let lone = chip("nrf52840_dk", Runtime::Blocking, &all[..3], |_| {});
        assert_eq!(
            built(&lone),
            Some((names(&["P0.06", "P1.08"]), UsartDirection::TxRx))
        );
        // A block the part lacks is said as main.rs says it.
        let no_block = chip(
            "nrf52832_dk",
            Runtime::Blocking,
            &[
                ("P0.06", PinFunction::UsartTx(1)),
                ("P0.08", PinFunction::UsartRx(1)),
            ],
            |_| {},
        );
        assert_eq!(
            nrf::uarte_built(&no_block, 1),
            UartBuilt::Not("the nRF52832 has no UARTE1".to_owned())
        );
    }

    /// esp-hal's `UartTx` takes RTS and its `UartRx` CTS - the other way
    /// round from embassy - and a stored single wire is built as the pair.
    #[test]
    fn an_esp_uart_takes_its_halves_flow_pads() {
        let all = [
            ("GPIO4", PinFunction::UsartTx(1)),
            ("GPIO5", PinFunction::UsartRx(1)),
            ("GPIO6", PinFunction::UsartCts(1)),
            ("GPIO7", PinFunction::UsartRts(1)),
        ];
        let tx_only = chip("esp32s3", Runtime::Blocking, &all, |c| {
            c.direction = UsartDirection::TxOnly;
            c.flow = UsartFlow::CtsRts;
        });
        assert_eq!(
            built(&tx_only),
            Some((names(&["GPIO4", "GPIO7"]), UsartDirection::TxOnly))
        );
        let no_flow = chip("esp32s3", Runtime::Async, &all, |_| {});
        assert_eq!(
            built(&no_flow),
            Some((names(&["GPIO4", "GPIO5"]), UsartDirection::TxRx)),
            "a flow pad the flow does not name is no pad of the line"
        );
        let single = chip("esp32s3", Runtime::Blocking, &all[..2], |c| {
            c.direction = UsartDirection::HalfDuplexOnTx
        });
        assert_eq!(
            built(&single),
            Some((names(&["GPIO4", "GPIO5"]), UsartDirection::TxRx))
        );
    }

    /// The F1: stm32f1xx-hal builds the pair and no flow control; embassy
    /// on Async builds what its constructors take, after every narrowing.
    #[test]
    fn an_f1_usart_is_built_as_its_runtime_builds_it() {
        let all = [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA11", PinFunction::UsartCts(1)),
            ("PA12", PinFunction::UsartRts(1)),
        ];
        let rtscts = |c: &mut UsartModuleConfig| {
            c.mode = UsartMode::Dma;
            c.flow = UsartFlow::CtsRts;
        };
        for runtime in [Runtime::Blocking, Runtime::Rtic] {
            let hal = chip("stm32f103c8t6", runtime, &all, rtscts);
            assert_eq!(
                built(&hal),
                Some((names(&["PA9", "PA10"]), UsartDirection::TxRx)),
                "{runtime:?}"
            );
            let one = chip("stm32f103c8t6", runtime, &all[..1], |_| {});
            assert_eq!(built(&one), None, "{runtime:?}");
        }
        let embassy = chip("stm32f103c8t6", Runtime::Async, &all, rtscts);
        assert_eq!(
            built(&embassy),
            Some((
                names(&["PA9", "PA10", "PA11", "PA12"]),
                UsartDirection::TxRx
            ))
        );
        // CTS alone with the pair: embassy has no constructor for it.
        let cts_only = chip("stm32f103c8t6", Runtime::Async, &all[..3], |c| {
            c.mode = UsartMode::Dma;
            c.flow = UsartFlow::Cts;
        });
        assert_eq!(
            built(&cts_only),
            Some((names(&["PA9", "PA10"]), UsartDirection::TxRx))
        );
        // "Generate driver" off: the user's own code builds it from the raw
        // pads, so it is read as stored - no answer here.
        let mut raw = chip("stm32f103c8t6", Runtime::Async, &all, rtscts);
        raw.raw_modules
            .insert((ModuleKind::GenericInterfaceUsart, 1));
        assert_eq!(raw.uarts_built().map(|m| m.len()), Some(0));
    }

    /// The reason a UART is not built is its generator's own.
    #[test]
    fn a_uart_that_is_not_built_says_why() {
        let one = chip(
            "stm32f103c8t6",
            Runtime::Blocking,
            &[("PA9", PinFunction::UsartTx(1))],
            |_| {},
        );
        assert_eq!(
            stm32::f1_usart_built(&one, 1),
            UartBuilt::Not(
                "stm32f1xx-hal builds a Serial only from the TX+RX pair, and one is not wired"
                    .to_owned()
            )
        );
        assert_eq!(
            stm32::f1_usart_built(&one, 4),
            UartBuilt::Not("stm32f1xx-hal has no Serial for UART4".to_owned())
        );
        let rp = chip(
            "rp2040_pico",
            Runtime::Blocking,
            &[("GP0", PinFunction::UsartTx(0))],
            |_| {},
        );
        let UartBuilt::Not(why) = rp::uart_built(&rp, 0) else {
            panic!("a pair with one pad is refused");
        };
        assert!(why.starts_with("its RX pad is not wired"), "{why}");
    }
}
