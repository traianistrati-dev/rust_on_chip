//! The firmware's Bluetooth Mesh node (`codegen/iot_templates/ble_mesh_core.rs`
//! and `ble_mesh_models.rs`), the very text a project's ble_mesh.rs carries,
//! run on the PC: the key derivations and the sample messages of the Mesh
//! Profile specification, then nodes talking to each other through an
//! "air" these tests control - lost segments, relays, replays, a reset.
//!
//! ```text
//! cargo test --bin rust_on_chip ble_mesh_core
//! ```

#![allow(dead_code)]

include!("codegen/iot_templates/ble_mesh_core.rs");
include!("codegen/iot_templates/ble_mesh_models.rs");

fn h(s: &str) -> Vec<u8> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn h16(s: &str) -> [u8; 16] {
    h(s).try_into().unwrap()
}

// The network every node below is on: the specification's sample NetKey and
// AppKey, so the PDUs can be checked against the sample data too.
const NET_KEY: &str = "7dd7364cd842ad18c17c2b820c84c3d6";
const APP_KEY: &str = "63964771734fbd76e3b40519d1d94a48";

static NO_SUBS: [u16; 0] = [];
static GROUP_1: [u16; 1] = [0xC000];
static GROUP_2: [u16; 1] = [0xC001];

const fn publish_to(address: u16) -> Publication {
    Publication { address, ttl: 0xFF, period: 0, retransmit: 0, app_index: 0, credentials: false }
}

static LIGHT: [ModelSetup; 3] = [
    ModelSetup { id: ONOFF_SERVER, publication: publish_to(0xC000), subscribe: &GROUP_1 },
    ModelSetup { id: SENSOR_SERVER, publication: publish_to(0xC001), subscribe: &GROUP_2 },
    ModelSetup { id: SENSOR_SETUP_SERVER, publication: Publication::NONE, subscribe: &NO_SUBS },
];

static SWITCH: [ModelSetup; 2] = [
    ModelSetup { id: ONOFF_CLIENT, publication: publish_to(0xC000), subscribe: &GROUP_1 },
    ModelSetup { id: SENSOR_CLIENT, publication: publish_to(0xC001), subscribe: &GROUP_2 },
];

static RELAY_ONLY: [ModelSetup; 0] = [];

fn setup(address: u16, models: &'static [ModelSetup], relay: bool) -> Setup {
    let mut dev_key = [0u8; 16];
    dev_key[0] = address as u8;
    dev_key[15] = 0x5A;
    Setup {
        address,
        iv_index: 0x1234_5678,
        net_key: h16(NET_KEY),
        app_key: h16(APP_KEY),
        dev_key,
        default_ttl: 7,
        relay,
        cid: 0x05F1,
        pid: 0x0001,
        vid: 0x0001,
        models,
    }
}

/// A node as the glue runs it: the SEQ stored as soon as it asks.
fn node(address: u16, models: &'static [ModelSetup], relay: bool) -> Mesh {
    let mut m = Mesh::new(&setup(address, models, relay), &[0x004F, 0x004C], None);
    store_seq(&mut m);
    m
}

fn store_seq(m: &mut Mesh) {
    while let Some(v) = m.stack.seq_to_store() {
        m.stack.seq_stored(v);
    }
}

/// Every PDU `from` has queued, delivered to each of `to`; how many went.
fn air(from: &mut Mesh, to: &mut [&mut Mesh], now: u64) -> usize {
    let mut n = 0;
    while let Some(t) = from.stack.outbox.pop() {
        n += 1;
        for m in to.iter_mut() {
            m.receive(&t.pdu[..t.len], now);
            store_seq(m);
        }
    }
    n
}

fn events(m: &mut Mesh) -> Vec<Event> {
    let mut v = Vec::new();
    while let Some(e) = m.stack.next_event() {
        v.push(e);
    }
    v
}

fn model_events(m: &mut Mesh) -> Vec<ModelEvent> {
    events(m)
        .into_iter()
        .filter_map(|e| match e {
            Event::Model(x) => Some(x),
            _ => None,
        })
        .collect()
}

// ── Keys ──────────────────────────────────────────────────────────────────

#[test]
fn a_key_is_read_from_its_hex() {
    const K: [u8; 16] = hex16("7dd7364cd842ad18C17C2B820C84C3D6");
    assert_eq!(K, h16(NET_KEY));
}

#[test]
fn the_specification_s_key_derivations() {
    // Mesh Profile 1.0.1, 8.1.1 / 8.1.3 / 8.1.5 / 8.1.6.
    assert_eq!(s1(b"test").to_vec(), h("b73cefbd641ef2ea598c2b6efb62f79c"));
    let k = k2(&h16(NET_KEY), &[0x00]);
    assert_eq!(k.nid, 0x68);
    assert_eq!(k.encryption.to_vec(), h("0953fa93e7caac9638f58820220a398e"));
    assert_eq!(k.privacy.to_vec(), h("8b84eedec100067d670971dd2aa700cf"));
    assert_eq!(k3(&h16(NET_KEY)).to_vec(), h("3ecaff672f673370"));
    assert_eq!(k4(&h16("3216d1509884b533248541792b877f98")), 0x38);
}

// ── Network PDUs ──────────────────────────────────────────────────────────

fn pdu(ctl: bool, ttl: u8, seq: u32, src: u16, dst: u16, transport: &[u8]) -> NetPdu {
    let mut t = [0u8; 16];
    t[..transport.len()].copy_from_slice(transport);
    NetPdu { iv_index: 0x1234_5678, ctl, ttl, seq, src, dst, transport: t, len: transport.len() }
}

#[test]
fn a_network_pdu_comes_back_as_it_went() {
    let keys = k2(&h16(NET_KEY), &[0x00]);
    for (ctl, len) in [(false, 1), (false, 16), (true, 1), (true, 12)] {
        let p = pdu(ctl, 5, 0x00ABCD, 0x1201, 0xC000, &[0x5A; 16][..len]);
        let mut out = [0u8; NET_MAX];
        let n = net_encode(&keys, &p, &mut out);
        assert_eq!(n, 9 + len + if ctl { 8 } else { 4 });
        assert_eq!(net_decode(&keys, 0x1234_5678, &out[..n]), Some(p));
        // The IV index after: the IVI bit says the PDU is under the one before.
        assert_eq!(net_decode(&keys, 0x1234_5679, &out[..n]).map(|q| q.iv_index), Some(0x1234_5678));
    }
}

#[test]
fn a_pdu_too_long_for_one_advertisement_is_not_made() {
    let keys = k2(&h16(NET_KEY), &[0x00]);
    let mut out = [0u8; NET_MAX];
    assert_eq!(net_encode(&keys, &pdu(false, 5, 1, 1, 2, &[0; 16]), &mut out), 29);
    let mut long = pdu(false, 5, 1, 1, 2, &[0; 16]);
    long.ctl = true;
    assert_eq!(net_encode(&keys, &long, &mut out), 0);
}

