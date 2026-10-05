//! The `mcu.config` file — out-of-source persistence of the MCU's virtual
//! modules (`@modules`) and clock-tree config (`@clock`). Written to the project
//! root (next to Cargo.toml), created automatically, and NOT shown in the
//! project tree. Replaces the old one-line `// @modules` / `// @clock` comment
//! markers that lived inside `main.rs`.
//!
//! Format — each section header on its own line, the body multi-line:
//! ```text
//! @modules
//! [
//!     (id: "i2c_1", kind: GenericInterfaceI2c, …),
//! ]
//!
//! @clock
//! hse=8000000
//! hse_on=0
//! …
//! ```

use super::clock::model::Stm32f1Clock;
use super::clock::persist as clock_persist;
use super::mcu::{AutoBuild, Runtime};
use super::modules::{ApiStyle, VirtualModule};
use crate::panels::mcu_module::pins::logic::pin::{Edge, GpioMode, TaskPriority};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

const MODULES_HEADER: &str = "@modules";
const CLOCK_HEADER: &str = "@clock";
const RUNTIME_HEADER: &str = "@runtime";
const GPIO_HEADER: &str = "@gpio";
const AUTOBUILD_HEADER: &str = "@autobuild";
const STRICT_HEADER: &str = "@strict";
const DEBUGBUILD_HEADER: &str = "@debugbuild";
const ROTATION_HEADER: &str = "@rotation";
const CLOCK_MANUAL_HEADER: &str = "@clockmanual";
const CLOCK_NODES_HEADER: &str = "@clocknodes";
const IOPINS_HEADER: &str = "@iopins";
const IRQ_HEADER: &str = "@irq";
const IOMODE_HEADER: &str = "@iomode";
const GROUPS_HEADER: &str = "@groups";
const GROUP_I2C_HEADER: &str = "@groupi2c";
const I2C_POS_HEADER: &str = "@i2cpos";
const WATCHDOG_HEADER: &str = "@watchdog";
const COMP_HEADER: &str = "@comp";
const FLASHSTORE_HEADER: &str = "@flashstore";
const LABELS_HEADER: &str = "@labels";
const PINS_HEADER: &str = "@pins";
const NOTES_HEADER: &str = "@modulenotes";

/// The `@autobuild` section text (or "" for the default `Check`) — appended by
/// `Mcu::mcu_config_text` after [`serialize`]. Kept separate so `serialize`'s
/// signature (and its many test call-sites) stays put; it's a workflow setting,
/// not part of the module/clock/runtime config.
pub fn autobuild_section(auto_build: AutoBuild) -> String {
    if auto_build == AutoBuild::Check {
        String::new()
    } else {
        format!("{AUTOBUILD_HEADER}\n{}\n", auto_build.as_token())
    }
}

/// The auto-build preference recorded in `@autobuild`; a missing section is the
/// default `Check`.
pub fn parse_autobuild(text: &str) -> AutoBuild {
    section_body(text, AUTOBUILD_HEADER)
        .map(|b| AutoBuild::from_token(&b))
        .unwrap_or_default()
}

/// The `@strict` section text (or "" for the default OFF) — the strict-lints
/// Clippy preference. Appended like `@autobuild`.
pub fn strict_section(strict: bool) -> String {
    if strict {
        format!("{STRICT_HEADER}\non\n")
    } else {
        String::new()
    }
}

/// The `@watchdog` section — the Configuration tab's IWDG/WWDG settings.
///
/// Durations in microseconds, one line per watchdog, absent when not enabled:
///
/// ```text
/// @watchdog
/// iwdg 32768000
/// wwdg 41472 0
/// ```
///
/// These are CODEGEN input, not a view preference: they decide whether
/// `pins/configs/{iwdg,wwdg}.rs` exist at all, so they travel with the project.
pub fn watchdog_section(w: &crate::panels::mcu_module::watchdog::WatchdogSettings) -> String {
    let mut body = String::new();
    if let Some(i) = w.iwdg {
        body.push_str(&format!(
            "iwdg {}
",
            i.timeout_us
        ));
    }
    if let Some(x) = w.wwdg {
        body.push_str(&format!(
            "wwdg {} {}
",
            x.timeout_us, x.window_us
        ));
    }
    // The ESP three. Written on their own keys rather than reusing `iwdg`,
    // because a project carried from an STM32 to an ESP keeps both sets and
    // neither should be read as the other: 26 seconds of IWDG is not 26
    // seconds of RWDT, and the tab shows only the pair its family uses.
    for (key, cfg) in [("rwdt", w.rwdt), ("mwdt0", w.mwdt0), ("mwdt1", w.mwdt1)] {
        if let Some(c) = cfg {
            body.push_str(&format!(
                "{key} {}
",
                c.timeout_us
            ));
        }
    }
    // The RP one on its own key too, for the same reason: its range is the
    // DRIVER's, and a period carried in from another family must not arrive
    // in a Pico's firmware unseen.
    if let Some(c) = w.rp {
        body.push_str(&format!(
            "rp_watchdog {}
",
            c.timeout_us
        ));
    }
    if let Some(c) = w.nrf {
        body.push_str(&format!(
            "nrf_watchdog {}
",
            c.timeout_us
        ));
    }
    if body.is_empty() {
        String::new()
    } else {
        format!(
            "{WATCHDOG_HEADER}
{body}"
        )
    }
}

