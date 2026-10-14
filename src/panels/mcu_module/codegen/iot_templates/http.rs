
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
        let n = build(&mut x.head, req.method, &url, req.auth, req.content_type, req.headers, req.payload.len())?;
        x.head.truncate(n);
        x.payload = heapless::Vec::from_slice(req.payload).map_err(|_| HttpError::TooLarge)?;
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

/// The longest URL `aws_publish()` builds: the endpoint, its port, and a
/// 256-byte topic percent-encoded throughout.
const AWS_URL_MAX: usize = "https://".len() + HOST_MAX + ":65535/topics/?qos=1".len() + 3 * AWS_TOPIC_MAX;

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
    if AWS_PORT == 0 {
        return Err(HttpError::Url);
    }
    let mut url = [0u8; AWS_URL_MAX];
    let n = aws_url(&mut url, CERT_HOST, AWS_PORT, topic, AWS_QOS)?;
    // Every byte aws_url wrote is ASCII.
    let url = core::str::from_utf8(&url[..n]).map_err(|_| HttpError::Url)?;
    let req = Request {
        method: Method::Post,
        payload,
        ..Request::get(url)
    };
    request(req, body).await
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
