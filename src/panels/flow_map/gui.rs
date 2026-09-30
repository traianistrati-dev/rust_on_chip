//! Draw a laid-out [`FlowLayout`] — the only part of the Flow tab that touches
//! egui.
//!
//! The view controls are deliberately the SAME as the Structure tab's, because
//! they sit next to each other in the Project group and a diagram that pans
//! differently from the diagram beside it is a diagram the user fights: auto-fit
//! as the base scale, mouse wheel and Ctrl+± on top, background drag to pan,
//! Ctrl+0 to re-centre.
//!
//! "All — whole file" is the exception, and on purpose: it is a list, not a
//! diagram, so there the wheel scrolls (see [`outline`]).

use super::compose::{Composed, FRAME_PAD, FRAME_TITLE_H, Frame, card_tip};
use super::layout::{Edge, EdgeKind, FlowLayout, Placed};
use super::parse::{Element, ElementKind, EntryKind, FileModel, Shape};
use eframe::egui;

/// Session view state for the Flow tab.
pub struct FlowView {
    /// User zoom over the auto-fit base (1.0 = the whole chart fits).
    pub zoom: f32,
    /// View offset from centred, in screen px.
    pub pan: egui::Vec2,
    /// The scale actually drawn last frame, so the toolbar can say so.
    pub last_scale: f32,
    /// What is open, by its [`Element::key`] - a function, a container or a
    /// type. A key, not an index, which would silently point at something
    /// else the moment the file is edited, and not a bare name, which cannot
    /// tell two same-named functions apart.
    ///
    /// Kept while [`Self::all`] is on, so leaving the whole-file view goes back
    /// to what was open.
    pub selected: String,
    /// "All — whole file": every element of the file instead of one chart.
    /// Its own flag rather than a magic value of `selected`, which a function
    /// named `All` would collide with.
    pub all: bool,
    /// For the whole file or a container: every element DRAWN (functions as
    /// flowcharts, the rest as cards) instead of listed. "Implementation"
    /// against "Outline".
    pub implementation: bool,
    /// Zoom over the base scale of whatever is drawn as a page - the whole
    /// file, a container, a type's card, a function too tall to fit. The base
    /// is the width-fit kept between 60 % and 100 %, so 1.0 is that base, not
    /// always the exact width. Every open and every return to All start it at
    /// 1.0 again ([`Self::reset_page`]).
    pub all_zoom: f32,
    /// The scale the page was drawn at last frame (0 = not drawn yet), which
    /// its pixel scroll offset belongs to.
    pub page_scale: f32,
    /// Scroll the page to this point (canvas units) next frame - to the top
    /// on an open or a file switch, to its function on a clicked call.
    pub pending_scroll: Option<egui::Vec2>,
    /// The page's scroll offset last frame, in screen px - what a zoom about
    /// the pointer is computed from.
    pub last_offset: egui::Vec2,
    /// Last frame's decision that the open function is too tall to read fitted
    /// whole, and so is drawn as a page. Kept so the decision has a margin
    /// before it flips back (a view that alternates between two layouts from
    /// one frame to the next is unreadable), and so the toolbar, drawn first,
    /// can report the scale of the view that is really on screen.
    pub chart_paged: bool,
    /// Height of the legend and hint rows above a chart, measured last frame:
    /// what the chart gets is what is left under them, and a narrow panel wraps
    /// them onto more rows than any constant would allow for.
    pub header_h: f32,
    /// Put the list of the next scope drawn back at its top (a new file).
    pub lists_to_top: bool,
    /// The editor's caret moved onto this 1-based line: bring what draws it
    /// into sight - its row in a list, its box on a page - unless enough of
    /// it is on screen already. Taken by the next frame whatever that frame
    /// shows, so a view with nothing to scroll (a chart fitted whole) does not
    /// act on it later, in a view the reader has since scrolled themselves.
    pub reveal_line: Option<usize>,
}

impl Default for FlowView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan: egui::Vec2::ZERO,
            last_scale: 1.0,
            selected: String::new(),
            all: false,
            implementation: false,
            all_zoom: 1.0,
            page_scale: 0.0,
            pending_scroll: None,
            last_offset: egui::Vec2::ZERO,
            chart_paged: false,
            header_h: 44.0,
            lists_to_top: false,
            reveal_line: None,
        }
    }
}

/// `@flow_mode` bit: the whole-file view is on.
const MODE_ALL: u8 = 1;
/// `@flow_mode` bit: the whole-file view draws instead of listing.
const MODE_IMPLEMENTATION: u8 = 2;

impl FlowView {
    /// The persisted mode, as a bit set. Bits this build does not know are
    /// ignored on reading, so a file written by a newer build still opens here
    /// with everything it DOES know.
    pub fn mode_bits(&self) -> u8 {
        let mut bits = 0;
        if self.all {
            bits |= MODE_ALL;
        }
        if self.implementation {
            bits |= MODE_IMPLEMENTATION;
        }
        bits
    }

    pub fn set_mode_bits(&mut self, bits: u8) {
        self.all = bits & MODE_ALL != 0;
        self.implementation = bits & MODE_IMPLEMENTATION != 0;
    }

    /// Back to the top of the page, at its base scale - for something just
    /// opened, a return to All, or a new file.
    pub fn reset_page(&mut self) {
        self.all_zoom = 1.0;
        self.pending_scroll = Some(egui::Vec2::ZERO);
        // "Not drawn yet": the toolbar, drawn before the page, then shows no
        // legibility note left over from the previous page.
        self.page_scale = 0.0;
    }

    /// A different file: its page AND its lists start at the top. Lists keep
    /// their place while the reader moves between the scopes of ONE file.
    pub fn reset_file(&mut self) {
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
        self.chart_paged = false;
        self.lists_to_top = true;
        self.reset_page();
    }

    /// Open the element `key` - a function's chart, a container, a type - from
    /// the picker, a subroutine box, or a double click in a list. Always
    /// leaves the whole-file view, and starts fitted, at the top.
    pub fn open(&mut self, key: String) {
        self.all = false;
        self.selected = key;
        self.zoom = 1.0;
        self.pan = egui::Vec2::ZERO;
        self.chart_paged = false;
        self.reset_page();
        // The toolbar reads the scale before the canvas redraws; a scale left
        // over from another chart would flash a wrong legibility note.
        self.last_scale = 1.0;
    }
}

/// What one frame of the chart reports back to the driver.
#[derive(Default)]
pub struct ShowResult {
    /// Jump the editor to this 1-based line of the charted file.
    pub goto_line: Option<usize>,
    /// Open this element instead, by key — a subroutine box was opened, or an
    /// element was double-clicked in a list.
    pub open_chart: Option<String>,
}

/// The first row of the element picker.
pub const ALL_LABEL: &str = "All — whole file";

/// One row of the element picker below [`ALL_LABEL`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PickerRow {
    pub depth: usize,
    pub label: String,
    /// The element it opens.
    pub key: String,
    /// An entry point - drawn bright, like before.
    pub entry: bool,
}

/// The picker's rows: everything that opens on its own - functions,
/// containers and types - members indented under their container.
///
/// Consts, `use`s and macro calls are one line each and are read through
/// "All" - listing every one of them here would put a few hundred rows in the
/// popup.
pub fn picker_rows(model: &FileModel) -> Vec<PickerRow> {
    model
        .elements
        .iter()
        .enumerate()
        .filter(|(_, e)| e.openable())
        .map(|(i, e)| PickerRow {
            depth: e.depth,
            label: match e.kind {
                ElementKind::Fn(k) => format!("{}  ·  {}", pick_label(model, i), k.word()),
                _ => pick_label(model, i),
            },
            key: e.key.clone(),
            entry: matches!(e.kind, ElementKind::Fn(k) if k.is_entry()),
        })
        .collect()
}

/// How element `i` is named where it is picked - its [`Element::label`], with
/// its line added when another openable element reads the same. A type split
/// over two `impl Uart` blocks, or two `extern "C"` blocks, would otherwise be
/// two rows nobody can tell apart.
pub fn pick_label(model: &FileModel, i: usize) -> String {
    let e = &model.elements[i];
    let label = e.label();
    let twins = model
        .elements
        .iter()
        .filter(|o| o.openable() && o.depth == e.depth && o.label() == label)
        .count();
    if twins > 1 {
        format!("{label} · line {}", e.ident_line)
    } else {
        label
    }
}

/// What the tab is showing, from the view state and the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// "All — whole file".
    Whole,
    /// One function's chart (the element's index).
    Chart(usize),
    /// A container - `impl`, `trait`, `mod`, `extern` - and its members.
    Container(usize),
    /// A type on its own: its card.
    Card(usize),
    /// Nothing to show (a file with no functions, before a pick).
    Nothing,
}

impl Scope {
    /// Shows ELEMENTS rather than one function's chart - what the status line
    /// and the Outline | Implementation switch go by.
    pub fn shows_elements(self) -> bool {
        matches!(self, Scope::Whole | Scope::Container(_) | Scope::Card(_))
    }
}

/// The scope `view` selects in `model`.
pub fn scope_of(model: &FileModel, view: &FlowView) -> Scope {
    if view.all {
        return Scope::Whole;
    }
    match model
        .elements
        .iter()
        .position(|e| e.key == view.selected && e.openable())
    {
        Some(i) if model.elements[i].chart.is_some() => Scope::Chart(i),
        Some(i) if model.elements[i].kind.is_container() => Scope::Container(i),
        Some(i) => Scope::Card(i),
        None => Scope::Nothing,
    }
}

/// Indices of `root` and everything under it - a contiguous run, because the
/// elements are listed with members right after their container.
pub fn subtree(model: &FileModel, root: usize) -> std::ops::Range<usize> {
    let depth = model.elements[root].depth;
    let end = model.elements[root + 1..]
        .iter()
        .position(|e| e.depth <= depth)
        .map_or(model.elements.len(), |n| root + 1 + n);
    root..end
}

/// Whether a function's chart, fitted whole into `avail`, would be too small
/// to read - in which case it is drawn like a page instead: width-fit and
/// scrolled, so its boxes keep their text.
pub fn too_tall(avail: egui::Vec2, lay: &FlowLayout) -> bool {
    stays_paged(avail, lay, false)
}

/// [`too_tall`] with a margin: once paged, a chart goes back to being fitted
/// only when it fits clearly ABOVE legible, not the moment it scrapes past.
/// Without it a panel resized to the edge, or a toolbar note that comes and
/// goes, would flip the view between two layouts from one frame to the next.
pub fn stays_paged(avail: egui::Vec2, lay: &FlowLayout, was_paged: bool) -> bool {
    if lay.boxes.is_empty() {
        return false;
    }
    let fit = ((avail.x - 2.0 * FIT_PAD) / lay.width.max(1.0))
        .min((avail.y - 2.0 * FIT_PAD) / lay.height.max(1.0));
    let threshold = if was_paged {
        LEGIBLE_SCALE * 1.15
    } else {
        LEGIBLE_SCALE
    };
    fit < threshold
}

/// The colour an element's kind word is drawn in. Function / struct / enum /
/// trait match the Structure tab's glyphs, so the two Project tabs agree.
pub fn kind_color(kind: ElementKind) -> egui::Color32 {
    use egui::Color32 as C;
    match kind {
        ElementKind::Fn(EntryKind::Function) => C::from_rgb(130, 170, 240),
        // An entry point starts on its own - the gold the rest of the app uses
        // for "look here".
        ElementKind::Fn(_) => C::from_rgb(240, 200, 110),
        ElementKind::RequiredFn => C::from_rgb(105, 135, 190),
        ElementKind::Struct | ElementKind::Union => C::from_rgb(230, 160, 80),
        ElementKind::Enum => C::from_rgb(190, 130, 230),
        ElementKind::Trait | ElementKind::TraitAlias => C::from_rgb(120, 200, 140),
        ElementKind::Impl => C::from_rgb(110, 190, 200),
        ElementKind::Const | ElementKind::Static => C::from_rgb(215, 175, 125),
        ElementKind::TypeAlias => C::from_rgb(190, 190, 130),
        ElementKind::Mod | ElementKind::ModDecl => C::from_rgb(170, 174, 184),
        ElementKind::MacroRules | ElementKind::MacroCall => C::from_rgb(230, 130, 160),
        ElementKind::Use => C::from_rgb(140, 146, 158),
        ElementKind::ExternCrate | ElementKind::ForeignMod => C::from_rgb(160, 160, 205),
        ElementKind::CrateAttrs | ElementKind::Other => C::from_rgb(150, 150, 160),
    }
}

const BG: egui::Color32 = egui::Color32::from_rgb(24, 26, 32);
const TEXT: egui::Color32 = egui::Color32::from_rgb(228, 232, 240);
const DIM_TEXT: egui::Color32 = egui::Color32::from_rgb(132, 138, 150);
const LABEL: egui::Color32 = egui::Color32::from_rgb(186, 194, 210);
const BORDER: egui::Color32 = egui::Color32::from_rgb(96, 106, 128);
const HOVER: egui::Color32 = egui::Color32::from_rgb(250, 250, 250);
/// The `.await` pill — the executor's yield point, in the definition-highlight
/// gold the rest of the app already uses for "look here".
const AWAIT: egui::Color32 = egui::Color32::from_rgb(255, 214, 90);

/// Fill per shape. Muted, so the white box text stays readable on the dark
/// canvas — the same constraint the module diagram's package palette works
/// under.
fn fill(shape: Shape) -> egui::Color32 {
    match shape {
        Shape::Terminal => egui::Color32::from_rgb(60, 58, 44),
        Shape::Process => egui::Color32::from_rgb(46, 52, 66),
        Shape::Io => egui::Color32::from_rgb(38, 60, 56),
        Shape::Decision => egui::Color32::from_rgb(64, 54, 40),
        Shape::Subroutine => egui::Color32::from_rgb(56, 46, 68),
        Shape::Generated => egui::Color32::from_rgb(31, 33, 39),
        Shape::Decl => egui::Color32::from_rgb(40, 44, 54),
    }
}

/// A container frame's panel, a shade off the canvas so its members read as
/// belonging to it.
const FRAME_FILL: egui::Color32 = egui::Color32::from_rgb(30, 33, 41);

fn edge_color(kind: EdgeKind) -> egui::Color32 {
    match kind {
        EdgeKind::Flow => egui::Color32::from_rgb(150, 165, 195),
        EdgeKind::Back => egui::Color32::from_rgb(120, 170, 240),
        EdgeKind::Break | EdgeKind::Return => egui::Color32::from_rgb(226, 148, 96),
        EdgeKind::Continue => egui::Color32::from_rgb(140, 200, 140),
        EdgeKind::Try => egui::Color32::from_rgb(220, 96, 86),
    }
}

