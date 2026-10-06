//! The **IoT** tab's model: which links a chip can carry, and the settings of
//! the ones switched on. Pure - no egui, no codegen.
//!
//! # Layers, not a list
//!
//! MQTT does not run on a chip; it runs on TCP, which runs on an IP stack,
//! which runs on a link. The tab is three cards in that order - Link, Network,
//! Application - and a card is live only while the one below it is. A flat
//! list of protocols would let MQTT be ticked on an nRF52 with no Wi-Fi at
//! all, and the project would have nothing to generate.
//!
//! # What reaches the code today
//!
//! Wi-Fi station + embassy-net + MQTT + SNTP, on the ESP chips with Wi-Fi and
//! on the Pico W / Pico 2 W, and ESP-NOW on the ESP chips with Wi-Fi - with or
//! without the station. Every other link is listed with the reason it is not
//! generated - [`availability`] is the single place that says so, and the
//! versions behind each reason were read off crates.io, not guessed:
//!
//! - esp-radio 0.18 is the release on `esp-hal ~1.1`, the line this IDE pins.
//! - cyw43 0.7 and esp-radio 0.18 speak `bt-hci` 0.8; nrf-sdc 0.4 speaks 0.10.
//!   One trouble-host version cannot serve both, which is why BLE is a phase
//!   of its own.
//! - openthread 0.4 needs esp-radio 1.0 on an ESP - a newer esp-hal than this
//!   IDE's - but builds against embassy-nrf 0.11, which it already uses.

use serde::{Deserialize, Serialize};

/// The ESP heap the Wi-Fi driver allocates from, in KiB, when nobody chose.
/// esp-radio's own examples use 72 KiB; less starves its RX buffers.
pub const DEFAULT_HEAP_KIB: u32 = 72;

/// The tab's settings. The SSID and the passwords are NOT here: they live in
/// `src/pins/configs/secrets.rs`, which the project's `.gitignore` lists, so
/// `mcu.config` - a committed file - never carries them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct IotConfig {
    /// Wi-Fi station on.
    pub wifi: bool,
    pub ip: IpConfig,
    /// The MQTT client, `Some` while it is switched on.
    pub mqtt: Option<MqttConfig>,
    /// ESP only: the heap `esp_alloc::heap_allocator!` reserves.
    pub heap_kib: u32,
    /// ESP-NOW, `Some` while it is switched on. A link of its own: it runs
    /// beside the station or without it.
    pub esp_now: Option<EspNowConfig>,
    /// Network time, `Some` while it is switched on. Needs the station.
    pub sntp: Option<SntpConfig>,
}

impl Default for IotConfig {
    fn default() -> Self {
        Self {
            wifi: false,
            ip: IpConfig::default(),
            mqtt: None,
            heap_kib: DEFAULT_HEAP_KIB,
            esp_now: None,
            sntp: None,
        }
    }
}

/// ESP-NOW: the channel it uses while the station is off, and the peers it
/// may send to by address (broadcast needs no peer).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct EspNowConfig {
    /// 1..=13. With the station on, ESP-NOW is on the access point's channel
    /// instead - the radio has one.
    pub channel: u8,
    pub peers: Vec<[u8; 6]>,
}

impl Default for EspNowConfig {
    fn default() -> Self {
        Self {
            channel: 1,
            peers: Vec::new(),
        }
    }
}

/// The shortest resync interval the generator writes. pool.ntp.org's terms
/// ask clients not to poll more often than this; the tab warns below it.
pub const MIN_SNTP_INTERVAL_S: u32 = 64;

/// The peers the generated `start()` can register: esp-radio's list holds 20,
/// and the broadcast address it adds itself is one of them.
pub const MAX_ESPNOW_PEERS: usize = 19;

/// The SNTP server as written: an IPv4 literal or a DNS name; the port is
/// NTP's own. `None` = fine.
pub fn ntp_server_problem(host: &str) -> Option<&'static str> {
    let host = host.trim();
    if host.is_empty() {
        return Some("no server");
    }
    if host.contains("://") {
        return Some("a host name or an IPv4 address, without a scheme");
    }
    if host.contains(':') {
        return Some("a host alone - SNTP is always port 123");
    }
    None
}

/// SNTP: which server, and how often the clock is set again.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct SntpConfig {
    pub server: String,
    pub interval_s: u32,
}

impl Default for SntpConfig {
    fn default() -> Self {
        Self {
            server: "pool.ntp.org".to_owned(),
            interval_s: 3600,
        }
    }
}

