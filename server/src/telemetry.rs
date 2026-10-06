//! CPU / GPU / RAM telemetry using the `sysinfo` crate.
//! GPU metrics are read from NVML when available; gracefully absent on AMD/Intel.

use protocol::{CpuInfo, GpuInfo, TelemetryState};
use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

// Global system handle (re-used across calls for CPU percent calculation).
use std::sync::Mutex;

static SYS: Mutex<Option<System>> = Mutex::new(None);

pub fn snapshot() -> TelemetryState {
    let cpu = cpu_info();
    let gpu = gpu_info();
    TelemetryState { cpu: Some(cpu), gpu }
}

fn cpu_info() -> CpuInfo {
    let mut guard = SYS.lock().unwrap();
    let sys = guard.get_or_insert_with(|| {
        System::new_with_specifics(
            RefreshKind::new()
                .with_cpu(CpuRefreshKind::everything())
                .with_memory(MemoryRefreshKind::everything()),
        )
    });

    sys.refresh_cpu_usage();
    sys.refresh_memory();

    let util = sys.global_cpu_info().cpu_usage();

    // CPU temp: try reading from hwmon (linux sysfs).
    let temp = read_cpu_temp();

    let mem_total_gb = sys.total_memory() as f32 / 1024.0 / 1024.0 / 1024.0;
    let mem_used_gb = (sys.total_memory() - sys.available_memory()) as f32 / 1024.0 / 1024.0 / 1024.0;

    CpuInfo {
        util,
        temp,
        mem_used: (mem_used_gb * 10.0).round() / 10.0,
        mem_total: (mem_total_gb * 10.0).round() / 10.0,
    }
}

fn read_cpu_temp() -> Option<f32> {
    // Walk /sys/class/hwmon/hwmonN/temp*_input looking for a CPU package sensor.
    let hwmon = std::path::Path::new("/sys/class/hwmon");
    if !hwmon.exists() { return None; }

    let Ok(entries) = std::fs::read_dir(hwmon) else { return None };
    for entry in entries.flatten() {
        let path = entry.path();
        // Check "name" file to identify the sensor
        let name = std::fs::read_to_string(path.join("name")).unwrap_or_default();
        let name = name.trim();
        // Common CPU thermal zones
        if !["coretemp", "k10temp", "zenpower", "cpu_thermal"].contains(&name) {
            continue;
        }
        // Find temp1_input (package/die temp)
        let temp_path = path.join("temp1_input");
        if let Ok(raw) = std::fs::read_to_string(&temp_path) {
            if let Ok(millic) = raw.trim().parse::<i32>() {
                return Some(millic as f32 / 1000.0);
            }
        }
    }
    None
}

fn gpu_info() -> Option<GpuInfo> {
    // Try NVML via nvidia-smi (avoids a native library dependency).
    let out = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;

    if !out.status.success() { return None; }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().next()?;
    let parts: Vec<f32> = line.split(',')
        .map(|s| s.trim().parse::<f32>().unwrap_or(0.0))
        .collect();

    if parts.len() < 4 { return None; }
    Some(GpuInfo {
        util: parts[0],
        temp: parts[1],
        vram_used: parts[2],
        vram_total: parts[3],
    })
}
