//! Data-driven clock diagram renderer — CubeMX-style vector schematic.
//!
//! Everything is driven by a [`ClockLayout`] (blocks, outputs, tags, wires)
//! plus [`Widget`]s editing [`ClockGraph`] node states:
//! - Orthogonal wiring routed in lanes so paths never cross blocks.
//! - Mux selectors drawn as **trapezoids with radio buttons** so the active
//!   input is obvious — like CubeMX.
//! - Frequency tags/boxes turn red past their datasheet limit.
//!
//! The figure is drawn at its natural size ([`ClockLayout::bounds`]); the caller
//! wraps it in an [`egui::Scene`], which owns zoom and pan.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Shape, Stroke, UiBuilder, Vec2};

use super::super::graph::layout::{ClockLayout, NodeBox, ValueSrc, Widget};
use super::super::graph::model::{ClockGraph, NodeState};
use super::super::graph::validate::ceiling_for;
use super::super::model::ClockLimits;

// ── Palette ───────────────────────────────────────────────────────────────────
const BG: Color32 = Color32::from_rgb(26, 28, 34);
const BOX_FILL: Color32 = Color32::from_rgb(42, 46, 56);
const OUT_FILL: Color32 = Color32::from_rgb(34, 40, 50);
const STROKE_C: Color32 = Color32::from_rgb(140, 150, 165);
const WIRE_C: Color32 = Color32::from_rgb(110, 122, 140);
const LABEL_C: Color32 = Color32::from_rgb(208, 215, 228);
const DIM_C: Color32 = Color32::from_rgb(150, 158, 172);
const MUX_FILL: Color32 = Color32::from_rgb(50, 56, 70);
const MUX_STROKE: Color32 = Color32::from_rgb(95, 140, 215);
const FREQ_OK: Color32 = Color32::from_rgb(120, 205, 140);
const FREQ_BAD: Color32 = Color32::from_rgb(235, 95, 85);
/// Outline of a draggable box in edit mode — amber, so "this is a layout edit"
/// reads differently from the blue mux selection.
const EDIT_STROKE: Color32 = Color32::from_rgb(225, 175, 75);
/// The selected node, and the wire being drawn — brighter than the rest so the
/// current subject of the properties panel is unmistakable.
const SELECT_STROKE: Color32 = Color32::from_rgb(255, 232, 150);
/// A mux's selected source, and the caption naming the choice. Muxes are where a
/// clock tree is actually DECIDED, so they get picked out of the dozens of other
/// captions on a datasheet-sized figure.
const MUX_VALUE_C: Color32 = Color32::from_rgb(240, 150, 60);
const MUX_TITLE_C: Color32 = Color32::from_rgb(235, 210, 90);
/// Every wire in and out of the node under the pointer.
const TRACE_C: Color32 = Color32::from_rgb(226, 178, 62);
/// The single wire under the pointer — brightest thing on the canvas, because
/// following one line across a 3000 px figure is what this is for.
const TRACE_ONE_C: Color32 = Color32::from_rgb(245, 248, 255);

/// Places the layout's virtual coordinates at the allocated canvas origin.
///
/// A pure translation: zoom belongs to the enclosing [`egui::Scene`], which
/// transforms the whole layer, so the diagram is painted 1:1 and font sizes are
/// plain point values again.
#[derive(Clone, Copy)]
pub(crate) struct Tf {
    origin: Pos2,
}
impl Tf {
    fn p(&self, x: f32, y: f32) -> Pos2 {
        self.origin + Vec2::new(x, y)
    }
    fn r(&self, x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::from_min_size(self.p(x, y), Vec2::new(w, h))
    }
}

/// Shared static-diagram renderer. Allocates the layout's natural size, paints
/// the background + wires + blocks/outputs/tags/labels/mux-titles, and returns
/// the canvas `Rect` + transform. `resolve` supplies each box/tag's frequency —
/// `value_of` for the F103 typed path, `value_from_graph` for imported graph
/// clocks. No interactivity (callers add their own).
pub(crate) fn draw_static_diagram<R: Fn(&ValueSrc) -> u32, M: Fn(&str) -> bool>(
    ui: &mut egui::Ui,
    lay: &ClockLayout,
    l: &ClockLimits,
    resolve: R,
    is_mux: M,
) -> (Rect, Tf) {
    let (w, h) = lay.bounds();
    let (rect, _resp) = ui.allocate_exact_size(Vec2::new(w, h), egui::Sense::hover());
    let tf = Tf { origin: rect.min };

    let clip = rect.intersect(ui.clip_rect());
    let painter = ui.painter().with_clip_rect(clip);
    painter.rect_filled(rect, 4.0, BG);

    let hover = hovered(ui, &tf, lay);
    draw_wires(&painter, &tf, lay, &hover);
    draw_static(&painter, &tf, lay, l, &resolve, &is_mux);
    (rect, tf)
}

