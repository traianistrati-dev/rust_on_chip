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
use crate::panels::mcu_module::aws_iot;
use crate::panels::mcu_module::codegen::iot_gen;
use crate::panels::mcu_module::iot::{
    self, Availability, BleConfig, EspNowConfig, HttpConfig, IotConfig, Link, MqttConfig, Platform,
    SntpConfig, ThreadConfig,
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
            let project_dir = self.project_dir.clone();
            let Some(mcu) = self.mcu.as_mut() else { return };
            let family = mcu.family.clone();
            let chip_id = mcu.id.clone();
            let cyw43 = iot::has_cyw43(mcu);
            let platform = iot::platform(&family, cyw43);
            // Only what this chip's radio can carry: nothing of another
            // vendor's, nothing at all on a chip without a radio.
            let links = iot::links_for(&family, cyw43);
            // Bluetooth has its own set of radios (the ESP32-H2 has it with no
            // Wi-Fi; an nRF has only it), and on an nRF it waits for USB.
            let ble = BleFacts {
                platform: iot::ble_platform(&family, cyw43),
                usb_blocks: iot::nrf_ble_blocked_by_usb(mcu),
                family: family.clone(),
                thread: iot::thread_platform(&family),
            };
            let is_async = matches!(mcu.runtime, Runtime::Async);
            // Errata the radio walks into (ESP32 ADC2): shown here as well as
            // on the pads, since this is where the switch that causes it is.
            let radio_clashes: Vec<String> = crate::panels::mcu_module::errata::clashes(mcu)
                .into_iter()
                .filter(|c| c.text.contains("Wi-Fi"))
                .map(|c| c.text)
                .collect();
            // A phase-1 ESP wifi.rs edited past what the IDE can move: main.rs
            // creates the radio now, and its old `init` no longer fits.
            let stale_wifi = platform == Some(Platform::Esp)
                && self.project_tree.user_src_files.iter().any(|(p, c)| {
                    p == "src/pins/configs/wifi.rs"
                        && crate::panels::mcu_module::codegen::iot_gen::phase_one_wifi_left(c)
                });
            // A wifi.rs / ble.rs written for the radio of the chip before a
            // retarget, and edited since, so the IDE left it: it no longer
            // fits the main.rs this chip gets.
            let foreign: Vec<&'static str> = [
                (iot_gen::WIFI, platform),
                (iot_gen::BLE, ble.platform),
                // Thread's radio is the Bluetooth one on every Thread chip.
                (iot_gen::THREAD, ble.platform.filter(|_| ble.thread)),
                // tls.rs carries the radio's randomness and (Pico) heap.
                (iot_gen::TLS, platform),
            ]
            .into_iter()
            .filter_map(|(name, p)| {
                let p = p?;
                let path = format!("src/pins/configs/{name}");
                self.project_tree
                    .user_src_files
                    .iter()
                    .any(|(f, c)| *f == path && iot_gen::foreign_radio_file(name, c, p))
                    .then_some(name)
            })
            .collect();
            // A `.cargo/config.toml` the IDE does not manage (no markers) keeps
            // its own target: Thread on it builds hard-float, and Build then
            // compiles OpenThread's C.
            let thread_target_off = crate::panels::mcu_module::codegen::nrf::thread_on(mcu)
                .then(|| crate::panels::mcu_module::project_gen::build_target(&self.cargo_config))
                .flatten()
                .filter(|t| *t != crate::panels::mcu_module::codegen::nrf::THREAD_TARGET)
                .map(str::to_owned);
            // Restart on panic: what this build reports to, and what main.rs's
            // header holds for a panic handler.
            let panic =
                {
                    let src = |path: &str| {
                        self.project_tree
                            .user_src_files
                            .iter()
                            .find(|(p, _)| p == path)
                            .map(|(_, c)| c.as_str())
                    };
                    let active = iot::active(mcu);
                    // main.rs is the editor's buffer, not a file of the tree.
                    let main_rs = self.generated_code.as_str();
                    PanicFacts {
                        is_async,
                        links: mcu.iot.wifi
                            || mcu.iot.esp_now.is_some()
                            || mcu.iot.ble.is_some()
                            || mcu.iot.thread.is_some(),
                        active: active.is_some(),
                        sinks: active.map(|a| iot_gen::panic_sinks(mcu, a)),
                        header: (!main_rs.is_empty()).then(|| iot_gen::panic_header(main_rs)),
                        calls_restart: iot_gen::header_calls_panic(main_rs),
                        publish_moved: active.is_some_and(|a| a.mqtt)
                            && src("src/pins/configs/mqtt.rs")
                                .is_some_and(|c| !c.contains(iot_gen::MQTT_PUBLISH_SIGNATURE)),
                        public_broker: mcu.iot.mqtt.as_ref().is_some_and(|m| {
                            m.host.trim().eq_ignore_ascii_case("test.mosquitto.org")
                        }),
                    }
                };
            // MQTT over TLS: whether this chip can, what mqtt_ca.rs holds, and an
            // edited mqtt.rs / mqtt_link.rs that no longer fits the switch.
            let tls = {
                let file = |name: &str| {
                    let path = format!("src/pins/configs/{name}");
                    self.project_tree
                        .user_src_files
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, c)| c.as_str())
                };
                let coex = mcu.iot.ble.is_some() && (mcu.iot.wifi || mcu.iot.esp_now.is_some());
                let want = mcu.iot.mqtt.as_ref().is_some_and(|m| m.tls);
                TlsFacts {
                    available: iot::tls_available(&family, coex),
                    pico: platform == Some(Platform::Cyw43),
                    rp2350: platform == Some(Platform::Cyw43) && iot::tls_trng(&family),
                    entropy_left: iot_gen::entropy_link_left(
                        file(iot_gen::MQTT_LINK),
                        want && platform == Some(Platform::Cyw43) && iot::tls_trng(&family),
                    ),
                    sntp: mcu.iot.sntp.is_some(),
                    coex,
                    s2: family == "esp32s2",
                    ca: file(iot_gen::MQTT_CA).map(|c| iot_gen::ca_pem_of(c).map(iot::ca_summary)),
                    // What "Test from this PC" sends: mqtt_ca.rs's CA (the one
                    // it will be written with, until it is), and the login.
                    ca_der: iot_gen::ca_pem_of(
                        file(iot_gen::MQTT_CA)
                            .map_or(iot_gen::mqtt_ca_file(), str::to_owned)
                            .as_str(),
                    )
                    .and_then(iot::cas_der),
                    // The client certificate and its key, as secrets.rs holds them.
                    client_key_pem: file(iot_gen::SECRETS)
                        .and_then(|s| iot_gen::read_secret(s, iot_gen::CLIENT_SECRETS[1]))
                        .unwrap_or_default(),
                    client_cert_pem: file(iot_gen::SECRETS)
                        .and_then(|s| iot_gen::read_secret(s, iot_gen::CLIENT_SECRETS[0]))
                        .unwrap_or_default(),
                    client_key: file(iot_gen::SECRETS).map(|s| {
                        iot_gen::read_secret(s, iot_gen::CLIENT_SECRETS[1])
                            .map(|k| iot::client_key(&k))
                    }),
                    client_cert: file(iot_gen::SECRETS).map(|s| {
                        iot_gen::read_secret(s, iot_gen::CLIENT_SECRETS[0])
                            .map(|c| iot::client_cert(&c))
                    }),
                    client_left: iot_gen::client_link_left(
                        file(iot_gen::MQTT_LINK),
                        mcu.iot
                            .mqtt
                            .as_ref()
                            .is_some_and(|m| m.tls && m.client_cert),
                    ),
                    project: project_dir.clone(),
                    // No secrets.rs yet = it will be written with an empty login.
                    user: file(iot_gen::SECRETS).map_or(Some(String::new()), |s| {
                        iot_gen::read_secret(s, "MQTT_USERNAME")
                    }),
                    password: file(iot_gen::SECRETS).map_or(Some(String::new()), |s| {
                        iot_gen::read_secret(s, "MQTT_PASSWORD")
                    }),
                    left: platform.and_then(|p| {
                        iot_gen::mqtt_files_left(
                            file(iot_gen::MQTT),
                            file(iot_gen::MQTT_LINK),
                            want,
                            p,
                        )
                    }),
                }
            };
            // The HTTP client: whether HTTPS fits beside what else runs, and
            // what http_ca.rs holds.
            let http = {
                let ca_file = self
                    .project_tree
                    .user_src_files
                    .iter()
                    .find(|(p, _)| *p == format!("src/pins/configs/{}", iot_gen::HTTP_CA))
                    .map(|(_, c)| c.clone());
                let coex = mcu.iot.ble.is_some() && (mcu.iot.wifi || mcu.iot.esp_now.is_some());
                let mqtt_tls = iot::active(mcu).is_some_and(|a| a.mqtt_tls);
                HttpFacts {
                    https: iot::https_available(&family, coex, mqtt_tls),
                    tight: iot::https_tight(&family, mqtt_tls),
                    beside_mqtt: mqtt_tls,
                    s2: family == "esp32s2",
                    coex,
                    p384_built: iot::active(mcu).is_some_and(|a| a.p384()),
                    sntp: mcu.iot.sntp.is_some(),
                    ca: ca_file
                        .as_deref()
                        .map(|c| iot_gen::ca_pem_of(c).map(iot::ca_summary)),
                    ca_der: iot_gen::ca_pem_of(
                        ca_file.unwrap_or_else(iot_gen::http_ca_file).as_str(),
                    )
                    .and_then(iot::cas_der),
                    project: project_dir.clone(),
                }
            };
            let files = &mut self.project_tree.user_src_files;
            let mut changed = false;
            let mut tls_actions: Vec<TlsAction> = Vec::new();

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(4.0);
                if links.is_empty() {
                    let what = if iot::has_subghz_radio(&family) {
                        format!(
                            "{}'s radio is sub-GHz (LoRa, FSK), which carries none of the links \
                             this tab generates.",
                            mcu.name
                        )
                    } else {
                        format!(
                            "{} has no Wi-Fi, Bluetooth or 802.15.4 radio, so there is nothing to \
                             set up here.",
                            mcu.name
                        )
                    };
                    ui.label(dim(format!(
                        "{what} Wi-Fi, Bluetooth and the protocols over them come with an ESP32, \
                         a Pico W or Pico 2 W, or an nRF."
                    )));
                    return;
                }
                ui.label(dim(concat!(
                    "What this chip's radio can carry, in layers: a card is live once the one ",
                    "below it is. A link the IDE does not generate yet says why."
                )));
                ui.add_space(8.0);

                if (platform.is_some() || ble.platform.is_some() || ble.thread) && !is_async {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(warn(concat!(
                            "Needs the Async runtime: esp-radio, cyw43, embassy-net, nrf-sdc, ",
                            "trouble-host and openthread are async-only, so nothing is generated ",
                            "on this one."
                        )));
                        if ui.small_button("Open System tab").clicked() {
                            go_system = true;
                        }
                    });
                    ui.add_space(8.0);
                }

                link_card(ui, &mut mcu.iot, &links, &family, cyw43, platform, &ble, is_async);
                if stale_wifi {
                    ui.add_space(4.0);
                    ui.label(warn(concat!(
                        "src/pins/configs/wifi.rs still creates the radio itself (its `init` takes ",
                        "the WIFI peripheral), and you edited it too far for the IDE to move it. ",
                        "main.rs creates the radio now: make `init` take ",
                        "(spawner, controller: WifiController<'static>, station: Interface<'static>), ",
                        "or delete the file to get the current template."
                    )));
                }
                if let Some(t) = &thread_target_off {
                    ui.add_space(4.0);
                    ui.label(warn(format!(
                        ".cargo/config.toml builds for {t}, and the IDE does not manage it (no \
                         GENERATED block): Thread needs {}, the target OpenThread ships compiled \
                         for. Set it in [build] and rename [target.{t}], or Build compiles \
                         OpenThread's C (CMake, clang, libclang).",
                        crate::panels::mcu_module::codegen::nrf::THREAD_TARGET
                    )));
                }
                for name in &foreign {
                    ui.add_space(4.0);
                    ui.label(warn(format!(
                        "src/pins/configs/{name} was written for the radio of the chip before, and \
                         you edited it, so the IDE left it: it does not fit this chip's main.rs. \
                         Move your code into the current template - delete the file to get it."
                    )));
                }
                for text in &radio_clashes {
                    ui.add_space(4.0);
                    ui.label(warn(text.as_str()));
                }
                if panic.links {
                    ui.add_space(8.0);
                    restart_row(ui, &mut mcu.iot.restart_on_panic, &panic);
                }
                ui.add_space(12.0);

                let link_up = mcu.iot.wifi && platform.is_some();
                // Thread's dataset is a secret too; another link on the radio
                // wins it, and then Thread has nothing to join with.
                let thread_up = ble.thread && mcu.iot.thread.is_some() && !iot::thread_blocked(mcu);
                if link_up || thread_up {
                    let wanted = Secrets {
                        wifi: link_up,
                        mqtt: link_up && mcu.iot.mqtt.is_some(),
                        thread: thread_up,
                    };
                    changed |= secrets_card(ui, files, is_async, wanted);
                    ui.add_space(12.0);
                }

                // The IP stack and what runs on it need the station: on a chip
                // without Wi-Fi (an nRF, the ESP32-H2) they are no option.
                if platform.is_some() {
                    ui.add_enabled_ui(link_up, |ui| network_card(ui, &mut mcu.iot));
                    ui.add_space(12.0);

                    ui.add_enabled_ui(link_up, |ui| {
                        application_card(ui, &mut mcu.iot, &chip_id, &tls, &http, &mut tls_actions);
                    });
                }
            });
            // What the TLS section changed: secrets.rs (on disk at once) and
            // mqtt_ca.rs.
            for action in tls_actions {
                match action {
                    TlsAction::Secret(name, value) => {
                        let path = iot_gen::SECRETS_PATH;
                        let Some(entry) = files.iter_mut().find(|(p, _)| p == path) else {
                            continue;
                        };
                        entry.1 = iot_gen::write_secret(&entry.1, name, &value);
                        // On disk at once - and said out loud when that fails:
                        // the key would then live in memory alone.
                        let written = match &project_dir {
                            Some(dir) => {
                                let full = dir.join(path);
                                full.parent()
                                    .map_or(Ok(()), std::fs::create_dir_all)
                                    .and_then(|()| std::fs::write(&full, &entry.1))
                                    .map_err(|e| e.to_string())
                            }
                            None => Err("the project has no folder yet".to_owned()),
                        };
                        let id = egui::Id::new("iot_secrets_write_error");
                        ui.ctx().data_mut(|d| match written {
                            Ok(()) => {
                                d.remove_temp::<String>(id);
                            }
                            Err(e) => {
                                d.insert_temp(
                                    id,
                                    format!("secrets.rs could not be written ({e}) - Save the project now: the key is in memory only"),
                                );
                            }
                        });
                        changed = true;
                    }
                    TlsAction::Ca(preset) => {
                        let path = format!("src/pins/configs/{}", iot_gen::MQTT_CA);
                        let body = iot_gen::mqtt_ca_preset(preset);
                        match files.iter_mut().find(|(p, _)| *p == path) {
                            Some(entry) => entry.1 = body,
                            None => files.push((path, body)),
                        }
                        changed = true;
                    }
                    TlsAction::HttpCa(ca) => {
                        // Added to what http_ca.rs holds - written first, when
                        // it is not there yet.
                        let path = format!("src/pins/configs/{}", iot_gen::HTTP_CA);
                        let at = files.iter().position(|(p, _)| *p == path);
                        let existing = at.map_or_else(iot_gen::http_ca_file, |i| files[i].1.clone());
                        if let Some(body) = iot_gen::http_ca_with(&existing, ca) {
                            match at {
                                Some(i) => files[i].1 = body,
                                None => files.push((path, body)),
                            }
                            changed = true;
                        }
                    }
                }
            }
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

