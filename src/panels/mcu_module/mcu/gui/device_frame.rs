//! The device mat: a tinted ground under the parts of one device, with a small
//! named tab.
//!
//! A device (see [`PinGroup`](crate::panels::mcu_module::mcu_config::PinGroup))
//! already wears three marks — a bar under a grouped module box's title, a bar
//! over a grouped io field's name, a tick at a grouped pad's stub. Each of them
//! says "this one thing belongs to something". None of them says WHICH things
//! belong to EACH OTHER, which is the whole reason a device exists: a radar is
//! a UART pair and one interrupt line, and a reader holding the datasheet had
//! to match three identical bars by colour to find that out.
//!
//! The mat says it with AREA. That is deliberate: every mark already on this
//! canvas is a stroke, a text colour or a 3 px bar, so area is the one channel
//! nothing else spends. It also survives the Scene's zoom-out, where a 1 px
//! dashed outline would go sub-pixel and the device would vanish rather than
//! fade.
//!
//! ## It may never lie
//!
//! A frame that appears to contain a pad the device does not own is worse than
//! no frame, so the merge rule has two halves and only the second is about
//! truth:
//!
//! * `JOIN` decides EAGERNESS — how close two parts must be to be drawn on one
//!   mat;
//! * a merge is REFUSED when the resulting hull would reach something the device
//!   does not own — another device's box, an ungrouped field, any pad in the row
//!   — either by newly touching it, or by ending up holding it whole. See
//!   [`refuses`] for why those are two rules and not one.
//!
//! The guard subsumes the threshold: a device holding pads 3 and 5 cannot
//! swallow pad 4 whatever `JOIN` says. It is also what keeps a device with
//! parts on opposite sides of the chip from drawing one hull across the whole
//! package — the chip's own pin rows are in the way, so the merge is refused
//! and the device draws two mats, numbered `radar 1/2` and `radar 2/2`.
//!
//! ## It is not a widget
//!
//! [`frames`] takes a `&Painter` and never a `&mut Ui`. That is a compile-time
//! proof rather than a review rule: the pass registers no `Sense`, mints no id
//! and grows no `min_rect`, so clicking empty canvas still clears the selection
//! even inside a mat, and the Scene's drag-pan still works over one.

use super::chip;
use super::geometry::{self, PinPlace};
use super::modules::group_color;
use super::rotate::Rot;
use crate::panels::mcu_module::mcu::model::Mcu;
use eframe::egui;

/// Two footprints merge when the gap between them is at most this.
///
/// It decides EAGERNESS, never truth — the containment guard in [`cluster`]
/// decides truth. The band it has to sit in comes from constants this module
/// does not own, and `the_join_threshold_stays_inside_its_band` pins it:
///
/// * `>= BOX_GAP` (14), or two boxes packed side by side would not share a mat;
/// * `< 2*(PIN_WIDTH + PIN_SPACING) - TIP` (40), or a device holding pads 3 and
///   5 would be eager to reach across pad 4 — the guard would still refuse it,
///   but a threshold that has to be rescued is a threshold set wrong.
pub(super) const JOIN: f32 = 24.0;

/// The tinted rim a mat adds around its outermost part.
///
/// Seven, not four: at four the mat read as a tight outline traced on the parts
/// rather than as ground they sit on, and a box's own border sat almost on the
/// mat's hairline. Two devices on ADJACENT pads now have rims that overlap by a
/// few pixels — a rim is not a claim, and `cluster` still measures the
/// UN-expanded rects, so what the mats say about membership is unchanged.
const PAD: f32 = 7.0;

/// Side of the square a bare pad contributes, centred on its stub TIP.
///
/// A square on the TIP and not the stub's rotated bounding box: `Rot::quad`
/// inflates a 50×30 stub to roughly 57×57 on a diamond, which fuses a whole
/// face of the package into one blob. A tip square is the same size and shape
/// at 0°, 90° and 45°, which is why nothing below needs a rotation branch.
///
/// The square is the same in every mode; the PITCH is not. On the 45° diamond a
/// pin row runs diagonally, so two adjacent tips are `pitch / √2` ≈ 23.3 apart
/// on each AXIS — and `gap` and the union are both axis-aligned. At 26 the
/// squares of two neighbouring pads already overlapped, their union grew to
/// reach the pad beyond, the guard refused the merge, and a device on two
/// adjacent pads drew itself as `radar 1/2` + `radar 2/2` the moment the chip
/// was rotated. Staying under the diagonal pitch is what keeps that from
/// needing a rotation branch. See
/// `the_tip_square_fits_between_two_pads_in_every_rotation`.
const TIP: f32 = 22.0;

/// Tall enough for [`TAB_PT`] plus a little air above and below.
const TAB_H: f32 = 18.0;
/// Text inset inside the tab.
const TAB_PAD_X: f32 = 5.0;
/// The tab's type size. Half again the 8.5 the rest of the diagram's small text
/// uses: the tab names the whole assembly, so it outranks the signal names and
/// pin numbers around it rather than joining them.
const TAB_PT: f32 = 12.75;

/// A third corner radius: the chip body is 4, a module box 6.
const R_MAT: f32 = 8.0;
const R_TAB: f32 = 3.0;

/// The mat's fill and hairline alphas. `FILL_A` is the one number to tune by
/// eye — high enough to read as a ground, low enough that the wires crossing it
/// stay legible.
const FILL_A: u8 = 30;
const EDGE_A: u8 = 95;
/// The same fill and hairline when the canvas is pointing AT this device.
///
/// One step below the white 2.8 px a selected box wears: a mat is far larger
/// than a box, so matching that weight would make the device shout over the
/// thing inside it that the user actually clicked.
const FILL_A_SEL: u8 = 52;
const EDGE_A_SEL: u8 = 200;
const EDGE_W_SEL: f32 = 2.4;
/// Dark text on the light desaturated tab.
const TAB_FG: egui::Color32 = egui::Color32::from_rgb(0x21, 0x25, 0x2b);

/// What a mat adds beyond its outermost member — rim, tab and hairline.
///
/// Painted shapes never grow `ui.min_rect()`, so the Scene's auto-fit cannot
/// discover this on its own: the painter has to be told, in `gui::mod`.
pub const HALO: f32 = PAD + TAB_H + 1.0;

/// How far a mat on a BARE pad reaches past the stub tip.
pub const BARE_REACH: f32 = TIP / 2.0 + PAD;

/// A drawn thing that stands in for one or more pads: a module box, or an io
/// field's full footprint.
///
/// Collected by the two paint passes that already compute these rects, so a mat
/// is drawn around what was actually painted rather than around a second
/// formula that could drift from it.
pub struct Member {
    /// The device holding it, `None` when it is ungrouped.
    pub group: Option<String>,
    pub rect: egui::Rect,
    /// The pads this thing already speaks for.
    ///
    /// A box covers every pad it wires — INCLUDING one another device holds.
    /// `group_of_module` answers with the group of the first pad it finds, so
    /// without this the second device would draw a competing mat over the same
    /// box.
    pub covers: Vec<usize>,
}

