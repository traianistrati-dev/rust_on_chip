//! MCU business logic — partner assignment, state management, pin lookups.

use super::model::Mcu;
use crate::panels::mcu_module::modules::autowire;
use crate::panels::mcu_module::pins::logic::pin::Pin;
use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

// ── Peripheral pin groups ─────────────────────────────────────────────────────
// Defines which functions must be co-selected / co-deselected as a group.
// Selecting any member of a group auto-assigns the rest to the nearest
// available Unset pin; deselecting one member removes the whole group.

pub fn partner_functions(func: &PinFunction) -> Vec<PinFunction> {
    match func {
        // USART — basic full-duplex pair
        PinFunction::UsartTx(n) => vec![PinFunction::UsartRx(*n)],
        PinFunction::UsartRx(n) => vec![PinFunction::UsartTx(*n)],
        // USART — hardware flow-control pair (optional, separate from TX/RX)
        PinFunction::UsartCts(n) => vec![PinFunction::UsartRts(*n)],
        PinFunction::UsartRts(n) => vec![PinFunction::UsartCts(*n)],
        // LPUART — a peripheral of its own, paired exactly like the USART, so
        // assigning one half by hand completes the module the same way.
        PinFunction::LpuartTx(n) => vec![PinFunction::LpuartRx(*n)],
        PinFunction::LpuartRx(n) => vec![PinFunction::LpuartTx(*n)],
        PinFunction::LpuartCts(n) => vec![PinFunction::LpuartRts(*n)],
        PinFunction::LpuartRts(n) => vec![PinFunction::LpuartCts(*n)],
        // SPI — three-wire bus (NSS is optional, not auto-assigned)
        PinFunction::SpiSck(n) => vec![PinFunction::SpiMiso(*n), PinFunction::SpiMosi(*n)],
        PinFunction::SpiMiso(n) => vec![PinFunction::SpiSck(*n), PinFunction::SpiMosi(*n)],
        PinFunction::SpiMosi(n) => vec![PinFunction::SpiSck(*n), PinFunction::SpiMiso(*n)],
        // I²C — two-wire bus
        PinFunction::I2cScl(n) => vec![PinFunction::I2cSda(*n)],
        PinFunction::I2cSda(n) => vec![PinFunction::I2cScl(*n)],
        // CAN — differential pair
        PinFunction::CanRx => vec![PinFunction::CanTx],
        PinFunction::CanTx => vec![PinFunction::CanRx],
        // USB — differential pair
        PinFunction::UsbDm => vec![PinFunction::UsbDp],
        PinFunction::UsbDp => vec![PinFunction::UsbDm],
        // SWD — two-wire debug
        PinFunction::SwdIo => vec![PinFunction::SwdClk],
        PinFunction::SwdClk => vec![PinFunction::SwdIo],
        // GPIO, ADC, Timer, MCO, SpiNss, UsartCk — no automatic partners
        _ => vec![],
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

impl Mcu {
    /// The watchdog `init(...)` lines followed by the Custom-module ones.
    ///
    /// One string because every backend already threads a single "extra
    /// inits" slot through `make_generated_section`; adding a parameter to
    /// each of the nine call sites would have been churn for no gain.
    /// Watchdogs come FIRST - one that is meant to catch a hang during
    /// start-up is worth arming before the code that might hang.
    pub fn watchdog_and_custom_inits(&self) -> String {
        // The flash store between the two: after the clocks (on the F1 it
        // takes over the `flash` whose `acr` froze them), before the Custom
        // modules. STM32 only - the ESP backends have their own slot.
        let store = if self.family.starts_with("stm32") {
            crate::panels::mcu_module::codegen::flash_store_gen::init_lines_for(self)
        } else {
            String::new()
        };
        // Each part carries its own header, so one that is absent leaves no
        // empty heading behind (the backends used to put "Custom modules"
        // above the whole slot, watchdogs and store included).
        let custom = self.custom_module_inits();
        let custom = if custom.is_empty() {
            custom
        } else {
            format!("\n    // ── Custom modules ──\n{custom}")
        };
        format!(
            "{}{}{}",
            crate::panels::mcu_module::codegen::watchdog_gen::init_lines(
                &self.watchdog,
                &self.family,
                self.runtime,
            ),
            store,
            custom,
        )
    }
    /// Create a new MCU with the given configuration.
    ///
    /// `family` is the codegen backend key (e.g. "stm32f1", "esp32c3"); see
    /// [`FamilyBackend`](crate::panels::mcu_module::codegen::family::FamilyBackend).
    pub fn new(
        name: String,
        family: String,
        toolchain: crate::panels::mcu_module::mcu_catalog::ToolchainKind,
        top_pins: Vec<Pin>,
        bottom_pins: Vec<Pin>,
        left_pins: Vec<Pin>,
        right_pins: Vec<Pin>,
    ) -> Self {
        use crate::panels::mcu_module::clock::graph::{
            GraphClock, layout::stm32f1_layout, stm32f1_graph,
        };
        use crate::panels::mcu_module::clock::{ClockConfig, ClockLimits, Stm32f1Clock};
        // Only the STM32F1 family has a built-in clock graph; others get `None`
        // (a definition's `ClockDef` overrides this in `build_mcu`).
        let clock = match family.as_str() {
            "stm32f1" => ClockConfig::Graph(GraphClock {
                graph: stm32f1_graph(&Stm32f1Clock::default()),
                layout: stm32f1_layout(&ClockLimits::default()),
                bindings: Default::default(),
            }),
            _ => ClockConfig::None,
        };
        // The tree as built here IS the default — `build_mcu` re-captures after
        // overriding `clock` from the definition.
        // A family with no RCC recipe cannot have its clock generated, so its
        // block is hand-written from the start.
        let clock_manual = !crate::panels::mcu_module::codegen::rcc::generates_clock_code(&family);
        let clock_defaults = match &clock {
            ClockConfig::Graph(gc) => Some(gc.graph.clone()),
            ClockConfig::None => None,
        };
        Self {
            id: String::new(),
            name,
            family,
            toolchain,
            top_pins,
            bottom_pins,
            left_pins,
            right_pins,
            // Edge-packaged by default; a ball-grid chip fills this in
            // afterwards (see `McuDefinition::build_mcu`).
            grid: None,
            dma: None,
            irq_vectors: Vec::new(),
            // A bare chip until the definition says otherwise.
            board_chip: None,
            board_flash: None,
            usart_ip: None,
            sdmmc_ip: None,
            selected_pin: None,
            pin_search: String::new(),
            show_info: None,
            fn_scroll_offset: 0.0,
            clock,
            clock_limits: ClockLimits::default(),
            clock_presets: Vec::new(),
            clock_defaults,
            clock_manual,
            modules: Vec::new(),
            runtime: crate::panels::mcu_module::mcu::model::Runtime::default(),
            gpio_api: crate::panels::mcu_module::modules::ApiStyle::default(),
            pending_runtime: crate::panels::mcu_module::mcu::model::Runtime::default(),
            pending_gpio_api: crate::panels::mcu_module::modules::ApiStyle::default(),
            pending_module_styles: std::collections::BTreeMap::new(),
            pending_apply_confirm: false,
            config_regen_forced: false,
            auto_build: crate::panels::mcu_module::mcu::model::AutoBuild::default(),
            strict_lints: false,
            debug_build: false,
            expand_module: None,
            module_undo: Vec::new(),
            module_remove_confirm: None,
            i2c_remove_confirm: None,
            pending_i2c_acts: Vec::new(),
            pin_goto: None,
            module_goto: None,
            selected_module: None,
            selected_i2c_child: None,
            bus_reach: (0.0, 0.0),
            selected_device: None,
            device_remove_confirm: None,
            device_tabs: Vec::new(),
            device_drag: None,
            collapse_modules: false,
            rotated: false,
            io_pin_pos: std::collections::BTreeMap::new(),
            i2c_child_pos: std::collections::BTreeMap::new(),
            groups: Vec::new(),
            module_notes: std::collections::BTreeMap::new(),
            watchdog: Default::default(),
            comp: Default::default(),
            flash_store: None,
            iot: Default::default(),
        }
    }

    /// A 4-sided (QFP-style) package — pins on 3 or 4 edges.
    ///
    /// One of the two inputs to
    /// [`RotMode::for_package`](crate::panels::mcu_module::mcu::gui::rotate::RotMode::for_package),
    /// not the whole rotation rule: a ball grid has every side vec EMPTY and
    /// still becomes a diamond, on the strength of
    /// [`has_inner_pins`](Self::has_inner_pins). Only a genuinely 2-sided (DIP)
    /// package is left to rotate 90°.
    pub fn is_quad_package(&self) -> bool {
        [
            &self.top_pins,
            &self.bottom_pins,
            &self.left_pins,
            &self.right_pins,
        ]
        .iter()
        .filter(|v| !v.is_empty())
        .count()
            >= 3
    }

    // ── Virtual modules ───────────────────────────────────────────────────────

    /// Does this CHIP have the pins to host `kind` at all? A dry run of the
    /// real auto-wiring against a pristine chip (nothing wired yet), so it
    /// answers with exactly the logic [`add_module`](Self::add_module) uses —
    /// including the subtle part, that one peripheral INSTANCE must offer all
    /// the required signals (it isn't enough that some pin can TX and some
    /// unrelated pin can RX).
    ///
    /// Static: independent of what's currently wired. Use it to hide kinds the
    /// chip simply doesn't have.
    /// Why a peripheral this chip HAS is still not offered.
    ///
    /// [`supports_module`](Self::supports_module) answers from the pins, which
    /// is the right question almost always: no pins, no module. But a pin is
    /// only offered when something can be GENERATED for it, so a peripheral the
    /// silicon has and the HAL cannot drive disappears from the palette with no
    /// explanation — and someone holding the datasheet is left wondering.
    ///
    /// This returns the sentence to show instead. The palette keeps the entry
    /// visible and disabled, the same as it does for an exhausted instance.
    pub fn hardware_only_reason(
        &self,
        kind: crate::panels::mcu_module::modules::ModuleKind,
    ) -> Option<&'static str> {
        use crate::panels::mcu_module::modules::ModuleKind;
        match kind {
            // The F1's USB, CAN and SDIO on the Async runtime: the silicon is
            // there, the generated code is not. Only where the pads ARE there,
            // though - a value-line F100 has none of the three, and a disabled
            // row would claim it had.
            ModuleKind::GenericInterfaceUsb
            | ModuleKind::GenericInterfaceCan
            | ModuleKind::GenericInterfaceSdmmc => {
                crate::panels::mcu_module::codegen::family::f1_async_module_gap(
                    &self.family,
                    self.runtime,
                    kind,
                )
                .filter(|_| {
                    autowire::any_wiring_static(self, &Default::default(), kind.signals().0)
                })
            }
            // The S2 and S3 carry the touch sensors — their pads are in
            // Espressif's own pin tables — but esp-hal builds `touch` only for
            // the original ESP32, and does not even expose `peripherals::TOUCH`
            // on the other two. There is nothing to hand a constructor.
            ModuleKind::GenericInterfaceTouch
                if matches!(self.family.as_str(), "esp32s2" | "esp32s3") =>
            {
                Some(
                    "This chip HAS capacitive touch, but esp-hal builds no touch driver \
                     for it - only for the original ESP32. Nothing could be generated, so \
                     the pads are not offered either. An external touch controller over \
                     I2C works today.",
                )
            }
            _ => None,
        }
    }

    pub fn supports_module(&self, kind: crate::panels::mcu_module::modules::ModuleKind) -> bool {
        use crate::panels::mcu_module::modules::autowire;
        // A custom module needs no particular peripheral — any chip can host it.
        if kind.is_custom() {
            return true;
        }
        // Support is derived from the PINS below, which is right for every
        // peripheral whose init the backend can actually write. USB is the
        // exception: the D-/D+ pins exist on chips whose backend generates no
        // USB code at all, so the module was addable and produced nothing but
        // two stray dependencies. Only the family can answer that.
        if kind == crate::panels::mcu_module::modules::ModuleKind::GenericInterfaceUsb
            && !crate::panels::mcu_module::codegen::family::usb_supported(&self.family)
        {
            return false;
        }
        // An F1 on Async generates nothing for USB, CAN or SDIO;
        // `hardware_only_reason` keeps them in the palette, disabled, with the
        // reason.
        if crate::panels::mcu_module::codegen::family::f1_async_module_gap(
            &self.family,
            self.runtime,
            kind,
        )
        .is_some()
        {
            return false;
        }
        let (required, _optional) = kind.signals();
        // STATIC, as the doc above says: what the silicon has, not what is
        // wired. Asked dynamically - which is what `pick_pins` answers - a kind
        // whose every candidate pad the user had spent on another function left
        // the palette entirely, with no row and no reason. That is the one thing
        // the palette promises never to do; a blocked kind stays visible and
        // `add_module_block_reason` says why.
        //
        // The optional signals are not consulted and cannot change the answer:
        // each is added only `if let Some(pin)`, so it can extend a wiring but
        // never prevent one (`an_optional_signal_cannot_make_a_wiring_fail`).
        autowire::any_wiring_static(self, &Default::default(), required)
    }

    /// Whether the chip still hosts an instance of `kind` that no module holds.
    ///
    /// The question that separates the two ways [`Self::can_add_module`] can say
    /// no. Both used to be reported as "every instance is already wired to a
    /// module", which on a chip with a free instance whose pads were spent named
    /// the wrong thing to go and fix.
    ///
    /// Asked STATICALLY, because "free" here is about the PERIPHERAL, not about
    /// its pads: an instance nothing holds is free even when every pad it could
    /// use carries something else.
    pub fn has_free_instance(&self, kind: crate::panels::mcu_module::modules::ModuleKind) -> bool {
        use crate::panels::mcu_module::modules::autowire;
        if kind.is_custom() {
            return true;
        }
        // A single-instance peripheral is free exactly when nothing holds it.
        // The instance LOOP cannot answer this one: `pin_function` ignores the
        // instance for such a kind, so every index from 1 up looks like another
        // copy of the same peripheral and the search would always find one.
        if kind.is_single_instance() {
            return !self.modules.iter().any(|m| m.kind == kind);
        }
        let (required, _optional) = kind.signals();
        let used_instances: std::collections::HashSet<u8> = self
            .modules
            .iter()
            .filter(|m| m.kind == kind)
            .map(|m| m.instance())
            .collect();
        autowire::any_wiring_static(self, &used_instances, required)
    }

    /// Could another `kind` be added RIGHT NOW — i.e. are there still free pins
    /// and a free instance? Dynamic: changes as modules/pins are edited. A kind
    /// that is [`supports_module`](Self::supports_module) but not this is
    /// *exhausted*, which the palette shows disabled-with-a-reason rather than
    /// hiding (a button that silently vanishes is worse than one that explains).
    pub fn can_add_module(&self, kind: crate::panels::mcu_module::modules::ModuleKind) -> bool {
        use crate::panels::mcu_module::modules::autowire;
        if kind.is_single_instance() && self.modules.iter().any(|m| m.kind == kind) {
            return false;
        }
        // Custom modules claim no peripheral, so you can always add another.
        if kind.is_custom() {
            return true;
        }
        let (required, _optional) = kind.signals();
        let used: std::collections::HashSet<usize> = self
            .modules
            .iter()
            .flat_map(|m| m.connections.iter().map(|c| c.mcu_pin))
            .collect();
        let used_instances: std::collections::HashSet<u8> = self
            .modules
            .iter()
            .filter(|m| m.kind == kind)
            .map(|m| m.instance())
            .collect();
        // `any_wiring`, not `pick_pins`: this only asks WHETHER, and the
        // ranking `pick_pins` does to answer WHICH is the whole cost. The
        // palette asks this for every entry on every frame its menu is open -
        // 119 ms of one on an ESP32-S3, now 0.35 ms.
        //
        // The optional signals are not consulted, and cannot change the answer:
        // each is added only `if let Some(pin)`, so it can extend a wiring but
        // never prevent one (`an_optional_signal_cannot_make_a_wiring_fail`).
        autowire::any_wiring(self, &used, &used_instances, required)
    }

    /// Add a virtual module (_USART / _SPI / _I2C) and auto-wire it to
    /// compatible MCU pins, setting those pins' functions. Returns `false` (and
    /// adds nothing) when the chip has no free pins for the module's interface.
    pub fn add_module(&mut self, kind: crate::panels::mcu_module::modules::ModuleKind) -> bool {
        use crate::panels::mcu_module::modules::autowire;

        // CAN and USB are single-instance and their pin functions carry no
        // index, so the instance-exclusion guard below can't stop a 2nd module
        // from grabbing the alternate pins — refuse it here.
        if kind.is_single_instance() && self.modules.iter().any(|m| m.kind == kind) {
            return false;
        }

        // A CUSTOM module wires nothing: it is created empty and the user adds
        // pins in its config panel, so the auto-wiring path below doesn't apply.
        if kind.is_custom() {
            use crate::panels::mcu_module::modules::VirtualModule;
            // Past every live Custom AND every Custom key that still holds
            // notes. Notes are keyed by (kind, instance), so reusing the number
            // of a removed Custom would hand the new, empty module the old one's
            // notes. Undoing the Remove restores the same instance, so those
            // notes still come back where they belong.
            let inst = (self
                .modules
                .iter()
                .filter(|m| m.kind.is_custom())
                .map(|m| m.instance())
                .chain(
                    self.module_notes
                        .iter()
                        .filter(|((k, _), n)| k.is_custom() && !n.is_empty())
                        .map(|((_, i), _)| *i),
                )
                .max()
                .unwrap_or(0))
                + 1;
            let id = self.free_module_id("custom");
            self.modules.push(VirtualModule {
                id,
                kind,
                name: format!("Custom{inst}"),
                pos: (0.0, 0.0),
                config: kind.default_config(inst),
                connections: Vec::new(),
            });
            return true;
        }

        let (required, optional) = kind.signals();

        // Pins already wired to an existing module are off-limits.
        let used: std::collections::HashSet<usize> = self
            .modules
            .iter()
            .flat_map(|m| m.connections.iter().map(|c| c.mcu_pin))
            .collect();
        // Peripheral instances already hosting a module of THIS kind are off-limits
        // too — so a 2nd "+_SPI" advances to SPI2 instead of re-picking SPI1 on
        // its alternate pin set (which `reconcile_modules` would then merge in).
        let used_instances: std::collections::HashSet<u8> = self
            .modules
            .iter()
            .filter(|m| m.kind == kind)
            .map(|m| m.instance())
            .collect();

        let Some((inst, chosen)) =
            autowire::pick_pins(self, &used, &used_instances, required, optional)
        else {
            return false;
        };

        self.add_module_wired(inst, &chosen)
    }

    /// Commit a wiring the caller already decided on.
    ///
    /// The tail of [`add_module`](Self::add_module), split out so the two ways
    /// of adding a module - autowire's pick and the user's own choice in the
    /// palette dialog - end at exactly the same three lines. A second copy is
    /// how the two would drift into disagreeing about what a module IS.
    ///
    /// The module itself is created (with its default config) by
    /// `reconcile_modules`, the single source of truth mirroring pin
    /// assignments; nothing here builds a `VirtualModule`, because a
    /// hand-built one would be duplicated on the next reconcile.
    ///
    /// The whole set is written BEFORE the reconcile, and `apply_pin_function`
    /// is deliberately not used: it would run `auto_assign_partners` per pad
    /// and move the pins the caller just chose.
    /// Returns `false` and changes NOTHING when the wiring is not one the chip
    /// can form - every pad must be able to carry its signal AND be free (or
    /// already carrying exactly it). Checked whole, before anything is written,
    /// so a half-applied wiring cannot exist.
    ///
    /// The dialog enumerates from `autowire::eligible` and so cannot normally
    /// produce a bad set - but it holds its choice across frames, and the chip
    /// moves underneath it: a pad free when the dialog opened can be taken by
    /// the time it is confirmed. Without this the confirm would overwrite that
    /// pad, and whichever module owned it would lose a connection - or, if it
    /// owned only that one, be dropped by `reconcile_modules` with its config.
    pub fn add_module_wired(
        &mut self,
        inst: u8,
        chosen: &[(crate::panels::mcu_module::modules::ModuleSignal, usize)],
    ) -> bool {
        if chosen.is_empty() {
            return false;
        }
        // The peripheral itself has to be free, not just its pads.
        //
        // Same doctrine as the pad check below, for the same reason: this is
        // public and the "Choose pins..." dialog holds its instance across
        // frames while the palette behind it keeps adding modules. Writing the
        // pads anyway would succeed at the pin level and then
        // `reconcile_modules` would fold them into the module that already
        // holds the instance - no new module appears, and the user's existing
        // CAN1 quietly grows a second TX and a second RX.
        //
        // Asked through `module_signal_of`, which is the mapping
        // `reconcile_modules` itself uses, so this predicts exactly what it
        // would do. That matters for CAN/USB/TOUCH and the other indexless
        // kinds, where the caller's `inst` and the model's are two different
        // numbers.
        if let Some((kind, model_inst, _)) = chosen.first().and_then(|(sig, _)| {
            crate::panels::mcu_module::modules::module_signal_of(&sig.pin_function(inst))
        }) && self
            .modules
            .iter()
            .any(|m| m.kind == kind && m.instance() == model_inst)
        {
            return false;
        }
        let formable = chosen.iter().all(|(sig, pin)| {
            let want = sig.pin_function(inst);
            self.find_pin(*pin).is_some_and(|p| {
                !p.reserved
                    && p.available_functions.contains(&want)
                    && (p.selected_function == PinFunction::Unset || p.selected_function == want)
            })
        });
        if !formable {
            return false;
        }
        for (sig, pin) in chosen {
            if let Some(p) = self.find_pin_mut(*pin) {
                p.selected_function = sig.pin_function(inst);
            }
        }
        self.reconcile_modules();
        true
    }

    /// The group a pad belongs to, if any.
    ///
    /// A pad is in at most one: `join_group` takes it out of whatever held it
    /// first, because "this pad is part of the radar AND part of the display"
    /// is not a thing a schematic can show, and a pad with two accent colours
    /// would just look broken.
    /// Only a LIVE group answers — the same predicate persistence, the generated
    /// comment and the canvas mats use. A row the roster is still filling in has
    /// no name yet, and a pad tick or a box bar for it would mark a device that
    /// exists nowhere else.
    pub fn group_of_pin(
        &self,
        pin: usize,
    ) -> Option<&crate::panels::mcu_module::mcu_config::PinGroup> {
        self.groups
            .iter()
            .find(|g| g.is_live() && g.pins.contains(&pin))
    }

    /// The device the canvas is currently talking about.
    ///
    /// EXPLICIT first — a click on a device's tab says so outright — then
    /// DERIVED, so clicking any PART of a device already lights the whole device
    /// and there is nothing extra to learn. Trimmed, like `is_live`,
    /// `group_color` and `mcu.config`: two spellings of one name are one device
    /// everywhere else.
    ///
    /// The explicit answer is re-checked against a LIVE group every frame. The
    /// roster can rename or dissolve a device under us, and a stale name that
    /// merely suppressed the derivation would leave the canvas lighting nothing,
    /// forever, with no way to notice.
    pub fn active_device(&self) -> Option<&str> {
        if let Some(name) = self.selected_device.as_deref()
            && let Some(g) = self
                .groups
                .iter()
                .find(|g| g.is_live() && g.name.trim() == name.trim())
        {
            return Some(g.name.trim());
        }
        // A device of an I2C bus picked on the canvas speaks for its OWN
        // Device, which need not be its bus's.
        if let Some((id, key)) = self.selected_i2c_child()
            && let Some(m) = self.modules.iter().find(|m| m.id == id)
        {
            return self
                .group_of_i2c_device(m.instance(), key)
                .map(|g| g.name.trim());
        }
        if let Some(id) = self.selected_module.as_deref()
            && let Some(m) = self.modules.iter().find(|m| m.id == id)
            && let Some(g) = self.group_of_module(m)
        {
            return Some(g.name.trim());
        }
        self.selected_pin
            .and_then(|p| self.group_of_pin(p))
            .map(|g| g.name.trim())
    }

    /// The list folded `id`'s config away, so the canvas stops calling its box
    /// out.
    ///
    /// To the user the two are ONE state: a box is drawn white BECAUSE its
    /// config is showing, and the list is the other place that showing can end.
    /// Left set, the diagram kept a box picked out with nothing open to say why,
    /// and the only way back was to click the box twice.
    ///
    /// Only the module named — folding one config says nothing about another.
    pub fn config_collapsed(&mut self, id: &str) {
        if self.selected_module.as_deref() == Some(id) {
            self.selected_module = None;
        }
    }

    /// The list folded EVERY config away at once.
    ///
    /// [`Self::config_collapsed`] for all of them, and it has to do the same two
    /// things for the same reason: a box is drawn white BECAUSE its config is
    /// showing, so closing the configs has to put the selection out with them.
    ///
    /// A function rather than the one assignment it started as, because
    /// `collapse_modules` alone is only HALF of what the canvas means by this.
    /// The panel's expand caret set that half and left the other, so reopening
    /// the panel came back with every config folded and a box on the diagram
    /// still picked out in white, with nothing open to say why - and the only
    /// way back was to click that box twice.
    pub fn all_configs_collapsed(&mut self) {
        self.collapse_modules = true;
        self.selected_module = None;
        self.selected_i2c_child = None;
    }

    /// The I2C device picked on the canvas - `None` unless its bus is still the
    /// selected module and the device still exists. Checked here rather than
    /// cleared everywhere: a stale pick simply stops counting.
    pub fn selected_i2c_child(
        &self,
    ) -> Option<(&str, crate::panels::mcu_module::modules::I2cDeviceKey)> {
        use crate::panels::mcu_module::modules::ModuleConfig;
        let (id, key) = self.selected_i2c_child.as_ref()?;
        if self.selected_module.as_deref() != Some(id.as_str()) {
            return None;
        }
        let m = self.modules.iter().find(|m| &m.id == id)?;
        let ModuleConfig::I2c(c) = &m.config else {
            return None;
        };
        c.has(*key).then_some((id.as_str(), *key))
    }

    /// The one place the canvas drops what it is pointing at.
    ///
    /// One function and not three assignments, because there are now three
    /// selections and a fourth is plausible — a clearing site that forgets one
    /// leaves the canvas lit for something the user has stopped looking at.
    pub fn clear_canvas_selection(&mut self) {
        self.selected_pin = None;
        self.selected_device = None;
        self.all_configs_collapsed();
    }

    /// The group a MODULE belongs to: the one holding any of its pads.
    ///
    /// Derived rather than stored, which is what lets a group survive
    /// `reconcile_modules` deleting and re-creating the module under a new id.
    pub fn group_of_module(
        &self,
        m: &crate::panels::mcu_module::modules::VirtualModule,
    ) -> Option<&crate::panels::mcu_module::mcu_config::PinGroup> {
        m.connections
            .iter()
            .find_map(|c| self.group_of_pin(c.mcu_pin))
    }

    /// Whether any part of device `name` sits at a hand-placed position.
    ///
    /// What decides whether the tab offers "reset to auto" at all.
    pub fn device_is_manual(&self, name: &str) -> bool {
        let name = name.trim();
        let mine = |p: usize| self.group_of_pin(p).is_some_and(|g| g.name.trim() == name);
        self.modules
            .iter()
            .any(|m| m.pos != (0.0, 0.0) && m.connections.iter().any(|c| mine(c.mcu_pin)))
            || self.io_pin_pos.keys().any(|p| mine(*p))
            || self.i2c_child_pos.keys().any(|dev| {
                self.groups
                    .iter()
                    .any(|g| g.is_live() && g.name.trim() == name && g.i2c.contains(dev))
            })
    }

    /// Return every part of device `name` to auto-packing.
    ///
    /// The counterpart of the per-box "Reset field position" already on the
    /// canvas: a device moved as one has to be resettable as one, or the user is
    /// left hunting for every part they moved.
    pub fn reset_device_position(&mut self, name: &str) {
        let name = name.trim().to_owned();
        // Collected first: `group_of_pin` borrows the whole `Mcu`.
        let mine: Vec<usize> = self
            .groups
            .iter()
            .filter(|g| g.is_live() && g.name.trim() == name)
            .flat_map(|g| g.pins.iter().copied())
            .collect();
        let idx: Vec<usize> = self
            .modules
            .iter()
            .enumerate()
            .filter(|(_, m)| m.connections.iter().any(|c| mine.contains(&c.mcu_pin)))
            .map(|(i, _)| i)
            .collect();
        for i in idx {
            self.modules[i].pos = (0.0, 0.0);
        }
        for p in mine {
            self.io_pin_pos.remove(&p);
        }
        let devices: Vec<(u8, u32)> = self
            .groups
            .iter()
            .filter(|g| g.is_live() && g.name.trim() == name)
            .flat_map(|g| g.i2c.iter().copied())
            .collect();
        for d in devices {
            self.i2c_child_pos.remove(&d);
        }
    }

    /// Put `pin` in the group called `name`, creating it if it is new.
    ///
    /// An empty name is how a pad leaves its group - the same field does both,
    /// so there is no second gesture to find.
    pub fn join_group(&mut self, pin: usize, name: &str) {
        // A group that gave this pad up and has nothing left is finished. Only
        // THOSE are dropped: a device the user has just created and not filled
        // yet is empty too, and deleting it out from under them the moment they
        // group something else would be indistinguishable from a bug.
        let mut emptied: Vec<usize> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if g.pins.remove(&pin) && g.is_empty() {
                emptied.push(i);
            }
        }
        // STORED verbatim, so it matches a name the roster is still editing
        // (see `rename_group`) - but two names are the SAME NAME when they
        // differ only in padding. Storage and identity have to part company
        // here: `mcu.config` trims on the way out and `group_color` hashes the
        // trimmed name, so "radar " and "radar" would draw one colour, save as
        // one line, and come back after a reload as two devices with literally
        // the same name.
        if !name.trim().is_empty() {
            match self
                .groups
                .iter_mut()
                .find(|g| g.name.trim() == name.trim())
            {
                Some(g) => {
                    g.pins.insert(pin);
                }
                // Pushed, so the indices collected above stay valid.
                None => self
                    .groups
                    .push(crate::panels::mcu_module::mcu_config::PinGroup {
                        name: name.to_owned(),
                        pins: std::iter::once(pin).collect(),
                        ..Default::default()
                    }),
            }
        }
        for i in emptied.into_iter().rev() {
            // Still empty: moving a pad WITHIN its own group empties it here and
            // fills it again above, and that group must survive.
            if self.groups[i].is_empty() {
                self.groups.remove(i);
            }
        }
    }

    /// Start an empty device, named by the roster.
    ///
    /// It holds nothing yet, so it is not written to `mcu.config` and does not
    /// reach the generated comment - both skip empty groups. It exists to be
    /// filled in the next gesture.
    pub fn new_group(&mut self, name: String) {
        self.groups
            .push(crate::panels::mcu_module::mcu_config::PinGroup {
                name,
                pins: Default::default(),
                ..Default::default()
            });
    }

    /// Set group `idx`'s name without ever merging.
    ///
    /// What the roster calls while its field still has FOCUS. Committing a merge
    /// on every keystroke destroyed devices in passing: typing "disp" out to
    /// "display2" passes through "display", and if another device answered to
    /// that, the two were merged at that keystroke and the rest of the word
    /// landed on whatever row had shifted into the slot. A name is only a
    /// decision once the user leaves the field.
    ///
    /// Two devices may briefly share a name this way. Nothing is lost by it: the
    /// canvas draws them as one mat for those frames, and `rename_group` folds
    /// them together the moment the field is left.
    pub fn set_group_name(&mut self, idx: usize, name: &str) {
        if let Some(g) = self.groups.get_mut(idx) {
            g.name = name.to_owned();
        }
    }

    /// Rename group `idx`, merging onto a name already taken.
    ///
    /// Renaming onto a name already taken MERGES the two: `join_group` finds a
    /// group by name, so leaving duplicates behind would mean two rows on the
    /// roster, one colour between them, and only one of them ever receiving a
    /// pad. Merging is the reading that matches what the user typed.
    ///
    /// Called when the roster's field is LEFT, never while it is being typed in
    /// — see [`set_group_name`](Self::set_group_name).
    pub fn rename_group(&mut self, idx: usize, name: &str) {
        // Stored EXACTLY as typed. Trimming here made a space impossible to
        // type: the roster re-seeds its text field from the stored name every
        // frame, so "mw " came back as "mw" and the next keystroke produced
        // "mwr". Whitespace is normalised where it belongs - on the way into
        // `mcu.config` - and an all-whitespace name still counts as no name.
        let name = name.to_owned();
        if idx >= self.groups.len() {
            return;
        }
        // ANOTHER group answering to this name - found by scanning past `idx`
        // rather than by `position(..).filter(!= idx)`, which returns the FIRST
        // match and so answers "none" whenever `idx` is itself that first match.
        // Two rows can transiently share a name (a name being typed is stored
        // without merging), and that is exactly the case the merge is owed.
        //
        // Deliberately NOT skipped when the name is unchanged: the roster defers
        // every merge until the field is left, at which point the text has long
        // since stopped changing and this is the only call that will make it.
        let other = self
            .groups
            .iter()
            .enumerate()
            .find(|(k, g)| *k != idx && g.name.trim() == name.trim() && !name.trim().is_empty())
            .map(|(k, _)| k);
        match other {
            Some(other) => {
                let moved = std::mem::take(&mut self.groups[idx].pins);
                self.groups[other].pins.extend(moved);
                let moved = std::mem::take(&mut self.groups[idx].i2c);
                self.groups[other].i2c.extend(moved);
                self.groups.remove(idx);
            }
            None => {
                if self.groups[idx].name != name {
                    self.groups[idx].name = name;
                }
            }
        }
    }

    /// Put every pad of `m` in `name` at once - the gesture the panel offers on
    /// a module, since grouping "the UART" means its pads, not one of them.
    pub fn join_group_module(
        &mut self,
        m: &crate::panels::mcu_module::modules::VirtualModule,
        name: &str,
    ) {
        let pins: Vec<usize> = m.connections.iter().map(|c| c.mcu_pin).collect();
        for p in pins {
            self.join_group(p, name);
        }
    }

    /// Move one pin's function to ANOTHER pad, as a single operation.
    ///
    /// Deliberately NOT `apply_pin_function` twice, because neither order works:
    ///
    /// * clearing the old pad first runs `deselect_partners`, which takes the
    ///   whole bus with it - a USART TX drags its RX to `Unset`, the module
    ///   loses every connection, and `reconcile_modules` then hands the
    ///   re-created one a fresh `default_config`, so the baud rate the user set
    ///   is gone;
    /// * setting the new pad first leaves TWO pads carrying the same function,
    ///   and `reconcile_modules` does not de-duplicate by signal - the module
    ///   grows a second row for one wire.
    ///
    /// `auto_assign_partners` must not run here either: the partners are
    /// already placed, and re-picking them would move pads the user did not ask
    /// about. So the whole edit is one write-pair followed by one reconcile,
    /// which is also how [`add_module`](Self::add_module) commits.
    ///
    /// Returns `false` and changes nothing when the destination cannot carry
    /// the function - the caller offers only pads that can, but the check is
    /// here so the model cannot be driven into a state the panel forbids.
    pub fn move_pin_function(&mut self, from: usize, to: usize) -> bool {
        if from == to {
            return false;
        }
        let Some(func) = self.find_pin(from).map(|p| p.selected_function.clone()) else {
            return false;
        };
        if func == PinFunction::Unset {
            return false;
        }
        let reachable = self.find_pin(to).is_some_and(|p| {
            !p.reserved
                && p.available_functions.contains(&func)
                && (p.selected_function == PinFunction::Unset || p.selected_function == func)
        });
        if !reachable {
            return false;
        }
        // The label names the SIGNAL's binding, so it travels with it. Cleared
        // instead, the user loses the name they typed; left behind, it strands
        // on a pad that no longer carries the signal and reappears on whatever
        // is bound there next.
        let label = self
            .find_pin(from)
            .map(|p| p.custom_label.clone())
            .unwrap_or_default();
        if let Some(p) = self.find_pin_mut(from) {
            p.custom_label.clear();
            p.selected_function = PinFunction::Unset;
            // An armed edge belonged to the pad as an INPUT; the pad is now
            // unassigned, and a stale edge would arm a pin nothing drives.
            p.irq = None;
        }
        if let Some(p) = self.find_pin_mut(to) {
            p.selected_function = func;
            p.custom_label = label;
        }
        // A group is a set of PAD numbers, so a signal that changes pad drops
        // out of its device unless the set is rewritten here. This is the only
        // place in the app where a signal moves between pads, which is why it
        // is also the only place that has to know.
        //
        // The destination is not necessarily device-free: a pad keeps its
        // device when its function goes away (removing a module resets its pads
        // to `Unset` but leaves them grouped), and the move only requires the
        // pad to be function-free. So `to` is taken out of whatever held it
        // before it is handed `from`'s device - otherwise one pad sat in two
        // devices at once and `group_of_pin` answered by Vec order.
        if let Some(mine) = self.groups.iter().position(|g| g.pins.contains(&from)) {
            let mut emptied: Vec<usize> = Vec::new();
            for (k, g) in self.groups.iter_mut().enumerate() {
                if g.pins.remove(&to) && g.is_empty() && k != mine {
                    emptied.push(k);
                }
            }
            self.groups[mine].pins.remove(&from);
            self.groups[mine].pins.insert(to);
            // Only a device this move emptied disappears, the same rule
            // `join_group` follows.
            for k in emptied.into_iter().rev() {
                if self.groups[k].is_empty() {
                    self.groups.remove(k);
                }
            }
        }
        self.reconcile_modules();
        true
    }

    pub fn remove_module(&mut self, id: &str) {
        let pins: Vec<usize> = self
            .modules
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.connections.iter().map(|c| c.mcu_pin).collect())
            .unwrap_or_default();
        for pin in pins {
            if let Some(p) = self.find_pin_mut(pin) {
                p.selected_function = PinFunction::Unset;
                // Freeing a pad drops its user label - the same rule
                // `apply_pin_function` states on its `Unset` branch. This path
                // wrote the pin directly and skipped it, so a removed module's
                // pin name came back on the next binding for that pad.
                p.custom_label.clear();
            }
        }
        self.modules.retain(|m| m.id != id);
    }

    // ── Virtual-module undo (Ctrl+Z on the Pins tab) ──────────────────────────

    /// Cap on the module undo stack — a Ctrl+Z safety net, not full history.
    const MODULE_UNDO_CAP: usize = 30;

    /// Snapshot the modules + pin state BEFORE an explicit add/remove, so Ctrl+Z
    /// (or the Undo button) can revert it. `label` is shown on the Undo hover.
    pub fn push_module_undo(&mut self, label: String) {
        use crate::panels::mcu_module::mcu::model::ModuleUndo;
        let pins = self
            .iter_all_pins()
            .map(|p| {
                (
                    p.number,
                    p.selected_function.clone(),
                    p.custom_label.clone(),
                )
            })
            .collect();
        self.module_undo.push(ModuleUndo {
            modules: self.modules.clone(),
            pins,
            label,
        });
        if self.module_undo.len() > Self::MODULE_UNDO_CAP {
            self.module_undo.remove(0);
        }
    }

    /// Drop the most recent snapshot WITHOUT applying it — used when a snapshotted
    /// action turned out to be a no-op (e.g. an add that found no free pins).
    pub fn discard_last_module_undo(&mut self) {
        self.module_undo.pop();
    }

    /// Revert the last add/remove: restore its snapshot (pins + modules). Returns
    /// the undone action's label, or `None` when the stack is empty.
    pub fn undo_modules(&mut self) -> Option<String> {
        let snap = self.module_undo.pop()?;
        for (num, func, label) in &snap.pins {
            if let Some(p) = self.find_pin_mut(*num) {
                p.selected_function = func.clone();
                p.custom_label = label.clone();
            }
        }
        self.modules = snap.modules;
        self.module_remove_confirm = None;
        self.i2c_remove_confirm = None;
        Some(snap.label)
    }

    // ── I2C devices on a bus ──────────────────────────────────────────────────

    /// Apply one change to the device list of I2C bus `instance` - the ONE door
    /// both the panel and the canvas use. A real change is snapshotted for
    /// Ctrl+Z first; a no-op pushes nothing. Returns whether anything changed.
    /// The config of I2C bus `instance`, if the chip has that bus.
    pub fn i2c_bus(
        &self,
        instance: u8,
    ) -> Option<&crate::panels::mcu_module::modules::I2cModuleConfig> {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        self.modules.iter().find_map(|m| match &m.config {
            ModuleConfig::I2c(c)
                if m.kind == ModuleKind::GenericInterfaceI2c && m.instance() == instance =>
            {
                Some(c)
            }
            _ => None,
        })
    }

    /// Retire a device removal nobody can answer any more: its bus is gone, or
    /// a mint re-keyed the device. Left armed, the canvas would repaint every
    /// frame for it, and a later device that happened to get the same key
    /// would come up already asking to be removed.
    pub fn retire_i2c_confirm(&mut self) {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        if let Some((inst, key)) = self.i2c_remove_confirm {
            let alive = self.modules.iter().any(|m| {
                m.kind == ModuleKind::GenericInterfaceI2c
                    && m.instance() == inst
                    && matches!(&m.config, ModuleConfig::I2c(c) if c.has(key))
            });
            if !alive {
                self.i2c_remove_confirm = None;
            }
        }
    }

    pub fn edit_i2c_device(
        &mut self,
        instance: u8,
        edit: crate::panels::mcu_module::modules::I2cDeviceEdit,
    ) -> bool {
        use crate::panels::mcu_module::modules::{I2cDeviceEdit, ModuleConfig, ModuleKind};
        // Minted FIRST, undo history included: the snapshot pushed below must
        // carry the uids a Device may hold from now on, or undoing this edit
        // would take the device out of its Device (see `mint_i2c_bus`).
        let edit = self.pin_i2c_key(instance, edit);
        let floor = self.i2c_uid_floor(instance);
        let Some(pos) = self
            .modules
            .iter()
            .position(|m| m.kind == ModuleKind::GenericInterfaceI2c && m.instance() == instance)
        else {
            return false;
        };
        let ModuleConfig::I2c(cfg) = &self.modules[pos].config else {
            return false;
        };
        let mut next = cfg.clone();
        let label = match &edit {
            I2cDeviceEdit::Add => "Add I2C device".to_owned(),
            I2cDeviceEdit::Remove(k) => match cfg.device(*k) {
                Some((name, _)) if !name.is_empty() => format!("Remove I2C device {name}"),
                _ => "Remove I2C device".to_owned(),
            },
            I2cDeviceEdit::Name(..) => "Rename I2C device".to_owned(),
            I2cDeviceEdit::Address(..) => "Change I2C device address".to_owned(),
        };
        if !next.apply(&edit, floor) {
            return false;
        }
        self.push_module_undo(label);
        self.modules[pos].config = ModuleConfig::I2c(next);
        if let I2cDeviceEdit::Remove(k) = &edit {
            self.i2c_remove_confirm = None;
            // Its place goes with it - an undo brings it back into the column.
            if let crate::panels::mcu_module::modules::I2cDeviceKey::Uid(u) = k {
                self.i2c_child_pos.remove(&(instance, *u));
            }
        }
        true
    }

    /// Put device `key` of bus `instance` where the user dragged it (`Some`,
    /// the offset of its box's top-left from the chip centre), or back into
    /// its bus's column (`None`). A device with no uid yet is minted first - the
    /// place is kept by uid.
    pub fn move_i2c_device(
        &mut self,
        instance: u8,
        key: crate::panels::mcu_module::modules::I2cDeviceKey,
        at: Option<(f32, f32)>,
    ) {
        let uid = match (key, at) {
            (crate::panels::mcu_module::modules::I2cDeviceKey::Uid(u), _) => u,
            // Nothing to reset for a device never moved.
            (_, None) => return,
            (k, Some(_)) => match self.ensure_i2c_uid(instance, k) {
                Some(u) => u,
                None => return,
            },
        };
        match at {
            Some(p) => {
                self.i2c_child_pos.insert((instance, uid), p);
            }
            None => {
                self.i2c_child_pos.remove(&(instance, uid));
            }
        }
    }

    /// The highest device uid of bus `instance` that anything outside the bus's
    /// own config still points at - a Device holding it, maybe for a device
    /// removed since. New uids are minted above it (see
    /// `I2cModuleConfig::apply`), so a new device never walks into a Device
    /// that held an old one.
    ///
    /// And every uid the undo stack still holds for it: an undo can bring a
    /// removed device back, and a uid handed to a newer device meanwhile would
    /// give the old one the newer one's Device.
    pub fn i2c_uid_floor(&self, instance: u8) -> u32 {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        let held = self
            .groups
            .iter()
            .flat_map(|g| g.i2c.iter())
            .filter(|(i, _)| *i == instance)
            .map(|(_, u)| *u);
        let undoable = self
            .module_undo
            .iter()
            .flat_map(|snap| snap.modules.iter())
            .filter(|m| m.kind == ModuleKind::GenericInterfaceI2c && m.instance() == instance)
            .flat_map(|m| match &m.config {
                ModuleConfig::I2c(c) => c.devices.iter().map(|d| d.uid).collect(),
                _ => Vec::new(),
            });
        held.chain(undoable).max().unwrap_or(0)
    }

    /// Mint bus `instance` - its legacy address becomes a one-entry list and
    /// every device gets a uid - in the live config AND in every undo snapshot
    /// that holds the same, still unminted, bus. Returns whether it minted.
    ///
    /// The snapshots are the point. A Device holds a device by its uid, and a
    /// snapshot taken before the mint has none: undoing past the mint (a
    /// module added earlier, the device's own first rename) would restore a
    /// device no Device can name, and its membership would be gone for good.
    /// A snapshot can only hold the bus unminted while no device edit has run
    /// since it was loaded - every edit mints first - so it holds the same
    /// devices in the same order, and minting it with the same floor hands out
    /// the same uids. It is still checked, device by device.
    pub fn mint_i2c_bus(&mut self, instance: u8) -> bool {
        use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind};
        let floor = self.i2c_uid_floor(instance);
        let is_bus = |m: &crate::panels::mcu_module::modules::VirtualModule| {
            m.kind == ModuleKind::GenericInterfaceI2c && m.instance() == instance
        };
        let Some(ModuleConfig::I2c(cfg)) = self
            .modules
            .iter_mut()
            .find(|m| is_bus(m))
            .map(|m| &mut m.config)
        else {
            return false;
        };
        let before = (cfg.address, cfg.devices.clone());
        if !cfg.mint(floor) {
            return false;
        }
        let minted = cfg.clone();
        for snap in &mut self.module_undo {
            for m in snap.modules.iter_mut().filter(|m| is_bus(m)) {
                if let ModuleConfig::I2c(c) = &mut m.config
                    && (c.address, &c.devices) == (before.0, &before.1)
                {
                    c.devices = minted.devices.clone();
                    c.address = minted.address;
                }
            }
        }
        true
    }

    /// `edit` with a key that survives minting: an `Implicit` or `Unminted`
    /// key names a POSITION, which the mint keeps but renames - so the bus is
    /// minted here and the key rewritten to the uid now at that position.
    /// Anything that queues several edits (the panel and the canvas in one
    /// frame) runs each through this, or the second one would name a key the
    /// first one's mint retired.
    pub fn pin_i2c_key(
        &mut self,
        instance: u8,
        edit: crate::panels::mcu_module::modules::I2cDeviceEdit,
    ) -> crate::panels::mcu_module::modules::I2cDeviceEdit {
        use crate::panels::mcu_module::modules::I2cDeviceEdit as E;
        let key = match &edit {
            E::Add => return edit,
            E::Remove(k) | E::Name(k, _) | E::Address(k, _) => *k,
        };
        match self.ensure_i2c_uid(instance, key) {
            Some(uid) => {
                let k = crate::panels::mcu_module::modules::I2cDeviceKey::Uid(uid);
                match edit {
                    E::Remove(_) => E::Remove(k),
                    E::Name(_, n) => E::Name(k, n),
                    E::Address(_, a) => E::Address(k, a),
                    E::Add => E::Add,
                }
            }
            None => edit,
        }
    }

    /// The uid of device `key` on bus `instance`, minting the bus first if it
    /// has none yet (a legacy address, a file from before uids) - what a Device
    /// has to hold a device by. The mint changes nothing the device generates,
    /// so it is not an undo step of its own.
    pub fn ensure_i2c_uid(
        &mut self,
        instance: u8,
        key: crate::panels::mcu_module::modules::I2cDeviceKey,
    ) -> Option<u32> {
        use crate::panels::mcu_module::modules::{I2cDeviceKey, ModuleConfig, ModuleKind};
        let bus = |mcu: &Self| {
            mcu.modules.iter().find_map(|m| match &m.config {
                ModuleConfig::I2c(c)
                    if m.kind == ModuleKind::GenericInterfaceI2c && m.instance() == instance =>
                {
                    Some(c.clone())
                }
                _ => None,
            })
        };
        let cfg = bus(self)?;
        if !cfg.has(key) {
            return None;
        }
        if let I2cDeviceKey::Uid(u) = key {
            return Some(u);
        }
        // Where the key points, before minting renames it.
        let at = cfg.position(key).unwrap_or(0);
        self.mint_i2c_bus(instance);
        bus(self)?.devices.get(at).map(|d| d.uid)
    }

    /// The Device an I2C device is in: the one it was put in, and none until it
    /// is put in one.
    ///
    /// Not its bus's. A device used to follow the Device its bus's pads were
    /// in, which drew it inside that Device's mat and made it read as part of
    /// the bus box: a display on the bus of a "control panel" was part of the
    /// control panel whether or not it belonged there. Each device of a bus is
    /// a board part of its own, grouped on its own.
    pub fn group_of_i2c_device(
        &self,
        instance: u8,
        key: crate::panels::mcu_module::modules::I2cDeviceKey,
    ) -> Option<&crate::panels::mcu_module::mcu_config::PinGroup> {
        use crate::panels::mcu_module::modules::I2cDeviceKey;
        let I2cDeviceKey::Uid(u) = key else {
            return None;
        };
        self.groups
            .iter()
            .find(|g| g.is_live() && g.i2c.contains(&(instance, u)))
    }

    /// Put I2C device `key` of bus `instance` in the Device called `name`,
    /// creating it if it is new - or, with an empty name, take it out of the
    /// one it was put in, so it is in none. The same rules as
    /// [`Self::join_group`]: one Device at a time, and a Device this took its
    /// last member from is finished. Returns whether anything changed.
    pub fn join_group_i2c(
        &mut self,
        instance: u8,
        key: crate::panels::mcu_module::modules::I2cDeviceKey,
        name: &str,
    ) -> bool {
        let Some(uid) = self.ensure_i2c_uid(instance, key) else {
            return false;
        };
        let dev = (instance, uid);
        let before = self.groups.clone();
        let mut emptied: Vec<usize> = Vec::new();
        for (i, g) in self.groups.iter_mut().enumerate() {
            if g.i2c.remove(&dev) && g.is_empty() {
                emptied.push(i);
            }
        }
        if !name.trim().is_empty() {
            match self
                .groups
                .iter_mut()
                .find(|g| g.name.trim() == name.trim())
            {
                Some(g) => {
                    g.i2c.insert(dev);
                }
                None => self
                    .groups
                    .push(crate::panels::mcu_module::mcu_config::PinGroup {
                        name: name.to_owned(),
                        i2c: std::iter::once(dev).collect(),
                        ..Default::default()
                    }),
            }
        }
        for i in emptied.into_iter().rev() {
            if self.groups[i].is_empty() {
                self.groups.remove(i);
            }
        }
        self.groups != before
    }

    pub fn can_undo_modules(&self) -> bool {
        !self.module_undo.is_empty()
    }

    pub fn last_module_undo_label(&self) -> Option<&str> {
        self.module_undo.last().map(|u| u.label.as_str())
    }

    /// Returns `(number, name, selected_function)` for every non-reserved pin.
    /// Used by the IDE to sync the `pins/` source-file directory.
    pub fn all_pin_functions(&self) -> Vec<(usize, String, PinFunction)> {
        self.iter_all_pins()
            .filter(|p| !p.reserved)
            .map(|p| (p.number, p.name.clone(), p.selected_function.clone()))
            .collect()
    }

    /// Restores pin assignments parsed from `src/main.rs` by
    /// `codegen::parse_main_rs()`.
    ///
    /// - Resets all pins to `Unset` first (clean slate).
    /// - Sets each named pin to the given `PinFunction`.
    /// - Pins not found in this MCU layout (wrong name) are silently skipped.
    /// - Reserved pins are never overwritten.
    /// - Does NOT trigger auto-partner assignment — the saved state already
    ///   contains every pin individually.
    pub fn apply_saved_pins(&mut self, pins: &[(String, PinFunction)]) {
        self.reset_all_pins();
        for (name, func) in pins {
            let num = self
                .iter_all_pins()
                .find(|p| p.name == *name && !p.reserved)
                .map(|p| p.number);
            if let Some(num) = num {
                if let Some(pin) = self.find_pin_mut(num) {
                    pin.selected_function = func.clone();
                }
            }
        }
    }

    /// Restores pin assignments from the `@pins` section of `mcu.config`
    /// (`mcu_config::parse_pins`) - the store for a family whose generated
    /// code carries no label `parse_main_rs` could read back.
    ///
    /// The contract of [`Self::apply_saved_pins`], keyed by pin number:
    ///
    /// - Resets all pins to `Unset` first (clean slate).
    /// - A number this layout does not have (a project opened against a
    ///   re-imported definition) is silently skipped.
    /// - Reserved pins are never overwritten.
    /// - Does NOT trigger auto-partner assignment.
    pub fn apply_saved_pins_by_number(&mut self, pins: &[(usize, PinFunction)]) {
        self.reset_all_pins();
        for (num, func) in pins {
            if let Some(pin) = self.find_pin_mut(*num)
                && !pin.reserved
            {
                pin.selected_function = func.clone();
            }
        }
    }

    /// Restores the per-pin user labels parsed from a saved `src/main.rs` by
    /// `codegen::parse_pin_labels()` (the `_<label>` suffix on a binding name).
    /// Apply this *after* [`apply_saved_pins`], since clearing a pin to `Unset`
    /// drops its label. Pins not in this layout (wrong name) are skipped.
    pub fn apply_saved_pin_labels(&mut self, labels: &[(String, String)]) {
        for (name, label) in labels {
            let num = self
                .iter_all_pins()
                .find(|p| p.name == *name && !p.reserved)
                .map(|p| p.number);
            if let Some(num) = num {
                if let Some(pin) = self.find_pin_mut(num) {
                    pin.custom_label = label.clone();
                }
            }
        }
    }

    /// The `mcu.config` text for this chip — its virtual modules (`@modules`)
    /// and, for the STM32F1 family, the clock-tree config (`@clock`). Written to
    /// the project root on save; empty when there is nothing to persist.
    pub fn mcu_config_text(&self) -> String {
        use crate::panels::mcu_module::clock::graph::graph_to_stm32f1;
        use crate::panels::mcu_module::clock::{ClockConfig, Stm32f1Clock};
        use crate::panels::mcu_module::mcu_config;
        let clock = if self.family == "stm32f1" {
            Some(match &self.clock {
                ClockConfig::Graph(gc) => graph_to_stm32f1(&gc.for_codegen()),
                _ => Stm32f1Clock::default(),
            })
        } else {
            None
        };
        let mut s =
            mcu_config::serialize(&self.modules, clock.as_ref(), self.runtime, self.gpio_api);
        // The clock tree's own state, for EVERY family. `@clock` above is
        // written only for stm32f1 (it speaks `Stm32f1Clock`), which is why a
        // retuned H5 or WBA used to come back at the chip's defaults on reopen:
        // nothing had recorded that the project changed it.
        let nodes = match &self.clock {
            ClockConfig::Graph(gc) => crate::panels::mcu_module::clock::persist::nodes_to_block(
                &gc.graph,
                self.clock_defaults.as_ref(),
            ),
            ClockConfig::None => String::new(),
        };
        let nodes = mcu_config::clock_nodes_section(&nodes);
        if !nodes.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&nodes);
        }
        // Auto-build preference lives in its own `@autobuild` section (workflow
        // setting, not codegen config), appended here.
        let ab = mcu_config::autobuild_section(self.auto_build);
        if !ab.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&ab);
        }
        // Strict-lints preference (`@strict`) — workflow setting like @autobuild.
        let strict = mcu_config::strict_section(self.strict_lints);
        if !strict.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&strict);
        }
        // Debug-friendly release profile (`@debugbuild`) — workflow setting.
        let debug_build = mcu_config::debug_build_section(self.debug_build);
        if !debug_build.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&debug_build);
        }
        // Hand-written clock (`@clockmanual`) — this one is NOT a view
        // preference: it decides whether the generated clock block is replaced
        // or preserved, so it has to travel with the project's config.
        let clock_manual = mcu_config::clock_manual_section(self.clock_manual);
        if !clock_manual.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&clock_manual);
        }
        // Watchdogs (`@watchdog`) — codegen input, like `@clockmanual`: it
        // decides whether the watchdog config files exist at all.
        let wdg = mcu_config::watchdog_section(&self.watchdog);
        let comp = mcu_config::comp_section(&self.comp);
        if !wdg.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&wdg);
        }
        // Comparators (`@comp`) — codegen input for the same reason.
        if !comp.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&comp);
        }
        // The flash store (`@flashstore`) — codegen input too.
        let flash_store = mcu_config::flashstore_section(self.flash_store.as_ref());
        if !flash_store.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&flash_store);
        }
        // The IoT tab (`@iot`) - codegen input, and no secret in it.
        let iot = mcu_config::iot_section(&self.iot);
        if !iot.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&iot);
        }
        // Diagram rotation (`@rotation`) — view preference, same append pattern.
        let rotation = mcu_config::rotation_section(self.rotated);
        if !rotation.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&rotation);
        }
        // Manual in/out field positions (`@iopins`) — view preference.
        let groups = mcu_config::groups_section(&self.groups);
        if !groups.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&groups);
        }
        let iopins = mcu_config::iopins_section(&self.io_pin_pos);
        if !iopins.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&iopins);
        }
        // Dragged I2C device boxes (`@i2cpos`) - view preference, and only the
        // ones whose device is still there.
        let live: std::collections::BTreeMap<(u8, u32), (f32, f32)> = self
            .i2c_child_pos
            .iter()
            .filter(|((inst, uid), _)| {
                self.i2c_bus(*inst).is_some_and(|c| {
                    c.has(crate::panels::mcu_module::modules::I2cDeviceKey::Uid(*uid))
                })
            })
            .map(|(k, v)| (*k, *v))
            .collect();
        let i2cpos = mcu_config::i2c_pos_section(&live);
        if !i2cpos.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&i2cpos);
        }
        // Interrupt edges (`@irq`). Unlike the two sections above this is NOT a
        // view preference: it changes the generated code on the RTIC runtime.
        let irqs: std::collections::BTreeMap<usize, _> = self
            .iter_all_pins()
            .filter_map(|p| p.irq.map(|e| (p.number, (e, p.irq_priority))))
            .collect();
        let irq = mcu_config::irq_section(&irqs);
        if !irq.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&irq);
        }
        // GPIO drive/pull modes (`@iomode`) — also CODE, not a view preference:
        // it picks which `into_*` / `Pull::*` the binding is generated with.
        let modes: std::collections::BTreeMap<usize, _> = self
            .iter_all_pins()
            .filter_map(|p| p.io_mode.map(|m| (p.number, m)))
            .collect();
        let iomode = mcu_config::iomode_section(&modes);
        if !iomode.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&iomode);
        }
        // Per-pin user labels (`@labels`). The label used to have no store of
        // its own - it was recovered from the `_<label>` suffix on the generated
        // binding, which is a Rust identifier, so "Status LED" came back
        // "status_led" and a pin with no binding kept nothing at all.
        let labels: std::collections::BTreeMap<usize, String> = self
            .iter_all_pins()
            .filter(|p| !p.custom_label.trim().is_empty())
            .map(|p| (p.number, p.custom_label.clone()))
            .collect();
        let labels = mcu_config::labels_section(&labels);
        if !labels.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&labels);
        }
        // Virtual Module notes (`@modulenotes`). Written here like any other
        // section, but NOT codegen input - see `Mcu::module_notes`.
        let notes = mcu_config::notes_section(&self.module_notes);
        if !notes.is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&notes);
        }
        // Pin functions (`@pins`) - CODE, and the whole of it: every binding
        // in the generated block comes from these. Written for nRF and RP.
        // Their backends leave no `// label` on a binding for `parse_main_rs`
        // to recover the function from, so without this their projects came
        // back with an empty diagram on every open - on the pico2-ice that
        // also silently dropped the FPGA loader, which is a pad function too.
        // Every other family still reads its pins out of main.rs, and a
        // section it never writes cannot get in the way of that.
        if crate::panels::mcu_module::codegen::nrf::is_nrf(&self.family)
            || crate::panels::mcu_module::codegen::rp::is_rp(&self.family)
        {
            // `pins_section` leaves the Unset pads out.
            let pins: std::collections::BTreeMap<usize, PinFunction> = self
                .iter_all_pins()
                .filter(|p| !p.reserved)
                .map(|p| (p.number, p.selected_function.clone()))
                .collect();
            let pins = mcu_config::pins_section(&pins);
            if !pins.is_empty() {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&pins);
            }
        }
        s
    }

    /// Restore the Virtual Module notes from `@modulenotes`, REPLACING whatever
    /// the map held.
    ///
    /// It assigns rather than merges, and it is called on every project open
    /// with or without a `mcu.config`: a project with no notes must come up
    /// with none, not with the previous project's. The module list does not
    /// work that way - it is replaced only by a non-empty `@modules`, which is
    /// how a same-chip Open can keep the last project's Custom modules - and
    /// the notes deliberately do not share that.
    pub fn restore_module_notes(&mut self, text: Option<&str>) {
        self.module_notes = text
            .map(crate::panels::mcu_module::mcu_config::parse_notes)
            .unwrap_or_default();
    }

    /// Restore the per-pin user labels from `@labels`.
    ///
    /// Deliberately NOT part of [`Self::apply_mcu_config`], which runs before
    /// `apply_saved_pins` so the restored clock can drive the regenerated
    /// chain, and `apply_saved_pins` opens with `reset_all_pins`, which clears
    /// every label. This has to land after that, and after the lossy binding-derived
    /// labels it supersedes, so it is its own call at its own point in the
    /// sequence.
    ///
    /// A pin the section names but this chip does not have is skipped, the same
    /// as every other restore here.
    pub fn apply_config_pin_labels(&mut self, text: &str) {
        let labels = crate::panels::mcu_module::mcu_config::parse_labels(text);
        for (num, label) in labels {
            if let Some(pin) = self.find_pin_mut(num)
                && !pin.reserved
            {
                pin.custom_label = label;
            }
        }
    }

    /// Restore virtual modules + clock from an `mcu.config` file on project open.
    /// Apply *after* `apply_saved_pins` (which derives default modules from the
    /// pins) so the saved per-module config (labels, baud, …) wins.
    pub fn apply_mcu_config(&mut self, text: &str) {
        use crate::panels::mcu_module::mcu_config;
        let (modules, clock) = mcu_config::parse(text);
        if !modules.is_empty() {
            self.modules = modules;
        }
        if let Some(c) = clock {
            self.apply_saved_clock(c);
        }
        // Applied AFTER the legacy `@clock`: on an F1 both describe the same
        // tree, and this one is the authority — it records the graph itself
        // rather than the fields the F1 struct happens to have.
        if let Some(body) = mcu_config::parse_clock_nodes(text) {
            self.apply_saved_clock_nodes(&body);
        }
        // Runtime lives in its own `@runtime` section; a missing one (any
        // pre-async project) restores the default Blocking.
        self.runtime = mcu_config::parse_runtime(text);
        // GPIO api (`@gpio`) — missing restores the default Portable (io.rs bridge).
        self.gpio_api = mcu_config::parse_gpio_api(text);
        // Auto-build preference (`@autobuild`) — missing restores the default Check.
        self.auto_build = mcu_config::parse_autobuild(text);
        // Strict-lints preference (`@strict`) — missing restores the default OFF.
        self.strict_lints = mcu_config::parse_strict(text);
        // Debug-friendly release profile (`@debugbuild`) — missing = OFF.
        self.debug_build = mcu_config::parse_debug_build(text);
        // Diagram rotation (`@rotation`) — missing restores the default (0°).
        self.rotated = mcu_config::parse_rotation(text);
        // Hand-written clock (`@clockmanual`). A MISSING section keeps whatever
        // the chip's family decided (manual when it has no RCC recipe), so a
        // project saved before this existed still gets the right behaviour.
        if let Some(manual) = mcu_config::parse_clock_manual(text) {
            self.clock_manual = manual;
        }
        // Manual in/out field positions (`@iopins`) — missing = all auto-placed.
        //
        // `@labels` is NOT read here: this runs before `apply_saved_pins`, whose
        // `reset_all_pins` would clear every label it restored. It has its own
        // entry point, `apply_config_pin_labels`, called later in the sequence.
        self.io_pin_pos = mcu_config::parse_iopins(text);
        self.i2c_child_pos = mcu_config::parse_i2c_pos(text);
        self.groups = mcu_config::parse_groups(text);
        self.watchdog = mcu_config::parse_watchdog(text);
        self.comp = mcu_config::parse_comp(text);
        self.flash_store = mcu_config::parse_flashstore(text);
        self.iot = mcu_config::parse_iot(text);
        // Interrupt edges (`@irq`) — a missing section means every input is
        // polled, which is the pre-RTIC behaviour of every existing project.
        let irqs = mcu_config::parse_irq(text);
        // GPIO modes (`@iomode`) — a missing section means every pin is on the
        // backend default (floating in / push-pull out), i.e. what every project
        // generated before the mode was selectable.
        let modes = mcu_config::parse_iomode(text);
        for pin in self.iter_all_pins_mut() {
            // The priority rides with the edge; a pin the file does not arm
            // keeps the default, so an older project loads exactly as before.
            let armed = irqs.get(&pin.number).copied();
            pin.irq = armed.map(|(e, _)| e);
            pin.irq_priority = armed.map(|(_, p)| p).unwrap_or_default();
            pin.io_mode = modes.get(&pin.number).copied();
        }
        // A freshly loaded project has NO staged edits: pending == applied.
        self.sync_pending_style();
    }

    // ── Staged codegen-style choices (System-tab "Apply") ─────────────────────

    /// Reset the staged (`pending_*`) choices to the currently APPLIED ones — so
    /// nothing shows as dirty. Called on project load and right after an Apply.
    pub fn sync_pending_style(&mut self) {
        self.pending_runtime = self.runtime;
        self.pending_gpio_api = self.gpio_api;
        self.pending_module_styles = self
            .modules
            .iter()
            .map(|m| (m.id.clone(), module_style(&m.config)))
            .collect();
        self.pending_apply_confirm = false;
    }

    /// Commit the staged choices into the applied fields — the runtime, GPIO api
    /// and each module's `api_style`/`async_mode`. This changes the state hash, so
    /// the next `init_frame` regenerates `main.rs`, the config files and the deps.
    pub fn apply_pending_style(&mut self) {
        self.runtime = self.pending_runtime;
        self.gpio_api = self.pending_gpio_api;
        let pending = std::mem::take(&mut self.pending_module_styles);
        for m in &mut self.modules {
            if let Some(&(api, async_mode)) = pending.get(&m.id) {
                set_module_style(&mut m.config, api, async_mode);
            }
        }
        self.pending_module_styles = pending;
        self.pending_apply_confirm = false;
        // The runtime / api-style just changed → the config templates (whole
        // `init()`, not just the consts) must be regenerated in full next frame.
        self.config_regen_forced = true;
    }

    /// Whether any staged choice differs from the applied one (drives the Apply
    /// button's enabled state).
    pub fn style_dirty(&self) -> bool {
        if self.pending_runtime != self.runtime || self.pending_gpio_api != self.gpio_api {
            return true;
        }
        self.modules.iter().any(|m| {
            self.pending_module_styles
                .get(&m.id)
                .is_some_and(|&staged| staged != module_style(&m.config))
        })
    }

    /// Whether the staged-changes bar should be drawn — and the one place the
    /// question it asks is retired.
    ///
    /// A pending confirm only means anything while the bar is there to show it,
    /// and this is the ONLY path on which the bar goes away. The three places
    /// that used to clear the flag are all buttons INSIDE the bar (Discard,
    /// Confirm & apply, Cancel), so none of them is on it: undoing the staged
    /// change by hand — setting an Init API back to what it was un-dirties the
    /// state — hid the bar with the confirm still armed, and the next staged
    /// change brought it back already asking "Apply will regenerate … Confirm &
    /// apply?" about something the user had just typed and never submitted.
    pub fn apply_bar_visible(&mut self) -> bool {
        let dirty = self.style_dirty();
        if !dirty {
            self.pending_apply_confirm = false;
        }
        dirty
    }

    /// Human-readable lines of what an Apply would change (for the confirm
    /// prompt). Empty when nothing is staged.
    pub fn style_diff_summary(&self) -> Vec<String> {
        use crate::panels::mcu_module::modules::AsyncBusMode;
        let mut out = Vec::new();
        if self.pending_runtime != self.runtime {
            out.push(format!(
                "Runtime: {} -> {}",
                self.runtime.as_token(),
                self.pending_runtime.as_token()
            ));
        }
        if self.pending_gpio_api != self.gpio_api {
            out.push(format!(
                "GPIO In/Out: {:?} -> {:?}",
                self.gpio_api, self.pending_gpio_api
            ));
        }
        for m in &self.modules {
            let Some(&(api, asyncm)) = self.pending_module_styles.get(&m.id) else {
                continue;
            };
            let (cur_api, cur_async) = module_style(&m.config);
            let name = crate::panels::mcu_module::mcu::gui::modules::module_base_name(m);
            if api != cur_api {
                out.push(format!("{name} init: {cur_api:?} -> {api:?}"));
            }
            // Neither serial config HAS an `async_mode` - `module_style` reports
            // a constant `Blocking` for both - so there is no such change to
            // describe.
            if asyncm != cur_async
                && !matches!(
                    m.config,
                    crate::panels::mcu_module::modules::ModuleConfig::Usart(_)
                        | crate::panels::mcu_module::modules::ModuleConfig::Lpuart(_)
                )
            {
                let lbl = |x: AsyncBusMode| match x {
                    AsyncBusMode::Blocking => "Blocking",
                    AsyncBusMode::AsyncDma => "Async-DMA",
                };
                out.push(format!(
                    "{name} async: {} -> {}",
                    lbl(cur_async),
                    lbl(asyncm)
                ));
            }
        }
        out
    }

    /// The FULL list of concrete modifications an Apply would produce — the
    /// staged choices ([`style_diff_summary`](Self::style_diff_summary)) PLUS
    /// their effects: the `main.rs` entry-point change, and every
    /// `src/pins/configs/*.rs` file that would be added / removed / regenerated
    /// (dry-run: apply the pending choices to a clone and diff its
    /// `config_files()`). Shown in the Apply-confirm prompt. Empty when nothing is
    /// staged.
    pub fn apply_change_list(&self) -> Vec<String> {
        if !self.style_dirty() {
            return Vec::new();
        }
        // 1. The choices the user made.
        let mut out: Vec<String> = self
            .style_diff_summary()
            .into_iter()
            .map(|l| format!("• {l}"))
            .collect();

        // 2. Entry-point change. Blocking and Native share `#[entry] fn main()
        //    -> !`; Async and RTIC each write their own.
        {
            use super::model::Runtime;
            use crate::panels::mcu_module::codegen::family;
            // ESP spells both ends differently: esp-rtos drives the executor and
            // the blocking entry is esp-hal's, not cortex-m-rt's.
            let esp = family::async_is_esp(&self.family);
            let entry_of = |rt: Runtime| match rt {
                Runtime::Async if family::async_supported(&self.family) => {
                    if esp {
                        "#[esp_rtos::main] async fn main(Spawner)"
                    } else {
                        "#[embassy_executor::main] async fn main(Spawner)"
                    }
                }
                Runtime::Rtic if family::rtic_supported(&self.family) => {
                    "#[rtic::app] (init/idle, tasks bound to IRQs)"
                }
                _ if esp => "#[esp_hal::main] fn main() -> !",
                _ => "#[entry] fn main() -> !",
            };
            let (was, will) = (entry_of(self.runtime), entry_of(self.pending_runtime));
            if was != will {
                out.push(format!("main.rs entry -> {will}"));
                // Every other runtime closes its `fn main` in the tail below the
                // markers, and RTIC generates its idle loop - so that loop goes.
                // What follows `main` is kept (`rtic::splice_rtic_section`).
                if will.starts_with("#[rtic::app]") {
                    out.push(
                        concat!(
                            "! main.rs: the loop below the markers is replaced - ",
                            "RTIC generates its own idle task; code after fn main is kept"
                        )
                        .to_string(),
                    );
                }
            } else {
                out.push("~ main.rs regenerated (pin bindings)".to_string());
            }
        }

        // 3. Config-file adds / removes / regenerations — a dry-run of the regen.
        let mut preview = self.clone();
        preview.apply_pending_style();
        let before = self.config_files();
        let after = preview.config_files();
        let body_of = |v: &[(String, String)], n: &str| {
            v.iter().find(|(f, _)| f == n).map(|(_, b)| b.clone())
        };
        let names: std::collections::BTreeSet<String> = before
            .iter()
            .chain(after.iter())
            .map(|(n, _)| n.clone())
            .collect();
        for name in names {
            match (body_of(&before, &name), body_of(&after, &name)) {
                (None, Some(_)) => out.push(format!("+ src/pins/configs/{name}  (new)")),
                (Some(_), None) => out.push(format!(
                    "- src/pins/configs/{name}  (removed - your code in it comes back with it until you close the IDE)"
                )),
                (Some(b), Some(a)) if b != a => {
                    out.push(format!("~ src/pins/configs/{name}  (regenerated)"))
                }
                _ => {}
            }
        }

        // 4. Cargo.toml deps follow the choices (embassy / embedded-io / nb / …);
        //    the exact set is applied by `init_frame` after Apply.
        //
        //    On an F1 the HAL crate ITSELF changes with Async, which a line about
        //    "dependencies" does not say - nor that USB, CAN and SDIO are not
        //    generated there, which would otherwise surface only in main.rs.
        if self.family == "stm32f1" && self.pending_is_async() != self.is_async() {
            out.push(
                if self.pending_is_async() {
                    "~ Cargo.toml: stm32f1xx-hal -> embassy-stm32 (the HAL itself changes)"
                } else {
                    "~ Cargo.toml: embassy-stm32 -> stm32f1xx-hal (the HAL itself changes)"
                }
                .to_string(),
            );
            if self.pending_is_async() {
                use super::model::Runtime;
                use crate::panels::mcu_module::codegen::family::f1_async_module_gap;
                for m in &self.modules {
                    if f1_async_module_gap(&self.family, Runtime::Async, m.kind).is_some() {
                        out.push(format!(
                            "! {}: not generated on the Async runtime - its pads bind raw",
                            m.name
                        ));
                    }
                }
            }
        }
        out.push("~ Cargo.toml dependencies updated to match".to_string());
        out
    }

    /// Restores the clock-tree configuration parsed from a saved `main.rs`
    /// (`// @clock` marker). The saved config is expanded to graph node states
    /// and adopted by id — F103-shaped graphs restore fully; other-family
    /// graphs (no matching ids) are an intentional no-op.
    pub fn apply_saved_clock(&mut self, clock: crate::panels::mcu_module::clock::Stm32f1Clock) {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::graph::stm32f1_graph;
        if let ClockConfig::Graph(gc) = &mut self.clock {
            gc.graph.adopt_states(&stm32f1_graph(&clock));
        }
    }

    /// Restore the project's clock edits onto whatever tree this chip has.
    ///
    /// The family-neutral counterpart of [`apply_saved_clock`](Self::apply_saved_clock),
    /// which can only speak `Stm32f1Clock`. Call at the same point: after the
    /// definition's tree is installed and its defaults captured, since what is
    /// saved is the DELTA against those defaults.
    ///
    /// Returns how many states were applied — a tree whose node ids have since
    /// changed restores what it can rather than nothing.
    pub fn apply_saved_clock_nodes(&mut self, body: &str) -> usize {
        use crate::panels::mcu_module::clock::ClockConfig;
        use crate::panels::mcu_module::clock::persist;
        let saved = persist::nodes_from_block(body);
        match &mut self.clock {
            ClockConfig::Graph(gc) => persist::apply_nodes(&mut gc.graph, &saved),
            ClockConfig::None => 0,
        }
    }

    /// Snapshots the current clock tree as the "factory" configuration for
    /// [`reset_clock`](Self::reset_clock). Call right after installing the
    /// definition's clock and BEFORE any saved `@clock` state is adopted.
    pub fn capture_clock_defaults(&mut self) {
        use crate::panels::mcu_module::clock::ClockConfig;
        self.clock_defaults = match &self.clock {
            ClockConfig::Graph(gc) => Some(gc.graph.clone()),
            ClockConfig::None => None,
        };
    }

    /// Is the clock tree still exactly as the chip definition shipped it?
    /// `true` when there is nothing to reset (including chips with no clock).
    pub fn clock_is_default(&self) -> bool {
        use crate::panels::mcu_module::clock::ClockConfig;
        match (&self.clock, &self.clock_defaults) {
            (ClockConfig::Graph(gc), Some(def)) => gc.graph.states_match(def),
            _ => true,
        }
    }

    /// Restores the chip's default clock configuration (node states only — the
    /// diagram layout is cosmetic and stays put). Returns `true` if anything
    /// actually changed, so the caller can regenerate `main.rs`.
    pub fn reset_clock(&mut self) -> bool {
        use crate::panels::mcu_module::clock::ClockConfig;
        if self.clock_is_default() {
            return false;
        }
        // Cloned so the defaults stay borrowable independently of `self.clock`.
        let Some(defaults) = self.clock_defaults.clone() else {
            return false;
        };
        let ClockConfig::Graph(gc) = &mut self.clock else {
            return false;
        };
        gc.graph.adopt_states(&defaults);
        true
    }

    /// Resets all non-reserved pins to Unset and clears selection/info state.
    /// How many pins "Reset pins" would actually clear.
    ///
    /// The header button asks before wiping, and this is what makes the question
    /// worth asking: it names the loss, and it is 0 exactly when the button has
    /// nothing to do.
    pub fn configured_pin_count(&self) -> usize {
        self.iter_all_pins()
            .filter(|p| !p.reserved && p.selected_function != PinFunction::Unset)
            .count()
    }

    pub fn reset_all_pins(&mut self) {
        for pin in self.iter_all_pins_mut() {
            if !pin.reserved {
                pin.selected_function = PinFunction::Unset;
                // The label goes with the function, the rule
                // `apply_pin_function` states and enforces on its own `Unset`
                // branch. Left behind, the name the user typed survived a total
                // wipe and came back on the next binding for that pad - and, on
                // project open (where this is the clean slate before the saved
                // pins are applied), leaked from one project into the next.
                pin.custom_label.clear();
            }
        }
        self.selected_pin = None;
        self.show_info = None;
    }

    /// Whether the package carries pins INSIDE the body (a ball grid) rather
    /// than only around its edges.
    ///
    /// The body is shared real estate: an edge package has it empty and can put
    /// the chip name there, a grid package has it full of pads. Anything drawn in
    /// the middle has to ask this first.
    pub fn has_inner_pins(&self) -> bool {
        self.grid.as_ref().is_some_and(|g| !g.cells.is_empty())
    }

    /// Pin numbers matching the toolbar search box. Three ways to hit, because
    /// three different labels are printed on the diagram:
    /// * the pin NAME — case-insensitive substring (`pa5`, `osc`, `ph1`);
    /// * the package DESIGNATOR of a ball — same, case-insensitive substring
    ///   (`n13`, `m1`), so a BGA can be searched by the label under the ball;
    /// * the pin NUMBER — EXACT (`13` finds pin 13, not 13/1/31).
    ///
    /// Substring for the two text labels and exact for the number on purpose: a
    /// name/designator is what the user half-remembers off the package, a number
    /// is something they read precisely.
    ///
    /// The designator matters more than it looks on a ball-grid part: there the
    /// number is our own ordinal and is never drawn — the designator IS what the
    /// user sees under the ball, so searching "N13" has to work.
    pub fn pin_search_hits(&self) -> std::collections::HashSet<usize> {
        let q = self.pin_search.trim().to_ascii_lowercase();
        if q.is_empty() {
            return std::collections::HashSet::new();
        }
        let mut hits: std::collections::HashSet<usize> = self
            .iter_all_pins()
            .filter(|p| p.name.to_ascii_lowercase().contains(&q) || p.number.to_string() == q)
            .map(|p| p.number)
            .collect();
        for cell in self.grid.iter().flat_map(|g| g.cells.iter()) {
            if cell.designator().to_ascii_lowercase().contains(&q) {
                hits.insert(cell.pin.number);
            }
        }
        hits
    }

    /// The set the diagram highlights, or `None` when nothing should be dimmed.
    ///
    /// `None` covers both "no search" and "search matches nothing": fading the
    /// WHOLE chip while the user is still typing a prefix that hasn't matched yet
    /// would be noise, not feedback.
    pub fn pin_search_highlight(&self) -> Option<std::collections::HashSet<usize>> {
        let hits = self.pin_search_hits();
        (!hits.is_empty()).then_some(hits)
    }

    /// Iterator over every pin (all four sides), immutable.
    pub fn iter_all_pins(&self) -> impl Iterator<Item = &Pin> {
        self.top_pins
            .iter()
            .chain(self.bottom_pins.iter())
            .chain(self.left_pins.iter())
            .chain(self.right_pins.iter())
            // Ball-grid pads are pins like any other — chaining them HERE is
            // what lets autowire, codegen and persistence stay layout-blind.
            .chain(
                self.grid
                    .iter()
                    .flat_map(|g| g.cells.iter().map(|c| &c.pin)),
            )
    }

    /// Iterator over every pin (all four sides), mutable.
    pub fn iter_all_pins_mut(&mut self) -> impl Iterator<Item = &mut Pin> {
        self.top_pins
            .iter_mut()
            .chain(self.bottom_pins.iter_mut())
            .chain(self.left_pins.iter_mut())
            .chain(self.right_pins.iter_mut())
            .chain(
                self.grid
                    .iter_mut()
                    .flat_map(|g| g.cells.iter_mut().map(|c| &mut c.pin)),
            )
    }

    /// Auto-assigns partner functions when `source_pin` receives `func`: the
    /// MISO/MOSI that go with an SCK, the RX that goes with a TX.
    ///
    /// The pins come from the same scoring a whole module's wiring goes through
    /// ([`autowire::pick_partners`]) — which is what keeps the peripheral on ONE
    /// pad group. Picking the first available pin instead (what this did until
    /// 2026-08-12) answered PA5 SCK with PB4/PB5, mixing the F1 SPI1 default set
    /// with its remap set: a combination one AFIO bit cannot express, that no
    /// `stm32f1xx_hal::spi::Pins` impl accepts, and that therefore generated a
    /// project which could not compile.
    pub fn auto_assign_partners(&mut self, source_pin: usize, func: &PinFunction) {
        let picks = autowire::pick_partners(self, source_pin, func);
        for (partner, num) in picks {
            if let Some(pin) = self.find_pin_mut(num) {
                pin.selected_function = partner;
            }
        }
    }

    /// Removes the partner functions of `old_func` from whichever pins
    /// currently hold them (called when `source_pin` is deselected).
    pub fn deselect_partners(&mut self, source_pin: usize, old_func: &PinFunction) {
        for partner in partner_functions(old_func) {
            let target = self
                .iter_all_pins()
                .find(|p| p.number != source_pin && p.selected_function == partner)
                .map(|p| p.number);

            if let Some(num) = target {
                if let Some(pin) = self.find_pin_mut(num) {
                    pin.selected_function = PinFunction::Unset;
                }
            }
        }
    }

    /// Ask the editor to jump to the line that defines pin `pin_num`'s variable.
    /// A no-op for a pin with no function — it has no generated binding yet, and
    /// silently doing nothing is better than scrolling somewhere arbitrary.
    /// Consumed by `AppIde` (the panel owns the editor, the MCU doesn't).
    pub fn request_pin_goto(&mut self, pin_num: usize) {
        let configured = self
            .find_pin(pin_num)
            .is_some_and(|p| p.selected_function != PinFunction::Unset);
        if configured {
            self.pin_goto = Some(pin_num);
        }
    }

    /// Assign `func` to pin `pin_num`, applying the same side effects as a
    /// click on the Pins tab: auto-assign partner functions (or deselect them
    /// when clearing), and close any open info popup.
    ///
    /// Returns the `(number, name, func)` change tuple so code-sync callers can
    /// regenerate the `pins/` files; `None` if `pin_num` doesn't exist.
    pub fn apply_pin_function(
        &mut self,
        pin_num: usize,
        func: PinFunction,
    ) -> Option<(usize, String, PinFunction)> {
        let old_func = self.find_pin(pin_num)?.selected_function.clone();

        let changed = {
            let pin = self.find_pin_mut(pin_num)?;
            pin.selected_function = func.clone();
            // Clearing a pin also clears its user label, so a freed pin starts
            // clean if it's reassigned later.
            if func == PinFunction::Unset {
                pin.custom_label.clear();
            }
            (pin.number, pin.name.clone(), func.clone())
        };
        self.show_info = None;

        if func == PinFunction::Unset {
            self.deselect_partners(pin_num, &old_func);
        } else {
            self.auto_assign_partners(pin_num, &func);
        }

        // A pad re-pointed at another channel of the SAME timer keeps what the
        // user set for it. Done HERE, before `reconcile_modules` rebuilds the
        // wires, because this is the one door both entrances use — the canvas's
        // function list and the module panel's channel picker.
        self.carry_pwm_channel(&old_func, &func);

        // A pin re-purposed away from USART must drop any virtual-module wire.
        self.reconcile_modules();

        Some(changed)
    }

    /// Move a channel's duty (and its shape) when a pad changes which channel
    /// of the same timer it drives.
    ///
    /// Only when the OLD channel is left with no pad at all: two pads on one
    /// timer swapping channels between them must not have one of them drag the
    /// other's duty away.
    fn carry_pwm_channel(&mut self, old: &PinFunction, new: &PinFunction) {
        use crate::panels::mcu_module::modules::ModuleConfig;
        let (
            PinFunction::TimerPwm {
                timer: t_old,
                channel: from,
            },
            PinFunction::TimerPwm {
                timer: t_new,
                channel: to,
            },
        ) = (old, new)
        else {
            return;
        };
        if t_old != t_new || from == to {
            return;
        }
        let (timer, from, to) = (*t_old, *from, *to);
        // Still driven from somewhere else? Then its duty is not orphaned and
        // moving it would steal a live setting.
        let still_used = self.iter_all_pins().any(|p| {
            p.selected_function
                == PinFunction::TimerPwm {
                    timer,
                    channel: from,
                }
        });
        if still_used {
            return;
        }
        for m in &mut self.modules {
            if let ModuleConfig::Timer(cfg) = &mut m.config
                && cfg.instance == timer
            {
                cfg.move_channel(from, to);
            }
        }
    }

    /// Drop each module connection whose pin no longer carries the matching
    /// USART function — so re-purposing a pin disconnects the _USART from it
    /// (the module stays, just unwired). Idempotent.
    /// Make the virtual modules mirror the pin assignments: a peripheral
    /// instance with any assigned USART/SPI/I2C signal pin gets (or keeps) a
    /// module wired to exactly those pins; an instance with no assigned pins
    /// loses its module. So selecting peripheral pins in the Peripherals tab
    /// auto-adds the matching module, and clearing them removes it. Existing
    /// modules keep their config (only connections are re-synced); newly created
    /// ones get the default config. Idempotent; never mutates pins.
    pub fn reconcile_modules(&mut self) {
        use crate::panels::mcu_module::modules::{
            Connection, ModuleKind, ModuleSignal, VirtualModule, module_signal_of,
        };
        use std::collections::BTreeMap;

        let mut wanted: BTreeMap<(ModuleKind, u8), Vec<(ModuleSignal, usize)>> = BTreeMap::new();
        for p in self.iter_all_pins() {
            if let Some((kind, inst, sig)) = module_signal_of(&p.selected_function) {
                wanted
                    .entry((kind, inst))
                    .or_default()
                    .push((sig, p.number));
            }
        }

        // Drop modules whose peripheral no longer has any assigned pins — but
        // NEVER a Custom one: it is authored by the user, not derived from the
        // pins, so only an explicit Remove takes it away.
        self.modules
            .retain(|m| m.kind.is_custom() || wanted.contains_key(&(m.kind, m.instance())));

        // Every bit of view state keyed on a module ID has to die with the
        // module — and here, before the loop below mints new ones.
        //
        // `free_module_id` walks up from `modules.len() + 1`, so an id a removal
        // frees is the VERY NEXT one handed out. A survivor therefore does not
        // merely go stale, it lands on an unrelated new module: its staged
        // Init-API is applied to something the user never staged it for, its box
        // comes up already selected, and an armed remove-confirm pulses a
        // stranger red.
        //
        // This is the only place a derived module dies, and it runs every frame
        // from the canvas — so it is the one place that cannot be skipped by a
        // panel being collapsed or a tab being elsewhere.
        let live: std::collections::BTreeSet<&str> =
            self.modules.iter().map(|m| m.id.as_str()).collect();
        let dead = |id: &Option<String>| id.as_deref().is_some_and(|i| !live.contains(i));
        let (drop_confirm, drop_selected) = (
            dead(&self.module_remove_confirm),
            dead(&self.selected_module),
        );
        self.pending_module_styles
            .retain(|id, _| live.contains(id.as_str()));
        if drop_confirm {
            self.module_remove_confirm = None;
        }
        if drop_selected {
            self.selected_module = None;
        }
        // The same for a device of an I2C bus whose removal is armed.
        self.retire_i2c_confirm();

        // A custom module's wires mirror its own pin list (which the config
        // panel edits), so rebuild them here — the canvas then draws them with
        // the same machinery as every peripheral module.
        for m in self.modules.iter_mut().filter(|m| m.kind.is_custom()) {
            let pins: Vec<usize> = match &m.config {
                crate::panels::mcu_module::modules::ModuleConfig::Custom(c) => c.pins.clone(),
                _ => Vec::new(),
            };
            m.connections = pins
                .into_iter()
                .map(|mcu_pin| Connection {
                    signal: ModuleSignal::CustomPin,
                    mcu_pin,
                })
                .collect();
        }

        // Ensure a module per wanted peripheral and re-sync its connections.
        for ((kind, inst), mut conns) in wanted {
            conns.sort_by_key(|(s, _)| *s);
            let connections: Vec<Connection> = conns
                .into_iter()
                .map(|(signal, mcu_pin)| Connection { signal, mcu_pin })
                .collect();

            if let Some(pos) = self
                .modules
                .iter()
                .position(|m| m.kind == kind && m.instance() == inst)
            {
                self.modules[pos].connections = connections;
            } else {
                let id = self.free_module_id(&kind.short().to_ascii_lowercase());
                self.modules.push(VirtualModule {
                    id,
                    kind,
                    name: format!("{}{inst}", kind.short()),
                    pos: (0.0, 0.0),
                    config: kind.default_config(inst),
                    connections,
                });
            }
        }
    }

    /// The notes shown on `m`, if it has any worth showing. Looked up by
    /// `(kind, instance)`, never by id - see [`Mcu::module_notes`].
    pub fn notes_for(
        &self,
        m: &crate::panels::mcu_module::modules::VirtualModule,
    ) -> Option<&crate::panels::mcu_module::modules::ModuleNotes> {
        self.module_notes
            .get(&(m.kind, m.instance()))
            .filter(|n| !n.is_empty())
    }

    /// The notes for one peripheral instance, created empty on first write. An
    /// entry left empty costs nothing: it is neither saved nor listed.
    pub fn notes_mut(
        &mut self,
        key: crate::panels::mcu_module::modules::NotesKey,
    ) -> &mut crate::panels::mcu_module::modules::ModuleNotes {
        self.module_notes.entry(key).or_default()
    }

    /// Keys that hold notes no live module shows - the "Notes without a module"
    /// list. In key order, empty entries left out.
    pub fn orphan_notes(&self) -> Vec<crate::panels::mcu_module::modules::NotesKey> {
        self.module_notes
            .iter()
            .filter(|(k, n)| {
                !n.is_empty() && !self.modules.iter().any(|m| (m.kind, m.instance()) == **k)
            })
            .map(|(k, _)| *k)
            .collect()
    }

    /// Forget one peripheral's notes. The image FILE stays: the IDE never
    /// deletes one (see `notes::store_image`).
    pub fn delete_notes(&mut self, key: crate::panels::mcu_module::modules::NotesKey) {
        self.module_notes.remove(&key);
    }

    /// A module id nothing else is using.
    ///
    /// # Why `len() + 1` was not one
    ///
    /// Both id factories used to be `format!("{base}_{}", self.modules.len() + 1)`
    /// — which is "one more than however many modules happen to exist", not "the
    /// next free number". Remove a module and add another and the new one takes
    /// an id that is still taken:
    ///
    /// ```text
    /// wire USART0 + USART1   -> usart_1, usart_2
    /// unwire USART0          -> usart_2
    /// wire USART0 again      -> usart_2, usart_2      <- both, same id
    /// ```
    ///
    /// The id is not decoration. It keys the list's `CollapsingState`, so two
    /// modules sharing one open together and cannot be opened apart; it is the
    /// `push_id` namespace for the whole config grid, so every widget inside
    /// both collides and egui paints its ID-clash banner over the panel; and it
    /// names the `mod <id>` block `ensure_module_models` writes into main.rs.
    ///
    /// Starts at the old number and walks up, so an id that was free stays the
    /// id it always was — only a collision moves.
    fn free_module_id(&self, base: &str) -> String {
        let mut n = self.modules.len() + 1;
        loop {
            let id = format!("{base}_{n}");
            if !self.modules.iter().any(|m| m.id == id) {
                return id;
            }
            n += 1;
        }
    }

    /// Finds a pin by number (immutable)
    pub fn find_pin(&self, number: usize) -> Option<&Pin> {
        self.iter_all_pins().find(|p| p.number == number)
    }

    /// Finds a pin by number (mutable)
    pub fn find_pin_mut(&mut self, number: usize) -> Option<&mut Pin> {
        self.iter_all_pins_mut().find(|p| p.number == number)
    }
}

