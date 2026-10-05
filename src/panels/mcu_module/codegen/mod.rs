//! Code generation for MCU projects — routes to a per-**family** backend.
//!
//! [`Mcu::fresh_main_rs`] / [`Mcu::update_main_rs`] dispatch on `self.family`
//! via the [`family`] module's [`FamilyBackend`](family::FamilyBackend)
//! registry. Each backend wraps the HAL-specific generator:
//! - "stm32f1" → `stm32` module (Rust Embedded HAL)
//! - "esp32c3" → parent `codegen_esp` module (esp-hal)
//! - "rp2040" / "rp235x" → [`rp`] (rp2040-hal / rp235x-hal, embassy-rp)
//! - "nrf52833" (any `nrf52*`) → [`nrf`] (nrf52833-hal)
//! - unknown families (e.g. STM8) → no generated code yet.

pub mod common;
pub mod dma_data;
pub mod dma_map;
pub mod embassy_async;
pub mod embassy_common;
pub mod family;
pub mod flash_store_gen;
pub mod nrf;
pub mod nrf_boards;
pub mod nvic;
pub mod rcc;
pub mod rcc_mux;
mod rcc_mux_data;
pub mod rcc_n6;
pub mod rp;
pub mod rtic;
pub mod stm32;
pub mod watchdog_gen;
pub mod wba;

use super::mcu::{Mcu, Runtime};

// Re-export public API for backward compatibility
pub use common::{
    AUTOGEN_BANNER, GEN_BEGIN, GEN_END, LEGACY_AUTOGEN_BANNER, MCU_ID_MARKER, USER_TAIL,
    mcu_id_marker_line, parse_main_rs, parse_mcu_id, parse_pin_labels, pin_binding, sanitize_label,
    var_suffix,
};

/// Type-parameter name for a pin in a Custom module's struct: the pin's own name
/// (`PC15` → `PC15`), uppercased and sanitised to a valid Rust identifier.
fn generic_param_name(pin_name: &str) -> String {
    let mut s: String = pin_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        s.push('P');
    } else if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, 'P');
    }
    s
}

// ── Public API on Mcu ─────────────────────────────────────────────────────────

