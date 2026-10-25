//! Armed inputs: the EXTI line plan, the tasks that wait on an edge and their spawns.

use super::super::common::{EdgeHook, edge_call, edge_hook_name, hook_binding};
use super::*;

/// One input the user armed with an edge, resolved to its EXTI line + vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtiPin {
    /// The `let` binding in `main.rs` (`pb5_in_button`).
    pub binding: String,
    /// The embassy singleton (`PB5`).
    pub singleton: String,
    /// EXTI line — the pin NUMBER, which is what makes it a limited resource.
    pub line: u8,
    pub edge: Edge,
    /// The NVIC vector serving that line (`EXTI9_5`).
    pub vector: String,
}

/// The EXTI line a pad sits on: `PB5` → 5. Lines are numbered by PIN, not by
/// port, which is the whole reason two pads can collide on one.
pub(crate) fn exti_line(singleton: &str) -> Option<u8> {
    let rest = singleton.strip_prefix(['P', 'p'])?;
    let mut cs = rest.chars();
    if !cs.next()?.is_ascii_alphabetic() {
        return None;
    }
    let digits: String = cs.take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|n| *n <= 15)
}

/// The vector serving EXTI line `n`.
///
/// [`nvic::vector_for`] is not enough here. It reads `EXTI15_10` as covering 15
/// and 10 and nothing between — right for a peripheral list (`TIM6_DAC` really
/// is just those two) and wrong for an EXTI name, which states a RANGE: lines
/// 11 to 14 are on that vector too. So the numbers in the name are read as
/// bounds, and the NARROWEST vector containing the line wins (a G0 lists both
/// `EXTI0_1` and `EXTI4_15`; line 1 belongs to the first).
pub(crate) fn exti_vector(vectors: &[String], n: u8) -> Option<&str> {
    let bounds = |v: &str| -> Option<(u8, u8)> {
        let rest = v.strip_prefix("EXTI")?;
        let nums: Vec<u8> = rest
            .split('_')
            .filter_map(|seg| {
                let d: String = seg.chars().take_while(char::is_ascii_digit).collect();
                d.parse().ok()
            })
            .collect();
        let lo = *nums.iter().min()?;
        let hi = *nums.iter().max()?;
        Some((lo, hi))
    };
    vectors
        .iter()
        .filter_map(|v| bounds(v).map(|b| (v, b)))
        .filter(|(_, (lo, hi))| *lo <= n && n <= *hi)
        .min_by_key(|(_, (lo, hi))| hi - lo)
        .map(|(v, _)| v.as_str())
}

/// The armed inputs, resolved — plus a comment line per input that could NOT be
/// given an EXTI, which is the honest half of this.
///
/// Two ways to lose: the line is already taken (PA5 and PB5 are both line 5, and
/// embassy hands the channel to exactly one), or the chip's vector list does not
/// name a vector for it. Both are stated in `main.rs` rather than dropped.
pub(crate) fn exti_plan(pins: &[&Pin], vectors: &[String]) -> (Vec<ExtiPin>, String) {
    let mut out: Vec<ExtiPin> = Vec::new();
    let mut notes = String::new();
    // Sorted by singleton so the winner of a line clash is stable across runs —
    // an arbitrary but FIXED choice beats one that moves with pin order.
    let mut armed: Vec<&&Pin> = pins
        .iter()
        .filter(|p| !p.reserved && p.selected_function == PinFunction::GpioInput)
        .filter(|p| p.irq.is_some())
        .collect();
    armed.sort_by_key(|p| p.gpio().to_owned());

    for p in armed {
        let singleton = p.gpio().to_owned();
        let edge = p.irq.expect("filtered above");
        let Some(line) = exti_line(&singleton) else {
            continue; // not a P<port><n> pad — no EXTI to speak of
        };
        if let Some(prev) = out.iter().find(|e| e.line == line) {
            // `\n    //` escapes, not raw newlines: a line break inside the
            // literal carries this file's indentation into main.rs.
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: EXTI line {line} is already taken by\n    // {}. One pad per line — the channel is a peripheral, and embassy hands\n    // it to exactly one.\n",
                prev.singleton
            ));
            continue;
        }
        let Some(vector) = exti_vector(vectors, line) else {
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: this chip's vector list names none for\n    // EXTI line {line}, so there is nothing to bind the handler to.\n"
            ));
            continue;
        };
        out.push(ExtiPin {
            binding: pin_binding(
                &singleton.to_ascii_lowercase(),
                &p.selected_function,
                &p.custom_label,
            ),
            singleton,
            line,
            edge,
            vector: vector.to_owned(),
        });
    }
    (out, notes)
}

