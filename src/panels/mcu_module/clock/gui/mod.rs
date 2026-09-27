//! Clock-tab GUI — the data-driven graph is the only clock model.
//!
//! [`draw_graph_clock`] renders an imported/built-in [`GraphClock`]:
//! one toolbar (Reset, presets, view toggles, tools), a fixed Info footer, and
//! zoom toolbar, and the interactive diagram (shared static renderer + widget
//! overlay editing graph node states) inside an [`egui::Scene`] that pans and
//! zooms it like the Pins canvas. The old typed `Stm32f1Clock` UI
//! (`draw_clock_tree`, `interactive_nodes`) was retired — its visuals live on
//! as data in `stm32f1_layout`.
//!
//! **Edit mode** turns the same canvas into the tree CONSTRUCTOR: a palette adds
//! nodes, boxes are dragged and wired through their ports, and a properties
//! panel edits the selected node. The rules live in
//! [`edit`](super::graph::edit); this module is the mouse and the layout.

pub mod diagram;

use eframe::egui;
use egui_phosphor::regular as ph;

use super::compute::frequencies;
use super::graph::layout::ValueSrc;
use super::graph::validate::ceiling_for;
use super::graph::{
    ClockGraph, GraphClock, edit, evaluate, graph_to_stm32f1, over_limits, stm32f1_graph,
    value_from_graph,
};
use super::model::{ClockConfig, ClockLimits};
use super::presets::{ClockPreset, stm32f1_presets};
use super::validate::{Severity, warnings};
use crate::panels::mcu_module::structure_config::ClockPositions;

/// The Clock tab's per-project state, owned by the app.
///
/// Bundled rather than passed as three more parameters: it is one thing —
/// "what this project remembers about its clock view" — and the next addition
/// then costs nothing at the call sites.
#[derive(Default)]
pub struct ClockUiState {
    /// Node positions the user dragged; persisted in `project_structure.config`.
    pub positions: ClockPositions,
    /// One-line result of the last edit action, shown under the palette row.
    /// Session-only.
    pub note: String,
    /// Show the FIELDS list beside the diagram. Persisted per project — it is a
    /// working preference, not a momentary one.
    pub fields: bool,
    /// Filter for the Fields list. Session-only, unlike `fields`: a search is
    /// something you are doing right now, not how you like the tab set up.
    pub field_search: String,
}

/// What the Clock tab wants the app to do after this frame.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClockTabOut {
    /// The clock configuration changed — regenerate `main.rs`.
    pub changed: bool,
    /// The user asked to write the edited tree into the chip's `.ron`
    /// definition. Only the app can do it: it owns the registry.
    pub save_to_definition: bool,
    /// The user deleted the tree — the chip goes back to having no clock, and
    /// the tab back to [`draw_no_clock`]. Only the caller owns the
    /// [`ClockConfig`] enum, so the tab can only ask.
    pub remove_clock: bool,
    /// A DIFFERENT tree was installed from the toolbar (an import, a template,
    /// the minimal spine). The graph itself is already replaced in place; this
    /// says the chip's "factory" snapshot behind Reset — and the hand-written
    /// default — must be re-taken from it, which again only the caller can do.
    pub adopt_defaults: bool,
}

