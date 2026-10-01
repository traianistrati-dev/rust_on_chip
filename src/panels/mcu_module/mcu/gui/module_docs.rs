//! What a Virtual Module's config rows MEAN, and the collector that carries it
//! to the info pane.
//!
//! # Why the text lives here and not at the row
//!
//! A field can be described in three places: the `///` on the struct in
//! `modules/model.rs`, the `.on_hover_text(...)` on its widget, and now the
//! details pane. Three copies of one sentence is three chances to drift, and
//! the one that drifts is always the copy nobody is looking at.
//!
//! So the sentence is a `pub const` here, and the row site names it twice — once
//! for the hover, once for the collector:
//!
//! ```ignore
//! ui.label("Baud rate");
//! …combo…
//! out.field("Baud rate", docs::USART_BAUD);
//! ```
//!
//! There is one string. Keeping the two in step is not a discipline, it is a
//! `&'static str` binding the compiler checks.
//!
//! # What this is NOT
//!
//! * Not generated from the `///` comments in `model.rs`. Those are written for
//!   whoever maintains this code — "`#[serde(default)]` keeps old `@modules`
//!   markers valid" — and address a different reader. They stay where they are,
//!   and they are not canonical for the UI.
//! * Not a per-field table keyed by identifier. `buf_len` is drawn as
//!   "RX/TX buffer" or "RX DMA buffer" depending on the transport, and it is the
//!   LABEL the reader saw that the pane has to explain.
//! * Not a description of the generated code. That is `codegen`'s to state, and
//!   restating it here is what would rot first.

use std::borrow::Cow;

/// One documented row, as the panel actually drew it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldDoc {
    /// The label the row drew — "RX DMA buffer", not `buf_len`. Owned, because
    /// several are built with `format!` from the wiring.
    pub label: String,
    /// The sentence, usually a `const` from this module.
    pub doc: Cow<'static, str>,
}

/// Everything a config arm has to say that is not a control.
///
/// Replaces the bare `notes: &mut Vec<String>` the arms used to take. One
/// parameter still, on a function that already carries sixteen.
#[derive(Default, Debug)]
pub struct ConfigOut {
    notes: Vec<String>,
    fields: Vec<FieldDoc>,
    elsewhere: Vec<FieldDoc>,
    skipped: Vec<FieldDoc>,
    complete: bool,
}

impl ConfigOut {
    /// A standing remark about the module — not tied to one row.
    pub fn note(&mut self, text: impl Into<String>) {
        self.notes.push(text.into());
    }

    /// Document the row just drawn.
    ///
    /// A repeated label is DROPPED, not appended: several arms draw a row per
    /// wired channel or per operator, and four copies of the same sentence with
    /// different pin names in the label is noise, not documentation. The first
    /// one wins, so the label the reader meets first is the one explained.
    pub fn field(&mut self, label: impl Into<String>, doc: impl Into<Cow<'static, str>>) {
        let label = label.into();
        if self.fields.iter().any(|f| f.label == label) {
            return;
        }
        self.fields.push(FieldDoc {
            label,
            doc: doc.into(),
        });
    }

    /// A setting this module HAS, but that this grid does not draw.
    ///
    /// Kept apart from [`Self::field`] and NOT gated by
    /// [`Self::all_fields_documented`], because the two answer different
    /// questions. `field` says "here is what the row you are looking at means";
    /// this says "here is a setting you will not find above, and where it lives
    /// instead". The second is true for every kind from the day it is written,
    /// so withholding it until that kind's rows are documented would hide a
    /// finished answer behind an unfinished one.
    pub fn elsewhere(&mut self, label: impl Into<String>, doc: impl Into<Cow<'static, str>>) {
        let label = label.into();
        if self.elsewhere.iter().any(|f| f.label == label) {
            return;
        }
        self.elsewhere.push(FieldDoc {
            label,
            doc: doc.into(),
        });
    }

    pub fn elsewhere_fields(&self) -> &[FieldDoc] {
        &self.elsewhere
    }

    /// A row this chip or runtime does NOT draw, and the reason it does not.
    ///
    /// Only where the reason is already written down in the code. A gate is an
    /// `if` that did not fire, and there are some forty of them in this panel
    /// plus three early returns: inventing a sentence for each would be forty
    /// new claims nobody checked. Where the panel already explains itself - the
    /// F1 serial note, the ESP's missing polarity combo, a one-shot touch scan -
    /// that sentence is lifted here and the pane says it too.
    ///
    /// Everything else absent falls to the pane's flat "not offered on this
    /// chip or runtime", which claims nothing.
    pub fn skip(&mut self, label: impl Into<String>, reason: impl Into<Cow<'static, str>>) {
        let label = label.into();
        if self.skipped.iter().any(|f| f.label == label) {
            return;
        }
        self.skipped.push(FieldDoc {
            label,
            doc: reason.into(),
        });
    }

    pub fn skipped_fields(&self) -> &[FieldDoc] {
        &self.skipped
    }

    /// Say that this arm documents EVERY row it drew.
    ///
    /// Until an arm calls this, [`Self::fields`] hands back `None` and the pane
    /// shows no Fields section at all. That is deliberate: a half-filled roster
    /// reads as "this module has three settings", which is worse than saying
    /// nothing. Rolling the next kind out is one call at the end of its arm.
    pub fn all_fields_documented(&mut self) {
        self.complete = true;
    }

    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The documented rows, or `None` while this kind's arm is not finished —
    /// see [`Self::all_fields_documented`].
    pub fn fields(&self) -> Option<&[FieldDoc]> {
        self.complete.then_some(self.fields.as_slice())
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
            && self.fields.is_empty()
            && self.elsewhere.is_empty()
            && self.skipped.is_empty()
    }
}

