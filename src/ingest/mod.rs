//! Kill reporting over HTTP.
//!
//! This is a generic kill-reporting webhook: it posts `{token, kills}` to a
//! configured URL and the server resolves everything else. It knows nothing
//! about whatever consumes the reports , no teams, boards, squares, rooms or
//! opponents , and it must stay that way. If this module ever needs that kind
//! of knowledge, that is the signal to split it into its own mod rather than to
//! teach the overlay about one web app.
//!
//! Two properties are load-bearing:
//!
//! * **Read-only.** Flags are only ever read, never written. The same hook that
//!   reads a flag could set one, and that is the line between a tracker and a
//!   cheat tool. Nothing in this module writes to game memory, and no refactor
//!   should change that.
//! * **Full state every send.** Each request carries the complete observed kill
//!   set, not just the newest transition. The server diffs against what it has
//!   already acted on, so a dropped request, a network blip or a mid-match
//!   restart all recover on the next send. That is what buys us no
//!   acknowledgements, no sequence numbers and no replay buffer.
//!
//! The reporter never polls game memory itself. It consumes snapshots produced
//! by the overlay's existing monitor loop (`overlay::core`), which is already
//! reading these flags every tick to draw the HUD.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime},
};

use crossbeam::channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use serde::{Deserialize, Serialize};

use crate::overlay::game_monitor::MonitorObservation;
use crate::{debug_log, overlay::style::Ingest, util::time::rfc3339_millis_utc};

/// Wall-clock budget for one request, DNS through response body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Backoff schedule is `1s, 2s, 4s, …` capped at [`BACKOFF_CAP`].
const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(30);
/// Attempts per send before giving up. The kill set is retained either way and
/// rides along on the next send, so giving up costs a delay, not data.
const MAX_ATTEMPTS: u32 = 5;
/// Upper bound on how long the worker sleeps before re-checking the stop flag.
const TICK: Duration = Duration::from_millis(200);

/// Skip reasons that are the expected result of the protocol working, not
/// something to log.
///
/// All three are the norm rather than the exception, because we deliberately
/// send the whole boss list and deliberately resend the full kill set every
/// time: most flags are not squares at all (`not_a_square`), some are squares
/// that this board did not draw (`not_on_this_board`), and everything already
/// dealt with comes back `already_fired`. Together they account for nearly
/// every entry in `skipped`, so logging them would bury the interesting lines
/// under a couple of hundred per heartbeat.
///
/// Deliberately *not* listed: `no_opponents` and `insert_failed`, which both
/// mean a kill did not land and are worth seeing.
const ROUTINE_SKIPS: [&str; 3] = ["already_fired", "not_on_this_board", "not_a_square"];

//
// ----------------------------------------------------
// Settings
// ----------------------------------------------------
//

#[derive(Clone)]
pub struct IngestSettings {
    pub url: String,
    pub token: String,
    /// Minimum gap between requests. New kills are coalesced into one send
    /// rather than firing a request per flag.
    pub interval: Duration,
    /// How often to resend the full kill set when nothing has changed.
    pub heartbeat: Duration,
}

impl fmt::Debug for IngestSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IngestSettings")
            .field("url", &"[redacted]")
            .field("token", &"[redacted]")
            .field("interval", &self.interval)
            .field("heartbeat", &self.heartbeat)
            .finish()
    }
}

impl IngestSettings {
    /// Builds settings from the `[ingest]` config section.
    ///
    /// Returns `None` when the section is absent or when either `url` or
    /// `token` is empty, which is what keeps the feature off for everyone who
    /// has not opted in.
    pub fn from_config(cfg: Option<&Ingest>) -> Option<Self> {
        let cfg = cfg?;

        let url = cfg.url.as_deref().unwrap_or("").trim().to_string();
        let token = cfg.token.as_deref().unwrap_or("").trim().to_string();

        if url.is_empty() || token.is_empty() {
            debug_log!("[automark] [ingest] disabled (url or token not set)");
            return None;
        }

        // The server is the authority on token validity; a surprising shape is
        // worth a log line but not a refusal to start.
        if token.len() != 48 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
            debug_log!(
                "[automark] [ingest] ⚠ token is not 48 hex characters ({} chars) , sending anyway",
                token.len()
            );
        }

        let interval = Duration::from_millis(cfg.interval_ms.unwrap_or(1_000).max(100));
        let heartbeat = Duration::from_secs(cfg.heartbeat_s.unwrap_or(60).max(5));

        Some(Self {
            url,
            token,
            interval,
            heartbeat,
        })
    }
}

//
// ----------------------------------------------------
// Snapshots in, status out
// ----------------------------------------------------
//

/// The server-computed score for the current match. Rendered verbatim; the
/// overlay derives nothing from it.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct Tally {
    #[serde(default)]
    pub hits: u32,
    #[serde(default)]
    pub misses: u32,
    #[serde(default)]
    pub shots: u32,
    /// `None` before the first shot, which the server sends as an explicit
    /// `null` rather than `0` , nothing has missed yet, so `0%` would be a lie.
    ///
    /// This must be `Option`, not a defaulted `i32`: `#[serde(default)]` only
    /// covers a *missing* field, so `"accuracy": null` would fail to
    /// deserialise, sink the entire response, and turn every match start into a
    /// spurious retry storm.
    #[serde(default)]
    pub accuracy: Option<i32>,
}

