use crate::{
    config::{Config, Deployment, Model},
    hardware,
    process::OwnedProcess,
};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    process::Command,
    sync::{Mutex as AsyncMutex, Notify, OwnedSemaphorePermit, Semaphore},
};

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub code: String,
    pub message: String,
}
pub type Result<T> = std::result::Result<T, Failure>;
const PEER_FRESH: Duration = Duration::from_secs(2);
const PEER_BACKOFF: Duration = Duration::from_secs(15);
const ADMISSION_FRESH: Duration = Duration::from_secs(1);
const MAX_MEMORY_STALL_PERCENT: f64 = 5.0;

/// A load refused for lack of room says nothing about the model's health, so it must not put the
/// model in the failure cooldown that `score` and `load_cooldown` read.
fn is_capacity_refusal(code: &str) -> bool {
    matches!(
        code,
        "accelerator_busy"
            | "insufficient_memory"
            | "insufficient_vram"
            | "memory_pressure"
            | "admission_busy"
            | "model_not_warm"
    )
}

/// Scores at or above this are loaded workers with a free slot; below it, cold loads.
const WARM_SCORE: u64 = 1_000_000;

/// What one host can do for a model right now, for `model: "auto"`, which must never evict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostClass {
    /// Ready with a free slot.
    Warm,
    /// Ready, every slot taken.
    Full,
    /// Local, cold, fits, and no ready model shares its exclusive accelerator.
    ColdFree,
    /// Local and cold; loading it would evict a ready model.
    WouldDisplace,
    /// Memory, VRAM or memory pressure rule the load out, or a peer is not warm.
    NoFit,
    /// Held, not deployed there, or an operator-owned model that is not loaded.
    Held,
    /// Failed less than 30 s ago.
    Cooldown,
    /// A peer whose hardware snapshot failed.
    Unreachable,
}

/// The class of one host from its hardware report, by the same rules as `score`. A cold host is
/// `ColdFree` here; the caller decides whether loading it would displace another model.
fn classify(hw: &Value, model: &str, capability: &str, background: bool) -> HostClass {
    let Some(worker) = hw["managed_workers"]
        .as_array()
        .and_then(|w| w.iter().find(|w| w["model"] == model))
    else {
        return HostClass::Held;
    };
    let disabled = worker["enabled_capabilities"]
        .as_array()
        .is_some_and(|caps| !caps.iter().any(|c| c == capability));
    if worker["hold_reason"].is_string()
        || disabled
        || (worker["external"] == true
            && worker["external_lifecycle"] != true
            && worker["state"] != "ready")
    {
        return HostClass::Held;
    }
    match score(hw, model, capability, background) {
        Some(value) if value >= WARM_SCORE => HostClass::Warm,
        Some(_) => HostClass::ColdFree,
        None if worker["state"] == "ready" || worker["state"] == "loading" => HostClass::Full,
        None if worker["last_error"].is_object()
            && worker["idle_seconds"].as_u64().unwrap_or(0) < 30 =>
        {
            HostClass::Cooldown
        }
        None => HostClass::NoFit,
    }
}

/// Rank one host for a request from its hardware report; `None` means it cannot take the request.
fn score(hw: &Value, model: &str, capability: &str, background: bool) -> Option<u64> {
    let worker = hw["managed_workers"]
        .as_array()?
        .iter()
        .find(|w| w["model"] == model)?;
    if worker["hold_reason"].is_string() {
        return None;
    }
    if worker["external"] == true
        && worker["external_lifecycle"] != true
        && worker["state"] != "ready"
    {
        return None;
    }
    if let Some(caps) = worker["enabled_capabilities"].as_array()
        && !caps.iter().any(|c| c == capability)
    {
        return None;
    }
    // Held slots, not served requests: a slot waiting on a cold load is already taken.
    let active = worker["occupied_slots"]
        .as_u64()
        .or_else(|| worker["active_requests"].as_u64())?;
    let slots = worker["max_concurrency"].as_u64()?;
    if active >= slots || (background && active >= slots.saturating_sub(1)) {
        return None;
    }
    if worker["state"] == "ready" {
        return Some(WARM_SCORE + (slots - active) * 100);
    }
    if worker["last_error"].is_object() && worker["idle_seconds"].as_u64().unwrap_or(0) < 30 {
        return None;
    }
    if hw["memory_stall_percent"].as_f64().unwrap_or(0.0) > MAX_MEMORY_STALL_PERCENT {
        return None;
    }
    let budget = worker["reserved_peak_bytes"].as_u64()?;
    let reserve = hw["admission"]["ram_reserve_bytes"].as_u64()?;
    let available = hw["ram"]["available_bytes"].as_u64()?;
    if available < budget.saturating_add(reserve) {
        return None;
    }
    if worker["device"] == "cuda" {
        let free = hw["accelerators"]
            .as_array()?
            .iter()
            .find(|g| g["vendor"] == "nvidia")?["available_memory_bytes"]
            .as_u64()?;
        if free < budget.saturating_add(hw["admission"]["accelerator_reserve_bytes"].as_u64()?) {
            return None;
        }
    }
    Some(100 + (available - budget - reserve) / (1024 * 1024 * 1024))
}

/// How far a lease may go to make a model ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadMode {
    /// Load, evicting idle models when needed (explicit model ids).
    Normal,
    /// Load only if that evicts nothing (`auto` on a cold host).
    NoEvict,
    /// Never load; serve only a model that is already ready (`auto` on a warm host or a peer).
    WarmOnly,
}

impl LoadMode {
    /// The mode a peer asked for with `x-granite-load`.
    pub fn from_header(value: Option<&str>) -> Self {
        match value {
            Some("warm-only") => Self::WarmOnly,
            Some("no-evict") => Self::NoEvict,
            _ => Self::Normal,
        }
    }
}

pub fn failure(code: &str, message: impl Into<String>) -> Failure {
    Failure {
        code: code.into(),
        message: message.into(),
    }
}

pub struct Runtime {
    pub state: &'static str,
    pub last_error: Option<Failure>,
    pub failed_at: Option<Instant>,
    pub loaded_ms: Option<u128>,
    pub unloaded_ms: Option<u128>,
    pub last_used: Instant,
    pub native_port: Option<u16>,
    /// The context one request gets, as the engine reported it at load.
    pub context_tokens: Option<u64>,
}

pub struct Worker {
    pub spec: Deployment,
    pub runtime: Mutex<Runtime>,
    pub active: AtomicUsize,
    pub slots: Arc<Semaphore>,
    pub background_slots: Arc<Semaphore>,
    pub process: Arc<Mutex<Option<OwnedProcess>>>,
    pub verified_files: Mutex<BTreeMap<String, (u64, std::time::SystemTime)>>,
}

pub struct Lease {
    pub worker: Arc<Worker>,
    _permit: OwnedSemaphorePermit,
    _background_permit: Option<OwnedSemaphorePermit>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.worker.runtime.lock().unwrap().last_used = Instant::now();
        self.worker.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Serialize)]
