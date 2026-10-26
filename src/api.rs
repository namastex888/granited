use crate::{
    contracts,
    lifecycle::{Failure, HostClass, LoadMode, Service, failure},
};
use axum::{
    Json, Router,
    body::Body,
    extract::{
        DefaultBodyLimit, FromRequest, Multipart, OriginalUri, Path, Request, State,
        rejection::JsonRejection,
    },
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
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
            "unknown_model" | "unknown_operation" | "route_not_found" => StatusCode::NOT_FOUND,
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
        .route("/v1/audio/transcriptions",post(audio));
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
        if modality == Modality::Transcription {
            // User audio is transcribed on this node only; it never leaves for a peer.
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
fn resolve_auto(
    s: &Service,
    path: &str,
    headers: &axum::http::HeaderMap,
) -> Result<String, Failure> {
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
    let mut values =
        vec![json!({"host":s.config.host_id,"reachable":true,"hardware":s.hardware().await})];
    // Probe peers concurrently so one unreachable node does not serialize the whole report.
    values.extend(
        futures_util::future::join_all(s.config.peers.iter().map(|peer| async {
            let started = Instant::now();
            let result = s
                .client
                .get(format!("{}/internal/hardware", peer.url))
                .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
                .timeout(Duration::from_secs(5))
                .send()
                .await;
            match result {
                Ok(r) if r.status().is_success() => match r.json::<Value>().await {
                    Ok(hw)=>json!({"host":peer.id,"reachable":true,"round_trip_ms":started.elapsed().as_secs_f64()*1000.,"hardware":hw}),
                    Err(e)=>json!({"host":peer.id,"reachable":false,"error":e.to_string()}),
                },
                Ok(r)=>json!({"host":peer.id,"reachable":false,"status":r.status().as_u16()}),
                Err(e)=>json!({"host":peer.id,"reachable":false,"error":e.to_string()}),
            }
        }))
        .await,
    );
    Json(json!({"hosts":values}))
}
async fn metrics(State(s): State<Arc<Service>>) -> Json<Value> {
    Json(
        json!({"uptime_seconds":s.started.elapsed().as_secs(),"workers":s.worker_status(),"operations":s.operations.lock().unwrap().values().cloned().collect::<Vec<_>>(),
            "rejections":*s.rejections.lock().unwrap()}),
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
            Ok(r) => forward_response(r, None).await,
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
            return forward_response(r, None).await;
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
    let start = Instant::now();
    if s.config.auto.enabled && value["model"] == "auto" {
        if path == "/v1/chat/completions" && auto_capabilities(&s).contains(&"chat") {
            return auto_entry(&s, path, value, headers, start).await;
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
                match parsed {
                    Ok(output)=>Json(json!({"text":output["choices"][0]["message"]["content"],"usage":output["usage"],"model":id,"revision":revision,"structural_quality":null})).into_response(),
                    Err(e)=>failure("backend_error",e.to_string()).into_response(),
                }
            }
            Ok(upstream) if count_reasoning && upstream.status().is_success() => {
                let parsed = upstream.json::<Value>().await;
                let response = match parsed {
                    Ok(output) => reasoning_usage(&s.client, &url, output).await,
                    Err(e) => failure("backend_error", e.to_string()).into_response(),
                };
                drop(lease);
                response
            }
            Ok(upstream) => forward_response(upstream, lease).await,
            Err(e) => {
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

async fn forward_response(
    upstream: reqwest::Response,
    lease: Option<crate::lifecycle::Lease>,
) -> Response {
    let status = upstream.status();
    let headers = upstream.headers().clone();
    if !status.is_success() {
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
    let stream = futures_util::stream::unfold(
        (upstream.bytes_stream(), lease),
        |(mut stream, lease)| async move { stream.next().await.map(|chunk| (chunk, (stream, lease))) },
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
async fn reasoning_usage(client: &reqwest::Client, url: &str, mut output: Value) -> Response {
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
    let mut response = Json(output).into_response();
    if count.is_some() {
        response.headers_mut().insert(
            "x-granite-reasoning-tokens",
            HeaderValue::from_static("retokenized"),
        );
    }
    response
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
    use super::{engine_chat_body, equal, normalize_chat, reasoning_usage, refused_before_running};
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
        let (headers, value) =
            body(reasoning_usage(&client, &url, reply("one two three")).await).await;
        assert_eq!(
            value["usage"]["completion_tokens_details"]["reasoning_tokens"],
            3
        );
        assert_eq!(headers["x-granite-reasoning-tokens"], "retokenized");
        let (_, value) = body(reasoning_usage(&client, &url, reply("")).await).await;
        assert_eq!(
            value["usage"]["completion_tokens_details"]["reasoning_tokens"],
            0
        );
        // An engine without /tokenize leaves usage as the engine reported it.
        let (headers, value) =
            body(reasoning_usage(&client, "http://127.0.0.1:9", reply("a b")).await).await;
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
}
