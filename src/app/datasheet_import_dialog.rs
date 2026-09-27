//! "Import from datasheet (AI)" sub-dialog of the New MCU form (Phase 1).
//!
//! The user pastes a pin / alternate-function table copied from a datasheet;
//! we hand it to Claude (on a background thread) and patch the result into the
//! open [`McuForm`] for review. Nothing is auto-saved — the extraction only
//! FILLS the form; the human still reviews it and clicks Save.
//!
//! The pure prompt/JSON/patch logic lives in
//! `panels::mcu_module::datasheet_import`; this file is just the egui window and
//! the background-job plumbing.

use std::sync::{Arc, Mutex};

use super::AppIde;
use crate::panels::mcu_module::datasheet_import::{self as ds, Extraction, Source};
use crate::panels::mcu_module::mcu_form::McuForm;
use eframe::egui;
use egui_phosphor::regular as ph;

/// Background extraction job state, shared with the worker thread.
enum ImportJob {
    Running,
    Done(Result<Extraction, String>),
}

/// A worker-thread result slot: `None` while the thread runs, `Some(result)`
/// once it finishes. Three jobs share this shape (clock, package pre-pass,
/// cross-check); spelling it out inline tripped `clippy::type_complexity` at
/// every one of them.
type JobSlot<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// A PDF the user picked (kept in memory until Extract or Remove).
struct PdfPick {
    name: String,
    bytes: Vec<u8>,
}

/// Severity of one [`LogLine`] — drives its icon and colour only.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LogKind {
    /// Something started, or a plain fact worth recording.
    Step,
    /// A step completed successfully.
    Ok,
    /// Completed, but with something the user must look at.
    Warn,
    /// Did not complete.
    Err,
}

impl LogKind {
    fn icon(self) -> &'static str {
        match self {
            LogKind::Step => ph::DOT,
            LogKind::Ok => ph::CHECK_CIRCLE,
            LogKind::Warn => ph::WARNING,
            LogKind::Err => ph::X_CIRCLE,
        }
    }

    fn color(self) -> egui::Color32 {
        match self {
            LogKind::Step => egui::Color32::from_gray(150),
            LogKind::Ok => egui::Color32::from_rgb(120, 200, 120),
            LogKind::Warn => egui::Color32::from_rgb(220, 170, 70),
            LogKind::Err => egui::Color32::from_rgb(230, 110, 90),
        }
    }
}

/// One line of the extraction trace.
///
/// An extraction is three requests on two background threads with an on-disk
/// cache in front of them; when it goes wrong the user previously saw one red
/// sentence and no way to tell WHICH stage produced it, whether the API was
/// even called, or how long it ran. Each line records a single observable
/// transition so the whole run reads top to bottom.
struct LogLine {
    /// Seconds since this run began. Elapsed time is the useful clock here —
    /// "the pin request took 94 s" is actionable, "it happened at 14:03" is not.
    at: f32,
    kind: LogKind,
    text: String,
}

/// A frameless caret toggle that heads a collapsible section, with an optional
/// dimmed summary so collapsing never hides the state itself — e.g. the
/// provider group stays readable as "Anthropic (Claude) · key set" while shut.
fn section_header(
    ui: &mut egui::Ui,
    open: &mut bool,
    label: &str,
    summary: &str,
    hover: &str,
) -> bool {
    let caret = if *open {
        ph::CARET_DOWN
    } else {
        ph::CARET_RIGHT
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if ui
            .add(
                egui::Button::new(
                    egui::RichText::new(format!("{caret} {label}"))
                        .size(11.0)
                        .color(egui::Color32::from_gray(180)),
                )
                .frame(false),
            )
            .on_hover_text(hover)
            .clicked()
        {
            *open = !*open;
        }
        if !summary.is_empty() {
            ui.label(
                egui::RichText::new(summary)
                    .size(10.0)
                    .color(egui::Color32::from_gray(135)),
            );
        }
    });
    *open
}

/// Apply maximize / restore to a dialog window.
///
/// Maximized → the window is pinned to (almost) the whole screen via
/// `fixed_rect`. Restored → the caller's normal size + centre anchor. The only
/// window control is the custom maximize/restore button — no collapse triangle.
///
/// `default_w × default_h` is what the CONTENT gets (see
/// [`window_size`](super::helpers::window_size)); the maximized rect is the
/// whole window, frame and title bar included, so it stays on the screen.
pub(super) fn window_frame(
    ctx: &egui::Context,
    title: impl Into<egui::WidgetText>,
    maximized: bool,
    // Force the window back to `default_w × default_h`, centred, for ONE frame.
    // Needed because egui PERSISTS a window's rect in area memory, so
    // `default_size` alone is ignored once a rect is stored — both after a
    // maximize→restore and, crucially, when REOPENING a dialog that was left
    // maximized (the "opens huge" bug). A one-frame `fixed_rect` overrides the
    // stored rect, then we release to resizable.
    force_default_size: bool,
    default_w: f32,
    default_h: f32,
    anchor_y: f32,
) -> egui::Window<'static> {
    // `collapsible(false)` — no collapse triangle (the user removed it); the
    // custom maximize/restore button is the only window control.
    let win = egui::Window::new(title).collapsible(false).resizable(true);
    let size = super::helpers::window_size::outer_size(ctx, egui::vec2(default_w, default_h));
    if maximized {
        // fixed_rect also fixes position, so don't add an anchor here.
        win.fixed_rect(ctx.content_rect().shrink(12.0))
    } else if force_default_size {
        let screen = ctx.content_rect();
        let centre = screen.center() + egui::vec2(0.0, anchor_y);
        win.fixed_rect(egui::Rect::from_center_size(centre, size))
    } else {
        win.default_width(size.x)
            .default_height(size.y)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, anchor_y])
    }
}

/// A maximize/restore toggle button, right-aligned — put it on the window's
/// first row. Flips `maximized`.
///
/// The row height is ALLOCATED explicitly. A bare
/// `with_layout(right_to_left, …)` at the top of a window grabs the whole
/// remaining HEIGHT as its cross-axis, so the button ended up vertically
/// centred in a huge empty band with all content pushed to the bottom (the
/// reported bug). `allocate_ui_with_layout` with a one-row height pins it.
pub(super) fn maximize_button(ui: &mut egui::Ui, maximized: &mut bool) {
    let row_h = ui.spacing().interact_size.y;
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), row_h),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let (icon, tip) = if *maximized {
                (ph::ARROWS_IN, "Restore window")
            } else {
                (ph::ARROWS_OUT, "Maximize window")
            };
            if ui
                .add(egui::Button::new(egui::RichText::new(icon).size(13.0)).frame(false))
                .on_hover_text(tip)
                .clicked()
            {
                *maximized = !*maximized;
            }
        },
    );
}