/// Every sentence in this module, by name.
///
/// Named consts are what make the checks possible at all: an inline literal
/// at a row site is invisible to a test, which is half the reason the text
/// lives here. This table is what the hygiene tests walk, and what the panel's
/// own tests compare a collected roster against - a doc that is not in here is
/// a second copy of a sentence, which is the one thing this module exists to
/// prevent.
pub const ALL_DOCS: &[(&str, &str)] = &[
    ("INIT_API", INIT_API),
    ("INIT_API_ESP", INIT_API_ESP),
    ("INIT_API_NATIVE", INIT_API_NATIVE),
    ("INIT_API_DMA", INIT_API_DMA),
    ("BLOCKING_TRANSPORT", BLOCKING_TRANSPORT),
    ("DMA_CHANNEL", DMA_CHANNEL),
    ("ASYNC_INIT", ASYNC_INIT),
    ("ASYNC_INIT_LOCKED_RP", ASYNC_INIT_LOCKED_RP),
    ("ESP_TRANSFERS", ESP_TRANSFERS),
    ("ESP_DMA_CHANNEL", ESP_DMA_CHANNEL),
    ("SPI_BIT_ORDER", SPI_BIT_ORDER),
    ("SPI_CLOCK", SPI_CLOCK),
    ("SPI_CLOCK_SLAVE", SPI_CLOCK_SLAVE),
    ("SPI_MODE", SPI_MODE),
    ("SPI_MODE_SLAVE", SPI_MODE_SLAVE),
    ("SPI_MODE_SLAVE_ESP32", SPI_MODE_SLAVE_ESP32),
    ("SPI_ROLE", SPI_ROLE),
    ("SPI_TRANSFERS_SLAVE", SPI_TRANSFERS_SLAVE),
    ("I2C_ADDRESS", I2C_ADDRESS),
    ("I2C_CLOCK", I2C_CLOCK),
    ("I2C_DEVICES", I2C_DEVICES),
    ("I2C_TIMEOUT", I2C_TIMEOUT),
    ("I2S_SAMPLE_RATE", I2S_SAMPLE_RATE),
    ("I2S_DIRECTION", I2S_DIRECTION),
    ("I2S_ROLE", I2S_ROLE),
    ("I2S_ROLE_ESP", I2S_ROLE_ESP),
    ("I2S_STANDARD", I2S_STANDARD),
    ("I2S_STANDARD_ESP", I2S_STANDARD_ESP),
    ("I2S_FORMAT", I2S_FORMAT),
    ("I2S_FORMAT_ESP", I2S_FORMAT_ESP),
    ("I2S_BUFFER", I2S_BUFFER),
    ("I2S_DMA", I2S_DMA),
    ("CAN_BITRATE", CAN_BITRATE),
    ("CAN_BITRATE_ESP", CAN_BITRATE_ESP),
    ("CAN_MODE", CAN_MODE),
    ("CAN_TRANSCEIVER", CAN_TRANSCEIVER),
    ("USB_CONTROLLER", USB_CONTROLLER),
    ("USB_IDENTITY", USB_IDENTITY),
    ("USB_PORT", USB_PORT),
    ("USB_PRODUCT", USB_PRODUCT),
    ("USB_VID", USB_VID),
    ("USB_PID", USB_PID),
    ("USB_STACK", USB_STACK),
    ("USB_STACK_NRF", USB_STACK_NRF),
    ("USB_CLOCK_NRF", USB_CLOCK_NRF),
    ("RMT_DIRECTION_LOCKED", RMT_DIRECTION_LOCKED),
    ("RMT_DIRECTION", RMT_DIRECTION),
    ("RMT_CLK_DIVIDER", RMT_CLK_DIVIDER),
    ("RMT_IDLE_LEVEL", RMT_IDLE_LEVEL),
    ("RMT_IDLE_THRESHOLD", RMT_IDLE_THRESHOLD),
    ("RMT_CARRIER", RMT_CARRIER),
    ("PCNT_COUNTS", PCNT_COUNTS),
    ("PCNT_CTRL", PCNT_CTRL),
    ("PCNT_CTRL_ABSENT", PCNT_CTRL_ABSENT),
    ("PCNT_SECOND_CHANNEL", PCNT_SECOND_CHANNEL),
    ("PCNT_LIMITS", PCNT_LIMITS),
    ("PCNT_FILTER", PCNT_FILTER),
    ("MCPWM_OP_TIMER", MCPWM_OP_TIMER),
    ("MCPWM_FREQUENCY", MCPWM_FREQUENCY),
    ("MCPWM_FREQUENCY_PER_TIMER", MCPWM_FREQUENCY_PER_TIMER),
    ("MCPWM_RESOLUTION", MCPWM_RESOLUTION),
    ("MCPWM_RESOLUTION_PER_TIMER", MCPWM_RESOLUTION_PER_TIMER),
    ("MCPWM_DUTY", MCPWM_DUTY),
    ("HSPI_MODE", HSPI_MODE),
    ("HSPI_MODE_NO_FIT", HSPI_MODE_NO_FIT),
    ("HSPI_DEVICE", HSPI_DEVICE),
    ("XSPI_MODE", XSPI_MODE),
    ("XSPI_MODE_NO_FIT", XSPI_MODE_NO_FIT),
    ("XSPI_DEVICE", XSPI_DEVICE),
    ("XSPI_STROBE_IGNORED", XSPI_STROBE_IGNORED),
    ("XSPI_STROBE_DUAL", XSPI_STROBE_DUAL),
    ("XSPI_STROBE_SECOND_UNUSED", XSPI_STROBE_SECOND_UNUSED),
    ("XSPI_STROBE", XSPI_STROBE),
    ("OSPI_MODE", OSPI_MODE),
    ("OSPI_MODE_NO_FIT", OSPI_MODE_NO_FIT),
    ("OSPI_DEVICE", OSPI_DEVICE),
    ("QSPI_WIRING", QSPI_WIRING),
    ("QSPI_WIRING_INCOMPLETE", QSPI_WIRING_INCOMPLETE),
    ("QSPI_WIRING_NRF", QSPI_WIRING_NRF),
    ("QSPI_WIRING_INCOMPLETE_NRF", QSPI_WIRING_INCOMPLETE_NRF),
    ("QSPI_FLASH_SIZE", QSPI_FLASH_SIZE),
    ("QSPI_ADDRESS", QSPI_ADDRESS),
    ("QSPI_ADDRESS_NRF", QSPI_ADDRESS_NRF),
    ("SDMMC_WIDTH", SDMMC_WIDTH),
    ("SDMMC_WIDTH_UNSUPPORTED", SDMMC_WIDTH_UNSUPPORTED),
    ("SDMMC_DATA_TIMEOUT", SDMMC_DATA_TIMEOUT),
    ("SAI_SUBBLOCKS", SAI_SUBBLOCKS),
    ("SAI_STREAM", SAI_STREAM),
    ("SAI_FRAME", SAI_FRAME),
    ("SAI_DMA", SAI_DMA),
    ("PARLIO_DIRECTION", PARLIO_DIRECTION),
    ("PARLIO_WIDTH", PARLIO_WIDTH),
    ("PARLIO_WIDTH_NO_16", PARLIO_WIDTH_NO_16),
    ("PARLIO_CLOCK", PARLIO_CLOCK),
    ("PARLIO_BIT_ORDER", PARLIO_BIT_ORDER),
    ("PARLIO_DMA_BUFFER", PARLIO_DMA_BUFFER),
    ("LCDCAM_MODE", LCDCAM_MODE),
    ("LCDCAM_WIDTH", LCDCAM_WIDTH),
    ("LCDCAM_WIDTH_CAM", LCDCAM_WIDTH_CAM),
    ("LCDCAM_PIXEL_CLOCK_I8080", LCDCAM_PIXEL_CLOCK_I8080),
    ("LCDCAM_PIXEL_CLOCK_DPI", LCDCAM_PIXEL_CLOCK_DPI),
    ("LCDCAM_PIXEL_CLOCK_CAM", LCDCAM_PIXEL_CLOCK_CAM),
    ("LCDCAM_PIXEL_CLOCK_SLAVE", LCDCAM_PIXEL_CLOCK_SLAVE),
    ("LCDCAM_MASTER_CLOCK", LCDCAM_MASTER_CLOCK),
    ("LCDCAM_ACTIVE_AREA", LCDCAM_ACTIVE_AREA),
    ("NONE", NONE),
    ("LCDCAM_TOTAL", LCDCAM_TOTAL),
    ("LCDCAM_FRONT_PORCH", LCDCAM_FRONT_PORCH),
    ("LCDCAM_SYNC_WIDTH", LCDCAM_SYNC_WIDTH),
    ("LCDCAM_TRANSFERS", LCDCAM_TRANSFERS),
    ("TOUCH_SCAN", TOUCH_SCAN),
    ("TOUCH_THRESHOLD_MODE", TOUCH_THRESHOLD_MODE),
    ("TOUCH_THRESHOLD", TOUCH_THRESHOLD),
    ("TOUCH_MEASUREMENT", TOUCH_MEASUREMENT),
    ("TOUCH_SLEEP_CYCLES", TOUCH_SLEEP_CYCLES),
    ("DAC_CHANNELS", DAC_CHANNELS),
    ("DAC_START", DAC_START),
    ("DAC_START_ESP", DAC_START_ESP),
    ("DAC_NOTE_MORE_CHANNELS", DAC_NOTE_MORE_CHANNELS),
    ("DAC_NOTE_BLOCKING", DAC_NOTE_BLOCKING),
    ("DAC_ESP_BOTH_RUNTIMES", DAC_ESP_BOTH_RUNTIMES),
    ("CUSTOM_STRUCT", CUSTOM_STRUCT),
    ("CUSTOM_PINS", CUSTOM_PINS),
    ("CUSTOM_PIN_FUNCTION", CUSTOM_PIN_FUNCTION),
    ("CUSTOM_PIN_UNSET", CUSTOM_PIN_UNSET),
    ("CUSTOM_PIN_REMOVE", CUSTOM_PIN_REMOVE),
    ("CUSTOM_PIN_NAME", CUSTOM_PIN_NAME),
    ("CUSTOM_NOTE_NO_PINS", CUSTOM_NOTE_NO_PINS),
    ("CUSTOM_ADD_PIN", CUSTOM_ADD_PIN),
    ("CUSTOM_UPDATE", CUSTOM_UPDATE),
    ("CUSTOM_UPDATE_INCOMPLETE", CUSTOM_UPDATE_INCOMPLETE),
    ("CUSTOM_UPDATE_DISABLED", CUSTOM_UPDATE_DISABLED),
    ("CUSTOM_UPDATE_PENDING", CUSTOM_UPDATE_PENDING),
    ("CUSTOM_UNCONFIGURED_DIALOG", CUSTOM_UNCONFIGURED_DIALOG),
    ("SHARED_NAME_CUSTOM", SHARED_NAME_CUSTOM),
    ("TIMER_FREQUENCY", TIMER_FREQUENCY),
    ("TIMER_FREQUENCY_ESP", TIMER_FREQUENCY_ESP),
    ("TIMER_FREQUENCY_RP", TIMER_FREQUENCY_RP),
    ("TIMER_DUTY_RESOLUTION_ESP", TIMER_DUTY_RESOLUTION_ESP),
    ("TIMER_COUNTING", TIMER_COUNTING),
    ("TIMER_COUNTING_INERT_RP", TIMER_COUNTING_INERT_RP),
    ("TIMER_CHANNELS_EMPTY", TIMER_CHANNELS_EMPTY),
    ("TIMER_CHANNELS_EMPTY_ESP", TIMER_CHANNELS_EMPTY_ESP),
    ("TIMER_CHANNELS_EMPTY_RP", TIMER_CHANNELS_EMPTY_RP),
    ("TIMER_DUTY", TIMER_DUTY),
    ("TIMER_DUTY_ESP", TIMER_DUTY_ESP),
    ("TIMER_DUTY_RP", TIMER_DUTY_RP),
    ("TIMER_CHANNEL_OUTPUT", TIMER_CHANNEL_OUTPUT),
    (
        "TIMER_CHANNEL_OUTPUT_COMPLEMENTARY",
        TIMER_CHANNEL_OUTPUT_COMPLEMENTARY,
    ),
    (
        "TIMER_CHANNEL_OUTPUT_INERT_RP",
        TIMER_CHANNEL_OUTPUT_INERT_RP,
    ),
    ("TIMER_DEAD_TIME", TIMER_DEAD_TIME),
    ("TIMER_BREAK_INPUT", TIMER_BREAK_INPUT),
    ("TIMER_AUTO_OUTPUT_ENABLE", TIMER_AUTO_OUTPUT_ENABLE),
    ("SKIP_F1_SERIAL", SKIP_F1_SERIAL),
    ("SKIP_USART_BUF_TX_ONLY", SKIP_USART_BUF_TX_ONLY),
    ("SKIP_USART_BUF_RP_DMA", SKIP_USART_BUF_RP_DMA),
    ("SKIP_I2C_TIMEOUT_RP", SKIP_I2C_TIMEOUT_RP),
    ("SKIP_USART_BUF_NRF", SKIP_USART_BUF_NRF),
    ("SKIP_USART_TRANSPORT_NRF", SKIP_USART_TRANSPORT_NRF),
    ("SKIP_ASYNC_INIT_NRF", SKIP_ASYNC_INIT_NRF),
    ("SKIP_I2C_TIMEOUT_NRF", SKIP_I2C_TIMEOUT_NRF),
    ("SKIP_SPI_BIT_ORDER", SKIP_SPI_BIT_ORDER),
    ("SKIP_SPI_ROLE", SKIP_SPI_ROLE),
    ("SKIP_TOUCH_SLEEP", SKIP_TOUCH_SLEEP),
    ("SKIP_CAN_MODE", SKIP_CAN_MODE),
    ("SHARED_INSTANCE", SHARED_INSTANCE),
    ("SHARED_INSTANCE_TIMER_STM32", SHARED_INSTANCE_TIMER_STM32),
    ("SHARED_INSTANCE_TIMER_ESP", SHARED_INSTANCE_TIMER_ESP),
    ("SHARED_INSTANCE_TIMER_RP", SHARED_INSTANCE_TIMER_RP),
    ("SHARED_INSTANCE_CUSTOM", SHARED_INSTANCE_CUSTOM),
    ("SHARED_NAME", SHARED_NAME),
    ("SHARED_NAME_RP", SHARED_NAME_RP),
    ("SHARED_DATA_MODELS", SHARED_DATA_MODELS),
    ("USART_BAUD", USART_BAUD),
    ("USART_DATA_BITS", USART_DATA_BITS),
    ("USART_PARITY", USART_PARITY),
    ("USART_STOP_BITS", USART_STOP_BITS),
    ("USART_BUF_BUFFERED", USART_BUF_BUFFERED),
    ("USART_BUF_DMA", USART_BUF_DMA),
    ("USART_ASYNC_TRANSPORT", USART_ASYNC_TRANSPORT),
    ("USART_TRANSFERS_ESP", USART_TRANSFERS_ESP),
    ("USART_DIRECTION", USART_DIRECTION),
    ("USART_DIRECTION_LOCKED", USART_DIRECTION_LOCKED),
    ("USART_DIRECTION_LOCKED_RP", USART_DIRECTION_LOCKED_RP),
    ("USART_LINE", USART_LINE),
    ("USART_LINE_ABSENT", USART_LINE_ABSENT),
    ("USART_FLOW", USART_FLOW),
    ("USART_HALF_DUPLEX_READBACK", USART_HALF_DUPLEX_READBACK),
    ("LEGEND_PWM", LEGEND_PWM),
    ("LEGEND_USB", LEGEND_USB),
    ("LEGEND_TOUCH", LEGEND_TOUCH),
    ("LEGEND_DAC", LEGEND_DAC),
    ("LEGEND_PCNT", LEGEND_PCNT),
    ("LEGEND_SPI", LEGEND_SPI),
    ("LEGEND_I2C", LEGEND_I2C),
    ("LEGEND_I2S", LEGEND_I2S),
    ("LEGEND_MCPWM", LEGEND_MCPWM),
    ("LEGEND_UART", LEGEND_UART),
    ("LEGEND_CAN", LEGEND_CAN),
    ("LEGEND_RMT", LEGEND_RMT),
    ("LEGEND_MEMORY", LEGEND_MEMORY),
    ("LEGEND_PARALLEL", LEGEND_PARALLEL),
    ("LEGEND_LCD_CAM", LEGEND_LCD_CAM),
];

/// Every row the panel can draw, per module kind.
///
/// # Generated, never written
///
/// This is the union of every label `module_config_ui` draws across every chip,
/// runtime and config state the test matrix reaches. Regenerate it with
///
/// ```text
/// cargo test regenerate_the_roster -- --ignored --nocapture
/// cargo fmt
/// ```
///
/// and `the_roster_matches_the_matrix` pins it from BOTH sides: a row added
/// without regenerating fails, and so does an entry no cell can reach any more.
/// A hand-kept list of a hundred labels would be wrong within a month; this one
/// cannot be wrong without a test going red.
///
/// # What the second field is
///
/// The const NAME of the sentence that row carries - but only where it is the
/// SAME in every cell the matrix reached. A row whose meaning changes with the
/// chip carries `None`, because there is no one sentence to show for a row this
/// chip did not draw, and picking one of several would be inventing an answer.
///
/// Keyed by the kind's short name, which is unique across the kinds - see
/// `every_kind_has_its_own_short_name`.
/// One row of a kind's roster: the label the panel draws, and the const NAME
/// of its sentence when that sentence is the same on every chip.
pub type RosterRow = (&'static str, Option<&'static str>);

/// A kind's short name, and every row it can draw.
pub type KindRoster = (&'static str, &'static [RosterRow]);

