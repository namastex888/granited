use crate::{
    activity, contracts,
    lifecycle::{Failure, HostClass, LoadMode, Service, failure},
    voices,
};
use axum::{
    Json, Router,
    body::{Body, Bytes, HttpBody},
    extract::{
        DefaultBodyLimit, FromRequest, Multipart, OriginalUri, Path, Request, State,
        rejection::JsonRejection,
    },
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use base64::Engine;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        let status = match self.code.as_str() {
            "unknown_model" | "unknown_operation" | "route_not_found" | "voice_not_found" => {
                StatusCode::NOT_FOUND
            }
            "voice_exists" => StatusCode::CONFLICT,
            "voice_has_no_references" => StatusCode::BAD_REQUEST,
            "voice_store_error" => StatusCode::INTERNAL_SERVER_ERROR,
            "invalid_schema"
            | "invalid_operation"
            | "auto_not_forwardable"
            | "unsupported_auto" => StatusCode::BAD_REQUEST,
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "unsupported_capability" | "unsupported_model" | "prefetch_unsupported" => {
                StatusCode::NOT_IMPLEMENTED
            }
            "model_busy" | "operation_conflict" => StatusCode::CONFLICT,
            "admission_busy" => StatusCode::TOO_MANY_REQUESTS,
            "backend_error" | "worker_identity" | "revision_mismatch" => StatusCode::BAD_GATEWAY,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        };
        let mut response = (
            status,
            Json(json!({"error":{"type":"granite_error","code":self.code,"message":self.message}})),
        )
            .into_response();
        if status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::TOO_MANY_REQUESTS {
            // A full slot frees within seconds; loads, memory and outages take longer.
            let wait = match self.code.as_str() {
                "admission_busy" | "capacity_unavailable" => "2",
                _ => "30",
            };
            response
                .headers_mut()
                .insert("retry-after", HeaderValue::from_static(wait));
        }
        if let Ok(code) = HeaderValue::from_str(&self.code) {
            response.headers_mut().insert("x-granite-error-code", code);
        }
        response
    }
}

fn equal(a: &[u8], b: &[u8]) -> bool {
    let mut mismatch = a.len() ^ b.len();
    for (i, c) in a.iter().enumerate() {
        mismatch |= usize::from(*c ^ b.get(i).copied().unwrap_or(0));
    }
    mismatch == 0
}

/// Holds a request in its client's in-flight count until its reply's body is done.
struct ClientGuard {
    service: Arc<Service>,
    label: Option<String>,
}

impl ClientGuard {
    fn new(service: &Arc<Service>, label: Option<String>) -> Self {
        service.clients.lock().unwrap().start(label.as_deref());
        ClientGuard {
            service: service.clone(),
            label,
        }
    }
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        let mut clients = self.service.clients.lock().unwrap();
        clients.finish(self.label.as_deref());
    }
}

async fn guard(State(service): State<Arc<Service>>, mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let path = request.uri().path().to_owned();
    let id = request
        .headers()
        .get("x-granite-request-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4)
        .to_string();
    request
        .headers_mut()
        .insert("x-granite-request-id", HeaderValue::from_str(&id).unwrap());
    let public = matches!(path.as_str(), "/healthz" | "/readyz");
    let admin = path.starts_with("/admin/") || path.starts_with("/internal/");
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    let admin_key = std::env::var(&service.config.admin_key_env).unwrap_or_default();
    let api_key = std::env::var(&service.config.api_key_env).unwrap_or_default();
    let authorized = public
        || equal(token.as_bytes(), admin_key.as_bytes())
        || (!admin && equal(token.as_bytes(), api_key.as_bytes()));
    // The client label is counted on the node the request entered (no hop) and never passed on.
    let label = crate::lifecycle::client_label(
        request
            .headers()
            .get("x-localfleet-client")
            .and_then(|v| v.to_str().ok()),
    );
    request.headers_mut().remove("x-localfleet-client");
    let entry = request
        .headers()
        .get("x-granite-hop")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0)
        == 0;
    let client = (authorized
        && entry
        && request.method() == axum::http::Method::POST
        && path.starts_with("/v1/"))
    .then(|| ClientGuard::new(&service, label));
    let mut response = if authorized {
        next.run(request).await
    } else {
        failure("unauthorized", "Valid bearer token required").into_response()
    };
    response
        .headers_mut()
        .insert("x-granite-request-id", HeaderValue::from_str(&id).unwrap());
    if matches!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS
    ) && !response.headers().contains_key("retry-after")
        && response
            .headers()
            .get("x-granite-error-code")
            .map(|v| v.as_bytes())
            != Some(b"auto_unavailable")
    {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static("30"));
    }
    for (name, value) in [
        ("x-granite-model-revision", "unresolved"),
        ("x-granite-backend", "control_plane"),
    ] {
        if !response.headers().contains_key(name) {
            response
                .headers_mut()
                .insert(name, HeaderValue::from_static(value));
        }
    }
    if !response.headers().contains_key("server-timing") {
        response.headers_mut().insert(
            "server-timing",
            HeaderValue::from_str(&format!(
                "router_headers;dur={:.3}",
                started.elapsed().as_secs_f64() * 1000.
            ))
            .unwrap(),
        );
    }
    // A streamed reply stays in flight until its body ends; a buffered one is done here.
    if let Some(client) = client
        && HttpBody::size_hint(response.body()).exact().is_none()
    {
        let (parts, body) = response.into_parts();
        let body = body.into_data_stream().map(move |chunk| {
            let _ = &client;
            chunk
        });
        response = Response::from_parts(parts, Body::from_stream(body));
    }
    tracing::info!(
        request_id = id,
        path,
        status = response.status().as_u16(),
        headers_ms = started.elapsed().as_millis(),
        "request"
    );
    response
}

pub fn router(service: Arc<Service>) -> Router {
    let mut app=Router::new()
        .route("/healthz",get(||async { Json(json!({"status":"ok"})) }))
        .route("/readyz",get(||async { Json(json!({"status":"ready","meaning":"Registry and control plane initialized; native models load on demand."})) }))
        .route("/v1/models",get(models)).route("/admin/models",get(admin_models)).route("/admin/discovery",get(discovery))
        .route("/admin/models/{id}/{action}",post(model_operation)).route("/admin/operations/{id}",get(operation))
        .route("/admin/operations/{id}/cancel",post(cancel)).route("/internal/hardware",get(hardware)).route("/v1/devices",get(devices)).route("/internal/hosts",get(hosts))
        .route("/internal/contracts",get(||async {Json(contracts::schemas())})).route("/internal/metrics",get(metrics))
        .route("/internal/activity",get(activity_report))
        .route("/v1/audio/transcriptions",post(audio))
        .route("/v1/voices",get(voices_list).post(voice_create))
        .route("/v1/voices/{id}",get(voice_get).patch(voice_patch).delete(voice_delete))
        .route("/v1/voices/{id}/references/{mood}",put(voice_reference));
    for path in [
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
        "/v1/audio/speech",
    ] {
        app = app.route(path, post(infer));
    }
    app.fallback(|| async { failure("route_not_found", "Unknown route") })
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(service.clone(), guard))
        .with_state(service)
}

/// Runs a voice-store call off the async workers; the store is small files on local disk.
async fn voice_call<F>(s: &Service, f: F) -> Response
where
    F: FnOnce(voices::Store) -> Result<Value, voices::VoiceError> + Send + 'static,
{
    let store = voices::Store::new(&s.config.state_dir);
    match tokio::task::spawn_blocking(move || f(store)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(e)) => failure(e.code(), e.message()).into_response(),
        Err(_) => failure("voice_store_error", "voice store call failed").into_response(),
    }
}

fn json_body(bytes: &Bytes) -> Result<Value, Failure> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| failure("invalid_schema", "body must be a JSON object"))
}

async fn voices_list(State(s): State<Arc<Service>>) -> Response {
    voice_call(&s, |store| store.list()).await
}

/// `{"voice_id", "name"}`: a new personal voice, with no clips yet.
async fn voice_create(State(s): State<Arc<Service>>, bytes: Bytes) -> Response {
    let body = match json_body(&bytes) {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    let (Some(id), name) = (
        body["voice_id"].as_str().map(str::to_owned),
        body["name"].as_str().unwrap_or("").to_owned(),
    ) else {
        return failure("invalid_schema", "voice_id is required").into_response();
    };
    voice_call(&s, move |store| store.create(&id, &name)).await
}

async fn voice_get(State(s): State<Arc<Service>>, Path(id): Path<String>) -> Response {
    voice_call(&s, move |store| store.get(&id)).await
}

async fn voice_patch(
    State(s): State<Arc<Service>>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> Response {
    let body = match json_body(&bytes) {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    voice_call(&s, move |store| store.patch(&id, &body)).await
}

async fn voice_delete(State(s): State<Arc<Service>>, Path(id): Path<String>) -> Response {
    voice_call(&s, move |store| {
        store
            .delete(&id)
            .map(|()| json!({"voice_id": id, "deleted": true}))
    })
    .await
}

/// Multipart `audio` (a WAV, 1 to 30 s) and `text` (its exact transcript): that mood's clip.
async fn voice_reference(
    State(s): State<Arc<Service>>,
    Path((id, mood)): Path<(String, String)>,
    request: Request,
) -> Response {
    let mut multipart = match Multipart::from_request(request, &s).await {
        Ok(m) => m,
        Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
    };
    let (mut audio, mut text) = (None, None);
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
        };
        match field.name().unwrap_or("") {
            "audio" => audio = field.bytes().await.ok(),
            "text" => text = field.text().await.ok(),
            other => {
                return failure(
                    "invalid_schema",
                    format!("unsupported field {other}: send audio and text"),
                )
                .into_response();
            }
        }
    }
    let (Some(audio), Some(text)) = (audio, text) else {
        return failure(
            "invalid_schema",
            "audio (a WAV) and text (its transcript) are required",
        )
        .into_response();
    };
    voice_call(&s, move |store| {
        store.put_reference(&id, &mood, &audio, &text)
    })
    .await
}

async fn models(State(s): State<Arc<Service>>) -> Json<Value> {
    let mut data = s.models.values().map(|m| {
        let mut entry = json!({"id":m.id,"object":"model","created":0,"owned_by":"ibm-granite","revision":m.hf_revision,"family":m.family,"capabilities":m.capabilities,"deployment_configured":s.workers.contains_key(&m.id)||s.config.peers.iter().any(|p|p.models.contains(&m.id))});
        // Per request, on this node; "engine" once the engine reported it at load.
        if let Some((tokens, source)) = s.context_of(&m.id) {
            entry["context_tokens"] = json!(tokens);
            entry["context_source"] = json!(source);
        }
        entry
    }).collect::<Vec<_>>();
    let auto = auto_capabilities(&s);
    if !auto.is_empty() {
        let mut inputs = vec!["text"];
        let deployed = |key: &str| {
            s.config
                .auto
                .prefer
                .get(key)
                .into_iter()
                .flatten()
                .any(|id| {
                    s.workers.contains_key(id)
                        || s.config.peers.iter().any(|p| p.models.contains(id))
                })
        };
        if auto.contains(&"chat") && s.config.auto.images && deployed("vision") {
            inputs.push("image");
        }
        if auto.contains(&"chat") && s.config.auto.audio && deployed("transcription") {
            inputs.push("audio");
        }
        data.insert(0, json!({"id":"auto","object":"model","created":0,"owned_by":"localfleet","family":"auto","capabilities":auto,"input_modalities":inputs,"deployment_configured":true}));
    }
    Json(json!({"object":"list","data":data}))
}

/// The capabilities this profile resolves `auto` for.
fn auto_capabilities(s: &Service) -> Vec<&'static str> {
    crate::config::AUTO_CAPABILITIES
        .into_iter()
        .filter(|c| s.config.auto.enabled && s.config.auto.prefer.contains_key(*c))
        .collect()
}

fn off_requires_t0(model: &crate::config::Model) -> bool {
    model.metadata.get("off_requires_temperature_zero") == Some(&Value::Bool(true))
}

