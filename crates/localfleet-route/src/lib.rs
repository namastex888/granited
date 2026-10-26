//! What LocalFleet runs for `model: "auto"`. Where it runs stays in granited's placement.
//!
//! The shapes are ported from an Apache-2.0 agent runtime by the same team, which LocalFleet
//! serves. Its source is not public, so each ported file is stamped by digest:
//! - the runtime's `local.rs`, SHA-1 `4ebdb4d0eafa455566655d82b5ccedeb421c9994`: the
//!   `fallback_reason` strings.
//! - its router study's `offline_candidates.py`, SHA-256
//!   `36b2fe302e4f6e2efe3420387de7708b988caab41299fda76bbb2344777d9a6f`: ported as
//!   `tests/offline_candidates.rs`.
//!
//! `Modality`, `Effort`, `Thread` and `Source` follow that runtime's routing handoff, not its
//! code. LocalFleet adds `Thread::Decider`, `Source::Sticky` and
//! `FallbackReason::PreferredUnavailable`.

use serde::Serialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// The `x-granite-route` header is dropped rather than sent when it would exceed this.
pub const MAX_ROUTE_HEADER: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    Chat,
    Embeddings,
    Forecast,
    /// Chat with image parts, served by a vision model.
    Vision,
    /// Speech to text, as a stage before chat.
    Transcription,
    /// Text to speech on `/v1/audio/speech`.
    SpeechSynthesis,
}

/// A model that ran before the reply, such as speech to text for an `input_audio` part.
#[derive(Clone, Debug, Serialize)]
pub struct Stage {
    pub modality: Modality,
    pub model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Off,
    Low,
    Full,
}

impl Effort {
    /// LocalFleet's words, plus the OpenAI SDK's `none`, `minimal`, `medium` and `high`.
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "off" | "none" | "minimal" => Self::Off,
            "low" | "medium" => Self::Low,
            "full" | "high" => Self::Full,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Thread {
    Main,
    Background,
    Decider,
}

impl Thread {
    pub fn parse(word: &str) -> Option<Self> {
        Some(match word {
            "main" => Self::Main,
            "background" => Self::Background,
            "decider" => Self::Decider,
            _ => return None,
        })
    }
}

/// Why this model, in the source runtime's provenance vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Rule,
    Fallback,
    Sticky,
}

/// The source runtime's `fallback_reason` values, with the same strings, plus LocalFleet's
/// `preferred_unavailable`: a model other than the first preference served.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    Offline,
    Timeout,
    Stall,
    Error,
    Cap,
    PreferredUnavailable,
}

/// Which step of the pick chose the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Loaded with a free slot.
    Warm,
    /// A cold load that displaces no ready model.
    ColdFree,
    /// An earlier pick refused before running.
    RefusedNext,
    /// The conversation's model.
    Sticky,
    /// The conversation's model could not run, so it moved.
    StickyMoved,
    /// One fixed model per capability (embeddings, forecast).
    Pinned,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Clamp {
    /// A model that leaks reasoning with thinking off unless sampled greedily.
    TemperatureZero,
}

/// What placement can do for one candidate right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    Warm,
    ColdFree,
    /// Anything else: full, held, would evict, does not fit. `auto` never tries these.
    Unavailable,
}

/// The order to try candidates in: warm ones first, then cold loads that displace nothing, each
/// class in preference order. This is placement's own warm-before-cold rule applied across models,
/// so a warm smaller model is never evicted for a cold larger one, and nothing else is tried.
pub fn order<'a>(candidates: &[(&'a str, Availability)]) -> Vec<(&'a str, Why)> {
    [
        (Availability::Warm, Why::Warm),
        (Availability::ColdFree, Why::ColdFree),
    ]
    .iter()
    .flat_map(|&(class, why)| {
        candidates
            .iter()
            .filter(move |(_, a)| *a == class)
            .map(move |(id, _)| (*id, why))
    })
    .collect()
}

