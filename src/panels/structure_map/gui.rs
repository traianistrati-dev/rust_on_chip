//! Draw the module diagram: fit-to-panel with zoom + pan, dashed containment
//! lines, solid dependency arrows, and clickable nodes (click → the caller
//! opens that node's file in the editor).

use super::calls::CallEdge;
use super::layout::{
    GraphLayout, HEADER_H, MARGIN, MAX_SYMBOL_ROWS, ROW_H, ROW_NAME_CHARS, recompute_bounds,
    shown_rows,
};
use super::parse::{ModuleGraph, SymKind};
use eframe::egui;

/// Session view state of the diagram (not persisted). The BASE zoom is
/// automatic — the whole diagram fits the panel with [`FIT_PAD`] padding,
/// recomputed every frame so panel/window resizes always track. `zoom` is a
/// user multiplier on top (Ctrl+± while hovering the diagram, Ctrl+0 resets)
/// — at 1.0 the diagram always fits; above it, scrollbars take over.
pub struct StructureView {
    /// User zoom multiplier over the auto-fit base (1.0 = pure auto-fit).
    pub zoom: f32,
    /// The scale the LAST frame actually drew at, so the toolbar can say so.
    ///
    /// The auto-fit takes `min(width, height)` ratio, and this layout only ever
    /// grows SIDEWAYS: one config module per column, no wrapping. Measured, a
    /// project with 32 of them is 3971 wide and 427 tall, so the whole diagram
    /// fits at roughly a fifth of full size and every label with it. An ESP part
    /// reaches that easily - uart0..2, spi2..3, i2c0..1, eight LEDC channels,
    /// RMT, PCNT, MCPWM, PARL, LCD, CAM.
    ///
    /// Without this the user sees an unreadable diagram and no reason for it.
    /// One frame late on purpose: the scale is not known until the canvas has
    /// been measured, which happens after the toolbar is drawn.
    pub last_scale: f32,
    /// Draw the cross-module call edges (Phase 3) over the diagram.
    pub show_calls: bool,
    /// Nodes drawn OPEN: every symbol row listed, in the order the module's own
    /// code calls them, with its internal call edges drawn between the rows.
    ///
    /// Not persisted - it is a reading position, like zoom and pan. Part of the
    /// layout cache key in the tab driver, because an open node is taller.
    pub expanded: std::collections::BTreeSet<usize>,
    /// How many call-hops BELOW the focused module to draw: `Some(1)` = only
    /// its direct edges (default), `Some(n)` = the downstream tree n levels
    /// deep, `None` = "All" (the whole tree under the selected module).
    pub call_depth: Option<usize>,
    /// Which route SHAPES the call-edge router may pick from.
    pub path_style: PathStyle,
    /// Show GHOST nodes for external crates (std / HAL / …) with dependency
    /// edges from the modules that use them.
    pub show_externals: bool,
    /// Live search query — matching nodes get a gold border, matching symbol
    /// rows a gold tint. Session-only.
    pub search: String,
    /// View offset from the centered position, in screen px — dragging the
    /// empty background pans the diagram (useful past 1.0 zoom, where the
    /// diagram overflows the fixed-size canvas). Clamped so the diagram can
    /// never be dragged out of sight; Ctrl+0 re-centers. Session-only.
    pub pan: egui::Vec2,
    /// Whether the node being dragged has actually moved in this drag, which
    /// is what pins it where it ends. How egui classifies the release cannot
    /// say: a slipped right or middle click reports no `clicked()`, and Escape
    /// or a still hold ends a drag that moved nothing.
    pub drag_moved: bool,
}

/// Call-edge route shapes offered to the router.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum PathStyle {
    /// Orthogonal rounded "circuit traces" only (lanes + right-angle L).
    Straight,
    /// Bezier curves only (facing / side / flank arcs).
    Curved,
    /// Everything competes on cost (beziers pay a small stylistic premium).
    #[default]
    Mixed,
}

impl PathStyle {
    fn label(self) -> &'static str {
        match self {
            Self::Straight => "Straight",
            Self::Curved => "Curved",
            Self::Mixed => "Mixed",
        }
    }

    /// Stable numeric form for `mcu.config` persistence.
    pub fn to_u8(self) -> u8 {
        match self {
            Self::Straight => 0,
            Self::Curved => 1,
            Self::Mixed => 2,
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Self::Straight,
            1 => Self::Curved,
            _ => Self::Mixed,
        }
    }
}

impl Default for StructureView {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            last_scale: 1.0,
            show_calls: true,
            call_depth: Some(1),
            path_style: PathStyle::Mixed,
            show_externals: false,
            search: String::new(),
            pan: egui::Vec2::ZERO,
            expanded: std::collections::BTreeSet::new(),
            drag_moved: false,
        }
    }
}

/// Below this the node labels stop being readable, so the toolbar says what
/// scale the diagram is at rather than leaving it looking broken. Measured
/// against the 11px node text: a fifth of full size is where names turn to
/// smudges.
const LEGIBLE_SCALE: f32 = 0.45;

/// Padding kept around the diagram when it auto-fits the panel.
const FIT_PAD: f32 = 20.0;

/// A call-edge route candidate: a cubic bezier, or an (already rounded)
/// orthogonal polyline. Kept alongside the coarse cost polyline so the DRAW
/// pass can render beziers natively (smooth at any zoom — the coarse flatten
/// is for cost/hover only).
enum Route {
    Bez([egui::Pos2; 4]),
    Poly(Vec<egui::Pos2>),
}

/// A click on a node: which file to open (`None` = main.rs) and, when a
/// symbol row was clicked, the 1-based line to jump to.
pub struct NodeClick {
    pub file: Option<usize>,
    pub line: Option<usize>,
    /// The node's project-root-relative path — the key the driver needs to look
    /// the file's diagnostics up when no `line` was given.
    pub file_rel: String,
}

/// Everything one frame of the diagram can report back to the driver.
#[derive(Default)]
pub struct ShowResult {
    /// A node / symbol-row click (open the file, optionally jump to a line).
    pub click: Option<NodeClick>,
    /// A header drag ended on this node — persist its position override.
    pub moved: Option<usize>,
    /// The "Auto layout" button was clicked — clear overrides and re-lay-out.
    pub reset_layout: bool,
}

const ROOT_FILL: egui::Color32 = egui::Color32::from_rgb(60, 58, 44);
/// Per-PACKAGE fills: every node of one top-level module subtree (same first
/// path segment — `pins::configs::usart1` → "pins") shares a colour, and each
/// package gets a different one, for visual separation. Muted dark hues so the
/// white node text stays readable on the dark canvas; assigned in
/// first-encounter node order, cycling when there are more packages.
const PKG_PALETTE: [egui::Color32; 8] = [
    egui::Color32::from_rgb(46, 52, 66), // slate blue
    egui::Color32::from_rgb(38, 60, 56), // teal
    egui::Color32::from_rgb(58, 46, 68), // violet
    egui::Color32::from_rgb(64, 54, 40), // amber brown
    egui::Color32::from_rgb(42, 60, 46), // green
    egui::Color32::from_rgb(64, 46, 52), // maroon
    egui::Color32::from_rgb(38, 56, 68), // steel cyan
    egui::Color32::from_rgb(56, 50, 58), // plum grey
];
const NODE_STROKE: egui::Color32 = egui::Color32::from_rgb(96, 106, 128);
/// Blinking border of nodes whose file carries error diagnostics.
const ERROR_STROKE: egui::Color32 = egui::Color32::from_rgb(230, 70, 60);
/// Border / row tint of search matches (the definition-highlight gold).
const SEARCH_STROKE: egui::Color32 = egui::Color32::from_rgb(255, 214, 90);
// const HOVER_STROKE: egui::Color32 = egui::Color32::from_rgb(120, 170, 240);
const HOVER_STROKE: egui::Color32 = egui::Color32::from_rgb(250, 250, 250);
/// Module dependency edges (solid, straight): LIGHT GRAY — the old blue now
/// belongs to fn-call edges (see below), so the module-level wiring recedes
/// into the background while stays brighter than the dashed containment.
const DEP_COLOR: egui::Color32 = egui::Color32::from_rgb(40, 40, 40);
const CONTAIN_COLOR: egui::Color32 = egui::Color32::from_rgb(105, 105, 115);
/// A module's INTERNAL flow, drawn inside an expanded node. Dimmer than the
/// cross-module edges on purpose: at this zoom the reader is inside one box,
/// and these must not compete with the arrows that connect boxes to each other.
const FLOW_COLOR: egui::Color32 = egui::Color32::from_rgb(150, 165, 195);

