//! The **Filters** twisty in the chip picker.
//!
//! The search field answers "which part did you mean"; this answers "which part
//! would do". They are drawn together because they are the same question asked
//! from opposite ends, and because a filter is what makes an EMPTY query
//! meaningful — see `Catalogue::search`.
//!
//! # The count on the header is not decoration
//!
//! The twisty is collapsed by default, so a filter left on from earlier is
//! invisible. Without the badge, the next search for a part that plainly exists
//! answers *"No chip matches that"* with nothing on screen to explain why. The
//! count, and the Clear beside it, are the whole reason a collapsed filter is
//! safe to offer.
//!
//! # Log sliders
//!
//! Flash spans 0 KB to 4 MB and RAM 2 KB to 4.2 MB. On a linear slider the half
//! of the travel above 2 MB covers a handful of parts and the F1/G0 end — where
//! most of the catalogue lives — is a few pixels wide. `logarithmic(true)` plus
//! `integer()` gives every decade the same room and still lets the low end
//! reach 0, which is what turns a range back off.

use crate::panels::mcu_module::chip_filter::{self, Bounds, ChipFilter, CountFacet, TIER1};
use crate::panels::mcu_module::chip_search::Catalogue;
use eframe::egui;
use egui_phosphor::regular as ph;
use std::collections::BTreeSet;

/// What this machine's catalogue can be filtered by.
///
/// Derived once, when indexing lands, rather than per frame: it walks every
/// catalogued part, and the search beside it already does that on every
/// keystroke.
#[derive(Default)]
pub(super) struct Facets {
    pub bounds: Bounds,
    /// Tier-2 types, with how many parts carry each — commonest first.
    pub presence: Vec<(String, usize)>,
    pub families: Vec<String>,
    pub cores: Vec<String>,
    /// Package TYPES (`LQFP`, `UFBGA`), commonest first. 14 of them, against
    /// 133 full names — see `chip_filter::split_package`.
    pub packages: Vec<String>,
}

impl Facets {
    pub fn of(cat: &Catalogue) -> Self {
        let mut families = BTreeSet::new();
        let mut cores = BTreeSet::new();
        for e in cat.entries() {
            if !e.family.is_empty() {
                families.insert(e.family.clone());
            }
            for c in &e.cores {
                cores.insert(c.clone());
            }
        }
        Self {
            bounds: Bounds::of(cat.entries()),
            presence: chip_filter::presence_facets(cat.entries()),
            families: families.into_iter().collect(),
            cores: cores.into_iter().collect(),
            packages: chip_filter::package_types(cat.entries())
                .into_iter()
                .map(|(t, _)| t)
                .collect(),
        }
    }
}

fn dim(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(10.5)
        .color(egui::Color32::GRAY)
}

/// A KB figure at the size it is easiest to read.
fn kb(v: u32) -> String {
    if v >= 1024 && v % 1024 == 0 {
        format!("{} M", v / 1024)
    } else {
        format!("{v} K")
    }
}

/// One "from — to" row: two sliders that cannot cross.
///
/// Two single-thumb sliders rather than one double-thumb widget because egui
/// has no double-thumb slider, and the two-widget version types and tabs like
/// any other field.
fn range_row(
    ui: &mut egui::Ui,
    label: &str,
    v: &mut (u32, u32),
    b: (u32, u32),
    fmt: fn(u32) -> String,
) {
    ui.label(label);
    ui.horizontal(|ui| {
        // A catalogue with one indexed part gives a span of zero width. Nothing
        // to choose, and a slider over an empty range is a widget that can only
        // misbehave — say the value instead.
        if b.0 >= b.1 {
            ui.label(dim(format!("{} (every part)", fmt(b.0))));
            return;
        }
        let w = 118.0;
        ui.spacing_mut().slider_width = w;
        if ui
            .add(
                egui::Slider::new(&mut v.0, b.0..=b.1)
                    .logarithmic(true)
                    .integer()
                    .show_value(false),
            )
            .changed()
        {
            v.1 = v.1.max(v.0);
        }
        ui.label(dim(fmt(v.0)));
        ui.label(dim("to"));
        if ui
            .add(
                egui::Slider::new(&mut v.1, b.0..=b.1)
                    .logarithmic(true)
                    .integer()
                    .show_value(false),
            )
            .changed()
        {
            v.0 = v.0.min(v.1);
        }
        ui.label(dim(fmt(v.1)));
        // Only offered once the row is doing something, so the panel does not
        // read as a wall of buttons that all say "reset".
        if *v != b && ui.small_button(ph::X).on_hover_text("Any").clicked() {
            *v = b;
        }
    });
    ui.end_row();
}

