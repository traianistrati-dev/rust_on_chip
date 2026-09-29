//! The devices of an I2C bus on the Pins canvas, driven with real pointer and
//! key events through the MCU panel, the way `AppIde::ui` draws it.
//!
//! Same bench shape as `canvas_drag_tests`: the widgets are found where the
//! canvas really drew them last frame, so a box that is clipped, covered, or
//! placed somewhere else than the layout says fails here and not in the GUI.

use super::{AppIde, McuTab};
use crate::panels::mcu_module::mcu::Mcu;
use crate::panels::mcu_module::mcu::gui::i2c_children as kids;
use crate::panels::mcu_module::modules::{I2cDeviceKey, ModuleConfig};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

const CHIP: &str = "stm32f103c8t6";

struct Bench {
    ctx: egui::Context,
    app: AppIde,
    pass: u64,
}

/// Where a canvas widget was drawn last frame, and the map from canvas to
/// screen points around it.
#[derive(Clone, Copy, Debug)]
struct Seen {
    local: egui::Rect,
    screen: egui::Rect,
    zoom: f32,
}

impl Seen {
    /// A canvas point, on the screen.
    fn at(&self, p: egui::Pos2) -> egui::Pos2 {
        self.screen.min + (p - self.local.min) * self.zoom
    }
}

impl Bench {
    /// PB6/PB7 as I2C1 SCL/SDA: one bus, no device yet.
    fn new() -> Self {
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
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{CHIP} has no {name}"))
                .number;
            mcu.find_pin_mut(num).expect("the pin").selected_function = func;
        }
        mcu.reconcile_modules();
        app.mcu = Some(mcu);
        app.active_tab = McuTab::Pins;
        let mut b = Self { ctx, app, pass: 0 };
        b.settle();
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

