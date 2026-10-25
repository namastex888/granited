use crate::{
    contracts,
    lifecycle::{Failure, Service, failure},
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
            "invalid_schema" | "invalid_operation" => StatusCode::BAD_REQUEST,
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
        .route("/admin/operations/{id}/cancel",post(cancel)).route("/internal/hardware",get(hardware)).route("/internal/hosts",get(hosts))
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
    Json(
        json!({"object":"list","data":s.models.values().map(|m|json!({"id":m.id,"object":"model","created":0,"owned_by":"ibm-granite","revision":m.hf_revision,"family":m.family,"capabilities":m.capabilities,"deployment_configured":s.workers.contains_key(&m.id)||s.config.peers.iter().any(|p|p.models.contains(&m.id))})).collect::<Vec<_>>()}),
    )
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
    let id = s.resolve_model(value["model"].as_str().unwrap()).to_owned();
    value["model"] = json!(id);
    let Some(model) = s.models.get(&id) else {
        return failure("unknown_model", id).into_response();
    };
    let capability = contracts::capability(path).unwrap_or("");
    if !model.capabilities.iter().any(|c| c == capability) {
        return failure(
            "unsupported_capability",
            format!("{} does not declare {capability}", model.id),
        )
        .into_response();
    }
    if model.family == "switch"
        && path == "/v1/chat/completions"
        && value["chat_template_kwargs"]["adapter_name"]
            .as_str()
            .is_none()
    {
        return failure(
            "invalid_schema",
            "Switch requires explicit chat_template_kwargs.adapter_name",
        )
        .into_response();
    }
    if path == "/v1/chat/completions"
        && model.family == "language"
        && let Err(e) = normalize_chat(&id, &mut value)
    {
        return failure("invalid_schema", e).into_response();
    }
    let start = Instant::now();
    let request_id = headers
        .get("x-granite-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let thread = headers
        .get("x-granite-thread")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("main");
    if !matches!(thread, "main" | "background" | "decider") {
        return failure(
            "invalid_schema",
            "x-granite-thread must be main, background or decider",
        )
        .into_response();
    }
    if !s.workers.contains_key(&id) && !s.config.peers.iter().any(|p| p.models.contains(&id)) {
        return failure(
            "model_unavailable",
            format!("{id} is discoverable; no validated native deployment is configured"),
        )
        .into_response();
    }
    if path == "/v1/systemone" {
        value["cache_prompt"] = json!(false);
    }
    let candidates = match s.placement(&id, capability, thread == "background").await {
        Ok(candidates) => candidates,
        Err(e) => {
            s.count_rejection(&id, &e.code);
            return e.into_response();
        }
    };
    let attempts = candidates.len();
    let mut outcome = None;
    for (attempt, placement) in candidates.into_iter().enumerate() {
        // Placement reads snapshots up to 2 s old; a host that filled up meanwhile refuses before
        // running anything, so the next-ranked host may take the request.
        let fallback = attempt + 1 < attempts;
        let local = placement.is_none();
        let (url, token, lease, revision, backend) = if local
            && let Some(worker) = s.workers.get(&id)
        {
            let lease = match s.lease(&id, thread == "background").await {
                Ok(l) => l,
                Err(e) => {
                    s.count_rejection(&id, &e.code);
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
        let mut request = s
            .client
            .post(format!("{url}{forward_path}"))
            .json(&value)
            .header("x-granite-request-id", request_id)
            .header("x-granite-thread", thread)
            .timeout(Duration::from_secs(600));
        if let Some((token, hop)) = token {
            request = request.bearer_auth(token).header("x-granite-hop", hop);
        }
        let response = match request.send().await {
            Ok(upstream) if !local && fallback && refused_before_running(&upstream) => {
                let code = upstream
                    .headers()
                    .get("x-granite-error-code")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("admission_busy");
                s.count_rejection(&id, code);
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
            Ok(upstream) => forward_response(upstream, lease).await,
            Err(e) => failure("backend_unreachable", e.to_string()).into_response(),
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
        .insert("x-granite-model-alias", HeaderValue::from_str(&id).unwrap());
    if !response.headers().contains_key("x-granite-host") {
        response.headers_mut().insert(
            "x-granite-host",
            HeaderValue::from_str(&s.config.host_id).unwrap(),
        );
    }
    if local && let Some(worker) = s.workers.get(&id) {
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
        "server-timing",
    ] {
        if let Some(value) = headers.get(name) {
            response.headers_mut().insert(name, value.clone());
        }
    }
    response
}

fn normalize_chat(id: &str, value: &mut Value) -> Result<(), String> {
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
    if thinking && value.get("grammar").is_some() {
        return Err("GBNF grammar with thinking can consume the answer inside reasoning; use response_format.json_schema or explicitly disable thinking".into());
    }
    if id == "granite-4.2-3b"
        && !thinking
        && value.get("temperature").and_then(Value::as_f64) != Some(0.0)
    {
        return Err("Granite 4.2 3B non-thinking requires explicit temperature=0 because visible reasoning leaked at higher temperatures in prior probes".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{equal, normalize_chat, refused_before_running};
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
        normalize_chat("granite-4.2-8b", &mut value).unwrap();
        assert_eq!(
            value["chat_template_kwargs"],
            json!({"enable_thinking":true,"low_effort":true})
        );
        assert!(value.get("reasoning_effort").is_none());
    }

    #[test]
    fn conflicting_reasoning_controls_fail() {
        let mut value = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"off",
                               "chat_template_kwargs":{"enable_thinking":true}});
        assert!(normalize_chat("granite-4.2-8b", &mut value).is_err());
        let mut bad = json!({"model":"granite-4.2-8b","messages":[],"reasoning_effort":"max"});
        assert!(normalize_chat("granite-4.2-8b", &mut bad).is_err());
    }
}
