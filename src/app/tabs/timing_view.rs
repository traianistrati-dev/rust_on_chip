//! Profile tab, "Timing" mode: the regions marked in the editor and what the
//! board measured for each - see [`crate::timing`].

use crate::timing::{RegionStat, TimingRegion, TimingRun, TimingState};
use eframe::egui;
use egui_phosphor::regular as ph;
use std::sync::{Arc, Mutex};

/// What the Timing view reads, and the clicks it hands back.
pub struct TimingUi<'a> {
    pub state: &'a Arc<Mutex<TimingState>>,
    /// The regions marked in the editor, in table order.
    pub regions: &'a [TimingRegion],
    /// Why this target cannot be timed (no cycle counter), when it cannot.
    pub unsupported: Option<String>,
    pub go: &'a mut bool,
    pub stop: &'a mut bool,
    /// A row's remove button.
    pub remove: &'a mut Option<usize>,
    pub clear: &'a mut bool,
}

/// A clicked row's band in the editor - the same blue as the By-line table's.
const JUMP_BAND: egui::Color32 = egui::Color32::from_rgba_unmultiplied_const(120, 160, 200, 30);
const BUSY: egui::Color32 = egui::Color32::from_rgb(220, 180, 60);

#[allow(clippy::too_many_arguments)]
pub(super) fn timing_view(
    ui: &mut egui::Ui,
    t: TimingUi<'_>,
    nav: &mut Option<(String, usize, egui::Color32)>,
    chip: &str,
    can_run: bool,
    probe_list: &[crate::probe::ProbeInfo],
    selected_probe: &mut Option<String>,
    probe_scan: &mut bool,
    probe_scan_err: Option<&str>,
    toolchain: &crate::panels::mcu_module::mcu_catalog::ToolchainKind,
    no_probe_rs: bool,
    holder: Option<(&str, &str)>,
) {
    let chip_gap = (!no_probe_rs)
        .then(|| crate::probe::chip_gap(chip))
        .flatten();
    let st = t.state.lock().unwrap().clone();
    let busy = st.is_busy();
    let ready = can_run && !no_probe_rs && chip_gap.is_none() && t.unsupported.is_none();

    ui.horizontal_wrapped(|ui| {
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
        if busy {
            if ui
                .button(
                    egui::RichText::new(format!("{} Stop", ph::STOP))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(230, 130, 120)),
                )
                .on_hover_text(concat!(
                    "Stop reading the board. It keeps running the measuring build ",
                    "until it is flashed again.",
                ))
                .clicked()
            {
                *t.stop = true;
            }
        } else {
            let enabled = ready && !t.regions.is_empty();
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::new(
                        egui::RichText::new(format!("{} Measure", ph::TIMER))
                            .size(10.5)
                            .color(if enabled {
                                egui::Color32::from_rgb(120, 190, 230)
                            } else {
                                egui::Color32::GRAY
                            }),
                    ),
                )
                .on_hover_text(concat!(
                    "Build a copy of the project with a timer around each region, flash it ",
                    "with probe-rs and run it. The numbers are read from the board once a ",
                    "second, until Stop.\nYour files are not changed: only the build copy ",
                    "carries the timers.",
                ))
                .on_disabled_hover_text(if no_probe_rs {
                    super::needs_tool_hint("probe-rs")
                } else if let Some(gap) = &chip_gap {
                    gap.clone()
                } else if let Some(why) = &t.unsupported {
                    why.clone()
                } else if t.regions.is_empty() {
                    concat!(
                        "Mark something to time first: right-click in the editor, then ",
                        "Time this function or Time selected lines.",
                    )
                    .to_owned()
                } else {
                    "No chip config exists yet.".to_owned()
                })
                .clicked()
            {
                *t.go = true;
            }
        }
        if ui
            .add_enabled(
                !busy && !t.regions.is_empty(),
                egui::Button::new(egui::RichText::new(format!("{} Clear", ph::TRASH)).size(10.5)),
            )
            .on_hover_text("Forget every region.")
            .clicked()
        {
            *t.clear = true;
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
                TimingState::Building => {
                    ui.label(egui::RichText::new("building…").size(10.5).color(BUSY));
                    ui.spinner();
                }
                TimingState::Flashing(what) => {
                    ui.add(
                        egui::Label::new(egui::RichText::new(what).size(10.5).color(BUSY))
                            .truncate(),
                    );
                    ui.spinner();
                }
                TimingState::Running(run) => {
                    ui.label(
                        egui::RichText::new(format!("running · {} reads", run.reads))
                            .size(10.5)
                            .color(egui::Color32::from_rgb(120, 190, 120)),
                    );
                    ui.spinner();
                }
                TimingState::Stopped(run) => {
                    ui.label(
                        egui::RichText::new(format!("stopped · {} reads", run.reads))
                            .size(10.5)
                            .color(egui::Color32::GRAY),
                    );
                }
                TimingState::Failed(_) => {
                    ui.label(
                        egui::RichText::new("failed")
                            .size(10.5)
                            .color(egui::Color32::from_rgb(220, 110, 100)),
                    );
                }
                TimingState::Idle => {}
            },
        );
    });
    if busy {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
    ui.separator();

    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            match &st {
                TimingState::Failed(e) => {
                    if !crate::failure_hint::show_card(ui, e, |_| {}) {
                        ui.label(
                            egui::RichText::new(e)
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_rgb(230, 130, 120)),
                        );
                    }
                    ui.add_space(6.0);
                }
                TimingState::Building => note(ui, "Building the measuring copy (--release)…", BUSY),
                TimingState::Flashing(what) => note(ui, &format!("probe-rs: {what}"), BUSY),
                _ => {}
            }
            if t.regions.is_empty() {
                note(
                    ui,
                    concat!(
                        "Nothing to time yet. In the editor, right-click inside a function and ",
                        "pick Time this function, or select a few statements and pick Time ",
                        "selected lines. Measure then builds a copy of the project with a ",
                        "timer around each one, flashes it and shows how long each takes on ",
                        "the board: count, min, average, max and the last run, in CPU cycles ",
                        "and in time.",
                    ),
                    egui::Color32::GRAY,
                );
                return;
            }
            let run = match &st {
                TimingState::Running(r) | TimingState::Stopped(r) => Some(r),
                _ => None,
            };
            if let Some(run) = run {
                run_header(ui, run);
            }
            table(ui, t.regions, t.remove, run, busy, nav);
            if let Some(run) = run {
                ui.add_space(6.0);
                footer(ui, run);
            }
        });
}

