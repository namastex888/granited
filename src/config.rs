use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Deserialize, Serialize)]
pub struct Model {
    pub id: String,
    pub hf_repo: String,
    pub hf_revision: Option<String>,
    pub family: String,
    pub capabilities: Vec<String>,
    pub idle_timeout_seconds: u64,
    #[serde(flatten)]
    pub metadata: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
pub struct Registry {
    pub models: Vec<Model>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Deployment {
    pub model: String,
    pub backend: String,
    pub base_url: String,
    pub revision: String,
    pub artifact_repo: String,
    pub command: Vec<String>,
    #[serde(default)]
    pub prefetch_command: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    pub memory_bytes: u64,
    pub device: String,
    pub health_path: String,
    pub identity_path: String,
    pub runtime_manifest: String,
    pub backend_build: String,
    #[serde(default)]
    pub hold_reason: Option<String>,
    /// A lock file shared with runs outside granited (`lockf -k -t 0 FILE`). The worker starts only
    /// when the lock is free and holds it until it stops; a held lock refuses the load.
    #[serde(default)]
    pub slot_lock: Option<String>,
    #[serde(default)]
    pub context_tokens: Option<u64>,
    #[serde(default)]
    pub pid_registry: Option<String>,
    #[serde(default)]
    pub container_id_file: Option<String>,
    #[serde(default)]
    pub keep_warm: bool,
    /// Connect to an operator-owned native router without adopting its process.
    #[serde(default)]
    pub external: bool,
    #[serde(default)]
    pub external_lifecycle: bool,
    #[serde(default)]
    pub shared_accelerator: bool,
    #[serde(default)]
    pub enabled_capabilities: Option<Vec<String>>,
    #[serde(default)]
    pub backend_model: Option<String>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
    #[serde(default = "load_timeout")]
    pub load_timeout_seconds: u64,
    #[serde(default = "one")]
    pub max_concurrency: usize,
}

fn one() -> usize {
    1
}
fn load_timeout() -> u64 {
    240
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Peer {
    pub id: String,
    pub url: String,
    pub token_env: String,
    pub models: Vec<String>,
}

#[derive(Clone, Deserialize)]
pub struct Config {
    pub listen: String,
    pub host_id: String,
    pub registry: String,
    pub state_dir: String,
    pub api_key_env: String,
    pub admin_key_env: String,
    pub ram_reserve_bytes: u64,
    pub accelerator_reserve_bytes: u64,
    #[serde(default)]
    pub deployments: Vec<Deployment>,
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default)]
    pub auto: Auto,
}

/// `model: "auto"`, off unless a profile turns it on. `prefer` lists models per capability, best
/// first. Embeddings and forecast always use their first entry, so vectors never mix models.
#[derive(Clone, Default, Deserialize)]
pub struct Auto {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub prefer: BTreeMap<String, Vec<String>>,
    /// Chat with `image_url` parts goes to `prefer.vision`, never evicting, like text chat.
    #[serde(default)]
    pub images: bool,
    /// Chat with `input_audio` parts is transcribed by `prefer.transcription` first.
    #[serde(default)]
    pub audio: bool,
    /// `/v1/audio/speech` with `auto` picks from `prefer.speech_synthesis`, best tier first,
    /// never evicting, on this node only: voice references are personal audio.
    #[serde(default)]
    pub speech: bool,
}

/// The capabilities `auto` serves; every other endpoint answers `unsupported_auto`.
pub const AUTO_CAPABILITIES: [&str; 3] = ["chat", "embeddings", "forecast"];
/// Preference lists that only serve parts of an `auto` chat request, never their own endpoint.
pub const AUTO_INPUTS: [&str; 3] = ["vision", "transcription", "speech_synthesis"];
const MAX_AUTO_ID: usize = 64;

/// `auto` is reserved, and every preference must name a registered model of the right kind.
fn validate_auto(config: &Config, registry: &Registry) -> Result<(), String> {
    let reserved = |id: &str| id == "auto";
    if registry.models.iter().any(|m| {
        reserved(&m.id)
            || m.metadata
                .get("aliases")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|a| a.as_str().is_some_and(reserved)))
    }) || config
        .peers
        .iter()
        .any(|p| p.models.iter().any(|m| reserved(m)))
    {
        return Err("`auto` is reserved and cannot name a model, alias or peer model".into());
    }
    for (capability, ids) in &config.auto.prefer {
        if !AUTO_CAPABILITIES.contains(&capability.as_str())
            && !AUTO_INPUTS.contains(&capability.as_str())
        {
            return Err(format!(
                "auto.prefer.{capability}: auto serves only chat, embeddings, forecast, vision, transcription and speech_synthesis"
            ));
        }
        if ids.is_empty() {
            return Err(format!("auto.prefer.{capability} is empty"));
        }
        for id in ids {
            let model = registry
                .models
                .iter()
                .find(|m| &m.id == id && id.len() <= MAX_AUTO_ID)
                .ok_or(format!(
                    "auto.prefer.{capability}: {id} is not a registered model id"
                ))?;
            if !model.capabilities.contains(capability)
                || (capability == "chat" && model.family != "language")
                || (capability == "vision" && !model.capabilities.iter().any(|c| c == "chat"))
                || (capability == "forecast"
                    && model.metadata.get("license") != Some(&Value::from("apache-2.0")))
            {
                return Err(format!(
                    "auto.prefer.{capability}: {id} must declare {capability} (chat: family language; vision: also chat; forecast: Apache-2.0)"
                ));
            }
        }
    }
    // The speech stage forwards JSON, and a peer's /v1/audio/transcriptions takes only multipart,
    // so speech to text must run on the node that resolves `auto`.
    if let Some(ids) = config.auto.prefer.get("transcription")
        && let Some(id) = ids
            .iter()
            .find(|id| !config.deployments.iter().any(|d| &d.model == *id))
    {
        return Err(format!(
            "auto.prefer.transcription: {id} must be deployed on this node, not only on a peer"
        ));
    }
    if let Some(ids) = config.auto.prefer.get("speech_synthesis")
        && let Some(id) = ids
            .iter()
            .find(|id| !config.deployments.iter().any(|d| &d.model == *id))
    {
        return Err(format!(
            "auto.prefer.speech_synthesis: {id} must be deployed on this node; voice references never leave it"
        ));
    }
    for (on, flag, key) in [
        (config.auto.images, "images", "vision"),
        (config.auto.audio, "audio", "transcription"),
        (config.auto.speech, "speech", "speech_synthesis"),
    ] {
        if on && !config.auto.prefer.contains_key(key) {
            return Err(format!("auto.{flag} needs auto.prefer.{key}"));
        }
    }
    Ok(())
}

