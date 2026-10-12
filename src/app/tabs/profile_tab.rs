//! Profile tab — code efficiency. Two modes:
//! - **Static** (`cargo bloat`): `.text`/Flash size per function or crate.
//! - **Runtime** (on-target flamegraph): probe-rs stack-sampling of the running
//!   firmware (see [`crate::flamegraph`]), shown as a flamegraph or counted per
//!   source line ("By line").

use crate::flamegraph::{FlameFileOp, FlameNode, FlameResult, FlameState, LineStat, std_err_pct};
use crate::profile::{BloatRow, ProfileMode, ProfileState};
use eframe::egui;
use egui_phosphor::regular as ph;
use std::sync::{Arc, Mutex};

#[allow(clippy::too_many_arguments)]
pub fn show_profile_tab(
    ui: &mut egui::Ui,
    mode: &mut ProfileMode,
    // ── Static (cargo bloat) ──
    state: &Arc<Mutex<ProfileState>>,
    by_crate: &mut bool,
    run_clicked: &mut bool,
    // ── Runtime (flamegraph) ──
    flame_state: &Arc<Mutex<FlameState>>,
    sample_clicked: &mut bool,
    // Runtime's "Flame | By line" switch and the editor tint toggle
    // (`AppIde::flame_by_line` / `flame_heat`), the files edited since the
    // sample (their rows keep the old line numbers), and the jump a row click
    // asks for - the same one the diagnostic tabs use.
    flame_by_line: &mut bool,
    flame_heat: &mut bool,
    flame_stale: &[String],
    nav: &mut Option<(String, usize, egui::Color32)>,
    // "Save…" / "Load…" of a run's raw samples (the caller opens the dialog).
    flame_file: &mut Option<FlameFileOp>,
    // The probe-rs chip name (for the runtime hover), and whether a chip config
    // exists (gates both modes).
    chip: &str,
    can_run: bool,
    // Shared probe-rs probe row (same as Debug / RTT / Flash) — shown in the
    // Runtime view so the probe can be scanned/picked from here too.
    probe_list: &[crate::probe::ProbeInfo],
    selected_probe: &mut Option<String>,
    probe_scan: &mut bool,
    probe_scan_err: Option<&str>,
    toolchain: &crate::panels::mcu_module::mcu_catalog::ToolchainKind,
    // Tools confirmed missing — buttons needing one are greyed out with a
    // "install it in Tools" hint (see `super::tool_missing`).
    missing_tools: &[&'static str],
    // Who inside the IDE holds the device, if anyone. Threaded from the panel
    // so every tab that lists probes gives the SAME answer for why the list
    // is empty - see `tabs::no_probe_message`.
    holder: Option<(&str, &str)>,
) {
    // ── Mode switch ─────────────────────────────────────────────────────────────
    ui.horizontal(|ui| {
        ui.selectable_value(mode, ProfileMode::Static, "Static (size)")
            .on_hover_text(
                "cargo bloat: Flash (.text) size per function / crate. No target needed.",
            );
        ui.selectable_value(mode, ProfileMode::Runtime, "Runtime (flamegraph)")
            .on_hover_text(
                "On-target statistical profiler: halt-samples the running firmware's call \
                 stack via probe-rs. Needs the firmware flashed + running.",
            );
    });
    ui.separator();

    match mode {
        ProfileMode::Static => static_view(
            ui,
            state,
            by_crate,
            run_clicked,
            can_run && !super::tool_missing(missing_tools, "cargo-bloat"),
            super::tool_missing(missing_tools, "cargo-bloat"),
        ),
        ProfileMode::Runtime => runtime_view(
            ui,
            flame_state,
            sample_clicked,
            flame_by_line,
            flame_heat,
            flame_stale,
            nav,
            flame_file,
            chip,
            can_run,
            probe_list,
            selected_probe,
            probe_scan,
            probe_scan_err,
            toolchain,
            super::tool_missing(missing_tools, "probe-rs"),
            holder,
        ),
    }
}

// ── Static (cargo bloat) ──────────────────────────────────────────────────────

fn static_view(
    ui: &mut egui::Ui,
    state: &Arc<Mutex<ProfileState>>,
    by_crate: &mut bool,
    run_clicked: &mut bool,
    can_run: bool,
    // `cargo-bloat` proven absent → explain that instead of a generic disable.
    no_bloat: bool,
) {
    let st = state.lock().unwrap().clone();
    let busy = st.is_busy();

    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !busy && can_run,
                egui::Button::new(
                    egui::RichText::new(format!("{} Analyze", ph::CHART_BAR))
                        .size(10.5)
                        .color(if !busy && can_run {
                            egui::Color32::from_rgb(120, 190, 230)
                        } else {
                            egui::Color32::GRAY
                        }),
                ),
            )
            .on_hover_text(
                "cargo bloat --release: build, then report the .text (Flash) size of each \
                 function.\nNeeds cargo-bloat: cargo install cargo-bloat",
            )
            .on_disabled_hover_text(if no_bloat {
                super::needs_tool_hint("cargo-bloat")
            } else {
                "Busy, or no chip config exists yet.".to_owned()
            })
            .clicked()
        {
            *run_clicked = true;
        }
        let toggle = ui
            .selectable_label(*by_crate, egui::RichText::new("by crate").size(10.5))
            .on_hover_text("Group the sizes by CRATE instead of by function.");
        if toggle.clicked() {
            *by_crate = !*by_crate;
            if matches!(st, ProfileState::Done(_)) && can_run {
                *run_clicked = true;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (text, color) = match &st {
                ProfileState::Idle => ("—".to_owned(), egui::Color32::GRAY),
                ProfileState::Running => (
                    "building + analyzing…".to_owned(),
                    egui::Color32::from_rgb(220, 180, 60),
                ),
                ProfileState::Done(r) => (
                    format!(
                        "{} {}",
                        r.rows.len(),
                        if r.by_crate { "crates" } else { "fns" }
                    ),
                    egui::Color32::from_rgb(120, 190, 120),
                ),
                ProfileState::Failed(_) => {
                    ("failed".to_owned(), egui::Color32::from_rgb(220, 110, 100))
                }
            };
            ui.label(egui::RichText::new(text).size(10.5).color(color));
            if busy {
                ui.spinner();
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(150));
            }
        });
    });
    ui.separator();

    match &st {
        ProfileState::Idle => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Click Analyze to break down the Flash (.text) size per function. \
                     Biggest first — toggle \"by crate\" to group by dependency.",
                )
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
        }
        ProfileState::Running => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Building --release and running cargo bloat…")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(200, 180, 120)),
            );
        }
        ProfileState::Failed(e) => {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // `[BLOAT_MISSING]` & friends get the shared explanation card
                    // (with an "Open Tools" button); anything else prints as-is.
                    if !crate::failure_hint::show_card(ui, e, |_| {}) {
                        ui.label(
                            egui::RichText::new(e)
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_rgb(230, 130, 120)),
                        );
                    }
                });
        }
        ProfileState::Done(res) => {
            let max = res
                .rows
                .iter()
                .map(|r| r.size_bytes)
                .max()
                .unwrap_or(1)
                .max(1);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if !res.summary.is_empty() {
                        ui.label(
                            egui::RichText::new(&res.summary)
                                .size(11.0)
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.add_space(4.0);
                    }
                    if res.rows.is_empty() {
                        ui.label(
                            egui::RichText::new(&res.raw)
                                .monospace()
                                .size(10.5)
                                .color(egui::Color32::from_gray(190)),
                        );
                    }
                    for r in &res.rows {
                        bloat_row(ui, r, max);
                    }
                });
        }
    }
}

