//! Dropping the widget state no build will read again from the `"egui"` entry
//! of eframe's `app.ron`, before eframe loads it.
//!
//! egui persists widget state — resizable panel sizes, collapsing headers,
//! scroll offsets, text carets, `Resize` handles — in `Memory::data`, keyed by
//! `hash(TypeId::of::<T>()) ^ Id`. That `TypeId` moves with the egui version,
//! the compiler, any version or feature change in egui's own dependency tree,
//! and between debug and release, so every such rebuild orphans every record
//! written before it. egui never lets go of them: `PersistedMap::from_map`
//! always keeps the newest generation of every type and trims only above
//! 256 KiB per type. The slot-1 file on 2026-09-28 held 7,393 records under 49
//! type ids — ten build identities — in a 1.39 MB `"egui"` value, of which the
//! running build owned 291, and eframe rewrites all of it on every autosave
//! (~30 s).
//!
//! It cannot be done inside the app: `IdTypeMap` has no retain or iterate API,
//! and two of the five persisted types (`collapsing_header::InnerState`,
//! `resize::State`) are `pub(crate)`, so their dead type ids cannot even be
//! named. Hence text surgery on the stored value, before `run_native` —
//! cutting whole records out and leaving every other byte where it was.

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// A type none of whose records was read in this many launches of the slot
/// goes whole — unless it is one of the running build's own types.
///
/// Picked from the five real slot files (2026-09-28). A build identity shows up
/// as the five persisted types sharing a freshest generation; in slot 1 those
/// sit at 1 (running), 10, 14, 24 and 29 — the builds since the egui 0.36
/// upgrade, 57 launches within 50 hours, all kept so a rollback to any of them
/// still finds its layout — then 58 (a build already in use on 2026-08-28),
/// 235, 1125, 1258 and 1316, which go: 6,308 of 7,393 records. Slot 2 runs far
/// less (its newest dead build sits at 52), so there 50 launches is weeks.
/// Debug and release are two identities as well: each keeps the other's
/// layout as long as the other ran within its slot's last 50 launches.
///
/// Within one identity the rarely read type is `resize::State`, read only
/// while a resizable window is open: it trailed its group by at most 19
/// launches, so 50 would not split a build even without the probe (which
/// exempts the running build's types anyway).
///
/// The rule is per TYPE, not per build, and the probe knows only the running
/// build. So a build that is not running can lose just its lagging type: a
/// release build whose resizable windows went unopened for 50 launches of the
/// slot loses their sizes the first time a dev build prunes, while its panels
/// and folders stay. Accepted - it is a window size nobody reopened in 50
/// launches.
pub const STALE_TYPE_LAUNCHES: usize = 50;

/// A single record not read in this many launches goes, whatever its type —
/// the running build's included.
///
/// Widget ids move too (a renamed salt, a closed project's folders and files),
/// so a long-lived build still piles up records nothing asks for — the 235
/// build above holds 4,206 records from 887 launches. This is the backstop for
/// that, set far enough out that a dialog opened once a month survives even
/// at the upgrade-day pace (57 launches within 50 hours is ~800 a month). Every
/// record past it in the real files belongs to a dead build anyway. It does
/// reach further than egui itself, which trims a live type only past 256 KiB:
/// a window not opened in 1000 launches forgets its size here.
pub const STALE_RECORD_LAUNCHES: usize = 1000;

/// What a prune removed, with the replacement value.
#[derive(Debug)]
pub struct Pruned {
    /// The new `"egui"` value: the old one minus the dropped records.
    pub memory: String,
    pub removed: usize,
    pub kept: usize,
    /// Type ids left with no record at all.
    pub removed_types: usize,
}

/// One element of egui's `PersistedMap`: where it sits in the text, and the
/// two numbers the rule looks at.
#[derive(Debug)]
struct Record {
    type_id: u64,
    /// 1 when the session that saved it read (or created) it, +1 for every
    /// load that did not (`PersistedMap::into_map` adds one, a value that was
    /// read is written back as 1). So `generation - 1` launches since last use.
    generation: usize,
    span: Range<usize>,
}