/// The `x-granite-route` header, version 1. It holds closed enums and registry ids only, never
/// request content, and does not list the other candidates.
#[derive(Clone, Debug, Serialize)]
pub struct Route {
    pub v: u8,
    pub modality: Modality,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preferred: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    pub thread: Thread,
    pub source: Source,
    pub why: Why,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<FallbackReason>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub clamps: Vec<Clamp>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub stages: Vec<Stage>,
}

impl Route {
    /// `preferred` is set whenever `model` is not the first preference. A sticky pick keeps
    /// `source: sticky`; any other non-first pick is a `fallback`. `fallback_reason` is set only
    /// when the first preference could not serve, so a conversation that stays on a smaller model
    /// while the larger one is available is not counted as a fallback.
    pub fn new(
        modality: Modality,
        model: &str,
        first: &str,
        first_available: bool,
        thread: Thread,
        why: Why,
    ) -> Self {
        let other = model != first;
        Self {
            v: 1,
            modality,
            model: model.to_owned(),
            preferred: other.then(|| first.to_owned()),
            effort: None,
            thread,
            source: match why {
                Why::Sticky => Source::Sticky,
                _ if other => Source::Fallback,
                _ => Source::Rule,
            },
            why,
            fallback_reason: (other && !(why == Why::Sticky && first_available))
                .then_some(FallbackReason::PreferredUnavailable),
            clamps: Vec::new(),
            stages: Vec::new(),
        }
    }

    /// The header value, or None when it would not be short ASCII.
    pub fn header(&self) -> Option<String> {
        let value = serde_json::to_string(self).ok()?;
        (value.len() <= MAX_ROUTE_HEADER && value.is_ascii()).then_some(value)
    }
}

/// Which model served each conversation (`x-granite-conversation`), so its persona and style
/// do not flip between turns. In memory on the first node, least recently used evicted first.
pub struct Conversations {
    capacity: usize,
    ttl: Duration,
    entries: HashMap<String, (String, Instant)>,
}

impl Conversations {
    pub fn new(capacity: usize, ttl: Duration) -> Self {
        Self {
            capacity,
            ttl,
            entries: HashMap::new(),
        }
    }

    /// 1 to 128 characters from `[A-Za-z0-9._:-]`.
    pub fn valid_id(id: &str) -> bool {
        (1..=128).contains(&id.len())
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
    }

    /// The conversation's model, unless it expired.
    pub fn get(&mut self, id: &str, now: Instant) -> Option<String> {
        let (model, used) = self.entries.get_mut(id)?;
        if now.duration_since(*used) > self.ttl {
            self.entries.remove(id);
            return None;
        }
        *used = now;
        Some(model.clone())
    }

    pub fn put(&mut self, id: &str, model: &str, now: Instant) {
        if !self.entries.contains_key(id) && self.entries.len() >= self.capacity {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.entries.remove(&oldest);
            }
        }
        self.entries.insert(id.to_owned(), (model.to_owned(), now));
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Availability::*;

    #[test]
    fn warm_before_cold_free_and_nothing_else() {
        let picks = order(&[("8b", ColdFree), ("3b", Warm)]);
        assert_eq!(picks, [("3b", Why::Warm), ("8b", Why::ColdFree)]);
        let picks = order(&[("8b", ColdFree), ("3b", ColdFree)]);
        assert_eq!(picks, [("8b", Why::ColdFree), ("3b", Why::ColdFree)]);
        let picks = order(&[("8b", Unavailable), ("3b", Warm)]);
        assert_eq!(picks, [("3b", Why::Warm)]);
        assert!(order(&[("8b", Unavailable), ("3b", Unavailable)]).is_empty());
    }