/// What "Restart on panic" needs to know about this build.
struct PanicFacts {
    /// The runtime is Async.
    is_async: bool,
    /// A link is ticked: the row is drawn.
    links: bool,
    /// The IoT code is generated (Async, a link the chip can carry).
    active: bool,
    /// Where the report goes: the serial port, the MQTT topic.
    sinks: Option<(bool, Option<String>)>,
    /// What main.rs's header holds for a panic handler.
    header: Option<iot_gen::PanicHeader>,
    /// main.rs still calls into panic.rs.
    calls_restart: bool,
    /// An edited mqtt.rs whose `publish` panic.rs's report no longer fits.
    publish_moved: bool,
    /// The broker anyone can read.
    public_broker: bool,
}

const PANIC_USAGE: &str =
    "if let Some(r) = pins::configs::panic::last() { /* r.count, r.text() */ }";

/// "Restart on panic", for every link: the switch, where the report goes,
/// and what it does not cover.
fn restart_row(ui: &mut egui::Ui, on: &mut bool, facts: &PanicFacts) {
    ui.horizontal(|ui| {
        ui.checkbox(on, "").on_hover_text(
            "A panic restarts the chip instead of halting it, and is reported at the next boot",
        );
        ui.label(egui::RichText::new("Restart on panic").strong());
        ui.label(dim("src/pins/configs/panic.rs"));
    });
    if !*on {
        if facts.calls_restart {
            ui.label(warn(concat!(
                "main.rs still calls pins::configs::panic, which is not generated now: put back ",
                "`use panic_halt as _;` (an ESP: the `loop {}` handler) in place of the handler ",
                "that calls it."
            )));
        }
        return;
    }
    ui.indent("iot_restart_on_panic", |ui| {
        ui.label(dim(concat!(
            "A panic restarts the chip. The wait before the restart doubles with each panic in ",
            "a row - 1 s, 2 s ... 1 min - and starts over after 10 min without one, so a panic on ",
            "every boot does not hammer the network. Its place and message survive the restart."
        )));
        match &facts.sinks {
            None => {
                ui.label(dim(if facts.is_async {
                    "Kept, and generated with the first link above this chip generates."
                } else {
                    "Kept, and generated once the runtime is Async."
                }));
            }
            Some((serial, topic)) => {
                if *serial {
                    ui.label(dim("Reported on the serial port, at the panic and again at the next boot."));
                }
                if let Some(topic) = topic {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(dim("And on MQTT, topic"));
                        ui.label(egui::RichText::new(topic.as_str()).monospace().size(11.0));
                        if ui.small_button(ph::COPY).on_hover_text("Copy the topic").clicked() {
                            ui.ctx().copy_text(topic.clone());
                        }
                    });
                    ui.label(dim(if *serial {
                        "QoS 0 and not retained, once the next connection is up: a panic that \
                         comes back while connecting (a TLS handshake) is never sent - the serial \
                         port or last() has it."
                    } else {
                        "QoS 0 and not retained, once the next connection is up: a panic that \
                         comes back while connecting (a TLS handshake) is never sent - only \
                         last() has it."
                    }));
                    if facts.public_broker {
                        ui.label(warn(
                            "test.mosquitto.org is public: anyone can read the report on that topic.",
                        ));
                    }
                }
                if !*serial && topic.is_none() {
                    ui.label(dim(concat!(
                        "Nothing reports it by itself on this build: read pins::configs::panic::last() ",
                        "and send it on a link of yours."
                    )));
                }
            }
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(PANIC_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(PANIC_USAGE.to_owned());
            }
        });
        ui.label(dim(concat!(
            "Panics only: a hard fault or a hang needs the watchdog (Configuration tab). Under a ",
            "debugger it restarts too - untick it to keep the chip halted where it panicked."
        )));
        if facts.active && facts.header == Some(iot_gen::PanicHeader::Other) {
            ui.label(warn(concat!(
                "main.rs has a panic handler of its own, which the IDE leaves alone: call ",
                "pins::configs::panic::restart(info) from it - or put back the IDE's ",
                "(`use panic_halt as _;`, on an ESP the `loop {}` one) for the IDE to swap."
            )));
        }
        if facts.publish_moved {
            ui.label(warn(format!(
                "src/pins/configs/mqtt.rs no longer has `{}`, which panic.rs's report calls.",
                iot_gen::MQTT_PUBLISH_SIGNATURE
            )));
        }
    });
}

/// What the link card needs to know about Bluetooth on this chip.
struct BleFacts {
    /// The radio Bluetooth is generated for, `None` where it is not.
    platform: Option<Platform>,
    /// An nRF with USB wired: Bluetooth waits (see `iot::nrf_ble_blocked_by_usb`).
    usb_blocks: bool,
    family: String,
    /// Thread is generated on this chip - on the same radio as Bluetooth (and
    /// on an ESP Wi-Fi and ESP-NOW), so one at a time (see
    /// `iot::thread_blocked`).
    thread: bool,
}

