// A GUI app in EVERY profile: Windows never allocates a console for it, so no
// black window pops up at startup — debug included. Debug used to keep the
// console just to see `println!`; `build::attach_parent_console()` (called first
// thing in `main`) buys that back without the window, by adopting the console we
// were launched from when there is one.
//
// The other half: every child process we spawn is given CREATE_NO_WINDOW (see
// `build::no_window`) so none of them flash a console either. Both halves are
// needed — the subsystem alone makes children flash WORSE, since they no longer
// have a parent console to inherit.
#![windows_subsystem = "windows"]

use eframe::egui;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
pub mod activity;
pub mod app;
use app::AppIde;

pub mod build;
pub mod debugger;
pub mod dfu;
pub mod editor;
pub mod egui_memory_prune;
pub mod esp_monitor;
pub mod espflash;
pub mod failure_hint;
pub mod flamegraph;
pub mod flash_stop;
pub mod git;
pub mod headless;
pub mod lsp;
pub mod msvc;
pub mod names;
pub mod net;
pub mod openocd;
pub mod panels;
pub mod probe;
pub mod probe_flash;
pub mod profile;
pub mod project_tree;
pub mod publish;
pub mod publish_target;
pub mod recent;
pub mod required_tools;
pub mod reveal;
pub mod rtt;
pub mod rust_lex;
pub mod serial;
pub mod serial_bridge;
pub mod serial_frames;
pub mod serial_matrix;
pub mod serial_plot;
pub mod size;
pub mod startup;
pub mod terminal;
pub mod udev;
pub mod win_driver;
pub mod workspace;

/// Guard against the tofu-square bug coming back.
///
/// The bundled font has no arrow glyphs, so a raw `→` / `⇄` in a string that
/// reaches egui renders as an empty box. It has now been introduced and fixed
/// three times, always the same way: someone reaches for the prettier character
/// while writing a label. A grep-shaped test is the only thing that catches it
/// before the user does — the alternative is noticing it in a screenshot.
///
/// Comments are free to use arrows; only string LITERALS are checked. Use ASCII
/// `->` and `<->`, or a phosphor `ph::` icon.
#[cfg(test)]
mod glyph_guard {
    /// The Unicode BLOCKS of arrows, none of which the bundled fonts cover.
    ///
    /// This was a list of seven specific characters, and that is exactly how it
    /// failed: a `↳` (U+21B3) went into the dependency banner, was not one of
    /// the seven, compiled, passed every test, and rendered as an empty box in
    /// the user's screenshot. A denylist of characters only ever bans the ones
    /// someone already reached for — the next person reaches for a different
    /// one. Ranges close that door: the arrow blocks are absent from
    /// Ubuntu-Light, Hack, NotoEmoji and phosphor alike, so no character in
    /// them can render and banning them wholesale has no false positives.
    ///
    /// Em dash, ellipsis and `×` are deliberately NOT covered — those do
    /// render, and none of them lives in these blocks.
    const BANNED_RANGES: [(char, char); 7] = [
        ('\u{2190}', '\u{21ff}'), // Arrows
        ('\u{27f0}', '\u{27ff}'), // Supplemental Arrows-A
        ('\u{2900}', '\u{297f}'), // Supplemental Arrows-B
        ('\u{2300}', '\u{23ff}'), // Miscellaneous Technical (⌘ ⏎ ⏱)
        ('\u{25a0}', '\u{25ff}'), // Geometric Shapes (▶ ▾ ▲ ● □)
        ('\u{2600}', '\u{26ff}'), // Miscellaneous Symbols (⚠ ⛔ ★)
        ('\u{2700}', '\u{27bf}'), // Dingbats (✔ ✗ ✕ ✘)
    ];

    /// Symbols inside the banned blocks that the bundled fonts DO carry in both
    /// the proportional and the monospace family, and that the code already
    /// uses. Kept short on purpose: the project's rule is phosphor icons in UI
    /// text, so this is an escape hatch, not a menu.
    const ALLOWED: [char; 1] = [
        '\u{25cb}', // ○ — the clock-graph port marker
    ];

    fn banned(c: char) -> bool {
        !ALLOWED.contains(&c) && BANNED_RANGES.iter().any(|(lo, hi)| c >= *lo && c <= *hi)
    }

