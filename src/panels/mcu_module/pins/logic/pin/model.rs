//! Pin data model — struct definition and rendering constants.

use super::super::pin_function::PinFunction;

pub const PIN_FONT_SIZE: f32 = 10.0;
pub const PIN_ROUNDING: f32 = 0.0;

/// How urgently an armed pin's task runs, relative to the others.
///
/// On ESP the async runtime is COOPERATIVE: every task shares one executor and
/// runs to its next `await`. A task marked above `Normal` is spawned on its own
/// `InterruptExecutor` instead - an executor bound to a hardware interrupt
/// priority - so it PREEMPTS the tasks below it.
///
/// Only two levels above normal, and that is a hardware fact, not a taste: each
/// interrupt executor costs one software interrupt, and `esp_rtos::start` has
/// already taken one for its scheduler. Two covers "this must not wait" and
/// "this must not wait for THAT either", and leaves one spare.
///
/// Tasks at the SAME level do not preempt each other - they share one executor
/// and stay cooperative, exactly as they are today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum TaskPriority {
    /// The shared thread-mode executor - today's behaviour, and the default.
    #[default]
    Normal,
    High,
    Critical,
}

impl TaskPriority {
    /// Token persisted in `mcu.config` `@irq`, appended after the edge.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::High => "High",
            Self::Critical => "Critical",
        }
    }

    pub fn from_token(t: &str) -> Option<Self> {
        match t.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(Self::Normal),
            "high" => Some(Self::High),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }

    /// Label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::High => "High",
            Self::Critical => "Critical",
        }
    }

    /// Every level, lowest first - for a picker.
    pub fn all() -> [Self; 3] {
        [Self::Normal, Self::High, Self::Critical]
    }
}

/// Which edge of an input pin raises an interrupt.
///
/// `None` on a pin (the default) means "read it by polling" — the common case,
/// and why this is opt-in rather than implied by `GpioInput`. Only meaningful on
/// the RTIC runtime, where each interrupt-enabled pin becomes a hardware task.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Edge {
    Rising,
    Falling,
    Both,
}

impl Edge {
    /// Token persisted in `mcu.config` `@irq`.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Rising => "Rising",
            Self::Falling => "Falling",
            Self::Both => "Both",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s.trim() {
            "Rising" => Some(Self::Rising),
            "Falling" => Some(Self::Falling),
            "Both" => Some(Self::Both),
            _ => None,
        }
    }

    /// The `stm32f1xx-hal` `ExtiPin::trigger_on_edge` argument.
    pub fn hal_variant(self) -> &'static str {
        match self {
            Self::Rising => "Edge::Rising",
            Self::Falling => "Edge::Falling",
            Self::Both => "Edge::RisingFalling",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Rising => "Rising",
            Self::Falling => "Falling",
            Self::Both => "Both edges",
        }
    }
}

/// How a GPIO pin is driven / terminated — the part of the binding the user can
/// change (`into_floating_input` ⇄ `into_pull_up_input`, …).
///
/// It is a NEUTRAL key, not a HAL method name, because the same choice has to
/// survive a Runtime switch: `PullUp` is `into_pull_up_input(&mut gpiob.crl)` on
/// the blocking `stm32f1xx-hal` and `Input::new(p.PB5, Pull::Up)` on embassy.
/// Each backend renders it in its own syntax and says which of these it offers
/// ([`FamilyBackend::gpio_modes`]).
///
/// `None` on a pin means "the backend's default" — floating for an input,
/// push-pull for an output — which is what every project generated before this
/// existed, so a missing `@iomode` section round-trips unchanged.
///
/// [`FamilyBackend::gpio_modes`]: crate::panels::mcu_module::codegen::family::FamilyBackend::gpio_modes
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum GpioMode {
    // Inputs
    Floating,
    PullUp,
    PullDown,
    // Outputs
    PushPull,
    OpenDrain,
}