/// The links, each with its state on this chip. Wi-Fi, ESP-NOW and Bluetooth
/// have a switch where they are generated; the rest say why not.
#[allow(clippy::too_many_arguments)]
fn link_card(
    ui: &mut egui::Ui,
    cfg: &mut IotConfig,
    links: &[Link],
    family: &str,
    cyw43: bool,
    platform: Option<Platform>,
    ble: &BleFacts,
    is_async: bool,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  LINK", ph::SHARE_NETWORK)).strong());
        ui.label(dim("How the chip reaches the network. Mesh is a kind of link, not a protocol on top."));
        ui.add_space(4.0);
        for &link in links {
            let avail = iot::availability(link, family, cyw43);
            // One 2.4 GHz radio: Thread takes it alone (`iot::thread_blocked`).
            // Wi-Fi and ESP-NOW count only on an ESP with Wi-Fi.
            let wifi_taken = platform == Some(Platform::Esp) && (cfg.wifi || cfg.esp_now.is_some());
            let thread_holds = ble.thread && cfg.thread.is_some() && cfg.ble.is_none() && !wifi_taken;
            ui.horizontal(|ui| {
                if link == Link::Wifi && avail == Availability::Ready {
                    let free = cfg.wifi || !thread_holds;
                    ui.add_enabled(free, egui::Checkbox::new(&mut cfg.wifi, ""))
                        .on_hover_text("Generate the Wi-Fi station, the IP stack and the tasks that keep them up")
                        .on_disabled_hover_text("Thread has the radio: switch it off first");
                } else if link == Link::EspNow && avail == Availability::Ready {
                    let mut on = cfg.esp_now.is_some();
                    let free = on || !thread_holds;
                    if ui
                        .add_enabled(free, egui::Checkbox::new(&mut on, ""))
                        .on_hover_text("Generate ESP-NOW: send() and receive() between ESP boards, no access point")
                        .on_disabled_hover_text("Thread has the radio: switch it off first")
                        .changed()
                    {
                        toggle_kept(ui, &mut cfg.esp_now, on, "iot_stash_espnow");
                    }
                } else if link == Link::Ble && avail == Availability::Ready {
                    let mut on = cfg.ble.is_some();
                    // The radio is Thread's while it is on: switch that off first.
                    let free = on || !thread_holds;
                    if ui
                        .add_enabled(free, egui::Checkbox::new(&mut on, ""))
                        .on_hover_text("Generate a Bluetooth LE peripheral with the Nordic UART Service: send(), receive(), connected()")
                        .on_disabled_hover_text("Thread has the radio: switch it off first")
                        .changed()
                    {
                        toggle_kept(ui, &mut cfg.ble, on, "iot_stash_ble");
                    }
                } else if link == Link::Thread && avail == Availability::Ready {
                    let mut on = cfg.thread.is_some();
                    // The links already on win the radio; Thread already on can still go off.
                    let free = on || (cfg.ble.is_none() && !wifi_taken);
                    if ui
                        .add_enabled(free, egui::Checkbox::new(&mut on, ""))
                        .on_hover_text("Generate a Thread end device: OpenThread with UDP send_to() and receive() over IPv6")
                        .on_disabled_hover_text("Another link has the radio: switch it off first")
                        .changed()
                    {
                        toggle_kept(ui, &mut cfg.thread, on, "iot_stash_thread");
                    }
                } else {
                    ui.add_enabled(false, egui::Checkbox::new(&mut false, ""));
                }
                ui.label(egui::RichText::new(link.label()).strong())
                    .on_hover_text(link.blurb());
                let blocked = match link {
                    Link::Ble | Link::Wifi | Link::EspNow if thread_holds => {
                        Some(("not with Thread", "one radio: Thread has it"))
                    }
                    Link::Ble if ble.usb_blocks => Some(("not with USB", "unwire USB to generate it")),
                    Link::Thread if cfg.ble.is_some() => {
                        Some(("not with Bluetooth", "one radio: Bluetooth has it"))
                    }
                    Link::Thread if wifi_taken => {
                        Some(("not with Wi-Fi", "one radio: Wi-Fi or ESP-NOW has it"))
                    }
                    _ => None,
                };
                let (chip, color, why) = match avail {
                    Availability::Ready if blocked.is_some() => {
                        let (chip, why) = blocked.unwrap_or_default();
                        (chip, egui::Color32::from_rgb(235, 150, 90), Some(why))
                    }
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
        let esp_now_here = platform == Some(Platform::Esp) && cfg.esp_now.is_some();
        if esp_now_here {
            let station = cfg.wifi;
            if let Some(n) = cfg.esp_now.as_mut() {
                esp_now_body(ui, n, station);
            }
        }
        let ble_here = ble.platform.is_some() && cfg.ble.is_some();
        let wifi_here = (cfg.wifi && platform.is_some()) || esp_now_here;
        if ble_here {
            if let Some(b) = cfg.ble.as_mut() {
                ble_body(ui, b, ble, wifi_here);
            }
        }
        let wifi_taken = platform == Some(Platform::Esp) && (cfg.wifi || cfg.esp_now.is_some());
        let thread_here = ble.thread && cfg.thread.is_some() && cfg.ble.is_none() && !wifi_taken;
        if thread_here && let Some(th) = cfg.thread.as_mut() {
            thread_body(ui, th, ble);
        }
        if wifi_here || ble_here || thread_here {
            ui.add_space(6.0);
            // Thread on an ESP is esp-radio too: its receive queue is on the heap.
            let esp_radio = (wifi_here && platform == Some(Platform::Esp))
                || ((ble_here || thread_here) && ble.platform == Some(Platform::Esp));
            if esp_radio {
                let coex = ble_here && wifi_here;
                ui.horizontal(|ui| {
                    ui.label("Heap");
                    ui.add_enabled(
                        !coex,
                        crate::panels::drag_value(ui, &mut cfg.heap_kib)
                            .range(32..=256)
                            .clamp_existing_to_range(false)
                            .suffix(" KiB"),
                    );
                    if coex {
                        // Any TLS client checks RSA on the heap: MQTT's or HTTPS.
                        let tls = (cfg.mqtt.as_ref().is_some_and(|m| m.tls)
                            || cfg.http.as_ref().is_some_and(|h| h.tls))
                            && iot::tls_available(&ble.family, true).is_ok();
                        ui.label(dim(if ble.family == "esp32" {
                            "Wi-Fi beside Bluetooth takes a fixed heap: 96 KiB of the bootloader's RAM + 24 KiB."
                        } else if tls {
                            "Wi-Fi beside Bluetooth, with TLS, takes a fixed heap: 64 KiB of the bootloader's RAM + 96 KiB."
                        } else {
                            "Wi-Fi beside Bluetooth takes a fixed heap: 64 KiB of the bootloader's RAM + 64 KiB."
                        }));
                    } else {
                        ui.label(dim(format!(
                            "esp-radio allocates its buffers from it; {} KiB is what its examples use.",
                            iot::DEFAULT_HEAP_KIB
                        )));
                    }
                });
                if !coex && !(32..=256).contains(&cfg.heap_kib) {
                    ui.label(warn("between 32 and 256 KiB"));
                }
            }
            if wifi_here && platform == Some(Platform::Esp) {
                wifi_tx_power_row(ui, &mut cfg.wifi_tx_power);
            }
            if cfg.wifi && platform == Some(Platform::Cyw43) {
                ui.label(dim(concat!(
                    "The radio's `control` belongs to the Wi-Fi task, so the on-board LED is ",
                    "switched with pins::configs::wifi::set_led(true)."
                )));
            }
            // Bluetooth that USB blocks waits for more than the runtime.
            if !is_async && (wifi_here || thread_here || !ble.usb_blocks) {
                ui.label(dim("Kept, and generated once the runtime is Async."));
            }
        }
    });
}

/// The ESP radio's transmit power cap: the driver's steps, the default
/// marked.
fn wifi_tx_power_row(ui: &mut egui::Ui, q: &mut u8) {
    let (step, label) = iot::wifi_tx_power_step(*q);
    ui.horizontal(|ui| {
        ui.label("Transmit power");
        egui::ComboBox::from_id_salt("iot_wifi_tx_power")
            .selected_text(label)
            .show_ui(ui, |ui| {
                for (at, name) in iot::WIFI_TX_POWER_STEPS {
                    let text = if at == iot::DEFAULT_WIFI_TX_POWER {
                        format!("{name} (default)")
                    } else {
                        name.to_owned()
                    };
                    ui.selectable_value(q, at, text);
                }
            });
        ui.label(dim("at most - Wi-Fi and ESP-NOW"));
    });
    if step >= 66 {
        ui.label(warn(
            "cheap ESP32-C3 boards fail from 16.5 dBm up (one measured from 20, another from 16.5): Wi-Fi then never joins - AuthenticationExpired, then ConnectionFailed",
        ));
    } else if step > iot::DEFAULT_WIFI_TX_POWER {
        ui.label(dim(
            "More reach for a far access point or ESP-NOW peer; cheap ESP32-C3 boards fail from 16.5 dBm up.",
        ));
    } else {
        ui.label(dim(
            "esp-radio would transmit at 20 dBm, where cheap ESP32-C3 boards never join. Raise it for a far access point or ESP-NOW peer.",
        ));
    }
}

const BLE_USAGE: &str = "pins::configs::ble::send(b\"21.5\\n\").await.ok();";

/// Bluetooth's settings: the advertised name, and what the radio takes.
fn ble_body(ui: &mut egui::Ui, b: &mut BleConfig, ble: &BleFacts, wifi: bool) {
    ui.indent("ble", |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Name"));
            ui.add(egui::TextEdit::singleline(&mut b.device_name).desired_width(200.0));
        });
        if let Some(p) = iot::ble_name_problem(&b.device_name) {
            ui.label(warn(p));
        }
        ui.label(dim(concat!(
            "A GATT peripheral with the Nordic UART Service: nRF Connect, nRF Toolbox's UART or ",
            "Serial Bluetooth Terminal find the board by this name. One phone at a time; nothing ",
            "is paired or encrypted."
        )));
        match ble.platform {
            Some(Platform::Esp) if wifi => {
                ui.label(dim(
                    "Beside Wi-Fi or ESP-NOW, esp-radio's coexistence (`coex`) shares the antenna.",
                ));
            }
            Some(Platform::Cyw43) => {
                ui.label(dim(concat!(
                    "A fourth radio firmware, 43439A0_btfw.bin, is written into firmware/ with the ",
                    "project. The board advertises on a random static address made of the radio's MAC."
                )));
            }
            Some(Platform::Nrf) => {
                ui.label(dim(concat!(
                    "Nordic's SoftDevice Controller under the MPSL. They take RTC0, TIMER0, TEMP, RNG, ",
                    "PPI channels 17-31 and interrupt priority 0: every vector the generated main.rs ",
                    "binds runs at 2, and one you bind yourself must be moved off 0 too. A debugger ",
                    "halting the CPU stops the radio. nrf-sdc builds its bindings with bindgen: ",
                    "the build needs libclang (LIBCLANG_PATH)."
                )));
                if ble.usb_blocks {
                    ui.label(warn(concat!(
                        "Not generated while USB is wired: both need the CLOCK_POWER vector, and the ",
                        "two together have not been tried on a board."
                    )));
                }
            }
            _ => {}
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(BLE_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(BLE_USAGE.to_owned());
            }
        });
    });
}

const THREAD_USAGE: &str =
    "pins::configs::thread::send_to(addr, pins::configs::thread::UDP_PORT, b\"21.5\").await.ok();";

/// Thread's settings: the UDP port, and what the radio takes - per radio.
fn thread_body(ui: &mut egui::Ui, th: &mut ThreadConfig, ble: &BleFacts) {
    let esp = ble.platform == Some(Platform::Esp);
    ui.indent("thread", |ui| {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("UDP port"));
            ui.add(crate::panels::drag_value(ui, &mut th.udp_port).range(1..=65535));
        });
        ui.label(dim(format!(
            "OpenThread {} as a Minimal End Device that stays awake: it joins the network in \
             THREAD_DATASET (Credentials, below) and never forms one. One UDP socket on this \
             port: send_to() and receive() over IPv6; addresses() lists the link-local, \
             mesh-local and border-router (OMR) ones. A Thread border router (OpenThread BR, \
             Home Assistant) supplies the dataset and the route to your LAN.",
            if esp { "0.2" } else { "0.4" }
        )));
        if esp {
            ui.label(dim(concat!(
                "The radio is the chip's own 802.15.4 (esp-radio), which acknowledges frames in ",
                "hardware (OpenThread retries unacknowledged ones in software). It takes IEEE802154 ",
                "and the RNG, and its receive queue comes from the heap below; settings stay in RAM, ",
                "so the board joins anew on every boot."
            )));
            ui.label(dim(if ble.family == "esp32h2" {
                "OpenThread and Mbed TLS link prebuilt for riscv32imac: no C compiler, CMake or \
                 libclang needed. Not with Bluetooth: esp-radio 0.18 has no coexistence for 802.15.4."
            } else {
                "OpenThread and Mbed TLS link prebuilt for riscv32imac: no C compiler, CMake or \
                 libclang needed. Not with Wi-Fi, ESP-NOW or Bluetooth: esp-radio 0.18 has no \
                 coexistence for 802.15.4."
            }));
            if ble.family == "esp32c5" {
                ui.label(dim(concat!(
                    "openthread names the C6 and H2; the C5 builds and links the same way. It has ",
                    "no TRNG: its RNG is random while the radio runs, which is when it is used."
                )));
            }
        } else {
            ui.label(dim(concat!(
                "The radio is embassy-nrf's 802.15.4 driver with OpenThread's MAC in software: it ",
                "acknowledges frames late (500-650 us where the standard asks 192 us), so unicasts to ",
                "the board are retried a few times, and a sleepy device would not attach. It takes ",
                "RADIO, RNG, the 32 MHz crystal and EGU0_SWI0 (the radio's executor, priority 7); ",
                "settings stay in RAM, so the board joins anew on every boot."
            )));
            ui.label(dim(concat!(
                "The project builds for thumbv7em-none-eabi - soft-float, the FPU unused - because ",
                "openthread-sys ships OpenThread compiled for it and for no hard-float target: no C ",
                "compiler, CMake or libclang needed. Not with Bluetooth: one radio."
            )));
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(THREAD_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(THREAD_USAGE.to_owned());
            }
        });
    });
}

/// Which secrets the credentials card shows: the links that are on.
#[derive(Clone, Copy)]
struct Secrets {
    wifi: bool,
    mqtt: bool,
    thread: bool,
}

