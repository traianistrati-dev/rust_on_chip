//! RTIC 2.x code generation for STM32F1 (`Runtime::Rtic`).
//!
//! # What changes versus the bare-metal path
//!
//! The init sequence is IDENTICAL — same clock chain, same port splits, same pin
//! bindings, same `pins::configs::*::init(...)` calls. It is produced by
//! [`super::stm32::gen_parts`], the very function the blocking generator uses,
//! so the two cannot drift. What differs is where that sequence lives and who
//! calls it: RTIC owns `main`, so init moves into `#[init]` and its results are
//! handed to the framework as `Local` / `Shared` resources.
//!
//! # Interrupts
//!
//! A GPIO input with [`Edge`] set becomes a hardware task. RTIC binds the task
//! to the vector and enables it in the NVIC itself, which is why the generated
//! code never calls `NVIC::unmask` — doing so is at best redundant and at worst
//! races the framework's own setup.
//!
//! On STM32 the EXTI lines are not one vector each: 0-4 have their own, 5-9
//! share `EXTI9_5`, and 10-15 share `EXTI15_10`. A shared vector therefore
//! produces ONE task that asks each of its pins `check_interrupt()` — two tasks
//! bound to the same vector is a compile error from the RTIC macro.

use super::common::{AUTOGEN_BANNER, GEN_BEGIN, GEN_END, pin_binding};
use super::stm32::{Binding, GenParts, gen_parts};
use crate::panels::mcu_module::clock::ClockConfig;
use crate::panels::mcu_module::modules::{
    CanModuleConfig, I2cModuleConfig, SpiModuleConfig, TimerModuleConfig, UsartModuleConfig,
    UsbModuleConfig,
};
use crate::panels::mcu_module::pins::logic::pin::{Edge, Pin};
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
use std::collections::BTreeMap;

/// A GPIO pin that survives `#[init]` as an RTIC `Local` resource.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalPin {
    pub name: String,
    pub binding: String,
    /// `true` for an input, `false` for an output — picks the HAL type.
    pub input: bool,
}

/// Every plain GPIO pin, in binding order.
///
/// Bus pins are deliberately absent: `pins::configs::*::init(...)` has already
/// consumed them, so there is no value left to hand to the framework.
pub fn local_pins(all_pins: &[&Pin]) -> Vec<LocalPin> {
    all_pins
        .iter()
        .filter(|p| !p.reserved)
        .filter_map(|p| {
            let input = match p.selected_function {
                PinFunction::GpioInput => true,
                PinFunction::GpioOutput => false,
                _ => return None,
            };
            Some(LocalPin {
                binding: pin_binding(
                    &p.name.to_ascii_lowercase(),
                    &p.selected_function,
                    &p.custom_label,
                ),
                name: p.name.clone(),
                input,
            })
        })
        .collect()
}

/// A bus peripheral handle promoted to an RTIC `Local` resource.
#[derive(Clone, Debug, PartialEq)]
pub struct BusHandle {
    /// Binding without the leading underscore, e.g. `serial1_gps`.
    pub binding: String,
    /// The named type its config module exposes.
    pub ty: String,
}

/// The bus bindings the blocking generator emits, mapped to the config module
/// that names their `Handle`.
///
/// `numbered` says whether the binding carries the instance number, which is
/// also where the module name comes from (`_serial1_gps` → `usart1`). CAN does
/// not: STM32F1 has exactly one, and its file is always `can1.rs`.
const BUS_HANDLES: &[(&str, &str, bool)] = &[
    ("_serial", "usart", true),
    ("_spi", "spi", true),
    ("_i2c", "i2c", true),
    ("_can", "can1", false),
    // PWM joined the list when the timers got their own config module: the
    // frequency, the duty and the remap live in `pwm{N}.rs`, and with them a
    // `Handle` alias — so RTIC no longer needs the type spelled out by hand.
    ("_pwm", "pwm", true),
];

/// Promote the bus handles in `fn_calls` from dropped temporaries to named
/// bindings, returning the rewritten block and the resources to declare.
///
/// The blocking generator writes `let mut _serial1 = ...`: the underscore says
/// "configured, never used again", which is right when `fn main` continues into
/// the user's loop. Under RTIC `#[init]` RETURNS, so that value is dropped and
/// the peripheral becomes unreachable. Dropping the underscore and handing it to
/// the framework is the whole fix.
///
/// Every bus is covered now that `configs/{usart,spi,i2c,can1}{N}.rs` all expose
/// a named `Handle` — SPI and I2C needed concrete pin types in the alias first,
/// because the HAL keeps the pins inside the handle's own type.
pub fn promote_bus_handles(fn_calls: &str) -> (String, Vec<BusHandle>) {
    let mut out = String::new();
    let mut found = Vec::new();
    for line in fn_calls.lines() {
        let promoted = (|| {
            let (indent, rest) = line.split_at(line.len() - line.trim_start().len());
            // SPI/I2C bind without `mut` (nothing writes through them here),
            // USART and CAN with it.
            let rest = rest.strip_prefix("let ")?;
            let rest = rest.strip_prefix("mut ").unwrap_or(rest);
            let (name, module) = BUS_HANDLES.iter().find_map(|(pfx, stem, numbered)| {
                let (tail, _) = rest.strip_prefix(pfx)?.split_once(" =")?;
                let n: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
                if *numbered && n.is_empty() {
                    return None;
                }
                // `_serial1_gps` → binding `serial1_gps`, module `usart1`.
                Some((format!("{}{tail}", &pfx[1..]), format!("{stem}{n}")))
            })?;
            found.push(BusHandle {
                binding: name.clone(),
                ty: format!("pins::configs::{module}::Handle"),
            });
            // No `mut`: the value is only moved into the resource here, and
            // RTIC hands tasks a `&mut` to it anyway.
            Some(format!("{indent}let {name}{}", &rest[rest.find(" =")?..]))
        })();
        out.push_str(&promoted.unwrap_or_else(|| line.to_string()));
        out.push('\n');
    }
    (out, found)
}

