//! MCU registry loader (Phase 5).
//!
//! Assembles the list of available [`McuDefinition`]s from two sources:
//! 1. **Built-ins** bundled in the binary via `include_str!`
//!    ([`builtins::builtin_definitions`]).
//! 2. **User imports** — `.ron` files dropped into a per-user `mcus/` folder
//!    (`%APPDATA%/embedded_ide_0/mcus` on Windows, `$XDG_CONFIG_HOME` /
//!    `~/.config` elsewhere). These load at startup with **no recompile**, so
//!    new chips inside an already-supported family are pure data.
//!
//! User definitions with the same `id` as a built-in **override** it, letting
//! a user patch a bundled chip without touching the binary.

use std::path::{Path, PathBuf};

use super::builtins;
use super::mcu_def::McuDefinition;

/// The app's per-user folder — `%APPDATA%/embedded_ide_0` on Windows,
/// `$XDG_CONFIG_HOME/embedded_ide_0` or `~/.config/embedded_ide_0` elsewhere.
///
/// `None` only if no suitable home/config dir can be resolved (rare). The single
/// definition of that path: the MCU registry and the crash log both hang off it,
/// and two copies of this env chain would drift.
pub fn user_config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA") // Windows
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from)) // Linux
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config"))); // Unix
    // Frozen at the rename: this folder holds the user's imported chips.
    base.map(|b| b.join(crate::names::LEGACY_DATA_DIR))
}

/// Per-user folder scanned for imported `.ron` definitions.
///
/// Returns `None` only if no suitable home/config dir can be resolved (rare);
/// callers then fall back to built-ins only.
pub fn user_mcus_dir() -> Option<PathBuf> {
    user_config_dir().map(|b| b.join("mcus"))
}

/// Build the full registry: built-ins first, then user `.ron` imports merged on
/// top (same-`id` user files override the built-in entry).
pub fn load_registry() -> Vec<McuDefinition> {
    load_registry_from(user_mcus_dir().as_deref())
}

/// Core loader, parameterised on the scan directory so it is unit-testable
/// without touching the real per-user folder. `None` (or a missing dir) yields
/// the built-ins only.
fn load_registry_from(user_dir: Option<&Path>) -> Vec<McuDefinition> {
    let mut defs = builtins::builtin_definitions();

    if let Some(dir) = user_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                    continue;
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(def) = parse_definition(&text) {
                        merge_def(&mut defs, def);
                    }
                    // Silently skip files that fail to parse — a malformed user
                    // file must never break startup.
                }
            }
        }
    }

    defs
}

/// Parse + validate a definition from RON text. Pure (no I/O) so it is unit
/// testable and reused by both [`load_registry`] and [`import_file`].
pub fn parse_definition(text: &str) -> Result<McuDefinition, String> {
    let mut def: McuDefinition =
        ron::from_str(text).map_err(|e| format!("RON parse error: {e}"))?;
    if def.id.trim().is_empty() {
        return Err("definition has an empty `id`".to_owned());
    }
    if def.display_name.trim().is_empty() {
        return Err("definition has an empty `display_name`".to_owned());
    }
    // A line an older importer derived is brought up to the current rule; the
    // file itself is rewritten only when the definition is next saved.
    super::stm32_pin_data::repair_hal_feature(&mut def);
    super::stm32_pin_data::repair_memory(&mut def);
    Ok(def)
}

/// Insert `def` into `defs`, overriding any existing entry with the same `id`.
pub fn merge_def(defs: &mut Vec<McuDefinition>, def: McuDefinition) {
    if let Some(slot) = defs.iter_mut().find(|d| d.id == def.id) {
        *slot = def;
    } else {
        defs.push(def);
    }
}

/// Which definition a project on disk targets, from its `src/main.rs` and
/// `Cargo.toml` texts.
///
/// Two signals, in priority order:
///   1. The `// rust_on_chip:mcu=<id>` marker in main.rs (or its pre-rename
///      spelling), written by our own codegen. It pins the EXACT definition,
///      including imported chips that share a HAL crate with a built-in.
///   2. The HAL crate named in Cargo.toml (first token of each definition's
///      `hal_dep`), for projects older than the marker.
///
/// ONE rule for both readers: opening a project and the Board tab reading a
/// chip it has not opened. Two copies would drift, and the Board would then
/// name a different chip than the one opening the same folder gives you.
pub fn detect_chip_id(
    defs: &[McuDefinition],
    main_rs: Option<&str>,
    cargo_toml: Option<&str>,
) -> Option<String> {
    main_rs
        .and_then(crate::panels::mcu_module::codegen::parse_mcu_id)
        .filter(|id| defs.iter().any(|d| &d.id == id))
        .or_else(|| {
            let cargo = cargo_toml?;
            defs.iter()
                .find(|d| {
                    let crate_name = d.project.hal_dep.split_whitespace().next().unwrap_or("");
                    !crate_name.is_empty() && cargo.contains(crate_name)
                })
                .map(|d| d.id.clone())
        })
}

