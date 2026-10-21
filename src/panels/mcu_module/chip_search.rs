//! Searching the chip catalogue — the ranking, not the rendering.
//!
//! [`chip_sources`](super::chip_sources) answers "which chips exist on this
//! machine"; this answers "which of them did the user mean". Kept apart from the
//! UI because the interesting part — what ranks above what, and which of two
//! copies of the same part wins — is logic worth testing, and a `TextEdit` is
//! not.
//!
//! # One list, two kinds of hit
//!
//! A chip the IDE already knows and a chip sitting in a vendor file are the same
//! thing to a person typing `F103C8`, so they share one list. They are not the
//! same thing to the IDE — one is selectable now, the other has to be imported
//! first — which is what [`Origin`] carries.
//!
//! # The searchable form
//!
//! Nobody types `STM32`. Ranking therefore matches against the part number both
//! with and without that prefix, so `f103c8` finds `STM32F103C8Tx` and still
//! ranks it below an exact hit. Keys are lowercased ONCE at build time: the
//! search runs over several thousand entries on every keystroke, and lowercasing
//! them per frame is the difference between free and noticeable.

use super::chip_filter::{ChipFilter, Metrics, RowMetrics, Verdict};
use super::chip_sources::{ChipEntry, ChipSource};
use super::stm32_support::Refusal;

/// Where a hit can be acted on.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Origin {
    /// Already a definition in the registry — selecting it is immediate.
    Registry { id: String },
    /// A vendor file on disk. Needs an import before it can be selected.
    Disk {
        /// Index into [`Catalogue::sources`].
        source: usize,
        /// The XML file stem — see [`ChipEntry::file`], which is not the part.
        file: String,
        /// Whether that source can also deliver the clock tree.
        has_clock: bool,
    },
}

impl Origin {
    pub fn is_registry(&self) -> bool {
        matches!(self, Origin::Registry { .. })
    }
}

/// One row of the search list.
///
/// Owned rather than borrowed: a query returns at most a screenful, so the
/// copies are trivial, and the alternative — lifetimes tying every row back to
/// the catalogue — would spread through the UI for nothing.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Hit {
    /// The part number, as the vendor spells it.
    pub name: String,
    pub family: String,
    /// Package, memory and speed, pre-formatted; empty when the source has no
    /// index to read them from.
    pub detail: String,
    pub origin: Origin,
    /// Lower is better — see [`rank`].
    pub rank: u8,
    /// The vendor file this part could be re-imported from, when it is
    /// ALREADY in the registry and still on disk.
    ///
    /// A registry hit shadows the disk one - re-offering an import as the
    /// primary action would be a worse answer to "which chip do I want".
    /// But the vendor data can have changed (a newer CubeMX, a fixed import),
    /// so the row keeps the location for a secondary "re-import" that
    /// overwrites the stored `.ron`.
    pub reimport: Option<Origin>,
    /// What [`ChipFilter`] judges this row on.
    ///
    /// Owned, because a registry row has no catalogue entry of its own: it
    /// inherits the numbers of the vendor file it shadows, and falls back to
    /// whatever its `memory.x` sizes say when there is no such file.
    pub metrics: RowMetrics,
    /// Why no project can be built for this part, when none can: the row is
    /// listed greyed with the reason rather than left out, because a search
    /// that silently drops a part reads exactly like one that does not know
    /// it. On a registry row, the reason its re-import is refused.
    pub refused: Option<Refusal>,
}

/// A registry entry, as the search needs to see it.
///
/// A borrowed row rather than the real `McuDefinition`, so the ranking can be
/// tested without building chip definitions.
///
/// `Hash` so the list can tell a re-imported definition from the old one: the
/// registry keeps its length when a chip is replaced in place.
#[derive(Clone, Copy, Debug, Hash)]
pub struct RegistryRow<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub family: &'a str,
    /// From the definition's `memory.x` sizes — see
    /// [`chip_filter::parse_memory_kb`](super::chip_filter::parse_memory_kb).
    /// The only memory figure a registry entry holds, and absent on chips whose
    /// definition leaves them blank.
    pub flash_kb: Option<u32>,
    pub ram_kb: Option<u32>,
    /// As the definition records it, e.g. `LQFP48`; empty when it does not.
    pub package: &'a str,
    /// From `McuDefinition::max_mhz`.
    pub mhz: Option<u32>,
    /// Usable (non-reserved) pins the definition lists.
    pub io: Option<u32>,
    /// The definition's `cpu` string, e.g. `ARM Cortex-M3`.
    ///
    /// Normalised before it is compared — see [`normalise_core`]. The chip
    /// definitions capitalise it `ARM`, the vendor index `Arm`, and a core
    /// filter that compares them raw matches nothing.
    pub cpu: &'a str,
}

/// A core name in the vendor index's spelling.
///
/// `families.xml` writes `Arm Cortex-M3`; the bundled definitions write
/// `ARM Cortex-M3`. The catalogue's core filter is keyed on the former, so a
/// registry row spelling it the other way would silently never match — and the
/// chip the user definitely has is the one that would vanish.
pub fn normalise_core(cpu: &str) -> String {
    let t = cpu.trim();
    match t.strip_prefix("ARM ") {
        Some(rest) => format!("Arm {rest}"),
        None => t.to_owned(),
    }
}

/// A search, and what it had to leave out.
pub struct Results {
    pub hits: Vec<Hit>,
    /// How many rows matched in total, before `limit` truncated them.
    pub total: usize,
    /// Rows excluded ONLY because their source could not answer an active
    /// facet — an open-pin-data checkout ships part numbers and nothing else.
    ///
    /// Reported rather than absorbed: a filter that quietly deletes a whole
    /// source is indistinguishable from a filter that found nothing.
    pub unknown: usize,
}

