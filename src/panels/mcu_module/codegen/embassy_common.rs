//! Shared `embassy-stm32` Blocking code generation: every STM32 on the
//! Blocking runtime except the F1 (stm32f1xx-hal, `stm32.rs`).
//!
//! Each generates the SAME `main.rs` shape. First the EXTI statics and
//! `#[interrupt]` handlers of the armed inputs, above `#[entry]`. Then
//! `embassy_stm32::init(...)` under `#[cortex_m_rt::entry]`, and one labelled
//! `let` per pin via `Output::new` / `Input::new` / `Flex` / the raw singleton.
//! After that come the bus drivers' `pins::configs::*::init(..)` lines from
//! `embassy_stm32::peripherals(PeriphMode::Blocking, ..)`, each handed its
//! pad's labelled binding ([`driver_arg`]; a pad that does not bind raw is
//! handed as `p.PXn`, and [`unbound_by_drivers`] leaves its binding out), and a
//! "NOT initialised" note where a driver is refused, async-only or switched
//! off. Last are the Configuration tab's and the Custom modules' inits. The
//! per-chip parts arrive as strings: the clock block, the driver lines and the
//! comparators' `bind_interrupts!` (above `#[entry]`, with the EXTI handlers).
//!
//! Used by the STM32WBA backend and the generic STM32 backend. Both render the
//! Clock tab's RCC block (`rcc::graph_clock_block_*`), and fall back to
//! embassy's default only when the tree cannot be read. GPIO bindings
//! round-trip through [`super::parse_main_rs`], so the Pins canvas restores on
//! reopen.

use super::common::retarget_pristine_tail;
use super::{AUTOGEN_BANNER, GEN_BEGIN, GEN_END, USER_TAIL, mcu_id_marker_line, pin_binding};
use crate::panels::mcu_module::pins::logic::pin::{Edge, GpioMode, Pin};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

/// Invariant file header (above `GEN_BEGIN`, survives every re-splice).
pub fn invariant_header(mcu_name: &str, mcu_id: &str) -> String {
    format!(
        // Spelled out because the crate NAME reads like the async runtime: on
        // every STM32 but F1, embassy-stm32 is simply the family's HAL, and this
        // entry point is plain `#[entry] fn main() -> !`. Seeing `embassy_stm32`
        // under the Blocking runtime otherwise looks like the choice was ignored.
        "{AUTOGEN_BANNER}\n\
         // MCU: {mcu_name} | HAL: embassy-stm32 (blocking — sync HAL, no executor)\n\
         {id}\n\
         #![no_std]\n\
         #![no_main]\n\n\
         pub mod pins;\n\n\
         use panic_halt as _;\n\
         use cortex_m_rt::entry;\n\n",
        id = mcu_id_marker_line(mcu_id),
    )
}

/// The gpio `use` line (only the types actually used, so no unused-import
/// warnings) and the per-pin `let` binding body for the BLOCKING section
/// builder, with no input armed (the GPIO API is the async one's too; only the
/// entry point and the EXTI differ). An empty pin set yields an empty `use`
/// line and a placeholder-comment body. The section builder arms inputs, so it
/// calls [`bindings`] itself; this is what the binding tests read.
#[cfg(test)]
pub(super) fn gpio_bindings(pins: &[&Pin]) -> (String, String) {
    bindings(pins, &[], true)
}

/// [`gpio_bindings`], with the inputs that were given an EXTI line.
///
/// `exti` is `(singleton, line)` — an armed input binds as an `ExtiInput`
/// instead of an `Input`, because the two are different types and only the
/// second can be awaited. The pull and the trailing `// GPIO Input` comment are
/// unchanged, so [`super::parse_main_rs`] still reads the pin back.
///
/// An armed input that is NOT in `exti` gets no note here: `exti_plan` already
/// wrote why (a line clash, or no vector), and the Blocking note would be wrong.
pub(super) fn gpio_bindings_exti(pins: &[&Pin], exti: &[(String, u8)]) -> (String, String) {
    bindings(pins, exti, false)
}

/// `exti` names the inputs `exti_plan` gave a line. `blocking` picks what they
/// become: an `ExtiInput<Blocking>` parked in its static for the `#[interrupt]`
/// (see `embassy_stm32::exti_blocking_items`), or the `ExtiInput<Async>` a task
/// awaits. A refused input binds as a plain `Input` on both, and `exti_plan`
/// writes why.
fn bindings(pins: &[&Pin], exti: &[(String, u8)], blocking: bool) -> (String, String) {
    let configured: Vec<&&Pin> = pins
        .iter()
        .filter(|p| p.selected_function != PinFunction::Unset)
        .collect();

    let any_output = configured
        .iter()
        .any(|p| p.selected_function == PinFunction::GpioOutput);
    // An input armed on an EXTI line binds as `ExtiInput`, not `Input` - counted
    // as a plain one, a project whose only input was armed imported `Input` for
    // nothing (`unused import`). It still takes a `Pull`.
    let armed = |p: &Pin| exti.iter().any(|(g, _)| g == p.gpio());
    let any_input = configured
        .iter()
        .any(|p| p.selected_function == PinFunction::GpioInput && !armed(p));
    let any_armed = configured
        .iter()
        .any(|p| p.selected_function == PinFunction::GpioInput && armed(p));

    // Only import the gpio types actually used (no unused-import warnings).
    // A pin wired as a raw alternate function needs Flex + AfType, and the
    // AfType constructor pulls in Pull or OutputType/Speed depending on the mode.
    let af_pins: Vec<&&Pin> = configured
        .iter()
        .filter(|p| matches!(&p.selected_function, PinFunction::Other(s) if p.af_of(s).is_some()))
        .filter(|p| p.io_mode.is_some())
        .copied()
        .collect();
    let af_input = af_pins.iter().any(|p| {
        matches!(
            p.io_mode,
            Some(GpioMode::Floating | GpioMode::PullUp | GpioMode::PullDown)
        )
    });
    let af_output = af_pins
        .iter()
        .any(|p| matches!(p.io_mode, Some(GpioMode::PushPull | GpioMode::OpenDrain)));

    let mut imports = Vec::new();
    if any_input || any_armed || af_input {
        imports.push("Pull");
    }
    if any_input {
        imports.push("Input");
    }
    if any_output {
        imports.push("Level");
        imports.push("Output");
    }
    if any_output || af_output {
        imports.push("Speed");
    }
    if !af_pins.is_empty() {
        imports.push("AfType");
        imports.push("Flex");
    }
    if af_output {
        imports.push("OutputType");
    }
    imports.sort_unstable();
    imports.dedup();
    let mut use_line = if imports.is_empty() {
        String::new()
    } else {
        format!("use embassy_stm32::gpio::{{{}}};\n", imports.join(", "))
    };
    // `ExtiInput` lives in its own module, and its type carries the MODE — the
    // tasks below name `ExtiInput<'static, Async>`, so both come in here. On
    // Blocking the statics name `Blocking`, and the handlers need the rest.
    if !exti.is_empty() && blocking {
        use_line.push_str(super::embassy_stm32::EXTI_BLOCKING_USES);
    } else if !exti.is_empty() {
        use_line.push_str("use embassy_stm32::exti::ExtiInput;\n");
        use_line.push_str("use embassy_stm32::mode::Async;\n");
    }

    let mut body = String::new();
    for p in &configured {
        body.push_str(&pin_binding_line(p, exti, blocking));
        body.push('\n');
    }
    // NB: no "no pins" placeholder here — the caller decides, since the async
    // section may still have peripheral (USART) init lines when no pin is bound.
    (use_line, body)
}

/// The comment shown in `fn main` when nothing is generated yet.
pub(super) const NO_PINS_PLACEHOLDER: &str =
    "    // No pins configured yet — assign functions on the Pins canvas.\n";

/// The generated section: gpio `use` items (only when needed), `#[entry]`, the
/// caller-supplied `clock_block` (which must define `let p = …` and end in
/// `\n`), one `let` binding per configured pin, then `periph_calls`. Opens
/// `fn main()` — `USER_TAIL` closes it with the editable loop.
pub fn make_generated_section(
    mcu_name: &str,
    pins: &[&Pin],
    clock_block: &str,
    // The bus drivers' `pins::configs::*::init(…)` lines ("" when none), which
    // take the bindings above them - see `embassy_stm32::peripherals`.
    periph_calls: &str,
    // `let x = Foo::new(pa0_out, …);` lines for the Custom modules — appended
    // after the pin bindings they consume (see `Mcu::custom_module_inits`).
    custom_inits: &str,
    // The chip's interrupt vectors (`Mcu::irq_vectors`). An armed input gets a
    // handler only on a line one of them serves; none - a chip imported without
    // vendor data - and every armed input is refused, in writing.
    vectors: &[String],
    // The `bind_interrupts!` the drivers' inits take (`AsyncPeriphs::dma_irqs`:
    // the comparators' on this runtime), "" when none. An item, so above the
    // entry with the EXTI handlers.
    irqs: &str,
) -> String {
    // The Async plan, so both runtimes refuse the same pads for the same
    // reasons: a line already taken, or no vector for it.
    let (exti, exti_notes) = super::embassy_stm32::exti_plan(pins, vectors);
    let lines: Vec<(String, u8)> = exti.iter().map(|e| (e.singleton.clone(), e.line)).collect();
    let (use_line, mut body) = bindings(pins, &lines, true);
    body.push_str(&exti_notes);
    if !periph_calls.is_empty() {
        if !body.is_empty() {
            body.push('\n');
        }
        body.push_str(periph_calls);
    }
    if body.is_empty() {
        body.push_str(NO_PINS_PLACEHOLDER);
    }
    // The Configuration tab's lines and the Custom modules, each part under
    // its own header (`Mcu::watchdog_and_custom_inits`).
    body.push_str(custom_inits);
    format!(
        "{GEN_BEGIN}\n\
         {use_line}\n\
         {items}#[entry]\n\
         #[allow(unused_variables, unused_mut)]\n\
         fn main() -> ! {{\n\
         \x20   // {mcu_name}\n\
         {clock_block}\
         \n\
         {body}\
         {GEN_END}\n",
        // Statics and handlers are items: above the entry, not inside it.
        items = format!("{irqs}{}", super::embassy_stm32::exti_blocking_items(&exti)),
    )
}

/// Re-splice the generated section of an existing `main.rs`, preserving the user
/// tail. The invariant header is rebuilt (not kept from `existing[..begin]`) so
/// an Async→Blocking runtime switch restores the blocking imports/entry over a
/// file that was previously async. Rebuilds from scratch when the markers are
/// gone.
pub fn splice_section(existing: &str, new_section: &str, mcu_name: &str, mcu_id: &str) -> String {
    let header = invariant_header(mcu_name, mcu_id);
    if let (Some(_begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END)) {
        let end = end_start + GEN_END.len();
        // The mirror of the async splice: a project switched BACK to
        // Blocking must not keep an "every iteration must `.await`" warning in
        // a program that has no executor.
        let after = retarget_pristine_tail(existing[end..].trim_start_matches('\n'), false);
        format!("{header}{new_section}\n{after}")
    } else {
        format!("{header}{new_section}\n{USER_TAIL}")
    }
}

/// The variable [`pin_binding_line`] binds `p` to (`pa9_usart1_tx`).
pub(super) fn binding_var(p: &Pin) -> String {
    pin_binding(
        &p.gpio().to_ascii_lowercase(),
        &p.selected_function,
        &p.custom_label,
    )
}

/// Whether `p` binds as its bare singleton (`let pa9_usart1_tx = p.PA9;`), the
/// one shape a driver's `init` can be handed. A GPIO binds as an `Output` or an
/// `Input`, and a generic alternate function with a mode as a `Flex`.
pub(super) fn binds_raw(p: &Pin) -> bool {
    match &p.selected_function {
        PinFunction::Unset | PinFunction::GpioOutput | PinFunction::GpioInput => false,
        PinFunction::Other(signal) => p.af_of(signal).is_none() || p.io_mode.is_none(),
        _ => true,
    }
}

/// What a driver's `init` is handed for the pad on GPIO `gpio`: the variable
/// its labelled `let` line binds, so that line stays in main.rs - it is what
/// [`super::parse_main_rs`] reads the pin back from on reopen. A pad that does
/// not bind raw is handed as `p.PXn` itself, and the section leaves its
/// binding out ([`unbound_by_drivers`]).
pub(super) fn driver_arg(pins: &[&Pin], gpio: &str) -> String {
    pins.iter()
        .find(|p| p.selected_function != PinFunction::Unset && p.gpio() == gpio)
        .filter(|p| binds_raw(p))
        .map(|p| binding_var(p))
        .unwrap_or_else(|| format!("p.{gpio}"))
}

/// `pins` minus those a driver takes as `p.PXn` (see [`driver_arg`]): their
/// binding would move the singleton first. `driven` is the GPIOs the drivers
/// take, as `AsyncPeriphs::consumed_pins` lists them.
pub(super) fn unbound_by_drivers<'a>(pins: &[&'a Pin], driven: &[String]) -> Vec<&'a Pin> {
    pins.iter()
        .copied()
        .filter(|p| binds_raw(p) || !driven.iter().any(|d| d == p.gpio()))
        .collect()
}

/// One pin's `let` line. Output → `let mut pXY = Output::new(...)`; input →
/// `let pXY = Input::new(...)`; anything else → the raw singleton `let pXY =
/// p.PXY;` (hand to a driver). The trailing `// <Label>` is what
/// [`super::parse_main_rs`] reads back.
fn pin_binding_line(p: &Pin, exti: &[(String, u8)], blocking: bool) -> String {
    let func = &p.selected_function;
    // The GPIO that actually provides the chosen function - normally the pin's
    // own name, but a package pin with two pads bonded together answers to a
    // different one depending on what you picked (see `Pin::gpio_for`).
    let singleton = p.gpio(); // embassy singleton = "PB5"
    let var = binding_var(p);
    let label = func.label();
    match func {
        PinFunction::GpioOutput => format!(
            "    let mut {var} = Output::new(p.{singleton}, Level::Low, Speed::Low); // {label}"
        ),
        PinFunction::GpioInput => {
            // embassy takes the pull as an argument, so the user's mode choice is
            // just this variant (default = no pull, what it always generated).
            let pull = match p.io_mode {
                Some(GpioMode::PullUp) => "Pull::Up",
                Some(GpioMode::PullDown) => "Pull::Down",
                _ => "Pull::None",
            };
            match exti.iter().find(|(g, _)| g == singleton) {
                // Blocking: parked for the vector's `#[interrupt]`.
                Some((_, line)) if blocking => super::embassy_stm32::exti_blocking_binding(
                    &var,
                    singleton,
                    *line,
                    pull,
                    p.irq.unwrap_or(Edge::Rising),
                    &label,
                ),
                // `Irqs` is the same struct the DMA peripherals bind into —
                // `ExtiInput::new` takes the binding, not just the channel.
                Some((_, line)) => format!(
                    "    let {var} = ExtiInput::new(p.{singleton}, p.EXTI{line}, {pull}, Irqs); \
                     // {label}"
                ),
                None => format!("    let {var} = Input::new(p.{singleton}, {pull}); // {label}"),
            }
        }
        // A generic alternate function whose AF index the vendor publishes: bind
        // the pad AS that function. `Flex` + `set_as_af_unchecked` is embassy's
        // own escape hatch for an AF it has no driver for, and the index is the
        // one thing the user cannot look up from the IDE otherwise.
        //
        // The DIRECTION comes from the pin's mode, never from a guess: a signal
        // like `SAI1_SD_A` is an input in one configuration and an output in
        // another. Without a mode the pin binds raw, exactly as before, with the
        // AF named in the comment so it can be wired by hand.
        PinFunction::Other(signal) => match (p.af_of(signal), p.io_mode) {
            (Some(af), Some(mode)) => {
                let af_type = match mode {
                    GpioMode::PullUp => "AfType::input(Pull::Up)",
                    GpioMode::PullDown => "AfType::input(Pull::Down)",
                    GpioMode::Floating => "AfType::input(Pull::None)",
                    GpioMode::OpenDrain => "AfType::output(OutputType::OpenDrain, Speed::Low)",
                    GpioMode::PushPull => "AfType::output(OutputType::PushPull, Speed::Low)",
                };
                // Two lines, one binding: `Flex::new` then the AF call. The
                // second line's four-space indent is part of the payload — the
                // generated file is not re-formatted.
                // Two lines for one binding: `Flex::new`, then the AF call.
                // Built separately so the indentation of the second line is not
                // at the mercy of how this source file happens to be wrapped.
                let bind = format!("    let mut {var} = Flex::new(p.{singleton});");
                let wire =
                    format!("    {var}.set_as_af_unchecked({af}, {af_type}); // {label} (AF{af})");
                format!("{bind}\n{wire}")
            }
            (Some(af), None) => format!(
                "    let {var} = p.{singleton}; // {label} (AF{af} — pick a mode to wire it)"
            ),
            _ => format!("    let {var} = p.{singleton}; // {label}"),
        },
        // Bus / analog / debug: bind the raw peripheral, ready for its driver.
        _ => format!("    let {var} = p.{singleton}; // {label}"),
    }
}

#[cfg(test)]
mod emit_for_manual_compile {
    //! A generator, not an assertion: it writes a complete embassy-stm32
    //! project to a temp folder so the version pin can be verified the only way
    //! that counts — by compiling it.
    //!
    //! `#[ignore]`d because it needs the `thumbv7em-none-eabihf` target and the
    //! network. Run it, then check the output:
    //!
    //! ```text
    //! cargo test --bin rust_on_chip emit_embassy_project -- --ignored --nocapture
    //! cd %TEMP%\eide_embassy_check && cargo check --target thumbv7em-none-eabihf
    //! ```

    use crate::panels::mcu_module::builtins::builtin_for;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    use crate::panels::mcu_module::{manifest_sync, project_gen, stm32_pin_data};

    /// Where a named vendor file actually is on THIS machine.
    ///
    /// The default used to be one hard-coded `H:` path. That works until the
    /// checkout there turns out to be partial — this machine's holds 232 of the
    /// 2123 parts, and neither G431 nor G474 is among them, so both importer
    /// cases reported "no chip xml" and failed the whole matrix. The chip was
    /// installed the whole time, one directory over.
    ///
    /// So: ask the sources the IDE itself would search, and take the first that
    /// has the file. The env override still wins, for pointing at a specific
    /// copy - and a bare FILE NAME there is searched for the same way, so a
    /// matrix row can name another part without naming this machine's paths.
    fn vendor_chip_file(env_var: &str, file: &str) -> Option<std::path::PathBuf> {
        let named = std::env::var(env_var).ok();
        if let Some(p) = &named
            && p.contains(['/', '\\'])
        {
            let p = std::path::PathBuf::from(p);
            return p.is_file().then_some(p);
        }
        let file = named.as_deref().unwrap_or(file);
        crate::panels::mcu_module::chip_sources::all_sources()
            .into_iter()
            .filter(|s| s.has_clock())
            .map(|s| s.chips.join(file))
            .find(|p| p.is_file())
    }