/// The same promotion as [`promote_bus_handles`], for the peripherals whose
/// type `gen_parts` had to spell out itself (see `GenParts::inline_handles`).
///
/// Only the `let mut _x = ` prefix moves: everything to the right of the `=` is
/// the init expression and is left exactly as the shared generator wrote it, so
/// the two runtimes cannot drift on what a peripheral is, only on where it
/// lives.
pub fn promote_inline_handles(
    fn_calls: &str,
    handles: &[(String, String, bool)],
) -> (String, Vec<BusHandle>) {
    let mut out = fn_calls.to_owned();
    let mut found = Vec::new();
    for (binding, ty, mutated) in handles {
        let old = format!("let mut {binding} = ");
        if !out.contains(&old) {
            continue;
        }
        let name = binding.trim_start_matches('_').to_owned();
        // The buses drop their `mut` because init only MOVES them, and RTIC
        // hands tasks a `&mut` anyway. A PWM is different: init sets its duty
        // and enables its channels, both through `&mut self`.
        let keep = if *mutated { "let mut " } else { "let " };
        out = out.replace(&old, &format!("{keep}{name} = "));
        // Anything the init block said about the old name — `_pwm2_max`, the
        // `set_duty` / `enable` calls — has to follow it.
        out = out.replace(&format!("{binding}."), &format!("{name}."));
        out = out.replace(&format!("{binding}_max"), &format!("{name}_max"));
        found.push(BusHandle {
            binding: name,
            ty: ty.clone(),
        });
    }
    (out, found)
}

/// One interrupt-enabled input pin, resolved to what the generator needs.
#[derive(Clone, Debug, PartialEq)]
pub struct IrqPin {
    /// Chip pin name, e.g. `PA5`.
    pub name: String,
    /// Generated binding, e.g. `pa5_in_button`.
    pub binding: String,
    /// EXTI line — the pin INDEX, not the port (PA5, PB5 and PC5 all use 5).
    pub line: u8,
    pub edge: Edge,
}

/// NVIC vector for an EXTI line.
///
/// The grouping is the whole reason shared-vector branching exists: lines 5-9
/// and 10-15 have one vector between them, so the pins on them cannot each get
/// their own task.
pub fn exti_vector(line: u8) -> &'static str {
    match line {
        0 => "EXTI0",
        1 => "EXTI1",
        2 => "EXTI2",
        3 => "EXTI3",
        4 => "EXTI4",
        5..=9 => "EXTI9_5",
        _ => "EXTI15_10",
    }
}

/// The EXTI line of a pin name (`PA5` -> 5). `None` when the name is not a
/// port+index pin.
pub fn exti_line(pin_name: &str) -> Option<u8> {
    let rest = pin_name.strip_prefix(['P', 'p'])?;
    let mut cs = rest.chars();
    let port = cs.next()?;
    if !port.is_ascii_alphabetic() {
        return None;
    }
    let idx: String = cs.take_while(|c| c.is_ascii_digit()).collect();
    idx.parse().ok().filter(|n| *n <= 15)
}

/// Every input pin the user asked to interrupt on, in vector order.
pub fn irq_pins(all_pins: &[&Pin]) -> Vec<IrqPin> {
    let mut out: Vec<IrqPin> = all_pins
        .iter()
        .filter(|p| !p.reserved && p.selected_function == PinFunction::GpioInput)
        .filter_map(|p| {
            let edge = p.irq?;
            let line = exti_line(&p.name)?;
            Some(IrqPin {
                binding: pin_binding(
                    &p.name.to_ascii_lowercase(),
                    &p.selected_function,
                    &p.custom_label,
                ),
                name: p.name.clone(),
                line,
                edge,
            })
        })
        .collect();
    out.sort_by_key(|p| (p.line, p.name.clone()));
    out
}

