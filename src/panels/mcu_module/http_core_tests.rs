//! The firmware's HTTP/1.1 core (`codegen/iot_templates/http_core.rs`), the
//! very text a project's http.rs carries, run on the PC: RFC 9112's corner
//! cases, the request bytes, and fuzz runs that must neither panic nor spin.
//! On a board a panic on a server's bytes is a reboot loop under Restart on
//! panic, and a spin starves the executor so that no timeout can fire.
//!
//! ```text
//! cargo test --bin rust_on_chip http_core
//! ```

use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

// What the firmware's tail imports above the core - so a second import of
// either name inside the core fails here as it would on the board (E0252).
use embedded_io_async::{ErrorType, Read, Write};

/// Just enough of embedded-io-async 0.7 for the core, in the same shapes:
/// the IDE does not depend on the crate, and the core names it by full path.
mod embedded_io_async {
    #![allow(async_fn_in_trait)]

    pub trait ErrorType {
        type Error: core::fmt::Debug;
    }

    pub trait Read: ErrorType {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error>;
    }

    pub trait Write: ErrorType {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error>;

        async fn flush(&mut self) -> Result<(), Self::Error>;

        async fn write_all(&mut self, mut buf: &[u8]) -> Result<(), Self::Error> {
            while !buf.is_empty() {
                match self.write(buf).await {
                    Ok(0) => panic!("write() returned Ok(0)"),
                    Ok(n) => buf = &buf[n..],
                    Err(e) => return Err(e),
                }
            }
            Ok(())
        }
    }
}

/// The generated block above the core defines it; `build` reads it by name.
const USER_AGENT: &str = "RustOnChip";

include!("codegen/iot_templates/http_core.rs");

/// An in-memory connection: the server's bytes, handed out at most `step`
/// at a time; what the client wrote is kept.
struct Mem {
    input: Vec<u8>,
    pos: usize,
    step: usize,
    /// Each read first answers "not yet" once, as a socket waiting for data
    /// does: the core must pick up where it left off.
    pending: bool,
    /// Reads fail once this many bytes are out.
    fail_at: Option<usize>,
    eof_reads: u32,
    written: Vec<u8>,
}

impl Mem {
    fn new(input: &[u8], step: usize) -> Self {
        Mem {
            input: input.to_vec(),
            pos: 0,
            step,
            pending: false,
            fail_at: None,
            eof_reads: 0,
            written: Vec::new(),
        }
    }
}

/// The transport's error.
#[derive(Debug)]
struct Broken;

impl ErrorType for Mem {
    type Error = Broken;
}