/// Where one device's tab was drawn, so the next frame can put a widget there.
///
/// An OFFSET from the chip centre, never an absolute rect. `Mcu::show` grows the
/// allocated painter to cover parts dragged far from the chip, which moves the
/// canvas centre — a rect stashed in screen coordinates would drift by half that
/// growth exactly while a drag is in flight. Both existing drag stores are
/// centre-relative for the same reason.
#[derive(Clone, Debug, PartialEq)]
pub struct TabHandle {
    /// The device's TRIMMED name — the key everything else uses.
    pub name: String,
    /// Which of that device's mats this is, so a device drawn in two pieces
    /// gets two distinct widget ids.
    pub cluster: usize,
    pub off: egui::Rect,
}

/// One edge pad's tip square.
///
/// Two jobs, and the second is the important one: it is the fallback mark for a
/// pad nothing else draws, AND it is the obstacle that stops another device's
/// mat reaching across the pin row.
pub struct Pad {
    pub group: Option<String>,
    pub num: usize,
    pub rect: egui::Rect,
}

/// Every EDGE pad's tip square, grouped or not, with rotation already applied.
///
/// Balls are left out on purpose: a ball sits INSIDE the body and the mat is
/// painted under the body, so its mat would be invisible. The 2.5 px dot from
/// `chip::draw_group_tick` stays a ball's only device mark.
pub fn pad_footprints(mcu: &Mcu, local_chip: egui::Rect, rot: Rot) -> Vec<Pad> {
    geometry::pin_geometry(mcu, local_chip)
        .filter(|g| matches!(g.place, PinPlace::Edge(_)))
        .map(|g| Pad {
            group: mcu.group_of_pin(g.pin.number).map(|x| x.name.clone()),
            num: g.pin.number,
            rect: egui::Rect::from_center_size(rot.apply(g.anchor()), egui::Vec2::splat(TIP)),
        })
        .collect()
}

/// The gap between two rectangles — zero when they touch or overlap.
fn gap(a: egui::Rect, b: egui::Rect) -> f32 {
    let dx = (b.left() - a.right()).max(a.left() - b.right()).max(0.0);
    let dy = (b.top() - a.bottom()).max(a.top() - b.bottom()).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// Split every footprint on the canvas into what device `name` owns and what it
/// must not touch.
///
/// The asymmetry between the two is the whole correctness argument:
///
/// * `spoken` (every pad any member already draws) suppresses DOUBLE
///   representation inside the device — a UART box plus a lone input field
///   yields exactly two footprints and zero pad squares, so the mat never has
///   to reach into the pin row at all;
/// * but a pad a FOREIGN box speaks for is still an obstacle, because that box
///   is packed far outside the row and would not stop a mat crossing the pad's
///   own stub.
pub(crate) fn split(
    name: &str,
    members: &[Member],
    pads: &[Pad],
    spoken: &std::collections::HashSet<usize>,
) -> (Vec<egui::Rect>, Vec<egui::Rect>) {
    let mut mine = Vec::new();
    let mut foreign = Vec::new();
    for m in members {
        if m.group.as_deref().map(str::trim) == Some(name) {
            mine.push(m.rect);
        } else {
            foreign.push(m.rect);
        }
    }
    for p in pads {
        if p.group.as_deref().map(str::trim) == Some(name) {
            if !spoken.contains(&p.num) {
                mine.push(p.rect);
            }
        } else {
            foreign.push(p.rect);
        }
    }
    (mine, foreign)
}

/// How a candidate merge is ranked: nearest first, then the tightest union,
/// then the union's own coordinates.
///
/// Every field after the first is a TIE-BREAK, and every one of them is needed.
/// Ranking on distance alone leaves ties — two parts equidistant from a third is
/// an ordinary arrangement on a pin row — and a tie resolved by scan order makes
/// the whole pass depend on the order `mine` happened to arrive in. Ending on
/// the union's coordinates makes the ranking total for any two DISTINCT
/// candidates, so the result is the same set whatever order the inputs came in.
fn merge_rank(d: f32, u: egui::Rect) -> [f32; 6] {
    [
        d,
        u.width() * u.height(),
        u.left(),
        u.top(),
        u.right(),
        u.bottom(),
    ]
}

fn ranks_before(a: &[f32; 6], b: &[f32; 6]) -> bool {
    for (x, y) in a.iter().zip(b.iter()) {
        match x.total_cmp(y) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    false
}

/// Whether foreign footprint `f` refuses the merge of `a` and `b` into `u`.
///
/// Two rules, and they answer two different questions.
///
/// * NEWLY CLAIMED. A footprint one of the two parts already overlaps is not
///   something this merge took, so it cannot refuse it. Without that exemption
///   an io field — taller than the pin pitch, at 18 px of box plus its name, its
///   bar and an IRQ strip against a 33 px pitch — overlaps its neighbour before
///   anything is merged, and every merge that field could ever take part in was
///   refused forever.
/// * CONTAINMENT, always. "Already overlapping" is not a licence to grow without
///   limit over the same footprint: a chain of merges, each individually
///   claiming nothing new, could end with a hull that holds a foreign part
///   ENTIRELY. That is precisely the lie this module exists to prevent, so it is
///   refused whatever the parts already overlapped.
fn refuses(f: egui::Rect, a: egui::Rect, b: egui::Rect, u: egui::Rect) -> bool {
    let swallowed = u.contains(f.min) && u.contains(f.max);
    let newly = u.intersects(f) && !a.intersects(f) && !b.intersects(f);
    swallowed || newly
}

/// Merge `mine` into as few hulls as the truth allows.
///
/// Best admissible pair first (see [`merge_rank`]), repeat until nothing merges.
/// Ranked and not first-fit because the input order is arbitrary — boxes arrive
/// in side-packing order, fields in `iter_all_pins` order — and a mat that
/// changes shape when a pin is renamed is a bug nobody would ever find.
///
/// Two rules, and only the second is about truth:
///
/// * `JOIN` is the EAGERNESS. It measures the GAP between two parts, which is a
///   distance along one direction;
/// * the guard is the TRUTH. It tests the UNION, which is an area. The two come
///   apart on a diagonal pair: two parts 20 px apart across a corner have a
///   union hundreds of pixels wide, and everything inside it would be claimed.
///   That is the case `JOIN` structurally cannot see, and the one the guard is
///   for.
///
/// The candidate union is tested UN-expanded: the rim [`PAD`] adds afterwards
/// may graze a neighbour, and a rim is not a claim. Containment is the lie, and
/// containment is what is refused.
pub(crate) fn cluster(mine: &[egui::Rect], foreign: &[egui::Rect]) -> Vec<egui::Rect> {
    let mut hulls: Vec<egui::Rect> = mine.to_vec();
    loop {
        let mut best: Option<([f32; 6], usize, usize)> = None;
        for i in 0..hulls.len() {
            for j in (i + 1)..hulls.len() {
                let d = gap(hulls[i], hulls[j]);
                if d > JOIN {
                    continue;
                }
                let u = hulls[i].union(hulls[j]);
                // Only territory the merge NEWLY claims can refuse it. A
                // footprint one of the two parts ALREADY overlaps is not
                // something the merge took: an io field is taller than the pin
                // pitch (18 px of box plus the name, the bar and an IRQ strip,
                // against a 33 px pitch), so a field vertically next to a
                // foreign one overlaps it before any merging happens - and
                // testing the bare union refused every merge that part could
                // ever be in, forever.
                if foreign.iter().any(|f| refuses(*f, hulls[i], hulls[j], u)) {
                    continue;
                }
                let rank = merge_rank(d, u);
                if best.as_ref().is_none_or(|(b, _, _)| ranks_before(&rank, b)) {
                    best = Some((rank, i, j));
                }
            }
        }
        let Some((_, i, j)) = best else { return hulls };
        let u = hulls[i].union(hulls[j]);
        // Higher index first, so the lower one does not shift under us.
        hulls.remove(j);
        hulls[i] = u;
    }
}

/// What a cluster's tab reads. A device drawn in one piece is just its name;
/// one drawn in several says so, rather than leaving the reader to notice that
/// two identical tabs are the same device.
pub(crate) fn label(name: &str, i: usize, n: usize) -> String {
    if n <= 1 {
        name.to_owned()
    } else {
        format!("{name} {}/{n}", i + 1)
    }
}

/// `c` at alpha `a`, faded by `dim`.
///
/// The ALPHA is scaled, never the components: `Color32` is premultiplied, so
/// gamma-scaling the channels and then rebuilding with `from_rgba_unmultiplied`
/// would darken the hue instead of fading it.
fn alpha(c: egui::Color32, a: u8, dim: f32) -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (f32::from(a) * dim) as u8)
}

