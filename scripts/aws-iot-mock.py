#!/usr/bin/env python3
r"""A local AWS IoT Core look-alike, to test the IoT tab's "Test from this PC"
helper - or a real board - with no AWS account.

One MQTT broker behind TLS, with one registered thing, `rustonchip-test`. Its
certificate is the board's own: the helper's `csr` mode makes the P-256 key
and its CSR, and a mock CA signs the CSR, as `aws iot
create-certificate-from-csr` would. The thing's policy is:

    iot:Connect               client/rustonchip-test
    iot:Publish, iot:Receive  topic/rustonchip-test/*
    iot:Subscribe             topicfilter/rustonchip-test/*

with `*` the only wildcard (`+` and `#` are literal, as in AWS policies).

Only the TLS handshake copies AWS byte for byte, from probes of a real
endpoint: TLS 1.3, an RSA chain [leaf, "M01", "Root CA 1" cross-signed by
"Starfield G2"] whose look-alikes the board trusts as a self-signed Root CA 1
(plus an EC "Root CA 3"), port 443 routed by ALPN (x-amzn-mqtt-ca = MQTT, none
= the silent HTTPS side), and a TCP reset, no alert, for a missing or
unregistered client certificate. The MQTT side follows what AWS documents
(developerguide/mqtt.html and the message-broker limits in
general/latest/gr/iot-core.html), not captured traffic.

HTTPS publish (`aws_publish()`, the helper's `http` mode): `POST
/topics/<topic>?qos=0|1` on "8443", or on "443" with the ALPN x-amzn-http-ca,
with the board's certificate - checked as on the MQTT side (a reset for a
missing or unregistered one: AWS's on 8443 probed, on 443 assumed alike) -
and delivered to the MQTT subscribers. What it answers follows AWS's
documentation and the captures others published, NOT traffic this mock was
built from: 200 {"message":"OK","traceId":...}, 403 {"message":"Forbidden",
...} outside the policy (the documented ForbiddenException), 400 for a QoS
other than 0 or 1, 413 over 128 KB. AWS's own status for those may differ.

The Device Shadow's REST API (`shadow_get()` and the rest), on the same
side: `GET|POST|DELETE /things/<thing>/shadow[?name=<shadow>]`, allowed by
iot:Get/Update/DeleteThingShadow on thing/<thing> and thing/<thing>/* only
with --allow-shadow. It keeps the documents, merges updates (a key set to
null goes), counts versions, works out the delta, answers with AWS's
documented error texts ("Missing required node: state", "Version conflict",
...) in `{"code":..,"message":..}` - a shape AWS shows for its MQTT errors,
assumed for REST - and publishes .../update/accepted, /documents and /delta
to MQTT subscribers that may receive them (--allow '$aws/things/<thing>/shadow/#').
Its limits: 8 KB of state (compact JSON; whitespace is not counted), 6
levels. Again documentation, not traffic.

Needs Python 3.8+ (standard library only) and openssl: $OPENSSL, then PATH,
then Git for Windows' copy. The PKI and scratch files go to --work (default:
a folder in the temp directory); they are made once and reused.

    python scripts/aws-iot-mock.py [--helper EXE] [--work DIR] [--aws-sigalgs] [-v] [CASE ...]

runs the self-checking cases against the helper (--helper, else
$RUSTONCHIP_TLS_CHECK, else the one the IDE builds for "Test from this PC"
under %LOCALAPPDATA%\embedded_ide_0 - press that button once first): one
PASS / FAIL / SKIP line each, and exit code 1 on any FAIL. CASE picks some by id: `1` runs
1a, 1b and 1c, `5b` just that one. -v prints every case's helper output and
broker log.

    python scripts/aws-iot-mock.py serve [--bind 127.0.0.1] [--port-443 18443] [--port-8883 18883] [--port-8443 28443]

runs only the broker, for a real board, and prints what to set in the IoT
tab. serve also takes --allow FILTER, --deny rst, --reason-strings,
--retained-before-suback, --retain TOPIC=TEXT, --csr FILE (sign the IDE's own
CSR and register it) and --tls12; see `serve --help`.

--aws-sigalgs makes the CertificateRequest list AWS's 16 signature schemes in
AWS's order. Python has no API for that list, so a ctypes call reaches into
the OpenSSL that Python's ssl module uses. Where that cannot be done safely,
the script warns and goes on with OpenSSL's own list.
"""

import argparse
import base64
import binascii
import collections
import concurrent.futures
import ctypes
import datetime
import hashlib
import json
import os
import pathlib
import platform
import re
import select
import shutil
import socket
import ssl
import struct
import subprocess
import sys
import tempfile
import threading
import time

REGION = "eu-south-1"
ENDPOINT = "a1b2c3d4e5f6g7-ats.iot." + REGION + ".amazonaws.com"
THING = "rustonchip-test"
ALPN_MQTT = "x-amzn-mqtt-ca"
ALPN_HTTP = "x-amzn-http-ca"
# HTTPS publish: AWS's documented payload limit.
HTTP_PUBLISH_MAX = 128 * 1024

# Where the IDE builds the helper for "Test from this PC": beside its
# workspace, for the host's triple (Windows).
DEFAULT_HELPER = os.path.join(
    os.environ.get("LOCALAPPDATA", ""), "embedded_ide_0", "rustonchip_tls_check", "target",
    "x86_64-pc-windows-msvc", "release", "rustonchip-tls-check.exe")
GIT_OPENSSL = r"C:\Program Files\Git\mingw64\bin\openssl.exe"
PKI_VERSION = "aws-iot-mock pki 1 " + THING + " " + REGION

# AWS's TLS 1.3 CertificateRequest, captured in eu-south-1 on 443 and 8883: an
# empty context and one extension, signature_algorithms, with 16 schemes:
# 0809 080a 080b 0804 0805 0806 0401 0501 0601 0301 0403 0503 0603 0303 0201 0203
AWS_CR_SIGALGS = (b"rsa_pss_pss_sha256:rsa_pss_pss_sha384:rsa_pss_pss_sha512:rsa_pss_rsae_sha256:"
                  b"rsa_pss_rsae_sha384:rsa_pss_rsae_sha512:rsa_pkcs1_sha256:rsa_pkcs1_sha384:"
                  b"rsa_pkcs1_sha512:rsa_pkcs1_sha224:ecdsa_secp256r1_sha256:ecdsa_secp384r1_sha384:"
                  b"ecdsa_secp521r1_sha512:ECDSA+SHA224:rsa_pkcs1_sha1:ECDSA+SHA1")
AWS_CR = bytes.fromhex("0d000029000026000d002200200809080a080b0804080508060401050106010301040305030603030302010203")
# ... and its EncryptedExtensions on 443: the SNI acknowledged, the ALPN echoed.
AWS_EE = bytes.fromhex("0800001b00190000000000100011000f0e782d616d7a6e2d6d7174742d6361")

# The quotas AWS documents (message-broker limits).
MAX_SLASHES = 7
MAX_SUBSCRIPTIONS = 50
MIN_CLIENT_PACKET = 512
MAX_PACKET = 149504
MAX_CLIENT_ID = 128


def hexl(value):
    if isinstance(value, str):
        value = value.encode("utf-8")
    return binascii.hexlify(value).decode("ascii")


def fingerprint(der):
    return hashlib.sha256(der).hexdigest()


def default_work():
    return os.path.join(tempfile.gettempdir(), "rustonchip-aws-iot-mock")


def find_openssl():
    env = os.environ.get("OPENSSL")
    for candidate in (env and shutil.which(env), shutil.which("openssl"), GIT_OPENSSL):
        if candidate and os.path.isfile(candidate):
            return candidate
    sys.exit("openssl not found: set OPENSSL to its path")


def find_helper(arg):
    return arg or os.environ.get("RUSTONCHIP_TLS_CHECK") or DEFAULT_HELPER


def run_helper_csr(helper, cn):
    """The board's key and CSR, from the helper's `csr` mode - as the IDE makes them."""
    lines = [hexl("RO"), hexl("RustOnChip"), hexl(cn), ""]
    r = subprocess.run([helper, "csr"], input=("\n".join(lines) + "\n").encode("ascii"),
                       capture_output=True, timeout=60)
    got = {}
    for line in r.stdout.decode("utf-8", "replace").splitlines():
        tag, _, value = line.partition(" ")
        if tag in ("KEY", "CSR"):
            got[tag] = binascii.unhexlify(value.strip()).decode("ascii")
    return got.get("KEY"), got.get("CSR")


# --- PKI -----------------------------------------------------------------------

OPENSSL_CNF = """\
# Written by aws-iot-mock.py: what `openssl req` needs, and each kind of certificate.
[req]
distinguished_name = req_dn
prompt = no
[req_dn]
CN = unused
[root]
basicConstraints=critical,CA:TRUE
keyUsage=critical,digitalSignature,keyCertSign,cRLSign
subjectKeyIdentifier=hash
[xroot]
basicConstraints=critical,CA:TRUE
keyUsage=critical,digitalSignature,keyCertSign,cRLSign
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
[inter]
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,digitalSignature,keyCertSign,cRLSign
extendedKeyUsage=serverAuth,clientAuth
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
authorityInfoAccess=OCSP;URI:http://ocsp.example.test,caIssuers;URI:http://crt.example.test/root1.cer
crlDistributionPoints=URI:http://crl.example.test/root1.crl
certificatePolicies=2.23.140.1.2.1
[leaf]
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
subjectAltName=DNS:*.iot.{region}.amazonaws.com,DNS:iot.{region}.amazonaws.com
certificatePolicies=2.23.140.1.2.1
authorityInfoAccess=OCSP;URI:http://ocsp.example.test,caIssuers;URI:http://crt.example.test/m01.cer
crlDistributionPoints=URI:http://crl.example.test/m01.crl
1.3.6.1.4.1.11129.2.4.2=DER:{sct}
[device]
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature
subjectKeyIdentifier=hash
authorityKeyIdentifier=keyid:always
"""

# Look-alikes of AWS's chain, every name marked "Test" - nothing here passes for the real CAs.
SUBJECTS = {
    "class2": "/C=US/O=Test Starfield Technologies, Inc./OU=Test Starfield Class 2 Certification Authority",
    "sfg2": "/C=US/ST=Arizona/L=Scottsdale/O=Test Starfield Technologies, Inc./"
            "CN=Test Starfield Services Root Certificate Authority - G2",
    "root1": "/C=US/O=Test Amazon/CN=Test Amazon Root CA 1",
    "root3": "/C=US/O=Test Amazon/CN=Test Amazon Root CA 3",
    "m01": "/C=US/O=Test Amazon/CN=Test Amazon RSA 2048 M01",
    "leaf": "/CN=*.iot." + REGION + ".amazonaws.com",
    "iotca": "/C=US/ST=Washington/L=Seattle/O=Test Amazon.com Inc./OU=Test Amazon Web Services",
}


def fake_sct_hex():
    # A Certificate Transparency extension of a real one's size, so the leaf weighs what AWS's does.
    inner = (360).to_bytes(2, "big") + os.urandom(360)
    return "0482" + format(len(inner), "04x") + inner.hex()


def days_to_2049():
    # AWS's client certificates run to 2049-12-31.
    return (datetime.date(2049, 12, 31) - datetime.date.today()).days


class Pki:
    """The certificates, made once in `work` with openssl."""

    def __init__(self, work, openssl, helper):
        self.work = pathlib.Path(work)
        self.openssl = openssl
        self.helper = helper

    def path(self, name):
        return self.work / name

    def ossl(self, *args, check=True):
        env = dict(os.environ, OPENSSL_CONF=str(self.path("openssl.cnf")))
        r = subprocess.run([self.openssl] + [str(a) for a in args], cwd=str(self.work), env=env, capture_output=True)
        if check and r.returncode:
            sys.exit("openssl %s failed: %s" % (" ".join(map(str, args)), r.stderr.decode("utf-8", "replace").strip()))
        return r

    def der(self, name):
        return ssl.PEM_cert_to_DER_cert(self.path(name + ".pem").read_text())

    def ensure(self):
        marker = self.path("pki.txt")
        if not marker.exists() or marker.read_text().strip() != PKI_VERSION:
            print("making the look-alike PKI in %s ..." % self.work, flush=True)
            self.build()
            marker.write_text(PKI_VERSION + "\n")
        elif self.ossl("x509", "-checkend", "86400", "-noout", "-in", "leaf.pem", check=False).returncode:
            self.issue_leaf()  # the broker's certificate lives about a year, as AWS's do
        return self

    def sign(self, name, ext, days, ca=None, out=None, csr=None):
        args = ["x509", "-req", "-in", csr or name + ".csr", "-days", days, "-sha256",
                "-set_serial", "0x" + os.urandom(12).hex(), "-extfile", "openssl.cnf", "-extensions", ext,
                "-out", (out or name) + ".pem"]
        args += ["-signkey", name + ".key"] if ca is None else ["-CA", ca + ".pem", "-CAkey", ca + ".key"]
        self.ossl(*args)

    def build(self):
        self.work.mkdir(parents=True, exist_ok=True)
        cnf = OPENSSL_CNF.replace("{region}", REGION).replace("{sct}", fake_sct_hex())
        self.path("openssl.cnf").write_bytes(cnf.encode("ascii"))
        for name in ("class2", "sfg2", "root1", "m01", "leaf", "iotca"):
            self.ossl("genrsa", "-out", name + ".key", 2048)
        self.ossl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", "root3.key")
        for name, subject in SUBJECTS.items():
            self.ossl("req", "-new", "-key", name + ".key", "-subj", subject, "-out", name + ".csr")
        self.sign("class2", "root", 7300)
        self.sign("sfg2", "root", 7300)
        self.sign("sfg2", "xroot", 5000, ca="class2", out="sfg2_x")     # AWS's 4th certificate over TLS 1.2
        self.sign("root1", "root", 7300)                                # what the board trusts
        self.sign("root1", "xroot", 5000, ca="sfg2", out="root1_x")     # the 3rd certificate AWS sends
        self.sign("root3", "root", 7300)
        self.sign("m01", "inter", 2900, ca="root1")
        self.sign("iotca", "root", 7300)                                # signs the boards; the board never sees it
        self.issue_leaf()
        # The board, and a second key whose certificate the CA signs but nobody registers.
        for name, cn in (("device", THING), ("other", "rustonchip-other")):
            self.board_key(name, cn)
            self.sign(name, "device", days_to_2049(), ca="iotca", csr=name + ".csr")
        # A certificate no registered CA issued: the second key, self-signed.
        self.ossl("req", "-new", "-x509", "-key", "other.key", "-subj", "/CN=self-signed board", "-days", 30,
                  "-out", "selfsigned.pem")

    def issue_leaf(self):
        self.sign("leaf", "leaf", 395, ca="m01")
        chain3 = b"".join(self.path(f).read_bytes() for f in ("leaf.pem", "m01.pem", "root1_x.pem"))
        self.path("chain3.pem").write_bytes(chain3)
        self.path("chain4.pem").write_bytes(chain3 + self.path("sfg2_x.pem").read_bytes())

    def board_key(self, name, cn):
        if self.helper and os.path.isfile(self.helper):
            key, csr = run_helper_csr(self.helper, cn)
            if key and csr:
                self.path(name + ".key").write_bytes(key.encode("ascii"))
                self.path(name + ".csr").write_bytes(csr.encode("ascii"))
                return
            print("warning: the helper's csr mode gave no key: openssl makes it", flush=True)
        self.ossl("ecparam", "-name", "prime256v1", "-genkey", "-noout", "-out", name + ".key")
        self.ossl("req", "-new", "-key", name + ".key", "-subj", "/C=RO/O=RustOnChip/CN=" + cn, "-out", name + ".csr")

    def sign_csr(self, csr_file):
        """What `aws iot create-certificate-from-csr --set-as-active` does to the IDE's own CSR."""
        data = pathlib.Path(csr_file).read_bytes()
        name = "csr-" + hashlib.sha256(data).hexdigest()[:12]
        self.path(name + ".csr").write_bytes(data)
        self.sign(name, "device", days_to_2049(), ca="iotca", csr=name + ".csr")
        return self.path(name + ".pem")