/// Switches an optional setting on or off without losing it: off keeps the
/// last values in egui's memory for this session, on brings them back (or the
/// defaults the first time). The project file keeps only what is on, as for
/// every other switch.
fn toggle_kept<T: Clone + Default + Send + Sync + 'static>(
    ui: &egui::Ui,
    slot: &mut Option<T>,
    on: bool,
    stash: &str,
) {
    let id = egui::Id::new(stash);
    if on {
        *slot = Some(ui.data_mut(|d| d.get_temp::<T>(id)).unwrap_or_default());
    } else if let Some(old) = slot.take() {
        ui.data_mut(|d| d.insert_temp(id, old));
    }
}

const ESPNOW_USAGE: &str =
    "pins::configs::espnow::send(pins::configs::espnow::BROADCAST, b\"hi\").await.ok();";

/// ESP-NOW's settings: the channel while the station is off, and the peers.
fn esp_now_body(ui: &mut egui::Ui, n: &mut EspNowConfig, station: bool) {
    ui.indent("espnow", |ui| {
        ui.add_space(4.0);
        ui.add_enabled_ui(!station, |ui| {
            ui.horizontal(|ui| {
                ui.add_sized([90.0, 18.0], egui::Label::new("Channel"));
                ui.add(
                    crate::panels::drag_value(ui, &mut n.channel)
                        .range(1..=13)
                        .clamp_existing_to_range(false),
                );
            });
        });
        ui.label(dim(if station {
            "With the station on, ESP-NOW is on the access point's channel - the radio has one."
        } else {
            "Every board that talks to this one must be on the same channel."
        }));
        if !(1..=13).contains(&n.channel) {
            ui.label(warn("a channel is 1 to 13"));
        }

        ui.add_space(4.0);
        ui.label("Peers (send by address; broadcast needs none)");
        let mut remove = None;
        for (i, mac) in n.peers.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(iot::fmt_mac(mac)).monospace());
                if ui.small_button(ph::X).on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                if let Some(p) = iot::peer_problem(mac) {
                    ui.label(warn(format!("{p} - left out")));
                }
            });
        }
        if let Some(i) = remove {
            n.peers.remove(i);
        }
        if n.peers.len() > iot::MAX_ESPNOW_PEERS {
            ui.label(warn(format!(
                "only the first {} are put on the list - it holds 20, the broadcast address among them",
                iot::MAX_ESPNOW_PEERS
            )));
        }
        let full = n.peers.len() >= iot::MAX_ESPNOW_PEERS;
        let id = ui.id().with("espnow_new_peer");
        let mut text: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("AA:BB:CC:DD:EE:FF")
                    .desired_width(150.0),
            );
            let parsed = iot::parse_mac(&text);
            let fresh = parsed.filter(|m| !n.peers.contains(m) && !full);
            if ui
                .add_enabled(fresh.is_some(), egui::Button::new(format!("{} Add peer", ph::PLUS)))
                .on_disabled_hover_text(if full {
                    "The list is full: 19 peers and the broadcast address. Broadcast reaches the rest."
                } else {
                    "Type an address that is not on the list yet"
                })
                .clicked()
            {
                if let Some(m) = fresh {
                    n.peers.push(m);
                    text.clear();
                }
            }
            if !text.is_empty() && parsed.is_none() {
                ui.label(dim("six hex bytes, `:`-separated"));
            }
        });
        ui.data_mut(|d| d.insert_temp(id, text));
        ui.label(dim(concat!(
            "A board's address: pins::configs::espnow::own_mac(). Frames are at most ",
            "250 bytes; a mesh relays what it receives - see the comment in espnow.rs."
        )));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(ESPNOW_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(ESPNOW_USAGE.to_owned());
            }
        });
    });
}

/// The SSID and passwords, read from and written straight into `secrets.rs`.
/// Returns whether the file changed.
fn secrets_card(
    ui: &mut egui::Ui,
    files: &mut [(String, String)],
    is_async: bool,
    wanted: Secrets,
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
        // A link switched on after the file was written, or a line the user
        // deleted: its empty line goes back now, as the next sync would.
        if let Some(up) =
            iot_gen::topped_up(body, &iot_gen::secrets_body_for(wanted.wifi, wanted.thread))
        {
            *body = up;
            changed = true;
        }
        ui.add_space(4.0);
        let mut rows: Vec<(&str, &str, bool)> = Vec::new();
        if wanted.wifi {
            rows.push(("WIFI_SSID", "Network (SSID)", false));
            rows.push(("WIFI_PASSWORD", "Password", true));
        }
        if wanted.mqtt {
            rows.push(("MQTT_USERNAME", "Broker user", false));
            rows.push(("MQTT_PASSWORD", "Broker password", true));
        }
        if wanted.thread {
            rows.push((iot_gen::THREAD_DATASET, "Thread dataset", true));
        }
        for (name, label, secret) in &rows {
            ui.horizontal(|ui| {
                ui.add_sized([120.0, 18.0], egui::Label::new(*label));
                match iot_gen::read_secret(body, name) {
                    Some(mut value) => {
                        let edit = egui::TextEdit::singleline(&mut value)
                            .password(*secret)
                            .desired_width(if *name == iot_gen::THREAD_DATASET {
                                320.0
                            } else {
                                220.0
                            });
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
        if wanted.wifi {
            let ssid = iot_gen::read_secret(body, "WIFI_SSID").unwrap_or_default();
            if ssid.is_empty() {
                ui.label(warn(
                    "no network name yet - the station has nothing to join",
                ));
            } else if ssid.len() > 32 {
                ui.label(warn("an SSID is at most 32 bytes"));
            }
            let pass = iot_gen::read_secret(body, "WIFI_PASSWORD").unwrap_or_default();
            if pass.is_empty() {
                ui.label(dim("No password: the network is joined as an open one."));
            } else if !(8..=63).contains(&pass.len()) {
                ui.label(warn("a WPA2 passphrase is 8 to 63 characters"));
            }
        }
        if wanted.thread
            && let Some(hex) = iot_gen::read_secret(body, iot_gen::THREAD_DATASET)
        {
            dataset_summary(ui, &hex);
        }
    });
    changed
}

/// What the pasted dataset says, so the user can see which network it is -
/// or why it would not attach.
fn dataset_summary(ui: &mut egui::Ui, hex: &str) {
    if hex.trim().is_empty() {
        ui.label(warn(concat!(
            "no dataset yet - nothing starts. On the border router: ",
            "`ot-ctl dataset active -x`, and paste the hex here."
        )));
        return;
    }
    match iot::decode_dataset(hex) {
        Ok(s) => {
            let hexs = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
            let mut parts = Vec::new();
            if let Some(n) = &s.network_name {
                parts.push(format!("network \"{n}\""));
            }
            if let Some(c) = s.channel {
                parts.push(format!("channel {c}"));
            }
            if let Some(p) = s.pan_id {
                parts.push(format!("PAN 0x{p:04x}"));
            }
            if let Some(x) = s.ext_pan_id {
                parts.push(format!("ext PAN {}", hexs(&x)));
            }
            if let Some(m) = s.mesh_local_prefix {
                parts.push(format!("mesh-local {}", iot::fmt_mesh_local_prefix(&m)));
            }
            ui.label(dim(parts.join(" · ")));
        }
        Err(why) => {
            ui.label(warn(format!("dataset: {why} - the board would not attach")));
        }
    }
}

/// The IP stack: how it gets its address.
fn network_card(ui: &mut egui::Ui, cfg: &mut IotConfig) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  NETWORK", ph::GLOBE)).strong());
        ui.label(dim(
            "embassy-net 0.9: IPv4, TCP, UDP, DNS - src/pins/configs/net.rs.",
        ));
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

/// What the MQTT card needs to know about TLS on this chip and project.
struct TlsFacts {
    /// The chip can carry it (`iot::tls_available`); `Err` says why not.
    available: Result<(), &'static str>,
    /// A Pico W / Pico 2 W: TLS brings a heap.
    pico: bool,
    /// ... the Pico 2 W, whose keys come from the RP2350's TRNG.
    rp2350: bool,
    /// An edited mqtt_link.rs on a Pico 2 W that never asks the TRNG.
    entropy_left: Option<&'static str>,
    /// SNTP is on: the certificates' dates are checked.
    sntp: bool,
    /// Bluetooth beside Wi-Fi: the ESP heap is the fixed coex one.
    coex: bool,
    /// The ESP32-S2, whose whole heap moves to the reclaimed RAM with TLS.
    s2: bool,
    /// What mqtt_ca.rs holds, once it exists: `None` inside when CA_PEM is
    /// not a plain string literal the tab can read.
    ca: Option<Option<Result<iot::CaSummary, &'static str>>>,
    /// An edited MQTT file that no longer fits the TLS switch.
    left: Option<&'static str>,
    /// The CA as DER, for "Test from this PC".
    ca_der: Option<Vec<u8>>,
    /// The broker login from secrets.rs, for the same: `None` when the line
    /// is not a plain string the tab can read.
    user: Option<String>,
    password: Option<String>,
    /// The client certificate's key and certificate as secrets.rs holds them
    /// (PEM, empty: none) - for "Test from this PC" and a CSR for the key.
    client_key_pem: String,
    client_cert_pem: String,
    /// ... read as the board reads them. Outer `None`: no secrets.rs yet;
    /// inner `None`: not a plain string the tab can read.
    client_key: Option<Option<Result<iot::ClientKey, &'static str>>>,
    client_cert: Option<Option<Result<iot::ClientCert, &'static str>>>,
    /// An edited mqtt_link.rs that cannot send the certificate.
    client_left: Option<&'static str>,
    /// The project's folder: a key made for one project never lands in another.
    project: Option<std::path::PathBuf>,
}

/// What the TLS section changes in the project's files, applied once the tab
/// is drawn.
enum TlsAction {
    /// A line of secrets.rs - written to disk at once: a key exists nowhere
    /// else, and the certificate made for it is tied to it.
    Secret(&'static str, String),
    /// mqtt_ca.rs replaced with a known CA.
    Ca(iot_gen::CaPreset),
    /// A known CA added to http_ca.rs.
    HttpCa(iot_gen::HttpCa),
}

/// What the HTTP card shows besides the settings.
struct HttpFacts {
    /// HTTPS fits on this chip, beside what else runs (`iot::https_available`).
    https: Result<(), &'static str>,
    /// ... with about 15 KB of stack left (`iot::https_tight`).
    tight: bool,
    /// MQTT runs over TLS too: HTTPS is a second session (about 40 KB).
    beside_mqtt: bool,
    /// The ESP32-S2, whose whole heap moves to the reclaimed RAM with TLS.
    s2: bool,
    /// Bluetooth beside Wi-Fi: the ESP heap is the fixed coex one.
    coex: bool,
    /// embedded-tls is built with `p384` - one switch, MQTT's or HTTP's, turns
    /// it on for both.
    p384_built: bool,
    /// SNTP is on: the certificates' dates are checked.
    sntp: bool,
    /// What http_ca.rs holds, once it exists.
    ca: Option<Option<Result<iot::CaSummary, &'static str>>>,
    /// Its CAs as DER, for "Test from this PC" (the one it will be written
    /// with, until it is).
    ca_der: Option<Vec<u8>>,
    project: Option<std::path::PathBuf>,
}

const HTTP_USAGE: &str =
    "let r = pins::configs::http::get(\"https://api.open-meteo.com/v1/forecast?latitude=44.43&longitude=26.1&current=temperature_2m\", &mut body).await;";

/// What runs over the stack: MQTT today, the rest listed.
fn application_card(
    ui: &mut egui::Ui,
    cfg: &mut IotConfig,
    chip_id: &str,
    tls: &TlsFacts,
    http: &HttpFacts,
    actions: &mut Vec<TlsAction>,
) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new(format!("{}  APPLICATION", ph::CLOUD)).strong());
        ui.add_space(4.0);
        let mut on = cfg.mqtt.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut on, "").changed() {
                let stash = egui::Id::new("iot_stash_mqtt");
                if on {
                    cfg.mqtt = Some(
                        ui.data_mut(|d| d.get_temp::<MqttConfig>(stash))
                            .unwrap_or_else(|| MqttConfig::for_chip(chip_id)),
                    );
                } else {
                    toggle_kept(ui, &mut cfg.mqtt, false, "iot_stash_mqtt");
                }
            }
            ui.label(egui::RichText::new("MQTT").strong());
            ui.label(dim("rust-mqtt 0.6 (MQTT 5) - src/pins/configs/mqtt.rs"));
        });
        if let Some(m) = cfg.mqtt.as_mut() {
            mqtt_body(ui, m, tls, actions);
        }
        ui.add_space(6.0);
        let mut sntp_on = cfg.sntp.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut sntp_on, "").changed() {
                toggle_kept(ui, &mut cfg.sntp, sntp_on, "iot_stash_sntp");
            }
            ui.label(egui::RichText::new("SNTP").strong());
            ui.label(dim("network time over UDP - src/pins/configs/sntp.rs"));
        });
        if let Some(s) = cfg.sntp.as_mut() {
            sntp_body(ui, s);
        }
        ui.add_space(6.0);
        let mut http_on = cfg.http.is_some();
        ui.horizontal(|ui| {
            if ui.checkbox(&mut http_on, "").changed() {
                toggle_kept(ui, &mut cfg.http, http_on, "iot_stash_http");
            }
            ui.label(egui::RichText::new("HTTP client").strong());
            ui.label(dim("HTTP/1.1, HTTPS through tls.rs - src/pins/configs/http.rs"));
        });
        if let Some(h) = cfg.http.as_mut() {
            http_body(ui, h, http, actions);
        }
    });
}