/// What the pointer is over: a node's body, or a single wire.
///
/// Both are answered geometrically rather than from stored ids. A wire's ends
/// lie ON the boxes it joins by construction — it leaves the source's right edge
/// and enters the target's left — so "which wires touch this node" needs no
/// bookkeeping, and stays right when a node is dragged.
#[derive(Default)]
struct Hover {
    /// Screen rect of the node body under the pointer.
    node: Option<Rect>,
    /// Index into `lay.wires`.
    wire: Option<usize>,
}

fn hovered(ui: &egui::Ui, tf: &Tf, lay: &ClockLayout) -> Hover {
    let mut out = Hover::default();
    if !ui.rect_contains_pointer(ui.clip_rect()) {
        return out;
    }
    // A node body wins over the wires under it: it is the bigger target and the
    // one the pointer is most likely aimed at.
    for b in &lay.nodes {
        let r = tf.r(b.x, b.y, b.w, b.h);
        if ui.rect_contains_pointer(r) {
            out.node = Some(r);
            return out;
        }
    }
    for (i, poly) in lay.wires.iter().enumerate() {
        if poly.windows(2).any(|s| {
            let (a, b) = (tf.p(s[0].0, s[0].1), tf.p(s[1].0, s[1].1));
            // A segment is axis-aligned, so its hit area is the rectangle it
            // spans, fattened to something a mouse can actually land on.
            ui.rect_contains_pointer(Rect::from_two_pos(a, b).expand(3.0))
        }) {
            out.wire = Some(i);
            return out;
        }
    }
    out
}

// ──────────────────────────────────────────────────────────────────────────────
// Wires (drawn first, under the blocks)
// ──────────────────────────────────────────────────────────────────────────────

fn draw_wires(p: &egui::Painter, tf: &Tf, lay: &ClockLayout, hover: &Hover) {
    // CubeMX style: muxes show their sources as labelled stubs (drawn inside
    // `mux_radios`), so the routed wires are short, local links only. The
    // polyline geometry is data ([`ClockLayout::wires`]).
    //
    // Highlighted wires are drawn in a SECOND pass, so a traced line is never
    // hidden under the plain ones it crosses.
    let mut lit: Vec<(&Vec<(f32, f32)>, Color32)> = Vec::new();
    for (i, poly) in lay.wires.iter().enumerate() {
        let colour = if hover.wire == Some(i) {
            Some(TRACE_ONE_C)
        } else if hover.node.is_some_and(|r| touches(tf, poly, r)) {
            Some(TRACE_C)
        } else {
            None
        };
        match colour {
            Some(c) => lit.push((poly, c)),
            None => wire(p, tf, poly),
        }
    }
    for (poly, c) in lit {
        wire_in(p, tf, poly, c, 2.0);
    }
}

/// Does this wire start or end on `rect`? Its ends sit exactly on the node
/// edges, so a pixel of slack is enough.
fn touches(tf: &Tf, poly: &[(f32, f32)], rect: Rect) -> bool {
    let r = rect.expand(2.0);
    poly.first().is_some_and(|&(x, y)| r.contains(tf.p(x, y)))
        || poly.last().is_some_and(|&(x, y)| r.contains(tf.p(x, y)))
}

// ──────────────────────────────────────────────────────────────────────────────
// Static blocks + labels + frequency tags — all driven by `ClockLayout` data
// ──────────────────────────────────────────────────────────────────────────────