#[cfg(test)]
mod module_id_tests {
    use crate::panels::mcu_module::modules::ModuleKind;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    fn ids(mcu: &super::Mcu) -> Vec<String> {
        mcu.modules.iter().map(|m| m.id.clone()).collect()
    }

    fn assert_distinct(mcu: &super::Mcu, when: &str) {
        let got = ids(mcu);
        let mut u = got.clone();
        u.sort();
        u.dedup();
        assert_eq!(
            u.len(),
            got.len(),
            "{when}: ids must be distinct, got {got:?}"
        );
    }

    /// Removing a module and adding another must not hand out an id that is
    /// still in use.
    ///
    /// The id was built from `modules.len() + 1` — "one more than however many
    /// exist", not "the next free one" — so this exact sequence produced TWO
    /// modules called `usart_2`. The id keys the list's `CollapsingState` and is
    /// the `push_id` namespace for the config grid, so the pair opened and
    /// closed together and egui painted its ID-clash banner over every widget in
    /// both of their grids.
    #[test]
    fn a_re_wired_module_does_not_reuse_a_live_id() {
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c3")
            .expect("a bundled ESP32-C3")
            .build_mcu();

        let mut wired = 0;
        for inst in [0u8, 1] {
            for f in [PinFunction::UsartTx(inst), PinFunction::UsartRx(inst)] {
                let free = mcu
                    .iter_all_pins()
                    .find(|p| {
                        p.selected_function == PinFunction::Unset
                            && p.available_functions.contains(&f)
                    })
                    .map(|p| p.number);
                if let Some(n) = free {
                    mcu.apply_pin_function(n, f);
                    wired += 1;
                }
            }
        }
        assert_eq!(wired, 4, "the C3 has two USARTs to wire");
        assert_eq!(mcu.modules.len(), 2);
        assert_distinct(&mcu, "freshly wired");

        // Move USART0 off its pads and back on — what a user does when the
        // board wants the peripheral somewhere else.
        let pads: Vec<usize> = mcu
            .iter_all_pins()
            .filter(|p| {
                matches!(
                    p.selected_function,
                    PinFunction::UsartTx(0) | PinFunction::UsartRx(0)
                )
            })
            .map(|p| p.number)
            .collect();
        for n in &pads {
            mcu.apply_pin_function(*n, PinFunction::Unset);
        }
        assert_eq!(mcu.modules.len(), 1, "USART0's module went with its pads");

        for n in &pads {
            let f = mcu
                .find_pin(*n)
                .unwrap()
                .available_functions
                .iter()
                .find(|f| matches!(f, PinFunction::UsartTx(0) | PinFunction::UsartRx(0)))
                .cloned()
                .expect("the pad still offers USART0");
            mcu.apply_pin_function(*n, f);
        }
        assert_eq!(mcu.modules.len(), 2, "and came back");
        assert_distinct(&mcu, "after a re-wire");
    }