/// Group the pins by the vector they land on, preserving line order.
pub fn by_vector(pins: &[IrqPin]) -> Vec<(&'static str, Vec<&IrqPin>)> {
    let mut groups: BTreeMap<&'static str, Vec<&IrqPin>> = BTreeMap::new();
    for p in pins {
        groups.entry(exti_vector(p.line)).or_default().push(p);
    }
    // Vector order follows the lowest line in each group, so the tasks appear in
    // the same order as the pins above them.
    let mut v: Vec<(&'static str, Vec<&IrqPin>)> = groups.into_iter().collect();
    v.sort_by_key(|(_, ps)| ps.first().map(|p| p.line).unwrap_or(u8::MAX));
    v
}

/// The `#[init]` lines that arm one pin's EXTI source.
///
/// `make_interrupt_source` + `trigger_on_edge` + `enable_interrupt` is the
/// stm32f1xx-hal `ExtiPin` sequence; AFIO is required because the interrupt
/// source multiplexer lives there.
fn arm_lines(p: &IrqPin) -> String {
    let b = &p.binding;
    let edge = p.edge.hal_variant();
    // One `\n`-joined block with NO leading indentation of its own: `indent()`
    // is what places it. A `\` string continuation eats the next line's
    // indentation, which is how this came out ragged the first time.
    format!(
        "    {b}.make_interrupt_source(&mut afio);\n\
         \x20   {b}.trigger_on_edge(&mut dp.EXTI, {edge});\n\
         \x20   {b}.enable_interrupt(&mut dp.EXTI);\n"
    )
}

/// One task per vector. A shared vector branches per pin; a private one does not
/// need to ask, since only that pin can have raised it.
fn tasks(pins: &[IrqPin]) -> String {
    let mut out = String::new();
    for (vector, group) in by_vector(pins) {
        let locals = group
            .iter()
            .map(|p| p.binding.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push_str(&format!(
            "\n    #[task(binds = {vector}, local = [{locals}])]\n\
             \x20   fn {fname}(cx: {fname}::Context) {{\n",
            fname = vector.to_ascii_lowercase(),
        ));
        if group.len() == 1 {
            let p = group[0];
            out.push_str(&format!(
                "        // {name} (EXTI line {line}).\n\
                 \x20       cx.local.{b}.clear_interrupt_pending_bit();\n\
                 \x20       // Your handler code here.\n",
                name = p.name,
                line = p.line,
                b = p.binding,
            ));
        } else {
            out.push_str(&format!(
                "        // {vector} is shared by {} pins - ask each which one fired.\n",
                group.len()
            ));
            for p in group {
                out.push_str(&format!(
                    "        if cx.local.{b}.check_interrupt() {{\n\
                     \x20           cx.local.{b}.clear_interrupt_pending_bit();\n\
                     \x20           // {name} (EXTI line {line}): your handler code here.\n\
                     \x20       }}\n",
                    b = p.binding,
                    name = p.name,
                    line = p.line,
                ));
            }
        }
        out.push_str("    }\n");
    }
    out
}

/// What sits after the GEN block in an RTIC project.
///
/// The bare-metal `USER_TAIL` is `loop { ... }\n}` — the body and closing brace
/// of `fn main`. RTIC has no `fn main` (the macro writes it), so reusing that
/// tail leaves an unmatched `}` and the file does not parse. User code goes
/// INSIDE the app module as tasks, so out here there is nothing to close.
/// The invariant file header for an RTIC project.
///
/// The same shape as [`super::stm32::invariant_header`] minus
/// `use cortex_m_rt::entry` — RTIC's macro writes `fn main`, so importing
/// `entry` would be a dead import in every generated project. A SEPARATE header
/// rather than a flag on the shared one: the other runtimes' output must not
/// move.
pub fn invariant_header(mcu_name: &str, mcu_id: &str) -> String {
    format!(
        "{AUTOGEN_BANNER}\n\
         // MCU: {mcu_name} | HAL: stm32f1xx-hal | Runtime: RTIC 2\n\
         {id}\n\
         #![no_std]\n\
         #![no_main]\n\n\
         pub mod pins;\n\n\
         use panic_halt as _;\n\n",
        id = super::common::mcu_id_marker_line(mcu_id),
    )
}

pub const RTIC_USER_TAIL: &str = "// Add helpers, `impl`s and `use`s for your tasks here.\n     // Task bodies live inside the `mod app` block above.\n";

/// Where a tail stops closing the entry fn the GEN section used to open: the
/// byte just after the `}` that takes the brace depth below zero, or `None`
/// when no brace does - the tail is already RTIC-shaped.
///
/// The switch that matters: turning an EXISTING bare-metal or Async project
/// into an RTIC one leaves the old `loop { ... }\n}` behind the markers. That
/// much closed a `fn main` that no longer exists, so it goes; whatever the user
/// wrote AFTER it - helpers, `impl`s, and on a round trip the RTIC tail itself
/// - is module-level code and stays.
///
/// Braces inside comments, strings and char literals do not count, so a `'}'`
/// or a `"{"` in the user's loop cannot move the cut into their own code.
fn entry_close(tail: &str) -> Option<usize> {
    let b = tail.as_bytes();
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
    let mut depth = 0i64;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                i = tail[i..].find('\n').map_or(b.len(), |n| i + n);
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // Rust's block comments nest.
                let mut nest = 1;
                i += 2;
                while i < b.len() && nest > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        nest += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        nest -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            // A raw string, `r"..."` / `r#"..."#` (also after a `b`).
            b'r' if matches!(b.get(i + 1), Some(b'"' | b'#'))
                && (i == 0
                    || !ident(b[i - 1])
                    || (b[i - 1] == b'b' && (i < 2 || !ident(b[i - 2])))) =>
            {
                let mut j = i + 1;
                while b.get(j) == Some(&b'#') {
                    j += 1;
                }
                if b.get(j) == Some(&b'"') {
                    let close = format!("\"{}", "#".repeat(j - i - 1));
                    i = tail[j + 1..]
                        .find(&close)
                        .map_or(b.len(), |n| j + 1 + n + close.len());
                } else {
                    i += 1; // `r#ident`, a raw identifier
                }
                continue;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
                continue;
            }
            b'\'' => {
                // A char literal ('{', '\'', '\u{7d}') or a lifetime ('a).
                if b.get(i + 1) == Some(&b'\\') {
                    // Past the quote, the backslash and the escaped char.
                    let from = (i + 3).min(b.len());
                    i = tail[from..].find('\'').map_or(b.len(), |n| from + n + 1);
                    continue;
                }
                let c = tail[i + 1..].chars().next().map_or(1, char::len_utf8);
                if tail[i + 1..].get(c..).is_some_and(|r| r.starts_with('\'')) {
                    i += c + 2;
                    continue;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Re-splice an RTIC GEN block, replacing an inherited bare-metal tail.
///
/// Coming from another runtime the header is refitted to RTIC (its label, no
/// dead `entry` import) - or rebuilt, when the Async runtime wrote it: an RTIC
/// project has no `embassy_executor` to import. Between two RTIC splices it is
/// kept verbatim.
///
/// The tail loses only what closed the old `fn main` ([`entry_close`]); what
/// follows it is kept, under RTIC's own seed comment unless it already starts
/// with it - so RTIC -> Blocking -> RTIC gives back the tail it started with.
pub fn splice_rtic_section(
    existing: &str,
    new_section: &str,
    mcu_name: &str,
    mcu_id: &str,
) -> String {
    let header = invariant_header(mcu_name, mcu_id);
    let (Some(begin), Some(end_start)) = (existing.find(GEN_BEGIN), existing.find(GEN_END)) else {
        return format!("{header}{new_section}\n{RTIC_USER_TAIL}");
    };
    let head = &existing[..begin];
    let head = if head.contains("embassy_executor") {
        header
    } else if super::common::section_is_rtic(existing) {
        head.to_owned()
    } else {
        super::common::refit_header(head, &header)
    };
    let after = existing[end_start + GEN_END.len()..].trim_start_matches('\n');
    let Some(cut) = entry_close(after) else {
        return format!("{head}{new_section}\n{after}");
    };
    let rest = after[cut..].trim_start_matches('\n');
    let seed_head = RTIC_USER_TAIL.lines().next().unwrap_or_default();
    let tail = if rest.trim().is_empty() {
        RTIC_USER_TAIL.to_owned()
    } else if rest.starts_with(seed_head) {
        rest.to_owned()
    } else {
        format!("{RTIC_USER_TAIL}\n{rest}")
    };
    format!("{head}{new_section}\n{tail}")
}

/// The full `main.rs` GEN block for an RTIC project.
#[allow(clippy::too_many_arguments)]
pub fn make_generated_section(
    mcu_name: &str,
    all_pins: &[&Pin],
    clock: &ClockConfig,
    usart: &BTreeMap<u8, UsartModuleConfig>,
    spi: &BTreeMap<u8, SpiModuleConfig>,
    i2c: &BTreeMap<u8, I2cModuleConfig>,
    can: &BTreeMap<u8, CanModuleConfig>,
    usb: &BTreeMap<u8, UsbModuleConfig>,
    timer: &BTreeMap<u8, TimerModuleConfig>,
    gpio_native: bool,
    custom_inits: &str,
) -> String {
    let Some(parts) = gen_parts(
        Binding::Owned,
        all_pins,
        clock,
        usart,
        spi,
        i2c,
        can,
        usb,
        timer,
        gpio_native,
        custom_inits,
    ) else {
        return default_section(mcu_name, clock);
    };
    let GenParts {
        use_block,
        extra_uses,
        clock_chain,
        port_splits,
        pin_section,
        fn_calls,
        inline_handles,
        ..
    } = parts;

    let irqs = irq_pins(all_pins);
    let locals_all = local_pins(all_pins);
    let (fn_calls, buses) = promote_bus_handles(&fn_calls);
    // The ADC and each PWM timer have no config module to name a `Handle`, so
    // `gen_parts` hands their concrete types over instead. Without this they
    // were built in `#[init]` and dropped the moment it returned: the ADC
    // unusable, and a PWM that keeps whatever duty init programmed but can
    // never be changed again.
    let (fn_calls, inline) = promote_inline_handles(&fn_calls, &inline_handles);
    let buses: Vec<BusHandle> = buses.into_iter().chain(inline).collect();
    // `make_interrupt_source(&mut self)` needs a mutable binding. Rewriting only
    // the armed pins keeps `unused_mut` off the polled ones.
    let pin_section = irqs.iter().fold(pin_section, |acc, p| {
        acc.replace(
            &format!("let {} =", p.binding),
            &format!("let mut {} =", p.binding),
        )
    });
    // AFIO is unconditional here, unlike the blocking path: arming an EXTI
    // source needs it even on a project whose peripherals do not.
    let arming: String = irqs.iter().map(arm_lines).collect();
    let locals = locals_all
        .iter()
        .map(|p| p.binding.clone())
        .chain(buses.iter().map(|b| b.binding.clone()))
        .map(|b| format!("            {b},\n"))
        .collect::<String>();
    let task_fns = tasks(&irqs);

    format!(
        "{GEN_BEGIN}\n\
         // RTIC writes `fn main` and names the PAC by path, so the shared\n\
         // header's `entry` / `pac` imports are unused on this runtime.\n\
         #[allow(unused_imports)]\n\
         use rtic_monotonics::systick::prelude::*;\n\
         #[allow(unused_imports)]\n\
         use stm32f1xx_hal::{{\n\
         {use_block}\n\
         \x20   gpio::{{self, Edge, ExtiPin}},\n\
         }};\n\
         {extra_uses}\
         \n\
         systick_monotonic!(Mono, 1_000);\n\
         \n\
         #[rtic::app(device = stm32f1xx_hal::pac, peripherals = true)]\n\
         // Resources exist for YOUR tasks to use, and a freshly generated\n\
         // project has not written those yet. The allow sits on the MODULE\n\
         // because `#[rtic::app]` re-emits `struct Local` itself: one on the\n\
         // struct is consumed with the source and never reaches the fields the\n\
         // lint actually reports.\n\
         #[allow(dead_code)]\n\
         mod app {{\n\
         \x20   use super::*;\n\
         \n\
         \x20   #[shared]\n\
         \x20   struct Shared {{}}\n\
         \n\
         \x20   #[local]\n\
         \x20   struct Local {{\n\
         {local_fields}\
         \x20   }}\n\
         \n\
         \x20   #[init]\n\
         \x20   fn init(cx: init::Context) -> (Shared, Local) {{\n\
         \x20       let mut dp = cx.device;\n\
         \x20       let mut flash = dp.FLASH.constrain();\n\
         \x20       let rcc = dp.RCC.constrain();\n\
         \x20       let mut afio = dp.AFIO.constrain();\n\
         \x20       let clocks = {clock_chain_i};\n\
         \x20       Mono::start(cx.core.SYST, clocks.sysclk().to_Hz());\n\n\
         {port_splits_i}\n\
         {pin_section_i}\
         {fn_calls_i}\
         {arming_i}\n\
         \x20       (Shared {{}}, Local {{{local_init}}})\n\
         \x20   }}\n\
         \n\
         \x20   #[idle]\n\
         \x20   fn idle(_cx: idle::Context) -> ! {{\n\
         \x20       loop {{\n\
         \x20           rtic::export::wfi();\n\
         \x20       }}\n\
         \x20   }}\n\
         {task_fns}\
         }}\n\
         {GEN_END}\n",
        clock_chain_i = clock_chain.replace('\n', "\n        "),
        local_fields = format!(
            "{}{}",
            locals_decl(&locals_all),
            buses
                .iter()
                .map(|b| format!("        {}: {},\n", b.binding, b.ty))
                .collect::<String>()
        ),
        local_init = if locals_all.is_empty() && buses.is_empty() {
            String::new()
        } else {
            format!("\n{locals}        ")
        },
        port_splits_i = indent(&port_splits),
        pin_section_i = indent(&pin_section),
        fn_calls_i = indent(&fn_calls),
        arming_i = indent(&arming),
    )
}

/// `Local` field declarations, typed by inference from `#[init]`'s return.
///
/// RTIC needs concrete types here, and the HAL pin types are long and
/// port-specific (`gpioa::PA5<Input<Floating>>`). `gen_parts` already emits the
/// binding, so the type is spelled out per pin from its name.
fn locals_decl(pins: &[LocalPin]) -> String {
    pins.iter()
        .map(|p| {
            let port = p.name.chars().nth(1).unwrap_or('a').to_ascii_lowercase();
            let ty = if p.input {
                "gpio::Input<gpio::Floating>"
            } else {
                "gpio::Output<gpio::PushPull>"
            };
            format!(
                "        {b}: gpio::gpio{port}::{up}<{ty}>,\n",
                b = p.binding,
                up = p.name.to_ascii_uppercase(),
            )
        })
        .collect()
}

/// Re-indent a generated block by one level, so init code sits inside `mod app`.
/// Blank lines stay blank rather than becoming trailing whitespace.
fn indent(block: &str) -> String {
    block
        .lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::from("\n")
            } else {
                format!("    {l}\n")
            }
        })
        .collect()
}

/// An RTIC skeleton for a project with no pins configured yet.
fn default_section(mcu_name: &str, clock: &ClockConfig) -> String {
    let clock_chain = super::stm32::clock_setup_chain(clock);
    format!(
        "{GEN_BEGIN}\n\
         // MCU: {mcu_name}\n\
         use rtic_monotonics::systick::prelude::*;\n\
         use stm32f1xx_hal::{{pac, prelude::*}};\n\
         \n\
         systick_monotonic!(Mono, 1_000);\n\
         \n\
         #[rtic::app(device = stm32f1xx_hal::pac, peripherals = true)]\n\
         mod app {{\n\
         \x20   use super::*;\n\
         \n\
         \x20   #[shared]\n\
         \x20   struct Shared {{}}\n\
         \n\
         \x20   #[local]\n\
         \x20   struct Local {{}}\n\
         \n\
         \x20   #[init]\n\
         \x20   fn init(cx: init::Context) -> (Shared, Local) {{\n\
         \x20       let dp = cx.device;\n\
         \x20       let mut flash = dp.FLASH.constrain();\n\
         \x20       let rcc = dp.RCC.constrain();\n\
         \x20       let clocks = {clock_chain};\n\
         \x20       Mono::start(cx.core.SYST, clocks.sysclk().to_Hz());\n\
         \x20       // Select pins in the MCU Configurator to generate code here.\n\
         \x20       (Shared {{}}, Local {{}})\n\
         \x20   }}\n\
         \n\
         \x20   #[idle]\n\
         \x20   fn idle(_cx: idle::Context) -> ! {{\n\
         \x20       loop {{\n\
         \x20           rtic::export::wfi();\n\
         \x20       }}\n\
         \x20   }}\n\
         }}\n\
         {GEN_END}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exti_lines_come_from_the_pin_index_not_the_port() {
        assert_eq!(exti_line("PA5"), Some(5));
        assert_eq!(exti_line("PB5"), Some(5));
        assert_eq!(exti_line("PC13"), Some(13));
        assert_eq!(exti_line("PA0"), Some(0));
        // Not a port+index pin.
        assert_eq!(exti_line("VDD"), None);
        assert_eq!(exti_line("PA16"), None, "no EXTI line above 15");
        assert_eq!(exti_line(""), None);
    }

    /// The grouping that forces shared-vector branching.
    #[test]
    fn vectors_group_5_to_9_and_10_to_15() {
        assert_eq!(exti_vector(0), "EXTI0");
        assert_eq!(exti_vector(4), "EXTI4");
        assert_eq!(exti_vector(5), "EXTI9_5");
        assert_eq!(exti_vector(9), "EXTI9_5");
        assert_eq!(exti_vector(10), "EXTI15_10");
        assert_eq!(exti_vector(15), "EXTI15_10");
    }

    /// Under RTIC every bus handle has to reach the tasks, and the only way in
    /// is a `Local` field — which must name its type. `_spi1` / `_i2c1` were the
    /// last two still dropped at the end of `#[init]`.
    #[test]
    fn every_bus_handle_becomes_a_named_local() {
        let calls = "\
    // ── Peripheral initialisation ──
    let mut _serial1_gps = pins::configs::usart1::init(dp.USART1, (pa9_usart1_tx, pa10_usart1_rx), &mut afio, &clocks);
    let _spi1 = pins::configs::spi1::init(dp.SPI1, (pa5_spi1_sck, pa6_spi1_miso, pa7_spi1_mosi), &mut afio, &clocks);
    let _i2c2_imu = pins::configs::i2c2::init(dp.I2C2, (pb10_i2c2_scl, pb11_i2c2_sda), &mut afio, &clocks);
    let mut _can = pins::configs::can1::init(dp.CAN1, (pa12_can_tx, pa11_can_rx), &mut afio);
    let mut adc1 = adc::Adc::adc1(dp.ADC1, clocks);
";
        let (out, buses) = promote_bus_handles(calls);

        let names: Vec<(&str, &str)> = buses
            .iter()
            .map(|b| (b.binding.as_str(), b.ty.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("serial1_gps", "pins::configs::usart1::Handle"),
                ("spi1", "pins::configs::spi1::Handle"),
                ("i2c2_imu", "pins::configs::i2c2::Handle"),
                ("can", "pins::configs::can1::Handle"),
            ],
            "{out}"
        );
        // The underscore is gone and nothing else about the line moved.
        assert!(
            out.contains("    let spi1 = pins::configs::spi1::init(dp.SPI1, ("),
            "{out}"
        );
        assert!(
            out.contains("    let i2c2_imu = pins::configs::i2c2::init("),
            "{out}"
        );
        // No `mut`: the handle is only moved into the resource, and RTIC hands
        // tasks a `&mut` to it anyway.
        assert!(!out.contains("let mut serial1_gps"), "{out}");
        // A non-bus line is passed through untouched.
        assert!(
            out.contains("    let mut adc1 = adc::Adc::adc1(dp.ADC1, clocks);"),
            "{out}"
        );
    }

    fn input(name: &str, irq: Option<Edge>) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::GpioInput;
        p.irq = irq;
        p
    }

    /// An input WITHOUT an edge is a polled input, and must not become a task.
    #[test]
    fn only_pins_with_an_edge_become_interrupts() {
        let a = input("PA0", Some(Edge::Rising));
        let b = input("PA1", None);
        let pins: Vec<&Pin> = vec![&a, &b];
        let irqs = irq_pins(&pins);
        assert_eq!(irqs.len(), 1);
        assert_eq!(irqs[0].name, "PA0");
        assert_eq!(irqs[0].edge, Edge::Rising);
    }

    /// Two tasks bound to one vector is a compile error from the RTIC macro, so
    /// pins sharing a vector MUST end up in a single task.
    #[test]
    fn pins_sharing_a_vector_produce_one_task() {
        let a = input("PA6", Some(Edge::Rising));
        let b = input("PB7", Some(Edge::Falling));
        let c = input("PC13", Some(Edge::Both));
        let pins: Vec<&Pin> = vec![&a, &b, &c];
        let irqs = irq_pins(&pins);
        let groups = by_vector(&irqs);
        assert_eq!(groups.len(), 2, "{groups:?}");
        assert_eq!(groups[0].0, "EXTI9_5");
        assert_eq!(groups[0].1.len(), 2);
        assert_eq!(groups[1].0, "EXTI15_10");

        let code = tasks(&irqs);
        assert_eq!(code.matches("binds = EXTI9_5").count(), 1);
        assert_eq!(code.matches("binds = EXTI15_10").count(), 1);
        // The shared one branches; the lone one does not need to.
        assert!(code.contains("check_interrupt()"));
        assert_eq!(code.matches("check_interrupt()").count(), 2);
    }

    /// Every task clears its pending bit FIRST — without it the handler re-fires
    /// forever, which looks like a hung program rather than a missing line.
    #[test]
    fn every_task_clears_the_pending_bit() {
        let a = input("PA0", Some(Edge::Rising));
        let b = input("PA6", Some(Edge::Rising));
        let pins: Vec<&Pin> = vec![&a, &b];
        let code = tasks(&irq_pins(&pins));
        assert_eq!(code.matches("clear_interrupt_pending_bit()").count(), 2);
        for block in code.split("#[task(").skip(1) {
            let body = block.split_once('{').map(|x| x.1).unwrap_or("");
            let clear = body
                .find("clear_interrupt_pending_bit")
                .unwrap_or(usize::MAX);
            let handler = body.find("handler code here").unwrap_or(0);
            assert!(
                clear < handler,
                "pending bit cleared after the handler:\n{body}"
            );
        }
    }

    /// RTIC enables the vector itself; an `unmask` here would race its setup.
    #[test]
    fn nothing_touches_the_nvic() {
        let a = input("PA0", Some(Edge::Rising));
        let pins: Vec<&Pin> = vec![&a];
        let code = make_generated_section(
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
        assert!(!code.contains("unmask"), "{code}");
        assert!(!code.contains("NVIC"), "{code}");
    }

    /// Print a full generated app for eyeballing:
    /// `cargo test -- --ignored --nocapture rtic_sample`.
    ///
    /// Ignored, not asserted: the value is in READING it. Generated code is the
    /// kind of thing that passes every substring check and still looks wrong.
    #[test]
    #[ignore = "prints a sample; run with --ignored"]
    fn rtic_sample() {
        let mut btn = Pin::new(1, "PA0");
        btn.selected_function = PinFunction::GpioInput;
        btn.custom_label = "button".into();
        btn.irq = Some(Edge::Falling);
        let mut b = Pin::new(2, "PB6");
        b.selected_function = PinFunction::GpioInput;
        b.irq = Some(Edge::Rising);
        let mut c = Pin::new(3, "PB7");
        c.selected_function = PinFunction::GpioInput;
        c.irq = Some(Edge::Both);
        let mut led = Pin::new(4, "PC13");
        led.selected_function = PinFunction::GpioOutput;
        led.custom_label = "led".into();
        let pins: Vec<&Pin> = vec![&btn, &b, &c, &led];
        println!(
            "{}",
            make_generated_section(
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
            )
        );
    }

    /// Write a COMPLETE F103 RTIC project to `$RTIC_OUT`, for a real
    /// `cargo check`:
    ///
    /// ```text
    /// RTIC_OUT=/tmp/rtic cargo test -- --ignored rtic_write_project
    /// cd /tmp/rtic && cargo check --target thumbv7m-none-eabi
    /// ```
    ///
    /// `RTIC_RUNTIME=blocking` / `=native` writes the same pin set on the other
    /// runtimes, which share these config files but not their `init`.
    ///
    /// Ignored: it writes to the filesystem and only means anything next to a
    /// toolchain. It exists because every assertion in this file checks the
    /// SHAPE of the output — only rustc checks whether it is Rust.
    #[test]
    #[ignore = "writes a project to $RTIC_OUT; run with --ignored"]
    fn rtic_write_project() {
        use crate::panels::mcu_module::project_gen;
        use crate::panels::mcu_module::registry;

        let Ok(out) = std::env::var("RTIC_OUT") else {
            panic!("set RTIC_OUT to the directory to write the project into");
        };
        let dest = std::path::PathBuf::from(&out);

        let def = registry::load_registry()
            .into_iter()
            .find(|d| d.family == "stm32f1")
            .expect("no stm32f1 definition in the registry");
        let mut mcu = def.build_mcu();
        // `RTIC_RUNTIME=blocking|native` writes the same project on another
        // runtime instead — the config files' `init` differs per runtime, so the
        // other two want a real `cargo check` of their own.
        mcu.runtime = match std::env::var("RTIC_RUNTIME").as_deref() {
            Ok("blocking") => crate::panels::mcu_module::mcu::Runtime::Blocking,
            Ok("native") => crate::panels::mcu_module::mcu::Runtime::Native,
            _ => crate::panels::mcu_module::mcu::Runtime::Rtic,
        };

        // A button on its own vector, two more sharing EXTI9_5, and an LED.
        for (name, func, edge, label) in [
            ("PA0", PinFunction::GpioInput, Some(Edge::Falling), "button"),
            ("PB6", PinFunction::GpioInput, Some(Edge::Rising), ""),
            ("PB7", PinFunction::GpioInput, Some(Edge::Both), ""),
            ("PC13", PinFunction::GpioOutput, None, "led"),
            // Bus peripherals too: their init goes through
            // `pins::configs::<periph>::init(...)` inside `#[init]`, and each
            // handle has to survive as a `Local` — the SPI/I2C ones only can
            // because their config module names concrete pin types.
            ("PA9", PinFunction::UsartTx(1), None, ""),
            ("PA10", PinFunction::UsartRx(1), None, ""),
            ("PA5", PinFunction::SpiSck(1), None, ""),
            ("PA6", PinFunction::SpiMiso(1), None, ""),
            ("PA7", PinFunction::SpiMosi(1), None, ""),
            ("PB8", PinFunction::I2cScl(1), None, ""),
            ("PB9", PinFunction::I2cSda(1), None, ""),
            // The SECOND instance of each too: those HAL constructors take no
            // AFIO register, which the config template has to know.
            ("PB13", PinFunction::SpiSck(2), None, ""),
            ("PB14", PinFunction::SpiMiso(2), None, ""),
            ("PB15", PinFunction::SpiMosi(2), None, ""),
            ("PB10", PinFunction::I2cScl(2), None, ""),
            ("PB11", PinFunction::I2cSda(2), None, ""),
        ] {
            let num = mcu
                .iter_all_pins()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("no pin {name} on this chip"))
                .number;
            if let Some(p) = mcu.find_pin_mut(num) {
                p.selected_function = func;
                p.irq = edge;
                p.custom_label = label.to_string();
            }
        }

        mcu.reconcile_modules();
        let main_rs = mcu.fresh_main_rs();
        let mut files = project_gen::build_project_files(&def.project, &def.toolchain, &main_rs);
        // The deps the IDE adds on the RTIC runtime (app.rs does this per frame).
        files.cargo_toml =
            project_gen::ensure_rtic_deps(&files.cargo_toml, true, &def.project.target, &[]);
        // The GPIO Portable bridge (`pins/configs/io.rs`) needs the embedded-hal
        // crates — app.rs adds these from `has_cfg("io")` on every frame.
        files.cargo_toml = project_gen::ensure_peripheral_deps(
            &files.cargo_toml,
            false, // CAN
            true,  // USART
            true,  // SPI
            true,  // I2C
            true,  // GPIO
            true,  // nb
            &[],
        );

        // `sync_pin_files` normally writes these two; without them `mod pins;`
        // in the invariant header has nothing to point at.
        let user: Vec<(String, String)> = mcu.pin_tree_files();
        std::fs::create_dir_all(&dest).expect("create dest");
        project_gen::write_project(&dest, &files, &user, &mcu.mcu_config_text(), "")
            .expect("write project");
        println!("wrote {} files to {}", user.len() + 6, dest.display());
    }

    /// The contract of this whole change: adding RTIC must not move a byte of
    /// what the other runtimes emit. `gen_parts` was extracted OUT of the
    /// blocking generator, so this is the test that the extraction was pure.
    #[test]
    fn the_blocking_generator_still_owns_its_output() {
        let mut a = Pin::new(1, "PA0");
        a.selected_function = PinFunction::GpioInput;
        a.irq = Some(Edge::Rising); // honoured off the RTIC path too, differently
        let mut b = Pin::new(2, "PC13");
        b.selected_function = PinFunction::GpioOutput;
        b.custom_label = "led".into();
        let pins: Vec<&Pin> = vec![&a, &b];
        let out = super::super::stm32::make_generated_section(
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
        // The bare-metal shape, unchanged: an #[entry] fn main with the init
        // inline and no RTIC anywhere.
        assert!(out.contains("#[entry]"), "{out}");
        assert!(out.contains("fn main() -> ! {"), "{out}");
        // `mut` here because the armed pin below needs `&mut dp.EXTI`; without
        // one the binding stays immutable.
        assert!(
            out.contains("let mut dp = pac::Peripherals::take().unwrap();"),
            "{out}"
        );
        assert!(
            !out.contains("rtic"),
            "RTIC leaked into the blocking output:
{out}"
        );
        assert!(!out.contains("#[init]"), "{out}");
        // The same edge is honoured here, but as a bare-metal `#[interrupt]`
        // over a static — NOT as an RTIC hardware task. Both arm the line with
        // the same HAL sequence; only where the pin then LIVES differs.
        assert!(
            out.contains("trigger_on_edge(&mut dp.EXTI, Edge::Rising)"),
            "{out}"
        );
        assert!(out.contains("#[interrupt]"), "{out}");
        assert!(out.contains("fn EXTI0() {"), "{out}");
        assert!(!out.contains("#[task("), "{out}");
    }

    /// The init sequence is the SAME sequence, just relocated — same clock
    /// chain, same bindings, same config calls.
    #[test]
    fn rtic_reuses_the_blocking_init_verbatim() {
        let mut b = Pin::new(2, "PC13");
        b.selected_function = PinFunction::GpioOutput;
        b.custom_label = "led".into();
        let pins: Vec<&Pin> = vec![&b];
        let blocking = super::super::stm32::make_generated_section(
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
        let rtic = make_generated_section(
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
        // The pin binding is identical text in both, modulo indentation and the
        // `mut`: blocking writes through the pin (`let mut`), RTIC moves it into
        // a `Local` (`let`). Everything to the right of the name is the same.
        let binding = "pc13_out_led = gpioc.pc13.into_push_pull_output";
        assert!(blocking.contains(binding), "{blocking}");
        assert!(rtic.contains(binding), "{rtic}");
        assert!(rtic.contains("#[init]"), "{rtic}");
        assert!(!rtic.contains("#[entry]"), "{rtic}");
    }

    /// The shape RTIC 2 requires: an app module with all four items.
    #[test]
    fn the_skeleton_has_the_rtic_items() {
        let pins: Vec<&Pin> = Vec::new();
        let code = make_generated_section(
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
        for item in [
            "#[rtic::app(device = stm32f1xx_hal::pac",
            "#[shared]",
            "#[local]",
            "#[init]",
            "#[idle]",
            "-> (Shared, Local)",
            "systick_monotonic!(Mono, 1_000);",
        ] {
            assert!(code.contains(item), "missing {item}:\n{code}");
        }
        // No `fn main` — RTIC generates it.
        assert!(!code.contains("fn main"), "{code}");
    }
}
