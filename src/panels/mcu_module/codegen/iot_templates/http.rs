
// Everything below is editable — your changes are preserved on regeneration.
//
// From anywhere in the firmware:
//
//     let mut body = [0u8; 512];
//     let r = pins::configs::http::get("https://api.open-meteo.com/v1/forecast?latitude=44.43&longitude=26.1&current=temperature_2m", &mut body).await;
//     // Ok(r): r.status, &body[..r.len], r.truncated, r.location()
//
//     pins::configs::http::post("http://192.168.1.10:8123/api/states/sensor.board", "application/json", b"{\"state\":\"21.5\"}", &mut body).await.ok();
//
// `request()` takes the rest: the method, an Authorization (Bearer, Basic or
// as given), more headers. Tokens belong in secrets.rs, never in a commit.
//
// AWS IoT, with the board's certificate (IoT tab: HTTP > Client certificate):
//
//     let r = pins::configs::http::aws_publish("my-thing/temperature", b"{\"t\":21.5}", &mut body).await;
//     // Ok(r) with r.status == 200: the broker took it
//
// Retained, or with MQTT 5's properties - a content type, an expiry, a
// response topic, user properties:
//
//     use pins::configs::http::AwsMessage;
//     let m = AwsMessage { retain: true, ..AwsMessage::new("my-thing/state", b"{\"on\":true}") };
//     pins::configs::http::aws_publish_with(&m, &mut body).await.ok();
//
// The thing's Device Shadow - read, reported to; the answer is the shadow:
//
//     use pins::configs::http;
//     if let Ok(r) = http::shadow_get(None, &mut body).await {
//         let led = http::json_bool(&body[..r.len], &["state", "delta", "led"]);
//     }
//     http::shadow_report(None, br#"{"led":true}"#, &mut body).await.ok();
//
// One task makes the requests, one at a time: it holds the socket, the TLS
// buffers and the handshake, so a caller's own task stays small. Each request
// opens its own connection (`Connection: close`) and takes TIMEOUT_S at most.
// A redirect (3xx) is reported - `location()` - and never followed. The
// protocol is at the end of this file; the IDE tests it.

use embassy_executor::Spawner;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::{IpAddress, Stack};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_sync::semaphore::{FairSemaphore, Semaphore};
use embassy_sync::signal::Signal;
use embassy_time::{Duration, with_timeout};
use static_cell::ConstStaticCell;

/// The longest host name a URL may carry - the TLS name check takes 64.
const HOST_MAX: usize = 64;
/// Callers that may wait for their turn at once; one more gets `Busy`.
const QUEUE: usize = 8;
/// The socket's buffers: TLS records stream through the receive side.
const SOCKET_RX: usize = 2048;
const SOCKET_TX: usize = 1024;

/// One request, as `request()` sends it.
pub struct Request<'a> {
    pub method: Method,
    /// `http://` or `https://`, a host name or an IPv4 address, a port, the
    /// path and the query.
    pub url: &'a str,
    pub auth: Auth<'a>,
    pub content_type: Option<&'a str>,
    /// More headers, as (name, value).
    pub headers: &'a [(&'a str, &'a str)],
    /// The body; at most PAYLOAD_MAX bytes.
    pub payload: &'a [u8],
}

impl<'a> Request<'a> {
    /// A GET of `url`, nothing else.
    pub const fn get(url: &'a str) -> Self {
        Self {
            method: Method::Get,
            url,
            auth: Auth::None,
            content_type: None,
            headers: &[],
            payload: &[],
        }
    }

    /// A POST of `payload` to `url`, as `content_type`.
    pub const fn post(url: &'a str, content_type: &'a str, payload: &'a [u8]) -> Self {
        Self {
            method: Method::Post,
            url,
            auth: Auth::None,
            content_type: Some(content_type),
            headers: &[],
            payload,
        }
    }
}

/// GETs `url`; the body lands in `body`, `len` bytes of it.
pub async fn get(url: &str, body: &mut [u8]) -> Result<Response, HttpError> {
    request(Request::get(url), body).await
}

/// POSTs `payload` to `url` as `content_type`; the answer's body lands in
/// `body`.
pub async fn post(url: &str, content_type: &str, payload: &[u8], body: &mut [u8]) -> Result<Response, HttpError> {
    request(Request::post(url, content_type, payload), body).await
}

