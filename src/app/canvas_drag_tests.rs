//! A click that slips off a Pins-canvas item does not move it.
//!
//! egui 0.36 starts the drag of a `Sense::click_and_drag` widget the moment
//! the pointer leaves it, where 0.34 waited for the pointer to travel the
//! click distance (6 px). An io field, a module box and a device tab each
//! keep a moved position as a MANUAL placement, so a click on one's edge with
//! a pixel or two of hand movement pinned it where it stood. This draws the
//! MCU panel the way `AppIde::ui` does and plays real pointer events on them.

use super::{AppIde, McuTab};
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::mcu_config::PinGroup;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

const CHIP: &str = "stm32f103c8t6";

struct Bench {
    ctx: egui::Context,
    app: AppIde,
    pass: u64,
}

/// Where a canvas widget was drawn on the last frame.
#[derive(Clone, Copy, Debug)]
struct Seen {
    /// In canvas (scene) coordinates - what a stored position moves.
    local: egui::Rect,
    /// On the screen - where the pointer has to go.
    screen: egui::Rect,
    /// Screen points per canvas point.
    zoom: f32,
}

impl Bench {
    /// PC13 an output (an io field), PA9/PA10 USART1 (a module box), and
    /// optionally PC13 made a device of its own (a device tab).
    fn new(device: bool) -> Self {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(
            &eframe::CreationContext::_new_kittest(ctx.clone()),
            None,
            None,
        );
        app.startup_picker = None;
        app.selected_mcu_id = CHIP.to_owned();
        let mut mcu = AppIde::build_mcu_for(&app.mcu_registry, CHIP).expect("a built-in chip");
        for (name, func) in [
            ("PC13", PinFunction::GpioOutput),
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
        ] {
            let num = pin(&mcu, name);
            mcu.find_pin_mut(num).expect("the pin").selected_function = func;
        }
        mcu.reconcile_modules();
        if device {
            mcu.groups.push(PinGroup {
                name: "Led".into(),
                pins: [pin(&mcu, "PC13")].into(),
            });
        }
        app.mcu = Some(mcu);
        app.active_tab = McuTab::Pins;
        let mut b = Self { ctx, app, pass: 0 };
        // The canvas is fitted, and the device tabs minted, a frame late.
        for _ in 0..4 {
            b.step(vec![]);
        }
        b
    }

    fn step(&mut self, events: Vec<egui::Event>) {
        self.pass += 1;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            time: Some(self.pass as f64 / 30.0),
            predicted_dt: 1.0 / 30.0,
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let _ = crate::headless::run_ui(&self.ctx, input, |ui| app.show_mcu_panel(ui));
    }

    fn mcu(&self) -> &Mcu {
        self.app.mcu.as_ref().expect("the chip is loaded")
    }

    /// The widget the canvas minted as `its_ui.id().with(key)`.
    fn find(&self, key: impl egui::AsIdSalt + Copy) -> Seen {
        let w = self
            .ctx
            .viewport_for(egui::ViewportId::ROOT, |vp| {
                vp.prev_pass
                    .widgets
                    .layers()
                    .flat_map(|(_, ws)| ws.iter())
                    .find(|w| w.id == w.parent_id.with(key))
                    .copied()
            })
            .expect("the canvas drew the widget");
        let to_screen = self
            .ctx
            .layer_transform_to_global(w.layer_id)
            .unwrap_or_default();
        Seen {
            local: w.interact_rect,
            screen: to_screen * w.interact_rect,
            zoom: to_screen.scaling,
        }
    }

    /// Hover `at`, press there, move the pointer by `by` in `steps` frames,
    /// release where it ended, and let a frame pass.
    fn gesture(&mut self, at: egui::Pos2, by: egui::Vec2, steps: usize) {
        self.step(vec![egui::Event::PointerMoved(at)]);
        self.step(vec![button(at, true)]);
        let mut p = at;
        for _ in 0..steps {
            p += by / steps as f32;
            self.step(vec![egui::Event::PointerMoved(p)]);
        }
        self.step(vec![button(p, false)]);
        self.step(vec![]);
    }

    /// A click on the item's top edge whose hand drifted 3 px up while the
    /// button was down: 2 px off the item, half the click distance.
    fn slip_off(&mut self, item: Seen) {
        let at = egui::pos2(item.screen.center().x, item.screen.top() + 1.0);
        self.gesture(at, egui::vec2(0.0, -3.0), 2);
    }

    /// A real drag of the item `key` names, 30 px down: how far it went, in
    /// screen points.
    fn drag_down(&mut self, key: impl egui::AsIdSalt + Copy) -> f32 {
        // The click before this may have unfolded the module list, which
        // shrinks the canvas under the pointer: let that finish, and aim anew.
        for _ in 0..10 {
            self.step(vec![]);
        }
        let item = self.find(key);
        self.gesture(item.screen.center(), egui::vec2(0.0, 30.0), 3);
        (self.find(key).local.top() - item.local.top()) * item.zoom
    }
}

fn pin(mcu: &Mcu, name: &str) -> usize {
    mcu.iter_all_pins()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("{CHIP} has no {name}"))
        .number
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn an_io_field_moves_only_on_a_real_drag() {
    let mut b = Bench::new(false);
    let pc13 = pin(b.mcu(), "PC13");
    let key = ("io_drag", pc13);
    let before = b.find(key);
    assert!(
        b.mcu().io_pin_pos.is_empty(),
        "the field starts auto-placed"
    );

    b.slip_off(before);
    assert!(
        b.mcu().io_pin_pos.is_empty(),
        "a slipped click pinned the field: {:?}",
        b.mcu().io_pin_pos
    );
    assert_eq!(b.find(key).local, before.local);

    let went = b.drag_down(key);
    assert!(b.mcu().io_pin_pos.contains_key(&pc13), "a drag pins it");
    assert!(
        (went - 30.0).abs() < 1.0,
        "it followed the 30 px drag: {went}"
    );
}

#[test]
fn a_module_box_moves_only_on_a_real_drag() {
    let mut b = Bench::new(false);
    let i = b.mcu().modules.len() - 1;
    assert_eq!(
        b.mcu().modules[i].pos,
        (0.0, 0.0),
        "the box starts auto-placed"
    );
    let key = ("vmod_box", i);
    let before = b.find(key);

    b.slip_off(before);
    assert_eq!(
        b.mcu().modules[i].pos,
        (0.0, 0.0),
        "a slipped click pinned the box"
    );
    assert_eq!(b.find(key).local, before.local);

    let went = b.drag_down(key);
    assert_ne!(b.mcu().modules[i].pos, (0.0, 0.0), "a drag pins it");
    assert!(
        (went - 30.0).abs() < 1.0,
        "it followed the 30 px drag: {went}"
    );
}

/// A device tab moves every part of the device, and pins each of them.
#[test]
fn a_device_moves_only_on_a_real_drag() {
    let mut b = Bench::new(true);
    let tab = b
        .mcu()
        .device_tabs
        .first()
        .cloned()
        .expect("the device has a tab");
    let key = ("device_tab", &tab.name, tab.cluster);
    let before = b.find(key);
    assert!(
        !b.mcu().device_is_manual("Led"),
        "the device starts auto-placed"
    );

    b.slip_off(before);
    assert!(
        !b.mcu().device_is_manual("Led"),
        "a slipped click pinned the device: {:?}",
        b.mcu().io_pin_pos
    );

    let went = b.drag_down(key);
    assert!(b.mcu().device_is_manual("Led"), "a drag pins it");
    assert!(
        (went - 30.0).abs() < 1.0,
        "it followed the 30 px drag: {went}"
    );
}