/// Open the user `mcus/` folder in the OS file manager, creating it first so
/// the user always has a place to drop `.ron` files. Best-effort: any failure
/// is ignored (the path is also shown in the UI for manual navigation).
pub fn open_user_mcus_dir() {
    if let Some(dir) = user_mcus_dir() {
        let _ = std::fs::create_dir_all(&dir);
        open_in_file_manager(&dir);
    }
}

#[cfg(target_os = "windows")]
fn open_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

#[cfg(target_os = "macos")]
fn open_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("open").arg(path).spawn();
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_in_file_manager(path: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

/// Import a `.ron` file chosen by the user: parse it and persist a copy into the
/// user `mcus/` folder so it survives restarts. Returns the parsed definition
/// (the caller merges it into the live registry).
///
/// Persistence failures are non-fatal: the chip is still usable this session.
pub fn import_file(path: &Path) -> Result<McuDefinition, String> {
    import_file_into(path, user_mcus_dir().as_deref())
}

/// [`import_file`] into `dir`, so a test can point it somewhere harmless.
///
/// A part no project can be built for is refused BEFORE the copy is written:
/// the folder is read again at every start, so a refusal made only in the
/// dialog would bring the definition back on the next launch.
fn import_file_into(path: &Path, dir: Option<&Path>) -> Result<McuDefinition, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;
    let def = parse_definition(&text)?;
    if let Some(r) = super::stm32_support::refusal_for_def(&def) {
        return Err(format!("Not imported {}: {}", def.display_name, r.reason()));
    }

    if let Some(dir) = dir
        && std::fs::create_dir_all(dir).is_ok()
    {
        // Name the persisted copy by id so re-imports overwrite cleanly.
        let dest = dir.join(format!("{}.ron", def.id));
        let _ = std::fs::write(&dest, &text);
    }

    Ok(def)
}

