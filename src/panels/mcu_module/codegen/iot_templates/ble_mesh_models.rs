
// ── Bluetooth Mesh: the models ────────────────────────────────────────────
// Generic OnOff Server and Client, Sensor Server (with the Sensor Setup
// Server the specification asks for beside it) and Sensor Client, on the
// node's one element. No I/O either: `Mesh` joins them to the `Stack` above,
// and the glue turns their events into what the board's code awaits.

pub const ONOFF_SERVER: u16 = 0x1000;
pub const ONOFF_CLIENT: u16 = 0x1001;
pub const SENSOR_SERVER: u16 = 0x1100;
pub const SENSOR_SETUP_SERVER: u16 = 0x1101;
pub const SENSOR_CLIENT: u16 = 0x1102;

const ONOFF_GET: u32 = 0x8201;
const ONOFF_SET: u32 = 0x8202;
const ONOFF_SET_UNACK: u32 = 0x8203;
const ONOFF_STATUS: u32 = 0x8204;

const SENSOR_DESCRIPTOR_GET: u32 = 0x8230;
const SENSOR_DESCRIPTOR_STATUS: u32 = 0x51;
const SENSOR_GET: u32 = 0x8231;
const SENSOR_STATUS: u32 = 0x52;
const SENSOR_COLUMN_GET: u32 = 0x8232;
const SENSOR_COLUMN_STATUS: u32 = 0x53;
const SENSOR_SERIES_GET: u32 = 0x8233;
const SENSOR_SERIES_STATUS: u32 = 0x54;
const SENSOR_CADENCE_GET: u32 = 0x8234;
const SENSOR_CADENCE_SET: u32 = 0x55;
const SENSOR_CADENCE_SET_UNACK: u32 = 0x56;
const SENSOR_CADENCE_STATUS: u32 = 0x57;
const SENSOR_SETTINGS_GET: u32 = 0x8235;
const SENSOR_SETTINGS_STATUS: u32 = 0x58;
const SENSOR_SETTING_GET: u32 = 0x8236;
const SENSOR_SETTING_SET: u32 = 0x59;
const SENSOR_SETTING_SET_UNACK: u32 = 0x5A;
const SENSOR_SETTING_STATUS: u32 = 0x5B;

/// A Set with the same source, destination and TID as the last one, within
/// this long of it, is the same transaction again (Mesh Model 3.3.1.2.2).
const TID_WINDOW_MS: u64 = 6_000;
/// Properties one Sensor Server offers.
pub const SENSORS_MAX: usize = 8;
/// The longest sensor value (bytes).
pub const SENSOR_VALUE_MAX: usize = 4;
/// A changed sensor value is published at once, but no more often than this
/// (there is no Sensor Cadence to say otherwise).
const SENSOR_CHANGE_GAP_MS: u64 = 1_000;

// ── Generic Transition Time ───────────────────────────────────────────────

/// A Generic Transition Time (steps in bits 0-5, resolution in bits 6-7:
/// 100 ms, 1 s, 10 s, 10 min) in milliseconds. Steps 0x3F: no Default
/// Transition Time here, so at once (Mesh Model 1.1, 3.2.9.3).
pub fn transition_ms(b: u8) -> u64 {
    let steps = (b & 0x3F) as u64;
    if steps == 0x3F {
        return 0;
    }
    steps
        * match b >> 6 {
            0 => 100,
            1 => 1_000,
            2 => 10_000,
            _ => 600_000,
        }
}

/// Milliseconds as a Generic Transition Time, rounded up; past 0x3E steps
/// of 10 min, 0x3F: "unknown".
pub fn transition_byte(ms: u64) -> u8 {
    for (resolution, step) in [(0u8, 100u64), (1, 1_000), (2, 10_000), (3, 600_000)] {
        let steps = ms.div_ceil(step);
        if steps <= 0x3E {
            return (resolution << 6) | steps as u8;
        }
    }
    0x3F
}