    /// The Custom palette had the same defect, from the same expression.
    #[test]
    fn removing_a_custom_module_does_not_free_a_live_id() {
        let mut mcu = crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx();
        for _ in 0..3 {
            assert!(mcu.add_module(ModuleKind::Custom));
        }
        assert_distinct(&mcu, "three customs");

        // Drop the FIRST, so the count no longer matches the highest number.
        let first = mcu.modules[0].id.clone();
        mcu.modules.retain(|m| m.id != first);
        assert!(mcu.add_module(ModuleKind::Custom));
        assert_distinct(&mcu, "after removing one and adding another");
    }

    /// The numbering a project already has must not move: only a collision
    /// does. Otherwise a `mod <id>` data-model block already written into
    /// main.rs would be orphaned by a rename it never asked for.
    #[test]
    fn an_uncontested_id_keeps_the_number_it_always_had() {
        let mut mcu = crate::panels::mcu_module::mock_mcu::create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert_eq!(mcu.modules[0].id, "usart_1");
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        assert_eq!(mcu.modules[1].id, "spi_2");
    }
}

#[cfg(test)]
mod reset_pins_tests {
    use crate::panels::mcu_module::create_stm32f103c8tx;
    use crate::panels::mcu_module::modules::ModuleKind;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    /// The count the header's confirm names, and the reset it confirms.
    ///
    /// Reserved pins (VDD/VSS/NRST) never count: they carry no function to
    /// clear, so including them would inflate the number the question shows.
    #[test]
    fn the_count_is_what_reset_actually_clears() {
        let mut mcu = create_stm32f103c8tx();
        assert_eq!(mcu.configured_pin_count(), 0, "a fresh chip has none");

        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        mcu.apply_pin_function(11, PinFunction::GpioInput);
        assert_eq!(mcu.configured_pin_count(), 2);

        // A module wires several pins at once — all of them count.
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let with_module = mcu.configured_pin_count();
        assert!(with_module > 2, "the USART's pins count too: {with_module}");

        mcu.reset_all_pins();
        assert_eq!(mcu.configured_pin_count(), 0);
        assert!(
            mcu.iter_all_pins()
                .all(|p| p.reserved || p.selected_function == PinFunction::Unset)
        );
        // And the modules go with the pins they were wired to — which is the
        // part of the loss the confirm has to warn about.
        mcu.reconcile_modules();
        assert!(mcu.modules.is_empty(), "{:?}", mcu.modules.len());
    }
}

