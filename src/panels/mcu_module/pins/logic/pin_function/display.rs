use super::enum_::PinFunction;

// ── Display helpers ──────────────────────────────────────────────────────────

/// What this chip calls its SD-card block: unit 0 is the un-numbered `SDIO`
/// of F1/F2/F4/L1, everything else is `SDMMC{n}`. The same name the peripheral
/// singleton goes by in embassy, which is why it lives in one place.
pub fn sdmmc_name(unit: u8) -> String {
    if unit == 0 {
        "SDIO".to_owned()
    } else {
        format!("SDMMC{unit}")
    }
}

/// The letter a SAI sub-block goes by: 1 is A, everything else is B.
pub fn sai_block(block: u8) -> &'static str {
    if block == 1 { "A" } else { "B" }
}

// ── Grouping a pin's functions by peripheral instance ───────────────────────
//
// Both lists of a pin's functions - the Peripherals tab's signal menu and the
// Pins tab's in-chip list - shade every other instance (TIM0 on a band, TIM1
// plain, TIM2 on a band, ...), so a long list reads as groups. One rule here,
// so the two lists cannot group differently.

/// The peripheral instance a function's `label()` names: its first word
/// (`TIM0  CH2  (PWM)` -> `TIM0`, `GPIO Output` -> `GPIO`), cut at the first
/// underscore for a datasheet signal name (`COMP1_INP` -> `COMP1`).
pub fn label_instance(label: &str) -> &str {
    let word = label.split_whitespace().next().unwrap_or(label);
    word.split('_').next().unwrap_or(word)
}

/// `labels` as runs of consecutive entries naming one instance, in their own
/// order. Nothing is reordered: an instance whose entries are not next to
/// each other makes two runs.
pub fn instance_runs<S: AsRef<str>>(labels: &[S]) -> Vec<std::ops::Range<usize>> {
    let mut runs: Vec<std::ops::Range<usize>> = Vec::new();
    for (i, label) in labels.iter().enumerate() {
        match runs.last_mut() {
            Some(r)
                if label_instance(labels[r.start].as_ref()) == label_instance(label.as_ref()) =>
            {
                r.end = i + 1;
            }
            _ => runs.push(i..i + 1),
        }
    }
    runs
}

/// Is run `index` of `count` on a band? Every other one, the first included -
/// and none when the list holds a single instance, where a band would
/// separate nothing.
pub fn run_shaded(index: usize, count: usize) -> bool {
    count > 1 && index.is_multiple_of(2)
}