/// Read `@watchdog` back. A malformed or partial line is DROPPED rather than
/// defaulted: a watchdog the user cannot see in the tab must not end up in the
/// generated firmware, and silently substituting a number would do exactly that.
pub fn parse_watchdog(text: &str) -> crate::panels::mcu_module::watchdog::WatchdogSettings {
    use crate::panels::mcu_module::watchdog::{
        EspWdtConfig, IwdgConfig, NrfWdtConfig, RpWdtConfig, WatchdogSettings, WwdgConfig,
    };
    let mut out = WatchdogSettings::default();
    let Some(body) = section_body(text, WATCHDOG_HEADER) else {
        return out;
    };
    for line in body.lines() {
        let mut it = line.split_whitespace();
        match (it.next(), it.next().and_then(|v| v.parse().ok())) {
            (Some("iwdg"), Some(timeout_us)) => out.iwdg = Some(IwdgConfig { timeout_us }),
            (Some("rp_watchdog"), Some(timeout_us)) => out.rp = Some(RpWdtConfig { timeout_us }),
            (Some("nrf_watchdog"), Some(timeout_us)) => out.nrf = Some(NrfWdtConfig { timeout_us }),
            (Some("rwdt"), Some(timeout_us)) => out.rwdt = Some(EspWdtConfig { timeout_us }),
            (Some("mwdt0"), Some(timeout_us)) => out.mwdt0 = Some(EspWdtConfig { timeout_us }),
            (Some("mwdt1"), Some(timeout_us)) => out.mwdt1 = Some(EspWdtConfig { timeout_us }),
            (Some("wwdg"), Some(timeout_us)) => {
                // The window is required: without it the pair is meaningless,
                // and defaulting it to 0 would quietly change the behaviour the
                // user configured.
                if let Some(window_us) = it.next().and_then(|v| v.parse().ok()) {
                    out.wwdg = Some(WwdgConfig {
                        timeout_us,
                        window_us,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// The `@comp` section — one line per ENABLED comparator:
///
/// ```text
/// @comp
/// 3 HighSpeed Mv20 NotInverted HalfVref None
/// ```
///
/// Positional rather than `key=value`: five fields that always appear in the
/// same order, and a line that lost one is dropped whole (below) rather than
/// half-applied.
pub fn comp_section(c: &crate::panels::mcu_module::comparator::CompSettings) -> String {
    let mut body = String::new();
    for (n, cfg) in c {
        body.push_str(&format!(
            "{n} {} {} {} {} {}\n",
            cfg.power_mode.token(),
            cfg.hysteresis.token(),
            cfg.output_polarity.token(),
            cfg.inverting_input.token(),
            cfg.blanking_source.token(),
        ));
    }
    if body.is_empty() {
        String::new()
    } else {
        format!("{COMP_HEADER}\n{body}")
    }
}

/// Read `@comp` back. Same policy as `@watchdog`: a line that does not parse
/// COMPLETELY is dropped, because a comparator the user cannot see in the tab
/// must not reach the generated firmware, and defaulting a field would quietly
/// change what it compares against.
pub fn parse_comp(text: &str) -> crate::panels::mcu_module::comparator::CompSettings {
    use crate::panels::mcu_module::comparator::{
        BlankingSource, CompConfig, CompSettings, Hysteresis, InvertingInput, OutputPolarity,
        PowerMode,
    };
    let mut out = CompSettings::new();
    let Some(body) = section_body(text, COMP_HEADER) else {
        return out;
    };
    // The token spellings are the generator's own, so a round trip cannot drift
    // from what the templates emit.
    let by_token = |tok: &str, all: &[&str]| all.iter().position(|t| *t == tok);
    for line in body.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() != 6 {
            continue;
        }
        let Ok(n) = f[0].parse::<u8>() else { continue };
        let power = PowerMode::ALL.iter().find(|v| v.token() == f[1]).copied();
        let hyst = Hysteresis::ALL.iter().find(|v| v.token() == f[2]).copied();
        let pol = OutputPolarity::ALL
            .iter()
            .find(|v| v.token() == f[3])
            .copied();
        let inm = InvertingInput::ALL
            .iter()
            .find(|v| v.token() == f[4])
            .copied();
        let blank = BlankingSource::ALL
            .iter()
            .find(|v| v.token() == f[5])
            .copied();
        let _ = by_token;
        if let (
            Some(power_mode),
            Some(hysteresis),
            Some(output_polarity),
            Some(inverting_input),
            Some(blanking_source),
        ) = (power, hyst, pol, inm, blank)
        {
            out.insert(
                n,
                CompConfig {
                    power_mode,
                    hysteresis,
                    output_polarity,
                    inverting_input,
                    blanking_source,
                },
            );
        }
    }
    out
}

/// The `@flashstore` section - the Configuration tab's flash store, absent when
/// it is off. One line: the mode, then the flash size, the store's size and its
/// offset, in hex:
///
/// ```text
/// @flashstore
/// partition 0x400000 0x4000 0x3FC000
/// ```
///
/// All four are written in either mode, so switching to `nvs` and back gives
/// the partition its old place.
pub fn flashstore_section(
    cfg: Option<&crate::panels::mcu_module::flash_store::FlashStoreConfig>,
) -> String {
    match cfg {
        None => String::new(),
        Some(c) => format!(
            "{FLASHSTORE_HEADER}\n{} 0x{:X} 0x{:X} 0x{:X}\n",
            c.mode.token(),
            c.flash_size,
            c.size,
            c.offset
        ),
    }
}

/// Read `@flashstore` back; `None` when it is absent OR malformed. A store the
/// tab cannot show must not reach the firmware, and guessing a field would
/// move it - the same rule as `@watchdog`.
pub fn parse_flashstore(
    text: &str,
) -> Option<crate::panels::mcu_module::flash_store::FlashStoreConfig> {
    use crate::panels::mcu_module::flash_store::{FlashStoreConfig, FlashStoreMode};
    let body = section_body(text, FLASHSTORE_HEADER)?;
    let f: Vec<&str> = body.split_whitespace().collect();
    let [mode, flash_size, size, offset] = f.as_slice() else {
        return None;
    };
    let hex = |s: &str| u32::from_str_radix(s.strip_prefix("0x")?, 16).ok();
    Some(FlashStoreConfig {
        mode: FlashStoreMode::from_token(mode)?,
        flash_size: hex(flash_size)?,
        size: hex(size)?,
        offset: hex(offset)?,
    })
}

/// The strict-lints preference recorded in `@strict`; missing / anything but
/// `on` is OFF (the default).
pub fn parse_strict(text: &str) -> bool {
    section_body(text, STRICT_HEADER).as_deref() == Some("on")
}

/// The `@debugbuild` section text (or "" for the default OFF) — the Debug tab's
/// "Debug-friendly build" toggle, which relaxes `[profile.release]` so every
/// source line can hold a breakpoint. Appended like `@autobuild`.
pub fn debug_build_section(debug_build: bool) -> String {
    if debug_build {
        format!("{DEBUGBUILD_HEADER}\non\n")
    } else {
        String::new()
    }
}

/// The debug-build preference recorded in `@debugbuild`; missing / anything but
/// `on` is OFF (the optimised profile that gets flashed).
pub fn parse_debug_build(text: &str) -> bool {
    section_body(text, DEBUGBUILD_HEADER).as_deref() == Some("on")
}

/// The `@rotation` section text (or "" for the default un-rotated) — the diagram
/// rotation toggle. Appended like `@autobuild`.
pub fn rotation_section(rotated: bool) -> String {
    if rotated {
        format!("{ROTATION_HEADER}\non\n")
    } else {
        String::new()
    }
}

/// The `@clockmanual` section text (or "" when the clock is generated) — the
/// hand-written-clock switch. In `mcu.config` rather than the view-state file
/// because it CHANGES THE GENERATED CODE, so it belongs in Git with the rest of
/// the project's configuration. Appended like `@autobuild`.
/// The `@clocknodes` section — the project's clock edits for ANY family, as
/// `node=state` tokens.
///
/// Separate from `@clock`, which speaks `Stm32f1Clock` and is written only for
/// STM32F1. That section stays exactly as it was so existing projects keep
/// loading; this one carries what it never could — the state of a tree with a
/// shape the F1 struct has no field for.
///
/// Empty body writes no section, so an untouched clock leaves the file alone.
pub fn clock_nodes_section(body: &str) -> String {
    if body.trim().is_empty() {
        String::new()
    } else {
        format!(
            "{CLOCK_NODES_HEADER}
{}
",
            body.trim_end()
        )
    }
}

/// The raw `@clocknodes` body, for
/// [`clock::persist::nodes_from_block`](crate::panels::mcu_module::clock::persist::nodes_from_block)
/// to parse. This module carries the section; the format belongs to the clock.
pub fn parse_clock_nodes(text: &str) -> Option<String> {
    section_body(text, CLOCK_NODES_HEADER).filter(|b| !b.trim().is_empty())
}

pub fn clock_manual_section(manual: bool) -> String {
    if manual {
        format!(
            "{CLOCK_MANUAL_HEADER}
on
"
        )
    } else {
        String::new()
    }
}

/// The hand-written-clock preference recorded in `@clockmanual`.
///
/// A MISSING section is not simply "off": a chip whose family has no RCC recipe
/// defaults to manual, and that default is applied by the caller — this only
/// reports what the file says.
pub fn parse_clock_manual(text: &str) -> Option<bool> {
    section_body(text, CLOCK_MANUAL_HEADER).map(|b| b == "on")
}

/// The diagram-rotation preference recorded in `@rotation`; missing / anything
/// but `on` is un-rotated (the default).
pub fn parse_rotation(text: &str) -> bool {
    section_body(text, ROTATION_HEADER).as_deref() == Some("on")
}

/// The `@iopins` section — manual in/out field positions, one `num=x,y` per
/// line — or "" when none are placed.
pub fn iopins_section(pos: &std::collections::BTreeMap<usize, (f32, f32)>) -> String {
    if pos.is_empty() {
        return String::new();
    }
    let mut s = String::from(IOPINS_HEADER);
    s.push('\n');
    for (num, (x, y)) in pos {
        s.push_str(&format!("{num}={x},{y}\n"));
    }
    s
}

/// The `@i2cpos` section - one `i2c<instance>/<uid>=x,y` per I2C device box the
/// user dragged out of its bus's column - or "" when none was.
pub fn i2c_pos_section(pos: &std::collections::BTreeMap<(u8, u32), (f32, f32)>) -> String {
    if pos.is_empty() {
        return String::new();
    }
    let mut s = String::from(I2C_POS_HEADER);
    s.push('\n');
    for ((inst, uid), (x, y)) in pos {
        s.push_str(&format!("{}={x},{y}\n", i2c_token(*inst, *uid)));
    }
    s
}

/// Read `@i2cpos` back; a line that does not parse is dropped alone.
pub fn parse_i2c_pos(text: &str) -> std::collections::BTreeMap<(u8, u32), (f32, f32)> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, I2C_POS_HEADER) else {
        return map;
    };
    for line in body.lines() {
        let Some((dev, xy)) = line.trim().split_once('=') else {
            continue;
        };
        let (Some(dev), Some((xs, ys))) = (parse_i2c_token(dev), xy.split_once(',')) else {
            continue;
        };
        if let (Ok(x), Ok(y)) = (xs.trim().parse::<f32>(), ys.trim().parse::<f32>()) {
            map.insert(dev, (x, y));
        }
    }
    map
}

/// One device on the board: a name, and the pads that belong to it.
///
/// Keyed by PIN NUMBER, like `@iopins`, `@irq` and `@iomode` - and that is the
/// whole design decision. A group could have named the modules it contains
/// instead, but a module has no stable identity: `reconcile_modules` deletes a
/// peripheral module whose pads were re-purposed and re-wiring mints a NEW id,
/// so a list of ids loses its members on an ordinary gesture. A pin number
/// comes from the chip definition and never moves.
///
/// A MODULE is in the group when any of its pads is - derived, never stored, so
/// it survives that delete-and-recreate for free.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PinGroup {
    pub name: String,
    pub pins: std::collections::BTreeSet<usize>,
    /// Devices on an I2C bus put in this group by hand: `(bus instance, device
    /// uid)`. They have no pad of their own - every device of a bus shares its
    /// SCL and SDA, and a pad is in one group at most - so they cannot be
    /// members the way a pad is.
    ///
    /// A device NOT listed in any group is in none - not in its bus's: each
    /// device of a bus is a board part of its own. Keyed by the uid, never by
    /// position, name or address (see
    /// `I2cDevice::uid`). A key whose device is gone stays - the way a pad
    /// keeps its group when its function goes - so an undo brings the device
    /// back into it; new uids are minted above every key a group still holds
    /// (`Mcu::i2c_uid_floor`), so no new device inherits one.
    pub i2c: std::collections::BTreeSet<(u8, u32)>,
}

impl PinGroup {
    /// Whether this is a device yet.
    ///
    /// ONE predicate, shared by persistence, the generated comment and the
    /// canvas mats. They used to disagree: `@groups` required a name and the
    /// other two did not, so a device whose name the user had cleared was
    /// painted on the canvas and written into main.rs as a nameless
    /// `// : PA4, PA5` - and then vanished on the next save, because
    /// persistence alone refused to write it.
    ///
    /// A group the roster is still filling in is not yet a device; it stays on
    /// the roster and nowhere else.
    pub fn is_live(&self) -> bool {
        !self.name.trim().is_empty() && !self.is_empty()
    }

    /// Holds nothing at all - no pad and no I2C device. What decides that a
    /// group a gesture just took its last member from is finished.
    pub fn is_empty(&self) -> bool {
        self.pins.is_empty() && self.i2c.is_empty()
    }
}

/// An I2C device's token in `@groupi2c`: `i2c<instance>/<uid>`. No `,` or `=`
/// in it, it never starts with `@`, and it never parses as a pad number.
fn i2c_token(instance: u8, uid: u32) -> String {
    format!("i2c{instance}/{uid}")
}

fn parse_i2c_token(t: &str) -> Option<(u8, u32)> {
    let (inst, uid) = t.trim().strip_prefix("i2c")?.split_once('/')?;
    let uid: u32 = uid.parse().ok()?;
    (uid != 0).then_some((inst.parse().ok()?, uid))
}

/// The `@groups` section - one `pin,pin,pin=name` per device - or "" when
/// nothing is grouped, so a project that groups nothing round-trips without it.
///
/// PINS FIRST, name last, which is the opposite of every other section here and
/// is load-bearing twice over. The name is free text typed into a panel field:
///
/// * written last and split on the FIRST `=`, it may CONTAIN an `=`
///   ("PA0=reset"). Written first it would take the pin list with it.
/// * written after the digits, the line can never START with `@` - and
///   [`section_body`] ends a section at the first line that does. A device
///   named "@radar" on the other layout would truncate the section and take
///   every group after it with it.
///
/// A group's I2C devices go in a section of their own, `@groupi2c` - one
/// `i2c1/3,i2c1/4=name` per group that holds any. Mixed into the `@groups`
/// line, a token an older build cannot read as a pad number would make it drop
/// the WHOLE line, pads included; on its own line an older build drops only
/// what it cannot know about. A group holding only I2C devices still writes a
/// `=name` line in `@groups`, so its place in the order - which is the order of
/// the generated comment and of the roster - survives a save; older builds drop
/// that line, as they would the group anyway.
pub fn groups_section(groups: &[PinGroup]) -> String {
    let live: Vec<&PinGroup> = groups.iter().filter(|g| g.is_live()).collect();
    if live.is_empty() {
        return String::new();
    }
    let mut s = String::from(GROUPS_HEADER);
    s.push('\n');
    for g in &live {
        let pins: Vec<String> = g.pins.iter().map(usize::to_string).collect();
        s.push_str(&format!("{}={}\n", pins.join(","), g.name.trim()));
    }
    if live.iter().any(|g| !g.i2c.is_empty()) {
        s.push_str(GROUP_I2C_HEADER);
        s.push('\n');
        for g in live.iter().filter(|g| !g.i2c.is_empty()) {
            let devs: Vec<String> = g.i2c.iter().map(|(i, u)| i2c_token(*i, *u)).collect();
            s.push_str(&format!("{}={}\n", devs.join(","), g.name.trim()));
        }
    }
    s
}

/// Read `@groups` and `@groupi2c` back - the one reader, which the Board's
/// rebuild of a closed chip uses too. A line whose pins do not parse is dropped
/// rather than guessed at - a half-read group would claim pads it was never
/// given. An I2C device claimed by two groups (a hand-edited file) stays in the
/// first.
pub fn parse_groups(text: &str) -> Vec<PinGroup> {
    let mut out: Vec<PinGroup> = Vec::new();
    // The I2C devices first: they decide whether a `=name` line is a group.
    let mut i2c: Vec<(String, std::collections::BTreeSet<(u8, u32)>)> = Vec::new();
    let mut claimed = std::collections::BTreeSet::new();
    if let Some(body) = section_body(text, GROUP_I2C_HEADER) {
        for line in body.lines() {
            let Some((rest, name)) = line.split_once('=') else {
                continue;
            };
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let devs: Option<std::collections::BTreeSet<(u8, u32)>> = rest
                .split(',')
                .filter(|t| !t.trim().is_empty())
                .map(parse_i2c_token)
                .collect();
            let Some(devs) = devs else { continue };
            let devs: std::collections::BTreeSet<(u8, u32)> =
                devs.into_iter().filter(|d| claimed.insert(*d)).collect();
            if !devs.is_empty() {
                i2c.push((name.to_owned(), devs));
            }
        }
    }
    if let Some(body) = section_body(text, GROUPS_HEADER) {
        for line in body.lines() {
            let Some((rest, name)) = line.split_once('=') else {
                continue;
            };
            // Only the padding a panel field allows is trimmed off the name; its
            // interior is whatever the user typed.
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            let pins: Option<std::collections::BTreeSet<usize>> = rest
                .split(',')
                .filter(|p| !p.trim().is_empty())
                .map(|p| p.trim().parse::<usize>().ok())
                .collect();
            let holds_i2c = i2c.iter().any(|(n, _)| n == name);
            match pins {
                Some(pins) if !pins.is_empty() || holds_i2c => out.push(PinGroup {
                    name: name.to_owned(),
                    pins,
                    ..Default::default()
                }),
                _ => continue,
            }
        }
    }
    for (name, devs) in i2c {
        match out.iter_mut().find(|g| g.name.trim() == name) {
            Some(g) => g.i2c.extend(devs),
            // A file whose `@groups` has no line for it: still a group.
            None => out.push(PinGroup {
                name,
                i2c: devs,
                ..Default::default()
            }),
        }
    }
    out
}

/// The `@irq` section — one `num=Edge` per interrupt-enabled input pin — or ""
/// when none are armed, so a project that uses no interrupts round-trips without
/// the section at all.
pub fn irq_section(irqs: &std::collections::BTreeMap<usize, (Edge, TaskPriority)>) -> String {
    if irqs.is_empty() {
        return String::new();
    }
    let mut s = String::from(IRQ_HEADER);
    s.push('\n');
    for (num, (e, prio)) in irqs {
        // Written only when it is NOT the default, so every project saved
        // before task priorities existed round-trips byte for byte, and an
        // unprioritised pin keeps the shorter line.
        if *prio == TaskPriority::default() {
            s.push_str(&format!("{num}={}\n", e.as_token()));
        } else {
            s.push_str(&format!("{num}={},{}\n", e.as_token(), prio.as_token()));
        }
    }
    s
}

/// The `@iomode` section — one `num=Mode` per GPIO pin whose drive/pull mode the
/// user changed from the backend default — or "" when every pin is on its
/// default, so a project that never touched a mode round-trips without it.
pub fn iomode_section(modes: &std::collections::BTreeMap<usize, GpioMode>) -> String {
    if modes.is_empty() {
        return String::new();
    }
    let mut s = String::from(IOMODE_HEADER);
    s.push('\n');
    for (num, m) in modes {
        s.push_str(&format!("{num}={}\n", m.as_token()));
    }
    s
}

/// The `@labels` section — one `num=free text` per pin the user has named.
///
/// # Why this exists at all
///
/// A pin's name had no store of its own. Its only record was the generated
/// binding: `pin_binding` appends `sanitize_label(custom_label)` to the variable
/// name and `parse_pin_labels` reads that suffix back out of main.rs. Two things
/// fall out of that, and both were real:
///
/// * the round-trip is LOSSY, because the suffix has to be a Rust identifier.
///   "Status LED" is written as `pc13_out_status_led` and comes back
///   `status_led` — the user's capitals and space, gone on the next open;
/// * a pin with no binding has nowhere to keep a name at all. A Custom module's
///   pads are named in its own box before they are given a function, so those
///   names simply did not survive a save — and the module's `applied_sig`, which
///   records the label it last generated for, came back disagreeing with the
///   field beside it, so the module read as "changed" with nothing changed.
///
/// # Shape
///
/// `num=label`, the number FIRST and the split on the FIRST `=` — the same
/// decision `@groups` makes, for the same two reasons. A label is free text, so
/// it can contain `=`; and [`section_body`] ends a section at the first line
/// starting with `@`, so a label must never be able to start one.
///
/// Keyed on the pin NUMBER rather than its name, which is the identity the rest
/// of `mcu.config` uses: pin names carry vendor tags (`PB3 (JTDO-TRACESWO)`) and
/// are not stable across a re-import.
///
/// Empty when no pin is named, so a project that never used the field
/// round-trips without the section.
pub fn labels_section(labels: &std::collections::BTreeMap<usize, String>) -> String {
    let named: Vec<(&usize, &String)> = labels
        .iter()
        .filter(|(_, l)| !l.trim().is_empty())
        .collect();
    if named.is_empty() {
        return String::new();
    }
    let mut s = String::from(LABELS_HEADER);
    s.push('\n');
    for (num, label) in named {
        s.push_str(&format!("{num}={}\n", label.trim()));
    }
    s
}

/// Read `@labels` back. A line whose pin number does not parse is dropped.
pub fn parse_labels(text: &str) -> std::collections::BTreeMap<usize, String> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, LABELS_HEADER) else {
        return map;
    };
    for line in body.lines() {
        // The FIRST `=` only: everything after it is the label, `=` included.
        let Some((num, label)) = line.split_once('=') else {
            continue;
        };
        let Ok(num) = num.trim().parse::<usize>() else {
            continue;
        };
        // Only the padding the panel field allows is trimmed; the interior is
        // whatever the user typed.
        let label = label.trim();
        if !label.is_empty() {
            map.insert(num, label.to_owned());
        }
    }
    map
}

/// The `@pins` section — one `num=function label` per pin that has a function.
///
/// # Why this exists at all
///
/// A pin's FUNCTION had no store of its own either. On open it is recovered
/// by `codegen::parse_main_rs`, which reads the `// GPIO Output` label the
/// STM32 and ESP backends leave on every binding they generate. The nRF
/// backend leaves no such label (`let mut p0_21_out = Output::new(p.P0_21,
/// ...)`, with the pad name in a comment above and nothing about the
/// function), so an nRF project came back with an empty diagram - and, since
/// that is a state change, the next save wrote the empty block over the good
/// file.
///
/// Written only by the families that need it: `Mcu::mcu_config_text` gates
/// the call on `codegen::nrf::is_nrf`. Read unconditionally, since nothing
/// else writes the section, with `parse_main_rs` the fallback for any project
/// without it.
///
/// # Shape
///
/// `num=label`, keyed on the pin NUMBER and split on the FIRST `=`, as
/// `@iomode`, `@irq` and `@labels` are: a number comes from the chip
/// definition and never moves, where a pin name carries vendor tags. The
/// value is [`PinFunction::label`], reversed by [`PinFunction::from_label`] -
/// the pair the codegen comments already round-trip through, so a function
/// has one encoding, not two. An `Unset` pin is not written: it has no
/// binding to restore, and its label does not parse back.
///
/// Empty when no pin has a function, so a blank project round-trips without
/// the section.
pub fn pins_section(pins: &std::collections::BTreeMap<usize, PinFunction>) -> String {
    let set: Vec<(&usize, &PinFunction)> = pins
        .iter()
        .filter(|(_, f)| **f != PinFunction::Unset)
        .collect();
    if set.is_empty() {
        return String::new();
    }
    let mut s = String::from(PINS_HEADER);
    s.push('\n');
    for (num, f) in set {
        s.push_str(&format!("{num}={}\n", f.label()));
    }
    s
}

/// Read `@pins` back. A line whose number does not parse, or whose label
/// [`PinFunction::from_label`] does not know, is dropped rather than guessed
/// at: that pin comes back `Unset`, as it would from a hand-edited file, and
/// the pins around it still load.
pub fn parse_pins(text: &str) -> Vec<(usize, PinFunction)> {
    let Some(body) = section_body(text, PINS_HEADER) else {
        return Vec::new();
    };
    body.lines()
        .filter_map(|line| {
            // The FIRST `=` only, as every keyed section here splits.
            let (num, label) = line.split_once('=')?;
            let num = num.trim().parse::<usize>().ok()?;
            // The ends only: labels carry a double space INSIDE (`ADC0  IN0`).
            let func = PinFunction::from_label(label.trim())?;
            Some((num, func))
        })
        .collect()
}

/// Parse `@iomode` back into `pin -> GpioMode`; malformed lines are skipped.
pub fn parse_iomode(text: &str) -> std::collections::BTreeMap<usize, GpioMode> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, IOMODE_HEADER) else {
        return map;
    };
    for line in body.lines() {
        if let Some((n, m)) = line.trim().split_once('=') {
            if let (Ok(num), Some(mode)) = (n.trim().parse::<usize>(), GpioMode::from_token(m)) {
                map.insert(num, mode);
            }
        }
    }
    map
}

