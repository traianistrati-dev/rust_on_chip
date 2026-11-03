//! A Custom module's file left on the other GPIO api's traits, said in the
//! module's own panel on the Pins tab as well as in the System tab, drawn
//! through the MCU panel the way `AppIde::ui` draws it.
//!
//! Same bench shape as `i2c_canvas_tests`: what is checked is what a frame
//! painted, and the button is pressed where it was painted.

use super::{AppIde, McuTab};
use crate::panels::mcu_module::codegen::custom_file_for_other_gpio_api;
use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use eframe::egui;

const CHIP: &str = "stm32f103c8t6";
const PATH: &str = "src/pins/configs/custom_panel.rs";
const OTHER: &str = "src/pins/configs/custom_other.rs";
const HEADER: &str = "This module's file is on the other GPIO api's traits";
const SYSTEM_HEADER: &str = "A Custom module's file is on the other GPIO api's traits";
const ROW: &str = "src/pins/configs/custom_panel.rs imports embedded-hal 0.2's traits";
const BY_HAND: &str = "Change its embedded_hal use by hand.";

/// How the "panel" module's file stands.
#[derive(Clone, Copy, PartialEq)]
enum PanelFile {
    /// On the traits its pads speak.
    OwnApi,
    /// On 0.2's, in the line the IDE writes - one click swaps it.
    OtherApi,
    /// On 0.2's, with a comment of the user's after the line - by hand.
    OtherApiEdited,
}

struct Bench {
    ctx: egui::Context,
    app: AppIde,
    pass: u64,
}

impl Bench {
    /// Two applied Custom modules on the Portable api, whose pads cross the
    /// io.rs bridge and so speak 1.0: "panel" over PB12/PB13, its file as
    /// `panel` says, and "other" over PB14, its file on 1.0. Both files carry
    /// a line of the user's below the IDE's text, and both panels are open.
    fn new(panel: PanelFile) -> Self {
        let ctx = egui::Context::default();
        let mut app = AppIde::new(
            &eframe::CreationContext::_new_kittest(ctx.clone()),
            None,
            None,
        );
        app.startup_picker = None;
        app.selected_mcu_id = CHIP.to_owned();
        let mut mcu = AppIde::build_mcu_for(&app.mcu_registry, CHIP).expect("a built-in chip");
        let mut pad = |name: &str, func| {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{CHIP} has no {name}"))
                .number;
            mcu.find_pin_mut(num).expect("the pin").selected_function = func;
            num
        };
        let panel_pads = vec![
            pad("PB12", PinFunction::GpioOutput),
            pad("PB13", PinFunction::GpioInput),
        ];
        let other_pads = vec![pad("PB14", PinFunction::GpioOutput)];
        mcu.reconcile_modules();
        let mut ids = Vec::new();
        for (label, pads) in [("panel", panel_pads), ("other", other_pads)] {
            assert!(mcu.add_module(ModuleKind::Custom));
            let m = mcu
                .modules
                .iter_mut()
                .rfind(|m| m.kind == ModuleKind::Custom)
                .expect("the Custom module");
            *m.config.custom_label_mut() = label.to_owned();
            if let ModuleConfig::Custom(c) = &mut m.config {
                c.pins = pads.clone();
                c.applied_pins = pads;
            }
            ids.push(m.id.clone());
        }
        assert!(!mcu.custom_pads_speak_eh02(), "the Portable api");
        let file = |name: &str| {
            mcu.config_files()
                .into_iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .1
        };
        let ide = file("custom_panel.rs");
        let panel_text = match panel {
            PanelFile::OwnApi => ide,
            PanelFile::OtherApi => custom_file_for_other_gpio_api(&ide).expect("a traits line"),
            PanelFile::OtherApiEdited => custom_file_for_other_gpio_api(&ide)
                .expect("a traits line")
                .replacen("embedded-hal 0.2\n", "embedded-hal 0.2 (mine)\n", 1),
        };
        assert_ne!(
            panel == PanelFile::OtherApiEdited,
            custom_file_for_other_gpio_api(&panel_text).is_some(),
            "the swap is offered for the IDE's line only"
        );
        app.project_tree.user_src_files = vec![
            (PATH.to_owned(), format!("{panel_text}\nfn mine() {{}}\n")),
            (
                OTHER.to_owned(),
                format!("{}\nfn mine() {{}}\n", file("custom_other.rs")),
            ),
        ];
        app.mcu = Some(mcu);
        app.active_tab = McuTab::Pins;
        // The modules' settings open, as a click on each row leaves them.
        for id in &ids {
            egui::collapsing_header::CollapsingState::load_with_default_open(
                &ctx,
                egui::Id::new(("vmod_hdr", id.as_str())),
                true,
            )
            .store(&ctx);
        }
        let mut b = Self { ctx, app, pass: 0 };
        for _ in 0..4 {
            b.painted(vec![]);
        }
        b
    }