pub struct Operation {
    pub id: String,
    pub model: String,
    pub action: String,
    pub status: String,
    pub elapsed_ms: Option<u128>,
    pub error: Option<Failure>,
    #[serde(skip)]
    pub cancel: Arc<Notify>,
}

pub struct Service {
    pub config: Config,
    pub models: BTreeMap<String, Model>,
    pub workers: BTreeMap<String, Arc<Worker>>,
    pub client: reqwest::Client,
    pub lifecycle: AsyncMutex<()>,
    pub operations: Mutex<BTreeMap<String, Operation>>,
    pub hardware: AsyncMutex<(Instant, Value)>,
    /// `None` records a failed probe so an unreachable peer is skipped until the backoff expires.
    pub peer_hardware: AsyncMutex<BTreeMap<String, (Instant, Option<Value>)>>,
    pub started: Instant,
    /// Requests refused before they ran, by model and error code (`/internal/metrics`).
    pub rejections: Mutex<BTreeMap<String, BTreeMap<String, u64>>>,
    /// The model each `x-granite-conversation` was served by, for `auto`.
    pub conversations: Mutex<localfleet_route::Conversations>,
}

impl Service {
    pub fn count_rejection(&self, model: &str, code: &str) {
        *self
            .rejections
            .lock()
            .unwrap()
            .entry(model.to_owned())
            .or_default()
            .entry(code.to_owned())
            .or_default() += 1;
    }

    pub fn resolve_model<'a>(&'a self, id: &'a str) -> &'a str {
        if self.models.contains_key(id) {
            return id;
        }
        self.models
            .values()
            .find(|m| {
                m.metadata
                    .get("aliases")
                    .and_then(Value::as_array)
                    .is_some_and(|aliases| aliases.iter().any(|a| a.as_str() == Some(id)))
            })
            .map(|m| m.id.as_str())
            .unwrap_or(id)
    }

    /// Rank available warm workers first, then cold workers that fit measured memory. The chosen
    /// node still enforces admission; a request is retried on the next candidate only when it was
    /// refused before running, never after execution.
    pub async fn placement(
        &self,
        model: &str,
        capability: &str,
        background: bool,
    ) -> Result<Vec<Option<crate::config::Peer>>> {
        let mut candidates = Vec::new();
        if self.workers.contains_key(model)
            && let Some(value) = score(&self.hardware().await, model, capability, background)
        {
            candidates.push((value, None));
        }
        let peers = self
            .config
            .peers
            .iter()
            .filter(|p| p.models.iter().any(|m| m == model));
        let results = futures_util::future::join_all(peers.map(|peer| async move {
            score(
                &self.peer_snapshot(peer).await?,
                model,
                capability,
                background,
            )
            .map(|value| (value, Some(peer.clone())))
        }))
        .await;
        candidates.extend(results.into_iter().flatten());
        candidates.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        if candidates.is_empty() {
            return Err(failure(
                "capacity_unavailable",
                "No reachable deployment has an available slot and measured memory capacity; inspect /internal/hosts",
            ));
        }
        Ok(candidates.into_iter().map(|(_, peer)| peer).collect())
    }

    /// Mark a peer unreachable now, so placement skips it for `PEER_BACKOFF` instead of trusting
    /// a snapshot taken before it died.
    pub async fn forget_peer(&self, id: &str) {
        self.peer_hardware
            .lock()
            .await
            .insert(id.to_owned(), (Instant::now(), None));
    }

