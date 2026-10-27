//! Timers in PWM mode: `pwm{N}.rs` (`SimplePwm` / `ComplementaryPwm`) and their pads.

use super::*;

// ── PWM config file (embassy SimplePwm / ComplementaryPwm, both runtimes) ─────

/// `src/pins/configs/pwm{N}.rs`: embassy's `SimplePwm` over ONE timer.
///
/// The whole file is built per timer rather than per channel because the timer
/// is what owns the frequency — one prescaler and one reload value serve all
/// four channels, so a per-channel frequency could not be honoured. Only the
/// channels actually wired on the canvas become parameters; the rest are `None`.
pub(super) const PWM_TMPL: &str = include_str!("../templates/embassy_stm32/pwm.rs.tmpl");

/// The advanced-control timers, the only ones embassy's `ComplementaryPwm`
/// accepts: its bound is `AdvancedInstance4Channel`. TIM15/16/17 carry a CH1N
/// pad on real silicon but are 1- and 2-channel advanced timers, which that
/// driver does not cover.
pub fn is_advanced_timer(n: u8) -> bool {
    matches!(n, 1 | 8 | 20)
}

/// Render [`PWM_TMPL`] for timer `n` from its wiring and its module
/// config.
///
/// Two shapes come out of one template. With plain channels only it is
/// `SimplePwm`, exactly as before. As soon as one CHxN pad is wired it becomes
/// `ComplementaryPwm`, whose API is NOT the same: duty is a raw compare value
/// against `get_max_duty()` rather than a ratio, channels are addressed by the
/// `Channel` enum rather than `.chN()`, and there is no output-compare-mode
/// setter at all.
pub fn pwm_config_file(n: u8, cfg: &TimerModuleConfig, wiring: &PwmWiring, handle: &str) -> String {
    let complementary = wiring.needs_complementary();
    let param_list = wiring.params();
    let has_plain = !wiring.chans.is_empty();

    let mut duty_consts = String::new();
    let mut params = String::new();
    let mut ch_args = String::new();
    let mut enables = String::new();
    // Only the `low_level` items the emitted code actually names — an unused
    // import in a generated file is a warning the user cannot fix.
    let mut low_level: Vec<String> = vec!["CountingMode".to_owned()];
    let mut timer_items: Vec<String> = Vec::new();
    let mut gpio_items: Vec<String> = vec!["OutputType".to_owned()];
    let mut prelude = String::new();

    if complementary {
        duty_consts.push_str(&format!(
            "pub const DEAD_TIME: u16 = {}; // timer ticks, same scale as the duty compare value\n",
            cfg.dead_time
        ));
    }
    for ch in wiring.active_channels() {
        let duty = cfg.duty_x100_of(ch);
        duty_consts.push_str(&format!(
            "pub const DUTY_CH{ch}: u32 = {duty}; // {} %, in hundredths (0..=10_000)\n",
            duty_percent_str(duty)
        ));
    }

    for (ch, name, _, comp) in &param_list {
        if name.starts_with("bkin") {
            // The break pad is bounded by `BreakInputPin`, which embassy
            // declares but no driver consumes — see `prelude` below.
            timer_items.push("BreakInputPin".to_owned());
            timer_items.push(format!("BkIn{ch}"));
            params.push_str(&format!(
                "    {name}: Peri<'d, impl BreakInputPin<peripherals::TIM{n}, BkIn{ch}>>,\n"
            ));
            continue;
        }
        let bound = if *comp {
            "TimerComplementaryPin"
        } else {
            "TimerPin"
        };
        timer_items.push(format!("Ch{ch}"));
        timer_items.push(bound.to_owned());
        params.push_str(&format!(
            "    {name}: Peri<'d, impl {bound}<peripherals::TIM{n}, Ch{ch}>>,\n"
        ));
    }

    // The slots, in the order the driver declares them: ch1..ch4 for
    // `SimplePwm`, ch1/ch1n/ch2/ch2n/… for `ComplementaryPwm`. A pad that is
    // not wired is still a slot.
    for ch in 1..=4u8 {
        let out = cfg.channel_of(ch);
        if wiring.chans.iter().any(|(c, _)| *c == ch) {
            ch_args.push_str(&format!(
                "        Some(PwmPin::new(ch{ch}, OutputType::{})),\n",
                out.output.embassy()
            ));
        } else {
            ch_args.push_str("        None,\n");
        }
        if complementary {
            if wiring.comp.iter().any(|(c, _)| *c == ch) {
                ch_args.push_str(&format!(
                    "        Some(ComplementaryPwmPin::new(ch{ch}n, OutputType::{})),\n",
                    out.output.embassy()
                ));
            } else {
                ch_args.push_str("        None,\n");
            }
        }
    }

    if complementary {
        // Dead time first: it has to be in the register before an output can
        // turn on, or the first edges go out with both sides live.
        enables.push_str("    pwm.set_dead_time(DEAD_TIME);\n");
        enables.push_str("    let max = pwm.get_max_duty();\n");
        let mut mode_asked = false;
        for ch in wiring.active_channels() {
            let out = cfg.channel_of(ch);
            enables.push_str(&format!(
                "    pwm.set_duty(Channel::Ch{ch}, max * DUTY_CH{ch} / 10_000);\n"
            ));
            if out.polarity != PwmPolarity::default() {
                low_level.push("OutputPolarity".to_owned());
                // The MAIN side only: inverting both would undo the pairing the
                // complementary output exists for.
                enables.push_str(&format!(
                    "    pwm.set_main_polarity(Channel::Ch{ch}, OutputPolarity::{});\n",
                    out.polarity.embassy()
                ));
            }
            mode_asked |= out.mode != PwmMode::default();
            enables.push_str(&format!("    pwm.enable(Channel::Ch{ch});\n"));
        }
        for (i, _) in &wiring.breaks {
            let b = cfg.break_of(*i);
            let sfx = if *i == 1 {
                String::new()
            } else {
                i.to_string()
            };
            low_level.push("BreakInputPolarity".to_owned());
            low_level.push("FilterValue".to_owned());
            enables.push_str(&format!(
                "    pwm.set_break{sfx}_polarity(BreakInputPolarity::{});\n",
                b.polarity.embassy()
            ));
            enables.push_str(&format!(
                "    pwm.set_break{sfx}_filter(FilterValue::{});\n",
                b.filter_embassy()
            ));
            enables.push_str(&format!("    pwm.set_break{sfx}_input_pin_enable(true);\n"));
            enables.push_str(&format!("    pwm.set_break{sfx}_enable(true);\n"));
        }
        if !wiring.breaks.is_empty() {
            enables.push_str(&format!(
                "    pwm.set_automatic_output_enable({});\n",
                cfg.auto_output_enable
            ));
        }
        if mode_asked {
            // Three pushes, not one `\`-continued literal: rustfmt joins those
            // back into a single source line and the continuation loses its
            // `//`, which lands as a bare statement in the user's file.
            enables.push_str("    // PWM mode 2 is not applied: `ComplementaryPwm` has no\n");
            enables.push_str("    // output-compare-mode setter. The channel polarity above\n");
            enables.push_str("    // gives the same inversion.\n");
        }
        timer_items.push("Channel".to_owned());
    } else {
        for ch in wiring.active_channels() {
            let out = cfg.channel_of(ch);
            enables.push_str(&format!(
                "    pwm.ch{ch}().enable();\n    pwm.ch{ch}().set_duty_cycle_fraction(DUTY_CH{ch}, 10_000);\n"
            ));
            // Only what DIFFERS from the timer's reset state gets a line, so a
            // project that never opened these settings generates what it always did.
            if out.polarity != PwmPolarity::default() {
                low_level.push("OutputPolarity".to_owned());
                enables.push_str(&format!(
                    "    pwm.ch{ch}().set_polarity(OutputPolarity::{});\n",
                    out.polarity.embassy()
                ));
            }
            if out.mode != PwmMode::default() {
                low_level.push("OutputCompareMode".to_owned());
                enables.push_str(&format!(
                    "    pwm.ch{ch}().set_output_compare_mode(OutputCompareMode::{});\n",
                    out.mode.embassy()
                ));
            }
        }
    }

    if !wiring.breaks.is_empty() {
        gpio_items.push("AfType".to_owned());
        gpio_items.push("Flex".to_owned());
        gpio_items.push("Pull".to_owned());
        prelude.push_str("    // The break pads are put into alternate-function mode by hand:\n");
        prelude.push_str("    // embassy declares `BreakInputPin` but no driver takes one, so\n");
        prelude.push_str("    // nothing else does it.\n");
        for (i, _) in &wiring.breaks {
            prelude.push_str(&format!("    let bkin{i}_af = bkin{i}.af_num();\n"));
            prelude.push_str(&format!("    let mut bkin{i} = Flex::new(bkin{i});\n"));
            prelude.push_str(&format!(
                "    bkin{i}.set_as_af_unchecked(bkin{i}_af, AfType::input(Pull::None));\n"
            ));
        }
        prelude.push('\n');
    }

    let use_line = |path: &str, items: &[String]| -> String {
        if items.len() == 1 {
            format!("use embassy_stm32::{path}::{};", items[0])
        } else {
            format!("use embassy_stm32::{path}::{{{}}};", items.join(", "))
        }
    };
    let dedup = |mut v: Vec<String>| -> Vec<String> {
        v.sort();
        v.dedup();
        v
    };

    let low_level_use = use_line("timer::low_level", &dedup(low_level));
    let timer_use = use_line("timer", &dedup(timer_items));
    let driver = if complementary {
        "ComplementaryPwm"
    } else {
        "SimplePwm"
    };
    let driver_use = if complementary {
        let mut l =
            "use embassy_stm32::timer::complementary_pwm::{ComplementaryPwm, ComplementaryPwmPin};"
                .to_owned();
        if has_plain {
            l.push_str("\nuse embassy_stm32::timer::simple_pwm::PwmPin;");
        }
        l
    } else {
        "use embassy_stm32::timer::simple_pwm::{PwmPin, SimplePwm};".to_owned()
    };

    // The pads have to outlive `init`: a dropped `Flex` disconnects its pin,
    // and a disconnected break pad is a fault line nobody is listening to.
    let (break_struct, ret, ret_expr) = if wiring.breaks.is_empty() {
        (
            String::new(),
            format!("{driver}<'d, peripherals::TIM{n}>"),
            "pwm".to_owned(),
        )
    } else {
        let fields: String = wiring
            .breaks
            .iter()
            .map(|(i, _)| format!("    pub bkin{i}: Flex<'d>,\n"))
            .collect();
        let inits: String = wiring
            .breaks
            .iter()
            .map(|(i, _)| format!("bkin{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        (
            format!(
                "\n/// The break pads, handed back so their alternate-function setting outlives\n\
                 /// `init`. Dropping one disconnects the pin, and the fault line goes deaf.\n\
                 pub struct BreakPads<'d> {{\n{fields}}}\n"
            ),
            format!("({driver}<'d, peripherals::TIM{n}>, BreakPads<'d>)"),
            format!("(pwm, BreakPads {{ {inits} }})"),
        )
    };

    let intro = if complementary {
        format!(
            "// `init` returns embassy's `ComplementaryPwm` for TIM{n}. Each channel drives a\n\
             // PAIR of pads — CHx and its inverse CHxN — with DEAD_TIME between the edges, so\n\
             // the two sides of a half-bridge are never on at once. The frequency belongs to\n\
             // the timer; the duty is what each channel owns."
        )
    } else {
        format!(
            "// `init` returns embassy's `SimplePwm` for TIM{n}. Every channel of a timer\n\
             // shares its frequency (one prescaler, one reload value) — the duty cycle is\n\
             // what each channel owns."
        )
    };
    let usage = if complementary {
        format!(
            "// The handle addresses channels by name, and a channel means BOTH its pads:\n\
             //\n\
             //     let max = {handle}.get_max_duty();\n\
             //     {handle}.set_duty(Channel::Ch1, max / 2); // 50 %\n\
             //     {handle}.disable(Channel::Ch1);           // both pads idle\n\
             //\n\
             // DEAD_TIME is in timer ticks on the same scale as the compare value; embassy\n\
             // encodes it into the CKD + DTG fields. Zero means no dead time at all, which a\n\
             // half-bridge will not survive."
        )
    } else {
        format!(
            "// The handle owns every channel; each one is reached by name:\n\
             //\n\
             //     {handle}.ch1().set_duty_cycle_percent(75);\n\
             //     {handle}.ch1().disable();\n\
             //\n\
             // The generated duty is a ratio out of 10_000, so a value like 7.5 % lands\n\
             // exactly. Any other ratio works the same way — a third of the period:\n\
             //\n\
             //     {handle}.ch1().set_duty_cycle_fraction(1, 3);"
        )
    };

    // The same duty trait the F1 backend generates, so switching runtime does
    // not rename the call sites. The channel is part of the METHOD NAME rather
    // than an argument for the same reason it is on F1: there, asking for a
    // channel with no pad PANICS (`PINS::check_used`); here the driver accepts
    // it and silently drives nothing, which is quieter and no more useful.
    let active = wiring.active_channels();
    let duty_trait = if active.is_empty() {
        String::new()
    } else {
        let first = active[0];
        let list = active
            .iter()
            .map(|c| format!("CH{c}"))
            .collect::<Vec<_>>()
            .join("+");
        let mut t = String::new();
        t.push_str("\n/// Set a channel's duty in the same units the `DUTY_*` constants above\n");
        t.push_str(
            "/// use \u{2014} HUNDREDTHS of a percent, so `10_000` is 100 % and `750` is 7.5 %.\n",
        );
        t.push_str("///\n");
        t.push_str("/// A trait rather than an inherent method because the handle is embassy's\n");
        t.push_str("/// own type, which this crate does not own. One method per WIRED channel\n");
        t.push_str(&format!(
            "/// ({list}); a channel this timer has no pad for cannot be named at all.\n"
        ));
        if !wiring.breaks.is_empty() {
            t.push_str("///\n");
            t.push_str(
                "/// The impl is on the DRIVER, not on the tuple `init` returns here \u{2014}\n",
            );
            t.push_str("/// the break pads ride along in it. `main.rs` destructures that tuple,\n");
            t.push_str(&format!("/// so `{handle}` is already the driver.\n"));
        }
        t.push_str("pub trait DutyHandle {\n");
        t.push_str(&format!(
            "    /// Ch{first}, the lowest channel wired to TIM{n}.\n"
        ));
        t.push_str(&format!(
            "    fn set_duty_tim_{n}(&mut self, value: u32);\n"
        ));
        for ch in &active {
            let both = if complementary {
                format!(" and CH{ch}N")
            } else {
                String::new()
            };
            t.push_str(&format!("\n    /// CH{ch}{both}.\n"));
            t.push_str(&format!(
                "    fn set_duty_tim_{n}_ch{ch}(&mut self, value: u32);\n"
            ));
        }
        t.push_str("}\n\n");
        t.push_str(&format!(
            "impl<'d> DutyHandle for {driver}<'d, peripherals::TIM{n}> {{\n"
        ));
        t.push_str(&format!(
            "    fn set_duty_tim_{n}(&mut self, value: u32) {{\n"
        ));
        t.push_str(&format!(
            "        self.set_duty_tim_{n}_ch{first}(value);\n"
        ));
        t.push_str("    }\n");
        for ch in &active {
            t.push_str(&format!(
                "\n    fn set_duty_tim_{n}_ch{ch}(&mut self, value: u32) {{\n"
            ));
            // Each body is copied from the `enables` line above it, which is
            // already proven to compile against that driver's API: the two
            // drivers do NOT share one.
            if complementary {
                t.push_str(&format!(
                    "        self.set_duty(Channel::Ch{ch}, self.get_max_duty() * value / 10_000);\n"
                ));
            } else {
                t.push_str(&format!(
                    "        self.ch{ch}().set_duty_cycle_fraction(value, 10_000);\n"
                ));
            }
            t.push_str("    }\n");
        }
        t.push_str("}\n");
        t
    };

    tmpl::render(
        PWM_TMPL,
        &[
            ("{GPIO_USE}", &use_line("gpio", &dedup(gpio_items))),
            ("{BREAK_STRUCT}", &break_struct),
            ("{PRELUDE}", &prelude),
            ("{RET_EXPR}", &ret_expr),
            ("{RET}", &ret),
            ("{FREQ}", &cfg.freq_hz.to_string()),
            ("{COUNTING}", cfg.counting.embassy()),
            ("{INTRO}", &intro),
            ("{USAGE}", &usage),
            ("{LOW_LEVEL_USE}", &low_level_use),
            ("{DRIVER_USE}", &driver_use),
            ("{TIMER_USE}", &timer_use),
            ("{DRIVER}", driver),
            ("{DUTY_TRAIT}", &duty_trait),
            ("{DUTY_CONSTS}", &duty_consts),
            ("{PARAMS}", &params),
            ("{CH_ARGS}", &ch_args),
            ("{ENABLES}", &enables),
            ("{HANDLE}", handle),
            ("{N}", &n.to_string()),
        ],
    )
}

/// The timers with at least one PWM channel wired, as
/// the shape `pwm_config_file` and the `main.rs` call both need.
#[derive(Default)]
pub struct PwmWiring {
    /// `(channel, pin name)` for the plain outputs, ascending.
    pub chans: Vec<(u8, String)>,
    /// `(channel, pin name)` for the complementary `CHxN` pads, ascending.
    pub comp: Vec<(u8, String)>,
    /// `(input index, pin name)` for the break pads — 1 is BKIN, 2 is BKIN2.
    pub breaks: Vec<(u8, String)>,
}

impl PwmWiring {
    /// The `init` parameters in call order: channel by channel, the plain pad
    /// before its complementary one. `(parameter name, pin, complementary)`.
    ///
    /// One list drives the signature, the arguments in `main.rs` and the slots
    /// handed to embassy, so the three cannot fall out of step.
    pub(super) fn params(&self) -> Vec<(u8, String, &str, bool)> {
        let mut out = Vec::new();
        for ch in 1..=4u8 {
            if let Some((_, pin)) = self.chans.iter().find(|(c, _)| *c == ch) {
                out.push((ch, format!("ch{ch}"), pin.as_str(), false));
            }
            if let Some((_, pin)) = self.comp.iter().find(|(c, _)| *c == ch) {
                out.push((ch, format!("ch{ch}n"), pin.as_str(), true));
            }
        }
        // Break pads last, so adding one never renumbers the channel arguments.
        for (i, pin) in &self.breaks {
            out.push((*i, format!("bkin{i}"), pin.as_str(), false));
        }
        out
    }

    /// `true` when this timer needs `ComplementaryPwm` rather than `SimplePwm`:
    /// a complementary pad, or a break input, both of which live only there.
    pub(super) fn needs_complementary(&self) -> bool {
        !self.comp.is_empty() || !self.breaks.is_empty()
    }

    /// Channels with at least one pad wired — the ones that need a duty. A
    /// channel whose only pad is the complementary one still has a compare
    /// value; it is the channel that carries the duty, not the pin.
    fn active_channels(&self) -> Vec<u8> {
        (1..=4u8)
            .filter(|ch| {
                self.chans.iter().any(|(c, _)| c == ch) || self.comp.iter().any(|(c, _)| c == ch)
            })
            .collect()
    }
}

/// The timer `time-driver-any` hands embassy-time on this chip, among the
/// timers its pads offer - `None` when it has none of the candidates.
///
/// embassy's own order (`build.rs`): two-channel timers, then two-channel with
/// complementary outputs, then 16-bit general purpose, 32-bit, advanced; the
/// larger number first inside each. On an STM32F103C8 that is TIM4, which is
/// why `p.TIM4` does not exist in an async F103 project.
pub(super) fn time_driver_timer(pins: &[&Pin]) -> Option<u8> {
    const ORDER: [u8; 15] = [22, 21, 12, 9, 15, 19, 4, 3, 24, 23, 5, 2, 20, 8, 1];
    let present: std::collections::BTreeSet<u8> = pins
        .iter()
        .flat_map(|p| p.available_functions.iter())
        .filter_map(|f| match f {
            PinFunction::TimerPwm { timer, .. }
            | PinFunction::TimerPwmN { timer, .. }
            | PinFunction::TimerBreak { timer, .. } => Some(*timer),
            _ => None,
        })
        .collect();
    ORDER.into_iter().find(|t| present.contains(t))
}

/// The timers with at least one PWM pad wired, plain or complementary.
pub(super) fn pwm_wires(pins: &[&Pin]) -> Vec<(u8, PwmWiring)> {
    let mut by_timer: BTreeMap<u8, PwmWiring> = BTreeMap::new();
    for p in pins.iter().filter(|p| !p.reserved) {
        let (timer, channel, which) = match p.selected_function {
            PinFunction::TimerPwm { timer, channel } => (timer, channel, 0),
            PinFunction::TimerPwmN { timer, channel } => (timer, channel, 1),
            PinFunction::TimerBreak { timer, input } => (timer, input, 2),
            _ => continue,
        };
        let w = by_timer.entry(timer).or_default();
        let list = match which {
            0 => &mut w.chans,
            1 => &mut w.comp,
            _ => &mut w.breaks,
        };
        list.push((channel, p.gpio().to_owned()));
    }
    for w in by_timer.values_mut() {
        for list in [&mut w.chans, &mut w.comp, &mut w.breaks] {
            list.sort_unstable();
            // One pin per channel: a second pad claiming the same channel would
            // become a duplicate argument.
            list.dedup_by_key(|(c, _)| *c);
        }
    }
    by_timer.into_iter().collect()
}

#[cfg(test)]
mod pwm_tests {
    use super::*;
    use crate::panels::mcu_module::pins::logic::pin::Pin;

    fn mk(name: &str, timer: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerPwm { timer, channel };
        p
    }

    /// The complementary pad of `timer` CH`channel`.
    fn mkn(name: &str, timer: u8, channel: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerPwmN { timer, channel };
        p
    }

    /// A break pad of `timer` — `input` 1 is BKIN, 2 is BKIN2.
    fn mkb(name: &str, timer: u8, input: u8) -> Pin {
        let mut p = Pin::new(1, name);
        p.selected_function = PinFunction::TimerBreak { timer, input };
        p
    }

    fn run(pins: &[Pin], timer: BTreeMap<u8, TimerModuleConfig>) -> AsyncPeriphs {
        let refs: Vec<&Pin> = pins.iter().collect();
        async_peripherals(
            "stm32g0",
            ChipData {
                i2s: true,
                dma: None,
                irq_vectors: &[],
                usart_ip: Some("sci3_v2_1_Cube"),
                sdmmc_ip: None,
            },
            CompInputs {
                settings: &Default::default(),
                instances: &[],
                pins: &[],
                raw: &Default::default(),
            },
            &refs,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &timer,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            None,
            &Default::default(),
            &Default::default(),
            &Default::default(),
        )
    }

    /// Two channels of ONE timer are one module, one config file and one `init`
    /// — the whole reason the module is the timer rather than the channel.
    #[test]
    fn channels_of_one_timer_share_a_single_init() {
        let pins = [mk("PA6", 3, 1), mk("PA7", 3, 2)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.freq_hz = 20_000;
        cfg.set_duty_x100(1, 7_500);
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());

        assert_eq!(
            out.config_files.len(),
            1,
            "one file per timer: {:?}",
            out.config_files.iter().map(|(n, _)| n).collect::<Vec<_>>()
        );
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "pwm3.rs");
        assert!(
            out.init_calls.contains(
                "let mut _pwm3 = pins::configs::pwm3::init(p.TIM3, pa6_tim3_ch1, pa7_tim3_ch2);"
            ),
            "{}",
            out.init_calls
        );
        // The frequency is the module's, shared; the duty is per channel, and a
        // channel the user never touched starts at 0 %.
        assert!(body.contains("pub const FREQ_HZ: u32 = 20000;"), "{body}");
        assert!(
            body.contains("pub const DUTY_CH1: u32 = 7500; // 75 %, in hundredths"),
            "{body}"
        );
        assert!(
            body.contains("pub const DUTY_CH2: u32 = 0; // 0 %, in hundredths"),
            "{body}"
        );
        // Wired channels become parameters; the rest stay `None` slots.
        assert!(
            body.contains("ch1: Peri<'d, impl TimerPin<peripherals::TIM3, Ch1>>"),
            "{body}"
        );
        assert!(
            body.contains("Some(PwmPin::new(ch2, OutputType::PushPull)),"),
            "{body}"
        );
        assert_eq!(
            body.matches("        None,").count(),
            2,
            "CH3 + CH4: {body}"
        );
        assert!(
            body.contains("pwm.ch1().set_duty_cycle_fraction(DUTY_CH1, 10_000);"),
            "{body}"
        );
    }

    /// One CHxN pad changes the whole driver: `ComplementaryPwm` instead of
    /// `SimplePwm`, eight slots instead of four, and a dead time between the
    /// two edges — which is the entire reason the pad exists.
    #[test]
    fn a_complementary_pad_switches_the_driver() {
        let pins = [mk("PA8", 1, 1), mkn("PB13", 1, 1)];
        let mut cfg = TimerModuleConfig::new(1);
        cfg.freq_hz = 20_000;
        cfg.set_duty_x100(1, 5_000);
        cfg.dead_time = 40;
        cfg.set_channel(
            1,
            crate::panels::mcu_module::modules::PwmChannelConfig {
                polarity: PwmPolarity::ActiveLow,
                mode: PwmMode::Mode2,
                ..Default::default()
            },
        );
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());
        let (name, body) = &out.config_files[0];
        assert_eq!(name, "pwm1.rs");

        // Both pads are parameters, the plain one before its complement.
        assert!(
            out.init_calls.contains(
                "let mut _pwm1 = pins::configs::pwm1::init(p.TIM1, pa8_tim1_ch1, pb13_tim1_ch1n);"
            ),
            "{}",
            out.init_calls
        );
        assert!(
            body.contains("ch1: Peri<'d, impl TimerPin<peripherals::TIM1, Ch1>>"),
            "{body}"
        );
        assert!(
            body.contains("ch1n: Peri<'d, impl TimerComplementaryPin<peripherals::TIM1, Ch1>>"),
            "{body}"
        );

        assert!(
            body.contains("ComplementaryPwm<'d, peripherals::TIM1>"),
            "{body}"
        );
        assert!(
            body.contains("Some(ComplementaryPwmPin::new(ch1n, OutputType::PushPull)),"),
            "{body}"
        );
        // Eight slots: four channels, each with its complement.
        assert_eq!(body.matches("        None,").count(), 6, "{body}");

        // Dead time is set BEFORE anything is enabled, and the duty is a
        // compare value here, not a ratio.
        let dead = body.find("set_dead_time").expect("dead time");
        let enable = body.find("pwm.enable(").expect("enable");
        assert!(dead < enable, "dead time must land first:\n{body}");
        assert!(body.contains("pub const DEAD_TIME: u16 = 40;"), "{body}");
        assert!(
            body.contains("pwm.set_duty(Channel::Ch1, max * DUTY_CH1 / 10_000);"),
            "{body}"
        );
        assert!(!body.contains("set_duty_cycle_fraction"), "{body}");

        // Polarity lands on the MAIN side only — inverting both would undo the
        // pairing the complementary pad exists for.
        assert!(
            body.contains("pwm.set_main_polarity(Channel::Ch1, OutputPolarity::ActiveLow);"),
            "{body}"
        );
        assert!(!body.contains("set_complementary_polarity"), "{body}");
        // PWM mode 2 has no setter on this driver, and saying so must not spill
        // a bare line into the user's file: every line of the note is a comment.
        assert!(
            body.contains("    // output-compare-mode setter."),
            "the note must stay commented on every line:
{body}"
        );
        for line in body.lines() {
            let t = line.trim_start();
            assert!(
                !t.starts_with("setter.") && !t.starts_with("gives the same"),
                "a continuation lost its `//`:
{body}"
            );
        }
    }

    /// A channel whose ONLY wired pad is the complementary one still owns a
    /// duty: the compare value belongs to the channel, not to the pin.
    #[test]
    fn a_lone_complementary_pad_still_gets_its_duty() {
        let pins = [mkn("PB13", 1, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("pub const DUTY_CH1: u32 = 0;"), "{body}");
        assert!(
            body.contains("pwm.set_duty(Channel::Ch1, max * DUTY_CH1 / 10_000);"),
            "{body}"
        );
        // No plain pad, so nothing imports `PwmPin` or names `TimerPin`.
        assert!(!body.contains("simple_pwm::PwmPin"), "{body}");
        assert!(!body.contains("TimerPin<"), "{body}");
    }

    /// TIM15/16/17 carry a CH1N pad, but embassy's `ComplementaryPwm` is bound
    /// to the advanced-control timers. Say so and drive the plain channel,
    /// rather than emitting code that cannot compile.
    #[test]
    fn a_complementary_pad_on_a_plain_timer_is_refused_with_a_reason() {
        let pins = [mk("PA6", 16, 1), mkn("PB6", 16, 1)];
        let out = run(&pins, Default::default());
        assert!(
            out.init_calls
                .contains("TIM16 CH1N (PB6) left unconfigured"),
            "{}",
            out.init_calls
        );
        // The plain channel still works, and the refused pad is NOT passed in.
        assert!(
            out.init_calls
                .contains("pins::configs::pwm16::init(p.TIM16, pa6_tim16_ch1);"),
            "{}",
            out.init_calls
        );
        let (_, body) = &out.config_files[0];
        assert!(body.contains("SimplePwm<'d, peripherals::TIM16>"), "{body}");
        assert!(!body.contains("ComplementaryPwm"), "{body}");
    }

    /// A break pad alone — no complementary channel in sight — is still enough
    /// to need `ComplementaryPwm`: that is where every break bit lives.
    #[test]
    fn a_break_pad_alone_switches_the_driver() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("ComplementaryPwm<'d, peripherals::TIM1>"),
            "{body}"
        );
        // No CHxN pad, so all four complementary slots stay `None`.
        assert_eq!(body.matches("        None,").count(), 7, "{body}");
    }

    /// The break pad is the one pin the generated `init` has to put into
    /// alternate-function mode itself, and it must OUTLIVE `init` — a dropped
    /// `Flex` disconnects the pin and the fault line goes deaf.
    #[test]
    fn a_break_pad_is_configured_by_hand_and_handed_back() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let mut cfg = TimerModuleConfig::new(1);
        cfg.set_break(
            1,
            crate::panels::mcu_module::modules::BreakInputConfig {
                polarity: crate::panels::mcu_module::modules::BreakPolarity::ActiveHigh,
                filter: 3,
            },
        );
        cfg.auto_output_enable = true;
        let out = run(&pins, [(1u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];

        // The pad is a parameter, bounded so a pin that cannot be BKIN for this
        // timer is a compile error rather than a silent no-op.
        assert!(
            body.contains("bkin1: Peri<'d, impl BreakInputPin<peripherals::TIM1, BkIn1>>"),
            "{body}"
        );
        // …put into AF mode by hand, because no embassy driver takes one.
        assert!(body.contains("let bkin1_af = bkin1.af_num();"), "{body}");
        assert!(
            body.contains("bkin1.set_as_af_unchecked(bkin1_af, AfType::input(Pull::None));"),
            "{body}"
        );
        // …and handed back, so the caller keeps it alive.
        assert!(body.contains("pub struct BreakPads<'d> {"), "{body}");
        assert!(body.contains("pub bkin1: Flex<'d>,"), "{body}");
        assert!(body.contains("(pwm, BreakPads { bkin1 })"), "{body}");
        assert!(
            out.init_calls.contains(
                "let (mut _pwm1, _pwm1_break_pads) = pins::configs::pwm1::init(p.TIM1, pa8_tim1_ch1, pa6_tim1_bkin1);"
            ),
            "{}",
            out.init_calls
        );

        // Every break setting reaches the file, enable last.
        assert!(
            body.contains("pwm.set_break_polarity(BreakInputPolarity::ACTIVE_HIGH);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.set_break_filter(FilterValue::FCK_INT_N8);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.set_break_input_pin_enable(true);"),
            "{body}"
        );
        assert!(body.contains("pwm.set_break_enable(true);"), "{body}");
        assert!(
            body.contains("pwm.set_automatic_output_enable(true);"),
            "{body}"
        );
        let cfgd = body.find("set_break_polarity").expect("polarity");
        let enabled = body.find("set_break_enable").expect("enable");
        assert!(cfgd < enabled, "configure before enabling:\n{body}");
    }

    /// A break pad is NOT a complementary pad, even though `BKIN` ends in an N.
    /// They land in different lists, and a break-only timer has an empty one.
    #[test]
    fn a_break_pad_is_not_mistaken_for_a_complementary_one() {
        let pins = [mk("PA8", 1, 1), mkb("PA6", 1, 1)];
        let refs: Vec<&Pin> = pins.iter().collect();
        let wires = pwm_wires(&refs);
        assert_eq!(wires.len(), 1);
        let (timer, w) = &wires[0];
        assert_eq!(*timer, 1);
        assert_eq!(w.chans.len(), 1);
        assert!(w.comp.is_empty(), "BKIN is not half of a pair");
        assert_eq!(w.breaks, vec![(1u8, "PA6".to_owned())]);
    }

    /// TIM15/16/17 have a break pad too, and the same driver limit applies.
    #[test]
    fn a_break_pad_on_a_plain_timer_is_refused_with_a_reason() {
        let pins = [mk("PA6", 16, 1), mkb("PB6", 16, 1)];
        let out = run(&pins, Default::default());
        assert!(
            out.init_calls
                .contains("TIM16 BKIN (PB6) left unconfigured"),
            "{}",
            out.init_calls
        );
        // …and the refused pad is not passed in, nor held.
        assert!(
            out.init_calls
                .contains("pins::configs::pwm16::init(p.TIM16, pa6_tim16_ch1);"),
            "{}",
            out.init_calls
        );
        let (_, body) = &out.config_files[0];
        assert!(!body.contains("BreakPads"), "{body}");
    }

    /// Untouched settings generate exactly what they always did: the reset
    /// state of the timer, and not one line explaining that it is the reset
    /// state. Everything below is what a project pays for only if it asks.
    #[test]
    fn default_output_settings_add_nothing_to_the_file() {
        let pins = [mk("PA6", 3, 1)];
        let out = run(&pins, Default::default());
        let (_, body) = &out.config_files[0];
        assert!(body.contains("CountingMode::EdgeAlignedUp,"), "{body}");
        assert!(
            body.contains("Some(PwmPin::new(ch1, OutputType::PushPull)),"),
            "{body}"
        );
        assert!(!body.contains("set_polarity"), "{body}");
        assert!(!body.contains("set_output_compare_mode"), "{body}");
        // One item, so no braces around the import.
        assert!(
            body.contains("use embassy_stm32::timer::low_level::CountingMode;"),
            "{body}"
        );
    }

    /// …and each of the four settings reaches the generated file, with the
    /// `low_level` import growing to cover exactly what the code names.
    #[test]
    fn the_output_settings_reach_the_generated_file() {
        use crate::panels::mcu_module::modules::{
            PwmChannelConfig, PwmCounting, PwmMode, PwmOutput, PwmPolarity,
        };

        let pins = [mk("PA6", 3, 1), mk("PA7", 3, 2)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.counting = PwmCounting::CenterBothInterrupts;
        cfg.set_channel(
            1,
            PwmChannelConfig {
                output: PwmOutput::OpenDrain,
                polarity: PwmPolarity::ActiveLow,
                mode: PwmMode::Mode2,
            },
        );
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];

        assert!(
            body.contains("CountingMode::CenterAlignedBothInterrupts,"),
            "{body}"
        );
        assert!(
            body.contains("Some(PwmPin::new(ch1, OutputType::OpenDrain)),"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_polarity(OutputPolarity::ActiveLow);"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_output_compare_mode(OutputCompareMode::PwmMode2);"),
            "{body}"
        );
        assert!(
            body.contains("low_level::{CountingMode, OutputCompareMode, OutputPolarity};"),
            "{body}"
        );
        // CH2 was never touched, so it keeps the reset state and its silence.
        assert!(
            body.contains("Some(PwmPin::new(ch2, OutputType::PushPull)),"),
            "{body}"
        );
        assert!(!body.contains("pwm.ch2().set_polarity"), "{body}");
    }

    /// The duty everybody needs first: a hobby servo at 1.5 ms of a 20 ms
    /// frame is 7.5 %, which whole percent could only round away. The ratio
    /// out of 10_000 carries it into the generated file untouched.
    #[test]
    fn a_fractional_duty_survives_into_the_generated_file() {
        let pins = [mk("PA6", 3, 1)];
        let mut cfg = TimerModuleConfig::new(3);
        cfg.freq_hz = 50;
        cfg.set_duty_x100(1, 750);
        let out = run(&pins, [(3u8, cfg)].into_iter().collect());
        let (_, body) = &out.config_files[0];
        assert!(
            body.contains("pub const DUTY_CH1: u32 = 750; // 7.5 %, in hundredths"),
            "{body}"
        );
        assert!(
            body.contains("pwm.ch1().set_duty_cycle_fraction(DUTY_CH1, 10_000);"),
            "{body}"
        );
    }

    /// Two DIFFERENT timers are two modules, and a timer with no module config
    /// still generates at the defaults rather than being skipped.
    #[test]
    fn each_timer_gets_its_own_module() {
        let pins = [mk("PA6", 3, 1), mk("PA8", 1, 1)];
        let out = run(&pins, Default::default());
        let names: Vec<&str> = out.config_files.iter().map(|(n, _)| n.as_str()).collect();
        assert!(
            names.contains(&"pwm1.rs") && names.contains(&"pwm3.rs"),
            "{names:?}"
        );
        assert!(
            out.init_calls.contains("init(p.TIM1, pa8_tim1_ch1)"),
            "{}",
            out.init_calls
        );
        // Defaults when the module carries no config yet.
        let body = &out
            .config_files
            .iter()
            .find(|(n, _)| n == "pwm1.rs")
            .unwrap()
            .1;
        assert!(body.contains("pub const FREQ_HZ: u32 = 1000;"), "{body}");
    }

    /// PWM needs no interrupt and no DMA — `SimplePwm` writes the compare
    /// registers directly, so nothing may be bound for it.
    #[test]
    fn pwm_binds_no_interrupts() {
        let out = run(&[mk("PA6", 3, 1)], Default::default());
        assert!(!out.any_async_dma);
        assert!(out.dma_irqs.is_empty(), "{}", out.dma_irqs);
        assert!(out.dma_uses.is_empty());
    }
}

