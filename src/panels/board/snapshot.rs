//! What a chip frame on the Board tab shows: the chip, its runtime, its Virtual
//! Modules and its devices - and, for the links between chips, which pad each
//! module signal sits on and what that pad does.
//!
//! A chip that is not open is read from its files the way OPENING it would:
//! the chip id from `src/main.rs` (Cargo.toml for older projects), a fresh
//! `Mcu` from its definition, `mcu.config` applied, the pins restored from
//! `@pins` or the generated block, then `reconcile_modules`. The same steps in
//! the same order, so the Board never describes a chip differently from the
//! Pins tab that opening it shows. The chip that IS open is built from the
//! live `Mcu` instead ([`ChipView::from_mcu`]), so unsaved changes show on the
//! Board as they are made.

use std::collections::BTreeSet;
use std::path::Path;

use crate::panels::mcu_module::mcu::gui::modules::{custom_var_name, module_base_name};
use crate::panels::mcu_module::mcu::{Mcu, Runtime};
use crate::panels::mcu_module::mcu_config::{self, PinGroup, SavedPins};
use crate::panels::mcu_module::mcu_def::McuDefinition;
use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, ModuleSignal, VirtualModule};
use crate::panels::mcu_module::pins::logic::pin::GpioMode;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

/// One module terminal and the pad it is wired to.
#[derive(Clone, Debug, PartialEq)]
pub struct SignalPin {
    pub signal: ModuleSignal,
    /// Pin number on the chip.
    pub pin: usize,
    /// The pad's name (`PA9`, `GPIO20`); empty when the chip could not say.
    pub pad: String,
    /// What the pad is configured as - the direction of a custom module's
    /// GPIO comes from here.
    pub function: PinFunction,
    /// An open-drain output: two of them on one line make a wired-AND, not a
    /// fight.
    pub open_drain: bool,
}

/// What a chip says about one of its pins: its name, its function, and
/// whether it is open-drain.
pub type PadInfo = (String, PinFunction, bool);

/// One Virtual Module, as a frame shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct ModuleItem {
    /// `USART1`, `I2C1`, or a custom module's own name.
    pub name: String,
    pub kind: ModuleKind,
    pub instance: u8,
    /// The pads it is wired to - what ties a device to it.
    pub pins: BTreeSet<usize>,
    /// Each terminal with its pad, in the module's own order.
    pub signals: Vec<SignalPin>,
    /// Its settings (baud rate, SPI mode, …), for the checks on a link.
    pub config: ModuleConfig,
}

/// One device (a named group of pads on the Pins tab).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceItem {
    pub name: String,
    /// Indices into [`ChipView::modules`] of every module sharing a pad with
    /// the device. Empty for a device on bare pads (an LED on a plain GPIO).
    pub modules: Vec<usize>,
}

/// Everything one chip frame draws.
#[derive(Clone, Debug, PartialEq)]
pub struct ChipView {
    /// The chip's folder under the system root - its identity in the system.
    pub dir: String,
    /// The chip's display name; empty when it could not be told.
    pub chip: String,
    pub runtime: Option<Runtime>,
    pub modules: Vec<ModuleItem>,
    pub devices: Vec<DeviceItem>,
    /// Why the frame has nothing to show (folder gone, not a project, …).
    pub problem: Option<String>,
    /// `Some(I/O millivolts)` for an external part, which is described in
    /// `system.config` rather than read from a project.
    pub external_mv: Option<u32>,
}

impl ChipView {
    /// A frame for a chip whose files could not be read.
    pub fn broken(dir: &str, problem: impl Into<String>) -> Self {
        Self {
            dir: dir.to_owned(),
            chip: String::new(),
            runtime: None,
            modules: Vec::new(),
            devices: Vec::new(),
            problem: Some(problem.into()),
            external_mv: None,
        }
    }

    /// The frame of a chip held as an `Mcu` - the open one, or one just
    /// rebuilt from its files.
    pub fn from_mcu(dir: &str, chip: &str, mcu: &Mcu) -> Self {
        Self::from_parts(
            dir,
            chip,
            Some(mcu.runtime),
            &mcu.modules,
            &mcu.groups,
            |n| {
                mcu.find_pin(n).map(|p| {
                    (
                        p.name.clone(),
                        p.selected_function.clone(),
                        p.io_mode == Some(GpioMode::OpenDrain),
                    )
                })
            },
        )
    }

