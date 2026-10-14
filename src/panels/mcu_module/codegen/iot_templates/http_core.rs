
// ── HTTP/1.1: the request and the answer ──────────────────────────────────
// Everything from here down is the protocol alone, over any embedded-io-async
// connection - the TcpSocket for http://, the TLS link for https://. The IDE
// runs this very text through its own tests (RFC 9112's corner cases and a
// fuzz run that must neither panic nor spin) before it ships.
//
// One request per connection (`Connection: close`): the server ends the
// connection after its answer, so nothing is kept between requests and a
// body that does not fit is simply left unread.
//
// Every item carries #[allow(dead_code)]: a firmware that makes no request
// still builds without a warning.

/// Longest response head (status line and headers) accepted. Real ones pass
/// 1 KB: Discord's, behind Cloudflare, is 1445 bytes.
#[allow(dead_code)]
pub const HEAD_MAX: usize = 2048;
/// Longest `Location` kept from a redirect; a longer one is dropped.
#[allow(dead_code)]
pub const LOCATION_MAX: usize = 128;
/// Longest request head (request line and headers): built whole, then
/// written with one call - one await instead of one per piece.
#[allow(dead_code)]
pub const REQUEST_HEAD_MAX: usize = 1024;
/// Interim answers (100 Continue, 103 Early Hints) skipped before the real one.
#[allow(dead_code)]
const INTERIM_MAX: u8 = 4;
/// Longest chunk-size line, extensions included.
#[allow(dead_code)]
const LINE_MAX: usize = 256;
/// Longest trailer section after the last chunk.
#[allow(dead_code)]
const TRAILERS_MAX: usize = 1024;

/// The request's method.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    /// The head alone comes back, whatever its Content-Length says.
    Head,
    Post,
    Put,
    Patch,
    Delete,
}

#[allow(dead_code)]
impl Method {
    fn as_bytes(self) -> &'static [u8] {
        match self {
            Method::Get => b"GET",
            Method::Head => b"HEAD",
            Method::Post => b"POST",
            Method::Put => b"PUT",
            Method::Patch => b"PATCH",
            Method::Delete => b"DELETE",
        }
    }

    /// Requests that always say how long their body is, even when it is
    /// empty: a server may refuse one without (411 Length Required).
    fn sends_length(self) -> bool {
        matches!(self, Method::Post | Method::Put | Method::Patch)
    }
}

/// The `Authorization` header. No `Debug`: it would print the secret.
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum Auth<'a> {
    None,
    /// `Bearer <token>`: Home Assistant, most REST APIs.
    Bearer(&'a str),
    /// `Basic base64(user:password)`; the user may not hold a `:` (RFC 7617).
    Basic(&'a str, &'a str),
    /// The whole value as given, e.g. InfluxDB's `Token <token>`.
    Raw(&'a str),
}

/// Why a request failed. `Url::parse`, `build` and `receive` return the
/// first six and `TooLarge`; the rest belong to the connection, which the
/// code above this part opens.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    /// More tasks wait for their turn than the queue holds (QUEUE in the
    /// tail above).
    Busy,
    /// Not http:// or https://, user info or an IPv6 host in it, a bad port,
    /// or a byte the path must carry percent-encoded.
    Url,
    /// A header the request cannot carry: CR, LF or NUL in a value, a bad
    /// name, a Basic user with a `:`, or one this code writes itself.
    Header,
    /// The connection failed while the request or the answer went through it.
    Io,
    /// The server closed before the head was whole, or inside a framed body.
    Closed,
    /// The response head is longer than `HEAD_MAX`.
    HeadTooLarge,
    /// Not HTTP/1.x, a bad status line or header, conflicting lengths, a bad
    /// chunk, a 101 Switching Protocols, or more than `INTERIM_MAX` 1xx answers.
    BadResponse,
    /// The host name could not be resolved.
    Dns,
    /// No TCP connection: refused, unreachable, or no answer at all.
    Connect,
    /// The TLS handshake or its certificate checks failed: the chain, the
    /// name, the dates.
    Tls,
    /// An https:// URL while HTTPS is off for this chip or project; nothing
    /// was sent.
    NoTls,
    /// The whole request - connection, handshake, request and answer - took
    /// longer than `TIMEOUT_S`.
    Timeout,
    /// The URL, the request head or the payload does not fit the board's
    /// buffers.
    TooLarge,
}