impl Mcu {
    /// The code-generation backend for this MCU, honouring both `family` and
    /// `runtime`: the async embassy backend when [`Runtime::Async`] applies to
    /// the family, otherwise the family's default (blocking) backend. Families
    /// without a registered backend (e.g. STM8) yield `None`.
    ///
    /// [`Runtime::Async`]: crate::panels::mcu_module::mcu::Runtime
    fn backend(&self) -> Option<&'static dyn family::FamilyBackend> {
        family::backend_for_runtime(&self.family, self.runtime)
    }

    /// Whether code generation for this MCU is on the async (embassy) path — the
    /// project [`Runtime`](crate::panels::mcu_module::mcu::Runtime) is Async AND
    /// the family supports it. Drives the embassy async deps + the System-tab
    /// toggle's effective state (Async selected on a family with no async
    /// backend is inert).
    pub fn is_async(&self) -> bool {
        self.runtime == Runtime::Async && family::async_supported(&self.family)
    }

    /// Whether code generation for this MCU is on the RTIC path — the project
    /// Runtime is Rtic AND the family supports it. Drives the RTIC deps and the
    /// System-tab card's effective state (Rtic selected elsewhere is inert, the
    /// same way Async is on a family with no async backend).
    pub fn is_rtic(&self) -> bool {
        self.runtime == Runtime::Rtic && family::rtic_supported(&self.family)
    }

    /// Whether code generation is on the Native (concrete-HAL) path — Runtime is
    /// Native AND the family has concrete-HAL templates (STM32F1). The bus
    /// peripherals are then forced to `ApiStyle::Native` regardless of their
    /// per-module setting.
    pub fn is_native(&self) -> bool {
        self.runtime == Runtime::Native && family::native_supported(&self.family)
    }

    /// Like [`Mcu::is_async`] but reading the STAGED `pending_runtime` — used by
    /// the module config UI so its selectors follow the runtime the user is about
    /// to apply, not the applied one.
    pub fn pending_is_async(&self) -> bool {
        self.pending_runtime == Runtime::Async && family::async_supported(&self.family)
    }

    /// Like [`Mcu::is_native`] but reading the STAGED `pending_runtime`.
    pub fn pending_is_native(&self) -> bool {
        self.pending_runtime == Runtime::Native && family::native_supported(&self.family)
    }

    /// Whether GPIO In/Out bind the RAW HAL pin (no `pins/configs/io.rs`
    /// `DigitalOut`/`DigitalIn` bridge) — true when the GPIO api is `Native` OR
    /// the whole project runs Native. `false` (the default) emits the portable
    /// embedded-hal 1.0 bridge. Only consulted on the STM32F1 blocking path
    /// (async uses embassy `Output`/`Input`; other families have no io.rs).
    pub fn gpio_native(&self) -> bool {
        use crate::panels::mcu_module::modules::ApiStyle;
        self.is_native() || self.gpio_api == ApiStyle::Native
    }

    /// Keep the GPIO api in step with the Runtime: **Native binds every GPIO
    /// raw**, so "Portable" is not a live choice there — the System tab locks
    /// the selector, but the stored value used to stay `Portable`, which showed
    /// a setting the build does not use AND left `embedded-hal` looking needed.
    /// Snapping it to `Native` makes the panel honest and lets the unused
    /// dependency be dropped. Idempotent; called every frame from `init_frame`.
    /// Both the applied and the staged value follow, so the locked selector
    /// updates the moment Native is picked (before Apply).
    /// Build a brand-new `src/main.rs` (called when the MCU type is first
    /// selected or reset). Dispatches on `self.family` + `self.runtime`; families
    /// without a registered backend produce an empty file.
    pub fn fresh_main_rs(&self) -> String {
        let code = self
            .backend()
            .map(|b| b.fresh_main_rs(self))
            .unwrap_or_default();
        let code = self.seed_flash_store_tail(code);
        // Module/clock state is persisted out-of-source in `mcu.config`
        // (see `Mcu::mcu_config_text`), not as comment markers in main.rs.
        let code = common::ensure_module_models(code, &self.modules);
        // The handler of an armed input calls a hook the user owns: seeded
        // once below the tail, never rewritten (see `common::EdgeHook`).
        let hooks = self
            .backend()
            .map(|b| b.edge_hooks(self))
            .unwrap_or_default();
        let code = common::ensure_edge_hooks(code, &hooks);
        // The devices the user grouped, gathered into one comment.
        let code = common::with_device_comment(code, self);
        // Strict-lints: exempt the generated entry fn (its init uses unwrap/as…).
        common::strict_main_exemption(code, self.strict_lints)
    }

    /// Update `existing` in-place: replace only the generated section
    /// (between the markers), preserving the user-editable parts.
    ///
    /// Families without a GEN block (ESP32-C3 has its own scheme; STM8 has
    /// no backend) return the existing file unchanged.
    pub fn update_main_rs(&self, existing: &str) -> String {
        let code = self
            .backend()
            .map(|b| b.update_main_rs(self, existing))
            .unwrap_or_else(|| existing.to_owned());
        let code = self.seed_flash_store_tail(code);
        // Module/clock state is persisted in `mcu.config`, not in main.rs.
        let code = common::ensure_module_models(code, &self.modules);
        // The handler of an armed input calls a hook the user owns: seeded
        // once below the tail, never rewritten (see `common::EdgeHook`).
        let hooks = self
            .backend()
            .map(|b| b.edge_hooks(self))
            .unwrap_or_default();
        let code = common::ensure_edge_hooks(code, &hooks);
        // Rebuilt with the block, so renaming a device in the panel renames it
        // here on the next save.
        let code = common::with_device_comment(code, self);
        // Strict-lints exemption on the (freshly re-spliced) entry fn.
        common::strict_main_exemption(code, self.strict_lints)
    }

    /// The flash store's line at the head of the user's tail, on or off with
    /// the Configuration-tab toggle (see `flash_store_gen::seed_tail`). Where
    /// the store is not generated the line can only be a leftover (a chip
    /// changed under it): it is taken off and no other family's seed is
    /// touched. Run BEFORE the passes that append to the end of the file,
    /// whose additions do not count as the user's.
    fn seed_flash_store_tail(&self, code: String) -> String {
        if crate::panels::mcu_module::flash_store::platform_of(self).is_err() {
            return flash_store_gen::strip_tail_seed(code);
        }
        flash_store_gen::seed_tail(code, self.flash_store.is_some(), self.is_async())
    }

    /// Per-peripheral init module bodies for `src/pins/configs/` — `(file_name,
    /// generated_body)`. Empty for families without separate config files.
    pub fn config_files(&self) -> Vec<(String, String)> {
        let mut files = self
            .backend()
            .map(|b| b.config_files(self))
            .unwrap_or_default();
        // User-authored Custom modules produce a file each. They are generated
        // HERE rather than in a family backend because the struct is generic
        // over its pin types — no HAL type is named, so the same code is valid
        // on every family and on both the Portable and Native paths.
        files.extend(self.custom_module_files());
        // The Configuration tab's flash store, on every family it is generated
        // for (ESP and STM32 alike - `flash_store::platform` decides).
        files.extend(flash_store_gen::config_files_for(self));
        files
            .into_iter()
            // Strict-lints: exempt each generated peripheral config module.
            .map(|(name, body)| {
                (
                    name,
                    common::strict_config_exemption(body, self.strict_lints),
                )
            })
            .collect()
    }

    /// The `custom_<name>` root of every Custom module's file: the older
    /// revisions under it stay on disk when the project tree prunes
    /// `pins/configs/` (see `ProjectTreeState::sync_config_files`).
    pub fn custom_keep_prefixes(&self) -> Vec<String> {
        self.modules
            .iter()
            .filter(|m| m.kind.is_custom())
            .map(crate::panels::mcu_module::mcu::gui::modules::custom_file_prefix)
            .collect()
    }

    /// The `src/pins/` files a project with this configuration holds, built by
    /// the SAME two calls the app makes on every regeneration. Every emit
    /// harness writes these rather than assembling `configs/mod.rs` itself: a
    /// harness that builds the tree its own way tests a project the app never
    /// produces.
    #[cfg(test)]
    pub(crate) fn pin_tree_files(&self) -> Vec<(String, String)> {
        let mut tree = crate::project_tree::logic::ProjectTreeState::new();
        tree.sync_config_files(
            &self.config_files(),
            false,
            &self.custom_keep_prefixes(),
            &mut None,
        );
        tree.sync_pin_files(&self.all_pin_functions());
        tree.user_src_files
    }

    /// Put `devices` - `(name, address)`, an empty name for an unnamed one -
    /// on the first I2C bus, through the panel's own write path, so an emit
    /// harness compiles a project WITH device files rather than a bus that
    /// has none. `false`, and no change, when there is no I2C module: a
    /// harness asserts it, so a fixture that lost its bus cannot pass quietly
    /// without a device.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_i2c_devices(&mut self, devices: &[(&str, u8)]) -> bool {
        use crate::panels::mcu_module::modules::{I2cDeviceEdit as E, ModuleConfig};
        let Some(instance) = self.modules.iter().find_map(|m| match m.config {
            ModuleConfig::I2c(_) => Some(m.instance()),
            _ => None,
        }) else {
            return false;
        };
        for (name, address) in devices {
            assert!(
                self.edit_i2c_device(instance, E::Add),
                "no I2C{instance} to add to"
            );
            let key = self
                .i2c_bus(instance)
                .and_then(|c| c.rows().last().map(|r| r.key))
                .expect("the device just added");
            if !name.is_empty() {
                self.edit_i2c_device(instance, E::Name(key, (*name).to_owned()));
            }
            self.edit_i2c_device(instance, E::Address(key, *address));
        }
        true
    }

    /// `let <name> = <Struct>::new(<pin bindings…>);` for every Custom module —
    /// the lines spliced into main.rs's generated section, right after the pin
    /// bindings the call consumes.
    ///
    /// Only pins that main.rs binds as an INDEPENDENT variable can be passed:
    /// GPIO In/Out and PWM. A peripheral pin (USART TX, SPI SCK…) is moved into
    /// its `pins::configs::*::init(…)` call, so handing it to a custom struct
    /// too would not compile — those modules get a commented line explaining it
    /// instead of code that breaks the build.
    pub fn custom_module_inits(&self) -> String {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let mut out = String::new();
        for m in self.modules.iter().filter(|m| m.kind == ModuleKind::Custom) {
            let ModuleConfig::Custom(cfg) = &m.config else {
                continue;
            };
            if cfg.applied_pins.is_empty() {
                continue;
            }
            let var = super::mcu::gui::modules::custom_binding_name(m);
            let struct_name = super::mcu::gui::modules::custom_struct_name(m);
            let pins: Vec<_> = cfg
                .applied_pins
                .iter()
                .filter_map(|n| self.find_pin(*n))
                .collect();
            if pins.len() != cfg.applied_pins.len() {
                continue; // a pin vanished from the chip
            }
            // Independently-bound pins only.
            let taken: Vec<&str> = pins
                .iter()
                .filter(|p| {
                    !matches!(
                        p.selected_function,
                        PinFunction::GpioInput
                            | PinFunction::GpioOutput
                            | PinFunction::TimerPwm { .. }
                    )
                })
                .map(|p| p.name.as_str())
                .collect();
            if !taken.is_empty() {
                out.push_str(&format!(
                    "    // Custom module `{var}` not built here: {} {} owned by a \
                     peripheral init,\n    // so {} cannot also be moved into \
                     `{struct_name}`. Use GPIO In/Out or PWM pins,\n    // or construct it \
                     yourself after that peripheral.\n",
                    taken.join(", "),
                    if taken.len() == 1 { "is" } else { "are" },
                    if taken.len() == 1 { "it" } else { "they" },
                ));
                continue;
            }
            let args: Vec<String> = pins
                .iter()
                .map(|p| {
                    common::pin_binding(
                        &p.name.to_ascii_lowercase(),
                        &p.selected_function,
                        &p.custom_label,
                    )
                })
                .collect();
            // The module path follows the CURRENT revision, so main.rs always
            // calls the file this Update just wrote.
            out.push_str(&format!(
                "    let mut {var} = pins::configs::{stem}::{struct_name}::new({});\n",
                args.join(", "),
                stem = super::mcu::gui::modules::custom_file_stem(m),
            ));
        }
        out
    }

    /// `configs/custom_<name>.rs` for every Custom module that has at least one
    /// pin: a struct with one field per pin (named `<pin>_<type>`, e.g.
    /// `pa0_out`) plus a `new(…)` taking them in the order the user added them.
    ///
    /// Each field gets its OWN type parameter, so the struct holds whatever the
    /// pin binding happens to be — a raw `stm32f1xx-hal` pin on the Native path,
    /// a `DigitalOut` bridge on the Portable one — without this generator having
    /// to know or track HAL types.
    fn custom_module_files(&self) -> Vec<(String, String)> {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let mut out = Vec::new();
        for m in self.modules.iter().filter(|m| m.kind == ModuleKind::Custom) {
            let ModuleConfig::Custom(cfg) = &m.config else {
                continue;
            };
            if cfg.applied_pins.is_empty() {
                continue; // nothing committed yet — press Update
            }
            // Field name per pin: `<pin name lowercased>_<function suffix>`.
            // `<pin>_<type>_<label>` — the SAME name main.rs binds, so the
            // generated `::new(…)` call reads as a straight hand-over.
            let fields: Vec<String> = cfg
                .applied_pins
                .iter()
                .filter_map(|n| self.find_pin(*n))
                .map(|p| {
                    common::pin_binding(
                        &p.name.to_ascii_lowercase(),
                        &p.selected_function,
                        &p.custom_label,
                    )
                })
                .collect();
            if fields.is_empty() {
                continue;
            }
            let struct_name = super::mcu::gui::modules::custom_struct_name(m);
            // One type parameter per pin, NAMED AFTER THE PIN (`PC15`, `PD0`, …)
            // instead of `P0, P1, P2` — the impl bounds then read against the pin
            // you actually wired, which is what the user reads them for.
            let generics: Vec<String> = cfg
                .applied_pins
                .iter()
                .filter_map(|n| self.find_pin(*n))
                .map(|p| generic_param_name(&p.name))
                .collect();
            let gen_list = generics.join(", ");
            let decl = fields
                .iter()
                .zip(&generics)
                .map(|(f, g)| format!("    pub {f}: {g},"))
                .collect::<Vec<_>>()
                .join("\n");
            let params = fields
                .iter()
                .zip(&generics)
                .map(|(f, g)| format!("{f}: {g}"))
                .collect::<Vec<_>>()
                .join(", ");
            let init = fields
                .iter()
                .map(|f| format!("            {f},"))
                .collect::<Vec<_>>()
                .join("\n");

            // ── impl skeleton with the right embedded-hal bounds ─────────────
            // The struct itself stays bound-free (bounds on a struct propagate
            // everywhere); the READY-TO-EDIT impl below carries them, chosen per
            // pin direction: an input pin gets `InputPin`, an output `OutputPin`.
            // Part of a file written ONCE — see the body below.
            let bounds: Vec<Option<&str>> = cfg
                .applied_pins
                .iter()
                .filter_map(|n| self.find_pin(*n))
                .map(|p| match p.selected_function {
                    PinFunction::GpioInput => Some("InputPin"),
                    PinFunction::GpioOutput | PinFunction::TimerPwm { .. } => Some("OutputPin"),
                    _ => None,
                })
                .collect();
            let bounded: Vec<String> = generics
                .iter()
                .zip(&bounds)
                .map(|(g, b)| match b {
                    Some(t) => format!("{g}: {t}"),
                    None => g.clone(),
                })
                .collect();
            let mut traits: Vec<&str> = bounds.iter().flatten().copied().collect();
            traits.sort_unstable();
            traits.dedup();
            let use_line = if traits.is_empty() {
                String::new()
            } else {
                format!("use embedded_hal::digital::{{{}}};\n\n", traits.join(", "))
            };
            // One commented example call per pin, using its real field name.
            let examples: Vec<String> = fields
                .iter()
                .zip(&bounds)
                .filter_map(|(f, b)| match b {
                    Some("InputPin") => Some(format!(
                        "        // let {f} = self.{f}.is_high().unwrap_or(false);"
                    )),
                    Some("OutputPin") => Some(format!("        // let _ = self.{f}.set_high();")),
                    _ => None,
                })
                .collect();
            let skeleton = format!(
                "{use_line}\
                 // Your own methods for this module. The bounds come from each pin's\n\
                 // direction, so `.is_high()` / `.set_high()` are available here.\n\
                 // NB: after a pin change, Update writes a NEW `custom_<name>_<n>.rs`\n\
                 // and main.rs switches to it; this file is left exactly as it is.\n\
                 impl<{bounded_list}> {struct_name}<{gen_list}> {{\n\
                 \x20   pub fn update(&mut self) {{\n\
                 {examples}\n\
                 \x20   }}\n\
                 }}\n",
                bounded_list = bounded.join(", "),
                examples = if examples.is_empty() {
                    "        // …".to_owned()
                } else {
                    examples.join("\n")
                },
            );
            // No GENERATED markers: the whole file is the user's from the moment
            // it exists. It is written once per Update (every Update is a new
            // revision file) and `ProjectTree::sync_config_files` never touches an
            // existing one — before, every regeneration (including the one on the
            // first frame after the IDE starts) re-spliced the struct and `new()`,
            // deleting any field the user had added.
            let body = format!(
                "// Custom module \"{name}\" — one field per pin, in the order they\n\
                 // were added in the Virtual-module panel. Generic over each pin's\n\
                 // type, so it compiles whatever the pins bind to.\n\
                 // The whole file is yours: it is never regenerated. Extra fields are\n\
                 // fine — initialise them inside `new()`, since main.rs calls `new()`\n\
                 // with exactly one argument per pin.\n\
                 pub struct {struct_name}<{gen_list}> {{\n\
                 {decl}\n\
                 }}\n\
                 \n\
                 impl<{gen_list}> {struct_name}<{gen_list}> {{\n\
                 \x20   pub fn new({params}) -> Self {{\n\
                 \x20       Self {{\n\
                 {init}\n\
                 \x20       }}\n\
                 \x20   }}\n\
                 }}\n\
                 \n\
                 {skeleton}",
                name = m.config.custom_label(),
            );
            out.push((
                format!("{}.rs", super::mcu::gui::modules::custom_file_stem(m)),
                body,
            ));
        }
        out
    }

    /// Kept for any remaining call sites — delegates to `fresh_main_rs`.
    #[allow(dead_code)]
    pub fn generate_code(&self) -> String {
        self.fresh_main_rs()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    // ── Helper functions ──────────────────────────────────────────────────────

    fn assert_contains_substring(haystack: &str, needle: &str) {
        assert!(
            haystack.contains(needle),
            "Expected substring not found: '{}'\n\nIn:\n{}",
            needle,
            haystack
        );
    }

    fn assert_not_contains_substring(haystack: &str, needle: &str) {
        assert!(
            !haystack.contains(needle),
            "Unexpected substring found: '{}'\n\nIn:\n{}",
            needle,
            haystack
        );
    }

    fn count_gen_sections(code: &str) -> usize {
        code.matches(GEN_BEGIN).count()
    }

    // ── Parse Main RS Tests (PUBLIC API) ──────────────────────────────────────

    #[test]
    fn test_parse_main_rs_stm32_gpio_output() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nlet pa0 = &mut gpioa.pa0.into_push_pull_output(&mut gpioa.crl); // GPIO Output\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        assert!(parsed.iter().any(|(name, _)| name == "PA0"));
        assert!(
            parsed
                .iter()
                .any(|(_, func)| *func == PinFunction::GpioOutput)
        );
    }

    #[test]
    fn test_parse_main_rs_stm32_gpio_input() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nlet pc13 = &mut gpioc.pc13.into_floating_input(&mut gpioc.crh); // GPIO Input\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        assert!(parsed.iter().any(|(name, _)| name == "PC13"));
        assert!(
            parsed
                .iter()
                .any(|(_, func)| *func == PinFunction::GpioInput)
        );
    }

    #[test]
    fn test_parse_main_rs_multiple_pins() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nlet pa0 = &mut gpioa.pa0.into_push_pull_output(&mut gpioa.crl); // GPIO Output\nlet pc13 = &mut gpioc.pc13.into_floating_input(&mut gpioc.crh); // GPIO Input\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn test_parse_main_rs_no_markers() {
        let input =
            "let pa0 = &mut gpioa.pa0.into_push_pull_output(&mut gpioa.crl); // GPIO Output";
        let parsed = parse_main_rs(input);

        assert!(parsed.is_empty(), "Should not parse without markers");
    }

    #[test]
    fn test_parse_main_rs_empty_gen_section() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_main_rs_adc_channel() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nlet pa1 = &mut gpioa.pa1.into_analog(&mut gpioa.crl); // ADC1  IN1\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].0, "PA1");
    }

    #[test]
    fn test_parse_main_rs_skips_port_split_lines() {
        let input = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nlet mut gpioa = dp.GPIOA.split();\nlet pa0 = &mut gpioa.pa0.into_push_pull_output(&mut gpioa.crl); // GPIO Output\n// <<< GENERATED END >>>";
        let parsed = parse_main_rs(input);

        // Should only have one pin (PA0), not the port split
        assert_eq!(parsed.len(), 1);
    }

    // ── Marker Constants Tests ────────────────────────────────────────────────

    #[test]
    fn test_gen_begin_marker_constant_exists() {
        assert!(!GEN_BEGIN.is_empty());
        assert!(GEN_BEGIN.contains("GENERATED BEGIN"));
        assert!(GEN_BEGIN.contains("do not edit"));
    }

    #[test]
    fn test_gen_end_marker_constant_exists() {
        assert!(!GEN_END.is_empty());
        assert!(GEN_END.contains("GENERATED END"));
    }

    #[test]
    fn test_user_tail_constant_exists() {
        assert!(!USER_TAIL.is_empty());
        assert!(USER_TAIL.contains("loop"));
    }

    // ── Marker Count Tests ────────────────────────────────────────────────────

    #[test]
    fn test_count_gen_sections_single() {
        let code = "// <<< GENERATED BEGIN — do not edit between these markers >>>\ncode\n// <<< GENERATED END >>>";
        assert_eq!(count_gen_sections(code), 1);
    }

    #[test]
    fn test_count_gen_sections_multiple() {
        let code = "// <<< GENERATED BEGIN — do not edit between these markers >>>\ncode1\n// <<< GENERATED END >>>\n// <<< GENERATED BEGIN — do not edit between these markers >>>\ncode2\n// <<< GENERATED END >>>";
        assert_eq!(count_gen_sections(code), 2);
    }

    #[test]
    fn test_count_gen_sections_none() {
        let code = "// No markers here";
        assert_eq!(count_gen_sections(code), 0);
    }

    // ── Fresh Code Generation Tests (via Mcu::fresh_main_rs) ─────────────────

    #[test]
    fn test_fresh_main_rs_stm32_no_pins_has_markers() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        assert_contains_substring(&code, GEN_BEGIN);
        assert_contains_substring(&code, GEN_END);
        assert_eq!(count_gen_sections(&code), 1);
    }

    #[test]
    fn test_fresh_main_rs_stm32_has_required_headers() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        assert_contains_substring(&code, "#![no_std]");
        assert_contains_substring(&code, "#![no_main]");
        assert_contains_substring(&code, "use cortex_m_rt::entry;");
        assert_contains_substring(&code, "#[entry]");
        assert_contains_substring(&code, "fn main() -> !");
    }

    #[test]
    fn test_fresh_main_rs_stm32_ends_with_user_tail() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        assert_contains_substring(&code, USER_TAIL);
        let trimmed_code = code.trim_end();
        let trimmed_tail = USER_TAIL.trim_end();
        assert!(
            trimmed_code.ends_with(trimmed_tail),
            "Code should end with USER_TAIL"
        );
    }

    #[test]
    fn test_fresh_main_rs_stm32_contains_preamble_before_gen() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        let gen_start = code.find(GEN_BEGIN).expect("GEN_BEGIN not found");
        let preamble = &code[..gen_start];

        // Should have some content before gen section
        assert!(preamble.len() > 0);
        assert!(preamble.contains("#!") || preamble.contains("use"));
    }

    // ── Update Code Generation Tests (via Mcu::update_main_rs) ───────────────

    #[test]
    fn test_update_main_rs_preserves_user_code() {
        use super::super::mock_mcu;

        let before = "// preamble\n// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD_GEN\n// <<< GENERATED END >>>\nloop { eprintln!(\"Custom\"); }";
        let mcu = mock_mcu::create_stm32f103c8tx();
        let after = mcu.update_main_rs(before);

        assert_contains_substring(&after, "eprintln!(\"Custom\")");
        assert_not_contains_substring(&after, "OLD_GEN");
    }

    #[test]
    fn test_update_main_rs_produces_valid_markers() {
        use super::super::mock_mcu;

        let before = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD\n// <<< GENERATED END >>>\nloop";
        let mcu = mock_mcu::create_stm32f103c8tx();
        let after = mcu.update_main_rs(before);

        assert_eq!(count_gen_sections(&after), 1);
        assert_contains_substring(&after, GEN_BEGIN);
        assert_contains_substring(&after, GEN_END);
    }

    #[test]
    fn test_update_main_rs_idempotent() {
        use super::super::mock_mcu;

        let original = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD\n// <<< GENERATED END >>>\nloop";
        let mcu = mock_mcu::create_stm32f103c8tx();

        let first = mcu.update_main_rs(original);
        let second = mcu.update_main_rs(&first);

        assert_eq!(first, second, "update_main_rs should be idempotent");
    }

    #[test]
    fn test_update_main_rs_multiple_iterations() {
        use super::super::mock_mcu;

        let original = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD\n// <<< GENERATED END >>>\nloop";
        let mcu = mock_mcu::create_stm32f103c8tx();

        let first = mcu.update_main_rs(original);
        let second = mcu.update_main_rs(&first);

        // Second update should be identical to first (idempotent)
        assert_eq!(first, second, "Second update should be identical to first");

        // Count markers to ensure they didn't multiply
        assert_eq!(
            first.matches(GEN_BEGIN).count(),
            1,
            "Should have exactly 1 GEN_BEGIN"
        );
        assert_eq!(
            second.matches(GEN_BEGIN).count(),
            1,
            "Should have exactly 1 GEN_BEGIN"
        );
    }

    #[test]
    fn test_update_main_rs_handles_multiline_user_code() {
        use super::super::mock_mcu;

        let before = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD\n// <<< GENERATED END >>>\nloop {\n    eprintln!(\"Line 1\");\n    eprintln!(\"Line 2\");\n}";
        let mcu = mock_mcu::create_stm32f103c8tx();
        let after = mcu.update_main_rs(before);

        assert_contains_substring(&after, "eprintln!(\"Line 1\")");
        assert_contains_substring(&after, "eprintln!(\"Line 2\")");
    }

    // ── Round-Trip Tests (Parse → Generate → Parse) ──────────────────────────

    #[test]
    fn test_round_trip_generated_then_parsed() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        // Parse what we generated
        let parsed = parse_main_rs(&code);

        // Since we didn't configure pins, should be empty
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_update_preserves_gen_section_integrity() {
        use super::super::mock_mcu;

        let before = "// <<< GENERATED BEGIN — do not edit between these markers >>>\nOLD\n// <<< GENERATED END >>>\nloop";
        let mcu = mock_mcu::create_stm32f103c8tx();
        let after = mcu.update_main_rs(before);

        // After update, should still have exactly one marker pair
        let begin_count = after.matches(GEN_BEGIN).count();
        let end_count = after.matches(GEN_END).count();

        assert_eq!(begin_count, 1);
        assert_eq!(end_count, 1);
    }

    // ── ADC init is a plain inline line (no helper fn) ───────────────────────

    #[test]
    fn test_adc_init_is_a_single_inline_line() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        // PA0 (pin 10) supports ADC1 IN0.
        mcu.apply_pin_function(10, PinFunction::AdcChannel { adc: 1, channel: 0 });
        let code = mcu.fresh_main_rs();

        // One plain line inside the GENERATED block — `Clocks` passed BY VALUE.
        assert_contains_substring(&code, "let mut _adc1 = adc::Adc::adc1(dp.ADC1, clocks);");
        assert_not_contains_substring(&code, "clocks: &Clocks)");
        // …and no helper fn is appended after `fn main` any more.
        assert_not_contains_substring(&code, "fn init_adc1");
        assert_not_contains_substring(&code, "── Peripheral init helpers");

        let init_pos = code.find("adc::Adc::adc1(").unwrap();
        let gen_end = code.find(GEN_END).unwrap();
        assert!(init_pos < gen_end, "ADC init must be inside the GEN block");
    }

    #[test]
    fn test_adc_init_not_duplicated_on_update() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::AdcChannel { adc: 1, channel: 0 });

        let blank = "// <<< GENERATED BEGIN — do not edit between these markers >>>\n\
                     OLD\n\
                     // <<< GENERATED END >>>\n    loop {}\n}\n";
        let once = mcu.update_main_rs(blank);
        assert_not_contains_substring(&once, "fn init_adc1");

        let twice = mcu.update_main_rs(&once);
        assert_eq!(
            twice.matches("adc::Adc::adc1(").count(),
            1,
            "ADC init duplicated"
        );
        assert_eq!(once, twice, "update must be idempotent");
    }

    #[test]
    fn test_generic_helpers_not_duplicated_on_repeated_update() {
        use super::super::mock_mcu;

        // SPI1 (PA5 = pin 15 → SpiSck(1); partners auto-assign MISO/MOSI) and
        // I2C1 (PB6 = pin 42 → I2cScl(1); PB7 → I2cSda(1)) both emit GENERIC
        // helpers `fn init_spi1<PINS>(` / `fn init_i2c1<PINS>(`. These were the
        // ones that grew without bound.
        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(15, PinFunction::SpiSck(1));
        mcu.apply_pin_function(42, PinFunction::I2cScl(1));
        mcu.apply_pin_function(43, PinFunction::I2cSda(1));

        // Init now lives in `src/pins/configs/`; main.rs only calls into it.
        let mut code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "pins::configs::spi1::init(");
        assert_contains_substring(&code, "pins::configs::i2c1::init(");

        // Re-run the per-frame regeneration many times — the calls aren't
        // duplicated in main.rs and each config file has one `pub fn init`.
        for _ in 0..10 {
            code = mcu.update_main_rs(&code);
        }
        assert_eq!(
            code.matches("pins::configs::spi1::init(").count(),
            1,
            "SPI call duplicated"
        );
        assert_eq!(
            code.matches("pins::configs::i2c1::init(").count(),
            1,
            "I2C call duplicated"
        );
        let cfgs = mcu.config_files();
        let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
        let i2c1 = &cfgs.iter().find(|(n, _)| n == "i2c1/mod.rs").unwrap().1;
        assert_eq!(spi1.matches("pub fn init").count(), 1);
        assert_eq!(i2c1.matches("pub fn init").count(), 1);
    }

    /// Pins wired straight to the generator, bypassing `apply_pin_function`.
    ///
    /// Not to dodge the auto-wiring (that picks one pad group correctly now —
    /// see `spi1_config_follows_the_pin_the_user_assigned`), but to reach states
    /// it will never produce: instance 2, and a bus with a signal left unwired.
    fn bus_configs(wiring: &[(&str, PinFunction)]) -> Vec<(String, String)> {
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use std::collections::BTreeMap;

        let pins: Vec<Pin> = wiring
            .iter()
            .map(|(name, f)| {
                let mut p = Pin::new(1, name);
                p.selected_function = f.clone();
                p
            })
            .collect();
        let refs: Vec<&Pin> = pins.iter().collect();
        super::stm32::config_files(
            &refs,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &crate::panels::mcu_module::clock::ClockConfig::None,
            true,
        )
    }

    /// SPI and I2C keep their pins inside the handle's own HAL type, so naming
    /// what `init` returns means naming the pins. The aliases that do it live in
    /// the GENERATED block — only that block is re-spliced when the user rewires
    /// a peripheral, and a stale pin type is a type error, not a stale comment.
    #[test]
    fn spi_and_i2c_name_their_handle_with_concrete_pin_types() {
        // SPI1 on its default pins + I2C1 on PB6/PB7.
        let cfgs = bus_configs(&[
            ("PA5", PinFunction::SpiSck(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ]);
        let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
        let i2c1 = &cfgs.iter().find(|(n, _)| n == "i2c1/mod.rs").unwrap().1;

        // The pin types are the ones the `into_*` calls in main.rs produce.
        assert_contains_substring(spi1, "hal_gpio::PA5<hal_gpio::Alternate>,");
        assert_contains_substring(spi1, "hal_gpio::PA6<hal_gpio::Input<hal_gpio::Floating>>,");
        assert_contains_substring(spi1, "hal_gpio::PA7<hal_gpio::Alternate>,");
        assert_contains_substring(spi1, "pub type SpiRemap = hal_spi::Spi1NoRemap;");
        assert_contains_substring(
            spi1,
            "pub type Handle = SpiBusIo<Spi<pac::SPI1, SpiRemap, SpiPins, u8>>;",
        );
        assert_contains_substring(spi1, ") -> Handle {");
        // No `impl Trait` left: an RTIC resource cannot name one.
        assert_not_contains_substring(spi1, "-> impl embedded_hal::spi::SpiBus");

        assert_contains_substring(
            i2c1,
            "hal_gpio::PB6<hal_gpio::Alternate<hal_gpio::OpenDrain>>,",
        );
        assert_contains_substring(
            i2c1,
            "hal_gpio::PB7<hal_gpio::Alternate<hal_gpio::OpenDrain>>,",
        );
        assert_contains_substring(
            i2c1,
            "pub type Handle = I2cIo<BlockingI2c<pac::I2C1, I2cPins>>;",
        );
        assert_not_contains_substring(i2c1, "-> impl embedded_hal::i2c::I2c");

        // Both aliases sit INSIDE the auto-updated block.
        for (body, alias) in [(spi1, "pub type SpiPins"), (i2c1, "pub type I2cPins")] {
            let gen_end = body.find(GEN_END).expect("marker");
            assert!(
                body.find(alias).expect("alias") < gen_end,
                "{alias} must be regenerated with the pin map:\n{body}"
            );
        }
    }

    /// PA15/PB3/PB4 come out of reset as the JTAG port — typed
    /// `Pin<'B', 3, Debugger>`, a state with NO `into_*` method — so a project
    /// touching any of them did not compile at all. `disable_jtag` is the way
    /// out; it takes all three at once and hands them back as plain pins.
    #[test]
    fn jtag_pins_are_released_before_they_are_configured() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use std::collections::BTreeMap;

        let section = |pins: &[(&str, PinFunction)]| {
            let owned: Vec<Pin> = pins
                .iter()
                .map(|(name, f)| {
                    let mut p = Pin::new(1, name);
                    p.selected_function = f.clone();
                    p
                })
                .collect();
            let refs: Vec<&Pin> = owned.iter().collect();
            super::stm32::make_generated_section(
                "STM32F103",
                &refs,
                &ClockConfig::None,
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                &BTreeMap::new(),
                true,
                "",
            )
        };

        // One JTAG pin in use pulls the whole call in, and the pins the project
        // does not use are underscored rather than left looking forgotten.
        // PA15/PB4 are listed as Unset because the call needs all three to EXIST
        // on the chip — a definition missing one gets no `disable_jtag` at all.
        let code = section(&[
            ("PB3", PinFunction::GpioOutput),
            ("PA15", PinFunction::Unset),
            ("PB4", PinFunction::Unset),
        ]);
        assert_contains_substring(
            &code,
            "let (_pa15, pb3, _pb4) = afio.mapr.disable_jtag(gpioa.pa15, gpiob.pb3, gpiob.pb4);",
        );
        // The pin now binds from that value, not from the port it left.
        assert_contains_substring(
            &code,
            "let mut pb3_out = pb3.into_push_pull_output(&mut gpiob.crl);",
        );
        assert_not_contains_substring(&code, "gpiob.pb3.into_");
        // Port A is split only to hand PA15 over — nothing writes through it.
        assert_contains_substring(&code, "let gpioa = dp.GPIOA.split();");
        assert_contains_substring(&code, "let mut gpiob = dp.GPIOB.split();");
        // `disable_jtag` lives on the AFIO, so a project that needed no AFIO
        // before needs one now.
        assert_contains_substring(&code, "let mut afio = dp.AFIO.constrain();");

        // A project clear of those three pins is untouched by any of it.
        let code = section(&[
            ("PC13", PinFunction::GpioOutput),
            ("PA15", PinFunction::Unset),
            ("PB3", PinFunction::Unset),
            ("PB4", PinFunction::Unset),
        ]);
        assert_not_contains_substring(&code, "disable_jtag");
        assert_not_contains_substring(&code, "dp.GPIOA.split()");
    }

    /// End to end, through the click path the user actually takes: assigning one
    /// SPI1 signal auto-wires the partners, and the config module's pin types
    /// must be the group that pin belongs to. This used to come out as (PA5,
    /// PB4, PB5) — a set the hardware cannot form and rustc rejects.
    #[test]
    fn spi1_config_follows_the_pin_the_user_assigned() {
        use super::super::mock_mcu;

        for (pin, group) in [
            (15, ["PA5", "PA6", "PA7"]), // the default set
            (39, ["PB3", "PB4", "PB5"]), // the remap set
        ] {
            let mut mcu = mock_mcu::create_stm32f103c8tx();
            mcu.apply_pin_function(pin, PinFunction::SpiSck(1));
            let cfgs = mcu.config_files();
            let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
            for name in group {
                assert_contains_substring(spi1, &format!("hal_gpio::{name}<"));
            }
        }
    }

    /// SPI1 on PB3/PB4/PB5 is the SAME peripheral through different pads, and
    /// the HAL takes the remap register bit from the type. Naming NoRemap there
    /// would configure the pins the project does not use.
    #[test]
    fn spi1_on_its_alternate_pins_names_the_remap_type() {
        let cfgs = bus_configs(&[
            ("PB3", PinFunction::SpiSck(1)),
            ("PB4", PinFunction::SpiMiso(1)),
            ("PB5", PinFunction::SpiMosi(1)),
        ]);
        let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
        assert_contains_substring(spi1, "pub type SpiRemap = hal_spi::Spi1Remap;");
        assert_contains_substring(spi1, "hal_gpio::PB3<hal_gpio::Alternate>,");

        // SPI2's own pins are on port B too — that is not a remap.
        let cfgs = bus_configs(&[
            ("PB13", PinFunction::SpiSck(2)),
            ("PB14", PinFunction::SpiMiso(2)),
            ("PB15", PinFunction::SpiMosi(2)),
        ]);
        let spi2 = &cfgs.iter().find(|(n, _)| n == "spi2.rs").unwrap().1;
        assert_contains_substring(spi2, "pub type SpiRemap = hal_spi::Spi2NoRemap;");
    }

    /// In stm32f1xx-hal 0.10 only the FIRST SPI/I2C takes the AFIO remap
    /// register: `Spi::spi2` / `BlockingI2c::i2c2` have no `mapr` parameter, so
    /// passing one is an arity error and NO project on instance 2 ever built.
    /// main.rs's call shape is the same for every instance, so the `init`
    /// parameter stays — underscored where the HAL cannot take it.
    #[test]
    fn only_the_first_spi_and_i2c_pass_the_afio_register() {
        let one = bus_configs(&[
            ("PA5", PinFunction::SpiSck(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ]);
        let two = bus_configs(&[
            ("PB13", PinFunction::SpiSck(2)),
            ("PB14", PinFunction::SpiMiso(2)),
            ("PB15", PinFunction::SpiMosi(2)),
            ("PB10", PinFunction::I2cScl(2)),
            ("PB11", PinFunction::I2cSda(2)),
        ]);
        let body = |files: &[(String, String)], name: &str| {
            files
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("no {name}"))
                .1
                .clone()
        };

        let spi1 = body(&one, "spi1.rs");
        assert_contains_substring(&spi1, "Spi::spi1(spi, pins, &mut afio.mapr, get_mode()");
        assert_contains_substring(&spi1, "    afio: &mut afio::Parts,");
        let i2c1 = body(&one, "i2c1/mod.rs");
        assert_contains_substring(
            &i2c1,
            "BlockingI2c::i2c1(i2c, pins, &mut afio.mapr, get_mode()",
        );

        let spi2 = body(&two, "spi2.rs");
        assert_contains_substring(&spi2, "Spi::spi2(spi, pins, get_mode()");
        // The parameter is still there — main.rs passes it — but unused.
        assert_contains_substring(&spi2, "    _afio: &mut afio::Parts,");
        assert_not_contains_substring(&spi2, "afio.mapr");
        let i2c2 = body(&two, "i2c2/mod.rs");
        assert_contains_substring(&i2c2, "BlockingI2c::i2c2(i2c, pins, get_mode()");
        assert_contains_substring(&i2c2, "    _afio: &mut afio::Parts,");
        assert_not_contains_substring(&i2c2, "afio.mapr");
    }

    /// A signal the user never wired gets the HAL's own placeholder — in the
    /// alias AND in main.rs's call, which used to pass a binding (`_miso1`) that
    /// nothing declared.
    #[test]
    fn an_unwired_spi_signal_becomes_the_hal_placeholder() {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::pins::logic::pin::Pin;
        use std::collections::BTreeMap;

        // SCK alone — no MISO, no MOSI.
        let mut sck = Pin::new(1, "PA5");
        sck.selected_function = PinFunction::SpiSck(1);
        let pins: Vec<&Pin> = vec![&sck];

        let code = super::stm32::make_generated_section(
            "STM32F103",
            &pins,
            &ClockConfig::None,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            true,
            "",
        );
        assert_contains_substring(&code, "stm32f1xx_hal::spi::NoMiso");
        assert_not_contains_substring(&code, "_miso1");

        let cfgs = bus_configs(&[("PA5", PinFunction::SpiSck(1))]);
        let spi1 = &cfgs.iter().find(|(n, _)| n == "spi1.rs").unwrap().1;
        assert_contains_substring(spi1, "hal_spi::NoMiso,");
        assert_contains_substring(spi1, "hal_spi::NoMosi,");
    }

    #[test]
    fn test_can_emits_config_file_and_call() {
        use super::super::mock_mcu;

        // CAN1 on PA11 (RX, pin 32) + PA12 (TX, pin 33). Codegen must emit a
        // `can1.rs` config module and a single call into it from main.rs, with
        // pins ordered (TX, RX) as bxcan/assign_pins expect.
        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(32, PinFunction::CanRx);
        mcu.apply_pin_function(33, PinFunction::CanTx);

        let mut code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "pins::configs::can1::init(dp.CAN1,");
        assert_contains_substring(&code, "&mut afio);");
        // CAN needs AFIO constrained.
        assert_contains_substring(&code, "let mut afio = dp.AFIO.constrain();");

        // The call isn't duplicated across repeated per-frame regeneration.
        for _ in 0..10 {
            code = mcu.update_main_rs(&code);
        }
        assert_eq!(
            code.matches("pins::configs::can1::init(").count(),
            1,
            "CAN call duplicated"
        );

        // The config file carries the GENERATED constants + one editable `init`.
        let cfgs = mcu.config_files();
        let can1 = &cfgs.iter().find(|(n, _)| n == "can1.rs").unwrap().1;
        assert_contains_substring(can1, "const BITRATE: u32 =");
        assert_contains_substring(can1, "const BTR: u32 =");
        assert_eq!(can1.matches("pub fn init").count(), 1);
    }

    #[test]
    fn test_usb_emits_cdc_serial_init_and_uses() {
        use super::super::mock_mcu;

        // USB on PA11 (D-, pin 32) + PA12 (D+, pin 33). Codegen must emit the
        // CDC serial setup in main + the external-crate `use`s.
        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(32, PinFunction::UsbDm);
        mcu.apply_pin_function(33, PinFunction::UsbDp);

        let mut code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "use usb_device::prelude::*;");
        assert_contains_substring(&code, "use usbd_serial::{SerialPort, USB_CLASS_CDC};");
        assert_contains_substring(&code, "let usb_bus = UsbBus::new(usb_periph);");
        assert_contains_substring(&code, "UsbVidPid(0x16c0, 0x27dd)");
        assert_contains_substring(&code, ".device_class(USB_CLASS_CDC)");
        // D+ reset trick + HAL usb import present.
        assert_contains_substring(&code, "usb_dp.set_low();");
        assert_contains_substring(&code, "usb::{Peripheral, UsbBus}");

        // The block isn't duplicated across repeated per-frame regeneration.
        for _ in 0..10 {
            code = mcu.update_main_rs(&code);
        }
        assert_eq!(
            code.matches("let usb_bus = UsbBus::new").count(),
            1,
            "USB init duplicated"
        );
    }

    #[test]
    fn test_legacy_helper_is_stripped_on_update() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::AdcChannel { adc: 1, channel: 0 });

        // A file written by an older version: helper appended after `fn main`.
        let legacy = "// <<< GENERATED BEGIN — do not edit between these markers >>>\n\
                      OLD\n\
                      // <<< GENERATED END >>>\n    loop {}\n}\n\n\
                      // ── Peripheral init helpers (editable — tweak as needed) ──\n\n\
                      fn init_adc1(adc1: pac::ADC1, clocks: Clocks) -> adc::Adc<pac::ADC1> {\n\
                          let mut a = adc::Adc::adc1(adc1, clocks);\n\
                          a.set_sample_time(adc::SampleTime::T_239);\n\
                          a\n}\n";
        let out = mcu.update_main_rs(legacy);

        // Helper and its orphaned section header are gone; user code before it
        // (the `loop {}` body of main) is untouched.
        assert_not_contains_substring(&out, "fn init_adc1");
        assert_not_contains_substring(&out, "set_sample_time");
        assert_not_contains_substring(&out, "── Peripheral init helpers");
        assert_contains_substring(&out, "loop {}");
        assert_contains_substring(&out, "let mut _adc1 = adc::Adc::adc1(dp.ADC1, clocks);");
    }

    // ── Clock codegen Tests ───────────────────────────────────────────────────

    #[test]
    fn test_default_clock_chain_matches_legacy() {
        use super::super::mock_mcu;

        let mcu = mock_mcu::create_stm32f103c8tx();
        let code = mcu.fresh_main_rs();

        // Default 72 MHz config must reproduce the original hardcoded chain…
        assert_contains_substring(&code, ".use_hse(8.MHz())");
        assert_contains_substring(&code, ".sysclk(72.MHz())");
        assert_contains_substring(&code, ".pclk1(36.MHz())");
        // …and omit knobs that match the HAL defaults (ahb/1, apb2/1, adc/6).
        assert_not_contains_substring(&code, ".hclk(");
        assert_not_contains_substring(&code, ".pclk2(");
        assert_not_contains_substring(&code, ".adcclk(");
    }

    /// An imported `ClockConfig::Graph` drives the generated `rcc.cfgr` chain —
    /// not the hardcoded default — closing the data-driven loop.
    #[test]
    fn test_graph_clock_drives_generated_chain() {
        use super::super::clock::graph::layout::stm32f1_layout;
        use super::super::clock::graph::{GraphClock, stm32f1_graph};
        use super::super::clock::model::{Stm32f1Clock, SysclkSrc};
        use super::super::clock::{ClockConfig, ClockLimits};
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        // HSI-direct config carried as a graph → SYSCLK 8 MHz (not the default 72).
        let mut cfg = Stm32f1Clock::default();
        cfg.sysclk_src = SysclkSrc::Hsi;
        mcu.clock = ClockConfig::Graph(GraphClock {
            graph: stm32f1_graph(&cfg),
            layout: stm32f1_layout(&ClockLimits::default()),
            bindings: Default::default(),
        });

        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, ".sysclk(8.MHz())");
        assert_not_contains_substring(&code, ".sysclk(72.MHz())");
    }

    /// Build a graph clock from a typed config (the only runtime clock model).
    fn graph_clock(
        cfg: &super::super::clock::model::Stm32f1Clock,
    ) -> super::super::clock::ClockConfig {
        use super::super::clock::graph::{GraphClock, layout::stm32f1_layout, stm32f1_graph};
        use super::super::clock::{ClockConfig, ClockLimits};
        ClockConfig::Graph(GraphClock {
            graph: stm32f1_graph(cfg),
            layout: stm32f1_layout(&ClockLimits::default()),
            bindings: Default::default(),
        })
    }

    #[test]
    fn test_modified_clock_emits_extra_knobs() {
        use super::super::clock::model::Stm32f1Clock;
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        let mut cfg = Stm32f1Clock::default();
        cfg.ahb_pre = 2; // HCLK = 36 → emits .hclk(...)
        cfg.apb2_pre = 2; // PCLK2 = 18 → emits .pclk2(...)
        mcu.clock = graph_clock(&cfg);
        let code = mcu.fresh_main_rs();

        assert_contains_substring(&code, ".hclk(");
        assert_contains_substring(&code, ".pclk2(");
    }

    #[test]
    fn test_clock_marker_roundtrips_through_mcu_config() {
        use super::super::clock::ClockConfig;
        use super::super::clock::graph::graph_to_stm32f1;
        use super::super::clock::model::Stm32f1Clock;
        use super::super::mcu_config;
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        let mut cfg = Stm32f1Clock::default();
        cfg.pll_mul = 6;
        cfg.apb2_pre = 2;
        cfg.adc_pre = 8;
        mcu.clock = graph_clock(&cfg);

        // The clock is persisted in mcu.config (no longer a marker in main.rs),
        // and parses back to the graph's effective config.
        assert!(
            !mcu.fresh_main_rs().contains("@clock"),
            "no marker in main.rs"
        );
        let (_, parsed) = mcu_config::parse(&mcu.mcu_config_text());
        let ClockConfig::Graph(gc) = &mcu.clock else {
            panic!("expected graph clock")
        };
        assert_eq!(parsed, Some(graph_to_stm32f1(&gc.graph)));
    }

    #[test]
    fn test_hsi_preset_omits_use_hse() {
        use super::super::clock::model::{Stm32f1Clock, SysclkSrc};
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        // HSI-only: no crystal → no `.use_hse(...)`, SYSCLK = 8 MHz.
        mcu.clock = graph_clock(&Stm32f1Clock {
            hse_enabled: false,
            sysclk_src: SysclkSrc::Hsi,
            ..Stm32f1Clock::default()
        });
        let code = mcu.fresh_main_rs();

        assert_not_contains_substring(&code, ".use_hse(");
        assert_contains_substring(&code, ".sysclk(8.MHz())");
    }

    // ── Variable naming: `<pin>_<type>` bindings ─────────────────────────────

    /// The generated `let` binding carries the type suffix (`pa0_out`), while
    /// the HAL field access stays the bare pin (`gpioa.pa0`).
    #[test]
    fn test_binding_uses_pin_type_format_gpio() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        // PA0 (pin 10) → GPIO output.
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        let code = mcu.fresh_main_rs();

        // GPIO out is wrapped in the eh-1.0 bridge (portable seam), keeping the
        // `&mut` + var-name shape so the round-trip parsers still work.
        assert_contains_substring(
            &code,
            "let mut pa0_out = pins::configs::io::DigitalOut(gpioa.pa0.into_push_pull_output",
        );
        // The bare-field form must NOT be used for the binding name.
        assert_not_contains_substring(&code, "let pa0 = &mut gpioa.pa0");
    }

    /// A Custom module generates `configs/custom_<name>.rs` holding a struct with
    /// one field per pin and a `new(...)` in the same order — generic over each
    /// pin type, so no HAL type is baked in.
    #[test]
    fn custom_module_generates_generic_struct_file() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        mcu.apply_pin_function(11, PinFunction::GpioInput); // PA1

        assert!(
            mcu.add_module(ModuleKind::Custom),
            "custom module always addable"
        );
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "temp sensor".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10, 11];
            c.applied_pins = c.pins.clone(); // what the Update button does
        }

        let files = mcu.config_files();
        let (name, body) = files
            .iter()
            .find(|(n, _)| n.starts_with("custom_"))
            .expect("custom config file generated");
        assert_eq!(name, "custom_temp_sensor.rs");

        // Struct + impl, generic over one type parameter per pin.
        assert_contains_substring(body, "pub struct TempSensor<PA0, PA1>");
        assert_contains_substring(body, "pub pa0_out: PA0,");
        assert_contains_substring(body, "pub pa1_in: PA1,");
        assert_contains_substring(body, "TempSensor<PA0, PA1>");
        assert_contains_substring(body, "pub fn new(pa0_out: PA0, pa1_in: PA1) -> Self");
        // No HAL type is named — that is what makes it work on every family.
        assert_not_contains_substring(body, "stm32f1xx_hal");
        // The whole file is the user's: no region that regeneration owns.
        assert_not_contains_substring(body, "GENERATED");
    }

    /// Editing the pin list is a DRAFT: nothing is generated until Update
    /// commits it (so a half-finished edit never rewrites the user's file), and
    /// an explicit struct name wins over the module name.
    #[test]
    fn pins_are_staged_until_update_and_struct_name_is_explicit() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "panel".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10]; // drafted, NOT applied
        }

        // Draft only → no file, no instantiation.
        assert!(
            mcu.config_files()
                .iter()
                .all(|(n, _)| !n.starts_with("custom_"))
        );
        assert!(mcu.custom_module_inits().is_empty());

        // Update commits it.
        if let Some(ModuleConfig::Custom(c)) = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .map(|m| &mut m.config)
        {
            // The signature is what the panel computes from the live pins; an
            // empty one here just exercises the list comparison.
            assert!(c.has_pending_pins(""), "the button must be offered");
            c.applied_pins = c.pins.clone();
            assert!(!c.has_pending_pins(""));
        }
        assert!(
            mcu.config_files()
                .iter()
                .any(|(n, _)| n == "custom_panel.rs")
        );

        // An explicit struct name overrides the module-derived one.
        if let Some(ModuleConfig::Custom(c)) = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .map(|m| &mut m.config)
        {
            c.struct_name = "encoder nav".to_owned();
        }
        let files = mcu.config_files();
        let (_, body) = files
            .iter()
            .find(|(n, _)| n.starts_with("custom_"))
            .unwrap();
        assert_contains_substring(body, "pub struct EncoderNav<PA0>");
        assert_contains_substring(&mcu.custom_module_inits(), "EncoderNav::new(");
    }

    /// Every Update writes a NEW file (`custom_x.rs` -> `_1` -> `_2`), and both
    /// the generated file name and main.rs's call follow the current revision —
    /// so a regenerated struct never has to be merged into the previous file.
    #[test]
    fn each_update_targets_a_new_revision_file() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "menu".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10];
            c.applied_pins = c.pins.clone(); // first Update: revision stays 0
        }
        let files = mcu.config_files();
        assert!(
            files.iter().any(|(n, _)| n == "custom_menu.rs"),
            "{files:?}"
        );
        assert_contains_substring(&mcu.custom_module_inits(), "pins::configs::custom_menu::");

        // Second Update (a pin was added) → revision 1 → a fresh file.
        mcu.apply_pin_function(11, PinFunction::GpioInput);
        if let Some(ModuleConfig::Custom(c)) = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .map(|m| &mut m.config)
        {
            c.pins = vec![10, 11];
            c.revision += 1;
            c.applied_pins = c.pins.clone();
        }
        let files = mcu.config_files();
        assert!(
            files.iter().any(|(n, _)| n == "custom_menu_1.rs"),
            "{files:?}"
        );
        // Only the CURRENT revision is emitted, so `configs/mod.rs` declares one
        // module and the build can never see two copies of the struct.
        assert_eq!(
            files
                .iter()
                .filter(|(n, _)| n.starts_with("custom_"))
                .count(),
            1
        );
        assert_contains_substring(&mcu.custom_module_inits(), "pins::configs::custom_menu_1::");
        assert_not_contains_substring(&mcu.custom_module_inits(), "custom_menu::");
    }

    /// A module named `flash` must not shadow the flash store's `flash`
    /// binding; its file keeps the name, only the `let` moves.
    #[test]
    fn a_custom_module_never_takes_a_name_main_binds_itself() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "Flash".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10];
            c.applied_pins = c.pins.clone();
        }
        let inits = mcu.custom_module_inits();
        assert_contains_substring(&inits, "let mut flash_dev = pins::configs::custom_flash::");
        assert_not_contains_substring(&inits, "let mut flash =");
        assert!(
            mcu.config_files()
                .iter()
                .any(|(n, _)| n == "custom_flash.rs")
        );
        // The slot titles the modules itself, right above their lines.
        let slot = mcu.watchdog_and_custom_inits();
        assert_contains_substring(&slot, "    // ── Custom modules ──\n    let mut flash_dev");
        mcu.modules.clear();
        assert_not_contains_substring(&mcu.watchdog_and_custom_inits(), "Custom modules");
    }

    /// The custom file carries NO GENERATED markers. With them, every
    /// regeneration re-spliced the struct and `new()` — including the one on the
    /// first frame after the IDE starts — so a field the user added vanished on
    /// every launch. A pin change reaches the code through Update, which writes a
    /// new revision file, never by rewriting this one.
    #[test]
    fn custom_file_has_no_generated_markers() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "demo".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10];
            c.applied_pins = c.pins.clone();
        }
        let files = mcu.config_files();
        let (_, body) = files
            .iter()
            .find(|(n, _)| n.starts_with("custom_"))
            .unwrap();

        assert_not_contains_substring(body, "<<< GENERATED");
        assert_not_contains_substring(body, "GENERATED END");
        // The peripheral config files keep theirs — only the custom one changed.
        let other = files.iter().find(|(n, _)| n == "io.rs");
        if let Some((_, io)) = other {
            assert!(io.contains("// <<< GENERATED>>>"));
        }
    }

    /// main.rs gets the instantiation line, using the REAL binding names (which
    /// carry the pin's user label) and placed after the bindings it consumes.
    #[test]
    fn custom_module_is_instantiated_in_main() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        mcu.apply_pin_function(11, PinFunction::GpioInput); // PA1
        // A user label must be reflected in the ARGUMENT (the real variable),
        // while the struct field stays label-free.
        if let Some(p) = mcu.find_pin_mut(10) {
            p.custom_label = "led".to_owned();
        }
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "panel".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10, 11];
            c.applied_pins = c.pins.clone(); // what the Update button does
        }

        let code = mcu.fresh_main_rs();
        assert_contains_substring(
            &code,
            "let mut panel = pins::configs::custom_panel::Panel::new(pa0_out_led, pa1_in);",
        );
        // …and it comes AFTER the bindings it consumes.
        let bind = code.find("let pa0_out_led").expect("binding emitted");
        let init = code.find("Panel::new").unwrap();
        assert!(bind < init, "instantiation must follow the bindings");
    }

    /// The instantiation lines must reach EVERY backend's generated section, not
    /// just the STM32F1 blocking one — async (embassy) and ESP32-C3 too.
    #[test]
    fn custom_inits_are_appended_by_every_backend_section() {
        use super::super::mock_mcu;
        use super::{embassy_async, embassy_common};
        use crate::panels::mcu_module::pins::logic::pin::Pin;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        let pins: Vec<&Pin> = mcu.iter_all_pins().collect();
        let line = "    let mut demo = pins::configs::custom_demo::Demo::new(pa0_out);
";

        // Blocking embassy (non-F1 STM32).
        let blocking = embassy_common::make_generated_section("X", &pins, "", line);
        assert!(
            blocking.contains(line),
            "embassy blocking:
{blocking}"
        );
        // The heading comes with the lines (`Mcu::watchdog_and_custom_inits`),
        // not from the section: one put above the whole slot titled the
        // watchdogs and the flash store "Custom modules" too.
        assert!(!blocking.contains("Custom modules"), "{blocking}");

        // Async embassy.
        let async_ = embassy_async::make_generated_section("X", &pins, "", "", "", line, &[]);
        assert!(
            async_.contains(line),
            "embassy async:
{async_}"
        );

        // ESP32-C3.
        let esp = crate::panels::mcu_module::codegen_esp::fresh_esp32c3_main_rs(
            &pins,
            &mcu.clock,
            "ESP32-C3",
            "esp32c3",
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            line,
            "",
            "esp32c3",
            crate::panels::mcu_module::codegen_esp::EspRuntime::Blocking,
            None,
        );
        assert!(
            esp.contains(line),
            "esp:
{esp}"
        );

        // In every case the line sits INSIDE the generated block.
        for code in [&blocking, &async_, &esp] {
            let b = code.find(GEN_BEGIN).expect("begin");
            let e = code.find(GEN_END).expect("end");
            let at = code.find(line).expect("line");
            assert!(b < at && at < e, "must be inside the GEN block");
        }
    }

    /// A pin owned by a peripheral init is MOVED there, so it can't also go into
    /// a custom struct — emit an explanation instead of code that won't compile.
    #[test]
    fn custom_module_with_peripheral_pin_emits_a_note_not_broken_code() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        // A USART TX pin is consumed by `pins::configs::usart1::init`.
        mcu.apply_pin_function(30, PinFunction::UsartTx(1));
        let usart_pin = mcu
            .iter_all_pins()
            .find(|p| matches!(p.selected_function, PinFunction::UsartTx(_)))
            .map(|p| p.number)
            .expect("a USART TX pin");

        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "mix".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10, usart_pin];
            c.applied_pins = c.pins.clone();
        }

        let inits = mcu.custom_module_inits();
        assert!(
            inits.contains("not built here"),
            "expected an explanatory note, got:
{inits}"
        );
        assert!(
            !inits.contains("Mix::new("),
            "must NOT emit a broken call:
{inits}"
        );
    }

    /// Field names must carry the pin LABEL, so they match the variables main.rs
    /// binds (`pc15_in_clk`, not `pc15_in`) — the user reported the mismatch. And
    /// the editable tail ships a ready `impl` whose bounds follow each pin's
    /// direction.
    #[test]
    fn custom_fields_carry_labels_and_tail_has_bounded_impl() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        // Two inputs + one output, each with a user label.
        for (n, f, lbl) in [
            (10usize, PinFunction::GpioInput, "clk"),
            (11, PinFunction::GpioInput, "dt"),
            (12, PinFunction::GpioOutput, "led"),
        ] {
            mcu.apply_pin_function(n, f);
            if let Some(p) = mcu.find_pin_mut(n) {
                p.custom_label = lbl.to_owned();
            }
        }
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "menu controler".to_owned();
        *m.config.custom_label_mut() = "menu controler".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.struct_name = "Encoder".to_owned();
            c.pins = vec![10, 11, 12];
            c.applied_pins = c.pins.clone();
        }

        let files = mcu.config_files();
        let (name, body) = files
            .iter()
            .find(|(n, _)| n.starts_with("custom_"))
            .unwrap();
        assert_eq!(name, "custom_menu_controler.rs");

        // Fields + `new` params carry the labels…
        // Type parameters are NAMED AFTER THE PINS (PA0, PA1, PA2).
        assert_contains_substring(body, "pub struct Encoder<PA0, PA1, PA2>");
        assert_contains_substring(body, "pub pa0_in_clk: PA0,");
        assert_contains_substring(body, "pub pa1_in_dt: PA1,");
        assert_contains_substring(body, "pub pa2_out_led: PA2,");
        assert_contains_substring(
            body,
            "pub fn new(pa0_in_clk: PA0, pa1_in_dt: PA1, pa2_out_led: PA2)",
        );
        // …and match EXACTLY what main.rs passes.
        assert_contains_substring(
            &mcu.custom_module_inits(),
            "Encoder::new(pa0_in_clk, pa1_in_dt, pa2_out_led)",
        );

        // The skeleton impl: bounds per direction + the import, after the struct.
        assert_contains_substring(body, "use embedded_hal::digital::{InputPin, OutputPin};");
        assert_contains_substring(
            body,
            "impl<PA0: InputPin, PA1: InputPin, PA2: OutputPin> Encoder<PA0, PA1, PA2>",
        );
        let strukt = body.find("pub struct Encoder").unwrap();
        assert!(
            body.find("impl<PA0: InputPin").unwrap() > strukt,
            "skeleton follows the struct"
        );
    }

    /// A custom module is user-authored, so `reconcile_modules` (which rebuilds
    /// the peripheral modules FROM the pins) must never delete it — and it keeps
    /// its wires in step with its own pin list.
    #[test]
    fn reconcile_keeps_custom_modules_and_syncs_their_wires() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::Custom));
        if let Some(m) = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
        {
            if let ModuleConfig::Custom(c) = &mut m.config {
                c.pins = vec![10, 11];
            }
        }
        // No peripheral pins at all: every derived module would be dropped here.
        mcu.reconcile_modules();
        let m = mcu
            .modules
            .iter()
            .find(|m| m.kind == ModuleKind::Custom)
            .expect("custom module survived reconcile");
        assert_eq!(m.connections.len(), 2, "wires mirror the pin list");
        assert!(m.connections.iter().all(|c| [10, 11].contains(&c.mcu_pin)));

        // Empty the pin list → wires go, module stays.
        if let Some(m) = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
        {
            if let ModuleConfig::Custom(c) = &mut m.config {
                c.pins.clear();
            }
        }
        mcu.reconcile_modules();
        let m = mcu
            .modules
            .iter()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        assert!(m.connections.is_empty());
        assert!(
            mcu.config_files()
                .iter()
                .all(|(n, _)| !n.starts_with("custom_")),
            "no file for a module with no pins"
        );
    }

    /// The Native runtime binds every GPIO raw, so no io.rs bridge is emitted
    /// and `embedded-hal` stops being needed — WITHOUT the stored GPIO choice
    /// being overwritten.
    ///
    /// It used to be overwritten, by a `normalize_gpio_api` that snapped both
    /// `gpio_api` and `pending_gpio_api` to Native so the (locked) selector
    /// would read right. That was two bugs wearing one coat. The staged half
    /// put a change into the Apply bar that nobody could have made — the GPIO
    /// cards are greyed out while Native is staged — and applying it made the
    /// applied half permanent, so switching back to Blocking afterwards left
    /// GPIO on Native for good.
    ///
    /// Nothing downstream ever needed the write: `gpio_native()` is
    /// `is_native() || gpio_api == Native`, every emitter goes through it, and
    /// the dependency scan follows the io.rs that comes out of it. The System
    /// tab derives what the locked cards show.
    #[test]
    fn the_native_runtime_binds_gpio_raw_without_eating_the_choice() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::modules::ApiStyle;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        assert_eq!(mcu.gpio_api, ApiStyle::Portable, "default");
        assert!(!mcu.gpio_native(), "Blocking + Portable: the bridge is on");
        assert!(
            mcu.config_files().iter().any(|(n, _)| n == "io.rs"),
            "so io.rs is emitted"
        );

        mcu.runtime = Runtime::Native;
        mcu.pending_runtime = Runtime::Native;

        assert!(
            mcu.gpio_native(),
            "Native binds raw whatever the field says"
        );
        assert!(
            !mcu.config_files().iter().any(|(n, _)| n == "io.rs"),
            "no io.rs on the Native runtime — so `embedded-hal` is no longer needed"
        );
        assert_eq!(
            (mcu.gpio_api, mcu.pending_gpio_api),
            (ApiStyle::Portable, ApiStyle::Portable),
            "and the choice the user made is still theirs"
        );

        // Back to Blocking: the bridge they never left comes back.
        mcu.runtime = Runtime::Blocking;
        mcu.pending_runtime = Runtime::Blocking;
        assert!(!mcu.gpio_native());
        assert!(mcu.config_files().iter().any(|(n, _)| n == "io.rs"));
    }

    /// The GPIO api toggle switches the binding shape AND the io.rs emission:
    /// Portable (default) wraps in the `pins::configs::io` bridge + emits io.rs;
    /// Native binds the raw HAL pin + emits NO io.rs.
    #[test]
    fn test_gpio_api_toggle_switches_binding_and_io_rs() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::ApiStyle;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0

        // Default (Portable) → DigitalOut bridge + io.rs config file.
        let code = mcu.fresh_main_rs();
        assert_contains_substring(
            &code,
            "pins::configs::io::DigitalOut(gpioa.pa0.into_push_pull_output",
        );
        assert!(
            mcu.config_files().iter().any(|(n, _)| n == "io.rs"),
            "io.rs emitted for Portable"
        );

        // Native GPIO → raw HAL pin, NO io.rs, NO DigitalOut.
        mcu.gpio_api = ApiStyle::Native;
        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "let mut pa0_out = gpioa.pa0.into_push_pull_output");
        assert_not_contains_substring(&code, "DigitalOut");
        assert!(
            !mcu.config_files().iter().any(|(n, _)| n == "io.rs"),
            "no io.rs on Native GPIO"
        );
    }

    /// The per-pin GPIO mode picks which `into_*` the binding is generated with,
    /// on both the Portable and the Native path — and `None` keeps the historic
    /// default, so an untouched project's code does not move.
    #[test]
    fn gpio_mode_selects_the_into_method() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::pins::logic::pin::GpioMode;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        mcu.apply_pin_function(11, PinFunction::GpioInput); // PA1

        // Defaults, unchanged from before the mode existed.
        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "gpioa.pa0.into_push_pull_output");
        assert_contains_substring(&code, "gpioa.pa1.into_floating_input");

        mcu.find_pin_mut(10).unwrap().io_mode = Some(GpioMode::OpenDrain);
        mcu.find_pin_mut(11).unwrap().io_mode = Some(GpioMode::PullUp);
        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "gpioa.pa0.into_open_drain_output");
        assert_contains_substring(&code, "gpioa.pa1.into_pull_up_input");
        assert_not_contains_substring(&code, "into_floating_input");
    }

    /// A GPIO pin is bound BY VALUE — no `&mut` of a temporary — so it can be
    /// moved into a driver. `mut` appears exactly where the pin is written
    /// through: an output always, an input only on the Portable path (where
    /// `embedded-hal` 1.0 reads through `&mut self`).
    #[test]
    fn gpio_bindings_are_owned_and_mut_only_where_written() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::ApiStyle;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        mcu.apply_pin_function(11, PinFunction::GpioInput); // PA1

        let code = mcu.fresh_main_rs();
        assert_not_contains_substring(&code, "= &mut pins::configs::io::");
        assert_contains_substring(&code, "let mut pa0_out = pins::configs::io::DigitalOut(");
        // Portable input: eh-1.0 `is_high(&mut self)` → needs `mut`.
        assert_contains_substring(&code, "let mut pa1_in = pins::configs::io::DigitalIn(");

        // Native input: the raw HAL reads through `&self`, so no `mut` (it would
        // only earn an unused-mut warning).
        mcu.gpio_api = ApiStyle::Native;
        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "let mut pa0_out = gpioa.pa0.into_push_pull_output");
        assert_contains_substring(&code, "let pa1_in = gpioa.pa1.into_floating_input");
    }

    /// A pin MOVED into a Custom module is not written through, so its binding
    /// must not be `let mut` — that would be an unused-mut warning on a line the
    /// user cannot edit.
    #[test]
    fn a_pin_moved_into_a_custom_module_is_not_mut() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu
            .modules
            .iter_mut()
            .find(|m| m.kind == ModuleKind::Custom)
            .unwrap();
        *m.config.custom_label_mut() = "panel".to_owned();
        if let ModuleConfig::Custom(c) = &mut m.config {
            c.pins = vec![10];
            c.applied_pins = vec![10]; // Update pressed
        }
        let code = mcu.fresh_main_rs();
        assert_contains_substring(&code, "Panel::new(pa0_out)");
        assert_contains_substring(&code, "let pa0_out = pins::configs::io::DigitalOut(");
        assert_not_contains_substring(&code, "let mut pa0_out");
    }

    /// Staged style choices don't affect codegen until `apply_pending_style`:
    /// editing `pending_*` sets `style_dirty` but leaves the applied fields (and
    /// therefore the generated code) alone; Apply commits them.
    #[test]
    fn test_staged_style_apply_and_dirty() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::{
            ApiStyle, AsyncBusMode, ModuleConfig, ModuleKind,
        };

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.sync_pending_style();
        assert!(!mcu.style_dirty(), "fresh project: pending == applied");

        // Stage GPIO Native → dirty, but codegen still sees Portable.
        mcu.pending_gpio_api = ApiStyle::Native;
        assert!(mcu.style_dirty());
        assert_eq!(mcu.gpio_api, ApiStyle::Portable, "applied field unchanged");
        assert!(!mcu.gpio_native(), "codegen still Portable until Apply");
        assert!(mcu.style_diff_summary().iter().any(|l| l.contains("GPIO")));

        // Apply → committed; codegen now Native; not dirty.
        mcu.apply_pending_style();
        assert!(!mcu.style_dirty());
        assert_eq!(mcu.gpio_api, ApiStyle::Native);
        assert!(mcu.gpio_native());

        // Stage a per-module api_style; dirty; config unchanged until Apply.
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        mcu.sync_pending_style();
        assert!(!mcu.style_dirty());
        let id = mcu.modules[0].id.clone();
        mcu.pending_module_styles
            .insert(id, (ApiStyle::Native, AsyncBusMode::Blocking));
        assert!(mcu.style_dirty());
        assert!(
            matches!(&mcu.modules[0].config, ModuleConfig::Usart(c) if c.api_style == ApiStyle::Portable),
            "module config unchanged until Apply"
        );
        mcu.apply_pending_style();
        assert!(
            matches!(&mcu.modules[0].config, ModuleConfig::Usart(c) if c.api_style == ApiStyle::Native),
            "Apply committed the module api_style"
        );
        assert!(!mcu.style_dirty());
    }

    /// `apply_change_list` lists not just the staged CHOICE but its concrete
    /// effects — here, staging GPIO Native removes `io.rs` from the config files.
    #[test]
    fn test_apply_change_list_shows_config_effects() {
        use super::super::mock_mcu;
        use crate::panels::mcu_module::modules::ApiStyle;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput); // PA0 → io.rs (Portable)
        mcu.sync_pending_style();
        assert!(mcu.apply_change_list().is_empty(), "nothing staged");
        assert!(
            mcu.config_files().iter().any(|(n, _)| n == "io.rs"),
            "io.rs present"
        );

        // Stage GPIO Native → the change list includes the choice + io.rs removal.
        mcu.pending_gpio_api = ApiStyle::Native;
        let list = mcu.apply_change_list();
        assert!(
            list.iter().any(|l| l.contains("GPIO In/Out")),
            "choice line: {list:?}"
        );
        assert!(
            list.iter()
                .any(|l| l.contains("io.rs") && l.contains("removed")),
            "io.rs removal listed: {list:?}"
        );
        assert!(
            list.iter().any(|l| l.contains("Cargo.toml")),
            "deps note: {list:?}"
        );
        assert!(
            list.iter().any(|l| l.contains("main.rs")),
            "main.rs note: {list:?}"
        );
    }

    /// The entry line names the entry the STAGED runtime writes - RTIC's app
    /// included, which is neither async nor `#[entry]` - and moving into RTIC
    /// says the loop below the markers goes.
    #[test]
    fn the_apply_list_names_each_runtimes_entry() {
        use super::super::mcu::Runtime;
        use super::super::mock_mcu;

        let entry = |from: Runtime, to: Runtime| {
            let mut mcu = mock_mcu::create_stm32f103c8tx();
            mcu.runtime = from;
            mcu.sync_pending_style();
            mcu.pending_runtime = to;
            mcu.apply_change_list()
        };
        let has = |list: &[String], s: &str| list.iter().any(|l| l.contains(s));

        let into_rtic = entry(Runtime::Async, Runtime::Rtic);
        assert!(
            has(&into_rtic, "main.rs entry -> #[rtic::app]"),
            "{into_rtic:?}"
        );
        assert!(
            has(&into_rtic, "RTIC generates its own idle task"),
            "{into_rtic:?}"
        );
        let into_rtic = entry(Runtime::Blocking, Runtime::Rtic);
        assert!(
            has(&into_rtic, "main.rs entry -> #[rtic::app]"),
            "{into_rtic:?}"
        );

        let out_of_rtic = entry(Runtime::Rtic, Runtime::Async);
        assert!(
            has(&out_of_rtic, "#[embassy_executor::main]"),
            "{out_of_rtic:?}"
        );
        assert!(!has(&out_of_rtic, "idle task"), "{out_of_rtic:?}");
        let out_of_rtic = entry(Runtime::Rtic, Runtime::Blocking);
        assert!(
            has(&out_of_rtic, "main.rs entry -> #[entry]"),
            "{out_of_rtic:?}"
        );

        // Blocking and Native share `#[entry]`: nothing to announce.
        let same = entry(Runtime::Blocking, Runtime::Native);
        assert!(!has(&same, "main.rs entry"), "{same:?}");
        assert!(has(&same, "main.rs regenerated"), "{same:?}");
    }

    /// A `<pin>_<type>` binding still round-trips back through `parse_main_rs`.
    #[test]
    fn test_binding_pin_type_format_roundtrips() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        let code = mcu.fresh_main_rs();

        let parsed = parse_main_rs(&code);
        assert!(
            parsed
                .iter()
                .any(|(name, func)| name == "PA0" && *func == PinFunction::GpioOutput)
        );
    }

    /// A user label on a pin is appended to the generated binding name
    /// (`pa0_out_led`) while the HAL field access stays bare (`gpioa.pa0`).
    #[test]
    fn test_custom_label_appended_to_binding() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        mcu.find_pin_mut(10).unwrap().custom_label = "Status LED".into();
        let code = mcu.fresh_main_rs();

        assert_contains_substring(
            &code,
            "let mut pa0_out_status_led = pins::configs::io::DigitalOut(gpioa.pa0.into_push_pull_output",
        );

        // The label round-trips back into the pin's custom_label on reopen.
        let labels = parse_pin_labels(&code);
        assert!(labels.contains(&("PA0".to_owned(), "status_led".to_owned())));
    }

    /// Clearing a pin (Unset) drops its custom label, so a reassigned pin
    /// starts clean.
    #[test]
    fn test_unset_clears_custom_label() {
        use super::super::mock_mcu;

        let mut mcu = mock_mcu::create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        mcu.find_pin_mut(10).unwrap().custom_label = "led".into();
        mcu.apply_pin_function(10, PinFunction::Unset);

        assert!(mcu.find_pin(10).unwrap().custom_label.is_empty());
    }
}