/// The records of a serialized `PersistedMap`, and the byte range between its
/// brackets.
struct RecordList {
    items: Range<usize>,
    records: Vec<Record>,
}

/// Drop the dead records from a serialized `egui::Memory` — the `"egui"` value
/// of eframe's storage.
///
/// A record goes when it was not read in [`STALE_RECORD_LAUNCHES`], or when
/// its whole type was not read in [`STALE_TYPE_LAUNCHES`] and is not one of
/// `live_types` — the running build's own. `live_types` runs only when some
/// type is that stale (it lays out a headless frame); `None` from it keeps
/// every type, since a build that cannot name its own types must not guess.
///
/// `None` means "leave it alone": nothing to drop, a shape this parser does not
/// know (a future egui, a different `ron`), or a result this build's egui would
/// not load back with every kept record in it. Everything but the dropped
/// records — options, areas, the kept records — is copied through byte for
/// byte.
pub fn prune(memory: &str, live_types: impl FnOnce() -> Option<HashSet<u64>>) -> Option<Pruned> {
    let list = memory_records(memory)?;

    let mut freshest: HashMap<u64, usize> = HashMap::new();
    for r in &list.records {
        freshest
            .entry(r.type_id)
            .and_modify(|g| *g = (*g).min(r.generation))
            .or_insert(r.generation);
    }
    let stale_types = freshest.values().any(|&g| g > STALE_TYPE_LAUNCHES);
    let stale_records = list
        .records
        .iter()
        .any(|r| r.generation > STALE_RECORD_LAUNCHES);
    if !stale_types && !stale_records {
        return None;
    }
    let live = if stale_types { live_types() } else { None };
    let dead = |r: &Record| {
        r.generation > STALE_RECORD_LAUNCHES
            || live.as_ref().is_some_and(|live| {
                !live.contains(&r.type_id) && freshest[&r.type_id] > STALE_TYPE_LAUNCHES
            })
    };

    let mut out = String::with_capacity(memory.len());
    out.push_str(&memory[..list.items.start]);
    let mut kept_types = HashSet::new();
    let (mut kept, mut removed) = (0, 0);
    for r in &list.records {
        if dead(r) {
            removed += 1;
            continue;
        }
        if kept > 0 {
            out.push(',');
        }
        out.push_str(&memory[r.span.clone()]);
        kept_types.insert(r.type_id);
        kept += 1;
    }
    if removed == 0 {
        return None;
    }
    out.push_str(&memory[list.items.end..]);

    // The proof before anything is written: the value must load into THIS
    // build's `egui::Memory`, the way eframe will read it, with every kept
    // record still there. A record list this parser misread would fail here.
    let loaded: egui::Memory = ron::from_str(&out).ok()?;
    (loaded.data.len() == kept).then(|| Pruned {
        memory: out,
        removed,
        kept,
        removed_types: freshest.len() - kept_types.len(),
    })
}

/// The type ids this build's egui files its persisted widget state under.
///
/// Two of the five types are `pub(crate)`, so they cannot be asked for by
/// name: one headless frame shows one widget of each persisting kind, and the
/// ids are read back out of the serialized map. `None` if egui stored nothing,
/// which would mean the widgets below stopped persisting — a new egui that
/// wants this list revisited, not a build without types.
pub fn live_type_ids() -> Option<HashSet<u64>> {
    let ctx = egui::Context::default();
    crate::headless::run_ui(&ctx, egui::RawInput::default(), |ui| {
        egui::Panel::left("prune_probe_panel").show(ui, |_| {});
        egui::CollapsingHeader::new("prune_probe_header").show(ui, |_| {});
        egui::ScrollArea::vertical()
            .id_salt("prune_probe_scroll")
            .show(ui, |_| {});
        egui::Resize::default()
            .id_salt("prune_probe_resize")
            .show(ui, |_| {});
        ui.text_edit_singleline(&mut String::new());
    });

    let data = ctx.memory(|m| ron::to_string(&m.data)).ok()?;
    let mut c = Cursor::new(&data);
    c.eat(b"(")?;
    let list = c.record_list()?;
    c.eat(b")")?;
    let ids: HashSet<u64> = list.records.iter().map(|r| r.type_id).collect();
    (!ids.is_empty()).then_some(ids)
}

