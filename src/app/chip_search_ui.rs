//! The chip search field in the New Project dialog.
//!
//! Picking a chip used to mean one of two things: choosing from the handful the
//! IDE already knew, or knowing that `STM32F103C8Tx` lives in a file called
//! `STM32F103C(8-B)Tx.xml` and finding it in a file dialog. This is the third
//! way — type the part number.
//!
//! The ranking lives in [`chip_search`](crate::panels::mcu_module::chip_search)
//! and the catalogue in
//! [`chip_sources`](crate::panels::mcu_module::chip_sources); this module is the
//! field, the list and the thread that keeps indexing off the frame.
//!
//! # Why indexing is off-thread
//!
//! Cataloguing a CubeMX installation is ~2800 parts out of a 4.8 MB index, which
//! measured 563 ms in a debug build. Doing that on the frame that opens the
//! dialog would be half a second of frozen window, so it runs on a worker and
//! the field says "indexing…" until it lands.
//!
//! # Two halves
//!
//! The field, the filters and the sources stay in the dialog; the matches are a
//! tall column of their own, LEFT of it. Inside the dialog the list was a
//! scroll area inside the body's scroll area, capped at 200px under a filter
//! panel several times that height: with Filters open it showed three rows,
//! and the wheel was split between the two. On a screen with no room beside
//! the dialog the column goes back inside it — as a sibling of the body, never
//! nested in it.

use eframe::egui;
use egui_phosphor::regular as ph;

use super::chip_filter_ui::{self, Facets};
use crate::panels::mcu_module::chip_filter::{self, ChipFilter};
use crate::panels::mcu_module::chip_search::{self, Catalogue, Hit, Origin, RegistryRow};
use crate::panels::mcu_module::chip_sources;
use crate::panels::mcu_module::mcu_def::McuDefinition;

/// How many matches the list holds.
///
/// Only the rows on screen are laid out (`show_rows`), so this bounds how far
/// the list scrolls, not what a frame costs — and not the search either, which
/// sorts every match before it truncates. Past a few screenfuls a narrower
/// query is still the better answer.
const MAX_ROWS: usize = 200;

/// The New Project window's content width, pinned.
///
/// Left to its content, a non-resizable window keeps the widest it has ever
/// been — and egui persists that, so one wide row once was enough to make the
/// dialog cover most of the screen for good. The results column is also placed
/// against the dialog's left edge, which must not move with what the body holds.
pub(super) const DIALOG_W: f32 = 480.0;

/// The New Project window's OUTER width: [`DIALOG_W`] plus the window frame.
///
/// egui reads a `Window`'s min/max width as its outer size (see
/// [`window_size`](super::helpers::window_size)), while [`DIALOG_W`] is what
/// the content gets - the Filters rows and the in-dialog results box are sized
/// to it. The pin and the "does the column fit beside it" check both come from
/// here, so they cannot drift apart again.
pub(super) fn dialog_outer_w(ctx: &egui::Context) -> f32 {
    DIALOG_W + super::helpers::window_size::chrome(ctx).x
}
/// Between the results column and the dialog.
const LIST_GAP: f32 = 6.0;
/// Between the results column and the screen's left and bottom edges.
const LIST_EDGE: f32 = 10.0;
/// Narrower than this, a row is its part number and tag with no detail left, so
/// the column goes back inside the dialog instead.
const MIN_LIST_W: f32 = 420.0;
/// The column covers whatever panel is under it; this is as much as it takes.
const MAX_LIST_W: f32 = 640.0;

/// The search field's state, owned by the app.
#[derive(Default)]
pub(super) struct ChipSearchState {
    pub query: String,
    /// The DRAFT the Filters panel edits. Nothing acts on it until Apply.
    pub filter: ChipFilter,
    /// What the list is actually showing.
    ///
    /// Separate from `filter` because a slider fires a change on every frame it
    /// is dragged, and each change is a fresh search over several thousand
    /// parts. Committing on a button turns a drag from hundreds of searches
    /// into one.
    applied: ChipFilter,
    /// The last search, with the inputs that produced it.
    ///
    /// `search` runs from the frame callback, so without this it re-runs at the
    /// refresh rate even when nothing has changed — an Apply button alone would
    /// have stopped the filter from CHANGING every frame while leaving it
    /// re-evaluated every frame.
    ///
    /// The registry is in the key by CONTENT, not by length: a re-import
    /// replaces a definition in place, so the length never moves while the
    /// package or pin count the filters judge that chip on can.
    cached: Option<(String, ChipFilter, u64, chip_search::Results)>,
    /// What the catalogue offers to filter by — derived once it lands.
    facets: Facets,
    catalogue: Option<Catalogue>,
    /// The worker's channel while indexing is in flight.
    pending: Option<std::sync::mpsc::Receiver<Catalogue>>,
    /// Result of the last source change, shown under the source list.
    pub note: String,
}

impl ChipSearchState {
    /// Whether the list has a question to answer: a part number typed, or a
    /// filter applied. Without either it lists nothing, so it needs no room.
    pub(super) fn is_asking(&self) -> bool {
        !self.query.trim().is_empty() || self.applied.is_active()
    }

    /// Kick off indexing once, and adopt the result when it arrives.
    fn poll(&mut self, ctx: &egui::Context) {
        if self.catalogue.is_some() {
            return;
        }
        match &self.pending {
            None => {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    // A closed channel just means the dialog went away.
                    let _ = tx.send(Catalogue::build(chip_sources::all_sources()));
                });
                self.pending = Some(rx);
                ctx.request_repaint();
            }
            Some(rx) => match rx.try_recv() {
                Ok(c) => {
                    // The dialog can be drawn before indexing finishes, so the
                    // sliders start on placeholder bounds. Re-spanning them here
                    // is what stops an untouched filter from reading as active
                    // the moment the real catalogue arrives.
                    self.facets = Facets::of(&c);
                    self.filter.rebound(self.facets.bounds);
                    self.applied.rebound(self.facets.bounds);
                    self.cached = None;
                    self.catalogue = Some(c);
                    self.pending = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint(),
                // The worker died; stop waiting on it rather than spin forever.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.pending = None,
            },
        }
    }

    /// Re-index — after the set of sources changes.
    fn reload(&mut self) {
        self.catalogue = None;
        self.pending = None;
        // The facets describe the OLD set of sources; keeping them would offer
        // filters for peripherals no remaining source mentions.
        self.facets = Facets::default();
        self.cached = None;
    }
}

