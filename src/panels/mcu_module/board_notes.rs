//! What each built-in board - or, for a bare chip, its package - does to a
//! pad that the pad's name and functions cannot say: a strapping pin, the
//! flash bus, a divider, an erratum. Shown in the pin panel from
//! [`PinDef::note`](super::mcu_def::PinDef::note).
//!
//! The `.ron` files carry the notes; these tables are where they are written
//! and checked, one sourced fact at a time. Nordic's kits keep theirs beside
//! their pin tables in `codegen::nrf_boards`.
//!
//! ```text
//! cargo test --bin rust_on_chip emit_board_notes -- --ignored --nocapture
//! ```
//! writes every definition listed here to the temp dir with its notes.

use super::mcu_def::{McuDefinition, PinDef};

/// One definition's notes.
pub(crate) struct DefNotes {
    pub id: &'static str,
    /// Appended to every non-reserved pad: a rule of the silicon that holds
    /// for each user GPIO alike. Empty for none.
    pub every_gpio: &'static str,
    /// Non-reserved pads `every_gpio` must skip because they are not the
    /// chip's GPIO at all - a Pico W's LED is the radio's.
    pub not_every: &'static [&'static str],
    /// `(exact pad name, note)`. A name several pads share (`3V3`) gives
    /// each of them the note.
    pub pads: &'static [(&'static str, &'static str)],
}

