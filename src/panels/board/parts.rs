//! External parts: what sits on the board beside the chips but is no chip
//! project of this IDE - an FPGA, a sensor, a radio module. Nothing is
//! generated for one; it exists so the chips' links to it can be drawn and
//! checked.
//!
//! A part is described by hand: a name, what it is, its I/O voltage, and its
//! interfaces, each a bus with named pins. On the Board it is a frame like a
//! chip's, and [`Part::view`] turns it into the same [`ChipView`] - so linking
//! to it, drawing the wires and checking them is exactly the code chips use.
//! Its interfaces take the place of Virtual Modules, keyed the same way: a
//! module kind and an instance number that is never re-used within the part.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::snapshot::{ChipView, ModuleItem, SignalPin};
use crate::panels::mcu_module::modules::model::CustomModuleConfig;
use crate::panels::mcu_module::modules::{
    CanModuleConfig, I2cModuleConfig, ModuleConfig, ModuleKind, ModuleSignal, SpiModuleConfig,
    SpiRole, UsartModuleConfig,
};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

/// The I/O voltages a part can be set to, in millivolts.
pub const VOLTAGES: [u32; 5] = [1200, 1800, 2500, 3300, 5000];

/// The I/O level every chip project is taken to have: the chips this IDE
/// knows run their pins at 3.3 V, and a definition says nothing else.
pub const CHIP_IO_MV: u32 = 3300;

/// `3.3 V`.
pub fn volts(mv: u32) -> String {
    if mv.is_multiple_of(1000) {
        format!("{} V", mv / 1000)
    } else {
        format!("{:.1} V", mv as f32 / 1000.0)
    }
}

/// What an interface of a part is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartBus {
    Uart,
    /// The part answers: the chip on the other end drives the clock.
    SpiSlave,
    /// The part drives the clock.
    SpiMaster,
    /// A target (a sensor, a register bank) - the chip is the controller.
    I2c,
    Can,
    /// Loose lines: an interrupt, a reset, a done flag.
    Gpio,
}

impl PartBus {
    pub const ALL: [PartBus; 6] = [
        PartBus::Uart,
        PartBus::SpiSlave,
        PartBus::SpiMaster,
        PartBus::I2c,
        PartBus::Can,
        PartBus::Gpio,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PartBus::Uart => "UART",
            PartBus::SpiSlave => "SPI slave",
            PartBus::SpiMaster => "SPI master",
            PartBus::I2c => "I2C target",
            PartBus::Can => "CAN",
            PartBus::Gpio => "GPIO lines",
        }
    }

    /// The Virtual Module kind it stands in for on the Board.
    pub fn kind(self) -> ModuleKind {
        match self {
            PartBus::Uart => ModuleKind::GenericInterfaceUsart,
            PartBus::SpiSlave | PartBus::SpiMaster => ModuleKind::GenericInterfaceSpi,
            PartBus::I2c => ModuleKind::GenericInterfaceI2c,
            PartBus::Can => ModuleKind::GenericInterfaceCan,
            PartBus::Gpio => ModuleKind::Custom,
        }
    }

    /// The roles a pin of this interface can have.
    pub fn roles(self) -> &'static [PinRole] {
        use PinRole::*;
        match self {
            PartBus::Uart | PartBus::Can => &[Tx, Rx],
            PartBus::SpiSlave | PartBus::SpiMaster => &[Sck, Mosi, Miso, Cs],
            PartBus::I2c => &[Scl, Sda],
            PartBus::Gpio => &[In, Out, OpenDrain],
        }
    }

    /// The pins a new interface starts with: one of each role (one output for
    /// GPIO), named after it - the user renames them to the part's pins.
    pub fn default_pins(self) -> Vec<PartPin> {
        let roles: &[PinRole] = match self {
            PartBus::Gpio => &[PinRole::Out],
            _ => self.roles(),
        };
        roles
            .iter()
            .map(|r| PartPin {
                name: r.label().to_owned(),
                role: *r,
            })
            .collect()
    }

    /// The rate a new interface starts with (baud or bit rate), 0 for none.
    pub fn default_rate(self) -> u32 {
        match self {
            PartBus::Uart => 115_200,
            PartBus::Can => 500_000,
            _ => 0,
        }
    }
}