/// The refusals after which `auto` may try its next host: the request never ran there.
const AUTO_REFUSALS: [&str; 9] = [
    "admission_busy",
    "capacity_unavailable",
    "model_not_warm",
    "accelerator_busy",
    "insufficient_memory",
    "insufficient_vram",
    "memory_pressure",
    "load_cooldown",
    "model_loading",
];

fn auto_refusal(response: &Response) -> Option<String> {
    if !matches!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS
    ) {
        return None;
    }
    let code = response
        .headers()
        .get("x-granite-error-code")
        .and_then(|v| v.to_str().ok());
    match code {
        Some(code) if AUTO_REFUSALS.contains(&code) => Some(code.to_owned()),
        None if response.status() == StatusCode::TOO_MANY_REQUESTS => Some("admission_busy".into()),
        _ => None,
    }
}

/// Largest decoded image part a chat request may carry.
const MAX_IMAGE_BYTES: usize = 8 << 20;
/// Largest decoded `input_audio` part `auto` transcribes.
const MAX_AUDIO_BYTES: usize = 16 << 20;
/// Each `input_audio` part is its own speech-to-text call, so a request carries at most this many.
const MAX_AUDIO_PARTS: usize = 4;
const TRANSCRIBE: &str = "/v1/audio/transcriptions";

fn content_parts(value: &Value) -> impl Iterator<Item = &Value> {
    value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| m["content"].as_array())
        .flatten()
}

/// The content part types a chat request may carry. A part of any other type, or with no type,
/// is refused: `video_url`, `audio_url`, `input_image`, `file` and the like can name a URL an
/// engine would fetch from inside the fleet.
const CHAT_PARTS: [&str; 4] = ["text", "image_url", "input_audio", "refusal"];

/// Content parts are allowlisted by type, and image parts must be inline `data:` URIs of png,
/// jpeg (or jpg) or webp in standard base64, at most 8 MiB decoded. Returns whether the request
/// carries an image.
fn check_images(value: &Value) -> Result<bool, String> {
    let mut any = false;
    for message in value["messages"].as_array().into_iter().flatten() {
        if !matches!(
            message.get("content"),
            None | Some(Value::Null | Value::String(_) | Value::Array(_))
        ) {
            return Err("message content must be a string or an array of parts".into());
        }
    }
    for part in content_parts(value) {
        let kind = part["type"].as_str().unwrap_or("");
        if !CHAT_PARTS.contains(&kind) {
            return Err(format!(
                "content part type {:?} is not accepted; use text, image_url (data: URI) or input_audio",
                part["type"]
            ));
        }
        if kind != "image_url" {
            continue;
        }
        any = true;
        let url = part["image_url"]["url"].as_str().unwrap_or("");
        let Some((mime, data)) = url
            .strip_prefix("data:")
            .and_then(|rest| rest.split_once(";base64,"))
        else {
            return Err(
                "image_url must be a data: URI (data:image/png;base64,...); remote URLs are not fetched"
                    .into(),
            );
        };
        let mime = mime.split(';').next().unwrap_or("");
        if !matches!(
            mime,
            "image/png" | "image/jpeg" | "image/jpg" | "image/webp"
        ) {
            return Err("image_url must be image/png, image/jpeg or image/webp".into());
        }
        let too_big = format!("an image exceeds {} MiB", MAX_IMAGE_BYTES >> 20);
        if data.len() / 4 * 3 > MAX_IMAGE_BYTES + 3 {
            return Err(too_big);
        }
        match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(bytes) if bytes.len() <= MAX_IMAGE_BYTES => {}
            Ok(_) => return Err(too_big),
            Err(_) => {
                return Err("image_url data is not valid standard base64 (no line breaks)".into());
            }
        }
    }
    Ok(any)
}

/// A vision model does not think: effort fields are dropped, and the route header carries none.
fn strip_reasoning(body: &mut Value) {
    if let Some(body) = body.as_object_mut() {
        body.remove("reasoning_effort");
        body.remove("thinking_budget_tokens");
        if let Some(kwargs) = body
            .get_mut("chat_template_kwargs")
            .and_then(Value::as_object_mut)
        {
            kwargs.remove("enable_thinking");
            kwargs.remove("low_effort");
            if kwargs.is_empty() {
                body.remove("chat_template_kwargs");
            }
        }
    }
}

/// Chat with `model: "auto"`: audio parts become text through the speech model first, then
/// the request goes to the vision list if it carries an image, else to the chat list.
async fn auto_entry(
    s: &Arc<Service>,
    path: &str,
    mut value: Value,
    headers: &axum::http::HeaderMap,
    start: Instant,
) -> Response {
    use localfleet_route::{Modality, Stage};
    if let Err(e) = resolve_auto(s, path, headers) {
        return e.into_response();
    }
    let images = match check_images(&value) {
        Ok(images) => images,
        Err(e) => return failure("invalid_schema", e).into_response(),
    };
    let audio = content_parts(&value).any(|p| p["type"] == "input_audio");
    for (present, on, what, flag) in [
        (images, s.config.auto.images, "Image", "images"),
        (audio, s.config.auto.audio, "Audio", "audio"),
    ] {
        if present && !on {
            return failure(
                "unsupported_auto",
                format!(
                    "{what} input under auto is off in this profile (auto.{flag}); name a model"
                ),
            )
            .into_response();
        }
    }
    let mut stages: Vec<Stage> = Vec::new();
    let mut positions = Vec::new();
    for (m, message) in value["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        for (p, part) in message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if part["type"] == "input_audio" {
                positions.push((m, p));
            }
        }
    }
    if positions.len() > MAX_AUDIO_PARTS {
        return failure(
            "invalid_schema",
            format!("at most {MAX_AUDIO_PARTS} input_audio parts per request"),
        )
        .into_response();
    }
    for (m, p) in positions {
        let part = &value["messages"][m]["content"][p];
        if part["input_audio"]["format"] != "wav" {
            return failure("invalid_schema", "input_audio format must be wav").into_response();
        }
        let data = part["input_audio"]["data"]
            .as_str()
            .unwrap_or("")
            .to_owned();
        match base64::engine::general_purpose::STANDARD.decode(&data) {
            Ok(bytes)
                if bytes.len() <= 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" =>
            {
                return failure("invalid_schema", "input_audio data is not a wav file")
                    .into_response();
            }
            Ok(bytes) if bytes.len() <= MAX_AUDIO_BYTES => {}
            Ok(_) => {
                return failure(
                    "invalid_schema",
                    format!("input_audio exceeds {} MiB", MAX_AUDIO_BYTES >> 20),
                )
                .into_response();
            }
            Err(_) => {
                return failure("invalid_schema", "input_audio data is not valid base64")
                    .into_response();
            }
        }
        // User audio only ever reaches speech to text, never a voice-reference slot.
        let body = json!({"model":s.config.auto.prefer["transcription"][0],"audio_base64":data});
        if let Err(e) = contracts::validate(TRANSCRIBE, &body) {
            return failure("invalid_schema", e).into_response();
        }
        let (response, served) = auto_route(
            s,
            TRANSCRIBE,
            Modality::Transcription,
            body,
            headers,
            start,
            Vec::new(),
        )
        .await;
        if !response.status().is_success() {
            return response;
        }
        let text = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v["text"].as_str().map(str::to_owned));
        let Some(text) = text else {
            return failure("backend_error", "The speech model returned no text").into_response();
        };
        value["messages"][m]["content"][p] = json!({"type":"text","text":text});
        if let Some(model) = served
            && !stages.iter().any(|stage| stage.model == model)
        {
            stages.push(Stage {
                modality: Modality::Transcription,
                model,
            });
        }
    }
    let modality = if images {
        Modality::Vision
    } else {
        Modality::Chat
    };
    auto_route(s, path, modality, value, headers, start, stages)
        .await
        .0
}

/// `auto`'s effort, written into the body so `normalize_chat` sees one explicit choice: the
/// client's own wins; a thinking budget alone means full; otherwise off. Then the one clamp:
/// a model that leaks reasoning with thinking off is sampled at temperature 0.
fn auto_effort(
    model: &crate::config::Model,
    body: &mut Value,
) -> (localfleet_route::Effort, Vec<localfleet_route::Clamp>) {
    use localfleet_route::{Clamp, Effort};
    let kwargs = &body["chat_template_kwargs"];
    let explicit = body.get("reasoning_effort").is_some()
        || kwargs.get("enable_thinking").is_some()
        || kwargs.get("low_effort").is_some();
    if !explicit {
        let budget = body.get("thinking_budget_tokens").and_then(Value::as_u64);
        body["reasoning_effort"] = json!(if budget.unwrap_or(0) > 0 {
            "full"
        } else {
            "off"
        });
    }
    let effort = match body.get("reasoning_effort").and_then(Value::as_str) {
        Some(word) => Effort::parse(word).unwrap_or(Effort::Full),
        None if body["chat_template_kwargs"]["enable_thinking"] == false => Effort::Off,
        None if body["chat_template_kwargs"]["low_effort"] == true => Effort::Low,
        None => Effort::Full,
    };
    let mut clamps = Vec::new();
    if effort == Effort::Off
        && off_requires_t0(model)
        && body.get("temperature").and_then(Value::as_f64) != Some(0.0)
    {
        body["temperature"] = json!(0);
        clamps.push(Clamp::TemperatureZero);
    }
    (effort, clamps)
}