    /// A peer's hardware report, cached for `PEER_FRESH`; `None` while it is unreachable.
    async fn peer_snapshot(&self, peer: &crate::config::Peer) -> Option<Value> {
        let cached = self.peer_hardware.lock().await.get(&peer.id).cloned();
        match cached {
            Some((at, hw @ Some(_))) if at.elapsed() < PEER_FRESH => return hw,
            Some((at, None)) if at.elapsed() < PEER_BACKOFF => return None,
            _ => {}
        }
        let hardware = match self
            .client
            .get(format!("{}/internal/hardware", peer.url))
            .bearer_auth(std::env::var(&peer.token_env).unwrap_or_default())
            .timeout(Duration::from_secs(3))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
            _ => None,
        };
        self.peer_hardware
            .lock()
            .await
            .insert(peer.id.clone(), (Instant::now(), hardware.clone()));
        hardware
    }

    /// Every host of `model` with its class, local first, for `auto`. A local cold host is
    /// `ColdFree` only when no ready or loading model shares its exclusive accelerator; a peer
    /// counts only when warm, because its snapshot cannot show what its load would evict.
    pub async fn auto_hosts(
        &self,
        model: &str,
        capability: &str,
        background: bool,
    ) -> Vec<(Option<crate::config::Peer>, HostClass)> {
        let mut hosts = Vec::new();
        if let Some(target) = self.workers.get(model) {
            let mut class = classify(&self.hardware().await, model, capability, background);
            if class == HostClass::ColdFree && self.would_displace(target) {
                class = HostClass::WouldDisplace;
            }
            hosts.push((None, class));
        }
        for peer in self
            .config
            .peers
            .iter()
            .filter(|p| p.models.iter().any(|m| m == model))
        {
            let class = match self.peer_snapshot(peer).await {
                None => HostClass::Unreachable,
                Some(hw) => match classify(&hw, model, capability, background) {
                    HostClass::ColdFree | HostClass::WouldDisplace => HostClass::NoFit,
                    class => class,
                },
            };
            hosts.push((Some(peer.clone()), class));
        }
        hosts
    }

    /// `GET /v1/devices`: each host of the fleet, and per capability the state of every model
    /// it deploys, in placement's words (`warm`, `cold_fits`, `no_room`, `held`), from the same
    /// snapshots placement reads. `recommended` is the best registry `tier` that fits, preferring
    /// warm, so every client draws the same conclusion. No hostname or address is included.
    pub async fn devices(&self) -> Value {
        const TIERS: [&str; 3] = ["good_gpu", "gpu_poor", "cpu"];
        let mut hosts = vec![(
            self.config.host_id.clone(),
            true,
            Some(self.hardware().await),
        )];
        for peer in &self.config.peers {
            hosts.push((peer.id.clone(), false, self.peer_snapshot(peer).await));
        }
        let mut devices = Vec::new();
        for (id, local, hw) in hosts {
            let Some(hw) = hw else {
                devices.push(json!({"device": id, "local": local, "reachable": false}));
                continue;
            };
            let workers = hw["managed_workers"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut by_capability: BTreeMap<String, Vec<Value>> = BTreeMap::new();
            for worker in &workers {
                let Some(model) = worker["model"].as_str().and_then(|m| self.models.get(m)) else {
                    continue;
                };
                for capability in &model.capabilities {
                    let mut class = classify(&hw, &model.id, capability, false);
                    if local
                        && class == HostClass::ColdFree
                        && self
                            .workers
                            .get(&model.id)
                            .is_some_and(|w| self.would_displace(w))
                    {
                        class = HostClass::WouldDisplace;
                    }
                    let state = match class {
                        HostClass::Warm | HostClass::Full => "warm",
                        HostClass::ColdFree => "cold_fits",
                        HostClass::Held => "held",
                        _ => "no_room",
                    };
                    let mut entry = json!({"model": model.id, "state": state});
                    if let Some(tier) = model.metadata.get("tier") {
                        entry["tier"] = tier.clone();
                    }
                    if let Some(bytes) = worker["reserved_peak_bytes"].as_u64() {
                        entry["budget_gib"] =
                            json!((bytes as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0);
                    }
                    if let Some(ms) = worker["load_ms"].as_u64() {
                        entry["load_s"] = json!((ms as f64 / 100.0).round() / 10.0);
                    }
                    by_capability
                        .entry(capability.clone())
                        .or_default()
                        .push(entry);
                }
            }
            let mut recommended = serde_json::Map::new();
            for (capability, entries) in &by_capability {
                let rank = |e: &Value| {
                    let tier = TIERS.iter().position(|t| e["tier"] == *t)?;
                    let fits = matches!(e["state"].as_str(), Some("warm" | "cold_fits"));
                    fits.then_some((tier, e["state"] != "warm"))
                };
                if let Some(best) = entries
                    .iter()
                    .filter_map(|e| Some((rank(e)?, e)))
                    .min_by_key(|(r, _)| *r)
                {
                    recommended.insert(
                        capability.clone(),
                        json!({"model": best.1["model"], "why": "best tier that fits, warm first"}),
                    );
                } else if entries.iter().any(|e| e.get("tier").is_some()) {
                    recommended.insert(
                        capability.clone(),
                        json!({"model": null, "why": "no tier fits on this device now"}),
                    );
                }
            }
            let accelerator = hw["accelerators"].get(0).map(|a| json!({"vendor": a["vendor"], "name": a["name"], "memory_kind": a["memory_kind"]}));
            devices.push(json!({
                "device": id, "local": local, "reachable": true, "accelerator": accelerator,
                "memory_free_gib": hw["ram"]["available_bytes"].as_u64().map(|b| (b as f64 / (1u64 << 30) as f64 * 10.0).round() / 10.0),
                "capabilities": by_capability, "recommended": recommended,
            }));
        }
        json!({"devices": devices})
    }

    /// Whether loading `target` would have to evict: `room()`'s exclusive-accelerator test.
    /// Models that share one llama.cpp router address (`--models-max 1`) swap inside it, on any
    /// device, so a ready sibling there counts as exclusive too.
    fn would_displace(&self, target: &Worker) -> bool {
        self.workers.values().any(|w| {
            let exclusive_device = target.spec.device != "cpu"
                && !target.spec.shared_accelerator
                && w.spec.device == target.spec.device
                && !w.spec.shared_accelerator;
            w.spec.model != target.spec.model
                && (exclusive_device || w.spec.base_url == target.spec.base_url)
                && matches!(w.runtime.lock().unwrap().state, "ready" | "loading")
        })
    }

    /// Re-measure local hardware and, when asked, operator-owned native routers. The background
    /// refresher and admission call this; request placement only reads the last snapshot.
    pub async fn refresh_hardware(&self, sync_external: bool) {
        if let Ok(value) = tokio::task::spawn_blocking(hardware::collect).await {
            *self.hardware.lock().await = (Instant::now(), value);
        }
        if !sync_external {
            return;
        }
        let mut checked = std::collections::BTreeSet::new();
        for worker in self.workers.values().filter(|w| w.spec.external) {
            if checked.insert(&worker.spec.base_url) {
                let _ = self.sync_native_status(worker).await;
            }
        }
    }

    pub async fn hardware(&self) -> Value {
        let mut value = self.hardware.lock().await.1.clone();
        value["host_id"] = json!(self.config.host_id);
        value["managed_workers"] = self.worker_status();
        value["admission"] = json!({"ram_reserve_bytes":self.config.ram_reserve_bytes,"accelerator_reserve_bytes":self.config.accelerator_reserve_bytes});
        value
    }

    pub fn worker_status(&self) -> Value {
        json!(self.workers.iter().map(|(id, w)| {
            let mut rt = w.runtime.lock().unwrap();
            let mut process = w.process.lock().unwrap();
            if let Some(child) = &mut *process
                && child.try_wait().ok().flatten().is_some() {
                *process = None;
                rt.state = "failed";
                rt.failed_at = Some(Instant::now());
                rt.last_error = Some(failure("worker_exited", "Native worker exited; next attempt observes cooldown."));
            }
            json!({"model":id, "state":rt.state, "pid":process.as_ref().and_then(OwnedProcess::id),
                "active_requests":w.active.load(Ordering::SeqCst), "max_concurrency":w.spec.max_concurrency,
                "occupied_slots":w.spec.max_concurrency.saturating_sub(w.slots.available_permits()),
                "external":w.spec.external,"external_lifecycle":w.spec.external_lifecycle,
                "enabled_capabilities":w.spec.enabled_capabilities,"backend":w.spec.backend,
                "revision":w.spec.revision, "device":w.spec.device, "reserved_peak_bytes":w.spec.memory_bytes,
                "memory_budget_kind":"configured_conservative_bound", "load_ms":rt.loaded_ms,"hold_reason":w.spec.hold_reason,
                "deployment_context_tokens":w.spec.context_tokens,
                "unload_ms":rt.unloaded_ms, "last_error":rt.last_error,
                "idle_seconds":rt.last_used.elapsed().as_secs()})
        }).collect::<Vec<_>>())
    }

    async fn room(&self, target: &Worker) -> Result<()> {
        if self.hardware.lock().await.0.elapsed() > ADMISSION_FRESH {
            self.refresh_hardware(true).await;
        }
        let hw = self.hardware().await;
        if let Some(stall) = hw["memory_stall_percent"].as_f64()
            && stall > MAX_MEMORY_STALL_PERCENT
        {
            return Err(failure(
                "memory_pressure",
                format!(
                    "Host is stalling on memory ({stall:.1}% of the last 10 s); cold loads wait until it recovers"
                ),
            ));
        }
        let ram = hw["ram"]["available_bytes"]
            .as_u64()
            .ok_or_else(|| failure("hardware_unknown", "Available RAM probe failed"))?;
        if ram
            < target
                .spec
                .memory_bytes
                .saturating_add(self.config.ram_reserve_bytes)
        {
            return Err(failure(
                "insufficient_memory",
                format!(
                    "{} bytes available RAM; {} bytes model budget plus {} bytes OS reserve required",
                    ram, target.spec.memory_bytes, self.config.ram_reserve_bytes
                ),
            ));
        }
        if target.spec.device == "cuda" {
            let gpus: Vec<_> = hw["accelerators"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|d| d["vendor"] == "nvidia")
                .collect();
            if gpus.len() != 1 {
                return Err(failure(
                    "unsupported_hardware",
                    "Exactly one measured NVIDIA device required by this profile",
                ));
            }
            let free = gpus[0]["available_memory_bytes"]
                .as_u64()
                .ok_or_else(|| failure("hardware_unknown", "Available VRAM unknown"))?;
            if free
                < target
                    .spec
                    .memory_bytes
                    .saturating_add(self.config.accelerator_reserve_bytes)
            {
                return Err(failure(
                    "insufficient_vram",
                    format!(
                        "{free} bytes free VRAM; budget {} plus reserve {} required",
                        target.spec.memory_bytes, self.config.accelerator_reserve_bytes
                    ),
                ));
            }
        }
        if target.spec.device != "cpu"
            && !target.spec.shared_accelerator
            && self.workers.values().any(|w| {
                w.spec.model != target.spec.model
                    && w.spec.device == target.spec.device
                    && !w.spec.shared_accelerator
                    && w.runtime.lock().unwrap().state == "ready"
            })
        {
            return Err(failure(
                "accelerator_busy",
                "Accelerator profile permits one resident model",
            ));
        }
        Ok(())
    }

    async fn verify_artifacts(spec: Deployment) -> Result<()> {
        tokio::task::spawn_blocking(move || {
            for (path, expected) in &spec.artifacts {
                let mut file = std::fs::File::open(path)
                    .map_err(|e| failure("artifact_missing", format!("{path}: {e}")))?;
                let mut hasher = Sha256::new();
                let mut buffer = [0u8; 1024 * 1024];
                loop {
                    let count = file
                        .read(&mut buffer)
                        .map_err(|e| failure("artifact_read", e.to_string()))?;
                    if count == 0 {
                        break;
                    }
                    hasher.update(&buffer[..count]);
                }
                let actual: String = hasher
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                if actual != *expected {
                    return Err(failure(
                        "artifact_mismatch",
                        format!("SHA256 mismatch: {path}"),
                    ));
                }
            }
            Ok(())
        })
        .await
        .map_err(|e| failure("artifact_check", e.to_string()))?
    }

    async fn verify_worker_artifacts(worker: &Worker) -> Result<()> {
        let fingerprints = || -> Result<BTreeMap<String, (u64, std::time::SystemTime)>> {
            worker
                .spec
                .artifacts
                .keys()
                .map(|path| {
                    let m = std::fs::metadata(path)
                        .map_err(|e| failure("artifact_check", e.to_string()))?;
                    let modified = m
                        .modified()
                        .map_err(|e| failure("artifact_check", e.to_string()))?;
                    Ok((path.clone(), (m.len(), modified)))
                })
                .collect()
        };
        let before = fingerprints()?;
        if !before.is_empty() && *worker.verified_files.lock().unwrap() == before {
            return Ok(());
        }
        Self::verify_artifacts(worker.spec.clone()).await?;
        if before != fingerprints()? {
            return Err(failure(
                "artifact_changed",
                "Artifact changed during verification",
            ));
        }
        *worker.verified_files.lock().unwrap() = before;
        Ok(())
    }

    async fn stop_worker(&self, worker: &Worker) -> Result<()> {
        if worker.spec.external && !worker.spec.external_lifecycle {
            return Err(failure(
                "lifecycle_forbidden",
                "Operator-owned router is inference-only; model lifecycle is reserved",
            ));
        }
        {
            let mut runtime = worker.runtime.lock().unwrap();
            if worker.active.load(Ordering::SeqCst) > 0 {
                return Err(failure("model_busy", "Active requests hold this model"));
            }
            runtime.state = "unloading";
        }
        let start = Instant::now();
        self.cleanup_container(worker).await?;
        if worker.spec.backend == "llama_cpp" {
            if worker.spec.external || worker.process.lock().unwrap().is_some() {
                self.native_model_action(worker, "unload").await?;
            }
        } else {
            let child = worker.process.lock().unwrap().take();
            if let Some(mut child) = child {
                child
                    .terminate()
                    .await
                    .map_err(|e| failure("unload_failed", e.to_string()))?;
            }
        }
        let mut rt = worker.runtime.lock().unwrap();
        rt.state = "unloaded";
        rt.unloaded_ms = Some(start.elapsed().as_millis());
        rt.context_tokens = None;
        Ok(())
    }

    async fn load_worker(&self, worker: &Worker, mode: LoadMode) -> Result<()> {
        if let Some(reason) = &worker.spec.hold_reason {
            return Err(failure("hardware_reserved", reason));
        }
        if worker.spec.external {
            Self::verify_worker_artifacts(worker).await?;
            self.sync_native_status(worker).await?;
            let loaded = matches!(worker.runtime.lock().unwrap().state, "loading" | "ready");
            let started = Instant::now();
            if !loaded {
                if mode == LoadMode::WarmOnly {
                    return Err(failure(
                        "model_not_warm",
                        "auto serves only a loaded model here",
                    ));
                }
                if !worker.spec.external_lifecycle {
                    return Err(failure(
                        "hardware_reserved",
                        "Operator-owned model is unloaded; automatic activation is prohibited",
                    ));
                }
                self.room(worker).await?;
                self.native_model_action(worker, "load").await?;
            }
            // Props is queried only after verifying the model is loaded, avoiding implicit activation.
            let mut props_url = reqwest::Url::parse(&format!("{}/props", worker.spec.base_url))
                .map_err(|e| failure("worker_identity", e.to_string()))?;
            props_url.query_pairs_mut().append_pair(
                "model",
                worker
                    .spec
                    .backend_model
                    .as_deref()
                    .unwrap_or(&worker.spec.model),
            );
            let props = self
                .client
                .get(props_url)
                .timeout(Duration::from_secs(10))
                .send()
                .await
                .map_err(|e| failure("worker_identity", e.to_string()))?;
            if !props.status().is_success() {
                return Err(failure(
                    "model_loading",
                    "Native model has not become ready; retry later",
                ));
            }
            let props: Value = props
                .json()
                .await
                .map_err(|e| failure("worker_identity", e.to_string()))?;
            if !props["build_info"]
                .to_string()
                .contains(&worker.spec.backend_build)
            {
                return Err(failure(
                    "worker_identity",
                    format!(
                        "Native build differs from pinned {}",
                        worker.spec.backend_build
                    ),
                ));
            }
            self.record_context(worker).await;
            self.mark_loaded(worker, started);
            return Ok(());
        }
        if worker.spec.backend == "llama_cpp" && worker.process.lock().unwrap().is_some() {
            self.sync_native_status(worker).await?;
        }
        {
            let mut rt = worker.runtime.lock().unwrap();
            let mut process = worker.process.lock().unwrap();
            if let Some(child) = &mut *process {
                if child
                    .try_wait()
                    .map_err(|e| failure("worker_status", e.to_string()))?
                    .is_none()
                {
                    if rt.state == "ready" {
                        return Ok(());
                    }
                } else {
                    *process = None;
                    rt.failed_at = Some(Instant::now());
                    rt.state = "failed";
                }
            }
            if rt
                .failed_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
            {
                return Err(failure(
                    "load_cooldown",
                    "Load failed recently; retry after 30 seconds",
                ));
            }
        }
        Self::verify_worker_artifacts(worker).await?;
        if mode == LoadMode::Normal && self.room(worker).await.is_err() {
            let mut idle: Vec<_> = self
                .workers
                .values()
                .filter(|w| {
                    w.spec.model != worker.spec.model
                        && !w.spec.keep_warm
                        && (!w.spec.external || w.spec.external_lifecycle)
                        && w.active.load(Ordering::SeqCst) == 0
                        && w.runtime.lock().unwrap().state == "ready"
                })
                .cloned()
                .collect();
            idle.sort_by_key(|w| w.runtime.lock().unwrap().last_used);
            for candidate in idle {
                self.stop_worker(&candidate).await?;
                self.refresh_hardware(true).await;
                if self.room(worker).await.is_ok() {
                    break;
                }
            }
        }
        self.room(worker).await?;
        if worker.spec.backend == "llama_cpp" && worker.process.lock().unwrap().is_some() {
            let start = Instant::now();
            self.native_model_action(worker, "load").await?;
            self.record_context(worker).await;
            self.mark_loaded(worker, start);
            return Ok(());
        }
        self.cleanup_container(worker).await?;
        // Never attach to a process outside this manager or claim its model identity.
        if self
            .client
            .get(format!(
                "{}{}",
                worker.spec.base_url, worker.spec.health_path
            ))
            .timeout(Duration::from_secs(2))
            .send()
            .await
            .is_ok()
        {
            return Err(failure(
                "worker_port_in_use",
                "Worker port already responds; refusing to adopt an unmanaged process",
            ));
        }
        worker.runtime.lock().unwrap().state = "loading";
        let start = Instant::now();
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!(
                "{}/{}.log",
                self.config.state_dir, worker.spec.model
            ))
            .map_err(|e| failure("worker_log", e.to_string()))?;
        let mut command = Command::new(&worker.spec.command[0]);
        command
            .args(&worker.spec.command[1..])
            .envs(&worker.spec.env)
            .stdin(std::process::Stdio::null())
            .stdout(
                log.try_clone()
                    .map_err(|e| failure("worker_log", e.to_string()))?,
            )
            .stderr(log)
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = OwnedProcess::spawn(
            command,
            worker.spec.pid_registry.clone(),
            worker.spec.model.clone(),
            Some(format!("{}/owned/{}.pid", self.config.state_dir, worker.spec.model).into()),
        )
        .map_err(|e| failure("worker_spawn", e.to_string()))?;
        tracing::info!(
            model = worker.spec.model,
            pid = child.id(),
            "spawned owned native process"
        );
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|e| failure("worker_status", e.to_string()))?
            {
                return Err(failure(
                    "worker_exited",
                    format!("Native worker exited: {status}; inspect its state log"),
                ));
            }
            if start.elapsed().as_secs() >= worker.spec.load_timeout_seconds {
                return Err(failure(
                    "cold_start_timeout",
                    "Worker did not become healthy before its load deadline",
                ));
            }
            if let Ok(response) = self
                .client
                .get(format!(
                    "{}{}",
                    worker.spec.base_url, worker.spec.health_path
                ))
                .timeout(Duration::from_secs(2))
                .send()
                .await
                && response.status().is_success()
            {
                let identity: Value = self
                    .client
                    .get(format!(
                        "{}{}",
                        worker.spec.base_url, worker.spec.identity_path
                    ))
                    .timeout(Duration::from_secs(3))
                    .send()
                    .await
                    .map_err(|e| failure("worker_identity", e.to_string()))?
                    .json()
                    .await
                    .map_err(|e| failure("worker_identity", e.to_string()))?;
                let valid = if worker.spec.backend == "llama_cpp" {
                    let props: Value = self
                        .client
                        .get(format!("{}/props", worker.spec.base_url))
                        .timeout(Duration::from_secs(3))
                        .send()
                        .await
                        .map_err(|e| failure("worker_identity", e.to_string()))?
                        .json()
                        .await
                        .map_err(|e| failure("worker_identity", e.to_string()))?;
                    !worker.spec.artifacts.is_empty()
                        && props["build_info"] == worker.spec.backend_build
                        && identity["data"].as_array().is_some_and(|models| {
                            models.iter().any(|m| {
                                m["id"]
                                    == *worker
                                        .spec
                                        .backend_model
                                        .as_ref()
                                        .unwrap_or(&worker.spec.model)
                            })
                        })
                } else if worker.spec.backend == "vllm" {
                    let version: Value = self
                        .client
                        .get(format!("{}/version", worker.spec.base_url))
                        .timeout(Duration::from_secs(3))
                        .send()
                        .await
                        .map_err(|e| failure("worker_identity", e.to_string()))?
                        .json()
                        .await
                        .map_err(|e| failure("worker_identity", e.to_string()))?;
                    identity["data"]
                        .as_array()
                        .is_some_and(|models| models.iter().any(|m| m["id"] == worker.spec.model))
                        && worker.spec.backend_build
                            == format!("vllm-v{}", version["version"].as_str().unwrap_or("unknown"))
                } else if worker.spec.backend == "omlx" {
                    // oMLX names a model after its directory; weights were hash-checked before spawn.
                    !worker.spec.artifacts.is_empty()
                        && identity["data"].as_array().is_some_and(|models| {
                            models.iter().any(|m| {
                                m["id"]
                                    == *worker
                                        .spec
                                        .backend_model
                                        .as_ref()
                                        .unwrap_or(&worker.spec.model)
                            })
                        })
                } else {
                    identity["revision"] == worker.spec.revision
                        && identity["model"] == worker.spec.model
                };
                if !valid {
                    return Err(failure(
                        "revision_mismatch",
                        "Worker identity differs from the deployment manifest",
                    ));
                }
                *worker.process.lock().unwrap() = Some(child);
                if worker.spec.backend == "llama_cpp" {
                    self.native_model_action(worker, "load").await?;
                }
                if worker.spec.backend == "omlx" {
                    self.omlx_preload(worker, start).await?;
                }
                self.record_context(worker).await;
                self.mark_loaded(worker, start);
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// oMLX loads lazily; load explicitly so "ready" means weights are resident.
    async fn omlx_preload(&self, worker: &Worker, start: Instant) -> Result<()> {
        let alias = worker
            .spec
            .backend_model
            .as_deref()
            .unwrap_or(&worker.spec.model);
        let remaining = Duration::from_secs(worker.spec.load_timeout_seconds)
            .saturating_sub(start.elapsed())
            .max(Duration::from_secs(5));
        let response = self
            .client
            .post(format!("{}/v1/models/{alias}/load", worker.spec.base_url))
            .timeout(remaining)
            .send()
            .await
            .map_err(|e| failure("worker_load", e.to_string()))?;
        if !response.status().is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(failure(
                "worker_load",
                format!("oMLX did not load {alias}: {detail}"),
            ));
        }
        Ok(())
    }

    fn record_failure(&self, worker: &Worker, err: &Failure) {
        if is_capacity_refusal(&err.code) {
            return;
        }
        let mut rt = worker.runtime.lock().unwrap();
        if rt.state != "ready" {
            rt.state = "failed";
            rt.failed_at = Some(Instant::now());
        }
        rt.last_error = Some(err.clone());
    }

    pub async fn lease(
        self: &Arc<Self>,
        model: &str,
        background: bool,
        mode: LoadMode,
    ) -> Result<Lease> {
        let worker = self.workers.get(model).cloned().ok_or_else(|| {
            failure(
                "model_unavailable",
                "No validated local deployment configured",
            )
        })?;
        let background_permit = if background {
            Some(
                worker
                    .background_slots
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| {
                        failure(
                            "admission_busy",
                            "Background capacity exhausted; one slot is reserved for main requests",
                        )
                    })?,
            )
        } else {
            None
        };
        let permit = worker.slots.clone().try_acquire_owned().map_err(|_| {
            failure(
                "admission_busy",
                "Worker concurrency budget is full; retry later",
            )
        })?;
        // Warm calls do not wait behind another model's cold load.
        let ready = {
            let runtime = worker.runtime.lock().unwrap();
            let mut process = worker.process.lock().unwrap();
            let ready = runtime.state == "ready"
                && process
                    .as_mut()
                    .is_some_and(|child| child.try_wait().ok() == Some(None));
            if ready {
                worker.active.fetch_add(1, Ordering::SeqCst);
            }
            ready
        };
        if ready {
            return Ok(Lease {
                worker,
                _permit: permit,
                _background_permit: background_permit,
            });
        }
        if mode == LoadMode::WarmOnly && !worker.spec.external {
            return Err(failure(
                "model_not_warm",
                "auto serves only a loaded model here",
            ));
        }
        // Detached load ownership survives a disconnected client; process cleanup is explicit.
        let service = self.clone();
        tokio::spawn(async move {
            let _guard = service.lifecycle.lock().await;
            match service.load_worker(&worker, mode).await {
                Ok(()) => {
                    worker.active.fetch_add(1, Ordering::SeqCst);
                    Ok(Lease {
                        worker,
                        _permit: permit,
                        _background_permit: background_permit,
                    })
                }
                Err(err) => {
                    if let Err(cleanup) = service.cleanup_container(&worker).await {
                        tracing::error!(error=?cleanup,"failed native container cleanup");
                    }
                    service.record_failure(&worker, &err);
                    Err(err)
                }
            }
        })
        .await
        .map_err(|e| failure("lifecycle_task", e.to_string()))?
    }

    pub fn start_operation(self: &Arc<Self>, model: &str, action: &str) -> Result<Operation> {
        if !self.models.contains_key(model) {
            return Err(failure("unknown_model", model));
        }
        let worker = self
            .workers
            .get(model)
            .cloned()
            .ok_or_else(|| failure("model_unavailable", "No local deployment configured"))?;
        let mut operations = self.operations.lock().unwrap();
        if let Some(existing) = operations
            .values()
            .find(|o| o.model == model && o.status == "running")
        {
            if existing.action == action {
                return Ok(existing.clone());
            }
            return Err(failure(
                "operation_conflict",
                "Another lifecycle operation is running for this model",
            ));
        }
        let operation = Operation {
            id: uuid::Uuid::new_v4().to_string(),
            model: model.into(),
            action: action.into(),
            status: "running".into(),
            elapsed_ms: None,
            error: None,
            cancel: Arc::new(Notify::new()),
        };
        operations.insert(operation.id.clone(), operation.clone());
        let op = operation.clone();
        let service = self.clone();
        tokio::spawn(async move {
            let start = Instant::now();
            let work = async {
                let _guard = if op.action == "prefetch" {
                    None
                } else {
                    Some(service.lifecycle.lock().await)
                };
                match op.action.as_str() {
                    "load" => service.load_worker(&worker, LoadMode::Normal).await,
                    "unload" => service.stop_worker(&worker).await,
                    "prefetch" => {
                        if worker.spec.prefetch_command.is_empty() {
                            return Err(failure(
                                "prefetch_unsupported",
                                "No pinned prefetch command configured",
                            ));
                        }
                        let mut child = Command::new(&worker.spec.prefetch_command[0])
                            .args(&worker.spec.prefetch_command[1..])
                            .envs(&worker.spec.env)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .kill_on_drop(true)
                            .spawn()
                            .map_err(|e| failure("prefetch_failed", e.to_string()))?;
                        let status = child
                            .wait()
                            .await
                            .map_err(|e| failure("prefetch_failed", e.to_string()))?;
                        if !status.success() {
                            return Err(failure("prefetch_failed", status.to_string()));
                        }
                        Ok(())
                    }
                    _ => Err(failure(
                        "invalid_operation",
                        "Supported actions: load, unload, prefetch",
                    )),
                }
            };
            let result = tokio::select! {
                _ = op.cancel.notified() => Err(failure("operation_cancelled", "Operation cancelled by caller")),
                result = tokio::time::timeout(Duration::from_secs(worker.spec.load_timeout_seconds+30),work) =>
                    result.unwrap_or_else(|_| Err(failure("operation_timeout", "Lifecycle operation deadline exceeded"))),
            };
            if result.as_ref().is_err_and(|e| {
                matches!(e.code.as_str(), "operation_cancelled" | "operation_timeout")
            }) && op.action == "load"
                && worker.spec.backend == "llama_cpp"
            {
                let _guard = service.lifecycle.lock().await;
                if let Err(error) = service.stop_worker(&worker).await {
                    tracing::error!(model=worker.spec.model,error=?error,"native load cancellation cleanup failed");
                }
            }
            if let Err(err) = &result {
                if let Err(cleanup) = service.cleanup_container(&worker).await {
                    tracing::error!(error=?cleanup,"failed operation container cleanup");
                }
                service.record_failure(&worker, err);
            }
            let completed = {
                let mut operations = service.operations.lock().unwrap();
                let current = operations.get_mut(&op.id).unwrap();
                current.status = if result.is_ok() {
                    "succeeded"
                } else {
                    "failed"
                }
                .into();
                current.elapsed_ms = Some(start.elapsed().as_millis());
                current.error = result.err();
                current.clone()
            };
            let path = format!("{}/operations/{}.json", service.config.state_dir, op.id);
            if let Ok(bytes) = serde_json::to_vec_pretty(&completed) {
                let temporary = format!("{path}.tmp");
                if tokio::fs::write(&temporary, bytes).await.is_ok() {
                    let _ = tokio::fs::rename(temporary, path).await;
                }
            }
        });
        Ok(operation)
    }

    pub async fn evict_idle(&self) {
        let _guard = self.lifecycle.lock().await;
        for (id, worker) in &self.workers {
            let idle = {
                let rt = worker.runtime.lock().unwrap();
                !worker.spec.keep_warm
                    && (!worker.spec.external || worker.spec.external_lifecycle)
                    && rt.state == "ready"
                    && rt.last_used.elapsed().as_secs() >= self.models[id].idle_timeout_seconds
            };
            if idle
                && worker.active.load(Ordering::SeqCst) == 0
                && let Err(err) = self.stop_worker(worker).await
            {
                tracing::warn!(model=id, error=?err, "idle eviction failed");
            }
        }
    }

    pub async fn shutdown(&self) {
        let _guard = self.lifecycle.lock().await;
        for worker in self.workers.values() {
            if worker.spec.external {
                continue;
            }
            if let Err(err) = self.stop_worker(worker).await {
                tracing::error!(error=?err,"shutdown worker cleanup");
            }
            let child = worker.process.lock().unwrap().take();
            if let Some(mut child) = child {
                let _ = child.terminate().await;
            }
        }
    }

    /// The context one request gets on this engine, read once per load: llama.cpp reports its
    /// per-slot `n_ctx` (the total divided by `-np`), vLLM its `max_model_len`.
    async fn record_context(&self, worker: &Worker) {
        if worker.runtime.lock().unwrap().context_tokens.is_some() {
            return;
        }
        let get = |url: reqwest::Url| async {
            let response = self
                .client
                .get(url)
                .timeout(Duration::from_secs(3))
                .send()
                .await
                .ok()?;
            response.json::<Value>().await.ok()
        };
        let base = &worker.spec.base_url;
        let served = match worker.spec.backend.as_str() {
            "llama_cpp" => {
                let Ok(mut url) = reqwest::Url::parse(&format!("{base}/props")) else {
                    return;
                };
                let alias = worker
                    .spec
                    .backend_model
                    .as_deref()
                    .unwrap_or(&worker.spec.model);
                url.query_pairs_mut().append_pair("model", alias);
                get(url)
                    .await
                    .and_then(|v| v["default_generation_settings"]["n_ctx"].as_u64())
            }
            "vllm" => {
                let Ok(url) = reqwest::Url::parse(&format!("{base}/v1/models")) else {
                    return;
                };
                get(url).await.and_then(|v| {
                    v["data"]
                        .as_array()?
                        .iter()
                        .find(|m| m["id"] == worker.spec.model)?["max_model_len"]
                        .as_u64()
                })
            }
            _ => None,
        };
        worker.runtime.lock().unwrap().context_tokens = served;
    }

    /// The per-request context this node serves `model` with: the engine's own figure once
    /// loaded, else the deployment's configured `context_tokens`.
    pub fn context_of(&self, model: &str) -> Option<(u64, &'static str)> {
        let worker = self.workers.get(model)?;
        if let Some(served) = worker.runtime.lock().unwrap().context_tokens {
            return Some((served, "engine"));
        }
        worker.spec.context_tokens.map(|n| (n, "config"))
    }

    fn mark_loaded(&self, worker: &Worker, start: Instant) {
        let mut rt = worker.runtime.lock().unwrap();
        rt.state = "ready";
        rt.loaded_ms = Some(start.elapsed().as_millis());
        rt.failed_at = None;
        rt.last_error = None;
        rt.last_used = Instant::now();
    }

    async fn native_model_action(&self, worker: &Worker, action: &str) -> Result<()> {
        let model = worker
            .spec
            .backend_model
            .as_deref()
            .unwrap_or(&worker.spec.model);
        let admission_started = Instant::now();
        let result: Value = loop {
            let request = self
                .client
                .post(format!("{}/models/{action}", worker.spec.base_url))
                .json(&json!({"model":model}))
                .timeout(Duration::from_secs(worker.spec.load_timeout_seconds))
                .send();
            tokio::pin!(request);
            let response = loop {
                tokio::select! {
                    response=&mut request=>break response.map_err(|e|failure("native_lifecycle",e.to_string()))?,
                    _=tokio::time::sleep(Duration::from_millis(250))=>self.register_native_children(worker)?,
                }
            };
            self.register_native_children(worker)?;
            let status = response.status();
            let result: Value = response
                .json()
                .await
                .map_err(|e| failure("native_lifecycle", e.to_string()))?;
            if !status.is_success() {
                // Router unload acknowledges the signal before its child exits. A replacement
                // may briefly hit models-max; retry only that explicit, non-executing failure.
                if action == "load"
                    && result["error"]["message"]
                        .as_str()
                        .is_some_and(|m| m.contains("model limit reached"))
                    && admission_started.elapsed() < Duration::from_secs(5)
                {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
                return Err(failure(
                    "native_lifecycle",
                    format!("Native {action} returned {status}: {result}"),
                ));
            }
            break result;
        };
        if result["success"] != true {
            return Err(failure("native_lifecycle", result.to_string()));
        }
        self.sync_native_status(worker).await?;
        if action == "load" {
            let started = Instant::now();
            loop {
                let port = self.sync_native_status(worker).await?;
                if worker.spec.external || port.is_some() {
                    let url = if worker.spec.external {
                        format!(
                            "{}/props?model={}",
                            worker.spec.base_url,
                            worker
                                .spec
                                .backend_model
                                .as_deref()
                                .unwrap_or(&worker.spec.model)
                        )
                    } else {
                        format!("http://127.0.0.1:{}/health", port.unwrap())
                    };
                    let response = self
                        .client
                        .get(url)
                        .timeout(Duration::from_secs(2))
                        .send()
                        .await;
                    if let Ok(response) = response
                        && response.status().is_success()
                    {
                        worker.runtime.lock().unwrap().native_port = port;
                        break;
                    }
                }
                if started.elapsed().as_secs() >= worker.spec.load_timeout_seconds {
                    return Err(failure(
                        "cold_start_timeout",
                        "Native router child did not become healthy",
                    ));
                }
                self.register_native_children(worker)?;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
        Ok(())
    }

    fn register_native_children(&self, worker: &Worker) -> Result<()> {
        if let Some(process) = worker.process.lock().unwrap().as_mut() {
            process.set_model(&worker.spec.model);
            process
                .register_children()
                .map_err(|e| failure("pid_registry", e.to_string()))?;
        }
        Ok(())
    }

    async fn sync_native_status(&self, worker: &Worker) -> Result<Option<u16>> {
        let models: Value = self
            .client
            .get(format!("{}/v1/models", worker.spec.base_url))
            .timeout(Duration::from_secs(3))
            .send()
            .await
            .map_err(|e| failure("worker_status", e.to_string()))?
            .json()
            .await
            .map_err(|e| failure("worker_status", e.to_string()))?;
        let models = models["data"]
            .as_array()
            .ok_or_else(|| failure("worker_status", "Native model list is invalid"))?;
        let mut target_port = None;
        for sibling in self
            .workers
            .values()
            .filter(|w| w.spec.base_url == worker.spec.base_url)
        {
            let alias = sibling
                .spec
                .backend_model
                .as_ref()
                .unwrap_or(&sibling.spec.model);
            let entry = models.iter().find(|m| m["id"] == *alias);
            let status = entry.and_then(|m| m["status"]["value"].as_str());
            let port = entry
                .and_then(|m| m["status"]["args"].as_array())
                .and_then(|args| {
                    args.windows(2)
                        .find(|pair| pair[0] == "--port")
                        .and_then(|pair| pair[1].as_str())
                        .and_then(|s| s.parse::<u16>().ok())
                })
                .filter(|p| *p > 0);
            if sibling.spec.model == worker.spec.model {
                target_port = port;
            }
            let mut rt = sibling.runtime.lock().unwrap();
            rt.state = match status {
                Some("loaded") if sibling.spec.external => "ready",
                Some("loaded")
                    if rt.native_port == port && port.is_some() && rt.state == "ready" =>
                {
                    "ready"
                }
                Some("loaded") => "loading",
                Some("loading") => "loading",
                Some("unloaded") => {
                    rt.native_port = None;
                    "unloaded"
                }
                _ => "unknown",
            };
        }
        Ok(target_port)
    }

    async fn cleanup_container(&self, worker: &Worker) -> Result<()> {
        let Some(path) = &worker.spec.container_id_file else {
            return Ok(());
        };
        let Ok(contents) = tokio::fs::read_to_string(path).await else {
            return Ok(());
        };
        let id = contents.trim();
        if id.len() != 64 || !id.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(failure(
                "container_identity",
                "Refusing to signal an invalid recorded container ID",
            ));
        }
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            Command::new("docker")
                .args(["rm", "-f", id])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| failure("container_cleanup", "Docker cleanup deadline exceeded"))?
        .map_err(|e| failure("container_cleanup", e.to_string()))?;
        if !result.status.success()
            && !String::from_utf8_lossy(&result.stderr).contains("No such container")
        {
            return Err(failure(
                "container_cleanup",
                String::from_utf8_lossy(&result.stderr).to_string(),
            ));
        }
        tracing::info!(
            container_id = id,
            model = worker.spec.model,
            "removed owned container or confirmed already absent"
        );
        tokio::fs::remove_file(path)
            .await
            .map_err(|e| failure("container_cleanup", e.to_string()))?;
        Ok(())
    }
}

