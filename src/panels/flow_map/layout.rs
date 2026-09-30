//! Place a [`Chart`] on a virtual canvas (pure logic, tested).
//!
//! **Not** the layered/barycenter machinery the module diagram uses. Rust has
//! no `goto`, so a function body is a NESTING of sequences, branches and loops —
//! and a nesting lays out the way a document does: measure every sub-box, then
//! place it. Two passes, deterministic, no iteration to convergence.
//!
//! Everything is aligned on a SPINE: a vertical line that a sequence runs down,
//! that a diamond sits centred on, and that a branch's arms rejoin. `Size.spine`
//! is that line's offset from a sub-box's left edge, which is why it is tracked
//! separately from the width — a loop reserves a gutter on its left for the back
//! edge, so its spine is NOT its centre.
//!
//! The only edges that escape the nesting are `break`, `continue`, `return` and
//! `?`. Those are routed in gutters that the MEASURE pass reserves, so an
//! arrow never has to cross a box: a loop containing a `break` is physically
//! wider than one without.

use super::parse::{Chart, Flow, FlowNode, Shape, Target, falls_through};

// ── Virtual-unit constants (the GUI scales the whole canvas to fit) ──────────

const CHAR_W: f32 = 6.2;
const LINE_H: f32 = 14.0;
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 9.0;
const MIN_W: f32 = 96.0;
/// The drawing cap on a box's width.
///
/// It does NOT follow from the parser's label cap — 56 characters want 347
/// units here, and this is 300 — so a long label produces a box that does not
/// hold its own text. That is what [`text_room`] exists to tell the painter.
const MAX_W: f32 = 300.0;
const MIN_H: f32 = 32.0;
/// A diamond wastes its corners, so its text needs noticeably more room.
const DIAMOND_PAD: f32 = 44.0;
const DIAMOND_MIN_W: f32 = 116.0;
const DIAMOND_H: f32 = 48.0;
/// A stadium wastes its two round ends the way a diamond wastes its corners.
const TERMINAL_PAD: f32 = 40.0;
const TERMINAL_MIN_W: f32 = 90.0;
const TERMINAL_H: f32 = 30.0;
/// The parallelogram's slant, and the subroutine box's side bars.
const SLANT: f32 = 14.0;
const BARS: f32 = 16.0;
/// A declaration card is read down its left edge, so its text starts closer in
/// than a flow box's centred label — `draw_card_text` paints it here.
const CARD_PAD_LEFT: f32 = 10.0;
/// What is left of a card's reserve once the left inset is spent. The two sum
/// to `2 * PAD_X`, so naming the left one did not move any card's width.
const CARD_PAD_RIGHT: f32 = 2.0 * PAD_X - CARD_PAD_LEFT;
/// Spaces `draw_card_text` puts before every row under a card's header. It is
/// here, not in the painter, because [`box_size`] has to measure them: a card
/// sized for an unindented row is two characters too narrow for the one drawn.
pub const CARD_ROW_INDENT: usize = 2;
/// The least gap this feature ever leaves between a box's text and its border.
///
/// Every shape's own reserve below is larger, so this never binds today; it is
/// the floor [`text_insets`] is held to, and the guard that a future shape
/// cannot be given a reserve so thin the text touches the outline.
pub const MIN_TEXT_PAD: f32 = 5.0;

/// Vertical room between two boxes — arrowheads and edge labels live here.
pub const V_GAP: f32 = 36.0;
/// Horizontal room between a branch's arms.
const H_GAP: f32 = 26.0;
/// A routing lane reserved beside a loop (back edge, `break`, `continue`).
const GUTTER: f32 = 32.0;
/// Lane above an infinite loop's body where the back edge rejoins the spine.
const LOOP_TOP: f32 = 20.0;
/// An `if` with no `else`: the empty arm still needs a line to run down.
const EMPTY_H: f32 = 28.0;
/// Outer margin of the virtual canvas.
pub const MARGIN: f32 = 20.0;
/// A declaration card's width bounds.
const CARD_MIN_W: f32 = 150.0;
const CARD_MAX_W: f32 = 460.0;

// ── Output ───────────────────────────────────────────────────────────────────

/// A box on the canvas.
#[derive(Clone, Debug)]
pub struct Placed {
    pub node: FlowNode,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Placed {
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
}

/// What an edge means — the GUI colours by this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// Ordinary sequential flow.
    Flow,
    /// A loop's back edge.
    Back,
    Break,
    Continue,
    Return,
    /// The error path of a `?`.
    Try,
}

/// A routed polyline in virtual space.
#[derive(Clone, Debug)]
pub struct Edge {
    pub pts: Vec<(f32, f32)>,
    pub label: String,
    pub kind: EdgeKind,
    /// Draw an arrowhead at the last point (false where the edge merges into a
    /// junction rather than entering a box).
    pub arrow: bool,
}

#[derive(Clone, Debug, Default)]
pub struct FlowLayout {
    pub boxes: Vec<Placed>,
    pub edges: Vec<Edge>,
    pub width: f32,
    pub height: f32,
}

// ── Measure ──────────────────────────────────────────────────────────────────

/// A measured sub-tree: its extent, and where its spine runs inside it.
#[derive(Debug)]
struct M {
    w: f32,
    h: f32,
    spine: f32,
    kind: MKind,
}

#[derive(Debug)]
enum MKind {
    Node,
    Seq(Vec<M>),
    Branch {
        cond: (f32, f32),
        arms: Vec<M>,
    },
    Loop {
        head: Option<(f32, f32)>,
        body: Box<M>,
        /// Reserved lane widths, left and right of the loop's content.
        left: f32,
        right: f32,
    },
}