pub const ROSTER: &[KindRoster] = &[
    (
        "CAM",
        &[
            ("Bus width", Some(LCDCAM_WIDTH_CAM)),
            ("Master clock", Some(LCDCAM_MASTER_CLOCK)),
            ("Pixel clock", Some(LCDCAM_PIXEL_CLOCK_CAM)),
            ("Transfers", Some(LCDCAM_TRANSFERS)),
        ],
    ),
    (
        "CAN",
        &[
            ("Bit rate", None),
            ("Mode", Some(CAN_MODE)),
            ("Transceiver", Some(CAN_TRANSCEIVER)),
        ],
    ),
    (
        "Custom",
        &[
            ("Add pin", Some(CUSTOM_ADD_PIN)),
            ("Pin function", Some(CUSTOM_PIN_FUNCTION)),
            ("Pins", Some(CUSTOM_PINS)),
            ("Struct", Some(CUSTOM_STRUCT)),
            ("Update", Some(CUSTOM_UPDATE_DISABLED)),
        ],
    ),
    ("DAC", &[("Channels", Some(DAC_CHANNELS))]),
    (
        "HSPI",
        &[
            ("Device", Some(HSPI_DEVICE)),
            ("Mode", Some(HSPI_MODE_NO_FIT)),
        ],
    ),
    (
        "I2C",
        &[
            ("Async init", Some(ASYNC_INIT)),
            ("Clock", Some(I2C_CLOCK)),
            ("Devices", Some(I2C_DEVICES)),
            ("Init API", None),
            ("Timeout", Some(I2C_TIMEOUT)),
        ],
    ),
    (
        "I2S",
        &[
            ("DMA", Some(I2S_DMA)),
            ("Direction", Some(I2S_DIRECTION)),
            ("Format", None),
            ("Ring buffer", Some(I2S_BUFFER)),
            ("Role", None),
            ("Sample rate", Some(I2S_SAMPLE_RATE)),
            ("Standard", None),
        ],
    ),
    (
        "LCD",
        &[
            ("Bus width", Some(LCDCAM_WIDTH)),
            ("Mode", Some(LCDCAM_MODE)),
            ("Pixel clock", Some(LCDCAM_PIXEL_CLOCK_I8080)),
            ("Transfers", Some(LCDCAM_TRANSFERS)),
        ],
    ),
    (
        "LPUART",
        &[
            ("Async transport", Some(USART_ASYNC_TRANSPORT)),
            ("Baud rate", Some(USART_BAUD)),
            ("Data bits", Some(USART_DATA_BITS)),
            ("Data direction", None),
            ("Hardware flow control", Some(USART_FLOW)),
            ("Init API", None),
            ("Line", None),
            ("Parity", Some(USART_PARITY)),
            ("RX/TX buffer", Some(USART_BUF_BUFFERED)),
            ("Stop bits", Some(USART_STOP_BITS)),
            ("Transfers", Some(USART_TRANSFERS_ESP)),
        ],
    ),
    (
        "OSPI",
        &[
            ("Device", Some(OSPI_DEVICE)),
            ("Mode", Some(OSPI_MODE_NO_FIT)),
        ],
    ),
    (
        "PARL",
        &[
            ("Bit order", Some(PARLIO_BIT_ORDER)),
            ("Bus width", Some(PARLIO_WIDTH_NO_16)),
            ("Clock", Some(PARLIO_CLOCK)),
            ("DMA buffer", Some(PARLIO_DMA_BUFFER)),
            ("Direction", Some(PARLIO_DIRECTION)),
        ],
    ),
    (
        "PARL RX",
        &[
            ("Bit order", Some(PARLIO_BIT_ORDER)),
            ("Bus width", Some(PARLIO_WIDTH_NO_16)),
            ("Clock", Some(PARLIO_CLOCK)),
            ("DMA buffer", Some(PARLIO_DMA_BUFFER)),
            ("Direction", Some(PARLIO_DIRECTION)),
        ],
    ),
    (
        "PCNT",
        &[
            ("Glitch filter", Some(PCNT_FILTER)),
            ("Limits", Some(PCNT_LIMITS)),
            ("Second channel", Some(PCNT_SECOND_CHANNEL)),
        ],
    ),
    (
        "PWM",
        &[
            ("Channels", None),
            ("Counter", None),
            ("Duty resolution", Some(TIMER_DUTY_RESOLUTION_ESP)),
            ("Frequency", None),
        ],
    ),
    (
        "QSPI",
        &[
            ("Address", Some(QSPI_ADDRESS)),
            ("Flash size", Some(QSPI_FLASH_SIZE)),
            ("Wiring", Some(QSPI_WIRING_INCOMPLETE)),
        ],
    ),
    (
        "RMT",
        &[
            ("Carrier", Some(RMT_CARRIER)),
            ("Clock divider", Some(RMT_CLK_DIVIDER)),
            ("Direction", None),
            ("Idle level", Some(RMT_IDLE_LEVEL)),
        ],
    ),
    ("SAI", &[("Sub-blocks", Some(SAI_SUBBLOCKS))]),
    (
        "SDMMC",
        &[
            ("Bus width", Some(SDMMC_WIDTH_UNSUPPORTED)),
            ("Data timeout", Some(SDMMC_DATA_TIMEOUT)),
        ],
    ),
    (
        "SPI",
        &[
            ("Async init", None),
            ("Bit order", Some(SPI_BIT_ORDER)),
            ("Clock", Some(SPI_CLOCK)),
            ("Init API", None),
            ("Role", Some(SPI_ROLE)),
            ("SPI mode", Some(SPI_MODE)),
            ("Transfers", Some(ESP_TRANSFERS)),
            ("Transport", Some(BLOCKING_TRANSPORT)),
        ],
    ),
    (
        "TOUCH",
        &[
            ("Measurement", Some(TOUCH_MEASUREMENT)),
            ("Scan", Some(TOUCH_SCAN)),
            ("Sleep cycles", Some(TOUCH_SLEEP_CYCLES)),
            ("Threshold", Some(TOUCH_THRESHOLD)),
            ("Touched when", Some(TOUCH_THRESHOLD_MODE)),
        ],
    ),
    (
        "USART",
        &[
            ("Async transport", Some(USART_ASYNC_TRANSPORT)),
            ("Baud rate", Some(USART_BAUD)),
            ("Data bits", Some(USART_DATA_BITS)),
            ("Data direction", None),
            ("Hardware flow control", Some(USART_FLOW)),
            ("Init API", None),
            ("Line", None),
            ("Parity", Some(USART_PARITY)),
            ("RX/TX buffer", Some(USART_BUF_BUFFERED)),
            ("Stop bits", Some(USART_STOP_BITS)),
            ("Transfers", Some(USART_TRANSFERS_ESP)),
            ("Transport", Some(BLOCKING_TRANSPORT)),
        ],
    ),
    (
        "USB",
        &[
            ("Clock", Some(USB_CLOCK_NRF)),
            ("Identity", Some(USB_IDENTITY)),
            ("Port", Some(USB_PORT)),
            ("Product", Some(USB_PRODUCT)),
            ("Product ID", Some(USB_PID)),
            ("Stack", Some(USB_STACK_NRF)),
            ("Vendor ID", Some(USB_VID)),
        ],
    ),
    (
        "XSPI",
        &[
            ("Device", Some(XSPI_DEVICE)),
            ("Mode", Some(XSPI_MODE_NO_FIT)),
        ],
    ),
];

// ── Rows shared by USART / SPI / I2C ─────────────────────────────────────────

pub const INIT_API: &str = "Which TYPE `init` hands back. Portable gives an embedded-io / \
                            embedded-hal 1.0 value, so driver code moves between HALs; Native \
                            gives the HAL's own type, with everything it can do.";

pub const INIT_API_ESP: &str = "Fixed on an ESP: every esp-hal driver is the concrete type, \
                                because that is what esp-hal implements its traits on. The \
                                Portable bridge is an STM32F1 choice.";

pub const INIT_API_NATIVE: &str = "Fixed by the Native runtime, which uses the concrete HAL type \
                                   for every peripheral. Change it on the System tab to choose \
                                   per module again.";

pub const INIT_API_DMA: &str = "Fixed by the DMA transport: stm32f1xx-hal's DMA handles are its \
                                own types, and there is no portable bus to bridge them to. Turn \
                                DMA off and the choice comes back.";

pub const BLOCKING_TRANSPORT: &str = "Which HALF of the bus the DMA moves, on the blocking F1 \
                                      HAL. Directions and not channels, because stm32f1xx-hal \
                                      fixes the channel per peripheral in its types. The half you \
                                      leave off keeps the ordinary polled handle and frees its \
                                      channel.";

pub const DMA_CHANNEL: &str = "The channel this direction takes. Automatic gives it the first one \
                               the peripheral can use that nothing else has claimed - pin it by \
                               hand to leave a particular channel free, or to match a driver you \
                               already have.";

// ── More rows shared by USART / SPI / I2C ─────────────────────────────────────

pub const ASYNC_INIT: &str = "Whether this bus is an ordinary blocking one or one you can await. \
                              Blocking takes no DMA channel and always builds - a blocking \
                              driver inside an async project is normal. Async-DMA leaves the CPU \
                              free while a transfer runs, at the price of a channel each way.";

pub const ASYNC_INIT_LOCKED_RP: &str = "On a Pico there is nothing to choose: this backend emits \
                                        the DMA form only. The chip is not the limit - \
                                        embassy-rp has a blocking SPI constructor too - so \
                                        picking Blocking here would change nothing. Channels are \
                                        allocated for you either way.";

pub const ESP_TRANSFERS: &str = "Who carries the bytes: the CPU, one at a time, or the DMA \
                                 controller. CPU is the simplest and fine for short transfers; \
                                 DMA takes one of the chip's channels and moves whole buffers on \
                                 its own, which is what long or continuous traffic needs.";

pub const ESP_DMA_CHANNEL: &str = "Which channel this bus takes. Automatic gives it the first \
                                   one the peripheral can use that nothing else has claimed; \
                                   naming one pins it - a pinned channel is reserved before \
                                   anything is handed out, which is how two buses are kept off \
                                   the same one.";
// ── SPI ───────────────────────────────────────────────────────────────────────

pub const SPI_BIT_ORDER: &str = "Which end of each byte goes on the wire first. MSB first is \
                                 what nearly every device expects; some sensors and shift \
                                 registers are LSB first, and getting it wrong gives \
                                 bit-reversed data rather than silence, so check the datasheet.";

pub const SPI_CLOCK: &str = "How fast the master clocks SCK. Higher is only useful up to what \
                             the slowest device on the bus and the wiring can follow - long \
                             leads or a breadboard start corrupting bytes well below the \
                             peripheral's own limit.";

pub const SPI_CLOCK_SLAVE: &str = "A slave has no clock of its own: the master drives SCK and \
                                   this end simply follows it, so there is nothing to set. If \
                                   the link is too fast for this chip, it is the master that has \
                                   to slow down.";

pub const SPI_MODE: &str = "CPOL and CPHA together: the level the clock idles at, and whether a \
                            bit is sampled on the first or the second edge. The device on the \
                            other end names the one it wants, and the wrong mode gives garbage \
                            or silence rather than an error you can read.";

pub const SPI_MODE_SLAVE: &str = "CPOL and CPHA - the clock's idle level and the sampling edge. \
                                  As a slave this chip chooses nothing: it has to be set to \
                                  whatever the master already drives, or every byte it clocks in \
                                  is shifted by half a clock.";

pub const SPI_MODE_SLAVE_ESP32: &str = "This chip's SPI slave can only be set to modes 1 and 3, \
                                        the two that sample on the second clock edge. A master \
                                        running mode 0 or 2 cannot be answered here, so it is \
                                        that end that has to move.";