/// Render the Clock tab for a graph clock.
///
/// `limits` are the chip's datasheet ceilings; `presets` the chip-specific
/// one-click configs (empty + `family == "stm32f1"` → built-in F103 presets);
/// `defaults` the chip's factory node states behind the "Reset" button
/// (`None` → no Reset button); `family` gates the family-specific extras
/// (presets fallback + footnote validation via the `graph_to_stm32f1` bridge)
/// and names the ids code generation reads.
pub fn draw_graph_clock(
    ui: &mut egui::Ui,
    gc: &mut GraphClock,
    limits: &ClockLimits,
    presets: &[ClockPreset],
    defaults: Option<&ClockGraph>,
    family: &str,
    state: &mut ClockUiState,
    clock_manual: &mut bool,
) -> ClockTabOut {
    // Destructured so the parts keep their old names below.
    let ClockUiState {
        positions,
        note,
        fields,
        field_search,
    } = state;
    let mut out = ClockTabOut::default();
    let mut changed = false;
    let is_stm32f1 = family == "stm32f1";

    // A graph without a hand-authored diagram (AI-imported clock, or any future
    // family) gets one computed from its topology — filled ONCE and kept on the
    // live model, so it isn't recomputed per frame. Does NOT set `changed`: the
    // layout is cosmetic, codegen ignores it.
    if gc.layout.is_empty() {
        gc.layout = super::graph::auto_layout(&gc.graph);
    }

    // ── Reset + presets (thin top bar) ───────────────────────────────────────
    // Presets are typed `Stm32f1Clock` configs; applying one expands it to
    // graph node states and adopts them by id (no-op on non-F103 graphs).
    let family_presets;
    let presets: &[ClockPreset] = if presets.is_empty() && is_stm32f1 {
        family_presets = stm32f1_presets();
        &family_presets
    } else {
        presets
    };

    // Evaluated AFTER any preset click so the footer reflects the new state.
    let mut freqs = evaluate(&gc.graph);

    // ── Fixed footer: Info (always visible) ─────────────────────────────────
    egui::Panel::bottom("graph_clock_footer")
        .resizable(true)
        .default_size(190.0)
        .min_size(100.0)
        .show(ui, |ui| {
            // Info alone now. The "Frequencies" half listed the OUTPUTS, which
            // the Fields list ends with instead — one place for every value,
            // rather than the results being somewhere the inputs are not.
            ui.strong("Info");
            egui::ScrollArea::vertical()
                .id_salt("ginfo_scroll")
                .show(ui, |ui| {
                    if graph_info_zone(ui, gc, limits, &freqs, is_stm32f1, family, *clock_manual) {
                        changed = true;
                    }
                });
        });

    // ── View state (session-only, never persisted) ───────────────────────────
    // Deliberately in `ui.data` temp, like the zoom it replaces: zoom/pan is a
    // view preference, so it must not reach `mcu.config`, `project_structure.
    // config` or the Git snapshot. `sig` re-fits the view whenever the graph
    // itself changes (Reset, preset, chip switch, AI import) — one check that
    // covers every call site, instead of clearing a latch at each of them.
    let view_id = egui::Id::new("graph_clock_view");
    let mut view: ClockView = ui.data(|d| d.get_temp(view_id)).unwrap_or_default();
    let sig = graph_signature(&gc.graph);
    if view.sig != sig {
        view.sig = sig;
        view.adjusted = false;
    }

    // Saved node positions (from `project_structure.config`) are applied over the
    // generated layout — once, whenever either side changes, since re-deriving
    // per frame would rebuild every dropdown's option list.
    // Only a DERIVED layout can take them: they are `NodeBox` positions, and a
    // hand-authored figure has none (its own positions are the primitives').
    let pos_sig = positions_signature(positions);
    if !gc.layout.nodes.is_empty() && (view.pos_sig != pos_sig || view.layout_sig != sig) {
        view.pos_sig = pos_sig;
        view.layout_sig = sig;
        apply_positions(gc, positions);
    }

    // ── Zoom toolbar ─────────────────────────────────────────────────────────
    // The buttons drive the Scene (same effect as Ctrl+± / Ctrl+0) and the
    // percentage is the Scene's real scale, so it tells the truth after a wheel
    // zoom or a drag too. The clicks are only RECORDED here — applying them
    // needs the viewport rect, which is known once this row has been laid out.
    let mut zoom_click: Option<f32> = None;
    let mut replaced = false;
    let mut manual_changed = false;
    ui.horizontal_wrapped(|ui| {
        // The fields list sits BESIDE the diagram rather than replacing it, so
        // the toggle is a visibility switch, not a view switch.
        if ui
            .add(egui::Button::new(format!("{} Fields", ph::LIST_BULLETS)).selected(*fields))
            .on_hover_text("Show every selectable value as a list beside the diagram")
            .clicked()
        {
            *fields = !*fields;
        }
        ui.separator();
            // Reset — back to the chip definition's factory tree. Disabled (and
            // grey) while the config already IS the default, so the button also
            // reads as a "modified" indicator.
            if let Some(def) = defaults {
                let dirty = !gc.graph.states_match(def);
                let color = if dirty {
                    egui::Color32::from_rgb(220, 100, 80)
                } else {
                    egui::Color32::GRAY
                };
                let btn = egui::Button::new(
                    egui::RichText::new(format!("{} Reset", ph::ARROW_COUNTER_CLOCKWISE))
                        .color(color),
                );
                if ui
                    .add_enabled(dirty, btn)
                    .on_hover_text("Restore this chip's default clock configuration")
                    .on_disabled_hover_text("Clock is already at the chip default")
                    .clicked()
                {
                    gc.graph.adopt_states(def);
                    changed = true;
                }
                if !presets.is_empty() {
                    ui.separator();
                }
            }
            if !presets.is_empty() {
                ui.label(egui::RichText::new("Presets:").strong());
                for p in presets {
                    if ui.button(&p.name).on_hover_text(&p.description).clicked() {
                        gc.graph.adopt_states(&stm32f1_graph(&p.config));
                        changed = true;
                    }
                }
                ui.separator();
            }
        ui.separator();

        ui.label("Zoom:");
        if ui.small_button("−").on_hover_text("Ctrl+−").clicked() {
            zoom_click = Some(1.2);
        }
        if ui
            .small_button(format!("{:.0}%", view.last_scale * 100.0))
            .on_hover_text("Fit the whole diagram (Ctrl+0)")
            .clicked()
        {
            view.adjusted = false;
        }
        if ui.small_button("+").on_hover_text("Ctrl++").clicked() {
            zoom_click = Some(1.0 / 1.2);
        }
        ui.separator();


        // One "Tools" menu for the two things that CHANGE the tree, rather
        // than two toggles competing with the view controls beside them.
        ui.menu_button(format!("{} Tools", ph::WRENCH), |ui| {
            if ui
                .add(
                    egui::Button::new(format!("{} Edit the tree", ph::ARROWS_OUT_CARDINAL))
                        .selected(view.edit),
                )
                .on_hover_text(
                    "Build the tree: drag nodes, add / delete them, wire them up. A hand-drawn figure is edited in place - only its wires stay where its author routed them.",
                )
                .clicked()
            {
                view.edit = !view.edit;
                view.selected = None;
                view.linking = None;
                ui.close();
            }
            // Re-space a figure that was imported before, or arranged into a
            // mess. Works off the CURRENT positions, so it keeps the ordering
            // (which column is left of which) and only fixes the spacing.
            //
            // Three settings because there is a real trade-off and no right
            // answer: merging vendor rows packs the figure down hard — a U5 goes
            // from 45 rows to a handful — but rows that merge are rows that no
            // longer align, and alignment is what keeps wires straight.
            if !gc.layout.nodes.is_empty() {
                ui.menu_button(format!("{} Tidy up the layout", ph::ARROWS_IN), |ui| {
                    for spread in super::graph::auto_layout::Spread::ALL {
                        if ui
                            .button(spread.label())
                            .on_hover_text(match spread {
                                super::graph::auto_layout::Spread::Min => {
                                    "Densest - rows merge freely, so the whole tree fits on screen sooner"
                                }
                                super::graph::auto_layout::Spread::Mid => {
                                    "A middle ground between size and alignment"
                                }
                                super::graph::auto_layout::Spread::Max => {
                                    "Loosest - one row per row the vendor drew, best aligned, largest canvas"
                                }
                            })
                            .clicked()
                        {
                            let boxes = std::mem::take(&mut gc.layout.nodes);
                            gc.layout = super::graph::derive(
                                &gc.graph,
                                super::graph::auto_layout::respace(&gc.graph, boxes, spread),
                            );
                            positions.clear();
                            view.pos_sig = positions_signature(positions);
                            view.adjusted = false;
                            (*note) = format!("Layout re-spaced ({}).", spread.label());
                            ui.close();
                        }
                    }
                });
            }
            ui.separator();
            if let Some(ClockConfig::Graph(new_gc)) = tree_sources(ui, family, limits, note) {
                *gc = new_gc;
                replaced = true;
                ui.close();
            }
            ui.separator();
            if ui
                .button(
                    egui::RichText::new(format!("{}  Remove this tree", ph::TRASH))
                        .color(egui::Color32::from_rgb(220, 100, 80)),
                )
                .on_hover_text(
                    "Back to 'no clock tree'. The chip's .ron definition is untouched — this \
                     only drops the tree from the project until you save it to the chip.",
                )
                .clicked()
            {
                out.remove_clock = true;
                ui.close();
            }
        });
        ui.separator();
        if clock_manual_checkbox(ui, family, clock_manual) {
            manual_changed = true;
        }

        ui.label(
            egui::RichText::new(if view.edit {
                "· click a box to select · drag it to move · ports (○) wire nodes up"
            } else {
                "· wheel = zoom · drag = pan"
            })
            .size(10.0)
            .color(egui::Color32::from_rgb(150, 158, 172)),
        );
    });

    // A different tree is now in `gc`. Everything derived from the old one has
    // to go: the saved node positions address ids that may not exist, and the
    // frequencies above were evaluated before the swap.
    if manual_changed {
        changed = true;
    }
    // The toolbar now sits BELOW the footer in code (the footer is a bottom
    // panel, so it must be declared first), which means a Reset or a preset
    // clicked up there lands after the footer has drawn. Re-evaluate so the
    // DIAGRAM at least shows the new state this frame; the footer catches up on
    // the next one, which is the same frame the user sees the click land.
    if changed {
        freqs = evaluate(&gc.graph);
    }
    if replaced {
        positions.clear();
        if gc.layout.is_empty() {
            gc.layout = super::graph::auto_layout(&gc.graph);
        }
        freqs = evaluate(&gc.graph);
        view.selected = None;
        view.linking = None;
        view.pos_sig = positions_signature(positions);
        view.adjusted = false;
        changed = true;
        out.adopt_defaults = true;
    }

    // ── Palette row (edit mode) ──────────────────────────────────────────────
    if view.edit {
        let mut structural = false;
        ui.horizontal_wrapped(|ui| {
            ui.menu_button(format!("{} Add node", ph::PLUS), |ui| {
                for kind in edit::PaletteKind::ALL {
                    if ui.button(kind.label()).clicked() {
                        // Below the diagram's current extent, so a new node is
                        // never dropped on top of an existing one. The graph
                        // signature changes, which re-fits the view onto it.
                        let (_, h) = gc.layout.bounds();
                        let id = add_node_to(gc, kind, 46.0, h);
                        view.selected = Some(id);
                        structural = true;
                        ui.close();
                    }
                }
            });

            let has_sel = view.selected.is_some();
            if ui
                .add_enabled(has_sel, egui::Button::new(format!("{} Delete", ph::TRASH)))
                .on_hover_text("Remove the selected node and its wires")
                .clicked()
                && let Some(id) = view.selected.take()
            {
                remove_node_from(gc, &id);
                positions.remove(&id);
                structural = true;
            }

            if !positions.is_empty()
                && ui
                    .button("Auto-arrange")
                    .on_hover_text(
                        "Drop the saved positions and lay the diagram out from the graph again",
                    )
                    .clicked()
            {
                positions.clear();
                gc.layout = super::graph::auto_layout(&gc.graph);
                view.pos_sig = positions_signature(positions);
                view.adjusted = false;
            }

            // THE persistence path for structural edits: the graph lives in the
            // chip's definition, so that is where it must be written back.
            if ui
                .button(format!("{} Save to chip", ph::FLOPPY_DISK))
                .on_hover_text(
                    "Write this clock tree into the chip's .ron definition, so every future \
                     project with this chip gets it",
                )
                .clicked()
            {
                out.save_to_definition = true;
            }

            if ui
                .button("Export .ron…")
                .on_hover_text("Save the tree (graph + diagram) to a file of your choosing")
                .clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .set_file_name("clock.ron")
                    .add_filter("RON", &["ron"])
                    .save_file()
            {
                (*note) = match std::fs::write(&path, super::graph::export_clock_ron(gc)) {
                    Ok(()) => format!("Saved to {}", path.display()),
                    Err(e) => format!("Could not save: {e}"),
                };
            }

            if let Some(from) = view.linking.clone() {
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "Wiring from `{from}` — click an input port (Esc to cancel)"
                    ))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(255, 232, 150)),
                );
            }
        });
        if structural {
            changed = true;
            view.pos_sig = positions_signature(positions);
            view.adjusted = false;
        }
        if !(*note).is_empty() {
            ui.label(
                egui::RichText::new(&(*note))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(150, 200, 160)),
            );
        }
    }

    // ── Properties panel (edit mode) ─────────────────────────────────────────
    if view.edit {
        let mut edited = false;
        egui::Panel::right("clock_edit_props")
            .resizable(true)
            .default_size(260.0)
            .show(ui, |ui| {
                edited = properties_panel(ui, gc, &mut view, positions, family, note);
            });
        if edited {
            changed = true;
        }
    }

    // ── Fields (left, beside the diagram) ────────────────────────────────────
    if *fields {
        egui::Panel::left("clock_fields")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                if fields_panel(ui, gc, limits, &freqs, field_search) {
                    changed = true;
                }
            });
    }

    // The rect the Scene will fill — the cursor-anchored zoom below replicates
    // the Scene's own scene→screen fit from it, so it must be exactly that rect
    // (measured AFTER the toolbar and the properties panel, not before).
    let outer = ui.available_rect_before_wrap();
    let avail = ui.available_size_before_wrap();
    if let Some(f) = zoom_click {
        zoom_by(&mut view, f, avail);
    }

    // ── Diagram + interactive widgets, inside a pan/zoom Scene ───────────────
    // The layout is painted at its natural size (`ClockLayout::bounds`) and the
    // Scene owns the view, so a full datasheet clock tree is navigated instead
    // of being shrunk to fit — see the Pins canvas for the same pattern.
    let mut scene_rect = view.scene;
    let mut content_bounds = egui::Rect::NOTHING;
    let mut hits = diagram::EditHits::default();

    // The draggable handles. A DERIVED layout owns its boxes; a HAND-AUTHORED
    // figure has none, so they are derived from the primitives that name a node
    // — which is what lets such a figure be edited in place instead of being
    // regenerated. `before` is kept so a drag can be applied as a delta.
    let derived = !gc.layout.nodes.is_empty();
    let mut handles = if derived {
        gc.layout.nodes.clone()
    } else {
        gc.layout.node_anchors()
    };
    let before = handles.clone();

    // egui's Scene PANS on a plain wheel; turn that into a cursor-anchored zoom
    // (Ctrl+wheel stays the Scene's own zoom) by replicating its scene→screen
    // fit, zooming about the pointer, then consuming the scroll.
    let fit_tf = |scene: egui::Rect| -> egui::emath::TSTransform {
        let scale = (outer.size() / scene.size())
            .min_elem()
            .clamp(ZOOM_MIN, ZOOM_MAX);
        egui::emath::TSTransform::from_translation(
            outer.center().to_vec2() - scale * scene.center().to_vec2(),
        ) * egui::emath::TSTransform::from_scaling(scale)
    };
    // Only when the diagram is the TOPMOST thing under the pointer: anything
    // drawn over it (the New Project chip list, a window) scrolls its own
    // content, and what it leaves of the wheel must not zoom the diagram.
    let hover = ui.input(|i| i.pointer.hover_pos());
    let ptr = hover.filter(|_| ui.rect_contains_pointer(outer));
    // Refused, the wheel must not reach the Scene either: its hit test reaches a
    // few points in under the edge of whatever covers the diagram, and it would
    // PAN by the wheel there. Held back, then handed back after it.
    let held = if ptr.is_none() && hover.is_some_and(|p| outer.contains(p)) {
        ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta))
    } else {
        egui::Vec2::ZERO
    };
    let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.command));
    if let Some(ptr) = ptr
        && scroll_y != 0.0
        && !ctrl
        && outer.contains(ptr)
        && scene_rect.is_finite()
        && scene_rect.size() != egui::Vec2::ZERO
    {
        let to_global = fit_tf(scene_rect);
        let cur = to_global.scaling;
        let z = ((scroll_y * 0.002).exp() * cur).clamp(ZOOM_MIN, ZOOM_MAX) / cur;
        let p = to_global.inverse() * ptr;
        let new = to_global
            * egui::emath::TSTransform::from_translation(p.to_vec2())
            * egui::emath::TSTransform::from_scaling(z)
            * egui::emath::TSTransform::from_translation(-p.to_vec2());
        let new_rect = new.inverse() * outer;
        if new_rect.is_finite() && new_rect.size() != egui::Vec2::ZERO {
            scene_rect = new_rect;
            view.scene = new_rect;
            view.adjusted = true;
        }
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }

    let scene = crate::app::helpers::scene::show(
        egui::Scene::new()
            .zoom_range(ZOOM_MIN..=ZOOM_MAX)
            .drag_pan_buttons(egui::DragPanButtons::PRIMARY | egui::DragPanButtons::MIDDLE),
        ui,
        &mut scene_rect,
        |ui| {
            let (rect, tf) = {
                let resolve = |src: &ValueSrc| value_from_graph(src, &freqs);
                let is_mux = |id: &str| {
                    matches!(
                        gc.graph.node(id).map(|n| &n.kind),
                        Some(super::graph::model::NodeKind::Mux { .. })
                    )
                };
                diagram::draw_static_diagram(ui, &gc.layout, limits, resolve, is_mux)
            };
            content_bounds = ui.min_rect();
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                if view.edit {
                    // Edit mode REPLACES the value controls — they sit on exactly
                    // these rectangles, so both at once would fight for the drag.
                    hits = diagram::edit_nodes(
                        ui,
                        &tf,
                        &mut handles,
                        view.selected.as_deref(),
                        view.linking.as_deref(),
                    );
                } else if diagram::interactive_graph(ui, &tf, &mut gc.graph, &gc.layout.widgets) {
                    changed = true;
                }
            });
        },
    );
    if held != egui::Vec2::ZERO {
        ui.input_mut(|i| i.smooth_scroll_delta += held);
    }

    // A drag moved boxes: rebuild the primitives from them and record the
    // positions for `project_structure.config`. NOT `changed` — the layout is
    // cosmetic and must not regenerate `main.rs` on every mouse move.
    if hits.dragged {
        if derived {
            gc.layout.nodes = handles;
            let boxes = std::mem::take(&mut gc.layout.nodes);
            gc.layout = super::graph::derive(&gc.graph, boxes);
            *positions = moved_positions(gc);
            view.pos_sig = positions_signature(positions);
        } else {
            // In place: move exactly the primitives that belong to the node, so
            // the hand-drawn figure survives the edit. Its wires are not moved —
            // nothing says which node a routed polyline belongs to.
            for (now, was) in handles.iter().zip(&before) {
                let (dx, dy) = (now.x - was.x, now.y - was.y);
                if dx != 0.0 || dy != 0.0 {
                    gc.layout.move_node(&now.node, dx, dy);
                }
            }
        }
    }
    if let Some(id) = hits.select {
        view.selected = Some(id);
    }
    // Wiring: an output port arms the link, an input port completes it.
    if let Some(id) = hits.out_port {
        view.linking = Some(id);
        (*note).clear();
    }
    if let Some(to) = hits.in_port
        && let Some(from) = view.linking.take()
    {
        match edit::connect(&mut gc.graph, &from, &to) {
            Ok(input) => {
                let boxes = std::mem::take(&mut gc.layout.nodes);
                gc.layout = super::graph::derive(&gc.graph, boxes);
                (*note) = format!("Wired `{from}` -> `{to}` (input {input}).");
                changed = true;
            }
            Err(e) => (*note) = e,
        }
    }
    if view.edit && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        view.linking = None;
    }

    // The Scene writes back the view it owns (drag-pan + Ctrl+wheel) — latch it.
    if scene.response.changed() {
        view.scene = scene_rect;
        view.adjusted = true;
    }

    // Ctrl+± / Ctrl+0, consumed only over the canvas so the editor keeps its own.
    if scene.response.contains_pointer() {
        let (reset, factor) = ui.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            if i.consume_key(cmd, egui::Key::Num0) {
                (true, None)
            } else if i.consume_key(cmd, egui::Key::Plus) || i.consume_key(cmd, egui::Key::Equals) {
                (false, Some(1.0 / 1.2)) // smaller rect = zoom IN
            } else if i.consume_key(cmd, egui::Key::Minus) {
                (false, Some(1.2))
            } else {
                (false, None)
            }
        });
        if reset {
            view.adjusted = false;
        } else if let Some(f) = factor {
            // `scene_rect` post-show is the live view — zoom relative to it.
            view.scene = scene_rect;
            zoom_by(&mut view, f, avail);
        }
    }

    // What the toolbar percentage shows next frame — the scale the Scene ended
    // up at, whoever changed it (button, wheel, drag, Ctrl+key, auto-fit).
    view.last_scale = scale_of(scene_rect, outer);

    // Auto-fit until the user takes over. Padded to at least the panel size so a
    // diagram smaller than the panel sits at 100% instead of being blown up.
    if !view.adjusted {
        view.scene = if content_bounds.is_finite() {
            egui::Rect::from_center_size(
                content_bounds.center(),
                egui::vec2(
                    content_bounds.width().max(avail.x),
                    content_bounds.height().max(avail.y),
                ),
            )
        } else {
            content_bounds
        };
    }
    ui.data_mut(|d| d.insert_temp(view_id, view));

    out.changed = changed;
    out
}

