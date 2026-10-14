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
//! - openthread 0.4 builds against embassy-nrf 0.11. Its openthread-sys ships
//!   OpenThread compiled for `thumbv7em-none-eabi` with the default (`matter`)
//!   features, and for no hard-float target: a Thread project on the nRF52840
//!   / nRF52833 builds for that soft-float target and needs no C compiler,
//!   CMake or libclang. On the IDE's usual `-eabihf` it would compile
//!   OpenThread's C and C++ on every clean build.
//! - On an ESP, openthread 0.3+ needs esp-radio 1.0 beta; 0.2 is the release
//!   on esp-radio 0.18. Its openthread-sys links OpenThread prebuilt for
//!   riscv32imac (the C6, H2, C5) - and mbedtls-rs-sys links Mbed TLS
//!   prebuilt only with its default `tls` profile, which is why a Thread
//!   project on an ESP names mbedtls-rs-sys itself.

use serde::{Deserialize, Serialize};

/// The ESP heap the Wi-Fi driver allocates from, in KiB, when nobody chose.
/// esp-radio's own examples use 72 KiB; less starves its RX buffers.
pub const DEFAULT_HEAP_KIB: u32 = 72;

/// The ESP radio's transmit power cap when nobody chose, in quarter dBm as
/// esp-radio takes it (`set_max_tx_power`): 13 dBm. esp-radio 0.18 leaves the
/// PHY at 20 dBm, and at that cheap ESP32-C3 boards (a badly matched chip
/// antenna) never join: the access point cannot decode their Authentication,
/// the driver reports `AuthenticationExpired`, then `ConnectionFailed` (it
/// blacklisted the AP for one try), and so on. Measured 2026-10-14 on a C3
/// board 1 m from a TP-Link: 0 joins at 20 dBm, every one from 18 dBm down;
/// esp-hal #5631's C3 failed from 16.5 dBm up. Two steps below that.
pub const DEFAULT_WIFI_TX_POWER: u8 = 52;

/// The caps the driver keeps, each from its value up to the next one's
/// (ESP-IDF's `esp_wifi_set_max_tx_power`), in quarter dBm.
pub const WIFI_TX_POWER_STEPS: [(u8, &str); 11] = [
    (8, "2 dBm"),
    (20, "5 dBm"),
    (28, "7 dBm"),
    (34, "8.5 dBm"),
    (44, "11 dBm"),
    (52, "13 dBm"),
    (56, "14 dBm"),
    (60, "15 dBm"),
    (66, "16.5 dBm"),
    (72, "18 dBm"),
    (80, "20 dBm"),
];

/// `q` (quarter dBm) as the step the driver keeps, and its label.
pub fn wifi_tx_power_step(q: u8) -> (u8, &'static str) {
    WIFI_TX_POWER_STEPS
        .iter()
        .rev()
        .find(|(at, _)| q >= *at)
        .copied()
        .unwrap_or(WIFI_TX_POWER_STEPS[0])
}

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
    /// ESP only: the Wi-Fi radio's transmit power cap, in quarter dBm
    /// ([`WIFI_TX_POWER_STEPS`]). An `@iot` line from before the setting
    /// reads the default too: the radio's own 20 dBm is what kept cheap C3
    /// boards off the network.
    pub wifi_tx_power: u8,
    /// ESP-NOW, `Some` while it is switched on. A link of its own: it runs
    /// beside the station or without it.
    pub esp_now: Option<EspNowConfig>,
    /// Network time, `Some` while it is switched on. Needs the station.
    pub sntp: Option<SntpConfig>,
    /// Bluetooth LE (a Nordic UART Service peripheral), `Some` while it is
    /// switched on. A link of its own, beside Wi-Fi or without it.
    pub ble: Option<BleConfig>,
    /// Thread (an OpenThread end device with UDP), `Some` while it is switched
    /// on. nRF52840 / nRF52833 and ESP32-C6 / H2 / C5, and never beside
    /// another link on the same radio - Bluetooth, and on an ESP Wi-Fi and
    /// ESP-NOW too. The network's dataset is a secret, in `secrets.rs`.
    pub thread: Option<ThreadConfig>,
    /// A panic restarts the chip (`src/pins/configs/panic.rs`) instead of
    /// halting it. On for a new setup; an `@iot` line written before the
    /// switch existed reads `false` (the field's own default), so a project
    /// already in use keeps halting - its main.rs header is the user's, and
    /// the IDE edits it only once the box is ticked.
    #[serde(default)]
    pub restart_on_panic: bool,
    /// The HTTP client (`src/pins/configs/http.rs`), `Some` while it is
    /// switched on. Needs the station.
    #[serde(default)]
    pub http: Option<HttpConfig>,
}

impl Default for IotConfig {
    fn default() -> Self {
        Self {
            wifi: false,
            ip: IpConfig::default(),
            mqtt: None,
            heap_kib: DEFAULT_HEAP_KIB,
            wifi_tx_power: DEFAULT_WIFI_TX_POWER,
            esp_now: None,
            sntp: None,
            ble: None,
            thread: None,
            restart_on_panic: true,
            http: None,
        }
    }
}

/// The HTTP client. The URLs are the firmware's own, given at each call;
/// these are what every request shares.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct HttpConfig {
    /// https:// URLs: TLS 1.3 (tls.rs), checked against http_ca.rs's CAs.
    /// Off, an https:// URL is refused - never sent in clear.
    pub tls: bool,
    /// Check the server's certificate. Off = encrypted, but anyone on the
    /// path can pose as the server.
    pub verify: bool,
    /// Accept chains signed with P-384 (embedded-tls's `p384`): Discord,
    /// ECDSA Let's Encrypt certificates.
    pub p384: bool,
    /// The largest response body a request keeps, in bytes.
    pub body_max: u32,
    /// The largest request body, in bytes.
    pub payload_max: u32,
    /// Seconds a whole request may take.
    pub timeout_s: u16,
    /// The User-Agent header; some APIs (Discord) refuse a request without
    /// one.
    pub user_agent: String,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            tls: true,
            verify: true,
            p384: false,
            body_max: 2048,
            payload_max: 1024,
            timeout_s: 30,
            user_agent: "RustOnChip".to_owned(),
        }
    }
}

/// The User-Agent as the request carries it: visible ASCII and spaces only (a
/// line break would end the header), 64 bytes at most, "RustOnChip" when
/// nothing is left.
pub fn http_user_agent(text: &str) -> String {
    let mut ua: String = text
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .collect::<String>()
        .trim()
        .to_owned();
    while ua.len() > 64 {
        ua.pop();
    }
    if ua.is_empty() {
        ua = "RustOnChip".to_owned();
    }
    ua
}

/// The longest response head (status line and headers) the board takes -
/// http_core.rs's HEAD_MAX, which a test holds this to.
pub const HTTP_HEAD_MAX: usize = 2048;

/// The longest request head (request line and headers) the board builds -
/// http_core.rs's REQUEST_HEAD_MAX: an Authorization with a JWT fits.
pub const HTTP_REQUEST_HEAD_MAX: usize = 1024;

/// The body limits the tab offers, in bytes: below 64 nothing useful fits,
/// above 16 KiB the RAM of a small chip runs out.
pub const HTTP_BODY_RANGE: core::ops::RangeInclusive<u32> = 64..=16_384;

/// Can HTTP run over TLS on `family`? As MQTT ([`tls_available`]), and one
/// more: beside MQTT over TLS a second TLS session takes about 40 KB of RAM
/// more (measured), which the ESP32-C2 has not. `Err` says why not.
pub fn https_available(family: &str, coex: bool, mqtt_tls: bool) -> Result<(), &'static str> {
    tls_available(family, coex)?;
    if mqtt_tls && family == "esp32c2" {
        return Err("beside MQTT over TLS the ESP32-C2 has no RAM for a second TLS session (about 40 KB)");
    }
    Ok(())
}

/// A second TLS session beside MQTT's leaves these chips about 15 KB of
/// stack (measured: it links, but was never run on a board).
pub fn https_tight(family: &str, mqtt_tls: bool) -> bool {
    mqtt_tls && matches!(family, "esp32" | "esp32s2")
}

/// Thread: the UDP port the device listens on. Which network it joins is the
/// Active Operational Dataset in `secrets.rs` (`THREAD_DATASET`), since it
/// carries the network key.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadConfig {
    pub udp_port: u16,
}

impl Default for ThreadConfig {
    fn default() -> Self {
        Self {
            udp_port: DEFAULT_THREAD_PORT,
        }
    }
}

/// The UDP port when nobody chose one: openthread's own examples use it.
pub const DEFAULT_THREAD_PORT: u16 = 1212;

/// The longest Active Operational Dataset, in bytes (OpenThread's
/// `OT_OPERATIONAL_DATASET_MAX_LENGTH`): 508 hex characters.
pub const MAX_DATASET_BYTES: usize = 254;

/// What a Thread dataset says, read off its TLVs - shown under the secret so
/// the user can see which network the hex is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatasetSummary {
    pub network_name: Option<String>,
    pub channel: Option<u16>,
    pub pan_id: Option<u16>,
    pub ext_pan_id: Option<[u8; 8]>,
    pub mesh_local_prefix: Option<[u8; 8]>,
    pub has_network_key: bool,
}

/// The TLV types read here (`openthread/dataset.h`, `otMeshcopTlvType`).
const TLV_CHANNEL: u8 = 0;
const TLV_PAN_ID: u8 = 1;
const TLV_EXT_PAN_ID: u8 = 2;
const TLV_NETWORK_NAME: u8 = 3;
const TLV_NETWORK_KEY: u8 = 5;
const TLV_MESH_LOCAL_PREFIX: u8 = 7;

