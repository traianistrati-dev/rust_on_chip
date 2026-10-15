//! RustOnChip's "Test from this PC" (IoT tab, MQTT over TLS).
//!
//! The board's own certificate checks - `tls_verify.rs`, the very text the
//! firmware's `mqtt_link.rs` carries - run on this PC against the broker, then
//! the MQTT 5 CONNECT the firmware sends, each of its subscriptions, and one
//! test publication: what a broker's policy (AWS IoT's) refuses shows here,
//! not as a board that silently reconnects. The IDE writes this program,
//! builds it once, and runs it with the request on stdin: one hex-encoded
//! value per line - host, port, server name, verify ("1"/"0"), the CAs as DER,
//! client id, user name, password, AWS IoT's ALPN ("1"/"0"), the client
//! certificate and its key as PEM (empty: none), the keep-alive, the filters
//! subscribed to (one per line), and the topic of the test publication
//! (empty: none). It prints `INFO ...` lines, then one `RESULT ok ...` or
//! `RESULT fail ...`.
//!
//! `csr` as its argument: a P-256 key and a certificate request for it.
//! stdin: country, organization, common name, and a key as PEM (empty: make
//! one). It prints `KEY <hex PEM>` (a key it made), `CSR <hex PEM>`, then the
//! `RESULT` line.
//!
//! `http` as its argument: one request as the firmware's `http.rs` makes it
//! - a GET, or a POST as `post()` and `aws_publish()` send it - for https://
//! the same handshake and checks as above, nothing for http:// - and the
//! answer measured against the board's buffers: a head the board would
//! refuse, a redirect it would not follow, a body it would cut, a client
//! certificate refused, show here before flashing. stdin: the URL, verify
//! ("1"/"0"), the CAs as DER (http_ca.rs's), "P-384 chains" ("1"/"0"), the
//! board's HEAD_MAX and BODY_MAX (decimal), its User-Agent, its
//! REQUEST_HEAD_MAX (0: unchecked); then, each absent from an older IDE's
//! request: the method ("POST", else a GET), the Content-Type, the payload,
//! the client certificate and its key as PEM, the ALPN, a host:port to dial
//! instead of the URL's (a look-alike on this PC), AWS IoT ("1"/"0"), and
//! more headers (`Name: value` one a line).
//! It prints `INFO ...` lines, then the `RESULT` line.
#![allow(dead_code)]

use std::cell::Cell;
use std::io::BufRead;
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use embedded_io_adapters::std::FromStd;
use embedded_tls::blocking::TlsConnection;
use embedded_tls::{Aes128GcmSha256, Certificate, SignatureScheme, TlsConfig, TlsContext};

/// The certificates' dates are checked against this PC's clock. (The board
/// checks them only once SNTP has the time.)
type CertClock = std::time::SystemTime;
/// The longest certificate chain the broker may send - as on the board.
const MAX_CHAIN: usize = 8192;
/// The TLS record buffers, as on the board: a whole 16 KiB record in.
const RECORD_RX: usize = 16_640;
const RECORD_TX: usize = 2048;

include!("tls_verify.rs");
include!("pem_der.rs");
include!("csr_der.rs");

struct Request {
    host: String,
    port: u16,
    server_name: String,
    verify: bool,
    ca: Vec<u8>,
    client_id: String,
    user: String,
    password: String,
    alpn: bool,
    client_cert: String,
    client_key: String,
    keep_alive: u16,
    subscribe: Vec<String>,
    test_topic: String,
}

fn unhex(line: &str) -> Vec<u8> {
    let line = line.trim();
    (0..line.len() / 2)
        .filter_map(|i| u8::from_str_radix(&line[2 * i..2 * i + 2], 16).ok())
        .collect()
}

fn stdin_values() -> Vec<Vec<u8>> {
    let stdin = std::io::stdin();
    stdin.lock().lines().map_while(Result::ok).map(|l| unhex(&l)).collect()
}

fn read_request() -> Option<Request> {
    let lines = stdin_values();
    let text = |i: usize| lines.get(i).map(|b| String::from_utf8_lossy(b).into_owned());
    Some(Request {
        host: text(0)?,
        port: text(1)?.parse().ok()?,
        server_name: text(2)?,
        verify: text(3)? == "1",
        ca: lines.get(4)?.clone(),
        client_id: text(5)?,
        user: text(6)?,
        password: text(7)?,
        alpn: text(8)? == "1",
        client_cert: text(9)?,
        client_key: text(10)?,
        // Absent from an older IDE's request: the CONNECT alone.
        keep_alive: text(11).and_then(|k| k.parse().ok()).unwrap_or(60),
        subscribe: text(12)
            .map(|s| s.lines().filter(|l| !l.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default(),
        test_topic: text(13).unwrap_or_default(),
    })
}

/// The `http` mode's request: a URL and what the board's `http.rs` holds.
struct HttpRequest {
    url: String,
    verify: bool,
    ca: Vec<u8>,
    /// "P-384 chains" is on: the board checks a chain signed with P-384.
    p384: bool,
    /// The board's buffer for the response head, and for the body.
    head_max: usize,
    body_max: usize,
    user_agent: String,
    /// The board's buffer for the request head; 0: not checked.
    request_head_max: usize,
    /// POST `payload` (as `aws_publish()` and `post()` send it) instead of
    /// a GET; `content_type` "" = no Content-Type header.
    post: bool,
    content_type: String,
    payload: Vec<u8>,
    /// The board's certificate and key as PEM, for the server that asks -
    /// empty: none.
    client_cert: String,
    client_key: String,
    /// The ALPN the hello names ("" = none): x-amzn-http-ca for AWS IoT on 443.
    alpn: String,
    /// `host:port` to connect to instead of the URL's - the name, the check
    /// and the Host header stay the URL's (a look-alike on this PC).
    connect_to: String,
    /// The URL is AWS IoT's HTTPS publish: refusals said in its terms.
    aws: bool,
    /// More headers, `Name: value` one a line - after Content-Length, as the
    /// board's `build()` writes a request's own (AWS's x-amz-mqtt5-*).
    headers: Vec<(String, String)>,
}

fn read_http_request() -> Option<HttpRequest> {
    let lines = stdin_values();
    let text = |i: usize| lines.get(i).map(|b| String::from_utf8_lossy(b).into_owned());
    Some(HttpRequest {
        url: text(0)?,
        verify: text(1)? == "1",
        ca: lines.get(2)?.clone(),
        p384: text(3)? == "1",
        head_max: text(4)?.parse().ok()?,
        body_max: text(5)?.parse().ok()?,
        user_agent: text(6)?,
        // Absent from an older IDE's request: not checked - and, below, a
        // GET without a certificate, as before.
        request_head_max: text(7).and_then(|v| v.parse().ok()).unwrap_or(0),
        post: text(8).is_some_and(|m| m == "POST"),
        content_type: text(9).unwrap_or_default(),
        payload: lines.get(10).cloned().unwrap_or_default(),
        client_cert: text(11).unwrap_or_default(),
        client_key: text(12).unwrap_or_default(),
        alpn: text(13).unwrap_or_default(),
        connect_to: text(14).unwrap_or_default(),
        aws: text(15).is_some_and(|a| a == "1"),
        headers: text(16)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(':').map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned())))
            .collect(),
    })
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("csr") {
        match csr() {
            Ok(what) => println!("RESULT ok {what}"),
            Err(why) => println!("RESULT fail {why}"),
        }
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("http") {
        let done = match read_http_request() {
            Some(request) => http(&request),
            None => Err("the IDE sent no readable request".into()),
        };
        match done {
            Ok(what) => println!("RESULT ok {what}"),
            Err(why) => println!("RESULT fail {why}"),
        }
        return;
    }
    let Some(request) = read_request() else {
        println!("RESULT fail the IDE sent no readable request");
        return;
    };
    match run(&request) {
        Ok(what) => println!("RESULT ok {what}"),
        Err(why) => println!("RESULT fail {why}"),
    }
}

/// The firmware's verifier, and what this PC also wants to know: whether the
/// chain needs P-384 (the board checks it only with "P-384 chains").
struct HostVerifier<'a> {
    inner: Verifier<'a>,
    p384: bool,
    /// The broker's certificate arrived (a failure before it is in the hello).
    chain_seen: bool,
}

impl embedded_tls::TlsVerifier<Aes128GcmSha256> for HostVerifier<'_> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), embedded_tls::TlsError> {
        self.inner.set_hostname_verification(hostname)
    }

    fn verify_certificate(
        &mut self,
        transcript: &embedded_tls::Sha256,
        chain: embedded_tls::CertificateRef,
    ) -> Result<(), embedded_tls::TlsError> {
        // Only what the board checks: the chain up to the CA it ends in.
        let (_, keep) = self.inner.anchor(&chain);
        self.p384 |= chain
            .entries
            .iter()
            .take(keep)
            .any(|e| matches!(e, embedded_tls::CertificateEntryRef::X509(der) if signed_with_p384(der)));
        self.chain_seen = true;
        self.inner.verify_certificate(transcript, chain)
    }

    fn verify_signature(&mut self, verify: embedded_tls::CertificateVerifyRef) -> Result<(), embedded_tls::TlsError> {
        self.p384 |= matches!(verify.signature_scheme, embedded_tls::SignatureScheme::EcdsaSecp384r1Sha384);
        self.inner.verify_signature(verify)
    }
}

