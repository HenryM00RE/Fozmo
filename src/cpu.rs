use std::time::Instant;

const MIN_SAMPLE_INTERVAL_SECS: f64 = 0.25;

#[derive(Debug)]
pub struct ProcessCpuMonitor {
    last_process_time_100ns: Option<u64>,
    last_wall_time: Option<Instant>,
    last_percent: f32,
    logical_cpus: f64,
}

impl ProcessCpuMonitor {
    pub fn new() -> Self {
        Self {
            last_process_time_100ns: None,
            last_wall_time: None,
            last_percent: 0.0,
            logical_cpus: std::thread::available_parallelism()
                .map(|n| n.get() as f64)
                .unwrap_or(1.0)
                .max(1.0),
        }
    }

    pub fn sample_percent(&mut self) -> f32 {
        let Some(process_time_100ns) = process_time_100ns() else {
            return self.last_percent;
        };
        let now = Instant::now();

        let (Some(last_process_time_100ns), Some(last_wall_time)) =
            (self.last_process_time_100ns, self.last_wall_time)
        else {
            self.last_process_time_100ns = Some(process_time_100ns);
            self.last_wall_time = Some(now);
            return self.last_percent;
        };

        let elapsed_secs = now.duration_since(last_wall_time).as_secs_f64();
        if elapsed_secs < MIN_SAMPLE_INTERVAL_SECS {
            return self.last_percent;
        }

        self.last_process_time_100ns = Some(process_time_100ns);
        self.last_wall_time = Some(now);

        let process_delta_secs =
            process_time_100ns.saturating_sub(last_process_time_100ns) as f64 / 10_000_000.0;
        self.last_percent = ((process_delta_secs / elapsed_secs) * 100.0 / self.logical_cpus)
            .clamp(0.0, 100.0) as f32;
        self.last_percent
    }
}

impl Default for ProcessCpuMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(target_os = "windows")]
fn process_time_100ns() -> Option<u64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    fn filetime_to_u64(filetime: FILETIME) -> u64 {
        ((filetime.dwHighDateTime as u64) << 32) | filetime.dwLowDateTime as u64
    }

    unsafe {
        let process = GetCurrentProcess();
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user).ok()?;
        Some(filetime_to_u64(kernel) + filetime_to_u64(user))
    }
}

#[cfg(target_os = "macos")]
fn process_time_100ns() -> Option<u64> {
    use std::mem::MaybeUninit;

    unsafe {
        let mut usage = MaybeUninit::<libc::rusage>::uninit();
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        let usage = usage.assume_init();
        let user_100ns = timeval_to_100ns(usage.ru_utime);
        let system_100ns = timeval_to_100ns(usage.ru_stime);
        Some(user_100ns + system_100ns)
    }
}

#[cfg(target_os = "macos")]
fn timeval_to_100ns(timeval: libc::timeval) -> u64 {
    (timeval.tv_sec as u64 * 10_000_000) + (timeval.tv_usec as u64 * 10)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn process_time_100ns() -> Option<u64> {
    None
}