/// Decode an Active Operational Dataset given as hex TLVs (what a border
/// router's `ot-ctl dataset active -x` prints), with the checks OpenThread
/// makes before it takes one (`Dataset::ValidateTlvs`) and attaches with it
/// (`otDatasetIsCommissioned`). `Err` says what is wrong: not hex, an odd
/// length, too long, a TLV cut short, repeated or shorter than its type, a
/// channel off page 0 / 11-26, a name that is not 1-16 bytes of UTF-8, or one
/// of the five things attaching needs missing - network key, name, channel,
/// PAN ID, extended PAN ID. The generated `thread.rs` starts nothing then.
pub fn decode_dataset(hex: &str) -> Result<DatasetSummary, &'static str> {
    let hex = hex.trim();
    if hex.is_empty() {
        return Err("empty");
    }
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("not hex - paste what `ot-ctl dataset active -x` prints");
    }
    if !hex.len().is_multiple_of(2) {
        return Err("an odd number of hex digits");
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap_or(0))
        .collect();
    if bytes.len() > MAX_DATASET_BYTES {
        return Err("longer than 254 bytes");
    }
    let mut s = DatasetSummary::default();
    let mut seen = [false; 256];
    let mut i = 0;
    while i < bytes.len() {
        let Some(&len) = bytes.get(i + 1) else {
            return Err("a TLV is cut short");
        };
        let start = i + 2;
        let end = start + len as usize;
        let Some(v) = bytes.get(start..end) else {
            return Err("a TLV runs past the end");
        };
        let ty = bytes[i];
        if std::mem::replace(&mut seen[ty as usize], true) {
            return Err("a TLV appears twice");
        }
        // The shortest value each known type takes, as OpenThread checks it.
        let min = match ty {
            TLV_CHANNEL => 3,
            TLV_PAN_ID => 2,
            TLV_EXT_PAN_ID | TLV_MESH_LOCAL_PREFIX => 8,
            TLV_NETWORK_KEY => 16,
            TLV_NETWORK_NAME => 1,
            _ => 0,
        };
        if v.len() < min {
            return Err("a TLV is shorter than its type needs");
        }
        let eight = |v: &[u8]| <[u8; 8]>::try_from(&v[..8]).ok();
        match ty {
            // A page byte, then the channel: 2.4 GHz is page 0, 11 to 26.
            TLV_CHANNEL => {
                let channel = u16::from_be_bytes([v[1], v[2]]);
                if v[0] != 0 || !(11..=26).contains(&channel) {
                    return Err("a channel outside 11-26 (page 0)");
                }
                s.channel = Some(channel);
            }
            TLV_PAN_ID => s.pan_id = Some(u16::from_be_bytes([v[0], v[1]])),
            TLV_EXT_PAN_ID => s.ext_pan_id = eight(v),
            TLV_NETWORK_NAME => match std::str::from_utf8(v) {
                Ok(name) if name.len() <= 16 => s.network_name = Some(name.to_owned()),
                _ => return Err("a network name that is not 1-16 bytes of UTF-8"),
            },
            TLV_NETWORK_KEY => s.has_network_key = true,
            TLV_MESH_LOCAL_PREFIX => s.mesh_local_prefix = eight(v),
            _ => {}
        }
        i = end;
    }
    if !s.has_network_key {
        return Err("no network key in it");
    }
    if s.network_name.is_none() {
        return Err("no network name in it");
    }
    if s.channel.is_none() {
        return Err("no channel in it");
    }
    if s.pan_id.is_none() {
        return Err("no PAN ID in it");
    }
    if s.ext_pan_id.is_none() {
        return Err("no extended PAN ID in it");
    }
    Ok(s)
}

/// The mesh-local prefix as IPv6 (`fd7d:4f82:32cb:0::/64`).
pub fn fmt_mesh_local_prefix(p: &[u8; 8]) -> String {
    let groups: Vec<String> = p
        .chunks(2)
        .map(|g| format!("{:x}", u16::from_be_bytes([g[0], g[1]])))
        .collect();
    format!("{}::/64", groups.join(":"))
}

/// Bluetooth LE: the name the board advertises.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct BleConfig {
    pub device_name: String,
}

impl Default for BleConfig {
    fn default() -> Self {
        Self {
            device_name: DEFAULT_BLE_NAME.to_owned(),
        }
    }
}

/// The advertised name when nobody chose one, or the chosen one cannot be.
pub const DEFAULT_BLE_NAME: &str = "RustOnChip";

/// The longest name trouble-host's GAP service takes (`gap.rs`). A longer one
/// fails `GapConfig::build` - at boot, as a panic - so it never reaches the
/// generated code.
pub const MAX_BLE_NAME: usize = 22;

/// What is wrong with an advertised name. `None` = fine.
pub fn ble_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        return Some("no name - the generated code uses the default");
    }
    if name.len() > MAX_BLE_NAME {
        return Some("longer than 22 bytes - the generated code uses the default");
    }
    None
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
    /// MQTT over TLS 1.3 (embedded-tls): `mqtt_link.rs` wraps the socket.
    pub tls: bool,
    /// With TLS: check the broker's certificate against `mqtt_ca.rs` (and its
    /// name). Off = encrypted, but anyone on the path can pose as the broker.
    pub verify: bool,
    /// The name the broker's certificate must carry, and the SNI; empty =
    /// the host.
    pub server_name: String,
    /// With TLS: accept chains signed with P-384 - an ECDSA Let's Encrypt
    /// certificate's issuers are (embedded-tls's `p384`, 114 KiB of flash).
    pub p384: bool,
    /// The board presents a client certificate (secrets.rs: MQTT_CLIENT_CERT
    /// and MQTT_CLIENT_KEY, P-256) when the broker asks - AWS IoT does.
    #[serde(default)]
    pub client_cert: bool,
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            host: "test.mosquitto.org".to_owned(),
            port: PLAIN_PORT,
            client_id: "rustonchip".to_owned(),
            keep_alive_s: 60,
            subscribe: Vec::new(),
            tls: false,
            verify: true,
            server_name: String::new(),
            p384: false,
            client_cert: false,
        }
    }
}

/// MQTT's own ports: plain TCP, and over TLS.
pub const PLAIN_PORT: u16 = 1883;
pub const TLS_PORT: u16 = 8883;

/// test.mosquitto.org serves its Let's Encrypt chain - the ISRG Root X1 the
/// IDE writes into `mqtt_ca.rs` - on 8886; its 8883 is signed by its own CA.
const MOSQUITTO_TEST: &str = "test.mosquitto.org";
const MOSQUITTO_LE_PORT: u16 = 8886;

impl MqttConfig {
    /// The port TLS on (`true`) or off moves to, when the port is still the
    /// other mode's default - a port the user chose stays.
    pub fn port_for_tls(&self, tls: bool) -> u16 {
        let tls_default = if self.host.trim() == MOSQUITTO_TEST {
            MOSQUITTO_LE_PORT
        } else {
            TLS_PORT
        };
        match (tls, self.port) {
            (true, PLAIN_PORT) => tls_default,
            (false, p) if p == TLS_PORT || p == MOSQUITTO_LE_PORT => PLAIN_PORT,
            (_, p) => p,
        }
    }

    /// The name the certificate is checked against and sent as SNI.
    pub fn tls_name(&self) -> &str {
        let name = self.server_name.trim();
        if name.is_empty() {
            self.host.trim()
        } else {
            name
        }
    }
}

/// The key a CA certificate carries - which signatures it makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaKey {
    Rsa,
    EcP256,
    /// Checked only with "P-384 chains" (embedded-tls's `p384`).
    EcP384,
    /// P-521 and other curves: embedded-tls 0.19 checks none of them.
    EcOther,
    /// Checked only with embedded-tls's `ed25519`, which the IDE leaves off.
    Ed25519,
    Other,
}

/// What `mqtt_ca.rs`'s certificate says, for the line in the tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaSummary {
    /// The subject's common name, or its organisation.
    pub name: String,
    /// notAfter as `YYYY-MM-DD`.
    pub not_after: String,
    /// basicConstraints' cA flag; `None` when there is no such extension (an
    /// X.509 v1 root), which is still fine as the trust anchor.
    pub is_ca: Option<bool>,
    pub key: CaKey,
    /// More than one certificate pasted: only the first is used.
    /// More than one certificate: each is trusted.
    pub more: bool,
}

/// One DER element at the start of `d`: (tag, contents, what follows).
fn der(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, d) = d.split_first()?;
    let (&first, mut d) = d.split_first()?;
    let mut len = usize::from(first);
    if first >= 0x80 {
        let n = usize::from(first & 0x7F);
        if n == 0 || n > 3 || d.len() < n {
            return None;
        }
        len = d[..n].iter().fold(0, |l, &b| (l << 8) | usize::from(b));
        d = &d[n..];
    }
    (d.len() >= len).then(|| (tag, &d[..len], &d[len..]))
}

/// The board's PEM and DER code (`iot_templates/pem_der.rs`), word for word:
/// what the tab says of a key or a certificate is what the firmware builds.
#[allow(dead_code)]
pub mod pem {
    include!("codegen/iot_templates/pem_der.rs");
}

/// The helper's key and certificate-request layout (`iot_templates/csr_der.rs`):
/// the tab checks a subject with it, the tests the bytes.
#[allow(dead_code)]
pub mod csr {
    include!("codegen/iot_templates/csr_der.rs");
}

/// Every certificate of `pem`, DER after DER - as the firmware's `CA_DER`
/// holds mqtt_ca.rs's CAs. `None` when there is none.
pub fn cas_der(pem_text: &str) -> Option<Vec<u8>> {
    let p = pem_text.as_bytes();
    let mut out = vec![0u8; pem::pem_len(p, b"CERTIFICATE", true)];
    let n = pem::pem_decode(p, b"CERTIFICATE", true, &mut out);
    (n == out.len() && out.first() == Some(&0x30)).then_some(out)
}

/// A P-256 key: its scalar, and its public point when the key carries one.
pub type ClientKey = ([u8; 32], Option<[u8; 65]>);

/// A client certificate: its DER, what it says, its P-256 point.
pub type ClientCert = (Vec<u8>, CaSummary, Option<[u8; 65]>);

