//! One place that knows what a tagged build/tool failure MEANS, and one card
//! that renders it.
//!
//! Background jobs (`build`, `profile`, …) prefix a fatal message with a
//! `[TAG]` when the cause is a known, actionable environment problem rather
//! than the user's code:
//!
//! ```text
//! [MSVC_LIBS] The MSVC toolchain can't find its C-runtime libraries …
//! ```
//!
//! Before this module every tab stripped its own prefix by hand and rendered
//! its own box — so a new tag (e.g. `[MSVC_LIBS]`) showed up as raw text with
//! the marker still in it, and nothing offered a way out. Now the mapping tag →
//! (headline, responsible tool, guidance) lives here, and [`show_card`] renders
//! the same banner everywhere, with an **Open Tools** button when a catalog
//! entry can fix it (see [`crate::required_tools`]).

use eframe::egui;
use egui_phosphor::regular as ph;

/// What a `[TAG]` means and who can fix it.
pub struct Hint {
    /// Marker at the very start of the message, e.g. `[MSVC_LIBS]`.
    pub tag: &'static str,
    /// Short headline for the card.
    pub title: &'static str,
    /// Matching [`crate::required_tools`] entry name → offer "Open Tools".
    /// `None` for problems no tool install can fix (e.g. a full disk).
    pub tool: Option<&'static str>,
}

pub const HINTS: &[Hint] = &[
    Hint {
        tag: "[MSVC_LIBS]",
        title: "MSVC build tools missing or incomplete",
        tool: Some("MSVC build tools"),
    },
    Hint {
        tag: "[CLIPPY_MISSING]",
        title: "Clippy isn't installed for this toolchain",
        tool: None, // installed with `rustup component add clippy`, not a catalog row
    },
    Hint {
        tag: "[BLOAT_MISSING]",
        title: "cargo-bloat isn't installed",
        tool: Some("cargo-bloat"),
    },
    Hint {
        tag: "[DISK_FULL]",
        title: "Disk full",
        tool: None, // freeing space is the caller's own action
    },
    Hint {
        tag: "[FLASH_FULL]",
        title: "Firmware doesn't fit in the chip's memory",
        tool: None, // nothing to install — the binary has to get smaller
    },
    Hint {
        tag: "[PROBE_RS_PANIC]",
        title: "probe-rs crashed (upstream bug)",
        tool: Some("probe-rs"), // the catalog row reinstalls / updates it
    },
    Hint {
        tag: "[PROBE_OPEN_FAILED]",
        title: "The debug probe could not be opened",
        tool: Some("probe-rs"), // a probe-rs version change is one of the fixes
    },
    Hint {
        tag: "[LAUNCH_STALLED]",
        title: "The debug session never started",
        tool: None, // nothing to install — it is a build/probe situation
    },
    Hint {
        tag: "[STALE_OUT_DIR]",
        title: "A dependency's generated file is missing",
        tool: None, // nothing to install — one package's fingerprint has to go
    },
];

/// The tagged message for a `launch` that the adapter never answered. `elf_mb`
/// is the size of the binary handed to it, because that is usually the whole
/// story: probe-rs flashes the image AND parses its debug info here, and a
/// debug-friendly build makes both much bigger.
pub fn launch_stalled_message(waited_s: u64, elf_mb: f64) -> String {
    format!(
        "[LAUNCH_STALLED] probe-rs accepted the connection but has not answered `launch` in \
         {waited_s}s.\n\n\
         That step flashes the chip and loads the ELF's debug info ({elf_mb:.1} MB here); it \
         reports nothing while it works, so a stall and a slow start look identical from the \
         outside.\n\n\
         -> Turn \"Debug-friendly build\" OFF in the toolbar: opt-level = 1 makes both the \
         image and its debug info much bigger, which is the usual cause of a very long launch.\n\
         -> Check the probe still answers: Scan in the probe selector (`probe-rs list`).\n\
         -> Unplug and replug the probe, then Debug again — a wedged ST-Link accepts the \
         connection and then goes quiet.\n\
         -> Make sure nothing else holds it (a second IDE, STM32CubeProgrammer, OpenOCD)."
    )
}