/// What the overlay renders. Every field is set from the last response , the
/// overlay holds no tally state of its own.
#[derive(Debug, Clone, Default)]
pub struct IngestStatus {
    /// Whether the server will act on our reports at all , see [`INELIGIBLE`].
    /// The tally line is hidden entirely while this is false, so the overlay
    /// stays off screen in a lobby that kill reporting does not apply to.
    pub eligible: bool,
    pub tally: Option<Tally>,
    /// Last send failed, or was rejected for a reason outside [`INELIGIBLE`].
    /// Without this a stale tally looks identical to a live one.
    pub warn: bool,
    /// Short reason for `warn`, for the expanded overlay and logs.
    pub last_error: Option<String>,
    /// Size of the local kill set, for diagnostics.
    pub kills_tracked: usize,
}

pub type SharedIngestStatus = Arc<RwLock<IngestStatus>>;

pub fn create_status() -> SharedIngestStatus {
    Arc::new(RwLock::new(IngestStatus::default()))
}

//
// ----------------------------------------------------
// Wire format
// ----------------------------------------------------
//

#[derive(Serialize)]
struct Payload<'a> {
    token: &'a str,
    kills: Vec<WireKill>,
}

#[derive(Serialize)]
struct WireKill {
    flag: i32,
    /// First local observation of this flag being true. Optional on the wire;
    /// the server clamps it to a few seconds around arrival. It exists so a
    /// recorded kill time is not inflated by poll and network latency.
    at: String,
}

#[derive(Deserialize)]
struct IngestResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default, deserialize_with = "lenient_tally")]
    tally: Option<Tally>,
    #[serde(default)]
    fired: Vec<FiredEntry>,
    #[serde(default)]
    skipped: Vec<SkippedEntry>,
}

// `fired` and `skipped` are deserialised purely so they can be logged, and
// `debug_log!` compiles to nothing in release builds , hence the allow.
#[derive(Deserialize)]
#[allow(dead_code)]
struct FiredEntry {
    #[serde(default)]
    flag: i64,
    #[serde(default)]
    cell: Option<i64>,
    #[serde(default)]
    result: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct SkippedEntry {
    #[serde(default)]
    flag: i64,
    #[serde(default)]
    reason: Option<String>,
}

/// Reads the tally without letting a surprising shape sink the response.
///
/// The tally is cosmetic, but by the time we are reading it the server has
/// already fired the shots , so a tally we cannot understand must degrade to
/// "no tally shown", never to a failed parse that we would then retry and
/// report as an error. This is the lesson from `accuracy` arriving as `null`:
/// one unexpected field turned a completely successful report into five
/// retries and a warning glyph.
fn lenient_tally<'de, D>(deserializer: D) -> Result<Option<Tally>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value::<Option<Tally>>(raw).ok().flatten())
}

/// Result of one HTTP attempt, classified into what the caller should do next.
enum Attempt {
    /// `ok: true`.
    Accepted(IngestResponse),
    /// This match is not one the server will act on: the player is not in a
    /// live match, or its square set is not the one kill reporting supports.
    /// Normal, not a failure, and not worth a retry.
    Ineligible(String),
    /// `ok: false` with a reason that will not change on retry.
    Rejected(String),
    /// Transport failure, 5xx or 429 , worth retrying.
    Retryable(String),
}

/// Rejections that mean reporting simply does not apply here, rather than that
/// something is wrong.
///
/// These hide the overlay line instead of warning on it. `not_in_match` is the
/// idle case, and `unsupported_square_set` is a lobby whose squares no event
/// flag can settle , an objective set like "collect 4 unique helms". In neither
/// case is there anything for the player to act on, and a warning glyph over a
/// tally left from an earlier match would claim their kills are being lost when
/// there is nothing to lose.
const INELIGIBLE: [&str; 2] = ["not_in_match", "unsupported_square_set"];

/// Classifies an `ok: false` reply.
///
/// Anything not in [`INELIGIBLE`], including anything unrecognised, is treated
/// as a permanent rejection: retrying `unknown_token` cannot help, and treating
/// an unexpected reply as retryable would turn a server-side change into a hot
/// loop. `ambiguous_match` deliberately lands here rather than in `INELIGIBLE`,
/// because it is a real misconfiguration the player can fix , two live matches
/// , and silence would leave them wondering why nothing fires.
fn classify_rejection(error: String) -> Attempt {
    if INELIGIBLE.contains(&error.as_str()) {
        Attempt::Ineligible(error)
    } else {
        Attempt::Rejected(error)
    }
}

//
// ----------------------------------------------------
// Worker
// ----------------------------------------------------
//