fn draw_static<R: Fn(&ValueSrc) -> u32, M: Fn(&str) -> bool>(
    p: &egui::Painter,
    tf: &Tf,
    lay: &ClockLayout,
    l: &ClockLimits,
    resolve: &R,
    is_mux: &M,
) {
    for b in &lay.blocks {
        block(p, tf, b.x, b.y, b.w, b.h, &b.label);
    }

    for o in &lay.outputs {
        let hz = resolve(&o.src);
        let limit = o.limit.and_then(|k| ceiling_for(k, l));
        out_box(p, tf, o.x, o.y, o.w, o.h, &o.label, hz, limit);
    }

    for t in &lay.tags {
        let hz = resolve(&t.src);
        let bad = t
            .limit
            .and_then(|k| ceiling_for(k, l))
            .map_or(false, |lim| over(hz, lim));
        tag(p, tf, t.x, t.y, &mhz(hz), bad, &t.name);
    }

    for lb in &lay.labels_above {
        // A caption belongs to a mux when the node it names is one — asked of
        // the graph rather than stored, so no saved layout has to carry it.
        let mux = lb.node.as_deref().is_some_and(is_mux);
        label_above(p, tf, lb.x, lb.y, &lb.text, mux);
    }

    for mt in &lay.mux_titles {
        p.text(
            tf.p(mt.x, mt.y),
            Align2::CENTER_BOTTOM,
            &mt.text,
            FontId::proportional(9.0),
            DIM_C,
        );
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Edit mode — drag the node boxes
// ──────────────────────────────────────────────────────────────────────────────

/// What the edit overlay saw this frame. Everything is reported back rather
/// than acted on here, because the actual edits are graph operations
/// ([`super::super::graph::edit`]) that the caller owns.
#[derive(Default)]
pub(crate) struct EditHits {
    /// A box is being dragged right now.
    pub dragged: bool,
    /// A box was clicked — the new selection.
    pub select: Option<String>,
    /// The output port (right edge) was clicked: start wiring from here.
    pub out_port: Option<String>,
    /// The input port (left edge) was clicked: finish wiring into here.
    pub in_port: Option<String>,
}

/// Edit-mode overlay: draw each [`NodeBox`] as a draggable plate with an input
/// and an output port, move it with the pointer, and report clicks.
///
/// This REPLACES [`interactive_graph`] rather than sitting next to it — the
/// value controls occupy exactly these rectangles, so a drag would otherwise
/// fight the dropdown underneath it. Values stay readable: the name labels above
/// and the frequency tags below are painted by the static layer either way.
///
/// `drag_delta` is already divided by the enclosing [`egui::Scene`]'s zoom, so a
/// box follows the cursor exactly at any zoom level.
pub(crate) fn edit_nodes(
    ui: &mut egui::Ui,
    tf: &Tf,
    nodes: &mut [NodeBox],
    selected: Option<&str>,
    linking: Option<&str>,
) -> EditHits {
    let mut hits = EditHits::default();

    for nb in nodes.iter_mut() {
        let rect = tf.r(nb.x, nb.y, nb.w, nb.h);
        let resp = ui.interact(
            rect,
            egui::Id::new(("clock_node_drag", &nb.node)),
            egui::Sense::click_and_drag(),
        );
        let is_sel = selected == Some(nb.node.as_str());
        let active = resp.dragged() || resp.hovered() || is_sel;
        let p = ui.painter();
        p.rect(
            rect,
            3.0,
            if resp.dragged() { MUX_FILL } else { BOX_FILL },
            Stroke::new(
                if is_sel {
                    2.4_f32
                } else if active {
                    1.8_f32
                } else {
                    1.0_f32
                },
                if is_sel { SELECT_STROKE } else { EDIT_STROKE },
            ),
            egui::StrokeKind::Inside,
        );
        p.text(
            rect.center(),
            Align2::CENTER_CENTER,
            &nb.node,
            FontId::proportional(9.0),
            LABEL_C,
        );
        if resp.dragged() && crate::panels::drag_decided(ui) {
            let d = resp.drag_delta();
            // Keep the diagram in positive space — `ClockLayout::bounds` measures
            // the far edge, so anything dragged past the origin would be clipped.
            nb.x = (nb.x + d.x).max(0.0);
            nb.y = (nb.y + d.y).max(0.0);
            hits.dragged = true;
        }
        if resp.clicked() {
            hits.select = Some(nb.node.clone());
        }

        // Ports last, so they sit on top of the plate and win the click.
        let wiring_from = linking == Some(nb.node.as_str());
        if port(
            ui,
            tf.p(nb.x + nb.w, nb.y + nb.h / 2.0),
            &nb.node,
            "out",
            wiring_from,
        ) {
            hits.out_port = Some(nb.node.clone());
        }
        if port(
            ui,
            tf.p(nb.x, nb.y + nb.h / 2.0),
            &nb.node,
            "in",
            linking.is_some() && !wiring_from,
        ) {
            hits.in_port = Some(nb.node.clone());
        }
    }

    // The wire being drawn, from its source port to the cursor.
    if let Some(from) = linking
        && let Some(nb) = nodes.iter().find(|n| n.node == from)
        && let Some(cursor) = ui.ctx().pointer_latest_pos()
    {
        ui.painter().line_segment(
            [tf.p(nb.x + nb.w, nb.y + nb.h / 2.0), cursor],
            Stroke::new(1.5_f32, SELECT_STROKE),
        );
    }

    hits
}

/// One connection port. `armed` highlights it as a valid next click.
fn port(ui: &mut egui::Ui, center: Pos2, node: &str, side: &'static str, armed: bool) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(11.0));
    let resp = ui.interact(
        rect,
        egui::Id::new(("clock_port", node, side)),
        egui::Sense::click(),
    );
    let hot = armed || resp.hovered();
    ui.painter().circle(
        center,
        if hot { 5.0 } else { 3.5 },
        if hot { SELECT_STROKE } else { MUX_FILL },
        Stroke::new(1.2_f32, if hot { SELECT_STROKE } else { STROKE_C }),
    );
    resp.clicked()
}

// ──────────────────────────────────────────────────────────────────────────────
// Generic interactive overlay for graph clocks
// ──────────────────────────────────────────────────────────────────────────────

/// Draw each [`Widget`] dropdown from the layout and write the picked
/// [`NodeState`] back into `graph`. Returns `true` when a state changed.
pub(crate) fn interactive_graph(
    ui: &mut egui::Ui,
    tf: &Tf,
    graph: &mut ClockGraph,
    widgets: &[Widget],
) -> bool {
    let mut pending: Option<(String, NodeState)> = None;
    for w in widgets {
        match w {
            Widget::Combo {
                node,
                x,
                y,
                w: cw,
                options,
            } => {
                let Some(n) = graph.node(node) else {
                    continue;
                };
                let cur = n.state.clone();
                // A mux's value is WHICH SOURCE feeds this branch — the one
                // choice on a clock tree that changes its shape rather than its
                // arithmetic, so it reads in its own colour.
                let mux = matches!(
                    n.kind,
                    crate::panels::mcu_module::clock::graph::model::NodeKind::Mux { .. }
                );
                if let Some(state) = graph_combo(ui, tf, *x, *y, *cw, node, &cur, options, mux) {
                    pending = Some((node.clone(), state));
                }
            }
            // Trapezoid mux — reuses the proven `mux_radios` primitive 1:1.
            Widget::MuxRadios {
                node,
                x,
                y,
                w: mw,
                h,
                flip,
                inputs,
            } => {
                let Some(cur) = graph.node(node).map(|n| n.state.clone()) else {
                    continue;
                };
                let selected = inputs.iter().position(|(_, _, st)| st == &cur);
                let labels: Vec<(&str, f32)> =
                    inputs.iter().map(|(l, dy, _)| (l.as_str(), *dy)).collect();
                let mut picked: Option<usize> = None;
                mux_radios(ui, tf, *x, *y, *mw, *h, &labels, selected, *flip, |i| {
                    picked = Some(i);
                });
                if let Some((_, _, st)) = picked.and_then(|i| inputs.get(i)) {
                    pending = Some((node.clone(), st.clone()));
                }
            }
            Widget::DragMhz {
                node,
                x,
                y,
                w: dw,
                min_mhz,
                max_mhz,
            } => {
                let Some(NodeState::Source { enabled, hz }) =
                    graph.node(node).map(|n| n.state.clone())
                else {
                    continue;
                };
                let rect = tf.r(*x, *y, *dw, 22.0);
                let mut mhz = hz as f64 / 1e6;
                let mut dragged = false;
                ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
                    if ui
                        .add(
                            crate::panels::drag_value(ui, &mut mhz)
                                .range(*min_mhz as f64..=*max_mhz as f64)
                                .speed(0.1)
                                .suffix(" MHz"),
                        )
                        .changed()
                    {
                        dragged = true;
                    }
                });
                if dragged {
                    pending = Some((
                        node.clone(),
                        NodeState::Source {
                            enabled,
                            hz: (mhz * 1e6).round() as u32,
                        },
                    ));
                }
            }
        }
    }
    if let Some((id, state)) = pending {
        if let Some(n) = graph.node_mut(&id) {
            if n.state != state {
                n.state = state;
                return true;
            }
        }
    }
    false
}