/// The `Input` method that waits for `edge`.
pub(super) fn exti_wait(edge: Edge) -> &'static str {
    match edge {
        Edge::Rising => "wait_for_rising_edge",
        Edge::Falling => "wait_for_falling_edge",
        Edge::Both => "wait_for_any_edge",
    }
}

/// The user's hook an armed input's task calls: named after the pad alone
/// (`on_pb5_in_edge`), the same name the F1's Blocking and RTIC handlers call.
fn exti_hook(e: &ExtiPin) -> String {
    edge_hook_name(&hook_binding(&e.singleton.to_ascii_lowercase()))
}

/// The hooks the tasks call, one per input that GOT a line - a refused pad has
/// no task, so no hook - in the order the tasks are emitted.
pub fn exti_hooks(armed: &[ExtiPin]) -> Vec<EdgeHook> {
    armed
        .iter()
        .map(|e| EdgeHook {
            name: exti_hook(e),
            what: e.singleton.clone(),
            caller: format!("{}_irq", e.binding),
            is_async: true,
        })
        .collect()
}

/// One `#[embassy_executor::task]` per armed input, placed above the entry —
/// a task cannot live inside `async fn main`.
///
/// The task OWNS its pin: `wait_for_*` takes `&mut self`, so the pin cannot also
/// be read from `main` without a mutex. The body is NOT here: the task is
/// rebuilt on every regeneration, so it calls a hook seeded once below the
/// tail, with the level after the edge.
pub fn exti_tasks(armed: &[ExtiPin]) -> String {
    let mut out = String::new();
    for e in armed {
        let hook = exti_hook(e);
        out.push_str(&format!(
            "/// {sing} — wakes on {phrase}. The task owns the pin;\n\
             /// `{hook}` below `main` is yours.\n\
             #[embassy_executor::task]\n\
             async fn {b}_irq(mut pin: ExtiInput<'static, Async>) {{\n\
             \x20   loop {{\n\
             \x20       pin.{wait}().await;\n\
             {call}\
             \x20   }}\n\
             }}\n\n",
            sing = e.singleton,
            phrase = e.edge.phrase(),
            b = e.binding,
            wait = exti_wait(e.edge),
            call = edge_call(&hook, "pin.is_high()", true, "        "),
        ));
    }
    out
}

/// The `spawner.spawn(...)` lines for the armed inputs.
///
/// NOTE the shape: on this stack (embassy-executor 0.9 / macros 0.7) the TASK
/// returns a `SpawnToken` and `spawn` returns a `Result`. The ESP backend runs
/// executor 0.10, where it is exactly the other way round — see
/// `codegen_esp::irq_inputs`. Do not copy one call into the other.
pub fn exti_spawns(armed: &[ExtiPin]) -> String {
    if armed.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "
    // ── GPIO interrupts ──
",
    );
    for e in armed {
        out.push_str(&format!(
            "    _spawner.spawn({b}_irq({b})).ok();\n",
            b = e.binding
        ));
    }
    out
}

// ── Blocking: `ExtiInput<Blocking>` + `#[interrupt]` ─────────────────────────
//
// The same plan, a different owner. With no executor the pin is parked in a
// static and the vector's handler asks it - the F1's Blocking shape, on
// embassy's driver. `embassy_stm32::init` already unmasks every EXTI vector in
// the NVIC once the `exti` feature is on, and 0.6 binds no handler of its own
// unless `bind_interrupts!` says so, so the `#[interrupt]` below owns the
// vector and nothing is unmasked here.

/// The `TriggerEdge` `new_blocking` arms the line with.
pub(crate) fn exti_trigger(edge: Edge) -> &'static str {
    match edge {
        Edge::Rising => "TriggerEdge::Rising",
        Edge::Falling => "TriggerEdge::Falling",
        Edge::Both => "TriggerEdge::Any",
    }
}

/// The static an armed input is parked in: its binding, upper-cased.
pub(crate) fn exti_static(binding: &str) -> String {
    binding.to_ascii_uppercase()
}