#[cfg(test)]
mod module_notes_tests {
    use crate::panels::mcu_module::create_stm32f103c8tx;
    use crate::panels::mcu_module::modules::{ModuleKind, ModuleNotes};
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    fn note(text: &str) -> ModuleNotes {
        ModuleNotes {
            text: text.to_owned(),
            ..Default::default()
        }
    }

    /// The case the whole design exists for. Clearing a bus to move it drops
    /// the module and a re-wire mints a NEW id - so notes stored on the module,
    /// or keyed by its id, would be gone. Keyed by (kind, instance), they are
    /// an orphan in between and come back by themselves.
    #[test]
    fn notes_survive_unwiring_and_rewiring_the_bus() {
        let mut mcu = crate::panels::mcu_module::builtins::builtin_for("esp32c3")
            .expect("a bundled ESP32-C3")
            .build_mcu();
        let mut pads = Vec::new();
        for f in [PinFunction::UsartTx(0), PinFunction::UsartRx(0)] {
            let n = mcu
                .iter_all_pins()
                .find(|p| {
                    p.selected_function == PinFunction::Unset && p.available_functions.contains(&f)
                })
                .map(|p| p.number)
                .expect("a free USART0 pad");
            mcu.apply_pin_function(n, f.clone());
            pads.push((n, f));
        }
        let key = (ModuleKind::GenericInterfaceUsart, 0);
        *mcu.notes_mut(key) = note("GPS NEO-6M, 9600 baud");

        for (n, _) in &pads {
            mcu.apply_pin_function(*n, PinFunction::Unset);
        }
        assert!(mcu.modules.is_empty(), "the module went with its pads");
        assert_eq!(mcu.orphan_notes(), vec![key], "and its notes are an orphan");

        for (n, f) in &pads {
            mcu.apply_pin_function(*n, f.clone());
        }
        let m = mcu.modules[0].clone();
        assert!(mcu.orphan_notes().is_empty(), "re-attached");
        assert_eq!(mcu.notes_for(&m).unwrap().text, "GPS NEO-6M, 9600 baud");
    }

