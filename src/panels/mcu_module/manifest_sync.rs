//! What a project's manifests follow: the crates and features in `Cargo.toml`,
//! the target and runner flags in `.cargo/config.toml`, `partitions.csv`, and
//! the flash store's reservation in `memory.x`.
//!
//! ONE decision, made here. It used to live inline in `app.rs`, and every emit
//! harness rebuilt its own subset of it by hand - so the matrix compiled the
//! manifests the HARNESSES wrote, which is not the same thing as the ones the
//! application writes. That gap already shipped once: a Pico on Async was saved
//! with the STM32 executor line for months while every row stayed green,
//! because its harness passed the right flavour by hand. Both now call
//! [`sync`], so a crate the app adds or drops is one the matrix builds with.

use super::codegen::flash_store_gen;
use super::mcu::Mcu;
use super::mcu_catalog::ToolchainKind;
use super::mcu_def::ProjectDef;
use super::pins::logic::pin_function::PinFunction;
use super::project_gen::{self, ProjectFiles};

/// The four files [`sync`] may rewrite. Each comes back unchanged when the
/// model asks nothing of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifests {
    pub cargo_toml: String,
    pub cargo_config: String,
    pub partitions_csv: String,
    pub memory_x: String,
}

impl Manifests {
    pub fn of(files: &ProjectFiles) -> Self {
        Self {
            cargo_toml: files.cargo_toml.clone(),
            cargo_config: files.cargo_config.clone(),
            partitions_csv: files.partitions_csv.clone(),
            memory_x: files.memory_x.clone(),
        }
    }

    pub fn apply_to(self, files: &mut ProjectFiles) {
        files.cargo_toml = self.cargo_toml;
        files.cargo_config = self.cargo_config;
        files.partitions_csv = self.partitions_csv;
        files.memory_x = self.memory_x;
    }
}

/// What the decision reads.
pub struct Inputs<'a> {
    pub mcu: &'a Mcu,
    /// The selected chip's build config - `mcu_def::build_cfg(def, Some(mcu))`,
    /// which already follows the runtime and the IoT tab - and its toolchain.
    /// `None` when no definition is selected: the HAL line, the target and
    /// `memory.x` are then left as they are.
    pub build: Option<(&'a ProjectDef, &'a ToolchainKind)>,
    /// `mcu.config_files()`, as the sync is about to write them.
    pub config_files: &'a [(String, String)],
    /// The generated `main.rs`.
    pub main_rs: &'a str,
    /// The user's own sources OUTSIDE `src/pins/`. A crate one of them names
    /// is never stripped, whatever the model says (a Runtime switch used to
    /// delete a hand-added `embedded-hal`) - see
    /// `project_gen::is_crate_referenced`. `src/pins/**` stays out on purpose:
    /// those files are regenerated from the model, so a bridge the IDE is
    /// about to stop emitting must not keep its crate alive (that is what left
    /// `embedded-hal` behind after switching to the Native runtime).
    pub user_sources: &'a [&'a str],
}

