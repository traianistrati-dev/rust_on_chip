//! Live crates.io lookups for Cargo.toml completion: name search, and the one
//! canonical-spelling question the sparse index cannot answer.
//!
//! The curated list in `cargo_complete` stays first. This adds a second group
//! below it, so a crate outside that list — one published an hour ago included
//! — is reachable by `Ctrl+Space` at all. Before, it never was.
//!
//! Three properties of crates.io shape everything here, all measured:
//!
//! - **The search matches WORDS, not prefixes.** `q=hmm` does not find
//!   `hmmd_mmwave_sensor_async`; `q=hmmd` and `q=mmwave` do. So answers of
//!   EARLIER queries keep being shown (filtered) while a longer one is pending.
//! - **No ordering surfaces names.** The search matches descriptions and
//!   keywords too, and a short word has hundreds of matches: relevance order put
//!   `embassy-embedded-hal` outside the first 100 of 808 for `embassy`, while
//!   download order has it 10th but pushes an exact `time` down to 24th. Neither
//!   page alone is enough, so a truncated answer is followed by the download
//!   ordered page, and the popup says when matches were left out.
//! - **The sparse index is keyed by the exact published spelling.** `-` and `_`
//!   are NOT interchangeable there (`hmmd-mmwave-sensor-async` is a 404), and
//!   Cargo rejects the other spelling too (and other case: `Heapless`). The API,
//!   unlike the index, resolves either spelling — see [`canonical_name`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Shorter queries are not sent: crates.io matches whole words, and a one- or
/// two-letter word returns hundreds of crates whose names rarely contain it.
pub(crate) const MIN_QUERY_CHARS: usize = 3;
/// Quiet time after the last keystroke before a query is sent.
const DEBOUNCE: Duration = Duration::from_millis(350);
/// crates.io's crawler policy asks for at most one API request per second.
const MIN_SPACING: Duration = Duration::from_secs(1);
/// Results asked for per page — the API's maximum.
const PER_PAGE: usize = 100;
/// A bound on remembered queries — a session types a few dozen at most.
const MAX_CACHED: usize = 64;
/// crates.io refuses API requests without an identifying User-Agent (403).
const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (Cargo.toml crate completion)"
);

/// One crate as the search answered it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Hit {
    /// Exactly as published — this is what goes into the manifest.
    pub name: String,
    pub description: String,
    pub downloads: u64,
}

/// The crates one query brought back, and how many crates.io says match.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Answer {
    pub hits: Vec<Hit>,
    pub total: u64,
}

pub(crate) enum SearchFetch {
    Loading,
    /// The first page is in; the second is still coming. Its hits are shown.
    Partial(Answer),
    Done(Answer),
    Error(String),
}

/// A query to send now, and the slot its answer goes into.
pub(crate) type Request = (String, Arc<Mutex<SearchFetch>>);

/// What the popup should show for the current prefix.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SearchView {
    /// Every hit ANY answered query produced. Unfiltered: the caller keeps only
    /// names containing the prefix, and every such row is a real crate whichever
    /// query found it.
    pub hits: Vec<Hit>,
    /// The current prefix has no full answer yet (debouncing, spaced, in flight).
    pub pending: bool,
    /// The current prefix's query failed.
    pub error: Option<String>,
    /// crates.io matched more crates for the current prefix than were fetched —
    /// the missing ones may include the wanted name. `Some(total)` then.
    pub truncated: Option<u64>,
}

/// Per-editor search state: answers by query, plus the debounce and spacing.
#[derive(Default)]
pub(crate) struct CrateSearch {
    cache: HashMap<String, Arc<Mutex<SearchFetch>>>,
    /// The query the caret wants, and when it started wanting it.
    wanted: Option<(String, Instant)>,
    /// When the next request may go out.
    next_allowed: Option<Instant>,
}