/// The `data` records of a serialized `egui::Memory`.
///
/// eframe stores `ron::ser::to_string(&memory)`: compact, and with egui 0.36.2
/// and ron 0.12 exactly
///
/// ```text
/// (options:(..),data:([REC,REC,..]),to_global:{..},areas:{..})
/// REC = (RAW_KEY,(type_id:(TYPE_ID),ron:"..",generation:G))
/// ```
///
/// where `data` is `IdTypeMap` serialized as `PersistedMap(Vec<(u64,
/// SerializedElement)>)` — hence the extra parentheses — `RAW_KEY` is
/// `TypeId ^ Id` and the string is the widget state's own RON, escaped. The
/// fields before `data` are skipped structurally; the ones after are never
/// looked at. Anything off this shape is `None`.
fn memory_records(memory: &str) -> Option<RecordList> {
    let mut c = Cursor::new(memory);
    c.eat(b"(")?;
    loop {
        let field = c.ident()?;
        c.eat(b":")?;
        if field == b"data" {
            c.eat(b"(")?;
            let list = c.record_list()?;
            c.eat(b")")?;
            return Some(list);
        }
        c.skip_value()?;
        // `)` here: a Memory without `data`, nothing to prune.
        c.eat(b",")?;
    }
}

/// A byte cursor for the few RON productions above. Every method returns
/// `None` on anything unexpected, and the caller gives up.
struct Cursor<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
}