/// `true` when the certificate was signed with ecdsa-with-SHA384 (a P-384 key).
fn signed_with_p384(cert: &[u8]) -> bool {
    let Some((0x30, cert, _)) = tlv(cert) else { return false };
    let Some((_, _, after_tbs)) = tlv(cert) else { return false };
    let Some((0x30, algorithm, _)) = tlv(after_tbs) else { return false };
    matches!(
        tlv(algorithm),
        Some((0x06, [0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x03], _))
    )
}

/// As the board's, plus what this PC reports: did the broker ask for the
/// client certificate, and was the handshake signed with its key.
struct Provider<'v, 'a> {
    rng: rand_core::OsRng,
    verifier: Option<&'v mut HostVerifier<'a>>,
    client: Option<(&'v [u8], [u8; 32])>,
    asked: &'v Cell<bool>,
    signed: &'v Cell<bool>,
}

impl embedded_tls::CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl embedded_tls::TlsVerifier<Aes128GcmSha256>, embedded_tls::TlsError> {
        self.verifier.as_deref_mut().ok_or(embedded_tls::TlsError::Unimplemented)
    }

    fn signer(&mut self) -> Result<(impl ecdsa::signature::SignerMut<Self::Signature>, SignatureScheme), embedded_tls::TlsError> {
        let (_, key) = self.client.ok_or(embedded_tls::TlsError::Unimplemented)?;
        self.signed.set(true);
        Ok((ClientKey::new(key)?, SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        // Called only when the broker asked.
        self.asked.set(true);
        self.client.map(|(cert, _)| Certificate::X509(cert))
    }
}

/// The board's identity from the PEM text the tab holds, read by the
/// board's own code (`pem_der.rs`) - and, as the build would, refused when it
/// does not hold together. The key's public point, when the key lacks one, is
/// worked out here and compared too.
fn client_identity_of(cert_pem: &str, key_pem: &str) -> Result<Option<(Vec<u8>, [u8; 32])>, String> {
    let (cert_text, key_text) = (cert_pem.as_bytes(), key_pem.as_bytes());
    if blank(cert_text) || blank(key_text) {
        return Ok(None);
    }
    let mut cert = vec![0u8; pem_len(cert_text, b"CERTIFICATE", false)];
    pem_decode(cert_text, b"CERTIFICATE", false, &mut cert);
    let label = key_label(key_text);
    let mut key = vec![0u8; pem_len(key_text, label, false)];
    pem_decode(key_text, label, false, &mut key);
    let Some((d, point)) = ec_key(&key) else {
        return Err("MQTT_CLIENT_KEY in secrets.rs is not a P-256 key - the board would not build".into());
    };
    if !cert_is_p256(&cert) {
        return Err("MQTT_CLIENT_CERT in secrets.rs does not carry a P-256 key - the board would not build".into());
    }
    let Some(cert_point) = cert_ec_point(&cert) else {
        // Stored compressed: not matched here.
        return Ok(Some((cert, d)));
    };
    let point = match point {
        Some(p) => p,
        None => {
            let secret = p256::SecretKey::from_bytes(&d.into()).map_err(|_| "MQTT_CLIENT_KEY is not a usable P-256 key".to_string())?;
            use p256::elliptic_curve::sec1::ToEncodedPoint as _;
            let encoded = secret.public_key().to_encoded_point(false);
            encoded.as_bytes().try_into().map_err(|_| "a P-256 point of another length".to_string())?
        }
    };
    if point != cert_point {
        return Err("MQTT_CLIENT_CERT in secrets.rs is not the certificate of MQTT_CLIENT_KEY: the broker would refuse the board".into());
    }
    Ok(Some((cert, d)))
}

/// Who answers at the other end, in the messages: MQTT's broker, its CAs in
/// mqtt_ca.rs, or an HTTP server, its CAs in http_ca.rs.
#[derive(Clone, Copy)]
struct Peer {
    /// "the broker".
    the: &'static str,
    /// "a broker".
    a: &'static str,
    /// The file the board's CAs are in.
    ca_file: &'static str,
    /// One that speaks TLS 1.2 only - the board speaks TLS 1.3.
    tls12: &'static str,
    /// Where an EC CA for an RSA chain is the usual slip.
    ec_ca: &'static str,
}

const BROKER: Peer = Peer {
    the: "the broker",
    a: "a broker",
    ca_file: "mqtt_ca.rs",
    tls12: "an AWS IoT endpoint without a TLS 1.3 security policy",
    ec_ca: " - AWS IoT's ends in Amazon Root CA 1",
};

const SERVER: Peer = Peer {
    the: "the server",
    a: "a server",
    ca_file: "http_ca.rs",
    tls12: "httpbin.org",
    ec_ca: "",
};

/// AWS IoT's HTTPS publish: the endpoint, checked against http_ca.rs.
const AWS_HTTPS: Peer = Peer {
    the: "AWS IoT",
    a: "an endpoint",
    ca_file: "http_ca.rs",
    tls12: "an AWS IoT endpoint without a TLS 1.3 security policy",
    ec_ca: " - AWS IoT's ends in Amazon Root CA 1",
};

/// What a refused handshake means, in the user's terms.
fn explain(e: &embedded_tls::TlsError, peer: Peer) -> String {
    use embedded_tls::TlsError as E;
    let Peer { the, a, ca_file, tls12, .. } = peer;
    match e {
        E::InvalidCertificate => format!(
            "{the}'s certificate did not check out: its chain does not end in {ca_file}'s CA, it does not name the server name, or it is out of date"
        ),
        E::InvalidSignatureScheme | E::InvalidSignature => {
            "a signature the board cannot check: a key or curve embedded-tls does not take".into()
        }
        E::HandshakeAborted(..) => {
            format!("{the} refused the handshake: it may speak only TLS 1.2 ({tls12} answers like this)")
        }
        E::Io(_) | E::IoError | E::ConnectionClosed => format!(
            "the connection broke during the handshake - {a} that speaks only TLS 1.2 ({tls12}) hangs up like this"
        ),
        _ => "the handshake failed".into(),
    }
}

/// Why a broker hangs up right after a handshake in which it asked for the
/// board's certificate.
const REFUSED_CERT: &str = "the broker closed the connection after the handshake: it refused the client certificate. On AWS IoT: the certificate is not registered or not ACTIVE, no policy attached to it allows iot:Connect for this client id, the thing is not attached to it, or the client id is not the thing's name";

/// Resolves `host` - its first IPv4 address, as the board's DNS query asks
/// for no other - and connects to it.
fn connect(host: &str, port: u16) -> Result<TcpStream, String> {
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("{host} could not be resolved: {e}"))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("{host} has no IPv4 address"))?;
    println!("INFO resolved {address}");
    let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| format!("no TCP connection to {address}: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(15)));
    println!("INFO TCP connected");
    Ok(tcp)
}

/// What a handshake is made with: the name the certificate must carry, the
/// board's CAs, and what else the board's ClientHello carries.
struct Dial<'r> {
    server_name: &'r str,
    verify: bool,
    ca: &'r [u8],
    alpn: Option<&'r [&'r [u8]]>,
    /// The client certificate and its key as PEM - empty: none.
    client_cert: &'r str,
    client_key: &'r str,
    peer: Peer,
}

/// A connection through its handshake, and what the handshake showed.
struct Opened<'b> {
    tls: Tls<'b>,
    /// The server asked for the client certificate.
    asked: bool,
    /// A client certificate is set - sent when it was asked for.
    identity: bool,
    /// The checked chain needs P-384 (`INFO p384` printed).
    p384: bool,
}

