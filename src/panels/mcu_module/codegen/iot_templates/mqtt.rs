
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::mqtt::publish("my/topic", b"21.5").await.ok();
//     let msg = pins::configs::mqtt::incoming().await;   // msg.topic, msg.payload
//
// One task owns the connection: it reconnects on its own, and `publish` only
// queues the message for it - and waits while the queue is full, which it
// stays while the broker is away. A loop that must not stop for that calls
// `try_publish`, which never waits; `connected()` says whether it is up. How it reaches the broker - plain TCP, or TLS
// with the broker's certificate checked - is mqtt_link.rs, which the IoT
// tab swaps; this file stays the same either way.
//
// What the broker refused last - a login, a policy (AWS IoT), a subscription -
// is `last_refusal()`, for your own log.
//
// AWS IoT's Device Shadow (IoT tab: MQTT > Device Shadow) comes over the same
// connection: a delta is pushed the moment `desired` changes. One task reads
// them:
//
//     use pins::configs::mqtt;
//     let mut doc = [0u8; 1024];
//     loop {
//         let Ok(d) = mqtt::shadow_delta(&mut doc).await else { break };
//         if d.resync {
//             // After every (re)connect: deltas sent meanwhile are gone.
//             if let Ok(r) = mqtt::shadow_get(d.shadow, &mut doc).await {
//                 let led = mqtt::json_bool(&doc[..r.len], &["state", "delta", "led"]);
//             }
//         } else {
//             let led = mqtt::json_bool(&doc[..d.len], &["state", "led"]);
//             // ... apply it, then say so - only the keys that changed:
//             mqtt::shadow_report(d.shadow, br#"{"led":true}"#, &mut doc).await.ok();
//         }
//     }
//
// A get's answer carries a timestamp for every key - about three times the
// state: keep the state under a third of SHADOW_MAX. Never report again a
// key the board cannot take (`d.own`: a delta its own report here caused):
// desired and reported would go back and forth.

use core::cell::{Cell, RefCell};
use core::num::NonZero;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either, Either4, select, select4};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_sync::mutex::Mutex as AsyncMutex;
use embassy_sync::semaphore::{FairSemaphore, Semaphore};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, Instant, Timer, with_deadline, with_timeout};
use static_cell::ConstStaticCell;
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::event::Event;
use rust_mqtt::client::options::{
    ConnectOptions, PublicationOptions, SubscriptionOptions, TopicReference,
};
use rust_mqtt::client::{Client, MqttError};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::types::{MqttBinary, MqttString, ReasonCode, TopicFilter, TopicName};

use super::mqtt_link::{self as link, Link, LinkBuffers};
use super::secrets::{MQTT_PASSWORD, MQTT_USERNAME};

/// Longest topic a `Message` carries.
pub const MAX_TOPIC: usize = 64;
/// Largest payload a `Message` carries.
pub const MAX_PAYLOAD: usize = 256;
/// Seconds to wait before connecting again after the broker is lost.
pub const RETRY_S: u64 = 5;
/// Seconds the TLS handshake may take (an RP2040 checks RSA in software),
/// and an answer to the CONNECT or a SUBSCRIBE: a broker that took the TCP
/// connection and then says nothing must not hold the session for ever.
const HANDSHAKE_S: u64 = 60;
const ANSWER_S: u64 = 30;
/// Seconds a whole shadow call may take: its turn, the shadow's
/// subscriptions, the answer.
const SHADOW_WAIT_S: u64 = 10;
/// Seconds after the last shadow SUBACK before a shadow is called ready: AWS
/// starts delivering to a new subscription a little after its SUBACK.
const SETTLE_S: u64 = 2;
/// Seconds a request that ended the connection - an answer too large, a
/// policy that refuses it - then fails at once instead of ending the next.
const HOLD_S: u64 = 600;
/// Shadow calls that may wait for their turn; one more gets `Busy`.
const SHADOW_QUEUE: usize = 4;
/// The largest MQTT packet the board takes - and tells the broker: a shadow
/// document, the longest named topic, MQTT 5's framing. Never under 1024.
const WORK: usize = if SHADOW_MAX + 320 > 1024 { SHADOW_MAX + 320 } else { 1024 };
/// The socket's receive buffer: a TLS record of the largest packet held
/// whole (the link's Gate needs it), when the shadow is on.
const RX: usize = if SHADOW_MAX > 0 && WORK + 64 > link::TCP_RX { WORK + 64 } else { link::TCP_RX };
/// A shadow request, then its answer: the document and the clientToken.
const SLOT: usize = if SHADOW_MAX == 0 { 0 } else { SHADOW_MAX + 64 };