#[cfg(test)]
mod async_duty_handle_tests {
    use super::*;

    fn wiring(chans: &[u8], comp: &[u8]) -> PwmWiring {
        PwmWiring {
            chans: chans.iter().map(|c| (*c, format!("pa{c}"))).collect(),
            comp: comp.iter().map(|c| (*c, format!("pb{c}"))).collect(),
            breaks: Vec::new(),
        }
    }

    /// `DutyHandle` may never name a channel the timer has no pad for.
    ///
    /// The same rule as the F1 backend, for a different reason: there, asking
    /// for an unwired channel PANICS; here embassy takes it and drives nothing,
    /// which is quieter and no more useful. Both are answered by putting the
    /// channel in the method NAME.
    #[test]
    fn simple_pwm_only_reaches_wired_channels() {
        let cfg = TimerModuleConfig::new(3);
        let f = pwm_config_file(3, &cfg, &wiring(&[2, 4], &[]), "_pwm3");

        assert!(
            f.contains("impl<'d> DutyHandle for SimplePwm<'d, peripherals::TIM3>"),
            "{f}"
        );
        assert!(
            f.contains("fn set_duty_tim_3_ch2(&mut self, value: u32);"),
            "{f}"
        );
        assert!(
            f.contains("fn set_duty_tim_3_ch4(&mut self, value: u32);"),
            "{f}"
        );
        // CH1 and CH3 have no pad, so nothing may reach them.
        for ch in [1, 3] {
            assert!(
                !f.contains(&format!("set_duty_tim_3_ch{ch}")),
                "TIM3 CH{ch} is unwired:\n{f}"
            );
            assert!(!f.contains(&format!("self.ch{ch}()")), "{f}");
        }
        // The bare method delegates to the lowest wired channel, not to CH1.
        assert!(f.contains("        self.set_duty_tim_3_ch2(value);"), "{f}");
    }

    /// The complementary driver has a DIFFERENT duty API — `set_duty(Channel,
    /// value)` against `get_max_duty()`, not `.chN()` — so the trait body is
    /// generated per driver rather than shared. Getting that backwards compiles
    /// nowhere, which is exactly why it is worth pinning.
    #[test]
    fn complementary_pwm_uses_its_own_duty_api() {
        let cfg = TimerModuleConfig::new(1);
        let f = pwm_config_file(1, &cfg, &wiring(&[1], &[1]), "_pwm1");

        assert!(
            f.contains("impl<'d> DutyHandle for ComplementaryPwm<'d, peripherals::TIM1>"),
            "{f}"
        );
        assert!(
            f.contains(
                "        self.set_duty(Channel::Ch1, self.get_max_duty() * value / 10_000);"
            ),
            "{f}"
        );
        assert!(
            !f.contains("set_duty_cycle_fraction(value"),
            "wrong driver's API:\n{f}"
        );
        // A complementary channel means BOTH its pads, and the doc says so.
        assert!(f.contains("/// CH1 and CH1N."), "{f}");
    }
}