pub const SPI_ROLE: &str = "Which end of the bus this chip is. A master drives the clock and the \
                            chip select and starts every transfer; a slave only answers, and \
                            nothing moves until the other side asserts CS - so a slave nobody \
                            addresses is indistinguishable from a dead one.";

pub const SPI_TRANSFERS_SLAVE: &str = "Not a choice for a slave: the master decides when bytes \
                                       move, so this end has to be armed and waiting before they \
                                       do, and only DMA can do that. A channel is taken whatever \
                                       runtime the project uses.";
// ── I2C ───────────────────────────────────────────────────────────────────────

pub const I2C_ADDRESS: &str = "The 7-bit address of the device this bus talks to, without the \
                               read/write bit - a datasheet's 0xD0 is 0x68 here. A master sends \
                               it at the start of every transaction rather than once at setup, \
                               so it is kept for your code to use; wrong, and nothing answers.";

pub const I2C_DEVICES: &str = "Every device sharing these two pads, each with a name and its 7-bit address (the ID on its box under the bus, on the canvas). An I2C bus is meant to be shared, and the address is the ONLY thing that tells its devices apart - the bus driver itself is built once, whatever is on it. The bus is a folder under `pins/configs/`: its `mod.rs` builds the driver, and every device - a bus's only one included - has a file beside it, `device<n>_<name>.rs`, carrying its address for your code to use.";

pub const I2C_CLOCK: &str = "How fast SCL is driven. 100 kHz is what every I2C device supports; \
                             400 kHz needs the whole bus to agree - every device on it, and \
                             pull-ups strong enough to pull the line high again in time, or the \
                             edges round off and bytes are misread.";

pub const I2C_TIMEOUT: &str = "How long one transfer may take before it gives up. A stuck I2C \
                               bus is a real failure mode - a device stretching the clock \
                               forever, or a bus with no pull-ups - and without this your code \
                               waits for it for good. 0 leaves the default of 1000 ms.";
// ── I2S ───────────────────────────────────────────────────────────────────────

pub const I2S_SAMPLE_RATE: &str = "Audio frames per second - 44.1k for material that came off a \
                                   CD, 48k for almost everything else. The bit clock is derived \
                                   from it, so both ends have to agree; as Slave the other \
                                   device drives the clocks and this only says what to expect.";

pub const I2S_DIRECTION: &str = "Which way the audio moves, and what the SD pad becomes: an \
                                 output when transmitting, an input when receiving. One \
                                 direction per module - full duplex would need a second data pad \
                                 and a newer SPI block, so it is not offered here.";

pub const I2S_ROLE: &str = "Who generates the bit clock and the word select. Master drives both \
                            pads and sets the rate; Slave takes them from the other device, \
                            which then owns the timing. Two masters on one bus, or two slaves, \
                            and no sample is ever clocked.";

pub const I2S_ROLE_ESP: &str = "Locked to Master here: esp-hal puts the I2S block into master \
                                mode as it is built and exposes no follower driver, so this chip \
                                always generates the bit clock and the word select for whatever \
                                is on the other end.";

pub const I2S_STANDARD: &str = "Where the data sits relative to the word select. Philips is \
                                plain I2S and what most codecs expect; the justified forms shift \
                                the first bit by one clock and the PCM ones shrink WS to a \
                                pulse. Disagree with the codec and the channels swap.";

pub const I2S_STANDARD_ESP: &str = "Where the data sits relative to the word select. Philips is \
                                    plain I2S and what most codecs expect; the PCM forms shrink \
                                    WS to a pulse. Four here and not five: esp-hal has no \
                                    LSB-first form, so a right-justified codec has no entry.";

pub const I2S_FORMAT: &str = "How many data bits ride in how wide a channel slot - 24-in-32 pads \
                              a 24-bit codec out to a full slot. The second box is which level \
                              the bit clock idles at, and it decides which edge the other end \
                              latches on: wrong, and every sample is garbage.";

pub const I2S_FORMAT_ESP: &str = "How many data bits ride in how wide a channel slot. Two widths \
                                  and no polarity box beside them: esp-hal builds 16-in-16 and \
                                  32-in-32, and its only polarity knob is on the word select, \
                                  which is a different signal.";

pub const I2S_BUFFER: &str = "Length of the ring the DMA cycles through, counted in samples. The \
                              controller owns it for the whole program and never pauses: too \
                              short and any scheduling hiccup is heard as a click or a dropout, \
                              too long and it is RAM standing idle.";

pub const I2S_DMA: &str = "The one channel this I2S takes - the direction decides whether it is \
                           the transmit or the receive request, so there is never a second. \
                           Automatic takes the first free channel the block can use; pin it by \
                           hand to keep one for something else.";
// ── CAN ───────────────────────────────────────────────────────────────────────

pub const CAN_BITRATE: &str = "Bits per second on the bus. Every node must be set to the same \
                               one, and a node at the wrong rate does not merely miss traffic - \
                               it error-frames what the others send, which can take the whole \
                               bus down.";

pub const CAN_BITRATE_ESP: &str = "Bits per second on the bus, and every node must be set to the \
                                   same one - a node at the wrong rate error-frames what the \
                                   others send. These four are the timings esp-hal ships \
                                   ready-made; any other rate has to be computed by hand.";

pub const CAN_MODE: &str = "How this node sits on the bus. Normal sends, receives and \
                            acknowledges; Self-test sends without waiting for anyone to answer, \
                            so one board alone on a bench does not fault; Listen only never \
                            drives the wire, not even an ack.";

pub const CAN_TRANSCEIVER: &str = "On means a real CAN transceiver sits between these pads and \
                                   the bus, which is what any actual bus needs. Clear it only \
                                   for two boards wired TX-to-RX with nothing in between, the \
                                   bench case esp-hal builds a different way.";
// ── USB ───────────────────────────────────────────────────────────────────────

pub const USB_CONTROLLER: &str = "Which of the two USB blocks gets the D-/D+ pads - they share \
                                  one pair, so only one can have them. Serial/JTAG is the \
                                  built-in console: no code, no crates, fixed identity. OTG is a \
                                  device of your own design, and needs a USB stack.";

pub const USB_IDENTITY: &str = "How this chip introduces itself to the host: Espressif's own \
                                vendor and product numbers, fixed in silicon, with a descriptor \
                                set nothing here can change. A board that must appear as its own \
                                product needs the OTG controller instead.";

pub const USB_PORT: &str = "What the host gets is one CDC serial port straight off the chip's \
                            pads, with no bridge chip in between. A board that also carries a \
                            USB-UART bridge shows a second port - two devices to the host, and \
                            picking the wrong one looks like a dead console.";

pub const USB_PRODUCT: &str = "The name the host shows for this device - in Device Manager, in \
                               lsusb, in the notification when it is plugged in. Cosmetic: \
                               drivers bind to the vendor and product numbers, not to this, but \
                               it is the part of the identity a person reads.";

pub const USB_VID: &str = "The 16-bit vendor number the host reads first; with the product ID it \
                           is what picks a driver. The default is the pid.codes test pair, which \
                           is fine on a bench and not for anything shipped - that needs a vendor \
                           number of your own.";

pub const USB_PID: &str = "The 16-bit product number, and it only means anything paired with the \
                           vendor ID. Windows remembers which driver it bound to a pair, so \
                           reusing a pair some other device already used on that machine can \
                           hand your board the wrong driver.";

pub const USB_STACK: &str = "The usb-device and usbd-serial crates come with this controller and \
                             are added for you, wired up as a CDC serial port. Not a choice, and \
                             not a limit either - the class is swappable in your own code for \
                             anything the crate offers.";
pub const USB_STACK_NRF: &str = "Which USB stack the generated code builds on, and it follows the \
                                 Runtime: on Blocking nrf-hal's Usbd under usb-device and \
                                 usbd-serial, on Async embassy-nrf's driver under embassy-usb, \
                                 with the device running in its own task. Either crate set is \
                                 added to Cargo.toml for you, as a CDC serial port.";
pub const USB_CLOCK_NRF: &str = "USB runs only from the 32 MHz crystal. With the pads wired, the \
                                 generated code starts it whatever the Clock tab chose, and says \
                                 so in a comment where it does.";
// ── RMT ───────────────────────────────────────────────────────────────────────

pub const RMT_DIRECTION_LOCKED: &str = "Fixed in silicon on this chip: the low RMT channels only \
                                        transmit and the high ones only receive, so this channel \
                                        has no say. To go the other way, wire the module to a \
                                        pad belonging to a channel from the other half.";

pub const RMT_DIRECTION: &str = "Whether this channel drives the line or listens to it. Every \
                                 RMT channel on this chip can do either, and the choice decides \
                                 what the module needs on the canvas - an output pad for a LED \
                                 strip, an input for an IR receiver.";

pub const RMT_CLK_DIVIDER: &str = "Divides the RMT source clock into the tick that every pulse \
                                   length is counted in - the ns figure beside it. A small \
                                   divider resolves finer edges, a large one buys a longer \
                                   maximum pulse, and one channel cannot have both.";

pub const RMT_IDLE_LEVEL: &str = "Where the pad rests between pulse trains. It has to be what \
                                  the far end reads as quiet - Low for a WS2812 strip, and High \
                                  for the usual IR driver whose transistor pulls the LED down.";

pub const RMT_IDLE_THRESHOLD: &str = "How many ticks of silence end a received frame. Too short \
                                      and one remote-control burst arrives as several fragments; \
                                      too long and two separate presses are read as one.";

pub const RMT_CARRIER: &str = "Modulates the pulses onto a carrier, which is what an IR receiver \
                               demodulates - 38 kHz for nearly every remote. Leave it off for \
                               WS2812 and 1-Wire, which want the edges raw.";
// ── PCNT ──────────────────────────────────────────────────────────────────────

pub const PCNT_COUNTS: &str = "What each edge on this channel's pulse input does to the counter: \
                               count up, count down, or nothing. Counting both edges doubles the \
                               resolution and halves how far the count gets before it hits a \
                               limit.";

pub const PCNT_CTRL: &str = "What the control pad's level does to this channel. Keep counts as \
                             the edges say, Reverse swaps up for down, and Ignore edges pauses \
                             counting - Reverse on one level is what makes the unit follow an \
                             encoder's direction on its own.";

pub const PCNT_CTRL_ABSENT: &str = "Greyed out because this channel has no control pad. Assign a \
                                    PCNT CTRL pin on the canvas to use it - the control level is \
                                    what turns a plain pulse counter into a direction-aware \
                                    encoder input.";

pub const PCNT_SECOND_CHANNEL: &str = "A PCNT unit has a second channel with its own edge and \
                                       control pads and its own rules, counting into the same \
                                       counter. Wire PCNT EDGE1 on the canvas to get it - two \
                                       channels with opposite rules is a quadrature encoder.";

pub const PCNT_LIMITS: &str = "The counter is signed 16-bit, and reaching either limit clears it \
                               and raises an event. Pick the span you want to see between \
                               events; a total wider than 16 bits is built by counting them.";

pub const PCNT_FILTER: &str = "Pulses shorter than this many APB clocks are ignored - the \
                               difference between counting a contact bounce once and counting it \
                               eight times. 0 turns the filter off, and the hardware takes no \
                               more than 1023.";
// ── MCPWM ─────────────────────────────────────────────────────────────────────

pub const MCPWM_OP_TIMER: &str = "Which of the unit's three timers this operator runs on. \
                                  Operators on one timer share its frequency and period; on \
                                  separate timers they are independent, which is how a single \
                                  unit drives two motors at two speeds.";

pub const MCPWM_FREQUENCY: &str = "How many PWM periods a second every wired output gets. 20 kHz \
                                   and up is above hearing, which is where a motor drive wants \
                                   to be - lower and the load sings, much higher and switching \
                                   losses grow.";