/// A client key from secrets.rs (PEM): its scalar and, when it carries one,
/// its public point - as the firmware reads it. `Err` says why not.
pub fn client_key(pem_text: &str) -> Result<ClientKey, &'static str> {
    let p = pem_text.as_bytes();
    if pem::blank(p) {
        return Err("no key yet");
    }
    if pem_text.contains("ENCRYPTED") || pem_text.contains("Proc-Type:") {
        return Err(
            "an encrypted key - the board needs it in the clear (openssl ec -in key.pem -out plain.pem)",
        );
    }
    if pem_text.contains("BEGIN RSA PRIVATE KEY") {
        return Err(
            "an RSA key - the board signs with P-256 only: make a P-256 key and a CSR (Generate key + CSR)",
        );
    }
    let label = pem::key_label(p);
    let mut der = vec![0u8; pem::pem_len(p, label, false)];
    if der.is_empty() {
        return Err("not a PEM key - BEGIN EC PRIVATE KEY or BEGIN PRIVATE KEY, END line included");
    }
    pem::pem_decode(p, label, false, &mut der);
    pem::ec_key(&der).ok_or(
        "not a P-256 key - the board signs with P-256 only (an RSA or other curve key will not do)",
    )
}

/// A client certificate from secrets.rs (PEM): its DER, what it says, and its
/// public point when its key is P-256.
pub fn client_cert(pem_text: &str) -> Result<ClientCert, &'static str> {
    let p = pem_text.as_bytes();
    if pem::blank(p) {
        return Err("no certificate yet");
    }
    let mut der = vec![0u8; pem::pem_len(p, b"CERTIFICATE", false)];
    if der.is_empty() {
        return Err("not a PEM certificate - BEGIN CERTIFICATE and END lines included");
    }
    pem::pem_decode(p, b"CERTIFICATE", false, &mut der);
    let summary = ca_summary(pem_text)?;
    let point = pem::cert_ec_point(&der);
    Ok((der, summary, point))
}

/// A certificate pasted from the AWS CLI's JSON carries `\n` escapes rather
/// than line ends: `n` is base64, so it would decode into garbage. Undone
/// when the text has no real line end at all.
pub fn unescape_pem(text: &str) -> String {
    if text.contains('\n') || !text.contains("\\n") {
        return text.trim().to_owned();
    }
    text.replace("\\n", "\n")
        .trim()
        .trim_matches('"')
        .to_owned()
}

/// The first PEM certificate's DER - as the firmware's `pem_to_der` reads it.
pub fn pem_der(pem: &str) -> Option<Vec<u8>> {
    let start = pem.find("-----BEGIN CERTIFICATE-----")?;
    let body = &pem[start + "-----BEGIN CERTIFICATE-----".len()..];
    let body = &body[..body.find("-----END CERTIFICATE-----")?];
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in body.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
        acc &= (1 << bits) - 1;
    }
    (out.first() == Some(&0x30)).then_some(out)
}

/// Read the CA `mqtt_ca.rs` holds (its PEM text). `Err` = not a certificate.
pub fn ca_summary(pem: &str) -> Result<CaSummary, &'static str> {
    const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
    const OID_O: &[u8] = &[0x55, 0x04, 0x0A];
    const OID_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
    const OID_EC: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
    const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
    const OID_P384: &[u8] = &[0x2B, 0x81, 0x04, 0x00, 0x22];
    const OID_ED25519: &[u8] = &[0x2B, 0x65, 0x70];
    let bad = "not a PEM certificate - paste one, BEGIN and END lines included";
    let cert = pem_der(pem).ok_or(bad)?;
    let (_, cert, _) = der(&cert).ok_or(bad)?;
    let (0x30, tbs, _) = der(cert).ok_or(bad)? else {
        return Err(bad);
    };
    // version [0] (optional), then 0 serial, 1 signature, 2 issuer, 3 validity,
    // 4 subject, 5 subjectPublicKeyInfo
    let mut rest = tbs;
    let mut fields = Vec::new();
    while let Some((tag, value, next)) = der(rest) {
        fields.push((tag, value));
        rest = next;
    }
    let base = usize::from(fields.first().is_some_and(|(tag, _)| *tag == 0xA0));
    let field = |i: usize| fields.get(base + i).copied().ok_or(bad);
    // validity: two times, UTCTime (YYMMDD...) or GeneralizedTime (YYYYMMDD...)
    let (_, validity) = field(3)?;
    let (_, _, after) = der(validity).ok_or(bad)?;
    let (tag, time, _) = der(after).ok_or(bad)?;
    let time = std::str::from_utf8(time).map_err(|_| bad)?;
    if !time.is_ascii() {
        return Err(bad);
    }
    let (year, md) = match tag {
        0x17 if time.len() >= 6 => {
            let yy: u32 = time[..2].parse().map_err(|_| bad)?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &time[2..6])
        }
        0x18 if time.len() >= 8 => (time[..4].parse().map_err(|_| bad)?, &time[4..8]),
        _ => return Err(bad),
    };
    let not_after = format!("{year:04}-{}-{}", &md[..2], &md[2..4]);
    // subject: the CN, else the O
    let (_, subject) = field(4)?;
    let mut cn = None;
    let mut org = None;
    let mut rdns = subject;
    while let Some((_, set, next)) = der(rdns) {
        rdns = next;
        let Some((_, atv, _)) = der(set) else {
            continue;
        };
        let Some((0x06, oid, value)) = der(atv) else {
            continue;
        };
        let Some((_, text, _)) = der(value) else {
            continue;
        };
        let text = String::from_utf8_lossy(text).into_owned();
        if oid == OID_CN {
            cn = Some(text);
        } else if oid == OID_O {
            org = Some(text);
        }
    }
    let name = cn.or(org).unwrap_or_else(|| "(no name)".to_owned());
    // the key: rsaEncryption, or an EC key on P-256 or another curve
    let (_, spki) = field(5)?;
    let (_, algorithm, _) = der(spki).ok_or(bad)?;
    let (_, oid, params) = der(algorithm).ok_or(bad)?;
    let key = if oid == OID_RSA {
        CaKey::Rsa
    } else if oid == OID_EC {
        match der(params) {
            Some((0x06, OID_P256, _)) => CaKey::EcP256,
            Some((0x06, OID_P384, _)) => CaKey::EcP384,
            _ => CaKey::EcOther,
        }
    } else if oid == OID_ED25519 {
        CaKey::Ed25519
    } else {
        CaKey::Other
    };
    // basicConstraints (2.5.29.19) in the extensions [3]
    let mut is_ca = None;
    if let Some((_, extensions)) = fields.iter().find(|(tag, _)| *tag == 0xA3)
        && let Some((_, mut list, _)) = der(extensions)
    {
        while let Some((_, extension, next)) = der(list) {
            list = next;
            let Some((0x06, oid, mut value)) = der(extension) else {
                continue;
            };
            if oid != [0x55, 0x1D, 0x13] {
                continue;
            }
            if let Some((0x01, _, after)) = der(value) {
                value = after;
            }
            is_ca = Some(false);
            if let Some((0x04, octets, _)) = der(value)
                && let Some((0x30, constraints, _)) = der(octets)
            {
                is_ca = Some(matches!(der(constraints), Some((0x01, [0xFF], _))));
            }
        }
    }
    let more = pem.matches("-----BEGIN CERTIFICATE-----").count() > 1;
    Ok(CaSummary {
        name,
        not_after,
        is_ca,
        key,
        more,
    })
}

/// What is wrong with the TLS settings. `None` = fine.
pub fn tls_problem(m: &MqttConfig) -> Option<&'static str> {
    if !m.tls {
        return None;
    }
    let name = m.tls_name();
    if m.verify && name.parse::<std::net::Ipv4Addr>().is_ok() {
        return Some(
            "a certificate names DNS names, never this address: set Server name to the name in the broker's certificate",
        );
    }
    if name.len() > 64 {
        return Some("longer than 64 bytes - embedded-tls cannot check a name that long");
    }
    if m.port == PLAIN_PORT {
        return Some(
            "1883 is MQTT without TLS - TLS brokers listen on 8883 (test.mosquitto.org: 8886)",
        );
    }
    let host = m.host.trim();
    if host == "broker.hivemq.com" {
        return Some("broker.hivemq.com speaks only TLS 1.2, and embedded-tls only TLS 1.3");
    }
    if host.ends_with(".azure-devices.net") {
        return Some("Azure IoT Hub speaks MQTT 3.1.1, and rust-mqtt only MQTT 5");
    }
    if aws_iot(m) && !m.client_cert {
        return Some(
            "AWS IoT takes a board by its client certificate (a password only through a custom authorizer, which the board cannot reach): switch \"Client certificate\" on",
        );
    }
    None
}

/// Does MQTT over TLS on `family` draw its keys from a hardware TRNG the
/// Pico's link drives itself? The RP2350's (the Pico 2 W); the RP2040 has
/// only its ring oscillator. The ESPs' TRNG goes through esp-hal instead.
pub fn tls_trng(family: &str) -> bool {
    family == "rp235x"
}

/// Does this client talk to AWS IoT? Read off the name the TLS handshake
/// sends (the SNI) - a host given as an address still reaches AWS by it - or
/// the host: see `aws_iot::of`.
pub fn aws_iot(m: &MqttConfig) -> bool {
    super::aws_iot::of(m).is_some()
}