/// How far inside a box its text starts, on the left and on the right, for a
/// row whose centre sits at `t` of the box's height (0 = top, 1 = bottom).
///
/// The inverse of the reserve [`box_size`] adds, and the reason it has to exist
/// separately: the moment the `MAX_W` clamp fires, `box_size` returns a width
/// that does NOT hold the text it was handed, and nothing outside this module
/// can say by how much. Without it the painter drew the whole string centred
/// and let its clip rectangle cut the overflow — which takes as many characters
/// off the HEAD as off the tail, and the head is the half that says what the
/// statement is.
///
/// `t` matters for [`Shape::Io`] alone. The parallelogram is a SHEAR, not a
/// taper: `(left + s, top), (right, top), (right - s, bottom), (left, bottom)`
/// keeps the same usable width on every row and slides its centre sideways
/// instead. A diamond does narrow with `t`, but a [`Shape::Decision`] is built
/// from a single condition and never carries a second row, so its text sits on
/// the middle row where the shape is at its widest.
pub fn text_insets(shape: Shape, t: f32) -> (f32, f32) {
    match shape {
        Shape::Decision => (DIAMOND_PAD * 0.5, DIAMOND_PAD * 0.5),
        Shape::Terminal => (TERMINAL_PAD * 0.5, TERMINAL_PAD * 0.5),
        Shape::Decl => (CARD_PAD_LEFT, CARD_PAD_RIGHT),
        Shape::Io => {
            let t = t.clamp(0.0, 1.0);
            (PAD_X + SLANT * (1.0 - t), PAD_X + SLANT * t)
        }
        // The side bars are painted INSIDE the box, so the text clears them.
        Shape::Subroutine => (PAD_X + BARS * 0.5, PAD_X + BARS * 0.5),
        Shape::Process | Shape::Generated => (PAD_X, PAD_X),
    }
}

/// The width one row of text may occupy inside a box of width `w`.
pub fn text_room(shape: Shape, w: f32, t: f32) -> f32 {
    let (l, r) = text_insets(shape, t);
    (w - l - r).max(0.0)
}

/// Width and height of one box, from its shape and the rows it shows.
pub fn box_size(n: &FlowNode) -> (f32, f32) {
    let indent = if n.shape == Shape::Decl {
        CARD_ROW_INDENT
    } else {
        0
    };
    let widest = n
        .rows()
        .iter()
        .enumerate()
        .map(|(i, row)| row.chars().count() + if i == 0 { 0 } else { indent })
        .max()
        .unwrap_or(0) as f32;
    // The middle row: what the shape reserves there is what it reserves in
    // total, because `Io` only ever trades one side's inset for the other's.
    let (il, ir) = text_insets(n.shape, 0.5);
    let w = widest * CHAR_W + il + ir;
    match n.shape {
        Shape::Decision => (w.clamp(DIAMOND_MIN_W, MAX_W), DIAMOND_H),
        Shape::Terminal => (w.clamp(TERMINAL_MIN_W, MAX_W), TERMINAL_H),
        // A card reads like code, left-aligned, so it may run wider than a
        // flow box - a field's type is often the longest thing in the file.
        Shape::Decl => (
            w.clamp(CARD_MIN_W, CARD_MAX_W),
            2.0 * PAD_Y + n.lines() as f32 * LINE_H,
        ),
        _ => (
            w.clamp(MIN_W, MAX_W),
            (2.0 * PAD_Y + n.lines() as f32 * LINE_H).max(MIN_H),
        ),
    }
}

/// Combine sub-boxes that share one spine: the spine sits at the widest left
/// half, and the total is that plus the widest right half.
fn align(parts: impl Iterator<Item = (f32, f32)>) -> (f32, f32) {
    let (mut left, mut right) = (0.0f32, 0.0f32);
    for (w, spine) in parts {
        left = left.max(spine);
        right = right.max(w - spine);
    }
    (left + right, left)
}

fn measure(f: &Flow) -> M {
    match f {
        Flow::Node(n) | Flow::Jump { node: n, .. } => {
            let (w, h) = box_size(n);
            M {
                w,
                h,
                spine: w / 2.0,
                kind: MKind::Node,
            }
        }
        Flow::Seq(v) => {
            if v.is_empty() {
                return M {
                    w: 0.0,
                    h: EMPTY_H,
                    spine: 0.0,
                    kind: MKind::Seq(Vec::new()),
                };
            }
            let ms: Vec<M> = v.iter().map(measure).collect();
            let (w, spine) = align(ms.iter().map(|m| (m.w, m.spine)));
            let h = ms.iter().map(|m| m.h).sum::<f32>() + V_GAP * (ms.len() - 1) as f32;
            M {
                w,
                h,
                spine,
                kind: MKind::Seq(ms),
            }
        }
        Flow::Branch { cond, arms } => {
            let (cw, ch) = box_size(cond);
            let ms: Vec<M> = arms.iter().map(|a| measure(&a.body)).collect();
            let arms_w = ms.iter().map(|m| m.w).sum::<f32>() + H_GAP * (ms.len().max(1) - 1) as f32;
            let arms_h = ms.iter().map(|m| m.h).fold(0.0f32, f32::max);
            // The arms row is centred under the diamond, so both halves need
            // whichever of the two is wider.
            let half = (cw / 2.0).max(arms_w / 2.0);
            M {
                w: half * 2.0,
                h: ch + V_GAP + arms_h + V_GAP,
                spine: half,
                kind: MKind::Branch {
                    cond: (cw, ch),
                    arms: ms,
                },
            }
        }
        Flow::Loop { head, body, .. } => {
            let hm = head.node().map(box_size);
            let bm = measure(body);
            let (has_break, _) = jumps_in(body, 1);
            // The left lane always exists: it carries the back edge, and a
            // `continue` joins it. The right lane carries the way OUT — a
            // tested loop always has one, an endless loop only if it breaks.
            let left = GUTTER;
            let right = if hm.is_some() || has_break {
                GUTTER
            } else {
                0.0
            };
            let (inner_w, inner_spine) = align(
                hm.map(|(w, _)| (w, w / 2.0))
                    .into_iter()
                    .chain(std::iter::once((bm.w, bm.spine))),
            );
            let head_h = hm.map(|(_, h)| h + V_GAP).unwrap_or(LOOP_TOP);
            M {
                w: left + inner_w + right,
                h: head_h + bm.h + V_GAP,
                spine: left + inner_spine,
                kind: MKind::Loop {
                    head: hm,
                    body: Box::new(bm),
                    left,
                    right,
                },
            }
        }
    }
}