    /// `EIDE_F1_IRQ_SHARED=1` arms PB5 (either edge) and PB8 (rising): two
    /// lines on ONE vector, `EXTI9_5`, so the handler - or the RTIC task - asks
    /// each pin and calls each pin's hook. Both pads are free in every F1
    /// wiring here; one that is not is a broken row, so it panics.
    fn arm_shared_exti9_5(mcu: &mut crate::panels::mcu_module::mcu::Mcu) {
        use crate::panels::mcu_module::pins::logic::pin::Edge;
        if std::env::var_os("EIDE_F1_IRQ_SHARED").is_none() {
            return;
        }
        for (name, edge) in [("PB5", Edge::Both), ("PB8", Edge::Rising)] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) if p.selected_function == PinFunction::Unset => {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = Some(edge);
                }
                _ => panic!("{name} is not free to arm in this wiring"),
            }
        }
    }

    /// Say something when a codegen matrix run is in progress.
    ///
    /// These harnesses REWRITE fixed directories under the temp dir, and the
    /// matrix cross-compiles those same directories. Running one by hand during
    /// a matrix run corrupts both, and the damage does not read as concurrency:
    /// it comes out as `could not write output`, `failed to write fingerprint`,
    /// `link.exe: 1104` — errors that look like a codegen regression and point
    /// at the wrong file. That is exactly how the first of two collisions in one
    /// evening happened.
    ///
    /// A WARNING, not a wait: whoever runs a single emit test should not be held
    /// for the twenty-odd minutes a matrix takes. The matrix sets
    /// `EIDE_MATRIX_RUN` for the harnesses it drives itself, so its own runs
    /// stay quiet.
    ///
    /// The probe is Windows-shaped on purpose. The script holds the lock with no
    /// sharing, so a plain write-open fails here and succeeds on unix, where
    /// there is no mandatory locking — and the matrix is a PowerShell script, so
    /// staying silent there costs nothing.
    fn warn_if_matrix_running() {
        if std::env::var_os("EIDE_MATRIX_RUN").is_some() {
            return;
        }
        let lock = std::env::temp_dir().join("eide-codegen-matrix.lock");
        if !lock.exists() || std::fs::OpenOptions::new().write(true).open(&lock).is_ok() {
            return;
        }
        for line in [
            "a codegen matrix run is holding the lock RIGHT NOW",
            "this harness rewrites the very projects it is cross-compiling",
            "both runs will report errors that look like codegen bugs",
            "wait for it to finish, or expect to re-run both",
        ] {
            eprintln!("!!! {line}");
        }
    }
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_embassy_project() {
        warn_if_matrix_running();
        // No embassy chip is bundled (the two built-ins are F1 and ESP32-C3), so
        // build the F411 the XML importer would: same pin data, the family and
        // dependency line an import produces.
        let mut def = builtin_for("stm32f103c8t6").expect("built-in F103");
        def.id = "stm32f411re".into();
        def.display_name = "STM32F411RETx".into();
        def.family = "stm32f4".into();
        def.project.pkg_name = "stm32f411re".into();
        def.project.target = "thumbv7em-none-eabihf".into();
        def.project.probe_chip = "STM32F411RETx".into();
        def.project.hal_dep = stm32_pin_data::hal_dep_for_name("stm32f4", "STM32F411RETx");
        def.clock = crate::panels::mcu_module::mcu_def::ClockDef::None;

        let mut mcu = def.build_mcu();
        // One GPIO out and one GPIO in — the two shapes `gpio_binding` emits.
        let nums: Vec<usize> = mcu
            .iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| p.number)
            .take(5)
            .collect();
        if let Some(p) = mcu.find_pin_mut(nums[0]) {
            p.selected_function = PinFunction::GpioOutput;
        }
        if let Some(p) = mcu.find_pin_mut(nums[1]) {
            p.selected_function = PinFunction::GpioInput;
        }
        // Analog mode too — a pin state with no embassy type of its own, so the
        // backend binds the raw singleton. Included here because "it compiles"
        // is the only real check for that.
        if let Some(n) = nums.get(2).copied() {
            if let Some(p) = mcu.find_pin_mut(n) {
                p.selected_function = PinFunction::GpioAnalog;
            }
        }
        // A generic alternate function WITH a vendor AF index and a direction:
        // the `Flex` + `set_as_af_unchecked` path. Only a real cross-compile
        // proves that call, its argument order and the imports are right.
        if let Some(n) = nums.get(3).copied() {
            if let Some(p) = mcu.find_pin_mut(n) {
                p.selected_function = PinFunction::Other("SAI1_SD_A".into());
                p.af = vec![("SAI1_SD_A".into(), 6)];
                p.io_mode = Some(crate::panels::mcu_module::pins::logic::pin::GpioMode::PushPull);
            }
        }
        // …and one with an index but NO mode, which must stay a raw binding.
        if let Some(n) = nums.get(4).copied() {
            if let Some(p) = mcu.find_pin_mut(n) {
                p.selected_function = PinFunction::Other("FMC_A0".into());
                p.af = vec![("FMC_A0".into(), 12)];
            }
        }
        let main_rs = mcu.fresh_main_rs();
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);

        // `sync_pin_files` always keeps `src/pins/mod.rs` in a real project, and
        // every invariant header declares `pub mod pins;` — so the harness has to
        // supply it too, or it tests a project shape the app never produces.
        let pins_mod = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_embassy_check");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &pins_mod, "", "").expect("write project");
        println!("wrote {}", dir.display());
        println!("target: {}", def.project.target);
        println!("hal_dep: {}", def.project.hal_dep);

        // ── The ASYNC variant ────────────────────────────────────────────────
        // Same chip on the Async runtime: this is where the version pin is most
        // exposed, because `embassy-executor` and `embassy-time` are pinned
        // SEPARATELY from `embassy-stm32` and have to agree with it.
        mcu.runtime = crate::panels::mcu_module::mcu::model::Runtime::Async;
        let main_rs = mcu.fresh_main_rs();
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        let adir = std::env::temp_dir().join("eide_embassy_check_async");
        project_gen::clear_project_dir_keep_target(&adir);
        project_gen::write_project(&adir, &files, &mcu.pin_tree_files(), "", "")
            .expect("write async project");
        println!("wrote {}", adir.display());
        println!("target: {}", def.project.target);

        // ── Async + USART ────────────────────────────────────────────────────
        // `BufferedUart` is the embassy API most likely to move between
        // versions, and it lives in a generated config file rather than main.rs.
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        mcu.reconcile_modules();
        let main_rs = mcu.fresh_main_rs();
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        let configs = mcu.config_files();
        println!(
            "config files: {:?}",
            configs.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let udir = std::env::temp_dir().join("eide_embassy_check_usart");
        project_gen::clear_project_dir_keep_target(&udir);
        project_gen::write_project(&udir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write usart project");
        println!("wrote {}", udir.display());
        println!("target: {}", def.project.target);

        // ── Async + SPI/I2C on DMA ────────────────────────────────────
        // The path that had NEVER been compiled: every assertion on it was a
        // substring check, and both templates were in fact calling embassy 0.6
        // with the wrong arity (SPI) and the wrong argument order (I2C).
        //
        // The generated code deliberately does not compile as-is -- the DMA
        // channels are a TODO the IDE cannot fill (see `DMA_TODO`). Pass real
        // ones for this chip to get a project that should build end to end:
        //
        //   EIDE_DMA_TX=DMA2_CH3 EIDE_DMA_RX=DMA2_CH2         //   EIDE_DMA_TX_IRQ=DMA2_STREAM3 EIDE_DMA_RX_IRQ=DMA2_STREAM2         //   cargo test -- --ignored --nocapture emit_embassy_project
        for (name, func) in [
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            // I2C too: its template had the OTHER defect (irq argument in the
            // wrong position), so both need a real compile.
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        mcu.reconcile_modules();
        // Flip every SPI module the reconcile created to async-DMA.
        for m in &mut mcu.modules {
            use crate::panels::mcu_module::modules::{AsyncBusMode, ModuleConfig};
            match &mut m.config {
                ModuleConfig::Spi(c) => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::I2c(c) => c.async_mode = AsyncBusMode::AsyncDma,
                // The USART too: its DMA form is a different template again
                // (UartTx + RingBufferedUartRx), so it needs its own compile.
                ModuleConfig::Usart(c) => {
                    c.mode = crate::panels::mcu_module::modules::UsartMode::Dma
                }
                _ => {}
            }
        }
        let mut main_rs = mcu.fresh_main_rs();
        // Substitute real channels when the caller supplied them, so the
        // TEMPLATE can be compiled without the product pretending to know a
        // chip's DMA map (that is the next stage).
        let ev = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_owned());
        let (tx, rx) = (
            ev("EIDE_DMA_TX", "DMA_TX_TODO"),
            ev("EIDE_DMA_RX", "DMA_RX_TODO"),
        );
        // Per-peripheral: a DMA channel is valid for ONE peripheral, so a single
        // pair for the whole file only ever proves the first of them.
        let (itx, irx) = (ev("EIDE_I2C_DMA_TX", &tx), ev("EIDE_I2C_DMA_RX", &rx));
        let (utx, urx) = (ev("EIDE_USART_DMA_TX", &tx), ev("EIDE_USART_DMA_RX", &rx));
        main_rs = main_rs
            .lines()
            .map(|l| {
                let (t, r) = if l.contains("i2c") {
                    (&itx, &irx)
                } else if l.contains("usart") {
                    (&utx, &urx)
                } else {
                    (&tx, &rx)
                };
                format!(
                    "{}
",
                    l.replace("DMA_TX_TODO", t).replace("DMA_RX_TODO", r)
                )
            })
            .collect();
        if let (Ok(ti), Ok(ri)) = (
            std::env::var("EIDE_DMA_TX_IRQ"),
            std::env::var("EIDE_DMA_RX_IRQ"),
        ) {
            let (i_ti, i_ri) = (
                ev("EIDE_I2C_DMA_TX_IRQ", &ti),
                ev("EIDE_I2C_DMA_RX_IRQ", &ri),
            );
            let (u_ti, u_ri) = (
                ev("EIDE_USART_DMA_TX_IRQ", &ti),
                ev("EIDE_USART_DMA_RX_IRQ", &ri),
            );
            let binds = format!(
                "    {ti} => embassy_stm32::dma::InterruptHandler<peripherals::{tx}>;
                     {ri} => embassy_stm32::dma::InterruptHandler<peripherals::{rx}>;
                     {i_ti} => embassy_stm32::dma::InterruptHandler<peripherals::{itx}>;
                     {i_ri} => embassy_stm32::dma::InterruptHandler<peripherals::{irx}>;
                     {u_ti} => embassy_stm32::dma::InterruptHandler<peripherals::{utx}>;
                     {u_ri} => embassy_stm32::dma::InterruptHandler<peripherals::{urx}>;
"
            );
            main_rs = main_rs.replace(
                "bind_interrupts!(struct Irqs {
",
                &format!(
                    "bind_interrupts!(struct Irqs {{
{binds}"
                ),
            );
        }
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        // One device, so its file and the `pub mod` in the bus's `mod.rs` -
        // which sits right against the END marker in this template - are
        // compiled too.
        assert!(mcu.with_i2c_devices(&[("imu", 0x68)]));
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let ddir = std::env::temp_dir().join("eide_embassy_check_dma");
        project_gen::clear_project_dir_keep_target(&ddir);
        project_gen::write_project(&ddir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write dma project");
        println!("wrote {}", ddir.display());
        println!("target: {}", def.project.target);

        // -- The same, on an F2 --------------------------------------------
        // Its DMA request map is its OWN (six entries differ from F4 despite
        // the families looking interchangeable), so the table is only as good
        // as a compile of the family it claims to describe.
        let mut f2def = def.clone();
        f2def.id = "stm32f217ze".into();
        f2def.display_name = "STM32F217ZETx".into();
        f2def.family = "stm32f2".into();
        f2def.project.pkg_name = "stm32f217ze".into();
        f2def.project.target = "thumbv7m-none-eabi".into();
        f2def.project.probe_chip = "STM32F217ZETx".into();
        f2def.project.hal_dep = stm32_pin_data::hal_dep_for_name("stm32f2", "STM32F217ZETx");
        let mut f2mcu = f2def.build_mcu();
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ] {
            let num = f2mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| f2mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        f2mcu.runtime = crate::panels::mcu_module::mcu::model::Runtime::Async;
        f2mcu.reconcile_modules();
        for m in &mut f2mcu.modules {
            use crate::panels::mcu_module::modules::{AsyncBusMode, ModuleConfig, UsartMode};
            match &mut m.config {
                ModuleConfig::Spi(c) => {
                    c.async_mode = AsyncBusMode::AsyncDma;
                    // Non-default on purpose: the default writes no line, so
                    // only this proves the emitted one compiles.
                    c.bit_order = crate::panels::mcu_module::modules::SpiBitOrder::LsbFirst;
                }
                ModuleConfig::I2c(c) => {
                    c.async_mode = AsyncBusMode::AsyncDma;
                    c.timeout_ms = 250;
                }
                ModuleConfig::Usart(c) => c.mode = UsartMode::Dma,
                _ => {}
            }
        }
        let f2_main = f2mcu.fresh_main_rs();
        let f2files = manifest_sync::project_files(&f2def, &f2mcu, &f2_main);
        let f2user: Vec<(String, String)> = f2mcu.pin_tree_files();
        let f2dir = std::env::temp_dir().join("eide_embassy_check_dma_f2");
        project_gen::clear_project_dir_keep_target(&f2dir);
        project_gen::write_project(&f2dir, &f2files, &f2user, &f2mcu.mcu_config_text(), "")
            .expect("write f2 dma project");
        println!("wrote {}", f2dir.display());
        println!("target: {}", f2def.project.target);

        // -- and on an F7 --------------------------------------------------
        // STM32F767ZI: the part that used to fail in embassy's build script
        // for want of a `single-bank` feature, before `needs_bank_feature`
        // started emitting one. Its DMA map was diffed against an F746ZG over
        // every instance in the table and they agree, so this stands for the
        // family rather than for one chip.
        let mut f7def = def.clone();
        f7def.id = "stm32f767zi".into();
        f7def.display_name = "STM32F767ZITx".into();
        f7def.family = "stm32f7".into();
        f7def.project.pkg_name = "stm32f767zi".into();
        f7def.project.target = "thumbv7em-none-eabihf".into();
        f7def.project.probe_chip = "STM32F767ZITx".into();
        f7def.project.hal_dep = stm32_pin_data::hal_dep_for_name("stm32f7", "STM32F767ZITx");
        let mut f7mcu = f7def.build_mcu();
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
        ] {
            let num = f7mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| f7mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        f7mcu.runtime = crate::panels::mcu_module::mcu::model::Runtime::Async;
        f7mcu.reconcile_modules();
        for m in &mut f7mcu.modules {
            use crate::panels::mcu_module::modules::{AsyncBusMode, ModuleConfig, UsartMode};
            match &mut m.config {
                ModuleConfig::Spi(c) => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::I2c(c) => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::Usart(c) => c.mode = UsartMode::Dma,
                _ => {}
            }
        }
        let f7_main = f7mcu.fresh_main_rs();
        let f7files = manifest_sync::project_files(&f7def, &f7mcu, &f7_main);
        let f7user: Vec<(String, String)> = f7mcu.pin_tree_files();
        let f7dir = std::env::temp_dir().join("eide_embassy_check_dma_f7");
        project_gen::clear_project_dir_keep_target(&f7dir);
        project_gen::write_project(&f7dir, &f7files, &f7user, &f7mcu.mcu_config_text(), "")
            .expect("write f7 dma project");
        println!("wrote {}", f7dir.display());
        println!("target: {}", f7def.project.target);

        // -- Watchdogs, on the same F4 -------------------------------------
        // Pin-less, so they exercise a path nothing else does: a config file
        // and an init line that come from a TAB rather than from the pins.
        // Both are enabled at once because their lifecycles differ and the
        // generated code has to be right for each (`unleash` vs already
        // running).
        {
            use crate::panels::mcu_module::watchdog::{
                IwdgConfig, WatchdogSettings, WwdgConfig, iwdg_range_us, limits_for, wwdg_range_us,
            };
            let mut w = def.build_mcu();
            let l = limits_for(&w.family, w.runtime);
            // The default the Reset button restores, i.e. the value the tab
            // hands out unedited - so the harness proves exactly what a user
            // gets by switching both on and touching nothing.
            let pclk1 = 100_000_000;
            w.watchdog = WatchdogSettings {
                iwdg: Some(IwdgConfig {
                    timeout_us: iwdg_range_us(&l).1,
                }),
                wwdg: Some(WwdgConfig {
                    timeout_us: wwdg_range_us(&l, pclk1).unwrap().1,
                    window_us: 0,
                }),
                ..Default::default()
            };
            let w_main = w.fresh_main_rs();
            let wfiles = manifest_sync::project_files(&def, &w, &w_main);
            let wuser: Vec<(String, String)> = w.pin_tree_files();
            let wdir = std::env::temp_dir().join("eide_embassy_check_wdg");
            project_gen::clear_project_dir_keep_target(&wdir);
            project_gen::write_project(&wdir, &wfiles, &wuser, &w.mcu_config_text(), "")
                .expect("write watchdog project");
            println!("wrote {}", wdir.display());
            println!("target: {}", def.project.target);
        }

        // -- Watchdogs on the WBA ------------------------------------------
        // Its own backend and its own limits: `wwdg_v2` prescales to /128
        // (sixteen times further than the F4), so the WWDG default here is a
        // number no other row produces - worth compiling rather than assuming
        // the embassy templates carry over. Its `iwdg_v3` would reach /1024,
        // but embassy 0.6 stops at /256, so the IWDG default is the F4's.
        {
            use crate::panels::mcu_module::watchdog::{
                IwdgConfig, WatchdogSettings, WwdgConfig, iwdg_range_us, limits_for, wwdg_range_us,
            };
            let mut wdef = def.clone();
            wdef.id = "stm32wba55cg".into();
            wdef.display_name = "STM32WBA55CGUx".into();
            wdef.family = "stm32wba".into();
            wdef.project.pkg_name = "stm32wba55cg".into();
            wdef.project.target = "thumbv8m.main-none-eabihf".into();
            wdef.project.probe_chip = "STM32WBA55CGUx".into();
            wdef.project.hal_dep = stm32_pin_data::hal_dep_for_name("stm32wba", "STM32WBA55CGUx");
            let mut w = wdef.build_mcu();
            let l = limits_for(&w.family, w.runtime);
            let pclk1 = 100_000_000;
            w.watchdog = WatchdogSettings {
                iwdg: Some(IwdgConfig {
                    timeout_us: iwdg_range_us(&l).1,
                }),
                wwdg: Some(WwdgConfig {
                    timeout_us: wwdg_range_us(&l, pclk1).unwrap().1,
                    window_us: 0,
                }),
                ..Default::default()
            };
            let w_main = w.fresh_main_rs();
            let wfiles = manifest_sync::project_files(&wdef, &w, &w_main);
            let wuser: Vec<(String, String)> = w.pin_tree_files();
            let bdir = std::env::temp_dir().join("eide_wba_check_wdg");
            project_gen::clear_project_dir_keep_target(&bdir);
            project_gen::write_project(&bdir, &wfiles, &wuser, &w.mcu_config_text(), "")
                .expect("write wba watchdog project");
            println!("wrote {}", bdir.display());
            println!("target: {}", wdef.project.target);
        }

        // ── The F1 blocking USART, unchanged by this migration ───────────────
        // `embedded-io` is shared by both seams, so bumping it for embassy has
        // to be proved harmless for the stm32f1xx-hal bridge too.
        let f1 = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut m1 = f1.build_mcu();
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
        ] {
            let num = m1
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| m1.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        m1.reconcile_modules();
        // The IWDG too: the F1 takes a DIFFERENT HAL, unit and method names
        // from every embassy family, so its template only counts as working
        // once stm32f1xx-hal has actually compiled it.
        {
            use crate::panels::mcu_module::watchdog::{
                IwdgConfig, WatchdogSettings, iwdg_range_us, limits_for,
            };
            m1.watchdog = WatchdogSettings {
                iwdg: Some(IwdgConfig {
                    // The Reset default: the longest period this HAL accepts.
                    // Its own arithmetic, not embassy's - 42 ms shorter, and
                    // every millisecond of that gap panics.
                    timeout_us: iwdg_range_us(&limits_for(&m1.family, m1.runtime)).1,
                }),
                ..Default::default()
            };
        }
        let main_rs = m1.fresh_main_rs();
        let files = manifest_sync::project_files(&f1, &m1, &main_rs);
        let user: Vec<(String, String)> = m1.pin_tree_files();
        let f1dir = std::env::temp_dir().join("eide_f1_check_usart");
        project_gen::clear_project_dir_keep_target(&f1dir);
        project_gen::write_project(&f1dir, &files, &user, &m1.mcu_config_text(), "")
            .expect("write f1 project");
        println!("wrote {}", f1dir.display());
        println!("target: {}", f1.project.target);
    }

    /// Comparators on a real STM32G474, both constructors.
    ///
    /// COMP1 against an internal reference (`Comp::new`) and COMP4 against a
    /// pin (`new_with_input_minus_pin`), which are different call shapes AND
    /// different `Irqs` keys — `COMP1_2_3` and `COMP4_5_6` on this part, where
    /// a G431 would use plain `COMP4`. Only a compiler can confirm any of that.
    ///
    /// `EIDE_COMP_XML` picks another part (a U5 or WBA: COMP1 and COMP2 on one
    /// `COMP` vector), `EIDE_COMP_RUNTIME=blocking` the Blocking runtime, and
    /// `EIDE_COMP_UPGRADE=hand` (with it) an old Blocking project whose own
    /// code wires COMP1 - see the branch below.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_comp_project -- --ignored --nocapture
    /// cd %TEMP%\eide_comp_check && cargo check --target thumbv7em-none-eabihf
    /// ```
    #[test]
    #[ignore = "needs the STM32Cube database, writes a project for a manual cross-compile"]
    fn emit_comp_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::codegen::{dma_data, nvic};
        use crate::panels::mcu_module::comparator::{
            self, CompConfig, Hysteresis, InvertingInput, PowerMode,
        };

        let Some(path) = vendor_chip_file("EIDE_COMP_XML", "STM32G474R(B-C-E)Tx.xml") else {
            eprintln!(
                "STM32G474R(B-C-E)Tx.xml is in none of this machine's chip sources - nothing emitted"
            );
            return;
        };
        let path = path.as_path();
        let Ok(xml) = std::fs::read_to_string(path) else {
            eprintln!("could not read {} - nothing emitted", path.display());
            return;
        };
        let af = stm32_pin_data::gpio_ip_version(&xml).and_then(|v| {
            let f = path
                .parent()?
                .join("IP")
                .join(stm32_pin_data::gpio_ip_file_name(&v));
            Some(stm32_pin_data::GpioAf::parse(
                &std::fs::read_to_string(f).ok()?,
            ))
        });
        let mut def = stm32_pin_data::convert_xml_with_af(&xml, af.as_ref())
            .expect("converts")
            .remove(0)
            .form
            .to_definition();
        let mut c1 = std::collections::HashMap::new();
        def.dma = dma_data::dma_def_for(&xml, path.parent(), &mut c1);
        let mut c2 = std::collections::HashMap::new();
        def.irq_vectors = nvic::vectors_for(&xml, path.parent(), &mut c2);
        def.usart_ip = stm32_pin_data::usart_ip_version(&xml);
        def.sdmmc_ip = stm32_pin_data::sdmmc_ip_version(&xml);

        let mut mcu = def.build_mcu();
        // `EIDE_COMP_RUNTIME=blocking`: the same two comparators under
        // `#[entry]`, bound by the Blocking runtime's own `bind_interrupts!`.
        let blocking = std::env::var("EIDE_COMP_RUNTIME").as_deref() == Ok("blocking");
        mcu.runtime = if blocking {
            crate::panels::mcu_module::mcu::model::Runtime::Blocking
        } else {
            crate::panels::mcu_module::mcu::model::Runtime::Async
        };
        mcu.pending_runtime = mcu.runtime;
        let instances = comparator::instances(&mcu);
        let generation = comparator::Generation::of(&def.family)
            .unwrap_or_else(|| panic!("{} has no comparator driver", def.family));
        println!(
            "comparators on {}: {instances:?} ({generation:?})",
            def.display_name
        );
        // The first two the chip has: 1 and 4 on a G4 (different vectors), 1
        // and 2 on a U5/WBA (which share one).
        let (a, b) = match generation {
            comparator::Generation::V2 => (1u8, 4u8),
            comparator::Generation::U5 => (1u8, 2u8),
        };
        assert!(
            instances.contains(&a) && instances.contains(&b),
            "{instances:?}"
        );

        // Wire A's INP, and B's INP + INM.
        for want in [
            format!("COMP{a}_INP"),
            format!("COMP{b}_INP"),
            format!("COMP{b}_INM"),
        ] {
            let want = want.as_str();
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset
                        && p.available_functions
                            .iter()
                            .any(|f| matches!(f, PinFunction::Other(s) if s == want))
                })
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => p.selected_function = PinFunction::Other(want.into()),
                None => panic!("no pin offers {want}"),
            }
        }
        // A hysteresis level this generation can actually express.
        let hyst = match generation {
            comparator::Generation::V2 => Hysteresis::Mv30,
            comparator::Generation::U5 => Hysteresis::Medium,
        };
        mcu.comp.insert(
            a,
            CompConfig {
                power_mode: PowerMode::MediumSpeed,
                hysteresis: hyst,
                inverting_input: InvertingInput::ThreeQuarterVref,
                ..CompConfig::default()
            },
        );
        mcu.comp.insert(
            b,
            CompConfig {
                inverting_input: InvertingInput::InputPin,
                ..CompConfig::default()
            },
        );

        // `EIDE_COMP_UPGRADE=hand` (Blocking): a project from before the
        // Blocking runtime built comparators, whose own code wires COMP{a}
        // from its raw pad and binds its vector itself, the way embassy's
        // examples do it: a `use` of `bind_interrupts` and `peripherals` and a
        // `struct Irqs` below `main`. Opened, COMP{a} - and every comparator on
        // its vector - must start with "Generate driver" off, and the project
        // build with that code next to whatever is still generated: COMP{b}
        // on a G474, where the two are on different vectors; nothing on a
        // U5/WBA, where both share `COMP`.
        let by_hand = std::env::var("EIDE_COMP_UPGRADE").as_deref() == Ok("hand");
        let vector = |n: u8| {
            crate::panels::mcu_module::codegen::embassy_stm32::comp_irq(
                &mcu.irq_vectors,
                n,
                &instances,
            )
        };
        let shared = vector(a) == vector(b);
        let main_rs = if by_hand {
            assert!(blocking, "EIDE_COMP_UPGRADE=hand is a Blocking project");
            let inp = mcu
                .iter_all_pins()
                .find(|p| p.selected_function == PinFunction::Other(format!("COMP{a}_INP")))
                .map(super::binding_var)
                .unwrap_or_else(|| panic!("COMP{a}_INP is not wired"));
            let va = vector(a);
            // The pads bound raw, as every Blocking project bound them then.
            mcu.raw_comps.insert(a);
            mcu.raw_comps.insert(b);
            let old = mcu.fresh_main_rs();
            mcu.raw_comps.clear();
            let end = old.find(super::GEN_END).expect("the block") + super::GEN_END.len();
            let hand = format!(
                "\n    let mut wired_by_hand = embassy_stm32::comp::Comp::new(\n        \
                 p.COMP{a}, {inp}, Irqs, embassy_stm32::comp::Config::default());\n    \
                 wired_by_hand.enable();\n    \
                 let _level = wired_by_hand.output_level();\n"
            );
            let items = format!(
                "\nuse embassy_stm32::{{bind_interrupts, peripherals}};\n\n\
                 bind_interrupts!(struct Irqs {{\n    \
                 {va} => embassy_stm32::comp::InterruptHandler<peripherals::COMP{a}>;\n\
                 }});\n"
            );
            let old = format!("{}{hand}{}{items}", &old[..end], &old[end..]);
            // What the open path does with a config that has no comparator
            // marker in `@drivers`.
            mcu.raw_comps = mcu.comparators_used_by_hand(&old);
            assert!(
                mcu.raw_comps.contains(&a) && mcu.raw_comps.contains(&b) == shared,
                "COMP{a} wired by hand, COMP{b} {}: {:?}",
                if shared { "on its vector" } else { "not" },
                mcu.raw_comps
            );
            mcu.update_main_rs(&old)
        } else {
            mcu.fresh_main_rs()
        };
        // Whatever the vectors are called on this part, both comparators must
        // be initialised AND bound. Derived, not hardcoded: a G474 uses
        // `COMP1_2_3` and `COMP4_5_6`, a U5 puts both on one vector named
        // `COMP`, and the point is that the emitter agrees with the chip. The
        // hand-wired one is the user's, bound by their own struct.
        let generated: &[u8] = match (by_hand, shared) {
            (false, _) => &[a, b],
            (true, false) => &[b],
            (true, true) => &[],
        };
        if blocking && !generated.is_empty() {
            let bind = main_rs.find("bind_interrupts!(struct CompIrqs {");
            let entry = main_rs.find("#[entry]");
            assert!(
                matches!((bind, entry), (Some(i), Some(e)) if i < e),
                "Blocking binds above #[entry]:\n{main_rs}"
            );
        }
        if by_hand {
            assert!(
                !main_rs.contains(&format!("pins::configs::comp{a}::init(")),
                "{main_rs}"
            );
            assert!(main_rs.contains("wired_by_hand.enable();"), "{main_rs}");
            assert!(
                main_rs.contains("bind_interrupts!(struct Irqs {"),
                "{main_rs}"
            );
        }
        let peri = if blocking {
            "embassy_stm32::peripherals::"
        } else {
            "peripherals::"
        };
        for &n in generated {
            assert!(
                main_rs.contains(&format!("pins::configs::comp{n}::init(p.COMP{n},")),
                "COMP{n} not initialised:
{main_rs}"
            );
            assert!(
                main_rs.contains(&format!(
                    "embassy_stm32::comp::InterruptHandler<{peri}COMP{n}>"
                )),
                "COMP{n} not bound:
{main_rs}"
            );
        }
        for line in main_rs
            .lines()
            .filter(|l| l.contains("comp::InterruptHandler"))
        {
            println!("  bind: {}", line.trim());
        }

        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        let configs = mcu.config_files();
        println!(
            "config files: {:?}",
            configs.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        // `EIDE_COMP_UPGRADE=edited`: COMP{a} as an older IDE wrote it, with
        // lines of the user's naming what its block imported, upgraded by the
        // app's own sync - so what an upgrade leaves behind is what compiles.
        let user: Vec<(String, String)> = if std::env::var("EIDE_COMP_UPGRADE").as_deref()
            == Ok("edited")
        {
            let name = format!("comp{a}.rs");
            let body = &configs
                .iter()
                .find(|(n, _)| *n == name)
                .expect("the comp file")
                .1;
            let legacy = crate::panels::mcu_module::codegen::embassy_stm32::legacy_comp_file(body)
                .replace(
                    "    comp.enable();\n",
                    concat!(
                        "    comp.enable();\n",
                        "    comp.set_blanking_source(BlankingSource::Blank1);\n",
                        "    let _c: Config = get_config();\n",
                    ),
                );
            let mut tree = crate::project_tree::logic::ProjectTreeState::new();
            tree.user_src_files
                .push((format!("src/pins/configs/{name}"), legacy));
            tree.sync_config_files(&configs, &mcu.custom_keep_prefixes(), &mut None);
            tree.sync_pin_files(&mcu.all_pin_functions());
            let text = &tree
                .user_src_files
                .iter()
                .find(|(p, _)| p.ends_with(&name))
                .expect("upgraded")
                .1;
            assert!(
                text.contains("use embassy_stm32::comp::{BlankingSource, Config};")
                    && text.contains("fn get_config("),
                "{text}"
            );
            println!("upgraded {name}");
            tree.user_src_files
        } else {
            mcu.pin_tree_files()
        };
        let dir = std::env::temp_dir().join("eide_comp_check");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write comp project");
        println!("wrote {} ({})", dir.display(), def.display_name);
        println!(
            "target: {}  hal: {}",
            def.project.target, def.project.hal_dep
        );
    }

    /// The F1's BLOCKING DMA transport — `stm32f1xx-hal`'s own, not embassy's.
    ///
    /// Every assertion about it is a substring check until a compiler sees it,
    /// and the shapes are unusual enough to deserve one: a transfer consumes the
    /// handle, the channels are fixed in the TYPE (so `init`'s signature has to
    /// name the same ones `main.rs` passes), and the SPI handle carries the pins
    /// and the remap state as generic parameters.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_f1_dma_project -- --ignored --nocapture
    /// cd %TEMP%\eide_f1_check_dma && cargo check --target thumbv7m-none-eabi
    /// ```
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_f1_dma_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::modules::ModuleConfig;
        use crate::panels::mcu_module::modules::model::BlockingDma;

        let f1 = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut mcu = f1.build_mcu();
        // `EIDE_USB` takes PA11/PA12 away from the CAN and gives them to the USB
        // — the two peripherals share those pads (and the SRAM behind them).
        //
        //   both     — D- and D+, the ordinary CDC device
        //   dm | dp  — only that pad wired, which is the case under test
        //   dm-gpio  — D- wired and PA12 given to a GPIO output, so the pad the
        //              USB block takes unasked is one the user spent elsewhere
        let usb_pads: Option<Vec<(&str, PinFunction)>> = match std::env::var("EIDE_USB").as_deref()
        {
            Ok("both") => Some(vec![
                ("PA11", PinFunction::UsbDm),
                ("PA12", PinFunction::UsbDp),
            ]),
            Ok("dm") => Some(vec![("PA11", PinFunction::UsbDm)]),
            Ok("dp") => Some(vec![("PA12", PinFunction::UsbDp)]),
            Ok("dm-gpio") => Some(vec![
                ("PA11", PinFunction::UsbDm),
                ("PA12", PinFunction::GpioOutput),
            ]),
            _ => None,
        };
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            // `EIDE_SPI_TXONLY=1` leaves MISO unwired. On F1 that is not a different
            // constructor, it is the HAL's `NoMiso` placeholder in `SpiPins`.
            ("PA6", PinFunction::SpiMiso(1)),
            // I2C1's default (non-remapped) pads, for the `EIDE_I2C_HALF` case
            // below — `i2c::Pins<I2C1>` exists only for this exact pair.
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
            // bxCAN's default pads. `can::Pins` is a PAIR too (PA12/PA11, or
            // PB9/PB8 remapped) — `EIDE_CAN_HALF` below wires only one.
            //
            // USB lives on the SAME two pads (PA11 = D-, PA12 = D+) and cannot
            // coexist with the CAN, so `EIDE_USB` re-purposes them instead.
            ("PA12", PinFunction::CanTx),
            ("PA11", PinFunction::CanRx),
            // An ADC channel and a bare analog pad: both bind and both sit
            // unused until the reader writes a read, exactly like a GPIO.
            ("PA0", PinFunction::AdcChannel { adc: 1, channel: 0 }),
            ("PA1", PinFunction::GpioAnalog),
            // Two channels of ONE timer — TIM2 CH3/CH4 on their default pads.
            // PA0/PA1 (CH1/CH2) are taken by the ADC above, which is exactly the
            // case the hardcoded `(pa0, pa1)` comment used to claim anyway.
            (
                "PA2",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 3,
                },
            ),
            (
                "PA3",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 4,
                },
            ),
        ] {
            if usb_pads.is_some() && matches!(func, PinFunction::CanTx | PinFunction::CanRx) {
                continue;
            }
            if func == PinFunction::SpiMiso(1) && std::env::var("EIDE_SPI_TXONLY").is_ok() {
                continue;
            }
            // `EIDE_USART_HALF=tx|rx` wires only that pad of the USART. The HAL
            // has no `NoTx`/`NoRx` to stand in for the other — `serial::Pins` is
            // implemented for the PAIR and nothing else — so both spellings are
            // cases that must NOT generate an init.
            let dropped = match std::env::var("EIDE_USART_HALF").as_deref() {
                Ok("tx") => Some(PinFunction::UsartRx(1)),
                Ok("rx") => Some(PinFunction::UsartTx(1)),
                _ => None,
            };
            if Some(&func) == dropped.as_ref() {
                continue;
            }
            // `EIDE_I2C_HALF=scl|sda` wires only that wire. Same story as the
            // USART, and a two-wire bus with one wire is not even arguable.
            let dropped = match std::env::var("EIDE_I2C_HALF").as_deref() {
                Ok("scl") => Some(PinFunction::I2cSda(1)),
                Ok("sda") => Some(PinFunction::I2cScl(1)),
                _ => None,
            };
            if Some(&func) == dropped.as_ref() {
                continue;
            }
            // `EIDE_CAN_HALF=tx|rx` wires only that pad of the CAN transceiver.
            let dropped = match std::env::var("EIDE_CAN_HALF").as_deref() {
                Ok("tx") => Some(PinFunction::CanRx),
                Ok("rx") => Some(PinFunction::CanTx),
                _ => None,
            };
            if Some(&func) == dropped.as_ref() {
                continue;
            }
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        for (name, func) in usb_pads.clone().unwrap_or_default() {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        // `EIDE_F1_IRQ=rising|falling|both` arms a GPIO input with an edge.
        // Off the RTIC path this becomes a bare-metal `#[interrupt]` over a
        // static — a different shape from the hardware task, and only a
        // compiler settles whether it is the right one.
        if let Ok(e) = std::env::var("EIDE_F1_IRQ") {
            use crate::panels::mcu_module::pins::logic::pin::Edge;
            let edge = match e.as_str() {
                "falling" => Edge::Falling,
                "both" => Edge::Both,
                _ => Edge::Rising,
            };
            // An input if one is already wired, else the first free pad.
            let num = mcu
                .iter_all_pins()
                .find(|p| p.selected_function == PinFunction::GpioInput)
                .or_else(|| {
                    mcu.iter_all_pins()
                        .find(|p| !p.reserved && p.selected_function == PinFunction::Unset)
                })
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = Some(edge);
                }
                None => println!("no pad free to arm"),
            }
        }
        arm_shared_exti9_5(&mut mcu);
        mcu.reconcile_modules();
        // `EIDE_F1_DMA=tx|rx|both` picks which halves run on DMA. Each is a
        // DIFFERENT set of HAL types, so each needs its own compile - `TxDma`
        // and `Tx` share no methods at all.
        let halves = match std::env::var("EIDE_F1_DMA").as_deref() {
            Ok("tx") => BlockingDma::Tx,
            Ok("rx") => BlockingDma::Rx,
            Ok("off") => BlockingDma::Off,
            _ => BlockingDma::Both,
        };
        println!("halves: {halves:?}");
        for m in &mut mcu.modules {
            match &mut m.config {
                ModuleConfig::Usart(c) => c.blocking_dma = halves,
                ModuleConfig::Spi(c) => c.blocking_dma = halves,
                // Non-default settings on purpose: 20 kHz and two different
                // duties, so the generated code cannot pass by accident on the
                // module's defaults (1 kHz, every channel 0 %).
                ModuleConfig::Timer(c) => {
                    c.freq_hz = 20_000;
                    c.set_duty_x100(3, 7_500);
                    c.set_duty_x100(4, 1_000);
                }
                _ => {}
            }
        }
        let main_rs = mcu.fresh_main_rs();
        // `EIDE_F1_ADC=example`: the ADC at a sample time off the HAL's
        // default, and below the block the reads `adc1.rs`'s example shows,
        // plus the line an older project set the sample time with - naming
        // `adc::`, which keeps the import (`stm32::adc_import_alias`).
        // Compiled, so the example and the alias are both what builds.
        let main_rs = if std::env::var("EIDE_F1_ADC").as_deref() == Ok("example") {
            use crate::panels::mcu_module::codegen::GEN_END;
            crate::panels::mcu_module::adc::set_f1_sample_time(&mut mcu.adc, 1, 7);
            let mine = concat!(
                "    let raw: u16 = _adc1.read(&mut pa0_adc1_in0).unwrap();\n",
                "    let _mv = u32::from(raw) * 3300 / 4095;\n",
                "    let _vref: u16 = _adc1.read_vref();\n",
                "    let _celsius: i32 = _adc1.read_temp();\n",
                "    _adc1.set_sample_time(adc::SampleTime::T_239);\n",
            );
            let end = main_rs.find(GEN_END).expect("the block");
            let old = format!(
                "{}{}",
                &main_rs[..end],
                main_rs[end..].replacen("    loop {\n", &format!("{mine}    loop {{\n"), 1)
            );
            let main_rs = mcu.update_main_rs(&old);
            assert!(
                main_rs.contains("\n    adc,\n") && main_rs.contains(mine),
                "{main_rs}"
            );
            main_rs
        } else {
            main_rs
        };
        // The numbers live in the timer's config module now; main.rs only calls
        // its `init`. Both halves are checked, so neither can quietly go missing.
        assert!(
            main_rs.contains("pins::configs::pwm2::init(dp.TIM2"),
            "main.rs must call the timer's init:\n{main_rs}"
        );
        let pwm2 = mcu
            .config_files()
            .into_iter()
            .find(|(n, _)| n == "pwm2.rs")
            .map(|(_, b)| b)
            .unwrap_or_default();
        assert!(
            pwm2.contains("pub const FREQUENCY_HZ: u32 = 20000;")
                && pwm2.contains("pub const DUTY_CH3_X100: u32 = 7500;"),
            "the module's frequency and duty must reach the code:\n{pwm2}"
        );
        // What each peripheral is left asking for once its WIRING has had its
        // say, which is not always what its module says:
        //  · `EIDE_USART_HALF=tx|rx` unwires the other pad, and this HAL has no
        //    placeholder for it — USART1 is not built at all.
        //  · `EIDE_SPI_TXONLY=1` unwires MISO — the SPI keeps its TX half only.
        let usart_built = std::env::var("EIDE_USART_HALF").is_err();
        let usart_halves = if usart_built {
            halves
        } else {
            BlockingDma::Off
        };
        let spi_halves = if std::env::var("EIDE_SPI_TXONLY").is_ok() {
            halves.without_rx()
        } else {
            halves
        };
        // One split for whichever peripherals are left.
        assert_eq!(
            main_rs.matches("dp.DMA1.split()").count(),
            usize::from(usart_halves.any() || spi_halves.any()),
            "the channels are moved out one at a time - a second split would be a \
             second owner:\n{main_rs}"
        );
        // Only the halves in use contribute an argument, in `init`'s order.
        // `tx_first` is the peripheral's own parameter order: the USART takes
        // TX then RX, the SPI takes RX then TX (`with_rx_tx_dma`'s order).
        let args = |h: BlockingDma, tx: &str, rx: &str, tx_first: bool| match h {
            BlockingDma::Both if tx_first => format!("&clocks, {tx}, {rx})"),
            BlockingDma::Both => format!("&clocks, {rx}, {tx})"),
            BlockingDma::Tx => format!("&clocks, {tx})"),
            BlockingDma::Rx => format!("&clocks, {rx})"),
            BlockingDma::Off => "&clocks)".to_owned(),
        };
        let usart_args = args(usart_halves, "dma1.4", "dma1.5", true);
        let spi_args = args(spi_halves, "dma1.3", "dma1.2", false);
        let (usart_args, spi_args) = (usart_args.as_str(), spi_args.as_str());
        if usart_built {
            assert!(
                main_rs.contains(usart_args),
                "USART1 wants {usart_args}:\n{main_rs}"
            );
        } else {
            assert!(
                !main_rs.contains("configs::usart1::init")
                    && main_rs.contains("USART1 is NOT initialised"),
                "half a USART must not be initialised, and must say so:\n{main_rs}"
            );
        }
        assert!(
            main_rs.contains(spi_args),
            "SPI1 wants {spi_args}:\n{main_rs}"
        );
        // The I2C takes no DMA on this HAL, so it only has to be there — or,
        // with one wire unwired, be absent WITH a reason.
        if std::env::var("EIDE_I2C_HALF").is_ok() {
            assert!(
                !main_rs.contains("configs::i2c1::init")
                    && main_rs.contains("I2C1 is NOT initialised"),
                "half an I2C must not be initialised, and must say so:\n{main_rs}"
            );
        } else {
            assert!(main_rs.contains("configs::i2c1::init"), "{main_rs}");
        }
        // Same rule again for the CAN, plus the USB token `Can::new` demands.
        // Skipped entirely when `EIDE_USB` took the CAN's pads — and there the
        // USB gets the same treatment: both data pads, or a reason.
        if let Some(pads) = &usb_pads {
            assert!(!main_rs.contains("configs::can1::init"), "{main_rs}");
            let both = pads.iter().any(|(_, f)| *f == PinFunction::UsbDm)
                && pads.iter().any(|(_, f)| *f == PinFunction::UsbDp);
            if both {
                assert!(main_rs.contains("UsbBus::new(usb_periph)"), "{main_rs}");
            } else {
                assert!(
                    !main_rs.contains("UsbBus::new") && main_rs.contains("USB is NOT initialised"),
                    "half a USB must not be initialised, and must say so:\n{main_rs}"
                );
                // The whole point: the pad it used to take uninvited. Matched
                // through the USB block's own binding, because a GPIO module on
                // PA12 configures that pad legitimately.
                assert!(
                    !main_rs.contains("let mut usb_dp = gpioa.pa12"),
                    "{main_rs}"
                );
            }
        } else if std::env::var("EIDE_CAN_HALF").is_ok() {
            assert!(
                !main_rs.contains("configs::can1::init")
                    && main_rs.contains("CAN1 is NOT initialised"),
                "half a CAN must not be initialised, and must say so:\n{main_rs}"
            );
        } else {
            assert!(
                main_rs.contains("configs::can1::init(dp.CAN1, dp.USB,"),
                "bxCAN shares SRAM with USB, so the HAL takes the USB token:\n{main_rs}"
            );
        }

        let files = manifest_sync::project_files(&f1, &mcu, &main_rs);
        // Two named devices on I2C1: the bus is a folder, and each device file
        // is a module of it.
        // A half-wired I2C (`EIDE_I2C_HALF`) may have no module to put them on.
        let on_bus = mcu.with_i2c_devices(&[("oled", 0x3C), ("imu", 0x68)]);
        assert!(
            on_bus || std::env::var("EIDE_I2C_HALF").is_ok(),
            "no I2C bus for the devices"
        );
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_f1_check_dma");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write f1 dma project");
        println!("wrote {}", dir.display());
        println!("target: {}", f1.project.target);
    }

    /// The F103 on the Async runtime - embassy-stm32 instead of stm32f1xx-hal -
    /// written the way the application writes it: `main.rs` and `Cargo.toml`
    /// both come through a runtime SWITCH from the blocking project, and the
    /// manifest through the same `refresh_hal_dependency` + `ensure_*` chain
    /// `app.rs` runs. What only a compiler settles: the AFIO remap generic on
    /// every bus pin, the concrete remap on a timer, the F1 clock block, the
    /// `swj` release, the DMA channel names and the EXTI vector.
    ///
    /// Knobs:
    /// * `EIDE_F1_ASYNC_REMAP=1` - USART1 on PB6/PB7 and I2C1 on PB8/PB9, both
    ///   remapped (the default is their unremapped pads plus TIM4 on PB8, which
    ///   the time driver owns, so that timer must come out as a note);
    /// * `EIDE_F1_ASYNC_DMA=1` - USART1, SPI1 and I2C1 all on DMA;
    /// * `EIDE_F1_ASYNC_UART=tx|rx|half|flow` - USART3 in a shape only this
    ///   runtime builds on the F1: TX only with CTS, RX only with RTS (both on
    ///   DMA), a single wire on the TX pad (DMA, readback), or full duplex
    ///   with CTS/RTS on the buffered transport;
    /// * `EIDE_F1_SWITCH=back` - also writes the project switched BACK to
    ///   Blocking, to `eide_f1_check_async_back`.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_f1_async_project -- --ignored --nocapture
    /// cd %TEMP%\eide_f1_check_async && cargo check --target thumbv7m-none-eabi
    /// ```
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_f1_async_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::mcu_def::build_cfg;
        use crate::panels::mcu_module::modules::{AsyncBusMode, ModuleConfig, UsartMode};
        use crate::panels::mcu_module::pins::logic::pin::Edge;
        use crate::panels::mcu_module::watchdog::{IwdgConfig, WwdgConfig};

        let remap = std::env::var("EIDE_F1_ASYNC_REMAP").is_ok();
        let dma = std::env::var("EIDE_F1_ASYNC_DMA").is_ok();
        let def = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut mcu = def.build_mcu();
        let mut wiring: Vec<(&str, PinFunction)> = vec![
            ("PA5", PinFunction::SpiSck(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            // TIM2 CH3/CH4: the same pads unremapped and partly remapped, so
            // the remap has to be NAMED (E0283 otherwise).
            (
                "PA2",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 3,
                },
            ),
            (
                "PA3",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 4,
                },
            ),
            ("PC13", PinFunction::GpioOutput),
            // A JTAG pad: without `swj = SwdOnly` it compiles and does nothing.
            ("PB3", PinFunction::GpioOutput),
            // The CAN pads, which an F1 project switched to Async keeps on the
            // canvas: bound raw, with the note that says why.
            ("PA11", PinFunction::CanRx),
            ("PA12", PinFunction::CanTx),
        ];
        if remap {
            wiring.extend([
                ("PB6", PinFunction::UsartTx(1)),
                ("PB7", PinFunction::UsartRx(1)),
                ("PB8", PinFunction::I2cScl(1)),
                ("PB9", PinFunction::I2cSda(1)),
            ]);
        } else {
            wiring.extend([
                ("PA9", PinFunction::UsartTx(1)),
                ("PA10", PinFunction::UsartRx(1)),
                ("PB6", PinFunction::I2cScl(1)),
                ("PB7", PinFunction::I2cSda(1)),
                // TIM4 is embassy-time's on this chip: a note, not an init.
                (
                    "PB8",
                    PinFunction::TimerPwm {
                        timer: 4,
                        channel: 3,
                    },
                ),
            ]);
        }
        // USART3 on its free pads (PB10/PB11, CTS PB13, RTS PB14), wired for
        // the shape alone - a one-way UART has no other data pad. Its fixed
        // DMA channels (DMA1_CH2/CH3) are SPI1's, hence not with the DMA knob.
        let uart3 = std::env::var("EIDE_F1_ASYNC_UART").ok();
        if let Some(shape) = uart3.as_deref() {
            assert!(
                !dma,
                "USART3's DMA1_CH2/CH3 are SPI1's: not with EIDE_F1_ASYNC_DMA"
            );
            wiring.extend(match shape {
                "tx" => vec![
                    ("PB10", PinFunction::UsartTx(3)),
                    ("PB13", PinFunction::UsartCts(3)),
                ],
                "rx" => vec![
                    ("PB11", PinFunction::UsartRx(3)),
                    ("PB14", PinFunction::UsartRts(3)),
                ],
                "half" => vec![("PB10", PinFunction::UsartTx(3))],
                "flow" => vec![
                    ("PB10", PinFunction::UsartTx(3)),
                    ("PB11", PinFunction::UsartRx(3)),
                    ("PB13", PinFunction::UsartCts(3)),
                    ("PB14", PinFunction::UsartRts(3)),
                ],
                other => panic!("EIDE_F1_ASYNC_UART={other}: tx, rx, half or flow"),
            });
        }
        for (name, func) in wiring {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number)
                .unwrap_or_else(|| panic!("no pad {name}"));
            let p = mcu.find_pin_mut(num).expect("the pad just found");
            p.selected_function = func;
        }
        // An armed input on EXTI line 5 (EXTI9_5).
        let pb5 = mcu
            .iter_all_pins()
            .find(|p| p.name == "PB5")
            .map(|p| p.number)
            .expect("PB5");
        let p = mcu.find_pin_mut(pb5).expect("PB5");
        p.selected_function = PinFunction::GpioInput;
        p.irq = Some(Edge::Both);
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            match &mut m.config {
                ModuleConfig::Usart(c) if c.instance == 3 => {
                    use crate::panels::mcu_module::modules::{UsartDirection as D, UsartFlow as F};
                    (c.mode, c.direction, c.flow) = match uart3.as_deref() {
                        Some("tx") => (UsartMode::Dma, D::TxOnly, F::Cts),
                        Some("rx") => (UsartMode::Dma, D::RxOnly, F::Rts),
                        Some("half") => {
                            c.half_duplex_readback = true;
                            (UsartMode::Dma, D::HalfDuplexOnTx, F::None)
                        }
                        _ => (UsartMode::Buffered, D::TxRx, F::CtsRts),
                    };
                }
                ModuleConfig::Usart(c) if dma => c.mode = UsartMode::Dma,
                ModuleConfig::Spi(c) if dma => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::I2c(c) if dma => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::Timer(c) => {
                    c.freq_hz = 20_000;
                    c.set_duty_x100(3, 7_500);
                }
                _ => {}
            }
        }
        mcu.watchdog.iwdg = Some(IwdgConfig {
            timeout_us: 2_000_000,
        });
        mcu.watchdog.wwdg = Some(WwdgConfig {
            timeout_us: 20_000,
            window_us: 0,
        });

        // Through the switch, the way a user gets here.
        let mut blocking = mcu.clone();
        blocking.runtime = Runtime::Blocking;
        blocking.pending_runtime = Runtime::Blocking;
        mcu.runtime = Runtime::Async;
        mcu.pending_runtime = Runtime::Async;
        assert!(mcu.is_async(), "the F1 has an async path");
        let main_rs = mcu.update_main_rs(&blocking.fresh_main_rs());
        assert!(main_rs.contains("#[embassy_executor::main]"), "{main_rs}");
        assert!(!main_rs.contains("stm32f1xx_hal"), "{main_rs}");
        assert!(main_rs.contains("SwjCfg::SwdOnly"), "{main_rs}");
        assert!(main_rs.contains("CAN is NOT initialised"), "{main_rs}");
        if !remap {
            assert!(main_rs.contains("TIM4 is NOT initialised"), "{main_rs}");
        }
        if let Some(shape) = uart3.as_deref() {
            assert!(
                main_rs.contains("let mut _serial3 = pins::configs::usart3::init(p.USART3"),
                "USART3 ({shape}) not built:\n{main_rs}"
            );
            let usart3 = mcu
                .config_files()
                .into_iter()
                .find(|(n, _)| n == "usart3.rs")
                .expect("usart3.rs")
                .1;
            let ctor = match shape {
                "tx" => "UartTx::new_with_cts(",
                "rx" => "UartRx::new_with_rts(",
                "half" => "Uart::new_half_duplex(",
                _ => "BufferedUart::new_with_rtscts(",
            };
            assert!(usart3.contains(ctor), "{ctor}:\n{usart3}");
            println!("USART3 ({shape}): {ctor}..)");
        }

        let project = build_cfg(&def, Some(&mcu));
        assert!(
            project.hal_dep.starts_with("embassy-stm32"),
            "{}",
            project.hal_dep
        );
        let files = project_gen::build_project_files(&project, &def.toolchain, &main_rs);
        let blocking_cfg = build_cfg(&def, Some(&blocking));
        let blocking_toml =
            project_gen::build_project_files(&blocking_cfg, &def.toolchain, "").cargo_toml;
        assert!(blocking_toml.contains("stm32f1xx-hal"), "{blocking_toml}");
        assert_eq!(
            project_gen::refresh_hal_dependency(&blocking_toml, &project, &def.toolchain),
            files.cargo_toml,
            "the switch lands on the fresh manifest"
        );

        let configs = mcu.config_files();
        let names: Vec<&str> = configs.iter().map(|(n, _)| n.as_str()).collect();
        for want in [
            "usart1.rs",
            "spi1.rs",
            "i2c1/mod.rs",
            "pwm2.rs",
            "iwdg.rs",
            "wwdg.rs",
        ] {
            assert!(names.contains(&want), "{want} missing: {names:?}");
        }
        // The app's path, manifest included: the blocking project as the app
        // synced it, then the switch and the app's sync over what it left - so
        // the bridge crates only Blocking needs have to be STRIPPED, not just
        // never added. Each landing must be the project a fresh sync writes.
        let before = manifest_sync::project_files(&def, &blocking, &blocking.fresh_main_rs());
        assert!(before.cargo_toml.contains("embedded-io "), "{}", before.cargo_toml);
        let on_async = manifest_sync::switched(&before, &main_rs, &def, &mcu);
        let fresh = manifest_sync::project_files(&def, &mcu, &main_rs).cargo_toml;
        assert!(
            manifest_sync::same_lines(&on_async.cargo_toml, &fresh),
            "the switch to Async left:\n{}\nnot:\n{fresh}",
            on_async.cargo_toml
        );
        let write = |dir_name: &str,
                     files: &project_gen::ProjectFiles,
                     mcu: &crate::panels::mcu_module::mcu::Mcu| {
            let user = mcu.pin_tree_files();
            let dir = std::env::temp_dir().join(dir_name);
            project_gen::clear_project_dir_keep_target(&dir);
            project_gen::write_project(&dir, files, &user, &mcu.mcu_config_text(), "")
                .expect("write f1 project");
            println!("wrote {}", dir.display());
            println!("target: {}", def.project.target);
        };
        write("eide_f1_check_async", &on_async, &mcu);

        // And back: the header the Async runtime wrote has to go with it.
        if std::env::var("EIDE_F1_SWITCH").as_deref() == Ok("back") {
            let back_main = blocking.update_main_rs(&main_rs);
            assert!(back_main.contains("use cortex_m_rt::entry;"), "{back_main}");
            assert!(!back_main.contains("embassy_executor"), "{back_main}");
            let back_files =
                project_gen::build_project_files(&blocking_cfg, &def.toolchain, &back_main);
            assert_eq!(
                project_gen::refresh_hal_dependency(
                    &files.cargo_toml,
                    &blocking_cfg,
                    &def.toolchain
                ),
                back_files.cargo_toml,
                "the switch back lands on the blocking manifest"
            );
            // ...and the manifest back from the synced Async one, as the app
            // holds it: the executor's crates have to go again.
            let back = manifest_sync::switched(&on_async, &back_main, &def, &blocking);
            let fresh = manifest_sync::project_files(&def, &blocking, &back_main).cargo_toml;
            assert!(
                manifest_sync::same_lines(&back.cargo_toml, &fresh),
                "the switch back left:\n{}\nnot:\n{fresh}",
                back.cargo_toml
            );
            write("eide_f1_check_async_back", &back, &blocking);
        }
    }

    /// The SAME wiring under the RTIC runtime.
    ///
    /// `#[init]` RETURNS, so anything it builds and does not hand to the
    /// framework is dropped there — which is why `promote_bus_handles` exists.
    /// Whether every peripheral the blocking path emits survives that trip is a
    /// question only a compiler answers:
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_f1_rtic_project -- --ignored --nocapture
    /// cd %TEMP%\eide_f1_check_rtic && cargo check --target thumbv7m-none-eabi
    /// ```
    ///
    /// The Configuration tab's IWDG is on unless `EIDE_F1_WDG=0`: its handle
    /// has to leave `#[init]` as a `Local` too, and the RTIC backend once did
    /// not call it at all. `EIDE_F1_NOPINS=1` wires nothing, so the "select
    /// pins" default block - which used to drop every init - is what builds.
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_f1_rtic_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::modules::ModuleConfig;
        use crate::panels::mcu_module::pins::logic::pin::Edge;

        let f1 = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut mcu = f1.build_mcu();
        mcu.runtime = Runtime::Rtic;
        mcu.pending_runtime = Runtime::Rtic;
        let wiring = if std::env::var("EIDE_F1_NOPINS").is_ok() {
            Vec::new()
        } else {
            vec![
                ("PA9", PinFunction::UsartTx(1)),
                ("PA10", PinFunction::UsartRx(1)),
                ("PA5", PinFunction::SpiSck(1)),
                ("PA7", PinFunction::SpiMosi(1)),
                ("PA6", PinFunction::SpiMiso(1)),
                ("PB6", PinFunction::I2cScl(1)),
                ("PB7", PinFunction::I2cSda(1)),
                ("PA0", PinFunction::AdcChannel { adc: 1, channel: 0 }),
                (
                    "PA2",
                    PinFunction::TimerPwm {
                        timer: 2,
                        channel: 3,
                    },
                ),
                (
                    "PA3",
                    PinFunction::TimerPwm {
                        timer: 2,
                        channel: 4,
                    },
                ),
                ("PC13", PinFunction::GpioOutput),
                // An interrupt-enabled input is what makes this an RTIC project
                // rather than a blocking one with extra ceremony: it becomes a
                // `#[task(binds = EXTI1)]`.
                ("PB1", PinFunction::GpioInput),
            ]
        };
        for (name, func) in wiring {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
                if p.name == "PB1" {
                    p.irq = Some(Edge::Rising);
                }
            }
        }
        arm_shared_exti9_5(&mut mcu);
        if std::env::var("EIDE_F1_WDG").as_deref() != Ok("0") {
            use crate::panels::mcu_module::watchdog::IwdgConfig;
            mcu.watchdog.iwdg = Some(IwdgConfig {
                timeout_us: 2_000_000,
            });
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = 20_000;
                c.set_duty_x100(3, 7_500);
            }
        }

        let main_rs = mcu.fresh_main_rs();
        assert!(
            main_rs.contains("#[rtic::app"),
            "not an RTIC project:\n{main_rs}"
        );
        // `EIDE_F1_ADC=example`: a helper below the block set the sample time
        // the way a project from before `adc1.rs` did, naming `adc::` - which
        // keeps the import at the crate root (`stm32::adc_import_alias`).
        let main_rs = if std::env::var("EIDE_F1_ADC").as_deref() == Ok("example") {
            crate::panels::mcu_module::adc::set_f1_sample_time(&mut mcu.adc, 1, 7);
            let mine = concat!(
                "\n#[allow(dead_code)]\n",
                "fn slowest(adc1: &mut pins::configs::adc1::Handle) {\n",
                "    adc1.set_sample_time(adc::SampleTime::T_239);\n",
                "}\n",
            );
            let main_rs = mcu.update_main_rs(&format!("{main_rs}{mine}"));
            assert!(
                main_rs.contains("\n    adc,\n") && main_rs.contains(mine),
                "{main_rs}"
            );
            main_rs
        } else {
            main_rs
        };

        let files = manifest_sync::project_files(&f1, &mcu, &main_rs);
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_f1_check_rtic");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write f1 rtic project");
        println!("wrote {}", dir.display());
        println!("target: {}", f1.project.target);
    }

    /// The same wiring under the NATIVE runtime.
    ///
    /// Native forces every peripheral to the concrete `stm32f1xx-hal` type: no
    /// `embedded-io` USART bridge, no `SpiBusIo`, no `io.rs` GPIO wrapper. That
    /// is a different set of templates and a different set of dependencies, so
    /// it is a different compile:
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_f1_native_project -- --ignored --nocapture
    /// cd %TEMP%\eide_f1_check_native && cargo check --target thumbv7m-none-eabi
    /// ```
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_f1_native_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::modules::ModuleConfig;

        let f1 = builtin_for("stm32f103c8t6").expect("built-in F103");
        let mut mcu = f1.build_mcu();
        mcu.runtime = Runtime::Native;
        mcu.pending_runtime = Runtime::Native;
        for (name, func) in [
            ("PA9", PinFunction::UsartTx(1)),
            ("PA10", PinFunction::UsartRx(1)),
            ("PA5", PinFunction::SpiSck(1)),
            ("PA7", PinFunction::SpiMosi(1)),
            ("PA6", PinFunction::SpiMiso(1)),
            ("PB6", PinFunction::I2cScl(1)),
            ("PB7", PinFunction::I2cSda(1)),
            ("PA0", PinFunction::AdcChannel { adc: 1, channel: 0 }),
            (
                "PA2",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 3,
                },
            ),
            (
                "PA3",
                PinFunction::TimerPwm {
                    timer: 2,
                    channel: 4,
                },
            ),
            ("PC13", PinFunction::GpioOutput),
            ("PB1", PinFunction::GpioInput),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .map(|p| p.number);
            if let Some(p) = num.and_then(|n| mcu.find_pin_mut(n)) {
                p.selected_function = func;
            }
        }
        // `EIDE_F1_IRQ=rising|falling|both` arms a GPIO input with an edge.
        // Off the RTIC path this becomes a bare-metal `#[interrupt]` over a
        // static — a different shape from the hardware task, and only a
        // compiler settles whether it is the right one.
        if let Ok(e) = std::env::var("EIDE_F1_IRQ") {
            use crate::panels::mcu_module::pins::logic::pin::Edge;
            let edge = match e.as_str() {
                "falling" => Edge::Falling,
                "both" => Edge::Both,
                _ => Edge::Rising,
            };
            // An input if one is already wired, else the first free pad.
            let num = mcu
                .iter_all_pins()
                .find(|p| p.selected_function == PinFunction::GpioInput)
                .or_else(|| {
                    mcu.iter_all_pins()
                        .find(|p| !p.reserved && p.selected_function == PinFunction::Unset)
                })
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = Some(edge);
                }
                None => println!("no pad free to arm"),
            }
        }
        arm_shared_exti9_5(&mut mcu);
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = 20_000;
                c.set_duty_x100(3, 7_500);
            }
        }

        let main_rs = mcu.fresh_main_rs();
        // The tell-tale of Native: the USART hands back the split pair, not the
        // `embedded-io` bridge the Portable path wraps them in.
        assert!(
            main_rs.contains("let (mut _tx1, mut _rx1)"),
            "not a Native project:\n{main_rs}"
        );

        // What the app computes on this runtime: NO portable trait crates
        // (that is the whole point of Native), but `nb` stays - the concrete
        // `Tx`/`Rx` are nb-based.
        let files = manifest_sync::project_files(&f1, &mcu, &main_rs);
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_f1_check_native");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write f1 native project");
        println!("wrote {}", dir.display());
        println!("target: {}", f1.project.target);
    }

    /// The ESP32-C3, with the LEDC wired. Nothing here is a question of taste:
    /// the `Channel` borrows its timer, the duty resolution has to leave the
    /// divisor above 256, and both are things only a compiler settles.
    ///
    /// `EIDE_ESP_RUNTIME=async` builds the esp-rtos variant instead;
    /// `EIDE_ESP_PWM=0,1,2` picks which LEDC channels are wired (default 1).
    /// All three watchdogs are switched on unless `EIDE_ESP_WDG=0`: the RWDT's
    /// `Rtc` and the timer-group `Wdt`s are the only ESP files no other matrix
    /// case compiles.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_esp32c3_project -- --ignored --nocapture
    /// cd %TEMP%\eide_esp_check && cargo build --release
    /// ```
    #[test]
    #[ignore = "writes a project to disk for a manual cross-compile"]
    fn emit_esp32c3_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::modules::ModuleConfig;

        // Any bundled Espressif part, so the five generated definitions can be
        // cross-compiled the same way the hand-written one always could.
        let chip = std::env::var("EIDE_ESP_CHIP").unwrap_or_else(|_| "esp32c3".into());
        let esp = builtin_for(&chip).unwrap_or_else(|| panic!("no built-in {chip}"));
        let mut mcu = esp.build_mcu();
        if std::env::var("EIDE_ESP_RUNTIME").as_deref() == Ok("async") {
            mcu.runtime = Runtime::Async;
            mcu.pending_runtime = Runtime::Async;
        }
        let chans: Vec<u8> = std::env::var("EIDE_ESP_PWM")
            .unwrap_or_else(|_| "1".into())
            .split(',')
            .filter_map(|c| c.trim().parse().ok())
            .collect();

        // Pads are CHOSEN, not named: the GPIO numbering differs per chip (a C5
        // has no GPIO15..22 at all), so a hard-coded list would silently drop
        // half the wiring on five of the six parts.
        let mut want: Vec<PinFunction> = vec![
            PinFunction::GpioOutput,
            PinFunction::GpioInput,
            PinFunction::UsartTx(1),
            PinFunction::UsartRx(1),
            PinFunction::I2cSda(0),
            PinFunction::I2cScl(0),
        ];
        for ch in &chans {
            want.push(PinFunction::TimerPwm {
                timer: 0,
                channel: *ch,
            });
        }
        // `EIDE_ESP_NOPINS=1` wires nothing at all: the path where main.rs's GEN
        // block is the "select pins" default, which once dropped the watchdog
        // inits - and would have dropped the flash store's `flash` with them.
        if std::env::var("EIDE_ESP_NOPINS").as_deref() == Ok("1") {
            want.clear();
        }
        let mut taken: Vec<usize> = Vec::new();
        for func in want {
            let pick = mcu
                .iter_all_pins()
                .find(|p| {
                    !p.reserved
                        && !taken.contains(&p.number)
                        && p.available_functions.contains(&func)
                })
                .map(|p| p.number);
            match pick.and_then(|n| {
                taken.push(n);
                mcu.find_pin_mut(n)
            }) {
                Some(p) => p.selected_function = func,
                // Said out loud: a C61 has no PWM, and a run that quietly wired
                // five of six peripherals would still look like a pass.
                None => println!("[{chip}] no free pad for {func:?} - skipped"),
            }
        }
        // `EIDE_ESP_PULL=up|down|none` puts a pull on the GPIO input, which is
        // the only way this matrix compiles `InputConfig::default().with_pull`
        // against a real esp-hal - the enum variants are checked by the crate,
        // not by us.
        if let Ok(m) = std::env::var("EIDE_ESP_PULL") {
            use crate::panels::mcu_module::pins::logic::pin::GpioMode;
            let mode = match m.as_str() {
                "down" => GpioMode::PullDown,
                "none" => GpioMode::Floating,
                _ => GpioMode::PullUp,
            };
            let num = mcu
                .iter_all_pins()
                .find(|p| p.selected_function == PinFunction::GpioInput)
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => p.io_mode = Some(mode),
                None => println!("[{chip}] no input pin to pull"),
            }
        }
        // `EIDE_ESP_IRQ=rising|falling|both` arms the GPIO input with an edge.
        // Only the Async runtime builds it — into a task that awaits the edge —
        // so this is where that compiles or does not.
        if let Ok(e) = std::env::var("EIDE_ESP_IRQ") {
            use crate::panels::mcu_module::pins::logic::pin::{Edge, TaskPriority};
            let edge = match e.as_str() {
                "falling" => Edge::Falling,
                "both" => Edge::Both,
                _ => Edge::Rising,
            };
            let num = mcu
                .iter_all_pins()
                .find(|p| p.selected_function == PinFunction::GpioInput)
                .map(|p| p.number);
            // `EIDE_ESP_TASK_PRIO=high|critical` raises that task off the
            // shared executor onto its own InterruptExecutor. This is the ONLY
            // place the emitted `StaticCell<InterruptExecutor<N>>` +
            // `start(Priority::…)` is put in front of a real esp-rtos: the unit
            // tests assert on TEXT, and text cannot tell us the const generic,
            // the software-interrupt field and the Priority variant all exist.
            let prio = match std::env::var("EIDE_ESP_TASK_PRIO").as_deref() {
                Ok("high") => TaskPriority::High,
                Ok("critical") => TaskPriority::Critical,
                _ => TaskPriority::Normal,
            };
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => {
                    p.irq = Some(edge);
                    p.irq_priority = prio;
                }
                None => println!("[{chip}] no input pin to arm"),
            }
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            if let ModuleConfig::Timer(c) = &mut m.config {
                c.freq_hz = 20_000;
                c.custom_label = "power led".into();
                for ch in &chans {
                    c.set_duty_x100(*ch, 2_000);
                }
            }
        }

        // All three watchdogs on every chip - MWDT1 included on the C2, which
        // has no TIMG1. Dropping it there is the GENERATOR's job, and the C2
        // project compiles only if it does. `EIDE_ESP_WDG=0` leaves them out.
        let wdg = std::env::var("EIDE_ESP_WDG").as_deref() != Ok("0");
        if wdg {
            use crate::panels::mcu_module::watchdog::EspWdtConfig;
            mcu.watchdog.rwdt = Some(EspWdtConfig {
                timeout_us: 2_000_000,
            });
            mcu.watchdog.mwdt0 = Some(EspWdtConfig {
                timeout_us: 1_500_000,
            });
            mcu.watchdog.mwdt1 = Some(EspWdtConfig {
                timeout_us: 500_000,
            });
        }

        // `EIDE_ESP_FLASHSTORE=partition|nvs` switches the Configuration tab's
        // flash store on: the only place its template, the GEN lines and the
        // seeded tail line meet a real esp-hal, esp-storage and sequential-storage.
        let store = std::env::var("EIDE_ESP_FLASHSTORE").ok();
        if let Some(mode) = &store {
            use crate::panels::mcu_module::flash_store::{FlashStoreConfig, FlashStoreMode};
            let mut c = FlashStoreConfig::default_for(&chip);
            if mode == "nvs" {
                c.mode = FlashStoreMode::Nvs;
            }
            mcu.flash_store = Some(c);
        }

        // `EIDE_ESP_IOT` switches the IoT tab on (the case sets the Async
        // runtime too): the only place esp-radio, embassy-net, rust-mqtt and
        // the SNTP and ESP-NOW templates meet the generated heap, spawner and
        // files. `wifi` = the station alone, `mqtt` = station + MQTT (no
        // `esp-now` feature, so `radio.esp_now` does not even exist),
        // `espnow` = ESP-NOW with the station off (`hold_radio`), `all` =
        // station + MQTT + SNTP + ESP-NOW, `ble` = Bluetooth alone (the H2's
        // only link), `ble+espnow` = Bluetooth beside ESP-NOW with the station
        // off (`coex` with no IP stack), `ble+all` = everything at once,
        // `thread` = Thread alone on the 802.15.4 radio (C6, H2, C5:
        // openthread 0.2, prebuilt OpenThread and Mbed TLS), `mqtts` = the
        // station + MQTT over TLS with no SNTP (dates unchecked), `http` = the
        // station + the HTTP client, plain only, `https` = the station + HTTPS
        // alone (tls.rs for HTTP only), `awshttps` = HTTPS with the board's
        // certificate for AWS IoT on 443 (its ALPN) and aws_publish() called -
        // the identity seeded, so the signing is linked. `all` and `ble+all` are over TLS
        // wherever the chip can carry it, HTTPS beside MQTT's included - two
        // TLS sessions, where `iot::https_available` allows the second.
        // `EIDE_ESP_SNTP=1` adds SNTP to any mode and `EIDE_ESP_P384=1` builds
        // HTTPS's P-384 checks: a board test reaching real servers, whose
        // dates are checked and whose ECDSA Let's Encrypt chains need P-384.
        // `mesh` = Bluetooth Mesh alone with all four models (and the flash
        // store it keeps its sequence number in), `mesh+all` = the same beside
        // `all` - coexistence, the scan beside Wi-Fi.
        let iot_mode = std::env::var("EIDE_ESP_IOT").ok();
        if let Some(mode) = iot_mode.as_deref() {
            use crate::panels::mcu_module::iot::{
                BleConfig, BleMeshConfig, EspNowConfig, HttpConfig, MqttConfig, SntpConfig,
                ThreadConfig,
            };
            let all = matches!(mode, "all" | "ble+all" | "mesh+all");
            mcu.iot.wifi = matches!(mode, "wifi" | "mqtt" | "mqtts" | "http" | "https" | "awshttps" | "awsmqtt") || all;
            if matches!(mode, "http" | "https" | "awshttps") || all {
                let aws = mode == "awshttps";
                mcu.iot.http = Some(HttpConfig {
                    tls: mode != "http",
                    p384: std::env::var_os("EIDE_ESP_P384").is_some(),
                    client_cert: aws,
                    cert_host: if aws { "a1b2c3d4e5f6g7-ats.iot.eu-south-1.amazonaws.com".into() } else { String::new() },
                    aws_port: 443,
                    aws_thing: "rustonchip-test".into(),
                    ..HttpConfig::default()
                });
            }
            if matches!(mode, "ble" | "ble+espnow" | "ble+all") {
                mcu.iot.ble = Some(BleConfig::default());
            }
            if mode == "thread" {
                mcu.iot.thread = Some(ThreadConfig::default());
            }
            if matches!(mode, "mesh" | "mesh+all") {
                mcu.iot.ble_mesh = Some(BleMeshConfig {
                    address: 0x0002,
                    onoff_server: true,
                    onoff_client: true,
                    sensor_server: true,
                    sensor_client: true,
                    sensor_properties: vec![0x004F, 0x004C, 0x004E],
                    ..BleMeshConfig::default()
                });
                if mcu.flash_store.is_none() {
                    use crate::panels::mcu_module::flash_store::FlashStoreConfig;
                    mcu.flash_store = Some(FlashStoreConfig::default_for(&chip));
                }
            }
            if matches!(mode, "mqtt" | "mqtts") || all {
                let mut m = MqttConfig::for_chip(&chip);
                m.subscribe = vec!["rustonchip/cmd".into(), "rustonchip/#".into()];
                // TLS where the chip can carry it: asked for anywhere else, it
                // generates no MQTT at all (never the password in clear), and
                // the loop below calls MQTT.
                let coex = matches!(mode, "ble+all" | "mesh+all");
                m.tls = (mode == "mqtts" || all)
                    && crate::panels::mcu_module::iot::tls_available(&mcu.family, coex).is_ok();
                // The C3's TLS row also builds embedded-tls's P-384 checks.
                m.p384 = mode == "mqtts";
                // `all` presents a client certificate wherever TLS runs: the
                // signing code on every chip's TLS row (the S2 and C2 are tight).
                m.client_cert = all && m.tls;
                m.port = m.port_for_tls(m.tls);
                mcu.iot.mqtt = Some(m);
            }
            // MQTT on AWS IoT with the board's certificate and the Device
            // Shadow, a named one beside the classic: every shadow call linked.
            if mode == "awsmqtt" {
                mcu.iot.mqtt = Some(MqttConfig {
                    host: "a1b2c3d4e5f6g7-ats.iot.eu-south-1.amazonaws.com".into(),
                    port: 8883,
                    client_id: "rustonchip-test".into(),
                    tls: true,
                    client_cert: true,
                    aws_shadow: true,
                    shadow_names: vec!["config".into()],
                    shadow_max: 2048,
                    subscribe: vec!["rustonchip-test/cmd".into()],
                    ..MqttConfig::for_chip(&chip)
                });
            }
            if all || std::env::var_os("EIDE_ESP_SNTP").is_some() {
                mcu.iot.sntp = Some(SntpConfig::default());
            }
            if matches!(mode, "espnow" | "ble+espnow") || all {
                mcu.iot.esp_now = Some(EspNowConfig {
                    channel: 6,
                    peers: vec![[0x24, 0x0A, 0xC4, 0x12, 0x34, 0x56]],
                });
            }
            mcu.iot.ip.dhcp = mode != "mqtt";
        }

        let mut main_rs = mcu.fresh_main_rs();
        if let Some(mode) = iot_mode.as_deref() {
            let all = matches!(mode, "all" | "ble+all" | "mesh+all");
            let ble = matches!(mode, "ble" | "ble+espnow" | "ble+all");
            let mesh = matches!(mode, "mesh" | "mesh+all");
            assert_eq!(
                main_rs.contains("esp_radio::wifi::new(peripherals.WIFI"),
                !matches!(mode, "ble" | "thread" | "mesh"),
                "the Wi-Fi radio exactly when a Wi-Fi link is on:\n{main_rs}"
            );
            assert_eq!(
                main_rs.contains("pins::configs::ble_mesh::start(_spawner, ble, None, flash);")
                    && main_rs.contains(
                        "let mut flash = crate::pins::configs::ble_mesh::Flash::share(esp_storage::FlashStorage::new(peripherals.FLASH));"
                    ),
                mesh,
                "Bluetooth Mesh, on the shared flash, exactly when it is on:\n{main_rs}"
            );
            assert_eq!(
                main_rs.contains("pins::configs::http::start(_spawner, net_stack);"),
                matches!(mode, "http" | "https" | "awshttps") || all,
                "the HTTP task exactly with HTTP:\n{main_rs}"
            );
            assert_eq!(
                main_rs.contains("pins::configs::thread::start(_spawner, peripherals.IEEE802154);"),
                mode == "thread",
                "Thread exactly when it is on:\n{main_rs}"
            );
            assert_eq!(
                main_rs.contains("BleConnector::new(peripherals.BT"),
                ble || mesh,
                "the Bluetooth controller exactly when it is on:\n{main_rs}"
            );
            // The user's loop calls everything the files offer, as a user
            // would, so the API is compiled and not just declared.
            let mut used = String::from("        // Your main loop code here.\n");
            // The user's code names `main`'s spawner as `_spawner`, whatever
            // the IoT tab turns on: it must still be there by that name.
            used.push_str("        let _ = _spawner;\n");
            if matches!(mode, "mqtt" | "mqtts" | "awsmqtt") || all {
                used.push_str("        pins::configs::mqtt::publish(\"rustonchip/hello\", b\"hi\").await.ok();\n");
                used.push_str("        let msg = pins::configs::mqtt::incoming().await;\n");
                used.push_str("        let _ = (msg.topic.as_str(), &msg.payload[..]);\n");
            }
            if all {
                used.push_str("        pins::configs::sntp::wait_synced().await;\n");
                used.push_str("        let _ = (pins::configs::sntp::now_unix(), pins::configs::sntp::now_unix_ms());\n");
            }
            if ble {
                used.push_str("        let packet = pins::configs::ble::receive().await;\n");
                used.push_str("        if pins::configs::ble::connected() {\n");
                used.push_str("            pins::configs::ble::send(&packet.data).await.ok();\n");
                used.push_str("        }\n");
            }
            if mesh {
                // Every call of the four models, as a light, a switch, a
                // sensor and a reader would make them.
                used.push_str("        pins::configs::ble_mesh::onoff_set(true).await.ok();\n");
                used.push_str("        pins::configs::ble_mesh::onoff_set_to(0x0003, false).await.ok();\n");
                used.push_str("        pins::configs::ble_mesh::onoff_get(None).await.ok();\n");
                used.push_str("        let st = pins::configs::ble_mesh::onoff_status().await;\n");
                used.push_str("        let set = pins::configs::ble_mesh::onoff_changed().await;\n");
                used.push_str("        pins::configs::ble_mesh::set_onoff(!set.on).await.ok();\n");
                used.push_str("        let _ = (st.src, st.on, set.src, pins::configs::ble_mesh::onoff());\n");
                used.push_str("        pins::configs::ble_mesh::sensor_set(0x004F, 21.5).await.ok();\n");
                used.push_str("        pins::configs::ble_mesh::sensor_publish().await.ok();\n");
                used.push_str("        pins::configs::ble_mesh::sensor_get(None, Some(0x004F)).await.ok();\n");
                used.push_str("        let r = pins::configs::ble_mesh::sensor_reading().await;\n");
                used.push_str("        let s = pins::configs::ble_mesh::stats();\n");
                used.push_str("        let _ = (r.src, r.property, r.value(), s.seq, s.rx_frames, pins::configs::ble_mesh::address());\n");
            }
            if mode == "thread" {
                used.push_str("        pins::configs::thread::wait_attached().await;\n");
                used.push_str("        let _ = (pins::configs::thread::role(), pins::configs::thread::addresses());\n");
                used.push_str("        let d = pins::configs::thread::receive().await;\n");
                used.push_str("        pins::configs::thread::send_to(*d.from.ip(), d.from.port(), &d.data).await.ok();\n");
            }
            if mode == "awsmqtt" {
                // The Device Shadow over MQTT: the delta loop, a get on a
                // resync, a report, an update and a delete of the named one,
                // and the JSON reader on what comes back.
                used.push_str("        let mut doc = [0u8; 1024];\n");
                used.push_str("        match pins::configs::mqtt::shadow_delta(&mut doc).await {\n");
                used.push_str("            Ok(d) if d.resync => {\n");
                used.push_str("                if let Ok(r) = pins::configs::mqtt::shadow_get(d.shadow, &mut doc).await {\n");
                used.push_str("                    let s = &doc[..r.len];\n");
                used.push_str("                    let _ = (r.code, r.truncated, pins::configs::mqtt::json_bool(s, &[\"state\", \"delta\", \"led\"]), pins::configs::mqtt::json_i64(s, &[\"version\"]));\n");
                used.push_str("                }\n");
                used.push_str("            }\n");
                used.push_str("            Ok(d) => {\n");
                used.push_str("                let mut name = [0u8; 16];\n");
                used.push_str("                let s = &doc[..d.len];\n");
                used.push_str("                let _ = (d.version, d.own, d.truncated, pins::configs::mqtt::json_f64(s, &[\"state\", \"t\"]));\n");
                used.push_str("                let _ = (pins::configs::mqtt::json_str(s, &[\"state\", \"name\"], &mut name).map(|n| n.len()), pins::configs::mqtt::json_get(s, &[\"state\"]).is_some());\n");
                used.push_str("                pins::configs::mqtt::shadow_report(d.shadow, br#\"{\"led\":true}\"#, &mut doc).await.ok();\n");
                used.push_str("            }\n");
                used.push_str("            Err(e) => {\n");
                used.push_str("                let _ = e;\n");
                used.push_str("            }\n");
                used.push_str("        }\n");
                used.push_str("        pins::configs::mqtt::shadow_update(Some(\"config\"), br#\"{\"state\":{\"desired\":null}}\"#, &mut doc).await.ok();\n");
                used.push_str("        pins::configs::mqtt::shadow_delete(Some(\"config\"), &mut doc).await.ok();\n");
            }
            if mode == "awshttps" {
                used.push_str("        let mut reply = [0u8; 128];\n");
                used.push_str("        if let Ok(r) = pins::configs::http::aws_publish(\"rustonchip-test/data\", b\"{}\", &mut reply).await {\n");
                used.push_str("            let _ = (r.status, r.len);\n");
                used.push_str("        }\n");
                // Every field of a message, so all of the encoding is linked.
                used.push_str("        let props = [(\"board\", \"esp32c3\")];\n");
                used.push_str("        let m = pins::configs::http::AwsMessage {\n");
                used.push_str("            qos: Some(0),\n            retain: true,\n            content_type: Some(\"application/json\"),\n");
                used.push_str("            message_expiry_s: Some(60),\n            response_topic: Some(\"rustonchip-test/reply\"),\n");
                used.push_str("            correlation_data: Some(b\"7\"),\n            user_properties: &props,\n            utf8: true,\n");
                used.push_str("            ..pins::configs::http::AwsMessage::new(\"rustonchip-test/state\", b\"{}\")\n        };\n");
                used.push_str("        pins::configs::http::aws_publish_with(&m, &mut reply).await.ok();\n");
                // The Device Shadow: each call, classic and named.
                used.push_str("        let mut doc = [0u8; 1024];\n");
                used.push_str("        if let Ok(r) = pins::configs::http::shadow_get(None, &mut doc).await {\n");
                used.push_str("            let _ = pins::configs::http::json_bool(&doc[..r.len], &[\"state\", \"delta\", \"led\"]);\n");
                used.push_str("        }\n");
                used.push_str("        pins::configs::http::shadow_report(Some(\"config\"), br#\"{\"led\":true}\"#, &mut doc).await.ok();\n");
                used.push_str("        pins::configs::http::shadow_update(None, br#\"{\"state\":{\"desired\":null},\"version\":3}\"#, &mut doc).await.ok();\n");
                used.push_str("        pins::configs::http::shadow_delete(Some(\"config\"), &mut doc).await.ok();\n");
            }
            if matches!(mode, "http" | "https" | "awshttps") || all {
                used.push_str("        let mut body = [0u8; 256];\n");
                used.push_str("        if let Ok(r) = pins::configs::http::get(\"http://example.com/\", &mut body).await {\n");
                used.push_str("            let _ = (r.status, r.len, r.truncated, r.location());\n");
                // The JSON reader, on the answer.
                used.push_str("            let doc = &body[..r.len];\n");
                used.push_str("            let mut text = [0u8; 32];\n");
                used.push_str("            let _ = (\n");
                used.push_str("                pins::configs::http::json_get(doc, &[\"a\", \"0\"]),\n");
                used.push_str("                pins::configs::http::json_f64(doc, &[\"t\"]),\n");
                used.push_str("                pins::configs::http::json_i64(doc, &[\"version\"]),\n");
                used.push_str("                pins::configs::http::json_bool(doc, &[\"on\"]),\n");
                used.push_str("                pins::configs::http::json_str(doc, &[\"name\"], &mut text).map(|s| s.len()),\n");
                used.push_str("            );\n");
                used.push_str("        }\n");
                used.push_str("        pins::configs::http::post(\"https://example.com/in\", \"application/json\", b\"{}\", &mut body).await.ok();\n");
                used.push_str("        let req = pins::configs::http::Request {\n");
                used.push_str("            auth: pins::configs::http::Auth::Bearer(\"token\"),\n");
                used.push_str("            headers: &[(\"X-Board\", \"esp\")],\n");
                used.push_str("            ..pins::configs::http::Request::get(\"http://192.168.1.10:8123/api/\")\n");
                used.push_str("        };\n");
                used.push_str("        pins::configs::http::request(req, &mut body).await.ok();\n");
            }
            if matches!(mode, "espnow" | "ble+espnow") || all {
                used.push_str("        let me = pins::configs::espnow::own_mac();\n");
                used.push_str("        pins::configs::espnow::send(pins::configs::espnow::BROADCAST, &me).await.ok();\n");
                used.push_str("        let frame = pins::configs::espnow::receive().await;\n");
                used.push_str("        let _ = (frame.from, &frame.data[..]);\n");
            }
            main_rs = main_rs.replacen("        // Your main loop code here.\n", &used, 1);
        }
        if store.is_some() {
            // Bluetooth Mesh shares the store's flash: the same driver, wrapped.
            assert!(
                main_rs
                    .contains("let mut flash = esp_storage::FlashStorage::new(peripherals.FLASH);")
                    || main_rs.contains(
                        "let mut flash = crate::pins::configs::ble_mesh::Flash::share(esp_storage::FlashStorage::new(peripherals.FLASH));"
                    ),
                "no flash in main.rs:\n{main_rs}"
            );
            assert!(
                main_rs.contains("let mut flash_store = flash_store::ConfigStore::new(flash);"),
                "the store line was not seeded:\n{main_rs}"
            );
            // Use everything the template offers, as a user would: the check
            // before the store takes the flash, then a load and a save in the
            // loop - async or blocking, as the runtime is.
            let seed = crate::panels::mcu_module::codegen::flash_store_gen::TAIL_SEED;
            main_rs = main_rs.replacen(
                seed,
                &format!("    flash_store::verify(&mut flash).ok();\n{seed}"),
                1,
            );
            let body = if mcu.runtime == Runtime::Async {
                "let mut d = flash_store.load().await;\n        d.counter += 1;\n        flash_store.save(&d).await.ok();\n"
            } else {
                "let mut d = flash_store.load_blocking();\n        d.counter += 1;\n        flash_store.save_blocking(&d).ok();\n"
            };
            main_rs = main_rs.replacen(
                "        // Your main loop code here.\n",
                &format!("        // Your main loop code here.\n        {body}"),
                1,
            );
        }
        if wdg {
            assert!(
                main_rs.contains("pins::configs::rwdt::init(peripherals.LPWR)"),
                "no RWDT in main.rs:\n{main_rs}"
            );
            let timg1 = crate::panels::mcu_module::watchdog::esp_limits_for(&chip).has_mwdt1;
            assert_eq!(
                main_rs.contains("pins::configs::mwdt1::init()"),
                timg1,
                "[{chip}] MWDT1 in main.rs must follow TIMG1:\n{main_rs}"
            );
        }
        // Only where the chip HAS a LEDC: esp32c5 and esp32c61 carry none,
        // so their definitions offer no PWM function to wire in at all.
        if mcu
            .iter_all_pins()
            .any(|p| matches!(p.selected_function, PinFunction::TimerPwm { .. }))
        {
            assert!(
                main_rs.contains("pins::configs::pwm0::init(&ledc"),
                "no PWM in main.rs:\n{main_rs}"
            );
        }

        // Async runtime, flash store, partition table, the IoT tab's crates
        // and `alloc` in build-std: the app's decision, in the app's order.
        let files = manifest_sync::project_files(&esp, &mcu, &main_rs);
        let iot_active = crate::panels::mcu_module::iot::active(&mcu);
        let tls = iot_active.is_some_and(|a| a.tls());
        if matches!(iot_mode.as_deref(), Some("mqtts")) {
            assert!(iot_active.is_some_and(|a| a.mqtt_tls), "{chip}: mqtts is MQTT over TLS");
        }
        if matches!(iot_mode.as_deref(), Some("https")) {
            assert!(
                iot_active.is_some_and(|a| a.http_tls && !a.mqtt),
                "{chip}: https is HTTPS alone"
            );
        }
        if matches!(iot_mode.as_deref(), Some("awshttps")) {
            assert!(
                iot_active.is_some_and(|a| a.http_client_cert && !a.mqtt),
                "{chip}: awshttps is HTTPS with the board's certificate, MQTT off"
            );
        }
        if matches!(iot_mode.as_deref(), Some("awsmqtt")) {
            assert!(
                iot_active.is_some_and(|a| a.mqtt_client_cert && !a.http),
                "{chip}: awsmqtt is MQTT on AWS IoT with the board's certificate, HTTP off"
            );
            let m = mcu.iot.mqtt.as_ref().expect("awsmqtt has MQTT");
            assert!(
                crate::panels::mcu_module::iot::mqtt_shadow(m).is_some(),
                "{chip}: awsmqtt runs the Device Shadow"
            );
        }
        if tls {
            assert!(
                files.cargo_toml.contains("embedded-tls = "),
                "{}",
                files.cargo_toml
            );
            assert!(
                files.cargo_toml.contains("embedded-io-async = "),
                "{}",
                files.cargo_toml
            );
            assert!(
                !files.cargo_toml.contains("embedded-alloc"),
                "the ESP heap is esp-alloc's"
            );
        }
        if iot_mode.as_deref() == Some("thread") {
            for want in [
                "openthread = { version = \"0.2\", features = [\"esp-radio\"] }",
                "mbedtls-rs-sys = \"0.1\"",
                "\"ieee802154\"",
                "tinyrlibc = ",
            ] {
                assert!(
                    files.cargo_toml.contains(want),
                    "{want}:\n{}",
                    files.cargo_toml
                );
            }
            for absent in ["executor-interrupt", "\"wifi\"", "\"ble\""] {
                assert!(
                    !files.cargo_toml.contains(absent),
                    "{absent}:\n{}",
                    files.cargo_toml
                );
            }
        }
        // The legacy single address, never edited into a list - what every
        // project from before device lists holds. It is device 1 all the same,
        // and gets `device1.rs`. Set straight on the config: any edit through
        // the panel would turn it into a list first.
        for m in &mut mcu.modules {
            if let crate::panels::mcu_module::modules::ModuleConfig::I2c(c) = &mut m.config {
                c.address = 0x3C;
            }
        }
        let mut user: Vec<(String, String)> = mcu.pin_tree_files();
        // The client certificate: the dummy identity, so the signing code is
        // built and linked, not folded away as `IDENTITY = None`.
        if iot_active.is_some_and(|a| a.client_cert()) {
            use crate::panels::mcu_module::codegen::iot_gen;
            iot_gen::seed_test_identity(&mut user);
            let tls = user
                .iter()
                .find(|(p, _)| p == "src/pins/configs/tls.rs")
                .map(|(_, c)| c.as_str())
                .unwrap_or("");
            assert!(
                tls.contains("client_identity(&CLIENT_CERT_DER, &CLIENT_KEY_DER)"),
                "{tls}"
            );
        }
        // Thread with a real dataset: an empty one makes `commissioned()` a
        // constant false, and LTO would drop the whole attach / IPv6 / UDP
        // path - the code a user with a dataset builds - from the link.
        if iot_mode.as_deref() == Some("thread") {
            use crate::panels::mcu_module::codegen::iot_gen;
            const DATASET: &str = "000300001901020fd80208b566147d38e384200e080000639c5d67a3bd0510c490f58d4be0d5eaeb0f09b395d1ae17030d4e4553542d50414e2d304644380708fd7d4f8232cb00000410a7e08419ae47c177fb91bcfcec789aa50c0402a0f77835060004001fffe0";
            let secrets = user
                .iter_mut()
                .find(|(p, _)| p == iot_gen::SECRETS_PATH)
                .expect("secrets.rs with Thread");
            secrets.1 = iot_gen::write_secret(&secrets.1, iot_gen::THREAD_DATASET, DATASET);
        }
        // Bluetooth Mesh with real keys: empty ones fail the build on purpose.
        if iot_active.is_some_and(|a| a.ble_mesh) {
            crate::panels::mcu_module::codegen::iot_gen::seed_test_mesh_keys(&mut user, 2);
        }
        let dir = std::env::temp_dir().join(format!("eide_esp_check_{chip}"));
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write esp project");
        println!("wrote {}", dir.display());
        println!("target: {}", esp.project.target);
    }

    /// A whole STM32 project with the Configuration tab's flash store on - the
    /// only place its template, the GEN lines, the seeded tail line and the
    /// shrunk memory.x meet a real HAL, sequential-storage and a LINKER (a
    /// memory.x mistake passes `cargo check`; the matrix rows build).
    ///
    /// `EIDE_STORE_CHIP` picks the part: `f103` (the built-in, default),
    /// `g431`, `wba55`, or `f411` / `f746` (the store after the vector table) -
    /// derived from the F103's definition the way
    /// the importer would build them, as the other embassy harnesses do.
    /// `EIDE_STORE_RUNTIME` = `blocking` (default) | `native` | `async`. No
    /// pins are wired: the store's lines live in the Configuration tab's slot,
    /// which the "select pins" default section must carry too.
    #[test]
    #[ignore]
    fn emit_stm32_store_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::codegen::flash_store_gen::TAIL_SEED;
        use crate::panels::mcu_module::flash_store::{self, FlashStoreConfig, Platform};
        use crate::panels::mcu_module::mcu::model::Runtime;
        use crate::panels::mcu_module::mcu_def::{ClockDef, build_cfg};

        let chip = std::env::var("EIDE_STORE_CHIP").unwrap_or_else(|_| "f103".into());
        let mut def = builtin_for("stm32f103c8t6").expect("built-in F103");
        // (id, display name, family, target, flash, RAM)
        let derived = match chip.as_str() {
            "f103" => None,
            "g431" => Some((
                "stm32g431cb",
                "STM32G431CBUx",
                "stm32g4",
                "thumbv7em-none-eabihf",
                "128K",
                "32K",
            )),
            "wba55" => Some((
                "stm32wba55cg",
                "STM32WBA55CGUx",
                "stm32wba",
                "thumbv8m.main-none-eabihf",
                "1024K",
                "128K",
            )),
            // 128 KiB sectors at the end: the store goes right after the
            // vector table (16 KiB sectors on the F411, 32 KiB on the F746).
            "f411" => Some((
                "stm32f411re",
                "STM32F411RETx",
                "stm32f4",
                "thumbv7em-none-eabihf",
                "512K",
                "128K",
            )),
            "f746" => Some((
                "stm32f746zg",
                "STM32F746ZGTx",
                "stm32f7",
                "thumbv7em-none-eabihf",
                "1024K",
                "320K",
            )),
            other => panic!("EIDE_STORE_CHIP={other}: f103 | g431 | wba55 | f411 | f746"),
        };
        if let Some((id, name, family, target, flash, ram)) = derived {
            def.id = id.into();
            def.display_name = name.into();
            def.family = family.into();
            def.project.pkg_name = id.into();
            def.project.target = target.into();
            def.project.probe_chip = name.into();
            def.project.flash_size = flash.into();
            def.project.ram_size = ram.into();
            def.project.hal_dep = stm32_pin_data::hal_dep_for_name(family, name);
            def.clock = ClockDef::None;
        }
        let mut mcu = def.build_mcu();
        let runtime = match std::env::var("EIDE_STORE_RUNTIME").as_deref() {
            Ok("native") => Runtime::Native,
            Ok("async") => Runtime::Async,
            _ => Runtime::Blocking,
        };
        mcu.runtime = runtime;
        mcu.pending_runtime = runtime;
        let platform = flash_store::platform_of(&mcu)
            .unwrap_or_else(|why| panic!("[{chip} {runtime:?}] not generated: {why}"));
        // The card's own default for the part: the end of flash, or the
        // sectors after the vector table.
        mcu.flash_store = Some(FlashStoreConfig::default_on(&platform, &mcu.family));
        println!("[{chip} {runtime:?}] {platform:?}");
        assert!(matches!(platform, Platform::Stm32 { .. }));

        let mut main_rs = mcu.fresh_main_rs();
        assert!(
            main_rs.contains(TAIL_SEED),
            "the store line was not seeded:\n{main_rs}"
        );
        // Use what the template offers, as a user would: a load and a save in
        // the loop - async or blocking, as the runtime is.
        let body = if mcu.is_async() {
            "let mut d = flash_store.load().await;\n        d.counter += 1;\n        flash_store.save(&d).await.ok();\n"
        } else {
            "let mut d = flash_store.load_blocking();\n        d.counter += 1;\n        flash_store.save_blocking(&d).ok();\n"
        };
        main_rs = main_rs.replacen(
            "        // Your main loop code here.\n",
            &format!("        // Your main loop code here.\n        {body}"),
            1,
        );
        assert!(
            main_rs.contains("flash_store.save"),
            "no loop to use it in:\n{main_rs}"
        );

        let project = build_cfg(&def, Some(&mcu));
        // memory.x included: the app's decision (`flash_store::memory_x_reservation`).
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        assert!(flash_store::memory_x_reservation(&mcu).is_some(), "nothing reserved");
        assert!(
            files.memory_x.contains("_flash_store_start"),
            "{}",
            files.memory_x
        );
        let user = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join(format!(
            "eide_store_check_{chip}_{}",
            format!("{runtime:?}").to_ascii_lowercase()
        ));
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write store project");
        println!("wrote {}", dir.display());
        println!("target: {}", project.target);
    }

    /// A whole STM32N6 project, from the vendor file to `main.rs`.
    ///
    /// The clock block was type-checked on its own; this is the rest of the
    /// generation around it — manifest, target, entry point — for a family that
    /// until now only ever produced a commented skeleton.
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_n6_project -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs the STM32Cube database, writes a project for a manual cross-compile"]
    fn emit_n6_project() {
        warn_if_matrix_running();
        let Some(path) = vendor_chip_file("EIDE_CHIP_XML", "STM32N645A0HxQ.xml") else {
            println!("no STM32N6 vendor file - skipped");
            return;
        };
        let path = path.as_path();
        let xml = std::fs::read_to_string(path).expect("read the vendor file");
        let mut def = stm32_pin_data::convert_xml(&xml)
            .expect("convert")
            .remove(0)
            .form
            .to_definition();

        // Without the vendor's tree there is no RCC block to generate, and this
        // harness would silently prove nothing.
        let db = crate::panels::mcu_module::chip_sources::all_sources()
            .into_iter()
            .find_map(|s| s.db)
            .expect("a source with clock trees");
        let (gc, _) = crate::panels::mcu_module::clock::graph::cubemx::graph_for_chip_xml(
            &db, &xml, "stm32n6",
        )
        .expect("the N6 clock tree");
        def.clock = crate::panels::mcu_module::mcu_def::ClockDef::Graph(gc);

        let mcu = def.build_mcu();
        let main_rs = mcu.fresh_main_rs();
        assert!(
            main_rs.contains("config.rcc.cpu = CpuClk::"),
            "the N6 clock block is missing from main.rs:
{main_rs}"
        );
        assert!(
            !main_rs.contains("has no generated RCC recipe"),
            "still the commented skeleton:
{main_rs}"
        );

        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        // `main.rs` declares `pub mod pins;`, and the app writes that module
        // when it saves. A harness that skips it compiles a project the IDE
        // never produces - and fails on the one file it forgot.
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join("eide_n6_check");
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write n6 project");
        println!("wrote {} ({})", dir.display(), def.display_name);
        println!(
            "target: {}  hal: {}",
            def.project.target, def.project.hal_dep
        );
    }

    /// A REAL chip imported from the vendor database, with USART/SPI/I2C all on
    /// async DMA — the point of the mux rule.
    ///
    /// `EIDE_RUNTIME=blocking` builds the same wiring on the Blocking backend
    /// instead: embassy's polled / blocking drivers (`new_blocking*`), with the
    /// SPI/I2C `async_mode` and the USART transport ignored, `EIDE_USART_DIR`
    /// picking the one-way or half-duplex `new_blocking` form, and SDMMC/SAI
    /// left to their written "NOT initialised" refusal. There
    /// `EIDE_EXPECT_CONFIGS` fails a row whose drivers all fell away, and
    /// `EIDE_BLK_UPGRADE=used` opens a project from before the Blocking drivers,
    /// whose hand-wired USART must come back with "Generate driver" off.
    /// `EIDE_LPUART_DMA=1` puts the LPUART on DMA too, and `EIDE_EXPECT_MAIN`
    /// names a line main.rs must carry - the refusal a row exists to prove.
    ///
    /// STM32G4 has no hand-written request table and never will need one: its
    /// controller is muxed, so any free channel serves any peripheral. Whether
    /// the channel names, the interrupt names and the `bind_interrupts!`
    /// grouping this produces are the ones embassy accepts is a question only a
    /// compiler can answer:
    ///
    /// ```text
    /// cargo test --bin rust_on_chip emit_imported_dma_project -- --ignored --nocapture
    /// cd %TEMP%\eide_dma_check && cargo check --target thumbv7em-none-eabihf
    /// ```
    #[test]
    #[ignore = "needs the STM32Cube database, writes a project for a manual cross-compile"]
    fn emit_imported_dma_project() {
        warn_if_matrix_running();
        use crate::panels::mcu_module::codegen::dma_data;
        use crate::panels::mcu_module::modules::{AsyncBusMode, ModuleConfig, UsartMode};

        // `EIDE_CHIP_XML` names another part's file: a FULL path. Each one then
        // gets its own directory, so four chips' builds do not pile into one
        // `target/`; the default part keeps the one it always had.
        let chosen = std::env::var("EIDE_CHIP_XML").ok();
        let Some(path) = vendor_chip_file("EIDE_CHIP_XML", "STM32G431C(6-8-B)Tx.xml") else {
            match &chosen {
                Some(p) => eprintln!("EIDE_CHIP_XML={p} is not a file - nothing emitted"),
                None => eprintln!(
                    "STM32G431C(6-8-B)Tx.xml is in none of this machine's chip sources - nothing emitted"
                ),
            }
            return;
        };
        let path = path.as_path();
        let Ok(xml) = std::fs::read_to_string(path) else {
            eprintln!("could not read {} - nothing emitted", path.display());
            return;
        };
        let af = stm32_pin_data::gpio_ip_version(&xml).and_then(|v| {
            let f = path
                .parent()?
                .join("IP")
                .join(stm32_pin_data::gpio_ip_file_name(&v));
            Some(stm32_pin_data::GpioAf::parse(
                &std::fs::read_to_string(f).ok()?,
            ))
        });
        let mut cache = std::collections::HashMap::new();
        let dma = dma_data::dma_def_for(&xml, path.parent(), &mut cache);
        let mut icache = std::collections::HashMap::new();
        let irqs =
            crate::panels::mcu_module::codegen::nvic::vectors_for(&xml, path.parent(), &mut icache);
        println!(
            "i2c1 vectors: {:?}",
            crate::panels::mcu_module::codegen::nvic::i2c_irqs(&irqs, 1)
        );
        println!(
            "dma: mux={:?} channels={}",
            dma.as_ref().map(|d| d.mux),
            dma.as_ref().map_or(0, |d| d.channels.len())
        );

        let mut def = stm32_pin_data::convert_xml_with_af(&xml, af.as_ref())
            .expect("converts")
            .remove(0)
            .form
            .to_definition();
        def.dma = dma;
        def.irq_vectors = irqs;
        def.usart_ip = stm32_pin_data::usart_ip_version(&xml);
        def.sdmmc_ip = stm32_pin_data::sdmmc_ip_version(&xml);
        // A part embassy publishes twice (the STM32L1 `A` / `X` dies) compiles
        // on either feature, so only the line itself shows which one it got.
        if let Ok(want) = std::env::var("EIDE_EXPECT_FEATURE") {
            assert_eq!(
                stm32_pin_data::embassy_feature_in(&def.project.hal_dep),
                Some(want.as_str()),
                "{}: {}",
                def.display_name,
                def.project.hal_dep
            );
        }
        let mut mcu = def.build_mcu();
        // `EIDE_RUNTIME=blocking` builds the same chip on the Blocking backend
        // (generic STM32 or WBA) - with `EIDE_EXTI`, the `#[interrupt]` handlers
        // on the vectors this part really has, which is the half no bundled
        // definition can test: none of them carries a vector list.
        mcu.runtime = match std::env::var("EIDE_RUNTIME").as_deref() {
            Ok("blocking") => crate::panels::mcu_module::mcu::model::Runtime::Blocking,
            _ => crate::panels::mcu_module::mcu::model::Runtime::Async,
        };

        // Take the first pin the chip itself offers for each bus signal, so this
        // works for whatever part `EIDE_CHIP_XML` names.
        // `EIDE_USART_N=3` on an STM32G0 exercises the SHARED vector
        // (`USART3_4_LPUART1`), which is a different emission path from USART1's
        // dedicated one.
        let un: u8 = std::env::var("EIDE_USART_N")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1);
        // `EIDE_EXTI=rising|falling|both` wires TWO GPIO inputs on the SAME line
        // number (PA<n> and PB<n>) and arms both. One gets the EXTI channel; the
        // other has to be refused, because the channel is a peripheral and
        // embassy hands it to exactly one pad. Both halves in one run.
        if let Ok(e) = std::env::var("EIDE_EXTI") {
            use crate::panels::mcu_module::pins::logic::pin::Edge;
            let edge = match e.as_str() {
                "falling" => Edge::Falling,
                "both" => Edge::Both,
                _ => Edge::Rising,
            };
            // A line number carried by two ports on this package, so the clash is
            // real rather than staged.
            let free: Vec<(usize, String)> = mcu
                .iter_all_pins()
                .filter(|p| !p.reserved && p.selected_function == PinFunction::Unset)
                .map(|p| (p.number, p.gpio().to_owned()))
                .collect();
            let mut pair: Vec<usize> = Vec::new();
            for (num, name) in &free {
                let tail = name.get(2..).unwrap_or_default();
                if free
                    .iter()
                    .any(|(n2, g2)| n2 != num && g2.get(2..) == Some(tail))
                {
                    pair.push(*num);
                    let same = free
                        .iter()
                        .find(|(n2, g2)| n2 != num && g2.get(2..) == Some(tail))
                        .map(|(n2, _)| *n2);
                    if let Some(n2) = same {
                        pair.push(n2);
                    }
                    break;
                }
            }
            if pair.is_empty() {
                println!("no two free pads share a line number - EXTI clash not exercised");
            }
            for num in pair {
                if let Some(p) = mcu.find_pin_mut(num) {
                    p.selected_function = PinFunction::GpioInput;
                    p.irq = Some(edge);
                }
            }
        }
        // `EIDE_EXTI_SHARED=1` arms two more free pads on DIFFERENT lines of
        // ONE vector (`EXTI9_5`, `EXTI4_15`...), so the handler that asks each
        // of its pins is compiled too - the pair above shares a LINE, so only
        // one of it ever gets a handler. A part with a vector per line (WBA)
        // has no such pair, and says so.
        if std::env::var_os("EIDE_EXTI_SHARED").is_some() {
            use crate::panels::mcu_module::codegen::embassy_stm32::{exti_line, exti_vector};
            use crate::panels::mcu_module::pins::logic::pin::Edge;
            let taken: Vec<u8> = mcu
                .iter_all_pins()
                .filter(|p| p.irq.is_some())
                .filter_map(|p| exti_line(p.gpio()))
                .collect();
            let free: Vec<(usize, u8, String)> = mcu
                .iter_all_pins()
                .filter(|p| !p.reserved && p.selected_function == PinFunction::Unset)
                .filter_map(|p| {
                    let line = exti_line(p.gpio())?;
                    let vector = exti_vector(&mcu.irq_vectors, line)?.to_owned();
                    (!taken.contains(&line)).then_some((p.number, line, vector))
                })
                .collect();
            let shared = free.iter().find_map(|(n1, l1, v1)| {
                free.iter()
                    .find(|(_, l2, v2)| l2 != l1 && v2 == v1)
                    .map(|(n2, _, _)| (*n1, *n2, v1.clone()))
            });
            match shared {
                Some((a, b, vector)) => {
                    println!("shared vector exercised: {vector}");
                    for (num, edge) in [(a, Edge::Rising), (b, Edge::Both)] {
                        if let Some(p) = mcu.find_pin_mut(num) {
                            p.selected_function = PinFunction::GpioInput;
                            p.irq = Some(edge);
                        }
                    }
                }
                None => println!("no vector serves two free lines - shared handler not exercised"),
            }
        }
        // `EIDE_HSPI=lanes` wires an HSPI device on controller 1 with that many
        // data lines (2 or 8 — embassy has nothing between); `EIDE_HSPI_DQS=1`
        // adds the strobe, which the octal call REQUIRES.
        if let Ok(l) = std::env::var("EIDE_HSPI") {
            let lanes: u8 = l.parse().unwrap_or(8);
            let unit = 1u8;
            let mut want = vec![PinFunction::HspiClk { unit }, PinFunction::HspiNcs { unit }];
            want.extend((0..lanes).map(|lane| PinFunction::HspiIo { unit, lane }));
            if std::env::var("EIDE_HSPI_DQS").is_ok() {
                want.push(PinFunction::HspiDqs { unit, index: 0 });
            }
            for want in want {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        // `EIDE_XSPI=lanes` wires an XSPI device on port 1 with that many data
        // lines (2, 4, 8 or 16); `EIDE_XSPI_DQS=1|2` adds one or both strobes,
        // which only the wide modes read.
        if let Ok(l) = std::env::var("EIDE_XSPI") {
            let lanes: u8 = l.parse().unwrap_or(8);
            let port = 1u8;
            let mut want = vec![
                PinFunction::XspiClk { port },
                PinFunction::XspiNcs { port, cs: 1 },
            ];
            want.extend((0..lanes).map(|lane| PinFunction::XspiIo { port, lane }));
            let dqs: u8 = std::env::var("EIDE_XSPI_DQS")
                .ok()
                .and_then(|d| d.parse().ok())
                .unwrap_or(0);
            want.extend((0..dqs).map(|index| PinFunction::XspiDqs { port, index }));
            for want in want {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        // `EIDE_OSPI=lanes` wires an OCTOSPI device on port 1 with that many
        // data lines (2, 4 or 8); `EIDE_OSPI_DQS=1` adds the strobe, which only
        // the octal mode reads.
        if let Ok(l) = std::env::var("EIDE_OSPI") {
            let lanes: u8 = l.parse().unwrap_or(4);
            let port = 1u8;
            let mut want = vec![PinFunction::OspiClk { port }, PinFunction::OspiNcs { port }];
            want.extend((0..lanes).map(|lane| PinFunction::OspiIo { port, lane }));
            if std::env::var("EIDE_OSPI_DQS").is_ok() {
                want.push(PinFunction::OspiDqs { port });
            }
            for want in want {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        // `EIDE_QSPI=1|2|dual` wires an external flash on that bank (or both,
        // which is the 8-line dual-flash shape). Assigned first: the QUADSPI
        // pads are the least interchangeable ones on any of these packages.
        if let Ok(which) = std::env::var("EIDE_QSPI") {
            let banks: Vec<u8> = match which.as_str() {
                "2" => vec![2],
                "dual" => vec![1, 2],
                _ => vec![1],
            };
            let mut want = vec![PinFunction::QspiClk];
            for b in banks {
                want.push(PinFunction::QspiNcs { bank: b });
                want.extend((0..4).map(|lane| PinFunction::QspiIo { bank: b, lane }));
            }
            for want in want {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        // `EIDE_SDMMC=w` wires an SD card at bus width `w` (1, 4 or 8) — on a
        // chip that has the controller, which the G431 does not. Assigned first
        // because the card pads are the least interchangeable ones here.
        if let Ok(w) = std::env::var("EIDE_SDMMC") {
            let lanes: u8 = w.parse().unwrap_or(4);
            let unit = std::env::var("EIDE_SDMMC_UNIT")
                .ok()
                .and_then(|u| u.parse().ok())
                .unwrap_or(1u8);
            let mut want: Vec<PinFunction> = vec![
                PinFunction::SdmmcCk { unit },
                PinFunction::SdmmcCmd { unit },
            ];
            want.extend((0..lanes).map(|lane| PinFunction::SdmmcD { unit, lane }));
            for want in want {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        // `EIDE_SAI=1` gives the SAI pads FIRST pick. On this 48-pin part they
        // collide with the pads the timer, SPI and USART want — SAI1_SCK_A is
        // PA8, SCK_B is PB3 — and a sub-block needs three of them, so both
        // layouts cannot fit at once. One fixture, two runs.
        if std::env::var("EIDE_SAI").is_ok() {
            for want in [
                PinFunction::SaiSck { sai: 1, block: 1 },
                PinFunction::SaiSd { sai: 1, block: 1 },
                PinFunction::SaiFs { sai: 1, block: 1 },
                PinFunction::SaiMclk { sai: 1, block: 1 },
                PinFunction::SaiSck { sai: 1, block: 2 },
                PinFunction::SaiSd { sai: 1, block: 2 },
                PinFunction::SaiFs { sai: 1, block: 2 },
            ] {
                let num = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&want)
                    })
                    .map(|p| p.number);
                match num.and_then(|n| mcu.find_pin_mut(n)) {
                    Some(p) => p.selected_function = want,
                    None => println!("chip has no pin for {want:?}"),
                }
            }
        }
        for want in [
            PinFunction::UsartTx(un),
            PinFunction::UsartRx(un),
            // The LPUART, left BUFFERED: a peripheral of its own driven through
            // embassy's `usart` API, and on a G0 it shares one NVIC vector with
            // USART3/4 — so this is also where a duplicate `bind_interrupts!`
            // for that vector would show up.
            PinFunction::LpuartTx(1),
            PinFunction::LpuartRx(1),
            // Flow control on the LPUART: the pads join the module by being
            // assigned, and only a compile proves the constructor's argument
            // order (embassy moves the interrupt binding around between the
            // `new` and `new_with_*` forms).
            PinFunction::LpuartCts(1),
            PinFunction::LpuartRts(1),
            // The BUSES come before PWM: a timer channel has many candidate
            // pads, SPI1's MISO has two, and first-come-first-served on a
            // 64-pin part left the SPI without one - which then silently
            // exercised the TX-only path instead of the full-duplex one.
            PinFunction::SpiSck(1),
            PinFunction::SpiMosi(1),
            // `EIDE_SPI_TXONLY=1` leaves MISO unwired, which is a different
            // constructor and a different return type - `Spi::new_txonly` and
            // the concrete `Spi<'d, Async, Master>` rather than `impl SpiBus`.
            PinFunction::SpiMiso(1),
            PinFunction::I2cScl(1),
            PinFunction::I2cSda(1),
            // Two channels of ONE timer: the PWM module's whole premise is that
            // they share a frequency, and only a compiler can confirm the
            // `SimplePwm::new` slot order and the per-channel handles.
            // TIM2 rather than TIM3: TIM3.s channels compete with SPI1.s MISO
            // on this package, and a PWM module with one channel would stop
            // testing the thing it is here to test.
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
            PinFunction::TimerPwm {
                timer: 2,
                channel: 2,
            },
            // A SECOND timer, this one advanced and wired as a PAIR: TIM1 CH1
            // with its complementary CH1N. That is the whole `ComplementaryPwm`
            // path — a different driver, eight slots instead of four, and a
            // dead time — so the project carries both shapes at once and only a
            // compiler can confirm either.
            PinFunction::TimerPwm {
                timer: 1,
                channel: 1,
            },
            PinFunction::TimerPwmN {
                timer: 1,
                channel: 1,
            },
            // …and the fault line that switches the pair off in hardware. It
            // is the only pad whose alternate function the generated `init`
            // has to set by hand, and the only one it hands back.
            PinFunction::TimerBreak { timer: 1, input: 1 },
            // A whole SPI block running as audio instead: the I2S path is a
            // different driver, a constructor per direction, and the only place
            // a `StaticCell` ring buffer meets a DMA binding.
            PinFunction::I2sCk(2),
            PinFunction::I2sWs(2),
            PinFunction::I2sSd(2),
            PinFunction::I2sMck(2),
            // Both DAC channels: that is the `Dac` + `DualValue` shape, the
            // richer of the two the module can emit.
            PinFunction::DacOut { dac: 1, channel: 1 },
            PinFunction::DacOut { dac: 1, channel: 2 },
        ] {
            if want == PinFunction::SpiMiso(1) && std::env::var("EIDE_SPI_TXONLY").is_ok() {
                continue;
            }
            // `EIDE_IMPORT_SKIP=I2s,Dac` leaves those functions out. A chip whose
            // DMA requests sit on FIXED channels cannot run every bus at once:
            // on the STM32L151 USART1 and SPI2 both need DMA1 CH4/CH5, and the
            // IDE rightly writes a TODO for the second - which no compiler takes.
            let skip = std::env::var("EIDE_IMPORT_SKIP").unwrap_or_default();
            let kind = format!("{want:?}");
            if skip
                .split(',')
                .map(str::trim)
                .any(|k| !k.is_empty() && kind.starts_with(k))
            {
                println!("left out by EIDE_IMPORT_SKIP: {kind}");
                continue;
            }
            let num = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset
                        && p.available_functions.contains(&want)
                })
                .map(|p| p.number);
            match num.and_then(|n| mcu.find_pin_mut(n)) {
                Some(p) => p.selected_function = want,
                None => println!("chip has no pin for {want:?}"),
            }
        }
        mcu.reconcile_modules();
        for m in &mut mcu.modules {
            match &mut m.config {
                ModuleConfig::Spi(c) => c.async_mode = AsyncBusMode::AsyncDma,
                ModuleConfig::I2c(c) => c.async_mode = AsyncBusMode::AsyncDma,
                // `EIDE_USART_DIR=tx` / `=rx` builds the ONE-WAY DMA UART
                // instead of the full-duplex pair — a different template and a
                // different return type, so it needs its own compile.
                ModuleConfig::Usart(c) => {
                    c.mode = UsartMode::Dma;
                    // The line-level extras, on a chip that has the bits — the
                    // assignments only compile where embassy declares the fields.
                    c.swap_rx_tx = true;
                    c.invert_rx = true;
                    c.direction = match std::env::var("EIDE_USART_DIR").as_deref() {
                        Ok("tx") => crate::panels::mcu_module::modules::UsartDirection::TxOnly,
                        Ok("rx") => crate::panels::mcu_module::modules::UsartDirection::RxOnly,
                        // `half` / `halfrx`: ONE pad, both directions — a whole
                        // `Uart` from a single pin, and the readback argument.
                        Ok("half") => {
                            c.half_duplex_readback = true;
                            crate::panels::mcu_module::modules::UsartDirection::HalfDuplexOnTx
                        }
                        Ok("halfrx") => {
                            crate::panels::mcu_module::modules::UsartDirection::HalfDuplexOnRx
                        }
                        _ => crate::panels::mcu_module::modules::UsartDirection::TxRx,
                    };
                }
                // The LPUART stays buffered, with hardware flow control — the
                // combination whose constructor takes the pads AND reorders the
                // interrupt binding.
                ModuleConfig::Lpuart(c) => {
                    // `EIDE_LPUART_HALF=1` swaps the flow-control case for the
                    // BUFFERED half-duplex one — the other constructor whose
                    // argument order embassy shuffles.
                    if std::env::var("EIDE_LPUART_HALF").is_ok() {
                        c.direction =
                            crate::panels::mcu_module::modules::UsartDirection::HalfDuplexOnTx;
                    } else {
                        c.flow = crate::panels::mcu_module::modules::UsartFlow::CtsRts;
                    }
                    // `EIDE_LPUART_DMA=1` puts it on DMA instead - the request an
                    // STM32L4 spells without its number (`LPUART_TX`).
                    if std::env::var("EIDE_LPUART_DMA").is_ok() {
                        c.mode = UsartMode::Dma;
                    }
                }
                // A non-default frequency, duty and output shape, so the
                // generated consts and the `low_level` calls are exercised
                // rather than agreeing with the template by accident. CH2 is
                // left alone on purpose: the file must carry both shapes.
                ModuleConfig::Timer(c) => {
                    use crate::panels::mcu_module::modules::{
                        PwmChannelConfig, PwmCounting, PwmMode, PwmOutput, PwmPolarity,
                    };
                    c.freq_hz = 20_000;
                    c.set_duty_x100(1, 7_500);
                    c.counting = PwmCounting::CenterBothInterrupts;
                    // Only reaches the code on the timer that has a pair.
                    c.dead_time = 40;
                    c.set_break(
                        1,
                        crate::panels::mcu_module::modules::BreakInputConfig {
                            polarity: crate::panels::mcu_module::modules::BreakPolarity::ActiveHigh,
                            filter: 3,
                        },
                    );
                    c.auto_output_enable = true;
                    c.set_channel(
                        1,
                        PwmChannelConfig {
                            output: PwmOutput::OpenDrain,
                            polarity: PwmPolarity::ActiveLow,
                            mode: PwmMode::Mode2,
                        },
                    );
                }
                // Non-default everything, so the template's substitutions are
                // exercised rather than agreeing with `Config::default()`.
                ModuleConfig::I2s(c) => {
                    use crate::panels::mcu_module::modules::{
                        I2sClockPolarity, I2sFormat, I2sStandard,
                    };
                    c.sample_rate_hz = 44_100;
                    c.standard = I2sStandard::MsbFirst;
                    c.format = I2sFormat::Data24Channel32;
                    c.clock_polarity = I2sClockPolarity::IdleHigh;
                    c.buffer_len = 512;
                }
                ModuleConfig::Sai(c) => {
                    // Non-default on both sub-blocks, and OPPOSITE directions —
                    // the codec case, and the reason the module is the unit.
                    use crate::panels::mcu_module::modules::{
                        SaiBlockConfig, SaiDataSize, SaiTxRx,
                    };
                    c.set_block(
                        1,
                        SaiBlockConfig {
                            tx_rx: SaiTxRx::Transmitter,
                            data_size: SaiDataSize::Data24,
                            frame_length: 64,
                            buffer_len: 512,
                            ..Default::default()
                        },
                    );
                    c.set_block(
                        2,
                        SaiBlockConfig {
                            tx_rx: SaiTxRx::Receiver,
                            data_size: SaiDataSize::Data16,
                            ..Default::default()
                        },
                    );
                }
                ModuleConfig::Hspi(c) => {
                    use crate::panels::mcu_module::modules::{HspiMode, OspiMemoryType};
                    // The mode has to match the pads `EIDE_HSPI` wired.
                    c.mode = match std::env::var("EIDE_HSPI").as_deref() {
                        Ok("2") => HspiMode::Single,
                        _ => HspiMode::Octal,
                    };
                    c.memory_type = OspiMemoryType::HyperBusMemory;
                    c.device_size = 16; // _64MiB
                    c.prescaler = 3;
                }
                ModuleConfig::Xspi(c) => {
                    use crate::panels::mcu_module::modules::{XspiMemoryType, XspiMode};
                    // The mode has to match the pads `EIDE_XSPI` wired.
                    c.mode = match std::env::var("EIDE_XSPI").as_deref() {
                        Ok("2") => XspiMode::Dual,
                        Ok("4") => XspiMode::Quad,
                        Ok("16") => XspiMode::Hexa,
                        _ => XspiMode::Octal,
                    };
                    c.memory_type = XspiMemoryType::ApMemory16Bits;
                    c.device_size = 17; // _128MiB
                    c.prescaler = 2;
                }
                ModuleConfig::Ospi(c) => {
                    use crate::panels::mcu_module::modules::{OspiMemoryType, OspiMode};
                    // The mode has to match the pads `EIDE_OSPI` wired.
                    c.mode = match std::env::var("EIDE_OSPI").as_deref() {
                        Ok("2") => OspiMode::Dual,
                        Ok("8") => OspiMode::Octal,
                        _ => OspiMode::Quad,
                    };
                    c.memory_type = OspiMemoryType::Macronix;
                    c.device_size = 16; // _64MiB
                    c.prescaler = 3;
                }
                ModuleConfig::Dac(c) => {
                    // Non-default start values, so the consts are exercised.
                    c.set_value(1, 2048);
                    c.set_value(2, 4095);
                }
                _ => {}
            }
        }
        // `EIDE_PIN_SPI_TX=DMA1_CH9` pins the SPI's TX channel by hand, the way
        // the Virtual Module's picker does - deliberately a channel automatic
        // allocation would not have reached, so the compile proves the override
        // rather than agreeing with it by accident.
        if let Ok(chan) = std::env::var("EIDE_PIN_SPI_TX") {
            for m in &mut mcu.modules {
                if let ModuleConfig::Spi(c) = &mut m.config {
                    c.dma_tx = chan.clone();
                }
            }
        }

        // `EIDE_IMPORT_WDG=1` switches both watchdogs on at the periods the
        // tab hands out unedited, as the F4 and WBA rows do. Only a compile
        // tells `p.IWDG` from the H7's `p.IWDG1`.
        if std::env::var_os("EIDE_IMPORT_WDG").is_some() {
            use crate::panels::mcu_module::watchdog::{
                IwdgConfig, WwdgConfig, iwdg_range_us, limits_for, wwdg_range_us,
            };
            let l = limits_for(&mcu.family, mcu.runtime);
            mcu.watchdog.iwdg = Some(IwdgConfig {
                timeout_us: iwdg_range_us(&l).1,
            });
            mcu.watchdog.wwdg = Some(WwdgConfig {
                timeout_us: wwdg_range_us(&l, 100_000_000).unwrap().1,
                window_us: 0,
            });
        }
        // `EIDE_BLK_UPGRADE=used`: a project from before the Blocking runtime
        // generated drivers, whose own code wires USART{un} from the raw
        // bindings the way it had to then. Opened, that module must start with
        // "Generate driver" off - and the project must build with the code in.
        let upgraded = std::env::var("EIDE_BLK_UPGRADE").as_deref() == Ok("used");
        let main_rs = if upgraded {
            use crate::panels::mcu_module::modules::ModuleKind;
            let key = (ModuleKind::GenericInterfaceUsart, un);
            let var = |f: PinFunction| {
                mcu.iter_all_pins()
                    .find(|p| p.selected_function == f)
                    .map(|p| super::binding_var(p))
                    .unwrap_or_else(|| panic!("{f:?} is not wired, so nothing uses it"))
            };
            let (rx, tx) = (var(PinFunction::UsartRx(un)), var(PinFunction::UsartTx(un)));
            // The pads bound raw, as every Blocking project bound them then.
            mcu.raw_modules.insert(key);
            let old = mcu.fresh_main_rs();
            mcu.raw_modules.clear();
            let end = old.find(super::GEN_END).expect("the block") + super::GEN_END.len();
            let by_hand = format!(
                "\n    let mut uart = embassy_stm32::usart::Uart::new_blocking(\n        \
                 p.USART{un}, {rx}, {tx}, Default::default()).unwrap();\n    \
                 uart.blocking_write(b\"wired by hand\").ok();\n"
            );
            let old = format!("{}{by_hand}{}", &old[..end], &old[end..]);
            // What the open path does with a config that has no `@drivers`.
            mcu.raw_modules = mcu.modules_used_by_hand(&old);
            assert!(
                mcu.raw_modules.contains(&key),
                "the hand-wired USART{un} was not kept raw: {:?}",
                mcu.raw_modules
            );
            mcu.update_main_rs(&old)
        } else {
            mcu.fresh_main_rs()
        };
        if upgraded {
            let have = mcu.config_files();
            assert!(
                !have.iter().any(|(n, _)| *n == format!("usart{un}.rs")),
                "a driver was generated for the hand-wired USART{un}"
            );
            assert!(main_rs.contains("wired by hand"), "{main_rs}");
        }
        assert!(
            !main_rs.contains("DMA_TX_TODO") && !main_rs.contains("DMA_RX_TODO"),
            "a muxed chip should need no TODO:\n{main_rs}"
        );
        // `EIDE_EXPECT_MAIN=text` names a line main.rs must carry: a row whose
        // point is a refusal (UART5 on DMA, which no F1 routes) would otherwise
        // pass by compiling a project where the pads never got wired.
        if let Ok(want) = std::env::var("EIDE_EXPECT_MAIN") {
            assert!(
                main_rs.contains(&want),
                "main.rs lacks {want:?}:\n{main_rs}"
            );
        }
        // A Blocking EXTI row that armed nothing would pass by building GPIO.
        if mcu.runtime == crate::panels::mcu_module::mcu::model::Runtime::Blocking
            && std::env::var_os("EIDE_EXTI").is_some()
        {
            assert!(
                main_rs.contains("ExtiInput::new_blocking(") && main_rs.contains("#[interrupt]"),
                "no Blocking EXTI handler was generated:\n{main_rs}"
            );
        }
        // `EIDE_EXPECT_CONFIGS=usart1.rs,spi1.rs` names config files the row
        // must generate: a Blocking row whose drivers all fell away would pass
        // by compiling the GPIO alone.
        if let Ok(want) = std::env::var("EIDE_EXPECT_CONFIGS") {
            let have = mcu.config_files();
            let names: Vec<&str> = have.iter().map(|(n, _)| n.as_str()).collect();
            for name in want.split(',').map(str::trim).filter(|n| !n.is_empty()) {
                assert!(names.contains(&name), "{name} was not generated: {names:?}");
            }
        }
        // What the app writes when it saves this async project - a harness
        // that wrote anything else would compile a manifest the IDE never does.
        let files = manifest_sync::project_files(&def, &mcu, &main_rs);
        // A green link proves nothing about RAM: the linker takes any region.
        // memory.x must be the contiguous block the chip has.
        if let Some(map) = stm32_pin_data::embassy_feature_in(&def.project.hal_dep)
            .and_then(stm32_pin_data::memory_map)
        {
            let ram = crate::size::parse_memory_x(&files.memory_x).ram.expect("a RAM region");
            assert_eq!(ram.origin, map.ram_origin as u64, "{}", files.memory_x);
            assert!(ram.length <= map.ram_kib as u64 * 1024, "{}", files.memory_x);
        }
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        let dir = std::env::temp_dir().join(match chosen {
            Some(_) => format!("eide_dma_{}", def.id),
            None => "eide_dma_check".to_owned(),
        });
        project_gen::clear_project_dir_keep_target(&dir);
        project_gen::write_project(&dir, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write dma project");
        println!("wrote {} ({})", dir.display(), def.display_name);
        println!(
            "target: {}  hal: {}",
            def.project.target, def.project.hal_dep
        );
    }
}

#[cfg(test)]
mod af_binding_tests {
    use super::gpio_bindings;
    use crate::panels::mcu_module::pins::logic::pin::{GpioMode, Pin};
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    fn af_pin(name: &str, signal: &str, af: Option<u8>, mode: Option<GpioMode>) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::Other(signal.into());
        p.af = af
            .map(|n| vec![(signal.to_string(), n)])
            .unwrap_or_default();
        p.io_mode = mode;
        p
    }

    /// Index + direction: the pad is wired AS the alternate function.
    #[test]
    fn a_known_af_with_a_mode_is_wired() {
        let p = af_pin("PB6", "SAI1_SD_A", Some(6), Some(GpioMode::PushPull));
        let (uses, body) = gpio_bindings(&[&p]);
        assert!(
            body.contains(
                "set_as_af_unchecked(6, AfType::output(OutputType::PushPull, Speed::Low))"
            ),
            "{body}"
        );
        assert!(body.contains("Flex::new(p.PB6)"), "{body}");
        // Only what the line needs is imported — no unused-import warnings.
        for want in ["Flex", "AfType", "OutputType", "Speed"] {
            assert!(uses.contains(want), "missing {want} in `{uses}`");
        }
        assert!(!uses.contains("Input"), "nothing here is an Input: {uses}");
    }

    /// An input direction picks the other `AfType` constructor and imports Pull.
    #[test]
    fn an_input_mode_uses_the_input_af_type() {
        let p = af_pin("PA3", "SAI1_SD_B", Some(6), Some(GpioMode::PullUp));
        let (uses, body) = gpio_bindings(&[&p]);
        assert!(body.contains("AfType::input(Pull::Up)"), "{body}");
        assert!(uses.contains("Pull"), "{uses}");
        assert!(!uses.contains("OutputType"), "{uses}");
    }

    /// No direction stated -> no guess. The pin binds raw and the comment names
    /// the AF, because an AF signal can be an input in one configuration and an
    /// output in another; wiring it the wrong way round is a silent hardware bug.
    #[test]
    fn a_known_af_without_a_mode_is_not_guessed() {
        let p = af_pin("PB5", "FMC_A0", Some(12), None);
        let (uses, body) = gpio_bindings(&[&p]);
        assert!(body.contains("let pb5_fmc_a0 = p.PB5;"), "{body}");
        assert!(body.contains("AF12"), "the index is still shown: {body}");
        assert!(!body.contains("set_as_af"), "{body}");
        assert!(uses.is_empty(), "nothing to import: {uses}");
    }

    /// No index (STM32F1, or a definition imported before they were captured):
    /// unchanged behaviour, and no mention of an AF that is not known.
    #[test]
    fn an_unknown_af_binds_exactly_as_before() {
        let p = af_pin("PB4", "FMC_A1", None, Some(GpioMode::PushPull));
        let (_, body) = gpio_bindings(&[&p]);
        assert_eq!(body.trim(), "let pb4_fmc_a1 = p.PB4; // FMC_A1");
    }
}

#[cfg(test)]
mod pin_interrupts {
    use crate::panels::mcu_module::builtins::builtin_for;
    use crate::panels::mcu_module::codegen::parse_main_rs;
    use crate::panels::mcu_module::mcu::{Mcu, Runtime};
    use crate::panels::mcu_module::mcu_def::McuDefinition;
    use crate::panels::mcu_module::pins::logic::pin::model::Edge;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    use crate::panels::mcu_module::{manifest_sync, stm32_pin_data};

    /// What an F4 names its EXTI vectors: five private lines, two shared.
    const F4_VECTORS: &[&str] = &[
        "EXTI0",
        "EXTI1",
        "EXTI2",
        "EXTI3",
        "EXTI4",
        "EXTI9_5",
        "EXTI15_10",
    ];

    /// The F103 definition retargeted to an embassy family: `stm32f4` reaches
    /// `StmEmbassyBackend`, `stm32wba` reaches `WbaBackend` (a WBA has one
    /// vector per line). `arm` is the inputs to arm; PA1 is a plain input beside
    /// them, so the two shapes are proved to differ.
    fn def(family: &str, vectors: bool) -> McuDefinition {
        let mut def = builtin_for("stm32f103c8t6").expect("built-in F103");
        def.family = family.into();
        def.id = format!("{family}_armed_input");
        // The dependency line an import writes - the F103's is stm32f1xx-hal,
        // which has no `exti` feature to ask for.
        let (part, target) = if family == "stm32wba" {
            ("STM32WBA55CGUx", "thumbv8m.main-none-eabihf")
        } else {
            ("STM32F411RETx", "thumbv7em-none-eabihf")
        };
        def.project.hal_dep = stm32_pin_data::hal_dep_for_name(family, part);
        def.project.target = target.into();
        if vectors {
            def.irq_vectors = if family == "stm32wba" {
                (0..16).map(|n| format!("EXTI{n}")).collect()
            } else {
                F4_VECTORS.iter().map(|v| (*v).to_owned()).collect()
            };
        }
        def
    }

    fn stm32(family: &str, runtime: Runtime, arm: &[(&str, Edge)]) -> Mcu {
        let mut mcu = def(family, true).build_mcu();
        arm_pins(&mut mcu, runtime, arm);
        mcu
    }

    fn arm_pins(mcu: &mut Mcu, runtime: Runtime, arm: &[(&str, Edge)]) {
        mcu.runtime = runtime;
        for p in mcu.iter_all_pins_mut() {
            if let Some((_, edge)) = arm.iter().find(|(n, _)| *n == p.name) {
                p.selected_function = PinFunction::GpioInput;
                p.irq = Some(*edge);
            } else if p.name == "PA1" {
                p.selected_function = PinFunction::GpioInput;
            }
        }
    }

    const FAMILIES: [&str; 2] = ["stm32f4", "stm32wba"];

    /// An armed input gets a real handler on Blocking: built with
    /// `new_blocking`, parked in a static, served by its vector's `#[interrupt]`,
    /// which calls the user's hook - a plain `fn`, since nothing can await here.
    #[test]
    fn blocking_arms_a_real_handler() {
        for family in FAMILIES {
            for (edge, trigger) in [
                (Edge::Rising, "TriggerEdge::Rising"),
                (Edge::Falling, "TriggerEdge::Falling"),
                (Edge::Both, "TriggerEdge::Any"),
            ] {
                let code = stm32(family, Runtime::Blocking, &[("PA0", edge)]).fresh_main_rs();
                assert!(
                    code.contains(&format!(
                        "let pa0_in = ExtiInput::new_blocking(p.PA0, p.EXTI0, Pull::None, {trigger}); // GPIO Input"
                    )),
                    "{family} {edge:?}:\n{code}"
                );
                assert!(
                    code.contains(
                        "static PA0_IN: Mutex<RefCell<Option<ExtiInput<'static, Blocking>>>> = Mutex::new(RefCell::new(None));"
                    ),
                    "{family}:\n{code}"
                );
                assert!(
                    code.contains("#[interrupt]\nfn EXTI0() {"),
                    "{family}:\n{code}"
                );
                assert!(
                    code.contains(
                        "    if let Some(high) = fired {\n        #[deny(unused_must_use)]\n        on_pa0_in_edge(high);\n    }"
                    ),
                    "{family}:\n{code}"
                );
                // The pending bit is cleared before the hook runs, or the
                // handler re-enters the moment it returns.
                assert!(
                    code.find("pin.clear_pending();") < code.find("on_pa0_in_edge(high);"),
                    "{family}:\n{code}"
                );
                for want in [
                    "use embassy_stm32::exti::{ExtiInput, TriggerEdge};",
                    "use embassy_stm32::interrupt;",
                    "use embassy_stm32::mode::Blocking;",
                    "use core::cell::RefCell;",
                    "use cortex_m::interrupt::Mutex;",
                    // PA1 is still a plain input, so `Input` stays imported.
                    "use embassy_stm32::gpio::{Input, Pull};",
                ] {
                    assert!(code.contains(want), "{family} needs `{want}`:\n{code}");
                }
                assert!(
                    code.contains("let pa1_in = Input::new(p.PA1, Pull::None);"),
                    "{code}"
                );
                // Nothing of the Async shape: no executor, no binding struct.
                for not in [
                    "embassy_executor",
                    "Irqs",
                    "mode::Async",
                    "armed for an interrupt",
                ] {
                    assert!(!code.contains(not), "{family}: `{not}` in\n{code}");
                }
                // Items above the entry, the arming inside it.
                assert!(code.find("fn EXTI0()") < code.find("#[entry]"), "{code}");
                assert!(
                    code.find("#[entry]") < code.find("ExtiInput::new_blocking"),
                    "{code}"
                );
            }
        }
    }

    /// The pin is built AND parked inside one critical section: `new_blocking`
    /// arms the line at once and the NVIC is already open, so an edge before
    /// the static held the pin would reach a handler that cannot clear it.
    #[test]
    fn the_pin_is_built_and_parked_in_one_critical_section() {
        let code = stm32("stm32f4", Runtime::Blocking, &[("PA0", Edge::Rising)]).fresh_main_rs();
        let lines: Vec<&str> = code.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.contains("ExtiInput::new_blocking(p.PA0"))
            .unwrap_or_else(|| panic!("no arming:\n{code}"));
        assert_eq!(
            lines[at - 1],
            "    cortex_m::interrupt::free(|cs| {",
            "{code}"
        );
        assert_eq!(
            lines[at + 1],
            "        *PA0_IN.borrow(cs).borrow_mut() = Some(pa0_in);",
            "{code}"
        );
        assert_eq!(lines[at + 2], "    });", "{code}");
    }

    /// `EXTI9_5` carries five lines: ONE handler asks each of its pins, in line
    /// order, through one `fired` tuple, and calls each pin's hook.
    #[test]
    fn a_shared_vector_asks_each_pin() {
        let code = stm32(
            "stm32f4",
            Runtime::Blocking,
            &[("PB8", Edge::Rising), ("PB5", Edge::Both)],
        )
        .fresh_main_rs();
        assert_eq!(code.matches("fn EXTI9_5()").count(), 1, "{code}");
        assert!(
            code.contains("    let mut fired = (None, None);\n"),
            "{code}"
        );
        assert!(code.contains("`EXTI9_5` is shared by 2 lines"), "{code}");
        let pb5 = code.find("PB5_IN.borrow(cs)").expect("PB5 asked");
        let pb8 = code.find("PB8_IN.borrow(cs)").expect("PB8 asked");
        assert!(pb5 < pb8, "line order:\n{code}");
        assert!(code.contains("fired.0 = Some(pin.is_high());"), "{code}");
        assert!(code.contains("fired.1 = Some(pin.is_high());"), "{code}");
        assert!(code.contains("on_pb5_in_edge(high);"), "{code}");
        assert!(code.contains("on_pb8_in_edge(high);"), "{code}");
        assert!(
            code.contains("TriggerEdge::Any"),
            "PB5 is on both edges:\n{code}"
        );
    }

    /// PA5 and PB5 are both line 5. One gets it; the other stays a plain input
    /// and says why - the same pad, refused for the same reason, as on Async.
    #[test]
    fn a_line_clash_is_refused_in_writing() {
        let code = stm32(
            "stm32f4",
            Runtime::Blocking,
            &[("PA5", Edge::Rising), ("PB5", Edge::Rising)],
        )
        .fresh_main_rs();
        assert!(
            code.contains("ExtiInput::new_blocking(p.PA5, p.EXTI5"),
            "{code}"
        );
        assert!(
            code.contains("let pb5_in = Input::new(p.PB5, Pull::None);"),
            "{code}"
        );
        assert!(
            code.contains("    // PB5 is NOT on an interrupt: EXTI line 5 is already taken by\n"),
            "{code}"
        );
        assert!(
            !code.contains("PB5_IN"),
            "a refused pad gets no static:\n{code}"
        );
        assert!(!code.contains("on_pb5_in_edge"), "nor a hook:\n{code}");
    }

    /// A chip imported without vendor data has no vector list: every armed
    /// input is refused, said so, and nothing EXTI is imported.
    #[test]
    fn no_vectors_no_handler() {
        for family in FAMILIES {
            let mut mcu = def(family, false).build_mcu();
            arm_pins(&mut mcu, Runtime::Blocking, &[("PA0", Edge::Rising)]);
            let code = mcu.fresh_main_rs();
            assert!(
                code.contains("let pa0_in = Input::new(p.PA0, Pull::None);"),
                "{code}"
            );
            assert!(
                code.contains("names none for\n    // EXTI line 0"),
                "{code}"
            );
            for not in ["#[interrupt]", "embassy_stm32::exti", "on_pa0_in_edge"] {
                assert!(!code.contains(not), "{family}: `{not}` in\n{code}");
            }
        }
    }

    /// An unarmed input is untouched — arming is opt-in, not implied.
    #[test]
    fn an_unarmed_input_has_no_handler() {
        for family in FAMILIES {
            let code = stm32(family, Runtime::Blocking, &[]).fresh_main_rs();
            for not in ["#[interrupt]", "static ", "embassy_stm32::exti", "_edge("] {
                assert!(!code.contains(not), "{family}: `{not}` in\n{code}");
            }
            assert!(
                code.contains("let pa1_in = Input::new(p.PA1, Pull::None);"),
                "{code}"
            );
        }
    }

    /// The hook is the user's: seeded once below the tail as a plain `fn`,
    /// never rewritten, and a regeneration keeps what they wrote in it.
    #[test]
    fn the_hook_is_seeded_once_and_kept() {
        for family in FAMILIES {
            let mcu = stm32(family, Runtime::Blocking, &[("PA0", Edge::Rising)]);
            let fresh = mcu.fresh_main_rs();
            assert_eq!(
                fresh.matches("fn on_pa0_in_edge(_high: bool) {").count(),
                1,
                "{fresh}"
            );
            assert!(!fresh.contains("async fn on_pa0_in_edge"), "{fresh}");
            let end = fresh
                .find(crate::panels::mcu_module::codegen::GEN_END)
                .expect("block end");
            assert!(
                fresh.find("fn on_pa0_in_edge(") > Some(end),
                "below the block:\n{fresh}"
            );
            let edited = fresh.replace(
                "fn on_pa0_in_edge(_high: bool) {\n    // Your code here.\n",
                "fn on_pa0_in_edge(_high: bool) {\n    // Your code here.\n    user_did_this();\n",
            );
            assert_ne!(edited, fresh, "the seed has the shape this test edits");
            let again = mcu.update_main_rs(&mcu.update_main_rs(&edited));
            assert!(again.contains("    user_did_this();\n"), "{again}");
            assert_eq!(again.matches("fn on_pa0_in_edge(").count(), 1, "{again}");
        }
    }

    /// An untouched project switched between Blocking and Async lands on the
    /// project the other runtime writes fresh: the hook keeps its NAME, and
    /// its seed swaps `fn` and `async fn` with its caller.
    #[test]
    fn a_pristine_switch_lands_on_the_fresh_project() {
        for family in FAMILIES {
            let arm = [("PA0", Edge::Falling)];
            let blocking = stm32(family, Runtime::Blocking, &arm);
            let asynch = stm32(family, Runtime::Async, &arm);
            let b_fresh = blocking.fresh_main_rs();
            let a_fresh = asynch.fresh_main_rs();
            assert!(
                a_fresh.contains("async fn on_pa0_in_edge(_high: bool)"),
                "{a_fresh}"
            );
            assert_eq!(
                asynch.update_main_rs(&b_fresh),
                a_fresh,
                "{family}: Blocking -> Async"
            );
            assert_eq!(
                blocking.update_main_rs(&a_fresh),
                b_fresh,
                "{family}: Async -> Blocking"
            );
        }
    }

    /// The Pins canvas still reads every input back - the armed one from the
    /// `let` inside the critical section, like any other.
    #[test]
    fn the_pins_still_parse() {
        for family in FAMILIES {
            let code = stm32(
                family,
                Runtime::Blocking,
                &[("PA0", Edge::Rising), ("PB5", Edge::Both)],
            )
            .fresh_main_rs();
            let parsed = parse_main_rs(&code);
            for pad in ["PA0", "PA1", "PB5"] {
                assert!(
                    parsed
                        .iter()
                        .any(|(n, f)| n == pad && *f == PinFunction::GpioInput),
                    "{family}: {pad} reads back as an input: {parsed:?}"
                );
            }
            assert_eq!(parsed.len(), 3, "{family}: nothing extra: {parsed:?}");
        }
    }

    /// `embassy_stm32::exti` is behind a Cargo feature: an armed Blocking
    /// project asks for it, and one with nothing armed does not.
    #[test]
    fn the_manifest_asks_for_exti_only_when_armed() {
        for family in FAMILIES {
            let def = def(family, true);
            for (arm, want) in [(&[("PA0", Edge::Rising)][..], true), (&[][..], false)] {
                let mut mcu = def.build_mcu();
                arm_pins(&mut mcu, Runtime::Blocking, arm);
                let main_rs = mcu.fresh_main_rs();
                let toml = manifest_sync::project_files(&def, &mcu, &main_rs).cargo_toml;
                assert_eq!(
                    toml.lines()
                        .any(|l| l.trim_start().starts_with("embassy-stm32")
                            && l.contains("\"exti\"")),
                    want,
                    "{family} armed={want}:\n{toml}"
                );
            }
        }
    }

    /// The backend reports the hooks its handlers call - plain `fn`s, named as
    /// the Async tasks name them, one per pad that got a line.
    #[test]
    fn the_backend_reports_one_sync_hook_per_armed_pad() {
        use crate::panels::mcu_module::codegen::family::backend_for_runtime;
        for family in FAMILIES {
            let mcu = stm32(
                family,
                Runtime::Blocking,
                &[
                    ("PA5", Edge::Rising),
                    ("PB5", Edge::Rising),
                    ("PA0", Edge::Both),
                ],
            );
            let hooks = backend_for_runtime(&mcu.family, mcu.runtime)
                .expect("a backend")
                .edge_hooks(&mcu);
            let names: Vec<&str> = hooks.iter().map(|h| h.name.as_str()).collect();
            assert_eq!(
                names,
                ["on_pa0_in_edge", "on_pa5_in_edge"],
                "{family}: PB5 lost line 5"
            );
            assert!(hooks.iter().all(|h| !h.is_async), "{family}: {hooks:?}");
            assert_eq!(hooks[0].caller, "EXTI0", "{family}");
        }
    }
}