/// One counted peripheral: "at least N of these".
fn count_cell(ui: &mut egui::Ui, f: &mut ChipFilter, facet: &CountFacet) {
    let mut n = f.counts.get(facet.id).copied().unwrap_or(0);
    ui.horizontal(|ui| {
        let on = n > 0;
        ui.add(
            crate::panels::drag_value(ui, &mut n)
                .range(0..=facet.max)
                .speed(0.05)
                .prefix(">="),
        )
        .on_hover_text(format!(
            "At least this many {} — the catalogue tops out at {}",
            facet.label, facet.max
        ));
        ui.label(if on {
            egui::RichText::new(facet.label).strong()
        } else {
            egui::RichText::new(facet.label).color(egui::Color32::from_rgb(150, 158, 172))
        });
    });
    if n == 0 {
        f.counts.remove(facet.id);
    } else {
        f.counts.insert(facet.id, n);
    }
}

/// A wrapped row of on/off chips.
fn chips(
    ui: &mut egui::Ui,
    all: &[String],
    picked: &mut BTreeSet<String>,
    label: fn(&str) -> String,
) {
    ui.horizontal_wrapped(|ui| {
        for name in all {
            let mut on = picked.contains(name);
            if ui
                .add(egui::Button::new(egui::RichText::new(label(name)).size(10.5)).selected(on))
                .clicked()
            {
                on = !on;
                if on {
                    picked.insert(name.clone());
                } else {
                    picked.remove(name);
                }
            }
        }
    });
}

