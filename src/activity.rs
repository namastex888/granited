//! The jobs this node ran, per 15-minute UTC bucket, registered model and capability, with the work
//! units each reply reported. Counts only: no prompt, output or audio is kept. The request path
//! updates memory under a short lock; a background flush writes `state_dir/activity.json`.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub const BUCKET_MS: u64 = 15 * 60 * 1000;
pub const RETENTION_DAYS: u64 = 35;
pub const RETENTION_MS: u64 = RETENTION_DAYS * 24 * 60 * 60 * 1000;
pub const FLUSH_SECONDS: u64 = 30;
/// Buckets per host in one reply; past this the reply says `truncated`.
pub const MAX_BUCKETS: usize = 10_000;
const CORRUPT_COPIES: usize = 3;
const MAX_SPANS: usize = 1_000;
const FILE: &str = "activity.json";
/// Units kept in thousandths, so fractional seconds survive integer counters.
const MILLI_UNITS: [&str; 1] = ["audio_seconds"];

pub enum Outcome {
    /// The units the reply reported, or `None` when they could not be read.
    Succeeded(Option<Vec<(&'static str, f64)>>),
    /// The backend ran the request and failed it.
    Failed,
    /// The backend refused the request as malformed (a 4xx other than 408 and 429).
    Rejected,
    Cancelled,
}

#[derive(Clone, Default, PartialEq, Debug)]
struct Counts {
    succeeded: u64,
    failed: u64,
    rejected: u64,
    cancelled: u64,
    units: BTreeMap<String, u64>,
    units_unavailable: u64,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Debug)]
struct Span {
    start_unix_ms: u64,
    end_unix_ms: u64,
}

type Key = (u64, String, String);

struct State {
    /// Earlier processes' lifetimes, as last flushed.
    past: Vec<Span>,
    started: u64,
    buckets: BTreeMap<Key, Counts>,
    dirty: bool,
    /// When this process last wrote its coverage end.
    written: u64,
}

pub struct Activity {
    path: PathBuf,
    state: Mutex<State>,
    /// Held for a whole flush, so two flushes never write out of order.
    flushing: tokio::sync::Mutex<()>,
}

/// The 13 capability families (`contracts::capability`). Families with no runnable model are valid
/// keys too; they count jobs with no units.
pub const FAMILIES: [&str; 13] = [
    "chat",
    "adapters",
    "embeddings",
    "retrieval",
    "forecast",
    "anomalies",
    "rerank",
    "documents",
    "geospatial",
    "guardian",
    "vision",
    "transcription",
    "speech_synthesis",
];

/// What a reply carried that units are made of; filled from the parsed body only.
#[derive(Default)]
pub struct Reply {
    pub usage: Option<Value>,
    /// Entries in `answers{}` (`/v1/systemone`).
    pub answers: Option<u64>,
    /// Length of `data[]` (embeddings).
    pub data: Option<u64>,
    pub ranked_ids: Option<u64>,
    pub horizon: Option<u64>,
    pub audio_duration_seconds: Option<f64>,
    /// Seconds of relayed WAV audio (speech synthesis).
    pub audio_seconds: Option<f64>,
    /// A streamed reply: its usage never carries a reasoning share.
    pub streamed: bool,
}

/// The capabilities whose replies report token usage. A reply from any other one carries no
/// tokens by design, so it is a completed job, never a request "without usage".
pub fn carries_tokens(capability: &str) -> bool {
    matches!(capability, "chat" | "vision" | "adapters" | "embeddings")
}

pub fn now_ms() -> u64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    now.unwrap_or_default().as_millis() as u64
}

/// The bucket holding `ms`: `[start, start + BUCKET_MS)`. Every UTC offset in use is a multiple of
/// 15 minutes, so a local day is an exact union of buckets.
pub fn bucket_start(ms: u64) -> u64 {
    ms - ms % BUCKET_MS
}

