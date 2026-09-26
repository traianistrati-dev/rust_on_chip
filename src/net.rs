//! Outgoing HTTP: a ureq agent whose timeout really is the timeout, and the
//! words its errors are shown in.
//!
//! The Cargo.toml feature check gives itself 4 s on the UI THREAD during an
//! import. Under ureq 2 its `.timeout()` covered neither the connect (ureq's
//! own 30 s won) nor DNS, and a host that never answered froze the window for
//! 21 s. ureq 3 can bound every step, but only when told to: it sets no
//! timeout of its own at all.

use std::sync::mpsc;
use std::time::Duration;

/// Resolving, and connecting, never get longer than this each, however long
/// the whole request may take: ureq 2's connect default, which the 10-minute
/// AI calls have always had.
const CONNECT_CAP: Duration = Duration::from_secs(30);

/// What [`agent`] returns, for code that takes one as a parameter without
/// naming ureq (see the guard test at the bottom).
pub(crate) type Agent = ureq::Agent;

/// An agent for requests that end within `timeout`.
///
/// `timeout` bounds the request from DNS to the last byte of the body,
/// redirects included, with ONE exception: a TLS handshake that keeps
/// trickling in, because ureq times each handshake read and not the handshake.
/// A caller on the UI thread must not lean on it - see [`within`]. The lookup
/// and the connect are also bounded on their own, each by `timeout` or
/// [`CONNECT_CAP`], whichever is shorter; ureq runs a bounded lookup on a
/// thread of its own, since the system call cannot be interrupted.
///
/// Proxy settings come from the environment (`ALL_PROXY`, `HTTPS_PROXY`,
/// `HTTP_PROXY`, `NO_PROXY`), as ureq 3 does by default. Two differences from
/// curl and cargo: `NO_PROXY=crates.io` matches only that exact host (write
/// `.crates.io` for its subdomains), and ONE proxy serves every scheme. A
/// proxy in the way is named in [`describe`]'s text.
///
/// Cheap to build, so build one per request.
pub(crate) fn agent(timeout: Duration) -> Agent {
    agent_with(timeout, ureq::Proxy::try_from_env())
}

/// [`agent`] that ignores the environment's proxy: for tests that talk to a
/// server on 127.0.0.1, which a developer's `HTTPS_PROXY` would otherwise
/// swallow - ureq exempts no loopback address.
#[cfg(test)]
pub(crate) fn agent_without_proxy(timeout: Duration) -> Agent {
    agent_with(timeout, None)
}

fn agent_with(timeout: Duration, proxy: Option<ureq::Proxy>) -> Agent {
    let step = Some(timeout.min(CONNECT_CAP));
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .timeout_resolve(step)
        .timeout_connect(step)
        .proxy(proxy)
        .build()
        .into()
}

/// `work()`, waited for at most `limit`: `None` when it took longer.
///
/// For the UI thread, where "at most" has to hold whatever the transport
/// does. The work runs on a thread of its own and is left to finish there, so
/// give it a limit of its own too.
pub(crate) fn within<T: Send + 'static>(
    limit: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("bounded request".into())
        .spawn(move || {
            // The receiver is gone once the caller stopped waiting.
            let _ = tx.send(work());
        })
        .ok()?;
    rx.recv_timeout(limit).ok()
}

/// `e`, from a request to `url`, in the words the IDE shows it in: what went
/// wrong, without ureq's own labels (`timeout: global`, `http status: 429`,
/// `io: …`), and naming the environment's proxy when it was in the way - a
/// stale `HTTPS_PROXY` otherwise reads as crates.io refusing the connection.
pub(crate) fn describe(url: &str, e: &ureq::Error) -> String {
    describe_with(ureq::Proxy::try_from_env().as_ref(), url, e)
}

