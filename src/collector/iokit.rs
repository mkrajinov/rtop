//! Disk, GPU, battery and core-type data on macOS live in the IOKit registry,
//! not in sysctl. Every driver object carries properties; some are
//! dictionaries of running statistics. `ioreg -r -c IOBlockStorageDriver`
//! (or `-c AppleSmartBattery`) shows the same data. Temperatures come from
//! the HID event system and fan speeds from the SMC, both also IOKit.

use std::ffi::{CStr, c_char, c_void};
use std::ptr::null;

use crate::model::{Battery, Fan, GpuInfo, IoCounters, Thermal};

type CfRef = *const c_void;
type IoObject = u32;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOServiceMatching(name: *const c_char) -> *mut c_void;
    fn IOServiceGetMatchingServices(main_port: u32, matching: *mut c_void, iter: *mut IoObject) -> i32;
    fn IOIteratorNext(iter: IoObject) -> IoObject;
    fn IOObjectRelease(obj: IoObject) -> i32;
    fn IORegistryEntryCreateCFProperty(entry: IoObject, key: CfRef, alloc: CfRef, options: u32) -> CfRef;
    fn IORegistryEntryCreateCFProperties(entry: IoObject, props: *mut CfRef, alloc: CfRef, options: u32) -> i32;
    fn IORegistryEntryFromPath(main_port: u32, path: *const c_char) -> IoObject;
    fn IORegistryEntryGetChildIterator(entry: IoObject, plane: *const c_char, iter: *mut IoObject) -> i32;
    fn IOServiceGetMatchingService(main_port: u32, matching: *mut c_void) -> IoObject;
    fn IOServiceOpen(service: IoObject, task: u32, kind: u32, conn: *mut u32) -> i32;
    fn IOServiceClose(conn: u32) -> i32;
    fn IOConnectCallStructMethod(
        conn: u32,
        selector: u32,
        input: *const c_void,
        in_size: usize,
        output: *mut c_void,
        out_size: *mut usize,
    ) -> i32;
    // Not in the public headers, but exported: the HID event system client
    // that powermetrics-free tools use to read Apple Silicon sensors.
    fn IOHIDEventSystemClientCreate(alloc: CfRef) -> CfRef;
    fn IOHIDEventSystemClientSetMatching(client: CfRef, matching: CfRef) -> i32;
    fn IOHIDEventSystemClientCopyServices(client: CfRef) -> CfRef;
    fn IOHIDServiceClientCopyProperty(service: CfRef, key: CfRef) -> CfRef;
    fn IOHIDServiceClientCopyEvent(service: CfRef, kind: i64, options: i32, timestamp: i64) -> CfRef;
    fn IOHIDEventGetFloatValue(event: CfRef, field: i32) -> f64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(alloc: CfRef, s: *const c_char, encoding: u32) -> CfRef;
    fn CFStringGetCString(s: CfRef, buf: *mut c_char, size: isize, encoding: u32) -> u8;
    fn CFDictionaryGetValue(dict: CfRef, key: CfRef) -> CfRef;
    fn CFNumberGetValue(num: CfRef, kind: isize, out: *mut c_void) -> u8;
    fn CFGetTypeID(cf: CfRef) -> usize;
    fn CFDictionaryGetTypeID() -> usize;
    fn CFNumberGetTypeID() -> usize;
    fn CFStringGetTypeID() -> usize;
    fn CFBooleanGetTypeID() -> usize;
    fn CFBooleanGetValue(b: CfRef) -> u8;
    fn CFDataGetTypeID() -> usize;
    fn CFDataGetBytePtr(d: CfRef) -> *const u8;
    fn CFDataGetLength(d: CfRef) -> isize;
    fn CFRelease(cf: CfRef);
    fn CFNumberCreate(alloc: CfRef, kind: isize, value: *const c_void) -> CfRef;
    fn CFDictionaryCreate(
        alloc: CfRef,
        keys: *const CfRef,
        values: *const CfRef,
        count: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CfRef;
    fn CFArrayGetCount(array: CfRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CfRef, i: isize) -> CfRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

const UTF8: u32 = 0x0800_0100;
const SINT64: isize = 4;

/// An owned CoreFoundation reference, released on drop.
struct Cf(CfRef);

impl Drop for Cf {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}

impl Cf {
    fn string(s: &CStr) -> Cf {
        Cf(unsafe { CFStringCreateWithCString(null(), s.as_ptr(), UTF8) })
    }

    fn is(&self, type_id: usize) -> bool {
        !self.0.is_null() && unsafe { CFGetTypeID(self.0) } == type_id
    }

    /// A number in this dictionary; `None` when missing or not a number.
    fn number(&self, key: &CStr) -> Option<i64> {
        number(get(self.0, key))
    }
}

/// A value in dictionary `dict`, borrowed (not retained): null when missing.
fn get(dict: CfRef, key: &CStr) -> CfRef {
    if dict.is_null() {
        return null();
    }
    let k = Cf::string(key);
    unsafe { CFDictionaryGetValue(dict, k.0) }
}

fn number(v: CfRef) -> Option<i64> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFNumberGetTypeID() } {
        return None;
    }
    let mut n = 0i64;
    (unsafe { CFNumberGetValue(v, SINT64, (&raw mut n).cast()) } != 0).then_some(n)
}