/// A dropdown over `(label, state)` options bound to a graph node.
fn graph_combo(
    ui: &mut egui::Ui,
    tf: &Tf,
    x: f32,
    y: f32,
    w: f32,
    id: &str,
    current: &NodeState,
    options: &[(String, NodeState)],
    mux: bool,
) -> Option<NodeState> {
    let rect = tf.r(x, y, w, 26.0);
    let cur_label = options
        .iter()
        .find(|(_, s)| s == current)
        .map(|(l, _)| l.as_str())
        .unwrap_or("?");
    let mut picked = None;
    ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
        egui::ComboBox::from_id_salt(("graph_combo", id))
            .selected_text({
                let txt = egui::RichText::new(cur_label).size(9.0);
                if mux { txt.color(MUX_VALUE_C) } else { txt }
            })
            .width(rect.width())
            .show_ui(ui, |ui| {
                for (label, state) in options {
                    if ui.selectable_label(state == current, label).clicked() {
                        picked = Some(state.clone());
                    }
                }
            });
    });
    picked
}

/// Draw a vertical trapezoid mux with one radio button per input. Returns
/// `true` and runs `on_pick` if the user selects a new input.
///
/// `flip = false`: inputs on the left, output on the right (the usual case).
/// `flip = true`:  mirrored — inputs on the right, output on the left (MCO).
fn mux_radios(
    ui: &mut egui::Ui,
    tf: &Tf,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    inputs: &[(&str, f32)], // (label, dy from y where the input enters)
    selected: Option<usize>,
    flip: bool,
    mut on_pick: impl FnMut(usize),
) -> bool {
    let painter = ui.painter().clone();
    // Small, bounded taper → nearly rectangular mux so the radio buttons always
    // sit comfortably INSIDE the trapezoid (CubeMX style).
    let taper = (h * 0.16).clamp(8.0, 12.0);
    let poly = if flip {
        // Tall on the right (inputs), short on the left (output).
        vec![
            tf.p(x + w, y),
            tf.p(x + w, y + h),
            tf.p(x, y + h - taper),
            tf.p(x, y + taper),
        ]
    } else {
        // Tall on the left (inputs), short on the right (output).
        vec![
            tf.p(x, y),
            tf.p(x, y + h),
            tf.p(x + w, y + h - taper),
            tf.p(x + w, y + taper),
        ]
    };
    painter.add(Shape::convex_polygon(
        poly,
        MUX_FILL,
        Stroke::new(1.2_f32, MUX_STROKE),
    ));

    let mut changed = false;
    for (i, (label, dy)) in inputs.iter().enumerate() {
        let cy = y + dy;
        let stroke = Stroke::new(1.2_f32, WIRE_C);
        // Radio sits just inside the wide edge (left for normal, right for flip);
        // the input stub stops at the mux edge so the wire meets the radio.
        let (stub_a, stub_b, lbl_x, lbl_align, radio_cx) = if flip {
            (
                x + w + 24.0,
                x + w,
                x + w + 26.0,
                Align2::LEFT_CENTER,
                x + w - 11.0,
            )
        } else {
            (x - 24.0, x, x - 26.0, Align2::RIGHT_CENTER, x + 11.0)
        };
        painter.line_segment([tf.p(stub_a, cy), tf.p(stub_b, cy)], stroke);
        painter.text(
            tf.p(lbl_x, cy),
            lbl_align,
            *label,
            FontId::proportional(8.0),
            DIM_C,
        );

        let center = tf.p(radio_cx, cy);
        let rsz = 12.0;
        let rrect = Rect::from_center_size(center, Vec2::splat(rsz));
        let is_sel = selected == Some(i);
        if ui.put(rrect, egui::RadioButton::new(is_sel, "")).clicked() {
            on_pick(i);
            changed = true;
        }
    }
    changed
}