# --- The CertificateRequest's signature list (--aws-sigalgs) -------------------

_sigalgs_warned = []


def _libssl():
    """The libssl Python's own ssl module runs on (a second copy would hold other SSL_CTXs)."""
    import _ssl
    path = getattr(_ssl, "__file__", None)
    if not path:
        return None  # ssl built into the interpreter
    if os.name == "nt":
        folder = os.path.dirname(path)
        for name in sorted(os.listdir(folder)):
            if name.lower().startswith("libssl") and name.lower().endswith(".dll"):
                return ctypes.CDLL(os.path.join(folder, name))
        return None
    return ctypes.CDLL(path)  # dlsym finds SSL_* through _ssl's own libssl


def set_cr_sigalgs(ctx):
    """SSL_CTX_set1_client_sigalgs_list on `ctx`: the list a server's CertificateRequest carries.
    True when it took; a warning (once) and False otherwise - never a crash we can foresee."""
    def skip(why):
        if not _sigalgs_warned:
            _sigalgs_warned.append(why)
            print("warning: --aws-sigalgs skipped (%s): the CertificateRequest keeps OpenSSL's own list" % why,
                  flush=True)
        return False

    try:
        import _ssl
        if platform.python_implementation() != "CPython" or not isinstance(ctx, _ssl._SSLContext):
            return skip("not CPython's own SSLContext")
        lib = _libssl()
        if lib is None:
            return skip("no libssl next to Python's _ssl")
        # PySSLContext is PyObject_HEAD, then SSL_CTX *ctx; object()'s size is that head.
        head = sys.getsizeof(object())
        if _ssl._SSLContext.__basicsize__ < head + ctypes.sizeof(ctypes.c_void_p):
            return skip("PySSLContext is smaller than expected")
        raw = ctypes.c_void_p.from_address(id(ctx) + head).value
        if not raw or raw % ctypes.sizeof(ctypes.c_void_p):
            return skip("no SSL_CTX pointer where CPython keeps it")
        lib.SSL_CTX_get_options.argtypes = [ctypes.c_void_p]
        lib.SSL_CTX_get_options.restype = ctypes.c_ulong
        lib.SSL_CTX_get_verify_mode.argtypes = [ctypes.c_void_p]
        lib.SSL_CTX_get_verify_mode.restype = ctypes.c_int
        lib.SSL_CTX_ctrl.argtypes = [ctypes.c_void_p, ctypes.c_int, ctypes.c_long, ctypes.c_void_p]
        lib.SSL_CTX_ctrl.restype = ctypes.c_long
        # The pointer check: OpenSSL must report what Python set on this very context.
        if lib.SSL_CTX_get_options(raw) & 0xFFFFFFFF != int(ctx.options) & 0xFFFFFFFF:
            return skip("the SSL_CTX's options are not the context's")
        want = {ssl.CERT_NONE: 0, ssl.CERT_OPTIONAL: 1, ssl.CERT_REQUIRED: 3}[ctx.verify_mode]
        if lib.SSL_CTX_get_verify_mode(raw) != want:
            return skip("the SSL_CTX's verify mode is not the context's")
        if lib.SSL_CTX_ctrl(raw, 102, 0, ctypes.c_char_p(AWS_CR_SIGALGS)) != 1:  # SSL_CTRL_SET_CLIENT_SIGALGS_LIST
            return skip("OpenSSL refused the list")
        return True
    except Exception as e:  # noqa: BLE001 - any surprise here means: leave OpenSSL alone
        return skip("%s: %s" % (type(e).__name__, e))


# --- TLS: the ClientHello, peeked -------------------------------------------------

def peek_hello(conn, wait=10):
    """The ClientHello, read without consuming it: SNI, ALPN and versions decide the route."""
    deadline = time.monotonic() + wait
    while True:
        data = conn.recv(16384, socket.MSG_PEEK)
        if not data or data[0] != 0x16:
            return None
        if len(data) >= 5 and len(data) >= 5 + int.from_bytes(data[3:5], "big"):
            break
        if time.monotonic() > deadline:
            return None
        time.sleep(0.01)
    try:
        return parse_hello(data[5:5 + int.from_bytes(data[3:5], "big")])
    except IndexError:
        return None


def parse_hello(rec):
    if rec[0] != 1:
        return None
    b = rec[4:]
    p = 2 + 32
    p += 1 + b[p]                                       # session id
    p += 2 + int.from_bytes(b[p:p + 2], "big")          # cipher suites
    p += 1 + b[p]                                       # compression
    end = p + 2 + int.from_bytes(b[p:p + 2], "big")
    p += 2
    hello = {"sni": None, "alpn": [], "versions": []}
    while p + 4 <= end:
        kind, size = int.from_bytes(b[p:p + 2], "big"), int.from_bytes(b[p + 2:p + 4], "big")
        d = b[p + 4:p + 4 + size]
        if kind == 0:
            hello["sni"] = d[5:5 + int.from_bytes(d[3:5], "big")].decode("ascii", "replace")
        elif kind == 16:
            q = 2
            while q < len(d):
                hello["alpn"].append(d[q + 1:q + 1 + d[q]].decode("ascii", "replace"))
                q += 1 + d[q]
        elif kind == 43:
            hello["versions"] = [d[1 + i:3 + i].hex() for i in range(0, d[0], 2)]
        p += 4 + size
    return hello


def reset(sock):
    """Close with a TCP RST and no TLS alert - what AWS does to a certificate it refuses."""
    try:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("HH" if os.name == "nt" else "ii", 1, 0))
    except OSError:
        pass
    close_quietly(sock)


def close_quietly(sock):
    try:
        sock.close()
    except OSError:
        pass


# --- MQTT encoding --------------------------------------------------------------

PROPERTY = {
    0x01: ("Payload Format Indicator", "u8"), 0x02: ("Message Expiry Interval", "u32"),
    0x03: ("Content Type", "str"), 0x08: ("Response Topic", "str"), 0x09: ("Correlation Data", "bin"),
    0x0B: ("Subscription Identifier", "vbi"), 0x11: ("Session Expiry Interval", "u32"),
    0x12: ("Assigned Client Identifier", "str"), 0x13: ("Server Keep Alive", "u16"),
    0x15: ("Authentication Method", "str"), 0x16: ("Authentication Data", "bin"),
    0x17: ("Request Problem Information", "u8"), 0x18: ("Will Delay Interval", "u32"),
    0x19: ("Request Response Information", "u8"), 0x1A: ("Response Information", "str"),
    0x1C: ("Server Reference", "str"), 0x1F: ("Reason String", "str"), 0x21: ("Receive Maximum", "u16"),
    0x22: ("Topic Alias Maximum", "u16"), 0x23: ("Topic Alias", "u16"), 0x24: ("Maximum QoS", "u8"),
    0x25: ("Retain Available", "u8"), 0x26: ("User Property", "pair"), 0x27: ("Maximum Packet Size", "u32"),
    0x28: ("Wildcard Subscription Available", "u8"), 0x29: ("Subscription Identifier Available", "u8"),
    0x2A: ("Shared Subscription Available", "u8"),
}


class PacketTooLarge(Exception):
    pass


def vbi(n):
    out = bytearray()
    while True:
        b, n = n % 128, n // 128
        out.append(b | (0x80 if n else 0))
        if not n:
            return bytes(out)


def packet(first, body):
    return bytes([first]) + vbi(len(body)) + body


def field(data):
    if isinstance(data, str):
        data = data.encode("utf-8")
    return len(data).to_bytes(2, "big") + data


def properties(*items):
    body = b"".join(items)
    return vbi(len(body)) + body


def p8(pid, v):
    return bytes([pid, v])


def p16(pid, v):
    return bytes([pid]) + v.to_bytes(2, "big")


def p32(pid, v):
    return bytes([pid]) + v.to_bytes(4, "big")


def pstr(pid, text):
    return bytes([pid]) + field(text)


def read_exact(s, n):
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise ConnectionError("the peer closed the connection")
        buf += chunk
    return buf


def read_packet(s):
    first = read_exact(s, 1)[0]
    n, shift = 0, 0
    while True:
        b = read_exact(s, 1)[0]
        n |= (b & 0x7F) << shift
        shift += 7
        if not b & 0x80 or shift > 21:
            break
    if n > MAX_PACKET:
        raise PacketTooLarge("a packet of %d bytes" % n)
    return first, read_exact(s, n) if n else b""


def u16(b, p):
    if p + 2 > len(b):
        raise ValueError("a packet cut short")
    return int.from_bytes(b[p:p + 2], "big"), p + 2


def mstr(b, p):
    n, p = u16(b, p)
    if p + n > len(b):
        raise ValueError("a string longer than its packet")
    return b[p:p + n], p + n


def read_vbi(b, p):
    n, shift = 0, 0
    while True:
        n |= (b[p] & 0x7F) << shift
        shift += 7
        p += 1
        if not b[p - 1] & 0x80:
            return n, p


def parse_props(b):
    """MQTT 5 properties as (id, value) in their order: numbers as ints, strings as bytes."""
    out, p = [], 0
    while p < len(b):
        pid = b[p]
        p += 1
        if pid not in PROPERTY:
            raise ValueError("unknown property 0x%02X" % pid)
        kind = PROPERTY[pid][1]
        if kind == "u8":
            value, p = b[p], p + 1
        elif kind == "u16":
            value, p = u16(b, p)
        elif kind == "u32":
            value, p = int.from_bytes(b[p:p + 4], "big"), p + 4
        elif kind == "vbi":
            value, p = read_vbi(b, p)
        elif kind == "pair":
            key, p = mstr(b, p)
            val, p = mstr(b, p)
            value = (key, val)
        else:
            value, p = mstr(b, p)
        out.append((pid, value))
    return out


def prop(props, pid, default=None):
    return next((v for i, v in props if i == pid), default)


def describe_props(props):
    def show(v):
        return repr(v.decode("utf-8", "replace")) if isinstance(v, bytes) else str(v)
    return ", ".join("%s=%s" % (PROPERTY[i][0], show(v)) for i, v in props) or "none"


def parse_connect(b):
    name, p = mstr(b, 0)
    level, flags = b[p], b[p + 1]
    keep, p = u16(b, p + 2)
    c = {"protocol": name.decode("utf-8", "replace"), "level": level, "flags": flags, "keep_alive": keep, "props": []}
    if level == 5:
        n, p = read_vbi(b, p)
        c["props"], p = parse_props(b[p:p + n]), p + n
    cid, p = mstr(b, p)
    c["client_id"] = cid.decode("utf-8")
    if flags & 0x04:  # a will
        if level == 5:
            n, p = read_vbi(b, p)
            p += n
        c["will_topic"], p = mstr(b, p)
        _, p = mstr(b, p)
    if flags & 0x80:
        c["user"], p = mstr(b, p)
    if flags & 0x40:
        c["password"], p = mstr(b, p)
    return c


def server_keep_alive(asked):
    """The quota page: 0 or more than 1200 s is taken as 1200 s, 1..29 s as 30 s."""
    if asked == 0 or asked > 1200:
        return 1200
    return max(asked, 30)


def connack_properties(keep_alive):
    """The terms AWS's CONNACK carries, as AWS's own MQTT 5 blog shows them."""
    return properties(p32(0x11, 0), p16(0x13, keep_alive), p16(0x21, 100), p16(0x22, 8), p8(0x24, 1),
                      p8(0x25, 1), p32(0x27, MAX_PACKET), p8(0x28, 1), p8(0x29, 0), p8(0x2A, 1))


# --- Topics and the policy ---------------------------------------------------------

def strip_share(flt):
    if flt.startswith("$share/"):
        parts = flt.split("/", 2)
        return parts[2] if len(parts) > 2 else ""
    return flt


def slashes(name):
    """The '/' AWS counts against its limit of 7: not those of $share/<name>/ or $aws/rules/<rule>/."""
    if name.startswith("$aws/rules/"):
        parts = name.split("/", 3)
        name = parts[3] if len(parts) > 3 else ""
    return strip_share(name).count("/")


def valid_filter(flt):
    if flt.startswith("$share/"):
        parts = flt.split("/", 2)
        if len(parts) < 3 or not parts[1] or "+" in parts[1] or "#" in parts[1]:
            return False
        flt = parts[2]
    if not flt:
        return False
    levels = flt.split("/")
    for i, level in enumerate(levels):
        if "#" in level and (level != "#" or i != len(levels) - 1):
            return False
        if "+" in level and level != "+":
            return False
    return True


def filter_matches(flt, topic):
    """MQTT matching, with AWS's difference: `a/#` does not match `a`."""
    f, t = strip_share(flt).split("/"), topic.split("/")
    if topic.startswith("$") and f[0] in ("+", "#"):
        return False
    for i, level in enumerate(f):
        if level == "#":
            return len(t) > i
        if i >= len(t) or (level != "+" and level != t[i]):
            return False
    return len(f) == len(t)


def policy_match(pattern, value):
    """An AWS policy resource: `*` matches anything, `/` included; `+` and `#` are just characters."""
    regex = ".*".join(re.escape(part) for part in pattern.split("*"))
    return re.fullmatch(regex, value, re.DOTALL) is not None


# --- The broker ------------------------------------------------------------------

class Options:
    """What the broker does where AWS leaves a choice, or where a case bends it."""

    def __init__(self, **changes):
        self.deny = "connack"              # client id not the thing: CONNACK 0x87, or "rst"
        self.reason_strings = False        # reason strings even after Request Problem Information 0
        self.retained_before_suback = False
        self.allow = []                    # more filters: iot:Subscribe and iot:Receive
        self.retained = {}                 # topic -> payload, there before anyone connects
        self.aws_sigalgs = False
        self.tls12 = False                 # an endpoint with no TLS 1.3 security policy
        self.chain = "chain3.pem"          # what AWS sends over TLS 1.3 (chain4.pem: over TLS 1.2)
        self.tickets = 0                   # AWS sends no NewSessionTicket
        self.nocert = "rst"                # no client certificate: a reset (AWS), or "alert"
        self.allow_retain = False          # iot:RetainPublish on topic/<thing>/* (--allow-retain)
        self.allow_shadow = False          # iot:*ThingShadow on thing/<thing>[/*] (--allow-shadow)
        self.allow_mqtt_shadow = False     # Publish/Subscribe/Receive on $aws/things/<thing>/shadow/* (--allow-mqtt-shadow)
        self.shadows = {}                  # (thing, name or None) -> {"desired":.., "reported":..}, there at the start
        for key, value in changes.items():
            if not hasattr(self, key):
                raise TypeError("no option " + key)
            setattr(self, key, value)