/// Sends `req` and waits for the answer: its body lands in `body`, and what
/// does not fit there or in BODY_MAX is dropped (`truncated`).
pub async fn request(req: Request<'_>, body: &mut [u8]) -> Result<Response, HttpError> {
    send(&req, &[req.payload], body).await
}

/// `request()` with the body in pieces, sent one after the other -
/// `req.payload` aside - so a body put together is never put together on a
/// stack.
async fn send(req: &Request<'_>, payload: &[&[u8]], body: &mut [u8]) -> Result<Response, HttpError> {
    let url = Url::parse(req.url)?;
    if url.https && !HTTPS {
        return Err(HttpError::NoTls);
    }
    // One request at a time - the task holds one connection - and the
    // callers in the order they came: one that calls again at once does not
    // starve the others.
    let _turn = TURN.acquire(1).await.map_err(|_| HttpError::Busy)?;
    let id = {
        let mut x = EXCHANGE.lock().await;
        x.id = x.id.wrapping_add(1);
        x.https = url.https;
        x.host = heapless::String::try_from(url.host).map_err(|_| HttpError::TooLarge)?;
        x.port = url.port;
        x.method = req.method;
        x.head.clear();
        x.head
            .resize_default(REQUEST_HEAD_MAX)
            .map_err(|_| HttpError::TooLarge)?;
        let len = payload.iter().map(|p| p.len()).sum();
        let n = build(&mut x.head, req.method, &url, req.auth, req.content_type, req.headers, len)?;
        x.head.truncate(n);
        x.payload.clear();
        for piece in payload {
            x.payload
                .extend_from_slice(piece)
                .map_err(|_| HttpError::TooLarge)?;
        }
        x.id
    };
    START.signal(id);
    // An answer for an earlier caller who gave up is not this one's.
    while DONE.wait().await != id {}
    let x = EXCHANGE.lock().await;
    let mut response = x.result?;
    let keep = response.len.min(body.len());
    body[..keep].copy_from_slice(&x.body[..keep]);
    if keep < response.len {
        response.len = keep;
        response.truncated = true;
    }
    Ok(response)
}

/// Publishes `payload` on `topic` in AWS IoT over HTTPS, with the board's
/// certificate: the endpoint, port and QoS the tab gave (CERT_HOST,
/// AWS_PORT, AWS_QOS). The answer lands in `body`, as `request()`'s does:
///
/// - `Ok(r)`, `r.status == 200`: the broker took the message - whether
///   anyone is subscribed or not. Any other status is a refusal; 403: the
///   policy allows no iot:Publish on this topic (`body` may say more).
/// - `Err(Io)` or `Err(Closed)`: AWS reset the connection after the
///   handshake - the certificate is not registered, not active, or no
///   policy is attached to it.
/// - `Err(Url)`: no AWS endpoint in the tab, or a topic AWS would refuse
///   (empty, over 256 bytes, more than 7 `/`, `+` or `#`, a `$` topic other
///   than `$aws/...`).
///
/// Each call is a connection and a handshake of its own: about a second on
/// an ESP32-C3. A lost answer and a retry can publish the message twice.
pub async fn aws_publish(topic: &str, payload: &[u8], body: &mut [u8]) -> Result<Response, HttpError> {
    aws_publish_with(&AwsMessage::new(topic, payload), body).await
}

/// `aws_publish()` with the rest of an MQTT 5 message: retained, a content
/// type, an expiry, a response topic and correlation data, user properties,
/// the payload marked UTF-8 (see `AwsMessage`):
///
///     let m = AwsMessage { retain: true, ..AwsMessage::new("my-thing/state", b"{\"on\":true}") };
///     pins::configs::http::aws_publish_with(&m, &mut body).await
///
/// A retained message needs iot:RetainPublish (the tab's "Retained
/// messages"), at most one a second per topic; an empty retained payload
/// deletes the topic's. `Err(Url)` also for a retained `$` topic, a content
/// type over 256 bytes, a response topic with a wildcard, a payload marked
/// UTF-8 that is not. The URL and the headers are built here, on the
/// caller's task - one kilobyte of it while the request runs.
pub async fn aws_publish_with(m: &AwsMessage<'_>, body: &mut [u8]) -> Result<Response, HttpError> {
    if AWS_PORT == 0 {
        return Err(HttpError::Url);
    }
    // Both go into the request head, so one head's worth holds both: the
    // URL, then the headers' values after it.
    let mut buf = [0u8; REQUEST_HEAD_MAX];
    let n = aws_message_url(&mut buf, CERT_HOST, AWS_PORT, m, AWS_QOS)?;
    let (url, values) = buf.split_at_mut(n);
    // Every byte aws_message_url wrote is ASCII.
    let url = core::str::from_utf8(url).map_err(|_| HttpError::Url)?;
    let headers = aws_message_headers(values, m)?;
    let req = Request {
        method: Method::Post,
        headers: headers.as_slice(),
        payload: m.payload,
        ..Request::get(url)
    };
    request(req, body).await
}