/// AWS IoT on port 443 takes MQTT with a client certificate only when the
/// ClientHello asks for it by ALPN: `x-amzn-mqtt-ca`.
pub fn aws_alpn(m: &MqttConfig) -> bool {
    m.tls && m.client_cert && m.port == 443 && aws_iot(m)
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

/// The topic "Restart on panic" reports on: `<client id>/panic` - see
/// [`client_topic`].
pub fn panic_topic(m: &MqttConfig) -> String {
    client_topic(m, "panic")
}

/// A topic under the board's own name, `<client id>/<leaf>`, made a valid
/// MQTT topic NAME whatever the client id holds - `+` and `#` are wildcards
/// there, a leading `$` is the broker's own namespace, and a `Message` holds
/// 64 bytes. An empty client id (every board alike) gets `rustonchip/<leaf>`.
/// On AWS IoT the policy the tab writes allows `<client id>/*`.
pub fn client_topic(m: &MqttConfig, leaf: &str) -> String {
    let suffix = format!("/{leaf}");
    let id: String = m
        .client_id
        .chars()
        .map(|c| {
            if matches!(c, '+' | '#') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut id = id
        .trim()
        .trim_start_matches(['/', '$', ' '])
        .trim_end_matches('/')
        .to_owned();
    while id.len() > MAX_TOPIC - suffix.len() {
        id.pop();
    }
    if id.is_empty() {
        id = "rustonchip".to_owned();
    }
    format!("{id}{suffix}")
}

/// What is wrong with an MQTT setting, for the line under the field. `None`
/// = fine. These are the checks `rust-mqtt` would otherwise fail AT RUN TIME:
/// an empty or wildcard topic name, a filter whose `#` is not last, a shared
/// subscription that is not `$share/<name>/<filter>`.
pub fn topic_filter_problem(filter: &str) -> Option<&'static str> {
    if filter.is_empty() {
        return Some("empty topic");
    }
    if filter.len() > MAX_TOPIC {
        return Some("longer than 64 bytes - a message on it would not fit `Message`");
    }
    // rust-mqtt takes anything starting `$share` for a shared subscription.
    if let Some(rest) = filter.strip_prefix("$share") {
        let shared = rest
            .strip_prefix('/')
            .and_then(|r| r.split_once('/'))
            .is_some_and(|(name, f)| !name.is_empty() && !name.contains(['+', '#']) && !f.is_empty());
        if !shared {
            return Some("a shared subscription is `$share/<name>/<filter>`, the name without `+` or `#`");
        }
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

/// Why `filter` is left out of the board's SUBSCRIBE: what rust-mqtt would
/// fail at run time, and on AWS IoT what the broker refuses or ends the
/// connection over. `None` = subscribed.
pub fn filter_left_out(m: &MqttConfig, filter: &str) -> Option<&'static str> {
    topic_filter_problem(filter).or_else(|| {
        if aws_iot(m) {
            super::aws_iot::filter_problem(filter)
        } else {
            None
        }
    })
}

/// The filters the board subscribes to, in order.
pub fn subscriptions(m: &MqttConfig) -> Vec<&str> {
    m.subscribe
        .iter()
        .map(String::as_str)
        .filter(|f| filter_left_out(m, f).is_none())
        .collect()
}

/// A text file as a tool may have saved it: UTF-8 (a BOM dropped), or
/// UTF-16 with its BOM - what PowerShell 5.1's `>` writes. `None`: neither.
pub fn decode_text(bytes: &[u8]) -> Option<String> {
    let utf16 = |rest: &[u8], unit: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = rest.as_chunks::<2>().0.iter().map(|c| unit(*c)).collect();
        String::from_utf16(&units).ok()
    };
    match bytes {
        [0xEF, 0xBB, 0xBF, rest @ ..] => String::from_utf8(rest.to_vec()).ok(),
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        _ => String::from_utf8(bytes.to_vec()).ok(),
    }
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
    /// An nRF52's own radio: Bluetooth (Nordic's SoftDevice Controller under
    /// the MPSL) or Thread (OpenThread on embassy-nrf's 802.15.4 driver) -
    /// one at a time. No Wi-Fi.
    Nrf,
}

/// The ESP parts with Bluetooth LE that esp-radio 0.18 drives. Not the S2
/// (no Bluetooth); the H2 has it without Wi-Fi.
const ESP_BT: [&str; 8] = [
    "esp32", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61", "esp32h2",
];

/// The nRF parts whose Bluetooth the generator writes: nrf-sdc 0.4 on
/// embassy-nrf 0.11, compiled and linked on the 52832 and the 52840. Not the
/// 54L15 (its time driver and the MPSL share a GRTC channel) nor the 5340
/// (its radio is on the network core), see [`availability`].
const NRF_BLE: [&str; 3] = ["nrf52832", "nrf52833", "nrf52840"];

/// The radio a Bluetooth project is generated for, or `None` when the IDE does
/// not generate Bluetooth for this chip.
pub fn ble_platform(family: &str, cyw43: bool) -> Option<Platform> {
    if ESP_BT.contains(&family) {
        Some(Platform::Esp)
    } else if cyw43 && matches!(family, "rp2040" | "rp235x") {
        Some(Platform::Cyw43)
    } else if NRF_BLE.contains(&family) {
        Some(Platform::Nrf)
    } else {
        None
    }
}

/// The nRF parts whose Thread the generator writes: openthread 0.4's
/// prebuilt MTD on embassy-nrf 0.11's 802.15.4 driver, linked on both. Not the
/// 52811 / 52820 (OpenThread's statics alone are ~36 KB of RAM), the 5340 (its
/// radio is on the network core) nor the 54L15 (no 802.15.4 driver in
/// embassy-nrf 0.11), see [`availability`].
const NRF_THREAD: [&str; 2] = ["nrf52833", "nrf52840"];

/// The ESP parts whose Thread the generator writes: openthread 0.2 - the
/// release on esp-radio 0.18, the IDE's - on esp-radio's `ieee802154`, linked
/// on all three with OpenThread and Mbed TLS prebuilt for riscv32imac. The
/// C5 is not in openthread's own list (it names the C6 and H2), and has no
/// TRNG: its RNG is random only while a radio runs.
const ESP_THREAD: [&str; 3] = ["esp32c6", "esp32h2", "esp32c5"];

/// Does the IDE generate Thread for this chip?
pub fn thread_platform(family: &str) -> bool {
    NRF_THREAD.contains(&family) || ESP_THREAD.contains(&family)
}

/// The nrf-sdc / embassy-nrf chip feature for an nRF family (`"nrf52840"`).
/// The family key IS the feature for the parts in [`NRF_BLE`].
pub fn nrf_ble_feature(family: &str) -> Option<&'static str> {
    NRF_BLE.iter().copied().find(|f| *f == family)
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
    /// The radio: the Wi-Fi one when there is Wi-Fi, else the Bluetooth one.
    pub platform: Platform,
    /// The Wi-Fi station, and with it the IP stack.
    pub station: bool,
    /// ESP-NOW - only ever on an ESP.
    pub esp_now: bool,
    /// MQTT over the stack - only with the station.
    pub mqtt: bool,
    /// ... over TLS - where the chip has what it takes ([`tls_available`]).
    pub mqtt_tls: bool,
    /// ... accepting P-384 chains.
    pub mqtt_p384: bool,
    /// ... the board presenting a client certificate.
    pub mqtt_client_cert: bool,
    /// SNTP over the stack - only with the station.
    pub sntp: bool,
    /// Bluetooth LE.
    pub ble: bool,
    /// Thread, with OpenThread's UDP - on an nRF or an ESP with 802.15.4,
    /// never beside another link on the radio (see [`thread_blocked`]).
    pub thread: bool,
    /// Restart on panic: `src/pins/configs/panic.rs`, the handler in main.rs's
    /// header and the boot report.
    pub panic: bool,
    /// The HTTP client - only with the station.
    pub http: bool,
    /// ... https:// URLs, where the chip has what it takes ([`https_available`]).
    pub http_tls: bool,
    /// ... accepting P-384 chains.
    pub http_p384: bool,
}

impl Active {
    /// Some client speaks TLS: tls.rs, embedded-tls and (on a Pico) the heap.
    pub fn tls(&self) -> bool {
        self.mqtt_tls || self.http_tls
    }

    /// Some TLS client accepts P-384 chains: embedded-tls's `p384`.
    pub fn p384(&self) -> bool {
        self.mqtt_p384 || self.http_p384
    }
}

/// `Some` when a link is on, the chip can carry it, and the runtime is Async:
/// every driver below is async-only, so a Blocking project gets nothing (the
/// tab says why) rather than code that cannot compile. What needs the IP
/// stack (MQTT, SNTP) is off without the station, whatever the tab holds -
/// and so is any switch a project carried over from another chip still has.
pub fn active(mcu: &crate::panels::mcu_module::mcu::Mcu) -> Option<Active> {
    use crate::panels::mcu_module::mcu::model::Runtime;
    if !matches!(mcu.runtime, Runtime::Async) {
        return None;
    }
    let cyw43 = has_cyw43(mcu);
    let wifi = platform(&mcu.family, cyw43);
    let bt = ble_platform(&mcu.family, cyw43);
    let station = mcu.iot.wifi && wifi.is_some();
    let esp_now = mcu.iot.esp_now.is_some() && ESP_WIFI.contains(&mcu.family.as_str());
    let ble = mcu.iot.ble.is_some() && bt.is_some() && !nrf_ble_blocked_by_usb(mcu);
    let thread = mcu.iot.thread.is_some() && thread_platform(&mcu.family) && !thread_blocked(mcu);
    if !station && !esp_now && !ble && !thread {
        return None;
    }
    let coex = ble && (station || esp_now);
    let tls_ok = tls_available(&mcu.family, coex).is_ok();
    let mqtt_tls = station && mcu.iot.mqtt.as_ref().is_some_and(|m| m.tls) && tls_ok;
    let http = station && mcu.iot.http.is_some();
    // HTTPS where it cannot be carried: http.rs refuses https:// URLs, so
    // nothing ever goes out in clear - HTTP itself stays.
    let http_tls = http
        && mcu.iot.http.as_ref().is_some_and(|h| h.tls)
        && https_available(&mcu.family, coex, mqtt_tls).is_ok();
    Some(Active {
        platform: wifi.or(bt)?,
        station,
        esp_now,
        // TLS asked for where the chip cannot carry it: no MQTT at all, never
        // the password in clear to a port that expects TLS.
        mqtt: station && mcu.iot.mqtt.as_ref().is_some_and(|m| !m.tls || tls_ok),
        mqtt_tls,
        mqtt_p384: station && mcu.iot.mqtt.as_ref().is_some_and(|m| m.tls && m.p384) && tls_ok,
        mqtt_client_cert: station
            && mcu
                .iot
                .mqtt
                .as_ref()
                .is_some_and(|m| m.tls && m.client_cert)
            && tls_ok,
        sntp: station && mcu.iot.sntp.is_some(),
        ble,
        thread,
        panic: mcu.iot.restart_on_panic,
        http,
        http_tls,
        http_p384: http_tls && mcu.iot.http.as_ref().is_some_and(|h| h.p384),
    })
}

/// Can MQTT run over TLS on `family`? `coex` = Bluetooth beside Wi-Fi.
/// `Err` says why not:
///
/// - the ESP32-C5 / C61 have no TRNG in esp-hal 1.1, and TLS keys come from it;
/// - the ESP32 and the ESP32-C2 with Bluetooth beside Wi-Fi: coexistence
///   already takes most of their RAM (the ESP32 had 6 KiB of stack left before
///   TLS's 30 KB; the C2 overflows its DRAM by 41 KB).
pub fn tls_available(family: &str, coex: bool) -> Result<(), &'static str> {
    match family {
        "esp32c5" | "esp32c61" => {
            Err("no TRNG in esp-hal 1.1 on this chip, and TLS's keys need one")
        }
        "esp32" | "esp32c2" if coex => {
            Err("Bluetooth beside Wi-Fi leaves this chip no RAM for TLS")
        }
        _ => Ok(()),
    }
}