/// The collapsible "Prompt" section shared by both AI import dialogs: a
/// READ-ONLY view of the base prompt (so the user sees the contract they must
/// not repeat) plus an editable, persisted supplementary field.
///
/// The base is read-only ON PURPOSE — it carries the JSON shape and the
/// extraction rules; letting the user delete those would silently degrade
/// quality. Additions can only help (or, at worst, confuse — the JSON schema
/// still enforces the shape).
pub(super) fn prompt_section(
    ui: &mut egui::Ui,
    id: &str,
    open: &mut bool,
    base: &str,
    extra: &mut String,
) {
    let caret = if *open {
        ph::CARET_DOWN
    } else {
        ph::CARET_RIGHT
    };
    if ui
        .add(
            egui::Button::new(
                egui::RichText::new(format!("{caret} Prompt"))
                    .size(11.0)
                    .color(egui::Color32::from_gray(170)),
            )
            .frame(false),
        )
        .on_hover_text("View the base prompt and add your own extra instructions")
        .clicked()
    {
        *open = !*open;
    }
    if !*open {
        return;
    }
    // Full window width: the fields sit in a vertical-only `Resize` (drag the
    // bottom edge to grow the HEIGHT), and their WIDTH is pinned to the row's
    // available width. `Resize` can't track width itself — `default_size` is
    // read only on first creation — so the non-resizable x axis follows the
    // inner content, and the content is sized to `w` explicitly.
    let w = ui.available_width();

    ui.label(
        egui::RichText::new("Additional instructions (saved, appended to the base prompt):")
            .size(10.5)
            .color(egui::Color32::from_gray(150)),
    );
    egui::Resize::default()
        .id_salt(format!("{id}_extra_resize"))
        .resizable([false, true]) // height only
        .default_size(egui::vec2(w, 44.0))
        .min_height(28.0)
        .show(ui, |ui| {
            ui.add_sized(
                egui::vec2(w, ui.available_height()),
                egui::TextEdit::multiline(extra)
                    .id_salt(format!("{id}_extra"))
                    .desired_width(w)
                    .hint_text("e.g. prefer the non-SMPS variant · the PLL table is on page 200"),
            );
        });
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new("Base prompt (read-only — carries the required JSON shape and rules):")
            .size(10.0)
            .color(egui::Color32::from_gray(130)),
    );
    egui::Resize::default()
        .id_salt(format!("{id}_base_resize"))
        .resizable([false, true]) // height only
        .default_size(egui::vec2(w, 120.0))
        .min_height(48.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt(format!("{id}_base"))
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Read-only but selectable (the `&str` buffer can't be
                    // edited), so the base prompt stays copyable. Natural
                    // (content) height so the ScrollArea scrolls the full text;
                    // width pinned to the row.
                    let mut base_ref = base;
                    ui.add(
                        egui::TextEdit::multiline(&mut base_ref)
                            .id_salt(format!("{id}_base_view"))
                            .desired_width(w)
                            .font(egui::TextStyle::Monospace),
                    );
                });
        });
}

/// Cache entry labels (newest first), each with its size — for the list.
fn cache_entry_labels() -> Vec<String> {
    ds::cache_entries()
        .into_iter()
        .map(|e| format!("{}  ({})", e.label, human_size(e.bytes)))
        .collect()
}

/// Compact human-readable byte size for the cache row.
fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{} KB", bytes / KB)
    } else {
        format!("{bytes} B")
    }
}

/// Session-only state for the import sub-dialog.
pub(crate) struct DatasheetImport {
    /// Which AI backend to use. Remembered across sessions in the user config
    /// folder, since it also selects which stored key applies.
    provider: ds::Provider,
    api_key: String,
    show_key: bool,
    model: String,
    text: String,
    /// A chosen datasheet PDF — when set, it is extracted instead of the text.
    pdf: Option<PdfPick>,
    /// Copied from the form when opened — biases the model only.
    family_hint: String,
    /// The target package. AUTHORITATIVE (it picks which pin-number column the
    /// model reads), editable here and REQUIRED before extracting; written back
    /// to the form on apply.
    package: String,
    /// Ignore the on-disk cache and force a fresh (billed) API call.
    force_reextract: bool,
    /// Whether the last applied extraction came from the cache (free).
    last_from_cache: bool,
    /// `(entries, bytes)` on disk — refreshed on open, after an extraction and
    /// after clearing, so the row never re-scans the folder per frame.
    cache_stats: (usize, u64),
    /// Human labels of the cached extractions, newest first — shown in a
    /// collapsible list. Refreshed with `cache_stats`.
    cache_entries: Vec<String>,
    cache_list_open: bool,
    cache_note: Option<String>,
    /// Persisted supplementary prompt appended to the base extraction prompt.
    extra_prompt: String,
    /// Whether the collapsible "Prompt" section is expanded.
    prompt_open: bool,
    /// Window maximized (fills the screen) vs normal size.
    maximized: bool,
    /// Previous frame's `maximized`, to detect the restore transition.
    prev_maximized: bool,
    /// `false` until the window has rendered once — the first frame forces the
    /// default size, so a dialog left maximized last time reopens normal.
    shown_once: bool,
    job: Option<Arc<Mutex<ImportJob>>>,
    report: Option<ds::ApplyReport>,
    error: Option<String>,
    key_note: Option<String>,
    /// When set, the same Extract click also runs a CLOCK extraction on the
    /// same source (one click fills pins AND the Clock tab). Default on.
    also_clock: bool,
    /// The parallel clock-extraction worker: `None` while unstarted/finished,
    /// `Some(result)` once the thread is done. A plain `Option<Result>` slot
    /// rather than reusing the standalone dialog's enum keeps the two dialogs
    /// decoupled.
    clock_job: Option<JobSlot<ds::ClockExtraction>>,
    /// Green confirmation / red error from the combined clock extraction.
    clock_note: Option<String>,
    clock_error: Option<String>,
    /// The package-detection pre-pass worker (fills `detected_packages`).
    pkg_job: Option<JobSlot<Vec<String>>>,
    /// Packages found in the datasheet — shown as one-click chips that set
    /// `package`, so the user picks the exact name instead of typing it.
    detected_packages: Vec<String>,
    pkg_error: Option<String>,
    /// Step-by-step trace of the run(s) so far, oldest first. Kept for the whole
    /// dialog session — comparing a failed attempt with the retry that fixed it
    /// is most of its value, so a new Extract appends rather than clears.
    log: Vec<LogLine>,
    log_open: bool,
    /// Start of the current run; every [`LogLine::at`] is measured from it.
    run_started: Option<std::time::Instant>,
    /// Provider + key + where-to-get-one. Collapsed by default: it is configured
    /// once and then never touched, unlike everything below it.
    setup_open: bool,
    /// The paste area. Collapsed by default because the PDF picker is the normal
    /// path — a 159-page datasheet is a file, not something anyone pastes.
    text_open: bool,

    // ── Cross-check (step 5) ────────────────────────────────────────────────
    /// Run a SECOND provider on the same document and diff the two answers.
    /// OFF by default: it doubles the API spend, so it must be a deliberate act.
    verify_enabled: bool,
    /// Which provider gives the second opinion. Never the primary — comparing a
    /// model with itself measures nothing.
    verify_provider: ds::Provider,
    /// That provider's stored key, reloaded when the picker changes rather than
    /// every frame (it is a file read).
    verify_key: String,
    verify_job: Option<JobSlot<Extraction>>,
    /// The two raw extractions, held until BOTH have landed — either thread can
    /// finish first, and the diff needs the pair.
    primary_chip: Option<ds::ExtractedChip>,
    verify_chip: Option<ds::ExtractedChip>,
    consensus: Option<ds::ConsensusReport>,
    verify_error: Option<String>,
}