/// Render the diagram; the [`ShowResult`] carries clicks, a finished node drag
/// and the Auto-layout request. `lay` is mutable: dragging a node's HEADER
/// moves it live (edges/ports/detours recompute from `lay.pos` every frame).
/// `calls` are the Phase-3 cross-module call edges (drawn when `view.show_calls`);
/// `calls_status` is a short toolbar note ("analyzing 12/47…" / save hint / "").
pub fn show(
    ui: &mut egui::Ui,
    graph: &ModuleGraph,
    lay: &mut GraphLayout,
    view: &mut StructureView,
    calls: &[CallEdge],
    // Calls WITHIN a module - see `show_canvas`.
    inner_calls: &[CallEdge],
    calls_status: &str,
    // Focused module (index): only call edges touching it are drawn — showing
    // every collected edge at once was unreadable. Driven by the currently
    // selected FILE (main.rs by default), so clicking a node (which opens its
    // file) also moves the focus.
    focus_node: usize,
    // Per-node error flag (diagnostics in that module's file): the node's
    // border blinks red at 3× width so broken files stand out.
    node_errors: &[bool],
    // Total reference sites per symbol `(node, row)` — drawn right-aligned in
    // the row (single edges aggregate many sites; the count shows the total).
    ref_counts: &std::collections::HashMap<(usize, usize), usize>,
    // Reference sites per drawn edge — scales the stroke width (heavier
    // relationships read thicker).
    pair_counts: &std::collections::HashMap<CallEdge, usize>,
) -> ShowResult {
    let mut result = ShowResult::default();
    // ── Toolbar row 1: info text only ─────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{} modules · {} dependencies · {} mod declarations",
                graph.nodes.len(),
                graph.deps.len(),
                graph.contains.len()
            ))
            .size(11.0)
            .color(egui::Color32::from_rgb(150, 150, 160)),
        );
        // Whose calls are shown (follows the selected file; main by default).
        if view.show_calls {
            if let Some(node) = graph.nodes.get(focus_node) {
                // Always this one module: the focus never silently widens to
                // a package, so the name here is the whole truth.
                let label = format!("· calls of {}", node.name);
                ui.label(
                    egui::RichText::new(label)
                        .size(10.5)
                        .color(egui::Color32::from_rgb(150, 158, 172)),
                )
                .on_hover_text(
                    "The call edges shown are the ones reaching the selected \
                     file's module, either way round - what it calls and \
                     what calls it, out to the Depth beside this. Always \
                     that ONE module: selecting a mod.rs no longer widens \
                     the focus to its whole package. Select another file \
                     (tree / editor / node click) to move the focus.",
                );
            }
        }
        if !calls_status.is_empty() {
            ui.label(
                egui::RichText::new(calls_status)
                    .size(10.5)
                    .color(egui::Color32::from_rgb(200, 160, 70)),
            );
        }
        // Say the scale when the fit has shrunk the labels past reading. The
        // diagram only grows sideways, so a project with many `pins/configs`
        // modules - which is every well-populated ESP one - fits at a fifth of
        // full size, and an unreadable diagram with no explanation reads as a
        // broken one. One frame behind; see `StructureView::last_scale`.
        if view.last_scale < LEGIBLE_SCALE {
            ui.label(
                egui::RichText::new(format!(
                    "{:.0}% - fit to panel; scroll to zoom",
                    view.last_scale * 100.0
                ))
                .size(10.5)
                .color(egui::Color32::from_rgb(150, 150, 165)),
            )
            .on_hover_text(
                "The whole diagram is being shown at once. It grows sideways with the number of modules, so a project with many peripheral configs fits only by shrinking.",
            );
        }
    });

    // ── Toolbar row 2: the commands, groups separated by `|` ──────────────
    ui.horizontal(|ui| {
        if ui
            .small_button("Auto layout")
            .on_hover_text(
                "Discard the manually dragged positions and re-run the \
                 automatic arrangement",
            )
            .clicked()
        {
            result.reset_layout = true;
            view.pan = egui::Vec2::ZERO;
        }
        ui.separator();
        ui.checkbox(
            &mut view.show_calls,
            egui::RichText::new("Calls").size(11.0),
        )
        .on_hover_text(
            "Show cross-module call edges, coloured by the TARGET's kind \
                 (like the row glyphs): blue = fn, orange = struct, purple = \
                 enum, green = trait. Computed via rust-analyzer, one symbol \
                 at a time, only while the project is saved/in sync.",
        );
        if view.show_calls {
            egui::ComboBox::from_id_salt("structure_call_depth")
                .width(52.0)
                .selected_text(match view.call_depth {
                    Some(n) => n.to_string(),
                    None => "All".to_owned(),
                })
                .show_ui(ui, |ui| {
                    for n in 1..=10usize {
                        ui.selectable_value(&mut view.call_depth, Some(n), n.to_string());
                    }
                    ui.selectable_value(&mut view.call_depth, None, "All");
                })
                .response
                .on_hover_text(
                    "How far from the focused module to follow the call tree, in \
                     BOTH directions: 1 = its direct edges only (who it calls, who \
                     calls it); N = also callees of callees and callers of callers, \
                     N levels out; All = everything connected to it. The caller side \
                     used to stop at one hop whatever this said, which left the \
                     setting doing nothing on a module that mostly gets called.",
                );
            ui.separator();
            ui.checkbox(
                &mut view.show_externals,
                egui::RichText::new("Externals").size(11.0),
            )
            .on_hover_text(
                "Show ghost nodes for external crates (std / core / the HAL…) \
                 with dependency edges from the modules that use them.",
            );
            ui.separator();
            egui::ComboBox::from_id_salt("structure_path_style")
                .width(76.0)
                .selected_text(view.path_style.label())
                .show_ui(ui, |ui| {
                    for style in [PathStyle::Straight, PathStyle::Curved, PathStyle::Mixed] {
                        ui.selectable_value(&mut view.path_style, style, style.label());
                    }
                })
                .response
                .on_hover_text(
                    "Call-edge shape: Straight = orthogonal rounded traces \
                     only; Curved = bezier arcs only; Mixed = the router picks \
                     the cheapest of both per edge.",
                );
        }
        // Search: matching nodes get a gold border, matching rows a gold tint.
        ui.separator();
        ui.label(egui::RichText::new(egui_phosphor::regular::MAGNIFYING_GLASS).size(11.0));
        ui.add(
            egui::TextEdit::singleline(&mut view.search)
                .desired_width(110.0)
                .hint_text("Search…"),
        )
        .on_hover_text("Highlight modules and symbols whose name contains this text");
        if !view.search.is_empty()
            && ui
                .add(
                    egui::Button::new(egui::RichText::new(egui_phosphor::regular::X).size(10.0))
                        .frame(false),
                )
                .clicked()
        {
            view.search.clear();
        }
    });

    // ── Toolbar row 3: usage hints, last line under the buttons ───────────
    ui.label(
        egui::RichText::new(
            "Ctrl+± / mouse wheel zoom, Ctrl+0 reset · drag the background = \
             pan · drag a module's header = move it · click = open",
        )
        .size(10.5)
        .color(egui::Color32::from_rgb(120, 120, 130)),
    );
    ui.add_space(2.0);

    // ── Canvas ────────────────────────────────────────────────────────────
    let avail = ui.available_size();
    if avail.x < 3.0 * FIT_PAD || avail.y < 3.0 * FIT_PAD || lay.pos.is_empty() {
        return result;
    }

    // Ctrl+± / Ctrl+0 zoom, consumed ONLY while the pointer hovers the diagram
    // area (the editor keeps its own Ctrl+± when hovered — hover routes them).
    if ui.rect_contains_pointer(ui.available_rect_before_wrap()) {
        ui.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            if i.consume_key(cmd, egui::Key::Num0) {
                view.zoom = 1.0;
                view.pan = egui::Vec2::ZERO; // re-center too
            } else if i.consume_key(cmd, egui::Key::Plus) || i.consume_key(cmd, egui::Key::Equals) {
                view.zoom = (view.zoom * 1.15).min(4.0);
            } else if i.consume_key(cmd, egui::Key::Minus) {
                view.zoom = (view.zoom / 1.15).max(0.3);
            }
            // Mouse wheel = smooth zoom (consumed so no outer area scrolls) —
            // multiplicative, so notches compose like the Ctrl+± steps.
            let scroll = i.smooth_scroll_delta.y;
            if scroll != 0.0 {
                i.smooth_scroll_delta = egui::Vec2::ZERO;
                view.zoom = (view.zoom * (scroll * 0.002).exp()).clamp(0.3, 4.0);
            }
        });
    }

    // AUTO-ZOOM base: the WHOLE diagram fits the panel with FIT_PAD padding,
    // recomputed every frame — panel/window resizes always rescale. The user
    // multiplier stacks on top; past 1.0 the diagram overflows the fixed
    // canvas and background-drag panning takes over (no scrollbars).
    let base = ((avail.x - 2.0 * FIT_PAD) / lay.width)
        .min((avail.y - 2.0 * FIT_PAD) / lay.height)
        .clamp(0.05, 2.5);
    let scale = (base * view.zoom).clamp(0.05, 5.0);
    view.last_scale = scale;
    let content = egui::vec2(lay.width, lay.height) * scale;
    show_canvas(
        ui,
        graph,
        lay,
        view,
        calls,
        inner_calls,
        scale,
        content,
        avail,
        focus_node,
        node_errors,
        ref_counts,
        pair_counts,
        &mut result,
    );

    result
}

/// One panning axis: the content edge position (relative to the canvas edge)
/// clamped so the diagram never leaves the viewport — fully inside (with the
/// pad) while it fits, slideable edge-to-edge while it overflows.
///
/// The two bounds are ORDERED rather than picked by a `content + 2·FIT_PAD <=
/// avail` test, because at the auto-fit scale the two disagree: `content` is
/// `width * ((avail - 2·FIT_PAD) / width)`, and those two roundings can land it
/// a hair ABOVE `avail - 2·FIT_PAD` while `content + 2·FIT_PAD` still rounds
/// down to `<= avail` — the branch then fed `clamp` a max below its min and
/// panicked ("min > max ... min = 20.0, max = 19.99997"). `max`/`min` also
/// swallow a non-finite bound instead of panicking on it.
fn clamp_rel(rel: f32, avail: f32, content: f32) -> f32 {
    // Fits: pinned FIT_PAD from the near edge. Overflows: the far edge, i.e. a
    // negative offset. Whichever is smaller is the lower bound.
    let inside = FIT_PAD;
    let overflow = avail - content - FIT_PAD;
    let (lo, hi) = if inside <= overflow {
        (inside, overflow)
    } else {
        (overflow, inside)
    };
    rel.max(lo).min(hi)
}