/// The board's handshake over `tcp`, with its own certificate checks
/// (`tls_verify.rs`): what happened as `INFO` lines, a refusal in the user's
/// terms. MQTT and HTTP open their connections alike, so one tests both.
fn handshake<'b>(tcp: TcpStream, rx: &'b mut [u8], tx: &'b mut [u8], dial: &Dial<'_>) -> Result<Opened<'b>, String> {
    let Peer { the, ca_file, ec_ca, .. } = dial.peer;
    let mut tls: Tls<'b> = TlsConnection::new(FromStd::new(tcp), rx, tx);
    let ip = dial.server_name.parse::<Ipv4Addr>().is_ok();
    let config = if ip {
        TlsConfig::new()
    } else {
        TlsConfig::new().with_server_name(dial.server_name)
    };
    let config = match dial.alpn {
        Some(alpn) => config.with_alpn(alpn),
        None => config,
    };
    if dial.verify && ip {
        return Err("the server name is an address, and a certificate names DNS names".into());
    }
    let identity = client_identity_of(dial.client_cert, dial.client_key)?;
    if identity.is_some() {
        println!("INFO client certificate and key belong together (P-256)");
    }
    let (asked, signed) = (Cell::new(false), Cell::new(false));
    let mut verifier = HostVerifier { inner: Verifier::new(dial.ca), p384: false, chain_seen: false };
    let provider = Provider {
        rng: rand_core::OsRng,
        verifier: if dial.verify { Some(&mut verifier) } else { None },
        client: identity.as_ref().map(|(cert, d)| (cert.as_slice(), *d)),
        asked: &asked,
        signed: &signed,
    };
    let opened = tls.open(TlsContext::new(&config, provider));
    if let Err(e) = opened {
        // embedded-tls reads the broker's certificate request too, and some
        // list a signature it does not know: then nothing of the chain came.
        if dial.verify && !verifier.chain_seen && matches!(e, embedded_tls::TlsError::DecodeError) {
            return Err(format!("embedded-tls could not read {the}'s handshake - likely its request for a client certificate ({e:?})"));
        }
        // A CA whose key is not the kind the chain is signed with (an EC CA
        // for an RSA chain) fails to parse as that key, not as a bad chain.
        if dial.verify && verifier.chain_seen && !verifier.inner.chain_ok && matches!(e, embedded_tls::TlsError::DecodeError) {
            return Err(format!(
                "{the}'s certificate did not check out: its chain is signed with another kind of key than {ca_file}'s CA holds (an EC CA for an RSA chain{ec_ca}) ({e:?})"
            ));
        }
        return Err(format!("{} ({e:?})", explain(&e, dial.peer)));
    }
    if dial.verify {
        if !verifier.inner.authenticated {
            return Err(format!("{the} finished the handshake without proving a certificate"));
        }
        println!(
            "INFO certificate checked: its chain ends in a CA of {ca_file}, it names {}, and it is in date by this PC's clock",
            dial.server_name
        );
        if verifier.p384 {
            println!("INFO p384");
        }
    } else {
        println!("INFO encrypted, the certificate not checked (encrypt only)");
    }
    match (asked.get(), identity.is_some()) {
        (true, true) if signed.get() => println!("INFO {the} asked for the client certificate: sent, and the handshake signed with its key"),
        (true, false) => println!("INFO {the} asked for a client certificate, and none is set"),
        (false, true) => println!("INFO {the} did not ask for the client certificate, so it was not sent"),
        _ => {}
    }
    Ok(Opened {
        tls,
        asked: asked.get(),
        identity: identity.is_some(),
        p384: dial.verify && verifier.p384,
    })
}

fn run(r: &Request) -> Result<String, String> {
    let tcp = connect(&r.host, r.port)?;
    let mut rx = vec![0u8; RECORD_RX];
    let mut tx = vec![0u8; RECORD_TX];
    const AWS_ALPN: &[&[u8]] = &[b"x-amzn-mqtt-ca"];
    let dial = Dial {
        server_name: &r.server_name,
        verify: r.verify,
        ca: &r.ca,
        alpn: r.alpn.then_some(AWS_ALPN),
        client_cert: &r.client_cert,
        client_key: &r.client_key,
        peer: BROKER,
    };
    let Opened { mut tls, asked, identity: has_identity, .. } = handshake(tcp, &mut rx, &mut tx, &dial)?;
    // What a hang-up from here on means - a broker that asked for the
    // board's certificate and then closes is refusing it. (Silence is not a
    // hang-up.)
    let refused = |e: String| {
        // A read that times out is `TimedOut` on Windows, `Other` (from
        // `WouldBlock`) on Linux and macOS.
        if !asked || e.contains("TimedOut") || e.contains("Io(Other)") {
            e
        } else if !has_identity {
            format!("the broker wants a client certificate and hangs up without one: switch \"Client certificate\" on and set it in secrets.rs ({e})")
        } else {
            format!("{REFUSED_CERT} ({e})")
        }
    };

    // MQTT 5 CONNECT, as the board sends it. The credentials only to a
    // broker whose certificate was checked.
    let credentials = r.verify;
    // Encrypt only sends no login from here, and the board sends
    // secrets.rs's: what a broker refuses this PC then says nothing of it.
    let login_untested = !credentials && !(r.user.is_empty() && r.password.is_empty());
    if !r.client_id.is_empty() {
        println!(
            "INFO connecting as `{}`: a board online with this client id is dropped once, and reconnects",
            r.client_id
        );
    }
    let packet = connect_packet(&r.client_id, r.keep_alive, credentials.then_some((r.user.as_str(), r.password.as_str())));
    send(&mut tls, &packet).map_err(|e| refused(format!("the CONNECT could not be sent: {e}")))?;
    let (kind, connack) = read_packet(&mut tls, "a CONNACK").map_err(|e| silent_443(r, refused(e)))?;
    if kind != 0x20 {
        return Err(format!("the broker answered something else than a CONNACK (0x{kind:02X})"));
    }
    let Some(&reason) = connack.get(1) else {
        return Err("a CONNACK too short to read".into());
    };
    // The terms the broker set (a refusal may carry them too).
    let terms = connack.get(2..).and_then(property_list).unwrap_or_default();
    let assigned = terms
        .iter()
        .find(|(p, _)| *p == 0x12)
        .map(|(_, v)| String::from_utf8_lossy(v).into_owned());
    let untested = if has_identity && !asked {
        " - the client certificate untested: this broker never asks for one (test.mosquitto.org asks on 8884 only)"
    } else {
        ""
    };
    match reason {
        0x00 => {}
        // The broker asked for the certificate and still refuses, with the
        // login (if any) sent: its policy - AWS IoT's.
        code @ (0x87 | 0x05) if asked && has_identity && !login_untested => {
            let id = if r.client_id.is_empty() || assigned.is_some() {
                " The client id is empty, so the broker made one up - no AWS IoT policy allows it: set it to the thing's name."
            } else {
                ""
            };
            return Err(format!("the broker refused the CONNECT: not authorized (0x{code:02X}).{id} {AWS_CONNECT}"));
        }
        // A login refused that was never sent: the board sends secrets.rs's
        // whatever the check, so TLS is all this could test.
        code @ (0x04 | 0x05 | 0x86 | 0x87) if login_untested => {
            return Ok(format!(
                "TLS works; the broker wants a login (0x{code:02X}), which encrypt only does not send from this PC - the board sends secrets.rs's, so turn the check on to test it"
            ));
        }
        code => return Err(format!("the broker refused the CONNECT: {} (0x{code:02X})", connack_reason(code))),
    }
    // The board pings within both keep-alives, its own and the broker's.
    if let Some(k) = u16_property(&terms, 0x13)
        && k != r.keep_alive
    {
        println!("INFO the broker settled the keep-alive at {k} s (asked {} s): the board pings in time for both", r.keep_alive);
    }
    if let Some(id) = &assigned {
        println!("INFO the broker assigned the client id `{id}` (the board sends none)");
    }
    let max_qos = terms.iter().find(|(p, _)| *p == 0x24).and_then(|(_, v)| v.first().copied());
    let (subscribe, test_topic): (&[String], &str) = if login_untested && (!r.subscribe.is_empty() || !r.test_topic.is_empty()) {
        println!("INFO the subscriptions and the test publication are left out: encrypt only sends no login from this PC, and the board logs in - turn the check on to test them");
        (&[], "")
    } else {
        (r.subscribe.as_slice(), r.test_topic.as_str())
    };

    // Each subscription, one at a time as the board makes them: a policy that
    // refuses one answers here, where the board says nothing.
    let mut refusals = Vec::new();
    let mut problem_info = false;
    for (i, filter) in subscribe.iter().enumerate() {
        let id = u16::try_from(i + 1).unwrap_or(u16::MAX);
        let at = format!("the subscription to `{filter}`");
        send(&mut tls, &subscribe_packet(id, filter)).map_err(|e| Wait::Ended(e).explain(&at, AWS_SUBSCRIBE_LOST))?;
        let suback = read_until(&mut tls, 0x90, id, "a SUBACK").map_err(|w| w.explain(&at, AWS_SUBSCRIBE_LOST))?;
        let (props, codes) = suback.get(2..).and_then(split_properties).ok_or("a SUBACK too short to read")?;
        // The board asks for no "problem information"; rust-mqtt ends its
        // connection on a reason string in a SUBACK all the same.
        problem_info |= props.iter().any(|(p, _)| *p == 0x1F);
        match codes.first() {
            Some(&code) if code >= 0x80 => refusals.push(format!(
                "{at}: {} (0x{code:02X}){}",
                ack_reason(code),
                if code == 0x87 { AWS_SUBSCRIBE } else { "" }
            )),
            Some(_) => println!("INFO subscribed to `{filter}`"),
            None => return Err("a SUBACK without its reason code".into()),
        }
    }

    // One test message under the board's own name (AWS IoT: where the tab's
    // policy allows it), QoS 1 for the broker's answer.
    let mut published = false;
    if !test_topic.is_empty() {
        if max_qos == Some(0) {
            println!("INFO the broker takes QoS 0 only: the test publication is left out");
        } else {
            let at = format!("the publication on `{test_topic}`");
            const PUBLISH_ID: u16 = 0x7E57;
            send(&mut tls, &publish_packet(test_topic, PUBLISH_ID, b"RustOnChip: Test from this PC"))
                .map_err(|e| Wait::Ended(e).explain(&at, AWS_PUBLISH_LOST))?;
            let puback = read_until(&mut tls, 0x40, PUBLISH_ID, "a PUBACK").map_err(|w| w.explain(&at, AWS_PUBLISH_LOST))?;
            match puback.get(2).copied().unwrap_or(0x00) {
                code if code >= 0x80 => refusals.push(format!(
                    "{at}: {} (0x{code:02X}){}",
                    ack_reason(code),
                    if code == 0x87 { AWS_PUBLISH } else { "" }
                )),
                _ => {
                    println!("INFO published a test message on `{test_topic}` (QoS 1): the broker took it");
                    published = true;
                }
            }
        }
    }
    let _ = tls.write(&[0xE0, 0x00]);
    let _ = tls.flush();

    if let Some(first) = refusals.first() {
        let more = if refusals.len() > 1 { format!(" (and {} more: {})", refusals.len() - 1, refusals[1..].join("; ")) } else { String::new() };
        return Err(format!("the broker took the connection but refused {first}{more}"));
    }
    if problem_info {
        return Err("the broker answers a subscription with a reason string the board did not ask for: rust-mqtt 0.6 ends the connection on it, so the board would reconnect for ever".into());
    }
    let mut did = Vec::new();
    match subscribe.len() {
        0 => {}
        1 => did.push("the subscription".to_owned()),
        n => did.push(format!("all {n} subscriptions")),
    }
    if published {
        did.push("a publication".to_owned());
    }
    let did = if did.is_empty() { String::new() } else { format!(", then {}", did.join(" and ")) };
    Ok(if credentials {
        format!("the broker accepted the connection (CONNACK: success){did}{untested}")
    } else {
        format!("the broker accepted the connection without credentials (encrypt only sends none from here){did}{untested}")
    })
}

