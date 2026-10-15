
// ── Device Shadow: what the glue above decides ────────────────────────────
// No I/O here: which subscribed topic a message came on, the request topics,
// the clientToken, the request documents, which answer is the pending
// request's, and the deltas kept - the version guard, the shadows to read
// again. The IDE runs this very text through its own tests
// (`mqtt_shadow_core_tests`).
//
// The block names the shadows: SHADOW_THING, SHADOW_NAMES, SHADOW_MAX and
// SHADOW_SUBSCRIBE - 7 answer topics a shadow, in KINDS's order, the classic
// shadow first.

/// Why a shadow call failed. `Ok(r)` with `r.code != 200` is AWS's own
/// refusal instead: 400 a bad document, 401/403 the policy, 404 no shadow
/// yet (the first `shadow_report()` makes it), 409 another version, 413 over
/// 8 KB, 429 too many at once.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShadowError {
    /// The Device Shadow is off in the IoT tab.
    Off,
    /// A name that is not one of SHADOW_NAMES: nothing listens for its
    /// answers.
    UnknownShadow,
    /// Not a JSON object, or one that carries its own `clientToken` - the
    /// board puts its own in, to know its answer.
    BadDocument,
    /// The document is over SHADOW_MAX; or the answer was over what the
    /// board takes and the connection ended over it - the same call then
    /// fails at once for HOLD_S. Raise "Document up to" in the tab, or keep
    /// the shadow smaller (named shadows split it).
    TooLarge,
    /// SHADOW_QUEUE calls already wait their turn, or none came within
    /// SHADOW_WAIT_S.
    Busy,
    /// The shadow's subscriptions were not up within SHADOW_WAIT_S: no
    /// connection to AWS IoT.
    NotConnected,
    /// AWS refused the shadow's subscriptions on this connection, or ended
    /// the connection with 0x87 right after the request, nothing else of the
    /// board's sent since - most likely the policy, which needs iot:Publish,
    /// iot:Subscribe and iot:Receive on $aws/things/<thing>/shadow/*. The
    /// same call then fails at once for HOLD_S.
    Refused,
    /// No answer within SHADOW_WAIT_S. An update, a report or a delete MAY
    /// have been applied.
    Timeout,
    /// The connection was lost before the answer came.
    Disconnected,
}

/// AWS's answer to a shadow call: its document in the caller's buffer.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowResponse {
    /// 200: accepted. Otherwise the `code` of AWS's refusal (0 if it had
    /// none).
    pub code: u16,
    /// The bytes of the answer in the buffer.
    pub len: usize,
    /// The answer was longer than the buffer or SHADOW_MAX: the rest is gone.
    pub truncated: bool,
}

/// What `shadow_delta()` hands over: a delta, or a shadow to read again.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShadowDelta {
    /// `None` = the classic shadow, `Some(name)` a named one.
    pub shadow: Option<&'static str>,
    /// Read this shadow again with `shadow_get()`: deltas may have been
    /// missed - after every (re)connect, or when one was overwritten before
    /// it was read. `len` is 0 then.
    pub resync: bool,
    /// The delta's version.
    pub version: Option<i64>,
    /// The delta came from this board's own `shadow_report()` or
    /// `shadow_update()` here - a key it reported that desired still asks
    /// otherwise; never report such a key again in a loop. (A report through
    /// `http::shadow_*` carries no token of this board's: `false` then.)
    pub own: bool,
    /// The bytes of the delta in the buffer.
    pub len: usize,
    /// It was longer than the buffer or SHADOW_MAX: the rest is gone.
    pub truncated: bool,
}

/// A shadow request.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Get = 0,
    Update = 1,
    Delete = 2,
}

/// An answer topic.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    GetAccepted,
    GetRejected,
    UpdateAccepted,
    UpdateRejected,
    Delta,
    DeleteAccepted,
    DeleteRejected,
}

/// The 7 answer topics of a shadow, in SHADOW_SUBSCRIBE's order.
#[allow(dead_code)]
const KINDS: [Kind; 7] = [
    Kind::GetAccepted,
    Kind::GetRejected,
    Kind::UpdateAccepted,
    Kind::UpdateRejected,
    Kind::Delta,
    Kind::DeleteAccepted,
    Kind::DeleteRejected,
];

#[allow(dead_code)]
impl Kind {
    /// The request it answers; a delta answers none.
    const fn op(self) -> Option<Op> {
        match self {
            Kind::GetAccepted | Kind::GetRejected => Some(Op::Get),
            Kind::UpdateAccepted | Kind::UpdateRejected => Some(Op::Update),
            Kind::DeleteAccepted | Kind::DeleteRejected => Some(Op::Delete),
            Kind::Delta => None,
        }
    }