    /// One frame, returning every string it painted.
    fn painted(&mut self) -> Vec<String> {
        fn walk(s: &egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        self.pass += 1;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 900.0),
            )),
            time: Some(self.pass as f64 / 30.0),
            predicted_dt: 1.0 / 30.0,
            ..Default::default()
        };
        let app = &mut self.app;
        let out = crate::headless::run_ui(&self.ctx, input, |ui| app.show_mcu_panel(ui));
        let mut texts = Vec::new();
        for s in &out.shapes {
            walk(&s.shape, &mut texts);
        }
        texts
    }

    /// Let the canvas refit and the module list fold or unfold.
    fn settle(&mut self) {
        for _ in 0..10 {
            self.step(vec![]);
        }
    }

    fn mcu(&self) -> &Mcu {
        self.app.mcu.as_ref().expect("the chip is loaded")
    }

    fn mcu_mut(&mut self) -> &mut Mcu {
        self.app.mcu.as_mut().expect("the chip is loaded")
    }

    fn bus(&self) -> (usize, u8, String) {
        let i = self
            .mcu()
            .modules
            .iter()
            .position(|m| matches!(m.config, ModuleConfig::I2c(_)))
            .expect("the I2C bus");
        let m = &self.mcu().modules[i];
        (i, m.instance(), m.id.clone())
    }

    fn devices(&self) -> Vec<(String, u8, u32)> {
        let (i, ..) = self.bus();
        match &self.mcu().modules[i].config {
            ModuleConfig::I2c(c) => c
                .devices
                .iter()
                .map(|d| (d.name.clone(), d.address, d.uid))
                .collect(),
            _ => unreachable!(),
        }
    }

    /// The widget the canvas minted as `its_ui.id().with(key)`, if it drew it.
    fn try_find(&self, key: impl egui::AsIdSalt + Copy) -> Option<Seen> {
        let w = self.ctx.viewport_for(egui::ViewportId::ROOT, |vp| {
            vp.prev_pass
                .widgets
                .layers()
                .flat_map(|(_, ws)| ws.iter())
                .find(|w| w.id == w.parent_id.with(key))
                .copied()
        })?;
        let to_screen = self
            .ctx
            .layer_transform_to_global(w.layer_id)
            .unwrap_or_default();
        Some(Seen {
            local: w.interact_rect,
            screen: to_screen * w.interact_rect,
            zoom: to_screen.scaling,
        })
    }

    fn find(&self, key: impl egui::AsIdSalt + Copy) -> Seen {
        self.try_find(key).expect("the canvas drew the widget")
    }

    /// A field the canvas minted as `canvas_ui.id().with(key)` - put with
    /// `ui.put`, which registers it under a child ui, so its own parent id is
    /// not the canvas's. The strip of device `near` is interacted on the
    /// canvas ui itself, so ITS parent id is the one to salt.
    fn find_field(&self, near: I2cDeviceKey, key: impl egui::AsIdSalt + Copy) -> Seen {
        let (_, inst, _) = self.bus();
        let strip_key = ("i2c_child", inst, near);
        let w = self
            .ctx
            .viewport_for(egui::ViewportId::ROOT, |vp| {
                let all: Vec<_> = vp
                    .prev_pass
                    .widgets
                    .layers()
                    .flat_map(|(_, ws)| ws.iter().copied())
                    .collect();
                let canvas = all
                    .iter()
                    .find(|w| w.id == w.parent_id.with(strip_key))?
                    .parent_id;
                all.into_iter().find(|w| w.id == canvas.with(key))
            })
            .expect("the canvas drew the field");
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

    fn click(&mut self, at: egui::Pos2) {
        self.step(vec![egui::Event::PointerMoved(at)]);
        self.step(vec![button(at, true)]);
        self.step(vec![button(at, false)]);
        self.step(vec![]);
    }

    /// Press at `at`, move the pointer by `by` over `steps` frames, release.
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

    /// The bus's first device, added and settled: (instance, its key).
    fn one_device(&mut self) -> (u8, I2cDeviceKey) {
        let at = self.add_button();
        self.click(at);
        self.settle();
        let (_, inst, _) = self.bus();
        (inst, self.key(0))
    }

    /// Where the strip of device `key` sits: a bare part of its box.
    fn grip(&self, key: I2cDeviceKey) -> (Seen, egui::Pos2) {
        let s = self.strip(key);
        let p = s.at(egui::pos2(s.local.center().x, s.local.top() + 7.0));
        (s, p)
    }

    /// The bus box's "+ device" button, on the screen.
    fn add_button(&self) -> egui::Pos2 {
        let (i, ..) = self.bus();
        let header = self.find(("vmod_box", i));
        // The header is the box less its bottom 30 px, where the button sits.
        let full = egui::Rect::from_min_max(
            header.local.min,
            egui::pos2(header.local.right(), header.local.bottom() + 30.0),
        );
        header.at(kids::add_button_rect(full).center())
    }

    /// The strip of the device `key`.
    fn strip(&self, key: I2cDeviceKey) -> Seen {
        let (_, inst, _) = self.bus();
        self.find(("i2c_child", inst, key))
    }

    fn key(&self, n: usize) -> I2cDeviceKey {
        I2cDeviceKey::Uid(self.devices()[n].2)
    }

    fn typing(&mut self, text: &str) {
        for c in text.chars() {
            self.step(vec![egui::Event::Text(c.to_string())]);
        }
    }

    fn press(&mut self, key: egui::Key) {
        self.step(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        self.step(vec![]);
    }
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// "+ device" on the bus box adds exactly one device, and its box is drawn
/// beside the bus - away from the chip, never over the bus box.
#[test]
fn plus_device_on_the_canvas_adds_one_box() {
    let mut b = Bench::new();
    assert!(b.devices().is_empty());
    let at = b.add_button();
    b.click(at);
    assert_eq!(b.devices().len(), 1, "one click, one device");
    b.settle();
    let strip = b.strip(b.key(0));
    let (i, ..) = b.bus();
    let header = b.find(("vmod_box", i));
    assert!(
        !kids::child_of_strip(strip.local).intersects(header.local),
        "the device box sits over its bus"
    );
    assert!(
        b.mcu().bus_reach.0 > 0.0,
        "the canvas was told how far it reaches"
    );
}

/// The name typed into a device box reaches the model only once, on Enter -
/// the file named after it is not renamed on every keystroke.
#[test]
fn a_name_typed_on_the_canvas_is_kept_on_enter() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let (_, inst, _) = b.bus();
    let key = b.key(0);
    let field = b.find_field(key, ("i2c_child_name", inst, key));
    b.click(field.screen.center());
    b.typing("oled");
    assert_eq!(b.devices()[0].0, "", "committed while typing");
    b.press(egui::Key::Enter);
    assert_eq!(b.devices()[0].0, "oled");
    assert_eq!(
        b.mcu().last_module_undo_label(),
        Some("Rename I2C device"),
        "and it can be undone"
    );
}

/// The ID field takes an address as a datasheet writes it.
#[test]
fn an_address_typed_on_the_canvas_is_read_as_hex() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let (_, inst, _) = b.bus();
    let key = b.key(0);
    let field = b.find_field(key, ("i2c_child_id", inst, key));
    b.click(field.screen.center());
    // Select what is there, then type over it.
    b.step(vec![egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    }]);
    b.typing("3c");
    b.press(egui::Key::Enter);
    assert_eq!(b.devices()[0].1, 0x3C);
}