/// Thread beside another link on the one 2.4 GHz radio - never generated:
///
/// - Bluetooth, on an nRF: both drive the RADIO (the MPSL binds its vector;
///   embassy-nrf's 802.15.4 driver takes the peripheral), and sharing it needs
///   Nordic's multiprotocol 802.15.4 driver, which openthread does not use.
/// - Bluetooth, Wi-Fi or ESP-NOW, on an ESP: esp-radio 0.18 has no
///   coexistence for 802.15.4 ("things will break"), and its build script
///   refuses `ieee802154` beside `wifi`.
///
/// The other link's switch wins - Bluetooth's even while USB keeps it from
/// being generated, so unwiring USB never silently swaps the radio's owner.
/// A Wi-Fi switch a project carried over from a chip with Wi-Fi blocks
/// nothing on one without (the H2). `false` on a chip without Thread.
pub fn thread_blocked(mcu: &crate::panels::mcu_module::mcu::Mcu) -> bool {
    let family = mcu.family.as_str();
    let wifi = (mcu.iot.wifi && platform(family, has_cyw43(mcu)).is_some())
        || (mcu.iot.esp_now.is_some() && ESP_WIFI.contains(&family));
    thread_platform(family) && (mcu.iot.ble.is_some() || wifi)
}

/// Bluetooth beside USB on an nRF: both bind the CLOCK_POWER vector (the MPSL
/// runs the clocks, USB's VBUS detection the power events), and the two
/// together have not been tried on a board. Not generated until they are; the
/// tab says why - on either runtime, since switching to Async alone would not
/// bring Bluetooth.
pub fn nrf_ble_blocked_by_usb(mcu: &crate::panels::mcu_module::mcu::Mcu) -> bool {
    NRF_BLE.contains(&mcu.family.as_str())
        && crate::panels::mcu_module::codegen::nrf::usb_wired(mcu)
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

/// Can this chip's HARDWARE carry `link` at all, whatever this IDE generates?
///
/// The tab lists only these. An ESP-only protocol on an STM32, or Bluetooth on
/// a Pico without the radio, is not an option with a reason - it is no option.
/// What the chip can carry but the IDE does not generate (yet) stays listed,
/// with [`availability`]'s reason: that is the answer to "can it do mesh?".
pub fn chip_can_carry(link: Link, family: &str, cyw43: bool) -> bool {
    match link {
        Link::Wifi => platform(family, cyw43).is_some(),
        Link::EspNow | Link::EspWifiMesh => ESP_WIFI.contains(&family),
        Link::Ble | Link::BleMesh => has_ble_radio(family, cyw43),
        Link::Thread | Link::Zigbee => has_ieee802154_radio(family),
    }
}

/// A Bluetooth LE radio: every ESP but the S2, the CYW43 beside a Pico W,
/// every nRF, and the STM32WB / WBA.
fn has_ble_radio(family: &str, cyw43: bool) -> bool {
    (family.starts_with("esp32") && family != "esp32s2")
        || (cyw43 && matches!(family, "rp2040" | "rp235x"))
        || family.starts_with("nrf")
        || family.starts_with("stm32wb")
}

/// An IEEE 802.15.4 radio (Thread, Zigbee): the ESP32-C5/C6/H2, the nRF
/// parts that have one (the nRF5340's is on its network core), the STM32WB.
fn has_ieee802154_radio(family: &str) -> bool {
    matches!(
        family,
        "esp32c5"
            | "esp32c6"
            | "esp32h2"
            | "nrf52811"
            | "nrf52820"
            | "nrf52833"
            | "nrf52840"
            | "nrf5340"
            | "nrf54l15"
            | "stm32wb"
    )
}

/// A sub-GHz radio (LoRa, (G)FSK): the STM32WL and WL3. It carries none of the
/// links the tab lists, which the tab says rather than "no radio".
pub fn has_subghz_radio(family: &str) -> bool {
    matches!(family, "stm32wl" | "stm32wl3")
}

/// The links the tab lists for this chip, in the usual order.
pub fn links_for(family: &str, cyw43: bool) -> Vec<Link> {
    Link::ALL
        .into_iter()
        .filter(|l| chip_can_carry(*l, family, cyw43))
        .collect()
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

        Link::Ble if ble_platform(family, cyw43).is_some() => Ready,
        Link::Ble if family == "esp32s2" => NotHere("the ESP32-S2 has no Bluetooth radio"),
        Link::Ble if family == "nrf54l15" => Planned(
            "embassy-nrf 0.11's time driver and the MPSL both use GRTC channel 11 - it builds, and would lose timer alarms on the board",
        ),
        Link::Ble if family == "nrf5340" => NotHere(
            "the SoftDevice Controller runs on the network core, which this IDE does not generate",
        ),
        Link::Ble if nrf => Planned(
            "not tried on this part yet: the SoftDevice Controller and trouble need 20 KB of RAM",
        ),
        Link::Ble if family.starts_with("stm32") => {
            NotHere("embassy-stm32-wpan is not published on crates.io")
        }
        Link::Ble => NotHere("no Bluetooth radio on this chip"),

        Link::Thread if thread_platform(family) => Ready,
        Link::Thread if matches!(family, "nrf52811" | "nrf52820") => NotHere(
            "OpenThread's prebuilt end device needs about 36 KB of static RAM; this part has 24 / 32 KB",
        ),
        Link::Thread if family == "nrf54l15" => NotHere(
            "embassy-nrf 0.11 has no 802.15.4 driver for the nRF54L, and OpenThread ships no prebuilt for its target",
        ),
        Link::Thread if family == "nrf5340" => {
            NotHere("the 802.15.4 radio is on the network core, which this IDE does not generate")
        }
        Link::Thread if family.starts_with("stm32wb") => NotHere(
            "the WB55 / WB35 radio runs Thread inside ST's co-processor firmware, reached through embassy-stm32-wpan - not published on crates.io (the WB15 / WB10 have no 802.15.4)",
        ),
        Link::Thread => NotHere(
            "the chip has an 802.15.4 radio, but no Thread stack is generated for this part",
        ),

        Link::BleMesh => NotHere("no maintained no_std Rust stack"),
        Link::Zigbee => NotHere("no Rust Zigbee stack"),
        Link::EspWifiMesh if esp_wifi => NotHere(
            "ESP-IDF only (esp-idf-svc, std); projects here are no_std esp-hal - ESP-NOW is the mesh-capable link",
        ),
        Link::EspWifiMesh => NotHere("Espressif's own, on the ESP chips with Wi-Fi"),
    }
}