/// One `auto` modality. Every preferred model's hosts are classified once; warm hosts are tried
/// first in preference order, then cold hosts whose load evicts nothing, and nothing else. Each
/// attempt leases in a mode that cannot load (warm) or cannot evict (cold), so the rule holds
/// where the load happens. A refusal before running moves to the next host. Returns the model
/// that served, if one did.
async fn auto_route(
    s: &Arc<Service>,
    path: &str,
    modality: localfleet_route::Modality,
    value: Value,
    headers: &axum::http::HeaderMap,
    start: Instant,
    stages: Vec<localfleet_route::Stage>,
) -> (Response, Option<String>) {
    use localfleet_route::{Modality, Route, Thread, Why};
    let (key, noun) = match modality {
        Modality::Vision => ("vision", "vision"),
        Modality::Transcription => ("transcription", "speech"),
        Modality::SpeechSynthesis => ("speech_synthesis", "voice"),
        _ => ("chat", "chat"),
    };
    let thread = match thread_of(headers) {
        Ok(thread) => thread,
        Err(e) => return (e.into_response(), None),
    };
    let conversation = match headers.get("x-granite-conversation").map(|v| v.to_str()) {
        None => None,
        Some(Ok(id)) if localfleet_route::Conversations::valid_id(id) => Some(id.to_owned()),
        Some(_) => {
            let e = failure(
                "invalid_schema",
                "x-granite-conversation must be 1-128 characters from A-Z, a-z, 0-9, '.', '_', ':' and '-'",
            );
            return (e.into_response(), None);
        }
    };
    // Conversations stick to a text chat model only; an image or speech stage never moves one.
    let conversation = conversation.filter(|_| modality == Modality::Chat);
    let prefer = &s.config.auto.prefer[key];
    let first = prefer[0].as_str();
    let capability = contracts::capability(path).unwrap_or("chat");
    let background = thread == "background";
    let mut classes = Vec::new();
    for id in prefer {
        let mut hosts = s.auto_hosts(id, capability, background).await;
        if matches!(
            modality,
            Modality::Transcription | Modality::SpeechSynthesis
        ) {
            // User audio, and voice references, stay on this node; they never leave for a peer.
            hosts.retain(|(peer, _)| peer.is_none());
        }
        classes.push((id.as_str(), hosts));
    }
    let usable = |id: &str| {
        classes.iter().any(|(m, hosts)| {
            *m == id
                && hosts
                    .iter()
                    .any(|(_, c)| matches!(c, HostClass::Warm | HostClass::ColdFree))
        })
    };
    let sticky = conversation
        .as_deref()
        .and_then(|c| s.conversations.lock().unwrap().get(c, Instant::now()));
    let moved = sticky.as_deref().is_some_and(|m| !usable(m));
    let mut attempts: Vec<(&str, Option<crate::config::Peer>, HostClass, Why)> = Vec::new();
    for class in [HostClass::Warm, HostClass::ColdFree] {
        for (id, hosts) in &classes {
            for (peer, _) in hosts.iter().filter(|(_, c)| *c == class) {
                let why = match (sticky.as_deref() == Some(*id), moved, class) {
                    (true, _, _) => Why::Sticky,
                    (_, true, _) => Why::StickyMoved,
                    (_, _, HostClass::Warm) => Why::Warm,
                    _ => Why::ColdFree,
                };
                attempts.push((id, peer.clone(), class, why));
            }
        }
    }
    // The conversation's model goes first while it can serve.
    attempts.sort_by_key(|(_, _, _, why)| *why != Why::Sticky);
    let thread_word = Thread::parse(thread).unwrap_or(Thread::Main);
    let mut refused_busy = false;
    let mut refused = false;
    for (id, peer, class, why) in attempts {
        let model = &s.models[id];
        let mut body = value.clone();
        body["model"] = json!(id);
        let (effort, clamps) = match modality {
            Modality::Chat => {
                let (effort, clamps) = auto_effort(model, &mut body);
                (Some(effort), clamps)
            }
            Modality::Vision => {
                strip_reasoning(&mut body);
                (None, Vec::new())
            }
            _ => (None, Vec::new()),
        };
        if let Err(e) = prepare(s, path, id, &mut body) {
            return (e.into_response(), None);
        }
        let mode = if class == HostClass::Warm {
            LoadMode::WarmOnly
        } else {
            LoadMode::NoEvict
        };
        let peer_attempt = peer.is_some();
        let mut response = run(s, path, id, body, headers, thread, vec![peer], mode, start).await;
        if let Some(code) = auto_refusal(&response) {
            refused_busy |= code == "admission_busy";
            refused = true;
            if peer_attempt {
                // run counts local lease refusals itself; a lone peer attempt is not counted there.
                s.count_rejection(id, &code);
            }
            continue;
        }
        let why = if refused && why != Why::Sticky {
            Why::RefusedNext
        } else {
            why
        };
        let mut route = Route::new(modality, id, first, usable(first), thread_word, why);
        route.effort = effort;
        route.clamps = clamps;
        route.stages = stages;
        if let Some(header) = route.header()
            && let Ok(value) = HeaderValue::from_str(&header)
        {
            response.headers_mut().insert("x-granite-route", value);
        }
        if response.status().is_success()
            && let Some(conversation) = &conversation
        {
            s.conversations
                .lock()
                .unwrap()
                .put(conversation, id, Instant::now());
        }
        return (response, Some(id.to_owned()));
    }
    // Nothing could serve without loading over a ready model. The reason is closed and the
    // per-host detail stays in /internal/hosts and the logs.
    let all = classes
        .iter()
        .flat_map(|(_, hosts)| hosts.iter().map(|(_, c)| *c));
    let (reason, status, wait, message) = if refused_busy
        || all.clone().any(|c| c == HostClass::Full)
    {
        (
            "busy",
            StatusCode::TOO_MANY_REQUESTS,
            Some("2"),
            format!("Every loaded {noun} model is busy; retry shortly"),
        )
    } else if all.clone().any(|c| c != HostClass::Held) {
        (
            "no_room",
            StatusCode::SERVICE_UNAVAILABLE,
            Some("30"),
            format!(
                "No preferred {noun} model can serve without evicting a loaded model; name a model to allow loading"
            ),
        )
    } else {
        (
            "held",
            StatusCode::SERVICE_UNAVAILABLE,
            None,
            format!(
                "Every preferred {noun} model is held or not deployed here; an operator must release one"
            ),
        )
    };
    let summary: Vec<_> = classes
        .iter()
        .map(|(id, hosts)| {
            (
                *id,
                hosts
                    .iter()
                    .map(|(peer, c)| (peer.as_ref().map(|p| p.id.as_str()), *c))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    tracing::info!(?summary, reason, noun, "auto found no host");
    let mut response = (
        status,
        Json(json!({"error":{"type":"granite_error","code":"auto_unavailable","reason":reason,"message":message}})),
    )
        .into_response();
    response.headers_mut().insert(
        "x-granite-error-code",
        HeaderValue::from_static("auto_unavailable"),
    );
    if let Some(wait) = wait {
        response
            .headers_mut()
            .insert("retry-after", HeaderValue::from_static(wait));
    }
    (response, None)
}

/// `auto` resolves once, on the node that first receives the request; peers only ever see a
/// concrete id. Embeddings and forecast are pinned to their first preference.
fn no_hop(headers: &axum::http::HeaderMap) -> Result<(), Failure> {
    if headers
        .get("x-granite-hop")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0)
        > 0
    {
        return Err(failure(
            "auto_not_forwardable",
            "auto is resolved by the first node; peers receive a model id",
        ));
    }
    Ok(())
}

fn resolve_auto(
    s: &Service,
    path: &str,
    headers: &axum::http::HeaderMap,
) -> Result<String, Failure> {
    no_hop(headers)?;
    let capability = contracts::capability(path).unwrap_or("");
    if !auto_capabilities(s).contains(&capability) {
        return Err(failure(
            "unsupported_auto",
            format!("auto is not available for {path} in this profile; name a model"),
        ));
    }
    Ok(s.config.auto.prefer[capability][0].clone())
}
async fn devices(State(s): State<Arc<Service>>) -> Json<Value> {
    Json(s.devices().await)
}
async fn admin_models(State(s): State<Arc<Service>>) -> Json<Value> {
    Json(
        json!({"models":s.models.values().collect::<Vec<_>>(),"workers":s.worker_status(),"peers":s.config.peers,"note":"Registry validation describes card review; execution receipts and benchmarks are separate artifacts."}),
    )
}
async fn discovery() -> Result<Json<Value>, Failure> {
    let bytes = tokio::fs::read("reports/official-inventory.json")
        .await
        .map_err(|e| failure("inventory_unavailable", e.to_string()))?;
    let value = serde_json::from_slice::<Value>(&bytes)
        .map_err(|e| failure("inventory_invalid", e.to_string()))?;
    Ok(Json(
        json!({"source":"https://huggingface.co/ibm-granite","executable":false,"repositories":value}),
    ))
}
async fn hardware(State(s): State<Arc<Service>>) -> Json<Value> {
    Json(s.hardware().await)
}
async fn hosts(State(s): State<Arc<Service>>) -> Json<Value> {
    let mut values = vec![
        json!({"host":s.config.host_id,"reachable":true,"hardware":s.hardware().await,"usage":s.usage_block(),
            "clients":s.clients.lock().unwrap().block()}),
    ];
    // Probe peers concurrently so one unreachable node does not serialize the whole report.
    values.extend(
        futures_util::future::join_all(s.config.peers.iter().map(|peer| async {
            let get = |path: &str| {
                s.client
                    .get(format!("{}{path}", peer.url))
                    .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
                    .timeout(Duration::from_secs(5))
                    .send()
            };
            let started = Instant::now();
            let (result, metrics) =
                futures_util::future::join(get("/internal/hardware"), get("/internal/metrics"))
                    .await;
            let mut entry = match result {
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(hw)=>json!({"host":peer.id,"reachable":true,"round_trip_ms":started.elapsed().as_secs_f64()*1000.,"hardware":hw}),
                    Err(e)=>json!({"host":peer.id,"reachable":false,"error":e.to_string()}),
                },
                Ok(r)=>json!({"host":peer.id,"reachable":false,"status":r.status().as_u16()}),
                Err(e)=>json!({"host":peer.id,"reachable":false,"error":e.to_string()}),
            };
            // Each host reports only what it ran, so one entry per host never double counts.
            let metrics = match metrics {
                Ok(r) if r.status().is_success() => {
                    r.json::<Value>().await.map_err(|e| e.to_string())
                }
                Ok(r) => Err(format!("metrics answered {}", r.status().as_u16())),
                Err(e) => Err(e.to_string()),
            };
            for key in ["usage", "clients"] {
                match &metrics {
                    Ok(m) if m[key].is_object() => entry[key] = m[key].clone(),
                    Ok(_) => {
                        entry[key] = Value::Null;
                        entry[format!("{key}_error")] =
                            json!(format!("peer reports no {key} (older engine)"));
                    }
                    Err(reason) => {
                        entry[key] = Value::Null;
                        entry[format!("{key}_error")] = json!(reason);
                    }
                }
            }
            entry
        }))
        .await,
    );
    Json(json!({"hosts":values}))
}
#[derive(serde::Deserialize)]
struct ActivityQuery {
    since_unix_ms: Option<u64>,
    until_unix_ms: Option<u64>,
    /// `local`: this host only, which is what a node asks its peers for, so nothing fans out twice.
    scope: Option<String>,
}

/// Jobs per 15-minute bucket on each host of the fleet. `until` defaults to now and `since` to 24 h
/// before it; one request spans at most the 35 days kept.
async fn activity_report(
    State(s): State<Arc<Service>>,
    query: Result<axum::extract::Query<ActivityQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(axum::extract::Query(query)) = query else {
        return failure(
            "invalid_schema",
            "since_unix_ms and until_unix_ms must be unix milliseconds",
        )
        .into_response();
    };
    let until = query.until_unix_ms.unwrap_or_else(activity::now_ms);
    let since = query
        .since_unix_ms
        .unwrap_or(until.saturating_sub(24 * 60 * 60 * 1000));
    if since > until || until - since > activity::RETENTION_MS {
        return failure(
            "invalid_schema",
            "since_unix_ms must not be after until_unix_ms, and one request spans at most 35 days",
        )
        .into_response();
    }
    let mut local = s.activity.query(since, until);
    local["host"] = json!(s.config.host_id);
    let mut hosts = vec![local];
    if query.scope.as_deref() != Some("local") {
        // Each host reports only the jobs it ran, so the fleet sum counts every job once.
        hosts.extend(
            futures_util::future::join_all(s.config.peers.iter().map(|peer| async {
                let url = format!(
                    "{}/internal/activity?since_unix_ms={since}&until_unix_ms={until}&scope=local",
                    peer.url
                );
                let result = s
                    .client
                    .get(url)
                    .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
                    .timeout(Duration::from_secs(5))
                    .send()
                    .await;
                let entry = match result {
                    Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                        Ok(mut v) if v["hosts"][0].is_object() => Ok(v["hosts"][0].take()),
                        Ok(_) => Err("peer reports no activity (older engine)".to_owned()),
                        Err(e) => Err(e.to_string()),
                    },
                    Ok(r) => Err(format!("activity answered {}", r.status().as_u16())),
                    Err(e) => Err(e.to_string()),
                };
                let mut entry = entry.unwrap_or_else(|e| json!({"activity_error":e}));
                entry["host"] = json!(peer.id);
                entry
            }))
            .await,
        );
    }
    // A crash loses at most the counts since the last flush.
    Json(json!({"bucket_seconds":activity::BUCKET_MS/1000,"retention_days":activity::RETENTION_DAYS,
        "flush_seconds":activity::FLUSH_SECONDS,"since_unix_ms":since,"until_unix_ms":until,"hosts":hosts}))
    .into_response()
}