/// A drag that starts on the ID field does not scrub the address. It was a
/// DragValue in the panel, where a slipped drag changed the value (and each
/// frame of it pushed an undo step); on the canvas it is text, so a drag only
/// selects.
#[test]
fn a_drag_on_the_id_field_leaves_the_address_alone() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let (_, inst, _) = b.bus();
    let key = b.key(0);
    let field = b.find_field(key, ("i2c_child_id", inst, key));
    let start = field.screen.center();
    b.step(vec![egui::Event::PointerMoved(start)]);
    b.step(vec![button(start, true)]);
    for k in 1..=6 {
        b.step(vec![egui::Event::PointerMoved(
            start + egui::vec2(8.0 * k as f32, 0.0),
        )]);
    }
    b.step(vec![button(start + egui::vec2(48.0, 0.0), false)]);
    b.settle();
    assert_eq!(b.devices()[0].1, 0x00);
}

/// Clicking a device selects its bus and opens its config - and a second
/// click, or a click on its sibling, does not toggle the bus off again.
#[test]
fn a_device_click_selects_its_bus_and_keeps_it_selected() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let at = b.add_button();
    b.click(at);
    b.settle();
    assert_eq!(b.devices().len(), 2);
    let (_, _, id) = b.bus();
    let (first, second) = (b.key(0), b.key(1));
    let s = b.strip(first);
    b.click(s.at(egui::pos2(s.local.center().x, s.local.top() + 7.0)));
    assert_eq!(b.mcu().selected_module.as_deref(), Some(id.as_str()));
    assert_eq!(b.mcu().selected_i2c_child(), Some((id.as_str(), first)));
    b.settle();
    let s = b.strip(second);
    b.click(s.at(egui::pos2(s.local.center().x, s.local.top() + 7.0)));
    assert_eq!(
        b.mcu().selected_module.as_deref(),
        Some(id.as_str()),
        "the sibling's click toggled the bus off"
    );
    assert_eq!(b.mcu().selected_i2c_child(), Some((id.as_str(), second)));
}

/// "x" on a device box asks first; Remove removes it, Cancel keeps it.
#[test]
fn removing_a_device_on_the_canvas_asks_first() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let (_, inst, _) = b.bus();
    let key = b.key(1);
    let strip = b.strip(key);
    let child = kids::child_of_strip(strip.local);
    b.click(strip.at(kids::remove_rect(child).center()));
    assert_eq!(b.devices().len(), 2, "removed without asking");
    assert_eq!(b.mcu().i2c_remove_confirm, Some((inst, key)));

    let strip = b.strip(key);
    let (_, cancel) = kids::confirm_rects(kids::child_of_strip(strip.local));
    b.click(strip.at(cancel.center()));
    assert_eq!(b.mcu().i2c_remove_confirm, None);
    assert_eq!(b.devices().len(), 2);

    b.mcu_mut().i2c_remove_confirm = Some((inst, key));
    b.settle();
    let strip = b.strip(key);
    let (yes, _) = kids::confirm_rects(kids::child_of_strip(strip.local));
    b.click(strip.at(yes.center()));
    assert_eq!(b.devices().len(), 1, "Remove removed it");
    assert!(
        b.try_find(("i2c_child", inst, key)).is_none(),
        "and its box is gone"
    );
}