/// Padding kept around the chart when it auto-fits the panel.
const FIT_PAD: f32 = 20.0;
/// Below this the box text stops being readable, so the toolbar says what scale
/// the chart is at rather than leaving it looking broken.
const LEGIBLE_SCALE: f32 = 0.45;

/// Render the toolbar and the chart. `status` is a short note from the driver
/// (a syntax error, an empty file); an empty string means all is well.
///
/// `composed` is the canvas of whatever this frame draws as a page - the
/// whole file or a container in Implementation, a type's card, a function (for
/// when it is too tall to fit) - built and cached by the driver.
pub fn show(
    ui: &mut egui::Ui,
    model: &FileModel,
    lay: &FlowLayout,
    composed: Option<&Composed>,
    view: &mut FlowView,
    status: &str,
) -> ShowResult {
    let charts = &model.charts;
    let mut result = ShowResult::default();
    let scope = scope_of(model, view);
    // What this frame draws is decided by the state it STARTED with: the
    // driver built the canvas for that state, and a switch flipped in the
    // toolbar below takes effect next frame rather than drawing a canvas
    // built for something else.
    let implementation = view.implementation;
    // For the toolbar, drawn before the canvas: whether the open function was
    // drawn as a page last frame (the decision itself is made under the
    // toolbar, from the room really left).
    let was_paged = matches!(scope, Scope::Chart(_)) && view.chart_paged;
    let reveal = view.reveal_line.take();

    // ── Toolbar ───────────────────────────────────────────────────────────
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Element").size(11.0).color(DIM_TEXT));
        let current = match scope {
            Scope::Whole => ALL_LABEL.to_string(),
            Scope::Chart(i) => model.elements[i]
                .chart
                .map(|c| charts[c].name.clone())
                .unwrap_or_default(),
            Scope::Container(i) | Scope::Card(i) => pick_label(model, i),
            Scope::Nothing => "—".to_string(),
        };
        egui::ComboBox::from_id_salt("flow_chart_pick")
            .selected_text(egui::RichText::new(current).size(11.5))
            .width(260.0)
            .height(420.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(
                        view.all,
                        egui::RichText::new(ALL_LABEL).size(11.5).color(TEXT),
                    )
                    .on_hover_text("Every element of the file, in source order")
                    .clicked()
                    && !view.all
                {
                    view.all = true;
                    view.reset_page();
                }
                ui.separator();
                for row in picker_rows(model) {
                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 14.0);
                        // Entry points lead with what starts them; an
                        // `#[interrupt]` in the same list as a helper `fn` is
                        // the difference between "hardware calls this" and
                        // "someone calls this".
                        let text = egui::RichText::new(&row.label)
                            .size(11.5)
                            .color(if row.entry { TEXT } else { DIM_TEXT });
                        let on = !view.all && view.selected == row.key;
                        if ui.selectable_label(on, text).clicked() {
                            view.open(row.key.clone());
                        }
                    });
                }
            });

        // Two ways to read many elements: as a list of what they are, or
        // drawn - functions as flowcharts, the rest as cards. Always shown, so
        // the toolbar does not shift; there is nothing to choose for one
        // function or one type.
        ui.add_space(6.0);
        let many = matches!(scope, Scope::Whole | Scope::Container(_));
        ui.add_enabled_ui(many, |ui| {
            let tab = |ui: &mut egui::Ui, on: bool, label: &str, tip: &str| {
                ui.selectable_label(on, egui::RichText::new(label).size(11.0))
                    .on_hover_text(tip)
                    .on_disabled_hover_text(
                        "A function is always drawn in full, and a type is its card - pick All or a container to choose",
                    )
                    .clicked()
            };
            if tab(
                ui,
                !view.implementation,
                "Outline",
                "One row per element: what it holds",
            ) {
                view.implementation = false;
            }
            if tab(
                ui,
                view.implementation,
                "Implementation",
                "Every element drawn: functions as flowcharts, declarations as cards",
            ) {
                view.implementation = true;
            }
        });

        ui.add_space(10.0);
        let counts = match scope {
            Scope::Whole => format!(
                "{} · {}",
                plural(model.elements.len(), "element"),
                plural(charts.len(), "function")
            ),
            Scope::Container(i) => {
                let inner = subtree(model, i);
                let fns = model.elements[inner.clone()]
                    .iter()
                    .filter(|e| e.chart.is_some())
                    .count();
                format!(
                    "{} · {}",
                    plural(inner.len() - 1, "member"),
                    plural(fns, "function")
                )
            }
            Scope::Chart(_) | Scope::Nothing => {
                format!("{} boxes · {} edges", lay.boxes.len(), lay.edges.len())
            }
            Scope::Card(_) => String::new(),
        };
        ui.label(egui::RichText::new(counts).size(11.0).color(DIM_TEXT));
        // The scale the view in front of the reader was drawn at last frame.
        let paged = was_paged
            || scope.shows_elements() && (implementation || matches!(scope, Scope::Card(_)));
        let drawn = if paged {
            Some(view.page_scale).filter(|s| *s > 0.0)
        } else if matches!(scope, Scope::Chart(_)) {
            Some(view.last_scale)
        } else {
            None
        };
        if let Some(s) = drawn.filter(|s| *s < LEGIBLE_SCALE) {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("at {:.0}% — zoom in to read the boxes", s * 100.0))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(220, 180, 90)),
            );
        }
        if !status.is_empty() {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(status)
                    .size(11.0)
                    .color(egui::Color32::from_rgb(226, 148, 96)),
            );
        }
    });

    // A chart fitted whole below legible is drawn as a scrolled page instead.
    // Judged on the room really left under the toolbar, less the legend and
    // hint rows as measured last frame - a narrow panel wraps all of them.
    let tall = matches!(scope, Scope::Chart(_))
        && stays_paged(
            ui.available_size() - egui::vec2(0.0, view.header_h),
            lay,
            view.chart_paged,
        );
    if matches!(scope, Scope::Chart(_)) {
        view.chart_paged = tall;
    }

    // ── Many elements, or one drawn as a page ─────────────────────────────
    match scope {
        Scope::Whole | Scope::Container(_) if !implementation => {
            hint(
                ui,
                "click a row = go to its line · double-click = open it on its own",
            );
            let root = match scope {
                Scope::Container(i) => Some(i),
                _ => None,
            };
            let to_top = std::mem::take(&mut view.lists_to_top);
            let (goto, open) = outline(ui, model, &view.selected, root, to_top, reveal);
            result.goto_line = goto;
            result.open_chart = open;
            return result;
        }
        Scope::Whole | Scope::Container(_) | Scope::Card(_) => {
            legend(ui, true);
            hint(
                ui,
                "mouse wheel = scroll, Ctrl+wheel / Ctrl+± = zoom, Ctrl+0 = reset the zoom · click a box = go to its line · click a call = go to that function",
            );
            return match composed {
                Some(c) => page(ui, model, c, view, reveal),
                None => result,
            };
        }
        Scope::Chart(_) if tall => {
            let top = ui.cursor().top();
            legend(ui, false);
            hint(
                ui,
                "too tall to read whole, so it scrolls: mouse wheel = scroll, Ctrl+wheel / Ctrl+± = zoom, Ctrl+0 = reset · click a box = go to its line · click a call = open that function",
            );
            view.header_h = ui.cursor().top() - top;
            return match composed {
                Some(c) => page(ui, model, c, view, reveal),
                None => result,
            };
        }
        _ => {}
    }

    // ── Legend + hints ────────────────────────────────────────────────────
    let top = ui.cursor().top();
    legend(ui, false);
    hint(
        ui,
        "Ctrl+± / mouse wheel zoom, Ctrl+0 reset · drag the background = pan · click a box = go to its line",
    );
    view.header_h = ui.cursor().top() - top;

    // ── Canvas ────────────────────────────────────────────────────────────
    let avail = ui.available_size();
    if avail.x < 3.0 * FIT_PAD || avail.y < 3.0 * FIT_PAD || lay.boxes.is_empty() {
        // Still claim the space, so the panel does not jump around while the
        // file has nothing to draw.
        ui.allocate_exact_size(avail.max(egui::Vec2::ZERO), egui::Sense::hover());
        return result;
    }

    if ui.rect_contains_pointer(ui.available_rect_before_wrap()) {
        ui.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            if i.consume_key(cmd, egui::Key::Num0) {
                view.zoom = 1.0;
                view.pan = egui::Vec2::ZERO;
            } else if i.consume_key(cmd, egui::Key::Plus) || i.consume_key(cmd, egui::Key::Equals) {
                view.zoom = (view.zoom * 1.15).min(4.0);
            } else if i.consume_key(cmd, egui::Key::Minus) {
                view.zoom = (view.zoom / 1.15).max(0.3);
            }
            let scroll = i.smooth_scroll_delta.y;
            if scroll != 0.0 {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                view.zoom = (view.zoom * (scroll * 0.002).exp()).clamp(0.3, 4.0);
            }
        });
    }

    let base = ((avail.x - 2.0 * FIT_PAD) / lay.width.max(1.0_f32))
        .min((avail.y - 2.0 * FIT_PAD) / lay.height.max(1.0_f32))
        .clamp(0.05, 2.5);
    let scale = (base * view.zoom).clamp(0.05, 5.0);
    view.last_scale = scale;
    let content = egui::vec2(lay.width, lay.height) * scale;

    let (rect, bg) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
    if bg.dragged() {
        view.pan += bg.drag_delta();
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    let free = (rect.size() - content) * 0.5;
    let rel = egui::vec2(
        clamp_rel(free.x + view.pan.x, rect.width(), content.x),
        clamp_rel(free.y + view.pan.y, rect.height(), content.y),
    );
    view.pan = rel - free;
    let origin = rect.left_top() + rel;
    let to_screen = |x: f32, y: f32| -> egui::Pos2 { origin + egui::vec2(x, y) * scale };

    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, BG);

    // `rect_contains_pointer`, not `rect.contains`: it also asks whether the
    // chart is the topmost thing there, so a box under the New Project chip
    // list or a window does not light up and pop its tooltip over it.
    let pointer = ui
        .ctx()
        .pointer_latest_pos()
        .filter(|_| ui.rect_contains_pointer(rect));
    let hit = draw_scene(&painter, lay, &[], &to_screen, scale, rect, pointer, true);

    // A click lands on whatever the pointer is over. Opening a subroutine also
    // jumps the editor to the CALL, so the two views never disagree about what
    // is being looked at.
    if let Some(Hit::Box(i)) = hit {
        let b = &lay.boxes[i];
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        if bg.clicked() {
            result.goto_line = Some(b.node.line);
            if b.node.shape == Shape::Subroutine
                && let Some(key) = &b.node.goto_key
                && charts.iter().any(|c| c.key == *key)
            {
                result.open_chart = Some(key.clone());
            }
        }
        box_tip(ui, b);
    }

    result
}

/// The row of shape swatches above a chart. The whole-file canvas adds the
/// declaration card.
fn legend(ui: &mut egui::Ui, with_decl: bool) {
    ui.horizontal_wrapped(|ui| {
        let mut shapes = vec![
            (Shape::Process, "statements"),
            (Shape::Io, "in / out"),
            (Shape::Decision, "decision"),
            (Shape::Subroutine, "call — click to open"),
            (Shape::Generated, "generated"),
        ];
        if with_decl {
            shapes[3].1 = "call — click to go to it";
            shapes.push((Shape::Decl, "declaration"));
        }
        for (shape, name) in shapes {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(13.0, 9.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 2.0, fill(shape));
            ui.painter().rect_stroke(
                rect,
                2.0,
                egui::Stroke::new(1.0_f32, BORDER),
                egui::StrokeKind::Inside,
            );
            ui.label(egui::RichText::new(name).size(10.5).color(DIM_TEXT));
            ui.add_space(6.0);
        }
    });
}

/// The one-line usage hint under the toolbar.
fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(10.5)
            .color(egui::Color32::from_rgb(120, 120, 130)),
    );
    ui.add_space(2.0);
}

/// What the pointer is over on a drawn canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Box(usize),
    /// A frame's title bar.
    Frame(usize),
}

/// Paint frames, then edges, then boxes - only what intersects `visible`
/// (screen space) - and report what the pointer is over.
///
/// The visibility test comes BEFORE anything builds a string or lays out text:
/// egui's painter lays a galley out and records the shape even when it is off
/// screen, and only the tessellator culls. Measured on a ten-thousand-line
/// file, that made every zoom step of the whole-file canvas cost ~30 ms
/// against ~7 ms culled.
///
/// `labels`: draw the edges' labels (YES / NO, match arms). The single chart
/// always does, as it always has; the whole-file canvas only where they read.
#[allow(clippy::too_many_arguments)]
fn draw_scene(
    painter: &egui::Painter,
    lay: &FlowLayout,
    frames: &[Frame],
    to_screen: &impl Fn(f32, f32) -> egui::Pos2,
    scale: f32,
    visible: egui::Rect,
    pointer: Option<egui::Pos2>,
    labels: bool,
) -> Option<Hit> {
    let mut hit = None;
    // Frames under everything, outer before inner - `compose` orders them so.
    for (i, f) in frames.iter().enumerate() {
        let r = egui::Rect::from_min_size(to_screen(f.x, f.y), egui::vec2(f.w, f.h) * scale);
        if !r.intersects(visible) {
            continue;
        }
        let title = egui::Rect::from_min_size(r.min, egui::vec2(r.width(), FRAME_TITLE_H * scale));
        let on_title = pointer.is_some_and(|p| title.contains(p));
        if on_title {
            hit = Some(Hit::Frame(i));
        }
        draw_frame(painter, f, r, title, scale, on_title);
    }

    // Edges next, so a box always covers the tail of its own arrow.
    let stroke_w = (1.4 * scale).clamp(0.7, 2.6);
    for e in &lay.edges {
        let Some(bb) = edge_bounds(e, to_screen) else {
            continue;
        };
        if bb.expand(edge_reach(e, scale, labels)).intersects(visible) {
            draw_edge(painter, e, to_screen, stroke_w, scale, labels);
        }
    }

    // Boxes are drawn after edges, so the hit-test uses the same rectangles the
    // user sees; a box wins over the frame it sits in.
    let mut hovered = None;
    for (i, b) in lay.boxes.iter().enumerate() {
        let r = box_rect(b, to_screen, scale);
        if !r.intersects(visible) {
            continue;
        }
        if pointer.is_some_and(|p| r.contains(p)) {
            hovered = Some(i);
        }
    }
    for (i, b) in lay.boxes.iter().enumerate() {
        if box_rect(b, to_screen, scale).intersects(visible) {
            draw_box(painter, b, to_screen, scale, hovered == Some(i));
        }
    }
    hovered.map(Hit::Box).or(hit)
}

