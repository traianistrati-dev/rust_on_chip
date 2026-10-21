//! Deterministic clock-tree import from an STM32CubeMX installation (Phase 7).
//!
//! **Verified against a real install** (`STM32CubeMX/db`, 67 family files): the
//! clock tree ST draws in CubeMX is shipped as data, and it carries everything
//! this IDE's model needs — including the node coordinates, so the imported
//! diagram is ST's own layout rather than a computed one.
//!
//! Two files are joined, exactly like the pin importer joins `mcu/*.xml` with
//! its IP files:
//!
//! - `db/plugins/clock/STM32<FAM>.xml` — the TOPOLOGY. Each `<Element id type
//!   refParameter x y>` is a node; each `<Input signalId from refValue/>` is a
//!   wire from another element, and `refValue` names the register value that
//!   selects it on a mux — so the input INDEX comes from the register, not from
//!   the order the file happens to list them in.
//! - `db/mcu/IP/RCC-STM32<FAM>_rcc_*_Modes.xml` — the VALUES. `refParameter`
//!   points at a `<RefParameter>` there, which holds either a `<PossibleValue
//!   Comment="4" Value="RCC_SYSCLK_DIV4"/>` list (divider options, mux inputs)
//!   or a `Min`/`Max` integer range (PLL multipliers), plus the reset default.
//!
//! `<Condition Expression="STM32WBAx4|STM32WBAx5"/>` gates elements per chip
//! variant, so the same family file serves several parts. [`Variant`] evaluates
//! those expressions against the tokens a chip defines; an element whose
//! condition is false is left out rather than imported as a dead node.
//!
//! This is the deterministic counterpart to [`super::extract_tree`]: where an
//! ST-supported chip is concerned, this is the accurate path and the AI passes
//! are the fallback for everything else.

use std::collections::BTreeMap;

use super::layout::NodeBox;
use super::model::{ClockGraph, Edge, LimitKey, Node, NodeKind, NodeState};

// ── RCC parameters (the value half) ──────────────────────────────────────────

/// One `<RefParameter>`: either a list of selectable values or an integer range.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Param {
    /// `(Value, Comment)` pairs in file order — `Value` is the register constant
    /// an `<Input refValue=…>` matches, `Comment` the human number (`"4"`).
    pub values: Vec<(String, String)>,
    pub min: Option<u32>,
    pub max: Option<u32>,
    pub default: String,
    /// `Unit="KHz"` / `"MHz"` — what the `Comment` numbers are expressed in when
    /// the parameter is a FREQUENCY list (MSI's range enum). Absent for the
    /// dividers, whose Comments are plain factors.
    pub unit: Option<String>,
}

impl Param {
    /// The `Comment`s parsed as divisors — CubeMX writes the numeric factor
    /// there (`Comment="4"` for `RCC_SYSCLK_DIV4`).
    fn divisors(&self) -> Vec<u32> {
        self.values
            .iter()
            .filter_map(|(_, c)| c.trim().parse::<u32>().ok())
            .filter(|d| *d > 0)
            .collect()
    }

    /// Index of the reset default within `values`.
    fn default_index(&self) -> usize {
        self.values
            .iter()
            .position(|(v, _)| *v == self.default)
            .unwrap_or(0)
    }

    /// The `Comment`s read as FREQUENCIES, honouring `Unit` — MSI's range enum
    /// lists `Comment="4000"` with `Unit="KHz"`, i.e. 4 MHz.
    fn frequencies(&self) -> Vec<u32> {
        let scale: u64 = match self.unit.as_deref().map(str::trim) {
            Some(u) if u.eq_ignore_ascii_case("KHz") => 1_000,
            Some(u) if u.eq_ignore_ascii_case("MHz") => 1_000_000,
            _ => 1,
        };
        self.values
            .iter()
            .filter_map(|(_, c)| c.trim().parse::<u64>().ok())
            .map(|v| (v * scale).min(u32::MAX as u64) as u32)
            .collect()
    }
}

/// Every `<RefParameter>` in an RCC IP file, by name.
pub type RccParams = BTreeMap<String, Param>;

/// Parse `db/mcu/IP/RCC-*_Modes.xml`.
pub fn parse_rcc_params(xml: &str) -> Result<RccParams, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("RCC XML parse error: {e}"))?;
    let mut out = RccParams::new();
    for p in doc.descendants().filter(|n| n.has_tag_name("RefParameter")) {
        let Some(name) = p.attribute("Name") else {
            continue;
        };
        out.insert(
            name.to_owned(),
            Param {
                values: p
                    .children()
                    .filter(|c| c.has_tag_name("PossibleValue"))
                    .filter_map(|c| {
                        Some((
                            c.attribute("Value")?.to_owned(),
                            c.attribute("Comment").unwrap_or_default().to_owned(),
                        ))
                    })
                    .collect(),
                min: p.attribute("Min").and_then(|v| v.trim().parse().ok()),
                max: p.attribute("Max").and_then(|v| v.trim().parse().ok()),
                default: p.attribute("DefaultValue").unwrap_or_default().to_owned(),
                unit: p.attribute("Unit").map(str::to_owned),
            },
        );
    }
    if out.is_empty() {
        return Err("no <RefParameter> found — is this an RCC *_Modes.xml?".into());
    }
    Ok(out)
}

// ── Variant conditions ───────────────────────────────────────────────────────

/// The tokens a particular chip defines (`STM32WBAx5`, `SAI1_Exist`, …), used to
/// evaluate the `<Condition Expression>` gates.
///
/// Unknown tokens are FALSE: a family file mentions peripherals a given part may
/// not have, and importing a branch for hardware that isn't there is worse than
/// leaving it out (the user can always add it in the editor).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Variant {
    pub defines: Vec<String>,
}

impl Variant {
    pub fn new<I: IntoIterator<Item = S>, S: Into<String>>(defines: I) -> Self {
        Self {
            defines: defines.into_iter().map(Into::into).collect(),
        }
    }

    /// Evaluate a CubeMX condition: identifiers combined with `&`, `|`, `!` and
    /// parentheses. `&` binds tighter than `|`, as in C. An empty expression is
    /// true (the element is unconditional).
    pub fn eval(&self, expr: &str) -> bool {
        let expr = expr.trim();
        if expr.is_empty() {
            return true;
        }
        let toks: Vec<char> = expr.chars().collect();
        let mut pos = 0;
        self.or_expr(&toks, &mut pos)
    }

    fn or_expr(&self, t: &[char], i: &mut usize) -> bool {
        let mut v = self.and_expr(t, i);
        while self.peek(t, i) == Some('|') {
            *i += 1;
            v |= self.and_expr(t, i);
        }
        v
    }

    fn and_expr(&self, t: &[char], i: &mut usize) -> bool {
        let mut v = self.unary(t, i);
        while self.peek(t, i) == Some('&') {
            *i += 1;
            v &= self.unary(t, i);
        }
        v
    }

    fn unary(&self, t: &[char], i: &mut usize) -> bool {
        match self.peek(t, i) {
            Some('!') => {
                *i += 1;
                !self.unary(t, i)
            }
            Some('(') => {
                *i += 1;
                let v = self.or_expr(t, i);
                if self.peek(t, i) == Some(')') {
                    *i += 1;
                }
                v
            }
            _ => {
                let mut id = String::new();
                while *i < t.len() && (t[*i].is_alphanumeric() || t[*i] == '_' || t[*i] == '.') {
                    id.push(t[*i]);
                    *i += 1;
                }
                // A malformed expression yields an empty identifier; skip the
                // offending char so evaluation always terminates.
                if id.is_empty() && *i < t.len() {
                    *i += 1;
                    return false;
                }
                self.defines.iter().any(|d| *d == id)
            }
        }
    }