impl PinFunction {
    /// Short, filesystem-safe token for the selected function, used as the
    /// suffix of a generated pin file name (`pin<n>_<name>_<token>.rs`, e.g.
    /// `pin2_pc13_out.rs`). Always a valid Rust identifier so it can also be a
    /// `pub mod` name. Peripheral numbers are intentionally omitted — the pin
    /// number already makes each file name unique.
    pub fn file_token(&self) -> &'static str {
        match self {
            PinFunction::Unset => "unset",
            PinFunction::GpioInput => "in",
            PinFunction::GpioOutput => "out",
            PinFunction::GpioAnalog => "analog",
            PinFunction::AdcChannel { .. } => "adc",
            PinFunction::TouchPad(..) => "touch",
            PinFunction::LcdCamData { .. } => "lcd_d",
            PinFunction::LcdCamDc => "lcd_dc",
            PinFunction::LcdCamWr => "lcd_wr",
            PinFunction::LcdCamCs => "lcd_cs",
            PinFunction::LcdCamPclk => "lcd_pclk",
            PinFunction::LcdCamVsync => "lcd_vsync",
            PinFunction::LcdCamHsync => "lcd_hsync",
            PinFunction::LcdCamDe => "lcd_de",
            PinFunction::CamData { .. } => "cam_d",
            PinFunction::CamPclk => "cam_pclk",
            PinFunction::CamVsync => "cam_vsync",
            PinFunction::CamHsync => "cam_hsync",
            PinFunction::CamHenable => "cam_href",
            PinFunction::CamMclk => "cam_mclk",
            PinFunction::ParlData { .. } => "parl_d",
            PinFunction::ParlClk => "parl_clk",
            PinFunction::ParlValid => "parl_valid",
            PinFunction::ParlRxData { .. } => "parl_rx_d",
            PinFunction::ParlRxClk => "parl_rx_clk",
            PinFunction::ParlRxValid => "parl_rx_valid",
            PinFunction::McpwmA { .. } => "mcpwm_a",
            PinFunction::McpwmB { .. } => "mcpwm_b",
            PinFunction::PcntEdge { .. } => "pcnt_edge",
            PinFunction::PcntCtrl { .. } => "pcnt_ctrl",
            PinFunction::RmtChannel(..) => "rmt",
            PinFunction::TimerPwm { .. } => "pwm",
            PinFunction::TimerPwmN { .. } => "pwmn",
            PinFunction::TimerBreak { .. } => "bkin",
            PinFunction::DacOut { .. } => "dac",
            PinFunction::HspiClk { .. } => "hspi_clk",
            PinFunction::HspiNcs { .. } => "hspi_ncs",
            PinFunction::HspiDqs { .. } => "hspi_dqs",
            PinFunction::HspiIo { .. } => "hspi_io",
            PinFunction::XspiClk { .. } => "xspi_clk",
            PinFunction::XspiNcs { .. } => "xspi_ncs",
            PinFunction::XspiDqs { .. } => "xspi_dqs",
            PinFunction::XspiIo { .. } => "xspi_io",
            PinFunction::OspiClk { .. } => "ospi_clk",
            PinFunction::OspiNcs { .. } => "ospi_ncs",
            PinFunction::OspiDqs { .. } => "ospi_dqs",
            PinFunction::OspiIo { .. } => "ospi_io",
            PinFunction::QspiClk => "qspi_clk",
            PinFunction::QspiNcs { .. } => "qspi_ncs",
            PinFunction::QspiIo { .. } => "qspi_io",
            PinFunction::SdmmcCk { .. } => "sdmmc_ck",
            PinFunction::SdmmcCmd { .. } => "sdmmc_cmd",
            PinFunction::SdmmcD { .. } => "sdmmc_d",
            PinFunction::SaiSck { .. } => "sai_sck",
            PinFunction::SaiSd { .. } => "sai_sd",
            PinFunction::SaiFs { .. } => "sai_fs",
            PinFunction::SaiMclk { .. } => "sai_mclk",
            PinFunction::I2sCk(_) => "i2s_ck",
            PinFunction::I2sWs(_) => "i2s_ws",
            PinFunction::I2sSd(_) => "i2s_sd",
            PinFunction::I2sMck(_) => "i2s_mck",
            PinFunction::UsartTx(_) => "uart_tx",
            PinFunction::UsartRx(_) => "uart_rx",
            PinFunction::UsartCts(_) => "uart_cts",
            PinFunction::UsartRts(_) => "uart_rts",
            PinFunction::UsartCk(_) => "uart_ck",
            PinFunction::LpuartTx(_) => "lpuart_tx",
            PinFunction::LpuartRx(_) => "lpuart_rx",
            PinFunction::LpuartCts(_) => "lpuart_cts",
            PinFunction::LpuartRts(_) => "lpuart_rts",
            PinFunction::SpiNss(_) => "spi_nss",
            PinFunction::SpiSck(_) => "spi_sck",
            PinFunction::SpiMiso(_) => "spi_miso",
            PinFunction::SpiMosi(_) => "spi_mosi",
            PinFunction::SpiRdy(_) => "spi_rdy",
            PinFunction::I2cScl(_) => "i2c_scl",
            PinFunction::I2cSda(_) => "i2c_sda",
            PinFunction::UsbDm => "usb_dm",
            PinFunction::UsbDp => "usb_dp",
            PinFunction::CanRx => "can_rx",
            PinFunction::CanTx => "can_tx",
            PinFunction::SwdIo => "swdio",
            PinFunction::SwdClk => "swclk",
            PinFunction::Mco => "mco",
            // The pin number already makes the file name unique, so a single
            // token is enough for every generic alternate function.
            PinFunction::Other(_) => "af",
        }
    }

    /// Full label shown on the function button
    pub fn label(&self) -> String {
        match self {
            PinFunction::Unset => "Not configured".into(),
            PinFunction::GpioInput => "GPIO Input".into(),
            PinFunction::GpioOutput => "GPIO Output".into(),
            PinFunction::GpioAnalog => "GPIO Analog".into(),
            PinFunction::AdcChannel { adc, channel } => format!("ADC{adc}  IN{channel}"),
            PinFunction::TouchPad(n) => format!("TOUCH{n}"),
            PinFunction::LcdCamData { lane } => format!("LCD  D{lane}"),
            PinFunction::LcdCamDc => "LCD  DC".to_owned(),
            PinFunction::LcdCamWr => "LCD  WR".to_owned(),
            PinFunction::LcdCamCs => "LCD  CS".to_owned(),
            PinFunction::LcdCamPclk => "LCD  PCLK".to_owned(),
            PinFunction::LcdCamVsync => "LCD  VSYNC".to_owned(),
            PinFunction::LcdCamHsync => "LCD  HSYNC".to_owned(),
            PinFunction::LcdCamDe => "LCD  DE".to_owned(),
            PinFunction::CamData { lane } => format!("CAM  D{lane}"),
            PinFunction::CamPclk => "CAM  PCLK".to_owned(),
            PinFunction::CamVsync => "CAM  VSYNC".to_owned(),
            PinFunction::CamHsync => "CAM  HSYNC".to_owned(),
            PinFunction::CamHenable => "CAM  HREF".to_owned(),
            PinFunction::CamMclk => "CAM  MCLK".to_owned(),
            PinFunction::ParlData { lane } => format!("PARL  D{lane}"),
            PinFunction::ParlClk => "PARL  CLK".to_owned(),
            PinFunction::ParlValid => "PARL  VALID".to_owned(),
            PinFunction::ParlRxData { lane } => format!("PARL  RXD{lane}"),
            PinFunction::ParlRxClk => "PARL  RXCLK".to_owned(),
            PinFunction::ParlRxValid => "PARL  RXVALID".to_owned(),
            PinFunction::McpwmA { unit, operator } => format!("MCPWM{unit}  OP{operator}A"),
            PinFunction::McpwmB { unit, operator } => format!("MCPWM{unit}  OP{operator}B"),
            PinFunction::PcntEdge { unit, channel } => format!("PCNT{unit}  EDGE{channel}"),
            PinFunction::PcntCtrl { unit, channel } => format!("PCNT{unit}  CTRL{channel}"),
            PinFunction::RmtChannel(n) => format!("RMT  CH{n}"),
            PinFunction::TimerPwm { timer, channel } => format!("TIM{timer}  CH{channel}  (PWM)"),
            PinFunction::TimerPwmN { timer, channel } => {
                format!("TIM{timer}  CH{channel}N  (PWM)")
            }
            PinFunction::TimerBreak { timer, input } => {
                let n = if *input == 1 {
                    String::new()
                } else {
                    input.to_string()
                };
                format!("TIM{timer}  BKIN{n}  (break)")
            }
            PinFunction::DacOut { dac, channel } => format!("DAC{dac}  OUT{channel}"),
            PinFunction::HspiClk { unit } => format!("HSPI{unit}  CLK"),
            PinFunction::HspiNcs { unit } => format!("HSPI{unit}  NCS"),
            PinFunction::HspiDqs { unit, index } => format!("HSPI{unit}  DQS{index}"),
            PinFunction::HspiIo { unit, lane } => format!("HSPI{unit}  IO{lane}"),
            PinFunction::XspiClk { port } => format!("XSPI  P{port}  CLK"),
            PinFunction::XspiNcs { port, cs } => format!("XSPI  P{port}  NCS{cs}"),
            PinFunction::XspiDqs { port, index } => format!("XSPI  P{port}  DQS{index}"),
            PinFunction::XspiIo { port, lane } => format!("XSPI  P{port}  IO{lane}"),
            PinFunction::OspiClk { port } => format!("OCTOSPI  P{port}  CLK"),
            PinFunction::OspiNcs { port } => format!("OCTOSPI  P{port}  NCS"),
            PinFunction::OspiDqs { port } => format!("OCTOSPI  P{port}  DQS"),
            PinFunction::OspiIo { port, lane } => format!("OCTOSPI  P{port}  IO{lane}"),
            PinFunction::QspiClk => "QUADSPI  CLK".to_owned(),
            PinFunction::QspiNcs { bank } => format!("QUADSPI  BK{bank}  NCS"),
            PinFunction::QspiIo { bank, lane } => format!("QUADSPI  BK{bank}  IO{lane}"),
            PinFunction::SdmmcCk { unit } => format!("{}  CK", sdmmc_name(*unit)),
            PinFunction::SdmmcCmd { unit } => format!("{}  CMD", sdmmc_name(*unit)),
            PinFunction::SdmmcD { unit, lane } => format!("{}  D{lane}", sdmmc_name(*unit)),
            PinFunction::SaiSck { sai, block } => format!("SAI{sai}  {}  SCK", sai_block(*block)),
            PinFunction::SaiSd { sai, block } => format!("SAI{sai}  {}  SD", sai_block(*block)),
            PinFunction::SaiFs { sai, block } => format!("SAI{sai}  {}  FS", sai_block(*block)),
            PinFunction::SaiMclk { sai, block } => {
                format!("SAI{sai}  {}  MCLK", sai_block(*block))
            }
            PinFunction::I2sCk(n) => format!("I2S{n}  CK"),
            PinFunction::I2sWs(n) => format!("I2S{n}  WS"),
            PinFunction::I2sSd(n) => format!("I2S{n}  SD"),
            PinFunction::I2sMck(n) => format!("I2S{n}  MCK"),
            PinFunction::UsartTx(n) => format!("USART{n}  TX"),
            PinFunction::UsartRx(n) => format!("USART{n}  RX"),
            PinFunction::UsartCts(n) => format!("USART{n}  CTS"),
            PinFunction::UsartRts(n) => format!("USART{n}  RTS"),
            PinFunction::UsartCk(n) => format!("USART{n}  CK"),
            PinFunction::LpuartTx(n) => format!("LPUART{n}  TX"),
            PinFunction::LpuartRx(n) => format!("LPUART{n}  RX"),
            PinFunction::LpuartCts(n) => format!("LPUART{n}  CTS"),
            PinFunction::LpuartRts(n) => format!("LPUART{n}  RTS"),
            PinFunction::SpiNss(n) => format!("SPI{n}  NSS"),
            PinFunction::SpiSck(n) => format!("SPI{n}  SCK"),
            PinFunction::SpiMiso(n) => format!("SPI{n}  MISO"),
            PinFunction::SpiMosi(n) => format!("SPI{n}  MOSI"),
            PinFunction::SpiRdy(n) => format!("SPI{n}  RDY"),
            PinFunction::I2cScl(n) => format!("I2C{n}  SCL"),
            PinFunction::I2cSda(n) => format!("I2C{n}  SDA"),
            PinFunction::UsbDm => "USB  D−".into(),
            PinFunction::UsbDp => "USB  D+".into(),
            PinFunction::CanRx => "CAN  RX".into(),
            PinFunction::CanTx => "CAN  TX".into(),
            PinFunction::SwdIo => "SWD  IO  (JTMS)".into(),
            PinFunction::SwdClk => "SWD  CLK  (JTCK)".into(),
            PinFunction::Mco => "MCO  (Master Clock Out)".into(),
            // The datasheet's own signal name is the label.
            PinFunction::Other(name) => name.clone(),
        }
    }

    /// Reverse of `label()` — parses a label string back into a `PinFunction`.
    ///
    /// Used by `codegen::parse_main_rs()` to restore pin state from an
    /// existing `src/main.rs` when opening a project.
    /// Returns `None` for unrecognised or unsupported labels (e.g. "Not configured").
    pub fn from_label(s: &str) -> Option<PinFunction> {
        // ── Fixed / parameter-free variants ──────────────────────────────────
        match s {
            "GPIO Input" => return Some(PinFunction::GpioInput),
            "GPIO Output" => return Some(PinFunction::GpioOutput),
            "GPIO Analog" => return Some(PinFunction::GpioAnalog),
            "USB  D−" => return Some(PinFunction::UsbDm),
            "USB  D+" => return Some(PinFunction::UsbDp),
            "CAN  RX" => return Some(PinFunction::CanRx),
            "CAN  TX" => return Some(PinFunction::CanTx),
            "SWD  IO  (JTMS)" => return Some(PinFunction::SwdIo),
            "SWD  CLK  (JTCK)" => return Some(PinFunction::SwdClk),
            "MCO  (Master Clock Out)" => return Some(PinFunction::Mco),
            _ => {}
        }

        // ── LPUART{n}  {role} ────────────────────────────────────────────────
        // Checked BEFORE USART is irrelevant (the prefixes differ at index 0),
        // but keep it first so the intent is obvious.
        if let Some(rest) = s.strip_prefix("LPUART") {
            if let Some((num_str, role)) = rest.split_once("  ") {
                if let Ok(n) = num_str.parse::<u8>() {
                    return match role {
                        "TX" => Some(PinFunction::LpuartTx(n)),
                        "RX" => Some(PinFunction::LpuartRx(n)),
                        "CTS" => Some(PinFunction::LpuartCts(n)),
                        "RTS" => Some(PinFunction::LpuartRts(n)),
                        _ => None,
                    };
                }
            }
        }

        // ── USART{n}  {role} ─────────────────────────────────────────────────
        if let Some(rest) = s.strip_prefix("USART") {
            if let Some((num_str, role)) = rest.split_once("  ") {
                if let Ok(n) = num_str.parse::<u8>() {
                    return match role {
                        "TX" => Some(PinFunction::UsartTx(n)),
                        "RX" => Some(PinFunction::UsartRx(n)),
                        "CTS" => Some(PinFunction::UsartCts(n)),
                        "RTS" => Some(PinFunction::UsartRts(n)),
                        "CK" => Some(PinFunction::UsartCk(n)),
                        _ => None,
                    };
                }
            }
        }

        // ── SPI{n}  {role} ───────────────────────────────────────────────────
        if let Some(rest) = s.strip_prefix("SPI") {
            if let Some((num_str, role)) = rest.split_once("  ") {
                if let Ok(n) = num_str.parse::<u8>() {
                    return match role {
                        "NSS" => Some(PinFunction::SpiNss(n)),
                        "SCK" => Some(PinFunction::SpiSck(n)),
                        "MISO" => Some(PinFunction::SpiMiso(n)),
                        "MOSI" => Some(PinFunction::SpiMosi(n)),
                        "RDY" => Some(PinFunction::SpiRdy(n)),
                        _ => None,
                    };
                }
            }
        }

        // ── I2C{n}  {role} ───────────────────────────────────────────────────
        if let Some(rest) = s.strip_prefix("I2C") {
            if let Some((num_str, role)) = rest.split_once("  ") {
                if let Ok(n) = num_str.parse::<u8>() {
                    return match role {
                        "SCL" => Some(PinFunction::I2cScl(n)),
                        "SDA" => Some(PinFunction::I2cSda(n)),
                        _ => None,
                    };
                }
            }
        }

        // ── ADC{adc}  IN{channel} ────────────────────────────────────────────
        // label() → format!("ADC{adc}  IN{channel}")
        if let Some(rest) = s.strip_prefix("ADC") {
            // rest = "1  IN3"  →  split on "  IN"
            if let Some((adc_str, ch_str)) = rest.split_once("  IN") {
                if let (Ok(adc), Ok(channel)) = (adc_str.parse::<u8>(), ch_str.parse::<u8>()) {
                    return Some(PinFunction::AdcChannel { adc, channel });
                }
            }
        }

        // ── TIM{timer}  CH{channel}  (PWM) ───────────────────────────────────
        // label() → format!("TIM{timer}  CH{channel}  (PWM)")
        if let Some(rest) = s.strip_prefix("TIM") {
            // ── TIM{timer}  BKIN[2]  (break) ─────────────────────────────────
            if let Some((timer_str, tail)) = rest.split_once("  BKIN") {
                if let Ok(timer) = timer_str.parse::<u8>() {
                    let input = if tail.starts_with('2') { 2 } else { 1 };
                    return Some(PinFunction::TimerBreak { timer, input });
                }
            }
            // rest = "2  CH1  (PWM)"
            if let Some((timer_str, ch_rest)) = rest.split_once("  CH") {
                // ch_rest = "1  (PWM)" — take everything up to the next "  "
                let ch_num_str = ch_rest.split("  ").next().unwrap_or("");
                // A trailing N is the complementary output: "CH1N", not CH1.
                let (ch_num_str, complementary) = match ch_num_str.strip_suffix('N') {
                    Some(head) => (head, true),
                    None => (ch_num_str, false),
                };
                if let (Ok(timer), Ok(channel)) =
                    (timer_str.parse::<u8>(), ch_num_str.parse::<u8>())
                {
                    return Some(if complementary {
                        PinFunction::TimerPwmN { timer, channel }
                    } else {
                        PinFunction::TimerPwm { timer, channel }
                    });
                }
            }
        }

        // ── Generic alternate function ───────────────────────────────────────
        // `label()` for `Other` is the datasheet signal name itself, which is
        // always UPPERCASE alphanumerics / `_` / `-` and never contains the
        // double space the labels above use — so this can't shadow them.
        if !s.is_empty()
            && s.starts_with(|c: char| c.is_ascii_uppercase())
            && !s.contains("  ")
            && s.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Some(PinFunction::Other(s.to_owned()));
        }

        None
    }

    /// Returns the stm32f1xx-hal GPIO mode type name used to generate
    /// `pub type PinType = Pin<PORT, N, MODE>;` in the per-pin source file.
    /// Returns `None` for `Unset` (no file should be written).
    pub fn hal_gpio_mode(&self) -> Option<&'static str> {
        match self {
            PinFunction::Unset => None,
            PinFunction::GpioInput => Some("Input"),
            PinFunction::GpioOutput => Some("Output"),
            PinFunction::AdcChannel { .. } => Some("Analog"),
            // Everything else is an alternate-function pin
            _ => Some("Alternate"),
        }
    }

    /// Short badge shown on the pin in the diagram
    /// The label for the function list inside the chip: the full name, with
    /// the short tag in front ONLY when it says something the full name does
    /// not.
    ///
    /// The list used to read `OUT  —  GPIO Output`, `SDA  —  I2C3 SDA`,
    /// `DCMIPP_D8  —  DCMIPP_D8`. The tag is the same word twice in almost
    /// every row, which costs a third of the width and buys nothing.
    ///
    /// "Says something new" is decided by SUBSEQUENCE, not equality: the tag
    /// is redundant when each of its letters and digits appears in the full
    /// name in the same order. Substring would have been the obvious test but
    /// it keeps `LPTX  —  LPUART1 TX`, where the tag is plainly redundant -
    /// its letters are all there, just not adjacent.
    ///
    /// With today's table every tag turns out redundant, so the list shows
    /// full names only. The rule stays anyway: it is what a NEW function is
    /// measured against, and a tag that does earn its place will show up
    /// without anyone having to notice.
    ///
    /// The subsequence test is loose — `OUT` is a subsequence of
    /// `GPIO Input` — and that is safe only because the two strings always
    /// come from the SAME variant, read off `self` here. Do not reuse
    /// [`tag_is_redundant`] to compare a tag with somebody else's name.
    pub fn list_label(&self) -> String {
        let long = self.label();
        if tag_is_redundant(self.short_label(), &long) {
            long
        } else {
            format!("{}  —  {}", self.short_label(), long)
        }
    }

    pub fn short_label(&self) -> &str {
        match self {
            PinFunction::Unset => "—",
            PinFunction::GpioInput => "IN",
            PinFunction::GpioOutput => "OUT",
            PinFunction::GpioAnalog => "ANA",
            PinFunction::AdcChannel { .. } => "ADC",
            // Both PWM shapes share a tag: the label already spells out which
            // channel, and CH1N reads as complementary on its own.
            PinFunction::TimerPwm { .. } | PinFunction::TimerPwmN { .. } => "PWM",
            PinFunction::RmtChannel(..) => "RMT",
            PinFunction::PcntEdge { .. } | PinFunction::PcntCtrl { .. } => "PCNT",
            PinFunction::McpwmA { .. } | PinFunction::McpwmB { .. } => "MCPWM",
            PinFunction::TouchPad(..) => "TOUCH",
            PinFunction::LcdCamData { .. }
            | PinFunction::LcdCamDc
            | PinFunction::LcdCamWr
            | PinFunction::LcdCamCs
            | PinFunction::LcdCamPclk
            | PinFunction::LcdCamVsync
            | PinFunction::LcdCamHsync
            | PinFunction::LcdCamDe => "LCD",
            PinFunction::CamData { .. }
            | PinFunction::CamPclk
            | PinFunction::CamVsync
            | PinFunction::CamHsync
            | PinFunction::CamHenable
            | PinFunction::CamMclk => "CAM",
            PinFunction::ParlData { .. }
            | PinFunction::ParlClk
            | PinFunction::ParlValid
            | PinFunction::ParlRxData { .. }
            | PinFunction::ParlRxClk
            | PinFunction::ParlRxValid => "PARL",
            PinFunction::TimerBreak { .. } => "BRK",
            PinFunction::DacOut { .. } => "DAC",
            PinFunction::HspiClk { .. }
            | PinFunction::HspiNcs { .. }
            | PinFunction::HspiDqs { .. }
            | PinFunction::HspiIo { .. } => "HSPI",
            PinFunction::XspiClk { .. }
            | PinFunction::XspiNcs { .. }
            | PinFunction::XspiDqs { .. }
            | PinFunction::XspiIo { .. } => "XSPI",
            PinFunction::OspiClk { .. }
            | PinFunction::OspiNcs { .. }
            | PinFunction::OspiDqs { .. }
            | PinFunction::OspiIo { .. } => "OSPI",
            PinFunction::QspiClk | PinFunction::QspiNcs { .. } | PinFunction::QspiIo { .. } => {
                "QSPI"
            }
            PinFunction::SdmmcCk { .. }
            | PinFunction::SdmmcCmd { .. }
            | PinFunction::SdmmcD { .. } => "SDMMC",
            PinFunction::SaiSck { .. }
            | PinFunction::SaiSd { .. }
            | PinFunction::SaiFs { .. }
            | PinFunction::SaiMclk { .. } => "SAI",
            PinFunction::I2sCk(_)
            | PinFunction::I2sWs(_)
            | PinFunction::I2sSd(_)
            | PinFunction::I2sMck(_) => "I2S",
            PinFunction::UsartTx(_) => "TX",
            PinFunction::UsartRx(_) => "RX",
            PinFunction::UsartCts(_) => "CTS",
            PinFunction::UsartRts(_) => "RTS",
            PinFunction::UsartCk(_) => "CK",
            PinFunction::LpuartTx(_) => "LPTX",
            PinFunction::LpuartRx(_) => "LPRX",
            PinFunction::LpuartCts(_) => "LPCTS",
            PinFunction::LpuartRts(_) => "LPRTS",
            PinFunction::SpiNss(_) => "NSS",
            PinFunction::SpiSck(_) => "SCK",
            PinFunction::SpiMiso(_) => "MISO",
            PinFunction::SpiMosi(_) => "MOSI",
            PinFunction::SpiRdy(_) => "RDY",
            PinFunction::I2cScl(_) => "SCL",
            PinFunction::I2cSda(_) => "SDA",
            PinFunction::UsbDm => "D−",
            PinFunction::UsbDp => "D+",
            PinFunction::CanRx => "CAN",
            PinFunction::CanTx => "CAN",
            PinFunction::SwdIo => "SWD",
            PinFunction::SwdClk => "SWD",
            PinFunction::Mco => "MCO",
            PinFunction::Other(name) => name,
        }
    }
}

