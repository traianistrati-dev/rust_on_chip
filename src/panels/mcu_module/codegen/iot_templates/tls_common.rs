
/// The TCP receive buffer a TLS client gives its socket: a whole TLS record of
/// one MQTT packet (1024 + 22 bytes) must fit, so `Gate` can hold it back
/// until all of it has arrived.
pub const TCP_RX: usize = 2048;
/// TLS records: up to 16 KiB of plaintext and its overhead in; packets out.
const RECORD_RX: usize = 16_640;
const RECORD_TX: usize = 2048;
/// The longest certificate chain a server may send.
const MAX_CHAIN: usize = 8192;

/// A connection: TLS over the socket.
pub type Link<'s> = TlsConnection<'s, Gate<'s>, Aes128GcmSha256>;

/// One connection's TLS record buffers, kept between connections: each
/// client has its own (MQTT keeps its connection open for good).
pub struct LinkBuffers {
    rx: [u8; RECORD_RX],
    tx: [u8; RECORD_TX],
}

impl LinkBuffers {
    pub const fn new() -> Self {
        Self {
            rx: [0; RECORD_RX],
            tx: [0; RECORD_TX],
        }
    }
}

impl Default for LinkBuffers {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a connection goes, and how the server is checked.
pub struct Target<'a> {
    /// The name the server's certificate must carry, sent as SNI.
    pub server_name: &'a str,
    /// Check the server's certificate. `false` = encrypted, but anyone on
    /// the path can pose as the server.
    pub verify: bool,
    /// The CAs the server's chain may end in, as DER one after the other.
    pub cas: &'a [u8],
    /// The board's certificate and P-256 key, sent when the server asks.
    pub client: Option<(&'a [u8], [u8; 32])>,
    /// ALPN protocols - AWS IoT on 443 picks MQTT by one.
    pub alpn: &'a [&'a [u8]],
}

/// One handshake at a time: checking an RSA chain borrows about 22 KB of
/// the heap, and two at once would not fit what TLS brings.
static HANDSHAKE: Mutex<CriticalSectionRawMutex, ()> = Mutex::new(());
/// The longest a handshake may hold that turn: a server that took the TCP
/// connection and then says nothing must not keep the other client waiting
/// for its whole timeout. An RP2040 checks an RSA-4096 chain in a few seconds.
const OPEN_S: u64 = 20;

/// The certificates' dates are checked against `wall_clock()`.
struct CertClock;

impl embedded_tls::TlsClock for CertClock {
    fn now() -> Option<u64> {
        wall_clock()
    }
}

/// The connected socket, wrapped in TLS: the handshake, awaited whole.
pub async fn open<'s>(
    socket: TcpSocket<'s>,
    links: &'s mut LinkBuffers,
    to: &Target<'_>,
) -> Result<Link<'s>, ()> {
    init();
    clock_ready().await;
    let mut link: Link<'s> = TlsConnection::new(Gate::new(socket), &mut links.rx, &mut links.tx);
    // A certificate names DNS names, and an address is never sent as SNI.
    let ip = to.server_name.parse::<Ipv4Addr>().is_ok();
    let config = if ip {
        TlsConfig::new()
    } else {
        TlsConfig::new().with_server_name(to.server_name)
    };
    let config = if to.alpn.is_empty() {
        config
    } else {
        config.with_alpn(to.alpn)
    };
    if to.verify && ip {
        return Err(());
    }
    let mut verifier = Verifier::new(to.cas);
    let provider = Provider {
        rng: tls_rng()?,
        verifier: if to.verify { Some(&mut verifier) } else { None },
        client: to.client,
    };
    let _one = HANDSHAKE.lock().await;
    embassy_time::with_timeout(
        embassy_time::Duration::from_secs(OPEN_S),
        link.open(TlsContext::new(&config, provider)),
    )
    .await
    .map_err(drop)?
    .map_err(drop)?;
    // embedded-tls 0.19 also completes a handshake in which the server sent
    // no certificate at all (its issue #215): then nobody was authenticated.
    if to.verify && !verifier.authenticated {
        return Err(());
    }
    // A server that refuses the board's certificate says so on the first
    // read, not here.
    Ok(link)
}