/// `http[s]://host[:port][/path][?query][#fragment]`, the host a name or an
/// IPv4 address.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Url<'a> {
    pub https: bool,
    pub host: &'a str,
    pub port: u16,
    /// Path and query as given; the fragment is dropped. Private, so a `Url`
    /// comes from `parse` alone and what reaches the request line was checked.
    target: &'a str,
}

#[allow(dead_code)]
impl<'a> Url<'a> {
    pub fn parse(url: &'a str) -> Result<Self, HttpError> {
        let (scheme, rest) = url.split_once("://").ok_or(HttpError::Url)?;
        let https = if scheme.eq_ignore_ascii_case("https") {
            true
        } else if scheme.eq_ignore_ascii_case("http") {
            false
        } else {
            return Err(HttpError::Url);
        };
        // The host ends at the path, or at a query or fragment that follows
        // it with no `/`: `https://host?key=1` is a valid URL, and its key
        // must not go out as part of a DNS name.
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, target) = rest.split_at(end);
        let target = target.split('#').next().unwrap_or("");
        // Credentials go through `Auth`, never the URL; IPv6 is not built in.
        if authority.contains(['@', '[']) {
            return Err(HttpError::Url);
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, "")) => (host, None),
            Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                (host, Some(port.parse::<u16>().map_err(|_| HttpError::Url)?))
            }
            Some(_) => return Err(HttpError::Url),
            None => (authority, None),
        };
        let port = port.unwrap_or(if https { 443 } else { 80 });
        let name = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_';
        if port == 0 || host.is_empty() || !host.bytes().all(name) {
            return Err(HttpError::Url);
        }
        // A space, CR or LF here would end the request line early.
        if !target.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(HttpError::Url);
        }
        Ok(Url {
            https,
            host,
            port,
            target,
        })
    }

    /// Path and query as given: "" when there is neither, "?a=1" when the
    /// query follows the host with no `/`. The request line puts a `/` in
    /// front of both.
    pub fn target(&self) -> &'a str {
        self.target
    }

    fn default_port(&self) -> bool {
        self.port == if self.https { 443 } else { 80 }
    }
}

/// What came back. The body is in the caller's buffer, `len` bytes of it.
/// `Copy`, so a task can hand it on to another as it is.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct Response {
    pub status: u16,
    pub len: usize,
    /// The body did not fit: the first `len` bytes are kept, the rest is
    /// left unread (the connection ends anyway).
    pub truncated: bool,
    location: [u8; LOCATION_MAX],
    location_len: usize,
}

#[allow(dead_code)]
impl Response {
    /// Where a redirect (3xx) points - reported, never followed: the new
    /// place may be another host, with another certificate to trust and no
    /// business seeing this request's Authorization.
    pub fn location(&self) -> Option<&str> {
        let bytes = self.location.get(..self.location_len)?;
        if bytes.is_empty() {
            return None;
        }
        core::str::from_utf8(bytes).ok()
    }
}

/// Headers this code writes itself. A second copy from the caller would give
/// the server two answers to which host, or how long the body is - and a
/// `Transfer-Encoding` would announce chunks that never come.
#[allow(dead_code)]
const OWN_HEADERS: [&str; 4] = ["Host", "Connection", "Content-Length", "Transfer-Encoding"];