// ── No clock yet ──────────────────────────────────────────────────────────────

/// The Clock tab for a chip whose definition carries no tree.
///
/// This used to be a sentence and nothing else — *"not modelled yet"* — which was
/// true of the DEFINITION but not of the IDE: the editor can build a tree from
/// nothing and the importers can fetch an exact one. Every family outside the
/// nine `ClockChoice::for_family` knows (H5, H7, U5, F3, C0, WB, WL…) landed
/// here, including the chips whose clock code is now hand-written — so the one
/// place they could look at their clock was blank.
///
/// Returns the clock the user chose to create, if any. Importing is offered
/// FIRST and drawing last, deliberately: an H5 tree is 178 nodes — two clicks to
/// import, an afternoon to draw.
pub fn draw_no_clock(
    ui: &mut egui::Ui,
    chip: &str,
    family: &str,
    limits: &ClockLimits,
    state: &mut ClockUiState,
) -> Option<ClockConfig> {
    use crate::panels::mcu_module::mcu_form::ClockChoice;

    let mut chosen: Option<ClockConfig> = None;
    egui::ScrollArea::vertical()
        .id_salt("no_clock_scroll")
        .show(ui, |ui| {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new(format!("{chip} has no clock tree yet"))
                        .size(15.0)
                        .strong(),
                );
                ui.label(
                    egui::RichText::new(
                        "Give it one and the diagram, the fields list and the frequency checks \
                         all come alive. Save it to the chip afterwards — a tree belongs to the \
                         chip definition, not to this project.",
                    )
                    .size(11.0)
                    .color(egui::Color32::GRAY),
                );
                ui.add_space(14.0);

                chosen = tree_sources(ui, family, limits, &mut state.note);

                let template = ClockChoice::for_family(family);
                ui.add_space(12.0);
                if !state.note.is_empty() {
                    ui.label(
                        egui::RichText::new(&state.note)
                            .size(11.0)
                            .color(egui::Color32::from_rgb(150, 200, 160)),
                    );
                }
                if template == ClockChoice::None {
                    ui.add_space(8.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  This IDE ships no template for `{family}` — importing is the \
                             quickest route.",
                            ph::INFO
                        ))
                        .size(10.0)
                        .color(egui::Color32::GRAY),
                    );
                }
            });
        });
    chosen
}

/// Every way of putting a tree in front of the user, as a column of buttons.
///
/// Shared by the empty state and by the toolbar's "Tree" menu, so the two cannot
/// drift apart — and so a tree that turned out to be the wrong one (the generic
/// spine, a template that isn't this part) is replaced from the same list that
/// created it, instead of being a one-way door.
///
/// Importing comes FIRST and drawing last, deliberately: an H5 tree is 178 nodes
/// — two clicks to import, an afternoon to draw.
fn tree_sources(
    ui: &mut egui::Ui,
    family: &str,
    limits: &ClockLimits,
    note: &mut String,
) -> Option<ClockConfig> {
    use crate::panels::mcu_module::mcu_form::ClockChoice;

    let mut chosen: Option<ClockConfig> = None;

    // 1. The family template, when this IDE ships one.
    let template = ClockChoice::for_family(family);
    if template != ClockChoice::None
        && ui
            .button(format!(
                "{}  Use the {} template",
                ph::LIGHTNING,
                template.label()
            ))
            .on_hover_text("The tree this IDE ships for the family, ready to tune")
            .clicked()
    {
        chosen = Some(template.to_def().to_config(limits));
        *note = format!("Started from the {} template.", template.label());
    }

    // 2. The exact tree for THIS part, from a CubeMX installation.
    if ui
        .button(format!("{}  Import from a chip XML (+ CubeMX)…", ph::CPU))
        .on_hover_text(
            "Pick this part in STM32_open_pin_data (mcu/STM32….xml). Its ClockTree and RCC \
             version select the exact CubeMX files.",
        )
        .clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter("STM32 chip XML", &["xml"])
            .pick_file()
    {
        match import_chip_xml(&path, family) {
            Ok((gc, msg)) => {
                chosen = Some(ClockConfig::Graph(gc));
                *note = msg;
            }
            Err(e) => *note = e,
        }
    }

    // 3. A family's CubeMX clock file, when the pin-data repo is not at hand.
    if ui
        .button(format!("{}  Import a CubeMX clock XML…", ph::FILE_CODE))
        .on_hover_text("db/plugins/clock/STM32<FAMILY>.xml from a CubeMX install")
        .clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter("CubeMX clock XML", &["xml"])
            .pick_file()
    {
        match import_cubemx_family(&path, family) {
            Ok((gc, msg)) => {
                chosen = Some(ClockConfig::Graph(gc));
                *note = msg;
            }
            Err(e) => *note = e,
        }
    }

    // 4. A tree someone already exported.
    if ui
        .button(format!("{}  Import a clock .ron…", ph::DOWNLOAD_SIMPLE))
        .on_hover_text("A GraphClock or a bare ClockGraph, validated on import")
        .clicked()
        && let Some(path) = rfd::FileDialog::new()
            .add_filter("RON", &["ron"])
            .pick_file()
    {
        *note = match std::fs::read_to_string(&path)
            .map_err(|e| format!("Could not read the file: {e}"))
            .and_then(|text| super::graph::parse_clock_ron(&text))
        {
            Ok(gc) => {
                let n = gc.graph.nodes.len();
                chosen = Some(ClockConfig::Graph(gc));
                format!("Imported {n} nodes.")
            }
            Err(e) => e,
        };
    }

    ui.add_space(10.0);

    // 5. The spine every MCU has, under the ids code generation reads — a
    //    starting point rather than a blank canvas.
    if ui
        .button(format!("{}  Start from a minimal tree", ph::TREE_STRUCTURE))
        .on_hover_text(
            "The universal spine — HSI/HSE, PLL, SYSCLK, AHB, APB1/APB2 — at its reset \
             settings. Generic: it computes frequencies but claims none of this chip's limits, \
             so set the oscillators from the datasheet.",
        )
        .clicked()
    {
        let graph = super::graph::minimal_graph();
        let n = graph.nodes.len();
        let layout = super::graph::auto_layout(&graph);
        chosen = Some(ClockConfig::Graph(GraphClock {
            graph,
            layout,
            // The ids ARE the canonical ones, so nothing to map.
            bindings: Default::default(),
        }));
        *note = format!(
            "Generic {n}-node tree at reset (SYSCLK on HSI). Set HSI/HSE from the datasheet — \
             the ceilings are not this chip's."
        );
    }

    if ui
        .button(format!("{}  Start an empty tree", ph::PENCIL_SIMPLE))
        .on_hover_text("Draw it yourself, node by node, in the editor")
        .clicked()
    {
        chosen = Some(ClockConfig::Graph(GraphClock {
            graph: ClockGraph {
                nodes: Vec::new(),
                edges: Vec::new(),
            },
            layout: Default::default(),
            bindings: Default::default(),
        }));
        *note = "Empty tree — add nodes from the palette.".to_owned();
        start_editing(ui);
    }

    chosen
}

/// Open the editor on the next frame — what "Start an empty tree" means, since
/// an empty diagram has nothing to look at.
fn start_editing(ui: &egui::Ui) {
    let id = egui::Id::new("graph_clock_view");
    let mut view: ClockView = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    view.edit = true;
    ui.data_mut(|d| d.insert_temp(id, view));
}

/// Import this part's exact tree: its pin-data XML names the CubeMX files.
fn import_chip_xml(path: &std::path::Path, family: &str) -> Result<(GraphClock, String), String> {
    use super::graph::cubemx;
    let xml = std::fs::read_to_string(path).map_err(|e| format!("Could not read the file: {e}"))?;
    let key = cubemx::clock_key_from_mcu_xml(&xml)?;
    let db = cubemx::default_db_dir().ok_or(
        "No STM32CubeMX installation found — use the CubeMX clock XML button and point at \
         db/plugins/clock/ yourself.",
    )?;
    let (graph, boxes) = cubemx::import_for_chip(&db, &key)?;
    Ok(bound_import(graph, boxes, family, &key.clock_tree))
}