// ── Sensor properties ─────────────────────────────────────────────────────

/// How a property's value is carried: its size, sign, resolution, the raw
/// value meaning "not known", and the range its characteristic allows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PropertyFormat {
    pub id: u16,
    pub len: u8,
    pub signed: bool,
    /// The value of one raw step, in the unit the board's code uses.
    pub step: f32,
    pub unknown: u32,
    pub min: f32,
    pub max: f32,
}

/// The properties the IDE offers (Mesh Device Properties, formats from the
/// GATT Specification Supplement).
pub const PROPERTIES: &[PropertyFormat] = &[
    // Present Ambient Temperature: Temperature 8, 0.5 degC.
    PropertyFormat { id: 0x004F, len: 1, signed: true, step: 0.5, unknown: 0x7F, min: -64.0, max: 63.0 },
    // Present Indoor Ambient Temperature: Temperature 8.
    PropertyFormat { id: 0x0056, len: 1, signed: true, step: 0.5, unknown: 0x7F, min: -64.0, max: 63.0 },
    // Present Device Operating Temperature: Temperature, 0.01 degC.
    PropertyFormat { id: 0x0054, len: 2, signed: true, step: 0.01, unknown: 0x8000, min: -273.15, max: 327.67 },
    // Present Ambient Relative Humidity: Humidity, 0.01 %.
    PropertyFormat { id: 0x0076, len: 2, signed: false, step: 0.01, unknown: 0xFFFF, min: 0.0, max: 100.0 },
    // Present Ambient Light Level: Illuminance, 0.01 lux.
    PropertyFormat { id: 0x004E, len: 3, signed: false, step: 0.01, unknown: 0xFF_FFFF, min: 0.0, max: 167_772.14 },
    // Motion Sensed: Percentage 8, 0.5 %.
    PropertyFormat { id: 0x0042, len: 1, signed: false, step: 0.5, unknown: 0xFF, min: 0.0, max: 100.0 },
    // People Count: Count 16.
    PropertyFormat { id: 0x004C, len: 2, signed: false, step: 1.0, unknown: 0xFFFF, min: 0.0, max: 65_534.0 },
    // Present Input Voltage: Voltage, 1/64 V, saturating at 1022 V.
    PropertyFormat { id: 0x0059, len: 2, signed: false, step: 1.0 / 64.0, unknown: 0xFFFF, min: 0.0, max: 1_022.0 },
];

pub fn property_format(id: u16) -> Option<&'static PropertyFormat> {
    PROPERTIES.iter().find(|p| p.id == id)
}

/// A sensor value as it travels: little-endian raw bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SensorValue {
    pub raw: [u8; SENSOR_VALUE_MAX],
    pub len: u8,
}

impl SensorValue {
    pub const EMPTY: SensorValue = SensorValue { raw: [0; SENSOR_VALUE_MAX], len: 0 };

    pub fn bytes(&self) -> &[u8] {
        &self.raw[..self.len as usize]
    }

    fn word(&self) -> u32 {
        let mut w = [0u8; 4];
        w[..self.len as usize].copy_from_slice(self.bytes());
        u32::from_le_bytes(w)
    }

    /// The raw value of `f` for `value`, held inside the characteristic's
    /// range and rounded; the "not known" one for NaN.
    pub fn encode(f: &PropertyFormat, value: f32) -> SensorValue {
        let bits = f.len as u32 * 8;
        let raw: u32 = if value.is_nan() {
            f.unknown
        } else {
            let x = value.clamp(f.min, f.max) / f.step;
            let r = if x >= 0.0 { x + 0.5 } else { x - 0.5 };
            if f.signed {
                let max = (1i64 << (bits - 1)) - 1;
                let min = -(1i64 << (bits - 1));
                // The not-known code sits at one end of the range: keep off it.
                let (lo, hi) = if f.unknown == (1u32 << (bits - 1)) { (min + 1, max) } else { (min, max - 1) };
                let v = (r as i64).clamp(lo, hi);
                (v as u32) & mask(bits)
            } else {
                let hi = mask(bits) as i64 - 1;
                (r as i64).clamp(0, hi) as u32
            }
        };
        let mut s = SensorValue { raw: [0; SENSOR_VALUE_MAX], len: f.len };
        s.raw[..f.len as usize].copy_from_slice(&raw.to_le_bytes()[..f.len as usize]);
        s
    }

