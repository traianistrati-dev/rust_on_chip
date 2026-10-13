
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

use core::cell::Cell;
use core::num::NonZero;
use core::sync::atomic::{AtomicBool, Ordering};

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer, with_timeout};
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

/// Starts the task that owns the connection.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(mqtt_task(stack).unwrap());
}

/// The link's own buffers (TLS records; nothing for plain TCP), in a static:
/// built in place, never on the stack.
static LINK: ConstStaticCell<LinkBuffers> = ConstStaticCell::new(LinkBuffers::new());

#[embassy_executor::task]
async fn mqtt_task(stack: Stack<'static>) -> ! {
    link::init();
    let links = LINK.take();
    let mut rx = [0u8; link::TCP_RX];
    let mut tx = [0u8; 1024];
    let mut work = [0u8; 1024];
    loop {
        stack.wait_config_up().await;
        // `Err` is a lost or refused connection; either way, start over.
        let _ = session(stack, &mut rx, &mut tx, &mut work, links).await;
        CONNECTED.store(false, Ordering::Relaxed);
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

/// One connection, from TCP connect to the first error.
async fn session(
    stack: Stack<'static>,
    rx: &mut [u8],
    tx: &mut [u8],
    work: &mut [u8],
    links: &mut LinkBuffers,
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

    for topic in SUBSCRIBE {
        let filter = TopicFilter::new(MqttString::try_from(*topic).map_err(drop)?).ok_or(())?;
        client
            .subscribe(filter, &SubscriptionOptions::new())
            .await
            .map_err(lost)?;
        // One subscription in flight at a time: wait for its SUBACK.
        let code = with_timeout(Duration::from_secs(ANSWER_S), async {
            loop {
                if let Some(code) = poll(&mut client).await? {
                    return Ok::<ReasonCode, ()>(code);
                }
            }
        })
        .await
        .map_err(drop)??;
        if code.is_erroneous() {
            REFUSAL.lock(|r| r.set(Some(Refusal::Subscribe(topic, code.value()))));
        }
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
    loop {
        match select3(
            OUTGOING.receive(),
            Timer::at(next_ping),
            client.poll_header(),
        )
        .await
        {
            Either3::First(message) => {
                let name = MqttString::try_from(message.topic.as_str()).map_err(drop)?;
                let topic = TopicReference::Name(TopicName::new(name).ok_or(())?);
                client
                    .publish(
                        &PublicationOptions::new(topic),
                        Bytes::from(&message.payload[..]),
                    )
                    .await
                    .map_err(lost)?;
                next_ping = after(Instant::now());
            }
            Either3::Second(()) => {
                client.ping().await.map_err(lost)?;
                next_ping = after(Instant::now());
            }
            Either3::Third(header) => {
                let header = header.map_err(lost)?;
                let event = client.poll_body(header).await.map_err(lost)?;
                forward(event);
                reset(&mut client);
            }
        }
    }
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

/// Hands a publication to `incoming()`; drops it if that queue is full.
fn forward(event: Event<'_, 0, 0>) {
    let Event::Publish(publish) = event else {
        return;
    };
    let Some(name) = publish.topic.name() else {
        return;
    };
    let topic = heapless::String::try_from(name.as_ref().as_str());
    let payload = heapless::Vec::from_slice(&publish.message);
    if let (Ok(topic), Ok(payload)) = (topic, payload) {
        let _ = INCOMING.try_send(Message { topic, payload });
    }
}

/// Frees the packet memory of the event just handled.
fn reset(client: &mut MqttClient<'_>) {
    // SAFETY: every event and error borrowed from this buffer has been
    // dropped by now - `forward` copies what it keeps.
    unsafe { client.buffer_mut().reset() };
}