    /// Skip whitespace and report the next character — advancing `i` past the
    /// spaces, so the identifier reader that follows starts on real input.
    fn peek(&self, t: &[char], i: &mut usize) -> Option<char> {
        while *i < t.len() && t[*i].is_whitespace() {
            *i += 1;
        }
        t.get(*i).copied()
    }
}

// ── The clock tree (the topology half) ───────────────────────────────────────

/// One imported element, before it becomes a node.
struct RawElement<'a> {
    id: &'a str,
    kind: &'a str,
    param: Option<&'a str>,
    x: f32,
    y: f32,
    /// `(from, refValue)` per `<Input>`, in file order.
    inputs: Vec<(&'a str, Option<&'a str>)>,
}

/// Import a CubeMX clock tree: the graph plus ST's own node positions.
///
/// `variant` decides which conditional elements belong to this chip. Elements
/// whose `refParameter` is missing from `rcc` still import — they simply get a
/// pass-through/default shape, which is better than dropping a branch.
pub fn parse_clock_tree(
    clock_xml: &str,
    rcc: &RccParams,
    variant: &Variant,
) -> Result<(ClockGraph, Vec<NodeBox>), String> {
    let doc =
        roxmltree::Document::parse(clock_xml).map_err(|e| format!("clock XML parse error: {e}"))?;

    // 1. Collect the elements this variant has.
    let mut raw: Vec<RawElement> = Vec::new();
    for el in doc.descendants().filter(|n| n.has_tag_name("Element")) {
        let keep = el
            .children()
            .filter(|c| c.has_tag_name("Condition"))
            .all(|c| variant.eval(c.attribute("Expression").unwrap_or_default()));
        if !keep {
            continue;
        }
        let (Some(id), Some(kind)) = (el.attribute("id"), el.attribute("type")) else {
            continue;
        };
        // The same id can appear twice under mutually exclusive conditions
        // (LSIRC on WBAx4 vs the rest); the first one this variant matches wins.
        if raw.iter().any(|r| r.id == id) {
            continue;
        }
        raw.push(RawElement {
            id,
            kind,
            param: el.attribute("refParameter"),
            x: el
                .attribute("x")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            y: el
                .attribute("y")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0.0),
            inputs: el
                .children()
                .filter(|c| c.has_tag_name("Input"))
                .filter(|c| {
                    c.children()
                        .filter(|g| g.has_tag_name("Condition"))
                        .all(|g| variant.eval(g.attribute("Expression").unwrap_or_default()))
                })
                .filter_map(|c| Some((c.attribute("from")?, c.attribute("refValue"))))
                .collect(),
        });
    }
    if raw.is_empty() {
        return Err("no clock elements found — is this a db/plugins/clock file?".into());
    }

    // 2. Elements → nodes.
    let mut nodes = Vec::with_capacity(raw.len());
    let mut boxes = Vec::with_capacity(raw.len());
    for r in &raw {
        let p = r.param.and_then(|name| rcc.get(name));
        let (kind, state) = shape(r, p);
        nodes.push(Node {
            id: r.id.to_owned(),
            kind,
            state,
            limit: None,
        });
        boxes.push(NodeBox {
            node: r.id.to_owned(),
            x: r.x,
            y: r.y,
            w: 96.0,
            h: 26.0,
        });
    }

    // 3. Inputs → edges. A mux input's index comes from the REGISTER value that
    //    selects it, so a file listing them in a different order still wires up
    //    correctly; without a `refValue` we fall back to file order.
    let mut edges = Vec::new();
    for r in &raw {
        let p = r.param.and_then(|name| rcc.get(name));
        for (n, (from, ref_value)) in r.inputs.iter().enumerate() {
            if !raw.iter().any(|o| o.id == *from) {
                continue; // fed by an element this variant does not have
            }
            // `xbar` belongs here for the same reason it belongs in the node
            // kind: it IS a mux. Giving its inputs the index 0 apiece left the
            // evaluator unable to find any input but the first, so an IC set to
            // PLL2 evaluated to 0 Hz — and on STM32N6 the vendor's own default
            // tree puts four ICs on PLL2 and two on PLL3.
            let input = match (r.kind, ref_value, p) {
                ("multiplexor" | "xbar", Some(rv), Some(p)) => {
                    p.values.iter().position(|(v, _)| v == rv).unwrap_or(n)
                }
                ("multiplexor" | "xbar", _, _) => n,
                _ => 0,
            };
            edges.push(Edge {
                from: (*from).to_owned(),
                to: r.id.to_owned(),
                input,
            });
        }
    }

    Ok((ClockGraph { nodes, edges }, boxes))
}

/// The node kind + reset state for one element.
fn shape(r: &RawElement, p: Option<&Param>) -> (NodeKind, NodeState) {
    let default_int = || -> u32 {
        p.and_then(|p| p.default.trim().parse::<u32>().ok())
            .unwrap_or(0)
    };
    match r.kind {
        "fixedSource" | "variedSource" => {
            let hz = default_int();
            let (min, max) = match p {
                Some(p) => (p.min.unwrap_or(hz), p.max.unwrap_or(hz)),
                None => (hz, hz),
            };
            (
                NodeKind::Source {
                    min_hz: min,
                    max_hz: max.max(min),
                    gated: r.kind == "variedSource",
                },
                NodeState::Source {
                    enabled: true,
                    hz: hz.clamp(min, max.max(min)),
                },
            )
        }
        // A source whose frequency is picked from a list — MSI's range enum.
        // Used by 20 of the 66 family files (every MSI part: L4/L5/U5/WB/WL),
        // and it is what SYSCLK boots on there, so getting it wrong zeroes the
        // whole tree downstream.
        "distinctValsSource" => {
            let hz = p.map(|p| p.frequencies()).unwrap_or_default();
            let sel = p
                .map(|p| p.default_index())
                .and_then(|i| hz.get(i).copied())
                .unwrap_or_else(|| hz.first().copied().unwrap_or(0));
            let (min, max) = (
                hz.iter().copied().min().unwrap_or(sel),
                hz.iter().copied().max().unwrap_or(sel),
            );
            (
                NodeKind::Source {
                    min_hz: min,
                    max_hz: max.max(min),
                    gated: true,
                },
                NodeState::Source {
                    enabled: true,
                    hz: sel,
                },
            )
        }
        // `xbar` is a multiplexor drawn as a crossbar — same `refParameter`, same
        // list of possible values, same single selection. Falling through to the
        // default made it a `Tap`, which SILENTLY DROPPED the choice: STM32N6's
        // twenty IC clock selectors all came out as taps fed by four PLLs at
        // once, so nothing downstream could tell which PLL a peripheral ran on.
        //
        // Contained: 3 of the 66 clock files use it (N6, MP2, MP21).
        "multiplexor" | "xbar" => {
            let n = p.map(|p| p.values.len()).unwrap_or(r.inputs.len()).max(1);
            let sel = p.map(|p| p.default_index()).unwrap_or(0);
            (
                NodeKind::Mux { inputs: n },
                NodeState::Index(sel.min(n.saturating_sub(1))),
            )
        }
        "devisor" => match p {
            Some(p) if !p.divisors().is_empty() => {
                let options = p.divisors();
                // The default is a Value in the list; its position indexes the
                // parsed divisors only when every entry parsed, so re-find it by
                // the number the Comment carries.
                let want = p
                    .values
                    .iter()
                    .find(|(v, _)| *v == p.default)
                    .and_then(|(_, c)| c.trim().parse::<u32>().ok());
                let idx = want
                    .and_then(|w| options.iter().position(|o| *o == w))
                    .unwrap_or(0);
                (NodeKind::Divider { options }, NodeState::Index(idx))
            }
            // An integer-typed divider (Min/Max) — e.g. PLLM.
            Some(p) if p.max.is_some() => {
                let (lo, hi) = (p.min.unwrap_or(1).max(1), p.max.unwrap_or(1).max(1));
                let options: Vec<u32> = (lo..=hi).collect();
                let idx = default_int()
                    .checked_sub(lo)
                    .map(|d| d as usize)
                    .unwrap_or(0);
                (
                    NodeKind::Divider { options },
                    NodeState::Index(idx.min((hi - lo) as usize)),
                )
            }
            _ => (NodeKind::FixedDiv { by: 1 }, NodeState::Fixed),
        },
        "multiplicator" | "multiplicatorFrac" | "fractional" => match p {
            // A ×1/×2 style list (the APB timer rule) is a ratio choice.
            Some(p) if !p.values.is_empty() => {
                let ratios: Vec<(u32, u32)> =
                    p.divisors().into_iter().map(|n| (n, 1)).collect::<Vec<_>>();
                if ratios.is_empty() {
                    (NodeKind::Tap, NodeState::Fixed)
                } else {
                    let idx = p.default_index().min(ratios.len() - 1);
                    (NodeKind::Choice { ratios }, NodeState::Index(idx))
                }
            }
            Some(p) if p.max.is_some() => {
                let (lo, hi) = (p.min.unwrap_or(1).max(1), p.max.unwrap_or(1).max(1));
                (
                    NodeKind::Multiplier { min: lo, max: hi },
                    NodeState::Value(default_int().clamp(lo, hi)),
                )
            }
            _ => (NodeKind::Tap, NodeState::Fixed),
        },
        "output" | "activeOutput" => (NodeKind::Output, NodeState::Fixed),
        // Anything new ST adds still imports, as a pass-through.
        _ => (NodeKind::Tap, NodeState::Fixed),
    }
}