/// Is `tag` already spelled out inside `long`?
///
/// Letters and digits only, case-insensitive, in order but not necessarily
/// adjacent. Punctuation and spacing are ignored on both sides, so `D−`
/// against `USB  D−` and `PWM` against `TIM4  CH3  (PWM)` both count.
/// An empty tag (the placeholder for an unset pin) is redundant by
/// definition.
fn tag_is_redundant(tag: &str, long: &str) -> bool {
    let keep = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(|c| c.to_lowercase())
            .collect()
    };
    let (tag, long) = (keep(tag), keep(long));
    let mut it = long.into_iter();
    tag.into_iter().all(|c| it.any(|l| l == c))
}

#[cfg(test)]
mod list_label_tests {
    use super::super::enum_::PinFunction;
    use super::tag_is_redundant;

    /// The rows from the report, each losing a tag that repeated the name.
    #[test]
    fn the_redundant_tags_are_gone() {
        let cases = [
            (PinFunction::GpioOutput, "GPIO Output"),
            (PinFunction::GpioInput, "GPIO Input"),
            (PinFunction::GpioAnalog, "GPIO Analog"),
            (PinFunction::I2cSda(3), "I2C3  SDA"),
            (PinFunction::UsartRx(1), "USART1  RX"),
            (PinFunction::UsartCts(8), "USART8  CTS"),
            (PinFunction::Other("DCMIPP_D8".into()), "DCMIPP_D8"),
        ];
        for (f, want) in cases {
            assert_eq!(f.list_label(), want, "{f:?}");
        }
    }