    /// Build the frame from a chip's parts. `pad` names pin `n` and says what
    /// it is configured as, when the chip is known.
    pub fn from_parts(
        dir: &str,
        chip: &str,
        runtime: Option<Runtime>,
        modules: &[VirtualModule],
        groups: &[PinGroup],
        pad: impl Fn(usize) -> Option<PadInfo>,
    ) -> Self {
        let mut items: Vec<ModuleItem> = modules
            .iter()
            .map(|m| ModuleItem {
                name: module_name(m),
                kind: m.kind,
                instance: m.instance(),
                pins: m.connections.iter().map(|c| c.mcu_pin).collect(),
                signals: m
                    .connections
                    .iter()
                    .map(|c| {
                        let (pad, function, open_drain) = pad(c.mcu_pin).unwrap_or_default();
                        SignalPin {
                            signal: c.signal,
                            pin: c.mcu_pin,
                            pad,
                            function,
                            open_drain,
                        }
                    })
                    .collect(),
                config: m.config.clone(),
            })
            .collect();
        // Peripherals first, in kind-then-instance order, custom modules after
        // them by name: the order `mcu.modules` happens to hold changes with
        // every reconcile, and a frame should not reshuffle on a save.
        items.sort_by(|a, b| {
            (a.kind.is_custom(), a.kind, a.instance, &a.name).cmp(&(
                b.kind.is_custom(),
                b.kind,
                b.instance,
                &b.name,
            ))
        });
        let devices = groups
            .iter()
            .filter(|g| g.is_live())
            .map(|g| DeviceItem {
                name: g.name.trim().to_owned(),
                // A device on an I2C bus has no pad of its own; a Device
                // holding one hangs on that bus.
                modules: items
                    .iter()
                    .enumerate()
                    .filter(|(_, m)| {
                        !m.pins.is_disjoint(&g.pins)
                            || matches!(&m.config, ModuleConfig::I2c(c)
                                if m.kind == ModuleKind::GenericInterfaceI2c
                                    && g.i2c.iter().any(|(inst, uid)| *inst == m.instance
                                        && c.has(crate::panels::mcu_module::modules::I2cDeviceKey::Uid(*uid))))
                    })
                    .map(|(i, _)| i)
                    .collect(),
            })
            .collect();
        Self {
            dir: dir.to_owned(),
            chip: chip.to_owned(),
            runtime,
            modules: items,
            devices,
            problem: None,
            external_mv: None,
        }
    }

    /// The grey line under the chip name: `stm32_main · Blocking`.
    pub fn subtitle(&self) -> String {
        if let Some(mv) = self.external_mv {
            return format!("{} · external part · {}", self.dir, super::parts::volts(mv));
        }
        match self.runtime {
            Some(r) => format!("{} · {}", self.dir, r.as_token()),
            None => self.dir.clone(),
        }
    }

    /// The module with this identity - how a link end finds its module.
    pub fn module(&self, kind: ModuleKind, instance: u8) -> Option<usize> {
        self.modules
            .iter()
            .position(|m| m.kind == kind && m.instance == instance)
    }
}

/// A module's name on the Board: the peripheral instance (`USART1`), or for a
/// custom module the name its code uses (`irq_in`) - the only name it has.
pub fn module_name(m: &VirtualModule) -> String {
    if m.kind.is_custom() {
        custom_var_name(m)
    } else {
        module_base_name(m).to_owned()
    }
}

/// Read the chip in `<root>/<dir>` for its frame.
pub fn read_chip(root: &Path, dir: &str, defs: &[McuDefinition]) -> ChipView {
    let path = root.join(dir);
    if !path.is_dir() {
        return ChipView::broken(dir, "Folder not found");
    }
    let read = |rel: &Path| std::fs::read_to_string(path.join(rel)).ok();
    // LF-normalised, like every buffer the app reads (the markers are matched
    // line by line).
    let main_rs = read(Path::new("src/main.rs")).map(|s| s.replace("\r\n", "\n"));
    let cargo = read(Path::new("Cargo.toml"));
    if main_rs.is_none() && cargo.is_none() {
        return ChipView::broken(dir, "Not a project - no Cargo.toml or src/main.rs");
    }
    let cfg = read(Path::new(mcu_config::FILE_NAME));
    let def = crate::panels::mcu_module::registry::detect_chip_id(
        defs,
        main_rs.as_deref(),
        cargo.as_deref(),
    )
    .and_then(|id| defs.iter().find(|d| d.id == id));
    let Some(def) = def else {
        // No chip to rebuild pins on: what `mcu.config` says, unwired.
        let modules = cfg
            .as_deref()
            .map(|t| mcu_config::parse(t).0)
            .unwrap_or_default();
        let groups = cfg
            .as_deref()
            .map(mcu_config::parse_groups)
            .unwrap_or_default();
        let mut view = ChipView::from_parts(dir, "", None, &modules, &groups, |_| None);
        view.problem = Some("Unknown chip - no marker in main.rs and no known HAL".to_owned());
        return view;
    };
    ChipView::from_mcu(
        dir,
        &def.display_name,
        &rebuild(def, cfg.as_deref(), main_rs.as_deref()),
    )
}