/// Attach a datasheet ceiling to a node, by id — the CubeMX files describe the
/// topology but not the maxima, which the IDE already carries per chip.
pub fn set_limit(graph: &mut ClockGraph, id: &str, hz: u32) {
    if let Some(n) = graph.node_mut(id) {
        n.limit = Some(LimitKey::Hz(hz));
    }
}

// ── Locating the two files ───────────────────────────────────────────────────

/// The RCC IP file for an exact IP version, as a chip's pin-data XML names it
/// (`Version="STM32H5_rcc_v1_1"` → `RCC-STM32H5_rcc_v1_1_Modes.xml`).
///
/// Exact beats guessing: H5 alone ships `v1_0`, `v1_1`, `v1_128_0` and
/// `v1_512_0`, and [`find_rcc_file`]'s newest-wins fallback picks the wrong one.
/// The version string carries the separator quirk too — ST writes
/// `STM32WBA_rcc_v1_0` but `STM32F411-rcc_v1_0`.
pub fn rcc_file_for_version(db_dir: &std::path::Path, version: &str) -> Option<std::path::PathBuf> {
    let p = db_dir
        .join("mcu")
        .join("IP")
        .join(format!("RCC-{version}_Modes.xml"));
    p.is_file().then_some(p)
}

/// The RCC IP file that goes with a clock-tree file, when no exact version is
/// known — FOUND by prefix, newest match wins.
///
/// Prefer [`rcc_file_for_version`]: this is the fallback for a bare file pick,
/// and on a family with several IP versions it can pick the wrong one.
pub fn find_rcc_file(db_dir: &std::path::Path, family: &str) -> Option<std::path::PathBuf> {
    let ip = db_dir.join("mcu").join("IP");
    let want = format!("rcc-{}", family.to_ascii_lowercase());
    let mut hits: Vec<std::path::PathBuf> = std::fs::read_dir(ip)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            let lower = name.to_ascii_lowercase();
            lower.ends_with("_modes.xml")
                && lower
                    .strip_prefix(&want)
                    .is_some_and(|rest| rest.starts_with('_') || rest.starts_with('-'))
        })
        .collect();
    hits.sort();
    hits.pop()
}

/// The family key a clock-tree file stands for: `…/plugins/clock/STM32WBA.xml`
/// → `STM32WBA`. Used to find its RCC partner.
pub fn family_of(clock_path: &std::path::Path) -> Option<String> {
    clock_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_owned())
}

/// Import both halves from a CubeMX `db` directory: the family's clock tree and
/// the RCC parameters it references.
pub fn import_from_db(
    db_dir: &std::path::Path,
    family: &str,
    variant: &Variant,
) -> Result<(ClockGraph, Vec<NodeBox>), String> {
    let rcc_path = find_rcc_file(db_dir, family)
        .ok_or_else(|| format!("no RCC IP file for {family} under {}", db_dir.display()))?;
    import_files(db_dir, family, &rcc_path, variant)
}

/// Read the two named files and parse them.
fn import_files(
    db_dir: &std::path::Path,
    clock_tree: &str,
    rcc_path: &std::path::Path,
    variant: &Variant,
) -> Result<(ClockGraph, Vec<NodeBox>), String> {
    let clock_path = db_dir
        .join("plugins")
        .join("clock")
        .join(format!("{clock_tree}.xml"));
    let clock = std::fs::read_to_string(&clock_path)
        .map_err(|e| format!("could not read {}: {e}", clock_path.display()))?;
    let rcc = std::fs::read_to_string(rcc_path)
        .map_err(|e| format!("could not read {}: {e}", rcc_path.display()))?;
    parse_clock_tree(&clock, &parse_rcc_params(&rcc)?, variant)
}

// ── Driving the import from a chip's own pin-data XML ────────────────────────

/// Everything a chip's `STM32_open_pin_data` `mcu/*.xml` says about its clock.
///
/// That file has no clock tree in it — but it names, exactly, which CubeMX files
/// describe one, and which conditional branches this particular part has. Which
/// turns the import from per-FAMILY guesswork into a per-CHIP lookup:
///
/// - `<Mcu ClockTree="STM32H5_4M">` → `db/plugins/clock/STM32H5_4M.xml`. H5 alone
///   ships four topologies (`STM32H5`, `_128`, `_4M`, `_512`); the family name
///   picks the wrong one.
/// - `<IP Name="RCC" Version="STM32H5_rcc_v1_1">` → the exact RCC parameter file.
/// - every `<IP InstanceName>` → the `<NAME>_Exist` tokens CubeMX's conditions
///   test (L4's clock file gates on `LCD_Exist` and `USB_OTG_FS_Exist`), and
///   `<Mcu Line="STM32WBAx5">` → the family-variant tokens WBA gates on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChipClockKey {
    /// Name of the clock-tree file, without `.xml`.
    pub clock_tree: String,
    /// RCC IP version, as it appears in the file name.
    pub rcc_version: String,
    pub variant: Variant,
}