/// The scaled diagram body — a FIXED canvas exactly the visible size (the
/// diagram scales/pans inside it; it never grows the panel).
#[allow(clippy::too_many_arguments)]
fn show_canvas(
    ui: &mut egui::Ui,
    graph: &ModuleGraph,
    lay: &mut GraphLayout,
    view: &mut StructureView,
    calls: &[CallEdge],
    // Calls WITHIN a module - drawn only inside an expanded node, and the
    // source of the order its rows are listed in.
    inner_calls: &[CallEdge],
    scale: f32,
    content: egui::Vec2,
    avail: egui::Vec2,
    focus_node: usize,
    node_errors: &[bool],
    ref_counts: &std::collections::HashMap<(usize, usize), usize>,
    pair_counts: &std::collections::HashMap<CallEdge, usize>,
    result: &mut ShowResult,
) {
    // The canvas is exactly the space that is visible. Dragging its empty
    // background pans the view (nodes/rows are registered AFTER this response,
    // so egui hands them the pointer first — the background only sees drags
    // that start in the gaps around them).
    let (rect, bg_resp) = ui.allocate_exact_size(avail, egui::Sense::click_and_drag());
    if bg_resp.dragged() {
        view.pan += bg_resp.drag_delta();
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    // Center the diagram, then apply the (clamped) pan on top.
    let free = (rect.size() - content) * 0.5;
    let rel = egui::vec2(
        clamp_rel(free.x + view.pan.x, rect.width(), content.x),
        clamp_rel(free.y + view.pan.y, rect.height(), content.y),
    );
    view.pan = rel - free;
    let origin = rect.left_top() + rel;
    let to_screen = |x: f32, y: f32| -> egui::Pos2 { origin + egui::vec2(x, y) * scale };

    let painter = ui.painter().with_clip_rect(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_rgb(24, 26, 32));

    let stroke_w = (1.3 * scale).clamp(0.6, 2.4);

    // Screen polylines of every edge already drawn — the call-edge router
    // below scores its candidate routes against them (crossing penalties).
    let mut edge_polys: Vec<Vec<egui::Pos2>> = Vec::new();

    // ── Module edges (containment dashed + dependency solid), routed ──────
    // CENTER anchoring (user fix): every dep/containment edge runs from node
    // CENTER to node CENTER — the convergence point hides BEHIND the node box
    // (edges draw before nodes), so nothing splays along the borders anymore;
    // only the arrowhead stays visible, clipped to the target's boundary.
    // OBSTACLE DETOUR kept: an edge whose straight segment would cut through
    // another node box becomes a lateral bezier around the blockers.
    {
        // (u, v, is_dep): deps drawn solid + arrowhead, containment dashed.
        let module_edges: Vec<(usize, usize, bool)> = graph
            .contains
            .iter()
            .map(|&(u, v)| (u, v, false))
            .chain(graph.deps.iter().map(|&(u, v)| (u, v, true)))
            .collect();

        let dash = 4.0 * scale.max(0.5);
        for &(u, v, is_dep) in &module_edges {
            let (a, b) = (lay.pos[u], lay.pos[v]);
            let from_v = (a.center_x(), a.y + a.h / 2.0);
            let to_v = (b.center_x(), b.y + b.h / 2.0);

            // Straight segment blocked by a non-endpoint node box? → detour x
            // just past the blockers, on whichever side deviates less.
            let mut blocked = false;
            let (mut left_cand, mut right_cand) = (f32::MAX, f32::MIN);
            for (k, q) in lay.pos.iter().enumerate() {
                if k == u || k == v {
                    continue;
                }
                if crate::panels::structure_map::layout::seg_hits_rect(
                    from_v, to_v, q.x, q.y, q.w, q.h,
                ) {
                    blocked = true;
                    left_cand = left_cand.min(q.x);
                    right_cand = right_cand.max(q.x + q.w);
                }
            }
            let stroke =
                egui::Stroke::new(stroke_w, if is_dep { DEP_COLOR } else { CONTAIN_COLOR });
            let from = to_screen(from_v.0, from_v.1);
            let to = to_screen(to_v.0, to_v.1);
            // Screen rect of the target — the arrowhead is drawn where the
            // edge crosses its boundary (the tail hides behind the boxes).
            let target_rect =
                egui::Rect::from_min_size(to_screen(b.x, b.y), egui::vec2(b.w, b.h) * scale);
            let pts: Vec<egui::Pos2> = if !blocked {
                if is_dep {
                    painter.line_segment([from, to], stroke);
                } else {
                    painter.add(egui::Shape::dashed_line(&[from, to], stroke, dash, dash));
                }
                vec![from, to]
            } else {
                const PAD: f32 = 14.0;
                let mid = (from_v.0 + to_v.0) / 2.0;
                let (l, r) = (left_cand - PAD, right_cand + PAD);
                let detour_x = if (mid - l).abs() <= (r - mid).abs() {
                    l
                } else {
                    r
                };
                let c1 = to_screen(detour_x, from_v.1 + 0.30 * (to_v.1 - from_v.1));
                let c2 = to_screen(detour_x, from_v.1 + 0.70 * (to_v.1 - from_v.1));
                let bez = egui::epaint::CubicBezierShape::from_points_stroke(
                    [from, c1, c2, to],
                    false,
                    egui::Color32::TRANSPARENT,
                    stroke,
                );
                let pts = bez.flatten(Some(2.0));
                if is_dep {
                    painter.add(bez);
                } else {
                    // Dashed beziers aren't a primitive — flatten to a polyline.
                    painter.add(egui::Shape::dashed_line(&pts, stroke, dash, dash));
                }
                pts
            };
            if is_dep {
                if let Some((prev, tip)) = boundary_tip(&pts, &target_rect) {
                    arrowhead(&painter, prev, tip, 7.0 * scale.clamp(0.5, 1.6), stroke);
                }
            }
            edge_polys.push(pts);
        }
    }

    // ── Nodes ─────────────────────────────────────────────────────────────
    let name_font = egui::FontId::proportional((11.5 * scale).clamp(6.0, 24.0));
    let sub_font = egui::FontId::proportional((8.5 * scale).clamp(5.0, 18.0));
    let row_font = egui::FontId::monospace((8.0 * scale).clamp(5.0, 16.0));
    // Symbol rows are unreadable below this scale — draw compact nodes instead.
    let show_detail = scale > 0.45;
    // Hover-focus state, fed to the call-edge draw pass below: while a node
    // or a symbol row is hovered, unrelated call edges dim.
    let mut hovered_node: Option<usize> = None;
    let mut hovered_row: Option<(usize, usize)> = None;
    // Search query (case-insensitive substring on module and symbol names).
    let query = view.search.trim().to_lowercase();
    // One fill per PACKAGE (top-level module subtree): same colour for a
    // package and all its children, different across packages. main keeps its
    // olive root fill.
    let fills: Vec<egui::Color32> = {
        let mut by_pkg: std::collections::HashMap<&str, egui::Color32> =
            std::collections::HashMap::new();
        let mut next = 0usize;
        graph
            .nodes
            .iter()
            .map(|n| {
                if n.path.is_empty() {
                    ROOT_FILL
                } else {
                    let key = n.path.split("::").next().unwrap_or("");
                    *by_pkg.entry(key).or_insert_with(|| {
                        let c = PKG_PALETTE[next % PKG_PALETTE.len()];
                        next += 1;
                        c
                    })
                }
            })
            .collect()
    };
    for (i, node) in graph.nodes.iter().enumerate() {
        let p = lay.pos[i];
        let r = egui::Rect::from_min_size(to_screen(p.x, p.y), egui::vec2(p.w, p.h) * scale);
        if !rect.intersects(r) {
            continue;
        }
        let resp = ui.interact(
            r.intersect(rect),
            ui.id().with(("structure_node", i)),
            egui::Sense::click(),
        );

        // ── Header drag: move the node ─────────────────────────────────────
        // The header band is the drag handle (the whole box when compact) —
        // registered after the node's click response so the pointer prefers
        // it there; symbol rows below keep their click-to-jump untouched.
        // Deltas are divided by `scale` (screen → virtual). Edges recompute
        // from `lay.pos` next frame, so they follow with a one-frame lag —
        // the usual immediate-mode drag behaviour.
        let pre_rows = if show_detail {
            shown_rows(node.symbols.len(), view.expanded.contains(&i))
        } else {
            0
        };
        let handle_h = if pre_rows > 0 {
            HEADER_H * scale
        } else {
            r.height()
        };
        let drag_resp = ui
            .interact(
                egui::Rect::from_min_size(r.min, egui::vec2(r.width(), handle_h)).intersect(rect),
                ui.id().with(("structure_drag", i)),
                egui::Sense::click_and_drag(),
            )
            .on_hover_cursor(egui::CursorIcon::Grab);
        // Double-click the header: open the node, or close it again.
        //
        // On the header rather than the box, so it cannot be confused with a
        // row's click-to-jump; a double-click, so it cannot be confused with the
        // single click that opens the file or with the drag that moves the node.
        // A node with no symbols has nothing to open.
        if drag_resp.double_clicked() && show_detail && !node.symbols.is_empty() {
            if !view.expanded.remove(&i) {
                view.expanded.insert(i);
            }
        }
        if drag_resp.drag_started() {
            view.drag_moved = false;
        }
        if drag_resp.dragged() && crate::panels::drag_decided(ui) {
            let d = drag_resp.drag_delta() / scale;
            let was = (lay.pos[i].x, lay.pos[i].y);
            lay.pos[i].x = (lay.pos[i].x + d.x).max(MARGIN);
            lay.pos[i].y = (lay.pos[i].y + d.y).max(MARGIN);
            view.drag_moved |= (lay.pos[i].x, lay.pos[i].y) != was;
        }
        // Only a drag that moved the node pins it. A click that slipped off
        // the header ends a drag too, one that moved nothing - with any
        // button, and also when Escape or a still hold ends it.
        if drag_resp.drag_stopped() && std::mem::take(&mut view.drag_moved) {
            recompute_bounds(lay);
            result.moved = Some(i);
        }
        // Re-read the (possibly just-moved) position for drawing.
        let p = lay.pos[i];
        let r = egui::Rect::from_min_size(to_screen(p.x, p.y), egui::vec2(p.w, p.h) * scale);

        // Package roots (a `mod.rs` file) stand out: pill corners, a border
        // twice as thick as regular nodes, and a bold name. External-crate
        // ghosts get a neutral fill.
        let is_pkg_root = node.file_rel.ends_with("/mod.rs");
        let fill = if node.is_external {
            egui::Color32::from_rgb(42, 44, 50)
        } else {
            fills[i]
        };
        // Border style, by priority:
        //   1. ERROR — the file has diagnostics: BLINKING red at 3× width
        //      (driven by wall time; repaint scheduled below);
        //   2. SELECTED (the focus node) — the hover style, held while the
        //      node stays selected, so it's obvious whose edges are shown;
        //   3. hover; 4. normal.
        if resp.hovered() || drag_resp.hovered() {
            hovered_node = Some(i);
        }
        let has_error = node_errors.get(i).copied().unwrap_or(false);
        let blink_on = has_error && (ui.ctx().input(|inp| inp.time) * 2.0) as i64 % 2 == 0;
        if has_error {
            // Keep frames coming so the blink actually blinks.
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(250));
        }
        let selected = i == focus_node;
        // Search: the node matches when its name/path or ANY of its symbols
        // contains the query — so matches show even zoomed out (rows hidden).
        let search_hit = !query.is_empty()
            && (node.name.to_lowercase().contains(&query)
                || node.path.to_lowercase().contains(&query)
                || node
                    .symbols
                    .iter()
                    .any(|s| s.name.to_lowercase().contains(&query)));
        let (stroke_c, base_w) = if blink_on {
            (ERROR_STROKE, 3.0_f32)
        } else if search_hit {
            (SEARCH_STROKE, 2.0_f32)
        } else if selected || resp.hovered() {
            (HOVER_STROKE, 1.6_f32)
        } else {
            (NODE_STROKE, 1.0_f32)
        };
        painter.rect(
            r,
            if is_pkg_root {
                20.0 * scale.clamp(0.5, 1.5)
            } else {
                4.0 * scale.clamp(0.5, 1.5)
            },
            fill,
            egui::Stroke::new(
                if is_pkg_root {
                    base_w * 2.0_f32
                } else {
                    base_w
                },
                stroke_c,
            ),
            egui::StrokeKind::Inside,
        );

        // ── Header band: name + fn/ty badge ───────────────────────────────
        // Rows below need the detail scale; a row-less (or zoomed-out) node
        // centers the header content in the whole box instead.
        // Open nodes list every row, in call order, with their internal flow
        // drawn between the rows. Zoomed out past the legibility floor nothing
        // is detailed, so expansion is moot there.
        let expanded_here = show_detail && view.expanded.contains(&i);
        let rows = if show_detail {
            shown_rows(node.symbols.len(), expanded_here)
        } else {
            0
        };
        let header = if rows > 0 {
            egui::Rect::from_min_size(r.min, egui::vec2(r.width(), HEADER_H * scale))
        } else {
            r
        };
        let name_y = if show_detail {
            header.center().y - header.height() * 0.18
        } else {
            header.center().y
        };
        let name_pos = egui::pos2(header.center().x, name_y);
        // A detached library's files in the amber the LIBRARIES panel gives
        // it, so the diagram says what the tree says: not part of the build.
        let name_col = if node.detached {
            crate::project_tree::gui::ICON_LIBRARY_DETACHED
        } else {
            egui::Color32::WHITE
        };
        painter.text(
            name_pos,
            egui::Align2::CENTER_CENTER,
            &node.name,
            name_font.clone(),
            name_col,
        );
        if is_pkg_root {
            // Poor-man's bold: egui ships no bold face, so overdraw the name
            // with a sub-pixel offset to thicken the glyphs.
            painter.text(
                name_pos + egui::vec2((0.6 * scale).clamp(0.3, 0.9), 0.0),
                egui::Align2::CENTER_CENTER,
                &node.name,
                name_font.clone(),
                name_col,
            );
        }
        if show_detail {
            painter.text(
                egui::pos2(
                    header.center().x,
                    header.center().y + header.height() * 0.24,
                ),
                egui::Align2::CENTER_CENTER,
                if node.is_external {
                    "extern crate".to_owned()
                } else {
                    format!("{} fn · {} ty", node.fn_count, node.ty_count)
                },
                sub_font.clone(),
                egui::Color32::from_rgb(150, 158, 172),
            );
        }

        // ── Symbol rows (top-level fn/struct/enum/trait; click → jump) ────
        let mut row_clicked = false;
        if rows > 0 {
            painter.line_segment(
                [
                    egui::pos2(r.left() + 4.0 * scale, header.bottom()),
                    egui::pos2(r.right() - 4.0 * scale, header.bottom()),
                ],
                egui::Stroke::new(0.8_f32, NODE_STROKE),
            );
            let row_h = ROW_H * scale;
            // Where each row is DRAWN, when the node is open: its own call
            // order rather than source order.
            //
            // Only the position moves. `j` stays the SYMBOL index everywhere
            // else - it addresses `node.symbols`, it is half of the click id,
            // and it is what a `CallEdge`'s `from_row`/`to_row` mean. Reordering
            // the identity instead of the placement would silently point every
            // call edge and every jump-to-line at the wrong item.
            let draw_order: Vec<usize> = if expanded_here {
                let mine: Vec<CallEdge> = inner_calls
                    .iter()
                    .copied()
                    .filter(|e| e.from_node == i)
                    .collect();
                crate::panels::structure_map::calls::call_order(node.symbols.len(), &mine)
            } else {
                (0..rows).collect()
            };
            // Slot of each symbol, for drawing the flow between rows.
            let mut slot_of = vec![usize::MAX; node.symbols.len()];
            for (slot, &j) in draw_order.iter().enumerate() {
                if j < slot_of.len() {
                    slot_of[j] = slot;
                }
            }
            if expanded_here {
                // The module's own flow, drawn in the left gutter INSIDE the
                // box: a short bow from the caller's row down to the callee's.
                //
                // Rows are already in call order, so nearly every one of these
                // runs downward and short - which is the whole point of the
                // ordering. A bow that runs UP is a back edge, and reads as one.
                let gutter = r.left() + 3.0 * scale;
                for e in inner_calls.iter().filter(|e| e.from_node == i) {
                    let (Some(&a), Some(&b)) = (slot_of.get(e.from_row), slot_of.get(e.to_row))
                    else {
                        continue;
                    };
                    if a == usize::MAX || b == usize::MAX {
                        continue;
                    }
                    let y = |slot: usize| header.bottom() + (slot as f32 + 0.5) * row_h;
                    let (ya, yb) = (y(a), y(b));
                    let bow = (6.0 * scale).min((yb - ya).abs() * 0.4);
                    painter.add(egui::Shape::CubicBezier(
                        egui::epaint::CubicBezierShape::from_points_stroke(
                            [
                                egui::pos2(gutter, ya),
                                egui::pos2(gutter - bow, ya),
                                egui::pos2(gutter - bow, yb),
                                egui::pos2(gutter, yb),
                            ],
                            false,
                            egui::Color32::TRANSPARENT,
                            egui::Stroke::new(1.0_f32.max(scale), FLOW_COLOR),
                        ),
                    ));
                }
            }
            for (slot, &j) in draw_order.iter().enumerate() {
                let row_rect = egui::Rect::from_min_size(
                    egui::pos2(r.left(), header.bottom() + slot as f32 * row_h),
                    egui::vec2(r.width(), row_h),
                );
                // Trailing "+K more" row when truncated (not clickable). Never
                // on an open node, which lists everything.
                if !expanded_here && node.symbols.len() > MAX_SYMBOL_ROWS && slot == rows - 1 {
                    painter.text(
                        egui::pos2(row_rect.left() + 6.0 * scale, row_rect.center().y),
                        egui::Align2::LEFT_CENTER,
                        format!("+{} more", node.symbols.len() - MAX_SYMBOL_ROWS),
                        row_font.clone(),
                        egui::Color32::from_rgb(130, 135, 150),
                    );
                    continue;
                }
                let sym = &node.symbols[j];
                let row_resp = ui.interact(
                    row_rect.intersect(rect),
                    ui.id().with(("structure_row", i, j)),
                    egui::Sense::click(),
                );
                if row_resp.hovered() {
                    hovered_row = Some((i, j));
                    painter.rect_filled(
                        row_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(120, 170, 240, 26),
                    );
                }
                // Search hit on this symbol → gold row tint.
                if !query.is_empty() && sym.name.to_lowercase().contains(&query) {
                    painter.rect_filled(
                        row_rect,
                        0.0,
                        egui::Color32::from_rgba_unmultiplied(255, 214, 90, 30),
                    );
                }
                let (glyph, g_color) = kind_glyph(sym.kind);
                painter.text(
                    egui::pos2(row_rect.left() + 6.0 * scale, row_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    glyph,
                    row_font.clone(),
                    g_color,
                );
                let name: String = if sym.name.chars().count() > ROW_NAME_CHARS {
                    let mut s: String = sym.name.chars().take(ROW_NAME_CHARS - 1).collect();
                    s.push('…');
                    s
                } else {
                    sym.name.clone()
                };
                painter.text(
                    egui::pos2(row_rect.left() + 16.0 * scale, row_rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    name,
                    row_font.clone(),
                    egui::Color32::from_rgb(200, 205, 215),
                );
                // Total reference sites (right-aligned, kind-coloured) — many
                // sites aggregate into one drawn edge, so the count keeps the
                // full picture (matches the editor's "N refs" pill).
                let refs = ref_counts.get(&(i, j)).copied();
                if let Some(cnt) = refs {
                    painter.text(
                        egui::pos2(row_rect.right() - 5.0 * scale, row_rect.center().y),
                        egui::Align2::RIGHT_CENTER,
                        cnt.to_string(),
                        row_font.clone(),
                        g_color,
                    );
                }
                row_resp.clone().on_hover_text(format!(
                    "{} {} — line {}{}\nClick to jump to it",
                    kind_word(sym.kind),
                    sym.name,
                    sym.line,
                    refs.map(|c| format!(" — {c} reference site(s)"))
                        .unwrap_or_default()
                ));
                if row_resp.clicked() {
                    result.click = Some(NodeClick {
                        file: node.file,
                        line: Some(sym.line),
                        file_rel: node.file_rel.clone(),
                    });
                    row_clicked = true;
                }
            }
        }

        let full = if node.path.is_empty() {
            "crate root"
        } else {
            &node.path
        };
        // What the graph alone cannot say (a registry version, a detached
        // copy), as a closing paragraph.
        let note = if node.note.is_empty() {
            String::new()
        } else {
            format!("\n\n{}", node.note)
        };
        if node.is_external {
            resp.clone().on_hover_text(format!(
                "external crate `{}`\nUsed by the modules pointing at it \
                 (no project file to open).{note}",
                node.name
            ));
        } else {
            resp.clone().on_hover_text(format!(
                "{full}\n{}\n{} fn · {} struct/enum/trait\nClick to open in the editor{note}",
                node.file_rel, node.fn_count, node.ty_count
            ));
        }
        // The header's drag-sense response wins the pointer there, so a plain
        // click on the header surfaces as `drag_resp.clicked()`. External
        // ghosts have no file to open — clicks are ignored.
        if (resp.clicked() || drag_resp.clicked()) && !row_clicked && !node.is_external {
            result.click = Some(NodeClick {
                file: node.file,
                line: None,
                file_rel: node.file_rel.clone(),
            });
        }
    }

    // ── Call edges (Phase 3): caller row → callee row, amber beziers ──────
    // Drawn OVER the nodes (they attach to row side-edges and travel between
    // nodes) with a translucent stroke so the text stays readable.
    if view.show_calls && !calls.is_empty() {
        let anchor = |ni: usize, row: usize, right_side: bool| -> egui::Pos2 {
            let p = lay.pos[ni];
            let visible = graph.nodes[ni].symbols.len().min(MAX_SYMBOL_ROWS);
            // Row center when rows are drawn; node center when zoomed out or
            // the row sits past the "+K more" cut.
            let y = if show_detail && row < visible {
                p.y + HEADER_H + (row as f32 + 0.5) * ROW_H
            } else {
                p.y + p.h / 2.0
            };
            let x = if right_side { p.x + p.w } else { p.x };
            to_screen(x, y)
        };
        // Which edges to draw: everything within `call_depth` hops of the
        // FOCUS SET, in EITHER direction. See `calls::visible_edges` for why
        // both — the caller side used to be pinned at one hop, which is what
        // made the depth control look like it only worked on `main`.
        let depth_limit = view.call_depth.unwrap_or(usize::MAX);
        // ── Candidate ROUTER (user fix: "choose the shortest path without
        //    crossing other paths") ─────────────────────────────────────────
        // For every visible edge THREE routes are scored — the direct
        // facing-sides bezier, and the left / right outer-flank arcs — by
        // length plus penalties for crossing anything already drawn (module
        // edges + previously routed calls) or cutting through a node box; the
        // cheapest wins. Short edges route first so they claim the direct
        // lanes and longer ones bend around them.
        let mut visible: Vec<&CallEdge> =
            super::calls::visible_edges(graph.nodes.len(), calls, focus_node, depth_limit);
        let center_dist2 = |e: &CallEdge| -> f32 {
            let (a, b) = (lay.pos[e.from_node], lay.pos[e.to_node]);
            let dx = a.center_x() - b.center_x();
            let dy = (a.y + a.h / 2.0) - (b.y + b.h / 2.0);
            dx * dx + dy * dy
        };
        visible.sort_by(|x, y| {
            center_dist2(x)
                .partial_cmp(&center_dist2(y))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let node_rects: Vec<egui::Rect> = lay
            .pos
            .iter()
            .map(|p| egui::Rect::from_min_size(to_screen(p.x, p.y), egui::vec2(p.w, p.h) * scale))
            .collect();
        use crate::panels::structure_map::layout::segments_cross;
        // Everything in `edge_polys` up to this point is MODULE wiring (gray
        // deps/containment) — background lines, cheap to cross. Entries pushed
        // later are routed CALL edges — crossing those costs real money.
        let module_polys_len = edge_polys.len();
        // Grace given to the ENDPOINT boxes only: an anchor sits ON its node's
        // boundary, so the first / last segment is allowed to touch that much
        // of it — and no more (see `path_cuts_box`).
        let anchor_slack = (2.0 * scale).clamp(1.0, 4.0);
        // Candidate cost: length in px, +60 per crossed module line (they're
        // background — a short direct route beats dodging them to the far
        // side, the reported bug), +400 per crossed CALL edge, +2600 per node
        // box it cuts through (endpoints included, beyond the anchor slack).
        // Returns `(cost, cuts_a_node)` — the flag feeds the HARD straight-
        // route rule in `pick_route` (a penalty alone still let a cutting
        // route win when every alternative was pricier).
        let poly_cost = |pts: &[egui::Pos2],
                         skip_a: usize,
                         skip_b: usize,
                         polys: &[Vec<egui::Pos2>]|
         -> (f32, bool) {
            let mut cost = 0.0;
            for w in pts.windows(2) {
                cost += w[0].distance(w[1]);
            }
            for (oi, other) in polys.iter().enumerate() {
                let penalty = if oi < module_polys_len { 60.0 } else { 400.0 };
                'pair: for w1 in pts.windows(2) {
                    for w2 in other.windows(2) {
                        if segments_cross(
                            (w1[0].x, w1[0].y),
                            (w1[1].x, w1[1].y),
                            (w2[0].x, w2[0].y),
                            (w2[1].x, w2[1].y),
                        ) {
                            cost += penalty;
                            break 'pair;
                        }
                    }
                }
            }
            let mut cuts = false;
            for (i, r) in node_rects.iter().enumerate() {
                // Near-prohibitive: at 900 a box cut could still beat 3
                // call-crossings (1200) on the near side and the route
                // sailed OVER the node face. Crossing a node never wins.
                if path_cuts_box(pts, *r, i == skip_a || i == skip_b, anchor_slack) {
                    cost += 2600.0;
                    cuts = true;
                }
            }
            (cost, cuts)
        };
        // Routed edges collected first (route + coarse polyline + kind
        // colour), then drawn in a second pass so hover-focus can dim the
        // unrelated ones. The polyline serves cost + hover; beziers draw
        // natively for smoothness.
        let mut routed: Vec<(&CallEdge, Route, Vec<egui::Pos2>, egui::Color32)> = Vec::new();
        for e in visible {
            let (a, b) = (lay.pos[e.from_node], lay.pos[e.to_node]);
            // Edge colour = the TARGET symbol's kind colour (the same palette
            // as the row glyphs): fn blue, Struct orange, Enum purple, Trait
            // green — the arrow tells at a glance WHAT is being used.
            let kind = graph.nodes[e.to_node]
                .symbols
                .get(e.to_row)
                .map(|s| s.kind)
                .unwrap_or(SymKind::Fn);
            let (_, kc) = kind_glyph(kind);
            let stroke = egui::Stroke::new(
                (1.1 * scale).clamp(0.5, 2.0),
                egui::Color32::from_rgba_unmultiplied(kc.r(), kc.g(), kc.b(), 200),
            );
            let toward_right = b.center_x() >= a.center_x();
            // Stagger keeps parallel calls between the same nodes off one arc.
            let swing = 26.0 + ((e.from_row + e.to_row) % 4) as f32 * 8.0;
            // SOURCE anchor (user fix): a call edge never latches onto the
            // CALLER's symbol-row zone (the f/S/E/T list) — it leaves through
            // the node's top/bottom edge beside the header's left/right
            // corner. Only the TARGET end still points at the used symbol's
            // row. Exit side follows the target's vertical direction; a small
            // per-row stagger keeps several departures off one corner point.
            let via_top = (b.y + b.h / 2.0) < (a.y + a.h / 2.0);
            let src_at = |right: bool, top: bool| -> egui::Pos2 {
                let inset = (10.0 + (e.from_row % 4) as f32 * 7.0).min(a.w * 0.45);
                let x = if right {
                    a.x + a.w - inset
                } else {
                    a.x + inset
                };
                let y = if top { a.y } else { a.y + a.h };
                to_screen(x, y)
            };
            let src = |right: bool| -> egui::Pos2 { src_at(right, via_top) };
            // Candidate 1: FACING sides — the short direct route (vertical
            // takeoff from the corner, horizontal landing at the row).
            let f_from = src(toward_right);
            let f_to = anchor(e.to_node, e.to_row, !toward_right);
            let dyf = ((f_to.y - f_from.y).abs() * 0.4).clamp(16.0 * scale, 100.0 * scale)
                * if via_top { -1.0 } else { 1.0 };
            let dxf = ((f_to.x - f_from.x).abs() * 0.4).clamp(20.0 * scale, 120.0 * scale)
                * if toward_right { 1.0 } else { -1.0 };
            let cand_facing = [
                f_from,
                f_from + egui::vec2(0.0, dyf),
                f_to - egui::vec2(dxf, 0.0),
                f_to,
            ];
            // Candidates 2 + 3: LEFT / RIGHT outer-flank arcs past the boxes.
            let mk_flank = |right: bool| {
                let from = src(right);
                let to = anchor(e.to_node, e.to_row, right);
                let outer_x = if right {
                    (a.x + a.w).max(b.x + b.w) + swing
                } else {
                    a.x.min(b.x) - swing
                };
                let cx = to_screen(outer_x, 0.0).x;
                [
                    from,
                    egui::pos2(cx, from.y + 0.25 * (to.y - from.y)),
                    egui::pos2(cx, from.y + 0.75 * (to.y - from.y)),
                    to,
                ]
            };
            // Candidate shapes: 3 beziers + an ORTHOGONAL 90°-bend route
            // (vertical drop from the corner, right-angle turn, horizontal run
            // into the row) — added only when the source corner sits clearly
            // beside the target's x-range, so the horizontal leg is real.
            // SIDE exit (user fix): the source may also leave through the
            // node's LEFT/RIGHT edge — but only at HEADER height, never beside
            // the symbol-row zone. Horizontal takeoff, straight into the
            // target row: the shortest shape when the nodes sit side by side.
            let src_side = |right: bool| -> egui::Pos2 {
                let y = a.y + (HEADER_H * 0.5).min(a.h * 0.5);
                let x = if right { a.x + a.w } else { a.x };
                to_screen(x, y)
            };
            let s_from = src_side(toward_right);
            let dxs = ((f_to.x - s_from.x).abs() * 0.4).clamp(20.0 * scale, 120.0 * scale)
                * if toward_right { 1.0 } else { -1.0 };
            let cand_side = [
                s_from,
                s_from + egui::vec2(dxs, 0.0),
                f_to - egui::vec2(dxs, 0.0),
                f_to,
            ];
            let corner_r = 12.0 * scale.clamp(0.5, 1.5);
            // ── UNDER / OVER lane routes (user fix) ──────────────────────────
            // Every shape above reaches the target row by running horizontally
            // AT the row's height — so when another node sits between caller
            // and callee at that height, all of them cut straight across its
            // face (the reported bug: the edge left `read_report` on the far
            // side, wrapped around and sailed over `data`'s rows). These leave
            // through the source's bottom/top instead, run a horizontal lane
            // clear of every box in the way, then come up in the gap BESIDE
            // the target and turn into its row.
            // The lane leaves through the source edge FACING the target: up
            // when the target is above, down otherwise. Offering the opposite
            // side too let a route exit the bottom to reach a node ABOVE it and
            // loop back up (the reported `send_models → data` bug) — and every
            // extra candidate clutters the diagram. One lane per edge, on the
            // sensible side.
            let lane_below = lane_exits_below(a.y + a.h / 2.0, b.y + b.h / 2.0);
            let hl_from = src_at(toward_right, !lane_below);
            // Entry side by GEOMETRY: the target side facing the EXIT CORNER,
            // not `!toward_right` (node centers). With overlapping x-ranges
            // the two disagree — send_models→data exited the top-LEFT corner
            // but was sent around to data's RIGHT side, wrapping the node it
            // could reach by going straight up (the reported bug).
            let entry_right = hl_from.x >= to_screen(b.x + b.w * 0.5, 0.0).x;
            let hl_to = anchor(e.to_node, e.to_row, entry_right);
            // The vertical leg sits just outside the target, on the caller's
            // side — i.e. in the gap between the target and its neighbour.
            let hl_turn = if entry_right {
                hl_to.x + 16.0 * scale
            } else {
                hl_to.x - 16.0 * scale
            };
            let lane_y_for = |below: bool, from_y: f32, from_x: f32| -> f32 {
                lane_y(
                    &node_rects,
                    e.to_node,
                    below,
                    from_y,
                    from_x,
                    hl_turn,
                    swing * scale,
                )
            };
            let mk_hlane = |below: bool| -> Vec<egui::Pos2> {
                let from = src_at(toward_right, !below);
                let lane_y = lane_y_for(below, from.y, from.x);
                rounded_path(
                    &[
                        from,
                        egui::pos2(from.x, lane_y),
                        egui::pos2(hl_turn, lane_y),
                        egui::pos2(hl_turn, hl_to.y),
                        hl_to,
                    ],
                    corner_r,
                )
            };
            // Bezier sibling for the Curved style: a cubic's belly only
            // reaches ~¾ of the way to its controls, so they overshoot the
            // lane — B(0.5) = (P0 + 3·P1 + 3·P2 + P3)/8, solved for the
            // control y that puts the midpoint exactly on the lane.
            let mk_arc = |below: bool| -> [egui::Pos2; 4] {
                let from = src_at(toward_right, !below);
                let lane_y = lane_y_for(below, from.y, from.x);
                let cy = (8.0 * lane_y - from.y - hl_to.y) / 6.0;
                [from, egui::pos2(from.x, cy), egui::pos2(hl_turn, cy), hl_to]
            };
            // Rounded-orthogonal LANE routes (user mockup): out the side at
            // header height, rounded bend, vertical run in the outer lane,
            // rounded bend, horizontal into the target row.
            let mk_lane = |right: bool| -> Vec<egui::Pos2> {
                let s = src_side(right);
                let lane_x_v = if right {
                    (a.x + a.w).max(b.x + b.w) + swing
                } else {
                    a.x.min(b.x) - swing
                };
                let lane_x = to_screen(lane_x_v, 0.0).x;
                let to_pt = anchor(e.to_node, e.to_row, right);
                rounded_path(
                    &[
                        s,
                        egui::pos2(lane_x, s.y),
                        egui::pos2(lane_x, to_pt.y),
                        to_pt,
                    ],
                    corner_r,
                )
            };
            // (route, stylistic cost multiplier). The Paths dropdown picks the
            // shape family: Straight = orthogonal traces only, Curved =
            // beziers only, Mixed = both (beziers pay a small premium so at
            // similar length the rounded-orthogonal shape wins).
            // (route, cost multiplier, entry_right): entering the target row
            // from the side FACING the caller is free; a far-side entry pays
            // +250, so wrap-arounds happen only when the near side is truly
            // blocked (the user's "connect on the left, not the right" fix).
            let mut cands: Vec<(Route, f32, bool)> = Vec::new();
            let style = view.path_style;
            if style != PathStyle::Straight {
                let bez_mult = if style == PathStyle::Mixed { 1.15 } else { 1.0 };
                cands.extend([
                    (Route::Bez(cand_facing), bez_mult, !toward_right),
                    (Route::Bez(cand_side), bez_mult, !toward_right),
                    (Route::Bez(mk_flank(false)), bez_mult, false),
                    (Route::Bez(mk_flank(true)), bez_mult, true),
                    // Over/under the boxes in the way, into the row's near side.
                    (Route::Bez(mk_arc(lane_below)), bez_mult, entry_right),
                ]);
            }
            if style != PathStyle::Curved {
                cands.extend([
                    (Route::Poly(mk_lane(false)), 1.0, false),
                    (Route::Poly(mk_lane(true)), 1.0, true),
                    (Route::Poly(mk_hlane(lane_below)), 1.0, entry_right),
                ]);
            }
            if style != PathStyle::Curved {
                let (bl, br) = (to_screen(b.x, 0.0).x, to_screen(b.x + b.w, 0.0).x);
                let src_top = to_screen(0.0, a.y).y;
                let src_bot = to_screen(0.0, a.y + a.h).y;
                // The L's vertical leg must run OUTSIDE the source box, so the
                // exit edge follows the TARGET ROW's y (not the node centers —
                // that variant sent the leg straight through the source, the
                // reported bug), and the route is offered only when the row
                // sits clearly above/below the source.
                if f_to.y < src_top - 4.0 || f_to.y > src_bot + 4.0 {
                    let l_from = src_at(toward_right, f_to.y < src_top);
                    if l_from.x < bl - 8.0 || l_from.x > br + 8.0 {
                        cands.push((
                            Route::Poly(rounded_path(
                                &[l_from, egui::pos2(l_from.x, f_to.y), f_to],
                                corner_r,
                            )),
                            1.0,
                            !toward_right,
                        ));
                    }
                }
            }
            let mut scored: Vec<(Route, Vec<egui::Pos2>, f32, bool)> = Vec::new();
            for (cand, mult, entry_right) in cands {
                let pts = match &cand {
                    Route::Bez(b4) => egui::epaint::CubicBezierShape::from_points_stroke(
                        *b4,
                        false,
                        egui::Color32::TRANSPARENT,
                        stroke,
                    )
                    .flatten(Some(4.0)),
                    Route::Poly(p) => p.clone(),
                };
                let (raw, cuts) = poly_cost(&pts, e.from_node, e.to_node, &edge_polys);
                let mut cost = raw * mult;
                // Far-side entry: the anchor sits on the side pointing AWAY
                // from the caller (entry_right == "target is to the right").
                if entry_right == toward_right {
                    cost += 250.0;
                }
                scored.push((cand, pts, cost, cuts));
            }
            let (cand, pts) = pick_route(scored);
            edge_polys.push(pts.clone());
            routed.push((e, cand, pts, kc));
        }

        // ── Hover detection + draw pass ───────────────────────────────────
        // Edges are painter shapes, not widgets, so hover is a distance test
        // against the routed polylines (suppressed while the pointer is over
        // a node — its own interactions win there).
        // Only while the canvas is the topmost thing under the pointer: an
        // edge found under the New Project chip list, or a window, would pop
        // its tooltip over it and take the click meant for it.
        let pointer = ui
            .ctx()
            .pointer_latest_pos()
            .filter(|_| ui.rect_contains_pointer(rect));
        let over_node = pointer
            .map(|p| node_rects.iter().any(|r| r.contains(p)))
            .unwrap_or(false);
        let hovered_edge: Option<usize> = pointer.filter(|_| !over_node).and_then(|p| {
            let mut best: Option<(usize, f32)> = None;
            for (idx, (_, _, pts, _)) in routed.iter().enumerate() {
                let d = dist_to_polyline(p, pts);
                if d < 6.0 && best.map_or(true, |(_, bd)| d < bd) {
                    best = Some((idx, d));
                }
            }
            best.map(|(i, _)| i)
        });
        let any_focus = hovered_row.is_some() || hovered_node.is_some() || hovered_edge.is_some();
        for (idx, (e, route, pts, kc)) in routed.iter().enumerate() {
            // HOVER-FOCUS: while something is hovered (a row, a node or an
            // edge), unrelated edges dim so the relevant wiring pops out.
            let emphasized = if let Some((hn, hr)) = hovered_row {
                (e.to_node == hn && e.to_row == hr) || (e.from_node == hn && e.from_row == hr)
            } else if let Some(hn) = hovered_node {
                e.from_node == hn || e.to_node == hn
            } else {
                hovered_edge == Some(idx)
            };
            let (alpha, width_mult) = if !any_focus {
                (200, 1.0)
            } else if emphasized {
                (255, if hovered_edge == Some(idx) { 1.8 } else { 1.4 })
            } else {
                (45, 1.0)
            };
            // Width ∝ ln(reference sites): 1 site = 1×, 4 ≈ 1.5×, 16 ≈ 2× —
            // heavy relationships read thicker, complementing the row counts.
            let thick = pair_counts
                .get(*e)
                .map(|&c| (1.0 + (c as f32).ln() * 0.35).min(2.4))
                .unwrap_or(1.0);
            let stroke = egui::Stroke::new(
                (1.1 * scale).clamp(0.5, 2.0) * width_mult * thick,
                egui::Color32::from_rgba_unmultiplied(kc.r(), kc.g(), kc.b(), alpha),
            );
            // Beziers render natively (smooth at any zoom); the coarse
            // polyline is only for cost/hover.
            match route {
                Route::Bez(b4) => {
                    painter.add(egui::epaint::CubicBezierShape::from_points_stroke(
                        *b4,
                        false,
                        egui::Color32::TRANSPARENT,
                        stroke,
                    ));
                }
                Route::Poly(p) => {
                    painter.add(egui::Shape::line(p.clone(), stroke));
                }
            }
            // Arrowhead along the final segment, whatever the shape.
            if pts.len() >= 2 {
                arrowhead(
                    &painter,
                    pts[pts.len() - 2],
                    pts[pts.len() - 1],
                    6.0 * scale.clamp(0.5, 1.5),
                    stroke,
                );
            }

            // Interactive edge: tooltip with the relation + click to jump to
            // the used symbol's definition.
            if hovered_edge == Some(idx) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                let from_n = &graph.nodes[e.from_node];
                let to_n = &graph.nodes[e.to_node];
                let from_sym = from_n
                    .symbols
                    .get(e.from_row)
                    .map(|s| s.name.as_str())
                    .unwrap_or("?");
                let to_sym = to_n.symbols.get(e.to_row);
                let sites = ref_counts
                    .get(&(e.to_node, e.to_row))
                    .map(|c| format!("\n{c} reference site(s) in total"))
                    .unwrap_or_default();
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    egui::Id::new(("structure_edge_tip", idx)),
                    egui::PopupAnchor::Pointer,
                )
                .gap(12.0)
                .show(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}::{} {} {}::{}{}",
                            from_n.name,
                            from_sym,
                            egui_phosphor::regular::ARROW_RIGHT,
                            to_n.name,
                            to_sym.map(|s| s.name.as_str()).unwrap_or("?"),
                            sites
                        ))
                        .size(11.0),
                    );
                    ui.label(
                        egui::RichText::new("Click to open the used symbol")
                            .size(9.5)
                            .color(egui::Color32::from_rgb(130, 140, 155)),
                    );
                });
                if ui.input(|i| i.pointer.primary_clicked()) {
                    result.click = Some(NodeClick {
                        file: to_n.file,
                        line: to_sym.map(|s| s.line),
                        file_rel: to_n.file_rel.clone(),
                    });
                }
            }
        }
    }
}

