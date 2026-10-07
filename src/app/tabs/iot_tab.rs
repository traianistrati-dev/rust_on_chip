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
    self, Availability, BleConfig, EspNowConfig, IotConfig, Link, MqttConfig, Platform, SntpConfig,
    ThreadConfig,
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
            let files = &mut self.project_tree.user_src_files;
            let mut changed = false;

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

                    ui.add_enabled_ui(link_up, |ui| application_card(ui, &mut mcu.iot, &chip_id));
                }
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
                        ui.label(dim(if ble.family == "esp32" {
                            "Wi-Fi beside Bluetooth takes a fixed heap: 96 KiB of the bootloader's RAM + 24 KiB."
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
        if let Some(up) = iot_gen::topped_up(body, &iot_gen::secrets_body_for(wanted.wifi, wanted.thread)) {
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
                            .desired_width(if *name == iot_gen::THREAD_DATASET { 320.0 } else { 220.0 });
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
fn application_card(ui: &mut egui::Ui, cfg: &mut IotConfig, chip_id: &str) {
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
            mqtt_body(ui, m);
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
        for (name, why) in [
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
    });
}

const SNTP_USAGE: &str = "let unix_s = pins::configs::sntp::now_unix(); // None until the first answer";

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
            ui.label(dim("the clock is set again this often; between, it runs on embassy-time"));
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