/// The manifests `current` becomes for this model.
pub fn sync(current: &Manifests, inp: &Inputs<'_>) -> Manifests {
    use super::modules::{ApiStyle, AsyncBusMode, ModuleConfig};
    let m = inp.mcu;
    let config_files = inp.config_files;
    let all_pins = m.all_pin_functions();
    // Peripheral config files pull in standard-trait crates (embedded-io
    // for USART, embedded-hal 1.0 `SpiBus`/`I2c` for SPI/I2C, bxcan for
    // CAN, nb for the blocking bridges). Keyed on the config files AND —
    // for the trait crates — on each module's API style: a NATIVE-style
    // USART/SPI/I2C module emits a config file but needs NO trait crate.
    let has_cfg = |p: &str| config_files.iter().any(|(name, _)| name.starts_with(p));
    // On the async (embassy) path the blocking F1 trait-bridge deps
    // (embedded-io / embedded-hal-0-2 / nb / bxcan / usb-device) DON'T
    // apply — the async USART config pulls embedded-io-async + static_cell
    // via `ensure_async_deps` instead. An F1 on Async is on embassy-stm32
    // too, so the bridges (and bxcan / usb-device) go with the HAL swap.
    let is_async = m.is_async();
    // Native runtime → the bus peripherals expose CONCRETE HAL types
    // (all `ApiStyle::Native`), so NONE of the portable trait-bridge
    // crates are pulled — same as async, for a different reason.
    let is_native = m.is_native();
    // ESP config modules speak esp-hal DIRECTLY (`Uart<'d, Blocking>`,
    // `I2c<'d, Blocking>`) — no portable bridge, so none of the trait
    // crates below apply. Without this an ESP project with a USART
    // module collected `embedded-io` / `nb` / `embedded-hal`, which
    // nothing in its generated code ever mentions.
    let is_esp = inp.build.is_some_and(|(_, tc)| *tc == ToolchainKind::EspRust);
    // The nRF config modules speak nrf-hal directly too (`Uarte<UARTE0>`,
    // `Spim<SPIM2>`), so the same exclusion: without it a USART module
    // on the micro:bit collected `embedded-io`, `embedded-hal-0-2` and
    // `nb`, none of which the generated code names.
    let is_nrf = super::codegen::nrf::is_nrf(&m.family);
    // And the Raspberry Pi boards: rp2040-hal / rp235x-hal and embassy-rp,
    // directly (`UartPeripheral`, `Spi<Enabled, ..>`). A Pico on Blocking with
    // a UART collected `nb`, `embedded-hal-0-2` and `embedded-io` for months,
    // none of them named by its code; the app's own harnesses could not see
    // it while each built its manifest by hand.
    let is_rp = super::codegen::rp::is_rp(&m.family);
    let hal_direct = is_esp || is_nrf || is_rp;
    // embassy-stm32 on the Blocking runtime (every STM32 but the F1): its
    // drivers speak the standard traits themselves - `embedded-io` 0.7 for a
    // UART, embedded-hal 1.0 for SPI and I2C - so the generated FILES decide,
    // never a module's API style, and none of the F1 bridges' crates
    // (embedded-hal-0-2, nb) apply. Every one of them used to be added for a
    // USART module here, before the backend generated any driver.
    let embassy_blocking = super::codegen::family::embassy_blocking(m);
    let embassy_io = embassy_blocking && (has_cfg("usart") || has_cfg("lpuart"));
    let embassy_eh = embassy_blocking && (has_cfg("spi") || has_cfg("i2c"));
    let needs_can = !is_async && !hal_direct && has_cfg("can");
    // Trait crates only for PORTABLE modules on the Blocking path.
    let (mut needs_usart, mut needs_spi, mut needs_i2c) = (false, false, false);
    // Async SPI/I2C deps: `embedded-hal` 1.0 for any bus, plus
    // `embedded-hal-async` when a module is in async-DMA mode.
    let (mut needs_eh, mut needs_eh_async) = (false, false);
    // ANY USART on the blocking/native path needs `nb`: a Portable USART
    // via its `nb::block!` bridge, a Native USART because the concrete
    // stm32f1xx-hal `Tx`/`Rx` are nb-based (`nb::block!(rx.read())`).
    // (Async USART uses embedded-io-async → no nb.)
    let mut any_usart_blocking = false;
    if !hal_direct && !embassy_blocking {
        for md in &m.modules {
            // A module whose "Generate driver" is off has no file, and so no
            // crate - as on embassy Blocking, where the files decide.
            if is_async && m.raw_modules.contains(&(md.kind, md.instance())) {
                continue;
            }
            if is_async {
                let mode = match &md.config {
                    ModuleConfig::Spi(c) => Some(c.async_mode),
                    ModuleConfig::I2c(c) => Some(c.async_mode),
                    _ => None,
                };
                if let Some(mode) = mode {
                    needs_eh = true;
                    needs_eh_async |= mode == AsyncBusMode::AsyncDma;
                }
            } else {
                // Blocking OR Native path.
                if matches!(&md.config, ModuleConfig::Usart(_)) {
                    any_usart_blocking = true;
                }
                if !is_native {
                    // Blocking (mixed): trait crates for the Portable modules.
                    match &md.config {
                        ModuleConfig::Usart(c) if c.api_style == ApiStyle::Portable => {
                            needs_usart = true
                        }
                        ModuleConfig::Spi(c) if c.api_style == ApiStyle::Portable => {
                            needs_spi = true
                        }
                        ModuleConfig::I2c(c) if c.api_style == ApiStyle::Portable => {
                            needs_i2c = true
                        }
                        _ => {}
                    }
                }
                // is_native → all Native → no portable trait crates
                // (but still `nb` via `any_usart_blocking` above).
            }
        }
    }
    // `nb`: CAN (bxcan), the Portable SPI FullDuplex bridge, and ANY
    // USART on the non-async path. Kept separate so a Native USART (which
    // sets no other trait crate) still keeps `nb` — and a user's pinned
    // `nb = "1.1.0"` survives Save instead of being stripped.
    let needs_nb = needs_can || needs_spi || any_usart_blocking;
    // GPIO in/out pins are wrapped in the `pins::configs::io` eh-1.0
    // bridge, emitted as `io.rs` (always portable).
    let needs_gpio = !is_async && has_cfg("io");
    // USB CDC init needs `usb-device`/`usbd-serial` + the `stm32-usbd`
    // HAL feature, keyed on whether the USB D-/D+ pins are configured.
    // BOTH pads, matching what `gen_parts` will actually emit: one pad
    // alone generates no USB init, so the crates would be unused.
    let usb_pad = |want: PinFunction| all_pins.iter().any(|(_, _, f)| *f == want);
    let is_esp_family = super::codegen::family::is_esp(&m.family);
    let both_usb_pads = usb_pad(PinFunction::UsbDm) && usb_pad(PinFunction::UsbDp);
    // The STM32F1 `usb-device` 0.2 stack. It used to fire on an ESP too:
    // both pads wired on a blocking project added two crates nothing
    // referenced, at versions the ESP bus cannot even use.
    //
    // Not on an nRF either: its USB is not this stack (see below).
    let needs_usb = !is_async && !is_esp_family && !is_nrf && !embassy_blocking && both_usb_pads;
    // The nRF's: nrf-hal's `Usbd` is a usb-device 0.3 bus (the ESP OTG
    // versions), embassy-nrf's driver runs under embassy-usb. One
    // decision, shared with the harness - see `nrf::usb_stack`.
    let (nrf_usb_device, nrf_embassy_usb) = super::codegen::nrf::usb_stack(m);
    // …and the ESP's own stack, which only the OTG role wants.
    let needs_esp_otg = is_esp_family
        && both_usb_pads
        && super::codegen_esp::has_usb_otg(&m.family)
        && super::modules::usb_configs(&m.modules)
            .get(&1)
            .is_some_and(|c| c.role.is_otg());
    let sources: Vec<&str> = std::iter::once(inp.main_rs)
        .chain(inp.user_sources.iter().copied())
        .collect();
    // The HAL crate first: a Runtime switch swaps it on the boards that
    // change HALs (nrf52833-hal / embassy-nrf, rp2040-hal / embassy-rp),
    // and every `ensure_*` below edits the manifest that comes out of it.
    let base_toml = match inp.build {
        Some((cfg, tc)) => project_gen::refresh_hal_dependency(&current.cargo_toml, cfg, tc),
        None => current.cargo_toml.clone(),
    };
    let eh_bridge = needs_spi || needs_i2c || needs_gpio;
    let new_toml = project_gen::ensure_peripheral_crates(
        &base_toml,
        project_gen::PeripheralCrates {
            can: needs_can,
            io: needs_usart || embassy_io,
            eh02: needs_usart || eh_bridge,
            eh1: eh_bridge || embassy_eh,
            nb: needs_nb,
        },
        &sources,
    );
    let new_toml = project_gen::ensure_usb_deps(&new_toml, needs_usb, &sources);
    let new_toml =
        project_gen::ensure_esp_usb_deps(&new_toml, needs_esp_otg || nrf_usb_device, &sources);
    let new_toml = project_gen::ensure_embassy_usb_deps(&new_toml, nrf_embassy_usb, &sources);
    // Async runtime (embassy-executor + embassy-time + the HAL time
    // driver), plus — when the respective config files were emitted —
    // embedded-io-async + static_cell (USART) and embedded-hal /
    // embedded-hal-async (SPI/I2C). Keyed on the project Runtime being
    // Async on an embassy-capable STM32 family.
    //
    // BOTH `ensure_peripheral_deps` (blocking eh1 = SPI/I2C/GPIO) and this
    // manage `embedded-hal` 1.0 — so the need passed here MUST include the
    // blocking case, or the blocking GPIO (io.rs) / SPI / I2C `embedded-hal`
    // that `ensure_peripheral_deps` just added would be stripped right back
    // out here (and a user's manual `embedded-hal` line with it).
    let needs_eh_total = needs_eh || eh_bridge || embassy_eh;
    // Which async stack: embassy-stm32, or esp-rtos on the ESP32-C3.
    // Same crate names, different versions — see `AsyncFlavor`. The chip
    // feature (`esp32c3`) comes from the same `ProjectDef` the base
    // Cargo.toml template uses.
    let esp_chip = inp.build.map(|(p, _)| p.probe_chip.as_str()).unwrap_or_default();
    // One decision, shared with the harness - see `async_flavor_for`.
    // Choosing here as well is how a Pico on Async came to be saved with
    // the STM32 executor line while every test stayed green.
    let async_flavor = project_gen::async_flavor_for(&m.family, esp_chip);
    // `has_cfg` matches a config-FILE prefix, and the async RP backend
    // writes no BUS config files (its only one is the watchdog's) - so a
    // Pico with a `BufferedUart` in main.rs would ask for `static_cell`
    // against a manifest that never got the line. A Pico W hides it,
    // since the radio adds that crate for its own reasons.
    //
    // An LPUART is the same `BufferedUart` - and an LPUART-only project used
    // to get neither crate.
    let needs_async_usart = is_async && (has_cfg("usart") || has_cfg("lpuart"))
        || super::codegen::rp::needs_async_usart(m);
    // SAI and I2S keep their DMA ring buffer in a `StaticCell` too.
    let stm32_ring_buffer =
        is_async && m.family.starts_with("stm32") && (has_cfg("sai") || has_cfg("i2s"));
    // `static_cell` for the async USART AND the SAI/I2S ring buffers: one
    // need for both of the crate's managers (`ensure_task_priority_deps`
    // below), or the first would strip the line the second puts back, moving
    // it on every sync.
    let new_toml = project_gen::ensure_async_deps_with(
        &new_toml,
        is_async,
        async_flavor,
        needs_async_usart,
        needs_async_usart || stm32_ring_buffer,
        needs_eh_total,
        needs_eh_async,
        &sources,
    );
    // `embassy_stm32::exti` is behind a Cargo feature, so an input the
    // user armed with an edge needs the FEATURE as well as the code.
    // Read off the generated main.rs rather than off the pins: a pin
    // refused for a line clash generates no `exti` use, and asking for a
    // feature nothing imports would be a lie in the manifest.
    let new_toml =
        project_gen::ensure_exti_feature(&new_toml, inp.main_rs.contains("embassy_stm32::exti"));
    // `static_cell`, when a raised task priority put an interrupt
    // executor in the generated code. Read off that code for the same
    // reason as the line above. An async nRF TWIM needs it too, for its
    // `'static` RAM buffer; no config file names it, so `has_cfg` above
    // cannot see it, the same gap the Pico's `BufferedUart` had.
    //
    // And the async USART, which `ensure_async_deps` just added it for:
    // its `StaticCell` lives in `src/pins/configs/usart*.rs`, which
    // `sources` leaves out on purpose, so passing only the executor's
    // need here took the line straight back out - every async STM32
    // project with a USART lost `static_cell` on Save.
    let new_toml = project_gen::ensure_task_priority_deps(
        &new_toml,
        needs_async_usart
            || stm32_ring_buffer
            || inp.main_rs.contains("InterruptExecutor")
            || super::codegen::nrf::needs_static_cell(m),
        &sources,
    );
    // The CYW43 radio, on a Pico W / Pico 2 W whose WL_LED is driven or
    // whose IoT tab runs Wi-Fi on it. Gated on use rather than on the
    // board, because a W board that does neither should not carry a
    // wifi stack it never calls. The generator asks the same function.
    let needs_radio = super::codegen::rp::needs_radio(m);
    let needs_bluetooth = super::codegen::rp::needs_bluetooth(m);
    let new_toml =
        project_gen::ensure_cyw43_deps(&new_toml, needs_radio, needs_bluetooth, &sources);
    // Cortex-M0 async: `static_cell` needs CAS the core does not have.
    let target = inp.build.map(|(p, _)| p.target.as_str()).unwrap_or_default();
    let new_toml = project_gen::ensure_m0_atomics(&new_toml, is_async, target, &sources);
    // RTIC runtime: the framework + its SysTick monotonic. The backend
    // feature follows the chip's Rust target, which only `ProjectDef`
    // knows — hence reading it here rather than in the codegen backend.
    let new_toml = project_gen::ensure_rtic_deps(&new_toml, m.is_rtic(), target, &sources);
    // The flash store (Configuration tab): needed exactly when its file
    // is generated - the decision the harnesses make too. esp-storage
    // only on an ESP; an STM32 writes its flash through its own HAL.
    let on_esp = super::flash_store::supported(&m.family);
    let new_toml = project_gen::ensure_flash_store_deps(
        &new_toml,
        flash_store_gen::in_files(config_files),
        on_esp.then_some(esp_chip),
        &sources,
    );
    // The IoT tab's network stack: after the flash store and the radio,
    // because it only ADDS the crates it shares with them.
    let iot_active = super::iot::active(m);
    let new_toml =
        project_gen::ensure_iot_deps(&new_toml, iot_active, esp_chip, &m.family, &sources);
    // The nRF's MPSL brings its own critical section: cortex-m's goes.
    let new_toml =
        project_gen::ensure_mpsl_critical_section(&new_toml, super::codegen::nrf::ble_on(m));
    // Strict-lints `[lints.clippy]` block (MCU System toggle).
    let new_toml = project_gen::ensure_strict_lints(&new_toml, m.strict_lints);
    // Debug-friendly `[profile.release]` (Debug tab toggle).
    let cargo_toml = project_gen::ensure_debug_build(&new_toml, m.debug_build);

    // partitions.csv follows the flash store: the IDE's block while the
    // store has a partition of its own, nothing of ours otherwise (a
    // hand-written table is left as it is). The runner flag goes with it,
    // so `cargo run` flashes the same table the Flash button does.
    let rows = super::flash_store::table_rows(m.flash_store.as_ref(), &m.family);
    let partitions_csv = project_gen::splice_partitions_csv(&current.partitions_csv, rows.as_deref());
    // The build target follows the build config: Thread (IoT tab)
    // moves an nRF52840 / nRF52833 to soft-float and back.
    let targeted = match inp.build {
        Some((cfg, tc)) => project_gen::ensure_cargo_target(&current.cargo_config, cfg, tc),
        None => current.cargo_config.clone(),
    };
    let runner =
        project_gen::ensure_partition_table_runner(&targeted, !partitions_csv.trim().is_empty());
    // esp-radio is built on `alloc`, which the Xtensa toolchain only
    // has when `build-std` lists it.
    let cargo_config = project_gen::ensure_build_std_alloc(
        &runner,
        iot_active.is_some_and(|a| a.platform == super::iot::Platform::Esp),
    );
    // memory.x follows the flash store on an STM32 and a Raspberry Pi
    // board: FLASH ends where the store begins (on an F2/F4/F7
    // `_stext` starts the program after it), so a program that grows
    // into it fails to link. The same decision as the generated
    // `flash_store.rs`; a memory.x with no markers is the user's and is
    // only checked before a flash (not on a Pico under Blocking, where
    // the store's code is not generated).
    let reservation = super::flash_store::memory_x_reservation(m);
    let memory_x = match inp.build {
        Some((def, _)) => project_gen::splice_memory_x_store(&current.memory_x, def, reservation),
        None => current.memory_x.clone(),
    };
    Manifests {
        cargo_toml,
        cargo_config,
        partitions_csv,
        memory_x,
    }
}