#[cfg(test)]
mod public_constants {
    use crate::panels::mcu_module::builtins;
    use crate::panels::mcu_module::mcu::Runtime;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    /// A generated constant the reader cannot import is a value they have to
    /// retype — the file already knows the baud rate, the frequency, the duty
    /// resolution, and the whole point of a `const` is that other code can say
    /// `usart1::BAUDRATE` instead of repeating the number.
    ///
    /// Scans REAL generated output rather than the templates, and that is the
    /// load-bearing choice: the templates come in FOUR shapes — `"const …"`,
    /// `…\nconst …`, a continuation line of a `\`-joined literal, and a
    /// `r#"…"#` block with real newlines. Three passes over the source made 106
    /// constants public and missed 60 more in the raw blocks entirely; only the
    /// generated text said so.
    fn assert_all_public(what: &str, body: &str) {
        for (n, line) in body.lines().enumerate() {
            let t = line.trim_start();
            // `const fn` is a function, not a constant, and the ones generated
            // here are helpers living in the reader's own editable zone.
            if t.starts_with("const ") && !t.starts_with("const fn ") {
                panic!("{what}:{}: generated constant is private:\n{line}", n + 1);
            }
        }
    }

    /// Give `mcu` a USART and an SPI on whatever pads can carry them, so the
    /// bus config files — the richest source of generated constants — exist.
    fn wire_two_buses(mcu: &mut crate::panels::mcu_module::mcu::Mcu) {
        let wants = [
            PinFunction::UsartTx(1),
            PinFunction::UsartRx(1),
            PinFunction::SpiSck(1),
            PinFunction::SpiMosi(1),
            PinFunction::SpiMiso(1),
        ];
        for want in wants {
            let target = mcu
                .iter_all_pins()
                .find(|p| {
                    !p.reserved
                        && p.selected_function == PinFunction::Unset
                        && p.available_functions.contains(&want)
                })
                .map(|p| p.number);
            if let Some(n) = target.and_then(|n| mcu.find_pin_mut(n)) {
                n.selected_function = want;
            }
        }
        mcu.reconcile_modules();
    }

    /// Every built-in chip, on every runtime, main.rs and every config file.
    #[test]
    fn no_generated_constant_is_private() {
        let mut files = 0;
        for def in builtins::builtin_definitions() {
            for runtime in [Runtime::Blocking, Runtime::Async] {
                let mut mcu = def.build_mcu();
                mcu.runtime = runtime;
                wire_two_buses(&mut mcu);
                assert_all_public(&format!("{} main.rs", def.id), &mcu.fresh_main_rs());
                for (name, body) in mcu.config_files() {
                    assert_all_public(&format!("{} {name}", def.id), &body);
                    files += 1;
                }
            }
        }
        assert!(
            files >= 2,
            "no bus config file was generated, so the richest source of \
             constants went unchecked"
        );
    }
}
