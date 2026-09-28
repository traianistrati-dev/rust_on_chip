//! Headless egui frames for unit tests — and for the one frame the app lays
//! out itself, before eframe starts (`egui_memory_prune::live_type_ids`).
//!
//! Every test that lays real widgets out runs its frames through [`run_ui`]
//! instead of calling `Context::run_ui` itself.

use eframe::egui;

/// One headless egui frame, returned with its texture deltas already dropped.
///
/// From egui 0.36 on, a `TexturesDelta` that is dropped while still holding
/// deltas trips a `debug_assert!` in its `Drop`: the backend is expected to
/// upload them. The first frame of every `Context` carries the whole font
/// atlas, so a test that dropped its `FullOutput` (`let _ = ctx.run_ui(..)`,
/// or `.shapes` taken off the temporary) panics before it asserts anything.
/// A test (or the prune's probe) has no GPU to upload to, so this is the one
/// place that says so.
///
/// Everything a test reads is returned untouched: `shapes`,
/// `platform_output`, `viewport_output`, `pixels_per_point`.
pub fn run_ui(
    ctx: &egui::Context,
    input: egui::RawInput,
    ui: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut out = ctx.run_ui(input, ui);
    out.textures_delta.clear();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without the clear, dropping this frame's output panics in a debug
    /// build on egui 0.36: the font atlas is a delta nobody applied.
    #[test]
    fn a_dropped_first_frame_does_not_panic() {
        let ctx = egui::Context::default();
        let out = run_ui(&ctx, Default::default(), |ui| {
            ui.label("atlas");
        });
        assert!(out.textures_delta.is_empty());
        assert!(!out.shapes.is_empty(), "the label was painted");
    }

    /// The guard that keeps it the ONE place: a test calling
    /// `Context::run_ui` (or `end_pass`, which also hands back a `FullOutput`)
    /// directly compiles, passes on egui 0.34, and panics on 0.36 as soon as
    /// it drops the output. The app itself never calls either - eframe does,
    /// and the prune's probe goes through the helper - so any hit is a test
    /// that bypassed it.
    #[test]
    fn no_test_calls_context_run_ui_directly() {
        fn scan(dir: &std::path::Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    scan(&p, out);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs")
                    || p.file_name().and_then(|x| x.to_str()) == Some("headless.rs")
                {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else {
                    continue;
                };
                for (i, line) in text.lines().enumerate() {
                    let t = line.trim_start();
                    if t.starts_with("//") {
                        continue;
                    }
                    // `.run_ui(` also matches `headless::run_ui(`; only a
                    // method call on a context is the bypass.
                    if line.contains(".run_ui(") || line.contains(".end_pass(") {
                        out.push(format!("{}:{}  {}", p.display(), i + 1, t));
                    }
                }
            }
        }
        let mut bad = Vec::new();
        scan(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut bad,
        );
        assert!(
            bad.is_empty(),
            "call crate::headless::run_ui(&ctx, input, |ui| ..) - a FullOutput \
             dropped with the font atlas in it panics on egui 0.36:\n{}",
            bad.join("\n")
        );
    }
}