/// The units a capability's reply carries. All of them or none: a missing field makes the job's
/// units unavailable, never a zero. Families without defined units get an empty set.
pub fn units(capability: &str, reply: &Reply) -> Option<Vec<(&'static str, f64)>> {
    let tokens = |key: &str| {
        reply
            .usage
            .as_ref()
            .and_then(|u| u.pointer(key))
            .and_then(Value::as_u64)
            .map(|v| v as f64)
    };
    let count = |v: Option<u64>| v.map(|v| v as f64);
    let seconds = |v: Option<f64>| v.filter(|s| s.is_finite() && *s >= 0.0);
    let all = |pairs: Vec<(&'static str, Option<f64>)>| {
        pairs
            .into_iter()
            .map(|(name, value)| value.map(|v| (name, v)))
            .collect::<Option<Vec<_>>>()
    };
    match capability {
        "chat" => {
            // output_tokens is everything generated, thinking included. reasoning_tokens is a
            // share of it, recorded only when a buffered reply reports it, and never added to it.
            let mut units = all(vec![
                ("prompt_tokens", tokens("/prompt_tokens")),
                ("output_tokens", tokens("/completion_tokens")),
            ])?;
            if let Some(reasoning) =
                tokens("/completion_tokens_details/reasoning_tokens").filter(|_| !reply.streamed)
            {
                units.push(("reasoning_tokens", reasoning));
            }
            Some(units)
        }
        "vision" => all(vec![
            ("prompt_tokens", tokens("/prompt_tokens")),
            ("output_tokens", tokens("/completion_tokens")),
        ]),
        "adapters" => all(vec![
            ("questions_answered", count(reply.answers)),
            ("input_tokens", tokens("/input_tokens")),
        ]),
        "embeddings" => all(vec![
            ("vectors", count(reply.data)),
            ("prompt_tokens", tokens("/prompt_tokens")),
        ]),
        "retrieval" => all(vec![("documents_ranked", count(reply.ranked_ids))]),
        "forecast" => all(vec![("forecast_steps", count(reply.horizon))]),
        "transcription" => all(vec![(
            "audio_seconds",
            seconds(reply.audio_duration_seconds),
        )]),
        "speech_synthesis" => all(vec![("audio_seconds", seconds(reply.audio_seconds))]),
        "anomalies" | "rerank" | "documents" | "geospatial" | "guardian" => Some(Vec::new()),
        _ => None,
    }
}

/// Why a relayed WAV gave no seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WavFault {
    /// Not a RIFF/WAVE body at all; only then may `Server-Timing` stand in.
    NotRiff,
    /// Unsupported, malformed, or cut off before its `data` chunk.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Phase {
    Riff,
    Chunk,
    Fmt(usize),
    Skip(u64),
    /// Payload bytes of the data chunk still to come; `None` when its size is unknown (streamed).
    Data(Option<u64>),
    Done,
    Fault(WavFault),
}

/// Walks a relayed WAV's RIFF chunks as its bytes pass, holding only chunk headers and the `fmt `
/// body. Audio is the data chunk's payload; LIST and other chunks, padding and trailing bytes are not.
pub struct Wav {
    phase: Phase,
    pending: Vec<u8>,
    byte_rate: Option<u64>,
    data_bytes: u64,
}

impl Default for Wav {
    fn default() -> Self {
        Wav {
            phase: Phase::Riff,
            pending: Vec::new(),
            byte_rate: None,
            data_bytes: 0,
        }
    }
}

impl Wav {
    pub fn feed(&mut self, mut input: &[u8]) {
        loop {
            match self.phase {
                Phase::Done | Phase::Fault(_) => return,
                Phase::Data(None) => {
                    self.data_bytes = self.data_bytes.saturating_add(input.len() as u64);
                    return;
                }
                Phase::Data(Some(left)) | Phase::Skip(left) => {
                    if input.is_empty() {
                        return;
                    }
                    let take = left.min(input.len() as u64);
                    input = &input[take as usize..];
                    let rest = left - take;
                    self.phase = match self.phase {
                        Phase::Data(_) => {
                            self.data_bytes += take;
                            if rest == 0 {
                                Phase::Done
                            } else {
                                Phase::Data(Some(rest))
                            }
                        }
                        _ if rest == 0 => Phase::Chunk,
                        _ => Phase::Skip(rest),
                    };
                }
                Phase::Riff | Phase::Chunk | Phase::Fmt(_) => {
                    let want = match self.phase {
                        Phase::Riff => 12,
                        Phase::Chunk => 8,
                        Phase::Fmt(n) => n,
                        _ => unreachable!(),
                    };
                    let take = (want - self.pending.len()).min(input.len());
                    self.pending.extend_from_slice(&input[..take]);
                    input = &input[take..];
                    if self.pending.len() < want {
                        return;
                    }
                    let block = std::mem::take(&mut self.pending);
                    self.phase = self.parse(&block);
                }
            }
        }
    }