    /// The value in `f`'s unit; None when the sensor said "not known" or the
    /// size is not the format's.
    pub fn decode(&self, f: &PropertyFormat) -> Option<f32> {
        if self.len != f.len {
            return None;
        }
        let w = self.word();
        if w == f.unknown {
            return None;
        }
        let bits = f.len as u32 * 8;
        let v = if f.signed && w & (1 << (bits - 1)) != 0 { w as i64 - (1i64 << bits) } else { w as i64 };
        Some(v as f32 * f.step)
    }
}

fn mask(bits: u32) -> u32 {
    if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 }
}

/// Marshalled Sensor Data: format A (property below 0x0800, 1..16 bytes) or
/// format B; a zero-length value is format B with length 0x7F.
pub fn marshal(property: u16, value: &[u8], out: &mut [u8]) -> usize {
    let len = value.len();
    if len != 0 && len <= 16 && property < 0x0800 {
        let h = (((len - 1) as u16) << 1) | (property << 5);
        out[..2].copy_from_slice(&h.to_le_bytes());
        out[2..2 + len].copy_from_slice(value);
        2 + len
    } else {
        out[0] = 0x01 | (if len == 0 { 0x7F } else { (len as u8 - 1) & 0x7F } << 1);
        out[1..3].copy_from_slice(&property.to_le_bytes());
        out[3..3 + len].copy_from_slice(value);
        3 + len
    }
}

/// The next (property, value) of a Sensor Status, and what is left after.
pub fn unmarshal(b: &[u8]) -> Option<(u16, &[u8], &[u8])> {
    let first = *b.first()?;
    if first & 0x01 == 0 {
        if b.len() < 2 {
            return None;
        }
        let h = u16::from_le_bytes([b[0], b[1]]);
        let len = ((h >> 1) & 0x0F) as usize + 1;
        let property = h >> 5;
        let rest = b.get(2..)?;
        if rest.len() < len {
            return None;
        }
        Some((property, &rest[..len], &rest[len..]))
    } else {
        if b.len() < 3 {
            return None;
        }
        let l = (first >> 1) as usize;
        let len = if l == 0x7F { 0 } else { l + 1 };
        let property = u16::from_le_bytes([b[1], b[2]]);
        let rest = &b[3..];
        if rest.len() < len {
            return None;
        }
        Some((property, &rest[..len], &rest[len..]))
    }
}

// ── Events for the glue ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelEvent {
    /// The OnOff Server's state changed, set from the network by `src` -
    /// at once, or when a transition got there.
    OnOffSet { src: u16, on: bool },
    /// A Generic OnOff Status the client heard: `src`'s present state.
    OnOffStatus { src: u16, on: bool },
    /// One property of a Sensor Status the client heard. An empty value:
    /// `src` has no such property.
    Sensor { src: u16, property: u16, value: SensorValue },
}

// ── The models ────────────────────────────────────────────────────────────

#[derive(Clone, Copy)]
struct Transaction {
    src: u16,
    dst: u16,
    tid: u8,
    at: u64,
}

/// A Set with a Transition Time or a Delay, under way. Off to On: On when
/// it starts; On to Off: Off when it ends (Mesh Model 3.1.1).
#[derive(Clone, Copy)]
struct Transition {
    src: u16,
    target: bool,
    start_at: u64,
    end_at: u64,
}