pub fn immutable(revision: &str) -> bool {
    revision.len() == 40 && revision.bytes().all(|c| c.is_ascii_hexdigit())
}

impl Config {
    pub fn read(path: &str) -> Result<(Self, Registry), Box<dyn std::error::Error>> {
        let config: Self = serde_json::from_slice(&std::fs::read(path)?)?;
        let registry: Registry = serde_json::from_slice(&std::fs::read(&config.registry)?)?;
        let mut ids = std::collections::HashSet::new();
        for model in &registry.models {
            if !ids.insert(&model.id) {
                return Err(format!("duplicate model {}", model.id).into());
            }
            if let Some(rev) = &model.hf_revision
                && !immutable(rev)
            {
                return Err(format!("mutable revision for {}", model.id).into());
            }
        }
        ids.clear();
        for d in &config.deployments {
            if let Some(model) = registry.models.iter().find(|m| m.id == d.model)
                && d.artifact_repo == model.hf_repo
                && model.hf_revision.as_deref() != Some(d.revision.as_str())
            {
                return Err(
                    format!("deployment revision differs from registry: {}", d.model).into(),
                );
            }
            if !ids.insert(&d.model) || !registry.models.iter().any(|m| m.id == d.model) {
                return Err(format!("duplicate or unregistered deployment {}", d.model).into());
            }
            if !immutable(&d.revision)
                || d.memory_bytes == 0
                || (!d.external && d.command.is_empty())
                || (d.external && d.backend != "llama_cpp")
                || (d.external_lifecycle && !d.external)
                || d.max_concurrency == 0
                || !matches!(d.device.as_str(), "cpu" | "unified" | "cuda")
                || !Path::new(&d.runtime_manifest).is_file()
            {
                return Err(format!(
                    "invalid deployment or missing runtime manifest: {}",
                    d.model
                )
                .into());
            }
            let url = reqwest::Url::parse(&d.base_url)?;
            if !d.external
                && url.host_str() != Some("127.0.0.1")
                && url.host_str() != Some("localhost")
            {
                return Err("local worker must bind loopback; use peers for remote routing".into());
            }
            for sibling in config
                .deployments
                .iter()
                .filter(|s| s.model != d.model && s.base_url == d.base_url)
            {
                if d.backend != "llama_cpp"
                    || sibling.backend != d.backend
                    || sibling.command != d.command
                {
                    return Err(
                        "Only identical native llama router commands may share a worker address"
                            .into(),
                    );
                }
            }
        }
        for peer in &config.peers {
            reqwest::Url::parse(&peer.url)?;
            if std::env::var(&peer.token_env)
                .unwrap_or_default()
                .is_empty()
            {
                return Err(format!("missing peer token environment {}", peer.token_env).into());
            }
        }
        validate_auto(&config, &registry)?;
        // The 3B temperature rule lives in the registry; an older registry would silently drop it.
        if registry.models.iter().any(|m| {
            m.id == "granite-4.2-3b"
                && m.metadata.get("off_requires_temperature_zero") != Some(&Value::Bool(true))
        }) {
            return Err(format!(
                "{} predates off_requires_temperature_zero on granite-4.2-3b; use this checkout's configs/registry.json",
                config.registry
            )
            .into());
        }
        for key in [&config.api_key_env, &config.admin_key_env] {
            if std::env::var(key).unwrap_or_default().len() < 24 {
                return Err(format!("{key} must contain at least 24 characters").into());
            }
        }
        Ok((config, registry))
    }
}

