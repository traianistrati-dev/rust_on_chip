//! Where every pin sits — the single source of pin geometry.
//!
//! This used to be computed three times, independently, each starting from the
//! four side vectors: once for the un-rotated renderer, once for the rotated
//! one, and once more for the anchors that module wires and I/O arrows attach
//! to. Three copies of one formula is three places to change for any new
//! package shape — and the reason a ball-grid layout (WLCSP, BGA) could not be
//! added cheaply. They all read this module now.
//!
//! Everything here is in the chip's LOCAL, un-rotated frame. Rotation is a
//! transform applied afterwards by [`super::rotate::Rot`], which is what keeps
//! rotation from having an opinion about layout.

use super::super::model::{GridCell, Mcu, PIN_HEIGHT, PIN_SPACING, PIN_WIDTH, PinGrid};
use crate::panels::mcu_module::pins::logic::pin::Pin;
use eframe::egui;

/// Inset from the chip edge for the pin number drawn inside the body.
pub const NUM_MARGIN: f32 = 4.0;

/// Which edge of the chip BODY a pin sits on, in the model's own frame.
///
/// Not [`super::rotate::ScreenSide`], which is where a pin ends up *on screen*
/// after rotation — the two coincide only at 0°.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PinSide {
    Right,
    Left,
    Top,
    Bottom,
}

/// Where a pin physically is on the package.
#[derive(Clone, PartialEq, Debug)]
pub enum PinPlace {
    /// A stub sticking out of one edge — QFP, DIP, QFN.
    Edge(PinSide),
    /// A ball UNDER the die, at a grid cell — WLCSP, BGA. Carries its datasheet
    /// designator ("A2"), which takes the place of the pin number on the drawing
    /// because that is what the package is labelled with.
    Ball { designator: String },
}

/// Ball diameter, and the pitch between ball centres. Smaller than an edge pin
/// so a 4x6 grid fits inside a body sized for its widest row.
pub const BALL_D: f32 = 34.0;
pub const BALL_PITCH: f32 = BALL_D + 12.0;

/// One pin's placement: everything any renderer needs, computed once.
pub struct PinGeom<'a> {
    pub pin: &'a Pin,
    pub place: PinPlace,
    /// The pin stub. Its `min` is the origin the per-side `Pin::draw_*` take.
    pub rect: egui::Rect,
    /// Unit vector pointing away from the chip body.
    pub outward: egui::Vec2,
    /// Where the pin number is drawn, just inside the body edge.
    pub num_pos: egui::Pos2,
    /// Alignment for that number, so it hugs its edge.
    pub num_align: egui::Align2,
}

impl PinGeom<'_> {
    /// The OUTER end of the stub — where a module wire or an I/O arrow attaches.
    ///
    /// A ball has no stub to stick out of, so its attachment point is the ball
    /// itself; a wire to it will cross the package, which is what a ball-grid
    /// schematic actually looks like.
    pub fn anchor(&self) -> egui::Pos2 {
        match self.place {
            PinPlace::Edge(_) => self.rect.center() + self.outward * (PIN_HEIGHT / 2.0),
            PinPlace::Ball { .. } => self.rect.center(),
        }
    }

    /// The edge a pin sits on, or `None` for a ball.
    pub fn side(&self) -> Option<PinSide> {
        match self.place {
            PinPlace::Edge(s) => Some(s),
            PinPlace::Ball { .. } => None,
        }
    }
}

/// Position along an edge for the `i`-th pin on it.
fn offset(i: usize) -> f32 {
    PIN_SPACING + i as f32 * (PIN_WIDTH + PIN_SPACING)
}

/// Blank slots kept at each end of a BOARD's top/bottom row.
///
/// The pads there are not on the header — they are chip pins the PCB routes
/// elsewhere — and butting them against the board edge read as though they
/// continued the numbered header. Two empty widths each side says "these are
/// somewhere else on the board" without a word of explanation.
pub const BOARD_EDGE_PAD: usize = 2;

