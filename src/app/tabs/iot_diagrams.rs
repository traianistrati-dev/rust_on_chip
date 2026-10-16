//! The IoT tab's diagrams: what the board runs, and how each part of it works.
//!
//! Drawn from the decision the generator itself makes ([`iot::active`]), with
//! the numbers read off the templates the firmware is generated from
//! ([`iot_gen::template_numbers`]) - so a diagram cannot show what the code
//! does not do, and a template change moves the diagram with it.
//!
//! Two halves: pure functions that say WHAT to draw ([`overview_bands`],
//! [`mqtt_chain`], [`http_chain`], the queue paths), tested without egui, and
//! the painters under them. Hand-painted like the Serial tab's Info view, but
//! laid out to the width there is instead of scaled down, so the text keeps
//! its size and is cut with "…" where it must; no `Scene`, which would take
//! the mouse wheel from the tab's scroll area. Arrows are painted: the bundled
//! font has no arrow glyphs (`glyph_guard`).

use std::sync::Arc;

use crate::panels::mcu_module::codegen::iot_gen::{self, TemplateNumbers};
use crate::panels::mcu_module::iot::{self, Active, Availability, IotConfig, Link, Platform};
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::mcu::model::Runtime;
use eframe::egui;

/// Generated: the tab's own green.
pub(super) const GENERATED: egui::Color32 = egui::Color32::from_rgb(120, 200, 140);
/// Switched on in the tab, not generated: the tab's own orange.
pub(super) const BLOCKED: egui::Color32 = egui::Color32::from_rgb(235, 150, 90);
/// The path an error takes back.
const ERROR_PATH: egui::Color32 = egui::Color32::from_rgb(200, 160, 95);
const FG: egui::Color32 = egui::Color32::from_rgb(210, 214, 226);
const DIM: egui::Color32 = egui::Color32::from_rgb(140, 146, 160);
const BAND_FILL: egui::Color32 = egui::Color32::from_rgb(40, 44, 52);
const BOX_FILL: egui::Color32 = egui::Color32::from_rgb(46, 51, 61);
const BORDER: egui::Color32 = egui::Color32::from_rgb(70, 76, 90);

/// One row of text.
const ROW: f32 = 20.0;
/// Inside a band, above and below its rows.
const PAD: f32 = 5.0;
/// Between two bands: room for the arrow joining them.
const GAP: f32 = 12.0;
/// The bands' left column, where the layer is named.
const LABEL_W: f32 = 92.0;

/// Muted body text, as the tab writes it.
fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(11.0)
        .color(egui::Color32::from_rgb(130, 130, 145))
}

/// What the diagrams are drawn from: the generator's decision and the tab's
/// settings, taken once per frame before the cards edit them.
pub(super) struct Facts {
    active: Option<Active>,
    iot: IotConfig,
    is_async: bool,
    family: String,
    /// The chip's Wi-Fi radio and Bluetooth radio, and whether it has Thread
    /// and ESP-NOW at all.
    wifi: Option<Platform>,
    bt: Option<Platform>,
    thread: bool,
    esp_now: bool,
    /// Bluetooth kept off an nRF by USB; Thread kept off by another link.
    ble_usb: bool,
    thread_blocked: bool,
    /// The chip's Bluetooth Mesh radio, and what keeps Mesh off it.
    mesh: Option<Platform>,
    mesh_blocked: Option<&'static str>,
    numbers: &'static TemplateNumbers,
}

impl Facts {
    pub(super) fn of(mcu: &Mcu) -> Self {
        let cyw43 = iot::has_cyw43(mcu);
        Self {
            active: iot::active(mcu),
            iot: mcu.iot.clone(),
            is_async: matches!(mcu.runtime, Runtime::Async),
            family: mcu.family.clone(),
            wifi: iot::platform(&mcu.family, cyw43),
            bt: iot::ble_platform(&mcu.family, cyw43),
            thread: iot::thread_platform(&mcu.family),
            esp_now: iot::availability(Link::EspNow, &mcu.family, cyw43) == Availability::Ready,
            ble_usb: iot::nrf_ble_blocked_by_usb(mcu),
            thread_blocked: iot::thread_blocked(mcu),
            mesh: iot::mesh_platform(&mcu.family, cyw43),
            mesh_blocked: iot::mesh_blocked(mcu),
            numbers: iot_gen::template_numbers(),
        }
    }

    /// Is this part of the generator's decision on?
    fn on(&self, pick: fn(&Active) -> bool) -> bool {
        self.active.as_ref().is_some_and(pick)
    }

    /// `None` when generated; else why not - the runtime first, since nothing
    /// is generated off Async.
    fn missing(&self, generated: bool, why: impl FnOnce() -> String) -> Option<String> {
        if generated {
            None
        } else if !self.is_async {
            Some("needs the Async runtime (System tab)".to_owned())
        } else {
            Some(why())
        }
    }
}

// ── The stack ─────────────────────────────────────────────────────────────────

/// A layer of the stack, top to bottom as it is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Layer {
    Code,
    Application,
    Network,
    Link,
}

impl Layer {
    fn title(self) -> &'static str {
        match self {
            Layer::Code => "YOUR CODE",
            Layer::Application => "APPLICATION",
            Layer::Network => "NETWORK",
            Layer::Link => "LINK",
        }
    }
}

/// One row of a layer: what runs there, in which file, and what it talks to.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Item {
    pub name: String,
    pub file: Option<&'static str>,
    pub detail: String,
    /// What it talks to - a host, a phone, a mesh. Empty: nothing outside.
    pub remote: String,
    /// `None`: generated. `Some(why)`: switched on in the tab, not generated.
    pub blocked: Option<String>,
    /// The hover: what it does.
    pub tip: String,
}

/// A layer and its rows.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Band {
    pub layer: Layer,
    pub items: Vec<Item>,
}

