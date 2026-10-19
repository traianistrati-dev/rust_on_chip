//! Which `src/pins/configs/` files still hold the IDE's own text - and so
//! follow a changed template - and which the user edited, which keep their
//! edits while the new template waits beside them.
//!
//! A config file is a GENERATED block the IDE re-splices on every sync, and
//! the text around it - the `use` lines, `init`, the example - which is
//! written once and is the user's from then on. That text used to be
//! rewritten wholesale by every Runtime / Init-API Apply, edits and all, and
//! never otherwise: an Apply that changed nothing in a file still threw its
//! edits away, and an IDE upgrade that improved a template reached no
//! existing project.
//!
//! Now each file is decided on its content, on every sync. The FINGERPRINT
//! of its editable text (everything outside the block) is compared with the
//! new template's and with what the IDE last wrote there:
//!
//! - still the IDE's (equal to the template, or to what was last written, or
//!   a Reset was asked for): the file becomes the template, whatever changed;
//! - edited: the block is spliced as always, and when the template changed
//!   under the edits it is OFFERED - kept beside the file in
//!   `.rustonchip/templates/<path>.txt` - instead of written over them.
//!
//! What was last written is the RECORD, `.rustonchip/templates.ron`, saved
//! with the project. A file it does not know - a project from before it, a
//! lost record - counts as edited unless it equals the template: the safe
//! direction, which never overwrites anything.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The record, relative to the project root.
pub const RECORD: &str = ".rustonchip/templates.ron";
/// Where offered templates and pre-Reset copies go, relative to the root.
/// Never as `.rs`: a Save deletes every `.rs` the tree does not hold.
pub const SIDECARS: &str = ".rustonchip/templates";

pub(crate) const GEN_BEGIN: &str = "// <<< GENERATED>>>";
pub(crate) const GEN_END: &str = "// <<< GENERATED END >>>";

/// FNV-1a, 64 bit: the same function the IDE uses for its other content
/// hashes. A collision would only make an edited file look pristine with odds
/// of 2^-64.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// What stands before `pos` on its line - a BOM an outside editor left aside.
fn line_lead(content: &str, pos: usize) -> &str {
    let line_start = content[..pos].rfind(['\n', '\r']).map_or(0, |n| n + 1);
    let lead = &content[line_start..pos];
    if line_start == 0 {
        lead.strip_prefix('\u{feff}').unwrap_or(lead)
    } else {
        lead
    }
}

/// Does what stands at `pos` start its line, after indentation?
fn starts_line(content: &str, pos: usize) -> bool {
    line_lead(content, pos)
        .chars()
        .all(|c| c == ' ' || c == '\t')
}

/// `(begin, end)` of the GENERATED block: where its BEGIN marker starts and
/// just past the END that closes it.
///
/// The first BEGIN and the first END after it, as the splice always found
/// them - whatever stands beside the markers (a BOM, indentation, a note, a
/// `/* */`), inside the block (a string the user typed in a tab) or below it
/// (a marker in a string, a pasted pair). A BEGIN is passed over only where
/// it can be nothing but a mention, so a comment ABOUT the block splits
/// nothing: when it stands inside a line comment or a string (`//` or `"`
/// before it on its line) AND
///
/// - its END is on the same line: the pair named in one comment - a block
///   the user emptied in place (`BEGIN// END`) has nothing before its BEGIN
///   and stays the block;
/// - or a BEGIN that starts its line comes before its END: the real one,
///   below the mention.
///
/// Text of any other kind before a BEGIN - a `/* pinned */` - leaves it the
/// block.
pub(crate) fn block(content: &str) -> Option<(usize, usize)> {
    for (begin, _) in content.match_indices(GEN_BEGIN) {
        let from = begin + GEN_BEGIN.len();
        // No END after this BEGIN: none after any later one either.
        let Some(len) = content[from..].find(GEN_END) else {
            break;
        };
        let inside = &content[from..from + len];
        let lead = line_lead(content, begin);
        let quoted = lead.contains("//") || lead.contains('"');
        let one_line = !inside.contains(['\n', '\r']);
        let real_below = inside
            .match_indices(GEN_BEGIN)
            .any(|(i, _)| starts_line(content, from + i));
        if quoted && (one_line || real_below) {
            continue;
        }
        return Some((begin, from + len + GEN_END.len()));
    }
    None
}

/// Has the user turned `body`'s template down for the file `stamp` is of?
/// Then nothing may move the file onto it - not `decide`, not an IoT upgrade.
pub fn declines(stamp: &Stamp, body: &str) -> bool {
    fingerprint(body, headed(body)).is_some_and(|fp| stamp.declined.contains(&fp))
}