fn boolean(v: CfRef) -> Option<bool> {
    (!v.is_null() && unsafe { CFGetTypeID(v) == CFBooleanGetTypeID() }).then(|| unsafe { CFBooleanGetValue(v) } != 0)
}

/// Calls `f` for every registry entry of `class`.
fn each_service(class: &CStr, mut f: impl FnMut(IoObject)) {
    let mut iter = 0;
    // IOServiceGetMatchingServices consumes the matching dictionary.
    let matching = unsafe { IOServiceMatching(class.as_ptr()) };
    if matching.is_null() || unsafe { IOServiceGetMatchingServices(0, matching, &mut iter) } != 0 {
        return;
    }
    loop {
        let entry = unsafe { IOIteratorNext(iter) };
        if entry == 0 {
            break;
        }
        f(entry);
        unsafe { IOObjectRelease(entry) };
    }
    unsafe { IOObjectRelease(iter) };
}

fn property(entry: IoObject, key: &CStr) -> Cf {
    let k = Cf::string(key);
    Cf(unsafe { IORegistryEntryCreateCFProperty(entry, k.0, null(), 0) })
}

fn string_property(entry: IoObject, key: &CStr) -> Option<String> {
    string(property(entry, key).0)
}

fn string(v: CfRef) -> Option<String> {
    if v.is_null() || unsafe { CFGetTypeID(v) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0 as c_char; 256];
    let ok = unsafe { CFStringGetCString(v, buf.as_mut_ptr(), buf.len() as isize, UTF8) } != 0;
    ok.then(|| unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned())
}

/// Bytes read and written, summed over every IOBlockStorageDriver: one per
/// physical disk, disk image or external drive.
pub fn disk() -> Option<IoCounters> {
    let mut c = IoCounters { source: "IOBlockStorageDriver · Statistics".into(), ..Default::default() };
    let mut found = false;
    each_service(c"IOBlockStorageDriver", |e| {
        let stats = property(e, c"Statistics");
        if stats.is(unsafe { CFDictionaryGetTypeID() }) {
            found = true;
            c.read += stats.number(c"Bytes (Read)").unwrap_or(0) as u64;
            c.write += stats.number(c"Bytes (Write)").unwrap_or(0) as u64;
        }
    });
    found.then_some(c)
}