/// `[192, 168, 1, 50]` as `192.168.1.50`.
fn dotted(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

/// The stack the board runs, as the tab is set: every switched-on link and
/// protocol, generated (green) or not and why (orange), with your code's
/// calls on top. Empty when no link is on.
pub(super) fn overview_bands(f: &Facts) -> Vec<Band> {
    let n = f.numbers;
    let mut link = Vec::new();
    let wifi = f.iot.wifi && f.wifi.is_some();
    if wifi {
        let radio = match f.wifi {
            Some(Platform::Cyw43) => "the CYW43 radio beside the RP".to_owned(),
            _ => format!(
                "esp-radio, at most {}",
                iot::wifi_tx_power_step(f.iot.wifi_tx_power).1
            ),
        };
        link.push(Item {
            name: "Wi-Fi station".into(),
            file: Some(iot_gen::WIFI),
            detail: radio,
            remote: "access point (SSID in secrets.rs)".into(),
            blocked: f.missing(f.on(|a| a.station), || "not generated".into()),
            tip: format!(
                "wifi.rs joins the access point, and joins again {} s after losing it.",
                n.wifi_retry_s
            ),
        });
    }
    if f.esp_now
        && let Some(e) = &f.iot.esp_now
    {
        link.push(Item {
            name: "ESP-NOW".into(),
            file: Some(iot_gen::ESPNOW),
            detail: if wifi {
                "on the access point's channel".into()
            } else {
                format!("channel {}", e.channel)
            },
            remote: match e.peers.len() {
                0 => "broadcast".into(),
                k => format!("{k} peer(s) and broadcast"),
            },
            blocked: f.missing(f.on(|a| a.esp_now), || "not generated".into()),
            tip: "espnow.rs owns ESP-NOW: frames of up to 250 bytes between ESP chips, no access point.".into(),
        });
    }
    if f.bt.is_some()
        && let Some(b) = &f.iot.ble
    {
        link.push(Item {
            name: "Bluetooth LE".into(),
            file: Some(iot_gen::BLE),
            detail: format!("Nordic UART, \"{}\"", b.device_name.trim()),
            remote: "a phone (central)".into(),
            blocked: f.missing(f.on(|a| a.ble), || {
                if f.ble_usb {
                    "USB is wired: on an nRF both need CLOCK_POWER".into()
                } else {
                    "not generated".into()
                }
            }),
            tip:
                "ble.rs advertises the name; a phone connects, writes and subscribes - no pairing."
                    .into(),
        });
    }
    if f.mesh.is_some()
        && let Some(m) = &f.iot.ble_mesh
    {
        let mut groups = Vec::new();
        if m.onoff_server || m.onoff_client {
            groups.push(format!("on/off 0x{:04X}", m.onoff_group));
        }
        if m.sensor_server || m.sensor_client {
            groups.push(format!("sensor 0x{:04X}", m.sensor_group));
        }
        link.push(Item {
            name: "Bluetooth Mesh".into(),
            file: Some(iot_gen::BLE_MESH),
            detail: format!(
                "node 0x{:04X}{}",
                m.address,
                if m.relay { ", relays" } else { "" }
            ),
            remote: if groups.is_empty() {
                "the mesh".into()
            } else {
                format!("mesh groups: {}", groups.join(", "))
            },
            blocked: f.missing(f.on(|a| a.ble_mesh), || {
                f.mesh_blocked.unwrap_or("not generated").into()
            }),
            tip: concat!(
                "ble_mesh.rs: a node the IDE provisioned (its keys in secrets.rs). It sends ",
                "and hears mesh packets over advertising, relays them when Relay is on, and ",
                "keeps its sequence number in the flash store."
            )
            .into(),
        });
    }
    if f.thread
        && let Some(t) = &f.iot.thread
    {
        link.push(Item {
            name: "Thread".into(),
            file: Some(iot_gen::THREAD),
            detail: format!("OpenThread end device, UDP :{}", t.udp_port),
            remote: "mesh -> border router".into(),
            blocked: f.missing(f.on(|a| a.thread), || {
                if f.thread_blocked {
                    "the radio is another link's: one at a time".into()
                } else {
                    "not generated".into()
                }
            }),
            tip: "thread.rs joins the network in THREAD_DATASET (secrets.rs) and sends UDP over the mesh.".into(),
        });
    }
    if link.is_empty() {
        return Vec::new();
    }

    let mut app = Vec::new();
    let mut net = Vec::new();
    if wifi {
        let station = f.on(|a| a.station);
        let station_why = || "needs the Wi-Fi station".to_owned();
        let ip = &f.iot.ip;
        net.push(Item {
            name: "embassy-net".into(),
            file: Some(iot_gen::NET),
            detail: if ip.dhcp {
                "IPv4 by DHCP, DNS".into()
            } else {
                format!("IPv4 {}/{}, DNS", dotted(ip.address), ip.prefix)
            },
            remote: String::new(),
            blocked: f.missing(station, station_why),
            tip: "net.rs: the IP stack - TCP, UDP, DNS - over the Wi-Fi station.".into(),
        });
        let coex = f.active.is_some_and(|a| a.ble && (a.station || a.esp_now));
        if let Some(m) = &f.iot.mqtt {
            let tls = f.on(|a| a.mqtt_tls);
            let mut detail = if tls {
                "MQTT 5 over TLS 1.3 (tls.rs)".to_owned()
            } else if m.tls {
                "MQTT 5 over TLS".to_owned()
            } else {
                "MQTT 5, plain TCP".to_owned()
            };
            if f.on(|a| a.mqtt_client_cert) {
                detail.push_str(", the board's certificate");
            }
            app.push(Item {
                name: "MQTT".into(),
                file: Some(iot_gen::MQTT),
                detail,
                remote: format!("{}:{}", m.host.trim(), m.port),
                blocked: f.missing(f.on(|a| a.mqtt), || {
                    if !station {
                        return station_why();
                    }
                    match iot::tls_available(&f.family, coex) {
                        Err(e) if m.tls => {
                            format!("TLS asked for, and {e}: not generated, never in clear")
                        }
                        _ => "not generated".into(),
                    }
                }),
                tip: format!(
                    "mqtt.rs keeps one connection to the broker and starts again {} s after any error.",
                    n.mqtt_retry_s
                ),
            });
        }
        if let Some(s) = &f.iot.sntp {
            app.push(Item {
                name: "SNTP".into(),
                file: Some(iot_gen::SNTP),
                detail: format!("every {} s", s.interval_s.max(iot::MIN_SNTP_INTERVAL_S)),
                remote: format!("{}:123", s.server.trim()),
                blocked: f.missing(f.on(|a| a.sntp), station_why),
                tip:
                    "sntp.rs asks the server for the time; now_unix() reads it, never the network."
                        .into(),
            });
        }
        if let Some(h) = &f.iot.http {
            let detail = if f.on(|a| a.http_tls) {
                let mut d = "HTTP/1.1 and HTTPS (tls.rs)".to_owned();
                if f.on(|a| a.http_client_cert) {
                    d.push_str(&format!(
                        ", the board's certificate to {}",
                        iot::http_cert_host(h)
                    ));
                }
                d
            } else if h.tls {
                let mqtt_tls = f.on(|a| a.mqtt_tls);
                match iot::https_available(&f.family, coex, mqtt_tls) {
                    Err(e) => format!("HTTP/1.1; https:// refused here: {e}"),
                    Ok(()) => "HTTP/1.1".into(),
                }
            } else {
                "HTTP/1.1, no HTTPS".into()
            };
            app.push(Item {
                name: "HTTP client".into(),
                file: Some(iot_gen::HTTP),
                detail,
                remote: "the URLs your code asks for".into(),
                blocked: f.missing(f.on(|a| a.http), station_why),
                tip: format!(
                    "http.rs makes one request at a time, a new connection each, at most {} s each.",
                    h.timeout_s
                ),
            });
        }
    }

    // Your code's calls: what is generated, nothing else.
    let mut code = Vec::new();
    let api = |name: &str, file: &'static str| Item {
        name: name.to_owned(),
        file: Some(file),
        detail: String::new(),
        remote: String::new(),
        blocked: None,
        tip: format!("Call it from your code as pins::configs::{name}."),
    };
    if f.on(|a| a.mqtt) {
        code.push(api("mqtt::publish / incoming", iot_gen::MQTT));
    }
    if f.on(|a| a.http) {
        code.push(api("http::get / post / request", iot_gen::HTTP));
    }
    if f.on(|a| a.sntp) {
        code.push(api("sntp::now_unix / wait_synced", iot_gen::SNTP));
    }
    if f.on(|a| a.esp_now) {
        code.push(api("espnow::send / receive", iot_gen::ESPNOW));
    }
    if f.on(|a| a.ble) {
        code.push(api("ble::send / receive / connected", iot_gen::BLE));
    }
    if f.on(|a| a.thread) {
        code.push(api("thread::send_to / receive / role", iot_gen::THREAD));
    }
    if f.on(|a| a.ble_mesh)
        && let Some(m) = &f.iot.ble_mesh
    {
        // The model's own call: the light hears, the switch sends, the
        // sensor sets, the reader asks.
        let calls: Vec<&str> = [
            (m.onoff_server, "onoff_changed"),
            (m.onoff_client, "onoff_set"),
            (m.sensor_server, "sensor_set"),
            (m.sensor_client, "sensor_get"),
        ]
        .into_iter()
        .filter_map(|(on, call)| on.then_some(call))
        .collect();
        let calls = if calls.is_empty() {
            "stats".to_owned()
        } else {
            calls.join(" / ")
        };
        code.push(api(&format!("ble_mesh::{calls}"), iot_gen::BLE_MESH));
    }

    [
        (Layer::Code, code),
        (Layer::Application, app),
        (Layer::Network, net),
        (Layer::Link, link),
    ]
    .into_iter()
    .filter(|(_, items)| !items.is_empty())
    .map(|(layer, items)| Band { layer, items })
    .collect()
}