pub const MCPWM_FREQUENCY_PER_TIMER: &str = "The rate of THIS timer, shared by every operator \
                                             pointed at it and by nothing else. Change the wrong \
                                             row and that motor stays at its old speed. 20 kHz \
                                             and up is above hearing, where a motor drive wants \
                                             to be.";

pub const MCPWM_RESOLUTION: &str = "The timer's counter top: a duty can only land on one of \
                                    period + 1 steps, so 99 gives whole percents. Period times \
                                    frequency is capped by the MCPWM clock - 40 MHz, 32 on the \
                                    H2 - so finer steps cost top speed.";

pub const MCPWM_RESOLUTION_PER_TIMER: &str = "This timer's counter top, and the step size for \
                                              duties on its operators only. Each timer has its \
                                              own, so a percentage worked out against another \
                                              timer's period is a different pulse width here.";

pub const MCPWM_DUTY: &str = "How much of the period this output stays high, measured against \
                              ITS operator's timer. It can only land on one of that timer's \
                              steps, and A and B are set apart - a half-bridge wants them \
                              complementary, two separate loads do not.";
// ── HSPI ──────────────────────────────────────────────────────────────────────

pub const HSPI_MODE: &str = "How many data lines carry the payload. Only two shapes exist here - \
                             plain one-line SPI, or eight lines plus the DQS0 strobe - and the \
                             list offers only the one your wiring can feed, so the sixteen pads \
                             the silicon has are not on the menu.";

pub const HSPI_MODE_NO_FIT: &str = "Nothing to choose: the data pads you wired add up to a width \
                                    this controller has no call for. Wire two IO lines for \
                                    single, or eight plus DQS0 for octal; anything in between \
                                    generates no HSPI at all.";

pub const HSPI_DEVICE: &str = "What sits on the other end: the chip's whole capacity, the device \
                               family whose command framing the controller follows, and a \
                               divider that puts the bus at kernel clock / (value + 1). 0 is the \
                               fastest it can go, and often faster than the flash can answer.";
// ── XSPI ──────────────────────────────────────────────────────────────────────

pub const XSPI_MODE: &str = "How many data lines the controller drives, and how. Only the modes \
                             your wiring can carry are offered: single and dual share the same \
                             two pads, octal and dual-quad the same eight, so the pins cannot \
                             tell them apart and this has to ask.";

pub const XSPI_MODE_NO_FIT: &str = "Nothing to choose: the IO pads you wired add up to a width \
                                    the controller has no mode for. It takes 2, 4, 8 or 16 data \
                                    lines - wire up to one of those and the list comes back.";

pub const XSPI_DEVICE: &str = "What sits on the other end: the chip's whole capacity, the device \
                               family the controller frames its commands for - two more here \
                               than the OCTOSPI offers - and a divider that puts the bus at \
                               kernel clock / (value + 1), where 0 is the fastest and often too \
                               fast.";

pub const XSPI_STROBE_IGNORED: &str = "Read-only, and a warning: the strobe is wired but only \
                                       the octal and hexadeca modes read it, so at this width \
                                       the pad does nothing. Widen the mode, or free the pin for \
                                       something else.";

pub const XSPI_STROBE_DUAL: &str = "Read-only. Both strobes are wired and hexadeca is the only \
                                    mode that reads both - sixteen data lines split into two \
                                    halves, each sampled by its own strobe. This is the widest \
                                    form the controller has.";

pub const XSPI_STROBE_SECOND_UNUSED: &str = "Read-only, and a warning: DQS1 belongs to the \
                                             hexadeca mode, where the second half of a \
                                             sixteen-line bus has its own strobe. At this width \
                                             only DQS0 is used and the second pad is spent for \
                                             nothing.";

pub const XSPI_STROBE: &str = "Read-only. DQS0 is wired and this mode uses it: the memory sends \
                               a strobe back alongside the data and the controller samples on \
                               that edge instead of on its own clock, which is what makes the \
                               fast read timings safe.";
// ── OSPI ──────────────────────────────────────────────────────────────────────

pub const OSPI_MODE: &str = "How many data lines the controller drives, and how. Only the modes \
                             your wiring can carry are offered: single and dual use the same two \
                             pads, octal and dual-quad the same eight, so the pads cannot tell \
                             them apart and this has to ask.";

pub const OSPI_MODE_NO_FIT: &str = "Nothing to choose: the IO pads you wired add up to a width \
                                    the OCTOSPI has no mode for. It takes 2, 4 or 8 data lines - \
                                    wire up to one of those and the list comes back.";

pub const OSPI_DEVICE: &str = "What sits on the other end: the chip's whole capacity, the device \
                               family - Standard covers ordinary NOR flash, HyperBus is a \
                               different protocol altogether - and a divider that puts the bus \
                               at kernel clock / (value + 1).";
// ── QSPI ──────────────────────────────────────────────────────────────────────

pub const QSPI_WIRING: &str = "Read-only: which flash banks your wiring completed. A bank counts \
                               only with its own chip select and all four data lines; wire both \
                               and the two chips are driven side by side as one eight-line dual \
                               flash.";

pub const QSPI_WIRING_INCOMPLETE: &str = "Read-only, and nothing will be generated as it stands. \
                                          The controller needs CLK plus at least one whole bank \
                                          - that bank's NCS and all four of its IO lines. Three \
                                          lines wired is not a narrower bus, it is no bus.";

pub const QSPI_WIRING_NRF: &str = "Read-only: the bus is complete. The nRF52840's QSPI drives one \
                                   flash chip, four data lines wide, and its driver takes SCK, CSN \
                                   and IO0 to IO3 together.";

pub const QSPI_WIRING_INCOMPLETE_NRF: &str = "Read-only, and nothing will be generated as it \
                                              stands. The nRF52840's QSPI driver is quad only: it \
                                              takes SCK, CSN and all four IO lines in one call, so \
                                              a missing lane is not a narrower bus, it is no bus.";

pub const QSPI_FLASH_SIZE: &str = "The capacity of the flash chip on the board. The controller \
                                   needs it to know where its memory-mapped window ends: set it \
                                   larger than the part and addresses past the end read back \
                                   nothing real, set it smaller and the top of the chip is \
                                   unreachable.";

pub const QSPI_ADDRESS: &str = "How many address bytes the chip expects, and how fast to clock \
                                it. 24 bit reaches 16 MiB and bigger flash needs 32; the bus \
                                runs at kernel clock / (value + 1), so 0 is the fastest the \
                                controller can go and is often more than the flash can follow.";
pub const QSPI_ADDRESS_NRF: &str = "How many address bytes the chip expects, and how fast to clock \
                                    it. The nRF52840 addresses in 24 bit, which reaches 16 MiB, or \
                                    32 bit for bigger flash; its bus runs at 32 MHz divided by 1 \
                                    to 16, so the fastest setting is 32 MHz and is often more \
                                    than the flash can follow.";
// ── SDMMC ─────────────────────────────────────────────────────────────────────

pub const SDMMC_WIDTH: &str = "Read-only: the width is however many data lanes you wired, not a \
                               setting. One, four and eight lanes are three different ways to \
                               drive a card, so widening the bus means assigning more D pins on \
                               the canvas, never changing a number here.";

pub const SDMMC_WIDTH_UNSUPPORTED: &str = "The data lanes you wired do not add up to a bus. A \
                                           card is driven over exactly 1, 4 or 8 lines, and at \
                                           any other count no SD-card code is generated - assign \
                                           or release D pins on the canvas until the count is \
                                           one of the three.";

pub const SDMMC_DATA_TIMEOUT: &str = "How long the host waits for a data block before giving up, \
                                      counted in CARD bus clock periods rather than \
                                      microseconds. The default 5 000 000 is a few seconds on a \
                                      slow card; too low and a healthy card fails mid-transfer, \
                                      too high and a dead one stalls the read.";
// ── SAI ───────────────────────────────────────────────────────────────────────

pub const SAI_SUBBLOCKS: &str = "Read-only: neither sub-block is wired, so this unit generates \
                                 nothing. A sub-block needs its bit clock, its data line and its \
                                 frame sync assigned on the canvas - the master clock pad is \
                                 optional and only appears in the code when you wire it.";

pub const SAI_STREAM: &str = "What this sub-block does: transmit or receive, master (this chip \
                              drives the bit clock and frame sync) or slave (the device on the \
                              other end does), and how many bits one sample carries. A and B are \
                              independent, so a codec is usually one of each.";

pub const SAI_FRAME: &str = "How one audio frame is laid out, and how much of it is buffered: \
                             stereo or mono, how many slots the frame holds, its total length in \
                             bits (slots x slot size - 32 is one 16-bit stereo frame), and the \
                             ring buffer in samples. Too small a buffer and the audio breaks up.";

pub const SAI_DMA: &str = "The channel this sub-block streams on. A SAI runs out of a ring \
                           buffer the DMA keeps filling, so a channel is not optional; Automatic \
                           takes the first free one, and pinning it by hand is for keeping a \
                           particular channel for something else. A and B need one each.";
// ── PARLIO ────────────────────────────────────────────────────────────────────

pub const PARLIO_DIRECTION: &str = "Which way this port moves data. It sends or it receives, \
                                    never both at once, and the choice is also what makes the \
                                    data pads and the clock outputs or inputs. The two halves \
                                    are separate modules on the canvas - PARL and PARL RX - and \
                                    both can run at once off the one port.";

pub const PARLIO_WIDTH: &str = "How many data lines move together on every clock tick. Assign \
                                D0..D15 on the canvas to match - only D0 and the clock are wired \
                                for you, and at sixteen lines the VALID signal is one of the \
                                data lines, so a VALID pad has nowhere to go.";

pub const PARLIO_WIDTH_NO_16: &str = "How many data lines move together on every clock tick, up \
                                      to eight. Sixteen is not offered because it exists only on \
                                      the first generation of this port, which this chip is not \
                                      - the ESP32-C6 is. Assign D0..D7 on the canvas to match.";

pub const PARLIO_CLOCK: &str = "The bus clock, in Hz. The whole width moves on every tick, so \
                                the throughput is this times the number of lines - that is the \
                                MB/s shown beside it. Too fast for the far end and bits are \
                                simply missed: neither side reports it.";

pub const PARLIO_BIT_ORDER: &str = "Which end of each byte goes onto the lines first. It has to \
                                    match the far end - get it backwards and every byte arrives \
                                    bit-reversed, which reads as scrambled data rather than as a \
                                    fault.";

pub const PARLIO_DMA_BUFFER: &str = "How many bytes the DMA moves in one go. This port has no \
                                     non-DMA form, so this block is the transfer - size it to \
                                     the frame you actually send, and remember the RAM is held \
                                     for as long as the program runs.";
// ── LCDCAM ────────────────────────────────────────────────────────────────────

pub const LCDCAM_MODE: &str = "Which display this half drives: an i8080 panel, where commands \
                               and pixels share the bus and WR is the strobe, or an RGB panel \
                               with no controller of its own, which needs the pixel clock and \
                               the sync lines driven forever. Two drivers over the same pads, so \
                               the control pads you assign differ.";

pub const LCDCAM_WIDTH: &str = "How many data pads this half drives, 8 or 16 - the peripheral \
                                has no other widths. Assign D0..D15 on the canvas to match: only \
                                D0 and D1 are wired for you, and a lane you leave unassigned is \
                                simply never driven.";

pub const LCDCAM_WIDTH_CAM: &str = "How many data lines this half reads off the sensor, 8 or 16 \
                                    - a DVP sensor in two-byte mode sends 16. Assign D0..D15 on \
                                    the canvas to match, and set the sensor to the same width, \
                                    or every pixel comes back misread.";