impl GpioMode {
    /// Token persisted in `mcu.config` `@iomode`.
    pub fn as_token(self) -> &'static str {
        match self {
            Self::Floating => "Floating",
            Self::PullUp => "PullUp",
            Self::PullDown => "PullDown",
            Self::PushPull => "PushPull",
            Self::OpenDrain => "OpenDrain",
        }
    }

    pub fn from_token(s: &str) -> Option<Self> {
        match s.trim() {
            "Floating" => Some(Self::Floating),
            "PullUp" => Some(Self::PullUp),
            "PullDown" => Some(Self::PullDown),
            "PushPull" => Some(Self::PushPull),
            "OpenDrain" => Some(Self::OpenDrain),
            _ => None,
        }
    }

    /// Row text in the mode list under the selected function.
    pub fn label(self) -> &'static str {
        match self {
            Self::Floating => "Floating",
            Self::PullUp => "Pull-up",
            Self::PullDown => "Pull-down",
            Self::PushPull => "Push-pull",
            Self::OpenDrain => "Open-drain",
        }
    }

    /// The `stm32f1xx-hal` (and generally `into_*`) method this mode maps to.
    /// Is this an INPUT mode - a pull, rather than a drive?
    pub fn is_input(self) -> bool {
        matches!(self, Self::Floating | Self::PullUp | Self::PullDown)
    }

    /// The input mode to generate, given whatever the pin has stored.
    ///
    /// # Why a stored mode cannot be trusted
    ///
    /// `io_mode` OUTLIVES the function it was chosen for: `apply_pin_function`
    /// sets `selected_function` and clears `custom_label`, and never touches the
    /// mode. So a pad set to GPIO input, given a pull-up, and then switched to
    /// GPIO output still carries `Some(PullUp)`.
    ///
    /// Handing that to [`Self::into_method`] emits `into_pull_up_input` on a
    /// line commented `// GPIO Output`. It compiles, and it silently makes the
    /// pin an input - which is why the choice is filtered here rather than
    /// defaulted with `unwrap_or`.
    pub fn for_input(stored: Option<Self>) -> Self {
        stored.filter(|m| m.is_input()).unwrap_or(Self::Floating)
    }

    /// The output mode to generate - see [`Self::for_input`] for why the stored
    /// one is filtered rather than merely defaulted.
    pub fn for_output(stored: Option<Self>) -> Self {
        stored.filter(|m| !m.is_input()).unwrap_or(Self::PushPull)
    }

    pub fn into_method(self) -> &'static str {
        match self {
            Self::Floating => "into_floating_input",
            Self::PullUp => "into_pull_up_input",
            Self::PullDown => "into_pull_down_input",
            Self::PushPull => "into_push_pull_output",
            Self::OpenDrain => "into_open_drain_output",
        }
    }
}

/// Represents a physical pin on a microcontroller.
///
/// Each pin has:
/// - A name (e.g., "PA5", "GPIO_21") used for display
/// - A number for identification
/// - A reserved flag indicating if it cannot be reconfigured (VDD, VSS, NRST)
/// - Available functions (GPIO, ADC, SPI, USART, etc.)
/// - Currently selected function
/// - An optional user label appended to the generated variable name
#[derive(Clone)]
pub struct Pin {
    pub name: String,
    pub number: usize,
    pub reserved: bool,
    pub available_functions: Vec<PinFunction>,
    pub selected_function: PinFunction,
    /// User-typed name appended to the generated binding, e.g. a `pc13` output
    /// with label "led" generates `let pc13_out_led = …`. Empty by default;
    /// editable via the in/out arrow on the Pins canvas (GPIO In/Out & PWM).
    pub custom_label: String,
    /// Interrupt trigger for a GPIO input, when the user asked for one.
    ///
    /// Opt-in: most inputs are polled, so deriving "interrupt" from
    /// `GpioInput` alone would generate a task for every button. Consumed by the
    /// RTIC backend; ignored by the other runtimes, which do not wire the NVIC.
    /// Persisted in `mcu.config` (`@irq`).
    pub irq: Option<Edge>,
    /// How urgently this pin's task runs - see [`TaskPriority`]. Persisted on
    /// the same `@irq` line, after the edge, so a project written before this
    /// existed reads back unchanged.
    pub irq_priority: TaskPriority,
    /// Drive / pull mode of a GPIO In/Out pin — `None` = the backend's default
    /// (floating input, push-pull output). Chosen from the mode list under the
    /// function in the chip, persisted in `mcu.config` (`@iomode`), and rendered
    /// by each backend in its own syntax. See [`GpioMode`].
    pub io_mode: Option<GpioMode>,
    /// `(vendor signal, alternate-function index)` for this pin, from the chip
    /// definition ([`crate::panels::mcu_module::mcu_def::PinDef::af`]).
    ///
    /// Per (pin, SIGNAL) rather than per function: the same signal sits on a
    /// different AF number on a different pin, so the index cannot live inside
    /// `PinFunction` — a value that is shared across the whole chip. Empty on
    /// STM32F1 (no per-pin AF mux; it remaps whole peripherals through AFIO) and
    /// for definitions imported before the indices were captured.
    pub af: Vec<(String, u8)>,
    /// `(function, GPIO)` for functions this package pin gets from a GPIO other
    /// than [`name`](Self::name) - see
    /// [`PinDef::fn_owner`](crate::panels::mcu_module::mcu_def::PinDef::fn_owner).
    /// Read through [`Pin::gpio_for`]; never index it directly.
    pub fn_owner: Vec<(PinFunction, String)>,
    /// The board's note on this pad - see
    /// [`PinDef::note`](crate::panels::mcu_module::mcu_def::PinDef::note).
    pub note: String,
}