#[derive(Clone, Copy)]
struct Sensor {
    property: u16,
    value: SensorValue,
}

pub struct Models {
    /// The Generic OnOff Server's present state.
    pub on: bool,
    last: Option<Transaction>,
    transition: Option<Transition>,
    /// The Generic OnOff Client's last TID.
    pub tid: u8,
    sensors: [Sensor; SENSORS_MAX],
    sensor_count: usize,
    /// A sensor value changed since the last Sensor Status published.
    sensor_changed: bool,
    last_sensor_publish: Option<u64>,
    /// When each model (by index on the element) publishes next; 0: not
    /// scheduled. And the period it was scheduled for.
    next_publish: [u64; MODELS_MAX],
    period_of: [u8; MODELS_MAX],
}

impl Models {
    /// `properties`: the Sensor Server's - sent in ascending order, as Mesh
    /// Model 1.1 (4.2.14) asks, each once.
    pub fn new(properties: &[u16]) -> Self {
        let mut ids = [0u16; SENSORS_MAX];
        let mut n = 0;
        for p in properties {
            if n < SENSORS_MAX && *p != 0 && !ids[..n].contains(p) {
                ids[n] = *p;
                n += 1;
            }
        }
        ids[..n].sort_unstable();
        let mut sensors = [Sensor { property: 0, value: SensorValue::EMPTY }; SENSORS_MAX];
        for (i, id) in ids[..n].iter().enumerate() {
            let value = match property_format(*id) {
                Some(f) => SensorValue::encode(f, f32::NAN),
                None => SensorValue::EMPTY,
            };
            sensors[i] = Sensor { property: *id, value };
        }
        Models {
            on: false,
            last: None,
            transition: None,
            tid: 0,
            sensors,
            sensor_count: n,
            sensor_changed: false,
            last_sensor_publish: None,
            next_publish: [0; MODELS_MAX],
            period_of: [0; MODELS_MAX],
        }
    }

    fn sensors(&self) -> &[Sensor] {
        &self.sensors[..self.sensor_count]
    }

    /// A message the access layer delivers (the AppKey's; the destination
    /// one of the node's).
    pub fn handle(&mut self, s: &mut Stack, m: &Incoming<'_>, now: u64) {
        // The model `id`, when the element has it and it takes this message.
        let taking = |s: &Stack, id: u16| s.model_index(id).filter(|&i| s.model_takes(i, m.dst, m.key));
        match m.opcode {
            ONOFF_GET | ONOFF_SET | ONOFF_SET_UNACK => {
                if let Some(i) = taking(s, ONOFF_SERVER) {
                    self.onoff_server(s, i, m, now);
                }
            }
            ONOFF_STATUS => {
                let p = m.params;
                let valid = (p.len() == 1 || (p.len() == 3 && p[1] <= 1)) && p[0] <= 1;
                if valid && taking(s, ONOFF_CLIENT).is_some() {
                    s.event(Event::Model(ModelEvent::OnOffStatus { src: m.src, on: p[0] == 1 }));
                }
            }
            SENSOR_DESCRIPTOR_GET | SENSOR_GET | SENSOR_COLUMN_GET | SENSOR_SERIES_GET => {
                if taking(s, SENSOR_SERVER).is_some() {
                    self.sensor_server(s, m, now);
                }
            }
            SENSOR_CADENCE_GET | SENSOR_CADENCE_SET | SENSOR_CADENCE_SET_UNACK | SENSOR_SETTINGS_GET | SENSOR_SETTING_GET
            | SENSOR_SETTING_SET | SENSOR_SETTING_SET_UNACK => {
                if taking(s, SENSOR_SETUP_SERVER).is_some() {
                    self.sensor_setup_server(s, m, now);
                }
            }
            SENSOR_STATUS if taking(s, SENSOR_CLIENT).is_some() => {
                let mut rest = m.params;
                while let Some((property, value, next)) = unmarshal(rest) {
                    let mut v = SensorValue::EMPTY;
                    if value.len() <= SENSOR_VALUE_MAX {
                        v.raw[..value.len()].copy_from_slice(value);
                        v.len = value.len() as u8;
                        s.event(Event::Model(ModelEvent::Sensor { src: m.src, property, value: v }));
                    }
                    rest = next;
                }
            }
            _ => {}
        }
    }

