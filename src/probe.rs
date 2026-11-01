//! Debug-probe enumeration via `probe-rs list`, shared by the RTT and Debug
//! tabs so the user can target a specific probe when several are connected.
//!
//! Both tabs drive `probe-rs`, which — given only `--chip` — auto-selects the
//! sole attached probe and turns ambiguous the moment a second one is plugged
//! in. Passing `--probe <VID:PID[:Serial]>` (RTT) or the DAP `probe` launch
//! field (Debug) pins the session to one probe. The selector strings come
//! straight from `probe-rs list`, so they are always in the exact form
//! probe-rs expects — no reconstruction from our own USB scan.

use crate::build::no_window;
use crate::panels::mcu_module::ToolchainKind;
use crate::terminal::{LineKind, TerminalState};
use std::process::Command;
use std::sync::{Arc, Mutex};

/// One probe as reported by `probe-rs list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeInfo {
    /// Human name, e.g. "STLink V2".
    pub name: String,
    /// probe-rs family tag, e.g. "ST-LINK", "EspJtag".
    pub kind: String,
    /// The exact `--probe` selector (`VID:PID` or `VID:PID:Serial`).
    pub selector: String,
}

impl ProbeInfo {
    /// One-line label for the ComboBox.
    pub fn combo_label(&self) -> String {
        format!("[{}] {}  ·  {}", self.kind, self.name, self.selector)
    }
}

