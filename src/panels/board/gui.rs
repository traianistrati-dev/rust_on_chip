//! The Board canvas: one frame per chip, dragged by the user, with its modules
//! and devices arranged inside by [`super::layout`], and the links between
//! the chips on top.
//!
//! Drawn inside an `egui::Scene`, so every coordinate here is a scene
//! coordinate - including `drag_delta`, which egui already scales by the zoom.
//! Everything drawn is decided by the caller; this module paints it and reports
//! what the user did.

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use egui_phosphor::regular as ph;

use super::layout::{self, Arrange, FrameLayout, PAD, PILL_H, Side};
use super::snapshot::ChipView;
use crate::panels::mcu_module::mcu::gui::modules::module_color;

/// One frame to draw.
pub struct Frame<'a> {
    pub view: &'a ChipView,
    /// Top-left, in scene coordinates.
    pub pos: Pos2,
    /// The chip open in this window.
    pub active: bool,
    /// An external part, not a chip project: drawn dashed, edited instead of
    /// opened.
    pub external: bool,
    pub layout: FrameLayout,
    /// While a link is being made: the module it starts from…
    pub armed: Option<usize>,
    /// …and, per module, whether it cannot be its other end.
    pub dimmed: Vec<bool>,
}

impl<'a> Frame<'a> {
    /// A frame with no links: every module on the right, nothing armed.
    pub fn plain(view: &'a ChipView, pos: Pos2, active: bool) -> Self {
        Self {
            external: view.external_mv.is_some(),
            view,
            pos,
            active,
            layout: layout::frame_layout(view, &Arrange::plain(view)),
            armed: None,
            dimmed: Vec::new(),
        }
    }
}

/// One drawn line of a link: the whole link (Abstract) or one of its wires.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkShape {
    /// Index of the link in the system's list.
    pub link: usize,
    pub points: Vec<Pos2>,
    pub color: Color32,
    pub width: f32,
    /// An arrowhead where the line ends / starts.
    pub arrow_end: bool,
    pub arrow_start: bool,
    /// The link picked in the list.
    pub selected: bool,
}

/// The badge on a link that has something to say: amber for a warning.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub link: usize,
    pub at: Pos2,
}

/// What the user did on the canvas this frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A frame is being dragged: its new top-left.
    Moved { dir: String, pos: (f32, f32) },
    /// A drag ended - the moment to write the positions down.
    DragEnded,
    /// Open this chip in this window.
    Open(String),
    /// Open this chip in a window of its own.
    OpenNewWindow(String),
    /// Open the window that edits this external part.
    EditPart(String),
    /// Take this chip out of the system (its folder stays).
    Remove(String),
    /// A module was clicked - to start a link, or to finish one.
    ModuleClicked { dir: String, module: usize },
    /// A link's badge was clicked: pick it in the list.
    LinkClicked(usize),
}

const FRAME_FILL: Color32 = Color32::from_rgb(30, 34, 42);
const FRAME_STROKE: Color32 = Color32::from_gray(78);
const ACTIVE_STROKE: Color32 = Color32::from_rgb(110, 170, 240);
const DEVICE_STROKE: Color32 = Color32::from_rgb(150, 138, 230);
const PROBLEM: Color32 = Color32::from_rgb(230, 170, 80);
const WARN: Color32 = Color32::from_rgb(186, 117, 23);