/// Writes the request head - request line, Host, User-Agent, `Connection:
/// close`, Authorization, Content-Type, Content-Length and `headers` - into
/// `buf` and returns its length; `payload_len` bytes of body go right after
/// it. Built whole, so it leaves with one write instead of one per piece.
#[allow(dead_code)]
pub fn build(
    buf: &mut [u8],
    method: Method,
    url: &Url<'_>,
    auth: Auth<'_>,
    content_type: Option<&str>,
    headers: &[(&str, &str)],
    payload_len: usize,
) -> Result<usize, HttpError> {
    let mut o = Out { buf, n: 0 };
    o.put(method.as_bytes())?;
    o.put(b" ")?;
    if !url.target.starts_with('/') {
        o.put(b"/")?;
    }
    o.put(url.target.as_bytes())?;
    o.put(b" HTTP/1.1\r\nHost: ")?;
    o.put(url.host.as_bytes())?;
    if !url.default_port() {
        o.put(b":")?;
        o.number(usize::from(url.port))?;
    }
    o.put(b"\r\n")?;
    o.header(b"User-Agent", USER_AGENT.as_bytes())?;
    o.header(b"Connection", b"close")?;
    match auth {
        Auth::None => {}
        Auth::Bearer(token) if is_value(token) => {
            o.put(b"Authorization: Bearer ")?;
            o.put(token.as_bytes())?;
            o.put(b"\r\n")?;
        }
        Auth::Raw(value) if is_value(value) => o.header(b"Authorization", value.as_bytes())?,
        Auth::Basic(user, password) if !user.contains(':') => {
            o.put(b"Authorization: Basic ")?;
            o.base64(user, password)?;
            o.put(b"\r\n")?;
        }
        _ => return Err(HttpError::Header),
    }
    if let Some(kind) = content_type {
        if !is_value(kind) {
            return Err(HttpError::Header);
        }
        o.header(b"Content-Type", kind.as_bytes())?;
    }
    if payload_len > 0 || method.sends_length() {
        o.put(b"Content-Length: ")?;
        o.number(payload_len)?;
        o.put(b"\r\n")?;
    }
    for (name, value) in headers {
        let own = OWN_HEADERS.iter().any(|own| name.eq_ignore_ascii_case(own));
        if own || !is_token(name.as_bytes()) || !is_value(value) {
            return Err(HttpError::Header);
        }
        o.header(name.as_bytes(), value.as_bytes())?;
    }
    o.put(b"\r\n")?;
    Ok(o.n)
}

/// AWS IoT's longest topic, in bytes.
#[allow(dead_code)]
pub const AWS_TOPIC_MAX: usize = 256;

/// The URL of AWS IoT's HTTPS publish - `https://<host>[:<port>]/topics/
/// <topic>?qos=<qos>` - written into `out`; its length back. The topic goes
/// percent-encoded byte by byte (its UTF-8), all but RFC 3986's unreserved
/// characters and the `/` between levels, which stays as AWS's own examples
/// send it. What AWS refuses is refused here, before anything is sent
/// (`Url`): an empty topic or one over 256 bytes, more than 7 `/` (a Basic
/// Ingest `$aws/rules/<rule>/` not counted), an MQTT wildcard (`+`, `#`), a
/// NUL, a `$` topic other than AWS's own `$aws/...`, a QoS other than 0 or
/// 1. `TooLarge` when `out` cannot hold it all.
#[allow(dead_code)]
pub fn aws_url(out: &mut [u8], host: &str, port: u16, topic: &str, qos: u8) -> Result<usize, HttpError> {
    let mut o = Out { buf: out, n: 0 };
    aws_target(&mut o, host, port, topic, qos)?;
    Ok(o.n)
}

/// A topic AWS IoT takes to publish on: see `aws_url`.
#[allow(dead_code)]
fn aws_topic_ok(topic: &str) -> bool {
    let t = topic.as_bytes();
    let counted = match topic.strip_prefix("$aws/rules/").and_then(|r| r.split_once('/')) {
        Some((_, rest)) => rest,
        None => topic,
    };
    !t.is_empty()
        && t.len() <= AWS_TOPIC_MAX
        && counted.bytes().filter(|&b| b == b'/').count() <= 7
        && !t.iter().any(|&b| matches!(b, b'+' | b'#' | 0))
        && (t[0] != b'$' || topic.starts_with("$aws/"))
}

/// `aws_url`'s text, into `o`.
#[allow(dead_code)]
fn aws_target(o: &mut Out<'_>, host: &str, port: u16, topic: &str, qos: u8) -> Result<(), HttpError> {
    let host_ok = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'));
    if !host_ok || port == 0 || !aws_topic_ok(topic) || qos > 1 {
        return Err(HttpError::Url);
    }
    o.put(b"https://")?;
    o.put(host.as_bytes())?;
    if port != 443 {
        o.put(b":")?;
        o.number(usize::from(port))?;
    }
    o.put(b"/topics/")?;
    o.percent(topic.as_bytes(), true)?;
    o.put(b"?qos=")?;
    o.put(&[b'0' + qos])
}