/// Starts the reporter worker.
///
/// Returns the sender the monitor loop should push snapshots into, or `None` if
/// the feature is not configured , in which case the caller does no extra work
/// per tick and the mod behaves exactly as it did before.
pub fn start_reporter(
    settings: Option<IngestSettings>,
    status: SharedIngestStatus,
    stop: Arc<AtomicBool>,
) -> Option<Sender<MonitorObservation>> {
    let settings = settings?;
    let (tx, rx) = unbounded();

    debug_log!(
        "[automark] [ingest] reporting enabled (interval {}ms, heartbeat {}s)",
        settings.interval.as_millis(),
        settings.heartbeat.as_secs()
    );

    thread::spawn(move || run(settings, rx, status, stop));
    Some(tx)
}

fn run(
    settings: IngestSettings,
    rx: Receiver<MonitorObservation>,
    status: SharedIngestStatus,
    stop: Arc<AtomicBool>,
) {
    let agent = build_agent();

    // First local observation of each flag being true. Insert-only: a save
    // reload or a quit to menu makes the observed set shrink, and that is never
    // an un-kill. The server only ever adds, so retaining the flag is both
    // harmless and what lets a reloaded save keep reporting correctly.
    let mut first_seen: BTreeMap<i32, SystemTime> = BTreeMap::new();
    let mut unsent = false;
    let mut have_snapshot = false;
    let mut last_send: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) {
        // Absorb everything queued, then block briefly so the stop flag stays
        // responsive. Later snapshots supersede earlier ones.
        let mut received = false;
        loop {
            match rx.try_recv() {
                Ok(snap) => {
                    received = true;
                    have_snapshot = true;
                    unsent |= merge(&mut first_seen, snap);
                }
                Err(_) => break,
            }
        }
        if !received {
            match rx.recv_timeout(TICK) {
                Ok(snap) => {
                    have_snapshot = true;
                    unsent |= merge(&mut first_seen, snap);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    debug_log!("[automark] [ingest] monitor disconnected , worker exiting");
                    break;
                }
            }
        }

        // Nothing is sent until the monitor has produced a snapshot, which is
        // what keeps us silent while the player is not yet in world.
        let due = match last_send {
            // First contact as soon as there is anything to report from, even
            // with an empty kill set: it populates the tally and surfaces a bad
            // token straight away rather than at the first kill of the match.
            None => have_snapshot,
            // New kills are coalesced into one request rather than one each.
            Some(t) if unsent => t.elapsed() >= settings.interval,
            Some(t) => t.elapsed() >= settings.heartbeat,
        };
        if !due {
            continue;
        }

        last_send = Some(Instant::now());
        // Cleared before sending, not after: on failure the kills stay in the
        // local set and ride along on the next heartbeat rather than retrying
        // every `interval`.
        unsent = false;

        send_with_retry(&agent, &settings, &first_seen, &status, &stop);
    }

    debug_log!("[automark] [ingest] worker exiting");
}

/// Folds a snapshot into the kill set. Returns true if anything was newly seen.
fn merge(first_seen: &mut BTreeMap<i32, SystemTime>, observation: MonitorObservation) -> bool {
    let MonitorObservation {
        active_boss_flags,
        observed_at,
    } = observation;
    let mut new_kills = false;

    for flag in active_boss_flags {
        if let Entry::Vacant(slot) = first_seen.entry(flag) {
            slot.insert(observed_at);
            new_kills = true;
        }
    }

    new_kills
}

fn build_agent() -> ureq::Agent {
    // Pooling is disabled so every report is a fresh connect/send/close. The
    // endpoint is a serverless function with a per-invocation wall-clock limit
    // while matches run over an hour, so a held-open socket would be killed
    // partway through and kills would silently stop landing.
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(REQUEST_TIMEOUT))
        // 4xx/5xx must not become `Err`: the endpoint returns its JSON error
        // body with a 400 or 403, and that body is how we tell
        // `missing_token` from `not_in_match`.
        .http_status_as_error(false)
        .max_idle_connections(0)
        .max_idle_connections_per_host(0)
        .max_idle_age(Duration::from_secs(0))
        .build();

    config.into()
}