impl CrateSearch {
    /// The view for `prefix` at `now`. When a request should go out, its query
    /// and the slot to fill are returned too — the caller spawns [`run`], so
    /// this stays free of threads and network and can be tested.
    pub(crate) fn view(&mut self, prefix: &str, now: Instant) -> (SearchView, Option<Request>) {
        let mut view = SearchView {
            hits: self.answered_hits(),
            ..SearchView::default()
        };
        let Some(key) = query_key(prefix) else {
            self.wanted = None;
            return (view, None);
        };
        if let Some(slot) = self.cache.get(&key) {
            match &*slot.lock().unwrap() {
                SearchFetch::Loading | SearchFetch::Partial(_) => view.pending = true,
                SearchFetch::Done(a) => {
                    view.truncated = (a.total > a.hits.len() as u64).then_some(a.total)
                }
                SearchFetch::Error(e) => view.error = Some(e.clone()),
            }
            return (view, None);
        }

        view.pending = true;
        let since = match &self.wanted {
            Some((q, t)) if *q == key => *t,
            _ => {
                self.wanted = Some((key.clone(), now));
                now
            }
        };
        let quiet = now.saturating_duration_since(since) >= DEBOUNCE;
        let allowed = self.next_allowed.is_none_or(|t| now >= t);
        if !(quiet && allowed) {
            return (view, None);
        }
        if self.cache.len() >= MAX_CACHED {
            self.cache.clear();
        }
        let slot = Arc::new(Mutex::new(SearchFetch::Loading));
        self.cache.insert(key.clone(), slot.clone());
        // `run` may send a second page one spacing later, so the next query
        // waits for that one too.
        self.next_allowed = Some(now + MIN_SPACING * 2);
        self.wanted = None;
        (view, Some((key, slot)))
    }

    /// Forget failed queries, so reopening the popup retries them — a request
    /// that failed offline must not stay failed after the network comes back.
    pub(crate) fn forget_errors(&mut self) {
        self.cache
            .retain(|_, slot| !matches!(&*slot.lock().unwrap(), SearchFetch::Error(_)));
    }

    fn answered_hits(&self) -> Vec<Hit> {
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for slot in self.cache.values() {
            if let SearchFetch::Partial(a) | SearchFetch::Done(a) = &*slot.lock().unwrap() {
                for h in &a.hits {
                    if seen.insert(h.name.clone()) {
                        out.push(h.clone());
                    }
                }
            }
        }
        out
    }
}

/// The spelling-insensitive form Cargo and crates.io compare names by:
/// lower-case, `_` read as `-`.
pub(crate) fn normalize(name: &str) -> String {
    name.to_lowercase().replace('_', "-")
}

/// The cache key for `prefix`, or `None` when it should not be searched: too
/// short, or holding a character no crate name has (which also keeps the URL
/// free of anything needing escaping).
fn query_key(prefix: &str) -> Option<String> {
    let ok = prefix.chars().count() >= MIN_QUERY_CHARS
        && prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| normalize(prefix))
}

/// Answer one query into `slot`, on the calling thread (a background one).
///
/// The relevance page comes first — it puts an exact name at the top. When
/// crates.io matched more than it returned, the download-ordered page follows,
/// one spacing later, and the two are merged. A failed second page keeps the
/// first: rows already on screen must not vanish because of it.
pub(crate) fn run(query: &str, slot: &Mutex<SearchFetch>) {
    let first = match fetch_page(query, "relevance") {
        Ok(a) => a,
        Err(e) => {
            *slot.lock().unwrap() = SearchFetch::Error(e);
            return;
        }
    };
    if first.total <= first.hits.len() as u64 {
        *slot.lock().unwrap() = SearchFetch::Done(first);
        return;
    }
    *slot.lock().unwrap() = SearchFetch::Partial(first.clone());
    std::thread::sleep(MIN_SPACING);
    let answer = match fetch_page(query, "downloads") {
        Ok(second) => merge(first, second),
        Err(_) => first,
    };
    *slot.lock().unwrap() = SearchFetch::Done(answer);
}