    /// Reset pins drops every derived module on the next reconcile, and Ctrl+Z
    /// brings the modules back from a snapshot that holds no notes at all.
    #[test]
    fn reset_pins_then_undo_keeps_the_notes() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let key = (mcu.modules[0].kind, mcu.modules[0].instance());
        *mcu.notes_mut(key) = note("level shifter on RX");
        mcu.push_module_undo("Reset pins".into());

        mcu.reset_all_pins();
        mcu.reconcile_modules();
        assert!(mcu.modules.is_empty());
        assert_eq!(mcu.module_notes.len(), 1, "the map is untouched");

        assert!(mcu.undo_modules().is_some());
        let m = mcu.modules[0].clone();
        assert_eq!(mcu.notes_for(&m).unwrap().text, "level shifter on RX");
    }

    /// Notes are not in the undo snapshot, so undoing a module edit never
    /// throws away text typed after it.
    #[test]
    fn undo_never_reverts_typed_notes() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        mcu.push_module_undo("before".into());
        let key = (mcu.modules[0].kind, mcu.modules[0].instance());
        *mcu.notes_mut(key) = note("typed after the snapshot");
        assert!(mcu.undo_modules().is_some());
        assert_eq!(mcu.module_notes[&key].text, "typed after the snapshot");
    }

    /// A new Custom must not open with the notes of a removed one. With no
    /// notes around, the numbering is exactly what it always was.
    #[test]
    fn a_new_custom_never_inherits_orphaned_notes() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::Custom));
        assert_eq!(mcu.modules[0].instance(), 1);
        let id = mcu.modules[0].id.clone();
        mcu.remove_module(&id);
        assert!(mcu.add_module(ModuleKind::Custom));
        assert_eq!(mcu.modules[0].instance(), 1, "no notes: old numbering");

        *mcu.notes_mut((ModuleKind::Custom, 1)) = note("breadboard LEDs");
        let id = mcu.modules[0].id.clone();
        mcu.remove_module(&id);
        assert!(mcu.add_module(ModuleKind::Custom));
        let m = mcu.modules[0].clone();
        assert_eq!(m.instance(), 2, "skips the number that holds notes");
        assert!(mcu.notes_for(&m).is_none());
        assert_eq!(mcu.orphan_notes(), vec![(ModuleKind::Custom, 1)]);
    }

    #[test]
    fn orphans_are_non_empty_keys_without_a_live_module() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let live = (mcu.modules[0].kind, mcu.modules[0].instance());
        *mcu.notes_mut(live) = note("live");
        *mcu.notes_mut((ModuleKind::GenericInterfaceSpi, 2)) = note("gone");
        mcu.notes_mut((ModuleKind::GenericInterfaceI2c, 1)); // empty: not an orphan
        assert_eq!(
            mcu.orphan_notes(),
            vec![(ModuleKind::GenericInterfaceSpi, 2)]
        );

        mcu.delete_notes((ModuleKind::GenericInterfaceSpi, 2));
        assert!(mcu.orphan_notes().is_empty());
        assert!(mcu.module_notes.contains_key(&live), "only that key went");
    }
}