async fn metrics(State(s): State<Arc<Service>>) -> Json<Value> {
    Json(
        json!({"uptime_seconds":s.started.elapsed().as_secs(),"workers":s.worker_status(),"operations":s.operations.lock().unwrap().values().cloned().collect::<Vec<_>>(),
            "rejections":*s.rejections.lock().unwrap(),"usage":s.usage_block(),"clients":s.clients.lock().unwrap().block()}),
    )
}
async fn model_operation(
    State(s): State<Arc<Service>>,
    Path((id, action)): Path<(String, String)>,
) -> Response {
    if !matches!(action.as_str(), "load" | "unload" | "prefetch") {
        return failure("invalid_operation", "Use load, unload or prefetch").into_response();
    }
    if !s.workers.contains_key(&id)
        && let Some(peer) = s.config.peers.iter().find(|p| p.models.contains(&id))
    {
        return match s
            .client
            .post(format!("{}/admin/models/{id}/{action}", peer.url))
            .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
            .timeout(Duration::from_secs(10))
            .send()
            .await
        {
            Ok(r) => forward_response(r, None, None).await,
            Err(e) => failure("host_unreachable", e.to_string()).into_response(),
        };
    }
    match s.start_operation(&id, &action) {
        Ok(op) => (StatusCode::ACCEPTED, Json(op)).into_response(),
        Err(e) => e.into_response(),
    }
}
async fn operation(
    State(s): State<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Failure> {
    if uuid::Uuid::parse_str(&id).is_err() {
        return Err(failure("unknown_operation", id));
    }
    if let Some(op) = s.operations.lock().unwrap().get(&id) {
        return Ok(Json(json!(op)));
    }
    if let Ok(bytes) = tokio::fs::read(format!("{}/operations/{id}.json", s.config.state_dir)).await
    {
        return serde_json::from_slice(&bytes)
            .map(Json)
            .map_err(|e| failure("operation_record", e.to_string()));
    }
    for peer in &s.config.peers {
        if let Ok(r) = s
            .client
            .get(format!("{}/admin/operations/{id}", peer.url))
            .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
            .timeout(Duration::from_secs(5))
            .send()
            .await
            && r.status().is_success()
        {
            return r
                .json()
                .await
                .map(Json)
                .map_err(|e| failure("operation_record", e.to_string()));
        }
    }
    Err(failure("unknown_operation", id))
}
async fn cancel(State(s): State<Arc<Service>>, Path(id): Path<String>) -> Response {
    if uuid::Uuid::parse_str(&id).is_err() {
        return failure("unknown_operation", id).into_response();
    }
    {
        let operations = s.operations.lock().unwrap();
        if let Some(op) = operations.get(&id) {
            if op.status == "running" {
                op.cancel.notify_one();
            }
            return (
                StatusCode::ACCEPTED,
                Json(json!({"id":id,"cancellation_requested":op.status=="running"})),
            )
                .into_response();
        }
    }
    for peer in &s.config.peers {
        if let Ok(r) = s
            .client
            .post(format!("{}/admin/operations/{id}/cancel", peer.url))
            .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
            .timeout(Duration::from_secs(5))
            .send()
            .await
            && r.status() != StatusCode::NOT_FOUND
        {
            return forward_response(r, None, None).await;
        }
    }
    failure("unknown_operation", id).into_response()
}
async fn infer(
    State(s): State<Arc<Service>>,
    OriginalUri(uri): OriginalUri,
    headers: axum::http::HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let Json(value) = match body {
        Ok(v) => v,
        Err(e) => return failure("invalid_schema", e.body_text()).into_response(),
    };
    dispatch(s, uri.path(), value, &headers).await
}
async fn audio(State(s): State<Arc<Service>>, request: Request) -> Response {
    let headers = request.headers().clone();
    // The JSON form ({model, audio_base64, ...}) is what a node forwards to its peer.
    if headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"))
    {
        let value = match axum::body::to_bytes(request.into_body(), 32 * 1024 * 1024).await {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => value,
                Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
            },
            Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
        };
        return dispatch(s, "/v1/audio/transcriptions", value, &headers).await;
    }
    let mut multipart = match Multipart::from_request(request, &s).await {
        Ok(m) => m,
        Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
    };
    let mut value = json!({});
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
        };
        let name = field.name().unwrap_or("").to_owned();
        if name == "file" {
            match field.bytes().await {
                Ok(bytes) => {
                    value["audio_base64"] =
                        json!(base64::engine::general_purpose::STANDARD.encode(bytes))
                }
                Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
            }
        } else if ["model", "language", "response_format"].contains(&name.as_str()) {
            match field.text().await {
                Ok(text) => value[&name] = json!(text),
                Err(e) => return failure("invalid_schema", e.to_string()).into_response(),
            }
        } else {
            return failure(
                "invalid_schema",
                format!("Unsupported transcription field {name}"),
            )
            .into_response();
        }
    }
    if value.get("audio_base64").is_none() {
        return failure("invalid_schema", "file is required").into_response();
    }
    if value.get("response_format").is_some_and(|v| v != "json") {
        return failure("invalid_schema", "Only response_format=json is supported").into_response();
    }
    dispatch(s, "/v1/audio/transcriptions", value, &headers).await
}
async fn dispatch(
    s: Arc<Service>,
    path: &str,
    mut value: Value,
    headers: &axum::http::HeaderMap,
) -> Response {
    if let Err(e) = contracts::validate(path, &value) {
        return failure("invalid_schema", e).into_response();
    }
    if path != "/v1/audio/speech" {
        return dispatch_resolved(s, path, value, headers).await;
    }
    let voice_headers = match speech_voice(&s, &mut value).await {
        Ok(found) => found,
        Err(e) => return e.into_response(),
    };
    let mut response = dispatch_resolved(s, path, value, headers).await;
    for (name, text) in voice_headers {
        if let Ok(text) = HeaderValue::from_str(&text) {
            response.headers_mut().insert(name, text);
        }
    }
    response
}

/// A speech request naming a stored voice and sending no `references` gets that voice's clip for
/// the mood inline; `mood` is removed, so a worker or a peer sees only inline references. Returns
/// the headers that say which voice and mood were used.
async fn speech_voice(
    s: &Service,
    value: &mut Value,
) -> Result<Vec<(&'static str, String)>, Failure> {
    let mood = value
        .as_object_mut()
        .and_then(|o| o.remove("mood"))
        .and_then(|m| m.as_str().map(str::to_owned));
    let has_references = value["references"]
        .as_array()
        .is_some_and(|r| !r.is_empty());
    let Some(voice) = value["voice"].as_str().map(str::to_owned) else {
        return Ok(Vec::new());
    };
    if has_references {
        // Inline references win; a voice name beside them is ignored (OpenAI compatibility).
        return Ok(Vec::new());
    }
    let store = voices::Store::new(&s.config.state_dir);
    let input = value["input"].as_str().unwrap_or("").to_owned();
    let (id, wanted) = (voice.clone(), mood.clone());
    let resolved =
        tokio::task::spawn_blocking(move || store.resolve(&id, wanted.as_deref(), &input))
            .await
            .map_err(|_| failure("voice_store_error", "voice store call failed"))?
            .map_err(|e| match e {
                voices::VoiceError::Invalid(_) => failure(
                    "voice_not_found",
                    "voice names no stored voice; send references or a stored voice id",
                ),
                e => failure(e.code(), e.message()),
            })?;
    value["references"] = json!([resolved.reference]);
    if value["temperature"].is_null()
        && let Some(t) = resolved.temperature
    {
        value["temperature"] = json!(t);
    }
    let requested = match mood {
        Some(m) if voices::valid_name(&m) => m,
        Some(_) => "invalid".into(),
        None => "none".into(),
    };
    Ok(vec![
        ("x-granite-voice", voice),
        ("x-granite-mood-requested", requested),
        ("x-granite-mood-used", resolved.mood_used),
        ("x-granite-ref-hash", resolved.ref_hash),
    ])
}

/// A model's own speech limits from the registry: `max_input_chars` and `references_required`.
fn speech_limits(s: &Service, id: &str, value: &Value) -> Result<(), Failure> {
    let Some(model) = s.models.get(id) else {
        return Ok(());
    };
    if let Some(max) = model
        .metadata
        .get("max_input_chars")
        .and_then(Value::as_u64)
        && value["input"]
            .as_str()
            .is_some_and(|t| t.chars().count() as u64 > max)
    {
        return Err(failure(
            "invalid_schema",
            format!("{id} takes at most {max} input characters"),
        ));
    }
    if model.metadata.get("references_required") == Some(&Value::Bool(true))
        && value["references"].as_array().is_none_or(|r| r.is_empty())
    {
        return Err(failure(
            "invalid_schema",
            format!("{id} has no default voice: send references, or a stored voice id as voice"),
        ));
    }
    Ok(())
}

async fn dispatch_resolved(
    s: Arc<Service>,
    path: &str,
    mut value: Value,
    headers: &axum::http::HeaderMap,
) -> Response {
    let start = Instant::now();
    if s.config.auto.enabled && value["model"] == "auto" {
        if path == "/v1/chat/completions" && auto_capabilities(&s).contains(&"chat") {
            return auto_entry(&s, path, value, headers, start).await;
        }
        if path == "/v1/audio/speech"
            && s.config.auto.speech
            && s.config.auto.prefer.contains_key("speech_synthesis")
        {
            if let Err(e) = no_hop(headers) {
                return e.into_response();
            }
            let modality = localfleet_route::Modality::SpeechSynthesis;
            return auto_route(&s, path, modality, value, headers, start, Vec::new())
                .await
                .0;
        }
        match resolve_auto(&s, path, headers) {
            Ok(id) => value["model"] = json!(id),
            Err(e) => return e.into_response(),
        }
    }
    let id = s.resolve_model(value["model"].as_str().unwrap()).to_owned();
    value["model"] = json!(id);
    if let Err(e) = prepare(&s, path, &id, &mut value) {
        return e.into_response();
    }
    if path == "/v1/audio/speech"
        && let Err(e) = speech_limits(&s, &id, &value)
    {
        return e.into_response();
    }
    let thread = match thread_of(headers) {
        Ok(thread) => thread,
        Err(e) => return e.into_response(),
    };
    if !s.workers.contains_key(&id) && !s.config.peers.iter().any(|p| p.models.contains(&id)) {
        return failure(
            "model_unavailable",
            format!("{id} is discoverable; no validated native deployment is configured"),
        )
        .into_response();
    }
    let capability = contracts::capability(path).unwrap_or("");
    let candidates = match s.placement(&id, capability, thread == "background").await {
        Ok(candidates) => candidates,
        Err(e) => {
            s.count_rejection(&id, &e.code);
            return e.into_response();
        }
    };
    let mode = LoadMode::from_header(headers.get("x-granite-load").and_then(|v| v.to_str().ok()));
    run(
        &s, path, &id, value, headers, thread, candidates, mode, start,
    )
    .await
}

/// Checks and rewrites that depend on the concrete model: capability, Switch's adapter, and the
/// chat effort and thinking budget. Runs once per model a request is tried on.
fn prepare(s: &Service, path: &str, id: &str, value: &mut Value) -> Result<(), Failure> {
    let Some(model) = s.models.get(id) else {
        return Err(failure("unknown_model", id));
    };
    let capability = contracts::capability(path).unwrap_or("");
    if !model.capabilities.iter().any(|c| c == capability) {
        return Err(failure(
            "unsupported_capability",
            format!("{} does not declare {capability}", model.id),
        ));
    }
    if model.family == "switch"
        && path == "/v1/chat/completions"
        && value["chat_template_kwargs"]["adapter_name"]
            .as_str()
            .is_none()
    {
        return Err(failure(
            "invalid_schema",
            "Switch requires explicit chat_template_kwargs.adapter_name",
        ));
    }
    if path == "/v1/chat/completions"
        && let Err(e) = check_images(value)
    {
        return Err(failure("invalid_schema", e));
    }
    if path == "/v1/chat/completions"
        && model.family == "language"
        && let Err(e) = normalize_chat(off_requires_t0(model), value)
    {
        return Err(failure("invalid_schema", e));
    }
    if path == "/v1/systemone" {
        value["cache_prompt"] = json!(false);
    }
    Ok(())
}

fn thread_of(headers: &axum::http::HeaderMap) -> Result<&str, Failure> {
    let thread = headers
        .get("x-granite-thread")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("main");
    if !matches!(thread, "main" | "background" | "decider") {
        return Err(failure(
            "invalid_schema",
            "x-granite-thread must be main, background or decider",
        ));
    }
    Ok(thread)
}