/// One size bar: width ∝ size, coloured small→large, `size · crate · name` on it.
fn bloat_row(ui: &mut egui::Ui, r: &BloatRow, max: u64) {
    let frac = (r.size_bytes as f32 / max as f32).clamp(0.0, 1.0);
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), egui::Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, egui::Color32::from_gray(38));
    let bar = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * frac, rect.height()));
    let col = egui::Color32::from_rgb(
        (70.0 + frac * 155.0) as u8,
        (170.0 - frac * 70.0) as u8,
        (150.0 - frac * 100.0) as u8,
    );
    painter.rect_filled(bar, 2.0, col);
    let label = if r.name.is_empty() {
        format!("{}   {}", r.size_label, r.crate_name)
    } else if r.crate_name.is_empty() {
        format!("{}   {}", r.size_label, r.name)
    } else {
        format!("{}   {}::{}", r.size_label, r.crate_name, r.name)
    };
    // Same fitting as the flamegraph: a Rust symbol is long enough to run past
    // any row it is given.
    draw_fitted_text(
        painter,
        rect,
        &label,
        egui::FontId::monospace(11.0),
        egui::Color32::from_gray(235),
    );
    resp.on_hover_text(format!(
        "{:.1}% of .text · {} bytes",
        r.text_pct, r.size_bytes
    ));
    ui.add_space(2.0);
}