    const fn accepted(self) -> bool {
        matches!(self, Kind::GetAccepted | Kind::UpdateAccepted | Kind::DeleteAccepted)
    }
}

/// The classic shadow and the named ones.
#[allow(dead_code)]
const SHADOWS: usize = 1 + SHADOW_NAMES.len();
/// The board's clientToken: 8 hex digits, `-`, a decimal number.
#[allow(dead_code)]
const TOKEN_MAX: usize = 19;

const _: () = assert!(SHADOWS <= 8, "at most 7 named shadows");
const _: () = assert!(
    if SHADOW_MAX == 0 {
        SHADOW_SUBSCRIBE.is_empty() && SHADOW_THING.is_empty()
    } else {
        SHADOW_SUBSCRIBE.len() == 7 * SHADOWS && !SHADOW_THING.is_empty()
    },
    "the generated block's shadow constants do not agree"
);

/// The shadow `name` is - 0 the classic one - or `None` for one the block
/// does not name.
#[allow(dead_code)]
fn shadow_index(name: Option<&str>) -> Option<usize> {
    match name {
        None => Some(0),
        Some(n) => SHADOW_NAMES.iter().position(|s| *s == n).map(|i| i + 1),
    }
}

/// The name of shadow `i`; `None` for the classic one.
#[allow(dead_code)]
fn shadow_label(i: usize) -> Option<&'static str> {
    i.checked_sub(1).and_then(|n| SHADOW_NAMES.get(n).copied())
}

/// The shadow and the answer a topic is, when it is one SHADOW_SUBSCRIBE
/// names - exactly: update/documents, another thing, another name are none.
#[allow(dead_code)]
fn shadow_topic(topic: &str) -> Option<(usize, Kind)> {
    if !topic.starts_with("$aws/") {
        return None;
    }
    let i = SHADOW_SUBSCRIBE.iter().position(|t| *t == topic)?;
    Some((i / 7, KINDS[i % 7]))
}

/// The topic a request of shadow `shadow` goes to: its accepted topic
/// without `/accepted`.
#[allow(dead_code)]
fn request_topic(shadow: usize, op: Op) -> &'static str {
    let accepted = SHADOW_SUBSCRIBE[7 * shadow + [0, 2, 5][op as usize]];
    &accepted[..accepted.len() - "/accepted".len()]
}

/// The clientToken of request `id`: `<nonce, 8 hex digits>-<id>`, into
/// `out`; its length back.
#[allow(dead_code)]
fn shadow_token(nonce: u32, id: u32, out: &mut [u8; TOKEN_MAX]) -> usize {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, slot) in out.iter_mut().take(8).enumerate() {
        *slot = HEX[((nonce >> (28 - 4 * i)) & 15) as usize];
    }
    out[8] = b'-';
    let mut digits = [0u8; 10];
    let mut n = id;
    let mut k = digits.len();
    loop {
        k -= 1;
        digits[k] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    let len = 9 + digits.len() - k;
    out[9..len].copy_from_slice(&digits[k..]);
    len
}

/// A clientToken this boot's requests carry: its nonce, then `-`.
#[allow(dead_code)]
fn own_token(token: &str, nonce: u32) -> bool {
    let mut mine = [0u8; TOKEN_MAX];
    shadow_token(nonce, 0, &mut mine);
    token.len() > 9 && token.as_bytes()[..9] == mine[..9]
}

