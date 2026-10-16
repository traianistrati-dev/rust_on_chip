
// ── Bluetooth Mesh: the node itself ───────────────────────────────────────
// No radio and no clock here: Network PDUs in (the bytes of an AD 0x2A off
// the air), Network PDUs out (`Outbox`, for the bearer to advertise) and the
// time in milliseconds passed in. The keys, the network layer (obfuscation,
// AES-CCM, the message cache, relaying), the lower and upper transport
// (segmentation, Segment Acknowledgments, AppKey and DevKey), the replay
// list, the access layer and the Configuration Server. The IDE runs this
// very text through its own tests (`ble_mesh_core_tests`), against the
// sample data of the Mesh Profile specification.
//
// Provisioning is the IDE's: the keys, the IV index and the address come
// from the project (secrets.rs and the block above). Not here yet: the IV
// Update procedure and the Secure Network Beacon (the IV index never moves:
// a network another provisioner updates leaves this node behind, and after
// 2^24 PDUs it runs out of sequence numbers), the Health Server the
// specification asks of every node, the Friend and Low Power features,
// segmented control messages, virtual addresses and the GATT bearer.

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use aes::Aes128;
use ccm::aead::AeadInPlace;
use ccm::consts::{U13, U4, U8};
use ccm::Ccm;
use cmac::{Cmac, Mac};

// ── Sizes ─────────────────────────────────────────────────────────────────

/// The longest Network PDU: what fits in one advertisement (31 bytes less
/// the AD length and type).
pub const NET_MAX: usize = 29;
/// Segments a message may span, sent or received. 12 bytes each: an access
/// message of up to `ACCESS_MAX` bytes.
pub const SEG_MAX: usize = 8;
/// The longest access message (opcode and parameters) the node sends or
/// takes: SEG_MAX segments less the 4-byte TransMIC.
pub const ACCESS_MAX: usize = SEG_MAX * 12 - 4;
/// Network PDUs remembered, so a copy heard again (relayed, or the same
/// advertisement twice) is dropped before it is decrypted.
pub const NET_CACHE: usize = 32;
/// Sources the replay list tracks. A source past these is refused while the
/// list is full - the composition data's CRPL.
pub const RPL_SIZE: usize = 32;
/// Segmented messages taken in at once (from different sources).
pub const SEG_RX: usize = 2;
/// Segmented messages sent at once.
pub const SEG_TX: usize = 2;
/// Group addresses one model subscribes to.
pub const SUBS_MAX: usize = 4;
/// Models on the element, the Configuration Server included.
pub const MODELS_MAX: usize = 8;
/// Network PDUs waiting for the bearer.
pub const OUTBOX: usize = 16;
/// Events waiting for the glue: a Sensor Status alone can bring SENSORS_MAX.
pub const EVENTS: usize = 16;
/// Messages the node sends to its own models, waiting to be delivered.
pub const LOOPBACK: usize = 4;
/// Publications waiting to be sent again (Publish Retransmit).
pub const PUB_RTX: usize = 4;
/// The SEQ is stored this far ahead of use: a reset loses at most this many
/// numbers, and the flash takes one record per this many PDUs sent.
pub const SEQ_STORE_RATE: u32 = 256;

// ── Addresses ─────────────────────────────────────────────────────────────

pub const UNASSIGNED: u16 = 0x0000;
pub const ALL_PROXIES: u16 = 0xFFFC;
pub const ALL_FRIENDS: u16 = 0xFFFD;
pub const ALL_RELAYS: u16 = 0xFFFE;
pub const ALL_NODES: u16 = 0xFFFF;

pub const fn is_unicast(a: u16) -> bool {
    a != 0 && a & 0x8000 == 0
}

pub const fn is_group(a: u16) -> bool {
    a >= 0xC000
}

pub const fn is_virtual(a: u16) -> bool {
    a & 0xC000 == 0x8000
}

// ── Keys from secrets.rs ──────────────────────────────────────────────────

/// 32 hex digits into a 128-bit key. In a `const`, a malformed key is a
/// COMPILE error naming this function, never a node on the wrong network.
pub const fn hex16(s: &str) -> [u8; 16] {
    let b = s.as_bytes();
    if b.len() != 32 {
        panic!("a mesh key is 32 hex digits");
    }
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 16 {
        out[i] = (hex_digit(b[2 * i]) << 4) | hex_digit(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn hex_digit(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("a mesh key is 32 hex digits"),
    }
}

// ── The cryptographic functions (Mesh Profile 3.8.2) ──────────────────────

pub fn aes_ecb(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let cipher = Aes128::new(GenericArray::from_slice(key));
    let mut b = GenericArray::clone_from_slice(block);
    cipher.encrypt_block(&mut b);
    let mut out = [0u8; 16];
    out.copy_from_slice(&b);
    out
}

/// AES-CMAC of the parts, one after the other.
pub fn aes_cmac(key: &[u8; 16], parts: &[&[u8]]) -> [u8; 16] {
    let mut mac = <Cmac<Aes128> as KeyInit>::new(GenericArray::from_slice(key));
    for p in parts {
        mac.update(p);
    }
    let mut out = [0u8; 16];
    out.copy_from_slice(&mac.finalize().into_bytes());
    out
}

pub fn s1(m: &[u8]) -> [u8; 16] {
    aes_cmac(&[0u8; 16], &[m])
}

pub fn k1(n: &[u8], salt: &[u8; 16], p: &[u8]) -> [u8; 16] {
    let t = aes_cmac(salt, &[n]);
    aes_cmac(&t, &[p])
}

/// The keys k2 makes of a NetKey (master credentials: `p` = [0x00]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetKeys {
    pub nid: u8,
    pub encryption: [u8; 16],
    pub privacy: [u8; 16],
}

pub fn k2(n: &[u8; 16], p: &[u8]) -> NetKeys {
    let t = aes_cmac(&s1(b"smk2"), &[n]);
    let t1 = aes_cmac(&t, &[p, &[0x01u8]]);
    let t2 = aes_cmac(&t, &[&t1, p, &[0x02u8]]);
    let t3 = aes_cmac(&t, &[&t2, p, &[0x03u8]]);
    NetKeys { nid: t1[15] & 0x7F, encryption: t2, privacy: t3 }
}

/// The Network ID: what a Secure Network Beacon and a proxy advertise.
pub fn k3(n: &[u8; 16]) -> [u8; 8] {
    let t = aes_cmac(&s1(b"smk3"), &[n]);
    let r = aes_cmac(&t, &[b"id64", &[0x01u8]]);
    let mut out = [0u8; 8];
    out.copy_from_slice(&r[8..]);
    out
}

/// A Label UUID's 16-bit virtual address: 0b10 and 14 bits of its hash.
pub fn virtual_address(label: &[u8; 16]) -> u16 {
    let h = aes_cmac(&s1(b"vtad"), &[label]);
    0x8000 | (u16::from_be_bytes([h[14], h[15]]) & 0x3FFF)
}

/// An AppKey's AID: the 6 bits that say which key a message is under.
pub fn k4(n: &[u8; 16]) -> u8 {
    let t = aes_cmac(&s1(b"smk4"), &[n]);
    aes_cmac(&t, &[b"id6", &[0x01u8]])[15] & 0x3F
}

/// AES-CCM: `data` encrypted in place, its MIC (4 or 8 bytes) into `mic`.
pub fn ccm_seal(key: &[u8; 16], nonce: &[u8; 13], aad: &[u8], data: &mut [u8], mic: &mut [u8]) -> bool {
    let k = GenericArray::from_slice(key);
    let n = GenericArray::from_slice(nonce);
    match mic.len() {
        4 => match Ccm::<Aes128, U4, U13>::new(k).encrypt_in_place_detached(n, aad, data) {
            Ok(tag) => {
                mic.copy_from_slice(&tag);
                true
            }
            Err(_) => false,
        },
        8 => match Ccm::<Aes128, U8, U13>::new(k).encrypt_in_place_detached(n, aad, data) {
            Ok(tag) => {
                mic.copy_from_slice(&tag);
                true
            }
            Err(_) => false,
        },
        _ => false,
    }
}

/// AES-CCM the other way: `data` decrypted in place if `mic` holds.
pub fn ccm_open(key: &[u8; 16], nonce: &[u8; 13], aad: &[u8], data: &mut [u8], mic: &[u8]) -> bool {
    let k = GenericArray::from_slice(key);
    let n = GenericArray::from_slice(nonce);
    match mic.len() {
        4 => Ccm::<Aes128, U4, U13>::new(k)
            .decrypt_in_place_detached(n, aad, data, GenericArray::from_slice(mic))
            .is_ok(),
        8 => Ccm::<Aes128, U8, U13>::new(k)
            .decrypt_in_place_detached(n, aad, data, GenericArray::from_slice(mic))
            .is_ok(),
        _ => false,
    }
}

// ── Nonces (Mesh Profile 3.8.5) ───────────────────────────────────────────

pub fn network_nonce(ctl_ttl: u8, seq: u32, src: u16, iv_index: u32) -> [u8; 13] {
    let mut n = [0u8; 13];
    n[0] = 0x00;
    n[1] = ctl_ttl;
    n[2..5].copy_from_slice(&seq.to_be_bytes()[1..]);
    n[5..7].copy_from_slice(&src.to_be_bytes());
    n[9..13].copy_from_slice(&iv_index.to_be_bytes());
    n
}

/// The application nonce (`device` false) or the device nonce.
pub fn access_nonce(device: bool, aszmic: bool, seq: u32, src: u16, dst: u16, iv_index: u32) -> [u8; 13] {
    let mut n = [0u8; 13];
    n[0] = if device { 0x02 } else { 0x01 };
    n[1] = (aszmic as u8) << 7;
    n[2..5].copy_from_slice(&seq.to_be_bytes()[1..]);
    n[5..7].copy_from_slice(&src.to_be_bytes());
    n[7..9].copy_from_slice(&dst.to_be_bytes());
    n[9..13].copy_from_slice(&iv_index.to_be_bytes());
    n
}

// ── The network layer (Mesh Profile 3.4) ──────────────────────────────────

/// A Network PDU in the clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NetPdu {
    /// The IV index it is under (the IVI bit picks the current or the one
    /// before).
    pub iv_index: u32,
    pub ctl: bool,
    pub ttl: u8,
    pub seq: u32,
    pub src: u16,
    pub dst: u16,
    /// The Lower Transport PDU: up to 16 bytes for access, 12 for control.
    pub transport: [u8; 16],
    pub len: usize,
}