    /// Two the substring rule would have kept: the tag is spelled out, just
    /// not contiguously.
    #[test]
    fn a_split_tag_counts_as_redundant_too() {
        assert_eq!(
            PinFunction::TimerPwm {
                timer: 4,
                channel: 3
            }
            .list_label(),
            "TIM4  CH3  (PWM)"
        );
        assert_eq!(PinFunction::LpuartTx(1).list_label(), "LPUART1  TX");
    }

    /// Every variant the enum has today is redundant, so nothing in the list
    /// carries a tag. Pinned so that adding a function with a tag that DOES
    /// earn its place shows up here as a deliberate change rather than a
    /// surprise in the UI.
    #[test]
    fn nothing_in_the_current_table_needs_its_tag() {
        let all = [
            PinFunction::Unset,
            PinFunction::GpioInput,
            PinFunction::GpioOutput,
            PinFunction::GpioAnalog,
            PinFunction::AdcChannel { adc: 1, channel: 0 },
            PinFunction::TimerPwm {
                timer: 2,
                channel: 1,
            },
            PinFunction::TimerPwmN {
                timer: 1,
                channel: 1,
            },
            PinFunction::UsartTx(1),
            PinFunction::UsartRx(1),
            PinFunction::UsartCts(1),
            PinFunction::UsartRts(1),
            PinFunction::UsartCk(1),
            PinFunction::LpuartTx(1),
            PinFunction::LpuartRx(1),
            PinFunction::LpuartCts(1),
            PinFunction::LpuartRts(1),
            PinFunction::SpiNss(1),
            PinFunction::SpiSck(1),
            PinFunction::SpiMiso(1),
            PinFunction::SpiMosi(1),
            PinFunction::SpiRdy(1),
            PinFunction::I2cScl(1),
            PinFunction::I2cSda(1),
            PinFunction::UsbDm,
            PinFunction::UsbDp,
            PinFunction::CanRx,
            PinFunction::CanTx,
            PinFunction::SwdIo,
            PinFunction::SwdClk,
            PinFunction::Mco,
        ];
        for f in all {
            assert_eq!(f.list_label(), f.label(), "{f:?} still carries a tag");
        }
    }