/// One publication, either way.
pub struct Message {
    pub topic: heapless::String<MAX_TOPIC>,
    pub payload: heapless::Vec<u8, MAX_PAYLOAD>,
}

/// Why `publish` refused a message.
#[derive(Debug)]
pub enum PublishError {
    TopicTooLong,
    PayloadTooLarge,
    /// Empty, or with a `+` or `#` in it: a topic NAME holds no wildcard,
    /// and the broker would end the connection over it.
    InvalidTopic,
    /// `try_publish` only: the queue is full - the broker away, or messages
    /// coming faster than it takes them.
    QueueFull,
}

/// What the broker refused, with MQTT 5's reason code (0x87 = not
/// authorized, 0x86 = bad user name or password).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The CONNECT: the login, the client id, or a policy that does not
    /// allow it (AWS IoT: iot:Connect for this client id).
    Connect(u8),
    /// A subscription in SUBSCRIBE: nothing arrives on it.
    Subscribe(&'static str, u8),
    /// The broker ended the connection: 0x87 after a publication its policy
    /// refuses, 0x8E when another client took over this client id.
    Disconnect(u8),
}

static OUTGOING: Channel<CriticalSectionRawMutex, Message, 4> = Channel::new();
static INCOMING: Channel<CriticalSectionRawMutex, Message, 4> = Channel::new();
static REFUSAL: Mutex<CriticalSectionRawMutex, Cell<Option<Refusal>>> = Mutex::new(Cell::new(None));
static CONNECTED: AtomicBool = AtomicBool::new(false);

/// Queues `payload` for `topic` (QoS 0). Waits while the queue is full - for
/// as long as the broker is away: `try_publish` does not.
pub async fn publish(topic: &str, payload: &[u8]) -> Result<(), PublishError> {
    OUTGOING.send(message(topic, payload)?).await;
    Ok(())
}

/// Queues `payload` for `topic` (QoS 0) without waiting: `QueueFull` while
/// the queue is - the message is dropped, as QoS 0 allows.
pub fn try_publish(topic: &str, payload: &[u8]) -> Result<(), PublishError> {
    OUTGOING
        .try_send(message(topic, payload)?)
        .map_err(|_| PublishError::QueueFull)
}

/// The broker took the CONNECT, and the connection still stands.
pub fn connected() -> bool {
    CONNECTED.load(Ordering::Relaxed)
}

fn message(topic: &str, payload: &[u8]) -> Result<Message, PublishError> {
    if topic.is_empty() || topic.contains(['+', '#']) {
        return Err(PublishError::InvalidTopic);
    }
    Ok(Message {
        topic: heapless::String::try_from(topic).map_err(|_| PublishError::TopicTooLong)?,
        payload: heapless::Vec::from_slice(payload).map_err(|_| PublishError::PayloadTooLarge)?,
    })
}

/// The next publication on a subscribed topic.
pub async fn incoming() -> Message {
    INCOMING.receive().await
}

/// What the broker refused last; `None` until it refuses something. The
/// board only reconnects - this says why.
pub fn last_refusal() -> Option<Refusal> {
    REFUSAL.lock(Cell::get)
}

/// Reads the thing's shadow `shadow` (`None` = the classic one, `Some(name)`
/// one of SHADOW_NAMES): AWS's answer in `doc` - `state.desired`,
/// `state.reported`, `state.delta`, `metadata`, `version` - read with
/// `json_bool`, `json_f64`, `json_i64`, `json_str`, `json_get`. `r.code`
/// 200, or AWS's refusal: 404 = no shadow yet. See `ShadowError` for the
/// rest.
pub async fn shadow_get(shadow: Option<&str>, doc: &mut [u8]) -> Result<ShadowResponse, ShadowError> {
    shadow_call(shadow, Op::Get, Body::Empty, doc).await
}