#[cfg(test)]
mod tests {
    use super::{Config, Registry, validate_auto};
    use serde_json::json;

    fn fleet(prefer: serde_json::Value, peer_models: &[&str]) -> (Config, Registry) {
        let config = serde_json::from_value(json!({"listen":"127.0.0.1:0","host_id":"h","registry":"r",
            "state_dir":"s","api_key_env":"A","admin_key_env":"B","ram_reserve_bytes":0,
            "accelerator_reserve_bytes":0,"peers":[{"id":"p","url":"http://x","token_env":"T","models":peer_models}],
            "auto":{"enabled":true,"prefer":prefer}}))
        .unwrap();
        let model = |id: &str, family: &str, caps: &[&str], license: &str| json!({"id":id,"hf_repo":"r","family":family,"capabilities":caps,"idle_timeout_seconds":1,"license":license});
        let registry = serde_json::from_value(json!({"models":[
            model("granite-4.2-8b", "language", &["chat"], "apache-2.0"),
            model("granite-vision-4.1-4b", "vision", &["chat", "vision"], "apache-2.0"),
            model("granite-embedding-97m-multilingual-r2", "embedding", &["embeddings"], "apache-2.0"),
            model("granite-timeseries-patchtst-fm-r2", "time_series", &["forecast"], "openmdw-1.0"),
            model("granite-speech-5.0-470m-turboctc", "speech", &["transcription"], "apache-2.0")]}))
        .unwrap();
        (config, registry)
    }

    #[test]
    fn auto_preferences_are_validated_and_auto_is_reserved() {
        let ok = fleet(
            json!({"chat":["granite-4.2-8b"],"embeddings":["granite-embedding-97m-multilingual-r2"]}),
            &[],
        );
        assert!(validate_auto(&ok.0, &ok.1).is_ok());
        let inputs = fleet(
            json!({"chat":["granite-4.2-8b"],"vision":["granite-vision-4.1-4b"]}),
            &[],
        );
        assert!(validate_auto(&inputs.0, &inputs.1).is_ok());
        let remote_speech = fleet(
            json!({"chat":["granite-4.2-8b"],"transcription":["granite-speech-5.0-470m-turboctc"]}),
            &["granite-speech-5.0-470m-turboctc"],
        );
        assert!(
            validate_auto(&remote_speech.0, &remote_speech.1).is_err(),
            "speech to text must be deployed on the entry node"
        );
        let mut no_list = fleet(json!({"chat":["granite-4.2-8b"]}), &[]);
        no_list.0.auto.images = true;
        assert!(
            validate_auto(&no_list.0, &no_list.1).is_err(),
            "images needs prefer.vision"
        );
        no_list.0.auto.images = false;
        no_list.0.auto.audio = true;
        assert!(
            validate_auto(&no_list.0, &no_list.1).is_err(),
            "audio needs prefer.transcription"
        );
        for bad in [
            json!({"chat":["granite-vision-4.1-4b"]}),
            json!({"embeddings":["granite-4.2-8b"]}),
            json!({"forecast":["granite-timeseries-patchtst-fm-r2"]}),
            json!({"vision":["granite-4.2-8b"]}),
            json!({"transcription":["granite-4.2-8b"]}),
            json!({"rerank":["granite-4.2-8b"]}),
            json!({"chat":[]}),
            json!({"chat":["granite-unknown"]}),
        ] {
            let (config, registry) = fleet(bad.clone(), &[]);
            assert!(validate_auto(&config, &registry).is_err(), "{bad}");
        }
        let (config, registry) = fleet(json!({}), &["auto"]);
        assert!(validate_auto(&config, &registry).is_err());
    }

    #[test]
    fn profiles_load_with_auto_off_except_the_auto_example() {
        for entry in std::fs::read_dir("configs")
            .unwrap()
            .chain(std::fs::read_dir("configs/examples").unwrap())
        {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json")
                && path.file_name().unwrap() != "registry.json"
            {
                let value: serde_json::Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if value.get("listen").is_some() {
                    let config: Config = serde_json::from_value(value).unwrap();
                    if path.file_name().unwrap() == "auto.json" {
                        let registry: Registry =
                            serde_json::from_slice(&std::fs::read(&config.registry).unwrap())
                                .unwrap();
                        assert!(config.auto.enabled && validate_auto(&config, &registry).is_ok());
                        assert_eq!(
                            config.auto.prefer.len(),
                            3,
                            "the README gate reads this profile"
                        );
                    } else {
                        assert!(!config.auto.enabled, "{}", path.display());
                    }
                }
            }
        }
    }

    #[test]
    fn only_full_commit_hashes_are_immutable() {
        assert!(super::immutable("835ad14087e140460703cf0fae09f97d469d65c2"));
        assert!(!super::immutable("main"));
        assert!(!super::immutable("835ad14"));
    }
}