#[test]
fn a_tampered_or_foreign_pdu_is_dropped() {
    let keys = k2(&h16(NET_KEY), &[0x00]);
    let p = pdu(false, 5, 7, 0x0001, 0x0002, &[1, 2, 3, 4, 5, 6]);
    let mut out = [0u8; NET_MAX];
    let n = net_encode(&keys, &p, &mut out);
    for i in 1..n {
        let mut bad = out;
        bad[i] ^= 0x01;
        assert_eq!(net_decode(&keys, 0x1234_5678, &bad[..n]), None, "byte {i}");
    }
    let other = k2(&h16("00112233445566778899aabbccddeeff"), &[0x00]);
    assert_eq!(net_decode(&other, 0x1234_5678, &out[..n]), None);
    assert_eq!(net_decode(&keys, 0x1234_5678, &out[..13]), None);
}

#[test]
fn a_group_source_is_dropped() {
    let keys = k2(&h16(NET_KEY), &[0x00]);
    let mut out = [0u8; NET_MAX];
    let n = net_encode(&keys, &pdu(false, 5, 7, 0xC000, 0x0002, &[1, 2, 3, 4, 5, 6]), &mut out);
    assert_eq!(net_decode(&keys, 0x1234_5678, &out[..n]), None);
}

// ── Opcodes and sensor data ───────────────────────────────────────────────

#[test]
fn opcodes_of_one_two_and_three_bytes() {
    assert_eq!(parse_opcode(&[0x52, 9]), Some((0x52, &[9u8][..])));
    assert_eq!(parse_opcode(&[0x82, 0x02, 1, 2]), Some((0x8202, &[1u8, 2][..])));
    assert_eq!(parse_opcode(&[0xC1, 0x59, 0x00, 7]), Some((0xC15900, &[7u8][..])));
    assert_eq!(parse_opcode(&[0x7F]), None);
    assert_eq!(parse_opcode(&[0x82]), None);
    let mut b = [0u8; 3];
    assert_eq!((opcode_bytes(0x52, &mut b), b[0]), (1, 0x52));
    assert_eq!((opcode_bytes(0x8204, &mut b), &b[..2]), (2, &[0x82u8, 0x04][..]));
}

#[test]
fn marshalled_sensor_data_both_formats() {
    let mut b = [0u8; 32];
    // Format A: property 0x004F, one byte.
    let n = marshal(0x004F, &[0x2B], &mut b);
    assert_eq!(&b[..n], &[0xE0, 0x09, 0x2B]);
    assert_eq!(unmarshal(&b[..n]), Some((0x004F, &[0x2B][..], &[][..])));
    // Format B: a property past 11 bits.
    let n = marshal(0x2A6E, &[1, 2], &mut b);
    assert_eq!(&b[..n], &[0x03, 0x6E, 0x2A, 1, 2]);
    assert_eq!(unmarshal(&b[..n]), Some((0x2A6E, &[1u8, 2][..], &[][..])));
    // A property the server lacks: format B, length 0x7F, no value.
    let n = marshal(0x004F, &[], &mut b);
    assert_eq!(&b[..n], &[0xFF, 0x4F, 0x00]);
    assert_eq!(unmarshal(&b[..n]), Some((0x004F, &[][..], &[][..])));
    assert_eq!(unmarshal(&[0x00]), None);
}

#[test]
fn sensor_values_round_and_stay_off_the_unknown_code() {
    let t8 = property_format(0x004F).unwrap();
    assert_eq!(SensorValue::encode(t8, 21.5).bytes(), &[43]);
    assert_eq!(SensorValue::encode(t8, -3.2).bytes(), &[0xFA]);
    assert_eq!(SensorValue::encode(t8, 1000.0).bytes(), &[0x7E]);
    assert_eq!(SensorValue::encode(t8, f32::NAN).bytes(), &[0x7F]);
    assert_eq!(SensorValue::encode(t8, 21.5).decode(t8), Some(21.5));
    assert_eq!(SensorValue::encode(t8, -3.2).decode(t8), Some(-3.0));
    assert_eq!(SensorValue::encode(t8, f32::NAN).decode(t8), None);
    let t16 = property_format(0x0054).unwrap();
    // Held at absolute zero, the characteristic's floor.
    assert_eq!(SensorValue::encode(t16, -400.0).bytes(), &(-27315i16).to_le_bytes());
    let lux = property_format(0x004E).unwrap();
    assert_eq!(SensorValue::encode(lux, 1234.56).bytes(), &[0x40, 0xE2, 0x01]);
    assert_eq!(SensorValue::encode(lux, -1.0).bytes(), &[0, 0, 0]);
}

// ── Two nodes ─────────────────────────────────────────────────────────────

#[test]
fn a_switch_turns_a_light_on_through_their_group() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    assert_eq!(switch.models.onoff_set(&mut switch.stack, None, true, false, 0), Ok(None));
    assert_eq!(air(&mut switch, &mut [&mut light], 0), 1);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: true }]);
    assert!(light.models.on);
    // The light publishes its new state to the group, where the switch hears it.
    assert_eq!(air(&mut light, &mut [&mut switch], 0), 1);
    assert_eq!(model_events(&mut switch), [ModelEvent::OnOffStatus { src: 0x0002, on: true }]);
}

#[test]
fn an_acknowledged_set_is_answered_and_a_repeat_is_the_same_transaction() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    switch.models.onoff_set(&mut switch.stack, Some(0x0002), true, true, 0).unwrap();
    // The same message again, with a SEQ of its own: what the client's
    // retransmission looks like.
    let tid = switch.models.tid;
    let again = [0x82, 0x02, 1, tid];
    switch.stack.send_access(0x0002, Key::App, 0xFF, &again, 100).unwrap();
    air(&mut switch, &mut [&mut light], 100);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: true }]);
    // Two statuses to the switch (one per Set), one publication to the group.
    air(&mut light, &mut [&mut switch], 100);
    assert_eq!(model_events(&mut switch).len(), 3);
    // Past the 6 s window the same TID is a new transaction.
    switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x82, 0x03, 0, tid], 7_000).unwrap();
    air(&mut switch, &mut [&mut light], 7_000);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: false }]);
}

#[test]
fn a_prohibited_onoff_value_is_ignored() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    switch.stack.send_access(0xC000, Key::App, 0xFF, &[0x82, 0x03, 2, 1], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    assert!(model_events(&mut light).is_empty());
}

#[test]
fn a_message_under_another_appkey_or_to_another_group_is_not_taken() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    switch.stack.send_access(0xC005, Key::App, 0xFF, &[0x82, 0x03, 1, 1], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    assert!(model_events(&mut light).is_empty());
    let mut other = Mesh::new(
        &Setup { app_key: h16("00112233445566778899aabbccddeeff"), ..setup(0x0003, &SWITCH, false) },
        &[],
        None,
    );
    store_seq(&mut other);
    other.models.onoff_set(&mut other.stack, None, true, false, 0).unwrap();
    air(&mut other, &mut [&mut light], 0);
    assert!(model_events(&mut light).is_empty());
}