/// The `use` lines the Blocking items and the arming need.
///
/// `cortex_m::interrupt::Mutex`, not `critical_section::Mutex`: `cortex-m` is
/// already a dependency of every Blocking project here, with
/// `critical-section-single-core`, so this adds nothing to Cargo.toml.
/// `embassy_stm32::interrupt` brings both the `#[interrupt]` attribute and the
/// `EXTIn` names it checks the handler's name against.
pub(crate) const EXTI_BLOCKING_USES: &str = "use embassy_stm32::exti::{ExtiInput, TriggerEdge};\n\
     use embassy_stm32::interrupt;\n\
     use embassy_stm32::mode::Blocking;\n\
     use core::cell::RefCell;\n\
     use cortex_m::interrupt::Mutex;\n";

/// The hooks the Blocking handlers call: the names the Async tasks call, so a
/// runtime switch finds the hook it seeded, but plain `fn`s - an interrupt
/// cannot await.
pub fn exti_blocking_hooks(armed: &[ExtiPin]) -> Vec<EdgeHook> {
    armed
        .iter()
        .map(|e| EdgeHook {
            name: exti_hook(e),
            what: e.singleton.clone(),
            caller: e.vector.clone(),
            is_async: false,
        })
        .collect()
}

/// The vectors the armed inputs land on, each with its pins, in the order of
/// each vector's lowest line - so the handlers read in the order of the pins.
fn exti_by_vector(armed: &[ExtiPin]) -> Vec<(&str, Vec<&ExtiPin>)> {
    let mut groups: Vec<(&str, Vec<&ExtiPin>)> = Vec::new();
    for e in armed {
        match groups.iter_mut().find(|(v, _)| *v == e.vector) {
            Some((_, ps)) => ps.push(e),
            None => groups.push((e.vector.as_str(), vec![e])),
        }
    }
    for (_, ps) in &mut groups {
        ps.sort_by_key(|e| e.line);
    }
    groups.sort_by_key(|(_, ps)| ps.first().map_or(u8::MAX, |e| e.line));
    groups
}

/// The module-level items: one static per armed input, one `#[interrupt]` per
/// VECTOR. Statics because a handler takes no arguments and cannot borrow a
/// local; one handler per vector because that is what the NVIC gives -
/// `EXTI9_5` carries five lines, so its handler asks each pin.
///
/// The body is NOT here: the handler is rebuilt on every regeneration, so it
/// calls a hook seeded once below the tail - AFTER the critical section, with
/// the level it read, so the hook may take one itself. ONE local, a tuple on a
/// shared vector: one per pin would differ by a letter, which
/// `clippy::similar_names` denies under Strict, and would start with the pin's
/// name, which is how the IDE finds a pin's binding in the block.
pub fn exti_blocking_items(armed: &[ExtiPin]) -> String {
    let mut out = String::new();
    for e in armed {
        out.push_str(&format!(
            "/// {} — parked here so its interrupt handler can reach it.\n",
            e.singleton
        ));
        out.push_str(&format!(
            "static {}: Mutex<RefCell<Option<ExtiInput<'static, Blocking>>>> = Mutex::new(RefCell::new(None));\n",
            exti_static(&e.binding)
        ));
    }
    if !armed.is_empty() {
        out.push('\n');
    }
    for (vector, group) in exti_by_vector(armed) {
        if group.len() == 1 {
            out.push_str(&format!(
                "/// {} (EXTI line {}). `{}` below `main` is yours, a plain `fn`.\n",
                group[0].singleton,
                group[0].line,
                exti_hook(group[0])
            ));
        } else {
            out.push_str(&format!(
                "/// `{vector}` is shared by {} lines — each pin is asked in turn, and\n/// each pin's hook below `main` is yours, a plain `fn`.\n",
                group.len()
            ));
        }
        out.push_str("#[interrupt]\n");
        out.push_str(&format!("fn {vector}() {{\n"));
        let slot = |i: usize| {
            if group.len() == 1 {
                "fired".to_owned()
            } else {
                format!("fired.{i}")
            }
        };
        out.push_str(&if group.len() == 1 {
            "    let mut fired = None;\n".to_owned()
        } else {
            format!(
                "    let mut fired = ({});\n",
                vec!["None"; group.len()].join(", ")
            )
        });
        out.push_str("    cortex_m::interrupt::free(|cs| {\n");
        for (i, e) in group.iter().enumerate() {
            out.push_str(&format!(
                "        if let Some(pin) = {}.borrow(cs).borrow_mut().as_mut() {{\n",
                exti_static(&e.binding)
            ));
            // A private vector still asks: one register read, and it keeps the
            // two shapes identical to read.
            out.push_str("            if pin.is_pending() {\n");
            out.push_str("                // Cleared FIRST: leave the pending bit set and the\n");
            out.push_str("                // handler re-enters the moment it returns.\n");
            out.push_str("                pin.clear_pending();\n");
            out.push_str(&format!(
                "                {} = Some(pin.is_high());\n",
                slot(i)
            ));
            out.push_str("            }\n");
            out.push_str("        }\n");
        }
        out.push_str("    });\n");
        for (i, e) in group.iter().enumerate() {
            out.push_str(&format!(
                "    if let Some(high) = {} {{\n{}    }}\n",
                slot(i),
                edge_call(&exti_hook(e), "high", false, "        ")
            ));
        }
        out.push_str("}\n\n");
    }
    out
}