fn send_with_retry(
    agent: &ureq::Agent,
    settings: &IngestSettings,
    first_seen: &BTreeMap<i32, SystemTime>,
    status: &SharedIngestStatus,
    stop: &Arc<AtomicBool>,
) {
    let body = match build_body(&settings.token, first_seen) {
        Ok(body) => body,
        Err(e) => {
            // Serialising our own struct should not fail; if it somehow does,
            // there is nothing to retry.
            debug_log!("[automark] [ingest] ❌ could not serialise payload: {e}");
            set_warn(status, first_seen.len(), &format!("payload error: {e}"));
            return;
        }
    };

    let mut backoff = BACKOFF_START;

    for attempt in 1..=MAX_ATTEMPTS {
        if stop.load(Ordering::SeqCst) {
            return;
        }

        match send_once(agent, &settings.url, &body) {
            Attempt::Accepted(resp) => {
                log_accepted(&resp);
                let mut w = status.write().unwrap();
                *w = IngestStatus {
                    eligible: true,
                    tally: resp.tally,
                    warn: false,
                    last_error: None,
                    kills_tracked: first_seen.len(),
                };
                return;
            }
            // Underscored so release builds, where `debug_log!` compiles away,
            // do not see it as unused.
            Attempt::Ineligible(_reason) => {
                debug_log!("[automark] [ingest] idle: {_reason}");
                // Wipe the tally rather than leave the last match's numbers
                // sitting on screen in a lobby this does not apply to.
                let mut w = status.write().unwrap();
                *w = IngestStatus {
                    eligible: false,
                    tally: None,
                    warn: false,
                    last_error: None,
                    kills_tracked: first_seen.len(),
                };
                return;
            }
            Attempt::Rejected(error) => {
                let error = redact_config_secrets(&error, settings);
                debug_log!("[automark] [ingest] ❌ rejected: {error}");
                set_warn(status, first_seen.len(), &error);
                return;
            }
            Attempt::Retryable(error) => {
                let error = redact_config_secrets(&error, settings);
                if attempt == MAX_ATTEMPTS {
                    debug_log!(
                        "[automark] [ingest] ❌ giving up after {attempt} attempts: {error} \
                         , kills retained for the next send"
                    );
                    set_warn(status, first_seen.len(), &error);
                    return;
                }
                debug_log!(
                    "[automark] [ingest] ⚠ attempt {attempt} failed ({error}); retrying in {}s",
                    backoff.as_secs()
                );
                if !sleep_interruptible(backoff, stop) {
                    return;
                }
                backoff = (backoff * 2).min(BACKOFF_CAP);
            }
        }
    }
}

fn redact_config_secrets(error: &str, settings: &IngestSettings) -> String {
    error
        .replace(&settings.url, "[redacted endpoint]")
        .replace(&settings.token, "[redacted token]")
}

fn build_body(
    token: &str,
    first_seen: &BTreeMap<i32, SystemTime>,
) -> Result<String, serde_json::Error> {
    let kills = first_seen
        .iter()
        .map(|(&flag, &at)| WireKill {
            flag,
            at: rfc3339_millis_utc(at),
        })
        .collect();

    serde_json::to_string(&Payload { token, kills })
}

fn send_once(agent: &ureq::Agent, url: &str, body: &str) -> Attempt {
    let response = agent
        .post(url)
        .header("Content-Type", "application/json")
        // Belt and braces alongside the disabled pool: no connection is kept.
        .header("Connection", "close")
        .send(body);

    let mut response = match response {
        Ok(r) => r,
        Err(e) => return Attempt::Retryable(format!("transport: {e}")),
    };

    let status = response.status().as_u16();

    let text = match response.body_mut().read_to_string() {
        Ok(t) => t,
        Err(e) => return Attempt::Retryable(format!("read body (HTTP {status}): {e}")),
    };

    // 5xx and 429 are the server's problem and may well pass on a retry.
    if status >= 500 || status == 429 {
        return Attempt::Retryable(format!("HTTP {status}"));
    }

    let parsed: IngestResponse = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            // A proxy or captive portal can return HTML with a 200. Retrying is
            // reasonable and bounded.
            return Attempt::Retryable(format!("HTTP {status}, unparseable body: {e}"));
        }
    };

    if parsed.ok {
        return Attempt::Accepted(parsed);
    }

    classify_rejection(
        parsed
            .error
            .unwrap_or_else(|| format!("HTTP {status}, no error field")),
    )
}

fn set_warn(status: &SharedIngestStatus, kills_tracked: usize, error: &str) {
    let mut w = status.write().unwrap();
    w.warn = true;
    w.last_error = Some(error.to_string());
    w.kills_tracked = kills_tracked;
}

/// Release builds compile `debug_log!` away, leaving these loop bindings unused.
#[cfg_attr(not(debug_assertions), allow(unused_variables))]
fn log_accepted(resp: &IngestResponse) {
    for f in &resp.fired {
        debug_log!(
            "[automark] [ingest] ✅ fired flag {} → cell {:?} ({})",
            f.flag,
            f.cell,
            f.result.as_deref().unwrap_or("?")
        );
    }
    for s in &resp.skipped {
        let reason = s.reason.as_deref().unwrap_or("?");
        if !ROUTINE_SKIPS.contains(&reason) {
            debug_log!("[automark] [ingest] skipped flag {}: {}", s.flag, reason);
        }
    }
}

/// Sleeps in short slices so teardown is not held up by a long backoff.
/// Returns false if the stop flag was raised.
fn sleep_interruptible(total: Duration, stop: &Arc<AtomicBool>) -> bool {
    let mut slept = Duration::ZERO;
    while slept < total {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let slice = TICK.min(total - slept);
        thread::sleep(slice);
        slept += slice;
    }
    !stop.load(Ordering::SeqCst)
}

//
// ----------------------------------------------------
// Overlay text
// ----------------------------------------------------
//