/// Where a cluster's tab sits.
///
/// It is as wide as the mat (wider only when the name would not fit), so it
/// reads as that device's title bar rather than as a label pinned to its corner.
///
/// Three candidates, in order: the mat's outward edge, its other edge, and — if
/// neither is clear — INSIDE the mat's top edge.
///
/// The first two can fail for opposite reasons, and the tab sits between the two
/// depths that cause them. A box or an io field is painted AFTER the tab and
/// would cover it; a pin stub is painted BEFORE it, so the tab would cover the
/// stub, and on a pin row an edge tab lands on the NEIGHBOURING pad. The third
/// candidate ends that argument by putting the tab on the device's own ground,
/// which is the one place it can neither hide nor hide anything.
fn tab_rect(
    hull: egui::Rect,
    w: f32,
    chip_center: egui::Pos2,
    canvas: egui::Rect,
    foreign: &[egui::Rect],
) -> egui::Rect {
    // The tab spans the DEVICE, so the name reads as a title over the whole
    // assembly instead of a sticker on its corner. It grows past the mat only
    // when the name needs more room than the mat is wide - a clipped name would
    // be worse than an overhanging tab.
    let w = w.max(hull.width());
    let at = |top: bool| {
        let (t0, t1) = if top {
            (hull.top() - TAB_H + 1.0, hull.top() + 1.0)
        } else {
            (hull.bottom() - 1.0, hull.bottom() + TAB_H - 1.0)
        };
        egui::Rect::from_min_max(egui::pos2(hull.left(), t0), egui::pos2(hull.left() + w, t1))
    };
    let d = hull.center() - chip_center;
    let prefer_top = !(d.y.abs() > d.x.abs() && d.y > 0.0);
    let clear = |r: &egui::Rect| !foreign.iter().any(|f| r.intersects(*f));
    // Inside the mat's own top edge: not pretty on a small mat, but it is the
    // device's own area, so it covers nobody and nobody covers it.
    let inside = egui::Rect::from_min_max(
        egui::pos2(hull.left(), hull.top()),
        egui::pos2(hull.left() + w, hull.top() + TAB_H),
    );
    // The two edges are the candidates; `inside` is what is left when neither is
    // clear, which is why it is the fallback and not a third entry - as an entry
    // it would be unreachable, since `unwrap_or` already ends there.
    let mut t = [at(prefer_top), at(!prefer_top)]
        .into_iter()
        .find(|r| clear(r))
        .unwrap_or(inside);
    // The tab grows rightwards by the measured width of a name the user typed,
    // and the painter clips to the canvas it was allocated - so an unbounded
    // width means a long device name is simply cut off mid-glyph. The margin
    // cannot budget for it (it is text, measured after the canvas is sized), so
    // the tab is pushed back inside instead. Right edge first, so a name wider
    // than the whole canvas stays left-aligned and readable from its start.
    if t.right() > canvas.right() {
        t = t.translate(egui::vec2(canvas.right() - t.right(), 0.0));
    }
    if t.left() < canvas.left() {
        t = t.translate(egui::vec2(canvas.left() - t.left(), 0.0));
    }
    t
}