/// What one pin of an interface carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PinRole {
    Tx,
    Rx,
    Sck,
    Mosi,
    Miso,
    Cs,
    Scl,
    Sda,
    In,
    Out,
    OpenDrain,
}

impl PinRole {
    pub fn label(self) -> &'static str {
        match self {
            PinRole::Tx => "TX",
            PinRole::Rx => "RX",
            PinRole::Sck => "SCK",
            PinRole::Mosi => "MOSI",
            PinRole::Miso => "MISO",
            PinRole::Cs => "CS",
            PinRole::Scl => "SCL",
            PinRole::Sda => "SDA",
            PinRole::In => "IN",
            PinRole::Out => "OUT",
            PinRole::OpenDrain => "OUT (open-drain)",
        }
    }

    fn signal(self, bus: PartBus) -> ModuleSignal {
        match (self, bus) {
            (PinRole::Tx, PartBus::Can) => ModuleSignal::CanTx,
            (PinRole::Rx, PartBus::Can) => ModuleSignal::CanRx,
            (PinRole::Tx, _) => ModuleSignal::Tx,
            (PinRole::Rx, _) => ModuleSignal::Rx,
            (PinRole::Sck, _) => ModuleSignal::Sck,
            (PinRole::Mosi, _) => ModuleSignal::Mosi,
            (PinRole::Miso, _) => ModuleSignal::Miso,
            (PinRole::Cs, _) => ModuleSignal::Nss,
            (PinRole::Scl, _) => ModuleSignal::Scl,
            (PinRole::Sda, _) => ModuleSignal::Sda,
            (PinRole::In | PinRole::Out | PinRole::OpenDrain, _) => ModuleSignal::CustomPin,
        }
    }

    /// What a chip's pad would be configured as to do the same.
    fn function(self) -> PinFunction {
        match self {
            PinRole::In => PinFunction::GpioInput,
            PinRole::Out | PinRole::OpenDrain => PinFunction::GpioOutput,
            _ => PinFunction::Unset,
        }
    }
}

/// One pin of an interface.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PartPin {
    /// The part's own name for it: `pin 15`, `SDO`, `CDONE`.
    pub name: String,
    pub role: PinRole,
}

/// One interface of a part.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interface {
    pub bus: PartBus,
    /// Its identity in links, together with the bus's module kind. Unique in
    /// the part and never re-used (see [`Part::add_interface`]).
    pub instance: u8,
    /// Empty = the bus and the instance (`SPI1`, `gpio2`).
    #[serde(default)]
    pub name: String,
    /// UART baud rate or CAN bit rate; 0 where there is none.
    #[serde(default)]
    pub rate: u32,
    /// SPI mode 0..=3.
    #[serde(default)]
    pub spi_mode: u8,
    #[serde(default)]
    pub pins: Vec<PartPin>,
}

impl Interface {
    /// What the Board calls it.
    pub fn display_name(&self) -> String {
        let name = self.name.trim();
        if !name.is_empty() {
            return name.to_owned();
        }
        let prefix = match self.bus {
            PartBus::Uart => "UART",
            PartBus::SpiSlave | PartBus::SpiMaster => "SPI",
            PartBus::I2c => "I2C",
            PartBus::Can => "CAN",
            PartBus::Gpio => "gpio",
        };
        format!("{prefix}{}", self.instance)
    }
}

fn default_io_mv() -> u32 {
    CHIP_IO_MV
}

/// An external part on the board.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Part {
    /// Its identity in the system - shares one namespace with the chip
    /// folders, since a link end names either.
    pub id: String,
    /// What it is: `iCE40UP5K`.
    #[serde(default)]
    pub label: String,
    /// I/O voltage in millivolts.
    #[serde(default = "default_io_mv")]
    pub io_mv: u32,
    #[serde(default)]
    pub interfaces: Vec<Interface>,
    /// Top-left of its frame; `None` = not placed yet.
    #[serde(default)]
    pub pos: Option<(f32, f32)>,
    /// The next instance number to hand out. Only ever grows, and is saved:
    /// a number freed by a removal must never come back, or a link to the
    /// removed interface would quietly carry over to the new one.
    #[serde(default)]
    pub next_instance: u8,
}