/// probe-rs failing to OPEN the probe (as opposed to not finding one), with the
/// innermost cause it reported — `None` when the output holds no such failure.
///
/// Worth telling apart from every other probe error: enumeration works, the
/// cable is fine, and the firmware is irrelevant — something between probe-rs
/// and the USB driver refuses. The user sees only `[dap] cancelled` otherwise.
pub fn probe_open_failure(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let i = lines.iter().position(|l| {
        l.contains("Failed to open the debug probe") || l.contains("Failed to open probe")
    })?;
    // The cause chain is printed under it, most specific LAST; quotes and
    // `N:` numbering are probe-rs's own formatting.
    let cause = lines[i + 1..]
        .iter()
        .take(8)
        .map(|l| {
            l.trim_start_matches(|c: char| c.is_ascii_digit() || c == ':')
                .trim()
        })
        .map(|l| l.trim_matches('"'))
        .filter(|l| {
            !l.is_empty() && !l.starts_with("Caused by") && !l.starts_with("Stack backtrace")
        })
        .next_back()
        .unwrap_or("");
    Some(if cause.is_empty() {
        "Failed to open the debug probe".to_owned()
    } else {
        format!("Failed to open the debug probe — {cause}")
    })
}

/// The tagged message for a probe that enumerates but won't open. Two cases are
/// called out by name because they are deterministic (every attach fails the
/// same way) and their fixes are not guessable from the error text: the WinUSB
/// reset, and - via `no_guid`, which the caller establishes from the registry
/// (`probe::missing_device_interface_guid`) - an interface with no
/// device-interface GUID. Everything else gets the list of usual suspects.
pub fn probe_open_message(detail: &str, no_guid: bool) -> String {
    let winusb = detail.contains("reset not supported by WinUSB");
    let mut s = format!("[PROBE_OPEN_FAILED] {detail}\n\n");
    if winusb {
        s.push_str(
            "probe-rs asks the USB stack to reset the probe when it opens it, and the WinUSB \
             driver bound to your ST-Link does not support that operation. `probe-rs list` still \
             works (enumeration doesn't open anything) — every attach fails.\n\n\
             -> Install a probe-rs release from before that behaviour:\n   \
             cargo install probe-rs-tools --locked --version 0.29.0\n\
             -> Or rebind the probe to the libusbK driver with Zadig (it does support reset). \
             That can upset STM32CubeProgrammer / ST-Link Utility, and is undone from Device \
             Manager -> Update driver.\n\n\
             Nothing is wrong with your firmware, wiring or the Debug-friendly build.",
        );
    } else if no_guid {
        s.push_str(
            "Windows registered this probe's USB interface WITHOUT a device-interface GUID, so \
             probe-rs has no device path to open - that is checked in the registry, under \
             Enum\\USB, not guessed from the error. Listing the probe still works, because \
             enumeration never opens anything; that is why it is in the Probe list. Nothing is \
             holding it.\n\n\
             -> Reinstall its driver with Zadig (zadig.akeo.ie): Options -> List All Devices, \
             pick the probe's DEBUG interface - on an ESP built-in JTAG that is \"USB \
             JTAG/serial debug unit (Interface 2)\" - and install WinUSB. Zadig's driver package \
             writes the GUID; the binding Windows made on its own did not.\n\
             -> The serial port on the same cable is a different interface and is left alone by \
             that, so flashing and the monitor over USB-Serial keep working.\n\n\
             Until it is registered, RTT, Debug and Profile-Runtime all fail here the same way - \
             none of them can open the probe.",
        );
    } else {
        s.push_str(
            "The probe was found but could not be opened. Usual causes:\n\
             -> Another program is holding it — a Debug or RTT session in this IDE, \
             STM32CubeProgrammer / ST-Link Utility, OpenOCD, or a leftover probe-rs process.\n\
             -> The USB driver bound to it can't do what probe-rs asks (see Device Manager).\n\
             -> The probe is wedged: unplug it and plug it back in.",
        );
    }
    s
}

