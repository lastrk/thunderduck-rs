/// Hardware profile used to configure DuckDB's resource limits.
pub struct HardwareProfile {
    /// Number of OS threads available for DuckDB parallelism.
    pub cpu_threads: usize,
    /// Memory limit in GB.
    pub memory_limit_gb: usize,
}

impl HardwareProfile {
    pub fn detect() -> Self {
        let cpu_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let memory_limit_gb = Self::detect_memory_gb();
        Self {
            cpu_threads,
            memory_limit_gb,
        }
    }

    #[cfg(target_os = "linux")]
    fn detect_memory_gb() -> usize {
        std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|contents| {
                contents
                    .lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<usize>().ok())
                    .map(|kb| (kb / (1024 * 1024)).max(1))
            })
            .unwrap_or(4)
    }

    #[cfg(target_os = "macos")]
    fn detect_memory_gb() -> usize {
        let output = std::process::Command::new("sysctl")
            .arg("-n")
            .arg("hw.memsize")
            .output()
            .ok();
        output
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| s.trim().parse::<usize>().ok())
            .map(|bytes| (bytes / (1024 * 1024 * 1024)).max(1))
            .unwrap_or(4)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn detect_memory_gb() -> usize {
        4
    }
}