/// The busiest IOAccelerator (the GPU driver) and its PerformanceStatistics.
pub fn gpu() -> Option<GpuInfo> {
    let mut best: Option<GpuInfo> = None;
    each_service(c"IOAccelerator", |e| {
        let stats = property(e, c"PerformanceStatistics");
        if !stats.is(unsafe { CFDictionaryGetTypeID() }) {
            return;
        }
        let Some(busy) = stats.number(c"Device Utilization %") else { return };
        let mut name = string_property(e, c"model").unwrap_or_else(|| "GPU".into());
        let cores = property(e, c"gpu-core-count");
        if cores.is(unsafe { CFNumberGetTypeID() }) {
            let mut n = 0i64;
            if unsafe { CFNumberGetValue(cores.0, SINT64, (&raw mut n).cast()) } != 0 {
                name = format!("{name} · {n} cores");
            }
        }
        if best.as_ref().is_none_or(|b| busy as f32 > b.busy) {
            best = Some(GpuInfo {
                name,
                busy: busy as f32,
                mem_used: stats.number(c"In use system memory").map(|n| n as u64),
            });
        }
    });
    best
}

/// The battery gauge (AppleSmartBattery): charge, health, and the power
/// telemetry that also reports what the whole machine draws.
pub fn battery() -> Option<Battery> {
    let mut out = None;
    each_service(c"AppleSmartBattery", |e| {
        let mut props: CfRef = null();
        if unsafe { IORegistryEntryCreateCFProperties(e, &mut props, null(), 0) } != 0 {
            return;
        }
        let props = Cf(props);
        let p = props.0;
        if boolean(get(p, c"BatteryInstalled")) == Some(false) {
            return;
        }
        let Some(percent) = number(get(p, c"CurrentCapacity")) else { return };
        let data = get(p, c"BatteryData");
        let plugged = boolean(get(p, c"ExternalConnected")).unwrap_or(false);
        let charging = boolean(get(p, c"IsCharging")).unwrap_or(false);
        let state = match (charging, plugged, boolean(get(p, c"FullyCharged")).unwrap_or(false)) {
            (true, ..) => "charging",
            (_, true, true) => "full",
            (_, true, _) => "on power, not charging",
            _ => "on battery",
        };
        // 65535 means "still estimating".
        let minutes = number(get(p, if charging { c"AvgTimeToFull" } else { c"AvgTimeToEmpty" }))
            .filter(|&m| m > 0 && m < 65535)
            .map(|m| m as u32);
        let full = number(get(p, c"AppleRawMaxCapacity")).or_else(|| number(get(data, c"FullChargeCapacity")));
        let design = number(get(data, c"DesignCapacity")).or_else(|| number(get(p, c"DesignCapacity")));
        let telemetry = get(p, c"PowerTelemetryData");
        out = Some(Battery {
            percent: percent as f32,
            state: state.into(),
            minutes,
            capacity: full.zip(design).filter(|&(_, d)| d > 0).map(|(f, d)| (f as u64, d as u64, "mAh")),
            cycles: number(get(p, c"CycleCount")).map(|n| n as u32),
            voltage_mv: number(get(p, c"Voltage")).map(|n| n as u32),
            current_ma: number(get(p, c"Amperage")).map(|n| n as i32),
            temp_c: pack_temperature().or_else(|| number(get(p, c"Temperature"))).map(|t| t as f32 / 100.0),
            system_watts: number(get(telemetry, c"SystemLoad")).filter(|&w| w > 0).map(|mw| mw as f32 / 1000.0),
            adapter_watts: plugged
                .then(|| number(get(get(p, c"AdapterDetails"), c"Watts")))
                .flatten()
                .map(|w| w as u32),
            source: "AppleSmartBattery".into(),
        });
    });
    out
}

/// Hundredths of a degree, from the pack under the gauge (Apple Silicon).
fn pack_temperature() -> Option<i64> {
    let mut t = None;
    each_service(c"AppleSmartBatteryPack", |e| {
        t = t.or_else(|| number(get(property(e, c"BatteryData").0, c"Temperature")));
    });
    t
}