// ── Runtime (flamegraph) ──────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn runtime_view(
    ui: &mut egui::Ui,
    flame_state: &Arc<Mutex<FlameState>>,
    sample_clicked: &mut bool,
    flame_by_line: &mut bool,
    flame_heat: &mut bool,
    flame_stale: &[String],
    nav: &mut Option<(String, usize, egui::Color32)>,
    flame_file: &mut Option<FlameFileOp>,
    chip: &str,
    can_run: bool,
    probe_list: &[crate::probe::ProbeInfo],
    selected_probe: &mut Option<String>,
    probe_scan: &mut bool,
    probe_scan_err: Option<&str>,
    toolchain: &crate::panels::mcu_module::mcu_catalog::ToolchainKind,
    // `probe-rs` proven absent → the on-target sampler can't run at all.
    no_probe_rs: bool,
    // Who inside the IDE holds the device - see `tabs::no_probe_message`.
    holder: Option<(&str, &str)>,
) {
    // Halt-sampling drives probe-rs, so a chip it has no target for cannot be
    // sampled. Asked of the installed binary — see `probe::chip_gap`.
    let chip_gap = (!no_probe_rs)
        .then(|| crate::probe::chip_gap(chip))
        .flatten();
    let can_run = can_run && !no_probe_rs && chip_gap.is_none();
    let st = flame_state.lock().unwrap().clone();
    let busy = st.is_busy();

    // One row: the probe it will use, then the action, then what it targets.
    ui.horizontal_wrapped(|ui| {
        // Shared probe picker — the SAME probe as the Debug / RTT / Flash tabs.
        super::probe_selector_ui(
            ui,
            probe_list,
            selected_probe,
            probe_scan,
            probe_scan_err,
            toolchain,
            holder,
        );
        ui.separator();
        if ui
            .add_enabled(
                !busy && can_run,
                egui::Button::new(
                    egui::RichText::new(format!("{} Sample", ph::FLAME))
                        .size(10.5)
                        .color(if !busy && can_run {
                            egui::Color32::from_rgb(230, 150, 90)
                        } else {
                            egui::Color32::GRAY
                        }),
                ),
            )
            .on_hover_text(
                "Halt-sample the RUNNING firmware's call stack via probe-rs, then fold it \
                 into a flamegraph.\nThe firmware must already be flashed + running. \
                 Intrusive (each halt stops the CPU) -> a statistical view.",
            )
            .on_disabled_hover_text(if no_probe_rs {
                super::needs_tool_hint("probe-rs")
            } else if let Some(gap) = &chip_gap {
                gap.clone()
            } else {
                "Sampling, or no chip config exists yet.".to_owned()
            })
            .clicked()
        {
            *sample_clicked = true;
        }
        // A run's raw samples, to and from a file: to look again later, to
        // compare with the next run, or to hand to someone. A loaded run is
        // counted again by this version of the IDE.
        let done = matches!(st, FlameState::Done(_));
        if ui
            .add_enabled(
                done && !busy,
                egui::Button::new(
                    egui::RichText::new(format!("{} Save…", ph::FLOPPY_DISK)).size(10.5),
                ),
            )
            .on_hover_text(
                "Save this run's raw samples - the stacks probe-rs reported and the \
                 source they point at - to a file.",
            )
            .on_disabled_hover_text("Sample, or load, a run first.")
            .clicked()
        {
            *flame_file = Some(FlameFileOp::Save);
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(
                    egui::RichText::new(format!("{} Load…", ph::FOLDER_OPEN)).size(10.5),
                ),
            )
            .on_hover_text(
                "Load a saved run. Its samples are counted again by this version of the \
                 IDE, with the release ELF it was sampled with while that is still the \
                 same build. No board needed.",
            )
            .clicked()
        {
            *flame_file = Some(FlameFileOp::Load);
        }
        ui.separator();
        ui.label(
            egui::RichText::new("Chip:")
                .size(10.5)
                .color(egui::Color32::GRAY),
        );
        ui.label(
            egui::RichText::new(if chip.is_empty() { "—" } else { chip })
                .size(10.5)
                .monospace()
                .color(egui::Color32::from_rgb(120, 160, 200)),
        );
        ui.with_layout(
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| match &st {
                FlameState::Sampling(done, total) => {
                    ui.label(
                        egui::RichText::new(format!("{done}/{total}"))
                            .size(10.5)
                            .color(egui::Color32::from_rgb(220, 180, 60)),
                    );
                    ui.spinner();
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(120));
                }
                FlameState::Building => {
                    ui.label(
                        egui::RichText::new("building…")
                            .size(10.5)
                            .color(egui::Color32::from_rgb(220, 180, 60)),
                    );
                    ui.spinner();
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(150));
                }
                FlameState::Loading => {
                    ui.label(
                        egui::RichText::new("loading…")
                            .size(10.5)
                            .color(egui::Color32::from_rgb(220, 180, 60)),
                    );
                    ui.spinner();
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(150));
                }
                FlameState::Done(r) => {
                    // Right to left: the warning first, so it is never the one
                    // squeezed; the file name last, cut to the room that is left
                    // (its full path is in the hover).
                    if r.elf_changed {
                        ui.label(
                            egui::RichText::new(format!("{} ELF rebuilt", ph::WARNING))
                                .size(10.5)
                                .color(egui::Color32::from_rgb(220, 170, 60)),
                        )
                        .on_hover_text(concat!(
                            "The release ELF this run was sampled with is gone or was rebuilt ",
                            "since (Flash, Size and Sample all rebuild it). Its frames are ",
                            "counted as probe-rs reported them: no Cortex-M caller fix, no ",
                            "unwinding of ESP stacks - Sample again for those.",
                        ));
                    }
                    let green = egui::Color32::from_rgb(120, 190, 120);
                    let count = ui.label(
                        egui::RichText::new(format!("{} samples", r.samples))
                            .size(10.5)
                            .color(green),
                    );
                    match &r.loaded_from {
                        Some(p) => {
                            let name = p
                                .file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("{name} ·"))
                                        .size(10.5)
                                        .color(green),
                                )
                                .truncate(),
                            )
                            .on_hover_text(format!("Loaded from {}", p.display()));
                        }
                        None => {
                            count.on_hover_text("Sampled live.");
                        }
                    }
                }
                FlameState::Failed(_) => {
                    ui.label(
                        egui::RichText::new("failed")
                            .size(10.5)
                            .color(egui::Color32::from_rgb(220, 110, 100)),
                    );
                }
                FlameState::Idle => {}
            },
        );
    });
    ui.separator();

    match &st {
        FlameState::Idle => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Flash + run your firmware, then click Sample. probe-rs halt-samples the \
                     call stack; the flamegraph shows where the CPU spends time (wider = more \
                     samples). Statistical + intrusive — not cycle-accurate.",
                )
                .size(11.0)
                .color(egui::Color32::GRAY),
            );
        }
        FlameState::Building => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Building --release (for symbols)…")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(200, 180, 120)),
            );
        }
        FlameState::Sampling(done, total) => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!("Sampling the call stack… {done}/{total}"))
                    .size(11.0)
                    .color(egui::Color32::from_rgb(200, 180, 120)),
            );
        }
        FlameState::Loading => {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new("Reading the saved run and counting its samples…")
                    .size(11.0)
                    .color(egui::Color32::from_rgb(200, 180, 120)),
            );
        }
        FlameState::Failed(e) => {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Same card as the static view above: a probe that won't
                    // open (`[PROBE_OPEN_FAILED]`) is the failure this path hits
                    // most, and it has a fix that the raw chain doesn't name.
                    if !crate::failure_hint::show_card(ui, e, |_| {}) {
                        ui.label(
                            egui::RichText::new(e)
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_rgb(230, 130, 120)),
                        );
                    }
                });
        }
        FlameState::Done(res) => {
            ui.horizontal(|ui| {
                ui.selectable_value(
                    flame_by_line,
                    false,
                    egui::RichText::new("Flame").size(10.5),
                )
                .on_hover_text(
                    "The samples' call stacks folded into a flamegraph: which FUNCTIONS the \
                         time goes to.",
                );
                ui.selectable_value(
                    flame_by_line,
                    true,
                    egui::RichText::new("By line").size(10.5),
                )
                .on_hover_text(
                    "The same samples counted per SOURCE LINE of the project: which lines \
                         the time goes to.",
                );
                // In both views: the tint belongs to the run, not to the
                // table, and a band with no visible switch would be a mystery.
                ui.separator();
                ui.checkbox(flame_heat, egui::RichText::new("Heat in editor").size(10.5))
                    .on_hover_text(
                        "Tint every line the samples caught, in the editor - the strongest is \
                         the run's hottest line - and hover a line for its numbers.\nHidden on \
                         a file edited since the sample: its line numbers may point at other \
                         code by now.",
                    );
            });
            if *flame_by_line {
                by_line_view(ui, res, flame_stale, nav);
            } else {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        flame_widget(ui, &res.root);
                    });
            }
        }
    }
}

