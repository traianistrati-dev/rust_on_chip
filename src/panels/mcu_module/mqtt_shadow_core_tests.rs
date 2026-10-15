//! The firmware's Device Shadow logic over MQTT
//! (`codegen/iot_templates/mqtt_shadow_core.rs`), the very text a project's
//! mqtt.rs carries, run on the PC with a block of its own: which topic a
//! message came on, the request documents and their clientToken, which answer
//! is the pending request's, and what the board keeps of the deltas.
//!
//! ```text
//! cargo test --bin rust_on_chip mqtt_shadow_core
//! ```

// The generated block above the tail, as the IDE writes it for thing "t" with
// one named shadow.
const SHADOW_THING: &str = "t";
const SHADOW_NAMES: &[&str] = &["config"];
const SHADOW_MAX: usize = 256;
const SHADOW_SUBSCRIBE: &[&str] = &[
    "$aws/things/t/shadow/get/accepted",
    "$aws/things/t/shadow/get/rejected",
    "$aws/things/t/shadow/update/accepted",
    "$aws/things/t/shadow/update/rejected",
    "$aws/things/t/shadow/update/delta",
    "$aws/things/t/shadow/delete/accepted",
    "$aws/things/t/shadow/delete/rejected",
    "$aws/things/t/shadow/name/config/get/accepted",
    "$aws/things/t/shadow/name/config/get/rejected",
    "$aws/things/t/shadow/name/config/update/accepted",
    "$aws/things/t/shadow/name/config/update/rejected",
    "$aws/things/t/shadow/name/config/update/delta",
    "$aws/things/t/shadow/name/config/delete/accepted",
    "$aws/things/t/shadow/name/config/delete/rejected",
];

include!("codegen/iot_templates/mqtt_shadow_core.rs");
include!("codegen/iot_templates/json.rs");

/// The block above is what the generator writes: the order of KINDS is the
/// order of `iot::shadow_filters`, and request topics are cut from it.
#[test]
fn the_topics_are_the_generators() {
    let names = vec!["config".to_owned()];
    assert_eq!(SHADOW_SUBSCRIBE, crate::panels::mcu_module::iot::shadow_filters("t", &names).as_slice());
    let _ = SHADOW_THING;
}

#[test]
fn every_answer_topic_is_its_shadow_and_kind_and_nothing_else_is() {
    for (i, t) in SHADOW_SUBSCRIBE.iter().enumerate() {
        assert_eq!(shadow_topic(t), Some((i / 7, KINDS[i % 7])), "{t}");
    }
    for t in [
        "$aws/things/t/shadow/update/documents",
        "$aws/things/t/shadow/update",
        "$aws/things/t/shadow/get",
        "$aws/things/other/shadow/update/delta",
        "$aws/things/t/shadow/name/other/update/delta",
        "$aws/things/t/shadow/update/delta/x",
        "$aws/things/t/shadow/",
        "t/shadow/update/delta",
        "",
    ] {
        assert_eq!(shadow_topic(t), None, "{t:?}");
    }
    assert_eq!(request_topic(0, Op::Get), "$aws/things/t/shadow/get");
    assert_eq!(request_topic(0, Op::Update), "$aws/things/t/shadow/update");
    assert_eq!(request_topic(0, Op::Delete), "$aws/things/t/shadow/delete");
    assert_eq!(request_topic(1, Op::Update), "$aws/things/t/shadow/name/config/update");
    assert_eq!(shadow_index(None), Some(0));
    assert_eq!(shadow_index(Some("config")), Some(1));
    assert_eq!(shadow_index(Some("Config")), None, "byte for byte");
    assert_eq!(shadow_index(Some("")), None);
    assert_eq!((shadow_label(0), shadow_label(1), shadow_label(2)), (None, Some("config"), None));
    assert_eq!(KINDS.iter().filter(|k| k.op().is_none()).count(), 1, "the delta alone answers no request");
}

fn token(nonce: u32, id: u32) -> String {
    let mut out = [0u8; TOKEN_MAX];
    let n = shadow_token(nonce, id, &mut out);
    String::from_utf8(out[..n].to_vec()).unwrap()
}

#[test]
fn the_board_s_token_is_its_nonce_and_a_number() {
    assert_eq!(token(0x1a2b_3c4d, 7), "1a2b3c4d-7");
    assert_eq!(token(1, u32::MAX), "00000001-4294967295");
    assert_eq!(token(1, u32::MAX).len(), TOKEN_MAX);
    assert!(own_token(&token(0xdead_beef, 3), 0xdead_beef));
    assert!(!own_token(&token(0xdead_beef, 3), 0xdead_beee));
    assert!(!own_token("deadbeef", 0xdead_beef), "the nonce alone is no token");
    assert!(!own_token("deadbeef3", 0xdead_beef));
    assert!(!own_token("", 0xdead_beef));
}