impl Part {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            label: String::new(),
            io_mv: CHIP_IO_MV,
            interfaces: Vec::new(),
            pos: None,
            next_instance: 0,
        }
    }

    /// Add an interface with its default pins. Its instance is a number this
    /// part has never handed out - removed interfaces included, so a remove
    /// and an add in one edit cannot carry the removed one's links over.
    pub fn add_interface(&mut self, bus: PartBus) {
        let past_all = self
            .interfaces
            .iter()
            .map(|i| i.instance.saturating_add(1))
            .max()
            .unwrap_or(0);
        let instance = self.next_instance.max(past_all);
        self.next_instance = instance.saturating_add(1);
        self.interfaces.push(Interface {
            bus,
            instance,
            name: String::new(),
            rate: bus.default_rate(),
            spi_mode: 0,
            pins: bus.default_pins(),
        });
    }

    /// The part as a frame and as link ends: each interface a module, each
    /// pin a signal on a pad named after the part's own pin.
    pub fn view(&self) -> ChipView {
        let modules = self
            .interfaces
            .iter()
            .map(|i| {
                let kind = i.bus.kind();
                let signals: Vec<SignalPin> = i
                    .pins
                    .iter()
                    .enumerate()
                    .map(|(k, p)| SignalPin {
                        signal: p.role.signal(i.bus),
                        // Unique within the part: pads are told apart by
                        // number when wires find their rows.
                        pin: i.instance as usize * 1000 + kind_slot(kind) * 100 + k,
                        // An unnamed pin goes by its role, so no message
                        // or wire label ends up with a blank.
                        pad: if p.name.trim().is_empty() {
                            p.role.label().to_owned()
                        } else {
                            p.name.trim().to_owned()
                        },
                        function: p.role.function(),
                        open_drain: p.role == PinRole::OpenDrain,
                    })
                    .collect();
                ModuleItem {
                    name: i.display_name(),
                    kind,
                    instance: i.instance,
                    pins: signals.iter().map(|s| s.pin).collect::<BTreeSet<_>>(),
                    config: config_of(i, &signals),
                    signals,
                    not_built: None,
                }
            })
            .collect();
        ChipView {
            dir: self.id.clone(),
            chip: if self.label.trim().is_empty() {
                "External part".to_owned()
            } else {
                self.label.trim().to_owned()
            },
            runtime: None,
            modules,
            devices: Vec::new(),
            problem: None,
            external_mv: Some(self.io_mv),
        }
    }
}

/// Keeps pad numbers of two interfaces with the same instance but different
/// kinds apart.
fn kind_slot(kind: ModuleKind) -> usize {
    match kind {
        ModuleKind::GenericInterfaceUsart => 1,
        ModuleKind::GenericInterfaceSpi => 2,
        ModuleKind::GenericInterfaceI2c => 3,
        ModuleKind::GenericInterfaceCan => 4,
        _ => 5,
    }
}