// ── Runtime: by line ──────────────────────────────────────────────────────────

/// Column the by-line table is sorted on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LineSort {
    SelfTime,
    Total,
    Location,
}

const LINE_ROW_H: f32 = 18.0;
/// Left edge of each column: self (with its bar), ±, total, samples, line.
/// The code takes the rest.
const COLS: [f32; 5] = [0.0, 62.0, 106.0, 162.0, 206.0];
const LOC_W: f32 = 190.0;
/// A clicked row's band in the editor: the location column's blue, off the
/// heat scale - an orange band would stack on the line's heat and make a cold
/// line read hot.
const JUMP_BAND: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(120, 160, 200, 30);

/// Where column `i` of a row starting at `left` begins and ends: the five
/// cells at [`COLS`], then the code up to `right`.
fn col_span(left: f32, right: f32, i: usize) -> (f32, f32) {
    let start = |i: usize| COLS.get(i).copied().unwrap_or(COLS[4] + LOC_W);
    let end = if i >= COLS.len() {
        right
    } else {
        left + start(i + 1)
    };
    (left + start(i), end.min(right))
}

/// The per-line table: two fixed rows (idle, outside the project), then every
/// sampled project line. A row click opens the line in the editor.
fn by_line_view(
    ui: &mut egui::Ui,
    res: &FlameResult,
    stale: &[String],
    nav: &mut Option<(String, usize, egui::Color32)>,
) {
    let by = &res.by_line;
    let n = by.samples.max(1);
    let pct = |k: usize| k as f32 * 100.0 / n as f32;
    let sort_id = ui.id().with("flame_line_sort");
    let mut sort = ui
        .data(|d| d.get_temp::<LineSort>(sort_id))
        .unwrap_or(LineSort::SelfTime);

    ui.label(
        egui::RichText::new(
            "Self = time on the line itself, the library calls it makes included. Total = \
             time with the line anywhere on the stack. ± = one standard error: more samples, \
             smaller ±.",
        )
        .size(10.5)
        .color(egui::Color32::GRAY),
    );

    // ── Header ────────────────────────────────────────────────────────────────
    let (head, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LINE_ROW_H),
        egui::Sense::hover(),
    );
    let heads: [(&str, Option<LineSort>, &str); 6] = [
        (
            "Self",
            Some(LineSort::SelfTime),
            "Share of the samples spent on this line itself.",
        ),
        (
            "±",
            None,
            "One standard error of the share, √(p(1−p)/n) in points. Twice it covers ~95% \
             of runs.",
        ),
        (
            "Total",
            Some(LineSort::Total),
            "Share of the samples with this line anywhere on the call stack.",
        ),
        ("n", None, "Samples on the line itself."),
        (
            "Line",
            Some(LineSort::Location),
            "File and line, as this run compiled them.",
        ),
        ("Code", None, "The line's text, as this run compiled it."),
    ];
    for (i, (label, key, hover)) in heads.iter().enumerate() {
        let (x0, x1) = col_span(head.left(), head.right(), i);
        if x1 <= x0 {
            continue;
        }
        let cell =
            egui::Rect::from_min_max(egui::pos2(x0, head.top()), egui::pos2(x1, head.bottom()));
        let resp = ui.interact(
            cell,
            sort_id.with(i),
            if key.is_some() {
                egui::Sense::click()
            } else {
                egui::Sense::hover()
            },
        );
        let active = *key == Some(sort);
        let text = if active {
            format!("{label} {}", ph::CARET_DOWN)
        } else {
            (*label).to_owned()
        };
        draw_fitted_text(
            ui.painter(),
            cell,
            &text,
            egui::FontId::proportional(10.5),
            if active || resp.hovered() {
                egui::Color32::from_gray(230)
            } else {
                egui::Color32::from_gray(160)
            },
        );
        if let Some(k) = key
            && resp.clicked()
        {
            sort = *k;
        }
        resp.on_hover_text(*hover);
    }
    ui.data_mut(|d| d.insert_temp(sort_id, sort));

    let mut order: Vec<&LineStat> = by.lines.iter().collect();
    match sort {
        // `by_line` already sorts this way.
        LineSort::SelfTime => {}
        LineSort::Total => order.sort_by(|a, b| {
            b.total_n
                .cmp(&a.total_n)
                .then(b.self_n.cmp(&a.self_n))
                .then_with(|| a.file.cmp(&b.file))
                .then(a.line.cmp(&b.line))
        }),
        LineSort::Location => order.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line))),
    }

    // ── Rows: idle, outside, no line (only when there is one), the lines ────
    let dim = egui::Color32::from_gray(150);
    let no_line_row = by.no_line > 0;
    let fixed = if no_line_row { 3 } else { 2 };
    egui::ScrollArea::vertical()
        // Not the flamegraph's scroll state: switching views would open the
        // table at the graph's offset, the fixed rows already scrolled away.
        .id_salt("flame_by_line")
        .auto_shrink([false, false])
        .show_rows(ui, LINE_ROW_H, order.len() + fixed, |ui, range| {
            for r in range {
                if r == 0 {
                    line_row(
                        ui,
                        [
                            format!("{:>5.1}%", pct(by.idle)),
                            format!("±{:.1}", std_err_pct(by.idle, n)),
                            String::new(),
                            by.idle.to_string(),
                            "Idle".to_owned(),
                        ],
                        "waiting for an interrupt (wfi / wfe / waiti)",
                        pct(by.idle) / 100.0,
                        dim,
                    )
                    .on_hover_text(
                        "Samples that caught the core asleep, waiting for an interrupt: time \
                         the firmware had to spare.",
                    );
                    continue;
                }
                if r == 1 {
                    let tip = fns_tip(
                        "Samples with none of the project's code on the stack: interrupt \
                         handlers in the HAL, the executor, the ROM, a radio blob.",
                        &by.outside_fns,
                    );
                    line_row(
                        ui,
                        [
                            format!("{:>5.1}%", pct(by.outside)),
                            format!("±{:.1}", std_err_pct(by.outside, n)),
                            String::new(),
                            by.outside.to_string(),
                            "Outside project".to_owned(),
                        ],
                        by.outside_fns.first().map_or("", |(f, _)| f.as_str()),
                        pct(by.outside) / 100.0,
                        dim,
                    )
                    .on_hover_text(tip);
                    continue;
                }
                if r == 2 && no_line_row {
                    let tip = fns_tip(
                        "Samples in the project's own code that the DWARF gives no line: \
                         optimised instructions on a \"line 0\" row. Not the caller's \
                         time, so counted here instead of on the line that called it.",
                        &by.no_line_fns,
                    );
                    line_row(
                        ui,
                        [
                            format!("{:>5.1}%", pct(by.no_line)),
                            format!("±{:.1}", std_err_pct(by.no_line, n)),
                            String::new(),
                            by.no_line.to_string(),
                            "No line".to_owned(),
                        ],
                        by.no_line_fns.first().map_or("", |(f, _)| f.as_str()),
                        pct(by.no_line) / 100.0,
                        dim,
                    )
                    .on_hover_text(tip);
                    continue;
                }
                let l = order[r - fixed];
                let edited = stale.contains(&l.file);
                let resp = line_row(
                    ui,
                    [
                        format!("{:>5.1}%", pct(l.self_n)),
                        format!("±{:.1}", std_err_pct(l.self_n, n)),
                        format!("{:>5.1}%", pct(l.total_n)),
                        l.self_n.to_string(),
                        format!("{}:{}", file_name(&l.file), l.line),
                    ],
                    &l.code,
                    pct(l.self_n) / 100.0,
                    if edited {
                        egui::Color32::from_rgb(220, 170, 60)
                    } else {
                        egui::Color32::from_rgb(120, 160, 200)
                    },
                );
                let mut tip = format!(
                    "{}:{}  in {}\nSelf: {} of {} samples · Total: {}",
                    l.file, l.line, l.func, l.self_n, by.samples, l.total_n
                );
                if edited {
                    tip.push_str(
                        "\n\nThe file was edited after this sample, so the line may have \
                         moved. Sample again for fresh numbers.",
                    );
                }
                tip.push_str("\n\nClick to open the line.");
                if resp.on_hover_text(tip).clicked() {
                    *nav = Some((l.file.clone(), l.line as usize, JUMP_BAND));
                }
            }
        });
}