/// Updates the shadow with `document` - AWS's request document, a JSON
/// object whose `state` holds `desired` and/or `reported`, maybe a
/// `version` (409 when the shadow is at another); no `clientToken`, the
/// board puts its own in. Only the keys it names change; one set to `null`
/// goes. AWS's answer in `doc`: what it took, and the new `version`.
pub async fn shadow_update(
    shadow: Option<&str>,
    document: &[u8],
    doc: &mut [u8],
) -> Result<ShadowResponse, ShadowError> {
    shadow_call(shadow, Op::Update, Body::Update(document), doc).await
}

/// `shadow_update()` with `{"state":{"reported":<reported>}}` - the board
/// saying what it is; `reported` is a JSON object:
///
///     mqtt::shadow_report(None, br#"{"t":21.5,"led":true}"#, &mut doc).await
pub async fn shadow_report(
    shadow: Option<&str>,
    reported: &[u8],
    doc: &mut [u8],
) -> Result<ShadowResponse, ShadowError> {
    shadow_call(shadow, Op::Update, Body::Report(reported), doc).await
}

/// Deletes the shadow. 404: there was none.
pub async fn shadow_delete(shadow: Option<&str>, doc: &mut [u8]) -> Result<ShadowResponse, ShadowError> {
    shadow_call(shadow, Op::Delete, Body::Empty, doc).await
}

/// The next delta - what `desired` asks that `reported` does not show yet,
/// pushed by AWS - into `doc`, read with `json_*` under `["state", ...]`;
/// or a shadow to read again (`resync`): after every (re)connect, and when
/// a delta was overwritten before it was read. Duplicates and late ones are
/// dropped. One task only should wait here.
pub async fn shadow_delta(doc: &mut [u8]) -> Result<ShadowDelta, ShadowError> {
    if SHADOW_MAX == 0 {
        return Err(ShadowError::Off);
    }
    loop {
        let ready = READY.load(Ordering::Relaxed);
        let next = DELTAS.lock(|d| {
            let mut d = d.borrow_mut();
            let next = d.deltas.next(ready)?;
            Some(match next {
                Next::Delta(u) => {
                    let n = u.len.min(SHADOW_MAX).min(doc.len());
                    doc[..n].copy_from_slice(&d.buf[..n]);
                    ShadowDelta {
                        shadow: shadow_label(u.shadow),
                        resync: false,
                        version: u.version,
                        own: u.own,
                        len: n,
                        truncated: n < u.len,
                    }
                }
                Next::Resync(s) => ShadowDelta {
                    shadow: shadow_label(s),
                    resync: true,
                    version: None,
                    own: false,
                    len: 0,
                    truncated: false,
                },
            })
        });
        if let Some(d) = next {
            return Ok(d);
        }
        DELTA_NEW.wait().await;
    }
}

/// Starts the task that owns the connection.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(mqtt_task(stack).unwrap());
}

/// The link's own buffers (TLS records; nothing for plain TCP), in a static:
/// built in place, never on the stack.
static LINK: ConstStaticCell<LinkBuffers> = ConstStaticCell::new(LinkBuffers::new());

/// The socket's and the client's buffers, in a static too.
struct Buffers {
    rx: [u8; RX],
    tx: [u8; 1024],
    work: [u8; WORK],
}

static BUFFERS: ConstStaticCell<Buffers> = ConstStaticCell::new(Buffers {
    rx: [0; RX],
    tx: [0; 1024],
    work: [0; WORK],
});

/// Where the one shadow request stands.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    /// Built by its caller, for the task to send.
    Queued(u32),
    /// Sent; waiting for AWS's answer.
    Sent(u32),
    /// Answered, or failed: `result`.
    Done(u32),
}