// ── Step chains ───────────────────────────────────────────────────────────────

/// One step of a sequence: who does it, what, and its limit.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Step {
    /// The file doing it. Drawn where it changes from the step above.
    pub owner: Option<&'static str>,
    pub text: String,
    /// A deadline or a count, at the right.
    pub note: String,
}

/// A sequence, the step an error goes back to, and a line under it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Chain {
    pub steps: Vec<Step>,
    /// `(step, label)`: any error goes back to that step.
    pub back: Option<(usize, String)>,
    pub foot: String,
}

fn step(owner: Option<&'static str>, text: impl Into<String>, note: impl Into<String>) -> Step {
    Step {
        owner,
        text: text.into(),
        note: note.into(),
    }
}

/// How the board reaches the broker, and what it does on any error. `None`
/// while MQTT is not generated.
pub(super) fn mqtt_chain(f: &Facts) -> Option<Chain> {
    let a = f.active.filter(|a| a.mqtt)?;
    let m = f.iot.mqtt.as_ref()?;
    let n = f.numbers;
    let mut steps = vec![
        step(
            Some(iot_gen::WIFI),
            "join the access point",
            format!("again {} s after losing it", n.wifi_retry_s),
        ),
        step(
            Some(iot_gen::NET),
            if f.iot.ip.dhcp {
                "link up, an address by DHCP".to_owned()
            } else {
                format!("link up, address {}", dotted(f.iot.ip.address))
            },
            "",
        ),
    ];
    if a.mqtt_tls && a.sntp {
        steps.push(step(
            Some(iot_gen::MQTT),
            "SNTP's first time, for the certificates' dates",
            format!("up to {} s, once", iot_gen::CLOCK_WAIT_S),
        ));
    }
    let host = m.host.trim();
    steps.push(step(
        Some(iot_gen::MQTT),
        if host.parse::<std::net::Ipv4Addr>().is_ok() {
            format!("address {host}")
        } else {
            format!("DNS: {host}")
        },
        "",
    ));
    steps.push(step(
        Some(iot_gen::MQTT),
        format!("TCP connect, port {}", m.port),
        "",
    ));
    if a.mqtt_tls {
        let mut text = if m.verify {
            "TLS 1.3 handshake, the broker's certificate checked".to_owned()
        } else {
            "TLS 1.3 handshake, the certificate NOT checked".to_owned()
        };
        if a.mqtt_client_cert {
            text.push_str(", the board's sent");
        }
        steps.push(step(
            Some(iot_gen::TLS),
            text,
            format!("up to {} s", n.tls_open_s),
        ));
    }
    steps.push(step(
        Some(iot_gen::MQTT),
        "CONNECT, the broker answers CONNACK",
        format!("up to {} s; then connected()", n.mqtt_answer_s),
    ));
    // The Device Shadow's answer topics go first, 7 a shadow, then the tab's
    // own filters - the order mqtt.rs subscribes in, skipping a filter of the
    // tab's that is one of the shadow's already.
    let shadow_topics = iot::mqtt_shadow(m)
        .map(|s| iot::shadow_filters(&s.thing, &s.names))
        .unwrap_or_default();
    let shadow = shadow_topics.len();
    let own = iot::subscriptions(m)
        .into_iter()
        .filter(|f| !shadow_topics.iter().any(|t| t == f))
        .count();
    if shadow > 0 {
        steps.push(step(
            Some(iot_gen::MQTT),
            format!("SUBSCRIBE x{shadow}: the Device Shadow's answer topics"),
            format!("up to {} s each", n.mqtt_answer_s),
        ));
    }
    if own > 0 {
        steps.push(step(
            Some(iot_gen::MQTT),
            if shadow > 0 {
                format!("SUBSCRIBE x{own}: your filters")
            } else {
                format!("SUBSCRIBE x{own}")
            },
            format!("up to {} s each", n.mqtt_answer_s),
        ));
    }
    if shadow > 0 {
        // Counted from the shadow's last SUBACK, so your filters' subscribing
        // overlaps it: AWS delivers to a new subscription a little after.
        steps.push(step(
            Some(iot_gen::MQTT),
            "the Device Shadow is ready",
            format!("{} s after its last SUBACK", n.mqtt_settle_s),
        ));
    }
    steps.push(step(
        Some(iot_gen::MQTT),
        "loop: send what is queued, ping, receive",
        if m.keep_alive_s == 0 {
            "keep-alive off".to_owned()
        } else {
            format!("keep-alive {} s", m.keep_alive_s)
        },
    ));
    Some(Chain {
        steps,
        back: Some((
            1,
            format!(
                "Any error: connected() turns false, and after {} s it starts again from the address.",
                n.mqtt_retry_s
            ),
        )),
        foot: String::new(),
    })
}