/// Every catalogued chip, ready to search.
pub struct Catalogue {
    pub sources: Vec<ChipSource>,
    /// One flat list rather than a list per source: a search crosses all of
    /// them anyway, and flattening keeps the source index on the row where the
    /// de-duplication needs it.
    rows: Vec<Indexed>,
    /// Sources that failed to index, as `(source index, reason)` — surfaced in
    /// the UI instead of silently shrinking the catalogue.
    pub errors: Vec<(usize, String)>,
    /// Indices into `rows`: one per DISTINCT part, the copy that knows the most
    /// about it. This is the set a search actually walks.
    ///
    /// Computed once here rather than per keystroke: the same part number
    /// appears in every source that carries it, and picking between them by
    /// hand — first one wins — silently preferred whichever source happened to
    /// be listed first over whichever one had the data.
    unified: Vec<usize>,
}

/// One winner per part: the source that knows the most about it.
///
/// A clock tree outranks every other fact — it is the difference between a Clock
/// tab that works and one that says the chip has no clock — and beyond that it
/// is simply how many facts the source carries. Ties keep the earlier source,
/// so a re-index cannot reshuffle the catalogue for no reason.
fn pick_unified(rows: &[Indexed], sources: &[ChipSource]) -> Vec<usize> {
    let score = |r: &Indexed| (sources[r.source].has_clock(), r.entry.completeness());
    let mut best: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(rows.len());
    for (ix, row) in rows.iter().enumerate() {
        match best.get(&row.key) {
            Some(&cur) if score(&rows[cur]) >= score(row) => {}
            _ => {
                best.insert(row.key.clone(), ix);
            }
        }
    }
    let mut out: Vec<usize> = best.into_values().collect();
    out.sort_unstable();
    out
}

struct Indexed {
    source: usize,
    entry: ChipEntry,
    /// Judged once, when the catalogue is built: per frame it would be a
    /// feature derivation for every row of every search.
    refused: Option<Refusal>,
    /// `stm32f103c8tx` — lowercased once.
    key: String,
    /// The same without the `stm32` prefix, for the way people actually type.
    short: String,
}

impl Catalogue {
    /// Index every source. Failures are recorded, not propagated: one
    /// unreadable folder must not empty the whole list.
    pub fn build(sources: Vec<ChipSource>) -> Self {
        let mut rows = Vec::new();
        let mut errors = Vec::new();
        for (ix, src) in sources.iter().enumerate() {
            match super::chip_sources::index(src) {
                Ok(entries) => rows.extend(entries.into_iter().map(|entry| {
                    let key = entry.ref_name.to_ascii_lowercase();
                    let short = key.strip_prefix("stm32").unwrap_or(&key).to_owned();
                    Indexed {
                        source: ix,
                        refused: super::stm32_support::refusal(
                            &entry.family,
                            &entry.ref_name,
                            &entry.cores,
                        ),
                        entry,
                        key,
                        short,
                    }
                })),
                Err(e) => errors.push((ix, e)),
            }
        }
        // One winner per part: the source that knows the most about it. A clock
        // tree outranks every other fact — it is the difference between a Clock
        // tab that works and one that says the chip has no clock — and beyond
        // that it is simply how many facts the source carries.
        let unified = pick_unified(&rows, &sources);

        Self {
            sources,
            rows,
            errors,
            unified,
        }
    }

    /// How many DISTINCT parts the sources describe between them.
    ///
    /// Not the sum of the per-source counts: a part in three sources is one
    /// part. This is the number a search can actually return.
    pub fn unified_len(&self) -> usize {
        self.unified.len()
    }

    /// How many parts are catalogued, across every source.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Every catalogued part, for deriving the filter's ranges and facets.
    pub fn entries(&self) -> impl Iterator<Item = &ChipEntry> {
        self.rows.iter().map(|r| &r.entry)
    }

    /// How many parts a given source contributed.
    pub fn count_of(&self, source: usize) -> usize {
        self.rows.iter().filter(|r| r.source == source).count()
    }