    /// Does this line carry a banned glyph INSIDE a double-quoted literal?
    ///
    /// Walks the line tracking string state, so `// see "x" -> y` (a comment
    /// that merely contains quotes) is not flagged and `"a -> b" // note` is.
    /// A `//` reached outside a string ends the line.
    fn in_literal(line: &str) -> bool {
        let mut chars = line.chars().peekable();
        let mut in_str = false;
        while let Some(c) = chars.next() {
            match c {
                '\\' if in_str => {
                    chars.next(); // escaped char, whatever it is
                }
                '"' => in_str = !in_str,
                '/' if !in_str && chars.peek() == Some(&'/') => return false,
                c if in_str && banned(c) => return true,
                _ => {}
            }
        }
        false
    }

    fn scan(dir: &std::path::Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                scan(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    if in_literal(line) {
                        out.push(format!("{}:{}  {}", p.display(), i + 1, line.trim()));
                    }
                }
            }
        }
    }

    #[test]
    fn the_scanner_tells_comments_from_literals() {
        // Both the arrow this guard always knew about and the two that got past
        // it — `↳` into the dependency banner, `↗` into the diagnostics link —
        // so the widening is checked through the SCANNER, not just through
        // `banned()`. The two have to agree for the test to mean anything.
        for a in ['\u{2192}', '\u{21b3}', '\u{2197}'] {
            assert!(in_literal(&format!("ui.label(\"a {a} b\");")));
            assert!(in_literal(&format!("x(\"a {a} b\"); // fine")));
            // Comments are allowed to use arrows, even alongside quotes.
            assert!(!in_literal(&format!("// \"Restore\" {a} confirm")));
            assert!(!in_literal(&format!("/// maps `\"x\"` {a} text")));
            assert!(!in_literal(&format!("let x = 1; // re-open {a} promoted")));
            // A `//` inside a string is not a comment.
            assert!(in_literal(&format!("u(\"http://x {a} y\");")));
        }
        assert!(!in_literal("nothing here at all"));
        // The punctuation that renders must survive a literal untouched.
        assert!(!in_literal("ui.label(\"Saving — 3 files… 2×\");"));
    }

    /// The whole point of the widening: the character that got through.
    ///
    /// `↳` was not one of the seven this guard used to list, so it shipped to a
    /// screenshot. Each range is pinned at both ends too — an off-by-one there
    /// is invisible until it is a box on someone's screen.
    #[test]
    fn every_arrow_block_is_covered_not_just_the_ones_seen_before() {
        // The escapee.
        assert!(banned('\u{21b3}'), "the arrow that reached the user");
        // The original seven still count.
        for c in [
            '\u{2192}', '\u{2190}', '\u{2194}', '\u{21c4}', '\u{21c6}', '\u{21d2}', '\u{21bb}',
        ] {
            assert!(banned(c), "{c:?} was banned before and must stay banned");
        }
        // Both edges of each block.
        for (lo, hi) in BANNED_RANGES {
            assert!(banned(lo), "low edge {lo:?}");
            assert!(banned(hi), "high edge {hi:?}");
        }
        // …and the characters just outside them, which DO render.
        for c in [
            '\u{218f}', '\u{2200}', '\u{27ef}', '\u{2800}', '\u{28ff}', '\u{2980}',
        ] {
            assert!(!banned(c), "{c:?} is outside the arrow blocks");
        }
        // The punctuation this guard has always allowed on purpose.
        for c in ['—', '–', '…', '×', '·', '•', '°'] {
            assert!(!banned(c), "{c:?} renders and must not be flagged");
        }
    }

    /// The symbol blocks, added after the arrows.
    ///
    /// The four the user named are NOT one story, which is the point: reading
    /// the bundled fonts' cmap tables says `✗` is missing everywhere, `▾` is in
    /// Hack only — so it renders in a `.monospace()` label and is a box in the
    /// proportional ones that most UI uses — while `✔` and `▶` are carried by
    /// NotoEmoji and do render. The scanner cannot know which family a literal
    /// ends up in, so "renders in EVERY family or it is banned" is the only
    /// rule it can apply, and the phosphor-icons convention wants them gone
    /// regardless.
    #[test]
    fn the_symbol_blocks_are_covered_too() {
        // The four from the request, plus their neighbours.
        for c in [
            '✔', '✗', '▶', '▾', '✓', '✕', '✘', '▲', '▼', '●', '□', '★', '⚠',
        ] {
            assert!(banned(c), "{c:?} must not go into a UI literal");
        }
        // Block edges.
        for (lo, hi) in BANNED_RANGES {
            assert!(banned(lo), "low edge {lo:?}");
            assert!(banned(hi), "high edge {hi:?}");
        }
        // The escape hatch, and only it.
        assert!(!banned('\u{25cb}'), "the allow-list entry stays usable");
        assert_eq!(ALLOWED.len(), 1, "keep this short — phosphor is the rule");
        // Ordinary text is untouched by the new blocks.
        for c in ['a', 'Z', '9', 'ă', 'ș', '"', '\'', '\u{22ff}', '\u{2b00}'] {
            assert!(!banned(c), "{c:?} is not a symbol-block character");
        }
    }

    #[test]
    fn no_arrow_glyphs_in_string_literals() {
        let mut bad = Vec::new();
        scan(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut bad,
        );
        assert!(
            bad.is_empty(),
            "the bundled font has no arrow glyphs - these render as empty boxes.\n\
             Use ASCII (-> , <->) or a phosphor ph:: icon:\n{}",
            bad.join("\n")
        );
    }
}