    fn reply(s: &mut Stack, m: &Incoming<'_>, payload: &[u8], now: u64) {
        let ttl = m.reply_ttl(s.default_ttl);
        let _ = s.send_access(m.src, Key::App, ttl, payload, now);
    }

    // ── Generic OnOff Server ──

    /// Generic OnOff Status: the present state, and while a transition is
    /// under way its target and the time left.
    fn onoff_status(&self, now: u64, out: &mut [u8; 5]) -> usize {
        out[..3].copy_from_slice(&[0x82, 0x04, self.on as u8]);
        match self.transition {
            Some(t) if now < t.end_at => {
                out[3] = t.target as u8;
                out[4] = transition_byte(t.end_at - now);
                5
            }
            _ => 3,
        }
    }

    fn onoff_server(&mut self, s: &mut Stack, i: usize, m: &Incoming<'_>, now: u64) {
        let mut st = [0u8; 5];
        if m.opcode == ONOFF_GET {
            if m.params.is_empty() {
                let n = self.onoff_status(now, &mut st);
                Self::reply(s, m, &st[..n], now);
            }
            return;
        }
        let p = m.params;
        // OnOff and TID, then optionally Transition Time and Delay.
        if (p.len() != 2 && p.len() != 4) || p[0] > 1 {
            return;
        }
        let same = matches!(self.last, Some(t) if t.src == m.src && t.dst == m.dst && t.tid == p[1] && now.saturating_sub(t.at) < TID_WINDOW_MS);
        // Every copy refreshes the window: a client repeating its Set
        // within 6 s of the last copy is still on the one transaction.
        self.last = Some(Transaction { src: m.src, dst: m.dst, tid: p[1], at: now });
        if !same {
            let target = p[0] == 1;
            let (tt, delay) = if p.len() == 4 { (transition_ms(p[2]), p[3] as u64 * 5) } else { (0, 0) };
            self.transition = None;
            if tt == 0 && delay == 0 {
                self.on = target;
                s.event(Event::Model(ModelEvent::OnOffSet { src: m.src, on: target }));
            } else {
                self.transition = Some(Transition { src: m.src, target, start_at: now + delay, end_at: now + delay + tt });
            }
        }
        let n = self.onoff_status(now, &mut st);
        if m.opcode == ONOFF_SET {
            Self::reply(s, m, &st[..n], now);
        }
        // Published after every new transaction, changed or not - as nRF's
        // and Espressif's servers do (Mesh Protocol 3.7.5.1.2).
        if !same {
            let _ = s.publish(i, &st[..n], now);
        }
    }

    /// The board changed the state itself: no transition any more, and the
    /// state published - every call, so one that failed (NotReady) can be
    /// made again.
    pub fn onoff_local(&mut self, s: &mut Stack, on: bool, now: u64) -> Result<(), SendError> {
        let i = s.model_index(ONOFF_SERVER).ok_or(SendError::NoAddress)?;
        self.on = on;
        self.transition = None;
        let mut st = [0u8; 5];
        let n = self.onoff_status(now, &mut st);
        s.publish(i, &st[..n], now)?;
        Ok(())
    }