/// One message for AWS IoT's HTTPS publish, with what MQTT 5 adds to it -
/// AWS takes it as the URL's query and three headers. `new` gives the topic
/// and the payload; the rest is optional:
///
///     let m = AwsMessage { retain: true, ..AwsMessage::new("my-thing/state", b"{\"on\":true}") };
#[allow(dead_code)]
#[derive(Clone, Copy)]
pub struct AwsMessage<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
    /// 0 or 1; `None` = the tab's (AWS_QOS).
    pub qos: Option<u8>,
    /// Kept by the broker for whoever subscribes later - an empty payload
    /// deletes the topic's. Needs iot:RetainPublish; never on a `$` topic.
    pub retain: bool,
    /// MQTT 5's Content Type ("application/json"): at most 256 bytes.
    pub content_type: Option<&'a str>,
    /// Seconds until the broker drops it undelivered: AWS raises 0 to 1 and
    /// lowers anything over 604800 (7 days) to that.
    pub message_expiry_s: Option<u32>,
    /// Where an answer should go: a topic name, no wildcard (`+`, `#`).
    pub response_topic: Option<&'a str>,
    /// What an answer carries back to be matched to this message.
    pub correlation_data: Option<&'a [u8]>,
    /// Name-value pairs, in order; a name may repeat.
    pub user_properties: &'a [(&'a str, &'a str)],
    /// The payload is UTF-8 text (MQTT 5's Payload Format Indicator).
    pub utf8: bool,
}

#[allow(dead_code)]
impl<'a> AwsMessage<'a> {
    pub const fn new(topic: &'a str, payload: &'a [u8]) -> Self {
        Self {
            topic,
            payload,
            qos: None,
            retain: false,
            content_type: None,
            message_expiry_s: None,
            response_topic: None,
            correlation_data: None,
            user_properties: &[],
            utf8: false,
        }
    }
}

/// AWS's limits on MQTT 5's Content Type, and on Correlation Data and the
/// user properties (as JSON).
#[allow(dead_code)]
const AWS_CONTENT_TYPE_MAX: usize = 256;
#[allow(dead_code)]
const AWS_PROPERTIES_MAX: usize = 8192;

/// `m`'s URL - `aws_url`'s, then `&retain=true`, `&contentType=`,
/// `&messageExpiry=`, `&responseTopic=` as `m` has them, each value
/// percent-encoded (a `/` too, as AWS's SDKs send a query) - into `out`; its
/// length back. `Url` for what AWS refuses (see `aws_url`, and a retained
/// `$` topic, a Content Type over 256 bytes, a response topic with a
/// wildcard or none at all, a payload marked UTF-8 that is not).
#[allow(dead_code)]
pub fn aws_message_url(out: &mut [u8], host: &str, port: u16, m: &AwsMessage<'_>, qos: u8) -> Result<usize, HttpError> {
    let bad_type = m.content_type.is_some_and(|t| t.len() > AWS_CONTENT_TYPE_MAX);
    // AWS's rule for a response topic: no wildcard. An empty one, or a NUL,
    // is no topic name at all.
    let bad_response = m
        .response_topic
        .is_some_and(|t| t.is_empty() || t.bytes().any(|b| matches!(b, b'+' | b'#' | 0)));
    let bad_text = m.utf8 && core::str::from_utf8(m.payload).is_err();
    if (m.retain && m.topic.starts_with('$')) || bad_type || bad_response || bad_text {
        return Err(HttpError::Url);
    }
    let mut o = Out { buf: out, n: 0 };
    aws_target(&mut o, host, port, m.topic, m.qos.unwrap_or(qos))?;
    if m.retain {
        o.put(b"&retain=true")?;
    }
    if let Some(kind) = m.content_type {
        o.put(b"&contentType=")?;
        o.percent(kind.as_bytes(), false)?;
    }
    if let Some(s) = m.message_expiry_s {
        o.put(b"&messageExpiry=")?;
        o.number(s as usize)?;
    }
    if let Some(topic) = m.response_topic {
        o.put(b"&responseTopic=")?;
        o.percent(topic.as_bytes(), false)?;
    }
    Ok(o.n)
}

/// The MQTT 5 headers of AWS's HTTPS publish, their values in `values`.
#[allow(dead_code)]
pub struct AwsHeaders<'v> {
    list: [(&'static str, &'v str); 3],
    n: usize,
}

#[allow(dead_code)]
impl<'v> AwsHeaders<'v> {
    pub fn as_slice(&self) -> &[(&'static str, &'v str)] {
        &self.list[..self.n]
    }
}

