//! The automatic arrangement inside a chip frame, a spot for a frame that has
//! none yet, and the path of a link between two frames.
//!
//! The frame is the only thing the user moves; everything inside it is placed
//! here, from the chip and its links:
//!
//! ```text
//! ┌ STM32F103C8 ─────────────────────────────────────┐
//! │ stm32_main · Blocking                            │
//! │ ( UART0 )        ( OLED 128x32 )─┐   ( USART1 ) TX PA9 ●──
//! │                  ( BME280      )─┴───( I2C1   ) RX PA10 ●──
//! │                  ( Status LED  )     ( SPI1   )      │
//! └──────────────────────────────────────────────────┘
//! ```
//!
//! Modules in a column on the side their partner chip is on (right when they
//! have none), in the order of what they link to, so the links do not cross.
//! Devices in the middle column, each level with the module(s) it uses and
//! pushed down only as far as it takes not to overlap the device above; a
//! device on bare pads goes last. In the Detailed view a linked module grows a
//! row per pin, with the pad named between it and the frame edge. All in the
//! frame's LOCAL coordinates: the canvas adds the frame's position.

use eframe::egui::{Pos2, Rect, Vec2, pos2, vec2};

use super::snapshot::ChipView;

/// Inner margin of a frame.
pub const PAD: f32 = 14.0;
/// Title and subtitle, above the first row.
pub const HEADER_H: f32 = 48.0;
/// One module or device.
pub const PILL_H: f32 = 24.0;
/// One pin row of a module in the Detailed view.
pub const PIN_ROW_H: f32 = 20.0;
/// Space between two rows in a column.
pub const ROW_GAP: f32 = 8.0;
/// Device column width.
pub const DEVICE_W: f32 = 128.0;
/// Module column width.
pub const MODULE_W: f32 = 104.0;
/// The pad names between a module and the frame edge (Detailed view).
pub const PIN_W: f32 = 84.0;
/// Between two columns: room for the device-to-module wires.
pub const COLUMN_GAP: f32 = 36.0;
/// The narrowest frame: room for the chip's name.
pub const MIN_FRAME_W: f32 = 200.0;
/// A frame with devices and one module column - the Phase 1 frame, and what
/// a chip nobody has placed yet is assumed to take.
pub const FRAME_W: f32 = PAD + DEVICE_W + COLUMN_GAP + MODULE_W + PAD;
/// The body of a frame with nothing in it: one line of text.
pub const NOTE_H: f32 = 28.0;
/// Between frames the canvas places side by side.
pub const FRAME_GAP: f32 = 80.0;
/// Where the first frame of a new system goes.
pub const ORIGIN: Pos2 = pos2(40.0, 40.0);
/// How far a link leaves the frame edge before it may turn.
pub const STUB: f32 = 16.0;

/// Which edge of its frame a module sits at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    /// +1 for right, -1 for left: the way a link leaves this edge.
    pub fn out(self) -> f32 {
        match self {
            Side::Left => -1.0,
            Side::Right => 1.0,
        }
    }
}

/// How the modules of one frame are arranged - decided from the links.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrange {
    /// Per module (same order as [`ChipView::modules`]).
    pub side: Vec<Side>,
    /// Sort key per module within its column; lower is higher up.
    pub rank: Vec<f32>,
    /// Per module, the pads shown at the frame edge (Detailed view), each
    /// with its pin number so a wire can find its row.
    pub pins: Vec<Vec<(usize, String)>>,
}

impl Arrange {
    /// No links: every module on the right, in list order, no pins.
    pub fn plain(chip: &ChipView) -> Self {
        let n = chip.modules.len();
        Self {
            side: vec![Side::Right; n],
            rank: (0..n).map(|i| i as f32).collect(),
            pins: vec![Vec::new(); n],
        }
    }
}

/// One pad of a module, named at the frame edge.
#[derive(Clone, Debug, PartialEq)]
pub struct PinRow {
    pub pin: usize,
    /// `TX PA9`.
    pub text: String,
    /// Where the text goes, between the module and the edge.
    pub label: Rect,
    /// The point on the frame edge its wire leaves from.
    pub port: Pos2,
}