pub const LCDCAM_PIXEL_CLOCK_I8080: &str = "How fast bytes are strobed out to the display. Every \
                                            panel names a maximum on its datasheet - go past it \
                                            and pixels are dropped or garbled, with nothing in \
                                            software to catch it.";

pub const LCDCAM_PIXEL_CLOCK_DPI: &str = "The pixel clock the panel is fed. With the totals \
                                          below it sets the refresh rate: the whole line and \
                                          frame, blanking included, go by once per frame. This \
                                          and the timings both have to match the datasheet, or \
                                          the picture rolls.";

pub const LCDCAM_PIXEL_CLOCK_CAM: &str = "What the MCLK pad gives the sensor. The sensor divides \
                                          it down to its own pixel clock, so the frame rate you \
                                          get depends on this and on how the sensor is \
                                          programmed - it is not the rate at which pixels arrive \
                                          here.";

pub const LCDCAM_PIXEL_CLOCK_SLAVE: &str = "Greyed out: in slave mode the sensor is clocked from \
                                            elsewhere and drives PCLK itself, so nothing set \
                                            here is used. Turn on the master clock below to have \
                                            this chip supply the clock instead.";

pub const LCDCAM_MASTER_CLOCK: &str = "On, this chip generates the sensor's clock on the MCLK \
                                       pad, at the frequency set above - assign that pad on the \
                                       canvas or nothing comes out of it. Off is slave mode: the \
                                       sensor is clocked from elsewhere, drives PCLK itself, and \
                                       this chip only reads.";

pub const LCDCAM_ACTIVE_AREA: &str = "The visible picture, width by height in pixels. It has to \
                                      match the panel, and the framebuffer you send has to match \
                                      it too - a mismatch shows up as a shifted or torn picture, \
                                      never as a build error.";

pub const NONE: &str = "No const. It is punctuation between two number fields, not a setting: \
                        the meaning of both numbers is carried by the row label to its left \
                        (Active area, Total, Front porch, Sync width).";

pub const LCDCAM_TOTAL: &str = "The whole line and the whole frame, active area plus blanking, \
                                straight off the panel datasheet. With the pixel clock these \
                                decide the refresh rate, and leaving too little room for the \
                                porch and sync below means the panel never locks on.";

pub const LCDCAM_FRONT_PORCH: &str = "The idle gap between the end of the active area and the \
                                      sync pulse - one across the line, one down the frame. Off \
                                      the panel datasheet: a wrong one shifts the picture \
                                      sideways or rolls it, and nothing reports it.";

pub const LCDCAM_SYNC_WIDTH: &str = "How long HSYNC and VSYNC are held, in pixel clocks across \
                                     and in lines down. Off the panel datasheet again - too \
                                     short and the panel never locks on, so the screen stays \
                                     dark or the picture rolls.";

pub const LCDCAM_TRANSFERS: &str = "Not a choice: every mode of this port moves its data by DMA \
                                    and there is no CPU path at all. Which channel it took is on \
                                    the Configuration tab, and with no channel left the port \
                                    cannot be built.";
// ── TOUCH ─────────────────────────────────────────────────────────────────────

pub const TOUCH_SCAN: &str = "How often the pads are measured. One-shot measures when you ask, \
                              so a reading is only as fresh as your last request; Continuous \
                              keeps a hardware timer measuring in the background, which makes \
                              every read current and is the only mode you can await.";

pub const TOUCH_THRESHOLD_MODE: &str = "Which way a reading has to cross the threshold to count \
                                        as a touch. A finger adds capacitance, so the pad \
                                        charges more slowly and the count usually FALLS - \
                                        below-threshold is the normal choice, and above is for a \
                                        pad that behaves the other way.";

pub const TOUCH_THRESHOLD: &str = "The count that separates touched from untouched, one value \
                                   for every pad. There is no right number: read your own pad \
                                   untouched, then take a margin off it. Too tight and a warm \
                                   hand or a humid day trips it on its own; too loose and a real \
                                   touch never registers.";

pub const TOUCH_MEASUREMENT: &str = "How long one measurement runs, in cycles of the 8 MHz touch \
                                     clock. Longer gives a bigger count and a wider gap between \
                                     touched and untouched; shorter costs less time and less \
                                     power. Change it and your threshold moves with it.";

pub const TOUCH_SLEEP_CYCLES: &str = "Idle time between the background measurements, as a count \
                                      on the slow timer that paces them. Longer sleeps draw less \
                                      power and let the pad settle between readings; shorter \
                                      ones notice a finger sooner. Only continuous scanning has \
                                      this.";
// ── DAC ───────────────────────────────────────────────────────────────────────

pub const DAC_CHANNELS: &str = "Which of the two analog outputs this block drives. Not picked \
                                here - each channel is welded to one pad, so a channel is turned \
                                on by giving that pad its DAC OUT function on the canvas, and \
                                its level row then appears here.";

pub const DAC_START: &str = "The level this channel holds from startup. There is no off state - \
                             the pad drives a real voltage the moment the channel is enabled - \
                             so this is a resting point you are choosing deliberately: 0 is \
                             ground, 4095 is the reference, and mid is half of it.";

pub const DAC_START_ESP: &str = "The level this channel holds from startup - there is no off \
                                 state, the pad drives a real voltage as soon as the channel is \
                                 enabled. An ESP converter is eight bits against an STM32's \
                                 twelve, so the slider stops at 255: 256 levels and no finer.";

pub const DAC_NOTE_MORE_CHANNELS: &str = "This block has an output nothing has wired yet. Give \
                                          its pad the DAC OUT function on the canvas and a level \
                                          row for it appears here - the pad is fixed in silicon, \
                                          so there is nothing to choose but whether to use it.";

pub const DAC_NOTE_BLOCKING: &str = "Nothing is generated for this module on the Blocking or \
                                     Native runtimes - those backends write GPIO and watchdogs \
                                     only. Switch the runtime on the System tab and the DAC is \
                                     brought up and set to the levels above.";

pub const DAC_ESP_BOTH_RUNTIMES: &str = "On an ESP this module is generated whichever runtime \
                                         you pick. The DAC has no async surface at all - writing \
                                         a level is one register store - so Blocking and Async \
                                         produce the same code for it.";
// ── CUSTOM ────────────────────────────────────────────────────────────────────

pub const CUSTOM_STRUCT: &str = "Name of the generated struct. Left empty it follows the module \
                                 name, so renaming the module renames the struct and any impl \
                                 block you wrote against the old name stops matching. Type one \
                                 here and it stays put.";

pub const CUSTOM_PINS: &str = "The pins this module owns, in the order you added them - and that \
                               order is the order of the generated struct's fields, so it is a \
                               decision rather than a display. There is no dragging: to move a \
                               pin, remove it and add it again at the end.";

pub const CUSTOM_PIN_FUNCTION: &str = "This pin's function, the same list the chip itself \
                                       offers. It decides the pin's field name in the struct, so \
                                       changing it here is a change the module has to regenerate \
                                       and Update lights up again.";

pub const CUSTOM_PIN_UNSET: &str = "This pin has no function yet. Nothing can be generated for \
                                    it, so Update refuses and names it instead - pick In, Out or \
                                    PWM here. Those three are the only functions a custom module \
                                    can actually be handed.";

pub const CUSTOM_PIN_REMOVE: &str = "Takes this pin out of the module and frees it. The pin \
                                     keeps the function you gave it, and the Add pin list only \
                                     offers pins with no function, so it will not be offered \
                                     back until you reset it. Nothing reaches the generated code \
                                     until Update.";

pub const CUSTOM_PIN_NAME: &str = "A word of your own appended to this pin's generated variable \
                                   and to its field in the struct - `pc13` with `led` gives \
                                   `pc13_out_led`. The struct is built from these names, so \
                                   editing one makes Update light up again.";

pub const CUSTOM_NOTE_NO_PINS: &str = "A module with no pins generates nothing at all, and \
                                       Update stays disabled until it has one. Add pins from the \
                                       picker below.";

pub const CUSTOM_ADD_PIN: &str = "Adds a pin to the list. Only pins that are still unassigned \
                                  appear: one that already has a function, or is reserved, or \
                                  belongs to another module, is not offered. So the order is add \
                                  the pin here first, then give it its function on its row \
                                  above.";

pub const CUSTOM_UPDATE: &str = "Writes the struct for the pins above into a fresh file and \
                                 switches the project to it. Nothing regenerates until you press \
                                 it, so adding or renaming a pin cannot rewrite code you are \
                                 mid-edit in. Older revisions stay on disk, uncompiled, as \
                                 history.";

pub const CUSTOM_UPDATE_INCOMPLETE: &str = "Amber because a pin above still has no function. \
                                            Pressing it lists those pins instead of generating - \
                                            a field for a variable nothing ever binds would not \
                                            compile - so nothing is written until every pin is \
                                            In, Out or PWM.";

pub const CUSTOM_UPDATE_DISABLED: &str = "Greyed out because the generated file already matches \
                                          this pin list - same pins, same functions, same names; \
                                          change any of the three and it comes back. An empty \
                                          module is greyed out too, and the last file it wrote \
                                          stays where it is.";

pub const CUSTOM_UPDATE_PENDING: &str = "The pins here and the pins the generated file was built \
                                         from have drifted apart. The code on disk is still the \
                                         old list until Update is pressed - it will compile, it \
                                         just describes a module you have since changed.";

pub const CUSTOM_UNCONFIGURED_DIALOG: &str = "The pins named here have no function, so no field \
                                              can be generated for them. Click a pin's name in \
                                              the list above, or the pin on the chip, and choose \
                                              In, Out or PWM - those are the ones a custom \
                                              module can be handed.";

pub const SHARED_NAME_CUSTOM: &str = "On a custom module the name is not just a suffix on a \
                                      handle: it is the generated file's name and, unless Struct \
                                      above is filled in, the struct's name too. Renaming the \
                                      module renames both, and impl blocks you wrote stop \
                                      matching.";
// ── TIMER / PWM ───────────────────────────────────────────────────────────────

pub const TIMER_FREQUENCY: &str = "How many periods per second every channel on this timer puts \
                                   out. One timer, one frequency: the prescaler and the reload \
                                   are shared, so a change here moves every wired channel at \
                                   once - and the higher it goes, the fewer steps are left for a \
                                   duty to land on.";

pub const TIMER_FREQUENCY_ESP: &str = "How fast this LEDC timer runs, shared by every channel on \
                                       it. It also sets the window the Duty resolution row below \
                                       may pick from: the LEDC divides an 80 MHz clock, so a \
                                       higher frequency leaves fewer bits, and moving it can \
                                       push a width you pinned out of range.";

pub const TIMER_FREQUENCY_RP: &str = "How fast this slice counts, shared by its A and B \
                                      channels. An RP reaches a frequency with a whole-number \
                                      divider and a top value, so you get the nearest one it can \
                                      build rather than the number typed - the generated file \
                                      says in a comment what the pad really sees.";

pub const TIMER_DUTY_RESOLUTION_ESP: &str = "How many steps of duty a channel on this timer can \
                                             hold - 8 bit is 256 of them, 14 bit is 16384. Auto \
                                             takes the widest the current frequency allows. A \
                                             width outside that window is quietly narrowed to \
                                             fit, because esp-hal would otherwise refuse it and \
                                             the board would stop at boot.";

pub const TIMER_COUNTING: &str = "Which way the counter runs. Edge counts one way and restarts; \
                                  center-aligned counts up then back down, so a pulse sits in \
                                  the middle of its period and several channels do not all \
                                  switch on the same edge - which is what motor drive wants. The \
                                  three center modes differ only in when the compare interrupt \
                                  fires.";