/// Blank slots kept at each end of the top and bottom rows' body span.
///
/// A board pads; a bare chip does not. Read from the same field the green fill
/// and the chip square read, so the three cannot disagree about what a board is.
/// Where each row actually starts is [`row_start`], which on a board also
/// centers the shorter row.
pub fn top_pad(mcu: &Mcu) -> usize {
    if mcu.board_chip.is_some() {
        BOARD_EDGE_PAD
    } else {
        0
    }
}

/// Least body height of a BOARD, in pin slots.
///
/// A board with no pins down its sides (the micro:bit's edge connector) would
/// otherwise take the bare-chip floor of three slots, and the chip square drawn
/// above the name would not fit between the two rows of pin numbers.
const BOARD_MIN_ROWS: usize = 8;

/// The body's size in pin slots: `(across, down)`.
///
/// Each is the LONGER of its two opposite rows, so neither row runs off the
/// body when a board carries more pads along one edge than the other.
fn body_slots(mcu: &Mcu) -> (usize, usize) {
    let across = mcu.top_pins.len().max(mcu.bottom_pins.len());
    let down = mcu.left_pins.len().max(mcu.right_pins.len());
    if mcu.board_chip.is_some() {
        (across, down.max(BOARD_MIN_ROWS))
    } else {
        (across, down)
    }
}

/// How many slots in the top or bottom row starts.
///
/// On a board, the shorter of the two rows is also centered under the longer
/// one, in whole slots so the two rows keep one column grid. A bare chip is
/// never shifted: its pins run on around the corners, and centering one edge
/// would break that.
fn row_start(mcu: &Mcu, len: usize) -> usize {
    let pad = top_pad(mcu);
    if pad == 0 {
        return 0;
    }
    pad + (body_slots(mcu).0 - len) / 2
}

/// Body and canvas size of an edge-pin package: `(body_w, body_h, canvas_w,
/// canvas_h)`.
///
/// The one place the body is sized from the pins, so `Mcu::draw` and the tests
/// that check pins and the chip square against the body cannot size it apart.
pub fn body_layout(mcu: &Mcu) -> (f32, f32, f32, f32) {
    let (across, down) = body_slots(mcu);
    super::layout::calculate_layout(across, down, top_pad(mcu))
}

/// Geometry of the `i`-th pin on `side`. `start` is the slot a top or bottom
/// row begins at ([`row_start`]); it is ignored on the left and right edges.
fn geom<'a>(side: PinSide, i: usize, pin: &'a Pin, chip: egui::Rect, start: usize) -> PinGeom<'a> {
    let (rect, outward, num_pos, num_align) = match side {
        PinSide::Right => {
            let y = chip.top() + offset(i);
            (
                egui::Rect::from_min_size(
                    egui::pos2(chip.right(), y),
                    egui::vec2(PIN_HEIGHT, PIN_WIDTH),
                ),
                egui::vec2(1.0, 0.0),
                egui::pos2(chip.right() - NUM_MARGIN, y + PIN_WIDTH / 2.0),
                egui::Align2::RIGHT_CENTER,
            )
        }
        PinSide::Left => {
            let y = chip.top() + offset(i);
            (
                egui::Rect::from_min_size(
                    egui::pos2(chip.left() - PIN_HEIGHT, y),
                    egui::vec2(PIN_HEIGHT, PIN_WIDTH),
                ),
                egui::vec2(-1.0, 0.0),
                egui::pos2(chip.left() + NUM_MARGIN, y + PIN_WIDTH / 2.0),
                egui::Align2::LEFT_CENTER,
            )
        }
        PinSide::Top => {
            let x = chip.left() + offset(i + start);
            (
                egui::Rect::from_min_size(
                    egui::pos2(x, chip.top() - PIN_HEIGHT),
                    egui::vec2(PIN_WIDTH, PIN_HEIGHT),
                ),
                egui::vec2(0.0, -1.0),
                egui::pos2(x + PIN_WIDTH / 2.0, chip.top() + NUM_MARGIN),
                egui::Align2::CENTER_TOP,
            )
        }
        PinSide::Bottom => {
            let x = chip.left() + offset(i + start);
            (
                egui::Rect::from_min_size(
                    egui::pos2(x, chip.bottom()),
                    egui::vec2(PIN_WIDTH, PIN_HEIGHT),
                ),
                egui::vec2(0.0, 1.0),
                egui::pos2(x + PIN_WIDTH / 2.0, chip.bottom() - NUM_MARGIN),
                egui::Align2::CENTER_BOTTOM,
            )
        }
    };
    PinGeom {
        pin,
        place: PinPlace::Edge(side),
        rect,
        outward,
        num_pos,
        num_align,
    }
}