/// What a CONNACK 0x87 means when the broker asked for the board's
/// certificate - AWS IoT's policy check.
const AWS_CONNECT: &str = "On AWS IoT: no policy attached to the certificate allows iot:Connect for this client id, the thing is not attached to it, or the client id is not the thing's name - a policy change takes up to 8 minutes";
const AWS_SUBSCRIBE: &str = " - on AWS IoT the policy needs iot:Subscribe on topicfilter/<this filter>";
const AWS_SUBSCRIBE_LOST: &str = "AWS IoT ends it over a reserved `$` topic or a filter it cannot take (a policy's refusal is a SUBACK 0x87 instead)";
const AWS_PUBLISH: &str = " - on AWS IoT the policy needs iot:Publish on topic/<this topic>; the board publishes at QoS 0, where AWS ends the connection over the same refusal";
const AWS_PUBLISH_LOST: &str = "AWS IoT ends it over a topic it cannot take (a policy's refusal at QoS 1 is a PUBACK 0x87 instead)";

/// A broker on 443 that never answers: AWS IoT's HTTPS side, which a
/// ClientHello without its ALPN reaches.
fn silent_443(r: &Request, e: String) -> String {
    if r.port == 443 && !r.alpn && (e.contains("TimedOut") || e.contains("Io(Other)")) {
        format!("{e} - on 443 AWS IoT takes MQTT with a client certificate only by the ALPN x-amzn-mqtt-ca, which the IDE adds for an AWS endpoint name; elsewhere use 8883")
    } else {
        e
    }
}

/// Why an acknowledgement did not come: the connection ended, or it is
/// still open and nothing came in time.
enum Wait {
    Ended(String),
    Silent(String),
}

impl Wait {
    /// A read that times out is `TimedOut` on Windows, `Other` (from
    /// `WouldBlock`) on Linux and macOS: the connection is still there.
    fn of(e: String) -> Self {
        if e.contains("TimedOut") || e.contains("Io(Other)") {
            Wait::Silent(e)
        } else {
            Wait::Ended(e)
        }
    }

    fn explain(self, at: &str, aws: &str) -> String {
        match self {
            // A DISCONNECT says why itself.
            Wait::Ended(e) if e.starts_with("DISCONNECT") => format!("the broker ended the connection at {at} ({e})"),
            Wait::Ended(e) => format!("the broker ended the connection at {at} - {aws} ({e})"),
            Wait::Silent(e) => format!("no answer came to {at}, the connection still open ({e})"),
        }
    }
}

type Tls<'a> = TlsConnection<'a, FromStd<TcpStream>, Aes128GcmSha256>;

/// Writes all of `packet`: `write` takes what fits in one record (about
/// 1.9 KB), and a long token as the password needs more than one.
fn send(tls: &mut Tls<'_>, packet: &[u8]) -> Result<(), String> {
    let mut rest = packet;
    while !rest.is_empty() {
        match tls.write(rest) {
            Ok(0) => return Err("no room".into()),
            Ok(n) => rest = &rest[n..],
            Err(e) => return Err(format!("{e:?}")),
        }
    }
    tls.flush().map_err(|e| format!("{e:?}"))
}

fn read_byte(tls: &mut Tls<'_>, what: &str) -> Result<u8, String> {
    let mut b = [0u8; 1];
    match tls.read(&mut b) {
        Ok(1) => Ok(b[0]),
        Ok(_) => Err(format!("the broker closed the connection before {what}")),
        Err(e) => Err(format!("{what} did not come from the broker ({e:?})")),
    }
}

/// One MQTT packet: its first byte and its body.
fn read_packet(tls: &mut Tls<'_>, what: &str) -> Result<(u8, Vec<u8>), String> {
    let kind = read_byte(tls, what)?;
    let mut len = 0usize;
    for shift in [0, 7, 14, 21] {
        let b = read_byte(tls, what)?;
        len |= usize::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            break;
        }
    }
    // The CONNECT capped what the broker may send at the board's buffer.
    if len > 1 << 20 {
        return Err(format!("{what} longer than any packet the board takes ({len} bytes)"));
    }
    let mut body = vec![0u8; len];
    let mut got = 0;
    while got < len {
        match tls.read(&mut body[got..]) {
            Ok(0) => return Err(format!("the broker closed the connection in the middle of {what}")),
            Ok(n) => got += n,
            Err(e) => return Err(format!("{what} did not come whole ({e:?})")),
        }
    }
    Ok((kind, body))
}

/// Reads until the acknowledgement of type `ack` (its first byte) for packet
/// `id`, passing over what else arrives - publications on a filter just
/// subscribed, in any number for 15 s; a few of anything else. A DISCONNECT
/// is the broker's answer instead.
fn read_until(tls: &mut Tls<'_>, ack: u8, id: u16, what: &str) -> Result<Vec<u8>, Wait> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut others = 0;
    while std::time::Instant::now() < deadline {
        let (kind, body) = read_packet(tls, what).map_err(Wait::of)?;
        if kind == ack && body.get(..2) == Some(&id.to_be_bytes()[..]) {
            return Ok(body);
        }
        if kind == 0xE0 {
            let code = body.first().copied().unwrap_or(0x00);
            return Err(Wait::Ended(format!("DISCONNECT: {} (0x{code:02X})", ack_reason(code))));
        }
        if kind & 0xF0 != 0x30 {
            others += 1;
            if others > 64 {
                return Err(Wait::Silent(format!("{what} did not come among 64 other packets")));
            }
        }
    }
    Err(Wait::Silent(format!("{what} did not come within 15 s - publications on the subscriptions kept coming")))
}

/// An MQTT 5 CONNECT as the board's rust-mqtt sends it: clean start, its
/// keep-alive, and the properties Receive Maximum 1, Maximum Packet Size
/// 1024 (the board's buffer) and Request Problem Information 0.
fn connect_packet(client_id: &str, keep_alive: u16, credentials: Option<(&str, &str)>) -> Vec<u8> {
    let mut flags = 0x02u8;
    let mut body = vec![0x00, 0x04, b'M', b'Q', b'T', b'T', 0x05];
    let mut payload = Vec::new();
    field(&mut payload, client_id.as_bytes());
    if let Some((user, password)) = credentials {
        if !user.is_empty() {
            flags |= 0x80;
            field(&mut payload, user.as_bytes());
        }
        if !password.is_empty() {
            flags |= 0x40;
            field(&mut payload, password.as_bytes());
        }
    }
    body.push(flags);
    body.extend_from_slice(&keep_alive.to_be_bytes());
    body.extend_from_slice(&[10, 0x21, 0x00, 0x01, 0x27, 0x00, 0x00, 0x04, 0x00, 0x17, 0x00]);
    body.extend_from_slice(&payload);
    packet(0x10, &body)
}

/// A SUBSCRIBE of one filter at QoS 0, as the board makes it.
fn subscribe_packet(id: u16, filter: &str) -> Vec<u8> {
    let mut body = id.to_be_bytes().to_vec();
    body.push(0x00);
    field(&mut body, filter.as_bytes());
    body.push(0x00);
    packet(0x82, &body)
}

/// A PUBLISH at QoS 1, so the broker answers it.
fn publish_packet(topic: &str, id: u16, payload: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    field(&mut body, topic.as_bytes());
    body.extend_from_slice(&id.to_be_bytes());
    body.push(0x00);
    body.extend_from_slice(payload);
    packet(0x32, &body)
}

