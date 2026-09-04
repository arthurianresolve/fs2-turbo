use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::ptr;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::Observation;
use crate::{Result, invalid_data};

#[derive(Clone, Copy, Default)]
#[repr(C)]
struct ProcessorInfo {
    mask: usize,
    relationship: i32,
    reserved: [u64; 2],
}

#[derive(Default)]
#[repr(C)]
struct PowerStatus {
    ac: u8,
    battery_flags: u8,
    battery_percent: u8,
    system_flags: u8,
    lifetime: u32,
    full_lifetime: u32,
}

#[derive(Default)]
#[repr(C)]
struct CounterValue {
    status: u32,
    value: f64,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetLogicalProcessorInformation(buffer: *mut ProcessorInfo, length: *mut u32) -> i32;
    fn GetActiveProcessorGroupCount() -> u16;
    fn GetCurrentProcess() -> *mut c_void;
    fn GetProcessAffinityMask(process: *mut c_void, mask: *mut usize, system: *mut usize) -> i32;
    fn SetProcessAffinityMask(process: *mut c_void, mask: usize) -> i32;
    fn GetSystemPowerStatus(status: *mut PowerStatus) -> i32;
}

#[link(name = "pdh")]
unsafe extern "system" {
    fn PdhOpenQueryW(source: *const u16, user: usize, query: *mut *mut c_void) -> u32;
    fn PdhAddEnglishCounterW(
        query: *mut c_void,
        path: *const u16,
        user: usize,
        counter: *mut *mut c_void,
    ) -> u32;
    fn PdhCollectQueryData(query: *mut c_void) -> u32;
    fn PdhGetFormattedCounterValue(
        counter: *mut c_void,
        format: u32,
        kind: *mut u32,
        value: *mut CounterValue,
    ) -> u32;
    fn PdhCloseQuery(query: *mut c_void) -> u32;
}

fn pdh(status: u32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "native PDH counter failed: {status:#010x}"
        )))
    }
}

fn affinity() -> Result<usize> {
    let mut mask = 0;
    let mut system = 0;
    // The borrowed current-process handle is valid; both outputs are initialized.
    if unsafe { GetProcessAffinityMask(GetCurrentProcess(), &mut mask, &mut system) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if mask == 0 {
        return Err(invalid_data("empty process affinity mask"));
    }
    Ok(mask)
}

pub(in super::super) struct AffinityGuard {
    previous: Option<usize>,
}

impl AffinityGuard {
    pub(in super::super) fn pin(cpu: u32) -> Result<Self> {
        let previous = affinity()?;
        let mask = 1usize
            .checked_shl(cpu)
            .ok_or_else(|| invalid_data("CPU exceeds affinity mask"))?;
        if previous & mask == 0 {
            return Err(invalid_data("selected CPU no longer allowed"));
        }
        // Only this process and its subsequently created children are affected.
        if unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            previous: Some(previous),
        })
    }

    pub(in super::super) fn restore(&mut self) -> Result<()> {
        if let Some(mask) = self.previous {
            // Restore the mask captured before this guard changed it.
            if unsafe { SetProcessAffinityMask(GetCurrentProcess(), mask) } == 0 {
                return Err(io::Error::last_os_error().into());
            }
            self.previous = None;
        }
        Ok(())
    }
}

impl Drop for AffinityGuard {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            eprintln!("unable to restore benchmark process affinity: {error}");
        }
    }
}