/// A light with five sensors: a full Sensor Status is 21 bytes, three
/// segments.
fn light_with_five_sensors() -> Mesh {
    let mut light = Mesh::new(&setup(0x0002, &LIGHT, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
    store_seq(&mut light);
    light
}

#[test]
fn a_sensor_status_in_segments_is_taken_whole_and_acknowledged() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = light_with_five_sensors();
    assert!(light.models.sensor_set(0x004F, 21.5));
    assert!(light.models.sensor_set(0x004E, 400.0));
    assert!(!light.models.sensor_set(0x0042, 1.0));
    // A Get to the group: the answer comes back to the switch's address.
    switch.models.sensor_get(&mut switch.stack, None, None, 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    assert_eq!(air(&mut light, &mut [&mut switch], 0), 3);
    let got = model_events(&mut switch);
    assert_eq!(got.len(), 5);
    // In ascending order of property, as Mesh Model 1.1 asks.
    let ids: Vec<u16> = got
        .iter()
        .map(|e| match e {
            ModelEvent::Sensor { property, .. } => *property,
            e => panic!("{e:?}"),
        })
        .collect();
    assert_eq!(ids, [0x004C, 0x004E, 0x004F, 0x0054, 0x0076]);
    let t8 = property_format(0x004F).unwrap();
    match got[2] {
        ModelEvent::Sensor { src, property, value } => {
            assert_eq!((src, property, value.decode(t8)), (0x0002, 0x004F, Some(21.5)));
        }
        e => panic!("{e:?}"),
    }
    match got[3] {
        ModelEvent::Sensor { property, value, .. } => {
            assert_eq!((property, value.decode(property_format(0x0054).unwrap())), (0x0054, None));
        }
        e => panic!("{e:?}"),
    }
    // The switch acknowledged the whole at once: the light is done.
    assert_eq!(air(&mut switch, &mut [&mut light], 0), 1);
    assert_eq!(events(&mut light), [Event::Sent { handle: 1, ok: true }]);
}

#[test]
fn a_group_gets_its_segments_in_rounds_and_takes_them_once() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = light_with_five_sensors();
    let h = light.models.sensor_publish(&mut light.stack, 0).unwrap().unwrap();
    assert_eq!(air(&mut light, &mut [&mut switch], 0), 3);
    assert_eq!(model_events(&mut switch).len(), 5);
    // Nobody acknowledges a group: two more rounds, each after the last one
    // went out, then done - and the switch took the message once.
    let mut aired = 0;
    let mut t = 0;
    while !events(&mut light).contains(&Event::Sent { handle: h, ok: true }) {
        t += 50;
        assert!(t < 5_000, "the rounds never ended");
        light.poll(t);
        aired += air(&mut light, &mut [&mut switch], t);
        assert!(switch.stack.outbox.is_empty(), "a group is never acknowledged");
    }
    assert_eq!(aired, 6);
    assert!(model_events(&mut switch).is_empty());
}

#[test]
fn a_lost_segment_is_asked_for_again_and_the_message_completes() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = light_with_five_sensors();
    switch.models.sensor_get(&mut switch.stack, Some(0x0002), None, 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    // Segments 0 and 2 reach the switch, segment 1 is lost.
    let s0 = light.stack.outbox.pop().unwrap();
    let _lost = light.stack.outbox.pop().unwrap();
    let s2 = light.stack.outbox.pop().unwrap();
    assert!(light.stack.outbox.is_empty());
    switch.receive(&s0.pdu[..s0.len], 10);
    switch.receive(&s2.pdu[..s2.len], 10);
    assert!(model_events(&mut switch).is_empty());
    // The switch's acknowledgment timer (150 + 50 x TTL 7 ms) names 0 and 2.
    assert_eq!(switch.poll(10), Some(510));
    switch.poll(510);
    assert_eq!(air(&mut switch, &mut [&mut light], 510), 1);
    // The light sends what is missing at once.
    assert_eq!(air(&mut light, &mut [&mut switch], 510), 1);
    assert_eq!(model_events(&mut switch).len(), 5);
    air(&mut switch, &mut [&mut light], 510);
    assert_eq!(events(&mut light), [Event::Sent { handle: 1, ok: true }]);
}

#[test]
fn a_unicast_segmented_message_nobody_acknowledges_gives_up() {
    let mut light = Mesh::new(&setup(0x0002, &LIGHT, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
    store_seq(&mut light);
    let mut out = [0u8; ACCESS_MAX];
    let n = light.models.sensor_status(None, &mut out);
    let h = light.stack.send_access(0x0009, Key::App, 0xFF, &out[..n], 0).unwrap().unwrap();
    let mut t = 0;
    for _ in 0..10 {
        t += 1_000;
        light.poll(t);
        while light.stack.outbox.pop().is_some() {}
    }
    assert!(events(&mut light).contains(&Event::Sent { handle: h, ok: false }));
}

#[test]
fn a_relay_carries_a_message_past_the_range_of_its_sender() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut relay = node(0x0003, &RELAY_ONLY, true);
    let mut light = node(0x0002, &LIGHT, false);
    switch.models.onoff_set(&mut switch.stack, None, true, false, 0).unwrap();
    // The light does not hear the switch, only the relay.
    let t = switch.stack.outbox.pop().unwrap();
    relay.receive(&t.pdu[..t.len], 0);
    let r = relay.stack.outbox.pop().unwrap();
    let keys = k2(&h16(NET_KEY), &[0x00]);
    let before = net_decode(&keys, 0x1234_5678, &t.pdu[..t.len]).unwrap();
    let after = net_decode(&keys, 0x1234_5678, &r.pdu[..r.len]).unwrap();
    assert_eq!((after.ttl, after.seq, after.src), (before.ttl - 1, before.seq, before.src));
    light.receive(&r.pdu[..r.len], 0);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: true }]);
    // The relay hearing its own relayed PDU, or the original again, adds nothing.
    relay.receive(&r.pdu[..r.len], 0);
    relay.receive(&t.pdu[..t.len], 0);
    assert!(relay.stack.outbox.is_empty());
}

#[test]
fn ttl_1_is_not_relayed() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut relay = node(0x0003, &RELAY_ONLY, true);
    switch.stack.send_access(0xC000, Key::App, 1, &[0x82, 0x03, 1, 1], 0).unwrap();
    air(&mut switch, &mut [&mut relay], 0);
    assert!(relay.stack.outbox.is_empty());
}

#[test]
fn an_old_pdu_replayed_after_the_cache_forgot_it_is_refused() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    switch.models.onoff_set(&mut switch.stack, None, true, false, 0).unwrap();
    let old = switch.stack.outbox.pop().unwrap();
    light.receive(&old.pdu[..old.len], 0);
    assert_eq!(model_events(&mut light).len(), 1);
    // Newer traffic from the switch pushes the old PDU out of the cache.
    for i in 0..NET_CACHE as u8 {
        switch.stack.send_access(0xC005, Key::App, 0xFF, &[0x82, 0x03, 1, i], 0).unwrap();
        air(&mut switch, &mut [&mut light], 0);
    }
    light.models.on = false;
    light.receive(&old.pdu[..old.len], 0);
    assert!(model_events(&mut light).is_empty());
    assert!(!light.models.on);
}