/// Read the clock keys out of a chip's pin-data XML.
pub fn clock_key_from_mcu_xml(xml: &str) -> Result<ChipClockKey, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("MCU XML parse error: {e}"))?;
    let mcu = doc
        .descendants()
        .find(|n| n.has_tag_name("Mcu"))
        .ok_or("no <Mcu> element — is this an STM32_open_pin_data mcu/*.xml?")?;

    let clock_tree = mcu
        .attribute("ClockTree")
        .ok_or("this chip's XML has no ClockTree attribute")?
        .to_owned();

    let mut defines: Vec<String> = Vec::new();
    // Family-variant tokens: `Line="STM32WBAx5"` is literally what WBA's
    // conditions test; Family and RefName cost nothing and may be tested too.
    for attr in ["Line", "Family", "RefName"] {
        if let Some(v) = mcu.attribute(attr).filter(|v| !v.is_empty()) {
            defines.push(v.to_owned());
        }
    }
    let mut rcc_version = String::new();
    for ip in doc.descendants().filter(|n| n.has_tag_name("IP")) {
        if ip.attribute("Name") == Some("RCC")
            && let Some(v) = ip.attribute("Version")
        {
            rcc_version = v.to_owned();
        }
        // A peripheral this part HAS — CubeMX asks `<NAME>_Exist`.
        for attr in ["InstanceName", "Name"] {
            if let Some(name) = ip.attribute(attr).filter(|n| !n.is_empty()) {
                defines.push(format!("{name}_Exist"));
                defines.push(name.to_owned());
            }
        }
    }
    defines.sort();
    defines.dedup();

    if rcc_version.is_empty() {
        return Err("this chip's XML has no RCC IP version".into());
    }
    Ok(ChipClockKey {
        clock_tree,
        rcc_version,
        variant: Variant { defines },
    })
}

/// Import the clock tree of one specific chip: its pin-data XML names the files,
/// the CubeMX `db` supplies them.
/// Finish an import: lay the vendor boxes out and bind the vendor node names to
/// the ids code generation reads.
///
/// The two callers that need this — the Clock tab's importers and the chip
/// import in New Project — were about to grow a copy each. They must not: the
/// bindings are what decide whether an imported tree reaches `main.rs` at all,
/// so two versions of this would be two answers to that question.
///
/// Returns the ready-to-use clock and the codegen ids that found no node. A
/// non-empty list is not an error — that value falls back to a default — but it
/// is worth saying out loud, which is why it comes back instead of being logged.
pub fn bind_graph(
    graph: ClockGraph,
    boxes: Vec<NodeBox>,
    family: &str,
) -> (super::GraphClock, Vec<String>) {
    use super::bind;
    use crate::panels::mcu_module::codegen::rcc::codegen_node_ids;

    let ids = codegen_node_ids(family);
    let bindings = bind::propose(&ids, &graph);
    let mut missing = bind::unbound(&ids, &bindings);
    // `pllp` and `pllr` are ALTERNATIVE spellings offered to a family with no
    // recipe, not two things the tree owes us: at most one can ever bind, and a
    // family whose PLL just multiplies (F0/F1/F3) has neither. Reporting them
    // would put a permanent two-item warning on trees that are complete.
    if crate::panels::mcu_module::codegen::rcc::rcc_recipe(family).is_none() {
        missing.retain(|id| id != "pllp" && id != "pllr");
    }
    // The vendor's coordinates are read as ORDER, not position — see
    // `auto_layout::respace`. Import is the only place this may run: it would
    // otherwise throw away an arrangement the user had dragged.
    let layout = super::derive(
        &graph,
        super::auto_layout::respace(&graph, boxes, super::auto_layout::Spread::default()),
    );
    (
        super::GraphClock {
            graph,
            layout,
            bindings,
        },
        missing,
    )
}

#[cfg(test)]
mod chip_import_tests {
    use super::*;