/// Draw the twisty.
///
/// `f` is the DRAFT and `applied` is what the list is showing. They are separate
/// because every widget here reports a change on every frame it is touched, and
/// a slider drag is hundreds of frames — each one otherwise a fresh search over
/// several thousand parts. Nothing reaches the list until Apply.
pub(super) fn show_filters(
    ui: &mut egui::Ui,
    f: &mut ChipFilter,
    applied: &mut ChipFilter,
    facets: &Facets,
) {
    // The badge counts what is NARROWING THE LIST, not what is drafted — it
    // exists to explain a short list, and a draft explains nothing yet.
    let n = applied.active_count();
    let pending = f != applied;
    let title = if n == 0 && !pending {
        egui::RichText::new(format!("{} Filters", ph::FUNNEL))
            .size(10.5)
            .color(egui::Color32::GRAY)
    } else {
        // Loud on purpose: this is the only thing on screen that explains a
        // list which has gone short or empty.
        let label = match (n, pending) {
            (0, _) => format!("{} Filters (not applied)", ph::FUNNEL),
            (n, false) => format!("{} Filters ({n})", ph::FUNNEL),
            (n, true) => format!("{} Filters ({n}, edited)", ph::FUNNEL),
        };
        egui::RichText::new(label)
            .size(10.5)
            .strong()
            .color(egui::Color32::from_rgb(235, 185, 90))
    };

    egui::CollapsingHeader::new(title)
        .id_salt("chip_filters")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                // Enabled only when there is something to commit, so the button
                // doubles as the answer to "is what I see what I picked?".
                let apply = ui.add_enabled(
                    pending,
                    egui::Button::new(
                        egui::RichText::new(format!("{} Apply", ph::CHECK))
                            .size(11.0)
                            .strong(),
                    ),
                );
                if apply
                    .on_hover_text("Search with these filters")
                    .on_disabled_hover_text("The list already shows these filters")
                    .clicked()
                {
                    *applied = f.clone();
                }
                if (f.is_active() || applied.is_active())
                    && ui
                        .button(
                            egui::RichText::new(format!("{} Clear", ph::FUNNEL_X)).size(11.0),
                        )
                        .on_hover_text("Drop every filter, and search again now")
                        .clicked()
                {
                    // Clearing applies immediately: there is no version of
                    // "cleared, but not yet" worth making someone confirm.
                    *f = ChipFilter::new(f.bounds);
                    *applied = f.clone();
                }
                if pending {
                    ui.label(
                        egui::RichText::new("not applied yet")
                            .size(10.0)
                            .color(egui::Color32::from_rgb(235, 185, 90)),
                    );
                }
            });
            ui.add_space(2.0);

            let b = f.bounds;
            egui::Grid::new("chip_filter_ranges")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    range_row(ui, "Flash", &mut f.flash_kb, b.flash_kb, kb);
                    range_row(ui, "RAM", &mut f.ram_kb, b.ram_kb, kb);
                    range_row(ui, "Frequency", &mut f.mhz, b.mhz, |v| format!("{v} MHz"));
                    range_row(ui, "I/O pins", &mut f.io, b.io, |v| v.to_string());
                    range_row(ui, "Package pins", &mut f.pins, b.pins, |v| v.to_string());
                });

            ui.add_space(6.0);
            ui.label(dim("Peripherals — at least this many:"));
            egui::Grid::new("chip_filter_counts")
                .num_columns(3)
                .spacing([10.0, 3.0])
                .show(ui, |ui| {
                    for (i, facet) in TIER1.iter().enumerate() {
                        count_cell(ui, f, facet);
                        if i % 3 == 2 {
                            ui.end_row();
                        }
                    }
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.adc.required, "ADC").on_hover_text(
                    "Presence, not a count: the vendor's ADC number is CHANNELS, \
                         not instances — an F411 reports 16 and has one ADC.",
                );
                ui.checkbox(&mut f.adc.res_12, "12-bit");
                ui.checkbox(&mut f.adc.res_16, "16-bit");
                ui.label(dim("(the only two resolutions the vendor data names)"));
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut f.usb, "USB").on_hover_text(
                    "Any of the five the vendor names: USB Device, OTG_FS, OTG_HS, \
                     DRD_FS, USBH_HS.",
                );
            });

            if !facets.packages.is_empty() {
                ui.add_space(6.0);
                ui.label(dim("Package:"))
                    .on_hover_text(
                        "The package TYPE - one tick covers every size of it. Narrow the size with the Package pins row above, which counts the FOOTPRINT, not the usable I/O.",
                    );
                chips(ui, &facets.packages, &mut f.packages, str::to_owned);
            }

            if !facets.families.is_empty() {
                ui.add_space(6.0);
                ui.label(dim("Family:"));
                chips(ui, &facets.families, &mut f.families, str::to_owned);
            }
            if !facets.cores.is_empty() {
                ui.add_space(4.0);
                ui.label(dim("Core:"));
                // "Arm Cortex-M4" is four words of which one is the answer.
                chips(ui, &facets.cores, &mut f.cores, |c| {
                    c.trim_start_matches("Arm ").to_owned()
                });
            }

            if !facets.presence.is_empty() {
                ui.add_space(6.0);
                egui::CollapsingHeader::new(dim(format!(
                    "Advanced — {} more peripherals",
                    facets.presence.len()
                )))
                .id_salt("chip_filter_advanced")
                .default_open(false)
                .show(ui, |ui| {
                    ui.label(dim(
                        "Presence only. Derived from the vendor data on this machine, \
                         so it lists exactly what your CubeMX knows about.",
                    ));
                    // No scroll area of its own: the dialog body already
                    // scrolls, and a scroll area inside another splits the
                    // wheel between them - the same trap that hid the results.
                    ui.horizontal_wrapped(|ui| {
                        for (ty, parts) in &facets.presence {
                            let mut on = f.present.contains(ty);
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new(ty).size(10.5))
                                        .selected(on),
                                )
                                .on_hover_text(format!("{parts} parts have it"))
                                .clicked()
                            {
                                on = !on;
                                if on {
                                    f.present.insert(ty.clone());
                                } else {
                                    f.present.remove(ty);
                                }
                            }
                        }
                    });
                });
            }
        });
}