pub fn new_workers(specs: Vec<Deployment>) -> BTreeMap<String, Arc<Worker>> {
    let mut processes = BTreeMap::new();
    specs
        .into_iter()
        .map(|spec| {
            let process = processes
                .entry(spec.base_url.clone())
                .or_insert_with(|| Arc::new(Mutex::new(None)))
                .clone();
            let id = spec.model.clone();
            let worker = Arc::new(Worker {
                background_slots: Arc::new(Semaphore::new(spec.max_concurrency.saturating_sub(1))),
                slots: Arc::new(Semaphore::new(spec.max_concurrency)),
                spec,
                process,
                verified_files: Mutex::new(BTreeMap::new()),
                active: AtomicUsize::new(0),
                runtime: Mutex::new(Runtime {
                    state: "unloaded",
                    last_error: None,
                    failed_at: None,
                    loaded_ms: None,
                    unloaded_ms: None,
                    last_used: Instant::now(),
                    native_port: None,
                    context_tokens: None,
                }),
            });
            (id, worker)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::score;
    use serde_json::json;

    fn host(worker: serde_json::Value, available: u64) -> serde_json::Value {
        json!({"managed_workers":[worker],"admission":{"ram_reserve_bytes":1000,"accelerator_reserve_bytes":0},
               "ram":{"available_bytes":available}})
    }

    fn worker(state: &str, active: u64) -> serde_json::Value {
        json!({"model":"m","state":state,"active_requests":active,"max_concurrency":2,
               "reserved_peak_bytes":5000,"device":"cpu","idle_seconds":0})
    }

    #[test]
    fn capacity_refusals_are_not_failures() {
        for code in [
            "accelerator_busy",
            "insufficient_memory",
            "insufficient_vram",
            "memory_pressure",
        ] {
            assert!(super::is_capacity_refusal(code));
        }
        for code in [
            "worker_spawn",
            "worker_load",
            "artifact_missing",
            "worker_exited",
        ] {
            assert!(!super::is_capacity_refusal(code));
        }
    }

    #[test]
    fn warm_worker_outranks_cold_worker() {
        let warm = score(&host(worker("ready", 0), 0), "m", "chat", false).unwrap();
        let cold = score(&host(worker("unloaded", 0), 1 << 40), "m", "chat", false).unwrap();
        assert!(warm > cold);
    }

    #[test]
    fn background_keeps_one_slot_for_main_requests() {
        assert!(score(&host(worker("ready", 1), 0), "m", "chat", false).is_some());
        assert!(score(&host(worker("ready", 1), 0), "m", "chat", true).is_none());
    }

    #[test]
    fn cold_worker_needs_budget_plus_reserve() {
        assert!(score(&host(worker("unloaded", 0), 5999), "m", "chat", false).is_none());
        assert!(score(&host(worker("unloaded", 0), 6000), "m", "chat", false).is_some());
    }

    #[test]
    fn held_or_capability_disabled_workers_are_skipped() {
        let mut held = worker("ready", 0);
        held["hold_reason"] = json!("reserved");
        assert!(score(&host(held, 1 << 40), "m", "chat", false).is_none());
        let mut limited = worker("ready", 0);
        limited["enabled_capabilities"] = json!(["decisions"]);
        assert!(score(&host(limited, 1 << 40), "m", "chat", false).is_none());
    }

    #[test]
    fn memory_stall_blocks_cold_but_not_warm_placement() {
        let mut stalled = host(worker("unloaded", 0), 1 << 40);
        stalled["memory_stall_percent"] = json!(12.0);
        assert!(score(&stalled, "m", "chat", false).is_none());
        let mut warm = host(worker("ready", 0), 0);
        warm["memory_stall_percent"] = json!(12.0);
        assert!(score(&warm, "m", "chat", false).is_some());
    }

    #[test]
    fn recent_failure_blocks_cold_placement() {
        let mut failed = worker("failed", 0);
        failed["last_error"] = json!({"code":"worker_exited"});
        assert!(score(&host(failed, 1 << 40), "m", "chat", false).is_none());
    }
    #[test]
    fn held_slots_count_even_before_the_load_finishes() {
        // A slot taken for a cold load is not yet in active_requests.
        let mut loading = worker("ready", 0);
        loading["occupied_slots"] = json!(2);
        assert!(score(&host(loading, 1 << 40), "m", "chat", false).is_none());
        let mut one = worker("ready", 0);
        one["occupied_slots"] = json!(1);
        assert!(score(&host(one.clone(), 1 << 40), "m", "chat", false).is_some());
        assert!(score(&host(one, 1 << 40), "m", "chat", true).is_none());
    }
}
