//! Code for the IoT tab: `src/pins/configs/{net,wifi,mqtt,sntp,espnow,secrets}.rs`,
//! the `main.rs` lines that start them, and the credentials file's rules.
//!
//! The editable halves of the files live in `iot_templates/` as plain Rust,
//! byte for byte what was cross-compiled on an ESP32-C3 and a Pico W - only
//! the GENERATED blocks are assembled here. That split is what keeps a
//! template from drifting into code nobody compiled: there is no placeholder
//! below the markers to substitute wrongly.
//!
//! - `net.rs`: the address (DHCP or static) as constants, `config()` below.
//! - `wifi.rs`: per radio - esp-radio, or the CYW43 beside a Pico W.
//! - `mqtt.rs`: broker, client id, keep-alive, subscriptions as constants;
//!   the one task that owns the connection, `publish()` and `incoming()`.
//! - `mqtt_link.rs`: how `mqtt.rs` reaches the broker - plain TCP, or TLS 1.3
//!   (embedded-tls) with the broker's certificate checked - swapped by the
//!   tab's TLS switch, so `mqtt.rs` itself never changes with it.
//! - `mqtt_ca.rs`: with TLS, the CA the broker's chain must end in. Written
//!   ONCE (ISRG Root X1) and committed - a CA is public; the user pastes their
//!   broker's own over it.
//! - `sntp.rs`: server and interval as constants; the task that keeps the
//!   clock, `now_unix()` / `now_unix_ms()` / `wait_synced()`.
//! - `espnow.rs`: channel and peers as constants; the task that owns ESP-NOW,
//!   `send()` / `receive()` / `own_mac()`, and `hold_radio` for ESP-NOW alone.
//! - `ble.rs`: the advertised name as a constant; a Nordic UART Service
//!   peripheral on trouble-host, `send()` / `receive()` / `connected()`. One
//!   editable half for the ESP and the Pico W (trouble-host 0.6, the radio as
//!   a `Radio` type alias in the generated block), one for the nRF
//!   (trouble-host 0.8 on the SoftDevice Controller).
//! - `thread.rs`: the UDP port as a constant; an OpenThread end device on an
//!   nRF52840 / nRF52833, `role()` / `wait_attached()` / `addresses()` /
//!   `send_to()` / `receive()`.
//! - `secrets.rs`: SSID and passwords, the Thread dataset. Written ONCE, never
//!   spliced - the file is the store, and the project's `.gitignore` lists
//!   it. A link switched on later only ADDS its missing lines ([`topped_up`]).

use crate::panels::mcu_module::iot::{
    self, Active, BleConfig, EspNowConfig, IotConfig, IpConfig, MqttConfig, Platform, SntpConfig,
    ThreadConfig,
};
use crate::panels::mcu_module::mcu::Mcu;

pub const NET: &str = "net.rs";
pub const WIFI: &str = "wifi.rs";
pub const MQTT: &str = "mqtt.rs";
pub const SNTP: &str = "sntp.rs";
pub const ESPNOW: &str = "espnow.rs";
pub const BLE: &str = "ble.rs";
pub const THREAD: &str = "thread.rs";
pub const MQTT_LINK: &str = "mqtt_link.rs";
pub const MQTT_CA: &str = "mqtt_ca.rs";
pub const SECRETS: &str = "secrets.rs";
pub const PANIC: &str = "panic.rs";

/// The credentials file's path in the project tree.
pub const SECRETS_PATH: &str = "src/pins/configs/secrets.rs";

/// The `.gitignore` line that keeps it out of every commit. Rooted at the
/// project folder (the leading `/`), which is where `.gitignore` sits, so it
/// holds whether the repository starts there or at a system folder above.
pub const SECRETS_IGNORE: &str = "/src/pins/configs/secrets.rs";

const WIFI_ESP_TAIL: &str = include_str!("iot_templates/wifi_esp.rs");
const WIFI_CYW43_TAIL: &str = include_str!("iot_templates/wifi_cyw43.rs");
const NET_TAIL: &str = include_str!("iot_templates/net.rs");
const MQTT_TAIL: &str = include_str!("iot_templates/mqtt.rs");
const SNTP_TAIL: &str = include_str!("iot_templates/sntp.rs");
const ESPNOW_TAIL: &str = include_str!("iot_templates/espnow.rs");
const BLE_TAIL: &str = include_str!("iot_templates/ble.rs");
const BLE_NRF_TAIL: &str = include_str!("iot_templates/ble_nrf.rs");
const THREAD_NRF_TAIL: &str = include_str!("iot_templates/thread_nrf.rs");
const THREAD_ESP_TAIL: &str = include_str!("iot_templates/thread_esp.rs");
const LINK_PLAIN_TAIL: &str = include_str!("iot_templates/mqtt_link_plain.rs");
/// "Restart on panic": one editable half for every platform - the record,
/// the wait, the report - over a generated block with the chip's pieces.
const PANIC_TAIL: &str = include_str!("iot_templates/panic.rs");
/// A TLS link: the radio's half, then the certificate checks - which the
/// IoT tab's "Test from this PC" compiles too (`tls_check`), so the PC checks
/// a broker exactly as the board will.
const LINK_TLS_ESP_TAIL: &str = concat!(
    include_str!("iot_templates/mqtt_link_tls_esp.rs"),
    include_str!("iot_templates/tls_verify.rs"),
    include_str!("iot_templates/pem_der.rs")
);
const LINK_TLS_CYW43_TAIL: &str = concat!(
    include_str!("iot_templates/mqtt_link_tls_cyw43.rs"),
    include_str!("iot_templates/tls_verify.rs"),
    include_str!("iot_templates/pem_der.rs")
);
/// The certificate checks alone, for the PC's check.
pub const TLS_VERIFY: &str = include_str!("iot_templates/tls_verify.rs");
/// The PEM and DER code, for the PC's check (the IDE reads it too: `iot::pem`).
pub const PEM_DER: &str = include_str!("iot_templates/pem_der.rs");
/// A key and its certificate request laid out as DER, for the PC's helper.
pub const CSR_DER: &str = include_str!("iot_templates/csr_der.rs");

/// The IoT tab's "Test from this PC": a small host program the IDE builds
/// once and runs, with the same embedded-tls (`std` for the PC, `p384` so it
/// can say whether the board needs it) and the same certificate checks
/// (`tls_verify.rs`) the firmware compiles.
pub fn tls_check_files() -> Vec<(&'static str, String)> {
    const CARGO_TOML: &str = "\
[package]
name = \"rustonchip-tls-check\"
version = \"0.1.0\"
edition = \"2024\"
publish = false

# Its own workspace: never a member of whatever folder holds it.
[workspace]

[dependencies]
embedded-tls = { version = \"0.19\", default-features = false, features = [\"std\", \"rsa\", \"p384\"] }
embedded-io-adapters = { version = \"0.7\", features = [\"std\"] }
rand_core = { version = \"0.6\", features = [\"getrandom\"] }
# The board's client certificate: its signature, and the key + CSR the tab makes.
p256 = { version = \"0.13\", default-features = false, features = [\"ecdsa\", \"std\"] }
ecdsa = { version = \"0.16\", default-features = false, features = [\"hazmat\", \"rfc6979\", \"std\"] }

[profile.release]
opt-level = 2
";
    vec![
        ("Cargo.toml", CARGO_TOML.to_owned()),
        ("src/main.rs", lf(include_str!("iot_templates/tls_check_main.rs"))),
        ("src/tls_verify.rs", lf(TLS_VERIFY)),
        ("src/pem_der.rs", lf(PEM_DER)),
        ("src/csr_der.rs", lf(CSR_DER)),
    ]
}
/// The CA `mqtt_ca.rs` starts with: ISRG Root X1, Let's Encrypt's root (SHA-256
/// 96:BC:EC:06:...:BD:DF:08:C6, valid to 2035-06-04).
const CA_ISRG_ROOT_X1: &str = include_str!("iot_templates/ca/isrg_root_x1.pem");
/// Editable halves an earlier version of the IDE wrote and this one no longer
/// does, by file. A file still holding one exactly was never touched by its
/// user, so it is moved to the current template instead of being left to fail
/// against a `main.rs` that now calls something else - see [`upgraded`].
///
/// - `wifi_esp_v1`: phase 1's ESP `wifi.rs`, whose `init` took the `WIFI`
///   peripheral itself. Phase 2 creates the radio in `main.rs`, because
///   ESP-NOW needs the same `esp_radio::wifi::new` call's other interface.
/// - `wifi_cyw43_v1`: phase 1's Pico W `wifi.rs`, the same code with two
///   `loop { match .. }` that clippy reads as `while let` (`while_let_loop`).
/// - `thread_nrf_v1`: phase 4's nRF `thread.rs`, which dropped its OpenThread
///   handle when the dataset was empty - the default - and so finalized the
///   instance under the task running it.
/// - `mqtt_v1`: phases 1-4's `mqtt.rs`, which talked to its socket itself;
///   `mqtt.rs` now goes through `mqtt_link.rs` (plain or TLS) and caps the
///   packets the broker may send at its buffer.
/// - `mqtt_link_tls_*_v1`: the first TLS links, the same code with the
///   certificate checks inline - now a block shared with "Test from this PC".
/// - `mqtt_link_tls_*_v2`: the TLS links before the client certificate, one
///   CA, the chain walked from its last certificate.
const LEGACY_TAILS: [(&str, &str); 8] = [
    (WIFI, include_str!("iot_templates/legacy/wifi_esp_v1.rs")),
    (WIFI, include_str!("iot_templates/legacy/wifi_cyw43_v1.rs")),
    (THREAD, include_str!("iot_templates/legacy/thread_nrf_v1.rs")),
    (MQTT, include_str!("iot_templates/legacy/mqtt_v1.rs")),
    (MQTT_LINK, include_str!("iot_templates/legacy/mqtt_link_tls_esp_v1.rs")),
    (MQTT_LINK, include_str!("iot_templates/legacy/mqtt_link_tls_cyw43_v1.rs")),
    (MQTT_LINK, include_str!("iot_templates/legacy/mqtt_link_tls_esp_v2.rs")),
    (MQTT_LINK, include_str!("iot_templates/legacy/mqtt_link_tls_cyw43_v2.rs")),
];

/// Every editable half, current and legacy, for `is_pristine`.
const TAILS: [(&str, &str); 22] = [
    (PANIC, PANIC_TAIL),
    (BLE, BLE_TAIL),
    (BLE, BLE_NRF_TAIL),
    (THREAD, THREAD_NRF_TAIL),
    (THREAD, THREAD_ESP_TAIL),
    (MQTT_LINK, LINK_PLAIN_TAIL),
    (MQTT_LINK, LINK_TLS_ESP_TAIL),
    (MQTT_LINK, LINK_TLS_CYW43_TAIL),
    (WIFI, WIFI_ESP_TAIL),
    (WIFI, WIFI_CYW43_TAIL),
    (NET, NET_TAIL),
    (MQTT, MQTT_TAIL),
    (SNTP, SNTP_TAIL),
    (ESPNOW, ESPNOW_TAIL),
    LEGACY_TAILS[0],
    LEGACY_TAILS[1],
    LEGACY_TAILS[2],
    LEGACY_TAILS[3],
    LEGACY_TAILS[4],
    LEGACY_TAILS[5],
    LEGACY_TAILS[6],
    LEGACY_TAILS[7],
];

const GEN_BEGIN_CFG: &str = "// <<< GENERATED>>>";
const GEN_END_CFG: &str = "// <<< GENERATED END >>>";

/// A template as LF. `include_str!` hands over the file as it is checked out,
/// and with `core.autocrlf` that is CRLF - while every buffer the IDE holds is
/// LF. Unnormalised, no file would ever match its template again (no upgrade,
/// no prune), and generated files would carry mixed line endings.
fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Is `name` one of the IoT tab's files? None of them changes with the
/// runtime - they exist only on Async - so a Runtime Apply's forced rewrite
/// must pass them by, as it does the flash store's: it would only wipe what
/// the user wrote below the markers.
pub fn runtime_free(name: &str) -> bool {
    matches!(
        name,
        NET | WIFI | MQTT | MQTT_LINK | MQTT_CA | SNTP | ESPNOW | BLE | THREAD | SECRETS | PANIC
    )
}

/// The Wi-Fi station's secrets, in the order they are written: the network,
/// and the broker's login (written with the station, MQTT or not).
pub const WIFI_SECRETS: [&str; 4] = ["WIFI_SSID", "WIFI_PASSWORD", "MQTT_USERNAME", "MQTT_PASSWORD"];

/// Thread's one secret: the Active Operational Dataset, as hex TLVs. It holds
/// the network key and the PSKc.
pub const THREAD_DATASET: &str = "THREAD_DATASET";

/// The board's client certificate and its P-256 key, as PEM (MQTT over TLS
/// with a client certificate). The key is the one secret whose loss cannot
/// be undone: the broker knows the board by the certificate made for it.
pub const CLIENT_SECRETS: [&str; 2] = ["MQTT_CLIENT_CERT", "MQTT_CLIENT_KEY"];

/// Every secret the tab edits.
pub const SECRET_NAMES: [&str; 7] = [
    WIFI_SECRETS[0],
    WIFI_SECRETS[1],
    WIFI_SECRETS[2],
    WIFI_SECRETS[3],
    THREAD_DATASET,
    CLIENT_SECRETS[0],
    CLIENT_SECRETS[1],
];

/// What a fresh `secrets.rs` holds for the Wi-Fi station: every value empty.
pub fn secrets_body() -> String {
    secrets_body_for(true, false)
}

/// What a fresh `secrets.rs` holds for the links on: the station's lines,
/// Thread's, or both - every value empty.
pub fn secrets_body_for(station: bool, thread: bool) -> String {
    secrets_body_with(station, thread, false)
}

/// The same, with the client certificate's lines when `client`.
pub fn secrets_body_with(station: bool, thread: bool, client: bool) -> String {
    let mut o = String::new();
    o.push_str("// Credentials for the IoT tab. This file is listed in .gitignore, so it stays\n");
    o.push_str("// on this machine: edit the values here or in the tab, never in a commit.\n");
    let names = WIFI_SECRETS
        .iter()
        .filter(|_| station)
        .chain(std::iter::once(&THREAD_DATASET).filter(|_| thread))
        .chain(CLIENT_SECRETS.iter().filter(|_| client));
    for name in names {
        o.push_str(&format!("pub const {name}: &str = \"\";\n"));
    }
    o
}

/// The `src/pins/configs/` files `mcu`'s IoT tab generates - none unless
/// [`iot::active`] says the code can exist.
pub fn config_files_for(mcu: &Mcu) -> Vec<(String, String)> {
    let Some(active) = iot::active(mcu) else {
        return Vec::new();
    };
    let mut files = config_files(&mcu.iot, active);
    // Last, so the other files keep their places in the list.
    if active.panic {
        files.push((PANIC.to_owned(), panic_file(mcu, active)));
    }
    files
}

/// The files for `cfg` on `active` - see [`config_files_for`]. The station
/// brings the stack and the credentials; MQTT and SNTP ride on it; ESP-NOW
/// stands on its own.
pub fn config_files(cfg: &IotConfig, active: Active) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if active.station {
        out.push((NET.to_owned(), net_file(&cfg.ip, active)));
        out.push((WIFI.to_owned(), wifi_file(active.platform)));
    }
    if active.station || active.thread {
        out.push((
            SECRETS.to_owned(),
            secrets_body_with(active.station, active.thread, active.mqtt_client_cert),
        ));
    }
    if let (true, Some(m)) = (active.mqtt, &cfg.mqtt) {
        out.push((MQTT.to_owned(), mqtt_file(m)));
        out.push((MQTT_LINK.to_owned(), mqtt_link_file(m, active)));
        if active.mqtt_tls {
            out.push((MQTT_CA.to_owned(), mqtt_ca_file()));
        }
    }
    if let (true, Some(s)) = (active.sntp, &cfg.sntp) {
        out.push((SNTP.to_owned(), sntp_file(s)));
    }
    if let (true, Some(n)) = (active.esp_now, &cfg.esp_now) {
        out.push((ESPNOW.to_owned(), espnow_file(n)));
    }
    if let (true, Some(b)) = (active.ble, &cfg.ble) {
        out.push((BLE.to_owned(), ble_file(b, active.platform)));
    }
    if let (true, Some(t)) = (active.thread, &cfg.thread) {
        out.push((THREAD.to_owned(), thread_file(t, active.platform)));
    }
    out
}

/// A Rust string literal for `s`. `{:?}` escapes quotes, backslashes and
/// control characters exactly the way the language reads them back.
fn lit(s: &str) -> String {
    format!("{s:?}")
}

fn bytes4(b: [u8; 4]) -> String {
    format!("[{}, {}, {}, {}]", b[0], b[1], b[2], b[3])
}

/// The sockets the stack must hold: DHCP and DNS, one per protocol the tab
/// switched on, and two left for the user's own. One short and a socket
/// panics inside smoltcp (`socket_set.rs`), at run time. Two spare, because
/// phase 1 wrote 4 for a station without MQTT: fewer now would break code
/// that already opens two sockets of its own.
pub fn sockets(active: Active) -> usize {
    4 + usize::from(active.mqtt) + usize::from(active.sntp)
}

fn net_file(ip: &IpConfig, active: Active) -> String {
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// IP settings (from the IoT tab) — auto-updated; edit them in the tab.\n");
    o.push_str(&format!("pub const DHCP: bool = {};\n", ip.dhcp));
    o.push_str(&format!("pub const STATIC_IP: [u8; 4] = {};\n", bytes4(ip.address)));
    o.push_str(&format!("pub const PREFIX_LEN: u8 = {};\n", ip.prefix.min(32)));
    o.push_str(&format!("pub const GATEWAY: [u8; 4] = {};\n", bytes4(ip.gateway)));
    o.push_str(&format!("pub const DNS: [u8; 4] = {};\n", bytes4(ip.dns)));
    let mut users = vec!["DHCP and DNS take one each"];
    if active.mqtt {
        users.push("MQTT one");
    }
    if active.sntp {
        users.push("SNTP one");
    }
    o.push_str(&format!(
        "/// Sockets embassy-net can hold at once: {}, and two are left for yours.\n",
        users.join(", ")
    ));
    o.push_str(&format!("pub const SOCKETS: usize = {};\n", sockets(active)));
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(NET_TAIL));
    o
}

fn wifi_file(platform: Platform) -> String {
    let (radio, tail) = match platform {
        Platform::Esp => ("the chip's own radio (esp-radio)", WIFI_ESP_TAIL),
        Platform::Cyw43 => ("the CYW43 radio beside the chip", WIFI_CYW43_TAIL),
        // No Wi-Fi on an nRF: `active()` never sets `station` there.
        Platform::Nrf => return String::new(),
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str(&format!("// Wi-Fi station (from the IoT tab) on {radio}.\n"));
    o.push_str("// The network name and password are in `secrets.rs`, which git ignores.\n");
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(tail));
    o
}

fn mqtt_file(m: &MqttConfig) -> String {
    let topics: Vec<String> = m
        .subscribe
        .iter()
        .filter(|t| iot::topic_filter_problem(t).is_none())
        .map(|t| lit(t))
        .collect();
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// MQTT client (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str(&format!("pub const BROKER_HOST: &str = {};\n", lit(m.host.trim())));
    o.push_str(&format!("pub const BROKER_PORT: u16 = {};\n", m.port));
    o.push_str(&format!("pub const CLIENT_ID: &str = {};\n", lit(&m.client_id)));
    o.push_str(&format!("pub const KEEP_ALIVE_S: u16 = {};\n", m.keep_alive_s));
    o.push_str("/// Subscribed after every connect; what arrives comes out of `incoming()`.\n");
    o.push_str(&format!("pub const SUBSCRIBE: &[&str] = &[{}];\n", topics.join(", ")));
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(MQTT_TAIL));
    o
}

/// `mqtt_link.rs`: plain TCP, or TLS with the tab's verification, name and -
/// with SNTP on - its clock for the certificates' dates. The editable half
/// per radio: the RNG (and on a Pico the heap) is the chip's.
fn mqtt_link_file(m: &MqttConfig, active: Active) -> String {
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    if !active.mqtt_tls {
        o.push_str("// MQTT link (from the IoT tab): plain TCP - switch TLS on in the tab.\n");
        o.push_str(GEN_END_CFG);
        o.push('\n');
        o.push_str(&lf(LINK_PLAIN_TAIL));
        return o;
    }
    o.push_str("// MQTT over TLS (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str("/// Check the broker's certificate: its chain must end in mqtt_ca.rs's CA and\n");
    o.push_str("/// name SERVER_NAME. `false` = encrypted, but anyone on the path can pose as\n");
    o.push_str("/// the broker and read the MQTT password.\n");
    o.push_str(&format!("pub const VERIFY: bool = {};\n", m.verify));
    o.push_str("/// The name the broker's certificate must carry, sent as SNI.\n");
    o.push_str(&format!("pub const SERVER_NAME: &str = {};\n", lit(m.tls_name())));
    if active.sntp {
        o.push_str("/// The time the certificates' dates are checked against: SNTP's.\n");
        o.push_str("fn wall_clock() -> Option<u64> {\n    super::sntp::now_unix()\n}\n");
        o.push_str("/// Before a handshake: up to 10 s for SNTP's first answer, so the dates are\n");
        o.push_str("/// checked from the first connection on.\n");
        o.push_str("async fn clock_ready() {\n");
        o.push_str("    let wait = embassy_time::Duration::from_secs(10);\n");
        o.push_str("    let _ = embassy_time::with_timeout(wait, super::sntp::wait_synced()).await;\n");
        o.push_str("}\n");
    } else {
        o.push_str("/// No wall clock - SNTP is off: the certificates' dates are NOT checked.\n");
        o.push_str("fn wall_clock() -> Option<u64> {\n    None\n}\n");
        o.push_str("async fn clock_ready() {}\n");
    }
    if active.mqtt_client_cert {
        o.push_str("/// The board's certificate and P-256 key (secrets.rs), sent when the broker\n");
        o.push_str("/// asks - none while either is empty there. A key that is not P-256, or a\n");
        o.push_str("/// certificate that is not its own, fails the build.\n");
        o.push_str("const CLIENT_CERT_DER: [u8; pem_len(super::secrets::MQTT_CLIENT_CERT.as_bytes(), b\"CERTIFICATE\", false)] = pem_der(\n");
        o.push_str("    super::secrets::MQTT_CLIENT_CERT,\n    b\"CERTIFICATE\",\n    false,\n    true,\n");
        o.push_str("    \"MQTT_CLIENT_CERT in secrets.rs holds no PEM certificate\",\n);\n");
        o.push_str("const CLIENT_KEY_DER: [u8; pem_len(\n    super::secrets::MQTT_CLIENT_KEY.as_bytes(),\n");
        o.push_str("    key_label(super::secrets::MQTT_CLIENT_KEY.as_bytes()),\n    false,\n)] = pem_der(\n");
        o.push_str("    super::secrets::MQTT_CLIENT_KEY,\n    key_label(super::secrets::MQTT_CLIENT_KEY.as_bytes()),\n    false,\n    true,\n");
        o.push_str("    \"MQTT_CLIENT_KEY in secrets.rs holds no P-256 key (EC PRIVATE KEY or PRIVATE KEY, not encrypted)\",\n);\n");
        o.push_str("pub const CLIENT: Option<(&[u8], [u8; 32])> = client_identity(&CLIENT_CERT_DER, &CLIENT_KEY_DER);\n");
    } else {
        o.push_str("/// No client certificate - switch it on in the tab when the broker wants one.\n");
        o.push_str("pub const CLIENT: Option<(&[u8], [u8; 32])> = None;\n");
    }
    if iot::aws_alpn(m) {
        o.push_str("/// AWS IoT on port 443 takes MQTT with a client certificate by this ALPN.\n");
        o.push_str("pub const ALPN: &[&[u8]] = &[b\"x-amzn-mqtt-ca\"];\n");
    } else {
        o.push_str("pub const ALPN: &[&[u8]] = &[];\n");
    }
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(match active.platform {
        Platform::Cyw43 => LINK_TLS_CYW43_TAIL,
        _ => LINK_TLS_ESP_TAIL,
    }));
    o
}

/// The CAs the tab offers to put in mqtt_ca.rs, by the brokers that need
/// them. Public certificates; each SHA-256 checked when added.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CaPreset {
    /// ISRG Root X1: Let's Encrypt's (test.mosquitto.org on 8886).
    LetsEncrypt,
    /// Amazon Root CA 1 (RSA, 8E:CD:E6:88...19:6E) and 3 (P-256, 18:CE:6C:FE...B3:A4):
    /// AWS IoT serves a chain ending in either, by the signature it picks.
    Aws,
    /// mosquitto.org's own CA (FC:9E:45:E2...65:39): test.mosquitto.org on 8883 and 8884.
    Mosquitto,
}

impl CaPreset {
    pub const ALL: [CaPreset; 3] = [Self::LetsEncrypt, Self::Aws, Self::Mosquitto];

    pub fn label(self) -> &'static str {
        match self {
            Self::LetsEncrypt => "ISRG Root X1 (Let's Encrypt; test.mosquitto.org 8886)",
            Self::Aws => "Amazon Root CA 1 + 3 (AWS IoT)",
            Self::Mosquitto => "mosquitto.org (test.mosquitto.org 8883, 8884)",
        }
    }
}