/// Parse `@irq` back into `pin -> Edge`; malformed lines are skipped.
pub fn parse_irq(text: &str) -> std::collections::BTreeMap<usize, (Edge, TaskPriority)> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, IRQ_HEADER) else {
        return map;
    };
    for line in body.lines() {
        let Some((n, rest)) = line.trim().split_once('=') else {
            continue;
        };
        // The priority is OPTIONAL: `7=rising` - every project written before
        // this existed - reads as Normal. An unrecognised token also falls back
        // to Normal rather than dropping the line: losing the interrupt
        // entirely is a far worse answer to a typo than losing its urgency.
        let (edge_tok, prio_tok) = match rest.split_once(',') {
            Some((e, prio)) => (e, Some(prio)),
            None => (rest, None),
        };
        if let (Ok(num), Some(edge)) = (n.trim().parse::<usize>(), Edge::from_token(edge_tok)) {
            let prio = prio_tok
                .and_then(TaskPriority::from_token)
                .unwrap_or_default();
            map.insert(num, (edge, prio));
        }
    }
    map
}

/// Parse the `@iopins` section back into the `pin → (x,y)` map; malformed lines
/// are skipped.
pub fn parse_iopins(text: &str) -> std::collections::BTreeMap<usize, (f32, f32)> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, IOPINS_HEADER) else {
        return map;
    };
    for line in body.lines() {
        let line = line.trim();
        if let Some((n, xy)) = line.split_once('=') {
            if let (Ok(num), Some((xs, ys))) = (n.trim().parse::<usize>(), xy.split_once(',')) {
                if let (Ok(x), Ok(y)) = (xs.trim().parse::<f32>(), ys.trim().parse::<f32>()) {
                    map.insert(num, (x, y));
                }
            }
        }
    }
    map
}