impl Pin {
    /// The GPIO singleton that actually provides `f` on this package pin.
    ///
    /// Normally the pin's own name. It differs only where a small package bonds
    /// two die pads together: an STM32G030F6P's pin 1 answers to `PB7` for
    /// `USART1_RX` and to `PB8` for `I2C1_SCL`. Generated code must name the one
    /// that carries the chosen signal, or it addresses the wrong peripheral -
    /// so every place that emits `p.<PIN>` goes through here rather than
    /// reaching for `name`.
    pub fn gpio_for(&self, f: &PinFunction) -> &str {
        self.fn_owner
            .iter()
            .find(|(func, _)| func == f)
            .map(|(_, gpio)| gpio.as_str())
            .unwrap_or(&self.name)
    }

    /// The GPIO for the function currently selected on this pin.
    pub fn gpio(&self) -> &str {
        self.gpio_for(&self.selected_function)
    }

    /// Every GPIO bonded to this package pin, primary first - what the canvas
    /// shows as `PB7/PB8`. One entry for an ordinary pin.
    pub fn gpio_names(&self) -> Vec<&str> {
        let mut out = vec![self.name.as_str()];
        for (_, g) in &self.fn_owner {
            if !out.contains(&g.as_str()) {
                out.push(g.as_str());
            }
        }
        out
    }

    /// `true` when any available function is a serial communication bus
    /// (USART / SPI / I2C / USB / CAN). Such pins get an orange number on the
    /// chip so they stand out from plain GPIO / analog / power pins.
    /// The alternate-function index this pin uses for `signal`, if the vendor
    /// published one. `signal` is the datasheet name (`TIM1_CH1N`), which is what
    /// [`PinFunction::Other`] carries.
    pub fn af_of(&self, signal: &str) -> Option<u8> {
        self.af
            .iter()
            .find(|(s, _)| s.eq_ignore_ascii_case(signal))
            .map(|(_, n)| *n)
    }

    /// The AF index of the function currently selected on this pin, when that
    /// function is a generic alternate one. `None` for GPIO / modelled
    /// peripherals (whose driver sets the AF itself) and on STM32F1.
    pub fn selected_af(&self) -> Option<u8> {
        match &self.selected_function {
            PinFunction::Other(signal) => self.af_of(signal),
            _ => None,
        }
    }

    pub fn has_bus_function(&self) -> bool {
        self.available_functions.iter().any(PinFunction::is_bus)
    }
}

#[cfg(test)]
mod stale_mode_tests {
    use super::GpioMode;

    /// A mode chosen for one direction never generates a call for the other.
    ///
    /// `io_mode` outlives the function it was picked for - nothing clears it
    /// when the pad changes direction - so the reachable sequence
    ///
    ///     GPIO input  ->  click Pull-up  ->  GPIO output
    ///
    /// left `Some(PullUp)` on an OUTPUT. `unwrap_or(PushPull)` kept it, and
    /// `into_method` turned it into `into_pull_up_input(...)` on a line
    /// commented `// GPIO Output`: it compiles, and the pin is silently an
    /// input.
    #[test]
    fn a_mode_from_the_other_direction_is_dropped() {
        for stale in [GpioMode::PushPull, GpioMode::OpenDrain] {
            let m = GpioMode::for_input(Some(stale));
            assert!(m.is_input(), "an input got {m:?}");
            assert!(
                m.into_method().ends_with("_input"),
                "{stale:?} on an input generated {}",
                m.into_method()
            );
        }
        for stale in [GpioMode::Floating, GpioMode::PullUp, GpioMode::PullDown] {
            let m = GpioMode::for_output(Some(stale));
            assert!(!m.is_input(), "an output got {m:?}");
            assert!(
                m.into_method().ends_with("_output"),
                "{stale:?} on an output generated {}",
                m.into_method()
            );
        }
    }

    /// A mode of the RIGHT direction is honoured, and no mode at all falls to
    /// the backend default - the two behaviours the filter must not break.
    #[test]
    fn a_matching_mode_survives_and_none_defaults() {
        assert_eq!(
            GpioMode::for_input(Some(GpioMode::PullDown)),
            GpioMode::PullDown
        );
        assert_eq!(
            GpioMode::for_output(Some(GpioMode::OpenDrain)),
            GpioMode::OpenDrain
        );
        assert_eq!(GpioMode::for_input(None), GpioMode::Floating);
        assert_eq!(GpioMode::for_output(None), GpioMode::PushPull);
    }
}
