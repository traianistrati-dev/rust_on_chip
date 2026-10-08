//! RustOnChip's "Test from this PC" (IoT tab, MQTT over TLS).
//!
//! The board's own certificate checks - `tls_verify.rs`, the very text the
//! firmware's `mqtt_link.rs` carries - run on this PC against the broker, then
//! an MQTT 5 CONNECT as the firmware sends it. The IDE writes this program,
//! builds it once, and runs it with the request on stdin: one hex-encoded
//! value per line - host, port, server name, verify ("1"/"0"), the CA as DER,
//! client id, user name, password. It prints `INFO ...` lines, then one
//! `RESULT ok ...` or `RESULT fail ...`.
#![allow(dead_code)]

use std::io::BufRead;
use std::net::{Ipv4Addr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use embedded_io_adapters::std::FromStd;
use embedded_tls::blocking::TlsConnection;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsContext, UnsecureProvider};

/// The certificates' dates are checked against this PC's clock. (The board
/// checks them only once SNTP has the time.)
type CertClock = std::time::SystemTime;
/// The longest certificate chain the broker may send - as on the board.
const MAX_CHAIN: usize = 8192;

include!("tls_verify.rs");

struct Request {
    host: String,
    port: u16,
    server_name: String,
    verify: bool,
    ca: Vec<u8>,
    client_id: String,
    user: String,
    password: String,
}

fn unhex(line: &str) -> Vec<u8> {
    let line = line.trim();
    (0..line.len() / 2)
        .filter_map(|i| u8::from_str_radix(&line[2 * i..2 * i + 2], 16).ok())
        .collect()
}

fn read_request() -> Option<Request> {
    let stdin = std::io::stdin();
    let lines: Vec<Vec<u8>> = stdin.lock().lines().map_while(Result::ok).map(|l| unhex(&l)).collect();
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
    })
}

fn main() {
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
        self.p384 |= chain
            .entries
            .iter()
            .any(|e| matches!(e, embedded_tls::CertificateEntryRef::X509(der) if signed_with_p384(der)));
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

struct Provider<'v, 'a> {
    rng: rand_core::OsRng,
    verifier: &'v mut HostVerifier<'a>,
}

impl embedded_tls::CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = [u8; 0];

    fn rng(&mut self) -> impl embedded_tls::CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl embedded_tls::TlsVerifier<Aes128GcmSha256>, embedded_tls::TlsError> {
        Ok(&mut *self.verifier)
    }
}

/// What a refused handshake means, in the user's terms.
fn explain(e: &embedded_tls::TlsError) -> &'static str {
    use embedded_tls::TlsError as E;
    match e {
        E::InvalidCertificate => {
            "the broker's certificate did not check out: its chain does not end in mqtt_ca.rs's CA, it does not name the server name, or it is out of date"
        }
        E::InvalidSignatureScheme | E::InvalidSignature => {
            "a signature the board cannot check: a key or curve embedded-tls does not take"
        }
        E::HandshakeAborted(..) => {
            "the broker refused the handshake: it may speak only TLS 1.2, or want a client certificate"
        }
        E::Io(_) | E::IoError | E::ConnectionClosed => "the connection broke during the handshake",
        _ => "the handshake failed",
    }
}