/// Import a family's CubeMX clock file directly. Variant-conditional branches
/// stay out — nothing here names the part.
fn import_cubemx_family(
    path: &std::path::Path,
    family: &str,
) -> Result<(GraphClock, String), String> {
    use super::graph::cubemx;
    let tree = cubemx::family_of(path).ok_or("that file has no name to take a family from")?;
    let db = path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .ok_or("expected the file to sit in <CubeMX>/db/plugins/clock/")?;
    let (graph, boxes) = cubemx::import_from_db(db, &tree, &cubemx::Variant::default())?;
    Ok(bound_import(graph, boxes, family, &tree))
}

/// Finish an import: lay it out, propose the codegen bindings, and say what is
/// still unbound — an id with no node means that value falls back to a default.
fn bound_import(
    graph: super::graph::ClockGraph,
    boxes: Vec<super::graph::NodeBox>,
    family: &str,
    source: &str,
) -> (GraphClock, String) {
    let nodes = graph.nodes.len();
    // The binding itself is shared with the chip import in New Project — see
    // `cubemx::bind_graph`. Only the sentence is this module's business.
    let (gc, missing) = super::graph::cubemx::bind_graph(graph, boxes, family);
    let note = if missing.is_empty() {
        format!("Imported {nodes} nodes from {source}.")
    } else {
        format!(
            "Imported {nodes} nodes from {source} · {} codegen id(s) unbound ({}).",
            missing.len(),
            missing.join(", ")
        )
    };
    (gc, note)
}

// ── Fields view ───────────────────────────────────────────────────────────────

/// Every SELECTABLE value as a list, beside the diagram.
///
/// The same choices the diagram's dropdowns offer — both come from
/// [`options_for`](super::graph::auto_layout::options_for), so they cannot drift
/// apart — and the same node states, so everything downstream (frequencies,
/// checks, generated code) is unaffected by which one you used.
///
/// Only editable nodes appear: taps and outputs have nothing to pick, and the
/// frequencies they deliver are already in the footer. Rows follow the graph's
/// TOPOLOGICAL order — sources, then the PLL, then SYSCLK, then the buses —
/// which is the order the datasheet reads in, not alphabetical.
///
/// Returns `true` when a value changed.
/// Does a Fields row answer `query`?
///
/// A row is three things a user might remember it by — its NAME, the value
/// SELECTED in it, and the frequency it PRODUCES — and any of them should find
/// it. Someone hunting for where 258 MHz comes from types `258`; someone
/// looking for what feeds the RTC types `rtc`; someone checking which mux is
/// still on the internal RC types `msirc`.
///
/// Words are ANDed, so `pll 258` narrows instead of widening. An empty query
/// matches everything — a filter nobody has typed into must hide nothing.
fn field_matches(query: &str, name: &str, selected: &str, hz: u32) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    // The frequency has two renderings, because the tab shows both: this list
    // prints a 32 kHz output as `0.03 MHz` while the diagram tags it `32 kHz`.
    // Someone searching for the number they can see should not have to know
    // which panel they read it in.
    //
    // They are matched as ALTERNATIVES rather than being concatenated: with one
    // haystack holding both, `32 mhz` would find a 32 kHz row by taking `32`
    // from one rendering and `mhz` from the other.
    let khz = if hz.is_multiple_of(1_000) {
        format!("{} kHz", hz / 1_000)
    } else {
        format!("{:.1} kHz", hz as f64 / 1_000.0)
    };
    [fmt_mhz(hz), khz].iter().any(|freq| {
        let hay = format!("{name} {selected} {freq}").to_ascii_lowercase();
        q.split_whitespace().all(|word| hay.contains(word))
    })
}

/// What a node currently SHOWS in its Fields row — the selected option's label,
/// or empty for a row that has nothing to pick.
fn selected_label(gc: &GraphClock, id: &str) -> String {
    use super::graph::auto_layout::options_for;
    let Some(node) = gc.graph.node(id) else {
        return String::new();
    };
    options_for(&gc.graph, node)
        .and_then(|opts| {
            opts.iter()
                .find(|(_, s)| *s == node.state)
                .map(|(l, _)| l.clone())
        })
        .unwrap_or_default()
}