fn note(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(text).size(11.0).color(color));
}

/// What the numbers are in, and whether the board has run a region yet.
fn run_header(ui: &mut egui::Ui, run: &TimingRun) {
    if let Some(why) = &run.ended {
        ui.label(
            egui::RichText::new(format!("{} Reading stopped: {why}", ph::WARNING))
                .size(10.5)
                .color(egui::Color32::from_rgb(220, 170, 60)),
        );
    }
    let clock = match run.hz {
        Some(hz) => format!("CPU {} MHz", hz as f64 / 1e6),
        None => "CPU clock unknown: cycles only".to_owned(),
    };
    let lead = if run.started || run.stats.iter().any(|s| s.count > 0) {
        format!("{} · {clock}", run.counter.name())
    } else if run.reads > 0 {
        format!("No region has run on the board yet · {clock}")
    } else {
        format!("Waiting for the first read · {clock}")
    };
    let r = ui.label(
        egui::RichText::new(lead)
            .size(10.5)
            .color(egui::Color32::GRAY),
    );
    if run.hz.is_none() {
        r.on_hover_text(concat!(
            "The Clock tab does not say what this firmware runs at (a hand-written ",
            "clock block, or a chip whose clock is not modelled), so the counts stay ",
            "in CPU cycles.",
        ));
    }
    ui.add_space(4.0);
}

fn footer(ui: &mut egui::Ui, run: &TimingRun) {
    let wrap = run
        .hz
        .map(|hz| {
            format!(
                " A region longer than {} reads wrong: the counter is 32 bits.",
                fmt_time(4_294_967_296.0 / hz as f64)
            )
        })
        .unwrap_or_default();
    let mut text = format!(
        concat!(
            "Each time includes any interrupt that ran inside the region.{} The board keeps ",
            "running the measuring build until it is flashed again.",
        ),
        wrap
    );
    if run.halting {
        text.push_str(concat!(
            " probe-rs could not read the board while it ran, so every read halts it for a ",
            "moment: a region caught in one reads longer (in Max and Last).",
        ));
    } else if run.halted_reads > 0 {
        text.push_str(&format!(
            concat!(
                " {} of the {} reads halted the board for a moment (a region ran while the ",
                "probe read it): a region caught in one reads longer (in Max and Last).",
            ),
            run.halted_reads, run.reads
        ));
    }
    ui.label(
        egui::RichText::new(text)
            .size(10.0)
            .color(egui::Color32::GRAY),
    );
}

