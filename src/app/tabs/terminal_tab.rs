//! Terminal tab — a streaming command console. Runs any command in the host's
//! shell (PowerShell on Windows, `$SHELL` on macOS / Linux) in the project
//! workspace, streaming output live. See [`crate::terminal::TerminalConsole`].

use crate::terminal::{LineKind, TerminalConsole, TerminalState};
use eframe::egui;
use egui_phosphor::regular as ph;
use std::sync::{Arc, Mutex};

/// Render a [`TerminalState`] scrollback: one `LayoutJob` per line, ANSI span
/// colours honoured, per-kind default colours. Shared with the RTT tab.
pub(crate) fn render_scrollback(
    ui: &mut egui::Ui,
    state: &Arc<Mutex<TerminalState>>,
    id_salt: &str,
    max_height: f32,
) {
    egui::ScrollArea::vertical()
        .id_salt(id_salt.to_owned())
        .max_height(max_height)
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .show(ui, |ui| {
            // Tells the reader thread this view is on screen (its repaints).
            crate::terminal::mark_drawn(state, ui.ctx());
            let st = state.lock().unwrap();
            for line in &st.lines {
                let default_col = match line.kind {
                    LineKind::Input => egui::Color32::from_rgb(150, 200, 130),
                    LineKind::Stdout => egui::Color32::from_gray(205),
                    LineKind::Stderr => egui::Color32::from_rgb(230, 140, 120),
                    LineKind::Notice => egui::Color32::from_rgb(130, 160, 200),
                };
                let mut job = egui::text::LayoutJob::default();
                for (text, col) in &line.spans {
                    job.append(
                        text,
                        0.0,
                        egui::TextFormat {
                            font_id: egui::FontId::monospace(11.5),
                            color: col.unwrap_or(default_col),
                            ..Default::default()
                        },
                    );
                }
                ui.label(job);
            }
        });
}

pub fn show_terminal_tab(ui: &mut egui::Ui, term: &mut TerminalConsole, ctx: &egui::Context) {
    let running = term.is_running();

    // ── Controls row: cwd + Stop + Clear ──────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(format!("{} ", ph::TERMINAL_WINDOW)).size(12.0));
        let shown = term.cwd.to_string_lossy().replace(r"\\?\", "");
        ui.label(
            egui::RichText::new(shown)
                .size(10.5)
                .monospace()
                .color(egui::Color32::from_rgb(150, 175, 210)),
        )
        .on_hover_text("Working directory. Use `cd <dir>` to change it.");

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button(egui::RichText::new(format!("{} Clear", ph::BROOM)).size(11.0))
                .clicked()
            {
                term.clear();
            }
            ui.add_enabled_ui(running, |ui| {
                if ui
                    .button(
                        egui::RichText::new(format!("{} Stop", ph::STOP_CIRCLE))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(230, 120, 110)),
                    )
                    .on_hover_text("Kill the running command (Ctrl+C)")
                    .clicked()
                {
                    term.stop();
                }
            });
            if running {
                // Throttled: a long-running command otherwise keeps the whole
                // app repainting every frame for its entire duration.
                crate::app::helpers::spinner::throttled_spinner(ui, 12.0);
            }
        });
    });

    ui.separator();

    // ── Output scrollback ─────────────────────────────────────────────────────
    let input_row_h = 26.0;
    let out_h = (ui.available_height() - input_row_h).max(40.0);
    render_scrollback(ui, &term.state, "terminal_scroll", out_h);

    // ── Input row ─────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(">").size(12.0).monospace().strong());
        // Up/Down recall history — consumed BEFORE the TextEdit so they don't
        // just move the text caret.
        let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
        let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
        let edit = ui
            .add_sized(
                [ui.available_width(), 22.0],
                egui::TextEdit::singleline(&mut term.input)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("type a command, Enter to run"),
            )
            // Name the shell: the command is passed to it verbatim, so which
            // dialect to type in is the first thing a user needs to know.
            .on_hover_text(format!(
                "Runs in {}. No TTY — a command that prompts will hang \
                 (Stop kills it).",
                crate::terminal::shell_name()
            ));
        if edit.has_focus() {
            if up {
                term.history_prev();
            } else if down {
                term.history_next();
            }
        }
        if crate::app::helpers::text_field::ended_with_enter(ui, &edit) {
            term.run(ctx);
            ui.memory_mut(|m| m.request_focus(edit.id)); // keep typing
        }
    });
}
