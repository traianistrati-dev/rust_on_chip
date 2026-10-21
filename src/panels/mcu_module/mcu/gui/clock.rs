//! Clock tab rendering — dispatches to the per-family clock GUI.

use crate::panels::mcu_module::clock::{ClockConfig, gui as clock_gui};
use crate::panels::mcu_module::codegen::rcc::tree_drives_clock_code;
use crate::panels::mcu_module::mcu::model::Mcu;
use eframe::egui;

impl Mcu {
    /// Render the "Clock" tab. The returned [`ClockTabOut`] says whether the
    /// configuration changed (the app regenerates `main.rs` from MCU state every
    /// frame in `init_frame`) and whether the user asked to write the edited tree
    /// back into the chip definition.
    ///
    /// `state` is the project's Clock-tab state — dragged node positions, the
    /// last action's note, and whether the fields list is shown. The positions
    /// and the view preference persist in `project_structure.config`, not in the
    /// clock config, so moving a box never regenerates code.
    pub fn draw_clock_tab(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut clock_gui::ClockUiState,
    ) -> clock_gui::ClockTabOut {
        // Destructure so the config borrows mutably while the chip's limits,
        // presets and family stay readable alongside it.
        // On Async, what `embassy_stm32::init` refuses at boot and the
        // datasheet limits below cannot see.
        let boot_issue = if self.is_async() {
            let pins: Vec<_> = self.iter_all_pins().collect();
            crate::panels::mcu_module::codegen::rcc::async_boot_issue(
                &self.family,
                &self.clock,
                crate::panels::mcu_module::codegen::embassy_stm32::time_driver_timer_of(&pins),
            )
        } else {
            None
        };
        let Mcu {
            clock,
            clock_limits,
            clock_presets,
            clock_defaults,
            clock_manual,
            family,
            name,
            ..
        } = self;
        match clock {
            ClockConfig::Graph(gc) => {
                let mut out = clock_gui::draw_graph_clock(
                    ui,
                    gc,
                    clock_limits,
                    clock_presets,
                    clock_defaults.as_ref(),
                    family,
                    boot_issue.as_deref(),
                    state,
                    clock_manual,
                );
                // The tab replaced the tree in place, or asked for it to go.
                // Cloned while `gc` is still borrowed, so the swap below can
                // take `*clock`.
                let fresh = out.adopt_defaults.then(|| gc.graph.clone());
                if out.remove_clock {
                    *clock = ClockConfig::None;
                    *clock_defaults = None;
                    // Losing the tree changes the generated clock block as much
                    // as retuning it does.
                    out.changed = true;
                } else if let Some(graph) = fresh {
                    // A tree from the toolbar is this chip's new factory tree —
                    // Reset must go back to IT, not to the one it replaced.
                    *clock_defaults = Some(graph);
                }
                if out.remove_clock || out.adopt_defaults {
                    *clock_manual = !tree_drives_clock_code(family, clock);
                }
                out
            }
            ClockConfig::None => {
                // Not a dead end any more: the chip has no tree YET, and every
                // way of giving it one is offered here. A tree made this way is
                // the chip's, so it only outlives the session once "Save to
                // chip" writes it into the definition.
                match clock_gui::draw_no_clock(ui, name, family, clock_limits, state) {
                    Some(new_clock) => {
                        if let ClockConfig::Graph(gc) = &new_clock {
                            *clock_defaults = Some(gc.graph.clone());
                        }
                        *clock = new_clock;
                        // A tree the generic recipe can read makes the clock
                        // generated, so the hand-written default this chip got
                        // for having no recipe no longer holds — leaving it on
                        // would fence the block off and freeze it there.
                        *clock_manual = !tree_drives_clock_code(family, clock);
                        clock_gui::ClockTabOut {
                            changed: true,
                            ..Default::default()
                        }
                    }
                    None => clock_gui::ClockTabOut::default(),
                }
            }
        }
    }
}