    /// The best `limit` matches for `query`, plus how many there were in total.
    ///
    /// Registry chips are searched alongside the disk ones and always win when
    /// both describe the same part — a chip the IDE already has needs no import,
    /// and offering to import it again would be a worse answer to the same
    /// question. Between two disk sources, the one that can also deliver a clock
    /// tree wins, for the same reason: same part, more of it.
    ///
    /// An empty query returns nothing — UNLESS a filter is narrowing the list.
    ///
    /// The catalogue is thousands of parts long and a list that starts by
    /// offering all of them is a list nobody reads. A filter is exactly what
    /// makes such a list readable, which is why it is the one thing that turns
    /// browsing on: "every G4 with three SPIs and 256K" is a question with a
    /// short answer. With no filter set, the behaviour is unchanged.
    pub fn search(
        &self,
        query: &str,
        registry: &[RegistryRow],
        filter: &ChipFilter,
        limit: usize,
    ) -> Results {
        let q = query.trim().to_ascii_lowercase();
        let browsing = q.is_empty();
        if browsing && !filter.is_active() {
            return Results {
                hits: Vec::new(),
                total: 0,
                unknown: 0,
            };
        }
        let active = filter.is_active();
        let mut hits: Vec<Hit> = Vec::new();

        for r in registry {
            let key = r.name.to_ascii_lowercase();
            let short = key.strip_prefix("stm32").unwrap_or(&key);
            // While browsing every row is equally relevant; the sort below is
            // alphabetical anyway.
            let Some(rank) = (if browsing {
                Some(0)
            } else {
                rank(&key, short, &q)
            }) else {
                continue;
            };
            hits.push(Hit {
                name: r.name.to_owned(),
                family: r.family.to_owned(),
                detail: String::new(),
                origin: Origin::Registry {
                    id: r.id.to_owned(),
                },
                rank,
                reimport: None,
                // A registry row is never refused here: a definition that
                // cannot build is blocked in New Project, with the reason,
                // and picking it is how the user gets to read that.
                refused: None,
                // Only what the definition itself holds. The dedup below
                // upgrades this the moment a vendor file for the same part
                // turns up, which is the usual case — the chip got here by
                // being imported from one.
                metrics: RowMetrics {
                    flash_kb: r.flash_kb,
                    ram_kb: r.ram_kb,
                    mhz: r.mhz,
                    io: r.io,
                    package: r.package.to_owned(),
                    cores: if r.cpu.is_empty() {
                        Vec::new()
                    } else {
                        vec![normalise_core(r.cpu)]
                    },
                    ..Default::default()
                },
            });
        }

        let registry_count = hits.len();

        // Where each part already sits, keyed on the name that is ALREADY
        // lowercased on both sides. This replaced a linear scan of the growing
        // hit list, which browsing turned into ~15 million string comparisons
        // per FRAME - 100 ms in a release build, and the search runs every frame.
        let mut seen_at: std::collections::HashMap<String, usize> = hits
            .iter()
            .enumerate()
            .map(|(ix, h)| (h.name.to_ascii_lowercase(), ix))
            .collect();
        // Parts a source could not answer for. Held as keys rather than counted
        // on the spot: the SAME part often arrives again from an indexed source,
        // and a row that is on screen is not a row that was hidden.
        let mut silent: std::collections::HashSet<&str> = std::collections::HashSet::new();

        for row in self.unified.iter().map(|&ix| &self.rows[ix]) {
            let Some(rank) = (if browsing {
                Some(0)
            } else {
                rank(&row.key, &row.short, &q)
            }) else {
                continue;
            };
            let has_clock = self.sources[row.source].has_clock();
            // Same part, already covered? Keep whichever offers more.
            if let Some(&ix) = seen_at.get(row.key.as_str()) {
                let seen = &mut hits[ix];
                let better = match &seen.origin {
                    // Nothing beats a chip that is already here - but keep
                    // WHERE it came from, so the row can offer to refresh it.
                    Origin::Registry { .. } => {
                        // …and take its NUMBERS, which the registry entry does
                        // not carry. Without this a chip already imported would
                        // be excluded by every peripheral filter, which is the
                        // one chip the user certainly has.
                        if row.entry.knows_peripherals() {
                            let own = std::mem::take(&mut seen.metrics.package);
                            seen.metrics = RowMetrics::of(&row.entry);
                            if seen.metrics.package.is_empty() {
                                seen.metrics.package = own;
                            }
                        }
                        let keep = match &seen.reimport {
                            // Same rule as between two disk rows: more data wins.
                            Some(Origin::Disk {
                                has_clock: seen_clock,
                                ..
                            }) => has_clock && !seen_clock,
                            _ => true,
                        };
                        if keep {
                            seen.reimport = Some(disk_hit(row, rank, has_clock).origin);
                            seen.refused = row.refused.clone();
                        }
                        false
                    }
                    Origin::Disk {
                        has_clock: seen_clock,
                        ..
                    } => has_clock && !seen_clock,
                };
                if better {
                    *seen = disk_hit(row, rank, has_clock);
                }
                continue;
            }
            // Judged on the BORROWED entry, before a single string is cloned.
            // Building the hit first and dropping it afterwards meant ~80,000
            // wasted allocations per frame on a catalogue this size.
            if active {
                match filter.matches(&Metrics::of(&row.entry)) {
                    Verdict::Pass => {}
                    Verdict::Unknown => {
                        silent.insert(row.key.as_str());
                        continue;
                    }
                    Verdict::Fail => continue,
                }
            }
            seen_at.insert(row.key.clone(), hits.len());
            hits.push(disk_hit(row, rank, has_clock));
        }

        // The REGISTRY rows are the only ones still unjudged: they were built
        // before the vendor metrics they inherit above could be known. Disk rows
        // were judged on the way in.
        let mut unknown = 0usize;
        if active {
            let mut ix = 0usize;
            hits.retain(|h| {
                let keep = ix >= registry_count
                    || match filter.matches(&h.metrics.view(&h.family)) {
                        Verdict::Pass => true,
                        Verdict::Unknown => {
                            unknown += 1;
                            false
                        }
                        Verdict::Fail => false,
                    };
                ix += 1;
                keep
            });
        }
        // A part one source could not describe, but another could, was never
        // hidden - so it must not be counted as such.
        unknown += silent.iter().filter(|k| !seen_at.contains_key(**k)).count();

        let total = hits.len();
        // Rank first, then alphabetical, so the order is stable and the reason a
        // row is near the top is visible. Browsing gives every row rank 0, which
        // leaves the list plainly alphabetical.
        hits.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.name.cmp(&b.name)));
        hits.truncate(limit);
        Results {
            hits,
            total,
            unknown,
        }
    }
}

fn disk_hit(row: &Indexed, rank: u8, has_clock: bool) -> Hit {
    Hit {
        name: row.entry.ref_name.clone(),
        family: row.entry.family.clone(),
        detail: detail_of(&row.entry),
        origin: Origin::Disk {
            source: row.source,
            file: row.entry.file.clone(),
            has_clock,
        },
        rank,
        reimport: None,
        metrics: RowMetrics::of(&row.entry),
        refused: row.refused.clone(),
    }
}

/// How well `key` answers `q`, or `None` for no match at all.
///
/// `short` is `key` without its `stm32` prefix. The two prefix ranks are
/// separate on purpose: someone who types the full prefix is being specific and
/// should not be outranked by a part that merely contains the same letters.
fn rank(key: &str, short: &str, q: &str) -> Option<u8> {
    if key == q || short == q {
        Some(0)
    } else if key.starts_with(q) {
        Some(1)
    } else if short.starts_with(q) {
        Some(2)
    } else if key.contains(q) {
        Some(3)
    } else {
        None
    }
}