/// Where everything in one frame goes.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameLayout {
    pub size: Vec2,
    /// One rect per [`ChipView::modules`], same order.
    pub modules: Vec<Rect>,
    /// Per module: the point on its outer edge a link leaves from, and the
    /// side it is on.
    pub module_ports: Vec<(Pos2, Side)>,
    /// Per module, its shown pads (Detailed view), top-down.
    pub pins: Vec<Vec<PinRow>>,
    /// One rect per [`ChipView::devices`], same order.
    pub devices: Vec<Rect>,
    /// A device-to-module wire: the module's index, then device end to module
    /// end with the two corners between.
    pub wires: Vec<(usize, [Pos2; 4])>,
    /// Where the "nothing here" or problem text goes, when there is one.
    pub note: Option<Rect>,
}

/// Lay out one chip's frame.
pub fn frame_layout(chip: &ChipView, arrange: &Arrange) -> FrameLayout {
    let n = chip.modules.len();
    let side = |i: usize| arrange.side.get(i).copied().unwrap_or(Side::Right);
    let pins_of = |i: usize| arrange.pins.get(i).map_or(&[][..], |p| p.as_slice());
    let has = |s: Side| (0..n).any(|i| side(i) == s);
    let pinned = |s: Side| (0..n).any(|i| side(i) == s && !pins_of(i).is_empty());

    // Columns, left to right: [pad names] [modules] [devices] [modules] [pad
    // names]. The main columns keep a wire's width between them; a pad name
    // sits right against its module.
    let mut x = PAD;
    let left_pin_x = pinned(Side::Left).then(|| {
        let at = x;
        x += PIN_W;
        at
    });
    let mut column = |present: bool, w: f32| {
        present.then(|| {
            let at = x;
            x += w + COLUMN_GAP;
            at
        })
    };
    let left_mod_x = column(has(Side::Left), MODULE_W);
    let dev_x = column(!chip.devices.is_empty(), DEVICE_W);
    let right_mod_x = column(has(Side::Right), MODULE_W);
    if left_mod_x.is_some() || dev_x.is_some() || right_mod_x.is_some() {
        x -= COLUMN_GAP;
    }
    let right_pin_x = pinned(Side::Right).then(|| {
        let at = x + 8.0;
        x += PIN_W;
        at
    });
    let width = (x + PAD).max(MIN_FRAME_W);

    // Modules: each column top-down in rank order.
    let mut modules = vec![Rect::NOTHING; n];
    for s in [Side::Left, Side::Right] {
        let mut order: Vec<usize> = (0..n).filter(|&i| side(i) == s).collect();
        order.sort_by(|&a, &b| {
            let key = |i: usize| arrange.rank.get(i).copied().unwrap_or(i as f32);
            key(a).total_cmp(&key(b)).then(a.cmp(&b))
        });
        let mx = match s {
            Side::Left => left_mod_x,
            Side::Right => right_mod_x,
        }
        .unwrap_or(PAD);
        let mut y = HEADER_H;
        for i in order {
            let h = (pins_of(i).len() as f32 * PIN_ROW_H).max(PILL_H);
            modules[i] = Rect::from_min_size(pos2(mx, y), vec2(MODULE_W, h));
            y += h + ROW_GAP;
        }
    }
    let module_ports = (0..n)
        .map(|i| {
            let r = modules[i];
            match side(i) {
                Side::Left => (r.left_center(), Side::Left),
                Side::Right => (r.right_center(), Side::Right),
            }
        })
        .collect();
    let pins = (0..n)
        .map(|i| {
            let r = modules[i];
            pins_of(i)
                .iter()
                .enumerate()
                .map(|(k, (num, text))| {
                    let top = r.top() + k as f32 * PIN_ROW_H;
                    let (lx, edge) = match side(i) {
                        Side::Left => (left_pin_x.unwrap_or(PAD), 0.0),
                        Side::Right => (right_pin_x.unwrap_or(r.right()), width),
                    };
                    PinRow {
                        pin: *num,
                        text: text.clone(),
                        label: Rect::from_min_size(pos2(lx, top), vec2(PIN_W - 8.0, PIN_ROW_H)),
                        port: pos2(edge, top + PIN_ROW_H / 2.0),
                    }
                })
                .collect()
        })
        .collect();

    // Devices, level with the middle of their modules.
    let anchor = |i: usize| -> Option<f32> {
        let ms = &chip.devices[i].modules;
        (!ms.is_empty())
            .then(|| ms.iter().map(|&m| modules[m].center().y).sum::<f32>() / ms.len() as f32)
    };
    let mut order: Vec<usize> = (0..chip.devices.len()).collect();
    order.sort_by(|&a, &b| match (anchor(a), anchor(b)) {
        (Some(x), Some(y)) => x.total_cmp(&y).then(a.cmp(&b)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(&b),
    });
    let dx = dev_x.unwrap_or(PAD);
    let mut devices = vec![Rect::NOTHING; chip.devices.len()];
    let mut cursor = HEADER_H;
    for i in order {
        let want = anchor(i).map_or(cursor, |c| c - PILL_H / 2.0);
        let y = want.max(cursor);
        devices[i] = Rect::from_min_size(pos2(dx, y), vec2(DEVICE_W, PILL_H));
        cursor = y + PILL_H + ROW_GAP;
    }

    // Device-to-module wires, from the device's edge that faces the module.
    let mut wires = Vec::new();
    for (d, dev) in chip.devices.iter().enumerate() {
        for &m in &dev.modules {
            let (from, to) = match side(m) {
                Side::Right => (devices[d].right_center(), modules[m].left_center()),
                Side::Left => (devices[d].left_center(), modules[m].right_center()),
            };
            let mid = (from.x + to.x) / 2.0;
            wires.push((m, [from, pos2(mid, from.y), pos2(mid, to.y), to]));
        }
    }

    let bottom = modules
        .iter()
        .chain(devices.iter())
        .map(|r| r.bottom())
        .fold(f32::NEG_INFINITY, f32::max);
    let empty = chip.modules.is_empty() && chip.devices.is_empty();
    let (note, body_bottom) = if empty || chip.problem.is_some() {
        let top = if empty { HEADER_H } else { bottom + ROW_GAP };
        let r = Rect::from_min_size(pos2(PAD, top), vec2(width - 2.0 * PAD, NOTE_H));
        (Some(r), r.bottom())
    } else {
        (None, bottom)
    };
    FrameLayout {
        size: vec2(width, body_bottom + PAD),
        modules,
        module_ports,
        pins,
        devices,
        wires,
        note,
    }
}

/// Top-left for every frame: its saved spot, or - for one that has none yet -
/// the next spot to the right of everything already placed, in list order.
pub fn frame_positions(frames: &[(Option<(f32, f32)>, Vec2)]) -> Vec<Pos2> {
    let placed_right = frames
        .iter()
        .filter_map(|(p, size)| p.map(|(x, _)| x + size.x))
        .fold(f32::NEG_INFINITY, f32::max);
    let mut next_x = if placed_right.is_finite() {
        placed_right + FRAME_GAP
    } else {
        ORIGIN.x
    };
    frames
        .iter()
        .map(|(p, size)| match p {
            Some((x, y)) => pos2(*x, *y),
            None => {
                let at = pos2(next_x, ORIGIN.y);
                next_x += size.x + FRAME_GAP;
                at
            }
        })
        .collect()
}

/// The path of a link from `a` (leaving its frame edge towards `a_out`, +1
/// right / -1 left) to `b`, square-cornered. `lane` shifts the vertical run
/// so the wires of one link, and parallel links, do not lie on each other.
/// `frames` holds the frame `a` leaves first and the one `b` enters second,
/// then any others: a path through one of them goes around it instead.
///
/// Three shapes, by how the two edges face:
/// - facing each other with room between: across, down, across - the run in
///   the gap, never closer to either edge than a stub;
/// - facing the same way: out past the further edge, down, back;
/// - facing each other but overlapping: out, to half height, over, in.
///
/// Any of them that would cross a frame is replaced by one that goes out a
/// stub, over (or under) both end frames, and in.
pub fn route(a: Pos2, a_out: f32, b: Pos2, b_out: f32, lane: f32, frames: &[Rect]) -> Vec<Pos2> {
    let simple = direct(a, a_out, b, b_out, lane);
    let blocked = frames.iter().any(|f| {
        let f = f.shrink(2.0);
        simple.windows(2).any(|w| crosses(w[0], w[1], f))
    });
    if !blocked || frames.len() < 2 {
        return simple;
    }
    let s = STUB + lane.abs();
    let (ax, bx) = (a.x + a_out * s, b.x + b_out * s);
    let above = frames[0].top().min(frames[1].top()) - s;
    let below = frames[0].bottom().max(frames[1].bottom()) + s;
    let mid = (a.y + b.y) / 2.0;
    let y = if (mid - above).abs() <= (below - mid).abs() {
        above
    } else {
        below
    };
    tidy(vec![
        a,
        pos2(ax, a.y),
        pos2(ax, y),
        pos2(bx, y),
        pos2(bx, b.y),
        b,
    ])
}

fn direct(a: Pos2, a_out: f32, b: Pos2, b_out: f32, lane: f32) -> Vec<Pos2> {
    let gap = (b.x - a.x) * a_out;
    if a_out != b_out && gap > 2.0 * STUB {
        // However many wires share the gap, their runs stay inside it: a run
        // past an edge would turn inside a frame and point its arrow back.
        let room = gap / 2.0 - STUB / 2.0;
        let x = (a.x + b.x) / 2.0 + lane.clamp(-room, room);
        return tidy(vec![a, pos2(x, a.y), pos2(x, b.y), b]);
    }
    if a_out == b_out {
        let x = if a_out > 0.0 {
            a.x.max(b.x) + STUB + lane.abs()
        } else {
            a.x.min(b.x) - STUB - lane.abs()
        };
        return tidy(vec![a, pos2(x, a.y), pos2(x, b.y), b]);
    }
    let (ax, bx) = (
        a.x + a_out * (STUB + lane.abs()),
        b.x + b_out * (STUB + lane.abs()),
    );
    let y = (a.y + b.y) / 2.0 + lane;
    tidy(vec![
        a,
        pos2(ax, a.y),
        pos2(ax, y),
        pos2(bx, y),
        pos2(bx, b.y),
        b,
    ])
}

/// Whether the axis-aligned segment `p`-`q` passes through `r`.
fn crosses(p: Pos2, q: Pos2, r: Rect) -> bool {
    let (x0, x1) = (p.x.min(q.x), p.x.max(q.x));
    let (y0, y1) = (p.y.min(q.y), p.y.max(q.y));
    x0 < r.right() && x1 > r.left() && y0 < r.bottom() && y1 > r.top()
}

/// Frames that grew for their links (a second module column, pad names) can
/// reach into the frame beside them. Each frame, left to right, moves right
/// just far enough to clear every frame it overlaps; frames that sit above
/// or below each other are left alone. Nothing is saved: the spot the user
/// dragged a frame to is kept, and this is only where it is drawn.
pub fn separate(positions: &[Pos2], sizes: &[Vec2]) -> Vec<Pos2> {
    const CLEAR: f32 = 24.0;
    let n = positions.len().min(sizes.len());
    let mut out = positions[..n].to_vec();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| {
        positions[i]
            .x
            .total_cmp(&positions[j].x)
            .then(positions[i].y.total_cmp(&positions[j].y))
            .then(i.cmp(&j))
    });
    for k in 0..n {
        let i = order[k];
        // Bounded: each pass clears one frame, there are only so many.
        for _ in 0..n {
            let me = Rect::from_min_size(out[i], sizes[i]);
            let hit = order[..k]
                .iter()
                .map(|&j| Rect::from_min_size(out[j], sizes[j]))
                .filter(|o| {
                    me.left() < o.right() + CLEAR
                        && o.left() < me.right() + CLEAR
                        && me.top() < o.bottom() + CLEAR
                        && o.top() < me.bottom() + CLEAR
                })
                .map(|o| o.right())
                .fold(f32::NEG_INFINITY, f32::max);
            if !hit.is_finite() {
                break;
            }
            out[i].x = hit + CLEAR;
        }
    }
    out
}