    /// The whole Phase 2 chain on real vendor data: a chip file the IDE ships no
    /// clock template for, imported into pins AND a working clock that reaches
    /// `main.rs`.
    ///
    /// H5 is the case that motivated all of it — importable, no template, and
    /// no RCC recipe — so if it works here it works for the families that merely
    /// lack a template.
    ///
    /// `cargo test -- --ignored a_chip_imports_with_its_clock`
    #[test]
    #[ignore]
    fn a_chip_imports_with_its_clock() {
        use crate::panels::mcu_module::chip_sources;
        use crate::panels::mcu_module::clock::model::ClockConfig;
        use crate::panels::mcu_module::codegen::rcc::{
            generates_clock_code, generates_clock_code_for, graph_clock_block,
        };
        use crate::panels::mcu_module::stm32_pin_data::convert_xml;

        let Some(src) = chip_sources::all_sources()
            .into_iter()
            .find(|s| s.has_clock())
        else {
            println!("no CubeMX installation — nothing to check");
            return;
        };
        let db = src.db.as_deref().unwrap();

        // Any H5 part; the file name varies by installation.
        let Some(file) = std::fs::read_dir(&src.chips)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("STM32H563") && n.ends_with(".xml"))
            })
        else {
            println!("no STM32H563 in this installation");
            return;
        };

        let xml = std::fs::read_to_string(&file).unwrap();
        let chips = convert_xml(&xml).expect("pins");
        let family = chips[0].form.family.clone();
        assert_eq!(family, "stm32h5");

        // What the pin import alone produces: no tree at all.
        assert!(
            matches!(
                chips[0].form.clock,
                crate::panels::mcu_module::mcu_form::ClockChoice::None
            ),
            "the IDE ships no H5 template — that is the gap this closes"
        );

        let (gc, missing) = graph_for_chip_xml(db, &xml, &family).expect("clock");
        println!(
            "{}: {} nodes, {} bound, unbound: {missing:?}",
            file.file_name().unwrap().to_string_lossy(),
            gc.graph.nodes.len(),
            gc.bindings.len()
        );
        assert!(gc.graph.nodes.len() > 50, "a real H5 tree is large");
        assert!(!gc.layout.is_empty(), "and it has a diagram");

        // And the payoff: that tree generates real clock code, via the generic
        // recipe, on a family with no RCC recipe of its own.
        assert!(!generates_clock_code(&family), "still no family recipe");
        // Not at its default: SYSCLK is HSI through HSIDIV /2, a divider no
        // emitter writes (embassy would run the HSI undivided), so that is the
        // skeleton. With HSIDIV at /1 it is plain HSI.
        let at_default = ClockConfig::Graph(gc.clone());
        assert!(!generates_clock_code_for(&family, &at_default));
        assert!(
            graph_clock_block(&family, &at_default, false).contains("not write (MSI, CSI, HSI48"),
            "the skeleton names why"
        );
        let mut gc = gc;
        let hsidiv = gc
            .bindings
            .get("hsi")
            .cloned()
            .expect("HSIDIV is bound as hsi");
        let node = gc.graph.node_mut(&hsidiv).expect("the HSIDIV node");
        let NodeKind::Divider { options } = &node.kind else {
            panic!("HSIDIV is a divider: {:?}", node.kind)
        };
        let by_one = options.iter().position(|d| *d == 1).expect("a /1 option");
        node.state = NodeState::Index(by_one);
        let clock = ClockConfig::Graph(gc);
        assert!(
            generates_clock_code_for(&family, &clock),
            "but the TREE generates: {:?}",
            clock
        );
        let block = graph_clock_block(&family, &clock, false);
        assert!(
            !block.contains("has no generated RCC recipe yet"),
            "not the skeleton any more:\n{block}"
        );
        assert!(block.contains("embassy_stm32::init"), "{block}");
        // Printed because the FIELD NAMES here are a guess — the generic recipe
        // picks embassy's most common spelling off the tree's shape — and this
        // is the only place that guess can be eyeballed against a real family.
        println!("--- generated for {family} ---\n{block}");
    }

    /// The other reported bug: the imported figure was unreadable.
    ///
    /// Two acceptance criteria, both measured rather than eyeballed — nothing
    /// may overlap, and wires must mostly run straight.
    ///
    /// `cargo test -- --ignored an_imported_figure_does_not_overlap`
    #[test]
    #[ignore]
    fn an_imported_figure_does_not_overlap() {
        use crate::panels::mcu_module::chip_sources;

        let Some(src) = chip_sources::all_sources()
            .into_iter()
            .find(|s| s.has_clock())
        else {
            println!("no CubeMX installation — nothing to check");
            return;
        };
        let db = src.db.as_deref().unwrap();
        let Some(file) = std::fs::read_dir(&src.chips)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("STM32U575") && n.ends_with(".xml"))
            })
        else {
            println!("no STM32U575 in this installation");
            return;
        };
        let xml = std::fs::read_to_string(&file).unwrap();
        let key = clock_key_from_mcu_xml(&xml).unwrap();
        let (graph, raw) = import_for_chip(db, &key).unwrap();

        // What the vendor coordinates gave us, for comparison.
        let before = overlaps(&raw);
        // Every setting must be overlap-free; the report is what each costs.
        use super::super::auto_layout::Spread;
        for s in Spread::ALL {
            let b = super::super::auto_layout::respace(&graph, raw.clone(), s);
            let w = b.iter().map(|x| x.x + x.w).fold(0.0f32, f32::max);
            let h = b.iter().map(|x| x.y + x.h).fold(0.0f32, f32::max);
            let mut ys: Vec<i32> = b.iter().map(|x| x.y as i32).collect();
            ys.sort_unstable();
            ys.dedup();
            let lay = super::super::derive(&graph, b.clone());
            println!(
                "{:>3}: {w:.0}x{h:.0} · {} rows · {} straight of {} wires · {} overlaps",
                s.label(),
                ys.len(),
                lay.wires.iter().filter(|w| w.len() == 2).count(),
                lay.wires.len(),
                overlaps(&b)
            );
            assert_eq!(overlaps(&b), 0, "{:?} overlaps", s);
        }
        let spaced = super::super::auto_layout::respace(&graph, raw, Spread::default());
        let after = overlaps(&spaced);
        println!("{} nodes · overlaps {before} -> {after}", spaced.len());
        {
            let w = spaced.iter().map(|b| b.x + b.w).fold(0.0f32, f32::max);
            let h = spaced.iter().map(|b| b.y + b.h).fold(0.0f32, f32::max);
            let mut ys: Vec<i32> = spaced.iter().map(|b| b.y as i32).collect();
            ys.sort_unstable();
            ys.dedup();
            let mut xs: Vec<i32> = spaced.iter().map(|b| b.x as i32).collect();
            xs.sort_unstable();
            xs.dedup();
            // How much of the height is air a column does not need: each row is
            // as tall as its tallest member, across every column.
            let own: f32 = spaced.iter().map(|b| b.h).sum::<f32>() / xs.len() as f32;
            println!(
                "canvas {w:.0}x{h:.0} · {} columns x {} rows · mean column content {own:.0} px",
                xs.len(),
                ys.len()
            );
        }
        assert_eq!(after, 0, "nothing may sit on top of anything else");
        assert!(before > 0, "the bug was real, or this test proves nothing");

        // A node several wires enter has to be tall enough to hold them.
        let widest = graph
            .nodes
            .iter()
            .map(|n| (graph.edges.iter().filter(|e| e.to == n.id).count(), &n.id))
            .max()
            .unwrap();
        let tall = spaced.iter().find(|b| &b.node == widest.1).unwrap();
        println!(
            "widest node `{}` has {} inputs, h = {}",
            widest.1, widest.0, tall.h
        );
        assert!(
            tall.h >= widest.0 as f32 * 12.0,
            "{} inputs will not fit in {} px",
            widest.0,
            tall.h
        );

        // Straight wires: `derive` emits a bend-free run when the source centre
        // and the target entry line up.
        let straight =
            |lay: &super::super::ClockLayout| lay.wires.iter().filter(|w| w.len() == 2).count();
        let bent = super::super::derive(&graph, vendor_boxes(db, &key));
        let ours = super::super::derive(&graph, spaced);
        println!(
            "straight wires {}/{} -> {}/{}",
            straight(&bent),
            bent.wires.len(),
            straight(&ours),
            ours.wires.len()
        );
        assert!(
            straight(&ours) > straight(&bent),
            "aligning the rows is what removes the bends"
        );
    }

    /// The raw vendor boxes again — `import_for_chip` consumes them.
    fn vendor_boxes(db: &std::path::Path, key: &ChipClockKey) -> Vec<NodeBox> {
        import_for_chip(db, key).unwrap().1
    }

    /// How many node footprints intersect.
    ///
    /// The footprint is what is actually PAINTED, taken from `gui/diagram.rs`:
    /// the id label is bottom-anchored at `y - 3` in an 8.5 px font, the control
    /// runs `y .. y + h`, and the frequency tag is centred at `y + h + 12` in a
    /// 9 px font. An earlier version of this test under-measured it and passed
    /// on a figure the eye could see was crowded.
    fn overlaps(boxes: &[NodeBox]) -> usize {
        let rect = |b: &NodeBox| (b.x, b.y - 15.0, b.x + b.w, b.y + b.h + 18.0);
        let mut n = 0;
        for (i, a) in boxes.iter().enumerate() {
            let (ax0, ay0, ax1, ay1) = rect(a);
            for b in &boxes[i + 1..] {
                let (bx0, by0, bx1, by1) = rect(b);
                if ax0 < bx1 && bx0 < ax1 && ay0 < by1 && by0 < ay1 {
                    n += 1;
                }
            }
        }
        n
    }

    /// The reported bug, on the reported chip: an STM32F358 imported complete —
    /// pins and its whole clock tree — and still generating no clock code,
    /// because its PLL has no output divider for the generic recipe to key on.
    ///
    /// `cargo test -- --ignored an_f3_generates_its_clock`
    #[test]
    #[ignore]
    fn an_f3_generates_its_clock() {
        use crate::panels::mcu_module::chip_sources;
        use crate::panels::mcu_module::clock::model::ClockConfig;
        use crate::panels::mcu_module::codegen::rcc::{
            generates_clock_code_for, graph_clock_block,
        };
        use crate::panels::mcu_module::stm32_pin_data::convert_xml;

        let Some(src) = chip_sources::all_sources()
            .into_iter()
            .find(|s| s.has_clock())
        else {
            println!("no CubeMX installation — nothing to check");
            return;
        };
        let db = src.db.as_deref().unwrap();
        let Some(file) = std::fs::read_dir(&src.chips)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("STM32F358") && n.ends_with(".xml"))
            })
        else {
            println!("no STM32F358 in this installation");
            return;
        };

        let xml = std::fs::read_to_string(&file).unwrap();
        let family = convert_xml(&xml).unwrap()[0].form.family.clone();
        assert_eq!(family, "stm32f3");

        let (gc, missing) = graph_for_chip_xml(db, &xml, &family).expect("clock");
        println!(
            "{} nodes, {} bound, unbound: {missing:?}",
            gc.graph.nodes.len(),
            gc.bindings.len()
        );
        // The PLL leg the old rule demanded simply does not exist here.
        let ids: Vec<&str> = gc.bindings.keys().map(String::as_str).collect();
        assert!(
            !ids.contains(&"pllp") && !ids.contains(&"pllr"),
            "F3's PLL multiplies and stops: {ids:?}"
        );

        let clock = ClockConfig::Graph(gc);
        assert!(
            generates_clock_code_for(&family, &clock),
            "THE bug: a complete tree that generated nothing"
        );
        let block = graph_clock_block(&family, &clock, false);
        assert!(
            !block.contains("has no generated RCC recipe yet"),
            "{block}"
        );
        // The f013 shape, field for field.
        assert!(
            !block.contains("divp"),
            "F3's Pll has no output dividers:\n{block}"
        );
        assert!(!block.contains("source:"), "it spells it `src`:\n{block}");
        println!("--- generated for {family} ---\n{block}");
    }
}