/// The `panicked at <where>` line of a probe-rs crash, plus the message under
/// it — `None` when the output holds no panic.
///
/// probe-rs runs as a subprocess (dap-server, `run`, `list`), so a panic inside
/// it reaches us only as text: the process dies and the socket/pipe closes,
/// which on its own looks like an ordinary end of session.
pub fn probe_rs_panic(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let i = lines.iter().position(|l| l.contains("panicked at"))?;
    // `panicked at <path>:<line>:<col>:` then the message on the NEXT line.
    let where_ = lines[i]
        .split_once("panicked at ")
        .map_or(lines[i], |(_, rest)| rest);
    // Keep the crate@version + file, drop the local registry path noise.
    let where_ = where_
        .rsplit_once("index.crates.io-")
        .map_or(where_, |(_, rest)| {
            rest.split_once('\\').map_or(rest, |(_, r)| r)
        });
    let what = lines.get(i + 1).copied().unwrap_or_default();
    Some(if what.is_empty() || what.starts_with("stack backtrace") {
        where_.to_owned()
    } else {
        format!("{where_}\n  {what}")
    })
}

/// The tagged message for a probe-rs crash. The point is to say plainly that
/// this is probe-rs failing, not the user's firmware or this IDE — the console
/// alone shows a backtrace of `<unknown>` frames that reads like a local bug.
pub fn probe_rs_panic_message(detail: &str) -> String {
    format!(
        "[PROBE_RS_PANIC] probe-rs itself panicked and its process died — the session \
         ended with it:\n  {detail}\n\n\
         Nothing is wrong with your firmware or your probe wiring: this is a bug inside \
         probe-rs, usually hit while it enumerates USB devices looking for probes.\n\n\
         -> Update it:  cargo install probe-rs-tools --locked  (Open Tools runs this)\n\
         -> If the newest release still crashes, pin an older one:\n   \
         cargo install probe-rs-tools --locked --version 0.29.0\n\
         -> Meanwhile, unplug other dev boards / USB adapters and retry — enumeration \
         touches every candidate device.\n\n\
         Scan in the probe selector runs the same enumeration, so it is the quickest \
         way to tell whether a version change helped."
    )
}

/// `rust-lld`'s linker-script overflow, as one compact line — `None` when the
/// output holds none.
///
/// The raw form is one line per section, all overflowing by roughly the same
/// amount, e.g.:
/// ```text
/// rust-lld: error: section '.text' will not fit in region 'FLASH': overflowed by 11472 bytes
/// ```
/// Only the FIRST is kept: the others are the same shortfall counted again, and
/// a four-line dump buries the number that matters.
pub fn flash_overflow(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| l.contains("will not fit in region"))
        .map(|l| {
            // Drop the tool prefix ("rust-lld: error: ") — the card says it.
            l.rsplit_once("error: ")
                .map_or(l, |(_, rest)| rest)
                .to_owned()
        })
}

/// The tagged message for a link that overflowed the chip's memory. One place
/// composes it so the Cargo tab and the RTT/Debug builds say the same thing.
pub fn flash_full_message(detail: &str) -> String {
    format!(
        "[FLASH_FULL] The linker couldn't fit the firmware into the memory declared in \
         memory.x:\n  {detail}\n\n\
         The build itself is fine — the binary is simply too big for this part.\n\n\
         -> If \"Debug-friendly build\" is ON in the Debug tab, turn it OFF: it relaxes \
         [profile.release] to opt-level = 1, which costs several KB. That is the usual \
         cause when a project that used to link suddenly doesn't.\n\
         -> If the Configuration tab's flash store is on (STM32), its pages - and \
         on an F2/F4/F7 the rest of the vector table's sector - are not the \
         program's: fewer pages leave it more room.\n\
         -> Otherwise: drop features or dependencies, keep lto = true and \
         opt-level = \"s\"/\"z\", or move to a part with more Flash.\n\n\
         The Size button (Cargo / Flash tab) shows what is actually using the space."
    )
}

/// The package whose build script left nothing in `OUT_DIR`, from a rustc
/// `couldn't read` diagnostic — `None` when the output holds no such failure.
///
/// A build script writes generated code into `OUT_DIR` and its crate pulls it
/// back in with `include!(concat!(env!("OUT_DIR"), …))`. Delete that one file
/// and the build is stuck for good, because cargo fingerprints a build script's
/// INPUTS and never its outputs: the script still counts as fresh, so it is
/// never re-run, and every Build and every Clippy fails identically until the
/// fingerprint is cleared by hand.
///
/// Worth naming rather than leaving in the diagnostic list, because the line it
/// points at lives inside a registry crate — it reads as "that dependency is
/// broken" or "my code is broken", and neither is true. The path is the only
/// thing that names the culprit, and cargo's layout puts it there verbatim:
/// `…/build/<pkg>-<hash>/out/<file>`.
pub fn stale_out_dir(text: &str) -> Option<String> {
    text.lines()
        .filter(|l| l.contains("couldn't read"))
        .find_map(package_from_out_dir_path)
}