/// A string or binary field: its length, then it.
fn field(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&u16::try_from(s.len()).unwrap_or(u16::MAX).to_be_bytes());
    out.extend_from_slice(s);
}

/// The fixed header - the remaining length as a variable byte integer - then
/// the body.
fn packet(first: u8, body: &[u8]) -> Vec<u8> {
    let mut packet = vec![first];
    let mut len = body.len();
    loop {
        let mut byte = (len % 128) as u8;
        len /= 128;
        if len > 0 {
            byte |= 0x80;
        }
        packet.push(byte);
        if len == 0 {
            break;
        }
    }
    packet.extend_from_slice(body);
    packet
}

/// A variable byte integer, and what follows it.
fn varint(b: &[u8]) -> Option<(usize, &[u8])> {
    let mut n = 0usize;
    for (i, &byte) in b.iter().enumerate().take(4) {
        n |= usize::from(byte & 0x7F) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((n, &b[i + 1..]));
        }
    }
    None
}

/// A property list (its length first): what follows it too.
fn split_properties(b: &[u8]) -> Option<(Vec<(u8, &[u8])>, &[u8])> {
    let (n, rest) = varint(b)?;
    Some((properties(rest.get(..n)?)?, rest.get(n..)?))
}

fn property_list(b: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    split_properties(b).map(|(p, _)| p)
}

/// MQTT 5 properties: (identifier, value) - numbers as their bytes, strings
/// and binary data without their length. `None` = one this does not know.
fn properties(mut p: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let len16 = |b: &[u8]| -> Option<usize> { Some(usize::from(u16::from_be_bytes([*b.first()?, *b.get(1)?]))) };
    let mut out = Vec::new();
    while let Some((&id, rest)) = p.split_first() {
        let (skip, take) = match id {
            0x01 | 0x17 | 0x19 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2A => (0, 1),
            0x13 | 0x21 | 0x22 | 0x23 => (0, 2),
            0x02 | 0x11 | 0x18 | 0x27 => (0, 4),
            0x0B => (0, rest.iter().position(|b| b & 0x80 == 0)? + 1),
            0x03 | 0x08 | 0x09 | 0x12 | 0x15 | 0x16 | 0x1A | 0x1C | 0x1F => (2, len16(rest)?),
            0x26 => {
                let key = len16(rest)?;
                (0, 2 + key + 2 + len16(rest.get(2 + key..)?)?)
            }
            _ => return None,
        };
        out.push((id, rest.get(skip..skip + take)?));
        p = &rest[skip + take..];
    }
    Some(out)
}

fn u16_property(props: &[(u8, &[u8])], id: u8) -> Option<u16> {
    let (_, v) = props.iter().find(|(p, _)| *p == id)?;
    Some(u16::from_be_bytes([*v.first()?, *v.get(1)?]))
}

/// A SUBACK's, PUBACK's or DISCONNECT's reason code.
fn ack_reason(code: u8) -> &'static str {
    match code {
        0x00 => "normal",
        0x80 => "an unspecified error",
        0x81 => "a malformed packet",
        0x82 => "a protocol error",
        0x83 => "refused by the broker's own rules",
        0x87 => "not authorized",
        0x89 => "the server is busy",
        0x8B => "the server is shutting down",
        0x8D => "the keep-alive ran out",
        0x8E => "another connection took the client id over",
        0x8F => "the topic filter is not valid",
        0x90 => "the topic name is not valid",
        0x91 => "the packet id is in use",
        0x93 => "too many messages in flight",
        0x94 => "a bad topic alias",
        0x95 => "packet too large",
        0x97 => "a quota is exceeded",
        0x98 => "an administrative action",
        0x9A => "retained messages are not taken",
        0x9B => "the QoS is not taken",
        0x9E => "shared subscriptions are not taken",
        0xA1 => "subscription identifiers are not taken",
        0xA2 => "wildcard subscriptions are not taken",
        _ => "another reason",
    }
}

fn connack_reason(code: u8) -> &'static str {
    match code {
        0x01 => "it speaks MQTT 3.1.1 only, and the board speaks MQTT 5",
        0x80 => "an unspecified error",
        0x84 => "it does not take MQTT 5",
        0x85 => "the client id is not valid",
        0x86 | 0x04 => "bad user name or password (secrets.rs)",
        0x87 | 0x05 => "not authorized",
        0x88 | 0x03 => "the server is unavailable",
        0x89 => "the server is busy",
        0x8A => "banned",
        0x8C => "a bad authentication method",
        0x95 => "packet too large",
        0x9F => "too many connections lately",
        _ => "another reason",
    }
}

/// The `csr` mode: a P-256 key (made here, or given) and its certificate
/// request, both as PEM on stdout - checked before they are printed: the
/// request's signature verifies with the key it names.
fn csr() -> Result<String, String> {
    use p256::ecdsa::signature::{Signer as _, Verifier as _};
    use p256::elliptic_curve::sec1::ToEncodedPoint as _;
    let lines = stdin_values();
    let text = |i: usize| lines.get(i).map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
    let (country, organization, common_name, key_pem) = (text(0), text(1), text(2), text(3));
    let subject = Subject {
        country: &country,
        organization: &organization,
        common_name: &common_name,
    };
    if let Some(why) = subject.problem() {
        return Err(why.into());
    }
    let made = blank(key_pem.as_bytes());
    let secret = if made {
        p256::SecretKey::random(&mut rand_core::OsRng)
    } else {
        let p = key_pem.as_bytes();
        let label = key_label(p);
        let mut der = vec![0u8; pem_len(p, label, false)];
        pem_decode(p, label, false, &mut der);
        let (d, _) = ec_key(&der).ok_or("the key in secrets.rs is not a P-256 key")?;
        p256::SecretKey::from_bytes(&d.into()).map_err(|_| "the key in secrets.rs is not a usable P-256 key")?
    };
    let encoded = secret.public_key().to_encoded_point(false);
    let point: [u8; 65] = encoded.as_bytes().try_into().map_err(|_| "a P-256 point of another length")?;
    let info = request_info(&subject, &point);
    let signing = p256::ecdsa::SigningKey::from(&secret);
    let signature: p256::ecdsa::DerSignature = signing.sign(&info);
    let request_der = request(&info, signature.as_bytes());
    // Checked before anything leaves: the signature over the info verifies
    // with the key the request names.
    p256::ecdsa::VerifyingKey::from(&signing)
        .verify(&info, &signature)
        .map_err(|_| "the request did not verify")?;
    let hex = |s: &str| s.bytes().map(|b| format!("{b:02x}")).collect::<String>();
    if made {
        let d: [u8; 32] = secret.to_bytes().as_slice().try_into().map_err(|_| "a P-256 key of another length")?;
        println!("KEY {}", hex(&to_pem("EC PRIVATE KEY", &sec1_key(&d, &point))));
    }
    println!("CSR {}", hex(&to_pem("CERTIFICATE REQUEST", &request_der)));
    Ok(if made { "a new P-256 key and its request".into() } else { "a request for the key in secrets.rs".into() })
}

// ── The `http` mode ──────────────────────────────────────────────────────

/// The longest host name the board takes: its TLS name check holds 64.
const HOST_MAX: usize = 64;
/// Interim answers (100 Continue, 103 Early Hints) the board passes over
/// before the real one.
const INTERIM_MAX: usize = 4;
/// The longest chunk-size line and trailer section the board reads.
const CHUNK_LINE_MAX: usize = 256;
const TRAILERS_MAX: usize = 1024;
/// How much of a head, and of a body, this PC reads to measure it: far past
/// the board's buffers, short of a download.
const HEAD_LIMIT: usize = 64 * 1024;
const BODY_LIMIT: usize = 1 << 20;