/// `AA:BB:CC:DD:EE:FF` (or `-`-separated) as six bytes; `None` when it is not
/// exactly that.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = text.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (b, p) in mac.iter_mut().zip(parts) {
        if p.len() != 2 {
            return None;
        }
        *b = u8::from_str_radix(p, 16).ok()?;
    }
    Some(mac)
}

/// Six bytes as `AA:BB:CC:DD:EE:FF`.
pub fn fmt_mac(mac: &[u8; 6]) -> String {
    mac.iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// What is wrong with a peer address, for the line under it. `None` = fine.
pub fn peer_problem(mac: &[u8; 6]) -> Option<&'static str> {
    if *mac == [0xFF; 6] {
        return Some("that is the broadcast address - it needs no peer");
    }
    if mac[0] & 1 == 1 {
        return Some("a multicast address cannot be a peer");
    }
    None
}

impl IotConfig {
    /// Nothing switched on and nothing changed: no `@iot` section is written.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// How the stack gets its address. The static fields are kept while DHCP is
/// on, so switching back does not lose them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct IpConfig {
    pub dhcp: bool,
    pub address: [u8; 4],
    pub prefix: u8,
    pub gateway: [u8; 4],
    pub dns: [u8; 4],
}

impl Default for IpConfig {
    fn default() -> Self {
        Self {
            dhcp: true,
            address: [192, 168, 1, 50],
            prefix: 24,
            gateway: [192, 168, 1, 1],
            dns: [1, 1, 1, 1],
        }
    }
}

/// The MQTT client. The broker's user name and password are in `secrets.rs`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub keep_alive_s: u16,
    /// Subscribed after every connect.
    pub subscribe: Vec<String>,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "test.mosquitto.org".to_owned(),
            port: 1883,
            client_id: "rustonchip".to_owned(),
            keep_alive_s: 60,
            subscribe: Vec::new(),
        }
    }
}

impl MqttConfig {
    /// A fresh client, named after the chip so two boards on one broker do not
    /// throw each other off (a broker drops the older of two equal ids).
    pub fn for_chip(chip_id: &str) -> Self {
        Self {
            client_id: format!("rustonchip-{chip_id}"),
            ..Self::default()
        }
    }
}

/// The longest topic and payload a generated `Message` carries. Shared with
/// the template, so the tab can refuse a subscription the code would drop.
pub const MAX_TOPIC: usize = 64;

/// What is wrong with an MQTT setting, for the line under the field. `None`
/// = fine. These are the checks `rust-mqtt` would otherwise fail AT RUN TIME:
/// an empty or wildcard topic name, a filter whose `#` is not last.
pub fn topic_filter_problem(filter: &str) -> Option<&'static str> {
    if filter.is_empty() {
        return Some("empty topic");
    }
    if filter.len() > MAX_TOPIC {
        return Some("longer than 64 bytes - a message on it would not fit `Message`");
    }
    let levels: Vec<&str> = filter.split('/').collect();
    for (i, level) in levels.iter().enumerate() {
        if level.contains('#') && (*level != "#" || i + 1 != levels.len()) {
            return Some("`#` must be a whole level, and the last one");
        }
        if level.contains('+') && *level != "+" {
            return Some("`+` must be a whole level");
        }
    }
    None
}

/// The broker host as written: an IPv4 literal or a DNS name. `None` = fine.
pub fn host_problem(host: &str) -> Option<&'static str> {
    let host = host.trim();
    if host.is_empty() {
        return Some("no broker host");
    }
    if host.contains("://") {
        return Some("a host name or an IPv4 address, without `mqtt://`");
    }
    if host.contains(':') {
        return Some("the port goes in its own field");
    }
    None
}

// ── What the chip can carry ──────────────────────────────────────────────────

/// Which radio driver a Wi-Fi project is generated for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    /// esp-radio on the chip itself.
    Esp,
    /// The CYW43 radio beside the RP2040 / RP2350 on a Pico W / Pico 2 W.
    Cyw43,
}

/// The ESP parts with a Wi-Fi radio. The H2 has only 802.15.4 + BLE, the P4
/// no radio at all.
const ESP_WIFI: [&str; 8] = [
    "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61",
];

/// The Wi-Fi platform of a chip, or `None` when it has no Wi-Fi this IDE
/// generates for. `cyw43` = the board carries the radio (its `WL_LED` pad).
pub fn platform(family: &str, cyw43: bool) -> Option<Platform> {
    if ESP_WIFI.contains(&family) {
        Some(Platform::Esp)
    } else if cyw43 && matches!(family, "rp2040" | "rp235x") {
        Some(Platform::Cyw43)
    } else {
        None
    }
}