    /// …and the predicate itself refuses a tag that is genuinely new.
    #[test]
    fn a_tag_that_adds_something_survives() {
        assert!(!tag_is_redundant("QSPI", "OCTOSPI1  IO0"));
        // Order matters: the same letters the wrong way round is not a match.
        assert!(!tag_is_redundant("XT", "USART1  TX"));
    }

    /// The rule is LOOSE, and knowing where is the point of this test.
    #[test]
    fn scattered_letters_can_match_by_accident() {
        // "OUT" is a subsequence of "GPIO Input" - o, u, t all appear in
        // order. A mismatched pair like this would be judged redundant.
        assert!(tag_is_redundant("OUT", "GPIO Input"));
        // It never bites, because a tag is only ever compared with the name
        // of the SAME function: `list_label` reads both off `self`. Recording
        // it so nobody reuses the predicate somewhere it could.
    }
}

/// The grouping both pin lists shade by.
#[cfg(test)]
mod instance_group_tests {
    use super::{instance_runs, label_instance, run_shaded};

    /// The instance is the label's first word, cut at an underscore for a
    /// datasheet signal name.
    #[test]
    fn the_group_is_the_peripheral_instance() {
        assert_eq!(label_instance("TIM0  CH2  (PWM)"), "TIM0");
        assert_eq!(label_instance("TIM1  CH1N  (PWM)"), "TIM1");
        assert_eq!(label_instance("USART2  TX"), "USART2");
        assert_eq!(label_instance("ADC1  IN5"), "ADC1");
        assert_eq!(label_instance("GPIO Output"), "GPIO");
        assert_eq!(label_instance("COMP1_INP"), "COMP1");
        assert_eq!(label_instance("RTC"), "RTC");
    }