#[test]
fn nothing_is_sent_before_the_seq_is_stored_and_a_reset_never_reuses_one() {
    let mut a = Mesh::new(&setup(0x0001, &SWITCH, false), &[], None);
    assert_eq!(a.models.onoff_set(&mut a.stack, None, true, false, 0), Err(SendError::SeqNotStored));
    let first = a.stack.seq_to_store().unwrap();
    assert_eq!(first, SEQ_STORE_RATE);
    a.stack.seq_stored(first);
    assert_eq!(a.stack.seq_to_store(), None);
    // Past half the reserve: the next one is asked for while sending goes on.
    for _ in 0..=SEQ_STORE_RATE / 2 {
        a.stack.send_access(0xC000, Key::App, 0xFF, &[0x82, 0x03, 1, 1], 0).unwrap();
    }
    assert_eq!(a.stack.seq_to_store(), Some(SEQ_STORE_RATE / 2 + 1 + SEQ_STORE_RATE));
    // The board resets: it starts where the store said, past every SEQ used.
    let used = a.stack.seq();
    let b = Mesh::new(&setup(0x0001, &SWITCH, false), &[], Some(first));
    assert!(b.stack.seq() >= used);
}

#[test]
fn the_seq_runs_out_at_24_bits() {
    let mut a = Mesh::new(&setup(0x0001, &SWITCH, false), &[], Some(SEQ_LIMIT - 3));
    store_seq(&mut a);
    for _ in 0..3 {
        a.stack.send_access(0xC000, Key::App, 0xFF, &[0x82, 0x03, 1, 1], 0).unwrap();
    }
    assert_eq!(
        a.stack.send_access(0xC000, Key::App, 0xFF, &[0x82, 0x03, 1, 1], 0),
        Err(SendError::SeqNotStored)
    );
    assert_eq!(a.stack.seq_to_store(), None);
}

#[test]
fn the_sensor_server_answers_a_get_for_one_property_and_for_one_it_lacks() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    light.models.sensor_set(0x004C, 12.0);
    switch.models.sensor_get(&mut switch.stack, Some(0x0002), Some(0x004C), 0).unwrap();
    switch.models.sensor_get(&mut switch.stack, Some(0x0002), Some(0x0042), 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    air(&mut light, &mut [&mut switch], 0);
    let got = model_events(&mut switch);
    assert_eq!(got.len(), 2);
    assert!(matches!(got[0], ModelEvent::Sensor { property: 0x004C, value, .. } if value.bytes() == [12, 0]));
    assert!(matches!(got[1], ModelEvent::Sensor { property: 0x0042, value, .. } if value.len == 0));
}

static PERIODIC: [ModelSetup; 2] = [
    ModelSetup {
        id: SENSOR_SERVER,
        publication: Publication {
            address: 0xC001,
            ttl: 0xFF,
            // 10 steps of 1 s.
            period: 0x40 | 10,
            retransmit: 0,
            app_index: 0,
            credentials: false,
        },
        subscribe: &NO_SUBS,
    },
    ModelSetup { id: SENSOR_SETUP_SERVER, publication: Publication::NONE, subscribe: &NO_SUBS },
];

#[test]
fn a_periodic_publication_goes_out_every_period() {
    let mut s = node(0x0002, &PERIODIC, false);
    let mut client = node(0x0001, &SWITCH, false);
    assert_eq!(s.poll(0), Some(10_000));
    assert!(s.stack.outbox.is_empty());
    assert_eq!(s.poll(10_000), Some(20_000));
    air(&mut s, &mut [&mut client], 10_000);
    assert_eq!(model_events(&mut client).len(), 2);
}

#[test]
fn a_changed_sensor_value_is_published_at_once_and_at_most_once_a_second() {
    let mut s = node(0x0002, &PERIODIC, false);
    let mut client = node(0x0001, &SWITCH, false);
    s.poll(0);
    assert!(s.models.sensor_set(0x004F, 20.0));
    s.poll(100);
    air(&mut s, &mut [&mut client], 100);
    assert_eq!(model_events(&mut client).len(), 2);
    // The same value again is no change.
    assert!(s.models.sensor_set(0x004F, 20.0));
    s.poll(200);
    assert!(s.stack.outbox.is_empty());
    // A new one within the second waits for it.
    assert!(s.models.sensor_set(0x004F, 21.0));
    assert_eq!(s.poll(300), Some(1_100));
    assert!(s.stack.outbox.is_empty());
    s.poll(1_100);
    air(&mut s, &mut [&mut client], 1_100);
    let got = model_events(&mut client);
    assert!(got.contains(&ModelEvent::Sensor {
        src: 0x0002,
        property: 0x004F,
        value: SensorValue::encode(property_format(0x004F).unwrap(), 21.0)
    }));
}

#[test]
fn sensor_values_are_held_inside_their_characteristic() {
    let pct = property_format(0x0042).unwrap();
    assert_eq!(SensorValue::encode(pct, 120.0).bytes(), &[200]);
    let v = property_format(0x0059).unwrap();
    assert_eq!(SensorValue::encode(v, 2000.0).decode(v), Some(1022.0));
    let t = property_format(0x0054).unwrap();
    assert_eq!(SensorValue::encode(t, -300.0).bytes(), &(-27315i16).to_le_bytes());
}

#[test]
fn the_setup_server_hears_its_sensor_server_s_group() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    // Sensor Cadence Get for 0x004F to the sensors' group.
    switch.stack.send_access(0xC001, Key::App, 0xFF, &[0x82, 0x34, 0x4F, 0x00], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    let t = light.stack.outbox.pop().expect("a Cadence Status");
    let keys = k2(&h16(NET_KEY), &[0x00]);
    assert_eq!(net_decode(&keys, 0x1234_5678, &t.pdu[..t.len]).unwrap().dst, 0x0001);
}

#[test]
fn a_column_get_is_answered_with_the_property_alone() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x82, 0x32, 0x4F, 0x00, 0x2A], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    let mut p = provisioner_like(0x0001);
    let got = take_app(&mut p, &mut light);
    assert_eq!(got, Some(vec![0x53, 0x4F, 0x00]));
}

// ── Generic OnOff: transactions, transitions ──────────────────────────────

/// The access payload of the next message `node` sends to `p` under the
/// AppKey, as `p` reads it.
fn take_app(p: &mut Mesh, node: &mut Mesh) -> Option<Vec<u8>> {
    let mut got: Option<Vec<u8>> = None;
    while let Some(t) = node.stack.outbox.pop() {
        let mut cap = |_s: &mut Stack, m: &Incoming<'_>| {
            let mut b = [0u8; 4];
            let n = opcode_bytes(m.opcode, &mut b);
            let mut v = b[..n].to_vec();
            v.extend_from_slice(m.params);
            got = Some(v);
        };
        p.stack.receive(&t.pdu[..t.len], 0, &mut cap);
    }
    got
}