class Broker:
    def __init__(self, pki, options, echo=False):
        self.pki = pki
        self.o = options
        self.echo = echo
        self.lines = []
        self.lock = threading.Lock()
        self.registry = {fingerprint(pki.der("device")): THING}   # certificate -> its attached thing
        self.retained = dict(options.retained)
        self.shadows = {key: Shadow(state, 1) for key, state in options.shadows.items()}
        self.deleted = {}                                          # (thing, name) -> the version it was deleted at
        self.sessions = {}                                         # client id -> live Session
        self.listeners = []
        self.ports = {}
        self.count = 0
        self.active = 0                                            # connections still being handled
        self.stopped = False
        self.sigalgs_set = False
        self.ctx = self.contexts()

    def log(self, text):
        with self.lock:
            self.lines.append(text)
        if self.echo:
            print(time.strftime("%H:%M:%S ") + text, flush=True)

    def contexts(self):
        def base():
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
            version = ssl.TLSVersion.TLSv1_2 if self.o.tls12 else ssl.TLSVersion.TLSv1_3
            ctx.minimum_version = ctx.maximum_version = version
            ctx.options |= ssl.OP_NO_TICKET           # AWS takes no SessionTicket extension
            if not self.o.tls12:
                ctx.num_tickets = self.o.tickets
            ctx.load_cert_chain(str(self.pki.path(self.o.chain)), str(self.pki.path("leaf.key")))
            # A servername callback makes OpenSSL acknowledge the SNI in EncryptedExtensions, as AWS does.
            ctx.sni_callback = lambda sock, name, c: None
            return ctx

        def client_cert(alpn):
            ctx = base()
            ctx.load_verify_locations(str(self.pki.path("iotca.pem")))
            ctx.verify_mode = ssl.CERT_REQUIRED if self.o.nocert == "alert" else ssl.CERT_OPTIONAL
            ctx.set_alpn_protocols([alpn])
            return ctx

        mqtt = client_cert(ALPN_MQTT)
        http = client_cert(ALPN_HTTP)
        if self.o.aws_sigalgs:
            # wire_check reads the MQTT side's.
            self.sigalgs_set = set_cr_sigalgs(mqtt)
            set_cr_sigalgs(http)
        return {"mqtt": mqtt, "http": http, "https": base()}

    def start(self, bind, port_443, port_8883, port_8443=0):
        for kind, port in (("443", port_443), ("8883", port_8883), ("8443", port_8443)):
            srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            if os.name != "nt":
                srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)  # (on Windows it would share the port)
            try:
                srv.bind((bind, port))
            except OSError:
                srv.close()
                self.stop()
                raise
            srv.listen(16)
            srv.settimeout(0.5)
            self.listeners.append(srv)
            self.ports[kind] = srv.getsockname()[1]
            threading.Thread(target=self.accept_loop, args=(srv, kind), daemon=True).start()
        return self

    def stop(self):
        self.stopped = True
        for srv in self.listeners:
            close_quietly(srv)

    def accept_loop(self, srv, kind):
        while not self.stopped:
            try:
                conn, _ = srv.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            with self.lock:
                self.count += 1
                self.active += 1
                n = self.count
            threading.Thread(target=self.handle_counted, args=(conn, kind, n), daemon=True).start()

    def wait_idle(self, seconds):
        """Until every connection has ended (its last log line written), or `seconds` pass."""
        deadline = time.monotonic() + seconds
        while self.active and time.monotonic() < deadline:
            time.sleep(0.02)

    def handle_counted(self, conn, kind, n):
        try:
            self.handle(conn, kind, n)
        finally:
            with self.lock:
                self.active -= 1

    def handle(self, conn, kind, n):
        def log(text):
            self.log("[%d] %s" % (n, text))

        conn.settimeout(10)
        try:
            hello = peek_hello(conn)
        except OSError as e:
            log("no ClientHello (%s)" % e)
            close_quietly(conn)
            return
        if hello is None:
            log("not a TLS ClientHello: closed")
            close_quietly(conn)
            return
        log('ClientHello on "%s": SNI %r, ALPN %s, versions %s' % (kind, hello["sni"], hello["alpn"], hello["versions"]))
        if self.o.tls12 and hello["versions"] and "0303" not in hello["versions"]:
            # A TLS 1.2-only AWS endpoint answers a TLS 1.3-only ClientHello with a warning
            # close_notify, then a reset (probed in us-west-2).
            conn.sendall(bytes.fromhex("15030300020100"))
            time.sleep(0.05)
            log("TLS 1.3 alone, to a TLS 1.2 endpoint: close_notify, then a reset (as AWS)")
            reset(conn)
            return
        http = kind == "8443" or (kind == "443" and ALPN_HTTP in hello["alpn"])
        mqtt = not http and (kind == "8883" or ALPN_MQTT in hello["alpn"])
        side = "http" if http else ("mqtt" if mqtt else "https")
        try:
            s = self.ctx[side].wrap_socket(conn, server_side=True)
        except (OSError, ssl.SSLError) as e:
            log("TLS handshake failed: %s" % e)
            close_quietly(conn)
            return
        log("%s %s, ALPN %r" % (s.version(), s.cipher()[0], s.selected_alpn_protocol()))
        if side == "https":
            self.https_side(s, log)
            return
        cert = s.getpeercert(binary_form=True)
        if cert is None:
            log("no client certificate: TCP reset, no alert (as AWS)")
            reset(s)
            return
        fp = fingerprint(cert)
        thing = self.registry.get(fp)
        if thing is None:
            log("client certificate %s... is not registered: TCP reset" % fp[:16])
            reset(s)
            return
        log("client certificate %s... is registered, attached to the thing %r" % (fp[:16], thing))
        if http:
            HttpsPublish(self, s, thing, log).run()
        else:
            Session(self, s, thing, log).run()

    def https_side(self, s, log):
        """443 without the MQTT ALPN is AWS's HTTPS / WebSocket side: it asked for no
        certificate, and an MQTT CONNECT there gets no answer at all."""
        s.settimeout(40)
        try:
            data = s.recv(4096)
            log("443 without ALPN = the HTTPS side: got %s... and answers nothing" % data[:12].hex())
            while s.recv(4096):
                pass
        except (OSError, ssl.SSLError) as e:
            log("HTTPS side ended (%s)" % type(e).__name__)
        close_quietly(s)

    def policy(self, thing):
        # --allow FILTER grants Subscribe on the filter as written, and Receive on what it
        # matches - its + and # turned into the policy's own wildcard.
        allow = self.o.allow
        # The tab's policy with the Device Shadow over MQTT: its topics, `*` spanning `/`.
        shadow = ["$aws/things/%s/shadow/*" % thing] if self.o.allow_mqtt_shadow else []
        return {
            "connect": ["client/" + thing],
            "publish": ["topic/%s/*" % thing] + ["topic/" + t for t in shadow],
            "receive": ["topic/%s/*" % thing] + ["topic/" + t for t in shadow]
                       + ["topic/" + strip_share(f).replace("+", "*").replace("#", "*") for f in allow],
            "subscribe": ["topicfilter/%s/*" % thing] + ["topicfilter/" + t for t in shadow]
                         + ["topicfilter/" + f for f in allow],
            "retainpublish": ["topic/%s/*" % thing] if self.o.allow_retain else [],
            # The shadow's REST actions: the classic shadow is thing/<thing>, a named
            # one thing/<thing>/<name> - which thing/<thing> alone does not cover.
            "shadow": ["thing/%s" % thing, "thing/%s/*" % thing] if self.o.allow_shadow else [],
        }

    def may(self, thing, action, value):
        prefix = {"connect": "client/", "subscribe": "topicfilter/", "shadow": ""}.get(action, "topic/")
        return any(policy_match(r, prefix + value) for r in self.policy(thing)[action])

    def shadow_op(self, thing, name, op, body, token=None):
        """One shadow request, REST or MQTT alike, by AWS's documented rules:
        (status, document, broadcasts, note). The document is the answer - an
        error document {code, message, timestamp[, clientToken]} for a refusal;
        broadcasts are (topic suffix, document) for the shadow's MQTT topics;
        note says what happened, for the log. `token`: an MQTT get's or
        delete's clientToken, echoed. No policy check, no log, and no deliver()
        inside the lock."""
        key = (thing, name)
        now = int(time.time())

        def error(status, message, echo=None):
            doc = {"code": status, "message": message, "timestamp": now}
            if echo is not None:
                doc["clientToken"] = echo
            return status, doc, [], None

        tagged = lambda doc: {**doc, "clientToken": token} if token is not None else doc
        missing = "No shadow exists with name: '%s'" % (name or thing)
        if op == "get":
            with self.lock:
                current = self.shadows.get(key)
                doc = None if current is None else current.document(now)
            if doc is None:
                return error(404, missing, token)
            return 200, tagged(doc), [], "version %d" % doc["version"]
        if op == "delete":
            # Taken and recorded at once: an update in between cannot be lost.
            with self.lock:
                current = self.shadows.pop(key, None)
                if current is not None:
                    self.deleted[key] = current.version
            if current is None:
                return error(404, missing, token)
            doc = tagged({"version": current.version, "timestamp": now})
            return 200, doc, [("/delete/accepted", doc)], "deleted at version %d" % current.version

        # UpdateThingShadow. Python's json takes NaN and Infinity; JSON does not.
        def not_json(constant):
            raise ValueError("not JSON: " + constant)
        try:
            request = json.loads(body.decode("utf-8"), parse_constant=not_json)
        except UnicodeDecodeError:
            return error(415, "Unsupported documented encoding; supported encoding is UTF-8")
        except ValueError:
            return error(400, "Invalid JSON")
        token = request.get("clientToken") if isinstance(request, dict) else None
        problem = update_problem(request)
        if problem:
            return error(400, problem, token)
        with self.lock:
            current = self.shadows.get(key)
            # Only that version is updated - none at all when there is no shadow.
            conflict = "version" in request and (current is None or request["version"] != current.version)
            before = None if current is None else current.snapshot()
            version = (current.version if current else self.deleted.get(key, 0)) + 1
            merged = Shadow.merged(current, request["state"], now, version)
            too_big = merged.size() > SHADOW_STATE_MAX
            if not conflict and not too_big:
                self.shadows[key] = merged
        if conflict:
            return error(409, "Version conflict", token)
        if too_big:
            return error(413, "The payload exceeds the maximum size allowed", token)
        tagged = lambda doc: {**doc, "clientToken": token} if token is not None else doc
        accepted = tagged({"state": request["state"] or {}, "metadata": metadata_of(request["state"] or {}, now),
                           "version": version, "timestamp": now})
        broadcasts = [("/update/accepted", accepted),
                      ("/update/documents", tagged({"previous": before, "current": merged.snapshot(), "timestamp": now}))]
        delta = merged.delta()
        if delta:
            broadcasts.append(("/update/delta", tagged({"state": delta, "metadata": merged.delta_metadata(delta),
                                                        "version": version, "timestamp": now})))
        return 200, accepted, broadcasts, "version %d, %d bytes of state" % (version, merged.size())

    def shadow_broadcast(self, thing, name, broadcasts):
        """The shadow's MQTT messages, to every subscriber the policy lets receive them."""
        topic = "$aws/things/%s/shadow%s" % (thing, "" if name is None else "/name/" + name)
        for leaf, doc in broadcasts:
            self.deliver(topic + leaf, json.dumps(doc, separators=(",", ":")).encode(), None)

    def adopt(self, session):
        """AWS takes the newer of two connections with one client id, and drops the older."""
        with self.lock:
            old = self.sessions.get(session.client_id)
            if old is not None and old is not session:
                old.kicked = True
            self.sessions[session.client_id] = session

    def forget(self, session):
        with self.lock:
            if self.sessions.get(session.client_id) is session:
                del self.sessions[session.client_id]

    def deliver(self, topic, payload, sender):
        with self.lock:
            sessions = list(self.sessions.values())
        receivers = 0
        for s in sessions:
            if s.takes(topic, sender) and self.may(s.thing, "receive", topic):
                receivers += 1
                s.queue(topic, payload)
        return receivers