/// The board's Device Shadow in AWS IoT: the thing the tab names
/// (AWS_THING), over HTTPS with the board's certificate - AWS's REST API,
/// whose answer is the shadow itself, so nothing needs subscribing.
/// `shadow`: `None` = the thing's classic shadow, `Some("config")` a named
/// one.
///
/// - `Ok(r)`, `r.status == 200`: `body[..r.len]` holds the document -
///   `state.desired`, `state.reported`, `state.delta` (what desired asks
///   and reported does not show yet), `metadata`, `version`. Read it with
///   `json_bool`, `json_f64`, `json_i64`, `json_str`, `json_get`:
///
///       let doc = &body[..r.len];
///       if let Some(on) = http::json_bool(doc, &["state", "delta", "led"]) { /* ... */ }
///
///   The metadata - a timestamp for every key - makes it about three times
///   the state: an answer over BODY_MAX comes `truncated`, and what was cut
///   reads `None`.
/// - 404: no such shadow yet (or no such thing: AWS answers alike).
/// - 401 or 403: the policy allows no iot:GetThingShadow on this thing.
/// - `Err(Io)` or `Err(Closed)`: the certificate, as for `aws_publish()`.
///   `Err(Url)`: no AWS endpoint or no thing in the tab, or a shadow name
///   that is not 1 to 64 letters, digits, `:`, `_`, `-`, `$`.
pub async fn shadow_get(shadow: Option<&str>, body: &mut [u8]) -> Result<Response, HttpError> {
    shadow_request(Method::Get, shadow, &[], body).await
}

/// Updates the shadow with `document` - AWS's request document, `state`
/// holding `desired` and/or `reported`, nothing else:
///
///     http::shadow_update(None, br#"{"state":{"reported":{"led":true}}}"#, &mut body).await
///
/// Only the keys it names change; a key set to `null` goes. `"version": N`
/// beside `state` updates only that version (409 otherwise);
/// `"clientToken"` (64 bytes at most) comes back in the answer. 200: the
/// answer holds what was taken, and `version`, the new one. 400: AWS says
/// why in `body` - `{}` alone, or a key beside desired and reported inside
/// `state`, are refused. 413: the shadow would pass 8 KB. A change to
/// `desired` that `reported` does not show reaches MQTT subscribers on
/// `$aws/things/<thing>/shadow/update/delta` - a named shadow's on
/// `$aws/things/<thing>/shadow/name/<shadow>/update/delta`. Needs
/// iot:UpdateThingShadow.
pub async fn shadow_update(shadow: Option<&str>, document: &[u8], body: &mut [u8]) -> Result<Response, HttpError> {
    shadow_request(Method::Post, shadow, &[document], body).await
}