impl NetPdu {
    pub fn transport(&self) -> &[u8] {
        &self.transport[..self.len]
    }
}

fn pecb(privacy: &[u8; 16], iv_index: u32, pdu: &[u8]) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[5..9].copy_from_slice(&iv_index.to_be_bytes());
    b[9..16].copy_from_slice(&pdu[7..14]);
    aes_ecb(privacy, &b)
}

/// Encrypt and obfuscate. The length of the PDU, or 0 if the transport PDU
/// does not fit (16 bytes access, 12 control).
pub fn net_encode(keys: &NetKeys, p: &NetPdu, out: &mut [u8; NET_MAX]) -> usize {
    let mic = if p.ctl { 8 } else { 4 };
    if p.len == 0 || 9 + p.len + mic > NET_MAX {
        return 0;
    }
    let n = 9 + p.len + mic;
    out[0] = (((p.iv_index & 1) as u8) << 7) | keys.nid;
    out[1] = ((p.ctl as u8) << 7) | (p.ttl & 0x7F);
    out[2..5].copy_from_slice(&p.seq.to_be_bytes()[1..]);
    out[5..7].copy_from_slice(&p.src.to_be_bytes());
    out[7..9].copy_from_slice(&p.dst.to_be_bytes());
    out[9..9 + p.len].copy_from_slice(p.transport());
    let nonce = network_nonce(out[1], p.seq, p.src, p.iv_index);
    let (body, tail) = out[7..n].split_at_mut(2 + p.len);
    if !ccm_seal(&keys.encryption, &nonce, &[], body, tail) {
        return 0;
    }
    let pecb = pecb(&keys.privacy, p.iv_index, &out[..n]);
    for i in 0..6 {
        out[1 + i] ^= pecb[i];
    }
    n
}

/// Deobfuscate and decrypt under `iv_index` (or the one before it, as IVI
/// says). None: not this network, too short, a bad NetMIC, or a source that
/// is not a unicast address.
pub fn net_decode(keys: &NetKeys, iv_index: u32, pdu: &[u8]) -> Option<NetPdu> {
    if pdu.len() < 14 || pdu.len() > NET_MAX || pdu[0] & 0x7F != keys.nid {
        return None;
    }
    let iv = if (pdu[0] >> 7) as u32 == iv_index & 1 { iv_index } else { iv_index.checked_sub(1)? };
    let pecb = pecb(&keys.privacy, iv, pdu);
    let mut b = [0u8; NET_MAX];
    b[..pdu.len()].copy_from_slice(pdu);
    for i in 0..6 {
        b[1 + i] ^= pecb[i];
    }
    let ctl = b[1] & 0x80 != 0;
    let mic = if ctl { 8 } else { 4 };
    if pdu.len() < 9 + 1 + mic {
        return None;
    }
    let seq = u32::from_be_bytes([0, b[2], b[3], b[4]]);
    let src = u16::from_be_bytes([b[5], b[6]]);
    let nonce = network_nonce(b[1], seq, src, iv);
    let end = pdu.len() - mic;
    let (body, tail) = b[7..pdu.len()].split_at_mut(end - 7);
    if !ccm_open(&keys.encryption, &nonce, &[], body, tail) || !is_unicast(src) {
        return None;
    }
    let len = end - 9;
    let mut transport = [0u8; 16];
    transport[..len].copy_from_slice(&b[9..end]);
    Some(NetPdu {
        iv_index: iv,
        ctl,
        ttl: b[1] & 0x7F,
        seq,
        src,
        dst: u16::from_be_bytes([b[7], b[8]]),
        transport,
        len,
    })
}

/// The Network Message Cache: (SRC, SEQ) of the PDUs taken in lately.
struct NetCache {
    e: [(u16, u32); NET_CACHE],
    next: usize,
}

impl NetCache {
    const fn new() -> Self {
        NetCache { e: [(0, 0); NET_CACHE], next: 0 }
    }

    fn has(&self, src: u16, seq: u32) -> bool {
        self.e.iter().any(|&(s, q)| s == src && q == seq)
    }

    fn add(&mut self, src: u16, seq: u32) {
        self.e[self.next] = (src, seq);
        self.next = (self.next + 1) % NET_CACHE;
    }
}

// ── The replay protection list (Mesh Profile 3.8.8) ───────────────────────

#[derive(Clone, Copy)]
struct RplEntry {
    src: u16,
    iv_index: u32,
    seq: u32,
    /// The SeqAuth of the last segmented message taken in whole from `src`:
    /// its repeats are acknowledged, never delivered again, once its
    /// reassembly slot is gone.
    seg_auth: u64,
}

/// The newest (IV index, SEQ) taken in from each source. In RAM: after a
/// reset the node takes again what it had already taken, until it hears the
/// source once more.
struct Rpl {
    e: [RplEntry; RPL_SIZE],
}

impl Rpl {
    const fn new() -> Self {
        Rpl { e: [RplEntry { src: 0, iv_index: 0, seq: 0, seg_auth: 0 }; RPL_SIZE] }
    }

    /// Whether a PDU from `src` is new.
    fn fresh(&self, src: u16, iv_index: u32, seq: u32) -> bool {
        match self.e.iter().find(|e| e.src == src) {
            Some(e) => iv_index > e.iv_index || (iv_index == e.iv_index && seq > e.seq),
            None => self.e.iter().any(|e| e.src == 0),
        }
    }

    fn take(&mut self, src: u16, iv_index: u32, seq: u32) {
        self.completed(src, iv_index, seq, None);
    }

    /// `take`, and for the segment that completed a message its SeqAuth.
    fn completed(&mut self, src: u16, iv_index: u32, seq: u32, seg_auth: Option<u64>) {
        let at = match self.e.iter().position(|e| e.src == src) {
            Some(i) => i,
            None => match self.e.iter().position(|e| e.src == 0) {
                Some(i) => i,
                None => return,
            },
        };
        let old = if self.e[at].src == src { self.e[at].seg_auth } else { 0 };
        self.e[at] = RplEntry { src, iv_index, seq, seg_auth: seg_auth.unwrap_or(old) };
    }

    fn seg_auth(&self, src: u16) -> u64 {
        self.e.iter().find(|e| e.src == src).map_or(0, |e| e.seg_auth)
    }
}

// ── What goes out, and what the glue is told ──────────────────────────────

/// A Network PDU to advertise, `transmits` times `interval_ms` apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Transmission {
    pub pdu: [u8; NET_MAX],
    pub len: usize,
    pub transmits: u8,
    pub interval_ms: u16,
}

pub struct Outbox {
    q: [Transmission; OUTBOX],
    head: usize,
    count: usize,
    /// PDUs dropped because the bearer fell behind.
    pub dropped: u32,
}

impl Outbox {
    pub const fn new() -> Self {
        Outbox {
            q: [Transmission { pdu: [0; NET_MAX], len: 0, transmits: 0, interval_ms: 0 }; OUTBOX],
            head: 0,
            count: 0,
            dropped: 0,
        }
    }

    fn push(&mut self, t: Transmission) {
        if self.count == OUTBOX {
            self.dropped += 1;
            return;
        }
        self.q[(self.head + self.count) % OUTBOX] = t;
        self.count += 1;
    }

    pub fn pop(&mut self) -> Option<Transmission> {
        if self.count == 0 {
            return None;
        }
        let t = self.q[self.head];
        self.head = (self.head + 1) % OUTBOX;
        self.count -= 1;
        Some(t)
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// About how long the bearer takes to send what is queued: each PDU
    /// `transmits` events, an interval and the controller's 0..10 ms apart.
    pub fn airtime_ms(&self) -> u64 {
        (0..self.count)
            .map(|i| {
                let t = &self.q[(self.head + i) % OUTBOX];
                t.transmits.max(1) as u64 * (t.interval_ms as u64 + 10) + 10
            })
            .sum()
    }
}

/// Which key an access message is under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// The node's own DevKey: the Configuration Server's messages.
    Device,
    /// The network's AppKey (index 0): every other model's.
    App,
}

/// Why a message could not be sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendError {
    /// Opcode and parameters over ACCESS_MAX bytes.
    TooLong,
    /// SEG_TX segmented messages are already on their way.
    Busy,
    /// The SEQ is not stored far enough ahead yet (the glue stores it).
    SeqNotStored,
    /// No address to send to: an unassigned publish address.
    NoAddress,
    /// A Config Node Reset took the node off the network (until a reset:
    /// the IDE's keys come back with the firmware).
    Removed,
}

/// What the glue learns from the node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A segmented message `handle` reached every segment's destination
    /// (`ok`), or gave up. A group's is `ok` once its segments went out.
    Sent { handle: u8, ok: bool },
    /// A model's message: what the models below made of it.
    Model(ModelEvent),
    /// The Configuration Server changed a publication, a subscription, a
    /// binding, the default TTL, the relay or the network transmit.
    ConfigChanged,
    /// A Config Node Reset: the node answered it and has left the network.
    Removed,
}