/// `m`'s headers: x-amz-mqtt5-user-properties (its pairs as a JSON array of
/// one-pair objects, base64), x-amz-mqtt5-correlation-data (base64),
/// x-amz-mqtt5-payload-format-indicator (UTF8_DATA) - each only when `m`
/// has it, the values written into `values`. `TooLarge` past `values` or
/// AWS's 8 KB.
#[allow(dead_code)]
pub fn aws_message_headers<'v>(values: &'v mut [u8], m: &AwsMessage<'_>) -> Result<AwsHeaders<'v>, HttpError> {
    let mut o = Out { buf: values, n: 0 };
    let mut ends = [0usize; 2];
    if !m.user_properties.is_empty() {
        let mut b = Base64 { out: &mut o, group: [0; 3], k: 0, fed: 0 };
        b.feed(b"[")?;
        for (i, (name, value)) in m.user_properties.iter().enumerate() {
            b.feed(if i == 0 { b"{" } else { b",{" })?;
            b.json_string(name)?;
            b.feed(b":")?;
            b.json_string(value)?;
            b.feed(b"}")?;
        }
        b.feed(b"]")?;
        if b.fed > AWS_PROPERTIES_MAX {
            return Err(HttpError::TooLarge);
        }
        b.finish()?;
    }
    ends[0] = o.n;
    if let Some(data) = m.correlation_data {
        if data.len() > AWS_PROPERTIES_MAX {
            return Err(HttpError::TooLarge);
        }
        let mut b = Base64 { out: &mut o, group: [0; 3], k: 0, fed: 0 };
        b.feed(data)?;
        b.finish()?;
    }
    ends[1] = o.n;
    let values: &'v [u8] = o.buf;
    let text = |at: core::ops::Range<usize>| core::str::from_utf8(&values[at]).map_err(|_| HttpError::Header);
    let mut list = [("", ""); 3];
    let mut n = 0;
    if !m.user_properties.is_empty() {
        list[n] = ("x-amz-mqtt5-user-properties", text(0..ends[0])?);
        n += 1;
    }
    if m.correlation_data.is_some() {
        list[n] = ("x-amz-mqtt5-correlation-data", text(ends[0]..ends[1])?);
        n += 1;
    }
    if m.utf8 {
        list[n] = ("x-amz-mqtt5-payload-format-indicator", "UTF8_DATA");
        n += 1;
    }
    Ok(AwsHeaders { list, n })
}

/// Bytes fed in, base64 (RFC 4648, padded) out, three at a time - so what is
/// encoded is never held whole anywhere.
#[allow(dead_code)]
struct Base64<'o, 'b> {
    out: &'o mut Out<'b>,
    group: [u8; 3],
    k: usize,
    /// Bytes fed so far.
    fed: usize,
}

#[allow(dead_code)]
impl Base64<'_, '_> {
    fn feed(&mut self, bytes: &[u8]) -> Result<(), HttpError> {
        for &b in bytes {
            self.group[self.k] = b;
            self.k += 1;
            self.fed += 1;
            if self.k == 3 {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), HttpError> {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        if self.k == 0 {
            return Ok(());
        }
        for b in self.group.iter_mut().skip(self.k) {
            *b = 0;
        }
        let v = (u32::from(self.group[0]) << 16) | (u32::from(self.group[1]) << 8) | u32::from(self.group[2]);
        let mut quad = [b'='; 4];
        for (i, q) in quad.iter_mut().enumerate().take(self.k + 1) {
            *q = ALPHABET[((v >> (18 - 6 * i)) & 63) as usize];
        }
        self.k = 0;
        self.out.put(&quad)
    }

    fn finish(mut self) -> Result<(), HttpError> {
        self.flush()
    }

    /// `s` as a JSON string: quoted, `"` and `\` escaped, control
    /// characters as \u00XX.
    fn json_string(&mut self, s: &str) -> Result<(), HttpError> {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        self.feed(b"\"")?;
        for &b in s.as_bytes() {
            match b {
                b'"' => self.feed(b"\\\"")?,
                b'\\' => self.feed(b"\\\\")?,
                0..=0x1f => self.feed(&[b'\\', b'u', b'0', b'0', HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]])?,
                _ => self.feed(&[b])?,
            }
        }
        self.feed(b"\"")
    }
}

/// The request head as it is built: `buf[..n]` so far.
#[allow(dead_code)]
struct Out<'b> {
    buf: &'b mut [u8],
    n: usize,
}