/// `(head, tail)`: the text before the GENERATED block and after it, LF and
/// trimmed - exactly what a splice keeps, so a splice never changes it. `None`
/// for a file without both markers. A BOM an outside editor left is not text.
pub fn editable(content: &str) -> Option<(String, String)> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let (begin, end) = block(content)?;
    let lf = |s: &str| s.replace("\r\n", "\n").trim().to_owned();
    Some((lf(&content[..begin]), lf(&content[end..])))
}

/// Does the TEMPLATE carry text above its block? Some do (ESP's `use` lines,
/// which follow the runtime); most start with the marker, and then a head the
/// user typed above it is theirs and takes no part in the decision.
pub fn headed(body: &str) -> bool {
    editable(body).is_some_and(|(head, _)| !head.is_empty())
}

/// The fingerprint of `content`'s editable text, counting the head only when
/// the template has one. `None` without both markers.
pub fn fingerprint(content: &str, headed: bool) -> Option<u64> {
    let (head, tail) = editable(content)?;
    let head = if headed { head.as_str() } else { "" };
    Some(fnv1a64(format!("{head}\n\u{1}\n{tail}").as_bytes()))
}

/// What the record holds for one file.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq, Debug)]
pub struct Stamp {
    /// The editable text the IDE last wrote into the file. `None`: unknown -
    /// a file from before the record that did not equal the template then.
    pub written: Option<u64>,
    /// The template the last sync compared the file with. A different one at
    /// the next sync means the template changed.
    pub seen: Option<u64>,
    /// A template offered beside the edited file and not yet taken or
    /// dismissed.
    pub offered: Option<u64>,
    /// Templates the user turned down ("Keep my version"): never offered
    /// again, and never followed by a file that only looks untouched.
    #[serde(default)]
    pub declined: BTreeSet<u64>,
    /// Whether the template `written` came from had text above its block -
    /// so the file is measured the same way when it is compared with it.
    #[serde(default)]
    pub headed: bool,
}

/// `.rustonchip/templates.ron`: one [`Stamp`] per config file, keyed by its
/// project-root-relative path. A `BTreeMap`, so the file diffs cleanly.
#[derive(Serialize, Deserialize, Clone, PartialEq, Eq, Debug)]
pub struct TemplateRecord {
    pub version: u32,
    pub files: BTreeMap<String, Stamp>,
}

impl TemplateRecord {
    pub const VERSION: u32 = 1;

    /// The record under `root`, or an empty one: missing, unreadable or of
    /// another version all mean "nothing known", which only makes the sync
    /// more careful.
    pub fn load(root: &Path) -> Self {
        std::fs::read_to_string(root.join(RECORD))
            .ok()
            .and_then(|text| ron::from_str::<Self>(&text).ok())
            .filter(|r| r.version == Self::VERSION)
            .unwrap_or_default()
    }

    /// The record with only the paths `keep` says are still in the project: a
    /// file pruned this session keeps its stamp in memory, in case it comes
    /// back, but not on disk.
    pub fn kept(&self, keep: impl Fn(&str) -> bool) -> Self {
        Self {
            version: self.version,
            files: self
                .files
                .iter()
                .filter(|(path, _)| keep(path))
                .map(|(path, stamp)| (path.clone(), stamp.clone()))
                .collect(),
        }
    }

    /// The text of the record as it goes on disk.
    pub fn to_ron(&self) -> String {
        // LF on every OS (ron's default is CRLF on Windows): the header is
        // LF, and a Save compares this with the file to skip an unchanged one.
        let pretty = ron::ser::PrettyConfig::default().new_line("\n");
        let body = ron::ser::to_string_pretty(self, pretty).unwrap_or_default();
        format!(
            "// Written by RustOnChip: what it last wrote into each config file, so a\n\
             // changed template replaces only the files you did not edit. Commit it\n\
             // with the files it describes.\n{body}\n"
        )
    }

    /// Is `path` offered a template now?
    pub fn offered(&self, path: &str) -> bool {
        self.files.get(path).is_some_and(|s| s.offered.is_some())
    }
}

impl Default for TemplateRecord {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            files: BTreeMap::new(),
        }
    }
}

/// What one sync does with one existing config file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Decision {
    /// The file is the IDE's: it becomes the template ([`template_text`]).
    /// `replaced` when that changed its editable text; `keep_head` when the
    /// text above the block is the user's and goes with it.
    Template { replaced: bool, keep_head: bool },
    /// The file is the user's: only the block is spliced. `offer` when the
    /// template changed under the edits this sync.
    Keep { offer: bool },
}