/// MQTT over TLS: the switch, the check, the name, the CA - and what each
/// leaves unchecked.
fn tls_section(
    ui: &mut egui::Ui,
    m: &mut MqttConfig,
    tls: &TlsFacts,
    actions: &mut Vec<TlsAction>,
) {
    ui.horizontal(|ui| {
        let mut on = m.tls;
        let can = tls.available.is_ok();
        if ui
            .add_enabled(can || on, egui::Checkbox::new(&mut on, ""))
            .on_hover_text(
                "Encrypt the connection: TLS 1.3 (embedded-tls) in src/pins/configs/mqtt_link.rs",
            )
            .changed()
        {
            m.port = m.port_for_tls(on);
            m.tls = on;
        }
        ui.label(egui::RichText::new("TLS").strong());
        ui.label(dim(
            "MQTT over TLS 1.3 - src/pins/configs/mqtt_link.rs, the CA in mqtt_ca.rs",
        ));
    });
    if let Err(why) = tls.available {
        if m.tls {
            ui.label(warn(format!(
                "TLS is not on this chip ({why}), so MQTT is not generated at all while TLS is on - never the password in clear. Switch TLS off for plain MQTT."
            )));
        } else {
            ui.label(dim(format!("TLS is not on this chip: {why}.")));
            if m.port == iot::PLAIN_PORT {
                ui.label(warn(
                    "1883 is plain TCP: anyone on the network can read it, the password included.",
                ));
            }
        }
        return;
    }
    if !m.tls {
        if m.port == iot::PLAIN_PORT {
            ui.label(warn(
                "1883 is plain TCP: anyone on the network can read it, the password included - switch TLS on.",
            ));
        }
        if let Some(left) = tls.left {
            ui.label(warn(left));
        }
        return;
    }
    ui.indent("tls", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut m.verify, true, "Check the broker's certificate");
            ui.radio_value(&mut m.verify, false, "Encrypt only - NO check");
        });
        if !m.verify {
            ui.label(
                egui::RichText::new(format!(
                    "{}  Encrypted but NOT authenticated: whoever controls the Wi-Fi, the router or DNS can pose as the broker and read the MQTT password.",
                    ph::WARNING
                ))
                .size(11.0)
                .color(egui::Color32::from_rgb(230, 90, 80)),
            );
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Server name"));
            ui.add(
                egui::TextEdit::singleline(&mut m.server_name)
                    .hint_text(m.host.trim().to_owned())
                    .desired_width(200.0),
            );
        });
        ui.label(dim(
            "The name the broker's certificate carries, sent as SNI - empty = the broker host.",
        ));
        if let Some(p) = iot::tls_problem(m) {
            ui.label(warn(p));
        }
        if let Some(p) = aws_iot::server_name_problem(m) {
            ui.label(warn(p));
        }
        // The CA is compiled in either mode, so what is wrong with it is said
        // in both.
        match &tls.ca {
            Some(Some(Ok(ca))) => {
                let key = match ca.key {
                    iot::CaKey::Rsa => "RSA",
                    iot::CaKey::EcP256 => "EC P-256",
                    iot::CaKey::EcP384 => "EC P-384",
                    iot::CaKey::EcOther => "EC (another curve)",
                    iot::CaKey::Ed25519 => "Ed25519",
                    iot::CaKey::Other => "another key",
                };
                ui.label(dim(format!(
                    "CA: {} · {key} · valid to {} - src/pins/configs/mqtt_ca.rs; paste your broker's CA there.",
                    ca.name, ca.not_after
                )));
                match ca.is_ca {
                    Some(false) => {
                        ui.label(warn("mqtt_ca.rs holds a certificate that is not a CA - paste the CA that signed the broker's"));
                    }
                    None => {
                        ui.label(dim("An X.509 v1 certificate (no extensions): used as the trust anchor as it is."));
                    }
                    Some(true) => {}
                }
                match ca.key {
                    iot::CaKey::EcP384 if !m.p384 => {
                        ui.label(warn("a P-384 CA: switch \"P-384 chains\" on below, or nothing it signed is accepted"));
                    }
                    iot::CaKey::EcOther | iot::CaKey::Other => {
                        ui.label(warn("embedded-tls cannot check this key's signatures: only RSA, EC P-256 and (with \"P-384 chains\") P-384"));
                    }
                    iot::CaKey::Ed25519 => {
                        ui.label(warn("embedded-tls checks Ed25519 only with its `ed25519` feature, which the IDE does not turn on"));
                    }
                    _ => {}
                }
                if ca.more {
                    ui.label(dim("More than one certificate in mqtt_ca.rs: each is trusted - the broker's chain may end in any of them."));
                }
            }
            Some(Some(Err(why))) => {
                ui.label(warn(format!("mqtt_ca.rs: {why} - the build fails on it")));
            }
            Some(None) => {
                ui.label(dim("mqtt_ca.rs's CA_PEM is not a plain string literal: the tab cannot read it, the build checks it."));
            }
            None => {
                ui.label(dim(
                    "mqtt_ca.rs is written with the next regeneration: ISRG Root X1 (Let's Encrypt).",
                ));
            }
        }
        ui.horizontal(|ui| {
            ui.menu_button(format!("{} Use a known CA", ph::CERTIFICATE), |ui| {
                for preset in iot_gen::CaPreset::ALL {
                    if ui.button(preset.label()).clicked() {
                        actions.push(TlsAction::Ca(preset));
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Write mqtt_ca.rs with a CA these brokers' chains end in");
        });
        if m.verify {
            ui.label(dim(if tls.sntp {
                "The certificates' dates are checked once SNTP has the time (up to 10 s waited for it)."
            } else {
                "The certificates' dates are NOT checked: there is no clock - switch SNTP on."
            }));
        }
        ui.horizontal(|ui| {
            ui.checkbox(&mut m.p384, "P-384 chains")
                .on_hover_text("embedded-tls's `p384` feature: about 114 KiB of flash more");
            ui.label(dim("an ECDSA Let's Encrypt certificate (certbot's default) needs it: its issuers are P-384. About 12 s per connection on an ESP32-C3"));
        });
        client_cert_section(ui, m, tls, actions);
        ui.label(dim(concat!(
            "TLS 1.3 only, AES-128-GCM with P-256; every certificate in the broker's chain must be signed ",
            "with RSA or P-256 (P-384 with the switch above). broker.hivemq.com (TLS 1.2 only) is out of ",
            "reach; test.mosquitto.org works on 8886. Revocation is not checked."
        )));
        ui.label(dim(if tls.pico {
            if tls.rp2350 {
                "On a Pico 2 W TLS brings a 32 KiB heap (the RSA checks), and keys from the RP2350's TRNG and ring oscillator, whitened by SHA-256."
            } else {
                "On a Pico W TLS brings a 32 KiB heap (the RSA checks), and keys from the ring oscillator, whitened by SHA-256 - the RP2040 has no TRNG."
            }
        } else if tls.coex {
            "Beside Bluetooth the heap is fixed: 64 KiB of the bootloader's RAM + 96 KiB (32 more for the RSA checks)."
        } else if tls.s2 {
            "On the S2 all of the heap moves to the RAM the bootloader used (TLS's buffers fill its DRAM), 32 KiB more for the RSA checks: keep it at 104 KiB or less."
        } else {
            "The heap grows by 32 KiB for the RSA checks, in the RAM the bootloader used."
        }));
        if let Some(left) = tls.entropy_left {
            ui.label(warn(left));
        }
        if let Some(left) = tls.left {
            ui.label(warn(left));
        }
        test_from_pc(ui, m, tls);
    });
}

/// One copyable command line.
fn command_line(ui: &mut egui::Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(text).monospace().size(10.5));
        if ui
            .small_button(ph::COPY)
            .on_hover_text("Copy the command")
            .clicked()
        {
            ui.ctx().copy_text(text.to_owned());
        }
    });
}