/// A node at `address` that takes any AppKey message to it: a model-less
/// reader for the tests.
fn provisioner_like(address: u16) -> Mesh {
    let mut m = Mesh::new(&setup(address, &RELAY_ONLY, false), &[], None);
    store_seq(&mut m);
    m
}

#[test]
fn a_set_to_the_same_value_is_published_too() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    for tid_step in 0..2 {
        switch.models.onoff_set(&mut switch.stack, None, false, false, tid_step * 10).unwrap();
        air(&mut switch, &mut [&mut light], tid_step * 10);
        assert_eq!(air(&mut light, &mut [&mut switch], tid_step * 10), 1, "round {tid_step}");
    }
    assert_eq!(model_events(&mut switch).len(), 2);
}

#[test]
fn a_repeat_inside_6_s_of_the_last_copy_is_still_the_same_transaction() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    for t in [0, 5_000, 10_000] {
        switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x82, 0x03, 1, 7], t).unwrap();
        air(&mut switch, &mut [&mut light], t);
    }
    assert_eq!(model_events(&mut light).len(), 1);
}

#[test]
fn a_transition_to_on_switches_at_its_start_and_to_off_at_its_end() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = node(0x0002, &LIGHT, false);
    let mut reader = provisioner_like(0x0001);
    // On, in 1 s (steps 10 of 100 ms), after 50 ms (10 x 5 ms).
    switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x82, 0x02, 1, 1, 10, 10], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    // The answer: still off, going to on, 1.05 s left (rounded up to 11 x 100 ms).
    assert_eq!(take_app(&mut reader, &mut light), Some(vec![0x82, 0x04, 0, 1, 11]));
    assert!(!light.models.on && model_events(&mut light).is_empty());
    assert_eq!(light.poll(10), Some(50));
    light.poll(50);
    assert!(light.models.on);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: true }]);
    light.poll(1_050);
    // The end publishes the state reached, to the group the switch hears.
    assert_eq!(air(&mut light, &mut [&mut switch], 1_050), 1);
    assert_eq!(model_events(&mut switch), [ModelEvent::OnOffStatus { src: 0x0002, on: true }]);
    // Off in 1 s: on until the end.
    switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x82, 0x03, 0, 2, 10, 0], 2_000).unwrap();
    air(&mut switch, &mut [&mut light], 2_000);
    light.poll(2_500);
    assert!(light.models.on);
    light.poll(3_000);
    assert!(!light.models.on);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: false }]);
}

#[test]
fn transition_times_round_up_and_0x3f_steps_are_at_once() {
    assert_eq!(transition_ms(0x0A), 1_000);
    assert_eq!(transition_ms(0x41), 1_000);
    assert_eq!(transition_ms(0x3F), 0);
    assert_eq!(transition_byte(1_050), 11);
    assert_eq!(transition_byte(6_300), 0x47);
    assert_eq!(transition_byte(u64::MAX / 2), 0x3F);
}

// ── The lower transport against the specification ────────────────────────

const DEV_KEY_1201: &str = "9d6dd0e96eb25dc19a40ed9914f8f03f";

/// A node as the sample messages have them: IV index 0x12345678, its next
/// SEQ `seq`, the DevKey of node 0x1201.
fn sample_node(address: u16, seq: u32, ttl: u8, relay: bool) -> Mesh {
    let s = Setup { dev_key: h16(DEV_KEY_1201), default_ttl: ttl, ..setup(address, &RELAY_ONLY, relay) };
    let mut m = Mesh::new(&s, &[], Some(seq));
    store_seq(&mut m);
    m
}