/// The project a harness writes: what `AppIde::reset_config_files` seeds for
/// this chip, then the sync the app runs on the next model change. A harness
/// that built its manifest any other way would be testing a project nobody
/// ships.
#[cfg(test)]
pub(crate) fn project_files(
    def: &super::mcu_def::McuDefinition,
    mcu: &Mcu,
    main_rs: &str,
) -> ProjectFiles {
    let cfg = super::mcu_def::build_cfg(def, Some(mcu));
    let mut files = project_gen::build_project_files(&cfg, &def.toolchain, main_rs);
    // Not from the chip, as in `reset_config_files`: the flash store decides it.
    files.partitions_csv.clear();
    sync_files(&mut files, def, mcu);
    files
}

/// A runtime switch the way the app makes one: the project as the app last
/// synced it for the OLD model, the new `main.rs`, then the sync for the new
/// model over what is there. So a crate only the old runtime needed has to be
/// STRIPPED - starting from a fresh manifest would only prove it is never
/// added.
#[cfg(test)]
pub(crate) fn switched(
    from: &ProjectFiles,
    main_rs: &str,
    def: &super::mcu_def::McuDefinition,
    to: &Mcu,
) -> ProjectFiles {
    let mut files = from.clone();
    files.main_rs = main_rs.to_owned();
    sync_files(&mut files, def, to);
    files
}