/// This chip's clock, from its own pin-data XML plus a CubeMX installation.
///
/// The whole path in one call: the chip file names its clock tree and RCC
/// version, those select the two CubeMX files, and the result is bound to
/// `family`'s codegen ids. `family` is the IDE's key (`stm32h5`), which the
/// caller already has — it is not derivable from the clock files.
pub fn graph_for_chip_xml(
    db_dir: &std::path::Path,
    chip_xml: &str,
    family: &str,
) -> Result<(super::GraphClock, Vec<String>), String> {
    let key = clock_key_from_mcu_xml(chip_xml)?;
    let (graph, boxes) = import_for_chip(db_dir, &key)?;
    Ok(bind_graph(graph, boxes, family))
}

pub fn import_for_chip(
    db_dir: &std::path::Path,
    key: &ChipClockKey,
) -> Result<(ClockGraph, Vec<NodeBox>), String> {
    // Exact version first; a family fallback keeps an unusual naming working.
    let rcc_path = rcc_file_for_version(db_dir, &key.rcc_version)
        .or_else(|| find_rcc_file(db_dir, &key.clock_tree))
        .ok_or_else(|| {
            format!(
                "no RCC IP file `RCC-{}_Modes.xml` under {}",
                key.rcc_version,
                db_dir.display()
            )
        })?;
    import_files(db_dir, &key.clock_tree, &rcc_path, &key.variant)
}