/// `<pkg>` out of a `…/build/<pkg>-<hash>/out/…` path anywhere in `line`.
///
/// Separators are MIXED in the real message and matching both forms is hopeless
/// without normalising first: the `OUT_DIR` half arrives with the platform's own
/// (`\` on Windows) while the `concat!` half is always `/`, so one path reads
/// `…\build\serde_core-f9656f87a1d5476f\out/private.rs`.
fn package_from_out_dir_path(line: &str) -> Option<String> {
    let norm = line.replace('\\', "/");
    // The LAST `/build/`: a project living under a directory of that name would
    // otherwise win over cargo's own, which is always the deeper one.
    let (_, after) = norm.rsplit_once("/build/")?;
    let (dir, _) = after.split_once("/out/")?;
    // Package names carry hyphens (`proc-macro2`) and the hash never does, so
    // only the LAST segment may be cut off.
    let (name, hash) = dir.rsplit_once('-')?;
    let hashish = hash.len() >= 8 && hash.bytes().all(|b| b.is_ascii_hexdigit());
    (!name.is_empty() && hashish).then(|| name.to_owned())
}

/// The tagged message for a build script whose `OUT_DIR` lost its generated
/// file. `pkg` is spelled exactly as `cargo clean -p` wants it.
pub fn stale_out_dir_message(pkg: &str) -> String {
    format!(
        "[STALE_OUT_DIR] `{pkg}` can't find the file its own build script generated.\n\n\
         The script writes that file into OUT_DIR and the crate includes it back with \
         `include!(concat!(env!(\"OUT_DIR\"), …))`. It is gone — but cargo still counts the \
         script as fresh, because a build script is fingerprinted by its INPUTS and never by \
         what it produced. So cargo will not re-run it on its own, and every Build and every \
         Clippy fails this same way until that one fingerprint is thrown away.\n\n\
         This is neither your code nor a broken dependency: something outside cargo deleted \
         files under the build workspace. A temp-file cleaner (Windows Storage Sense, Disk \
         Cleanup), an antivirus, or a build killed mid-write all do it.\n\n\
         -> Click \"Clean {pkg}\" below, then Build again. Only that package is rebuilt.\n\
         -> The same by hand:  cargo clean -p {pkg}"
    )
}

/// The package name back out of a [`stale_out_dir_message`] — what the card's
/// recovery button hands to `cargo clean -p`.
///
/// Read back from the composed message rather than carried beside it: the
/// message is the whole of what crosses into [`crate::build::BuildState`]`::Failed`,
/// and a second channel for one string is a second thing to keep in step.
pub fn stale_out_dir_package(msg: &str) -> Option<&str> {
    let (_, rest) = msg.split_once("cargo clean -p ")?;
    let pkg = rest.split_whitespace().next()?;
    (!pkg.is_empty()).then_some(pkg)
}

/// The hint for `msg` plus the message with its marker removed. Pure.
pub fn parse(msg: &str) -> Option<(&'static Hint, &str)> {
    HINTS.iter().find_map(|h| {
        msg.strip_prefix(h.tag)
            .map(|rest| (h, rest.trim_start_matches(' ')))
    })
}

/// `msg` without a leading `[TAG] ` (unchanged when it carries none) — for the
/// one-line status badges that have no room for a card.
pub fn strip(msg: &str) -> &str {
    parse(msg).map(|(_, rest)| rest).unwrap_or(msg)
}

/// egui id used to ask the app to switch to the Tools tab. A temp-data flag
/// avoids threading an out-param through `show_diag_panel` and every tab;
/// [`take_open_tools_request`] consumes it once per frame.
fn open_tools_id() -> egui::Id {
    egui::Id::new("failure_hint_open_tools")
}