class Session:
    """One MQTT connection, after its certificate was found in the registry."""

    def __init__(self, broker, sock, thing, log):
        self.broker = broker
        self.sock = sock
        self.thing = thing
        self.log = log
        self.client_id = None
        self.v5 = True
        self.subs = {}                     # filter -> subscription options
        self.outbox = collections.deque()  # written only by this connection's own thread
        self.kicked = False
        self.max_packet = MAX_PACKET
        self.problem_info = 1
        self.aliases = {}
        self.keep_alive = 1200

    def run(self):
        try:
            self.serve()
        except PacketTooLarge as e:
            self.disconnect(0x95, "%s, over AWS's %d" % (e, MAX_PACKET))
        except (ValueError, IndexError) as e:
            self.log("a malformed packet (%s): reset" % e)
            reset(self.sock)
        except OSError as e:  # ConnectionError, ssl.SSLError, timeouts
            self.log("connection ended (%s: %s)" % (type(e).__name__, e))
        finally:
            self.broker.forget(self)
            close_quietly(self.sock)

    def send(self, first, body):
        self.sock.sendall(packet(first, body))

    def close(self):
        # A plain close: what was sent still arrives (a reset would drop it).
        close_quietly(self.sock)
        return False

    def disconnect(self, code, why):
        if self.v5:
            try:
                self.send(0xE0, bytes([code]))
            except OSError:
                pass
        self.log("%s -> DISCONNECT 0x%02X, then close" % (why, code))
        return self.close()

    def refuse(self, code, why, extra=b""):
        if self.v5:
            self.send(0x20, b"\x00" + bytes([code]) + properties(extra))
        else:
            self.send(0x20, b"\x00" + bytes([{0x87: 0x05, 0x85: 0x02}.get(code, 0x03)]))
        self.log("%s -> CONNACK 0x%02X, then close" % (why, code))
        return self.close()

    def reason(self, code, text):
        """A Reason String: only where the client asked for problem information (and then only on
        a refusal), unless --reason-strings sends one anyway."""
        if self.v5 and (self.broker.o.reason_strings or (self.problem_info and code >= 0x80)):
            return pstr(0x1F, text)
        return b""

    def serve(self):
        s = self.sock
        s.settimeout(20)
        first, body = read_packet(s)
        if first != 0x10:
            self.log("first packet 0x%02X is not a CONNECT: reset" % first)
            reset(s)
            return
        c = parse_connect(body)
        self.v5 = c["level"] == 5
        props = c["props"]
        cid = c["client_id"]
        self.client_id = cid
        self.log("CONNECT MQTT %s, client id %r, keep-alive %d s, %s, properties: %s%s%s" % (
            {5: "5", 4: "3.1.1"}.get(c["level"], str(c["level"])), cid, c["keep_alive"],
            "clean start" if c["flags"] & 0x02 else "no clean start", describe_props(props),
            ", user %r" % c["user"].decode("utf-8", "replace") if "user" in c else "",
            ", a password" if "password" in c else ""))
        if c["protocol"] != "MQTT" or c["level"] not in (4, 5):
            self.v5 = False
            self.send(0x20, b"\x00\x01")
            self.log("neither MQTT 3.1.1 nor 5: CONNACK 0x01, then close")
            return self.close()
        self.problem_info = prop(props, 0x17, 1)
        self.max_packet = prop(props, 0x27, MAX_PACKET)
        if self.v5 and self.max_packet < MIN_CLIENT_PACKET:
            # The quota page: a smaller one is not allowed to connect. (AWS names no reason code
            # for it; 0x80 is in its CONNACK list.)
            return self.refuse(0x80, "Maximum Packet Size %d, under AWS's minimum of %d"
                               % (self.max_packet, MIN_CLIENT_PACKET))
        if not cid:
            # AWS assigns an id - which is no thing's name, so iot:Connect is denied.
            assigned = "mock-" + os.urandom(8).hex()
            return self.refuse(0x87, "an empty client id: Assigned Client Identifier %r, not the thing's name"
                               % assigned, pstr(0x12, assigned) if self.v5 else b"")
        if len(cid.encode("utf-8")) > MAX_CLIENT_ID:
            return self.refuse(0x85, "a client id over %d bytes" % MAX_CLIENT_ID)
        if not self.broker.may(self.thing, "connect", cid):
            why = "iot:Connect on client/%s denied: the certificate's thing is %r" % (cid, self.thing)
            if self.broker.o.deny == "rst":
                self.log(why + " -> TCP reset (--deny rst)")
                reset(s)
                return
            return self.refuse(0x87, why)
        self.keep_alive = server_keep_alive(c["keep_alive"])
        self.broker.adopt(self)
        if self.v5:
            self.send(0x20, b"\x00\x00" + connack_properties(self.keep_alive))
        else:
            self.send(0x20, b"\x00\x00")
        self.log("CONNACK 0x00, Server Keep Alive %d s" % self.keep_alive)
        self.loop()

    def loop(self):
        s = self.sock
        s.settimeout(10)
        last = time.monotonic()
        while True:
            if self.kicked:
                return self.disconnect(0x8E, "another connection took the client id over")
            while self.outbox:
                s.sendall(self.outbox.popleft())
            if time.monotonic() - last > self.keep_alive * 1.5:
                return self.disconnect(0x8D, "nothing for 1.5 x the keep-alive")
            if not s.pending():
                ready, _, _ = select.select([s], [], [], 0.25)
                if not ready:
                    continue
            first, body = read_packet(s)
            last = time.monotonic()
            kind = first >> 4
            if kind == 12:
                self.send(0xD0, b"")
                self.log("PINGREQ -> PINGRESP")
            elif kind == 14:
                self.log("the client sent DISCONNECT %s" % (("0x%02X" % body[0]) if body else "0x00"))
                return self.close()
            elif kind == 8:
                if not self.subscribe(body):
                    return
            elif kind == 10:
                self.unsubscribe(body)
            elif kind == 3:
                if not self.publish(first, body):
                    return
            elif kind == 4:
                pass  # a PUBACK; this broker delivers at QoS 0 only, so there is nothing to settle
            else:
                return self.disconnect(0x82, "packet type %d is not handled here" % kind)

    def subscribe(self, body):
        pid, p = u16(body, 0)
        if self.v5:
            n, p = read_vbi(body, p)
            p += n
        entries = []
        while p < len(body):
            raw, p = mstr(body, p)
            entries.append((raw.decode("utf-8"), body[p]))
            p += 1
        what = "SUBSCRIBE id %d %s" % (pid, ", ".join("`%s` QoS %d" % (f, o & 3) for f, o in entries))
        if any(o & 3 == 2 for _, o in entries):
            self.log(what + ": QoS 2 asked -> no SUBACK at all (as AWS)")
            return True
        codes, granted = [], []
        for flt, opts in entries:
            if flt.startswith("$") and not flt.startswith(("$aws/", "$share/")):
                # reserved-topics.html: such an operation "can result in a terminated connection".
                self.log(what + ": `%s` is a reserved topic -> connection ended" % flt)
                return self.close()
            if not valid_filter(flt) or slashes(flt) > MAX_SLASHES:
                code = 0x8F
            elif flt not in self.subs and len(self.subs) + len(granted) >= MAX_SUBSCRIPTIONS:
                code = 0x97
            elif not self.broker.may(self.thing, "subscribe", flt):
                code = 0x87
            else:
                code = min(opts & 3, 1)
                granted.append((flt, opts))
            codes.append(code)
        retained = []
        for flt, opts in granted:
            handling = (opts >> 4) & 3
            new = flt not in self.subs
            self.subs[flt] = opts
            # AWS sends a retained message only to a filter that names its topic exactly.
            if handling == 2 or (handling == 1 and not new) or flt not in self.broker.retained:
                continue
            if self.broker.may(self.thing, "receive", flt):
                retained.append(flt)
        before = self.broker.o.retained_before_suback
        if before:
            self.send_retained(retained, "before")
        if self.v5:
            text = "; ".join("`%s`: 0x%02X" % (f, c) for (f, _), c in zip(entries, codes))
            self.send(0x90, pid.to_bytes(2, "big") + properties(self.reason(max(codes), text)) + bytes(codes))
        else:
            codes = [c if c < 0x80 else 0x80 for c in codes]  # MQTT 3.1.1 has one failure code
            self.send(0x90, pid.to_bytes(2, "big") + bytes(codes))
        self.log("%s -> SUBACK %s" % (what, " ".join("0x%02X" % c for c in codes)))
        if not before:
            self.send_retained(retained, "after")
        return True

    def send_retained(self, topics, when):
        for topic in topics:
            data = self.publish_packet(topic, self.broker.retained[topic], retain=True)
            if data:
                self.sock.sendall(data)
                self.log("retained `%s` sent %s the SUBACK" % (topic, when))

    def unsubscribe(self, body):
        pid, p = u16(body, 0)
        if self.v5:
            n, p = read_vbi(body, p)
            p += n
        codes = []
        while p < len(body):
            raw, p = mstr(body, p)
            codes.append(0x00 if self.subs.pop(raw.decode("utf-8"), None) is not None else 0x11)
        self.send(0xB0, pid.to_bytes(2, "big") + (b"\x00" + bytes(codes) if self.v5 else b""))
        self.log("UNSUBSCRIBE id %d -> UNSUBACK %s" % (pid, " ".join("0x%02X" % c for c in codes)))

    def publish(self, first, body):
        qos, retain = (first >> 1) & 3, first & 1
        name, p = mstr(body, 0)
        pid = None
        if qos:
            pid, p = u16(body, p)
        props = []
        if self.v5:
            n, p = read_vbi(body, p)
            props, p = parse_props(body[p:p + n]), p + n
        payload = body[p:]
        topic = name.decode("utf-8")
        alias = prop(props, 0x23)
        if alias is not None:
            if not 1 <= alias <= 8:
                return self.disconnect(0x94, "Topic Alias %d, over the maximum of 8" % alias)
            if topic:
                self.aliases[alias] = topic
            elif alias in self.aliases:
                topic = self.aliases[alias]
            else:
                return self.disconnect(0x82, "Topic Alias %d was never set" % alias)
        what = "PUBLISH QoS %d%s%s `%s` %r" % (qos, " id 0x%04X" % pid if qos else "", " retained" if retain else "",
                                             topic, payload[:48])
        if qos == 2:
            return self.disconnect(0x9B, what + ": AWS takes QoS 0 and 1 only")
        if topic.startswith("$") and not topic.startswith("$aws/"):
            self.log(what + ": a reserved topic -> connection ended")
            return self.close()
        if not topic or "+" in topic or "#" in topic or slashes(topic) > MAX_SLASHES:
            return self.refuse_publish(qos, pid, 0x90, what + ": the topic name is not valid")
        if not self.broker.may(self.thing, "publish", topic):
            return self.refuse_publish(qos, pid, 0x87, what + ": iot:Publish on topic/%s denied" % topic)
        if retain and not self.broker.may(self.thing, "retainpublish", topic):
            return self.refuse_publish(qos, pid, 0x87, what + ": iot:RetainPublish on topic/%s denied" % topic)
        if retain:
            # Kept for later subscribers, as AWS does - an empty payload deletes.
            with self.broker.lock:
                if payload:
                    self.broker.retained[topic] = payload
                else:
                    self.broker.retained.pop(topic, None)
            self.log("retained `%s` %s" % (topic, "stored (%d bytes)" % len(payload) if payload else "deleted"))
        receivers = self.broker.deliver(topic, payload, self)
        code = 0x00 if receivers else 0x10  # 0x10: no matching subscribers
        if qos == 1:
            self.puback(pid, code, "accepted" if receivers else "no matching subscribers")
        self.log("%s: allowed, %d receiver(s)%s" % (what, receivers, " -> PUBACK 0x%02X" % code if qos else ""))
        request = re.fullmatch(r"\$aws/things/([^/]+)/shadow(?:/name/([^/]+))?/(get|update|delete)", topic)
        if request:
            self.shadow_request(request.group(1), request.group(2), request.group(3), payload)
        return True

    def shadow_request(self, thing, name, op, payload):
        """The Device Shadow over MQTT: the answer on <op>/accepted or
        <op>/rejected, to every subscriber - the clientToken echoed - and an
        update's documents and delta."""
        prefix = "$aws/things/%s/shadow%s" % (thing, "" if name is None else "/name/" + name)
        what = "shadow over MQTT %s %s%s" % (op, thing, "" if name is None else "/" + name)
        token = None
        if op != "update" and payload:
            # get and delete: the content is ignored but its clientToken.
            try:
                doc = json.loads(payload.decode("utf-8"))
                token = doc.get("clientToken") if isinstance(doc, dict) else None
            except (UnicodeDecodeError, ValueError):
                token = None
            if token is not None and (not isinstance(token, str) or len(token.encode()) > 64):
                return self.shadow_reply(prefix, op, what, 400, {"code": 400, "message": "Invalid clientToken",
                                                                 "timestamp": int(time.time())})
        status, doc, broadcasts, note = self.broker.shadow_op(thing, name, op, payload, token)
        if status != 200:
            return self.shadow_reply(prefix, op, what, status, doc)
        self.log("%s: %s -> %s/accepted" % (what, note, op))
        if op == "get":
            self.broker.deliver(prefix + "/get/accepted", json.dumps(doc, separators=(",", ":")).encode(), None)
        self.broker.shadow_broadcast(thing, name, broadcasts)

    def shadow_reply(self, prefix, op, what, status, doc):
        self.log("%s: %d %s -> %s/rejected" % (what, status, doc.get("message"), op))
        self.broker.deliver(prefix + "/%s/rejected" % op, json.dumps(doc, separators=(",", ":")).encode(), None)

    def refuse_publish(self, qos, pid, code, why):
        if qos == 1 and self.v5:
            self.puback(pid, code, why)
            self.log("%s -> PUBACK 0x%02X" % (why, code))
            return True
        # QoS 0 has no answer but the connection itself.
        return self.disconnect(code, why)

    def puback(self, pid, code, text):
        body = pid.to_bytes(2, "big")
        if self.v5:
            body += bytes([code])
            extra = self.reason(code, text)
            if extra:
                body += properties(extra)
        self.send(0x40, body)

    def takes(self, topic, sender):
        # Bit 2 of the options is No Local: not one's own publications.
        return any(filter_matches(f, topic) and not (o & 0x04 and self is sender) for f, o in self.subs.items())

    def publish_packet(self, topic, payload, retain=False):
        data = packet(0x30 | int(retain), field(topic) + (b"\x00" if self.v5 else b"") + payload)
        if len(data) > self.max_packet:
            self.log("`%s` (%d bytes) is over the client's Maximum Packet Size: dropped" % (topic, len(data)))
            return None
        return data

    def queue(self, topic, payload):
        data = self.publish_packet(topic, payload)
        if data:
            self.outbox.append(data)
            self.log("delivered `%s` to %r at QoS 0" % (topic, self.client_id))