/// Shortest distance from `p` to the polyline `pts` (segment-wise).
fn dist_to_polyline(p: egui::Pos2, pts: &[egui::Pos2]) -> f32 {
    let mut best = f32::MAX;
    for w in pts.windows(2) {
        let ab = w[1] - w[0];
        let len2 = ab.length_sq();
        let t = if len2 <= 0.0 {
            0.0
        } else {
            ((p - w[0]).dot(ab) / len2).clamp(0.0, 1.0)
        };
        best = best.min((w[0] + ab * t).distance(p));
    }
    best
}

/// Glyph letter + colour for a symbol kind (drawn before the name in a row).
fn kind_glyph(kind: SymKind) -> (&'static str, egui::Color32) {
    match kind {
        SymKind::Fn => ("f", egui::Color32::from_rgb(130, 170, 240)),
        SymKind::Struct => ("S", egui::Color32::from_rgb(230, 160, 80)),
        SymKind::Enum => ("E", egui::Color32::from_rgb(190, 130, 230)),
        SymKind::Trait => ("T", egui::Color32::from_rgb(120, 200, 140)),
    }
}

fn kind_word(kind: SymKind) -> &'static str {
    match kind {
        SymKind::Fn => "fn",
        SymKind::Struct => "struct",
        SymKind::Enum => "enum",
        SymKind::Trait => "trait",
    }
}