/// Renders the one-line status, or `None` when the line should be hidden.
///
/// Everything shown comes from the last response; nothing is computed here.
///
/// The ordering matters. A failure is shown *before* the eligibility check,
/// because the failures worth telling someone about , a bad token, or being in
/// two live matches , all happen before a first report ever succeeds. Gating
/// them behind "we already had a good response" meant the compact line stayed
/// empty in exactly the situation where the player most needs to know something
/// is wrong.
pub fn status_line(status: &IngestStatus) -> Option<String> {
    if status.warn {
        let reason = status
            .last_error
            .as_deref()
            .map(short_reason)
            .unwrap_or_else(|| "failed".to_string());

        return Some(match status.tally {
            // There is a tally to show, so keep the line narrow and leave the
            // reason to expanded mode.
            Some(t) => format!("{}   [!]", tally_text(&t)),
            // Nothing else to show, so the reason IS the line.
            None => format!("Automark [!] {reason}"),
        });
    }

    if !status.eligible {
        return None;
    }

    Some(match status.tally {
        Some(t) => tally_text(&t),
        // Accepted, but the server sent no tally. Still worth confirming the
        // round trip is working.
        None => "Automark connected".to_string(),
    })
}

/// ASCII only. The overlay's embedded font covers Latin characters, so a warning
/// sign or an em dash renders as a `?` box, and the reassurance the line exists
/// for turns into a puzzle.
fn tally_text(t: &Tally) -> String {
    // A dash whenever there is no percentage to state: before the first shot the
    // server sends null, and nothing has missed yet, so `0%` would be a lie.
    let acc = match t.accuracy {
        Some(pct) if t.shots > 0 => format!("{pct}%"),
        _ => "-".to_string(),
    };

    format!(
        "Hit {}   Miss {}   Total {}   Acc {}",
        t.hits, t.misses, t.shots, acc
    )
}