/// Does this board carry the CYW43 radio? The `WL_LED` pad is the radio's
/// GPIO0, and only a W board has it - the same fact the codegen keys on.
pub fn has_cyw43(mcu: &crate::panels::mcu_module::mcu::Mcu) -> bool {
    mcu.iter_all_pins().any(|p| p.name == "WL_LED")
}

/// The Wi-Fi platform of `mcu`.
pub fn platform_of(mcu: &crate::panels::mcu_module::mcu::Mcu) -> Option<Platform> {
    platform(&mcu.family, has_cyw43(mcu))
}

/// What the generator emits for `mcu` right now - the ONE decision the
/// generator, the dependency sync and the harness all ask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Active {
    pub platform: Platform,
    /// The Wi-Fi station, and with it the IP stack.
    pub station: bool,
    /// ESP-NOW - only ever on an ESP.
    pub esp_now: bool,
    /// MQTT over the stack - only with the station.
    pub mqtt: bool,
    /// SNTP over the stack - only with the station.
    pub sntp: bool,
}

/// `Some` when a link is on, the chip can carry it, and the runtime is Async:
/// every driver below is async-only, so a Blocking project gets nothing (the
/// tab says why) rather than code that cannot compile. What needs the IP
/// stack (MQTT, SNTP) is off without the station, whatever the tab holds.
pub fn active(mcu: &crate::panels::mcu_module::mcu::Mcu) -> Option<Active> {
    use crate::panels::mcu_module::mcu::model::Runtime;
    if !matches!(mcu.runtime, Runtime::Async) {
        return None;
    }
    let platform = platform_of(mcu)?;
    let station = mcu.iot.wifi;
    let esp_now = mcu.iot.esp_now.is_some() && platform == Platform::Esp;
    if !station && !esp_now {
        return None;
    }
    Some(Active {
        platform,
        station,
        esp_now,
        mqtt: station && mcu.iot.mqtt.is_some(),
        sntp: station && mcu.iot.sntp.is_some(),
    })
}

/// A link the tab lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    Wifi,
    EspNow,
    Ble,
    Thread,
    BleMesh,
    Zigbee,
    EspWifiMesh,
}

impl Link {
    pub const ALL: [Link; 7] = [
        Link::Wifi,
        Link::EspNow,
        Link::Ble,
        Link::Thread,
        Link::BleMesh,
        Link::Zigbee,
        Link::EspWifiMesh,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Link::Wifi => "Wi-Fi station",
            Link::EspNow => "ESP-NOW",
            Link::Ble => "Bluetooth LE",
            Link::Thread => "Thread (mesh)",
            Link::BleMesh => "Bluetooth Mesh",
            Link::Zigbee => "Zigbee (mesh)",
            Link::EspWifiMesh => "ESP-WIFI-MESH",
        }
    }

    /// One line on what it is, for the hover.
    pub fn blurb(self) -> &'static str {
        match self {
            Link::Wifi => "Joins an access point; IP over it through embassy-net.",
            Link::EspNow => {
                "Espressif's connectionless frames between ESP chips, up to 250 bytes, no access point. A mesh is built on top by relaying."
            }
            Link::Ble => "A GATT peripheral a phone can connect to.",
            Link::Thread => {
                "IPv6 mesh over 802.15.4, the network under Matter. Nodes route for each other."
            }
            Link::BleMesh => "Flooding mesh over Bluetooth LE advertisements.",
            Link::Zigbee => "Mesh over 802.15.4 with its own application layer.",
            Link::EspWifiMesh => "Espressif's tree of Wi-Fi nodes relaying to one root.",
        }
    }
}