/// Does `f` contain a `break` / `continue` aimed at the loop `k` levels above?
fn jumps_in(f: &Flow, k: usize) -> (bool, bool) {
    match f {
        Flow::Node(_) => (false, false),
        Flow::Jump { target, .. } => match target {
            Target::Break(d) => (*d >= k, false),
            Target::Continue(d) => (false, *d >= k),
            Target::Return => (false, false),
        },
        Flow::Seq(v) => v.iter().fold((false, false), |acc, x| {
            let j = jumps_in(x, k);
            (acc.0 || j.0, acc.1 || j.1)
        }),
        Flow::Branch { arms, .. } => arms.iter().fold((false, false), |acc, a| {
            let j = jumps_in(&a.body, k);
            (acc.0 || j.0, acc.1 || j.1)
        }),
        Flow::Loop { body, .. } => jumps_in(body, k + 1),
    }
}

/// Does anything in `f` leave for the function's END (a `return`, or a `?`)?
fn exits_to_end(f: &Flow) -> bool {
    match f {
        Flow::Node(n) => n.try_exit,
        Flow::Jump { node, target } => matches!(target, Target::Return) || node.try_exit,
        Flow::Seq(v) => v.iter().any(exits_to_end),
        Flow::Branch { cond, arms } => cond.try_exit || arms.iter().any(|a| exits_to_end(&a.body)),
        Flow::Loop { head, body, .. } => {
            head.node().is_some_and(|n| n.try_exit) || exits_to_end(body)
        }
    }
}

// ── Place ────────────────────────────────────────────────────────────────────

/// How a jump leaves its box.
///
/// `band` is the clear horizontal strip to drop to before turning sideways (see
/// [`Placer::lanes`]). Without one — the jump is not inside any branch — the
/// arrow leaves through `side` directly, which is only safe because nothing
/// else sits at that height.
#[derive(Clone, Copy)]
struct Leave {
    side: (f32, f32),
    bottom: (f32, f32),
    band: Option<f32>,
}

/// One enclosing loop, as the `break` / `continue` router needs it.
struct Frame {
    /// Where a `continue` arrives — the loop's entry point.
    entry: (f32, f32),
    /// Where a `break` arrives — the loop's exit point.
    exit: (f32, f32),
    left_lane: f32,
    right_lane: f32,
    /// Length of [`Placer::lanes`] when this loop was entered, so a jump only
    /// considers the branches BETWEEN itself and the loop it is leaving.
    lane_base: usize,
}

struct Placer {
    out: FlowLayout,
    frames: Vec<Frame>,
    /// Join-line heights of the branches currently being placed, outermost
    /// first — the free horizontal bands a jump can travel along.
    ///
    /// A jump inside a branch arm cannot simply leave sideways: a sibling arm
    /// may sit right there, and the arrow would run straight through it. Every
    /// branch has one band that is guaranteed clear across its whole width —
    /// the gap between the bottom of its arms and whatever follows, where the
    /// arms rejoin the spine. Dropping to the OUTERMOST such band before
    /// turning sideways is what keeps the arrow off the boxes.
    lanes: Vec<f32>,
    /// Where a `return` / `?` arrives, and the lane it travels in. `None` on a
    /// chart with no END (an endless `main`), where such an edge cannot exist.
    end: Option<((f32, f32), f32)>,
}

impl Placer {
    fn edge(&mut self, pts: Vec<(f32, f32)>, label: &str, kind: EdgeKind, arrow: bool) {
        self.out.edges.push(Edge {
            pts,
            label: label.to_string(),
            kind,
            arrow,
        });
    }

    /// A straight run down the spine.
    fn spine_edge(&mut self, x: f32, y0: f32, y1: f32, label: &str, arrow: bool) {
        self.edge(vec![(x, y0), (x, y1)], label, EdgeKind::Flow, arrow);
    }

    fn place_box(&mut self, node: &FlowNode, spine: f32, y: f32, w: f32, h: f32) {
        self.out.boxes.push(Placed {
            node: node.clone(),
            x: spine - w / 2.0,
            y,
            w,
            h,
        });
    }

    /// Route a jump out of `from` to `to` through the vertical `lane`.
    fn route_out(&mut self, from: Leave, to: (f32, f32), lane: f32, label: &str, k: EdgeKind) {
        let mut pts = match from.band {
            Some(y) => vec![from.bottom, (from.bottom.0, y), (lane, y)],
            None => vec![from.side, (lane, from.side.1)],
        };
        pts.push((lane, to.1));
        pts.push(to);
        self.edge(pts, label, k, true);
    }

    /// The clear band to drop to before leaving the loop `d` levels out, or
    /// `None` when the jump is not inside a branch of that loop.
    fn band_for(&self, lane_base: usize) -> Option<f32> {
        self.lanes[lane_base.min(self.lanes.len())..]
            .iter()
            .copied()
            .fold(None, |acc: Option<f32>, y| {
                Some(acc.map_or(y, |a| a.max(y)))
            })
    }