/// True once after a card's "Open Tools" was clicked (clears the request).
pub fn take_open_tools_request(ctx: &egui::Context) -> bool {
    ctx.data_mut(|d| d.remove_temp::<bool>(open_tools_id()).unwrap_or(false))
}

/// Render the explanation card for a tagged failure. Returns `false` (drawing
/// nothing) when `msg` carries no known tag, so callers can fall back to their
/// plain error view. `extra` adds tab-specific actions next to the standard
/// buttons (e.g. Cargo's "Clean target/").
pub fn show_card(ui: &mut egui::Ui, msg: &str, extra: impl FnOnce(&mut egui::Ui)) -> bool {
    let Some((hint, body)) = parse(msg) else {
        return false;
    };
    egui::Frame::new()
        .fill(egui::Color32::from_rgb(60, 45, 10))
        .inner_margin(egui::Margin::same(8))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} {}", ph::WARNING, hint.title))
                        .size(12.0)
                        .strong()
                        .color(egui::Color32::from_rgb(250, 190, 60)),
                );
            });
            ui.add_space(4.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(body)
                        .size(10.5)
                        .color(egui::Color32::from_rgb(215, 200, 165)),
                )
                .wrap(),
            );
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if hint.tool.is_some()
                    && ui
                        .button(
                            egui::RichText::new(format!("{} Open Tools", ph::WRENCH))
                                .size(11.0)
                                .color(egui::Color32::from_rgb(255, 210, 80)),
                        )
                        .on_hover_text("Check / install this dependency")
                        .clicked()
                {
                    ui.ctx().data_mut(|d| d.insert_temp(open_tools_id(), true));
                }
                extra(ui);
            });
        });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_tags_and_strips_marker() {
        let (h, rest) = parse("[MSVC_LIBS] libs are gone").unwrap();
        assert_eq!(h.title, "MSVC build tools missing or incomplete");
        assert_eq!(rest, "libs are gone");
        assert_eq!(strip("[DISK_FULL] no space"), "no space");
    }

    /// The real diagnostic, kept verbatim — mixed separators and all. The `\`
    /// of `OUT_DIR` meeting the `/` of `concat!` is exactly what a naive
    /// `contains("/out/")` misses on Windows.
    const LOST_OUT_DIR: &str = "error: couldn't read `C:\\Users\\istra\\AppData\\Local\\Temp\\embedded_ide_0_check\\target\\debug\\build\\serde_core-f9656f87a1d5476f\\out/private.rs`: The system cannot find the file specified. (os error 2)\n   --> C:\\Users\\istra\\.cargo\\registry\\src\\index.crates.io-1949cf8c6b5b557f\\serde_core-1.0.229\\src\\crate_root.rs:165:9";

    #[test]
    fn finds_the_package_that_lost_its_out_dir() {
        assert_eq!(stale_out_dir(LOST_OUT_DIR).as_deref(), Some("serde_core"));
    }

    /// Package names contain hyphens; the hash does not. Cutting at the FIRST
    /// one would hand `cargo clean -p` the string "proc", which is not a
    /// package and fails with no useful message.
    #[test]
    fn hyphenated_package_survives_the_hash_split() {
        let line = "error: couldn't read `/tmp/w/target/debug/build/proc-macro2-4b2f539c19a5d13e/out/probe.rs`: No such file";
        assert_eq!(stale_out_dir(line).as_deref(), Some("proc-macro2"));
    }

    /// A project that happens to live under a `build/` directory must not
    /// shadow cargo's own, which is always deeper.
    #[test]
    fn the_deepest_build_directory_wins() {
        let line = "error: couldn't read `C:\\build\\myproj\\target\\debug\\build\\ring-0123456789abcdef\\out/x.rs`: nope";
        assert_eq!(stale_out_dir(line).as_deref(), Some("ring"));
    }

    /// Not every `couldn't read` is this. A missing file of the user's own has
    /// no `build/<pkg>-<hash>/out/` shape, and claiming it does would offer a
    /// `cargo clean -p` for a package that does not exist.
    #[test]
    fn an_ordinary_missing_file_is_not_a_stale_out_dir() {
        assert!(stale_out_dir("error: couldn't read `src/pins/configs/uart1.rs`: nope").is_none());
        assert!(stale_out_dir("error[E0425]: cannot find value `x`").is_none());
        // Right shape, but the tail is not a hash — so not cargo's layout.
        assert!(
            stale_out_dir("error: couldn't read `/w/target/debug/build/my-crate/out/g.rs`: nope")
                .is_none()
        );
    }

    /// The button reads the package back out of the message it renders, so the
    /// two must round-trip. Written as one test because separately they can
    /// both pass while disagreeing.
    #[test]
    fn message_round_trips_the_package_name() {
        for pkg in ["serde_core", "proc-macro2", "ring"] {
            let msg = stale_out_dir_message(pkg);
            assert!(msg.starts_with("[STALE_OUT_DIR]"), "{msg}");
            assert_eq!(stale_out_dir_package(&msg), Some(pkg));
            // The copy-paste line has to survive rustfmt joining the literal.
            assert!(msg.contains(&format!("cargo clean -p {pkg}")), "{msg}");
            // And the card must know the tag, or it renders as raw text.
            assert!(parse(&msg).is_some(), "{pkg} tag missing from HINTS");
        }
        assert!(stale_out_dir_package("[DISK_FULL] no space").is_none());
    }

    #[test]
    fn untagged_message_is_untouched() {
        assert!(parse("error[E0425]: not found").is_none());
        assert_eq!(strip("error[E0425]: not found"), "error[E0425]: not found");
        // A tag must be at the START to count.
        assert!(parse("see [DISK_FULL] above").is_none());
    }

    /// Every tag a background job can emit must be in the table — otherwise it
    /// reaches the user as raw text with the marker still in it (the bug this
    /// module fixes for `[MSVC_LIBS]`).
    #[test]
    fn table_covers_every_emitted_tag() {
        for tag in [
            "[MSVC_LIBS]",
            "[CLIPPY_MISSING]",
            "[BLOAT_MISSING]",
            "[DISK_FULL]",
            "[FLASH_FULL]",
            "[PROBE_RS_PANIC]",
            "[PROBE_OPEN_FAILED]",
            "[LAUNCH_STALLED]",
        ] {
            assert!(
                HINTS.iter().any(|h| h.tag == tag),
                "{tag} has no hint entry"
            );
        }
    }

    /// The linker dump repeats the same shortfall once per section — the card
    /// gets the first line, without the tool prefix, and the composed message
    /// stays parseable as a tagged hint.
    #[test]
    fn flash_overflow_is_summarised_to_one_line() {
        let dump = "  = note: rust-lld: error: section '.text' will not fit in region \
                    'FLASH': overflowed by 11472 bytes\n\
                    rust-lld: error: section '.rodata' will not fit in region 'FLASH': \
                    overflowed by 26620 bytes\n";
        let detail = flash_overflow(dump).expect("detected");
        assert_eq!(
            detail,
            "section '.text' will not fit in region 'FLASH': overflowed by 11472 bytes"
        );
        // The composed message is a real tagged hint, and keeps the numbers.
        let msg = flash_full_message(&detail);
        let (hint, body) = parse(&msg).expect("tagged");
        assert_eq!(hint.tag, "[FLASH_FULL]");
        assert!(body.contains("11472 bytes"), "{body}");
        assert!(body.contains("Debug-friendly build"), "{body}");
        // An ordinary compile error is not mistaken for one.
        assert!(flash_overflow("error[E0425]: cannot find value `x`").is_none());
    }

    /// A probe-rs crash reaches us as console text; the card needs the crate +
    /// file and the message, without the user's cargo-registry path.
    #[test]
    fn probe_rs_panic_keeps_crate_file_and_message() {
        let out = "probe-rs-debug: Starting debug session from: 127.0.0.1:54531\n\
                   thread 'main' (20992) panicked at C:\\Users\\me\\.cargo\\registry\\src\\\
                   index.crates.io-1949cf8c6b5b557f\\probe-rs-0.31.0\\src\\probe\\glasgow\\\
                   mux.rs:97:13:\n\
                   internal error: entered unreachable code\n\
                   stack backtrace:\n   0:     0x7ff699f13a82 - <unknown>\n";
        let detail = probe_rs_panic(out).expect("detected");
        assert!(detail.starts_with("probe-rs-0.31.0"), "{detail}");
        assert!(detail.contains("mux.rs:97:13"), "{detail}");
        assert!(detail.contains("entered unreachable code"), "{detail}");
        assert!(
            !detail.contains("index.crates.io"),
            "registry noise:\n{detail}"
        );

        let msg = probe_rs_panic_message(&detail);
        let (hint, body) = parse(&msg).expect("tagged");
        assert_eq!(hint.tool, Some("probe-rs"), "the card offers Open Tools");
        assert!(body.contains("probe-rs-tools --locked"), "{body}");
        // Ordinary probe-rs chatter is not a crash.
        assert!(probe_rs_panic("probe-rs-debug: Listening on port 54529").is_none());
    }

    /// The WinUSB open failure: the innermost cause is what identifies it, and
    /// the card must name the two fixes — neither is guessable from the text
    /// probe-rs prints.
    #[test]
    fn probe_open_failure_keeps_the_innermost_cause() {
        let out = "probe-rs-debug: Starting debug session from: 127.0.0.1:55402\n\
                   Failed to open the debug probe.\n\
                   \t\"An error which is specific to the debug probe in use occurred.\"\n\
                   \t\t\"USB error.\"\n\
                   \t\t\t\"reset not supported by WinUSB\"\n";
        let detail = probe_open_failure(out).expect("detected");
        assert!(detail.contains("reset not supported by WinUSB"), "{detail}");

        let msg = probe_open_message(&detail, false);
        let (hint, body) = parse(&msg).expect("tagged");
        assert_eq!(hint.tag, "[PROBE_OPEN_FAILED]");
        assert!(body.contains("--version 0.29.0"), "{body}");
        assert!(body.contains("libusbK"), "{body}");

        // A different open failure still gets the generic checklist.
        let other = probe_open_failure("Failed to open probe: device busy").expect("detected");
        let generic = probe_open_message(&other, false);
        assert!(
            generic.contains("Another program is holding it"),
            "{generic}"
        );
        assert!(!generic.contains("libusbK"), "{generic}");

        // "no probe found" is a different problem and must not match.
        assert!(probe_open_failure("Error: no debug probe was found").is_none());
    }

    /// A probe whose USB interface was registered without a device-interface
    /// GUID is NOT busy: the generic checklist's first line ("another program
    /// is holding it") is advice that can never work here, and the one thing
    /// that does work - reinstalling the driver - is on no list the user can
    /// guess. The caller establishes the fact from the registry; this only has
    /// to keep the two cards apart.
    #[test]
    fn a_probe_with_no_device_interface_guid_gets_its_own_card() {
        // probe-rs's own shape on an ESP32-C3 whose JTAG interface has no GUID.
        let out = "Failed to open the debug probe.\n\
                   Caused by:\n\
                   0: The debug probe could not be created.\n\
                   1: The selected USB device could not be opened.\n";
        let detail = probe_open_failure(out).expect("detected");

        let msg = probe_open_message(&detail, true);
        let (hint, body) = parse(&msg).expect("tagged");
        assert_eq!(hint.tag, "[PROBE_OPEN_FAILED]");
        assert!(body.contains("Zadig"), "{body}");
        assert!(
            body.contains("Interface 2"),
            "names the ESP interface: {body}"
        );
        assert!(
            !body.contains("Another program is holding it"),
            "the one cause that is ruled out: {body}"
        );

        // The SAME text without the registry finding keeps the old checklist —
        // the verdict comes from the caller, never from the error string.
        let generic = probe_open_message(&detail, false);
        assert!(
            generic.contains("Another program is holding it"),
            "{generic}"
        );
        assert!(!generic.contains("Zadig"), "{generic}");
    }

    /// A hint that names a tool must name one that actually exists in the
    /// catalog, or "Open Tools" would send the user to a row that isn't there.
    #[test]
    fn referenced_tools_exist_in_the_catalog() {
        let state = crate::required_tools::make_tools_state();
        let state = state.lock().unwrap();
        for h in HINTS.iter().filter_map(|h| h.tool.map(|t| (h.tag, t))) {
            let (tag, tool) = h;
            assert!(
                state.tools.iter().any(|t| t.name == tool),
                "{tag} points at unknown tool {tool:?}"
            );
        }
    }
}
