//! Pin color logic — background and text colors based on reserved status and
//! function, plus the plain-English role of a reserved pin.

use super::model::Pin;
use eframe::egui;

const POWER: egui::Color32 = egui::Color32::from_rgb(200, 50, 50);
const GROUND: egui::Color32 = egui::Color32::from_rgb(30, 30, 30);
const VBAT: egui::Color32 = egui::Color32::from_rgb(220, 100, 100);

/// Is this reserved pin a supply rail?
///
/// PREFIX, not an exact name. Real parts label these `VDDA/VREF+`, `VSSA/VREF-`,
/// `VDD_1`, `VSS_2` — an exact match caught only the bare `VDD`/`VSS`, so every
/// decorated variant fell through to the grey that means "some other reserved
/// pin" and the supply rails stopped being findable at a glance.
fn is_power(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    n.starts_with("VDD") || n.starts_with("VBAT")
}

/// The same for ground, including the ESP32 spelling.
fn is_ground(name: &str) -> bool {
    let n = name.trim().to_ascii_uppercase();
    n.starts_with("VSS") || n.starts_with("GND")
}

/// What a reserved pin is FOR, in one line, for the panel that opens when you
/// click it.
///
/// Reserved pins carry no selectable functions, so without this the panel would
/// be a header over an empty box. Matched by prefix for the same reason the
/// colours are.
pub fn reserved_role(name: &str) -> &'static str {
    let n = name.trim().to_ascii_uppercase();
    // Order matters: the longer, more specific prefixes are tested first, or
    // `VDDA` would be answered by the plain-`VDD` arm.
    if n.starts_with("VBAT") {
        "Backup supply - keeps the RTC and the backup registers alive while VDD is off."
    } else if n.starts_with("VDDA") {
        "Analog supply. Often shared with the converter reference (VREF+); decouple it separately from VDD."
    } else if n.starts_with("VREF") {
        "ADC/DAC voltage reference. Its accuracy sets the accuracy of every conversion."
    } else if n.starts_with("VDD") {
        "Digital supply. Each one wants its own decoupling capacitor."
    } else if n.starts_with("VSSA") {
        "Analog ground. Tie it to VSS at a single point to keep converter noise out of it."
    } else if n.starts_with("VSS") || n.starts_with("GND") {
        "Ground."
    } else if n.starts_with("NRST") || n == "RST" {
        "Reset, active LOW. Driven by the internal pull-up circuit at power-on."
    } else if n.starts_with("BOOT") {
        "Boot mode select, sampled at reset: it chooses whether the chip starts from flash or from the bootloader."
    // Raspberry Pi Pico board pads. These are BOARD pins, not chip pins, and
    // every one of them answers a question a user actually asks while wiring.
    // The CYW43 radio's four lines on a Pico W / Pico 2 W. They are GP23, GP24,
    // GP25 and GP29 on the die, but on a W board they are spoken for — and GP25
    // is the one that surprises people, because on a non-W Pico it is the LED.
    } else if n == "WL_ON" {
        "Powers the CYW43 radio (GP23). Held low until the wireless driver brings it up."
    } else if n == "WL_D" {
        "The radio's SPI data line (GP24), driven by a PIO program rather than the SPI block."
    } else if n == "WL_CS" {
        "The radio's chip select (GP25). The on-board LED is NOT here on a W board - it hangs off the radio's own GPIO0, so blinking it needs the wireless driver."
    } else if n == "WL_CLK" {
        "The radio's SPI clock (GP29), shared with the VSYS sense divider."
    } else if n == "AGND" {
        "Analog ground, for the ADC. Star-tie it to GND so converter noise does not ride on the digital return."
    } else if n == "ADC_VREF" {
        "ADC reference. Filtered 3V3 on the board; drive it separately for a cleaner conversion."
    } else if n == "RUN" {
        "Chip enable, active HIGH. Pull it LOW to reset the board; it is how an external circuit holds the Pico down."
    } else if n == "3V3_OUT" {
        "3.3 V from the on-board regulator. Good for about 300 mA, and it is what powers the chip."
    } else if n == "3V3_EN" {
        "Enables the 3.3 V regulator, pulled HIGH on the board. Pull it LOW to switch the Pico off."
    } else if n == "VSYS" {
        "Main input, 1.8 to 5.5 V. Feeds the regulator through a diode from VBUS, so it can also be back-powered."
    // tinyVision pico2-ice. Its reserved pads carry their GPIO in the name -
    // `ICE_SS (GP5)` - so they are matched on the first word.
    } else if let Some(role) = pico2_ice_role(&n) {
        role
    // BBC micro:bit v2 edge connector. The two large rings and the small pads
    // tied to them carry the same supply; the accessibility pad is a GPIO the
    // board reserves for assistive switches.
    } else if n == "3V" || n.starts_with("3V (") {
        "3.3 V from the micro:bit's own regulator, shared with the chip and the on-board hardware, so the current left for accessories is limited."
    // Nordic's DKs. These are GPIOs on the die that the kit has wired to its
    // own hardware, so the name leads with the pin and the role is inside.
    } else if n.contains("(XL1") || n.contains("(XL2") {
        "The 32.768 kHz crystal. It is the LFXO the Clock tab offers, so the pin is not free for GPIO."
    } else if n.starts_with("P0.") && n.contains("(RESET)") {
        "Pin reset, wired to the DK's RESET button. A plain GPIO while UICR.PSELRESET is erased - but embassy-nrf's init programs it as pin reset (unless built with `reset-pin-as-gpio`), and once written the UICR stays, so it is kept reserved."
    } else if n.contains("ACCESSIBILITY") {
        "P0.12, reserved by the micro:bit for accessibility hardware (switch access). The foundation asks that nothing else use it."
    } else if n == "VBUS" {
        "5 V straight from the USB connector, present only while USB is plugged in."
    } else if n.starts_with("NPOR") {
        "Power-on reset."
    } else if n.starts_with("CHIP_PU") || n.starts_with("CHIP_EN") {
        // Two spellings of one pin: an ESP32/S2/S3/C5/C6/C61 calls it CHIP_PU,
        // a C2/C3/H2 calls it CHIP_EN. Only the first was answered, so the most
        // important reserved pad on half the Espressif parts read "Reserved -
        // fixed by the package".
        "Chip enable, active HIGH. Held low the part stays in reset."
    } else if n.starts_with("XTAL_") {
        "Main crystal. Its frequency is what the PLL multiplies up - the Clock tab shows which."
    } else if n.starts_with("LNA_IN") || n.starts_with("ANT") {
        "Radio antenna feed. It reaches the antenna through a matching network, and nothing else may load it."
    } else if n.starts_with("CAP") {
        "Filter capacitor for an on-chip supply. It takes the part its datasheet specifies and no signal."
    } else if n.starts_with("GPIO") {
        // A GPIO that is nonetheless RESERVED. On an ESP32-C5 the datasheet
        // gives pins 25-32 as GPIO15-GPIO22, and the esp-metadata release
        // esp-hal pins has no `peripherals.GPIO15` for the part — so the pad is
        // on the package and there is nothing the generated code could name.
        // Without this arm those seven pads read "Reserved - fixed by the
        // package", which says nothing about why a numbered GPIO is greyed out.
        "On the package, but this chip's HAL has no singleton for it - most often the in-package flash bus. Nothing generated can name it."
    } else {
        "Reserved - fixed by the package, not configurable here."
    }
}

