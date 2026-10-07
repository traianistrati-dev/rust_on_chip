
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     pins::configs::mqtt::publish("my/topic", b"21.5").await.ok();
//     let msg = pins::configs::mqtt::incoming().await;   // msg.topic, msg.payload
//
// One task owns the connection: it reconnects on its own, and `publish` only
// queues the message for it. Port 1883 is plain TCP - nothing is encrypted.

use core::num::NonZero;

use embassy_executor::Spawner;
use embassy_futures::select::{Either3, select3};
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Instant, Timer};
use rust_mqtt::Bytes;
use rust_mqtt::buffer::BumpBuffer;
use rust_mqtt::client::Client;
use rust_mqtt::client::event::Event;
use rust_mqtt::client::options::{
    ConnectOptions, PublicationOptions, SubscriptionOptions, TopicReference,
};
use rust_mqtt::config::KeepAlive;
use rust_mqtt::types::{MqttBinary, MqttString, TopicFilter, TopicName};

use super::secrets::{MQTT_PASSWORD, MQTT_USERNAME};

/// Longest topic a `Message` carries.
pub const MAX_TOPIC: usize = 64;
/// Largest payload a `Message` carries.
pub const MAX_PAYLOAD: usize = 256;
/// Seconds to wait before connecting again after the broker is lost.
pub const RETRY_S: u64 = 5;

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
}

static OUTGOING: Channel<CriticalSectionRawMutex, Message, 4> = Channel::new();
static INCOMING: Channel<CriticalSectionRawMutex, Message, 4> = Channel::new();

/// Queues `payload` for `topic` (QoS 0). Waits only while the queue is full.
pub async fn publish(topic: &str, payload: &[u8]) -> Result<(), PublishError> {
    let message = Message {
        topic: heapless::String::try_from(topic).map_err(|_| PublishError::TopicTooLong)?,
        payload: heapless::Vec::from_slice(payload).map_err(|_| PublishError::PayloadTooLarge)?,
    };
    OUTGOING.send(message).await;
    Ok(())
}

/// The next publication on a subscribed topic.
pub async fn incoming() -> Message {
    INCOMING.receive().await
}

/// Starts the task that owns the connection.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(mqtt_task(stack).unwrap());
}

#[embassy_executor::task]
async fn mqtt_task(stack: Stack<'static>) -> ! {
    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 1024];
    let mut work = [0u8; 1024];
    loop {
        stack.wait_config_up().await;
        // `Err` is a lost or refused connection; either way, start over.
        let _ = session(stack, &mut rx, &mut tx, &mut work).await;
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

type MqttClient<'c> = Client<'c, 'c, TcpSocket<'c>, BumpBuffer<'c>, 1, 1, 1, 0, 0, 0, 0>;

/// One connection, from TCP connect to the first error.
async fn session(
    stack: Stack<'static>,
    rx: &mut [u8],
    tx: &mut [u8],
    work: &mut [u8],
) -> Result<(), ()> {
    let address = resolve(stack).await?;
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(u64::from(KEEP_ALIVE_S) * 2)));
    socket.connect((address, BROKER_PORT)).await.map_err(drop)?;

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
    let id = MqttString::try_from(CLIENT_ID).map_err(drop)?;
    client.connect(socket, &options, Some(id)).await.map_err(drop)?;
    reset(&mut client);

    for topic in SUBSCRIBE {
        let filter = TopicFilter::new(MqttString::try_from(*topic).map_err(drop)?).ok_or(())?;
        client
            .subscribe(filter, &SubscriptionOptions::new())
            .await
            .map_err(drop)?;
        // One subscription in flight at a time: wait for its SUBACK.
        while !poll(&mut client).await? {}
    }

    // Ping at half the keep-alive, unless something else went out since.
    let interval = Duration::from_secs(u64::from(KEEP_ALIVE_S.max(2) / 2));
    let mut next_ping = Instant::now() + interval;
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
                    .map_err(drop)?;
                next_ping = Instant::now() + interval;
            }
            Either3::Second(()) => {
                client.ping().await.map_err(drop)?;
                next_ping = Instant::now() + interval;
            }
            Either3::Third(header) => {
                let header = header.map_err(drop)?;
                let event = client.poll_body(header).await.map_err(drop)?;
                forward(event);
                reset(&mut client);
            }
        }
    }
}

/// Reads one packet; `true` when it was a SUBACK.
async fn poll(client: &mut MqttClient<'_>) -> Result<bool, ()> {
    let event = client.poll().await.map_err(drop)?;
    let was_suback = matches!(event, Event::Suback(_));
    forward(event);
    reset(client);
    Ok(was_suback)
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