class HttpsPublish:
    """AWS IoT's HTTPS publish, after the certificate was found in the registry:
    one request, then the connection ends. What it answers is AWS's documentation
    (and published captures), not probed traffic - see the docstring at the top."""

    def __init__(self, broker, sock, thing, log):
        self.broker, self.sock, self.thing, self.log = broker, sock, thing, log

    def run(self):
        self.sock.settimeout(20)
        try:
            self.serve()
        except (OSError, ssl.SSLError, ValueError) as e:
            self.log("HTTPS publish ended: %s" % e)
        close_quietly(self.sock)

    def answer(self, status, reason, message):
        import uuid
        body = ('{"message":%s,"traceId":"%s"}' % (
            '"%s"' % message if message is not None else "null", uuid.uuid4())).encode()
        head = ("HTTP/1.1 %d %s\r\ncontent-type: application/json\r\ncontent-length: %d\r\n"
                "connection: close\r\n\r\n" % (status, reason, len(body))).encode()
        self.sock.sendall(head + body)
        return status

    def serve(self):
        data = b""
        while b"\r\n\r\n" not in data:
            chunk = self.sock.recv(4096)
            if not chunk:
                self.log("HTTPS publish: the client closed before a whole head")
                return
            data += chunk
            if len(data) > 16 * 1024:
                self.log("HTTPS publish: a head over 16 KB")
                return self.answer(400, "Bad Request", "Bad Request")
        head, _, body = data.partition(b"\r\n\r\n")
        lines = head.decode("latin-1").split("\r\n")
        method, target, _ = (lines[0].split(" ") + ["", "", ""])[:3]
        headers = {}
        for line in lines[1:]:
            name, _, value = line.partition(":")
            headers[name.strip().lower()] = value.strip()
        length = int(headers.get("content-length", "0") or 0)
        if length > HTTP_PUBLISH_MAX:
            self.log("HTTPS %s: %d bytes, over %d" % (method, length, HTTP_PUBLISH_MAX))
            return self.answer(413, "Payload Too Large", "Payload Too Large")
        while len(body) < length:
            chunk = self.sock.recv(4096)
            if not chunk:
                break
            body += chunk
        body = body[:length]
        self.log("HTTPS %s %s, Host %r, %d byte(s)" % (method, target, headers.get("host"), len(body)))
        path, _, query = target.partition("?")
        if path.startswith("/things/"):
            return self.shadow(method, path, query, body)
        if method != "POST" or not path.startswith("/topics/"):
            if method != "POST":
                return self.answer(405, "Method Not Allowed", "Method Not Allowed")
            return self.answer(404, "Not Found", "Not Found")
        import urllib.parse
        try:
            topic = urllib.parse.unquote_to_bytes(path[len("/topics/"):]).decode("utf-8")
        except UnicodeDecodeError:
            return self.answer(400, "Bad Request", "Bad Request")
        params = urllib.parse.parse_qs(query, keep_blank_values=True)
        qos = params.get("qos", ["0"])[0]
        what = "HTTPS POST `%s` qos %s %r" % (topic, qos, body[:48])
        if qos not in ("0", "1"):
            self.log(what + ": a QoS other than 0 or 1 -> 400")
            return self.answer(400, "Bad Request", "Bad Request")
        counted = topic.split("/", 3)[3] if topic.startswith("$aws/rules/") and topic.count("/") >= 3 else topic
        if (not topic or "+" in topic or "#" in topic or slashes(counted) > MAX_SLASHES
                or (topic.startswith("$") and not topic.startswith("$aws/"))):
            self.log(what + ": not a topic AWS takes -> 400")
            return self.answer(400, "Bad Request", "Bad Request")
        if not self.broker.may(self.thing, "publish", topic):
            self.log(what + ": iot:Publish on topic/%s denied -> 403" % topic)
            return self.answer(403, "Forbidden", "Forbidden")
        retain = params.get("retain", ["false"])[0] == "true"
        if retain and topic.startswith("$"):
            self.log(what + ": retained, on a reserved topic -> 400")
            return self.answer(400, "Bad Request", "Bad Request")
        if retain and not self.broker.may(self.thing, "retainpublish", topic):
            self.log(what + ": iot:RetainPublish on topic/%s denied -> 403" % topic)
            return self.answer(403, "Forbidden", "Forbidden")
        props = self.properties(params, headers, body)
        if props is None:
            self.log(what + ": MQTT 5 properties AWS would refuse -> 400")
            return self.answer(400, "Bad Request", "Bad Request")
        if props:
            self.log("MQTT 5 properties: " + ", ".join("%s=%r" % kv for kv in props))
        if retain:
            with self.broker.lock:
                if body:
                    self.broker.retained[topic] = body
                else:
                    self.broker.retained.pop(topic, None)
            self.log("retained `%s` %s" % (topic, "stored (%d bytes)" % len(body) if body else "deleted"))
        receivers = self.broker.deliver(topic, body, None)
        # HTTP has no "no matching subscribers": a 200 either way.
        self.log("%s: allowed, %d receiver(s) -> 200" % (what, receivers))
        return self.answer(200, "OK", "OK")

    def properties(self, params, headers, body):
        """MQTT 5's properties as AWS's HTTP publish takes them - the query's
        contentType, messageExpiry, responseTopic and three x-amz-mqtt5-*
        headers - checked by AWS's documented limits (contentType 256 bytes,
        user properties and correlation data 8 KB each, no wildcard in the
        response topic, the indicator's two names). None for what it would
        refuse - its status for that is not documented: 400 here, an
        assumption, as is reading an empty responseTopic as no topic."""
        import base64
        import json
        out = []
        kind = params.get("contentType", [None])[0]
        if kind is not None:
            if len(kind.encode()) > 256:
                return None
            out.append(("contentType", kind))
        expiry = params.get("messageExpiry", [None])[0]
        if expiry is not None:
            if not expiry.isdigit():
                return None
            out.append(("messageExpiry", int(expiry)))
        reply = params.get("responseTopic", [None])[0]
        if reply is not None:
            if not reply or "+" in reply or "#" in reply:
                return None
            out.append(("responseTopic", reply))
        users = headers.get("x-amz-mqtt5-user-properties")
        if users is not None:
            try:
                raw = base64.b64decode(users, validate=True)
                pairs = json.loads(raw.decode("utf-8"))
            except (ValueError, UnicodeDecodeError):
                return None
            # An array of objects: AWS's example gives one pair each.
            if len(raw) > 8192 or not isinstance(pairs, list) or not all(isinstance(p, dict) for p in pairs):
                return None
            out.append(("userProperties", [kv for p in pairs for kv in p.items()]))
        correlation = headers.get("x-amz-mqtt5-correlation-data")
        if correlation is not None:
            try:
                data = base64.b64decode(correlation, validate=True)
            except ValueError:
                return None
            if len(data) > 8192:
                return None
            out.append(("correlationData", data.hex()))
        indicator = headers.get("x-amz-mqtt5-payload-format-indicator")
        if indicator is not None:
            if indicator not in ("UTF8_DATA", "UNSPECIFIED_BYTES"):
                return None
            if indicator == "UTF8_DATA":
                try:
                    body.decode("utf-8")
                except UnicodeDecodeError:
                    return None
            out.append(("payloadFormat", indicator))
        return out

    def shadow_answer(self, status, reason, doc):
        body = json.dumps(doc, separators=(",", ":")).encode()
        head = ("HTTP/1.1 %d %s\r\ncontent-type: application/json\r\ncontent-length: %d\r\n"
                "connection: close\r\n\r\n" % (status, reason, len(body))).encode()
        self.sock.sendall(head + body)
        return status

    def shadow_error(self, status, reason, message, token=None):
        doc = {"code": status, "message": message, "timestamp": int(time.time())}
        if token is not None:
            doc["clientToken"] = token
        self.log("shadow: %d %s" % (status, message))
        return self.shadow_answer(status, reason, doc)

    def shadow(self, method, path, query, body):
        """AWS IoT's Device Shadow REST API: GetThingShadow, UpdateThingShadow,
        DeleteThingShadow - by its documented rules and texts."""
        import urllib.parse
        m = re.fullmatch(r"/things/([^/]+)/shadow", path)
        if m is None:
            return self.shadow_error(404, "Not Found", "Not Found")
        thing = urllib.parse.unquote(m.group(1))
        params = urllib.parse.parse_qs(query, keep_blank_values=True)
        name = params.get("name", [None])[0]
        action = {"GET": "GetThingShadow", "POST": "UpdateThingShadow", "DELETE": "DeleteThingShadow"}.get(method)
        if action is None:
            return self.shadow_error(405, "Method Not Allowed", "The specified combination of HTTP verb and URI is not supported.")
        # Names AWS takes; its status for one it does not is undocumented (400 here).
        if (not re.fullmatch(r"[a-zA-Z0-9:_-]{1,128}", thing)
                or (name is not None and not re.fullmatch(r"[$a-zA-Z0-9:_-]{1,64}", name))):
            return self.shadow_error(400, "Bad Request", "Invalid name")
        resource = "thing/%s%s" % (thing, "" if name is None else "/" + name)
        what = "shadow %s %s" % (method, resource)
        if not self.broker.may(self.thing, "shadow", resource):
            self.log(what + ": iot:%s on %s denied -> 403" % (action, resource))
            return self.shadow_error(403, "Forbidden", "Forbidden")
        op = {"GET": "get", "POST": "update", "DELETE": "delete"}[method]
        status, doc, broadcasts, note = self.broker.shadow_op(thing, name, op, body)
        if status != 200:
            return self.shadow_error(status, REASONS[status], doc["message"], doc.get("clientToken"))
        self.log("%s: %s -> 200" % (what, note))
        self.broker.shadow_broadcast(thing, name, broadcasts)
        return self.shadow_answer(200, "OK", doc)


# The REST answers' reason phrases.
REASONS = {400: "Bad Request", 404: "Not Found", 409: "Conflict", 413: "Payload Too Large",
           415: "Unsupported Media Type"}

# The Device Shadow, as AWS documents it (developerguide/device-shadow-document.html,
# device-shadow-data-flow.html, device-shadow-error-messages.html; general/latest/gr/iot-core.html).
SHADOW_STATE_MAX = 8192      # "8KB": 8192 assumed; compact JSON, metadata not counted
SHADOW_DEPTH_MAX = 6         # the error text says 6, the quota page 8: the stricter


def depth(value):
    if isinstance(value, dict):
        return 1 + max((depth(v) for v in value.values()), default=0)
    if isinstance(value, list):
        return 1 + max((depth(v) for v in value), default=0)
    return 0


def null_in_array(value):
    """An array holding null, anywhere: "Arrays can't have null values ... will be rejected"."""
    if isinstance(value, dict):
        return any(null_in_array(v) for v in value.values())
    if isinstance(value, list):
        return any(v is None or null_in_array(v) for v in value)
    return False


def same(a, b):
    """JSON equality: `true` is no `1` (Python's == says it is), an array or an
    object equals only one of its own kind, element by element."""
    if isinstance(a, bool) or isinstance(b, bool):
        return isinstance(a, bool) and isinstance(b, bool) and a == b
    if isinstance(a, list) or isinstance(b, list):
        return isinstance(a, list) and isinstance(b, list) and len(a) == len(b) and all(map(same, a, b))
    if isinstance(a, dict) or isinstance(b, dict):
        return (isinstance(a, dict) and isinstance(b, dict) and a.keys() == b.keys()
                and all(same(a[k], b[k]) for k in a))
    return a == b


def update_problem(request):
    """What AWS answers 400 for in an update's request document, its text - or None."""
    if not isinstance(request, dict) or "state" not in request:
        return "Missing required node: state"
    state = request["state"]
    if state is not None and not isinstance(state, dict):
        return "State node must be an object"
    if isinstance(state, dict):
        if any(k not in ("desired", "reported") for k in state):
            return "State contains an invalid node"
        for section in ("desired", "reported"):
            value = state.get(section)
            if section in state and value is not None and not isinstance(value, dict):
                return "%s node must be an object" % section.capitalize()
            if isinstance(value, dict) and depth(value) > SHADOW_DEPTH_MAX:
                return "JSON contains too many levels of nesting; maximum is %d" % SHADOW_DEPTH_MAX
            if null_in_array(value):
                return "Arrays can't have null values"   # AWS rejects it; its text is not documented
    if "version" in request and (not isinstance(request["version"], int) or isinstance(request["version"], bool)):
        return "Invalid version"
    token = request.get("clientToken")
    if token is not None and (not isinstance(token, str) or len(token.encode()) > 64):
        return "Invalid clientToken"
    return None


def merge(into, change):
    """An update's section merged into what is kept: only the keys it names
    change, a key set to null goes, objects merge, anything else replaces."""
    out = dict(into or {})
    for k, v in change.items():
        if v is None:
            out.pop(k, None)
        elif isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = merge(out[k], v)
        elif isinstance(v, dict):
            out[k] = merge({}, v)
        else:
            out[k] = v
    return out


def metadata_of(value, now):
    """A timestamp for every leaf - per element in an array, as AWS keeps it."""
    if isinstance(value, dict):
        return {k: metadata_of(v, now) for k, v in value.items() if v is not None}
    if isinstance(value, list):
        return [{"timestamp": now} for _ in value]
    return {"timestamp": now}


def merge_metadata(kept, change, now):
    """The metadata beside `merge`: the keys an update names get its time, the
    ones it sets to null go, the rest keep theirs."""
    out = dict(kept or {})
    for k, v in change.items():
        if v is None:
            out.pop(k, None)
        elif isinstance(v, dict):
            old = out.get(k)
            out[k] = merge_metadata(old if isinstance(old, dict) and not is_leaf(old) else {}, v, now)
        else:
            out[k] = metadata_of(v, now)
    return out


def is_leaf(meta):
    """A leaf's metadata, {"timestamp": <int>} - not an object's that has a
    key named "timestamp", whose metadata is itself a dict."""
    return isinstance(meta, dict) and isinstance(meta.get("timestamp"), int) and not isinstance(meta.get("timestamp"), bool)


class Shadow:
    """One shadow: its desired and reported state, and its version."""

    def __init__(self, state, version, metadata=None):
        self.state = {k: v for k, v in state.items() if v is not None}
        self.version = version
        self.metadata = metadata if metadata is not None else metadata_of(self.state, int(time.time()))

    @staticmethod
    def merged(current, change, now, version):
        state = dict(current.state) if current else {}
        metadata = dict(current.metadata) if current else {}
        if change is None:
            # `"state": null` clears both, the shadow stays.
            return Shadow({}, version, {})
        for section, value in change.items():
            if value is None:
                state.pop(section, None)
                metadata.pop(section, None)
                continue
            state[section] = merge(state.get(section), value)
            metadata[section] = merge_metadata(metadata.get(section), value, now)
        return Shadow(state, version, metadata)

    def size(self):
        # UTF-8 bytes, not Python's \u-escaped text.
        return len(json.dumps(self.state, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))

    def delta(self):
        def walk(desired, reported):
            out = {}
            for k, v in desired.items():
                r = reported.get(k) if isinstance(reported, dict) else None
                if isinstance(v, dict) and isinstance(r, dict):
                    d = walk(v, r)
                    if d:
                        out[k] = d
                elif not isinstance(reported, dict) or k not in reported or not same(r, v):
                    out[k] = v
            return out
        return walk(self.state.get("desired", {}), self.state.get("reported", {}))

    def delta_metadata(self, delta):
        """The delta's metadata: "equal to the metadata in the desired field"."""
        def pick(d, meta):
            out = {}
            for k, v in d.items():
                m = meta.get(k) if isinstance(meta, dict) else None
                out[k] = pick(v, m) if isinstance(v, dict) and isinstance(m, dict) and not is_leaf(m) else m
            return out
        return pick(delta, self.metadata.get("desired", {}))

    def snapshot(self):
        """previous / current in .../update/documents: no delta, no timestamp of their own."""
        return {"state": dict(self.state), "metadata": self.metadata, "version": self.version}

    def document(self, now):
        state = dict(self.state)
        metadata = dict(self.metadata)
        delta = self.delta()
        if delta:
            state["delta"] = delta
            metadata["delta"] = self.delta_metadata(delta)
        return {"state": state, "metadata": metadata, "version": self.version, "timestamp": now}


class Listener:
    """A subscriber on the MQTT side, in the broker's sessions: what an HTTPS
    publish reaches. It records instead of sending."""

    def __init__(self, broker, flt):
        self.broker, self.flt = broker, flt
        self.thing, self.client_id = THING, "listener"
        self.got = []

    def takes(self, topic, sender):
        return filter_matches(self.flt, topic)

    def queue(self, topic, payload):
        self.got.append((topic, payload))
        self.broker.log("delivered `%s` to %r: %r" % (topic, self.client_id, payload[:48]))


# --- The cases ---------------------------------------------------------------------

GOOD_FILTERS = ("rustonchip-test/cmd", "rustonchip-test/+/set")
GOOD_TOPIC = "rustonchip-test/test"
SEED_RETAINED = {"rustonchip-test/cmd": b"mock: a retained command"}
REFUSED_CERT = "the broker closed the connection after the handshake: it refused the client certificate"
GOOD_RESULT = ("all 2 subscriptions", "a publication")
GOOD_INFO = ("subscribed to", "published a test message")


class Case:
    def __init__(self, cid, title, port="443", alpn=True, cert="device", client_id=THING, ca=("root1", "root3"),
                 filters=GOOD_FILTERS, topic=GOOD_TOPIC, keep_alive=60, broker=None, real_443=False, wire=False,
                 result="ok", result_has=GOOD_RESULT, info_has=GOOD_INFO, info_lacks=(), log_has=(),
                 https=False, qos=1, payload=b'{"t":21.5}', listener=GOOD_TOPIC, retain=False, query="",
                 headers=(), shadow=None, direct=None, shadow_filters=(), shadow_max=0, shadow_work=0):
        self.cid, self.title, self.port, self.alpn, self.cert = cid, title, port, alpn, cert
        self.client_id, self.ca, self.filters, self.topic, self.keep_alive = client_id, ca, filters, topic, keep_alive
        self.broker, self.real_443, self.wire = broker or {}, real_443, wire
        self.result, self.result_has, self.info_has = result, result_has, info_has
        self.info_lacks, self.log_has = info_lacks, log_has
        # HTTPS publish: the helper's `http` mode, a POST of `payload` on `topic`,
        # an MQTT subscriber to `listener` (None: nobody subscribed).
        self.https, self.qos, self.payload, self.listener = https, qos, payload, listener
        # ... retained, more of the query (MQTT 5's), and headers as `Name: value`.
        self.retain, self.query, self.headers = retain, query, headers
        # The Device Shadow instead: {"method": "GET"|"POST", "name": None|str},
        # `payload` the update. `direct`: a check the mock runs itself, as
        # direct(broker, pki) -> (problems, notes) - for what the helper cannot send.
        self.shadow, self.direct = shadow, direct
        # The MQTT mode's Device Shadow probe: the board's SHADOW_SUBSCRIBE
        # (none = no shadow), its SHADOW_MAX and WORK.
        self.shadow_filters, self.shadow_max, self.shadow_work = shadow_filters, shadow_max, shadow_work

    def picked_by(self, arg):
        return self.cid == arg or (self.cid.startswith(arg) and self.cid[len(arg):].isalpha())