/// One HTTP request, start to end. `None` while the client is not generated.
pub(super) fn http_chain(f: &Facts) -> Option<Chain> {
    let a = f.active.filter(|a| a.http)?;
    let h = f.iot.http.as_ref()?;
    let n = f.numbers;
    let mut steps = vec![
        step(
            Some(iot_gen::HTTP),
            "your call waits for its turn",
            format!("one at a time; {} wait, more: Busy", n.http_queue),
        ),
        step(Some(iot_gen::HTTP), "DNS: the URL's host", ""),
        step(Some(iot_gen::HTTP), "TCP connect", ""),
    ];
    if a.http_tls {
        let mut text = if h.verify {
            "https://: TLS 1.3, the server's certificate checked".to_owned()
        } else {
            "https://: TLS 1.3, the certificate NOT checked".to_owned()
        };
        if a.http_client_cert {
            text.push_str(&format!(
                ", the board's sent to {} only",
                iot::http_cert_host(h)
            ));
        }
        steps.push(step(
            Some(iot_gen::TLS),
            text,
            format!("up to {} s", n.tls_open_s),
        ));
    }
    steps.push(step(
        Some(iot_gen::HTTP),
        "the request: head and body",
        format!("body up to {} B", h.payload_max),
    ));
    steps.push(step(
        Some(iot_gen::HTTP),
        "the answer: status and body",
        format!("body up to {} B, the rest cut", h.body_max),
    ));
    Some(Chain {
        steps,
        back: None,
        foot: format!(
            "At most {} s in all, and no retry: a new connection for each request, closed after it.",
            h.timeout_s
        ),
    })
}

// ── Where a message waits ─────────────────────────────────────────────────────

/// Your code, the queues each way, the task that owns the link, the far end.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct QueuePath {
    pub out_api: &'static str,
    pub in_api: &'static str,
    pub task: &'static str,
    pub remote: &'static str,
    pub cap: u64,
    pub notes: Vec<String>,
}

pub(super) fn mqtt_path(n: &TemplateNumbers) -> QueuePath {
    QueuePath {
        out_api: "publish(topic, bytes)",
        in_api: "incoming()",
        task: "mqtt.rs task",
        remote: "broker",
        cap: n.mqtt_queue,
        notes: vec![
            format!(
                "publish() waits while {} are queued - while the broker is away, that is; try_publish() never waits (QueueFull).",
                n.mqtt_queue
            ),
            format!(
                "A message arriving while {} wait, or over {} bytes, is dropped.",
                n.mqtt_queue, n.mqtt_max_payload
            ),
        ],
    }
}

pub(super) fn espnow_path(n: &TemplateNumbers) -> QueuePath {
    QueuePath {
        out_api: "send(to, data)",
        in_api: "receive()",
        task: "espnow.rs task",
        remote: "peers",
        cap: n.espnow_queue,
        notes: vec![
            format!(
                "send() waits only while {} are queued; whether a peer got it is not reported.",
                n.espnow_queue
            ),
            format!("A frame arriving while {} wait is dropped.", n.espnow_queue),
        ],
    }
}

pub(super) fn ble_path(n: &TemplateNumbers) -> QueuePath {
    QueuePath {
        out_api: "send(data)",
        in_api: "receive()",
        task: "ble.rs task",
        remote: "phone",
        cap: n.ble_queue,
        notes: vec![
            "With no phone connected the queue is emptied: send() never stalls, and nothing is sent."
                .to_owned(),
            format!(
                "Up to {} bytes a notification; a write arriving while {} wait is dropped.",
                n.ble_max_data, n.ble_queue
            ),
        ],
    }
}

pub(super) fn thread_path(n: &TemplateNumbers) -> QueuePath {
    QueuePath {
        out_api: "send_to(addr, port, data)",
        in_api: "receive()",
        task: "thread.rs task",
        remote: "mesh",
        cap: n.thread_queue,
        notes: vec![
            format!("send_to() waits only while {} are queued.", n.thread_queue),
            format!(
                "A datagram arriving while {} wait, or over {} bytes, is dropped.",
                n.thread_queue, n.thread_max_data
            ),
        ],
    }
}

// ── The sections in the tab ───────────────────────────────────────────────────

/// The stack, at the top of the tab: open until the user closes it.
pub(super) fn overview(ui: &mut egui::Ui, f: &Facts) {
    egui::CollapsingHeader::new(egui::RichText::new("How it fits together").size(12.0))
        .id_salt("iot_how_overview")
        .default_open(true)
        .show(ui, |ui| {
            let bands = overview_bands(f);
            if bands.is_empty() {
                ui.label(dim(
                    "Switch a link on below: this draws what the board will run on it.",
                ));
                return;
            }
            paint_bands(ui, &bands);
        });
}

/// A closed "How it works" section, said to be the IDE's template.
fn how(ui: &mut egui::Ui, salt: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::CollapsingHeader::new(egui::RichText::new("How it works").size(11.0))
        .id_salt(salt)
        .default_open(false)
        .show(ui, |ui| {
            ui.label(dim(
                "As the IDE generates it - your own edits below the markers are not read here.",
            ));
            ui.add_space(4.0);
            body(ui);
        });
}

pub(super) fn how_mqtt(ui: &mut egui::Ui, f: &Facts) {
    let Some(chain) = mqtt_chain(f) else { return };
    ui.indent("mqtt_how", |ui| {
        how(ui, "iot_how_mqtt", |ui| {
            paint_chain(ui, &chain);
            ui.add_space(8.0);
            paint_path(ui, &mqtt_path(f.numbers));
        });
    });
}

pub(super) fn how_http(ui: &mut egui::Ui, f: &Facts) {
    let Some(chain) = http_chain(f) else { return };
    ui.indent("http_how", |ui| {
        how(ui, "iot_how_http", |ui| paint_chain(ui, &chain));
    });
}

pub(super) fn how_espnow(ui: &mut egui::Ui, f: &Facts) {
    if f.on(|a| a.esp_now) {
        ui.indent("espnow_how", |ui| {
            how(ui, "iot_how_espnow", |ui| {
                paint_path(ui, &espnow_path(f.numbers))
            });
        });
    }
}

pub(super) fn how_ble(ui: &mut egui::Ui, f: &Facts) {
    if f.on(|a| a.ble) {
        ui.indent("ble_how", |ui| {
            how(ui, "iot_how_ble", |ui| paint_path(ui, &ble_path(f.numbers)));
        });
    }
}

pub(super) fn how_thread(ui: &mut egui::Ui, f: &Facts) {
    if f.on(|a| a.thread) {
        ui.indent("thread_how", |ui| {
            how(ui, "iot_how_thread", |ui| {
                paint_path(ui, &thread_path(f.numbers))
            });
        });
    }
}

// ── Painting ──────────────────────────────────────────────────────────────────

/// `text` on one line, cut with "…" past `max_w`.
fn line(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: egui::Color32,
    max_w: f32,
) -> Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::proportional(size), color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(8.0));
    painter.layout_job(job)
}