fn core_masks() -> Result<Vec<u64>> {
    // This collector deliberately rejects rather than truncates multi-group hosts.
    if unsafe { GetActiveProcessorGroupCount() } != 1 {
        return Err(invalid_data(
            "idle admission requires one Windows processor group",
        ));
    }
    let mut length = 0;
    // A null buffer requests the required byte count; no data are read on failure.
    if unsafe { GetLogicalProcessorInformation(ptr::null_mut(), &mut length) } != 0
        || io::Error::last_os_error().raw_os_error() != Some(122)
    {
        return Err(invalid_data("unable to size native CPU topology"));
    }
    let stride = size_of::<ProcessorInfo>();
    let capacity = length as usize;
    if capacity == 0 || capacity > 1024 * 1024 || !capacity.is_multiple_of(stride) {
        return Err(invalid_data("invalid native CPU topology buffer length"));
    }
    let mut buffer = vec![ProcessorInfo::default(); capacity / stride];
    // The vector is initialized, aligned, and holds exactly the declared capacity.
    if unsafe { GetLogicalProcessorInformation(buffer.as_mut_ptr(), &mut length) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if length as usize > capacity || !(length as usize).is_multiple_of(stride) {
        return Err(invalid_data("native CPU topology changed during capture"));
    }
    let mut masks = buffer[..length as usize / stride]
        .iter()
        .filter(|row| row.relationship == 0)
        .map(|row| row.mask as u64)
        .collect::<Vec<_>>();
    masks.sort_unstable();
    Ok(masks)
}

fn ac_online() -> Result<bool> {
    let mut status = PowerStatus::default();
    // The API writes a complete SYSTEM_POWER_STATUS into aligned live storage.
    if unsafe { GetSystemPowerStatus(&mut status) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(status.ac == 1)
}

struct Query(*mut c_void);

impl Drop for Query {
    fn drop(&mut self) {
        // Closing this query releases only the counters owned by this collector.
        unsafe { PdhCloseQuery(self.0) };
    }
}

pub(super) fn observe() -> Result<Observation> {
    let core_masks = core_masks()?;
    let allowed_mask = affinity()? as u64;
    let mask = core_masks.iter().fold(0, |all, mask| all | mask);
    let logical_cpus = (0..64)
        .filter(|cpu| mask & (1u64 << cpu) != 0)
        .collect::<Vec<_>>();
    if logical_cpus.is_empty() || !ac_online()? {
        return Err(invalid_data(
            "host admission requires CPU topology and known AC power",
        ));
    }
    eprintln!("host admission: 60-second settling, then 30 native counter intervals; no retries");
    thread::sleep(Duration::from_secs(60));
    let mut raw_query = ptr::null_mut();
    // A null data source requests local real-time counters, with an output handle.
    pdh(unsafe { PdhOpenQueryW(ptr::null(), 0, &mut raw_query) })?;
    let query = Query(raw_query);
    let mut counters = Vec::new();
    for cpu in &logical_cpus {
        let path = format!("\\Processor Information(0,{cpu})\\% Processor Time")
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut counter = ptr::null_mut();
        // The path is local, language-neutral, NUL-terminated, and live for the call.
        pdh(unsafe { PdhAddEnglishCounterW(query.0, path.as_ptr(), 0, &mut counter) })?;
        counters.push(counter);
    }
    // Rate counters need an initial observation before the first timed interval.
    pdh(unsafe { PdhCollectQueryData(query.0) })?;
    let mut busy_samples = Vec::with_capacity(30);
    let mut interval_seconds = Vec::with_capacity(30);
    let mut previous = Instant::now();
    for _ in 0..30 {
        thread::sleep(Duration::from_secs(1));
        // The query and its counters remain owned and live until collection ends.
        pdh(unsafe { PdhCollectQueryData(query.0) })?;
        let now = Instant::now();
        interval_seconds.push(now.duration_since(previous).as_secs_f64());
        previous = now;
        let mut sample = Vec::with_capacity(counters.len());
        for &counter in &counters {
            let mut value = CounterValue::default();
            // PDH_FMT_DOUBLE selects the double union member; status is checked below.
            pdh(unsafe {
                PdhGetFormattedCounterValue(counter, 0x200, ptr::null_mut(), &mut value)
            })?;
            if value.status > 1 {
                return Err(invalid_data("native CPU counter data are unavailable"));
            }
            sample.push(value.value);
        }
        busy_samples.push(sample);
    }
    if core_masks != self::core_masks()? || allowed_mask != affinity()? as u64 {
        return Err(invalid_data(
            "CPU topology or allowed affinity changed during admission",
        ));
    }
    Ok(Observation {
        core_masks,
        allowed_mask,
        logical_cpus,
        busy_samples,
        interval_seconds,
        ac_online: ac_online()?,
        completed_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    })
}