/// The one-line summary shown beside a part, from whatever the source knew.
///
/// The core is in here because it is FILTERABLE: being able to narrow the list
/// to Cortex-M33 parts, and then not being told which core any row has, leaves
/// the filter with no visible effect. It is also the one field where a part can
/// legitimately give two answers - see [`ChipEntry::cores`].
fn detail_of(e: &ChipEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !e.package.is_empty() {
        parts.push(e.package.clone());
    }
    if !e.cores.is_empty() {
        // "Arm Cortex-M4" is four words of which one is the answer, and a
        // dual-core part would otherwise spend half the line saying "Arm"
        // twice.
        parts.push(
            e.cores
                .iter()
                .map(|c| c.trim_start_matches("Arm ").trim_start_matches("Cortex-"))
                .collect::<Vec<_>>()
                .join("+"),
        );
    }
    // `Some(0)` is printed, because an MP1 with no internal flash is a fact
    // worth showing; `None` is skipped, because it is only our ignorance.
    if let Some(v) = e.flash_kb {
        parts.push(format!("{v}K flash"));
    }
    if let Some(v) = e.ram_kb {
        parts.push(format!("{v}K RAM"));
    }
    if let Some(v) = e.mhz {
        parts.push(format!("{v} MHz"));
    }
    parts.join(" · ")
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::chip_sources::SourceKind;
    use super::*;
    use std::path::PathBuf;

    /// A registry row, as the definitions the IDE already has produce one.
    fn reg_row<'a>(id: &'a str, name: &'a str, family: &'a str) -> RegistryRow<'a> {
        RegistryRow {
            id,
            name,
            family,
            flash_kb: None,
            ram_kb: None,
            package: "",
            mhz: None,
            io: None,
            cpu: "",
        }
    }

    /// Search with no filter — the shape these tests were written against, and
    /// the behaviour that must not have changed.
    fn find(c: &Catalogue, q: &str, reg: &[RegistryRow], limit: usize) -> (Vec<Hit>, usize) {
        let r = c.search(q, reg, &ChipFilter::default(), limit);
        (r.hits, r.total)
    }

    fn entry(ref_name: &str, family: &str) -> ChipEntry {
        ChipEntry {
            ref_name: ref_name.into(),
            file: ref_name.into(),
            family: family.into(),
            package: "LQFP48".into(),
            cores: vec!["Arm Cortex-M3".into()],
            flash_kb: Some(64),
            ram_kb: Some(20),
            mhz: Some(72),
            ..Default::default()
        }
    }

    fn source(kind: SourceKind, clock: bool) -> ChipSource {
        ChipSource {
            user_added: false,
            kind,
            chips: PathBuf::from("chips"),
            db: clock.then(|| PathBuf::from("db")),
        }
    }

    /// Builds a catalogue directly, bypassing the filesystem.
    fn catalogue(sources: Vec<ChipSource>, rows: Vec<(usize, ChipEntry)>) -> Catalogue {
        let rows: Vec<Indexed> = rows
            .into_iter()
            .map(|(source, entry)| {
                let key = entry.ref_name.to_ascii_lowercase();
                let short = key.strip_prefix("stm32").unwrap_or(&key).to_owned();
                Indexed {
                    source,
                    refused: super::super::stm32_support::refusal(
                        &entry.family,
                        &entry.ref_name,
                        &entry.cores,
                    ),
                    entry,
                    key,
                    short,
                }
            })
            .collect();
        // The same winner pass the real constructor runs — a test catalogue that
        // skipped it would search a set no user ever sees.
        let unified = pick_unified(&rows, &sources);
        Catalogue {
            sources,
            rows,
            errors: Vec::new(),
            unified,
        }
    }

    /// Nobody types `STM32`, and the ranking has to survive that.
    #[test]
    fn the_prefix_is_optional_but_being_specific_still_pays() {
        let c = catalogue(
            vec![source(SourceKind::CubeMxDb, true)],
            vec![
                (0, entry("STM32F103C8Tx", "stm32f1")),
                (0, entry("STM32F103CBTx", "stm32f1")),
                (0, entry("STM32L4F103Zx", "stm32l4")), // contains, does not start
            ],
        );

        let (hits, total) = find(&c, "f103c8", &[], 10);
        assert_eq!(hits[0].name, "STM32F103C8Tx");
        assert_eq!(hits[0].rank, 2, "matched past the STM32 prefix");
        assert_eq!(total, 1);

        // Typing the whole thing is an exact hit.
        assert_eq!(find(&c, "stm32f103c8tx", &[], 10).0[0].rank, 0);
        // …and so is typing it without the prefix.
        assert_eq!(find(&c, "f103c8tx", &[], 10).0[0].rank, 0);

        // A part that merely CONTAINS the query sorts below one that starts with
        // it, however the user typed it.
        let (hits, _) = find(&c, "f103", &[], 10);
        assert_eq!(
            hits.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(),
            ["STM32F103C8Tx", "STM32F103CBTx", "STM32L4F103Zx"]
        );
        assert_eq!(hits[2].rank, 3, "the contains-match is last");
    }

    /// An empty query must not dump the catalogue.
    #[test]
    fn nothing_typed_means_nothing_offered() {
        let c = catalogue(
            vec![source(SourceKind::CubeMxDb, true)],
            vec![(0, entry("STM32F103C8Tx", "stm32f1"))],
        );
        assert_eq!(find(&c, "", &[], 10).0.len(), 0);
        assert_eq!(find(&c, "   ", &[], 10).0.len(), 0);
    }

    /// The same part in several places is ONE row — the richest one.
    #[test]
    fn duplicates_collapse_to_the_best_copy() {
        // Source 0 has no clock (open-pin-data), source 1 does (CubeMX).
        let c = catalogue(
            vec![
                source(SourceKind::OpenPinData, false),
                source(SourceKind::CubeMxDb, true),
            ],
            vec![
                (0, entry("STM32F103C8Tx", "stm32f1")),
                (1, entry("STM32F103C8Tx", "stm32f1")),
                (0, entry("STM32H563ZITx", "stm32h5")),
            ],
        );

        let (hits, _) = find(&c, "f103c8", &[], 10);
        assert_eq!(hits.len(), 1, "one part, one row: {hits:?}");
        assert_eq!(
            hits[0].origin,
            Origin::Disk {
                source: 1,
                file: "STM32F103C8Tx".into(),
                has_clock: true,
            },
            "the source that can also give a clock wins"
        );

        // Order of discovery must not change the winner.
        let flipped = catalogue(
            vec![
                source(SourceKind::CubeMxDb, true),
                source(SourceKind::OpenPinData, false),
            ],
            vec![
                (0, entry("STM32F103C8Tx", "stm32f1")),
                (1, entry("STM32F103C8Tx", "stm32f1")),
            ],
        );
        let (hits, _) = find(&flipped, "f103c8", &[], 10);
        assert_eq!(hits.len(), 1);
        assert!(matches!(hits[0].origin, Origin::Disk { source: 0, .. }));

        // A part only one source has is unaffected.
        assert_eq!(find(&c, "h563", &[], 10).0.len(), 1);
    }

    /// A chip the IDE already has needs no import, and says so.
    #[test]
    fn a_registry_chip_beats_its_own_vendor_file() {
        let c = catalogue(
            vec![source(SourceKind::CubeMxDb, true)],
            vec![(0, entry("STM32F103C8Tx", "stm32f1"))],
        );
        let reg = [reg_row("stm32f103c8tx", "STM32F103C8Tx", "stm32f1")];

        let (hits, _) = find(&c, "f103c8", &reg, 10);
        assert_eq!(hits.len(), 1, "not offered twice: {hits:?}");
        assert_eq!(
            hits[0].origin,
            Origin::Registry {
                id: "stm32f103c8tx".into()
            }
        );
        assert!(hits[0].origin.is_registry());

        // A registry chip with no vendor file still shows up.
        let reg = [reg_row("esp32c3", "ESP32-C3", "esp32c3")];
        let (hits, _) = find(&c, "esp32", &reg, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "ESP32-C3");
    }

    /// The list is capped, but the count tells the truth about what was cut.
    #[test]
    fn the_cap_is_visible() {
        let rows: Vec<_> = (0..40)
            .map(|i| (0, entry(&format!("STM32F103C{i:02}Tx"), "stm32f1")))
            .collect();
        let c = catalogue(vec![source(SourceKind::CubeMxDb, true)], rows);

        let (hits, total) = find(&c, "f103", &[], 10);
        assert_eq!(hits.len(), 10, "a screenful");
        assert_eq!(total, 40, "out of this many");
        assert_eq!(c.len(), 40);
        assert_eq!(c.count_of(0), 40);
    }

    /// The unified set, against the real sources on this machine.
    ///
    /// Answers the question two CubeMX folders actually raise: is a part that
    /// exists in BOTH of them one entry or two, and does the copy that survives
    /// come from the folder that knows more — or merely from the one listed
    /// first?
    ///
    /// `cargo test --bin rust_on_chip the_unified_set -- --ignored --nocapture`
    #[test]
    #[ignore = "needs real vendor data on this machine"]
    fn the_unified_set_keeps_one_copy_of_each_part() {
        use super::super::chip_sources;

        let c = Catalogue::build(chip_sources::all_sources());
        if c.sources.is_empty() {
            println!("no chip sources on this machine - nothing to check");
            return;
        }
        for (ix, src) in c.sources.iter().enumerate() {
            println!(
                "source {ix}: {} parts, clock={}, user_added={}  {}",
                c.count_of(ix),
                src.has_clock(),
                src.user_added,
                src.chips.display()
            );
        }
        println!("rows={}  unified={}", c.len(), c.unified_len());
        // Which source each surviving part came from. A source contributing a
        // handful of parts nothing else has is usually not a chip catalogue at
        // all — CubeMX keeps clock trees and memory maps in folders whose files
        // are also named STM32*.xml.
        let mut per_source = vec![0usize; c.sources.len()];
        for &ix in &c.unified {
            per_source[c.rows[ix].source] += 1;
        }
        for (ix, n) in per_source.iter().enumerate() {
            println!("  source {ix} won {n} of the unified set");
        }
        assert!(c.unified_len() <= c.len(), "unifying cannot invent parts");
        assert!(c.unified_len() > 0);

        // Every part appears exactly once in the unified set.
        let mut seen = std::collections::HashSet::new();
        for &ix in &c.unified {
            assert!(
                seen.insert(c.rows[ix].key.clone()),
                "duplicate: {}",
                c.rows[ix].key
            );
        }

        // And the part this was all about.
        let wl30: Vec<&Indexed> = c
            .unified
            .iter()
            .map(|&ix| &c.rows[ix])
            .filter(|r| r.key == "stm32wl30kbvx")
            .collect();
        if wl30.is_empty() {
            println!("STM32WL30KBVx is not in these sources - skipped");
            return;
        }
        assert_eq!(wl30.len(), 1, "WL30 must be ONE entry, not one per source");
        let w = wl30[0];
        let copies = c.rows.iter().filter(|r| r.key == w.key).count();
        println!(
            "STM32WL30KBVx: {copies} copy(ies) catalogued, kept the one from source {}              (clock={}, completeness={}/7)",
            w.source,
            c.sources[w.source].has_clock(),
            w.entry.completeness()
        );
        // The winner may not be beaten by any other copy.
        let best = c
            .rows
            .iter()
            .filter(|r| r.key == w.key)
            .map(|r| (c.sources[r.source].has_clock(), r.entry.completeness()))
            .max()
            .unwrap();
        assert_eq!(
            (c.sources[w.source].has_clock(), w.entry.completeness()),
            best,
            "a better copy of WL30 was catalogued and thrown away"
        );
    }

    /// Which families actually get clock code, and which keep the commented
    /// skeleton — asked of the SAME predicate codegen uses, one representative
    /// chip per family, against the real vendor data.
    ///
    /// `cargo test --bin rust_on_chip which_families_generate_clock -- --ignored --nocapture`
    #[test]
    #[ignore = "needs real vendor data on this machine"]
    fn which_families_generate_clock_code() {
        use super::super::chip_sources;
        use super::super::clock::graph::cubemx::graph_for_chip_xml;
        use super::super::clock::model::ClockConfig;
        use super::super::codegen::rcc::tree_drives_clock_code;

        let c = Catalogue::build(chip_sources::all_sources());
        if c.sources.is_empty() {
            println!("no chip sources - nothing to check");
            return;
        }
        // One part per family, preferring a source that ships clock trees:
        // without a tree the generic recipe has nothing to read and every
        // family would look unsupported.
        let mut per_family: std::collections::BTreeMap<String, &Indexed> =
            std::collections::BTreeMap::new();
        for &ix in &c.unified {
            let row = &c.rows[ix];
            if row.entry.family.is_empty() {
                continue;
            }
            let better = per_family.get(&row.entry.family).is_none_or(|cur| {
                !c.sources[cur.source].has_clock() && c.sources[row.source].has_clock()
            });
            if better {
                per_family.insert(row.entry.family.clone(), row);
            }
        }

        let mut parts_of: std::collections::BTreeMap<&str, usize> =
            std::collections::BTreeMap::new();
        for &ix in &c.unified {
            *parts_of
                .entry(c.rows[ix].entry.family.as_str())
                .or_default() += 1;
        }
        let (mut yes, mut no) = (Vec::new(), Vec::new());
        for (family, row) in &per_family {
            let src = &c.sources[row.source];
            let path = src.chip_file(&row.entry);
            let Ok(xml) = std::fs::read_to_string(&path) else {
                println!("{family}: could not read {}", path.display());
                continue;
            };
            let clock = match src.db.as_deref() {
                Some(db) => match graph_for_chip_xml(db, &xml, family) {
                    Ok((gc, _)) => ClockConfig::Graph(gc),
                    Err(_) => ClockConfig::None,
                },
                None => ClockConfig::None,
            };
            if tree_drives_clock_code(family, &clock) {
                yes.push(format!(
                    "{family}	{}",
                    parts_of.get(family.as_str()).copied().unwrap_or(0)
                ));
            } else {
                no.push(format!(
                    "{family}	{}	(e.g. {})",
                    parts_of.get(family.as_str()).copied().unwrap_or(0),
                    row.entry.ref_name
                ));
            }
        }
        println!(
            "
GENERATES clock code ({}):",
            yes.len()
        );
        for f in &yes {
            println!("  {f}");
        }
        println!(
            "
KEEPS THE COMMENTED SKELETON ({}):",
            no.len()
        );
        for f in &no {
            println!("  {f}");
        }
        // How much of the catalogue that actually is. A family with three parts
        // and one with three hundred are not the same problem.
        let affected: Vec<&str> = no.iter().filter_map(|f| f.split('\t').next()).collect();
        let mut count = 0usize;
        for &ix in &c.unified {
            if affected.contains(&c.rows[ix].entry.family.as_str()) {
                count += 1;
            }
        }
        println!(
            "
{count} of {} parts ({:.1} %) keep the skeleton",
            c.unified_len(),
            100.0 * count as f64 / c.unified_len() as f64
        );
        assert!(!yes.is_empty(), "some family must generate clock code");
    }

    /// Dump one family's clock-tree node ids, which is what a codegen recipe
    /// has to read. `EIDE_FAMILY=stm32n6` picks the family.
    ///
    /// `cargo test --bin rust_on_chip dump_clock_tree -- --ignored --nocapture`
    #[test]
    #[ignore = "needs real vendor data on this machine"]
    fn dump_clock_tree_nodes() {
        use super::super::chip_sources;
        use super::super::clock::graph::cubemx::graph_for_chip_xml;

        let want = std::env::var("EIDE_FAMILY").unwrap_or_else(|_| "stm32n6".into());
        let c = Catalogue::build(chip_sources::all_sources());
        let Some(row) = c
            .unified
            .iter()
            .map(|&ix| &c.rows[ix])
            .find(|r| r.entry.family == want && c.sources[r.source].has_clock())
        else {
            println!("no {want} part from a source with clock trees");
            return;
        };
        let src = &c.sources[row.source];
        let xml = std::fs::read_to_string(src.chip_file(&row.entry)).expect("read");
        match graph_for_chip_xml(src.db.as_deref().unwrap(), &xml, &want) {
            Ok((gc, missing)) => {
                println!(
                    "{} ({want}): {} nodes, {} unbound",
                    row.entry.ref_name,
                    gc.graph.nodes.len(),
                    missing.len()
                );
                // `EIDE_NODES=IC1,IC1Div` describes just those, with what feeds
                // them — which is what a codegen recipe has to read.
                let want: Vec<String> = std::env::var("EIDE_NODES")
                    .unwrap_or_default()
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if want.is_empty() {
                    let mut ids: Vec<&str> = gc.graph.nodes.iter().map(|n| n.id.as_str()).collect();
                    ids.sort_unstable();
                    println!("  {}", ids.join(" "));
                } else {
                    for id in &want {
                        match gc.graph.nodes.iter().find(|n| n.id == *id) {
                            None => println!("  {id}: NOT IN THIS TREE"),
                            Some(n) => {
                                let feeds: Vec<String> = gc
                                    .graph
                                    .edges
                                    .iter()
                                    .filter(|e| e.to == n.id)
                                    .map(|e| format!("{}#{}", e.from, e.input))
                                    .collect();
                                let hz = super::super::clock::graph::eval::evaluate(&gc.graph)
                                    .get(id.as_str())
                                    .copied()
                                    .unwrap_or(0);
                                println!(
                                    "  {id}: {:?}  -> {hz} Hz
      fed by: {}",
                                    n.kind,
                                    feeds.join(" ")
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => println!("{} ({want}): no tree - {e}", row.entry.ref_name),
        }
    }

    /// The N6 clock block, emitted from the vendor's own tree.
    ///
    /// `cargo test --bin rust_on_chip n6_clock_block -- --ignored --nocapture`
    #[test]
    #[ignore = "needs real vendor data on this machine"]
    fn n6_clock_block_comes_out_of_the_real_tree() {
        use super::super::chip_sources;
        use super::super::clock::graph::cubemx::graph_for_chip_xml;
        use super::super::clock::model::ClockConfig;
        use super::super::codegen::rcc::{generates_clock_code_for, graph_clock_block};

        let c = Catalogue::build(chip_sources::all_sources());
        let Some(row) = c
            .unified
            .iter()
            .map(|&ix| &c.rows[ix])
            .find(|r| r.entry.family == "stm32n6" && c.sources[r.source].has_clock())
        else {
            println!("no stm32n6 part with a clock tree - skipped");
            return;
        };
        let src = &c.sources[row.source];
        let xml = std::fs::read_to_string(src.chip_file(&row.entry)).expect("read");
        let (gc, _) =
            graph_for_chip_xml(src.db.as_deref().unwrap(), &xml, "stm32n6").expect("tree");
        let clock = ClockConfig::Graph(gc);

        assert!(
            generates_clock_code_for("stm32n6", &clock),
            "N6 must now count as generating clock code"
        );
        let block = graph_clock_block("stm32n6", &clock, false);
        println!(
            "--- {} ---
{block}",
            row.entry.ref_name
        );

        // The subset this emitter promises, and nothing silently missing from it.
        for want in [
            "config.rcc.pll1 = Some(Pll::Oscillator {",
            "source: Pllsel::",
            "divm: Plldivm::DIV",
            "divp1: Pllpdiv::DIV",
            "config.rcc.ic1 = Some(IcConfig {",
            "config.rcc.ic2 = Some(IcConfig {",
            "config.rcc.cpu = CpuClk::",
            "config.rcc.sys = SysClk::",
            "config.rcc.ahb = AhbPrescaler::DIV",
            "config.rcc.apb5 = ApbPrescaler::DIV",
            "embassy_stm32::init(config)",
        ] {
            assert!(
                block.contains(want),
                "missing `{want}` in:
{block}"
            );
        }
        // And NOT the commented skeleton it used to emit.
        assert!(
            !block.contains("has no generated RCC recipe"),
            "still the skeleton:
{block}"
        );
    }

    /// Against every source this machine has. Ignored: needs real vendor data.
    ///
    /// Also times the build, because that cost lands on the frame that opens the
    /// dialog — if indexing were slow, the UI would have to move it off-thread.
    ///
    /// `cargo test -- --ignored the_real_catalogue`
    #[test]
    #[ignore]
    fn the_real_catalogue_answers_a_part_number() {
        use super::super::chip_sources;

        let started = std::time::Instant::now();
        let c = Catalogue::build(chip_sources::all_sources());
        let took = started.elapsed();
        println!(
            "indexed {} parts from {} source(s) in {took:?}",
            c.len(),
            c.sources.len()
        );
        for (ix, e) in &c.errors {
            println!("  source {ix} failed: {e}");
        }
        assert!(!c.is_empty(), "no chips found on this machine");

        let (hits, total) = find(&c, "f103c8", &[], 20);
        println!("f103c8 -> {total} hit(s): {:?}", hits.first());
        let blue_pill = hits
            .iter()
            .find(|h| h.name == "STM32F103C8Tx")
            .expect("the blue-pill part");
        assert_eq!(blue_pill.family, "stm32f1");
        assert_eq!(
            hits.iter().filter(|h| h.name == "STM32F103C8Tx").count(),
            1,
            "one row per part, even with several sources"
        );
    }

    /// Whatever the source knew, formatted; whatever it did not, absent.
    #[test]
    fn the_detail_line_skips_what_is_unknown() {
        assert_eq!(
            detail_of(&entry("STM32F103C8Tx", "stm32f1")),
            "LQFP48 · M3 · 64K flash · 20K RAM · 72 MHz"
        );

        // A dual-core part says both, in one field rather than two.
        let mut h7 = entry("STM32H747XIHx", "stm32h7");
        h7.cores = vec!["Arm Cortex-M7".into(), "Arm Cortex-M4".into()];
        assert!(detail_of(&h7).contains("M7+M4"), "got {:?}", detail_of(&h7));
        // A listing-derived entry knows only the part number.
        let bare = ChipEntry {
            ref_name: "STM32H563ZITx".into(),
            ..Default::default()
        };
        assert_eq!(detail_of(&bare), "");
    }

    /// The same part in two sources is ONE part, and the copy that survives is
    /// the one that knows more — not the one whose source was listed first.
    ///
    /// This is what two CubeMX folders on one machine actually look like: both
    /// carry the part, and picking by position threw away flash, RAM and the
    /// peripheral table roughly half the time.
    /// A part no project can be built for is in the list, with its reason,
    /// and a filter that the part passes still shows it: refused is not
    /// hidden, and it is not counted among the parts a filter could not judge.
    #[test]
    fn a_refused_part_is_listed_not_hidden() {
        let cat = catalogue(
            vec![source(SourceKind::CubeMxDb, true)],
            vec![
                (0, entry("STM32WL30KBVx", "stm32wl3")),
                (0, entry("STM32G071CBTx", "stm32g0")),
                (0, entry("STM32MP157CAAx", "stm32mp1")),
            ],
        );
        let (hits, total) = find(&cat, "wl30", &[], 10);
        assert_eq!(total, 1);
        assert!(
            matches!(&hits[0].refused, Some(Refusal::NoHal { feature, .. }) if feature == "stm32wl30kb"),
            "{:?}",
            hits[0].refused
        );
        let (hits, _) = find(&cat, "g071", &[], 10);
        assert_eq!(hits[0].refused, None);
        let (hits, _) = find(&cat, "mp157", &[], 10);
        assert!(matches!(hits[0].refused, Some(Refusal::CortexA { .. })));

        // The fixture's core, so the part passes the filter.
        let mut filter = ChipFilter::default();
        filter.cores.insert("Arm Cortex-M3".to_owned());
        let r = cat.search("wl30", &[], &filter, 10);
        assert_eq!(r.hits.len(), 1, "a filter it passes does not hide it");
        assert!(r.hits[0].refused.is_some());
        assert_eq!(r.unknown, 0);
    }

    #[test]
    fn a_part_in_two_sources_is_one_part_and_keeps_the_better_copy() {
        let thin = ChipEntry {
            ref_name: "STM32WL30KBVx".into(),
            file: "STM32WL30KBVx".into(),
            family: "stm32wl3".into(),
            ..Default::default()
        };
        let rich = entry("STM32WL30KBVx", "stm32wl3");
        assert!(
            rich.completeness() > thin.completeness(),
            "the fixture is the point"
        );

        // Thin source listed FIRST, so "first wins" would pick the wrong one.
        let cat = catalogue(
            vec![
                source(SourceKind::OpenPinData, false),
                source(SourceKind::CubeMxDb, true),
            ],
            vec![(0, thin), (1, rich)],
        );

        assert_eq!(cat.len(), 2, "both copies are still catalogued");
        assert_eq!(cat.unified_len(), 1, "but they are ONE part");

        let (hits, total) = find(&cat, "wl30", &[], 10);
        assert_eq!(total, 1, "and a search returns it once");
        assert_eq!(hits.len(), 1);
        // The surviving row is the one with the data.
        assert!(
            hits[0].detail.contains("64"),
            "kept the thin copy: {:?}",
            hits[0].detail
        );
    }

    /// A part that is BOTH in the registry and on disk keeps its disk
    /// location, so the row can offer to overwrite the stored definition.
    /// Before this it was thrown away and the only way to refresh a chip was
    /// to delete its `.ron` by hand.
    #[test]
    fn an_already_added_chip_remembers_where_to_re_import_from() {
        let cat = catalogue(
            vec![source(SourceKind::CubeMxDb, true)],
            vec![(0, entry("STM32F358CCTx", "stm32f3"))],
        );
        let registry = [reg_row("stm32f358cc", "STM32F358CCTx", "stm32f3")];
        let (hits, _) = find(&cat, "358cc", &registry, 10);
        let hit = hits
            .iter()
            .find(|h| h.name.eq_ignore_ascii_case("STM32F358CCTx"))
            .expect("the part is in both");
        // The registry still wins the row itself - selecting must not import.
        assert!(hit.origin.is_registry());
        // …but the vendor file is remembered.
        assert!(
            matches!(hit.reimport, Some(Origin::Disk { .. })),
            "no re-import offered: {:?}",
            hit.reimport
        );
    }

    /// A bundled chip must be FILTERABLE, not merely visible.
    ///
    /// Before the definitions carried their own numbers, every registry row was
    /// `Unknown` on every numeric facet — so the moment a filter was set, the
    /// chips the IDE ships were the first to disappear.
    #[test]
    fn a_bundled_chip_is_judged_on_its_own_numbers() {
        use crate::panels::mcu_module::chip_filter::Bounds;

        let cat = catalogue(vec![source(SourceKind::CubeMxDb, true)], vec![]);
        let esp = RegistryRow {
            id: "esp32c6",
            name: "ESP32-C6",
            family: "esp32c6",
            // No flash: it is an external SPI part, chosen by the module.
            flash_kb: None,
            ram_kb: Some(512),
            package: "",
            mhz: Some(160),
            io: Some(31),
            cpu: "RISC-V 32-bit",
        };
        let bounds = Bounds {
            ram_kb: (2, 4200),
            mhz: (24, 600),
            io: (6, 200),
            ..Bounds::default()
        };

        // Asked for something it has.
        let mut f = ChipFilter::new(bounds);
        f.ram_kb = (256, 4200);
        f.mhz = (100, 600);
        let r = cat.search("esp32", &[esp], &f, 10);
        assert_eq!(r.hits.len(), 1, "hidden despite fitting: {r:?}", r = r.hits);
        assert_eq!(r.unknown, 0, "counted as unanswerable");

        // Asked for something it does not have.
        let mut slow = ChipFilter::new(bounds);
        slow.mhz = (200, 600);
        assert!(cat.search("esp32", &[esp], &slow, 10).hits.is_empty());

        // And FLASH is genuinely unknown, not zero — an ESP32 die cannot answer
        // that question, so the row is set aside and counted rather than failed.
        let mut flashy = ChipFilter::new(Bounds {
            flash_kb: (0, 4096),
            ..bounds
        });
        flashy.flash_kb = (64, 4096);
        let r = cat.search("esp32", &[esp], &flashy, 10);
        assert!(r.hits.is_empty());
        assert_eq!(r.unknown, 1, "silently dropped instead of counted");
    }

    /// The two spellings of the same core.
    #[test]
    fn a_definition_core_is_normalised_to_the_vendor_spelling() {
        assert_eq!(normalise_core("ARM Cortex-M3"), "Arm Cortex-M3");
        assert_eq!(normalise_core("Arm Cortex-M4"), "Arm Cortex-M4");
        assert_eq!(normalise_core("RISC-V 32-bit"), "RISC-V 32-bit");
        assert_eq!(normalise_core("  ARM Cortex-M0+ "), "Arm Cortex-M0+");
    }

    /// …and that it actually makes a core filter match a bundled chip.
    #[test]
    fn a_bundled_arm_chip_matches_the_vendor_core_filter() {
        let cat = catalogue(vec![source(SourceKind::CubeMxDb, true)], vec![]);
        let mut row = reg_row("stm32f103c8t6", "STM32F103C8Tx", "stm32f1");
        row.cpu = "ARM Cortex-M3";
        let mut f = ChipFilter::default();
        f.cores.insert("Arm Cortex-M3".to_owned());
        assert_eq!(
            cat.search("f103", &[row], &f, 10).hits.len(),
            1,
            "the definition says ARM, the filter says Arm"
        );
    }

    /// A registry chip with no vendor file behind it offers nothing, rather
    /// than a button that would fail.
    #[test]
    fn a_registry_only_chip_offers_no_re_import() {
        let cat = catalogue(vec![source(SourceKind::CubeMxDb, true)], vec![]);
        let registry = [reg_row("mystery1", "MYSTERYCHIP1", "custom")];
        let (hits, _) = find(&cat, "mystery", &registry, 10);
        assert_eq!(hits.len(), 1);
        assert!(hits[0].reimport.is_none());
    }
}