/// The shadow request and its answer: built and read by the caller, sent
/// and filled by the task. In a static, never on a stack.
struct Exchange {
    id: u32,
    state: State,
    shadow: usize,
    op: Op,
    token: [u8; TOKEN_MAX],
    token_len: usize,
    /// The request's length, then the answer's whole length.
    len: usize,
    result: Result<u16, ShadowError>,
    /// Requests that ended the connection, each failing at once until then.
    hold: [[Option<(Instant, ShadowError)>; 3]; SHADOWS],
    buf: [u8; SLOT],
}

impl Exchange {
    const fn new() -> Self {
        Self {
            id: 0,
            state: State::Idle,
            shadow: 0,
            op: Op::Get,
            token: [0; TOKEN_MAX],
            token_len: 0,
            len: 0,
            result: Err(ShadowError::Disconnected),
            hold: [[None; 3]; SHADOWS],
            buf: [0; SLOT],
        }
    }

    fn held(&self, s: usize, op: Op) -> Option<ShadowError> {
        match self.hold[s][op as usize] {
            Some((until, e)) if Instant::now() < until => Some(e),
            _ => None,
        }
    }

    fn token(&self) -> &str {
        core::str::from_utf8(&self.token[..self.token_len]).unwrap_or("")
    }
}

/// The deltas and the one unread delta's bytes.
struct Shadows {
    deltas: Deltas,
    buf: [u8; SHADOW_MAX],
}

/// Why a connection ended, for the request in flight.
#[derive(Clone, Copy, PartialEq, Eq)]
enum End {
    Lost,
    /// A packet over WORK came: the answer, likely.
    TooLarge,
    /// The broker ended the connection with 0x87 right after the request,
    /// nothing of the user's sent since: the policy.
    Refused,
}

static EXCHANGE: AsyncMutex<CriticalSectionRawMutex, Exchange> = AsyncMutex::new(Exchange::new());
static DELTAS: Mutex<CriticalSectionRawMutex, RefCell<Shadows>> = Mutex::new(RefCell::new(Shadows {
    deltas: Deltas::NEW,
    buf: [0; SHADOW_MAX],
}));
/// The shadows whose answers are subscribed on this connection, a bit each;
/// and those the broker refused.
static READY: AtomicU8 = AtomicU8::new(0);
static REFUSED: AtomicU8 = AtomicU8::new(0);
/// This boot's clientToken prefix; 0 until the first request.
static NONCE: AtomicU32 = AtomicU32::new(0);
static TURN: FairSemaphore<CriticalSectionRawMutex, SHADOW_QUEUE> = FairSemaphore::new(1);
static START: Signal<CriticalSectionRawMutex, u32> = Signal::new();
static DONE: Signal<CriticalSectionRawMutex, u32> = Signal::new();
static READY_EDGE: Signal<CriticalSectionRawMutex, ()> = Signal::new();
static DELTA_NEW: Signal<CriticalSectionRawMutex, ()> = Signal::new();