const CA_AMAZON_ROOT_1: &str = include_str!("iot_templates/ca/amazon_root_ca1.pem");
const CA_AMAZON_ROOT_3: &str = include_str!("iot_templates/ca/amazon_root_ca3.pem");
const CA_MOSQUITTO: &str = include_str!("iot_templates/ca/mosquitto_org.pem");

/// mqtt_ca.rs holding `preset`'s CA(s).
pub fn mqtt_ca_preset(preset: CaPreset) -> String {
    if preset == CaPreset::LetsEncrypt {
        return mqtt_ca_file();
    }
    let (about, pem) = match preset {
        CaPreset::Aws => (
            "//! AWS IoT Core: Amazon Root CA 1 (RSA) and Amazon Root CA 3 (P-256) - the\n\
             //! broker's chain ends in one or the other, by the signature it picks.\n",
            format!("{}\n{}", lf(CA_AMAZON_ROOT_1).trim_end(), lf(CA_AMAZON_ROOT_3).trim_end()),
        ),
        _ => (
            "//! test.mosquitto.org on ports 8883 and 8884 (client certificate): the\n\
             //! mosquitto.org CA (https://test.mosquitto.org/ssl/mosquitto.org.crt).\n",
            lf(CA_MOSQUITTO).trim_end().to_owned(),
        ),
    };
    let mut o = String::new();
    o.push_str("//! The CAs the MQTT broker's certificate chain must end in (IoT tab: MQTT over\n");
    o.push_str("//! TLS). A CA is public: this file is committed. Paste your broker's CA(s)\n");
    o.push_str("//! here, as PEM - every certificate in it is trusted.\n//!\n");
    o.push_str(about);
    o.push_str(&format!("pub const CA_PEM: &str = \"{pem}\";\n"));
    o
}

/// `mqtt_ca.rs` as first written: ISRG Root X1. No markers - the whole file
/// is the user's once written (`write_once`).
pub fn mqtt_ca_file() -> String {
    let mut o = String::new();
    o.push_str("//! The CA the MQTT broker's certificate chain must end in (IoT tab: MQTT over\n");
    o.push_str("//! TLS). A CA is public: this file is committed. Paste your broker's CA here,\n");
    o.push_str("//! as PEM - every certificate in it is trusted.\n");
    o.push_str("//!\n");
    o.push_str("//! Written with ISRG Root X1, Let's Encrypt's root (valid to 2035-06-04): it\n");
    o.push_str("//! signs test.mosquitto.org on port 8886, and brokers with an RSA Let's\n");
    o.push_str("//! Encrypt certificate. An ECDSA one (certbot's default) also ends here, but\n");
    o.push_str("//! through P-384 issuers: switch \"P-384 chains\" on in the IoT tab for it.\n");
    o.push_str("//! test.mosquitto.org's 8883 needs mosquitto.org's own CA\n");
    o.push_str("//! (https://test.mosquitto.org/ssl/mosquitto.org.crt).\n");
    o.push_str(&format!("pub const CA_PEM: &str = \"{}\";\n", lf(CA_ISRG_ROOT_X1).trim_end()));
    o
}

fn sntp_file(s: &SntpConfig) -> String {
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// Network time (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str(&format!("pub const SERVER: &str = {};\n", lit(s.server.trim())));
    o.push_str("pub const PORT: u16 = 123;\n");
    o.push_str("/// Seconds between two synchronisations once the clock is set.\n");
    o.push_str(&format!(
        "pub const INTERVAL_S: u64 = {};\n",
        s.interval_s.max(iot::MIN_SNTP_INTERVAL_S)
    ));
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(SNTP_TAIL));
    o
}

fn espnow_file(n: &EspNowConfig) -> String {
    let peers: Vec<String> = n
        .peers
        .iter()
        .filter(|m| iot::peer_problem(m).is_none())
        .take(iot::MAX_ESPNOW_PEERS)
        .map(|m| {
            let bytes: Vec<String> = m.iter().map(|b| format!("0x{b:02X}")).collect();
            format!("[{}]", bytes.join(", "))
        })
        .collect();
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// ESP-NOW (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str("/// The channel ESP-NOW talks on while the Wi-Fi station is off (1..=13).\n");
    o.push_str("/// With the station on, the radio is on the access point's channel instead.\n");
    o.push_str(&format!("pub const CHANNEL: u8 = {};\n", n.channel.clamp(1, 13)));
    o.push_str("/// Boards put on the peer list at start. `send` adds any other address the\n");
    o.push_str("/// first time it is used; the list holds 20, the broadcast address among them.\n");
    o.push_str(&format!("pub const PEERS: &[[u8; 6]] = &[{}];\n", peers.join(", ")));
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(ESPNOW_TAIL));
    o
}

/// `ble.rs`: the name, and on the ESP / Pico W the radio's type, which is all
/// that differs between them - the editable half is the same text.
fn ble_file(b: &BleConfig, platform: Platform) -> String {
    let name = if iot::ble_name_problem(&b.device_name).is_none() {
        b.device_name.as_str()
    } else {
        iot::DEFAULT_BLE_NAME
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// Bluetooth LE (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str("/// The name a phone lists the board under: 22 bytes at most.\n");
    o.push_str(&format!("pub const DEVICE_NAME: &str = {};\n", lit(name)));
    let tail = match platform {
        Platform::Esp => {
            o.push_str("/// The radio's Bluetooth half, which `main.rs` hands to `start`: the\n");
            o.push_str("/// chip's own (esp-radio).\n");
            o.push_str("pub type Radio = esp_radio::ble::controller::BleConnector<'static>;\n");
            BLE_TAIL
        }
        Platform::Cyw43 => {
            o.push_str("/// The radio's Bluetooth half, which `main.rs` hands to `start`: the\n");
            o.push_str("/// CYW43 beside the chip.\n");
            o.push_str("pub type Radio = cyw43::bluetooth::BtDriver<'static>;\n");
            BLE_TAIL
        }
        Platform::Nrf => BLE_NRF_TAIL,
    };
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(tail));
    o
}

/// `thread.rs`: the UDP port. Which network is the dataset in `secrets.rs`.
/// The editable half per radio: openthread 0.4 on an nRF, 0.2 on an ESP -
/// the same API for the user's code.
fn thread_file(t: &ThreadConfig, platform: Platform) -> String {
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// Thread (from the IoT tab) — auto-updated; edit it in the tab.\n");
    o.push_str("/// The UDP port `receive` listens on and `send_to` sends from.\n");
    o.push_str(&format!("pub const UDP_PORT: u16 = {};\n", t.udp_port));
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(match platform {
        Platform::Esp => THREAD_ESP_TAIL,
        _ => THREAD_NRF_TAIL,
    }));
    o
}

// ── The credentials file ─────────────────────────────────────────────────────

/// A file `sync_config_files` writes when it is missing and never touches
/// again: the user's values live in it, nowhere else.
pub fn write_once(name: &str) -> bool {
    name == SECRETS || name == MQTT_CA
}

/// Read one secret back out of `secrets.rs`. `None` when the line is missing
/// or is no longer a plain string literal the tab can show.
pub fn read_secret(file: &str, name: &str) -> Option<String> {
    let prefix = format!("pub const {name}: &str = ");
    let line = file.lines().find(|l| l.trim_start().starts_with(&prefix))?;
    let rest = line.trim_start().strip_prefix(&prefix)?;
    let lit = rest.trim_end().strip_suffix(';')?;
    unescape(lit)
}