    /// Place `f` (measured as `m`) with its spine at `spine` and its top at `y`.
    fn place(&mut self, f: &Flow, m: &M, spine: f32, y: f32) {
        match (f, &m.kind) {
            (Flow::Node(n), MKind::Node) => {
                self.place_box(n, spine, y, m.w, m.h);
                self.try_edge(n, spine, y, m.w, m.h);
            }
            (Flow::Jump { node, target }, MKind::Node) => {
                self.place_box(node, spine, y, m.w, m.h);
                self.jump_edge(*target, spine, y, m.w, m.h);
                self.try_edge(node, spine, y, m.w, m.h);
            }
            (Flow::Seq(items), MKind::Seq(ms)) => {
                if items.is_empty() {
                    // The missing `else`: a plain line down to the join.
                    self.spine_edge(spine, y, y + m.h, "", false);
                    return;
                }
                let mut cy = y;
                for (i, (child, cm)) in items.iter().zip(ms).enumerate() {
                    self.place(child, cm, spine, cy);
                    let bottom = cy + cm.h;
                    if i + 1 < items.len() {
                        if falls_through(child) {
                            self.spine_edge(spine, bottom, bottom + V_GAP, "", true);
                        }
                        cy = bottom + V_GAP;
                    }
                }
            }
            (
                Flow::Branch { cond, arms },
                MKind::Branch {
                    cond: (cw, ch),
                    arms: ms,
                },
            ) => {
                self.place_box(cond, spine, y, *cw, *ch);
                let arms_y = y + ch + V_GAP;
                let join_y = y + m.h;
                let arms_w =
                    ms.iter().map(|x| x.w).sum::<f32>() + H_GAP * (ms.len().max(1) - 1) as f32;
                let mut x = spine - arms_w / 2.0;
                // Anything jumping out of an arm drops to this band first.
                self.lanes.push(join_y - V_GAP * 0.35);
                for (arm, am) in arms.iter().zip(ms) {
                    let arm_spine = x + am.spine;
                    self.place(&arm.body, am, arm_spine, arms_y);
                    // Leave the diamond by whichever vertex faces this arm.
                    //
                    // A SIDE vertex only works when the arm is clear of the
                    // diamond's own width: an arm centred inside that width
                    // would be reached by running from the side vertex back
                    // ACROSS the diamond. Those leave from the bottom instead
                    // and jog sideways underneath it.
                    let half = cw / 2.0;
                    let mid = y + ch / 2.0;
                    let pts = if arm_spine <= spine - half + 0.5 {
                        vec![(spine - half, mid), (arm_spine, mid), (arm_spine, arms_y)]
                    } else if arm_spine >= spine + half - 0.5 {
                        vec![(spine + half, mid), (arm_spine, mid), (arm_spine, arms_y)]
                    } else if (arm_spine - spine).abs() < 0.5 {
                        vec![(spine, y + ch), (spine, arms_y)]
                    } else {
                        let jog = y + ch + V_GAP * 0.4;
                        vec![
                            (spine, y + ch),
                            (spine, jog),
                            (arm_spine, jog),
                            (arm_spine, arms_y),
                        ]
                    };
                    self.edge(pts, &arm.label, EdgeKind::Flow, true);
                    // Rejoin the spine below, unless this arm never gets there.
                    if falls_through(&arm.body) {
                        let bottom = arms_y + am.h;
                        let pts = if (arm_spine - spine).abs() < 0.5 {
                            vec![(spine, bottom), (spine, join_y)]
                        } else {
                            vec![(arm_spine, bottom), (arm_spine, join_y), (spine, join_y)]
                        };
                        self.edge(pts, "", EdgeKind::Flow, false);
                    }
                    x += am.w + H_GAP;
                }
                self.lanes.pop();
            }
            (
                Flow::Loop { head, body, .. },
                MKind::Loop {
                    head: hm,
                    body: bm,
                    left,
                    right,
                },
            ) => {
                let exit_y = y + m.h;
                let left_lane = spine - m.spine + left / 2.0;
                let right_lane = spine + (m.w - m.spine) - right / 2.0;
                let (into_body, out_of_loop) = head.labels();

                let (entry, body_top) = match (head.node(), hm) {
                    (Some(hn), Some((hw, hh))) => {
                        self.place_box(hn, spine, y, *hw, *hh);
                        let body_top = y + hh + V_GAP;
                        self.spine_edge(spine, y + hh, body_top, into_body, true);
                        // The way out leaves sideways and drops to the exit.
                        self.edge(
                            vec![
                                (spine + hw / 2.0, y + hh / 2.0),
                                (right_lane, y + hh / 2.0),
                                (right_lane, exit_y),
                                (spine, exit_y),
                            ],
                            out_of_loop,
                            EdgeKind::Flow,
                            false,
                        );
                        // A `continue` rejoins at the test, so that is the entry.
                        ((spine - hw / 2.0, y + hh / 2.0), body_top)
                    }
                    _ => {
                        // Endless loop: a short lane above the body is where the
                        // back edge comes home.
                        let body_top = y + LOOP_TOP;
                        self.spine_edge(spine, y, body_top, "", false);
                        ((spine, y + LOOP_TOP / 2.0), body_top)
                    }
                };

                self.frames.push(Frame {
                    entry,
                    exit: (spine, exit_y),
                    left_lane,
                    right_lane,
                    lane_base: self.lanes.len(),
                });
                self.place(body, bm, spine, body_top);
                self.frames.pop();

                // The back edge — only when the body can reach its own end.
                if falls_through(body) {
                    let bottom = body_top + bm.h;
                    let turn = bottom + V_GAP * 0.5;
                    self.edge(
                        vec![
                            (spine, bottom),
                            (spine, turn),
                            (left_lane, turn),
                            (left_lane, entry.1),
                            entry,
                        ],
                        "",
                        EdgeKind::Back,
                        true,
                    );
                }
            }
            // A measured tree always mirrors its flow tree; a mismatch would be
            // a bug in `measure`, and drawing nothing is better than panicking
            // inside a paint pass.
            _ => {}
        }
    }

