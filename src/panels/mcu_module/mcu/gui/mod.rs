//! MCU GUI rendering — orchestrates chip visualization and pin configuration.
//!
//! The draw() method coordinates multiple components:
//! - layout: geometry calculations
//! - chip: chip body + pin rendering
//! - panel: function selection UI
//! - info: information popup

pub mod chip;
pub mod clock;
pub mod device_frame;
pub mod geometry;
pub mod info;
pub mod io_arrows;
pub mod layout;
pub mod module_docs;
pub mod modules;
pub mod panel;
pub mod rotate;
pub mod wire;

use crate::panels::mcu_module::mcu::model::{Mcu, PIN_HEIGHT};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

impl Mcu {
    /// Main rendering method — draws the chip, its pins, the selected pin's
    /// function list inside the body, the virtual modules and the in/out fields.
    ///
    /// Returns `(pin_number, pin_name, selected_function)` when a pin's function
    /// changes, plus the function list's rect in SCREEN coordinates — the caller
    /// needs it to route the mouse wheel to the list instead of the canvas zoom
    /// (see [`panel::draw_pin_functions`]).
    pub fn draw(
        &mut self,
        ui: &mut egui::Ui,
    ) -> (Option<(usize, String, PinFunction)>, egui::Rect) {
        // Drop any module wire whose pin was re-purposed away from USART.
        self.reconcile_modules();

        let (mcu_width, mcu_height, base_w, base_h) = match &self.grid {
            // A ball-grid package has no edge pins to size the body from — the
            // body must instead be big enough to HOLD the grid.
            Some(g) => layout::calculate_grid_layout(geometry::grid_body_size(g)),
            None => geometry::body_layout(self),
        };

        // Diagram rotation (view-only): a 2-sided chip turns 90°, while a
        // 4-sided one and a ball grid both become a 45° diamond. `local_chip`
        // is the un-rotated body used to compute pin geometry; `display_chip`
        // is the axis-aligned rect the body + info panel + modules use;
        // `content_rect` is where the inner panel draws (shrunk to fit the
        // diamond). See `rotate.rs`.
        let rot_mode = rotate::RotMode::of(self);

        // Reserve a margin all around the chip for virtual modules and in/out
        // arrows, so their boxes/arrows + wires sit beyond the pins (on the
        // pins' own side) without overlapping the chip. Use the larger of the
        // two when both are present.
        let mut mx = 0.0_f32;
        let mut my = 0.0_f32;
        if !self.modules.is_empty() {
            mx = modules::MARGIN_X;
            my = modules::MARGIN_Y;
        }
        let has_io = io_arrows::has_io_pins(self);
        if has_io {
            mx = mx.max(io_arrows::MARGIN_X);
            my = my.max(io_arrows::MARGIN_Y);
        }
        // A device mat reaches past its outermost part by a rim and a tab, and a
        // mat on a BARE pad reaches past the stub tip as well. Painted shapes
        // never grow `ui.min_rect()`, so the Scene's auto-fit cannot discover
        // that on its own - the painter has to be told, or the mat is clipped at
        // the canvas edge with nothing to show for it.
        let has_groups = self.groups.iter().any(|g| g.is_live());
        if has_groups {
            mx = mx.max(device_frame::BARE_REACH) + device_frame::HALO;
            my = my.max(device_frame::BARE_REACH) + device_frame::HALO;
        }
        // Canvas size follows the rotation — a diamond needs a bigger square box
        // (its bounding circle spans the chip's diagonal), a 90° chip swaps axes.
        let (canvas_w, canvas_h) = match rot_mode {
            rotate::RotMode::Diamond => {
                let diag = (mcu_width * mcu_width + mcu_height * mcu_height).sqrt();
                let ext = diag + 2.0 * (PIN_HEIGHT + 64.0);
                (ext, ext)
            }
            rotate::RotMode::Quarter => (base_h, base_w),
            rotate::RotMode::None => (base_w, base_h),
        };
        // Grow the painter to cover modules dragged far from the chip, so the
        // Scene's auto-fit encompasses them instead of clipping at the panel
        // edge. The chip stays centred; the extra span is just empty canvas.
        let drag_ext = modules::dragged_half_extent(self).max(io_arrows::dragged_half_extent(self));
        let halo = if has_groups { device_frame::HALO } else { 0.0 };
        let half_w = (canvas_w / 2.0 + mx).max(drag_ext.x + 16.0 + halo);
        let half_h = (canvas_h / 2.0 + my).max(drag_ext.y + 16.0 + halo);
        // The canvas senses CLICKS so empty space can clear the selection. Sensing
        // click (not drag) leaves the Scene's drag-pan alone: egui hit-tests click
        // and drag targets separately, and every pin / module / field registers
        // its own `interact` LATER, i.e. above this one — so a click only reaches
        // the background when it landed on none of them.
        let (response, painter) =
            ui.allocate_painter(egui::vec2(2.0 * half_w, 2.0 * half_h), egui::Sense::click());

        let rect = response.rect;
        let center = rect.center();
        let local_chip = egui::Rect::from_center_size(center, egui::vec2(mcu_width, mcu_height));
        let rot = rotate::Rot::new(center, rot_mode.angle());
        let display_chip = match rot_mode {
            rotate::RotMode::None => local_chip,
            rotate::RotMode::Quarter => {
                egui::Rect::from_center_size(center, egui::vec2(mcu_height, mcu_width))
            }
            rotate::RotMode::Diamond => egui::Rect::from_points(&rot.quad(local_chip)),
        };
        // The inner info panel stays upright — full body, or the largest upright
        // square inside the diamond (≈ 0.71× the body).
        let content_rect = match rot_mode {
            rotate::RotMode::Diamond => {
                let s = mcu_width.min(mcu_height) / std::f32::consts::SQRT_2;
                egui::Rect::from_center_size(center, egui::vec2(s, s))
            }
            _ => display_chip,
        };

        // Filled by the two passes below with every box and field rect they
        // actually paint.
        let mut members: Vec<device_frame::Member> = Vec::new();
        // ── The device tabs' drag handles ────────────────────────────────
        // Minted against LAST frame's rects (see `Mcu::device_tabs`), and placed
        // HERE on purpose: after the canvas background, so a tab takes its own
        // click instead of clearing the selection; before every pad, box and
        // field, so each of those wins any tie inside a tab that happens to lie
        // under one.
        let mut drag_now: Option<(String, (f32, f32))> = None;
        let mut tab_click: Option<String> = None;
        let mut tab_reset: Option<String> = None;
        for h in self.device_tabs.clone() {
            let manual = self.device_is_manual(&h.name);
            let resp = ui
                .interact(
                    h.off.translate(center.to_vec2()),
                    ui.id().with(("device_tab", &h.name, h.cluster)),
                    egui::Sense::click_and_drag(),
                )
                .on_hover_cursor(egui::CursorIcon::Grab)
                .on_hover_text("Click to select the whole device - drag to move every part of it");
            if resp.dragged() && crate::panels::drag_decided(ui) {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                let d = resp.drag_delta();
                if d != egui::Vec2::ZERO {
                    drag_now = Some((h.name.clone(), (d.x, d.y)));
                }
            }
            if resp.clicked() {
                tab_click = Some(h.name.clone());
            }
            if manual {
                resp.context_menu(|ui| {
                    if ui.button("Reset device to auto position").clicked() {
                        tab_reset = Some(h.name.clone());
                        ui.close();
                    }
                });
            }
        }
        // Fixed BEFORE either paint pass, so the boxes and the io fields move by
        // one and the same value.
        self.device_drag = drag_now;

        // The device mats go in a slot reserved BEFORE the body, so the opaque
        // body punches the chip out of every mat for free - no keep-out rect, no
        // inner-edge geometry. `Painter::set` is documented for exactly this;
        // this is the codebase's first use of it.
        let mat_slot = has_groups.then(|| painter.add(egui::Shape::Noop));

        // ── Chip body ───────────────────────────────────────────────────────
        // A board is a PCB with a chip on it, and it says so in one colour.
        let body_fill = if self.board_chip.is_some() {
            chip::BOARD_FILL
        } else {
            chip::CHIP_FILL
        };
        match rot_mode {
            rotate::RotMode::Diamond => chip::draw_chip_body_diamond(&painter, local_chip, rot),
            _ => chip::draw_chip_body(&painter, display_chip, body_fill),
        }

        // ── Pins + click detection ───────────────────────────────────────────
        let clicked_pin = match rot_mode {
            rotate::RotMode::None => {
                chip::render_pins_and_detect_clicks(self, &painter, display_chip, rot, ui)
            }
            _ => chip::render_pins_rotated(self, &painter, local_chip, rot, rot_mode, ui),
        };

        // The name tabs go in a SECOND slot, here: above the body and the pin
        // stubs, still below every box, wire and field. A tab shares the mat's
        // depth only until a mat lands on the pin row - which is exactly where a
        // bare pad's mat is - and a name under a 50 px opaque stub cannot be
        // read at all.
        let tab_slot = has_groups.then(|| painter.add(egui::Shape::Noop));

        // ── Virtual modules (boxes + wires) around the chip ───────────────────
        if !self.modules.is_empty() {
            modules::draw_modules(
                self,
                &painter,
                local_chip,
                display_chip,
                rot,
                ui,
                &mut members,
            );
        }

        // ── In/out arrows + rename fields for GPIO In/Out/PWM pins ────────────
        if has_io {
            io_arrows::draw_io_arrows(self, &painter, local_chip, rot, ui, &mut members);
        }

        // ── Device mats, back-filled into the reserved slot ───────────────────
        // Unconditional, because both passes above are gated: a device made only
        // of module pads on a chip with no io pins - or only of loose pads on a
        // chip with no modules - still has a mat to draw.
        //
        // Through the SAME `painter` binding, never a `chip::dimmed` clone and
        // never `with_clip_rect`: `Painter::set` re-stamps the setting painter's
        // clip rect and re-runs its opacity transform, so a faded or narrowed
        // painter would silently fade or clip every mat at once.
        if let (Some(mats), Some(tabs)) = (mat_slot, tab_slot) {
            let pads = device_frame::pad_footprints(self, local_chip, rot);
            let mut handles: Vec<device_frame::TabHandle> = Vec::new();
            let (mat_shapes, tab_shapes) = device_frame::frames(
                self,
                &painter,
                display_chip.center(),
                rect,
                &members,
                &pads,
                &mut handles,
            );
            painter.set(mats, egui::Shape::Vec(mat_shapes));
            painter.set(tabs, egui::Shape::Vec(tab_shapes));
            // A device under the pointer keeps LAST frame's handle entries. Its
            // mat re-clusters as it moves past a foreign box, which would change
            // the cluster index, change the widget id, and drop the drag halfway
            // across the canvas.
            if let Some((dev, _)) = self.device_drag.clone() {
                handles.retain(|h| h.name != dev);
                handles.extend(self.device_tabs.iter().filter(|h| h.name == dev).cloned());
            }
            self.device_tabs = handles;
        }

        // Deferred out of the handle loop above, which ran while `self` was
        // borrowed for the interact.
        if let Some(name) = tab_click {
            // Click again to deselect, exactly as a pin and a box behave.
            self.selected_device = if self.selected_device.as_deref() == Some(name.as_str()) {
                None
            } else {
                Some(name)
            };
        }
        if let Some(name) = tab_reset {
            self.reset_device_position(&name);
        }

        // Toggle selected_pin (click again to deselect); reset the list scroll on
        // change, so another pin's list starts at the top.
        if let Some(n) = clicked_pin {
            let prev = self.selected_pin;
            self.selected_pin = if self.selected_pin == Some(n) {
                None
            } else {
                Some(n)
            };
            if prev != self.selected_pin {
                self.fn_scroll_offset = 0.0;
            }
            // Selecting a CONFIGURED pin also asks the editor to jump to the
            // line that binds its variable. Only on the click that selects: the
            // second click deselects, and jumping again there would be noise. An
            // Unset pin has no binding to jump to.
            if self.selected_pin == Some(n) {
                self.request_pin_goto(n);
            }
        }

        // A click that reached the BACKGROUND — no pin, no module box, no pin
        // field took it — means "focus nothing": drop both selections and ask the
        // module list to collapse, so the canvas and the list agree. The chip
        // BODY is not background: it is the chip itself, and a click that merely
        // missed a pin by a few pixels shouldn't throw the selection away.
        let on_body = response
            .interact_pointer_pos()
            .is_some_and(|p| display_chip.contains(p));
        if response.clicked() && !on_body {
            self.clear_canvas_selection();
        }

        // ── Inner chip panel ─────────────────────────────────────────────────
        // A selected pin turns the body into its function list; otherwise the
        // body carries the chip's name — but ONLY on a package whose pins are on
        // the outside. A ball grid fills that same body with pads, so the name
        // would be painted straight through them (and through their designators).
        // The chip is named in the tab header anyway; the balls are not.
        if self.selected_pin.is_some() {
            panel::draw_pin_functions(self, &painter, ui, content_rect)
        } else {
            if !self.has_inner_pins() {
                const NAME_PT: f32 = 22.0;
                // The board's own furniture goes UNDER the name, so a long name
                // stays readable across the chip square rather than behind it.
                chip::draw_board_features(
                    &painter,
                    self,
                    display_chip,
                    content_rect.center(),
                    NAME_PT,
                );
                painter.text(
                    content_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    &self.name,
                    egui::FontId::proportional(NAME_PT),
                    egui::Color32::WHITE,
                );
            }
            (None, egui::Rect::NOTHING)
        }
    }
}
