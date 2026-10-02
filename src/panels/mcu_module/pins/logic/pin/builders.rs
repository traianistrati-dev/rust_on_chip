//! Pin constructors and builder methods.

use super::super::pin_function::PinFunction;
use super::model::Pin;

impl Pin {
    /// Create a standard GPIO pin with Input and Output capabilities.
    pub fn new(number: usize, name: &str) -> Self {
        Self {
            number,
            name: name.to_owned(),
            reserved: false,
            available_functions: vec![PinFunction::GpioInput, PinFunction::GpioOutput],
            selected_function: PinFunction::Unset,
            custom_label: String::new(),
            irq: None,
            irq_priority: crate::panels::mcu_module::pins::logic::pin::model::TaskPriority::default(
            ),
            io_mode: None,
            af: Vec::new(),
            fn_owner: Vec::new(),
            note: String::new(),
        }
    }

    /// Create a GPIO pin with ADC (analog input) capability.
    pub fn new_with_analog(number: usize, name: &str, adc: u8, channel: u8) -> Self {
        Self {
            number,
            name: name.to_owned(),
            reserved: false,
            available_functions: vec![
                PinFunction::GpioInput,
                PinFunction::GpioOutput,
                PinFunction::AdcChannel { adc, channel },
            ],
            selected_function: PinFunction::Unset,
            custom_label: String::new(),
            irq: None,
            irq_priority: crate::panels::mcu_module::pins::logic::pin::model::TaskPriority::default(
            ),
            io_mode: None,
            af: Vec::new(),
            fn_owner: Vec::new(),
            note: String::new(),
        }
    }

    /// Create a reserved pin (e.g., VDD, VSS, NRST, GND) that cannot be reconfigured.
    pub fn new_reserved(number: usize, name: &str) -> Self {
        Self {
            number,
            name: name.to_owned(),
            reserved: true,
            available_functions: vec![],
            selected_function: PinFunction::Unset,
            custom_label: String::new(),
            irq: None,
            irq_priority: crate::panels::mcu_module::pins::logic::pin::model::TaskPriority::default(
            ),
            io_mode: None,
            af: Vec::new(),
            fn_owner: Vec::new(),
            note: String::new(),
        }
    }

    /// Builder method: add additional peripheral functions to this pin.
    pub fn with_functions(mut self, extra: Vec<PinFunction>) -> Self {
        self.available_functions.extend(extra);
        self
    }
}