    fn parse(&mut self, b: &[u8]) -> Phase {
        let u16_at = |i: usize| u64::from(u16::from_le_bytes([b[i], b[i + 1]]));
        let u32_at = |i: usize| u64::from(u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]));
        match self.phase {
            Phase::Riff if &b[0..4] == b"RIFF" && &b[8..12] == b"WAVE" => Phase::Chunk,
            Phase::Riff => Phase::Fault(WavFault::NotRiff),
            Phase::Chunk => {
                let size = u32_at(4);
                // Chunks are word-aligned: an odd-sized one is followed by a pad byte.
                let padded = size + size % 2;
                match &b[0..4] {
                    b"fmt " if (16..=64).contains(&size) => Phase::Fmt(padded as usize),
                    b"fmt " => Phase::Fault(WavFault::Invalid),
                    b"data" if self.byte_rate.is_none() => Phase::Fault(WavFault::Invalid),
                    b"data" if size == 0 || size == 0xFFFF_FFFF => Phase::Data(None),
                    b"data" => Phase::Data(Some(size)),
                    _ if padded == 0 => Phase::Chunk,
                    _ => Phase::Skip(padded),
                }
            }
            Phase::Fmt(_) => {
                let (tag, channels, rate) = (u16_at(0), u16_at(2), u32_at(4));
                let (byte_rate, align, bits) = (u32_at(8), u16_at(12), u16_at(14));
                let frame = channels * bits / 8;
                // PCM, IEEE float or extensible, with a byte rate and block alignment that agree.
                let valid = matches!(tag, 1 | 3 | 0xFFFE)
                    && channels > 0
                    && rate > 0
                    && bits > 0
                    && bits % 8 == 0
                    && align == frame
                    && byte_rate == rate * frame;
                if valid {
                    self.byte_rate = Some(byte_rate);
                    Phase::Chunk
                } else {
                    Phase::Fault(WavFault::Invalid)
                }
            }
            phase => phase,
        }
    }

    /// Seconds of audio relayed inside the data chunk.
    pub fn seconds(&self) -> Result<f64, WavFault> {
        match (self.phase, self.byte_rate) {
            (Phase::Data(_) | Phase::Done, Some(rate)) => Ok(self.data_bytes as f64 / rate as f64),
            (Phase::Fault(fault), _) => Err(fault),
            (Phase::Riff, _) if self.pending.is_empty() => Err(WavFault::NotRiff),
            _ => Err(WavFault::Invalid),
        }
    }
}

/// `audio;dur=<ms>` from a `Server-Timing` header, in seconds.
pub fn timing_seconds(server_timing: &str) -> Option<f64> {
    server_timing.split(',').find_map(|metric| {
        let mut parts = metric.trim().split(';');
        (parts.next()? == "audio")
            .then(|| parts.find_map(|p| p.trim().strip_prefix("dur=")?.parse::<f64>().ok()))
            .flatten()
            .map(|ms| ms / 1000.0)
    })
}

impl Counts {
    fn json(&self, key: &Key) -> Value {
        let units: serde_json::Map<String, Value> = self
            .units
            .iter()
            .map(|(name, v)| {
                let value = if MILLI_UNITS.contains(&name.as_str()) {
                    json!(*v as f64 / 1000.0)
                } else {
                    json!(v)
                };
                (name.clone(), value)
            })
            .collect();
        json!({"start_unix_ms":key.0,"model":key.1,"capability":key.2,"succeeded":self.succeeded,
            "failed":self.failed,"rejected":self.rejected,"cancelled":self.cancelled,"units":units,
            "units_unavailable":self.units_unavailable})
    }

    fn parse(value: &Value) -> Option<(Key, Counts)> {
        let n = |k: &str| value.get(k)?.as_u64();
        let key = (
            n("start_unix_ms")?,
            value["model"].as_str()?.to_owned(),
            value["capability"].as_str()?.to_owned(),
        );
        let mut units = BTreeMap::new();
        for (name, v) in value["units"].as_object()? {
            let v = v.as_f64().filter(|v| v.is_finite() && *v >= 0.0)?;
            let scale = if MILLI_UNITS.contains(&name.as_str()) {
                1000.0
            } else {
                1.0
            };
            units.insert(name.clone(), (v * scale).round() as u64);
        }
        let counts = Counts {
            succeeded: n("succeeded")?,
            failed: n("failed")?,
            // Files written before rejected was counted have no such key.
            rejected: match value.get("rejected") {
                None => 0,
                Some(_) => n("rejected")?,
            },
            cancelled: n("cancelled")?,
            units,
            units_unavailable: n("units_unavailable")?,
        };
        Some((key, counts))
    }
}

#[derive(Serialize, Deserialize)]
struct Disk {
    version: u32,
    coverage: Vec<Span>,
    buckets: Vec<Value>,
}

/// What a flush writes, taken under the state lock and serialized outside it.
struct Snapshot {
    coverage: Vec<Span>,
    buckets: BTreeMap<Key, Counts>,
}

impl Snapshot {
    fn bytes(&self) -> Vec<u8> {
        let disk = Disk {
            version: 1,
            coverage: self.coverage.clone(),
            buckets: self.buckets.iter().map(|(k, c)| c.json(k)).collect(),
        };
        serde_json::to_vec(&disk).unwrap_or_default()
    }
}

