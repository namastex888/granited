use serde_json::{Value, json};
use std::{
    process::Command,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use sysinfo::{Disks, System};

fn output(program: &str, args: &[&str]) -> Option<String> {
    let result = Command::new(program).args(args).output().ok()?;
    result
        .status
        .success()
        .then(|| String::from_utf8_lossy(&result.stdout).trim().to_owned())
}

/// Static inventory changes rarely and some probes (system_profiler, runtime --version) cost
/// hundreds of milliseconds, so it is re-measured at most once a minute.
const INVENTORY_TTL: Duration = Duration::from_secs(60);
static INVENTORY: Mutex<Option<(Instant, Value)>> = Mutex::new(None);

pub fn collect() -> Value {
    let cached = INVENTORY
        .lock()
        .unwrap()
        .clone()
        .filter(|(at, _)| at.elapsed() < INVENTORY_TTL);
    let mut value = match cached {
        Some((_, v)) => v,
        None => {
            let v = inventory();
            *INVENTORY.lock().unwrap() = Some((Instant::now(), v.clone()));
            v
        }
    };
    let mut system = System::new();
    system.refresh_memory();
    let mut accelerators = nvidia();
    accelerators.extend(
        value["accelerators"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
    );
    let mut issues: Vec<Value> = value["issues"].as_array().cloned().unwrap_or_default();
    if accelerators.is_empty() {
        issues.push(json!(
            "No accelerator probe succeeded; accelerator capacity unknown."
        ));
    }
    let processes = output("ps", &["-axo", "pid=,rss=,comm="]).map(|s| {
        s.lines()
            .filter(|line| {
                ["llama", "vllm", "mlx", "python", "granited", "ollama"]
                    .iter()
                    .any(|name| line.contains(name))
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    value["observed_unix_ms"] = json!(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    value["ram"] = json!({"total_bytes":system.total_memory(), "available_bytes":system.available_memory(),
        "swap_total_bytes":system.total_swap(), "swap_used_bytes":system.used_swap()});
    value["memory_pressure"] = json!(output("memory_pressure", &["-Q"]));
    value["memory_stall_percent"] = json!(memory_stall_percent());
    value["load_average"] = json!({"one":System::load_average().one, "five":System::load_average().five, "fifteen":System::load_average().fifteen});
    value["accelerators"] = json!(accelerators);
    value["active_model_processes"] = json!(processes);
    value["issues"] = json!(issues);
    value
}

/// Share of the last 10 s in which all non-idle tasks stalled on memory (Linux PSI "full"),
/// or 100 minus the free percentage macOS reports. `None` when neither source exists.
pub fn memory_stall_percent() -> Option<f64> {
    if let Ok(psi) = std::fs::read_to_string("/proc/pressure/memory") {
        return psi
            .lines()
            .find(|l| l.starts_with("full"))
            .and_then(|l| l.split_whitespace().find_map(|f| f.strip_prefix("avg10=")))
            .and_then(|v| v.parse().ok());
    }
    let free = output("memory_pressure", &["-Q"])?;
    let percent = free
        .rsplit(':')
        .next()?
        .trim()
        .trim_end_matches('%')
        .parse::<f64>()
        .ok()?;
    // macOS reports free memory; below 10% free the system is compressing and swapping.
    Some(if percent < 10.0 { 100.0 - percent } else { 0.0 })
}

fn nvidia() -> Vec<Value> {
    let mut accelerators = Vec::new();
    if let Some(csv) = output(
        "nvidia-smi",
        &[
            "--query-gpu=name,uuid,pci.device_id,memory.total,memory.free,driver_version,compute_cap",
            "--format=csv,noheader,nounits",
        ],
    ) {
        for line in csv.lines() {
            let f: Vec<_> = line.split(',').map(str::trim).collect();
            if f.len() == 7 {
                accelerators.push(
                    json!({"vendor":"nvidia", "name":f[0], "uuid":f[1], "device_id":f[2],
                    "total_memory_bytes":f[3].parse::<u64>().ok().map(|v| v*1024*1024),
                    "available_memory_bytes":f[4].parse::<u64>().ok().map(|v| v*1024*1024),
                    "driver":f[5], "compute_capability":f[6], "memory_kind":"dedicated"}),
                );
            }
        }
    }
    accelerators
}

fn inventory() -> Value {
    let mut system = System::new();
    system.refresh_cpu_all();
    let mut accelerators = Vec::new();
    let issues: Vec<Value> = Vec::new();
    if cfg!(target_os = "macos") {
        let display = output("system_profiler", &["SPDisplaysDataType", "-json"])
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        for device in display
            .as_ref()
            .and_then(|v| v["SPDisplaysDataType"].as_array())
            .into_iter()
            .flatten()
        {
            accelerators.push(json!({"vendor":"apple", "name":device["sppci_model"],
                "device_id":device.get("spdisplays_device-id"), "cores":device["sppci_cores"],
                "metal_family":device["spdisplays_mtlgpufamilysupport"], "memory_kind":"unified",
                "total_memory_bytes":null, "available_memory_bytes":null,
                "note":"Unified system RAM is not guaranteed accelerator capacity."}));
        }
    }
    if cfg!(target_os = "linux") {
        for class in ["/sys/class/accel", "/sys/class/drm"] {
            if let Ok(entries) = std::fs::read_dir(class) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if name.contains('-') || name.starts_with("render") {
                        continue;
                    }
                    let read = |name: &str| {
                        std::fs::read_to_string(path.join("device").join(name))
                            .ok()
                            .map(|s| s.trim().to_owned())
                    };
                    if let Some(vendor) = read("vendor") {
                        accelerators.push(json!({"vendor_pci_id":vendor, "device_id":read("device"),
                            "node":name, "class":class, "driver":std::fs::read_link(path.join("device/driver")).ok(),
                            "total_memory_bytes":null, "available_memory_bytes":null,
                            "granite_runtime_validated":false}));
                    }
                }
            }
        }
    }
    let disks: Vec<_> = Disks::new_with_refreshed_list().list().iter().map(|d| json!({
        "mount":d.mount_point(), "total_bytes":d.total_space(), "available_bytes":d.available_space()
    })).collect();
    json!({"schema_version":1,
        "hostname":System::host_name(), "os":System::long_os_version(), "kernel":System::kernel_version(),
        "architecture":std::env::consts::ARCH, "cpu_brand":system.cpus().first().map(|cpu| cpu.brand()),
        "logical_cpus":system.cpus().len(), "physical_cores":System::physical_core_count(),
        "accelerators":accelerators, "disks":disks,
        "active_model_process_columns":["pid","resident_kib","executable"],
        "network_interfaces":output("ifconfig", &[]).or_else(|| output("ip", &["-json", "address"])),
        "runtime_versions":{"llama_cpp":output("llama-server", &["--version"]),
            "nvidia_smi":output("nvidia-smi", &["--query-gpu=driver_version", "--format=csv,noheader"])},
        "issues":issues})
}
