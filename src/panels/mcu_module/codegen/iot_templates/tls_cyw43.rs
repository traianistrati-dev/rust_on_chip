
// Everything below is editable — your changes are preserved on regeneration.
//
// TLS 1.3 (embedded-tls) for every client the IoT tab generates: mqtt_link.rs
// and http.rs hand `open()` a connected socket and a `Target` - the name the
// server's certificate must carry, whether to check it, the CAs it must end
// in, the board's own certificate - and get the connection back, handshake
// done. Checked with VERIFY: every signature along the chain, that each
// certificate after the server's own is a CA, the name (the first 3 DNS
// names, or the CN; never an IP address), and the dates once wall_clock()
// knows the time (SNTP). Not checked: revocation. Without it the traffic is
// encrypted but nobody is authenticated.
//
// TLS 1.3 only, AES-128-GCM with P-256: a server that speaks only TLS 1.2
// cannot be reached. Every certificate in a chain must be signed with RSA or
// ECDSA P-256 - or P-384 with the tab's "P-384 chains" (embedded-tls's
// `p384`, about 114 KiB of flash more). Checking RSA takes the heap, about
// 22 KB at its peak - on a Pico this file brings that heap (32 KiB), and one
// handshake at a time, so two never need it at once.

use core::net::Ipv4Addr;

use embassy_net::tcp::{Error as TcpError, State, TcpSocket};
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use embassy_time::Timer;
use embedded_io_async::{ErrorType, Read, Write};
use embedded_tls::{
    Aes128GcmSha256, Certificate, CryptoProvider, CryptoRngCore, SignatureScheme, TlsConfig,
    TlsConnection, TlsContext, TlsError, TlsVerifier,
};

// ── This chip's pieces ──
/// The chip's raw entropy - `entropy()` in the block above: the ring
/// oscillator on the Pico W, the TRNG and the oscillator on the Pico 2 W -
/// whitened: each 32 bytes out are the SHA-256 of ENTROPY_BYTES raw bytes
/// and a counter.
struct TlsRng {
    counter: u64,
}

impl rand_core_06::RngCore for TlsRng {
    fn next_u32(&mut self) -> u32 {
        let mut b = [0u8; 4];
        self.fill_bytes(&mut b);
        u32::from_le_bytes(b)
    }

    fn next_u64(&mut self) -> u64 {
        let mut b = [0u8; 8];
        self.fill_bytes(&mut b);
        u64::from_le_bytes(b)
    }

    fn fill_bytes(&mut self, dest: &mut [u8]) {
        use sha2::Digest as _;
        for chunk in dest.chunks_mut(32) {
            let mut raw = [0u8; ENTROPY_BYTES];
            entropy(&mut raw);
            let mut hash = sha2::Sha256::new();
            hash.update(raw);
            hash.update(self.counter.to_le_bytes());
            self.counter = self.counter.wrapping_add(1);
            let out = hash.finalize();
            chunk.copy_from_slice(&out[..chunk.len()]);
        }
    }

    fn try_fill_bytes(&mut self, dest: &mut [u8]) -> Result<(), rand_core_06::Error> {
        self.fill_bytes(dest);
        Ok(())
    }
}

impl rand_core_06::CryptoRng for TlsRng {}

fn tls_rng() -> Result<TlsRng, ()> {
    Ok(TlsRng { counter: 0 })
}

/// RSA (num-bigint) allocates, and the Pico has no heap of its own: TLS
/// brings one. Only the handshake's certificate checks use it (about 22 KB
/// at their peak, verifying a Let's Encrypt chain).
#[global_allocator]
static HEAP: embedded_alloc::LlffHeap = embedded_alloc::LlffHeap::empty();
const HEAP_SIZE: usize = 32 * 1024;

/// Called before a handshake: gives the heap its memory, the first time -
/// MQTT and HTTP both call it.
pub fn init() {
    static DONE: embassy_sync::blocking_mutex::Mutex<CriticalSectionRawMutex, core::cell::Cell<bool>> =
        embassy_sync::blocking_mutex::Mutex::new(core::cell::Cell::new(false));
    static mut MEMORY: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
    if DONE.lock(|done| done.replace(true)) {
        return;
    }
    // SAFETY: reached once, before the first allocation; nothing else
    // touches MEMORY.
    unsafe { HEAP.init(&raw mut MEMORY as usize, HEAP_SIZE) }
}
// ── end of this chip's pieces ──
