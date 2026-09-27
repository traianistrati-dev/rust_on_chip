//! A window's size in the terms this app's numbers were written in: what its
//! CONTENT gets.
//!
//! From egui 0.35 on, `Window::default_size`, `fixed_size`, `default_width`,
//! `min_width`, `max_width` and the rest set the OUTER size - the frame's
//! margin and stroke on every side, and the title bar, all come out of the
//! number. On 0.34 the same number was the content's, and every window here
//! was sized for that: a dialog that asked for 680 x 560 got 662 x 509 after
//! the upgrade, and the rows that fill its width wrapped and narrowed.

use eframe::egui;

/// What a titled `egui::Window` in the default window frame adds around its
/// content: the frame (margin + stroke) on each side, and the title bar above.
///
/// The title bar is the heading row padded by the window margin, over a line
/// as wide as the frame's stroke - how egui 0.36 lays it out. `window_size_tests`
/// measure real content rects against it, so an egui that draws it differently
/// fails there rather than on screen.
pub fn chrome(ctx: &egui::Context) -> egui::Vec2 {
    let style = ctx.global_style();
    let frame = egui::Frame::window(&style);
    let heading = egui::TextStyle::Heading.resolve(&style);
    let row = ctx.fonts_mut(|f| f.row_height(&heading));
    let title_bar =
        row + frame.inner_margin.sum().y + frame.stroke.width + frame.outer_margin.bottomf();
    frame.total_margin().sum() + egui::vec2(0.0, title_bar)
}

/// The outer size to hand a titled `egui::Window` so its content gets `content`.
pub fn outer_size(ctx: &egui::Context, content: egui::Vec2) -> egui::Vec2 {
    content + chrome(ctx)
}