    /// A transition's steps: On at its start, the target at its end. The
    /// next time one is due.
    fn onoff_transition(&mut self, s: &mut Stack, now: u64) -> Option<u64> {
        let t = self.transition?;
        if t.target && !self.on && now >= t.start_at {
            self.on = true;
            s.event(Event::Model(ModelEvent::OnOffSet { src: t.src, on: true }));
        }
        if now < t.end_at {
            return Some(if t.target && !self.on { t.start_at } else { t.end_at });
        }
        self.transition = None;
        if self.on != t.target {
            self.on = t.target;
            s.event(Event::Model(ModelEvent::OnOffSet { src: t.src, on: t.target }));
        }
        if let Some(i) = s.model_index(ONOFF_SERVER) {
            let mut st = [0u8; 5];
            let n = self.onoff_status(now, &mut st);
            let _ = s.publish(i, &st[..n], now);
        }
        None
    }

    // ── Generic OnOff Client ──

    /// A Set (acknowledged or not) to `dst`, or to the client's publication
    /// address. A new TID every call: every call is a new transaction.
    pub fn onoff_set(&mut self, s: &mut Stack, dst: Option<u16>, on: bool, ack: bool, now: u64) -> Result<Option<u8>, SendError> {
        let i = s.model_index(ONOFF_CLIENT).ok_or(SendError::NoAddress)?;
        self.tid = self.tid.wrapping_add(1);
        let op = if ack { 0x02 } else { 0x03 };
        let msg = [0x82, op, on as u8, self.tid];
        match dst {
            Some(d) => s.send_access(d, Key::App, 0xFF, &msg, now),
            None => s.publish(i, &msg, now),
        }
    }

    pub fn onoff_get(&mut self, s: &mut Stack, dst: Option<u16>, now: u64) -> Result<Option<u8>, SendError> {
        let i = s.model_index(ONOFF_CLIENT).ok_or(SendError::NoAddress)?;
        let msg = [0x82, 0x01];
        match dst {
            Some(d) => s.send_access(d, Key::App, 0xFF, &msg, now),
            None => s.publish(i, &msg, now),
        }
    }

    // ── Sensor Server ──

    /// Set a property's value (NaN: not known). False if the server has no
    /// such property. A new value is published (`poll`), at most once a
    /// second.
    pub fn sensor_set(&mut self, property: u16, value: f32) -> bool {
        let f = match property_format(property) {
            Some(f) => f,
            None => return false,
        };
        match self.sensors[..self.sensor_count].iter_mut().find(|x| x.property == property) {
            Some(x) => {
                let v = SensorValue::encode(f, value);
                if v != x.value {
                    x.value = v;
                    self.sensor_changed = true;
                }
                true
            }
            None => false,
        }
    }

    /// A Sensor Status: every property, or only `property` (zero length if
    /// the server has none such).
    pub fn sensor_status(&self, property: Option<u16>, out: &mut [u8]) -> usize {
        out[0] = SENSOR_STATUS as u8;
        let mut n = 1;
        match property {
            Some(p) => match self.sensors().iter().find(|x| x.property == p) {
                Some(x) => n += marshal(p, x.value.bytes(), &mut out[n..]),
                None => n += marshal(p, &[], &mut out[n..]),
            },
            None => {
                for x in self.sensors() {
                    if n + 3 + x.value.len as usize > out.len() {
                        break;
                    }
                    n += marshal(x.property, x.value.bytes(), &mut out[n..]);
                }
            }
        }
        n
    }