#[allow(dead_code)]
impl Out<'_> {
    fn put(&mut self, bytes: &[u8]) -> Result<(), HttpError> {
        let end = self.n + bytes.len();
        self.buf
            .get_mut(self.n..end)
            .ok_or(HttpError::TooLarge)?
            .copy_from_slice(bytes);
        self.n = end;
        Ok(())
    }

    fn number(&mut self, mut n: usize) -> Result<(), HttpError> {
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        self.put(&digits[i..])
    }

    /// `user:password` in base64 (RFC 7617), three bytes at a time, so the
    /// pair is never copied whole anywhere.
    fn base64(&mut self, user: &str, password: &str) -> Result<(), HttpError> {
        const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bytes = user.bytes().chain(core::iter::once(b':')).chain(password.bytes());
        loop {
            let mut group = [0u8; 3];
            let mut k = 0;
            while k < 3 {
                match bytes.next() {
                    Some(b) => {
                        group[k] = b;
                        k += 1;
                    }
                    None => break,
                }
            }
            if k == 0 {
                return Ok(());
            }
            let v = (u32::from(group[0]) << 16) | (u32::from(group[1]) << 8) | u32::from(group[2]);
            let mut quad = [b'='; 4];
            for (i, q) in quad.iter_mut().enumerate().take(k + 1) {
                *q = ALPHABET[((v >> (18 - 6 * i)) & 63) as usize];
            }
            self.put(&quad)?;
            if k < 3 {
                return Ok(());
            }
        }
    }

    fn header(&mut self, name: &[u8], value: &[u8]) -> Result<(), HttpError> {
        self.put(name)?;
        self.put(b": ")?;
        self.put(value)?;
        self.put(b"\r\n")
    }

    /// `bytes` percent-encoded but for RFC 3986's unreserved characters - and
    /// `/` when `slash` (a path; a query value encodes it).
    fn percent(&mut self, bytes: &[u8], slash: bool) -> Result<(), HttpError> {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        for &b in bytes {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') || (slash && b == b'/') {
                self.put(&[b])?;
            } else {
                self.put(&[b'%', HEX[usize::from(b >> 4)], HEX[usize::from(b & 15)]])?;
            }
        }
        Ok(())
    }
}