/// A storage file's new text, from [`tidied_storage`].
struct TidiedStorage {
    text: String,
    /// Widget-state records dropped from the `"egui"` entry; 0 when only the
    /// window geometry went.
    pruned: usize,
}

/// Tidy a storage file's RON text: strip eframe's saved window geometry, and
/// drop the widget-state records no build will read again
/// (`egui_memory_prune`).
///
/// `None` means "leave the file alone": nothing to change, or the text did not
/// survive a parse → serialize → parse round-trip. That check is the point — the
/// file holds the ENTIRE persisted state (over a megabyte here), and a
/// replacement eframe could not read would lose all of it, so the only
/// acceptable way to touch it is to prove the replacement first. (Our `ron` and
/// eframe's are the same major since 2026-09; the check does not rely on it.)
/// Every other entry comes back as the same string, byte for byte.
fn tidied_storage(
    text: &str,
    live_types: impl FnOnce() -> Option<HashSet<u64>>,
) -> Option<TidiedStorage> {
    use std::collections::HashMap;
    // eframe's `STORAGE_WINDOW_KEY` and `STORAGE_EGUI_MEMORY_KEY`, private to
    // it — hence the literals.
    const WINDOW_KEY: &str = "window";
    const EGUI_KEY: &str = "egui";

    // No cheap gate any more: whether a record is dead is only known by
    // reading them all. Measured in the dev profile on the real slot-1 file:
    // ~17 ms to parse and scan the 1.5 MB it held, ~100 ms for the one launch
    // that pruned it (probe, copy and write included), 4-5 ms a launch for
    // the 397 KB left.
    let mut kv: HashMap<String, String> = ron::from_str(text).ok()?;
    let window = kv.remove(WINDOW_KEY).is_some();
    let pruned = kv
        .get(EGUI_KEY)
        .and_then(|memory| egui_memory_prune::prune(memory, live_types));
    if !window && pruned.is_none() {
        return None;
    }
    let pruned = pruned.map_or(0, |p| {
        kv.insert(EGUI_KEY.to_owned(), p.memory);
        p.removed
    });
    let out = ron::ser::to_string_pretty(&kv, ron::ser::PrettyConfig::default()).ok()?;
    (ron::from_str::<HashMap<String, String>>(&out).ok()? == kv)
        .then_some(TidiedStorage { text: out, pruned })
}

/// Tidy eframe's storage file before eframe reads it — see [`tidied_storage`].
///
/// The window geometry goes so this run can open maximized like it asks to.
/// `with_maximized(true)` on the viewport does NOT win on its own: a restored
/// geometry is applied after it (`WindowSettings::initialize_viewport_builder`
/// ends with `.with_maximized(self.maximized)`), so one session left small
/// reopened small ever after. `persist_window: false` stops new writes, but
/// eframe LOADS that entry regardless of the flag — an entry already on disk
/// would keep winning. This removes it, once.
///
/// The dead widget state goes because eframe never drops it and rewrites all
/// of it on every autosave: see `egui_memory_prune`.
///
/// Best-effort from end to end: a storage file we cannot read, parse or replace
/// is not worth failing a launch over, and the worst case is the old behaviour
/// (which the viewport command in `AppIde::new` then corrects a frame later).
/// The replace goes through a temp file and a rename so a crash mid-write
/// cannot truncate the user's state.
fn tidy_storage_file(app_name: &str) {
    if let Some(path) = eframe::storage_dir(app_name).map(|d| d.join("app.ron")) {
        tidy_storage_file_at(&path, egui_memory_prune::live_type_ids);
    }
}