fn fields_panel(
    ui: &mut egui::Ui,
    gc: &mut GraphClock,
    limits: &ClockLimits,
    freqs: &std::collections::BTreeMap<String, u32>,
    query: &mut String,
) -> bool {
    use super::graph::auto_layout::{options_for, place};
    use super::graph::model::{NodeKind, NodeState};

    let mut changed = false;
    ui.horizontal(|ui| {
        ui.strong("Fields");
        ui.label(
            egui::RichText::new("every selectable value")
                .size(10.0)
                .color(egui::Color32::GRAY),
        );
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(ph::MAGNIFYING_GLASS).size(11.0));
        ui.add(
            egui::TextEdit::singleline(query)
                .hint_text("name or value")
                .desired_width(140.0),
        );
        if !query.is_empty()
            && ui
                .small_button(ph::X)
                .on_hover_text("Clear the filter")
                .clicked()
        {
            query.clear();
        }
    });
    ui.separator();

    // Reading order = the layering `place` already computes (column, then row).
    // Cheap at this size, and it keeps the list in step with the diagram.
    let order: Vec<(String, f32, f32)> = place(&gc.graph)
        .into_iter()
        .map(|b| (b.node, b.x, b.y))
        .collect();
    let rank = |id: &str| {
        order
            .iter()
            .find(|(n, _, _)| n == id)
            .map(|(_, x, y)| (*x as i32, *y as i32))
            .unwrap_or((i32::MAX, 0))
    };
    let mut ids: Vec<String> = gc
        .graph
        .nodes
        .iter()
        .filter(|n| {
            options_for(&gc.graph, n).is_some() || matches!(n.kind, NodeKind::Source { .. })
        })
        .map(|n| n.id.clone())
        .collect();
    ids.sort_by_key(|id| rank(id));
    let total = ids.len();
    ids.retain(|id| {
        field_matches(
            query,
            id,
            &selected_label(gc, id),
            freqs.get(id).copied().unwrap_or(0),
        )
    });
    let filtered = !query.trim().is_empty();

    if ids.is_empty() && !filtered {
        ui.label(
            egui::RichText::new("This tree has nothing to select.")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
        return false;
    }
    if filtered {
        ui.label(
            egui::RichText::new(format!("{} of {total} fields", ids.len()))
                .size(10.0)
                .color(egui::Color32::GRAY),
        );
    }

    let mut pending: Option<(String, NodeState)> = None;
    egui::ScrollArea::vertical()
        .id_salt("clock_fields_scroll")
        .show(ui, |ui| {
            egui::Grid::new("clock_fields_grid")
                .num_columns(3)
                .spacing([10.0, 4.0])
                .striped(true)
                .show(ui, |ui| {
                    for id in &ids {
                        let Some(node) = gc.graph.node(id) else {
                            continue;
                        };
                        ui.label(egui::RichText::new(id).size(11.0));

                        match &node.kind {
                            // A source is a frequency to TYPE, not a choice.
                            NodeKind::Source { min_hz, max_hz, .. } => {
                                let NodeState::Source { enabled, hz } = node.state.clone() else {
                                    ui.label("");
                                    ui.label("");
                                    ui.end_row();
                                    continue;
                                };
                                let (lo, hi) = (*min_hz as f64 / 1e6, *max_hz as f64 / 1e6);
                                let mut mhz = hz as f64 / 1e6;
                                let fixed = (hi - lo).abs() < f64::EPSILON;
                                let resp = ui.add_enabled(
                                    !fixed,
                                    crate::panels::drag_value(ui, &mut mhz)
                                        .range(lo..=hi.max(lo))
                                        .speed(0.1)
                                        .suffix(" MHz"),
                                );
                                if resp.changed() {
                                    pending = Some((
                                        id.clone(),
                                        NodeState::Source {
                                            enabled,
                                            hz: (mhz * 1e6).round() as u32,
                                        },
                                    ));
                                }
                            }
                            _ => {
                                let options = options_for(&gc.graph, node).unwrap_or_default();
                                let current = options
                                    .iter()
                                    .find(|(_, s)| *s == node.state)
                                    .map(|(l, _)| l.clone())
                                    .unwrap_or_else(|| "—".to_owned());
                                egui::ComboBox::from_id_salt(("clock_field", id))
                                    .selected_text(egui::RichText::new(current).size(11.0))
                                    .width(110.0)
                                    .show_ui(ui, |ui| {
                                        for (label, state) in &options {
                                            if ui
                                                .selectable_label(*state == node.state, label)
                                                .clicked()
                                            {
                                                pending = Some((id.clone(), state.clone()));
                                            }
                                        }
                                    });
                            }
                        }

                        // The frequency this node ends up producing, red past a
                        // ceiling — the same rule the diagram's tags use.
                        let hz = freqs.get(id).copied().unwrap_or(0);
                        let over = node
                            .limit
                            .and_then(|k| ceiling_for(k, limits))
                            .is_some_and(|lim| hz > lim);
                        ui.colored_label(
                            if over {
                                egui::Color32::from_rgb(230, 90, 80)
                            } else {
                                egui::Color32::from_rgb(150, 200, 160)
                            },
                            egui::RichText::new(fmt_mhz(hz)).size(11.0),
                        );
                        ui.end_row();
                    }

                    // ── The results ──────────────────────────────────────────
                    // Everything above is something you SET; these are what
                    // comes out. They used to live in a separate "Frequencies"
                    // panel — the outputs are not selectable, so `options_for`
                    // excludes them and removing that panel would have taken the
                    // only place they were listed.
                    let outs: Vec<_> = output_rows(gc, limits, freqs)
                        .into_iter()
                        .filter(|(name, hz, _)| field_matches(query, name, "", *hz))
                        .collect();
                    if !outs.is_empty() {
                        ui.end_row();
                        ui.label(
                            egui::RichText::new("Outputs")
                                .size(10.5)
                                .color(egui::Color32::GRAY),
                        );
                        ui.label("");
                        ui.label("");
                        ui.end_row();
                        for (name, hz, over) in outs {
                            ui.label(egui::RichText::new(name).size(11.0));
                            ui.label(""); // nothing to pick
                            ui.colored_label(
                                if over {
                                    egui::Color32::from_rgb(230, 90, 80)
                                } else {
                                    egui::Color32::from_rgb(150, 200, 160)
                                },
                                egui::RichText::new(fmt_mhz(hz)).size(11.0),
                            );
                            ui.end_row();
                        }
                    }
                });
        });

    if filtered && ids.is_empty() {
        ui.label(
            egui::RichText::new(format!("{}  Nothing matches that.", ph::MAGNIFYING_GLASS))
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
    }

    if let Some((id, state)) = pending
        && let Some(n) = gc.graph.node_mut(&id)
        && n.state != state
    {
        n.state = state;
        changed = true;
    }
    changed
}

// ── Pan / zoom view ───────────────────────────────────────────────────────────

const ZOOM_MIN: f32 = 0.05;
const ZOOM_MAX: f32 = 4.0;

/// Session-only diagram view: the Scene rect, whether the user has taken it over
/// (else it auto-fits), and a signature of the graph it belongs to.
#[derive(Clone)]
struct ClockView {
    scene: egui::Rect,
    adjusted: bool,
    sig: u64,
    /// Last frame's effective scale — what the toolbar shows as a percentage.
    /// Measured after the Scene ran, so it is the real one and not an estimate.
    last_scale: f32,
    /// Edit mode: build the tree instead of setting its values.
    edit: bool,
    /// Which saved positions, and which graph, the current layout was built
    /// from — so applying them (and re-deriving) happens on change, not per
    /// frame.
    pos_sig: u64,
    layout_sig: u64,
    /// The node the properties panel is showing.
    selected: Option<String>,
    /// Wiring in progress: the node whose output port was clicked.
    linking: Option<String>,
    /// Text buffers for the fields that must not be reparsed mid-typing (the id
    /// and the list-valued parameters), plus which node they belong to.
    buf_for: String,
    id_buf: String,
    param_buf: String,
}

impl Default for ClockView {
    fn default() -> Self {
        Self {
            scene: egui::Rect::NOTHING,
            adjusted: false,
            sig: 0,
            last_scale: 1.0,
            edit: false,
            pos_sig: 0,
            layout_sig: 0,
            selected: None,
            linking: None,
            buf_for: String::new(),
            id_buf: String::new(),
            param_buf: String::new(),
        }
    }
}

// ── Properties panel ──────────────────────────────────────────────────────────

/// The edit-mode side panel: identity and parameters of the selected node, its
/// incoming wires, and the graph's structural problems. Returns `true` when the
/// GRAPH changed (so the caller regenerates code) — moving a box does not come
/// through here.
fn properties_panel(
    ui: &mut egui::Ui,
    gc: &mut GraphClock,
    view: &mut ClockView,
    positions: &mut ClockPositions,
    family: &str,
    note: &mut String,
) -> bool {
    use super::graph::model::{LimitKey, NodeKind};
    let mut changed = false;

    ui.strong("Node");
    ui.separator();

    let Some(id) = view.selected.clone() else {
        ui.label(
            egui::RichText::new("Select a node in the diagram to edit it.")
                .size(11.0)
                .color(egui::Color32::GRAY),
        );
        ui.add_space(10.0);
        let bound = binding_table(ui, gc, family);
        ui.add_space(10.0);
        issue_list(ui, gc, family);
        return bound;
    };
    if gc.graph.node(&id).is_none() {
        view.selected = None;
        return false;
    }

    // Refill the text buffers when the subject changes, so typing isn't
    // reformatted under the cursor.
    if view.buf_for != id {
        view.buf_for = id.clone();
        view.id_buf = id.clone();
        view.param_buf = param_text(gc.graph.node(&id).unwrap());
    }

    // ── Identity ─────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label("id");
        ui.add(egui::TextEdit::singleline(&mut view.id_buf).desired_width(120.0));
        if ui.small_button("Rename").clicked() {
            let mut boxes = std::mem::take(&mut gc.layout.nodes);
            match edit::rename_node(&mut gc.graph, &mut boxes, &id, &view.id_buf.clone()) {
                Ok(()) => {
                    let new = view.id_buf.trim().to_owned();
                    if let Some((x, y)) = positions.remove(&id) {
                        positions.insert(new.clone(), (x, y));
                    }
                    view.selected = Some(new.clone());
                    view.buf_for = new;
                    (*note).clear();
                    changed = true;
                }
                Err(e) => (*note) = e,
            }
            gc.layout = super::graph::derive(&gc.graph, boxes);
        }
    });
    if codegen_ids(family).contains(&id.as_str()) {
        ui.label(
            egui::RichText::new(format!(
                "{}  Read by code generation — renaming or deleting it changes main.rs.",
                ph::WARNING
            ))
            .size(10.0)
            .color(egui::Color32::from_rgb(225, 185, 60)),
        );
    }

    // ── Kind parameters ──────────────────────────────────────────────────────
    ui.add_space(6.0);
    let node = gc.graph.node_mut(&id).unwrap();
    ui.label(egui::RichText::new(kind_name(&node.kind)).strong());
    let mut params_edited = false;
    match &mut node.kind {
        NodeKind::Source {
            min_hz,
            max_hz,
            gated,
        } => {
            let mut lo = *min_hz as f64 / 1e6;
            let mut hi = *max_hz as f64 / 1e6;
            ui.horizontal(|ui| {
                ui.label("min");
                params_edited |= ui
                    .add(
                        crate::panels::drag_value(ui, &mut lo)
                            .speed(0.1)
                            .suffix(" MHz"),
                    )
                    .changed();
                ui.label("max");
                params_edited |= ui
                    .add(
                        crate::panels::drag_value(ui, &mut hi)
                            .speed(0.1)
                            .suffix(" MHz"),
                    )
                    .changed();
            });
            params_edited |= ui.checkbox(gated, "can be switched off").changed();
            *min_hz = (lo * 1e6).max(0.0) as u32;
            *max_hz = (hi * 1e6).max(0.0) as u32;
        }
        NodeKind::Mux { inputs } => {
            ui.horizontal(|ui| {
                ui.label("inputs");
                params_edited |= ui
                    .add(crate::panels::drag_value(ui, inputs).range(1..=12))
                    .changed();
            });
        }
        NodeKind::FixedDiv { by } => {
            ui.horizontal(|ui| {
                ui.label("divide by");
                params_edited |= ui
                    .add(crate::panels::drag_value(ui, by).range(1..=4096))
                    .changed();
            });
        }
        NodeKind::Multiplier { min, max } => {
            ui.horizontal(|ui| {
                ui.label("×");
                params_edited |= ui
                    .add(crate::panels::drag_value(ui, min).range(1..=1024))
                    .changed();
                ui.label("…");
                params_edited |= ui
                    .add(crate::panels::drag_value(ui, max).range(1..=1024))
                    .changed();
            });
        }
        NodeKind::Divider { .. } | NodeKind::Choice { .. } => {
            let hint = match &node.kind {
                NodeKind::Divider { .. } => "divisors, comma separated (1, 2, 4, 8)",
                _ => "ratios n/d, comma separated (1/1, 2/3)",
            };
            ui.label(
                egui::RichText::new(hint)
                    .size(10.0)
                    .color(egui::Color32::GRAY),
            );
            if ui
                .add(egui::TextEdit::singleline(&mut view.param_buf).desired_width(f32::INFINITY))
                .lost_focus()
            {
                params_edited = apply_param_text(node, &view.param_buf);
                view.param_buf = param_text(node);
            }
        }
        NodeKind::TimerMul { prescaler } => {
            // Which prescaler it follows is a reference to another node, so the
            // picker needs the whole graph — it is drawn just below, outside
            // this borrow. Here we only show the current value.
            let current = prescaler.clone();
            ui.horizontal(|ui| {
                ui.label("follows");
                ui.label(
                    egui::RichText::new(if current.is_empty() { "—" } else { &current })
                        .color(egui::Color32::from_rgb(150, 200, 160)),
                );
            });
        }
        NodeKind::Gate | NodeKind::Tap | NodeKind::Output => {
            ui.label(
                egui::RichText::new("No parameters.")
                    .size(10.0)
                    .color(egui::Color32::GRAY),
            );
        }
    }
    if params_edited {
        edit::clamp_state(node);
        changed = true;
    }

    // The timer rule's prescaler is a reference to another node, so it needs the
    // id list — picked here, outside the borrow above.
    if matches!(
        gc.graph.node(&id).map(|n| &n.kind),
        Some(NodeKind::TimerMul { .. })
    ) {
        let ids: Vec<String> = gc.graph.nodes.iter().map(|n| n.id.clone()).collect();
        let mut pick: Option<String> = None;
        egui::ComboBox::from_id_salt("clock_timer_presc")
            .selected_text("choose prescaler")
            .show_ui(ui, |ui| {
                for other in ids.iter().filter(|o| **o != id) {
                    if ui.selectable_label(false, other).clicked() {
                        pick = Some(other.clone());
                    }
                }
            });
        if let Some(p) = pick
            && let Some(n) = gc.graph.node_mut(&id)
        {
            n.kind = NodeKind::TimerMul { prescaler: p };
            changed = true;
        }
    }

    // ── Datasheet ceiling ────────────────────────────────────────────────────
    ui.add_space(6.0);
    let node = gc.graph.node_mut(&id).unwrap();
    let mut mhz = match node.limit {
        Some(LimitKey::Hz(hz)) => hz as f64 / 1e6,
        _ => 0.0,
    };
    let named = !matches!(node.limit, Some(LimitKey::Hz(_)) | None);
    ui.horizontal(|ui| {
        ui.label("limit");
        if named {
            ui.label(
                egui::RichText::new(format!("{:?} (from the chip)", node.limit.unwrap()))
                    .size(10.0)
                    .color(egui::Color32::GRAY),
            );
        } else if ui
            .add(
                crate::panels::drag_value(ui, &mut mhz)
                    .speed(1.0)
                    .suffix(" MHz"),
            )
            .changed()
        {
            node.limit = if mhz > 0.0 {
                Some(LimitKey::Hz((mhz * 1e6) as u32))
            } else {
                None
            };
            changed = true;
        }
    });

    // ── Incoming wires ───────────────────────────────────────────────────────
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Inputs").strong());
    let incoming: Vec<(String, usize)> = gc
        .graph
        .edges
        .iter()
        .filter(|e| e.to == id)
        .map(|e| (e.from.clone(), e.input))
        .collect();
    if incoming.is_empty() {
        ui.label(
            egui::RichText::new("nothing connected")
                .size(10.0)
                .color(egui::Color32::GRAY),
        );
    }
    for (from, input) in incoming {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(format!("{input}: {from}")).size(11.0));
            if ui.small_button(ph::X).on_hover_text("Disconnect").clicked() {
                edit::disconnect(&mut gc.graph, &from, &id, input);
                let boxes = std::mem::take(&mut gc.layout.nodes);
                gc.layout = super::graph::derive(&gc.graph, boxes);
                changed = true;
            }
        });
    }

    ui.add_space(10.0);
    if binding_table(ui, gc, family) {
        changed = true;
    }
    ui.add_space(10.0);
    issue_list(ui, gc, family);
    changed
}

/// The node ids this chip's code generation reads — marked in the panel and
/// checked for existence, since they are addressed by name.
fn codegen_ids(family: &str) -> Vec<&'static str> {
    crate::panels::mcu_module::codegen::rcc::codegen_node_ids(family)
}

