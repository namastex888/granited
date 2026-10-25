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
    #[test]
    fn only_full_commit_hashes_are_immutable() {
        assert!(super::immutable("835ad14087e140460703cf0fae09f97d469d65c2"));
        assert!(!super::immutable("main"));
        assert!(!super::immutable("835ad14"));
    }
}
