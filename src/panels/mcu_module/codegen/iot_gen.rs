//! Code for the IoT tab: `src/pins/configs/{net,wifi,mqtt,secrets}.rs`, the
//! `main.rs` lines that start them, and the credentials file's rules.
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
//! - `secrets.rs`: SSID and passwords. Written ONCE, never spliced - the file
//!   is the store, and the project's `.gitignore` lists it.

use crate::panels::mcu_module::iot::{self, Active, IotConfig, IpConfig, MqttConfig, Platform};
use crate::panels::mcu_module::mcu::Mcu;

pub const NET: &str = "net.rs";
pub const WIFI: &str = "wifi.rs";
pub const MQTT: &str = "mqtt.rs";
pub const SECRETS: &str = "secrets.rs";

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

/// Every editable half, for `is_pristine`.
const TAILS: [(&str, &str); 4] = [
    (WIFI, WIFI_ESP_TAIL),
    (WIFI, WIFI_CYW43_TAIL),
    (NET, NET_TAIL),
    (MQTT, MQTT_TAIL),
];

const GEN_BEGIN_CFG: &str = "// <<< GENERATED>>>";
const GEN_END_CFG: &str = "// <<< GENERATED END >>>";

/// The secrets the tab edits, in the order they are written.
pub const SECRET_NAMES: [&str; 4] = ["WIFI_SSID", "WIFI_PASSWORD", "MQTT_USERNAME", "MQTT_PASSWORD"];