/// The `http` mode: one GET as the board's `http.rs` makes it, and the
/// answer held against the board's buffers. What the board would refuse, or
/// not follow, fails here; a body it would cut is a success that says so.
fn http(r: &HttpRequest) -> Result<String, String> {
    let url = Url::parse(&r.url)?;
    if r.user_agent.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err("the User-Agent holds a line break: the request would come apart".into());
    }
    if r.content_type.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
        return Err("the Content-Type holds a line break: the request would come apart".into());
    }
    let mut extra = String::new();
    for (name, value) in &r.headers {
        if name.is_empty() || name.bytes().any(|b| !(b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))) {
            return Err(format!("`{name}` is no header name"));
        }
        if value.bytes().any(|b| matches!(b, b'\r' | b'\n' | 0)) {
            return Err(format!("the {name} header holds a line break: the request would come apart"));
        }
        extra.push_str(&format!("{name}: {value}\r\n"));
    }
    let mut request = if r.post {
        url.post(&r.user_agent, &r.content_type, r.payload.len(), &extra)
    } else {
        url.get(&r.user_agent, &extra)
    }
    .into_bytes();
    if r.request_head_max > 0 && request.len() > r.request_head_max {
        return Err(format!(
            "the request head is {} bytes, over the board's {} - the board returns HttpError::TooLarge before connecting",
            request.len(),
            r.request_head_max
        ));
    }
    request.extend_from_slice(&r.payload);
    // A look-alike on this PC: dialled where it listens, named as the URL.
    let (host, port) = match r.connect_to.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() => {
            let port = port.parse::<u16>().map_err(|_| format!("`{}` is not host:port", r.connect_to))?;
            println!("INFO connecting to {host}:{port}, in the name of {}", url.host);
            (host, port)
        }
        _ => (url.host, url.port),
    };
    let tcp = connect(host, port)?;
    let mut rx = vec![0u8; RECORD_RX];
    let mut tx = vec![0u8; RECORD_TX];
    let alpn = [r.alpn.as_bytes()];
    let (mut conn, asked, identity) = if url.https {
        let dial = Dial {
            server_name: url.host,
            verify: r.verify,
            ca: &r.ca,
            alpn: (!r.alpn.is_empty()).then_some(&alpn[..]),
            client_cert: &r.client_cert,
            client_key: &r.client_key,
            peer: if r.aws { AWS_HTTPS } else { SERVER },
        };
        let opened = handshake(tcp, &mut rx, &mut tx, &dial)?;
        // This PC checks P-384 whatever the switch says; the board only with
        // it on, and fails the handshake without.
        if opened.p384 && !r.p384 {
            return Err("the server's chain is signed with P-384: switch \"P-384 chains\" on for HTTP".into());
        }
        (Conn::Tls(Box::new(opened.tls)), opened.asked, opened.identity)
    } else {
        println!("INFO plain HTTP: nothing encrypted, nothing checked");
        (Conn::Plain(tcp), false, false)
    };
    // A server that asked for the board's certificate and then hangs up is
    // refusing it - AWS IoT resets the connection, it answers no 403.
    let refused = |e: String| {
        if !asked || e.contains("TimedOut") || e.contains("Io(Other)") {
            e
        } else if !identity {
            format!(
                "the server asked for a client certificate and hung up without one: the board sends its certificate only to the host named under HTTP > Client certificate (Sent only to), once secrets.rs holds it ({e})"
            )
        } else if r.aws {
            format!("AWS IoT reset the connection after the handshake: it refused the board's certificate - not registered, not ACTIVE, or no policy attached to it ({e})")
        } else {
            format!("the server hung up after the handshake: it refused the board's certificate ({e})")
        }
    };
    conn.send(&request)
        .map_err(|e| refused(format!("the request could not be sent ({e})")))?;
    println!("INFO sent {} {}", if r.post { "POST" } else { "GET" }, url.path());
    if r.post {
        println!("INFO the payload: {} bytes", r.payload.len());
    }

    let mut src = Incoming { conn: &mut conn, buf: Vec::new(), pos: 0, ended: false, kept: Vec::new() };
    let mut interim = 0;
    let (head, size) = loop {
        let bytes = match src.head() {
            Ok(HeadRead::Whole(bytes)) => bytes,
            Ok(HeadRead::Ended(0)) => return Err(refused("the server closed the connection without an answer".into())),
            Ok(HeadRead::Ended(n)) => {
                return Err(format!("the server closed the connection {n} bytes into the answer's head - the board reports it cut short"));
            }
            Ok(HeadRead::TooLong) => {
                return Err(format!("the response head is over {HEAD_LIMIT} bytes, over the board's {} - it would refuse it", r.head_max));
            }
            Err(e) => return Err(refused(format!("no answer came ({e})"))),
        };
        let line = bytes.split(|&b| b == b'\n').next().unwrap_or_default();
        println!("INFO the server answered {}", String::from_utf8_lossy(line).trim_end());
        let size = bytes.len();
        // Before anything in it is read: the board's head buffer is full.
        if size > r.head_max {
            return Err(format!("the response head is {size} bytes, over the board's {} - it would refuse it", r.head_max));
        }
        let head = parse_head(&bytes).map_err(|why| format!("the answer does not read as HTTP/1.1: {why} - the board would refuse it"))?;
        if !(100..200).contains(&head.status) {
            println!("INFO the response head is {size} bytes - the board takes {}", r.head_max);
            break (head, size);
        }
        // 101 would switch protocols - the board never asks for that.
        if head.status == 101 {
            return Err("the server answered 101 Switching Protocols, which the board never asks for".into());
        }
        interim += 1;
        if interim > INTERIM_MAX {
            return Err(format!("the server sent more than {INTERIM_MAX} interim answers - the board stops at {INTERIM_MAX}"));
        }
        println!("INFO an interim answer: the board passes over it to the next");
    };

    let what = if head.reason.is_empty() {
        head.status.to_string()
    } else {
        format!("{} {}", head.status, head.reason)
    };
    // 304 Not Modified is no redirect (and a plain GET never gets one).
    if (300..400).contains(&head.status) && head.status != 304 {
        return Err(match &head.location {
            Some(to) => format!("redirected to {to} - the board reports redirects and does not follow them"),
            None => format!("the server answered {what}, a redirect without a Location - the board reports redirects and does not follow them"),
        });
    }
    // RFC 9112 6.3, as the board frames it: Transfer-Encoding wins over
    // Content-Length; neither, and the body runs until the server closes.
    let framing = if head.status == 204 || head.status == 304 {
        Framing::None
    } else if head.coded {
        if head.chunked { Framing::Chunked } else { Framing::Close }
    } else {
        head.length.map_or(Framing::Close, Framing::Length)
    };
    // AWS IoT's Device Shadow REST API, beside its HTTPS publish.
    let shadow = r.aws && url.path().starts_with("/things/");
    // A refusal says why in its body: read it first.
    if head.status >= 400 {
        // What came is kept even when the body then breaks off.
        let _ = read_body(&mut src, framing, r.body_max);
        let said = text_of(&src.kept, 200);
        let said = if said.is_empty() { String::new() } else { format!(": {said}") };
        if shadow {
            let (action, which) = if r.post { ("UpdateThingShadow", "update") } else { ("GetThingShadow", "read") };
            let resource = shadow_in(&url.path());
            return match head.status {
                // AWS looks for the shadow once the policy let the request
                // through: the certificate and the policy work.
                404 if !r.post => Ok(format!(
                    "AWS IoT answered {what}{said} - {resource} has no shadow yet: the certificate and the policy let the board {which} it, and shadow_report() creates it"
                )),
                401 | 403 => Err(format!(
                    "AWS IoT answered {what}{said} - the policy on the board's certificate allows no iot:{action} on {resource} (iot:Publish on $aws/things/... does not grant it)"
                )),
                _ => Err(format!("AWS IoT answered {what}{said}")),
            };
        }
        return Err(if r.aws && head.status == 403 {
            let retained = url.path().contains("retain=true");
            format!(
                "AWS IoT answered {what}{said} - the policy on the board's certificate allows no iot:Publish{} on topic/{}",
                if retained { " - or, for a retained message, no iot:RetainPublish -" } else { "" },
                topic_in(&url.path())
            )
        } else {
            format!("the server answered {what}{said}")
        });
    }
    match framing {
        Framing::None => println!("INFO no body: a {} has none", head.status),
        Framing::Length(n) => println!("INFO the body: Content-Length {n}"),
        Framing::Chunked => println!("INFO the body: chunked"),
        Framing::Close => println!("INFO the body: until the server closes (no Content-Length, not chunked)"),
    }
    let body = read_body(&mut src, framing, r.body_max)?;
    if shadow {
        println!("INFO {}: {}", if r.post { "the answer" } else { "the shadow" }, text_of(&src.kept, 480));
        let len = if body.exact { body.len.to_string() } else { format!("at least {}", body.len) };
        let size = if body.len > r.body_max {
            format!(
                "over the board's {} (Answer up to): shadow_get() gets it truncated, and what was cut reads None - raise it",
                r.body_max
            )
        } else {
            format!("the board keeps {} - all of it", r.body_max)
        };
        return if head.status == 200 && r.post {
            Ok(format!("AWS IoT answered {what}: {} took the update, {len} bytes back - {size}", shadow_in(&url.path())))
        } else if head.status == 200 {
            Ok(format!("AWS IoT answered {what} with the shadow of {}, {len} bytes - {size}", shadow_in(&url.path())))
        } else {
            Err(format!("AWS IoT answered {what}, not 200 - the board's shadow calls report it as it is"))
        };
    }
    if r.post && !src.kept.is_empty() {
        println!("INFO the answer: {}", text_of(&src.kept, 200));
    }
    if r.aws {
        // A 200 is AWS's only acknowledgement - its body varies.
        return if head.status == 200 {
            Ok(format!(
                "AWS IoT took the message on `{}` (HTTP {what}) - the board's aws_publish() gets the same",
                topic_in(&url.path())
            ))
        } else {
            Err(format!("AWS IoT answered {what}, not 200 - aws_publish() reports it as a refusal"))
        };
    }
    let len = if body.exact { body.len.to_string() } else { format!("at least {}", body.len) };
    let head_part = format!("head {size} bytes (the board takes {})", r.head_max);
    Ok(if body.len > r.body_max {
        println!(
            "INFO the body is {len} bytes, over the board's {}: it keeps the first {} and flags it truncated",
            r.body_max, r.body_max
        );
        format!(
            "HTTP {what} - {head_part}, body {len} bytes: the board keeps the first {} bytes and flags it truncated",
            r.body_max
        )
    } else {
        println!("INFO the body is {len} bytes - the board keeps {}", r.body_max);
        format!("HTTP {what} - {head_part}, body {len} bytes (the board keeps {})", r.body_max)
    })
}