/// `file` with `name` set to `value` - the line rewritten in place, or added
/// at the end when the user deleted it.
pub fn write_secret(file: &str, name: &str, value: &str) -> String {
    let prefix = format!("pub const {name}: &str = ");
    let line = format!("{prefix}{};", lit(value));
    let mut found = false;
    // The whole statement goes, however many lines it spans (a PEM written
    // by hand as a multi-line literal), not just its first.
    let mut inside = false;
    let mut out: Vec<String> = Vec::new();
    for l in file.lines() {
        if inside {
            inside = !l.trim_end().ends_with(';');
            continue;
        }
        if !found && l.trim_start().starts_with(&prefix) {
            found = true;
            inside = !l.trim_end().ends_with(';');
            out.push(line.clone());
        } else {
            out.push(l.to_owned());
        }
    }
    if !found {
        out.push(line);
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// A Rust string literal back to its value - only the escapes `{:?}` writes.
fn unescape(lit: &str) -> Option<String> {
    let body = lit.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return None; // an unescaped quote: not one literal
        }
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            '0' => out.push('\0'),
            '\\' => out.push('\\'),
            '"' => out.push('"'),
            '\'' => out.push('\''),
            'u' => {
                let rest: String = chars.by_ref().take_while(|c| *c != '}').collect();
                let hex = rest.strip_prefix('{')?;
                out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Does `file` carry a line for `name`, whatever its value? `read_secret`
/// cannot say: it is `None` for a missing line and for one the user turned
/// into something other than a plain literal alike.
///
/// Any declaration of the name counts - `pub(crate) const`, `static`, a type
/// of the user's own - since adding the IDE's line beside it would define the
/// name twice.
pub fn has_secret_line(file: &str, name: &str) -> bool {
    file.lines().any(|l| declares(l, name))
}

/// Does `line` declare a `const` or `static` named `name`, at any visibility?
fn declares(line: &str, name: &str) -> bool {
    let mut l = line.trim_start();
    if let Some(rest) = l.strip_prefix("pub") {
        l = rest.trim_start();
        if l.starts_with('(') {
            let Some(close) = l.find(')') else {
                return false;
            };
            l = l[close + 1..].trim_start();
        }
    }
    let Some(rest) = l.strip_prefix("const ").or_else(|| l.strip_prefix("static ")) else {
        return false;
    };
    let rest = rest.trim_start();
    let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
    rest.strip_prefix(name).is_some_and(|r| r.trim_start().starts_with(':'))
}

/// Exactly what `secrets_body_for` wrote, for whichever links: comments, and
/// the IDE's own secret lines, every one still empty. A value, a line of the
/// user's own or any other edit keeps the file.
fn secrets_pristine(content: &str) -> bool {
    let empty: Vec<String> = SECRET_NAMES
        .iter()
        .map(|n| format!("pub const {n}: &str = \"\";"))
        .collect();
    let mut any = false;
    for line in content.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("//")) {
        if !empty.iter().any(|e| e == line) {
            return false;
        }
        any = true;
    }
    any
}

/// `existing` with the secret lines `body` has and it lacks appended - empty,
/// as `body` writes them - or `None` when nothing is missing. `secrets.rs` is
/// written once, so a link switched on after it (Thread beside a station
/// project, MQTT's names in a file from before them) would otherwise import
/// a name that is not there. Lines already in the file are never touched.
pub fn topped_up(existing: &str, body: &str) -> Option<String> {
    let missing: Vec<&str> = body
        .lines()
        .filter(|l| {
            SECRET_NAMES
                .iter()
                .any(|n| l.starts_with(&format!("pub const {n}:")) && !has_secret_line(existing, n))
        })
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut out = existing.to_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    for line in missing {
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

/// Is `content` still exactly a template below its markers? Then the IoT tab
/// switched off may let it go; otherwise the user wrote in it and it stays.
fn is_pristine(name: &str, content: &str) -> bool {
    if name == SECRETS {
        return secrets_pristine(content);
    }
    if name == MQTT_CA {
        // Also as the IDE wrote it before mqtt_ca.rs held several CAs.
        let content = lf(content);
        return content == mqtt_ca_file()
            || content
                == mqtt_ca_file().replace(
                    "//! as PEM - every certificate in it is trusted.\n",
                    "//! as PEM - only the first certificate is used.\n",
                );
    }
    let Some(tail) = tail_of(content) else {
        return false;
    };
    TAILS.iter().any(|(n, t)| *n == name && lf(t) == tail)
}

/// What replaces `existing` when it still holds an older IDE's template, or
/// another platform's:
///
/// - untouched (see [`LEGACY_TAILS`]), or untouched but written for another
///   radio (an ESP `ble.rs` on a project retargeted to an nRF, a Pico W
///   `wifi.rs` on an ESP): the current template, below whatever the user wrote
///   ABOVE the generated block, which is theirs;
/// - phase 1's ESP `wifi.rs` with the user's own edits: the same file with
///   `init` moved to the signature `main.rs` now calls - the three lines that
///   differ, nothing else - so their edits survive and the project compiles.
///
/// `None` for anything else: the usual splice of the generated block applies,
/// and a file the user edited is never rewritten. A phase-1 `wifi.rs` edited
/// past recognition is left too, and the IoT tab says what to change
/// ([`phase_one_wifi_left`]).
pub fn upgraded(name: &str, existing: &str, body: &str) -> Option<String> {
    let tail = tail_of(existing)?;
    let foreign = tail_of(body).is_some_and(|want| want != tail)
        && TAILS.iter().any(|(n, t)| *n == name && lf(t) == tail);
    if foreign
        || LEGACY_TAILS
            .iter()
            .any(|(n, t)| *n == name && lf(t) == tail)
    {
        let text = lf(existing);
        let head = text.find(GEN_BEGIN_CFG).map_or("", |i| &text[..i]);
        return Some(format!("{head}{body}"));
    }
    if name == WIFI {
        return migrate_wifi_esp_v1(existing);
    }
    None
}

/// Phase 1's ESP `init`, which created the radio itself.
const V1_INIT: &str =
    "pub fn init(spawner: Spawner, wifi: esp_hal::peripherals::WIFI<'static>) -> Stack<'static> {\n";
const V1_NEW: &str =
    "    let (controller, interfaces) = esp_radio::wifi::new(wifi, Default::default()).unwrap();\n";
const V2_INIT: &str = "pub fn init(\n    spawner: Spawner,\n    controller: WifiController<'static>,\n    station: Interface<'static>,\n) -> Stack<'static> {\n";

/// An edited phase-1 ESP `wifi.rs` moved to the phase-2 `init`: the signature
/// takes the controller and the station half `main.rs` now creates, the line
/// that created them goes, and `interfaces.station` becomes `station`. Only
/// when all three are still there exactly as phase 1 wrote them.
fn migrate_wifi_esp_v1(existing: &str) -> Option<String> {
    let text = existing.replace("\r\n", "\n");
    if text.matches(V1_INIT).count() != 1 || text.matches(V1_NEW).count() != 1 {
        return None;
    }
    let out = text
        .replacen(V1_INIT, V2_INIT, 1)
        .replacen(V1_NEW, "", 1)
        .replace("interfaces.station", "station");
    // `Interface` and `WifiController` were imported in phase 1 already.
    out.contains("Interface").then_some(out)
}

/// Is `content` a `wifi.rs` that still creates the radio itself (phase 1),
/// edited so far that [`upgraded`] could not move it? Then `main.rs`, which
/// creates the radio now, no longer compiles against it.
pub fn phase_one_wifi_left(content: &str) -> bool {
    content.contains("esp_hal::peripherals::WIFI<'static>")
}

/// The PEM text `mqtt_ca.rs` holds: its `CA_PEM` literal. `None` when the
/// user turned it into something the tab cannot read.
pub fn ca_pem_of(content: &str) -> Option<&str> {
    let at = content.find("CA_PEM")?;
    let rest = &content[at..];
    let open = rest.find('"')? + 1;
    let close = open + rest[open..].find('"')?;
    Some(&rest[open..close])
}

/// What no longer fits the tab's TLS switch, among MQTT's files the user
/// edited (so the IDE left them): `mqtt.rs` from before `mqtt_link.rs`, which
/// talks to its socket itself - TLS never reaches it; or a `mqtt_link.rs`
/// plain while TLS is on, TLS while it is off (its crates are gone then), or
/// TLS for the other radio. The warning for the tab, or `None`.
pub fn mqtt_files_left(
    mqtt: Option<&str>,
    link: Option<&str>,
    tls: bool,
    platform: Platform,
) -> Option<&'static str> {
    if tls && mqtt.is_some_and(|m| !m.contains("mqtt_link")) {
        return Some(
            "src/pins/configs/mqtt.rs is from before TLS and talks to its socket itself, so TLS never \
             reaches it - and you edited it, so the IDE left it. Move your changes into the current \
             template (delete the file to get it).",
        );
    }
    let link = link?;
    let has_tls = link.contains("embedded_tls::");
    let other_radio = match platform {
        Platform::Esp => link.contains("embassy_rp::"),
        Platform::Cyw43 => link.contains("esp_hal::"),
        Platform::Nrf => false,
    };
    if has_tls != tls || (tls && other_radio) {
        return Some(
            "src/pins/configs/mqtt_link.rs was written for another TLS setting or radio, and you \
             edited it, so the IDE left it: it does not fit this project. Delete it to get the \
             current one.",
        );
    }
    None
}

/// The harnesses' secrets.rs with the dummy client identity in it
/// (`iot::test_pki`): an empty one would build no signing code at all.
#[cfg(test)]
pub(crate) fn seed_test_identity(files: &mut [(String, String)]) {
    use crate::panels::mcu_module::iot::test_pki;
    if let Some(secrets) = files.iter_mut().find(|(p, _)| p == SECRETS_PATH) {
        secrets.1 = write_secret(&secrets.1, CLIENT_SECRETS[0], test_pki::CERT);
        secrets.1 = write_secret(&secrets.1, CLIENT_SECRETS[1], test_pki::KEY_SEC1);
    }
}

/// An edited mqtt_link.rs from before the client certificate, which the IDE
/// left: with the certificate on, its generated block calls what that tail
/// does not have.
pub fn client_link_left(link: Option<&str>, client: bool) -> Option<&'static str> {
    (client && link.is_some_and(|l| l.contains("embedded_tls::") && !l.contains("fn client_cert(")))
        .then_some(
            "src/pins/configs/mqtt_link.rs is from before the client certificate, and you edited it, \
             so the IDE left it: it cannot send the certificate. Move your changes into the current \
             template (delete the file to get it).",
        )
}

/// Is `content` the file `name` written for another radio than `platform`'s,
/// and edited since, so [`upgraded`] left it? Then it no longer compiles
/// against the `main.rs` this chip gets. Read off the crates each template
/// names - the ESP and the Pico W share `ble.rs`'s editable half, so only the
/// nRF's is told apart there.
pub fn foreign_radio_file(name: &str, content: &str, platform: Platform) -> bool {
    match name {
        BLE => content.contains("nrf_sdc::") != (platform == Platform::Nrf),
        THREAD => content.contains("openthread::nrf::") != (platform == Platform::Nrf),
        WIFI => match platform {
            Platform::Esp => content.contains("cyw43::"),
            Platform::Cyw43 => content.contains("esp_radio::"),
            Platform::Nrf => false,
        },
        _ => false,
    }
}

/// The editable half of a generated file, as written: what follows the end
/// marker's own newline, line endings normalised.
fn tail_of(content: &str) -> Option<String> {
    let (_, tail) = content.split_once(GEN_END_CFG)?;
    let tail = tail.replace("\r\n", "\n");
    Some(tail.strip_prefix('\n').unwrap_or(&tail).to_owned())
}

/// The `pins/configs/` paths the tree must keep although `files` no longer
/// generates them: an IoT file the user wrote in - above all `secrets.rs`
/// with a password in it. A pristine one goes like any pruned config file.
pub fn kept_paths(files: &[(String, String)], tree: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for name in [NET, WIFI, MQTT, MQTT_LINK, MQTT_CA, SNTP, ESPNOW, BLE, THREAD, SECRETS, PANIC] {
        if files.iter().any(|(n, _)| n == name) {
            continue;
        }
        let path = format!("src/pins/configs/{name}");
        if tree
            .iter()
            .any(|(p, content)| *p == path && !is_pristine(name, content))
        {
            out.push(path);
        }
    }
    out
}

// ── Restart on panic ─────────────────────────────────────────────────────────

/// Does a USART module use the ESP's UART0? Then it is the user's line, and
/// "Restart on panic" prints nothing on it (esp-println's fallback port).
fn esp_uart0_taken(mcu: &Mcu) -> bool {
    use crate::panels::mcu_module::PinFunction;
    mcu.iter_all_pins()
        .filter(|p| !p.reserved)
        .any(|p| matches!(p.selected_function, PinFunction::UsartRx(0) | PinFunction::UsartTx(0)))
}

/// The `publish` panic.rs's MQTT report calls, as mqtt.rs's template has it:
/// the tab warns when an edited mqtt.rs no longer does.
pub const MQTT_PUBLISH_SIGNATURE: &str =
    "pub async fn publish(topic: &str, payload: &[u8]) -> Result<(), PublishError>";

/// Is the chip's USB Serial/JTAG the user's (its pads wired, not as OTG)?
/// esp-println would write there too once a host is attached.
fn esp_usb_jtag_taken(mcu: &Mcu) -> bool {
    use crate::panels::mcu_module::PinFunction;
    use crate::panels::mcu_module::codegen_esp::{has_usb_otg, has_usb_serial_jtag};
    let pads = mcu
        .iter_all_pins()
        .filter(|p| !p.reserved)
        .any(|p| matches!(p.selected_function, PinFunction::UsbDm | PinFunction::UsbDp));
    let otg = super::family::esp_usb_configs(mcu)
        .get(&1)
        .is_some_and(|c| c.role.is_otg())
        && has_usb_otg(&mcu.family);
    pads && !otg && has_usb_serial_jtag(&mcu.family)
}

/// Where the panic report goes on `mcu`, for the tab's note: the serial
/// port, MQTT, or nowhere but `last()`.
pub fn panic_sinks(mcu: &Mcu, active: Active) -> (bool, Option<String>) {
    let serial =
        active.platform == Platform::Esp && !esp_uart0_taken(mcu) && !esp_usb_jtag_taken(mcu);
    let mqtt = active
        .mqtt
        .then(|| mcu.iot.mqtt.as_ref().map(iot::panic_topic))
        .flatten();
    (serial, mqtt)
}

/// `panic.rs`: the chip's pieces of "Restart on panic" - where the record
/// lives, how the chip waits and restarts, where the report goes - above
/// the tail every platform shares.
fn panic_file(mcu: &Mcu, active: Active) -> String {
    let (serial, topic) = panic_sinks(mcu, active);
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// Restart on panic (from the IoT tab) — auto-updated; switch it off in the tab.\n");
    match active.platform {
        Platform::Esp => o.push_str(&esp_panic_pieces(&mcu.family)),
        Platform::Cyw43 => o.push_str(RP_PANIC_PIECES),
        Platform::Nrf => o.push_str(NRF_PANIC_PIECES),
    }
    if serial {
        o.push_str("/// At the panic: the serial port (esp-println).\n");
        o.push_str("fn announce(text: &str, wait_ms: u32) {\n");
        o.push_str("    esp_println::println!(\"panic: {text} - restarting in {wait_ms} ms\");\n");
        o.push_str("}\n");
    } else {
        o.push_str("/// At the panic: nothing to print on.\n");
        o.push_str("fn announce(_text: &str, _wait_ms: u32) {}\n");
    }
    let spawner = if topic.is_some() { "spawner" } else { "_spawner" };
    let report = if serial || topic.is_some() { "report" } else { "_report" };
    o.push_str(match (serial, topic.is_some()) {
        (true, true) => "/// At the next boot: the serial port, and MQTT once it connects.\n",
        (true, false) => "/// At the next boot: the serial port.\n",
        (false, true) => "/// At the next boot: MQTT, once it connects.\n",
        (false, false) => "/// At the next boot: nowhere by itself - `last()` has it for your code.\n",
    });
    o.push_str(&format!(
        "fn report_at_boot({spawner}: embassy_executor::Spawner, {report}: Report) {{\n"
    ));
    if serial {
        o.push_str("    esp_println::println!(\"{report}\");\n");
    }
    if topic.is_some() {
        o.push_str("    if let Ok(task) = report_task(report) {\n");
        o.push_str("        spawner.spawn(task);\n");
        o.push_str("    }\n");
    }
    o.push_str("}\n");
    if let Some(topic) = &topic {
        o.push_str("/// The report's MQTT topic. QoS 0 and not retained: a client subscribed\n");
        o.push_str("/// when the next connection is up sees it.\n");
        o.push_str(&format!("pub const REPORT_TOPIC: &str = {};\n", lit(topic)));
        o.push_str("#[embassy_executor::task]\n");
        o.push_str("async fn report_task(report: Report) {\n");
        o.push_str("    let mut text: heapless::String<256> = heapless::String::new();\n");
        o.push_str("    let _ = write!(text, \"{report}\");\n");
        o.push_str("    let _ = super::mqtt::publish(REPORT_TOPIC, text.as_bytes()).await;\n");
        o.push_str("}\n");
    }
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(&lf(PANIC_TAIL));
    o
}

/// The ESP's record, wait and restart. The record goes in RTC memory, which
/// a software reset keeps and esp-hal clears at power-on: RTC SLOW on the
/// dual-core parts (RTC FAST is the first core's alone), RTC FAST on the
/// RISC-V ones. The C2 and C61 have neither: RAM no start-up code clears,
/// with room after it for a stack that overflows into it.
fn esp_panic_pieces(family: &str) -> String {
    let mut o = String::new();
    let rtc = match family {
        "esp32" | "esp32s2" | "esp32s3" => Some("rtc_slow"),
        "esp32c3" | "esp32c5" | "esp32c6" | "esp32h2" => Some("rtc_fast"),
        _ => None,
    };
    if let Some(region) = rtc {
        o.push_str(&format!(
            "/// The record: RTC {} memory, which a software reset keeps and a\n/// power-on clears.\n",
            if region == "rtc_slow" { "SLOW" } else { "FAST" }
        ));
        o.push_str(&format!("#[esp_hal::ram(unstable({region}, persistent))]\n"));
        o.push_str("static mut RECORD: [u32; RECORD_WORDS] = [0; RECORD_WORDS];\n");
        o.push_str("fn record() -> *mut [u32; RECORD_WORDS] {\n");
        o.push_str("    &raw mut RECORD\n");
        o.push_str("}\n");
    } else {
        o.push_str("/// The record: RAM no start-up code clears (this chip has no RTC memory).\n");
        o.push_str("#[unsafe(link_section = \".noinit.rustonchip_panic\")]\n");
        o.push_str("static mut RECORD: core::mem::MaybeUninit<[u32; RECORD_WORDS]> = core::mem::MaybeUninit::uninit();\n");
        o.push_str("/// Linked after the record, before the stack: an overflowing stack's panic\n");
        o.push_str("/// runs here rather than over the record.\n");
        o.push_str("#[unsafe(link_section = \".uninit.rustonchip_panic\")]\n");
        o.push_str("static mut ROOM: core::mem::MaybeUninit<[u8; 1536]> = core::mem::MaybeUninit::uninit();\n");
        o.push_str("fn record() -> *mut [u32; RECORD_WORDS] {\n");
        o.push_str("    core::hint::black_box(&raw const ROOM);\n");
        o.push_str("    (&raw mut RECORD).cast()\n");
        o.push_str("}\n");
    }
    o.push_str(ESP_PANIC_PIECES);
    o
}

const ESP_PANIC_PIECES: &str = "\
/// Interrupts off on this core, for good: nothing else runs while it waits.
fn interrupts_off() {
    // SAFETY: never released - the chip restarts.
    let _ = unsafe { critical_section::acquire() };
}
fn uptime_ms() -> u64 {
    esp_hal::time::Instant::now().duration_since_epoch().as_millis()
}
fn wait_ms(ms: u32) {
    // Before esp_hal::init its timer may not run yet: then no wait at all.
    // (`black_box`: an empty loop is optimised away, and the two readings
    // would fall in the same microsecond.)
    let t0 = esp_hal::time::Instant::now();
    for i in 0..10_000u32 {
        core::hint::black_box(i);
    }
    if esp_hal::time::Instant::now() != t0 {
        esp_hal::delay::Delay::new().delay_millis(ms);
    }
}
fn reset() -> ! {
    esp_hal::system::software_reset()
}
/// Can the record be this boot's: a software reset - `reset()`'s - or a
/// watchdog that cut the wait short? Not power, the reset button or a new
/// firmware flashed over USB, which leave RTC memory as it was.
fn restart_kept() -> bool {
    matches!(
        esp_hal::system::reset_reason().map(|r| r as u32),
        Some(0x03 | 0x0C | 0x07 | 0x08 | 0x09 | 0x0B | 0x0D | 0x10 | 0x11 | 0x12)
    )
}
";

/// The Pico W's: RAM no start-up code clears (`.uninit`), the core reset.
const RP_PANIC_PIECES: &str = "\
/// The record: RAM no start-up code clears. A software reset keeps it; at
/// power-on it holds noise, which the checksum turns away.
#[unsafe(link_section = \".uninit.rustonchip_panic\")]
static mut RECORD: core::mem::MaybeUninit<[u32; RECORD_WORDS]> = core::mem::MaybeUninit::uninit();
fn record() -> *mut [u32; RECORD_WORDS] {
    (&raw mut RECORD).cast()
}
/// Interrupts off, for good: nothing else runs while it waits.
fn interrupts_off() {
    cortex_m::interrupt::disable();
}
fn uptime_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}
fn wait_ms(ms: u32) {
    // A watchdog left running would end the wait early, and its reset clears
    // the RAM the record is in.
    // SAFETY: nothing else uses the watchdog any more.
    embassy_rp::watchdog::Watchdog::new(unsafe { embassy_rp::peripherals::WATCHDOG::steal() }).stop();
    // Before embassy_rp::init the timer is held in reset: then no wait at all.
    let t0 = embassy_time::Instant::now();
    cortex_m::asm::delay(10_000);
    if embassy_time::Instant::now() != t0 {
        embassy_time::block_for(embassy_time::Duration::from_millis(u64::from(ms)));
    }
}
fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}
/// Can the record be this boot's? A core reset says nothing of its cause
/// here: the record's mark (this firmware's) and checksum decide alone.
fn restart_kept() -> bool {
    true
}
";

