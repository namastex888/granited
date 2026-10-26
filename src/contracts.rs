use base64::Engine;
use schemars::{JsonSchema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Forecast {
    pub model: String,
    /// Consecutive, finite, equally spaced observations. Oldest first. Original units.
    pub history: Vec<f64>,
    pub horizon: usize,
    #[serde(default)]
    pub quantiles: Vec<f64>,
    pub frequency: Option<String>,
}

/// OpenAI `/v1/audio/speech` plus optional voice-cloning references; the worker returns WAV.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Synthesis {
    pub model: String,
    pub input: String,
    /// Accepted for OpenAI compatibility; voices come from `references`.
    pub voice: Option<String>,
    /// Only `wav`.
    pub response_format: Option<String>,
    #[serde(default)]
    pub references: Vec<Reference>,
    pub seed: Option<u64>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub max_new_tokens: Option<u32>,
    /// `true`: a WAV header with an unknown length, then 16-bit PCM as it is generated.
    pub stream: Option<bool>,
    /// With `stream`: `wav` (the default, above) or `events`: server-sent events, one `audio` event
    /// per chunk (base64 16-bit PCM), then one `done` event, always last, with `truncated`,
    /// `error`, `frames` and `audio_seconds`.
    pub stream_format: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// A WAV, FLAC or MP3 clip, base64 encoded.
    pub audio_base64: String,
    /// What the clip says.
    pub text: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Speech {
    pub model: String,
    /// Mono PCM samples in [-1,1]. No resampling is implicit.
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// Optional caller hint; it is not an observed language identification result.
    pub language: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
pub struct Document {
    pub id: String,
    pub text: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Retrieval {
    pub model: String,
    pub query: String,
    pub documents: Vec<Document>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rerank {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: Option<usize>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Vision {
    pub model: String,
    pub image_base64: String,
    pub mime_type: String,
    pub prompt: String,
    pub max_tokens: Option<u32>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Guardian {
    pub model: String,
    pub criterion: String,
    pub prompt: String,
    pub response: Option<String>,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Documents {
    pub model: String,
    pub file_base64: String,
    pub mime_type: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Geospatial {
    pub model: String,
    pub raster_base64: String,
    pub shape: [usize; 3],
    pub dtype: String,
    pub bands: Vec<String>,
    pub crs: String,
    pub transform: [f64; 6],
}

pub fn schemas() -> Value {
    json!({"forecast":schema_for!(Forecast), "speech":schema_for!(Speech), "synthesis":schema_for!(Synthesis),
        "retrieval":schema_for!(Retrieval), "rerank":schema_for!(Rerank), "vision":schema_for!(Vision),
        "guardian":schema_for!(Guardian), "documents":schema_for!(Documents), "geospatial":schema_for!(Geospatial)})
}

pub fn capability(path: &str) -> Option<&'static str> {
    Some(match path {
        "/v1/chat/completions" => "chat",
        "/v1/systemone" => "adapters",
        "/v1/embeddings" => "embeddings",
        "/v1/forecast" => "forecast",
        "/v1/anomalies" => "anomalies",
        "/v1/rerank" => "rerank",
        "/v1/retrieval/rank" => "retrieval",
        "/v1/documents/parse" => "documents",
        "/v1/geospatial/predict" => "geospatial",
        "/v1/guardian/evaluate" => "guardian",
        "/v1/vision/extract" => "vision",
        "/v1/speech/transcribe" | "/v1/audio/transcriptions" => "transcription",
        "/v1/audio/speech" => "speech_synthesis",
        _ => return None,
    })
}

pub fn validate(path: &str, value: &Value) -> Result<(), String> {
    if value
        .get("model")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        return Err("model must be a nonempty registered alias".into());
    }
    match path {
        "/v1/systemone" => {
            if value.get("state").is_none() {
                return Err("state is required".into());
            }
            let questions = value["questions"]
                .as_object()
                .filter(|q| !q.is_empty() && q.len() <= 64)
                .ok_or("questions must contain 1..64 typed questions")?;
            for question in questions.values() {
                if question.get("instructions").is_none() {
                    return Err("Every question requires instructions".into());
                }
                match question["type"].as_str() {
                    Some("choice")
                        if question["criteria"]
                            .as_object()
                            .is_some_and(|c| (2..=255).contains(&c.len())) => {}
                    Some("score")
                        if question["criteria"]
                            .as_array()
                            .is_some_and(|c| (2..=10).contains(&c.len())) => {}
                    Some("noul") => {}
                    _ => return Err(
                        "Expected choice with 2..255 criteria, score with 2..10 levels, or noul"
                            .into(),
                    ),
                }
            }
        }
        "/v1/forecast" => {
            let v: Forecast = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            if v.history.is_empty()
                || v.history.len() > 1_000_000
                || v.history.iter().any(|x| !x.is_finite())
                || v.horizon == 0
                || v.horizon > 16_384
                || v.quantiles
                    .iter()
                    .any(|q| !q.is_finite() || *q <= 0.0 || *q >= 1.0)
            {
                return Err("invalid history, horizon or quantiles".into());
            }
        }
        "/v1/audio/speech" => {
            let v: Synthesis = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            if v.input.trim().is_empty() || v.input.chars().count() > 4000 {
                return Err("input must be 1..4000 characters".into());
            }
            if v.response_format.as_deref().is_some_and(|f| f != "wav") {
                return Err("response_format must be wav".into());
            }
            if v.references.len() > 4
                || v.references.iter().any(|r| {
                    r.text.trim().is_empty()
                        || base64::engine::general_purpose::STANDARD
                            .decode(&r.audio_base64)
                            .is_err()
                })
            {
                return Err("up to 4 references, each with base64 audio and its transcript".into());
            }
        }
        "/v1/speech/transcribe" => {
            let v: Speech = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            if v.sample_rate == 0
                || v.samples.is_empty()
                || v.samples.len() as u64 > u64::from(v.sample_rate) * 300
                || v.samples.iter().any(|v| !v.is_finite() || v.abs() > 1.0)
            {
                return Err("expected up to 300 seconds of finite mono PCM in [-1,1]".into());
            }
        }
        "/v1/retrieval/rank" => {
            let v: Retrieval = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
            let ids: std::collections::HashSet<_> = v.documents.iter().map(|d| &d.id).collect();
            if v.query.is_empty()
                || v.documents.is_empty()
                || v.documents.len() > 1024
                || ids.len() != v.documents.len()
            {
                return Err(
                    "retrieval requires a query and 1..1024 documents with unique IDs".into(),
                );
            }
        }
        "/v1/chat/completions" => {
            if value["messages"].as_array().is_none_or(Vec::is_empty) {
                return Err("messages must be a nonempty array".into());
            }
        }
        "/v1/embeddings" => {
            if !value["input"].is_string() && !value["input"].is_array() {
                return Err("input must be text or an array of texts".into());
            }
        }
        "/v1/rerank" => {
            serde_json::from_value::<Rerank>(value.clone()).map_err(|e| e.to_string())?;
        }
        "/v1/vision/extract" => {
            serde_json::from_value::<Vision>(value.clone()).map_err(|e| e.to_string())?;
        }
        "/v1/guardian/evaluate" => {
            serde_json::from_value::<Guardian>(value.clone()).map_err(|e| e.to_string())?;
        }
        "/v1/documents/parse" => {
            serde_json::from_value::<Documents>(value.clone()).map_err(|e| e.to_string())?;
        }
        "/v1/geospatial/predict" => {
            serde_json::from_value::<Geospatial>(value.clone()).map_err(|e| e.to_string())?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{capability, validate};
    use serde_json::json;

    #[test]
    fn synthesis_contract() {
        let ok = json!({"model":"tts","input":"Olá.","references":[{"audio_base64":"UklGRg==","text":"oi"}]});
        assert_eq!(capability("/v1/audio/speech"), Some("speech_synthesis"));
        assert!(validate("/v1/audio/speech", &ok).is_ok());
        let streamed = json!({"model":"tts","input":"x","stream":true});
        assert!(validate("/v1/audio/speech", &streamed).is_ok());
        for bad in [
            json!({"model":"tts","input":"  "}),
            json!({"model":"tts","input":"x","response_format":"mp3"}),
            json!({"model":"tts","input":"x","references":[{"audio_base64":"not base64!","text":"oi"}]}),
            json!({"model":"tts","input":"x","references":[{"audio_base64":"UklGRg==","text":""}]}),
            json!({"model":"tts","input":"x","speed":2}),
        ] {
            assert!(validate("/v1/audio/speech", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn model_alias_is_required() {
        assert!(validate("/v1/embeddings", &json!({"input":"x"})).is_err());
        assert!(validate("/v1/embeddings", &json!({"model":"","input":"x"})).is_err());
    }

    #[test]
    fn systemone_requires_typed_questions() {
        let base = json!({"model":"switch","state":{}});
        assert!(validate("/v1/systemone", &base).is_err());
        let ok = json!({"model":"switch","state":{},"questions":{"q":{"instructions":"pick","type":"choice",
                        "criteria":{"a":"first","b":"second"}}}});
        assert!(validate("/v1/systemone", &ok).is_ok());
    }

    #[test]
    fn routes_map_to_capabilities() {
        assert_eq!(
            capability("/v1/audio/transcriptions"),
            Some("transcription")
        );
        assert_eq!(capability("/v1/unknown"), None);
    }
}