/// Draw every frame, then the links over them. Returns what the user did, and
/// the union of the frames (the content to fit the view to).
pub fn draw(
    ui: &mut egui::Ui,
    frames: &[Frame<'_>],
    links: &[LinkShape],
    markers: &[Marker],
) -> (Vec<Event>, Rect) {
    let mut events = Vec::new();
    let mut bounds = Rect::NOTHING;
    for f in frames {
        let rect = Rect::from_min_size(f.pos, f.layout.size);
        bounds = bounds.union(rect);
        draw_frame(ui, f, rect);
        interact(ui, f, rect, &mut events);
        modules_interact(ui, f, &mut events);
    }
    for l in links {
        draw_link(ui, l);
    }
    for m in markers {
        let r = Rect::from_center_size(m.at, egui::vec2(18.0, 18.0));
        ui.painter().circle_filled(m.at, 8.0, WARN);
        ui.painter().text(
            m.at,
            egui::Align2::CENTER_CENTER,
            "!",
            egui::FontId::proportional(12.0),
            Color32::from_rgb(250, 238, 218),
        );
        let id = egui::Id::new(("board_link_marker", m.link));
        if ui
            .interact(r, id, Sense::click())
            .on_hover_text("Something about this link does not match - see Links below")
            .clicked()
        {
            events.push(Event::LinkClicked(m.link));
        }
    }
    (events, bounds)
}

fn draw_frame(ui: &egui::Ui, f: &Frame<'_>, rect: Rect) {
    let painter = ui.painter();
    let l = &f.layout;
    let origin = rect.min.to_vec2();
    let (stroke_w, stroke_c) = if f.active {
        (1.5_f32, ACTIVE_STROKE)
    } else {
        (1.0_f32, FRAME_STROKE)
    };
    if f.external {
        // Dashed: something on the board, but no project of this IDE.
        painter.rect_filled(rect, 10.0, FRAME_FILL);
        let r = rect.shrink(0.5);
        let outline = [
            r.left_top(),
            r.right_top(),
            r.right_bottom(),
            r.left_bottom(),
            r.left_top(),
        ];
        painter.extend(egui::Shape::dashed_line(
            &outline,
            Stroke::new(stroke_w, stroke_c),
            6.0,
            4.0,
        ));
    } else {
        painter.rect(
            rect,
            10.0,
            FRAME_FILL,
            Stroke::new(stroke_w, stroke_c),
            egui::StrokeKind::Inside,
        );
    }

    // Header: the chip, then folder and runtime.
    let title = if f.view.chip.is_empty() {
        "Unknown chip"
    } else {
        f.view.chip.as_str()
    };
    let text_w = rect.width() - 2.0 * PAD - if f.active { 44.0 } else { 0.0 };
    let title = one_line(ui, title, 14.0, Color32::WHITE, text_w);
    painter.galley(rect.min + egui::vec2(PAD, 10.0), title, Color32::WHITE);
    let sub = one_line(
        ui,
        &f.view.subtitle(),
        11.0,
        Color32::GRAY,
        rect.width() - 2.0 * PAD,
    );
    painter.galley(rect.min + egui::vec2(PAD, 29.0), sub, Color32::GRAY);
    if f.active {
        painter.text(
            egui::pos2(rect.right() - PAD, rect.top() + 12.0),
            egui::Align2::RIGHT_TOP,
            "open",
            egui::FontId::proportional(11.0),
            ACTIVE_STROKE,
        );
    }

    // Wires first, so the pills sit on top of their ends.
    for (m, pts) in &l.wires {
        let color = module_color(f.view.modules[*m].kind, f.view.modules[*m].instance);
        let pts: Vec<Pos2> = pts.iter().map(|p| *p + origin).collect();
        painter.add(egui::Shape::line(pts, Stroke::new(1.5_f32, color)));
    }
    for (i, (m, r)) in f.view.modules.iter().zip(&l.modules).enumerate() {
        let mut color = module_color(m.kind, m.instance);
        if f.dimmed.get(i).copied().unwrap_or(false) {
            color = color.gamma_multiply(0.3);
        }
        // Square corners for a custom module, as on the Pins tab: it is the
        // user's, not a peripheral's.
        let radius = if m.kind.is_custom() { 0.0 } else { 6.0 };
        let r = r.translate(origin);
        pill(ui, r, &m.name, color, radius);
        if f.armed == Some(i) {
            painter.rect_stroke(
                r.expand(3.0),
                radius + 3.0,
                Stroke::new(2.0_f32, Color32::WHITE),
                egui::StrokeKind::Outside,
            );
        }
        // Detailed: each linked pad, named between the module and the edge,
        // with a dot where its wire leaves the frame.
        for row in &l.pins[i] {
            let label = row.label.translate(origin);
            let galley = one_line(ui, &row.text, 11.0, Color32::from_gray(200), label.width());
            let x = match l.module_ports[i].1 {
                Side::Right => label.right() - galley.size().x,
                Side::Left => label.left(),
            };
            painter.galley(
                egui::pos2(x, label.center().y - galley.size().y / 2.0),
                galley,
                Color32::from_gray(200),
            );
            painter.circle_filled(row.port + origin, 3.0, color);
        }
    }
    for (d, r) in f.view.devices.iter().zip(&l.devices) {
        pill(
            ui,
            r.translate(origin),
            &d.name,
            DEVICE_STROKE,
            PILL_H / 2.0,
        );
    }

    if let Some(note) = l.note {
        let (text, color) = match &f.view.problem {
            Some(p) => (format!("{}  {p}", ph::WARNING), PROBLEM),
            None => (
                "No modules or devices yet".to_owned(),
                Color32::from_gray(120),
            ),
        };
        let galley = one_line(ui, &text, 12.0, color, note.width());
        let at = note.translate(origin).left_center() - egui::vec2(0.0, galley.size().y / 2.0);
        painter.galley(at, galley, color);
    }
}

fn interact(ui: &egui::Ui, f: &Frame<'_>, rect: Rect, events: &mut Vec<Event>) {
    let dir = &f.view.dir;
    let resp = ui.interact(
        rect,
        egui::Id::new(("board_frame", dir)),
        Sense::click_and_drag(),
    );
    // The left button only: a middle or right drag (the canvas's pan button,
    // the menu button) must not carry a frame off.
    if resp.dragged_by(egui::PointerButton::Primary) && crate::panels::drag_decided(ui) {
        let to = rect.min + resp.drag_delta();
        events.push(Event::Moved {
            dir: dir.clone(),
            pos: (to.x, to.y),
        });
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    } else if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    if resp.drag_stopped_by(egui::PointerButton::Primary) {
        events.push(Event::DragEnded);
    }
    if resp.double_clicked() {
        if f.external {
            events.push(Event::EditPart(dir.clone()));
        } else if !f.active {
            events.push(Event::Open(dir.clone()));
        }
    }
    // egui shows one tooltip per layer: the frame's, over a module, would
    // hide the module's own hint.
    let over_module = resp.hover_pos().is_some_and(|p| {
        f.layout
            .modules
            .iter()
            .any(|m| m.translate(rect.min.to_vec2()).contains(p))
    });
    let resp = if over_module {
        resp
    } else if f.external {
        resp.on_hover_text("Double-click to edit this part. Drag to move.")
    } else if f.active {
        resp.on_hover_text("The chip open in this window. Drag to move.")
    } else {
        resp.on_hover_text("Double-click to open this chip. Drag to move.")
    };
    resp.context_menu(|ui| chip_menu(ui, f, events));
}

/// The chip's right-click menu - on the frame, and on each of its modules.
fn chip_menu(ui: &mut egui::Ui, f: &Frame<'_>, events: &mut Vec<Event>) {
    let dir = &f.view.dir;
    if f.external {
        if ui
            .button(format!("{}  Edit part…", ph::PENCIL_SIMPLE))
            .clicked()
        {
            events.push(Event::EditPart(dir.clone()));
            ui.close();
        }
        ui.separator();
        if ui
            .button(format!("{}  Remove from system", ph::MINUS_CIRCLE))
            .on_hover_text("The part and its links go")
            .clicked()
        {
            events.push(Event::Remove(dir.clone()));
            ui.close();
        }
        return;
    }
    let open = ui.add_enabled(
        !f.active,
        egui::Button::new(format!("{}  Open in this window", ph::FOLDER_OPEN)),
    );
    if open.on_disabled_hover_text("Already open").clicked() {
        events.push(Event::Open(dir.clone()));
        ui.close();
    }
    let new_window = ui.add_enabled(
        !f.active,
        egui::Button::new(format!("{}  Open in new window", ph::ARROW_SQUARE_OUT)),
    );
    if new_window
        .on_disabled_hover_text("It is the chip open here")
        .clicked()
    {
        events.push(Event::OpenNewWindow(dir.clone()));
        ui.close();
    }
    ui.separator();
    if ui
        .button(format!("{}  Remove from system", ph::MINUS_CIRCLE))
        .on_hover_text("The Board forgets this chip and its links. Its folder and files stay.")
        .clicked()
    {
        events.push(Event::Remove(dir.clone()));
        ui.close();
    }
}

/// A click on a module starts a link, or finishes the one started. Registered
/// after the frame, so the module is on top for clicks while a drag that
/// starts on it still moves the frame. A module covers much of its frame, so
/// it also answers the frame's double-click and right-click.
fn modules_interact(ui: &egui::Ui, f: &Frame<'_>, events: &mut Vec<Event>) {
    for (i, r) in f.layout.modules.iter().enumerate() {
        let r = r.translate(f.pos.to_vec2());
        let id = egui::Id::new(("board_module", &f.view.dir, i));
        let dimmed = f.dimmed.get(i).copied().unwrap_or(false);
        let hint = if f.armed == Some(i) {
            "Linking from here - click a module on another chip, or this one again to stop"
        } else if dimmed {
            "Cannot be linked to the module you started from"
        } else {
            "Click to link this module to one on another chip"
        };
        let resp = ui.interact(r, id, Sense::click()).on_hover_text(hint);
        if resp.hovered() && !dimmed {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.double_clicked() {
            // Its two clicks armed a link and dropped it again; what was
            // meant was the frame's double-click.
            if f.external {
                events.push(Event::EditPart(f.view.dir.clone()));
            } else if !f.active {
                events.push(Event::Open(f.view.dir.clone()));
            }
        } else if resp.clicked() && !dimmed {
            events.push(Event::ModuleClicked {
                dir: f.view.dir.clone(),
                module: i,
            });
        }
        resp.context_menu(|ui| chip_menu(ui, f, events));
    }
}

fn draw_link(ui: &egui::Ui, l: &LinkShape) {
    if l.points.len() < 2 {
        return;
    }
    let painter = ui.painter();
    let (width, color) = if l.selected {
        (l.width + 1.5, l.color.gamma_multiply(1.4))
    } else {
        (l.width, l.color)
    };
    let pts = crate::panels::structure_map::gui::rounded_path(&l.points, 6.0);
    if l.selected {
        painter.add(egui::Shape::line(
            pts.clone(),
            Stroke::new(width + 3.0, Color32::from_white_alpha(40)),
        ));
    }
    painter.add(egui::Shape::line(pts, Stroke::new(width, color)));
    let n = l.points.len();
    if l.arrow_end {
        arrow(
            painter,
            l.points[n - 1],
            l.points[n - 1] - l.points[n - 2],
            color,
        );
    }
    if l.arrow_start {
        arrow(painter, l.points[0], l.points[0] - l.points[1], color);
    }
}

/// A filled arrowhead with its tip at `tip`, pointing along `along`.
fn arrow(painter: &egui::Painter, tip: Pos2, along: Vec2, color: Color32) {
    let d = along.normalized();
    if !d.is_finite() {
        return;
    }
    let back = tip - d * 8.0;
    let side = d.rot90() * 4.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side, back - side],
        color,
        Stroke::NONE,
    ));
}