/// The token for a GPIO api style (`@gpio` section): "Native" or "Portable".
fn gpio_token(s: ApiStyle) -> &'static str {
    match s {
        ApiStyle::Native => "Native",
        ApiStyle::Portable => "Portable",
    }
}

/// File name written at the project root.
pub const FILE_NAME: &str = "mcu.config";

/// Where a project's pin functions come from on open.
#[derive(Debug)]
pub enum SavedPins {
    /// The `@pins` section of `mcu.config`: the store for a family whose
    /// generated block carries no label to read back (nRF).
    ByNumber(Vec<(usize, PinFunction)>),
    /// The `// label` on each binding in `src/main.rs`, the STM32 and ESP
    /// shape `codegen::parse_main_rs` has always read.
    ByName(Vec<(String, PinFunction)>),
}

/// The pins to restore, or `None` when neither store has any: a blank
/// project, a hand-written main.rs, or a family the parser cannot read that
/// was saved before `@pins` existed. The section wins when both are present -
/// it is written from the diagram itself, where the parse is a recovery.
///
/// ONE reader for both callers: opening a project, and the Board tab reading a
/// chip it has not opened.
pub fn saved_pins(cfg_text: Option<&str>, source: &str) -> Option<SavedPins> {
    let by_number = cfg_text.map(parse_pins).unwrap_or_default();
    if !by_number.is_empty() {
        return Some(SavedPins::ByNumber(by_number));
    }
    let by_name = crate::panels::mcu_module::codegen::parse_main_rs(source);
    (!by_name.is_empty()).then_some(SavedPins::ByName(by_name))
}

// The Structure tab's `@structure_layout` / `@structure_view` sections used to
// live here too. They moved to `project_structure.config` (see
// [`super::structure_config`]) because they change on every node drag, which
// made this file — real, reviewable configuration — permanently dirty in Git.
// `section_body` stays shared: both files use the same `@section` layout, and
// the migration path reads the old sections straight out of this one.

/// Build the `mcu.config` text from the MCU's `modules`, (STM32-only) clock and
/// `runtime`. Returns an empty string when there is nothing to persist (no
/// modules, no clock, and the default Blocking runtime), so the caller can skip
/// writing the file.
pub fn serialize(
    modules: &[VirtualModule],
    clock: Option<&Stm32f1Clock>,
    runtime: Runtime,
    gpio_api: ApiStyle,
) -> String {
    let mut out = String::new();

    if !modules.is_empty() {
        // Pretty RON: one module per block, each field on its own line (ron's
        // default omits struct names, matching the documented format). The
        // `None` variants are written bare, as every saved file has them - see
        // `ron_text::bare_none`.
        let pretty = ron::ser::to_string_pretty(&modules, ron::ser::PrettyConfig::new())
            .unwrap_or_else(|_| ron::to_string(&modules).unwrap_or_else(|_| "[]".into()));
        let pretty = super::ron_text::bare_none(&pretty);
        out.push_str(MODULES_HEADER);
        out.push('\n');
        out.push_str(&pretty);
        out.push('\n');
    }

    if let Some(c) = clock {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(CLOCK_HEADER);
        out.push('\n');
        out.push_str(&clock_persist::to_config_block(c));
        out.push('\n');
    }

    // Only persist a non-default runtime — a Blocking project keeps the file
    // free of the section, so old projects (and the common case) round-trip
    // byte-identically.
    if runtime != Runtime::Blocking {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(RUNTIME_HEADER);
        out.push('\n');
        out.push_str(runtime.as_token());
        out.push('\n');
    }

    // Only persist a non-default (Native) GPIO api — Portable (the default,
    // with the io.rs bridge) keeps the file free of the section.
    if gpio_api != ApiStyle::Portable {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(GPIO_HEADER);
        out.push('\n');
        out.push_str(gpio_token(gpio_api));
        out.push('\n');
    }

    out
}

/// Parse an `mcu.config` file back into `(modules, clock)`. A missing or garbled
/// section yields an empty module list / `None` clock. (Runtime is read
/// separately via [`parse_runtime`].)
pub fn parse(text: &str) -> (Vec<VirtualModule>, Option<Stm32f1Clock>) {
    // Through `normalize`, not straight out of RON: this is the door every
    // project the IDE saves comes back through, and the field migrations used to
    // run only on the legacy `main.rs` marker path.
    let mut modules = section_body(text, MODULES_HEADER)
        .and_then(|body| ron::from_str::<Vec<VirtualModule>>(body.trim()).ok())
        .unwrap_or_default();
    super::modules::persist::normalize(&mut modules);
    let clock = section_body(text, CLOCK_HEADER).map(|b| clock_persist::from_config_block(&b));
    (modules, clock)
}

/// The project [`Runtime`] recorded in `@runtime`; a missing section (any
/// pre-async project) is the default [`Runtime::Blocking`].
pub fn parse_runtime(text: &str) -> Runtime {
    section_body(text, RUNTIME_HEADER)
        .map(|b| Runtime::from_token(&b))
        .unwrap_or_default()
}

/// The GPIO api style recorded in `@gpio`; a missing section is the default
/// `Portable` (the io.rs embedded-hal bridge).
pub fn parse_gpio_api(text: &str) -> ApiStyle {
    match section_body(text, GPIO_HEADER).as_deref().map(str::trim) {
        Some("Native") => ApiStyle::Native,
        _ => ApiStyle::Portable,
    }
}

/// One `@modulenotes` entry: a single RON line.
///
/// ONE LINE PER ENTRY, not one RON list like `@modules`, so a line that does
/// not parse loses only itself. That matters more here than anywhere else in the
/// file: this is the section a user is most likely to hand-edit, and `@modules`
/// shows what the alternative costs - one bad field there drops the whole list.
///
/// A line can never start a new section. `ron` writes every string through
/// `escape_debug`, so a newline in the notes becomes `\n` and the line always
/// begins with `(` - never with the `@` that [`section_body`] stops at.
#[derive(serde::Serialize, serde::Deserialize)]
struct NoteLine {
    kind: crate::panels::mcu_module::modules::ModuleKind,
    instance: u8,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    link: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    image: String,
}

