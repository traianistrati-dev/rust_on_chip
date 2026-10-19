//! Armed inputs: the EXTI line plan, the tasks that wait on an edge and their spawns.

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
pub(super) fn exti_line(singleton: &str) -> Option<u8> {
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
pub(super) fn exti_vector(vectors: &[String], n: u8) -> Option<&str> {
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
pub(super) fn exti_plan(pins: &[&Pin], vectors: &[String]) -> (Vec<ExtiPin>, String) {
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
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: EXTI line {line} is already taken by
                     // {}. One pad per line — the channel is a peripheral, and embassy hands
                     // it to exactly one.
",
                prev.singleton
            ));
            continue;
        }
        let Some(vector) = exti_vector(vectors, line) else {
            notes.push_str(&format!(
                "    // {singleton} is NOT on an interrupt: this chip's vector list names none for
                     // EXTI line {line}, so there is nothing to bind the handler to.
"
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

/// One `#[embassy_executor::task]` per armed input, placed above the entry —
/// a task cannot live inside `async fn main`.
///
/// The task OWNS its pin: `wait_for_*` takes `&mut self`, so the pin cannot also
/// be read from `main` without a mutex.
pub fn exti_tasks(armed: &[ExtiPin]) -> String {
    let mut out = String::new();
    for e in armed {
        out.push_str(&format!(
            "/// {sing} — wakes on a {label} edge. The task owns the pin.\n\
             #[embassy_executor::task]\n\
             async fn {b}_irq(mut pin: ExtiInput<'static, Async>) {{\n\
             \x20   loop {{\n\
             \x20       pin.{wait}().await;\n\
             \x20       // {sing} {label} edge: your handler code here.\n\
             \x20   }}\n\
             }}\n\n",
            sing = e.singleton,
            label = e.edge.label().to_ascii_lowercase(),
            b = e.binding,
            wait = exti_wait(e.edge),
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
        let pins = vec![armed("PA5", Edge::Rising), armed("PB5", Edge::Falling)];
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
        let pins = vec![armed("PA5", Edge::Rising)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let (plan, notes) = exti_plan(&refs, &vectors(&["EXTI0", "EXTI1"]));
        assert!(plan.is_empty(), "{plan:?}");
        assert!(notes.contains("names none for"), "{notes}");
    }

    /// An input with no edge is one you poll: no plan, no task, no feature.
    #[test]
    fn an_unarmed_input_is_left_alone() {
        let mut p = Pin::new(1, "PA5");
        p.selected_function = PinFunction::GpioInput;
        let pins = vec![p];
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
            let pins = vec![armed("PA5", edge)];
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