/// One shadow call: one at a time, the whole of it within SHADOW_WAIT_S.
async fn shadow_call(
    shadow: Option<&str>,
    op: Op,
    body: Body<'_>,
    doc: &mut [u8],
) -> Result<ShadowResponse, ShadowError> {
    if SHADOW_MAX == 0 {
        return Err(ShadowError::Off);
    }
    let s = shadow_index(shadow).ok_or(ShadowError::UnknownShadow)?;
    shadow_body_check(&body)?;
    let deadline = Instant::now() + Duration::from_secs(SHADOW_WAIT_S);
    // Its turn, the semaphore asked again every 100 ms: a waiter woken and
    // then dropped before it ran leaves the next one asleep otherwise.
    let mut acquire = core::pin::pin!(TURN.acquire(1));
    let _turn = loop {
        match select(acquire.as_mut(), Timer::after(Duration::from_millis(100))).await {
            Either::First(turn) => break turn.map_err(|_| ShadowError::Busy)?,
            Either::Second(()) if Instant::now() >= deadline => return Err(ShadowError::Busy),
            Either::Second(()) => {}
        }
    };
    let bit = 1u8 << s;
    let id = loop {
        let mut x = with_deadline(deadline, EXCHANGE.lock())
            .await
            .map_err(|_| ShadowError::Busy)?;
        if let Some(e) = x.held(s, op) {
            return Err(e);
        }
        if READY.load(Ordering::Relaxed) & bit != 0 {
            let x = &mut *x;
            x.id = x.id.wrapping_add(1).max(1);
            if NONCE.load(Ordering::Relaxed) == 0 {
                NONCE.store(Instant::now().as_ticks() as u32 | 1, Ordering::Relaxed);
            }
            let mut token = [0u8; TOKEN_MAX];
            let n = shadow_token(NONCE.load(Ordering::Relaxed), x.id, &mut token);
            let text = core::str::from_utf8(&token[..n]).unwrap_or("");
            x.len = shadow_body(body, text, &mut x.buf)?;
            x.token = token;
            x.token_len = n;
            x.shadow = s;
            x.op = op;
            x.state = State::Queued(x.id);
            break x.id;
        }
        if REFUSED.load(Ordering::Relaxed) & bit != 0 {
            return Err(ShadowError::Refused);
        }
        drop(x);
        with_deadline(deadline, READY_EDGE.wait())
            .await
            .map_err(|_| ShadowError::NotConnected)?;
    };
    START.signal(id);
    // An answer for an earlier caller who gave up is not this one's.
    let answered = with_deadline(deadline, async { while DONE.wait().await != id {} }).await;
    if answered.is_err() {
        if let Ok(mut x) = EXCHANGE.try_lock() {
            if matches!(x.state, State::Queued(i) | State::Sent(i) if i == id) {
                x.state = State::Idle;
            }
        }
        return Err(ShadowError::Timeout);
    }
    let mut x = EXCHANGE.lock().await;
    x.state = State::Idle;
    let code = x.result?;
    let n = x.len.min(SLOT).min(doc.len());
    doc[..n].copy_from_slice(&x.buf[..n]);
    Ok(ShadowResponse {
        code,
        len: n,
        truncated: n < x.len,
    })
}

/// The connection ended: no shadow is ready, and a request queued or sent on
/// it fails - one that ended it is held for HOLD_S.
async fn shadow_lost(end: End) {
    READY.store(0, Ordering::Relaxed);
    REFUSED.store(0, Ordering::Relaxed);
    START.reset();
    let mut x = EXCHANGE.lock().await;
    let (id, sent) = match x.state {
        State::Queued(id) => (id, false),
        State::Sent(id) => (id, true),
        _ => return,
    };
    // A delete's answer is a few dozen bytes: too large is never it.
    let e = match (sent, end) {
        (true, End::TooLarge) if x.op != Op::Delete => ShadowError::TooLarge,
        (true, End::Refused) => ShadowError::Refused,
        _ => ShadowError::Disconnected,
    };
    if e != ShadowError::Disconnected {
        let (s, op) = (x.shadow, x.op as usize);
        x.hold[s][op] = Some((Instant::now() + Duration::from_secs(HOLD_S), e));
    }
    x.result = Err(e);
    x.state = State::Done(id);
    drop(x);
    DONE.signal(id);
}

#[embassy_executor::task]
async fn mqtt_task(stack: Stack<'static>) -> ! {
    link::init();
    let links = LINK.take();
    let b = BUFFERS.take();
    loop {
        stack.wait_config_up().await;
        // `Err` is a lost or refused connection; either way, start over.
        let mut end = End::Lost;
        let _ = session(stack, &mut b.rx, &mut b.tx, &mut b.work, links, &mut end).await;
        CONNECTED.store(false, Ordering::Relaxed);
        shadow_lost(end).await;
        Timer::after(Duration::from_secs(RETRY_S)).await;
    }
}

/// The broker's address: the host as an IPv4 literal, or looked up by DNS.
async fn resolve(stack: Stack<'static>) -> Result<IpAddress, ()> {
    if let Ok(ip) = BROKER_HOST.parse() {
        return Ok(IpAddress::Ipv4(ip));
    }
    let found = stack
        .dns_query(BROKER_HOST, DnsQueryType::A)
        .await
        .map_err(drop)?;
    found.first().copied().ok_or(())
}