/// The board's own certificate: the switch, its key (made here with a CSR,
/// or loaded), the certificate the broker's CA signed - and whether the two
/// belong together.
fn client_cert_section(
    ui: &mut egui::Ui,
    m: &mut MqttConfig,
    tls: &TlsFacts,
    actions: &mut Vec<TlsAction>,
) {
    use crate::panels::mcu_module::tls_check::{self, CsrRequest, CsrShared, CsrState};
    ui.horizontal(|ui| {
        ui.checkbox(&mut m.client_cert, "Client certificate (P-256)")
            .on_hover_text("The board sends a certificate and signs the handshake with its key when the broker asks");
        ui.label(dim("the board proves who it is - AWS IoT, test.mosquitto.org:8884"));
    });
    if !m.client_cert {
        return;
    }
    ui.indent("iot_client_cert", |ui| {
        let Some(key) = &tls.client_key else {
            ui.label(dim("secrets.rs is written with the next regeneration; its key and certificate lines with it."));
            return;
        };
        // ── the key ──
        let key_point = match key {
            Some(Ok((_, point))) => {
                ui.label(dim("Key: P-256, in secrets.rs (MQTT_CLIENT_KEY) - never shown, never in mcu.config or git."));
                Some(*point)
            }
            Some(Err(why)) => {
                ui.label(if tls.client_key_pem.trim().is_empty() { dim(format!("Key: {why}.")) } else { warn(format!("MQTT_CLIENT_KEY: {why}")) });
                None
            }
            None => {
                ui.label(warn("MQTT_CLIENT_KEY in secrets.rs is not a plain string literal - the tab cannot read it"));
                None
            }
        };
        let has_key = key_point.is_some();
        // Anything in MQTT_CLIENT_KEY - readable here or not - is a key a new
        // one would replace.
        let key_present = has_key || !tls.client_key_pem.trim().is_empty() || matches!(key, None);
        // ── a key and its request ──
        let csr_id = egui::Id::new("iot_csr");
        let confirm_id = egui::Id::new("iot_csr_confirm");
        let shared: Option<CsrShared> = ui.data_mut(|d| d.get_temp::<CsrShared>(csr_id));
        // A new key comes back once: into secrets.rs (and its old certificate
        // out - it is not this key's), never kept in the tab's memory.
        if let Some(shared) = &shared
            && let Ok(mut state) = shared.lock()
            && let CsrState::Done(Ok(done)) = &mut *state
            && tls.project.as_ref() == Some(&done.project)
            && let Some(new_key) = done.key.take()
        {
            actions.push(TlsAction::Secret(iot_gen::CLIENT_SECRETS[1], new_key));
            actions.push(TlsAction::Secret(iot_gen::CLIENT_SECRETS[0], String::new()));
        }
        let busy = shared.as_ref().is_some_and(|s| s.lock().is_ok_and(|s| matches!(*s, CsrState::Building)));
        let mut subject: [String; 3] = ui.data_mut(|d| {
            d.get_temp::<[String; 3]>(egui::Id::new("iot_csr_subject"))
                .unwrap_or_else(|| [String::new(), String::new(), m.client_id.clone()])
        });
        ui.horizontal(|ui| {
            ui.label(dim("Request for"));
            ui.add(egui::TextEdit::singleline(&mut subject[0]).hint_text("C").desired_width(28.0));
            ui.add(egui::TextEdit::singleline(&mut subject[1]).hint_text("organization").desired_width(110.0));
            ui.add(egui::TextEdit::singleline(&mut subject[2]).hint_text("common name (CN)").desired_width(170.0));
        });
        ui.data_mut(|d| d.insert_temp(egui::Id::new("iot_csr_subject"), subject.clone()));
        let names = crate::panels::mcu_module::iot::csr::Subject {
            country: subject[0].trim(),
            organization: &subject[1],
            common_name: &subject[2],
        };
        let problem = names.problem();
        let ctx = ui.ctx().clone();
        let start = |key: Option<String>| {
            if let Some(project) = tls.project.clone() {
                let request = CsrRequest {
                    country: subject[0].trim().to_owned(),
                    organization: subject[1].clone(),
                    common_name: subject[2].clone(),
                    key,
                    project,
                };
                let started = tls_check::start_csr(request, ctx.clone());
                ctx.data_mut(|d| d.insert_temp(csr_id, started));
            }
        };
        // Bound to the project it was asked in.
        let confirming = tls.project.is_some()
            && ui.data_mut(|d| d.get_temp::<Option<std::path::PathBuf>>(confirm_id)) == Some(tls.project.clone());
        let load_error_id = egui::Id::new("iot_client_load_error");
        ui.horizontal(|ui| {
            let can = !busy && problem.is_none() && tls.project.is_some();
            if ui
                .add_enabled(can, egui::Button::new(format!("{} Generate key + CSR", ph::KEY)))
                .on_hover_text("A new P-256 key into secrets.rs, and a certificate request to have it signed")
                .clicked()
            {
                if key_present {
                    ui.data_mut(|d| d.insert_temp(confirm_id, tls.project.clone()));
                } else {
                    ui.data_mut(|d| d.remove_temp::<String>(load_error_id));
                    start(None);
                }
            }
            if has_key
                && ui
                    .add_enabled(can, egui::Button::new("CSR for this key"))
                    .on_hover_text("A certificate request for the key secrets.rs holds")
                    .clicked()
            {
                start(Some(tls.client_key_pem.clone()));
            }
            if ui.button("Load key...").on_hover_text("A P-256 key from a .pem / .key file").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("PEM key", &["pem", "key"]).pick_file()
            {
                match read_pem_file(&path).and_then(|text| iot::client_key(&text).map(|_| text)) {
                    Ok(text) => {
                        actions.push(TlsAction::Secret(iot_gen::CLIENT_SECRETS[1], text.trim().to_owned() + "\n"));
                        ui.data_mut(|d| d.remove_temp::<String>(load_error_id));
                    }
                    Err(why) => {
                        let error = format!("{}: {why}", path.display());
                        ui.data_mut(|d| d.insert_temp(load_error_id, error));
                    }
                }
            }
            if busy {
                ui.spinner();
                ui.label(dim("Building the helper - about a minute the first time."));
            }
        });
        if let Some(why) = problem {
            ui.label(dim(why));
        }
        if confirming {
            ui.horizontal_wrapped(|ui| {
                ui.label(warn("Replace the key? The certificate made for the old one stops working, and the old key is gone for good."));
                if ui.small_button("Replace").clicked() {
                    ui.data_mut(|d| {
                        d.remove_temp::<Option<std::path::PathBuf>>(confirm_id);
                        d.remove_temp::<String>(load_error_id);
                    });
                    start(None);
                }
                if ui.small_button("Keep").clicked() {
                    ui.data_mut(|d| d.remove_temp::<Option<std::path::PathBuf>>(confirm_id));
                }
            });
        }
        if let Some(error) = ui.data_mut(|d| d.get_temp::<String>(load_error_id)) {
            ui.label(warn(error));
        }
        if let Some(error) = ui.data_mut(|d| d.get_temp::<String>(egui::Id::new("iot_secrets_write_error"))) {
            ui.label(warn(error));
        }
        if let Some(shared) = &shared
            && let Ok(state) = shared.lock()
        {
            match &*state {
                CsrState::Done(Ok(done)) if tls.project.as_ref() == Some(&done.project) => {
                    ui.label(dim("The certificate request - have it signed by the broker's CA, then paste the certificate below:"));
                    let mut text = done.csr.clone();
                    ui.add(egui::TextEdit::multiline(&mut text).font(egui::TextStyle::Monospace).desired_rows(4).desired_width(f32::INFINITY));
                    ui.horizontal(|ui| {
                        if ui.small_button(format!("{} Copy", ph::COPY)).clicked() {
                            ui.ctx().copy_text(done.csr.clone());
                        }
                        if ui.small_button("Save as...").clicked()
                            && let Some(path) = rfd::FileDialog::new().set_file_name("device.csr").save_file()
                        {
                            match std::fs::write(&path, &done.csr) {
                                Ok(()) => {
                                    // The AWS lines run where device.csr is.
                                    if let Some(dir) = path.parent() {
                                        ui.data_mut(|d| d.insert_temp(aws_dir_id(tls), dir.to_path_buf()));
                                    }
                                }
                                Err(e) => {
                                    let error = format!("{}: {e}", path.display());
                                    ui.data_mut(|d| d.insert_temp(load_error_id, error));
                                }
                            }
                        }
                    });
                }
                CsrState::Done(Err(why)) => {
                    ui.label(warn(format!("No request: {why}")));
                }
                CsrState::Done(Ok(_)) => {}
                _ => {}
            }
        }
        // ── the certificate ──
        match &tls.client_cert {
            Some(Some(Ok((der, cert, cert_point)))) => {
                ui.label(dim(format!("Certificate: {} · valid to {} (MQTT_CLIENT_CERT).", cert.name, cert.not_after)));
                if cert.key != iot::CaKey::EcP256 {
                    ui.label(warn("its key is not P-256 - the board signs with P-256 only: make a key and a CSR here, and have that signed"));
                } else if cert.is_ca == Some(true) {
                    ui.label(warn("this is a CA's certificate, not the board's - paste the one the CA issued for the board"));
                }
                match (key_point, cert_point) {
                    (Some(Some(k)), Some(c)) if k == *c => {
                        ui.label(egui::RichText::new(format!("{}  it belongs to the key", ph::CHECK_CIRCLE)).size(11.0).color(egui::Color32::from_rgb(120, 200, 140)));
                    }
                    (Some(Some(_)), Some(_)) => {
                        ui.label(warn("it is NOT the certificate of this key - the build stops on it; paste the one issued for this key's CSR"));
                    }
                    (Some(None), _) => {
                        ui.label(dim("The key carries no public half, so the tab cannot match them: Test from this PC does."));
                    }
                    _ => {}
                }
                if der.len() > 2013 {
                    ui.label(warn("over 2013 bytes: it does not fit the board's TLS send buffer"));
                }
            }
            Some(Some(Err(why))) => {
                ui.label(if tls.client_cert_pem.trim().is_empty() { dim(format!("Certificate: {why}.")) } else { warn(format!("MQTT_CLIENT_CERT: {why}")) });
            }
            Some(None) => {
                ui.label(warn("MQTT_CLIENT_CERT in secrets.rs is not a plain string literal - the tab cannot read it"));
            }
            None => {}
        }
        let paste_id = egui::Id::new("iot_client_cert_paste");
        let mut pasting: Option<String> = ui.data_mut(|d| d.get_temp::<String>(paste_id));
        ui.horizontal(|ui| {
            if ui.button("Paste certificate...").clicked() {
                pasting = Some(String::new());
            }
            if ui.button("Load certificate...").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("PEM certificate", &["pem", "crt", "cer"]).pick_file()
            {
                match read_pem_file(&path)
                    .map(|text| iot::unescape_pem(&text))
                    .and_then(|text| iot::client_cert(&text).map(|_| text))
                {
                    Ok(text) => {
                        actions.push(TlsAction::Secret(iot_gen::CLIENT_SECRETS[0], text + "\n"));
                        ui.data_mut(|d| d.remove_temp::<String>(load_error_id));
                    }
                    Err(why) => {
                        let error = format!("{}: {why}", path.display());
                        ui.data_mut(|d| d.insert_temp(load_error_id, error));
                    }
                }
            }
        });
        let mut close = false;
        if let Some(text) = pasting.as_mut() {
            ui.add(egui::TextEdit::multiline(text).hint_text("-----BEGIN CERTIFICATE-----").font(egui::TextStyle::Monospace).desired_rows(4).desired_width(f32::INFINITY));
            let cleaned = iot::unescape_pem(text);
            let check = iot::client_cert(&cleaned);
            let typed = !text.trim().is_empty();
            ui.horizontal(|ui| {
                if ui.add_enabled(check.is_ok(), egui::Button::new("Use it")).clicked() {
                    actions.push(TlsAction::Secret(iot_gen::CLIENT_SECRETS[0], cleaned.clone() + "\n"));
                    close = true;
                } else if ui.button("Cancel").clicked() {
                    close = true;
                }
                if let Err(why) = &check
                    && typed
                {
                    ui.label(warn(*why));
                }
            });
        }
        if close {
            pasting = None;
        }
        ui.data_mut(|d| match pasting {
            Some(p) => {
                d.insert_temp(paste_id, p);
            }
            None => d.remove::<String>(paste_id),
        });
        ui.label(dim("Until both are in secrets.rs the board connects without a certificate - and a broker that wants one hangs up."));
        if let Some(left) = tls.client_left {
            ui.label(warn(left));
        }
        // ── where to have it signed ──
        if let Some(e) = aws_iot::of(m) {
            aws_section(ui, m, &e, tls, actions);
        } else if m.host.trim().eq_ignore_ascii_case("test.mosquitto.org") {
            ui.label(dim("test.mosquitto.org asks for a client certificate on port 8884: paste the CSR at https://test.mosquitto.org/ssl/ (it wants C, O and CN; its page shows an RSA key - whether it signs P-256 is untested). Its CA is mosquitto.org's."));
            if !matches!(&tls.ca, Some(Some(Ok(ca))) if ca.name.contains("mosquitto")) {
                ui.horizontal_wrapped(|ui| {
                    ui.label(dim("8883 and 8884 end in the mosquitto.org CA, not Let's Encrypt"));
                    if ui.small_button("Use mosquitto.org").clicked() {
                        actions.push(TlsAction::Ca(iot_gen::CaPreset::Mosquitto));
                    }
                });
            }
        }
        ui.label(dim(if tls.pico {
            "Signing the handshake takes about half a second on an RP2040 (one P-256 multiplication), tens of ms elsewhere."
        } else {
            "Signing the handshake takes tens of milliseconds (one P-256 multiplication)."
        }));
    });
}

/// A PEM file's text, whatever the tool that saved it wrote: PowerShell 5.1's
/// `>` makes UTF-16.
fn read_pem_file(path: &std::path::Path) -> Result<String, &'static str> {
    let bytes = std::fs::read(path).map_err(|_| "the file could not be read")?;
    iot::decode_text(&bytes).ok_or("neither UTF-8 nor UTF-16 text")
}