impl Read for Mem {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Broken> {
        // A read into nothing returns 0, which reads as the end.
        assert!(!buf.is_empty(), "read into an empty buffer");
        if self.pending {
            NotYet(false).await;
        }
        if self.fail_at.is_some_and(|at| self.pos >= at) {
            return Err(Broken);
        }
        if self.pos == self.input.len() {
            self.eof_reads += 1;
            // embassy-net answers every read past the end with 0 at once: a
            // client that reads on after it has seen one spins for ever.
            assert!(self.eof_reads < 2, "SPIN: read again after the end of the stream");
        }
        let n = buf.len().min(self.step).min(self.input.len() - self.pos);
        buf[..n].copy_from_slice(&self.input[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Write for Mem {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Broken> {
        self.written.extend_from_slice(buf);
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Broken> {
        Ok(())
    }
}

/// Pending once, then ready - waking itself, as a socket does when data comes.
struct NotYet(bool);

impl Future for NotYet {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.0 {
            return Poll::Ready(());
        }
        self.0 = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// More polls than any test needs, even a byte at a time with every read
/// pending once: a future still pending after them is spinning.
const POLLS_MAX: usize = 100_000;

fn block_on<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    let mut cx = Context::from_waker(Waker::noop());
    for _ in 0..POLLS_MAX {
        if let Poll::Ready(out) = f.as_mut().poll(&mut cx) {
            return out;
        }
    }
    panic!("SPIN: still pending after {POLLS_MAX} polls");
}

/// The answer to `method` read from `m` into a `cap`-byte body, as one line:
/// `200 body="hello" TRUNCATED location=...` or `ERR Closed`.
fn answer_from(m: &mut Mem, method: Method, cap: usize) -> String {
    let mut body = vec![0u8; cap];
    match block_on(receive(m, method, &mut body)) {
        Ok(r) => {
            // Interim answers never come back as the answer.
            assert!((200..=599).contains(&r.status), "status {}", r.status);
            format!(
                "{} body={:?}{}{}",
                r.status,
                String::from_utf8_lossy(&body[..r.len]),
                if r.truncated { " TRUNCATED" } else { "" },
                r.location().map(|l| format!(" location={l}")).unwrap_or_default()
            )
        }
        Err(e) => format!("ERR {e:?}"),
    }
}

fn answer(resp: &[u8], method: Method, step: usize, pending: bool, cap: usize) -> String {
    let mut m = Mem::new(resp, step);
    m.pending = pending;
    answer_from(&mut m, method, cap)
}

/// The answer read whole, a byte at a time (each read pending first), and in
/// odd pieces - which must all agree, since a server's packets fall anywhere.
fn answered(resp: &[u8], method: Method, cap: usize) -> String {
    let whole = answer(resp, method, usize::MAX, false, cap);
    for (step, pending) in [(1, true), (3, false), (7, true)] {
        assert_eq!(
            answer(resp, method, step, pending, cap),
            whole,
            "reads of {step} byte(s) disagree on {:?}",
            String::from_utf8_lossy(resp)
        );
    }
    whole
}

fn get(resp: &[u8]) -> String {
    answered(resp, Method::Get, 64)
}

/// The request as the firmware's tail sends it: the head built whole, then
/// the payload, through the connection's `write_all`.
fn request(
    method: Method,
    url: &str,
    auth: Auth<'_>,
    content_type: Option<&str>,
    headers: &[(&str, &str)],
    payload: &[u8],
) -> Result<String, HttpError> {
    let url = Url::parse(url)?;
    let mut head = [0u8; REQUEST_HEAD_MAX];
    let n = build(&mut head, method, &url, auth, content_type, headers, payload.len())?;
    let mut m = Mem::new(b"", 1);
    block_on(async {
        m.write_all(&head[..n]).await.unwrap();
        m.write_all(payload).await.unwrap();
        m.flush().await.unwrap();
    });
    Ok(String::from_utf8(m.written).unwrap())
}

fn get_line(url: &str) -> String {
    let text = request(Method::Get, url, Auth::None, None, &[], b"").unwrap();
    text.lines().next().unwrap().to_owned()
}

/// The ordinary answers the fuzz runs start from.
const CASES: &[(&[u8], Method)] = &[
    (b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello", Method::Get),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5;name=val\r\nhello\r\n0\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Sum: 1\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n", Method::Get),
    (b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", Method::Get),
    (b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok", Method::Get),
    (b"HTTP/1.1 304 Not Modified\r\nContent-Length: 1234\r\n\r\n", Method::Get),
    (b"HTTP/1.1 204 No Content\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n", Method::Head),
    (b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nabcdef", Method::Get),
    (b"HTTP/1.1 200\r\nContent-Length: 2\r\n\r\nok", Method::Get),
    (b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok", Method::Get),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nok", Method::Get),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nok!", Method::Get),
    (b"HTTP/1.1 200 OK\r\nX-A: 1\r\n  continued\r\nContent-Length: 2\r\n\r\nok", Method::Get),
    (b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://example.org/new\r\nContent-Length: 0\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort", Method::Get),
    (b"SSH-2.0-OpenSSH\r\n\r\n", Method::Get),
    (b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nhello", Method::Get),
];

/// Inputs per fuzz run: as many as the prototype passed, a fraction of a
/// second in a debug build.
const FUZZ_RUNS: usize = 20_000;

/// A xorshift: the same inputs on every run, so a failure can be replayed.
fn xorshift(mut seed: u64) -> impl FnMut() -> u64 {
    move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    }
}

// ── The answer ─────────────────────────────────────────────────────────────

#[test]
fn a_content_length_body_reads_whole() {
    assert_eq!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello"), "200 body=\"hello\"");
    assert_eq!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"), "200 body=\"\"");
}

#[test]
fn a_chunked_body_with_extensions_and_trailers_reads_whole() {
    let te = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    for (chunks, body) in [
        ("5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n", "hello world"),
        ("5;name=val\r\nhello\r\n0\r\n\r\n", "hello"),
        ("5 ; a=1 ; b=\"x y\"\r\nhello\r\n0;last\r\n\r\n", "hello"),
        ("5\r\nhello\r\n0\r\nX-Sum: 1\r\nX-Other: 2\r\n\r\n", "hello"),
        ("A\r\n0123456789\r\n0\r\n\r\n", "0123456789"),
        ("5\nhello\n0\n\n", "hello"),
    ] {
        assert_eq!(get(format!("{te}{chunks}").as_bytes()), format!("200 body={body:?}"), "{chunks:?}");
    }
    // The coding's name is case-insensitive.
    let upper = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
    assert_eq!(get(upper), "200 body=\"hello\"");
}

#[test]
fn transfer_encoding_wins_over_content_length() {
    let both = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n";
    assert_eq!(get(both), "200 body=\"hello\"");
    let last_chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n2\r\nGZ\r\n0\r\n\r\n";
    assert_eq!(get(last_chunked), "200 body=\"GZ\"");
    // A last coding other than chunked: the body runs until the close, whatever
    // Content-Length says.
    let gzip = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip\r\nContent-Length: 1\r\n\r\nGZIPPED";
    assert_eq!(get(gzip), "200 body=\"GZIPPED\"");
}

#[test]
fn interim_answers_are_skipped_and_101_is_refused() {
    let cont = b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    assert_eq!(get(cont), "200 body=\"ok\"");
    // A Location in an interim answer is not the final answer's.
    let hints = b"HTTP/1.1 103 Early Hints\r\nLink: </a.css>; rel=preload\r\nLocation: /x\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    assert_eq!(get(hints), "200 body=\"ok\"");
    let switch = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\nHTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok";
    assert_eq!(get(switch), "ERR BadResponse");
    let interims = |k: usize| {
        let mut r = b"HTTP/1.1 100 Continue\r\n\r\n".repeat(k);
        r.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        get(&r)
    };
    assert_eq!(interims(usize::from(INTERIM_MAX)), "200 body=\"ok\"");
    assert_eq!(interims(usize::from(INTERIM_MAX) + 1), "ERR BadResponse");
}

#[test]
fn head_204_and_304_carry_no_body_whatever_their_headers_say() {
    let not_modified = b"HTTP/1.1 304 Not Modified\r\nContent-Length: 1234\r\n\r\n";
    assert_eq!(get(not_modified), "304 body=\"\"");
    assert_eq!(get(b"HTTP/1.1 204 No Content\r\n\r\n"), "204 body=\"\"");
    let chunked_204 = b"HTTP/1.1 204 No Content\r\nTransfer-Encoding: chunked\r\n\r\n";
    assert_eq!(get(chunked_204), "204 body=\"\"");
    let head = b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n";
    assert_eq!(answered(head, Method::Head, 64), "200 body=\"\"");
}

#[test]
fn without_a_length_the_body_runs_until_the_server_closes() {
    let old = b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nabcdef";
    assert_eq!(get(old), "200 body=\"abcdef\"");
    assert_eq!(get(b"HTTP/1.1 200 OK\r\n\r\n"), "200 body=\"\"");
}

#[test]
fn a_status_line_without_a_reason_and_bare_lf_line_ends_are_taken() {
    assert_eq!(get(b"HTTP/1.1 200\r\nContent-Length: 2\r\n\r\nok"), "200 body=\"ok\"");
    assert_eq!(get(b"HTTP/1.1 200 OK\nContent-Length: 2\n\nok"), "200 body=\"ok\"");
    assert_eq!(get(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\nno"), "404 body=\"no\"");
}

#[test]
fn a_repeated_content_length_is_taken_and_a_conflicting_one_refused() {
    assert_eq!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 2, 2\r\n\r\nok"), "200 body=\"ok\"");
    assert_eq!(get(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\nok"), "200 body=\"ok\"");
    for bad in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\nok!"[..],
        b"HTTP/1.1 200 OK\r\nContent-Length: 2, 3\r\n\r\nok!",
        b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: +2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: \r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length: 99999999999999999999999\r\n\r\nok",
    ] {
        assert_eq!(get(bad), "ERR BadResponse", "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn a_folded_or_malformed_header_is_refused() {
    for bad in [
        &b"HTTP/1.1 200 OK\r\nX-A: 1\r\n  continued\r\nContent-Length: 2\r\n\r\nok"[..],
        b"HTTP/1.1 200 OK\r\nX-A: 1\r\n\tcontinued\r\nContent-Length: 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nContent-Length : 2\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\nNo colon here\r\n\r\nok",
        b"HTTP/1.1 200 OK\r\n: no name\r\n\r\nok",
    ] {
        assert_eq!(get(bad), "ERR BadResponse", "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn a_redirect_reports_its_location_and_is_not_followed() {
    let moved = b"HTTP/1.1 301 Moved Permanently\r\nLocation: https://example.org/new\r\nContent-Length: 0\r\n\r\n";
    assert_eq!(get(moved), "301 body=\"\" location=https://example.org/new");
    // Longer than LOCATION_MAX: dropped, not cut.
    let long = format!(
        "HTTP/1.1 302 Found\r\nLocation: https://example.org/{}\r\nContent-Length: 0\r\n\r\n",
        "a".repeat(LOCATION_MAX)
    );
    assert_eq!(get(long.as_bytes()), "302 body=\"\"");
    let fits = format!(
        "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\n\r\n",
        "b".repeat(LOCATION_MAX)
    );
    assert_eq!(get(fits.as_bytes()), format!("302 body=\"\" location={}", "b".repeat(LOCATION_MAX)));
}

#[test]
fn a_server_that_closes_early_is_reported_closed() {
    for cut in [
        &b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort"[..],
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n10\r\nhello",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n",
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\nX-Sum: 1\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n",
        b"HTTP/1.1 100 Continue\r\n\r\n",
        b"",
    ] {
        assert_eq!(get(cut), "ERR Closed", "{:?}", String::from_utf8_lossy(cut));
    }
}

#[test]
fn an_answer_that_is_not_http_1_is_a_bad_response() {
    for bad in [
        &b"SSH-2.0-OpenSSH\r\n\r\n"[..],
        b"HTTP/2.0 200 OK\r\n\r\n",
        b"HTTP/1.1 20 OK\r\n\r\n",
        b"HTTP/1.1 2000 OK\r\n\r\n",
        b"HTTP/1.1 600 Odd\r\n\r\n",
        b"HTTP/1.1 099 Odd\r\n\r\n",
        b"http/1.1 200 OK\r\n\r\n",
    ] {
        assert_eq!(get(bad), "ERR BadResponse", "{:?}", String::from_utf8_lossy(bad));
    }
}

#[test]
fn a_bad_chunk_is_a_bad_response() {
    let te = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n";
    // One hex digit more than a usize holds.
    let too_many_digits = format!("{}\r\n", "1".repeat(2 * core::mem::size_of::<usize>() + 1));
    let long_extension = format!("5;{}\r\nhello\r\n0\r\n\r\n", "x".repeat(LINE_MAX));
    let long_trailers = format!("0\r\nX-T: {}\r\n\r\n", "t".repeat(TRAILERS_MAX));
    for chunks in [
        "zz\r\nhello\r\n0\r\n\r\n",
        "\r\nhello\r\n0\r\n\r\n",
        "5 x\r\nhello\r\n0\r\n\r\n",
        "5;\x01\r\nhello\r\n0\r\n\r\n",
        "5\r\nhelloXX0\r\n\r\n",
        // A chunk longer than its size says: the byte after it must be the
        // line end, not skipped on the way to what looks like the next size.
        "5\r\nhelloX0\r\n\r\n",
        too_many_digits.as_str(),
        long_extension.as_str(),
        long_trailers.as_str(),
    ] {
        let resp = format!("{te}{chunks}");
        assert_eq!(get(resp.as_bytes()), "ERR BadResponse", "{chunks:?}");
    }
}

#[test]
fn seventy_headers_fit_and_a_head_over_head_max_is_refused() {
    let mut r = b"HTTP/1.1 200 OK\r\n".to_vec();
    for i in 0..70 {
        r.extend_from_slice(format!("X-{i}: v\r\n").as_bytes());
    }
    r.extend_from_slice(b"Content-Length: 2\r\n\r\nok");
    assert_eq!(get(&r), "200 body=\"ok\"");

    // A head of exactly HEAD_MAX bytes fits; one more byte does not.
    let head = |pad: usize| {
        let start = "HTTP/1.1 200 OK\r\nX-Pad: ";
        let end = "\r\nContent-Length: 2\r\n\r\n";
        format!("{start}{}{end}ok", "p".repeat(pad))
    };
    let exact = HEAD_MAX - head(0).len() + 2;
    assert_eq!(head(exact).len() - 2, HEAD_MAX);
    assert_eq!(get(head(exact).as_bytes()), "200 body=\"ok\"");
    assert_eq!(get(head(exact + 1).as_bytes()), "ERR HeadTooLarge");
}

#[test]
fn a_body_bigger_than_the_buffer_is_truncated_and_flagged() {
    let length = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n0123456789abcdefghij";
    let close = b"HTTP/1.1 200 OK\r\n\r\n0123456789abcdefghij";
    let chunk = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n14\r\n0123456789abcdefghij\r\n0\r\n\r\n";
    let chunks = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n01234567\r\n2\r\n89\r\n0\r\n\r\n";
    for resp in [&length[..], close, chunk, chunks] {
        assert_eq!(answered(resp, Method::Get, 8), "200 body=\"01234567\" TRUNCATED");
    }
    // Exactly the buffer: whole, not truncated - without a length, one more
    // read finds the close that says so.
    for resp in [&length[..], close, chunk] {
        assert_eq!(answered(resp, Method::Get, 20), "200 body=\"0123456789abcdefghij\"");
    }
    assert_eq!(answered(chunks, Method::Get, 10), "200 body=\"0123456789\"");
    let fits = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n8\r\n01234567\r\n0\r\n\r\n";
    assert_eq!(answered(fits, Method::Get, 8), "200 body=\"01234567\"");
    // No buffer at all: the status alone.
    assert_eq!(answered(length, Method::Get, 0), "200 body=\"\" TRUNCATED");
    assert_eq!(answered(close, Method::Get, 0), "200 body=\"\" TRUNCATED");
}

#[test]
fn a_failing_connection_is_io() {
    let resp = b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello";
    // In the status line, in the headers, before the body's last byte.
    for at in [0, 20, resp.len() - 1] {
        let mut m = Mem::new(resp, 1);
        m.fail_at = Some(at);
        assert_eq!(answer_from(&mut m, Method::Get, 64), "ERR Io", "failing at {at}");
    }
}

// ── The request ────────────────────────────────────────────────────────────

#[test]
fn the_request_head_carries_the_port_the_auth_and_the_length() {
    let text = request(
        Method::Post,
        "http://192.168.1.10:8123/api/states/sensor.t?x=1#frag",
        Auth::Basic("Aladdin", "open sesame"),
        Some("application/json"),
        &[("X-Extra", "1")],
        b"{\"state\":\"21.5\"}",
    )
    .unwrap();
    assert_eq!(
        text,
        "POST /api/states/sensor.t?x=1 HTTP/1.1\r\n\
         Host: 192.168.1.10:8123\r\n\
         User-Agent: RustOnChip\r\n\
         Connection: close\r\n\
         Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==\r\n\
         Content-Type: application/json\r\n\
         Content-Length: 16\r\n\
         X-Extra: 1\r\n\
         \r\n\
         {\"state\":\"21.5\"}"
    );
}

#[test]
fn a_get_sends_no_length_and_a_post_always_does() {
    let get = request(Method::Get, "http://example.org", Auth::None, None, &[], b"").unwrap();
    assert_eq!(
        get,
        "GET / HTTP/1.1\r\nHost: example.org\r\nUser-Agent: RustOnChip\r\nConnection: close\r\n\r\n"
    );
    let post = request(Method::Post, "https://example.org/x", Auth::None, None, &[], b"").unwrap();
    assert!(post.contains("\r\nContent-Length: 0\r\n"), "{post}");
    let put = request(Method::Put, "https://example.org/x", Auth::None, None, &[], b"").unwrap();
    assert!(put.starts_with("PUT /x HTTP/1.1\r\n") && put.contains("\r\nContent-Length: 0\r\n"));
    let delete = request(Method::Delete, "https://example.org/x", Auth::None, None, &[], b"").unwrap();
    assert!(delete.starts_with("DELETE /x HTTP/1.1\r\n") && !delete.contains("Content-Length"));
    let patch = request(Method::Patch, "https://example.org/x", Auth::None, None, &[], b"ab").unwrap();
    assert!(patch.starts_with("PATCH /x HTTP/1.1\r\n") && patch.ends_with("Content-Length: 2\r\n\r\nab"));
    let head = request(Method::Head, "https://example.org/x", Auth::None, None, &[], b"").unwrap();
    assert!(head.starts_with("HEAD /x HTTP/1.1\r\n") && !head.contains("Content-Length"));
    // The default port stays out of Host; any other goes in.
    assert!(get.contains("\r\nHost: example.org\r\n"));
    let https_443 = request(Method::Get, "https://example.org:443/", Auth::None, None, &[], b"").unwrap();
    assert!(https_443.contains("\r\nHost: example.org\r\n"));
    let https_8443 = request(Method::Get, "https://example.org:8443/", Auth::None, None, &[], b"").unwrap();
    assert!(https_8443.contains("\r\nHost: example.org:8443\r\n"));
    let http_443 = request(Method::Get, "http://example.org:443/", Auth::None, None, &[], b"").unwrap();
    assert!(http_443.contains("\r\nHost: example.org:443\r\n"));
}

#[test]
fn bearer_basic_and_raw_authorization_go_out_as_given() {
    fn auth(a: Auth<'_>) -> Option<String> {
        let text = request(Method::Get, "http://h/", a, None, &[], b"").unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("Authorization: "))
            .map(str::to_owned)
    }
    assert_eq!(auth(Auth::None), None);
    assert_eq!(auth(Auth::Bearer("abc.def")).as_deref(), Some("Bearer abc.def"));
    assert_eq!(auth(Auth::Raw("Token xyz==")).as_deref(), Some("Token xyz=="));
    // Each padding of base64.
    assert_eq!(auth(Auth::Basic("a", "")).as_deref(), Some("Basic YTo="));
    assert_eq!(auth(Auth::Basic("ab", "")).as_deref(), Some("Basic YWI6"));
    assert_eq!(auth(Auth::Basic("abcd", "")).as_deref(), Some("Basic YWJjZDo="));
    // The password may hold anything: it goes out encoded.
    assert_eq!(
        auth(Auth::Basic("user", "p\u{e4}ss:w\r\nd")).as_deref(),
        Some("Basic dXNlcjpww6Rzczp3DQpk")
    );
}

#[test]
fn a_header_that_would_break_the_request_is_refused() {
    fn refused(
        auth: Auth<'_>,
        content_type: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<String, HttpError> {
        request(Method::Post, "http://h/", auth, content_type, headers, b"x")
    }
    for (what, result) in [
        ("bearer CRLF", refused(Auth::Bearer("t\r\nX-Evil: 1"), None, &[])),
        ("raw LF", refused(Auth::Raw("Token t\nX: 1"), None, &[])),
        ("raw NUL", refused(Auth::Raw("Token \0"), None, &[])),
        ("basic user colon", refused(Auth::Basic("a:b", "c"), None, &[])),
        ("content type CR", refused(Auth::None, Some("text/plain\r"), &[])),
        ("value LF", refused(Auth::None, None, &[("X-A", "1\nX-B: 2")])),
        ("name space", refused(Auth::None, None, &[("X A", "1")])),
        ("name colon", refused(Auth::None, None, &[("X-A:", "1")])),
        ("name empty", refused(Auth::None, None, &[("", "1")])),
        ("own Host", refused(Auth::None, None, &[("host", "other")])),
        ("own Content-Length", refused(Auth::None, None, &[("Content-Length", "5")])),
        ("own Transfer-Encoding", refused(Auth::None, None, &[("TRANSFER-ENCODING", "chunked")])),
        ("own Connection", refused(Auth::None, None, &[("Connection", "keep-alive")])),
    ] {
        assert_eq!(result, Err(HttpError::Header), "{what}");
    }
    // A tab and visible punctuation are fine in a value.
    let ok = refused(Auth::None, Some("text/plain; charset=utf-8"), &[("X-A", "a\tb \"c\"")]).unwrap();
    assert!(ok.contains("\r\nX-A: a\tb \"c\"\r\n"));
}

#[test]
fn a_request_head_that_does_not_fit_is_too_large() {
    let url = Url::parse("http://example.org/a/long/path").unwrap();
    let mut whole = [0u8; REQUEST_HEAD_MAX];
    let n = build(&mut whole, Method::Get, &url, Auth::None, None, &[], 0).unwrap();
    let mut short = vec![0u8; n - 1];
    assert_eq!(
        build(&mut short, Method::Get, &url, Auth::None, None, &[], 0),
        Err(HttpError::TooLarge)
    );
    let mut exact = vec![0u8; n];
    assert_eq!(build(&mut exact, Method::Get, &url, Auth::None, None, &[], 0), Ok(n));
    let token = "t".repeat(REQUEST_HEAD_MAX);
    assert_eq!(
        build(&mut whole, Method::Get, &url, Auth::Bearer(&token), None, &[], 0),
        Err(HttpError::TooLarge)
    );
}

#[test]
fn urls_are_parsed_the_way_the_request_needs_them() {
    let parts = |u: &str| {
        let p = Url::parse(u).unwrap();
        (p.https, p.host.to_owned(), p.port, p.target().to_owned())
    };
    // The query right after the host: the key stays out of the host name.
    assert_eq!(
        parts("https://api.thingspeak.com?api_key=K&field1=1"),
        (true, "api.thingspeak.com".into(), 443, "?api_key=K&field1=1".into())
    );
    assert_eq!(get_line("https://api.thingspeak.com?api_key=K&field1=1"), "GET /?api_key=K&field1=1 HTTP/1.1");
    assert_eq!(parts("HTTPS://Example.COM"), (true, "Example.COM".into(), 443, String::new()));
    assert_eq!(get_line("HTTPS://Example.COM"), "GET / HTTP/1.1");
    assert_eq!(parts("http://host:/x"), (false, "host".into(), 80, "/x".into()));
    assert_eq!(parts("http://10.0.0.2:8123/api/"), (false, "10.0.0.2".into(), 8123, "/api/".into()));
    assert_eq!(parts("http://my_host.local:1"), (false, "my_host.local".into(), 1, String::new()));
    // The fragment never reaches the server.
    assert_eq!(parts("http://h/p?q=1#frag"), (false, "h".into(), 80, "/p?q=1".into()));
    assert_eq!(parts("http://h#frag"), (false, "h".into(), 80, String::new()));
    assert_eq!(get_line("http://h/p?q=1#frag"), "GET /p?q=1 HTTP/1.1");
    // Percent-encoded bytes go through as they are.
    assert_eq!(get_line("http://h/topics/a%2Fb?qos=1"), "GET /topics/a%2Fb?qos=1 HTTP/1.1");

    for bad in [
        "http://user:pw@host/",
        "http://user@host/",
        "http://[fe80::1]/",
        "http://host:99999/",
        "http://host:0/",
        "http://host:8o/",
        "http://host:-1/",
        "http://host/a b",
        "http://host/a\r\nX: 1",
        "http://host/\u{e9}",
        "http://ho st/",
        "http://h\u{e9}/",
        "http:///path",
        "http://",
        "http://:80/",
        "ftp://host/",
        "http:/host/",
        "host/path",
        "",
    ] {
        assert_eq!(Url::parse(bad).map(|_| ()), Err(HttpError::Url), "{bad:?}");
    }
}

// ── Fuzz ───────────────────────────────────────────────────────────────────

#[test]
fn mutated_answers_neither_panic_nor_spin() {
    let mut rnd = xorshift(0x9E37_79B9_7F4A_7C15);
    let alphabet = b"0123456789abcdefABCDEF;:= \r\n\t,HTTP/1.chunkedContent-LengthTransfer-Encoding\x00\xff";
    for i in 0..FUZZ_RUNS {
        let (base, method) = CASES[(rnd() as usize) % CASES.len()];
        let mut input = base.to_vec();
        for _ in 0..(rnd() % 6) {
            let at = if input.is_empty() { 0 } else { (rnd() as usize) % input.len() };
            let b = alphabet[(rnd() as usize) % alphabet.len()];
            match rnd() % 3 {
                0 if !input.is_empty() => input[at] = b,
                1 => input.insert(at, b),
                _ if !input.is_empty() => {
                    input.remove(at);
                }
                _ => {}
            }
        }
        let step = 1 + (rnd() as usize) % 7;
        let pending = rnd().is_multiple_of(2);
        let cap = (rnd() as usize) % 33;
        let run = std::panic::catch_unwind(|| answer(&input, method, step, pending, cap));
        assert!(
            run.is_ok(),
            "run {i} panicked: {method:?}, reads of {step}, pending {pending}, body {cap}: {:?}",
            String::from_utf8_lossy(&input)
        );
    }
}

#[test]
fn mutated_urls_never_break_the_request_line() {
    let mut rnd = xorshift(0x2545_F491_4F6C_DD1D);
    let bases = [
        "http://192.168.1.10:8123/api/states/sensor.t?x=1#frag",
        "https://api.thingspeak.com?api_key=K&field1=1",
        "https://example.org:8443/topics/a%2Fb?qos=1",
        "http://h",
    ];
    let alphabet = [
        ':', '/', '?', '#', '@', '[', ']', ' ', '\r', '\n', '\0', '\t', '%', '.', '-', '_', '9', '0', 'a',
        'Z', '\u{e9}', '\u{7f}',
    ];
    let mut parsed = 0;
    for i in 0..FUZZ_RUNS {
        let mut url: Vec<char> = bases[(rnd() as usize) % bases.len()].chars().collect();
        for _ in 0..(1 + rnd() % 4) {
            let at = (rnd() as usize) % (url.len() + 1);
            let c = alphabet[(rnd() as usize) % alphabet.len()];
            match rnd() % 3 {
                0 if at < url.len() => url[at] = c,
                1 => url.insert(at, c),
                _ if at < url.len() => {
                    url.remove(at);
                }
                _ => {}
            }
        }
        let url: String = url.into_iter().collect();
        let Ok(parsed_url) = Url::parse(&url) else {
            continue;
        };
        parsed += 1;
        let mut head = [0u8; REQUEST_HEAD_MAX];
        let n = build(&mut head, Method::Get, &parsed_url, Auth::None, None, &[], 0)
            .unwrap_or_else(|e| panic!("run {i}: {url:?} parsed but did not build: {e:?}"));
        let text = std::str::from_utf8(&head[..n]).unwrap();
        // Every CR and LF is one of the head's own line ends...
        let crlf = text.matches("\r\n").count();
        assert_eq!(text.matches('\r').count(), crlf, "run {i}: {url:?}");
        assert_eq!(text.matches('\n').count(), crlf, "run {i}: {url:?}");
        // ...and the request line is three parts, the target visible ASCII.
        let line = text.lines().next().unwrap();
        let words: Vec<&str> = line.split(' ').collect();
        assert_eq!(words.len(), 3, "run {i}: {url:?} -> {line:?}");
        assert!(words[1].starts_with('/'), "run {i}: {url:?} -> {line:?}");
        assert!(words[1].bytes().all(|b| (0x21..=0x7e).contains(&b)), "run {i}: {url:?}");
        assert!(!parsed_url.host.is_empty() && !parsed_url.host.contains(['/', '?', '#', ':', '@']));
    }
    // The mutations must leave enough URLs whole for the check to mean anything.
    assert!(parsed > FUZZ_RUNS / 6, "only {parsed} mutated URLs parsed");
}