/// Persist a form-authored definition to `<user mcus>/<id>.ron` (the same
/// folder imports live in, so it survives restarts and re-authoring an id
/// overwrites cleanly). Returns the written path. Fails only on I/O errors or
/// when no user dir resolves.
pub fn save_definition(def: &McuDefinition) -> Result<PathBuf, String> {
    let dir = user_mcus_dir().ok_or("could not resolve the user config folder")?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let text = ron::ser::to_string_pretty(def, ron::ser::PrettyConfig::default())
        .map(|t| super::ron_text::bare_none(&t))
        .map_err(|e| format!("RON serialize error: {e}"))?;
    let dest = dir.join(format!("{}.ron", def.id));
    std::fs::write(&dest, text).map_err(|e| format!("could not write {}: {e}", dest.display()))?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_registry_includes_builtins() {
        let defs = load_registry();
        assert!(defs.iter().any(|d| d.id == "stm32f103c8t6"));
        assert!(defs.iter().any(|d| d.id == "esp32c3"));
    }

    #[test]
    fn parse_definition_round_trips_a_builtin() {
        let original = builtins::builtin_for("stm32f103c8t6").unwrap();
        let text = ron::to_string(&original).unwrap();
        let parsed = parse_definition(&text).expect("valid definition parses");
        assert_eq!(parsed, original);
    }

    /// The marker names the exact chip, and wins over a HAL crate that would
    /// point at another one.
    #[test]
    fn the_main_rs_marker_decides_the_chip() {
        let defs = builtins::builtin_definitions();
        let main = "// Auto-generated by RustOnChip\n// rust_on_chip:mcu=esp32c3\n";
        let stm32_cargo = builtins::builtin_for("stm32f103c8t6")
            .unwrap()
            .project
            .hal_dep;
        assert_eq!(
            detect_chip_id(&defs, Some(main), Some(&stm32_cargo)).as_deref(),
            Some("esp32c3")
        );
        // The spelling written before the rename is read forever.
        let legacy = "// embedded-ide:mcu=esp32c3\n";
        assert_eq!(
            detect_chip_id(&defs, Some(legacy), None).as_deref(),
            Some("esp32c3")
        );
    }

    /// No marker (an older project): the HAL crate in Cargo.toml decides. A
    /// marker naming a chip this build does not know falls back the same way
    /// rather than inventing a chip.
    #[test]
    fn without_a_known_marker_the_hal_crate_decides() {
        let defs = builtins::builtin_definitions();
        let esp = builtins::builtin_for("esp32c3").unwrap();
        let hal = esp
            .project
            .hal_dep
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let cargo = format!("[dependencies]\n{hal} = \"1\"\n");
        let found = detect_chip_id(&defs, Some("fn main() {}"), Some(&cargo)).unwrap();
        let found_hal = defs
            .iter()
            .find(|d| d.id == found)
            .unwrap()
            .project
            .hal_dep
            .clone();
        assert!(found_hal.starts_with(&hal), "{found} does not use {hal}");
        let unknown = "// rust_on_chip:mcu=no_such_chip\n";
        assert!(detect_chip_id(&defs, Some(unknown), None).is_none());
        assert!(detect_chip_id(&defs, None, None).is_none());
    }

    /// A definition no project can be built for is refused with the reason,
    /// and nothing is written into the folder the registry loads from.
    #[test]
    fn a_refused_ron_is_not_copied_in() {
        let tmp = std::env::temp_dir().join(format!("eide_refused_ron_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let mut def = builtins::builtin_for("stm32f103c8t6").expect("the bundled F103");
        def.id = "stm32wb09tefx".into();
        def.display_name = "STM32WB09TEFx".into();
        def.family = "stm32wb0".into();
        def.project.hal_dep =
            "embassy-stm32 = { version = \"0.6\", features = [\"stm32wb09te\"] }".into();
        let src = tmp.join("in.ron");
        std::fs::write(&src, ron::ser::to_string(&def).unwrap()).unwrap();
        let store = tmp.join("mcus");
        let err = import_file_into(&src, Some(&store)).expect_err("refused");
        assert!(err.starts_with("Not imported STM32WB09TEFx: "), "{err}");
        assert!(err.contains("stm32wb09te"), "{err}");
        assert!(!store.exists(), "nothing written");
        // The same file for a part that builds goes in.
        let mut ok = def.clone();
        ok.id = "stm32wb55cgux".into();
        ok.display_name = "STM32WB55CGUx".into();
        ok.family = "stm32wb".into();
        ok.project.hal_dep =
            "embassy-stm32 = { version = \"0.6\", features = [\"stm32wb55cg\"] }".into();
        std::fs::write(&src, ron::ser::to_string(&ok).unwrap()).unwrap();
        import_file_into(&src, Some(&store)).expect("imports");
        assert!(store.join("stm32wb55cgux.ron").is_file());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parse_definition_rejects_empty_id() {
        let mut def = builtins::builtin_for("esp32c3").unwrap();
        def.id = "  ".to_owned();
        let text = ron::to_string(&def).unwrap();
        assert!(parse_definition(&text).is_err());
    }

    #[test]
    fn parse_definition_rejects_garbage() {
        assert!(parse_definition("not ron at all {{{").is_err());
    }

    #[test]
    fn merge_def_overrides_same_id() {
        let mut defs = builtins::builtin_definitions();
        let before = defs.len();

        // Same id → replace, length unchanged.
        let mut patched = builtins::builtin_for("stm32f103c8t6").unwrap();
        patched.display_name = "PATCHED".to_owned();
        merge_def(&mut defs, patched);
        assert_eq!(defs.len(), before);
        assert_eq!(
            defs.iter()
                .find(|d| d.id == "stm32f103c8t6")
                .unwrap()
                .display_name,
            "PATCHED"
        );
    }

    #[test]
    fn merge_def_appends_new_id() {
        let mut defs = builtins::builtin_definitions();
        let before = defs.len();

        let mut fresh = builtins::builtin_for("stm32f103c8t6").unwrap();
        fresh.id = "stm32f103rb".to_owned();
        merge_def(&mut defs, fresh);
        assert_eq!(defs.len(), before + 1);
        assert!(defs.iter().any(|d| d.id == "stm32f103rb"));
    }

    /// End-to-end scan: a `.ron` dropped into the user folder is discovered,
    /// parsed, merged, and builds into a runtime `Mcu`. Uses a temp dir so the
    /// real per-user folder is never touched.
    #[test]
    fn scan_picks_up_user_ron_file() {
        let dir = tempfile::tempdir().unwrap();

        // A user-authored variant in the already-supported STM32F1 family.
        let mut def = builtins::builtin_for("stm32f103c8t6").unwrap();
        def.id = "stm32f103rb".to_owned();
        def.display_name = "STM32F103RB (imported)".to_owned();
        let text = ron::to_string(&def).unwrap();
        std::fs::write(dir.path().join("stm32f103rb.ron"), &text).unwrap();
        // A non-RON file must be ignored, not break the scan.
        std::fs::write(dir.path().join("notes.txt"), "ignore me").unwrap();
        // A malformed RON must be skipped silently.
        std::fs::write(dir.path().join("broken.ron"), "garbage {{{").unwrap();

        let defs = load_registry_from(Some(dir.path()));

        // Built-ins still present + the imported chip added.
        assert!(defs.iter().any(|d| d.id == "stm32f103c8t6"));
        assert!(defs.iter().any(|d| d.id == "esp32c3"));
        let imported = defs
            .iter()
            .find(|d| d.id == "stm32f103rb")
            .expect("imported chip must appear in the registry");
        assert_eq!(imported.display_name, "STM32F103RB (imported)");

        // And it builds into a usable runtime Mcu.
        let mcu = imported.build_mcu();
        assert!(mcu.iter_all_pins().count() > 0);
    }

    #[test]
    fn missing_user_dir_yields_builtins_only() {
        let baseline = builtins::builtin_definitions().len();
        let defs = load_registry_from(Some(Path::new("Z:/no/such/dir/exists")));
        assert_eq!(defs.len(), baseline);
    }
}

#[cfg(test)]
mod one_shot_import {
    //! A hand-run import, for when a chip is needed on THIS machine and
    //! clicking through the dialog is not where the work is.
    //!
    //! It repeats the dialog's per-chip orchestration (`AppIde::import_stm32_xml`)
    //! rather than calling it, because that one is wound through `&mut self` and
    //! the UI's status strings. The building blocks are the same functions, so a
    //! definition written here is the one the dialog would write — but if the
    //! dialog grows a step, this has to grow it too.

    /// ```text
    /// EIDE_IMPORT_XML="…/STM32G474R(B-C-E)Tx.xml" \
    ///   cargo test --bin rust_on_chip import_one_chip -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "writes a chip definition into the user's config folder"]
    fn import_one_chip() {
        use crate::panels::mcu_module::codegen::{dma_data, nvic};
        use crate::panels::mcu_module::{clock::graph::cubemx, stm32_pin_data};

        let Ok(chip) = std::env::var("EIDE_IMPORT_XML") else {
            eprintln!("set EIDE_IMPORT_XML to the chip's .xml - nothing imported");
            return;
        };
        let path = std::path::Path::new(&chip);
        let xml = std::fs::read_to_string(path).expect("read the chip xml");
        let af = stm32_pin_data::gpio_ip_version(&xml).and_then(|v| {
            let f = path
                .parent()?
                .join("IP")
                .join(stm32_pin_data::gpio_ip_file_name(&v));
            Some(stm32_pin_data::GpioAf::parse(
                &std::fs::read_to_string(f).ok()?,
            ))
        });
        let mut dma_cache = std::collections::HashMap::new();
        let mut irq_cache = std::collections::HashMap::new();
        let dma = dma_data::dma_def_for(&xml, path.parent(), &mut dma_cache);
        let irqs = nvic::vectors_for(&xml, path.parent(), &mut irq_cache);
        // `db/mcu/<chip>.xml` -> `db`, which is where the clock trees live.
        let db = path.parent().and_then(|p| p.parent());

        for chip in stm32_pin_data::convert_xml_with_af(&xml, af.as_ref()).expect("converts") {
            let errs = chip.form.errors();
            assert!(errs.is_empty(), "{}: {}", chip.form.display_name, errs[0]);
            let mut def = chip.form.to_definition();
            def.dma = dma.clone();
            def.irq_vectors = irqs.clone();
            def.usart_ip = stm32_pin_data::usart_ip_version(&xml);
            if let Some(db) = db {
                match cubemx::graph_for_chip_xml(db, &xml, &def.family) {
                    Ok((gc, _missing)) => {
                        def.clock = crate::panels::mcu_module::mcu_def::ClockDef::Graph(gc)
                    }
                    Err(e) => eprintln!("{}: no clock tree ({e})", def.display_name),
                }
            }
            let where_ = super::save_definition(&def).expect("save");
            println!(
                "imported {} -> {}  (dma {}, {} vectors)",
                def.display_name,
                where_.display(),
                def.dma.as_ref().map_or(0, |d| d.channels.len()),
                def.irq_vectors.len()
            );
        }
    }
}