/// Write `@modulenotes`: one line per peripheral instance that holds notes.
///
/// Empty when there are none, so a project that never used notes round-trips
/// byte for byte without the section.
pub fn notes_section(
    notes: &std::collections::BTreeMap<
        crate::panels::mcu_module::modules::NotesKey,
        crate::panels::mcu_module::modules::ModuleNotes,
    >,
) -> String {
    let mut lines = Vec::new();
    for ((kind, instance), n) in notes {
        if n.is_empty() {
            continue;
        }
        let line = NoteLine {
            kind: *kind,
            instance: *instance,
            text: n.text.clone(),
            link: n.link.trim().to_owned(),
            image: n.image.trim().to_owned(),
        };
        if let Ok(l) = ron::to_string(&line) {
            lines.push(l);
        }
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut s = String::from(NOTES_HEADER);
    s.push('\n');
    for l in lines {
        s.push_str(&l);
        s.push('\n');
    }
    s
}

/// Read `@modulenotes` back. A line that does not parse is skipped - it and
/// only it - and an entry with nothing in it is not kept.
///
/// Unknown fields are ignored (`ron` skips them), so a file written by a newer
/// IDE still opens here.
pub fn parse_notes(
    text: &str,
) -> std::collections::BTreeMap<
    crate::panels::mcu_module::modules::NotesKey,
    crate::panels::mcu_module::modules::ModuleNotes,
> {
    let mut map = std::collections::BTreeMap::new();
    let Some(body) = section_body(text, NOTES_HEADER) else {
        return map;
    };
    for line in body.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(n) = ron::from_str::<NoteLine>(line) else {
            continue;
        };
        let notes = crate::panels::mcu_module::modules::ModuleNotes {
            text: n.text,
            link: n.link,
            image: n.image,
        };
        if !notes.is_empty() {
            map.insert((n.kind, n.instance), notes);
        }
    }
    map
}

/// The lines belonging to `header`: everything after the header line up to (but
/// excluding) the next `@`-prefixed section header, or EOF.
pub(super) fn section_body(text: &str, header: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| l.trim() == header)?;
    let mut body: Vec<&str> = Vec::new();
    for &l in &lines[start + 1..] {
        if l.trim_start().starts_with('@') {
            break;
        }
        body.push(l);
    }
    Some(body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::clock::model::{Stm32f1Clock, SysclkSrc};
    use crate::panels::mcu_module::modules::{
        Connection, I2cModuleConfig, ModuleConfig, ModuleKind, ModuleSignal, VirtualModule,
    };

    fn sample_module() -> VirtualModule {
        let mut cfg = I2cModuleConfig::new(1);
        cfg.custom_label = "128x32 display".into();
        VirtualModule {
            id: "_i2c_1".into(),
            kind: ModuleKind::GenericInterfaceI2c,
            name: "I2C1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::I2c(cfg),
            connections: vec![
                Connection {
                    signal: ModuleSignal::Scl,
                    mcu_pin: 45,
                },
                Connection {
                    signal: ModuleSignal::Sda,
                    mcu_pin: 46,
                },
            ],
        }
    }

    #[test]
    fn modules_and_clock_round_trip() {
        let modules = vec![sample_module()];
        let clock = Stm32f1Clock {
            sysclk_src: SysclkSrc::Hsi,
            ..Stm32f1Clock::default()
        };
        let text = serialize(
            &modules,
            Some(&clock),
            Runtime::Blocking,
            ApiStyle::Portable,
        );

        // Headers + multi-line layout present.
        assert!(text.contains("@modules\n"));
        assert!(text.contains("@clock\n"));
        assert!(text.contains("hse=8000000"));
        assert!(text.lines().count() > 10, "must be multi-line:\n{text}");

        let (m2, c2) = parse(&text);
        assert_eq!(m2, modules, "modules round-trip");
        assert_eq!(c2, Some(clock), "clock round-trip");
    }

    /// The live door runs the field migrations.
    ///
    /// `mcu.config` is what every project the IDE saves comes back through, and
    /// its module section was a bare `ron::from_str` with no migration hook at
    /// all. The hook lived on the legacy `// @modules` marker path in `main.rs`,
    /// which only a project WITHOUT an `mcu.config` ever takes — so a field shape
    /// brought forward there was brought forward for nobody, and had not run for
    /// a real project in a long time. `dedupe_ids` was in the same position.
    ///
    /// Tested through the PWM duty migration because that one has a visible
    /// consequence: a pre-hundredths project's duty is in `duty`, and a reader of
    /// `duty_x100` sees 0 % until something folds it forward. `duty` is
    /// `skip_serializing_if`, so writing a config with only the legacy map
    /// produces a file shaped exactly like the old ones.
    #[test]
    fn the_live_door_runs_the_field_migrations() {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, TimerModuleConfig};

        let mut legacy_cfg = TimerModuleConfig::new(1);
        legacy_cfg.duty_x100.clear();
        legacy_cfg.duty.insert(1, 42);
        let legacy = VirtualModule {
            id: "timer_1".into(),
            kind: ModuleKind::GenericInterfaceTimer,
            name: "TIM1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Timer(legacy_cfg),
            connections: Vec::new(),
        };

        let text = serialize(&[legacy], None, Runtime::Blocking, ApiStyle::Portable);
        assert!(
            text.contains("duty:"),
            "the legacy map must reach the file, or this proves nothing:\n{text}"
        );

        let (back, _) = parse(&text);
        let ModuleConfig::Timer(c) = &back[0].config else {
            panic!("a Timer module came back as something else");
        };
        assert_eq!(
            c.duty_x100.get(&1),
            Some(&4200),
            "the legacy whole-percent duty was never folded forward"
        );
    }

    #[test]
    fn empty_when_nothing_to_persist() {
        assert_eq!(
            serialize(&[], None, Runtime::Blocking, ApiStyle::Portable),
            ""
        );
    }

    #[test]
    fn rotation_round_trips() {
        assert_eq!(rotation_section(false), "");
        assert!(!parse_rotation(""));
        let t = rotation_section(true);
        assert!(t.contains("@rotation"));
        assert!(parse_rotation(&t));
    }

    #[test]
    fn iopins_round_trip() {
        let mut m = std::collections::BTreeMap::new();
        assert_eq!(iopins_section(&m), "");
        assert!(parse_iopins("").is_empty());
        m.insert(13usize, (12.5_f32, -8.0_f32));
        m.insert(45usize, (100.0_f32, 40.0_f32));
        let t = iopins_section(&m);
        assert!(t.starts_with("@iopins\n"), "{t}");
        assert_eq!(parse_iopins(&t), m);
    }

    /// The `@clocknodes` section carries a tree no `Stm32f1Clock` could
    /// describe, and does not disturb the sections around it.
    #[test]
    fn clock_nodes_section_round_trips() {
        let body = "pllsrc=i1
plln=v100
hse=s1:12000000";
        let section = clock_nodes_section(body);
        assert!(section.starts_with(
            "@clocknodes
"
        ));

        // Sitting between two other sections, it neither swallows nor leaks.
        let text = format!(
            "{}
{}{}",
            strict_section(true),
            section,
            clock_manual_section(true)
        );
        assert_eq!(parse_clock_nodes(&text).as_deref(), Some(body));
        assert_eq!(parse_clock_manual(&text), Some(true));

        // An untouched clock writes no section, so the file is unchanged.
        assert!(clock_nodes_section("").is_empty());
        assert!(
            clock_nodes_section(
                "   
 "
            )
            .is_empty()
        );
        assert_eq!(
            parse_clock_nodes(
                "@strict
on
"
            ),
            None
        );
    }

    #[test]
    fn clock_only_when_no_modules() {
        let text = serialize(
            &[],
            Some(&Stm32f1Clock::default()),
            Runtime::Blocking,
            ApiStyle::Portable,
        );
        assert!(!text.contains("@modules"));
        assert!(text.starts_with("@clock\n"));
        let (m, c) = parse(&text);
        assert!(m.is_empty());
        assert_eq!(c, Some(Stm32f1Clock::default()));
    }

    #[test]
    fn runtime_round_trips_and_defaults_to_blocking() {
        // Default runtime writes NO section — old projects stay byte-identical.
        assert_eq!(
            serialize(&[], None, Runtime::Blocking, ApiStyle::Portable),
            ""
        );
        assert_eq!(parse_runtime(""), Runtime::Blocking);

        // Async is persisted and parsed back, even with no modules/clock.
        let text = serialize(&[], None, Runtime::Async, ApiStyle::Portable);
        assert!(text.contains("@runtime\n"));
        assert!(text.contains("Async"));
        assert_eq!(parse_runtime(&text), Runtime::Async);

        // Native round-trips too.
        let text = serialize(&[], None, Runtime::Native, ApiStyle::Portable);
        assert!(text.contains("@runtime\n") && text.contains("Native"));
        assert_eq!(parse_runtime(&text), Runtime::Native);

        // …and it coexists with modules + clock.
        let text = serialize(
            &[sample_module()],
            Some(&Stm32f1Clock::default()),
            Runtime::Async,
            ApiStyle::Portable,
        );
        let (m, c) = parse(&text);
        assert_eq!(m.len(), 1);
        assert!(c.is_some());
        assert_eq!(parse_runtime(&text), Runtime::Async);
    }

    #[test]
    fn gpio_api_round_trips_and_defaults_to_portable() {
        // Default (Portable) writes NO @gpio section.
        assert_eq!(
            serialize(&[], None, Runtime::Blocking, ApiStyle::Portable),
            ""
        );
        assert_eq!(parse_gpio_api(""), ApiStyle::Portable);

        // Native is persisted + parsed back, independent of runtime.
        let text = serialize(&[], None, Runtime::Blocking, ApiStyle::Native);
        assert!(text.contains("@gpio\n") && text.contains("Native"));
        assert_eq!(parse_gpio_api(&text), ApiStyle::Native);
        assert_eq!(
            parse_runtime(&text),
            Runtime::Blocking,
            "gpio section doesn't disturb runtime"
        );

        // Coexists with the runtime section.
        let text = serialize(&[], None, Runtime::Async, ApiStyle::Native);
        assert_eq!(parse_runtime(&text), Runtime::Async);
        assert_eq!(parse_gpio_api(&text), ApiStyle::Native);
    }

    #[test]
    fn autobuild_round_trips_and_defaults_to_check() {
        use crate::panels::mcu_module::mcu::AutoBuild;
        // Default (Check) writes NO section; missing section parses as Check.
        assert_eq!(autobuild_section(AutoBuild::Check), "");
        assert_eq!(parse_autobuild(""), AutoBuild::Check);
        // Off / Release persist + parse back, independent of the other sections.
        for mode in [AutoBuild::Off, AutoBuild::Release] {
            let text = format!(
                "{}{}",
                serialize(&[], None, Runtime::Async, ApiStyle::Native),
                {
                    let s = autobuild_section(mode);
                    // (mcu_config_text joins with a blank line; a leading one here
                    //  is harmless for the section parser.)
                    format!("\n{s}")
                }
            );
            assert!(text.contains("@autobuild\n"));
            assert_eq!(parse_autobuild(&text), mode);
            // …and it doesn't disturb the other sections.
            assert_eq!(parse_runtime(&text), Runtime::Async);
            assert_eq!(parse_gpio_api(&text), ApiStyle::Native);
        }
    }

    #[test]
    fn parse_ignores_garbage() {
        let (m, c) = parse("not a config file\n");
        assert!(m.is_empty());
        assert!(c.is_none());
    }

    /// The Structure sections moved to their own file, but a `mcu.config` that
    /// still carries them (saved before the split) must keep parsing.
    #[test]
    fn legacy_structure_sections_do_not_break_parsing() {
        use crate::panels::mcu_module::structure_config;
        let mut pos = structure_config::StructurePositions::new();
        pos.insert("main.rs".into(), (14.0, 14.0));
        pos.insert("mw_radar/utils.rs".into(), (321.5, 208.0));

        // A legacy file: MCU sections followed by the old Structure ones.
        let mut text = serialize(
            &[sample_module()],
            Some(&Stm32f1Clock::default()),
            Runtime::Blocking,
            ApiStyle::Portable,
        );
        text.push('\n');
        use structure_config::CLOCK_VIEW_DEFAULT;
        text.push_str(&structure_config::serialize(
            &pos,
            &(true, Some(2), 0, false),
            &Default::default(),
            &CLOCK_VIEW_DEFAULT,
            &Default::default(),
        ));

        let (m, c) = parse(&text);
        assert_eq!(m.len(), 1, "modules unaffected by the extra sections");
        assert!(c.is_some(), "clock unaffected by the extra sections");
        // And the migration reader still finds the positions in there.
        assert_eq!(structure_config::parse_layout(&text), pos);
    }
}