/// Whether a link reaches the generated code on a chip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Availability {
    /// Generated today.
    Ready,
    /// The chip can carry it and a later phase will generate it - why it is
    /// not yet.
    Planned(&'static str),
    /// Not on this chip, or no Rust stack for it at all - why.
    NotHere(&'static str),
}

/// Whether `link` reaches the code on `family`, and if not, why.
pub fn availability(link: Link, family: &str, cyw43: bool) -> Availability {
    use Availability::{NotHere, Planned, Ready};
    let esp = family.starts_with("esp32");
    let esp_wifi = ESP_WIFI.contains(&family);
    let nrf = family.starts_with("nrf");
    let rp = matches!(family, "rp2040" | "rp235x");
    match link {
        Link::Wifi if platform(family, cyw43).is_some() => Ready,
        Link::Wifi if family == "esp32h2" => NotHere("the ESP32-H2 has 802.15.4 and BLE, no Wi-Fi"),
        Link::Wifi if rp => NotHere("no radio on this board - the Pico W and Pico 2 W carry one"),
        Link::Wifi if nrf => NotHere("no Wi-Fi radio on an nRF"),
        Link::Wifi if family.starts_with("stm32") => NotHere(
            "no Wi-Fi radio on an STM32; an external module (ESP-AT, a W5500) is not generated yet",
        ),
        Link::Wifi => NotHere("no Wi-Fi radio on this chip"),

        Link::EspNow if esp_wifi => Ready,
        Link::EspNow => NotHere("Espressif's own protocol, on the ESP chips with Wi-Fi"),

        Link::Ble if family == "esp32s2" => NotHere("the ESP32-S2 has no Bluetooth radio"),
        Link::Ble if esp => Planned("later phase: esp-radio BLE + trouble-host 0.6 (bt-hci 0.8)"),
        Link::Ble if rp && cyw43 => {
            Planned("later phase: cyw43 Bluetooth + trouble-host 0.6, and a fourth firmware blob")
        }
        Link::Ble if family == "nrf5340" => NotHere(
            "the SoftDevice Controller runs on the network core, which this IDE does not generate",
        ),
        Link::Ble if nrf => Planned("later phase: nrf-sdc 0.4 + trouble-host 0.8 (bt-hci 0.10)"),
        Link::Ble if family.starts_with("stm32") => {
            NotHere("embassy-stm32-wpan is not published on crates.io")
        }
        Link::Ble => NotHere("no Bluetooth radio on this chip"),

        Link::Thread if matches!(family, "nrf52840" | "nrf52833") => Planned(
            "research first: openthread 0.4 builds against embassy-nrf 0.11, but compiles OpenThread's C sources",
        ),
        Link::Thread if matches!(family, "esp32c5" | "esp32c6" | "esp32h2") => NotHere(
            "openthread 0.4 needs esp-radio 1.0 - a newer esp-hal than the ~1.1 this IDE pins",
        ),
        Link::Thread if family == "nrf5340" => NotHere(
            "the 802.15.4 radio is on the network core, which this IDE does not generate",
        ),
        Link::Thread if matches!(family, "nrf52811" | "nrf52820" | "nrf54l15") => NotHere(
            "the chip has an 802.15.4 radio, but no Thread stack is generated for this part",
        ),
        Link::Thread => NotHere("needs an 802.15.4 radio"),

        Link::BleMesh => NotHere("no maintained no_std Rust stack"),
        Link::Zigbee => NotHere("no Rust Zigbee stack"),
        Link::EspWifiMesh if esp_wifi => NotHere(
            "ESP-IDF only (esp-idf-svc, std); projects here are no_std esp-hal - ESP-NOW is the mesh-capable link",
        ),
        Link::EspWifiMesh => NotHere("Espressif's own, on the ESP chips with Wi-Fi"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every ESP with a Wi-Fi radio is Ready; the H2 says why it is not.
    #[test]
    fn wifi_follows_the_radio() {
        for chip in ESP_WIFI {
            assert_eq!(availability(Link::Wifi, chip, false), Availability::Ready, "{chip}");
        }
        assert!(matches!(availability(Link::Wifi, "esp32h2", false), Availability::NotHere(_)));
        assert!(matches!(availability(Link::Wifi, "nrf52840", false), Availability::NotHere(_)));
        assert!(matches!(availability(Link::Wifi, "stm32f1", false), Availability::NotHere(_)));
    }

    /// The RP family carries Wi-Fi only on a W board: the radio is beside the
    /// chip, not in it.
    #[test]
    fn a_pico_has_wifi_only_with_the_radio() {
        assert_eq!(platform("rp2040", true), Some(Platform::Cyw43));
        assert_eq!(platform("rp235x", true), Some(Platform::Cyw43));
        assert_eq!(platform("rp2040", false), None);
        assert!(matches!(availability(Link::Wifi, "rp235x", false), Availability::NotHere(_)));
    }

    /// A WL_LED pad on something that is not an RP is not a CYW43.
    #[test]
    fn the_radio_pad_means_nothing_off_the_rp() {
        assert_eq!(platform("nrf52840", true), None);
    }

    /// Nothing is Ready that the generator does not emit: Wi-Fi where there
    /// is a radio, ESP-NOW on the ESP chips with Wi-Fi, nothing else.
    #[test]
    fn only_wifi_and_esp_now_are_ready() {
        for family in ["esp32", "esp32s2", "esp32c6", "esp32h2", "rp2040", "nrf52840", "nrf5340", "stm32f1"] {
            for link in Link::ALL {
                let a = availability(link, family, family == "rp2040");
                match link {
                    Link::Wifi => {}
                    Link::EspNow if ESP_WIFI.contains(&family) => {
                        assert_eq!(a, Availability::Ready, "{family}")
                    }
                    _ => assert_ne!(a, Availability::Ready, "{family} {link:?}"),
                }
            }
        }
    }

    #[test]
    fn mac_addresses_parse_and_print() {
        let mac = parse_mac("24:6f:28:AA:bb:01").expect("valid");
        assert_eq!(mac, [0x24, 0x6F, 0x28, 0xAA, 0xBB, 0x01]);
        assert_eq!(fmt_mac(&mac), "24:6F:28:AA:BB:01");
        assert_eq!(parse_mac("24-6F-28-AA-BB-01"), Some(mac));
        for bad in ["24:6F:28:AA:BB", "24:6F:28:AA:BB:0G", "246F28AABB01", "24:6F:28:AA:BB:001"] {
            assert_eq!(parse_mac(bad), None, "{bad}");
        }
        assert!(peer_problem(&[0xFF; 6]).is_some());
        assert!(peer_problem(&[0x01, 0, 0x5E, 0, 0, 1]).is_some());
        assert!(peer_problem(&mac).is_none());
    }

    /// An `@iot` line written by phase 1 - no `esp_now`, no `sntp` - still
    /// reads, with both off.
    #[test]
    fn a_phase_one_config_still_reads() {
        let old = "(wifi:true,ip:(dhcp:true,address:(192,168,1,50),prefix:24,gateway:(192,168,1,1),dns:(1,1,1,1)),mqtt:None,heap_kib:72)";
        let cfg: IotConfig = ron::from_str(old).expect("phase-1 line");
        assert!(cfg.wifi);
        assert_eq!(cfg.esp_now, None);
        assert_eq!(cfg.sntp, None);
    }

    /// The mesh options are answered, never silently absent - that was the
    /// question the tab was asked for.
    #[test]
    fn every_mesh_link_has_a_reason() {
        for link in [Link::BleMesh, Link::Zigbee, Link::EspWifiMesh, Link::Thread] {
            for family in ["esp32c6", "nrf52840", "rp2040"] {
                match availability(link, family, false) {
                    Availability::Ready => panic!("{link:?} on {family} is not generated"),
                    Availability::Planned(why) | Availability::NotHere(why) => {
                        assert!(!why.is_empty())
                    }
                }
            }
        }
    }

    #[test]
    fn topic_filters_follow_the_mqtt_rules() {
        assert_eq!(topic_filter_problem("home/+/temp"), None);
        assert_eq!(topic_filter_problem("home/#"), None);
        assert!(topic_filter_problem("home/#/temp").is_some());
        assert!(topic_filter_problem("home/te#").is_some());
        assert!(topic_filter_problem("home/t+").is_some());
        assert!(topic_filter_problem("").is_some());
        assert!(topic_filter_problem(&"a".repeat(65)).is_some());
    }

    #[test]
    fn an_ntp_server_is_a_bare_host() {
        assert_eq!(ntp_server_problem("pool.ntp.org"), None);
        assert_eq!(ntp_server_problem("192.168.1.1"), None);
        assert!(ntp_server_problem("ntp://pool.ntp.org").is_some());
        assert!(ntp_server_problem("pool.ntp.org:123").is_some());
        assert!(ntp_server_problem("").is_some());
        // Not the broker's wording: there is no port field here.
        assert!(!ntp_server_problem("a:1").unwrap().contains("field"));
    }

    #[test]
    fn a_host_is_a_name_or_an_address() {
        assert_eq!(host_problem("test.mosquitto.org"), None);
        assert_eq!(host_problem("192.168.1.10"), None);
        assert!(host_problem("mqtt://broker").is_some());
        assert!(host_problem("broker:1883").is_some());
        assert!(host_problem(" ").is_some());
    }

    /// The default writes no section, and a touched one does.
    #[test]
    fn only_a_changed_config_is_persisted() {
        assert!(IotConfig::default().is_default());
        let on = IotConfig {
            wifi: true,
            ..IotConfig::default()
        };
        assert!(!on.is_default());
    }
}
