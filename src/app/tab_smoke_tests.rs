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

/// The IoT tab with Wi-Fi and MQTT on, on both radios - every card drawn,
/// the credentials card included once `secrets.rs` exists.
#[test]
fn the_iot_tab_draws_with_everything_on() {
    use crate::panels::mcu_module::iot::MqttConfig;
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
