//! Virtual electronic modules attached to the MCU (Phase 1: model + auto-wire).
//!
//! - [`model`]    — the module data types (kinds, signals, config, connections).
//! - [`autowire`] — pick compatible MCU pins for a new module.
//!
//! Modules live on the [`Mcu`](crate::panels::mcu_module::mcu::Mcu) and are added
//! via `Mcu::add_module`, which auto-wires them to USART pins and sets those pins'
//! functions (the "auto-connect" behaviour).

pub mod autowire;
pub mod model;
pub mod notes;
pub mod persist;

pub use model::{
    AddressIssue, ApiStyle, AsyncBusMode, BREAK_FILTERS, BreakInputConfig, BreakPolarity, CanMode,
    CanModuleConfig, Connection, DacModuleConfig, HspiMode, HspiModuleConfig, I2cDevice,
    I2cDeviceEdit, I2cDeviceKey, I2cModuleConfig, I2cRow, I2sClockPolarity, I2sDirection,
    I2sFormat, I2sMode, I2sModuleConfig, I2sStandard, LcdCamMode, LcdCamModuleConfig,
    McpwmModuleConfig, ModuleConfig, ModuleKind, ModuleSignal, OspiMemoryType, OspiMode,
    OspiModuleConfig, Parity, ParlIoBitOrder, ParlIoDirection, ParlIoModuleConfig, ParlIoWidth,
    PcntChannelCfg, PcntCtrlMode, PcntEdgeMode, PcntModuleConfig, PwmChannelConfig, PwmCounting,
    PwmMode, PwmOutput, PwmPolarity, QSPI_MEMORY_SIZES, QspiAddressSize, QspiModuleConfig,
    RmtDirection, RmtModuleConfig, SaiBlockConfig, SaiDataSize, SaiMode, SaiModuleConfig,
    SaiStereoMono, SaiTxRx, SdmmcModuleConfig, SpiBitOrder, SpiModuleConfig, SpiRole, StopBits,
    TimerModuleConfig, TouchModuleConfig, TouchScan, TouchThreshold, UsartDirection, UsartFlow,
    UsartMode, UsartModuleConfig, UsbModuleConfig, UsbRole, VirtualModule, XspiMemoryType,
    XspiMode, XspiModuleConfig, address_issue, format_i2c_address, module_signal_of,
    parse_i2c_address, usart_data_bits,
};

pub use notes::{ModuleNotes, NotesKey};

use std::collections::BTreeMap;