/// A filled arrowhead at `tip`, pointing along `dir`.
fn head(painter: &egui::Painter, tip: egui::Pos2, dir: egui::Vec2, color: egui::Color32) {
    let d = dir.normalized();
    let side = egui::vec2(-d.y, d.x);
    let back = tip - d * 6.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side * 3.5, back - side * 3.5],
        color,
        egui::Stroke::NONE,
    ));
}

/// A line from `a` to `b` with a head at `b`.
fn arrow(painter: &egui::Painter, a: egui::Pos2, b: egui::Pos2, color: egui::Color32) {
    painter.line_segment([a, b], egui::Stroke::new(1.3_f32, color));
    head(painter, b, b - a, color);
}

/// A tooltip at the pointer, one Id for every diagram - each tooltip is an
/// Area egui keeps.
fn tip(ui: &egui::Ui, text: &str) {
    egui::Tooltip::always_open(
        ui.ctx().clone(),
        ui.layer_id(),
        egui::Id::new("iot_diagram_tip"),
        egui::PopupAnchor::Pointer,
    )
    .gap(12.0)
    .show(|ui| {
        ui.set_max_width(340.0);
        ui.label(egui::RichText::new(text).size(11.0));
    });
}

/// The bands of the stack, with a two-way arrow between each, the rows'
/// hovers, and the legend under them.
fn paint_bands(ui: &mut egui::Ui, bands: &[Band]) {
    let rows: usize = bands.iter().map(|b| b.items.len()).sum();
    let width = ui.available_width().max(240.0);
    let legend = ROW;
    let height = rows as f32 * ROW
        + bands.len() as f32 * 2.0 * PAD
        + (bands.len() - 1) as f32 * GAP
        + legend;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let mut hits: Vec<(egui::Rect, String)> = Vec::new();
    let mut y = rect.top();
    for (bi, band) in bands.iter().enumerate() {
        let h = band.items.len() as f32 * ROW + 2.0 * PAD;
        let br = egui::Rect::from_min_size(egui::pos2(rect.left(), y), egui::vec2(width, h));
        painter.rect_filled(br, 5.0, BAND_FILL);
        let title = line(&painter, band.layer.title(), 10.0, DIM, LABEL_W - 12.0);
        painter.galley(
            egui::pos2(
                br.left() + 8.0,
                br.top() + PAD + (ROW - title.size().y) / 2.0,
            ),
            title,
            DIM,
        );
        for (i, item) in band.items.iter().enumerate() {
            let row = egui::Rect::from_min_size(
                egui::pos2(br.left() + LABEL_W, br.top() + PAD + i as f32 * ROW),
                egui::vec2(width - LABEL_W - 8.0, ROW),
            );
            paint_item(&painter, row, item);
            let mut text = item.tip.clone();
            if let Some(why) = &item.blocked {
                text.push_str(&format!("\n\nNot generated: {why}."));
            }
            hits.push((row, text));
        }
        y += h;
        if bi + 1 < bands.len() {
            // Data goes both ways between two layers.
            let x = rect.left() + 24.0;
            let (top, bottom) = (egui::pos2(x, y + 1.0), egui::pos2(x, y + GAP - 1.0));
            painter.line_segment([top, bottom], egui::Stroke::new(1.3_f32, DIM));
            head(&painter, top, egui::vec2(0.0, -1.0), DIM);
            head(&painter, bottom, egui::vec2(0.0, 1.0), DIM);
            y += GAP;
        }
    }
    // The legend: what green and orange mean.
    let ly = y + ROW / 2.0;
    let mut x = rect.left() + 6.0;
    let blocked = bands
        .iter()
        .flat_map(|b| &b.items)
        .any(|i| i.blocked.is_some());
    for (color, text) in [
        (GENERATED, "generated"),
        (BLOCKED, "switched on, not generated - hover for why"),
    ] {
        if color == BLOCKED && !blocked {
            continue;
        }
        painter.circle_filled(egui::pos2(x + 4.0, ly), 3.5, color);
        let g = line(&painter, text, 10.5, DIM, rect.right() - x - 14.0);
        let w = g.size().x;
        painter.galley(egui::pos2(x + 12.0, ly - g.size().y / 2.0), g, DIM);
        x += 12.0 + w + 16.0;
    }
    if let Some(pos) = resp.hover_pos()
        && let Some((_, text)) = hits.iter().find(|(r, _)| r.contains(pos))
    {
        tip(ui, text);
    }
}

/// A row of the stack: a dot (green or orange), the name, the file, the
/// detail - and, at the right, what it talks to.
fn paint_item(painter: &egui::Painter, row: egui::Rect, item: &Item) {
    let cy = row.center().y;
    let state = if item.blocked.is_some() {
        BLOCKED
    } else {
        GENERATED
    };
    painter.circle_filled(egui::pos2(row.left() + 4.0, cy), 3.5, state);
    // What it talks to first, so the left side knows where to stop.
    let mut right = row.right();
    if !item.remote.is_empty() {
        let g = line(
            painter,
            &format!("-> {}", item.remote),
            11.0,
            FG,
            row.width() * 0.42,
        );
        right -= g.size().x;
        painter.galley(egui::pos2(right, cy - g.size().y / 2.0), g, FG);
        right -= 10.0;
    }
    let mut x = row.left() + 14.0;
    let name_color = if item.blocked.is_some() { BLOCKED } else { FG };
    let pieces = [
        (item.name.as_str(), 12.0, name_color),
        (item.file.unwrap_or(""), 10.5, DIM),
        (item.detail.as_str(), 11.0, DIM),
    ];
    for (text, size, color) in pieces {
        if text.is_empty() || x >= right - 8.0 {
            continue;
        }
        let g = line(painter, text, size, color, right - x);
        let w = g.size().x;
        painter.galley(egui::pos2(x, cy - g.size().y / 2.0), g, color);
        x += w + 8.0;
    }
}