// ──────────────────────────────────────────────────────────────────────────────
// Primitive painters
// ──────────────────────────────────────────────────────────────────────────────

fn block(p: &egui::Painter, tf: &Tf, x: f32, y: f32, w: f32, h: f32, title: &str) {
    let r = tf.r(x, y, w, h);
    p.rect(
        r,
        3.0,
        BOX_FILL,
        Stroke::new(1.2_f32, STROKE_C),
        egui::StrokeKind::Inside,
    );
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        title,
        FontId::proportional(9.0),
        LABEL_C,
    );
}

fn out_box(
    p: &egui::Painter,
    tf: &Tf,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    label: &str,
    hz: u32,
    limit: Option<u32>,
) {
    let r = tf.r(x, y, w, h);
    p.rect(
        r,
        3.0,
        OUT_FILL,
        Stroke::new(1.0_f32, STROKE_C),
        egui::StrokeKind::Inside,
    );
    let bad = limit.map(|l| hz > l).unwrap_or(false);
    let col = if bad { FREQ_BAD } else { FREQ_OK };
    // value (left) + label (right)
    p.text(
        tf.p(x + 6.0, y + h / 2.0),
        Align2::LEFT_CENTER,
        mhz(hz),
        FontId::monospace(9.5),
        col,
    );
    p.text(
        tf.p(x + w - 6.0, y + h / 2.0),
        Align2::RIGHT_CENTER,
        label,
        FontId::proportional(8.5),
        DIM_C,
    );
}