// ── The node's configuration: models, publications, subscriptions ─────────

pub const CONFIG_SERVER: u16 = 0x0000;

/// A model's publication (Mesh Profile 4.2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publication {
    pub address: u16,
    /// 0xFF: the default TTL.
    pub ttl: u8,
    /// Steps (bits 0-5) and resolution (bits 6-7: 100 ms, 1 s, 10 s,
    /// 10 min). 0: no periodic publication.
    pub period: u8,
    /// Count (bits 0-2) and interval steps of 50 ms (bits 3-7).
    pub retransmit: u8,
    pub app_index: u16,
    pub credentials: bool,
}

impl Publication {
    pub const NONE: Publication =
        Publication { address: UNASSIGNED, ttl: 0xFF, period: 0, retransmit: 0, app_index: 0, credentials: false };

    /// The period in milliseconds; 0 when there is none.
    pub fn period_ms(&self) -> u32 {
        let steps = (self.period & 0x3F) as u32;
        steps
            * match self.period >> 6 {
                0 => 100,
                1 => 1_000,
                2 => 10_000,
                _ => 600_000,
            }
    }
}

/// A model on the element, as the IDE sets it up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelSetup {
    pub id: u16,
    pub publication: Publication,
    pub subscribe: &'static [u16],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelState {
    pub id: u16,
    pub publication: Publication,
    pub subs: [u16; SUBS_MAX],
    /// Bound to the AppKey. The Configuration Server is never.
    pub bound: bool,
}

impl ModelState {
    pub fn subscribed(&self, a: u16) -> bool {
        a != UNASSIGNED && self.subs.contains(&a)
    }
}

/// Everything the IDE puts into a node.
pub struct Setup {
    pub address: u16,
    pub iv_index: u32,
    pub net_key: [u8; 16],
    pub app_key: [u8; 16],
    pub dev_key: [u8; 16],
    pub default_ttl: u8,
    pub relay: bool,
    /// Company, product and version ids of the composition data.
    pub cid: u16,
    pub pid: u16,
    pub vid: u16,
    /// The models after the Configuration Server, which every node has.
    pub models: &'static [ModelSetup],
}

/// Network Transmit and Relay Retransmit: count (bits 0-2, transmissions
/// less one) and interval steps of 10 ms (bits 3-7).
pub fn transmit_of(state: u8) -> (u8, u16) {
    ((state & 0x07) + 1, (((state >> 3) as u16) + 1) * 10)
}

// ── Segmented messages ────────────────────────────────────────────────────

/// Mesh Profile 3.5.3.3: the segment transmission timer (200 + 50 x TTL ms)
/// and the acknowledgment timer (150 + 50 x TTL ms).
fn seg_tx_timeout(ttl: u8) -> u64 {
    200 + 50 * ttl as u64
}

fn seg_ack_timeout(ttl: u8) -> u64 {
    150 + 50 * ttl as u64
}

/// The incomplete timer: a message whose segments stop coming is dropped.
const SEG_INCOMPLETE_MS: u64 = 10_000;
/// A message taken in whole is remembered this long, to acknowledge the
/// segments its sender still repeats.
const SEG_DONE_KEEP_MS: u64 = 10_000;
/// Rounds of segments to a unicast address before giving up; to a group
/// (no acknowledgments) every round is sent.
const SEG_RETRANSMITS: u8 = 4;
const SEG_GROUP_ROUNDS: u8 = 3;
const SEG_GROUP_GAP_MS: u64 = 300;

#[derive(Clone, Copy)]
struct SegRx {
    active: bool,
    done: bool,
    src: u16,
    dst: u16,
    /// IV index << 24 | the SEQ of the first segment.
    seq_auth: u64,
    /// Byte 0 of the segments: AKF and AID.
    head: u8,
    szmic: bool,
    seg_n: u8,
    got: u32,
    buf: [u8; SEG_MAX * 12],
    last_len: usize,
    ttl: u8,
    ack_at: Option<u64>,
    drop_at: u64,
    /// When the last acknowledgment went out (repeats of a whole message
    /// are acknowledged at most every SEG_REACK_MS).
    last_ack: u64,
}

/// The least time between two acknowledgments of a message taken in whole
/// (Mesh Protocol 1.1's SAR Receiver default).
const SEG_REACK_MS: u64 = 150;

impl SegRx {
    const IDLE: SegRx = SegRx {
        active: false,
        done: false,
        src: 0,
        dst: 0,
        seq_auth: 0,
        head: 0,
        szmic: false,
        seg_n: 0,
        got: 0,
        buf: [0; SEG_MAX * 12],
        last_len: 0,
        ttl: 0,
        ack_at: None,
        drop_at: 0,
        last_ack: 0,
    };

    fn all(&self) -> u32 {
        all_segments(self.seg_n)
    }
}

/// The Block Ack of every segment up to `seg_n`.
fn all_segments(seg_n: u8) -> u32 {
    if seg_n >= 31 { u32::MAX } else { (1u32 << (seg_n + 1)) - 1 }
}

#[derive(Clone, Copy)]
struct SegTx {
    active: bool,
    handle: u8,
    dst: u16,
    /// Byte 0 of the segments: SEG, AKF and AID.
    head: u8,
    iv_index: u32,
    seq_auth: u32,
    seg_n: u8,
    acked: u32,
    buf: [u8; SEG_MAX * 12],
    len: usize,
    ttl: u8,
    tries: u8,
    next_at: u64,
}

impl SegTx {
    const IDLE: SegTx = SegTx {
        active: false,
        handle: 0,
        dst: 0,
        head: 0,
        iv_index: 0,
        seq_auth: 0,
        seg_n: 0,
        acked: 0,
        buf: [0; SEG_MAX * 12],
        len: 0,
        ttl: 0,
        tries: 0,
        next_at: 0,
    };

    fn all(&self) -> u32 {
        if self.seg_n >= 31 { u32::MAX } else { (1u32 << (self.seg_n + 1)) - 1 }
    }
}

// ── The stack: network, transport, access, Configuration Server ───────────

/// An access message taken in.
#[derive(Clone, Copy, Debug)]
pub struct Incoming<'a> {
    pub src: u16,
    pub dst: u16,
    pub key: Key,
    /// The TTL it came with: a reply to TTL 0 goes out with TTL 0 (it was
    /// meant for neighbours only).
    pub ttl: u8,
    pub opcode: u32,
    pub params: &'a [u8],
}

impl Incoming<'_> {
    /// The TTL to answer with.
    pub fn reply_ttl(&self, default_ttl: u8) -> u8 {
        if self.ttl == 0 { 0 } else { default_ttl }
    }
}

pub struct Stack {
    pub address: u16,
    pub iv_index: u32,
    net: NetKeys,
    net_key: [u8; 16],
    app_key: [u8; 16],
    aid: u8,
    dev_key: [u8; 16],
    pub default_ttl: u8,
    /// Relay: 0 off, 1 on (2 = not supported is never reported).
    pub relay: bool,
    pub relay_retransmit: u8,
    pub net_transmit: u8,
    cid: u16,
    pid: u16,
    vid: u16,
    pub models: [ModelState; MODELS_MAX],
    pub model_count: usize,
    /// The next SEQ to use.
    seq: u32,
    /// SEQs below this one may have been used before a reset: the glue
    /// stored it in flash. Nothing is sent at or past it.
    seq_stored: u32,
    cache: NetCache,
    rpl: Rpl,
    rx: [SegRx; SEG_RX],
    tx: [SegTx; SEG_TX],
    next_handle: u8,
    pub_rtx: [PubRetransmit; PUB_RTX],
    /// The local network interface: what the node sends to itself.
    loopback: [Loopback; LOOPBACK],
    /// Messages to the node's own models dropped: the queue was full.
    pub loopback_dropped: u32,
    /// The Secure Network Beacon state: kept and reported, though phase 1
    /// sends no beacon.
    pub beacon: bool,
    /// A Config Node Reset took the node off the network.
    pub removed: bool,
    pub outbox: Outbox,
    events: [Event; EVENTS],
    ev_head: usize,
    ev_count: usize,
    /// Events dropped because the glue fell behind.
    pub events_dropped: u32,
}

/// One past the last SEQ an IV index allows (24 bits).
const SEQ_LIMIT: u32 = 1 << 24;

/// A publication to send again: its payload (unsegmented ones only), how
/// many more times, how far apart.
#[derive(Clone, Copy)]
struct PubRetransmit {
    model: u8,
    left: u8,
    interval_ms: u16,
    next_at: u64,
    len: u8,
    payload: [u8; 11],
}

impl PubRetransmit {
    const IDLE: PubRetransmit = PubRetransmit { model: 0, left: 0, interval_ms: 0, next_at: 0, len: 0, payload: [0; 11] };
}

/// An access message the node sent that its own models take as well (Mesh
/// Protocol 3.4.5.3, the local network interface).
#[derive(Clone, Copy)]
pub struct Loopback {
    used: bool,
    dst: u16,
    key: Key,
    ttl: u8,
    len: u8,
    payload: [u8; ACCESS_MAX],
}

impl Loopback {
    const IDLE: Loopback = Loopback { used: false, dst: 0, key: Key::App, ttl: 0, len: 0, payload: [0; ACCESS_MAX] };
}