/// A click on a bare part of a device box - its "ID" label - picks the
/// device. It used to fall through to the canvas background, which clears
/// the selection and folds every config.
#[test]
fn a_click_on_a_bare_part_of_a_device_box_picks_it() {
    let mut b = Bench::new();
    let at = b.add_button();
    b.click(at);
    b.settle();
    let (_, _, id) = b.bus();
    let key = b.key(0);
    let s = b.strip(key);
    let child = kids::child_of_strip(s.local);
    let label = egui::pos2(child.left() + 14.0, kids::id_rect(child).center().y);
    b.click(s.at(label));
    assert_eq!(b.mcu().selected_i2c_child(), Some((id.as_str(), key)));
}

/// What the panel's rows did waits for the canvas and is applied when it
/// draws - one batch with the canvas's own.
#[test]
fn the_panels_edits_are_applied_when_the_canvas_draws() {
    use crate::panels::mcu_module::mcu::gui::i2c_devices::I2cAct;
    use crate::panels::mcu_module::modules::I2cDeviceEdit;
    let mut b = Bench::new();
    let (_, inst, _) = b.bus();
    b.mcu_mut()
        .pending_i2c_acts
        .push((inst, I2cAct::Edit(I2cDeviceEdit::Add)));
    b.step(vec![]);
    assert_eq!(b.devices().len(), 1);
    assert!(b.mcu().pending_i2c_acts.is_empty(), "applied once");
}

/// A bus in Device "sensors" with its first device put in "display": each
/// Device draws ONE mat - the bus with its own device, and the other device
/// on its own. The column puts the bus's own device first; drawn in list
/// order, the display box would sit between the bus and its own device and
/// split "sensors" in two.
#[test]
fn a_bus_and_its_devices_draw_one_mat_per_device() {
    use crate::panels::mcu_module::modules::I2cDeviceEdit;
    let mut b = Bench::new();
    let (i, inst, _) = b.bus();
    let bus = b.mcu().modules[i].clone();
    b.mcu_mut().join_group_module(&bus, "sensors");
    b.mcu_mut().edit_i2c_device(inst, I2cDeviceEdit::Add);
    b.mcu_mut().edit_i2c_device(inst, I2cDeviceEdit::Add);
    let first = b.key(0);
    assert!(b.mcu_mut().join_group_i2c(inst, first, "display"));
    b.settle();
    let tabs = |name: &str| {
        b.mcu()
            .device_tabs
            .iter()
            .filter(|t| t.name == name)
            .count()
    };
    assert_eq!(
        tabs("sensors"),
        1,
        "the bus's Device split: {:?}",
        b.mcu().device_tabs
    );
    assert_eq!(
        tabs("display"),
        1,
        "the other Device: {:?}",
        b.mcu().device_tabs
    );
}

/// Two buses side by side on one side of the chip (a Pico: I2C0 on GP0/GP1,
/// I2C1 on GP2/GP3, both down its left edge), each with a column of devices:
/// the packer knows how far the first column runs, so the second bus and
/// its column start after it - no device box of one bus lies on the other's.
#[test]
fn two_buses_on_one_side_keep_their_columns_apart() {
    use crate::panels::mcu_module::modules::I2cDeviceEdit;
    let mut mcu = crate::panels::mcu_module::builtins::builtin_definitions()
        .into_iter()
        .find(|d| d.id == "rp2040_pico")
        .expect("built-in Pico")
        .build_mcu();
    for (name, func) in [
        ("GP0", PinFunction::I2cSda(0)),
        ("GP1", PinFunction::I2cScl(0)),
        ("GP2", PinFunction::I2cSda(1)),
        ("GP3", PinFunction::I2cScl(1)),
    ] {
        let num = mcu
            .iter_all_pins()
            .find(|p| p.name == name)
            .expect("a Pico pin")
            .number;
        mcu.find_pin_mut(num).expect("the pin").selected_function = func;
    }
    mcu.reconcile_modules();
    for inst in [0, 1] {
        for _ in 0..3 {
            assert!(mcu.edit_i2c_device(inst, I2cDeviceEdit::Add));
        }
    }
    let ctx = egui::Context::default();
    for pass in 0..3 {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(2400.0, 2400.0),
            )),
            time: Some(pass as f64 / 30.0),
            ..Default::default()
        };
        crate::headless::run_ui(&ctx, input, |ui| {
            mcu.draw(ui);
        });
    }
    let boxes = |inst: u8| -> Vec<egui::Rect> {
        let keys: Vec<I2cDeviceKey> = mcu
            .i2c_bus(inst)
            .unwrap()
            .rows()
            .iter()
            .map(|r| r.key)
            .collect();
        keys.into_iter()
            .map(|k| {
                ctx.viewport_for(egui::ViewportId::ROOT, |vp| {
                    vp.prev_pass
                        .widgets
                        .layers()
                        .flat_map(|(_, ws)| ws.iter())
                        .find(|w| w.id == w.parent_id.with(("i2c_child", inst, k)))
                        .map(|w| w.interact_rect)
                })
                .expect("the device box was drawn")
            })
            .collect()
    };
    let (a, b) = (boxes(0), boxes(1));
    // Both columns on the same side, so the test is about the packing.
    assert!(
        (a[0].center().x - b[0].center().x).abs() < 1.0,
        "{a:?} / {b:?}"
    );
    for ra in &a {
        for rb in &b {
            assert!(
                !ra.intersects(*rb),
                "a device of I2C0 lies on one of I2C1: {ra:?} / {rb:?}"
            );
        }
    }
}