/// How far past its points an edge paints, in screen px: the arrowhead's
/// wings, and the label set beside its first segment - so an edge whose line
/// is just off screen still draws the label that is not.
fn edge_reach(e: &Edge, scale: f32, labels: bool) -> f32 {
    let arrow = (8.0 * scale).max(3.5) + 2.0;
    if !labels || e.label.is_empty() {
        return arrow;
    }
    // Mirrors `draw_edge`: the offset beside the segment, plus half the text.
    let font = (10.0 * scale).clamp(5.0, 15.0);
    let offset = 11.0 * scale.max(0.6);
    let half_text = 0.35 * font * e.label.chars().count() as f32 + font;
    arrow.max(offset + half_text)
}

/// The screen rectangle an edge's points span.
fn edge_bounds(e: &Edge, to_screen: &impl Fn(f32, f32) -> egui::Pos2) -> Option<egui::Rect> {
    let mut pts = e.pts.iter().map(|&(x, y)| to_screen(x, y));
    let first = pts.next()?;
    Some(pts.fold(egui::Rect::from_min_max(first, first), |r, p| {
        r.union(egui::Rect::from_min_max(p, p))
    }))
}

/// A container: a tinted panel with the kind-coloured title bar on top.
fn draw_frame(
    painter: &egui::Painter,
    f: &Frame,
    r: egui::Rect,
    title: egui::Rect,
    scale: f32,
    hovered: bool,
) {
    let kind = if f.generated {
        kind_color(f.kind).gamma_multiply(0.55)
    } else {
        kind_color(f.kind)
    };
    painter.rect_filled(r, 4.0 * scale.min(1.0), FRAME_FILL);
    painter.rect_filled(title, 4.0 * scale.min(1.0), kind.gamma_multiply(0.18));
    painter.rect_stroke(
        r,
        4.0 * scale.min(1.0),
        egui::Stroke::new(
            if hovered { 2.0 } else { 1.0 } * scale.clamp(0.6, 2.0),
            if hovered {
                HOVER
            } else {
                kind.gamma_multiply(0.6)
            },
        ),
        egui::StrokeKind::Inside,
    );
    let size = (11.0 * scale).clamp(4.0, 20.0);
    if size >= 4.5 {
        painter.with_clip_rect(title).text(
            egui::pos2(title.left() + FRAME_PAD * 0.7 * scale, title.center().y),
            egui::Align2::LEFT_CENTER,
            &f.title,
            egui::FontId::monospace(size),
            kind,
        );
    }
}

/// The tooltip of a hovered box: everything its text had no room for.
fn box_tip(ui: &egui::Ui, b: &Placed) {
    let mut tip = b.node.text.clone();
    for d in &b.node.detail {
        tip.push('\n');
        tip.push_str(d);
    }
    if b.node.hidden > 0 {
        tip.push_str(&format!("\n+{} more", b.node.hidden));
    }
    tip.push_str(&format!("\n\nline {}", b.node.line));
    if b.node.awaits {
        tip.push_str("  ·  yields to the executor (.await)");
    }
    text_tip(ui, tip);
}

/// A monospace tooltip at the pointer.
fn text_tip(ui: &egui::Ui, tip: String) {
    egui::Tooltip::always_open(
        ui.ctx().clone(),
        ui.layer_id(),
        egui::Id::new("flow_box_tip"),
        egui::PopupAnchor::Pointer,
    )
    .gap(12.0)
    .show(|ui| {
        ui.label(egui::RichText::new(tip).size(11.0).monospace());
    });
}

/// The scroll offset that keeps the canvas point under the pointer where it
/// is while the scale changes from `old` to `new` - what makes Ctrl+wheel zoom
/// INTO what the reader points at instead of towards the top-left corner.
///
/// `offset` and the result are the scroll area's offsets (screen px); `local`
/// is the pointer relative to the scroll area's top-left.
pub fn zoom_about(offset: egui::Vec2, local: egui::Vec2, old: f32, new: f32) -> egui::Vec2 {
    let pad = egui::vec2(FIT_PAD, FIT_PAD);
    let canvas = (offset + local - pad) / old;
    (canvas * new + pad - local).max(egui::Vec2::ZERO)
}

/// "All — whole file" drawn: every element on one tall canvas.
///
/// The canvas is a document, so it behaves like one: it fits the WIDTH (never
/// shrinking text below 60 %), the wheel scrolls, Ctrl+wheel and Ctrl± zoom
/// about the pointer, and the scroll bars say where in the file the reader is.
/// Fitting the whole height like a single chart does would put a long file at
/// 5 % - boxes with no text at all.
///
/// `reveal`: a source line the editor's caret moved onto (see
/// [`FlowView::reveal_line`]).
fn page(
    ui: &mut egui::Ui,
    model: &FileModel,
    comp: &Composed,
    view: &mut FlowView,
    reveal: Option<usize>,
) -> ShowResult {
    let mut result = ShowResult::default();
    let lay = &comp.layout;
    if lay.boxes.is_empty() && comp.frames.is_empty() {
        return result;
    }
    let area = ui.available_rect_before_wrap();
    let base = ((area.width() - 2.0 * FIT_PAD) / lay.width.max(1.0)).clamp(0.6, 1.0);
    let zoom_before = view.all_zoom;

    // Zoom: only while the pointer is over the canvas, so the editor keeps
    // its own Ctrl+± (the same hover routing the other diagrams use).
    let pointer = ui
        .ctx()
        .pointer_latest_pos()
        .filter(|_| ui.rect_contains_pointer(area));
    if pointer.is_some() {
        ui.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            if i.consume_key(cmd, egui::Key::Num0) {
                view.all_zoom = 1.0;
            } else if i.consume_key(cmd, egui::Key::Plus) || i.consume_key(cmd, egui::Key::Equals) {
                view.all_zoom = (view.all_zoom * 1.15).min(4.0);
            } else if i.consume_key(cmd, egui::Key::Minus) {
                view.all_zoom = (view.all_zoom / 1.15).max(0.3);
            }
            // Ctrl+wheel arrives as a zoom factor, not as a scroll.
            let z = i.zoom_delta();
            if z != 1.0 {
                view.all_zoom = (view.all_zoom * z).clamp(0.3, 4.0);
            }
        });
    }
    let scale = (base * view.all_zoom).clamp(0.05, 5.0);
    let content = egui::vec2(lay.width, lay.height) * scale + egui::vec2(2.0, 2.0) * FIT_PAD;

    // The scale the pixel offset was LAST drawn at - not rebuilt from this
    // frame's width, or a panel resize (which moves `base`) would rescale the
    // canvas about its top-left corner and throw the reader a screen or two
    // away from where they were reading.
    let old_scale = if view.page_scale > 0.0 {
        view.page_scale
    } else {
        scale
    };
    let mut offset = None;
    if (scale - old_scale).abs() > f32::EPSILON {
        // A zoom keeps what is under the pointer; a resize keeps the top line.
        let local = match pointer {
            Some(p) if view.all_zoom != zoom_before => p - area.min,
            _ => egui::Vec2::ZERO,
        };
        offset = Some(zoom_about(view.last_offset, local, old_scale, scale));
    }
    if let Some(target) = view.pending_scroll.take() {
        offset = Some(target * scale);
    }
    // The caret's statement, from wherever the page is about to be.
    if let Some(r) = reveal.and_then(|line| reveal_rect(model, comp, line)) {
        let px = content_rect(r, scale);
        if let Some(o) = reveal_offset(offset.unwrap_or(view.last_offset), area.size(), px) {
            offset = Some(o);
        }
    }
    // egui applies a builder offset as given for the frame it is drawn in and
    // only clamps it afterwards, so an offset past the end would draw one
    // frame of empty canvas (and nothing asks for the repaint that fixes it).
    let offset = offset.map(|o| clamp_offset(o, content, area.size()));

    let mut area_widget = egui::ScrollArea::both()
        .id_salt("flow_page")
        .auto_shrink([false, false]);
    if let Some(o) = offset {
        area_widget = area_widget.scroll_offset(o);
    }
    let mut hit = None;
    let mut clicked = false;
    let out = area_widget.show_viewport(ui, |ui, viewport| {
        let (rect, resp) = ui.allocate_exact_size(content, egui::Sense::click());
        clicked = resp.clicked();
        let origin = rect.min + egui::vec2(FIT_PAD, FIT_PAD);
        let to_screen = |x: f32, y: f32| origin + egui::vec2(x, y) * scale;
        let visible = viewport.translate(rect.min.to_vec2());
        let painter = ui.painter().with_clip_rect(visible);
        painter.rect_filled(visible, 0.0, BG);
        // Through the content's own response, which egui has already
        // resolved against the floating scroll bars and any popup on top: a
        // box under the bar must not light up for a click the bar will take.
        let over = pointer
            .filter(|p| visible.contains(*p))
            .filter(|_| resp.hovered());
        hit = draw_scene(
            &painter,
            lay,
            &comp.frames,
            &to_screen,
            scale,
            visible,
            over,
            scale >= LEGIBLE_SCALE,
        );
    });
    view.last_offset = out.state.offset;
    view.page_scale = scale;

    match hit {
        Some(Hit::Box(i)) => {
            let b = &lay.boxes[i];
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if clicked {
                result.goto_line = Some(b.node.line);
                // A call scrolls to the function it calls - it is on this very
                // canvas. Only one hidden in the test card opens on its own.
                if b.node.shape == Shape::Subroutine
                    && let Some(key) = &b.node.goto_key
                {
                    match comp.anchor(model, key) {
                        Some((x, y)) => {
                            view.pending_scroll =
                                Some(egui::vec2((x - FIT_PAD).max(0.0), (y - FIT_PAD).max(0.0)));
                        }
                        None => result.open_chart = Some(key.clone()),
                    }
                }
            }
            // Not while the page moves under a still pointer: the tip would
            // follow the scroll and change every frame, over what is being
            // read. egui's own tooltips wait the same way.
            let moving = ui.input(|i| {
                i.time_since_last_scroll() < ui.style().interaction.tooltip_delay
                    || i.pointer.is_decidedly_dragging()
            });
            if !moving {
                match b.node.decl {
                    // A card's own text is cut to fit; its tooltip is not.
                    Some(tag) => text_tip(ui, card_tip(model, tag.element)),
                    None => box_tip(ui, b),
                }
            }
        }
        Some(Hit::Frame(i)) => {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            if clicked {
                result.goto_line = Some(comp.frames[i].line);
            }
        }
        None => {}
    }
    result
}

/// What on `comp` draws source line `line`, in canvas units: the box of the
/// statement the line is in (the last box that starts at or above it), a
/// type's card, a container's title bar. `None` between items, and for a line
/// of something `comp` does not show.
///
/// A member of a folded test module has no place of its own on the page: the
/// module's card stands for it.
pub fn reveal_rect(model: &FileModel, comp: &Composed, line: usize) -> Option<egui::Rect> {
    let mut at = super::element_at_line(model, line);
    let ext = loop {
        let i = at?;
        match comp.extents.iter().find(|x| x.element == i) {
            Some(x) => break x,
            None => at = model.elements[i].parent,
        }
    };
    let whole = egui::Rect::from_min_size(egui::pos2(ext.x, ext.y), egui::vec2(ext.w, ext.h));
    if comp.frames.iter().any(|f| f.element == ext.element) {
        // Its header or a line between its members.
        return Some(egui::Rect::from_min_size(
            whole.min,
            egui::vec2(ext.w, FRAME_TITLE_H.min(ext.h)),
        ));
    }
    if model.elements[ext.element].chart.is_none() {
        return Some(whole);
    }
    // START and END both carry the function's own line; the top one wins.
    let statement = comp.layout.boxes[ext.boxes.clone()]
        .iter()
        .filter(|b| b.node.line <= line)
        .max_by(|a, b| a.node.line.cmp(&b.node.line).then(b.y.total_cmp(&a.y)));
    Some(match statement {
        Some(b) => egui::Rect::from_min_size(egui::pos2(b.x, b.y), egui::vec2(b.w, b.h)),
        // A doc comment or an attribute above it.
        None => whole,
    })
}

/// Where canvas rect `r` lands in a page's scrolled content, in px: scaled,
/// and past the padding the canvas is drawn inside - what the page's scroll
/// offsets are measured in.
pub fn content_rect(r: egui::Rect, scale: f32) -> egui::Rect {
    let pad = egui::vec2(FIT_PAD, FIT_PAD);
    egui::Rect::from_min_size((r.min.to_vec2() * scale + pad).to_pos2(), r.size() * scale)
}

/// The scroll offset that brings `target` (content px) into a view of size
/// `view` scrolled to `offset` - or `None` when enough of it is on screen
/// already: its top-left corner, and as much of it as half the view holds.
///
/// Brought in, its top lands a third of the way down, under what leads to it;
/// sideways the view moves only when that side is out of sight.
pub fn reveal_offset(
    offset: egui::Vec2,
    view: egui::Vec2,
    target: egui::Rect,
) -> Option<egui::Vec2> {
    let head = egui::Rect::from_min_size(target.min, target.size().min(view * 0.5));
    let shown = egui::Rect::from_min_size(offset.to_pos2(), view);
    if shown.contains_rect(head) {
        return None;
    }
    let x = if shown.x_range().contains(head.min.x) && shown.x_range().contains(head.max.x) {
        offset.x
    } else {
        (target.min.x - FIT_PAD).max(0.0)
    };
    let y = if shown.y_range().contains(head.min.y) && shown.y_range().contains(head.max.y) {
        offset.y
    } else {
        (target.min.y - view.y / 3.0).max(0.0)
    };
    Some(egui::vec2(x, y))
}

/// `offset` kept inside what a scroll area of size `view` over `content` can
/// actually show - from the top-left corner to the bottom-right end.
pub fn clamp_offset(offset: egui::Vec2, content: egui::Vec2, view: egui::Vec2) -> egui::Vec2 {
    offset.clamp(egui::Vec2::ZERO, (content - view).max(egui::Vec2::ZERO))
}

fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// Height of one outline row.
const ROW_H: f32 = 18.0;
/// Indent per level of nesting (a method under its `impl`).
const INDENT: f32 = 16.0;
/// Width of the kind-word column.
const KIND_W: f32 = 58.0;
/// Width kept free on the right for the line number.
const LINE_W: f32 = 44.0;

/// "All — whole file": every element of the file, one row each, in source
/// order, members indented under their container.
///
/// A LIST, not boxes on the zoomable canvas: it is an enumeration, and on the
/// canvas it would inherit the auto-fit that shrinks a long file to unreadable
/// and the wheel-zoom that fights reading down a page. Here the wheel scrolls,
/// the text stays at its native size, and only the visible rows are drawn
/// (`show_rows`), so a ten-thousand-line file costs what a screenful does.
///
/// `root`: list only that container and its members (a container picked on
/// its own), indented from its own depth.
///
/// `reveal`: a source line the editor's caret moved onto - its element's row
/// comes into view a third of the way down, unless it is in sight already.
///
/// Returns `(line to jump to, element to open)`. A single click only jumps -
/// the list stays, so it can be read top to bottom while the editor follows; a
/// double click opens the element on its own (a function's flowchart, a
/// container, a type's card).
fn outline(
    ui: &mut egui::Ui,
    model: &FileModel,
    current: &str,
    root: Option<usize>,
    to_top: bool,
    reveal: Option<usize>,
) -> (Option<usize>, Option<String>) {
    let mut goto = None;
    let mut open = None;
    let rows = match root {
        Some(r) => subtree(model, r),
        None => 0..model.elements.len(),
    };
    let base_depth = root.map_or(0, |r| model.elements[r].depth);
    if rows.is_empty() {
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new("This file has no items.")
                .size(12.0)
                .color(DIM_TEXT),
        );
        return (goto, open);
    }
    let bg = ui.available_rect_before_wrap();
    ui.painter().rect_filled(bg, 0.0, BG);
    // One scroll position PER scope: the whole-file list keeps the reader's
    // place while a container is visited, and a container's list opens at its
    // own top instead of at the whole file's offset, clamped to its bottom.
    let salt = ("flow_outline", root.map(|r| model.elements[r].key.as_str()));
    let mut list = egui::ScrollArea::vertical()
        .id_salt(salt)
        .auto_shrink([false, false]);
    let mut offset = to_top.then_some(0.0);
    // The caret's row, worked out from where the list is and handed to it for
    // THIS frame, like the page does - a `scroll_to_rect` from inside the list
    // lands two frames later, on a frame nothing asks to be drawn.
    if let Some(i) = reveal
        .and_then(|line| super::element_at_line(model, line))
        .filter(|i| rows.contains(i))
    {
        // `show_rows` puts row n at n row-heights, spacing included.
        let step = ROW_H + ui.spacing().item_spacing.y;
        let view = ui.available_size();
        let content = rows.len() as f32 * step - ui.spacing().item_spacing.y;
        // The id the list files its state under - egui's derivation, which
        // changed in 0.35, so it comes from the one helper that tests it.
        let now = offset.unwrap_or_else(|| {
            let id = crate::app::helpers::scroll_id::scroll_area_id(ui, salt);
            egui::scroll_area::State::load(ui.ctx(), id).map_or(0.0, |s| s.offset.y)
        });
        let row = egui::Rect::from_min_size(
            egui::pos2(0.0, (i - rows.start) as f32 * step),
            egui::vec2(view.x, ROW_H),
        );
        if let Some(o) = reveal_offset(egui::vec2(0.0, now), view, row) {
            offset = Some(o.y.clamp(0.0, (content - view.y).max(0.0)));
        }
    }
    if let Some(y) = offset {
        list = list.vertical_scroll_offset(y);
    }
    list.show_rows(ui, ROW_H, rows.len(), |ui, range| {
        let range = rows.start + range.start..rows.start + range.end;
        for e in &model.elements[range] {
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), ROW_H),
                egui::Sense::click(),
            );
            let painter = ui.painter();
            if resp.hovered() {
                painter.rect_filled(
                    rect,
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(120, 150, 210, 38),
                );
            } else if e.openable() && e.key == current {
                // What the single view would go back to.
                painter.rect_filled(
                    rect,
                    2.0,
                    egui::Color32::from_rgba_unmultiplied(255, 214, 90, 22),
                );
            }
            let dim = |c: egui::Color32| {
                if e.generated {
                    c.gamma_multiply(0.55)
                } else {
                    c
                }
            };
            let x = rect.left() + 6.0 + (e.depth - base_depth) as f32 * INDENT;
            let mid = rect.center().y;
            let kind = fit_galley(
                painter,
                e.kind.word(),
                10.5,
                dim(kind_color(e.kind)),
                KIND_W - 6.0,
            );
            painter.galley(egui::pos2(x, mid - kind.size().y / 2.0), kind, TEXT);
            let text_x = x + KIND_W;
            let room = rect.right() - LINE_W - text_x;
            let sig = fit_galley(painter, &e.signature, 11.5, dim(TEXT), room);
            painter.galley(egui::pos2(text_x, mid - sig.size().y / 2.0), sig, TEXT);
            painter.text(
                egui::pos2(rect.right() - 6.0, mid),
                egui::Align2::RIGHT_CENTER,
                e.ident_line.to_string(),
                egui::FontId::monospace(10.0),
                DIM_TEXT,
            );
            let resp = resp.on_hover_text(outline_tip(e));
            if resp.clicked() {
                goto = Some(e.ident_line);
            }
            if resp.double_clicked() && e.openable() {
                open = Some(e.key.clone());
            }
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
        }
    });
    (goto, open)
}

/// One line of monospace text, cut with an ellipsis at `max_w` rather than
/// wrapped - a row is one line high.
///
/// The one place this feature shortens text to fit a width, used by the outline
/// rows, the flow boxes and the declaration cards. It cuts by MEASURED width,
/// never by counting characters against [`super::layout`]'s `CHAR_W`: that
/// constant over-states the real advance by 3 % - safe when it SIZES a box,
/// wrong as a divisor - and a label carrying a glyph the monospace face lacks
/// is laid out from a proportional fallback, where no per-character width is
/// right at all. egui measures the glyphs it is about to draw; nothing else can.
fn fit_galley(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: egui::Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::single_section(
        text.to_owned(),
        egui::TextFormat::simple(egui::FontId::monospace(size), color),
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(8.0));
    painter.layout_job(job)
}

/// The hover text of an outline row: everything the row had no room for.
pub fn outline_tip(e: &Element) -> String {
    let mut lines: Vec<String> = match e.kind {
        ElementKind::Use => e.detail.iter().map(|t| format!("use {t};")).collect(),
        _ => e.detail.clone(),
    };
    lines.push(String::new());
    lines.push(if e.start_line == e.end_line {
        format!("line {}", e.start_line)
    } else {
        format!("lines {}–{}", e.start_line, e.end_line)
    });
    if e.generated {
        lines.push("generated by the IDE".to_string());
    }
    if e.test_code {
        lines.push("test code".to_string());
    }
    if e.chart.is_some() {
        lines.push("double-click to open its flowchart".to_string());
    } else if e.openable() {
        lines.push("double-click to open it on its own".to_string());
    }
    lines.join("\n")
}

/// One panning axis, clamped so the chart can never be dragged out of sight.
/// Same shape as the Structure tab's — see the note there on why the bounds are
/// ORDERED rather than branched on a fits/overflows test.
fn clamp_rel(rel: f32, avail: f32, content: f32) -> f32 {
    let inside = FIT_PAD;
    let overflow = avail - content - FIT_PAD;
    let (lo, hi) = if inside <= overflow {
        (inside, overflow)
    } else {
        (overflow, inside)
    };
    rel.max(lo).min(hi)
}

fn box_rect(b: &Placed, to_screen: &impl Fn(f32, f32) -> egui::Pos2, scale: f32) -> egui::Rect {
    egui::Rect::from_min_size(to_screen(b.x, b.y), egui::vec2(b.w, b.h) * scale)
}

fn draw_edge(
    painter: &egui::Painter,
    e: &Edge,
    to_screen: &impl Fn(f32, f32) -> egui::Pos2,
    w: f32,
    scale: f32,
    labels: bool,
) {
    let color = edge_color(e.kind);
    let stroke = egui::Stroke::new(w, color);
    let pts: Vec<egui::Pos2> = e.pts.iter().map(|&(x, y)| to_screen(x, y)).collect();
    if pts.len() < 2 {
        return;
    }
    for seg in pts.windows(2) {
        painter.line_segment([seg[0], seg[1]], stroke);
    }
    if e.arrow {
        let n = pts.len();
        arrowhead(
            painter,
            pts[n - 2],
            pts[n - 1],
            (8.0 * scale).max(3.5),
            stroke,
        );
    }
    if labels && !e.label.is_empty() {
        // Beside the FIRST segment: that is where the reader's eye is when it
        // leaves the diamond, and it is the only place a label cannot be
        // confused with the neighbouring arm's.
        let a = pts[0];
        let b = pts[1];
        let mid = a + (b - a) * 0.5;
        let horizontal = (b.x - a.x).abs() > (b.y - a.y).abs();
        let off = if horizontal {
            egui::vec2(0.0, -8.0 * scale.max(0.6))
        } else {
            egui::vec2(11.0 * scale.max(0.6), 0.0)
        };
        painter.text(
            mid + off,
            egui::Align2::CENTER_CENTER,
            &e.label,
            egui::FontId::proportional((10.0 * scale).clamp(5.0, 15.0)),
            LABEL,
        );
    }
}

fn draw_box(
    painter: &egui::Painter,
    b: &Placed,
    to_screen: &impl Fn(f32, f32) -> egui::Pos2,
    scale: f32,
    hovered: bool,
) {
    let r = box_rect(b, to_screen, scale);
    let bg = fill(b.node.shape);
    let stroke = egui::Stroke::new(
        if hovered { 2.0 } else { 1.2 } * scale.clamp(0.6, 2.0),
        if hovered { HOVER } else { BORDER },
    );
    match b.node.shape {
        Shape::Terminal => {
            let rad = r.height() * 0.5;
            painter.rect_filled(r, rad, bg);
            painter.rect_stroke(r, rad, stroke, egui::StrokeKind::Inside);
        }
        Shape::Decision => {
            let c = r.center();
            let pts = vec![
                egui::pos2(c.x, r.top()),
                egui::pos2(r.right(), c.y),
                egui::pos2(c.x, r.bottom()),
                egui::pos2(r.left(), c.y),
            ];
            painter.add(egui::Shape::convex_polygon(pts, bg, stroke));
        }
        Shape::Io => {
            let s = 12.0 * scale;
            let pts = vec![
                egui::pos2(r.left() + s, r.top()),
                egui::pos2(r.right(), r.top()),
                egui::pos2(r.right() - s, r.bottom()),
                egui::pos2(r.left(), r.bottom()),
            ];
            painter.add(egui::Shape::convex_polygon(pts, bg, stroke));
        }
        Shape::Subroutine => {
            painter.rect_filled(r, 2.0, bg);
            painter.rect_stroke(r, 2.0, stroke, egui::StrokeKind::Inside);
            // The two side bars that make it a "predefined process".
            let inset = 7.0 * scale;
            for x in [r.left() + inset, r.right() - inset] {
                painter.line_segment(
                    [egui::pos2(x, r.top()), egui::pos2(x, r.bottom())],
                    egui::Stroke::new(stroke.width * 0.8, BORDER),
                );
            }
        }
        Shape::Generated => {
            painter.rect_filled(r, 2.0, bg);
            let dash = 4.0 * scale.max(0.4);
            for (a, b2) in [
                (r.left_top(), r.right_top()),
                (r.right_top(), r.right_bottom()),
                (r.right_bottom(), r.left_bottom()),
                (r.left_bottom(), r.left_top()),
            ] {
                painter.add(egui::Shape::dashed_line(
                    &[a, b2],
                    egui::Stroke::new(stroke.width, if hovered { HOVER } else { DIM_TEXT }),
                    dash,
                    dash,
                ));
            }
        }
        Shape::Decl => {
            let tag = b.node.decl.map(|t| (kind_color(t.kind), t.generated));
            let (kind, generated) = tag.unwrap_or((BORDER, false));
            let kind = if generated {
                kind.gamma_multiply(0.55)
            } else {
                kind
            };
            painter.rect_filled(r, 3.0, bg);
            painter.rect_stroke(
                r,
                3.0,
                egui::Stroke::new(
                    stroke.width,
                    if hovered {
                        HOVER
                    } else {
                        kind.gamma_multiply(0.7)
                    },
                ),
                egui::StrokeKind::Inside,
            );
            draw_card_text(painter, b, r, scale, kind, generated);
            return;
        }
        _ => {
            painter.rect_filled(r, 3.0, bg);
            painter.rect_stroke(r, 3.0, stroke, egui::StrokeKind::Inside);
        }
    }

    // ── Text ──────────────────────────────────────────────────────────────
    let font = egui::FontId::monospace((10.0 * scale).clamp(4.0, 20.0));
    if font.size < 4.5 {
        return; // unreadable anyway; drawing it would only smear the box
    }
    let color = if b.node.shape == Shape::Generated {
        DIM_TEXT
    } else {
        TEXT
    };
    // Each row is cut to the room its SHAPE leaves, rather than painted whole
    // and left to the clip rectangle. A clip cuts a centred row at both ends,
    // and the half it takes off the front is the half that says what the
    // statement is - `…er.feed(b) && let Some(frame) = parser.decode_pa…` reads
    // as neither of the two calls it names. The ellipsis egui puts in its place
    // is also the only sign the reader gets that anything was dropped: the
    // parser's own ellipsis, at the far end of the string, was itself clipped.
    let rows = b.node.rows();
    let inner = painter.with_clip_rect(r.shrink(2.0 * scale));
    let line_h = font.size * 1.32;
    let total = line_h * rows.len() as f32;
    let mut y = r.center().y - total * 0.5 + line_h * 0.5;
    for row in &rows {
        // How far down the box this row sits, which is what the parallelogram's
        // shear needs to know; every other shape ignores it.
        let t = (y - r.top()) / r.height().max(1.0);
        let (il, ir) = super::layout::text_insets(b.node.shape, t);
        let (x0, x1) = (r.left() + il * scale, r.right() - ir * scale);
        let g = fit_galley(painter, row, font.size, color, x1 - x0);
        let pos = egui::pos2((x0 + x1 - g.size().x) * 0.5, y - g.size().y * 0.5);
        inner.galley(pos, g, color);
        y += line_h;
    }

    // ── Markers ───────────────────────────────────────────────────────────
    // `.await` is the whole reason an async chart is worth reading: it is where
    // the executor may hand the CPU to another task.
    if b.node.awaits {
        let d = (5.0 * scale).clamp(1.5, 7.0);
        painter.circle_filled(egui::pos2(r.right() - d * 1.6, r.top() + d * 1.6), d, AWAIT);
    }
    if b.node.try_exit {
        painter.text(
            egui::pos2(r.right() - 4.0 * scale, r.bottom() - 3.0 * scale),
            egui::Align2::RIGHT_BOTTOM,
            "?",
            egui::FontId::proportional((11.0 * scale).clamp(5.0, 16.0)),
            edge_color(EdgeKind::Try),
        );
    }
}