#[cfg(test)]
mod module_notes_persist_tests {
    use crate::panels::mcu_module::create_stm32f103c8tx;
    use crate::panels::mcu_module::modules::{ModuleKind, ModuleNotes};

    /// A project that never used notes is written exactly as before - no new
    /// section, no new byte - and reads back to the same text.
    #[test]
    fn a_project_without_notes_round_trips_byte_identically() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let text = mcu.mcu_config_text();
        assert!(!text.contains("@modulenotes"), "{text}");

        let mut back = create_stm32f103c8tx();
        back.apply_mcu_config(&text);
        back.restore_module_notes(Some(&text));
        assert!(back.module_notes.is_empty());
    }

    #[test]
    fn notes_round_trip_through_mcu_config_text() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let key = (mcu.modules[0].kind, mcu.modules[0].instance());
        *mcu.notes_mut(key) = ModuleNotes {
            text: "u-blox NEO-6M\n3.3 V only".into(),
            link: "https://x.example/neo6m.pdf".into(),
            image: "docs/modules/neo6m.jpg".into(),
        };
        let text = mcu.mcu_config_text();
        assert!(text.contains("@modulenotes"), "{text}");

        let mut back = create_stm32f103c8tx();
        back.restore_module_notes(Some(&text));
        assert_eq!(back.module_notes, mcu.module_notes);
    }

    /// Opening a project replaces the notes even when it has none - otherwise
    /// the previous project's would carry over into it.
    #[test]
    fn opening_a_project_replaces_the_notes_even_when_it_has_none() {
        let mut mcu = create_stm32f103c8tx();
        *mcu.notes_mut((ModuleKind::GenericInterfaceSpi, 1)) = ModuleNotes {
            text: "from the last project".into(),
            ..Default::default()
        };
        mcu.restore_module_notes(Some("@clock\nhse=8000000\n"));
        assert!(mcu.module_notes.is_empty(), "a config without the section");

        *mcu.notes_mut((ModuleKind::GenericInterfaceSpi, 1)) = ModuleNotes {
            text: "again".into(),
            ..Default::default()
        };
        mcu.restore_module_notes(None);
        assert!(mcu.module_notes.is_empty(), "no mcu.config at all");
    }
}

#[cfg(test)]
mod pin_search_tests {
    use crate::panels::mcu_module::create_stm32f103c8tx;

    #[test]
    fn name_matches_any_part_case_insensitively() {
        let mut mcu = create_stm32f103c8tx();
        mcu.pin_search = "pb1".to_owned();
        let hits = mcu.pin_search_hits();
        // PB1 and PB10..PB15 — a substring, which is what a half-remembered
        // name needs.
        let names: Vec<String> = mcu
            .iter_all_pins()
            .filter(|p| hits.contains(&p.number))
            .map(|p| p.name.clone())
            .collect();
        assert!(names.contains(&"PB1".to_owned()), "{names:?}");
        assert!(names.contains(&"PB12".to_owned()), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("PA")), "{names:?}");

        // Case doesn't matter.
        mcu.pin_search = "PB1".to_owned();
        assert_eq!(mcu.pin_search_hits(), hits);
    }

    /// On a ball-grid package the pin NUMBER is our own ordinal and is never
    /// drawn — the designator under the ball is what the user reads, so it has
    /// to be searchable (the reported bug: "N13" found nothing).
    #[test]
    fn ball_designator_is_searchable() {
        use crate::panels::mcu_module::mcu::model::{GridCell, PinGrid};
        use crate::panels::mcu_module::pins::logic::pin::Pin;

        let mut mcu = create_stm32f103c8tx();
        // Row 11 = "M", row 12 = "N" (JEDEC skips I/O/Q/S/X/Z); col 12 = "13".
        mcu.grid = Some(PinGrid {
            rows: 13,
            cols: 13,
            cells: vec![
                GridCell {
                    row: 12,
                    col: 12,
                    pin: Pin::new(900, "PH12"),
                },
                GridCell {
                    row: 11,
                    col: 11,
                    pin: Pin::new(901, "PH11"),
                },
            ],
        });
        mcu.pin_search = "N13".to_owned();
        let hits = mcu.pin_search_hits();
        assert!(hits.contains(&900), "designator N13 -> {hits:?}");
        assert!(!hits.contains(&901), "M12 must stay dimmed: {hits:?}");
        // Lower case works the same, and the NAME still matches on its own.
        mcu.pin_search = "n13".to_owned();
        assert!(mcu.pin_search_hits().contains(&900));
        mcu.pin_search = "ph12".to_owned();
        assert!(mcu.pin_search_hits().contains(&900));
    }

    /// A number is EXACT: "13" is pin 13, not 13 + 1 + 31.
    #[test]
    fn number_matches_exactly() {
        let mut mcu = create_stm32f103c8tx();
        mcu.pin_search = "13".to_owned();
        let hits = mcu.pin_search_hits();
        assert!(hits.contains(&13));
        assert!(!hits.contains(&1) && !hits.contains(&31), "{hits:?}");
    }

    /// Empty box, or a query nothing matches → NOTHING is dimmed. Fading the
    /// whole chip while the user is mid-word would be noise.
    #[test]
    fn no_query_and_no_match_both_dim_nothing() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.pin_search_highlight().is_none());
        mcu.pin_search = "   ".to_owned();
        assert!(mcu.pin_search_highlight().is_none());
        mcu.pin_search = "zzz".to_owned();
        assert!(mcu.pin_search_hits().is_empty());
        assert!(mcu.pin_search_highlight().is_none());
        mcu.pin_search = "pa5".to_owned();
        assert!(mcu.pin_search_highlight().is_some());
    }
}

#[cfg(test)]
mod iomode_persist_tests {
    use crate::panels::mcu_module::create_stm32f103c8tx;
    use crate::panels::mcu_module::pins::logic::pin::GpioMode;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;

    /// A chosen GPIO mode survives save → load through `mcu.config` `@iomode`.
    #[test]
    fn gpio_modes_round_trip_through_mcu_config() {
        let mut mcu = create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        mcu.apply_pin_function(11, PinFunction::GpioInput);
        mcu.find_pin_mut(10).unwrap().io_mode = Some(GpioMode::OpenDrain);
        mcu.find_pin_mut(11).unwrap().io_mode = Some(GpioMode::PullDown);

        let text = mcu.mcu_config_text();
        assert!(text.contains("@iomode"), "{text}");

        let mut reloaded = create_stm32f103c8tx();
        reloaded.apply_pin_function(10, PinFunction::GpioOutput);
        reloaded.apply_pin_function(11, PinFunction::GpioInput);
        reloaded.apply_mcu_config(&text);
        assert_eq!(
            reloaded.find_pin(10).unwrap().io_mode,
            Some(GpioMode::OpenDrain)
        );
        assert_eq!(
            reloaded.find_pin(11).unwrap().io_mode,
            Some(GpioMode::PullDown)
        );
    }

    /// A project that never touched a mode writes NO section at all, so its
    /// `mcu.config` is byte-identical to what older versions produced.
    #[test]
    fn untouched_modes_write_no_section() {
        let mut mcu = create_stm32f103c8tx();
        mcu.apply_pin_function(10, PinFunction::GpioOutput);
        assert!(!mcu.mcu_config_text().contains("@iomode"));
    }
}

#[cfg(test)]
mod module_support_tests {
    use crate::panels::mcu_module::modules::ModuleKind;
    use crate::panels::mcu_module::pins::logic::pin_function::PinFunction;
    use crate::panels::mcu_module::{create_esp32c3, create_stm32f103c8tx};

    /// The S2 and S3 have the touch sensors and esp-hal has no driver for them,
    /// so the palette keeps the entry and says why instead of dropping it.
    ///
    /// The distinction that matters: a chip with no touch AT ALL gets no
    /// sentence, because there is nothing to explain — the module is simply not
    /// its. Claiming otherwise would be worse than silence.
    #[test]
    fn a_peripheral_without_a_driver_says_so_instead_of_vanishing() {
        let touch = ModuleKind::GenericInterfaceTouch;
        for chip in ["esp32s2", "esp32s3"] {
            let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                .unwrap()
                .build_mcu();
            assert!(!mcu.supports_module(touch), "{chip}: no pads, so no module");
            let why = mcu
                .hardware_only_reason(touch)
                .unwrap_or_else(|| panic!("{chip} should explain itself"));
            assert!(why.contains("esp-hal"), "{chip}: names the real limit");
        }

        // The original ESP32 has the driver, so it is offered outright.
        let esp32 = crate::panels::mcu_module::builtins::builtin_for("esp32")
            .unwrap()
            .build_mcu();
        assert!(esp32.supports_module(touch));
        assert!(esp32.hardware_only_reason(touch).is_none());

        // …and a chip with no touch silicon says nothing at all.
        for chip in ["esp32c6", "stm32f103c8t6"] {
            let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                .unwrap()
                .build_mcu();
            assert!(
                mcu.hardware_only_reason(touch).is_none(),
                "{chip} has no touch to explain"
            );
        }

        // Nothing else claims to be hardware-only anywhere, so the palette is
        // unchanged for every kind but this one.
        for kind in ModuleKind::ALL {
            if kind == touch {
                continue;
            }
            for chip in ["esp32", "esp32s2", "esp32s3", "esp32c6", "stm32f103c8t6"] {
                let mcu = crate::panels::mcu_module::builtins::builtin_for(chip)
                    .unwrap()
                    .build_mcu();
                assert!(
                    mcu.hardware_only_reason(kind).is_none(),
                    "{chip} vs {}",
                    kind.short()
                );
            }
        }
    }

    /// Both bundled chips genuinely expose all five interfaces, so a fresh
    /// palette offers everything.
    #[test]
    fn bundled_chips_support_every_kind_they_have_pins_for() {
        for mcu in [create_stm32f103c8tx(), create_esp32c3()] {
            for kind in ModuleKind::ALL {
                // The exceptions are the RULE working: the palette is derived
                // from the PINS, and a built-in that does not name those pads
                // does not offer the module. No built-in has an LPUART (the
                // F103 predates the peripheral, the ESP32-C3 has no such
                // thing), the F103 spells out no SAI, SD-card, external-memory
                // or DAC pads, and neither does the C3.
                //
                // I2S is the one that MOVED: the ESP32-C3 has an I2S block that
                // esp-hal drives, so its pads now carry the four I2S functions
                // and the module is offered. The F103's I2S rides on its SPI
                // block and its hand-written definition still does not name the
                // pads, so it stays out.
                let esp = mcu.name.starts_with("ESP32");
                let want = !matches!(
                    kind,
                    ModuleKind::GenericInterfaceLpuart
                        // PCNT and MCPWM are Espressif's, and neither built-in
                        // has either: no STM32 does, and the ESP32-C3 is one of
                        // the parts esp-hal gives neither driver to.
                        | ModuleKind::GenericInterfacePcnt
                        | ModuleKind::GenericInterfaceMcpwm
                        // PARL_IO likewise: only three ESP parts have one, and
                        // the C3 is not among them.
                        | ModuleKind::GenericInterfaceParlIo
                        // …and its receiving half, which is its own kind.
                        | ModuleKind::GenericInterfaceParlIoRx
                        // LCD_CAM is the ESP32-S3's alone, and neither built-in
                        // is one - so neither offers the pads.
                        | ModuleKind::GenericInterfaceLcdCam
                        // …and so is its camera half.
                        | ModuleKind::GenericInterfaceCamera
                        // Touch is the original ESP32's alone, for the same
                        // reason: esp-hal builds no driver for the C3.
                        | ModuleKind::GenericInterfaceTouch
                        | ModuleKind::GenericInterfaceSai
                        | ModuleKind::GenericInterfaceSdmmc
                        | ModuleKind::GenericInterfaceQspi
                        | ModuleKind::GenericInterfaceOspi
                        | ModuleKind::GenericInterfaceXspi
                        | ModuleKind::GenericInterfaceHspi
                        | ModuleKind::GenericInterfaceDac
                ) && (kind != ModuleKind::GenericInterfaceI2s || esp)
                    // RMT is Espressif's outright: no STM32 pin ever carries it,
                    // and the C3's do, so the palette offers it on one and not
                    // the other.
                    && (kind != ModuleKind::GenericInterfaceRmt || esp);
                assert_eq!(
                    mcu.supports_module(kind),
                    want,
                    "{} vs {}",
                    mcu.name,
                    kind.short()
                );
            }
        }
    }

    /// USB is the one kind gated by FAMILY as well as by pins: the D-/D+ pins
    /// exist on chips whose backend writes no USB code, where adding the module
    /// only ever produced two stray dependencies.
    #[test]
    fn usb_is_offered_only_where_the_backend_generates_it() {
        let mut mcu = create_stm32f103c8tx();
        assert!(
            mcu.supports_module(ModuleKind::GenericInterfaceUsb),
            "F1 generates the whole CDC device"
        );

        // Same chip, same pins, a family whose backend emits no USB code.
        for family in ["stm32f4", "stm32h5", "stm32wba"] {
            mcu.family = family.to_string();
            assert!(
                !mcu.supports_module(ModuleKind::GenericInterfaceUsb),
                "{family} must not offer USB"
            );
            // …and every other kind is unaffected by the gate.
            assert!(
                mcu.supports_module(ModuleKind::GenericInterfaceUsart),
                "{family} still offers USART"
            );
            assert!(mcu.supports_module(ModuleKind::GenericInterfaceSpi));
        }

        // ESP acknowledges its hardware-fixed USB peripheral in the generated
        // code, so the module still means something there.
        mcu.family = "esp32c3".into();
        assert!(mcu.supports_module(ModuleKind::GenericInterfaceUsb));
    }

    /// Support is derived from the PINS: strip a peripheral's pins and its kind
    /// disappears from the palette — no per-family list to maintain.
    #[test]
    fn a_chip_without_the_pins_does_not_offer_the_kind() {
        let mut mcu = create_stm32f103c8tx();
        assert!(mcu.supports_module(ModuleKind::GenericInterfaceUsb));
        for p in mcu.iter_all_pins_mut() {
            p.available_functions
                .retain(|f| !matches!(f, PinFunction::UsbDm | PinFunction::UsbDp));
        }
        assert!(
            !mcu.supports_module(ModuleKind::GenericInterfaceUsb),
            "no USB pins -> the kind is hidden"
        );
        // Untouched peripherals still show.
        assert!(mcu.supports_module(ModuleKind::GenericInterfaceUsart));
        assert!(mcu.supports_module(ModuleKind::GenericInterfaceI2c));
    }

    /// The subtle part the dry-run buys us: ONE peripheral instance must offer
    /// every required signal — it is NOT enough that some pin can TX and some
    /// unrelated pin can RX.
    #[test]
    fn required_signals_must_come_from_the_same_instance() {
        let mut mcu = create_stm32f103c8tx();
        for p in mcu.iter_all_pins_mut() {
            p.available_functions.retain(|f| match f {
                PinFunction::UsartTx(n) => *n == 1, // TX only on USART1
                PinFunction::UsartRx(n) => *n == 2, // RX only on USART2
                _ => true,
            });
        }
        assert!(
            !mcu.supports_module(ModuleKind::GenericInterfaceUsart),
            "TX on USART1 + RX on USART2 is not a usable pair"
        );
    }

    /// Exhausting a kind must flip only AVAILABILITY — it stays supported, so
    /// the palette keeps the button visible (disabled + reason) instead of
    /// silently dropping it.
    #[test]
    fn exhausting_a_kind_keeps_it_supported_but_unavailable() {
        let mut mcu = create_stm32f103c8tx();
        let kind = ModuleKind::GenericInterfaceUsb; // single-instance
        assert!(mcu.supports_module(kind) && mcu.can_add_module(kind));
        assert!(mcu.add_module(kind));
        assert!(mcu.supports_module(kind), "the chip still has the pins");
        assert!(!mcu.can_add_module(kind), "but none are free any more");
    }

    /// …and the same when the pads are spent on ANOTHER function, which is the
    /// case the test above cannot reach: `add_module` leaves the pads carrying
    /// the peripheral's own signals, the one value the dynamic candidate filter
    /// still accepts. A kind whose every candidate pad the user assigned
    /// elsewhere used to leave the palette entirely — no row, no reason.
    #[test]
    fn spending_a_kinds_pads_elsewhere_keeps_it_in_the_palette() {
        let mut mcu = create_stm32f103c8tx();
        let kind = ModuleKind::GenericInterfaceUsb;
        assert!(mcu.supports_module(kind) && mcu.can_add_module(kind));

        // The only two pads USB could use, given to something else.
        let pads: Vec<usize> = mcu
            .iter_all_pins()
            .filter(|p| {
                p.available_functions.contains(&PinFunction::UsbDm)
                    || p.available_functions.contains(&PinFunction::UsbDp)
            })
            .map(|p| p.number)
            .collect();
        assert!(!pads.is_empty(), "the fixture has USB pads");
        for p in pads {
            mcu.apply_pin_function(p, PinFunction::GpioOutput);
        }

        assert!(
            mcu.supports_module(kind),
            "the chip still HAS the peripheral, so the palette still shows it"
        );
        assert!(
            !mcu.can_add_module(kind),
            "but it cannot be wired right now"
        );
    }

    /// The two ways `can_add_module` says no are told apart, so the greyed
    /// entry names the thing that is actually in the way.
    #[test]
    fn a_free_instance_with_spent_pads_is_not_reported_as_exhausted() {
        let mut mcu = create_stm32f103c8tx();
        let kind = ModuleKind::GenericInterfaceUsart;
        assert!(mcu.has_free_instance(kind), "nothing is wired yet");

        // Every USART pad on the chip, given to something else. No module holds
        // an instance, so every instance is still FREE - only its pads are gone.
        let pads: Vec<usize> = mcu
            .iter_all_pins()
            .filter(|p| {
                p.available_functions
                    .iter()
                    .any(|f| matches!(f, PinFunction::UsartTx { .. } | PinFunction::UsartRx { .. }))
            })
            .map(|p| p.number)
            .collect();
        assert!(!pads.is_empty());
        for p in pads {
            mcu.apply_pin_function(p, PinFunction::GpioOutput);
        }

        assert!(!mcu.can_add_module(kind), "it cannot be added");
        assert!(
            mcu.has_free_instance(kind),
            "…but not because the instances are taken - none of them is"
        );
    }

    /// A module HOLDING every instance is the other answer, and it must stay
    /// distinguishable from the one above.
    #[test]
    fn holding_every_instance_reports_no_free_instance() {
        let mut mcu = create_stm32f103c8tx();
        let kind = ModuleKind::GenericInterfaceUsb; // single-instance
        assert!(mcu.has_free_instance(kind));
        assert!(mcu.add_module(kind));
        assert!(
            !mcu.has_free_instance(kind),
            "the only instance is wired to a module"
        );
    }

    /// Wiring every instance of a multi-instance kind is the OTHER answer, and
    /// the one the instance loop has to give: nothing is left to skip to.
    #[test]
    fn wiring_every_instance_leaves_none_free() {
        let mut mcu = create_stm32f103c8tx();
        let kind = ModuleKind::GenericInterfaceUsart;
        assert!(mcu.has_free_instance(kind));
        // Take them until the chip refuses.
        while mcu.add_module(kind) {}
        assert!(
            mcu.modules.iter().filter(|m| m.kind == kind).count() > 1,
            "the fixture really has several USARTs"
        );
        assert!(
            !mcu.has_free_instance(kind),
            "every instance is held by a module"
        );
    }

    /// The palette can never lie: whatever `can_add_module` promises,
    /// `add_module` delivers — driven to exhaustion across every kind.
    #[test]
    fn can_add_module_always_agrees_with_add_module() {
        let mut mcu = create_stm32f103c8tx();
        for _ in 0..12 {
            for kind in ModuleKind::ALL {
                let promised = mcu.can_add_module(kind);
                let actual = mcu.add_module(kind);
                assert_eq!(
                    promised,
                    actual,
                    "{}: palette promised {promised} but add_module returned {actual}",
                    kind.short()
                );
            }
        }
    }
}

// ── Bus-module style helpers (used by the staged-Apply flow) ──────────────────
use crate::panels::mcu_module::modules::{ApiStyle, AsyncBusMode, ModuleConfig};

/// The `(api_style, async_mode)` a bus module currently carries. USART has no
/// `async_mode` (its async form is always the embedded-io-async bridge), so it
/// reports `Blocking`; non-bus kinds report the defaults.
pub fn module_style(config: &ModuleConfig) -> (ApiStyle, AsyncBusMode) {
    match config {
        // LPUART is its own peripheral but shares `UsartModuleConfig`, so it has
        // an `api_style` like the rest - and its config panel draws the same
        // "Init API" row. Missing from here, `style_dirty` compared the staged
        // value against a hardcoded `Portable` and `set_module_style` wrote
        // nothing, so the row could be moved, the Apply bar lit up, and Apply
        // left the value exactly where it was: a bar that could never be
        // cleared and a control that could never take effect.
        ModuleConfig::Usart(c) | ModuleConfig::Lpuart(c) => (c.api_style, AsyncBusMode::Blocking),
        ModuleConfig::Spi(c) => (c.api_style, c.async_mode),
        ModuleConfig::I2c(c) => (c.api_style, c.async_mode),
        _ => (ApiStyle::Portable, AsyncBusMode::Blocking),
    }
}