/// Round every interior corner of an orthogonal polyline: each bend is
/// replaced by a small quadratic arc of `radius` (clamped to half of the
/// adjacent segment lengths), so 90° routes read as smooth "circuit traces"
/// instead of sharp elbows.
/// Which source edge the over/under lane leaves through: `true` (bottom) when
/// the target's center is BELOW the source's, `false` (top) when it is above.
/// Always the side facing the target — so the route never exits away from it
/// and loops back (the `send_models → data` bug: source below, target above,
/// yet the lane left the bottom).
fn lane_exits_below(src_center_y: f32, tgt_center_y: f32) -> bool {
    tgt_center_y >= src_center_y
}

/// The winner among scored route candidates `(route, polyline, cost, cuts)`.
///
/// HARD RULE (user request): a STRAIGHT (orthogonal `Poly`) route may never
/// cross a node — cutting Polys are dropped outright, so the shortest
/// rule-abiding path wins even where a crossing shortcut would out-price the
/// detour. CURVED routes stay soft: their cuts are already priced (+2600
/// each), which keeps them out of nodes wherever a way around exists, but a
/// dense diagram may still bend one across ("cele curbate sunt ok").
///
/// Fallback: when EVERY candidate is a cutting Poly (Straight style, no way
/// through), the cheapest still draws — a missing edge would be worse.
fn pick_route(scored: Vec<(Route, Vec<egui::Pos2>, f32, bool)>) -> (Route, Vec<egui::Pos2>) {
    let mut best: Option<(Route, Vec<egui::Pos2>, f32)> = None;
    let mut fallback: Option<(Route, Vec<egui::Pos2>, f32)> = None;
    for (route, pts, cost, cuts) in scored {
        let banned = cuts && matches!(route, Route::Poly(_));
        let slot = if banned { &mut fallback } else { &mut best };
        if slot.as_ref().is_none_or(|(_, _, c)| cost < *c) {
            *slot = Some((route, pts, cost));
        }
    }
    let (route, pts, _) = best.or(fallback).expect("router always offers candidates");
    (route, pts)
}