fn body(b: Body<'_>) -> Result<String, ShadowError> {
    let mut out = [0u8; 512];
    let n = shadow_body(b, "1a2b3c4d-7", &mut out)?;
    Ok(String::from_utf8(out[..n].to_vec()).unwrap())
}

/// The board's token goes into every request; what is no JSON object, or
/// carries a token of its own, never leaves the board.
#[test]
fn requests_carry_the_board_s_token() {
    assert_eq!(body(Body::Empty).unwrap(), r#"{"clientToken":"1a2b3c4d-7"}"#);
    for empty in ["{}", "{ }", "{\n}", " \t{}\r\n"] {
        assert_eq!(body(Body::Update(empty.as_bytes())).unwrap(), r#"{"clientToken":"1a2b3c4d-7"}"#, "{empty:?}");
    }
    let update = br#" {"state":{"desired":null},"version":3} "#;
    let sent = body(Body::Update(update)).unwrap();
    assert_eq!(sent, r#"{"clientToken":"1a2b3c4d-7","state":{"desired":null},"version":3}"#);
    let parsed: serde_json::Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(parsed["version"], 3);
    let spaced = body(Body::Update(b"{\n  \"state\": {}\n}")).unwrap();
    assert!(serde_json::from_str::<serde_json::Value>(&spaced).is_ok(), "{spaced}");
    assert_eq!(
        body(Body::Report(br#" {"t":21.5,"led":true} "#)).unwrap(),
        r#"{"clientToken":"1a2b3c4d-7","state":{"reported":{"t":21.5,"led":true}}}"#
    );
    // A report's own key may be called clientToken - it sits under reported.
    assert!(body(Body::Report(br#"{"clientToken":"x"}"#)).is_ok());
    for bad in [
        "", "[1]", "1", "\"x\"", "{", "}", "null", r#"{"a":1} x"#, r#"{"a":}"#, r#"{"a":1,}"#, r#"{,}"#,
        r#"{"a" 1}"#, r#"{a:1}"#, r#"{"a":01}"#, r#"{"a":tru}"#, r#"{"a":"\x"}"#, r#"{"a":[1,]}"#, "{} {}",
        r#"{"a":1}}"#,
    ] {
        assert_eq!(body(Body::Update(bad.as_bytes())), Err(ShadowError::BadDocument), "{bad:?}");
        assert_eq!(body(Body::Report(bad.as_bytes())), Err(ShadowError::BadDocument), "{bad:?}");
    }
    // A report cannot reach past `reported`: what is no single object is refused.
    for inject in [r#"{"t":21},"desired":{"led":true}"#, r#"{"t":1}},"clientToken":"evil","x":{"y":1}"#] {
        assert_eq!(body(Body::Report(inject.as_bytes())), Err(ShadowError::BadDocument), "{inject}");
    }
    // Bytes that are no UTF-8, inside a string: refused, never sent to AWS.
    assert_eq!(body(Body::Update(b"{\"state\":{\"reported\":{\"s\":\"\xff\"}}}")), Err(ShadowError::BadDocument));
    let long_token = format!(r#"{{"clientToken":"{}","state":{{}}}}"#, "k".repeat(70));
    for own in [r#"{"clientToken":"mine","state":{}}"#, r#"{"state":{},"clientToken":7}"#, long_token.as_str()] {
        assert_eq!(body(Body::Update(own.as_bytes())), Err(ShadowError::BadDocument), "{own}");
    }
    let big = format!(r#"{{"state":{{"reported":{{"s":"{}"}}}}}}"#, "x".repeat(SHADOW_MAX));
    assert_eq!(body(Body::Update(big.as_bytes())), Err(ShadowError::TooLarge));
    // What it would carry does not fit where it goes: TooLarge, nothing cut.
    let mut small = [0u8; 16];
    assert_eq!(shadow_body(Body::Empty, "1a2b3c4d-7", &mut small), Err(ShadowError::TooLarge));
    // The slot the glue builds in always holds the largest request.
    let mut slot = [0u8; SHADOW_MAX + 64];
    let longest = format!(r#"{{"x":"{}"}}"#, "y".repeat(SHADOW_MAX - 8));
    assert_eq!(longest.len(), SHADOW_MAX);
    assert!(shadow_body(Body::Report(longest.as_bytes()), &token(u32::MAX, u32::MAX), &mut slot).is_ok());
}

/// The pending request's answer: its own op and its token - never one
/// without a token, which answers go to every subscriber with.
#[test]
fn an_answer_is_the_pending_request_s_by_its_token() {
    let p = "1a2b3c4d-7";
    assert!(shadow_answers(Kind::GetAccepted, Some(p), Op::Get, p));
    assert!(shadow_answers(Kind::GetRejected, Some(p), Op::Get, p));
    assert!(!shadow_answers(Kind::GetAccepted, Some("1a2b3c4d-6"), Op::Get, p), "an earlier one");
    assert!(!shadow_answers(Kind::GetAccepted, Some(p), Op::Update, p), "another op");
    assert!(!shadow_answers(Kind::Delta, Some(p), Op::Update, p), "a delta answers nothing");
    assert!(!shadow_answers(Kind::UpdateRejected, None, Op::Update, p), "someone else's unreadable request");
    assert!(shadow_answers(Kind::DeleteAccepted, Some(p), Op::Delete, p));
    // Each answer topic's request and verdict, pinned.
    let ops = KINDS.map(Kind::op);
    assert_eq!(ops, [Some(Op::Get), Some(Op::Get), Some(Op::Update), Some(Op::Update), None, Some(Op::Delete), Some(Op::Delete)]);
    assert_eq!(KINDS.map(Kind::accepted), [true, false, true, false, false, true, false]);
    assert_eq!(rejected_code(br#"{"code":404,"message":"No shadow exists"}"#), 404);
    assert_eq!(rejected_code(br#"{"code":70000}"#), 0);
    assert_eq!(rejected_code(br#"{"message":"x"}"#), 0);
    assert_eq!(rejected_code(b"not json"), 0);
}

#[test]
fn the_guard_drops_duplicates_and_late_ones_and_takes_a_restart() {
    let mut g = Guard::NEW;
    assert!(g.take(5, 100), "anything, before the first");
    assert!(!g.take(5, 100), "a duplicate");
    assert!(!g.take(4, 99), "a late one");
    assert!(g.take(6, 100));
    assert!(g.take(1, 200), "versions start over after a delete: a later timestamp");
    g.forget();
    assert!(g.take(0, 0));
    g.set(10, 10);
    assert!(!g.newer(10, 10) && g.newer(11, 0));
}

/// A random JSON text, valid or broken: values of every kind, nested, with
/// the bytes that break JSON now and then.
fn random_json(rnd: &mut dyn FnMut() -> u64, depth: u32, out: &mut String) {
    const PIECES: [&str; 14] = ["{", "}", "[", "]", ",", ":", "\"", "\\", "1", "-", "e", "tru", " ", "0"];
    if rnd() % 40 == 0 {
        out.push_str(PIECES[(rnd() as usize) % PIECES.len()]);
        return;
    }
    match rnd() % if depth == 0 { 4 } else { 6 } {
        0 => out.push_str(["true", "false", "null"][(rnd() % 3) as usize]),
        1 => out.push_str(["0", "-1", "21.5", "1e3", "-0.5E-2", "12345678901234"][(rnd() % 6) as usize]),
        2 => out.push_str([r#""""#, r#""a\"b""#, r#""\u00e9\n""#, r#""\ud83d\ude00""#, "\"\u{e9}\""][(rnd() % 5) as usize]),
        3 => out.push(' '),
        4 => {
            out.push('[');
            for k in 0..rnd() % 3 {
                if k > 0 {
                    out.push(',');
                }
                random_json(rnd, depth - 1, out);
            }
            out.push(']');
        }
        _ => {
            out.push('{');
            for k in 0..rnd() % 3 {
                if k > 0 {
                    out.push_str(" , ");
                }
                out.push_str(&format!("\"k{k}\" :"));
                random_json(rnd, depth - 1, out);
            }
            out.push('}');
        }
    }
}

/// The board's JSON check says what serde_json says, valid or not - so what
/// it sends AWS can read, and what it refuses AWS would refuse too.
#[test]
fn json_valid_agrees_with_serde_json() {
    let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut valid = 0;
    for i in 0..20_000 {
        let mut text = String::new();
        random_json(&mut rnd, 4, &mut text);
        let ours = json_valid(text.as_bytes());
        let theirs = serde_json::from_str::<serde_json::Value>(&text).is_ok();
        assert_eq!(ours, theirs, "run {i}: {text:?}");
        valid += usize::from(ours);
    }
    assert!(valid > 2_000, "only {valid} valid texts");
    // 64 levels, not 65; and what is no JSON at all.
    assert!(json_valid(format!("{}{}", "[".repeat(64), "]".repeat(64)).as_bytes()));
    assert!(!json_valid(format!("{}{}", "[".repeat(65), "]".repeat(65)).as_bytes()));
    for bad in ["", " ", "[", "]", "[}", "{]", "\"\\ud83d\"", "1 2", "nul", "+1", ".5"] {
        assert!(!json_valid(bad.as_bytes()), "{bad:?}");
    }
}

/// What the board keeps of the deltas, connection by connection.
#[test]
fn the_deltas_kept_and_the_shadows_to_read_again() {
    let mut d = Deltas::NEW;
    assert_eq!(d.next(0b11), None);
    // A new connection: both shadows to be read again - only those ready.
    d.connected(0b11);
    assert_eq!(d.next(0b01), Some(Next::Resync(0)));
    assert_eq!(d.next(0b01), None, "shadow 1 is not ready: not yet");
    assert_eq!(d.next(0b11), Some(Next::Resync(1)));
    // A delta, then its duplicate: one.
    assert!(d.delta(0, Some(3), Some(100), false, 40));
    assert!(!d.delta(0, Some(3), Some(100), false, 40));
    assert_eq!(d.next(0b11), Some(Next::Delta(Unread { shadow: 0, version: Some(3), timestamp: Some(100), own: false, len: 40 })));
    assert_eq!(d.next(0b11), None);
    // An unread delta replaced by another shadow's: that shadow is read again.
    assert!(d.delta(0, Some(4), Some(101), false, 10));
    assert!(d.delta(1, Some(9), Some(101), true, 12));
    assert!(matches!(d.next(0b11), Some(Next::Delta(Unread { shadow: 1, own: true, .. }))));
    assert_eq!(d.next(0b11), Some(Next::Resync(0)));
    // The same shadow's newer delta replaces its own unread one: no resync.
    assert!(d.delta(0, Some(5), Some(102), false, 1));
    assert!(d.delta(0, Some(6), Some(103), false, 2));
    assert!(matches!(d.next(0b11), Some(Next::Delta(Unread { version: Some(6), .. }))));
    assert_eq!(d.next(0b11), None);
    // A delta without a version is kept: nothing to tell it by.
    assert!(d.delta(0, None, None, false, 3));
    assert!(d.next(0b11).is_some());
}

/// The get that answers a resync settles its shadow: its version is the
/// newest, a delta not newer than it is dropped, the shadow needs no reading
/// again. Any other get - another task's - leaves all of that be.
#[test]
fn the_board_s_own_get_settles_its_shadow() {
    let mut d = Deltas::NEW;
    d.connected(0b01);
    assert_eq!(d.next(0b01), Some(Next::Resync(0)));
    assert!(d.delta(0, Some(7), Some(100), false, 5));
    d.own_get(0, Some(7), Some(100));
    assert_eq!(d.next(0b01), None, "the get had it all, and the resync is done");
    // A get nobody asked for through shadow_delta(): it settles nothing.
    let mut other = Deltas::NEW;
    other.connected(0b01);
    other.own_get(0, Some(9), Some(200));
    assert!(other.delta(0, Some(8), Some(150), false, 5), "the guard was not set by it");
    other.own_get(0, Some(8), Some(150));
    assert!(matches!(other.next(0b01), Some(Next::Delta(_))), "an unread delta stays");
    assert_eq!(other.next(0b01), Some(Next::Resync(0)), "and the reader still gets its resync");
    // Back on the first: a delta older than its get is dropped as it comes.
    assert!(!d.delta(0, Some(6), Some(99), false, 5));
    // The next resync's get, a newer delta unread when its answer comes: kept.
    d.connected(0b01);
    assert_eq!(d.next(0b01), Some(Next::Resync(0)));
    assert!(d.delta(0, Some(9), Some(120), false, 5));
    d.own_get(0, Some(8), Some(110));
    assert!(matches!(d.next(0b01), Some(Next::Delta(Unread { version: Some(9), .. }))));
}

#[test]
fn a_delete_or_a_404_lets_any_version_come_next() {
    let mut d = Deltas::NEW;
    assert!(d.delta(0, Some(50), Some(500), false, 5));
    assert!(d.delta(1, Some(3), Some(500), false, 5));
    d.deleted(0);
    assert!(matches!(d.next(0b11), Some(Next::Delta(Unread { shadow: 1, .. }))), "another shadow's delta stays");
    assert_eq!(d.next(0b11), Some(Next::Resync(0)), "the replaced one is read again");
    assert!(d.delta(0, Some(50), Some(500), false, 5));
    d.deleted(0);
    assert_eq!(d.next(0b11), None, "the deleted shadow's delta is gone");
    assert!(d.delta(0, Some(1), Some(1), false, 5), "a shadow made anew starts over");
    let _ = d.next(0b1);
    d.not_found(0);
    assert!(d.delta(0, Some(0), Some(0), false, 5));
    // A new connection forgets what the last one saw.
    d.reset();
    assert_eq!(d, Deltas::NEW);
}

impl PartialEq for Deltas {
    fn eq(&self, other: &Self) -> bool {
        self.guard == other.guard && self.resync == other.resync && self.unread == other.unread
    }
}

impl core::fmt::Debug for Deltas {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Deltas {{ guard: {:?}, resync: {:#b}, unread: {:?} }}", self.guard, self.resync, self.unread)
    }
}