/// What a NON-reserved board pad does when its function is picked, for the one
/// kind of pad where the function's own name undersells it: a board line whose
/// "GPIO Output" switches something on. `None` for every ordinary pin.
///
/// The Pins panel shows it above the function list and the Peripherals tab in
/// the tooltip, because both would otherwise offer the pad as the cheapest
/// plain output on the chip.
pub fn switch_role(name: &str) -> Option<&'static str> {
    name.trim().to_ascii_uppercase().starts_with("ICE_CRESET").then_some(
        "The FPGA's CRESET_B (GP31, 10k pull-down). GPIO Output here turns on the FPGA loader: at boot the firmware sends fpga/top.bin into the iCE40 over GP4..7, clocks it from GP21 and checks CDONE on GP40, before anything else runs. It is not a GPIO for your code - the loader hands back `fpga_reset` instead.",
    )
}

/// The pico2-ice's reserved pads: the FPGA's configuration port, its own I/O,
/// and the rails the board adds. `n` is the upper-cased name.
///
/// Most of these surprise someone who knows the Pico: that GP5 is ALSO the
/// FPGA flash's chip select, that the FPGA clock is GP21 and not the GP22 the
/// vendor header names, that most header pins never reach the RP2350.
fn pico2_ice_role(n: &str) -> Option<&'static str> {
    // Names another chip could carry for something else entirely match on the
    // WHOLE pico2-ice spelling, so an `ADC7` on some other part is not told
    // it hangs off a divider on this board.
    match n {
        "ADC7 (GP47)" => {
            return Some(
                "GP47 (ADC7) through a 2.2k / 10k divider with a clamp: an analog input scaled for higher voltages, not a plain GPIO.",
            );
        }
        "ADC5/VREF (GP45)" => {
            return Some(
                "GP45 (ADC5), tied by the bridged jumper R30 to a TL431 2.5 V reference with 100 nF to GND, which GP46 powers through 1k. It reads that reference; driven, it fights the shunt. Cut R30 and R31 to use the header pin.",
            );
        }
        "PSRAM_CS (GP8)" => {
            return Some(
                "Chip select of the 8 MB PSRAM on the QSPI bus (GP8, QMI CS1). On no header.",
            );
        }
        _ => {}
    }
    let first = n.split_whitespace().next().unwrap_or("");
    Some(match first {
        "ICE_SS" => {
            "The FPGA's SPI_SS (GP5) - and on the same net the FPGA flash's chip select, with a 10k pull-up. The FPGA loader drives it; anything else here selects the flash."
        }
        "ICE_SO" => {
            "The FPGA's SPI_SO (GP7), which is also the FPGA flash's data IN. The loader sends the flash to sleep on it before a load."
        }
        "ICE_SI" => {
            "Data INTO the FPGA while it is configured (GP4), and the FPGA flash's data OUT. GP4 is SPI0 RX only, so the load cannot use the SPI block."
        }
        "ICE_SCK" => {
            "The FPGA's configuration clock (GP6, through 27 R), shared with the FPGA flash."
        }
        "ICE_DONE" => {
            "CDONE (GP40): HIGH once the FPGA holds a configuration. It also lights the green LED D3."
        }
        "ICE_CLK" => {
            "Clock into the FPGA's global buffer (FPGA pin 35) from GP21, which is GPOUT0. GP22 cannot output a clock, whatever the vendor header says."
        }
        "ICE_LED_R" | "ICE_LED_G" | "ICE_LED_B" => {
            "The FPGA's RGB LED, active LOW, on the iCE40's 24 mA current-sink pins. Only gateware can drive it."
        }
        "ICE10" => {
            "iCE40 pin 10, wired to the FPGA's user button SW2 (active LOW, 10k pull-up). The RP2350 cannot read it."
        }
        "ICE12" | "ICE13" => {
            "iCE40 pin 12 / 13, which is also the FPGA flash's IO2 / IO3 (10k pull-down). Gateware only."
        }
        "3V3_FPGA" => {
            "The FPGA's 3.3 V rail, from the board's regulator. It also powers the FPGA flash."
        }
        "VIO_BANK2" => {
            "I/O supply of the FPGA's bank 2 (the ICE PMOD A pins). Tied to 3V3_FPGA by jumper SJ5; cut it to run that bank at another voltage."
        }
        "3V3" => "3.3 V from the on-board regulator, which powers the chip.",
        "VIN" => {
            "The board's supply input, into its regulator. Power the board here instead of from USB."
        }
        // Every other `ICEn`: an FPGA I/O with no wire to the RP2350.
        f if f
            .strip_prefix("ICE")
            .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) =>
        {
            "An iCE40 I/O pin with no wire to the RP2350: only the FPGA's gateware drives it, and the .pcf names it by this number."
        }
        _ => return None,
    })
}

