use std::time::{Duration, Instant};

use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct Application {
    name: String,
    bytes: u64,
}

#[derive(Clone, Default, Serialize)]
pub struct Usage {
    windows_bytes: Option<u64>,
    applications: Option<Vec<Application>>,
}

#[derive(Default)]
pub struct Cache(Option<(Instant, Usage)>);

impl Cache {
    pub async fn sample(&mut self) -> Usage {
        if let Some((sampled, usage)) = &self.0
            && sampled.elapsed() < Duration::from_secs(4)
        {
            return usage.clone();
        }
        let usage = tokio::task::spawn_blocking(collect)
            .await
            .unwrap_or_default();
        self.0 = Some((Instant::now(), usage.clone()));
        usage
    }
}

fn collect() -> Usage {
    #[cfg(windows)]
    return Usage {
        windows_bytes: windows_memory(),
        applications: applications(),
    };

    #[cfg(not(windows))]
    Usage::default()
}

#[cfg(any(windows, test))]
fn rank(processes: impl IntoIterator<Item = (String, u64)>) -> Vec<Application> {
    let mut totals = std::collections::HashMap::<String, u64>::new();
    for (name, bytes) in processes {
        *totals.entry(name).or_default() += bytes;
    }
    let mut apps: Vec<_> = totals
        .into_iter()
        .filter_map(|(name, bytes)| {
            (bytes >= 300 * 1024 * 1024).then_some(Application { name, bytes })
        })
        .collect();
    apps.sort_unstable_by(|left, right| {
        right
            .bytes
            .cmp(&left.bytes)
            .then(left.name.cmp(&right.name))
    });
    apps.truncate(7);
    apps
}

#[cfg(windows)]
fn applications() -> Option<Vec<Application>> {
    use std::collections::HashMap;
    use std::ffi::c_void;

    const TH32CS_SNAPPROCESS: u32 = 2;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const INVALID_HANDLE_VALUE: *mut c_void = -1_isize as *mut c_void;

    #[repr(C)]
    struct ProcessEntry {
        size: u32,
        usage: u32,
        pid: u32,
        default_heap: usize,
        module_id: u32,
        threads: u32,
        parent_pid: u32,
        priority: i32,
        flags: u32,
        executable: [u16; 260],
    }

    #[repr(C)]
    #[derive(Default)]
    struct MemoryCounters {
        size: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        page_file: usize,
        peak_page_file: usize,
        private_usage: usize,
        private_working_set: usize,
        shared_commit_usage: u64,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> *mut c_void;
        fn Process32FirstW(handle: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn Process32NextW(handle: *mut c_void, entry: *mut ProcessEntry) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn K32GetProcessMemoryInfo(
            handle: *mut c_void,
            counters: *mut MemoryCounters,
            size: u32,
        ) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    struct Handle(*mut c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            // Only valid, owned handles enter this wrapper.
            unsafe { CloseHandle(self.0) };
        }
    }

    // The snapshot and memory buffers match the documented Win32 layouts.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE || snapshot.is_null() {
        return None;
    }
    let snapshot = Handle(snapshot);
    let mut entry: ProcessEntry = unsafe { std::mem::zeroed() };
    entry.size = size_of::<ProcessEntry>() as u32;
    if unsafe { Process32FirstW(snapshot.0, &mut entry) } == 0 {
        return None;
    }

    let mut processes = HashMap::new();
    loop {
        let len = entry
            .executable
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(entry.executable.len());
        let executable = String::from_utf16_lossy(&entry.executable[..len]).to_lowercase();
        processes.insert(entry.pid, (executable, entry.parent_pid));
        if unsafe { Process32NextW(snapshot.0, &mut entry) } == 0 {
            break;
        }
    }
    drop(snapshot);

    let ollama = |name: &str| {
        matches!(
            name,
            "ollama.exe" | "ollama app.exe" | "ollama_app.exe" | "ollama_llama_server.exe"
        )
    };
    let mut readings = Vec::new();
    for (&pid, (name, parent_pid)) in &processes {
        if pid == 0 || name.is_empty() {
            continue;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            continue;
        }
        let process = Handle(process);
        let size = size_of::<MemoryCounters>() as u32;
        let mut counters = MemoryCounters {
            size,
            ..MemoryCounters::default()
        };
        if unsafe { K32GetProcessMemoryInfo(process.0, &mut counters, size) } == 0 {
            continue;
        }
        let app = if ollama(name)
            || (name == "llama-server.exe"
                && processes
                    .get(parent_pid)
                    .is_some_and(|(parent, _)| ollama(parent)))
        {
            "Ollama"
        } else {
            name.strip_suffix(".exe").unwrap_or(name)
        };
        readings.push((app.to_owned(), counters.private_working_set as u64));
    }
    if readings.is_empty() {
        return None;
    }
    Some(rank(readings))
}