/// Only ever called with a key from [`query_key`], so the query needs no
/// escaping.
fn fetch_page(query: &str, sort: &str) -> Result<Answer, String> {
    let url = format!("https://crates.io/api/v1/crates?q={query}&per_page={PER_PAGE}&sort={sort}");
    let body = crate::net::agent(Duration::from_secs(10))
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .call()
        .map_err(|e| crate::net::describe(&url, &e))?
        .into_body()
        .read_to_string()
        .map_err(|e| crate::net::describe(&url, &e))?;
    parse_search(&body)
}

/// Two pages of one query as one answer: the first page's order, then what
/// only the second one found.
fn merge(first: Answer, second: Answer) -> Answer {
    let mut seen: std::collections::HashSet<String> =
        first.hits.iter().map(|h| h.name.clone()).collect();
    let mut hits = first.hits;
    hits.extend(
        second
            .hits
            .into_iter()
            .filter(|h| seen.insert(h.name.clone())),
    );
    Answer {
        hits,
        total: first.total.max(second.total),
    }
}

/// Parse a search answer.
///
/// A body without a `crates` array is an ERROR, never an empty result: a
/// captive portal or proxy answering 200 with a login page must not read as
/// "crates.io has no such crate".
pub(crate) fn parse_search(body: &str) -> Result<Answer, String> {
    let unexpected = || "unexpected answer from crates.io".to_owned();
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| unexpected())?;
    let crates = v["crates"].as_array().ok_or_else(unexpected)?;
    let hits: Vec<Hit> = crates
        .iter()
        .filter_map(|c| {
            Some(Hit {
                name: c["name"].as_str()?.to_owned(),
                // Descriptions are free text, `"""`-quoted ones keep their
                // newlines — and a newline in a 19 px row paints over the rows
                // around it. One line, single-spaced.
                description: c["description"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                downloads: c["downloads"].as_u64().unwrap_or(0),
            })
        })
        .collect();
    let total = v["meta"]["total"]
        .as_u64()
        .unwrap_or(hits.len() as u64)
        .max(hits.len() as u64);
    Ok(Answer { hits, total })
}