/// What a request carries.
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Body<'a> {
    /// get, delete: the clientToken alone.
    Empty,
    /// update: the caller's request document, a JSON object.
    Update(&'a [u8]),
    /// report: the caller's JSON object, as `state.reported`.
    Report(&'a [u8]),
}

/// `d` without the JSON whitespace around it.
#[allow(dead_code)]
fn json_trim(mut d: &[u8]) -> &[u8] {
    while let [b' ' | b'\t' | b'\n' | b'\r', rest @ ..] = d {
        d = rest;
    }
    while let [rest @ .., b' ' | b'\t' | b'\n' | b'\r'] = d {
        d = rest;
    }
    d
}

/// What the request document must be before anything is sent: a JSON object
/// - all of it, by RFC 8259, so AWS can read the board's token in it and a
/// report cannot reach past `reported` - no larger than SHADOW_MAX, and an
/// update's without a clientToken of its own.
#[allow(dead_code)]
fn shadow_body_check(body: &Body<'_>) -> Result<(), ShadowError> {
    let (d, update) = match body {
        Body::Empty => return Ok(()),
        Body::Update(d) => (*d, true),
        Body::Report(d) => (*d, false),
    };
    if d.len() > SHADOW_MAX {
        return Err(ShadowError::TooLarge);
    }
    let t = json_trim(d);
    if t.first() != Some(&b'{') || !json_valid(t) {
        return Err(ShadowError::BadDocument);
    }
    if update && json_get(t, &["clientToken"]).is_some() {
        return Err(ShadowError::BadDocument);
    }
    Ok(())
}

/// `d` is one JSON value and nothing else, by RFC 8259: UTF-8, strings and
/// their escapes, numbers, `true`/`false`/`null`, at most 64 levels of
/// objects and arrays - walked with a bit a level, no recursion.
#[allow(dead_code)]
fn json_valid(d: &[u8]) -> bool {
    if core::str::from_utf8(d).is_err() {
        return false;
    }
    // A key's string, then `:`: one past the colon.
    let key = |i: usize| -> Option<usize> {
        if d.get(i) != Some(&b'"') {
            return None;
        }
        let end = json_string_end(d, i)?;
        if !json_unescape(&d[i + 1..end - 1], |_| true) {
            return None;
        }
        let colon = json_ws(d, end);
        (d.get(colon) == Some(&b':')).then_some(colon + 1)
    };
    // One bit a level: 1 = an object, 0 = an array.
    let mut objects = 0u64;
    let mut depth = 0u32;
    let mut i = 0;
    loop {
        // A value.
        i = json_ws(d, i);
        match d.get(i) {
            Some(&open @ (b'{' | b'[')) => {
                if depth == 64 {
                    return false;
                }
                objects = (objects << 1) | u64::from(open == b'{');
                depth += 1;
                i = json_ws(d, i + 1);
                if d.get(i) == Some(&if open == b'{' { b'}' } else { b']' }) {
                    i += 1;
                    objects >>= 1;
                    depth -= 1;
                } else if open == b'{' {
                    match key(i) {
                        Some(next) => i = next,
                        None => return false,
                    }
                    continue;
                } else {
                    continue;
                }
            }
            Some(b'"') => match json_string_end(d, i) {
                Some(end) if json_unescape(&d[i + 1..end - 1], |_| true) => i = end,
                _ => return false,
            },
            Some(_) => {
                let Some(end) = json_end(d, i) else {
                    return false;
                };
                let word = &d[i..end];
                if !(word == b"true" || word == b"false" || word == b"null" || json_number(word)) {
                    return false;
                }
                i = end;
            }
            None => return false,
        }
        // After a value: a comma, a closing bracket, or the end of it all.
        loop {
            i = json_ws(d, i);
            if depth == 0 {
                return i == d.len();
            }
            let object = objects & 1 == 1;
            match d.get(i) {
                Some(b',') => {
                    i = json_ws(d, i + 1);
                    if object {
                        match key(i) {
                            Some(next) => i = next,
                            None => return false,
                        }
                    }
                    break;
                }
                Some(b'}') if object => {}
                Some(b']') if !object => {}
                _ => return false,
            }
            i += 1;
            objects >>= 1;
            depth -= 1;
        }
    }
}

/// The request's payload with the board's clientToken `token` in it, into
/// `out`; its length back. `TooLarge` when `out` cannot hold it.
#[allow(dead_code)]
fn shadow_body(body: Body<'_>, token: &str, out: &mut [u8]) -> Result<usize, ShadowError> {
    shadow_body_check(&body)?;
    let mut n = 0;
    let mut put = |bytes: &[u8]| -> Result<(), ShadowError> {
        let end = n + bytes.len();
        out.get_mut(n..end)
            .ok_or(ShadowError::TooLarge)?
            .copy_from_slice(bytes);
        n = end;
        Ok(())
    };
    put(br#"{"clientToken":""#)?;
    put(token.as_bytes())?;
    match body {
        Body::Empty => put(br#""}"#)?,
        Body::Update(d) => {
            // The object's own members after the board's token.
            let inner = json_trim(&json_trim(d)[1..]);
            if inner == b"}" {
                put(br#""}"#)?;
            } else {
                put(br#"","#)?;
                put(inner)?;
            }
        }
        Body::Report(r) => {
            put(br#"","state":{"reported":"#)?;
            put(json_trim(r))?;
            put(b"}}")?;
        }
    }
    Ok(n)
}

/// An answer of kind `kind` is the pending `op`'s: the same request (the
/// caller checks the shadow) and the board's token `pending`. Never one
/// without a token: answers go to every subscriber, and a refusal without
/// one is of a request AWS could not read - never the board's, which sends
/// valid JSON only (`shadow_body_check`).
#[allow(dead_code)]
fn shadow_answers(kind: Kind, token: Option<&str>, op: Op, pending: &str) -> bool {
    kind.op() == Some(op) && token == Some(pending)
}

/// A refusal's `code`; 0 when it has none that fits.
#[allow(dead_code)]
fn rejected_code(payload: &[u8]) -> u16 {
    json_i64(payload, &["code"])
        .and_then(|c| u16::try_from(c).ok())
        .unwrap_or(0)
}

/// The newest (version, timestamp) of a shadow the board has seen: what is
/// not newer is a duplicate, or late. Versions start over 48 hours after a
/// delete, so a later timestamp counts too.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Guard(Option<(i64, i64)>);

#[allow(dead_code)]
impl Guard {
    const NEW: Guard = Guard(None);

    fn newer(&self, v: i64, t: i64) -> bool {
        match self.0 {
            None => true,
            Some((gv, gt)) => v > gv || t > gt,
        }
    }

    /// Takes (v, t) when it is newer; whether it was.
    fn take(&mut self, v: i64, t: i64) -> bool {
        let newer = self.newer(v, t);
        if newer {
            self.0 = Some((v, t));
        }
        newer
    }

    fn set(&mut self, v: i64, t: i64) {
        self.0 = Some((v, t));
    }

    fn forget(&mut self) {
        self.0 = None;
    }
}

/// The delta kept for `shadow_delta()`, its bytes beside it.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Unread {
    shadow: usize,
    version: Option<i64>,
    timestamp: Option<i64>,
    own: bool,
    /// Its whole length, kept or not.
    len: usize,
}

/// What `shadow_delta()` hands over next.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Next {
    Delta(Unread),
    Resync(usize),
}

/// The deltas: each shadow's guard, the shadows to read again (a bit each),
/// the one unread delta - the latest, of any shadow - and the shadows
/// `shadow_delta()` handed out to be read again, whose get settles them.
#[allow(dead_code)]
struct Deltas {
    guard: [Guard; SHADOWS],
    resync: u8,
    unread: Option<Unread>,
    asked: u8,
}

#[allow(dead_code)]
impl Deltas {
    const NEW: Deltas = Deltas {
        guard: [Guard::NEW; SHADOWS],
        resync: 0,
        unread: None,
        asked: 0,
    };

    /// A new connection: nothing seen yet on it.
    fn reset(&mut self) {
        *self = Deltas::NEW;
    }

    /// The shadows in `ready` are subscribed again: each to be read again,
    /// since deltas sent while the board was away are gone.
    fn connected(&mut self, ready: u8) {
        self.resync |= ready;
    }

    /// A delta of shadow `s` came: whether to keep it - then its bytes go
    /// beside it. A duplicate or a late one is dropped; one that replaces
    /// another shadow's unread delta leaves that shadow to be read again.
    fn delta(&mut self, s: usize, version: Option<i64>, timestamp: Option<i64>, own: bool, len: usize) -> bool {
        if let Some(v) = version {
            if !self.guard[s].take(v, timestamp.unwrap_or(0)) {
                return false;
            }
        }
        if let Some(u) = self.unread {
            if u.shadow != s {
                self.resync |= 1 << u.shadow;
            }
        }
        self.unread = Some(Unread { shadow: s, version, timestamp, own, len });
        true
    }

    /// The board's own get of shadow `s` was accepted. When it answers a
    /// resync `shadow_delta()` handed out, its version is the newest, an
    /// unread delta not newer than it is dropped, and the shadow needs no
    /// reading again. Any other get - another task's, another client's -
    /// leaves them be: the reader has not seen what it brought.
    fn own_get(&mut self, s: usize, version: Option<i64>, timestamp: Option<i64>) {
        if self.asked & (1 << s) == 0 {
            return;
        }
        self.asked &= !(1 << s);
        self.resync &= !(1 << s);
        let Some(v) = version else {
            return;
        };
        let t = timestamp.unwrap_or(0);
        self.guard[s].set(v, t);
        let stale = self
            .unread
            .is_some_and(|u| u.shadow == s && !u.version.is_some_and(|uv| uv > v || u.timestamp.unwrap_or(0) > t));
        if stale {
            self.unread = None;
        }
    }

    /// Shadow `s` is not there (a get's 404): any version may come next.
    fn not_found(&mut self, s: usize) {
        self.guard[s].forget();
        self.asked &= !(1 << s);
    }

    /// Shadow `s` was deleted, by anyone: any version may come next, and its
    /// unread delta is of a shadow that is gone.
    fn deleted(&mut self, s: usize) {
        self.guard[s].forget();
        self.asked &= !(1 << s);
        if self.unread.is_some_and(|u| u.shadow == s) {
            self.unread = None;
        }
    }

    /// What to hand over next: the unread delta, else the lowest shadow in
    /// `ready` to read again.
    fn next(&mut self, ready: u8) -> Option<Next> {
        if let Some(u) = self.unread.take() {
            return Some(Next::Delta(u));
        }
        let due = self.resync & ready;
        if due == 0 {
            return None;
        }
        let s = due.trailing_zeros() as usize;
        self.resync &= !(1 << s);
        self.asked |= 1 << s;
        Some(Next::Resync(s))
    }
}