#[cfg(windows)]
fn windows_memory() -> Option<u64> {
    use std::{ffi::c_void, ptr};

    const COUNTERS: [&str; 4] = [
        r"\Memory\Pool Nonpaged Bytes",
        r"\Memory\Pool Paged Resident Bytes",
        r"\Memory\System Code Resident Bytes",
        r"\Memory\System Driver Resident Bytes",
    ];
    const PDH_FMT_LARGE: u32 = 0x400;
    const PDH_FMT_NOSCALE: u32 = 0x1000;
    const PDH_FMT_NOCAP100: u32 = 0x8000;

    #[repr(C)]
    #[derive(Default)]
    struct FormattedCounter {
        status: u32,
        // The largeValue member of the PDH_FMT_COUNTERVALUE union.
        value: i64,
    }

    #[link(name = "pdh")]
    unsafe extern "system" {
        fn PdhOpenQueryW(source: *const u16, data: usize, query: *mut *mut c_void) -> i32;
        fn PdhAddEnglishCounterW(
            query: *mut c_void,
            path: *const u16,
            data: usize,
            counter: *mut *mut c_void,
        ) -> i32;
        fn PdhCollectQueryData(query: *mut c_void) -> i32;
        fn PdhGetFormattedCounterValue(
            counter: *mut c_void,
            format: u32,
            counter_type: *mut u32,
            value: *mut FormattedCounter,
        ) -> i32;
        fn PdhCloseQuery(query: *mut c_void) -> i32;
    }

    struct Query(*mut c_void);
    impl Drop for Query {
        fn drop(&mut self) {
            // Closing the query also releases all counters that belong to it.
            unsafe { PdhCloseQuery(self.0) };
        }
    }

    let mut query = ptr::null_mut();
    if unsafe { PdhOpenQueryW(ptr::null(), 0, &mut query) } != 0 {
        return None;
    }
    let query = Query(query);
    let mut handles = [ptr::null_mut(); COUNTERS.len()];
    for (name, handle) in COUNTERS.iter().zip(&mut handles) {
        let path: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        if unsafe { PdhAddEnglishCounterW(query.0, path.as_ptr(), 0, handle) } != 0 {
            return None;
        }
    }
    if unsafe { PdhCollectQueryData(query.0) } != 0 {
        return None;
    }
    handles.into_iter().try_fold(0_u64, |total, handle| {
        let mut counter = FormattedCounter::default();
        // The output buffer matches the PDH structure for the requested 64-bit format.
        let status = unsafe {
            PdhGetFormattedCounterValue(
                handle,
                PDH_FMT_LARGE | PDH_FMT_NOSCALE | PDH_FMT_NOCAP100,
                ptr::null_mut(),
                &mut counter,
            )
        };
        if status != 0 || !matches!(counter.status, 0 | 1) {
            return None;
        }
        total.checked_add(u64::try_from(counter.value).ok()?)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_before_filtering_and_caps_the_sorted_list() {
        const MIB: u64 = 1024 * 1024;
        let mut readings = vec![
            ("browser".into(), 200 * MIB),
            ("browser".into(), 100 * MIB),
            ("small".into(), 299 * MIB),
        ];
        let apps = rank(readings.clone());
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "browser");
        assert_eq!(apps[0].bytes, 300 * MIB);

        readings.extend((1..=8).map(|n| (format!("app{n}"), (300 + n) * MIB)));
        let apps = rank(readings);
        assert_eq!(apps.len(), 7);
        assert_eq!(apps[0].name, "app8");
        assert_eq!(apps[6].name, "app2");
    }
}