/// The `db` directory of a standard STM32CubeMX installation, if there is one.
pub fn default_db_dir() -> Option<std::path::PathBuf> {
    let candidates = [
        r"C:\Program Files\STMicroelectronics\STM32Cube\STM32CubeMX\db",
        r"C:\Program Files (x86)\STMicroelectronics\STM32Cube\STM32CubeMX\db",
        "/Applications/STMicroelectronics/STM32CubeMX.app/Contents/Resources/db",
        "/opt/STMicroelectronics/STM32Cube/STM32CubeMX/db",
    ];
    candidates
        .iter()
        .map(std::path::PathBuf::from)
        .find(|p| p.is_dir())
        .or_else(|| {
            // A per-user install (the Linux/macOS default offered by the
            // installer) lives under the home directory.
            let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
            let p = std::path::PathBuf::from(home)
                .join("STM32CubeMX")
                .join("db");
            p.is_dir().then_some(p)
        })
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panels::mcu_module::clock::graph::evaluate;

    const RCC: &str = r#"<IP>
        <RefParameter Name="HSI_VALUE" Type="integer" DefaultValue="16000000"/>
        <RefParameter Name="HSE_VALUE" Type="integer" DefaultValue="32000000" Min="32000000" Max="32000000"/>
        <RefParameter Name="SYSCLKSource" Type="list" DefaultValue="RCC_SYSCLKSOURCE_HSE">
            <PossibleValue Value="RCC_SYSCLKSOURCE_HSI" Comment="Hsi"/>
            <PossibleValue Value="RCC_SYSCLKSOURCE_HSE" Comment="Hse"/>
        </RefParameter>
        <RefParameter Name="AHBCLKDivider" Type="list" DefaultValue="RCC_SYSCLK_DIV2">
            <PossibleValue Value="RCC_SYSCLK_DIV1" Comment="1"/>
            <PossibleValue Value="RCC_SYSCLK_DIV2" Comment="2"/>
            <PossibleValue Value="RCC_SYSCLK_DIV4" Comment="4"/>
        </RefParameter>
        <RefParameter Name="PLLN" Type="integer" Min="4" Max="512" DefaultValue="25"/>
        <RefParameter Name="APB1TimCLKDivider" Type="list" DefaultValue="RCC_HCLK_DIV1">
            <PossibleValue Value="RCC_HCLK_DIV1" Comment="1"/>
            <PossibleValue Value="RCC_HCLK_DIV2" Comment="2"/>
        </RefParameter>
    </IP>"#;

    /// The shape of a real family file, cut down to what the parser reads.
    const CLOCK: &str = r#"<Clock>
        <Element id="HSIRC" type="fixedSource" refParameter="HSI_VALUE" x="295" y="485"/>
        <Element id="HSEOSC" type="variedSource" refParameter="HSE_VALUE" x="200" y="650"/>
        <Element id="OnlyOnBigParts" type="fixedSource" refParameter="HSI_VALUE" x="0" y="0">
            <Condition Expression="STM32WBAx5" Diagnostic=""/>
        </Element>
        <Element id="SysClkSource" type="multiplexor" refParameter="SYSCLKSource" x="502" y="427">
            <Input signalId="HSI" from="HSIRC" refValue="RCC_SYSCLKSOURCE_HSI"/>
            <Input signalId="HSE" from="HSEOSC" refValue="RCC_SYSCLKSOURCE_HSE"/>
        </Element>
        <Element id="AHBPrescaler" type="devisor" refParameter="AHBCLKDivider" x="900" y="427">
            <Input signalId="SYSCLK" from="SysClkSource"/>
        </Element>
        <Element id="HCLK" type="output" x="1100" y="427">
            <Input signalId="HCLK" from="AHBPrescaler"/>
        </Element>
    </Clock>"#;

    fn parse(defines: &[&str]) -> (ClockGraph, Vec<NodeBox>) {
        let rcc = parse_rcc_params(RCC).expect("rcc");
        parse_clock_tree(CLOCK, &rcc, &Variant::new(defines.to_vec())).expect("clock")
    }

    /// End to end: ST's data becomes a tree that evaluates to ST's numbers.
    #[test]
    fn a_cubemx_tree_imports_and_evaluates() {
        let (graph, boxes) = parse(&[]);
        // The conditional element is not on this variant.
        assert!(graph.node("OnlyOnBigParts").is_none());
        assert_eq!(graph.nodes.len(), 5);

        let f = evaluate(&graph);
        // HSE 32 MHz selected by default, AHB /2 by default.
        assert_eq!(f["SysClkSource"], 32_000_000);
        assert_eq!(f["HCLK"], 16_000_000);

        // ST's own coordinates come along, so the diagram is their layout.
        let hse = boxes.iter().find(|b| b.node == "HSEOSC").unwrap();
        assert_eq!((hse.x, hse.y), (200.0, 650.0));
    }

    /// A conditional element joins the tree when the chip defines its token.
    #[test]
    fn variant_conditions_select_the_elements() {
        let (graph, _) = parse(&["STM32WBAx5"]);
        assert!(graph.node("OnlyOnBigParts").is_some());
    }

    /// Mux inputs are indexed by the REGISTER value, not by file order — the
    /// same guarantee the AI path gets from matching names.
    #[test]
    fn mux_inputs_follow_the_register_value_not_the_file_order() {
        let clock = CLOCK.replace(
            r#"<Input signalId="HSI" from="HSIRC" refValue="RCC_SYSCLKSOURCE_HSI"/>
            <Input signalId="HSE" from="HSEOSC" refValue="RCC_SYSCLKSOURCE_HSE"/>"#,
            // Same two inputs, listed the other way round.
            r#"<Input signalId="HSE" from="HSEOSC" refValue="RCC_SYSCLKSOURCE_HSE"/>
            <Input signalId="HSI" from="HSIRC" refValue="RCC_SYSCLKSOURCE_HSI"/>"#,
        );
        let rcc = parse_rcc_params(RCC).unwrap();
        let (graph, _) = parse_clock_tree(&clock, &rcc, &Variant::default()).unwrap();
        let idx = |from: &str| {
            graph
                .edges
                .iter()
                .find(|e| e.from == from && e.to == "SysClkSource")
                .unwrap()
                .input
        };
        assert_eq!((idx("HSIRC"), idx("HSEOSC")), (0, 1), "register order wins");
        // And the default selection still resolves to HSE.
        assert_eq!(evaluate(&graph)["SysClkSource"], 32_000_000);
    }

    /// Divider option lists come from the `Comment` numbers, with the reset
    /// default selected.
    #[test]
    fn divider_options_and_defaults_come_from_the_rcc_file() {
        let (graph, _) = parse(&[]);
        let ahb = graph.node("AHBPrescaler").unwrap();
        let NodeKind::Divider { options } = &ahb.kind else {
            panic!("a divider");
        };
        assert_eq!(options, &[1, 2, 4]);
        assert_eq!(ahb.state, NodeState::Index(1), "RCC_SYSCLK_DIV2");
    }

    /// The two parameter shapes ST uses for multipliers: an integer range (PLLN)
    /// and a ×1/×2 list (the APB timer rule).
    #[test]
    fn multipliers_take_either_shape() {
        let rcc = parse_rcc_params(RCC).unwrap();
        let clock = r#"<Clock>
            <Element id="Src" type="fixedSource" refParameter="HSI_VALUE" x="0" y="0"/>
            <Element id="PLLN" type="multiplicator" refParameter="PLLN" x="1" y="1">
                <Input signalId="a" from="Src"/>
            </Element>
            <Element id="TimPrescalerAPB1" type="multiplicator" refParameter="APB1TimCLKDivider" x="2" y="2">
                <Input signalId="b" from="Src"/>
            </Element>
        </Clock>"#;
        let (graph, _) = parse_clock_tree(clock, &rcc, &Variant::default()).unwrap();
        assert!(matches!(
            graph.node("PLLN").unwrap().kind,
            NodeKind::Multiplier { min: 4, max: 512 }
        ));
        assert_eq!(graph.node("PLLN").unwrap().state, NodeState::Value(25));
        assert!(matches!(
            &graph.node("TimPrescalerAPB1").unwrap().kind,
            NodeKind::Choice { ratios } if ratios == &[(1, 1), (2, 1)]
        ));
    }

    /// MSI is a source whose frequency comes from a range ENUM, in kHz. Every
    /// MSI part (L4/L5/U5/WB/WL — 20 of the 66 family files) boots SYSCLK on it,
    /// so reading it as a plain pass-through zeroes the entire tree: the L4
    /// import went from 13 evaluated nodes to all 90 once this was handled.
    #[test]
    fn an_msi_style_range_source_is_read_in_its_own_unit() {
        let rcc = r#"<IP>
            <RefParameter Name="MSIClockRange" Type="list" Unit="KHz" DefaultValue="RCC_MSIRANGE_6">
                <PossibleValue Comment="100"  Value="RCC_MSIRANGE_0"/>
                <PossibleValue Comment="4000" Value="RCC_MSIRANGE_6"/>
                <PossibleValue Comment="48000" Value="RCC_MSIRANGE_11"/>
            </RefParameter>
            <RefParameter Name="AHBCLKDivider" Type="list" DefaultValue="RCC_SYSCLK_DIV1">
                <PossibleValue Value="RCC_SYSCLK_DIV1" Comment="1"/>
                <PossibleValue Value="RCC_SYSCLK_DIV2" Comment="2"/>
            </RefParameter>
        </IP>"#;
        let clock = r#"<Clock>
            <Element id="MSIRC" type="distinctValsSource" refParameter="MSIClockRange" x="1" y="1"/>
            <Element id="AHB" type="devisor" refParameter="AHBCLKDivider" x="2" y="2">
                <Input signalId="s" from="MSIRC"/>
            </Element>
        </Clock>"#;
        let params = parse_rcc_params(rcc).unwrap();
        let (graph, _) = parse_clock_tree(clock, &params, &Variant::default()).unwrap();

        let msi = graph.node("MSIRC").unwrap();
        assert!(
            matches!(
                msi.kind,
                NodeKind::Source {
                    min_hz: 100_000,
                    max_hz: 48_000_000,
                    ..
                }
            ),
            "the range is in kHz: {:?}",
            msi.kind
        );
        // RCC_MSIRANGE_6 = 4000 kHz = 4 MHz, the L4 reset default.
        assert_eq!(evaluate(&graph)["AHB"], 4_000_000);
    }

    /// The condition language: `&` binds tighter than `|`, `!` negates, and
    /// unknown tokens are false.
    #[test]
    fn condition_expressions_evaluate_like_c() {
        let v = Variant::new(vec!["A", "B"]);
        assert!(v.eval(""));
        assert!(v.eval("A"));
        assert!(!v.eval("C"));
        assert!(v.eval("A & B"));
        assert!(!v.eval("A & C"));
        assert!(v.eval("A | C"));
        assert!(v.eval("!C"));
        assert!(!v.eval("!A"));
        assert!(v.eval("!STM32WBAx4&!STM32WBAx5"), "the real WBA form");
        assert!(v.eval("(A|C) & B"));
        assert!(!v.eval("C | (A & !B)"));
    }

    #[test]
    fn a_wrong_file_is_reported() {
        assert!(
            parse_rcc_params("<Nope/>")
                .unwrap_err()
                .contains("RefParameter")
        );
        let rcc = parse_rcc_params(RCC).unwrap();
        assert!(
            parse_clock_tree("<Nope/>", &rcc, &Variant::default())
                .unwrap_err()
                .contains("no clock elements")
        );
        assert!(parse_rcc_params("not xml at all <<<").is_err());
    }

    /// A chip's pin-data XML carries no clock tree — but it names, exactly, the
    /// two CubeMX files that describe one, plus the conditions this part meets.
    #[test]
    fn a_chips_pin_data_xml_names_its_clock_files() {
        // The shape of a real STM32_open_pin_data mcu/*.xml, cut to the keys.
        let xml = r#"<Mcu ClockTree="STM32H5_4M" Family="STM32H5" Line="STM32H5x5"
                          RefName="STM32H5F5IJKxQ" Package="UFBGA176">
            <Core>ARM Cortex-M33</Core>
            <IP InstanceName="RCC" Name="RCC" Version="STM32H5_rcc_v1_1"/>
            <IP InstanceName="SAI1" Name="SAI"/>
            <IP InstanceName="USB_OTG_FS" Name="USB_OTG_FS"/>
            <Pin Name="PA0" Position="1"/>
        </Mcu>"#;
        let key = clock_key_from_mcu_xml(xml).expect("keys");

        // H5 ships four topologies; the family name would pick the wrong one.
        assert_eq!(key.clock_tree, "STM32H5_4M");
        assert_eq!(key.rcc_version, "STM32H5_rcc_v1_1");

        // Variant tokens: the family line WBA gates on, and the `_Exist` tokens
        // L4's file gates on.
        assert!(key.variant.eval("STM32H5x5"), "the Line token");
        assert!(key.variant.eval("SAI1_Exist"), "a peripheral it has");
        assert!(key.variant.eval("USB_OTG_FS_Exist"));
        assert!(!key.variant.eval("LCD_Exist"), "one it does not have");
        assert!(key.variant.eval("!LCD_Exist & SAI1_Exist"));
    }

    #[test]
    fn a_pin_data_xml_without_the_keys_is_reported() {
        assert!(
            clock_key_from_mcu_xml("<Nope/>")
                .unwrap_err()
                .contains("<Mcu>")
        );
        let no_tree = r#"<Mcu Family="X"><IP Name="RCC" Version="v"/></Mcu>"#;
        assert!(
            clock_key_from_mcu_xml(no_tree)
                .unwrap_err()
                .contains("ClockTree")
        );
        let no_rcc = r#"<Mcu ClockTree="T"><IP Name="GPIO"/></Mcu>"#;
        assert!(
            clock_key_from_mcu_xml(no_rcc)
                .unwrap_err()
                .contains("RCC IP version")
        );
    }

    /// End to end on the REAL files, driven by a chip's own pin-data XML
    /// (ignored — needs both a CubeMX install and the pin-data repo):
    /// `cargo test -- --ignored cubemx_for_a_real_chip --nocapture`
    #[test]
    #[ignore]
    fn cubemx_for_a_real_chip() {
        let Some(db) = default_db_dir() else {
            eprintln!("no CubeMX install found — skipping");
            return;
        };
        let repo = std::path::Path::new(
            r"F:\RUST_bootcampCourse\MyProjects\STM32_open_pin_data-master\mcu",
        );
        if !repo.is_dir() {
            eprintln!("no pin-data repo at {} — skipping", repo.display());
            return;
        }
        for chip in [
            "STM32H5F5IJKxQ",
            "STM32WBA55CGUx",
            "STM32L476R(C-E-G)Tx",
            "STM32F411R(C-E)Tx",
        ] {
            let path = repo.join(format!("{chip}.xml"));
            let Ok(xml) = std::fs::read_to_string(&path) else {
                eprintln!("{chip}: not in the repo, skipping");
                continue;
            };
            let key = clock_key_from_mcu_xml(&xml).expect("keys");
            match import_for_chip(&db, &key) {
                Ok((graph, boxes)) => {
                    let freqs = evaluate(&graph);
                    let live = freqs.values().filter(|hz| **hz > 0).count();
                    eprintln!(
                        "{chip}: tree={} rcc={} -> {} nodes, {} edges, {live} live, {} boxes",
                        key.clock_tree,
                        key.rcc_version,
                        graph.nodes.len(),
                        graph.edges.len(),
                        boxes.len()
                    );
                    assert!(graph.nodes.len() > 20);
                }
                Err(e) => eprintln!("{chip}: {e}"),
            }
        }
    }

    /// Write a chip's imported clock tree out as a `.ron` template.
    ///
    /// Ignored, like the other generators in this crate — it needs a CubeMX
    /// install and writes into the repo. The part and the destination come from
    /// the environment, so generating another chip needs no code edit:
    ///
    /// ```text
    /// CLOCK_CHIP_XML=…/mcu/STM32WBA55CGUx.xml \
    /// CLOCK_OUT_RON=assets/mcus/examples/stm32wba55_graphclock.ron \
    /// cargo test -- --ignored generate_chip_clock_ron --nocapture
    /// ```
    #[test]
    #[ignore]
    fn generate_chip_clock_ron() {
        use super::super::{GraphClock, derive, export_clock_ron};

        let chip_xml = std::env::var("CLOCK_CHIP_XML")
            .unwrap_or_else(|_| r"C:\Users\istra\Downloads\STM32F217Z(E-G)Tx.xml".to_owned());
        let out = std::env::var("CLOCK_OUT_RON")
            .unwrap_or_else(|_| "assets/mcus/examples/stm32f217_graphclock.ron".to_owned());
        let (chip_xml, out) = (chip_xml.as_str(), out.as_str());

        let Some(db) = default_db_dir() else {
            eprintln!("no CubeMX install found — skipping");
            return;
        };
        let xml = std::fs::read_to_string(chip_xml).expect("the chip's pin-data XML");
        let key = clock_key_from_mcu_xml(&xml).expect("clock keys");
        let (graph, boxes) = import_for_chip(&db, &key).expect("import");

        let freqs = evaluate(&graph);
        let live = freqs.values().filter(|hz| **hz > 0).count();
        eprintln!(
            "{} -> tree={} rcc={} : {} nodes, {} edges, {live} live",
            chip_xml,
            key.clock_tree,
            key.rcc_version,
            graph.nodes.len(),
            graph.edges.len()
        );
        for id in ["SysCLKOutput", "AHBOutput", "APB1Output", "APB2Output"] {
            if let Some(hz) = freqs.get(id) {
                eprintln!("   {id} = {} MHz", *hz as f64 / 1e6);
            }
        }

        // The generated file carries its codegen bindings, so importing it is
        // enough to produce real code — see `bind::propose`.
        let family = std::env::var("CLOCK_FAMILY").unwrap_or_else(|_| {
            // The IDE's family key, guessed from the tree name for the message.
            key.clock_tree.to_ascii_lowercase()[..8.min(key.clock_tree.len())].to_owned()
        });
        let ids = crate::panels::mcu_module::codegen::rcc::codegen_node_ids(&family);
        let bindings = super::super::bind::propose(&ids, &graph);
        let unbound = super::super::bind::unbound(&ids, &bindings);
        eprintln!(
            "   family={family} bindings={} unbound={unbound:?}",
            bindings.len()
        );

        let layout = derive(&graph, boxes);
        let gc = GraphClock {
            graph,
            layout,
            bindings,
        };
        std::fs::write(out, export_clock_ron(&gc)).expect("write the .ron");
        eprintln!("wrote {out}");

        // What was written must import back — the same guard Layer 1 applies.
        let back = super::super::parse_clock_ron(&std::fs::read_to_string(out).unwrap())
            .expect("the generated file must re-import");
        assert_eq!(back.graph, gc.graph);
    }

    /// Run against a REAL CubeMX install (ignored — it needs one):
    /// `cargo test -- --ignored cubemx_real_install --nocapture`
    #[test]
    #[ignore]
    fn cubemx_real_install() {
        let db =
            std::path::Path::new(r"C:\Program Files\STMicroelectronics\STM32Cube\STM32CubeMX\db");
        if !db.exists() {
            eprintln!("no CubeMX install at {} — skipping", db.display());
            return;
        }
        for fam in ["STM32WBA", "STM32F411", "STM32G0", "STM32L4"] {
            let variant = Variant::new(vec!["STM32WBAx5", "SAI1_Exist", "USART2_Exist"]);
            let (graph, boxes) = match import_from_db(db, fam, &variant) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("{fam}: {e}");
                    continue;
                }
            };
            let freqs = evaluate(&graph);
            let live = freqs.values().filter(|hz| **hz > 0).count();
            eprintln!(
                "{fam}: {} nodes, {} edges, {} boxes, {live} nodes with a frequency",
                graph.nodes.len(),
                graph.edges.len(),
                boxes.len()
            );
            assert!(graph.nodes.len() > 20, "{fam} should be a full tree");
            assert!(live > 5, "{fam} should evaluate to real frequencies");
            assert!(
                boxes.iter().any(|b| b.x > 0.0 && b.y > 0.0),
                "{fam} should carry ST's coordinates"
            );
        }
    }
}