/// 'P' or 'E' per logical CPU, from the `cluster-type` of each node under
/// IODeviceTree:/cpus. Empty when the machine doesn't say.
pub fn core_kinds() -> Vec<char> {
    let cpus = unsafe { IORegistryEntryFromPath(0, c"IODeviceTree:/cpus".as_ptr()) };
    if cpus == 0 {
        return Vec::new();
    }
    let mut iter = 0;
    let mut kinds = Vec::new();
    if unsafe { IORegistryEntryGetChildIterator(cpus, c"IODeviceTree".as_ptr(), &mut iter) } == 0 {
        loop {
            let cpu = unsafe { IOIteratorNext(iter) };
            if cpu == 0 {
                break;
            }
            let kind = property(cpu, c"cluster-type");
            let id = number(property(cpu, c"logical-cpu-id").0);
            if kind.is(unsafe { CFDataGetTypeID() }) && unsafe { CFDataGetLength(kind.0) } > 0 {
                let c = unsafe { *CFDataGetBytePtr(kind.0) } as char;
                kinds.push((id.unwrap_or(kinds.len() as i64), c));
            }
            unsafe { IOObjectRelease(cpu) };
        }
        unsafe { IOObjectRelease(iter) };
    }
    unsafe { IOObjectRelease(cpus) };
    kinds.sort();
    kinds.into_iter().map(|(_, c)| c).collect()
}

/// CPU, GPU and SSD temperatures plus fan speeds.
pub fn thermal() -> Thermal {
    let (mut acc, mut die, mut gpu, mut ssd) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for (name, c) in hid_temperatures() {
        // Sensors that aren't wired up report 0 or nonsense.
        if !(1.0..150.0).contains(&c) {
            continue;
        }
        // M2 and later name CPU-cluster sensors "pACC/eACC MTR Temp Sensor";
        // M1 only has the PMU's die sensors ("PMU tdie").
        if name.contains("ACC MTR") {
            acc.push(c);
        } else if name.starts_with("PMU tdie") {
            die.push(c);
        } else if name.contains("GPU MTR") {
            gpu.push(c);
        } else if name.starts_with("NAND") {
            ssd.push(c);
        }
    }
    let max = |v: Vec<f32>| v.into_iter().reduce(f32::max);
    let cpu = if acc.is_empty() { die } else { acc };
    Thermal {
        cpu_c: max(cpu),
        gpu_c: max(gpu),
        ssd_c: max(ssd),
        fans: smc_fans(),
        source: "IOHIDEventSystem · AppleSMC".into(),
    }
}

/// (product name, °C) of every HID temperature sensor: usage page 0xff00
/// (Apple vendor), usage 5 (temperature).
fn hid_temperatures() -> Vec<(String, f32)> {
    const SINT32: isize = 3;
    const TEMPERATURE_EVENT: i64 = 15;
    let mut out = Vec::new();
    let client = Cf(unsafe { IOHIDEventSystemClientCreate(null()) });
    if client.0.is_null() {
        return out;
    }
    let (page, usage) = (0xff00i32, 5i32);
    let values = [
        Cf(unsafe { CFNumberCreate(null(), SINT32, (&raw const page).cast()) }),
        Cf(unsafe { CFNumberCreate(null(), SINT32, (&raw const usage).cast()) }),
    ];
    let keys = [Cf::string(c"PrimaryUsagePage"), Cf::string(c"PrimaryUsage")];
    let matching = Cf(unsafe {
        CFDictionaryCreate(
            null(),
            [keys[0].0, keys[1].0].as_ptr(),
            [values[0].0, values[1].0].as_ptr(),
            2,
            &raw const kCFTypeDictionaryKeyCallBacks,
            &raw const kCFTypeDictionaryValueCallBacks,
        )
    });
    unsafe { IOHIDEventSystemClientSetMatching(client.0, matching.0) };
    let services = Cf(unsafe { IOHIDEventSystemClientCopyServices(client.0) });
    if services.0.is_null() {
        return out;
    }
    let product = Cf::string(c"Product");
    for i in 0..unsafe { CFArrayGetCount(services.0) } {
        let service = unsafe { CFArrayGetValueAtIndex(services.0, i) };
        let name = Cf(unsafe { IOHIDServiceClientCopyProperty(service, product.0) });
        let event = Cf(unsafe { IOHIDServiceClientCopyEvent(service, TEMPERATURE_EVENT, 0, 0) });
        if let (Some(n), false) = (string(name.0), event.0.is_null()) {
            // The value field of an event is (type << 16).
            let c = unsafe { IOHIDEventGetFloatValue(event.0, (TEMPERATURE_EVENT as i32) << 16) };
            out.push((n, c as f32));
        }
    }
    out
}