    fn sensor_server(&mut self, s: &mut Stack, m: &Incoming<'_>, now: u64) {
        let p = m.params;
        let asked = if p.len() >= 2 { Some(u16::from_le_bytes([p[0], p[1]])) } else { None };
        // Property ID 0x0000 is prohibited in every Get.
        if asked == Some(0) {
            return;
        }
        let mut out = [0u8; ACCESS_MAX];
        match m.opcode {
            SENSOR_DESCRIPTOR_GET if p.is_empty() || p.len() == 2 => {
                out[0] = SENSOR_DESCRIPTOR_STATUS as u8;
                let mut n = 1;
                match asked {
                    Some(id) if !self.sensors().iter().any(|x| x.property == id) => {
                        // Not one of ours: the Property ID alone.
                        out[1..3].copy_from_slice(&id.to_le_bytes());
                        n = 3;
                    }
                    _ => {
                        for x in self.sensors().iter().filter(|x| match asked {
                            None => true,
                            Some(a) => a == x.property,
                        }) {
                            if n + 8 > out.len() {
                                break;
                            }
                            // Tolerances, sampling function, measurement
                            // period and update interval: all "unspecified".
                            out[n..n + 2].copy_from_slice(&x.property.to_le_bytes());
                            n += 8;
                        }
                    }
                }
                Self::reply(s, m, &out[..n], now);
            }
            SENSOR_GET if p.is_empty() || p.len() == 2 => {
                let n = self.sensor_status(asked, &mut out);
                Self::reply(s, m, &out[..n], now);
            }
            SENSOR_COLUMN_GET if p.len() >= 3 => {
                // No series data: the Property ID alone.
                Self::reply(s, m, &[SENSOR_COLUMN_STATUS as u8, p[0], p[1]], now);
            }
            SENSOR_SERIES_GET if p.len() >= 2 => {
                Self::reply(s, m, &[SENSOR_SERIES_STATUS as u8, p[0], p[1]], now);
            }
            _ => {}
        }
    }

    // ── Sensor Setup Server: no cadence, no settings ──

    fn sensor_setup_server(&mut self, s: &mut Stack, m: &Incoming<'_>, now: u64) {
        let p = m.params;
        if p.len() < 2 || u16::from_le_bytes([p[0], p[1]]) == 0 {
            return;
        }
        // A Setting Property ID 0x0000 is prohibited too.
        let setting_zero = p.len() >= 4 && u16::from_le_bytes([p[2], p[3]]) == 0;
        match m.opcode {
            SENSOR_CADENCE_GET if p.len() == 2 => {
                Self::reply(s, m, &[SENSOR_CADENCE_STATUS as u8, p[0], p[1]], now);
            }
            // A Cadence Set is 8 octets at the least (one-octet values).
            SENSOR_CADENCE_SET if p.len() >= 8 => {
                Self::reply(s, m, &[SENSOR_CADENCE_STATUS as u8, p[0], p[1]], now);
            }
            SENSOR_SETTINGS_GET if p.len() == 2 => {
                Self::reply(s, m, &[SENSOR_SETTINGS_STATUS as u8, p[0], p[1]], now);
            }
            SENSOR_SETTING_GET if p.len() == 4 && !setting_zero => {
                Self::reply(s, m, &[SENSOR_SETTING_STATUS as u8, p[0], p[1], p[2], p[3]], now);
            }
            SENSOR_SETTING_SET if p.len() >= 5 && !setting_zero => {
                Self::reply(s, m, &[SENSOR_SETTING_STATUS as u8, p[0], p[1], p[2], p[3]], now);
            }
            _ => {}
        }
    }

    // ── Sensor Client ──

    pub fn sensor_get(&mut self, s: &mut Stack, dst: Option<u16>, property: Option<u16>, now: u64) -> Result<Option<u8>, SendError> {
        let i = s.model_index(SENSOR_CLIENT).ok_or(SendError::NoAddress)?;
        let mut msg = [0x82, 0x31, 0, 0];
        let n = match property {
            Some(p) => {
                msg[2..4].copy_from_slice(&p.to_le_bytes());
                4
            }
            None => 2,
        };
        match dst {
            Some(d) => s.send_access(d, Key::App, 0xFF, &msg[..n], now),
            None => s.publish(i, &msg[..n], now),
        }
    }

    /// The Sensor Server's values to its publication address now.
    pub fn sensor_publish(&mut self, s: &mut Stack, now: u64) -> Result<Option<u8>, SendError> {
        let i = s.model_index(SENSOR_SERVER).ok_or(SendError::NoAddress)?;
        let mut out = [0u8; ACCESS_MAX];
        let n = self.sensor_status(None, &mut out);
        let sent = s.publish(i, &out[..n], now)?;
        self.sensor_changed = false;
        self.last_sensor_publish = Some(now);
        Ok(sent)
    }