pub const TIMER_COUNTING_INERT_RP: &str = "Offered here, but a Pico does not read it: on Async \
                                           the slice counts one way, and the Blocking file \
                                           always turns phase-correct counting on. Whichever \
                                           mode you pick, the counter shape on an RP is the \
                                           backend's, so treat this row as having no effect on \
                                           that chip.";

pub const TIMER_CHANNELS_EMPTY: &str = "Channels are not added from this panel. One appears when \
                                        you give a pad this timer's CHn function on the Pins \
                                        canvas, and it leaves when you take it away - so an \
                                        empty list means nothing is wired here yet and this \
                                        timer generates no PWM at all.";

pub const TIMER_CHANNELS_EMPTY_ESP: &str = "No pad is wired to this LEDC timer yet. Channels are \
                                            not added from this panel: give a GPIO one of the \
                                            timer's channel functions on the Pins canvas. Any \
                                            pad can carry any channel, because the GPIO matrix \
                                            routes it - the pad is free and the channel number \
                                            is yours.";

pub const TIMER_CHANNELS_EMPTY_RP: &str = "No pad is wired to this slice yet. Channels are not \
                                           added from this panel, and on an RP they are not \
                                           chosen either - the pad decides. Give a GPIO this \
                                           slice's function on the Pins canvas: an even one \
                                           lands on channel A, an odd one on channel B.";

pub const TIMER_DUTY: &str = "How much of each period this channel holds active, down to a \
                              hundredth of a percent - a hobby servo wants 1.5 ms of a 20 ms \
                              frame, which is 7.50 % and whole percent cannot say it. A channel \
                              you never touch stays at 0 %, the quiet state a driver stage wants \
                              at reset.";

pub const TIMER_DUTY_ESP: &str = "How much of each period this channel holds on. You set it to a \
                                  hundredth of a percent, but esp-hal's LEDC takes whole \
                                  percent, so the value that reaches the board is rounded UP - \
                                  ask for 7.50 % and the pad starts at 8 %. Coarse duty is the \
                                  chip's limit here, not the slider's.";

pub const TIMER_DUTY_RP: &str = "How much of each period this channel holds high. On an RP it \
                                 becomes a compare value against the slice's top, and top falls \
                                 out of the frequency - so at a high frequency few steps are \
                                 left and two nearby percentages can land on exactly the same \
                                 pulse width.";

pub const TIMER_CHANNEL_OUTPUT: &str = "Three things about the pad itself. Push-pull drives both \
                                        levels, open-drain only pulls down and needs a pull-up \
                                        on the board. Active low inverts the output, so 100 % \
                                        duty holds the pin LOW - what a current-sinking driver \
                                        stage wants. PWM mode 2 is a second route to that same \
                                        inversion.";

pub const TIMER_CHANNEL_OUTPUT_COMPLEMENTARY: &str = "Three things about the pad. Push-pull \
                                                      drives both levels, open-drain only pulls \
                                                      down. Active low inverts the output, so \
                                                      100 % duty holds the pin LOW. Because this \
                                                      timer has a complementary CHxN pad wired, \
                                                      PWM mode 2 has no effect and the inversion \
                                                      reaches the main pad only - use Active \
                                                      low.";

pub const TIMER_CHANNEL_OUTPUT_INERT_RP: &str = "Offered here, but a Pico reads none of the \
                                                 three: its slice outputs are push-pull and \
                                                 active high, and it has no PWM mode to choose. \
                                                 To get an inverted waveform on an RP, ask for \
                                                 the complement of the duty instead - 30 % where \
                                                 you wanted 70 %.";

pub const TIMER_DEAD_TIME: &str = "The gap the hardware forces between one pad of a pair \
                                   switching off and the other switching on, counted in the same \
                                   ticks as the duty. One setting for the whole timer. Zero \
                                   means both sides move at the same instant - harmless for two \
                                   independent loads, a shoot-through short across a \
                                   half-bridge.";

pub const TIMER_BREAK_INPUT: &str = "A fault line the timer watches by itself: when it asserts, \
                                     every output on this timer goes off in hardware, with no \
                                     code in the path. Active low is the usual wiring - a broken \
                                     wire then reads as a fault too. The filter is how many \
                                     samples in a row must agree before the fault is believed.";

pub const TIMER_AUTO_OUTPUT_ENABLE: &str = "What happens once the fault line releases. Off - the \
                                            reset state, and the safer one - keeps the outputs \
                                            dark until your code turns them back on, so a fault \
                                            cannot be ridden out unnoticed. On brings them back \
                                            by themselves at the next period, straight into \
                                            whatever caused the fault.";
// ── Reasons a row is NOT offered here ─────────────────────────────────────────

pub const SKIP_F1_SERIAL: &str = "stm32f1xx-hal builds a Serial only from the TX+RX pair, so it \
                                  has no flow control, no half duplex and no one-way UART - and \
                                  the F1's USART has no swap or invert bits either. The rows are \
                                  absent because the HAL has nothing to pass them to.";

pub const SKIP_USART_BUF_TX_ONLY: &str = "A TX-only DMA link has no buffer at all: the \
                                          controller sends straight from the slice you hand it, \
                                          so there is nothing to size. Give the link a receiving \
                                          half and the row comes back.";

pub const SKIP_USART_BUF_RP_DMA: &str = "Only the buffered transport has a buffer on this chip. \
                                         embassy-rp's DMA `Uart` reads straight into the slice \
                                         you pass `read`, with no ring behind it - there is \
                                         nothing to size, and the row used to size a buffer that \
                                         was never allocated. Switch the transport to buffered \
                                         and it comes back.";

pub const SKIP_I2C_TIMEOUT_RP: &str = "embassy-rp's I2C takes no timeout. Its `Config` carries \
                                       the frequency alone, and a transfer that hangs on a \
                                       stretched clock hangs - the row was a control with \
                                       nothing to pass it to. On an STM32 the timeout is part of \
                                       embassy-stm32's own `Config`.";

pub const SKIP_USART_BUF_NRF: &str = "No buffer to size on an nRF. embassy-nrf's `Uarte` reads \
                                      straight into the slice you pass `read`, through EasyDMA, \
                                      with no ring behind it. Its `BufferedUarte`, which keeps \
                                      receiving between reads, needs a TIMER and PPI channels and \
                                      is not what this backend builds.";

pub const SKIP_USART_TRANSPORT_NRF: &str = "Nothing to choose on an nRF: the UARTE moves bytes by \
                                            EasyDMA, which is part of the peripheral, so there is \
                                            no DMA channel to take or give back. This backend \
                                            builds embassy-nrf's async `Uarte`.";

pub const SKIP_ASYNC_INIT_NRF: &str = "One form on an nRF. embassy-nrf's SPIM and TWIM are async \
                                       drivers that move bytes by EasyDMA, which is part of each \
                                       peripheral, and the same handle has `blocking_*` methods \
                                       for when you want to wait. No channel is taken either way, \
                                       so the two choices would build the same code.";

pub const SKIP_I2C_TIMEOUT_NRF: &str = "embassy-nrf's TWIM `Config` has no timeout, so the row had \
                                        nothing to pass it to. An async transfer that hangs on a \
                                        stretched clock waits until it is dropped: wrap the call \
                                        in `embassy_time::with_timeout` to bound it.";

pub const SKIP_SPI_BIT_ORDER: &str = "Only the async path can set it: stm32f1xx-hal's blocking \
                                      SPI takes no bit-order argument, so the row would be a \
                                      control that changes nothing. Every device this backend \
                                      builds sends MSB first.";

pub const SKIP_SPI_ROLE: &str = "Master only on this chip. An esp-hal slave can only work \
                                 through DMA, and an STM32 slave is work this IDE has not done \
                                 rather than a limit of the silicon - either way there is no \
                                 second role to pick.";

pub const SKIP_TOUCH_SLEEP: &str = "The sleep timer belongs to the continuous scan: in one-shot \
                                    the controller measures when you ask it to and idles in \
                                    between, so there is no interval to set.";

pub const SKIP_CAN_MODE: &str = "One mode on this chip. Loopback and listen-only are esp-hal's \
                                 TWAI settings; the embassy CAN path this backend builds offers \
                                 the normal bus mode alone.";
// ── Settings every module has, that its grid does not draw ───────────────────

pub const SHARED_INSTANCE: &str = "Which peripheral this module drives. Not editable: it is read \
                                   back off the pads - a module is keyed on the function each \
                                   wired pin carries - so moving the wiring to another instance is \
                                   what changes it.";

pub const SHARED_INSTANCE_TIMER_STM32: &str = "The TIM this module drives, read back off the pads \
                                               rather than chosen: a channel pad names its timer, \
                                               so re-wiring is what moves the module.";

pub const SHARED_INSTANCE_TIMER_ESP: &str = "The LEDC timer this module drives. Unlike an STM32's \
                                             TIM, its channels are not welded to pads - the GPIO \
                                             matrix routes them - so the pad picks the channel and \
                                             the module keeps the timer.";

pub const SHARED_INSTANCE_TIMER_RP: &str = "The PWM SLICE this module drives. A slice is welded to \
                                            its pads on the RP: pad n below 32 belongs to slice \
                                            (n/2) % 8, pad n from 32 up (RP2350B only) to slice \
                                            8 + ((n-32)/2) % 4, and A or B by whether it is even \
                                            or odd.";

pub const SHARED_INSTANCE_CUSTOM: &str = "A custom module drives no peripheral, so this number \
                                          means nothing - it exists only so every module can \
                                          answer the same question. Custom modules are told apart \
                                          by their id.";

pub const SHARED_NAME: &str = "The label from the Name row above, and from the field inside the \
                               module's box on the canvas. It is lowercased, every run of \
                               non-alphanumerics becomes one underscore, and the result is \
                               appended to the generated handle - `servo` gives `_pwm3_servo`.";

pub const SHARED_NAME_RP: &str = "The label from the Name row above, and from the field inside the \
                                  module's box on the canvas. The Pico backend does not read it \
                                  yet, so handles keep their bare names there; every other backend \
                                  appends it - `servo` gives `_pwm3_servo`.";

pub const SHARED_DATA_MODELS: &str = "Free-text Rust this module carries for the frames it sends \
                                      and receives. Nothing edits them today - a palette template \
                                      can fill them, or you can by hand - and whatever is there is \
                                      written into main.rs as a `mod` block named after the \
                                      module's id. Additively: a block already there is left \
                                      alone, so your edits inside it survive regeneration.";

// ── USART / LPUART ───────────────────────────────────────────────────────────

pub const USART_BAUD: &str = "Bits per second on the wire, both ways. It has to match the other end exactly - there is no negotiation on a UART. Pick a preset, or Custom… for any other rate. Where the IDE can check the chip, the line under the field says what it really makes of the rate at the current clock, and each preset shows its error in the list: the ceiling is a fraction of the clock feeding the peripheral, not a property of the chip.";

pub const USART_DATA_BITS: &str = "Bits in one character, parity NOT counted. 8 is what almost \
                                   everything uses; 9 exists for the addressed multi-drop modes.";

pub const USART_PARITY: &str = "An extra bit that makes the count of ones even or odd, so a \
                                single flipped bit is noticed. It is taken OUT of the character: \
                                8 data bits with parity is 9 bits on the wire.";

pub const USART_STOP_BITS: &str = "How long the line is held idle after each character, giving \
                                   the receiver time to resynchronise. One is normal; two is for \
                                   a slow or noisy peer.";

pub const USART_BUF_BUFFERED: &str = "Size of BOTH software ring buffers, TX and RX. The CPU \
                                      copies byte by byte on each interrupt, so this has to cover \
                                      what arrives between your reads.";

pub const USART_BUF_DMA: &str = "The circular buffer the DMA controller fills on its own. \
                                 Reception never stops, so this only has to cover the longest GAP \
                                 between your reads - overrun it and the OLDEST bytes are dropped, \
                                 silently.";