/// Where the user saved device.csr, per project: the AWS lines run there.
fn aws_dir_id(tls: &TlsFacts) -> egui::Id {
    egui::Id::new(("iot_aws_dir", tls.project.clone()))
}

/// One numbered AWS step: what it does, the line, and the line that checks it.
fn aws_step(ui: &mut egui::Ui, n: &mut usize, step: &aws_iot::Step) {
    *n += 1;
    ui.label(dim(format!("{n}. {}", step.what)));
    command_line(ui, &step.command);
    if let Some((what, line)) = &step.also {
        ui.label(dim(format!("    {what}")));
        command_line(ui, line);
    }
}

/// AWS IoT, step by step: the CLI lines with the region, the thing (the
/// client id) and the certificate's ARN filled in, and the policy that fits
/// this board - then how to see what AWS made of it.
fn aws_section(
    ui: &mut egui::Ui,
    m: &MqttConfig,
    e: &aws_iot::Endpoint,
    tls: &TlsFacts,
    actions: &mut Vec<TlsAction>,
) {
    ui.add_space(4.0);
    ui.label(egui::RichText::new("AWS IoT").strong());
    ui.label(dim(
        "One board = one thing: the client id is its name, the certificate its own. AWS CLI 2.11.17 or newer, run where device.csr is (Save as... above):",
    ));
    let dir: Option<std::path::PathBuf> = ui.data_mut(|d| d.get_temp(aws_dir_id(tls)));
    if let Some(dir) = &dir {
        // `pushd` changes the drive in cmd too; PowerShell and bash have it.
        command_line(ui, &format!("pushd \"{}\"", dir.display()));
    }
    let mut n = 0;
    for step in aws_iot::steps(m, e) {
        aws_step(ui, &mut n, &step);
    }
    // The certificate's ARN: the policy and the attach lines name it.
    let arn_id = egui::Id::new(("iot_aws_arn", tls.project.clone()));
    let mut arn_text: String = ui.data_mut(|d| d.get_temp(arn_id)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.label(dim(format!("{}. The ARN step {n} printed:", n + 1)));
        ui.add(
            egui::TextEdit::singleline(&mut arn_text)
                .hint_text("arn:aws:iot:<region>:<account>:cert/<id>")
                .desired_width(300.0),
        );
    });
    n += 1;
    ui.data_mut(|d| d.insert_temp(arn_id, arn_text.clone()));
    let id = aws_iot::thing(m);
    match aws_iot::cert_arn(&arn_text) {
        Err(why) if !arn_text.trim().is_empty() => {
            ui.label(warn(why));
        }
        Err(_) => {
            ui.label(dim("Then policy.json and the lines that attach it follow, filled in."));
        }
        Ok(arn) => {
            if arn.region != e.region {
                ui.label(warn(format!(
                    "this certificate is in {}, the endpoint in {} - a certificate works only in its own region",
                    arn.region, e.region
                )));
            }
            let policy = aws_iot::policy(m, &arn);
            let error_id = egui::Id::new("iot_aws_policy_error");
            ui.horizontal(|ui| {
                let mut dialog = rfd::FileDialog::new().set_file_name("policy.json");
                if let Some(dir) = &dir {
                    dialog = dialog.set_directory(dir);
                }
                if ui
                    .button("Save policy.json...")
                    .on_hover_text("UTF-8, where the commands run - a shell's `>` could make it UTF-16")
                    .clicked()
                    && let Some(path) = dialog.save_file()
                {
                    match std::fs::write(&path, &policy) {
                        Ok(()) => {
                            ui.data_mut(|d| d.remove_temp::<String>(error_id));
                        }
                        Err(e) => {
                            let error = format!("{}: {e}", path.display());
                            ui.data_mut(|d| d.insert_temp(error_id, error));
                        }
                    }
                }
                if ui.small_button(format!("{} Copy", ph::COPY)).on_hover_text("Copy the policy").clicked() {
                    ui.ctx().copy_text(policy.clone());
                }
            });
            if let Some(error) = ui.data_mut(|d| d.get_temp::<String>(error_id)) {
                ui.label(warn(error));
            }
            let outside: Vec<String> = iot::subscriptions(m)
                .into_iter()
                .filter(|f| !f.starts_with(&format!("{id}/")))
                .map(|f| format!("`{f}`"))
                .collect();
            ui.label(dim(format!(
                "policy.json: connect as `{id}`; publish and receive under `{id}/` (the panic report and the test go there); subscribe there{}. Publish your own topics under `{id}/` - or add them to it.",
                if outside.is_empty() {
                    String::new()
                } else {
                    format!(" and to {}", outside.join(", "))
                }
            )));
            if aws_iot::policy_too_long(&policy) {
                ui.label(warn("over AWS's 2048 characters for a policy: fewer subscriptions outside the client id's own topics"));
            }
            for step in aws_iot::attach_steps(m, &arn) {
                aws_step(ui, &mut n, &step);
            }
        }
    }
    ui.label(dim(format!(
        "Then wait up to 8 minutes - AWS caches policies - and Test from this PC: it connects, subscribes and publishes as the board would. Still silent on the board? In the AWS console's MQTT test client subscribe to `$aws/events/presence/+/{id}` (connects, disconnects, and why) and `{id}/#` (what it publishes); on the board, mqtt::last_refusal() says what AWS refused."
    )));
    if !matches!(&tls.ca, Some(Some(Ok(ca))) if ca.name.contains("Amazon")) {
        ui.horizontal_wrapped(|ui| {
            ui.label(warn("mqtt_ca.rs does not hold Amazon's CAs"));
            if ui.small_button("Use Amazon Root CA 1 + 3").clicked() {
                actions.push(TlsAction::Ca(iot_gen::CaPreset::Aws));
            }
        });
    }
}

/// "Test from this PC": the board's own certificate checks, built into a small
/// program and run here against the broker - then the CONNECT, the
/// subscriptions and a test publication (`tls_check`).
fn test_from_pc(ui: &mut egui::Ui, m: &MqttConfig, tls: &TlsFacts) {
    use crate::panels::mcu_module::tls_check::{self, Request, Shared, State};
    let id = egui::Id::new("iot_tls_check");
    let shared: Option<Shared> = ui.data_mut(|d| d.get_temp::<Shared>(id));
    let state = shared
        .as_ref()
        .and_then(|s| s.lock().ok().map(|s| s.clone()))
        .unwrap_or_default();
    let busy = matches!(state, State::Building | State::Connecting);
    let request = tls.ca_der.clone().map(|ca| Request {
        host: m.host.trim().to_owned(),
        port: m.port,
        server_name: m.tls_name().to_owned(),
        verify: m.verify,
        ca,
        client_id: m.client_id.clone(),
        // Encrypt only sends no login from here: a login the tab cannot read
        // still counts as one the board sends.
        user: tls.user.clone().unwrap_or_else(|| if m.verify { String::new() } else { "?".to_owned() }),
        password: tls.password.clone().unwrap_or_else(|| if m.verify { String::new() } else { "?".to_owned() }),
        alpn: iot::aws_alpn(m),
        client_cert: if m.client_cert {
            tls.client_cert_pem.clone()
        } else {
            String::new()
        },
        client_key: if m.client_cert {
            tls.client_key_pem.clone()
        } else {
            String::new()
        },
        keep_alive: m.keep_alive_s,
        subscribe: iot::subscriptions(m).into_iter().map(str::to_owned).collect(),
        // On AWS IoT only, where the policy the tab writes allows it: another
        // broker's ACL may refuse a topic the board never publishes on.
        test_topic: if iot::aws_iot(m) {
            iot::client_topic(m, "test")
        } else {
            String::new()
        },
    });
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let can = !busy && request.is_some() && iot::host_problem(&m.host).is_none();
        if ui
            .add_enabled(can, egui::Button::new(format!("{} Test from this PC", ph::PLUGS_CONNECTED)))
            .on_hover_text(format!(
                "Connect to the broker from this PC with the board's own certificate checks, then send the board's MQTT CONNECT and its subscriptions{}{}",
                if iot::aws_iot(m) {
                    format!(", and one test message on {}", iot::client_topic(m, "test"))
                } else {
                    String::new()
                },
                if m.client_id.is_empty() {
                    ""
                } else {
                    " - as the board's client id, so a board online with it is dropped once"
                }
            ))
            .clicked()
            && let Some(request) = request.clone()
        {
            let started = tls_check::start(request, ui.ctx().clone());
            ui.data_mut(|d| d.insert_temp(id, started));
        }
        match &state {
            State::Building => {
                ui.spinner();
                ui.label(dim("Building the checker - about a minute the first time, seconds after."));
            }
            State::Connecting => {
                ui.spinner();
                ui.label(dim("Connecting to the broker ..."));
            }
            _ => {}
        }
    });
    if request.is_none() {
        ui.label(dim("mqtt_ca.rs holds no certificate the checker can use."));
    }
    let State::Done {
        ok,
        summary,
        steps,
        p384,
        tested,
    } = &state
    else {
        return;
    };
    if m.verify && (tls.user.is_none() || tls.password.is_none()) {
        ui.label(dim(
            "MQTT_USERNAME or MQTT_PASSWORD in secrets.rs is not a plain string: the test sends it empty.",
        ));
    }
    let now = request.as_ref().map(|r| r.tested().fingerprint);
    if now != Some(tested.fingerprint) {
        ui.label(dim(format!(
            "For {} - the settings have changed since.",
            tested.key
        )));
    }
    for step in steps {
        ui.label(dim(format!("{}  {step}", ph::CHECK)));
    }
    if *ok {
        ui.label(
            egui::RichText::new(format!("{}  {summary}", ph::CHECK_CIRCLE))
                .size(11.0)
                .color(egui::Color32::from_rgb(120, 200, 140)),
        );
    } else {
        ui.label(warn(summary.as_str()));
    }
    if *p384 && !m.p384 {
        ui.label(warn(
            "the broker's chain is signed with P-384: this PC checks it, the board only with \"P-384 chains\" on",
        ));
    }
    if *ok && tested.verify && !tls.sntp {
        ui.label(dim(
            "The dates were checked by this PC's clock; the board checks them only with SNTP on.",
        ));
    }
}

/// The HTTP client's settings: HTTPS and its checks, the limits, the
/// User-Agent - and a URL tested from this PC.
fn http_body(ui: &mut egui::Ui, h: &mut HttpConfig, f: &HttpFacts, actions: &mut Vec<TlsAction>) {
    ui.indent("http", |ui| {
        ui.horizontal(|ui| {
            ui.checkbox(&mut h.tls, "HTTPS")
                .on_hover_text("https:// URLs: TLS 1.3 through tls.rs. Off, they are refused - never sent in clear");
            ui.label(dim("TLS 1.3, the CAs in http_ca.rs"));
        });
        if h.tls {
            if let Err(why) = f.https {
                ui.label(warn(format!(
                    "Not here: {why}. https:// URLs are refused (HttpError::NoTls); http:// ones still go."
                )));
            } else {
                https_settings(ui, h, f, actions);
            }
        } else {
            ui.label(dim("https:// URLs are refused (HttpError::NoTls); http:// ones go in clear."));
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Answer up to"));
            ui.add(
                crate::panels::drag_value(ui, &mut h.body_max)
                    .range(iot::HTTP_BODY_RANGE)
                    .clamp_existing_to_range(false)
                    .suffix(" B"),
            );
            ui.label(dim("more is dropped, and `truncated` set"));
        });
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Send up to"));
            ui.add(
                crate::panels::drag_value(ui, &mut h.payload_max)
                    .range(iot::HTTP_BODY_RANGE)
                    .clamp_existing_to_range(false)
                    .suffix(" B"),
            );
            ui.label(dim("a request's body"));
        });
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Timeout"));
            ui.add(
                crate::panels::drag_value(ui, &mut h.timeout_s)
                    .range(1..=300)
                    .clamp_existing_to_range(false)
                    .suffix(" s"),
            );
            ui.label(dim("a whole request: the lookup, the handshake, the answer"));
        });
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("User-Agent"));
            ui.add(egui::TextEdit::singleline(&mut h.user_agent).desired_width(200.0));
        });
        let sent = iot::http_user_agent(&h.user_agent);
        if sent != h.user_agent {
            ui.label(dim(format!("sent as \"{sent}\"")));
        }
        ui.label(dim(
            "One request at a time, from one task; each its own connection. A redirect is reported (location()), never followed. A .local name does not resolve: give the address.",
        ));
        ui.label(dim("From anywhere in the firmware (body: a [u8] buffer):"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(HTTP_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(HTTP_USAGE.to_owned());
            }
        });
        http_test_from_pc(ui, h, f);
    });
}

