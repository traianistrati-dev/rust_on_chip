//! What each built-in board - or, for a bare chip, its package - does to a
//! pad that the pad's name and functions cannot say: a strapping pin, the
//! flash bus, a divider, an erratum. Shown in the pin panel from
//! [`PinDef::note`](super::mcu_def::PinDef::note).
//!
//! The `.ron` files carry the notes; these tables are where they are written
//! and checked, one sourced fact at a time. Nordic's kits keep theirs beside
//! their pin tables in `codegen::nrf_boards`.
//!
//! ```text
//! cargo test --bin rust_on_chip emit_board_notes -- --ignored --nocapture
//! ```
//! writes every definition listed here to the temp dir with its notes.

use super::mcu_def::{McuDefinition, PinDef};

/// One definition's notes.
pub(crate) struct DefNotes {
    pub id: &'static str,
    /// Appended to every non-reserved pad: a rule of the silicon that holds
    /// for each user GPIO alike. Empty for none.
    pub every_gpio: &'static str,
    /// `(exact pad name, note)`.
    pub pads: &'static [(&'static str, &'static str)],
}

pub(crate) const TABLES: &[DefNotes] = &[];

/// The note pad `p` of definition `id` gets: its own, then the rule every
/// GPIO shares.
fn compose(t: &DefNotes, p: &PinDef) -> String {
    let own = t.pads.iter().find(|(name, _)| *name == p.name).map(|(_, n)| *n);
    let every = Some(t.every_gpio).filter(|e| !e.is_empty() && !p.reserved);
    [own, every].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// Write this definition's notes onto its pads, replacing whatever they had.
/// A definition with no table is left alone.
pub(crate) fn apply(def: &mut McuDefinition) {
    let Some(t) = TABLES.iter().find(|t| t.id == def.id) else {
        return;
    };
    let pins = &mut def.pins;
    for side in [&mut pins.top, &mut pins.bottom, &mut pins.left, &mut pins.right] {
        for p in side.iter_mut() {
            p.note = compose(t, p);
        }
    }
}

/// `text`, a definition's `.ron`, with each `PinDef`'s `note:` line set to
/// what `noted` gives the pad of that name - added, replaced or removed, and
/// nothing else in the file touched.
///
/// Not a parse and re-serialise: the hand-written files (the Picos, the
/// micro:bit, the C3, the F103) are laid out by hand, and the serializer would
/// rewrite all of them.
fn with_notes(text: &str, noted: &McuDefinition) -> String {
    let p = &noted.pins;
    let all: Vec<&PinDef> = p.top.iter().chain(&p.bottom).chain(&p.left).chain(&p.right).collect();
    let mut out = String::with_capacity(text.len() + 4096);
    let mut rest = text;
    while let Some(at) = rest.find("PinDef(") {
        let open = at + "PinDef(".len();
        let close = open + closing_paren(&rest[open..]);
        let body = &rest[open..close];
        out.push_str(&rest[..open]);
        out.push_str(&with_note(body, &all));
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

/// One `PinDef(...)` body (between its parens) with its note line set.
fn with_note(body: &str, all: &[&PinDef]) -> String {
    let name_at = body.find("name: ").expect("a PinDef has a name");
    let name_line_start = body[..name_at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &body[name_line_start..name_at];
    let quoted_end = name_at + "name: ".len() + string_len(&body[name_at + "name: ".len()..]);
    let name: String = ron::from_str(&body[name_at + "name: ".len()..quoted_end]).expect("a name");
    let mut notes = all.iter().filter(|p| p.name == name).map(|p| p.note.as_str());
    let note = notes.next().unwrap_or("");
    assert!(notes.all(|n| n == note), "pads named {name:?} get different notes");

    // Drop the old line, if any.
    let mut body = body.to_owned();
    let needle = format!("\n{indent}note: ");
    if let Some(i) = body.find(&needle) {
        let line_end = body[i + 1..].find('\n').map_or(body.len(), |j| i + 1 + j);
        body.replace_range(i..line_end, "");
    }
    if note.is_empty() {
        return body;
    }
    // The serializer writes `note` last, so the new line goes right before
    // the closing paren's line - ending as the file's lines do: ron writes
    // CRLF on Windows, and git may have checked the file out either way.
    let last_nl = body.rfind('\n').expect("a multi-line PinDef");
    let cr = if body.contains("\r\n") { "\r" } else { "" };
    let quoted =
        ron::ser::to_string_pretty(note, ron::ser::PrettyConfig::default()).expect("a string");
    let line = format!("\n{indent}note: {quoted},{cr}");
    body.insert_str(last_nl, &line);
    body
}

/// Bytes up to and excluding the `)` that closes the paren already open at
/// the start of `s`, skipping strings.
fn closing_paren(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut depth, mut i) = (1usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => i += string_len(&s[i..]) - 1,
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unclosed PinDef");
}

/// Length of the quoted string literal `s` starts with, quotes included.
fn string_len(s: &str) -> usize {
    let b = s.as_bytes();
    assert_eq!(b[0], b'"');
    let mut i = 1;
    while b[i] != b'"' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    i + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins;

    /// The kits whose notes `codegen::nrf_boards` writes.
    const NRF_KITS: [&str; 4] = ["nrf52840_dk", "nrf52832_dk", "nrf5340_dk", "nrf54l15_dk"];

    fn pads(def: &McuDefinition) -> impl Iterator<Item = &PinDef> {
        let p = &def.pins;
        p.top.iter().chain(&p.bottom).chain(&p.left).chain(&p.right)
    }

    fn ron_of(def: &McuDefinition) -> String {
        let text = ron::ser::to_string_pretty(
            def,
            ron::ser::PrettyConfig::default().struct_names(true),
        )
        .expect("serialise");
        crate::panels::mcu_module::ron_text::bare_none(&text)
    }

    /// Where two texts part, with a little of each, for an assert message.
    fn first_diff(a: &str, b: &str) -> String {
        let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let from = a[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = |s: &str| (at + 120).min(s.len());
        format!("\n--- left\n{:?}\n--- right\n{:?}", &a[from..end(a)], &b[from..end(b)])
    }

    fn committed(id: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/assets/mcus/{id}.ron",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    /// Every note lands on exactly one pad: a renamed pad must not take its
    /// note into the void, nor a repeated name hand it to two.
    #[test]
    fn every_note_names_one_pad() {
        for t in TABLES {
            let def = builtins::builtin_for(t.id).unwrap_or_else(|| panic!("no built-in {}", t.id));
            assert!(!NRF_KITS.contains(&t.id), "{}: its notes live in nrf_boards", t.id);
            for (name, note) in t.pads {
                let n = pads(&def).filter(|p| p.name == *name).count();
                assert_eq!(n, 1, "{}: {n} pads named {name:?}", t.id);
                assert!(!note.trim().is_empty(), "{}: empty note on {name}", t.id);
                assert!(note.is_ascii(), "{}: non-ASCII note on {name}", t.id);
            }
            assert!(t.every_gpio.is_ascii(), "{}", t.id);
        }
        let mut ids: Vec<_> = TABLES.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), TABLES.len(), "a definition listed twice");
    }

    /// The committed files say what the tables say, and no built-in outside
    /// them (or the Nordic kits) carries a note the tables do not know.
    #[test]
    fn the_committed_definitions_carry_the_notes() {
        for def in builtins::builtin_definitions() {
            if NRF_KITS.contains(&def.id.as_str()) {
                continue;
            }
            let mut want = def.clone();
            apply(&mut want);
            for (got, want) in pads(&def).zip(pads(&want)) {
                let want = if TABLES.iter().any(|t| t.id == def.id) {
                    want.note.as_str()
                } else {
                    ""
                };
                assert_eq!(
                    got.note, want,
                    "{} pad {:?} - run emit_board_notes",
                    def.id, got.name
                );
            }
        }
    }

    /// Setting the notes a file already has rewrites nothing - not the hand
    /// layout of the Picos, the micro:bit, the C3 or the F103, which a parse
    /// and re-serialise would all change.
    #[test]
    fn setting_the_notes_a_file_has_changes_nothing() {
        for def in builtins::builtin_definitions() {
            let text = committed(&def.id);
            let got = with_notes(&text, &def);
            assert!(got == text, "{}: with_notes moved something{}", def.id, first_diff(&got, &text));
        }
    }

    /// On a file the serializer DOES write back unchanged, a note set by text
    /// lands exactly where and as the serializer would put it - quotes,
    /// apostrophes and backslashes escaped its way - and removing it again
    /// restores the file.
    #[test]
    fn a_note_line_is_the_serializers() {
        for id in ["esp32c6", "rp2350_pico2_ice"] {
            let mut def = builtins::builtin_for(id).unwrap();
            assert!(ron_of(&def) == committed(id), "{id} no longer round-trips");
            let pins = &mut def.pins;
            for side in [&mut pins.top, &mut pins.bottom, &mut pins.left, &mut pins.right] {
                for p in side.iter_mut().filter(|p| !p.reserved) {
                    p.note = format!("The \"board's\" C:\\path - {}.", p.name);
                }
            }
            let text = with_notes(&committed(id), &def);
            let want = ron_of(&def);
            assert!(text == want, "{id}: not the serializer's lines{}", first_diff(&text, &want));
            let bare = builtins::builtin_for(id).unwrap();
            assert!(with_notes(&text, &bare) == committed(id), "{id}: removing them left a trace");
        }
    }

    /// Writes every definition with notes to the temp dir, to copy over
    /// `assets/mcus/`.
    #[test]
    #[ignore = "authoring tool: writes the noted definitions to the temp dir"]
    fn emit_board_notes() {
        for t in TABLES {
            let mut def = builtins::builtin_for(t.id).unwrap();
            apply(&mut def);
            let path = std::env::temp_dir().join(format!("{}.ron", t.id));
            std::fs::write(&path, with_notes(&committed(t.id), &def)).expect("write");
            println!("wrote {}", path.display());
        }
    }
}