/// Geometry of one ball, centred in the grid inside the chip body.
///
/// `outward` points from the body centre to the ball — the direction a wire
/// leaves in. A ball exactly at the centre gets a downward default rather than
/// a zero vector, which would make every consumer divide by zero.
fn ball_geom<'a>(cell: &'a GridCell, grid: &PinGrid, chip: egui::Rect) -> PinGeom<'a> {
    let span = |n: usize| (n.saturating_sub(1)) as f32 * BALL_PITCH;
    let origin = egui::pos2(
        chip.center().x - span(grid.cols) / 2.0,
        chip.center().y - span(grid.rows) / 2.0,
    );
    let center = origin + egui::vec2(cell.col as f32 * BALL_PITCH, cell.row as f32 * BALL_PITCH);
    let outward = {
        let v = center - chip.center();
        if v.length() < 0.5 {
            egui::vec2(0.0, 1.0)
        } else {
            v.normalized()
        }
    };
    PinGeom {
        pin: &cell.pin,
        place: PinPlace::Ball {
            designator: cell.designator(),
        },
        rect: egui::Rect::from_center_size(center, egui::vec2(BALL_D, BALL_D)),
        outward,
        // The designator sits just under the ball, where the datasheet puts it.
        num_pos: center + egui::vec2(0.0, BALL_D / 2.0 + 2.0),
        num_align: egui::Align2::CENTER_TOP,
    }
}

/// Every pin with its placement, in the order the renderers draw them
/// (right, left, top, bottom — kept because the last hit under the pointer
/// wins a click, and reordering would silently change which pin that is).
///
/// An iterator, not a `Vec`: the per-pin anchor lookup runs inside per-pin
/// loops, and allocating a vector for each of those would make a cheap query
/// quadratic.
pub fn pin_geometry(mcu: &Mcu, chip: egui::Rect) -> impl Iterator<Item = PinGeom<'_>> {
    let top_start = row_start(mcu, mcu.top_pins.len());
    let bottom_start = row_start(mcu, mcu.bottom_pins.len());
    let right = mcu
        .right_pins
        .iter()
        .enumerate()
        .map(move |(i, p)| geom(PinSide::Right, i, p, chip, 0));
    let left = mcu
        .left_pins
        .iter()
        .enumerate()
        .map(move |(i, p)| geom(PinSide::Left, i, p, chip, 0));
    let top = mcu
        .top_pins
        .iter()
        .enumerate()
        .map(move |(i, p)| geom(PinSide::Top, i, p, chip, top_start));
    let bottom = mcu
        .bottom_pins
        .iter()
        .enumerate()
        .map(move |(i, p)| geom(PinSide::Bottom, i, p, chip, bottom_start));
    // Balls last: they are drawn INSIDE the body, over it.
    let balls = mcu
        .grid
        .iter()
        .flat_map(move |g| g.cells.iter().map(move |c| ball_geom(c, g, chip)));
    right.chain(left).chain(top).chain(bottom).chain(balls)
}

/// Size the chip body must have to hold `grid` with a margin around the balls.
pub fn grid_body_size(grid: &PinGrid) -> egui::Vec2 {
    let span = |n: usize| (n.saturating_sub(1)) as f32 * BALL_PITCH + BALL_D;
    egui::vec2(span(grid.cols) + BALL_PITCH, span(grid.rows) + BALL_PITCH)
}

/// Placement of one pin by number. `None` if it isn't on this chip.
pub fn pin_geom(mcu: &Mcu, chip: egui::Rect, number: usize) -> Option<PinGeom<'_>> {
    pin_geometry(mcu, chip).find(|g| g.pin.number == number)
}

#[cfg(test)]
mod board_layout {
    use super::*;
    use crate::panels::mcu_module::builtins;