/// One row per region marked, with the run's numbers where the run has it.
fn table(
    ui: &mut egui::Ui,
    regions: &[TimingRegion],
    remove: &mut Option<usize>,
    run: Option<&TimingRun>,
    busy: bool,
    nav: &mut Option<(String, usize, egui::Color32)>,
) {
    let hz = run.and_then(|r| r.hz);
    egui::Grid::new("timing_regions")
        .striped(true)
        .num_columns(7)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for h in ["", "Region", "Count", "Min", "Avg", "Max", "Last"] {
                ui.label(
                    egui::RichText::new(h)
                        .size(10.5)
                        .strong()
                        .color(egui::Color32::GRAY),
                );
            }
            ui.end_row();
            for (i, r) in regions.iter().enumerate() {
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(egui::RichText::new(ph::X).size(10.0)).frame(false),
                    )
                    .on_hover_text("Stop timing this region.")
                    .clicked()
                {
                    *remove = Some(i);
                }
                let measured = run.and_then(|run| {
                    let j = run.regions.iter().position(|info| info.region == *r)?;
                    Some((
                        &run.regions[j],
                        run.stats.get(j).copied().unwrap_or_default(),
                    ))
                });
                region_cell(ui, r, measured.map(|(info, _)| info), nav);
                match measured {
                    Some((_, s)) => stat_cells(ui, &s, hz),
                    None => {
                        let hover = if run.is_some() {
                            "Marked after this run was built: Measure again."
                        } else {
                            "Not measured yet."
                        };
                        for _ in 0..5 {
                            ui.label(
                                egui::RichText::new("—")
                                    .size(10.5)
                                    .color(egui::Color32::GRAY),
                            )
                            .on_hover_text(hover);
                        }
                    }
                }
                ui.end_row();
            }
        });
}

fn region_cell(
    ui: &mut egui::Ui,
    r: &TimingRegion,
    info: Option<&crate::timing::RegionInfo>,
    nav: &mut Option<(String, usize, egui::Color32)>,
) {
    let lines = info.map(|i| i.lines).unwrap_or((r.first, r.last));
    let place = if lines.0 == lines.1 {
        format!("{}:{}", r.file, lines.0)
    } else {
        format!("{}:{}-{}", r.file, lines.0, lines.1)
    };
    let what = if r.whole_fn {
        format!("fn {}", r.func)
    } else {
        format!("in {}", r.func)
    };
    ui.horizontal(|ui| {
        let resp = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(format!("{what}  {place}"))
                        .size(10.5)
                        .monospace()
                        .color(egui::Color32::from_rgb(120, 160, 200)),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        let resp = match info.map(|i| i.code.as_str()).filter(|c| !c.is_empty()) {
            Some(code) => resp.on_hover_text(format!("{code}\n\nClick to open it in the editor.")),
            None => resp.on_hover_text("Click to open it in the editor."),
        };
        if resp.clicked() {
            *nav = Some((r.file.clone(), lines.0 as usize, JUMP_BAND));
        }
        if info.is_some_and(|i| i.awaits) {
            ui.label(
                egui::RichText::new(ph::HOURGLASS)
                    .size(10.5)
                    .color(egui::Color32::from_rgb(220, 170, 60)),
            )
            .on_hover_text(concat!(
                "This region awaits: its time is wall time, and includes whatever else ran ",
                "while it waited.",
            ));
        }
    });
}

fn stat_cells(ui: &mut egui::Ui, s: &RegionStat, hz: Option<u64>) {
    ui.label(
        egui::RichText::new(s.count.to_string())
            .size(10.5)
            .monospace(),
    );
    if s.count == 0 {
        for _ in 0..4 {
            ui.label(
                egui::RichText::new("—")
                    .size(10.5)
                    .color(egui::Color32::GRAY),
            )
            .on_hover_text("Not run on the board yet.");
        }
        return;
    }
    for cycles in [
        f64::from(s.min),
        s.avg().unwrap_or(0.0),
        f64::from(s.max),
        f64::from(s.last),
    ] {
        let cyc = format!("{cycles:.0} cycles");
        match hz {
            Some(hz) => {
                ui.label(
                    egui::RichText::new(fmt_time(cycles / hz as f64))
                        .size(10.5)
                        .monospace(),
                )
                .on_hover_text(cyc);
            }
            None => {
                ui.label(egui::RichText::new(cyc).size(10.5).monospace());
            }
        }
    }
}