impl Pin {
    /// Determine background color for this pin.
    ///
    /// Reserved pins (power, ground, reset) have specific colors.
    /// User-configurable pins inherit color from their selected function.
    pub fn get_background_color(&self) -> egui::Color32 {
        if self.reserved {
            // Ground first: `VSS`/`GND` can never be a supply, and testing it
            // first keeps each rule a single prefix check.
            if is_ground(&self.name) {
                return GROUND;
            }
            if is_power(&self.name) {
                // VBAT keeps its lighter red. It is a supply, but not THE
                // supply, and telling them apart on the diagram is worth a shade.
                return if self.name.trim().to_ascii_uppercase().starts_with("VBAT") {
                    VBAT
                } else {
                    POWER
                };
            }
            // No exact-match list of Espressif rails here. There used to be one
            // — `VDD3P3`, `VDD3P3_CPU`, `VDD3P3_RTC`, `VDD_SPI` — and it was
            // both DEAD and STALE: every one of those starts with `VDD`, so
            // `is_power` above had already answered, and the eight parts added
            // after the C3 brought `VDDA1..8`, `VDDPST1..3`, `VDD_SDIO` and
            // `VDDA_PMU`, none of which were in it.
            //
            // Misc reserved (NRST, BOOT0, CHIP_PU, LNA_IN, …)
            return egui::Color32::LIGHT_GRAY;
        }
        self.selected_function.color()
    }