/// The decision for a file holding `content` whose template is now `body`,
/// with what the record had for it, and the stamp to keep after it. `reset`:
/// the user asked for the template.
pub fn decide(stamp: Option<&Stamp>, content: &str, body: &str, reset: bool) -> (Decision, Stamp) {
    let old = stamp.cloned().unwrap_or_default();
    let headed = headed(body);
    let Some(new) = fingerprint(body, headed) else {
        // A template without markers has no editable text to compare: the
        // file is the user's, as today.
        return (Decision::Keep { offer: false }, old);
    };
    let head_free = editable(content).is_none_or(|(head, _)| head.is_empty());
    let cur = fingerprint(content, headed);
    // Compared with what was written the way it was measured then.
    let then = fingerprint(content, old.headed);
    let declined = old.declined.contains(&new);
    // What the IDE wrote, untouched - unless the new template brings a head
    // of its own over text the user typed above a headless one: that text
    // cannot ride along, so the file is theirs.
    let unedited = then.is_some() && old.written == then && (old.headed || !headed || head_free);
    // Equal to the template - unless the template lost its head and the
    // file still has one that is not the IDE's (that `unedited` would say).
    let equal = cur == Some(new) && (headed || !old.headed || head_free);
    if reset || equal || (unedited && !declined) {
        let stamp = Stamp {
            written: Some(new),
            seen: Some(new),
            offered: None,
            // A template turned down stays turned down through the files the
            // IDE writes meanwhile - until the user asks for it: by a Reset,
            // or by making the file the template (`equal`), which is taking
            // it after all.
            declined: if reset {
                BTreeSet::new()
            } else {
                let mut declined = old.declined;
                declined.remove(&new);
                declined
            },
            headed,
        };
        // Text above the block of a headless template is the user's - unless
        // the template it came from had a head (then it was ours), or the
        // user asked for the template as it is.
        let keep_head = !reset && !headed && !old.headed;
        return (
            Decision::Template {
                replaced: cur != Some(new) || (!keep_head && !head_free && !headed),
                keep_head,
            },
            stamp,
        );
    }
    // Edits built on the template as it is now (switched away and back), or a
    // template the user turned down: nothing to offer, and an earlier offer
    // is moot.
    if old.written == Some(new) || declined {
        let stamp = Stamp {
            seen: Some(new),
            offered: None,
            ..old
        };
        return (Decision::Keep { offer: false }, stamp);
    }
    // Quiet on a first look (`seen` unknown): there is no "before" to say the
    // template changed from.
    let offer = old.seen.is_some_and(|seen| seen != new);
    let stamp = Stamp {
        seen: Some(new),
        offered: if offer { Some(new) } else { old.offered },
        ..old
    };
    (Decision::Keep { offer }, stamp)
}

/// The stamp of a file the sync has just written whole from `body`.
pub fn written(body: &str) -> Stamp {
    let headed = headed(body);
    let fp = fingerprint(body, headed);
    Stamp {
        written: fp,
        seen: fp,
        headed,
        ..Stamp::default()
    }
}

/// `body` as the file becomes when it follows its template - with the text
/// the user typed above the block when `keep_head` (see [`Decision`]).
pub fn template_text(content: &str, body: &str, keep_head: bool) -> String {
    if !keep_head || headed(body) {
        return body.to_owned();
    }
    match editable(content) {
        Some((head, _)) if !head.is_empty() => format!("{head}\n\n{body}"),
        _ => body.to_owned(),
    }
}

/// What the tree shows for a config file: its template's whole editable text
/// as it is now, and whether one waits beside it. Rebuilt by every sync.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    /// [`fingerprint`] of the template, head INCLUDED - a Reset writes the
    /// template as it is, so text above the block is a difference too.
    pub full: u64,
    pub offered: bool,
}

impl Mark {
    /// The mark of a file whose template is `body`.
    pub fn of(body: &str, offered: bool) -> Option<Self> {
        Some(Self {
            full: fingerprint(body, true)?,
            offered,
        })
    }

    /// Does `content` differ from the template - so a Reset would change it?
    pub fn differs(&self, content: &str) -> bool {
        self.offered || fingerprint(content, true) != Some(self.full)
    }
}

/// `<root>/.rustonchip/templates/<path>.txt`: where the template offered for
/// `path` is kept.
pub fn sidecar(root: &Path, path: &str) -> PathBuf {
    root.join(SIDECARS).join(format!("{path}.txt"))
}

/// Where the copy of `path` as it was before a Reset goes: `<path>.mine.txt`,
/// or the first free `<path>.mine.<n>.txt` - never over a copy that says
/// something else. `None` when a copy with exactly `content` is there already.
pub fn backup(root: &Path, path: &str, content: &str) -> Option<PathBuf> {
    for n in 1.. {
        let name = if n == 1 {
            format!("{path}.mine.txt")
        } else {
            format!("{path}.mine.{n}.txt")
        };
        let file = root.join(SIDECARS).join(name);
        match std::fs::read(&file) {
            Ok(bytes) if bytes == content.as_bytes() => return None,
            Ok(_) => continue,
            Err(_) => return Some(file),
        }
    }
    unreachable!("an unbounded counter always reaches a free name")
}