/// The binding table: which node answers for each id code generation reads.
///
/// This is what makes an IMPORTED tree generate real code. The tree keeps the
/// vendor's names (`SysClkSource`, `AHBPrescaler`) — the ones printed in the
/// datasheet figure — and the mapping to `sw` / `ahb` lives here, proposed
/// automatically at import and correctable by hand. An id left unbound is an
/// error in Checks, because it means that value quietly becomes a default.
///
/// Returns `true` when a binding changed (the caller regenerates code).
fn binding_table(ui: &mut egui::Ui, gc: &mut GraphClock, family: &str) -> bool {
    let ids = codegen_ids(family);
    if ids.is_empty() {
        return false;
    }
    let mut changed = false;

    ui.label(egui::RichText::new("Code generation").strong());
    ui.label(
        egui::RichText::new("which node answers for each id main.rs is generated from")
            .size(10.0)
            .color(egui::Color32::GRAY),
    );

    // Only nodes that can carry a value are offered; a label would be noise.
    let node_ids: Vec<String> = gc.graph.nodes.iter().map(|n| n.id.clone()).collect();

    egui::ScrollArea::vertical()
        .id_salt("clock_binding_scroll")
        .max_height(200.0)
        .show(ui, |ui| {
            egui::Grid::new("clock_bindings")
                .num_columns(2)
                .spacing([8.0, 3.0])
                .striped(true)
                .show(ui, |ui| {
                    for id in &ids {
                        // A graph that uses the canonical name needs no entry.
                        let bound = gc.bindings.get(*id).cloned();
                        let direct = node_ids.iter().any(|n| n == id);
                        let current = bound.clone().unwrap_or_else(|| {
                            if direct {
                                (*id).to_string()
                            } else {
                                "—".to_string()
                            }
                        });
                        let ok = bound.is_some() || direct;
                        ui.label(egui::RichText::new(*id).size(11.0).color(if ok {
                            egui::Color32::from_rgb(150, 200, 160)
                        } else {
                            egui::Color32::from_rgb(230, 90, 80)
                        }));
                        let mut pick: Option<Option<String>> = None;
                        egui::ComboBox::from_id_salt(("clock_bind", *id))
                            .selected_text(egui::RichText::new(current).size(11.0))
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                if ui.selectable_label(bound.is_none(), "— none —").clicked() {
                                    pick = Some(None);
                                }
                                for n in &node_ids {
                                    if ui
                                        .selectable_label(bound.as_deref() == Some(n), n)
                                        .clicked()
                                    {
                                        pick = Some(Some(n.clone()));
                                    }
                                }
                            });
                        if let Some(p) = pick {
                            match p {
                                Some(node) => gc.bindings.insert((*id).to_string(), node),
                                None => gc.bindings.remove(*id),
                            };
                            changed = true;
                        }
                        ui.end_row();
                    }
                });
        });

    if ui
        .small_button("Propose from names")
        .on_hover_text("Match the ids against the node names again, replacing the bindings")
        .clicked()
    {
        gc.bindings = super::graph::bind::propose(&ids, &gc.graph);
        changed = true;
    }
    changed
}

/// Structural problems, worst first — the editor's own validation, separate
/// from the datasheet-ceiling checks in the footer.
fn issue_list(ui: &mut egui::Ui, gc: &GraphClock, family: &str) {
    let mut found = edit::issues(&gc.graph);
    // Family-aware check the pure validator cannot make: an id the code
    // generator looks up is neither a node nor bound to one, so that value
    // silently falls back to a default in `main.rs`.
    for id in codegen_ids(family) {
        let resolved = gc.bindings.get(id).map_or(id, |n| n.as_str());
        if !gc.graph.nodes.iter().any(|n| n.id == resolved) {
            found.insert(
                0,
                edit::Issue {
                    node: None,
                    msg: format!(
                        "`{id}` is unbound — code generation reads it, so main.rs will fall back \
                         to a default."
                    ),
                    severity: Severity::Error,
                },
            );
        }
    }
    ui.label(egui::RichText::new("Checks").strong());
    if found.is_empty() {
        ui.colored_label(
            egui::Color32::from_rgb(90, 200, 110),
            format!("{}  The tree is well-formed.", ph::CHECK_CIRCLE),
        );
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("clock_issue_scroll")
        .max_height(180.0)
        .show(ui, |ui| {
            for i in found {
                let (color, icon) = match i.severity {
                    Severity::Error => (egui::Color32::from_rgb(230, 90, 80), ph::X_CIRCLE),
                    Severity::Warning => (egui::Color32::from_rgb(225, 185, 60), ph::WARNING),
                };
                let where_ = i.node.map(|n| format!("{n}: ")).unwrap_or_default();
                ui.colored_label(color, format!("{icon}  {where_}{}", i.msg));
            }
        });
}

fn kind_name(kind: &super::graph::model::NodeKind) -> &'static str {
    use super::graph::model::NodeKind as K;
    match kind {
        K::Source { .. } => "Oscillator / source",
        K::Mux { .. } => "Mux (selector)",
        K::Divider { .. } => "Divider",
        K::FixedDiv { .. } => "Fixed divider",
        K::Choice { .. } => "Ratio choice",
        K::Multiplier { .. } => "Multiplier",
        K::Gate => "Enable gate",
        K::TimerMul { .. } => "Timer ×1/×2 rule",
        K::Tap => "Tap",
        K::Output => "Output",
    }
}

/// The list-valued parameter of a node as editable text.
fn param_text(node: &super::graph::model::Node) -> String {
    use super::graph::model::NodeKind as K;
    match &node.kind {
        K::Divider { options } => options
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        K::Choice { ratios } => ratios
            .iter()
            .map(|(n, d)| format!("{n}/{d}"))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Parse an edited parameter list back into the node. Unparseable entries are
/// dropped; an empty result is refused, so a typo can't erase the node's options.
fn apply_param_text(node: &mut super::graph::model::Node, text: &str) -> bool {
    use super::graph::model::NodeKind as K;
    match &mut node.kind {
        K::Divider { options } => {
            let parsed: Vec<u32> = text
                .split(',')
                .filter_map(|t| t.trim().parse::<u32>().ok())
                .filter(|v| *v > 0)
                .collect();
            if parsed.is_empty() || parsed == *options {
                return false;
            }
            *options = parsed;
            true
        }
        K::Choice { ratios } => {
            let parsed: Vec<(u32, u32)> = text
                .split(',')
                .filter_map(|t| {
                    let (n, d) = t.trim().split_once('/')?;
                    Some((n.trim().parse().ok()?, d.trim().parse::<u32>().ok()?))
                })
                .filter(|(_, d)| *d > 0)
                .collect();
            if parsed.is_empty() || parsed == *ratios {
                return false;
            }
            *ratios = parsed;
            true
        }
        _ => false,
    }
}

/// Add a node, drawing it in whichever kind of layout this is.
///
/// A DERIVED layout is regenerated from its boxes. A HAND-AUTHORED one is not —
/// it keeps its figure, and the new node's primitives are APPENDED: exactly what
/// `derive` would have produced for that one node. So a hand-drawn diagram can
/// grow without being redrawn.
fn add_node_to(gc: &mut GraphClock, kind: edit::PaletteKind, x: f32, y: f32) -> String {
    let derived = !gc.layout.nodes.is_empty();
    let mut boxes = std::mem::take(&mut gc.layout.nodes);
    let id = edit::add_node(&mut gc.graph, &mut boxes, kind, x, y, 96.0, 26.0);
    if derived {
        gc.layout = super::graph::derive(&gc.graph, boxes);
    } else {
        // Only the newcomer is drawn; everything already on the figure is left
        // exactly as its author placed it.
        let one = super::graph::derive(&gc.graph, vec![boxes.pop().expect("the new box")]);
        gc.layout.blocks.extend(one.blocks);
        gc.layout.tags.extend(one.tags);
        gc.layout.labels_above.extend(one.labels_above);
        gc.layout.widgets.extend(one.widgets);
    }
    id
}

/// Delete a node and whatever draws it, in either kind of layout.
fn remove_node_from(gc: &mut GraphClock, id: &str) {
    let derived = !gc.layout.nodes.is_empty();
    let mut boxes = std::mem::take(&mut gc.layout.nodes);
    edit::remove_node(&mut gc.graph, &mut boxes, id);
    if derived {
        gc.layout = super::graph::derive(&gc.graph, boxes);
    } else {
        gc.layout.remove_node_primitives(id);
    }
}

/// Overwrite the layout's node positions with the saved ones and rebuild the
/// drawable primitives. Ids the graph doesn't have are ignored, so a layout
/// saved for another chip degrades to "no overrides" instead of corrupting.
fn apply_positions(gc: &mut GraphClock, positions: &ClockPositions) {
    let mut boxes = std::mem::take(&mut gc.layout.nodes);
    for b in &mut boxes {
        if let Some(&(x, y)) = positions.get(&b.node) {
            b.x = x;
            b.y = y;
        }
    }
    gc.layout = super::graph::derive(&gc.graph, boxes);
}

/// The positions worth saving: only nodes the user MOVED away from where the
/// automatic layout puts them. Keeping the rest implicit means a later
/// `auto_layout` improvement still reaches every untouched node, and a project
/// nobody dragged in writes no `@clock_layout` section at all.
fn moved_positions(gc: &GraphClock) -> ClockPositions {
    let base = super::graph::place(&gc.graph);
    gc.layout
        .nodes
        .iter()
        .filter(|b| {
            base.iter()
                .find(|p| p.node == b.node)
                .is_none_or(|p| (p.x - b.x).abs() > 0.5 || (p.y - b.y).abs() > 0.5)
        })
        .map(|b| (b.node.clone(), (b.x, b.y)))
        .collect()
}

/// Identity of a saved-position set, so the layout is re-derived on a change
/// rather than every frame.
fn positions_signature(p: &ClockPositions) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.len().hash(&mut h);
    for (id, (x, y)) in p {
        id.hash(&mut h);
        x.to_bits().hash(&mut h);
        y.to_bits().hash(&mut h);
    }
    h.finish()
}

/// Cheap identity of a graph's SHAPE (ids + wiring, not the selected states):
/// changes exactly when the diagram needs re-fitting, and stays stable while the
/// user is only picking dividers and muxes.
fn graph_signature(g: &super::graph::ClockGraph) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    g.nodes.len().hash(&mut h);
    for n in &g.nodes {
        n.id.hash(&mut h);
    }
    g.edges.len().hash(&mut h);
    for e in &g.edges {
        e.from.hash(&mut h);
        e.to.hash(&mut h);
        e.input.hash(&mut h);
    }
    h.finish()
}

/// The Scene's effective scale — how `scene` maps onto the viewport `outer`.
/// This is what the toolbar shows as a percentage.
fn scale_of(scene: egui::Rect, outer: egui::Rect) -> f32 {
    if !scene.is_finite() || scene.size() == egui::Vec2::ZERO {
        return 1.0;
    }
    (outer.size() / scene.size())
        .min_elem()
        .clamp(ZOOM_MIN, ZOOM_MAX)
}

/// Scale the view rect about its centre — `f < 1` zooms IN (smaller rect shows
/// less of the scene). Bounded relative to the viewport so repeated presses
/// can't push the diagram out of reach.
fn zoom_by(view: &mut ClockView, f: f32, avail: egui::Vec2) {
    let base = view.scene;
    if !base.is_finite() || base.size() == egui::Vec2::ZERO || avail.x <= 0.0 || avail.y <= 0.0 {
        return;
    }
    let (min, max) = (avail * 0.2, avail * 5.0);
    let s = base.size() * f;
    let s = egui::vec2(s.x.clamp(min.x, max.x), s.y.clamp(min.y, max.y));
    view.scene = egui::Rect::from_center_size(base.center(), s);
    view.adjusted = true;
}

// ── Footer zones ──────────────────────────────────────────────────────────────