/// The nRF52's: RAM a soft reset keeps (`.uninit`), the wait counted in CPU
/// cycles - embassy-time's clock needs an interrupt to move on.
const NRF_PANIC_PIECES: &str = "\
/// The record: RAM no start-up code clears. A soft reset keeps it; at
/// power-on it holds noise, which the checksum turns away.
#[unsafe(link_section = \".uninit.rustonchip_panic\")]
static mut RECORD: core::mem::MaybeUninit<[u32; RECORD_WORDS]> = core::mem::MaybeUninit::uninit();
fn record() -> *mut [u32; RECORD_WORDS] {
    (&raw mut RECORD).cast()
}
/// Interrupts off, for good: nothing else runs while it waits.
fn interrupts_off() {
    cortex_m::interrupt::disable();
}
fn uptime_ms() -> u64 {
    embassy_time::Instant::now().as_millis()
}
fn wait_ms(ms: u32) {
    // In CPU cycles (64 MHz): embassy-time's clock needs an interrupt.
    // SAFETY: only the cycle counter is touched, with interrupts off.
    let mut core = unsafe { cortex_m::Peripherals::steal() };
    core.DCB.enable_trace();
    core.DWT.enable_cycle_counter();
    for _ in 0..ms {
        // A watchdog started before the panic keeps running through a soft
        // reset: fed, the wait ends in the restart rather than in its reset,
        // which may corrupt the RAM the record is in.
        for n in 0..8 {
            // SAFETY: a reload request; a register the watchdog does not use
            // ignores it.
            unsafe { embassy_nrf::wdt::WatchdogHandle::steal::<embassy_nrf::peripherals::WDT>(n) }.pet();
        }
        let t0 = cortex_m::peripheral::DWT::cycle_count();
        while cortex_m::peripheral::DWT::cycle_count().wrapping_sub(t0) < 64_000 {}
    }
}
fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}
/// Can the record be this boot's? A core reset says nothing of its cause
/// here: the record's mark (this firmware's) and checksum decide alone.
fn restart_kept() -> bool {
    true
}
";

/// The comment above the handler "Restart on panic" puts in main.rs's header.
const PANIC_HANDLER_NOTE: &str =
    "// Restart on panic (IoT tab): src/pins/configs/panic.rs does the work.\n";
/// The handler itself.
const PANIC_HANDLER_ITEM: &str = concat!(
    "#[panic_handler]\n",
    "fn panic(info: &core::panic::PanicInfo) -> ! {\n",
    "    crate::pins::configs::panic::restart(info)\n",
    "}\n",
);
/// The Cortex-M families' own handler: panic-halt's.
const PANIC_HALT_LINE: &str = "use panic_halt as _;\n";

/// What main.rs's header holds for a panic handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PanicHeader {
    /// The family's own halting one, as the IDE writes it.
    Halt,
    /// "Restart on panic"'s, as the IDE writes it.
    Restart,
    /// Anything else: the user's own (`panic_probe`, a handler that prints),
    /// or an edited copy. The IDE leaves it.
    Other,
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect()
}

/// The `#[panic_handler]` item in `head`: from its attribute's line through
/// the newline after the closing brace.
fn handler_item(head: &str) -> Option<(usize, usize)> {
    let mut start = None;
    let mut offset = 0;
    let mut comment = 0usize;
    for line in head.split_inclusive('\n') {
        if comment == 0 && line.trim() == "#[panic_handler]" {
            start = Some(offset);
            break;
        }
        comment = block_comment_depth(comment, line);
        offset += line.len();
    }
    let start = start?;
    let open = start + head.get(start..)?.find('{')?;
    let mut depth = 0usize;
    for (i, c) in head.get(open..)?.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = open + i + 1;
                    let end = if head.get(end..)?.starts_with('\n') { end + 1 } else { end };
                    return Some((start, end));
                }
            }
            _ => {}
        }
    }
    None
}

/// `use panic_halt as _;` on a line of its own, with no attribute over it
/// (a `#[cfg(..)]` would then govern whatever replaced it).
fn halt_line(head: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    let mut above = "";
    let mut comment = 0usize;
    for line in head.split_inclusive('\n') {
        if comment == 0
            && line.trim() == PANIC_HALT_LINE.trim()
            && !above.trim_start().starts_with("#[")
        {
            return Some((offset, offset + line.len()));
        }
        comment = block_comment_depth(comment, line);
        if !line.trim().is_empty() {
            above = line;
        }
        offset += line.len();
    }
    None
}

/// The `/* */` nesting after `line`, from `depth` before it - so a handler
/// commented out in the header is not taken for the live one. Text after a
/// `//` does not count.
fn block_comment_depth(depth: usize, line: &str) -> usize {
    let code = if depth == 0 {
        line.find("//").map_or(line, |i| &line[..i])
    } else {
        line
    };
    let opened = depth + code.matches("/*").count();
    opened.saturating_sub(code.matches("*/").count())
}

/// The start of the handler's note when the line above `start` is it.
fn with_note(head: &str, start: usize) -> usize {
    let above = head.get(..start).unwrap_or("");
    let line_start = above.trim_end_matches('\n').rfind('\n').map_or(0, |i| i + 1);
    match above.get(line_start..) {
        Some(line) if line.trim() == PANIC_HANDLER_NOTE.trim() => line_start,
        _ => start,
    }
}

/// The part of `main_rs` above the generated block - its header.
fn header_of(main_rs: &str) -> &str {
    main_rs.find(super::GEN_BEGIN).map_or(main_rs, |i| &main_rs[..i])
}

/// Does `main_rs`'s header still call into panic.rs (the tab's warning once
/// it is switched off)? The generated block's `start` line does not count.
pub fn header_calls_panic(main_rs: &str) -> bool {
    header_of(main_rs).contains("pins::configs::panic::")
}

/// What `main_rs`'s header holds for a panic handler (the tab's warnings).
pub fn panic_header(main_rs: &str) -> PanicHeader {
    let head = header_of(main_rs);
    match handler_item(head) {
        Some((s, e)) => {
            let item = squash(&head[s..e]);
            if item == squash(crate::panels::mcu_module::codegen_esp::PANIC_HALT) {
                PanicHeader::Halt
            } else if item == squash(PANIC_HANDLER_ITEM) {
                PanicHeader::Restart
            } else {
                PanicHeader::Other
            }
        }
        None if halt_line(head).is_some() => PanicHeader::Halt,
        None => PanicHeader::Other,
    }
}

/// `code` (a main.rs) with its header's panic handler as the IoT tab says:
/// "Restart on panic"'s while it is in effect, the family's own halt
/// otherwise (switched off, the runtime Blocking, every link off). The header
/// is the user's: only a handler exactly as the IDE writes one is swapped -
/// whitespace aside - and anything else is left, for the tab to point out.
pub fn panic_main(code: String, mcu: &Mcu) -> String {
    let Some(begin) = code.find(super::GEN_BEGIN) else {
        return code;
    };
    let head = &code[..begin];
    let halt = crate::panels::mcu_module::codegen_esp::PANIC_HALT;
    let swap = if iot::active(mcu).is_some_and(|a| a.panic) {
        match handler_item(head) {
            Some((s, e)) if squash(&head[s..e]) == squash(halt) => Some((s, e)),
            Some(_) => None,
            None => halt_line(head),
        }
        .map(|span| (span, format!("{PANIC_HANDLER_NOTE}{PANIC_HANDLER_ITEM}")))
    } else {
        handler_item(head)
            .filter(|&(s, e)| squash(&head[s..e]) == squash(PANIC_HANDLER_ITEM))
            .map(|(s, e)| {
                let back = if mcu.family.starts_with("esp32") { halt } else { PANIC_HALT_LINE };
                ((with_note(head, s), e), back.to_owned())
            })
    };
    match swap {
        Some(((s, e), with)) => format!("{}{with}{}", &code[..s], &code[e..]),
        None => code,
    }
}

/// The line main.rs calls right after the HAL is up, while "Restart on
/// panic" is in effect: a panic before this boot is reported now. Empty
/// otherwise.
pub fn panic_start_lines(mcu: &Mcu) -> String {
    if !iot::active(mcu).is_some_and(|a| a.panic) {
        return String::new();
    }
    concat!(
        "    // Restart on panic (IoT tab): a panic that restarted the chip is reported now.\n",
        "    pins::configs::panic::start(spawner);\n",
    )
    .to_owned()
}

// ── main.rs ──────────────────────────────────────────────────────────────────

/// What the ESP generator's block looks like where the heap must go in: the
/// scheduler start. esp-rtos and esp-radio allocate from the heap, so it has
/// to exist before `esp_rtos::start`, as the esp-radio docs order it.
const ESP_START_MARK: &str = "\n    // ── Async runtime (esp-rtos drives the embassy executor) ──\n";

/// The ESP lines, spliced into the GENERATED block of an already generated
/// `main.rs` - a no-op unless the IoT code is active. Done here rather than
/// by threading two more arguments through twenty call sites of the ESP
/// generator: the three places it touches are each fixed text of that
/// generator, and the tests below pin all three.
pub fn esp_main(code: String, mcu: &Mcu) -> String {
    match iot::active(mcu) {
        Some(active) if active.platform == Platform::Esp => {
            let code = esp_main_with(code, mcu.iot.heap_kib, active, &mcu.family);
            esp_panic_start(code, &panic_start_lines(mcu))
        }
        _ => code,
    }
}

/// The ESP's line of the scheduler start the panic report follows: the
/// report needs the spawner running, nothing else.
const ESP_RTOS_START: &str = "    esp_rtos::start(timg0.timer0, sw_int.software_interrupt0);\n";

/// `lines` right after the scheduler starts, inside the generated block.
fn esp_panic_start(code: String, lines: &str) -> String {
    use super::{GEN_BEGIN, GEN_END};
    if lines.is_empty() {
        return code;
    }
    let (Some(begin), Some(end)) = (code.find(GEN_BEGIN), code.find(GEN_END)) else {
        return code;
    };
    match code[begin..end].find(ESP_RTOS_START) {
        Some(at) => {
            let at = begin + at + ESP_RTOS_START.len();
            format!("{}{lines}{}", &code[..at], &code[at..])
        }
        None => code,
    }
}

/// Wi-Fi (or ESP-NOW) and Bluetooth at once: coexistence, which needs esp-radio's
/// `coex` and a larger heap.
pub fn coex(active: Active) -> bool {
    active.ble && (active.station || active.esp_now)
}

/// The heap lines for `active`. Bluetooth beside Wi-Fi takes two regions, the
/// first in the RAM the bootloader used (free once the app runs): 64 + 64 KiB
/// on the RISC-V parts, 96 + 24 on the ESP32, where 96 + 72 still links but
/// leaves 6 KiB of stack (measured). Anything else is the tab's one region.
pub fn esp_heap_lines(heap_kib: u32, active: Active, family: &str) -> String {
    let mut o = String::from(
        "\n    // ── Heap (IoT tab) ──\n    // esp-radio allocates its buffers here, so it has to exist before the\n    // scheduler starts.\n",
    );
    if active.mqtt_tls && family == "esp32s2" {
        // The S2's DRAM is full with TLS's record buffers: all of its heap goes
        // in the RAM the bootloader used (136 KiB there), 32 KiB more for the
        // RSA checks.
        o.push_str("    // MQTT over TLS fills the S2's DRAM: the heap goes in the RAM the\n");
        o.push_str("    // bootloader used, 32 KiB more for checking the broker's RSA chain.\n");
        o.push_str(&format!(
            "    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: {} * 1024);\n",
            heap_kib.max(1) + 32
        ));
        return o;
    }
    if coex(active) {
        // MQTT over TLS checks the broker's RSA chain on the heap too (about
        // 22 KB at its peak): 32 KiB more on the RISC-V parts. The ESP32 and
        // the C2 have no room for it (`iot::tls_available`).
        let (reclaimed, more, share) = if family == "esp32" {
            (96, 24, "most")
        } else if active.mqtt_tls {
            (64, 96, "half")
        } else {
            (64, 64, "half")
        };
        o.push_str("    // Wi-Fi beside Bluetooth (coex) needs more: the RAM the bootloader used\n");
        o.push_str(&format!("    // is free once the app runs, and carries {share} of it.\n"));
        o.push_str(&format!(
            "    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: {reclaimed} * 1024);\n"
        ));
        o.push_str(&format!("    esp_alloc::heap_allocator!(size: {more} * 1024);\n"));
    } else {
        if active.mqtt_tls {
            o.push_str("    // MQTT over TLS checks the broker's RSA chain on the heap (about 22 KB at\n");
            o.push_str("    // its peak): 32 KiB more, in the RAM the bootloader used.\n");
            o.push_str("    esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 32 * 1024);\n");
        }
        o.push_str(&format!(
            "    esp_alloc::heap_allocator!(size: {} * 1024);\n",
            heap_kib.max(1)
        ));
    }
    o
}