type MqttClient<'c> = Client<'c, 'c, Link<'c>, BumpBuffer<'c>, 1, 1, 1, 0, 0, 0, 0>;

/// Keeps a refusal's reason code for `last_refusal()`; any other error is a
/// lost connection.
fn refused<const N: usize, A>(e: MqttError<'_, N, A>, what: fn(u8) -> Refusal) {
    if let MqttError::Disconnect { reason, .. } = e {
        REFUSAL.lock(|r| r.set(Some(what(reason.value()))));
    }
}

/// An error once connected: a DISCONNECT from the broker, or a lost link.
fn lost<const N: usize, A>(e: MqttError<'_, N, A>) {
    refused(e, Refusal::Disconnect);
}

/// What an error that ends the connection says of the request in flight: a
/// packet over WORK (rust-mqtt ends it with PacketTooLarge), or the policy -
/// unless one of the user's publications went out after the request: then
/// that may be what the policy refused.
fn end_of<const N: usize, A>(e: &MqttError<'_, N, A>, user_since: bool) -> End {
    match e {
        MqttError::Server | MqttError::Alloc => End::TooLarge,
        MqttError::Disconnect { reason, .. } if reason.value() == 0x87 && !user_since => End::Refused,
        _ => End::Lost,
    }
}

/// One connection, from TCP connect to the first error.
async fn session(
    stack: Stack<'static>,
    rx: &mut [u8],
    tx: &mut [u8],
    work: &mut [u8],
    links: &mut LinkBuffers,
    end: &mut End,
) -> Result<(), ()> {
    let address = resolve(stack).await?;
    let mut socket = TcpSocket::new(stack, rx, tx);
    // Closed after twice the keep-alive without a packet from the broker.
    // Without one (0: a timeout of 0 s would close the socket before its SYN
    // left), TCP's own probes find a dead link within a few minutes.
    if KEEP_ALIVE_S > 0 {
        socket.set_timeout(Some(Duration::from_secs(u64::from(KEEP_ALIVE_S) * 2)));
    } else {
        socket.set_keep_alive(Some(Duration::from_secs(60)));
        socket.set_timeout(Some(Duration::from_secs(180)));
    }
    socket.connect((address, BROKER_PORT)).await.map_err(drop)?;
    // Plain TCP as it is, or the TLS handshake - awaited whole.
    let link = with_timeout(Duration::from_secs(HANDSHAKE_S), link::open(socket, links))
        .await
        .map_err(drop)??;

    // The broker must not send a packet the work buffer cannot hold: one
    // that does would end every connection it is sent on.
    let max_packet = NonZero::new(u32::try_from(work.len()).unwrap_or(u32::MAX));
    let mut buffer = BumpBuffer::new(work);
    let mut client: MqttClient<'_> = Client::new(&mut buffer);
    let keep_alive = NonZero::new(KEEP_ALIVE_S).map_or(KeepAlive::Infinite, KeepAlive::Seconds);
    let mut options = ConnectOptions::new().clean_start().keep_alive(keep_alive);
    if !MQTT_USERNAME.is_empty() {
        options = options.user_name(MqttString::try_from(MQTT_USERNAME).map_err(drop)?);
    }
    if !MQTT_PASSWORD.is_empty() {
        options = options.password(MqttBinary::try_from(MQTT_PASSWORD.as_bytes()).map_err(drop)?);
    }
    if let Some(max) = max_packet {
        options = options.maximum_packet_size(max);
    }
    let id = MqttString::try_from(CLIENT_ID).map_err(drop)?;
    with_timeout(Duration::from_secs(ANSWER_S), client.connect(link, &options, Some(id)))
        .await
        .map_err(drop)?
        .map_err(|e| refused(e, Refusal::Connect))?;
    CONNECTED.store(true, Ordering::Relaxed);
    reset(&mut client);

    // The Device Shadow's answers first: AWS takes 50 subscriptions a
    // connection, so any it refuses past that are the user's last ones. A
    // shadow with one refused stays off on this connection.
    DELTAS.lock(|d| d.borrow_mut().deltas.reset());
    let mut refused_shadows = 0u8;
    for (i, filter) in SHADOW_SUBSCRIBE.iter().enumerate() {
        if !subscribe(&mut client, filter).await? {
            refused_shadows |= 1 << (i / 7);
        }
    }
    let ready_at = Instant::now() + Duration::from_secs(SETTLE_S);
    REFUSED.store(refused_shadows, Ordering::Relaxed);
    READY_EDGE.signal(());
    for topic in SUBSCRIBE {
        // Already subscribed for the shadow: its messages go there.
        if SHADOW_SUBSCRIBE.contains(topic) {
            continue;
        }
        subscribe(&mut client, topic).await?;
    }
    let all = ((1u16 << SHADOWS) - 1) as u8;
    let ready = if SHADOW_MAX > 0 { all & !refused_shadows } else { 0 };
    if ready != 0 {
        Timer::at(ready_at).await;
        // Deltas sent while the board was away are gone: each shadow is to
        // be read again.
        DELTAS.lock(|d| d.borrow_mut().deltas.connected(ready));
        READY.store(ready, Ordering::Relaxed);
        READY_EDGE.signal(());
        DELTA_NEW.signal(());
    }

    // Ping at half the keep-alive - ours or the one the broker settled on
    // (AWS IoT: 1200 s for none, 30 s for less), the shorter: the socket
    // closes after twice ours without a packet - unless something else went
    // out since. Never without either.
    let settled = match client.shared_config().keep_alive {
        KeepAlive::Seconds(s) => Some(s.get()),
        KeepAlive::Infinite => None,
    };
    let interval = match (KEEP_ALIVE_S, settled) {
        (0, settled) => settled,
        (ours, Some(settled)) => Some(ours.min(settled)),
        (ours, None) => Some(ours),
    }
    .map(|s| Duration::from_secs(u64::from(s.max(2) / 2)));
    let after = |now: Instant| interval.map_or(Instant::MAX, |i| now + i);
    let mut next_ping = after(Instant::now());
    // One of the user's publications went out since the shadow's request.
    let mut user_since = false;
    loop {
        // What comes in first: a loop that publishes without a pause must not
        // leave the shadow's answers - or anything else - unread.
        match select4(
            START.wait(),
            client.poll_header(),
            Timer::at(next_ping),
            OUTGOING.receive(),
        )
        .await
        {
            Either4::First(id) => {
                let mut x = EXCHANGE.lock().await;
                // Given up on, or already ended with the last connection.
                if x.state != State::Queued(id) {
                    continue;
                }
                let name = MqttString::try_from(request_topic(x.shadow, x.op)).map_err(drop)?;
                let topic = TopicReference::Name(TopicName::new(name).ok_or(())?);
                client
                    .publish(&PublicationOptions::new(topic), Bytes::from(&x.buf[..x.len]))
                    .await
                    .map_err(lost)?;
                x.state = State::Sent(id);
                user_since = false;
                next_ping = after(Instant::now());
            }
            Either4::Fourth(message) => {
                let name = MqttString::try_from(message.topic.as_str()).map_err(drop)?;
                let topic = TopicReference::Name(TopicName::new(name).ok_or(())?);
                client
                    .publish(
                        &PublicationOptions::new(topic),
                        Bytes::from(&message.payload[..]),
                    )
                    .await
                    .map_err(lost)?;
                user_since = true;
                next_ping = after(Instant::now());
            }
            Either4::Third(()) => {
                client.ping().await.map_err(lost)?;
                next_ping = after(Instant::now());
            }
            Either4::Second(header) => {
                let header = header.map_err(|e| {
                    *end = end_of(&e, user_since);
                    lost(e)
                })?;
                let event = client.poll_body(header).await.map_err(|e| {
                    *end = end_of(&e, user_since);
                    lost(e)
                })?;
                forward(event);
                reset(&mut client);
            }
        }
    }
}