impl Stack {
    /// A node from its setup and the SEQ stored in flash (`None` the first
    /// time). Nothing is sent until the glue stored `seq_to_store()`.
    pub fn new(setup: &Setup, stored_seq: Option<u32>) -> Self {
        let mut models = [ModelState { id: 0, publication: Publication::NONE, subs: [UNASSIGNED; SUBS_MAX], bound: false };
            MODELS_MAX];
        let mut n = 1;
        for m in setup.models.iter().take(MODELS_MAX - 1) {
            let mut subs = [UNASSIGNED; SUBS_MAX];
            for (i, a) in m.subscribe.iter().take(SUBS_MAX).enumerate() {
                subs[i] = *a;
            }
            models[n] = ModelState { id: m.id, publication: m.publication, subs, bound: true };
            n += 1;
        }
        let seq = stored_seq.unwrap_or(0).min(SEQ_LIMIT);
        Stack {
            address: setup.address,
            iv_index: setup.iv_index,
            net: k2(&setup.net_key, &[0x00]),
            net_key: setup.net_key,
            app_key: setup.app_key,
            aid: k4(&setup.app_key),
            dev_key: setup.dev_key,
            default_ttl: setup.default_ttl,
            relay: setup.relay,
            // 3 transmissions 20 ms apart: Zephyr's BT_MESH_TRANSMIT(2, 20).
            relay_retransmit: (1 << 3) | 2,
            net_transmit: (1 << 3) | 2,
            cid: setup.cid,
            pid: setup.pid,
            vid: setup.vid,
            models,
            model_count: n,
            seq,
            seq_stored: seq,
            cache: NetCache::new(),
            rpl: Rpl::new(),
            rx: [SegRx::IDLE; SEG_RX],
            tx: [SegTx::IDLE; SEG_TX],
            next_handle: 0,
            pub_rtx: [PubRetransmit::IDLE; PUB_RTX],
            loopback: [Loopback::IDLE; LOOPBACK],
            loopback_dropped: 0,
            beacon: false,
            removed: false,
            outbox: Outbox::new(),
            events: [Event::ConfigChanged; EVENTS],
            ev_head: 0,
            ev_count: 0,
            events_dropped: 0,
        }
    }

    // ── SEQ ──

    /// The SEQ for the glue to store now, before the node may send: the
    /// stored one is less than SEQ_STORE_RATE / 2 ahead of use.
    pub fn seq_to_store(&self) -> Option<u32> {
        if self.seq_stored >= SEQ_LIMIT {
            return None;
        }
        if self.seq_stored.saturating_sub(self.seq) < SEQ_STORE_RATE / 2 {
            Some((self.seq + SEQ_STORE_RATE).min(SEQ_LIMIT))
        } else {
            None
        }
    }

    /// The glue stored `v` (what `seq_to_store` gave).
    pub fn seq_stored(&mut self, v: u32) {
        if v > self.seq_stored {
            self.seq_stored = v.min(SEQ_LIMIT);
        }
    }

    pub fn seq(&self) -> u32 {
        self.seq
    }

    fn next_seq(&mut self) -> Option<u32> {
        if self.seq >= self.seq_stored {
            return None;
        }
        let s = self.seq;
        self.seq += 1;
        Some(s)
    }

    /// SEQs left before the store must catch up.
    fn seqs_free(&self) -> u32 {
        self.seq_stored.saturating_sub(self.seq)
    }

    // ── Events ──

    pub fn event(&mut self, e: Event) {
        if self.ev_count == EVENTS {
            self.events_dropped += 1;
            return;
        }
        self.events[(self.ev_head + self.ev_count) % EVENTS] = e;
        self.ev_count += 1;
    }

    pub fn next_event(&mut self) -> Option<Event> {
        if self.ev_count == 0 {
            return None;
        }
        let e = self.events[self.ev_head];
        self.ev_head = (self.ev_head + 1) % EVENTS;
        self.ev_count -= 1;
        Some(e)
    }

    // ── Sending ──

    fn transmit(&mut self, pdu: &NetPdu, state: u8) -> bool {
        let mut t = Transmission { pdu: [0; NET_MAX], len: 0, transmits: 0, interval_ms: 0 };
        t.len = net_encode(&self.net, pdu, &mut t.pdu);
        if t.len == 0 {
            return false;
        }
        let (count, interval) = transmit_of(state);
        t.transmits = count;
        t.interval_ms = interval;
        // Our own PDU relayed back to us is not taken in again.
        self.cache.add(pdu.src, pdu.seq);
        self.outbox.push(t);
        true
    }

    fn send_lower(&mut self, ctl: bool, ttl: u8, dst: u16, transport: &[u8]) -> bool {
        let seq = match self.next_seq() {
            Some(s) => s,
            None => return false,
        };
        let mut p = NetPdu {
            iv_index: self.iv_index,
            ctl,
            ttl,
            seq,
            src: self.address,
            dst,
            transport: [0; 16],
            len: transport.len(),
        };
        p.transport[..transport.len()].copy_from_slice(transport);
        self.transmit(&p, self.net_transmit)
    }

    fn seal(&self, key: Key, aszmic: bool, seq: u32, dst: u16, data: &mut [u8], mic: &mut [u8]) -> bool {
        let nonce = access_nonce(key == Key::Device, aszmic, seq, self.address, dst, self.iv_index);
        let k = match key {
            Key::Device => &self.dev_key,
            Key::App => &self.app_key,
        };
        ccm_seal(k, &nonce, &[], data, mic)
    }

    /// Send an access message (opcode and parameters). Up to 11 bytes go in
    /// one PDU; longer ones are segmented and `Event::Sent` tells with the
    /// handle returned here how they ended.
    pub fn send_access(&mut self, dst: u16, key: Key, ttl: u8, payload: &[u8], now: u64) -> Result<Option<u8>, SendError> {
        if self.removed {
            return Err(SendError::Removed);
        }
        if dst == UNASSIGNED {
            return Err(SendError::NoAddress);
        }
        if payload.len() > ACCESS_MAX {
            return Err(SendError::TooLong);
        }
        let head = match key {
            Key::Device => 0x00,
            Key::App => 0x40 | self.aid,
        };
        let ttl = if ttl == 0xFF { self.default_ttl } else { ttl & 0x7F };
        // The node's own models take what it sends to them: its address, a
        // fixed group, a group one of them subscribes to.
        if key == Key::App && self.for_us(dst) {
            match self.loopback.iter().position(|l| !l.used) {
                Some(at) => {
                    let mut l = Loopback { used: true, dst, key, ttl, len: payload.len() as u8, payload: [0; ACCESS_MAX] };
                    l.payload[..payload.len()].copy_from_slice(payload);
                    self.loopback[at] = l;
                }
                None => self.loopback_dropped += 1,
            }
        }
        // To itself alone, or TTL 1 (this node only: the advertising
        // bearer's output filter drops it, Mesh Protocol 3.4.5.1) - nothing
        // goes on the air.
        if dst == self.address || ttl == 1 {
            return Ok(None);
        }
        if payload.len() <= 11 {
            if self.seqs_free() == 0 {
                return Err(SendError::SeqNotStored);
            }
            let seq = self.seq;
            let mut t = [0u8; 16];
            t[0] = head;
            t[1..1 + payload.len()].copy_from_slice(payload);
            let (data, mic) = t[1..1 + payload.len() + 4].split_at_mut(payload.len());
            if !self.seal(key, false, seq, dst, data, mic) {
                return Err(SendError::TooLong);
            }
            self.send_lower(false, ttl, dst, &t[..1 + payload.len() + 4]);
            return Ok(None);
        }
        // One segmented message to a destination at a time: the receiver
        // would drop the older one for the newer (Mesh Protocol 3.5.3.3).
        if self.tx.iter().any(|t| t.active && t.dst == dst) {
            return Err(SendError::Busy);
        }
        let slot = match self.tx.iter().position(|t| !t.active) {
            Some(i) => i,
            None => return Err(SendError::Busy),
        };
        let len = payload.len() + 4;
        let seg_n = ((len - 1) / 12) as u8;
        // Every segment and its retransmissions take a SEQ of their own.
        if self.seqs_free() < (seg_n as u32 + 1) * (SEG_RETRANSMITS as u32 + 1) {
            return Err(SendError::SeqNotStored);
        }
        let seq_auth = self.seq;
        let mut buf = [0u8; SEG_MAX * 12];
        buf[..payload.len()].copy_from_slice(payload);
        let (data, rest) = buf.split_at_mut(payload.len());
        if !self.seal(key, false, seq_auth, dst, data, &mut rest[..4]) {
            return Err(SendError::TooLong);
        }
        self.next_handle = self.next_handle.wrapping_add(1);
        let handle = self.next_handle;
        self.tx[slot] = SegTx {
            active: true,
            handle,
            dst,
            head: 0x80 | head,
            iv_index: self.iv_index,
            seq_auth,
            seg_n,
            acked: 0,
            buf,
            len,
            ttl,
            tries: if is_unicast(dst) { SEG_RETRANSMITS } else { SEG_GROUP_ROUNDS - 1 },
            next_at: 0,
        };
        self.send_segments(slot, now);
        Ok(Some(handle))
    }

    /// The segments of `tx[slot]` not yet acknowledged.
    fn send_segments(&mut self, slot: usize, now: u64) {
        let t = self.tx[slot];
        let seq_zero = (t.seq_auth & 0x1FFF) as u16;
        for o in 0..=t.seg_n {
            if t.acked & (1 << o) != 0 {
                continue;
            }
            // SeqZero must stay within 13 bits of the segment's own SEQ.
            if self.seq.wrapping_sub(t.seq_auth) >= 8192 {
                self.tx[slot].active = false;
                self.event(Event::Sent { handle: t.handle, ok: false });
                return;
            }
            let from = o as usize * 12;
            let to = (from + 12).min(t.len);
            let mut lt = [0u8; 16];
            lt[0] = t.head;
            // SZMIC 0: this node seals with a 4-byte TransMIC.
            lt[1] = (seq_zero >> 6) as u8;
            lt[2] = ((seq_zero & 0x3F) as u8) << 2 | (o >> 3);
            lt[3] = ((o & 0x07) << 5) | t.seg_n;
            lt[4..4 + to - from].copy_from_slice(&t.buf[from..to]);
            if !self.send_lower(false, t.ttl, t.dst, &lt[..4 + to - from]) {
                break;
            }
        }
        // The timer runs from the last segment on the air, not from the
        // queue: what the bearer still has to send comes first.
        self.tx[slot].next_at = now
            + self.outbox.airtime_ms()
            + if is_unicast(t.dst) { seg_tx_timeout(t.ttl) } else { SEG_GROUP_GAP_MS };
    }