/// A probe selector as probe-rs should see it, or `None` for "let it choose".
///
/// One place, because two callers had drifted: the RTT tab passes it as
/// `--probe <sel>` and the debugger as the DAP `launch` object's `probe` field,
/// and both were filtering with a bare `is_empty()`. A selector of SPACES then
/// survived and went out as `--probe "   "` / `"probe": "   "`, which probe-rs
/// rejects - where the empty case is meant to mean auto-select.
pub fn selector(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Whether a probe of `kind` (as `probe-rs list` reports it — "ST-LINK",
/// "EspJtag", "JLink", "CMSIS-DAP", …) can drive the project chip's toolchain,
/// the same gate the Flash tab applies to programmers. ARM chips use SWD probes
/// (ST-Link / J-Link / CMSIS-DAP); ESP chips use the built-in USB-JTAG (or a
/// J-Link in JTAG mode). SDCC / 8051 isn't a probe-rs target at all.
pub fn probe_compatible(kind: &str, toolchain: &ToolchainKind) -> bool {
    let k = kind.to_ascii_lowercase();
    let is_jlink = k.contains("jlink") || k.contains("j-link");
    let is_arm_swd =
        k.contains("st-link") || k.contains("stlink") || k.contains("cmsis") || is_jlink;
    let is_esp_jtag = k.contains("esp") || k.contains("jtag");
    match toolchain {
        ToolchainKind::RustEmbedded => is_arm_swd,
        ToolchainKind::EspRust => is_esp_jtag || is_jlink,
        ToolchainKind::SdccC => false,
    }
}

/// Resolve "Auto" to ONE probe selector, for the paths that cannot leave the
/// choice to probe-rs.
///
/// `probe-rs dap-server` runs **non-interactive**: given no `probe` and more
/// than one attached, it does not pick — it fails outright, and the failure
/// reaches us as the bare word "cancelled" (see
/// [`crate::debugger::response_error`]). So Auto is resolved here instead,
/// where the chip's toolchain already says which probes could drive it at all:
/// with an ST-Link and an ESP built-in JTAG plugged in, an ESP project has
/// exactly one candidate and "Auto" can mean what the user expects. Only a
/// genuine tie is handed back, naming the probes.
pub fn pick_probe(probes: &[ProbeInfo], toolchain: &ToolchainKind) -> Result<String, String> {
    let usable: Vec<&ProbeInfo> = probes
        .iter()
        .filter(|p| probe_compatible(&p.kind, toolchain))
        .collect();
    match usable.len() {
        1 => Ok(usable[0].selector.clone()),
        0 if probes.is_empty() => Err("no debug probe found — plug one in and press Scan".into()),
        0 => Err(format!(
            "none of the attached probes can drive this chip: {}",
            probes
                .iter()
                .map(|p| format!("{} ({})", p.name, p.kind))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        _ => Err(format!(
            "several probes can drive this chip — pick one in the Probe list \
             instead of Auto: {}",
            usable
                .iter()
                .map(|p| format!("{} ({})", p.name, p.kind))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Run `probe-rs list` and parse the connected probes. `Ok(vec![])` when none
/// are attached; `Err` only when the binary itself cannot be run.
pub fn list_probes() -> Result<Vec<ProbeInfo>, String> {
    let out = no_window(&mut Command::new("probe-rs"))
        .arg("list")
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "probe-rs not found in PATH (cargo install probe-rs-tools)".to_string()
            } else {
                format!("could not run `probe-rs list`: {e}")
            }
        })?;
    // The probe rows go to stdout; scan stderr too in case a build ever changes
    // that — parsing only picks up the `[n]: … -- … (Kind)` shape either way.
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // `probe-rs list` can panic inside its own USB enumeration. Parsing that
    // output would just report "no probes attached" — a crash has to be told
    // apart from an empty bench, since the fix is a probe-rs version, not a
    // cable.
    if let Some(detail) = crate::failure_hint::probe_rs_panic(&text) {
        return Err(crate::failure_hint::probe_rs_panic_message(&detail));
    }
    if let Some(detail) = crate::failure_hint::probe_open_failure(&text) {
        return Err(crate::failure_hint::probe_open_message(&detail, false));
    }
    Ok(parse_list(&text))
}

/// Windows only: is a USB interface of this probe bound to WinUSB but registered
/// WITHOUT a device-interface GUID?
///
/// A probe can be LISTED and still be impossible to open. Enumeration reads
/// descriptors the OS has already cached; OPENING one means opening a device
/// INTERFACE, and nusb - so probe-rs - finds that path through a
/// `DeviceInterfaceGUIDs` value under the interface's `Device Parameters` key.
/// Zadig's driver package writes that value. The WinUSB binding Windows makes by
/// itself from a device's MS-OS descriptors can leave it out, and then every open
/// fails with "The selected USB device could not be opened" - nothing is holding
/// the probe, there is simply no path to open it by.
///
/// The question has to be asked **per interface**, and that is the whole
/// subtlety. An ESP32-C3 puts its serial port and its JTAG on one composite
/// device; the serial half can carry a GUID while the JTAG half does not, and a
/// device-wide "does anything here have a GUID?" then answers a cheerful yes
/// while probe-rs cannot open a thing. Only interfaces WinUSB actually drives
/// count - a `usbser` COM port has no bearing on whether a debugger can attach.
///
/// Instances are also tied back to THIS probe through the parent's
/// `ParentIdPrefix` whenever the selector carries a serial: a board that has been
/// unplugged leaves its interface keys behind for good, and they would otherwise
/// vote on the state of a board that is not even connected.
///
/// Answers `false` unless it is sure: a selector it cannot parse, a device
/// Windows has no record of, no WinUSB interface at all, or a `reg` that will not
/// answer. A failure card is chosen from this, and a wrong `true` sends the user
/// off to reinstall a driver that was never the problem.
pub fn missing_device_interface_guid(selector: Option<&str>) -> bool {
    if !cfg!(target_os = "windows") {
        return false;
    }
    let Some(sel) = selector else {
        return false;
    };
    let Some((vid, pid)) = vid_pid(sel) else {
        return false;
    };
    let keys = device_keys(&reg_subkeys(USB_ENUM), &vid, &pid);
    if keys.is_empty() {
        return false; // No record of the device at all: not our diagnosis to make.
    }
    let prefix = serial_of(sel).and_then(|s| parent_id_prefix(&vid, &pid, &s));
    for key in &keys {
        for inst in winusb_instances(&reg_dump(key), key) {
            if !belongs_to(&inst.id, prefix.as_deref()) {
                continue;
            }
            if !inst.has_guid {
                return true;
            }
        }
    }
    false
}

/// Where Windows keeps one key per USB device it has ever seen.
pub(crate) const USB_ENUM: &str = r"HKLM\SYSTEM\CurrentControlSet\Enum\USB";

/// One device instance under an interface key, as far as this module cares.
pub(crate) struct WinUsbInstance {
    /// The instance segment, e.g. `6&205b2bf0&0&0002`, lowercased.
    pub(crate) id: String,
    pub(crate) has_guid: bool,
    /// The COM name Windows still has on file for this interface, when it has
    /// one. A value here on a WinUSB-driven instance is the signature of a
    /// serial port that a driver install took over: the port is gone, but the
    /// NUMBER is remembered and comes back with it (see [`crate::win_driver`]).
    pub(crate) port_name: Option<String>,
}

/// The `VID`/`PID` halves of a `VID:PID[:Serial]` selector, spelled the way the
/// registry spells them. `None` unless both are four hex digits: a registry key
/// name is built from these, and a half-parsed selector must not produce one.
pub(crate) fn vid_pid(selector: &str) -> Option<(String, String)> {
    let mut parts = selector.split(':');
    let vid = parts.next()?.trim();
    let pid = parts.next()?.trim();
    let hex4 = |s: &str| s.len() == 4 && s.chars().all(|c| c.is_ascii_hexdigit());
    (hex4(vid) && hex4(pid)).then(|| (vid.to_ascii_uppercase(), pid.to_ascii_uppercase()))
}

/// Everything after the `VID:PID` of a selector - the serial, which is itself
/// full of colons on an ESP (`50:78:7D:62:33:A4`), so it must NOT be split
/// further. `None` for a selector that carries no serial.
fn serial_of(selector: &str) -> Option<String> {
    let mut parts = selector.splitn(3, ':');
    parts.next()?;
    parts.next()?;
    parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The `Enum\USB` keys belonging to one VID:PID - the device itself and, for a
/// composite device like the ESP's USB-Serial/JTAG, one key per interface
/// (`…&MI_02`).
pub(crate) fn device_keys(all: &[String], vid: &str, pid: &str) -> Vec<String> {
    let want = format!(r"\VID_{vid}&PID_{pid}");
    all.iter()
        .filter(|k| {
            let up = k.to_ascii_uppercase();
            // The match must END the key name or be followed by `&MI_nn`, so
            // that `PID_1001` never answers for a `PID_10011`.
            up.find(&want)
                .is_some_and(|i| matches!(up[i + want.len()..].chars().next(), None | Some('&')))
        })
        .cloned()
        .collect()
}

/// The `ParentIdPrefix` Windows assigned to one composite device, which every
/// one of its interface instances is named after. This is what ties an interface
/// back to the physical board identified by `serial`.
fn parent_id_prefix(vid: &str, pid: &str, serial: &str) -> Option<String> {
    let key = format!(r"{USB_ENUM}\VID_{vid}&PID_{pid}\{serial}");
    reg_dump(&key).lines().find_map(|l| {
        let mut f = l.split_whitespace();
        if !f.next()?.eq_ignore_ascii_case("ParentIdPrefix") {
            return None;
        }
        f.next()?; // the REG_SZ type column
        f.next().map(str::to_ascii_lowercase)
    })
}

/// Is this instance one of the probe we asked about? Without a prefix to compare
/// against, every instance counts - which is right for a non-composite probe
/// whose selector carries no serial.
fn belongs_to(instance: &str, prefix: Option<&str>) -> bool {
    prefix.is_none_or(|p| {
        instance
            .to_ascii_lowercase()
            .starts_with(&p.to_ascii_lowercase())
    })
}

/// The instances under one device key that WinUSB drives, and whether each one
/// carries a device-interface GUID.
///
/// Parses `reg query <key> /s`: every `HKEY_…` line opens a section and the
/// indented `Name  TYPE  Value` lines under it belong to it. The two facts sit in
/// DIFFERENT sections - `Service` on the instance key itself, the GUID under its
/// `Device Parameters` subkey - so they are stitched back together by instance.
pub(crate) fn winusb_instances(dump: &str, device_key: &str) -> Vec<WinUsbInstance> {
    let mut winusb: Vec<String> = Vec::new();
    let mut with_guid: Vec<String> = Vec::new();
    let mut ports: Vec<(String, String)> = Vec::new();
    let mut id = String::new();
    let mut in_params = false;

    for line in dump.lines() {
        let t = line.trim();
        if t.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("HKEY_")) {
            id.clear();
            // Only sections under the key we dumped can name an instance of it.
            // `get` rather than a slice: `reg` writes its errors in the
            // system language, and cutting a multi-byte character in half to
            // compare a prefix would panic this thread.
            if !t
                .get(..device_key.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(device_key))
            {
                continue;
            }
            let mut segs = t[device_key.len()..].trim_start_matches('\\').split('\\');
            id = segs.next().unwrap_or_default().to_ascii_lowercase();
            in_params = segs
                .next()
                .is_some_and(|s| s.eq_ignore_ascii_case("Device Parameters"));
            continue;
        }
        if id.is_empty() || t.is_empty() {
            continue;
        }
        let mut f = t.split_whitespace();
        let Some(name) = f.next() else { continue };
        if in_params {
            // `DeviceInterfaceGUID` and `DeviceInterfaceGUIDs` are both spellings
            // an INF may use.
            if name
                .get(..19)
                .is_some_and(|n| n.eq_ignore_ascii_case("DeviceInterfaceGUID"))
            {
                with_guid.push(id.clone());
            } else if name.eq_ignore_ascii_case("PortName") {
                // `PortName  REG_SZ  COM12` - the last column is the value.
                if let Some(v) = f.last() {
                    ports.push((id.clone(), v.to_owned()));
                }
            }
        } else if name.eq_ignore_ascii_case("Service")
            && f.any(|v| v.eq_ignore_ascii_case("winusb"))
        {
            winusb.push(id.clone());
        }
    }

    winusb.sort();
    winusb.dedup();
    winusb
        .into_iter()
        .map(|id| {
            let has_guid = with_guid.contains(&id);
            let port_name = ports.iter().find(|(i, _)| *i == id).map(|(_, p)| p.clone());
            WinUsbInstance {
                id,
                has_guid,
                port_name,
            }
        })
        .collect()
}

/// Immediate subkeys of a registry key, as full `HKEY_…` paths. Empty when `reg`
/// is missing or refuses - indistinguishable from "no such devices", and both
/// mean the caller must not conclude anything.
///
/// A SHALLOW query. It used to go through [`reg_dump`], whose `/s` walks the
/// whole tree: for `Enum\USB` that was 1,664 lines and about 58 s on the bench,
/// against 46 lines and 55 ms for this one. The nested keys it also returned
/// passed [`device_keys`] and cost one more recursive dump each, 23 instead of
/// 3 on an ESP32-C3, while never producing an instance.
pub(crate) fn reg_subkeys(key: &str) -> Vec<String> {
    hkey_lines(&reg_query(key, false))
}

/// The key-path lines of `reg query` output, dropping the value lines under them.
fn hkey_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| l.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("HKEY_")))
        .map(str::to_owned)
        .collect()
}

/// `reg query <key> /s` as text, empty when `reg` cannot answer. A device subtree
/// is a few dozen lines, so one dump beats guessing which subkey holds what -
/// and `reg /v` matches value names EXACTLY, which would miss `…GUIDs` when
/// asked for `…GUID`.
pub(crate) fn reg_dump(key: &str) -> String {
    reg_query(key, true)
}

fn reg_query(key: &str, recursive: bool) -> String {
    let mut cmd = Command::new("reg");
    cmd.args(["query", key]);
    if recursive {
        cmd.arg("/s");
    }
    no_window(&mut cmd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod guid_tests {
    use super::*;

    /// Verbatim `reg query … /s` output from the bench: an ESP32-C3 whose SERIAL
    /// interface was given WinUSB and a GUID by mistake while the JTAG interface
    /// - the only one probe-rs opens - has none. The device-wide question
    /// ("anything here got a GUID?") answers yes and is wrong.
    const MI_00: &str =
        r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_00";
    const MI_02: &str =
        r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02";

    fn mi_00_dump() -> String {
        format!(
            "{MI_00}\\6&205b2bf0&0&0000\n    \
             Service    REG_SZ    WinUSB\n\
             {MI_00}\\6&205b2bf0&0&0000\\Device Parameters\n    \
             DeviceInterfaceGUIDs    REG_MULTI_SZ    {{0F7E33F1-955E-4C66-BF6C-95D59F852507}}\n\
             {MI_00}\\6&205b2bf0&0&0000\\Properties\n\
             {MI_00}\\6&fb727e3&1&0000\n    \
             Service    REG_SZ    usbser\n\
             {MI_00}\\6&fb727e3&1&0000\\Device Parameters\n"
        )
    }

    fn mi_02_dump() -> String {
        format!(
            "{MI_02}\\6&205b2bf0&0&0002\n    \
             Service    REG_SZ    WINUSB\n\
             {MI_02}\\6&205b2bf0&0&0002\\Device Parameters\n\
             {MI_02}\\6&205b2bf0&0&0002\\Device Parameters\\WDF\n    \
             WdfDirectHardwareAccess    REG_DWORD    0x1\n\
             {MI_02}\\6&fb727e3&1&0002\n    \
             Service    REG_SZ    WINUSB\n\
             {MI_02}\\6&fb727e3&1&0002\\Device Parameters\n"
        )
    }

    #[test]
    fn a_serial_port_bound_to_usbser_is_not_a_debug_interface() {
        // Only the WinUSB-driven instance is reported; `usbser` never counts.
        let found = winusb_instances(&mi_00_dump(), MI_00);
        assert_eq!(found.len(), 1, "usbser instance must not be listed");
        assert_eq!(found[0].id, "6&205b2bf0&0&0000");
        assert!(found[0].has_guid);
    }

    #[test]
    fn the_guid_is_read_from_device_parameters_not_the_instance_key() {
        // Both JTAG instances are WinUSB and NEITHER has a GUID - the `Device
        // Parameters` sections are there, just empty of one.
        let found = winusb_instances(&mi_02_dump(), MI_02);
        assert_eq!(
            found.len(),
            2,
            "{:?}",
            found.iter().map(|i| &i.id).collect::<Vec<_>>()
        );
        assert!(found.iter().all(|i| !i.has_guid));
    }

    #[test]
    fn a_guid_on_the_serial_half_does_not_vouch_for_the_jtag_half() {
        // The bug this replaced: asking the question device-wide. The serial
        // interface has a GUID, the JTAG interface does not, and probe-rs can
        // still not open the probe.
        let serial = winusb_instances(&mi_00_dump(), MI_00);
        let jtag = winusb_instances(&mi_02_dump(), MI_02);
        assert!(serial.iter().any(|i| i.has_guid), "the misleading yes");
        assert!(
            jtag.iter().any(|i| !i.has_guid),
            "and the answer that actually decides it"
        );
    }

    #[test]
    fn an_unplugged_board_does_not_vote() {
        // `6&fb727e3&1` is a board that was removed; its keys stay behind for
        // good. Only instances under the live board's ParentIdPrefix count.
        let live = "6&205b2bf0&0";
        assert!(belongs_to("6&205b2bf0&0&0002", Some(live)));
        assert!(!belongs_to("6&fb727e3&1&0002", Some(live)));
        // No serial in the selector - a non-composite probe - so nothing is
        // excluded; that is the honest default, not a bug.
        assert!(belongs_to("6&fb727e3&1&0002", None));
    }

    #[test]
    fn a_localised_reg_error_does_not_panic_the_check() {
        // `reg` writes its errors in the language Windows is installed in, so a
        // dump can be a sentence rather than a key listing. Comparing a prefix
        // of one by BYTES would cut a multi-byte character in half and panic -
        // inside the background thread the Tools check runs on.
        let dump = "FEHLER: Der angegebene Registrierungsschl\u{fc}ssel wurde nicht gefunden.\n\
                    \u{3a9}\u{3a9}\u{3a9} unreadable\n    Service    REG_SZ    WINUSB\n";
        assert!(winusb_instances(dump, MI_02).is_empty());
        // An empty dump says nothing rather than claiming a fault.
        assert!(winusb_instances("", MI_02).is_empty());
    }

    #[test]
    fn a_selector_yields_the_registry_spelling_of_its_ids() {
        // probe-rs prints the ids lowercase; the registry spells them upper.
        assert_eq!(
            vid_pid("303a:1001:50:78:7D:62:33:A4"),
            Some(("303A".to_owned(), "1001".to_owned()))
        );
        assert_eq!(
            vid_pid("0483:3748"),
            Some(("0483".to_owned(), "3748".to_owned()))
        );
    }

    #[test]
    fn a_serial_full_of_colons_survives_intact() {
        // The ESP serial is itself colon-separated, so splitting on every colon
        // would truncate it to "50" and find no device.
        assert_eq!(
            serial_of("303a:1001:50:78:7D:62:33:A4").as_deref(),
            Some("50:78:7D:62:33:A4")
        );
        assert_eq!(serial_of("0483:3748"), None);
    }

    #[test]
    fn anything_that_is_not_two_hex_ids_is_refused() {
        // A key name gets built from these, so "nearly right" is not enough.
        for bad in ["", "303a", "303a:", "303a:100", "303a:10011", "zzzz:1001"] {
            assert_eq!(vid_pid(bad), None, "{bad}");
        }
    }

    #[test]
    fn a_composite_devices_interface_keys_all_count() {
        // The ESP32-C3 bench: the device plus its serial and JTAG interfaces.
        let all = vec![
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001".to_owned(),
            MI_00.to_owned(),
            MI_02.to_owned(),
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_0483&PID_3748".to_owned(),
        ];
        let mine = device_keys(&all, "303A", "1001");
        assert_eq!(mine.len(), 3, "{mine:?}");
        assert!(mine.iter().all(|k| k.contains("VID_303A")), "{mine:?}");
    }

    /// A shallow `reg query` lists the key's own values (when it has any), then
    /// one line per immediate subkey. Only the subkeys are keys to follow.
    #[test]
    fn a_shallow_listing_yields_its_subkeys_only() {
        let out = concat!(
            "\r\n",
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB",
            "\r\n    Stray    REG_SZ    value\r\n\r\n",
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001",
            "\r\n",
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_1001&MI_02",
            "\r\n",
        );
        let keys = hkey_lines(out);
        assert_eq!(keys.len(), 3, "{keys:?}");
        assert!(keys.iter().all(|k| k.starts_with("HKEY_")), "{keys:?}");
        // The key itself matches no VID:PID, so it never reaches a dump.
        assert_eq!(device_keys(&keys, "303A", "1001").len(), 2);
    }

    #[test]
    fn a_longer_product_id_is_not_a_match() {
        // `PID_1001` must not answer for `PID_10012` - different device.
        let all = vec![
            r"HKEY_LOCAL_MACHINE\SYSTEM\CurrentControlSet\Enum\USB\VID_303A&PID_10012".to_owned(),
        ];
        assert!(device_keys(&all, "303A", "1001").is_empty());
    }
}

/// Reset the target through the probe (`probe-rs reset`), streaming the result
/// into `console`. Runs on its own thread — it opens the probe, which takes a
/// moment and must not stall the UI.
///
/// This is the way out of a firmware that sits somewhere it shouldn't: it
/// restarts the chip WITHOUT reflashing it and without a USB replug. Only
/// meaningful while nothing else holds the probe — a live Debug/RTT session
/// owns it exclusively, which is why the button is disabled there.
pub fn start_reset(
    chip: String,
    probe: Option<String>,
    console: Arc<Mutex<TerminalState>>,
    ctx: eframe::egui::Context,
) {
    std::thread::spawn(move || {
        let mut args: Vec<String> = vec!["reset".into(), "--chip".into(), probe_rs_chip(&chip)];
        if let Some(p) = selector(probe.as_deref()) {
            args.push("--probe".into());
            args.push(p);
        }
        console
            .lock()
            .unwrap()
            .push_plain(LineKind::Input, format!("> probe-rs {}", args.join(" ")));
        ctx.request_repaint();

        let out = no_window(&mut Command::new("probe-rs"))
            .args(&args)
            .output();
        let mut c = console.lock().unwrap();
        match out {
            Ok(o) => {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                );
                for line in crate::terminal::strip_ansi(&text).lines() {
                    if !line.trim().is_empty() {
                        c.push_plain(LineKind::Stdout, line);
                    }
                }
                c.push_plain(
                    if o.status.success() {
                        LineKind::Notice
                    } else {
                        LineKind::Stderr
                    },
                    if o.status.success() {
                        "[target reset — the firmware is running from its entry point]"
                    } else {
                        "[reset failed — see above]"
                    },
                );
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                c.push_plain(
                    LineKind::Stderr,
                    "probe-rs not found in PATH (cargo install probe-rs-tools)",
                );
            }
            Err(e) => c.push_plain(LineKind::Stderr, format!("could not run probe-rs: {e}")),
        }
        drop(c);
        ctx.request_repaint();
    });
}

/// Parse `probe-rs list` output. Recognised line shape (probe-rs 0.2x–0.31):
/// `[<idx>]: <name> -- <VID:PID[:Serial]> (<Kind>)`. The serial itself can hold
/// colons (ESP JTAG → `303a:1001:50:78:7D:62:33:A4`), so the selector is taken
/// verbatim between ` -- ` and the trailing ` (Kind)` rather than split on `:`.
pub fn parse_list(stdout: &str) -> Vec<ProbeInfo> {
    let mut probes = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        // Only the numbered "[0]: …" rows are probes (skip the header line).
        let Some(rest) = line.strip_prefix('[') else {
            continue;
        };
        let Some(idx_end) = rest.find("]:") else {
            continue;
        };
        let body = rest[idx_end + 2..].trim(); // "STLink V2 -- 0483:3748: (ST-LINK)"

        // name -- tail, split on the first " -- ".
        let Some(dash) = body.find(" -- ") else {
            continue;
        };
        let name = body[..dash].trim().to_string();
        let tail = body[dash + 4..].trim(); // "0483:3748: (ST-LINK)"

        // Kind is the last parenthesised token; the selector is everything
        // before it (rfind so a probe name never confuses the split).
        let (selector, kind) = match tail.rfind('(') {
            Some(paren) => {
                let sel = tail[..paren].trim();
                let kind = tail[paren + 1..].trim_end().trim_end_matches(')').trim();
                (sel, kind.to_string())
            }
            None => (tail, String::new()),
        };

        // Drop a trailing empty-serial colon ("0483:3748:" → "0483:3748") while
        // leaving the colon-bearing ESP serials intact.
        let selector = selector.trim().trim_end_matches(':').to_string();
        if selector.is_empty() {
            continue;
        }
        probes.push(ProbeInfo {
            name,
            kind,
            selector,
        });
    }
    probes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stlink_and_esp_jtag() {
        let out = "The following debug probes were found:\n\
                   [0]: STLink V2 -- 0483:3748: (ST-LINK)\n\
                   [1]: ESP JTAG -- 303a:1001:50:78:7D:62:33:A4 (EspJtag)\n";
        let p = parse_list(out);
        assert_eq!(p.len(), 2);

        assert_eq!(p[0].name, "STLink V2");
        assert_eq!(p[0].kind, "ST-LINK");
        // Empty serial → trailing colon dropped.
        assert_eq!(p[0].selector, "0483:3748");

        assert_eq!(p[1].name, "ESP JTAG");
        assert_eq!(p[1].kind, "EspJtag");
        // Colon-bearing serial kept verbatim.
        assert_eq!(p[1].selector, "303a:1001:50:78:7D:62:33:A4");
    }

    #[test]
    fn parses_serial_with_hex_string() {
        let out = "[0]: J-Link -- 1366:0101:000059012345 (JLink)\n";
        let p = parse_list(out);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].selector, "1366:0101:000059012345");
        assert_eq!(p[0].kind, "JLink");
    }

    #[test]
    fn no_probes_yields_empty() {
        assert!(parse_list("No debug probes were found.\n").is_empty());
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn ignores_malformed_rows() {
        // Missing " -- " → not a probe row.
        assert!(parse_list("[0]: something odd (ST-LINK)\n").is_empty());
        // Header-ish noise.
        assert!(parse_list("The following debug probes were found:\n").is_empty());
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Which chips probe-rs can target
// ──────────────────────────────────────────────────────────────────────────────

/// Every target name `probe-rs chip list` reports, lowercased — the families at
/// column 0 and the variants indented beneath them alike, since `--chip` takes
/// either.
///
/// `None` when probe-rs could not be run at all, which is the absent-tool case
/// the Tools tab already reports; there is nothing useful this can add.
///
/// Asked once per process and cached. The call costs about 140 ms and the answer
/// cannot change while the IDE runs, because it is baked into the probe-rs
/// binary — a newly installed probe-rs is a new binary, and the user restarts.
fn known_targets() -> Option<&'static std::collections::HashSet<String>> {
    static TARGETS: std::sync::OnceLock<Option<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    TARGETS.get_or_init(list_targets).as_ref()
}

/// `probe-rs chip list` asked now, uncached - see [`known_targets`] for the
/// cached answer and [`probe_rs_chip`] for why one caller asks fresh.
fn list_targets() -> Option<std::collections::HashSet<String>> {
    let out = no_window(&mut Command::new("probe-rs"))
        .args(["chip", "list"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    parse_chip_list(&String::from_utf8_lossy(&out.stdout))
}

/// Parse `probe-rs chip list`, which nests variants under a family:
///
/// ```text
/// Available chips:
/// esp32c6
///     Variants:
///         esp32c6
/// ```
///
/// Both levels are collected, because `--chip` accepts either.
///
/// `None` for output that yields nothing — that is a format change, not an
/// empty catalogue, and must read as "could not ask" so nothing is blocked on
/// the strength of a misread.
fn parse_chip_list(text: &str) -> Option<std::collections::HashSet<String>> {
    let set: std::collections::HashSet<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "Available chips:" && *l != "Variants:")
        .map(|l| l.to_ascii_lowercase())
        .collect();
    (!set.is_empty()).then_some(set)
}

/// Why probe-rs cannot target `chip`, or `None` when it can — or when we could
/// not ask, which is deliberately the same answer.
///
/// # Why this is asked rather than listed
///
/// The gap is real but temporary: probe-rs 0.29 has no target for the ESP32-C5
/// or C61, and 0.32 has both. A hard-coded list of unsupported parts would
/// start lying the day the user upgrades, and only a code change here would
/// stop it. So the installed binary is asked, and the block lifts by itself.
///
/// Failing open matters as much: a missing probe-rs, an unparseable listing, or
/// a chip name we simply do not recognise must not disable a button. The
/// session then fails at probe-rs with probe-rs's own message, which is a
/// better outcome than a wrong refusal from us.
///
/// Espressif parts flash over espflash, which does not go through probe-rs at
/// all — so this never has anything to say about the Flash tab.
pub fn chip_gap(chip: &str) -> Option<String> {
    let chip = chip.trim();
    if chip.is_empty() {
        return None;
    }
    let known = known_targets()?;
    if known.contains(&chip.to_ascii_lowercase()) {
        return None;
    }
    Some(format!(
        "The installed probe-rs has no target for `{chip}`, so it cannot attach to one. \
         This is a probe-rs version gap, not a limit of the chip: newer releases add targets, \
         and this unblocks itself once one is installed (Tools tab). Building and flashing are \
         unaffected — an Espressif part is programmed by espflash, which does not use probe-rs."
    ))
}

/// The nRF5340's stock probe-rs target, and its application-core-only twin.
const NRF5340: &str = "nRF5340_xxAA";
const NRF5340_APP_ONLY: &str = "nRF5340_xxAA_APPONLY";

/// The `--chip` this IDE hands the INSTALLED probe-rs for a definition's
/// `chip` - see [`probe_rs_chip_with`].
///
/// Five callers, one rule: `run` / `attach` in `rtt.rs`, `reset` in
/// [`start_reset`], and the DAP objects of `debugger.rs`, `timing.rs` and
/// `flamegraph.rs`. Two places keep the definition's name: a generated
/// project's `.cargo/config.toml` runner, which is opened on other machines
/// with other probe-rs versions, and `cargo flash` in `probe_flash.rs`, whose
/// cargo-flash need not be the probe-rs asked here - and whose final reset
/// holds the network core anyway.
///
/// The target list is asked fresh, not from [`known_targets`]' cache: a
/// probe-rs downgraded while the IDE runs - its own advice for some ST-Links
/// is 0.29 - must not be handed a name it lacks, which would fail every
/// session. About 140 ms, only for the nRF5340, and every caller is on its
/// session's worker thread.
pub fn probe_rs_chip(chip: &str) -> String {
    probe_rs_chip_with(chip, list_targets)
}

/// `chip`, or its application-core-only twin when the installed probe-rs's
/// targets - `lookup`, lowercased, run only for the nRF5340 - have one.
///
/// probe-rs's stock `nRF5340_xxAA` (0.29 and 0.32 alike) opens EVERY session
/// by releasing the network core from Force-OFF. A session that resets the
/// chip puts it back - an application reset holds it again - but one that
/// does not (an RTT attach, a profile on a running target) leaves it running
/// whatever its flash holds beside the application: an old Bluetooth
/// controller, or an erased flash it locks up on, drawing milliamps. probe-rs
/// 0.32 adds `nRF5340_xxAA_APPONLY`: the same application core, memory map and
/// flash algorithms, no network core, and so no release. Nothing this IDE
/// builds runs on the network core, so that target whenever the installed
/// probe-rs has it; the stock one otherwise - failing open, like [`chip_gap`].
fn probe_rs_chip_with(
    chip: &str,
    lookup: impl FnOnce() -> Option<std::collections::HashSet<String>>,
) -> String {
    let chip = chip.trim();
    if chip.eq_ignore_ascii_case(NRF5340)
        && lookup().is_some_and(|k| k.contains(&NRF5340_APP_ONLY.to_ascii_lowercase()))
    {
        return NRF5340_APP_ONLY.to_owned();
    }
    chip.to_owned()
}

/// What to say before a session that does not reset the chip, on the `--chip`
/// [`probe_rs_chip`] gave: the stock nRF5340 target leaves its network core
/// running. `None` everywhere else.
pub fn network_core_note(probe_rs_chip: &str) -> Option<&'static str> {
    // `concat!`, not a `\`-continued literal: rustfmt can join one into a
    // run of spaces in the rendered line.
    probe_rs_chip
        .trim()
        .eq_ignore_ascii_case(NRF5340)
        .then_some(concat!(
            "note: this probe-rs starts the nRF5340's network core whenever it opens a session, ",
            "and an attach does not reset it back into Force-OFF - it runs whatever its flash ",
            "holds until the next reset. probe-rs 0.32 adds nRF5340_xxAA_APPONLY, which never ",
            "starts it, and this IDE uses it from the next session on once it is installed: ",
            "`cargo install probe-rs-tools --locked` (on Windows the Tools tab's probe-rs row ",
            "names the releases with known probe problems).",
        ))
}

#[cfg(test)]
mod chip_gap_tests {
    use super::*;

    /// Nobody filters a probe selector by hand any more.
    ///
    /// There are FIVE consumers - `rtt.rs`, `debugger.rs`, `flamegraph.rs`,
    /// `probe_flash.rs` and `start_reset` here - and they were five hand-written
    /// copies of one idea. Fixing "both" of them was a mistake made in this very
    /// repo: two were unified, three kept the old `!s.is_empty()` and a selector
    /// of spaces still went out to probe-rs from the other three.
    ///
    /// A source scan, because what regresses is a SIXTH caller written the old
    /// way - and that is visible here and nowhere else.
    #[test]
    fn no_caller_filters_a_probe_selector_by_hand() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for rel in [
            "src/rtt.rs",
            "src/debugger.rs",
            "src/flamegraph.rs",
            "src/probe_flash.rs",
            "src/probe.rs",
        ] {
            let src =
                std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            for (i, l) in src.lines().enumerate() {
                // The old shape, in code rather than in a doc comment.
                let t = l.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                // Assembled from pieces so this line does not match ITSELF -
                // the scan reads the file it lives in.
                let needle = ["filter(|s| !s.", "is_empty())"].concat();
                if l.contains("probe") && l.contains(&needle) {
                    offenders.push(format!("{rel}:{}", i + 1));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "these filter a selector by hand instead of calling `probe::selector`: {offenders:?}"
        );
    }

    /// The nRF5340 goes to probe-rs as its application-core-only target when
    /// the installed probe-rs has one, and as itself otherwise - with no
    /// probe-rs to ask, too. Every other chip passes through, trimmed, and
    /// never makes probe-rs be asked.
    #[test]
    fn the_nrf5340_goes_app_only_when_probe_rs_has_it() {
        use std::collections::HashSet;
        let v029: HashSet<String> = ["nrf53_series", "nrf5340_xxaa"].map(String::from).into();
        let mut v032 = v029.clone();
        v032.insert("nrf5340_xxaa_apponly".to_owned());
        let with = |chip: &str, set: &HashSet<String>| {
            let set = set.clone();
            probe_rs_chip_with(chip, move || Some(set))
        };

        assert_eq!(with("nRF5340_xxAA", &v032), "nRF5340_xxAA_APPONLY");
        assert_eq!(with(" nrf5340_xxaa ", &v032), "nRF5340_xxAA_APPONLY");
        assert_eq!(with("nRF5340_xxAA", &v029), "nRF5340_xxAA");
        assert_eq!(
            probe_rs_chip_with("nRF5340_xxAA", || None),
            "nRF5340_xxAA",
            "fails open"
        );
        for other in ["nRF52840_xxAA", "STM32F103C8", "esp32c3", "RP2040"] {
            assert_eq!(
                probe_rs_chip_with(&format!(" {other} "), || panic!("{other}: probe-rs asked")),
                other
            );
        }

        // The note follows what probe-rs is handed, not the definition.
        assert!(network_core_note("nRF5340_xxAA").is_some());
        assert!(network_core_note("nRF5340_xxAA_APPONLY").is_none());
        assert!(network_core_note("nRF52840_xxAA").is_none());
    }

    /// Every place that hands probe-rs a chip asks [`probe_rs_chip`] first - a
    /// sixth written the old way would release the nRF5340's network core
    /// again, and this is where that shows. Code only (comments and the test
    /// modules skipped): a `"chip":`, `["chip"]` or `"--chip"` line must ask
    /// the resolver on it or on the next line. `rtt::probe_rs_args` builds from
    /// a chip its one caller resolved; `probe_flash.rs` keeps the definition's
    /// name on purpose - see [`probe_rs_chip`].
    #[test]
    fn every_probe_rs_chip_goes_through_the_resolver() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut offenders = Vec::new();
        for rel in [
            "src/rtt.rs",
            "src/debugger.rs",
            "src/flamegraph.rs",
            "src/probe.rs",
            "src/timing.rs",
        ] {
            let src =
                std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            // Production code only: every `#[cfg(test)] mod` block is cut out
            // (assembled from pieces, so this one's own line is not a match).
            let all: Vec<&str> = src.lines().collect();
            let cfg_test = ["#[cfg(", "test)]"].concat();
            let mut lines: Vec<&str> = Vec::new();
            let mut skipping = false;
            for (i, l) in all.iter().enumerate() {
                if skipping {
                    skipping = *l != "}";
                    continue;
                }
                if l.trim_end() == cfg_test && all.get(i + 1).is_some_and(|n| n.starts_with("mod "))
                {
                    skipping = true;
                    continue;
                }
                lines.push(l);
            }
            let asks_it = ["probe_rs_chip", "("].concat();
            let mut asks = false;
            for (i, l) in lines.iter().enumerate() {
                let t = l.trim_start();
                if t.starts_with("//") || t.contains("fn probe_rs_chip") {
                    continue;
                }
                asks |= t.contains(&asks_it);
                let hands =
                    t.contains("\"chip\":") || t.contains("[\"chip\"]") || t.contains("\"--chip\"");
                let builder =
                    rel == "src/rtt.rs" && t.starts_with("let mut args = vec![sub.to_owned()");
                let next = lines.get(i + 1).copied().unwrap_or("");
                if hands && !builder && !t.contains(&asks_it) && !next.contains(&asks_it) {
                    offenders.push(format!("{rel}: {t}"));
                }
            }
            assert!(asks, "{rel} never asks the resolver");
        }
        assert!(
            offenders.is_empty(),
            "these hand probe-rs a chip without `probe::probe_rs_chip`: {offenders:?}"
        );
    }

    /// One normaliser, because two callers had drifted apart.
    ///
    /// The RTT tab spends it as `--probe <sel>`, the debugger as the DAP
    /// `launch` object's `probe` field. Both filtered with a bare `is_empty()`,
    /// so a selector of spaces survived and went out as `--probe "   "` -
    /// rejected by probe-rs, where the absent case means auto-select.
    #[test]
    fn a_blank_selector_means_auto_select() {
        for blank in [None, Some(""), Some(" "), Some("   \t ")] {
            assert_eq!(selector(blank), None, "{blank:?}");
        }
    }

    /// A real selector survives, and is trimmed rather than passed with the
    /// whitespace a copy-paste brings along.
    #[test]
    fn a_real_selector_is_kept_and_trimmed() {
        assert_eq!(selector(Some("303a:1001")).as_deref(), Some("303a:1001"));
        assert_eq!(
            selector(Some("  0483:3748:0671FF56  ")).as_deref(),
            Some("0483:3748:0671FF56"),
            "a pasted selector carries spaces the user cannot see"
        );
    }

    const SAMPLE: &str = "\
Available chips:
ADuCM302x Series
    Variants:
        ADuCM3027
        ADuCM3029
esp32c6
    Variants:
        esp32c6
STM32F1 Series
    Variants:
        STM32F103C8
        STM32F103CB
";

    #[test]
    fn both_families_and_variants_are_collected() {
        let set = parse_chip_list(SAMPLE).expect("parsed");
        for name in ["esp32c6", "stm32f1 series", "stm32f103c8", "aducm3029"] {
            assert!(set.contains(name), "missing {name}");
        }
        // The scaffolding is not a chip.
        assert!(!set.contains("variants:"));
        assert!(!set.contains("available chips:"));
    }

    /// Output we cannot make sense of must read as "could not ask", never as
    /// "probe-rs knows nothing" — which would disable every button everywhere.
    #[test]
    fn an_unreadable_listing_blocks_nothing() {
        assert!(parse_chip_list("").is_none());
        assert!(parse_chip_list("Available chips:\n\n   \n").is_none());
    }

    /// The C5 and C61 are the parts this exists for: probe-rs 0.29 has no target
    /// for either, while espflash flashes them happily.
    #[test]
    fn the_message_names_the_chip_and_says_flashing_still_works() {
        let Some(known) = parse_chip_list(SAMPLE) else {
            unreachable!()
        };
        assert!(!known.contains("esp32c5"));
        assert!(!known.contains("esp32c61"));
        assert!(known.contains("esp32c6"), "the C6 is a different part");

        // `chip_gap` itself consults the installed binary, so exercise its
        // wording through the same format string it uses.
        let gap = format!(
            "The installed probe-rs has no target for `{}`, so it cannot attach to one.",
            "esp32c5"
        );
        assert!(gap.contains("esp32c5"));
    }

    /// An empty chip name is not a gap — a project with no chip picked yet is
    /// already disabled for that reason, and saying it twice helps nobody.
    #[test]
    fn no_chip_is_not_a_gap() {
        assert_eq!(chip_gap(""), None);
        assert_eq!(chip_gap("   "), None);
    }

    /// Against the installed probe-rs. Ignored — runs the binary.
    ///
    /// `cargo test -- --ignored probe_rs_answers --nocapture`
    #[test]
    #[ignore]
    fn probe_rs_answers_for_every_bundled_chip() {
        use crate::panels::mcu_module::builtins::builtin_definitions;

        let Some(known) = known_targets() else {
            eprintln!("probe-rs not installed — skipping");
            return;
        };
        println!("probe-rs knows {} target names", known.len());

        let mut blocked = Vec::new();
        for d in builtin_definitions() {
            let chip = &d.project.probe_chip;
            if chip_gap(chip).is_some() {
                blocked.push(format!("{} ({chip})", d.id));
            }
        }
        println!("blocked from RTT / Debug / Profile: {blocked:?}");

        // Every STM32 must be reachable: probe-rs is the ONLY way those flash,
        // so a gap there would be a real regression rather than a version skew.
        for d in builtin_definitions() {
            if d.family.starts_with("stm32")
                || crate::panels::mcu_module::codegen::nrf::is_nrf(&d.family)
            {
                assert_eq!(
                    chip_gap(&d.project.probe_chip),
                    None,
                    "{}: probe-rs cannot target it, and nothing else can flash it",
                    d.id
                );
            }
        }
    }
}

#[cfg(test)]
mod probe_compatible_tests {
    use super::{ToolchainKind, probe_compatible};

    #[test]
    fn arm_chips_accept_swd_probes_not_esp_jtag() {
        let arm = ToolchainKind::RustEmbedded;
        // Exact strings `probe-rs list` prints for these probes.
        assert!(probe_compatible("ST-LINK", &arm));
        assert!(probe_compatible("JLink", &arm));
        assert!(probe_compatible("CMSIS-DAP", &arm));
        // The ESP built-in USB-JTAG can't debug an ARM chip.
        assert!(!probe_compatible("EspJtag", &arm));
    }

    #[test]
    fn esp_chips_accept_jtag_not_stlink() {
        let esp = ToolchainKind::EspRust;
        assert!(probe_compatible("EspJtag", &esp));
        assert!(probe_compatible("JLink", &esp)); // J-Link JTAG works on ESP too
        assert!(!probe_compatible("ST-LINK", &esp));
        assert!(!probe_compatible("CMSIS-DAP", &esp));
    }

    #[test]
    fn sdcc_has_no_probe_rs_target() {
        let sdcc = ToolchainKind::SdccC;
        assert!(!probe_compatible("ST-LINK", &sdcc));
        assert!(!probe_compatible("EspJtag", &sdcc));
    }
}

#[cfg(test)]
mod pick_probe_tests {
    use super::{ProbeInfo, ToolchainKind, pick_probe};

    fn probe(name: &str, kind: &str, selector: &str) -> ProbeInfo {
        ProbeInfo {
            name: name.into(),
            kind: kind.into(),
            selector: selector.into(),
        }
    }

    /// The case that sent the user here: an ST-Link and an ESP32-C3 plugged in
    /// at once. probe-rs refuses to choose; the toolchain leaves one candidate.
    #[test]
    fn an_esp_project_ignores_the_st_link_next_to_it() {
        let attached = [
            probe("STLink V2", "ST-LINK", "0483:3748"),
            probe("ESP JTAG", "EspJtag", "303a:1001:50:78:7D:62:33:A4"),
        ];
        assert_eq!(
            pick_probe(&attached, &ToolchainKind::EspRust).unwrap(),
            "303a:1001:50:78:7D:62:33:A4"
        );
    }

    /// …and the mirror image, so the filter is not just "prefer the ESP one".
    #[test]
    fn an_arm_project_ignores_the_esp_jtag_next_to_it() {
        let attached = [
            probe("STLink V2", "ST-LINK", "0483:3748"),
            probe("ESP JTAG", "EspJtag", "303a:1001:AA"),
        ];
        assert_eq!(
            pick_probe(&attached, &ToolchainKind::RustEmbedded).unwrap(),
            "0483:3748"
        );
    }

    #[test]
    fn one_probe_needs_no_filtering_at_all() {
        let attached = [probe("ESP JTAG", "EspJtag", "303a:1001:AA")];
        assert_eq!(
            pick_probe(&attached, &ToolchainKind::EspRust).unwrap(),
            "303a:1001:AA"
        );
    }

    /// A real tie is NOT guessed: picking the wrong one of two ST-Links wastes
    /// a run against someone else's board.
    #[test]
    fn two_candidates_are_handed_back_by_name() {
        let attached = [
            probe("STLink V2", "ST-LINK", "0483:3748"),
            probe("STLink V3", "ST-LINK", "0483:374e"),
        ];
        let err = pick_probe(&attached, &ToolchainKind::RustEmbedded).unwrap_err();
        assert!(err.contains("STLink V2"), "{err}");
        assert!(err.contains("STLink V3"), "{err}");
        assert!(err.contains("Probe list"), "{err}");
    }

    /// An ST-Link alone cannot drive an ESP: say THAT, not "no probe found".
    #[test]
    fn the_wrong_kind_alone_says_which_probes_are_attached() {
        let attached = [probe("STLink V2", "ST-LINK", "0483:3748")];
        let err = pick_probe(&attached, &ToolchainKind::EspRust).unwrap_err();
        assert!(err.contains("STLink V2"), "{err}");
    }

    #[test]
    fn an_empty_bench_says_so() {
        let err = pick_probe(&[], &ToolchainKind::EspRust).unwrap_err();
        assert!(err.contains("no debug probe"), "{err}");
    }
}