/// Turns a wire error into something readable at a glance mid-fight.
///
/// The raw code stays in expanded mode, which is where someone actually
/// debugging will look.
fn short_reason(error: &str) -> String {
    match error {
        // The one that is genuinely confusing without an explanation: leaving a
        // match does not end it, so a stale room still counts as live.
        "ambiguous_match" => "in 2 live matches".to_string(),
        "missing_token" | "unknown_token" => "bad token".to_string(),
        "no_opponents" => "no opponents".to_string(),
        e if e.starts_with("transport") => "no connection".to_string(),
        e if e.starts_with("HTTP") || e.starts_with("read body") => "server error".to_string(),
        // Anything unrecognised passes through, clipped so it cannot stretch
        // the panel across the screen.
        e => e.chars().take(24).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_first_seen_and_never_unkills() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let mut seen = BTreeMap::new();

        assert!(merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![1, 2],
                observed_at: t0,
            }
        ));
        assert!(!merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![1],
                observed_at: t1,
            }
        ));
        assert_eq!(seen[&1], t0);
        assert_eq!(seen[&2], t0);
    }

    fn settings(url: &str, token: &str) -> Option<IngestSettings> {
        IngestSettings::from_config(Some(&Ingest {
            url: Some(url.to_string()),
            token: Some(token.to_string()),
            interval_ms: None,
            heartbeat_s: None,
        }))
    }

    #[test]
    fn disabled_without_url_or_token() {
        assert!(IngestSettings::from_config(None).is_none());
        assert!(settings("", "").is_none());
        assert!(settings("https://example.test/f", "").is_none());
        assert!(settings("", "ab").is_none());
        // Whitespace-only is empty too.
        assert!(settings("   ", "   ").is_none());
    }

    #[test]
    fn enabled_with_both() {
        let s = settings("https://example.test/f", &"a".repeat(48)).unwrap();
        assert_eq!(s.url, "https://example.test/f");
        assert_eq!(s.interval, Duration::from_millis(1_000));
        assert_eq!(s.heartbeat, Duration::from_secs(60));
    }

    #[test]
    fn odd_token_still_enables() {
        // The server is the authority on validity; a short token must not
        // silently disable reporting.
        assert!(settings("https://example.test/f", "not-hex").is_some());
    }

    #[test]
    fn intervals_have_floors() {
        let s = IngestSettings::from_config(Some(&Ingest {
            url: Some("https://example.test/f".into()),
            token: Some("a".repeat(48)),
            interval_ms: Some(0),
            heartbeat_s: Some(0),
        }))
        .unwrap();
        assert_eq!(s.interval, Duration::from_millis(100));
        assert_eq!(s.heartbeat, Duration::from_secs(5));
    }

    #[test]
    fn settings_debug_redacts_endpoint_and_token() {
        let endpoint = "https://private.example.test/report";
        let token = "secret-player-token";
        let settings = settings(endpoint, token).unwrap();

        let debug = format!("{settings:?}");

        assert!(!debug.contains(endpoint));
        assert!(!debug.contains(token));
    }

    #[test]
    fn configured_secrets_are_removed_from_reporter_errors() {
        let endpoint = "https://private.example.test/report";
        let token = "secret-player-token";
        let settings = settings(endpoint, token).unwrap();

        let safe = redact_config_secrets(
            &format!("request to {endpoint} included {token}"),
            &settings,
        );

        assert!(!safe.contains(endpoint));
        assert!(!safe.contains(token));
        assert!(safe.contains("[redacted endpoint]"));
        assert!(safe.contains("[redacted token]"));
    }

    #[test]
    fn reporter_shutdown_releases_sender_without_touching_status() {
        let status = create_status();
        let stop = Arc::new(AtomicBool::new(false));
        let sender = start_reporter(
            settings("https://unused.example.test/report", "secret-player-token"),
            status.clone(),
            stop.clone(),
        )
        .unwrap();

        stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let observation = MonitorObservation {
                active_boss_flags: Vec::new(),
                observed_at: SystemTime::UNIX_EPOCH,
            };
            if sender.send(observation).is_err() {
                break;
            }
            assert!(Instant::now() < deadline, "reporter did not shut down");
            thread::yield_now();
        }

        let status = status.read().expect("reporter must not poison status");
        assert!(!status.eligible);
        assert!(!status.warn);
        assert!(status.last_error.is_none());
    }

    #[test]
    fn body_carries_full_set_sorted_with_timestamps() {
        let mut seen = BTreeMap::new();
        seen.insert(
            31150800,
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_000_020),
        );
        seen.insert(
            1042360800,
            SystemTime::UNIX_EPOCH + Duration::from_millis(1_700_000_100_140),
        );

        let body = build_body("tok", &seen).unwrap();
        assert_eq!(
            body,
            r#"{"token":"tok","kills":[{"flag":31150800,"at":"2023-11-14T22:13:20.020Z"},{"flag":1042360800,"at":"2023-11-14T22:15:00.140Z"}]}"#
        );
    }

    #[test]
    fn merge_is_insert_only_and_keeps_first_timestamp() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let t1 = SystemTime::UNIX_EPOCH + Duration::from_secs(200);
        let mut seen = BTreeMap::new();

        assert!(merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![1, 2],
                observed_at: t0
            }
        ));
        // Same flags again: nothing new, timestamps unchanged.
        assert!(!merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![1, 2],
                observed_at: t1
            }
        ));
        assert_eq!(seen[&1], t0);

        // A save reload shrinks the observed set; that is never an un-kill.
        assert!(!merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![],
                observed_at: t1
            }
        ));
        assert_eq!(seen.len(), 2);

        // A genuinely new flag reports as new.
        assert!(merge(
            &mut seen,
            MonitorObservation {
                active_boss_flags: vec![2, 3],
                observed_at: t1
            }
        ));
        assert_eq!(seen[&3], t1);
    }

    /// Neither of these is a failure: one is idling, the other is a lobby kill
    /// reporting does not apply to. Both hide the line rather than warn.
    #[test]
    fn ineligible_replies_are_not_failures() {
        for e in ["not_in_match", "unsupported_square_set"] {
            assert!(
                matches!(classify_rejection(e.into()), Attempt::Ineligible(_)),
                "{e} should be ineligible, not a failure"
            );
        }
    }

    #[test]
    fn every_other_rejection_is_permanent() {
        for e in [
            "missing_token",
            "unknown_token",
            // Fixable by the player, so it must stay visible rather than hide.
            "ambiguous_match",
            // An error this build has never heard of must not be retried.
            "something_added_server_side_later",
        ] {
            assert!(
                matches!(classify_rejection(e.into()), Attempt::Rejected(_)),
                "{e} should be permanent, not retried"
            );
        }
    }

    /// The overlay must leave the screen in a lobby that does not support kill
    /// reporting , including when a previous match left a tally behind.
    #[test]
    fn line_hides_in_an_unsupported_lobby() {
        // Mid-match in a bosses room: the line is up.
        let mut s = IngestStatus {
            eligible: true,
            tally: Some(Tally {
                hits: 8,
                misses: 4,
                shots: 12,
                accuracy: Some(67),
            }),
            ..Default::default()
        };
        assert!(status_line(&s).is_some());

        // Then an unsupported lobby, applying what the Ineligible arm writes.
        s = IngestStatus {
            eligible: false,
            tally: None,
            warn: false,
            last_error: None,
            kills_tracked: s.kills_tracked,
        };
        assert_eq!(
            status_line(&s),
            None,
            "no line at all in an unsupported lobby"
        );
        assert!(
            !s.warn,
            "an unsupported lobby is not a failure to warn about"
        );
        assert!(
            s.last_error.is_none(),
            "and nothing to explain in expanded mode"
        );
    }

    #[test]
    fn status_line_hidden_until_something_is_known() {
        // Nothing has come back yet, and nothing is wrong: stay off screen.
        assert_eq!(status_line(&IngestStatus::default()), None);
    }

    /// The state that used to render nothing at all in compact mode.
    ///
    /// `ambiguous_match` arrives before any successful report, so `eligible` is
    /// still false and there is no tally. Gating the line on either of those
    /// left the player with a silent overlay and no idea their kills were being
    /// refused.
    #[test]
    fn status_line_reports_a_failure_that_precedes_any_success() {
        let s = IngestStatus {
            eligible: false,
            tally: None,
            warn: true,
            last_error: Some("ambiguous_match".into()),
            kills_tracked: 3,
        };
        let line = status_line(&s).expect("a pre-success failure must still show");
        assert_eq!(line, "Automark [!] in 2 live matches");
        assert!(line.is_ascii(), "must render in the embedded font: {line}");
    }

    #[test]
    fn status_line_explains_the_common_failures() {
        for (wire, shown) in [
            ("ambiguous_match", "in 2 live matches"),
            ("unknown_token", "bad token"),
            ("missing_token", "bad token"),
            ("no_opponents", "no opponents"),
            ("transport: dns error whatever", "no connection"),
            ("HTTP 503", "server error"),
        ] {
            let s = IngestStatus {
                warn: true,
                last_error: Some(wire.into()),
                ..Default::default()
            };
            assert_eq!(status_line(&s).unwrap(), format!("Automark [!] {shown}"));
        }
    }

    #[test]
    fn status_line_clips_an_unrecognised_reason() {
        let s = IngestStatus {
            warn: true,
            last_error: Some("some_enormous_reason_nobody_has_seen_before_at_all".into()),
            ..Default::default()
        };
        let line = status_line(&s).unwrap();
        assert!(
            line.len() <= "Automark [!] ".len() + 24,
            "panel would stretch: {line}"
        );
    }

    /// The exact body the endpoint sends at the start of every match, before
    /// the player has taken a shot. `accuracy` is an explicit `null`, which a
    /// defaulted `i32` cannot deserialise , this used to sink the whole response
    /// and turn a perfectly successful report into a retry storm and a warning.
    #[test]
    fn parses_the_match_start_tally_with_null_accuracy() {
        let raw = r#"{"ok":true,"fired":[],"skipped":[],
            "tally":{"hits":0,"misses":0,"shots":0,"accuracy":null}}"#;
        let r: IngestResponse = serde_json::from_str(raw).expect("null accuracy must parse");

        assert!(r.ok);
        let t = r.tally.expect("tally should survive a null accuracy");
        assert_eq!((t.hits, t.misses, t.shots, t.accuracy), (0, 0, 0, None));

        // And it renders as a dash, not as a failure and not as 0%.
        let s = IngestStatus {
            eligible: true,
            tally: r.tally,
            ..Default::default()
        };
        assert_eq!(status_line(&s).unwrap(), "Hit 0   Miss 0   Total 0   Acc -");
    }

    /// A tally shape we do not understand must cost us the tally, never the
    /// report: the shots have already been fired by the time we read it.
    #[test]
    fn an_unreadable_tally_does_not_sink_the_response() {
        // `[]` is deliberately absent: serde reads a struct from a sequence, so
        // an empty array is a legitimately zeroed tally rather than a broken one.
        for weird in [r#""nonsense""#, "42", "null", r#"{"hits":"lots"}"#] {
            let raw = format!(r#"{{"ok":true,"fired":[],"skipped":[],"tally":{weird}}}"#);
            let r: IngestResponse =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{weird} broke parsing: {e}"));
            assert!(r.ok, "{weird} should still be a successful report");
            assert!(r.tally.is_none(), "{weird} should degrade to no tally");
        }
    }

    #[test]
    fn status_line_confirms_a_round_trip_with_no_tally() {
        // Accepted, but the server sent no tally. Silence would be
        // indistinguishable from a broken setup.
        let s = IngestStatus {
            eligible: true,
            tally: None,
            ..Default::default()
        };
        assert_eq!(status_line(&s).unwrap(), "Automark connected");
    }

    #[test]
    fn status_line_renders_response() {
        let s = IngestStatus {
            eligible: true,
            tally: Some(Tally {
                hits: 8,
                misses: 4,
                shots: 12,
                accuracy: Some(67),
            }),
            ..Default::default()
        };
        assert_eq!(
            status_line(&s).unwrap(),
            "Hit 8   Miss 4   Total 12   Acc 67%"
        );
    }

    #[test]
    fn status_line_dashes_accuracy_before_first_shot() {
        let s = IngestStatus {
            eligible: true,
            tally: Some(Tally::default()),
            ..Default::default()
        };
        assert_eq!(status_line(&s).unwrap(), "Hit 0   Miss 0   Total 0   Acc -");
    }

    #[test]
    fn status_line_warns_when_last_send_failed() {
        let s = IngestStatus {
            eligible: true,
            tally: Some(Tally {
                hits: 8,
                misses: 4,
                shots: 12,
                accuracy: Some(67),
            }),
            warn: true,
            ..Default::default()
        };
        let line = status_line(&s).unwrap();
        assert!(line.ends_with("[!]"), "{line}");
        // Every character must exist in the Latin-only embedded font.
        assert!(line.is_ascii(), "tally line must stay ASCII: {line}");
    }

    #[test]
    fn parses_documented_success_response() {
        let raw = r#"{"ok":true,
            "fired":[{"flag":1042360800,"cell":37,"result":"hit"}],
            "skipped":[{"flag":31150800,"reason":"already_fired"}],
            "tally":{"hits":8,"misses":4,"shots":12,"accuracy":67}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();
        assert!(r.ok);
        assert_eq!(r.fired.len(), 1);
        assert_eq!(r.fired[0].cell, Some(37));
        assert_eq!(r.tally.unwrap().accuracy, Some(67));
    }

    #[test]
    fn parses_documented_error_response() {
        let r: IngestResponse =
            serde_json::from_str(r#"{"ok":false,"error":"not_in_match"}"#).unwrap();
        assert!(!r.ok);
        assert_eq!(r.error.as_deref(), Some("not_in_match"));
        assert!(r.tally.is_none());
        assert!(r.fired.is_empty());
    }

    /// Captured verbatim from the live endpoint. Sending the whole boss list
    /// means most flags come back `not_a_square` or `not_on_this_board`, and
    /// resending the kill set means the rest come back `already_fired`; all are
    /// the protocol working, so none of them is logged.
    #[test]
    fn routine_skips_are_not_noteworthy() {
        let raw = r#"{"ok":true,"fired":[],"skipped":[
            {"flag":31150800,"reason":"already_fired"},
            {"flag":18000850,"reason":"not_a_square"},
            {"flag":1042360800,"reason":"not_on_this_board"}],
            "tally":{"hits":0,"misses":1,"shots":1,"accuracy":0}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();

        assert!(r.ok);
        for s in &r.skipped {
            assert!(
                ROUTINE_SKIPS.contains(&s.reason.as_deref().unwrap()),
                "{:?} should be treated as routine",
                s.reason
            );
        }

        // A kill that did not land must stay visible.
        for noisy in ["no_opponents", "insert_failed"] {
            assert!(
                !ROUTINE_SKIPS.contains(&noisy),
                "{noisy} means a kill was lost and must be logged"
            );
        }

        // One shot taken, so accuracy renders as a number rather than a dash.
        let status = IngestStatus {
            eligible: true,
            tally: r.tally,
            ..Default::default()
        };
        assert_eq!(
            status_line(&status).unwrap(),
            "Hit 0   Miss 1   Total 1   Acc 0%"
        );
    }

    /// A live fire, also captured verbatim from the endpoint.
    #[test]
    fn parses_live_fired_entry() {
        let raw = r#"{"ok":true,"fired":[{"flag":31150800,"cell":28,"result":"miss"}],
            "skipped":[{"flag":1042360800,"reason":"not_on_this_board"}],
            "tally":{"hits":0,"misses":1,"shots":1,"accuracy":0}}"#;
        let r: IngestResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(r.fired.len(), 1);
        assert_eq!(r.fired[0].flag, 31150800);
        assert_eq!(r.fired[0].cell, Some(28));
        assert_eq!(r.fired[0].result.as_deref(), Some("miss"));
    }

    /// Exercises the real network path , ureq, TLS, serialisation and response
    /// classification , end to end. Ignored by default because it needs a
    /// network and a token; run it with:
    ///
    /// ```text
    /// $env:ER_OVERLAY_INGEST_URL   = "https://.../auto-fire"
    /// $env:ER_OVERLAY_INGEST_TOKEN = "<48 hex chars>"
    /// cargo test --lib -- --ignored --nocapture live_round_trip
    /// ```
    ///
    /// Safe to run mid-match: an empty kill set cannot fire anything.
    #[test]
    #[ignore = "needs ER_OVERLAY_INGEST_URL, ER_OVERLAY_INGEST_TOKEN and network"]
    fn live_round_trip() {
        let (Ok(url), Ok(token)) = (
            std::env::var("ER_OVERLAY_INGEST_URL"),
            std::env::var("ER_OVERLAY_INGEST_TOKEN"),
        ) else {
            panic!("set ER_OVERLAY_INGEST_URL and ER_OVERLAY_INGEST_TOKEN");
        };

        let agent = build_agent();
        let empty = BTreeMap::new();

        match send_once(&agent, &url, &build_body(&token, &empty).unwrap()) {
            Attempt::Accepted(r) => println!("accepted, tally = {:?}", r.tally),
            Attempt::Ineligible(r) => println!("ineligible: {r} (expected while idle)"),
            Attempt::Rejected(e) => panic!("unexpectedly rejected: {e}"),
            Attempt::Retryable(e) => panic!("transport failure: {e}"),
        }

        // A well-formed but unknown token must come back as a permanent
        // rejection rather than something we would sit and retry.
        let bogus = build_body(&"0".repeat(48), &empty).unwrap();
        match send_once(&agent, &url, &bogus) {
            Attempt::Rejected(e) => assert_eq!(e, "unknown_token"),
            Attempt::Accepted(_) => panic!("an unknown token was accepted"),
            Attempt::Ineligible(r) => panic!("an unknown token was treated as ineligible: {r}"),
            Attempt::Retryable(e) => panic!("expected a permanent rejection, got retryable: {e}"),
        }
    }

    #[test]
    fn tolerates_missing_and_null_fields() {
        // A trimmed-down or partially-null reply must not fail parsing.
        let r: IngestResponse =
            serde_json::from_str(r#"{"ok":true,"tally":{"hits":3},"fired":[{"flag":1}]}"#).unwrap();
        let t = r.tally.unwrap();
        // A missing accuracy is as absent as a null one.
        assert_eq!((t.hits, t.misses, t.shots, t.accuracy), (3, 0, 0, None));
        assert_eq!(r.fired[0].cell, None);

        let r: IngestResponse = serde_json::from_str(r#"{}"#).unwrap();
        assert!(!r.ok);
    }
}