/// [`tidy_storage_file`] on a path; `true` when the file was replaced.
///
/// The first rewrite that drops any records keeps the file it started from as
/// `app.ron.pre-prune` (see [`keep_pre_prune_copy`]), and when that copy cannot
/// be written nothing is.
fn tidy_storage_file_at(path: &Path, live_types: impl FnOnce() -> Option<HashSet<u64>>) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Some(tidied) = tidied_storage(&text, live_types) else {
        return false;
    };
    if tidied.pruned > 0 && !keep_pre_prune_copy(path, &text) {
        return false;
    }
    replace_through(&path.with_extension("ron.tmp"), &tidied.text, path)
}

/// Write `text` to `tmp`, then rename it over `dest`. A failed write or rename
/// takes its temp file with it, so no half-written copy lingers beside the
/// user's state until the next launch.
fn replace_through(tmp: &Path, text: &str, dest: &Path) -> bool {
    let done = std::fs::write(tmp, text).is_ok() && std::fs::rename(tmp, dest).is_ok();
    if !done {
        let _ = std::fs::remove_file(tmp);
    }
    done
}

/// Keep `app.ron.pre-prune`: the storage file exactly as it was before the
/// FIRST prune that removed anything. `false` only when it is missing and
/// cannot be written.
///
/// The prune removes most of a megabyte on its first run, from a file that
/// also holds the project pointer, and it goes by a rule; if the rule ever
/// takes a record a build still wanted, this is the way back (close the IDE,
/// copy it over `app.ron`). One copy, never refreshed: later prunes each drop
/// a little, and a rolling backup would only ever hold yesterday's layout. It
/// goes through a temp file, so a half-written copy never counts as the backup.
fn keep_pre_prune_copy(path: &Path, text: &str) -> bool {
    let backup = path.with_extension("ron.pre-prune");
    if backup.exists() {
        return true;
    }
    replace_through(&path.with_extension("ron.pre-prune.tmp"), text, &backup)
}

#[cfg(test)]
mod window_geometry_tests {
    use super::tidied_storage;

    /// The window-only cases: no record list in these values, so the prune
    /// never gets as far as asking for the live types.
    fn without_window_key(text: &str) -> Option<String> {
        let probe = || panic!("nothing here to prune");
        tidied_storage(text, probe).map(|t| t.text)
    }