/// A fixed row's hover: what it counts, then the functions it met most.
fn fns_tip(intro: &str, fns: &[(String, usize)]) -> String {
    if fns.is_empty() {
        return intro.to_owned();
    }
    let list: Vec<String> = fns.iter().map(|(f, k)| format!("{k:>4}  {f}")).collect();
    format!("{intro} Most often in:\n{}", list.join("\n"))
}

/// `src/app/pins.rs` -> `pins.rs`: the column is narrow, and the full path is
/// in the hover.
fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// One table row: the five cells at [`COLS`], the code after them, and a bar
/// under the self cell, `bar` (0..=1) of its width.
fn line_row(
    ui: &mut egui::Ui,
    cells: [String; 5],
    code: &str,
    bar: f32,
    loc_color: egui::Color32,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), LINE_ROW_H),
        egui::Sense::click(),
    );
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, 2.0, egui::Color32::from_gray(45));
    }
    let bar_w = (COLS[1] - 6.0) * bar.clamp(0.0, 1.0);
    if bar_w > 0.0 {
        painter.rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(bar_w.max(1.0), rect.height())),
            2.0,
            egui::Color32::from_rgb(150, 70, 35),
        );
    }
    let font = egui::FontId::monospace(10.5);
    let texts = cells
        .iter()
        .map(String::as_str)
        .chain(std::iter::once(code));
    for (i, cell) in texts.enumerate() {
        let (x0, x1) = col_span(rect.left(), rect.right(), i);
        if x1 <= x0 {
            continue;
        }
        let color = match i {
            4 => loc_color,
            5 => egui::Color32::from_gray(185),
            _ => egui::Color32::from_gray(225),
        };
        draw_fitted_text(
            painter,
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            cell,
            font.clone(),
            color,
        );
    }
    resp
}