/// Delivered-clock table: prefers the layout's labelled output boxes; falls
/// back to every Output / limit-bearing graph node.
/// The clock OUTPUTS — what the tree produces, as `(name, hz, over limit)`.
///
/// Extracted from the old Frequencies panel so the Fields list can end with it.
/// A hand-drawn figure names its outputs in the layout; a derived one has none,
/// so the graph's `Output` nodes (and anything carrying a ceiling) stand in.
fn output_rows(
    gc: &GraphClock,
    limits: &ClockLimits,
    freqs: &std::collections::BTreeMap<String, u32>,
) -> Vec<(String, u32, bool)> {
    use super::graph::model::NodeKind;
    let mut out = Vec::new();
    if !gc.layout.outputs.is_empty() {
        for o in &gc.layout.outputs {
            let hz = value_from_graph(&o.src, freqs);
            let over = o
                .limit
                .and_then(|k| ceiling_for(k, limits))
                .is_some_and(|l| hz > l);
            out.push((o.label.clone(), hz, over));
        }
        return out;
    }
    for node in &gc.graph.nodes {
        if !matches!(node.kind, NodeKind::Output) {
            continue;
        }
        let hz = freqs.get(&node.id).copied().unwrap_or(0);
        let over = node
            .limit
            .and_then(|k| ceiling_for(k, limits))
            .is_some_and(|l| hz > l);
        out.push((node.id.clone(), hz, over));
    }
    out
}

/// Validation + legend. STM32F1-family graphs get the FULL typed validation
/// (datasheet footnotes — USB = 48 MHz, HSI→PLL cap, ADC-1 µs APB2 set) via the
/// `graph_to_stm32f1` bridge; other families get the generic ceiling check.
fn graph_info_zone(
    ui: &mut egui::Ui,
    gc: &GraphClock,
    l: &ClockLimits,
    freqs: &std::collections::BTreeMap<String, u32>,
    is_stm32f1: bool,
    family: &str,
    manual: bool,
) -> bool {
    // The checkbox itself moved to the toolbar; what stays here is what it
    // MEANS — the amber warnings that a diagram driving nothing would hide.
    clock_manual_note(ui, family, gc, manual);
    let changed = false;
    if is_stm32f1 {
        let c = graph_to_stm32f1(&gc.for_codegen());
        let ws = warnings(&c, &frequencies(&c), l);
        if ws.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(90, 200, 110),
                format!("{}  Configuration is valid.", ph::CHECK_CIRCLE),
            );
        } else {
            for w in ws {
                let (color, icon) = match w.severity {
                    Severity::Error => (egui::Color32::from_rgb(230, 90, 80), ph::X_CIRCLE),
                    Severity::Warning => (egui::Color32::from_rgb(225, 185, 60), ph::WARNING),
                };
                ui.colored_label(color, format!("{icon}  {}", w.msg));
            }
        }
    } else {
        let issues = over_limits(&gc.graph, l, freqs);
        if issues.is_empty() {
            ui.colored_label(
                egui::Color32::from_rgb(90, 200, 110),
                format!("{}  All clocks within datasheet limits.", ph::CHECK_CIRCLE),
            );
        } else {
            for o in issues {
                ui.colored_label(
                    egui::Color32::from_rgb(230, 90, 80),
                    format!(
                        "{}  {} = {} exceeds {}",
                        ph::X_CIRCLE,
                        o.node,
                        fmt_mhz(o.hz),
                        fmt_mhz(o.limit)
                    ),
                );
            }
        }
    }

    ui.add_space(6.0);
    ui.separator();
    let dim = egui::Color32::from_rgb(150, 158, 172);
    let mut lines = vec![format!(
        "Limits: SYSCLK {} · HCLK {} · PCLK1 {} · PCLK2 {} · ADC {} · USB {} MHz",
        mhz_num(l.sysclk_max),
        mhz_num(l.hclk_max),
        mhz_num(l.pclk1_max),
        mhz_num(l.pclk2_max),
        mhz_num(l.adcclk_max),
        mhz_num(l.usbclk_hz),
    )];
    if is_stm32f1 {
        lines.insert(
            0,
            "HSE = high-speed external · HSI = high-speed internal".to_owned(),
        );
        lines.insert(
            1,
            "LSE = low-speed external · LSI = low-speed internal".to_owned(),
        );
        lines.push(format!(
            "USB needs HSE+PLL with USBCLK = {} MHz · ADC 1 µs needs PCLK2 in {{14,28,56}}",
            mhz_num(l.usbclk_hz)
        ));
    }
    for line in lines {
        ui.label(egui::RichText::new(line).size(11.0).color(dim));
    }
    changed
}

/// Whether this chip's clock reaches `main.rs` at all — the per-CHIP answer.
///
/// Not per family: a family with no RCC recipe still generates from a tree that
/// carries the canonical spine, and saying otherwise would send the user
/// hand-writing a block the IDE was about to write for them.
///
/// Returns `(generated at all, generated from the TREE rather than a verified
/// family recipe)`.
fn clock_codegen_state(family: &str, gc: &GraphClock) -> (bool, bool) {
    use crate::panels::mcu_module::codegen::rcc::{generates_clock_code, generates_clock_code_for};
    let clock = ClockConfig::Graph(gc.clone());
    let generated = generates_clock_code_for(family, &clock);
    (generated, generated && !generates_clock_code(family))
}

/// The hand-written-clock checkbox, in the toolbar.
///
/// F1 and ESP generate their clock through their own HALs, which are not
/// marker-wrapped — the switch would promise a preservation that never happens,
/// so it is not offered there and this draws nothing.
fn clock_manual_checkbox(ui: &mut egui::Ui, family: &str, manual: &mut bool) -> bool {
    use crate::panels::mcu_module::codegen::rcc::supports_manual_clock;
    if !supports_manual_clock(family) {
        return false;
    }
    ui.checkbox(manual, "Write the clock by hand")
        .on_hover_text(
            "Fence the clock block off in main.rs and keep your edits across regeneration. Turning it back off lets the Clock tab drive it again - and discards what you wrote.",
        )
        .changed()
}