/// `shadow_update()` with `{"state":{"reported":<reported>}}` - the board
/// saying what it is; `reported` is a JSON object:
///
///     http::shadow_report(None, br#"{"t":21.5,"led":true}"#, &mut body).await
pub async fn shadow_report(shadow: Option<&str>, reported: &[u8], body: &mut [u8]) -> Result<Response, HttpError> {
    let pieces: [&[u8]; 3] = [br#"{"state":{"reported":"#, reported, b"}}"];
    shadow_request(Method::Post, shadow, &pieces, body).await
}

/// Deletes the shadow. 200 alone says it went (AWS documents two different
/// answers for it); 404: there was none. Its version goes on from where it
/// was if it comes back within 48 hours. Needs iot:DeleteThingShadow.
pub async fn shadow_delete(shadow: Option<&str>, body: &mut [u8]) -> Result<Response, HttpError> {
    shadow_request(Method::Delete, shadow, &[], body).await
}

/// One call to the shadow's REST API: its URL built on the caller's task,
/// a kilobyte while the request runs. Another thing's shadow: build its URL
/// with `aws_shadow_url()` and send it with `request()`.
async fn shadow_request(method: Method, shadow: Option<&str>, payload: &[&[u8]], body: &mut [u8]) -> Result<Response, HttpError> {
    if AWS_PORT == 0 {
        return Err(HttpError::Url);
    }
    let mut buf = [0u8; REQUEST_HEAD_MAX];
    let n = aws_shadow_url(&mut buf, CERT_HOST, AWS_PORT, AWS_THING, shadow)?;
    // Every byte aws_shadow_url wrote is ASCII.
    let url = core::str::from_utf8(&buf[..n]).map_err(|_| HttpError::Url)?;
    send(&Request { method, ..Request::get(url) }, payload, body).await
}

/// Starts the task that makes the requests.
pub fn start(spawner: Spawner, stack: Stack<'static>) {
    spawner.spawn(http_task(stack).unwrap());
}

/// What the caller hands the task and gets back, in a static: built in
/// place, never on a stack.
pub struct Exchange {
    id: u32,
    https: bool,
    host: heapless::String<HOST_MAX>,
    port: u16,
    method: Method,
    head: heapless::Vec<u8, REQUEST_HEAD_MAX>,
    payload: heapless::Vec<u8, PAYLOAD_MAX>,
    result: Result<Response, HttpError>,
    body: [u8; BODY_MAX],
}

impl Exchange {
    const fn new() -> Self {
        Self {
            id: 0,
            https: false,
            host: heapless::String::new(),
            port: 0,
            method: Method::Get,
            head: heapless::Vec::new(),
            payload: heapless::Vec::new(),
            result: Err(HttpError::Closed),
            body: [0; BODY_MAX],
        }
    }
}

static TURN: FairSemaphore<CriticalSectionRawMutex, QUEUE> = FairSemaphore::new(1);
static EXCHANGE: Mutex<CriticalSectionRawMutex, Exchange> = Mutex::new(Exchange::new());
static START: Signal<CriticalSectionRawMutex, u32> = Signal::new();
static DONE: Signal<CriticalSectionRawMutex, u32> = Signal::new();
/// The TLS record buffers (nothing without HTTPS), kept between requests.
static LINKS: ConstStaticCell<LinkBuffers> = ConstStaticCell::new(LinkBuffers::new());

#[embassy_executor::task]
async fn http_task(stack: Stack<'static>) -> ! {
    let links = LINKS.take();
    let mut rx = [0u8; SOCKET_RX];
    let mut tx = [0u8; SOCKET_TX];
    loop {
        let id = START.wait().await;
        let mut x = EXCHANGE.lock().await;
        // A request its caller replaced before it began: the next start is
        // that caller's.
        if x.id != id {
            continue;
        }
        let run = one(stack, &mut rx, &mut tx, links, &mut x);
        x.result = with_timeout(Duration::from_secs(TIMEOUT_S), run)
            .await
            .unwrap_or(Err(HttpError::Timeout));
        drop(x);
        DONE.signal(id);
    }
}

/// One request, from looking the host up to the end of the answer.
async fn one(
    stack: Stack<'static>,
    rx: &mut [u8],
    tx: &mut [u8],
    links: &mut LinkBuffers,
    x: &mut Exchange,
) -> Result<Response, HttpError> {
    stack.wait_config_up().await;
    let address = if let Ok(ip) = x.host.parse() {
        IpAddress::Ipv4(ip)
    } else {
        let found = stack
            .dns_query(&x.host, DnsQueryType::A)
            .await
            .map_err(|_| HttpError::Dns)?;
        *found.first().ok_or(HttpError::Dns)?
    };
    let mut socket = TcpSocket::new(stack, rx, tx);
    socket.set_timeout(Some(Duration::from_secs(TIMEOUT_S)));
    socket
        .connect((address, x.port))
        .await
        .map_err(|_| HttpError::Connect)?;
    if x.https {
        over_tls(socket, links, x).await
    } else {
        exchange(&mut socket, x).await
    }
}

/// The request out - the head the caller built, the payload - and the
/// answer in.
async fn exchange<C: embedded_io_async::Read + embedded_io_async::Write>(
    c: &mut C,
    x: &mut Exchange,
) -> Result<Response, HttpError> {
    c.write_all(&x.head).await.map_err(|_| HttpError::Io)?;
    c.write_all(&x.payload).await.map_err(|_| HttpError::Io)?;
    c.flush().await.map_err(|_| HttpError::Io)?;
    receive(c, x.method, &mut x.body).await
}