    /// One frame, returning every string it painted and where.
    fn painted(&mut self, events: Vec<egui::Event>) -> Vec<(String, egui::Rect)> {
        fn walk(s: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match s {
                egui::Shape::Text(t) => {
                    out.push((t.galley.text().to_owned(), t.visual_bounding_rect()));
                }
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        self.pass += 1;
        // Tall enough to show both open panels whole: egui paints no text
        // outside the clip, so a notice in the panel below would go unseen.
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 3000.0),
            )),
            time: Some(self.pass as f64 / 30.0),
            predicted_dt: 1.0 / 30.0,
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let out = crate::headless::run_ui(&self.ctx, input, |ui| app.show_mcu_panel(ui));
        let mut texts = Vec::new();
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut texts));
        texts
    }

    /// Press and release where `needle` was painted.
    fn click(&mut self, needle: &str) {
        let texts = self.painted(vec![]);
        let at = texts
            .iter()
            .find(|(t, _)| t == needle)
            .unwrap_or_else(|| panic!("`{needle}` was not painted: {texts:#?}"))
            .1
            .center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.painted(vec![egui::Event::PointerMoved(at)]);
        self.painted(vec![button(true)]);
        self.painted(vec![button(false)]);
    }
}

/// How many painted strings hold `needle`.
fn count(texts: &[(String, egui::Rect)], needle: &str) -> usize {
    texts.iter().filter(|(t, _)| t.contains(needle)).count()
}

#[test]
fn the_custom_panel_names_its_file_on_the_other_api_and_swaps_it() {
    let mut b = Bench::new(PanelFile::OtherApi);
    let texts = b.painted(vec![]);
    // Once: in "panel"'s own panel, not in "other"'s beside it.
    assert_eq!(count(&texts, HEADER), 1, "{texts:#?}");
    assert_eq!(count(&texts, ROW), 1, "{texts:#?}");
    assert_eq!(count(&texts, "Switch to 1.0"), 1, "{texts:#?}");
    assert_eq!(count(&texts, OTHER), 0, "{texts:#?}");
    assert_eq!(count(&texts, BY_HAND), 0, "{texts:#?}");
    b.click("Switch to 1.0");
    assert_eq!(b.app.custom_use_swap.as_deref(), Some(PATH));

    // The System tab names it under its own header, with the same swap.
    b.app.custom_use_swap = None;
    b.app.active_tab = McuTab::System;
    let texts = b.painted(vec![]);
    assert_eq!(count(&texts, SYSTEM_HEADER), 1, "{texts:#?}");
    assert_eq!(count(&texts, ROW), 1, "{texts:#?}");
    assert_eq!(count(&texts, OTHER), 0, "{texts:#?}");
    b.click("Switch to 1.0");
    assert_eq!(b.app.custom_use_swap.as_deref(), Some(PATH));
}

#[test]
fn a_custom_file_edited_on_its_traits_line_is_named_without_a_swap() {
    let mut b = Bench::new(PanelFile::OtherApiEdited);
    for tab in [McuTab::Pins, McuTab::System] {
        b.app.active_tab = tab;
        let texts = b.painted(vec![]);
        assert_eq!(count(&texts, ROW), 1, "{texts:#?}");
        assert_eq!(count(&texts, BY_HAND), 1, "{texts:#?}");
        assert_eq!(count(&texts, "Switch to"), 0, "{texts:#?}");
    }
}

#[test]
fn the_custom_panel_says_nothing_of_a_file_on_its_pins_api() {
    let mut b = Bench::new(PanelFile::OwnApi);
    let texts = b.painted(vec![]);
    // Both panels are open - each drew its "Add pin" box - and quiet.
    assert_eq!(count(&texts, "Add pin"), 2, "{texts:#?}");
    assert_eq!(count(&texts, HEADER), 0, "{texts:#?}");
    assert_eq!(count(&texts, "Switch to"), 0, "{texts:#?}");
    b.app.active_tab = McuTab::System;
    let texts = b.painted(vec![]);
    assert_eq!(count(&texts, SYSTEM_HEADER), 0, "{texts:#?}");
}