/// A sequence down a rail: the file where it changes, a node per step, its
/// limit at the right - and the path back an error takes, on the left.
fn paint_chain(ui: &mut egui::Ui, chain: &Chain) {
    const OWNER_W: f32 = 58.0;
    let width = ui.available_width().max(240.0);
    let height = chain.steps.len() as f32 * ROW + 4.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let rail = rect.left() + OWNER_W + 14.0;
    let y_of = |i: usize| rect.top() + 2.0 + i as f32 * ROW + ROW / 2.0;
    let mut owner = None;
    for (i, s) in chain.steps.iter().enumerate() {
        let y = y_of(i);
        if i > 0 {
            arrow(
                &painter,
                egui::pos2(rail, y_of(i - 1) + 4.0),
                egui::pos2(rail, y - 4.0),
                DIM,
            );
        }
        painter.circle_filled(egui::pos2(rail, y), 3.0, FG);
        if s.owner.is_some() && s.owner != owner {
            let g = line(&painter, s.owner.unwrap_or(""), 10.0, DIM, OWNER_W);
            painter.galley(egui::pos2(rect.left() + 2.0, y - g.size().y / 2.0), g, DIM);
        }
        owner = s.owner.or(owner);
        let mut right = rect.right() - 2.0;
        if !s.note.is_empty() {
            let g = line(&painter, &s.note, 10.5, DIM, width * 0.38);
            right -= g.size().x;
            painter.galley(egui::pos2(right, y - g.size().y / 2.0), g, DIM);
            right -= 10.0;
        }
        let g = line(&painter, &s.text, 11.5, FG, right - (rail + 10.0));
        painter.galley(egui::pos2(rail + 10.0, y - g.size().y / 2.0), g, FG);
    }
    if let Some((to, _)) = &chain.back
        && *to < chain.steps.len()
    {
        // From the last step, back up the left of the rail into `to`.
        let x = rail - 9.0;
        let (from_y, to_y) = (y_of(chain.steps.len() - 1), y_of(*to));
        let stroke = egui::Stroke::new(1.3_f32, ERROR_PATH);
        painter.line_segment(
            [egui::pos2(rail - 4.0, from_y), egui::pos2(x, from_y)],
            stroke,
        );
        painter.line_segment([egui::pos2(x, from_y), egui::pos2(x, to_y)], stroke);
        arrow(
            &painter,
            egui::pos2(x, to_y),
            egui::pos2(rail - 4.0, to_y),
            ERROR_PATH,
        );
    }
    if let Some((_, label)) = &chain.back {
        ui.label(egui::RichText::new(label).size(11.0).color(ERROR_PATH));
    }
    if !chain.foot.is_empty() {
        ui.label(dim(&chain.foot));
    }
}

