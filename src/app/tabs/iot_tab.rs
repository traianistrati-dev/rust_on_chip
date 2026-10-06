//! The **IoT** tab: links and protocols, in layers.
//!
//! Three cards, bottom-up - Link, Network, Application - and a card is live
//! only while the one below it is: MQTT needs TCP, TCP needs the IP stack, the
//! stack needs a link. Every link the chip cannot carry, or this IDE does not
//! generate yet, is still listed, with the reason (`iot::availability`), so
//! "can my chip do mesh?" is answered on the screen rather than by silence.
//!
//! The SSID and the passwords are edited here but never stored in the model:
//! they are lines of `src/pins/configs/secrets.rs`, which `.gitignore` lists,
//! read and rewritten in place on every edit.

use crate::app::{AppIde, McuTab};
use crate::panels::mcu_module::codegen::iot_gen;
use crate::panels::mcu_module::iot::{
    self, Availability, IotConfig, Link, MqttConfig, Platform,
};
use crate::panels::mcu_module::mcu::model::Runtime;
use eframe::egui;
use egui_phosphor::regular as ph;

/// Muted body text, matching the other tabs' explanatory lines.
fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(11.0)
        .color(egui::Color32::from_rgb(130, 130, 145))
}

fn warn(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(format!("{}  {}", ph::WARNING, text.into()))
        .size(11.0)
        .color(egui::Color32::from_rgb(235, 150, 90))
}

const MQTT_USAGE: &str = "pins::configs::mqtt::publish(\"my/topic\", b\"21.5\").await.ok();";

impl AppIde {
    /// Render the IoT tab. Called only with a chip selected.
    pub(in crate::app) fn show_iot_tab(&mut self, ui: &mut egui::Ui) {
        let mut go_system = false;
        let tree_changed;
        {
            let Some(mcu) = self.mcu.as_mut() else { return };
            let family = mcu.family.clone();
            let chip_id = mcu.id.clone();
            let cyw43 = iot::has_cyw43(mcu);
            let platform = iot::platform(&family, cyw43);
            let is_async = matches!(mcu.runtime, Runtime::Async);
            // Errata the radio walks into (ESP32 ADC2): shown here as well as
            // on the pads, since this is where the switch that causes it is.
            let radio_clashes: Vec<String> = crate::panels::mcu_module::errata::clashes(mcu)
                .into_iter()
                .filter(|c| c.text.contains("Wi-Fi"))
                .map(|c| c.text)
                .collect();
            let files = &mut self.project_tree.user_src_files;
            let mut changed = false;

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                ui.label(dim(concat!(
                    "Links and protocols, in layers: a card is live once the one below it is. ",
                    "Wi-Fi and MQTT are generated today; every other link says why not."
                )));
                ui.add_space(8.0);

                if platform.is_some() && !is_async {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(warn(concat!(
                            "Needs the Async runtime: esp-radio, cyw43 and embassy-net are ",
                            "async-only, so nothing is generated on this one."
                        )));
                        if ui.small_button("Open System tab").clicked() {
                            go_system = true;
                        }
                    });
                    ui.add_space(8.0);
                }

                link_card(ui, &mut mcu.iot, &family, cyw43, platform, is_async);
                for text in &radio_clashes {
                    ui.add_space(4.0);
                    ui.label(warn(text.as_str()));
                }
                ui.add_space(12.0);

                let link_up = mcu.iot.wifi && platform.is_some();
                if link_up {
                    changed |= secrets_card(ui, files, is_async, mcu.iot.mqtt.is_some());
                    ui.add_space(12.0);
                }

                ui.add_enabled_ui(link_up, |ui| network_card(ui, &mut mcu.iot));
                ui.add_space(12.0);

                ui.add_enabled_ui(link_up, |ui| {
                    application_card(ui, &mut mcu.iot, &chip_id, platform)
                });
            });
            tree_changed = changed;
        }
        if tree_changed {
            // `secrets.rs` was rewritten in place: Save writes it, and the
            // exit prompt now sees it as unsaved.
            self.invalidate_project_files_cache();
        }
        if go_system {
            self.active_tab = McuTab::System;
        }
    }
}