/// Subscribes `filter` and waits for its SUBACK: `false` when the broker
/// refused it - kept for `last_refusal()`. One subscription in flight at a
/// time.
async fn subscribe(client: &mut MqttClient<'_>, filter: &'static str) -> Result<bool, ()> {
    let topic = TopicFilter::new(MqttString::try_from(filter).map_err(drop)?).ok_or(())?;
    client
        .subscribe(topic, &SubscriptionOptions::new())
        .await
        .map_err(lost)?;
    let code = with_timeout(Duration::from_secs(ANSWER_S), async {
        loop {
            if let Some(code) = poll(client).await? {
                return Ok::<ReasonCode, ()>(code);
            }
        }
    })
    .await
    .map_err(drop)??;
    if code.is_erroneous() {
        REFUSAL.lock(|r| r.set(Some(Refusal::Subscribe(filter, code.value()))));
        return Ok(false);
    }
    Ok(true)
}

/// Reads one packet; a SUBACK's reason code when it was one.
async fn poll(client: &mut MqttClient<'_>) -> Result<Option<ReasonCode>, ()> {
    let event = client.poll().await.map_err(lost)?;
    let suback = match &event {
        Event::Suback(suback) => Some(suback.reason_code),
        _ => None,
    };
    forward(event);
    reset(client);
    Ok(suback)
}