/// Serve one concrete model on the ranked hosts: lease or forward, then provenance headers.
#[allow(clippy::too_many_arguments)]
async fn run(
    s: &Arc<Service>,
    path: &str,
    id: &str,
    mut value: Value,
    headers: &axum::http::HeaderMap,
    thread: &str,
    candidates: Vec<Option<crate::config::Peer>>,
    mode: LoadMode,
    start: Instant,
) -> Response {
    let model = &s.models[id];
    let capability = contracts::capability(path).unwrap_or("");
    let request_id = headers
        .get("x-granite-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let attempts = candidates.len();
    let mut outcome = None;
    for (attempt, placement) in candidates.into_iter().enumerate() {
        // Placement reads snapshots up to 2 s old; a host that filled up meanwhile refuses before
        // running anything, so the next-ranked host may take the request.
        let fallback = attempt + 1 < attempts;
        let local = placement.is_none();
        let peer_id = placement.as_ref().map(|peer| peer.id.clone());
        let (url, token, lease, revision, backend) = if local
            && let Some(worker) = s.workers.get(id)
        {
            let lease = match s.lease(id, thread == "background", mode).await {
                Ok(l) => l,
                Err(e) => {
                    s.count_rejection(id, &e.code);
                    if e.code == "admission_busy" && fallback {
                        continue;
                    }
                    return model_headers(
                        e.into_response(),
                        &worker.spec.revision,
                        &worker.spec.backend,
                    );
                }
            };
            if let Some(alias) = &worker.spec.backend_model {
                value["model"] = json!(alias);
            }
            (
                worker.spec.base_url.clone(),
                None,
                Some(lease),
                worker.spec.revision.clone(),
                worker.spec.backend.clone(),
            )
        } else if let Some(peer) = placement {
            let hop = headers
                .get("x-granite-hop")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            if hop >= 2 {
                return failure("routing_loop", "Peer hop limit reached").into_response();
            }
            (
                peer.url.clone(),
                Some((std::env::var(&peer.token_env).unwrap_or_default(), hop + 1)),
                None,
                model.hf_revision.clone().unwrap_or_default(),
                format!("peer:{}", peer.id),
            )
        } else {
            return failure("model_unavailable",format!("{} is discoverable; no validated native worker configured. See /admin/models for incompatibilities.",model.id)).into_response();
        };
        let admission = start.elapsed();
        let mut forward_path = path;
        let normalize_vision = path == "/v1/vision/extract" && lease.is_some() && backend == "vllm";
        if normalize_vision {
            let mime = value["mime_type"].as_str().unwrap_or("");
            if !matches!(mime, "image/png" | "image/jpeg" | "image/webp") {
                return failure(
                    "invalid_schema",
                    "mime_type must be image/png, image/jpeg or image/webp",
                )
                .into_response();
            }
            let image = value["image_base64"].as_str().unwrap_or("");
            if base64::engine::general_purpose::STANDARD
                .decode(image)
                .is_err()
            {
                return failure("invalid_schema", "image_base64 is invalid").into_response();
            }
            value = json!({"model":value["model"],"messages":[{"role":"user","content":[
                {"type":"text","text":value["prompt"]},{"type":"image_url","image_url":{"url":format!("data:{mime};base64,{image}")}}
            ]}],"max_tokens":value["max_tokens"].as_u64().unwrap_or(1024),"temperature":0,"stream":false});
            forward_path = "/v1/chat/completions";
        }
        // llama.cpp reports one completion count; a buffered reply gets its thinking share counted.
        let count_reasoning = local
            && backend == "llama_cpp"
            && path == "/v1/chat/completions"
            && value.get("thinking_budget_tokens").is_some()
            && value["stream"] != true;
        // Peers are LocalFleet nodes that apply the budget themselves; only the last hop adds it.
        let engine_body =
            (local && path == "/v1/chat/completions").then(|| engine_chat_body(&value, &backend));
        // A stream reports usage only when asked (llama.cpp and vLLM honour this); the final
        // usage-only chunk is dropped again unless the client asked for it too.
        let stream_usage = engine_body.is_some()
            && value["stream"] == true
            && value
                .get("stream_options")
                .is_none_or(|o| o.is_null() || o.is_object());
        let engine_body = engine_body.map(|mut body| {
            if stream_usage {
                body["stream_options"]["include_usage"] = json!(true);
            }
            body
        });
        let mut request = s
            .client
            .post(format!("{url}{forward_path}"))
            .json(engine_body.as_ref().unwrap_or(&value))
            .header("x-granite-request-id", request_id)
            .header("x-granite-thread", thread)
            .timeout(Duration::from_secs(600));
        if let Some((token, hop)) = token {
            request = request.bearer_auth(token).header("x-granite-hop", hop);
            if mode != LoadMode::Normal {
                request = request.header("x-granite-load", "warm-only");
            }
        }
        // Own the execution before awaiting headers or buffered bodies. Dropping the handler
        // at either await records cancellation; forwarding nodes never own an execution.
        let mut meter = lease.is_some().then(|| Meter {
            service: s.clone(),
            model: id.to_owned(),
            capability,
            strip_usage_chunk: stream_usage && value["stream_options"]["include_usage"] != true,
            settled: false,
        });
        let response = match request.send().await {
            Ok(upstream) if !local && fallback && refused_before_running(&upstream) => {
                let code = upstream
                    .headers()
                    .get("x-granite-error-code")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("admission_busy");
                s.count_rejection(id, code);
                continue;
            }
            Ok(upstream) if normalize_vision && upstream.status().is_success() => {
                let parsed = upstream.json::<Value>().await;
                drop(lease);
                // A 2xx whose body cannot be read reaches the client as a 502: a failed job.
                let outcome = match &parsed {
                    Ok(output) => {
                        s.count_usage(id, output.get("usage"));
                        let reply = activity::Reply {
                            usage: output.get("usage").cloned(),
                            ..Default::default()
                        };
                        activity::Outcome::Succeeded(activity::units(capability, &reply))
                    }
                    Err(_) => activity::Outcome::Failed,
                };
                if let Some(meter) = meter.as_mut() {
                    meter.settle(outcome);
                }
                match parsed {
                    Ok(output)=>Json(json!({"text":output["choices"][0]["message"]["content"],"usage":output["usage"],"model":id,"revision":revision,"structural_quality":null})).into_response(),
                    Err(e)=>failure("backend_error",e.to_string()).into_response(),
                }
            }
            Ok(upstream) if count_reasoning && upstream.status().is_success() => {
                let parsed = upstream.json::<Value>().await;
                let response = match parsed {
                    Ok(output) => {
                        let (response, usage) = reasoning_usage(&s.client, &url, output).await;
                        s.count_usage(id, Some(&usage));
                        // Units after the reasoning share was counted, so it can be recorded.
                        let reply = activity::Reply {
                            usage: Some(usage),
                            ..Default::default()
                        };
                        let units = activity::units(capability, &reply);
                        if let Some(meter) = meter.as_mut() {
                            meter.settle(activity::Outcome::Succeeded(units));
                        }
                        response
                    }
                    Err(e) => {
                        if let Some(meter) = meter.as_mut() {
                            meter.settle(activity::Outcome::Failed);
                        }
                        failure("backend_error", e.to_string()).into_response()
                    }
                };
                drop(lease);
                response
            }
            Ok(upstream) => forward_response(upstream, lease, meter).await,
            Err(e) => {
                // This node's own backend timed out or could not be reached: a failed job. A peer's
                // attempt is the peer's to count.
                if let Some(meter) = meter.as_mut() {
                    meter.settle(activity::Outcome::Failed);
                }
                // A peer that cannot be reached (dead host, closed tunnel) is skipped from now on,
                // and the next host takes the request. A timeout is not retried: it may have run.
                if let Some(peer) = &peer_id
                    && !e.is_timeout()
                {
                    tracing::warn!(peer, error = %e, "peer unreachable; trying the next host");
                    s.forget_peer(peer).await;
                    if fallback {
                        continue;
                    }
                }
                failure("backend_unreachable", e.to_string()).into_response()
            }
        };
        outcome = Some((response, local, revision, backend, admission));
        break;
    }
    let Some((response, local, revision, backend, admission)) = outcome else {
        // Only reachable when every candidate refused a fallback; the last one never does.
        return failure("capacity_unavailable", "Every candidate host was full").into_response();
    };
    let mut response = model_headers(response, &revision, &backend);
    response
        .headers_mut()
        .insert("x-granite-model-alias", HeaderValue::from_str(id).unwrap());
    if !response.headers().contains_key("x-granite-host") {
        response.headers_mut().insert(
            "x-granite-host",
            HeaderValue::from_str(&s.config.host_id).unwrap(),
        );
    }
    if local && let Some((tokens, _)) = s.context_of(id) {
        response
            .headers_mut()
            .insert("x-granite-context-tokens", HeaderValue::from(tokens));
    }
    if local && let Some(worker) = s.workers.get(id) {
        response.headers_mut().insert(
            "x-granite-device",
            HeaderValue::from_str(&worker.spec.device).unwrap(),
        );
        if let Ok(build) = HeaderValue::from_str(&worker.spec.backend_build) {
            response
                .headers_mut()
                .insert("x-granite-backend-build", build);
        }
        if let Some((_, digest)) = worker
            .spec
            .artifacts
            .iter()
            .find(|(path, _)| path.ends_with(".gguf"))
            && let Ok(hash) = HeaderValue::from_str(digest)
        {
            response
                .headers_mut()
                .insert("x-granite-artifact-sha256", hash);
        }
    }
    response
        .headers_mut()
        .insert("x-granite-thread", HeaderValue::from_str(thread).unwrap());
    // Keep the worker's own metrics (a speech worker reports first_audio and rtf) after ours.
    let upstream = response
        .headers()
        .get("server-timing")
        .and_then(|v| v.to_str().ok())
        .map(|v| format!(", {v}"))
        .unwrap_or_default();
    response.headers_mut().insert(
        "server-timing",
        HeaderValue::from_str(&format!(
            "admission;dur={:.3}, upstream_headers;dur={:.3}{upstream}",
            admission.as_secs_f64() * 1000.,
            (start.elapsed() - admission).as_secs_f64() * 1000.
        ))
        .unwrap(),
    );
    response
}
fn model_headers(mut response: Response, revision: &str, backend: &str) -> Response {
    for (key, value) in [
        ("x-granite-model-revision", revision),
        ("x-granite-backend", backend),
    ] {
        if !response.headers().contains_key(key)
            && let Ok(v) = HeaderValue::from_str(value)
        {
            response.headers_mut().insert(key, v);
        }
    }
    response
}
/// A peer's 429 is always admission (the request never ran); a 503 only when it says so.
fn refused_before_running(upstream: &reqwest::Response) -> bool {
    let code = upstream
        .headers()
        .get("x-granite-error-code")
        .and_then(|v| v.to_str().ok());
    upstream.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (upstream.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE
            && code == Some("capacity_unavailable"))
}

/// Counts a reply this node's backend served, from the usage the backend reported, once the body
/// has been read to the end, and records the job's outcome in the activity ledger. Reads usage and
/// unit fields only; no content is kept or logged. Dropped before an outcome, it was cancelled.
struct Meter {
    service: Arc<Service>,
    model: String,
    capability: &'static str,
    strip_usage_chunk: bool,
    settled: bool,
}

/// A JSON reply larger than this is passed on without reading its usage.
const METERED_JSON_BYTES: usize = 8 * 1024 * 1024;
/// An event still open past this many bytes is passed through as it arrives, unread; parsing
/// resumes after its blank line, so nothing is held back beyond this bound.
const SSE_EVENT_BYTES: usize = 64 * 1024;

/// What a metered reply's body is read for, by its content type.
enum Watch {
    Json(Vec<Bytes>, usize),
    Events(Events),
    /// Relayed WAV, walked chunk by chunk, and `Server-Timing audio;dur` for a body that is not
    /// RIFF at all.
    Wav(activity::Wav, Option<f64>),
    /// Audio and other bodies carry no token usage: counted as completed, never as missing usage.
    /// Their audio seconds can only come from `Server-Timing`.
    Unmetered(Option<f64>),
}

struct Events {
    /// Bytes of the event not yet complete, and how far they were searched for its end.
    pending: Vec<u8>,
    scanned: usize,
    /// A backend can report an application error inside an HTTP 200 event stream.
    failed: bool,
    /// The last usage seen.
    usage: Option<Value>,
    /// Drop the usage-only event (the client did not ask for it).
    strip: bool,
    /// Speech in `stream_format: "events"`: the seconds its final `done` event reports.
    audio_seconds: Option<f64>,
    /// Speech events seen: PCM bytes relayed in `audio` events, and whether `done` has passed, so a
    /// stream that breaks before its `done` can still end with one.
    audio_bytes: u64,
    speech: bool,
    speech_done: bool,
    /// While an oversized event passes through: its last bytes, to find its end across chunks.
    skipping: Option<Vec<u8>>,
}

/// Only `usage` and the unit fields are read from a JSON reply; serde skips everything else.
#[derive(serde::Deserialize)]
struct Reported {
    usage: Option<Value>,
    answers: Option<Shape>,
    data: Option<Shape>,
    ranked_ids: Option<Shape>,
    horizon: Option<Value>,
    audio_duration_seconds: Option<Value>,
}

impl Reported {
    fn reply(self) -> activity::Reply {
        activity::Reply {
            usage: self.usage,
            answers: self.answers.and_then(|s| s.entries()),
            data: self.data.and_then(|s| s.items()),
            ranked_ids: self.ranked_ids.and_then(|s| s.items()),
            horizon: self.horizon.as_ref().and_then(Value::as_u64),
            audio_duration_seconds: self.audio_duration_seconds.as_ref().and_then(Value::as_f64),
            audio_seconds: None,
            streamed: false,
        }
    }
}

/// Any JSON value, kept only as a length: a field of an unexpected shape reads as no length
/// instead of failing the whole reply (and losing its usage).
enum Shape {
    List(u64),
    Map(u64),
    Other,
}

impl Shape {
    fn items(self) -> Option<u64> {
        match self {
            Shape::List(n) => Some(n),
            _ => None,
        }
    }

    fn entries(self) -> Option<u64> {
        match self {
            Shape::Map(n) => Some(n),
            _ => None,
        }
    }
}

impl<'de> serde::Deserialize<'de> for Shape {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
        struct Length;
        impl<'de> Visitor<'de> for Length {
            type Value = Shape;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Shape, A::Error> {
                let mut n = 0;
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    n += 1;
                }
                Ok(Shape::List(n))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Shape, A::Error> {
                let mut n = 0;
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
                    n += 1;
                }
                Ok(Shape::Map(n))
            }
            fn visit_bool<E>(self, _: bool) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
            fn visit_i64<E>(self, _: i64) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
            fn visit_u64<E>(self, _: u64) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
            fn visit_f64<E>(self, _: f64) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
            fn visit_str<E>(self, _: &str) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
            fn visit_unit<E>(self) -> Result<Shape, E> {
                Ok(Shape::Other)
            }
        }
        deserializer.deserialize_any(Length)
    }
}