/// A P-256 key whose scalar is 1 and a self-signed certificate of its public
/// key (the curve's G) - a dummy, nothing secret: for the tests and the
/// matrix, which build the client certificate's code.
#[cfg(test)]
pub(crate) mod test_pki {
    /// A P-256 key whose scalar is 1 - a dummy, for tests and the matrix only -
    /// and a self-signed certificate of its public key (the curve's G).
    pub const KEY_SEC1: &str = "-----BEGIN EC PRIVATE KEY-----\nMHcCAQEEIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABoAoGCCqGSM49\nAwEHoUQDQgAEaxfR8uEsQkf4vOblY6RA8ncDfYEt6zOg9KE5RdiYwpZP40Li/hp/\nm47n60p8D54WK84zV2sxXs7LtkBoN79R9Q==\n-----END EC PRIVATE KEY-----\n";
    pub const KEY_PKCS8: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgAAAAAAAAAAAAAAAA
AAAAAAAAAAAAAAAAAAAAAAAAAAGhRANCAARrF9Hy4SxCR/i85uVjpEDydwN9gS3r
M6D0oTlF2JjClk/jQuL+Gn+bjufrSnwPnhYrzjNXazFezsu2QGg3v1H1
-----END PRIVATE KEY-----
    ";
    pub const CERT: &str = "-----BEGIN CERTIFICATE-----
MIIBsjCCAVegAwIBAgIUQJoiYkcDmdfZJFlbe8O5ahizAUowCgYIKoZIzj0EAwIw
LTErMCkGA1UEAwwicnVzdG9uY2hpcCB0ZXN0IGtleSAxIChub3Qgc2VjcmV0KTAg
Fw0yNjEwMTExNzE4MDdaGA8yMTI2MDkxNzE3MTgwN1owLTErMCkGA1UEAwwicnVz
dG9uY2hpcCB0ZXN0IGtleSAxIChub3Qgc2VjcmV0KTBZMBMGByqGSM49AgEGCCqG
SM49AwEHA0IABGsX0fLhLEJH+Lzm5WOkQPJ3A32BLeszoPShOUXYmMKWT+NC4v4a
f5uO5+tKfA+eFivOM1drMV7Oy7ZAaDe/UfWjUzBRMB0GA1UdDgQWBBS0Z6OZnrXv
pnHeADLwipyeXv2g4TAfBgNVHSMEGDAWgBS0Z6OZnrXvpnHeADLwipyeXv2g4TAP
BgNVHRMBAf8EBTADAQH/MAoGCCqGSM49BAMCA0kAMEYCIQCXyy/KJ1vriPOsnMl2
gwSNA/Zp5yEe7ZGQh69qOfMwbQIhAPyHI8wv2E/0E2UrlNoMa+R8DCS2hEv0DeIF
6E/EYPT7
-----END CERTIFICATE-----
    ";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The certificate request the helper sends out, laid out as PKCS#10 says:
    /// read back with the board's own DER walker.
    #[test]
    fn the_certificate_request_is_pkcs10() {
        let (_, point) = client_key(test_pki::KEY_SEC1).unwrap();
        let point = point.unwrap();
        let subject = csr::Subject {
            country: "RO",
            organization: "RustOnChip",
            common_name: &"thing-".repeat(10),
        };
        assert_eq!(subject.problem(), None);
        let info = csr::request_info(&subject, &point);
        let request = csr::request(&info, &[0x30, 0x06, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01]);
        // CertificationRequest { info, algorithm, signature }
        let (0x30, body, len) = pem::der_at(&request, 0).unwrap() else {
            panic!()
        };
        assert_eq!(body + len, request.len());
        let (0x30, info_at, info_len) = pem::der_at(&request, body).unwrap() else {
            panic!()
        };
        assert_eq!(&request[body..info_at + info_len], &info[..]);
        let (0x30, alg, alg_len) = pem::der_at(&request, info_at + info_len).unwrap() else {
            panic!()
        };
        assert_eq!(
            &request[alg..alg + alg_len],
            &[0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02],
            "no parameters"
        );
        let (0x03, bits, _) = pem::der_at(&request, alg + alg_len).unwrap() else {
            panic!()
        };
        assert_eq!(request[bits], 0);
        // The info: version 0, the name (long form past 127 bytes), the key, [0] empty.
        assert_eq!(&info[..2], &[0x30, 0x81]);
        assert!(info.ends_with(&[0xA0, 0x00]));
        assert!(info.windows(65).any(|w| w == point), "the key is in it");
        // The key PEM the helper writes is the one the board reads.
        let d = client_key(test_pki::KEY_SEC1).unwrap().0;
        let pem_text = csr::to_pem("EC PRIVATE KEY", &csr::sec1_key(&d, &point));
        assert_eq!(client_key(&pem_text).unwrap(), (d, Some(point)));
        assert_eq!(pem_text, test_pki::KEY_SEC1, "openssl writes the same");
        assert!(
            csr::Subject {
                country: "ROU",
                organization: "",
                common_name: "x"
            }
            .problem()
            .is_some()
        );
        assert!(
            csr::Subject {
                country: "",
                organization: "",
                common_name: " "
            }
            .problem()
            .is_some()
        );
    }

    /// AWS IoT's data endpoints, not every amazonaws.com host.
    #[test]
    fn aws_iot_hosts_are_told_apart() {
        let aws_iot_host = |h: &str| crate::panels::mcu_module::aws_iot::endpoint(h).is_some();
        assert!(aws_iot_host(
            "a1b2c3d4e5f6g7-ats.iot.eu-central-1.amazonaws.com"
        ));
        assert!(aws_iot_host("A1B2-ats.iot.cn-north-1.amazonaws.com.cn"));
        assert!(
            !aws_iot_host("b-1234.mq.eu-central-1.amazonaws.com"),
            "Amazon MQ"
        );
        assert!(!aws_iot_host("iot.eu-central-1.amazonaws.com"));
        assert!(!aws_iot_host("test.mosquitto.org"));
        let mut m = MqttConfig {
            host: "x-ats.iot.us-east-1.amazonaws.com".into(),
            port: TLS_PORT,
            tls: true,
            ..MqttConfig::default()
        };
        assert!(tls_problem(&m).is_some_and(|p| p.contains("client certificate")));
        m.client_cert = true;
        assert_eq!(tls_problem(&m), None);
        assert!(!aws_alpn(&m), "8883 needs no ALPN");
        m.port = 443;
        assert!(aws_alpn(&m));
        // An address as the host, the endpoint as the name: still AWS - its
        // SNI is what AWS reads.
        m.host = "192.168.1.10".into();
        assert!(!aws_alpn(&m));
        m.server_name = "x-ats.iot.us-east-1.amazonaws.com".into();
        assert!(aws_alpn(&m) && aws_iot(&m));
    }

    /// On AWS IoT a `$` filter other than `$aws/` ends the connection, so it
    /// is left out of SUBSCRIBE there - and only there.
    #[test]
    fn subscriptions_leave_out_what_the_broker_ends_the_connection_over() {
        let mut m = MqttConfig {
            subscribe: vec!["a/b".into(), "$SYS/#".into(), "x/#/y".into(), "$aws/things/t/shadow/get/accepted".into()],
            ..MqttConfig::default()
        };
        assert_eq!(subscriptions(&m), ["a/b", "$SYS/#", "$aws/things/t/shadow/get/accepted"]);
        m.host = "x-ats.iot.us-east-1.amazonaws.com".into();
        assert_eq!(subscriptions(&m), ["a/b", "$aws/things/t/shadow/get/accepted"]);
        assert!(filter_left_out(&m, "$SYS/#").is_some());
        assert_eq!(client_topic(&m, "test"), "rustonchip/test");
    }

    /// What PowerShell 5.1's `>` writes (UTF-16 with a BOM) reads as the
    /// text it is.
    #[test]
    fn text_files_are_read_whatever_their_encoding() {
        let pem = "-----BEGIN CERTIFICATE-----\nAB==\n";
        let le: Vec<u8> = [0xFF, 0xFE].into_iter().chain(pem.encode_utf16().flat_map(u16::to_le_bytes)).collect();
        let be: Vec<u8> = [0xFE, 0xFF].into_iter().chain(pem.encode_utf16().flat_map(u16::to_be_bytes)).collect();
        let bom: Vec<u8> = [0xEF, 0xBB, 0xBF].into_iter().chain(pem.bytes()).collect();
        for bytes in [le, be, bom, pem.as_bytes().to_vec()] {
            assert_eq!(decode_text(&bytes).as_deref(), Some(pem));
        }
        assert_eq!(decode_text(&[0xC3]), None);
    }

    /// The tab reads a key and a certificate as the board does.
    #[test]
    fn client_keys_and_certificates_are_read_as_the_board_reads_them() {
        let (sec1, pkcs8, cert) = (test_pki::KEY_SEC1, test_pki::KEY_PKCS8, test_pki::CERT);
        let (d, point) = client_key(sec1).unwrap();
        assert_eq!(d[31], 1, "the test key is 1");
        let (d8, point8) = client_key(pkcs8).unwrap();
        assert_eq!(d, d8);
        let (der, summary, cert_point) = client_cert(cert).unwrap();
        assert_eq!(der.first(), Some(&0x30));
        assert_eq!(summary.key, CaKey::EcP256);
        assert_eq!(point, cert_point, "the certificate is the key's");
        assert_eq!(point8, cert_point);
        assert!(client_key("").unwrap_err().contains("no key"));
        assert!(
            client_key("-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----")
                .unwrap_err()
                .contains("RSA")
        );
        assert!(
            client_key(&sec1.replace(
                "-----BEGIN EC PRIVATE KEY-----\n",
                "-----BEGIN EC PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\n"
            ))
            .unwrap_err()
            .contains("encrypted")
        );
        assert!(client_cert(" ").unwrap_err().contains("no certificate"));
        // The AWS CLI's JSON form.
        let json = cert.trim().replace('\n', "\\n");
        assert_eq!(client_cert(&unescape_pem(&json)).unwrap().0, der);
        // mqtt_ca.rs's CAs, all of them.
        let two = format!("{cert}\n{cert}");
        assert_eq!(cas_der(&two).unwrap().len(), 2 * der.len());
    }

    /// The report's topic is always a topic name a `Message` can carry.
    #[test]
    fn the_panic_topic_is_a_valid_topic_name() {
        let topic = |id: &str| {
            panic_topic(&MqttConfig {
                client_id: id.into(),
                ..MqttConfig::default()
            })
        };
        assert_eq!(topic("rustonchip-esp32c3"), "rustonchip-esp32c3/panic");
        assert_eq!(topic(""), "rustonchip/panic");
        assert_eq!(topic("  "), "rustonchip/panic");
        assert_eq!(topic("a+b#c"), "a_b_c/panic");
        assert_eq!(topic("$SYS"), "SYS/panic");
        assert_eq!(topic("/x/"), "x/panic");
        assert_eq!(topic("/$SYS/x"), "SYS/x/panic");
        assert_eq!(topic(" /$share/g/t"), "share/g/t/panic");
        let long = topic(&"é".repeat(60));
        assert!(
            long.len() <= MAX_TOPIC && long.ends_with("/panic"),
            "{long}"
        );
        for id in ["", "a+b#c", "$SYS", "/x/", &"z".repeat(90)] {
            let t = topic(id);
            assert!(t.len() <= MAX_TOPIC, "{t}");
            assert!(!t.contains(['+', '#']) && !t.starts_with('$'), "{t}");
        }
    }