/// True when `pts` cuts through `r` — the router prices every hit as a
/// near-prohibitive crossing.
///
/// `endpoint` marks the route's OWN source / target box, whose anchor sits on
/// the boundary: those get `slack` px of grace (the box is tested shrunk), so
/// a segment that merely lands on the border is free while one that TRAVERSES
/// the face to reach a far-side anchor is not. Skipping the first / last
/// segment outright — the previous rule — made exactly that traversal free,
/// which is how a call edge ended up drawn straight across `data`'s rows.
fn path_cuts_box(pts: &[egui::Pos2], r: egui::Rect, endpoint: bool, slack: f32) -> bool {
    use super::layout::seg_hits_rect;
    let r = if endpoint { r.shrink(slack) } else { r };
    if r.width() <= 0.0 || r.height() <= 0.0 {
        return false; // node smaller than the slack (zoomed far out)
    }
    pts.windows(2).any(|w| {
        seg_hits_rect(
            (w[0].x, w[0].y),
            (w[1].x, w[1].y),
            r.left(),
            r.top(),
            r.width(),
            r.height(),
        )
    })
}

/// Y of a horizontal lane that clears every box whose x-range overlaps the
/// run `[x0, x1]` — `below` picks the lowest bottom (plus `clearance`), else
/// the highest top (minus it). `from_y` seeds it with the source's exit edge.
///
/// Boxes outside the run are ignored: the lane only has to dodge what it
/// would actually cross, so it stays as close to the nodes as it can.
/// `skip` is the CALLEE, which is never an obstacle: the route reaches it
/// through the vertical leg beside it, so counting its box would hoist the
/// whole lane over the very node it is heading for (whenever the callee sits
/// on the side the run comes from — `main → i2c1` in the reported diagram).
fn lane_y(
    rects: &[egui::Rect],
    skip: usize,
    below: bool,
    from_y: f32,
    x0: f32,
    x1: f32,
    clearance: f32,
) -> f32 {
    let (lo, hi) = (x0.min(x1), x0.max(x1));
    let mut y = from_y;
    for (_, r) in rects
        .iter()
        .enumerate()
        .filter(|(i, r)| *i != skip && r.right() >= lo && r.left() <= hi)
    {
        y = if below {
            y.max(r.bottom())
        } else {
            y.min(r.top())
        };
    }
    y + if below { clearance } else { -clearance }
}

/// A polyline with its corners filleted.
///
/// `pub(crate)` rather than moved: the Pins canvas needs exactly this and there
/// must be ONE of it, or two panels' corners drift apart over a release or two
/// and nobody can say which is right.
pub(crate) fn rounded_path(pts: &[egui::Pos2], radius: f32) -> Vec<egui::Pos2> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    for i in 1..pts.len() - 1 {
        let p = pts[i];
        let vin = p - pts[i - 1];
        let vout = pts[i + 1] - p;
        let (lin, lout) = (vin.length(), vout.length());
        if lin < 1.0 || lout < 1.0 {
            out.push(p);
            continue;
        }
        let r = radius.min(lin * 0.5).min(lout * 0.5);
        let a = p - vin / lin * r;
        let b = p + vout / lout * r;
        out.push(a);
        // 5 samples per corner arc — smooth even at thick strokes.
        for k in 1..6 {
            let t = k as f32 / 6.0;
            let ap = a.lerp(p, t);
            let pb = p.lerp(b, t);
            out.push(ap.lerp(pb, t));
        }
        out.push(b);
    }
    out.push(*pts.last().unwrap());
    out
}

