//! Background system statistics for the status bar.
//!
//! RAM is this process's resident working set, read through the native OS API
//! (`GetProcessMemoryInfo` on Windows, `/proc/self/status` on Linux). Total
//! physical RAM is sampled once only to describe the app's share in the
//! tooltip. GPU utilization and VRAM come from `nvidia-smi`, queried on the
//! monitor thread every other second. A missing driver or tool simply leaves
//! the GPU fields empty; the editor never blocks on monitoring.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use bevy::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GpuStats {
    pub utilization: Option<f32>,
    pub used_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct SystemStats {
    /// Resident working-set bytes used by this process.
    pub process_ram: Option<u64>,
    /// Total physical RAM, used only for the app's share and the tooltip.
    pub system_ram: Option<u64>,
    pub gpu: Option<GpuStats>,
}

#[derive(Resource, Clone)]
pub struct SystemStatsRes(Arc<RwLock<SystemStats>>);

impl SystemStatsRes {
    pub fn snapshot(&self) -> SystemStats {
        self.0.read().map(|stats| stats.clone()).unwrap_or_default()
    }
}

pub struct SystemStatsPlugin;

impl Plugin for SystemStatsPlugin {
    fn build(&self, app: &mut App) {
        let shared = Arc::new(RwLock::new(SystemStats::default()));
        let worker = shared.clone();
        let _ = std::thread::Builder::new()
            .name("n-shade-system-stats".into())
            .spawn(move || {
                // The installed RAM does not change while the app runs.
                let system_ram = read_system_ram_total();
                let mut tick = 0u32;
                loop {
                    let process_ram = read_process_ram();
                    let gpu = if tick.is_multiple_of(2) {
                        read_gpu()
                    } else {
                        None
                    };
                    if let Ok(mut stats) = worker.write() {
                        if let Some(used) = process_ram {
                            stats.process_ram = Some(used);
                        }
                        if let Some(total) = system_ram {
                            stats.system_ram = Some(total);
                        }
                        // `nvidia-smi` is a small process; sample it every
                        // other second while RAM updates every second.
                        if tick.is_multiple_of(2) {
                            stats.gpu = gpu;
                        }
                    }
                    tick = tick.wrapping_add(1);
                    std::thread::sleep(Duration::from_secs(1));
                }
            });
        app.insert_resource(SystemStatsRes(shared));
    }
}

#[cfg(target_os = "windows")]
fn read_process_ram() -> Option<u64> {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    unsafe {
        GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb).ok()?;
    }
    Some(counters.WorkingSetSize as u64)
}

#[cfg(target_os = "windows")]
fn read_system_ram_total() -> Option<u64> {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe {
        GlobalMemoryStatusEx(&mut status).ok()?;
    }
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

#[cfg(target_os = "linux")]
fn read_process_ram() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/status").ok()?;
    parse_vm_rss(&text)
}

#[cfg(target_os = "linux")]
fn parse_vm_rss(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(parse_kib)
}

#[cfg(target_os = "linux")]
fn read_system_ram_total() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(parse_kib)
        .filter(|total| *total > 0)
}

#[cfg(target_os = "linux")]
fn parse_kib(value: &str) -> Option<u64> {
    value
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
        .map(|kib| kib * 1024)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn read_process_ram() -> Option<u64> {
    None
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn read_system_ram_total() -> Option<u64> {
    None
}

fn read_gpu() -> Option<GpuStats> {
    let mut command = std::process::Command::new("nvidia-smi");
    command.args([
        "--query-gpu=utilization.gpu,memory.used,memory.total",
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().ok()?;
    if !output.status.success() {
        return None;
    }
    parse_smi(&String::from_utf8_lossy(&output.stdout))
}

fn parse_smi(output: &str) -> Option<GpuStats> {
    let line = output.lines().next()?.trim();
    let mut fields = line.split(',').map(str::trim);
    let utilization = fields.next().and_then(|value| value.parse::<f32>().ok());
    let used_mb = fields.next()?.parse::<f64>().ok()?;
    let total_mb = fields.next()?.parse::<f64>().ok()?;
    if total_mb <= 0.0 {
        return None;
    }
    Some(GpuStats {
        utilization,
        used_bytes: (used_mb * 1_048_576.0).max(0.0) as u64,
        total_bytes: (total_mb * 1_048_576.0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::parse_smi;

    #[test]
    fn process_ram_is_available() {
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            let bytes = super::read_process_ram().expect("process memory is readable");
            assert!(bytes > 0);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn parses_vm_rss_from_proc_status() {
        let text = "Name:\tn-shade\nVmRSS:\t  204800 kB\n";
        assert_eq!(super::parse_vm_rss(text), Some(204_800 * 1024));
    }

    #[test]
    fn parses_nvidia_smi_output() {
        let stats = parse_smi("37, 2048, 8192\n").expect("valid output");
        assert_eq!(stats.utilization, Some(37.0));
        assert_eq!(stats.used_bytes, 2048 * 1_048_576);
        assert_eq!(stats.total_bytes, 8192 * 1_048_576);

        let unavailable = parse_smi("[N/A], 1024, 6144\n").expect("memory still valid");
        assert_eq!(unavailable.utilization, None);
        assert_eq!(unavailable.used_bytes, 1024 * 1_048_576);

        assert!(parse_smi("garbage").is_none());
        assert!(parse_smi("12, 512, 0\n").is_none());
    }
}