impl DatasheetImport {
    /// Append one line to the trace, stamped with the elapsed time of the
    /// current run.
    fn log(&mut self, kind: LogKind, text: impl Into<String>) {
        let at = self
            .run_started
            .map(|t| t.elapsed().as_secs_f32())
            .unwrap_or(0.0);
        self.log.push(LogLine {
            at,
            kind,
            text: text.into(),
        });
    }

    /// The whole trace as plain text, for the Copy button. Pasting a run into a
    /// bug report (or to an assistant) beats describing it.
    fn log_as_text(&self) -> String {
        self.log
            .iter()
            .map(|l| format!("[{:>6.1}s] {}", l.at, l.text))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl DatasheetImport {
    fn new(form: &McuForm) -> Self {
        let provider = ds::load_last_provider();
        // Default the second opinion to any provider but the primary — a model
        // compared with itself measures nothing.
        let verify_provider = ds::Provider::ALL
            .into_iter()
            .find(|p| *p != provider)
            .unwrap_or(provider);
        Self {
            provider,
            api_key: ds::load_api_key(provider),
            show_key: false,
            model: provider.default_model().to_string(),
            text: String::new(),
            pdf: None,
            family_hint: form.family.clone(),
            package: form.package.clone(),
            force_reextract: false,
            last_from_cache: false,
            cache_stats: ds::cache_stats(),
            cache_entries: cache_entry_labels(),
            cache_list_open: false,
            cache_note: None,
            extra_prompt: ds::load_extra_prompt(ds::PromptSlot::Pins),
            prompt_open: false,
            maximized: false,
            prev_maximized: false,
            shown_once: false,
            job: None,
            report: None,
            error: None,
            key_note: None,
            also_clock: true,
            clock_job: None,
            clock_note: None,
            clock_error: None,
            pkg_job: None,
            detected_packages: Vec::new(),
            pkg_error: None,
            log: Vec::new(),
            log_open: true,
            run_started: None,
            setup_open: false,
            text_open: false,
            verify_enabled: false,
            verify_provider,
            verify_key: ds::load_api_key(verify_provider),
            verify_job: None,
            primary_chip: None,
            verify_chip: None,
            consensus: None,
            verify_error: None,
        }
    }
}

impl AppIde {
    /// Open the import sub-dialog, seeding hints from the current form.
    pub(crate) fn open_datasheet_import(&mut self, form: &McuForm) {
        self.datasheet_import = Some(DatasheetImport::new(form));
    }

    /// Render the sub-dialog. Applies a finished extraction straight into
    /// `form`, so the caller must hold the form mutably. No-op while closed.
    pub(super) fn show_datasheet_import(&mut self, ui: &egui::Ui, form: &mut McuForm) {
        let Some(mut di) = self.datasheet_import.take() else {
            return;
        };
        let mut keep_open = true;

        // ── Poll the worker ──────────────────────────────────────────────────
        if let Some(job) = &di.job {
            let done = matches!(&*job.lock().unwrap(), ImportJob::Done(_));
            if done {
                let result = {
                    let mut guard = job.lock().unwrap();
                    match std::mem::replace(&mut *guard, ImportJob::Running) {
                        ImportJob::Done(r) => Some(r),
                        ImportJob::Running => None,
                    }
                };
                di.job = None;
                match result {
                    Some(Ok(ex)) => {
                        // The user's package is authoritative (it drove which
                        // column was read) — write it to the form before
                        // applying, so the pin-count cross-check uses it.
                        form.package = di.package.trim().to_string();
                        di.last_from_cache = ex.from_cache;
                        // Applied into a local first: logging reads both the
                        // report and the raw chip, and `di.report` would hold a
                        // borrow of `di` across the `di.log` calls.
                        let rep = ds::apply_to_form(&ex.chip, form);

                        di.log(
                            LogKind::Ok,
                            format!(
                                "Pins: reply received{} — {} pin(s) applied to the form.",
                                if ex.from_cache {
                                    " from cache (no API call, not billed)"
                                } else {
                                    ""
                                },
                                rep.pins_added
                            ),
                        );
                        if rep.pins_added == 0 {
                            di.log(
                                LogKind::Err,
                                "Pins: the model returned an EMPTY pin list. Usually the package \
                                 name does not match the datasheet character-for-character — use \
                                 \"Detect from datasheet\" and pick from the chips.",
                            );
                        }
                        if !rep.patched.is_empty() {
                            di.log(
                                LogKind::Step,
                                format!("Identity filled: {}", rep.patched.join(" · ")),
                            );
                        }
                        // The other half of the answer: what the datasheet did
                        // NOT yield. `apply_to_form` leaves a form field alone
                        // when the extraction is empty, so silence here would
                        // otherwise be indistinguishable from "unchanged".
                        let missing: Vec<&str> = [
                            ("display name", &ex.chip.display_name),
                            ("family", &ex.chip.family),
                            ("CPU", &ex.chip.cpu),
                            ("flash origin", &ex.chip.flash_origin),
                            ("flash size", &ex.chip.flash_size),
                            ("RAM origin", &ex.chip.ram_origin),
                            ("RAM size", &ex.chip.ram_size),
                            ("probe-rs chip", &ex.chip.probe_chip),
                        ]
                        .into_iter()
                        .filter(|(_, v)| v.trim().is_empty())
                        .map(|(k, _)| k)
                        .collect();
                        if !missing.is_empty() {
                            di.log(
                                LogKind::Warn,
                                format!(
                                    "Not found in the datasheet, left as-is in the form: {}.",
                                    missing.join(", ")
                                ),
                            );
                        }
                        for w in &rep.warnings {
                            di.log(LogKind::Warn, format!("Cross-check: {w}"));
                        }
                        if !rep.raw_notes.is_empty() {
                            di.log(
                                LogKind::Warn,
                                format!(
                                    "{} alternate function(s) read but not mapped to a token — \
                                     place them by hand (listed in the report below).",
                                    rep.raw_notes.len()
                                ),
                            );
                        }

                        di.report = Some(rep);
                        di.error = None;
                        // Kept raw for the cross-check, which may still be in
                        // flight — the diff runs on whichever lands second.
                        di.primary_chip = Some(ex.chip.clone());
                        // A fresh extraction just wrote a new cache entry.
                        di.cache_stats = ds::cache_stats();
                        di.cache_entries = cache_entry_labels();
                        di.cache_note = None;
                    }
                    Some(Err(e)) => {
                        di.log(LogKind::Err, format!("Pins FAILED: {e}"));
                        di.error = Some(e);
                        di.report = None;
                    }
                    None => {}
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // ── Poll the parallel clock worker (combined import) ──────────────────
        if let Some(job) = &di.clock_job {
            let done = job.lock().unwrap().is_some();
            if done {
                let result = job.lock().unwrap().take();
                di.clock_job = None;
                match result {
                    Some(Ok(ex)) => {
                        let sysclk =
                            crate::panels::mcu_module::clock::graph::evaluate(&ex.clock.graph)
                                .get("sysclk")
                                .copied()
                                .unwrap_or(0);
                        form.set_imported_clock(ex.clock);
                        di.clock_note = Some(format!(
                            "{} Clock tree imported ({} MHz SYSCLK){} — review it in the Clock tab.",
                            ph::CHECK_CIRCLE,
                            sysclk / 1_000_000,
                            if ex.from_cache { " · from cache" } else { "" }
                        ));
                        di.clock_error = None;
                        di.log(
                            LogKind::Ok,
                            format!(
                                "Clock tree: imported, {} MHz SYSCLK{} — review it in the Clock tab.",
                                sysclk / 1_000_000,
                                if ex.from_cache { " (from cache)" } else { "" }
                            ),
                        );
                    }
                    Some(Err(e)) => {
                        // Amber, not red: the clock is a bonus pass, and the
                        // pins of the same click may well have applied fine.
                        di.log(LogKind::Warn, format!("Clock tree NOT imported: {e}"));
                        di.clock_error = Some(e);
                        di.clock_note = None;
                    }
                    None => {}
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
        }

        // ── Poll the cross-check worker (second provider, same document) ──────
        // Its result is NEVER applied to the form — it exists only to be diffed
        // against the primary, so a disagreement narrows the review instead of
        // silently overwriting good pins with a second guess.
        if let Some(job) = &di.verify_job {
            let done = job.lock().unwrap().is_some();
            if done {
                let result = job.lock().unwrap().take();
                di.verify_job = None;
                match result {
                    Some(Ok(ex)) => {
                        di.log(
                            LogKind::Step,
                            format!(
                                "Cross-check: {} replied{} — {} pin(s), not applied to the form.",
                                di.verify_provider.label(),
                                if ex.from_cache { " from cache" } else { "" },
                                ex.chip.pins.len()
                            ),
                        );
                        di.verify_chip = Some(ex.chip);
                        di.verify_error = None;
                    }
                    Some(Err(e)) => {
                        // Advisory: the primary extraction is unaffected.
                        di.log(LogKind::Warn, format!("Cross-check FAILED: {e}"));
                        di.verify_error = Some(e);
                    }
                    None => {}
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        // Both sides in → diff them. Guarded on `consensus` being empty so this
        // runs once per extraction, not once per frame.
        if di.consensus.is_none()
            && let (Some(a), Some(b)) = (di.primary_chip.as_ref(), di.verify_chip.as_ref())
        {
            let rep =
                ds::compare_extractions(a, b, di.provider.label(), di.verify_provider.label());
            if rep.is_clean() {
                di.log(
                    LogKind::Ok,
                    format!(
                        "Cross-check CLEAN — {} and {} agree on every pin, signal and \
                             identity field. {}",
                        rep.label_a,
                        rep.label_b,
                        rep.headline()
                    ),
                );
            } else {
                di.log(LogKind::Warn, format!("Cross-check: {}", rep.headline()));
                // Name conflicts are the severe kind — two models reading
                // different columns — so each one is named in full.
                for c in &rep.name_conflicts {
                    di.log(
                        LogKind::Warn,
                        format!(
                            "  pin {}: {} says \"{}\", {} says \"{}\"",
                            c.subject, rep.label_a, c.a, rep.label_b, c.b
                        ),
                    );
                }
                if !rep.only_a.is_empty() {
                    di.log(
                        LogKind::Warn,
                        format!(
                            "  only {} returned pin(s): {}",
                            rep.label_a,
                            rep.only_a.join(", ")
                        ),
                    );
                }
                if !rep.only_b.is_empty() {
                    di.log(
                        LogKind::Warn,
                        format!(
                            "  only {} returned pin(s): {}",
                            rep.label_b,
                            rep.only_b.join(", ")
                        ),
                    );
                }
                for c in &rep.identity_conflicts {
                    di.log(
                        LogKind::Warn,
                        format!("  {}: \"{}\" vs \"{}\"", c.subject, c.a, c.b),
                    );
                }
                // Signal diffs are usually many and usually minor; the count
                // goes here, the detail stays in the panel below.
                if !rep.signal_conflicts.is_empty() {
                    di.log(
                        LogKind::Step,
                        format!(
                            "  {} pin(s) differ only in their signal lists — see the \
                                 Cross-check panel.",
                            rep.signal_conflicts.len()
                        ),
                    );
                }
            }
            di.consensus = Some(rep);
        }

        // ── Poll the package-detection pre-pass ───────────────────────────────
        if let Some(job) = &di.pkg_job {
            let done = job.lock().unwrap().is_some();
            if done {
                let result = job.lock().unwrap().take();
                di.pkg_job = None;
                match result {
                    Some(Ok(list)) => {
                        di.log(
                            LogKind::Ok,
                            if list.is_empty() {
                                "Packages: the model found no package names in this document."
                                    .to_string()
                            } else {
                                format!("Packages found ({}): {}", list.len(), list.join(" · "))
                            },
                        );
                        // A single-package datasheet needs no choice — fill it,
                        // unless the user already typed something.
                        if list.len() == 1 && di.package.trim().is_empty() {
                            di.package = list[0].clone();
                            di.log(
                                LogKind::Step,
                                format!("Only one package — selected \"{}\".", di.package),
                            );
                        }
                        di.detected_packages = list;
                        di.pkg_error = None;
                    }
                    Some(Err(e)) => {
                        di.log(LogKind::Err, format!("Package detection FAILED: {e}"));
                        di.pkg_error = Some(e);
                        di.detected_packages.clear();
                    }
                    None => {}
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(120));
            }
        }
        let running = di.job.is_some()
            || di.clock_job.is_some()
            || di.pkg_job.is_some()
            || di.verify_job.is_some();

        let mut do_extract = false;
        let mut do_save_key = false;
        let mut dismiss_report = false;
        let mut do_clear_cache = false;
        let mut do_detect = false;
        let mut clear_log = false;

        let force_default = !di.shown_once || (di.prev_maximized && !di.maximized);
        di.prev_maximized = di.maximized;
        di.shown_once = true;
        window_frame(
            ui.ctx(),
            format!("{} Import from datasheet (AI)", ph::SPARKLE),
            di.maximized,
            force_default,
            560.0,
            540.0,
            30.0,
        )
        .show(ui.ctx(), |ui| {
            maximize_button(ui, &mut di.maximized);
            ui.label(
                egui::RichText::new(
                    "Paste a pin / alternate-function table from the datasheet. \
                         The AI fills the form for you to review — nothing is saved \
                         automatically.",
                )
                .size(11.0)
                .color(egui::Color32::from_gray(160)),
            );
            ui.add_space(6.0);

            // ── Provider + key (collapsed: set once, then never touched) ──
            // The summary keeps the state legible while shut, so a disabled
            // Extract button is never a mystery hidden behind a caret.
            let setup_summary = format!(
                "{} · {}",
                di.provider.label(),
                if di.api_key.trim().is_empty() {
                    "no key"
                } else {
                    "key set"
                }
            );
            let setup_open = section_header(
                ui,
                &mut di.setup_open,
                "AI provider & API key",
                &setup_summary,
                "Provider, API key and where to get one. Collapsed by default — \
                 these are configured once.",
            );
            if setup_open {
                // Switching providers swaps in that provider's own key and
                // default model — carrying either across would just produce an
                // auth failure with a confusing message.
                ui.horizontal(|ui| {
                    ui.label("AI provider:");
                    let before = di.provider;
                    egui::ComboBox::from_id_salt("ds_provider")
                        .selected_text(di.provider.label())
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for p in ds::Provider::ALL {
                                ui.selectable_value(&mut di.provider, p, p.label());
                            }
                        });
                    if di.provider != before {
                        di.api_key = ds::load_api_key(di.provider);
                        di.model = di.provider.default_model().to_string();
                        di.key_note = None;
                        ds::save_last_provider(di.provider);
                    }
                    ui.label(
                        egui::RichText::new("all three read the PDF natively")
                            .size(10.0)
                            .color(egui::Color32::from_gray(140))
                            .italics(),
                    )
                    .on_hover_text(
                        "Only providers that accept a PDF directly are offered. A pin \
                         table is a 2D layout — backends that can only be fed text \
                         extracted locally scramble the columns and return confident, \
                         wrong pinouts.",
                    );
                });

                // ── API key ──────────────────────────────────────────────────
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(format!("{} API key:", di.provider.label()));
                    ui.add(
                        egui::TextEdit::singleline(&mut di.api_key)
                            .password(!di.show_key)
                            .desired_width(260.0),
                    );
                    // Strip whitespace, never case: an API key is case-sensitive, so
                    // folding letters here would invalidate it — and `save_api_key`
                    // would persist the broken one. Whitespace is the actual paste
                    // hazard: copying a key out of a provider console picks up a
                    // trailing newline, and a web page can hand over a NO-BREAK
                    // SPACE, which `is_whitespace` catches and `is_ascii_whitespace`
                    // would not. With `password(true)` the user cannot see any of it.
                    di.api_key.retain(|c| !c.is_whitespace());
                    ui.checkbox(&mut di.show_key, "show");
                    if ui
                        .button("Save key")
                        .on_hover_text("Store in the user config folder (never in the project)")
                        .clicked()
                    {
                        do_save_key = true;
                    }
                });
                ui.label(
                    egui::RichText::new(format!(
                        "Stored per provider in your user config folder; the {} \
                         environment variable overrides it. Never written to the project.",
                        di.provider.env_var()
                    ))
                    .size(10.0)
                    .color(egui::Color32::from_gray(140))
                    .italics(),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    ui.label(
                        egui::RichText::new(format!("{} No key? Create one at", ph::KEY))
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                    ui.hyperlink_to(
                        egui::RichText::new(di.provider.console_url()).size(10.0),
                        di.provider.console_url(),
                    )
                    .on_hover_text(
                        "Opens the provider's console. Sign in, create a key, copy it \
                         (usually shown once) and paste it above.",
                    );
                    ui.label(
                        egui::RichText::new("(needs billing set up on the account).")
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                });
                if let Some(note) = &di.key_note {
                    ui.label(egui::RichText::new(note).size(10.5));
                }
            } // end of the collapsible provider/key group

            // ── Model ────────────────────────────────────────────────────
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Model:");
                ui.add(egui::TextEdit::singleline(&mut di.model).desired_width(200.0))
                    .on_hover_text(di.provider.model_hint());
                // Model ids are lowercase at every provider, and a stray capital
                // is a 404 the user has to diagnose from an API error. Fold it
                // away as they type. `make_ascii_lowercase` and not
                // `to_lowercase`: it rewrites in place and never changes the
                // byte length, so the text cursor cannot jump mid-word.
                di.model.make_ascii_lowercase();
                if !di.family_hint.trim().is_empty() {
                    ui.label(
                        egui::RichText::new(format!("family: {}", di.family_hint.trim()))
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                }
            });

            // ── Package (REQUIRED — picks the pin-number column) ─────────
            ui.horizontal(|ui| {
                ui.label("Package *:");
                ui.add(
                    egui::TextEdit::singleline(&mut di.package)
                        .desired_width(140.0)
                        .hint_text("UFQFPN48"),
                )
                .on_hover_text(
                    "Must match a package name in the datasheet EXACTLY.\n\
                         The datasheet lists one column / pinout figure per package \
                         (UFQFPN32, WLCSP41, UFQFPN48, UFQFPN48 SMPS, UFBGA59…) — \
                         this tells the model which one to read.\n\n\
                         Variants matter: \"UFQFPN48\" and \"UFQFPN48 SMPS\" are \
                         DIFFERENT pinouts (both figures just say \"UFQFPN48\" inside \
                         the chip outline). Type the full variant name, e.g. copy it \
                         from the figure title.",
                );
                if di.package.trim().is_empty() {
                    ui.label(
                        egui::RichText::new("required — without it the wrong column gets read")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(230, 170, 70)),
                    );
                }
            });

            // ── Detect packages (pre-pass) ───────────────────────────────
            // A cheap AI call lists the datasheet's package names so the user
            // PICKS the exact one (character-for-character matters) instead of
            // typing it — the historical #1 failure mode.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                let has_source = di.pdf.is_some() || !di.text.trim().is_empty();
                let can_detect = !running && !di.api_key.trim().is_empty() && has_source;
                if ui
                    .add_enabled(
                        can_detect,
                        egui::Button::new(
                            egui::RichText::new(format!(
                                "{} Detect from datasheet",
                                ph::MAGNIFYING_GLASS
                            ))
                            .size(10.5),
                        )
                        .small(),
                    )
                    .on_hover_text(
                        "Ask the AI which packages this datasheet describes, then pick one \
                         below — no need to type the exact name. Cached, so re-detecting the \
                         same document is free.",
                    )
                    .on_disabled_hover_text("needs an API key and a PDF or pasted text")
                    .clicked()
                {
                    do_detect = true;
                }
                if di.pkg_job.is_some() {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("scanning…")
                            .size(10.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                }
                for name in &di.detected_packages {
                    let selected = di.package.trim() == name.trim();
                    if ui
                        .selectable_label(
                            selected,
                            egui::RichText::new(name).size(10.5).monospace(),
                        )
                        .on_hover_text("Use this package")
                        .clicked()
                    {
                        di.package = name.clone();
                    }
                }
            });
            if let Some(err) = &di.pkg_error {
                ui.label(
                    egui::RichText::new(format!("{} {err}", ph::WARNING))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(220, 170, 70)),
                );
            }

            // ── Prompt (base read-only + supplementary) ──────────────────
            let base = ds::build_prompt(di.family_hint.trim(), di.package.trim());
            prompt_section(
                ui,
                "ds_pin_prompt",
                &mut di.prompt_open,
                &base,
                &mut di.extra_prompt,
            );

            // ── Cache stats + clear ──────────────────────────────────────
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let (n, bytes) = di.cache_stats;
                ui.label(
                    egui::RichText::new(format!(
                        "Cache: {n} extraction(s) · {}",
                        human_size(bytes)
                    ))
                    .size(10.0)
                    .color(egui::Color32::from_gray(140)),
                );
                if ui
                    .add_enabled(
                        n > 0,
                        egui::Button::new(egui::RichText::new("Clear").size(10.0)).small(),
                    )
                    .on_hover_text(
                        "Delete every cached extraction. Re-importing the same datasheet \
                             will call (and bill) the API again.",
                    )
                    .on_disabled_hover_text("nothing cached yet")
                    .clicked()
                {
                    do_clear_cache = true;
                }
                if let Some(note) = &di.cache_note {
                    ui.label(
                        egui::RichText::new(note)
                            .size(10.0)
                            .color(egui::Color32::from_gray(150)),
                    );
                }
            });
            // ── Cached-file names (collapsible) ──────────────────────────
            // The reply files are hash-named, so this lists the human label
            // (chip · package · provider/model · source) written beside
            // each one, newest first.
            if !di.cache_entries.is_empty() {
                let caret = if di.cache_list_open {
                    ph::CARET_DOWN
                } else {
                    ph::CARET_RIGHT
                };
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new(format!(
                                "{caret} cached datasheets ({})",
                                di.cache_entries.len()
                            ))
                            .size(10.0)
                            .color(egui::Color32::from_gray(150)),
                        )
                        .frame(false),
                    )
                    .clicked()
                {
                    di.cache_list_open = !di.cache_list_open;
                }
                if di.cache_list_open {
                    egui::ScrollArea::vertical()
                        .id_salt("ds_cache_list")
                        .max_height(120.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for label in &di.cache_entries {
                                ui.label(
                                    egui::RichText::new(format!("• {label}"))
                                        .size(10.0)
                                        .color(egui::Color32::from_gray(165)),
                                );
                            }
                        });
                }
            }

            // ── PDF picker ───────────────────────────────────────────────
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{} Choose PDF…", ph::FILE_PDF))
                    .on_hover_text("Send a datasheet PDF instead of pasted text")
                    .clicked()
                {
                    if let Some(path) = rfd::FileDialog::new()
                        .add_filter("PDF", &["pdf"])
                        .pick_file()
                    {
                        match std::fs::read(&path) {
                            Ok(bytes) => {
                                let name = path
                                    .file_name()
                                    .map(|n| n.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "datasheet.pdf".into());
                                di.pdf = Some(PdfPick { name, bytes });
                                di.error = None;
                            }
                            Err(e) => di.error = Some(format!("Could not read PDF: {e}")),
                        }
                    }
                }
                if let Some(pdf) = &di.pdf {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} {} ({:.1} MB) — used instead of the text",
                            ph::CHECK_CIRCLE,
                            pdf.name,
                            pdf.bytes.len() as f64 / (1024.0 * 1024.0),
                        ))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(120, 190, 120)),
                    );
                    if ui.button("Remove").clicked() {
                        di.pdf = None;
                    }
                } else {
                    ui.label(
                        egui::RichText::new("or paste text below")
                            .size(10.0)
                            .color(egui::Color32::from_gray(140)),
                    );
                }
            });

            // ── Paste area (collapsed: the PDF picker is the normal path) ─
            ui.add_space(4.0);
            let paste_enabled = di.pdf.is_none();
            // Summary so a collapsed field never hides content the user pasted
            // — and states plainly when a PDF has made it irrelevant.
            let text_summary = if !paste_enabled {
                "ignored — a PDF is selected".to_string()
            } else if di.text.trim().is_empty() {
                "empty".to_string()
            } else {
                format!("{} chars pasted", di.text.trim().len())
            };
            if section_header(
                ui,
                &mut di.text_open,
                "Datasheet text",
                &text_summary,
                "Paste a pin table here instead of picking a PDF. Collapsed by default \
                 — most datasheets are sent as a PDF.",
            ) {
                egui::ScrollArea::vertical()
                    .id_salt("ds_paste")
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.add_enabled(
                            paste_enabled,
                            egui::TextEdit::multiline(&mut di.text)
                                .desired_rows(8)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("Paste the pin table / alternate-function list here…"),
                        );
                    });
            }

            // ── Actions ──────────────────────────────────────────────────
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let has_input = di.pdf.is_some() || !di.text.trim().is_empty();
                let can_extract = !running
                    && has_input
                    && !di.api_key.trim().is_empty()
                    && !di.package.trim().is_empty();
                if ui
                    .add_enabled(
                        can_extract,
                        egui::Button::new(
                            egui::RichText::new(format!("{} Extract", ph::MAGIC_WAND)).color(
                                if can_extract {
                                    egui::Color32::from_rgb(150, 200, 255)
                                } else {
                                    egui::Color32::GRAY
                                },
                            ),
                        ),
                    )
                    .on_hover_text("Send the text to Claude and fill the form")
                    .on_disabled_hover_text(
                        "needs an API key, a Package, and either pasted text or a PDF",
                    )
                    .clicked()
                {
                    do_extract = true;
                }
                if running {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("extracting…")
                            .size(11.0)
                            .color(egui::Color32::from_gray(160)),
                    );
                }
                ui.checkbox(&mut di.also_clock, "+ clock tree")
                    .on_hover_text(
                        "Also run a clock-tree extraction on the SAME source, filling the \
                             Clock tab in the same click.\nBest with a full-datasheet PDF — a \
                             pins-only paste may not contain the clock section (the clock \
                             extraction then just reports it couldn't verify a SYSCLK).",
                    );
                ui.checkbox(&mut di.force_reextract, "re-extract")
                    .on_hover_text(
                        "Off: an identical document + package + model reuses the cached \
                             result — instant and free.\nOn: force a fresh (billed) API call, \
                             e.g. after editing the pasted text.",
                    );
                if ui.button("Close").clicked() {
                    keep_open = false;
                }
            });

            // ── Cross-check with a second provider ───────────────────────
            // Two models reading the same table rarely invent the SAME wrong
            // answer, so the pins they agree on are near-certainly right and
            // only the disputed handful needs a human. Off by default: it is a
            // second billed extraction.
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.checkbox(&mut di.verify_enabled, "cross-check with")
                    .on_hover_text(
                        "Run a SECOND provider on the same document and diff the two \
                         answers. Its result is never applied — it only tells you which \
                         pins to check.\nCosts a second extraction.",
                    );
                let before = di.verify_provider;
                egui::ComboBox::from_id_salt("ds_verify_provider")
                    .selected_text(di.verify_provider.label())
                    .width(150.0)
                    .show_ui(ui, |ui| {
                        // The primary is excluded: comparing a model with itself
                        // measures nothing.
                        for p in ds::Provider::ALL.into_iter().filter(|p| *p != di.provider) {
                            ui.selectable_value(&mut di.verify_provider, p, p.label());
                        }
                    });
                if di.verify_provider != before {
                    di.verify_key = ds::load_api_key(di.verify_provider);
                }
                // Switching the PRIMARY can leave the picker pointing at it.
                if di.verify_provider == di.provider
                    && let Some(p) = ds::Provider::ALL.into_iter().find(|p| *p != di.provider)
                {
                    di.verify_provider = p;
                    di.verify_key = ds::load_api_key(p);
                }
                if di.verify_enabled && di.verify_key.trim().is_empty() {
                    ui.label(
                        egui::RichText::new(format!(
                            "no {} key stored — cross-check will be skipped",
                            di.verify_provider.label()
                        ))
                        .size(10.0)
                        .color(egui::Color32::from_rgb(230, 170, 70)),
                    );
                }
            });

            // ── Log ──────────────────────────────────────────────────────
            // An extraction is up to three requests across two threads with a
            // cache in front; a single red sentence at the end could not say
            // which stage produced it, whether the API was reached, or how long
            // it ran. This is that missing narration — and its Copy button is
            // what makes a failed run reportable.
            if !di.log.is_empty() {
                ui.add_space(6.0);
                let log_summary = if running {
                    "running…".to_string()
                } else {
                    format!("{} step(s)", di.log.len())
                };
                if section_header(
                    ui,
                    &mut di.log_open,
                    "Log",
                    &log_summary,
                    "Step-by-step trace of this extraction: what was sent, what came \
                     back, what could not be extracted.",
                ) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        if ui
                            .add(egui::Button::new(egui::RichText::new("Copy").size(10.0)).small())
                            .on_hover_text("Copy the whole log as plain text")
                            .clicked()
                        {
                            ui.ctx().copy_text(di.log_as_text());
                        }
                        if ui
                            .add(egui::Button::new(egui::RichText::new("Clear").size(10.0)).small())
                            .on_hover_text("Discard the trace so far")
                            .clicked()
                        {
                            clear_log = true;
                        }
                        if running {
                            ui.spinner();
                        }
                    });
                    egui::ScrollArea::vertical()
                        .id_salt("ds_log")
                        .max_height(170.0)
                        .auto_shrink([false, true])
                        // Pinned to the bottom so a running extraction keeps the
                        // newest line in view without the user scrolling.
                        .stick_to_bottom(true)
                        .show(ui, |ui| {
                            for line in &di.log {
                                ui.horizontal_wrapped(|ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    ui.label(
                                        egui::RichText::new(format!("{:>5.1}s", line.at))
                                            .size(9.5)
                                            .monospace()
                                            .color(egui::Color32::from_gray(110)),
                                    );
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} {}",
                                            line.kind.icon(),
                                            line.text
                                        ))
                                        .size(10.5)
                                        .color(line.kind.color()),
                                    );
                                });
                            }
                        });
                }
            }

            if let Some(err) = &di.error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{} {err}", ph::X_CIRCLE))
                        .size(11.0)
                        .color(egui::Color32::from_rgb(230, 110, 90)),
                );
            }

            // ── Combined clock-extraction outcome ─────────────────────────
            // Advisory: a clock failure is amber (non-fatal — the pins still
            // applied), a success is a green confirmation.
            if let Some(note) = &di.clock_note {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(note)
                        .size(11.0)
                        .color(egui::Color32::from_rgb(120, 210, 120)),
                );
            }
            if let Some(err) = &di.clock_error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{} Clock not imported: {err}", ph::WARNING))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(220, 170, 70)),
                );
            }

            // ── Review report ────────────────────────────────────────────
            if let Some(rep) = &di.report {
                ui.add_space(6.0);
                ui.separator();
                ui.label(
                    egui::RichText::new(format!(
                        "{} Applied{} — {} pin(s). Review the form and fix \
                             anything flagged before Save.",
                        ph::CHECK_CIRCLE,
                        if di.last_from_cache {
                            " from cache (no API call)"
                        } else {
                            ""
                        },
                        rep.pins_added
                    ))
                    .size(11.5)
                    .strong()
                    .color(egui::Color32::from_rgb(120, 210, 120)),
                );
                egui::ScrollArea::vertical()
                    .id_salt("ds_report")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        if !rep.patched.is_empty() {
                            ui.label(
                                egui::RichText::new(format!("Filled: {}", rep.patched.join(" · ")))
                                    .size(10.5)
                                    .color(egui::Color32::from_gray(170)),
                            );
                        }
                        for w in &rep.warnings {
                            ui.label(
                                egui::RichText::new(format!("{} {w}", ph::WARNING))
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(220, 170, 70)),
                            );
                        }
                        if !rep.raw_notes.is_empty() {
                            ui.add_space(2.0);
                            ui.label(
                                egui::RichText::new(
                                    "Unmapped alternate functions (kept for you to place):",
                                )
                                .size(10.5)
                                .italics()
                                .color(egui::Color32::from_gray(150)),
                            );
                            for n in &rep.raw_notes {
                                ui.label(
                                    egui::RichText::new(format!("{} {n}", ph::DOT))
                                        .size(10.0)
                                        .color(egui::Color32::from_gray(160)),
                                );
                            }
                        }
                    });
                if ui.button("Dismiss report").clicked() {
                    dismiss_report = true;
                }
            }

            // ── Cross-check result ───────────────────────────────────────
            // The whole point is the SHORTLIST: name conflicts first (a pin the
            // two providers disagree about is the one worth opening the
            // datasheet for), then the merely-different signal lists.
            if let Some(err) = &di.verify_error {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!("{} Cross-check not run: {err}", ph::WARNING))
                        .size(10.5)
                        .color(egui::Color32::from_rgb(220, 170, 70)),
                );
            }
            if let Some(c) = &di.consensus {
                ui.add_space(6.0);
                ui.separator();
                let clean = c.is_clean();
                ui.label(
                    egui::RichText::new(format!(
                        "{} Cross-check {} vs {} — {}",
                        if clean { ph::CHECK_CIRCLE } else { ph::WARNING },
                        c.label_a,
                        c.label_b,
                        c.headline()
                    ))
                    .size(11.5)
                    .strong()
                    .color(if clean {
                        egui::Color32::from_rgb(120, 210, 120)
                    } else {
                        egui::Color32::from_rgb(230, 180, 80)
                    }),
                );
                if clean {
                    ui.label(
                        egui::RichText::new(
                            "Two providers read this document independently and matched. \
                             That is the strongest signal available short of the datasheet \
                             itself.",
                        )
                        .size(10.0)
                        .italics()
                        .color(egui::Color32::from_gray(150)),
                    );
                } else {
                    ui.label(
                        egui::RichText::new(
                            "Only the rows below are in doubt — everything else matched. \
                             The second provider's answer was NOT applied.",
                        )
                        .size(10.0)
                        .italics()
                        .color(egui::Color32::from_gray(150)),
                    );
                    egui::ScrollArea::vertical()
                        .id_salt("ds_consensus")
                        .max_height(160.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for cf in &c.name_conflicts {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} pin {}: \"{}\" vs \"{}\"  — check this one",
                                        ph::WARNING,
                                        cf.subject,
                                        cf.a,
                                        cf.b
                                    ))
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(230, 150, 90)),
                                );
                            }
                            for cf in &c.identity_conflicts {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} {}: \"{}\" vs \"{}\"",
                                        ph::WARNING,
                                        cf.subject,
                                        cf.a,
                                        cf.b
                                    ))
                                    .size(10.5)
                                    .color(egui::Color32::from_rgb(220, 170, 70)),
                                );
                            }
                            for (label, list) in [(&c.label_a, &c.only_a), (&c.label_b, &c.only_b)]
                            {
                                if !list.is_empty() {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} only {label} returned pin(s): {}",
                                            ph::DOT,
                                            list.join(", ")
                                        ))
                                        .size(10.5)
                                        .color(egui::Color32::from_rgb(220, 170, 70)),
                                    );
                                }
                            }
                            if !c.signal_conflicts.is_empty() {
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Same pin, different alternate functions ({} — \
                                         {} extra | {} extra):",
                                        c.signal_conflicts.len(),
                                        c.label_a,
                                        c.label_b
                                    ))
                                    .size(10.0)
                                    .italics()
                                    .color(egui::Color32::from_gray(150)),
                                );
                                for cf in &c.signal_conflicts {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "{} pin {}:  {}  |  {}",
                                            ph::DOT,
                                            cf.subject,
                                            cf.a,
                                            cf.b
                                        ))
                                        .size(10.0)
                                        .color(egui::Color32::from_gray(165)),
                                    );
                                }
                            }
                        });
                }
            }
        });

        // ── Deferred side effects ────────────────────────────────────────────
        if do_save_key {
            di.key_note = Some(match ds::save_api_key(di.provider, &di.api_key) {
                Ok(()) => format!("{} Saved to the config folder.", ph::CHECK_CIRCLE),
                Err(e) => format!("{} Could not save key: {e}", ph::X_CIRCLE),
            });
        }
        if dismiss_report {
            di.report = None;
        }
        if clear_log {
            di.log.clear();
            di.run_started = None;
        }
        if do_clear_cache {
            di.cache_note = Some(match ds::clear_cache() {
                Ok(n) => format!("{} cleared {n}", ph::CHECK_CIRCLE),
                Err(e) => format!("{} {e}", ph::X_CIRCLE),
            });
            di.cache_stats = ds::cache_stats();
            di.cache_entries = cache_entry_labels();
        }
        if do_detect && di.pkg_job.is_none() {
            let shared: JobSlot<Vec<String>> = Arc::new(Mutex::new(None));
            di.pkg_job = Some(shared.clone());
            di.pkg_error = None;
            // The pre-pass can also be the FIRST thing a user clicks, so it
            // starts the clock when no extraction has run yet.
            if di.run_started.is_none() {
                di.run_started = Some(std::time::Instant::now());
            }
            di.log(
                LogKind::Step,
                "Detecting the packages this datasheet describes…",
            );
            let (provider, key, model) = (di.provider, di.api_key.clone(), di.model.clone());
            let source = match &di.pdf {
                Some(pdf) => Source::Pdf(pdf.bytes.clone()),
                None => Source::Text(di.text.clone()),
            };
            // Detection reuses the cache; a manual re-detect is rare enough that
            // it isn't worth its own force flag — Clear-cache covers it.
            let use_cache = !di.force_reextract;
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let res = ds::call_ai_packages(provider, &key, &model, &source, use_cache)
                    .map(|l| l.packages);
                *shared.lock().unwrap() = Some(res);
                ctx.request_repaint();
            });
            ui.ctx().request_repaint();
        }
        if do_extract && di.job.is_none() {
            let shared = Arc::new(Mutex::new(ImportJob::Running));
            di.job = Some(shared.clone());
            di.error = None;
            di.report = None;
            di.clock_note = None;
            di.clock_error = None;
            // A new run invalidates the previous cross-check entirely.
            di.primary_chip = None;
            di.verify_chip = None;
            di.consensus = None;
            di.verify_error = None;

            // Restart the clock BEFORE the first line, so the run reads from 0s.
            di.run_started = Some(std::time::Instant::now());
            if !di.log.is_empty() {
                di.log.push(LogLine {
                    at: 0.0,
                    kind: LogKind::Step,
                    text: "─────────────────────────────".into(),
                });
            }
            // Record exactly WHAT was sent. When a run misbehaves the first
            // question is always which document, which package and whether the
            // API was even reached — none of which was recoverable afterwards.
            let src_desc = match &di.pdf {
                Some(pdf) => format!(
                    "PDF \"{}\" ({:.1} MB)",
                    pdf.name,
                    pdf.bytes.len() as f64 / (1024.0 * 1024.0)
                ),
                None => format!("pasted text ({} chars)", di.text.trim().len()),
            };
            let (provider, model, package) = (
                di.provider.label(),
                di.model.clone(),
                di.package.trim().to_string(),
            );
            di.log(
                LogKind::Step,
                format!("Extract started · {provider} · {model} · package \"{package}\""),
            );
            di.log(LogKind::Step, format!("Source: {src_desc}"));
            if di.force_reextract {
                di.log(
                    LogKind::Step,
                    "Cache bypassed (re-extract on) — this call is billed.",
                );
            } else {
                di.log(
                    LogKind::Step,
                    "Cache enabled — an identical document + package + model replies instantly.",
                );
            }
            if di.also_clock {
                di.log(
                    LogKind::Step,
                    "Clock tree requested too — a second, independent request.",
                );
            }
            // Persist the supplementary prompt so it survives across sessions.
            ds::save_extra_prompt(ds::PromptSlot::Pins, &di.extra_prompt);
            let (provider, key, model, family, package, extra) = (
                di.provider,
                di.api_key.clone(),
                di.model.clone(),
                di.family_hint.clone(),
                di.package.trim().to_string(),
                di.extra_prompt.clone(),
            );
            let source = match &di.pdf {
                Some(pdf) => Source::Pdf(pdf.bytes.clone()),
                None => Source::Text(di.text.clone()),
            };
            let use_cache = !di.force_reextract;
            let ctx = ui.ctx().clone();

            // Combined: fire a parallel CLOCK extraction on the SAME source, so
            // one click fills both the pinout and the Clock tab. It uses the
            // clock prompt slot's own supplementary text (loaded fresh, since
            // this dialog only edits the pins slot) and the clock schema +
            // numeric self-check — kept a SEPARATE request so a clock failure
            // never discards good pins, and vice versa.
            if di.also_clock {
                let clk_shared: JobSlot<ds::ClockExtraction> = Arc::new(Mutex::new(None));
                di.clock_job = Some(clk_shared.clone());
                let clock_extra = ds::load_extra_prompt(ds::PromptSlot::Clock);
                let (cp, ck, cm) = (provider, key.clone(), model.clone());
                let csource = source.clone();
                let cctx = ctx.clone();
                std::thread::spawn(move || {
                    let res = ds::call_ai_clock(cp, &ck, &cm, &clock_extra, &csource, use_cache);
                    *clk_shared.lock().unwrap() = Some(res);
                    cctx.request_repaint();
                });
            }

            // Cross-check: the SAME document, package and supplementary prompt
            // through a different provider (and its own default model, since
            // `di.model` names a model of the primary backend). Independent
            // thread — a failed second opinion must not cost the primary run.
            if di.verify_enabled && !di.verify_key.trim().is_empty() {
                let vp = di.verify_provider;
                let vmodel = vp.default_model().to_string();
                di.log(
                    LogKind::Step,
                    format!("Cross-check requested · {} · {vmodel}", vp.label()),
                );
                let v_shared: JobSlot<Extraction> = Arc::new(Mutex::new(None));
                di.verify_job = Some(v_shared.clone());
                let (vkey, vfamily, vpackage, vextra) = (
                    di.verify_key.clone(),
                    family.clone(),
                    package.clone(),
                    extra.clone(),
                );
                let vsource = source.clone();
                let vctx = ctx.clone();
                std::thread::spawn(move || {
                    let res = ds::call_ai(
                        vp, &vkey, &vmodel, &vfamily, &vpackage, &vextra, &vsource, use_cache,
                    );
                    *v_shared.lock().unwrap() = Some(res);
                    vctx.request_repaint();
                });
            } else if di.verify_enabled {
                di.log(
                    LogKind::Warn,
                    format!(
                        "Cross-check SKIPPED — no API key stored for {}.",
                        di.verify_provider.label()
                    ),
                );
            }

            std::thread::spawn(move || {
                let res = ds::call_ai(
                    provider, &key, &model, &family, &package, &extra, &source, use_cache,
                );
                *shared.lock().unwrap() = ImportJob::Done(res);
                ctx.request_repaint();
            });
            // Kick one repaint so the poll's `request_repaint_after` cycle (and
            // the spinner) starts on the next frame, not on the next input.
            ui.ctx().request_repaint();
        }

        if keep_open {
            self.datasheet_import = Some(di);
        }
    }
}