#[cfg(test)]
mod flashstore_section_tests {
    use super::*;
    use crate::panels::mcu_module::flash_store::{FlashStoreConfig, FlashStoreMode};

    /// Both modes round-trip, and the partition's place survives a trip
    /// through `nvs` because all four fields are always written.
    #[test]
    fn the_flash_store_round_trips_in_both_modes() {
        let c = FlashStoreConfig::default_for("esp32c3");
        let text = flashstore_section(Some(&c));
        assert_eq!(text, "@flashstore\npartition 0x400000 0x4000 0x3FC000\n");
        assert_eq!(parse_flashstore(&text), Some(c));
        let nvs = FlashStoreConfig {
            mode: FlashStoreMode::Nvs,
            ..c
        };
        assert_eq!(parse_flashstore(&flashstore_section(Some(&nvs))), Some(nvs));
        assert_eq!(flashstore_section(None), "");
        assert_eq!(parse_flashstore(""), None);
    }

    /// A malformed line is dropped, never defaulted: a store the tab cannot
    /// show must not reach the firmware.
    #[test]
    fn a_malformed_flash_store_line_is_dropped() {
        for bad in [
            "@flashstore\npartition 0x400000 0x4000\n",
            "@flashstore\nsomewhere 0x400000 0x4000 0x3FC000\n",
            "@flashstore\npartition 4194304 0x4000 0x3FC000\n",
            "@flashstore\npartition 0x400000 0x4000 0x3FC000 extra\n",
        ] {
            assert_eq!(parse_flashstore(bad), None, "{bad}");
        }
    }
}

#[cfg(test)]
mod watchdog_section_tests {
    use super::*;
    use crate::panels::mcu_module::watchdog::{
        EspWdtConfig, IwdgConfig, WatchdogSettings, WwdgConfig,
    };

    #[test]
    fn both_watchdogs_round_trip() {
        let w = WatchdogSettings {
            iwdg: Some(IwdgConfig {
                timeout_us: 32_768_000,
            }),
            wwdg: Some(WwdgConfig {
                timeout_us: 41_472,
                window_us: 5_000,
            }),
            ..Default::default()
        };
        assert_eq!(parse_watchdog(&watchdog_section(&w)), w);
    }

    /// The ESP three round-trip on their own keys, and do NOT come back as the
    /// STM32 pair — which is what reusing `iwdg` for the RWDT would have done.
    #[test]
    fn the_esp_watchdogs_round_trip_on_their_own_keys() {
        let w = WatchdogSettings {
            rwdt: Some(EspWdtConfig {
                timeout_us: 2_000_000,
            }),
            mwdt0: Some(EspWdtConfig {
                timeout_us: 750_000,
            }),
            mwdt1: Some(EspWdtConfig { timeout_us: 15 }),
            ..Default::default()
        };
        let text = watchdog_section(&w);
        assert_eq!(parse_watchdog(&text), w, "{text}");
        assert!(!text.contains("iwdg"), "{text}");

        // And an STM32 project keeps writing nothing for them.
        let stm = WatchdogSettings {
            iwdg: Some(IwdgConfig { timeout_us: 1_000 }),
            ..Default::default()
        };
        let text = watchdog_section(&stm);
        assert!(!text.contains("wdt"), "{text}");
        assert_eq!(parse_watchdog(&text), stm);
    }

    /// The RP watchdog comes back on its own key, and is not read as any of
    /// the others - nor any of them as it.
    #[test]
    fn the_rp_watchdog_round_trips_on_its_own_key() {
        use crate::panels::mcu_module::watchdog::RpWdtConfig;
        let w = WatchdogSettings {
            rp: Some(RpWdtConfig {
                timeout_us: 8_388_607,
            }),
            ..Default::default()
        };
        let text = watchdog_section(&w);
        assert_eq!(parse_watchdog(&text), w, "{text}");
        assert!(text.contains("rp_watchdog 8388607"), "{text}");
        assert!(!text.contains("iwdg") && !text.contains("rwdt"), "{text}");

        let esp = WatchdogSettings {
            rwdt: Some(EspWdtConfig { timeout_us: 1_000 }),
            ..Default::default()
        };
        assert_eq!(parse_watchdog(&watchdog_section(&esp)).rp, None);
    }

    /// The nRF WDT likewise, and it is not read as the RP one: both are "the
    /// chip's watchdog", which is exactly why each has its own key.
    #[test]
    fn the_nrf_watchdog_round_trips_on_its_own_key() {
        use crate::panels::mcu_module::watchdog::{NrfWdtConfig, RpWdtConfig};
        let w = WatchdogSettings {
            nrf: Some(NrfWdtConfig { timeout_us: 458 }),
            ..Default::default()
        };
        let text = watchdog_section(&w);
        assert_eq!(parse_watchdog(&text), w, "{text}");
        assert!(text.contains("nrf_watchdog 458"), "{text}");
        assert!(!text.contains("rp_watchdog"), "{text}");

        let rp = WatchdogSettings {
            rp: Some(RpWdtConfig { timeout_us: 1_000 }),
            ..Default::default()
        };
        assert_eq!(parse_watchdog(&watchdog_section(&rp)).nrf, None);
    }

    #[test]
    fn each_one_alone_round_trips_too() {
        for w in [
            WatchdogSettings {
                iwdg: Some(IwdgConfig { timeout_us: 1_000 }),
                ..Default::default()
            },
            WatchdogSettings {
                wwdg: Some(WwdgConfig {
                    timeout_us: 900,
                    window_us: 0,
                }),
                ..Default::default()
            },
            WatchdogSettings {
                mwdt1: Some(EspWdtConfig {
                    timeout_us: 5_000_000,
                }),
                ..Default::default()
            },
        ] {
            assert_eq!(parse_watchdog(&watchdog_section(&w)), w);
        }
    }

    #[test]
    fn nothing_enabled_writes_no_section() {
        // An empty section would leave `@watchdog` in every project file
        // that never touched the tab.
        assert_eq!(watchdog_section(&WatchdogSettings::default()), "");
        assert_eq!(parse_watchdog(""), WatchdogSettings::default());
    }

    #[test]
    fn a_malformed_line_is_dropped_not_defaulted() {
        // Substituting a number would put a watchdog in the firmware that
        // the user cannot see in the tab - the worst possible outcome for a
        // peripheral whose whole job is resetting the board.
        let w = parse_watchdog(
            "@watchdog
iwdg
wwdg 500
",
        );
        assert_eq!(w, WatchdogSettings::default(), "partial lines must vanish");
        // …and a WWDG without its window is partial, not a 0-window one.
        assert_eq!(
            parse_watchdog(
                "@watchdog
wwdg 500
"
            )
            .wwdg,
            None
        );
    }
}

#[cfg(test)]
mod comp_section_tests {
    use super::{comp_section, parse_comp};
    use crate::panels::mcu_module::comparator::{
        BlankingSource, CompConfig, CompSettings, Hysteresis, InvertingInput, OutputPolarity,
        PowerMode,
    };

    #[test]
    fn a_comparator_survives_the_round_trip() {
        let mut c = CompSettings::new();
        c.insert(
            2,
            CompConfig {
                power_mode: PowerMode::MediumSpeed,
                hysteresis: Hysteresis::Mv40,
                output_polarity: OutputPolarity::Inverted,
                inverting_input: InvertingInput::Dac2,
                blanking_source: BlankingSource::Blank1,
            },
        );
        c.insert(7, CompConfig::default());
        let text = comp_section(&c);
        assert_eq!(parse_comp(&text), c);
        // Nothing configured writes NO section, so an untouched project's
        // mcu.config does not grow an empty header.
        assert!(comp_section(&CompSettings::new()).is_empty());
        assert!(parse_comp("").is_empty());
    }