/// What embedded-tls asks for during the handshake: randomness, the verifier
/// that checks the server's chain, its signature and its name (none: encrypt
/// only) - and, when the server asks, the board's certificate and signature.
struct Provider<'v, 'a> {
    rng: TlsRng,
    verifier: Option<&'v mut Verifier<'a>>,
    client: Option<(&'a [u8], [u8; 32])>,
}

impl CryptoProvider for Provider<'_, '_> {
    type CipherSuite = Aes128GcmSha256;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl CryptoRngCore {
        &mut self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl TlsVerifier<Aes128GcmSha256>, TlsError> {
        // None: embedded-tls checks nothing at all - the chain, the signature
        // and the name alike.
        self.verifier.as_deref_mut().ok_or(TlsError::Unimplemented)
    }

    fn signer(&mut self) -> Result<(impl ecdsa::signature::SignerMut<Self::Signature>, SignatureScheme), TlsError> {
        if !self.proven() {
            return Err(TlsError::Unimplemented);
        }
        let (_, key) = self.client.ok_or(TlsError::Unimplemented)?;
        Ok((ClientKey::new(key)?, SignatureScheme::EcdsaSecp256r1Sha256))
    }

    fn client_cert(&mut self) -> Option<Certificate<impl AsRef<[u8]>>> {
        if !self.proven() {
            return None;
        }
        self.client.map(|(cert, _)| Certificate::X509(cert))
    }
}

impl Provider<'_, '_> {
    /// The server proved who it is - or nothing is checked (encrypt only).
    /// embedded-tls 0.19 asks for the board's certificate once the server's
    /// Finished came, even when no Certificate came before it: without this,
    /// whoever answered in the server's name would get the certificate and a
    /// signature, before `open()` refuses the link.
    fn proven(&self) -> bool {
        self.verifier.as_ref().is_none_or(|v| v.authenticated)
    }
}

/// A connection whose end - the server's close_notify, which embedded-tls
/// 0.19 returns as an error - reads as the end of the stream: an HTTP body
/// may run until the server closes.
#[allow(dead_code)]
pub struct Closing<'l, 's>(pub &'l mut Link<'s>);

impl ErrorType for Closing<'_, '_> {
    type Error = TlsError;
}

impl Read for Closing<'_, '_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TlsError> {
        match self.0.read(buf).await {
            Err(TlsError::ConnectionClosed) => Ok(0),
            other => other,
        }
    }
}

impl Write for Closing<'_, '_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, TlsError> {
        self.0.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), TlsError> {
        self.0.flush().await
    }
}

/// The TCP socket as embedded-tls sees it. A TLS record is handed over only
/// once all of it has arrived, so when mqtt.rs's `select3` drops a read the
/// TLS layer has never consumed half a record: embedded-tls keeps the 5-byte
/// record header inside the read future, and a record cut there would
/// desynchronise the stream for good.
pub struct Gate<'s> {
    socket: TcpSocket<'s>,
    head: [u8; 5],
    /// Header bytes taken from the socket.
    have: usize,
    /// Header bytes handed to TLS.
    given: usize,
    /// Body bytes of the current record still to hand over.
    body: usize,
}

impl<'s> Gate<'s> {
    fn new(socket: TcpSocket<'s>) -> Self {
        Self {
            socket,
            head: [0; 5],
            have: 0,
            given: 0,
            body: 0,
        }
    }

    fn next_record(&mut self) {
        self.have = 0;
        self.given = 0;
    }
}

impl ErrorType for Gate<'_> {
    type Error = TcpError;
}

impl Read for Gate<'_> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.given == 5 {
            // The body: already queued whole, so this returns at once.
            let want = buf.len().min(self.body);
            let n = self.socket.read(&mut buf[..want]).await?;
            self.body -= n;
            if self.body == 0 {
                self.next_record();
            }
            return Ok(n);
        }
        // A record boundary - the only place a read may wait. Everything it
        // has read so far is kept in `self`, so being dropped loses nothing.
        while self.have < 5 {
            let n = self.socket.read(&mut self.head[self.have..]).await?;
            if n == 0 {
                return Ok(0);
            }
            self.have += n;
        }
        if self.given == 0 {
            let len = usize::from(u16::from_be_bytes([self.head[3], self.head[4]]));
            // An encrypted record carries at least its inner content type and a
            // 16-byte tag (RFC 8446 5.2). A shorter one decrypts to nothing, and
            // embedded-tls 0.19 panics on that (connection.rs:62) - before any
            // certificate is checked, so anyone on the path could send it.
            if self.head[0] == 0x17 && len < 17 {
                return Err(TcpError::ConnectionReset);
            }
            // A record bigger than the socket's buffer can never be queued
            // whole: it streams through (only the handshake's, which is never
            // dropped halfway). Nor once the server can send nothing more - a
            // closed socket keeps what it holds, and `may_recv` stays true then.
            while len <= self.socket.recv_capacity()
                && self.socket.recv_queue() < len
                && matches!(self.socket.state(), State::Established | State::FinWait1 | State::FinWait2)
            {
                Timer::after_millis(2).await;
            }
            self.body = len;
        }
        let n = buf.len().min(5 - self.given);
        buf[..n].copy_from_slice(&self.head[self.given..self.given + n]);
        self.given += n;
        if self.given == 5 && self.body == 0 {
            self.next_record();
        }
        Ok(n)
    }
}

impl Write for Gate<'_> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        self.socket.write(buf).await
    }

    async fn flush(&mut self) -> Result<(), TcpError> {
        self.socket.flush().await
    }
}