/// What a click on the list asked for, resolved after the borrows end.
enum Action {
    /// A chip the registry already has.
    Select(String),
    /// A vendor file to import, the source it came from (which decides whether
    /// the clock tree comes with it), and the part that was actually asked for —
    /// a range file yields several chips, and the importer selects the LAST,
    /// which is rarely the one that was clicked.
    Import {
        path: std::path::PathBuf,
        source: chip_sources::ChipSource,
        part: String,
    },
}

/// Where to GET chip data, for a machine that does not have it yet.
///
/// The three links are not interchangeable, and the difference is the whole
/// reason this is spelled out rather than being one "download" link: ST
/// publishes the per-chip files openly, but the clock trees
/// (`db/plugins/clock`) ship only inside CubeMX. Someone who clones the open
/// repo expecting a Clock tab gets pins and a blank diagram.
///
/// Collapsed once a source with clock trees is present, because at that point
/// this is answered — and open otherwise, because at that point it is the most
/// useful thing on the screen.
fn source_links(ui: &mut egui::Ui, have_clock: bool) {
    if !have_clock {
        ui.label(
            egui::RichText::new(format!(
                "{}  Nothing here can supply a clock tree — chips will import with pins only.",
                ph::WARNING
            ))
            .size(10.5)
            .color(egui::Color32::from_rgb(225, 185, 60)),
        );
    }
    egui::CollapsingHeader::new(
        egui::RichText::new("Where to get chip data")
            .size(10.5)
            .color(egui::Color32::GRAY),
    )
    .id_salt("chip_data_links")
    .default_open(!have_clock)
    .show(ui, |ui| {
        let note = |ui: &mut egui::Ui, text: &str| {
            ui.label(
                egui::RichText::new(text)
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
            );
        };

        ui.hyperlink_to(
            egui::RichText::new(format!("{} STM32CubeMX", ph::CPU)).size(11.0),
            "https://www.st.com/en/development-tools/stm32cubemx.html",
        );
        note(
            ui,
            "The only official source of clock trees. Install it, then add its `db` folder \
             above — nothing else needs to be run.",
        );
        ui.add_space(3.0);

        ui.hyperlink_to(
            egui::RichText::new(format!("{} STM32_open_pin_data", ph::GIT_BRANCH)).size(11.0),
            "https://github.com/STMicroelectronics/STM32_open_pin_data",
        );
        note(
            ui,
            "ST's open chip data — pins, packages and memory, no clock trees. A git clone; \
             add its `mcu` folder above.",
        );
        ui.add_space(3.0);

        ui.hyperlink_to(
            egui::RichText::new(format!("{} esden/stm32cube-database", ph::GIT_BRANCH)).size(11.0),
            "https://github.com/esden/stm32cube-database",
        );
        note(
            ui,
            "A community mirror of the CubeMX database, clock trees included — for a machine \
             with no CubeMX. It is an OLDER snapshot: the newest parts and RCC revisions are \
             missing, so prefer a real installation where you have one.",
        );
    });
}

/// Whether the results column fits BESIDE the dialog.
///
/// Decided from the width the dialog is GOING to have, not the one it had last
/// frame: that one is remembered across sessions, and a stale wide value would
/// open the dialog in one mode and flip it to the other a frame later.
pub(super) fn side_list_fits(content_w: f32, dialog_outer_w: f32) -> bool {
    content_w - dialog_outer_w - LIST_GAP - LIST_EDGE >= MIN_LIST_W
}

/// Where the results column goes: left of `dialog`, from its top down to
/// `bottom`, never wider than [`MAX_LIST_W`].
pub(super) fn list_rect(content: egui::Rect, dialog: egui::Rect, bottom: f32) -> egui::Rect {
    let right = dialog.left() - LIST_GAP;
    let left = (content.left() + LIST_EDGE).max(right - MAX_LIST_W);
    egui::Rect::from_min_max(
        egui::pos2(left, dialog.top()),
        egui::pos2(right.max(left), (bottom - LIST_EDGE).max(dialog.top())),
    )
}

/// The id of the results column's layer.
pub(super) const LIST_AREA: &str = "new_project_chip_list";

/// The results column's own surface, at `rect`, stacked with `dialog` — the
/// New Project window's layer.
///
/// A SUBLAYER of the dialog, which egui re-seats directly above its parent at
/// the end of every frame, so the pair stack as one window: above the panels
/// and their floating overlays, under any window raised over the dialog (the
/// New MCU form opened from it), and a click on the column does not lift it
/// past its parent. It was on the Background order first, under every Area -
/// and the editor's error list is a Middle-order Area that then covered the
/// rows and took their clicks.
pub(super) fn list_surface<R>(
    ctx: &egui::Context,
    rect: egui::Rect,
    dialog: egui::LayerId,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let id = egui::Id::new(LIST_AREA);
    // Every frame: egui drops the parent links once it has applied them.
    ctx.set_sublayer(dialog, egui::LayerId::new(egui::Order::Middle, id));
    let frame = egui::Frame::window(&ctx.global_style());
    let inner = (rect.size() - frame.total_margin().sum()).max(egui::Vec2::ZERO);
    egui::Area::new(id)
        .order(egui::Order::Middle)
        // The default LEFT_TOP pivot keeps the position independent of the
        // size, which an Area only learns at the END of a frame: any other
        // pivot places it from last frame's size.
        .fixed_pos(rect.min)
        .default_size(rect.size())
        // Already inside the screen. Constraining would re-place it from last
        // frame's size too, and shift it for a frame after the window resizes.
        .constrain(false)
        .show(ctx, |ui| {
            frame
                .show(ui, |ui| {
                    ui.set_min_size(inner);
                    ui.set_max_size(inner);
                    ui.shrink_clip_rect(ui.max_rect());
                    add(ui)
                })
                .inner
        })
        .inner
}

/// The results column inside the dialog, for a screen with no room beside it.
///
/// A fixed box, so the dialog's height does not follow the number of matches.
pub(super) fn results_host<R>(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(size, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_size(size);
        ui.set_max_size(size);
        add(ui)
    })
    .inner
}

/// What a click on one row asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowClick {
    /// The part number: select the chip, importing it first if it is new.
    Pick,
    /// The small re-import button beside an "already added" row.
    Reimport,
}

/// The height every row is laid out at.
///
/// `show_rows` finds the rows on screen by arithmetic, so a row taller than
/// this would slide the list out of step with its own scrollbar.
///
/// Measured on real buttons in an invisible child, not added up from the
/// style: the button frame puts its stroke on top of `button_padding` (the
/// text style says 20.375px, the rows came out 21), and the download glyph
/// comes from another font.
fn row_height(ui: &mut egui::Ui) -> f32 {
    let mut probe = ui.new_child(egui::UiBuilder::new().sizing_pass().invisible());
    let icon = egui::Button::new(format!("{}  STM32", ph::DOWNLOAD_SIMPLE)).selected(true);
    let icon = probe.add(icon).rect.height();
    let plain = probe.add(egui::Button::new("STM32")).rect.height();
    icon.max(plain).max(ui.spacing().interact_size.y)
}