/// A module or device: a rounded box with its name.
fn pill(ui: &egui::Ui, r: Rect, name: &str, color: Color32, radius: f32) {
    let fill = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 46);
    ui.painter().rect(
        r,
        radius,
        fill,
        Stroke::new(1.0_f32, color),
        egui::StrokeKind::Inside,
    );
    let text = Color32::from_gray(232);
    let galley = one_line(ui, name, 12.0, text, r.width() - 12.0);
    // A module that grew pad rows keeps its name at the top row.
    let y = if r.height() > PILL_H {
        r.top() + PILL_H / 2.0
    } else {
        r.center().y
    };
    let at = egui::pos2(
        r.center().x - galley.size().x / 2.0,
        y - galley.size().y / 2.0,
    );
    ui.painter().galley(at, galley, text);
}

/// Text cut to one line of `max_w`, ending in `…` when it does not fit - a
/// long device name must not run out of its box.
fn one_line(
    ui: &egui::Ui,
    text: &str,
    size: f32,
    color: Color32,
    max_w: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_owned(),
        egui::FontId::proportional(size),
        color,
    );
    job.wrap = egui::text::TextWrapping {
        max_width: max_w.max(1.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.fonts_mut(|f| f.layout_job(job))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::board::snapshot::ModuleItem;
    use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, UsartModuleConfig};

    fn input(pass: usize, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            time: Some(pass as f64 / 60.0),
            events,
            ..Default::default()
        }
    }

    fn button(at: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn run(
        ctx: &egui::Context,
        pass: usize,
        events: Vec<egui::Event>,
        frames: &[Frame<'_>],
    ) -> Vec<Event> {
        let mut out = Vec::new();
        let _ = crate::headless::run_ui(ctx, input(pass, events), |ui| {
            out = draw(ui, frames, &[], &[]).0
        });
        out
    }

    /// A drag reports where the frame goes, and its end once - the moment the
    /// Board writes positions to disk.
    #[test]
    fn dragging_a_frame_moves_it_and_ends_once() {
        let view = ChipView::broken("stm32_main", "gone");
        let frames = [Frame::plain(&view, egui::pos2(100.0, 100.0), false)];
        let ctx = egui::Context::default();
        let at = egui::pos2(150.0, 120.0);
        assert!(run(&ctx, 0, vec![egui::Event::PointerMoved(at)], &frames).is_empty());
        run(&ctx, 1, vec![button(at, true)], &frames);
        let mut last = None;
        let mut ended = 0;
        for step in 1..=4 {
            let p = at + egui::vec2(10.0, 7.5) * step as f32;
            for e in run(&ctx, 1 + step, vec![egui::Event::PointerMoved(p)], &frames) {
                match e {
                    Event::Moved { dir, pos } => {
                        assert_eq!(dir, "stm32_main");
                        last = Some(pos);
                    }
                    Event::DragEnded => ended += 1,
                    other => panic!("unexpected {other:?}"),
                }
            }
        }
        // The frames here never move, so each report is the frame's spot plus
        // that frame's pointer step.
        assert_eq!(last, Some((110.0, 107.5)));
        let end = run(
            &ctx,
            6,
            vec![button(at + egui::vec2(40.0, 30.0), false)],
            &frames,
        );
        ended += end.iter().filter(|e| **e == Event::DragEnded).count();
        assert_eq!(ended, 1);
        assert!(!end.iter().any(|e| matches!(e, Event::Open(_))));
    }

    /// Only the left button carries a frame: the middle one is the canvas's
    /// pan button, the right one opens the menu.
    #[test]
    fn only_a_left_drag_moves_a_frame() {
        let view = ChipView::broken("stm32_main", "gone");
        let frames = [Frame::plain(&view, egui::pos2(100.0, 100.0), false)];
        for b in [egui::PointerButton::Middle, egui::PointerButton::Secondary] {
            let ctx = egui::Context::default();
            let at = egui::pos2(150.0, 120.0);
            let press = |pressed: bool, pos: Pos2| egui::Event::PointerButton {
                pos,
                button: b,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            let mut events = run(&ctx, 0, vec![egui::Event::PointerMoved(at)], &frames);
            events.extend(run(&ctx, 1, vec![press(true, at)], &frames));
            for step in 1..=4 {
                let p = at + egui::vec2(10.0, 7.5) * step as f32;
                events.extend(run(
                    &ctx,
                    1 + step,
                    vec![egui::Event::PointerMoved(p)],
                    &frames,
                ));
            }
            let end = at + egui::vec2(40.0, 30.0);
            events.extend(run(&ctx, 6, vec![press(false, end)], &frames));
            assert!(
                !events
                    .iter()
                    .any(|e| matches!(e, Event::Moved { .. } | Event::DragEnded)),
                "{b:?}: {events:?}"
            );
        }
    }

    /// A click whose hand slips a few points off a frame, onto the frame
    /// beside it, carries nothing along: egui 0.36 starts the drag the moment
    /// the pointer leaves the frame, well before the click distance.
    #[test]
    fn a_click_that_slips_onto_the_next_frame_moves_nothing() {
        let (left, right) = (
            ChipView::broken("stm32_main", "gone"),
            ChipView::broken("esp32_radio", "gone"),
        );
        let first = Frame::plain(&left, egui::pos2(100.0, 100.0), false);
        let edge = first.pos.x + first.layout.size.x;
        let frames = [first, Frame::plain(&right, egui::pos2(edge, 100.0), false)];
        let ctx = egui::Context::default();
        let at = egui::pos2(edge - 1.0, 120.0);
        let mut events = run(&ctx, 0, vec![egui::Event::PointerMoved(at)], &frames);
        events.extend(run(&ctx, 1, vec![button(at, true)], &frames));
        for (pass, dx) in [(2, 1.5), (3, 3.0)] {
            let p = at + egui::vec2(dx, 0.0);
            events.extend(run(&ctx, pass, vec![egui::Event::PointerMoved(p)], &frames));
        }
        let end = at + egui::vec2(3.0, 0.0);
        events.extend(run(&ctx, 4, vec![button(end, false)], &frames));
        assert!(
            !events.iter().any(|e| matches!(e, Event::Moved { .. })),
            "{events:?}"
        );
    }

    /// Double-clicking opens a chip - but not the one already open.
    #[test]
    fn a_double_click_opens_any_chip_but_the_open_one() {
        let view = ChipView::broken("esp32_radio", "gone");
        for active in [false, true] {
            let frames = [Frame::plain(&view, egui::pos2(100.0, 100.0), active)];
            let ctx = egui::Context::default();
            let at = egui::pos2(150.0, 120.0);
            let mut events = Vec::new();
            run(&ctx, 0, vec![egui::Event::PointerMoved(at)], &frames);
            for (pass, pressed) in [(1, true), (2, false), (3, true), (4, false)] {
                events.extend(run(&ctx, pass, vec![button(at, pressed)], &frames));
            }
            let opened = events.contains(&Event::Open("esp32_radio".into()));
            assert_eq!(opened, !active, "active = {active}: {events:?}");
        }
    }

    fn one_module_chip() -> ChipView {
        let mut v = ChipView::broken("stm32_main", "x");
        v.problem = None;
        v.modules.push(ModuleItem {
            name: "USART1".into(),
            kind: ModuleKind::GenericInterfaceUsart,
            instance: 1,
            pins: Default::default(),
            signals: vec![],
            config: ModuleConfig::Usart(UsartModuleConfig::new(1)),
        });
        v
    }

    /// Clicking a module reports it - that is how a link starts and ends - and
    /// a greyed-out one (not linkable to the armed module) reports nothing.
    #[test]
    fn a_module_click_is_reported_unless_it_is_greyed_out() {
        let view = one_module_chip();
        for dimmed in [false, true] {
            let mut f = Frame::plain(&view, egui::pos2(100.0, 100.0), false);
            f.dimmed = vec![dimmed];
            let at = egui::pos2(100.0, 100.0) + f.layout.modules[0].center().to_vec2();
            let frames = [f];
            let ctx = egui::Context::default();
            let mut events = run(&ctx, 0, vec![egui::Event::PointerMoved(at)], &frames);
            events.extend(run(&ctx, 1, vec![button(at, true)], &frames));
            events.extend(run(&ctx, 2, vec![button(at, false)], &frames));
            let clicked = events.contains(&Event::ModuleClicked {
                dir: "stm32_main".into(),
                module: 0,
            });
            assert_eq!(clicked, !dimmed, "dimmed = {dimmed}: {events:?}");
        }
    }
}