/// The chip as opening its project would leave it: `load_project_from_dir`'s
/// restore, in its order, minus everything that is not the diagram. Like the
/// open, it restores nothing without a `src/main.rs`.
fn rebuild(def: &McuDefinition, cfg: Option<&str>, main_rs: Option<&str>) -> Mcu {
    let mut mcu = def.build_mcu();
    if let Some(src) = main_rs {
        match cfg {
            Some(text) => mcu.apply_mcu_config(text),
            // Saved before `mcu.config` existed: the modules were a comment
            // marker in main.rs.
            None => {
                mcu.modules = crate::panels::mcu_module::modules::persist::parse_from_source(src);
            }
        }
        if let Some(saved) = mcu_config::saved_pins(cfg, src) {
            match &saved {
                SavedPins::ByNumber(pins) => mcu.apply_saved_pins_by_number(pins),
                SavedPins::ByName(pins) => mcu.apply_saved_pins(pins),
            }
        }
    }
    // What the Pins tab does before it draws: the modules follow the pins.
    mcu.reconcile_modules();
    mcu
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::modules::{Connection, UsartModuleConfig};

    fn usart(instance: u8, tx: usize, rx: usize) -> VirtualModule {
        VirtualModule {
            id: format!("_usart_{instance}"),
            kind: ModuleKind::GenericInterfaceUsart,
            name: format!("_USART{instance}"),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart(UsartModuleConfig::new(instance)),
            connections: vec![
                Connection {
                    signal: ModuleSignal::Tx,
                    mcu_pin: tx,
                },
                Connection {
                    signal: ModuleSignal::Rx,
                    mcu_pin: rx,
                },
            ],
        }
    }

    fn group(name: &str, pins: &[usize]) -> PinGroup {
        PinGroup {
            name: name.to_owned(),
            pins: pins.iter().copied().collect(),
            ..Default::default()
        }
    }

    /// A device hangs off every module it shares a pad with; one on bare pads
    /// hangs off none; a device the roster is still naming is not drawn.
    #[test]
    fn devices_attach_to_the_modules_they_share_a_pad_with() {
        let mods = [usart(2, 12, 13), usart(1, 30, 31)];
        let groups = [
            group("GPS", &[12, 13]),
            group("Bridge", &[13, 30]),
            group("Status LED", &[45]),
            group("  ", &[12]),
        ];
        let v = ChipView::from_parts("stm32_main", "STM32F103C8", None, &mods, &groups, |_| None);
        let names: Vec<&str> = v.modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            ["USART1", "USART2"],
            "sorted by instance, `_` dropped"
        );
        let dev: Vec<(&str, &[usize])> = v
            .devices
            .iter()
            .map(|d| (d.name.as_str(), d.modules.as_slice()))
            .collect();
        assert_eq!(
            dev,
            [
                ("GPS", &[1][..]),
                ("Bridge", &[0, 1][..]),
                ("Status LED", &[][..])
            ]
        );
        assert_eq!(v.module(ModuleKind::GenericInterfaceUsart, 2), Some(1));
        assert_eq!(v.module(ModuleKind::GenericInterfaceSpi, 1), None);
    }

    /// A device on an I2C bus has no pad of its own: a Device holding one hangs
    /// off that bus - and off no other bus of the same kind.
    #[test]
    fn a_device_holding_an_i2c_device_hangs_off_its_bus() {
        use crate::panels::mcu_module::modules::I2cModuleConfig;
        let i2c = |instance: u8, scl: usize, sda: usize| VirtualModule {
            id: format!("_i2c_{instance}"),
            kind: ModuleKind::GenericInterfaceI2c,
            name: format!("_I2C{instance}"),
            pos: (0.0, 0.0),
            config: ModuleConfig::I2c(I2cModuleConfig::new(instance)),
            connections: vec![
                Connection {
                    signal: ModuleSignal::Scl,
                    mcu_pin: scl,
                },
                Connection {
                    signal: ModuleSignal::Sda,
                    mcu_pin: sda,
                },
            ],
        };
        let mut mods = [i2c(1, 42, 43), i2c(2, 21, 22)];
        // I2C2 carries a device (uid 1); I2C1 carries none.
        if let ModuleConfig::I2c(c) = &mut mods[1].config {
            c.apply(&crate::panels::mcu_module::modules::I2cDeviceEdit::Add, 0);
        }
        let mut display = group("display", &[]);
        display.i2c = [(2, 1)].into();
        // A key whose device is gone hangs off nothing: the part is not there.
        let mut gone = group("gone", &[]);
        gone.i2c = [(2, 99)].into();
        let v = ChipView::from_parts("c", "STM32F103C8", None, &mods, &[display, gone], |_| None);
        let i2c2 = v.module(ModuleKind::GenericInterfaceI2c, 2).expect("I2C2");
        assert_eq!(v.devices.len(), 2);
        assert_eq!(v.devices[0].modules, vec![i2c2]);
        assert!(
            v.devices[1].modules.is_empty(),
            "{:?}",
            v.devices[1].modules
        );
    }

    #[test]
    fn the_subtitle_names_the_folder_and_the_runtime() {
        let v = ChipView::from_parts(
            "esp32_radio",
            "ESP32-C3",
            Some(Runtime::Async),
            &[],
            &[],
            |_| None,
        );
        assert_eq!(v.subtitle(), "esp32_radio · Async");
        assert_eq!(ChipView::broken("x", "gone").subtitle(), "x");
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("roc_board_snap_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A chip read from disk says what opening it would: the marker's chip,
    /// its runtime, the modules its PINS make (with each pad named), and the
    /// devices of `mcu.config`.
    #[test]
    fn a_chip_is_read_from_its_files() {
        let defs = crate::panels::mcu_module::builtin_definitions();
        let esp = defs.iter().find(|d| d.id == "esp32c3").unwrap();
        // Two free pads of the real chip, whatever they are called.
        let probe = esp.build_mcu();
        let free: Vec<(usize, String)> = probe
            .iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| (p.number, p.name.clone()))
            .take(2)
            .collect();
        let (tx, rx) = (&free[0], &free[1]);

        let root = scratch("read");
        let chip = root.join("radio");
        std::fs::create_dir_all(chip.join("src")).unwrap();
        std::fs::write(
            chip.join("src/main.rs"),
            "// Auto-generated by RustOnChip\n// rust_on_chip:mcu=esp32c3\nfn main() {}\n",
        )
        .unwrap();
        let mut cfg = mcu_config::serialize(
            &[],
            None,
            Runtime::Async,
            crate::panels::mcu_module::modules::ApiStyle::Portable,
        );
        cfg.push_str(&mcu_config::pins_section(
            &[
                (tx.0, PinFunction::UsartTx(0)),
                (rx.0, PinFunction::UsartRx(0)),
            ]
            .into_iter()
            .collect(),
        ));
        cfg.push_str(&mcu_config::groups_section(&[group(
            "Modem",
            &[tx.0, rx.0],
        )]));
        std::fs::write(chip.join(mcu_config::FILE_NAME), cfg).unwrap();

        let v = read_chip(&root, "radio", &defs);
        assert_eq!(v.chip, esp.display_name);
        assert_eq!(v.runtime, Some(Runtime::Async));
        assert_eq!(v.problem, None);
        assert_eq!(v.modules.len(), 1, "{:?}", v.modules);
        let m = &v.modules[0];
        assert_eq!(m.kind, ModuleKind::GenericInterfaceUsart);
        assert_eq!(m.instance, 0);
        let tx_pad = m
            .signals
            .iter()
            .find(|s| s.signal == ModuleSignal::Tx)
            .unwrap();
        assert_eq!((tx_pad.pin, tx_pad.pad.as_str()), (tx.0, tx.1.as_str()));
        assert_eq!(tx_pad.function, PinFunction::UsartTx(0));
        assert_eq!(
            v.devices,
            [DeviceItem {
                name: "Modem".into(),
                modules: vec![0]
            }]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A frame that cannot be filled says why instead of drawing an empty chip.
    #[test]
    fn a_missing_or_foreign_folder_is_reported() {
        let defs = crate::panels::mcu_module::builtin_definitions();
        let root = scratch("broken");
        assert_eq!(
            read_chip(&root, "gone", &defs).problem.as_deref(),
            Some("Folder not found")
        );
        std::fs::create_dir_all(root.join("notes")).unwrap();
        assert!(
            read_chip(&root, "notes", &defs)
                .problem
                .unwrap()
                .starts_with("Not a project")
        );
        std::fs::create_dir_all(root.join("odd/src")).unwrap();
        std::fs::write(root.join("odd/src/main.rs"), "fn main() {}\n").unwrap();
        let odd = read_chip(&root, "odd", &defs);
        assert!(odd.problem.unwrap().starts_with("Unknown chip"));
        assert_eq!(odd.chip, "");
        let _ = std::fs::remove_dir_all(&root);
    }
}