def fails(cid, title, has, info=(), **kw):
    return Case(cid, title, result="fail", result_has=has, info_has=info, **kw)


SENT_CERT = ("asked for the client certificate: sent",)
# A user property the board's JSON escapes: a quote in the name, a backslash in the value.
UP_KEY, UP_VALUE = 'a"b', "c\\d"


def publish(cid, title, has=("AWS IoT took the message on `rustonchip-test/test`",), info=SENT_CERT, **kw):
    kw.setdefault("port", "8443")
    return Case(cid, title, https=True, result_has=has, info_has=info, **kw)


def publish_fails(cid, title, has, info=(), **kw):
    kw.setdefault("port", "8443")
    return Case(cid, title, https=True, result="fail", result_has=has, info_has=info, **kw)


def answers_of(root):
    """A shadow's 7 answer topics, in the board's order (SHADOW_SUBSCRIBE)."""
    return tuple(root + "/" + k for k in ("get/accepted", "get/rejected", "update/accepted", "update/rejected",
                                          "update/delta", "delete/accepted", "delete/rejected"))


SHADOW_THING = "thing/" + THING
SHADOW_SEED = {(THING, None): {"desired": {"led": True, "color": "RED"}, "reported": {"led": False, "color": "RED"}}}
SHADOW_TOPIC = "$aws/things/%s/shadow" % THING


def shadow(cid, title, has, method="GET", name=None, info=SENT_CERT, result="ok", allow=True, **kw):
    """The Device Shadow from the helper: a GET as shadow_get() sends it, or a
    POST of `payload` as shadow_update() does; iot:*ThingShadow allowed
    unless `allow` is False."""
    kw.setdefault("port", "8443")
    kw.setdefault("listener", None)
    kw.setdefault("payload", b"")
    broker = dict(kw.pop("broker", {}))
    broker.setdefault("allow_shadow", allow)
    return Case(cid, title, https=True, result=result, result_has=has, info_has=info,
                shadow={"method": method, "name": name}, broker=broker, **kw)


def shadow_asker(broker, pki, notes):
    """One request at a time to the "8443" side with the board's certificate,
    as the board's shadow calls send them: (status, the answer as JSON)."""
    def ask(method, target, body=b""):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_3
        ctx.load_verify_locations(str(pki.path("root1.pem")))
        ctx.load_cert_chain(str(pki.path("device.pem")), str(pki.path("device.key")))
        raw = socket.create_connection(("127.0.0.1", broker.ports["8443"]), timeout=10)
        with ctx.wrap_socket(raw, server_hostname=ENDPOINT) as s:
            length = "Content-Length: %d\r\n" % len(body) if method == "POST" else ""
            s.sendall(("%s %s HTTP/1.1\r\nHost: %s:8443\r\nUser-Agent: RustOnChip\r\nConnection: close\r\n%s\r\n"
                       % (method, target, ENDPOINT, length)).encode() + body)
            data = b""
            while True:
                chunk = s.recv(4096)
                if not chunk:
                    break
                data += chunk
        head, _, doc = data.partition(b"\r\n\r\n")
        status = int(head.split(b" ", 2)[1])
        notes.append("%s %s %s -> %d %s" % (method, target, body.decode("utf-8", "replace"), status,
                                             doc.decode("utf-8", "replace")))
        return status, json.loads(doc) if doc else None
    return ask


def shadow_update_check(broker, pki):
    """What an update does to the kept shadow, read back with GET: only the
    keys it names change, null removes one (and its metadata), the matching
    version is taken, `true` is no `1` for the delta, and what is no JSON for
    AWS - NaN, an array holding null - is refused."""
    problems, notes = [], []
    ask = shadow_asker(broker, pki, notes)
    target = "/things/%s/shadow" % THING
    status, doc = ask("POST", target, b'{"state":{"reported":{"color":"GREEN"}},"version":1}')
    if status != 200 or (doc or {}).get("version") != 2:
        problems.append("an update naming the shadow's own version: %d %r, not 200 with version 2" % (status, doc))
    status, doc = ask("GET", target)
    reported = (doc or {}).get("state", {}).get("reported")
    if reported != {"led": False, "color": "GREEN"}:
        problems.append("after reporting color alone, reported is %r - not merged into {led: false, color: GREEN}"
                        % reported)
    if (doc or {}).get("metadata", {}).get("delta") != {"led": {"timestamp": doc["metadata"]["desired"]["led"]["timestamp"]},
                                                         "color": {"timestamp": doc["metadata"]["desired"]["color"]["timestamp"]}}:
        problems.append("metadata.delta is not desired's metadata: %r" % (doc or {}).get("metadata"))
    ask("POST", target, b'{"state":{"reported":{"led":null}}}')
    status, doc = ask("GET", target)
    reported = (doc or {}).get("state", {}).get("reported")
    meta = (doc or {}).get("metadata", {}).get("reported", {})
    if reported != {"color": "GREEN"} or "led" in meta:
        problems.append("a key set to null stays: reported %r, its metadata %r" % (reported, meta))
    # desired led true, reported led 1: different JSON values, so a delta.
    ask("POST", target, b'{"state":{"reported":{"led":1,"color":"RED"}}}')
    status, doc = ask("GET", target)
    if (doc or {}).get("state", {}).get("delta") != {"led": True}:
        problems.append("reported 1 against desired true gives delta %r, not {led: true}"
                        % (doc or {}).get("state", {}).get("delta"))
    for bad, text in ((b'{"state":{"reported":{"t":NaN}}}', "Invalid JSON"),
                      (b'{"state":{"desired":{"colors":[null,"RED"]}}}', "Arrays can't have null values")):
        status, doc = ask("POST", target, bad)
        if status != 400 or (doc or {}).get("message") != text:
            problems.append("%s: %d %r, not 400 %r" % (bad.decode(), status, doc, text))
    return problems, notes


class MqttProbe:
    """A minimal MQTT 5 client with the board's certificate, on the "8883"
    side - for the shadow requests the helper does not send."""

    def __init__(self, broker, pki, max_packet=None):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_3
        ctx.load_verify_locations(str(pki.path("root1.pem")))
        ctx.load_cert_chain(str(pki.path("device.pem")), str(pki.path("device.key")))
        raw = socket.create_connection(("127.0.0.1", broker.ports["8883"]), timeout=10)
        self.s = ctx.wrap_socket(raw, server_hostname=ENDPOINT)
        props = properties(p32(0x27, max_packet)) if max_packet else properties()
        self.s.sendall(packet(0x10, b"\x00\x04MQTT\x05\x02\x00\x3c" + props + field(THING)))
        first, body = read_packet(self.s)
        if first != 0x20 or body[1] != 0:
            raise ConnectionError("CONNACK 0x%02X" % body[1])
        self.pid = 0

    def subscribe(self, flt):
        self.pid += 1
        self.s.sendall(packet(0x82, self.pid.to_bytes(2, "big") + properties() + field(flt) + b"\x00"))
        while True:
            first, body = read_packet(self.s)
            if first == 0x90:
                n, p = read_vbi(body, 2)
                return body[p + n]

    def publish(self, topic, payload):
        self.s.sendall(packet(0x30, field(topic) + properties() + payload))

    def messages(self, want, seconds=3):
        """Publications until `want` of them came, or `seconds` passed: [(topic, doc)]."""
        got = []
        deadline = time.monotonic() + seconds
        while len(got) < want and time.monotonic() < deadline:
            self.s.settimeout(max(0.05, deadline - time.monotonic()))
            try:
                first, body = read_packet(self.s)
            except (socket.timeout, ssl.SSLError, OSError):
                break
            if first & 0xF0 != 0x30:
                continue
            name, p = mstr(body, 0)
            if (first >> 1) & 3:
                p += 2
            n, p = read_vbi(body, p)
            payload = body[p + n:]
            try:
                doc = json.loads(payload)
            except ValueError:
                doc = payload
            got.append((name.decode("utf-8"), doc))
        return got

    def close(self):
        close_quietly(self.s)


SHADOW_ANSWERS = list(answers_of(SHADOW_TOPIC))


def mqtt_shadow_check(broker, pki):
    """The Device Shadow over MQTT as the board uses it: its 7 answer topics
    subscribed, an update answered on update/accepted and the delta it leaves
    - both carrying the board's token - another version refused with 409, a
    document AWS cannot read refused with 400 and no token, a delete, then a
    get answered 404, and a token over 64 bytes refused."""
    problems, notes = [], []
    c = MqttProbe(broker, pki)
    try:
        for t in SHADOW_ANSWERS:
            code = c.subscribe(t)
            if code >= 0x80:
                problems.append("SUBACK 0x%02X for %s" % (code, t))

        def ask(op, payload, want):
            c.publish(SHADOW_TOPIC + "/" + op, payload)
            got = c.messages(want)
            notes.append("%s %s -> %s" % (op, payload.decode("utf-8", "replace"), got))
            return dict(got)

        got = ask("update", b'{"state":{"reported":{"color":"GREEN"}},"clientToken":"b-1"}', 2)
        accepted, delta = got.get(SHADOW_TOPIC + "/update/accepted"), got.get(SHADOW_TOPIC + "/update/delta")
        if not accepted or accepted.get("clientToken") != "b-1" or accepted.get("version") != 2:
            problems.append("update: no update/accepted with token b-1 and version 2: %r" % accepted)
        if not delta or delta.get("clientToken") != "b-1" or delta.get("state") != {"led": True, "color": "RED"}:
            problems.append("update: no delta {led: true, color: RED} carrying the board's token: %r" % delta)
        got = ask("update", b'{"state":{"reported":{"led":true}},"version":7,"clientToken":"b-2"}', 1)
        rejected = got.get(SHADOW_TOPIC + "/update/rejected")
        if not rejected or rejected.get("code") != 409 or rejected.get("clientToken") != "b-2":
            problems.append("another version: no update/rejected 409 with token b-2: %r" % rejected)
        got = ask("update", b'{"state":', 1)
        rejected = got.get(SHADOW_TOPIC + "/update/rejected")
        if not rejected or rejected.get("code") != 400 or "clientToken" in rejected:
            problems.append("a document AWS cannot read: no update/rejected 400 without a token: %r" % rejected)
        got = ask("delete", b'{"clientToken":"b-3"}', 1)
        deleted = got.get(SHADOW_TOPIC + "/delete/accepted")
        if not deleted or deleted.get("clientToken") != "b-3" or deleted.get("version") != 2:
            problems.append("delete: no delete/accepted version 2 with token b-3: %r" % deleted)
        got = ask("get", b'{"clientToken":"b-4"}', 1)
        missing = got.get(SHADOW_TOPIC + "/get/rejected")
        if not missing or missing.get("code") != 404 or missing.get("clientToken") != "b-4":
            problems.append("get after delete: no get/rejected 404 with token b-4: %r" % missing)
        got = ask("get", ('{"clientToken":"%s"}' % ("k" * 65)).encode(), 1)
        bad = got.get(SHADOW_TOPIC + "/get/rejected")
        if not bad or bad.get("code") != 400 or bad.get("message") != "Invalid clientToken":
            problems.append("a token over 64 bytes: no get/rejected 400 Invalid clientToken: %r" % bad)
    finally:
        c.close()
    return problems, notes


def mqtt_shadow_oversize_check(broker, pki):
    """A shadow answer over the Maximum Packet Size the client announced is
    dropped whole - never cut, never a disconnect: the board's call then
    times out (MQTT 5, and what the board must expect of AWS)."""
    problems, notes = [], []
    c = MqttProbe(broker, pki, max_packet=1024)
    try:
        for t in SHADOW_ANSWERS[:2]:
            c.subscribe(t)
        c.publish(SHADOW_TOPIC + "/get", b'{"clientToken":"b-9"}')
        got = c.messages(1, seconds=2)
        notes.append("get -> %r" % got)
        if got:
            problems.append("an answer over 1024 bytes reached the client: %r" % [t for t, _ in got])
        # The connection still stands: a small request still gets its answer.
        c.publish(SHADOW_TOPIC + "/name/none/get", b'{"clientToken":"b-10"}')
        c.subscribe(SHADOW_TOPIC + "/name/none/get/rejected")
        c.publish(SHADOW_TOPIC + "/name/none/get", b'{"clientToken":"b-11"}')
        got = c.messages(1, seconds=2)
        notes.append("a named shadow's get -> %r" % got)
        if not got or got[0][1].get("clientToken") != "b-11":
            problems.append("after the dropped answer the connection did not answer a small get: %r" % got)
    finally:
        c.close()
    return problems, notes


def shadow_delete_check(broker, pki):
    """DELETE, as shadow_delete() sends it - the helper sends GET and POST only:
    200 with the version, then 404, then an update that goes on from that
    version, as AWS's do within 48 hours."""
    problems, notes = [], []
    ask = shadow_asker(broker, pki, notes)
    target = "/things/%s/shadow" % THING
    status, doc = ask("DELETE", target)
    if status != 200 or not doc or doc.get("version") != 1:
        problems.append("DELETE: %d %r, not 200 with version 1" % (status, doc))
    status, doc = ask("GET", target)
    if status != 404 or "No shadow exists" not in (doc or {}).get("message", ""):
        problems.append("GET after DELETE: %d %r, not 404" % (status, doc))
    status, doc = ask("DELETE", target)
    if status != 404:
        problems.append("a second DELETE: %d, not 404" % status)
    status, doc = ask("POST", target, b'{"state":{"reported":{"led":true}}}')
    if status != 200 or not doc or doc.get("version") != 2:
        problems.append("an update after DELETE: %d %r, not 200 with version 2" % (status, doc))
    status, doc = ask("PUT", target, b"{}")
    if status != 405:
        problems.append("PUT: %d, not 405" % status)
    return problems, notes