/// Write a staged `(api_style, async_mode)` into a bus module's config.
fn set_module_style(config: &mut ModuleConfig, api: ApiStyle, async_mode: AsyncBusMode) {
    match config {
        ModuleConfig::Usart(c) | ModuleConfig::Lpuart(c) => c.api_style = api,
        ModuleConfig::Spi(c) => {
            c.api_style = api;
            c.async_mode = async_mode;
        }
        ModuleConfig::I2c(c) => {
            c.api_style = api;
            c.async_mode = async_mode;
        }
        _ => {}
    }
}

/// The staged Init-API / async-mode pair, and the two functions that move it.
///
/// `module_style` reads it out of a config and `set_module_style` writes it
/// back; the Apply bar is the only thing that calls either, and a config
/// neither of them knows about is a control the user can move and never apply.
/// The Apply bar and the question it asks live and die together.
/// A pin's user label goes out with the project and comes back the same.
#[cfg(test)]
mod a_pin_name_survives_a_save {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::pins::PinFunction;

    fn f103() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu()
    }

    /// The pad the label is on, whatever the chip numbers it.
    fn a_gpio(mcu: &Mcu) -> usize {
        mcu.iter_all_pins()
            .find(|p| !p.reserved && p.available_functions.contains(&PinFunction::GpioOutput))
            .map(|p| p.number)
            .expect("an output-capable pad")
    }

    /// "Status LED" is not an identifier, and the binding suffix was the only
    /// store: `sanitize_label` lowercased it and folded the space, so it came
    /// back "status_led" every time the project was opened.
    #[test]
    fn a_name_with_capitals_and_a_space_comes_back_intact() {
        let mut mcu = f103();
        let pad = a_gpio(&mcu);
        {
            let p = mcu.find_pin_mut(pad).expect("the pad");
            p.selected_function = PinFunction::GpioOutput;
            p.custom_label = "Status LED".into();
        }
        let cfg = mcu.mcu_config_text();

        // A fresh chip, as a project load gets it.
        let mut reopened = f103();
        reopened.apply_mcu_config(&cfg);
        reopened.apply_saved_pins(&[]); // the `reset_all_pins` a load runs
        reopened.apply_config_pin_labels(&cfg);

        assert_eq!(
            reopened.find_pin(pad).expect("the pad").custom_label,
            "Status LED"
        );
    }

    /// A pad with no FUNCTION has no binding, so it had nowhere to keep a name.
    ///
    /// This is how a Custom module's pads are named — in its own box, before
    /// they are given a function — so those names simply did not survive a save,
    /// and the module's `applied_sig` came back disagreeing with the field
    /// beside it.
    #[test]
    fn an_unset_pad_keeps_its_name_too() {
        let mut mcu = f103();
        let pad = a_gpio(&mcu);
        mcu.find_pin_mut(pad).expect("the pad").custom_label = "spare".into();
        assert_eq!(
            mcu.find_pin(pad).expect("the pad").selected_function,
            PinFunction::Unset,
            "no function, so no `let` line to hide the name in"
        );

        let cfg = mcu.mcu_config_text();
        let mut reopened = f103();
        reopened.apply_mcu_config(&cfg);
        reopened.apply_saved_pins(&[]);
        reopened.apply_config_pin_labels(&cfg);

        assert_eq!(
            reopened.find_pin(pad).expect("the pad").custom_label,
            "spare"
        );
    }

    /// A project that never named a pin writes no section, so it round-trips
    /// exactly as it did before the section existed.
    #[test]
    fn an_unnamed_project_is_unchanged() {
        let mcu = f103();
        assert!(
            !mcu.mcu_config_text().contains("@labels"),
            "nothing named, nothing written"
        );
    }

    /// A label for a pad this chip does not have is skipped, like every other
    /// restore here — a config file is something the user can edit.
    #[test]
    fn a_label_for_an_unknown_pad_is_skipped() {
        let mut mcu = f103();
        mcu.apply_config_pin_labels("@labels\n99999=ghost\n");
        assert!(
            mcu.iter_all_pins().all(|p| p.custom_label.is_empty()),
            "nothing was named"
        );
    }
}

/// The pin functions of an nRF project go out in `@pins` and come back by
/// number. The end-to-end reopen lives with the nRF backend (`nrf::pin_restore`);
/// this is the gate and the apply's edge cases.
#[cfg(test)]
mod the_pins_section {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::pins::PinFunction;

    fn chip(id: &str) -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == id)
            .unwrap_or_else(|| panic!("built-in {id}"))
            .build_mcu()
    }

    /// The pads that can be an output, in the chip's own order.
    fn gpios(mcu: &Mcu) -> Vec<usize> {
        mcu.iter_all_pins()
            .filter(|p| !p.reserved && p.available_functions.contains(&PinFunction::GpioOutput))
            .map(|p| p.number)
            .collect()
    }

    /// The write is gated to nRF: an STM32 project keeps reading its pins out
    /// of main.rs, and never carries a section its own open path would then
    /// prefer over that.
    #[test]
    fn other_families_do_not_write_the_section() {
        let mut mcu = chip("stm32f103c8t6");
        let pad = gpios(&mcu)[0];
        mcu.find_pin_mut(pad).expect("the pad").selected_function = PinFunction::GpioOutput;
        assert!(!mcu.mcu_config_text().contains("@pins"));
    }

    /// The apply is a clean slate: what the section does not name comes back
    /// Unset, so a pin wired in the previously open project cannot bleed into
    /// this one - the contract `apply_saved_pins` has always had.
    #[test]
    fn the_apply_starts_from_a_clean_slate() {
        let mut mcu = chip("nrf52833_microbit_v2");
        let [a, b, ..] = gpios(&mcu)[..] else {
            panic!("two output-capable pads")
        };
        mcu.find_pin_mut(a).expect("pad a").selected_function = PinFunction::GpioOutput;

        mcu.apply_saved_pins_by_number(&[(b, PinFunction::GpioInput)]);

        assert_eq!(
            mcu.find_pin(a).expect("pad a").selected_function,
            PinFunction::Unset
        );
        assert_eq!(
            mcu.find_pin(b).expect("pad b").selected_function,
            PinFunction::GpioInput
        );
    }

    /// A number this chip does not have (a project opened against a
    /// re-imported definition) is skipped, and a reserved pad is left as the
    /// definition set it, whatever the file says about it. The pad named
    /// beside them still loads.
    #[test]
    fn an_unknown_number_is_skipped_and_a_reserved_pad_is_left_alone() {
        let mut mcu = chip("nrf52833_microbit_v2");
        let reserved = mcu
            .iter_all_pins()
            .find(|p| p.reserved)
            .map(|p| (p.number, p.selected_function.clone()))
            .expect("a reserved pad");
        let pad = gpios(&mcu)[0];
        let unknown = mcu.iter_all_pins().map(|p| p.number).max().unwrap_or(0) + 1;

        mcu.apply_saved_pins_by_number(&[
            (unknown, PinFunction::GpioOutput),
            (reserved.0, PinFunction::GpioOutput),
            (pad, PinFunction::GpioOutput),
        ]);

        assert_eq!(
            mcu.find_pin(reserved.0)
                .expect("the reserved pad")
                .selected_function,
            reserved.1
        );
        assert_eq!(
            mcu.find_pin(pad).expect("the pad").selected_function,
            PinFunction::GpioOutput
        );
    }
}

#[cfg(test)]
mod the_confirm_does_not_outlive_the_bar {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::mcu::model::Runtime;

    fn f103() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu()
    }

    /// Staging a change, arming the confirm, then undoing the change BY HAND -
    /// which is neither Discard nor Cancel, and so cleared nothing.
    #[test]
    fn undoing_a_staged_change_by_hand_disarms_the_confirm() {
        let mut mcu = f103();
        mcu.pending_runtime = Runtime::Native;
        assert!(mcu.apply_bar_visible(), "the bar is up");

        // The user clicks Apply; the bar swaps to the confirm prompt.
        mcu.pending_apply_confirm = true;

        // ...and then puts the runtime back where it was instead of answering.
        mcu.pending_runtime = mcu.runtime;
        assert!(!mcu.apply_bar_visible(), "so the bar goes away");
        assert!(!mcu.pending_apply_confirm, "and takes its question with it");

        // The next staged change opens on the prompt, not mid-confirm.
        mcu.pending_runtime = Runtime::Native;
        assert!(mcu.apply_bar_visible());
        assert!(!mcu.pending_apply_confirm);
    }

    /// Staging the Native runtime is ONE change, and the bar says one.
    ///
    /// It used to say two. `normalize_gpio_api` ran every frame and forced
    /// `pending_gpio_api` to Native whenever the Native runtime was staged, so
    /// the bar read "2 staged changes - Runtime: Blocking -> Native AND GPIO
    /// In/Out: Portable -> Native" with the GPIO cards greyed out, i.e. a change
    /// the user had no way to make. Applying it then wrote `gpio_api = Native`
    /// for good, so going back to Blocking afterwards left GPIO bound raw.
    ///
    /// The forcing is gone: `gpio_native()` is already `is_native() || gpio_api
    /// == Native`, so every emitter was covered without it, and the System tab
    /// DERIVES what its two locked cards show.
    #[test]
    fn staging_the_native_runtime_is_one_change_not_two() {
        let mut mcu = f103();
        mcu.pending_runtime = Runtime::Native;
        let diff = mcu.style_diff_summary();
        assert_eq!(diff.len(), 1, "one staged change: {diff:?}");
        assert!(diff[0].starts_with("Runtime:"), "and it is the runtime");
        assert!(
            !diff.iter().any(|d| d.contains("GPIO")),
            "nothing about GPIO, which the user cannot even click here: {diff:?}"
        );
    }

    /// While something IS staged the confirm is left exactly as it was - this
    /// is a retirement, not a reset that would swallow the click that armed it.
    #[test]
    fn an_armed_confirm_survives_while_the_bar_is_up() {
        let mut mcu = f103();
        mcu.pending_runtime = Runtime::Native;
        mcu.pending_apply_confirm = true;
        assert!(mcu.apply_bar_visible());
        assert!(mcu.pending_apply_confirm, "still asking");
    }
}

#[cfg(test)]
mod a_staged_style_has_to_reach_the_config {
    use super::{module_style, set_module_style};
    use crate::panels::mcu_module::modules::{ApiStyle, AsyncBusMode, ModuleKind};

    /// Every config that HAS an `api_style` field must be reachable.
    ///
    /// The field list comes from the `Debug` derive, so this follows the structs
    /// rather than a list written beside them - which is how LPUART came to be
    /// missing. It shares `UsartModuleConfig` with USART, so it has the field
    /// and its panel draws the "Init API" row, but both functions fell through
    /// to the catch-all arm: `style_dirty` compared the staged value against a
    /// hardcoded `Portable` and stayed true for ever, so the Apply bar could
    /// never be cleared and the setting never took.
    #[test]
    fn every_config_with_an_init_api_can_be_staged_and_applied() {
        let mut with_api = 0usize;
        for kind in ModuleKind::ALL {
            let mut cfg = kind.default_config(1);
            let has_api = format!("{cfg:?}").contains("api_style");
            let has_async = format!("{cfg:?}").contains("async_mode");
            with_api += usize::from(has_api);

            set_module_style(&mut cfg, ApiStyle::Native, AsyncBusMode::AsyncDma);
            let (api, asyncm) = module_style(&cfg);

            assert_eq!(
                has_api,
                api == ApiStyle::Native,
                "{kind:?}: has an api_style field: {has_api}, applied: {api:?}"
            );
            assert_eq!(
                has_async,
                asyncm == AsyncBusMode::AsyncDma,
                "{kind:?}: has an async_mode field: {has_async}, applied: {asyncm:?}"
            );
        }
        assert_eq!(with_api, 4, "USART, LPUART, SPI and I2C");
    }

    /// An LPUART specifically, end to end - the case that could not be applied.
    #[test]
    fn an_lpuart_init_api_applies() {
        use crate::panels::mcu_module::modules::ModuleConfig;
        let mut cfg = ModuleKind::GenericInterfaceLpuart.default_config(1);
        assert_eq!(module_style(&cfg).0, ApiStyle::Portable, "the default");

        set_module_style(&mut cfg, ApiStyle::Native, AsyncBusMode::Blocking);

        assert_eq!(module_style(&cfg).0, ApiStyle::Native, "and it took");
        match &cfg {
            ModuleConfig::Lpuart(c) => assert_eq!(c.api_style, ApiStyle::Native),
            other => panic!("an LPUART config: {other:?}"),
        }
    }
}

#[cfg(test)]
mod moving_a_signal_to_another_pad {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::modules::{ModuleConfig, ModuleKind, ModuleSignal};
    use crate::panels::mcu_module::pins::PinFunction;

    /// A Pico, because the bundled F103 models no remap pads at all - each of
    /// its three USART TX signals sits on exactly one pad, so there is nowhere
    /// to move to and the case cannot be expressed there. On an RP the same
    /// UART reaches four pads.
    fn usart_mcu() -> crate::panels::mcu_module::mcu::Mcu {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        mcu
    }

    fn pin_of(mcu: &crate::panels::mcu_module::mcu::Mcu, sig: ModuleSignal) -> usize {
        mcu.modules[0]
            .connections
            .iter()
            .find(|c| c.signal == sig)
            .map(|c| c.mcu_pin)
            .expect("signal is wired")
    }

    /// The module survives the move with its config, and the wire does not
    /// double.
    ///
    /// Both halves are the reason this is not `apply_pin_function` twice:
    /// clearing first drags the partner to `Unset` and the re-created module
    /// gets a fresh `default_config`; setting first leaves two pads on one
    /// function and `reconcile_modules` makes two connections out of them.
    #[test]
    fn the_module_and_its_settings_come_along() {
        let mut mcu = usart_mcu();
        // A setting worth losing, so the test can see it survive.
        if let ModuleConfig::Usart(c) = &mut mcu.modules[0].config {
            c.baud_rate = 9600;
        }
        let tx = pin_of(&mcu, ModuleSignal::Tx);
        let rx = pin_of(&mcu, ModuleSignal::Rx);
        let want = mcu.find_pin(tx).expect("tx pin").selected_function.clone();

        // Somewhere else the same TX can go.
        let dest = mcu
            .iter_all_pins()
            .find(|p| {
                p.number != tx
                    && !p.reserved
                    && p.available_functions.contains(&want)
                    && p.selected_function == PinFunction::Unset
            })
            .map(|p| p.number)
            .expect("the F103 remaps USART TX to a second pad");

        assert!(mcu.move_pin_function(tx, dest));

        assert_eq!(mcu.modules.len(), 1, "still one module");
        assert_eq!(pin_of(&mcu, ModuleSignal::Tx), dest, "TX moved");
        assert_eq!(pin_of(&mcu, ModuleSignal::Rx), rx, "RX did not");
        assert_eq!(
            mcu.modules[0]
                .connections
                .iter()
                .filter(|c| c.signal == ModuleSignal::Tx)
                .count(),
            1,
            "one wire, not two"
        );
        assert_eq!(
            mcu.find_pin(tx).expect("old pad").selected_function,
            PinFunction::Unset,
            "the old pad is free again"
        );
        match &mcu.modules[0].config {
            ModuleConfig::Usart(c) => assert_eq!(c.baud_rate, 9600, "the config survived"),
            other => panic!("still a USART: {other:?}"),
        }
    }

    /// A destination that cannot carry the signal changes nothing at all.
    #[test]
    fn an_impossible_move_is_refused_whole() {
        let mut mcu = usart_mcu();
        let tx = pin_of(&mcu, ModuleSignal::Tx);
        let before: Vec<(usize, PinFunction)> = mcu
            .iter_all_pins()
            .map(|p| (p.number, p.selected_function.clone()))
            .collect();
        // A pad that offers no USART TX at all.
        let want = mcu.find_pin(tx).expect("tx").selected_function.clone();
        let bad = mcu
            .iter_all_pins()
            .find(|p| !p.reserved && !p.available_functions.contains(&want))
            .map(|p| p.number)
            .expect("some pad cannot carry a USART TX");
        assert!(!mcu.move_pin_function(tx, bad));
        let after: Vec<(usize, PinFunction)> = mcu
            .iter_all_pins()
            .map(|p| (p.number, p.selected_function.clone()))
            .collect();
        assert_eq!(before, after, "nothing moved");
        assert!(
            !mcu.move_pin_function(tx, tx),
            "a move onto itself is a no-op"
        );
    }
}

#[cfg(test)]
mod a_wiring_is_committed_whole_or_not_at_all {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::modules::ModuleSignal;
    use crate::panels::mcu_module::pins::PinFunction;

    /// `add_module_wired` is public and the dialog holds its choice across
    /// frames, so the check belongs in the model rather than only in the panel:
    /// a pad that cannot carry the signal must not be written at all.
    #[test]
    fn a_pad_that_cannot_carry_the_signal_is_refused() {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "stm32f103c8t6")
            .expect("built-in F103")
            .build_mcu();
        let want = ModuleSignal::Tx.pin_function(0);
        let bad = mcu
            .iter_all_pins()
            .find(|p| !p.reserved && !p.available_functions.contains(&want))
            .map(|p| p.number)
            .expect("a pad with no USART0 TX");

        assert!(!mcu.add_module_wired(0, &[(ModuleSignal::Tx, bad)]));
        assert_eq!(
            mcu.find_pin(bad).expect("the pad").selected_function,
            PinFunction::Unset,
            "nothing was written"
        );
        assert!(mcu.modules.is_empty(), "and no module appeared");
    }

    /// Whole or not at all: one bad pad must not leave the good ones written.
    #[test]
    fn one_bad_pad_rolls_the_whole_set_back() {
        let mut mcu = builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu();
        let tx_want = ModuleSignal::Tx.pin_function(0);
        let good = mcu
            .iter_all_pins()
            .find(|p| p.available_functions.contains(&tx_want))
            .map(|p| p.number)
            .expect("a UART0 TX pad");
        let rx_want = ModuleSignal::Rx.pin_function(0);
        let bad = mcu
            .iter_all_pins()
            .find(|p| !p.reserved && !p.available_functions.contains(&rx_want))
            .map(|p| p.number)
            .expect("a pad with no UART0 RX");

        assert!(!mcu.add_module_wired(0, &[(ModuleSignal::Tx, good), (ModuleSignal::Rx, bad)]));
        assert_eq!(
            mcu.find_pin(good).expect("the good pad").selected_function,
            PinFunction::Unset,
            "the pad that COULD have been written was not"
        );
    }
}

#[cfg(test)]
mod device_groups {
    use crate::panels::mcu_module::builtins::builtin_definitions;
    use crate::panels::mcu_module::mcu::Mcu;
    use crate::panels::mcu_module::mcu_config::PinGroup;
    use crate::panels::mcu_module::modules::{ModuleKind, ModuleSignal};
    use crate::panels::mcu_module::pins::PinFunction;

    fn pico() -> Mcu {
        builtin_definitions()
            .into_iter()
            .find(|d| d.id == "rp2040_pico")
            .expect("built-in Pico")
            .build_mcu()
    }

    /// Three free pads that can all host a plain output, so a move between two of
    /// them is legal on the silicon.
    fn three_output_pads(mcu: &Mcu) -> (usize, usize, usize) {
        let free: Vec<usize> = mcu
            .iter_all_pins()
            .filter(|p| {
                !p.reserved
                    && p.selected_function == PinFunction::Unset
                    && p.available_functions.contains(&PinFunction::GpioOutput)
            })
            .map(|p| p.number)
            .take(3)
            .collect();
        assert_eq!(free.len(), 3, "the Pico has three free GPIOs");
        (free[0], free[1], free[2])
    }

    fn named(mcu: &Mcu, name: &str) -> Option<Vec<usize>> {
        mcu.groups
            .iter()
            .find(|g| g.name == name)
            .map(|g| g.pins.iter().copied().collect())
    }