fn parse(bytes: &[u8]) -> Option<(Vec<Span>, BTreeMap<Key, Counts>)> {
    let disk: Disk = serde_json::from_slice(bytes).ok()?;
    let buckets = disk
        .buckets
        .iter()
        .map(Counts::parse)
        .collect::<Option<BTreeMap<_, _>>>()?;
    (disk.version == 1).then_some((disk.coverage, buckets))
}

/// Keeps a corrupt file as `activity.json.corrupt-<ms>`, and only the newest few of those.
fn preserve_corrupt(path: &Path, now: u64) {
    let Some(dir) = path.parent() else { return };
    let kept = dir.join(format!("{FILE}.corrupt-{now}"));
    if let Err(e) = std::fs::rename(path, &kept) {
        tracing::warn!(error = %e, "could not preserve the corrupt activity file");
        return;
    }
    tracing::warn!(kept = %kept.display(), "activity file was corrupt; kept it and started fresh");
    let prefix = format!("{FILE}.corrupt-");
    let mut copies: Vec<(u64, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            Some((name.strip_prefix(&prefix)?.parse().ok()?, e.path()))
        })
        .collect();
    copies.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, old) in copies.into_iter().skip(CORRUPT_COPIES) {
        let _ = std::fs::remove_file(old);
    }
}

/// Temp file, fsync, rename: a crash leaves the old file or the new one, never half of one.
fn write_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let temp = path.with_file_name(format!("{FILE}.tmp"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temp)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&temp, path)?;
    if let Some(dir) = path.parent() {
        std::fs::File::open(dir)?.sync_all()?;
    }
    Ok(())
}

impl Activity {
    /// Loads `state_dir/activity.json` (a name apart from the `owned` reaping ledger). A corrupt
    /// file is preserved and the node starts fresh; the lost history shows as a coverage gap.
    pub fn open(state_dir: &str) -> Activity {
        Activity::open_at(Path::new(state_dir).join(FILE), now_ms())
    }