/// Your code at the left, the queues, the task that owns the link, the far
/// end: out on the top lane, in on the bottom one - and what happens when a
/// queue is full, under it.
fn paint_path(ui: &mut egui::Ui, p: &QueuePath) {
    let width = ui.available_width().max(260.0);
    let height = 54.0;
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let (y_out, y_in) = (rect.top() + 14.0, rect.top() + 40.0);
    let api_r = rect.left() + width * 0.30;
    let cell = 9.0;
    let cells = p.cap.clamp(1, 8) as f32;
    let q_l = api_r + 22.0;
    let q_r = q_l + cells * cell;
    let t_l = q_r + 22.0;
    let t_r = rect.left() + width * 0.76;
    let r_l = t_r + 22.0;
    let r_r = rect.right() - 2.0;

    for (api, y) in [(p.out_api, y_out), (p.in_api, y_in)] {
        let g = line(&painter, api, 11.0, FG, api_r - rect.left() - 4.0);
        painter.galley(egui::pos2(api_r - g.size().x, y - g.size().y / 2.0), g, FG);
        // The queue: one cell a message.
        for c in 0..cells as usize {
            let r = egui::Rect::from_min_size(
                egui::pos2(q_l + c as f32 * cell, y - 6.0),
                egui::vec2(cell - 1.0, 12.0),
            );
            painter.rect_filled(r, 1.5, BOX_FILL);
            painter.rect_stroke(
                r,
                1.5,
                egui::Stroke::new(1.0_f32, BORDER),
                egui::StrokeKind::Inside,
            );
        }
    }
    let cap = line(
        &painter,
        &format!("{} queued", p.cap),
        9.5,
        DIM,
        t_l - q_l + 20.0,
    );
    painter.galley(
        egui::pos2(
            (q_l + q_r - cap.size().x) / 2.0,
            (y_out + y_in - cap.size().y) / 2.0,
        ),
        cap,
        DIM,
    );
    // The task and the far end, each one box across both lanes.
    for (l, r, text) in [(t_l, t_r, p.task), (r_l, r_r, p.remote)] {
        if r - l < 16.0 {
            continue;
        }
        let b = egui::Rect::from_min_max(
            egui::pos2(l, rect.top() + 3.0),
            egui::pos2(r, rect.bottom() - 3.0),
        );
        painter.rect_filled(b, 5.0, BOX_FILL);
        painter.rect_stroke(
            b,
            5.0,
            egui::Stroke::new(1.0_f32, BORDER),
            egui::StrokeKind::Inside,
        );
        let g = line(&painter, text, 11.0, FG, b.width() - 8.0);
        painter.galley(b.center() - g.size() / 2.0, g, FG);
    }
    // Out: left to right. In: right to left.
    arrow(
        &painter,
        egui::pos2(api_r + 4.0, y_out),
        egui::pos2(q_l - 3.0, y_out),
        DIM,
    );
    arrow(
        &painter,
        egui::pos2(q_r + 3.0, y_out),
        egui::pos2(t_l - 2.0, y_out),
        DIM,
    );
    arrow(
        &painter,
        egui::pos2(t_r + 2.0, y_out),
        egui::pos2(r_l - 2.0, y_out),
        DIM,
    );
    arrow(
        &painter,
        egui::pos2(r_l - 2.0, y_in),
        egui::pos2(t_r + 3.0, y_in),
        DIM,
    );
    arrow(
        &painter,
        egui::pos2(t_l - 2.0, y_in),
        egui::pos2(q_r + 3.0, y_in),
        DIM,
    );
    arrow(
        &painter,
        egui::pos2(q_l - 3.0, y_in),
        egui::pos2(api_r + 4.0, y_in),
        DIM,
    );
    for note in &p.notes {
        ui.label(dim(note));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::iot::{
        BleConfig, HttpConfig, MqttConfig, SntpConfig, ThreadConfig,
    };

    fn board(id: &str, runtime: Runtime) -> Mcu {
        let mut mcu = crate::panels::mcu_module::builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu();
        mcu.runtime = runtime;
        mcu
    }

    fn band(bands: &[Band], layer: Layer) -> Option<&Band> {
        bands.iter().find(|b| b.layer == layer)
    }

    fn item<'a>(bands: &'a [Band], layer: Layer, name: &str) -> &'a Item {
        band(bands, layer)
            .and_then(|b| b.items.iter().find(|i| i.name == name))
            .unwrap_or_else(|| panic!("no {name} in {layer:?}: {bands:#?}"))
    }

    /// Everything on an ESP32-C3: MQTT over TLS with the board's certificate,
    /// SNTP, HTTP - all generated, your code's calls on top.
    fn c3_everything() -> Mcu {
        let mut mcu = board("esp32c3", Runtime::Async);
        mcu.iot.wifi = true;
        let mut m = MqttConfig::for_chip("esp32c3");
        m.tls = true;
        m.client_cert = true;
        m.host = "broker.example.org".into();
        m.port = 8883;
        m.subscribe = vec!["a/#".into(), "b/+".into()];
        mcu.iot.mqtt = Some(m);
        mcu.iot.sntp = Some(SntpConfig::default());
        mcu.iot.http = Some(HttpConfig::default());
        mcu
    }

    /// Bluetooth Mesh on a C3, with the flash store it keeps its sequence
    /// number in when `store`.
    fn c3_mesh(store: bool) -> Mcu {
        use crate::panels::mcu_module::flash_store::{self, FlashStoreConfig};
        let mut mcu = board("esp32c3", Runtime::Async);
        mcu.iot.ble_mesh = Some(crate::panels::mcu_module::iot::BleMeshConfig::default());
        if store {
            let p = flash_store::platform_of(&mcu).expect("the C3 has a flash store");
            mcu.flash_store = Some(FlashStoreConfig::default_on(&p, &mcu.family));
        }
        mcu
    }

    /// Mesh is a link of its own: alone, it draws the link and your calls -
    /// never "switch a link on".
    #[test]
    fn mesh_alone_draws_its_link_and_your_calls() {
        let mut mcu = c3_mesh(true);
        let f = Facts::of(&mcu);
        assert!(f.on(|a| a.ble_mesh), "generated");
        let bands = overview_bands(&f);
        let layers: Vec<Layer> = bands.iter().map(|b| b.layer).collect();
        assert_eq!(layers, vec![Layer::Code, Layer::Link], "{bands:#?}");
        let mesh = item(&bands, Layer::Link, "Bluetooth Mesh");
        assert!(mesh.blocked.is_none() && mesh.file == Some(iot_gen::BLE_MESH));
        assert_eq!(mesh.detail, "node 0x0001, relays");
        assert_eq!(mesh.remote, "mesh groups: on/off 0xC000");
        item(&bands, Layer::Code, "ble_mesh::onoff_changed");

        // The calls follow the models.
        let m = mcu.iot.ble_mesh.as_mut().unwrap();
        m.onoff_server = false;
        m.onoff_client = true;
        m.sensor_client = true;
        let bands = overview_bands(&Facts::of(&mcu));
        item(&bands, Layer::Code, "ble_mesh::onoff_set / sensor_get");
        assert_eq!(
            item(&bands, Layer::Link, "Bluetooth Mesh").remote,
            "mesh groups: on/off 0xC000, sensor 0xC001"
        );
    }

    /// Switched on and held back: orange, with the generator's reason.
    #[test]
    fn mesh_says_what_keeps_it_off() {
        let why = |mcu: &Mcu| {
            let bands = overview_bands(&Facts::of(mcu));
            let calls = band(&bands, Layer::Code).map_or(&[][..], |b| &b.items[..]);
            assert!(
                !calls.iter().any(|i| i.name.starts_with("ble_mesh::")),
                "{calls:?}"
            );
            item(&bands, Layer::Link, "Bluetooth Mesh")
                .blocked
                .clone()
                .unwrap_or_default()
        };
        assert!(why(&c3_mesh(false)).contains("flash store"));
        let mut nus = c3_mesh(true);
        nus.iot.ble = Some(BleConfig::default());
        assert!(why(&nus).contains("Nordic UART"));
        let mut blocking = c3_mesh(true);
        blocking.runtime = Runtime::Blocking;
        assert!(why(&blocking).contains("Async"));
    }

    #[test]
    fn nothing_switched_on_draws_nothing() {
        let mcu = board("esp32c3", Runtime::Async);
        assert!(overview_bands(&Facts::of(&mcu)).is_empty());
    }

    #[test]
    fn wifi_alone_is_a_link_and_the_ip_stack() {
        let mut mcu = board("esp32c3", Runtime::Async);
        mcu.iot.wifi = true;
        let bands = overview_bands(&Facts::of(&mcu));
        let layers: Vec<Layer> = bands.iter().map(|b| b.layer).collect();
        assert_eq!(layers, vec![Layer::Network, Layer::Link], "{bands:#?}");
        let wifi = item(&bands, Layer::Link, "Wi-Fi station");
        assert!(wifi.blocked.is_none() && wifi.file == Some(iot_gen::WIFI));
        assert!(wifi.detail.contains("13 dBm"), "{wifi:?}");
    }

    #[test]
    fn the_stack_follows_the_generator() {
        let bands = overview_bands(&Facts::of(&c3_everything()));
        let layers: Vec<Layer> = bands.iter().map(|b| b.layer).collect();
        assert_eq!(
            layers,
            vec![Layer::Code, Layer::Application, Layer::Network, Layer::Link]
        );
        let mqtt = item(&bands, Layer::Application, "MQTT");
        assert!(mqtt.blocked.is_none());
        assert!(
            mqtt.detail.contains("TLS 1.3") && mqtt.detail.contains("board's certificate"),
            "{mqtt:?}"
        );
        assert_eq!(mqtt.remote, "broker.example.org:8883");
        let code: Vec<&str> = band(&bands, Layer::Code)
            .unwrap()
            .items
            .iter()
            .map(|i| i.name.as_str())
            .collect();
        assert!(code.iter().any(|c| c.starts_with("mqtt::")));
        assert!(code.iter().any(|c| c.starts_with("http::")));
        assert!(code.iter().any(|c| c.starts_with("sntp::")));
    }

    /// TLS asked for where the chip cannot carry it: MQTT orange, with the
    /// generator's own reason.
    #[test]
    fn a_c5_says_why_its_tls_mqtt_is_not_generated() {
        let mut mcu = board("esp32c5", Runtime::Async);
        mcu.iot.wifi = true;
        let mut m = MqttConfig::for_chip("esp32c5");
        m.tls = true;
        mcu.iot.mqtt = Some(m);
        let bands = overview_bands(&Facts::of(&mcu));
        let mqtt = item(&bands, Layer::Application, "MQTT");
        let why = mqtt.blocked.as_deref().unwrap_or("");
        assert!(
            why.contains("TRNG") && why.contains("never in clear"),
            "{why}"
        );
        assert!(band(&bands, Layer::Code).is_none(), "nothing to call");
    }

    /// Off Async nothing is generated, and every row says so.
    #[test]
    fn a_blocking_project_is_all_orange() {
        let mut mcu = c3_everything();
        mcu.runtime = Runtime::Blocking;
        let bands = overview_bands(&Facts::of(&mcu));
        assert!(!bands.is_empty());
        for i in bands.iter().flat_map(|b| &b.items) {
            assert_eq!(
                i.blocked.as_deref(),
                Some("needs the Async runtime (System tab)"),
                "{i:?}"
            );
        }
    }

    /// Thread on an nRF: a link with no IP stack of the board's own.
    #[test]
    fn nrf_thread_has_no_network_band() {
        let mut mcu = board("nrf52840_dk", Runtime::Async);
        mcu.iot.thread = Some(ThreadConfig::default());
        let bands = overview_bands(&Facts::of(&mcu));
        let layers: Vec<Layer> = bands.iter().map(|b| b.layer).collect();
        assert_eq!(layers, vec![Layer::Code, Layer::Link], "{bands:#?}");
        assert!(
            item(&bands, Layer::Link, "Thread")
                .remote
                .contains("border router")
        );
        // Bluetooth switched on too: the radio is one link's, Thread says why.
        mcu.iot.ble = Some(BleConfig::default());
        let bands = overview_bands(&Facts::of(&mcu));
        let thread = item(&bands, Layer::Link, "Thread");
        assert!(
            thread
                .blocked
                .as_deref()
                .is_some_and(|w| w.contains("one at a time")),
            "{thread:?}"
        );
    }

    #[test]
    fn the_mqtt_chain_has_tls_and_sntp_only_when_generated() {
        let mut mcu = c3_everything();
        let f = Facts::of(&mcu);
        let chain = mqtt_chain(&f).expect("MQTT generated");
        let texts: Vec<&str> = chain.steps.iter().map(|s| s.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.starts_with("SNTP's first time")),
            "{texts:?}"
        );
        let tls = chain
            .steps
            .iter()
            .find(|s| s.owner == Some(iot_gen::TLS))
            .expect("a TLS step");
        assert!(tls.text.contains("board's sent"), "{tls:?}");
        assert_eq!(tls.note, format!("up to {} s", f.numbers.tls_open_s));
        assert!(texts.contains(&"SUBSCRIBE x2"), "{texts:?}");
        assert!(texts.contains(&"DNS: broker.example.org"), "{texts:?}");
        let (to, label) = chain.back.clone().expect("the error path");
        assert_eq!(to, 1, "back to the address");
        assert!(label.contains(&format!("{} s", f.numbers.mqtt_retry_s)));

        // Plain MQTT, no SNTP: neither step.
        if let Some(m) = mcu.iot.mqtt.as_mut() {
            m.tls = false;
        }
        mcu.iot.sntp = None;
        let chain = mqtt_chain(&Facts::of(&mcu)).unwrap();
        assert!(chain.steps.iter().all(|s| s.owner != Some(iot_gen::TLS)));
        assert!(!chain.steps.iter().any(|s| s.text.starts_with("SNTP")));
        // Not generated: no chain.
        mcu.runtime = Runtime::Blocking;
        assert!(mqtt_chain(&Facts::of(&mcu)).is_none());
    }

    /// Found by the session that added it: with the Device Shadow on, mqtt.rs
    /// subscribes its answer topics - 7 a shadow - before the tab's filters,
    /// and the chain counted only the tab's.
    #[test]
    fn the_subscribe_count_includes_the_device_shadows_topics() {
        let mut mcu = c3_everything();
        if let Some(m) = mcu.iot.mqtt.as_mut() {
            m.host = "a1b2c3d4e5f6g7-ats.iot.eu-central-1.amazonaws.com".into();
            m.aws_shadow = true;
            m.shadow_names = vec!["config".into()];
        }
        let m = mcu.iot.mqtt.clone().unwrap();
        let shadow = iot::mqtt_shadow(&m).expect("the shadow runs on AWS with the certificate");
        let topics = iot::shadow_filters(&shadow.thing, &shadow.names).len();
        assert_eq!(topics, 14, "the classic shadow and one named, 7 each");
        let own = iot::subscriptions(&m).len();
        let f = Facts::of(&mcu);
        let texts =
            |chain: &Chain| -> Vec<String> { chain.steps.iter().map(|s| s.text.clone()).collect() };
        let chain = mqtt_chain(&f).unwrap();
        let shadow_step = "SUBSCRIBE x14: the Device Shadow's answer topics".to_owned();
        let own_step = format!("SUBSCRIBE x{own}: your filters");
        let ready = "the Device Shadow is ready".to_owned();
        let at = |t: &str| texts(&chain).iter().position(|s| s == t);
        // The shadow's topics, then yours, then the shadow ready - in that
        // order, as mqtt.rs runs them.
        assert!(
            at(&shadow_step) < at(&own_step) && at(&own_step) < at(&ready) && at(&ready).is_some(),
            "{:?}",
            chain.steps
        );
        let settle = chain.steps.iter().find(|s| s.text == ready).unwrap();
        assert_eq!(
            settle.note,
            format!("{} s after its last SUBACK", f.numbers.mqtt_settle_s)
        );
        // A filter of the tab's that IS one of the shadow's: the firmware
        // subscribes it once, and so does the count.
        if let Some(m) = mcu.iot.mqtt.as_mut() {
            m.subscribe
                .push(format!("$aws/things/{}/shadow/update/delta", shadow.thing));
        }
        let chain = mqtt_chain(&Facts::of(&mcu)).unwrap();
        assert!(
            texts(&chain).contains(&shadow_step) && texts(&chain).contains(&own_step),
            "still x14 and x{own}: {:?}",
            chain.steps
        );
    }

    #[test]
    fn the_http_chain_says_one_at_a_time_and_its_limits() {
        let f = Facts::of(&c3_everything());
        let chain = http_chain(&f).expect("HTTP generated");
        assert!(
            chain.steps[0]
                .note
                .contains(&format!("{} wait", f.numbers.http_queue)),
            "{chain:?}"
        );
        assert!(chain.steps.iter().any(|s| s.owner == Some(iot_gen::TLS)));
        assert!(chain.back.is_none());
        assert!(chain.foot.contains("30 s") && chain.foot.contains("no retry"));
    }

    #[test]
    fn the_queue_paths_quote_the_templates() {
        let n = iot_gen::template_numbers();
        for p in [mqtt_path(n), espnow_path(n), ble_path(n), thread_path(n)] {
            assert!(p.cap > 0, "{p:?}");
            assert!(
                p.notes
                    .iter()
                    .any(|t| t.contains(&p.cap.to_string()) || t.contains("emptied")),
                "{p:?}"
            );
        }
        assert!(mqtt_path(n).notes[1].contains(&n.mqtt_max_payload.to_string()));
    }

    /// The painters as egui runs them, narrow and wide: every text inside the
    /// width it was given, the rows' names there to read.
    #[test]
    fn the_diagrams_paint_inside_their_width() {
        fn texts(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| texts(s, out)),
                egui::Shape::Text(t) => {
                    out.push((t.galley.text().to_owned(), s.visual_bounding_rect()))
                }
                _ => {}
            }
        }
        let f = Facts::of(&c3_everything());
        let bands = overview_bands(&f);
        let chain = mqtt_chain(&f).unwrap();
        let http = http_chain(&f).unwrap();
        let path = mqtt_path(f.numbers);
        for width in [360.0, 1600.0] {
            let ctx = egui::Context::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 2000.0),
                )),
                ..Default::default()
            };
            let out = crate::headless::run_ui(&ctx, input, |ui| {
                paint_bands(ui, &bands);
                paint_chain(ui, &chain);
                paint_chain(ui, &http);
                paint_path(ui, &path);
            });
            let mut found = Vec::new();
            for cs in &out.shapes {
                texts(&cs.shape, &mut found);
            }
            for (text, r) in &found {
                assert!(
                    r.left() >= -0.5 && r.right() <= width + 0.5,
                    "{width}: {text:?} at {r:?}"
                );
            }
            for want in ["YOUR CODE", "LINK", "join the access point", "incoming()"] {
                assert!(
                    found.iter().any(|(t, _)| t.contains(want)),
                    "{width}: no {want:?}"
                );
            }
        }
    }
}
