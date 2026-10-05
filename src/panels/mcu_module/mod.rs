#[cfg(test)]
pub(crate) mod board_notes;
pub mod builtins;
pub mod chip_filter;
pub mod chip_search;
pub mod chip_sources;
pub mod clock;
pub mod codegen;
pub mod codegen_esp;
pub mod codegen_esp_configs;
pub mod comparator;
pub mod datasheet_import;
pub mod esp_clocks;
pub mod esp_gen;
pub mod esp_metadata;
pub mod flash_store;
pub mod fpga_bitstream;
pub mod mcu;
pub mod mcu_catalog;
pub mod mcu_config;
pub mod mcu_def;
pub mod mcu_form;
pub mod mcu_identity;
pub mod mock_esp32c3;
pub mod mock_mcu;
pub mod modules;
pub mod pins;
pub mod project_gen;
pub mod registry;
pub mod ron_text;
pub mod stm32_pin_data;
pub mod structure_config;
pub mod uart_baud;
pub mod watchdog;

// ── Core types re-exports (convenience imports) ──────────────────
pub use mcu::{Mcu, PIN_HEIGHT, PIN_SPACING, PIN_WIDTH};
pub use mcu_catalog::ToolchainKind;
pub use pins::logic::{
    pin::{PIN_FONT_SIZE, PIN_ROUNDING, Pin},
    pin_function::PinFunction,
};

// ── MCU definitions / registry ──────────────────────────────────
pub use builtins::{builtin_definitions, builtin_for};
pub use mcu_def::{McuDefinition, ProjectDef};
pub use registry::{import_file, load_registry, user_mcus_dir};

// ── Factory & generated code ────────────────────────────────────
pub use mock_esp32c3::create_esp32c3;
pub use mock_mcu::create_stm32f103c8tx;
pub use project_gen::ProjectFiles;