fn esp_main_with(code: String, heap_kib: u32, active: Active, family: &str) -> String {
    use super::{GEN_BEGIN, GEN_END};
    let (Some(begin), Some(end)) = (code.find(GEN_BEGIN), code.find(GEN_END)) else {
        return code;
    };
    let mut block = code[begin..end].to_owned();
    // 1. The heap, before the scheduler starts.
    let heap = esp_heap_lines(heap_kib, active, family);
    if let Some(at) = block.find(ESP_START_MARK) {
        block.insert_str(at, &heap);
    }
    // 2. The spawner is named: the IoT tasks are spawned on it.
    block = block.replacen(
        "async fn main(_spawner: Spawner)",
        "async fn main(spawner: Spawner)",
        1,
    );
    // 3. The bring-up, last in the block: it only needs `peripherals.BT`,
    //    `peripherals.WIFI` and `peripherals.IEEE802154`. One
    //    `esp_radio::wifi::new`: it hands out the station AND ESP-NOW, and a
    //    second call would fail. Thread is never beside them (`iot::active`).
    block.push_str("    // ── IoT (IoT tab) ──\n");
    if active.thread {
        block.push_str("    // Thread: OpenThread on the 802.15.4 radio, joining the network in\n");
        block.push_str("    // secrets.rs (THREAD_DATASET).\n");
        block.push_str("    pins::configs::thread::start(spawner, peripherals.IEEE802154);\n");
    }
    if active.ble {
        block.push_str("    // Bluetooth LE: the chip's own controller, on its public address.\n");
        block.push_str(
            "    let ble = esp_radio::ble::controller::BleConnector::new(peripherals.BT, Default::default()).unwrap();\n",
        );
        block.push_str("    pins::configs::ble::start(spawner, ble, None);\n");
    }
    if active.station || active.esp_now {
        block.push_str(
            "    let (wifi_controller, radio) = esp_radio::wifi::new(peripherals.WIFI, Default::default()).unwrap();\n",
        );
        if active.station {
            block.push_str(&stack_lines(active, "wifi_controller, radio.station"));
        } else {
            // ESP-NOW alone: someone has to keep the controller - dropping it
            // stops the radio, and ESP-NOW with it.
            block.push_str("    pins::configs::espnow::hold_radio(spawner, wifi_controller);\n");
        }
        if active.esp_now {
            block.push_str("    pins::configs::espnow::start(spawner, radio.esp_now);\n");
        }
    }
    block.push('\n');
    format!("{}{block}{}", &code[..begin], &code[end..])
}

/// `wifi::init` and what runs on the stack it returns. `args` are the radio
/// halves after `spawner`, which differ per radio.
fn stack_lines(active: Active, args: &str) -> String {
    let mut o = String::new();
    if !active.mqtt && !active.sntp {
        o.push_str("    // `net_stack` opens sockets: `embassy_net::tcp::TcpSocket::new(net_stack, ..)`.\n");
        o.push_str("    #[allow(unused_variables)]\n");
    }
    o.push_str(&format!(
        "    let net_stack = pins::configs::wifi::init(spawner, {args});\n"
    ));
    if active.mqtt {
        o.push_str("    pins::configs::mqtt::start(spawner, net_stack);\n");
    }
    if active.sntp {
        o.push_str("    pins::configs::sntp::start(spawner, net_stack);\n");
    }
    o
}