/// Every device's mats, and every device's tabs, as two lists.
///
/// TWO lists because they belong at two different depths. A mat has to be under
/// the chip body — that is what punches the package out of it for free — but a
/// tab under the body is invisible, and a tab under a 50 px pin stub is
/// unreadable, which is exactly where a bare pad's mat puts it. So the caller
/// gives the tabs their own slot, above the pins and still below every box,
/// wire and field.
///
/// Within each list, all of one device's shapes precede the next device's.
///
/// Takes a `&Painter` and never a `&mut Ui` — see the module docs.
pub fn frames(
    mcu: &Mcu,
    painter: &egui::Painter,
    chip_center: egui::Pos2,
    // The allocated canvas, so a long name is not clipped at its edge.
    canvas: egui::Rect,
    members: &[Member],
    pads: &[Pad],
    // Where each tab landed, for next frame's drag handle. Reported rather than
    // sensed: this function takes a `&Painter` and mints no widget, which is the
    // compile-time proof in the module docs.
    handles: &mut Vec<TabHandle>,
) -> (Vec<egui::Shape>, Vec<egui::Shape>) {
    let hits = mcu.pin_search_highlight();

    // Sorted and de-duplicated by name so the paint order cannot depend on the
    // roster's order — two devices sharing a name share a mat, which is the
    // same reading `join_group` gives them.
    // Trimmed, like `is_live`, `group_color` and `mcu.config` — so two spellings
    // of one name can never draw two mats in one colour.
    let mut names: Vec<&str> = mcu
        .groups
        .iter()
        .filter(|g| g.is_live())
        .map(|g| g.name.trim())
        .collect();
    names.sort_unstable();
    names.dedup();

    let mut mats: Vec<egui::Shape> = Vec::new();
    let mut tabs: Vec<egui::Shape> = Vec::new();
    for name in names {
        // What THIS device's own parts already draw. Scoped to the device and
        // not to the canvas: `group_of_module` resolves a box that wires two
        // devices' pads to the FIRST one it finds, so a canvas-wide set let that
        // box speak for the other device's pad too - the pad dropped out of
        // `mine`, and a device whose only pad was covered that way disappeared
        // from the canvas entirely.
        let spoken: std::collections::HashSet<usize> = members
            .iter()
            .filter(|m| m.group.as_deref().map(str::trim) == Some(name))
            .flat_map(|m| m.covers.iter().copied())
            .collect();
        let (mine, foreign) = split(name, members, pads, &spoken);
        if mine.is_empty() {
            continue;
        }
        let mut hulls = cluster(&mine, &foreign);
        // Canvas reading order, so `1/2` is the upper one and the numbering
        // does not shuffle when the input does.
        hulls.sort_by(|a, b| {
            a.center()
                .y
                .total_cmp(&b.center().y)
                .then(a.center().x.total_cmp(&b.center().x))
        });
        let c = group_color(name);
        // Search dimming is FOLLOWED, not inverted: a full-strength mat around
        // faded pads would make the device the brightest thing on a filtered
        // chip. Done in the colour and not through a `dimmed()` painter, because
        // `Painter::set` re-applies the setting painter's opacity to the whole
        // slot and would fade every device at once.
        let lit = match (&hits, mcu.groups.iter().find(|g| g.name.trim() == name)) {
            // An I2C device has no pad of its own: the search lights it through
            // its bus's SCL and SDA.
            (Some(h), Some(g)) => {
                g.pins.iter().any(|p| h.contains(p))
                    || g.i2c.iter().any(|(inst, _)| {
                        mcu.modules
                            .iter()
                            .filter(|m| {
                                m.kind
                                    == crate::panels::mcu_module::modules::ModuleKind::GenericInterfaceI2c
                                    && m.instance() == *inst
                            })
                            .flat_map(|m| m.connections.iter())
                            .any(|c| h.contains(&c.mcu_pin))
                    })
            }
            (Some(_), None) => false,
            (None, _) => true,
        };
        let dim = if lit { 1.0_f32 } else { chip::SEARCH_DIM };
        // The search filter answers FIRST. A device the search has faded may not
        // become the brightest thing on the chip just because it is selected -
        // two different questions would be answering in one alpha channel.
        let sel = lit && mcu.active_device() == Some(name);
        let (fill_a, edge_a, edge_w) = if sel {
            (FILL_A_SEL, EDGE_A_SEL, EDGE_W_SEL)
        } else {
            (FILL_A, EDGE_A, 1.0_f32)
        };
        let n = hulls.len();
        for (i, h) in hulls.into_iter().enumerate() {
            let hull = h.expand(PAD);
            mats.push(egui::Shape::Rect(egui::epaint::RectShape::new(
                hull,
                R_MAT,
                alpha(c, fill_a, dim),
                egui::Stroke::new(edge_w, alpha(c, edge_a, dim)),
                egui::StrokeKind::Inside,
            )));
            let galley = painter.layout_no_wrap(
                label(name, i, n),
                egui::FontId::proportional(TAB_PT),
                alpha(TAB_FG, 255, dim),
            );
            let tab = tab_rect(
                hull,
                galley.size().x + 2.0 * TAB_PAD_X,
                chip_center,
                canvas,
                &foreign,
            );
            handles.push(TabHandle {
                name: name.to_owned(),
                cluster: i,
                off: tab.translate(-chip_center.to_vec2()),
            });
            tabs.push(egui::Shape::rect_filled(tab, R_TAB, alpha(c, 255, dim)));
            tabs.push(egui::Shape::galley(
                egui::pos2(
                    tab.left() + TAB_PAD_X,
                    tab.center().y - galley.size().y / 2.0,
                ),
                galley,
                alpha(TAB_FG, 255, dim),
            ));
        }
    }
    (mats, tabs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::mcu::model::{PIN_SPACING, PIN_WIDTH};

    /// Run `f` with a painter that can lay out text — `frames` measures the tab
    /// from a real galley, so it needs fonts and therefore a frame in progress.
    fn with_painter(f: impl FnOnce(&egui::Painter)) {
        let ctx = egui::Context::default();
        // `Context::run` takes an `FnMut`, so the one-shot closure is parked in
        // an Option and taken on the first pass.
        let mut once = Some(f);
        let _ = crate::headless::run_ui(&ctx, Default::default(), |ui| {
            if let Some(f) = once.take() {
                f(&egui::Painter::new(
                    ui.ctx().clone(),
                    egui::LayerId::debug(),
                    egui::Rect::EVERYTHING,
                ));
            }
        });
        assert!(once.is_none(), "the closure ran inside a frame");
    }

    fn grouped_mcu(groups: &[(&str, &[usize])]) -> crate::panels::mcu_module::mcu::Mcu {
        let mut mcu = crate::panels::mcu_module::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        mcu.groups = groups
            .iter()
            .map(
                |(n, pins)| crate::panels::mcu_module::mcu_config::PinGroup {
                    name: (*n).to_owned(),
                    pins: pins.iter().copied().collect(),
                    ..Default::default()
                },
            )
            .collect();
        mcu
    }

    /// A search string that hits a real pin, but never `spare`.
    ///
    /// A search matching NOTHING is not a filter: `pin_search_highlight` turns
    /// an empty hit set into `None`, i.e. "show everything". A test that used one
    /// was asserting that an unfiltered chip is not brighter than an unfiltered
    /// chip, which is true and says nothing.
    fn search_missing(mcu: &crate::panels::mcu_module::mcu::Mcu, spare: usize) -> String {
        let name = mcu
            .iter_all_pins()
            .find(|p| p.number != spare && !p.name.is_empty())
            .map(|p| p.name.clone())
            .expect("some other pin");
        assert!(!name.eq_ignore_ascii_case(&mcu.find_pin(spare).expect("the spare pad").name));
        name
    }

    /// A canvas big enough that the tab clamp never fires.
    fn roomy() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(-2000.0, -2000.0), egui::pos2(2000.0, 2000.0))
    }

    /// One device drawn in one piece is one mat, and a tab made of its plate and
    /// its name.
    #[test]
    fn one_cluster_is_a_mat_a_tab_and_a_name() {
        let mcu = grouped_mcu(&[("radar", &[1, 2])]);
        let members = vec![member(Some("radar"), boxx(300.0, 0.0), &[1, 2])];
        with_painter(|p| {
            let (mats, tabs) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            );
            assert_eq!(mats.len(), 1);
            assert_eq!(tabs.len(), 2);
            assert!(matches!(tabs[1], egui::Shape::Text(_)), "the name is last");
        });
    }

    /// Mats and tabs come back as SEPARATE lists, because they are painted at two
    /// different depths: a mat under the chip body (which is what punches the
    /// package out of it), a tab above the pin stubs (where it can be read).
    /// Nothing in the mat list may be text.
    #[test]
    fn mats_and_tabs_come_back_as_two_layers() {
        let mcu = grouped_mcu(&[("radar", &[1]), ("display", &[9])]);
        let members = vec![
            member(Some("radar"), boxx(300.0, 0.0), &[1]),
            member(Some("display"), boxx(300.0, 400.0), &[9]),
        ];
        with_painter(|p| {
            let (mats, tabs) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            );
            assert_eq!(mats.len(), 2, "one mat per device");
            assert_eq!(tabs.len(), 4, "a plate and a name per device");
            assert!(
                !mats.iter().any(|s| matches!(s, egui::Shape::Text(_))),
                "no name is stranded in the layer under the body"
            );
        });
    }

    /// A device with nothing on the canvas draws nothing — and a board with no
    /// devices at all draws nothing, which is every project that predates this.
    #[test]
    fn a_device_with_no_footprint_draws_nothing() {
        let bare = grouped_mcu(&[]);
        let orphan = grouped_mcu(&[("radar", &[1])]);
        with_painter(|p| {
            let (m, t) = frames(
                &bare,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &[],
                &[],
                &mut Vec::new(),
            );
            assert!(m.is_empty() && t.is_empty());
            // Grouped, but nothing on the canvas stands for the pad.
            let (m, t) = frames(
                &orphan,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &[],
                &[],
                &mut Vec::new(),
            );
            assert!(m.is_empty() && t.is_empty());
        });
    }

    /// A device the user has not named yet is not a device: it stays on the
    /// roster and reaches neither the canvas, nor `mcu.config`, nor the
    /// generated comment. The three used to disagree.
    #[test]
    fn an_unnamed_device_is_not_drawn() {
        let mcu = grouped_mcu(&[("", &[1])]);
        let members = vec![member(Some(""), boxx(300.0, 0.0), &[1])];
        with_painter(|p| {
            let (m, t) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            );
            assert!(m.is_empty() && t.is_empty());
        });
    }

    /// A name is free text and the tab grows to fit it, but the painter clips to
    /// the canvas it was allocated — so the tab is pushed back inside rather than
    /// cut off mid-glyph.
    #[test]
    fn a_long_name_is_pushed_back_inside_the_canvas() {
        let mcu = grouped_mcu(&[("front distance sensor board rev C", &[1])]);
        let bx = boxx(300.0, 0.0);
        let members = vec![member(Some("front distance sensor board rev C"), bx, &[1])];
        // Where the tab WANTS to start, and a canvas that ends 40 px later — far
        // less than the name needs.
        let anchor = bx.expand(PAD).left();
        let canvas =
            egui::Rect::from_min_max(egui::pos2(0.0, -100.0), egui::pos2(anchor + 40.0, 400.0));
        with_painter(|p| {
            let (_, tabs) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                canvas,
                &members,
                &[],
                &mut Vec::new(),
            );
            let plate = match &tabs[0] {
                egui::Shape::Rect(r) => r.rect,
                _ => panic!("the plate is first"),
            };
            assert!(
                anchor + plate.width() > canvas.right(),
                "unclamped the tab would run off the canvas"
            );
            assert!(
                plate.right() <= canvas.right() + 0.01 && plate.left() >= canvas.left() - 0.01,
                "the tab {plate:?} stays inside {canvas:?}"
            );
        });
    }

    /// Selection may not undo the search dim. Two different questions answer in
    /// the same alpha channel, so the order between them has to be fixed: a
    /// device the search has faded stays faded even while it is selected, or it
    /// becomes the brightest thing on a filtered chip.
    #[test]
    fn a_searched_out_device_is_never_the_brightest_thing() {
        let mut mcu = grouped_mcu(&[("radar", &[1])]);
        let members = vec![member(Some("radar"), boxx(300.0, 0.0), &[1])];
        // The stroke WIDTH, not only the fill alpha: the alpha is multiplied by
        // the dim whichever order the two gates run in, so it cannot tell them
        // apart. The width is not dimmed at all, so it can.
        let mat = |shapes: &[egui::Shape]| match &shapes[0] {
            egui::Shape::Rect(r) => (r.fill.a(), r.stroke.width),
            _ => panic!("the mat is first"),
        };
        with_painter(|p| {
            let plain = mat(&frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            )
            .0);
            mcu.selected_device = Some("radar".into());
            let selected = mat(&frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            )
            .0);
            assert!(selected.0 > plain.0, "selecting it does brighten the mat");
            assert!(selected.1 > plain.1, "and thickens its hairline");

            mcu.pin_search = search_missing(&mcu, 1);
            let hits = mcu
                .pin_search_highlight()
                .expect("the search really filters");
            assert!(!hits.contains(&1), "and it excludes this device's pad");
            let searched_out = mat(&frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            )
            .0);
            assert!(
                searched_out.0 < plain.0,
                "…but the search still wins on the fill: {searched_out:?} vs {plain:?}"
            );
            assert_eq!(
                searched_out.1, plain.1,
                "and on the hairline, which no dim can hide"
            );
        });
    }

    /// The pin search dims the pads it filtered out; a full-strength mat around
    /// them would make the device the brightest thing on the chip.
    #[test]
    fn a_device_the_search_filtered_out_is_dimmed_with_its_pads() {
        let mut mcu = grouped_mcu(&[("radar", &[1])]);
        let members = vec![member(Some("radar"), boxx(300.0, 0.0), &[1])];
        let fill = |shapes: &[egui::Shape]| match &shapes[0] {
            egui::Shape::Rect(r) => r.fill.a(),
            _ => panic!("the mat is first"),
        };
        with_painter(|p| {
            let bright = fill(
                &frames(
                    &mcu,
                    p,
                    egui::Pos2::ZERO,
                    roomy(),
                    &members,
                    &[],
                    &mut Vec::new(),
                )
                .0,
            );
            // A search that hits a real pin, but not this device's.
            mcu.pin_search = search_missing(&mcu, 1);
            let hits = mcu
                .pin_search_highlight()
                .expect("the search really filters");
            assert!(!hits.contains(&1), "and it excludes this device's pad");
            let faded = fill(
                &frames(
                    &mcu,
                    p,
                    egui::Pos2::ZERO,
                    roomy(),
                    &members,
                    &[],
                    &mut Vec::new(),
                )
                .0,
            );
            assert!(faded < bright, "dimmed, and measurably so");
        });
    }

    /// A pad another DEVICE's box happens to wire must not silently erase this
    /// device from the canvas.
    ///
    /// `group_of_module` resolves a box that straddles two devices to the first
    /// pad it finds, so a canvas-wide "already drawn" set let that box speak for
    /// the other device's pad too — and a device whose only pad was covered that
    /// way vanished, with no mat, no tab and no complaint.
    #[test]
    fn a_device_keeps_its_mat_when_a_foreign_box_wires_its_pad() {
        let mcu = grouped_mcu(&[("radar", &[3]), ("display", &[4])]);
        // One box, wiring both pads, resolved to "radar".
        let members = vec![member(Some("radar"), boxx(300.0, 0.0), &[3, 4])];
        let pads = vec![
            pad(Some("radar"), 3, tip(3)),
            pad(Some("display"), 4, tip(4)),
        ];
        with_painter(|p| {
            let (mats, _) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &pads,
                &mut Vec::new(),
            );
            assert_eq!(mats.len(), 2, "both devices are on the canvas");
        });
    }

    /// A pad's tip square, `i` pads along a row. The real pitch, so the gaps
    /// these tests reason about are the ones the chip actually produces.
    fn tip(i: usize) -> egui::Rect {
        egui::Rect::from_center_size(
            egui::pos2(0.0, i as f32 * (PIN_WIDTH + PIN_SPACING)),
            egui::Vec2::splat(TIP),
        )
    }

    fn boxx(x: f32, y: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(170.0, 98.0))
    }

    fn member(group: Option<&str>, rect: egui::Rect, covers: &[usize]) -> Member {
        Member {
            group: group.map(str::to_owned),
            rect,
            covers: covers.to_vec(),
        }
    }

    fn pad(group: Option<&str>, num: usize, rect: egui::Rect) -> Pad {
        Pad {
            group: group.map(str::to_owned),
            num,
            rect,
        }
    }

    /// The everyday case: a device on two pads next to each other is ONE mat.
    #[test]
    fn two_adjacent_pads_of_one_device_share_a_mat() {
        assert_eq!(cluster(&[tip(3), tip(4)], &[tip(2), tip(5)]).len(), 1);
    }

    /// A device holding pads 3 and 5 must NOT draw a mat over pad 4, which it
    /// does not own and which may belong to something else entirely.
    ///
    /// Along a pin row this is `JOIN`'s job, not the guard's — the skip-one gap
    /// is 40 and the threshold is 24 — which is exactly why
    /// `the_join_threshold_stays_inside_its_band` pins the upper end of the
    /// band. The guard's own case is diagonal; see the two tests below.
    #[test]
    fn a_device_that_skips_a_pad_never_covers_it() {
        let hulls = cluster(&[tip(3), tip(5)], &[tip(4)]);
        assert_eq!(hulls.len(), 2, "two mats, not one reaching across");
        assert!(
            !hulls.iter().any(|h| h.intersects(tip(4))),
            "and neither one touches the pad in between"
        );
    }

    /// The obstacle set is EVERY foreign footprint, not just the grouped ones.
    /// A pad in no device at all is still a pad the mat may not cross.
    #[test]
    fn an_ungrouped_pad_is_an_obstacle_too() {
        let members = vec![];
        let pads = vec![
            pad(Some("radar"), 3, tip(3)),
            pad(None, 4, tip(4)),
            pad(Some("radar"), 5, tip(5)),
        ];
        let (mine, foreign) = split("radar", &members, &pads, &Default::default());
        assert_eq!(mine.len(), 2);
        assert_eq!(foreign.len(), 1, "the ungrouped pad is foreign, not absent");
        assert_eq!(cluster(&mine, &foreign).len(), 2);
    }

    /// Two boxes packed on one chip side sit `BOX_GAP` apart, and a device
    /// holding both must draw them on one mat — that is what `JOIN >= BOX_GAP`
    /// buys.
    #[test]
    fn two_boxes_packed_on_one_side_share_a_mat() {
        let a = boxx(0.0, 0.0);
        let b = boxx(0.0, a.height() + super::super::modules::BOX_GAP);
        assert_eq!(cluster(&[a, b], &[]).len(), 1);
    }

    /// Splitting is about FOOTPRINTS, not about pads: another device's box
    /// standing between two of mine splits my mat exactly as a pad would. Also
    /// a `JOIN` case — two boxes with a third between them are 126 px apart.
    #[test]
    fn a_foreign_box_between_two_members_splits_the_mat() {
        let g = super::super::modules::BOX_GAP;
        let a = boxx(0.0, 0.0);
        let mid = boxx(0.0, a.height() + g);
        let c = boxx(0.0, 2.0 * (a.height() + g));
        let hulls = cluster(&[a, c], &[mid]);
        assert_eq!(hulls.len(), 2);
        assert!(!hulls.iter().any(|h| h.intersects(mid)));
    }

    /// A device with parts on OPPOSITE sides of the chip draws two mats and
    /// never one hull across the package.
    ///
    /// The easy half of the scattered case: the two are hundreds of pixels
    /// apart, so `JOIN` refuses them long before the guard is consulted. The
    /// hard half is the corner, where the two parts really are close — that is
    /// `a_device_hugging_a_corner_never_covers_the_pads_in_it`.
    #[test]
    fn a_device_on_two_sides_never_swallows_the_chip() {
        let body = egui::Rect::from_min_max(egui::pos2(-100.0, -100.0), egui::pos2(100.0, 100.0));
        let left = boxx(-400.0, -49.0);
        let right = boxx(230.0, -49.0);
        let rows: Vec<egui::Rect> = (0..4)
            .flat_map(|i| {
                let y = -60.0 + i as f32 * 33.0;
                [
                    egui::Rect::from_center_size(egui::pos2(-113.0, y), egui::Vec2::splat(TIP)),
                    egui::Rect::from_center_size(egui::pos2(113.0, y), egui::Vec2::splat(TIP)),
                ]
            })
            .chain(std::iter::once(body))
            .collect();
        let hulls = cluster(&[left, right], &rows);
        assert_eq!(hulls.len(), 2, "one mat per side");
        assert!(!hulls.iter().any(|h| h.intersects(body)));
    }

    /// THE GUARD'S OWN CASE, and the one `JOIN` structurally cannot catch.
    ///
    /// `gap` is a distance; the union is an AREA. Two parts 20 px apart on a
    /// diagonal have a union wide enough to swallow whatever sits in the corner
    /// between them — and the gap says nothing about it.
    #[test]
    fn a_diagonal_pair_never_swallows_what_lies_between_them() {
        let a = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(40.0, 20.0));
        let b = egui::Rect::from_min_max(egui::pos2(54.0, 34.0), egui::pos2(94.0, 54.0));
        let between = egui::Rect::from_min_max(egui::pos2(45.0, 2.0), egui::pos2(50.0, 16.0));
        assert!(gap(a, b) < JOIN, "the threshold is happy to merge these");
        assert!(
            a.union(b).intersects(between),
            "and merging them would claim the thing in the corner"
        );
        let hulls = cluster(&[a, b], &[between]);
        assert_eq!(hulls.len(), 2, "so the merge is refused");
        assert!(!hulls.iter().any(|h| h.intersects(between)));
    }

    /// Half inside is inside.
    ///
    /// The guard tests INTERSECTION and not "does the union contain its centre":
    /// a pad square straddling the edge of a mat reads as part of the device to
    /// anyone looking at it, and its centre being a few pixels outside changes
    /// nothing about that.
    #[test]
    fn a_foreign_footprint_only_half_inside_still_refuses_the_merge() {
        let a = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(40.0, 20.0));
        let b = egui::Rect::from_min_max(egui::pos2(54.0, 34.0), egui::pos2(94.0, 54.0));
        // Straddles the union's right edge: centre well outside, body inside.
        let straddler = egui::Rect::from_min_max(egui::pos2(88.0, 20.0), egui::pos2(120.0, 30.0));
        let u = a.union(b);
        assert!(u.intersects(straddler));
        assert!(!u.contains(straddler.center()), "its centre is outside");
        assert!(!a.intersects(straddler) && !b.intersects(straddler));
        assert_eq!(cluster(&[a, b], &[straddler]).len(), 2);
    }

    /// The realistic shape of the guard's case: a device with one box above the
    /// chip and one to its right, packed a box-gap apart around the corner. The
    /// two are close enough for `JOIN`, and their union covers the whole corner
    /// of the package — every pad on it, none of them the device's.
    #[test]
    fn a_device_hugging_a_corner_never_covers_the_pads_in_it() {
        let top = egui::Rect::from_min_max(egui::pos2(60.0, -220.0), egui::pos2(230.0, -122.0));
        let right = egui::Rect::from_min_max(egui::pos2(244.0, -108.0), egui::pos2(414.0, -10.0));
        // The right edge's pads, none of them this device's.
        let pads: Vec<egui::Rect> = (0..3)
            .map(|i| {
                egui::Rect::from_center_size(
                    egui::pos2(113.0, -60.0 + i as f32 * 33.0),
                    egui::Vec2::splat(TIP),
                )
            })
            .collect();
        assert!(gap(top, right) < JOIN, "the threshold would merge them");
        assert!(
            pads.iter().any(|p| top.union(right).intersects(*p)),
            "and the merged hull would sit over the corner pads"
        );
        let hulls = cluster(&[top, right], &pads);
        assert_eq!(hulls.len(), 2);
        assert!(
            !hulls.iter().any(|h| pads.iter().any(|p| h.intersects(*p))),
            "neither mat touches a pad the device does not own"
        );
    }

    /// Boxes arrive in side-packing order and fields in pin order, so the
    /// clustering may not depend on either. A mat that changes shape when a pin
    /// is renamed is a bug nobody would ever find.
    ///
    /// The arrangement is chosen so ORDER can actually change the answer: three
    /// parts each within `JOIN` of the others, and a foreign rect in the fourth
    /// corner that any two-step merge would claim. Exactly one pair may merge,
    /// and which one is a pure ranking decision.
    #[test]
    fn clustering_does_not_depend_on_input_order() {
        let a = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(20.0, 20.0));
        let b = egui::Rect::from_min_max(egui::pos2(24.0, 0.0), egui::pos2(44.0, 20.0));
        let c = egui::Rect::from_min_max(egui::pos2(0.0, 24.0), egui::pos2(20.0, 44.0));
        let corner = egui::Rect::from_min_max(egui::pos2(30.0, 30.0), egui::pos2(40.0, 40.0));
        let key = |hs: Vec<egui::Rect>| {
            let mut v: Vec<(i32, i32, i32, i32)> = hs
                .iter()
                .map(|r| {
                    (
                        r.left() as i32,
                        r.top() as i32,
                        r.right() as i32,
                        r.bottom() as i32,
                    )
                })
                .collect();
            v.sort_unstable();
            v
        };
        let want = key(cluster(&[a, b, c], &[corner]));
        for order in [[c, b, a], [b, a, c], [c, a, b], [a, c, b], [b, c, a]] {
            assert_eq!(key(cluster(&order, &[corner])), want, "order {order:?}");
        }
        // …and a long row still collapses to one mat whatever order it arrives in.
        let row = [tip(0), tip(1), tip(2), tip(3)];
        let straight = key(cluster(&row, &[]));
        let mut shuffled = row;
        shuffled.reverse();
        assert_eq!(key(cluster(&shuffled, &[])), straight);
        assert_eq!(straight.len(), 1);
    }

    /// A pad a module box already draws contributes NO second footprint to its
    /// own device — but stays an obstacle for every other one.
    #[test]
    fn a_pad_drawn_by_its_module_box_gets_no_second_footprint() {
        let members = vec![member(Some("radar"), boxx(200.0, 0.0), &[3, 4])];
        let pads = vec![
            pad(Some("radar"), 3, tip(3)),
            pad(Some("radar"), 4, tip(4)),
            pad(Some("display"), 9, tip(9)),
        ];
        let spoken: std::collections::HashSet<usize> = [3, 4].into_iter().collect();

        let (mine, _) = split("radar", &members, &pads, &spoken);
        assert_eq!(mine.len(), 1, "the box, and no pad squares under it");

        let (_, foreign) = split("display", &members, &pads, &spoken);
        assert_eq!(
            foreign.len(),
            3,
            "the other device still sees the box AND both its pads"
        );
    }

    /// A pad contributes the SAME square in every rotation mode.
    ///
    /// This is what buys the module its total absence of rotation branches. Swap
    /// the tip square for the stub's rotated bounding box and the diamond
    /// inflates a 50x30 stub to ~57x57, adjacent squares start overlapping, and
    /// a whole face of the package fuses into one mat.
    #[test]
    fn a_pad_footprint_is_the_same_square_in_every_rotation() {
        let mcu = crate::panels::mcu_module::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        let chip = egui::Rect::from_center_size(egui::pos2(400.0, 300.0), egui::vec2(220.0, 460.0));
        let center = chip.center();

        let plain = pad_footprints(&mcu, chip, Rot::new(center, 0.0));
        assert!(!plain.is_empty(), "the Pico has edge pads");
        for angle in [
            0.0_f32,
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::FRAC_PI_4,
        ] {
            let rot = Rot::new(center, angle);
            let turned = pad_footprints(&mcu, chip, rot);
            assert_eq!(turned.len(), plain.len(), "same pads at {angle}");
            for (a, b) in plain.iter().zip(&turned) {
                assert_eq!(a.num, b.num);
                assert!(
                    (b.rect.size() - egui::Vec2::splat(TIP)).length() < 0.001,
                    "pin {} kept its square at {angle}",
                    b.num
                );
                // …centred on the rotated stub tip, not on some rotated box.
                let want = rot.apply(a.rect.center());
                assert!(
                    (b.rect.center() - want).length() < 0.01,
                    "pin {} sits on its rotated tip at {angle}",
                    b.num
                );
            }
        }

        // And the pitch survives, which is what the JOIN band is measured
        // against: adjacent squares on one edge stay a fixed gap apart in every
        // mode.
        let rot = Rot::new(center, std::f32::consts::FRAC_PI_4);
        let turned = pad_footprints(&mcu, chip, rot);
        let d = (turned[1].rect.center() - turned[0].rect.center()).length();
        assert!(
            (d - (PIN_WIDTH + PIN_SPACING)).abs() < 0.01,
            "adjacent tips are one pitch apart on a diamond too, got {d}"
        );
    }

    /// A tab says `1/2` only when the device really is drawn in two pieces.
    #[test]
    fn a_scattered_device_numbers_its_mats_but_a_whole_one_does_not() {
        assert_eq!(label("radar", 0, 1), "radar");
        assert_eq!(label("radar", 0, 2), "radar 1/2");
        assert_eq!(label("radar", 1, 2), "radar 2/2");
    }

    /// The painter is sized before anything is painted, and painted shapes never
    /// grow `min_rect` — so if these constants drift apart from what a mat draws,
    /// the mat is silently clipped at the canvas edge.
    #[test]
    fn the_margin_constants_cover_what_a_mat_actually_draws() {
        assert!(HALO >= PAD + TAB_H);
        assert!(BARE_REACH >= TIP / 2.0 + PAD);
    }

    /// The square has to fit between two pads in EVERY rotation, and the tightest
    /// case is the 45° diamond, where a pin row runs diagonally and two adjacent
    /// tips are only `pitch / √2` apart on each axis.
    ///
    /// Over that, neighbouring squares overlap, their union reaches the pad
    /// beyond, the guard refuses the merge, and a device on two adjacent pads
    /// draws itself as two numbered mats the moment the chip is rotated.
    #[test]
    fn the_tip_square_fits_between_two_pads_in_every_rotation() {
        let diagonal_pitch = (PIN_WIDTH + PIN_SPACING) / std::f32::consts::SQRT_2;
        assert!(TIP < diagonal_pitch, "{TIP} vs {diagonal_pitch}");
        // …and two adjacent tips still MERGE at that pitch, in both modes.
        let diag = |i: f32| {
            egui::Rect::from_center_size(
                egui::pos2(i * diagonal_pitch, i * diagonal_pitch),
                egui::Vec2::splat(TIP),
            )
        };
        assert_eq!(cluster(&[diag(0.0), diag(1.0)], &[diag(2.0)]).len(), 1);
        assert_eq!(cluster(&[tip(0), tip(1)], &[tip(2)]).len(), 1);
        // …and skipping one is still refused, in both.
        assert_eq!(cluster(&[diag(0.0), diag(2.0)], &[diag(1.0)]).len(), 2);
        assert_eq!(cluster(&[tip(0), tip(2)], &[tip(1)]).len(), 2);
    }

    /// A merge is refused only for territory it NEWLY claims.
    ///
    /// An io field's footprint is taller than the pin pitch, so a field next to a
    /// foreign one overlaps it before anything is merged. Testing the bare union
    /// refused every merge that field could ever take part in — a device on two
    /// adjacent inputs drew "radar 1/2" and "radar 2/2" whenever a third input
    /// sat beside them.
    #[test]
    fn a_merge_is_refused_only_for_what_it_newly_claims() {
        let pitch = PIN_WIDTH + PIN_SPACING;
        // Three io footprints on one row, each taller than the pitch.
        let field = |i: f32| {
            egui::Rect::from_min_max(
                egui::pos2(0.0, i * pitch - 19.0),
                egui::pos2(88.0, i * pitch + 19.0),
            )
        };
        let (a, b, foreign) = (field(0.0), field(1.0), field(2.0));
        assert!(b.intersects(foreign), "the neighbours already overlap");
        let hulls = cluster(&[a, b], &[foreign]);
        assert_eq!(hulls.len(), 1, "the merge takes no new territory");
        // …but one that WOULD reach past the neighbour is still refused.
        assert_eq!(cluster(&[a, field(3.0)], &[foreign]).len(), 2);
    }

    /// "Already overlapping" is not a licence to grow without limit over the same
    /// footprint.
    ///
    /// Two parts can each clip a foreign rect from opposite corners, so neither
    /// merge claims anything NEW — and their union holds the foreign part
    /// entirely. That is the exact lie this module exists to prevent, so it is
    /// refused whatever the parts already overlapped.
    #[test]
    fn a_merge_may_not_swallow_a_foreign_part_it_already_grazed() {
        let f = egui::Rect::from_min_max(egui::pos2(100.0, 100.0), egui::pos2(120.0, 140.0));
        let a = egui::Rect::from_min_max(egui::pos2(90.0, 90.0), egui::pos2(110.0, 110.0));
        let b = egui::Rect::from_min_max(egui::pos2(110.0, 130.0), egui::pos2(130.0, 150.0));
        assert!(a.intersects(f) && b.intersects(f), "both already graze it");
        assert!(
            gap(a, b) <= JOIN,
            "and they are close enough to want to merge"
        );
        let u = a.union(b);
        assert!(
            u.contains(f.min) && u.contains(f.max),
            "the union would hold it"
        );
        assert_eq!(cluster(&[a, b], &[f]).len(), 2, "so the merge is refused");
    }

    /// …and the exemption it does NOT undo: a merge that only keeps grazing what
    /// it already grazed is still allowed.
    #[test]
    fn a_merge_that_only_keeps_grazing_is_still_allowed() {
        let f = egui::Rect::from_min_max(egui::pos2(100.0, 0.0), egui::pos2(120.0, 400.0));
        let a = egui::Rect::from_min_max(egui::pos2(90.0, 90.0), egui::pos2(110.0, 110.0));
        let b = egui::Rect::from_min_max(egui::pos2(90.0, 120.0), egui::pos2(110.0, 140.0));
        assert!(a.intersects(f) && b.intersects(f));
        assert!(
            !a.union(b).contains(f.max),
            "the tall foreign rect is never held whole"
        );
        assert_eq!(cluster(&[a, b], &[f]).len(), 1);
    }

    /// When neither edge of the mat is clear, the tab goes INSIDE it — on the
    /// device's own ground, where it covers nobody and nobody covers it. Both
    /// edges fail for opposite reasons: a box painted after the tab would cover
    /// it, and a pin stub painted before it would be covered BY it.
    #[test]
    fn a_tab_with_no_clear_edge_sits_on_its_own_mat() {
        let hull = egui::Rect::from_min_max(egui::pos2(0.0, 100.0), egui::pos2(60.0, 140.0));
        let above = egui::Rect::from_min_max(egui::pos2(-40.0, 60.0), egui::pos2(100.0, 99.0));
        let below = egui::Rect::from_min_max(egui::pos2(-40.0, 141.0), egui::pos2(100.0, 180.0));
        let roomy = egui::Rect::from_min_max(egui::pos2(-500.0, -500.0), egui::pos2(500.0, 500.0));
        let t = tab_rect(hull, 40.0, egui::Pos2::ZERO, roomy, &[above, below]);
        assert!(!t.intersects(above) && !t.intersects(below), "{t:?}");
        assert!(hull.contains(t.left_top()) && hull.contains(t.left_bottom()));
    }

    /// The rim is at least seven pixels. Below that the mat stopped reading as
    /// ground the parts sit on and became an outline traced tightly around them,
    /// with a box's own border almost touching the mat's hairline.
    #[test]
    fn a_mat_keeps_at_least_seven_pixels_of_rim() {
        assert!(PAD >= 7.0, "{PAD}");
        let hull = boxx(0.0, 0.0);
        let grown = hull.expand(PAD);
        assert!(grown.left() <= hull.left() - 7.0);
        assert!(grown.top() <= hull.top() - 7.0);
    }

    /// The tab spans the device, so its name reads as a title over the whole
    /// assembly rather than a sticker on one corner — and it grows past the mat
    /// only when a long name would otherwise be cut.
    #[test]
    fn the_tab_is_at_least_as_wide_as_the_device() {
        let mcu = grouped_mcu(&[("r", &[1])]);
        let bx = boxx(300.0, 0.0);
        let members = vec![member(Some("r"), bx, &[1])];
        with_painter(|p| {
            let (mats, tabs) = frames(
                &mcu,
                p,
                egui::Pos2::ZERO,
                roomy(),
                &members,
                &[],
                &mut Vec::new(),
            );
            let mat = match &mats[0] {
                egui::Shape::Rect(r) => r.rect,
                _ => panic!("the mat is first"),
            };
            let plate = match &tabs[0] {
                egui::Shape::Rect(r) => r.rect,
                _ => panic!("the plate is first"),
            };
            assert!(
                plate.width() >= mat.width() - 0.01,
                "tab {} vs mat {}",
                plate.width(),
                mat.width()
            );
            assert!(plate.left() <= mat.left() + 0.01, "and it starts with it");
        });
    }

    /// The tab's type is half again the diagram's small text: it names the whole
    /// assembly, so it outranks the signal names and pin numbers around it.
    #[test]
    fn the_tab_reads_larger_than_the_diagrams_small_text() {
        assert!(TAB_PT >= 8.5 * 1.5 - 0.01, "{TAB_PT}");
        assert!(TAB_H > TAB_PT, "and the plate fits the type");
    }

    /// The only coupling this module has to constants it does not own. Both ends
    /// matter: below the box gap two packed boxes stop sharing a mat, at or above
    /// the skip-one-pad gap the threshold starts asking for merges the guard has
    /// to rescue.
    #[test]
    fn the_join_threshold_stays_inside_its_band() {
        assert!(JOIN >= super::super::modules::BOX_GAP);
        assert!(JOIN < 2.0 * (PIN_WIDTH + PIN_SPACING) - TIP);
    }
}