/// One armed input's lines in `main`: built AND parked in one critical section.
///
/// `new_blocking` arms the line the moment it returns, and the NVIC is already
/// open. An edge that came before the static held the pin would reach a handler
/// that cannot clear it - the pending bit stays, the handler re-enters for good
/// and `main` never runs again (a button bouncing at power-up is enough). Inside
/// `free` the edge waits until the pin is there to clear.
///
/// The `let` keeps the trailing label, so [`super::super::parse_main_rs`]
/// reads the pin back like any other input.
pub(crate) fn exti_blocking_binding(
    var: &str,
    singleton: &str,
    line: u8,
    pull: &str,
    edge: Edge,
    label: &str,
) -> String {
    format!(
        "    // {singleton} is built and parked in ONE critical section: an edge that comes\n    // early waits until its handler has the pin to clear.\n    cortex_m::interrupt::free(|cs| {{\n        let {var} = ExtiInput::new_blocking(p.{singleton}, p.EXTI{line}, {pull}, {trigger}); // {label}\n        *{up}.borrow(cs).borrow_mut() = Some({var});\n    }});",
        trigger = exti_trigger(edge),
        up = exti_static(var),
    )
}

#[cfg(test)]
#[cfg(test)]
mod exti_tests {
    use super::*;

    fn vectors(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| (*s).to_owned()).collect()
    }

    fn armed(name: &str, edge: Edge) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::GpioInput;
        p.irq = Some(edge);
        p
    }

    /// The task calls the user's hook - named after the pad, never its label
    /// - and a pad refused its line has no task, so no hook.
    #[test]
    fn the_task_calls_the_hook_and_a_refused_pad_has_none() {
        let a = armed("PA5", Edge::Both);
        let mut b = armed("PB5", Edge::Rising);
        b.custom_label = "btn".into();
        let pins = [&a, &b];
        let (plan, notes) = exti_plan(&pins, &vectors(&["EXTI9_5"]));
        assert_eq!(plan.len(), 1, "{notes}");
        let hooks = exti_hooks(&plan);
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "on_pa5_in_edge");
        assert!(hooks[0].is_async);
        let tasks = exti_tasks(&plan);
        assert!(
            tasks.contains("        on_pa5_in_edge(pin.is_high()).await;\n"),
            "{tasks}"
        );
        assert!(tasks.contains("wakes on either edge"), "{tasks}");
        assert!(!tasks.contains("code here"), "{tasks}");
    }

    /// An EXTI vector name states a RANGE, which is what makes `nvic::covered`
    /// the wrong reader for it: `EXTI15_10` carries lines 11..14 too.
    #[test]
    fn a_vector_name_is_read_as_a_range() {
        let v = vectors(&[
            "EXTI0",
            "EXTI1",
            "EXTI2",
            "EXTI3",
            "EXTI4",
            "EXTI9_5",
            "EXTI15_10",
        ]);
        assert_eq!(exti_vector(&v, 0), Some("EXTI0"));
        assert_eq!(exti_vector(&v, 5), Some("EXTI9_5"));
        // The ones in the middle — the whole point.
        assert_eq!(exti_vector(&v, 7), Some("EXTI9_5"));
        assert_eq!(exti_vector(&v, 12), Some("EXTI15_10"));
        assert_eq!(exti_vector(&v, 15), Some("EXTI15_10"));
        // A line the chip does not carry has no vector.
        assert_eq!(exti_vector(&v, 16), None);

        // A G0-style list: overlapping ranges, and the NARROWEST wins.
        let g0 = vectors(&["EXTI0_1", "EXTI2_3", "EXTI4_15"]);
        assert_eq!(exti_vector(&g0, 1), Some("EXTI0_1"));
        assert_eq!(exti_vector(&g0, 3), Some("EXTI2_3"));
        assert_eq!(exti_vector(&g0, 9), Some("EXTI4_15"));
    }

    /// The line is the PIN NUMBER, and that is what makes it scarce.
    #[test]
    fn the_line_is_the_pin_number() {
        assert_eq!(exti_line("PA0"), Some(0));
        assert_eq!(exti_line("PB15"), Some(15));
        assert_eq!(exti_line("PC7"), Some(7));
        // Not a P<port><n> pad — nothing to sit on.
        assert_eq!(exti_line("VDD"), None);
        assert_eq!(exti_line("PB16"), None);
    }

    /// Two pads on one line: one gets the channel, the other is refused with the
    /// reason — embassy hands the EXTI channel to exactly one.
    #[test]
    fn one_pad_per_line_and_the_other_is_told_why() {
        let pins = [armed("PA5", Edge::Rising), armed("PB5", Edge::Falling)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI9_5"]));

        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(plan[0].singleton, "PA5");
        assert_eq!(plan[0].line, 5);
        assert_eq!(plan[0].vector, "EXTI9_5");
        assert!(notes.contains("PB5 is NOT on an interrupt"), "{notes}");
        assert!(notes.contains("already taken by"), "{notes}");
    }

    /// No vector for the line → no binding to hang the handler on, said out loud.
    #[test]
    fn a_line_with_no_vector_is_refused() {
        let pins = [armed("PA5", Edge::Rising)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI0", "EXTI1"]));
        assert!(plan.is_empty(), "{plan:?}");
        assert!(notes.contains("names none for"), "{notes}");
    }

    /// Both refusals are comment lines at `main`'s own four spaces. They were
    /// raw newlines inside the literal, which put this file's 21 columns of
    /// indentation in front of every continuation line of the generated file.
    #[test]
    fn a_refusal_note_is_indented_like_main() {
        let pins = [armed("PA5", Edge::Rising), armed("PB5", Edge::Falling)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (_, clash) = exti_plan(&refs, &vectors(&["EXTI9_5"]));
        let pins = [armed("PA3", Edge::Rising)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (_, no_vector) = exti_plan(&refs, &vectors(&["EXTI0"]));
        for notes in [clash, no_vector] {
            assert!(notes.lines().count() >= 2, "{notes}");
            for line in notes.lines() {
                assert!(
                    line.starts_with("    // ") && !line.starts_with("     "),
                    "`{line}` in:\n{notes}"
                );
            }
        }
    }

    /// An input with no edge is one you poll: no plan, no task, no feature.
    #[test]
    fn an_unarmed_input_is_left_alone() {
        let mut p = Pin::new(1, "PA5");
        p.selected_function = PinFunction::GpioInput;
        let pins = [p];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI9_5"]));
        assert!(plan.is_empty());
        assert!(notes.is_empty(), "{notes}");
    }

    /// Each edge picks its own await, and the task owns the pin.
    #[test]
    fn the_task_awaits_the_chosen_edge() {
        for (edge, want) in [
            (Edge::Rising, "wait_for_rising_edge"),
            (Edge::Falling, "wait_for_falling_edge"),
            (Edge::Both, "wait_for_any_edge"),
        ] {
            let pins = [armed("PA5", edge)];
            let refs: Vec<&Pin> = pins.iter().collect();
            let (plan, _) = exti_plan(&refs, &vectors(&["EXTI9_5"]));
            let tasks = exti_tasks(&plan);
            assert!(tasks.contains(&format!("pin.{want}().await;")), "{tasks}");
            assert!(
                tasks.contains("async fn pa5_in_irq(mut pin: ExtiInput<'static, Async>)"),
                "{tasks}"
            );
            // The spawn shape is this stack's, NOT the ESP one: here the task
            // returns the token and `spawn` returns the Result.
            assert!(
                exti_spawns(&plan).contains("_spawner.spawn(pa5_in_irq(pa5_in)).ok();"),
                "{}",
                exti_spawns(&plan)
            );
        }
    }
}