pub const USART_ASYNC_TRANSPORT: &str = "Who moves the bytes. Buffered costs one interrupt per \
                                         byte and no DMA channel, so it always builds; DMA hands \
                                         the peripheral straight to the controller and keeps \
                                         receiving between your reads, at the price of channels.";

pub const USART_TRANSFERS_ESP: &str = "Fixed on an ESP: esp-hal moves UART bytes over DMA through \
                                       UHCI, a driver of its own this generator does not write \
                                       yet. The SPI module's DMA is a different peripheral and \
                                       does work.";

pub const USART_DIRECTION: &str = "Which halves to build. A one-way link frees the other pad for \
                                   something else; the half-duplex modes put both directions on \
                                   ONE wire.";

pub const USART_DIRECTION_LOCKED: &str = "Both halves, and no choice: embassy has no buffered \
                                          TX-only or RX-only - `BufferedUartTx` and \
                                          `BufferedUartRx` come only from splitting a \
                                          `BufferedUart`, so both pads are used either way. \
                                          Switch the transport to DMA for a one-way UART.";

/// The same row on an RP, where BOTH halves of the sentence above are false.
///
/// embassy-rp has real standalone constructors on both transports -
/// `BufferedUartTx::new` / `BufferedUartRx::new` (uart/buffered.rs:344, :195)
/// and `UartTx::<Async>::new` / `UartRx::<Async>::new` (uart/mod.rs:251, :403),
/// the DMA ones taking a single channel. So the pair is not the chip's limit and
/// switching transport unlocks nothing: `async_bus_lines` skips a UART with only
/// one pad wired, on either transport.
pub const USART_DIRECTION_LOCKED_RP: &str = "Both halves, and no choice yet. embassy-rp does \
                                             have one-way constructors on both transports - \
                                             BufferedUartTx / BufferedUartRx and UartTx / \
                                             UartRx, the DMA pair taking one channel each - but \
                                             this backend emits only the bidirectional form, so \
                                             a UART with a single pad wired generates nothing. A \
                                             gap in the generator, not in the chip.";

pub const USART_LINE: &str = "Pad-level fixes done inside the peripheral, so a crossed cable or an \
                              inverting transceiver needs no rework: swap RX with TX, or invert \
                              either line's idle level.";

pub const USART_LINE_ABSENT: &str = "This USART has no swap or invert bits - they arrived with a \
                                     later revision of the peripheral, and embassy's `Config` has \
                                     no field for them here.";

pub const USART_FLOW: &str = "Hardware handshaking. CTS lets the peer stop this node mid-stream, \
                              RTS lets this node stop the peer, and DE drives a transceiver's \
                              direction pin for RS-485. Each needs its own pad wired.";

pub const USART_HALF_DUPLEX_READBACK: &str = "One wire carries both directions, so everything this \
                                              node sends lands on its own receiver. Off - the \
                                              default - mutes the receiver while transmitting, \
                                              which is what a bus with other talkers wants; on \
                                              keeps the echo, which is how you check that a driver \
                                              can be shouted down.";

#[cfg(test)]
mod tests {
    use super::*;

    use super::ALL_DOCS as ALL;

    /// The table names every const this module declares.
    ///
    /// Without this, adding a const and forgetting the table entry silently
    /// exempts it from every check below - and from the panel test that a
    /// collected doc is a NAMED const, which is what stops an inline literal
    /// creeping back in.
    #[test]
    fn the_table_lists_every_const_in_this_file() {
        const SRC: &str = include_str!("module_docs.rs");
        let declared: Vec<&str> = SRC
            .lines()
            .filter_map(|l| l.strip_prefix("pub const "))
            .filter_map(|l| l.split_once(":"))
            .map(|(n, _)| n)
            // The tables and their aliases are not sentences.
            .filter(|n| !matches!(*n, "ALL_DOCS" | "ROSTER"))
            .collect();
        for n in &declared {
            assert!(
                ALL.iter().any(|(name, _)| name == n),
                "{n} is declared but missing from ALL_DOCS"
            );
        }
        assert_eq!(declared.len(), ALL.len(), "ALL_DOCS has an entry too many");
    }

    /// A `\`-continued literal keeps the newline's leading indentation UNLESS
    /// the backslash eats it — and rustfmt has more than once joined such a
    /// literal back onto one line, spaces and all. `ui.label` does not collapse
    /// them, so the reader gets a gap in the middle of a sentence.
    ///
    /// This has bitten the module panel twice (project memory
    /// `rustfmt-joins-continued-strings`; forty instances collapsed in
    /// `modules.rs` on 2026-08-28). Here it cannot: the strings have names, so a
    /// test can walk them.
    #[test]
    fn no_doc_string_carries_a_run_of_spaces() {
        for (name, s) in ALL {
            assert!(!s.contains("  "), "{name} has a run of spaces:\n{s}");
        }
    }

    /// A const that is empty, or that stops mid-thought, is worse than no
    /// entry: the pane would draw a label with nothing beside it.
    #[test]
    fn every_doc_string_is_a_finished_sentence() {
        for (name, s) in ALL {
            assert!(s.len() > 30, "{name} is too short to be a sentence: {s:?}");
            assert!(
                s.ends_with('.') || s.ends_with('?'),
                "{name} does not end a sentence: {s:?}"
            );
            assert_eq!(s.trim(), *s, "{name} has stray whitespace at an end");
        }
    }

    /// The pane must not show a partial roster — see
    /// [`ConfigOut::all_fields_documented`].
    #[test]
    fn fields_are_withheld_until_an_arm_says_it_is_done() {
        let mut out = ConfigOut::default();
        out.field("Baud rate", USART_BAUD);
        assert!(out.fields().is_none(), "not marked complete yet");
        out.all_fields_documented();
        assert_eq!(out.fields().map(<[_]>::len), Some(1));
    }

    /// A row drawn once per wired channel is documented once.
    #[test]
    fn a_repeated_label_is_documented_once() {
        let mut out = ConfigOut::default();
        out.field("CH duty", USART_BAUD);
        out.field("CH duty", USART_PARITY);
        out.all_fields_documented();
        let f = out.fields().unwrap();
        assert_eq!(f.len(), 1);
        // The FIRST wins: it is the one the reader met first.
        assert_eq!(f[0].doc, USART_BAUD);
    }

    /// "Set elsewhere" is NOT held back by the completeness gate: it is true for
    /// every kind the day it is written, and hiding a finished answer behind an
    /// unfinished one would be the wrong trade.
    #[test]
    fn the_elsewhere_group_does_not_wait_for_the_rows() {
        let mut out = ConfigOut::default();
        out.elsewhere("Instance", SHARED_INSTANCE);
        assert!(out.fields().is_none(), "the rows are still ungated");
        assert_eq!(out.elsewhere_fields().len(), 1);
        // …and it dedupes the same way.
        out.elsewhere("Instance", SHARED_NAME);
        assert_eq!(out.elsewhere_fields().len(), 1);
        assert_eq!(out.elsewhere_fields()[0].doc, SHARED_INSTANCE);
    }

    /// Notes and fields travel together but stay separate — the pane draws them
    /// under different headings.
    #[test]
    fn notes_and_fields_do_not_mix() {
        let mut out = ConfigOut::default();
        assert!(out.is_empty());
        out.note("something to know");
        out.field("Parity", USART_PARITY);
        assert_eq!(out.notes().len(), 1);
        assert!(!out.is_empty());
    }
}

/// The hover for a module's signal legend.
///
/// One sentence per picture, and every one of them ends with the same clause:
/// the same misreading - "this is my module's setting" - is available for all
/// of them, and the PWM one already had to say so. Named consts rather than
/// inline literals so the run-of-spaces guard in this file walks them too.
pub fn legend_hover(kind: crate::panels::mcu_module::modules::ModuleKind) -> &'static str {
    use crate::panels::mcu_module::modules::ModuleKind as K;
    match kind {
        K::GenericInterfaceTimer => LEGEND_PWM,
        K::GenericInterfaceUsb => LEGEND_USB,
        K::GenericInterfaceTouch => LEGEND_TOUCH,
        K::GenericInterfaceDac => LEGEND_DAC,
        K::GenericInterfacePcnt => LEGEND_PCNT,
        K::GenericInterfaceSpi => LEGEND_SPI,
        K::GenericInterfaceI2c => LEGEND_I2C,
        K::GenericInterfaceI2s | K::GenericInterfaceSai => LEGEND_I2S,
        K::GenericInterfaceMcpwm => LEGEND_MCPWM,
        K::GenericInterfaceUsart | K::GenericInterfaceLpuart => LEGEND_UART,
        K::GenericInterfaceCan => LEGEND_CAN,
        K::GenericInterfaceRmt => LEGEND_RMT,
        K::GenericInterfaceQspi
        | K::GenericInterfaceOspi
        | K::GenericInterfaceXspi
        | K::GenericInterfaceHspi
        | K::GenericInterfaceSdmmc => LEGEND_MEMORY,
        K::GenericInterfaceParlIo | K::GenericInterfaceParlIoRx => LEGEND_PARALLEL,
        K::GenericInterfaceLcdCam | K::GenericInterfaceCamera => LEGEND_LCD_CAM,
        _ => "",
    }
}

pub const LEGEND_PWM: &str = "What duty cycle does: the wider the pulse, the longer the output stays high, and the higher the average the load sees. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_USB: &str = "A differential pair: the two lines are mirrors of each other, except at the end of a packet where BOTH go low. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_TOUCH: &str = "A capacitance reading. A finger adds capacitance, so the count falls - and crossing the threshold is what counts as a touch. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_DAC: &str = "The code you write, and the level the pad holds. Drawn as a staircase because a DAC has a finite number of steps to reach for. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_PCNT: &str = "Edges in, a count out: the counter steps once per edge and holds between them. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_SPI: &str = "A clock and data that moves only on its edges. The clock runs from end to end, which is what tells this picture apart from I2C. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_I2C: &str = "The two conditions that frame every transfer: SDA falling while SCL is high starts one, SDA rising while SCL is high ends it. They are the only moments SDA may move with the clock high, which is why they can be the delimiters. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_I2S: &str = "A bit clock, and the word select that runs far slower than it - one edge per sample. The rate difference is the point; SAI puts the same three signals on the wire. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_UART: &str = "One asynchronous frame: idle high, a start bit down, data, a stop bit back up. There is no clock line - those two edges are all the receiver has to find the byte with, which is why they are marked. LPUART puts the same frame on the wire. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_MEMORY: &str = "A clock and a bus that turns around: the same lines carry the command out and the data back, with a gap in between where nobody drives them. The hexagons are the usual way to draw a bus whose width is not the point - here it is your wiring, not a setting. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_PARALLEL: &str = "A clock and a bus that goes one way: every window carries a value, latched on the clock edge. That one direction is what separates it from a memory port, whose bus has to turn around. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_LCD_CAM: &str = "A pixel bus and the sync that frames it. True of all three shapes this peripheral wears - a panel strobed by WR, a panel on a free-running pixel clock, and a camera whose sensor drives the clock - which is why the picture shows what they share rather than one of them. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_RMT: &str = "A list of durations sent out and then done: the high and low runs are whatever you put in them, and the train ENDS - which is what a PWM never does. The level it rests at afterwards is yours to pick. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_CAN: &str = "Recessive at rest, a dominant start, then the ACK slot - the one bit the sender leaves recessive and every listening node pulls down. It is the only thing on this wire that is not the sender's. An illustration of the peripheral - not this module's own setting, which is in the rows below.";

pub const LEGEND_MCPWM: &str = "A complementary pair, and the dead time between them: for that window BOTH outputs are off, which is what stops a bridge shooting through. Drawn far wider than a real one, which would be invisible here. An illustration of the peripheral - not this module's own setting, which is in the rows below.";