/// A URL by the board's rules: `http[s]://host[:port][/path][?query]`, the
/// host a name or an IPv4 address. The fragment never leaves.
struct Url<'a> {
    https: bool,
    host: &'a str,
    port: u16,
    /// Path and query as given ("" = "/").
    target: &'a str,
}

impl<'a> Url<'a> {
    fn parse(url: &'a str) -> Result<Self, String> {
        let refused = |why: String| format!("the board refuses this URL: {why}");
        let (scheme, rest) = url
            .split_once("://")
            .ok_or_else(|| refused("it does not start with http:// or https://".into()))?;
        let https = if scheme.eq_ignore_ascii_case("https") {
            true
        } else if scheme.eq_ignore_ascii_case("http") {
            false
        } else {
            return Err(refused(format!("`{scheme}://` is neither http:// nor https://")));
        };
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, target) = rest.split_at(end);
        let target = target.split('#').next().unwrap_or("");
        // A login goes in a header, never in the URL.
        if authority.contains('@') {
            return Err(refused("it carries user info (user@host) - a login goes in a header".into()));
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, "")) => (host, None),
            Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => {
                let port = port.parse::<u16>().ok().filter(|&p| p != 0);
                (host, Some(port.ok_or_else(|| refused("its port is not one from 1 to 65535".into()))?))
            }
            Some((host, _)) if !host.contains(['[', ':']) => {
                return Err(refused("its port is not a number".into()));
            }
            _ => (authority, None),
        };
        if host.contains(['[', ']', ':']) {
            return Err(refused("its host is an IPv6 address, and the board's network speaks IPv4 only".into()));
        }
        let port = port.unwrap_or(if https { 443 } else { 80 });
        if host.is_empty() {
            return Err(refused("it names no host".into()));
        }
        let name = |b: u8| b.is_ascii_alphanumeric() || b == b'-' || b == b'.' || b == b'_';
        if !host.bytes().all(name) {
            return Err(refused(format!("its host `{host}` is not a name or an IPv4 address")));
        }
        if host.len() > HOST_MAX {
            return Err(refused(format!("its host is {} characters, more than the {HOST_MAX} it takes", host.len())));
        }
        // A space, CR or LF would end the request line early.
        if !target.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
            return Err(refused("its path holds a space, a line break or a character outside ASCII - percent-encode it".into()));
        }
        Ok(Url { https, host, port, target })
    }

    /// What the request line asks for.
    fn path(&self) -> String {
        if self.target.starts_with('/') {
            self.target.to_owned()
        } else {
            format!("/{}", self.target)
        }
    }

    /// The request the board sends for a GET: the port in Host only when it
    /// is not the scheme's, one connection per request.
    fn get(&self, user_agent: &str, extra: &str) -> String {
        format!("GET {} HTTP/1.1\r\n{}{extra}\r\n", self.path(), self.headers(user_agent))
    }

    /// The head the board sends for a POST of `len` bytes, in its `build()`'s
    /// order: Content-Type when given, Content-Length always.
    fn post(&self, user_agent: &str, content_type: &str, len: usize, extra: &str) -> String {
        let kind = if content_type.is_empty() {
            String::new()
        } else {
            format!("Content-Type: {content_type}\r\n")
        };
        format!(
            "POST {} HTTP/1.1\r\n{}{kind}Content-Length: {len}\r\n{extra}\r\n",
            self.path(),
            self.headers(user_agent)
        )
    }

    /// Host, User-Agent and Connection, as `build()` writes them.
    fn headers(&self, user_agent: &str) -> String {
        let default = self.port == if self.https { 443 } else { 80 };
        let port = if default { String::new() } else { format!(":{}", self.port) };
        format!(
            "Host: {}{port}\r\nUser-Agent: {user_agent}\r\nConnection: close\r\n",
            self.host
        )
    }
}

/// The first `max` characters of an answer's body as one line of text: what
/// a server says when it refuses (AWS: `{"message":...}`).
fn text_of(body: &[u8], max: usize) -> String {
    let text: String = String::from_utf8_lossy(body)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max)
        .collect();
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The topic in an AWS IoT publish path, percent-decoded: `/topics/a%20b?qos=1`
/// is `a b`.
fn topic_in(path: &str) -> String {
    let encoded = path.strip_prefix("/topics/").unwrap_or(path);
    decoded(encoded.split('?').next().unwrap_or_default())
}

/// The shadow a Device Shadow path asks for, as its policy resource names
/// it: `/things/a%3Ab/shadow?name=cfg` is `thing/a:b/cfg`.
fn shadow_in(path: &str) -> String {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let thing = path.strip_prefix("/things/").and_then(|p| p.strip_suffix("/shadow")).unwrap_or(path);
    let name = query.split('&').find_map(|p| p.strip_prefix("name="));
    match name {
        Some(name) => format!("thing/{}/{}", decoded(thing), decoded(name)),
        None => format!("thing/{}", decoded(thing)),
    }
}

/// `%XX` back to its byte.
fn decoded(encoded: &str) -> String {
    let encoded = encoded.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < encoded.len() {
        let hex = encoded
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (encoded[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The connection an answer comes over (TLS boxed: its state is a kilobyte).
enum Conn<'b> {
    Plain(TcpStream),
    Tls(Box<Tls<'b>>),
}

impl Conn<'_> {
    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            Conn::Plain(tcp) => std::io::Write::write_all(tcp, data)
                .and_then(|()| std::io::Write::flush(tcp))
                .map_err(|e| format!("Io({:?})", e.kind())),
            Conn::Tls(tls) => send(tls, data),
        }
    }

    /// Some bytes; 0 = the end. The server's close_notify, which embedded-tls
    /// 0.19 returns as an error, is the end too - as the board's `Closing`
    /// reads it. A TCP close without it stays an error (`IoError`).
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            Conn::Plain(tcp) => std::io::Read::read(tcp, buf).map_err(|e| format!("Io({:?})", e.kind())),
            Conn::Tls(tls) => match tls.read(buf) {
                Err(embedded_tls::TlsError::ConnectionClosed) => Ok(0),
                other => other.map_err(|e| format!("{e:?}")),
            },
        }
    }
}

/// The answer as it arrives: what was read and not taken yet first, then
/// the connection.
struct Incoming<'c, 'b> {
    conn: &'c mut Conn<'b>,
    buf: Vec<u8>,
    pos: usize,
    /// The connection ended: nothing more is read from it (embedded-tls
    /// refuses a read after the close_notify).
    ended: bool,
    /// The body's first bytes, as they passed: what a refusal says.
    kept: Vec<u8>,
}

enum HeadRead {
    /// Status line, headers and the empty line.
    Whole(Vec<u8>),
    /// The connection ended this many bytes into the head.
    Ended(usize),
    /// Longer than HEAD_LIMIT, and still no end.
    TooLong,
}

impl Incoming<'_, '_> {
    /// Reads more; `false` once the connection ended.
    fn more(&mut self) -> Result<bool, String> {
        if self.ended {
            return Ok(false);
        }
        self.buf.drain(..self.pos);
        self.pos = 0;
        let mut chunk = [0u8; 4096];
        let n = self.conn.read(&mut chunk)?;
        self.buf.extend_from_slice(&chunk[..n]);
        self.ended = n == 0;
        Ok(n > 0)
    }

    /// The next head, whatever its length: the board's limit is checked by
    /// the caller, which then can say how long it was.
    fn head(&mut self) -> Result<HeadRead, String> {
        loop {
            if let Some(end) = head_end(&self.buf[self.pos..]) {
                let head = self.buf[self.pos..self.pos + end].to_vec();
                self.pos += end;
                return Ok(HeadRead::Whole(head));
            }
            if self.buf.len() - self.pos > HEAD_LIMIT {
                return Ok(HeadRead::TooLong);
            }
            if !self.more()? {
                return Ok(HeadRead::Ended(self.buf.len() - self.pos));
            }
        }
    }

    /// Passes over up to `n` (> 0) bytes; 0 = the connection ended.
    fn skip(&mut self, n: usize) -> Result<usize, String> {
        if self.pos == self.buf.len() && !self.more()? {
            return Ok(0);
        }
        let k = n.min(self.buf.len() - self.pos);
        let room = 512usize.saturating_sub(self.kept.len()).min(k);
        self.kept.extend_from_slice(&self.buf[self.pos..self.pos + room]);
        self.pos += k;
        Ok(k)
    }

    /// One byte; `None` = the connection ended.
    fn byte(&mut self) -> Result<Option<u8>, String> {
        if self.pos == self.buf.len() && !self.more()? {
            return Ok(None);
        }
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(Some(b))
    }
}