/// Hands a publication to `incoming()`; drops it if that queue is full. One
/// on a shadow's answer topic goes to the shadow instead.
fn forward(event: Event<'_, 0, 0>) {
    let Event::Publish(publish) = event else {
        return;
    };
    let Some(name) = publish.topic.name() else {
        return;
    };
    let name = name.as_ref().as_str();
    if let Some((s, kind)) = shadow_topic(name) {
        shadow_message(s, kind, &publish.message);
        return;
    }
    let topic = heapless::String::try_from(name);
    let payload = heapless::Vec::from_slice(&publish.message);
    if let (Ok(topic), Ok(payload)) = (topic, payload) {
        let _ = INCOMING.try_send(Message { topic, payload });
    }
}

/// A message on shadow `s`'s answer topic `kind`: the pending request's
/// answer, a delta, or someone else's - which still says a shadow was
/// deleted or is not there.
fn shadow_message(s: usize, kind: Kind, payload: &[u8]) {
    let mut text = [0u8; 64];
    let token = json_str(payload, &["clientToken"], &mut text);
    let version = json_i64(payload, &["version"]);
    let timestamp = json_i64(payload, &["timestamp"]);
    if kind == Kind::Delta {
        let own = token.is_some_and(|t| own_token(t, NONCE.load(Ordering::Relaxed)));
        let kept = DELTAS.lock(|d| {
            let mut d = d.borrow_mut();
            if !d.deltas.delta(s, version, timestamp, own, payload.len()) {
                return false;
            }
            let keep = payload.len().min(SHADOW_MAX);
            d.buf[..keep].copy_from_slice(&payload[..keep]);
            true
        });
        if kept {
            DELTA_NEW.signal(());
        }
        return;
    }
    // The caller holds the lock only while no answer can be its own.
    let mut ours = false;
    if let Ok(mut x) = EXCHANGE.try_lock() {
        if let State::Sent(id) = x.state {
            if x.shadow == s && shadow_answers(kind, token, x.op, x.token()) {
                let x = &mut *x;
                let keep = payload.len().min(SLOT);
                x.buf[..keep].copy_from_slice(&payload[..keep]);
                x.len = payload.len();
                x.result = Ok(if kind.accepted() { 200 } else { rejected_code(payload) });
                x.state = State::Done(id);
                DONE.signal(id);
                ours = true;
            }
        }
    }
    DELTAS.lock(|d| {
        let d = &mut d.borrow_mut().deltas;
        match kind {
            Kind::GetAccepted if ours => d.own_get(s, version, timestamp),
            Kind::GetRejected if rejected_code(payload) == 404 => d.not_found(s),
            Kind::DeleteAccepted => d.deleted(s),
            _ => {}
        }
    });
}

/// Frees the packet memory of the event just handled.
fn reset(client: &mut MqttClient<'_>) {
    // SAFETY: every event and error borrowed from this buffer has been
    // dropped by now - `forward` copies what it keeps.
    unsafe { client.buffer_mut().reset() };
}