/// The spelling `name` was published under, when it differs from `name` only in
/// `-` / `_` (or case).
///
/// `Ok(None)` means crates.io has no such crate under any spelling. The sparse
/// index cannot answer this — it is keyed by the exact spelling — but the API
/// resolves either one to the published crate.
pub(crate) fn canonical_name(name: &str) -> Result<Option<String>, String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Ok(None);
    }
    let url = format!("https://crates.io/api/v1/crates/{name}");
    let resp = crate::net::agent(Duration::from_secs(10))
        .get(&url)
        .header("User-Agent", USER_AGENT)
        .call();
    let body = match resp {
        Ok(r) => r
            .into_body()
            .read_to_string()
            .map_err(|e| crate::net::describe(&url, &e))?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(crate::net::describe(&url, &e)),
    };
    let unexpected = || "unexpected answer from crates.io".to_owned();
    let v: serde_json::Value = serde_json::from_str(&body).map_err(|_| unexpected())?;
    let published = v["crate"]["name"].as_str().ok_or_else(unexpected)?;
    // Only a spelling variant counts; anything else would be a different crate.
    Ok((normalize(published) == normalize(name)).then(|| published.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(names: &[&str], total: u64) -> Answer {
        Answer {
            hits: names.iter().map(|n| hit(n)).collect(),
            total,
        }
    }

    fn set(search: &CrateSearch, key: &str, state: SearchFetch) {
        *search.cache[key].lock().unwrap() = state;
    }

    fn hit(name: &str) -> Hit {
        Hit {
            name: name.to_owned(),
            description: String::new(),
            downloads: 0,
        }
    }

    /// Ask for `prefix` and let the debounce pass; returns the time after.
    fn asked(s: &mut CrateSearch, prefix: &str, t: Instant) -> Instant {
        s.view(prefix, t);
        s.view(prefix, t + DEBOUNCE).1.expect("the query goes out");
        t + DEBOUNCE
    }

    #[test]
    fn a_short_prefix_is_never_sent() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        for p in ["", "h", "hm"] {
            let (view, fire) = s.view(p, t0 + Duration::from_secs(5));
            assert!(fire.is_none(), "{p:?} must not be searched");
            assert!(!view.pending, "{p:?} must not spin forever");
        }
    }

    #[test]
    fn a_query_waits_for_the_typing_to_pause() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        let (view, fire) = s.view("hmmd", t0);
        assert!(fire.is_none());
        assert!(
            view.pending,
            "a wanted query shows as pending while it waits"
        );
        // Another keystroke restarts the wait.
        assert!(s.view("hmmd_", t0 + DEBOUNCE / 2).1.is_none());
        assert!(s.view("hmmd_", t0 + DEBOUNCE).1.is_none());
        let (_, fire) = s.view("hmmd_", t0 + DEBOUNCE / 2 + DEBOUNCE);
        assert_eq!(fire.map(|f| f.0).as_deref(), Some("hmmd-"));
    }

    /// One query may send two pages a spacing apart, so the next query waits
    /// out both — never more than one request per second.
    #[test]
    fn requests_are_spaced_for_two_pages() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        let fired = asked(&mut s, "hmmd", t0);
        s.view("mmwave", fired);
        assert!(s.view("mmwave", fired + MIN_SPACING).1.is_none());
        assert!(s.view("mmwave", fired + MIN_SPACING * 2).1.is_some());
    }

    #[test]
    fn an_answered_query_is_not_asked_again() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        asked(&mut s, "hmmd", t0);
        set(
            &s,
            "hmmd",
            SearchFetch::Done(answer(&["hmmd_mmwave_sensor_async"], 1)),
        );
        let (view, fire) = s.view("hmmd", t0 + Duration::from_secs(10));
        assert!(fire.is_none());
        assert!(!view.pending);
        assert_eq!(view.truncated, None);
        assert_eq!(view.hits, [hit("hmmd_mmwave_sensor_async")]);
    }

    /// The whole reason earlier answers are kept: crates.io matches words, so the
    /// longer query may not be answered yet — or may find nothing new — while the
    /// shorter one already found the crate.
    #[test]
    fn earlier_answers_stay_visible_while_a_longer_query_is_pending() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        let t = asked(&mut s, "hmmd", t0);
        set(
            &s,
            "hmmd",
            SearchFetch::Done(answer(&["hmmd_mmwave_sensor_async"], 1)),
        );
        let (view, _) = s.view("hmmd_mm", t + DEBOUNCE);
        assert!(view.pending);
        assert_eq!(view.hits, [hit("hmmd_mmwave_sensor_async")]);
    }

    /// A first page on screen is still pending — the second may add rows — and
    /// its rows show meanwhile.
    #[test]
    fn a_partial_answer_shows_its_rows_and_keeps_spinning() {
        let mut s = CrateSearch::default();
        let t = asked(&mut s, "embassy", Instant::now());
        set(
            &s,
            "embassy",
            SearchFetch::Partial(answer(&["embassy-dt"], 808)),
        );
        let (view, _) = s.view("embassy", t);
        assert!(view.pending);
        assert_eq!(view.hits, [hit("embassy-dt")]);
    }

    /// Honest about the gap: crates.io matched more than came back, so a missing
    /// name is not proof it does not exist.
    #[test]
    fn a_truncated_answer_says_so() {
        let mut s = CrateSearch::default();
        let t = asked(&mut s, "pca", Instant::now());
        set(
            &s,
            "pca",
            SearchFetch::Done(answer(&["efficient_pca"], 390)),
        );
        assert_eq!(s.view("pca", t).0.truncated, Some(390));
    }

    #[test]
    fn both_spellings_share_one_answer() {
        assert_eq!(query_key("Hmmd_MM"), query_key("hmmd-mm"));
    }

    #[test]
    fn a_failed_query_is_retried_after_forget_errors() {
        let mut s = CrateSearch::default();
        let t0 = Instant::now();
        asked(&mut s, "hmmd", t0);
        set(&s, "hmmd", SearchFetch::Error("offline".into()));
        let (view, fire) = s.view("hmmd", t0 + Duration::from_secs(5));
        assert_eq!(view.error.as_deref(), Some("offline"));
        assert!(fire.is_none(), "a failure is not re-asked every frame");
        s.forget_errors();
        asked(&mut s, "hmmd", t0 + Duration::from_secs(10));
    }

    #[test]
    fn the_second_page_adds_only_what_the_first_lacked() {
        let merged = merge(
            answer(&["time", "embassy-time"], 900),
            answer(&["embassy-time", "chrono"], 905),
        );
        assert_eq!(merged, answer(&["time", "embassy-time", "chrono"], 905));
    }

    #[test]
    fn a_page_that_is_not_a_search_answer_is_an_error_not_empty() {
        for body in [
            "",
            "<html><body>Sign in to continue</body></html>",
            r#"{"errors":[{"detail":"rate limited"}]}"#,
        ] {
            assert!(parse_search(body).is_err(), "{body:?}");
        }
        // The control: a real, empty answer IS empty.
        assert_eq!(
            parse_search(r#"{"crates":[],"meta":{"total":0}}"#),
            Ok(answer(&[], 0))
        );
    }

    #[test]
    fn a_search_answer_keeps_the_published_spelling_on_one_line() {
        let body = r#"{"crates":[{"name":"hmmd_mmwave_sensor_async","description":" Async\n  driver ","downloads":7},{"description":"no name"}],"meta":{"total":3}}"#;
        assert_eq!(
            parse_search(body),
            Ok(Answer {
                hits: vec![Hit {
                    name: "hmmd_mmwave_sensor_async".into(),
                    description: "Async driver".into(),
                    downloads: 7,
                }],
                total: 3,
            })
        );
    }

    /// The crates.io facts this module is built on, against the real service.
    /// Ignored: it needs the network, and a crate it names could one day be
    /// yanked or renamed.
    #[test]
    #[ignore = "talks to crates.io; run with --ignored"]
    fn live_crates_io_matches_words_and_resolves_spellings() {
        let pause = || std::thread::sleep(MIN_SPACING);
        let names = |q: &str, sort: &str| -> Vec<String> {
            let a = fetch_page(q, sort).expect("search answers");
            a.hits.into_iter().map(|h| h.name).collect()
        };
        let target = "hmmd_mmwave_sensor_async".to_owned();
        assert!(names("hmmd", "relevance").contains(&target));
        pause();
        assert!(names("mmwave", "relevance").contains(&target));
        pause();
        // Words, not prefixes — why earlier answers stay on screen.
        assert!(!names("hmm", "relevance").contains(&target));
        pause();
        // Why the second page exists: relevance alone misses a well-known name.
        let slot = Mutex::new(SearchFetch::Loading);
        run("embassy", &slot);
        match &*slot.lock().unwrap() {
            SearchFetch::Done(a) => assert!(
                a.hits.iter().any(|h| h.name == "embassy-embedded-hal"),
                "{} hits",
                a.hits.len()
            ),
            _ => panic!("embassy did not finish"),
        }
        pause();
        assert_eq!(
            canonical_name("hmmd-mmwave-sensor-async"),
            Ok(Some(target.clone()))
        );
        pause();
        assert_eq!(canonical_name("zz-no-such-crate-qqq"), Ok(None));
    }
}