/// One match: the part number as the button, then what it is and what it can
/// deliver.
///
/// The detail goes LAST and is truncated, with the full text on hover: it is
/// the only part of a row whose length varies with the data, and a row wider
/// than the column would be cut off at the edge instead.
fn result_row(ui: &mut egui::Ui, hit: &Hit, selected: bool, row_h: f32) -> Option<RowClick> {
    let size = egui::vec2(ui.available_width(), row_h);
    ui.allocate_ui_with_layout(
        size,
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_min_size(size);
            let mut click = None;
            let known = hit.origin.is_registry();
            // The part number is the button: one click does the obvious thing,
            // whichever kind of row it is.
            let label = if known {
                egui::RichText::new(&hit.name)
            } else {
                egui::RichText::new(format!("{}  {}", ph::DOWNLOAD_SIMPLE, hit.name))
            };
            let btn = ui.add(
                egui::Button::new(label)
                    .selected(selected)
                    .min_size(egui::vec2(170.0, 0.0)),
            );
            let btn = if known {
                btn.on_hover_text("Use this chip")
            } else {
                btn.on_hover_text("Import this chip from its vendor file and select it")
            };
            if btn.clicked() {
                click = Some(RowClick::Pick);
            }
            ui.label(
                egui::RichText::new(&hit.family)
                    .size(10.5)
                    .color(egui::Color32::from_rgb(150, 158, 172)),
            );
            // What this row can actually deliver — said before the click, not
            // discovered after it.
            let (tag, color) = match &hit.origin {
                Origin::Registry { .. } => {
                    ("already added", egui::Color32::from_rgb(120, 200, 120))
                }
                Origin::Disk {
                    has_clock: true, ..
                } => ("pins + clock", egui::Color32::from_rgb(120, 190, 200)),
                Origin::Disk {
                    has_clock: false, ..
                } => ("pins only", egui::Color32::from_rgb(225, 185, 60)),
            };
            ui.label(egui::RichText::new(tag).size(10.0).color(color));
            // A chip already in the registry can still be refreshed from its
            // vendor file: the data may have moved on (a newer CubeMX) or the
            // import itself may have been fixed since. Secondary, never the
            // primary action - the row's job is still "pick this chip".
            if hit.reimport.is_some()
                && ui
                    .small_button(ph::ARROW_CLOCKWISE)
                    .on_hover_text(
                        "Re-import from the vendor file, overwriting the stored definition",
                    )
                    .clicked()
            {
                click = Some(RowClick::Reimport);
            }
            if !hit.detail.is_empty() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&hit.detail)
                            .size(10.5)
                            .color(egui::Color32::GRAY),
                    )
                    .truncate(),
                );
            }
            click
        },
    )
    .inner
}

/// The matches, in a scroll area that takes all the height it is given.
///
/// Returns the scroll output rather than just the click, so a test can read
/// the list's real size and offset.
fn results_list(
    ui: &mut egui::Ui,
    hits: &[Hit],
    pending: Option<&str>,
) -> egui::scroll_area::ScrollAreaOutput<Option<(usize, RowClick)>> {
    let row_h = row_height(ui);
    egui::ScrollArea::vertical()
        .id_salt("chip_search_results")
        // Not shrunk: a column that follows its row count would jump at every
        // keystroke, and leave the rest of it for the panels behind.
        .auto_shrink([false, false])
        .show_rows(ui, row_h, hits.len(), |ui, rows| {
            let mut clicked = None;
            for ix in rows {
                let hit = &hits[ix];
                let selected = match &hit.origin {
                    Origin::Registry { id } => pending == Some(id.as_str()),
                    Origin::Disk { .. } => false,
                };
                if let Some(c) = result_row(ui, hit, selected, row_h) {
                    clicked = Some((ix, c));
                }
            }
            clicked
        })
}

/// What a click on `hit` means, once the sources can name the file.
fn row_action(sources: &[chip_sources::ChipSource], hit: &Hit, click: RowClick) -> Option<Action> {
    let import = |origin: &Origin| match origin {
        Origin::Disk { source, file, .. } => sources.get(*source).map(|src| Action::Import {
            path: src.chips.join(format!("{file}.xml")),
            source: src.clone(),
            part: hit.name.clone(),
        }),
        Origin::Registry { .. } => None,
    };
    match click {
        RowClick::Pick => match &hit.origin {
            Origin::Registry { id } => Some(Action::Select(id.clone())),
            disk => import(disk),
        },
        // From `reimport`, never `origin`: on exactly these rows `origin` is
        // the registry entry, and reading it would quietly turn a re-import
        // into a select.
        RowClick::Reimport => hit.reimport.as_ref().and_then(import),
    }
}

/// The registry, in the shape the ranking wants.
fn registry_rows(defs: &[McuDefinition]) -> Vec<RegistryRow<'_>> {
    defs.iter()
        .map(|d| RegistryRow {
            id: d.id.as_str(),
            name: d.display_name.as_str(),
            family: d.family.as_str(),
            // A registry entry carries no vendor row of its own, so these
            // come from the definition itself. The search upgrades them from
            // the vendor file whenever one exists.
            flash_kb: chip_filter::parse_memory_kb(&d.project.flash_size),
            // `sram_kb` first: an Espressif part has no `memory.x` of ours
            // to read a RAM size out of, because esp-hal writes its own.
            ram_kb: d
                .sram_kb
                .or_else(|| chip_filter::parse_memory_kb(&d.project.ram_size)),
            package: d.package.as_str(),
            mhz: d.max_mhz,
            // Pins the definition marks usable. Counted rather than stored:
            // it is the same number the Pins canvas draws, and a second copy
            // of it could only ever disagree.
            io: Some(
                [&d.pins.top, &d.pins.bottom, &d.pins.left, &d.pins.right]
                    .iter()
                    .flat_map(|s| s.iter())
                    .filter(|p| !p.reserved)
                    .count() as u32,
            ),
            cpu: d.cpu.as_str(),
        })
        .collect()
}