impl Watch {
    fn new(content_type: &str, strip_usage_chunk: bool, timing: Option<f64>) -> Watch {
        if ["audio/wav", "audio/x-wav", "audio/wave"]
            .iter()
            .any(|kind| content_type.starts_with(kind))
        {
            Watch::Wav(activity::Wav::default(), timing)
        } else if content_type.starts_with("application/json") {
            Watch::Json(Vec::new(), 0)
        } else if content_type.starts_with("text/event-stream") {
            Watch::Events(Events {
                pending: Vec::new(),
                scanned: 0,
                failed: false,
                usage: None,
                strip: strip_usage_chunk,
                audio_seconds: None,
                audio_bytes: 0,
                speech: false,
                speech_done: false,
                skipping: None,
            })
        } else {
            Watch::Unmetered(timing)
        }
    }

    fn pass(&mut self, chunk: Bytes) -> Bytes {
        match self {
            Watch::Json(parts, size) => {
                *size = size.saturating_add(chunk.len());
                if *size <= METERED_JSON_BYTES {
                    parts.push(chunk.clone());
                } else {
                    parts.clear();
                }
                chunk
            }
            Watch::Events(events) => events.pass(chunk),
            Watch::Wav(wav, _) => {
                wav.feed(&chunk);
                chunk
            }
            Watch::Unmetered(_) => chunk,
        }
    }

    /// The reply ended: whether its token usage is metered, what it carried (`None` when the body
    /// could not be read), and bytes still held back.
    fn end(self) -> (bool, Option<activity::Reply>, Bytes) {
        match self {
            Watch::Json(parts, size) => {
                let body = (size <= METERED_JSON_BYTES).then(|| parts.concat());
                // A struct also reads a JSON array, positionally; only an object can carry usage.
                let reply = body
                    .filter(|b| b.iter().find(|c| !c.is_ascii_whitespace()) == Some(&b'{'))
                    .and_then(|b| serde_json::from_slice::<Reported>(&b).ok())
                    .map(Reported::reply);
                (true, reply, Bytes::new())
            }
            Watch::Events(events) => {
                let rest = if events.strip {
                    events.pending.into()
                } else {
                    Bytes::new()
                };
                // Speech events carry seconds, not tokens: counted as completed, like a WAV body.
                let metered = events.usage.is_some() || events.audio_seconds.is_none();
                let reply = activity::Reply {
                    usage: events.usage,
                    streamed: true,
                    audio_seconds: events.audio_seconds,
                    ..Default::default()
                };
                (metered, Some(reply), rest)
            }
            Watch::Wav(wav, timing) => {
                // A malformed or cut-off WAV has no seconds; only a body that is not RIFF at all
                // falls back to the worker's own timing.
                let audio_seconds = match wav.seconds() {
                    Ok(seconds) => Some(seconds),
                    Err(activity::WavFault::NotRiff) => timing,
                    Err(activity::WavFault::Invalid) => None,
                };
                let reply = activity::Reply {
                    audio_seconds,
                    ..Default::default()
                };
                (false, Some(reply), Bytes::new())
            }
            Watch::Unmetered(timing) => {
                let reply = timing.map(|seconds| activity::Reply {
                    audio_seconds: Some(seconds),
                    ..Default::default()
                });
                (false, reply, Bytes::new())
            }
        }
    }
}

impl Events {
    /// For a speech event stream that broke before its `done`: the `done` event the worker would
    /// have sent, counting the 16-bit 24 kHz audio relayed. A blank line first ends any event the
    /// break cut short, so `done` always parses on its own.
    fn closing_done(&self) -> Option<Bytes> {
        if !self.speech || self.speech_done {
            return None;
        }
        let samples = self.audio_bytes / 2;
        let done = json!({"truncated": false, "frames": samples / 1920,
            "audio_seconds": (samples as f64 / 24.0).round() / 1000.0,
            "error": {"code": "backend_error", "message": "the backend stream broke before it finished"}});
        let lead = if self.pending.is_empty() { "" } else { "\n\n" };
        Some(Bytes::from(format!("{lead}event: done\ndata: {done}\n\n")))
    }

    /// Passes one chunk on. When the usage-only event is dropped, complete events go out as soon
    /// as their blank line arrives and only the incomplete tail waits.
    fn pass(&mut self, chunk: Bytes) -> Bytes {
        let mut out = Vec::new();
        let mut rest = &chunk[..];
        if let Some(mut tail) = self.skipping.take() {
            let held = tail.len();
            tail.extend_from_slice(rest);
            match event_end(&tail, 0) {
                Some(end) => {
                    // The held bytes held no blank line, so the end lies in this chunk.
                    let cut = end.saturating_sub(held);
                    out.extend_from_slice(&rest[..cut]);
                    rest = &rest[cut..];
                }
                None => {
                    self.skipping = Some(tail[tail.len().saturating_sub(3)..].to_vec());
                    return chunk;
                }
            }
        }
        self.pending.extend_from_slice(rest);
        let (mut start, mut from) = (0, self.scanned);
        while let Some(end) = event_end(&self.pending, from) {
            let event = &self.pending[start..end];
            let data = sse_data(event);
            let failed = data
                .as_ref()
                .is_some_and(|d| !d["error"].is_null() || d["type"] == "error")
                || std::str::from_utf8(event).is_ok_and(|text| {
                    text.lines().any(|line| {
                        line.strip_prefix("event:")
                            .is_some_and(|kind| kind.trim() == "error")
                    })
                });
            let usage = data.as_ref().map(|d| &d["usage"]).filter(|u| u.is_object());
            self.failed |= failed;
            let done = std::str::from_utf8(event).is_ok_and(|text| {
                text.lines().any(|line| {
                    line.strip_prefix("event:")
                        .is_some_and(|kind| kind.trim() == "done")
                })
            });
            if let Some(seconds) = data
                .as_ref()
                .and_then(|d| d["audio_seconds"].as_f64())
                .filter(|_| done)
            {
                self.audio_seconds = Some(seconds);
            }
            if let Some(pcm) = data.as_ref().and_then(|d| d["pcm_base64"].as_str()) {
                let padding = pcm.bytes().rev().take_while(|b| *b == b'=').count() as u64;
                self.audio_bytes += (pcm.len() as u64 / 4 * 3).saturating_sub(padding);
                self.speech = true;
            }
            self.speech_done |= done && self.speech;
            let usage_only = !failed
                && usage.is_some()
                && data
                    .as_ref()
                    .is_some_and(|d| d["choices"].as_array().is_none_or(|c| c.is_empty()));
            if let Some(usage) = usage {
                self.usage = Some(usage.clone());
            }
            if !(self.strip && usage_only) {
                out.extend_from_slice(event);
            }
            (start, from) = (end, end);
        }
        self.pending.drain(..start);
        if self.pending.len() > SSE_EVENT_BYTES {
            // Too large to hold: pass it through, in order, and find its end as it streams.
            self.skipping = Some(self.pending[self.pending.len() - 3..].to_vec());
            out.append(&mut self.pending);
        }
        // A blank line split across chunks is found by searching its last three bytes again.
        self.scanned = self.pending.len().saturating_sub(3);
        if self.strip { out.into() } else { chunk }
    }
}

impl Meter {
    fn settle(&mut self, outcome: activity::Outcome) {
        self.settled = true;
        let service = &self.service;
        service
            .activity
            .record(&self.model, self.capability, outcome);
    }

    fn finish(&mut self, watch: Watch) -> Bytes {
        let failed = matches!(&watch, Watch::Events(events) if events.failed);
        let (metered, reply, rest) = watch.end();
        if failed {
            self.settle(activity::Outcome::Failed);
            return rest;
        }
        let usage = reply
            .as_ref()
            .and_then(|r| r.usage.as_ref())
            .filter(|u| u.is_object());
        if metered && (usage.is_some() || activity::carries_tokens(self.capability)) {
            self.service.count_usage(&self.model, usage);
        } else {
            self.service.count_completed(&self.model);
        }
        // An unread body has no units, unless the capability defines none.
        let units = activity::units(self.capability, &reply.unwrap_or_default());
        self.settle(activity::Outcome::Succeeded(units));
        rest
    }
}

impl Drop for Meter {
    fn drop(&mut self) {
        if !self.settled {
            // The client went away before the body finished.
            self.settle(activity::Outcome::Cancelled);
        }
    }
}

/// The end of the first complete server-sent event in `buffer` that ends after `from`, after its
/// blank line (`\n\n` or `\r\n\r\n`).
fn event_end(buffer: &[u8], from: usize) -> Option<usize> {
    (from..buffer.len()).find_map(|i| {
        if buffer[i..].starts_with(b"\n\n") {
            Some(i + 2)
        } else if buffer[i..].starts_with(b"\r\n\r\n") {
            Some(i + 4)
        } else {
            None
        }
    })
}

/// The JSON in one event's `data:` lines, if it has any.
fn sse_data(event: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(event).ok()?;
    let data: Vec<&str> = text
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect();
    serde_json::from_str(&data.join("\n")).ok()
}