CASES = [
    Case("0", "wire: the handshake the mock sends, against AWS's", wire=True),
    Case("1a", 'good path on "443" with ALPN', info_lacks=("settled the keep-alive",),
         info_has=GOOD_INFO + ("subscribed to `rustonchip-test/cmd`", "subscribed to `rustonchip-test/+/set`"),
         log_has=("Receive Maximum=1, Maximum Packet Size=1024, Request Problem Information=0", "clean start",
                  "SUBSCRIBE id 1 ", "SUBSCRIBE id 2 ", "id 0x7E57", "RustOnChip: Test from this PC",
                  "retained `rustonchip-test/cmd` sent after the SUBACK", "PUBACK 0x10",
                  "the client sent DISCONNECT")),
    Case("1b", 'good path on "8883", no ALPN', port="8883", alpn=False),
    Case("1c", "the test publication comes back on its own subscription", filters=(GOOD_TOPIC,),
         result_has=("the subscription", "a publication"), log_has=("delivered `rustonchip-test/test`", "PUBACK 0x00")),
    fails("2", "a filter outside the policy", ("refused the subscription to `other/cmd`", "0x87", "iot:Subscribe"),
          ("subscribed to `rustonchip-test/cmd`",), filters=("rustonchip-test/cmd", "other/cmd")),
    fails("3", "a test topic outside the policy", ("publication on `elsewhere/test`", "0x87", "iot:Publish"),
          GOOD_INFO[:1], topic="elsewhere/test"),
    fails("4", "a $SYS/# filter", ("ended the connection at the subscription to `$SYS/#`",),
          filters=("rustonchip-test/cmd", "$SYS/#"), log_has=("reserved topic",)),
    fails("5a", "a wrong client id: CONNACK 0x87", ("not authorized (0x87)", "iot:Connect"),
          client_id="rustonchip-other"),
    fails("5b", "a wrong client id, --deny rst", (REFUSED_CERT,), client_id="rustonchip-other",
          broker={"deny": "rst"}, log_has=("TCP reset (--deny rst)",)),
    fails("5c", "an empty client id: an id assigned, then 0x87", ("not authorized (0x87)",), client_id="",
          log_has=("Assigned Client Identifier",)),
    Case("6a", "keep-alive 0", keep_alive=0, info_has=GOOD_INFO + ("settled the keep-alive at 1200 s",)),
    Case("6b", "keep-alive 10", keep_alive=10, info_has=GOOD_INFO + ("settled the keep-alive at 30 s",)),
    Case("6c", "keep-alive 3600", keep_alive=3600, info_has=GOOD_INFO + ("settled the keep-alive at 1200 s",)),
    fails("7", '"443" without ALPN, with a certificate (on the real port 443: the hint keys on it)',
          ("x-amzn-mqtt-ca",), ("did not ask for the client certificate",), alpn=False, real_443=True),
    fails("8", "an endpoint on a TLS 1.2 security policy", ("TLS 1.3 security policy",),
          broker={"tls12": True, "chain": "chain4.pem"}),
    fails("9", "the board trusts only the EC Root CA 3", ("another kind of key",), ca=("root3",)),
    fails("10", "--reason-strings", ("reason string",), GOOD_INFO[:1], broker={"reason_strings": True}),
    Case("11", "--retained-before-suback", broker={"retained_before_suback": True},
         log_has=("retained `rustonchip-test/cmd` sent before the SUBACK",)),
    fails("12", "more than 7 slashes in a filter", ("`rustonchip-test/a/b/c/d/e/f/g/h`", "0x8F"),
          ("subscribed to `rustonchip-test/a/b/c/d/e/f/g`",),
          filters=("rustonchip-test/a/b/c/d/e/f/g", "rustonchip-test/a/b/c/d/e/f/g/h")),
    fails("13", "no client certificate", ("hangs up without one",), cert=None),
    Case("14a", "the 4-certificate RSA chain (AWS's over TLS 1.2)", broker={"chain": "chain4.pem"}),
    Case("14b", "the board's CAs in the other order", ca=("root3", "root1")),
    Case("14c", "the board trusts only the cross-signing root", ca=("sfg2",)),
    fails("15a", "a certificate the CA signed but nobody registered", (REFUSED_CERT,), cert="other"),
    fails("15b", "a self-signed client certificate", (REFUSED_CERT,), cert="selfsigned"),
    fails("15c", "no client certificate, refused by an alert instead", ("hangs up without one",), cert=None,
          broker={"nocert": "alert"}),
    Case("15d", "two NewSessionTickets before the CONNACK (AWS sends none)", broker={"tickets": 2}),
    publish("16a", "HTTPS publish on 8443: delivered to the MQTT subscriber",
            log_has=("HTTPS POST /topics/rustonchip-test/test?qos=1", "allowed, 1 receiver(s) -> 200",
                     "delivered `rustonchip-test/test` to 'listener'", "Host '" + ENDPOINT + ":8443'")),
    publish("16b", 'HTTPS publish on "443" with the ALPN x-amzn-http-ca', port="443",
            log_has=("ALPN 'x-amzn-http-ca'", "allowed, 1 receiver(s) -> 200", "Host '" + ENDPOINT + "'")),
    publish_fails("16c", "a topic outside the policy: 403", ("403", "iot:Publish on topic/elsewhere/test"),
                  SENT_CERT, topic="elsewhere/test", log_has=("denied -> 403",)),
    publish_fails("16d", "a certificate nobody registered: a reset", ("refused the board's certificate",),
                  cert="other", log_has=("is not registered: TCP reset",)),
    publish_fails("16e", "no client certificate: a reset", ("hung up without one",), cert=None),
    publish("16f", "a topic with a space, a % and a ?, percent-encoded",
            ("AWS IoT took the message on `rustonchip-test/a b%c?d`",), topic="rustonchip-test/a b%c?d",
            listener="rustonchip-test/#", log_has=("HTTPS POST `rustonchip-test/a b%c?d` qos 1",
                                                    "delivered `rustonchip-test/a b%c?d`")),
    publish("16g", "QoS 0", qos=0, log_has=("qos 0", "-> 200")),
    publish("16h", "nobody subscribed: still 200 (HTTP says nothing of subscribers)", listener=None,
            log_has=("allowed, 0 receiver(s) -> 200",)),
    publish_fails("16i", "an endpoint on a TLS 1.2 security policy", ("TLS 1.3 security policy",),
                  broker={"tls12": True, "chain": "chain4.pem"}),
    publish_fails("16j", "a retained message without iot:RetainPublish: 403", ("403", "iot:RetainPublish"),
                  SENT_CERT, retain=True, log_has=("iot:RetainPublish on topic/rustonchip-test/test denied",)),
    publish("16k", "a retained message with iot:RetainPublish: kept", retain=True, broker={"allow_retain": True},
            log_has=("retained `rustonchip-test/test` stored", "allowed, 1 receiver(s) -> 200")),
    publish("16l", "an empty retained message deletes the topic's", retain=True, payload=b"",
            broker={"allow_retain": True, "retained": {"rustonchip-test/test": b"old"}},
            log_has=("retained `rustonchip-test/test` deleted",)),
    publish("16m", "MQTT 5's properties, in the query and three headers",
            query="&contentType=application%2Fjson&messageExpiry=60&responseTopic=rustonchip-test%2Freply",
            headers=("x-amz-mqtt5-user-properties: " + base64.b64encode(json.dumps(
                         [{"name": "alpha"}, {UP_KEY: UP_VALUE}], separators=(",", ":")).encode()).decode(),
                     "x-amz-mqtt5-correlation-data: AAEC/v8=",
                     "x-amz-mqtt5-payload-format-indicator: UTF8_DATA"),
            log_has=("contentType='application/json'", "messageExpiry=60", "responseTopic='rustonchip-test/reply'",
                     "userProperties=%r" % [("name", "alpha"), (UP_KEY, UP_VALUE)], "correlationData='000102feff'",
                     "payloadFormat='UTF8_DATA'")),
    shadow("17a", "the Device Shadow, none yet: 404 - the policy let the read through",
           ("has no shadow yet", SHADOW_THING), info=SENT_CERT + ("sent GET /things/rustonchip-test/shadow",),
           log_has=("shadow: 404 No shadow exists with name: 'rustonchip-test'",)),
    shadow("17b", "no iot:GetThingShadow: 403", ("403", "iot:GetThingShadow on " + SHADOW_THING), result="fail",
           allow=False, log_has=("iot:GetThingShadow on thing/rustonchip-test denied -> 403",)),
    shadow("17c", "the shadow, with its delta", ("with the shadow of " + SHADOW_THING, "the board keeps 2048 - all of it"),
           info=SENT_CERT + ('the shadow: {"state":{"desired":{"led":true,"color":"RED"}', '"delta":{"led":true}'),
           broker={"shadows": SHADOW_SEED}, log_has=("shadow GET thing/rustonchip-test: version 1 -> 200",)),
    shadow("17d", "a named shadow: ?name=", ("with the shadow of " + SHADOW_THING + "/config",), name="config",
           info=SENT_CERT + ("sent GET /things/rustonchip-test/shadow?name=config",),
           broker={"shadows": {(THING, "config"): {"desired": {"rate": 5}}}}),
    shadow("17e", "an update: merged, version 2, the delta to an MQTT subscriber", (SHADOW_THING + " took the update",),
           method="POST", payload=b'{"state":{"reported":{"color":"GREEN"}},"clientToken":"t-1"}',
           listener=SHADOW_TOPIC + "/update/delta",
           broker={"shadows": SHADOW_SEED, "allow": [SHADOW_TOPIC + "/#"]},
           log_has=("shadow POST thing/rustonchip-test: version 2", "delivered `" + SHADOW_TOPIC + "/update/delta`",
                    '{"state":{"led":true,"color":"RED"}')),
    shadow("17f", "an update for another version: 409", ("409", "Version conflict"), result="fail", method="POST",
           payload=b'{"state":{"reported":{"led":true}},"version":7}', broker={"shadows": SHADOW_SEED}),
    shadow("17g", "an update without `state`: 400", ("400", "Missing required node: state"), result="fail",
           method="POST", payload=b'{"reported":{"led":true}}'),
    shadow("17h", "a shadow over the board's Answer up to", ("over the board's 2048 (Answer up to)",),
           broker={"shadows": {(THING, None): {"reported": {"blob": "x" * 2500}}}}),
    shadow("17i", 'the shadow on "443" with the ALPN x-amzn-http-ca', ("has no shadow yet",), port="443",
           log_has=("ALPN 'x-amzn-http-ca'",)),
    shadow("17j", "a certificate nobody registered: a reset", ("refused the board's certificate",), result="fail",
           cert="other", info=(), log_has=("is not registered: TCP reset",)),
    Case("17k", "DELETE: the version kept, 404 after, the next update goes on from it", direct=shadow_delete_check,
         broker={"allow_shadow": True, "shadows": SHADOW_SEED}),
    Case("17l", "updates read back: merged, null removes, the version matched, true is no 1, NaN refused",
         direct=shadow_update_check, broker={"allow_shadow": True, "shadows": SHADOW_SEED}),
    Case("18a", "Test from this PC with the Device Shadow over MQTT: the get answered", port="8883", alpn=False,
         shadow_filters=answers_of(SHADOW_TOPIC), shadow_max=2048, shadow_work=2368,
         broker={"allow_mqtt_shadow": True, "shadows": SHADOW_SEED},
         result_has=("all 2 subscriptions", "a publication", "the shadow's get"),
         info_has=GOOD_INFO + ("subscribed to the Device Shadow's 7 answer topics, before the board's own",
                               "the classic shadow answered", "version 1 - the board takes it whole"),
         log_has=("Maximum Packet Size=65536", "shadow over MQTT get rustonchip-test: version 1 -> get/accepted")),
    Case("18b", "the shadow over MQTT, none yet: 404 - the policy let the get through", port="8883", alpn=False,
         shadow_filters=answers_of(SHADOW_TOPIC), shadow_max=2048, shadow_work=2368, broker={"allow_mqtt_shadow": True},
         result_has=("the shadow's get",), info_has=GOOD_INFO + ("the classic shadow does not exist yet (404)",),
         log_has=("shadow over MQTT get rustonchip-test: 404 No shadow exists",)),
    fails("18c", "no subscription to the shadow's answers in the policy: SUBACK 0x87",
          ("the shadow's answers `$aws/things/rustonchip-test/shadow/get/accepted`", "0x87",
           "iot:Subscribe on topicfilter/$aws/things/rustonchip-test/shadow/*",
           "every call on the classic shadow answers Refused"), GOOD_INFO,
          port="8883", alpn=False, shadow_filters=answers_of(SHADOW_TOPIC), shadow_max=2048, shadow_work=2368),
    fails("18d", "the answers subscribed, the get not allowed: AWS ends the connection",
          ("AWS ended the connection over the classic shadow's get", "0x87",
           "iot:Publish on topic/$aws/things/rustonchip-test/shadow/*"),
          GOOD_INFO + ("subscribed to the Device Shadow's 7 answer topics",),
          port="8883", alpn=False, shadow_filters=answers_of(SHADOW_TOPIC), shadow_max=2048, shadow_work=2368,
          broker={"allow": list(answers_of(SHADOW_TOPIC))}),
    fails("18e", "a shadow over what the board takes: said before it is flashed",
          ("the classic shadow's answer is", "bytes as a packet, over the 1024 the board takes (Document up to 512)"),
          GOOD_INFO,
          port="8883", alpn=False, shadow_filters=answers_of(SHADOW_TOPIC), shadow_max=512, shadow_work=1024,
          broker={"allow_mqtt_shadow": True, "shadows": {(THING, None): {"reported": {"blob": "x" * 1500}}}}),
    fails("18h", "a named shadow over what the board takes, the classic one fine: both asked, the named one said",
          ("the shadow `config`'s answer is", "over the 2368 the board takes"),
          GOOD_INFO + ("the classic shadow answered",),
          port="8883", alpn=False,
          shadow_filters=answers_of(SHADOW_TOPIC) + answers_of(SHADOW_TOPIC + "/name/config"),
          shadow_max=2048, shadow_work=2368,
          broker={"allow_mqtt_shadow": True,
                  "shadows": {**SHADOW_SEED, (THING, "config"): {"reported": {"blob": "x" * 2600}}}}),
    Case("18i", "the Device Shadow's topics subscribed first: AWS's 50 a connection refuse the user's last",
         result="fail", result_has=("0x97",), info_has=("subscribed to the Device Shadow's 14 answer topics",),
         port="8883", alpn=False, topic="",
         filters=tuple("rustonchip-test/f%d" % k for k in range(40)),
         shadow_filters=answers_of(SHADOW_TOPIC) + answers_of(SHADOW_TOPIC + "/name/config"),
         shadow_max=2048, shadow_work=2368, broker={"allow_mqtt_shadow": True}),
    Case("18f", "shadow requests over MQTT: answers carry the token, delta pushed, 409, 400 without a token, "
         "delete, 404, a long token refused", direct=mqtt_shadow_check,
         broker={"allow_mqtt_shadow": True, "shadows": SHADOW_SEED}),
    Case("18g", "an answer over the client's Maximum Packet Size is dropped whole, the connection kept",
         direct=mqtt_shadow_oversize_check,
         broker={"allow_mqtt_shadow": True, "shadows": {(THING, None): {"reported": {"blob": "x" * 1500}}}},
         log_has=("over the client's Maximum Packet Size: dropped",)),
]


def helper_request(case, pki, port):
    cert = key = ""
    if case.cert:
        cert = pki.path(case.cert + ".pem").read_text()
        key = pki.path(("device" if case.cert == "device" else "other") + ".key").read_text()
    ca = b"".join(pki.der(name) for name in case.ca)
    values = ["127.0.0.1", str(port), ENDPOINT, "1", ca, case.client_id, "", "", "1" if case.alpn else "0",
              cert, key, str(case.keep_alive), "\n".join(case.filters), case.topic,
              "\n".join(case.shadow_filters), str(case.shadow_max), str(case.shadow_work)]
    return "".join(hexl(v) + "\n" for v in values).encode("ascii")