impl<'a> Cursor<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            s,
            b: s.as_bytes(),
            i: 0,
        }
    }

    fn eat(&mut self, token: &[u8]) -> Option<()> {
        let end = self.i + token.len();
        (self.b.get(self.i..end)? == token).then(|| self.i = end)
    }

    fn ident(&mut self) -> Option<&'a [u8]> {
        let start = self.i;
        while self
            .b
            .get(self.i)
            .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
        {
            self.i += 1;
        }
        (self.i > start).then(|| &self.b[start..self.i])
    }

    fn number(&mut self) -> Option<u64> {
        let start = self.i;
        while self.b.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        // Checked: an overflowing number is not something egui wrote.
        std::str::from_utf8(&self.b[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    /// Past one escaped `"..."` string: up to the first `"` after an even run
    /// of `\` (every escape starts with `\`, and none contains a `"`, `\u{..}`
    /// included).
    ///
    /// The quotes are found with `str::find`, which runs std's optimized
    /// `memchr` even in a debug build. The 1.39 MB slot-1 value is mostly
    /// these strings: a byte loop here scanned it in 18 ms in the dev profile,
    /// this in 12-13.
    fn string(&mut self) -> Option<()> {
        self.eat(b"\"")?;
        let start = self.i;
        loop {
            let quote = self.i + self.s.get(self.i..)?.find('"')?;
            let escapes = self.b[start..quote]
                .iter()
                .rev()
                .take_while(|&&c| c == b'\\')
                .count();
            self.i = quote + 1;
            if escapes % 2 == 0 {
                return Some(());
            }
        }
    }

    /// Past one value, up to the `,` or closing bracket that ends it.
    fn skip_value(&mut self) -> Option<()> {
        let mut depth = 0usize;
        loop {
            match *self.b.get(self.i)? {
                // A raw string would read `\"` differently; egui writes none.
                b'"' if self.i > 0 && self.b[self.i - 1] == b'r' => return None,
                b'"' => self.string()?,
                // Char literals, raw strings, `#![enable(..)]`: not egui's.
                b'\'' | b'#' => return None,
                b'(' | b'[' | b'{' => {
                    depth += 1;
                    self.i += 1;
                }
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        return Some(());
                    }
                    depth -= 1;
                    self.i += 1;
                }
                b',' if depth == 0 => return Some(()),
                _ => self.i += 1,
            }
        }
    }

    /// `[REC,REC,..]`, each record checked field by field.
    fn record_list(&mut self) -> Option<RecordList> {
        self.eat(b"[")?;
        let start = self.i;
        let mut records = Vec::new();
        if self.eat(b"]").is_some() {
            return Some(RecordList {
                items: start..start,
                records,
            });
        }
        loop {
            let from = self.i;
            self.eat(b"(")?;
            self.number()?;
            self.eat(b",(type_id:(")?;
            let type_id = self.number()?;
            self.eat(b"),ron:")?;
            self.string()?;
            self.eat(b",generation:")?;
            let generation = usize::try_from(self.number()?).ok()?;
            self.eat(b"))")?;
            records.push(Record {
                type_id,
                generation,
                span: from..self.i,
            });
            if self.eat(b",").is_none() {
                let end = self.i;
                self.eat(b"]")?;
                return Some(RecordList {
                    items: start..end,
                    records,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::collapsing_header::CollapsingState;
    use egui::containers::panel::PanelState;
    use egui::containers::scroll_area::State as ScrollState;
    use egui::text::{CCursor, CCursorRange};
    use egui::text_edit::TextEditState;

    use crate::headless::run_ui;

    /// One `PersistedMap` element exactly as ron writes it.
    fn record(key: u64, type_id: u64, state_ron: &str, generation: usize) -> String {
        format!(
            "({key},(type_id:({type_id}),ron:{},generation:{generation}))",
            ron::to_string(state_ron).unwrap()
        )
    }

    /// A real Memory as eframe saves it — options off their defaults, a
    /// window's area — with `records` as its whole record list.
    fn memory(records: &[String]) -> String {
        static BASE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let base = BASE.get_or_init(|| {
            let ctx = egui::Context::default();
            ctx.options_mut(|o| {
                o.theme_preference = egui::ThemePreference::Light;
                o.zoom_factor = 1.25;
            });
            run_ui(&ctx, egui::RawInput::default(), |ui| {
                egui::Window::new("area").show(ui.ctx(), |_| {});
            });
            ctx.memory_mut(|m| m.data.clear());
            let text = ctx.memory(ron::ser::to_string).unwrap();
            assert_eq!(text.matches("data:([])").count(), 1, "{text}");
            assert!(text.contains("areas:{") && text.contains("zoom_factor:1.25"));
            text
        });
        base.replacen("data:([])", &format!("data:([{}])", records.join(",")), 1)
    }

    const LIVE: u64 = 1111;
    const RECENT: u64 = 2222; // a build that ran 9 launches ago
    const DEAD: u64 = 3333; // one that last ran 57 launches ago

    fn live() -> Option<HashSet<u64>> {
        Some(HashSet::from([LIVE]))
    }

    /// The dead build's records go; the running build's, and a recent one a
    /// rollback would want, stay; and not one other byte moves.
    #[test]
    fn a_dead_build_goes_and_every_other_byte_stays() {
        let keep = [
            record(1, LIVE, "(open:true,open_height:Some(20.0))", 1),
            record(2, LIVE, "(cursor:(ccursor_range:None))", 70),
            record(
                3,
                RECENT,
                "(outer_rect:(min:(x:0.0,y:0.0),max:(x:5.0,y:5.0)))",
                10,
            ),
            // Quotes and backslashes inside the state: the escape path.
            record(4, RECENT, r#"(label:"q\\\"")"#, 45),
        ];
        let dead = [
            record(5, DEAD, "(open:false)", 58),
            record(6, DEAD, "(open:true)", 234),
        ];
        let before = memory(&[
            dead[0].clone(),
            keep[0].clone(),
            keep[1].clone(),
            dead[1].clone(),
            keep[2].clone(),
            keep[3].clone(),
        ]);
        let pruned = prune(&before, live).expect("the dead build goes");
        assert_eq!(
            (pruned.removed, pruned.kept, pruned.removed_types),
            (2, 4, 1)
        );
        assert_eq!(
            pruned.memory,
            memory(&keep),
            "only the dead records may change"
        );
    }

    /// The running build is never dropped by type: a type of its own nobody
    /// read for longer than the type limit is a rarely opened dialog.
    #[test]
    fn the_running_builds_types_are_never_dropped_whole() {
        let only_live = memory(&[record(1, LIVE, "(open:true)", STALE_TYPE_LAUNCHES + 20)]);
        assert!(prune(&only_live, live).is_none());
        // Without the probe's word for it, the same type goes.
        let other = || Some(HashSet::from([9]));
        assert_eq!(prune(&only_live, other).expect("not live").removed, 1);
    }

    /// A probe that found nothing must not turn every type into a dead one.
    #[test]
    fn no_live_types_means_no_type_pruning() {
        let text = memory(&[record(1, DEAD, "(open:true)", 500)]);
        assert!(prune(&text, || None).is_none());
        // The per-record backstop still applies, to anyone's records.
        let fresh = record(1, LIVE, "(open:true)", 1);
        let ancient = record(2, LIVE, "(open:true)", STALE_RECORD_LAUNCHES + 1);
        let pruned = prune(&memory(&[fresh.clone(), ancient]), || None).expect("backstop");
        assert_eq!(pruned.removed, 1);
        assert_eq!(pruned.memory, memory(&[fresh]));
    }

    /// The probe lays out a whole headless frame: nothing stale, no frame.
    #[test]
    fn the_probe_runs_only_when_a_type_is_stale() {
        let fresh = memory(&[
            record(1, LIVE, "(open:true)", 3),
            record(2, DEAD, "(open:true)", STALE_TYPE_LAUNCHES),
        ]);
        let probe = || -> Option<HashSet<u64>> { panic!("no type is stale enough to ask") };
        assert!(prune(&fresh, probe).is_none());
    }

    /// Any shape but egui 0.36.2's is left alone, however close.
    #[test]
    fn off_shape_text_is_left_alone() {
        let good = memory(&[record(1, DEAD, "(open:true)", 500)]);
        assert!(prune(&good, live).is_some(), "the control case prunes");
        for bad in [
            String::new(),
            "garbage".to_owned(),
            good[..good.len() / 2].to_owned(),
            good[..good.len() - 1].to_owned(),
            good.replace("generation:", "generation: "),
            good.replace("type_id:(", "type_id:"),
            good.replace("ron:\"", "ron:r\""),
            good.replace("data:([", "data:["),
            good.replace(",generation:500", ",generation:99999999999999999999999"),
            "(options:(x:'a'),data:([]))".to_owned(),
            "(data:([(1,(type_id:(3333),ron:\"x\",generation:500))]),".to_owned(),
        ] {
            assert!(prune(&bad, live).is_none(), "{bad:?}");
        }
    }

    /// The whole egui path: states stored the way the IDE stores them, the
    /// Memory serialized the way eframe serializes it, dead records spliced
    /// in, the prune with the REAL probe, and a fresh Context that must read
    /// every live state back unchanged.
    #[test]
    fn pruned_memory_loads_into_egui_with_every_live_state() {
        let ctx = egui::Context::default();
        let panel = egui::Id::new("code_editor");
        let folder = egui::Id::new("tree_folder_src");
        let scroll = egui::Id::new("editor_scroll");
        let caret = egui::Id::new("editor_text");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let input = egui::RawInput {
            screen_rect: Some(screen),
            ..Default::default()
        };
        run_ui(&ctx, input, |ui| {
            egui::Panel::left(panel)
                .resizable(true)
                .default_size(321.0)
                .show(ui, |_| {});
            egui::Resize::default()
                .id_salt("ai_prompt")
                .default_size([200.0, 90.0])
                .show(ui, |_| {});
        });
        let mut header = CollapsingState::load_with_default_open(&ctx, folder, false);
        header.set_open(true);
        header.store(&ctx);
        let mut offset = ScrollState::default();
        offset.offset = egui::vec2(0.0, 123.5);
        offset.store(&ctx, scroll);
        let range = CCursorRange::two(CCursor::new(3), CCursor::new(7));
        let mut text = TextEditState::default();
        text.cursor.set_char_range(Some(range));
        text.store(&ctx, caret);
        let panel_before = PanelState::load(&ctx, panel).expect("the panel stored its size");

        // eframe: `epi::set_value(storage, "egui", mem)` is `ron::ser::to_string`.
        let saved = ctx.memory(ron::ser::to_string).unwrap();
        let live_records = memory_records(&saved).unwrap().records;
        assert_eq!(live_records.len(), 5, "one record per persisted type");
        // The running build unread for 69 launches (a rollback to it): only
        // the probe keeps it now. Then two dead builds, and one record of a
        // live type so old that the backstop takes it anyway.
        let aged = saved.replace(",generation:1))", ",generation:70))");
        let spliced = [
            record(11, 0xDEAD_0001, "(open:true,open_height:None)", 58),
            record(12, 0xDEAD_0001, "(open:false,open_height:None)", 120),
            record(13, 0xDEAD_0002, "(offset:(x:0.0,y:9.0))", 235),
            record(
                14,
                live_records[0].type_id,
                "(open:true)",
                STALE_RECORD_LAUNCHES + 1,
            ),
        ];
        let with_dead = aged.replacen("data:([", &format!("data:([{},", spliced.join(",")), 1);

        let pruned = prune(&with_dead, live_type_ids).expect("dead records to drop");
        assert_eq!(
            (pruned.removed, pruned.kept, pruned.removed_types),
            (4, 5, 2)
        );
        assert_eq!(
            pruned.memory, aged,
            "what is left is the live Memory, byte for byte"
        );

        // eframe: `epi::get_value` is `ron::from_str`; the Context adopts it.
        let loaded: egui::Memory = ron::from_str(&pruned.memory).unwrap();
        let fresh = egui::Context::default();
        fresh.memory_mut(|m| *m = loaded);
        let panel_after = PanelState::load(&fresh, panel).expect("panel size survives");
        assert_eq!(panel_after.outer_rect, panel_before.outer_rect);
        let header = CollapsingState::load(&fresh, folder).expect("folder state survives");
        assert!(header.is_open());
        let offset = ScrollState::load(&fresh, scroll).expect("offset survives");
        assert_eq!(offset.offset, egui::vec2(0.0, 123.5));
        let text = TextEditState::load(&fresh, caret).expect("caret survives");
        assert_eq!(text.cursor.char_range(), Some(range));
    }

    /// The probe names every type the IDE's widgets persist under: the three
    /// public ones by their real `TypeId`, five in all.
    #[test]
    fn the_probe_names_the_five_persisted_types() {
        use egui::util::id_type_map::TypeId;
        let ids = live_type_ids().expect("egui stored widget state");
        assert_eq!(ids.len(), 5, "{ids:?}");
        for t in [
            TypeId::of::<PanelState>(),
            TypeId::of::<ScrollState>(),
            TypeId::of::<TextEditState>(),
        ] {
            // `TypeId`'s number is private; its RON is `(N)`.
            let n: u64 = ron::to_string(&t)
                .unwrap()
                .trim_matches(['(', ')'])
                .parse()
                .unwrap();
            assert!(ids.contains(&n), "{t:?}");
        }
    }

    /// A second pass over its own output finds nothing to do.
    #[test]
    fn pruning_twice_changes_nothing_the_second_time() {
        let ctx = egui::Context::default();
        run_ui(&ctx, egui::RawInput::default(), |ui| {
            egui::CollapsingHeader::new("h").show(ui, |_| {});
        });
        let saved = ctx.memory(ron::ser::to_string).unwrap();
        let dead = record(7, 0xDEAD_0001, "(open:true)", 300);
        let with_dead = saved.replacen("data:([", &format!("data:([{dead},"), 1);
        let once = prune(&with_dead, live_type_ids).expect("one dead record");
        assert_eq!(once.removed, 1);
        assert_eq!(once.memory, saved);
        assert!(prune(&once.memory, live_type_ids).is_none());
    }
}