/// Drop the corner points a straight run does not need, so a level link is
/// one segment and its corners stay where they are.
pub fn tidy(pts: Vec<Pos2>) -> Vec<Pos2> {
    let mut out: Vec<Pos2> = Vec::with_capacity(pts.len());
    for p in pts {
        if out.last().is_some_and(|q| (*q - p).length() < 0.5) {
            continue;
        }
        if out.len() >= 2 {
            let (u, v) = (out[out.len() - 2], out[out.len() - 1]);
            let collinear = ((v.x - u.x) * (p.y - v.y) - (v.y - u.y) * (p.x - v.x)).abs() < 0.5;
            if collinear {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::board::snapshot::{DeviceItem, ModuleItem};
    use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, UsartModuleConfig};

    fn chip(modules: &[&str], devices: &[(&str, &[usize])]) -> ChipView {
        ChipView {
            dir: "c".into(),
            chip: "C".into(),
            runtime: None,
            modules: modules
                .iter()
                .map(|n| ModuleItem {
                    name: (*n).into(),
                    kind: ModuleKind::GenericInterfaceUsart,
                    instance: 1,
                    pins: Default::default(),
                    signals: vec![],
                    config: ModuleConfig::Usart(UsartModuleConfig::new(1)),
                    not_built: None,
                })
                .collect(),
            devices: devices
                .iter()
                .map(|(n, m)| DeviceItem {
                    name: (*n).into(),
                    modules: m.to_vec(),
                })
                .collect(),
            problem: None,
            external_mv: None,
        }
    }

    fn plain(c: &ChipView) -> FrameLayout {
        frame_layout(c, &Arrange::plain(c))
    }

    fn overlaps(rs: &[Rect]) -> bool {
        rs.iter()
            .enumerate()
            .any(|(i, a)| rs[i + 1..].iter().any(|b| a.intersects(*b)))
    }

    /// A device sits level with its module when nothing is in the way, and
    /// the wire runs from one to the other.
    #[test]
    fn a_device_sits_level_with_its_module() {
        let c = chip(&["USART1", "I2C1", "SPI1"], &[("OLED", &[1])]);
        let l = plain(&c);
        assert_eq!(l.size.x, FRAME_W, "the Phase 1 frame, unchanged");
        assert_eq!(l.devices[0].center().y, l.modules[1].center().y);
        assert_eq!(l.wires.len(), 1);
        let (m, pts) = l.wires[0];
        assert_eq!(m, 1);
        assert_eq!(pts[0], l.devices[0].right_center());
        assert_eq!(pts[3], l.modules[1].left_center());
    }

    /// Two devices on one module stack instead of overlapping; a device on
    /// bare pads goes after them; nothing leaves the frame or crosses the
    /// module column.
    #[test]
    fn devices_never_overlap_and_stay_in_their_column() {
        let c = chip(
            &["USART1", "I2C1"],
            &[
                ("Status LED", &[]),
                ("OLED", &[1]),
                ("BME280", &[1]),
                ("Bridge", &[0, 1]),
            ],
        );
        let l = plain(&c);
        assert!(!overlaps(&l.devices));
        assert!(!overlaps(&l.modules));
        let frame = Rect::from_min_size(Pos2::ZERO, l.size);
        let module_x = l.modules[0].left();
        for r in l.devices.iter().chain(&l.modules) {
            assert!(frame.contains_rect(*r), "{r:?} outside {frame:?}");
            assert!(r.top() >= HEADER_H);
        }
        for d in &l.devices {
            assert!(d.right() < module_x);
        }
        let led = l.devices[0];
        assert!(
            l.devices[1..].iter().all(|d| d.top() < led.top()),
            "bare-pad device last"
        );
        assert_eq!(l.wires.len(), 4, "Bridge draws one wire per module");
    }

    /// An empty chip, and a chip that could not be read, still get a body with
    /// room for one line saying so.
    #[test]
    fn an_empty_or_broken_chip_keeps_a_note_row() {
        let empty = plain(&chip(&[], &[]));
        let note = empty.note.unwrap();
        assert!(note.top() >= HEADER_H);
        assert_eq!(empty.size.y, note.bottom() + PAD);
        let broken = plain(&ChipView::broken("x", "Folder not found"));
        assert!(broken.note.is_some());
        let mut half = chip(&["USART1"], &[]);
        half.problem = Some("Unknown chip".into());
        let l = plain(&half);
        assert!(l.note.unwrap().top() > l.modules[0].bottom());
    }

    /// Modules split to the side of their partners, each column in rank order;
    /// devices sit between them and wire to whichever side their module is on.
    #[test]
    fn modules_face_their_partners_and_devices_sit_between() {
        let c = chip(&["USART1", "I2C1", "SPI1"], &[("OLED", &[0])]);
        let arrange = Arrange {
            side: vec![Side::Left, Side::Right, Side::Right],
            rank: vec![0.0, 5.0, 1.0],
            pins: vec![vec![], vec![], vec![]],
        };
        let l = frame_layout(&c, &arrange);
        let (usart, i2c, spi, oled) = (l.modules[0], l.modules[1], l.modules[2], l.devices[0]);
        assert!(usart.right() < oled.left() && oled.right() < i2c.left());
        assert!(spi.top() < i2c.top(), "rank 1 above rank 5");
        assert_eq!(l.module_ports[0], (usart.left_center(), Side::Left));
        assert_eq!(l.module_ports[2], (spi.right_center(), Side::Right));
        let (_, pts) = l.wires[0];
        assert_eq!((pts[0], pts[3]), (oled.left_center(), usart.right_center()));
        assert!(Rect::from_min_size(Pos2::ZERO, l.size).contains_rect(i2c));
    }

    /// Detailed: a linked module grows a row per pad, and each pad's wire
    /// leaves from the frame edge on its side, level with its row.
    #[test]
    fn detailed_pins_sit_at_the_frame_edge() {
        let c = chip(&["USART1", "SPI1"], &[]);
        let arrange = Arrange {
            side: vec![Side::Right, Side::Left],
            rank: vec![0.0, 0.0],
            pins: vec![
                vec![(30, "TX PA9".into()), (31, "RX PA10".into())],
                vec![(5, "SCK PA5".into())],
            ],
        };
        let l = frame_layout(&c, &arrange);
        assert_eq!(l.modules[0].height(), 2.0 * PIN_ROW_H);
        let rows = &l.pins[0];
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "TX PA9");
        for row in rows {
            assert_eq!(row.port.x, l.size.x, "right edge");
            assert!(row.label.left() >= l.modules[0].right());
            assert!(row.label.right() <= l.size.x);
            assert!((row.label.center().y - row.port.y).abs() < 0.01);
            assert!([30, 31].contains(&row.pin));
        }
        assert_eq!(l.pins[1][0].port.x, 0.0, "left edge");
        assert!(l.pins[1][0].label.right() <= l.modules[1].left());
    }

    /// Unplaced frames line up to the right of the placed ones, in order, never
    /// on top of each other.
    #[test]
    fn frames_without_a_spot_go_right_of_the_placed_ones() {
        let size = vec2(FRAME_W, 200.0);
        let pos = frame_positions(&[(None, size), (Some((100.0, 300.0)), size), (None, size)]);
        assert_eq!(pos[1], pos2(100.0, 300.0));
        let first_free = 100.0 + FRAME_W + FRAME_GAP;
        assert_eq!(pos[0], pos2(first_free, ORIGIN.y));
        assert_eq!(pos[2], pos2(first_free + FRAME_W + FRAME_GAP, ORIGIN.y));
        assert_eq!(frame_positions(&[(None, size)])[0], ORIGIN);
    }

    /// Every path is square-cornered, starts and ends on its ports, and leaves
    /// each port the way that port faces.
    #[test]
    fn a_link_leaves_each_frame_the_way_its_edge_faces() {
        let cases = [
            // Facing, with room: across, down, across.
            (pos2(300.0, 100.0), 1.0, pos2(500.0, 180.0), -1.0),
            // Level: one straight segment.
            (pos2(300.0, 100.0), 1.0, pos2(500.0, 100.0), -1.0),
            // Both facing right.
            (pos2(300.0, 100.0), 1.0, pos2(250.0, 300.0), 1.0),
            // Facing, but overlapping.
            (pos2(300.0, 100.0), 1.0, pos2(280.0, 300.0), -1.0),
        ];
        for (a, ao, b, bo) in cases {
            let p = route(a, ao, b, bo, 4.0, &[]);
            assert_eq!((p[0], *p.last().unwrap()), (a, b));
            for w in p.windows(2) {
                assert!(w[0].x == w[1].x || w[0].y == w[1].y, "not square: {p:?}");
            }
            assert!(
                (p[1].x - a.x) * ao > 0.0 || p.len() == 2,
                "leaves a backwards: {p:?}"
            );
            let n = p.len();
            assert!(
                (p[n - 2].x - b.x) * bo > 0.0 || n == 2,
                "enters b backwards: {p:?}"
            );
        }
        assert_eq!(
            route(pos2(300.0, 100.0), 1.0, pos2(500.0, 100.0), -1.0, 0.0, &[]).len(),
            2
        );
    }

    /// A link from the middle frame to the one on its left, leaving by the
    /// middle frame's right edge, goes around that frame - not through it.
    #[test]
    fn a_link_goes_around_a_frame_in_its_way() {
        let left = Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, 200.0));
        let mid = Rect::from_min_size(pos2(400.0, 0.0), vec2(300.0, 200.0));
        let a = pos2(mid.right(), 100.0);
        let b = pos2(left.right(), 120.0);
        let p = route(a, 1.0, b, 1.0, 0.0, &[mid, left]);
        assert_eq!((p[0], *p.last().unwrap()), (a, b));
        for w in p.windows(2) {
            assert!(
                !crosses(w[0], w[1], mid.shrink(2.0)),
                "through the middle frame: {p:?}"
            );
            assert!(w[0].x == w[1].x || w[0].y == w[1].y, "not square: {p:?}");
        }
    }

    /// Many wires in a narrow gap still turn inside the gap.
    #[test]
    fn wide_lanes_stay_between_the_frames() {
        let (a, b) = (pos2(300.0, 100.0), pos2(360.0, 200.0));
        for lane in [-40.0, 40.0] {
            let p = route(a, 1.0, b, -1.0, lane, &[]);
            assert!(p[1].x > a.x && p[1].x < b.x, "{p:?}");
        }
    }

    /// A frame that grew into its neighbour pushes it right; frames one above
    /// the other stay where they are.
    #[test]
    fn grown_frames_push_their_neighbours_right() {
        let pos = [pos2(0.0, 0.0), pos2(350.0, 50.0), pos2(0.0, 400.0)];
        let sizes = [vec2(420.0, 200.0), vec2(300.0, 200.0), vec2(300.0, 100.0)];
        let out = separate(&pos, &sizes);
        assert_eq!(out[0], pos[0]);
        assert!(out[1].x >= 420.0, "{out:?}");
        assert_eq!(out[1].y, 50.0);
        assert_eq!(out[2], pos[2], "below, not beside");
        let fine = separate(
            &[pos2(0.0, 0.0), pos2(500.0, 0.0)],
            &[vec2(300.0, 100.0); 2],
        );
        assert_eq!(fine, [pos2(0.0, 0.0), pos2(500.0, 0.0)]);
    }
}