    /// Shaped like eframe's real file: a map of strings whose VALUES are
    /// themselves RON, quotes and all — the part a naive text edit would break.
    fn storage() -> String {
        r#"{
    "egui": "(options:(theme_preference:Dark,zoom_factor:1.0))",
    "window": "(outer_position_pixels:Some((x:139.0,y:0.0)),maximized:false)",
    "embedded_ide_project_v1": "(user_src_files:[(\"src/app.rs\",\"fn main() {}\")])",
}"#
        .to_owned()
    }

    #[test]
    fn the_window_entry_goes_and_the_rest_survives_verbatim() {
        let out = without_window_key(&storage()).expect("the entry was there");
        assert!(!out.contains("\"window\""));
        // The other values must come back byte-identical — they are the user's
        // whole persisted state, and one of them contains escaped quotes.
        let kv: std::collections::HashMap<String, String> = ron::from_str(&out).unwrap();
        assert_eq!(kv.len(), 2);
        assert_eq!(
            kv["embedded_ide_project_v1"],
            r#"(user_src_files:[("src/app.rs","fn main() {}")])"#
        );
        assert!(kv["egui"].starts_with("(options:"));
    }

    #[test]
    fn a_file_without_the_entry_is_left_alone() {
        // Every launch after the first cleanup lands here — and must not rewrite
        // a megabyte of state to change nothing.
        assert_eq!(without_window_key(r#"{"egui": "(x:1)"}"#), None);
    }

    #[test]
    fn unparseable_storage_is_left_alone() {
        // Refusing to touch it is the whole safety story: the alternative is
        // truncating the user's state over a cosmetic startup detail.
        assert_eq!(without_window_key(r#"{"window": "(maxim"#), None);
    }
}

#[cfg(test)]
mod storage_tidy_tests {
    use super::{tidied_storage, tidy_storage_file_at};
    use crate::egui_memory_prune::live_type_ids;
    use eframe::egui;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    const PROJECT_V1: &str = r#"(user_src_files:[("src/app.rs","fn main() {}")])"#;
    const PROJECT_V2: &str = r#"(root:Some("F:\\work\\blinky"))"#;

    /// The `"egui"` value of a session that showed a resizable panel and a
    /// collapsing header, as eframe saves it.
    fn live_memory() -> String {
        let ctx = egui::Context::default();
        crate::headless::run_ui(&ctx, egui::RawInput::default(), |ui| {
            egui::Panel::left("code_editor")
                .resizable(true)
                .show(ui, |_| {});
            egui::CollapsingHeader::new("src").show(ui, |_| {});
        });
        ctx.memory(ron::ser::to_string).unwrap()
    }

    /// `memory` with three records of a build that last ran `age` launches
    /// ago spliced in front.
    fn with_dead(memory: &str, type_id: u64, age: usize) -> String {
        let dead: Vec<String> = (0..3)
            .map(|k| {
                let generation = age + 1 + k;
                format!("({k},(type_id:({type_id}),ron:\"(open:true)\",generation:{generation}))")
            })
            .collect();
        memory.replacen("data:([", &format!("data:([{},", dead.join(",")), 1)
    }

    /// A storage file in eframe's shape: `save_to_disk` writes the map with
    /// `to_io_writer_pretty` and the default config.
    fn storage(egui: &str) -> String {
        let kv = HashMap::from([
            ("egui".to_owned(), egui.to_owned()),
            ("embedded_ide_project_v1".to_owned(), PROJECT_V1.to_owned()),
            ("embedded_ide_project_v2".to_owned(), PROJECT_V2.to_owned()),
        ]);
        ron::Options::default()
            .to_string_pretty(&kv, ron::ser::PrettyConfig::default())
            .unwrap()
    }

    fn entries(text: &str) -> HashMap<String, String> {
        ron::from_str(text).unwrap()
    }

    /// A fresh scratch folder holding only an `app.ron`.
    fn scratch(name: &str, bytes: &[u8]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("eide_tidy_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app.ron");
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn files_beside(path: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn dead_records_go_and_every_other_entry_is_the_same_string() {
        let live = live_memory();
        let before = storage(&with_dead(&live, 0xDEAD_0001, 57));
        let tidied = tidied_storage(&before, live_type_ids).expect("a dead build to drop");
        assert_eq!(tidied.pruned, 3);
        let kv = entries(&tidied.text);
        assert_eq!(kv.len(), 3);
        assert_eq!(kv["egui"], live, "the live Memory, byte for byte");
        assert_eq!(kv["embedded_ide_project_v1"], PROJECT_V1);
        assert_eq!(kv["embedded_ide_project_v2"], PROJECT_V2);
    }

    #[test]
    fn the_first_prune_keeps_the_file_it_started_from_and_only_that() {
        let live = live_memory();
        let first = storage(&with_dead(&live, 0xDEAD_0001, 57));
        let path = scratch("backup", first.as_bytes());
        assert!(tidy_storage_file_at(&path, live_type_ids));
        let backup = path.with_extension("ron.pre-prune");
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), first);
        assert_eq!(
            entries(&std::fs::read_to_string(&path).unwrap())["egui"],
            live
        );

        // A later prune leaves that copy as it was.
        std::fs::write(&path, storage(&with_dead(&live, 0xDEAD_0002, 300))).unwrap();
        assert!(tidy_storage_file_at(&path, live_type_ids));
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), first);
        assert_eq!(
            files_beside(&path),
            ["app.ron", "app.ron.pre-prune"],
            "no temp file left"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let live = live_memory();
        let path = scratch(
            "twice",
            storage(&with_dead(&live, 0xDEAD_0001, 57)).as_bytes(),
        );
        assert!(tidy_storage_file_at(&path, live_type_ids));
        let once = std::fs::read(&path).unwrap();
        assert!(!tidy_storage_file_at(&path, live_type_ids));
        assert_eq!(std::fs::read(&path).unwrap(), once);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_file_it_cannot_read_is_left_untouched() {
        let live = live_memory();
        let whole = storage(&with_dead(&live, 0xDEAD_0001, 57));
        // A valid storage map whose Memory lost one closing parenthesis.
        let bad_memory = whole.replacen(",generation:58))", ",generation:58)", 1);
        assert_ne!(bad_memory, whole);
        for (name, bytes) in [
            ("garbage", b"garbage \xff\xfe not ron".to_vec()),
            ("truncated", whole.as_bytes()[..whole.len() / 2].to_vec()),
            ("bad_memory", bad_memory.into_bytes()),
        ] {
            let path = scratch(name, &bytes);
            assert!(!tidy_storage_file_at(&path, live_type_ids), "{name}");
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name}");
            assert_eq!(
                files_beside(&path),
                ["app.ron"],
                "{name}: no copy, no temp file"
            );
            let _ = std::fs::remove_dir_all(path.parent().unwrap());
        }
    }

    #[test]
    fn without_its_copy_the_prune_does_not_happen() {
        let live = live_memory();
        let before = storage(&with_dead(&live, 0xDEAD_0001, 57));
        let path = scratch("nocopy", before.as_bytes());
        // A folder where the copy's temp file goes: the copy cannot be written.
        let blocker = path.with_extension("ron.pre-prune.tmp");
        std::fs::create_dir(&blocker).unwrap();
        assert!(!tidy_storage_file_at(&path, live_type_ids));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // The control: the same file, once the copy can be written, is pruned
        // - and the copy is the file as it was.
        std::fs::remove_dir(&blocker).unwrap();
        assert!(tidy_storage_file_at(&path, live_type_ids));
        assert_ne!(std::fs::read_to_string(&path).unwrap(), before);
        assert_eq!(
            std::fs::read_to_string(path.with_extension("ron.pre-prune")).unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

/// The project folder asked for on the command line, if any.
///
/// Accepts `rust_on_chip <folder>` and `rust_on_chip --project <folder>`.
/// The bare form is what makes a per-project Windows shortcut, a drag of a
/// folder onto the exe, and "Open with" all work without extra syntax.
///
/// Which project a window opens used to be decided by LAUNCH ORDER — the slot
/// picked the eframe storage, and the storage remembered a project — so the
/// second window reopened whatever the second window last had. This is the way
/// to say it outright.
///
/// Returns `Err(reason)` for an argument that was given but can't be used, so
/// the caller can say why instead of silently opening something else.
fn project_arg<I: Iterator<Item = String>>(args: I) -> Result<Option<PathBuf>, String> {
    let mut args = args.skip(1); // argv[0] is the exe
    let Some(first) = args.next() else {
        return Ok(None);
    };
    let raw = match first.as_str() {
        "--project" | "-p" => args
            .next()
            .ok_or_else(|| "--project needs a folder path".to_owned())?,
        // A lone flag we don't know isn't a path — ignore it rather than
        // trying to open `--verbose` as a project.
        other if other.starts_with('-') => return Ok(None),
        other => other.to_owned(),
    };
    let path = PathBuf::from(&raw);
    if !path.is_dir() {
        return Err(format!("\"{raw}\" is not a folder"));
    }
    // A project is a cargo project; opening a random folder would produce an
    // empty tree and a chip detected from nothing.
    if !path.join("Cargo.toml").is_file() {
        return Err(format!(
            "\"{raw}\" has no Cargo.toml — not a project folder"
        ));
    }
    Ok(Some(std::fs::canonicalize(&path).unwrap_or(path)))
}

#[cfg(test)]
mod project_arg_tests {
    use super::project_arg;

    fn args(list: &[&str]) -> std::vec::IntoIter<String> {
        std::iter::once("rust_on_chip.exe".to_owned())
            .chain(list.iter().map(|s| (*s).to_owned()))
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn no_argument_means_the_remembered_project() {
        assert_eq!(project_arg(args(&[])), Ok(None));
    }

    #[test]
    fn both_the_bare_and_flagged_forms_resolve_the_same_folder() {
        let dir = std::env::temp_dir().join(format!("eide_arg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]").unwrap();
        let p = dir.to_string_lossy().into_owned();

        let bare = project_arg(args(&[&p])).expect("accepted");
        let flagged = project_arg(args(&["--project", &p])).expect("accepted");
        assert!(bare.is_some());
        assert_eq!(bare, flagged);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_is_not_a_project_is_reported_not_opened() {
        let dir = std::env::temp_dir().join(format!("eide_arg_bad_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.to_string_lossy().into_owned();
        assert!(
            project_arg(args(&[&p])).is_err(),
            "no Cargo.toml — say so rather than opening an empty tree"
        );
        assert!(project_arg(args(&["/no/such/folder"])).is_err());
        assert!(project_arg(args(&["--project"])).is_err(), "missing value");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unknown flag is not a path — it must not be opened as one.
    #[test]
    fn unknown_flags_are_ignored() {
        assert_eq!(project_arg(args(&["--verbose"])), Ok(None));
    }
}

fn main() -> eframe::Result<()> {
    // FIRST, before anything can print: adopt the console we were launched from
    // (if any). A GUI-subsystem binary has no standard handles until this runs.
    build::attach_parent_console();
    // Then make a crash survivable to diagnose — double-clicked, there is no
    // stderr for the panic message to reach, so it also goes to a file.
    build::install_panic_logger();

    // Which project this window should open, if the caller said so.
    let (cli_project, cli_project_error) = match project_arg(std::env::args()) {
        Ok(p) => (p, None),
        // A bad path must not stop the IDE from starting — open normally and
        // let the app say what was wrong with the argument.
        Err(e) => (None, Some(e)),
    };

    // Claim this instance's scratch workspace BEFORE anything can look it up —
    // `msvc::warm_up` below already spawns a thread, and every later reader
    // (build, LSP, watcher) must see the same answer. A second IDE window gets
    // its own slot instead of fighting over one directory (see `workspace`).
    workspace::init();
    // Reclaim the slots of windows long gone. Off-thread, and only ever
    // touching directories no live process holds.
    workspace::sweep_stale_slots();

    // Resolve the MSVC toolchain env off-thread so the first build doesn't pay
    // for the one-off `vcvars64.bat` capture (see `msvc`).
    msvc::warm_up();

    // Everything eframe keys off the app NAME — including where it persists the
    // app state. Sharing that file between windows meant the last one to exit
    // decided which project both would reopen, so instances past the first get
    // their own. Slot 1 keeps the original name, and with it the state every
    // existing install already has.
    //
    // FROZEN at the 2026-09-20 rename: this is the storage identity, not the
    // product name — see `names::LEGACY_EFRAME_NAME`.
    let app_name = format!("{}{}", names::LEGACY_EFRAME_NAME, workspace::suffix());
    // The title is set explicitly so the storage name above doesn't leak into
    // it verbatim — a second window says "#2", which is what you want on a
    // taskbar, not "Embedded IDE_2".
    let title = match workspace::slot() {
        1 => names::APP_DISPLAY_NAME.to_owned(),
        s => format!("{} #{s}", names::APP_DISPLAY_NAME),
    };

    // Drop any geometry an earlier session stored, or it would override the
    // `with_maximized(true)` below, and the widget state no build will read
    // again — see `tidy_storage_file`. After `workspace::init`: this slot's
    // file is ours alone from here on.
    tidy_storage_file(&app_name);

    let options = eframe::NativeOptions {
        // The window opens maximized every time, so there is nothing worth
        // remembering about its size or position — and remembering it is
        // precisely what stopped `with_maximized` from taking effect.
        persist_window: false,
        viewport: egui::ViewportBuilder::default()
            .with_maximized(true)
            .with_title(&title)
            // Window + taskbar icon while the app runs. The PNG is baked into
            // the exe at compile time — replace assets/icon.png (any size,
            // 256×256 recommended) and rebuild to change it.
            .with_icon(
                eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png"))
                    .expect("assets/icon.png must be a valid PNG"),
            ),
        ..Default::default()
    };

    eframe::run_native(
        &app_name,
        options,
        Box::new(move |cc| Ok(Box::new(AppIde::new(cc, cli_project, cli_project_error)))),
    )
}