    /// Determine text color for this pin.
    ///
    /// Reserved pins use white on the near-black ground colour and black
    /// elsewhere; user-configurable pins use black.
    pub fn get_text_color(&self) -> egui::Color32 {
        if self.reserved && is_ground(&self.name) {
            return egui::Color32::WHITE;
        }
        egui::Color32::BLACK
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bg(name: &str) -> egui::Color32 {
        Pin::new_reserved(1, name).get_background_color()
    }

    /// The decorated names real chips actually use. Every one of these was grey
    /// before, because the match was on the exact string.
    #[test]
    fn decorated_rail_names_still_read_as_rails() {
        for n in ["VDD", "VDDA", "VDDA/VREF+", "VDD_1", "VDDIO"] {
            assert_eq!(bg(n), POWER, "{n}");
        }
        for n in ["VSS", "VSSA", "VSSA/VREF-", "VSS_2", "GND"] {
            assert_eq!(bg(n), GROUND, "{n}");
        }
    }

    #[test]
    fn vbat_stays_distinguishable_from_the_main_supply() {
        assert_eq!(bg("VBAT"), VBAT);
        assert_ne!(bg("VBAT"), bg("VDD"));
    }

    #[test]
    fn ground_text_stays_readable_on_black() {
        assert_eq!(
            Pin::new_reserved(1, "VSSA/VREF-").get_text_color(),
            egui::Color32::WHITE
        );
        assert_eq!(
            Pin::new_reserved(1, "VDD").get_text_color(),
            egui::Color32::BLACK
        );
    }

    /// Every reserved pin says something specific where it can; the catch-all is
    /// the last resort, not the common case.
    #[test]
    fn the_roles_are_specific_where_they_can_be() {
        assert!(reserved_role("VDDA/VREF+").contains("Analog supply"));
        assert!(reserved_role("VSSA").contains("Analog ground"));
        assert!(reserved_role("VBAT").contains("Backup"));
        assert!(reserved_role("NRST").contains("active LOW"));
        assert!(reserved_role("BOOT0").contains("Boot mode"));
        // The prefix order matters: VDDA must not fall into the plain-VDD arm.
        assert_ne!(reserved_role("VDDA"), reserved_role("VDD"));

        // The Espressif pads. `LNA_IN` was pinned to the generic answer here,
        // which described the state rather than an invariant — on an ESP32 most
        // reserved pads are one of these, and the panel exists to say what a pin
        // is for.
        assert!(reserved_role("LNA_IN").contains("antenna"));
        assert!(reserved_role("ANT_2G").contains("antenna"));
        assert!(reserved_role("XTAL_P").contains("crystal"));
        assert!(reserved_role("CAP1").contains("capacitor"));
        // One pin, two spellings: an ESP32 says CHIP_PU, a C3 says CHIP_EN.
        assert_eq!(reserved_role("CHIP_EN"), reserved_role("CHIP_PU"));
        assert!(reserved_role("CHIP_EN").contains("Chip enable"));

        // …and the generic is still there, for a pad nothing is known about.
        assert!(reserved_role("PAD_7").starts_with("Reserved"));
    }

    /// The pico2-ice's answers stay on the pico2-ice: a pad another chip names
    /// `3V3` or `ADC7` is not told about the RP2350, a divider or GP47.
    #[test]
    fn the_pico2_ice_roles_do_not_leak_onto_other_chips() {
        for generic in ["3V3", "VIN", "ADC7", "PSRAM_CS"] {
            let role = reserved_role(generic);
            assert!(!role.contains("RP2350"), "{generic}: {role}");
            assert!(!role.contains("GP4"), "{generic}: {role}");
        }
        assert!(reserved_role("ADC7 (GP47)").contains("divider"));
        assert!(reserved_role("ADC5/VREF (GP45)").contains("TL431"));
        assert!(reserved_role("PSRAM_CS (GP8)").contains("PSRAM"));
        assert!(reserved_role("ICE_SS (GP5)").contains("flash"));
    }
}