/// The Pico W lines that replace the LED's `let mut wl_led = control;` once
/// the radio carries Wi-Fi or Bluetooth.
///
/// Bluetooth first: its address is made of the radio's Wi-Fi MAC, read through
/// `control` before Wi-Fi takes `control` for good. Without Wi-Fi, `control`
/// stays in `main` as the LED's, as it does with no IoT at all.
pub fn cyw43_main_lines(active: Active) -> String {
    let mut o = String::new();
    o.push_str("    // ── IoT (IoT tab) ──\n");
    if active.ble {
        o.push_str("    // Bluetooth LE: the radio's Bluetooth half goes to the BLE task. Its address\n");
        o.push_str("    // is made of the radio's Wi-Fi MAC, so that is read before `control` moves on.\n");
        o.push_str("    let mac = control.address().await;\n");
        o.push_str("    pins::configs::ble::start(spawner, bt_device, Some(mac));\n");
    }
    if active.station {
        o.push_str("    // The radio's `control` belongs to the Wi-Fi task from here on, so the LED\n");
        o.push_str("    // (GPIO0 on the radio) is switched through it: `pins::configs::wifi::set_led(true)`.\n");
        o.push_str(&stack_lines(active, "net_device, control"));
    } else {
        o.push_str("    // The LED is GPIO0 ON THE RADIO, so it is driven through `control` rather\n");
        o.push_str("    // than through a pin: `wl_led.gpio_set(0, true).await` turns it on.\n");
        o.push_str("    #[allow(unused_mut, unused_variables)]\n");
        o.push_str("    let mut wl_led = control;\n");
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::mcu::model::Runtime;

    fn mcu(id: &str, runtime: Runtime, mqtt: bool) -> Mcu {
        let mut mcu = crate::panels::mcu_module::builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu();
        mcu.runtime = runtime;
        mcu.iot.wifi = true;
        mcu.iot.mqtt = mqtt.then(|| MqttConfig::for_chip(id));
        mcu
    }

    /// The files, each only while its switch is on.
    #[test]
    fn the_files_follow_the_tab() {
        let names = |m: &Mcu| -> Vec<String> {
            config_files_for(m).into_iter().map(|(n, _)| n).collect()
        };
        assert_eq!(
            names(&mcu("esp32c3", Runtime::Async, true)),
            ["net.rs", "wifi.rs", "secrets.rs", "mqtt.rs", "mqtt_link.rs", "panic.rs"]
        );
        assert_eq!(
            names(&mcu("esp32c3", Runtime::Async, false)),
            ["net.rs", "wifi.rs", "secrets.rs", "panic.rs"]
        );
        let mut all = mcu("esp32c3", Runtime::Async, true);
        all.iot.sntp = Some(SntpConfig::default());
        all.iot.esp_now = Some(EspNowConfig::default());
        assert_eq!(
            names(&all),
            ["net.rs", "wifi.rs", "secrets.rs", "mqtt.rs", "mqtt_link.rs", "sntp.rs", "espnow.rs", "panic.rs"]
        );
        // ESP-NOW alone: no stack, no credentials - and MQTT/SNTP, still ticked,
        // have nothing to run on.
        all.iot.wifi = false;
        assert_eq!(names(&all), ["espnow.rs", "panic.rs"]);
        // Restart on panic switched off: its file goes.
        all.iot.restart_on_panic = false;
        assert_eq!(names(&all), ["espnow.rs"]);
    }

    fn tls_mcu(id: &str, sntp: bool) -> Mcu {
        let mut m = mcu(id, Runtime::Async, true);
        if let Some(mq) = m.iot.mqtt.as_mut() {
            mq.tls = true;
            mq.port = mq.port_for_tls(true);
        }
        m.iot.sntp = sntp.then(SntpConfig::default);
        m
    }

    /// MQTT over TLS: mqtt.rs is the same, mqtt_link.rs becomes the chip's
    /// TLS link, mqtt_ca.rs the CA; the GENERATED block carries the check,
    /// the name and SNTP's clock - or none.
    #[test]
    fn tls_swaps_the_link_and_brings_the_ca() {
        let plain = mcu("esp32c3", Runtime::Async, true);
        let files = config_files_for(&plain);
        let link = &files.iter().find(|(n, _)| n == MQTT_LINK).unwrap().1;
        assert!(link.contains("pub type Link<'s> = TcpSocket<'s>;"), "{link}");
        assert!(!files.iter().any(|(n, _)| n == MQTT_CA));
        let plain_mqtt = files.iter().find(|(n, _)| n == MQTT).unwrap().1.clone();

        for (id, chip) in [("esp32c3", "esp_hal::rng::Trng"), ("rp2040_pico_w", "embassy_rp::clocks::RoscRng")] {
            for sntp in [false, true] {
                let m = tls_mcu(id, sntp);
                let a = iot::active(&m).unwrap();
                assert!(a.mqtt_tls, "{id}");
                let files = config_files_for(&m);
                let get = |n: &str| files.iter().find(|(f, _)| f == n).map(|(_, b)| b.clone());
                let link = get(MQTT_LINK).unwrap();
                for want in [
                    "pub const VERIFY: bool = true;",
                    "pub const SERVER_NAME: &str = \"test.mosquitto.org\";",
                    "pub type Link<'s> = TlsConnection<'s, Gate<'s>, Aes128GcmSha256>;",
                    "if VERIFY && !verifier.authenticated {",
                    "CertificateEntryRef::X509(der) if is_ca(der) => {}",
                    "use super::mqtt_ca::CA_PEM;",
                    // The gaps embedded-tls 0.19 leaves, closed: an empty
                    // encrypted record, an RSA-PSS signature from an EC key, a
                    // dead socket under a half record.
                    "if self.head[0] == 0x17 && len < 17 {",
                    "if pss && !self.leaf_rsa {",
                    "State::Established | State::FinWait1 | State::FinWait2",
                    "pem_der(CA_PEM, b\"CERTIFICATE\", true, false,",
                    "chain.entries.truncate(keep);",
                    chip,
                ] {
                    assert!(link.contains(want), "{id} sntp={sntp}: {want}:\n{link}");
                }
                assert_eq!(link.contains("super::sntp::now_unix()"), sntp, "{id}");
                assert!(is_pristine(MQTT_LINK, &link));
                let mq = get(MQTT).unwrap();
                assert!(mq.contains("pub const BROKER_PORT: u16 = 8886;"), "{mq}");
                assert_eq!(tail_of(&mq), tail_of(&plain_mqtt), "mqtt.rs's own half never changes");
                let ca = get(MQTT_CA).unwrap();
                assert!(ca.starts_with("//! The CA the MQTT broker's"), "{ca}");
                assert!(ca.contains("pub const CA_PEM: &str = \"-----BEGIN CERTIFICATE-----\nMIIFazCC"), "{ca}");
                assert!(ca.contains("-----END CERTIFICATE-----\";"), "{ca}");
                assert!(write_once(MQTT_CA) && is_pristine(MQTT_CA, &ca) && runtime_free(MQTT_CA));
                assert!(!is_pristine(MQTT_CA, &ca.replace("MIIFazCC", "MIIFazCD")));
                // The SOCKETS count is the plain one: TLS runs on MQTT's socket.
                let mut same = mcu(id, Runtime::Async, true);
                same.iot.sntp = sntp.then(SntpConfig::default);
                let net = config_files_for(&same).into_iter().find(|(n, _)| n == NET).map(|(_, b)| b);
                assert_eq!(get(NET), net, "{id} sntp={sntp}");
            }
        }
        // Encrypt only, and another name.
        let mut m = tls_mcu("esp32c3", false);
        if let Some(mq) = m.iot.mqtt.as_mut() {
            mq.verify = false;
            mq.server_name = "broker.example.org".into();
        }
        let link = config_files_for(&m).into_iter().find(|(n, _)| n == MQTT_LINK).unwrap().1;
        assert!(link.contains("pub const VERIFY: bool = false;"), "{link}");
        assert!(link.contains("pub const SERVER_NAME: &str = \"broker.example.org\";"), "{link}");
    }

    /// Where TLS cannot run (no TRNG on the C5 / C61) and it is asked for, no
    /// MQTT is generated: plain MQTT to a TLS port would send the password in
    /// clear.
    #[test]
    fn tls_where_the_chip_cannot_stays_plain() {
        for id in ["esp32c5", "esp32c61"] {
            let m = tls_mcu(id, false);
            let a = iot::active(&m).unwrap();
            assert!(!a.mqtt && !a.mqtt_tls, "{id}: never the password in clear to a TLS port");
            let files = config_files_for(&m);
            assert!(!files.iter().any(|(n, _)| n == MQTT || n == MQTT_LINK || n == MQTT_CA), "{id}");
            // Switched off, MQTT comes back plain.
            let mut plain = m.clone();
            if let Some(mq) = plain.iot.mqtt.as_mut() {
                mq.tls = false;
            }
            assert!(iot::active(&plain).is_some_and(|a| a.mqtt && !a.mqtt_tls), "{id}");
        }
    }

    /// The ESP heap grows with TLS: a reclaimed 32 KiB beside the tab's one,
    /// or 96 KiB instead of 64 beside Bluetooth on the RISC-V parts.
    #[test]
    fn tls_brings_heap_on_an_esp() {
        let code = tls_mcu("esp32c3", false).fresh_main_rs();
        assert!(code.contains("esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 32 * 1024);"), "{code}");
        assert!(code.contains("esp_alloc::heap_allocator!(size: 72 * 1024);"), "{code}");
        let mut coex = tls_mcu("esp32c3", false);
        coex.iot.ble = Some(BleConfig::default());
        let code = coex.fresh_main_rs();
        assert!(code.contains("#[esp_hal::ram(reclaimed)] size: 64 * 1024"), "{code}");
        assert!(code.contains("esp_alloc::heap_allocator!(size: 96 * 1024);"), "{code}");
        // The S2: all of it in the reclaimed RAM.
        let s2 = tls_mcu("esp32s2", false).fresh_main_rs();
        assert!(s2.contains("esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 104 * 1024);"), "{s2}");
        assert!(!s2.contains("esp_alloc::heap_allocator!(size:"), "{s2}");
        // The ESP32 with coex: no TLS, and its heap as before.
        let mut e32 = tls_mcu("esp32", false);
        e32.iot.ble = Some(BleConfig::default());
        let a = iot::active(&e32).unwrap();
        assert!(!a.mqtt_tls && !a.mqtt, "TLS asked for and impossible: no MQTT at all");
        assert!(e32.fresh_main_rs().contains("esp_alloc::heap_allocator!(size: 24 * 1024);"));
        // Plain MQTT: nothing more.
        let plain = mcu("esp32c3", Runtime::Async, true).fresh_main_rs();
        assert!(!plain.contains("size: 32 * 1024"), "{plain}");
    }

    /// The CA the tab reads out of mqtt_ca.rs, and the files that no longer fit
    /// the TLS switch after the user edited them.
    #[test]
    fn mqtt_files_are_read_back_for_the_tab() {
        let ca = mqtt_ca_file();
        assert!(ca_pem_of(&ca).is_some_and(|p| p.starts_with("-----BEGIN CERTIFICATE-----")));
        assert!(iot::ca_summary(ca_pem_of(&ca).unwrap()).is_ok());
        assert_eq!(ca_pem_of("pub const OTHER: u8 = 1;"), None);

        let v1 = include_str!("iot_templates/legacy/mqtt_v1.rs");
        let v2 = lf(MQTT_TAIL);
        let plain = lf(LINK_PLAIN_TAIL);
        let esp = lf(LINK_TLS_ESP_TAIL);
        let pico = lf(LINK_TLS_CYW43_TAIL);
        assert_eq!(mqtt_files_left(Some(&v2), Some(&plain), false, Platform::Esp), None);
        assert_eq!(mqtt_files_left(Some(&v2), Some(&esp), true, Platform::Esp), None);
        assert_eq!(mqtt_files_left(Some(&v2), Some(&pico), true, Platform::Cyw43), None);
        assert_eq!(mqtt_files_left(Some(v1), None, false, Platform::Esp), None, "v1 still works plain");
        assert!(mqtt_files_left(Some(v1), Some(&esp), true, Platform::Esp).is_some());
        assert!(mqtt_files_left(Some(&v2), Some(&plain), true, Platform::Esp).is_some());
        assert!(mqtt_files_left(Some(&v2), Some(&esp), false, Platform::Esp).is_some());
        assert!(mqtt_files_left(Some(&v2), Some(&esp), true, Platform::Cyw43).is_some());
        assert!(mqtt_files_left(Some(&v2), Some(&pico), true, Platform::Esp).is_some());
    }

    /// An untouched phase-1..4 mqtt.rs moves to the link-agnostic one; the
    /// link swaps between plain and TLS, and between the ESP and the Pico,
    /// while untouched; an edited one stays.
    #[test]
    fn mqtt_rs_and_its_link_follow_the_tab() {
        let m = MqttConfig::default();
        let v2 = mqtt_file(&m);
        let v1 = format!(
            "{}{}",
            &v2[..v2.find(GEN_END_CFG).unwrap() + GEN_END_CFG.len() + 1],
            include_str!("iot_templates/legacy/mqtt_v1.rs")
        );
        assert!(!v1.contains("mqtt_link"), "v1 talks to its socket itself");
        assert_eq!(upgraded(MQTT, &v1, &v2).as_deref(), Some(v2.as_str()));
        assert!(is_pristine(MQTT, &v1));

        let plain = config_files_for(&mcu("esp32c3", Runtime::Async, true));
        let plain = plain.iter().find(|(n, _)| n == MQTT_LINK).unwrap().1.clone();
        let esp = config_files_for(&tls_mcu("esp32c3", false));
        let esp = esp.iter().find(|(n, _)| n == MQTT_LINK).unwrap().1.clone();
        let pico = config_files_for(&tls_mcu("rp2040_pico_w", false));
        let pico = pico.iter().find(|(n, _)| n == MQTT_LINK).unwrap().1.clone();
        assert_eq!(upgraded(MQTT_LINK, &plain, &esp).as_deref(), Some(esp.as_str()));
        assert_eq!(upgraded(MQTT_LINK, &esp, &plain).as_deref(), Some(plain.as_str()));
        assert_eq!(upgraded(MQTT_LINK, &esp, &pico).as_deref(), Some(pico.as_str()));
        let edited = format!("{plain}\n// mine\n");
        assert_eq!(upgraded(MQTT_LINK, &edited, &esp), None);
    }

    /// ESP-NOW is Espressif's: on a Pico W the switch generates nothing.
    #[test]
    fn esp_now_is_esp_only() {
        let mut m = mcu("rp2040_pico_w", Runtime::Async, false);
        m.iot.wifi = false;
        m.iot.esp_now = Some(EspNowConfig::default());
        assert!(config_files_for(&m).is_empty());
        assert!(iot::active(&m).is_none());
    }

    /// One socket per protocol plus DHCP, DNS and one spare - one short is a
    /// panic in smoltcp the first time the user opens their own.
    #[test]
    fn the_socket_count_follows_the_protocols() {
        let mut m = mcu("esp32c3", Runtime::Async, false);
        let count = |m: &Mcu| {
            let net = config_files_for(m)
                .into_iter()
                .find(|(n, _)| n == NET)
                .map(|(_, b)| b)
                .unwrap();
            let line = net.lines().find(|l| l.starts_with("pub const SOCKETS")).unwrap().to_owned();
            line
        };
        // Never fewer than phase 1's 4: code opening two sockets keeps working.
        assert_eq!(count(&m), "pub const SOCKETS: usize = 4;");
        m.iot.mqtt = Some(MqttConfig::default());
        assert_eq!(count(&m), "pub const SOCKETS: usize = 5;");
        m.iot.sntp = Some(SntpConfig::default());
        assert_eq!(count(&m), "pub const SOCKETS: usize = 6;");
    }

    /// The tab's ESP-NOW values reach the constants, an address that cannot be
    /// a peer is left out, and a channel out of range is clamped.
    #[test]
    fn esp_now_settings_reach_the_constants() {
        let f = espnow_file(&EspNowConfig {
            channel: 20,
            peers: vec![[0x24, 0x0A, 0xC4, 0x12, 0x34, 0x56], [0xFF; 6]],
        });
        assert!(f.contains("pub const CHANNEL: u8 = 13;"), "{f}");
        assert!(
            f.contains("pub const PEERS: &[[u8; 6]] = &[[0x24, 0x0A, 0xC4, 0x12, 0x34, 0x56]];"),
            "{f}"
        );
        let empty = espnow_file(&EspNowConfig::default());
        assert!(empty.contains("pub const PEERS: &[[u8; 6]] = &[];"), "{empty}");
    }

    #[test]
    fn sntp_settings_reach_the_constants() {
        let f = sntp_file(&SntpConfig {
            server: " time.example \"x\" ".into(),
            interval_s: 5,
        });
        assert!(f.contains(r#"pub const SERVER: &str = "time.example \"x\"";"#), "{f}");
        // Public pools refuse faster polling: clamped to their minimum.
        assert!(
            f.contains(&format!("pub const INTERVAL_S: u64 = {};", iot::MIN_SNTP_INTERVAL_S)),
            "{f}"
        );
    }

    /// The ESP block in every shape: one radio, then the station and what
    /// runs on it, then ESP-NOW - or the radio held for ESP-NOW alone.
    #[test]
    fn esp_main_starts_what_the_tab_switched_on() {
        let mut m = mcu("esp32c3", Runtime::Async, true);
        m.iot.sntp = Some(SntpConfig::default());
        m.iot.esp_now = Some(EspNowConfig::default());
        let code = m.fresh_main_rs();
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s} missing:\n{code}"));
        assert_eq!(code.matches("esp_radio::wifi::new(").count(), 1, "{code}");
        assert!(at("esp_radio::wifi::new(") < at("wifi::init(spawner, wifi_controller, radio.station)"));
        assert!(at("mqtt::start(spawner, net_stack)") < at("sntp::start(spawner, net_stack)"));
        assert!(at("sntp::start(") < at("espnow::start(spawner, radio.esp_now)"));
        assert!(!code.contains("hold_radio"), "the station keeps the controller:\n{code}");
        assert!(!code.contains("#[allow(unused_variables)]\n    let net_stack"), "{code}");

        m.iot.wifi = false;
        let alone = m.fresh_main_rs();
        assert!(alone.contains("pins::configs::espnow::hold_radio(spawner, wifi_controller);"), "{alone}");
        assert!(alone.contains("pins::configs::espnow::start(spawner, radio.esp_now);"), "{alone}");
        assert!(!alone.contains("net_stack"), "{alone}");
    }

    /// A phase-1 ESP `wifi.rs` nobody touched moves to the current template;
    /// one the user edited is left alone.
    #[test]
    fn an_untouched_phase_one_file_is_upgraded() {
        let old = format!(
            "{GEN_BEGIN_CFG}\n// x\n{GEN_END_CFG}\n{}",
            include_str!("iot_templates/legacy/wifi_esp_v1.rs")
        );
        let body = wifi_file(Platform::Esp);
        assert_eq!(upgraded(WIFI, &old, &body).as_deref(), Some(body.as_str()));
        assert_eq!(upgraded(MQTT, &old, &body), None, "only the file it was");
        // And the current template is not "legacy".
        assert_eq!(upgraded(WIFI, &body, &body), None);
        // What the user wrote above the generated block stays.
        let headed = format!("// my notes\n{old}");
        let up = upgraded(WIFI, &headed, &body).expect("upgraded");
        assert!(up.starts_with("// my notes\n"), "{up}");
        assert!(up.ends_with(&lf(WIFI_ESP_TAIL)), "{up}");
        // The same on a CRLF checkout of the template and of the file.
        let crlf = old.replace('\n', "\r\n");
        assert!(upgraded(WIFI, &crlf, &body).is_some());
    }

    /// A project retargeted to another radio: an untouched `ble.rs` or
    /// `wifi.rs` of the old one becomes the new one's template, both ways,
    /// with the user's head kept; an edited one stays and is reported.
    #[test]
    fn an_untouched_file_follows_the_radio_on_a_retarget() {
        let b = BleConfig::default();
        let esp = ble_file(&b, Platform::Esp);
        let nrf = ble_file(&b, Platform::Nrf);
        assert_eq!(upgraded(BLE, &esp, &nrf).as_deref(), Some(nrf.as_str()));
        assert_eq!(upgraded(BLE, &nrf, &esp).as_deref(), Some(esp.as_str()));
        // The ESP and the Pico W share the editable half: nothing to swap.
        assert_eq!(upgraded(BLE, &esp, &ble_file(&b, Platform::Cyw43)), None);
        let headed = format!("// mine\n{esp}");
        let up = upgraded(BLE, &headed, &nrf).expect("swapped");
        assert!(up.starts_with("// mine\n") && up.ends_with(&lf(BLE_NRF_TAIL)), "{up}");
        assert!(!foreign_radio_file(BLE, &up, Platform::Nrf));

        let (we, wc) = (wifi_file(Platform::Esp), wifi_file(Platform::Cyw43));
        assert_eq!(upgraded(WIFI, &we, &wc).as_deref(), Some(wc.as_str()));
        assert_eq!(upgraded(WIFI, &wc, &we).as_deref(), Some(we.as_str()));

        let edited = format!("{esp}\n// my code\n");
        assert_eq!(upgraded(BLE, &edited, &nrf), None, "an edited file stays");
        assert!(foreign_radio_file(BLE, &edited, Platform::Nrf));
        assert!(!foreign_radio_file(BLE, &edited, Platform::Cyw43));
        assert!(foreign_radio_file(BLE, &nrf, Platform::Esp));
        assert!(foreign_radio_file(WIFI, &wc, Platform::Esp));
        assert!(foreign_radio_file(WIFI, &we, Platform::Cyw43));
        assert!(!foreign_radio_file(WIFI, &we, Platform::Esp));
    }

    /// An EDITED phase-1 ESP `wifi.rs` keeps the user's edits and gets the
    /// `init` main.rs now calls; one edited past recognition is reported.
    #[test]
    fn an_edited_phase_one_file_is_migrated_in_place() {
        let old = format!(
            "{GEN_BEGIN_CFG}\n// x\n{GEN_END_CFG}\n{}",
            include_str!("iot_templates/legacy/wifi_esp_v1.rs")
        )
        .replace("pub const RETRY_S: u64 = 5;", "pub const RETRY_S: u64 = 10;");
        let body = wifi_file(Platform::Esp);
        let up = upgraded(WIFI, &old, &body).expect("migrated");
        assert!(up.contains("pub const RETRY_S: u64 = 10;"), "the user's edit stays:\n{up}");
        assert!(up.contains("    controller: WifiController<'static>,\n    station: Interface<'static>,"), "{up}");
        assert!(!up.contains("esp_radio::wifi::new("), "{up}");
        assert!(!up.contains("interfaces.station"), "{up}");
        assert!(!phase_one_wifi_left(&up));
        // Migrated twice is migrated once.
        assert_eq!(upgraded(WIFI, &up, &body), None);

        let mangled = old.replace(
            "    let (controller, interfaces) = esp_radio::wifi::new(wifi, Default::default()).unwrap();\n",
            "    let (controller, interfaces) = esp_radio::wifi::new(wifi, my_config()).unwrap();\n",
        );
        assert_eq!(upgraded(WIFI, &mangled, &body), None);
        assert!(phase_one_wifi_left(&mangled), "the tab tells the user");
    }

    /// The IoT files never change with the runtime, so a Runtime Apply's
    /// forced rewrite must leave them to the user.
    #[test]
    fn every_iot_file_is_runtime_free() {
        let mut m = mcu("esp32c3", Runtime::Async, true);
        m.iot.sntp = Some(SntpConfig::default());
        m.iot.esp_now = Some(EspNowConfig::default());
        for (name, _) in config_files_for(&m) {
            assert!(runtime_free(&name), "{name}");
        }
        for (name, _) in config_files_for(&thread_mcu("nrf52840_dk")) {
            assert!(runtime_free(&name), "{name}");
        }
    }

    /// The peer list holds 20 entries, the broadcast address among them.
    #[test]
    fn at_most_nineteen_peers_are_written() {
        let peers: Vec<[u8; 6]> = (0..25u8).map(|i| [0x24, 0, 0, 0, 0, i]).collect();
        let f = espnow_file(&EspNowConfig { channel: 1, peers });
        let line = f.lines().find(|l| l.starts_with("pub const PEERS")).unwrap();
        assert_eq!(line.matches("[0x24").count(), iot::MAX_ESPNOW_PEERS, "{line}");
    }

    /// Blocking has no driver to generate for: nothing, rather than files
    /// that cannot compile. The tab says why.
    #[test]
    fn nothing_is_generated_off_the_async_runtime() {
        assert!(config_files_for(&mcu("esp32c3", Runtime::Blocking, true)).is_empty());
        let code = esp_main("x".into(), &mcu("esp32c3", Runtime::Blocking, true));
        assert_eq!(code, "x");
    }

    /// A chip without the radio gets nothing even with the switch on - a
    /// project carried over from an ESP32-C3 to an H2 keeps its `@iot`.
    #[test]
    fn a_chip_without_wifi_generates_nothing() {
        assert!(config_files_for(&mcu("esp32h2", Runtime::Async, true)).is_empty());
        assert!(config_files_for(&mcu("rp2040_pico", Runtime::Async, true)).is_empty());
        assert!(!config_files_for(&mcu("rp2040_pico_w", Runtime::Async, true)).is_empty());
    }

    /// The Pico W gets the CYW43 file, the ESP the esp-radio one.
    #[test]
    fn each_radio_gets_its_own_wifi_file() {
        let wifi = |m: &Mcu| {
            config_files_for(m)
                .into_iter()
                .find(|(n, _)| n == WIFI)
                .map(|(_, b)| b)
                .unwrap()
        };
        // main.rs creates the radio now; wifi.rs takes its station half.
        let esp = wifi(&mcu("esp32c3", Runtime::Async, false));
        assert!(esp.contains("station: Interface<'static>,"), "{esp}");
        assert!(!esp.contains("esp_radio::wifi::new("), "{esp}");
        assert!(wifi(&mcu("rp2350_pico2_w", Runtime::Async, false)).contains("cyw43::Control<'static>"));
    }

    /// Strings typed in the tab reach the file as Rust literals, quotes and
    /// all - and an invalid filter never reaches `SUBSCRIBE`, where
    /// rust-mqtt would refuse it at run time.
    #[test]
    fn mqtt_settings_are_escaped_and_checked() {
        let m = MqttConfig {
            host: "broker\"x".into(),
            subscribe: vec!["a/#".into(), "bad/#/x".into(), "b/+".into()],
            ..MqttConfig::default()
        };
        let f = mqtt_file(&m);
        assert!(f.contains(r#"pub const BROKER_HOST: &str = "broker\"x";"#), "{f}");
        assert!(f.contains(r#"pub const SUBSCRIBE: &[&str] = &["a/#", "b/+"];"#), "{f}");
    }

    /// The ESP block: heap before the scheduler, spawner named, bring-up
    /// last - in that order.
    #[test]
    fn esp_main_orders_heap_start_and_bring_up() {
        let m = mcu("esp32c3", Runtime::Async, true);
        // Through the backend, as the app generates it - not `esp_main` by hand.
        let code = m.fresh_main_rs();
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s} missing:\n{code}"));
        assert!(at("esp_alloc::heap_allocator!(size: 72 * 1024);") < at("esp_rtos::start("));
        assert!(at("esp_rtos::start(") < at("esp_radio::wifi::new(peripherals.WIFI"));
        assert!(at("pins::configs::mqtt::start(spawner, net_stack);") < at(super::super::GEN_END));
        assert!(code.contains("async fn main(spawner: Spawner)"), "{code}");
        assert!(!code.contains("_spawner"), "{code}");
    }

    /// Splicing twice changes nothing: `update_main_rs` runs every regeneration.
    #[test]
    fn esp_main_is_idempotent_through_update() {
        let m = mcu("esp32c3", Runtime::Async, true);
        let once = m.fresh_main_rs();
        let twice = m.update_main_rs(&once);
        assert_eq!(once, twice);
        assert_eq!(once.matches("heap_allocator!").count(), 1);
    }

    /// On a Pico W the radio comes up for Wi-Fi alone, and `control` goes to
    /// the Wi-Fi task instead of the LED binding - one bring-up, one PIO.
    #[test]
    fn the_pico_w_radio_comes_up_for_wifi() {
        use crate::panels::mcu_module::codegen::rp;
        let m = mcu("rp2040_pico_w", Runtime::Async, true);
        assert!(rp::needs_radio(&m));
        let code = m.fresh_main_rs();
        assert_eq!(code.matches("cyw43::new(").count(), 1, "{code}");
        assert!(code.contains("let (net_device, mut control, runner) = cyw43::new("), "{code}");
        assert!(code.contains("pins::configs::wifi::init(spawner, net_device, control);"), "{code}");
        assert!(code.contains("pins::configs::mqtt::start(spawner, net_stack);"), "{code}");
        assert!(!code.contains("wl_led"), "{code}");
        // The Configuration tab lists the PIO the radio takes.
        assert_eq!(rp::pio_uses(&m).len(), 1);

        // Wi-Fi off: no radio at all on a board whose LED is not taken.
        let mut off = m.clone();
        off.iot.wifi = false;
        assert!(!rp::needs_radio(&off));
        assert!(!off.fresh_main_rs().contains("cyw43"));
    }

    fn ble_mcu(id: &str, wifi: bool) -> Mcu {
        let mut m = mcu(id, Runtime::Async, false);
        m.iot.wifi = wifi;
        m.iot.ble = Some(BleConfig::default());
        m
    }

    /// ESP Bluetooth alone: the controller, no Wi-Fi radio, the tab's heap.
    #[test]
    fn esp_ble_alone_starts_only_the_controller() {
        let m = ble_mcu("esp32c3", false);
        let code = m.fresh_main_rs();
        assert!(code.contains("esp_radio::ble::controller::BleConnector::new(peripherals.BT, Default::default()).unwrap();"), "{code}");
        assert!(code.contains("pins::configs::ble::start(spawner, ble, None);"), "{code}");
        assert!(!code.contains("esp_radio::wifi::new("), "{code}");
        assert!(code.contains("esp_alloc::heap_allocator!(size: 72 * 1024);"), "{code}");
        let names: Vec<String> = config_files_for(&m).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, ["ble.rs", "panic.rs"]);
    }

    /// Beside Wi-Fi: coex's two-region heap, ESP32's own split, Bluetooth
    /// started before the Wi-Fi radio.
    #[test]
    fn esp_ble_beside_wifi_takes_the_coex_heap() {
        let c3 = ble_mcu("esp32c3", true).fresh_main_rs();
        assert!(c3.contains("esp_alloc::heap_allocator!(#[esp_hal::ram(reclaimed)] size: 64 * 1024);"), "{c3}");
        assert!(c3.contains("esp_alloc::heap_allocator!(size: 64 * 1024);"), "{c3}");
        let at = |s: &str| c3.find(s).unwrap_or_else(|| panic!("{s}:\n{c3}"));
        assert!(at("pins::configs::ble::start(") < at("esp_radio::wifi::new("));
        let e32 = ble_mcu("esp32", true).fresh_main_rs();
        assert!(e32.contains("#[esp_hal::ram(reclaimed)] size: 96 * 1024"), "{e32}");
        assert!(e32.contains("esp_alloc::heap_allocator!(size: 24 * 1024);"), "{e32}");
    }

    /// Beside ESP-NOW with the station off: the coex heap, Bluetooth first,
    /// the Wi-Fi radio held for ESP-NOW, and no IP stack.
    #[test]
    fn esp_ble_beside_espnow_alone_holds_the_radio() {
        let mut m = ble_mcu("esp32c3", false);
        m.iot.esp_now = Some(EspNowConfig::default());
        let code = m.fresh_main_rs();
        assert!(code.contains("#[esp_hal::ram(reclaimed)] size: 64 * 1024"), "{code}");
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s}:\n{code}"));
        assert!(at("pins::configs::ble::start(spawner, ble, None);") < at("esp_radio::wifi::new("));
        assert!(code.contains("pins::configs::espnow::hold_radio(spawner, wifi_controller);"), "{code}");
        assert!(!code.contains("net_stack"), "{code}");
        let names: Vec<String> = config_files_for(&m).into_iter().map(|(n, _)| n).collect();
        assert!(names.contains(&BLE.to_owned()) && names.contains(&ESPNOW.to_owned()), "{names:?}");
        assert!(!names.contains(&NET.to_owned()), "{names:?}");
    }

    /// The H2: Bluetooth with no Wi-Fi radio - a stale Wi-Fi switch from
    /// another chip generates nothing.
    #[test]
    fn the_h2_gets_bluetooth_and_never_wifi() {
        let m = ble_mcu("esp32h2", true);
        let a = iot::active(&m).expect("BLE on the H2");
        assert!(a.ble && !a.station, "{a:?}");
        assert_eq!(a.platform, Platform::Esp);
        let code = m.fresh_main_rs();
        assert!(code.contains("BleConnector::new(peripherals.BT"), "{code}");
        assert!(!code.contains("esp_radio::wifi::new("), "{code}");
    }

    /// The ESP and the Pico W share ble.rs's editable half; the generated
    /// block names the radio, and a name that cannot be advertised is
    /// replaced by the default.
    #[test]
    fn ble_rs_names_its_radio() {
        let long = BleConfig {
            device_name: "x".repeat(iot::MAX_BLE_NAME + 1),
        };
        let esp = ble_file(&long, Platform::Esp);
        assert!(esp.contains("pub type Radio = esp_radio::ble::controller::BleConnector<'static>;"), "{esp}");
        assert!(esp.contains(&format!("pub const DEVICE_NAME: &str = \"{}\";", iot::DEFAULT_BLE_NAME)), "{esp}");
        let pico = ble_file(&BleConfig::default(), Platform::Cyw43);
        assert!(pico.contains("pub type Radio = cyw43::bluetooth::BtDriver<'static>;"), "{pico}");
        assert_eq!(tail_of(&esp), tail_of(&pico), "one editable half");
        let nrf = ble_file(&BleConfig::default(), Platform::Nrf);
        assert!(!nrf.contains("pub type Radio"), "{nrf}");
        assert!(nrf.contains("SoftdeviceController"), "{nrf}");
        assert!(is_pristine(BLE, &esp) && is_pristine(BLE, &nrf));
    }

    /// Pico W Bluetooth alone: the radio up once WITH its Bluetooth half, the
    /// fourth firmware, the LED kept in main - and the address read first.
    #[test]
    fn the_pico_w_brings_up_bluetooth_alone() {
        let m = ble_mcu("rp2040_pico_w", false);
        let code = m.fresh_main_rs();
        assert_eq!(code.matches("cyw43::new_with_bluetooth(").count(), 1, "{code}");
        assert!(!code.contains("cyw43::new(state"), "{code}");
        assert!(code.contains("cyw43::aligned_bytes!(\"../firmware/43439A0_btfw.bin\")"), "{code}");
        assert!(code.contains("let (_net_device, bt_device, mut control, runner) ="), "{code}");
        assert!(code.contains("pins::configs::ble::start(spawner, bt_device, Some(mac));"), "{code}");
        assert!(code.contains("let mut wl_led = control;"), "{code}");
        assert!(crate::panels::mcu_module::codegen::rp::needs_bluetooth(&m));
        assert!(crate::panels::mcu_module::codegen::rp::needs_radio(&m));
    }

    /// With Wi-Fi too: the MAC is read before Wi-Fi takes `control`.
    #[test]
    fn the_pico_w_reads_its_mac_before_wifi_takes_control() {
        let code = ble_mcu("rp2350_pico2_w", true).fresh_main_rs();
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s}:\n{code}"));
        assert!(at("control.address().await") < at("wifi::init(spawner, net_device, control)"));
        assert!(code.contains("let (net_device, bt_device, mut control, runner) ="), "{code}");
        assert!(!code.contains("wl_led"), "{code}");
    }

    /// nRF Bluetooth: the MPSL's vectors bound, priority 0 left to the radio,
    /// the bring-up last, the spawner named.
    #[test]
    fn the_nrf_brings_up_the_softdevice_controller() {
        let m = ble_mcu("nrf52840_dk", false);
        assert!(crate::panels::mcu_module::codegen::nrf::ble_on(&m));
        let code = m.fresh_main_rs();
        for want in [
            "EGU0_SWI0 => nrf_sdc::mpsl::LowPrioInterruptHandler;",
            "CLOCK_POWER => nrf_sdc::mpsl::ClockInterruptHandler;",
            "RADIO => nrf_sdc::mpsl::HighPrioInterruptHandler;",
            "TIMER0 => nrf_sdc::mpsl::HighPrioInterruptHandler;",
            "RTC0 => nrf_sdc::mpsl::HighPrioInterruptHandler;",
            "config.time_interrupt_priority = embassy_nrf::interrupt::Priority::P2;",
            "config.gpiote_interrupt_priority = embassy_nrf::interrupt::Priority::P2;",
            "nrf_sdc::mpsl::MultiprotocolServiceLayer::new(",
            "pins::configs::ble::start(",
            "async fn main(spawner: embassy_executor::Spawner)",
        ] {
            assert!(code.contains(want), "{want}:\n{code}");
        }
        let at = |s: &str| code.find(s).unwrap();
        assert!(at("config.time_interrupt_priority") < at("embassy_nrf::init(config)"));
        assert!(crate::panels::mcu_module::codegen::nrf::needs_static_cell(&m));
    }

    /// The nRF5340 and the 54L15 do not get Bluetooth generated (their reasons
    /// are in `availability`); an nRF52 with USB wired waits for it.
    #[test]
    fn nrf_bluetooth_where_it_is_not_generated() {
        for id in ["nrf5340_dk", "nrf54l15_dk"] {
            assert!(iot::active(&ble_mcu(id, false)).is_none(), "{id}");
        }
    }

    fn thread_mcu(id: &str) -> Mcu {
        let mut m = mcu(id, Runtime::Async, false);
        m.iot.wifi = false;
        m.iot.thread = Some(ThreadConfig::default());
        m
    }

    /// Thread on an nRF52840: RADIO bound to embassy-nrf's handler, the
    /// crystal started, the radio and an RNG handed to `thread::start` last,
    /// the spawner named - and none of Bluetooth's MPSL.
    #[test]
    fn the_nrf_brings_up_thread() {
        let m = thread_mcu("nrf52840_dk");
        assert!(crate::panels::mcu_module::codegen::nrf::thread_on(&m));
        let code = m.fresh_main_rs();
        for want in [
            "RADIO => embassy_nrf::radio::InterruptHandler<embassy_nrf::peripherals::RADIO>;",
            "config.hfclk_source = embassy_nrf::config::HfclkSource::ExternalXtal;",
            "Thread (IoT tab) needs the crystal",
            "embassy_nrf::radio::ieee802154::Radio::new(p.RADIO, Irqs),",
            "RNG.init(embassy_nrf::rng::Rng::new_blocking(p.RNG)),",
            "pins::configs::thread::start(",
            "async fn main(spawner: embassy_executor::Spawner)",
        ] {
            assert!(code.contains(want), "{want}:\n{code}");
        }
        for absent in ["nrf_sdc", "Priority::P2"] {
            assert!(!code.contains(absent), "{absent}:\n{code}");
        }
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s}:\n{code}"));
        assert!(at("embassy_nrf::init(config)") < at("pins::configs::thread::start("));

        let files = config_files_for(&m);
        let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, [SECRETS, THREAD, PANIC]);
        let secrets = &files[0].1;
        assert!(secrets.contains("pub const THREAD_DATASET: &str = \"\";"), "{secrets}");
        assert!(!secrets.contains("WIFI_SSID"), "no Wi-Fi on an nRF:\n{secrets}");
        let thread = &files[1].1;
        assert!(thread.contains("pub const UDP_PORT: u16 = 1212;"), "{thread}");
        assert!(thread.contains("use super::secrets::THREAD_DATASET;"), "{thread}");
        // An empty dataset is "set" without error: only a complete one starts
        // Thread, or the device would look for any parent on defaults.
        assert!(thread.contains("&& ot.is_commissioned()"), "{thread}");
        // A datagram longer than MAX_DATA is read whole and dropped, not cut.
        assert!(thread.contains("let mut buf = [0u8; UDP_RX];"), "{thread}");
        // With no dataset the handle is kept, not dropped under `ot_task`.
        assert!(thread.contains("core::mem::forget(ot);"), "{thread}");
        assert!(is_pristine(THREAD, thread) && is_pristine(SECRETS, secrets));
        assert!(runtime_free(THREAD));
    }

    /// Thread on an ESP: `thread::start` with the 802.15.4 peripheral, last in
    /// the block, after the heap its receive queue needs - and nothing of the
    /// nRF's (no InterruptExecutor, whose name alone would bring
    /// `executor-interrupt` back) nor of Wi-Fi or Bluetooth.
    #[test]
    fn the_esp_brings_up_thread() {
        for id in ["esp32c6", "esp32h2", "esp32c5"] {
            let m = thread_mcu(id);
            // The nRF's soft-float switch is the nRF's: an ESP keeps riscv32imac.
            assert!(!crate::panels::mcu_module::codegen::nrf::thread_on(&m), "{id}");
            let code = m.fresh_main_rs();
            for want in [
                "pins::configs::thread::start(spawner, peripherals.IEEE802154);",
                "esp_alloc::heap_allocator!(size: 72 * 1024);",
                "async fn main(spawner: Spawner)",
            ] {
                assert!(code.contains(want), "{id}: {want}:\n{code}");
            }
            for absent in ["esp_radio::wifi::new(", "BleConnector", "EGU0_SWI0", "InterruptExecutor"] {
                assert!(!code.contains(absent), "{id}: {absent}:\n{code}");
            }
            let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s}:\n{code}"));
            assert!(at("esp_alloc::heap_allocator!") < at("pins::configs::thread::start("));

            let files = config_files_for(&m);
            let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, [SECRETS, THREAD, PANIC], "{id}");
            let thread = &files[1].1;
            for want in [
                "use openthread::esp::{EspRadio, Ieee802154};",
                "pub fn start(spawner: Spawner, radio: esp_hal::peripherals::IEEE802154<'static>)",
                "commissioned(THREAD_DATASET)",
                "core::mem::forget(ot);",
                "pub const UDP_PORT: u16 = 1212;",
            ] {
                assert!(thread.contains(want), "{id}: {want}:\n{thread}");
            }
            for absent in ["InterruptExecutor", "openthread::nrf::", "EGU0_SWI0"] {
                assert!(!thread.contains(absent), "{id}: {absent}");
            }
            assert!(is_pristine(THREAD, thread));
            assert!(!foreign_radio_file(THREAD, thread, Platform::Esp));
            assert!(foreign_radio_file(THREAD, thread, Platform::Nrf));
        }
        // The radio is created before the RNG is read: it powers the RF.
        let t = lf(THREAD_ESP_TAIL);
        assert!(t.find("Ieee802154::new(radio)").unwrap() < t.find("rng.read(").unwrap());
    }

    /// A project retargeted between an nRF and an ESP: an untouched thread.rs
    /// becomes the other radio's, both ways; phase 4's nRF template (the one
    /// that dropped its handle) becomes the current one; an edited file stays.
    #[test]
    fn thread_rs_follows_the_radio_and_the_fix() {
        let t = ThreadConfig::default();
        let nrf = thread_file(&t, Platform::Nrf);
        let esp = thread_file(&t, Platform::Esp);
        assert_eq!(upgraded(THREAD, &nrf, &esp).as_deref(), Some(esp.as_str()));
        assert_eq!(upgraded(THREAD, &esp, &nrf).as_deref(), Some(nrf.as_str()));
        let v1 = format!(
            "{GEN_BEGIN_CFG}\npub const UDP_PORT: u16 = 1212;\n{GEN_END_CFG}\n{}",
            include_str!("iot_templates/legacy/thread_nrf_v1.rs")
        );
        assert!(!v1.contains("core::mem::forget(ot)"), "v1 is the one without the fix");
        let up = upgraded(THREAD, &v1, &nrf).expect("phase 4's template is moved");
        assert!(up.contains("core::mem::forget(ot);"), "{up}");
        assert!(is_pristine(THREAD, &v1), "an untouched v1 may go with the switch");
        let edited = format!("{esp}\n// mine\n");
        assert_eq!(upgraded(THREAD, &edited, &nrf), None);
        assert!(foreign_radio_file(THREAD, &edited, Platform::Nrf));
    }

    /// A crystal the Clock tab already chose is not "overridden"; the 52833
    /// gets Thread too; Bluetooth beside it takes the radio.
    #[test]
    fn thread_follows_the_clock_tab_and_yields_to_bluetooth() {
        let mut m = thread_mcu("nrf52833_microbit_v2");
        assert!(m.fresh_main_rs().contains("pins::configs::thread::start("));
        {
            use crate::panels::mcu_module::clock::graph::model::NodeState;
            use crate::panels::mcu_module::clock::model::ClockConfig;
            let ClockConfig::Graph(gc) = &mut m.clock else {
                panic!("the micro:bit carries a graph");
            };
            gc.graph.node_mut("hfclk_src").unwrap().state = NodeState::Index(1);
        }
        let code = m.fresh_main_rs();
        assert!(code.contains("HfclkSource::ExternalXtal;"), "{code}");
        assert!(!code.contains("Thread (IoT tab) needs the crystal"), "{code}");

        m.iot.ble = Some(BleConfig::default());
        let code = m.fresh_main_rs();
        assert!(!code.contains("thread::start"), "{code}");
        assert!(code.contains("pins::configs::ble::start("), "{code}");
        let names: Vec<String> = config_files_for(&m).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, [BLE, PANIC]);
    }

    /// `secrets.rs` is written once: a link switched on later adds its empty
    /// line, and nothing the user wrote moves.
    #[test]
    fn a_later_link_tops_the_secrets_up() {
        let wifi = write_secret(&secrets_body(), "WIFI_SSID", "home");
        let thread_body = secrets_body_for(false, true);
        let up = topped_up(&wifi, &thread_body).expect("THREAD_DATASET is missing");
        assert!(up.starts_with(&wifi), "the old lines stay as they were:\n{up}");
        assert!(up.ends_with("pub const THREAD_DATASET: &str = \"\";\n"), "{up}");
        assert_eq!(read_secret(&up, "WIFI_SSID").as_deref(), Some("home"));
        assert_eq!(topped_up(&up, &thread_body), None, "once");
        assert_eq!(topped_up(&up, &secrets_body_for(true, true)), None);
        // A line the user made into something else is still "there" - a
        // second declaration would not compile (E0428).
        for custom in [
            "pub const THREAD_DATASET: &str = env!(\"DATASET\");\n",
            "pub(crate) const THREAD_DATASET: &str = \"0e08\";\n",
            "  pub static THREAD_DATASET : &str = \"0e08\";\n",
            "const THREAD_DATASET: &'static str = \"\";\n",
        ] {
            assert_eq!(topped_up(custom, &thread_body), None, "{custom}");
            assert!(has_secret_line(custom, THREAD_DATASET), "{custom}");
        }
        // A longer name, or one only mentioned, is not the declaration.
        assert!(!has_secret_line("pub const THREAD_DATASET_V2: &str = \"\";", THREAD_DATASET));
        assert!(!has_secret_line("// THREAD_DATASET: paste it here", THREAD_DATASET));
        // No trailing newline: the line still starts on a line of its own.
        assert!(topped_up("// mine", &thread_body).unwrap().starts_with("// mine\npub const"));
    }

    /// Pristine is "every secret line there is empty", whichever links wrote
    /// the file - so an untouched Thread-only file may go with the switch.
    #[test]
    fn secrets_are_pristine_whatever_links_wrote_them() {
        for (station, thread) in [(true, false), (false, true), (true, true)] {
            let body = secrets_body_for(station, thread);
            assert!(secrets_pristine(&body), "{body}");
        }
        let set = write_secret(&secrets_body_for(false, true), THREAD_DATASET, "0e08");
        assert!(!secrets_pristine(&set));
        assert!(!secrets_pristine("// nothing of ours\n"));
        // A line of the user's own keeps the file, even beside empty ones -
        // pruning it would lose that line with the next Save.
        let mine = format!("{}pub const OTA_KEY: &str = \"s3cret\";\n", secrets_body_for(false, true));
        assert!(!secrets_pristine(&mine));
        // Lines deleted, nothing added: nothing of the user's is in it.
        assert!(secrets_pristine("pub const WIFI_SSID: &str = \"\";\npub const WIFI_PASSWORD: &str = \"\";\n"));
    }
    #[test]
    fn the_template_carries_the_topic_length_the_tab_checks() {
        let line = format!("pub const MAX_TOPIC: usize = {};", iot::MAX_TOPIC);
        assert!(MQTT_TAIL.contains(&line), "{line}");
    }

    #[test]
    fn secrets_round_trip_through_the_file() {
        let body = secrets_body();
        assert_eq!(read_secret(&body, "WIFI_SSID").as_deref(), Some(""));
        for v in ["home \"net\"", "back\\slash", "ünïcode", "tab\there"] {
            let f = write_secret(&body, "WIFI_PASSWORD", v);
            assert_eq!(read_secret(&f, "WIFI_PASSWORD").as_deref(), Some(v), "{f}");
            assert_eq!(read_secret(&f, "WIFI_SSID").as_deref(), Some(""));
        }
        // A deleted line comes back at the end.
        let f = write_secret("// mine\n", "MQTT_USERNAME", "u");
        assert_eq!(read_secret(&f, "MQTT_USERNAME").as_deref(), Some("u"));
        assert!(f.starts_with("// mine\n"));
    }

    /// A value the tab cannot show is left alone, not misread.
    #[test]
    fn a_hand_written_expression_is_not_a_secret() {
        assert_eq!(read_secret("pub const WIFI_SSID: &str = env!(\"X\");\n", "WIFI_SSID"), None);
    }

    /// Switched off, a file the user wrote in stays; a template goes.
    #[test]
    fn edited_files_outlive_the_switch() {
        let m = mcu("esp32c3", Runtime::Async, true);
        let files = config_files_for(&m);
        let tree: Vec<(String, String)> = files
            .iter()
            .map(|(n, b)| (format!("src/pins/configs/{n}"), b.clone()))
            .collect();
        assert!(kept_paths(&[], &tree).is_empty(), "pristine files go");
        let mut tree = tree;
        for (p, b) in &mut tree {
            if p.ends_with(SECRETS) {
                *b = write_secret(b, "WIFI_PASSWORD", "hunter2");
            }
            if p.ends_with(MQTT) {
                b.push_str("\n// mine\n");
            }
        }
        let kept = kept_paths(&[], &tree);
        assert!(kept.contains(&SECRETS_PATH.to_owned()), "{kept:?}");
        assert!(kept.contains(&"src/pins/configs/mqtt.rs".to_owned()), "{kept:?}");
        assert!(kept_paths(&files, &tree).is_empty(), "nothing to keep while generated");
    }

    // ── Restart on panic ──────────────────────────────────────────────────

    /// The ESP's header handler, as the IDE writes it, before and after.
    fn esp_header(handler: &str) -> String {
        format!(
            "{}\n#![no_std]\n#![no_main]\n\npub mod pins;\n\nesp_bootloader_esp_idf::esp_app_desc!();\n{handler}\n{}\n    // block\n{}\n",
            crate::panels::mcu_module::codegen::AUTOGEN_BANNER,
            super::super::GEN_BEGIN,
            super::super::GEN_END,
        )
    }

    /// On: the halting handler becomes the restart's; off again: back, byte
    /// for byte. Every IoT chip, both kinds of header.
    #[test]
    fn the_header_handler_follows_the_switch() {
        let halt = crate::panels::mcu_module::codegen_esp::PANIC_HALT;
        for id in ["esp32c3", "esp32", "esp32s3", "esp32c2", "rp2040_pico_w", "nrf52840_dk"] {
            let mut m = if id.starts_with("nrf") {
                let mut m = mcu(id, Runtime::Async, false);
                m.iot.wifi = false;
                m.iot.ble = Some(BleConfig::default());
                m
            } else {
                mcu(id, Runtime::Async, true)
            };
            assert!(m.iot.restart_on_panic, "a new setup restarts");
            let fresh = m.fresh_main_rs();
            assert_eq!(panic_header(&fresh), PanicHeader::Restart, "{id}:\n{fresh}");
            assert!(fresh.contains("pins::configs::panic::restart(info)"), "{id}");
            assert!(fresh.contains("pins::configs::panic::start(spawner);"), "{id}:\n{fresh}");
            assert!(!fresh.contains("use panic_halt as _;"), "{id}:\n{fresh}");
            // Idempotent.
            assert_eq!(m.update_main_rs(&fresh), fresh, "{id}");
            // Off: the family's own halt, the start line gone.
            m.iot.restart_on_panic = false;
            let off = m.update_main_rs(&fresh);
            assert_eq!(panic_header(&off), PanicHeader::Halt, "{id}:\n{off}");
            assert!(!off.contains("pins::configs::panic"), "{id}:\n{off}");
            assert!(!off.contains("Restart on panic (IoT tab)"), "{id}:\n{off}");
            assert_eq!(off, m.fresh_main_rs(), "{id}: off is what a fresh file holds");
            if id.starts_with("esp") {
                assert!(off.contains(halt), "{id}");
            } else {
                assert!(off.contains("use panic_halt as _;"), "{id}");
            }
            // On again: the same file as before.
            m.iot.restart_on_panic = true;
            assert_eq!(m.update_main_rs(&off), fresh, "{id}");
            // Blocking: no IoT, the halt comes back.
            m.runtime = Runtime::Blocking;
            let blocking = m.update_main_rs(&fresh);
            assert_eq!(panic_header(&blocking), PanicHeader::Halt, "{id}:\n{blocking}");
        }
    }

    /// The user's own handler - rustfmt'd copies aside - is never touched.
    #[test]
    fn a_handler_the_user_wrote_is_left_alone() {
        let m = mcu("esp32c3", Runtime::Async, true);
        // rustfmt's copy of the IDE's halt: swapped like the original.
        let fmt = esp_header("#[panic_handler]\nfn panic(_: &core::panic::PanicInfo) -> ! {\n    loop {}\n}\n");
        assert_eq!(panic_header(&panic_main(fmt, &m)), PanicHeader::Restart);
        // One that prints first: left.
        let own = esp_header(
            "#[panic_handler]\nfn panic(info: &core::panic::PanicInfo) -> ! {\n    esp_println::println!(\"{info}\");\n    loop {}\n}\n",
        );
        assert_eq!(panic_main(own.clone(), &m), own);
        assert_eq!(panic_header(&own), PanicHeader::Other);
        // An edited restart handler is left when switched off, too.
        let mut off = m.clone();
        off.iot.restart_on_panic = false;
        let edited = esp_header(
            "#[panic_handler]\nfn panic(info: &core::panic::PanicInfo) -> ! {\n    esp_println::println!(\"{info}\");\n    pins::configs::panic::restart(info)\n}\n",
        );
        assert_eq!(panic_main(edited.clone(), &off), edited);
        // A copy commented out is not the live one.
        let commented = esp_header(&format!(
            "/*\n{}*/\n#[panic_handler]\nfn panic(info: &core::panic::PanicInfo) -> ! {{\n    loop {{}}\n}}\n",
            crate::panels::mcu_module::codegen_esp::PANIC_HALT
        ));
        assert_eq!(panic_header(&commented), PanicHeader::Other);
        assert_eq!(panic_main(commented.clone(), &m), commented);
        // panic-halt under an attribute: the attribute would govern the new item.
        let gated = format!(
            "#![no_std]\n#[cfg(not(test))]\nuse panic_halt as _;\n{}\n{}\n",
            super::super::GEN_BEGIN,
            super::super::GEN_END
        );
        let mut pico = mcu("rp2040_pico_w", Runtime::Async, true);
        assert_eq!(panic_main(gated.clone(), &pico), gated);
        // ... and an inline one is swapped.
        let plain = gated.replace("#[cfg(not(test))]\n", "");
        assert_eq!(panic_header(&panic_main(plain.clone(), &pico)), PanicHeader::Restart);
        pico.iot.restart_on_panic = false;
        assert_eq!(panic_main(plain.clone(), &pico), plain);
    }

    /// Every chip's panic.rs: the record where a software reset keeps it, the
    /// report where the build has a way out.
    #[test]
    fn panic_rs_has_the_chips_pieces() {
        let file = |m: &Mcu| -> String {
            config_files_for(m)
                .into_iter()
                .find(|(n, _)| n == PANIC)
                .map(|(_, b)| b)
                .unwrap_or_else(|| panic!("no panic.rs for {}", m.family))
        };
        for (id, want) in [
            ("esp32c3", "#[esp_hal::ram(unstable(rtc_fast, persistent))]"),
            ("esp32c6", "#[esp_hal::ram(unstable(rtc_fast, persistent))]"),
            ("esp32", "#[esp_hal::ram(unstable(rtc_slow, persistent))]"),
            ("esp32s3", "#[esp_hal::ram(unstable(rtc_slow, persistent))]"),
            ("esp32c2", "#[unsafe(link_section = \".noinit.rustonchip_panic\")]"),
            ("esp32c61", "#[unsafe(link_section = \".noinit.rustonchip_panic\")]"),
        ] {
            let body = file(&mcu(id, Runtime::Async, true));
            assert!(body.contains(want), "{id}:\n{body}");
            assert!(body.contains("esp_hal::system::software_reset()"), "{id}");
            assert!(body.contains("esp_println::println!(\"{report}\");"), "{id}: the serial port");
            assert!(body.contains("pub const REPORT_TOPIC: &str = \"rustonchip-"), "{id}: MQTT");
            assert!(body.contains("super::mqtt::publish(REPORT_TOPIC, text.as_bytes())"), "{id}");
            assert!(is_pristine(PANIC, &body), "{id}");
        }
        // No MQTT: no topic, no task.
        let plain = file(&mcu("esp32c3", Runtime::Async, false));
        assert!(!plain.contains("REPORT_TOPIC") && !plain.contains("report_task"), "{plain}");
        assert!(plain.contains("fn report_at_boot(_spawner: embassy_executor::Spawner, report: Report)"));
        // UART0 is the user's: nothing printed on it.
        let mut uart = mcu("esp32c3", Runtime::Async, false);
        let pad = uart
            .iter_all_pins()
            .find(|p| !p.reserved)
            .map(|p| p.number)
            .unwrap();
        uart.apply_saved_pins_by_number(&[(pad, crate::panels::mcu_module::PinFunction::UsartTx(0))]);
        let quiet = file(&uart);
        assert!(!quiet.contains("esp_println"), "{quiet}");
        assert!(quiet.contains("fn report_at_boot(_spawner: embassy_executor::Spawner, _report: Report)"), "{quiet}");
        // The Pico W and the nRF: `.uninit`, a core reset.
        let pico = file(&mcu("rp2040_pico_w", Runtime::Async, true));
        assert!(pico.contains(".uninit.rustonchip_panic") && pico.contains("SCB::sys_reset()"), "{pico}");
        assert!(pico.contains("Watchdog::new(unsafe { embassy_rp::peripherals::WATCHDOG::steal() }).stop();"));
        let mut nrf = mcu("nrf52840_dk", Runtime::Async, false);
        nrf.iot.wifi = false;
        nrf.iot.ble = Some(BleConfig::default());
        let nrf = file(&nrf);
        assert!(nrf.contains("DWT::cycle_count()") && nrf.contains("WatchdogHandle::steal"), "{nrf}");
        assert!(nrf.contains("fn report_at_boot(_spawner: embassy_executor::Spawner, _report: Report)"), "{nrf}");
    }

    /// The client certificate's block, on and off; ALPN only for AWS IoT on 443.
    #[test]
    fn the_link_carries_the_client_certificate() {
        let mut m = tls_mcu("esp32c3", false);
        let link = |m: &Mcu| {
            config_files_for(m)
                .into_iter()
                .find(|(n, _)| n == MQTT_LINK)
                .map(|(_, b)| b)
                .unwrap()
        };
        let off = link(&m);
        assert!(off.contains("pub const CLIENT: Option<(&[u8], [u8; 32])> = None;"), "{off}");
        assert!(off.contains("pub const ALPN: &[&[u8]] = &[];"));
        assert!(off.contains("fn client_cert(&mut self)") && off.contains("struct ClientKey("));
        assert!(off.contains("pub const fn client_identity("), "pem_der.rs rides along");
        assert!(is_pristine(MQTT_LINK, &off));
        if let Some(mq) = m.iot.mqtt.as_mut() {
            mq.client_cert = true;
            mq.host = "abc-ats.iot.eu-west-1.amazonaws.com".into();
            mq.port = 443;
        }
        let on = link(&m);
        assert!(on.contains("pub const CLIENT: Option<(&[u8], [u8; 32])> = client_identity(&CLIENT_CERT_DER, &CLIENT_KEY_DER);"), "{on}");
        assert!(on.contains("super::secrets::MQTT_CLIENT_KEY"));
        assert!(on.contains("pub const ALPN: &[&[u8]] = &[b\"x-amzn-mqtt-ca\"];"));
        let secrets = config_files_for(&m).into_iter().find(|(n, _)| n == SECRETS).map(|(_, b)| b).unwrap();
        assert!(secrets.contains("pub const MQTT_CLIENT_CERT: &str = \"\";") && secrets.contains("pub const MQTT_CLIENT_KEY: &str = \"\";"));
        // An edited link from before it cannot send it; the tab says so.
        let legacy = format!("{GEN_END_CFG}\n{}", lf(LEGACY_TAILS[6].1));
        assert!(client_link_left(Some(&legacy), true).is_some());
        assert!(client_link_left(Some(&legacy), false).is_none());
        assert!(client_link_left(Some(&on), true).is_none());
    }

    /// An untouched TLS link from before the client certificate moves to the
    /// current one; one the user edited stays.
    #[test]
    fn a_pristine_v2_link_is_upgraded() {
        let m = tls_mcu("esp32c3", false);
        let body = config_files_for(&m).into_iter().find(|(n, _)| n == MQTT_LINK).map(|(_, b)| b).unwrap();
        let (block, _) = body.split_once(GEN_END_CFG).unwrap();
        for (legacy, current) in [(LEGACY_TAILS[6].1, LINK_TLS_ESP_TAIL), (LEGACY_TAILS[7].1, LINK_TLS_CYW43_TAIL)] {
            let old = format!("{block}{GEN_END_CFG}\n{}", lf(legacy));
            assert!(is_pristine(MQTT_LINK, &old));
            let up = upgraded(MQTT_LINK, &old, &body).expect("upgraded");
            assert!(up.ends_with(&lf(LINK_TLS_ESP_TAIL)) || up.ends_with(&lf(current)));
            let edited = old.replace("RETRY", "RETRY_EDITED");
            assert_eq!(upgraded(MQTT_LINK, &edited, &body), None);
        }
    }

    /// The CA presets hold what they say.
    #[test]
    fn the_ca_presets_hold_their_cas() {
        use crate::panels::mcu_module::iot;
        let aws = mqtt_ca_preset(CaPreset::Aws);
        let pem = ca_pem_of(&aws).unwrap();
        assert_eq!(pem.matches("-----BEGIN CERTIFICATE-----").count(), 2);
        assert!(iot::ca_summary(pem).unwrap().name.contains("Amazon Root CA 1"));
        assert!(iot::cas_der(pem).is_some());
        let mosq = mqtt_ca_preset(CaPreset::Mosquitto);
        assert!(iot::ca_summary(ca_pem_of(&mosq).unwrap()).unwrap().name.contains("mosquitto.org"));
        assert_eq!(mqtt_ca_preset(CaPreset::LetsEncrypt), mqtt_ca_file());
    }

    /// A secret written as several lines is replaced whole.
    #[test]
    fn a_multi_line_secret_is_replaced_whole() {
        let file = "pub const MQTT_CLIENT_KEY: &str = \"-----BEGIN EC PRIVATE KEY-----\nabc\n-----END EC PRIVATE KEY-----\";\npub const MQTT_CLIENT_CERT: &str = \"\";\n";
        let multi = "pub const MQTT_CLIENT_KEY: &str = r\"-----BEGIN EC PRIVATE KEY-----\nabc\n-----END EC PRIVATE KEY-----\n\";\npub const MQTT_CLIENT_CERT: &str = \"\";\n";
        for f in [file, multi] {
            let out = write_secret(f, CLIENT_SECRETS[1], "");
            assert_eq!(out, "pub const MQTT_CLIENT_KEY: &str = \"\";\npub const MQTT_CLIENT_CERT: &str = \"\";\n", "{f}");
        }
        // The CA file an earlier IDE wrote still counts as untouched.
        let old = mqtt_ca_file().replace("every certificate in it is trusted.", "only the first certificate is used.");
        assert!(is_pristine(MQTT_CA, &old));
    }

    /// The signature the tab checks is the template's.
    #[test]
    fn the_publish_signature_is_the_templates() {
        assert!(lf(MQTT_TAIL).contains(MQTT_PUBLISH_SIGNATURE));
    }

    /// panic.rs as the tree re-splices it is still the IDE's: switched off,
    /// it goes; edited, it stays.
    #[test]
    fn panic_rs_stays_pristine_through_a_splice() {
        let m = mcu("esp32c3", Runtime::Async, true);
        let body = config_files_for(&m)
            .into_iter()
            .find(|(n, _)| n == PANIC)
            .map(|(_, b)| b)
            .unwrap();
        let (block, tail) = body.split_once(GEN_END_CFG).unwrap();
        let spliced = format!("{block}{GEN_END_CFG}\n\n{}", tail.trim_start());
        assert!(is_pristine(PANIC, &spliced));
        let tree = vec![("src/pins/configs/panic.rs".to_owned(), spliced.clone())];
        assert!(kept_paths(&[], &tree).is_empty());
        let edited = vec![("src/pins/configs/panic.rs".to_owned(), spliced.replace("1_000;", "500;"))];
        assert_eq!(kept_paths(&[], &edited), ["src/pins/configs/panic.rs"]);
        assert!(runtime_free(PANIC));
    }
}