    /// A new setup restarts on panic; an `@iot` line from before the switch
    /// keeps halting - the IDE does not edit a header the user has not asked
    /// it to.
    #[test]
    fn restart_on_panic_is_on_only_for_a_new_setup() {
        assert!(IotConfig::default().restart_on_panic);
        let old: IotConfig = ron::from_str("(wifi: true, heap_kib: 72)").unwrap();
        assert!(!old.restart_on_panic);
        assert!(!old.is_default(), "persisted, so it stays off");
    }

    /// The power cap: an `@iot` line from before it reads the default (the
    /// radio's own 20 dBm kept C3 boards off the network), and any value
    /// lands on the step the driver keeps for it.
    #[test]
    fn the_wifi_power_cap_reads_and_rounds_like_the_driver() {
        let old: IotConfig = ron::from_str("(wifi: true, heap_kib: 72)").unwrap();
        assert_eq!(old.wifi_tx_power, DEFAULT_WIFI_TX_POWER);
        assert_eq!(wifi_tx_power_step(DEFAULT_WIFI_TX_POWER), (52, "13 dBm"));
        let cases = [
            (0, 8),
            (8, 8),
            (19, 8),
            (20, 20),
            (43, 34),
            (44, 44),
            (79, 72),
            (80, 80),
            (84, 80),
            (255, 80),
        ];
        for (q, want) in cases {
            assert_eq!(wifi_tx_power_step(q).0, want, "{q}");
        }
        // Ascending, so the search from the top finds the step.
        assert!(WIFI_TX_POWER_STEPS.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(
            WIFI_TX_POWER_STEPS
                .iter()
                .any(|(q, _)| *q == DEFAULT_WIFI_TX_POWER)
        );
    }

    /// Every ESP with a Wi-Fi radio is Ready; the H2 says why it is not.
    #[test]
    fn wifi_follows_the_radio() {
        for chip in ESP_WIFI {
            assert_eq!(
                availability(Link::Wifi, chip, false),
                Availability::Ready,
                "{chip}"
            );
        }
        assert!(matches!(
            availability(Link::Wifi, "esp32h2", false),
            Availability::NotHere(_)
        ));
        assert!(matches!(
            availability(Link::Wifi, "nrf52840", false),
            Availability::NotHere(_)
        ));
        assert!(matches!(
            availability(Link::Wifi, "stm32f1", false),
            Availability::NotHere(_)
        ));
    }

    /// The RP family carries Wi-Fi only on a W board: the radio is beside the
    /// chip, not in it.
    #[test]
    fn a_pico_has_wifi_only_with_the_radio() {
        assert_eq!(platform("rp2040", true), Some(Platform::Cyw43));
        assert_eq!(platform("rp235x", true), Some(Platform::Cyw43));
        assert_eq!(platform("rp2040", false), None);
        assert!(matches!(
            availability(Link::Wifi, "rp235x", false),
            Availability::NotHere(_)
        ));
    }

    /// A WL_LED pad on something that is not an RP is not a CYW43.
    #[test]
    fn the_radio_pad_means_nothing_off_the_rp() {
        assert_eq!(platform("nrf52840", true), None);
    }

    /// Nothing is Ready that the generator does not emit: Wi-Fi where there
    /// is a radio, ESP-NOW on the ESP chips with Wi-Fi, Bluetooth where
    /// `ble_platform` has a radio for it, nothing else.
    #[test]
    fn only_generated_links_are_ready() {
        for family in [
            "esp32", "esp32s2", "esp32c6", "esp32h2", "rp2040", "nrf52840", "nrf5340", "stm32f1",
        ] {
            for link in Link::ALL {
                let a = availability(link, family, family == "rp2040");
                let cyw43 = family == "rp2040";
                match link {
                    Link::Wifi => {}
                    Link::EspNow if ESP_WIFI.contains(&family) => {
                        assert_eq!(a, Availability::Ready, "{family}")
                    }
                    Link::Ble => assert_eq!(
                        a == Availability::Ready,
                        ble_platform(family, cyw43).is_some(),
                        "{family}"
                    ),
                    Link::Thread => assert_eq!(
                        a == Availability::Ready,
                        thread_platform(family),
                        "{family}"
                    ),
                    _ => assert_ne!(a, Availability::Ready, "{family} {link:?}"),
                }
            }
        }
    }

    /// Each chip lists only what its radio can carry - no ESP-only protocol on
    /// another vendor's chip, nothing at all on a chip without a radio.
    #[test]
    fn the_tab_lists_only_what_the_chip_can_carry() {
        use Link::*;
        assert!(links_for("stm32f1", false).is_empty());
        assert!(
            links_for("rp2040", false).is_empty(),
            "a Pico without the W"
        );
        assert_eq!(links_for("rp2040", true), [Wifi, Ble, BleMesh]);
        assert_eq!(links_for("rp235x", true), [Wifi, Ble, BleMesh]);
        assert_eq!(links_for("nrf52832", false), [Ble, BleMesh]);
        assert_eq!(links_for("nrf52840", false), [Ble, Thread, BleMesh, Zigbee]);
        assert_eq!(
            links_for("esp32c3", false),
            [Wifi, EspNow, Ble, BleMesh, EspWifiMesh]
        );
        assert_eq!(links_for("esp32s2", false), [Wifi, EspNow, EspWifiMesh]);
        assert_eq!(links_for("esp32h2", false), [Ble, Thread, BleMesh, Zigbee]);
        assert_eq!(
            links_for("esp32c6", false),
            [Wifi, EspNow, Ble, Thread, BleMesh, Zigbee, EspWifiMesh]
        );
        // A sub-GHz radio carries nothing listed, and is not "no radio".
        assert!(links_for("stm32wl", false).is_empty() && has_subghz_radio("stm32wl"));
        assert!(links_for("stm32wl3", false).is_empty() && has_subghz_radio("stm32wl3"));
        assert!(!has_subghz_radio("stm32f1"));
    }

    /// A listed link never gives a reason that denies the radio it was
    /// listed for: the chip has it, so "no radio" / "needs a radio" is false.
    #[test]
    fn a_listed_link_never_says_the_chip_lacks_its_radio() {
        for family in [
            "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61",
            "esp32h2", "rp2040", "rp235x", "nrf52805", "nrf52810", "nrf52811", "nrf52820",
            "nrf52832", "nrf52833", "nrf52840", "nrf5340", "nrf54l15", "stm32wb", "stm32wba",
            "stm32wl", "stm32wl3", "stm32f1",
        ] {
            for cyw43 in [false, true] {
                for link in links_for(family, cyw43) {
                    let why = match availability(link, family, cyw43) {
                        Availability::Ready => continue,
                        Availability::Planned(w) | Availability::NotHere(w) => w,
                    };
                    assert!(
                        !why.starts_with("no ") || !why.contains("radio"),
                        "{family} {link:?}: {why}"
                    );
                    assert!(!why.starts_with("needs an"), "{family} {link:?}: {why}");
                }
            }
        }
    }

    /// What the generator emits is always among what the tab lists.
    #[test]
    fn everything_generated_is_listed() {
        for family in [
            "esp32", "esp32s2", "esp32s3", "esp32c2", "esp32c3", "esp32c5", "esp32c6", "esp32c61",
            "esp32h2", "rp2040", "rp235x", "nrf52832", "nrf52833", "nrf52840", "nrf5340",
            "nrf54l15", "stm32f1",
        ] {
            for cyw43 in [false, true] {
                for link in Link::ALL {
                    if availability(link, family, cyw43) == Availability::Ready {
                        assert!(chip_can_carry(link, family, cyw43), "{family} {link:?}");
                    }
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
        for bad in [
            "24:6F:28:AA:BB",
            "24:6F:28:AA:BB:0G",
            "246F28AABB01",
            "24:6F:28:AA:BB:001",
        ] {
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
                if link == Link::Thread && family != "rp2040" {
                    assert_eq!(availability(link, family, false), Availability::Ready);
                    continue;
                }
                match availability(link, family, false) {
                    Availability::Ready => panic!("{link:?} on {family} is not generated"),
                    Availability::Planned(why) | Availability::NotHere(why) => {
                        assert!(!why.is_empty())
                    }
                }
            }
        }
    }

    /// The dataset openthread's own nRF example joins, as `ot-ctl dataset
    /// active -x` prints it: its channel, PAN, name and key are read back; a
    /// mangled one is refused with the reason.
    #[test]
    fn a_thread_dataset_decodes_and_a_broken_one_says_why() {
        const EXAMPLE: &str = "000300001901020fd80208b566147d38e384200e080000639c5d67a3bd0510c490f58d4be0d5eaeb0f09b395d1ae17030d4e4553542d50414e2d304644380708fd7d4f8232cb00000410a7e08419ae47c177fb91bcfcec789aa50c0402a0f77835060004001fffe0";
        let s = decode_dataset(EXAMPLE).expect("the example decodes");
        assert_eq!(s.channel, Some(25));
        assert_eq!(s.pan_id, Some(0x0fd8));
        assert_eq!(s.network_name.as_deref(), Some("NEST-PAN-0FD8"));
        assert_eq!(
            s.ext_pan_id,
            Some([0xb5, 0x66, 0x14, 0x7d, 0x38, 0xe3, 0x84, 0x20])
        );
        assert!(s.has_network_key);
        assert!(s.mesh_local_prefix.is_some());
        // Upper case and surrounding blanks are what a terminal copy gives.
        assert!(decode_dataset(&format!("  {}\n", EXAMPLE.to_uppercase())).is_ok());

        assert_eq!(decode_dataset(""), Err("empty"));
        assert!(decode_dataset("0e08zz").is_err(), "not hex");
        assert!(
            decode_dataset(&EXAMPLE[..EXAMPLE.len() - 1]).is_err(),
            "odd"
        );
        assert!(
            decode_dataset(&EXAMPLE[..EXAMPLE.len() - 2]).is_err(),
            "cut short"
        );
        assert!(
            decode_dataset(&"00".repeat(MAX_DATASET_BYTES + 1)).is_err(),
            "too long"
        );
        // A channel alone has no key to attach with.
        assert_eq!(decode_dataset("000300000f"), Err("no network key in it"));
        // What OpenThread refuses, or cannot attach with, is refused here too.
        let key = "0510c490f58d4be0d5eaeb0f09b395d1ae17";
        let rest = "0208b566147d38e38420030d4e4553542d50414e2d30464438";
        assert_eq!(
            decode_dataset(&format!("0003000005{key}")),
            Err("a channel outside 11-26 (page 0)")
        );
        assert_eq!(
            decode_dataset(&format!("0003020019{key}")),
            Err("a channel outside 11-26 (page 0)"),
            "page 2"
        );
        assert_eq!(
            decode_dataset(&format!("00030000190003000019{key}")),
            Err("a TLV appears twice")
        );
        assert_eq!(
            decode_dataset(&format!("01010f{key}")),
            Err("a TLV is shorter than its type needs")
        );
        assert_eq!(
            decode_dataset(&format!("0302ff00{key}")),
            Err("a network name that is not 1-16 bytes of UTF-8")
        );
        assert_eq!(
            decode_dataset(&format!("0003000019{key}{rest}")),
            Err("no PAN ID in it")
        );
        assert_eq!(
            fmt_mesh_local_prefix(&s.mesh_local_prefix.unwrap()),
            "fd7d:4f82:32cb:0::/64"
        );
    }

    /// Thread on the two parts that carry it, never beside Bluetooth (whose
    /// switch wins, USB or not), never off Async, never on a 52832.
    #[test]
    fn thread_takes_the_radio_only_without_bluetooth() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        let build = |id: &str| {
            let def = builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap();
            let mut m = def.build_mcu();
            m.runtime = crate::panels::mcu_module::mcu::model::Runtime::Async;
            m.iot.thread = Some(ThreadConfig::default());
            m
        };
        let mut dk = build("nrf52840_dk");
        let a = active(&dk).expect("Thread alone");
        assert!(a.thread && !a.ble && !a.station, "{a:?}");
        assert_eq!(a.platform, Platform::Nrf);
        assert!(!thread_blocked(&dk));

        dk.iot.ble = Some(BleConfig::default());
        let a = active(&dk).expect("Bluetooth");
        assert!(a.ble && !a.thread, "Bluetooth wins: {a:?}");
        assert!(thread_blocked(&dk));

        dk.iot.ble = None;
        dk.runtime = crate::panels::mcu_module::mcu::model::Runtime::Blocking;
        assert_eq!(active(&dk), None);

        assert!(active(&build("nrf52833_microbit_v2")).is_some_and(|a| a.thread));
        assert_eq!(active(&build("nrf52832_dk")), None, "no Thread on a 52832");
        assert!(!thread_blocked(&build("nrf52832_dk")));
    }

    /// Thread on an ESP: the C6, H2 and C5, alone on the radio - Bluetooth,
    /// Wi-Fi or ESP-NOW switched on keeps it off, and a Wi-Fi switch carried
    /// over to the H2 (no Wi-Fi) blocks nothing.
    #[test]
    fn thread_on_an_esp_takes_the_radio_alone() {
        use crate::panels::mcu_module::builtins::builtin_definitions;
        let build = |id: &str| {
            let def = builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap();
            let mut m = def.build_mcu();
            m.runtime = crate::panels::mcu_module::mcu::model::Runtime::Async;
            m.iot.thread = Some(ThreadConfig::default());
            m
        };
        for id in ["esp32c6", "esp32h2", "esp32c5"] {
            let m = build(id);
            let a = active(&m).unwrap_or_else(|| panic!("{id}: Thread alone"));
            assert!(
                a.thread && !a.station && !a.ble && !a.esp_now,
                "{id}: {a:?}"
            );
            assert_eq!(a.platform, Platform::Esp, "{id}");
            assert_eq!(
                availability(Link::Thread, &m.family, false),
                Availability::Ready
            );
        }
        let mut c6 = build("esp32c6");
        c6.iot.wifi = true;
        let a = active(&c6).unwrap();
        assert!(a.station && !a.thread, "Wi-Fi wins: {a:?}");
        c6.iot.wifi = false;
        c6.iot.esp_now = Some(EspNowConfig::default());
        assert!(active(&c6).is_some_and(|a| a.esp_now && !a.thread));
        c6.iot.esp_now = None;
        c6.iot.ble = Some(BleConfig::default());
        assert!(active(&c6).is_some_and(|a| a.ble && !a.thread));

        let mut h2 = build("esp32h2");
        h2.iot.wifi = true;
        assert!(!thread_blocked(&h2), "the H2 has no Wi-Fi to block with");
        assert!(active(&h2).is_some_and(|a| a.thread && !a.station));
        h2.iot.ble = Some(BleConfig::default());
        assert!(active(&h2).is_some_and(|a| a.ble && !a.thread));
        // No 802.15.4 on the C3: nothing to generate, nothing blocked.
        let c3 = build("esp32c3");
        assert_eq!(active(&c3), None);
        assert!(!thread_blocked(&c3));
    }

    /// The CA the IDE writes: read back as the tab shows it.
    #[test]
    fn the_isrg_root_reads_as_a_ca() {
        let pem = include_str!("codegen/iot_templates/ca/isrg_root_x1.pem");
        let ca = ca_summary(pem).expect("ISRG Root X1");
        assert_eq!(ca.name, "ISRG Root X1");
        assert_eq!(ca.not_after, "2035-06-04");
        assert!(ca.is_ca == Some(true) && !ca.more);
        assert_eq!(ca.key, CaKey::Rsa);
        assert_eq!(pem_der(pem).map(|d| d.len()), Some(1391));
        // CRLF, two certificates, garbage.
        assert!(ca_summary(&pem.replace('\n', "\r\n")).is_ok());
        assert!(ca_summary(&format!("{pem}\n{pem}")).unwrap().more);
        assert!(ca_summary("hello").is_err());
        // A non-ASCII time is refused, never sliced through a character.
        let der = pem_der(pem).unwrap();
        let at = der
            .windows(13)
            .position(|w| w == b"350604110438Z")
            .expect("notAfter");
        let mut bent = der.clone();
        bent[at..at + 2].copy_from_slice("é".as_bytes());
        let b64 = {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in bent.chunks(3) {
                let n = c
                    .iter()
                    .enumerate()
                    .fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
                for i in 0..=c.len() {
                    s.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
                }
            }
            s
        };
        let pem_bent = format!("-----BEGIN CERTIFICATE-----\n{b64}\n-----END CERTIFICATE-----\n");
        assert!(ca_summary(&pem_bent).is_err());
        assert!(
            ca_summary("-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----").is_err()
        );
    }

    /// TLS's settings: the port follows the switch only from its default,
    /// the name defaults to the host, and what cannot work is said.
    #[test]
    fn tls_settings_follow_the_switch() {
        let mut m = MqttConfig::default();
        assert_eq!(
            m.port_for_tls(true),
            8886,
            "test.mosquitto.org's Let's Encrypt port"
        );
        m.host = "broker.example.org".into();
        assert_eq!(m.port_for_tls(true), TLS_PORT);
        m.port = 8883;
        assert_eq!(m.port_for_tls(false), PLAIN_PORT);
        m.port = 9999;
        assert_eq!(m.port_for_tls(true), 9999, "a chosen port stays");
        assert_eq!(m.port_for_tls(false), 9999);

        m.port = 8883;
        assert_eq!(tls_problem(&m), None, "TLS off");
        m.tls = true;
        assert_eq!(tls_problem(&m), None);
        assert_eq!(m.tls_name(), "broker.example.org");
        m.host = "192.168.1.10".into();
        assert!(tls_problem(&m).is_some(), "an address cannot be checked");
        m.server_name = "mqtt.home.lan".into();
        assert_eq!(tls_problem(&m), None);
        assert_eq!(m.tls_name(), "mqtt.home.lan");
        m.verify = false;
        m.server_name.clear();
        assert_eq!(tls_problem(&m), None, "encrypt only: an address is fine");
        m.port = PLAIN_PORT;
        assert!(tls_problem(&m).is_some());
        m.port = 8883;
        m.host = "broker.hivemq.com".into();
        assert!(tls_problem(&m).unwrap().contains("TLS 1.2"));

        assert!(tls_available("esp32c3", false).is_ok());
        assert!(tls_available("esp32", false).is_ok());
        assert!(tls_available("esp32", true).is_err());
        assert!(tls_available("esp32c3", true).is_ok());
        assert!(tls_available("esp32c2", false).is_ok());
        assert!(tls_available("esp32c2", true).is_err());
        assert!(tls_available("esp32c5", false).is_err());
        assert!(tls_available("esp32c61", false).is_err());
        assert!(tls_available("rp2040", false).is_ok());
    }

    /// An MQTT config saved before TLS reads as plain, checked when switched on.
    #[test]
    fn an_mqtt_config_without_tls_still_reads() {
        let old: MqttConfig = ron::from_str("(host: \"h\", port: 1883)").unwrap();
        assert!(!old.tls && old.verify && old.server_name.is_empty());
    }

    /// A project saved before Thread existed reads with Thread off.
    #[test]
    fn a_config_without_thread_still_reads() {
        let old: IotConfig =
            ron::from_str("(wifi: true, ble: Some((device_name: \"x\")))").unwrap();
        assert_eq!(old.thread, None);
        let back: IotConfig = ron::from_str(
            &ron::to_string(&IotConfig {
                thread: Some(ThreadConfig { udp_port: 5683 }),
                ..old.clone()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(back.thread, Some(ThreadConfig { udp_port: 5683 }));
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
        // rust-mqtt's shared subscriptions: `$share/<name>/<filter>`.
        assert_eq!(topic_filter_problem("$share/g/a/#"), None);
        for bad in ["$share/g/", "$share//a", "$share/a", "$shared/x/y", "$share/+/a"] {
            assert!(topic_filter_problem(bad).is_some(), "{bad}");
        }
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