/// The last search, re-run only when an input moved.
fn search_cached<'c>(
    cached: &'c mut Option<(String, ChipFilter, u64, chip_search::Results)>,
    cat: &Catalogue,
    query: &str,
    applied: &ChipFilter,
    registry: &[RegistryRow],
) -> &'c chip_search::Results {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    registry.hash(&mut h);
    let fingerprint = h.finish();
    let fresh = cached
        .as_ref()
        .is_some_and(|(q, f, r, _)| q == query && f == applied && *r == fingerprint);
    if !fresh {
        *cached = Some((
            query.to_owned(),
            applied.clone(),
            fingerprint,
            cat.search(query, registry, applied, MAX_ROWS),
        ));
    }
    &cached.as_ref().expect("just filled").3
}

impl super::AppIde {
    /// The half of the chip picker that stays in the dialog: the search field,
    /// the filters and the source list.
    ///
    /// Drawn BEFORE [`Self::show_chip_results`] in every layout, so a keystroke
    /// or an Apply reaches the list in the same frame.
    pub(super) fn show_chip_search_controls(&mut self, ui: &mut egui::Ui) {
        self.chip_search.poll(ui.ctx());

        let mut add_folder = false;
        // Deferred like `add_folder`: acting on it needs `&mut self`, which the
        // closure drawing the rows does not have.
        let mut remove_source: Option<std::path::PathBuf> = None;

        let chip_search = &mut self.chip_search;
        ui.horizontal(|ui| {
            ui.label("Search:");
            // No debounce and nothing to invalidate: the list is recomputed from
            // the pre-lowercased keys every frame, which is a linear scan over a
            // few thousand short strings.
            ui.add(
                egui::TextEdit::singleline(&mut chip_search.query)
                    .hint_text("part number, e.g. F103C8")
                    .desired_width(200.0),
            );
            if !chip_search.query.is_empty()
                && ui
                    .small_button(ph::X)
                    .on_hover_text("Clear the search")
                    .clicked()
            {
                chip_search.query.clear();
            }
            if chip_search.catalogue.is_none() {
                ui.spinner();
                ui.label(
                    egui::RichText::new("indexing…")
                        .size(10.5)
                        .color(egui::Color32::GRAY),
                );
            }
        });

        // The fields apart: the filter panel WRITES `filter` while the source
        // list READS `catalogue`, and they live in the same struct.
        let ChipSearchState {
            filter,
            applied,
            facets,
            catalogue,
            note,
            ..
        } = chip_search;
        let Some(cat) = catalogue.as_ref() else {
            return;
        };

        chip_filter_ui::show_filters(ui, filter, applied, facets);

        // ── Sources ───────────────────────────────────────────────────────────
        // Collapsed by default: this is reference material (which vendor
        // folders are indexed, and where to get more), not something you
        // touch while picking a chip.
        ui.add_space(4.0);
        egui::CollapsingHeader::new(
            egui::RichText::new(format!("{} Sources", ph::DATABASE))
                .size(10.5)
                .color(egui::Color32::GRAY),
        )
        .id_salt("new_project_sources")
        .default_open(false)
        .show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} Sources:", ph::DATABASE))
                        .size(10.5)
                        .color(egui::Color32::GRAY),
                );
                if ui
                    .small_button("Add a folder…")
                    .on_hover_text(
                        "A STM32CubeMX installation (chips + clock trees) or an \
                         STM32_open_pin_data checkout (chips only)",
                    )
                    .clicked()
                {
                    add_folder = true;
                }
            });
            if cat.sources.is_empty() {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  No STM32CubeMX installation found — add a folder to search vendor data.",
                        ph::WARNING
                    ))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(225, 185, 60)),
                );
            }
            // The number that actually answers "how many chips can I search":
            // not the sum of the rows above, because a part in two sources is
            // one part, and the copy that survives is the one that knows more.
            if cat.sources.len() > 1 {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {} distinct parts in all sources together — searched and filtered as one set",
                        ph::FUNNEL,
                        cat.unified_len()
                    ))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(150, 200, 160)),
                );
                ui.add_space(2.0);
            }
            for (ix, src) in cat.sources.iter().enumerate() {
                let (what, color) = if src.has_clock() {
                    ("pins + clock trees", egui::Color32::from_rgb(120, 190, 200))
                } else {
                    ("pins only", egui::Color32::from_rgb(225, 185, 60))
                };
                ui.horizontal(|ui| {
                    // Removal is only honest for a folder the user added: an
                    // auto-detected install would be found again next launch, so
                    // a button promising to remove it would be a lie.
                    if src.user_added {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(ph::X)
                                        .size(10.0)
                                        .color(egui::Color32::from_rgb(220, 110, 90)),
                                )
                                .small()
                                .frame(false),
                            )
                            .on_hover_text("Forget this folder")
                            .clicked()
                        {
                            remove_source = Some(src.chips.clone());
                        }
                    } else {
                        ui.add_enabled(
                            false,
                            egui::Button::new(egui::RichText::new(ph::X).size(10.0))
                                .small()
                                .frame(false),
                        )
                        .on_disabled_hover_text(
                            "Found automatically — uninstall it to stop it being used",
                        );
                    }
                    ui.label(
                        egui::RichText::new(format!(
                            "{} — {} parts,",
                            src.kind.label(),
                            cat.count_of(ix)
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_gray(140)),
                    );
                    ui.label(egui::RichText::new(what).size(10.0).color(color));
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(src.chips.display().to_string())
                            .size(9.5)
                            .monospace()
                            .color(egui::Color32::from_gray(110)),
                    )
                    .truncate(),
                );
            }
            source_links(ui, cat.sources.iter().any(|s| s.has_clock()));
        });

        for (ix, err) in &cat.errors {
            let where_ = cat
                .sources
                .get(*ix)
                .map(|s| s.chips.display().to_string())
                .unwrap_or_default();
            ui.label(
                egui::RichText::new(format!("{}  {where_}: {err}", ph::X_CIRCLE))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(220, 120, 90)),
            );
        }
        if !note.is_empty() {
            ui.label(
                egui::RichText::new(&*note)
                    .size(10.5)
                    .color(egui::Color32::from_rgb(150, 200, 160)),
            );
        }

        // ── Deferred: both of these need `&mut self` ─────────────────────────
        if add_folder {
            self.add_chip_source();
        }
        if let Some(path) = remove_source {
            self.remove_chip_source(&path);
        }
    }

    /// The other half: what matched, wherever the dialog put it.
    ///
    /// The last import's outcome is repeated at the top, next to the row that
    /// was clicked: the dialog's own copy is a screen width away beside it, and
    /// scrolled out of sight under the Filters inside it.
    pub(super) fn show_chip_results(&mut self, ui: &mut egui::Ui) {
        let mut action: Option<Action> = None;
        let mut add_folder = false;

        // Borrow the fields apart, so the list can read the registry while
        // writing the selection.
        let Self {
            chip_search,
            mcu_registry,
            pending_mcu_id,
            mcu_import_status,
            ..
        } = self;
        let asked = chip_search.is_asking();
        let ChipSearchState {
            query,
            applied,
            cached,
            catalogue,
            ..
        } = chip_search;

        if let Some(msg) = mcu_import_status.as_deref() {
            // One line: the report can run to four paragraphs, and the whole of
            // it is on hover and in the dialog.
            ui.add(
                egui::Label::new(
                    egui::RichText::new(msg)
                        .size(10.5)
                        .color(super::dialogs::import_status_colour(msg)),
                )
                .truncate(),
            );
        }

        // `poll` belongs to the controls, which are always drawn first; this
        // only says why there is nothing yet. The host pinned the column's size,
        // so the catalogue arriving does not move anything.
        let Some(cat) = catalogue.as_ref() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(
                    egui::RichText::new("indexing the vendor data…")
                        .size(10.5)
                        .color(egui::Color32::GRAY),
                );
            });
            return;
        };

        let registry = registry_rows(mcu_registry);
        let found = search_cached(cached, cat, query, applied, &registry);
        let (hits, total) = (&found.hits, found.total);

        // Said here as well as under Sources: a column this tall inviting a
        // search is a promise, and with no vendor data only the chips already
        // added can keep it.
        if cat.sources.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(format!(
                        "{}  No chip data folder: only chips already added can be found.",
                        ph::WARNING
                    ))
                    .size(10.5)
                    .color(egui::Color32::from_rgb(225, 185, 60)),
                );
                if ui
                    .small_button("Add a folder…")
                    .on_hover_text(
                        "A STM32CubeMX installation (chips + clock trees) or an STM32_open_pin_data checkout (chips only)",
                    )
                    .clicked()
                {
                    add_folder = true;
                }
            });
        }

        // Typing is no longer the only way to ask a question: with a filter set,
        // an empty query means "show me everything that fits".
        if !asked {
            ui.label(
                egui::RichText::new(format!(
                    "{}  Type a part number or apply a filter: the matching chips are listed here.",
                    ph::MAGNIFYING_GLASS
                ))
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
        } else if hits.is_empty() {
            // WHICH of the two narrowed it to nothing, because they are
            // fixed differently: one by typing less, the other by a Clear
            // button the user may not have noticed is on.
            let why = match (query.trim().is_empty(), applied.active_count()) {
                (_, 0) => "No chip matches that.".to_owned(),
                (true, n) => format!("No chip matches those {n} filter(s)."),
                (false, n) => format!("No chip matches that, with {n} filter(s) on."),
            };
            ui.label(
                egui::RichText::new(format!("{}  {why}", ph::MAGNIFYING_GLASS))
                    .size(11.0)
                    .color(egui::Color32::GRAY),
            );
        } else if total > hits.len() {
            let how = if query.trim().is_empty() {
                "narrow the filters or type a part number"
            } else {
                "keep typing to narrow it"
            };
            ui.label(
                egui::RichText::new(format!("{} of {total} matches — {how}", hits.len()))
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            );
        }

        // Said out loud rather than absorbed. An open-pin-data checkout ships
        // part numbers and nothing else, so ANY filter hides all of it - and a
        // source vanishing without a word looks exactly like a filter that
        // found nothing. Above the list, not under it: the list takes every
        // pixel of height that is left.
        if asked && found.unknown > 0 {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(
                        "{}  {} more hidden: their source has no index, so nothing is known about their memory or peripherals.",
                        ph::INFO,
                        found.unknown
                    ))
                    .size(10.0)
                    .color(egui::Color32::from_rgb(150, 158, 172)),
                )
                .wrap(),
            );
        }

        if asked && !hits.is_empty() {
            ui.add_space(2.0);
            if let Some((ix, click)) = results_list(ui, hits, pending_mcu_id.as_deref()).inner {
                action = row_action(&cat.sources, &hits[ix], click);
            }
        }

        // ── Deferred: these need `&mut self` ────────────────────────────────
        if add_folder {
            self.add_chip_source();
        }
        match action {
            Some(Action::Select(id)) => self.pending_mcu_id = Some(id),
            Some(Action::Import { path, source, part }) => {
                self.import_searched_chip(&path, &source, &part)
            }
            None => {}
        }
    }

    /// Ask for a folder, keep it if it is a usable source, and re-index.
    /// Forget a folder the user added.
    ///
    /// Only the remembered list is touched — nothing on disk is deleted, and an
    /// auto-detected install is unaffected because it was never in that list.
    fn remove_chip_source(&mut self, chips: &std::path::Path) {
        let before = chip_sources::saved_paths();
        // The saved path may be the folder the user PICKED (a CubeMX root),
        // while `chips` is the `db/mcu` inside it — so match on either being a
        // prefix of the other rather than on equality, which would silently
        // remove nothing.
        let after: Vec<std::path::PathBuf> = before
            .iter()
            .filter(|p| !(chips.starts_with(p.as_path()) || p.starts_with(chips)))
            .cloned()
            .collect();
        if after.len() == before.len() {
            self.chip_search.note = format!(
                "{}  {} is not one of the remembered folders",
                ph::WARNING,
                chips.display()
            );
            return;
        }
        self.chip_search.note = match chip_sources::save_paths(&after) {
            Ok(()) => format!("{}  Forgot {}", ph::CHECK, chips.display()),
            Err(e) => format!("{}  Could not forget that folder: {e}", ph::WARNING),
        };
        self.chip_search.reload();
    }

    fn add_chip_source(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Add a chip data folder")
            .pick_folder()
        else {
            return;
        };
        let Some(src) = chip_sources::from_path(&path) else {
            self.chip_search.note = format!(
                "{}  No STM32 chip XMLs in {} — pick a CubeMX `db` folder or an \
                 STM32_open_pin_data checkout.",
                ph::WARNING,
                path.display()
            );
            return;
        };
        let mut saved = chip_sources::saved_paths();
        if !saved.contains(&path) {
            saved.push(path.clone());
        }
        self.chip_search.note = match chip_sources::save_paths(&saved) {
            Ok(()) => format!(
                "{}  Added {} ({})",
                ph::CHECK,
                src.kind.label(),
                if src.has_clock() {
                    "pins + clock trees"
                } else {
                    "pins only"
                }
            ),
            Err(e) => format!("{}  Could not remember that folder: {e}", ph::WARNING),
        };
        self.chip_search.reload();
    }

    /// Import the file a search hit names — pins AND clock — then select the
    /// part that was CLICKED.
    ///
    /// The source is passed on rather than dropped: it is what says whether a
    /// clock tree can come with the pins, and it is the row the user chose, not
    /// a guess. The bulk importer selects the last chip it saved, which for a
    /// range file (`STM32F103C(8-B)Tx.xml` holds both the C8 and the CB) is the
    /// wrong one about half the time. It has no way to know better — it takes
    /// files, not part numbers — so the correction belongs here.
    fn import_searched_chip(
        &mut self,
        path: &std::path::Path,
        source: &chip_sources::ChipSource,
        part: &str,
    ) {
        self.import_stm32_pin_data_from(std::slice::from_ref(&path.to_path_buf()), Some(source));
        if let Some(def) = self
            .mcu_registry
            .iter()
            .find(|d| d.display_name.eq_ignore_ascii_case(part))
        {
            self.pending_mcu_id = Some(def.id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    //! Layout claims, checked by laying the real widgets out headless.
    //!
    //! Every test dresses the context like the app: the theme changes the window
    //! margin, the button padding and the item spacing, so a bare context
    //! measures a different dialog - the first plan for this column did exactly
    //! that, and got the window frame 4px wrong.

    use super::*;
    use crate::panels::mcu_module::chip_filter::RowMetrics;
    use crate::panels::mcu_module::chip_sources::{ChipSource, SourceKind};

    fn list_layer() -> egui::LayerId {
        egui::LayerId::new(egui::Order::Middle, egui::Id::new(LIST_AREA))
    }

    fn app_ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::app::helpers::apply_dark_theme(&ctx);
        let mut fonts = egui::FontDefinitions::default();
        egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
        ctx.set_fonts(fonts);
        ctx
    }

    fn input(screen: egui::Vec2, pass: usize, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, screen)),
            time: Some(pass as f64 / 60.0),
            events,
            ..Default::default()
        }
    }

    fn disk(source: usize) -> Origin {
        Origin::Disk {
            source,
            file: "STM32F103C(8-B)Tx".into(),
            has_clock: true,
        }
    }

    fn hit(name: &str, detail: &str, origin: Origin, reimport: Option<Origin>) -> Hit {
        Hit {
            name: name.into(),
            family: "stm32wba".into(),
            detail: detail.into(),
            origin,
            rank: 0,
            reimport,
            metrics: RowMetrics::default(),
        }
    }

    /// The widest rows real data makes, one far wider, and a registry row with
    /// the extra re-import button.
    fn hits(n: usize) -> Vec<Hit> {
        let long = "VFBGA264 · M55 · 0K flash · 4096K RAM · 800 MHz · ".repeat(4);
        (0..n)
            .map(|i| match i % 3 {
                0 => hit(
                    "STM32WBA65RIVx",
                    "UFQFPN48_SMPS_USB · M33 · 2048K flash · 512K RAM · 100 MHz",
                    disk(0),
                    None,
                ),
                1 => hit("STM32N647A0HxQ", &long, disk(0), None),
                _ => hit(
                    "STM32F103C8Tx",
                    "LQFP48 · M3 · 64K flash · 20K RAM · 72 MHz",
                    Origin::Registry {
                        id: "stm32f103c8".into(),
                    },
                    Some(disk(0)),
                ),
            })
            .collect()
    }

    struct Measured {
        dialog: egui::Rect,
        list: egui::Rect,
        inner: egui::Rect,
        content: egui::Vec2,
        offset: egui::Vec2,
        body_w: f32,
        row_h: f32,
        /// What the Pins and Clock canvases now ask before they zoom.
        canvas_has_pointer: bool,
        /// Where the Filters header was painted, to click it open.
        filters_header: Option<egui::Rect>,
    }

    impl Default for Measured {
        fn default() -> Self {
            Self {
                dialog: egui::Rect::NOTHING,
                list: egui::Rect::NOTHING,
                inner: egui::Rect::NOTHING,
                content: egui::Vec2::ZERO,
                offset: egui::Vec2::ZERO,
                body_w: 0.0,
                row_h: 0.0,
                canvas_has_pointer: false,
                filters_header: None,
            }
        }
    }

    /// One pass of the New Project layout: `before` (what the app draws earlier
    /// in the frame), the real window builder around the real Filters panel,
    /// the dialog's action-row tail, the column placed as the dialog places it,
    /// then `after`.
    fn dialog_pass(
        ctx: &egui::Context,
        raw: egui::RawInput,
        hits: &[Hit],
        before: &mut dyn FnMut(&mut egui::Ui),
        after: &mut dyn FnMut(&mut egui::Ui),
    ) -> Measured {
        let mut m = Measured::default();
        let mut f = ChipFilter::default();
        let mut applied = ChipFilter::default();
        let facets = Facets::default();
        let out = crate::headless::run_ui(ctx, raw, |ui| {
            before(ui);
            let content = ui.ctx().content_rect();
            let dialog = crate::app::dialogs::new_project_window(ui.ctx())
                .show(ui.ctx(), |ui| {
                    let body = egui::ScrollArea::vertical()
                        .id_salt("new_project_body")
                        .max_height(content.height() * 0.70)
                        .show(ui, |ui| {
                            chip_filter_ui::show_filters(ui, &mut f, &mut applied, &facets);
                        });
                    m.body_w = body.content_size.x;
                    ui.separator();
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let _ = ui.button("New Project");
                        ui.add_space(8.0);
                        let _ = ui.button("Cancel");
                    });
                    ui.add_space(4.0);
                })
                .expect("nothing closes the dialog here")
                .response;
            m.dialog = dialog.rect;
            m.list = list_rect(content, m.dialog, content.bottom());
            let out = list_surface(ui.ctx(), m.list, dialog.layer_id, |ui| {
                m.row_h = row_height(ui);
                results_list(ui, hits, None)
            });
            m.inner = out.inner_rect;
            m.content = out.content_size;
            m.offset = out.state.offset;
            after(ui);
            m.canvas_has_pointer = ui.rect_contains_pointer(ui.max_rect());
        });
        m.filters_header = painted_text(&out.shapes, "Filters");
        m
    }

    /// Where a text was painted. The header's id sits under a child `Ui` with
    /// an automatic id, so clicking where it is drawn is the reliable way in.
    fn painted_text(shapes: &[egui::epaint::ClippedShape], needle: &str) -> Option<egui::Rect> {
        fn walk(s: &egui::Shape, needle: &str) -> Option<egui::Rect> {
            match s {
                egui::Shape::Text(t) if t.galley.text().contains(needle) => {
                    Some(t.visual_bounding_rect())
                }
                egui::Shape::Vec(v) => v.iter().find_map(|s| walk(s, needle)),
                _ => None,
            }
        }
        shapes.iter().find_map(|c| walk(&c.shape, needle))
    }

    fn click(at: egui::Pos2, pressed: bool) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }

    /// Leave a wide New Project window in memory first, the way an older build
    /// did: egui keeps the widest a non-resizable window has been.
    fn plant_wide_window(ctx: &egui::Context, screen: egui::Vec2) {
        for pass in 0..3 {
            let _ = crate::headless::run_ui(ctx, input(screen, pass, vec![]), |ui| {
                egui::Window::new("New Project")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::RIGHT_TOP, [20.0, 10.0])
                    .show(ui.ctx(), |ui| {
                        ui.allocate_space(egui::vec2(1150.0, 10.0));
                    });
            });
        }
    }

    fn frame_w(ctx: &egui::Context) -> f32 {
        egui::Frame::window(&ctx.global_style())
            .total_margin()
            .sum()
            .x
    }

    #[test]
    fn the_list_is_a_tall_column_left_of_the_dialog() {
        for screen in [egui::vec2(1366.0, 768.0), egui::vec2(1920.0, 1040.0)] {
            let ctx = app_ctx();
            plant_wide_window(&ctx, screen);
            let many = hits(MAX_ROWS);
            let few = hits(3);
            let mut seen = Vec::new();
            let mut header = None;
            for pass in 3..45 {
                let rows: &[Hit] = match pass {
                    ..35 => &many,
                    35..40 => &few,
                    _ => &[],
                };
                // Open Filters the way a user does: its widest rows are what
                // the pinned width has to hold.
                let events = match (pass, header) {
                    (6, Some(at)) => click(at, true),
                    (7, Some(at)) => click(at, false),
                    _ => vec![],
                };
                let m = dialog_pass(
                    &ctx,
                    input(screen, pass, events),
                    rows,
                    &mut |_| {},
                    &mut |_| {},
                );
                // Last frame's position: the first frame after the wide
                // window still draws the dialog where the old width put it.
                header = m.filters_header.map(|r| r.center());
                seen.push(m);
            }
            let m = &seen[28];
            let frame = frame_w(&ctx);
            let sp = ctx.global_style().spacing.item_spacing.y;
            let visible = ((m.inner.height() + sp) / (m.row_h + sp)).floor();
            eprintln!(
                "{screen:?}: dialog {:?} list {:?} inner {:?} content {:?} body_w {} row_h {} visible {visible} frame {frame}",
                m.dialog, m.list, m.inner, m.content, m.body_w, m.row_h
            );

            assert!(
                (m.dialog.width() - (DIALOG_W + frame)).abs() < 0.5,
                "{screen:?}: the dialog is {}px wide, not the pinned {} - the old wide width came back",
                m.dialog.width(),
                DIALOG_W + frame
            );
            assert!((m.dialog.right() - screen.x).abs() < 0.5, "flush right");
            assert!(
                m.body_w > 300.0,
                "Filters did not open ({}px), so the width check below proves nothing",
                m.body_w
            );
            assert!(
                m.body_w <= DIALOG_W + 0.5,
                "the open Filters panel needs {}px, more than the pinned {DIALOG_W}",
                m.body_w
            );
            assert!(
                m.list.right() <= m.dialog.left() - LIST_GAP + 0.01,
                "left of the dialog"
            );
            assert!(m.list.left() >= LIST_EDGE - 0.01);
            assert!(
                (m.list.width() - MAX_LIST_W).abs() < 0.5,
                "both screens have room for all of it"
            );
            assert!(
                (m.list.bottom() - (screen.y - LIST_EDGE)).abs() < 0.5,
                "down to the bottom"
            );
            assert!(
                m.inner.height() >= m.list.height() - frame - 1.0,
                "the rows get the column's whole height: {} of {}",
                m.inner.height(),
                m.list.height()
            );
            assert!(
                visible >= ((screen.y - 60.0) / (m.row_h + sp)).floor(),
                "{screen:?}: only {visible} rows visible"
            );
            assert!(
                m.content.x <= m.inner.width() + 0.5,
                "a row is {}px wide in a {}px column: the detail is not truncating",
                m.content.x,
                m.inner.width()
            );
            // The column does not follow its row count: 200 rows, 3, none.
            for later in &seen[8..] {
                assert_eq!(later.list, m.list, "the column moved");
                assert_eq!(later.inner, m.inner, "the list inside it resized");
            }
            let inside = m.list.left_top() + egui::vec2(20.0, 20.0);
            assert_eq!(
                ctx.layer_id_at(inside),
                Some(list_layer()),
                "the point is on the column's own layer"
            );
        }
    }

    #[test]
    fn a_window_opened_over_the_list_stays_on_top_of_it() {
        let screen = egui::vec2(1366.0, 768.0);
        let ctx = app_ctx();
        let rows = hits(30);
        // The form's layer as egui reports it: how a title becomes an `Id` is
        // egui's business (0.35 hashes it as an `Option<Cow<str>>`).
        let mut form_layer = None;
        let mut form = |ui: &mut egui::Ui| {
            form_layer = egui::Window::new("New MCU")
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .fixed_size([680.0, 560.0])
                .show(ui.ctx(), |ui| {
                    ui.label("form");
                })
                .map(|r| r.response.layer_id);
        };
        let mut pass = 0;
        let mut run = |events: Vec<egui::Event>| {
            let m = dialog_pass(
                &ctx,
                input(screen, pass, events),
                &rows,
                &mut |_| {},
                &mut form,
            );
            pass += 1;
            m
        };
        for _ in 0..5 {
            run(vec![]);
        }
        let m = run(vec![]);
        let form_center = egui::pos2(screen.x / 2.0, screen.y / 2.0);
        assert!(
            m.list.contains(form_center),
            "the two must overlap for this to test anything"
        );
        // Click the column where the form does not cover it - which raises an
        // Area to the top of its ORDER, and must not lift it over a window.
        let on_list = m.list.left_top() + egui::vec2(12.0, 30.0);
        let modifiers = egui::Modifiers::default();
        run(vec![
            egui::Event::PointerMoved(on_list),
            egui::Event::PointerButton {
                pos: on_list,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers,
            },
        ]);
        run(vec![egui::Event::PointerButton {
            pos: on_list,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers,
        }]);
        run(vec![]);
        let form_layer = form_layer.expect("the form was shown");
        assert_eq!(form_layer.order, egui::Order::Middle);
        assert_eq!(ctx.layer_id_at(form_center), Some(form_layer));
        assert_eq!(ctx.layer_id_at(on_list), Some(list_layer()));
    }

    #[test]
    fn a_panel_overlay_never_covers_the_list() {
        // The editor's error list: an Area of its own on the Middle order,
        // on screen before the dialog opens and drawn earlier in every frame.
        let screen = egui::vec2(1366.0, 768.0);
        let ctx = app_ctx();
        let rows = hits(30);
        let overlay_rect =
            egui::Rect::from_min_size(egui::pos2(395.0, 50.0), egui::vec2(272.0, 150.0));
        let mut overlay = |ui: &mut egui::Ui| {
            egui::Area::new(egui::Id::new("editor_error_list"))
                .fixed_pos(overlay_rect.min)
                .order(egui::Order::Middle)
                .show(ui.ctx(), |ui| {
                    ui.set_min_size(overlay_rect.size());
                });
        };
        for pass in 0..3 {
            let _ = crate::headless::run_ui(&ctx, input(screen, pass, vec![]), |ui| overlay(ui));
        }
        let mut m = Measured::default();
        for pass in 3..9 {
            m = dialog_pass(
                &ctx,
                input(screen, pass, vec![]),
                &rows,
                &mut overlay,
                &mut |_| {},
            );
        }
        let both = overlay_rect.center();
        assert!(
            m.list.contains(both),
            "the two must overlap for this to test anything"
        );
        assert_eq!(
            ctx.layer_id_at(both),
            Some(list_layer()),
            "the overlay covers the rows, and would take their clicks"
        );
    }

    #[test]
    fn the_wheel_over_the_list_never_reaches_the_canvas_behind_it() {
        let screen = egui::vec2(1366.0, 768.0);
        let ctx = app_ctx();
        let rows = hits(60);
        let mut last = Measured::default();
        for pass in 0..4 {
            last = dialog_pass(
                &ctx,
                input(screen, pass, vec![]),
                &rows,
                &mut |_| {},
                &mut |_| {},
            );
        }
        let over = last.list.center();
        let mut max_offset = 0.0_f32;
        for pass in 4..160 {
            let wheel = egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, -120.0),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::default(),
            };
            let m = dialog_pass(
                &ctx,
                input(screen, pass, vec![egui::Event::PointerMoved(over), wheel]),
                &rows,
                &mut |_| {},
                &mut |_| {},
            );
            assert!(
                !m.canvas_has_pointer,
                "pass {pass}: the panel under the list thinks the pointer is its own, so a wheel would zoom it"
            );
            max_offset = max_offset.max(m.offset.y);
            last = m;
        }
        let end = last.content.y - last.inner.height();
        assert!(end > 0.0, "60 rows must overflow the column");
        assert!(
            (max_offset - end).abs() < 1.0,
            "the wheel scrolled the list to {max_offset}, its end is {end}"
        );
        // And the gate is not simply always shut: beside the column, the
        // panel does own the pointer.
        let beside = egui::pos2(last.list.left() / 2.0, screen.y / 2.0);
        let m = dialog_pass(
            &ctx,
            input(screen, 200, vec![egui::Event::PointerMoved(beside)]),
            &rows,
            &mut |_| {},
            &mut |_| {},
        );
        assert!(m.canvas_has_pointer);
    }

    #[test]
    fn the_column_moves_into_the_dialog_when_there_is_no_room() {
        let outer = 498.0;
        assert!(side_list_fits(
            outer + LIST_GAP + LIST_EDGE + MIN_LIST_W,
            outer
        ));
        assert!(!side_list_fits(
            outer + LIST_GAP + LIST_EDGE + MIN_LIST_W - 0.5,
            outer
        ));

        let content = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1366.0, 768.0));
        let dialog = egui::Rect::from_min_max(egui::pos2(868.0, 10.0), egui::pos2(1366.0, 400.0));
        assert_eq!(
            list_rect(content, dialog, 744.0),
            egui::Rect::from_min_max(egui::pos2(222.0, 10.0), egui::pos2(862.0, 734.0))
        );
        // Less room than MAX_LIST_W: the column stops at the screen edge.
        let narrow = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 500.0));
        let d = egui::Rect::from_min_max(egui::pos2(202.0, 10.0), egui::pos2(700.0, 300.0));
        assert_eq!(list_rect(narrow, d, 500.0).left(), LIST_EDGE);
    }

    #[test]
    fn every_row_is_the_height_the_list_skips_by() {
        // Windows display scaling included: rects round to PHYSICAL pixels.
        for scale in [1.0, 1.25, 1.5] {
            let ctx = app_ctx();
            ctx.set_pixels_per_point(scale);
            let rows = hits(3);
            let mut heights = Vec::new();
            let mut row_h = 0.0;
            for _ in 0..2 {
                heights.clear();
                let _ = crate::headless::run_ui(&ctx, Default::default(), |ui| {
                    ui.set_max_width(MAX_LIST_W);
                    row_h = row_height(ui);
                    for h in &rows {
                        let r = ui.scope(|ui| result_row(ui, h, true, row_h));
                        heights.push(r.response.rect.height());
                    }
                });
            }
            for (h, hit) in heights.iter().zip(&rows) {
                assert!(
                    (h - row_h).abs() < 0.01,
                    "x{scale} {}: laid out {h}px high, but `show_rows` counts {row_h}px",
                    hit.name
                );
            }
        }
    }

    #[test]
    fn a_reimport_click_imports_and_never_selects() {
        let sources = vec![ChipSource {
            kind: SourceKind::CubeMxDb,
            chips: std::path::PathBuf::from("db/mcu"),
            db: Some(std::path::PathBuf::from("db")),
            user_added: false,
        }];
        let rows = hits(3);
        let (disk_row, registry_row) = (&rows[0], &rows[2]);

        let Some(Action::Import { path, part, .. }) =
            row_action(&sources, registry_row, RowClick::Reimport)
        else {
            panic!("re-import of an added chip must import from its vendor file");
        };
        assert_eq!(
            path,
            std::path::Path::new("db/mcu").join("STM32F103C(8-B)Tx.xml")
        );
        assert_eq!(part, "STM32F103C8Tx");

        assert!(matches!(
            row_action(&sources, registry_row, RowClick::Pick),
            Some(Action::Select(id)) if id == "stm32f103c8"
        ));
        assert!(matches!(
            row_action(&sources, disk_row, RowClick::Pick),
            Some(Action::Import { part, .. }) if part == "STM32WBA65RIVx"
        ));
        // A source gone since the search ran asks for nothing.
        assert!(row_action(&[], disk_row, RowClick::Pick).is_none());
    }
}