/// Icicle flamegraph: root at the top (full width), callees stacked below, each
/// as wide as its share of the parent's samples. Warm per-symbol colours.
/// **Clicking** a segment opens a 90%-wide details popup (function, samples, %,
/// and the callee breakdown) — richer than a hover tooltip and it stays put.
fn flame_widget(ui: &mut egui::Ui, root: &FlameNode) {
    let total = root.count.max(1);
    const ROW_H: f32 = 17.0;
    let depth = tree_depth(root);
    let avail_w = ui.available_width();
    let height = (ROW_H * depth as f32).max(ROW_H);

    // Horizontal zoom (mouse wheel up = in / down = out), persisted per-widget.
    // The graph is drawn `avail_w * zoom` wide inside a horizontal ScrollArea, so
    // a zoomed / very long graph gets a horizontal scrollbar. Zoom is ANCHORED at
    // the cursor: on a wheel step we adjust the scroll offset so the content point
    // under the pointer stays put (applied next frame via `off_id`).
    let zoom_id = ui.id().with("flame_zoom");
    let off_id = ui.id().with("flame_zoom_off");
    let mut zoom: f32 = ui.data(|d| d.get_temp(zoom_id)).unwrap_or(1.0);
    let content_w = (avail_w * zoom).max(avail_w);
    // A pending cursor-anchored offset stored last frame → apply once.
    let apply_off: Option<f32> = ui.data_mut(|d| {
        let v = d.get_temp::<f32>(off_id);
        if v.is_some() {
            d.remove::<f32>(off_id);
        }
        v
    });

    let mut hit: Option<FlameNode> = None;
    let mut scroll = egui::ScrollArea::horizontal().auto_shrink([false, false]);
    if let Some(x) = apply_off {
        scroll = scroll.horizontal_scroll_offset(x);
    }
    // Returns `Some((scroll_y, content_x_under_cursor))` when the wheel turned.
    let out = scroll.show(ui, |ui| {
        let (rect, resp) =
            ui.allocate_exact_size(egui::vec2(content_w, height), egui::Sense::click());
        // Vertical wheel = zoom; consume it so no scroll area scrolls with it.
        let wheel = resp.hover_pos().and_then(|hp| {
            let sy = ui.input(|i| i.smooth_scroll_delta.y);
            (sy != 0.0).then(|| {
                ui.input_mut(|i| i.smooth_scroll_delta.y = 0.0);
                (sy, hp.x - rect.left()) // content-x (0..content_w) under the cursor
            })
        });
        let painter = ui.painter().with_clip_rect(rect);
        // The click position picks exactly one segment (its row + x span).
        let click = resp
            .clicked()
            .then(|| resp.interact_pointer_pos())
            .flatten();
        draw_node(
            &painter,
            root,
            rect.left(),
            rect.top(),
            content_w,
            ROW_H,
            click,
            &mut hit,
        );
        wheel
    });
    if let Some((sy, content_x)) = out.inner {
        // Keep the point under the cursor fixed: same content fraction stays at
        // the same viewport x after the zoom.
        let off = out.state.offset.x;
        let vp_x = content_x - off; // cursor position within the viewport (fixed)
        let frac = content_x / (avail_w * zoom); // fraction of the OLD content width
        let new_zoom = (zoom * (sy * 0.0015).exp()).clamp(1.0, 64.0);
        let new_off = (frac * avail_w * new_zoom - vp_x).max(0.0);
        zoom = new_zoom;
        ui.data_mut(|d| {
            d.insert_temp(zoom_id, zoom);
            d.insert_temp(off_id, new_off);
        });
        ui.ctx().request_repaint();
    } else {
        ui.data_mut(|d| d.insert_temp(zoom_id, zoom));
    }

    // Click → details popup, capped at the Profile tab width (never wider than
    // the panel); long names wrap inside instead of stretching it.
    let sel_id = ui.id().with("flame_selected");
    if let Some(node) = hit {
        ui.data_mut(|d| d.insert_temp(sel_id, (node, total)));
    }
    if let Some((node, total)) = ui.data(|d| d.get_temp::<(FlameNode, usize)>(sel_id)) {
        let mut open = true;
        let pw = (avail_w - 12.0).max(160.0);
        egui::Window::new("Frame details")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .max_width(pw)
            .open(&mut open)
            .show(ui.ctx(), |ui| {
                ui.set_max_width(pw);
                flame_details(ui, &node, total);
            });
        if !open {
            ui.data_mut(|d| d.remove::<(FlameNode, usize)>(sel_id));
        }
    }
}