/// A declaration card's text: the header in the kind's colour, then the rows,
/// LEFT-aligned - it is code, read down its left edge like in the editor.
fn draw_card_text(
    painter: &egui::Painter,
    b: &Placed,
    r: egui::Rect,
    scale: f32,
    kind: egui::Color32,
    generated: bool,
) {
    let font = egui::FontId::monospace((10.0 * scale).clamp(4.0, 20.0));
    if font.size < 4.5 {
        return;
    }
    let body = if generated { DIM_TEXT } else { TEXT };
    let inner = painter.with_clip_rect(r.shrink(2.0 * scale));
    let line_h = font.size * 1.32;
    // The card's own inset is narrower than a flow box's, and asymmetric: it is
    // read down its left edge, so the room it gives its text is not the room the
    // shape reserves minus the same amount on both sides.
    let (il, ir) = super::layout::text_insets(Shape::Decl, 0.5);
    let x = r.left() + il * scale;
    let room = r.width() - (il + ir) * scale;
    let indent = " ".repeat(super::layout::CARD_ROW_INDENT);
    let rows = b.node.rows();
    let tail = usize::from(b.node.hidden > 0);
    let total = line_h * rows.len() as f32;
    let mut y = r.center().y - total * 0.5 + line_h * 0.5;
    for (i, text) in rows.iter().enumerate() {
        let (line, color) = if i == 0 {
            (text.clone(), kind)
        } else if tail == 1 && i + 1 == rows.len() {
            (format!("{indent}{text}"), DIM_TEXT)
        } else {
            (format!("{indent}{text}"), body)
        };
        let g = fit_galley(painter, &line, font.size, color, room);
        inner.galley(egui::pos2(x, y - g.size().y * 0.5), g, color);
        y += line_h;
    }
}