/// USART module configs keyed by peripheral instance — consumed by codegen to
/// drive the generated USART init (baud rate, parity, stop bits).
pub fn usart_configs(modules: &[VirtualModule]) -> BTreeMap<u8, UsartModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Usart(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// LPUART module configs keyed by peripheral instance.
///
/// Deliberately a SEPARATE map from [`usart_configs`] even though both hold a
/// `UsartModuleConfig`: LPUART1 and USART1 are different peripherals that share
/// the instance number 1, so merging them would silently drop one.
pub fn lpuart_configs(modules: &[VirtualModule]) -> BTreeMap<u8, UsartModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Lpuart(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// PWM module configs keyed by TIMER — one module per timer, however many of
/// its channels are wired.
pub fn timer_configs(modules: &[VirtualModule]) -> BTreeMap<u8, TimerModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Timer(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// SPI module configs keyed by peripheral instance (mode + clock for codegen).
pub fn spi_configs(modules: &[VirtualModule]) -> BTreeMap<u8, SpiModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Spi(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// The parallel port's config, keyed by its (always 0) instance.
pub fn parl_io_configs(modules: &[VirtualModule]) -> BTreeMap<u8, ParlIoModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::ParlIo(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// The LCD_CAM module config, keyed by instance — of which there is one.
pub fn lcd_cam_configs(modules: &[VirtualModule]) -> BTreeMap<u8, LcdCamModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::LcdCam(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// The touch module config, keyed by instance — of which there is one.
pub fn touch_configs(modules: &[VirtualModule]) -> BTreeMap<u8, TouchModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Touch(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// MCPWM module configs keyed by UNIT.
pub fn mcpwm_configs(modules: &[VirtualModule]) -> BTreeMap<u8, McpwmModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Mcpwm(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// PCNT module configs keyed by UNIT — the instance is the unit here.
pub fn pcnt_configs(modules: &[VirtualModule]) -> BTreeMap<u8, PcntModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Pcnt(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// RMT module configs keyed by CHANNEL — the instance is the channel here.
pub fn rmt_configs(modules: &[VirtualModule]) -> BTreeMap<u8, RmtModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Rmt(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// HSPI module configs keyed by controller instance.
pub fn hspi_configs(modules: &[VirtualModule]) -> BTreeMap<u8, HspiModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Hspi(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// XSPI module configs keyed by PORT.
pub fn xspi_configs(modules: &[VirtualModule]) -> BTreeMap<u8, XspiModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Xspi(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// OCTOSPI module configs keyed by PORT.
pub fn ospi_configs(modules: &[VirtualModule]) -> BTreeMap<u8, OspiModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Ospi(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// The QUADSPI module's config, if the project has one.
pub fn qspi_config(modules: &[VirtualModule]) -> Option<QspiModuleConfig> {
    modules.iter().find_map(|m| match &m.config {
        ModuleConfig::Qspi(c) => Some(c.clone()),
        _ => None,
    })
}

/// SDMMC module configs keyed by controller instance.
pub fn sdmmc_configs(modules: &[VirtualModule]) -> BTreeMap<u8, SdmmcModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Sdmmc(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// SAI module configs keyed by unit.
pub fn sai_configs(modules: &[VirtualModule]) -> BTreeMap<u8, SaiModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Sai(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// DAC module configs keyed by peripheral instance.
pub fn dac_configs(modules: &[VirtualModule]) -> BTreeMap<u8, DacModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Dac(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// I2S module configs keyed by the SPI instance they run on.
pub fn i2s_configs(modules: &[VirtualModule]) -> BTreeMap<u8, I2sModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::I2s(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// I2C module configs keyed by peripheral instance (clock for codegen).
pub fn i2c_configs(modules: &[VirtualModule]) -> BTreeMap<u8, I2cModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::I2c(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// CAN module configs keyed by peripheral instance (bit rate for codegen).
pub fn can_configs(modules: &[VirtualModule]) -> BTreeMap<u8, CanModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Can(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

/// USB module configs keyed by instance (VID/PID/product for codegen).
pub fn usb_configs(modules: &[VirtualModule]) -> BTreeMap<u8, UsbModuleConfig> {
    let mut map = BTreeMap::new();
    for m in modules {
        if let ModuleConfig::Usb(c) = &m.config {
            map.insert(c.instance, c.clone());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    #[test]
    fn add_usart_module_wires_a_valid_pair() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert_eq!(mcu.modules.len(), 1);

        let m = &mcu.modules[0];
        let n = m.instance();
        let tx = m.pin_for(ModuleSignal::Tx).unwrap();
        let rx = m.pin_for(ModuleSignal::Rx).unwrap();
        assert_ne!(tx, rx, "TX and RX must be different pins");

        // Auto-connect set the pins to the matching USART functions.
        assert_eq!(
            mcu.find_pin(tx).unwrap().selected_function,
            PinFunction::UsartTx(n)
        );
        assert_eq!(
            mcu.find_pin(rx).unwrap().selected_function,
            PinFunction::UsartRx(n)
        );
    }

    #[test]
    fn two_modules_use_distinct_instances_and_pins() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert_eq!(mcu.modules.len(), 2);

        let n0 = mcu.modules[0].instance();
        let n1 = mcu.modules[1].instance();
        assert_ne!(n0, n1, "second module must pick a free USART instance");

        // No MCU pin is shared between the two modules.
        let pins0: Vec<usize> = mcu.modules[0]
            .connections
            .iter()
            .map(|c| c.mcu_pin)
            .collect();
        let pins1: Vec<usize> = mcu.modules[1]
            .connections
            .iter()
            .map(|c| c.mcu_pin)
            .collect();
        assert!(pins0.iter().all(|p| !pins1.contains(p)));
    }

    /// Removing a module resets the pins it was wired to back to `Unset`.
    #[test]
    fn remove_module_resets_pins() {
        let mut mcu = create_stm32f103c8tx();
        mcu.add_module(ModuleKind::GenericInterfaceUsart);
        let tx = mcu.modules[0].pin_for(ModuleSignal::Tx).unwrap();
        let rx = mcu.modules[0].pin_for(ModuleSignal::Rx).unwrap();
        let id = mcu.modules[0].id.clone();

        mcu.remove_module(&id);

        assert!(mcu.modules.is_empty());
        assert_eq!(
            mcu.find_pin(tx).unwrap().selected_function,
            PinFunction::Unset,
            "TX pin freed"
        );
        assert_eq!(
            mcu.find_pin(rx).unwrap().selected_function,
            PinFunction::Unset,
            "RX pin freed"
        );
    }

    /// Ctrl+Z: a snapshot taken before an add/remove restores BOTH the modules
    /// and the pins (add unassigns them, remove re-assigns them).
    #[test]
    fn module_undo_reverts_add_and_remove() {
        let mut mcu = create_stm32f103c8tx();

        // Undo an ADD → back to no modules, pins freed.
        mcu.push_module_undo("Add USART".into());
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let tx = mcu.modules[0].pin_for(ModuleSignal::Tx).unwrap();
        assert_eq!(mcu.undo_modules().as_deref(), Some("Add USART"));
        assert!(mcu.modules.is_empty(), "add undone");
        assert_eq!(
            mcu.find_pin(tx).unwrap().selected_function,
            PinFunction::Unset,
            "pins freed by the undo"
        );

        // Undo a REMOVE → the module (and its pins) come back.
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let id = mcu.modules[0].id.clone();
        mcu.push_module_undo("Remove USART1".into());
        mcu.remove_module(&id);
        assert!(mcu.modules.is_empty());
        mcu.undo_modules();
        assert_eq!(mcu.modules.len(), 1, "remove undone");
        assert_ne!(
            mcu.find_pin(tx).unwrap().selected_function,
            PinFunction::Unset,
            "pins re-assigned by the undo"
        );

        // Discard drops the snapshot without applying it.
        mcu.push_module_undo("x".into());
        mcu.discard_last_module_undo();
        assert!(!mcu.can_undo_modules());
    }

    /// A bus on F103 I2C1 with its pads in "sensors" and two devices.
    fn grouped_bus() -> (crate::panels::mcu_module::mcu::Mcu, u8, Vec<I2cDeviceKey>) {
        use super::I2cDeviceEdit as E;
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let bus = mcu.modules[0].clone();
        let inst = bus.instance();
        mcu.join_group_module(&bus, "sensors");
        mcu.edit_i2c_device(inst, E::Add);
        mcu.edit_i2c_device(inst, E::Add);
        let keys = i2c_keys(&mcu);
        (mcu, inst, keys)
    }

    fn i2c_keys(mcu: &crate::panels::mcu_module::mcu::Mcu) -> Vec<I2cDeviceKey> {
        match &mcu.modules[0].config {
            ModuleConfig::I2c(c) => c.rows().iter().map(|r| r.key).collect(),
            _ => unreachable!(),
        }
    }

    fn device_of(
        mcu: &crate::panels::mcu_module::mcu::Mcu,
        inst: u8,
        k: I2cDeviceKey,
    ) -> Option<String> {
        mcu.group_of_i2c_device(inst, k).map(|g| g.name.clone())
    }

    /// A device of a bus is in NO Device - not its bus's, though the bus's pads
    /// are in "sensors" - until it is put in one, and an empty name takes it
    /// out again. The Device made for it alone is gone once it leaves.
    #[test]
    fn an_i2c_device_is_in_no_device_until_put_in_one() {
        let (mut mcu, inst, k) = grouped_bus();
        assert_eq!(device_of(&mcu, inst, k[0]), None, "it followed its bus");
        assert!(mcu.join_group_i2c(inst, k[1], "display"));
        assert_eq!(device_of(&mcu, inst, k[1]).as_deref(), Some("display"));
        assert_eq!(device_of(&mcu, inst, k[0]), None, "its sibling stays out");
        assert!(
            !mcu.join_group_i2c(inst, k[1], "display"),
            "already there: no change"
        );
        assert!(mcu.join_group_i2c(inst, k[1], ""));
        assert_eq!(device_of(&mcu, inst, k[1]), None);
        assert!(
            !mcu.groups.iter().any(|g| g.name == "display"),
            "the emptied Device is finished"
        );
        // And the bus's own Device never gained it along the way.
        let sensors = mcu.groups.iter().find(|g| g.name == "sensors").unwrap();
        assert!(sensors.i2c.is_empty(), "{:?}", sensors.i2c);
    }

    /// A Device that still holds an I2C device is not finished when its last
    /// PAD leaves - and a rename onto a taken name carries its I2C devices.
    #[test]
    fn a_device_holding_an_i2c_device_survives_its_pads() {
        let (mut mcu, inst, k) = grouped_bus();
        let free = mcu
            .iter_all_pins()
            .find(|p| {
                !p.reserved
                    && !mcu.modules[0]
                        .connections
                        .iter()
                        .any(|c| c.mcu_pin == p.number)
            })
            .map(|p| p.number)
            .unwrap();
        mcu.join_group(free, "display");
        assert!(mcu.join_group_i2c(inst, k[0], "display"));
        mcu.join_group(free, "");
        assert!(
            mcu.groups.iter().any(|g| g.name == "display"),
            "dropped with an I2C device in it"
        );

        let at = mcu.groups.iter().position(|g| g.name == "display").unwrap();
        mcu.rename_group(at, "sensors");
        let sensors = mcu.groups.iter().find(|g| g.name == "sensors").unwrap();
        assert!(!sensors.i2c.is_empty(), "the merge lost the I2C device");
    }

    /// An undone Add rolls the bus's uids back, and the Device the removed
    /// device was in still holds its uid: the next device must NOT walk into
    /// that Device.
    #[test]
    fn an_undone_add_hands_nobody_its_device() {
        use super::I2cDeviceEdit as E;
        let (mut mcu, inst, _) = grouped_bus();
        assert!(mcu.edit_i2c_device(inst, E::Add));
        let k = *i2c_keys(&mcu).last().unwrap();
        assert!(mcu.join_group_i2c(inst, k, "display"));
        mcu.undo_modules();
        assert_eq!(i2c_keys(&mcu).len(), 2, "the add is undone");
        assert!(mcu.edit_i2c_device(inst, E::Add));
        let fresh = *i2c_keys(&mcu).last().unwrap();
        assert_ne!(fresh, k, "the uid was handed out again");
        assert_eq!(
            device_of(&mcu, inst, fresh),
            None,
            "it walked into \"display\""
        );
    }

    /// The legacy single address has no entry to hold a uid: grouping it mints
    /// one, and nothing it generates changes.
    #[test]
    fn grouping_the_legacy_device_mints_it() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.address = 0x3C;
        }
        assert_eq!(i2c_keys(&mcu), vec![I2cDeviceKey::Implicit]);
        assert!(mcu.join_group_i2c(inst, I2cDeviceKey::Implicit, "oled"));
        let k = i2c_keys(&mcu);
        assert!(matches!(k[..], [I2cDeviceKey::Uid(_)]), "{k:?}");
        assert_eq!(device_of(&mcu, inst, k[0]).as_deref(), Some("oled"));
        if let ModuleConfig::I2c(c) = &mcu.modules[0].config {
            assert_eq!(c.primary_address(), 0x3C);
        }
    }

    /// A legacy bus (one address, no list) with that device grouped from the
    /// roster.
    fn legacy_bus_with_a_module_added() -> (crate::panels::mcu_module::mcu::Mcu, u8) {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.address = 0x3C;
        }
        mcu.push_module_undo("Add USART".into());
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        (mcu, inst)
    }

    fn bus_keys(mcu: &crate::panels::mcu_module::mcu::Mcu, inst: u8) -> Vec<I2cDeviceKey> {
        mcu.i2c_bus(inst)
            .unwrap()
            .rows()
            .iter()
            .map(|r| r.key)
            .collect()
    }

    /// Grouping a legacy device mints its bus - in the undo history too, so
    /// undoing an EARLIER module action does not take the device out of the
    /// Device it was put in afterwards.
    #[test]
    fn undoing_past_the_mint_keeps_the_device_in_its_device() {
        let (mut mcu, inst) = legacy_bus_with_a_module_added();
        assert!(mcu.join_group_i2c(inst, I2cDeviceKey::Implicit, "display"));
        mcu.undo_modules();
        let k = bus_keys(&mcu, inst);
        assert!(
            matches!(k[..], [I2cDeviceKey::Uid(_)]),
            "unminted again: {k:?}"
        );
        assert_eq!(device_of(&mcu, inst, k[0]).as_deref(), Some("display"));
    }

    /// The same through the device's own first edit: undoing the rename
    /// undoes the name, not the Device it was put in after.
    #[test]
    fn undoing_a_first_rename_keeps_the_device_in_its_device() {
        use super::I2cDeviceEdit as E;
        let (mut mcu, inst) = legacy_bus_with_a_module_added();
        assert!(mcu.edit_i2c_device(inst, E::Name(I2cDeviceKey::Implicit, "oled".into())));
        let k = bus_keys(&mcu, inst)[0];
        assert!(mcu.join_group_i2c(inst, k, "display"));
        mcu.undo_modules();
        let k = bus_keys(&mcu, inst)[0];
        assert_eq!(device_of(&mcu, inst, k).as_deref(), Some("display"));
        assert_eq!(
            mcu.i2c_bus(inst).unwrap().device(k).unwrap().0,
            "",
            "the name is undone"
        );
    }

    /// A removed device's uid is not handed to the next one while an undo can
    /// still bring the removed one back - or the undo would put it in the new
    /// one's Device.
    #[test]
    fn an_undone_remove_does_not_inherit_a_newer_devices_device() {
        use super::I2cDeviceEdit as E;
        let (mut mcu, inst, k) = grouped_bus();
        assert!(mcu.edit_i2c_device(inst, E::Remove(k[1])));
        assert!(mcu.edit_i2c_device(inst, E::Add));
        let fresh = *bus_keys(&mcu, inst).last().unwrap();
        assert_ne!(fresh, k[1], "the removed device's uid was handed out again");
        assert!(mcu.join_group_i2c(inst, fresh, "clock"));
        mcu.undo_modules();
        mcu.undo_modules();
        assert_eq!(device_of(&mcu, inst, k[1]), None, "it inherited \"clock\"");
    }

    /// A removal nobody can answer is retired: its bus went, or a mint
    /// re-keyed the device it named.
    #[test]
    fn a_removal_of_a_device_that_is_gone_is_retired() {
        use super::I2cDeviceEdit as E;
        let (mut mcu, inst, k) = grouped_bus();
        mcu.i2c_remove_confirm = Some((inst, k[1]));
        mcu.reconcile_modules();
        assert!(mcu.i2c_remove_confirm.is_some(), "a live one stays");
        let id = mcu.modules[0].id.clone();
        mcu.remove_module(&id);
        mcu.reconcile_modules();
        assert_eq!(mcu.i2c_remove_confirm, None);

        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.address = 0x3C;
        }
        mcu.i2c_remove_confirm = Some((inst, I2cDeviceKey::Implicit));
        assert!(
            mcu.edit_i2c_device(inst, E::Add),
            "mints, re-keying the device"
        );
        mcu.reconcile_modules();
        assert_eq!(mcu.i2c_remove_confirm, None);
    }

    /// The panel's edit and the canvas's arrive in one batch: a first edit
    /// that mints the bus does not strand the next one's positional key.
    #[test]
    fn one_batch_pins_every_key_before_the_first_mint() {
        use super::{I2cDevice, I2cDeviceEdit as E};
        use crate::panels::mcu_module::mcu::gui::i2c_devices::{I2cAct, apply_acts};
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.devices = vec![
                I2cDevice {
                    name: "a".into(),
                    address: 0x10,
                    uid: 0,
                },
                I2cDevice {
                    name: "b".into(),
                    address: 0x11,
                    uid: 0,
                },
            ];
        }
        apply_acts(
            &mut mcu,
            vec![
                (inst, I2cAct::Edit(E::Remove(I2cDeviceKey::Unminted(0)))),
                (
                    inst,
                    I2cAct::Edit(E::Address(I2cDeviceKey::Unminted(1), 0x50)),
                ),
            ],
        );
        let c = mcu.i2c_bus(inst).unwrap();
        assert_eq!(c.devices.len(), 1);
        assert_eq!(
            (c.devices[0].name.as_str(), c.devices[0].address),
            ("b", 0x50)
        );

        // A legacy device named on the canvas while "+ device" is clicked in
        // the panel, in the same frame.
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.address = 0x3C;
        }
        apply_acts(
            &mut mcu,
            vec![
                (inst, I2cAct::Edit(E::Add)),
                (
                    inst,
                    I2cAct::Edit(E::Name(I2cDeviceKey::Implicit, "oled".into())),
                ),
            ],
        );
        let names: Vec<&str> = mcu
            .i2c_bus(inst)
            .unwrap()
            .devices
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, vec!["oled", ""]);
    }

    /// A dragged device keeps its place by uid - a legacy one is minted for it -
    /// "Reset to auto position" puts it back into the column, removing the
    /// device drops its place, and a module group's reset resets its devices.
    #[test]
    fn a_dragged_device_keeps_its_place_until_reset_or_removed() {
        use super::I2cDeviceEdit as E;
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        if let ModuleConfig::I2c(c) = &mut mcu.modules[0].config {
            c.address = 0x3C;
        }
        mcu.move_i2c_device(inst, I2cDeviceKey::Implicit, Some((10.0, 20.0)));
        let k = i2c_keys(&mcu)[0];
        let I2cDeviceKey::Uid(uid) = k else {
            panic!("not minted: {k:?}")
        };
        assert_eq!(mcu.i2c_child_pos.get(&(inst, uid)), Some(&(10.0, 20.0)));
        mcu.move_i2c_device(inst, k, None);
        assert!(mcu.i2c_child_pos.is_empty(), "reset");

        mcu.move_i2c_device(inst, k, Some((5.0, 5.0)));
        assert!(mcu.join_group_i2c(inst, k, "display"));
        assert!(mcu.device_is_manual("display"));
        mcu.reset_device_position("display");
        assert!(
            mcu.i2c_child_pos.is_empty(),
            "the group's reset resets its device"
        );

        mcu.move_i2c_device(inst, k, Some((5.0, 5.0)));
        assert!(mcu.edit_i2c_device(inst, E::Remove(k)));
        assert!(
            mcu.i2c_child_pos.is_empty(),
            "the removed device's place went with it"
        );
    }

    /// The places are saved and read back - only the ones whose device is
    /// still there.
    #[test]
    fn dragged_device_places_round_trip() {
        use crate::panels::mcu_module::mcu_config::{i2c_pos_section, parse_i2c_pos};
        let (mut mcu, inst, k) = grouped_bus();
        mcu.move_i2c_device(inst, k[0], Some((-120.5, 44.0)));
        let I2cDeviceKey::Uid(u) = k[0] else {
            unreachable!()
        };
        mcu.i2c_child_pos.insert((inst, 999), (1.0, 1.0));
        let text = mcu.mcu_config_text();
        assert!(
            text.contains(&format!("@i2cpos\ni2c{inst}/{u}=-120.5,44\n")),
            "{text}"
        );
        assert!(
            !text.contains("/999="),
            "a dead device's place was saved: {text}"
        );
        let back = parse_i2c_pos(&text);
        assert_eq!(back.get(&(inst, u)), Some(&(-120.5, 44.0)));
        assert_eq!(parse_i2c_pos(&i2c_pos_section(&back)), back);
        assert!(parse_i2c_pos("@i2cpos\ni2c1/x=1,2\ni2c1/3=a,2\n").is_empty());
        // And an open puts them back.
        let (mut reopened, ..) = grouped_bus();
        reopened.apply_mcu_config(&text);
        assert_eq!(
            reopened.i2c_child_pos.get(&(inst, u)),
            Some(&(-120.5, 44.0))
        );
    }

    /// "Put in Device" on a device's box goes through the same door as the
    /// roster: into the Device named, and out of every Device with no name.
    #[test]
    fn put_in_device_from_the_canvas_groups_the_device() {
        use crate::panels::mcu_module::mcu::gui::i2c_devices::{I2cAct, apply_acts};
        let (mut mcu, inst, k) = grouped_bus();
        apply_acts(
            &mut mcu,
            vec![(inst, I2cAct::Group(k[0], "display".into()))],
        );
        assert_eq!(device_of(&mcu, inst, k[0]).as_deref(), Some("display"));
        apply_acts(&mut mcu, vec![(inst, I2cAct::Group(k[0], String::new()))]);
        assert_eq!(device_of(&mcu, inst, k[0]), None);
    }

    /// A device picked on the canvas speaks for ITS Device, not its bus's.
    #[test]
    fn a_picked_i2c_device_lights_its_own_device() {
        let (mut mcu, inst, k) = grouped_bus();
        mcu.join_group_i2c(inst, k[1], "display");
        let id = mcu.modules[0].id.clone();
        mcu.selected_module = Some(id.clone());
        assert_eq!(mcu.active_device(), Some("sensors"));
        mcu.selected_i2c_child = Some((id, k[1]));
        assert_eq!(mcu.active_device(), Some("display"));
        mcu.clear_canvas_selection();
        assert_eq!(mcu.selected_i2c_child(), None);
    }

    /// An I2C bus's device edits go through one door that snapshots them for
    /// Ctrl+Z - a real change only, so a field left unchanged, or an edit of a
    /// device that is gone, does not push an entry that undoes nothing.
    #[test]
    fn an_i2c_device_edit_is_undoable_and_a_no_op_is_not() {
        use super::{I2cDeviceEdit as E, I2cDeviceKey as K};
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let inst = mcu.modules[0].instance();
        let devices = |mcu: &crate::panels::mcu_module::mcu::Mcu| match &mcu.modules[0].config {
            ModuleConfig::I2c(c) => c.devices.clone(),
            _ => unreachable!(),
        };

        assert!(mcu.edit_i2c_device(inst, E::Add));
        assert_eq!(mcu.last_module_undo_label(), Some("Add I2C device"));
        let uid = devices(&mcu)[0].uid;
        assert!(mcu.edit_i2c_device(inst, E::Name(K::Uid(uid), "oled".into())));
        let depth = mcu.module_undo.len();
        assert!(!mcu.edit_i2c_device(inst, E::Name(K::Uid(uid), "oled".into())));
        assert!(!mcu.edit_i2c_device(inst, E::Address(K::Uid(uid + 9), 0x3C)));
        assert!(!mcu.edit_i2c_device(inst + 1, E::Add), "no such bus");
        assert_eq!(mcu.module_undo.len(), depth, "no-ops push nothing");

        mcu.i2c_remove_confirm = Some((inst, K::Uid(uid)));
        assert!(mcu.edit_i2c_device(inst, E::Remove(K::Uid(uid))));
        assert_eq!(mcu.i2c_remove_confirm, None, "the confirm is spent");
        assert!(devices(&mcu).is_empty());
        mcu.undo_modules();
        assert_eq!(devices(&mcu)[0].name, "oled", "the remove is undone");
        mcu.undo_modules();
        assert_eq!(devices(&mcu)[0].name, "", "and so is the rename");
    }

    /// The config constants live in `src/pins/configs/usart1.rs` and track the
    /// module config — editing the baud rate updates the `BAUDRATE` constant.
    #[test]
    fn const_updates_with_module_config() {
        let mut mcu = create_stm32f103c8tx();
        mcu.add_module(ModuleKind::GenericInterfaceUsart);
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "usart1.rs")
            .unwrap()
            .1;
        assert!(
            body.contains("const BAUDRATE: u32 = 115200;"),
            "default:\n{body}"
        );

        if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
            cfg.baud_rate = 9600;
        }
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "usart1.rs")
            .unwrap()
            .1;
        assert!(
            body.contains("const BAUDRATE: u32 = 9600;"),
            "updated:\n{body}"
        );
        assert!(!body.contains("115200"), "old value gone");
    }

    /// The module's USART config drives the `src/pins/configs/usart1.rs` module;
    /// main.rs just calls `pins::configs::usart1::init(...)`.
    #[test]
    fn module_config_drives_stm32_usart_init() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
            cfg.baud_rate = 9600;
            cfg.parity = Parity::Even;
            cfg.stop_bits = StopBits::Two;
        }
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "usart1.rs")
            .unwrap()
            .1;
        assert!(
            body.contains("const BAUDRATE: u32 = 9600;"),
            "baud:\n{body}"
        );
        assert!(body.contains("const PARITY: char = 'E';"), "parity");
        assert!(body.contains("const STOP_BITS: u8 = 2;"), "stop bits");
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains("pins::configs::usart1::init("),
            "main.rs init call:\n{code}"
        );
    }

    /// The RX/TX data model is emitted as an inline `mod <id>` and not
    /// duplicated on re-generation.
    #[test]
    fn data_model_emitted_as_inline_mod_idempotently() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
            cfg.rx_model = "pub struct Reading { pub temp: f32 }".into();
        }
        let id = mcu.modules[0].id.clone();

        let code = mcu.fresh_main_rs();
        assert!(
            code.contains(&format!("mod {id} {{")),
            "inline mod:\n{code}"
        );
        assert!(code.contains("pub struct Reading"), "rx model body present");

        let again = mcu.update_main_rs(&code);
        assert_eq!(
            again.matches(&format!("mod {id} {{")).count(),
            1,
            "data-model mod must not be duplicated on regen"
        );
    }

    /// Modules survive the `mcu.config` serialize → parse round-trip, so a saved
    /// project restores them exactly. (They no longer live in main.rs.)
    #[test]
    fn modules_round_trip_through_mcu_config() {
        use crate::panels::mcu_module::mcu_config;
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
            cfg.baud_rate = 9600;
            cfg.rx_model = "pub struct R { pub t: f32 }".into();
        }
        assert!(
            !mcu.fresh_main_rs().contains("@modules"),
            "no marker in main.rs"
        );
        let (parsed, _) = mcu_config::parse(&mcu.mcu_config_text());
        assert_eq!(parsed, mcu.modules);
    }

    /// An empty data model emits no module block.
    #[test]
    fn empty_data_model_emits_nothing() {
        let mut mcu = create_stm32f103c8tx();
        mcu.add_module(ModuleKind::GenericInterfaceUsart);
        assert!(!mcu.fresh_main_rs().contains("// Data model for"));
    }

    /// Re-purposing a wired pin away from USART disconnects that terminal; the
    /// module stays (with its other connection).
    #[test]
    fn repurposing_pin_disconnects_module() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert_eq!(mcu.modules[0].connections.len(), 2);
        let tx_pin = mcu.modules[0].pin_for(ModuleSignal::Tx).unwrap();

        mcu.apply_pin_function(tx_pin, PinFunction::GpioOutput);

        assert_eq!(mcu.modules[0].connections.len(), 1, "TX wire dropped");
        assert!(mcu.modules[0].pin_for(ModuleSignal::Tx).is_none());
        assert!(
            mcu.modules[0].pin_for(ModuleSignal::Rx).is_some(),
            "RX still wired"
        );
        assert!(!mcu.modules.is_empty(), "module stays (disconnected)");
    }

    #[test]
    fn add_spi_module_wires_pins() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        let m = &mcu.modules[0];
        let n = m.instance();
        let sck = m.pin_for(ModuleSignal::Sck).unwrap();
        assert!(m.pin_for(ModuleSignal::Mosi).is_some());
        assert!(m.pin_for(ModuleSignal::Miso).is_some());
        assert_eq!(
            mcu.find_pin(sck).unwrap().selected_function,
            PinFunction::SpiSck(n)
        );
    }

    /// The config constant lives in the config file (once), not in main.rs.
    #[test]
    fn config_constant_not_duplicated_on_regen() {
        let mut mcu = create_stm32f103c8tx();
        mcu.add_module(ModuleKind::GenericInterfaceUsart);
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "usart1.rs")
            .unwrap()
            .1;
        assert_eq!(body.matches("const BAUDRATE").count(), 1);
        // main.rs no longer carries the peripheral constants.
        assert!(!mcu.fresh_main_rs().contains("const BAUDRATE"));
    }

    /// A module's custom label is appended to its generated handle variable(s)
    /// and survives the `@modules` round-trip (persisted in the config).
    #[test]
    fn module_label_appended_to_handle_vars() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
            cfg.custom_label = "IMU sensor".into();
        }
        let n = mcu.modules[0].instance();
        let code = mcu.fresh_main_rs();

        // The USART handle carries the sanitized label: `_serial1_imu_sensor`
        // (one `embedded-io` Read+Write value, not a split tx/rx pair).
        assert!(
            code.contains(&format!("_serial{n}_imu_sensor")),
            "serial handle labelled:\n{code}"
        );

        // The label persists through the mcu.config round-trip (serde on config).
        let (parsed, _) = crate::panels::mcu_module::mcu_config::parse(&mcu.mcu_config_text());
        assert_eq!(parsed, mcu.modules);
    }

    /// The `ApiStyle` selector switches the generated `usart1.rs` init between the
    /// portable `embedded-io` shape and the native `stm32f1xx-hal` `Serial` shape.
    #[test]
    fn api_style_switches_the_generated_init() {
        use crate::panels::mcu_module::modules::ApiStyle;
        let usart_body = |style: ApiStyle| {
            let mut mcu = create_stm32f103c8tx();
            assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
            if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
                cfg.api_style = style;
            }
            mcu.config_files()
                .into_iter()
                .find(|(n, _)| n == "usart1.rs")
                .unwrap()
                .1
        };

        // Portable (default) → embedded-io return + the bridge.
        let portable = usart_body(ApiStyle::Portable);
        assert!(
            portable.contains("pub type Handle = SerialIo<"),
            "{portable}"
        );
        assert!(portable.contains("struct SerialIo"), "{portable}");

        // Native → the split `(Tx, Rx)` handles via `.split()`, no embedded-io.
        let native = usart_body(ApiStyle::Native);
        assert!(
            native.contains("-> (serial::Tx<pac::USART1>, serial::Rx<pac::USART1>)"),
            "{native}"
        );
        assert!(
            native
                .contains("Serial::new(usart, pins, &mut afio.mapr, get_config(), clocks).split()"),
            "{native}"
        );
        assert!(
            !native.contains("embedded_io"),
            "native has no embedded-io:\n{native}"
        );
    }

    /// The `main.rs` init binding follows the API style: Portable is one value
    /// (`let mut _serialN`), Native destructures the split `(Tx, Rx)`
    /// (`let (mut _txN, mut _rxN)`), matching each config's return type.
    #[test]
    fn usart_binding_shape_follows_api_style() {
        use crate::panels::mcu_module::modules::ApiStyle;
        let main_for = |style: ApiStyle| {
            let mut mcu = create_stm32f103c8tx();
            assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
            if let ModuleConfig::Usart(cfg) = &mut mcu.modules[0].config {
                cfg.custom_label = "mw radar".into();
                cfg.api_style = style;
            }
            mcu.fresh_main_rs()
        };

        // Portable → single-value binding.
        let portable = main_for(ApiStyle::Portable);
        assert!(
            portable.contains("let mut _serial1_mw_radar = pins::configs::usart1::init("),
            "{portable}"
        );
        assert!(
            !portable.contains("let (mut _tx1"),
            "portable is NOT a tuple:\n{portable}"
        );

        // Native → destructured `(Tx, Rx)` tuple binding.
        let native = main_for(ApiStyle::Native);
        assert!(
            native.contains(
                "let (mut _tx1_mw_radar, mut _rx1_mw_radar) = pins::configs::usart1::init("
            ),
            "{native}"
        );
        assert!(
            !native.contains("let mut _serial1"),
            "native is NOT single-value:\n{native}"
        );
    }

    /// An SPI module label lands on the `_spiN` handle.
    #[test]
    fn spi_module_label_on_handle() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        if let ModuleConfig::Spi(cfg) = &mut mcu.modules[0].config {
            cfg.custom_label = "flash".into();
        }
        let n = mcu.modules[0].instance();
        let code = mcu.fresh_main_rs();
        assert!(
            code.contains(&format!("let _spi{n}_flash =")),
            "spi handle:\n{code}"
        );
    }

    /// Adding _SPI twice must advance to SPI2 — not re-pick SPI1 on its
    /// alternate (PA5/6/7) pins, which would merge into the first module.
    /// (Regression: 2nd "+_SPI" wrongly grabbed PA4/5/6/7 for SPI1.)
    #[test]
    fn second_spi_module_picks_spi2_not_spi1_again() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        assert_eq!(mcu.modules.len(), 2, "two distinct SPI modules");

        let mut instances: Vec<u8> = mcu.modules.iter().map(|m| m.instance()).collect();
        instances.sort();
        assert_eq!(instances, vec![1, 2], "instances must be SPI1 and SPI2");

        // No pin is shared between the two modules.
        let pins0: Vec<usize> = mcu.modules[0]
            .connections
            .iter()
            .map(|c| c.mcu_pin)
            .collect();
        let pins1: Vec<usize> = mcu.modules[1]
            .connections
            .iter()
            .map(|c| c.mcu_pin)
            .collect();
        assert!(pins0.iter().all(|p| !pins1.contains(p)), "no shared pins");

        // A 3rd add fails — the F103 has only SPI1/SPI2.
        assert!(!mcu.add_module(ModuleKind::GenericInterfaceSpi));
        assert_eq!(mcu.modules.len(), 2);
    }

    /// _USB auto-wires to the USB D-/D+ pins (PA11/PA12) and is single-instance.
    #[test]
    fn add_usb_module_wires_pins_and_is_single_instance() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsb));
        assert_eq!(mcu.modules.len(), 1);

        let m = &mcu.modules[0];
        assert_eq!(m.kind, ModuleKind::GenericInterfaceUsb);
        let dm = m.pin_for(ModuleSignal::UsbDm).unwrap();
        let dp = m.pin_for(ModuleSignal::UsbDp).unwrap();
        assert_ne!(dm, dp);
        assert_eq!(
            mcu.find_pin(dm).unwrap().selected_function,
            PinFunction::UsbDm
        );
        assert_eq!(
            mcu.find_pin(dp).unwrap().selected_function,
            PinFunction::UsbDp
        );

        // Single USB FS peripheral — a 2nd add is refused.
        assert!(!mcu.add_module(ModuleKind::GenericInterfaceUsb));
        assert_eq!(mcu.modules.len(), 1);
    }

    #[test]
    fn add_i2c_module_wires_pins() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        let m = &mcu.modules[0];
        let n = m.instance();
        let scl = m.pin_for(ModuleSignal::Scl).unwrap();
        let sda = m.pin_for(ModuleSignal::Sda).unwrap();
        assert_ne!(scl, sda);
        assert_eq!(
            mcu.find_pin(scl).unwrap().selected_function,
            PinFunction::I2cScl(n)
        );
        assert_eq!(
            mcu.find_pin(sda).unwrap().selected_function,
            PinFunction::I2cSda(n)
        );
    }

    #[test]
    fn spi_config_drives_codegen() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        if let ModuleConfig::Spi(cfg) = &mut mcu.modules[0].config {
            cfg.mode = 3;
            cfg.clock_hz = 4_000_000;
        }
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "spi1.rs")
            .unwrap()
            .1;
        assert!(
            body.contains("const SPI_MODE: u8 = 3;"),
            "mode const:\n{body}"
        );
        assert!(body.contains("const CLOCK_KHZ: u32 = 4000;"), "clock const");
        assert!(
            body.contains("CLOCK_KHZ.kHz()"),
            "init references the constant"
        );
        // main.rs calls the config module.
        assert!(mcu.fresh_main_rs().contains("pins::configs::spi1::init("));
    }

    #[test]
    fn i2c_config_drives_codegen() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceI2c));
        if let ModuleConfig::I2c(cfg) = &mut mcu.modules[0].config {
            cfg.clock_hz = 400_000;
        }
        let body = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "i2c1/mod.rs")
            .unwrap()
            .1;
        assert!(
            body.contains("const CLOCK_KHZ: u32 = 400;"),
            "clock const:\n{body}"
        );
        assert!(body.contains("I2cMode::Fast"), "fast branch present");
        assert!(mcu.fresh_main_rs().contains("pins::configs::i2c1::init("));
    }

    /// Assigning a peripheral signal pin (as the Peripherals tab does) auto-adds
    /// the matching virtual module; clearing the last pin removes it.
    #[test]
    fn assigning_peripheral_pin_adds_and_clearing_removes_module() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.modules.is_empty());

        let scl = mcu
            .iter_all_pins()
            .find(|p| p.available_functions.contains(&PinFunction::I2cScl(1)))
            .map(|p| p.number)
            .unwrap();

        // Assign I2C1 SCL → a _I2C module appears, wired to that pin.
        mcu.apply_pin_function(scl, PinFunction::I2cScl(1));
        assert_eq!(mcu.modules.len(), 1, "I2C module auto-added");
        assert_eq!(mcu.modules[0].kind, ModuleKind::GenericInterfaceI2c);
        assert_eq!(mcu.modules[0].instance(), 1);
        assert_eq!(mcu.modules[0].pin_for(ModuleSignal::Scl), Some(scl));

        // Clear every I2C pin → the module disappears.
        for n in mcu
            .iter_all_pins()
            .filter(|p| {
                matches!(
                    p.selected_function,
                    PinFunction::I2cScl(_) | PinFunction::I2cSda(_)
                )
            })
            .map(|p| p.number)
            .collect::<Vec<_>>()
        {
            mcu.apply_pin_function(n, PinFunction::Unset);
        }
        assert!(
            mcu.modules.is_empty(),
            "module removed once its pins are cleared"
        );
    }

    /// A chip with no USART pins can't host a _USART module.
    #[test]
    fn add_fails_without_usart_pins() {
        use crate::panels::mcu_module::mcu::model::Mcu;
        use crate::panels::mcu_module::mcu_catalog::ToolchainKind;
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        let mut mcu = Mcu::new(
            "t".into(),
            "stm32f1".into(),
            ToolchainKind::RustEmbedded,
            vec![],
            vec![],
            vec![Pin::new(1, "PA0")], // GPIO only, no USART
            vec![],
        );
        assert!(!mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert!(mcu.modules.is_empty());
    }
}