    /// The `Err` path of a `?`, from the box out to the function's end.
    fn try_edge(&mut self, n: &FlowNode, spine: f32, y: f32, w: f32, h: f32) {
        if !n.try_exit {
            return;
        }
        let Some((end, lane)) = self.end else { return };
        let from = Leave {
            side: (spine + w / 2.0, y + h / 2.0),
            bottom: (spine, y + h),
            band: self.band_for(0),
        };
        self.route_out(from, end, lane, "Err", EdgeKind::Try);
    }

    fn jump_edge(&mut self, target: Target, spine: f32, y: f32, w: f32, h: f32) {
        let mid = y + h / 2.0;
        let bottom = (spine, y + h);
        match target {
            Target::Break(d) => {
                let Some(fr) = frame_at(&self.frames, d) else {
                    return;
                };
                let (exit, lane, base) = (fr.exit, fr.right_lane, fr.lane_base);
                let from = Leave {
                    side: (spine + w / 2.0, mid),
                    bottom,
                    band: self.band_for(base),
                };
                self.route_out(from, exit, lane, "", EdgeKind::Break);
            }
            Target::Continue(d) => {
                let Some(fr) = frame_at(&self.frames, d) else {
                    return;
                };
                let (entry, lane, base) = (fr.entry, fr.left_lane, fr.lane_base);
                let from = Leave {
                    side: (spine - w / 2.0, mid),
                    bottom,
                    band: self.band_for(base),
                };
                self.route_out(from, entry, lane, "", EdgeKind::Continue);
            }
            Target::Return => {
                let Some((end, lane)) = self.end else { return };
                let from = Leave {
                    side: (spine + w / 2.0, mid),
                    bottom,
                    band: self.band_for(0),
                };
                self.route_out(from, end, lane, "", EdgeKind::Return);
            }
        }
    }
}

/// The loop `d` levels out (1 = innermost). `None` when the code names a loop
/// that is not there — which real code cannot do, but a partial parse can.
fn frame_at(frames: &[Frame], d: usize) -> Option<&Frame> {
    frames.len().checked_sub(d).and_then(|i| frames.get(i))
}

// ── Entry point ──────────────────────────────────────────────────────────────

/// Lay `chart` out on a virtual canvas whose top-left is `(0, 0)`.
pub fn layout(chart: &Chart) -> FlowLayout {
    let m = measure(&chart.body);
    let start = terminal(&chart.name, chart.line);
    let end = terminal("END", chart.line);
    let (sw, sh) = box_size(&start);
    let (ew, eh) = box_size(&end);
    let needs_end = !chart.diverges;

    let mut left = m.spine.max(sw / 2.0);
    let mut right = (m.w - m.spine).max(sw / 2.0);
    if needs_end {
        left = left.max(ew / 2.0);
        right = right.max(ew / 2.0);
    }
    // A `return` or a `?` needs a lane of its own down the right-hand side.
    let ret_lane = needs_end && exits_to_end(&chart.body);
    if ret_lane {
        right += GUTTER;
    }

    let spine = MARGIN + left;
    let width = MARGIN * 2.0 + left + right;
    let body_top = MARGIN + sh + V_GAP;
    let body_bottom = body_top + m.h;
    let end_top = body_bottom + V_GAP;
    let height = if needs_end {
        end_top + eh + MARGIN
    } else {
        body_bottom + MARGIN
    };

    let mut p = Placer {
        out: FlowLayout {
            width,
            height,
            ..Default::default()
        },
        frames: Vec::new(),
        lanes: Vec::new(),
        end: if needs_end {
            Some((
                (spine + ew / 2.0, end_top + eh / 2.0),
                width - MARGIN - GUTTER / 2.0,
            ))
        } else {
            None
        },
    };

    p.place_box(&start, spine, MARGIN, sw, sh);
    p.spine_edge(spine, MARGIN + sh, body_top, "", true);
    p.place(&chart.body, &m, spine, body_top);
    if needs_end {
        // Only draw the last leg when control can actually walk down it; a body
        // whose every path jumps away reaches END through those arrows instead.
        if falls_through(&chart.body) {
            p.spine_edge(spine, body_bottom, end_top, "", true);
        }
        p.place_box(&end, spine, end_top, ew, eh);
    }
    p.out
}