    /// A Segment Acknowledgment (OBO 0: never on a friend's behalf).
    fn send_ack(&mut self, ttl: u8, dst: u16, seq_zero: u16, block: u32) {
        let mut c = [0u8; 7];
        c[0] = 0x00;
        c[1] = (seq_zero >> 6) as u8;
        c[2] = ((seq_zero & 0x3F) as u8) << 2;
        c[3..7].copy_from_slice(&block.to_be_bytes());
        self.send_lower(true, ttl, dst, &c);
    }

    // ── Receiving ──

    /// Whether the node takes in a PDU for `dst`.
    fn for_us(&self, dst: u16) -> bool {
        dst == self.address
            || dst == ALL_NODES
            || (dst == ALL_RELAYS && self.relay)
            || (is_group(dst) && dst < ALL_PROXIES && self.models[..self.model_count].iter().any(|m| m.subscribed(dst)))
    }

    /// A Network PDU off the air. What it carries for the models comes back
    /// through `deliver` (the access layer), replies and relays go to the
    /// outbox.
    pub fn receive(&mut self, pdu: &[u8], now: u64, deliver: &mut dyn FnMut(&mut Stack, &Incoming<'_>)) {
        if self.removed {
            return;
        }
        let p = match net_decode(&self.net, self.iv_index, pdu) {
            Some(p) => p,
            None => return,
        };
        if p.src == self.address || self.cache.has(p.src, p.seq) {
            return;
        }
        self.cache.add(p.src, p.seq);
        if p.dst == UNASSIGNED {
            return;
        }
        // Relay first: the neighbours should not wait for our own work.
        if self.relay && p.ttl >= 2 && p.dst != self.address {
            let mut r = p;
            r.ttl -= 1;
            let mut t = Transmission { pdu: [0; NET_MAX], len: 0, transmits: 0, interval_ms: 0 };
            t.len = net_encode(&self.net, &r, &mut t.pdu);
            if t.len != 0 {
                let (count, interval) = transmit_of(self.relay_retransmit);
                t.transmits = count;
                t.interval_ms = interval;
                self.outbox.push(t);
            }
        }
        if !self.for_us(p.dst) {
            return;
        }
        if !self.rpl.fresh(p.src, p.iv_index, p.seq) {
            return;
        }
        let tp = p.transport();
        if p.ctl {
            // Only the Segment Acknowledgment matters here; heartbeats and
            // the friendship messages are for features this node lacks.
            if tp[0] == 0x00 && tp.len() == 7 && p.dst == self.address {
                self.rpl.take(p.src, p.iv_index, p.seq);
                // OBO: a Friend acknowledging for the Low Power node it serves.
                let obo = tp[1] & 0x80 != 0;
                let seq_zero = ((tp[1] as u16 & 0x7F) << 6) | (tp[2] as u16 >> 2);
                let block = u32::from_be_bytes([tp[3], tp[4], tp[5], tp[6]]);
                self.acked(p.src, obo, seq_zero, block, now);
            }
            return;
        }
        if tp[0] & 0x80 == 0 {
            if tp.len() < 1 + 1 + 4 {
                return;
            }
            let mut b = [0u8; 15];
            let n = tp.len() - 1;
            b[..n].copy_from_slice(&tp[1..]);
            if let Some(key) = self.open(tp[0], false, p.seq, p.src, p.dst, p.iv_index, &mut b[..n]) {
                self.rpl.take(p.src, p.iv_index, p.seq);
                self.access(p.src, p.dst, key, p.ttl, &b[..n - 4], now, deliver);
            }
            return;
        }
        if tp.len() < 5 {
            return;
        }
        self.segment(&p, now, deliver);
    }

    /// Decrypt an upper transport access PDU in place (TransMIC last).
    #[allow(clippy::too_many_arguments)]
    fn open(&self, head: u8, szmic: bool, seq: u32, src: u16, dst: u16, iv: u32, b: &mut [u8]) -> Option<Key> {
        let mic = if szmic { 8 } else { 4 };
        if b.len() < mic + 1 {
            return None;
        }
        let (key, k) = if head & 0x40 == 0 {
            if dst != self.address {
                return None;
            }
            (Key::Device, &self.dev_key)
        } else if head & 0x3F == self.aid {
            (Key::App, &self.app_key)
        } else {
            return None;
        };
        let nonce = access_nonce(key == Key::Device, szmic, seq, src, dst, iv);
        let n = b.len() - mic;
        let (data, tag) = b.split_at_mut(n);
        if ccm_open(k, &nonce, &[], data, tag) { Some(key) } else { None }
    }

    fn segment(&mut self, p: &NetPdu, now: u64, deliver: &mut dyn FnMut(&mut Stack, &Incoming<'_>)) {
        let tp = p.transport();
        let szmic = tp[1] & 0x80 != 0;
        let seq_zero = ((tp[1] as u32 & 0x7F) << 6) | (tp[2] as u32 >> 2);
        let seg_o = ((tp[2] & 0x03) << 3) | (tp[3] >> 5);
        let seg_n = tp[3] & 0x1F;
        let data = &tp[4..];
        if seg_o > seg_n || (seg_o < seg_n && data.len() != 12) || data.is_empty() {
            return;
        }
        // SeqAuth: the newest SEQ not past this segment's whose low 13 bits
        // are SeqZero.
        let back = p.seq.wrapping_sub(seq_zero) & 0x1FFF;
        if back > p.seq {
            return;
        }
        let first = p.seq - back;
        let seq_auth = ((p.iv_index as u64) << 24) | first as u64;
        let unicast = p.dst == self.address;
        let ack_ttl = if p.ttl == 0 { 0 } else { self.default_ttl };
        // Longer than this node takes: a unicast sender is told to stop.
        if seg_n as usize >= SEG_MAX {
            if unicast {
                self.send_ack(ack_ttl, p.src, seq_zero as u16, 0);
            }
            return;
        }

        // A slot per (source, destination): a group message from a source
        // does not end its unicast one under way.
        let slot = match self.rx.iter().position(|r| r.active && r.src == p.src && r.dst == p.dst) {
            Some(i) => {
                let r = self.rx[i];
                if seq_auth < r.seq_auth {
                    return;
                }
                if seq_auth == r.seq_auth {
                    if r.done {
                        // Its sender missed our acknowledgment.
                        if unicast && now >= r.last_ack + SEG_REACK_MS {
                            self.rx[i].last_ack = now;
                            self.send_ack(ack_ttl, p.src, seq_zero as u16, r.all());
                        }
                        return;
                    }
                    if r.seg_n != seg_n || r.head != tp[0] || r.szmic != szmic {
                        return;
                    }
                    i
                } else {
                    self.rx[i] = SegRx::IDLE;
                    i
                }
            }
            // A message from this source taken in whole already, its slot
            // gone: acknowledged again, never delivered twice.
            None if seq_auth <= self.rpl.seg_auth(p.src) => {
                if unicast && seq_auth == self.rpl.seg_auth(p.src) {
                    self.send_ack(ack_ttl, p.src, seq_zero as u16, all_segments(seg_n));
                }
                return;
            }
            // A free slot, else one whose message is complete (kept only to
            // acknowledge repeats) - the one that finished first.
            None => match self.rx.iter().position(|r| !r.active).or_else(|| {
                self.rx
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.done)
                    .min_by_key(|(_, r)| r.drop_at)
                    .map(|(i, _)| i)
            }) {
                Some(i) => {
                    self.rx[i] = SegRx::IDLE;
                    i
                }
                None => {
                    // Busy with others: a unicast sender is told to stop.
                    if unicast {
                        self.send_ack(ack_ttl, p.src, seq_zero as u16, 0);
                    }
                    return;
                }
            },
        };
        let r = &mut self.rx[slot];
        if !r.active {
            *r = SegRx {
                active: true,
                done: false,
                src: p.src,
                dst: p.dst,
                seq_auth,
                head: tp[0],
                szmic,
                seg_n,
                got: 0,
                buf: [0; SEG_MAX * 12],
                last_len: 0,
                ttl: p.ttl,
                ack_at: None,
                drop_at: 0,
                last_ack: 0,
            };
        }
        let at = seg_o as usize * 12;
        r.buf[at..at + data.len()].copy_from_slice(data);
        if seg_o == seg_n {
            r.last_len = data.len();
        }
        let new = r.got & (1 << seg_o) == 0;
        r.got |= 1 << seg_o;
        r.drop_at = now + SEG_INCOMPLETE_MS;
        // The acknowledgment timer starts with a new segment while it is not
        // running (Mesh Profile 3.5.3.4) - it never restarts itself.
        if unicast && new && r.ack_at.is_none() {
            r.ack_at = Some(now + seg_ack_timeout(p.ttl));
        }
        if r.got != r.all() {
            return;
        }
        r.done = true;
        r.ack_at = None;
        r.drop_at = now + SEG_DONE_KEEP_MS;
        let total = seg_n as usize * 12 + r.last_len;
        let mut b = r.buf;
        let (head, src, dst) = (r.head, r.src, r.dst);
        let iv = (seq_auth >> 24) as u32;
        if unicast {
            let all = self.rx[slot].all();
            self.rx[slot].last_ack = now;
            self.send_ack(ack_ttl, p.src, seq_zero as u16, all);
        }
        if let Some(key) = self.open(head, szmic, first, src, dst, iv, &mut b[..total]) {
            // The replay list moves at the segment that completed a message
            // that decrypted (Mesh Protocol 3.9.8): a segment of it a relay
            // delivers late is not taken for a replay.
            self.rpl.completed(p.src, p.iv_index, p.seq, Some(seq_auth));
            let mic = if szmic { 8 } else { 4 };
            self.access(src, dst, key, p.ttl, &b[..total - mic], now, deliver);
        }
    }

    /// A Segment Acknowledgment for one of ours: from its destination, or
    /// (OBO) from the Friend of the Low Power node it went to.
    fn acked(&mut self, from: u16, obo: bool, seq_zero: u16, block: u32, now: u64) {
        let slot = match self.tx.iter().position(|t| {
            t.active
                && is_unicast(t.dst)
                && (t.dst == from || obo)
                && (t.seq_auth & 0x1FFF) as u16 == seq_zero
        }) {
            Some(i) => i,
            None => return,
        };
        let t = &mut self.tx[slot];
        if block == 0 {
            // The receiver cancelled it (busy, or out of room).
            t.active = false;
            let h = t.handle;
            self.event(Event::Sent { handle: h, ok: false });
            return;
        }
        let before = t.acked;
        t.acked |= block & t.all();
        if t.acked == t.all() {
            t.active = false;
            let h = t.handle;
            self.event(Event::Sent { handle: h, ok: true });
            return;
        }
        // What is still missing goes again at once, the timer from now
        // (Mesh Profile 3.5.3.3) - but only for news, not a repeated ack.
        if t.acked != before {
            self.send_segments(slot, now);
        }
    }

    /// Timers: segment retransmissions, acknowledgments, given-up messages.
    /// The next time anything is due, if anything is.
    pub fn poll(&mut self, now: u64) -> Option<u64> {
        if self.removed {
            return None;
        }
        for i in 0..SEG_TX {
            let t = self.tx[i];
            if !t.active || now < t.next_at {
                continue;
            }
            if t.tries == 0 {
                self.tx[i].active = false;
                // A group's segments all went out: that is all it gets.
                self.event(Event::Sent { handle: t.handle, ok: !is_unicast(t.dst) });
                continue;
            }
            self.tx[i].tries -= 1;
            self.send_segments(i, now);
        }
        for i in 0..SEG_RX {
            let r = self.rx[i];
            if !r.active {
                continue;
            }
            if now >= r.drop_at {
                self.rx[i] = SegRx::IDLE;
                continue;
            }
            if r.ack_at.is_some_and(|at| now >= at) {
                let ack_ttl = if r.ttl == 0 { 0 } else { self.default_ttl };
                self.send_ack(ack_ttl, r.src, (r.seq_auth & 0x1FFF) as u16, r.got);
                self.rx[i].ack_at = None;
                self.rx[i].last_ack = now;
            }
        }
        for i in 0..PUB_RTX {
            let r = self.pub_rtx[i];
            if r.left == 0 || now < r.next_at {
                continue;
            }
            self.pub_rtx[i].left -= 1;
            self.pub_rtx[i].next_at = now + r.interval_ms as u64;
            let p = self.models[r.model as usize].publication;
            let _ = self.send_access(p.address, Key::App, p.ttl, &r.payload[..r.len as usize], now);
        }
        let mut next: Option<u64> = None;
        let mut due = |at: u64| next = Some(next.map_or(at, |n: u64| n.min(at)));
        for t in self.tx.iter().filter(|t| t.active) {
            due(t.next_at);
        }
        for r in self.rx.iter().filter(|r| r.active) {
            due(r.drop_at);
            if let Some(at) = r.ack_at {
                due(at);
            }
        }
        for r in self.pub_rtx.iter().filter(|r| r.left > 0) {
            due(r.next_at);
        }
        next
    }

    // ── The access layer ──

    #[allow(clippy::too_many_arguments)]
    fn access(
        &mut self,
        src: u16,
        dst: u16,
        key: Key,
        ttl: u8,
        payload: &[u8],
        now: u64,
        deliver: &mut dyn FnMut(&mut Stack, &Incoming<'_>),
    ) {
        let (opcode, params) = match parse_opcode(payload) {
            Some(x) => x,
            None => return,
        };
        let m = Incoming { src, dst, key, ttl, opcode, params };
        // Under the DevKey (only ever to this node's address): what the
        // Configuration Server does not take is a status for a client.
        if key == Key::Device && self.config_server(&m, now) {
            return;
        }
        deliver(self, &m);
    }

    /// The next message the node sent to its own models.
    pub fn take_loopback(&mut self) -> Option<Loopback> {
        let at = self.loopback.iter().position(|l| l.used)?;
        let l = self.loopback[at];
        self.loopback[at].used = false;
        Some(l)
    }

    /// Deliver one: from the node's own address, through the access layer,
    /// as a message off the air would be (no replay list - it never left).
    pub fn deliver_local(&mut self, l: &Loopback, now: u64, deliver: &mut dyn FnMut(&mut Stack, &Incoming<'_>)) {
        let payload = l.payload;
        let src = self.address;
        self.access(src, l.dst, l.key, l.ttl, &payload[..l.len as usize], now, deliver);
    }

    /// Whether model `i` takes a message for `dst` under `key`. A model
    /// that extends another shares its subscription list (Mesh Protocol
    /// 4.2.4): the Sensor Setup Server the Sensor Server's.
    pub fn model_takes(&self, i: usize, dst: u16, key: Key) -> bool {
        let m = &self.models[i];
        let fixed = dst == ALL_NODES || (dst == ALL_RELAYS && self.relay);
        key == Key::App && m.bound && (dst == self.address || fixed || self.models[self.sub_root(i)].subscribed(dst))
    }

    /// The model whose subscription list model `i` uses.
    fn sub_root(&self, i: usize) -> usize {
        if self.models[i].id != SENSOR_SETUP_SERVER {
            return i;
        }
        self.model_index(SENSOR_SERVER).unwrap_or(i)
    }

    pub fn model_index(&self, id: u16) -> Option<usize> {
        self.models[..self.model_count].iter().position(|m| m.id == id)
    }

    /// Publish from model `i` to its publication address, with its TTL.
    pub fn publish(&mut self, i: usize, payload: &[u8], now: u64) -> Result<Option<u8>, SendError> {
        let p = self.models[i].publication;
        if p.address == UNASSIGNED || !self.models[i].bound {
            return Err(SendError::NoAddress);
        }
        let sent = self.send_access(p.address, Key::App, p.ttl, payload, now)?;
        // Publish Retransmit: count (bits 0-2) more times, (steps + 1) x 50 ms
        // apart - each a PDU of its own. Unsegmented messages only: a
        // segmented one is repeated by its own rounds.
        let count = p.retransmit & 0x07;
        if count > 0 && payload.len() <= 11 {
            let interval_ms = ((p.retransmit >> 3) as u16 + 1) * 50;
            let at = match self.pub_rtx.iter().position(|r| r.left > 0 && r.model as usize == i) {
                Some(at) => Some(at),
                None => self.pub_rtx.iter().position(|r| r.left == 0),
            };
            if let Some(at) = at {
                let mut r = PubRetransmit { model: i as u8, left: count, interval_ms, next_at: now + interval_ms as u64, len: payload.len() as u8, payload: [0; 11] };
                r.payload[..payload.len()].copy_from_slice(payload);
                self.pub_rtx[at] = r;
            }
        }
        Ok(sent)
    }

    // ── The Configuration Server (Mesh Profile 4.4.1), DevKey only ──

    fn reply_dev(&mut self, m: &Incoming<'_>, payload: &[u8], now: u64) {
        let ttl = m.reply_ttl(self.default_ttl);
        let _ = self.send_access(m.src, Key::Device, ttl, payload, now);
    }

    /// Composition Data page 0.
    pub fn composition(&self, out: &mut [u8]) -> usize {
        let mut n = 0;
        let mut put = |b: &[u8]| {
            out[n..n + b.len()].copy_from_slice(b);
            n += b.len();
        };
        put(&self.cid.to_le_bytes());
        put(&self.pid.to_le_bytes());
        put(&self.vid.to_le_bytes());
        put(&(RPL_SIZE as u16).to_le_bytes());
        // Features: Relay (bit 0) only.
        put(&1u16.to_le_bytes());
        put(&0u16.to_le_bytes());
        put(&[self.model_count as u8, 0]);
        for m in &self.models[..self.model_count] {
            put(&m.id.to_le_bytes());
        }
        n
    }

    /// Whether the message was the Configuration Server's.
    fn config_server(&mut self, m: &Incoming<'_>, now: u64) -> bool {
        let p = m.params;
        let mut out = [0u8; ACCESS_MAX];
        // An ElementAddress that is not unicast is prohibited: the message
        // is ignored (Zephyr does the same).
        let element_first = matches!(
            m.opcode,
            0x03 | 0x8018 | 0x801A..=0x801E | 0x8020..=0x8022 | 0x8029 | 0x802B | 0x803D | 0x803F | 0x804B | 0x804D
        );
        if element_first && p.len() >= 2 && !is_unicast(u16::from_le_bytes([p[0], p[1]])) {
            return true;
        }
        match m.opcode {
            // Composition Data Get: page 0 whatever was asked.
            0x8008 if p.len() == 1 => {
                out[0] = 0x02;
                out[1] = 0;
                let n = self.composition(&mut out[2..]);
                self.reply_dev(m, &out[..2 + n], now);
            }
            // Default TTL Get / Set.
            0x800C if p.is_empty() => {
                let t = self.default_ttl;
                self.reply_dev(m, &[0x80, 0x0E, t], now);
            }
            0x800D if p.len() == 1 => {
                if p[0] == 1 || p[0] > 0x7F {
                    return true;
                }
                self.default_ttl = p[0];
                self.event(Event::ConfigChanged);
                self.reply_dev(m, &[0x80, 0x0E, p[0]], now);
            }
            // Relay Get / Set.
            0x8026 if p.is_empty() => {
                let s = [0x80, 0x28, self.relay as u8, self.relay_retransmit];
                self.reply_dev(m, &s, now);
            }
            0x8027 if p.len() == 2 => {
                if p[0] > 1 {
                    return true;
                }
                self.relay = p[0] == 1;
                self.relay_retransmit = p[1];
                self.event(Event::ConfigChanged);
                let s = [0x80, 0x28, self.relay as u8, self.relay_retransmit];
                self.reply_dev(m, &s, now);
            }
            // Network Transmit Get / Set.
            0x8023 if p.is_empty() => {
                let s = [0x80, 0x25, self.net_transmit];
                self.reply_dev(m, &s, now);
            }
            0x8024 if p.len() == 1 => {
                self.net_transmit = p[0];
                self.event(Event::ConfigChanged);
                self.reply_dev(m, &[0x80, 0x25, p[0]], now);
            }
            // Beacon Get / Set: the state is kept and reported (Mesh Protocol
            // 4.4.1.2.1), though phase 1 sends no Secure Network Beacon.
            0x8009 if p.is_empty() => {
                let b = self.beacon as u8;
                self.reply_dev(m, &[0x80, 0x0B, b], now);
            }
            0x800A if p.len() == 1 && p[0] <= 1 => {
                self.beacon = p[0] == 1;
                self.event(Event::ConfigChanged);
                self.reply_dev(m, &[0x80, 0x0B, p[0]], now);
            }
            // GATT Proxy Get / Set and Friend Get / Set: not supported (2).
            0x8012 if p.is_empty() => self.reply_dev(m, &[0x80, 0x14, 2], now),
            0x8013 if p.len() == 1 && p[0] <= 1 => self.reply_dev(m, &[0x80, 0x14, 2], now),
            0x800F if p.is_empty() => self.reply_dev(m, &[0x80, 0x11, 2], now),
            0x8010 if p.len() == 1 && p[0] <= 1 => self.reply_dev(m, &[0x80, 0x11, 2], now),
            // Model Publication Get.
            0x8018 if p.len() == 4 || p.len() == 6 => {
                let (mut status, i) = self.find_model(p, 2);
                if status == 0 && self.models[i.unwrap_or(0)].id == CONFIG_SERVER {
                    status = 0x07;
                }
                self.publication_status(m, status, i, p, 2, now);
            }
            // Model Publication Set. A prohibited TTL, or a virtual address in
            // the 16-bit field, makes the message malformed: no status at all.
            0x03 if p.len() == 11 || p.len() == 13 => {
                let addr = u16::from_le_bytes([p[2], p[3]]);
                if (p[6] > 0x7F && p[6] != 0xFF) || is_virtual(addr) {
                    return true;
                }
                let (mut status, i) = self.find_model(p, 9);
                let app_index = u16::from_le_bytes([p[4], p[5]]) & 0x0FFF;
                if status == 0 {
                    let i = i.unwrap_or(0);
                    if self.models[i].id == CONFIG_SERVER {
                        status = 0x07;
                    } else if addr == UNASSIGNED {
                        // Publication off: the other fields are ignored.
                        self.models[i].publication = Publication::NONE;
                        self.event(Event::ConfigChanged);
                    } else if app_index != 0 || !self.models[i].bound {
                        status = 0x03;
                    } else if p[5] & 0x10 != 0 {
                        // Friendship credentials: no Low Power feature here.
                        status = 0x0A;
                    } else {
                        self.models[i].publication = Publication {
                            address: addr,
                            ttl: p[6],
                            period: p[7],
                            retransmit: p[8],
                            app_index,
                            credentials: false,
                        };
                        self.event(Event::ConfigChanged);
                    }
                }
                self.publication_status(m, status, i, p, 9, now);
            }
            // Model Publication Virtual Address Set: no Label UUIDs kept here.
            0x801A if p.len() == 25 || p.len() == 27 => {
                let (status, _) = self.find_model(p, 23);
                let status = if status == 0 { 0x05 } else { status };
                self.publication_status(m, status, None, p, 23, now);
            }
            // Model Subscription Add / Delete / Overwrite.
            0x801B | 0x801C | 0x801E if p.len() == 6 || p.len() == 8 => {
                let (mut status, i) = self.find_model(p, 4);
                let addr = u16::from_le_bytes([p[2], p[3]]);
                if status == 0 {
                    let i = i.unwrap_or(0);
                    if self.models[i].id == CONFIG_SERVER {
                        status = 0x08;
                    } else if !is_group(addr) || addr == ALL_NODES {
                        status = 0x01;
                    } else {
                        let root = self.sub_root(i);
                        status = self.subscribe(root, m.opcode, addr);
                    }
                }
                let mut s = [0u8; 11];
                s[0..2].copy_from_slice(&[0x80, 0x1F]);
                s[2] = status;
                s[3..7].copy_from_slice(&p[0..4]);
                let ml = p.len() - 4;
                s[7..7 + ml].copy_from_slice(&p[4..]);
                self.reply_dev(m, &s[..7 + ml], now);
            }
            // Model Subscription Delete All.
            0x801D if p.len() == 4 || p.len() == 6 => {
                let (mut status, i) = self.find_model(p, 2);
                if status == 0 {
                    let i = i.unwrap_or(0);
                    if self.models[i].id == CONFIG_SERVER {
                        status = 0x08;
                    } else {
                        let root = self.sub_root(i);
                        self.models[root].subs = [UNASSIGNED; SUBS_MAX];
                        self.event(Event::ConfigChanged);
                    }
                }
                let mut s = [0u8; 11];
                s[0..2].copy_from_slice(&[0x80, 0x1F]);
                s[2] = status;
                s[3..5].copy_from_slice(&p[0..2]);
                let ml = p.len() - 2;
                s[7..7 + ml].copy_from_slice(&p[2..]);
                self.reply_dev(m, &s[..7 + ml], now);
            }
            // SIG Model Subscription Get.
            0x8029 if p.len() == 4 => {
                let (mut status, i) = self.find_model(p, 2);
                let mut s = [0u8; 7 + 2 * SUBS_MAX];
                s[0..2].copy_from_slice(&[0x80, 0x2A]);
                let mut n = 7;
                if status == 0 {
                    let i = i.unwrap_or(0);
                    if self.models[i].id == CONFIG_SERVER {
                        status = 0x08;
                    } else {
                        let root = self.sub_root(i);
                        for a in self.models[root].subs.iter().filter(|a| **a != UNASSIGNED) {
                            s[n..n + 2].copy_from_slice(&a.to_le_bytes());
                            n += 2;
                        }
                    }
                }
                s[2] = status;
                s[3..7].copy_from_slice(&p[0..4]);
                self.reply_dev(m, &s[..n], now);
            }
            // Model App Bind / Unbind: AppKey index 0 is the only one.
            0x803D | 0x803F if p.len() == 6 || p.len() == 8 => {
                let (mut status, i) = self.find_model(p, 4);
                let app_index = u16::from_le_bytes([p[2], p[3]]) & 0x0FFF;
                if status == 0 {
                    let i = i.unwrap_or(0);
                    if self.models[i].id == CONFIG_SERVER {
                        status = 0x0D;
                    } else if app_index != 0 {
                        status = 0x03;
                    } else {
                        self.models[i].bound = m.opcode == 0x803D;
                        // Unbound from the key it publishes with: no more
                        // publication (Mesh Protocol 4.4.1.2.19).
                        if !self.models[i].bound {
                            self.models[i].publication = Publication::NONE;
                        }
                        self.event(Event::ConfigChanged);
                    }
                }
                let mut s = [0u8; 11];
                s[0..2].copy_from_slice(&[0x80, 0x3E]);
                s[2] = status;
                s[3..7].copy_from_slice(&p[0..4]);
                let ml = p.len() - 4;
                s[7..7 + ml].copy_from_slice(&p[4..]);
                self.reply_dev(m, &s[..7 + ml], now);
            }
            // SIG Model App Get.
            0x804B if p.len() == 4 => {
                let (status, i) = self.find_model(p, 2);
                let mut s = [0u8; 9];
                s[0..2].copy_from_slice(&[0x80, 0x4C]);
                let mut n = 7;
                if status == 0 {
                    // The Configuration Server is bound to no AppKey: an
                    // empty list.
                    let i = i.unwrap_or(0);
                    if self.models[i].id != CONFIG_SERVER && self.models[i].bound {
                        s[7..9].copy_from_slice(&0u16.to_le_bytes());
                        n = 9;
                    }
                }
                s[2] = status;
                s[3..7].copy_from_slice(&p[0..4]);
                self.reply_dev(m, &s[..n], now);
            }
            // AppKey Add: the one the IDE put in, at index 0, again.
            0x00 if p.len() == 19 => {
                let net = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let app = (u16::from_le_bytes([p[1], p[2]]) >> 4) & 0x0FFF;
                let status = if net != 0 {
                    0x04
                } else if app != 0 {
                    0x05
                } else if p[3..19] == self.app_key {
                    0x00
                } else {
                    0x06
                };
                let s = [0x80, 0x03, status, p[0], p[1], p[2]];
                self.reply_dev(m, &s, now);
            }
            // AppKey Get: index 0 under NetKey 0.
            0x8001 if p.len() == 2 => {
                let net = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                if net != 0 {
                    self.reply_dev(m, &[0x80, 0x02, 0x04, p[0], p[1]], now);
                } else {
                    self.reply_dev(m, &[0x80, 0x02, 0x00, p[0], p[1], 0x00, 0x00], now);
                }
            }
            // NetKey Get: index 0.
            0x8042 if p.is_empty() => self.reply_dev(m, &[0x80, 0x43, 0x00, 0x00], now),
            // NetKey Add / Update / Delete: index 0 is the IDE's, and the only
            // one - the same key again is fine, nothing else changes.
            0x8040 | 0x8045 if p.len() == 18 => {
                let idx = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let same = p[2..18] == self.net_key;
                let status = match (m.opcode, idx, same) {
                    (0x8040, 0, true) => 0x00,
                    (0x8040, 0, false) => 0x06,
                    (0x8040, _, _) => 0x05,
                    (_, 0, _) => 0x0B,
                    _ => 0x04,
                };
                self.reply_dev(m, &[0x80, 0x44, status, p[0], p[1]], now);
            }
            0x8041 if p.len() == 2 => {
                let idx = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let status = if idx == 0 { 0x0C } else { 0x00 };
                self.reply_dev(m, &[0x80, 0x44, status, p[0], p[1]], now);
            }
            // AppKey Update / Delete: the one AppKey stays.
            0x01 if p.len() == 19 => {
                let net = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let app = (u16::from_le_bytes([p[1], p[2]]) >> 4) & 0x0FFF;
                let status = if net != 0 { 0x04 } else if app != 0 { 0x03 } else { 0x0B };
                self.reply_dev(m, &[0x80, 0x03, status, p[0], p[1], p[2]], now);
            }
            0x8000 if p.len() == 3 => {
                let net = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let app = (u16::from_le_bytes([p[1], p[2]]) >> 4) & 0x0FFF;
                let status = if net != 0 { 0x04 } else if app == 0 { 0x0C } else { 0x00 };
                self.reply_dev(m, &[0x80, 0x03, status, p[0], p[1], p[2]], now);
            }
            // Key Refresh Phase Get: never refreshing (phase 0).
            0x8015 if p.len() == 2 => {
                let idx = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
                let status = if idx == 0 { 0x00 } else { 0x04 };
                self.reply_dev(m, &[0x80, 0x17, status, p[0], p[1], 0x00], now);
            }
            // Node Identity Get / Set: not supported (2), there is no proxy.
            0x8046 if p.len() == 2 => self.node_identity(m, now),
            0x8047 if p.len() == 3 && p[2] <= 1 => self.node_identity(m, now),
            // Low Power Node PollTimeout Get: this node is no Friend - 0.
            0x802D if p.len() == 2 => self.reply_dev(m, &[0x80, 0x2E, p[0], p[1], 0, 0, 0], now),
            // Heartbeat Publication Get: none. Subscription Get: none, and a
            // Set that clears it (an unassigned source or destination) is
            // answered the same; a Set that would start one is not (no
            // heartbeats in phase 1).
            0x8038 if p.is_empty() => self.reply_dev(m, &[0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], now),
            0x803A if p.is_empty() => self.reply_dev(m, &[0x80, 0x3C, 0, 0, 0, 0, 0, 0, 0, 0, 0], now),
            0x803B if p.len() == 5 => {
                let src = u16::from_le_bytes([p[0], p[1]]);
                let dst = u16::from_le_bytes([p[2], p[3]]);
                if src == UNASSIGNED || dst == UNASSIGNED {
                    self.reply_dev(m, &[0x80, 0x3C, 0, 0, 0, 0, 0, 0, 0, 0, 0], now);
                }
            }
            // Vendor Model Subscription Get / App Get: no vendor models.
            0x802B | 0x804D if p.len() == 6 => {
                let (status, _) = self.find_model(p, 2);
                let op: u8 = if m.opcode == 0x802B { 0x2C } else { 0x4E };
                let mut s = [0x80, op, status, 0, 0, 0, 0, 0, 0];
                s[3..9].copy_from_slice(&p[..6]);
                self.reply_dev(m, &s, now);
            }
            // Model Subscription Virtual Address Add / Delete / Overwrite: no
            // Label UUIDs kept here.
            0x8020..=0x8022 if p.len() == 20 || p.len() == 22 => {
                let (status, i) = self.find_model(p, 18);
                let status = match (status, i) {
                    (0, Some(i)) if self.models[i].id == CONFIG_SERVER => 0x08,
                    (0, _) if m.opcode == 0x8021 => 0x00,
                    (0, _) => 0x05,
                    (s, _) => s,
                };
                let mut label = [0u8; 16];
                label.copy_from_slice(&p[2..18]);
                let va = virtual_address(&label);
                let mut s = [0u8; 11];
                s[0..2].copy_from_slice(&[0x80, 0x1F]);
                s[2] = status;
                s[3..5].copy_from_slice(&p[0..2]);
                s[5..7].copy_from_slice(&va.to_le_bytes());
                let ml = p.len() - 18;
                s[7..7 + ml].copy_from_slice(&p[18..]);
                self.reply_dev(m, &s[..7 + ml], now);
            }
            // Node Reset: answered, then the node leaves the network - until
            // the next boot, which brings the IDE's keys back.
            0x8049 if p.is_empty() => {
                self.reply_dev(m, &[0x80, 0x4A], now);
                // The status is queued: nothing under way goes on.
                self.tx = [SegTx::IDLE; SEG_TX];
                self.rx = [SegRx::IDLE; SEG_RX];
                self.pub_rtx = [PubRetransmit::IDLE; PUB_RTX];
                self.loopback = [Loopback::IDLE; LOOPBACK];
                self.removed = true;
                self.event(Event::Removed);
            }
            _ => return false,
        }
        true
    }

    fn node_identity(&mut self, m: &Incoming<'_>, now: u64) {
        let p = m.params;
        let idx = u16::from_le_bytes([p[0], p[1]]) & 0x0FFF;
        let status = if idx == 0 { 0x00 } else { 0x04 };
        self.reply_dev(m, &[0x80, 0x48, status, p[0], p[1], 0x02], now);
    }

    /// The element address (bytes 0-1) and model id (at `at`, 2 bytes SIG
    /// or 4 vendor) of a Config message: (status, model index).
    fn find_model(&self, p: &[u8], at: usize) -> (u8, Option<usize>) {
        let element = u16::from_le_bytes([p[0], p[1]]);
        if element != self.address {
            return (0x01, None);
        }
        if p.len() - at != 2 {
            // No vendor models on this node.
            return (0x02, None);
        }
        let id = u16::from_le_bytes([p[at], p[at + 1]]);
        match self.model_index(id) {
            Some(i) => (0, Some(i)),
            None => (0x02, None),
        }
    }

    fn subscribe(&mut self, i: usize, opcode: u32, addr: u16) -> u8 {
        let subs = &mut self.models[i].subs;
        match opcode {
            0x801B => {
                if subs.contains(&addr) {
                    return 0;
                }
                match subs.iter().position(|a| *a == UNASSIGNED) {
                    Some(at) => subs[at] = addr,
                    None => return 0x05,
                }
            }
            0x801C => {
                for a in subs.iter_mut().filter(|a| **a == addr) {
                    *a = UNASSIGNED;
                }
            }
            _ => {
                *subs = [UNASSIGNED; SUBS_MAX];
                subs[0] = addr;
            }
        }
        self.event(Event::ConfigChanged);
        0
    }

    #[allow(clippy::too_many_arguments)]
    fn publication_status(&mut self, m: &Incoming<'_>, status: u8, i: Option<usize>, p: &[u8], at: usize, now: u64) {
        let mut s = [0u8; 16];
        s[0..2].copy_from_slice(&[0x80, 0x19]);
        s[2] = status;
        s[3..5].copy_from_slice(&p[0..2]);
        // A failed status, or no publication: every field 0.
        let pb = match (status, i) {
            (0, Some(i)) if self.models[i].publication.address != UNASSIGNED => self.models[i].publication,
            _ => Publication { ttl: 0, ..Publication::NONE },
        };
        s[5..7].copy_from_slice(&pb.address.to_le_bytes());
        let idx = (pb.app_index & 0x0FFF) | ((pb.credentials as u16) << 12);
        s[7..9].copy_from_slice(&idx.to_le_bytes());
        s[9] = pb.ttl;
        s[10] = pb.period;
        s[11] = pb.retransmit;
        let ml = p.len() - at;
        s[12..12 + ml].copy_from_slice(&p[at..]);
        self.reply_dev(m, &s[..12 + ml], now);
    }
}

/// The opcode (1, 2 or 3 bytes; vendor opcodes carry the company id) and
/// the parameters after it.
pub fn parse_opcode(b: &[u8]) -> Option<(u32, &[u8])> {
    let first = *b.first()?;
    match first >> 6 {
        0 | 1 => {
            if first == 0x7F {
                return None;
            }
            Some((first as u32, &b[1..]))
        }
        2 => {
            if b.len() < 2 {
                return None;
            }
            Some((((first as u32) << 8) | b[1] as u32, &b[2..]))
        }
        _ => {
            if b.len() < 3 {
                return None;
            }
            Some((((first as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32, &b[3..]))
        }
    }
}

/// An opcode's bytes: 1 for values below 0x80, else 2 (SIG) or 3 (vendor).
pub fn opcode_bytes(opcode: u32, out: &mut [u8]) -> usize {
    if opcode < 0x80 {
        out[0] = opcode as u8;
        1
    } else if opcode <= 0xFFFF {
        out[..2].copy_from_slice(&(opcode as u16).to_be_bytes());
        2
    } else {
        out[..3].copy_from_slice(&opcode.to_be_bytes()[1..]);
        3
    }
}