/// What that checkbox MEANS, in the Info panel.
///
/// Kept beside the validation rather than beside the checkbox, because this is
/// the part a user needs while reading the tree: a diagram that has quietly
/// stopped driving the code would otherwise look exactly like one that drives it.
fn clock_manual_note(ui: &mut egui::Ui, family: &str, gc: &GraphClock, manual: bool) {
    use crate::panels::mcu_module::codegen::rcc::supports_manual_clock;
    if !supports_manual_clock(family) {
        return;
    }
    // THE thing a reader has to know, and the reason this note exists at all:
    // while it is on, the tree below is not what ends up in `main.rs`.
    if manual {
        ui.colored_label(
            egui::Color32::from_rgb(225, 185, 60),
            format!(
                "{}  main.rs keeps its own clock block - the tree shows frequencies but does not generate them.",
                ph::WARNING
            ),
        );
    }
    let (generated, from_tree) = clock_codegen_state(family, gc);
    if !generated {
        ui.label(
            egui::RichText::new(format!(
                "{}  {family} has no clock generator - the System tab lists what this chip is missing",
                ph::WARNING
            ))
            .size(11.0)
            .color(egui::Color32::from_rgb(225, 185, 60)),
        );
    } else if from_tree {
        ui.label(
            egui::RichText::new(format!(
                "{}  generated from this tree with embassy's common RCC shape - check the field names in main.rs",
                ph::INFO
            ))
            .size(11.0)
            .color(egui::Color32::from_rgb(150, 180, 220)),
        );
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

/// Format Hz as a trimmed MHz string, e.g. 36_000_000 → "36 MHz", 12_500_000 → "12.5 MHz".
fn fmt_mhz(hz: u32) -> String {
    let mhz = hz as f64 / 1_000_000.0;
    if (mhz.fract()).abs() < 1e-6 {
        format!("{} MHz", mhz as u32)
    } else {
        format!("{mhz:.2} MHz")
    }
}

/// Bare MHz number for the compact legend lines, e.g. 72_000_000 → "72".
fn mhz_num(hz: u32) -> String {
    let mhz = hz as f64 / 1_000_000.0;
    if (mhz.fract()).abs() < 1e-6 {
        format!("{}", mhz as u32)
    } else {
        format!("{mhz:.1}")
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests — the pure half of edit mode (the drag itself needs a live egui pass)
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    /// Three ways to remember a field, all of which must find it.
    #[test]
    fn a_field_is_found_by_name_value_or_frequency() {
        use super::field_matches;

        // A mux called `SysClkSource`, currently on `MSIRC`, producing 4 MHz.
        let row = |q: &str| field_matches(q, "SysClkSource", "MSIRC", 4_000_000);

        assert!(row(""), "an empty filter hides nothing");
        assert!(row("sysclk"), "by name");
        assert!(row("msirc"), "by the value selected in it");
        assert!(row("4 mhz"), "by the frequency it produces");
        assert!(row("SYSCLK"), "case does not matter");
        assert!(!row("pll"), "and a miss is a miss");

        // Words are ANDed, so a second word narrows.
        assert!(row("sysclk msirc"));
        assert!(!row("sysclk hsi"));

        // An output row has no selection, and is still findable both ways.
        assert!(field_matches("iwdg", "IWDGOutput", "", 32_000));
        // A sub-MHz value is printed `0.03 MHz` in this list and `32 kHz` on
        // the diagram; both spellings have to find it.
        assert!(field_matches("32 khz", "IWDGOutput", "", 32_000));
        assert!(field_matches("0.03 mhz", "IWDGOutput", "", 32_000));
        assert!(!field_matches("32 mhz", "IWDGOutput", "", 32_000));
    }

    use super::*;
    use crate::panels::mcu_module::clock::graph::{
        ClockGraph, Edge, Node, NodeKind, NodeState, auto_layout,
    };

    fn sample() -> GraphClock {
        let graph = ClockGraph {
            nodes: vec![
                Node {
                    id: "hsi".into(),
                    kind: NodeKind::Source {
                        min_hz: 16_000_000,
                        max_hz: 16_000_000,
                        gated: false,
                    },
                    state: NodeState::Source {
                        enabled: true,
                        hz: 16_000_000,
                    },
                    limit: None,
                },
                Node {
                    id: "ahb".into(),
                    kind: NodeKind::Divider {
                        options: vec![1, 2, 4],
                    },
                    state: NodeState::Index(0),
                    limit: None,
                },
                Node {
                    id: "hclk".into(),
                    kind: NodeKind::Output,
                    state: NodeState::Fixed,
                    limit: None,
                },
            ],
            edges: vec![
                Edge {
                    from: "hsi".into(),
                    to: "ahb".into(),
                    input: 0,
                },
                Edge {
                    from: "ahb".into(),
                    to: "hclk".into(),
                    input: 0,
                },
            ],
        };
        let layout = auto_layout(&graph);
        GraphClock {
            graph,
            layout,
            bindings: Default::default(),
        }
    }

    fn box_of(gc: &GraphClock, id: &str) -> (f32, f32) {
        let b = gc.layout.nodes.iter().find(|b| b.node == id).unwrap();
        (b.x, b.y)
    }

    /// An untouched diagram records nothing — so a project nobody dragged in
    /// never grows a `@clock_layout` section, and a better `auto_layout` still
    /// reaches it later.
    #[test]
    fn an_untouched_layout_saves_no_positions() {
        assert!(moved_positions(&sample()).is_empty());
    }

    /// Only the node the user actually moved is recorded.
    #[test]
    fn only_moved_nodes_are_recorded() {
        let mut gc = sample();
        let b = gc
            .layout
            .nodes
            .iter_mut()
            .find(|b| b.node == "ahb")
            .unwrap();
        b.x += 60.0;
        b.y -= 12.0;
        let moved = moved_positions(&gc);
        assert_eq!(moved.len(), 1);
        assert_eq!(moved.get("ahb"), Some(&box_of(&gc, "ahb")));
    }

    /// Saved positions survive a reload: applying them to a fresh automatic
    /// layout puts the box back AND moves its label with it.
    #[test]
    fn saved_positions_are_reapplied_to_a_fresh_layout() {
        let mut edited = sample();
        {
            let b = edited
                .layout
                .nodes
                .iter_mut()
                .find(|b| b.node == "ahb")
                .unwrap();
            b.x = 500.0;
            b.y = 300.0;
        }
        let saved = moved_positions(&edited);

        // A fresh session: the layout is generated from scratch, then the saved
        // positions land on top of it.
        let mut reopened = sample();
        apply_positions(&mut reopened, &saved);
        assert_eq!(box_of(&reopened, "ahb"), (500.0, 300.0));
        assert_eq!(
            box_of(&reopened, "hsi"),
            box_of(&sample(), "hsi"),
            "untouched nodes keep the automatic position"
        );
        let label = reopened
            .layout
            .labels_above
            .iter()
            .find(|l| l.text == "ahb")
            .unwrap();
        assert_eq!(label.x, 500.0, "the derived primitives followed the box");
    }

    /// A layout saved for a different chip must not corrupt this one.
    #[test]
    fn positions_for_unknown_nodes_are_ignored() {
        let mut gc = sample();
        let before = gc.layout.nodes.clone();
        let mut alien = ClockPositions::new();
        alien.insert("some_other_chips_node".into(), (900.0, 900.0));
        apply_positions(&mut gc, &alien);
        assert_eq!(gc.layout.nodes, before);
    }

    /// The properties panel edits divisor / ratio lists as text; the round trip
    /// must be lossless and a typo must not wipe the node's options.
    #[test]
    fn parameter_lists_round_trip_through_their_text_field() {
        use crate::panels::mcu_module::clock::graph::{Node, NodeKind, NodeState};

        let mut div = Node {
            id: "ahb".into(),
            kind: NodeKind::Divider {
                options: vec![1, 2, 4, 8],
            },
            state: NodeState::Index(0),
            limit: None,
        };
        assert_eq!(param_text(&div), "1, 2, 4, 8");
        assert!(apply_param_text(&mut div, "1, 2, 4, 8, 16"));
        assert_eq!(param_text(&div), "1, 2, 4, 8, 16");
        assert!(
            !apply_param_text(&mut div, "1, 2, 4, 8, 16"),
            "an unchanged list is not an edit"
        );
        assert!(
            !apply_param_text(&mut div, "oops"),
            "an unparseable list is refused"
        );
        assert_eq!(param_text(&div), "1, 2, 4, 8, 16", "options survived");
        assert!(!apply_param_text(&mut div, "0, 0"), "zero divisors dropped");

        let mut choice = Node {
            id: "usb".into(),
            kind: NodeKind::Choice {
                ratios: vec![(1, 1), (2, 3)],
            },
            state: NodeState::Index(0),
            limit: None,
        };
        assert_eq!(param_text(&choice), "1/1, 2/3");
        assert!(apply_param_text(&mut choice, "1/1, 2/3, 1/2"));
        assert_eq!(param_text(&choice), "1/1, 2/3, 1/2");
        assert!(
            !apply_param_text(&mut choice, "3/0"),
            "a zero denominator is dropped, leaving nothing to apply"
        );
    }

    /// The signature changes when a position does — that is what triggers the
    /// re-derive instead of doing it every frame.
    #[test]
    fn the_position_signature_tracks_changes() {
        let mut p = ClockPositions::new();
        let empty = positions_signature(&p);
        p.insert("ahb".into(), (10.0, 20.0));
        let one = positions_signature(&p);
        assert_ne!(empty, one);
        p.insert("ahb".into(), (10.0, 21.0));
        assert_ne!(one, positions_signature(&p), "a moved node must register");
    }

    /// The Clock tab in edit mode, frame by frame.
    struct EditTab {
        ctx: egui::Context,
        gc: GraphClock,
        state: ClockUiState,
        pass: u64,
    }

    impl EditTab {
        fn new() -> Self {
            let mut tab = Self {
                ctx: egui::Context::default(),
                gc: sample(),
                state: ClockUiState::default(),
                pass: 0,
            };
            // The Scene is fitted to the diagram a frame late.
            for _ in 0..4 {
                tab.step(vec![]);
            }
            tab
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
            let (gc, state, first) = (&mut self.gc, &mut self.state, self.pass == 1);
            let _ = crate::headless::run_ui(&self.ctx, input, |ui| {
                if first {
                    start_editing(ui);
                }
                let mut manual = false;
                let limits = ClockLimits::default();
                draw_graph_clock(ui, gc, &limits, &[], None, "stm32g0", state, &mut manual);
            });
        }

        /// Node `id`'s plate on the screen, and the canvas zoom.
        fn node(&self, id: &str) -> (egui::Rect, f32) {
            let w = self
                .ctx
                .viewport_for(egui::ViewportId::ROOT, |vp| {
                    let id = egui::Id::new(("clock_node_drag", id));
                    vp.prev_pass.widgets.get(id).copied()
                })
                .expect("the editor drew the node");
            let to_screen = self
                .ctx
                .layer_transform_to_global(w.layer_id)
                .unwrap_or_default();
            (to_screen * w.interact_rect, to_screen.scaling)
        }

        /// Hover `at`, press there, move the pointer by `by` in `steps`
        /// frames, release where it ended, and let a frame pass.
        fn gesture(&mut self, at: egui::Pos2, by: egui::Vec2, steps: usize) {
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
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
    }

    /// A click on a node's edge whose hand drifts 3 px off it neither moves
    /// the node nor records a position for it; a real drag still does both.
    /// egui 0.36 starts the drag the moment the pointer leaves the node, well
    /// before the click distance.
    #[test]
    fn an_edited_node_moves_only_on_a_real_drag() {
        let mut tab = EditTab::new();
        let start = box_of(&tab.gc, "ahb");
        let (plate, _) = tab.node("ahb");

        tab.gesture(
            egui::pos2(plate.center().x, plate.top() + 1.0),
            egui::vec2(0.0, -3.0),
            2,
        );
        assert_eq!(box_of(&tab.gc, "ahb"), start, "a slipped click moved it");
        assert!(tab.state.positions.is_empty(), "{:?}", tab.state.positions);

        let (plate, zoom) = tab.node("ahb");
        tab.gesture(plate.center(), egui::vec2(0.0, 30.0), 3);
        let went = (box_of(&tab.gc, "ahb").1 - start.1) * zoom;
        assert!(
            (went - 30.0).abs() < 1.0,
            "it followed the 30 px drag: {went}"
        );
        assert!(tab.state.positions.contains_key("ahb"), "and is recorded");
    }

    /// The crystal's MHz field on the canvas: a click whose hand drifts 3 px
    /// off it leaves the frequency alone, a real drag still changes it. Before
    /// `drag_value`, egui 0.36 took the drift as a drag and 8 MHz became
    /// 8.3 MHz, which regenerates main.rs.
    #[test]
    fn a_slipped_click_on_a_frequency_leaves_it_alone() {
        fn press_move_release(by: egui::Vec2) -> (u32, u32) {
            let mut gc = sample();
            {
                let n = gc.graph.nodes.iter_mut().find(|n| n.id == "hsi").unwrap();
                n.kind = NodeKind::Source {
                    min_hz: 4_000_000,
                    max_hz: 26_000_000,
                    gated: false,
                };
                n.state = NodeState::Source {
                    enabled: true,
                    hz: 8_000_000,
                };
            }
            gc.layout = auto_layout(&gc.graph);
            let ctx = egui::Context::default();
            let mut state = ClockUiState::default();
            let mut pass = 0u64;
            let mut step =
                |gc: &mut GraphClock, state: &mut ClockUiState, events: Vec<egui::Event>| {
                    pass += 1;
                    let input = egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1600.0, 900.0),
                        )),
                        time: Some(pass as f64 / 30.0),
                        predicted_dt: 1.0 / 30.0,
                        events,
                        ..Default::default()
                    };
                    let _ = crate::headless::run_ui(&ctx, input, |ui| {
                        let mut manual = false;
                        let limits = ClockLimits::default();
                        draw_graph_clock(ui, gc, &limits, &[], None, "stm32g0", state, &mut manual);
                    });
                };
            for _ in 0..4 {
                step(&mut gc, &mut state, vec![]);
            }
            let hz = |gc: &GraphClock| match gc
                .graph
                .nodes
                .iter()
                .find(|n| n.id == "hsi")
                .unwrap()
                .state
            {
                NodeState::Source { hz, .. } => hz,
                _ => 0,
            };
            // The click-and-drag widgets on screen: the Scene background is
            // the largest, and the smallest one inside it is the MHz field.
            let rects: Vec<egui::Rect> = ctx
                .viewport_for(egui::ViewportId::ROOT, |vp| {
                    vp.prev_pass
                        .widgets
                        .layers()
                        .flat_map(|(_, ws)| ws.iter())
                        .filter(|w| w.sense.senses_click() && w.sense.senses_drag())
                        .map(|w| (w.layer_id, w.interact_rect))
                        .collect::<Vec<_>>()
                })
                .into_iter()
                .map(|(l, r)| ctx.layer_transform_to_global(l).unwrap_or_default() * r)
                .collect();
            let canvas = rects
                .iter()
                .copied()
                .max_by(|a, b| a.area().total_cmp(&b.area()))
                .expect("the canvas");
            let field = rects
                .iter()
                .copied()
                .filter(|r| *r != canvas && canvas.contains_rect(*r))
                .min_by(|a, b| a.area().total_cmp(&b.area()))
                .expect("the MHz field");
            let before = hz(&gc);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let at = egui::pos2(field.center().x, field.top() + 1.0);
            step(&mut gc, &mut state, vec![egui::Event::PointerMoved(at)]);
            step(&mut gc, &mut state, vec![button(at, true)]);
            let mut p = at;
            for _ in 0..2 {
                p += by / 2.0;
                step(&mut gc, &mut state, vec![egui::Event::PointerMoved(p)]);
            }
            step(&mut gc, &mut state, vec![button(p, false)]);
            step(&mut gc, &mut state, vec![]);
            (before, hz(&gc))
        }

        let (before, after) = press_move_release(egui::vec2(0.0, -3.0));
        assert_eq!(before, 8_000_000);
        assert_eq!(after, before, "a slipped click changed the crystal");
        let (before, after) = press_move_release(egui::vec2(30.0, 0.0));
        assert!(after > before, "a 30 px drag right raised it: {after}");
    }
}