    // ── Publication ──

    /// Transitions, periodic publications and changed sensor values; the
    /// next time anything is due.
    pub fn poll(&mut self, s: &mut Stack, now: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        let mut due = |at: u64| next = Some(next.map_or(at, |n: u64| n.min(at)));
        if let Some(at) = self.onoff_transition(s, now) {
            due(at);
        }
        for i in 0..s.model_count {
            let id = s.models[i].id;
            if id != ONOFF_SERVER && id != SENSOR_SERVER {
                continue;
            }
            let pb = s.models[i].publication;
            let period = pb.period_ms() as u64;
            if period == 0 || pb.address == UNASSIGNED {
                self.next_publish[i] = 0;
                continue;
            }
            // A new period starts counting at once (Config Model
            // Publication Set), not when the old one would have run out.
            if self.next_publish[i] == 0 || self.period_of[i] != pb.period {
                self.next_publish[i] = now + period;
                self.period_of[i] = pb.period;
            }
            if now >= self.next_publish[i] {
                let sent = if id == ONOFF_SERVER {
                    let mut st = [0u8; 5];
                    let n = self.onoff_status(now, &mut st);
                    s.publish(i, &st[..n], now)
                } else {
                    self.sensor_publish(s, now)
                };
                self.next_publish[i] = match sent {
                    // Not yet: the SEQ store or a busy segmented slot. Soon.
                    Err(SendError::SeqNotStored) | Err(SendError::Busy) => now + 1_000,
                    _ => now + period,
                };
            }
            due(self.next_publish[i]);
        }
        // A changed sensor value goes out at once - a second after the last
        // publication at the soonest.
        let changed = self.sensor_changed;
        if let Some(i) = s.model_index(SENSOR_SERVER).filter(|_| changed) {
            if s.models[i].publication.address == UNASSIGNED {
                self.sensor_changed = false;
            } else {
                let at = self.last_sensor_publish.map_or(now, |t| t + SENSOR_CHANGE_GAP_MS);
                if now < at {
                    due(at);
                } else if let Err(SendError::SeqNotStored | SendError::Busy) = self.sensor_publish(s, now) {
                    due(now + 1_000);
                }
            }
        }
        next
    }
}

// ── The node: stack and models together ───────────────────────────────────

pub struct Mesh {
    pub stack: Stack,
    pub models: Models,
}

impl Mesh {
    pub fn new(setup: &Setup, properties: &[u16], stored_seq: Option<u32>) -> Self {
        Mesh { stack: Stack::new(setup, stored_seq), models: Models::new(properties) }
    }

    /// A Network PDU off the air.
    pub fn receive(&mut self, pdu: &[u8], now: u64) {
        let models = &mut self.models;
        self.stack.receive(pdu, now, &mut |s: &mut Stack, m: &Incoming<'_>| models.handle(s, m, now));
        self.local(now);
    }

    /// What the node sent to its own models, to them - and what they answer
    /// to themselves in turn, a few rounds at most.
    pub fn local(&mut self, now: u64) {
        for _ in 0..4 * LOOPBACK {
            let Some(l) = self.stack.take_loopback() else {
                return;
            };
            let models = &mut self.models;
            self.stack.deliver_local(&l, now, &mut |s: &mut Stack, m: &Incoming<'_>| models.handle(s, m, now));
        }
    }

    /// Timers and publications; the next time anything is due.
    pub fn poll(&mut self, now: u64) -> Option<u64> {
        self.stack.poll(now);
        let b = self.models.poll(&mut self.stack, now);
        self.local(now);
        // Again: what the models just sent has timers of its own. At the
        // same `now` it sends nothing twice.
        let a = self.stack.poll(now);
        match (a, b) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}