/// Written by the generator from one verified research pass per board:
/// every fact checked against the vendor's own documents AND against an
/// implementation source (HAL, SDK or board files), wrong ones corrected or
/// dropped.
pub(crate) const TABLES: &[DefNotes] = &[
    DefNotes {
        id: "nrf52833_microbit_v2",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("P0.21 (ROW1)", "Drives the anodes of LED row 1 directly; each LED's cathode returns to a column pin (COL1-5) through 105R. HIGH here with a column LOW lights that LED. On no edge pad."),
            ("P0.22 (ROW2)", "Drives the anodes of LED row 2 directly; each LED's cathode returns to a column pin (COL1-5) through 105R. HIGH here with a column LOW lights that LED. On no edge pad."),
            ("P0.15 (ROW3)", "Drives the anodes of LED row 3 directly; each LED's cathode returns to a column pin (COL1-5) through 105R. HIGH here with a column LOW lights that LED. On no edge pad."),
            ("P0.24 (ROW4)", "Drives the anodes of LED row 4 directly; each LED's cathode returns to a column pin (COL1-5) through 105R. HIGH here with a column LOW lights that LED. On no edge pad."),
            ("P0.19 (ROW5)", "Drives the anodes of LED row 5 directly; each LED's cathode returns to a column pin (COL1-5) through 105R. HIGH here with a column LOW lights that LED. On no edge pad. Nordic rates this pin low-frequency I/O only (up to 10 kHz), so keep faster signals off it."),
            ("P0.00 (SPEAKER)", "Drives speaker SP1 (MLT-8530) via R55 4k7 and C23 100nF into the gate of MOSFET T2 (R17 100k to GND), so it is AC-coupled: only a changing signal such as PWM sounds it. The interface MCU joins ahead of C23 via R54 0R but normally stays high-impedance. Also XL1, and the board has no 32.768 kHz crystal: run LFCLK from RC or synth, not LFXO."),
            ("P0.05 (MIC_IN)", "Output of the analog microphone U4 (Knowles SPU0410LR5H), AC-coupled through C54 1uF and DC-biased by R62 33k / R63 1k. The mic is powered only while MIC_RUN (P0.20) is HIGH."),
            ("P0.20 (MIC_RUN)", "The microphone's power supply: driven HIGH, it feeds U4 through R28 105R and the red mic LED D70 through R58 68R, so D70 lights whenever the mic is on. The schematic asks for high drive - e.g. embassy-nrf OutputDrive::Standard0HighDrive1; the microbit crate uses Disconnect0HighDrive1."),
            ("P1.04 (LOGO touch)", "The gold logo on the front, with a 10M pull-up (R53, through R47 0R) to the 3V rail: as a plain input it reads HIGH, and LOW only while a finger bridges it to GND. The micro:bit runtime (CODAL) senses it capacitively instead. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.08 (INT_SCL)", "Internal I2C clock, on no edge pad, with a strong 1k pull-up (R51). On the bus: LSM303AGR accelerometer 0x19 and magnetometer 0x1E (an FXOS8700CQ at 0x1F on boards fitted with it) and the interface MCU at 0x70 (config) and 0x72 (storage), which may clock-stretch. Test point TP20."),
            ("P0.16 (INT_SDA)", "Internal I2C data, on no edge pad, with a strong 1k pull-up (R52). Same bus as INT_SCL (P0.08): motion sensor 0x19/0x1E (or 0x1F) and the interface MCU at 0x70/0x72 (0x71 reserved). Test point TP21."),
            ("P0.25 (SENSOR_INT)", "Combined open-drain, active-LOW interrupt of the motion sensor and the interface MCU. The board fits no pull-up (only C2 24 pF to GND): enable the nRF's internal pull-up, also while sleeping. Any of them can pull it, so query each over the internal I2C. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.06 (UART_TX)", "nRF52833 TX to the interface MCU, which bridges it to the micro:bit's USB serial port. On no edge pad. The schematic names the net UART_INT_RX, from the interface MCU's side; its design note saying the data flows from the interface MCU to the nRF52833 has the direction backwards."),
            ("P1.08 (UART_RX)", "nRF52833 RX from the interface MCU's USB serial bridge. On no edge pad. The schematic names the net UART_INT_TX, from the interface MCU's side; its design note saying the data flows from the nRF52833 to the interface MCU has the direction backwards."),
            ("P0.02 (ring 0)", "10M pull-up (R9) to the 3V rail for touch sensing: left open it reads HIGH, and a finger bridging it to the GND ring pulls it LOW. For an input that idles LOW, enable the internal pull-down (11-16k), as the micro:bit runtime does by default. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.31 (pad 3, COL3)", "Also LED column 3: the pad joins P0.31 through R11 (105R), which sits in series with your signal, and the pad itself carries the cathodes of the column's five LEDs. Stop the display and keep ROW1-5 LOW: a row left HIGH lights its LED whenever this pad goes LOW. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.28 (pad 4, COL1)", "Also LED column 1: the pad joins P0.28 through R45 (105R), which sits in series with your signal, and the pad itself carries the cathodes of the column's five LEDs. Stop the display and keep ROW1-5 LOW: a row left HIGH lights its LED whenever this pad goes LOW. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.14 (pad 5, BTN_A)", "Button A (SW2) shorts it to GND and R4 10k pulls it up to the 3V rail, so it reads LOW while pressed and needs no internal pull. The schematic allows echoing or faking a press here but not general GPIO use: a HIGH output is shorted to GND while the button is held."),
            ("P1.05 (pad 6, COL4)", "Also LED column 4: the pad joins P1.05 through R48 (105R), which sits in series with your signal, and the pad itself carries the cathodes of the column's five LEDs. Stop the display and keep ROW1-5 LOW: a row left HIGH lights its LED whenever this pad goes LOW. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.11 (pad 7, COL2)", "Also LED column 2: the pad joins P0.11 through R46 (105R), which sits in series with your signal, and the pad itself carries the cathodes of the column's five LEDs. Stop the display and keep ROW1-5 LOW: a row left HIGH lights its LED whenever this pad goes LOW."),
            ("P0.03 (ring 1)", "10M pull-up (R8) to the 3V rail for touch sensing: left open it reads HIGH, and a finger bridging it to the GND ring pulls it LOW. For an input that idles LOW, enable the internal pull-down (11-16k), as the micro:bit runtime does by default. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.10 (pad 8, NFC2)", "NFC2 on the die: GPIO only after UICR.NFCPINS is cleared and the chip reset (erased = NFC, GPIO off). CODAL builds do this; embassy-nrf does with `nfc-pins-as-gpio` (else no P0_10 exists); with nrf-hal write the UICR and reset yourself. Driven opposite to pad 9 the pair leaks up to 10 uA. Nordic rates it low-frequency I/O only (up to 10 kHz)."),
            ("P0.09 (pad 9, NFC1)", "NFC1 on the die: GPIO only after UICR.NFCPINS is cleared and the chip reset (erased = NFC, GPIO off). CODAL builds do this; embassy-nrf does with `nfc-pins-as-gpio` (else no P0_09 exists); with nrf-hal write the UICR and reset yourself. Driven opposite to pad 8 the pair leaks up to 10 uA. Nordic rates it low-frequency I/O only (up to 10 kHz)."),
            ("P0.30 (pad 10, COL5)", "The edge pad is net COLR5, the cathodes of LED column 5's five LEDs, and reaches P0.30 only through R49 105R in series. Keep the display off and ROW1-5 from going HIGH: a row driven HIGH lights its LED whenever this pad goes LOW. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.23 (pad 11, BTN_B)", "Button B (SW3) shorts it to GND and R5 10k pulls it up to the 3V rail, so it reads LOW while pressed and needs no internal pull. The schematic allows echoing or faking a press here but not general GPIO use: a HIGH output is shorted to GND while the button is held. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.04 (ring 2)", "10M pull-up (R7) to the 3V rail for touch sensing: left open it reads HIGH, and a finger bridging it to the GND ring pulls it LOW. For an input that idles LOW, enable the internal pull-down (11-16k), as the micro:bit runtime does by default."),
            ("P0.17 (pad 13, SCK)", "For SPIM0-2 at 8 Mbps the nRF52833 PS (aQFN73) recommends SCK on P0.27, P1.08, P0.04 or P1.09 - not this pin; of those only P0.04 (ring 2) reaches the edge connector."),
            ("P1.02 (pad 16)", "A free pin: its schematic net (GPIO3) goes only to the edge connector. Nordic rates this pin low-frequency I/O only (up to 10 kHz)."),
            ("P0.26 (pad 19, SCL)", "External I2C clock with a 4k7 pull-up (R2) to the 3V rail, sized for micro:bit V1 compatibility. On V2 the motion sensor and interface MCU sit on the separate internal bus, so nothing on the board answers here. As a GPIO it idles HIGH through the pull-up."),
            ("P1.00 (pad 20, SDA)", "External I2C data with a 4k7 pull-up (R1) to the 3V rail, sized for micro:bit V1 compatibility. On V2 the motion sensor and interface MCU sit on the separate internal bus, so nothing on the board answers here. As a GPIO it idles HIGH through the pull-up."),
        ],
    },
    DefNotes {
        id: "rp2040_pico",
        every_gpio: "The Pico runs all I/O at 3.3 V (IOVDD = 3V3): the absolute maximum on any GPIO is IOVDD + 0.5 V, about 3.8 V, so no pin is 5 V tolerant. All GPIO and QSPI pins together may source at most 50 mA and sink at most 50 mA.",
        not_every: &[],
        pads: &[
            ("GP23 (SMPS mode)", "Not on the 40-pin header (only test point TP4, marked do-not-use): it drives the RT6150 regulator's PS pin, held low by R8 (100k). Low = PFM, the default, best efficiency; HIGH = forced PWM: much less 3V3 ripple at light load, so less ripple on the ADC supply, but much worse light-load efficiency. A peripheral routed here only toggles that mode."),
            ("GP24 (VBUS sense)", "Not on the 40-pin header: VBUS through R10 (5.6k) with R1 (10k) to GND, about 3.2 V from 5 V, so it reads HIGH whenever VBUS is present (USB plugged in, or 5 V fed to pin 40) and LOW otherwise. Use it as a plain input; a peripheral routed here has nothing to connect to."),
            ("GP25 (on-board LED)", "Not on the 40-pin header: drives the green LED D2 through R3 (470R) to GND, so HIGH lights it. Test point TP5, between R3 and the LED, only swings from 0 V to the LED's forward voltage, so it is not recommended as a spare I/O."),
            ("GP29 (VSYS sense)", "Not on the 40-pin header: ADC3 reads VSYS/3 from R5 (200k) / R6 (100k) with C3 (1 nF), so multiply by 3. The divider reaches the pin through N-FET Q1 (gate on 3V3), which switches off when 3V3 is off so VSYS cannot leak into the 3V3 rail through this pin's ADC diode."),
            ("VBUS", "Loaded by R10 (5.6k) + R1 (10k) to GND, the divider that feeds GP24, so it reads 0 V without USB. Feeds VSYS through Schottky D1 (MBR120VLSFT1G). In USB host mode, power the board by feeding 5 V into this pin; with USB as the only supply, VBUS may be shorted to VSYS to remove the diode drop."),
            ("VSYS", "With USB it sits one Schottky drop (D1) below VBUS. Add another supply through its own Schottky diode or a P-FET (e.g. DMG2305UX, gate on VBUS) so neither source back-powers the other. VSYS/3 is on GP29 (ADC3), through R5 (200k) / R6 (100k)."),
            ("3V3_EN", "Its pull-up is R2 (100k) to VSYS, not to 3V3, so released it sits at VSYS (up to 5.5 V). Grounding it disables the RT6150 and puts it in a low-power state; FET Q1 then stops VSYS leaking into 3V3 through GP29's ADC diode."),
            ("ADC_VREF", "3V3 reaches it through R7 (200R, 0603) and goes on to the ADC supply through R9 (1R) into C13 (2.2 uF); the ADC's ~150 uA draw leaves a ~30 mV offset. A 3.0 V shunt reference here (e.g. LM4040) gives a cleaner 0-3.0 V range and draws ~1.5 mA through R7; remove R7 to supply it from elsewhere."),
            ("GP28", "GP26-29, the ADC pins, have a diode to 3V3: keep the input below 3V3 + about 0.3 V, and a voltage applied while the board is unpowered leaks into the 3V3 rail (GP0-22 do not leak this way). Erratum RP2040-E11 (all steppings): DNL error peaks at codes 512, 1536, 2560 and 3584. Ground reference: AGND (pin 33), with its own analog ground plane."),
            ("GP27", "As an ADC pin it has a diode to 3V3 (the digital-only GP0-25 do not): keep the input at or below 3V3, and a voltage applied while the board is unpowered leaks into the 3V3 rail. Erratum RP2040-E11 (B0-B2, not fixed): ADC DNL spikes at codes 512, 1536, 2560 and 3584. Ground reference: AGND, pin 33."),
            ("GP26", "As an ADC pin it has a diode to 3V3 (the digital-only GP0-25 do not): keep the input at or below 3V3, and a voltage applied while the board is unpowered leaks into the 3V3 rail. Erratum RP2040-E11 (B0-B2, not fixed): ADC DNL spikes at codes 512, 1536, 2560 and 3584. Ground reference: AGND, pin 33."),
            ("RUN", "Held high only by the RP2040's on-chip pull-up (about 50k to 3.3 V) - the board adds nothing on this line, so a push button from RUN to GND is a complete reset button."),
        ],
    },
    DefNotes {
        id: "rp2350_pico2",
        every_gpio: "RP2350 A2 silicon (chip marking ends in A2), erratum E9: an input pad whose voltage sits between VIL and VIH is held near 2.2 V by ~120 uA of leakage, which the internal pull-down cannot overcome. Use an external pull-down of 8.2k or less, or enable the pad's input only while reading it. Pico 2 production moved to A3/A4, which fix it, in July 2025.",
        not_every: &[],
        pads: &[
            ("GP23 (SMPS mode)", "Not on the 40-pin header (only test point TP4, marked do-not-use): it drives the RT6150 regulator's PS pin, held low by R8 (100k). Low = PFM, the default and most efficient; HIGH = forced PWM, much less 3V3 ripple at light load (the datasheet's tip for ADC readings) but much worse light-load efficiency. Any function here only toggles that mode."),
            ("GP24 (VBUS sense)", "Not on the 40-pin header: VBUS through R10 (5.6k) with R1 (10k) to GND, about 3.2 V from 5 V, so it reads HIGH while VBUS is present (USB plugged in, or 5 V fed into pin 40) and LOW otherwise. Use it as an input; the peripheral functions listed here have no wire to reach."),
            ("GP25 (on-board LED)", "Not on the 40-pin header: drives the green LED D2 through R3 (470R) to GND, so HIGH lights it. Test point TP5 sits between R3 and the LED; it only swings from 0 V to the LED's forward voltage, so the datasheet advises against using it."),
            ("GP29 (VSYS sense)", "Not on the 40-pin header: ADC3 reads VSYS/3 (R5 100k on top; R6 100k and, through FET Q1, R16 100k in parallel below; C3 1 nF), so multiply by 3. Q1's gate is on 3V3, so with 3V3 off (3V3_EN low) it isolates the pin and VSYS cannot leak through the ADC diode into 3V3."),
            ("VBUS", "Loaded by R10 (5.6k) + R1 (10k) to GND, the divider that feeds GP24, so it reads 0 V without USB. Feeds VSYS through Schottky D1 (PMEG6010ELR). In USB host mode, power the board by feeding 5 V into this pin; with USB as the only supply, VBUS may be shorted to VSYS to remove the diode drop."),
            ("VSYS", "With USB it sits one Schottky drop (D1) below VBUS. Add another supply through its own Schottky diode or a P-FET (e.g. DMG2305UX, gate on VBUS) so neither source back-powers the other. VSYS/3 is on GP29 (ADC3)."),
            ("3V3_EN", "Its pull-up is R2 (100k) to VSYS, not to 3V3, so released it sits at VSYS (up to 5.5 V). Grounding it disables the RT6150 regulator (U2) and puts it in a low-power state; FET Q1 then stops VSYS leaking into 3V3 through GP29's ADC diode."),
            ("ADC_VREF", "3V3 reaches it through R7 (200R) and goes on to the ADC supply through R9 (1R) into C13; the ADC's ~150 uA draw leaves a ~30 mV offset. A 3.0 V shunt reference here (e.g. LM4040) gives a cleaner 0-3.0 V range and draws ~1.5 mA through R7; remove R7 to supply it from elsewhere."),
            ("GP28", "ADC2. Not 5 V tolerant, unlike GP0-22 (5.5 V while 3V3 is up): this pin has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while 3V3 is off leaks through it into the 3V3 rail. Analog ground is AGND, pin 33."),
            ("GP27", "ADC1. Not 5 V tolerant, unlike GP0-22 (5.5 V while 3V3 is up): this pin has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while 3V3 is off leaks through it into the 3V3 rail. Analog ground is AGND, pin 33."),
            ("GP26", "ADC0. Not 5 V tolerant, unlike GP0-22 (5.5 V while 3V3 is up): this pin has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while 3V3 is off leaks through it into the 3V3 rail. Analog ground is AGND, pin 33."),
            ("RUN", "Held high only by the RP2350's on-chip pull-up (about 50k to 3.3 V); the board adds nothing on this line. Holding RUN low is no power-off: the board still draws a typical 388 uA from VBUS, more than in the P1.7 low-power state (148 uA). To de-power the RP2350, pull 3V3_EN low instead."),
        ],
    },
    DefNotes {
        id: "rp2040_pico_w",
        every_gpio: "GP pins are RP2040 GPIO, 3.3 V only - the absolute maximum on any of them is IOVDD + 0.5 V, so none is 5 V tolerant. All GPIO and QSPI pins together may source at most 50 mA and sink at most 50 mA.",
        not_every: &["WL_LED"],
        pads: &[
            ("WL_ON", "Drives both WL_REG_ON and BT_REG_ON of the CYW43439, so Wi-Fi and Bluetooth power up together. The cyw43 driver holds it low 20 ms, then high, and waits 250 ms before its first SPI access - start-up time that any radio use, even the LED, adds."),
            ("WL_D", "Also the radio's IRQ. On the CYW43439 it joins SDIO_CMD (gSPI data in), SDIO_DATA0 (data out) via R87 470R, SDIO_DATA1 (IRQ) via R88 10k, and SDIO_DATA2 directly - a strap sampled at power-up that selects gSPI when low, so GP24 must be low while WL_ON brings the radio up. IRQs can only be checked between SPI transactions."),
            ("WL_CS", "HIGH also turns on FET Q1 (DMG1012T), which connects VSYS through R5 (20k) to GP29, where R6 (10k) goes to GND - this is how VSYS/3 reaches ADC3. The radio drivers keep CS high whenever no transaction is running."),
            ("WL_CLK", "R6 (10k) to GND loads this line. VSYS/3 (ADC3) is readable only between radio transactions, with GP25 high and GP29 moved from the PIO to the ADC: pico_w.h says to wrap the read in cyw43_thread_enter/exit, and pico-sdk's driver re-selects the PIO at the next transaction. cyw43-pio 0.10's PioSpi has no call that lends GP29 out."),
            ("WL_LED", "Green LED D2 on the radio's WL_GPIO0, through R3 (470R) to GND. Only the CYW43439 can drive it: cyw43 first power-cycles the radio and downloads its ~226 KiB firmware (43439A0.bin, kept in flash), then each Control::gpio_set(0, on) is a command over the radio's SPI. Test point TP5 only swings up to the LED's forward voltage."),
            ("VBUS", "Loaded by R10 (10k) + R1 (10k) to GND, so it reads 0 V without USB. Their midpoint is the radio's WL_GPIO2, not an RP2040 pin, so sensing USB power needs the radio running - and the cyw43 crate (0.7) can set radio GPIOs but has no call to read one. For USB host mode, power the board with 5 V into this pin."),
            ("VSYS", "With USB it sits one Schottky drop (D1, MBR120VLSFT1G) below VBUS. Add another supply through its own Schottky diode or a P-FET (e.g. DMG2305UX, gate on VBUS) so neither source back-powers the other. VSYS/3 reaches GP29 (ADC3) through R5 (20k) / R6 (10k) only while GP25 (WL_CS) is high and no wireless transfer is in progress."),
            ("3V3_EN", "Its pull-up is R2 (100k) to VSYS, not to 3V3, so released it sits at VSYS (up to 5.5 V). Grounding it stops the RT6154, and with it the CYW43439, whose supply is 3V3 through ferrite FB8; FET Q1 keeps VSYS from leaking into 3V3 through GP29's ADC diode."),
            ("ADC_VREF", "3V3 reaches it through R7 (200R, 0603), then the ADC supply through R9 (1R) into C13 (2.2 uF); the ADC's ~150 uA leaves a ~30 mV offset. Less ripple: force the SMPS into PWM mode - on a W board its PS pin is the radio's WL_GPIO1 (R8 100k holds it low), so that is cyw43 Control::gpio_set(1, true).await, at a cost in light-load efficiency."),
            ("GP28", "ADC2. The ADC pins have a diode to 3V3: keep the input below 3V3 + 0.3 V, and a voltage applied while the board is unpowered leaks into the 3V3 rail (GP0-22 do not). Erratum RP2040-E11 (all steppings): DNL spikes at codes 512, 1536, 2560 and 3584. Analog ground is AGND, pin 33."),
            ("GP27", "ADC1. The ADC pins have a diode to 3V3: keep the input below 3V3 + 0.3 V, and a voltage applied while the board is unpowered leaks into the 3V3 rail (GP0-22 do not). Erratum RP2040-E11 (all steppings): DNL spikes at codes 512, 1536, 2560 and 3584. Analog ground is AGND, pin 33."),
            ("GP26", "GP26-28 are the ADC pins and have a diode to 3V3: keep the input below about 3V3 + 0.3 V, and a voltage applied while the board is unpowered leaks into the 3V3 rail (GP0-22 do not). Erratum RP2040-E11 (all steppings): DNL spikes at codes 512, 1536, 2560 and 3584. Analog ground is AGND, pin 33."),
            ("RUN", "Held high only by the RP2040's on-chip pull-up (about 50k to 3.3 V) - the board adds nothing on this line, so a push button from RUN to GND is a complete reset button."),
        ],
    },
    DefNotes {
        id: "rp2350_pico2_w",
        every_gpio: "RP2350 GPIO: on boards whose chip is marked A2 (PCN 32 moved Pico 2 W to A4 from July 2025), erratum RP2350-E9 makes an input left between the logic levels source ~120 uA and stick near 2.2 V, too much for the internal pull-down. Use an external pull-down of 8.2k or less; the pull-up works.",
        not_every: &["WL_LED"],
        pads: &[
            ("WL_ON", "Drives both WL_REG_ON and BT_REG_ON of the CYW43439, so Wi-Fi and Bluetooth power up together. The cyw43 driver holds it low 20 ms, then high, and waits 250 ms before its first SPI access - start-up time that any radio use, even the LED, adds."),
            ("WL_D", "Also the radio's interrupt line: it is SDIO_CMD on the CYW43439, tied straight to SDIO_DATA2, to SDIO_DATA0 through R22 (470R) and to SDIO_DATA1 through R23 (10k). Data in, data out and IRQ share this one GPIO, so the IRQ is only checked between SPI transactions."),
            ("WL_CS", "HIGH also turns on FET Q1 (DMG1012T), which connects VSYS through R5 (20k) to GP29, where R6 (10k) goes to GND - this is how VSYS/3 reaches ADC3. The radio drivers keep CS high whenever no transaction is running."),
            ("WL_CLK", "R6 (10k) to GND loads this line. VSYS/3 is readable on it (ADC3) only between radio transactions, with GP25 high and GP29 released from the PIO: pico-sdk code wraps the read in cyw43_thread_enter/exit and calls adc_gpio_init(29), and the SDK driver retakes the pin at its next transaction. cyw43-pio 0.10's PioSpi has no call that lends GP29 out."),
            ("WL_LED", "Green LED D2 on the radio's WL_GPIO0, through R3 (470R) to GND. Only the CYW43439 can drive it: cyw43 first power-cycles the radio and downloads its ~226 KiB firmware (43439A0.bin, kept in flash), then each Control::gpio_set(0, on) is a command over the radio's SPI. Test point TP5 only swings up to the LED's forward voltage."),
            ("VBUS", "Loaded by the R10/R1 divider to GND, so it reads 0 V without USB. Its midpoint is the radio's WL_GPIO2, not an RP2350 pin, so sensing USB power needs the radio running - and the cyw43 crate (0.7) can set radio GPIOs but has no call to read one. For USB host mode, power the board with 5 V into this pin."),
            ("VSYS", "With USB it sits one Schottky drop (D1) below VBUS. Add another supply through its own Schottky diode or a P-FET (e.g. DMG2305UX, gate on VBUS) so neither source back-powers the other. VSYS/3 reaches ADC3 on GP29 (WL_CLK) through R5 (20k) / R6 (10k) only while GP25 (WL_CS) is high."),
            ("3V3_EN", "Its pull-up is R2 (100k) to VSYS, not to 3V3, so released it sits at VSYS (up to 5.5 V). Grounding it stops the RT6154, and with it the CYW43439, whose supply is 3V3 through ferrite FB1; FET Q1 keeps VSYS from leaking into 3V3 through GP29's ADC diode."),
            ("ADC_VREF", "Fed from 3V3 through R7 (200R); R9 (1R) then carries it to the ADC supply and C13. The ADC's ~150 uA through R7 leaves a ~30 mV offset. For less ripple, force the SMPS into PWM mode: its PS pin is the radio's WL_GPIO1 (R8 100k holds it low), so cyw43 control.gpio_set(1, true).await - at a cost in light-load efficiency."),
            ("GP28", "Not 5 V tolerant, unlike GP0-22 (5.5 V while 3V3 is up): as an ADC pin it has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while 3V3 is off leaks into the 3V3 rail. Analog ground is AGND, pin 33."),
            ("GP27", "Not 5 V tolerant, unlike GP0-22 (5.5 V while 3V3 is up): as an ADC pin it has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while 3V3 is off leaks into the 3V3 rail. Analog ground is AGND, pin 33."),
            ("GP26", "Not 5 V tolerant, unlike GP0-22 (up to 5.5 V while 3V3 is up): this pin has a diode to 3V3, so keep it below 3V3 + 0.3 V, and a voltage applied while the board is unpowered leaks into the 3V3 rail. Its analog ground is AGND, pin 33."),
            ("RUN", "Held high only by the RP2350's on-chip pull-up (about 50k to 3.3 V) - the board adds nothing on this line, so a push button from RUN to GND is all a reset button needs."),
        ],
    },
    DefNotes {
        id: "rp2350_pico2_ice",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GP0 (LED G, active LOW)", "Wired only to RGB LED D2's green cathode through 1k (R18): no header or connector reaches GP0, and the FPGA's LED is a separate one (D4). D2 is common anode, fed from 3V3 through bridged solder jumper SJ1 on the back; cutting SJ1 darkens all three colours (GP0, GP1, GP9) at once."),
            ("GP1 (LED R, active LOW)", "Wired only to RGB LED D2's red cathode through 1k (R17): no header or connector reaches GP1, and the FPGA's LED is a separate one (D4). D2 is common anode, fed from 3V3 through bridged solder jumper SJ1 on the back; cutting SJ1 darkens all three colours (GP0, GP1, GP9) at once."),
            ("GP9 (LED B, active LOW)", "Wired only to RGB LED D2's blue cathode through 1k (R19): no header or connector reaches GP9, and the FPGA's LED is a separate one (D4). D2 is common anode, fed from 3V3 through bridged solder jumper SJ1 on the back; cutting SJ1 darkens all three colours (GP0, GP1, GP9) at once."),
            ("GP10 (FFC)", "Only on the 22-pin 0.5 mm FPC connector J6, pin 14. Unlike GP12..GP19 beside it, not an HSTX pin."),
            ("GP11 (FFC)", "Only on the 22-pin 0.5 mm FPC connector J6, pin 15. Unlike GP12..GP19 beside it, not an HSTX pin."),
            ("GP12 (FFC)", "Only on FPC connector J6, pin 2, paired with GP13 on pin 3; the current Rev2 design names GP12 Lane0_N and GP13 Lane0_P. GP12..GP19, the RP2350's HSTX pins, reach J6 as three data lanes plus a clock pair on GP16/GP17."),
            ("GP13 (FFC)", "Only on FPC connector J6, pin 3, paired with GP12 on pin 2; the current Rev2 design names GP13 Lane0_P and GP12 Lane0_N. GP12..GP19, the RP2350's HSTX pins, reach J6 as three data lanes plus a clock pair on GP16/GP17."),
            ("GP14 (FFC)", "Only on FPC connector J6, pin 5: HSTX pair Lane1_N, with GP15 as Lane1_P. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus a clock pair on GP16/GP17; HSTX's per-pin BITx registers choose which pin drives the clock, so firmware must follow this naming."),
            ("GP15 (FFC)", "Only on FPC connector J6, pin 6: HSTX pair Lane1_P, with GP14 as Lane1_N. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus a clock pair on GP16/GP17; HSTX's per-pin BITx registers choose which pin drives the clock, so firmware must follow this naming."),
            ("GP16 (FFC)", "Only on FPC connector J6, pin 8: HSTX clock pair Clock_N, with GP17 as Clock_P. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus this clock pair; HSTX's per-pin BITx registers choose which pin drives the clock, so set the CLK bit for GP16/GP17 to match."),
            ("GP17 (FFC)", "Only on FPC connector J6, pin 9: HSTX clock pair Clock_P, with GP16 as Clock_N. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus this clock pair; HSTX's per-pin BITx registers choose which pin drives the clock, so set the CLK bit for GP16/GP17 to match."),
            ("GP18 (FFC)", "Only on FPC connector J6, pin 11: HSTX pair Lane2_N, with GP19 as Lane2_P. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus a clock pair on GP16/GP17; HSTX's per-pin BITx registers choose which pin drives the clock, so firmware must follow this naming."),
            ("GP19 (FFC)", "Only on FPC connector J6, pin 12: HSTX pair Lane2_P, with GP18 as Lane2_N. J6 carries GP12..GP19, the RP2350's HSTX pins, as three data lanes plus a clock pair on GP16/GP17; HSTX's per-pin BITx registers choose which pin drives the clock, so firmware must follow this naming."),
            ("3V3_FPGA", "Joined to the main 3V3 by bridged solder jumper SJ6. Cut SJ6 to switch the FPGA off or feed this rail from outside, e.g. through a load switch the RP2350 controls (schematic FPGA Power Note)."),
            ("GP29 (ICE11)", "Same net as iCE40 pin 11, named DEFAULT_UART_TX / DEFAULT_I2C_SCL in pico2_ice.pcf (this pad's UART0 RX / I2C0 SCL). Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad takes 5 V with IOVDD powered) and never let gateware drive pin 11 while the RP2350 does. Until the FPGA is configured, pin 11 holds a weak pull-up (11-128 uA)."),
            ("GP28 (ICE9)", "Same net as iCE40 pin 9, named DEFAULT_UART_RX / DEFAULT_I2C_SDA in pico2_ice.pcf (this pad's UART0 TX / I2C0 SDA). Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad takes 5 V with IOVDD powered) and never let gateware drive pin 9 while the RP2350 does. Until the FPGA is configured, pin 9 holds a weak pull-up (11-128 uA)."),
            ("GP2 (SDA)", "10k pull-up to 3V3 on the board (R13). The same net goes to FPC connector J6 pin 21, so a device on the flex cable shares this I2C bus."),
            ("RUN", "10k pull-up (R16). The board has no reset button, so this pad is where a reset switch goes."),
            ("GP3 (SCL)", "10k pull-up to 3V3 on the board (R23). The same net goes to FPC connector J6 pin 20, so a device on the flex cable shares this I2C bus."),
            ("BOOTSEL", "Net ~USB_BOOT: button SW1 to GND with a 10k pull-up (R32), joined to the flash chip select QSPI_SS through Schottky D6 and 1k (R2). Hold SW1, or short this pad (the docs' BT pin) to GND, while powering up to get the RP2350's USB drive. GP42 reads the same net through 5.1k."),
            ("ADC7 (GP47)", "Sized for one lithium cell (schematic note): about 4 V here gives 3.3 V at GP47. R29 (the 2.2k) also limits the current if, say, 12 V lands here; for a higher voltage add a resistor in series with it."),
            ("GP30 (ICE25)", "Same net as iCE40 pin 25. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad takes 5 V with IOVDD powered) and never let gateware drive pin 25 while the RP2350 does. Until the FPGA is configured, pin 25 holds a weak pull-up (11-128 uA)."),
            ("GP25 (ICE23)", "Same net as iCE40 pin 23. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 23 while the RP2350 does. Until the FPGA is configured, pin 23 holds a weak pull-up (11-128 uA)."),
            ("GP23 (ICE19)", "Same net as iCE40 pin 19 and FPC connector J6 pin 18. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 19 while the RP2350 does. Until the FPGA is configured, pin 19 holds a weak pull-up (11-128 uA)."),
            ("GP27 (ICE18)", "Same net as iCE40 pin 18. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad takes 5 V with IOVDD powered) and never let gateware drive pin 18 while the RP2350 does. Until the FPGA is configured, pin 18 holds a weak pull-up (11-128 uA)."),
            ("GP20 (ICE27)", "Same net as iCE40 pin 27 and FPC connector J6 pin 17. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 27 while the RP2350 does. Until the FPGA is configured, pin 27 holds a weak pull-up (11-128 uA)."),
            ("GP24 (ICE26)", "Same net as iCE40 pin 26. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 26 while the RP2350 does. Until the FPGA is configured, pin 26 holds a weak pull-up (11-128 uA)."),
            ("GP26 (ICE21)", "Same net as iCE40 pin 21. Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 21 while the RP2350 does. Until the FPGA is configured, pin 21 holds a weak pull-up (11-128 uA)."),
            ("GP22 (ICE20)", "Reaches iCE40 pin 20, a global-buffer clock input (G3), through 27 R (R34). Keep it at 3.3 V logic (FPGA pin absolute max 3.6 V, though the RP2350 pad alone takes 5 V while IOVDD is powered) and never let gateware drive pin 20 while the RP2350 does. Until the FPGA is configured, pin 20 holds a weak pull-up (11-128 uA)."),
            ("GP41 (ADC1)", "Not 5 V tolerant: GP40..GP47 are the RP2350B's ADC pads, standard rather than fault-tolerant (FT) I/O, so the absolute maximum is IOVDD + 0.5 V, which is 3.8 V on this 3.3 V board. Nothing else on the board is wired to this header pin."),
            ("GP42 (ADC2/SW1)", "Reads button SW1 through 5.1k (R33): LOW while pressed, 10k pull-up (R32). SW1 is also the BOOTSEL button (net ~USB_BOOT, through Schottky D6 and 1k to QSPI_SS), so any function picked here still sees that pull-up and the button through R33. ADC pad: not 5 V tolerant."),
            ("3V3", "Output of a 300 mA NCP115 LDO (U9) fed from VIN. It feeds the RP2350's I/O, QSPI, USB and analog supplies (its core regulator input VREG_VIN takes VIN directly), the flash, the PSRAM, FPC J6 pin 22 and, through SJ6, the FPGA - the header gets what they leave."),
            ("VBUS", "Feeds VIN through Schottky D5 and 350 mA polyfuse PTC1. Each USB-C CC pin has a 5.1k pull-down (device mode) through 3-pad solder jumpers JP1/JP2. For host mode cut their 1-2 trace and bridge 2-3 for 56k pull-ups to VBUS; the 5 V must then come in on this pad - D5 only conducts toward VIN."),
            ("VIN", "Also the RP2350's core-regulator input VREG_VIN and the input of the 3.3 V LDO U9 (NCP115), both specified to 5.5 V max - stay at or below it. USB VBUS reaches this rail through Schottky D5 and 350 mA polyfuse PTC1; D5 stops a supply here from flowing back into USB."),
            ("GP43 (ADC3)", "Not 5 V tolerant: GP40..GP47 are the RP2350B's ADC pads, standard rather than fault-tolerant (FT) I/O, so the absolute maximum is IOVDD + 0.5 V, which is 3.8 V on this 3.3 V board. Nothing else on the board is wired to this header pin."),
            ("GP44 (ADC4)", "Not 5 V tolerant: GP40..GP47 are the RP2350B's ADC pads, standard rather than fault-tolerant (FT) I/O, so the absolute maximum is IOVDD + 0.5 V, which is 3.8 V on this 3.3 V board. Nothing else on the board is wired to this header pin."),
            ("GP46 (ADC6/VREF_EN)", "Feeds the TL431 2.5 V reference through bridged jumper R31 and 1k (R12): set it HIGH when GP45 should read the reference, e.g. to calibrate the ADC. While R31 is bridged, anything above ~2.5 V on this header pin is loaded by the shunt - cut R31 to free it. ADC pad: not 5 V tolerant."),
        ],
    },
    DefNotes {
        id: "esp32",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("CAP1", "Takes a 10 nF capacitor to GND, 10% tolerance - required for the chip to work."),
            ("CAP2", "Takes a 3.3 nF capacitor in parallel with a 20 kohm resistor to CAP1. That RC only shortens the drop of the internal 1.1 V supply to about 0.7 V on entering Deep-sleep; without Deep-sleep, or if Deep-sleep current matters little, it may be left out."),
            ("GPIO1", "UART0 TX after reset: the ROM prints its boot log here (unless MTDO/GPIO15 is low at reset), and UART0 on GPIO1/GPIO3 is the usual flashing and log port. Espressif suggests a 499 R series resistor on it and another UART for application traffic."),
            ("GPIO3", "UART0 RX after reset: the ROM serial bootloader receives the firmware here when flashing over UART (TX is GPIO1). Espressif suggests keeping UART0 for download and logs and using another UART for the application."),
            ("VDD3P3_CPU", "Supplies the I/O of GPIO1, 3, 5, 18, 19, 21, 22 and 23 and the CPU. It accepts 1.8-3.6 V where VDDA, VDD3P3 and VDD3P3_RTC need at least 2.3 V, so these eight pads can run at a lower I/O voltage than the RTC-domain pads."),
            ("GPIO33", "32K_XN: one of the two pads of the optional 32.768 kHz RTC crystal (GPIO32 is the other). With the crystal fitted and enabled as the RTC clock, both pads are switched to the RTC oscillator and neither is free; without it both are plain GPIOs."),
            ("GPIO25", "ADC2 channel: unusable while Wi-Fi is on, and with esp-hal also while BLE runs - Adc::new on ADC2 panics once the radio is up. Take an ADC1 pad (GPIO32-39) for readings with the radio."),
            ("GPIO26", "ADC2 channel: unusable while Wi-Fi is on, and with esp-hal also while BLE runs - Adc::new on ADC2 panics once the radio is up. Take an ADC1 pad (GPIO32-39) for readings with the radio."),
            ("GPIO27", "ADC2 channel: unusable while Wi-Fi is on, and with esp-hal also while BLE runs - Adc::new on ADC2 panics once the radio is up. Take an ADC1 pad (GPIO32-39) for readings with the radio."),
            ("GPIO14", "JTAG TMS after reset (MTMS), part of the GPIO12-15 port an external JTAG debugger uses; it comes out of reset with its pull-up on. ADC2: unusable while Wi-Fi is on, and with esp-hal also while BLE runs - Adc::new on ADC2 panics once the radio is up."),
            ("GPIO12", "Strapping pin (MTDI), internal pull-down: HIGH at reset switches VDD_SDIO, the flash supply, to 1.8 V, and a 3.3 V flash can brown out so flashing and/or booting fail. With 3.3 V flash keep it low at reset, or burn espefuse set-flash-voltage 3.3V (permanent) so it is ignored. JTAG TDI after reset. ADC2: unusable while Wi-Fi is on."),
            ("VDD3P3_RTC", "Supplies the RTC-domain pads (GPIO0, 2, 4, 12-15, 25-27, 32-39), the only GPIOs still controllable in Deep-sleep (other pads can at most hold a latched level), and feeds VDD_SDIO. 2.3-3.6 V; keep it above 3.0 V when VDD_SDIO powers 3.3 V flash/PSRAM. It cannot be powered alone: bring all supplies up together."),
            ("GPIO13", "JTAG TCK after reset (MTCK): with GPIO12, 14 and 15 it is the port an external JTAG debugger uses, so a project debugged over JTAG leaves these four free. ADC2: unusable while Wi-Fi is on."),
            ("GPIO15", "Strapping pin (MTDO), internal pull-up: LOW at reset silences the ROM's boot log on U0TXD (GPIO1); with GPIO5 it also sets the SDIO slave timing. JTAG TDO after reset. ADC2: unusable while Wi-Fi is on, and with esp-hal also while BLE runs."),
            ("GPIO2", "Strapping pin, internal pull-down: it must be LOW or floating at reset for a LOW GPIO0 to start the serial bootloader; with GPIO0 high its level is ignored. A pull-up here at reset (e.g. an SD card's DAT0 pull-up) blocks download mode. ADC2: unusable while Wi-Fi is on (esp-hal also locks it while Bluetooth runs)."),
            ("GPIO0", "Strapping pin, internal pull-up (about 45k): LOW at reset, with GPIO2 low, starts the ROM serial bootloader instead of the flash program - this is where a BOOT button and esptool's DTR auto-reset line connect. Keep large capacitors and power-up pull-downs off it. ADC2: unusable while Wi-Fi is on (esp-hal also locks it while Bluetooth runs)."),
            ("GPIO4", "ADC2 channel: unusable while Wi-Fi is on, and esp-hal also refuses ADC2 while Bluetooth runs - take an ADC1 pad (GPIO32-39) for readings with the radio on."),
            ("LNA_IN", "Once Wi-Fi or Bluetooth is started an antenna must be connected - Espressif warns that running without one can be unstable or damage the RF circuit. If the radio is never initialised the pin may be left floating."),
            ("VDD3P3", "Analog power (2.3-3.6 V), not a digital rail. Its current can jump when the radio transmits and collapse the rail, so Espressif highly recommends a 10 uF capacitor on it, plus an LC filter against high-frequency harmonics (inductor rated preferably 500 mA or more)."),
            ("GPIO36", "Input only, no internal pull-up or pull-down (add an external resistor). Erratum GPIO-3.11: powering SAR ADC1, SAR ADC2 or the AMP pulls this input low for about 80 ns, so do not use its interrupt with the ADC, or with Wi-Fi/Bluetooth in sleep mode."),
            ("GPIO37", "Input only: no output driver and no internal pull-up or pull-down, so a button or open-drain signal here needs an external resistor."),
            ("GPIO38", "Input only: no output driver and no internal pull-up or pull-down, so a button or open-drain signal here needs an external resistor."),
            ("GPIO39", "Input only, no internal pull-up or pull-down (add an external resistor). Erratum GPIO-3.11: powering SAR ADC1, SAR ADC2 or the AMP pulls this input low for about 80 ns, so do not use its interrupt with the ADC, or with Wi-Fi/Bluetooth in sleep mode."),
            ("CHIP_PU", "Must not float. Espressif recommends an RC delay (R = 10k, C = 1 uF) so it goes high at least 50 us after the 3.3 V rails; at or below 0.6 V the chip is held off. On slow or unstable supplies add a supervisor resetting near 3.0 V. Where esptool's auto-reset is wired, the USB-UART's RTS drives it (DTR drives GPIO0)."),
            ("GPIO34", "Input only: no output driver and no internal pull-up or pull-down, so a button or open-drain signal here needs an external resistor."),
            ("GPIO35", "Input only: no output driver and no internal pull-up or pull-down, so a button or open-drain signal here needs an external resistor."),
            ("GPIO32", "32K_XP: one of the two pads of the optional 32.768 kHz RTC crystal (GPIO33 is the other). With the crystal fitted and enabled as the RTC clock, both pads are switched to the RTC oscillator and neither is free; without it both are plain GPIOs."),
            ("GPIO5", "Strapping pin, internal pull-up. Its only strapping role is the SDIO slave timing: with MTDO (GPIO15) it picks the slave's sampling and output edges at reset (default: rising, rising)."),
            ("GPIO8", "Flash data (SD_DATA_1). An ESP32 without in-package flash runs from an off-package SPI flash, by default on GPIO6-11, where this pad is IO0/DI. It is also IO0/DI of the in-package flash on ESP32-U4WDH and SIO0/SI of the in-package PSRAM on ESP32-D0WDRH2-V3. Espressif: not recommended for any other use."),
            ("GPIO7", "Flash data (SD_DATA_0). An ESP32 without in-package flash runs from an off-package SPI flash, by default on GPIO6-11, where this pad is IO1/DO. It is IO2/WP# of the in-package flash on ESP32-U4WDH and SIO1/SO of the in-package PSRAM on ESP32-D0WDRH2-V3. Espressif: not recommended for any other use."),
            ("GPIO6", "SPI flash clock (chip pin SD_CLK). By default the ROM boots from an SPI flash on GPIO6-11 clocked here (the SPI_PAD_CONFIG eFuses can move it); the in-package flash of an ESP32-U4WDH and in-package PSRAM of an ESP32-D0WDRH2-V3 are clocked here too. Espressif: not recommended for any other use."),
            ("GPIO11", "Flash chip select (SD_CMD). An ESP32 without in-package flash runs from an off-package SPI flash, by default on GPIO6-11, where this pad is CS#; on ESP32-U4WDH it is IO3/HOLD# of the in-package flash. Espressif: not recommended for any other use."),
            ("GPIO10", "Flash data (SD_DATA_3). An ESP32 without in-package flash runs from an off-package SPI flash, by default on GPIO6-11, where this pad is IO2/WP#; on ESP32-D0WDRH2-V3 also SIO2 of the in-package PSRAM. Not recommended for other use, except on ESP32-U4WDH, whose in-package flash does not use it (ESP32-MINI-1 brings it out as IO10)."),
            ("GPIO9", "Flash data (SD_DATA_2). An ESP32 without in-package flash runs from an off-package SPI flash, by default on GPIO6-11, where this pad is IO3/HOLD#; on ESP32-D0WDRH2-V3 also SIO3 of the in-package PSRAM. Not recommended for other use, except on ESP32-U4WDH, whose in-package flash does not use it (ESP32-MINI-1 brings it out as IO9)."),
            ("GPIO17", "VDD_SDIO power domain (like GPIO6-11 and 16): its logic level follows VDD_SDIO, 1.8 V when that rail is 1.8 V. In-package flash IO1/DO on ESP32-U4WDH. External PSRAM SCLK in the certified ESP32-WROVER-E wiring, which esp-hal's PSRAM driver hard-codes on D0WD parts; Espressif's other recommended option is sharing SD_CLK."),
            ("VDD_SDIO", "Normally an OUTPUT that powers the flash/PSRAM and the GPIO6-11, 16, 17 pads: 3.3 V by default (VDD3P3_RTC through about 6 R; 1 uF to GND), or 1.8 V from an internal 40 mA LDO when MTDI (GPIO12) is high at reset (then add 2k and 4.7 uF to GND). eFuses can fix either voltage or turn it off, and then it must be supplied externally."),
            ("GPIO16", "VDD_SDIO power domain (like GPIO6-11 and 17): its logic level follows VDD_SDIO, 1.8 V when that rail is 1.8 V. In-package flash CS# on ESP32-U4WDH, in-package PSRAM CE# on ESP32-D0WDRH2-V3; for external PSRAM on D0WD parts esp-hal hard-codes CE# here (WROVER wiring). As PSRAM CE# it needs a pull-up (10k in Espressif's designs)."),
        ],
    },
    DefNotes {
        id: "esp32s2",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("CHIP_PU", "Must not float. Espressif advises an RC delay on it (usually 10 kohm, 1 uF) so it rises at least 50 us after the supply rails are stable; a reset needs it below VIL_nRST for at least 50 us. With slow, unstable or often-cycled power the RC alone may not be enough: add a power monitor (reset) chip with a threshold near 3.0 V."),
            ("GPIO46", "Input only: the S2 cannot drive GPIO46, yet esp-hal's metadata marks it as an output, so GPIO Output, TX, SCK, MOSI or PWM are offered here and drive nothing. Strapping pin, pulled down: must be low at reset for download mode; GPIO0 low with GPIO46 high is an invalid combination."),
            ("GPIO45", "Strapping pin, weak pull-down: sampled at reset to set the VDD_SPI flash/PSRAM supply - low = 3.3 V, high = 1.8 V from the internal LDO - unless EFUSE_VDD_SPI_FORCE is burned. With 3.3 V flash/PSRAM (all in-package S2 memory is) keep it low at reset."),
            ("GPIO44", "U0RXD after reset: the ROM's UART download mode receives here, through a USB-to-UART tool wired to U0TXD/U0RXD."),
            ("GPIO43", "U0TXD after reset: the ROM prints its boot log here and UART download mode answers on it. esp-println, which the generated Cargo.toml pulls in, writes straight into UART0's FIFO on the S2, so any function here other than UART0 TX cuts your println! output off this pad."),
            ("GPIO42", "JTAG TMS (MTMS), the pad's IO MUX function 0 and so selected at reset: an external JTAG adapter debugs the S2 here (TCK GPIO39, TDO GPIO40, TDI GPIO41, TMS GPIO42). Giving the pad another function takes it from the debugger."),
            ("GPIO41", "JTAG TDI (MTDI), the pad's IO MUX function 0 and so selected at reset: an external JTAG adapter debugs the S2 here (TCK GPIO39, TDO GPIO40, TDI GPIO41, TMS GPIO42). Giving the pad another function takes it from the debugger."),
            ("VDD3P3_CPU", "IO supply of GPIO38-GPIO46, and of GPIO33-GPIO37 unless IO_MUX_PAD_POWER_CTRL moves them to VDD_SPI. Keep it at or below 3.3 V while eFuses are being burned - the burning circuit is sensitive to higher voltage."),
            ("GPIO40", "JTAG TDO (MTDO), the pad's IO MUX function 0 and so selected at reset: an external JTAG adapter debugs the S2 here (TCK GPIO39, TDO GPIO40, TDI GPIO41, TMS GPIO42). Giving the pad another function takes it from the debugger."),
            ("GPIO39", "JTAG TCK (MTCK), the pad's IO MUX function 0 and so selected at reset: an external JTAG adapter debugs the S2 here (TCK GPIO39, TDO GPIO40, TDI GPIO41, TMS GPIO42). Giving the pad another function takes it from the debugger."),
            ("GPIO15", "Also XTAL_32K_P: an external 32 kHz crystal for the RTC slow clock connects here and on GPIO16. Where one is fitted, the pad is not free."),
            ("GPIO16", "Also XTAL_32K_N: an external 32 kHz crystal for the RTC slow clock connects here and on GPIO15. Where one is fitted, the pad is not free."),
            ("GPIO18", "Download mode also takes serial data on this pad (U1RXD); left floating it can make downloads fail, so Espressif advises an external pull-up (10 kohm typical). Chip revision v0.0 has no internal pull-up here (erratum SYSTEM-117); v1.0 adds one."),
            ("GPIO20", "USB D+. At power-up it fluctuates between high and low, and its high level is strong - only a robust pull-down holds it low. Where a defined start-up level matters, Espressif recommends an external pull-up."),
            ("VDD3P3_RTC_IO", "IO supply of GPIO0-GPIO21. Its recommended range is 3.0-3.6 V, a higher minimum than the 2.8 V of the other 3.3 V rails, and when it feeds VDD_SPI the drop across RSPI must be allowed for."),
            ("GPIO0", "Strapping pin, weak pull-up: low at reset (with GPIO46 low) starts download mode instead of the program in flash. Nothing may pull it low at power-up, not even a large capacitor; Espressif recommends an external pull-up."),
            ("GPIO37", "SPIDQS of the SPI0/1 bus in 8-line (octal) mode (DQS/DM): free unless an off-package octal flash/PSRAM is fitted - all in-package S2 memory is quad SPI."),
            ("GPIO36", "SPIIO7 of the SPI0/1 bus in 8-line (octal) mode (DQ7): free unless an off-package octal flash/PSRAM is fitted - all in-package S2 memory is quad SPI."),
            ("GPIO35", "SPIIO6 of the SPI0/1 bus in 8-line (octal) mode (DQ6): free unless an off-package octal flash/PSRAM is fitted - all in-package S2 memory is quad SPI."),
            ("GPIO34", "SPIIO5 of the SPI0/1 bus in 8-line (octal) mode (DQ5): free unless an off-package octal flash/PSRAM is fitted - all in-package S2 memory is quad SPI."),
            ("GPIO33", "SPIIO4 of the SPI0/1 bus in 8-line (octal) mode (DQ4): free unless an off-package octal flash/PSRAM is fitted - all in-package S2 memory is quad SPI."),
            ("GPIO32", "SPID: the flash's DI/IO0 and the PSRAM's SI/SIO0. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO31", "SPIQ: the flash's DO/IO1 and the PSRAM's SO/SIO1. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO30", "SPICLK: the flash and PSRAM clock. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO29", "SPICS0: the flash chip select (CS#). GPIO27-32 carry the SPI flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO28", "SPIWP: the flash's WP#/IO2 and the PSRAM's SIO2. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO27", "SPIHD: HOLD#/IO3 of the SPI flash and SIO3 of the PSRAM, if there is one. GPIO27-32 carry the flash the chip boots from (inside the package on S2FH2/S2FH4/S2FN4R2, otherwise the recommended pins for an external flash). Espressif: not for other uses."),
            ("VDD_SPI", "Normally a supply OUTPUT: it powers the flash/PSRAM and the SPI pads GPIO26-32 - by default 3.3 V from VDD3P3_RTC_IO through RSPI (5 ohm typ), or 1.8 V from the internal flash LDO (40 mA typ) when GPIO45 is high at reset or eFuses force it. Feed it from outside only once eFuses switch that regulator OFF."),
            ("GPIO26", "SPICS1, the PSRAM chip select: wired inside the package on ESP32-S2R2 and S2FN4R2, where the pad must not be used; otherwise the recommended CS for an off-package PSRAM. Espressif rates GPIO26-32 not recommended for other uses."),
        ],
    },
    DefNotes {
        id: "esp32s3",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO46", "Strapping pin, weak pull-down: GPIO0 low enters UART/USB download mode only if GPIO46 is also low at reset; with GPIO0 high GPIO46 is ignored. With EFUSE_UART_PRINT_CONTROL = 1 or 2 its reset level also gates the ROM's UART0 boot log in SPI Boot (1: high mutes it, 2: low mutes it)."),
            ("GPIO45", "Strapping pin, weak pull-down: sampled at reset to set the VDD_SPI flash/PSRAM supply - low = 3.3 V, high = 1.8 V from the internal LDO - unless EFUSE_VDD_SPI_FORCE is burned. With 3.3 V flash/PSRAM keep it low at reset."),
            ("GPIO44", "U0RXD after reset: the ROM's UART download mode receives here, through a USB-to-UART tool wired to U0TXD/U0RXD."),
            ("GPIO43", "U0TXD after reset: by default the ROM prints its boot log here (and over USB Serial/JTAG) and UART download mode answers on it. Firmware UART0 output also leaves here unless rerouted, so another function on this pad cuts off what a USB-to-UART bridge shows."),
            ("GPIO42", "Pad JTAG TMS (MTMS), unused by default: with the eFuses unburned the S3 takes JTAG from the USB Serial/JTAG on GPIO19/20. JTAG reaches this pad only after burning EFUSE_DIS_USB_JTAG, or EFUSE_STRAP_JTAG_SEL with GPIO3 low at reset."),
            ("GPIO41", "Pad JTAG TDI (MTDI), unused by default: with the eFuses unburned the S3 takes JTAG from the USB Serial/JTAG on GPIO19/20. JTAG reaches this pad only after burning EFUSE_DIS_USB_JTAG, or EFUSE_STRAP_JTAG_SEL with GPIO3 low at reset."),
            ("VDD3P3_CPU", "IO supply of GPIO38-GPIO46, and of GPIO33-37 and GPIO47/48 unless EFUSE_PIN_POWER_SELECTION / IO_MUX_PAD_POWER_CTRL move them to VDD_SPI. Keep it at or below 3.3 V while eFuses are being burned - the burning circuit is sensitive to higher voltage."),
            ("GPIO40", "Pad JTAG TDO (MTDO), unused by default: with the eFuses unburned the S3 takes JTAG from the USB Serial/JTAG on GPIO19/20. JTAG reaches this pad only after burning EFUSE_DIS_USB_JTAG, or EFUSE_STRAP_JTAG_SEL with GPIO3 low at reset."),
            ("GPIO39", "Pad JTAG TCK (MTCK), unused by default: with the eFuses unburned the S3 takes JTAG from the USB Serial/JTAG on GPIO19/20. JTAG reaches this pad only after burning EFUSE_DIS_USB_JTAG, or EFUSE_STRAP_JTAG_SEL with GPIO3 low at reset."),
            ("GPIO10", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO11", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO12", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO13", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO14", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("VDD3P3_RTC", "IO supply of GPIO0-GPIO21 - the only pads that can wake the chip from Deep-sleep - and, through RSPI (14 ohm typ), the source of a 3.3 V VDD_SPI: Espressif asks to keep it above 3.0 V when it feeds 3.3 V flash/PSRAM."),
            ("GPIO15", "Also XTAL_32K_P: an external 32 kHz crystal for the RTC slow clock connects here and on GPIO16; where one is fitted the pad is not free. Power-up: driven LOW for about 60 us before any firmware runs."),
            ("GPIO16", "Also XTAL_32K_N: an external 32 kHz crystal for the RTC slow clock connects here and on GPIO15; where one is fitted the pad is not free. Power-up: driven LOW for about 60 us before any firmware runs."),
            ("GPIO17", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO18", "Power-up glitches: the pad is driven LOW and also HIGH for about 60 us each while the chip powers up, before any firmware runs - mind loads that a short high pulse switches on."),
            ("GPIO19", "USB D- of the USB Serial/JTAG (and OTG): USB flashing and JTAG debugging run here by default. Using GPIO19 or GPIO20 as a GPIO or peripheral signal makes esp-hal clear USB_PAD_ENABLE, and USB OTG takes the one shared PHY - either way the Serial/JTAG link drops. Power-up: one low and two high glitches of about 60 us each."),
            ("GPIO20", "USB D+ of the USB Serial/JTAG (and OTG), with the D+ pull-up on at and after reset. Using GPIO19 or GPIO20 as a GPIO or a GPIO-matrix signal makes esp-hal clear USB_PAD_ENABLE, dropping the USB flashing/console/JTAG link; to flash again, hold GPIO0 low through a reset. Power-up: a pull-down glitch and two high glitches of about 60 us."),
            ("GPIO26", "SPICS1, the PSRAM chip select (CE#). Wired inside the package on every S3 with in-package PSRAM (S3R2, S3RH2, S3R8, S3R8V, S3R16V, S3FH4R2), where it must not be used for anything else; otherwise the CS for an off-package PSRAM, which esp-hal fixes at GPIO26. Espressif: GPIO26-32 not recommended for other uses."),
            ("CHIP_PU", "Must not float. Espressif advises an RC delay on it (usually 10 kohm, 1 uF) so it goes high at least 50 us after the 3.3 V rails are stable; a reset needs it held below VIL_nRST (0.25 x VDD3P3_RTC) for at least 50 us."),
            ("GPIO0", "Strapping pin (weak pull-up), latched at power-on/chip reset and to be held for 3 ms after CHIP_PU rises: low, with GPIO46 low (its default), starts download mode instead of the program in flash. Unless that is wanted, nothing may hold it low then - not even a high-value capacitor; Espressif recommends an external pull-up."),
            ("GPIO1", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO2", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO3", "Strapping pin for the JTAG source, read only when EFUSE_STRAP_JTAG_SEL is burned and EFUSE_DIS_PAD_JTAG/EFUSE_DIS_USB_JTAG are not (by default it is ignored and JTAG goes over USB). No pull is on at reset, so then drive it: 1 = USB JTAG, 0 = pad JTAG on GPIO39-42. Power-up: driven LOW for about 60 us before any firmware runs."),
            ("GPIO4", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO5", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO6", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO7", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO8", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO9", "Power-up glitch: the pad is driven LOW for about 60 us while the chip powers up, before any firmware runs - mind active-low loads."),
            ("GPIO37", "SPIDQS of the SPI0/1 bus in 8-line (octal) mode (DQS/DM): taken by the in-package octal PSRAM on S3R8, S3R8V and S3R16V - do not use it there. On other parts it is free unless an off-package octal flash/PSRAM is fitted."),
            ("GPIO36", "SPIIO7 of the SPI0/1 bus in 8-line (octal) mode (DQ7): taken by the in-package octal PSRAM on S3R8, S3R8V and S3R16V - do not use it there. On other parts it is free unless an off-package octal flash/PSRAM is fitted."),
            ("GPIO35", "SPIIO6 of the SPI0/1 bus in 8-line (octal) mode (DQ6): taken by the in-package octal PSRAM on S3R8, S3R8V and S3R16V - do not use it there. On other parts it is free unless an off-package octal flash/PSRAM is fitted."),
            ("GPIO34", "SPIIO5 of the SPI0/1 bus in 8-line (octal) mode (DQ5): taken by the in-package octal PSRAM on S3R8, S3R8V and S3R16V - do not use it there. On other parts it is free unless an off-package octal flash/PSRAM is fitted."),
            ("GPIO33", "SPIIO4 of the SPI0/1 bus in 8-line (octal) mode (DQ4): taken by the in-package octal PSRAM on S3R8, S3R8V and S3R16V - do not use it there. On other parts it is free unless an off-package octal flash/PSRAM is fitted."),
            ("GPIO47", "IO MUX F0 is SPICLK_P_DIFF, the positive half of a differential flash/PSRAM clock that no mode in the datasheet's flash/PSRAM pin map uses. It shares GPIO33-37's power domain (EFUSE_PIN_POWER_SELECTION, IO_MUX_PAD_POWER_CTRL): with 1.8 V octal flash/PSRAM, e.g. ESP32-S3R8V/S3R16V, it runs at 1.8 V, so a 3.3 V device on it needs a level shifter."),
            ("GPIO48", "IO MUX F0 is SPICLK_N_DIFF, the negative half of a differential flash/PSRAM clock that no mode in the datasheet's flash/PSRAM pin map uses. It shares GPIO33-37's power domain (EFUSE_PIN_POWER_SELECTION, IO_MUX_PAD_POWER_CTRL): with 1.8 V octal flash/PSRAM, e.g. ESP32-S3R8V/S3R16V, it runs at 1.8 V, so a 3.3 V device on it needs a level shifter."),
            ("GPIO32", "SPID: flash DI and PSRAM SI/SIO0 (DQ0 of both in octal mode). GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for the external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO31", "SPIQ: flash DO/IO1 and PSRAM SO/SIO1, or DQ1 of both in octal SPI mode. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for an external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO30", "SPICLK: the flash and PSRAM clock. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for an external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO29", "SPICS0: the flash chip select (CS#). GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for an external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO28", "SPIWP: flash WP#/IO2 and PSRAM SIO2, or DQ2 of both in octal SPI mode. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for an external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("GPIO27", "SPIHD: flash HOLD#/IO3 and PSRAM SIO3, or DQ3 of both in octal SPI mode. GPIO27-32 carry the SPI flash the chip boots from (inside the package on S3FN8/S3FH4R2, otherwise the recommended pins for an external flash), plus the PSRAM if there is one. Espressif: not recommended for other uses."),
            ("VDD_SPI", "Normally a supply OUTPUT for flash/PSRAM and the SPI pads GPIO26-32: 3.3 V from VDD3P3_RTC through RSPI (14 ohm typ) by default, or 1.8 V from the internal LDO (40 mA typ) when GPIO45 is high at reset. A burned EFUSE_VDD_SPI_FORCE ignores GPIO45 and EFUSE_VDD_SPI_TIEH picks the voltage. Espressif: 0.1 uF + 1 uF close to it."),
        ],
    },
    DefNotes {
        id: "esp32c2",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("XTAL_P", "The C2 runs from a 26 MHz or a 40 MHz crystal, and neither a strap nor an eFuse records which: the ROM assumes 40 MHz, esp-hal 1.1 measures it at startup unless told. Chip revisions v0.0/v1.0 may fail with 40 MHz (errata XTAL-5948) - fit 26 MHz on those."),
            ("XTAL_N", "The C2 runs from a 26 MHz or a 40 MHz crystal, and neither a strap nor an eFuse records which: the ROM assumes 40 MHz, esp-hal 1.1 measures it at startup unless told. Chip revisions v0.0/v1.0 may fail with 40 MHz (errata XTAL-5948) - fit 26 MHz on those."),
            ("GPIO20", "U0TXD: output-enabled after reset. With eFuse UART_PRINT_CONTROL left at 0 (default) the ROM prints its boot log here at reset - 115200 baud with a 40 MHz crystal, 74880 with 26 MHz - and the UART download mode answers here (the C2 has no USB). Espressif suggests a 499 ohm series resistor."),
            ("GPIO19", "U0RXD: UART0 RX after reset, weak pull-up on. The ROM's UART download mode (GPIO9 low and GPIO8 high at reset) receives here, and with no USB on the C2 this pad and GPIO20 are its serial flashing port. Espressif advises using a UART other than UART0 for application traffic."),
            ("CHIP_EN", "Must not be left floating. Espressif recommends an RC delay here (R = 10 kohm, C = 1 uF) so it goes high at least 50 us after the 3.3 V rails come up; holding it below VIL_nRST (0.25 x VDD) for at least 50 us resets the chip."),
            ("GPIO3", "Driven LOW for about 60 us at power-up, before any code runs (Espressif's power-up glitch table). Do not use it for a line that must stay high from power-on, such as an active-low enable or reset of another part."),
            ("GPIO4", "MTMS (JTAG TMS). The C2 has no USB Serial/JTAG, so GPIO4-7 are its only JTAG port: leave them free if you want to debug with a JTAG probe."),
            ("GPIO5", "MTDI (JTAG TDI) - with GPIO4/6/7 the C2's only JTAG port, as it has no USB Serial/JTAG. Also driven LOW for about 60 us at power-up (Espressif's glitch table), so it cannot hold an active-low enable of another part high from power-on."),
            ("GPIO6", "MTCK (JTAG TCK). The C2 has no USB Serial/JTAG, so GPIO4-7 are its only JTAG port: leave them free if you want to debug with a JTAG probe."),
            ("GPIO0", "Driven LOW for about 40 us at power-up, before any code runs (Espressif's power-up glitch table). Do not use it for a line that must stay high from power-on, such as an active-low enable or reset of another part."),
            ("GPIO1", "Driven LOW for about 60 us at power-up, before any code runs (Espressif's power-up glitch table). Do not use it for a line that must stay high from power-on, such as an active-low enable or reset of another part."),
            ("GPIO9", "Boot strap with a weak internal pull-up: LOW at reset (with GPIO8 high) starts the ROM's UART download mode instead of your firmware. Espressif recommends an external pull-up and no large capacitor on it, or the chip may enter download mode."),
            ("GPIO8", "Strapping pin with no default level. UART download mode (GPIO9 low at reset) requires GPIO8 HIGH, so give it a pull-up and keep loads here from holding it low at reset. With eFuse UART_PRINT_CONTROL burned (1 or 2), its level also turns the ROM's boot log on U0TXD on or off."),
            ("GPIO7", "MTDO (JTAG TDO). The C2 has no USB Serial/JTAG, so GPIO4-7 are its only JTAG port: leave them free if you want to debug with a JTAG probe."),
        ],
    },
    DefNotes {
        id: "esp32c3",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO21", "U0TXD: the ROM prints its boot log here at every boot by default (eFuse UART_PRINT_CONTROL can make it follow GPIO8 or switch it off) and answers UART download mode here. It is an enabled output after reset, so never tie it to another driver. Espressif suggests a 499 ohm series resistor against harmonics."),
            ("GPIO20", "U0RXD: UART0 RX by default and the receive line of the ROM's UART download mode (GPIO9 low and GPIO8 high at reset). Weak pull-up enabled after reset."),
            ("GPIO19", "USB D+ of the USB Serial/JTAG; its USB pull-up is on from reset, so a host sees a device. Using GPIO19 or GPIO18 in esp-hal as GPIO or for any other peripheral clears the USB pad enable and that pull-up, so the ROM-log/download/JTAG port vanishes while your firmware runs; reflash by holding GPIO9 low and GPIO8 high through reset."),
            ("GPIO18", "USB D- of the USB Serial/JTAG, which by default carries the ROM log, download mode and JTAG. esp-hal switches the USB pads off once GPIO18 or GPIO19 is used as GPIO or by any other peripheral, so the host loses that port while your firmware runs; reflash with GPIO9 low and GPIO8 high through reset. Drives high for about 50 us at power-up."),
            ("GPIO4", "MTMS (JTAG TMS) of the pad JTAG. By default the C3 runs JTAG over the USB Serial/JTAG on GPIO18/19, so this is a free GPIO; GPIO4-7 become the JTAG port only if eFuse DIS_USB_JTAG is burned, or JTAG_SEL_ENABLE is burned and GPIO10 is low at reset."),
            ("GPIO5", "ADC2_CH0, the C3's only ADC2 channel. ADC2 is not factory-calibrated, its digital (DMA) controller can lock up on revisions v0.0-v1.1 with no fix scheduled (errata ADC-183), and ESP-IDF dropped ADC2 one-shot reads as unstable - use ADC1 on GPIO0-4. Also MTDI (JTAG TDI) of the pad JTAG, unused by default because JTAG runs over USB."),
            ("GPIO6", "MTCK (JTAG TCK) of the pad JTAG. By default the C3 runs JTAG over the USB Serial/JTAG on GPIO18/19, so this is a free GPIO; GPIO4-7 become the JTAG port only if eFuse DIS_USB_JTAG is burned, or JTAG_SEL_ENABLE is burned and GPIO10 is low at reset."),
            ("GPIO7", "MTDO (JTAG TDO) of the pad JTAG. By default the C3 runs JTAG over the USB Serial/JTAG on GPIO18/19, so this is a free GPIO; GPIO4-7 become the JTAG port only if eFuse DIS_USB_JTAG is burned, or JTAG_SEL_ENABLE is burned and GPIO10 is low at reset."),
            ("GPIO8", "Strapping pin, floating at reset. UART0/USB download boot (GPIO9 low) needs GPIO8 HIGH, so give it a pull-up and keep loads from holding it low at reset. With eFuse UART_PRINT_CONTROL at 1 or 2, its level also turns the ROM's UART0 boot log on or off."),
            ("GPIO9", "Boot strap with a weak internal pull-up: LOW at reset (with GPIO8 high) starts the ROM's download mode over UART0 or USB instead of your firmware. Espressif recommends an external pull-up and no large capacitor on it, or the chip may enter download mode."),
            ("GPIO0", "XTAL_32K_P: one leg of the optional 32.768 kHz crystal that feeds XTAL32K, an RTC_SLOW source in the Clock tab (GPIO1 is the other leg). With that crystal fitted this pad is taken - not a GPIO or ADC input."),
            ("GPIO1", "XTAL_32K_N: the other leg of the optional 32.768 kHz crystal for XTAL32K, an RTC_SLOW source in the Clock tab (GPIO0 is XTAL_32K_P). With that crystal fitted this pad is taken - not a GPIO or ADC input."),
            ("GPIO2", "Strapping pin, latched at reset and floating by default (no internal pull). It does not choose between SPI Boot and Joint Download Boot, but Espressif recommends pulling it up because of glitches - fit a pull-up and keep loads from holding it low during reset."),
            ("CHIP_EN", "Must not be left floating. Espressif recommends an RC delay on it (usually R = 10 kohm, C = 1 uF) so it goes high only after the supply rails have had at least 50 us (tSTBL) to stabilize; holding it below VIL_nRST for at least 50 us (tRST) resets the chip."),
            ("GPIO17", "SPIQ, the flash data output DO (pin 24). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; a plain ESP32-C3 needs an external flash and this is its recommended pin; on FH4AZ/FH4X it is not bonded (NC). Treat it as taken, not as a GPIO."),
            ("GPIO16", "SPID, the flash data input DI (pin 23). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; a plain ESP32-C3 needs an external flash and this is its recommended pin; on FH4AZ/FH4X it is not bonded (NC). Treat it as taken, not as a GPIO."),
            ("GPIO15", "SPICLK, the flash clock (pin 22). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; a plain ESP32-C3 needs an external flash and this is its recommended pin; on FH4AZ/FH4X it is not bonded (NC). Treat it as taken, not as a GPIO."),
            ("GPIO14", "SPICS0, the flash chip select CS# (pin 21). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; a plain ESP32-C3 needs an external flash and this is its recommended pin; on FH4AZ/FH4X it is not bonded (NC). Treat it as taken, not as a GPIO."),
            ("GPIO13", "SPIWP, the flash WP# line (pin 20). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; on FH4AZ/FH4X it is not bonded (NC). A plain ESP32-C3 needs external flash and this is its recommended WP# pin, a data line only in QIO/QOUT mode. Treat it as taken."),
            ("GPIO12", "SPIHD, the flash HOLD# line (pin 19). ESP32-C3FN4/FH4/FH8X wire it to the in-package flash and Espressif says not to use it for anything else; on FH4AZ/FH4X it is not bonded (NC). A plain ESP32-C3 needs external flash and this is its recommended HOLD# pin, a data line only in QIO/QOUT mode. Treat it as taken."),
            ("VDD_SPI", "The flash supply pad, not a core supply: it outputs VDD3P3_CPU through RSPI (7.5 ohm typ) to the flash, in-package or external, so VDD3P3_CPU must stay above the flash's minimum voltage plus that drop. It becomes GPIO11 only with an off-package flash on its own supply and eFuse VDD_SPI_AS_GPIO burned."),
        ],
    },
    DefNotes {
        id: "esp32c5",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO28", "Boot-mode strapping pin with a weak pull-up: SPI boot from flash needs it HIGH at reset; held LOW, the chip enters a download mode instead. Espressif recommends an external pull-up and no large capacitor on it, or the chip may enter download mode."),
            ("GPIO27", "Boot-mode strapping pin with a weak pull-up. With GPIO28 low at reset, GPIO27 high selects Joint Download Boot 0 (USB Serial/JTAG or UART0) and low (with GPIO26 low) Joint Download Boot 1. If EFUSE_UART_PRINT_CONTROL is 1 or 2, its reset level also turns the UART0 ROM log on or off (1: low = on, 2: high = on)."),
            ("GPIO4", "JTAG MTCK for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads (except in Joint Download Boot 1 mode). LP GPIO4, usable in Deep-sleep."),
            ("GPIO5", "JTAG MTDO, but with the JTAG eFuses unburned (default) JTAG runs over the USB Serial/JTAG controller, not GPIO2-5 (except in Joint Download Boot 1). A probe here needs EFUSE_DIS_USB_JTAG burned, or EFUSE_JTAG_SEL_ENABLE burned with GPIO7 low at reset. LP GPIO5: one of GPIO0-6, the only Deep-sleep wake-up pins (esp-hal 1.1 has no C5 sleep driver)."),
            ("GPIO6", "LP GPIO6: GPIO0-6 are the only pins that can be operated in Deep-sleep or wake the chip from it; any other pad can at most hold a latched level. esp-hal 1.1, the version generated here, has no C5 sleep driver, so generated code cannot use this yet."),
            ("GPIO7", "Strapping pin for the JTAG source, used only if EFUSE_JTAG_SEL_ENABLE is burned (default 0) with both JTAG-disable eFuses still 0: then high at reset keeps JTAG on USB Serial/JTAG, low moves it to GPIO2-5. It floats at reset (no default pull), so in that case drive it externally. Otherwise an ordinary GPIO."),
            ("GPIO11", "U0TXD, UART0 TX by default. UART0 is the ROM's port for UART download and for its boot log, printed at every boot by default, so whatever is wired here sees that traffic. Espressif suggests a 499 ohm series resistor on this line."),
            ("GPIO12", "U0RXD, UART0 RX by default (weak pull-up after reset): the line the ROM listens on for UART download (UART0 is its download and boot-log port)."),
            ("GPIO13", "USB D- of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO13 or GPIO14 any other function disables it: esp-hal clears USB_PAD_ENABLE and the USB pulls when it sets the pin up as a GPIO or routes a peripheral to it."),
            ("GPIO14", "USB D+ of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO13 or GPIO14 any other function disables it (esp-hal clears USB_PAD_ENABLE). D+ toggles high/low at power-up; add an external pull-up if the line must start in a stable state."),
            ("CHIP_PU", "Must not be left floating, and must go high only after the supply rails have settled (tSTBL >= 50 us): Espressif advises an RC delay, typically 10 kohm and 1 uF, and a short CHIP_PU trace, since interference on it causes reboots."),
            ("GPIO0", "XTAL_32K_P: an optional external 32.768 kHz crystal for the RTC slow clock sits on GPIO0/GPIO1, and then both pins are taken; without it they are plain GPIOs. LP GPIO0: only GPIO0-6 can be controlled in Deep-sleep or wake the chip from it; other pads can at most hold their level."),
            ("GPIO1", "XTAL_32K_N: the other end of the optional 32.768 kHz RTC crystal (with GPIO0); fitted, both pins are taken. LP GPIO1: only GPIO0-6 can be controlled in Deep-sleep or wake the chip from it; other pads can at most hold their level."),
            ("GPIO2", "JTAG MTMS for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads (except in Joint Download Boot 1 mode). Also a strapping pin, latched at reset and floating by default, but the datasheet names no boot option for it. LP GPIO2, usable in Deep-sleep."),
            ("GPIO3", "JTAG MTDI for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads (except in Joint Download Boot 1 mode). Strapping pin, floating by default: it picks the SDIO slave's output driving edge (GPIO25 picks the sampling edge). LP GPIO3, usable in Deep-sleep."),
            ("GPIO26", "Boot-mode strapping pin, floating at reset. It only counts when GPIO27 and GPIO28 are both low at reset: GPIO26 low then selects Joint Download Boot 1 (UART0 or SDIO download, no USB), while GPIO26 high enters a test/diagnostic mode instead."),
            ("GPIO25", "Strapping pin, floating by default: with GPIO3 (MTDI) its level at reset sets the SDIO slave's sampling/driving clock edge."),
            ("GPIO22", "SPID (pin 32), MOSI / SIO0 of the flash/PSRAM bus: on ESP32-C5HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C5HF4 it is not connected (NC)."),
            ("GPIO21", "SPICLK (pin 31), the clock of the flash/PSRAM bus: on ESP32-C5HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C5HF4 it is not connected (NC)."),
            ("GPIO20", "SPIHD (pin 30), HOLD# / SIO3 of the flash/PSRAM bus: on ESP32-C5HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C5HF4 it is not connected (NC)."),
            ("VDD_SPI", "Normally a supply output for the flash/PSRAM: VDDPST2 through RSPI, so keep VDDPST2 at 3.0 V or more and put 1 uF close to it. Not connected on ESP32-C5HF4. It works as GPIO19 only if the flash/PSRAM is powered from elsewhere and eFuse VDD_SPI_AS_GPIO (0 by default) is burned."),
            ("GPIO18", "SPIWP (pin 28), WP# / SIO2 of the flash/PSRAM bus: on ESP32-C5HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C5HF4 it is not connected (NC)."),
            ("GPIO17", "SPIQ (pin 27), MISO / SIO1 of the flash/PSRAM bus: on ESP32-C5HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C5HF4 it is not connected (NC)."),
            ("GPIO16", "SPICS0 (pin 26), the flash chip select: on ESP32-C5HR2/HR8, which have no flash inside, it goes to the external boot flash; on ESP32-C5HF4 it is not connected (NC), the flash being in the package."),
            ("GPIO15", "SPICS1 (pin 25), the PSRAM chip select on the SPI0/1 flash bus. Where PSRAM is fitted, in the package or outside it, the PSRAM owns this pin; on a part with no PSRAM it can be used as an ordinary GPIO15."),
        ],
    },
    DefNotes {
        id: "esp32c6",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO5", "JTAG MTDI for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. Strapping pin too: with GPIO4 (MTMS) it sets the SDIO slave's default sampling/driving edge (a driver can override it); floating by default. LP_GPIO5, usable in Deep-sleep."),
            ("GPIO6", "JTAG MTCK for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. Its internal pull-up is on after reset while EFUSE_DIS_PAD_JTAG = 0. LP_GPIO6, usable in Deep-sleep."),
            ("GPIO7", "JTAG TDO (MTDO) for an external probe, but with the JTAG eFuses at their default 0 the chip takes JTAG from the USB Serial/JTAG controller, not from GPIO4-7. Pad JTAG needs EFUSE_DIS_USB_JTAG burned, or EFUSE_JTAG_SEL_ENABLE burned and GPIO15 low at reset. LP_GPIO7 in the VDDPST1 domain, so it can still be controlled in Deep-sleep."),
            ("GPIO8", "Strapping pin, floating by default. Download mode (GPIO9 low at reset) needs GPIO8 HIGH: GPIO8 = 0 with GPIO9 = 0 is an invalid combination. If EFUSE_UART_PRINT_CONTROL is 1 or 2, its level at reset also gates the ROM boot log on UART0 (1: prints when low, 2: prints when high)."),
            ("GPIO9", "BOOT strapping pin with a weak pull-up: held LOW at reset with GPIO8 high, the chip enters Joint Download Boot (UART0, USB Serial/JTAG or SDIO) instead of booting from flash. Espressif recommends an external pull-up on it and no high-value capacitor, which may make the chip enter download mode."),
            ("GPIO12", "USB D- of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO12 or GPIO13 any other function disables it: esp-hal clears USB_PAD_ENABLE and the USB pulls when it sets the pin up as a GPIO or routes a peripheral to it."),
            ("GPIO13", "USB D+ of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO12 or GPIO13 any other function disables it (esp-hal clears USB_PAD_ENABLE). D+ fluctuates high/low at power-up; add an external pull-up if it must start at a stable high level."),
            ("GPIO24", "SPICS0, the boot flash's CS#. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
            ("CHIP_PU", "Must not be left floating, and must go high only after the supply rails have settled (tSTBL >= 50 us): Espressif advises an RC delay, typically 10 kohm and 1 uF, and a short CHIP_PU trace, since interference on it causes reboots."),
            ("GPIO0", "XTAL_32K_P: an optional 32.768 kHz RTC crystal goes across GPIO0/GPIO1 and takes both pins; an external 32 kHz oscillator enters on GPIO0 alone. Without either they are plain GPIOs. LP_GPIO0 (VDDPST1): only GPIO0-7 can be used by the LP core and LP peripherals in Deep-sleep or wake the chip from it; GPIO8-30 can only hold a level."),
            ("GPIO1", "XTAL_32K_N: the other end of the optional 32.768 kHz RTC crystal (with GPIO0); fitted, both pins are taken. An external 32 kHz oscillator needs GPIO0 only. LP_GPIO1 (VDDPST1): only GPIO0-7 can be used by the LP core and LP peripherals in Deep-sleep or wake the chip from it; GPIO8-30 can only hold a level."),
            ("GPIO2", "LP_GPIO2 (VDDPST1 domain): only GPIO0-7 can be used by the LP core and LP peripherals in Deep-sleep or wake the chip from it; GPIO8-30 can only hold a latched level."),
            ("GPIO3", "LP_GPIO3 (VDDPST1 domain): only GPIO0-7 can be used by the LP core and LP peripherals in Deep-sleep or wake the chip from it; GPIO8-30 can only hold a latched level."),
            ("GPIO4", "JTAG MTMS for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. Strapping pin too: with GPIO5 (MTDI) it sets the SDIO slave's default sampling/driving edge (a driver can override it); floating by default. LP_GPIO4, usable in Deep-sleep."),
            ("GPIO17", "U0RXD, UART0 RX by default (weak pull-up after reset; TX is GPIO16): in download boot mode the ROM waits here for a UART download. The ROM also downloads over USB Serial/JTAG on GPIO12/13, so this pad is not the only way to flash."),
            ("GPIO16", "U0TXD, UART0 TX by default. UART0 is the ROM's port for UART download and for its boot log, printed at every reset by default, so whatever is wired here sees that traffic. Espressif suggests a 499 ohm series resistor on this line."),
            ("GPIO15", "JTAG-source strapping pin, read at reset only if EFUSE_JTAG_SEL_ENABLE is burned (default 0) and neither EFUSE_DIS_PAD_JTAG nor EFUSE_DIS_USB_JTAG is: low = JTAG on the GPIO4-7 pads, high = USB Serial/JTAG. It has no internal pull, so then it must be driven, never left floating. Otherwise an ordinary GPIO."),
            ("GPIO30", "SPID, the boot flash's MOSI / SIO0. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
            ("GPIO29", "SPICLK, the boot flash's clock. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
            ("GPIO28", "SPIHD, the boot flash's HOLD# / SIO3. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
            ("VDD_SPI", "By default an output: VDDPST2 feeds it through RSPI as the 3.3 V supply for the external flash, so keep VDDPST2 at 3.0 V or more and put 0.1 uF + 1 uF close to it. It works as GPIO27 only with the flash powered elsewhere and the pad switched over (eFuse VDD_SPI_AS_GPIO, or PMU_VDD_SPI_PWR_SEL_SW=1 with PMU_VDD_SPI_PWR_SW=0)."),
            ("GPIO26", "SPIWP, the boot flash's WP# / SIO2. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
            ("GPIO25", "SPIQ, the boot flash's MISO / SIO1. The QFN40 ESP32-C6 has no in-package flash, so GPIO24-26 and GPIO28-30 are its external flash bus; Espressif marks them already allocated and says not to use flash pins for anything else."),
        ],
    },
    DefNotes {
        id: "esp32c61",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO7", "Strapping pin for the JTAG source, but it only counts if EFUSE_JTAG_SEL_ENABLE is burned and EFUSE_DIS_PAD_JTAG/DIS_USB_JTAG are not (all 0 by default). It floats at reset (no internal pull), so in that case an external circuit must drive it: high keeps JTAG on USB Serial/JTAG, low moves it to the MTMS/MTDI/MTCK/MTDO pads (GPIO3-6)."),
            ("GPIO11", "U0TXD, UART0 TX by default. UART0 is the ROM's port for UART download and for its boot log, printed at every reset by default, so whatever is wired here sees that traffic. Espressif suggests a 499 ohm series resistor on this line."),
            ("GPIO10", "U0RXD, UART0 RX by default (weak pull-up after reset): the line the ROM listens on for UART download (UART0 is its download and boot-log port)."),
            ("GPIO9", "Boot-mode strapping pin with a weak pull-up: held LOW at reset while GPIO8 is HIGH, the chip enters Joint Download Boot (UART0, USB or SDIO) instead of booting from flash. Espressif recommends an external pull-up and no high-value capacitor on it, or the chip may enter download mode."),
            ("GPIO8", "Strapping pin, floating by default. Download mode (GPIO9 low at reset) needs GPIO8 HIGH; GPIO8 and GPIO9 both low is an invalid combination. If eFuse UART_PRINT_CONTROL is set to 1 or 2, GPIO8's level at reset also turns the ROM boot log on UART0 on or off (1: printed when low, 2: printed when high)."),
            ("GPIO5", "JTAG MTCK for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. LP GPIO5, one of GPIO0-6 that can wake the chip from Deep-sleep."),
            ("GPIO6", "JTAG MTDO for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. LP GPIO6, one of GPIO0-6 that can wake the chip from Deep-sleep."),
            ("GPIO14", "SPICS1 (pin 19), the PSRAM chip select: on ESP32-C61HR2/HR8 it is wired to the in-package PSRAM and cannot be used for anything else. On a part with no PSRAM (ESP32-C61HF4) Espressif's module datasheet says it can be used as GPIO14."),
            ("GPIO15", "SPICS0 (pin 20), the flash chip select. ESP32-C61HR2/HR8 have no flash inside, so this pin goes to the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
            ("CHIP_PU", "Must not be left floating, and must go high only after the supply rails have settled (tSTBL >= 50 us): Espressif advises an RC delay, typically 10 kohm and 1 uF, and a short CHIP_PU trace, since interference on it causes reboots."),
            ("GPIO0", "XTAL_32K_P: an optional external 32.768 kHz crystal for the RTC slow clock sits on GPIO0/GPIO1 and then takes both pins; an external 32 kHz oscillator takes GPIO0 alone. Unused, they are plain GPIOs. LP GPIO0 (VDDPST1 domain): only GPIO0-6 can wake the chip from Deep-sleep."),
            ("GPIO1", "XTAL_32K_N: the other end of the optional 32.768 kHz RTC crystal (with GPIO0); when it is fitted, both pins are taken. LP GPIO1 (VDDPST1 domain): only GPIO0-6 can wake the chip from Deep-sleep."),
            ("GPIO2", "LP GPIO2 (VDDPST1 domain): only GPIO0-6 can wake the chip from Deep-sleep."),
            ("GPIO3", "JTAG MTMS for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. Strapping pin too: with GPIO4 (MTDI) it sets the SDIO slave's sampling/driving clock edge; floating by default. LP GPIO3, one of GPIO0-6 that can wake the chip from Deep-sleep."),
            ("GPIO4", "JTAG MTDI for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads. Strapping pin too: with GPIO3 (MTMS) it sets the SDIO slave's sampling/driving clock edge; floating by default. LP GPIO4, one of GPIO0-6 that can wake the chip from Deep-sleep."),
            ("GPIO13", "USB D+ of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO12 or GPIO13 any other function disables it (esp-hal clears USB_PAD_ENABLE). D+ toggles high/low at power-up; add an external pull-up if the line must start in a stable state."),
            ("GPIO12", "USB D- of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO12 or GPIO13 any other function disables it: esp-hal clears USB_PAD_ENABLE and the USB pulls when it sets the pin up as a GPIO or routes a peripheral to it."),
            ("GPIO21", "SPID (pin 27), MOSI / SIO0 of the flash/PSRAM bus: on ESP32-C61HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
            ("GPIO20", "SPICLK (pin 26), the clock of the flash/PSRAM bus: on ESP32-C61HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
            ("GPIO19", "SPIHD (pin 25), HOLD# / SIO3 of the flash/PSRAM bus: on ESP32-C61HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
            ("VDD_SPI", "Supply OUTPUT for the flash/PSRAM: VDDPST2 through RSPI (typ. 3 ohm), so it sits a little below VDDPST2. Put 1 uF close to it and keep VDDPST2 at 3.0 V or more. On ESP32-C61HR2/HR8 the in-package PSRAM must be powered from it, so it can never be GPIO18; on ESP32-C61HF4 it is not connected (NC)."),
            ("GPIO17", "SPIWP (pin 23), WP# / SIO2 of the flash/PSRAM bus: on ESP32-C61HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
            ("GPIO16", "SPIQ (pin 22), MISO / SIO1 of the flash/PSRAM bus: on ESP32-C61HR2/HR8 it serves the in-package PSRAM and the external boot flash; on ESP32-C61HF4 it is not connected (NC). Not a usable GPIO on any variant."),
        ],
    },
    DefNotes {
        id: "esp32h2",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("GPIO27", "USB D+ of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO26 or GPIO27 any other function disables it (esp-hal clears USB_PAD_ENABLE). D+ toggles high/low at power-up; add an external pull-up if the line must start in a stable state."),
            ("GPIO26", "USB D- of the built-in USB Serial/JTAG controller (ROM download, boot log, default JTAG). Giving GPIO26 or GPIO27 any other function disables it: esp-hal clears USB_PAD_ENABLE and the USB pulls when it sets the pin up as a GPIO or routes a peripheral to it."),
            ("GPIO8", "Strapping pin, floating by default. Download mode (GPIO9 low at reset) needs GPIO8 HIGH; GPIO8 and GPIO9 both low is invalid. If EFUSE_UART_PRINT_CONTROL is set to 1 or 2, its level at reset also turns the ROM boot log on UART on or off. LP pin (GPIO8-14, LP power domain): it can wake the chip from Deep-sleep."),
            ("GPIO9", "BOOT strapping pin with a weak pull-up: held LOW at reset (with GPIO8 HIGH), the chip enters Joint Download Boot (UART0 or USB Serial/JTAG) instead of booting from flash. Espressif recommends an external pull-up and no high-value capacitor on it, or the chip may enter download mode. LP pin (GPIO8-14): it can wake the chip from Deep-sleep."),
            ("GPIO10", "LP pin: GPIO8-14 are the only pads on this package that can wake the chip from Deep-sleep (EXT1 wakeup, each pin with its own level - esp-hal Ext1WakeupSource). GPIO0-5 and GPIO22-27 can wake it only from Light-sleep."),
            ("GPIO11", "LP pin: GPIO8-14 are the only pads on this package that can wake the chip from Deep-sleep (EXT1 wakeup, each pin with its own level - esp-hal Ext1WakeupSource). GPIO0-5 and GPIO22-27 can wake it only from Light-sleep."),
            ("GPIO12", "Unlike GPIO8-11 (VDDPST1), its I/O is powered from VDDA_PMU/VBAT, as are the 32 kHz crystal pins GPIO13/14. LP pin: GPIO8-14 are the only pads that can wake the chip from Deep-sleep (EXT1); GPIO0-5 and GPIO22-27 wake it only from Light-sleep."),
            ("GPIO13", "XTAL_32K_P: an optional external 32.768 kHz crystal for the low-power slow clock goes across GPIO13/GPIO14, and then both pins are taken (an external 32 kHz oscillator uses this pin alone). Powered from VDDA_PMU/VBAT, not VDDPST1. LP pin: can wake the chip from Deep-sleep (EXT1)."),
            ("GPIO14", "XTAL_32K_N: the other end of the optional 32.768 kHz crystal (with GPIO13); with the crystal fitted both pins are taken. Powered from VDDA_PMU/VBAT, not VDDPST1. LP pin: can wake the chip from Deep-sleep (EXT1)."),
            ("GPIO2", "JTAG MTMS for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads."),
            ("GPIO3", "JTAG MTDO for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads."),
            ("GPIO4", "JTAG MTCK for an external probe, but with no eFuses burned the chip takes JTAG from the USB Serial/JTAG controller, not from these pads."),
            ("GPIO5", "ADC1_CH4 does not work on ESP32-H2 revisions v0.0 and v0.1 (erratum ADC-7227, fixed in v1.2): use another ADC channel on those parts. Also JTAG MTDI, though with no eFuses burned JTAG comes from the USB Serial/JTAG controller, not these pads."),
            ("GPIO25", "Strapping pin for the JTAG source, ignored unless EFUSE_JTAG_SEL_ENABLE is burned (default 0) while EFUSE_DIS_PAD_JTAG and EFUSE_DIS_USB_JTAG are 0; then low at reset = JTAG on GPIO2-GPIO5, high = USB Serial/JTAG. It has no internal pull, so in that case it must be driven, never left floating."),
            ("GPIO24", "U0TXD, UART0 TX by default. UART0 is the ROM's port for UART download and for its boot log, printed at every reset by default, so whatever is wired here sees that traffic. Espressif suggests a 499 ohm series resistor on this line."),
            ("GPIO23", "U0RXD, UART0 RX by default: the line the ROM listens on for UART download (UART0 is its download and boot-log port)."),
            ("VDDA_PMU", "On ESP32-H2 it takes 3.0-3.6 V, like VDD3P3 and VBAT, and must be stable before CHIP_EN goes high. It is also one of the two inputs (the other is VBAT) that an internal switch picks from to power the GPIO12, GPIO13 (XTAL_32K_P) and GPIO14 (XTAL_32K_N) pads."),
            ("VBAT", "On ESP32-H2 VBAT is not a backup-only input: without a backup cell it is one of the analog supply pins (3.0-3.6 V) and must be stable before CHIP_EN goes high. An internal switch powers GPIO12, GPIO13 (XTAL_32K_P) and GPIO14 (XTAL_32K_N) from either VBAT or VDDA_PMU."),
            ("CHIP_EN", "Must not be left floating, and must go high only after the supply rails have settled (tSTBL >= 50 us): Espressif advises an RC delay, typically 10 kohm and 1 uF, and a short CHIP_EN trace, since interference on it causes reboots."),
        ],
    },
    DefNotes {
        id: "stm32f103c8t6",
        every_gpio: "",
        not_every: &[],
        pads: &[
            ("PB9", "CAN here cannot run while USB is in use: the two share one 512-byte SRAM. Whenever I2C1 is clocked - here or on PB6/PB7 - its SMBA signal (which stays on PB5) collides with PB5 as an alternate-function output, so remapped SPI1 MOSI in master mode and TIM3_CH2 output on PB5 do not work (errata ES096 2.3.7, 2.3.8)."),
            ("PB8", "CAN here cannot run while USB is in use: the two share one 512-byte SRAM. Whenever I2C1 is clocked - here or on PB6/PB7 - its SMBA signal (which stays on PB5) collides with PB5 as an alternate-function output, so remapped SPI1 MOSI in master mode and TIM3_CH2 output on PB5 do not work (errata ES096 2.3.7, 2.3.8)."),
            ("PB7", "If I2C1 runs here: while it is clocked, its SMBA signal conflicts with PB5 as an alternate-function output even when SMBA is unused, so remapped SPI1 MOSI in master mode and TIM3_CH2 output on PB5 do not work (errata ES096 2.3.7, 2.3.8)."),
            ("PB6", "If I2C1 runs here: while it is clocked, its SMBA signal conflicts with PB5 as an alternate-function output even when SMBA is unused, so remapped SPI1 MOSI in master mode and TIM3_CH2 output on PB5 do not work (errata ES096 2.3.7, 2.3.8)."),
            ("PB5", "Not 5 V tolerant, unlike PB4 and PB6 beside it. While I2C1 is clocked (on PB6/PB7 or PB8/PB9), its SMBA signal conflicts with PB5 as an alternate-function output even when SMBA is unused: remapped SPI1 MOSI in master mode and TIM3_CH2 output do not work here next to I2C1 (errata ES096 2.3.7, 2.3.8)."),
            ("PB4", "NJTRST of the JTAG port from reset, internal pull-up, and no GPIO until it is released (SWJ_CFG = 001 frees only this pad and keeps JTAG). When this pad is used the generated code goes further (afio.mapr.disable_jtag / SwjCfg::SwdOnly, SWJ_CFG = 010): SWD on PA13/PA14 keeps working, JTAG does not."),
            ("PB3", "JTDO of the JTAG port from reset, and no GPIO until JTAG is switched off. When this pad is used the generated code does that (afio.mapr.disable_jtag / SwjCfg::SwdOnly, SWJ_CFG = 010): SWD on PA13/PA14 keeps working, JTAG does not. A debugger that turns on SWO trace keeps it as TRACESWO."),
            ("PA15", "JTDI of the JTAG port from reset, internal pull-up, and no GPIO until JTAG is switched off. When this pad is used the generated code does that (afio.mapr.disable_jtag / SwjCfg::SwdOnly, SWJ_CFG = 010): SWD on PA13/PA14 keeps working, JTAG does not."),
            ("PA14", "SWCLK/JTCK of the debug port from reset, internal pull-down. While SWD is on, GPIO settings here have no effect. Freeing it takes AFIO_MAPR.SWJ_CFG = 100 (embassy SwjCfg::Disabled; stm32f1xx-hal 0.10 has no call for it), and then a normal probe attach fails - reflash by connecting under reset (NRST held low while attaching)."),
            ("PA3", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA4", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA5", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA6", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA7", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PB0", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PB1", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PB2", "BOOT1, latched with BOOT0 on the 4th SYSCLK edge after reset and again on leaving Standby: with BOOT0 high, LOW here starts the ROM bootloader and HIGH boots from SRAM. A normal GPIO afterwards. On a Blue Pill board the BOOT1 jumper ties it to 3V3 or GND through R4 (100k)."),
            ("PC13", "Not 5 V tolerant. Fed through the backup-domain switch, which sinks only 3 mA: as an output use the 2 MHz speed with at most 30 pF (stm32f1xx-hal starts at 50 MHz - set_speed with IOPinSpeed::Mhz2) and never source current from it. On a Blue Pill board its LED D2 hangs here through R5 (510R) from 3V3, so LOW lights it."),
            ("PC14", "Not 5 V tolerant. The LSE crystal input OSC32_IN: GPIO only while the LSE is off, which takes priority. Fed through the backup-domain switch (3 mA sink): as an output use the 2 MHz speed with at most 30 pF (stm32f1xx-hal starts at 50 MHz) and never source current. On a Blue Pill board the 32.768 kHz crystal Y3 is wired here."),
            ("PC15", "Not 5 V tolerant. The LSE crystal output OSC32_OUT: GPIO only while the LSE is off, which takes priority. Fed through the backup-domain switch (3 mA sink): as an output use the 2 MHz speed with at most 30 pF (stm32f1xx-hal starts at 50 MHz) and never source current. On a Blue Pill board the 32.768 kHz crystal Y3 is wired here."),
            ("PD0", "OSC_IN after reset: the HSE crystal pin, which the default clock here (8 MHz HSE, PLL x9) uses. As GPIO it needs the clock on HSI plus AFIO_MAPR.PD01_REMAP set by hand, outputs only at the 50 MHz setting, and has no EXTI interrupt on this package. On a Blue Pill board the 8 MHz crystal sits here."),
            ("PD1", "OSC_OUT after reset: the HSE crystal pin, which the default clock here (8 MHz HSE, PLL x9) uses. As GPIO it needs the clock on HSI plus AFIO_MAPR.PD01_REMAP set by hand, outputs only at the 50 MHz setting, and has no EXTI interrupt on this package. On a Blue Pill board the 8 MHz crystal sits here."),
            ("PA0", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15. Also the WKUP pin: with PWR_CSR.EWUP set it is forced to input pull-down and a rising edge wakes the chip from Standby."),
            ("PA1", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA2", "Not 5 V tolerant: keep inputs within VDD + 0.3 V. The 5 V tolerant pads are PA8-PA15, PB2-PB4 and PB6-PB15."),
            ("PA13", "SWDIO/JTMS of the debug port from reset, internal pull-up. While SWD is on, GPIO settings here have no effect. Freeing it takes AFIO_MAPR.SWJ_CFG = 100 (embassy SwjCfg::Disabled; stm32f1xx-hal 0.10 has no call for it), and then a normal probe attach fails - reflash by connecting under reset (NRST held low while attaching)."),
            ("PA12", "USB D+, which must be pulled up with an external 1.5 kohm resistor to 3.0-3.6 V. USB and CAN share one 512-byte SRAM, so they cannot run at the same time. On a Blue Pill board that pull-up is R10, 4.7k in the maker's schematic (10k on some boards), hard-wired to this pad, so it pulls PA12 up even when used as GPIO."),
            ("PA11", "USB D-. USB and CAN share one 512-byte SRAM, so they cannot run at the same time - whether CAN sits on PA11/PA12 or on PB8/PB9."),
            ("PA10", "The ROM bootloader (BOOT0 high, BOOT1/PB2 low) talks on USART1: RX here, TX on PA9, 8 data bits, even parity, baud rate auto-detected - the port for flashing over serial without a probe."),
            ("PA9", "The ROM bootloader (BOOT0 high, BOOT1/PB2 low) talks on USART1: TX here, RX on PA10, 8 data bits, even parity, baud rate auto-detected - the port for flashing over serial without a probe."),
            ("PB12", "Also I2C2_SMBA (SMBus alert) - not I2C2's SCL, which is PB10. Erratum ES096 2.3.6: while I2C2 is clocked, PB12 as an alternate-function output is held high, so with I2C2 in use SPI2 needs software NSS and USART3 must not run synchronous."),
        ],
    },
];

/// The note pad `p` of definition `id` gets: its own, then the rule every
/// GPIO shares.
fn compose(t: &DefNotes, p: &PinDef) -> String {
    let own = t.pads.iter().find(|(name, _)| *name == p.name).map(|(_, n)| *n);
    let every = Some(t.every_gpio)
        .filter(|e| !e.is_empty() && !p.reserved && !t.not_every.contains(&p.name.as_str()));
    [own, every].into_iter().flatten().collect::<Vec<_>>().join(" ")
}

/// Write this definition's notes onto its pads, replacing whatever they had.
/// A definition with no table is left alone.
pub(crate) fn apply(def: &mut McuDefinition) {
    let Some(t) = TABLES.iter().find(|t| t.id == def.id) else {
        return;
    };
    let pins = &mut def.pins;
    for side in [&mut pins.top, &mut pins.bottom, &mut pins.left, &mut pins.right] {
        for p in side.iter_mut() {
            p.note = compose(t, p);
        }
    }
}

/// `text`, a definition's `.ron`, with each `PinDef`'s `note:` line set to
/// what `noted` gives the pad of that name - added, replaced or removed, and
/// nothing else in the file touched.
///
/// Not a parse and re-serialise: the hand-written files (the Picos, the
/// micro:bit, the C3, the F103) are laid out by hand, and the serializer would
/// rewrite all of them.
fn with_notes(text: &str, noted: &McuDefinition) -> String {
    let p = &noted.pins;
    let all: Vec<&PinDef> = p.top.iter().chain(&p.bottom).chain(&p.left).chain(&p.right).collect();
    let mut out = String::with_capacity(text.len() + 4096);
    let mut rest = text;
    while let Some(at) = rest.find("PinDef(") {
        let open = at + "PinDef(".len();
        let close = open + closing_paren(&rest[open..]);
        let body = &rest[open..close];
        out.push_str(&rest[..open]);
        out.push_str(&with_note(body, &all));
        rest = &rest[close..];
    }
    out.push_str(rest);
    out
}

/// One `PinDef(...)` body (between its parens) with its note line set.
fn with_note(body: &str, all: &[&PinDef]) -> String {
    let name_at = body.find("name: ").expect("a PinDef has a name");
    let name_line_start = body[..name_at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &body[name_line_start..name_at];
    let quoted_end = name_at + "name: ".len() + string_len(&body[name_at + "name: ".len()..]);
    let name: String = ron::from_str(&body[name_at + "name: ".len()..quoted_end]).expect("a name");
    let mut notes = all.iter().filter(|p| p.name == name).map(|p| p.note.as_str());
    let note = notes.next().unwrap_or("");
    assert!(notes.all(|n| n == note), "pads named {name:?} get different notes");

    // Drop the old line, if any.
    let mut body = body.to_owned();
    let needle = format!("\n{indent}note: ");
    if let Some(i) = body.find(&needle) {
        let line_end = body[i + 1..].find('\n').map_or(body.len(), |j| i + 1 + j);
        body.replace_range(i..line_end, "");
    }
    if note.is_empty() {
        return body;
    }
    // The serializer writes `note` last, so the new line goes right before
    // the closing paren's line - ending as the file's lines do: ron writes
    // CRLF on Windows, and git may have checked the file out either way.
    let last_nl = body.rfind('\n').expect("a multi-line PinDef");
    let cr = if body.contains("\r\n") { "\r" } else { "" };
    let quoted =
        ron::ser::to_string_pretty(note, ron::ser::PrettyConfig::default()).expect("a string");
    let line = format!("\n{indent}note: {quoted},{cr}");
    body.insert_str(last_nl, &line);
    body
}

/// Bytes up to and excluding the `)` that closes the paren already open at
/// the start of `s`, skipping strings.
fn closing_paren(s: &str) -> usize {
    let b = s.as_bytes();
    let (mut depth, mut i) = (1usize, 0usize);
    while i < b.len() {
        match b[i] {
            b'"' => i += string_len(&s[i..]) - 1,
            b'(' | b'[' => depth += 1,
            b')' | b']' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    panic!("unclosed PinDef");
}

/// Length of the quoted string literal `s` starts with, quotes included.
fn string_len(s: &str) -> usize {
    let b = s.as_bytes();
    assert_eq!(b[0], b'"');
    let mut i = 1;
    while b[i] != b'"' {
        i += if b[i] == b'\\' { 2 } else { 1 };
    }
    i + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::builtins;

    /// The kits whose notes `codegen::nrf_boards` writes.
    const NRF_KITS: [&str; 4] = ["nrf52840_dk", "nrf52832_dk", "nrf5340_dk", "nrf54l15_dk"];

    fn pads(def: &McuDefinition) -> impl Iterator<Item = &PinDef> {
        let p = &def.pins;
        p.top.iter().chain(&p.bottom).chain(&p.left).chain(&p.right)
    }

    fn ron_of(def: &McuDefinition) -> String {
        let text = ron::ser::to_string_pretty(
            def,
            ron::ser::PrettyConfig::default().struct_names(true),
        )
        .expect("serialise");
        crate::panels::mcu_module::ron_text::bare_none(&text)
    }

    /// Where two texts part, with a little of each, for an assert message.
    fn first_diff(a: &str, b: &str) -> String {
        let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
        let from = a[..at].rfind('\n').map_or(0, |i| i + 1);
        let end = |s: &str| (at + 120).min(s.len());
        format!("\n--- left\n{:?}\n--- right\n{:?}", &a[from..end(a)], &b[from..end(b)])
    }

    fn committed(id: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/assets/mcus/{id}.ron",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap_or_else(|e| panic!("{id}: {e}"))
    }

    /// Every note lands on a pad: a renamed pad must not take its note into
    /// the void. A name may cover several pads (`3V3`), but a table names it
    /// once, or the second note would silently lose.
    #[test]
    fn every_note_names_a_pad() {
        for t in TABLES {
            let def = builtins::builtin_for(t.id).unwrap_or_else(|| panic!("no built-in {}", t.id));
            assert!(!NRF_KITS.contains(&t.id), "{}: its notes live in nrf_boards", t.id);
            for (i, (name, note)) in t.pads.iter().enumerate() {
                assert!(pads(&def).any(|p| p.name == *name), "{}: no pad named {name:?}", t.id);
                assert!(
                    t.pads[..i].iter().all(|(other, _)| other != name),
                    "{}: {name:?} listed twice",
                    t.id
                );
                assert!(!note.trim().is_empty(), "{}: empty note on {name}", t.id);
                assert!(note.is_ascii(), "{}: non-ASCII note on {name}", t.id);
            }
            assert!(t.every_gpio.is_ascii(), "{}", t.id);
            for name in t.not_every {
                assert!(
                    pads(&def).any(|p| p.name == *name && !p.reserved),
                    "{}: not_every names {name:?}, no non-reserved pad",
                    t.id
                );
            }
        }
        let mut ids: Vec<_> = TABLES.iter().map(|t| t.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), TABLES.len(), "a definition listed twice");
    }

    /// The committed files say what the tables say, and no built-in outside
    /// them (or the Nordic kits) carries a note the tables do not know.
    #[test]
    fn the_committed_definitions_carry_the_notes() {
        for def in builtins::builtin_definitions() {
            if NRF_KITS.contains(&def.id.as_str()) {
                continue;
            }
            let mut want = def.clone();
            apply(&mut want);
            for (got, want) in pads(&def).zip(pads(&want)) {
                let want = if TABLES.iter().any(|t| t.id == def.id) {
                    want.note.as_str()
                } else {
                    ""
                };
                assert_eq!(
                    got.note, want,
                    "{} pad {:?} - run emit_board_notes",
                    def.id, got.name
                );
            }
        }
    }

    /// Setting the notes a file already has rewrites nothing - not the hand
    /// layout of the Picos, the micro:bit, the C3 or the F103, which a parse
    /// and re-serialise would all change.
    #[test]
    fn setting_the_notes_a_file_has_changes_nothing() {
        for def in builtins::builtin_definitions() {
            let text = committed(&def.id);
            let got = with_notes(&text, &def);
            assert!(got == text, "{}: with_notes moved something{}", def.id, first_diff(&got, &text));
        }
    }

    /// On a file the serializer DOES write back unchanged, a note set by text
    /// lands exactly where and as the serializer would put it - quotes,
    /// apostrophes and backslashes escaped its way - and removing it again
    /// restores the file.
    #[test]
    fn a_note_line_is_the_serializers() {
        for id in ["esp32c6", "rp2350_pico2_ice"] {
            let mut def = builtins::builtin_for(id).unwrap();
            assert!(ron_of(&def) == committed(id), "{id} no longer round-trips");
            let pins = &mut def.pins;
            for side in [&mut pins.top, &mut pins.bottom, &mut pins.left, &mut pins.right] {
                for p in side.iter_mut().filter(|p| !p.reserved) {
                    p.note = format!("The \"board's\" C:\\path - {}.", p.name);
                }
            }
            let text = with_notes(&committed(id), &def);
            let want = ron_of(&def);
            assert!(text == want, "{id}: not the serializer's lines{}", first_diff(&text, &want));
            let bare = builtins::builtin_for(id).unwrap();
            assert!(with_notes(&text, &bare) == committed(id), "{id}: removing them left a trace");
        }
    }

    /// Writes every definition with notes to the temp dir, to copy over
    /// `assets/mcus/`.
    #[test]
    #[ignore = "authoring tool: writes the noted definitions to the temp dir"]
    fn emit_board_notes() {
        for t in TABLES {
            let mut def = builtins::builtin_for(t.id).unwrap();
            apply(&mut def);
            let path = std::env::temp_dir().join(format!("{}.ron", t.id));
            std::fs::write(&path, with_notes(&committed(t.id), &def)).expect("write");
            println!("wrote {}", path.display());
        }
    }
}