    /// A pad belongs to ONE device. Two accent colours on one stub would read as
    /// a drawing error, and `join_group` finds a group by name - so a pad in two
    /// of them would answer to whichever came first.
    #[test]
    fn a_pad_belongs_to_one_device_at_a_time() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.join_group(8, "radar");
        mcu.join_group(7, "display");
        assert_eq!(named(&mcu, "radar"), Some(vec![8]));
        assert_eq!(named(&mcu, "display"), Some(vec![7]));
    }

    /// The empty name is how a pad leaves - the roster's × and the same call.
    /// The device it leaves behind disappears only when nothing is left in it.
    #[test]
    fn the_last_pad_out_takes_the_device_with_it() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.join_group(8, "radar");
        mcu.join_group(7, "");
        assert_eq!(
            named(&mcu, "radar"),
            Some(vec![8]),
            "one pad left, still a device"
        );
        mcu.join_group(8, "");
        assert!(mcu.groups.is_empty(), "nothing left, no device");
    }

    /// A device created from the roster is empty until the next gesture fills
    /// it. Grouping something ELSE in between must not sweep it away - the row
    /// vanishing under the user's cursor is indistinguishable from a bug.
    #[test]
    fn a_device_with_nothing_in_it_yet_survives_the_next_grouping() {
        let mut mcu = pico();
        mcu.new_group("Device 2".into());
        mcu.join_group(7, "Device 1");
        assert!(
            mcu.groups.iter().any(|g| g.name == "Device 2"),
            "the unfilled device is still on the roster"
        );
    }

    /// Moving a pad WITHIN its own device empties the group in passing. It must
    /// not be collected as a casualty of that.
    #[test]
    fn regrouping_a_pad_into_its_own_device_keeps_it() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.join_group(7, "radar");
        assert_eq!(named(&mcu, "radar"), Some(vec![7]));
    }

    /// Renaming onto a name already taken MERGES: `join_group` looks a group up
    /// by name, so two rows sharing one would draw one colour and only ever fill
    /// one of them.
    #[test]
    fn renaming_onto_a_taken_name_merges_the_two() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.join_group(8, "sensor");
        mcu.rename_group(1, "radar");
        assert_eq!(mcu.groups.len(), 1);
        assert_eq!(named(&mcu, "radar"), Some(vec![7, 8]));
    }

    /// A group is a set of PAD NUMBERS, so a signal that changes pad would drop
    /// out of its device unless the move rewrites the set. `move_pin_function`
    /// is the only place in the app where a signal changes pad.
    #[test]
    fn a_device_follows_its_pad_across_a_move() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let tx = mcu.modules[0]
            .pin_for(ModuleSignal::Tx)
            .expect("the UART got a TX pad");
        let want = mcu
            .find_pin(tx)
            .expect("the TX pad")
            .selected_function
            .clone();
        let dest = mcu
            .iter_all_pins()
            .find(|p| {
                p.number != tx
                    && !p.reserved
                    && p.available_functions.contains(&want)
                    && p.selected_function == PinFunction::Unset
            })
            .map(|p| p.number)
            .expect("the RP maps the same TX to a second pad");

        mcu.join_group(tx, "radar");
        assert!(mcu.move_pin_function(tx, dest));
        assert_eq!(
            named(&mcu, "radar"),
            Some(vec![dest]),
            "the device followed the signal to its new pad"
        );
    }

    /// The pad tick, the box bar and the io bar all ask `group_of_pin`, so it has
    /// to answer with the same predicate everything else uses. A device whose name
    /// the user cleared draws no mat and writes no comment — it may not keep
    /// marking its pads either.
    #[test]
    fn a_nameless_device_marks_none_of_its_pads() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        assert!(mcu.group_of_pin(7).is_some());
        mcu.rename_group(0, "  ");
        assert_eq!(mcu.groups.len(), 1, "still on the roster");
        assert!(
            mcu.group_of_pin(7).is_none(),
            "but nothing on the canvas answers for it"
        );
    }

    /// The explicit answer outranks both derived ones, and the module outranks
    /// the pin — a tab click has to be able to override a pad that is still
    /// selected from before.
    #[test]
    fn active_device_prefers_explicit_then_module_then_pin() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let bus_pad = m.connections[0].mcu_pin;
        let loose = mcu
            .iter_all_pins()
            .map(|p| p.number)
            .find(|n| !m.connections.iter().any(|c| c.mcu_pin == *n))
            .expect("a pad the bus does not use");

        mcu.join_group_module(&m, "by module");
        mcu.join_group(loose, "by pin");
        mcu.new_group("by tab".into());
        mcu.join_group(bus_pad, "by module");

        mcu.selected_pin = Some(loose);
        assert_eq!(mcu.active_device(), Some("by pin"));

        mcu.selected_module = Some(m.id.clone());
        assert_eq!(
            mcu.active_device(),
            Some("by module"),
            "the module outranks the pin"
        );

        // Give "by tab" a pad of its own, so it is a live device.
        mcu.join_group(loose, "by tab");
        mcu.selected_device = Some("by tab".into());
        assert_eq!(
            mcu.active_device(),
            Some("by tab"),
            "explicit outranks both"
        );
    }

    /// A device can be renamed or dissolved from the roster while its name is
    /// still stored here. A stale name that merely suppressed the derivation
    /// would leave the canvas lighting nothing at all, with no way to notice.
    #[test]
    fn active_device_ignores_a_dead_name() {
        let mut mcu = pico();
        // TWO devices, and the dead name is neither — so falling through to the
        // derivation and falling onto whichever device happens to be first are
        // different answers.
        mcu.join_group(8, "display");
        mcu.join_group(7, "radar");
        assert_eq!(mcu.groups[0].name, "display", "display is the first row");
        mcu.selected_pin = Some(7);
        mcu.selected_device = Some("dissolved long ago".into());
        assert_eq!(mcu.active_device(), Some("radar"), "the PIN's device");
    }

    /// Three selections now, and a fourth is plausible. A clearing site that
    /// forgets one leaves the canvas lit for something the user stopped looking
    /// at.
    #[test]
    fn clear_canvas_selection_drops_all_three() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.selected_pin = Some(7);
        mcu.selected_module = Some("whatever".into());
        mcu.selected_device = Some("radar".into());
        mcu.collapse_modules = false;

        mcu.clear_canvas_selection();

        assert!(mcu.selected_pin.is_none());
        assert!(mcu.selected_module.is_none());
        assert!(mcu.selected_device.is_none());
        assert!(mcu.collapse_modules, "and the list is told to agree");
        assert_eq!(mcu.active_device(), None);
    }

    /// A device dragged as one has to be resettable as one, or the user is left
    /// hunting for every part they moved.
    #[test]
    fn resetting_a_device_returns_every_part_of_it_to_auto() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let spare = mcu
            .iter_all_pins()
            .map(|p| p.number)
            .find(|n| !m.connections.iter().any(|c| c.mcu_pin == *n))
            .expect("a free pad");
        mcu.join_group_module(&m, "radar");
        mcu.join_group(spare, "radar");

        assert!(
            !mcu.device_is_manual("radar"),
            "everything starts auto-packed"
        );
        mcu.modules[0].pos = (40.0, -30.0);
        mcu.io_pin_pos.insert(spare, (10.0, 10.0));
        assert!(mcu.device_is_manual("radar"));

        mcu.reset_device_position("radar");

        assert_eq!(mcu.modules[0].pos, (0.0, 0.0), "the box is auto again");
        assert!(!mcu.io_pin_pos.contains_key(&spare), "and so is the field");
        assert!(!mcu.device_is_manual("radar"));
    }

    /// Resetting one device leaves another device's hand-placed parts alone.
    #[test]
    fn resetting_a_device_leaves_the_other_devices_where_they_are() {
        let mut mcu = pico();
        // A MODULE each, not just a pad each: the module half of the reset has
        // its own filter, and a test with no modules never runs it.
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        assert!(mcu.add_module(ModuleKind::GenericInterfaceSpi));
        let (uart, spi) = (mcu.modules[0].clone(), mcu.modules[1].clone());
        mcu.join_group_module(&uart, "radar");
        mcu.join_group_module(&spi, "display");
        mcu.modules[0].pos = (5.0, 5.0);
        mcu.modules[1].pos = (9.0, 9.0);

        mcu.reset_device_position("radar");

        assert_eq!(mcu.modules[0].pos, (0.0, 0.0), "radar's box is auto again");
        assert_eq!(
            mcu.modules[1].pos,
            (9.0, 9.0),
            "and display's box has not moved"
        );
    }

    /// `free_module_id` hands a freed id straight to the next module, so every
    /// bit of view state keyed on one has to die WITH the module. A survivor
    /// does not go stale — it lands on a stranger.
    #[test]
    fn every_bit_keyed_on_a_dead_modules_id_is_retired_with_it() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let tx = m.pin_for(ModuleSignal::Tx).expect("a TX pad");
        mcu.selected_module = Some(m.id.clone());
        mcu.module_remove_confirm = Some(m.id.clone());
        mcu.pending_module_styles.insert(
            m.id.clone(),
            (
                crate::panels::mcu_module::modules::ApiStyle::Native,
                crate::panels::mcu_module::modules::AsyncBusMode::Blocking,
            ),
        );

        // Re-purpose a bus pad: `reconcile_modules` drops the module.
        mcu.apply_pin_function(tx, PinFunction::Unset);
        assert!(mcu.modules.is_empty(), "the module really is gone");

        assert!(mcu.selected_module.is_none(), "no box is called out");
        assert!(
            mcu.module_remove_confirm.is_none(),
            "no question is pending"
        );
        assert!(
            mcu.pending_module_styles.is_empty(),
            "and nothing is staged for a module that does not exist"
        );
    }

    /// …and a LIVE module keeps every one of them.
    #[test]
    fn a_live_modules_state_is_left_alone() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let id = mcu.modules[0].id.clone();
        mcu.selected_module = Some(id.clone());
        mcu.module_remove_confirm = Some(id.clone());
        mcu.reconcile_modules();
        assert_eq!(mcu.selected_module.as_deref(), Some(id.as_str()));
        assert_eq!(mcu.module_remove_confirm.as_deref(), Some(id.as_str()));
    }

    /// A pad freed by ANY route starts clean — the rule `apply_pin_function`
    /// states on its `Unset` branch, which two other routes used to skip.
    #[test]
    fn a_pad_freed_by_any_route_loses_the_name_typed_on_it() {
        let named = |mcu: &mut Mcu, p: usize| {
            mcu.apply_pin_function(p, PinFunction::GpioOutput);
            if let Some(x) = mcu.find_pin_mut(p) {
                x.custom_label = "led".into();
            }
        };
        let label = |mcu: &Mcu, p: usize| mcu.find_pin(p).expect("the pad").custom_label.clone();

        // Reset all pins.
        let mut mcu = pico();
        named(&mut mcu, 7);
        mcu.reset_all_pins();
        assert_eq!(label(&mcu, 7), "", "the total wipe wipes the name too");

        // Removing the module that held the pad.
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let tx = m.pin_for(ModuleSignal::Tx).expect("a TX pad");
        if let Some(x) = mcu.find_pin_mut(tx) {
            x.custom_label = "radar".into();
        }
        mcu.remove_module(&m.id);
        assert_eq!(label(&mcu, tx), "", "the freed bus pad starts clean");
    }

    /// Moving a signal takes its NAME with it. The label names the binding, so
    /// clearing it loses what the user typed and leaving it strands the name on
    /// a pad that no longer carries the signal.
    #[test]
    fn moving_a_signal_carries_its_name_to_the_new_pad() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let tx = m.pin_for(ModuleSignal::Tx).expect("a TX pad");
        let want = mcu.find_pin(tx).expect("the pad").selected_function.clone();
        let dest = mcu
            .iter_all_pins()
            .find(|p| {
                p.number != tx
                    && !p.reserved
                    && p.available_functions.contains(&want)
                    && p.selected_function == PinFunction::Unset
            })
            .map(|p| p.number)
            .expect("a second TX pad");
        if let Some(x) = mcu.find_pin_mut(tx) {
            x.custom_label = "radar".into();
        }

        assert!(mcu.move_pin_function(tx, dest));

        assert_eq!(mcu.find_pin(dest).expect("dest").custom_label, "radar");
        assert_eq!(
            mcu.find_pin(tx).expect("source").custom_label,
            "",
            "and it does not stay behind"
        );
    }

    /// "Reset pins" is the most destructive act the panel offers, and Ctrl+Z has
    /// to bring it back. The snapshot covers exactly what the reset throws away:
    /// every pin's function and label, and every module.
    #[test]
    fn resetting_every_pin_can_be_undone() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        let tx = m.pin_for(ModuleSignal::Tx).expect("a TX pad");
        let spare = mcu
            .iter_all_pins()
            .map(|p| p.number)
            .find(|n| !m.connections.iter().any(|c| c.mcu_pin == *n))
            .expect("a free pad");
        mcu.apply_pin_function(spare, PinFunction::GpioOutput);
        if let Some(p) = mcu.find_pin_mut(spare) {
            p.custom_label = "led".into();
        }
        let before: Vec<PinFunction> = mcu
            .iter_all_pins()
            .map(|p| p.selected_function.clone())
            .collect();

        mcu.push_module_undo("Reset pins".to_owned());
        mcu.reset_all_pins();
        assert!(
            mcu.iter_all_pins()
                .all(|p| p.reserved || p.selected_function == PinFunction::Unset),
            "the reset really did clear them"
        );

        assert_eq!(mcu.undo_modules().as_deref(), Some("Reset pins"));

        let after: Vec<PinFunction> = mcu
            .iter_all_pins()
            .map(|p| p.selected_function.clone())
            .collect();
        assert_eq!(after, before, "every pin came back");
        assert_eq!(
            mcu.find_pin(spare).expect("the spare pad").custom_label,
            "led",
            "and so did its name"
        );
        assert_eq!(mcu.modules.len(), 1);
        assert_eq!(mcu.modules[0].pin_for(ModuleSignal::Tx), Some(tx));
    }

    /// Folding a module's config from the list drops the white border its box
    /// wears on the canvas — and touches nothing else.
    #[test]
    fn folding_a_config_stops_the_canvas_calling_that_box_out() {
        let mut mcu = pico();
        mcu.selected_module = Some("usart1".into());
        mcu.selected_pin = Some(7);

        mcu.config_collapsed("usart1");

        assert!(mcu.selected_module.is_none());
        assert_eq!(
            mcu.selected_pin,
            Some(7),
            "the pin selection is not its business"
        );
    }

    /// Closing EVERY config takes the white border with them, exactly as
    /// closing one does.
    ///
    /// The panel's expand caret set `collapse_modules` on its own, which is only
    /// half of it: the panel came back with every config folded and a box on the
    /// canvas still picked out in white, and the only way to clear it was to
    /// click that box twice.
    #[test]
    fn closing_every_config_stops_the_canvas_calling_any_box_out() {
        let mut mcu = pico();
        mcu.selected_module = Some("usart1".into());
        mcu.selected_pin = Some(7);
        mcu.selected_device = Some("radar".into());

        mcu.all_configs_collapsed();

        assert!(mcu.collapse_modules, "the list is told to fold them");
        assert!(mcu.selected_module.is_none(), "and the box goes unlit");
        assert_eq!(
            mcu.selected_pin,
            Some(7),
            "a selected PIN is not a config and is left alone"
        );
        assert_eq!(
            mcu.selected_device.as_deref(),
            Some("radar"),
            "and neither is a selected device"
        );
    }

    /// Clicking empty canvas still drops all three selections — it is the wider
    /// gesture, and it is built on the narrower one.
    #[test]
    fn clicking_empty_canvas_drops_every_selection() {
        let mut mcu = pico();
        mcu.selected_module = Some("usart1".into());
        mcu.selected_pin = Some(7);
        mcu.selected_device = Some("radar".into());

        mcu.clear_canvas_selection();

        assert!(mcu.collapse_modules);
        assert!(mcu.selected_module.is_none());
        assert!(mcu.selected_pin.is_none());
        assert!(mcu.selected_device.is_none());
    }

    /// Folding ONE config says nothing about another.
    #[test]
    fn folding_someone_elses_config_leaves_the_selection_alone() {
        let mut mcu = pico();
        mcu.selected_module = Some("usart1".into());
        mcu.config_collapsed("spi0");
        assert_eq!(mcu.selected_module.as_deref(), Some("usart1"));
    }

    /// A device lit only because its module was selected goes quiet with it —
    /// `active_device` derives, so there is no second thing to clear.
    #[test]
    fn folding_a_config_also_quiets_the_device_it_lit() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        mcu.join_group_module(&m, "radar");
        mcu.selected_module = Some(m.id.clone());
        assert_eq!(mcu.active_device(), Some("radar"));

        mcu.config_collapsed(&m.id);

        assert_eq!(mcu.active_device(), None);
    }

    /// A pad added under a padded spelling of a device's name joins THAT device
    /// rather than starting a second one beside it.
    #[test]
    fn a_pad_joins_a_device_whose_name_differs_only_in_padding() {
        let mut mcu = pico();
        mcu.join_group(7, "mw radar");
        mcu.join_group(8, " mw radar ");
        assert_eq!(mcu.groups.len(), 1);
        assert_eq!(named(&mcu, "mw radar"), Some(vec![7, 8]));
    }

    /// Membership is derived for a MODULE, never stored: `reconcile_modules`
    /// deletes and re-creates a module under a fresh id on an ordinary edit, and
    /// a stored id would lose its device every time.
    #[test]
    fn a_module_is_in_the_device_holding_any_of_its_pads() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        assert!(mcu.group_of_module(&m).is_none());
        mcu.join_group(m.connections[0].mcu_pin, "radar");
        assert_eq!(
            mcu.group_of_module(&m).map(|g| g.name.as_str()),
            Some("radar")
        );
        // …and the roster's gesture puts the WHOLE bus in.
        mcu.join_group_module(&m, "radar");
        assert_eq!(
            named(&mcu, "radar").map(|v| v.len()),
            Some(m.connections.len())
        );
    }

    /// The whole point of keying by pad: a device outlives the module that
    /// carried it, because it never knew the module's id.
    #[test]
    fn a_device_outlives_the_module_id_it_was_grouped_through() {
        let mut mcu = pico();
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        let m = mcu.modules[0].clone();
        mcu.join_group_module(&m, "radar");
        let before = m.id.clone();
        mcu.remove_module(&before);
        assert!(mcu.add_module(ModuleKind::GenericInterfaceUsart));
        // Same pads, and the device is still on them.
        let again = mcu.modules[0].clone();
        assert_eq!(
            mcu.group_of_module(&again).map(|g| g.name.as_str()),
            Some("radar"),
            "the re-created module is still part of the device"
        );
    }

    /// A pad keeps its device when its function goes away, so the destination of
    /// a move is not necessarily device-free. Handing it the mover's device
    /// without taking it out of its own left one pad in two devices at once, and
    /// `group_of_pin` then answered by Vec order.
    #[test]
    fn a_move_never_leaves_a_pad_in_two_devices() {
        let mut mcu = pico();
        let (from, to, spare) = three_output_pads(&mcu);
        mcu.apply_pin_function(from, PinFunction::GpioOutput);
        mcu.join_group(from, "radar");
        // The destination carries no function, but does carry a device.
        mcu.join_group(to, "display");
        mcu.join_group(spare, "display");

        assert!(mcu.move_pin_function(from, to));

        let holders: Vec<&str> = mcu
            .groups
            .iter()
            .filter(|g| g.pins.contains(&to))
            .map(|g| g.name.as_str())
            .collect();
        assert_eq!(
            holders,
            ["radar"],
            "the destination is in exactly one device"
        );
        assert_eq!(named(&mcu, "radar"), Some(vec![to]));
        assert_eq!(
            named(&mcu, "display"),
            Some(vec![spare]),
            "and display kept the rest"
        );
    }

    /// The device the move empties disappears — but only that one.
    #[test]
    fn a_move_onto_a_devices_last_pad_retires_that_device() {
        let mut mcu = pico();
        let (from, to, _) = three_output_pads(&mcu);
        mcu.apply_pin_function(from, PinFunction::GpioOutput);
        mcu.join_group(from, "radar");
        mcu.join_group(to, "display");
        mcu.new_group("unfilled".into());

        assert!(mcu.move_pin_function(from, to));

        assert!(named(&mcu, "display").is_none(), "it had only that pad");
        assert_eq!(named(&mcu, "radar"), Some(vec![to]));
        assert!(
            mcu.groups.iter().any(|g| g.name == "unfilled"),
            "a device the user has not filled yet is not swept up"
        );
    }

    /// A device the user has not named is not a device yet: it stays on the
    /// roster and reaches neither `mcu.config` nor the generated comment. The
    /// three used to disagree, so an unnamed device was written into main.rs as
    /// a nameless `// : PA4, PA5` and then lost on the next save.
    #[test]
    fn an_unnamed_device_reaches_neither_the_file_nor_the_comment() {
        let mut mcu = pico();
        mcu.join_group(7, "radar");
        mcu.rename_group(0, "");
        assert_eq!(mcu.groups.len(), 1, "still on the roster");
        assert!(!mcu.groups[0].is_live());
        assert!(!mcu.mcu_config_text().contains("@groups"));
        assert_eq!(
            crate::panels::mcu_module::codegen::common::device_comment(&mcu),
            ""
        );
    }

    /// `mcu.config` is the only place a device is stored, so the section has to
    /// survive the app's own write-then-read.
    #[test]
    fn a_device_round_trips_through_mcu_config() {
        let mut mcu = pico();
        mcu.join_group(7, "mw radar");
        mcu.join_group(8, "mw radar");
        let text = mcu.mcu_config_text();
        let mut back = pico();
        back.apply_mcu_config(&text);
        assert_eq!(
            back.groups,
            vec![PinGroup {
                name: "mw radar".into(),
                pins: [7, 8].into_iter().collect(),
                ..Default::default()
            }]
        );
    }
}