/// The bus box carries no name any more: its devices do. The module's label
/// still names the variable in the code - it just is not painted on the bus.
#[test]
fn the_bus_box_carries_no_name() {
    let mut b = Bench::new();
    let (i, inst, _) = b.bus();
    if let ModuleConfig::I2c(c) = &mut b.mcu_mut().modules[i].config {
        c.custom_label = "oled".into();
    }
    b.settle();
    let texts = b.painted();
    assert!(
        texts.iter().any(|t| t == &format!("I2C{inst}")),
        "the bus box is drawn: {texts:?}"
    );
    assert!(
        !texts.iter().any(|t| t.contains(&format!("_i2c{inst}"))),
        "the bus still shows a variable name: {texts:?}"
    );
}

/// A device box is dragged on its own - out of its bus's column, to where the
/// pointer took it - and a click whose hand slipped does not pin it.
#[test]
fn a_device_box_moves_on_a_real_drag_only() {
    let mut b = Bench::new();
    let (inst, key) = b.one_device();
    let uid = match key {
        I2cDeviceKey::Uid(u) => u,
        other => panic!("{other:?}"),
    };
    let (before, at) = b.grip(key);
    // A click that slips 3 px off the box's top edge.
    let edge = egui::pos2(at.x, before.screen.top() + 1.0);
    b.gesture(edge, egui::vec2(0.0, -3.0), 2);
    assert!(
        b.mcu().i2c_child_pos.is_empty(),
        "a slipped click pinned it"
    );

    b.settle();
    let (before, at) = b.grip(key);
    b.gesture(at, egui::vec2(0.0, 30.0), 3);
    assert!(
        b.mcu().i2c_child_pos.contains_key(&(inst, uid)),
        "a drag pins it"
    );
    b.settle();
    let after = b.strip(key);
    let went = (after.local.top() - before.local.top()) * before.zoom;
    assert!(
        (went - 30.0).abs() < 1.0,
        "it followed the 30 px drag: {went}"
    );
}

/// A device in a module group moves with the group's tab - also a group made
/// of nothing but that device, whose tab moves nothing else.
#[test]
fn a_device_moves_with_its_module_group() {
    let mut b = Bench::new();
    let (inst, key) = b.one_device();
    assert!(b.mcu_mut().join_group_i2c(inst, key, "display"));
    b.settle();
    let tab = b
        .mcu()
        .device_tabs
        .iter()
        .find(|t| t.name == "display")
        .cloned()
        .expect("the group has a tab");
    let seen = b.find(("device_tab", &tab.name, tab.cluster));
    let before = b.strip(key);
    b.gesture(seen.screen.center(), egui::vec2(40.0, 0.0), 4);
    b.settle();
    assert!(
        b.mcu().device_is_manual("display"),
        "the group's device was moved"
    );
    let after = b.strip(key);
    let went = (after.local.left() - before.local.left()) * before.zoom;
    assert!((went - 40.0).abs() < 1.5, "it followed the group: {went}");
}