/// The sidecar folder, with the `.gitignore` that keeps its offers and copies
/// out of git and out of `git clean` (the record, one level up, is meant to
/// be committed).
pub fn prepare_sidecars(root: &Path) -> std::io::Result<()> {
    let dir = root.join(SIDECARS);
    std::fs::create_dir_all(&dir)?;
    let ignore = dir.join(".gitignore");
    if !ignore.is_file() {
        std::fs::write(&ignore, "*\n")?;
    }
    Ok(())
}

/// Write `content` to `file` under the sidecar folder, making both.
pub fn write_sidecar(root: &Path, file: &Path, content: &str) -> std::io::Result<()> {
    prepare_sidecars(root)?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(file, content)
}

/// Write `record` under `root`. Only the record: the copies beside the files
/// are [`reconcile_sidecars`]'s, on the UI thread - this runs on the Save
/// worker, which may run behind what the user did meanwhile. A project with
/// no config file gets no record - and no `.rustonchip/` - until it has one.
pub fn save(root: &Path, record: &TemplateRecord) -> std::io::Result<()> {
    let record_path = root.join(RECORD);
    let record_ron = record.to_ron();
    // As git may have checked it out: CRLF is the same record.
    let on_disk = std::fs::read_to_string(&record_path)
        .ok()
        .map(|t| t.replace("\r\n", "\n"));
    if (!record.files.is_empty() || record_path.exists())
        && on_disk.as_deref() != Some(record_ron.as_str())
    {
        if let Some(dir) = record_path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&record_path, &record_ron)?;
    }
    Ok(())
}

/// Make the copies beside the files say what `record` says NOW: every offered
/// template in `offers` written, every offer file nothing offers any more -
/// pruned, moved, followed, kept - deleted. `<path>.mine[.n].txt` copies are
/// the user's and stay.
pub fn reconcile_sidecars(
    root: &Path,
    record: &TemplateRecord,
    offers: &[(String, String)],
) -> std::io::Result<()> {
    for (path, body) in offers.iter().filter(|(p, _)| record.offered(p)) {
        let file = sidecar(root, path);
        if std::fs::read_to_string(&file).ok().as_deref() != Some(body.as_str()) {
            write_sidecar(root, &file, body)?;
        }
    }
    let dir = root.join(SIDECARS);
    for file in sidecar_files(&dir) {
        let Ok(rel) = file.strip_prefix(&dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Some(path) = rel.strip_suffix(".txt") else {
            continue;
        };
        if path.contains(".mine") || !path.ends_with(".rs") {
            continue;
        }
        if !record.offered(path) {
            let _ = std::fs::remove_file(&file);
        }
    }
    Ok(())
}

/// The `.txt` files under `dir`. A link is not followed - whatever it points
/// at is not the IDE's to delete.
fn sidecar_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let p = entry.path();
        if kind.is_dir() {
            out.extend(sidecar_files(&p));
        } else if kind.is_file() && p.extension().is_some_and(|e| e == "txt") {
            out.push(p);
        }
    }
    out
}