    /// Same policy as `@watchdog`: half a line is no line. A comparator the tab
    /// cannot show must not reach the firmware, and defaulting a field would
    /// quietly change what it compares against.
    #[test]
    fn an_incomplete_line_is_dropped_whole() {
        for bad in [
            "@comp\n2 MediumSpeed Mv40 Inverted Dac2\n",
            "@comp\n2 MediumSpeed Mv40 Inverted Dac2 Blank1 extra\n",
            "@comp\nx MediumSpeed Mv40 Inverted Dac2 Blank1\n",
            "@comp\n2 Turbo Mv40 Inverted Dac2 Blank1\n",
            "@comp\n2 MediumSpeed Mv999 Inverted Dac2 Blank1\n",
        ] {
            assert!(parse_comp(bad).is_empty(), "{bad}");
        }
        // ...while a good line right after a bad one still lands.
        let mixed = "@comp\n2 Turbo Mv40 Inverted Dac2 Blank1\n7 HighSpeed None NotInverted HalfVref None\n";
        assert_eq!(parse_comp(mixed).len(), 1);
        assert!(parse_comp(mixed).contains_key(&7));
    }
}

#[cfg(test)]
mod lpuart_persist_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{
        Connection, ModuleConfig, ModuleKind, ModuleSignal, UsartModuleConfig, VirtualModule,
    };

    /// The LPUART variant survives `@modules` — it shares the USART's settings
    /// struct, so the only thing that can go wrong is the variant itself being
    /// read back as a USART, which would silently move the module to the wrong
    /// peripheral.
    #[test]
    fn an_lpuart_module_round_trips() {
        let mut cfg = UsartModuleConfig::new(1);
        cfg.baud_rate = 9600;
        let m = VirtualModule {
            id: "lpuart_1".into(),
            kind: ModuleKind::GenericInterfaceLpuart,
            name: "LPUART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Lpuart(cfg),
            connections: vec![
                Connection {
                    signal: ModuleSignal::LpTx,
                    mcu_pin: 21,
                },
                Connection {
                    signal: ModuleSignal::LpRx,
                    mcu_pin: 22,
                },
            ],
        };
        let text = serialize(&[m], None, Runtime::Blocking, ApiStyle::Portable);
        let (back, _) = parse(&text);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].kind, ModuleKind::GenericInterfaceLpuart);
        assert!(matches!(back[0].config, ModuleConfig::Lpuart(_)));
        assert_eq!(back[0].instance(), 1);
        assert_eq!(back[0].pin_for(ModuleSignal::LpTx), Some(21));
    }

    /// `Parity::None` and `UsartFlow::None` are saved bare, as every mcu.config
    /// on disk has them. ron 0.12 wrote `r#None`, so an untouched project with
    /// a USART compared as "unsaved" on open and diffed in Git on first save.
    #[test]
    fn a_none_variant_is_saved_bare_and_the_text_round_trips() {
        let mut cfg = UsartModuleConfig::new(1);
        cfg.custom_label = "keeps: r#None,".into();
        let m = VirtualModule {
            id: "usart_1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            name: "USART1".into(),
            pos: (0.0, 0.0),
            config: ModuleConfig::Usart(cfg),
            connections: Vec::new(),
        };
        let text = serialize(&[m], None, Runtime::Blocking, ApiStyle::Portable);
        let lf = text.replace("\r\n", "\n");
        assert!(lf.contains("\n            parity: None,\n"), "{lf}");
        assert!(lf.contains("\n            flow: None,\n"), "{lf}");
        assert!(!lf.contains(": r#None,\n"), "{lf}");
        assert!(
            lf.contains("\"keeps: r#None,\""),
            "a label keeps its text: {lf}"
        );
        // Byte for byte: what was read is what gets written back.
        let (back, _) = parse(&text);
        assert_eq!(
            serialize(&back, None, Runtime::Blocking, ApiStyle::Portable),
            text
        );
    }
}

#[cfg(test)]
mod group_tests {
    use super::{PinGroup, groups_section, parse_groups};