fn terminal(text: &str, line: usize) -> FlowNode {
    FlowNode {
        text: text.to_string(),
        detail: Vec::new(),
        hidden: 0,
        shape: Shape::Terminal,
        line,
        awaits: false,
        try_exit: false,
        goto_line: None,
        goto_key: None,
        decl: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::flow_map::parse::charts_of;

    fn lay(src: &str) -> FlowLayout {
        let c = charts_of(src).expect("parses").pop().expect("a chart");
        layout(&c)
    }

    fn texts(l: &FlowLayout) -> Vec<String> {
        l.boxes.iter().map(|b| b.node.text.clone()).collect()
    }

    fn find(l: &FlowLayout, text: &str) -> Placed {
        l.boxes
            .iter()
            .find(|b| b.node.text.contains(text))
            .unwrap_or_else(|| panic!("no box containing {text:?} in {:?}", texts(l)))
            .clone()
    }

    /// Every shape, so a new one cannot quietly skip the tests below. The
    /// exhaustive `match` in [`text_insets`] is the other half of that guard.
    const ALL_SHAPES: [Shape; 7] = [
        Shape::Terminal,
        Shape::Process,
        Shape::Io,
        Shape::Decision,
        Shape::Subroutine,
        Shape::Generated,
        Shape::Decl,
    ];

    fn node(shape: Shape, text: &str) -> FlowNode {
        let mut n = terminal(text, 1);
        n.shape = shape;
        n
    }

    /// The forward and the inverse are the same arithmetic read in two
    /// directions, so a box sized for its text has room for EXACTLY that text.
    ///
    /// This is the drift the whole change exists to stop: `box_size` adds a
    /// per-shape reserve, `text_insets` takes it back, and until they were one
    /// function nobody could notice when only one of them moved. Thirty
    /// characters is long enough to clear every shape's minimum width and short
    /// enough to stay under `MAX_W`, so no clamp hides the comparison.
    #[test]
    fn every_shape_gives_back_exactly_the_room_it_reserved() {
        let text = "x".repeat(30);
        for shape in ALL_SHAPES {
            let (w, _) = box_size(&node(shape, &text));
            let (il, ir) = text_insets(shape, 0.5);
            assert!(
                (w - (30.0 * CHAR_W + il + ir)).abs() < 0.01,
                "{shape:?}: sized to {w}, but its own insets ({il}, {ir}) say {}",
                30.0 * CHAR_W + il + ir
            );
            let room = text_room(shape, w, 0.5);
            assert!(
                (room - 30.0 * CHAR_W).abs() < 0.01,
                "{shape:?}: sized to {w} but offers {room} for text that needs {}",
                30.0 * CHAR_W
            );
        }
    }

    /// No shape may be given a reserve so thin its text touches its outline.
    #[test]
    fn no_shape_lets_its_text_touch_the_outline() {
        for shape in ALL_SHAPES {
            for t in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let (l, r) = text_insets(shape, t);
                assert!(
                    l >= MIN_TEXT_PAD && r >= MIN_TEXT_PAD,
                    "{shape:?} at t={t}: ({l}, {r}) is under the {MIN_TEXT_PAD} floor"
                );
            }
        }
    }

    /// The parallelogram is a shear: every row has the same room, and it is the
    /// CENTRE that slides. Reading it as a taper would make a centred row
    /// under-use one side and cross the slant on the other.
    #[test]
    fn the_parallelogram_keeps_its_width_and_slides_its_centre() {
        let rooms: Vec<f32> = [0.0, 0.5, 1.0]
            .iter()
            .map(|&t| text_room(Shape::Io, 200.0, t))
            .collect();
        assert!(
            (rooms[0] - rooms[1]).abs() < 0.01 && (rooms[1] - rooms[2]).abs() < 0.01,
            "the slant changed the usable width: {rooms:?}"
        );
        let (top_l, _) = text_insets(Shape::Io, 0.0);
        let (bot_l, _) = text_insets(Shape::Io, 1.0);
        assert!(
            (top_l - bot_l - SLANT).abs() < 0.01,
            "the top row should start a full slant right of the bottom one: {top_l} vs {bot_l}"
        );
    }

    /// `draw_box` paints a "+N more" row; `box_size` used to measure only the
    /// label and the folded statements, so a box whose tail row was the widest
    /// thing in it was sized for a row it does not draw.
    #[test]
    fn a_folded_box_is_sized_for_the_tail_row_it_draws() {
        let mut n = node(Shape::Process, "a");
        n.hidden = 99_999_999;
        let (w, _) = box_size(&n);
        let room = text_room(Shape::Process, w, 0.5);
        let tail = n.rows().pop().expect("a tail row");
        assert_eq!(tail, "+99999999 more");
        assert!(
            room >= tail.chars().count() as f32 * CHAR_W,
            "sized to {w} ({room} of room) for a {} unit tail row",
            tail.chars().count() as f32 * CHAR_W
        );
    }

    /// A card indents everything under its header, and those two spaces are as
    /// wide as any other two characters.
    #[test]
    fn a_card_is_sized_for_the_indent_it_draws() {
        let mut n = node(Shape::Decl, "Log");
        n.detail = vec!["x".repeat(30)];
        let (w, _) = box_size(&n);
        let room = text_room(Shape::Decl, w, 0.5);
        assert!(
            room >= (30 + CARD_ROW_INDENT) as f32 * CHAR_W,
            "sized to {w} ({room} of room) for a row that draws {} units",
            (30 + CARD_ROW_INDENT) as f32 * CHAR_W
        );
    }

    /// The row list and the row count are two readings of one thing.
    #[test]
    fn the_rows_a_box_draws_are_the_lines_it_counts() {
        let mut n = node(Shape::Process, "head");
        n.detail = vec!["one".into(), "two".into()];
        assert_eq!(n.rows().len(), n.lines());
        n.hidden = 3;
        assert_eq!(n.rows().len(), n.lines());
        assert_eq!(n.rows()[0], "head");
    }

    /// Nothing may be placed outside the canvas the GUI is told to scale — an
    /// off-canvas box is invisible at the auto-fit scale.
    fn assert_inside(l: &FlowLayout) {
        for b in &l.boxes {
            assert!(
                b.x >= 0.0
                    && b.y >= 0.0
                    && b.x + b.w <= l.width + 0.01
                    && b.y + b.h <= l.height + 0.01,
                "box {:?} at ({}, {}) {}x{} escapes the {}x{} canvas",
                b.node.text,
                b.x,
                b.y,
                b.w,
                b.h,
                l.width,
                l.height
            );
        }
        for e in &l.edges {
            for &(x, y) in &e.pts {
                assert!(
                    x >= -0.01 && y >= -0.01 && x <= l.width + 0.01 && y <= l.height + 0.01,
                    "edge point ({x}, {y}) escapes the {}x{} canvas",
                    l.width,
                    l.height
                );
            }
        }
    }

    #[test]
    fn a_straight_function_is_start_body_end() {
        let l = lay("fn f() { let a = 1; }");
        assert_eq!(texts(&l), ["f", "let a = 1;", "END"]);
        let (start, body, end) = (find(&l, "f"), find(&l, "let a"), find(&l, "END"));
        assert!(start.y < body.y && body.y < end.y, "top to bottom");
        assert_inside(&l);
    }

    /// The firmware case: an endless loop means no END box exists at all, so
    /// the chart cannot claim the program finishes.
    #[test]
    fn an_endless_main_has_no_end_box() {
        let l = lay("fn main() { setup(); loop { tick(); } }");
        assert!(
            !texts(&l).contains(&"END".to_string()),
            "got {:?}",
            texts(&l)
        );
        assert_inside(&l);
    }

    /// ...and its back edge really does return to the top of the loop.
    #[test]
    fn an_endless_loop_gets_a_back_edge_that_climbs() {
        let l = lay("fn main() { loop { tick(); } }");
        let back: Vec<&Edge> = l
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Back)
            .collect();
        assert_eq!(back.len(), 1, "exactly one back edge");
        let e = back[0];
        let first = e.pts.first().unwrap();
        let last = e.pts.last().unwrap();
        assert!(last.1 < first.1, "the back edge must go UP");
        let lane = e.pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        let body = find(&l, "tick");
        assert!(
            lane < body.x,
            "the back edge must travel LEFT of the body ({lane} vs {})",
            body.x
        );
        assert_inside(&l);
    }

    /// A `break` is what turns an endless loop into something with an END, and
    /// its arrow has to reach the loop's exit — down the RIGHT lane, so it
    /// never crosses the back edge.
    #[test]
    fn a_break_leaves_on_the_right_and_the_lane_is_reserved_for_it() {
        let l = lay("fn main() { loop { if done() { break; } tick(); } }");
        let e = l
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Break)
            .expect("a break edge");
        let brk = find(&l, "break");
        let lane = e.pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        assert!(lane > brk.x + brk.w, "the break lane is right of the box");
        assert!(
            e.pts.last().unwrap().1 > brk.y,
            "and it lands below, at the loop's exit"
        );
        assert_inside(&l);
    }

    /// The two arms of an `if` sit side by side and rejoin on the spine.
    #[test]
    fn an_if_puts_its_arms_side_by_side_under_the_diamond() {
        let l = lay("fn f() { if c { yes(); } else { no(); } }");
        let (d, y, n) = (find(&l, "c"), find(&l, "yes"), find(&l, "no"));
        assert!(y.y > d.y && n.y > d.y, "both arms are below the diamond");
        assert!(y.x < n.x, "YES on the left, NO on the right");
        let dc = d.center().0;
        assert!(
            y.center().0 < dc && n.center().0 > dc,
            "the arms straddle the spine"
        );
        assert_inside(&l);
    }

    /// An `if` with no `else` still needs the line that goes straight past it,
    /// or the NO case would look like a dead end.
    #[test]
    fn an_if_without_else_still_draws_the_no_path() {
        let l = lay("fn f() { if c { yes(); } done(); }");
        let labels: Vec<&str> = l.edges.iter().map(|e| e.label.as_str()).collect();
        assert!(labels.contains(&"NO"), "got {labels:?}");
        assert_inside(&l);
    }

    #[test]
    fn a_match_gets_one_column_per_arm() {
        let l = lay("fn f() { match m { A => a(), B => b(), C => c() } }");
        let (a, b, c) = (find(&l, "a()"), find(&l, "b()"), find(&l, "c()"));
        assert!(a.x < b.x && b.x < c.x, "three columns, left to right");
        assert!(
            (a.y - b.y).abs() < 0.01 && (b.y - c.y).abs() < 0.01,
            "all arms start at the same height"
        );
        assert_inside(&l);
    }

    /// The property the reserved lanes exist FOR: an arrow never runs through a
    /// box. Comparing two widths does not test this — a loop holding a `break`
    /// is wider anyway, because it holds a diamond too, so that comparison
    /// passes even with the reservation deleted. This one does not.
    fn assert_no_edge_crosses_a_box(l: &FlowLayout) {
        // Inset, because every edge legitimately starts and ends ON a boundary.
        const IN: f32 = 4.0;
        for e in &l.edges {
            for w in e.pts.windows(2) {
                for b in &l.boxes {
                    let hit = crate::panels::structure_map::layout::seg_hits_rect(
                        (w[0].0, w[0].1),
                        (w[1].0, w[1].1),
                        b.x + IN,
                        b.y + IN,
                        (b.w - 2.0 * IN).max(0.0),
                        (b.h - 2.0 * IN).max(0.0),
                    );
                    assert!(
                        !hit,
                        "a {:?} edge runs through the box {:?}: {:?} -> {:?}",
                        e.kind, b.node.text, w[0], w[1]
                    );
                }
            }
        }
    }

    #[test]
    fn no_edge_crosses_a_box_in_a_plain_loop() {
        assert_no_edge_crosses_a_box(&lay("fn main() { loop { tick(); if d() { break; } } }"));
    }

    /// The case that exposed the routing: a `break` in the LEFT arm with a wide
    /// sibling arm beside it. Leaving the box sideways would drive the arrow
    /// straight through that sibling; the fix is to drop to the branch's join
    /// band first, which is clear across the whole branch.
    #[test]
    fn a_break_in_one_arm_does_not_cross_its_sibling() {
        let l = lay(
            "fn main() { loop { match st { A => { break; }              B => { a_very_long_call_that_makes_this_column_wide(); } } } }",
        );
        assert_no_edge_crosses_a_box(&l);
        let e = l.edges.iter().find(|e| e.kind == EdgeKind::Break).unwrap();
        let sibling = find(&l, "a_very_long_call");
        let brk = find(&l, "break");
        assert!(
            e.pts.iter().any(|p| p.1 > sibling.y + sibling.h),
            "the break must descend past the sibling before turning: {:?}",
            e.pts
        );
        assert!(brk.x < sibling.x, "and it really is the left arm");
    }

    /// The lane is not merely "outside the boxes", it is CLEAR of them.
    ///
    /// Deleting the reservation still keeps every arrow OUT of every box — it
    /// just runs flush against their right edges. That is why the crossing
    /// guard above cannot see the regression and this measurement has to exist
    /// on its own.
    #[test]
    fn the_break_lane_keeps_its_distance_from_the_loop_body() {
        let l = lay("fn main() { loop { a_reasonably_wide_statement_here(); if d() { break; } } }");
        let e = l.edges.iter().find(|e| e.kind == EdgeKind::Break).unwrap();
        let lane = e.pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
        let widest = l
            .boxes
            .iter()
            .filter(|b| b.node.shape != Shape::Terminal)
            .map(|b| b.x + b.w)
            .fold(f32::MIN, f32::max);
        assert!(
            lane > widest + 8.0,
            "the break lane at {lane} hugs the body, whose right edge is {widest}"
        );
    }

    #[test]
    fn no_edge_crosses_a_box_when_a_return_leaves_a_branch() {
        assert_no_edge_crosses_a_box(&lay(
            "fn f() { if c { return; } else { wide_enough_to_sit_beside_it(); } tail(); }",
        ));
    }

    #[test]
    fn no_edge_crosses_a_box_in_the_deep_nest() {
        let src = "fn main() {
            'outer: loop {
                for x in it {
                    while go() {
                        if a { break 'outer; } else if b { continue; }
                        match x { A => { work(); } _ => {} }
                    }
                }
            }
        }";
        assert_no_edge_crosses_a_box(&lay(src));
    }

    /// Same for the function-level lane a `return` travels in.
    #[test]
    fn a_return_reserves_its_own_lane_and_reaches_the_end() {
        let l = lay("fn f() { if c { return; } more(); }");
        let e = l
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Return)
            .expect("a return edge");
        let end = find(&l, "END");
        let last = *e.pts.last().unwrap();
        assert!(
            (last.0 - (end.x + end.w)).abs() < 1.0 && last.1 > end.y,
            "the return arrow lands on END's right edge, got {last:?}"
        );
        assert_inside(&l);
    }

    /// A `?` is an error path to the same END, drawn without turning every
    /// statement into a diamond.
    #[test]
    fn a_question_mark_draws_an_err_path_to_the_end() {
        let l = lay("fn f() -> R { let v = thing()?; use_it(v); Ok(()) }");
        let e = l
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Try)
            .expect("a try edge");
        assert_eq!(e.label, "Err");
        assert_inside(&l);
    }

    /// An endless function has no END, so a `?` has nowhere to go — and must
    /// not be drawn heading off the canvas.
    #[test]
    fn a_try_edge_is_dropped_when_there_is_no_end() {
        let l = lay("fn main() -> ! { loop { let v = thing()?; use_it(v); } }");
        assert!(!l.edges.iter().any(|e| e.kind == EdgeKind::Try));
        assert_inside(&l);
    }

    /// A `continue` goes back up the SAME lane the back edge uses — one lane,
    /// one meaning, and it must be left of the body.
    #[test]
    fn a_continue_climbs_the_left_lane() {
        let l = lay("fn main() { loop { if skip() { continue; } work(); } }");
        let e = l
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Continue)
            .expect("a continue edge");
        let work = find(&l, "work");
        let lane = e.pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
        assert!(lane < work.x, "the continue lane is left of the body");
        assert!(
            e.pts.last().unwrap().1 < find(&l, "continue").y,
            "and it climbs"
        );
        assert_inside(&l);
    }

    /// Nesting must not push anything off the canvas — the case a hand-rolled
    /// gutter scheme gets wrong.
    #[test]
    fn deep_nesting_stays_on_the_canvas() {
        let src = "fn main() {
            'outer: loop {
                for x in it {
                    while go() {
                        if a { break 'outer; } else if b { continue; }
                        match x { A => { work(); } _ => {} }
                    }
                }
            }
        }";
        let l = lay(src);
        assert_inside(&l);
        assert!(l.boxes.len() > 6, "the whole nest is drawn");
    }

    /// A tested loop always has a way out, so it always reserves the exit lane.
    #[test]
    fn a_while_loop_draws_a_labelled_way_out() {
        let l = lay("fn f() { while go() { step(); } }");
        let labels: Vec<&str> = l.edges.iter().map(|e| e.label.as_str()).collect();
        assert!(
            labels.contains(&"YES") && labels.contains(&"NO"),
            "got {labels:?}"
        );
        assert_inside(&l);
    }

    #[test]
    fn a_for_loop_is_labelled_each_and_done() {
        let l = lay("fn f() { for b in buf { feed(b); } }");
        let labels: Vec<&str> = l.edges.iter().map(|e| e.label.as_str()).collect();
        assert!(
            labels.contains(&"each") && labels.contains(&"done"),
            "got {labels:?}"
        );
    }

    /// The reference shape: a decision whose YES climbs back to an earlier box
    /// and whose NO falls to END.
    #[test]
    fn the_textbook_counting_loop_comes_out_whole() {
        let l =
            lay("fn main() { let mut x = 0; loop { x += 1; print(x); if x >= 20 { break; } } }");
        assert_inside(&l);
        let end = find(&l, "END");
        let cond = find(&l, "x >= 20");
        assert!(cond.y < end.y, "the test sits above END");
        assert!(l.edges.iter().any(|e| e.kind == EdgeKind::Back));
        assert!(l.edges.iter().any(|e| e.kind == EdgeKind::Break));
    }
}