/// HTTPS's checks and CAs, where the chip carries it.
fn https_settings(ui: &mut egui::Ui, h: &mut HttpConfig, f: &HttpFacts, actions: &mut Vec<TlsAction>) {
    ui.indent("https", |ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut h.verify, true, "Check the server's certificate");
            ui.radio_value(&mut h.verify, false, "Encrypt only - NO check");
        });
        if !h.verify {
            ui.label(warn(
                "Encrypted but NOT authenticated: whoever controls the network can pose as the server, and read any token the board sends.",
            ));
        }
        match &f.ca {
            Some(Some(Ok(ca))) => {
                ui.label(dim(format!(
                    "CA: {}{} - src/pins/configs/http_ca.rs; every certificate in it is trusted.",
                    ca.name,
                    if ca.more { " and more" } else { "" }
                )));
            }
            Some(Some(Err(why))) => {
                ui.label(warn(format!("http_ca.rs: {why} - the build fails on it")));
            }
            Some(None) => {
                ui.label(dim("http_ca.rs's CA_PEM is not a plain string literal: the tab cannot read it, the build checks it."));
            }
            None => {
                ui.label(dim("http_ca.rs is written with the next regeneration: ISRG Root X1 (Let's Encrypt)."));
            }
        }
        ui.horizontal(|ui| {
            ui.menu_button(format!("{} Add a known CA", ph::CERTIFICATE), |ui| {
                for ca in iot_gen::HttpCa::ALL {
                    if ui.button(ca.label()).clicked() {
                        actions.push(TlsAction::HttpCa(ca));
                        // What a P-384 root signs checks only with P-384 on.
                        if ca.p384() {
                            h.p384 = true;
                        }
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Add to http_ca.rs a CA these services' chains end in");
            ui.checkbox(&mut h.p384, "P-384 chains")
                .on_hover_text("embedded-tls's `p384` feature: about 114 KiB of flash more");
        });
        ui.label(dim("P-384 chains: Discord and Google (GTS Root R4), ECDSA Let's Encrypt certificates - most Let's Encrypt sites now. Checking one takes about 12 s on an ESP32-C3 (an RSA chain: about a second), and every request opens its own connection. TLS 1.2-only servers (httpbin.org) cannot be reached."));
        ui.label(dim(if f.sntp {
            "The certificates' dates are checked once SNTP has the time."
        } else {
            "The certificates' dates are NOT checked: there is no clock - switch SNTP on."
        }));
        if f.beside_mqtt {
            ui.label(dim("Beside MQTT over TLS: a second TLS session, about 40 KB of RAM; one handshake at a time."));
        } else if f.s2 {
            ui.label(dim("On the S2 all of the heap moves to the RAM the bootloader used (TLS's buffers fill its DRAM), 32 KiB more for the RSA checks: keep it at 104 KiB or less."));
        } else if f.coex {
            ui.label(dim("Beside Bluetooth the heap is fixed: 64 KiB of the bootloader's RAM + 96 KiB (32 more for the RSA checks)."));
        } else {
            ui.label(dim("The ESP heap grows by 32 KiB for the RSA checks (a Pico gets a 32 KiB heap)."));
        }
        if f.tight {
            ui.label(warn(
                "Beside MQTT over TLS this chip keeps about 15 KB of stack: it links, but was never run on a board.",
            ));
        }
    });
}

/// "Test from this PC" for a URL: the board's checks and buffers, here.
fn http_test_from_pc(ui: &mut egui::Ui, h: &HttpConfig, f: &HttpFacts) {
    use crate::panels::mcu_module::tls_check::{self, HttpRequest, Shared, State};
    let url_id = egui::Id::new(("iot_http_test_url", f.project.clone()));
    let id = egui::Id::new(("iot_http_check", f.project.clone()));
    let mut url: String = ui.data_mut(|d| d.get_temp(url_id)).unwrap_or_else(|| {
        "https://api.open-meteo.com/v1/forecast?latitude=44.43&longitude=26.10&current=temperature_2m".to_owned()
    });
    let shared: Option<Shared> = ui.data_mut(|d| d.get_temp::<Shared>(id));
    let state = shared
        .as_ref()
        .and_then(|s| s.lock().ok().map(|s| s.clone()))
        .unwrap_or_default();
    let busy = matches!(state, State::Building | State::Connecting);
    ui.add_space(4.0);
    ui.add(egui::TextEdit::singleline(&mut url).hint_text("https://...").desired_width(320.0));
    let https_url = url
        .trim()
        .get(..8)
        .is_some_and(|s| s.eq_ignore_ascii_case("https://"));
    // What the board would do with this URL before any network: no HTTPS,
    // or no CA it could check against.
    let refused = if https_url && !(h.tls && f.https.is_ok()) {
        Some("HTTPS is off for this board: get() of an https:// URL returns HttpError::NoTls, and nothing is sent")
    } else if https_url && h.verify && f.ca_der.is_none() {
        Some("http_ca.rs holds no certificate the checker can read")
    } else {
        None
    };
    // The board's own limits, as http.rs gets them.
    let request = HttpRequest {
        url: url.trim().to_owned(),
        verify: h.verify,
        ca: f.ca_der.clone().unwrap_or_default(),
        // One switch builds P-384 in for both clients.
        p384: h.p384 || f.p384_built,
        head_max: iot::HTTP_HEAD_MAX,
        body_max: h.body_max.clamp(*iot::HTTP_BODY_RANGE.start(), *iot::HTTP_BODY_RANGE.end()) as usize,
        user_agent: iot::http_user_agent(&h.user_agent),
        request_head_max: iot::HTTP_REQUEST_HEAD_MAX,
    };
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                !busy && !url.trim().is_empty() && refused.is_none(),
                egui::Button::new(format!("{} Test from this PC", ph::PLUGS_CONNECTED)),
            )
            .on_hover_text("GET this URL from this PC with the board's own certificate checks and buffers")
            .clicked()
        {
            let started = tls_check::start_http(request.clone(), ui.ctx().clone());
            ui.data_mut(|d| d.insert_temp(id, started));
        }
        match &state {
            State::Building => {
                ui.spinner();
                ui.label(dim("Building the checker - about a minute the first time."));
            }
            State::Connecting => {
                ui.spinner();
            }
            _ => {}
        }
    });
    ui.data_mut(|d| d.insert_temp(url_id, url.clone()));
    if let Some(why) = refused {
        ui.label(warn(why));
    }
    let State::Done {
        ok,
        summary,
        steps,
        p384,
        tested,
    } = &state
    else {
        return;
    };
    if request.tested().fingerprint != tested.fingerprint {
        ui.label(dim(format!("For {} - the settings have changed since.", tested.key)));
    }
    for step in steps {
        ui.label(dim(format!("{}  {step}", ph::CHECK)));
    }
    if *ok {
        ui.label(
            egui::RichText::new(format!("{}  {summary}", ph::CHECK_CIRCLE))
                .size(11.0)
                .color(egui::Color32::from_rgb(120, 200, 140)),
        );
    } else {
        ui.label(warn(summary.as_str()));
    }
    if *p384 && !h.p384 && !f.p384_built {
        ui.label(warn("the server's chain is signed with P-384: this PC checks it, the board only with \"P-384 chains\" on"));
    }
    if *ok && tested.verify && !f.sntp {
        ui.label(dim("The dates were checked by this PC's clock; the board checks them only with SNTP on."));
    }
}

const SNTP_USAGE: &str =
    "let unix_s = pins::configs::sntp::now_unix(); // None until the first answer";

fn sntp_body(ui: &mut egui::Ui, s: &mut SntpConfig) {
    ui.indent("sntp", |ui| {
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Server"));
            ui.add(egui::TextEdit::singleline(&mut s.server).desired_width(200.0));
        });
        if let Some(p) = iot::ntp_server_problem(&s.server) {
            ui.label(warn(p));
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Every"));
            ui.add(
                crate::panels::drag_value(ui, &mut s.interval_s)
                    .range(iot::MIN_SNTP_INTERVAL_S..=86_400)
                    .clamp_existing_to_range(false)
                    .suffix(" s"),
            );
            ui.label(dim(
                "the clock is set again this often; between, it runs on embassy-time",
            ));
        });
        if !(iot::MIN_SNTP_INTERVAL_S..=86_400).contains(&s.interval_s) {
            ui.label(warn(format!(
                "between {} s and a day - public pools ask not to be polled faster",
                iot::MIN_SNTP_INTERVAL_S
            )));
        }
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(SNTP_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(SNTP_USAGE.to_owned());
            }
        });
    });
}

fn mqtt_body(ui: &mut egui::Ui, m: &mut MqttConfig, tls: &TlsFacts, actions: &mut Vec<TlsAction>) {
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
        let aws = aws_iot::of(m);
        if let Some(e) = &aws {
            if let Some(p) = aws_iot::endpoint_problem(e) {
                ui.label(warn(p));
            }
            if let Some(p) = aws_iot::port_problem(m.port) {
                ui.label(warn(p));
            }
        }
        ui.horizontal(|ui| {
            ui.add_sized([90.0, 18.0], egui::Label::new("Client id"));
            ui.add(egui::TextEdit::singleline(&mut m.client_id).desired_width(200.0));
        });
        if aws.is_some() {
            if let Some(p) = aws_iot::client_id_problem(&m.client_id) {
                ui.label(warn(p));
            }
        } else if m.client_id.is_empty() {
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
                "0 = none: TCP's own probes find a dead link within a few minutes"
            } else {
                "a ping goes out at half of it when nothing else did"
            }));
        });
        if aws.is_some()
            && let Some(note) = aws_iot::keep_alive_note(m.keep_alive_s)
        {
            ui.label(dim(note));
        }

        ui.add_space(4.0);
        ui.label("Subscribe to");
        let mut remove = None;
        for (i, topic) in m.subscribe.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(topic).desired_width(200.0));
                if ui.small_button(ph::X).on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                // `iot::filter_left_out`, with the AWS half read off above.
                let problem = iot::topic_filter_problem(topic)
                    .or_else(|| aws.as_ref().and_then(|_| aws_iot::filter_problem(topic)));
                if let Some(p) = problem {
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
        ui.label(dim(if aws.is_some() {
            "`+` matches one level, `#` the rest - on AWS IoT `a/#` does not match `a`, and a wildcard filter gets no retained message. What arrives comes out of incoming()."
        } else {
            "`+` matches one level, `#` the rest. What arrives comes out of incoming()."
        }));

        ui.add_space(4.0);
        tls_section(ui, m, tls, actions);
        ui.add_space(4.0);
        ui.label(dim("From anywhere in the firmware:"));
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(MQTT_USAGE).monospace().size(11.0));
            if ui.button(ph::COPY).on_hover_text("Copy the line").clicked() {
                ui.ctx().copy_text(MQTT_USAGE.to_owned());
            }
        });
        ui.label(dim(
            "publish waits while its queue is full - as long as the broker is away; try_publish never waits, and connected() says whether the broker is there.",
        ));
    });
}