fn describe_with(proxy: Option<&ureq::Proxy>, url: &str, e: &ureq::Error) -> String {
    use ureq::Timeout;
    let (text, on_the_way) = match e {
        ureq::Error::Timeout(Timeout::Resolve) => ("no DNS answer in time".to_owned(), true),
        ureq::Error::Timeout(Timeout::Connect) => ("could not connect in time".to_owned(), true),
        ureq::Error::Timeout(_) => ("timed out".to_owned(), true),
        ureq::Error::StatusCode(code) => (format!("HTTP {code}"), false),
        ureq::Error::HostNotFound => ("host not found".to_owned(), true),
        ureq::Error::ConnectionFailed => ("connection failed".to_owned(), true),
        ureq::Error::Io(io) => (io.to_string(), true),
        ureq::Error::BodyExceedsLimit(limit) => (
            format!("reply larger than {} MiB", limit / (1024 * 1024)),
            false,
        ),
        other => (other.to_string(), false),
    };
    let through = proxy.filter(|p| {
        on_the_way
            && url
                .parse::<ureq::http::Uri>()
                .is_ok_and(|uri| !p.is_no_proxy(&uri))
    });
    match through {
        Some(p) => format!(
            "{text} (through the proxy {}:{} set in the environment)",
            p.host(),
            p.port()
        ),
        None => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const BUDGET: Duration = Duration::from_millis(400);
    /// Scheduling slack on a loaded test machine; the bug this catches took
    /// 21 s (connect).
    const SLACK: Duration = Duration::from_millis(1600);

    /// `agent.get(url).call()`, which must FAIL within the budget.
    ///
    /// Waited for through [`within`], so a lost limit fails here, with this
    /// message, instead of hanging the test run.
    fn call_failing_in_time(what: &str, agent: ureq::Agent, url: &str) -> ureq::Error {
        let owned = url.to_owned();
        let start = Instant::now();
        match within(BUDGET + SLACK, move || agent.get(&owned).call()) {
            Some(Ok(_)) => panic!("{what}: nothing should answer {url}"),
            Some(Err(e)) => e,
            None => panic!(
                "{what}: still waiting after {:?}, the budget was {BUDGET:?}",
                start.elapsed()
            ),
        }
    }

    /// The freeze in the import: a host that swallows the connection attempt.
    /// 192.0.2.1 is TEST-NET-1, reserved for documentation and never routed,
    /// so the SYN goes out and nothing ever comes back. On a machine with no
    /// network at all it fails at once, which passes just the same - the
    /// limits themselves are pinned by the config test below.
    #[test]
    fn a_host_that_never_answers_ends_within_the_budget() {
        call_failing_in_time(
            "connect",
            agent_without_proxy(BUDGET),
            "http://192.0.2.1:81/",
        );
    }

    /// Connected, then silence.
    #[test]
    fn a_server_that_never_replies_ends_within_the_budget() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let held = std::thread::spawn(move || listener.accept().map(|(s, _)| s));
        let url = format!("http://127.0.0.1:{port}/");
        let e = call_failing_in_time("read", agent_without_proxy(BUDGET), &url);
        // Timed out, not reset.
        assert!(matches!(e, ureq::Error::Timeout(_)), "{e}");
        assert_eq!(describe_with(None, &url, &e), "timed out");
        drop(held);
    }

    /// A long request keeps a 30 s lookup and connect, it does not get to wait
    /// ten minutes for either; a short one gets its own budget for both.
    ///
    /// The lookup is where the DNS half of the freeze lived. It cannot be made
    /// to hang in a test - the resolver is ureq's own, which spawns a thread
    /// and stops waiting once this limit is set (ureq 3.4.2
    /// `unversioned/resolver.rs`) - so the limit is what is pinned here.
    #[test]
    fn the_lookup_and_connect_limits_follow_the_budget_up_to_30_s() {
        let limits = |t| {
            let timeouts = agent(t).config().timeouts();
            (timeouts.global, timeouts.resolve, timeouts.connect)
        };
        let s = |n| Some(Duration::from_secs(n));
        assert_eq!(limits(Duration::from_secs(600)), (s(600), s(30), s(30)));
        assert_eq!(limits(Duration::from_secs(4)), (s(4), s(4), s(4)));
        // A 404 has to arrive as `Error::StatusCode(404)`: the crates.io
        // lookups tell "no such crate" apart from a failure that way.
        assert!(
            agent(Duration::from_secs(4))
                .config()
                .http_status_as_error()
        );
    }

    #[test]
    fn within_stops_waiting_at_its_limit() {
        let start = Instant::now();
        let late = within(BUDGET, || std::thread::sleep(Duration::from_secs(5)));
        assert!(late.is_none());
        assert!(start.elapsed() < BUDGET + SLACK, "{:?}", start.elapsed());
        assert_eq!(within(BUDGET, || 7), Some(7));
    }

    /// What went wrong, in plain words, with the step it went wrong in.
    #[test]
    fn errors_are_described_without_ureqs_labels() {
        let url = "https://index.crates.io/se/rd/serde";
        let said = |e: ureq::Error| describe_with(None, url, &e);
        assert_eq!(said(ureq::Error::StatusCode(429)), "HTTP 429");
        assert_eq!(
            said(ureq::Error::Timeout(ureq::Timeout::Resolve)),
            "no DNS answer in time"
        );
        assert_eq!(
            said(ureq::Error::Timeout(ureq::Timeout::Connect)),
            "could not connect in time"
        );
        assert_eq!(
            said(ureq::Error::Timeout(ureq::Timeout::RecvBody)),
            "timed out"
        );
        assert_eq!(
            said(ureq::Error::BodyExceedsLimit(10 * 1024 * 1024)),
            "reply larger than 10 MiB"
        );
        let refused = std::io::Error::from(std::io::ErrorKind::ConnectionRefused);
        let text = refused.to_string();
        assert_eq!(said(ureq::Error::Io(refused)), text);
    }

    /// A stale `HTTPS_PROXY` must not read as crates.io refusing the
    /// connection; an answer from the far end is not the proxy's doing, and a
    /// host `NO_PROXY` exempts never went through it.
    #[test]
    fn a_proxy_in_the_way_is_named() {
        let proxy = ureq::Proxy::new("http://127.0.0.1:3128").unwrap();
        let url = "https://index.crates.io/se/rd/serde";
        let refused = ureq::Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let said = describe_with(Some(&proxy), url, &refused);
        assert!(
            said.ends_with("(through the proxy 127.0.0.1:3128 set in the environment)"),
            "{said}"
        );
        assert_eq!(
            describe_with(Some(&proxy), url, &ureq::Error::StatusCode(404)),
            "HTTP 404"
        );
        let exempt = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
            .host("127.0.0.1")
            .port(3128)
            .no_proxy("index.crates.io")
            .build()
            .unwrap();
        assert!(!describe_with(Some(&exempt), url, &refused).contains("proxy"));
    }

    /// Every request goes through [`agent`], with its budget intact: outside
    /// this file `ureq::` may only name what is matched on, and nothing may
    /// loosen a limit or swap the proxy.
    #[test]
    fn every_request_is_built_on_this_agent() {
        /// The only `ureq::` paths allowed outside this file.
        const NAMES: [&str; 4] = ["ureq::Error", "ureq::Timeout", "ureq::Body", "ureq::http::"];
        /// Leaves a `use ureq::…` may import, same idea.
        const LEAVES: [&str; 4] = ["Error", "Timeout", "Body", "http"];
        /// Per-request config that would undo what [`agent`] set.
        const LOOSENING: [&str; 9] = [
            ".timeout_global(",
            ".timeout_per_call(",
            ".timeout_resolve(",
            ".timeout_connect(",
            ".timeout_send_request(",
            ".timeout_send_body(",
            ".timeout_recv_response(",
            ".timeout_recv_body(",
            ".proxy(",
        ];

        /// `line` without its string literals and `//` comment.
        fn code_of(line: &str) -> String {
            let mut out = String::new();
            let (mut in_str, mut escaped) = (false, false);
            let mut chars = line.chars().peekable();
            while let Some(c) = chars.next() {
                if in_str {
                    match (escaped, c) {
                        (false, '\\') => escaped = true,
                        (false, '"') => in_str = false,
                        _ => escaped = false,
                    }
                } else if c == '"' {
                    in_str = true;
                    out.push_str("\"\"");
                } else if c == '/' && chars.peek() == Some(&'/') {
                    break;
                } else {
                    out.push(c);
                }
            }
            out
        }
        /// Whether a whole `use` statement brings in something that sends.
        fn imports_a_sender(statement: &str) -> bool {
            let s: String = statement.split_whitespace().collect::<Vec<_>>().join(" ");
            let s = s
                .trim_start_matches("pub(crate) ")
                .trim_start_matches("pub ");
            let Some(rest) = s.strip_prefix("use ").map(|r| r.trim_start_matches("::")) else {
                return false;
            };
            if !rest.starts_with("ureq") {
                return false;
            }
            let Some(rest) = rest.strip_prefix("ureq::") else {
                return true; // `use ureq;`, `use ureq as u;`
            };
            rest.trim_end_matches(';')
                .replace(['{', '}'], ",")
                .split(',')
                .map(|leaf| leaf.split(" as ").next().unwrap_or("").trim())
                .filter(|leaf| !leaf.is_empty())
                .any(|leaf| {
                    !LEAVES
                        .iter()
                        .any(|ok| leaf == *ok || leaf.starts_with("http::"))
                })
        }
        fn offences(text: &str) -> Vec<usize> {
            let lines: Vec<String> = text.lines().map(code_of).collect();
            let mut found = Vec::new();
            let mut i = 0;
            while i < lines.len() {
                let line = &lines[i];
                let trimmed = line.trim_start();
                if trimmed.starts_with("use ")
                    || trimmed.starts_with("pub use ")
                    || trimmed.starts_with("pub(crate) use ")
                {
                    // A `use` rustfmt spread over several lines is one statement.
                    let start = i;
                    let mut statement = line.clone();
                    while !statement.contains(';') && i + 1 < lines.len() {
                        i += 1;
                        statement.push(' ');
                        statement.push_str(&lines[i]);
                    }
                    if imports_a_sender(&statement) {
                        found.push(start + 1);
                    }
                } else {
                    let bad_path = line
                        .match_indices("ureq::")
                        .any(|(at, _)| !NAMES.iter().any(|ok| line[at..].starts_with(ok)));
                    if bad_path || LOOSENING.iter().any(|l| line.contains(l)) {
                        found.push(i + 1);
                    }
                }
                i += 1;
            }
            found
        }
        fn scan(dir: &std::path::Path, exempt: &std::path::Path, found: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    scan(&p, exempt, found);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("rs") || p == exempt {
                    continue;
                }
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                for line in offences(&text) {
                    found.push(format!("{}:{line}", p.display()));
                }
            }
        }

        // The shapes that slipped past the first version of this guard.
        for sends in [
            "let r = ureq::get(url).call();",
            "use ureq::{\n    Error, get,\n    http::{Method, Request},\n};",
            "use ureq as u;",
            "pub(crate) use ureq::post as send_it;",
            "let a = ureq::config::Config::default().new_agent();",
            "let a = ureq::Agent::from(config);",
            "urls.map(ureq::get);",
            "net::agent(t).get(u).config().timeout_global(None).build();",
            "req.config().proxy(None).build();",
        ] {
            assert_eq!(offences(sends).len(), 1, "should be flagged: {sends}");
        }
        for fine in [
            "Err(ureq::Error::StatusCode(404)) => None,",
            "use ureq::Error as UreqError;",
            "use ureq::http::StatusCode;",
            "use ureq::{Error, Timeout};",
            "log(\"was ureq::get( before\"); // ureq::get( here too",
            "let cfg = rustls::ClientConfig::builder();",
            "let t = c.timeout_us;",
        ] {
            assert!(offences(fine).is_empty(), "should pass: {fine}");
        }

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut found = Vec::new();
        scan(&src, &src.join("net.rs"), &mut found);
        assert!(
            found.is_empty(),
            "build these requests on crate::net::agent, as they are:\n{}",
            found.join("\n")
        );
    }
}