/// Where a polyline (running INTO `target`'s interior) crosses the target's
/// boundary: returns `(point just outside, boundary tip)` for the arrowhead —
/// walked from the END of `pts` (which sits at the node's hidden center) back
/// to the first point outside, then bisected onto the border. `None` when the
/// whole polyline is inside (overlapping nodes) — no arrow then.
fn boundary_tip(pts: &[egui::Pos2], target: &egui::Rect) -> Option<(egui::Pos2, egui::Pos2)> {
    for i in (0..pts.len().saturating_sub(1)).rev() {
        if !target.contains(pts[i]) {
            let (mut lo, mut hi) = (pts[i], pts[i + 1]);
            for _ in 0..8 {
                let mid = lo + (hi - lo) * 0.5;
                if target.contains(mid) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            return Some((pts[i], lo));
        }
    }
    None
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
    use super::{FIT_PAD, clamp_rel, lane_exits_below, lane_y, path_cuts_box, rounded_path};
    use crate::panels::structure_map::layout::seg_hits_rect;
    use eframe::egui::{Pos2, Rect, pos2, vec2};

    /// True when any segment of `pts` touches `r` — the predicate the router's
    /// cost function uses to price a box cut.
    fn hits(r: &Rect, pts: &[Pos2]) -> bool {
        pts.windows(2).any(|w| {
            seg_hits_rect(
                (w[0].x, w[0].y),
                (w[1].x, w[1].y),
                r.left(),
                r.top(),
                r.width(),
                r.height(),
            )
        })
    }

    /// The reported layout: caller `read_report` on the right, callee on the
    /// left, and `data` sitting between them AT the target row's height — so
    /// every route that reaches the row horizontally cuts across `data`'s
    /// face. The under-lane route must leave through the caller's bottom, pass
    /// below `data` and come up in the gap beside the callee.
    #[test]
    fn under_lane_route_clears_a_node_between_caller_and_callee() {
        let source = Rect::from_min_size(pos2(1150.0, 100.0), vec2(600.0, 340.0));
        let data = Rect::from_min_size(pos2(370.0, 100.0), vec2(590.0, 420.0));
        let callee = Rect::from_min_size(pos2(0.0, 95.0), vec2(195.0, 560.0));
        let rects = [source, data, callee];
        let row_y = 450.0; // the callee's target row
        let to = pos2(callee.right(), row_y); // near-side entry (caller is right)
        let from = pos2(source.left() + 40.0, source.bottom()); // bottom corner
        let turn_x = to.x + 16.0; // vertical leg in the callee/data gap

        let lane = lane_y(&rects, 2, true, from.y, from.x, turn_x, 30.0);
        // The lane clears the obstacle, not just the caller.
        assert!(lane > data.bottom(), "lane {lane} must clear data");
        let path = rounded_path(
            &[
                from,
                pos2(from.x, lane),
                pos2(turn_x, lane),
                pos2(turn_x, to.y),
                to,
            ],
            12.0,
        );
        // The point of the fix: nothing of the route touches `data`.
        assert!(!hits(&data, &path));
        // …while the row-height approach every older shape used does.
        assert!(hits(&data, &[pos2(source.left(), row_y), to]));
        // The vertical leg stays outside the callee too: only the final
        // approach reaches its border (the anchor itself).
        assert!(!hits(&callee, &path[..path.len() - 1]));
        // The lane must not dive below the callee — it is not in the run.
        assert!(lane < callee.bottom());
    }

    /// `lane_y` only dodges boxes the run would actually cross, and mirrors
    /// for the over-lane.
    #[test]
    fn lane_only_dodges_boxes_inside_the_run() {
        let near = Rect::from_min_size(pos2(100.0, 0.0), vec2(100.0, 200.0));
        let far = Rect::from_min_size(pos2(900.0, 0.0), vec2(100.0, 900.0)); // outside
        let rects = [near, far];
        let none = usize::MAX; // no callee to exclude
        // Run x∈[50, 300] overlaps `near` only → lane clears it by `clearance`.
        assert_eq!(lane_y(&rects, none, true, 10.0, 50.0, 300.0, 30.0), 230.0);
        // The over-lane mirrors (topmost - clearance).
        assert_eq!(lane_y(&rects, none, false, 190.0, 50.0, 300.0, 30.0), -30.0);
        // A run clear of everything just takes the exit edge + clearance.
        assert_eq!(lane_y(&rects, none, true, 10.0, 400.0, 800.0, 30.0), 40.0);
    }

    /// The over/under lane always leaves through the source edge facing the
    /// target. `send_models` (below) → `data` (above): the lane must exit the
    /// TOP, not the bottom (the reported loop-under-the-node bug).
    #[test]
    fn lane_exits_toward_the_target() {
        // send_models center ~820, data center ~200 → target above → exit top.
        assert!(!lane_exits_below(820.0, 200.0));
        // Target below → exit bottom.
        assert!(lane_exits_below(200.0, 820.0));
        // Level nodes default to the bottom (a horizontal edge has no "up").
        assert!(lane_exits_below(400.0, 400.0));
    }

    /// The straight-route rule: a cutting orthogonal candidate loses to ANY
    /// rule-abiding one, price notwithstanding; curved candidates stay
    /// cost-governed; all-cutting-Polys falls back to the cheapest.
    #[test]
    fn straight_routes_never_cross_nodes_curved_stay_priced() {
        use super::{Route, pick_route};
        // Identifiable candidates: the polyline length doubles as an id.
        let poly = |n: usize| Route::Poly(vec![pos2(0.0, 0.0); n]);
        let marker = |r: &[Pos2]| r.len();

        // A cutting Poly at cost 100 vs a clean Poly at cost 3000 → the rule
        // beats the price: the clean one wins.
        let (_, pts) = pick_route(vec![
            (poly(2), vec![pos2(0.0, 0.0); 2], 100.0, true),
            (poly(3), vec![pos2(0.0, 0.0); 3], 3000.0, false),
        ]);
        assert_eq!(marker(&pts), 3);

        // A cutting CURVE is allowed to compete (its +2600 is already in the
        // cost) — cheapest of the allowed set wins.
        let (r, pts) = pick_route(vec![
            (poly(2), vec![pos2(0.0, 0.0); 2], 100.0, true), // banned
            (
                Route::Bez([pos2(0.0, 0.0); 4]),
                vec![pos2(0.0, 0.0); 9],
                2700.0,
                true,
            ),
            (poly(4), vec![pos2(0.0, 0.0); 4], 2900.0, false),
        ]);
        assert!(matches!(r, Route::Bez(_)));
        assert_eq!(marker(&pts), 9);

        // Every candidate a cutting Poly (dense Straight diagram) → fallback
        // to the cheapest so the edge still draws.
        let (_, pts) = pick_route(vec![
            (poly(2), vec![pos2(0.0, 0.0); 2], 500.0, true),
            (poly(3), vec![pos2(0.0, 0.0); 3], 300.0, true),
        ]);
        assert_eq!(marker(&pts), 3);
    }

    /// The MIRROR of `side_by_side_call_goes_under_the_caller_not_across_it`:
    /// reported `radar → parameter` with the CALLER on the RIGHT. The old
    /// route left radar's right side, looped around its edge and came back at
    /// row height across radar's whole face. Verifies the exact current
    /// pipeline values: exit corner (top — parameter's center is the higher),
    /// entry side (parameter's RIGHT, facing the exit corner), a lane that
    /// hugs radar's top, and a bad route that pays the box-cut.
    #[test]
    fn mirror_side_by_side_caller_right_goes_over_not_around() {
        // Screen-space geometry lifted from the screenshot.
        let parameter = Rect::from_min_size(pos2(40.0, 45.0), vec2(560.0, 420.0));
        let radar = Rect::from_min_size(pos2(800.0, 45.0), vec2(740.0, 480.0));
        let rects = [parameter, radar];
        let row_y = 345.0; // parameter's `ReadParam` row
        let slack = 3.0;

        // lane_exits_below(radar_cy=285, parameter_cy=255) → false → exit TOP.
        assert!(!lane_exits_below(285.0, 255.0));
        let from = pos2(radar.left() + 15.0, radar.top());
        // entry_right = exit x >= target center x → parameter's RIGHT side.
        assert!(from.x >= parameter.center().x);
        let to = pos2(parameter.right(), row_y);
        let turn_x = to.x + 16.0;

        // The lane's run [turn_x, from.x] touches radar only → it hugs
        // radar's top edge instead of over-clearing.
        let lane = lane_y(&rects, 0, false, from.y, from.x, turn_x, 30.0);
        assert_eq!(lane, radar.top() - 30.0);

        let over = rounded_path(
            &[
                from,
                pos2(from.x, lane),
                pos2(turn_x, lane),
                pos2(turn_x, to.y),
                to,
            ],
            12.0,
        );
        assert!(!path_cuts_box(&over, radar, true, slack));
        assert!(!path_cuts_box(&over, parameter, true, slack));

        // The screenshot's route: out radar's RIGHT side, around its edge,
        // back at row height — the return leg crosses radar's face, which the
        // endpoint slack must NOT excuse.
        let around = rounded_path(
            &[
                pos2(radar.right(), 85.0),
                pos2(radar.right() + 30.0, 85.0),
                pos2(radar.right() + 30.0, row_y),
                to,
            ],
            12.0,
        );
        assert!(path_cuts_box(&around, radar, true, slack));
        // And even ignoring the penalty it is the long way.
        let len = |p: &[Pos2]| p.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
        assert!(len(&over) < len(&around));
    }

    /// The reported purple line: a route that reaches the callee's row by
    /// running across its whole face must be priced, even though the callee is
    /// its own endpoint — the old "skip the last segment" rule made exactly
    /// that traversal free, so the edge was drawn over `data`'s rows.
    #[test]
    fn a_route_across_its_own_target_face_is_still_a_cut() {
        let data = Rect::from_min_size(pos2(160.0, 30.0), vec2(370.0, 260.0));
        let row_y = 245.0; // the `CommandID` row
        let slack = 3.0;

        // Burrow: comes from the right, crosses the face, lands on the anchor
        // at the FAR (left) edge — what the screenshot shows.
        let burrow = [pos2(600.0, row_y), pos2(data.left(), row_y)];
        assert!(path_cuts_box(&burrow, data, true, slack));

        // Legitimate approach: from outside, stops AT the near edge's anchor.
        let approach = [pos2(600.0, row_y), pos2(data.right(), row_y)];
        assert!(!path_cuts_box(&approach, data, true, slack));

        // Same for the source end: leaving the top edge upwards is free…
        let exit = [pos2(300.0, data.top()), pos2(300.0, data.top() - 90.0)];
        assert!(!path_cuts_box(&exit, data, true, slack));
        // …while a leg dropped down THROUGH the node is not.
        let through = [
            pos2(300.0, data.top() - 20.0),
            pos2(300.0, data.bottom() + 20.0),
        ];
        assert!(path_cuts_box(&through, data, true, slack));

        // A third node in the way is granted no slack: a shallow clip of its
        // face counts…
        let graze = [pos2(600.0, row_y), pos2(data.right() - 2.0, row_y)];
        assert!(path_cuts_box(&graze, data, false, slack));
        // …while on the route's OWN endpoint that much is the anchor's grace.
        assert!(!path_cuts_box(&graze, data, true, slack));
        // Degenerate box (zoomed far out) must not panic on the shrink.
        let tiny = Rect::from_min_size(pos2(0.0, 0.0), vec2(2.0, 2.0));
        assert!(!path_cuts_box(&burrow, tiny, true, slack));
    }

    /// Reported `radar → parameter`: two nodes SIDE BY SIDE at the same
    /// height. In the Straight style the only shapes were the outer lanes,
    /// whose final leg runs back across the caller's own face — which is how
    /// the edge ended up drawn over `radar`'s rows, after looping out to its
    /// left. The under-lane dips below the caller instead: no cut, and short.
    #[test]
    fn side_by_side_call_goes_under_the_caller_not_across_it() {
        let radar = Rect::from_min_size(pos2(85.0, 40.0), vec2(245.0, 145.0));
        let parameter = Rect::from_min_size(pos2(390.0, 45.0), vec2(190.0, 135.0));
        let rects = [radar, parameter];
        let row_y = 143.0; // parameter's `ReadParam` row
        let to = pos2(parameter.left(), row_y); // near-side entry: caller is left
        let from = pos2(radar.right() - 15.0, radar.bottom()); // exit corner
        let turn_x = to.x - 16.0;
        let slack = 3.0;

        let lane = lane_y(&rects, 1, true, from.y, from.x, turn_x, 30.0);
        let under = rounded_path(
            &[
                from,
                pos2(from.x, lane),
                pos2(turn_x, lane),
                pos2(turn_x, to.y),
                to,
            ],
            12.0,
        );
        // Clears BOTH boxes — the caller's face included (only the anchors
        // touch, within the slack).
        assert!(!path_cuts_box(&under, radar, true, slack));
        assert!(!path_cuts_box(&under, parameter, true, slack));

        // The outer-lane shape it replaces: out the caller's left side, down
        // the outer lane, then a long leg back across `radar` into the row.
        let outer = rounded_path(
            &[
                pos2(radar.left(), 60.0),
                pos2(40.0, 60.0),
                pos2(40.0, row_y),
                to,
            ],
            12.0,
        );
        assert!(path_cuts_box(&outer, radar, true, slack));
        // …and it is the long way round, on top of the cut.
        let len = |p: &[Pos2]| p.windows(2).map(|w| w[0].distance(w[1])).sum::<f32>();
        assert!(len(&under) < len(&outer));
    }

    /// The callee is never an obstacle for its own lane. Reported case:
    /// `main → i2c1` — the callee sits above and to the right, so its x-range
    /// overlaps the run and counting it hoisted the route right over the node
    /// instead of taking it straight up into the row.
    #[test]
    fn lane_ignores_the_callee_box() {
        let main_ = Rect::from_min_size(pos2(450.0, 1240.0), vec2(370.0, 300.0));
        let i2c1 = Rect::from_min_size(pos2(580.0, 130.0), vec2(280.0, 360.0));
        let rects = [main_, i2c1];
        let from_x = main_.right() - 30.0; // caller's top-right exit corner
        let turn_x = i2c1.left() - 16.0; // vertical leg beside the callee
        // Skipping the callee (index 1): the lane hugs the caller's top edge,
        // so the route is "up, over, up into the row" — the shape asked for.
        let lane = lane_y(&rects, 1, false, main_.top(), from_x, turn_x, 30.0);
        assert_eq!(lane, main_.top() - 30.0);
        // Counting it would lift the lane above the callee — the whole edge
        // would loop over the node it is heading into.
        let looped = lane_y(&rects, usize::MAX, false, main_.top(), from_x, turn_x, 30.0);
        assert!(looped < i2c1.top(), "{looped} should be above i2c1");
    }

    /// The auto-fit geometry that crashed the tab: `content` comes from
    /// `width * ((avail - 2·FIT_PAD) / width)`, whose two roundings put it a
    /// hair ABOVE `avail - 2·FIT_PAD` while `content + 2·FIT_PAD` still rounds
    /// to `<= avail`. The old branch then called `clamp(20.0, 19.999985)` and
    /// panicked ("min > max"). Reproduces with width 100 / avail 256.
    #[test]
    fn clamp_rel_survives_the_auto_fit_rounding() {
        let (w, avail) = (100.0_f32, 256.0_f32);
        let content = w * ((avail - 2.0 * FIT_PAD) / w);
        // The exact disagreement this guards against.
        assert!(content + 2.0 * FIT_PAD <= avail);
        assert!(avail - content - FIT_PAD < FIT_PAD);
        // Must not panic, and must still park the diagram at the padding.
        let got = clamp_rel(FIT_PAD, avail, content);
        assert!(got.is_finite());
        assert!((got - (avail - content - FIT_PAD)).abs() < 0.001);
    }

    /// The panning contract on both sides of the fit boundary.
    #[test]
    fn clamp_rel_pins_when_it_fits_and_slides_when_it_overflows() {
        // Fits with room to spare: dragging past either edge parks it there,
        // anything in between is free.
        assert_eq!(clamp_rel(-999.0, 600.0, 200.0), FIT_PAD);
        assert_eq!(clamp_rel(999.0, 600.0, 200.0), 600.0 - 200.0 - FIT_PAD);
        assert_eq!(clamp_rel(100.0, 600.0, 200.0), 100.0);
        // Overflows: slides edge to edge, never further.
        assert_eq!(clamp_rel(999.0, 300.0, 800.0), FIT_PAD);
        assert_eq!(clamp_rel(-9999.0, 300.0, 800.0), 300.0 - 800.0 - FIT_PAD);
        assert_eq!(clamp_rel(-100.0, 300.0, 800.0), -100.0);
        // A non-finite bound returns a usable offset instead of panicking.
        assert!(clamp_rel(0.0, f32::NAN, 100.0).is_finite());
    }

    /// The diagram of a two-module crate, frame by frame.
    struct Diagram {
        ctx: eframe::egui::Context,
        graph: crate::panels::structure_map::parse::ModuleGraph,
        lay: crate::panels::structure_map::layout::GraphLayout,
        view: super::StructureView,
        pass: u64,
        /// Every node a frame reported a finished drag on - the ones the
        /// Structure tab pins.
        pinned: Vec<usize>,
    }

    impl Diagram {
        fn new() -> Self {
            use crate::panels::structure_map::{layout, parse};
            let files = [("net.rs".to_owned(), "pub fn send() {}".to_owned())];
            let graph = parse::build_graph("mod net;\nfn main() { net::send(); }", &files);
            let lay = layout::layout(&graph);
            let mut d = Self {
                ctx: eframe::egui::Context::default(),
                graph,
                lay,
                view: super::StructureView::default(),
                pass: 0,
                pinned: Vec::new(),
            };
            for _ in 0..3 {
                d.step(vec![]);
            }
            d
        }

        fn step(&mut self, events: Vec<eframe::egui::Event>) {
            self.pass += 1;
            let input = eframe::egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1200.0, 800.0))),
                time: Some(self.pass as f64 / 30.0),
                predicted_dt: 1.0 / 30.0,
                events,
                ..Default::default()
            };
            let (graph, lay, view) = (&self.graph, &mut self.lay, &mut self.view);
            let errors = vec![false; graph.nodes.len()];
            let (refs, pairs) = (Default::default(), Default::default());
            let mut moved = None;
            let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
                let r = super::show(
                    ui,
                    graph,
                    lay,
                    view,
                    &[],
                    &[],
                    "",
                    0,
                    &errors,
                    &refs,
                    &pairs,
                );
                moved = r.moved;
            });
            self.pinned.extend(moved);
        }

        /// Node `i`'s drag handle on the screen.
        fn handle(&self, i: usize) -> Rect {
            self.ctx
                .viewport_for(eframe::egui::ViewportId::ROOT, |vp| {
                    vp.prev_pass
                        .widgets
                        .layers()
                        .flat_map(|(_, ws)| ws.iter())
                        .find(|w| w.id == w.parent_id.with(("structure_drag", i)))
                        .map(|w| w.interact_rect)
                })
                .expect("the diagram drew the node")
        }

        /// Hover `at`, press there, move the pointer by `by` in `steps`
        /// frames, release where it ended, and let a frame pass.
        fn gesture(&mut self, at: Pos2, by: eframe::egui::Vec2, steps: usize) {
            self.gesture_with(eframe::egui::PointerButton::Primary, at, by, steps);
        }

        /// [`Self::gesture`] with any mouse button.
        fn gesture_with(
            &mut self,
            which: eframe::egui::PointerButton,
            at: Pos2,
            by: eframe::egui::Vec2,
            steps: usize,
        ) {
            let button = |pos, pressed| eframe::egui::Event::PointerButton {
                pos,
                button: which,
                pressed,
                modifiers: eframe::egui::Modifiers::NONE,
            };
            self.step(vec![eframe::egui::Event::PointerMoved(at)]);
            self.step(vec![button(at, true)]);
            let mut p = at;
            for _ in 0..steps {
                p += by / steps as f32;
                self.step(vec![eframe::egui::Event::PointerMoved(p)]);
            }
            self.step(vec![button(p, false)]);
            self.step(vec![]);
        }
    }

    /// A click on a node's header whose hand drifts 3 px off it neither moves
    /// the node nor pins it; a real drag still does both. egui 0.36 starts the
    /// drag the moment the pointer leaves the header, well before the click
    /// distance - and ends it on release like any other.
    #[test]
    fn a_node_moves_only_on_a_real_drag() {
        let mut d = Diagram::new();
        // `main`, not `net`: the used module is laid out on the top margin,
        // where a slip upwards could not move it anyway.
        let i = d
            .graph
            .nodes
            .iter()
            .position(|n| n.name == "main")
            .expect("the crate root is a node");
        let start = d.lay.pos[i].y;
        let header = d.handle(i);

        d.gesture(
            pos2(header.center().x, header.top() + 1.0),
            vec2(0.0, -3.0),
            2,
        );
        assert_eq!(d.lay.pos[i].y, start, "a slipped click moved the node");
        assert!(d.pinned.is_empty(), "…and pinned it: {:?}", d.pinned);

        let scale = d.view.last_scale;
        d.gesture(d.handle(i).center(), vec2(0.0, 30.0), 3);
        let went = (d.lay.pos[i].y - start) * scale;
        assert!(
            (went - 30.0).abs() < 1.0,
            "it followed the 30 px drag: {went}"
        );
        assert_eq!(d.pinned, vec![i], "and is pinned once, where it ended");
    }

    /// A slow nudge - the button held past the click time, the hand moved
    /// less than the click distance - is a real drag: it moves the node AND
    /// pins it there, or the node would jump back at the next relayout.
    #[test]
    fn a_slow_short_nudge_moves_the_node_and_pins_it() {
        let mut d = Diagram::new();
        let i = d
            .graph
            .nodes
            .iter()
            .position(|n| n.name == "main")
            .expect("the crate root is a node");
        let start = d.lay.pos[i].y;
        let at = d.handle(i).center();
        let button = |pos, pressed| eframe::egui::Event::PointerButton {
            pos,
            button: eframe::egui::PointerButton::Primary,
            pressed,
            modifiers: eframe::egui::Modifiers::NONE,
        };
        d.step(vec![eframe::egui::Event::PointerMoved(at)]);
        d.step(vec![button(at, true)]);
        // A second at 30 frames a second: past egui's 0.8 s click time.
        for _ in 0..30 {
            d.step(vec![]);
        }
        let to = at + vec2(0.0, 4.0);
        d.step(vec![eframe::egui::Event::PointerMoved(at + vec2(0.0, 2.0))]);
        d.step(vec![eframe::egui::Event::PointerMoved(to)]);
        d.step(vec![button(to, false)]);
        d.step(vec![]);
        assert!(d.lay.pos[i].y > start, "the nudge moved the node");
        assert_eq!(d.pinned, vec![i], "and pinned it where it ended");
    }

    /// Only a drag that MOVED the node pins it: not a right or middle click
    /// that slips off the header (egui's `clicked()` only knows the primary
    /// button), and not a press held still past the click time.
    #[test]
    fn a_node_that_did_not_move_is_not_pinned() {
        let mut d = Diagram::new();
        let i = d
            .graph
            .nodes
            .iter()
            .position(|n| n.name == "main")
            .expect("the crate root is a node");
        let start = d.lay.pos[i].y;
        let header = d.handle(i);
        let edge = pos2(header.center().x, header.top() + 1.0);
        for which in [
            eframe::egui::PointerButton::Secondary,
            eframe::egui::PointerButton::Middle,
        ] {
            d.gesture_with(which, edge, vec2(0.0, -3.0), 2);
            assert!(d.pinned.is_empty(), "a slipped {which:?} click pinned it");
        }

        let at = d.handle(i).center();
        let button = |pos, pressed| eframe::egui::Event::PointerButton {
            pos,
            button: eframe::egui::PointerButton::Primary,
            pressed,
            modifiers: eframe::egui::Modifiers::NONE,
        };
        d.step(vec![eframe::egui::Event::PointerMoved(at)]);
        d.step(vec![button(at, true)]);
        for _ in 0..30 {
            d.step(vec![]);
        }
        d.step(vec![button(at, false)]);
        d.step(vec![]);
        assert_eq!(d.lay.pos[i].y, start);
        assert!(
            d.pinned.is_empty(),
            "a still hold pinned it: {:?}",
            d.pinned
        );
    }
}