/// The paths whose record entry a Save would change on disk although no
/// file's text moved: what the IDE knows it wrote (a file adopted on open),
/// an offer made, changed or taken, a template turned down. Not `seen`: that
/// is the sync's own bookkeeping, rebuilt at every open.
pub fn record_changes(memory: &TemplateRecord, disk: &TemplateRecord) -> Vec<String> {
    let state = |r: &TemplateRecord, p: &str| {
        r.files
            .get(p)
            .map(|s| (s.written, s.offered, s.declined.clone()))
    };
    memory
        .files
        .keys()
        .chain(disk.files.keys())
        .filter(|p| state(memory, p) != state(disk, p))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "// <<< GENERATED>>>\npub const BAUD: u32 = 9600;\n// <<< GENERATED END >>>\n\npub fn init() {}\n";

    fn newer() -> String {
        BODY.replace("init() {}", "init() -> u8 { 0 }")
    }

    fn edited() -> String {
        BODY.replace("init() {}", "init() { mine(); }")
    }

    fn template(replaced: bool, keep_head: bool) -> Decision {
        Decision::Template {
            replaced,
            keep_head,
        }
    }

    const KEEP: Decision = Decision::Keep { offer: false };
    const OFFER: Decision = Decision::Keep { offer: true };

    #[test]
    fn fnv_matches_the_reference_vectors() {
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn the_fingerprint_ignores_the_block_line_endings_and_a_bom() {
        let other_block = BODY.replace("9600", "115200");
        assert_eq!(fingerprint(BODY, false), fingerprint(&other_block, false));
        assert_eq!(
            fingerprint(BODY, false),
            fingerprint(&BODY.replace('\n', "\r\n"), false)
        );
        // What a splice does to the space around the block.
        let spliced = BODY.replace("\n\npub fn", "\n\n\n\npub fn");
        assert_eq!(fingerprint(BODY, false), fingerprint(&spliced, false));
        // An outside editor's BOM, on a headed template too.
        let headed = format!("use esp_hal::Blocking;\n{BODY}");
        assert_eq!(
            fingerprint(&headed, true),
            fingerprint(&format!("\u{feff}{headed}"), true)
        );
        assert_eq!(editable(&format!("\u{feff}{BODY}")).unwrap().0, "");
    }

    /// A marker MENTIONED in a comment is not one: only a marker on a line
    /// of its own opens or closes the block.
    #[test]
    fn a_marker_in_a_comment_is_not_the_block() {
        let content =
            format!("// the block starts at // <<< GENERATED>>> below\nuse my::thing;\n\n{BODY}");
        let (head, tail) = editable(&content).unwrap();
        assert!(head.contains("use my::thing;"), "{head}");
        assert_eq!(tail, "pub fn init() {}");
        // ...nor is one naming the pair on one line.
        let mention = format!("// keep {GEN_BEGIN} .. {GEN_END} as the IDE wrote it\n");
        let content = format!("{mention}use my::thing;\n\n{BODY}");
        assert_eq!(
            block(&content).map(|(b, _)| &content[..b]),
            Some(format!("{mention}use my::thing;\n\n").as_str())
        );
        // ...but a block the user emptied in place, its markers on one line,
        // still is the block.
        let emptied = format!("{GEN_BEGIN}{GEN_END}\n\npub fn init() {{}}\n");
        assert_eq!(block(&emptied), Some((0, GEN_BEGIN.len() + GEN_END.len())));
        // A BEGIN mentioned INSIDE the block - a note, a topic typed in the
        // IoT tab - leaves the block where it starts.
        for inside in [
            format!("// note: the block starts at {GEN_BEGIN} above\n"),
            format!("pub const TOPIC: &str = \"dev/{GEN_BEGIN}/x\";\n"),
        ] {
            let content = BODY.replace("pub const BAUD", &format!("{inside}pub const BAUD"));
            assert_eq!(block(&content).map(|(b, _)| b), Some(0));
            assert!(!headed(&content));
        }
        // Indented, with CRLF, or after a BOM, it still is the block.
        let indented = BODY
            .replace(GEN_BEGIN, &format!("    {GEN_BEGIN}"))
            .replace('\n', "\r\n");
        assert_eq!(block(&indented).map(|(b, _)| b), Some(4));
        assert_eq!(
            block(&format!("\u{feff}{BODY}")).map(|(b, _)| b),
            Some('\u{feff}'.len_utf8())
        );
    }

    /// Text beside a marker - a note on its line - does not lose the block:
    /// it is found as before, by the first marker.
    #[test]
    fn a_marker_with_text_beside_it_is_still_the_block() {
        let noted = BODY.replace(GEN_END, &format!("{GEN_END} // do not touch"));
        let (begin, end) = block(&noted).unwrap();
        assert_eq!(begin, 0);
        assert!(noted[end..].starts_with(" // do not touch"));
        let noted = BODY.replace(GEN_BEGIN, &format!("{GEN_BEGIN} // do not touch"));
        assert_eq!(block(&noted).map(|(b, _)| b), Some(0));
        // A BEGIN of its own wins over a mention above it.
        let mentioned = format!("// see {GEN_BEGIN} below\n{BODY}");
        assert_eq!(
            block(&mentioned).map(|(b, _)| &mentioned[..b]),
            Some(format!("// see {GEN_BEGIN} below\n").as_str())
        );
        // A marker line below the block - in a string, a pasted pair - is not
        // it, whatever stands before the real BEGIN on its line.
        let pinned = BODY.replace(GEN_BEGIN, &format!("/* pinned */ {GEN_BEGIN}"));
        // ...and so is a `/* pinned */` one with a line inside the block that
        // starts with the marker text: only a BEGIN in a comment or a string
        // is a mention.
        let inner = pinned.replace(
            "pub const BAUD",
            &format!("{GEN_BEGIN} <- the IDE rewrites from here\npub const BAUD"),
        );
        assert_eq!(block(&inner).map(|(b, _)| b), pinned.find(GEN_BEGIN));
        let quoted = format!("const S: &str = \"{GEN_BEGIN}\";\n{BODY}");
        assert_eq!(block(&quoted).map(|(b, _)| &quoted[b..]), Some(BODY));
        for real in [noted.as_str(), pinned.as_str()] {
            let begin = real.find(GEN_BEGIN).unwrap();
            let in_string = format!("{real}const DOC: &str = r#\"\n{GEN_BEGIN}\n\"#;\n");
            assert_eq!(block(&in_string).map(|(b, _)| b), Some(begin));
            let pasted = format!("{real}{GEN_BEGIN}\nfn mine() {{}}\n{GEN_END}\n");
            assert_eq!(block(&pasted).map(|(b, _)| b), Some(begin));
        }
    }

    #[test]
    fn one_character_of_editable_text_changes_it() {
        let edited = BODY.replace("init() {}", "init() { }");
        assert_ne!(fingerprint(BODY, false), fingerprint(&edited, false));
        let headed = format!("use x;\n{BODY}");
        assert_ne!(fingerprint(BODY, true), fingerprint(&headed, true));
        // ...but a head above a headless template is not looked at.
        assert_eq!(fingerprint(BODY, false), fingerprint(&headed, false));
        assert_eq!(fingerprint("no markers", false), None);
    }

    #[test]
    fn a_user_head_survives_following_the_template_but_not_a_reset() {
        let content = format!("use my::thing;\n\n{BODY}");
        assert_eq!(
            template_text(&content, &newer(), true),
            format!("use my::thing;\n\n{}", newer())
        );
        assert_eq!(template_text(&content, &newer(), false), newer());
        // A template with its own head owns it.
        let headed = format!("use esp_hal::Blocking;\n{BODY}");
        assert_eq!(template_text(&content, &headed, true), headed);
        // The decision says which: a head above a headless template rides
        // along, a Reset drops it.
        let was = written(BODY);
        assert_eq!(
            decide(Some(&was), &content, &newer(), false).0,
            template(true, true)
        );
        assert_eq!(
            decide(Some(&was), &content, &newer(), true).0,
            template(true, false)
        );
    }

    #[test]
    fn the_record_round_trips_and_a_bad_one_is_empty() {
        let dir = std::env::temp_dir().join(format!("eide_tmpl_record_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut r = TemplateRecord::default();
        let mut declined = written(BODY);
        declined.declined.insert(7);
        r.files
            .insert("src/pins/configs/usart1.rs".into(), declined.clone());
        r.files
            .insert("src/pins/configs/gone.rs".into(), Stamp::default());
        save(&dir, &r.kept(|p| !p.ends_with("gone.rs"))).expect("save");
        // One line ending throughout - and an unchanged record, LF or as git
        // checked it out with CRLF, is not written again.
        let text = std::fs::read_to_string(dir.join(RECORD)).unwrap();
        assert!(!text.contains('\r'), "{text:?}");
        let kept = r.kept(|p| !p.ends_with("gone.rs"));
        for on_disk in [text.clone(), text.replace('\n', "\r\n")] {
            std::fs::write(dir.join(RECORD), &on_disk).unwrap();
            let mut perms = std::fs::metadata(dir.join(RECORD)).unwrap().permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(dir.join(RECORD), perms.clone()).unwrap();
            let unchanged = save(&dir, &kept);
            #[allow(clippy::permissions_set_readonly_false)]
            perms.set_readonly(false);
            std::fs::set_permissions(dir.join(RECORD), perms).unwrap();
            assert!(unchanged.is_ok(), "an unchanged record is not written");
        }
        let back = TemplateRecord::load(&dir);
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.files["src/pins/configs/usart1.rs"], declined);
        // A record written before `declined`/`headed` existed still reads.
        std::fs::write(
            dir.join(RECORD),
            "(version: 1, files: {\"a.rs\": (written: Some(1), seen: Some(1), offered: None)})",
        )
        .unwrap();
        assert_eq!(TemplateRecord::load(&dir).files["a.rs"].written, Some(1));
        std::fs::write(dir.join(RECORD), "(version: 99, files: {})").unwrap();
        assert_eq!(TemplateRecord::load(&dir), TemplateRecord::default());
        std::fs::write(dir.join(RECORD), "not ron").unwrap();
        assert_eq!(TemplateRecord::load(&dir), TemplateRecord::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every row of the decision.
    #[test]
    fn the_decision_table() {
        let (newer, edited) = (newer(), edited());
        let was = written(BODY);
        // Equal to the template: ours, nothing to replace.
        assert_eq!(
            decide(Some(&was), BODY, BODY, false).0,
            template(false, true)
        );
        // What we last wrote, template changed: replaced.
        let (d, s) = decide(Some(&was), BODY, &newer, false);
        assert_eq!(d, template(true, true));
        assert_eq!(s, written(&newer));
        // Edited, template unchanged: kept, no offer.
        let (d, s) = decide(Some(&was), &edited, BODY, false);
        assert_eq!(d, KEEP);
        assert_eq!(s, was);
        // Edited, template changed: kept and offered - once.
        let (d, s) = decide(Some(&was), &edited, &newer, false);
        assert_eq!(d, OFFER);
        assert_eq!(s.offered, written(&newer).seen);
        assert_eq!(decide(Some(&s), &edited, &newer, false), (KEEP, s.clone()));
        // ...and Reset takes it.
        assert_eq!(
            decide(Some(&s), &edited, &newer, true).0,
            template(true, false)
        );
        // No record: equal is adopted, different is the user's - quietly.
        assert_eq!(decide(None, BODY, BODY, false).0, template(false, true));
        let (d, s) = decide(None, &edited, BODY, false);
        assert_eq!(d, KEEP);
        assert_eq!(s.written, None);
        // ...until its template changes.
        assert_eq!(decide(Some(&s), &edited, &newer, false).0, OFFER);
        // Markers removed by the user: theirs.
        assert_eq!(
            decide(Some(&was), "pub fn init() {}\n", BODY, false).0,
            KEEP
        );
    }

    /// Switched away and back (A -> B -> A): the edits are built on A, so A
    /// is not "new" to them - no offer, and B's offer goes.
    #[test]
    fn the_template_the_edits_are_built_on_is_never_offered() {
        let (newer, edited) = (newer(), edited());
        let (_, on_b) = decide(Some(&written(BODY)), &edited, &newer, false);
        assert!(on_b.offered.is_some());
        let (d, back) = decide(Some(&on_b), &edited, BODY, false);
        assert_eq!(d, KEEP);
        assert_eq!(back.offered, None);
    }

    /// "Keep my version": every refused template is never offered again, and
    /// a file whose edits are later undone does not quietly follow one.
    #[test]
    fn declined_templates_stay_declined() {
        let (newer, edited) = (newer(), edited());
        let third = BODY.replace("init() {}", "init() -> u16 { 1 }");
        let keep = |mut s: Stamp| {
            if let Some(o) = s.offered.take() {
                s.declined.insert(o);
            }
            s
        };
        let (_, offered) = decide(Some(&written(BODY)), &edited, &newer, false);
        let kept = keep(offered);
        assert_eq!(
            decide(Some(&kept), &edited, &newer, false),
            (KEEP, kept.clone())
        );
        // Turned down a second one too: the first stays turned down.
        let (d, offered) = decide(Some(&kept), &edited, &third, false);
        assert_eq!(d, OFFER);
        let kept = keep(offered);
        assert_eq!(decide(Some(&kept), &edited, &newer, false).0, KEEP);
        assert_eq!(decide(Some(&kept), &edited, &third, false).0, KEEP);
        // The edits undone: the file is what the IDE wrote - but on the old
        // template, which the user chose.
        assert_eq!(decide(Some(&kept), BODY, &newer, false).0, KEEP);
        // A Reset still takes it, and a template not turned down follows.
        assert_eq!(
            decide(Some(&kept), &edited, &newer, true).0,
            template(true, false)
        );
        let fourth = BODY.replace("init() {}", "init() -> i8 { 2 }");
        let (d, on_fourth) = decide(Some(&kept), BODY, &fourth, false);
        assert_eq!(d, template(true, true));
        // ...and the ones turned down stay so through the files the IDE
        // writes meanwhile: switched back to the template the edits were
        // built on, the file follows it - and still not one turned down.
        assert_eq!(on_fourth.declined, kept.declined);
        let (d, on_body) = decide(Some(&kept), BODY, BODY, false);
        assert_eq!(d, template(false, true));
        assert_eq!(decide(Some(&on_body), BODY, &newer, false).0, KEEP);
        // A Reset is the user asking for the template: the slate is clean.
        let (_, reset) = decide(Some(&kept), &edited, &newer, true);
        assert!(reset.declined.is_empty());
        // So is making the file the template by hand: taken after all, it is
        // followed again when an Apply comes back to it.
        let fp = |b: &str| written(b).seen.unwrap();
        let (_, took) = decide(Some(&kept), &newer, &newer, false);
        assert!(!took.declined.contains(&fp(&newer)) && took.declined.contains(&fp(&third)));
        let (_, on_fourth) = decide(Some(&took), &newer, &fourth, false);
        assert_eq!(
            decide(Some(&on_fourth), &fourth, &newer, false).0,
            template(true, true)
        );
    }

    /// A template that loses its head (an ESP file on a chip whose template
    /// has none): an untouched file follows, and the old template's head,
    /// which was ours, does not stay.
    #[test]
    fn an_untouched_file_follows_a_template_that_lost_its_head() {
        let esp = format!("use esp_hal::Blocking;\n{BODY}");
        let headless = newer();
        let (d, s) = decide(Some(&written(&esp)), &esp, &headless, false);
        assert_eq!(d, template(true, false));
        assert_eq!(template_text(&esp, &headless, false), headless);
        assert!(!s.headed);
        // ...and back to a headed one.
        let (d, _) = decide(Some(&s), &headless, &esp, false);
        assert_eq!(d, template(true, false));
    }

    /// Text the user wrote above the block is never dropped by a template
    /// that gains or loses its head: the file is theirs, and offered.
    #[test]
    fn a_user_head_is_never_dropped_by_a_head_that_comes_or_goes() {
        // Headless -> headed: their `use` line cannot ride under the ESP one.
        let mine = format!("use crate::my::Helper;\n\n{BODY}");
        let esp = format!("use esp_hal::Blocking;\n{}", newer());
        let (d, _) = decide(Some(&written(BODY)), &mine, &esp, false);
        assert_eq!(d, OFFER);
        // Headed -> headless, with the ESP head edited and the tail pasted
        // from the new template: the edited head stays theirs.
        let esp_was = format!("use esp_hal::Blocking;\n{BODY}");
        let edited_head = format!("use esp_hal::{{Blocking, mine}};\n{}", newer());
        let (d, _) = decide(Some(&written(&esp_was)), &edited_head, &newer(), false);
        assert_eq!(d, OFFER);
        // ...and with the head deleted it IS the new template.
        let (d, _) = decide(Some(&written(&esp_was)), &newer(), &newer(), false);
        assert_eq!(d, template(false, false));
    }

    #[test]
    fn a_mark_says_when_a_reset_would_change_the_file() {
        let m = Mark::of(BODY, false).unwrap();
        assert!(!m.differs(BODY));
        assert!(m.differs(&edited()));
        // Text above the block of a headless template is a difference too:
        // a Reset writes the template as it is.
        assert!(m.differs(&format!("use stale::Thing;\n{BODY}")));
        assert!(Mark { offered: true, ..m }.differs(BODY));
        assert_eq!(Mark::of("no markers", false), None);
    }

    #[test]
    fn a_project_without_config_files_gets_no_record() {
        let dir = std::env::temp_dir().join(format!("eide_tmpl_none_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &TemplateRecord::default()).unwrap();
        assert!(!dir.join(".rustonchip").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_copies_follow_the_record_and_the_users_stay() {
        let dir = std::env::temp_dir().join(format!("eide_tmpl_side_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = "src/pins/configs/usart1.rs".to_owned();
        let b = "src/pins/configs/i2c1/mod.rs".to_owned();
        let mut r = TemplateRecord::default();
        let offered = Stamp {
            offered: Some(1),
            ..Stamp::default()
        };
        r.files.insert(a.clone(), offered.clone());
        r.files.insert(b.clone(), offered);
        let offers = [(a.clone(), "A".to_owned()), (b.clone(), "B".to_owned())];
        reconcile_sidecars(&dir, &r, &offers).unwrap();
        assert_eq!(std::fs::read_to_string(sidecar(&dir, &a)).unwrap(), "A");
        assert_eq!(std::fs::read_to_string(sidecar(&dir, &b)).unwrap(), "B");
        assert_eq!(
            std::fs::read_to_string(dir.join(SIDECARS).join(".gitignore")).unwrap(),
            "*\n"
        );
        let mine = backup(&dir, &a, "my old text").expect("a free name");
        write_sidecar(&dir, &mine, "my old text").unwrap();
        assert_eq!(
            backup(&dir, &a, "my old text"),
            None,
            "the same copy is not written twice"
        );
        assert!(
            backup(&dir, &a, "other text")
                .unwrap()
                .ends_with("usart1.rs.mine.2.txt")
        );
        // `a`'s offer ended (followed, kept, pruned): its copy goes, the
        // user's copy stays; `b`'s stays as it is.
        r.files.get_mut(&a).unwrap().offered = None;
        reconcile_sidecars(&dir, &r, &[]).unwrap();
        assert!(!sidecar(&dir, &a).exists());
        assert!(mine.exists());
        assert_eq!(std::fs::read_to_string(sidecar(&dir, &b)).unwrap(), "B");
        // ...and a path the record no longer has at all goes too.
        r.files.remove(&b);
        reconcile_sidecars(&dir, &r, &[]).unwrap();
        assert!(!sidecar(&dir, &b).exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What makes a project unsaved although no file's text moved.
    #[test]
    fn record_changes_are_what_a_save_would_write() {
        let mut disk = TemplateRecord::default();
        disk.files.insert("a".into(), written(BODY));
        let mut memory = disk.clone();
        assert!(record_changes(&memory, &disk).is_empty());
        memory.files.get_mut("a").unwrap().seen = Some(9);
        assert!(
            record_changes(&memory, &disk).is_empty(),
            "a sync's own bookkeeping is not a change"
        );
        memory.files.get_mut("a").unwrap().offered = Some(9);
        assert_eq!(record_changes(&memory, &disk), ["a"]);
        // A different offer is a change too, not only a new one.
        disk.files.get_mut("a").unwrap().offered = Some(8);
        assert_eq!(record_changes(&memory, &disk), ["a"]);
        disk.files.get_mut("a").unwrap().offered = Some(9);
        assert!(record_changes(&memory, &disk).is_empty());
        memory.files.get_mut("a").unwrap().declined.insert(9);
        assert_eq!(record_changes(&memory, &disk), ["a"]);
        // A file adopted on an open: what the IDE knows it wrote.
        let mut adopted = TemplateRecord::default();
        adopted.files.insert("b".into(), written(BODY));
        assert_eq!(record_changes(&adopted, &TemplateRecord::default()), ["b"]);
    }
}