/// The same lines, in any order: a crate stripped by one switch and added
/// back by the next may land somewhere else in the table, which is not a
/// difference cargo sees.
#[cfg(test)]
pub(crate) fn same_lines(a: &str, b: &str) -> bool {
    fn sorted(t: &str) -> Vec<&str> {
        let mut v: Vec<&str> = t.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
        v.sort_unstable();
        v
    }
    sorted(a) == sorted(b)
}

/// The app's sync over manifests a harness already holds - the ones it wrote
/// for the model BEFORE a runtime switch, say, which is how the app gets there.
#[cfg(test)]
pub(crate) fn sync_files(files: &mut ProjectFiles, def: &super::mcu_def::McuDefinition, mcu: &Mcu) {
    let cfg = super::mcu_def::build_cfg(def, Some(mcu));
    let config_files = mcu.config_files();
    let synced = sync(
        &Manifests::of(files),
        &Inputs {
            mcu,
            build: Some((&cfg, &def.toolchain)),
            config_files: &config_files,
            main_rs: &files.main_rs,
            user_sources: &[],
        },
    );
    synced.apply_to(files);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::model::Runtime;

    /// The app runs the sync on EVERY model change, over the manifests the last
    /// one wrote. A second pass that still moved something would rewrite the
    /// files frame after frame - and fight the user's own edits each time.
    #[test]
    fn a_second_sync_changes_nothing() {
        let mut checked = 0;
        for def in builtin_definitions() {
            for runtime in [Runtime::Blocking, Runtime::Native, Runtime::Async, Runtime::Rtic] {
                use crate::panels::mcu_module::codegen::family;
                let has = match runtime {
                    Runtime::Blocking => true,
                    Runtime::Native => family::native_supported(&def.family),
                    Runtime::Async => family::async_supported(&def.family),
                    Runtime::Rtic => family::rtic_supported(&def.family),
                };
                if !has {
                    continue;
                }
                let mut mcu = def.build_mcu();
                mcu.runtime = runtime;
                mcu.pending_runtime = runtime;
                let main_rs = mcu.fresh_main_rs();
                let once = project_files(&def, &mcu, &main_rs);
                let mut twice = once.clone();
                sync_files(&mut twice, &def, &mcu);
                assert!(
                    Manifests::of(&once) == Manifests::of(&twice),
                    "{} on {runtime:?}: a second sync moved the manifests",
                    def.id
                );
                checked += 1;
            }
        }
        assert!(checked > 10, "only {checked} chip/runtime pairs checked");
    }

    /// embassy-stm32 on Blocking speaks the standard traits itself: a UART
    /// file wants `embedded-io`, a SPI or I2C file embedded-hal 1.0, and
    /// nothing wants the F1 bridges' `embedded-hal-0-2` or `nb` - which a USART
    /// module used to add here before any driver was generated - nor
    /// `usb-device`, which has no template on embassy.
    #[test]
    fn embassy_blocking_gets_the_trait_crates_its_files_name() {
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let mut def = crate::panels::mcu_module::builtins::builtin_for("stm32f103c8t6")
            .expect("built-in F103");
        def.id = "stm32f411re".into();
        def.display_name = "STM32F411RETx".into();
        def.family = "stm32f4".into();
        def.project.pkg_name = "stm32f411re".into();
        def.project.target = "thumbv7em-none-eabihf".into();
        def.project.probe_chip = "STM32F411RETx".into();
        def.project.hal_dep =
            crate::panels::mcu_module::stm32_pin_data::hal_dep_for_name("stm32f4", "STM32F411RETx");
        def.clock = crate::panels::mcu_module::mcu_def::ClockDef::None;
        let mut mcu = def.build_mcu();
        mcu.runtime = Runtime::Blocking;
        mcu.pending_runtime = Runtime::Blocking;
        for (name, f) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PA11", PinFunction::UsbDm),
            ("PA12", PinFunction::UsbDp),
        ] {
            let n = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no {name}"));
            mcu.apply_pin_function(n, f);
        }
        let files = project_files(&def, &mcu, &mcu.fresh_main_rs());
        let toml = &files.cargo_toml;
        let dep = |name: &str| {
            toml.lines().any(|l| {
                l.trim_start()
                    .strip_prefix(name)
                    .is_some_and(|r| r.trim_start().starts_with('='))
            })
        };
        assert!(dep("embedded-io"), "the UART's crate:\n{toml}");
        assert!(dep("embedded-hal"), "the SPI's crate:\n{toml}");
        for gone in ["embedded-hal-0-2", "nb", "usb-device", "usbd-serial"] {
            assert!(!dep(gone), "{gone}:\n{toml}");
        }
        let mut again = files.clone();
        sync_files(&mut again, &def, &mcu);
        assert!(
            Manifests::of(&files) == Manifests::of(&again),
            "a second sync moved it"
        );
    }

    /// A board whose generated code speaks its HAL directly gets none of the
    /// F1 trait-bridge crates. Found by moving the harnesses onto this sync: a
    /// Pico on Blocking with a UART carried `nb`, `embedded-hal-0-2` and
    /// `embedded-io`, which no generated file names. On both RP families, and
    /// with the F103 beside them so the test cannot pass by never adding them.
    #[test]
    fn a_direct_hal_gets_no_bridge_crates() {
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        for (id, tx, rx, bridged) in [
            ("rp2040_pico", "GP0", "GP1", false),
            ("rp2350_pico2", "GP0", "GP1", false),
            ("stm32f103c8t6", "PA9", "PA10", true),
        ] {
            let def = builtin_definitions()
                .into_iter()
                .find(|d| d.id == id)
                .unwrap_or_else(|| panic!("built-in {id}"));
            let mut mcu = def.build_mcu();
            for (name, func) in [(tx, PinFunction::UsartTx(1)), (rx, PinFunction::UsartRx(1))] {
                let func = if id.starts_with("rp") {
                    match func {
                        PinFunction::UsartTx(_) => PinFunction::UsartTx(0),
                        _ => PinFunction::UsartRx(0),
                    }
                } else {
                    func
                };
                let num = mcu
                    .iter_all_pins()
                    .find(|p| p.name == name)
                    .map(|p| p.number)
                    .unwrap_or_else(|| panic!("{id}: no {name}"));
                if let Some(p) = mcu.find_pin_mut(num) {
                    p.selected_function = func;
                }
            }
            mcu.reconcile_modules();
            assert!(
                mcu.modules
                    .iter()
                    .any(|m| matches!(m.config, crate::panels::mcu_module::modules::ModuleConfig::Usart(_))),
                "{id}: the UART made no module, so this proves nothing"
            );
            let toml = project_files(&def, &mcu, &mcu.fresh_main_rs()).cargo_toml;
            for krate in ["nb ", "embedded-hal-0-2 ", "embedded-io "] {
                assert_eq!(toml.contains(krate), bridged, "{id}: {krate}\n{toml}");
            }
        }
    }

    /// The sync starts with the HAL line, as a Runtime switch needs: an F1
    /// saved on Blocking and switched to Async must land on embassy-stm32, and
    /// back. With a USART wired, so each switch also has crates to STRIP: the
    /// blocking bridge's `embedded-io` / `nb` going to Async, the executor
    /// coming back. Each landing must be the project a fresh sync writes.
    #[test]
    fn a_runtime_switch_swaps_the_hal_line() {
        use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
        let def = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103");
        let mut mcu = def.build_mcu();
        for (name, func) in [("PA9", PinFunction::UsartTx(1)), ("PA10", PinFunction::UsartRx(1))] {
            let num = mcu.iter_all_pins().find(|p| p.name == name).map(|p| p.number).expect(name);
            if let Some(p) = mcu.find_pin_mut(num) {
                p.selected_function = func;
            }
        }
        mcu.reconcile_modules();
        let blocking = project_files(&def, &mcu, &mcu.fresh_main_rs());
        let t = &blocking.cargo_toml;
        assert!(t.contains("stm32f1xx-hal") && t.contains("embedded-io ") && t.contains("nb "), "{t}");

        mcu.runtime = Runtime::Async;
        mcu.pending_runtime = Runtime::Async;
        let main_rs = mcu.update_main_rs(&blocking.main_rs);
        let on_async = switched(&blocking, &main_rs, &def, &mcu);
        let t = &on_async.cargo_toml;
        assert!(t.contains("embassy-stm32") && t.contains("embassy-executor"), "{t}");
        assert!(t.contains("embedded-io-async"), "{t}");
        for gone in ["stm32f1xx-hal", "embedded-io ", "nb ", "embedded-hal-0-2"] {
            assert!(!t.contains(gone), "{gone} survived the switch:\n{t}");
        }
        let fresh = project_files(&def, &mcu, &main_rs).cargo_toml;
        assert!(same_lines(t, &fresh), "switched:\n{t}\nfresh:\n{fresh}");

        mcu.runtime = Runtime::Blocking;
        mcu.pending_runtime = Runtime::Blocking;
        let main_rs = mcu.update_main_rs(&on_async.main_rs);
        let back = switched(&on_async, &main_rs, &def, &mcu);
        let t = &back.cargo_toml;
        assert!(t.contains("stm32f1xx-hal") && t.contains("embedded-io "), "{t}");
        assert!(!t.contains("embassy-executor") && !t.contains("embedded-io-async"), "{t}");
        let fresh = project_files(&def, &mcu, &main_rs).cargo_toml;
        assert!(same_lines(t, &fresh), "switched back:\n{t}\nfresh:\n{fresh}");
    }
}