def helper_http_request(case, pki, port):
    """The 17 lines of the helper's `http` mode: a POST as aws_publish() sends it,
    dialled at the mock while named and checked as the endpoint."""
    import urllib.parse
    cert = key = ""
    if case.cert:
        cert = pki.path(case.cert + ".pem").read_text()
        key = pki.path(("device" if case.cert == "device" else "other") + ".key").read_text()
    ca = b"".join(pki.der(name) for name in case.ca)
    on_443 = case.port == "443"
    method = "POST"
    if case.shadow:
        # As aws_shadow_url writes it.
        name = case.shadow["name"]
        url = "https://%s%s/things/%s/shadow%s" % (ENDPOINT, "" if on_443 else ":8443", urllib.parse.quote(THING, safe=""),
                                                   "" if name is None else "?name=" + urllib.parse.quote(name, safe=""))
        method = case.shadow["method"]
    else:
        url = "https://%s%s/topics/%s?qos=%d%s%s" % (ENDPOINT, "" if on_443 else ":8443",
                                                     urllib.parse.quote(case.topic, safe="/"), case.qos,
                                                     "&retain=true" if case.retain else "", case.query)
    values = [url, "1", ca, "0", "2048", "2048", "RustOnChip", "1024", method, "", case.payload, cert, key,
              ALPN_HTTP if on_443 else "", "127.0.0.1:%d" % port, "1", "\n".join(case.headers)]
    return "".join(hexl(v) + "\n" for v in values).encode("ascii")


def wire_check(broker):
    """A Python TLS client against the "443" side, catching the server's handshake messages."""
    if not hasattr(ssl.SSLContext, "_msg_callback"):
        return ["this Python's ssl has no message callback"], []
    seen, notes, problems = {}, [], []

    def callback(conn, direction, version, content_type, msg_type, data):
        if direction == "read" and content_type == ssl._TLSContentType.HANDSHAKE:
            seen.setdefault(getattr(msg_type, "name", str(msg_type)), data)

    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_3
    ctx.load_verify_locations(str(broker.pki.path("root1.pem")))
    ctx.set_alpn_protocols([ALPN_MQTT])
    ctx._msg_callback = callback
    raw = socket.create_connection(("127.0.0.1", broker.ports["443"]), timeout=10)
    try:
        s = ctx.wrap_socket(raw, server_hostname=ENDPOINT)
        notes.append("%s %s, ALPN %r, chain verified to the Test Root CA 1 by OpenSSL"
                     % (s.version(), s.cipher()[0], s.selected_alpn_protocol()))
        if s.selected_alpn_protocol() != ALPN_MQTT:
            problems.append("the ALPN was not echoed")
        s.settimeout(3)
        try:
            s.recv(10)
            notes.append("without a client certificate: closed")
        except (OSError, ssl.SSLError) as e:
            notes.append("without a client certificate the read gets %s" % type(e).__name__)
    except (OSError, ssl.SSLError) as e:
        problems.append("handshake: %s" % e)
    finally:
        raw.close()
    cr = seen.get("CERTIFICATE_REQUEST", b"")
    ee = seen.get("ENCRYPTED_EXTENSIONS", b"")
    cv = seen.get("CERTIFICATE_VERIFY", b"")
    if broker.sigalgs_set:
        (notes if cr == AWS_CR else problems).append("CertificateRequest %s %s AWS's" % (cr.hex(), "==" if cr == AWS_CR else "!="))
    else:
        notes.append("CertificateRequest %s (OpenSSL's own list; --aws-sigalgs gives AWS's)" % cr.hex())
    (notes if ee == AWS_EE else problems).append("EncryptedExtensions %s %s AWS's" % (ee.hex(), "==" if ee == AWS_EE else "!="))
    scheme = cv[4:6].hex()
    (notes if scheme == "0804" else problems).append("CertificateVerify scheme %s (AWS: 0804)" % scheme)
    return problems, notes


def run_case(case, pki, helper, aws_sigalgs):
    # A case's own retained messages win over the seeded ones.
    options = Options(aws_sigalgs=aws_sigalgs, **{"retained": dict(SEED_RETAINED), **case.broker})
    broker = Broker(pki, options)
    t0 = time.monotonic()
    out = {"status": "PASS", "why": [], "output": [], "log": broker.lines, "seconds": 0.0}
    if case.https and case.listener:
        broker.sessions["listener"] = Listener(broker, case.listener)
    try:
        broker.start("127.0.0.1", 443 if case.real_443 else 0, 0, 0)
    except OSError as e:
        out.update(status="SKIP", why=["cannot listen on 127.0.0.1:%d (%s)" % (443 if case.real_443 else 0, e)])
        return out
    try:
        if case.wire:
            out["why"], out["output"] = wire_check(broker)
        elif case.direct:
            try:
                out["why"], out["output"] = case.direct(broker, pki)
            except Exception as e:  # a FAIL line, never the end of the run
                out["why"] = ["%s: %s" % (type(e).__name__, e)]
            broker.wait_idle(5)
            out["why"] += ["the mock never logged %r" % s for s in case.log_has if not any(s in line for line in broker.lines)]
        else:
            port = broker.ports[case.port]
            if case.https:
                command, request = [helper, "http"], helper_http_request(case, pki, port)
            else:
                command, request = [helper], helper_request(case, pki, port)
            try:
                r = subprocess.run(command, input=request, capture_output=True, timeout=60)
                out["output"] = r.stdout.decode("utf-8", "replace").splitlines()
                if r.stderr.strip():
                    out["output"].append("STDERR " + r.stderr.decode("utf-8", "replace").strip()[:800])
            except subprocess.TimeoutExpired:
                out["output"] = ["(the helper ran over 60 s)"]
            broker.wait_idle(5)  # the broker's last lines, after the helper hung up
            out["why"] = judge(case, out["output"], broker.lines)
    finally:
        broker.stop()
    out["seconds"] = time.monotonic() - t0
    if out["why"]:
        out["status"] = "FAIL"
    return out


def judge(case, output, log):
    results = [line for line in output if line.startswith("RESULT ")]
    infos = [line for line in output if line.startswith("INFO ")]
    why = []
    if len(results) != 1:
        why.append("%d RESULT lines" % len(results))
    result = results[-1] if results else ""
    status = result.split(" ", 2)[1] if result.count(" ") else ""
    if status != case.result:
        why.append("RESULT %s where %s was expected" % (status or "(none)", case.result))
    why += ["RESULT lacks %r" % s for s in case.result_has if s not in result]
    why += ["no INFO line has %r" % s for s in case.info_has if not any(s in line for line in infos)]
    why += ["an INFO line has %r" % s for s in case.info_lacks if any(s in line for line in infos)]
    why += ["the mock never logged %r" % s for s in case.log_has if not any(s in line for line in log)]
    return why


def run_cases(argv):
    ap = argparse.ArgumentParser(prog="aws-iot-mock.py", description="Run the self-checking cases against the "
                                 "helper (`serve` as the first argument runs the broker alone).")
    ap.add_argument("--helper", help="the helper exe (default: $RUSTONCHIP_TLS_CHECK, else %s)" % DEFAULT_HELPER)
    ap.add_argument("--work", default=default_work(), help="where the PKI lives (default: %(default)s)")
    ap.add_argument("--aws-sigalgs", action="store_true", help="AWS's CertificateRequest signature list (ctypes)")
    ap.add_argument("--jobs", type=int, default=4, help="cases at once (default: %(default)s)")
    ap.add_argument("-v", "--verbose", action="store_true", help="print every case's output and broker log")
    ap.add_argument("cases", nargs="*", help="case ids: 1 = 1a 1b 1c; none = all")
    args = ap.parse_args(argv)
    helper = find_helper(args.helper)
    if not os.path.isfile(helper):
        sys.exit("no helper at %s: pass --helper or set RUSTONCHIP_TLS_CHECK" % helper)
    unknown = [a for a in args.cases if not any(c.picked_by(a) for c in CASES)]
    if unknown:
        sys.exit("no case %s; the cases are %s" % (", ".join(unknown), " ".join(c.cid for c in CASES)))
    picked = [c for c in CASES if not args.cases or any(c.picked_by(a) for a in args.cases)]
    openssl = find_openssl()
    pki = Pki(args.work, openssl, helper).ensure()
    print("helper   %s (built %s)" % (helper, time.ctime(os.path.getmtime(helper))))
    print("openssl  %s | Python %s, %s" % (openssl, platform.python_version(), ssl.OPENSSL_VERSION))
    print("work     %s" % pki.work, flush=True)
    t0 = time.monotonic()
    tally = collections.Counter()
    with concurrent.futures.ThreadPoolExecutor(max(1, args.jobs)) as pool:
        outcomes = pool.map(lambda c: run_case(c, pki, helper, args.aws_sigalgs), picked)
        for case, out in zip(picked, outcomes):
            tally[out["status"]] += 1
            print("%-4s  %-4s %s (%.1f s)" % (out["status"], case.cid, case.title, out["seconds"]), flush=True)
            if out["status"] != "PASS" or args.verbose:
                for line in out["why"]:
                    print("            " + ("! " if out["status"] == "FAIL" else "") + line)
                for line in out["output"]:
                    print("            " + line)
                for line in out["log"]:
                    print("              mock " + line)
    print("%d cases in %.0f s: %d PASS, %d FAIL, %d SKIP"
          % (len(picked), time.monotonic() - t0, tally["PASS"], tally["FAIL"], tally["SKIP"]))
    return 1 if tally["FAIL"] else 0


# --- serve -------------------------------------------------------------------------

def this_pc_address(bind):
    if bind not in ("0.0.0.0", ""):
        return bind
    probe = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    try:
        probe.connect(("192.0.2.1", 9))  # TEST-NET-1: a UDP connect picks the route and sends nothing
        return probe.getsockname()[0]
    except OSError:
        return socket.gethostbyname(socket.gethostname())
    finally:
        probe.close()


def serve(argv):
    ap = argparse.ArgumentParser(prog="aws-iot-mock.py serve", description="The broker alone, for a real board.")
    ap.add_argument("--bind", default="127.0.0.1", help="0.0.0.0 lets a board reach it (default: %(default)s)")
    ap.add_argument("--port-443", type=int, default=18443, help='the "443" side (default: %(default)s)')
    ap.add_argument("--port-8883", type=int, default=18883, help='the "8883" side (default: %(default)s)')
    ap.add_argument("--port-8443", type=int, default=28443, help='the "8443" side, HTTPS publish (default: %(default)s)')
    ap.add_argument("--work", default=default_work(), help="where the PKI lives (default: %(default)s)")
    ap.add_argument("--helper", help="makes the board's key and CSR the first time (else openssl does)")
    ap.add_argument("--aws-sigalgs", action="store_true", help="AWS's CertificateRequest signature list (ctypes)")
    ap.add_argument("--allow", action="append", default=[], metavar="FILTER",
                    help="also allow iot:Subscribe on FILTER and iot:Receive on what it matches (repeatable)")
    ap.add_argument("--deny", choices=("connack", "rst"), default="connack",
                    help="a client id that is not the thing: CONNACK 0x87 (default) or a TCP reset")
    ap.add_argument("--reason-strings", action="store_true", help="reason strings in SUBACK/PUBACK regardless")
    ap.add_argument("--retained-before-suback", action="store_true", help="a retained message ahead of its SUBACK")
    ap.add_argument("--retain", action="append", default=[], metavar="TOPIC=TEXT", help="a retained message")
    ap.add_argument("--csr", action="append", default=[], metavar="FILE",
                    help="sign this CSR (the IDE's) and register it to the thing, as create-certificate-from-csr")
    ap.add_argument("--tls12", action="store_true", help="act as an endpoint with no TLS 1.3 security policy")
    ap.add_argument("--allow-retain", action="store_true", help="also allow iot:RetainPublish on topic/<thing>/*")
    ap.add_argument("--allow-shadow", action="store_true",
                    help="also allow iot:Get/Update/DeleteThingShadow on thing/<thing> and thing/<thing>/*")
    ap.add_argument("--allow-mqtt-shadow", action="store_true",
                    help="also allow iot:Publish, iot:Subscribe and iot:Receive on $aws/things/<thing>/shadow/*")
    args = ap.parse_args(argv)
    helper = find_helper(args.helper)
    pki = Pki(args.work, find_openssl(), helper if os.path.isfile(helper) else None).ensure()
    retained = {}
    for item in args.retain:
        topic, _, text = item.partition("=")
        retained[topic] = text.encode("utf-8")
    options = Options(allow=args.allow, deny=args.deny, reason_strings=args.reason_strings,
                      retained_before_suback=args.retained_before_suback, retained=retained,
                      aws_sigalgs=args.aws_sigalgs, tls12=args.tls12, chain="chain4.pem" if args.tls12 else "chain3.pem",
                      allow_retain=args.allow_retain, allow_shadow=args.allow_shadow,
                      allow_mqtt_shadow=args.allow_mqtt_shadow)
    broker = Broker(pki, options, echo=True)
    certs = [(pki.path("device.pem"), pki.path("device.key"))]
    for csr in args.csr:
        pem = pki.sign_csr(csr)
        broker.registry[fingerprint(ssl.PEM_cert_to_DER_cert(pem.read_text()))] = THING
        certs.append((pem, None))
    broker.start(args.bind, args.port_443, args.port_8883, args.port_8443)
    loopback = args.bind.startswith("127.") or args.bind == "localhost"
    host = this_pc_address("0.0.0.0" if loopback else args.bind)
    print('AWS IoT Core look-alike, thing %s, on %s: "8883" side :%d, "443" side :%d, "8443" side :%d'
          % (THING, args.bind, broker.ports["8883"], broker.ports["443"], broker.ports["8443"]))
    print("\nSet in the IoT tab (MQTT, TLS, Client certificate on):")
    print("  Broker host   %s (this PC)%s" % (host, " - restart with --bind 0.0.0.0 first: only this PC reaches "
                                                 "%s" % args.bind if loopback else ""))
    print('  Port          %d (the "8883" side). The IDE adds the ALPN x-amzn-mqtt-ca only on port 443,'
          % broker.ports["8883"])
    print('                so the "443" side needs --port-443 443; on any other port it is the silent HTTPS side.')
    print("  Server name   %s" % ENDPOINT)
    print("  Client id     %s" % THING)
    print("  mqtt_ca.rs    these two roots (%s, %s):" % (pki.path("root1.pem"), pki.path("root3.pem")))
    for name in ("root1", "root3"):
        print(pki.path(name + ".pem").read_text().strip())
    for pem, key in certs:
        print("  secrets.rs    MQTT_CLIENT_CERT = %s" % pem)
        print("                MQTT_CLIENT_KEY  = %s" % (key or "the key the IDE made with that CSR"))
    print("Policy: iot:Connect client/%s; iot:Publish + iot:Receive topic/%s/*; iot:Subscribe topicfilter/%s/*%s"
          % (THING, THING, THING, "".join("; + " + f for f in args.allow)))
    print("\nHTTPS publish (HTTP, HTTPS, Client certificate on, sent only to %s; http_ca.rs holds the two"
          % ENDPOINT)
    print("roots above): the board dials %s on 8443 (or 443) by name, so a real board reaches" % ENDPOINT)
    print("this side only with --port-8443 8443 and a DNS entry pointing that name at this PC. From this")
    print("PC the helper (the mock's cases, `python scripts/aws-iot-mock.py 16`) dials it directly:")
    print("  POST https://%s:8443/topics/%s/test?qos=1  ->  127.0.0.1:%d" % (ENDPOINT, THING, broker.ports["8443"]))
    if args.allow_shadow:
        print("  GET  https://%s:8443/things/%s/shadow     (the Device Shadow, --allow-shadow)" % (ENDPOINT, THING))
    print("Ctrl+C stops it.\n", flush=True)
    try:
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        broker.stop()
    return 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["serve"]:
        sys.exit(serve(sys.argv[2:]))
    sys.exit(run_cases(sys.argv[1:]))