fn run(r: &Request) -> Result<String, String> {
    let address = (r.host.as_str(), r.port)
        .to_socket_addrs()
        .map_err(|e| format!("{} could not be resolved: {e}", r.host))?
        .find(|a| a.is_ipv4())
        .ok_or_else(|| format!("{} has no IPv4 address", r.host))?;
    println!("INFO resolved {address}");
    let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .map_err(|e| format!("no TCP connection to {address}: {e}"))?;
    let _ = tcp.set_read_timeout(Some(Duration::from_secs(15)));
    let _ = tcp.set_write_timeout(Some(Duration::from_secs(15)));
    println!("INFO TCP connected");

    let mut rx = vec![0u8; 16_640];
    let mut tx = vec![0u8; 2048];
    let mut tls: TlsConnection<'_, FromStd<TcpStream>, Aes128GcmSha256> =
        TlsConnection::new(FromStd::new(tcp), &mut rx, &mut tx);
    let ip = r.server_name.parse::<Ipv4Addr>().is_ok();
    let config = if ip {
        TlsConfig::new()
    } else {
        TlsConfig::new().with_server_name(&r.server_name)
    };
    if r.verify {
        if ip {
            return Err("the server name is an address, and a certificate names DNS names".into());
        }
        let mut verifier = HostVerifier { inner: Verifier::new(&r.ca), p384: false };
        let provider = Provider { rng: rand_core::OsRng, verifier: &mut verifier };
        tls.open(TlsContext::new(&config, provider))
            .map_err(|e| format!("{} ({e:?})", explain(&e)))?;
        if !verifier.inner.authenticated {
            return Err("the broker finished the handshake without proving a certificate".into());
        }
        println!(
            "INFO certificate checked: its chain ends in the CA, it names {}, and it is in date by this PC's clock",
            r.server_name
        );
        if verifier.p384 {
            println!("INFO p384");
        }
    } else {
        let provider = UnsecureProvider::new::<Aes128GcmSha256>(rand_core::OsRng);
        tls.open(TlsContext::new(&config, provider))
            .map_err(|e| format!("{} ({e:?})", explain(&e)))?;
        println!("INFO encrypted, the certificate not checked (encrypt only)");
    }

    // MQTT 5 CONNECT, as the board sends it. The credentials only to a
    // broker whose certificate was checked.
    let credentials = r.verify;
    let packet = connect_packet(&r.client_id, credentials.then_some((r.user.as_str(), r.password.as_str())));
    // `write` takes what fits in one record (about 1.9 KB): a long token as
    // the password needs more than one.
    let mut rest = packet.as_slice();
    while !rest.is_empty() {
        match tls.write(rest) {
            Ok(0) => return Err("the CONNECT could not be sent (no room)".into()),
            Ok(n) => rest = &rest[n..],
            Err(e) => return Err(format!("the CONNECT could not be sent ({e:?})")),
        }
    }
    tls.flush().map_err(|e| format!("the CONNECT could not be sent ({e:?})"))?;
    let reason = read_connack(&mut tls)?;
    let _ = tls.write(&[0xE0, 0x00]);
    let _ = tls.flush();
    match reason {
        0x00 => Ok(if credentials {
            "the broker accepted the connection (CONNACK: success)".into()
        } else {
            "the broker accepted the connection without credentials (encrypt only sends none from here)".into()
        }),
        // A login refused that was never sent: the board sends secrets.rs's
        // whatever the check, so TLS is all this could test.
        code @ (0x04 | 0x05 | 0x86 | 0x87) if !credentials => Ok(format!(
            "TLS works; the broker wants a login (0x{code:02X}), which encrypt only does not send from this PC - the board sends secrets.rs's, so turn the check on to test it"
        )),
        code => Err(format!("the broker refused the CONNECT: {} (0x{code:02X})", connack_reason(code))),
    }
}

/// An MQTT 5 CONNECT: clean start, keep-alive 60 s, no properties.
fn connect_packet(client_id: &str, credentials: Option<(&str, &str)>) -> Vec<u8> {
    let field = |out: &mut Vec<u8>, s: &[u8]| {
        out.extend_from_slice(&(s.len() as u16).to_be_bytes());
        out.extend_from_slice(s);
    };
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
    body.extend_from_slice(&60u16.to_be_bytes());
    body.push(0x00); // no properties
    body.extend_from_slice(&payload);
    let mut packet = vec![0x10];
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
    packet.extend_from_slice(&body);
    packet
}

/// The CONNACK's reason code (its second byte).
fn read_connack(tls: &mut TlsConnection<'_, FromStd<TcpStream>, Aes128GcmSha256>) -> Result<u8, String> {
    let mut byte = |what: &str| -> Result<u8, String> {
        let mut b = [0u8; 1];
        match tls.read(&mut b) {
            Ok(1) => Ok(b[0]),
            Ok(_) => Err(format!("the broker closed the connection before {what}")),
            Err(e) => Err(format!("no {what} from the broker ({e:?})")),
        }
    };
    let kind = byte("a CONNACK")?;
    if kind != 0x20 {
        return Err(format!("the broker answered something else than a CONNACK (0x{kind:02X})"));
    }
    let mut len = 0usize;
    for shift in [0, 7, 14, 21] {
        let b = byte("the CONNACK")?;
        len |= usize::from(b & 0x7F) << shift;
        if b & 0x80 == 0 {
            break;
        }
    }
    if len < 2 {
        return Err("a CONNACK too short to read".into());
    }
    let _flags = byte("the CONNACK")?;
    byte("the CONNACK")
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