/// The System Management Controller speaks in four-letter keys through one
/// struct-in, struct-out call. Offsets are those of the C `SMCKeyData_t`.
const SMC_SIZE: usize = 80;
const SMC_KEY_SIZE: usize = 28;
const SMC_KEY_TYPE: usize = 32;
const SMC_RESULT: usize = 40;
const SMC_COMMAND: usize = 42;
const SMC_BYTES: usize = 48;
const SMC_READ_BYTES: u8 = 5;
const SMC_KEY_INFO: u8 = 9;

struct Smc(u32);

impl Drop for Smc {
    fn drop(&mut self) {
        unsafe { IOServiceClose(self.0) };
    }
}

impl Smc {
    #[allow(deprecated)] // mach_task_self: libc points at the mach2 crate.
    fn open() -> Option<Smc> {
        let service = unsafe { IOServiceGetMatchingService(0, IOServiceMatching(c"AppleSMC".as_ptr())) };
        if service == 0 {
            return None;
        }
        let mut conn = 0;
        let rc = unsafe { IOServiceOpen(service, libc::mach_task_self(), 0, &mut conn) };
        unsafe { IOObjectRelease(service) };
        (rc == 0).then_some(Smc(conn))
    }

    fn call(&self, input: &[u8; SMC_SIZE]) -> Option<[u8; SMC_SIZE]> {
        let mut out = [0u8; SMC_SIZE];
        let mut size = SMC_SIZE;
        let rc = unsafe {
            IOConnectCallStructMethod(self.0, 2, input.as_ptr().cast(), SMC_SIZE, out.as_mut_ptr().cast(), &mut size)
        };
        (rc == 0 && out[SMC_RESULT] == 0).then_some(out)
    }

    /// (type code such as "flt ", raw bytes) of `key`.
    fn read(&self, key: &[u8; 4]) -> Option<([u8; 4], Vec<u8>)> {
        let mut input = [0u8; SMC_SIZE];
        input[..4].copy_from_slice(&u32::from_be_bytes(*key).to_ne_bytes());
        input[SMC_COMMAND] = SMC_KEY_INFO;
        let info = self.call(&input)?;
        let size = u32::from_ne_bytes(info[SMC_KEY_SIZE..SMC_KEY_SIZE + 4].try_into().ok()?);
        let kind = u32::from_ne_bytes(info[SMC_KEY_TYPE..SMC_KEY_TYPE + 4].try_into().ok()?).to_be_bytes();
        input[SMC_KEY_SIZE..SMC_KEY_SIZE + 4].copy_from_slice(&size.to_ne_bytes());
        input[SMC_COMMAND] = SMC_READ_BYTES;
        let out = self.call(&input)?;
        Some((kind, out[SMC_BYTES..SMC_BYTES + (size as usize).min(32)].to_vec()))
    }

    fn number(&self, key: &[u8; 4]) -> Option<f32> {
        let (kind, b) = self.read(key)?;
        match &kind {
            // Apple Silicon: a native float.
            b"flt " => Some(f32::from_ne_bytes(b.get(..4)?.try_into().ok()?)),
            // Intel: unsigned fixed point with 2 fraction bits.
            b"fpe2" => Some(u16::from_be_bytes(b.get(..2)?.try_into().ok()?) as f32 / 4.0),
            b"ui8 " => b.first().map(|&v| v as f32),
            _ => None,
        }
    }
}

/// FNum fans; F<n>Ac is the actual speed and F<n>Mx the maximum.
fn smc_fans() -> Vec<Fan> {
    let Some(smc) = Smc::open() else { return Vec::new() };
    let n = smc.number(b"FNum").unwrap_or(0.0) as u8;
    (0..n.min(10))
        .filter_map(|i| {
            let ac = smc.number(&[b'F', b'0' + i, b'A', b'c'])?;
            let mx = smc.number(&[b'F', b'0' + i, b'M', b'x']).unwrap_or(0.0);
            Some(Fan { rpm: ac, max: mx })
        })
        .collect()
}