    fn open_at(path: PathBuf, now: u64) -> Activity {
        let (past, buckets) = match std::fs::read(&path) {
            Ok(bytes) => parse(&bytes).unwrap_or_else(|| {
                preserve_corrupt(&path, now);
                Default::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Default::default(),
            Err(e) => {
                tracing::warn!(error = %e, "activity file unreadable");
                preserve_corrupt(&path, now);
                Default::default()
            }
        };
        let mut state = State {
            past,
            started: now,
            buckets,
            // The first flush records this process's start.
            dirty: true,
            written: now,
        };
        state.prune(now);
        Activity {
            path,
            state: Mutex::new(state),
            flushing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn record(&self, model: &str, capability: &str, outcome: Outcome) {
        self.record_at(model, capability, outcome, now_ms());
    }

    /// A job lands in the bucket of the moment its outcome was known (a stream that crosses a
    /// boundary counts where it ended). Memory only; no IO under this lock.
    fn record_at(&self, model: &str, capability: &str, outcome: Outcome, now: u64) {
        if !FAMILIES.contains(&capability) {
            return;
        }
        let mut state = self.state.lock().unwrap();
        let key = (bucket_start(now), model.to_owned(), capability.to_owned());
        let counts = state.buckets.entry(key).or_default();
        match outcome {
            Outcome::Succeeded(Some(units)) => {
                counts.succeeded = counts.succeeded.saturating_add(1);
                for (name, value) in units {
                    let scale = if MILLI_UNITS.contains(&name) {
                        1000.0
                    } else {
                        1.0
                    };
                    let add = (value * scale).round() as u64;
                    let total = counts.units.entry(name.to_owned()).or_default();
                    *total = total.saturating_add(add);
                }
            }
            Outcome::Succeeded(None) => {
                counts.succeeded = counts.succeeded.saturating_add(1);
                counts.units_unavailable = counts.units_unavailable.saturating_add(1);
            }
            Outcome::Failed => counts.failed = counts.failed.saturating_add(1),
            Outcome::Rejected => counts.rejected = counts.rejected.saturating_add(1),
            Outcome::Cancelled => counts.cancelled = counts.cancelled.saturating_add(1),
        }
        state.dirty = true;
    }

    /// Takes what to write when the counts changed, when this process's coverage end on disk is
    /// a bucket behind (so an idle node writes at most every 15 minutes), or when forced.
    fn snapshot(&self, now: u64, force: bool) -> Option<Snapshot> {
        let mut state = self.state.lock().unwrap();
        state.prune(now);
        if !(force || state.dirty || now >= state.written + BUCKET_MS) {
            return None;
        }
        state.dirty = false;
        state.written = now;
        let mut coverage = state.past.clone();
        coverage.push(Span {
            start_unix_ms: state.started,
            end_unix_ms: now,
        });
        Some(Snapshot {
            coverage,
            buckets: state.buckets.clone(),
        })
    }

    /// Writes the ledger if needed. Returns whether it wrote.
    pub async fn flush(self: &Arc<Self>, force: bool) -> std::io::Result<bool> {
        let _flushing = self.flushing.lock().await;
        let Some(snapshot) = self.snapshot(now_ms(), force) else {
            return Ok(false);
        };
        let path = self.path.clone();
        let written = tokio::task::spawn_blocking(move || write_file(&path, &snapshot.bytes()))
            .await
            .map_err(std::io::Error::other)
            .and_then(|r| r);
        if written.is_err() {
            self.state.lock().unwrap().dirty = true;
        }
        written.map(|_| true)
    }

    /// This host's coverage and buckets overlapping `[since, until)`, oldest first. This process's
    /// coverage runs to `now`.
    pub fn query(&self, since: u64, until: u64) -> Value {
        self.query_at(since, until, now_ms())
    }

    fn query_at(&self, since: u64, until: u64, now: u64) -> Value {
        let state = self.state.lock().unwrap();
        let current = Span {
            start_unix_ms: state.started,
            end_unix_ms: now,
        };
        let coverage: Vec<Value> = state
            .past
            .iter()
            .chain(std::iter::once(&current))
            .filter(|s| s.start_unix_ms < until && s.end_unix_ms >= since)
            .map(|s| {
                json!({"start_unix_ms":s.start_unix_ms.max(since),"end_unix_ms":s.end_unix_ms.min(until)})
            })
            .collect();
        let mut matching = state
            .buckets
            .range((bucket_start(since), String::new(), String::new())..)
            .take_while(|(k, _)| k.0 < until);
        let buckets: Vec<Value> = matching
            .by_ref()
            .take(MAX_BUCKETS)
            .map(|(k, c)| c.json(k))
            .collect();
        let truncated = matching.next().is_some();
        json!({"coverage":coverage,"buckets":buckets,"truncated":truncated})
    }
}

impl State {
    fn prune(&mut self, now: u64) {
        let oldest = bucket_start(now.saturating_sub(RETENTION_MS));
        self.buckets.retain(|k, _| k.0 >= oldest);
        self.past.retain(|s| s.end_unix_ms >= oldest);
        let excess = self.past.len().saturating_sub(MAX_SPANS);
        self.past.drain(..excess);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_792_972_800_000; // 2026-10-26T00:00:00Z, a bucket boundary

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("activity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn chat(tokens: f64) -> Outcome {
        Outcome::Succeeded(Some(vec![
            ("prompt_tokens", tokens),
            ("output_tokens", 1.0),
        ]))
    }

    #[test]
    fn buckets_are_15_minute_utc_half_open_intervals() {
        assert_eq!(T % BUCKET_MS, 0);
        assert_eq!(bucket_start(T), T);
        assert_eq!(bucket_start(T + BUCKET_MS - 1), T);
        assert_eq!(bucket_start(T + BUCKET_MS), T + BUCKET_MS);
        let a = Activity::open_at(temp_dir("bounds").join(FILE), T);
        a.record_at("m", "chat", chat(1.0), T + BUCKET_MS - 1);
        a.record_at("m", "chat", chat(2.0), T + BUCKET_MS);
        let all = a.query_at(T, T + 2 * BUCKET_MS, T + 2 * BUCKET_MS);
        let starts: Vec<_> = all["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["start_unix_ms"].as_u64().unwrap())
            .collect();
        assert_eq!(starts, [T, T + BUCKET_MS]);
        // until is exclusive; a bucket is included when it overlaps [since, until).
        let first = a.query_at(T, T + BUCKET_MS, T + 2 * BUCKET_MS);
        assert_eq!(first["buckets"].as_array().unwrap().len(), 1);
        let second = a.query_at(T + BUCKET_MS + 1, T + 2 * BUCKET_MS, T + 2 * BUCKET_MS);
        assert_eq!(second["buckets"][0]["start_unix_ms"], T + BUCKET_MS);
    }

    #[test]
    fn units_are_all_or_nothing_and_answers_are_counted() {
        let reply = Reply {
            answers: Some(3),
            usage: Some(json!({"input_tokens": 120})),
            ..Default::default()
        };
        assert_eq!(
            units("adapters", &reply).unwrap(),
            [("questions_answered", 3.0), ("input_tokens", 120.0)]
        );
        let no_usage = Reply {
            answers: Some(3),
            ..Default::default()
        };
        assert!(units("adapters", &no_usage).is_none());
        let embed = Reply {
            data: Some(2),
            ..Default::default()
        };
        assert!(units("embeddings", &embed).is_none());
        assert_eq!(units("rerank", &Reply::default()).unwrap(), []);
        let negative = Reply {
            audio_duration_seconds: Some(-1.0),
            ..Default::default()
        };
        assert!(units("transcription", &negative).is_none());
    }

    /// A RIFF chunk, padded to an even length.
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn fmt(rate: u32, channels: u16, bits: u16, byte_rate: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&channels.to_le_bytes());
        body.extend_from_slice(&rate.to_le_bytes());
        body.extend_from_slice(&byte_rate.to_le_bytes());
        body.extend_from_slice(&(channels * bits / 8).to_le_bytes());
        body.extend_from_slice(&bits.to_le_bytes());
        chunk(b"fmt ", &body)
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut out = b"RIFF\xff\xff\xff\xffWAVE".to_vec();
        for c in chunks {
            out.extend_from_slice(c);
        }
        out
    }

    /// Seconds from feeding `bytes` in pieces of every size from 1 to 7.
    fn wav(bytes: &[u8]) -> Result<f64, WavFault> {
        let mut seen = None;
        for cut in 1..8 {
            let mut w = Wav::default();
            for piece in bytes.chunks(cut) {
                w.feed(piece);
            }
            let s = w.seconds();
            assert!(seen.is_none_or(|prev| prev == s), "cut {cut}");
            seen = Some(s);
        }
        seen.unwrap()
    }

    #[test]
    fn wav_seconds_are_the_data_chunk_payload_over_the_byte_rate() {
        let fmt = fmt(24_000, 1, 16, 48_000);
        let pcm = vec![0u8; 72_000];
        // A plain 44-byte header.
        let plain = riff(&[fmt.clone(), chunk(b"data", &pcm)]);
        assert_eq!(plain.len(), 44 + 72_000);
        assert_eq!(wav(&plain), Ok(1.5));
        // A LIST chunk before the data, and trailing bytes after it, are not audio.
        let mut listed = riff(&[
            fmt.clone(),
            chunk(b"LIST", b"INFOISFT\x05\x00\x00\x00test\x00"),
            chunk(b"data", &pcm),
        ]);
        listed.extend_from_slice(&chunk(b"LIST", b"trailing metadata"));
        assert_eq!(wav(&listed), Ok(1.5));
        // An odd-sized chunk is followed by its pad byte.
        let odd = riff(&[fmt.clone(), chunk(b"junk", b"abc"), chunk(b"data", &pcm)]);
        assert_eq!(wav(&odd), Ok(1.5));
        // A streamed reply declares an unknown size: the relayed payload counts.
        let mut streamed = riff(std::slice::from_ref(&fmt));
        streamed.extend_from_slice(b"data\xff\xff\xff\xff");
        streamed.extend_from_slice(&pcm[..24_000]);
        assert_eq!(wav(&streamed), Ok(0.5));
        let mut zero = riff(std::slice::from_ref(&fmt));
        zero.extend_from_slice(b"data\x00\x00\x00\x00");
        zero.extend_from_slice(&pcm[..48_000]);
        assert_eq!(wav(&zero), Ok(1.0));
    }

    #[test]
    fn a_wav_that_cannot_be_trusted_has_no_seconds() {
        let pcm = vec![0u8; 4_800];
        // The byte rate disagrees with rate, channels and bits.
        let mismatch = riff(&[fmt(24_000, 1, 16, 96_000), chunk(b"data", &pcm)]);
        assert_eq!(wav(&mismatch), Err(WavFault::Invalid));
        // Cut off inside the header, or before the data chunk.
        let whole = riff(&[fmt(24_000, 1, 16, 48_000), chunk(b"data", &pcm)]);
        assert_eq!(wav(&whole[..20]), Err(WavFault::Invalid));
        assert_eq!(wav(&whole[..6]), Err(WavFault::Invalid));
        assert_eq!(wav(&whole[..36]), Err(WavFault::Invalid));
        // Data before its format.
        assert_eq!(wav(&riff(&[chunk(b"data", &pcm)])), Err(WavFault::Invalid));
        // Not a WAV at all: only then may Server-Timing stand in.
        assert_eq!(
            wav(b"ID3\x04\x00\x00\x00\x00\x00\x00\x00\x00"),
            Err(WavFault::NotRiff)
        );
        assert_eq!(wav(b""), Err(WavFault::NotRiff));
        assert_eq!(
            timing_seconds("synthesis;dur=812.4, audio;dur=2500.0, rtf;desc=\"0.3\""),
            Some(2.5)
        );
        assert_eq!(timing_seconds("synthesis;dur=812.4"), None);
    }

    #[test]
    fn chat_reasoning_tokens_are_a_share_of_output_tokens_and_only_when_reported() {
        let usage = json!({"prompt_tokens":10,"completion_tokens":40,
            "completion_tokens_details":{"reasoning_tokens":25}});
        let buffered = Reply {
            usage: Some(usage.clone()),
            ..Default::default()
        };
        assert_eq!(
            units("chat", &buffered).unwrap(),
            [
                ("prompt_tokens", 10.0),
                ("output_tokens", 40.0),
                ("reasoning_tokens", 25.0)
            ]
        );
        let streamed = Reply {
            usage: Some(usage),
            streamed: true,
            ..Default::default()
        };
        assert_eq!(
            units("chat", &streamed).unwrap(),
            [("prompt_tokens", 10.0), ("output_tokens", 40.0)]
        );
        let plain = Reply {
            usage: Some(json!({"prompt_tokens":10,"completion_tokens":40})),
            ..Default::default()
        };
        assert_eq!(units("chat", &plain).unwrap().len(), 2);
    }

    #[test]
    fn all_13_families_are_ledger_keys_and_every_route_maps_to_one() {
        let routes = [
            "/v1/chat/completions",
            "/v1/systemone",
            "/v1/embeddings",
            "/v1/forecast",
            "/v1/anomalies",
            "/v1/rerank",
            "/v1/retrieval/rank",
            "/v1/documents/parse",
            "/v1/geospatial/predict",
            "/v1/guardian/evaluate",
            "/v1/vision/extract",
            "/v1/speech/transcribe",
            "/v1/audio/transcriptions",
            "/v1/audio/speech",
        ];
        let mapped: std::collections::BTreeSet<_> = routes
            .iter()
            .map(|r| crate::contracts::capability(r).unwrap())
            .collect();
        assert_eq!(mapped, FAMILIES.iter().copied().collect());
        let a = Activity::open_at(temp_dir("families").join(FILE), T);
        for family in FAMILIES {
            a.record_at(
                "m",
                family,
                Outcome::Succeeded(units(family, &Reply::default())),
                T,
            );
        }
        a.record_at("m", "not-a-family", chat(1.0), T);
        let all = a.query_at(T, T + BUCKET_MS, T + 1);
        let buckets = all["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 13);
        let geo = buckets
            .iter()
            .find(|b| b["capability"] == "geospatial")
            .unwrap();
        assert_eq!(
            (
                geo["succeeded"].clone(),
                geo["units"].clone(),
                geo["units_unavailable"].clone()
            ),
            (json!(1), json!({}), json!(0))
        );
        let chat_row = buckets.iter().find(|b| b["capability"] == "chat").unwrap();
        assert_eq!(chat_row["units_unavailable"], 1);
    }

    #[tokio::test]
    async fn the_ledger_round_trips_and_a_restart_reloads_it() {
        let dir = temp_dir("reload");
        let a = Arc::new(Activity::open_at(dir.join(FILE), T));
        a.record_at("m", "chat", chat(5.0), T + 1);
        a.record_at("m", "chat", Outcome::Failed, T + 2);
        a.record_at("m", "chat", Outcome::Cancelled, T + 3);
        a.record_at("m", "chat", Outcome::Rejected, T + 3);
        a.record_at(
            "s",
            "speech_synthesis",
            Outcome::Succeeded(Some(vec![("audio_seconds", 1.25)])),
            T + 4,
        );
        a.record_at("s", "speech_synthesis", Outcome::Succeeded(None), T + 5);
        assert!(a.flush(true).await.unwrap());
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(dir.join(FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let before = a.query_at(T, T + BUCKET_MS, T + 10);
        let b = Activity::open_at(dir.join(FILE), T + 20);
        let after = b.query_at(T, T + BUCKET_MS, T + 30);
        assert_eq!(before["buckets"], after["buckets"]);
        assert_eq!(
            after["buckets"][0]["units"],
            json!({"output_tokens":1,"prompt_tokens":5})
        );
        assert_eq!(after["buckets"][1]["units"], json!({"audio_seconds":1.25}));
        assert_eq!(after["buckets"][1]["units_unavailable"], 1);
        assert_eq!(
            (
                &after["buckets"][0]["failed"],
                &after["buckets"][0]["rejected"]
            ),
            (&json!(1), &json!(1))
        );
        // The earlier process's span, then this one's.
        let coverage = after["coverage"].as_array().unwrap();
        assert_eq!(coverage.len(), 2);
        assert_eq!(coverage[1]["start_unix_ms"], T + 20);
    }

    #[tokio::test]
    async fn it_writes_only_when_dirty() {
        let dir = temp_dir("dirty");
        let a = Arc::new(Activity::open_at(dir.join(FILE), now_ms()));
        assert!(
            a.flush(false).await.unwrap(),
            "the process start is recorded"
        );
        assert!(!a.flush(false).await.unwrap(), "nothing changed");
        a.record("m", "chat", chat(1.0));
        assert!(a.flush(false).await.unwrap());
        assert!(!a.flush(false).await.unwrap());
        // An idle node still advances its coverage end once a bucket has passed.
        assert!(a.snapshot(now_ms() + BUCKET_MS, false).is_some());
    }

    #[tokio::test]
    async fn a_flush_in_progress_never_blocks_a_request() {
        let a = Arc::new(Activity::open_at(temp_dir("busy").join(FILE), T));
        // A slow write holds this for its whole duration.
        let writing = a.flushing.lock().await;
        let started = std::time::Instant::now();
        a.record_at("m", "chat", chat(1.0), T + 1);
        let seen = a.query_at(T, T + BUCKET_MS, T + 2);
        assert!(started.elapsed() < std::time::Duration::from_millis(50));
        assert_eq!(seen["buckets"][0]["succeeded"], 1);
        drop(writing);
    }

    #[test]
    fn a_file_written_before_rejected_was_counted_loads_with_none_rejected() {
        let dir = temp_dir("before-rejected");
        let old = json!({"version":1,"coverage":[{"start_unix_ms":T,"end_unix_ms":T + 5}],
            "buckets":[{"start_unix_ms":T,"model":"m","capability":"chat","succeeded":2,"failed":1,
                "cancelled":0,"units":{},"units_unavailable":0}]});
        std::fs::write(dir.join(FILE), old.to_string()).unwrap();
        let a = Activity::open_at(dir.join(FILE), T + 10);
        let row = &a.query_at(T, T + BUCKET_MS, T + 20)["buckets"][0];
        assert_eq!(
            (&row["succeeded"], &row["failed"], &row["rejected"]),
            (&json!(2), &json!(1), &json!(0))
        );
    }

    #[test]
    fn a_corrupt_file_is_preserved_and_only_three_copies_are_kept() {
        let dir = temp_dir("corrupt");
        for i in 0..5u64 {
            std::fs::write(dir.join(FILE), b"{not json").unwrap();
            let a = Activity::open_at(dir.join(FILE), T + i);
            assert!(
                a.query_at(T, T + BUCKET_MS, T + 10)["buckets"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
        let mut copies: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("activity.json.corrupt-"))
            .collect();
        copies.sort();
        let expected: Vec<String> = (2..5)
            .map(|i| format!("activity.json.corrupt-{}", T + i))
            .collect();
        assert_eq!(copies, expected);
        assert_eq!(
            std::fs::read(dir.join(&copies[2])).unwrap(),
            b"{not json".to_vec()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn jobs_recorded_during_flushes_are_written_once_and_never_lost() {
        let dir = temp_dir("race");
        let a = Arc::new(Activity::open_at(dir.join(FILE), now_ms()));
        let recorders: Vec<_> = (0..4)
            .map(|_| {
                let a = a.clone();
                tokio::spawn(async move {
                    for _ in 0..250 {
                        a.record("m", "chat", chat(1.0));
                        tokio::task::yield_now().await;
                    }
                })
            })
            .collect();
        let flusher = {
            let a = a.clone();
            tokio::spawn(async move {
                for _ in 0..20 {
                    a.flush(false).await.unwrap();
                }
            })
        };
        // The shutdown flush races the last recordings too.
        let shutdown = a.flush(true);
        let (_, written) = tokio::join!(futures_util::future::join_all(recorders), shutdown);
        written.unwrap();
        flusher.await.unwrap();
        // Whatever landed after a snapshot is still dirty, so the last flush writes it.
        a.flush(true).await.unwrap();
        let reloaded = Activity::open_at(dir.join(FILE), now_ms());
        let total: u64 = reloaded.query(0, now_ms() + BUCKET_MS)["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["succeeded"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 1000);
        // A restart that loads and flushes again does not add the loaded counts a second time.
        let reloaded = Arc::new(reloaded);
        reloaded.flush(true).await.unwrap();
        let again = Activity::open_at(dir.join(FILE), now_ms());
        let total: u64 = again.query(0, now_ms() + BUCKET_MS)["buckets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["succeeded"].as_u64().unwrap())
            .sum();
        assert_eq!(total, 1000);
    }

    #[test]
    fn old_buckets_and_spans_are_pruned_after_35_days() {
        let a = Activity::open_at(temp_dir("prune").join(FILE), T);
        a.record_at("m", "chat", chat(1.0), T);
        let later = T + RETENTION_MS + BUCKET_MS;
        a.record_at("m", "chat", chat(1.0), later);
        assert!(a.snapshot(later, true).is_some());
        let all = a.query_at(0, later + BUCKET_MS, later);
        assert_eq!(all["buckets"].as_array().unwrap().len(), 1);
    }
}