async fn forward_response(
    upstream: reqwest::Response,
    lease: Option<crate::lifecycle::Lease>,
    meter: Option<Meter>,
) -> Response {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    if !status.is_success() {
        // A 4xx is the backend refusing the request as malformed; a timeout (408) or a full queue
        // (429) is the backend failing to serve it, like a 5xx.
        if let Some(mut meter) = meter {
            let refused = status.is_client_error()
                && status != reqwest::StatusCode::REQUEST_TIMEOUT
                && status != reqwest::StatusCode::TOO_MANY_REQUESTS;
            meter.settle(if refused {
                activity::Outcome::Rejected
            } else {
                activity::Outcome::Failed
            });
        }
        let bytes = upstream.bytes().await.unwrap_or_default();
        let detail = serde_json::from_slice::<Value>(&bytes).unwrap_or_else(
            |_| json!({"message":String::from_utf8_lossy(&bytes[..bytes.len().min(4096)])}),
        );
        let body = if detail["error"]["type"] == "granite_error" {
            detail
        } else {
            json!({"error":{"type":"granite_backend_error","code":"backend_error","message":"Native worker rejected request","upstream":detail}})
        };
        let mut response = (status, Json(body)).into_response();
        for name in [
            "x-granite-model-revision",
            "x-granite-backend",
            "x-granite-error-code",
            "retry-after",
        ] {
            if let Some(value) = headers.get(name) {
                response.headers_mut().insert(name, value.clone());
            }
        }
        return response;
    }
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let timing = header("server-timing").and_then(activity::timing_seconds);
    let watch = meter.as_ref().map(|m| {
        Watch::new(
            header("content-type").unwrap_or(""),
            m.strip_usage_chunk,
            timing,
        )
    });
    let stream = futures_util::stream::unfold(
        (upstream.bytes_stream().fuse(), lease, meter, watch, false),
        |(mut stream, lease, mut meter, mut watch, ended)| async move {
            if ended {
                return None;
            }
            loop {
                let next = stream.next().await;
                match (next, meter.as_mut()) {
                    (Some(Ok(chunk)), Some(_)) => {
                        let chunk = watch.as_mut()?.pass(chunk);
                        if !chunk.is_empty() {
                            return Some((Ok(chunk), (stream, lease, meter, watch, false)));
                        }
                    }
                    (Some(Err(e)), m) => {
                        // A reply broken mid-stream did not complete: no usage, a failed job.
                        if let Some(m) = m {
                            m.settle(activity::Outcome::Failed);
                        }
                        // Speech events still end with `done`, carrying the error, then the stream ends.
                        if let Some(Watch::Events(events)) = &watch
                            && let Some(done) = events.closing_done()
                        {
                            return Some((Ok(done), (stream, lease, None, None, true)));
                        }
                        return Some((Err(e), (stream, lease, None, None, false)));
                    }
                    (Some(chunk), None) => {
                        return Some((chunk, (stream, lease, meter, watch, false)));
                    }
                    (None, Some(m)) => {
                        let rest = m.finish(watch.take()?);
                        return (!rest.is_empty())
                            .then(|| (Ok(rest), (stream, lease, None, None, false)));
                    }
                    (None, None) => return None,
                }
            }
        },
    );
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    for name in [
        "content-type",
        "cache-control",
        "x-granite-model-revision",
        "x-granite-backend",
        "x-granite-backend-build",
        "x-granite-artifact-sha256",
        "x-granite-host",
        "x-granite-model-alias",
        "x-granite-reasoning-tokens",
        "x-granite-context-tokens",
        "x-granite-truncated",
        "x-granite-frame-budget",
        "server-timing",
    ] {
        if let Some(value) = headers.get(name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

/// Adds `usage.completion_tokens_details.reasoning_tokens`, counted by re-tokenizing the returned
/// reasoning with the same engine. The header says so, because a re-tokenized count can differ
/// from the generated one by a token at the boundaries.
async fn reasoning_usage(
    client: &reqwest::Client,
    url: &str,
    mut output: Value,
) -> (Response, Value) {
    let reasoning = output["choices"][0]["message"]["reasoning_content"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let count = if reasoning.is_empty() {
        Some(0)
    } else {
        let tokenized = client
            .post(format!("{url}/tokenize"))
            .json(&json!({"model":output["model"],"content":reasoning,"add_special":false}))
            .timeout(Duration::from_secs(10))
            .send()
            .await;
        match tokenized {
            Ok(r) if r.status().is_success() => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["tokens"].as_array().map(|t| t.len() as u64)),
            _ => None,
        }
    };
    if let Some(count) = count
        && output["usage"].is_object()
    {
        output["usage"]["completion_tokens_details"]["reasoning_tokens"] = json!(count);
    }
    let usage = output["usage"].clone();
    let mut response = Json(output).into_response();
    if count.is_some() {
        response.headers_mut().insert(
            "x-granite-reasoning-tokens",
            HeaderValue::from_static("retokenized"),
        );
    }
    (response, usage)
}

/// Thinking tokens per effort when the client sends no `thinking_budget_tokens` (Felipe's ruling,
/// 2026-10-26): the reply keeps its own `max_tokens`, so thinking can never starve it.
const LOW_THINKING_BUDGET: u64 = 1024;
const FULL_THINKING_BUDGET: u64 = 4096;
const MAX_THINKING_BUDGET: u64 = 32768;

/// The body a local engine receives. Clients set `max_tokens` for the reply alone; engines count
/// thinking inside it, so the thinking budget is added here, at the last hop only, and the engine
/// ends thinking when the budget is spent. llama.cpp reads `thinking_budget_tokens`, oMLX
/// `thinking_budget`.
fn engine_chat_body(value: &Value, backend: &str) -> Value {
    let mut body = value.clone();
    let Some(budget) = body.get("thinking_budget_tokens").and_then(Value::as_u64) else {
        return body;
    };
    for key in ["max_tokens", "max_completion_tokens"] {
        if let Some(reply) = body.get(key).and_then(Value::as_u64) {
            body[key] = json!(reply.saturating_add(budget));
        }
    }
    if backend == "omlx" {
        let object = body.as_object_mut().unwrap();
        object.remove("thinking_budget_tokens");
        object.insert("thinking_budget".into(), json!(budget));
    }
    body
}

fn normalize_chat(off_needs_t0: bool, value: &mut Value) -> Result<(), String> {
    if value
        .get("chat_template_kwargs")
        .is_some_and(|v| !v.is_object())
    {
        return Err("chat_template_kwargs must be an object".into());
    }
    for key in ["enable_thinking", "low_effort"] {
        if value["chat_template_kwargs"]
            .get(key)
            .is_some_and(|v| !v.is_boolean())
        {
            return Err(format!("chat_template_kwargs.{key} must be boolean"));
        }
    }
    if let Some(effort) = value
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        let (thinking, low) = match effort.as_str() {
            "none" | "off" => (false, false),
            "low" => (true, true),
            "high" | "full" => (true, false),
            _ => return Err("reasoning_effort must be off, low or full".into()),
        };
        if value.get("chat_template_kwargs").is_some_and(|v| {
            v.get("enable_thinking").is_some_and(|v| v != thinking)
                || v.get("low_effort").is_some_and(|v| v != low)
        }) {
            return Err("reasoning_effort conflicts with chat_template_kwargs".into());
        }
        if value.get("chat_template_kwargs").is_none() {
            value["chat_template_kwargs"] = json!({});
        }
        value["chat_template_kwargs"]["enable_thinking"] = json!(thinking);
        value["chat_template_kwargs"]["low_effort"] = json!(low);
        value.as_object_mut().unwrap().remove("reasoning_effort");
    }
    let thinking = value["chat_template_kwargs"]["enable_thinking"] != false;
    let budget = match value.get("thinking_budget_tokens") {
        None if !thinking => 0,
        None if value["chat_template_kwargs"]["low_effort"] == true => LOW_THINKING_BUDGET,
        None => FULL_THINKING_BUDGET,
        Some(v) => v
            .as_u64()
            .filter(|b| *b <= MAX_THINKING_BUDGET)
            .ok_or("thinking_budget_tokens must be an integer from 0 to 32768")?,
    };
    if thinking && budget == 0 {
        // llama.cpp treats a per-request 0 as no limit, so thinking would run unbounded.
        return Err("thinking_budget_tokens 0 cannot stop thinking once it is on; send reasoning_effort off instead".into());
    }
    if thinking {
        value["thinking_budget_tokens"] = json!(budget);
    } else if budget > 0 {
        return Err(
            "thinking_budget_tokens needs thinking; set reasoning_effort to low or full".into(),
        );
    } else {
        value
            .as_object_mut()
            .unwrap()
            .remove("thinking_budget_tokens");
    }
    if thinking && value.get("grammar").is_some() {
        return Err("GBNF grammar with thinking can consume the answer inside reasoning; use response_format.json_schema or explicitly disable thinking".into());
    }
    if off_needs_t0 && !thinking && value.get("temperature").and_then(Value::as_f64) != Some(0.0) {
        return Err("This model with thinking off requires explicit temperature=0 because visible reasoning leaked at higher temperatures in prior probes".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Watch, engine_chat_body, equal, normalize_chat, reasoning_usage, refused_before_running,
    };
    use crate::lifecycle::failure;
    use axum::response::IntoResponse;
    use serde_json::json;

    #[test]
    fn busy_answers_retry_soon_and_name_their_code() {
        for (code, wait) in [
            ("admission_busy", "2"),
            ("capacity_unavailable", "2"),
            ("insufficient_memory", "30"),
        ] {
            let response = failure(code, "x").into_response();
            assert_eq!(response.headers()["retry-after"], wait, "{code}");
            assert_eq!(response.headers()["x-granite-error-code"], code);
        }
        assert!(
            !failure("unknown_model", "x")
                .into_response()
                .headers()
                .contains_key("retry-after")
        );
    }

    #[test]
    fn only_refusals_that_never_ran_move_to_the_next_host() {
        let peer = |status: u16, code: Option<&str>| {
            let mut b = axum::http::Response::builder().status(status);
            if let Some(c) = code {
                b = b.header("x-granite-error-code", c);
            }
            reqwest::Response::from(b.body("").unwrap())
        };
        assert!(refused_before_running(&peer(429, None)));
        assert!(refused_before_running(&peer(
            503,
            Some("capacity_unavailable")
        )));
        assert!(!refused_before_running(&peer(
            503,
            Some("backend_unreachable")
        )));
        assert!(!refused_before_running(&peer(503, None)));
        assert!(!refused_before_running(&peer(502, Some("backend_error"))));
        assert!(!refused_before_running(&peer(200, None)));
    }

    #[test]
    fn token_comparison_requires_exact_match() {
        assert!(equal(b"secret", b"secret"));
        assert!(!equal(b"secret", b"secreT"));
        assert!(!equal(b"secret", b"secret-longer"));
        assert!(!equal(b"", b"secret"));
    }

    #[test]
    fn reasoning_effort_maps_to_template_controls() {
        let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"low"});
        normalize_chat(false, &mut value).unwrap();
        assert_eq!(
            value["chat_template_kwargs"],
            json!({"enable_thinking":true,"low_effort":true})
        );
        assert!(value.get("reasoning_effort").is_none());
    }

    #[test]
    fn thinking_budget_defaults_per_effort_and_never_starves_the_reply() {
        let budget = |effort: &str| {
            let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":effort,
                                   "temperature":0,"max_tokens":300});
            normalize_chat(false, &mut value).unwrap();
            value
        };
        assert!(budget("off").get("thinking_budget_tokens").is_none());
        assert_eq!(budget("low")["thinking_budget_tokens"], 1024);
        let full = budget("full");
        assert_eq!(full["thinking_budget_tokens"], 4096);
        assert_eq!(
            full["max_tokens"], 300,
            "the client's reply budget is kept until the last hop"
        );
        let llama = engine_chat_body(&full, "llama_cpp");
        assert_eq!(
            (
                llama["max_tokens"].clone(),
                llama["thinking_budget_tokens"].clone()
            ),
            (json!(4396), json!(4096))
        );
        let mut huge = full.clone();
        huge["max_tokens"] = json!(u64::MAX);
        assert_eq!(
            engine_chat_body(&huge, "llama_cpp")["max_tokens"],
            json!(u64::MAX)
        );
        let omlx = engine_chat_body(&full, "omlx");
        assert_eq!(
            (
                omlx["thinking_budget"].clone(),
                omlx.get("thinking_budget_tokens")
            ),
            (json!(4096), None)
        );
        // Forwarding normalizes again on the peer; the reply budget must not grow twice.
        let mut again = full.clone();
        normalize_chat(false, &mut again).unwrap();
        assert_eq!(again, full);
    }

    #[tokio::test]
    async fn reasoning_tokens_are_counted_by_the_engine_tokenizer() {
        let engine = axum::Router::new().route(
            "/tokenize",
            axum::routing::post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let words = body["content"].as_str().unwrap().split(' ').count();
                    axum::Json(json!({"tokens": vec![0; words]}))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, engine).await.unwrap() });
        let client = reqwest::Client::new();
        let reply = |reasoning: &str| {
            json!({"model":"m","usage":{"completion_tokens":9},
            "choices":[{"message":{"content":"yes","reasoning_content":reasoning}}]})
        };
        let body = |r: axum::response::Response| async move {
            let headers = r.headers().clone();
            let bytes = axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap();
            (
                headers,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        };
        let (response, usage) = reasoning_usage(&client, &url, reply("one two three")).await;
        let (headers, value) = body(response).await;
        assert_eq!(
            value["usage"]["completion_tokens_details"]["reasoning_tokens"],
            3
        );
        // The ledger records the share beside output_tokens, which stays completion_tokens.
        let reply_usage = crate::activity::Reply {
            usage: Some(usage.clone()),
            ..Default::default()
        };
        assert_eq!(usage, value["usage"]);
        assert!(
            crate::activity::units("chat", &reply_usage).is_none(),
            "no prompt_tokens here"
        );
        let mut both = usage;
        both["prompt_tokens"] = json!(4);
        let units = crate::activity::units(
            "chat",
            &crate::activity::Reply {
                usage: Some(both),
                ..Default::default()
            },
        );
        assert_eq!(
            units.unwrap(),
            [
                ("prompt_tokens", 4.0),
                ("output_tokens", 9.0),
                ("reasoning_tokens", 3.0)
            ]
        );
        assert_eq!(headers["x-granite-reasoning-tokens"], "retokenized");
        let (_, value) = body(reasoning_usage(&client, &url, reply("")).await.0).await;
        assert_eq!(
            value["usage"]["completion_tokens_details"]["reasoning_tokens"],
            0
        );
        // An engine without /tokenize leaves usage as the engine reported it.
        let (headers, value) = body(
            reasoning_usage(&client, "http://127.0.0.1:9", reply("a b"))
                .await
                .0,
        )
        .await;
        assert!(value["usage"].get("completion_tokens_details").is_none());
        assert!(!headers.contains_key("x-granite-reasoning-tokens"));
    }

    #[test]
    fn explicit_thinking_budget_wins_and_is_validated() {
        let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"full",
                               "thinking_budget_tokens":200});
        normalize_chat(false, &mut value).unwrap();
        assert_eq!(value["thinking_budget_tokens"], 200);
        assert!(
            value.get("max_tokens").is_none()
                && engine_chat_body(&value, "llama_cpp")
                    .get("max_tokens")
                    .is_none()
        );
        for bad in [json!(-1), json!("4k"), json!(40000)] {
            let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"low","thinking_budget_tokens":bad});
            assert!(normalize_chat(false, &mut value).is_err());
        }
        let mut zero = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"full","thinking_budget_tokens":0});
        assert!(
            normalize_chat(false, &mut zero).is_err(),
            "a zero budget with thinking on is refused"
        );
        let mut zero_off = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"off","thinking_budget_tokens":0});
        assert!(normalize_chat(false, &mut zero_off).is_ok());
        let mut off = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"off","thinking_budget_tokens":100});
        assert!(normalize_chat(false, &mut off).is_err());
    }

    #[test]
    fn a_model_that_leaks_with_thinking_off_needs_temperature_zero() {
        for body in [
            json!({"model":"m","messages":[],"reasoning_effort":"off"}),
            json!({"model":"m","messages":[],"reasoning_effort":"off","temperature":0.7}),
        ] {
            let mut value = body.clone();
            assert!(normalize_chat(true, &mut value).is_err(), "{body}");
            let mut value = body;
            assert!(normalize_chat(false, &mut value).is_ok());
        }
        let mut zero = json!({"model":"m","messages":[],"reasoning_effort":"off","temperature":0});
        assert!(normalize_chat(true, &mut zero).is_ok());
    }

    #[test]
    fn conflicting_reasoning_controls_fail() {
        let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"off",
                               "chat_template_kwargs":{"enable_thinking":true}});
        assert!(normalize_chat(false, &mut value).is_err());
        let mut bad = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"max"});
        assert!(normalize_chat(false, &mut bad).is_err());
    }

    fn watched(watch: &mut Watch, body: &str, cut: usize) -> String {
        let mut out = Vec::new();
        for piece in body.as_bytes().chunks(cut) {
            out.extend_from_slice(&watch.pass(axum::body::Bytes::copy_from_slice(piece)));
        }
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_usage_only_chunk_is_dropped_whole_across_any_chunk_boundary() {
        let content = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}],\"usage\":null}\n\n";
        let usage =
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n";
        let body = format!("{content}{usage}data: [DONE]\n\n");
        for cut in 1..body.len() {
            let mut watch = Watch::new("text/event-stream", true, None);
            let mut out = watched(&mut watch, &body, cut);
            let (_, reported, rest) = watch.end();
            out.push_str(std::str::from_utf8(&rest).unwrap());
            assert_eq!(out, format!("{content}data: [DONE]\n\n"), "cut {cut}");
            assert_eq!(reported.unwrap().usage.unwrap()["completion_tokens"], 2);
            // A client that asked for usage gets every byte, unchanged.
            let mut watch = Watch::new("text/event-stream; charset=utf-8", false, None);
            assert_eq!(watched(&mut watch, &body, cut), body);
            assert_eq!(watch.end().1.unwrap().usage.unwrap()["prompt_tokens"], 3);
        }
    }

    #[test]
    fn usage_on_a_content_chunk_is_kept_and_a_stream_without_usage_reports_none() {
        let last = "data: {\"choices\":[{\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1}}\r\n\r\n";
        let mut watch = Watch::new("text/event-stream", true, None);
        assert_eq!(watched(&mut watch, last, 7), last);
        assert_eq!(watch.end().1.unwrap().usage.unwrap()["prompt_tokens"], 5);
        let mut watch = Watch::new("text/event-stream", true, None);
        watched(&mut watch, "data: {\"choices\":[]}\n\ndata: [DONE]\n\n", 3);
        assert!(watch.end().1.unwrap().usage.is_none());
        let mut watch = Watch::new("application/json", false, None);
        watched(
            &mut watch,
            "{\"data\":[],\"usage\":{\"prompt_tokens\":4}}",
            5,
        );
        assert_eq!(watch.end().1.unwrap().usage.unwrap()["prompt_tokens"], 4);
        // A top-level array or string passes, and reports no usage, without a panic.
        for body in ["[{\"prompt_tokens\":9}]", "[]", "\"text\"", " 42"] {
            let mut watch = Watch::new("application/json", false, None);
            assert_eq!(watched(&mut watch, body, 2), body);
            let (metered, usage, _) = watch.end();
            assert!(metered && usage.is_none(), "{body}");
        }
        // Audio carries no token usage: completed, but not metered.
        let mut watch = Watch::new("audio/wav", false, None);
        watched(&mut watch, "RIFF....WAVE", 4);
        assert!(!watch.end().0);
    }

    #[test]
    fn unit_fields_of_an_unexpected_shape_never_lose_the_usage() {
        let body = r#"{"data":{"not":"a list"},"answers":{"a":{},"b":{},"c":{}},"ranked_ids":"x",
            "horizon":12,"audio_duration_seconds":2.5,"usage":{"input_tokens":7}}"#;
        let mut watch = Watch::new("application/json", false, None);
        watched(&mut watch, body, 9);
        let reply = watch.end().1.unwrap();
        assert_eq!(reply.usage.unwrap()["input_tokens"], 7);
        assert_eq!(
            (reply.data, reply.answers, reply.ranked_ids),
            (None, Some(3), None)
        );
        assert_eq!(
            (reply.horizon, reply.audio_duration_seconds),
            (Some(12), Some(2.5))
        );
        // Only a body that is not RIFF at all falls back to Server-Timing's audio duration; a WAV
        // cut off before its data chunk has no seconds.
        let mut watch = Watch::new("audio/wav", false, Some(1.5));
        watched(&mut watch, "ID3\u{4}not a riff body", 4);
        assert_eq!(watch.end().1.unwrap().audio_seconds, Some(1.5));
        let mut watch = Watch::new("audio/wav", false, Some(1.5));
        watched(&mut watch, "RIFF....WAVE", 4);
        assert_eq!(watch.end().1.unwrap().audio_seconds, None);
    }

    #[test]
    fn application_errors_survive_chunk_boundaries_and_are_never_stripped() {
        for event in [
            "data: {\"error\":{\"message\":\"failed\"}}\n\n",
            "event: error\ndata: {\"message\":\"failed\"}\n\n",
            "data: {\"type\":\"error\"}\n\n",
        ] {
            for strip in [true, false] {
                for cut in 1..event.len() {
                    let mut watch = Watch::new("text/event-stream", strip, None);
                    assert_eq!(watched(&mut watch, event, cut), event);
                    assert!(matches!(watch, Watch::Events(events) if events.failed));
                }
            }
        }
    }

    #[test]
    fn an_event_past_the_bound_passes_through_and_parsing_resumes_after_it() {
        let usage = "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3}}\n\n";
        let big = format!("data: {}", "x".repeat(2 * super::SSE_EVENT_BYTES));
        let body = format!("{big}\n\n{usage}data: [DONE]\n\n");
        for cut in [1000, 4096, 70_000] {
            for strip in [true, false] {
                let mut watch = Watch::new("text/event-stream", strip, None);
                let mut out = watched(&mut watch, &body, cut);
                let (_, reported, rest) = watch.end();
                out.push_str(std::str::from_utf8(&rest).unwrap());
                // The large event arrives whole and in order; the usage event after it is still
                // read, and still dropped for a client that did not ask for it.
                let expected = if strip {
                    format!("{big}\n\ndata: [DONE]\n\n")
                } else {
                    body.clone()
                };
                assert_eq!(out, expected, "cut {cut} strip {strip}");
                assert_eq!(reported.unwrap().usage.unwrap()["prompt_tokens"], 3);
            }
        }
    }

    #[test]
    fn an_event_past_the_bound_that_never_ends_passes_every_byte_and_reports_no_usage() {
        let first = "data: {\"choices\":[{\"delta\":{}}]}\n\n";
        let body = format!("{first}data: {}", "x".repeat(2 * super::SSE_EVENT_BYTES));
        for strip in [true, false] {
            let mut watch = Watch::new("text/event-stream", strip, None);
            let mut out = watched(&mut watch, &body, 4096);
            let (metered, reported, rest) = watch.end();
            out.push_str(std::str::from_utf8(&rest).unwrap());
            assert_eq!(out, body, "strip {strip}");
            assert!(
                metered && reported.unwrap().usage.is_none(),
                "strip {strip}"
            );
        }
    }

    #[test]
    fn a_stream_that_ends_inside_an_event_keeps_its_last_bytes() {
        let body = "data: {\"choices\":[{\"delta\":{}}]}\n\ndata: {\"choices\":[{\"del";
        let mut watch = Watch::new("text/event-stream", true, None);
        let mut out = watched(&mut watch, body, 5);
        out.push_str(std::str::from_utf8(&watch.end().2).unwrap());
        assert_eq!(out, body);
    }
}
