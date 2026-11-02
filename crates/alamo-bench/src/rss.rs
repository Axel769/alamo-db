//! Resident memory of the process.
//!
//! §15 asks for two distinct numbers: **RAM / logical size** (how much the
//! layout costs compared to the bytes that arrived from the wire) and
//! **peak RSS / steady RSS** (if construction allocates twice as much and
//! then releases, the peak is what the user pays).
//!
//! `RAM / logical size` can be computed from the layout (§4.8) and does not
//! need the operating system. The peak, instead, is measured only here.

/// Current RSS in bytes, if obtainable.
///
/// On macOS it goes through `task_info(MACH_TASK_BASIC_INFO)`. If a version
/// of `libc` does not export those symbols, replace the body with `None`:
/// you lose the peak/steady ratio, not the other metrics.
#[cfg(target_os = "macos")]
#[allow(deprecated)] // `libc::mach_task_self` is deprecated in favour of the
                     // `mach2` crate. Not worth one more dependency for one
                     // line of instrumentation: if it disappears one day, we
                     // switch to `mach2`.
pub fn current_rss() -> Option<u64> {
    use std::mem;
    let mut info: libc::mach_task_basic_info = unsafe { mem::zeroed() };
    let mut count = (mem::size_of::<libc::mach_task_basic_info>()
        / mem::size_of::<libc::natural_t>()) as libc::mach_msg_type_number_t;
    let kr = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as libc::task_info_t,
            &mut count,
        )
    };
    if kr == libc::KERN_SUCCESS {
        Some(info.resident_size)
    } else {
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn current_rss() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(resident_pages * 4096)
}

#[cfg(not(unix))]
pub fn current_rss() -> Option<u64> {
    // TODO(M0, Windows): `GetProcessMemoryInfo` via `windows-sys`. Windows is
    // one of the two real gates of §15 phase 3, so this line must be closed
    // before declaring M0 passed — not after.
    None
}

/// Peak RSS since process start, in bytes.
#[cfg(unix)]
pub fn peak_rss() -> Option<u64> {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return None;
    }
    let raw = usage.ru_maxrss as u64;
    // ⚠️ `ru_maxrss` is in **bytes** on macOS and in **kilobytes** on Linux.
    // Mixing up the two units produces a factor of 1024 in a metric that has
    // a threshold at 1.3x: it would have gone unnoticed on only one of the
    // two platforms.
    #[cfg(target_os = "macos")]
    {
        Some(raw)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(raw * 1024)
    }
}

#[cfg(not(unix))]
pub fn peak_rss() -> Option<u64> {
    None
}

pub fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.1} {}", UNITS[u])
}