/// What a fresh `secrets.rs` holds: every value empty.
pub fn secrets_body() -> String {
    let mut o = String::new();
    o.push_str("// Credentials for the IoT tab. This file is listed in .gitignore, so it stays\n");
    o.push_str("// on this machine: edit the values here or in the tab, never in a commit.\n");
    for name in SECRET_NAMES {
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
    config_files(&mcu.iot, active)
}

/// The files for `cfg` on `active` - see [`config_files_for`].
pub fn config_files(cfg: &IotConfig, active: Active) -> Vec<(String, String)> {
    let mut out = vec![
        (NET.to_owned(), net_file(&cfg.ip)),
        (WIFI.to_owned(), wifi_file(active.platform)),
        (SECRETS.to_owned(), secrets_body()),
    ];
    if let (true, Some(m)) = (active.mqtt, &cfg.mqtt) {
        out.push((MQTT.to_owned(), mqtt_file(m)));
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

fn net_file(ip: &IpConfig) -> String {
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str("// IP settings (from the IoT tab) — auto-updated; edit them in the tab.\n");
    o.push_str(&format!("pub const DHCP: bool = {};\n", ip.dhcp));
    o.push_str(&format!("pub const STATIC_IP: [u8; 4] = {};\n", bytes4(ip.address)));
    o.push_str(&format!("pub const PREFIX_LEN: u8 = {};\n", ip.prefix.min(32)));
    o.push_str(&format!("pub const GATEWAY: [u8; 4] = {};\n", bytes4(ip.gateway)));
    o.push_str(&format!("pub const DNS: [u8; 4] = {};\n", bytes4(ip.dns)));
    o.push_str("/// Sockets embassy-net can hold at once: DHCP and DNS take one each, MQTT one.\n");
    o.push_str("pub const SOCKETS: usize = 4;\n");
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(NET_TAIL);
    o
}

fn wifi_file(platform: Platform) -> String {
    let (radio, tail) = match platform {
        Platform::Esp => ("the chip's own radio (esp-radio)", WIFI_ESP_TAIL),
        Platform::Cyw43 => ("the CYW43 radio beside the chip", WIFI_CYW43_TAIL),
    };
    let mut o = String::new();
    o.push_str(GEN_BEGIN_CFG);
    o.push('\n');
    o.push_str(&format!("// Wi-Fi station (from the IoT tab) on {radio}.\n"));
    o.push_str("// The network name and password are in `secrets.rs`, which git ignores.\n");
    o.push_str(GEN_END_CFG);
    o.push('\n');
    o.push_str(tail);
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
    o.push_str(MQTT_TAIL);
    o
}

// ── The credentials file ─────────────────────────────────────────────────────

/// A file `sync_config_files` writes when it is missing and never touches
/// again: the user's values live in it, nowhere else.
pub fn write_once(name: &str) -> bool {
    name == SECRETS
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
    let mut out: Vec<String> = file
        .lines()
        .map(|l| {
            if !found && l.trim_start().starts_with(&prefix) {
                found = true;
                line.clone()
            } else {
                l.to_owned()
            }
        })
        .collect();
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

/// Every secret still empty: what `secrets_body` wrote.
fn secrets_pristine(content: &str) -> bool {
    SECRET_NAMES
        .iter()
        .all(|n| read_secret(content, n).is_some_and(|v| v.is_empty()))
}

/// Is `content` still exactly a template below its markers? Then the IoT tab
/// switched off may let it go; otherwise the user wrote in it and it stays.
fn is_pristine(name: &str, content: &str) -> bool {
    if name == SECRETS {
        return secrets_pristine(content);
    }
    let Some((_, tail)) = content.split_once(GEN_END_CFG) else {
        return false;
    };
    // The newline that ends the marker line, then the template as written.
    let tail = tail.replace("\r\n", "\n");
    let tail = tail.strip_prefix('\n').unwrap_or(&tail);
    TAILS.iter().any(|(n, t)| *n == name && *t == tail)
}

/// The `pins/configs/` paths the tree must keep although `files` no longer
/// generates them: an IoT file the user wrote in - above all `secrets.rs`
/// with a password in it. A pristine one goes like any pruned config file.
pub fn kept_paths(files: &[(String, String)], tree: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for name in [NET, WIFI, MQTT, SECRETS] {
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
            esp_main_with(code, mcu.iot.heap_kib, active.mqtt)
        }
        _ => code,
    }
}

fn esp_main_with(code: String, heap_kib: u32, mqtt: bool) -> String {
    use super::{GEN_BEGIN, GEN_END};
    let (Some(begin), Some(end)) = (code.find(GEN_BEGIN), code.find(GEN_END)) else {
        return code;
    };
    let mut block = code[begin..end].to_owned();
    // 1. The heap, before the scheduler starts.
    let heap = format!(
        "\n    // ── Heap (IoT tab) ──\n\
         \x20   // esp-radio allocates its buffers here, so it has to exist before the\n\
         \x20   // scheduler starts.\n\
         \x20   esp_alloc::heap_allocator!(size: {} * 1024);\n",
        heap_kib.max(1)
    );
    if let Some(at) = block.find(ESP_START_MARK) {
        block.insert_str(at, &heap);
    }
    // 2. The spawner is named: the IoT tasks are spawned on it.
    block = block.replacen(
        "async fn main(_spawner: Spawner)",
        "async fn main(spawner: Spawner)",
        1,
    );
    // 3. The bring-up, last in the block: it only needs `peripherals.WIFI`.
    block.push_str("    // ── IoT (IoT tab) ──\n");
    if mqtt {
        block.push_str("    let net_stack = pins::configs::wifi::init(spawner, peripherals.WIFI);\n");
        block.push_str("    pins::configs::mqtt::start(spawner, net_stack);\n\n");
    } else {
        block.push_str("    // `net_stack` opens sockets: `embassy_net::tcp::TcpSocket::new(net_stack, ..)`.\n");
        block.push_str("    #[allow(unused_variables)]\n");
        block.push_str("    let net_stack = pins::configs::wifi::init(spawner, peripherals.WIFI);\n\n");
    }
    format!("{}{block}{}", &code[..begin], &code[end..])
}

/// The Pico W lines that replace the LED's `let mut wl_led = control;` once
/// the radio carries Wi-Fi: `control` belongs to the Wi-Fi task then.
pub fn cyw43_main_lines(mqtt: bool) -> String {
    let mut o = String::new();
    o.push_str("    // ── IoT (IoT tab) ──\n");
    o.push_str("    // The radio's `control` belongs to the Wi-Fi task from here on, so the LED\n");
    o.push_str("    // (GPIO0 on the radio) is switched through it: `pins::configs::wifi::set_led(true)`.\n");
    if mqtt {
        o.push_str("    let net_stack = pins::configs::wifi::init(spawner, net_device, control);\n");
        o.push_str("    pins::configs::mqtt::start(spawner, net_stack);\n");
    } else {
        o.push_str("    #[allow(unused_variables)]\n");
        o.push_str("    let net_stack = pins::configs::wifi::init(spawner, net_device, control);\n");
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

    /// The four files, and MQTT's only while it is on.
    #[test]
    fn the_files_follow_the_tab() {
        let names = |m: &Mcu| -> Vec<String> {
            config_files_for(m).into_iter().map(|(n, _)| n).collect()
        };
        assert_eq!(names(&mcu("esp32c3", Runtime::Async, true)), ["net.rs", "wifi.rs", "secrets.rs", "mqtt.rs"]);
        assert_eq!(names(&mcu("esp32c3", Runtime::Async, false)), ["net.rs", "wifi.rs", "secrets.rs"]);
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
        assert!(wifi(&mcu("esp32c3", Runtime::Async, false)).contains("esp_radio::wifi::new("));
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
        assert!(at("esp_rtos::start(") < at("pins::configs::wifi::init(spawner, peripherals.WIFI)"));
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

    /// The tab refuses a subscription longer than the template's `Message`
    /// carries - the two numbers must be the same one.
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
}