/// A duration at a precision worth reading.
pub(super) fn fmt_time(secs: f64) -> String {
    if secs >= 1.0 {
        format!("{secs:.3} s")
    } else if secs >= 1e-3 {
        format!("{:.3} ms", secs * 1e3)
    } else if secs >= 1e-6 {
        format!("{:.2} µs", secs * 1e6)
    } else {
        format!("{:.0} ns", secs * 1e9)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_at_their_scale() {
        assert_eq!(fmt_time(2.5), "2.500 s");
        assert_eq!(fmt_time(0.0125), "12.500 ms");
        assert_eq!(fmt_time(1.25e-6), "1.25 µs");
        assert_eq!(fmt_time(4.2e-7), "420 ns");
    }

    fn region(first: u32, whole_fn: bool, func: &str) -> TimingRegion {
        TimingRegion {
            file: "src/main.rs".into(),
            first,
            last: first + 1,
            whole_fn,
            func: func.into(),
            fn_copy: (0, 1),
            anchor: Vec::new(),
            copy: (0, 1),
        }
    }

    /// Every text painted in a few frames of the view.
    fn painted(state: TimingState, regions: &[TimingRegion]) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let state = Arc::new(Mutex::new(state));
        let (mut go, mut stop, mut clear, mut remove) = (false, false, false, None);
        let (mut nav, mut probe, mut scan) = (None, None, false);
        let mut found = Vec::new();
        for pass in 0..3u32 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 500.0),
                )),
                time: Some(f64::from(pass) / 30.0),
                ..Default::default()
            };
            let out = crate::headless::run_ui(&ctx, input, |ui| {
                let t = TimingUi {
                    state: &state,
                    regions,
                    unsupported: None,
                    go: &mut go,
                    stop: &mut stop,
                    remove: &mut remove,
                    clear: &mut clear,
                };
                timing_view(
                    ui,
                    t,
                    &mut nav,
                    "esp32c3",
                    true,
                    &[],
                    &mut probe,
                    &mut scan,
                    None,
                    &crate::panels::mcu_module::mcu_catalog::ToolchainKind::EspRust,
                    // No probe-rs: the view must not ask the installed one
                    // for its chip list from a test.
                    true,
                    None,
                )
            });
            found.clear();
            for s in &out.shapes {
                walk(&s.shape, &mut found);
            }
        }
        found
    }

    #[test]
    fn a_run_fills_the_rows_it_measured_and_only_those() {
        let measured = region(10, true, "fade");
        let later = region(30, false, "main");
        let run = TimingRun {
            regions: vec![crate::timing::RegionInfo {
                region: measured.clone(),
                lines: (10, 14),
                func: "fade".into(),
                awaits: false,
                code: "fn fade(level: u32) -> u32 {".into(),
            }],
            stats: vec![RegionStat {
                count: 4,
                min: 160,
                max: 640,
                last: 320,
                total: 1280,
            }],
            hz: Some(160_000_000),
            counter: crate::timing::Counter::EspPccr,
            reads: 3,
            halting: false,
            halted_reads: 0,
            started: true,
            ended: None,
        };
        let texts = painted(TimingState::Running(run), &[measured, later]);
        let has = |t: &str| texts.iter().any(|s| s.contains(t));
        assert!(has("fn fade  src/main.rs:10-14"), "{texts:?}");
        assert!(has("in main  src/main.rs:30-31"));
        // 160 cycles at 160 MHz, the average 320, the max 640.
        for t in ["1.00 µs", "2.00 µs", "4.00 µs", "running · 3 reads"] {
            assert!(has(t), "{t} not painted: {texts:?}");
        }
        // The row marked after the run started: dashes, not numbers.
        assert!(texts.iter().filter(|s| s.trim() == "—").count() >= 5);
    }

    #[test]
    fn nothing_marked_says_how_to_mark() {
        let texts = painted(TimingState::Idle, &[]);
        assert!(
            texts.iter().any(|s| s.contains("Time this function")),
            "{texts:?}"
        );
    }
}