/// The settings a link check reads, as a module of that kind would hold them.
fn config_of(i: &Interface, signals: &[SignalPin]) -> ModuleConfig {
    match i.bus {
        PartBus::Uart => {
            let mut c = UsartModuleConfig::new(i.instance);
            if i.rate > 0 {
                c.baud_rate = i.rate;
            }
            ModuleConfig::Usart(c)
        }
        PartBus::SpiSlave | PartBus::SpiMaster => {
            let mut c = SpiModuleConfig::new(i.instance);
            c.role = if i.bus == PartBus::SpiSlave {
                SpiRole::Slave
            } else {
                SpiRole::Master
            };
            c.mode = i.spi_mode.min(3);
            ModuleConfig::Spi(c)
        }
        PartBus::I2c => ModuleConfig::I2c(I2cModuleConfig::new(i.instance)),
        PartBus::Can => {
            let mut c = CanModuleConfig::new(i.instance);
            if i.rate > 0 {
                c.bitrate = i.rate;
            }
            ModuleConfig::Can(c)
        }
        PartBus::Gpio => {
            let mut c = CustomModuleConfig::new(i.instance);
            c.pins = signals.iter().map(|s| s.pin).collect();
            ModuleConfig::Custom(c)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fpga() -> Part {
        let mut p = Part::new("fpga");
        p.label = "iCE40UP5K".into();
        p.io_mv = 1800;
        p.add_interface(PartBus::SpiSlave);
        p.add_interface(PartBus::Gpio);
        p.interfaces[1].name = "cdone".into();
        p.interfaces[1].pins[0].name = "pin 7".into();
        p
    }

    /// A part becomes a frame like a chip's: its interfaces are modules of the
    /// kind they stand for, its pins signals with the part's own names.
    #[test]
    fn a_part_reads_as_a_chip_with_modules() {
        let v = fpga().view();
        assert_eq!(v.dir, "fpga");
        assert_eq!(v.chip, "iCE40UP5K");
        assert_eq!(v.external_mv, Some(1800));
        assert_eq!(v.modules.len(), 2);
        let spi = &v.modules[0];
        assert_eq!(
            (spi.kind, spi.name.as_str()),
            (ModuleKind::GenericInterfaceSpi, "SPI0")
        );
        assert!(matches!(&spi.config, ModuleConfig::Spi(c) if c.role == SpiRole::Slave));
        let sigs: Vec<(ModuleSignal, &str)> = spi
            .signals
            .iter()
            .map(|s| (s.signal, s.pad.as_str()))
            .collect();
        assert_eq!(
            sigs,
            [
                (ModuleSignal::Sck, "SCK"),
                (ModuleSignal::Mosi, "MOSI"),
                (ModuleSignal::Miso, "MISO"),
                (ModuleSignal::Nss, "CS"),
            ]
        );
        let gpio = &v.modules[1];
        assert_eq!(
            (gpio.kind, gpio.name.as_str()),
            (ModuleKind::Custom, "cdone")
        );
        assert_eq!(gpio.signals[0].function, PinFunction::GpioOutput);
        assert_eq!(gpio.signals[0].pad, "pin 7");
        let all: BTreeSet<usize> = v
            .modules
            .iter()
            .flat_map(|m| m.pins.iter().copied())
            .collect();
        assert_eq!(all.len(), 5, "every pad number distinct");
    }

    /// A number is never handed out twice - not after the last interface is
    /// removed, not across kinds, not after a save and a reload.
    #[test]
    fn interface_instances_are_never_reused() {
        let mut p = Part::new("x");
        p.add_interface(PartBus::Uart);
        p.add_interface(PartBus::SpiSlave);
        let inst: Vec<u8> = p.interfaces.iter().map(|i| i.instance).collect();
        assert_eq!(inst, [0, 1]);
        p.interfaces.pop();
        p.add_interface(PartBus::SpiMaster);
        assert_eq!(
            p.interfaces.last().unwrap().instance,
            2,
            "the removed SPI's 1 stays retired"
        );
        let text = ron::to_string(&p).unwrap();
        let mut back: Part = ron::from_str(&text).unwrap();
        back.interfaces.clear();
        back.add_interface(PartBus::Uart);
        assert_eq!(back.interfaces[0].instance, 3);
        assert_eq!(back.interfaces[0].rate, 115_200);
        // A part written before the counter existed still numbers past its
        // interfaces.
        let mut old = p.clone();
        old.next_instance = 0;
        old.add_interface(PartBus::Can);
        assert_eq!(old.interfaces.last().unwrap().instance, 3);
    }

    /// An unnamed pin goes by its role.
    #[test]
    fn an_unnamed_pin_goes_by_its_role() {
        let mut p = Part::new("x");
        p.add_interface(PartBus::Gpio);
        p.interfaces[0].pins[0].name.clear();
        assert_eq!(p.view().modules[0].signals[0].pad, "OUT");
    }

    #[test]
    fn voltages_read_as_volts() {
        assert_eq!(volts(3300), "3.3 V");
        assert_eq!(volts(5000), "5 V");
        assert_eq!(volts(1800), "1.8 V");
    }
}
