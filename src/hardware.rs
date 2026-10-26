use serde_json::{Value, json};
use std::{
    path::Path,
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
    drop_duplicates(&mut accelerators);
    // Power is read on every snapshot; the inventory above is cached for a minute.
    for accelerator in &mut accelerators {
        if accelerator.get("power").is_none() {
            accelerator["power"] = device_power(accelerator);
        }
    }
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
    output(
        "nvidia-smi",
        &[
            "--query-gpu=name,uuid,pci.device_id,memory.total,memory.free,driver_version,compute_cap,power.draw,power.limit",
            "--format=csv,noheader,nounits",
        ],
    )
    .map(|csv| csv.lines().filter_map(nvidia_line).collect())
    .unwrap_or_default()
}

fn nvidia_line(line: &str) -> Option<Value> {
    let f: Vec<_> = line.split(',').map(str::trim).collect();
    (f.len() == 9).then(|| {
        json!({"vendor":"nvidia", "name":f[0], "uuid":f[1], "device_id":f[2],
        "total_memory_bytes":f[3].parse::<u64>().ok().map(|v| v*1024*1024),
        "available_memory_bytes":f[4].parse::<u64>().ok().map(|v| v*1024*1024),
        "driver":f[5], "compute_capability":f[6], "memory_kind":"dedicated",
        "power":power(
            f[7].parse().map_err(|_| format!("nvidia-smi reported power.draw as {}", f[7])),
            f[8].parse().ok(),
            "GPU board, all workloads",
            "nvidia-smi power.draw",
        )})
    })
}

/// One reading with what it covers. An unreadable sensor is null with the reason, never 0.
fn power(watts: Result<f64, String>, limit: Option<f64>, scope: &str, source: &str) -> Value {
    let (watts, reason) = match watts {
        Ok(watts) => (Some(watts), None),
        Err(reason) => (None, Some(reason)),
    };
    json!({"watts":watts, "limit_watts":limit, "scope":scope, "source":source, "reason":reason})
}

fn device_power(accelerator: &Value) -> Value {
    if accelerator["vendor"] == "apple" {
        return power(
            Err("powermetrics needs root; LocalFleet does not run it".into()),
            None,
            "Apple SoC",
            "powermetrics",
        );
    }
    if accelerator["class"] == "/sys/class/drm" && accelerator["vendor_pci_id"] == "0x1002" {
        let node = accelerator["node"].as_str().unwrap_or("");
        return hwmon_power(&Path::new("/sys/class/drm").join(node).join("device"));
    }
    power(
        Err("no power sensor is read for this device".into()),
        None,
        "not measured",
        "none",
    )
}

/// amdgpu's hwmon `power1_average`, in microwatts, as an unprivileged user can read it.
fn hwmon_power(device: &Path) -> Value {
    let (scope, source) = ("AMDGPU SoC sensor, not wall power", "hwmon power1_average");
    let mut dirs: Vec<_> = std::fs::read_dir(device.join("hwmon"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("power1_average").exists())
        .collect();
    dirs.sort();
    let Some(dir) = dirs.first() else {
        return power(
            Err("no hwmon power1_average for this device".into()),
            None,
            scope,
            source,
        );
    };
    let read = |name: &str| {
        std::fs::read_to_string(dir.join(name))
            .map_err(|e| format!("{name} unreadable: {e}"))?
            .trim()
            .parse::<f64>()
            .map(|microwatts| microwatts / 1e6)
            .map_err(|e| format!("{name} unparsable: {e}"))
    };
    power(
        read("power1_average"),
        read("power1_cap").ok(),
        scope,
        source,
    )
}

/// A drm card that nvidia-smi already lists is the same physical device; keep the named entry.
/// nvidia-smi's `pci.device_id` is the device id followed by the vendor id (`0x280510DE`).
fn drop_duplicates(accelerators: &mut Vec<Value>) {
    let named: Vec<String> = accelerators
        .iter()
        .filter(|a| a["vendor"] == "nvidia")
        .filter_map(|a| a["device_id"].as_str().map(str::to_ascii_lowercase))
        .collect();
    accelerators.retain(|a| {
        let (Some(device), Some(vendor)) = (a["device_id"].as_str(), a["vendor_pci_id"].as_str())
        else {
            return true;
        };
        let id = format!("{device}{}", vendor.trim_start_matches("0x")).to_ascii_lowercase();
        a["class"] != "/sys/class/drm" || !named.contains(&id)
    });
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

#[cfg(test)]
mod tests {
    use super::{drop_duplicates, hwmon_power, nvidia_line};
    use serde_json::json;

    #[test]
    fn nvidia_power_is_null_with_a_reason_when_not_reported() {
        let gpu =
            nvidia_line("RTX, GPU-1, 0x280510DE, 16380, 15000, 580.1, 8.9, 23.45, 150.00").unwrap();
        assert_eq!(gpu["power"]["watts"], 23.45);
        assert_eq!(gpu["power"]["limit_watts"], 150.0);
        assert!(gpu["power"]["reason"].is_null());
        let gpu =
            nvidia_line("RTX, GPU-1, 0x280510DE, 16380, 15000, 580.1, 8.9, [N/A], [N/A]").unwrap();
        assert!(gpu["power"]["watts"].is_null() && gpu["power"]["limit_watts"].is_null());
        assert_eq!(
            gpu["power"]["reason"],
            "nvidia-smi reported power.draw as [N/A]"
        );
    }

    #[test]
    fn hwmon_power_reads_microwatts_and_names_a_missing_sensor() {
        let device = std::env::temp_dir().join(format!("hwmon-test-{}", std::process::id()));
        let hwmon = device.join("hwmon/hwmon3");
        std::fs::create_dir_all(&hwmon).unwrap();
        let missing = hwmon_power(&device);
        assert!(missing["watts"].is_null());
        assert_eq!(missing["reason"], "no hwmon power1_average for this device");
        std::fs::write(hwmon.join("power1_average"), "15250000\n").unwrap();
        let read = hwmon_power(&device);
        std::fs::remove_dir_all(&device).unwrap();
        assert_eq!(read["watts"], 15.25);
        assert!(read["limit_watts"].is_null() && read["reason"].is_null());
        assert_eq!(read["scope"], "AMDGPU SoC sensor, not wall power");
    }

    #[test]
    fn a_drm_card_listed_by_nvidia_smi_is_reported_once() {
        let mut accelerators = vec![
            json!({"vendor":"nvidia","device_id":"0x280510DE"}),
            json!({"class":"/sys/class/drm","vendor_pci_id":"0x10de","device_id":"0x2805"}),
            json!({"class":"/sys/class/drm","vendor_pci_id":"0x1002","device_id":"0x150e"}),
            json!({"class":"/sys/class/accel","vendor_pci_id":"0x1022","device_id":"0x17f0"}),
        ];
        drop_duplicates(&mut accelerators);
        assert_eq!(accelerators.len(), 3);
        assert!(accelerators.iter().all(|a| a["vendor_pci_id"] != "0x10de"));
    }
}