fn pdu_hex(t: &Transmission) -> String {
    t.pdu[..t.len].iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn sample_message_6_goes_out_as_its_two_segments_and_7_and_9_end_it() {
    let mut n = sample_node(0x0003, 0x3129ab, 4, false);
    let access = h("0056341263964771734fbd76e3b40519d1d94a48");
    let handle = n.stack.send_access(0x1201, Key::Device, 4, &access, 0).unwrap().unwrap();
    let s0 = n.stack.outbox.pop().unwrap();
    let s1 = n.stack.outbox.pop().unwrap();
    assert_eq!(pdu_hex(&s0), "68cab5c5348a230afba8c63d4e686364979deaf4fd40961145939cda0e");
    assert_eq!(pdu_hex(&s1), "681615b5dd4a846cae0c032bf0746f44f1b8cc8ce5edc57e55beed49c0");
    // #7: the Friend of 1201 acknowledges segment 1 on its behalf (OBO).
    n.receive(&h("68e476b5579c980d0d730f94d7f3509df987bb417eb7c05f"), 100);
    // #8: segment 0 again, on the next SEQ.
    let again = n.stack.outbox.pop().expect("segment 0 resent");
    assert_eq!(pdu_hex(&again), "684daa6267c2cf0e2f91add6f06e66006844cec97f973105ae2534f958");
    // #9: both now.
    n.receive(&h("68aec467ed4901d85d806bbed248614f938067b0d983bb7b"), 200);
    assert_eq!(events(&mut n), [Event::Sent { handle, ok: true }]);
}

#[test]
fn sample_message_16_is_1201_s_answer_and_17_its_relayed_copy() {
    let mut n = sample_node(0x1201, 0x000006, 0x0b, false);
    n.stack.send_access(0x0003, Key::Device, 0x0b, &h("800300563412"), 0).unwrap();
    let t = n.stack.outbox.pop().unwrap();
    assert_eq!(pdu_hex(&t), "68e80e5da5af0e6b9be7f5a642f2f98680e61c3a8b47f228");
    let mut relay = sample_node(0x0100, 0, 7, true);
    relay.receive(&t.pdu[..t.len], 0);
    assert_eq!(pdu_hex(&relay.stack.outbox.pop().unwrap()), "68b2bd2c1e1b6f2a80d381b91f824dd4f0a3cd54cea23b7a");
}

#[test]
fn the_sample_virtual_addresses() {
    assert_eq!(virtual_address(&h16("0073e7e4d8b9440faf8415df4c56c0e1")), 0xB529);
    assert_eq!(virtual_address(&h16("f4a002c7fb1e4ca0a469a021de0db875")), 0x9736);
}

#[test]
fn segments_out_of_order_are_not_taken_for_a_replay() {
    let mut sender = sample_node(0x0003, 0x3129ab, 4, false);
    let mut receiver = sample_node(0x1201, 0x10, 4, false);
    let access = h("0056341263964771734fbd76e3b40519d1d94a48");
    sender.stack.send_access(0x1201, Key::Device, 4, &access, 0).unwrap();
    let s0 = sender.stack.outbox.pop().unwrap();
    let s1 = sender.stack.outbox.pop().unwrap();
    let mut got = Vec::new();
    for s in [&s1, &s0] {
        receiver.stack.receive(&s.pdu[..s.len], 0, &mut |_s: &mut Stack, m: &Incoming<'_>| got.push(m.opcode));
    }
    // The AppKey Add (opcode 0x00) reached the Configuration Server, which
    // answered it (Invalid NetKey Index: the sample uses index 0x456).
    let mut out = Vec::new();
    while let Some(t) = receiver.stack.outbox.pop() {
        out.push(t);
    }
    assert_eq!(out.len(), 2, "the acknowledgment and the AppKey Status");
}

#[test]
fn one_segmented_message_to_a_destination_at_a_time() {
    let mut light = light_with_five_sensors();
    let mut out = [0u8; ACCESS_MAX];
    let n = light.models.sensor_status(None, &mut out);
    assert!(light.stack.send_access(0x0009, Key::App, 0xFF, &out[..n], 0).is_ok());
    assert_eq!(light.stack.send_access(0x0009, Key::App, 0xFF, &out[..n], 0), Err(SendError::Busy));
    assert!(light.stack.send_access(0x000A, Key::App, 0xFF, &out[..n], 0).is_ok());
}

#[test]
fn the_acknowledgment_timer_fires_once_per_new_segment() {
    let mut switch = node(0x0001, &SWITCH, false);
    let mut light = light_with_five_sensors();
    switch.models.sensor_get(&mut switch.stack, Some(0x0002), None, 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    let s0 = light.stack.outbox.pop().unwrap();
    while light.stack.outbox.pop().is_some() {}
    switch.receive(&s0.pdu[..s0.len], 0);
    let mut acks = 0;
    for t in (100..5_000).step_by(100) {
        switch.poll(t);
        while switch.stack.outbox.pop().is_some() {
            acks += 1;
        }
    }
    assert_eq!(acks, 1);
}

#[test]
fn a_completed_message_gives_its_slot_to_a_new_source() {
    let mut a = light_with_five_sensors();
    let mut b = Mesh::new(&setup(0x0003, &LIGHT, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
    store_seq(&mut b);
    let mut c = Mesh::new(&setup(0x0004, &LIGHT, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
    store_seq(&mut c);
    let mut switch = node(0x0001, &SWITCH, false);
    for (i, n) in [&mut a, &mut b, &mut c].into_iter().enumerate() {
        let h = n.models.sensor_publish(&mut n.stack, 0).unwrap();
        assert!(h.is_some());
        air(n, &mut [&mut switch], 0);
        assert_eq!(model_events(&mut switch).len(), 5, "source {i}");
    }
}

#[test]
fn a_message_longer_than_the_node_takes_is_refused_with_block_ack_0() {
    // A first segment of a 9-segment message, by hand.
    let keys = k2(&h16(NET_KEY), &[0x00]);
    let seq = 5u32;
    let mut lt = [0u8; 16];
    lt[0] = 0x80 | 0x40 | k4(&h16(APP_KEY));
    lt[1] = 0;
    lt[2] = ((seq as u8) & 0x3F) << 2;
    lt[3] = 8;
    let p = pdu(false, 5, seq, 0x0001, 0x0002, &lt);
    let mut raw = [0u8; NET_MAX];
    let n = net_encode(&keys, &p, &mut raw);
    let mut light = node(0x0002, &LIGHT, false);
    light.receive(&raw[..n], 0);
    let t = light.stack.outbox.pop().expect("an acknowledgment");
    let ack = net_decode(&keys, 0x1234_5678, &t.pdu[..t.len]).unwrap();
    assert!(ack.ctl);
    assert_eq!(&ack.transport()[3..7], &[0, 0, 0, 0]);
}

#[test]
fn ttl_1_never_goes_on_the_air() {
    let mut a = node(0x0001, &SWITCH, false);
    assert_eq!(a.stack.send_access(0xC000, Key::App, 1, &[0x82, 0x03, 1, 1], 0), Ok(None));
    assert!(a.stack.outbox.is_empty());
}

#[test]
fn a_publication_is_retransmitted_as_its_publish_retransmit_says() {
    let mut switch = node(0x0001, &SWITCH, false);
    let i = switch.stack.model_index(ONOFF_CLIENT).unwrap();
    // 2 more times, (1 + 1) x 50 ms apart.
    switch.stack.models[i].publication.retransmit = (1 << 3) | 2;
    let mut light = node(0x0002, &LIGHT, false);
    switch.models.onoff_set(&mut switch.stack, None, true, false, 0).unwrap();
    let mut sent = air(&mut switch, &mut [&mut light], 0);
    for t in [100, 200, 300] {
        switch.poll(t);
        sent += air(&mut switch, &mut [&mut light], t);
    }
    assert_eq!(sent, 3);
    // One transaction: the light took it once.
    assert_eq!(model_events(&mut light).len(), 1);
}

// ── The Configuration Server, from a provisioner holding the DevKey ───────

/// A "provisioner" at 0x0100 that holds `node`'s DevKey: its own Stack, the
/// node's key put in as its own, sees the node's DevKey answers.
fn provisioner_for(address: u16) -> Mesh {
    let mut s = setup(0x0100, &RELAY_ONLY, false);
    s.dev_key = setup(address, &LIGHT, false).dev_key;
    let mut m = Mesh::new(&s, &[], None);
    store_seq(&mut m);
    m
}

/// Ask `node` one Config message, return its answer's access payload.
fn ask(p: &mut Mesh, node: &mut Mesh, msg: &[u8]) -> Option<Vec<u8>> {
    p.stack.send_access(node.stack.address, Key::Device, 0xFF, msg, 0).unwrap();
    air(p, &mut [node], 0);
    let mut got: Option<Vec<u8>> = None;
    for _ in 0..4 {
        while let Some(t) = node.stack.outbox.pop() {
            let mut cap = |_s: &mut Stack, m: &Incoming<'_>| {
                let mut b = [0u8; 4];
                let n = opcode_bytes(m.opcode, &mut b);
                let mut v = b[..n].to_vec();
                v.extend_from_slice(m.params);
                got = Some(v);
            };
            p.stack.receive(&t.pdu[..t.len], 0, &mut cap);
            store_seq(p);
        }
        // Segment acknowledgments back to the node.
        air(p, &mut [node], 0);
    }
    got
}

#[test]
fn the_composition_data_lists_the_models() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    let got = ask(&mut p, &mut light, &[0x80, 0x08, 0x00]).unwrap();
    assert_eq!(
        got,
        [
            0x02, 0x00, // status, page 0
            0xF1, 0x05, 0x01, 0x00, 0x01, 0x00, // CID, PID, VID
            32, 0, // CRPL
            0x01, 0x00, // features: relay
            0x00, 0x00, 4, 0, // element: location, 4 SIG models, no vendor
            0x00, 0x00, 0x00, 0x10, 0x00, 0x11, 0x01, 0x11,
        ]
    );
}

#[test]
fn a_subscription_added_by_the_configuration_server_takes_effect() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut switch = node(0x0001, &SWITCH, false);
    let mut p = provisioner_for(0x0002);
    // Model Subscription Add: element 0x0002, group 0xC123, Generic OnOff Server.
    let got = ask(&mut p, &mut light, &[0x80, 0x1B, 0x02, 0x00, 0x23, 0xC1, 0x00, 0x10]).unwrap();
    assert_eq!(got, [0x80, 0x1F, 0x00, 0x02, 0x00, 0x23, 0xC1, 0x00, 0x10]);
    assert!(events(&mut light).contains(&Event::ConfigChanged));
    switch.stack.send_access(0xC123, Key::App, 0xFF, &[0x82, 0x03, 1, 9], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    assert_eq!(model_events(&mut light), [ModelEvent::OnOffSet { src: 0x0001, on: true }]);
    // SIG Model Subscription Get lists both.
    let got = ask(&mut p, &mut light, &[0x80, 0x29, 0x02, 0x00, 0x00, 0x10]).unwrap();
    assert_eq!(got, [0x80, 0x2A, 0x00, 0x02, 0x00, 0x00, 0x10, 0x00, 0xC0, 0x23, 0xC1]);
}

#[test]
fn the_configuration_server_refuses_what_it_has_not() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    // A model the element does not have: Invalid Model.
    let got = ask(&mut p, &mut light, &[0x80, 0x1B, 0x02, 0x00, 0x23, 0xC1, 0x01, 0x10]).unwrap();
    assert_eq!(got[2], 0x02);
    // Another element address: Invalid Address.
    let got = ask(&mut p, &mut light, &[0x80, 0x18, 0x05, 0x00, 0x00, 0x10]).unwrap();
    assert_eq!(got[2], 0x01);
    // GATT Proxy: not supported.
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x12]).unwrap(), [0x80, 0x14, 0x02]);
    // A Default TTL of 1 is prohibited: no answer at all.
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x0D, 0x01]), None);
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x0D, 0x05]).unwrap(), [0x80, 0x0E, 0x05]);
    assert_eq!(light.stack.default_ttl, 5);
}