    #[test]
    fn conversations_expire_and_evict_the_least_recent() {
        let start = Instant::now();
        let mut store = Conversations::new(2, Duration::from_secs(60));
        store.put("a", "8b", start);
        store.put("b", "3b", start + Duration::from_secs(1));
        assert_eq!(
            store.get("a", start + Duration::from_secs(2)),
            Some("8b".into())
        );
        store.put("c", "3b", start + Duration::from_secs(3));
        assert_eq!(store.len(), 2);
        assert_eq!(
            store.get("b", start + Duration::from_secs(3)),
            None,
            "b was least recently used"
        );
        assert_eq!(
            store.get("a", start + Duration::from_secs(200)),
            None,
            "a expired"
        );
        assert!(Conversations::valid_id("chat-42:turns.v1_x"));
        for bad in ["", "has space", "ünicode", &"x".repeat(129)] {
            assert!(!Conversations::valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn effort_accepts_openai_words() {
        for (word, effort) in [
            ("none", Effort::Off),
            ("minimal", Effort::Off),
            ("medium", Effort::Low),
            ("high", Effort::Full),
            ("full", Effort::Full),
        ] {
            assert_eq!(Effort::parse(word), Some(effort));
        }
        assert_eq!(Effort::parse("max"), None);
    }

    #[test]
    fn fallback_reason_strings_match_the_source() {
        let words = [
            FallbackReason::Offline,
            FallbackReason::Timeout,
            FallbackReason::Stall,
            FallbackReason::Error,
            FallbackReason::Cap,
            FallbackReason::PreferredUnavailable,
        ]
        .map(|r| serde_json::to_value(r).unwrap());
        assert_eq!(
            serde_json::json!(words),
            serde_json::json!([
                "offline",
                "timeout",
                "stall",
                "error",
                "cap",
                "preferred_unavailable"
            ])
        );
    }

    #[test]
    fn route_marks_a_fallback_and_stays_short() {
        let mut route = Route::new(
            Modality::Chat,
            "granite-4.2-3b",
            "granite-4.2-8b",
            false,
            Thread::Main,
            Why::Warm,
        );
        route.effort = Some(Effort::Off);
        route.clamps.push(Clamp::TemperatureZero);
        assert_eq!(
            route.header().unwrap(),
            r#"{"v":1,"modality":"chat","model":"granite-4.2-3b","preferred":"granite-4.2-8b","effort":"off","thread":"main","source":"fallback","why":"warm","fallback_reason":"preferred_unavailable","clamps":["temperature_zero"]}"#
        );
        let first = Route::new(
            Modality::Chat,
            "granite-4.2-8b",
            "granite-4.2-8b",
            true,
            Thread::Main,
            Why::ColdFree,
        );
        let first = first.header().unwrap();
        assert!(!first.contains("preferred") && first.contains(r#""source":"rule""#));
        // A conversation kept on 3B while 8B is available is sticky, not a fallback.
        let held = Route::new(
            Modality::Chat,
            "granite-4.2-3b",
            "granite-4.2-8b",
            true,
            Thread::Main,
            Why::Sticky,
        );
        let held = held.header().unwrap();
        assert!(
            held.contains(r#""source":"sticky""#)
                && held.contains(r#""preferred":"granite-4.2-8b""#)
        );
        assert!(!held.contains("fallback_reason"));
        let mut staged = Route::new(
            Modality::Vision,
            "granite-vision-4.1-4b",
            "granite-vision-4.1-4b",
            true,
            Thread::Main,
            Why::Warm,
        );
        staged.stages.push(Stage {
            modality: Modality::Transcription,
            model: "granite-speech-5.0-470m-turboctc".into(),
        });
        assert!(staged.header().unwrap().contains(
            r#""stages":[{"modality":"transcription","model":"granite-speech-5.0-470m-turboctc"}]"#
        ));
        let longest = "m".repeat(64);
        let mut big = Route::new(
            Modality::Chat,
            &longest,
            &format!("{longest}x"),
            false,
            Thread::Background,
            Why::StickyMoved,
        );
        big.effort = Some(Effort::Full);
        big.clamps.push(Clamp::TemperatureZero);
        assert!(big.header().unwrap().len() < 512);
        let long = "m".repeat(MAX_ROUTE_HEADER);
        assert!(
            Route::new(Modality::Chat, &long, &long, true, Thread::Main, Why::Warm)
                .header()
                .is_none()
        );
        assert!(
            Route::new(
                Modality::Chat,
                "modèle",
                "modèle",
                true,
                Thread::Main,
                Why::Warm
            )
            .header()
            .is_none()
        );
    }
}