/// Just past the head's empty line (CRLF CRLF; a bare LF taken too), where
/// the board finds it.
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

/// What the board reads of a head.
struct Head {
    status: u16,
    /// The reason phrase ("OK", "Not Found"); may be empty.
    reason: String,
    length: Option<usize>,
    /// A Transfer-Encoding came...
    coded: bool,
    /// ...and its last coding is chunked.
    chunked: bool,
    location: Option<String>,
}

/// A head by the board's rules: HTTP/1.x and a three-digit status, header
/// names that are tokens, no folded lines, one Content-Length (repeated at
/// most), Transfer-Encoding's last coding. `Err`: why the board refuses it.
fn parse_head(head: &[u8]) -> Result<Head, String> {
    let mut lines = head
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let first = lines.next().unwrap_or_default();
    let status = match *first {
        [b'H', b'T', b'T', b'P', b'/', b'1', b'.', minor, b' ', a, b, c, ref rest @ ..]
            if minor.is_ascii_digit()
                && [a, b, c].iter().all(u8::is_ascii_digit)
                && rest.first().is_none_or(|&s| s == b' ') =>
        {
            u16::from(a - b'0') * 100 + u16::from(b - b'0') * 10 + u16::from(c - b'0')
        }
        _ => return Err(format!("its status line `{}` is not HTTP/1.x's", String::from_utf8_lossy(first))),
    };
    if !(100..=599).contains(&status) {
        return Err(format!("{status} is no HTTP status"));
    }
    let reason = String::from_utf8_lossy(first.get(13..).unwrap_or_default()).trim().to_owned();
    let mut h = Head {
        status,
        reason,
        length: None,
        coded: false,
        chunked: false,
        location: None,
    };
    for line in lines {
        if line.is_empty() {
            break;
        }
        if line[0] == b' ' || line[0] == b'\t' {
            return Err("a header line folded onto the one before it".into());
        }
        let text = String::from_utf8_lossy(line);
        let colon = line
            .iter()
            .position(|&b| b == b':')
            .ok_or_else(|| format!("the header line `{text}` has no colon"))?;
        let (name, value) = (&line[..colon], trim(&line[colon + 1..]));
        // A space before the colon fails here too (RFC 9112 5.1).
        if !is_token(name) {
            return Err(format!("the header name `{}` is not a token", String::from_utf8_lossy(name)));
        }
        if name.eq_ignore_ascii_case(b"content-length") {
            // "5", or the same number repeated ("5, 5").
            for part in value.split(|&b| b == b',') {
                let n = decimal(trim(part)).ok_or_else(|| format!("`{text}` is not a length"))?;
                if h.length.is_some_and(|m| m != n) {
                    return Err("two Content-Lengths that differ".into());
                }
                h.length = Some(n);
            }
        } else if name.eq_ignore_ascii_case(b"transfer-encoding") {
            for coding in value.split(|&b| b == b',').map(trim).filter(|c| !c.is_empty()) {
                h.coded = true;
                h.chunked = coding.eq_ignore_ascii_case(b"chunked");
            }
        } else if name.eq_ignore_ascii_case(b"location") {
            h.location = Some(String::from_utf8_lossy(value).into_owned());
        }
    }
    Ok(h)
}

/// A header name: RFC 9110's token characters.
fn is_token(name: &[u8]) -> bool {
    !name.is_empty()
        && name
            .iter()
            .all(|&b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

fn trim(mut s: &[u8]) -> &[u8] {
    while let [b' ' | b'\t', rest @ ..] = s {
        s = rest;
    }
    while let [rest @ .., b' ' | b'\t'] = s {
        s = rest;
    }
    s
}

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

#[derive(Clone, Copy)]
enum Framing {
    /// 204, 304: no body whatever the headers say.
    None,
    Length(usize),
    Chunked,
    /// Neither: the body ends when the server closes.
    Close,
}

/// How long the body is.
struct Body {
    len: usize,
    /// `false`: at least `len` - this PC stopped reading where the board had
    /// long stopped (past BODY_LIMIT, or broken off after the board's buffer
    /// was full).
    exact: bool,
}

/// The body read as the board reads it, and measured. What breaks only
/// after the board's buffer is full does not matter to the board: it stops
/// there.
fn read_body(src: &mut Incoming<'_, '_>, framing: Framing, body_max: usize) -> Result<Body, String> {
    match framing {
        Framing::None => Ok(Body { len: 0, exact: true }),
        Framing::Length(n) => {
            // The board reads no further than its buffer; nor does this PC.
            let want = n.min(body_max);
            let mut got = 0;
            while got < want {
                match src.skip(want - got) {
                    Ok(0) => {
                        return Err(format!("the server closed the connection {got} bytes into a body of {n} - the board reports it cut short"));
                    }
                    Ok(k) => got += k,
                    Err(e) => return Err(format!("the body broke off after {got} of its {n} bytes ({e})")),
                }
            }
            Ok(Body { len: n, exact: true })
        }
        Framing::Chunked => {
            // The sizes say how long the body is; what came says whether the
            // board's buffer was full before a break.
            let (mut total, mut received) = (0, 0);
            match chunked(src, &mut total, &mut received) {
                Ok(whole) => Ok(Body { len: total, exact: whole }),
                Err(_) if received >= body_max => Ok(Body { len: total, exact: false }),
                Err(why) => Err(why),
            }
        }
        Framing::Close => {
            let mut got = 0;
            loop {
                match src.skip(usize::MAX) {
                    Ok(0) => return Ok(Body { len: got, exact: true }),
                    Ok(k) => got += k,
                    // The board took a byte past its buffer, and stopped.
                    Err(_) if got > body_max => return Ok(Body { len: got, exact: false }),
                    Err(e) if e == "IoError" => {
                        return Err(format!(
                            "the server ended the body by closing TCP without TLS's close_notify, after {got} bytes - the board takes that for a broken connection ({e})"
                        ));
                    }
                    Err(e) if e.contains("TimedOut") || e.contains("Io(Other)") => {
                        return Err(format!(
                            "the body runs until the server closes, and after {got} bytes it went silent without closing - the board waits out its timeout ({e})"
                        ));
                    }
                    Err(e) => return Err(format!("the body broke off after {got} bytes ({e})")),
                }
                if got > BODY_LIMIT {
                    return Ok(Body { len: got, exact: false });
                }
            }
        }
    }
}

/// A chunked body as the board decodes it - `size [; extensions] CRLF data
/// CRLF` ... `0 CRLF [trailers] CRLF` - counted into `total` as the sizes
/// come. `Ok(false)`: past BODY_LIMIT, not read to its end.
fn chunked(src: &mut Incoming<'_, '_>, total: &mut usize, received: &mut usize) -> Result<bool, String> {
    let bad = |why: &str| format!("the chunked body does not decode: {why} - the board would refuse it");
    let cut = "the server closed the connection inside the chunked body - the board reports it cut short";
    let byte = |src: &mut Incoming<'_, '_>| -> Result<u8, String> {
        src.byte()
            .map_err(|e| format!("the chunked body broke off ({e})"))?
            .ok_or_else(|| cut.to_owned())
    };
    loop {
        let mut size = 0usize;
        let mut digits = 0;
        let mut b = byte(src)?;
        while let Some(d) = char::from(b).to_digit(16) {
            // The board counts in 32 bits.
            if digits == 8 {
                return Err(bad("a chunk size past 32 bits"));
            }
            size = size << 4 | d as usize;
            digits += 1;
            b = byte(src)?;
        }
        if digits == 0 {
            return Err(bad("a chunk without its size"));
        }
        // Whitespace, then `;name=value` extensions - passed over - up to LF.
        let (mut extension, mut n) = (false, 0);
        while b != b'\n' {
            match b {
                b';' => extension = true,
                b'\r' | b' ' | b'\t' => {}
                _ if b < 0x20 || b == 0x7f || !extension => return Err(bad("something else after a chunk's size")),
                _ => {}
            }
            n += 1;
            if n > CHUNK_LINE_MAX {
                return Err(bad("a chunk-size line longer than the board reads"));
            }
            b = byte(src)?;
        }
        if size == 0 {
            break;
        }
        *total += size;
        if *total > BODY_LIMIT {
            return Ok(false);
        }
        let mut left = size;
        while left > 0 {
            match src.skip(left).map_err(|e| format!("the chunked body broke off ({e})"))? {
                0 => return Err(cut.to_owned()),
                k => {
                    left -= k;
                    *received += k;
                }
            }
        }
        let mut b = byte(src)?;
        if b == b'\r' {
            b = byte(src)?;
        }
        if b != b'\n' {
            return Err(bad("a chunk longer than its size"));
        }
    }
    // Trailer fields, passed over, up to the empty line.
    let mut all = 0;
    loop {
        let mut line = 0;
        loop {
            let b = byte(src)?;
            all += 1;
            if all > TRAILERS_MAX {
                return Err(bad("trailer fields longer than the board reads"));
            }
            match b {
                b'\n' => break,
                b'\r' => {}
                _ => line += 1,
            }
        }
        if line == 0 {
            return Ok(true);
        }
    }
}