/// The details popup body for a clicked flame segment.
fn flame_details(ui: &mut egui::Ui, node: &FlameNode, total: usize) {
    let pct = node.count as f32 / total as f32 * 100.0;
    // Wrap the (often huge, monomorphised) name so it never stretches the popup.
    ui.add(
        egui::Label::new(
            egui::RichText::new(&node.name)
                .monospace()
                .size(12.5)
                .strong(),
        )
        .wrap(),
    );
    ui.label(
        egui::RichText::new(format!("{} samples · {:.1}% of all", node.count, pct))
            .color(egui::Color32::from_rgb(120, 190, 120)),
    );
    if node.children.is_empty() {
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new("leaf — no callees sampled below this frame")
                .italics()
                .color(egui::Color32::GRAY),
        );
        return;
    }
    ui.separator();
    ui.label(egui::RichText::new("Callees (share of this frame):").size(11.5));
    let parent = node.count.max(1) as f32;
    let mut kids: Vec<&FlameNode> = node.children.iter().collect();
    kids.sort_by(|a, b| b.count.cmp(&a.count));
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            for c in kids {
                let cp = c.count as f32 / parent * 100.0;
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [46.0, 14.0],
                        egui::Label::new(
                            egui::RichText::new(format!("{cp:>5.1}%"))
                                .monospace()
                                .size(10.5),
                        ),
                    );
                    // mini share bar
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(80.0, 10.0), egui::Sense::hover());
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            r.min,
                            egui::vec2((80.0 * cp / 100.0).max(1.0), 10.0),
                        ),
                        1.0,
                        node_color(&c.name),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(format!("{}  {}", c.count, c.name))
                                .monospace()
                                .size(10.5),
                        )
                        .truncate(),
                    )
                    .on_hover_text(&c.name);
                });
            }
        });
}

fn tree_depth(node: &FlameNode) -> usize {
    1 + node.children.iter().map(tree_depth).max().unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
fn draw_node(
    painter: &egui::Painter,
    node: &FlameNode,
    x: f32,
    y: f32,
    w: f32,
    row_h: f32,
    click: Option<egui::Pos2>,
    hit: &mut Option<FlameNode>,
) {
    if w < 1.0 {
        return;
    }
    let rect = egui::Rect::from_min_size(
        egui::pos2(x, y),
        egui::vec2((w - 1.0).max(0.5), row_h - 1.0),
    );
    painter.rect_filled(rect, 1.0, node_color(&node.name));
    if w > 26.0 {
        draw_fitted_text(
            painter,
            rect,
            &node.name,
            egui::FontId::monospace(10.5),
            egui::Color32::from_rgb(25, 20, 15),
        );
    }
    // The click lands in exactly one row, so this matches a single segment.
    if let Some(p) = click {
        if rect.contains(p) {
            *hit = Some(node.clone());
        }
    }
    let mut cx = x;
    let parent = node.count.max(1) as f32;
    for c in &node.children {
        let cw = w * (c.count as f32 / parent);
        draw_node(painter, c, cx, y + row_h, cw, row_h, click, hit);
        cx += cw;
    }
}

/// Warm per-symbol colour (classic flamegraph palette), stable per name.
fn node_color(name: &str) -> egui::Color32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    name.hash(&mut h);
    let x = h.finish();
    egui::Color32::from_rgb(
        205 + (x % 50) as u8,
        90 + ((x >> 8) % 120) as u8,
        45 + ((x >> 16) % 40) as u8,
    )
}