    fn g(name: &str, pins: &[usize]) -> PinGroup {
        PinGroup {
            name: name.to_owned(),
            pins: pins.iter().copied().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_board_full_of_devices_round_trips() {
        let groups = vec![g("mw radar", &[4, 5, 6]), g("display", &[10, 11])];
        let back = parse_groups(&groups_section(&groups));
        assert_eq!(back, groups);
    }

    /// A project that groups nothing must write no section at all, or every
    /// existing `mcu.config` gains a line it did not have and stops matching
    /// itself across a save.
    #[test]
    fn nothing_grouped_writes_nothing() {
        assert_eq!(groups_section(&[]), "");
        assert_eq!(groups_section(&[g("", &[1]), g("empty", &[])]), "");
        assert!(parse_groups("@iopins\n1=3.0,4.0\n").is_empty());
    }

    /// The section ends where the next `@` begins - the shared rule for every
    /// section in this file. A group reading past its own body would swallow
    /// `@iopins` and drop the lot as unparseable.
    #[test]
    fn the_section_stops_at_the_next_one() {
        let text = format!("{}@irq\n7=rising\n", groups_section(&[g("radar", &[4, 5])]));
        assert_eq!(parse_groups(&text), vec![g("radar", &[4, 5])]);
    }

    /// Names are free text typed into a panel field. Everything but a newline
    /// has to survive - including the `=` the line is split on, and a leading
    /// `@`, which on the obvious `name=pins` layout would end the section.
    #[test]
    fn a_name_may_hold_anything_but_a_newline() {
        for name in ["a=b", "SPI, and the reset line", "  padded  ", "@iopins"] {
            let back = parse_groups(&groups_section(&[g(name, &[2])]));
            assert_eq!(back.len(), 1, "{name}");
            assert_eq!(back[0].name, name.trim(), "{name}");
            assert_eq!(back[0].pins, [2].into_iter().collect(), "{name}");
        }
    }

    /// A line whose pins do not parse is DROPPED, never half-read: a group that
    /// kept the pads it could read would silently claim a different set than the
    /// one saved, and the diagram would mark pads the user never grouped.
    #[test]
    fn a_half_readable_line_is_dropped_whole() {
        assert!(parse_groups("@groups\n4,x,6=radar\n").is_empty());
        assert!(parse_groups("@groups\n=radar\n").is_empty());
        assert!(parse_groups("@groups\n4,5=\n").is_empty());
        // …and it takes only itself with it.
        assert_eq!(
            parse_groups("@groups\n4,x=radar\n10,11=display\n"),
            vec![g("display", &[10, 11])]
        );
    }

    /// A device whose name starts with `@` must not end the section. This is the
    /// whole reason the line is written pins-first, so it is checked with a
    /// group AFTER it - the one that would otherwise be lost.
    #[test]
    fn a_name_starting_with_an_at_does_not_end_the_section() {
        let groups = vec![g("@radar", &[4, 5]), g("display", &[10])];
        assert_eq!(parse_groups(&groups_section(&groups)), groups);
    }

    fn with_i2c(mut group: PinGroup, devs: &[(u8, u32)]) -> PinGroup {
        group.i2c = devs.iter().copied().collect();
        group
    }

    /// Devices of an I2C bus round-trip, in a group with pads and in a group of
    /// their own - which keeps its place in the order, the order of the
    /// generated comment and of the roster.
    #[test]
    fn i2c_devices_in_a_group_round_trip_in_order() {
        let groups = vec![
            with_i2c(g("sensors", &[4]), &[(1, 2)]),
            with_i2c(g("display", &[]), &[(1, 1), (0, 5)]),
            g("radar", &[9]),
        ];
        let text = groups_section(&groups);
        assert!(
            text.contains("\n=display\n"),
            "the order placeholder: {text}"
        );
        assert!(
            text.contains("@groupi2c\ni2c1/2=sensors\ni2c0/5,i2c1/1=display\n"),
            "{text}"
        );
        assert_eq!(parse_groups(&text), groups);
    }

    /// What a build that has never heard of `@groupi2c` reads: every pad group
    /// whole - `@groups` ends at the next section, and the placeholder of a
    /// group holding only I2C devices is a line it already drops.
    #[test]
    fn an_older_reader_keeps_every_pad_group() {
        let groups = vec![
            with_i2c(g("sensors", &[4]), &[(1, 2)]),
            with_i2c(g("display", &[]), &[(1, 1)]),
        ];
        let text = groups_section(&groups);
        let old_body: Vec<&str> = text
            .lines()
            .skip(1)
            .take_while(|l| !l.starts_with('@'))
            .collect();
        assert_eq!(old_body, vec!["4=sensors", "=display"]);
    }

    /// A placeholder with nothing behind it is still no group; a device claimed
    /// twice (a hand edit) stays in the first; a token that is not one drops
    /// its line; refs for a name `@groups` never mentioned still make a group.
    #[test]
    fn the_i2c_section_is_read_strictly() {
        assert!(parse_groups("@groups\n=display\n").is_empty());
        assert_eq!(
            parse_groups("@groups\n=a\n=b\n@groupi2c\ni2c1/1=a\ni2c1/1,i2c1/2=b\n"),
            vec![
                with_i2c(g("a", &[]), &[(1, 1)]),
                with_i2c(g("b", &[]), &[(1, 2)])
            ]
        );
        assert_eq!(
            parse_groups("@groups\n4=a\n@groupi2c\ni2c1/x=a\n"),
            vec![g("a", &[4])]
        );
        assert!(
            parse_groups("@groupi2c\ni2c1/0=a\n").is_empty(),
            "uid 0 is never minted"
        );
        assert_eq!(
            parse_groups("@groupi2c\ni2c2/7=late\n"),
            vec![with_i2c(g("late", &[]), &[(2, 7)])]
        );
    }

    /// Liveness counts an I2C device as a member: a group holding only one is a
    /// device, written and drawn; one holding nothing is not.
    #[test]
    fn a_group_of_i2c_devices_only_is_live() {
        assert!(with_i2c(g("display", &[]), &[(1, 1)]).is_live());
        assert!(!g("display", &[]).is_live());
        assert!(!with_i2c(g("  ", &[]), &[(1, 1)]).is_live());
    }
}

#[cfg(test)]
mod labels_section_tests {
    use super::{labels_section, parse_labels};
    use std::collections::BTreeMap;

    fn map(pairs: &[(usize, &str)]) -> BTreeMap<usize, String> {
        pairs.iter().map(|(n, l)| (*n, (*l).to_owned())).collect()
    }

    /// The case the binding-suffix store could not carry: capitals and a space.
    ///
    /// `sanitize_label` lowercases and folds every non-alphanumeric to `_`, so
    /// "Status LED" was written into the variable name as `status_led` and came
    /// back as `status_led` on the next open.
    #[test]
    fn free_text_survives_the_round_trip() {
        let want = map(&[(13, "Status LED"), (2, "VBAT sense")]);
        let text = labels_section(&want);
        assert_eq!(parse_labels(&text), want);
    }

    /// A label can contain `=`, so the split is on the FIRST one only - the
    /// same decision `@groups` makes about free-text device names.
    #[test]
    fn an_equals_sign_inside_a_label_is_kept() {
        let want = map(&[(7, "Vref = 3V3")]);
        let text = labels_section(&want);
        assert_eq!(parse_labels(&text), want);
    }

    /// `section_body` ends a section at the first line starting with `@`, so the
    /// number has to come first - a label may well start with one.
    #[test]
    fn a_label_starting_with_an_at_sign_does_not_truncate_the_section() {
        let want = map(&[(4, "@irq handler"), (9, "later")]);
        let text = format!("{}\n@iomode\n4=PushPull\n", labels_section(&want));
        assert_eq!(
            parse_labels(&text),
            want,
            "both labels survive, and the @iomode line after them is not one"
        );
    }

    /// A project that never named a pin round-trips with no section at all.
    #[test]
    fn nothing_named_writes_nothing() {
        assert!(labels_section(&BTreeMap::new()).is_empty());
        assert!(labels_section(&map(&[(1, "   ")])).is_empty(), "nor blanks");
        assert!(parse_labels("").is_empty());
        assert!(
            parse_labels("@modules\nx\n").is_empty(),
            "and an old project without the section reads as unnamed"
        );
    }

    /// A malformed line is dropped rather than guessed at.
    #[test]
    fn a_line_without_a_pin_number_is_dropped() {
        let text = "@labels\nPC13=led\n=orphan\n13=real\n";
        assert_eq!(parse_labels(text), map(&[(13, "real")]));
    }
}

#[cfg(test)]
mod pins_section_tests {
    use super::{parse_pins, pins_section};
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    use std::collections::BTreeMap;

    fn map(pairs: &[(usize, PinFunction)]) -> BTreeMap<usize, PinFunction> {
        pairs.iter().cloned().collect()
    }

    /// One label of each shape the micro:bit offers, including the ones with
    /// a double space in the middle (`ADC0  IN3`) that a trim must not touch.
    #[test]
    fn a_wired_board_round_trips() {
        let want = vec![
            (2, PinFunction::GpioOutput),
            (5, PinFunction::GpioInput),
            (7, PinFunction::AdcChannel { adc: 0, channel: 3 }),
            (9, PinFunction::UsartTx(0)),
            (11, PinFunction::SpiSck(2)),
            (14, PinFunction::I2cSda(0)),
            (
                20,
                PinFunction::TimerPwm {
                    timer: 0,
                    channel: 1,
                },
            ),
        ];
        let text = pins_section(&map(&want));
        assert_eq!(parse_pins(&text), want, "{text}");
    }

    /// A blank project writes no section, and an old project without one
    /// reads as nothing saved - which is what sends the open path to
    /// `parse_main_rs`.
    #[test]
    fn nothing_wired_writes_nothing() {
        assert!(pins_section(&BTreeMap::new()).is_empty());
        assert!(
            pins_section(&map(&[(3, PinFunction::Unset)])).is_empty(),
            "nor an Unset pad, which has no binding to restore"
        );
        assert!(parse_pins("").is_empty());
        assert!(parse_pins("@modules\nx\n").is_empty());
    }

    /// A label `from_label` does not know, or a line without a number, is
    /// dropped on its own; the pins around it still load.
    #[test]
    fn an_unknown_label_is_skipped_not_fatal() {
        let text =
            "@pins\n2=GPIO Output\n3=Not configured\nP0.21=GPIO Input\n=GPIO Input\n5=GPIO Input\n";
        assert_eq!(
            parse_pins(text),
            vec![(2, PinFunction::GpioOutput), (5, PinFunction::GpioInput)]
        );
    }

    /// The section ends at the next header. The `@labels` line after it
    /// WOULD parse as a function, so this proves the boundary rather than the
    /// label check.
    #[test]
    fn the_section_stops_at_the_next_one() {
        let text = format!(
            "{}@labels\n4=GPIO Input\n",
            pins_section(&map(&[(2, PinFunction::GpioOutput)]))
        );
        assert_eq!(parse_pins(&text), vec![(2, PinFunction::GpioOutput)]);
    }
}

#[cfg(test)]
mod irq_priority_round_trip {
    use super::{irq_section, parse_irq};
    use crate::panels::mcu_module::pins::logic::pin::{Edge, TaskPriority};
    use std::collections::BTreeMap;

    fn map(v: &[(usize, Edge, TaskPriority)]) -> BTreeMap<usize, (Edge, TaskPriority)> {
        v.iter().map(|(n, e, p)| (*n, (*e, *p))).collect()
    }

    /// A file written before priorities existed reads back unchanged.
    ///
    /// This is the property that matters most: every project on disk today has
    /// the short form, and none of them may change meaning.
    #[test]
    fn an_old_file_reads_as_normal() {
        let got = parse_irq("@irq\n7=Rising\n9=Both\n");
        assert_eq!(
            got,
            map(&[
                (7, Edge::Rising, TaskPriority::Normal),
                (9, Edge::Both, TaskPriority::Normal)
            ])
        );
    }

    /// ...and is written back in the SAME short form, byte for byte.
    #[test]
    fn a_normal_priority_adds_nothing_to_the_line() {
        let text = irq_section(&map(&[(7, Edge::Rising, TaskPriority::Normal)]));
        assert!(text.contains("7=Rising\n"), "{text}");
        assert!(!text.contains(','), "no priority on a default line: {text}");
    }

    /// A raised priority survives the round trip.
    #[test]
    fn a_raised_priority_round_trips() {
        let want = map(&[
            (3, Edge::Falling, TaskPriority::Critical),
            (7, Edge::Rising, TaskPriority::Normal),
            (9, Edge::Both, TaskPriority::High),
        ]);
        assert_eq!(parse_irq(&irq_section(&want)), want);
    }

    /// A typo in the priority keeps the INTERRUPT and loses only the urgency.
    ///
    /// Dropping the line would silently disarm a pin the user armed - a far
    /// worse outcome than falling back to Normal.
    #[test]
    fn a_bad_priority_token_keeps_the_edge() {
        let got = parse_irq("@irq\n7=Rising,Urgent\n");
        assert_eq!(got, map(&[(7, Edge::Rising, TaskPriority::Normal)]));
    }

    /// A malformed EDGE is still skipped - there is no interrupt to keep.
    #[test]
    fn a_bad_edge_is_still_skipped() {
        assert!(parse_irq("@irq\n7=Sideways,High\n").is_empty());
    }

    /// No armed pins, no section - unchanged behaviour.
    #[test]
    fn nothing_armed_writes_no_section() {
        assert!(irq_section(&BTreeMap::new()).is_empty());
    }
}

#[cfg(test)]
mod module_notes_section_tests {
    use super::*;
    use crate::panels::mcu_module::modules::{ModuleKind, ModuleNotes};
    use std::collections::BTreeMap;

    fn one(text: &str, link: &str, image: &str) -> BTreeMap<(ModuleKind, u8), ModuleNotes> {
        let mut m = BTreeMap::new();
        m.insert(
            (ModuleKind::GenericInterfaceUsart, 1),
            ModuleNotes {
                text: text.into(),
                link: link.into(),
                image: image.into(),
            },
        );
        m
    }

    /// Everything a user can type survives, and each entry stays ONE line - a
    /// newline, a quote, a backslash, a tab, combining marks, and a line that
    /// looks exactly like a section header.
    #[test]
    fn notes_round_trip_with_newlines_quotes_and_at_signs() {
        let text = "GPS NEO-6M\n@modules\n\"9600\" \\ baud\tu\u{0308} done";
        let map = one(text, "https://x.example/neo.pdf", "docs/modules/neo.jpg");
        let s = notes_section(&map);
        let body = section_body(&s, NOTES_HEADER).unwrap();
        assert_eq!(body.lines().count(), 1, "one line per entry:\n{s}");
        assert!(body.trim_start().starts_with('('), "{body}");
        assert_eq!(parse_notes(&s), map);
    }

    /// A note containing `@labels` must not end its own section early and eat
    /// the real one that follows.
    #[test]
    fn notes_cannot_cut_the_next_section() {
        let mut labels = BTreeMap::new();
        labels.insert(12usize, "BOOT".to_owned());
        let file = format!(
            "{}\n{}",
            notes_section(&one("see @labels\n@labels", "", "")),
            labels_section(&labels)
        );
        assert_eq!(parse_labels(&file), labels);
        assert_eq!(parse_notes(&file).len(), 1);
    }

    #[test]
    fn a_project_without_notes_writes_no_section() {
        assert_eq!(notes_section(&BTreeMap::new()), "");
        assert_eq!(
            notes_section(&one("  ", " ", "")),
            "",
            "whitespace is empty"
        );
    }

    #[test]
    fn one_bad_line_loses_only_itself() {
        let good = notes_section(&one("kept", "", ""));
        let file = format!("{good}(kind: NotAKind, instance: 2, text: \"x\")\n");
        let got = parse_notes(&file);
        assert_eq!(got.len(), 1);
        assert_eq!(got[&(ModuleKind::GenericInterfaceUsart, 1)].text, "kept");
    }

    /// A file written by a newer IDE, with a field this one does not know.
    #[test]
    fn an_unknown_field_is_ignored() {
        let file = "@modulenotes\n(kind: GenericInterfaceSpi, instance: 3, text: \"t\", colour: \"red\")\n";
        let got = parse_notes(file);
        assert_eq!(got[&(ModuleKind::GenericInterfaceSpi, 3)].text, "t");
    }
}