    fn board(id: &str) -> Mcu {
        builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu()
    }

    /// The four Pico definitions are BOARDS, and say which chip they carry.
    ///
    /// One field decides three things that are easy to let drift apart: the
    /// green fill, the padded top row, and the square with the part number.
    #[test]
    fn the_pico_definitions_name_their_chip() {
        for (id, part) in [
            ("rp2040_pico", "RP2040"),
            ("rp2040_pico_w", "RP2040"),
            ("rp2350_pico2", "RP2350"),
            ("rp2350_pico2_w", "RP2350"),
            // The B package: 48 GPIOs, and a different embassy-rp feature.
            ("rp2350_pico2_ice", "RP2350B"),
        ] {
            assert_eq!(board(id).board_chip.as_deref(), Some(part), "{id}");
        }
        // A bare part stays a bare part, or every chip turns green.
        assert_eq!(board("stm32f103c8t6").board_chip, None);
    }

    /// A board's top row starts two slots in, at BOTH ends.
    ///
    /// `_ _ 41 42 43 44 45 _ _` — the pads there are not on the header, and
    /// running them to the board edge read as though they continued it.
    #[test]
    fn a_boards_top_row_is_inset_at_both_ends() {
        let mcu = board("rp2350_pico2_w");
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, 900.0));
        assert_eq!(top_pad(&mcu), BOARD_EDGE_PAD);

        let tops: Vec<f32> = pin_geometry(&mcu, rect)
            .filter(|g| g.side() == Some(PinSide::Top))
            .map(|g| g.rect.left())
            .collect();
        assert!(!tops.is_empty(), "the W board has off-header pads");
        let first = tops.iter().cloned().fold(f32::MAX, f32::min);
        assert!(
            (first - (rect.left() + offset(BOARD_EDGE_PAD))).abs() < 0.01,
            "first top pad sits {BOARD_EDGE_PAD} slots in, not at {first}"
        );

        // A bare chip is NOT inset — the padding is a property of boards.
        let chip = board("stm32f103c8t6");
        assert_eq!(top_pad(&chip), 0);
    }

    /// The body a board is drawn with, sized by the same call `Mcu::draw` makes.
    fn body(mcu: &Mcu) -> egui::Rect {
        let (w, h, ..) = body_layout(mcu);
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(w, h))
    }

    /// Every top and bottom pad of every board lies within the body's width,
    /// with the edge padding kept at both ends of the longer row.
    ///
    /// The micro:bit carries 25 pads on its edge connector and 14 on top; the
    /// body used to be sized from the top row alone, so the connector ran off it.
    #[test]
    fn a_boards_longer_row_sets_its_width() {
        let boards: Vec<Mcu> = builtins::builtin_definitions()
            .into_iter()
            .filter(|d| d.board_chip.is_some())
            .map(|d| d.build_mcu())
            .collect();
        assert!(boards.len() >= 5, "the Picos and the micro:bit");
        for mcu in &boards {
            let id = &mcu.name;
            let rect = body(mcu);
            // The body holds `n + 2 * pad` slots plus a PIN_SPACING at each
            // end, and a row of `n` pads ends one PIN_SPACING short of its
            // last slot, so the padded row's right margin is one spacing wider
            // than its left.
            let min_left = rect.left() + offset(BOARD_EDGE_PAD) - 0.01;
            let max_right = rect.right() - offset(BOARD_EDGE_PAD) - PIN_SPACING + 0.01;
            for g in pin_geometry(mcu, rect) {
                if matches!(g.side(), Some(PinSide::Top | PinSide::Bottom)) {
                    assert!(
                        g.rect.left() >= min_left && g.rect.right() <= max_right,
                        "{id}: pin {} at {:?} is outside {rect:?}",
                        g.pin.number,
                        g.rect
                    );
                }
            }
        }
    }

    /// The shorter row is centered under the longer one, on the same columns.
    #[test]
    fn a_boards_shorter_row_is_centered() {
        let mcu = board("nrf52833_microbit_v2");
        let rect = body(&mcu);
        let span = |side: PinSide| {
            let xs: Vec<f32> = pin_geometry(&mcu, rect)
                .filter(|g| g.side() == Some(side))
                .map(|g| g.rect.center().x)
                .collect();
            let lo = xs.iter().cloned().fold(f32::MAX, f32::min);
            let hi = xs.iter().cloned().fold(f32::MIN, f32::max);
            (lo, hi)
        };
        let (top, bottom) = (span(PinSide::Top), span(PinSide::Bottom));
        let slot = PIN_WIDTH + PIN_SPACING;
        // Within half a slot of centered, since shifts are whole slots.
        let off_center = ((top.0 + top.1) - (bottom.0 + bottom.1)) / 2.0;
        assert!(off_center.abs() <= slot / 2.0 + 0.01, "{off_center}");
        // And on the bottom row's column grid.
        let cols = (top.0 - bottom.0) / slot;
        assert!((cols - cols.round()).abs() < 0.01, "{cols}");
    }

    /// And the body grows by the padding, rather than squeezing the pins.
    #[test]
    fn the_body_carries_the_padding() {
        use super::super::layout::calculate_layout;
        let (bare, ..) = calculate_layout(5, 20, 0);
        let (padded, ..) = calculate_layout(5, 20, BOARD_EDGE_PAD);
        let slot = PIN_WIDTH + PIN_SPACING;
        assert!(
            (padded - bare - 2.0 * BOARD_EDGE_PAD as f32 * slot).abs() < 0.01,
            "the board is {} wider, want {} slots",
            padded - bare,
            2 * BOARD_EDGE_PAD
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::create_stm32f103c8tx;

    fn chip() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(300.0, 400.0))
    }

    /// Every pin is described exactly once, and the draw order is the one a
    /// click depends on.
    #[test]
    fn every_pin_appears_once_in_draw_order() {
        let mcu = create_stm32f103c8tx();
        let geoms: Vec<_> = pin_geometry(&mcu, chip()).collect();
        assert_eq!(geoms.len(), mcu.iter_all_pins().count());

        let sides: Vec<PinSide> = geoms.iter().filter_map(|g| g.side()).collect();
        let first_of = |s: PinSide| sides.iter().position(|x| *x == s);
        // right, then left, then top, then bottom — the historical order.
        assert!(first_of(PinSide::Right) < first_of(PinSide::Left));
        assert!(first_of(PinSide::Left) < first_of(PinSide::Top));
        assert!(first_of(PinSide::Top) < first_of(PinSide::Bottom));
    }

    /// The formulas the three old copies used, restated as a test so the
    /// unification can't drift the diagram by a pixel.
    #[test]
    fn stub_rects_match_the_historical_formulas() {
        let mcu = create_stm32f103c8tx();
        let c = chip();
        let pitch = PIN_WIDTH + PIN_SPACING;

        let right0 = pin_geometry(&mcu, c)
            .find(|g| g.side() == Some(PinSide::Right))
            .unwrap();
        assert_eq!(
            right0.rect.min,
            egui::pos2(c.right(), c.top() + PIN_SPACING)
        );
        assert_eq!(right0.rect.size(), egui::vec2(PIN_HEIGHT, PIN_WIDTH));

        let left1 = pin_geometry(&mcu, c)
            .filter(|g| g.side() == Some(PinSide::Left))
            .nth(1)
            .unwrap();
        assert_eq!(
            left1.rect.min,
            egui::pos2(c.left() - PIN_HEIGHT, c.top() + PIN_SPACING + pitch)
        );

        let top0 = pin_geometry(&mcu, c)
            .find(|g| g.side() == Some(PinSide::Top))
            .unwrap();
        assert_eq!(
            top0.rect.min,
            egui::pos2(c.left() + PIN_SPACING, c.top() - PIN_HEIGHT)
        );
        assert_eq!(top0.rect.size(), egui::vec2(PIN_WIDTH, PIN_HEIGHT));

        let bottom0 = pin_geometry(&mcu, c)
            .find(|g| g.side() == Some(PinSide::Bottom))
            .unwrap();
        assert_eq!(
            bottom0.rect.min,
            egui::pos2(c.left() + PIN_SPACING, c.bottom())
        );
    }

    /// `anchor()` must reproduce what `pin_anchor_local` returned: the outer end
    /// of the stub, centred across it. Module wires and I/O arrows land there.
    #[test]
    fn anchor_is_the_outer_end_of_the_stub() {
        let mcu = create_stm32f103c8tx();
        let c = chip();
        for g in pin_geometry(&mcu, c) {
            let expected = match g.side().expect("edge pin") {
                PinSide::Right => egui::pos2(c.right() + PIN_HEIGHT, g.rect.center().y),
                PinSide::Left => egui::pos2(c.left() - PIN_HEIGHT, g.rect.center().y),
                PinSide::Top => egui::pos2(g.rect.center().x, c.top() - PIN_HEIGHT),
                PinSide::Bottom => egui::pos2(g.rect.center().x, c.bottom() + PIN_HEIGHT),
            };
            assert_eq!(g.anchor(), expected, "pin {}", g.pin.number);
        }
    }

    // ── Ball grid ────────────────────────────────────────────────────────────

    /// The WLCSP12 ballout of Figure 4: 12 balls in a 4x6 staggered grid.
    fn wlcsp12() -> Mcu {
        use crate::panels::mcu_module::mcu::model::{GridCell, PinGrid};
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let cells = [
            (0, 1, "PB6"),
            (0, 3, "PC15"),
            (1, 0, "PA13"),
            (1, 2, "PC14"),
            (2, 1, "PA14"),
            (2, 3, "VDD"),
            (3, 0, "PA11/PA8"),
            (3, 2, "PB7"),
            (4, 1, "PA12/PA7"),
            (4, 3, "VSS"),
            (5, 0, "PA3/PA4/PA5/PA6"),
            (5, 2, "NRST/PA0/PA1/PA2"),
        ];
        let mut mcu = Mcu::new(
            "STM32C011D6Yx".into(),
            "stm32c0".into(),
            crate::panels::mcu_module::mcu_catalog::ToolchainKind::RustEmbedded,
            vec![],
            vec![],
            vec![],
            vec![],
        );
        mcu.grid = Some(PinGrid {
            rows: 6,
            cols: 4,
            cells: cells
                .iter()
                .enumerate()
                .map(|(i, (row, col, name))| GridCell {
                    row: *row,
                    col: *col,
                    pin: Pin {
                        number: i + 1,
                        name: (*name).to_owned(),
                        reserved: false,
                        available_functions: vec![PinFunction::GpioInput],
                        selected_function: PinFunction::Unset,
                        custom_label: String::new(),
                        irq: None,
                        irq_priority: Default::default(),
                        io_mode: None,
                        af: Vec::new(),
                        fn_owner: Vec::new(),
                        note: String::new(),
                    },
                })
                .collect(),
        });
        mcu
    }

    /// Balls are pins like any other to everything upstream of the drawing —
    /// which is what lets autowire, codegen and persistence ignore the package.
    #[test]
    fn grid_pins_are_reachable_through_the_normal_iterator() {
        let mcu = wlcsp12();
        assert_eq!(mcu.iter_all_pins().count(), 12);
        assert!(mcu.find_pin(1).is_some_and(|p| p.name == "PB6"));
        assert_eq!(pin_geometry(&mcu, chip()).count(), 12);
    }

    /// The staggered pattern of the datasheet, restated: row A holds columns 2
    /// and 4, row B columns 1 and 3 — so consecutive rows must not share an x.
    #[test]
    fn balls_land_on_their_grid_cell() {
        let mcu = wlcsp12();
        let c = chip();
        let by_name = |n: &str| {
            pin_geometry(&mcu, c)
                .find(|g| g.pin.name == n)
                .expect("ball exists")
                .rect
                .center()
        };
        // Same row (A) → same y, one pitch times two apart in x (cols 2 and 4).
        let (pb6, pc15) = (by_name("PB6"), by_name("PC15"));
        assert!((pb6.y - pc15.y).abs() < 0.01);
        assert!((pc15.x - pb6.x - 2.0 * BALL_PITCH).abs() < 0.01);
        // Next row down (B) → one pitch lower, and staggered in x.
        let pa13 = by_name("PA13");
        assert!((pa13.y - pb6.y - BALL_PITCH).abs() < 0.01);
        assert!((pa13.x - pb6.x).abs() > 1.0, "rows A and B must not align");
        // The grid is centred in the body.
        let xs: Vec<f32> = pin_geometry(&mcu, c).map(|g| g.rect.center().x).collect();
        let mid = (xs.iter().cloned().fold(f32::MAX, f32::min)
            + xs.iter().cloned().fold(f32::MIN, f32::max))
            / 2.0;
        assert!((mid - c.center().x).abs() < 0.01, "centred horizontally");
    }

    /// JEDEC row letters, and the column origin at 1 — the label a datasheet
    /// uses, and the only way to find a ball on a real package.
    #[test]
    fn designators_follow_the_datasheet() {
        let mcu = wlcsp12();
        let d = |n: &str| match pin_geometry(&mcu, chip())
            .find(|g| g.pin.name == n)
            .unwrap()
            .place
        {
            PinPlace::Ball { designator } => designator,
            PinPlace::Edge(_) => unreachable!("this chip has no edge pins"),
        };
        assert_eq!(d("PB6"), "A2");
        assert_eq!(d("PC15"), "A4");
        assert_eq!(d("PA13"), "B1");
        assert_eq!(d("NRST/PA0/PA1/PA2"), "F3");

        use crate::panels::mcu_module::mcu::model::{parse_designator, row_letter};
        // I, O, Q, S, X, Z are skipped — they read as 1, 0 and each other.
        assert_eq!(row_letter(7), "H");
        assert_eq!(row_letter(8), "J", "I is skipped");
        assert_eq!(row_letter(12), "N");
        assert_eq!(row_letter(13), "P", "O is skipped");

        // The XML importer parses designators back into cells, so the two
        // directions must agree — for every row a big BGA can reach.
        for row in 0..60_usize {
            for col in 0..24_usize {
                let text = format!("{}{}", row_letter(row), col + 1);
                assert_eq!(
                    parse_designator(&text),
                    Some((row, col)),
                    "{text} must round-trip"
                );
            }
        }
        // A plain pin number is not a designator, and neither is junk.
        assert_eq!(parse_designator("12"), None);
        assert_eq!(parse_designator(""), None);
        assert_eq!(parse_designator("I3"), None, "I is not a JEDEC row");
        assert_eq!(parse_designator("A0"), None, "columns start at 1");
    }

    /// A ball has no stub, so a wire attaches to the ball itself, and `side()`
    /// reports that it is on no edge at all.
    #[test]
    fn a_ball_anchors_on_itself_and_belongs_to_no_edge() {
        let mcu = wlcsp12();
        let g = pin_geometry(&mcu, chip()).next().unwrap();
        assert_eq!(g.anchor(), g.rect.center());
        assert_eq!(g.side(), None);
        // …and it points away from the middle, for wires to leave along.
        assert!(g.outward.length() > 0.9);
    }

    #[test]
    fn the_body_is_sized_to_hold_the_grid() {
        let mcu = wlcsp12();
        let grid = mcu.grid.as_ref().unwrap();
        let size = grid_body_size(grid);
        // Every ball must fit inside a body of that size, centred on it.
        let body = egui::Rect::from_center_size(egui::pos2(0.0, 0.0), size);
        for g in pin_geometry(&mcu, body) {
            assert!(
                body.contains_rect(g.rect),
                "{} spills out of the package",
                g.pin.name
            );
        }
    }

    #[test]
    fn lookup_by_number_finds_the_same_geometry() {
        let mcu = create_stm32f103c8tx();
        let c = chip();
        let num = mcu.iter_all_pins().next().unwrap().number;
        let byiter = pin_geometry(&mcu, c).find(|g| g.pin.number == num).unwrap();
        let bynum = pin_geom(&mcu, c, num).expect("pin exists");
        assert_eq!(byiter.rect, bynum.rect);
        assert_eq!(byiter.outward, bynum.outward);
        assert!(pin_geom(&mcu, c, 99_999).is_none());
    }
}