/// A header name: RFC 9110 token characters.
#[allow(dead_code)]
fn is_token(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

/// A header value: nothing that would end the line, and with it the header
/// (CR or LF would let a value write headers of its own).
#[allow(dead_code)]
fn is_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// How the body ends (RFC 9112 6.3).
#[allow(dead_code)]
#[derive(Clone, Copy)]
enum Framing {
    /// HEAD, 204, 304: no body whatever the headers say.
    None,
    Length(usize),
    Chunked,
    /// Neither: the body ends when the server closes.
    Close,
}

/// What the head says.
#[allow(dead_code)]
struct Head {
    status: u16,
    length: Option<usize>,
    /// A Transfer-Encoding header was there...
    coded: bool,
    /// ...and its last coding is chunked.
    chunked: bool,
}

/// Reads the answer to a request of `method` into `body`: interim 1xx
/// answers skipped, the body framed by RFC 9112 6.3. It stops at the end of
/// the body or once `body` is full - never on a guess - so every way out is
/// a byte the server sent, or its close.
#[allow(dead_code)]
pub async fn receive<C: embedded_io_async::Read>(
    c: &mut C,
    method: Method,
    body: &mut [u8],
) -> Result<Response, HttpError> {
    let mut head = [0u8; HEAD_MAX];
    let mut have = 0;
    let mut interim = 0u8;
    let mut response = Response {
        status: 0,
        len: 0,
        truncated: false,
        location: [0; LOCATION_MAX],
        location_len: 0,
    };
    loop {
        let end = loop {
            if let Some(end) = head_end(&head[..have]) {
                break end;
            }
            if have == HEAD_MAX {
                return Err(HttpError::HeadTooLarge);
            }
            let n = c.read(&mut head[have..]).await.map_err(|_| HttpError::Io)?;
            if n == 0 {
                return Err(HttpError::Closed);
            }
            have += n;
        };
        let h = parse_head(&head[..end], &mut response)?;
        if (100..200).contains(&h.status) {
            // 101 would switch protocols - never asked for.
            if h.status == 101 || interim == INTERIM_MAX {
                return Err(HttpError::BadResponse);
            }
            interim += 1;
            // The real answer may already be in the buffer, right behind.
            head.copy_within(end..have, 0);
            have -= end;
            response.location_len = 0;
            continue;
        }
        response.status = h.status;
        let framing = if method == Method::Head || h.status == 204 || h.status == 304 {
            Framing::None
        } else if h.coded {
            // Transfer-Encoding wins over Content-Length (RFC 9112 6.3): a
            // server that sends both is read the way it actually frames.
            if h.chunked {
                Framing::Chunked
            } else {
                Framing::Close
            }
        } else {
            h.length.map_or(Framing::Close, Framing::Length)
        };
        let mut src = Src {
            buf: &mut head,
            pos: end,
            end: have,
            c,
        };
        match framing {
            Framing::None => {}
            Framing::Length(n) => take(&mut src, body, &mut response, Some(n)).await?,
            Framing::Close => take(&mut src, body, &mut response, None).await?,
            Framing::Chunked => chunked(&mut src, body, &mut response).await?,
        }
        return Ok(response);
    }
}

/// Just past the first empty line (CRLF CRLF; a bare LF is taken too).
#[allow(dead_code)]
fn head_end(buf: &[u8]) -> Option<usize> {
    let mut i = 0;
    while let Some(p) = buf[i..].iter().position(|&b| b == b'\n') {
        let lf = i + p;
        match buf[lf + 1..] {
            [b'\n', ..] => return Some(lf + 2),
            [b'\r', b'\n', ..] => return Some(lf + 3),
            _ => i = lf + 1,
        }
    }
    None
}

#[allow(dead_code)]
fn parse_head(head: &[u8], response: &mut Response) -> Result<Head, HttpError> {
    let mut lines = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let status = match *lines.next().ok_or(HttpError::BadResponse)? {
        [b'H', b'T', b'T', b'P', b'/', b'1', b'.', minor, b' ', a, b, c, ref rest @ ..]
            if minor.is_ascii_digit()
                && [a, b, c].iter().all(u8::is_ascii_digit)
                && rest.first().is_none_or(|&s| s == b' ') =>
        {
            u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0')
        }
        _ => return Err(HttpError::BadResponse),
    };
    if !(100..=599).contains(&status) {
        return Err(HttpError::BadResponse);
    }
    let mut h = Head {
        status,
        length: None,
        coded: false,
        chunked: false,
    };
    for line in lines {
        if line.is_empty() {
            break;
        }
        // obs-fold (a line that continues the previous one): refused, as
        // RFC 9112 5.2 allows - it is how one header hides inside another.
        if line[0] == b' ' || line[0] == b'\t' {
            return Err(HttpError::BadResponse);
        }
        let colon = line
            .iter()
            .position(|&b| b == b':')
            .ok_or(HttpError::BadResponse)?;
        let name = &line[..colon];
        let value = trim(&line[colon + 1..]);
        // A space before the colon makes the name fail here too (RFC 9112 5.1).
        if !is_token(name) {
            return Err(HttpError::BadResponse);
        }
        if name.eq_ignore_ascii_case(b"content-length") {
            // "5" or the same number repeated ("5, 5"); anything else is
            // refused - two lengths are two ways to read where the body ends.
            for part in value.split(|&b| b == b',') {
                let n = decimal(trim(part)).ok_or(HttpError::BadResponse)?;
                if h.length.is_some_and(|m| m != n) {
                    return Err(HttpError::BadResponse);
                }
                h.length = Some(n);
            }
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            for coding in value.split(|&b| b == b',') {
                let coding = trim(coding);
                if !coding.is_empty() {
                    h.coded = true;
                    h.chunked = coding.eq_ignore_ascii_case(b"chunked");
                }
            }
        } else if name.eq_ignore_ascii_case(b"location") {
            if value.len() <= LOCATION_MAX {
                response.location[..value.len()].copy_from_slice(value);
                response.location_len = value.len();
            } else {
                response.location_len = 0;
            }
        }
    }
    Ok(h)
}

#[allow(dead_code)]
fn trim(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

/// A decimal number that fits a `usize`; `None` for anything else.
#[allow(dead_code)]
fn decimal(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    s.iter().try_fold(0usize, |n, &b| {
        if !b.is_ascii_digit() {
            return None;
        }
        n.checked_mul(10)?.checked_add(usize::from(b - b'0'))
    })
}

/// The bytes after the head (already in its buffer) first, then the connection.
#[allow(dead_code)]
struct Src<'b, C> {
    buf: &'b mut [u8],
    pos: usize,
    end: usize,
    c: &'b mut C,
}

#[allow(dead_code)]
impl<C: embedded_io_async::Read> Src<'_, C> {
    /// Up to `out.len()` (> 0) bytes; 0 = the server closed. Every caller
    /// stops at a 0: embassy-net answers each read past the end with 0 at
    /// once, so reading on would spin and starve the executor.
    async fn read(&mut self, out: &mut [u8]) -> Result<usize, HttpError> {
        if self.pos == self.end {
            if out.len() >= self.buf.len() {
                return self.c.read(out).await.map_err(|_| HttpError::Io);
            }
            self.end = self.c.read(self.buf).await.map_err(|_| HttpError::Io)?;
            self.pos = 0;
        }
        let n = out.len().min(self.end - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }

    async fn byte(&mut self) -> Result<u8, HttpError> {
        let mut b = [0u8];
        match self.read(&mut b).await? {
            0 => Err(HttpError::Closed),
            _ => Ok(b[0]),
        }
    }

    async fn exact(&mut self, mut out: &mut [u8]) -> Result<(), HttpError> {
        while !out.is_empty() {
            let n = self.read(out).await?;
            if n == 0 {
                return Err(HttpError::Closed);
            }
            out = &mut out[n..];
        }
        Ok(())
    }
}

/// `Some(n)`: exactly n body bytes; `None`: until the server closes.
#[allow(dead_code)]
async fn take<C: embedded_io_async::Read>(
    src: &mut Src<'_, C>,
    out: &mut [u8],
    r: &mut Response,
    mut left: Option<usize>,
) -> Result<(), HttpError> {
    loop {
        let want = left.unwrap_or(usize::MAX);
        if want == 0 {
            return Ok(());
        }
        let room = out.len() - r.len;
        if room == 0 {
            // Full. A length says whether more was coming; without one, one
            // more byte does (the server closes right after the body).
            r.truncated = left.is_some() || src.read(&mut [0u8]).await? > 0;
            return Ok(());
        }
        let n = src.read(&mut out[r.len..r.len + room.min(want)]).await?;
        if n == 0 {
            return match left {
                Some(_) => Err(HttpError::Closed),
                None => Ok(()),
            };
        }
        r.len += n;
        left = left.map(|l| l - n);
    }
}

/// `size [; extensions] CRLF data CRLF` ... `0 CRLF [trailers] CRLF`.
#[allow(dead_code)]
async fn chunked<C: embedded_io_async::Read>(
    src: &mut Src<'_, C>,
    out: &mut [u8],
    r: &mut Response,
) -> Result<(), HttpError> {
    loop {
        let mut size = 0usize;
        let mut digits = 0;
        let mut b = src.byte().await?;
        while let Some(d) = (b as char).to_digit(16) {
            // More hex digits than a usize holds would overflow it.
            if digits == 2 * core::mem::size_of::<usize>() {
                return Err(HttpError::BadResponse);
            }
            size = (size << 4) | d as usize;
            digits += 1;
            b = src.byte().await?;
        }
        if digits == 0 {
            return Err(HttpError::BadResponse);
        }
        // Whitespace, then `;name=value` extensions - ignored - up to LF.
        let mut ext = false;
        let mut n = 0;
        while b != b'\n' {
            match b {
                b';' => ext = true,
                b'\r' | b' ' | b'\t' => {}
                _ if b < 0x20 || b == 0x7f || !ext => return Err(HttpError::BadResponse),
                _ => {}
            }
            n += 1;
            if n > LINE_MAX {
                return Err(HttpError::BadResponse);
            }
            b = src.byte().await?;
        }
        if size == 0 {
            break;
        }
        let keep = size.min(out.len() - r.len);
        src.exact(&mut out[r.len..r.len + keep]).await?;
        r.len += keep;
        if keep < size {
            r.truncated = true;
            return Ok(());
        }
        let mut b = src.byte().await?;
        if b == b'\r' {
            b = src.byte().await?;
        }
        if b != b'\n' {
            return Err(HttpError::BadResponse);
        }
    }
    // Trailer fields, ignored, up to the empty line.
    let mut total = 0;
    loop {
        let mut line = 0;
        loop {
            let b = src.byte().await?;
            total += 1;
            if total > TRAILERS_MAX {
                return Err(HttpError::BadResponse);
            }
            match b {
                b'\n' => break,
                b'\r' => {}
                _ => line += 1,
            }
        }
        if line == 0 {
            return Ok(());
        }
    }
}