/// The links, each with its state on this chip. Only Wi-Fi has a switch: it
/// is the one link generated today.
fn link_card(
    ui: &mut egui::Ui,
    cfg: &mut IotConfig,
    family: &str,
    cyw43: bool,
    platform: Option<Platform>,
    is_async: bool,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  LINK", ph::SHARE_NETWORK)).strong());
        ui.label(dim("How the chip reaches the network. Mesh is a kind of link, not a protocol on top."));
        ui.add_space(4.0);
        for link in Link::ALL {
            let avail = iot::availability(link, family, cyw43);
            ui.horizontal(|ui| {
                if link == Link::Wifi && avail == Availability::Ready {
                    ui.checkbox(&mut cfg.wifi, "")
                        .on_hover_text("Generate the Wi-Fi station, the IP stack and the tasks that keep them up");
                } else {
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, ""));
                }
                ui.label(egui::RichText::new(link.label()).strong())
                    .on_hover_text(link.blurb());
                let (chip, color, why) = match avail {
                    Availability::Ready => ("generated", egui::Color32::from_rgb(120, 200, 140), None),
                    Availability::Planned(why) => {
                        ("planned", egui::Color32::from_rgb(120, 170, 230), Some(why))
                    }
                    Availability::NotHere(why) => {
                        ("not here", egui::Color32::from_rgb(150, 150, 160), Some(why))
                    }
                };
                ui.label(egui::RichText::new(chip).size(10.0).color(color));
                if let Some(why) = why {
                    ui.label(dim(why));
                }
            });
        }
        if cfg.wifi && platform.is_some() {
            ui.add_space(6.0);
            match platform {
                Some(Platform::Esp) => {
                    ui.horizontal(|ui| {
                        ui.label("Heap");
                        ui.add(
                            crate::panels::drag_value(ui, &mut cfg.heap_kib)
                                .range(32..=256)
                                .clamp_existing_to_range(false)
                                .suffix(" KiB"),
                        );
                        ui.label(dim(format!(
                            "esp-radio allocates its buffers from it; {} KiB is what its examples use.",
                            iot::DEFAULT_HEAP_KIB
                        )));
                    });
                    if !(32..=256).contains(&cfg.heap_kib) {
                        ui.label(warn("between 32 and 256 KiB"));
                    }
                }
                Some(Platform::Cyw43) => {
                    ui.label(dim(concat!(
                        "The radio's `control` belongs to the Wi-Fi task, so the on-board LED is ",
                        "switched with pins::configs::wifi::set_led(true)."
                    )));
                }
                None => {}
            }
            if !is_async {
                ui.label(dim("Kept, and generated once the runtime is Async."));
            }
        }
    });
}

/// The SSID and passwords, read from and written straight into `secrets.rs`.
/// Returns whether the file changed.
fn secrets_card(
    ui: &mut egui::Ui,
    files: &mut [(String, String)],
    is_async: bool,
    mqtt: bool,
) -> bool {
    let mut changed = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  CREDENTIALS", ph::KEY)).strong());
        ui.label(dim(concat!(
            "Kept in src/pins/configs/secrets.rs, which .gitignore lists - never in ",
            "mcu.config, which is committed."
        )));
        let Some((_, body)) = files.iter_mut().find(|(p, _)| p == iot_gen::SECRETS_PATH) else {
            ui.label(dim(if is_async {
                "secrets.rs is written with the next regeneration."
            } else {
                "secrets.rs is written once the runtime is Async."
            }));
            return;
        };
        ui.add_space(4.0);
        let rows: &[(&str, &str, bool)] = if mqtt {
            &[
                ("WIFI_SSID", "Network (SSID)", false),
                ("WIFI_PASSWORD", "Password", true),
                ("MQTT_USERNAME", "Broker user", false),
                ("MQTT_PASSWORD", "Broker password", true),
            ]
        } else {
            &[
                ("WIFI_SSID", "Network (SSID)", false),
                ("WIFI_PASSWORD", "Password", true),
            ]
        };
        for (name, label, secret) in rows {
            ui.horizontal(|ui| {
                ui.add_sized([120.0, 18.0], egui::Label::new(*label));
                match iot_gen::read_secret(body, name) {
                    Some(mut value) => {
                        let edit = egui::TextEdit::singleline(&mut value)
                            .password(*secret)
                            .desired_width(220.0);
                        if ui.add(edit).changed() {
                            *body = iot_gen::write_secret(body, name, &value);
                            changed = true;
                        }
                    }
                    None => {
                        ui.label(dim(format!(
                            "{name} in secrets.rs is not a plain string - edit it there"
                        )));
                    }
                }
            });
        }
        let ssid = iot_gen::read_secret(body, "WIFI_SSID").unwrap_or_default();
        if ssid.is_empty() {
            ui.label(warn("no network name yet - the station has nothing to join"));
        } else if ssid.len() > 32 {
            ui.label(warn("an SSID is at most 32 bytes"));
        }
        let pass = iot_gen::read_secret(body, "WIFI_PASSWORD").unwrap_or_default();
        if pass.is_empty() {
            ui.label(dim("No password: the network is joined as an open one."));
        } else if !(8..=63).contains(&pass.len()) {
            ui.label(warn("a WPA2 passphrase is 8 to 63 characters"));
        }
    });
    changed
}