    /// Four timers of four channels: four groups, TIM0 and TIM2 shaded.
    #[test]
    fn four_timers_make_four_groups_and_every_other_one_is_shaded() {
        let labels: Vec<String> = (0..4)
            .flat_map(|t| (0..4).map(move |c| format!("TIM{t}  CH{c}  (PWM)")))
            .collect();
        let runs = instance_runs(&labels);
        assert_eq!(runs, vec![0..4, 4..8, 8..12, 12..16]);
        let shaded: Vec<bool> = (0..runs.len()).map(|i| run_shaded(i, runs.len())).collect();
        assert_eq!(shaded, vec![true, false, true, false]);
    }

    /// One instance is one group, and a band would separate nothing.
    #[test]
    fn a_single_instance_gets_no_band() {
        let runs = instance_runs(&["GPIO Input", "GPIO Output"]);
        assert_eq!(runs, vec![0..2]);
        assert!(!run_shaded(0, runs.len()));
        let runs = instance_runs(&["COMP1_INP", "COMP1_INM", "COMP2_INP"]);
        assert_eq!(runs, vec![0..2, 2..3]);
    }

    /// Nothing is reordered: an instance split by another makes two runs.
    #[test]
    fn the_entries_keep_their_order() {
        let runs = instance_runs(&["SPI1  SCK", "SPI2  SCK", "SPI1  MOSI"]);
        assert_eq!(runs, vec![0..1, 1..2, 2..3]);
        assert!(instance_runs::<&str>(&[]).is_empty());
    }
}

#[cfg(test)]
mod config_label_round_trip {
    use super::super::enum_::PinFunction;

    /// `@pins` in `mcu.config` stores a function as `label()` and reads it
    /// back with `from_label`. The reverse was written for the codegen
    /// comments, which only the STM32 and ESP shapes ever went through - so
    /// every function the micro:bit offers is checked here, before the nRF
    /// store is allowed to depend on it.
    #[test]
    fn every_function_the_microbit_offers_survives_from_label_of_label() {
        let mcu = crate::panels::mcu_module::builtins::builtin_definitions()
            .into_iter()
            .find(|d| d.id == "nrf52833_microbit_v2")
            .expect("built-in micro:bit v2")
            .build_mcu();
        let mut seen = 0;
        for pin in mcu.iter_all_pins().filter(|p| !p.reserved) {
            for f in &pin.available_functions {
                if *f == PinFunction::Unset {
                    continue;
                }
                seen += 1;
                assert_eq!(
                    PinFunction::from_label(&f.label()).as_ref(),
                    Some(f),
                    "{}: {f:?} is written as {:?} and does not read back",
                    pin.name,
                    f.label()
                );
            }
        }
        assert!(seen > 0, "the board offers functions");
    }
}
