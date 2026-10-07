//! Every chip tab draws, on real chips, in a debug build.
//!
//! egui 0.36 added debug assertions (a NaN widget rect, a LayoutJob that does
//! not cover its text, a texture delta nobody applied) that panic the debug
//! build - the binary in daily use - where 0.34 drew on. None of the unit
//! tests drew the graphic tabs with a chip loaded, so the first frame of the
//! Pins, Board and Clock canvases crashed the IDE on start-up and on every
//! tab switch, while the whole suite stayed green. This draws the MCU panel the
//! way `AppIde::ui` does, tab after tab, for a few chips of different families.

use super::{AppIde, McuTab};
use eframe::egui;

const TABS: [McuTab; 9] = [
    McuTab::Pins,
    McuTab::Peripherals,
    McuTab::Configuration,
    McuTab::Clock,
    McuTab::System,
    McuTab::Iot,
    McuTab::Structure,
    McuTab::Flow,
    McuTab::Board,
];

fn draw_every_tab(chip: &str) {
    let ctx = egui::Context::default();
    let mut app = AppIde::new(
        &eframe::CreationContext::_new_kittest(ctx.clone()),
        None,
        None,
    );
    app.startup_picker = None;
    app.selected_mcu_id = chip.to_owned();
    app.mcu = AppIde::build_mcu_for(&app.mcu_registry, chip);
    assert!(app.mcu.is_some(), "{chip} is a built-in chip");
    let mut pass = 0u64;
    for tab in TABS {
        app.active_tab = tab;
        // A few frames each: the canvases are fitted to their content on the
        // frame AFTER the first, which is the one that used to go NaN.
        for _ in 0..3 {
            pass += 1;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                time: Some(pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                ..Default::default()
            };
            let _ = crate::headless::run_ui(&ctx, input, |ui| app.show_mcu_panel(ui));
        }
    }
}

#[test]
fn every_tab_draws_on_an_stm32() {
    draw_every_tab("stm32f103c8t6");
}

#[test]
fn every_tab_draws_on_an_esp32() {
    draw_every_tab("esp32c3");
}

#[test]
fn every_tab_draws_on_a_pico_board() {
    draw_every_tab("rp2350_pico2_ice");
}

/// The IoT tab with Thread on an nRF52840: the port, the dataset row with a
/// good dataset, a broken one and a Wi-Fi-era secrets.rs without the line -
/// and with Bluetooth switched on beside it, both blocked chips.
#[test]
fn the_iot_tab_draws_thread_on_an_nrf() {
    use crate::panels::mcu_module::codegen::iot_gen;
    use crate::panels::mcu_module::iot::{BleConfig, ThreadConfig};
    use crate::panels::mcu_module::mcu::model::Runtime;
    const GOOD: &str = "000300001901020fd80208b566147d38e384200e080000639c5d67a3bd0510c490f58d4be0d5eaeb0f09b395d1ae17030d4e4553542d50414e2d304644380708fd7d4f8232cb00000410a7e08419ae47c177fb91bcfcec789aa50c0402a0f77835060004001fffe0";
    let fresh = iot_gen::secrets_body_for(false, true);
    for (secrets, ble) in [
        (iot_gen::write_secret(&fresh, iot_gen::THREAD_DATASET, GOOD), false),
        (iot_gen::write_secret(&fresh, iot_gen::THREAD_DATASET, "0e08zz"), false),
        (iot_gen::secrets_body(), false),
        (fresh.clone(), true),
    ] {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(
            &eframe::CreationContext::_new_kittest(ctx.clone()),
            None,
            None,
        );
        app.startup_picker = None;
        app.selected_mcu_id = "nrf52840_dk".to_owned();
        app.mcu = AppIde::build_mcu_for(&app.mcu_registry, "nrf52840_dk");
        let mcu = app.mcu.as_mut().expect("built-in chip");
        mcu.runtime = Runtime::Async;
        mcu.iot.thread = Some(ThreadConfig::default());
        mcu.iot.ble = ble.then(BleConfig::default);
        app.project_tree
            .user_src_files
            .push((iot_gen::SECRETS_PATH.to_owned(), secrets));
        app.active_tab = McuTab::Iot;
        for pass in 0..3u64 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                time: Some(pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                ..Default::default()
            };
            let _ = crate::headless::run_ui(&ctx, input, |ui| app.show_mcu_panel(ui));
        }
    }
}

/// The IoT tab with Wi-Fi and MQTT on, on both radios - every card drawn,
/// the credentials card included once `secrets.rs` exists.
#[test]
fn the_iot_tab_draws_with_everything_on() {
    use crate::panels::mcu_module::iot::{EspNowConfig, MqttConfig, SntpConfig};
    use crate::panels::mcu_module::mcu::model::Runtime;
    for chip in ["esp32c3", "rp2040_pico_w"] {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(
            &eframe::CreationContext::_new_kittest(ctx.clone()),
            None,
            None,
        );
        app.startup_picker = None;
        app.selected_mcu_id = chip.to_owned();
        app.mcu = AppIde::build_mcu_for(&app.mcu_registry, chip);
        let mcu = app.mcu.as_mut().expect("built-in chip");
        mcu.runtime = Runtime::Async;
        mcu.iot.wifi = true;
        let mut m = MqttConfig::for_chip(chip);
        m.subscribe = vec!["a/#".into(), "bad/#/x".into()];
        mcu.iot.mqtt = Some(m);
        mcu.iot.sntp = Some(SntpConfig::default());
        // On the Pico W the switch is kept but nothing is generated for it.
        mcu.iot.esp_now = Some(EspNowConfig {
            channel: 99,
            peers: vec![[0xFF; 6], [0x24, 0x0A, 0xC4, 0x12, 0x34, 0x56]],
        });
        mcu.iot.ip.dhcp = false;
        app.project_tree.user_src_files.push((
            crate::panels::mcu_module::codegen::iot_gen::SECRETS_PATH.to_owned(),
            crate::panels::mcu_module::codegen::iot_gen::secrets_body(),
        ));
        app.active_tab = McuTab::Iot;
        for pass in 0..3u64 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                time: Some(pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                ..Default::default()
            };
            let _ = crate::headless::run_ui(&ctx, input, |ui| app.show_mcu_panel(ui));
        }
    }
}
