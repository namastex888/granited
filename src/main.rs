mod api;
mod config;
mod contracts;
mod hardware;
mod lifecycle;
mod process;

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_target(false).init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("hardware") => {
            println!("{}", serde_json::to_string_pretty(&hardware::collect())?);
            return Ok(());
        }
        Some("contracts") => {
            println!("{}", serde_json::to_string_pretty(&contracts::schemas())?);
            return Ok(());
        }
        Some("serve") => {}
        _ => {
            eprintln!("Usage: granited hardware | contracts | serve CONFIG.json");
            std::process::exit(2);
        }
    }
    let (config, registry) =
        config::Config::read(args.get(2).ok_or("configuration path required")?)?;
    tokio::fs::create_dir_all(format!("{}/operations", config.state_dir)).await?;
    let lock_path = format!("{}/manager.lock", config.state_dir);
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)?;
    lock.try_lock()
        .map_err(|e| format!("state directory already owned by another manager: {e}"))?;
    let workers = lifecycle::new_workers(config.deployments.clone());
    let initial = tokio::task::spawn_blocking(hardware::collect).await?;
    let service = Arc::new(lifecycle::Service {
        config: config.clone(),
        models: registry
            .models
            .into_iter()
            .map(|model| (model.id.clone(), model))
            .collect::<BTreeMap<_, _>>(),
        workers,
        client: reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(120))
            .build()?,
        lifecycle: tokio::sync::Mutex::new(()),
        operations: Mutex::new(BTreeMap::new()),
        hardware: tokio::sync::Mutex::new((Instant::now() - Duration::from_secs(10), initial)),
        peer_hardware: tokio::sync::Mutex::new(BTreeMap::new()),
        rejections: std::sync::Mutex::new(BTreeMap::new()),
        started: Instant::now(),
    });
    for peer in &config.peers {
        if std::env::var(&peer.token_env)
            .unwrap_or_default()
            .is_empty()
        {
            tracing::warn!(
                peer = peer.id,
                env = peer.token_env,
                "peer token is unset; its requests will be rejected"
            );
        }
    }
    let probe = service.clone();
    // Memory, pressure and VRAM every 5 s; operator-owned routers (often another team's service)
    // every 15 s. Admission re-measures on its own when the snapshot is older than a second.
    let refresher = tokio::spawn(async move {
        for tick in 0u64.. {
            probe.refresh_hardware(tick % 3 == 0).await;
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    let evict = service.clone();
    let eviction = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            evict.evict_idle().await;
        }
    });
    let listener = tokio::net::TcpListener::bind(&config.listen).await?;
    tracing::info!(
        listen = config.listen,
        host = config.host_id,
        "granited ready"
    );
    let shutdown = async {
        #[cfg(unix)]
        {
            let mut term =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("install SIGTERM handler");
            tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    };
    axum::serve(listener, api::router(service.clone()))
        .with_graceful_shutdown(shutdown)
        .await?;
    eviction.abort();
    refresher.abort();
    service.shutdown().await;
    drop(lock);
    Ok(())
}