/// Two short lines forming an arrowhead at `to`, pointing away from `from`.
fn arrowhead(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    len: f32,
    stroke: egui::Stroke,
) {
    let dir = (to - from).normalized();
    if !dir.x.is_finite() || !dir.y.is_finite() {
        return;
    }
    let left = egui::vec2(
        dir.x * (-0.866) - dir.y * (-0.5),
        dir.x * (-0.5) + dir.y * (-0.866),
    );
    let right = egui::vec2(
        dir.x * (-0.866) - dir.y * 0.5,
        dir.x * 0.5 + dir.y * (-0.866),
    );
    painter.line_segment([to, to + left * len], stroke);
    painter.line_segment([to, to + right * len], stroke);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same clamp the Structure tab needs: at the auto-fit scale the two bounds
    /// can cross by a rounding hair, and a naive `clamp` then panics with
    /// "min > max". Ordering them is what makes it total.
    #[test]
    fn clamp_rel_survives_bounds_that_cross() {
        let v = clamp_rel(10.0, 500.0, 500.00003);
        assert!(v.is_finite());
    }

    #[test]
    fn clamp_rel_pins_a_fitting_chart_inside_the_pad() {
        assert_eq!(clamp_rel(-999.0, 500.0, 100.0), FIT_PAD);
    }

    /// An overflowing chart may be dragged, but never past its own far edge.
    #[test]
    fn clamp_rel_stops_an_overflowing_chart_at_its_edge() {
        let v = clamp_rel(999.0, 500.0, 900.0);
        assert_eq!(v, FIT_PAD);
        let v = clamp_rel(-999.0, 500.0, 900.0);
        assert_eq!(v, 500.0 - 900.0 - FIT_PAD);
    }

    /// Every shape must have a fill that is actually distinguishable — two
    /// shapes sharing a colour would make the legend a lie.
    #[test]
    fn every_shape_has_its_own_fill() {
        let all = [
            Shape::Terminal,
            Shape::Process,
            Shape::Io,
            Shape::Decision,
            Shape::Subroutine,
            Shape::Generated,
            Shape::Decl,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(fill(*a), fill(*b), "{a:?} and {b:?} share a fill");
            }
        }
    }

    const FILE: &str = "use core::fmt;\n\
                        const LIMIT: u32 = 10;\n\
                        struct Frame { a: u8 }\n\
                        impl Frame {\n    fn feed(&mut self) {}\n}\n\
                        #[entry]\nfn main() -> ! { loop {} }\n\
                        fn helper() {}\n";

    fn model() -> FileModel {
        crate::panels::flow_map::parse::parse_file(FILE).unwrap()
    }

    /// The picker lists what opens on its own - functions, containers, types -
    /// members under their container; consts and `use`s are not in it.
    #[test]
    fn the_picker_lists_what_opens_on_its_own() {
        let rows = picker_rows(&model());
        let got: Vec<(usize, &str, &str, bool)> = rows
            .iter()
            .map(|r| (r.depth, r.label.as_str(), r.key.as_str(), r.entry))
            .collect();
        assert_eq!(
            got,
            [
                (0, "struct Frame", "struct Frame", false),
                (0, "impl Frame", "impl Frame", false),
                (1, "feed  ·  fn", "Frame::feed", false),
                (0, "main  ·  entry", "main", true),
                (0, "helper  ·  fn", "helper", false),
            ]
        );
    }

    /// What each pick shows.
    #[test]
    fn the_scope_follows_the_pick() {
        let m = model();
        let at = |key: &str| m.elements.iter().position(|e| e.key == key).unwrap();
        let mut v = FlowView::default();
        let mut pick = |key: &str| {
            v.selected = key.to_string();
            scope_of(&m, &v)
        };
        assert_eq!(pick("main"), Scope::Chart(at("main")));
        assert_eq!(pick("impl Frame"), Scope::Container(at("impl Frame")));
        assert_eq!(pick("struct Frame"), Scope::Card(at("struct Frame")));
        assert_eq!(pick("const LIMIT"), Scope::Nothing, "a const is not opened");
        v.all = true;
        assert_eq!(scope_of(&m, &v), Scope::Whole);
        assert!(Scope::Whole.shows_elements() && !Scope::Chart(0).shows_elements());
    }

    /// A container's subtree is the container and all it holds, nested
    /// containers included, and stops at its next sibling.
    #[test]
    fn a_subtree_is_the_container_and_what_it_holds() {
        let m = crate::panels::flow_map::parse::parse_file(
            "mod a {\n    mod b { fn x() {} }\n    fn y() {}\n}\nfn z() {}\n",
        )
        .unwrap();
        let keys: Vec<&str> = m.elements[subtree(&m, 0)]
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        assert_eq!(keys, ["mod a", "mod a::b", "a::b::x", "a::y"]);
        let last = m.elements.len() - 1;
        assert_eq!(subtree(&m, last), last..last + 1);
    }

    /// A chart fitted whole below legible is drawn as a page instead - and a
    /// normal one is not.
    #[test]
    fn only_a_chart_too_small_to_read_turns_into_a_page() {
        let lay = FlowLayout {
            width: 400.0,
            height: 600.0,
            boxes: vec![],
            edges: vec![],
        };
        assert!(
            !too_tall(egui::vec2(1000.0, 800.0), &lay),
            "no boxes, nothing to read"
        );
        let tall = |w: f32, h: f32| {
            let mut l = crate::panels::flow_map::layout::layout(
                &crate::panels::flow_map::parse::parse_file("fn f() { a(); }")
                    .unwrap()
                    .charts[0],
            );
            l.width = w;
            l.height = h;
            too_tall(egui::vec2(1000.0, 800.0), &l)
        };
        assert!(!tall(400.0, 600.0));
        assert!(tall(400.0, 9000.0), "tall");
        assert!(tall(9000.0, 400.0), "wide");
    }

    /// The function that was unreadable fitted whole opens as a page, at a
    /// scale its boxes can be read at.
    #[test]
    fn a_tall_function_opens_readable() {
        let body: String = (0..40)
            .map(|i| format!("    if a > {i} {{ step{i}(); }}\n"))
            .collect();
        let src = format!("fn tall(a: u8) {{\n{body}}}\n");
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let lay = crate::panels::flow_map::layout::layout(&m.charts[0]);
        let comp = crate::panels::flow_map::compose::compose_scope(&m, Some(0));
        let mut v = FlowView {
            selected: "tall".to_string(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let size = egui::vec2(1000.0, 800.0);
        let (texts, _, _) = render(&ctx, &m, &lay, Some(&comp), &mut v, Vec::new(), 0.0, size);
        assert!(v.page_scale >= LEGIBLE_SCALE, "{}", v.page_scale);
        assert!(texts.iter().any(|(t, _)| t == "step0()"), "{texts:?}");
        // The decision is kept for the next frame - it is what the margin
        // before flipping back, and the toolbar's note, go by.
        assert!(v.chart_paged);
        let small = crate::panels::flow_map::parse::parse_file("fn f() { a(); }").unwrap();
        let small_lay = crate::panels::flow_map::layout::layout(&small.charts[0]);
        v.open("f".to_string());
        render(
            &ctx,
            &small,
            &small_lay,
            None,
            &mut v,
            Vec::new(),
            1.0,
            size,
        );
        assert!(!v.chart_paged, "a chart that fits is not a page");
    }

    /// A container picked on its own: its members drawn in its frame (in
    /// Implementation), or listed (in Outline) - and nothing else of the file.
    #[test]
    fn a_container_shows_only_itself() {
        let m = crate::panels::flow_map::parse::parse_file(PAGE).unwrap();
        let i = m
            .elements
            .iter()
            .position(|e| e.key == "impl Frame")
            .unwrap();
        let comp = crate::panels::flow_map::compose::compose_scope(&m, Some(i));
        assert_eq!(comp.frames.len(), 1);
        let empty = FlowLayout::default();
        let ctx = egui::Context::default();
        let size = egui::vec2(1000.0, 900.0);
        let mut v = FlowView {
            selected: "impl Frame".to_string(),
            implementation: true,
            ..Default::default()
        };
        let (texts, _, _) = render(&ctx, &m, &empty, Some(&comp), &mut v, Vec::new(), 0.0, size);
        let has = |texts: &[(String, egui::Rect)], s: &str| texts.iter().any(|(t, _)| t == s);
        assert!(
            has(&texts, "impl Frame") && has(&texts, "Frame::feed"),
            "{texts:?}"
        );
        assert!(
            !has(&texts, "main") && !has(&texts, "struct Frame"),
            "{texts:?}"
        );

        v.implementation = false;
        let (texts, _, _) = render(&ctx, &m, &empty, Some(&comp), &mut v, Vec::new(), 1.0, size);
        assert!(has(&texts, "feed(&mut self)"), "{texts:?}");
        assert!(
            !has(&texts, "helper()"),
            "the rest of the file is not listed: {texts:?}"
        );
    }

    /// A type picked on its own is its card, at no more than 100 %.
    #[test]
    fn a_type_on_its_own_is_its_card() {
        let m = crate::panels::flow_map::parse::parse_file(PAGE).unwrap();
        let i = m
            .elements
            .iter()
            .position(|e| e.key == "struct Frame")
            .unwrap();
        let comp = crate::panels::flow_map::compose::compose_scope(&m, Some(i));
        assert_eq!(comp.layout.boxes.len(), 1);
        let mut v = FlowView {
            selected: "struct Frame".to_string(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let (texts, _, _) = render(
            &ctx,
            &m,
            &empty,
            Some(&comp),
            &mut v,
            Vec::new(),
            0.0,
            egui::vec2(1000.0, 800.0),
        );
        assert!(texts.iter().any(|(t, _)| t == "struct Frame"), "{texts:?}");
        assert!(
            v.page_scale > 0.0 && v.page_scale <= 1.0,
            "{}",
            v.page_scale
        );
    }

    /// The Outline | Implementation switch is there for one function too, but
    /// does nothing: there is nothing to choose.
    #[test]
    fn the_switch_does_nothing_for_one_function() {
        let m = crate::panels::flow_map::parse::parse_file(PAGE).unwrap();
        let lay = crate::panels::flow_map::layout::layout(
            m.charts.iter().find(|c| c.key == "helper").unwrap(),
        );
        let mut v = FlowView {
            selected: "helper".to_string(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let size = egui::vec2(1000.0, 800.0);
        let (texts, _, _) = render(&ctx, &m, &lay, None, &mut v, Vec::new(), 0.0, size);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "Implementation")
            .expect("the switch is shown");
        render(
            &ctx,
            &m,
            &lay,
            None,
            &mut v,
            click_at(r.center()),
            1.0,
            size,
        );
        assert!(!v.implementation, "a disabled switch does not flip");
    }

    /// A `#[cfg(test)]` module, folded to one card in the whole file, opens in
    /// full when it is picked itself.
    #[test]
    fn a_test_module_picked_itself_opens_in_full() {
        let m = crate::panels::flow_map::parse::parse_file(
            "fn main() {}\n#[cfg(test)]\nmod tests {\n    fn a() { b(); }\n}\n",
        )
        .unwrap();
        let i = m
            .elements
            .iter()
            .position(|e| e.key == "mod tests")
            .unwrap();
        let whole = crate::panels::flow_map::compose::compose(&m);
        assert!(whole.frames.is_empty(), "folded in the whole file");
        let alone = crate::panels::flow_map::compose::compose_scope(&m, Some(i));
        assert_eq!(alone.frames.len(), 1, "a frame of its own");
        assert!(
            alone
                .layout
                .boxes
                .iter()
                .any(|b| b.node.text == "tests::a" || b.node.text == "a")
        );
    }

    /// The mode survives a save as a bit, and bits a newer build added are
    /// ignored rather than turning the view off.
    #[test]
    fn the_mode_bits_round_trip_and_ignore_what_they_do_not_know() {
        let mut v = FlowView::default();
        assert_eq!(v.mode_bits(), 0, "the default writes nothing");
        v.all = true;
        let bits = v.mode_bits();
        let mut back = FlowView::default();
        back.set_mode_bits(bits);
        assert!(back.all);
        back.set_mode_bits(bits | 0b1000_0000);
        assert!(back.all, "an unknown bit leaves the known one alone");
        back.set_mode_bits(0b1000_0000);
        assert!(!back.all);
    }

    /// Opening a function always lands on its chart, fitted - including from
    /// the whole-file view, which it leaves.
    #[test]
    fn opening_a_chart_leaves_the_whole_file_view() {
        let mut v = FlowView {
            all: true,
            zoom: 3.0,
            pan: egui::vec2(40.0, -12.0),
            ..Default::default()
        };
        v.open("Frame::feed".to_string());
        assert!(!v.all);
        assert_eq!(v.selected, "Frame::feed");
        assert_eq!((v.zoom, v.pan), (1.0, egui::Vec2::ZERO));
        // Whatever it opens as - a chart, or a page - it starts at the top.
        assert_eq!(v.pending_scroll, Some(egui::Vec2::ZERO));
    }

    #[test]
    fn a_use_row_tooltip_reads_as_code() {
        let m = crate::panels::flow_map::parse::parse_file("use a::b;\nuse c::d;\n").unwrap();
        let tip = outline_tip(&m.elements[0]);
        assert!(tip.starts_with("use a::b;\nuse c::d;\n"), "{tip}");
        assert!(tip.contains("lines 1–2"), "{tip}");
    }

    /// Every string one frame of the tab paints, plus what it reported.
    fn frame(
        ctx: &egui::Context,
        m: &FileModel,
        view: &mut FlowView,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (Vec<String>, ShowResult) {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            time: Some(time),
            events,
            ..Default::default()
        };
        let empty = FlowLayout::default();
        let mut result = ShowResult::default();
        let shapes = crate::headless::run_ui(ctx, input, |ui| {
            result = show(ui, m, &empty, None, view, "");
        })
        .shapes;
        let mut out = Vec::new();
        for s in &shapes {
            walk(&s.shape, &mut out);
        }
        (out, result)
    }

    const PAGE: &str = "struct Frame { a: u8 }\n\
                        impl Frame {\n    fn feed(&mut self) { if self.a > 1 { self.a = 0; } }\n}\n\
                        #[entry]\nfn main() -> ! {\n    helper();\n    loop {}\n}\n\
                        fn helper() {}\n";

    /// One frame of the tab with the whole-file canvas: every text shape with
    /// where it landed, every shape in paint order, and the result.
    fn paint(
        ctx: &egui::Context,
        m: &FileModel,
        comp: &Composed,
        view: &mut FlowView,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (Vec<(String, egui::Rect)>, Vec<egui::Shape>, ShowResult) {
        let empty = FlowLayout::default();
        render(
            ctx,
            m,
            &empty,
            Some(comp),
            view,
            events,
            time,
            egui::vec2(1000.0, 1200.0),
        )
    }

    /// [`paint`] with the single chart's layout and the screen size given.
    #[allow(clippy::too_many_arguments)]
    fn render(
        ctx: &egui::Context,
        m: &FileModel,
        lay: &FlowLayout,
        comp: Option<&Composed>,
        view: &mut FlowView,
        events: Vec<egui::Event>,
        time: f64,
        size: egui::Vec2,
    ) -> (Vec<(String, egui::Rect)>, Vec<egui::Shape>, ShowResult) {
        fn walk(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                other => out.push(other.clone()),
            }
        }
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(time),
            events,
            ..Default::default()
        };
        let mut result = ShowResult::default();
        let clipped = crate::headless::run_ui(ctx, input, |ui| {
            result = show(ui, m, lay, comp, view, "");
        })
        .shapes;
        let mut shapes = Vec::new();
        for s in &clipped {
            walk(&s.shape, &mut shapes);
        }
        let texts = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_owned(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect();
        (texts, shapes, result)
    }

    fn click_at(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    fn drawn_page() -> (FileModel, Composed, FlowView) {
        let m = crate::panels::flow_map::parse::parse_file(PAGE).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let v = FlowView {
            all: true,
            implementation: true,
            ..Default::default()
        };
        (m, c, v)
    }

    /// Implementation draws the file: the struct's card, the impl's frame,
    /// and the functions' flowcharts - with the declaration in the legend.
    #[test]
    fn implementation_draws_cards_frames_and_charts() {
        let (m, c, mut v) = drawn_page();
        let ctx = egui::Context::default();
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let has = |s: &str| texts.iter().any(|(t, _)| t == s);
        for want in [
            "struct Frame",
            "  a: u8",
            "impl Frame",
            "main",
            "helper()",
            "declaration",
        ] {
            assert!(has(want), "{want:?} not painted: {texts:?}");
        }
    }

    /// Frames go under everything: a frame painted after the edges would hide
    /// the arrows of the methods inside it.
    #[test]
    fn frames_are_painted_before_edges() {
        let (m, c, mut v) = drawn_page();
        let ctx = egui::Context::default();
        let (_, shapes, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let last_frame = shapes
            .iter()
            .rposition(|s| matches!(s, egui::Shape::Rect(r) if r.fill == FRAME_FILL));
        let first_edge = shapes.iter().position(|s| {
            matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == edge_color(EdgeKind::Flow))
        });
        let (Some(f), Some(e)) = (last_frame, first_edge) else {
            panic!("frame {last_frame:?}, edge {first_edge:?}")
        };
        assert!(f < e, "frame at {f}, first edge at {e}");
    }

    /// Only what is on screen is drawn: a file of two hundred functions paints
    /// a screenful of boxes, not a thousand.
    #[test]
    fn a_long_file_draws_only_what_is_visible() {
        // Every tenth function inside an `impl`, so frames are culled too.
        let src: String = (0..200)
            .map(|i| {
                let f = format!("fn f{i}() {{\n    let a = {i};\n    if a > 3 {{ go(); }}\n}}\n");
                if i % 10 == 0 {
                    format!("impl T{i} {{\n{f}}}\n")
                } else {
                    f
                }
            })
            .collect();
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let mut v = FlowView {
            all: true,
            implementation: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let starts = texts
            .iter()
            .filter(|(t, _)| t.starts_with('f') && t[1..].parse::<u32>().is_ok())
            .count();
        assert!(
            c.layout.boxes.len() > 800,
            "{} boxes in all",
            c.layout.boxes.len()
        );
        assert!(
            (1..30).contains(&starts),
            "{starts} of 200 function names painted"
        );
        let frames = texts
            .iter()
            .filter(|(t, _)| t.starts_with("impl T"))
            .count();
        assert!(
            (1..5).contains(&frames),
            "{frames} of 20 frame titles painted"
        );
        let labels = texts.iter().filter(|(t, _)| t == "YES").count();
        assert!(
            (1..30).contains(&labels),
            "{labels} of 200 edge labels painted"
        );
    }

    /// An offset asked for past the end is kept inside what can be shown.
    #[test]
    fn a_scroll_target_stays_inside_the_page() {
        let (content, view) = (egui::vec2(900.0, 4040.0), egui::vec2(840.0, 600.0));
        // 4040 - 600 = 3440 is as far down as the page goes.
        assert_eq!(
            clamp_offset(egui::vec2(14.0, 3880.0), content, view),
            egui::vec2(14.0, 3440.0)
        );
        assert_eq!(
            clamp_offset(egui::vec2(200.0, 0.0), content, view).x,
            60.0,
            "and 900 - 840 as far right"
        );
        assert_eq!(
            clamp_offset(egui::vec2(-5.0, -5.0), content, view),
            egui::Vec2::ZERO
        );
        // A page shorter than the view does not scroll at all.
        assert_eq!(
            clamp_offset(egui::vec2(30.0, 30.0), egui::vec2(100.0, 100.0), view),
            egui::Vec2::ZERO
        );
    }

    /// Narrowing the panel rescales the page, and the line at the top stays
    /// the line at the top.
    #[test]
    fn a_resize_keeps_the_reading_position() {
        // One wide function (a match of many arms) so the width-fit moves.
        let arms: String = (0..12).map(|i| format!("{i} => a{i}(),\n")).collect();
        let mut src = format!("fn wide(x: u8) {{\n    match x {{\n{arms}_ => {{}}\n    }}\n}}\n");
        for i in 0..60 {
            src.push_str(&format!(
                "fn f{i}() {{\n    let a = {i};\n    if a > 3 {{ go(); }}\n}}\n"
            ));
        }
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let mut v = FlowView {
            all: true,
            implementation: true,
            pending_scroll: Some(egui::vec2(0.0, 3000.0)),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let top = |v: &FlowView| (v.last_offset.y - FIT_PAD) / v.page_scale;
        let wide = egui::vec2(1100.0, 900.0);
        for t in 0..3 {
            render(
                &ctx,
                &m,
                &empty,
                Some(&c),
                &mut v,
                Vec::new(),
                t as f64,
                wide,
            );
        }
        let (before, s1) = (top(&v), v.page_scale);
        for t in 3..6 {
            render(
                &ctx,
                &m,
                &empty,
                Some(&c),
                &mut v,
                Vec::new(),
                t as f64,
                egui::vec2(900.0, 900.0),
            );
        }
        assert!(
            (v.page_scale - s1).abs() > 0.01,
            "the scale moved: {s1} -> {}",
            v.page_scale
        );
        assert!(
            (top(&v) - before).abs() < 2.0,
            "top line {before} -> {}",
            top(&v)
        );
    }

    /// The single chart keeps its edge labels at every scale, as it always
    /// has; the whole-file page hides them only where they cannot be read -
    /// and says so in the toolbar.
    #[test]
    fn edge_labels_follow_the_view() {
        // A medium function - it fits legibly, so it stays a chart, not a page
        // - zoomed out by hand below legible.
        let body: String = (0..4)
            .map(|i| format!("    if a > {i} {{ b(); }}\n"))
            .collect();
        let m =
            crate::panels::flow_map::parse::parse_file(&format!("fn mid(a: u8) {{\n{body}}}\n"))
                .unwrap();
        let lay = crate::panels::flow_map::layout::layout(&m.charts[0]);
        assert!(
            !too_tall(egui::vec2(1000.0, 720.0), &lay),
            "it is not a page"
        );
        let ctx = egui::Context::default();
        let mut single = FlowView {
            selected: "mid".to_string(),
            zoom: 0.3,
            ..Default::default()
        };
        let (texts, _, _) = render(
            &ctx,
            &m,
            &lay,
            None,
            &mut single,
            Vec::new(),
            0.0,
            egui::vec2(1000.0, 800.0),
        );
        assert!(single.last_scale < LEGIBLE_SCALE, "{}", single.last_scale);
        assert!(texts.iter().any(|(t, _)| t == "YES"), "{texts:?}");

        let (m, c, mut page_view) = drawn_page();
        page_view.all_zoom = 0.3;
        let ctx = egui::Context::default();
        paint(&ctx, &m, &c, &mut page_view, Vec::new(), 0.0);
        let (texts, _, _) = paint(&ctx, &m, &c, &mut page_view, Vec::new(), 1.0);
        assert!(!texts.iter().any(|(t, _)| t == "YES"), "{texts:?}");
        assert!(
            texts.iter().any(|(t, _)| t.starts_with("at 30% — zoom in")),
            "{texts:?}"
        );
    }

    /// An edge reaches past its points by its label, so culling on the points
    /// alone would drop a label that is on screen.
    #[test]
    fn an_edge_reaches_as_far_as_its_label() {
        let mut e = Edge {
            pts: vec![(0.0, 0.0), (0.0, 50.0)],
            label: String::new(),
            kind: EdgeKind::Flow,
            arrow: true,
        };
        let bare = edge_reach(&e, 1.0, true);
        e.label = "Some(frame) if frame.ok".to_string();
        assert!(edge_reach(&e, 1.0, true) > bare + 40.0);
        assert_eq!(edge_reach(&e, 1.0, false), bare, "no labels, no reach");
    }

    /// A card's tooltip holds what the card had to cut.
    #[test]
    fn a_card_tooltip_is_uncut() {
        let long = "const TABLE: [u16; 8] = [0x0001, 0x0002, 0x0004, 0x0008, 0x0010, 0x0020, 0x0040, 0x0080];\n";
        let m = crate::panels::flow_map::parse::parse_file(long).unwrap();
        let card = crate::panels::flow_map::compose::card(&m, 0);
        assert!(card.text.ends_with('…'), "{}", card.text);
        let tip = card_tip(&m, 0);
        assert!(tip.contains("0x0080];"), "{tip}");
        assert!(tip.contains("line 1"), "{tip}");
    }

    /// The rows too, not only the header: a field whose type is longer than a
    /// card row is cut on the card and whole in the tooltip.
    #[test]
    fn a_card_tooltip_keeps_long_rows_whole() {
        let ty = "core::cell::RefCell<heapless::Vec<embassy_time::Instant, 32>>";
        let src = format!("struct Log {{ entries_since_boot_by_source: {ty} }}\n");
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let card = crate::panels::flow_map::compose::card(&m, 0);
        assert!(card.detail[0].ends_with('…'), "{:?}", card.detail);
        assert!(card_tip(&m, 0).contains(ty), "{}", card_tip(&m, 0));
    }

    /// Hovering a card on the page shows that uncut text.
    #[test]
    fn hovering_a_card_shows_its_whole_text() {
        let long = "const TABLE: [u16; 8] = [0x0001, 0x0002, 0x0004, 0x0008, 0x0010, 0x0020, 0x0040, 0x0080];\nfn main() {}\n";
        let m = crate::panels::flow_map::parse::parse_file(long).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let mut v = FlowView {
            all: true,
            implementation: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 10.0);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t.starts_with("const TABLE"))
            .expect("the card is on screen");
        let at = vec![egui::Event::PointerMoved(r.center())];
        paint(&ctx, &m, &c, &mut v, at, 11.0);
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 12.0);
        assert!(
            texts.iter().any(|(t, _)| t.contains("0x0080];")),
            "the tooltip shows the uncut declaration: {texts:?}"
        );
    }

    /// Scrolling to the LAST function cannot scroll past the end of the page:
    /// the bottom of the page stays at the bottom of the view, so that
    /// function is drawn low on the screen, not pinned to the top above a
    /// band of empty canvas.
    #[test]
    fn a_scroll_to_the_end_stops_at_the_end() {
        let src: String = (0..30)
            .map(|i| format!("fn f{i}() {{\n    let a = {i};\n    if a > 3 {{ go(); }}\n}}\n"))
            .collect();
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let (x, y) = c.anchor(&m, "f29").unwrap();
        let mut v = FlowView {
            all: true,
            implementation: true,
            pending_scroll: Some(egui::vec2(x, y)),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let size = egui::vec2(1000.0, 800.0);
        let empty = FlowLayout::default();
        let (texts, _, _) = render(&ctx, &m, &empty, Some(&c), &mut v, Vec::new(), 0.0, size);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "f29")
            .expect("the last function is on screen");
        assert!(
            r.min.y > size.y * 0.4,
            "f29 at y {} - the page scrolled past its end",
            r.min.y
        );
    }

    #[test]
    fn a_file_switch_goes_back_to_the_top_of_the_page() {
        let mut v = FlowView {
            all_zoom: 2.5,
            page_scale: 0.2,
            chart_paged: true,
            ..Default::default()
        };
        v.reset_page();
        assert_eq!(
            (v.all_zoom, v.pending_scroll),
            (1.0, Some(egui::Vec2::ZERO))
        );
        assert_eq!(v.page_scale, 0.0, "no note left over from the last page");
        v.reset_file();
        assert!(v.lists_to_top && !v.chart_paged);
    }

    /// Two blocks that read the same are told apart by their line - in the
    /// picker and in the toolbar alike.
    #[test]
    fn twin_containers_are_told_apart() {
        let m = crate::panels::flow_map::parse::parse_file(
            "struct Uart;\nimpl Uart { fn a() {} }\nimpl Uart { fn b() {} }\nimpl Clone for Uart { fn clone(&self) -> Self { Uart } }\n",
        )
        .unwrap();
        let at = |key: &str| m.elements.iter().position(|e| e.key == key).unwrap();
        assert_eq!(pick_label(&m, at("impl Uart")), "impl Uart · line 2");
        assert_eq!(pick_label(&m, at("impl Uart#2")), "impl Uart · line 3");
        assert_eq!(
            pick_label(&m, at("impl Clone for Uart")),
            "impl Clone for Uart"
        );
        assert_eq!(pick_label(&m, at("struct Uart")), "struct Uart");
    }

    /// Once paged, a chart goes back to being fitted only when it fits clearly
    /// above legible - no flipping at the edge.
    #[test]
    fn the_page_decision_has_a_margin() {
        let mut lay = crate::panels::flow_map::layout::layout(
            &crate::panels::flow_map::parse::parse_file("fn f() { a(); }")
                .unwrap()
                .charts[0],
        );
        lay.width = 100.0;
        // Fits at just over LEGIBLE_SCALE: 0.46.
        lay.height = (800.0 - 2.0 * FIT_PAD) / 0.46;
        let avail = egui::vec2(1000.0, 800.0);
        assert!(!stays_paged(avail, &lay, false), "fitted: it reads");
        assert!(
            stays_paged(avail, &lay, true),
            "paged: stays paged inside the margin"
        );
        lay.height = (800.0 - 2.0 * FIT_PAD) / 0.6;
        assert!(
            !stays_paged(avail, &lay, true),
            "clearly legible: back to fitted"
        );
    }

    /// The legend and hint above a chart are measured, not assumed: a narrow
    /// panel wraps them onto more rows.
    #[test]
    fn the_header_above_a_chart_is_measured() {
        let m = crate::panels::flow_map::parse::parse_file("fn f() { a(); }").unwrap();
        let lay = crate::panels::flow_map::layout::layout(&m.charts[0]);
        let ctx = egui::Context::default();
        let mut v = FlowView {
            selected: "f".to_string(),
            ..Default::default()
        };
        render(
            &ctx,
            &m,
            &lay,
            None,
            &mut v,
            Vec::new(),
            0.0,
            egui::vec2(1400.0, 800.0),
        );
        let wide = v.header_h;
        render(
            &ctx,
            &m,
            &lay,
            None,
            &mut v,
            Vec::new(),
            1.0,
            egui::vec2(320.0, 800.0),
        );
        assert!(
            wide > 10.0 && v.header_h > wide,
            "wide {wide}, narrow {}",
            v.header_h
        );
    }

    /// A box paints the START of every row, and never wider than its own shape
    /// leaves room for.
    ///
    /// `box_size` clamps a box to `MAX_W`, so a label at the parser's 56-char
    /// cap is some 40 units wider than the box it is given. The painter used to
    /// draw the whole string centred and leave the overflow to
    /// `with_clip_rect`, and a clip takes as much off the head of a centred row
    /// as off its tail - which is how a condition reached the screen reading
    /// `…er.feed(b) && let Some(frame) = parser.decode_pa…`, naming neither of
    /// the two calls in it. Worse, the parser's own ellipsis sits at the far end
    /// and was clipped with everything else, so a cut box looked intact.
    ///
    /// egui records a galley at the width it laid out whether or not the clip
    /// shows it, so `rect.width()` below is what was really measured out, not
    /// what happened to be visible. And the bare test context is faithful for
    /// this one measurement: egui-phosphor adds its face to the PROPORTIONAL
    /// family only, so the monospace chain here is the app's.
    #[test]
    fn a_box_paints_the_head_of_each_row_and_never_wider_than_its_shape_allows() {
        let src = "fn f() {\n    \
                   if parser.feed(b) && parser.decode_payload().is_some() && nav.is_leaf() {\n        \
                   crate::utils::i2c1_display::write_text(&mut display, nav.selected_node());\n    \
                   }\n}\n";
        let m = crate::panels::flow_map::parse::parse_file(src).unwrap();
        let lay = crate::panels::flow_map::layout::layout(&m.charts[0]);
        let ctx = egui::Context::default();
        let mut v = FlowView {
            selected: "f".to_string(),
            ..Default::default()
        };
        let (texts, _, _) = render(
            &ctx,
            &m,
            &lay,
            None,
            &mut v,
            Vec::new(),
            0.0,
            egui::vec2(1000.0, 800.0),
        );
        let scale = v.last_scale;
        assert!(scale > 0.0, "the chart was never drawn");

        let mut cut = 0;
        for b in &lay.boxes {
            for row in b.node.rows() {
                // Short rows can neither overflow nor be told apart from the
                // toolbar's own words by a prefix.
                if row.chars().count() < 20 {
                    continue;
                }
                let (drawn, rect) = texts
                    .iter()
                    .find(|(t, _)| {
                        let head = t.trim_end_matches('…');
                        head.chars().count() >= 8 && row.starts_with(head)
                    })
                    .unwrap_or_else(|| {
                        panic!(
                            "nothing painted starts with {row:?}; painted: {:?}",
                            texts.iter().map(|(t, _)| t).collect::<Vec<_>>()
                        )
                    });
                let room =
                    crate::panels::flow_map::layout::text_room(b.node.shape, b.w, 0.5) * scale;
                assert!(
                    rect.width() <= room + 0.5,
                    "{:?} laid out {} px of text into {room} px of room: {drawn:?}",
                    b.node.shape,
                    rect.width()
                );
                cut += usize::from(drawn.ends_with('…'));
            }
        }
        assert!(cut > 0, "nothing was cut at all, so this proves nothing");
    }

    /// A test module nested inside the one that was picked is drawn too - a
    /// frame, not a folded card.
    #[test]
    fn a_picked_test_module_draws_its_nested_modules() {
        let m = crate::panels::flow_map::parse::parse_file(
            "#[cfg(test)]\nmod tests {\n    mod util { fn u() {} }\n    fn t() {}\n}\n",
        )
        .unwrap();
        let alone = crate::panels::flow_map::compose::compose_scope(&m, Some(0));
        let titles: Vec<&str> = alone.frames.iter().map(|f| f.title.as_str()).collect();
        assert_eq!(titles, ["mod tests", "mod util"]);
    }

    /// The switch flipped in the toolbar takes effect next frame: the frame it
    /// is clicked in still draws what the canvas was built for (the list), not
    /// a canvas left over from another scope.
    #[test]
    fn the_switch_takes_effect_next_frame() {
        let m = crate::panels::flow_map::parse::parse_file(PAGE).unwrap();
        let i = m
            .elements
            .iter()
            .position(|e| e.key == "impl Frame")
            .unwrap();
        let h = m.elements.iter().position(|e| e.key == "helper").unwrap();
        // What the driver still has cached from before: another scope's page.
        let stale = crate::panels::flow_map::compose::compose_scope(&m, Some(h));
        let mut v = FlowView {
            selected: m.elements[i].key.clone(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(1000.0, 900.0);
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.0, size);
        let (_, r) = texts.iter().find(|(t, _)| t == "Implementation").unwrap();
        let (texts, _, _) = render(
            &ctx,
            &m,
            &empty,
            Some(&stale),
            &mut v,
            click_at(r.center()),
            1.0,
            size,
        );
        assert!(v.implementation);
        assert!(
            !texts.iter().any(|(t, _)| t == "helper"),
            "the stale canvas is not drawn: {texts:?}"
        );
    }

    /// Each scope keeps its own place in its list: a container opened from a
    /// whole-file list scrolled far down opens at its OWN top, not at the
    /// whole file's offset clamped to its bottom.
    #[test]
    fn each_list_keeps_its_own_place() {
        let mut src: String = (0..120).map(|i| format!("fn f{i}() {{}}\n")).collect();
        src.push_str("mod tail {\n");
        for i in 0..40 {
            src.push_str(&format!("    fn g{i}() {{}}\n"));
        }
        src.push_str("}\n");
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let mut v = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(900.0, 600.0);
        let wheel = |dy: f32| {
            vec![
                egui::Event::PointerMoved(egui::pos2(400.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, dy),
                    phase: egui::TouchPhase::Move,
                    modifiers: egui::Modifiers::NONE,
                },
            ]
        };
        render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.0, size);
        for t in 1..40 {
            render(
                &ctx,
                &m,
                &empty,
                None,
                &mut v,
                wheel(-400.0),
                t as f64 * 0.05,
                size,
            );
        }
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 3.0, size);
        assert!(
            !texts.iter().any(|(t, _)| t == "f0()"),
            "the whole-file list has scrolled away from its top"
        );
        v.open("mod tail".to_string());
        v.implementation = false;
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 4.0, size);
        assert!(
            texts.iter().any(|(t, _)| t == "g0()"),
            "the container opens at its own top: {texts:?}"
        );

        // A new file: its list starts at the top too.
        v.all = true;
        v.reset_file();
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 5.0, size);
        assert!(texts.iter().any(|(t, _)| t == "f0()"), "{texts:?}");
        assert!(!v.lists_to_top, "applied once");
    }

    /// The Outline | Implementation switch sits in the toolbar in the
    /// whole-file view.
    #[test]
    fn the_toolbar_switches_between_outline_and_implementation() {
        let (m, c, mut v) = drawn_page();
        v.implementation = false;
        let ctx = egui::Context::default();
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "Implementation")
            .expect("the switch is shown");
        paint(&ctx, &m, &c, &mut v, click_at(r.center()), 1.0);
        assert!(v.implementation);
    }

    /// A call on the whole-file canvas goes to the function it calls - by
    /// scrolling, since that function is on the same canvas - and the view
    /// stays the whole file.
    #[test]
    fn a_call_scrolls_to_its_function() {
        let (m, c, mut v) = drawn_page();
        let ctx = egui::Context::default();
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "helper()")
            .expect("the call box is on screen");
        let (_, _, res) = paint(&ctx, &m, &c, &mut v, click_at(r.center()), 1.0);
        assert!(v.all, "still the whole file");
        assert_eq!(res.open_chart, None);
        assert_eq!(res.goto_line, Some(7), "the editor goes to the call");
        let (x, y) = c.anchor(&m, "helper").unwrap();
        assert_eq!(
            v.pending_scroll,
            Some(egui::vec2((x - FIT_PAD).max(0.0), (y - FIT_PAD).max(0.0)))
        );
        // The next frame consumes it.
        paint(&ctx, &m, &c, &mut v, Vec::new(), 2.0);
        assert_eq!(v.pending_scroll, None);
    }

    /// Zooming keeps the canvas point under the pointer where it was.
    #[test]
    fn zoom_keeps_the_point_under_the_pointer() {
        let (offset, local) = (egui::vec2(300.0, 1200.0), egui::vec2(250.0, 180.0));
        for (old, new) in [(1.0, 1.5), (0.8, 0.6), (1.0, 1.0)] {
            let before = (offset + local - egui::vec2(FIT_PAD, FIT_PAD)) / old;
            let o2 = zoom_about(offset, local, old, new);
            let after = (o2 + local - egui::vec2(FIT_PAD, FIT_PAD)) / new;
            assert!((before - after).length() < 1e-3, "{old} -> {new}");
        }
        // Zooming out at the top would ask for a negative offset: never
        // scrolled before the top of the page.
        assert_eq!(
            zoom_about(egui::Vec2::ZERO, egui::vec2(500.0, 400.0), 1.0, 0.5),
            egui::Vec2::ZERO
        );
    }

    /// Both mode bits survive a save together.
    #[test]
    fn the_implementation_bit_rides_with_the_all_bit() {
        let mut v = FlowView {
            all: true,
            implementation: true,
            ..Default::default()
        };
        let bits = v.mode_bits();
        v.set_mode_bits(0);
        assert!(!v.all && !v.implementation);
        v.set_mode_bits(bits);
        assert!(v.all && v.implementation);
        v.set_mode_bits(MODE_IMPLEMENTATION);
        assert!(!v.all && v.implementation, "the bits are independent");
    }

    /// "All — whole file" paints one row per element - kind word and
    /// declaration - and names itself in the picker.
    #[test]
    fn the_whole_file_view_lists_every_element() {
        let m = model();
        let mut view = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let (texts, _) = frame(&ctx, &m, &mut view, Vec::new(), 0.0);
        assert!(texts.iter().any(|t| t == ALL_LABEL), "{texts:?}");
        for e in &m.elements {
            assert!(
                texts.contains(&e.signature),
                "row for {} missing: {texts:?}",
                e.key
            );
            assert!(texts.iter().any(|t| t == e.kind.word()), "{texts:?}");
        }
        // It is a list: none of the chart view's legend.
        assert!(!texts.iter().any(|t| t == "decision"), "{texts:?}");
    }

    /// A click on a row jumps the editor and keeps the list; a double click on
    /// a function opens its chart.
    #[test]
    fn a_click_jumps_and_a_double_click_opens() {
        let m = model();
        let mut view = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let (_, first) = frame(&ctx, &m, &mut view, Vec::new(), 0.0);
        assert!(first.goto_line.is_none());

        // Where the `helper` row lands depends on the toolbar above the list,
        // so find it the way a user would: click down the list until the
        // click jumps to `helper`'s line.
        let helper = m.elements.iter().position(|e| e.key == "helper").unwrap();
        let mut hit = None;
        for y in (60..600).step_by(3) {
            let pos = egui::pos2(300.0, y as f32);
            let (_, r) = frame(
                &ctx,
                &m,
                &mut view,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                1.0 + y as f64,
            );
            if r.goto_line == Some(m.elements[helper].ident_line) {
                hit = Some(pos);
                break;
            }
        }
        let pos = hit.expect("clicking the helper row jumps to its line");
        assert!(view.all, "a single click keeps the list");

        // Two clicks in quick succession on the same row: a double click.
        let click = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let t = 5000.0;
        frame(&ctx, &m, &mut view, vec![egui::Event::PointerMoved(pos)], t);
        frame(
            &ctx,
            &m,
            &mut view,
            vec![click(true), click(false)],
            t + 0.05,
        );
        let (_, r) = frame(
            &ctx,
            &m,
            &mut view,
            vec![click(true), click(false)],
            t + 0.10,
        );
        assert_eq!(r.open_chart.as_deref(), Some("helper"));
    }

    #[test]
    fn every_edge_kind_has_its_own_colour_except_the_two_that_mean_the_same() {
        // Break and Return are both "leave through the right-hand lane", and
        // sharing one colour is deliberate — they are the same gesture.
        assert_eq!(edge_color(EdgeKind::Break), edge_color(EdgeKind::Return));
        for k in [
            EdgeKind::Flow,
            EdgeKind::Back,
            EdgeKind::Continue,
            EdgeKind::Try,
        ] {
            assert_ne!(edge_color(k), edge_color(EdgeKind::Break));
        }
    }
    const CARET: &str = concat!(
        "struct Frame { a: u8 }\n", // 1
        "impl Frame {\n",           // 2
        "    fn feed(&mut self) {\n",
        "        let x = 1;\n", // 4
        "        if x > 0 {\n",
        "            go();\n", // 6
        "        }\n",
        "    }\n", // 8
        "}\n",
        "\n", // 10
        "#[cfg(test)]\n",
        "mod tests {\n", // 12
        "    fn t() { a(); }\n",
        "}\n", // 14
    );

    /// What draws a line, per kind of element.
    #[test]
    fn a_line_is_revealed_at_what_draws_it() {
        let m = crate::panels::flow_map::parse::parse_file(CARET).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let ext = |key: &str| {
            let x = c
                .extents
                .iter()
                .find(|x| m.elements[x.element].key == key)
                .unwrap();
            egui::Rect::from_min_size(egui::pos2(x.x, x.y), egui::vec2(x.w, x.h))
        };
        let boxed = |line: usize, text: &str| {
            let b = c
                .layout
                .boxes
                .iter()
                .find(|b| b.node.line == line && b.node.text.contains(text))
                .unwrap_or_else(|| panic!("no box {text:?} on line {line}"));
            egui::Rect::from_min_size(egui::pos2(b.x, b.y), egui::vec2(b.w, b.h))
        };
        let at = |line: usize| reveal_rect(&m, &c, line);

        assert_eq!(at(1), Some(ext("struct Frame")), "a type is its card");
        let imp = ext(&m.elements[crate::panels::flow_map::element_at_line(&m, 2).unwrap()].key);
        assert_eq!(
            at(2),
            Some(egui::Rect::from_min_size(
                imp.min,
                egui::vec2(imp.width(), FRAME_TITLE_H)
            )),
            "a container's own line is its title bar"
        );
        assert_eq!(at(4), Some(boxed(4, "let x")));
        assert_eq!(at(6), Some(boxed(6, "go")));
        // The closing brace of the `if` belongs to the last statement above.
        assert_eq!(at(7), Some(boxed(6, "go")));
        // The function's own line: START, not END, which carries it too.
        let start = boxed(3, "feed");
        assert_eq!(at(3), Some(start));
        assert!(
            c.layout
                .boxes
                .iter()
                .any(|b| b.node.line == 3 && b.node.text == "END" && b.y > start.max.y),
            "END shares the line and is below"
        );
        assert_eq!(at(10), None, "between items");
        assert_eq!(at(13), Some(ext("mod tests")), "folded: the module's card");
    }

    /// A line of something a scoped page does not show reveals nothing.
    #[test]
    fn a_line_outside_the_scope_is_not_revealed() {
        let m = crate::panels::flow_map::parse::parse_file(CARET).unwrap();
        let feed = m
            .elements
            .iter()
            .position(|e| e.key == "Frame::feed")
            .unwrap();
        let c = crate::panels::flow_map::compose::compose_scope(&m, Some(feed));
        assert_eq!(reveal_rect(&m, &c, 1), None, "a sibling type");
        assert_eq!(reveal_rect(&m, &c, 2), None, "its own impl's header");
        assert!(reveal_rect(&m, &c, 4).is_some());
    }

    /// Enough of it on screen already: the view stays. Out of sight: its top
    /// a third of the way down, and sideways only when it has to.
    #[test]
    fn a_reveal_moves_the_view_only_as_far_as_it_must() {
        let view = egui::vec2(800.0, 600.0);
        let r = |x: f32, y: f32, w: f32, h: f32| {
            egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h))
        };
        let top = egui::Vec2::ZERO;
        assert_eq!(reveal_offset(top, view, r(100.0, 100.0, 200.0, 50.0)), None);
        // Taller than the view, but its head is in sight.
        assert_eq!(
            reveal_offset(top, view, r(100.0, 100.0, 200.0, 2000.0)),
            None
        );
        assert_eq!(
            reveal_offset(top, view, r(100.0, 1500.0, 200.0, 50.0)),
            Some(egui::vec2(0.0, 1300.0))
        );
        // Cut by the bottom edge counts as out of sight.
        assert_eq!(
            reveal_offset(top, view, r(100.0, 580.0, 200.0, 50.0)),
            Some(egui::vec2(0.0, 380.0))
        );
        assert_eq!(
            reveal_offset(egui::vec2(0.0, 1000.0), view, r(100.0, 150.0, 200.0, 50.0)),
            Some(egui::Vec2::ZERO),
            "above: up, but not past the top"
        );
        assert_eq!(
            reveal_offset(
                egui::vec2(0.0, 1000.0),
                view,
                r(1200.0, 1100.0, 200.0, 50.0)
            ),
            Some(egui::vec2(1200.0 - FIT_PAD, 1000.0)),
            "off to the right only"
        );
    }

    /// A canvas rect in the page's content: scaled, past the padding.
    #[test]
    fn a_canvas_rect_lands_past_the_padding() {
        let r = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(30.0, 40.0));
        assert_eq!(
            content_rect(r, 2.0),
            egui::Rect::from_min_size(
                egui::pos2(FIT_PAD + 20.0, FIT_PAD + 40.0),
                egui::vec2(60.0, 80.0)
            )
        );
    }

    /// A long file, one function per line: `fn f{i}` is on line i+1.
    fn long_list() -> FileModel {
        let src: String = (0..120).map(|i| format!("fn f{i}() {{}}\n")).collect();
        crate::panels::flow_map::parse::parse_file(&src).unwrap()
    }

    fn wheel(dy: f32) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(egui::pos2(400.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, dy),
                phase: egui::TouchPhase::Move,
                modifiers: egui::Modifiers::NONE,
            },
        ]
    }

    /// The caret moved onto a row far down the list: the row comes into view
    /// in the same frame, a third of the way down. Once - the wheel then
    /// scrolls the list away from it freely. And a row in sight of the list
    /// as it is SCROLLED stays where it is: the list's own offset was read.
    #[test]
    fn the_list_follows_a_caret_that_moved_and_only_then() {
        let m = long_list();
        let mut v = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(900.0, 600.0);
        let mut t = 0.0;
        let mut frame = |v: &mut FlowView, events: Vec<egui::Event>| {
            t += 0.05;
            render(&ctx, &m, &empty, None, v, events, t, size).0
        };
        let at = |texts: &[(String, egui::Rect)], name: &str| {
            texts.iter().find(|(t, _)| t == name).map(|(_, r)| r.top())
        };
        frame(&mut v, Vec::new());
        v.reveal_line = Some(100);
        let texts = frame(&mut v, Vec::new());
        assert_eq!(v.reveal_line, None, "taken");
        let y = at(&texts, "f99()").unwrap_or_else(|| panic!("f99 not in view: {texts:?}"));
        assert!(
            (150.0..300.0).contains(&y),
            "not a third of the way down: {y}"
        );

        let before = at(&texts, "f95()");
        assert!(before.is_some());
        v.reveal_line = Some(96);
        let texts = frame(&mut v, Vec::new());
        assert_eq!(at(&texts, "f95()"), before, "in sight, yet moved");

        for _ in 0..30 {
            frame(&mut v, wheel(400.0));
        }
        let texts = frame(&mut v, Vec::new());
        assert!(
            !texts.iter().any(|(t, _)| t == "f99()"),
            "scrolled back to f99 against the wheel"
        );
    }

    /// The last row comes in at the bottom - the list does not scroll past its
    /// end to put it a third of the way down over blank space.
    #[test]
    fn the_last_row_comes_in_at_the_bottom() {
        let m = long_list();
        let mut v = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(900.0, 600.0);
        render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.0, size);
        v.reveal_line = Some(120);
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.1, size);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "f119()")
            .expect("the last row is in view");
        assert!(r.bottom() > 560.0, "{r:?}");
    }

    /// A container's list and a caret outside it: nothing to reveal there.
    #[test]
    fn a_caret_outside_the_container_leaves_its_list_alone() {
        let mut src: String = (0..60).map(|i| format!("fn f{i}() {{}}\n")).collect();
        src.push_str("mod tail {\n");
        for i in 0..60 {
            src.push_str(&format!("    fn g{i}() {{}}\n"));
        }
        src.push_str("}\n");
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let mut v = FlowView::default();
        v.open("mod tail".to_string());
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(900.0, 600.0);
        render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.0, size);
        v.reveal_line = Some(3);
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.1, size);
        assert!(texts.iter().any(|(t, _)| t == "g0()"), "{texts:?}");
        // And one inside it is revealed.
        v.reveal_line = Some(61 + 50);
        let (texts, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.2, size);
        assert!(texts.iter().any(|(t, _)| t == "g49()"), "{texts:?}");
    }

    /// A row already in sight does not move the list.
    #[test]
    fn a_row_in_sight_does_not_move_the_list() {
        let m = long_list();
        let mut v = FlowView {
            all: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        let empty = FlowLayout::default();
        let size = egui::vec2(900.0, 600.0);
        let (before, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.0, size);
        v.reveal_line = Some(4);
        render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.1, size);
        let (after, _, _) = render(&ctx, &m, &empty, None, &mut v, Vec::new(), 0.2, size);
        let y = |texts: &[(String, egui::Rect)]| {
            texts
                .iter()
                .find(|(t, _)| t == "f3()")
                .map(|(_, r)| r.top())
        };
        assert!(y(&before).is_some());
        assert_eq!(y(&before), y(&after));
    }

    /// The whole-file page brings the caret's statement into view - and only
    /// the frame it is asked; the wheel then scrolls away from it.
    #[test]
    fn the_page_follows_a_caret_that_moved_and_only_then() {
        let src: String = (0..200)
            .map(|i| format!("fn f{i}() {{\n    let a = {i};\n    if a > 3 {{ go(); }}\n}}\n"))
            .collect();
        let m = crate::panels::flow_map::parse::parse_file(&src).unwrap();
        let c = crate::panels::flow_map::compose::compose(&m);
        let mut v = FlowView {
            all: true,
            implementation: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        paint(&ctx, &m, &c, &mut v, Vec::new(), 0.0);
        let line = m
            .elements
            .iter()
            .find(|e| e.key == "f151")
            .unwrap()
            .ident_line
            + 1;
        v.reveal_line = Some(line);
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 0.1);
        let (_, r) = texts
            .iter()
            .find(|(t, _)| t == "let a = 151;")
            .expect("the statement is on screen");
        assert!(
            (100.0..700.0).contains(&r.top()),
            "a third of the way down, not at an edge: {r:?}"
        );
        let there = v.last_offset;

        // Asked again while it is in sight: nothing moves.
        v.reveal_line = Some(line);
        paint(&ctx, &m, &c, &mut v, Vec::new(), 0.2);
        assert_eq!(v.last_offset, there);

        // Nor once the reader has scrolled a little, with it still in sight:
        // it is judged from where the page IS, not from its top.
        paint(&ctx, &m, &c, &mut v, wheel(60.0), 0.25);
        for k in 0..20 {
            paint(&ctx, &m, &c, &mut v, Vec::new(), 0.3 + k as f64 * 0.05);
        }
        let nudged = v.last_offset;
        assert!(
            nudged.y < there.y - 1.0 && nudged.y > there.y - 300.0,
            "{nudged:?} against {there:?}"
        );
        v.reveal_line = Some(line);
        let (texts, _, _) = paint(&ctx, &m, &c, &mut v, Vec::new(), 1.4);
        assert_eq!(v.last_offset, nudged);
        assert!(texts.iter().any(|(t, _)| t == "let a = 151;"));

        for k in 0..10 {
            paint(&ctx, &m, &c, &mut v, wheel(400.0), 1.5 + k as f64 * 0.05);
        }
        paint(&ctx, &m, &c, &mut v, Vec::new(), 3.0);
        assert!(
            v.last_offset.y < there.y - 1000.0,
            "{:?} against {there:?}",
            v.last_offset
        );
    }

    /// A chart fitted whole has nothing to scroll: the request is dropped
    /// rather than acted on later, in a view the reader has scrolled since.
    #[test]
    fn a_fitted_chart_drops_the_request() {
        let m = crate::panels::flow_map::parse::parse_file("fn f() {\n    a();\n}\n").unwrap();
        let lay = crate::panels::flow_map::layout::layout(&m.charts[0]);
        let mut v = FlowView::default();
        v.open("f".to_string());
        v.reveal_line = Some(2);
        let ctx = egui::Context::default();
        render(
            &ctx,
            &m,
            &lay,
            None,
            &mut v,
            Vec::new(),
            0.0,
            egui::vec2(900.0, 700.0),
        );
        assert!(!v.chart_paged);
        assert_eq!(v.reveal_line, None);
    }
}