/// Draw `text` inside `rect`, truncated to what actually fits and clipped to the
/// box either way.
///
/// Both halves matter. The truncation is MEASURED, not estimated: the old
/// `≈6 px/char` guess was optimistic for this font, so long Rust symbols
/// (`…gpio::Alternate<stm32f1xx_hal::gpio::OpenDrain>>, …`) kept a few characters
/// too many and ran into the neighbouring segment. And the painter is clipped
/// regardless, so no future rounding error can spill again — a label that leaves
/// its box doesn't just look wrong, it reads as belonging to the box it lands in.
fn draw_fitted_text(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
) {
    const PAD: f32 = 3.0;
    let avail = rect.width() - 2.0 * PAD;
    if avail <= 0.0 || text.is_empty() {
        return;
    }
    let galley = painter.layout_no_wrap(text.to_owned(), font.clone(), color);
    let shown = if galley.size().x <= avail {
        text.to_owned()
    } else {
        // Monospace: one measurement gives the exact per-character width.
        let chars = text.chars().count().max(1);
        let cw = galley.size().x / chars as f32;
        let keep = ((avail / cw).floor() as usize).saturating_sub(1);
        if keep == 0 {
            return; // narrower than a single glyph — the colour is the label
        }
        format!("{}…", text.chars().take(keep).collect::<String>())
    };
    painter
        .with_clip_rect(rect.intersect(painter.clip_rect()))
        .text(
            rect.left_center() + egui::vec2(PAD, 0.0),
            egui::Align2::LEFT_CENTER,
            shown,
            font,
            color,
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flamegraph::{ByLine, SampleRun};

    fn line(file: &str, line: u32, self_n: usize, total_n: usize, code: &str) -> LineStat {
        LineStat {
            file: file.into(),
            line,
            func: "app::work".into(),
            code: code.into(),
            self_n,
            total_n,
        }
    }

    fn result(lines: Vec<LineStat>, idle: usize, outside: usize) -> FlameResult {
        let samples = lines.iter().map(|l| l.self_n).sum::<usize>() + idle + outside;
        FlameResult {
            root: FlameNode {
                name: "all".into(),
                count: samples,
                children: Vec::new(),
            },
            by_line: ByLine {
                lines,
                idle,
                outside,
                outside_fns: vec![("esp_hal::timer::isr".into(), outside)],
                no_line: 0,
                no_line_fns: Vec::new(),
                samples,
            },
            run: Arc::new(SampleRun::default()),
            loaded_from: None,
            elf_changed: false,
            samples,
        }
    }

    /// Every text the table painted, with the rect it was laid out in.
    fn texts(out: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((
                    t.galley.text().to_owned(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut found = Vec::new();
        for s in &out.shapes {
            walk(&s.shape, &mut found);
        }
        found
    }

    fn draw(res: &FlameResult, width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let mut nav = None;
        let mut last = None;
        // A few frames: the scroll area settles its size after the first.
        for pass in 0..3u32 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 500.0),
                )),
                time: Some(f64::from(pass) / 30.0),
                ..Default::default()
            };
            last = Some(crate::headless::run_ui(&ctx, input, |ui| {
                by_line_view(ui, res, &[], &mut nav)
            }));
        }
        last.unwrap()
    }

    fn find<'a>(found: &'a [(String, egui::Rect)], text: &str) -> &'a egui::Rect {
        &found
            .iter()
            .find(|(t, _)| t.trim() == text)
            .unwrap_or_else(|| panic!("{text:?} not painted: {found:?}"))
            .1
    }

    #[test]
    fn the_header_and_the_cells_share_their_columns() {
        let res = result(
            vec![
                line("src/main.rs", 42, 30, 45, "let v = adc.read(&mut pin);"),
                line("src/app/pins.rs", 7, 10, 10, "led.toggle();"),
            ],
            50,
            10,
        );
        let found = texts(&draw(&res, 1200.0));
        let x = |t: &str| find(&found, t).left();
        // The active sort column carries a caret after its name (and the note
        // above the table starts with "Self =").
        let self_head = found
            .iter()
            .find(|(t, _)| t.starts_with("Self ") && !t.contains('='))
            .expect("Self header")
            .1;
        assert!((self_head.left() - x("30.0%")).abs() < 1.0);
        assert!((x("Total") - x("45.0%")).abs() < 1.0);
        assert!((x("Line") - x("main.rs:42")).abs() < 1.0);
        assert!((x("Code") - x("let v = adc.read(&mut pin);")).abs() < 1.0);
        // The fixed rows come first, hottest line next.
        let y = |t: &str| find(&found, t).top();
        assert!(y("Idle") < y("Outside project"));
        assert!(y("Outside project") < y("main.rs:42"));
        assert!(y("main.rs:42") < y("pins.rs:7"));
    }

    #[test]
    fn the_no_line_row_shows_only_when_it_counts() {
        let mut res = result(vec![line("src/main.rs", 42, 30, 45, "x();")], 50, 10);
        let found = texts(&draw(&res, 1200.0));
        assert!(!found.iter().any(|(t, _)| t == "No line"), "{found:?}");
        res.by_line.no_line = 5;
        res.by_line.no_line_fns = vec![("app::work".into(), 5)];
        let found = texts(&draw(&res, 1200.0));
        let y = |t: &str| find(&found, t).top();
        assert!(y("Outside project") < y("No line"));
        assert!(y("No line") < y("main.rs:42"));
        assert!(find(&found, "app::work").left() > find(&found, "No line").left());
    }

    #[test]
    fn no_two_cells_of_a_row_overlap() {
        let long = "x".repeat(400);
        let res = result(
            vec![line(
                "very_long_library_name/src/deeply/nested/module_file.rs",
                12345,
                99999,
                99999,
                &long,
            )],
            0,
            0,
        );
        for width in [1200.0, 420.0] {
            let found = texts(&draw(&res, width));
            // The long row itself: the fixed rows above it share its "±0.0".
            let row = find(&found, "module_file.rs:12345");
            let mut cells: Vec<egui::Rect> = found
                .iter()
                .map(|(_, r)| *r)
                .filter(|r| (r.center().y - row.center().y).abs() < 2.0)
                .collect();
            cells.sort_by(|a, b| a.left().total_cmp(&b.left()));
            assert!(cells.len() >= 4, "{width}: {found:?}");
            for pair in cells.windows(2) {
                assert!(
                    pair[0].right() <= pair[1].left() + 0.5,
                    "{width}: {:?} runs into {:?}",
                    pair[0],
                    pair[1]
                );
            }
            assert!(
                cells.iter().all(|r| r.right() <= width),
                "{width}: {cells:?}"
            );
        }
    }

    #[test]
    fn a_narrow_or_empty_table_still_draws() {
        // No samples at all, and a panel narrower than the fixed columns.
        let empty = result(Vec::new(), 0, 0);
        for width in [1200.0, 150.0, 30.0] {
            draw(&empty, width);
            draw(
                &result(vec![line("src/main.rs", 1, 1, 1, "loop {}")], 1, 0),
                width,
            );
        }
    }
}