#[test]
fn the_configuration_server_ignores_the_appkey_and_other_nodes_devkeys() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut switch = node(0x0001, &SWITCH, false);
    switch.stack.send_access(0x0002, Key::App, 0xFF, &[0x80, 0x0D, 0x05], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    switch.stack.send_access(0x0002, Key::Device, 0xFF, &[0x80, 0x0D, 0x05], 0).unwrap();
    air(&mut switch, &mut [&mut light], 0);
    assert_eq!(light.stack.default_ttl, 7);
    assert!(light.stack.outbox.is_empty());
}

#[test]
fn the_beacon_state_is_kept_and_a_node_reset_takes_the_node_off_the_network() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x0A, 0x01]).unwrap(), [0x80, 0x0B, 0x01]);
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x09]).unwrap(), [0x80, 0x0B, 0x01]);
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x49]).unwrap(), [0x80, 0x4A]);
    assert!(events(&mut light).contains(&Event::Removed));
    assert_eq!(ask(&mut p, &mut light, &[0x80, 0x0C]), None);
    assert_eq!(
        light.models.onoff_local(&mut light.stack, true, 0),
        Err(SendError::Removed)
    );
}

#[test]
fn the_mandatory_configuration_messages_are_answered() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    let cases: [(&[u8], &[u8]); 9] = [
        // Heartbeat Publication Get / Subscription Get: none.
        (&[0x80, 0x38], &[0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        (&[0x80, 0x3A], &[0x80, 0x3C, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
        // Key Refresh Phase Get: phase 0; another NetKey: Invalid NetKey Index.
        (&[0x80, 0x15, 0x00, 0x00], &[0x80, 0x17, 0x00, 0x00, 0x00, 0x00]),
        (&[0x80, 0x15, 0x01, 0x00], &[0x80, 0x17, 0x04, 0x01, 0x00, 0x00]),
        // Node Identity: not supported.
        (&[0x80, 0x46, 0x00, 0x00], &[0x80, 0x48, 0x00, 0x00, 0x00, 0x02]),
        // LPN PollTimeout: none.
        (&[0x80, 0x2D, 0x05, 0x00], &[0x80, 0x2E, 0x05, 0x00, 0, 0, 0]),
        // NetKey Delete of the only one: Cannot Remove.
        (&[0x80, 0x41, 0x00, 0x00], &[0x80, 0x44, 0x0C, 0x00, 0x00]),
        // Vendor Model Subscription Get: no such model.
        (&[0x80, 0x2B, 0x02, 0x00, 0x59, 0x00, 0x01, 0x00], &[0x80, 0x2C, 0x02, 0x02, 0x00, 0x59, 0x00, 0x01, 0x00]),
        // The Configuration Server's App list: empty, not an error.
        (&[0x80, 0x4B, 0x02, 0x00, 0x00, 0x00], &[0x80, 0x4C, 0x00, 0x02, 0x00, 0x00, 0x00]),
    ];
    for (ask_for, want) in cases {
        assert_eq!(ask(&mut p, &mut light, ask_for).as_deref(), Some(want), "{ask_for:02x?}");
    }
}

#[test]
fn a_prohibited_publication_set_gets_no_status_and_unbinding_ends_the_publication() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    // TTL 0x80: prohibited, no answer - even for the Configuration Server.
    assert_eq!(ask(&mut p, &mut light, &[0x03, 0x02, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x80, 0, 0, 0x00, 0x00]), None);
    // A virtual address in the 16-bit field: prohibited too.
    assert_eq!(ask(&mut p, &mut light, &[0x03, 0x02, 0x00, 0x29, 0xB5, 0x00, 0x00, 0x05, 0, 0, 0x00, 0x10]), None);
    // Friendship credentials: Feature Not Supported.
    let got = ask(&mut p, &mut light, &[0x03, 0x02, 0x00, 0x00, 0xC0, 0x00, 0x10, 0x05, 0, 0, 0x00, 0x10]).unwrap();
    assert_eq!(got[2], 0x0A);
    // Unbind the OnOff Server: its publication goes.
    let got = ask(&mut p, &mut light, &[0x80, 0x3F, 0x02, 0x00, 0x00, 0x00, 0x00, 0x10]).unwrap();
    assert_eq!(got[2], 0x00);
    let got = ask(&mut p, &mut light, &[0x80, 0x18, 0x02, 0x00, 0x00, 0x10]).unwrap();
    assert_eq!(got, [0x80, 0x19, 0x00, 0x02, 0x00, 0, 0, 0, 0, 0, 0, 0, 0x00, 0x10]);
}

#[test]
fn a_virtual_subscription_is_refused_with_its_address() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    let mut msg = vec![0x80, 0x20, 0x02, 0x00];
    msg.extend(h("0073e7e4d8b9440faf8415df4c56c0e1"));
    msg.extend([0x00, 0x10]);
    assert_eq!(ask(&mut p, &mut light, &msg).unwrap(), [0x80, 0x1F, 0x05, 0x02, 0x00, 0x29, 0xB5, 0x00, 0x10]);
}

#[test]
fn a_reply_to_ttl_0_goes_out_with_ttl_0() {
    let mut light = node(0x0002, &LIGHT, false);
    let mut p = provisioner_for(0x0002);
    p.stack.send_access(0x0002, Key::Device, 0, &[0x80, 0x0C], 0).unwrap();
    air(&mut p, &mut [&mut light], 0);
    let t = light.stack.outbox.pop().unwrap();
    let keys = k2(&h16(NET_KEY), &[0x00]);
    assert_eq!(net_decode(&keys, 0x1234_5678, &t.pdu[..t.len]).unwrap().ttl, 0);
}

// ── After the review ──────────────────────────────────────────────────────

static BOTH: [ModelSetup; 2] = [
    ModelSetup { id: ONOFF_SERVER, publication: publish_to(0xC000), subscribe: &GROUP_1 },
    ModelSetup { id: ONOFF_CLIENT, publication: publish_to(0xC000), subscribe: &GROUP_1 },
];

#[test]
fn a_node_s_own_switch_reaches_its_own_light() {
    let mut n = node(0x0005, &BOTH, false);
    n.models.onoff_set(&mut n.stack, None, true, false, 0).unwrap();
    n.local(0);
    let got = model_events(&mut n);
    assert!(got.contains(&ModelEvent::OnOffSet { src: 0x0005, on: true }), "{got:?}");
    // The light's Status, published to the group, reaches the node's switch too.
    assert!(got.contains(&ModelEvent::OnOffStatus { src: 0x0005, on: true }), "{got:?}");
    assert!(n.models.on);
    // On the air as well: the Set and the Status.
    let mut aired = 0;
    while n.stack.outbox.pop().is_some() {
        aired += 1;
    }
    assert_eq!(aired, 2);
}

#[test]
fn a_message_to_the_node_s_own_address_never_goes_on_the_air() {
    let mut n = node(0x0005, &BOTH, false);
    n.models.onoff_set(&mut n.stack, Some(0x0005), true, true, 0).unwrap();
    n.local(0);
    assert!(n.models.on);
    // The Set stayed home; only the light's publication to the group went out.
    let keys = k2(&h16(NET_KEY), &[0x00]);
    while let Some(t) = n.stack.outbox.pop() {
        assert_eq!(net_decode(&keys, 0x1234_5678, &t.pdu[..t.len]).unwrap().dst, 0xC000);
    }
}

#[test]
fn a_whole_message_is_never_delivered_twice_after_its_slot_went() {
    let mut light = light_with_five_sensors();
    let mut switch = node(0x0001, &SWITCH, false);
    let mut out = [0u8; ACCESS_MAX];
    let n = light.models.sensor_status(None, &mut out);
    light.stack.send_access(0x0001, Key::App, 0xFF, &out[..n], 0).unwrap();
    let segs: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    for s in &segs {
        switch.receive(&s.pdu[..s.len], 0);
    }
    assert_eq!(model_events(&mut switch).len(), 5);
    while switch.stack.outbox.pop().is_some() {}
    // Two other sources take both reassembly slots.
    for addr in [0x0003u16, 0x0004] {
        let mut other = Mesh::new(&setup(addr, &LIGHT, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
        store_seq(&mut other);
        other.stack.send_access(0x0001, Key::App, 0xFF, &out[..n], 0).unwrap();
        air(&mut other, &mut [&mut switch], 0);
    }
    model_events(&mut switch);
    while switch.stack.outbox.pop().is_some() {}
    // The light repeats its segments with new SEQs, as after a lost ack.
    light.stack.poll(5_000);
    air(&mut light, &mut [&mut switch], 5_000);
    assert!(model_events(&mut switch).is_empty(), "delivered twice");
    // ... and is told again that the switch has it all.
    assert!(!switch.stack.outbox.is_empty());
}

#[test]
fn a_group_message_does_not_end_a_unicast_one_from_the_same_source() {
    let mut light = light_with_five_sensors();
    let mut switch = node(0x0001, &SWITCH, false);
    let mut out = [0u8; ACCESS_MAX];
    let n = light.models.sensor_status(None, &mut out);
    light.stack.send_access(0x0001, Key::App, 0xFF, &out[..n], 0).unwrap();
    let unicast: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    let i = light.stack.model_index(SENSOR_SERVER).unwrap();
    light.stack.publish(i, &out[..n], 0).unwrap();
    let group: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    // Unicast segment 0 arrives, its other two are lost; the switch
    // acknowledges segment 0, so the light resends only 1 and 2.
    switch.receive(&unicast[0].pdu[..unicast[0].len], 0);
    switch.poll(1_000);
    air(&mut switch, &mut [&mut light], 1_000);
    let resent: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    assert_eq!(resent.len(), 2);
    // The whole group message gets there first.
    for s in &group {
        switch.receive(&s.pdu[..s.len], 1_000);
    }
    assert_eq!(model_events(&mut switch).len(), 5);
    // The unicast slot is still there, segment 0 in it: the two resent
    // segments complete the message.
    for s in &resent {
        switch.receive(&s.pdu[..s.len], 1_100);
    }
    assert_eq!(model_events(&mut switch).len(), 5);
}

#[test]
fn the_deadline_poll_returns_covers_what_the_models_just_sent() {
    let mut s = node(0x0002, &PERIODIC, false);
    // Five sensors' worth of status is segmented: its SAR timer must be in
    // the deadline the glue sleeps until.
    let mut big = Mesh::new(&setup(0x0002, &PERIODIC, false), &[0x004F, 0x0054, 0x004E, 0x0076, 0x004C], None);
    store_seq(&mut big);
    big.poll(0);
    let next = big.poll(10_000).unwrap();
    assert!(next < 20_000, "the group rounds come before the next period: {next}");
    let _ = s.poll(0);
}

#[test]
fn repeats_of_a_whole_message_are_acknowledged_at_most_every_150_ms() {
    let mut light = light_with_five_sensors();
    let mut switch = node(0x0001, &SWITCH, false);
    let mut out = [0u8; ACCESS_MAX];
    let n = light.models.sensor_status(None, &mut out);
    light.stack.send_access(0x0001, Key::App, 0xFF, &out[..n], 0).unwrap();
    let segs: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    for s in &segs {
        switch.receive(&s.pdu[..s.len], 0);
    }
    assert_eq!(std::iter::from_fn(|| switch.stack.outbox.pop()).count(), 1);
    // The same three segments again at once: no new acknowledgment yet.
    light.stack.poll(2_000);
    let again: Vec<Transmission> = std::iter::from_fn(|| light.stack.outbox.pop()).collect();
    for s in &again {
        switch.receive(&s.pdu[..s.len], 100);
    }
    assert_eq!(std::iter::from_fn(|| switch.stack.outbox.pop()).count(), 0);
    for s in &again {
        switch.receive(&s.pdu[..s.len], 300);
    }
    assert_eq!(std::iter::from_fn(|| switch.stack.outbox.pop()).count(), 0, "repeats are new PDUs only once");
}