/// The IP stack: how it gets its address.
fn network_card(ui: &mut egui::Ui, cfg: &mut IotConfig) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  NETWORK", ph::GLOBE)).strong());
        ui.label(dim("embassy-net 0.9: IPv4, TCP, UDP, DNS - src/pins/configs/net.rs."));
        ui.add_space(4.0);
        let ip = &mut cfg.ip;
        ui.horizontal(|ui| {
            ui.radio_value(&mut ip.dhcp, true, "DHCP");
            ui.radio_value(&mut ip.dhcp, false, "Static");
        });
        if !ip.dhcp {
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 18.0], egui::Label::new("Address"));
                octets(ui, &mut ip.address);
                ui.label("/");
                ui.add(
                    crate::panels::drag_value(ui, &mut ip.prefix)
                        .range(0..=32)
                        .clamp_existing_to_range(false),
                );
            });
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 18.0], egui::Label::new("Gateway"));
                octets(ui, &mut ip.gateway);
            });
            ui.horizontal(|ui| {
                ui.add_sized([70.0, 18.0], egui::Label::new("DNS"));
                octets(ui, &mut ip.dns);
            });
            if ip.prefix > 32 {
                ui.label(warn("a prefix is 0 to 32 bits"));
            }
        }
    });
}

fn octets(ui: &mut egui::Ui, b: &mut [u8; 4]) {
    for (i, o) in b.iter_mut().enumerate() {
        if i > 0 {
            ui.label(".");
        }
        ui.add(crate::panels::drag_value(ui, o).speed(0.2));
    }
}

/// What runs over the stack: MQTT today, the rest listed.
fn application_card(
    ui: &mut egui::Ui,
    cfg: &mut IotConfig,
    chip_id: &str,
    platform: Option<Platform>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  APPLICATION", ph::CLOUD)).strong());
        ui.add_space(4.0);
        let mut on = cfg.mqtt.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut on, "").changed() {
                cfg.mqtt = on.then(|| MqttConfig::for_chip(chip_id));
            }
            ui.label(egui::RichText::new("MQTT").strong());
            ui.label(dim("rust-mqtt 0.6 (MQTT 5) - src/pins/configs/mqtt.rs"));
        });
        if let Some(m) = cfg.mqtt.as_mut() {
            mqtt_body(ui, m);
        }
        ui.add_space(6.0);
        for (name, why) in [
            ("SNTP (network time)", "planned with ESP-NOW: embassy-net has the UDP socket it needs"),
            ("HTTP client", "on request: reqwless runs on the same TcpSocket"),
            ("TLS (MQTT on 8883)", "planned: embedded-tls / esp-mbedtls"),
        ] {
            ui.horizontal(|ui| {
                ui.add_enabled(false, egui::Checkbox::new(&mut false, ""));
                ui.label(egui::RichText::new(name).strong());
                ui.label(egui::RichText::new("planned").size(10.0).color(egui::Color32::from_rgb(120, 170, 230)));
                ui.label(dim(why));
            });
        }
        if platform.is_none() {
            ui.label(dim("No link on this chip, so nothing above it can run."));
        }
    });
}

fn mqtt_body(ui: &mut egui::Ui, m: &mut MqttConfig) {
    ui.indent("mqtt", |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Broker"));
            ui.add(egui::TextEdit::singleline(&mut m.host).desired_width(200.0));
            ui.label("port");
            ui.add(
                crate::panels::drag_value(ui, &mut m.port)
                    .range(1..=65535)
                    .clamp_existing_to_range(false),
            );
        });
        if let Some(p) = iot::host_problem(&m.host) {
            ui.label(warn(p));
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Client id"));
            ui.add(egui::TextEdit::singleline(&mut m.client_id).desired_width(200.0));
        });
        if m.client_id.is_empty() {
            ui.label(dim("Empty: the broker assigns one."));
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Keep-alive"));
            ui.add(
                crate::panels::drag_value(ui, &mut m.keep_alive_s)
                    .range(0..=3600)
                    .clamp_existing_to_range(false)
                    .suffix(" s"),
            );
            ui.label(dim(if m.keep_alive_s == 0 {
                "0 = none: a dead link is noticed only when a send fails"
            } else {
                "a ping goes out at half of it when nothing else did"
            }));
        });

        ui.add_space(4.0);
        ui.label("Subscribe to");
        let mut remove = None;
        for (i, topic) in m.subscribe.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(topic).desired_width(200.0));
                if ui.small_button(ph::X).on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                if let Some(p) = iot::topic_filter_problem(topic) {
                    ui.label(warn(format!("{p} - left out")));
                }
            });
        }
        if let Some(i) = remove {
            m.subscribe.remove(i);
        }
        if ui.small_button(format!("{} Add topic", ph::PLUS)).clicked() {
            m.subscribe.push("rustonchip/cmd".to_owned());
        }
        ui.label(dim("`+` matches one level, `#` the rest. What arrives comes out of incoming()."));

        ui.add_space(4.0);
        if m.port == 1883 {
            ui.label(warn("1883 is plain TCP: anyone on the network can read it. TLS is not generated yet."));
        }
        ui.label(dim("From anywhere in the firmware:"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(MQTT_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(MQTT_USAGE.to_owned());
            }
        });
    });
}