fn label_above(p: &egui::Painter, tf: &Tf, x: f32, y: f32, text: &str, mux: bool) {
    p.text(
        tf.p(x, y),
        Align2::LEFT_BOTTOM,
        text,
        FontId::proportional(if mux { 9.0 } else { 8.5 }),
        if mux { MUX_TITLE_C } else { DIM_C },
    );
}

fn wire(p: &egui::Painter, tf: &Tf, pts: &[(f32, f32)]) {
    wire_in(p, tf, pts, WIRE_C, 1.3);
}

fn wire_in(p: &egui::Painter, tf: &Tf, pts: &[(f32, f32)], colour: Color32, width: f32) {
    let stroke = Stroke::new(width, colour);
    for w in pts.windows(2) {
        p.line_segment([tf.p(w[0].0, w[0].1), tf.p(w[1].0, w[1].1)], stroke);
    }
    if pts.len() >= 2 {
        let a = pts[pts.len() - 2];
        let b = pts[pts.len() - 1];
        arrowhead(p, tf, a, b);
    }
}

fn arrowhead(p: &egui::Painter, tf: &Tf, from: (f32, f32), to: (f32, f32)) {
    let a = tf.p(from.0, from.1);
    let b = tf.p(to.0, to.1);
    let dir = (b - a).normalized();
    if !dir.is_finite() {
        return;
    }
    let n = Vec2::new(-dir.y, dir.x);
    let s = 5.0;
    let p1 = b - dir * s + n * (s * 0.5);
    let p2 = b - dir * s - n * (s * 0.5);
    p.add(Shape::convex_polygon(vec![b, p1, p2], WIRE_C, Stroke::NONE));
}

fn tag(p: &egui::Painter, tf: &Tf, x: f32, y: f32, value: &str, bad: bool, name: &str) {
    let color = if bad { FREQ_BAD } else { FREQ_OK };
    p.text(
        tf.p(x, y),
        Align2::LEFT_CENTER,
        format!("{name} {value}"),
        FontId::monospace(9.0),
        color,
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn mhz(hz: u32) -> String {
    if hz == 0 {
        return "—".to_string();
    }
    if hz < 1_000_000 {
        let khz = hz as f64 / 1000.0;
        if (khz.fract()).abs() < 1e-6 {
            return format!("{} kHz", khz as u32);
        }
        return format!("{khz:.3} kHz");
    }
    let m = hz as f64 / 1e6;
    if (m.fract()).abs() < 1e-6 {
        format!("{} MHz", m as u32)
    } else {
        format!("{m:.2} MHz")
    }
}
fn over(hz: u32, limit: u32) -> bool {
    hz > limit
}
