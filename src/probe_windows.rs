//! Windows hardware probing.
//!
//! No extra crates and no FFI: everything comes from Windows PowerShell 5.1
//! (present on every Windows 10/11), asked for JSON.
//!
//! * An inventory script (CIM/WMI classes, PnP devices and their properties,
//!   monitor EDIDs from the registry) runs on a background thread every 30 s.
//! * Live values (CPU load and clocks, memory, battery, disk and network
//!   traffic, brightness, ACPI thermal zones) come from one long-lived
//!   PowerShell process, so there is no PowerShell start-up cost per refresh.
//! * `nvidia-smi` (ships with the NVIDIA driver) for NVIDIA GPUs.
//!
//! `INSIDER_CAPTURE=/dir` records the JSON (`cmd/powershell-inventory`,
//! `cmd/powershell-live`); replay with `INSIDER_SYSROOT=/dir INSIDER_PLATFORM=windows`.

use crate::json::Json;
use crate::model::*;
use crate::platform::{Options, Refresh};
use crate::probe::{
    capture, chassis_name, clean, clean_cpu_name, edid_props, edid_title, join, nvidia_props,
    panel_vendor, parse_edid, query_nvidia, slot_suffix, sysroot, throughput, touchpad_vendor,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------- PowerShell plumbing

const PRELUDE: &str = r#"
$ErrorActionPreference = 'SilentlyContinue'
$ProgressPreference = 'SilentlyContinue'
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}
function Q($c, $ns = 'root/cimv2') { @(Get-CimInstance -ClassName $c -Namespace $ns -ErrorAction SilentlyContinue) }
function P($o, $p) { @($o | Select-Object -Property $p) }
function Unix($d) { if ($d) { [DateTimeOffset]::new($d).ToUnixTimeSeconds() } }
"#;

const INVENTORY: &str = r#"
$o = [ordered]@{}
$o.cs = P (Q Win32_ComputerSystem) Manufacturer,Model,SystemFamily,SystemSKUNumber,PCSystemType,TotalPhysicalMemory
$o.bb = P (Q Win32_BaseBoard) Manufacturer,Product,Version
$o.bios = P (Q Win32_BIOS) Manufacturer,SMBIOSBIOSVersion,@{n='ReleaseDate';e={if ($_.ReleaseDate) { $_.ReleaseDate.ToString('yyyy-MM-dd') }}},SystemBiosMajorVersion,SystemBiosMinorVersion,EmbeddedControllerMajorVersion,EmbeddedControllerMinorVersion
$o.enc = P (Q Win32_SystemEnclosure) ChassisTypes,Manufacturer
$o.os = P (Q Win32_OperatingSystem) Caption,Version,BuildNumber,CSName,@{n='Boot';e={Unix $_.LastBootUpTime}}
$o.cpu = P (Q Win32_Processor) Name,Manufacturer,NumberOfCores,NumberOfLogicalProcessors,MaxClockSpeed,L2CacheSize,L3CacheSize,SocketDesignation,Caption,VirtualizationFirmwareEnabled
$o.mem = P (Q Win32_PhysicalMemory) BankLabel,DeviceLocator,Capacity,Speed,ConfiguredClockSpeed,Manufacturer,PartNumber,SerialNumber,FormFactor,SMBIOSMemoryType,ConfiguredVoltage,TotalWidth,Attributes
$o.memarr = P (Q Win32_PhysicalMemoryArray) MemoryDevices,Use
$o.gpu = P (Q Win32_VideoController) Name,AdapterCompatibility,AdapterRAM,DriverVersion,PNPDeviceID,VideoProcessor,CurrentHorizontalResolution,CurrentVerticalResolution,CurrentRefreshRate
$o.vram = P (Get-ItemProperty 'HKLM:\SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}\0*') DriverDesc,MatchingDeviceId,@{n='Vram';e={$_.'HardwareInformation.qwMemorySize'}}
$o.disk = P (Q MSFT_PhysicalDisk root/Microsoft/Windows/Storage) DeviceId,FriendlyName,Model,SerialNumber,FirmwareVersion,MediaType,BusType,Size,HealthStatus,SpindleSpeed
$o.rel = @(Get-PhysicalDisk | ForEach-Object { $r = $_ | Get-StorageReliabilityCounter; if ($r) { [pscustomobject]@{DeviceId=$_.DeviceId; Temperature=$r.Temperature; Wear=$r.Wear; PowerOnHours=$r.PowerOnHours} } })
$o.net = P (Get-NetAdapter -Physical) Name,InterfaceDescription,MacAddress,LinkSpeed,Status,PhysicalMediaType,NdisPhysicalMedium,DriverVersionString,DriverProvider,PnPDeviceID,MtuSize
$o.bat = P (Q Win32_Battery) Name,DeviceID,Chemistry,DesignVoltage
$o.batstatic = P (Q BatteryStaticData root/wmi) DesignedCapacity,DeviceName,ManufactureName,Chemistry,InstanceName
$o.batfull = P (Q BatteryFullChargedCapacity root/wmi) FullChargedCapacity,InstanceName
$o.batcyc = P (Q BatteryCycleCount root/wmi) CycleCount,InstanceName
$o.mon = P (Q WmiMonitorID root/wmi) InstanceName,@{n='Make';e={-join [char[]]($_.ManufacturerName -ne 0)}},@{n='Model';e={-join [char[]]($_.UserFriendlyName -ne 0)}},@{n='Code';e={-join [char[]]($_.ProductCodeID -ne 0)}},YearOfManufacture
$o.monconn = P (Q WmiMonitorConnectionParams root/wmi) InstanceName,VideoOutputTechnology
$o.edid = @(foreach ($m in (Q WmiMonitorID root/wmi)) { $k = 'HKLM:\SYSTEM\CurrentControlSet\Enum\' + ($m.InstanceName -replace '_\d+$','') + '\Device Parameters'; $e = (Get-ItemProperty -Path $k -Name EDID).EDID; if ($e) { [pscustomobject]@{InstanceName=$m.InstanceName; Edid=(($e | ForEach-Object { $_.ToString('x2') }) -join '')} } })
$o.snd = P (Q Win32_SoundDevice) Name,Manufacturer,PNPDeviceID
$o.ep = P (Get-PnpDevice -Class AudioEndpoint -PresentOnly) FriendlyName,Status
$o.kbd = P (Q Win32_Keyboard) Name,Description,PNPDeviceID
$o.ptr = P (Q Win32_PointingDevice) Name,Description,Manufacturer,PNPDeviceID
$o.pnp = P (Get-PnpDevice -PresentOnly | Where-Object { $_.InstanceId -match '^(USB\\VID_|PCI\\)' }) FriendlyName,InstanceId,Class,Manufacturer,Status
$ids = @($o.pnp | ForEach-Object { $_.InstanceId })
if ($ids.Count) { $o.props = P (Get-PnpDeviceProperty -InstanceId $ids -KeyName DEVPKEY_Device_LocationInfo,DEVPKEY_Device_Capabilities,DEVPKEY_Device_BusReportedDeviceDesc,DEVPKEY_Device_Parent,DEVPKEY_PciDevice_CurrentLinkSpeed,DEVPKEY_PciDevice_CurrentLinkWidth,DEVPKEY_PciDevice_MaxLinkSpeed,DEVPKEY_PciDevice_MaxLinkWidth) InstanceId,KeyName,Data }
$o.fw = $env:firmware_type
$o.sb = try { Confirm-SecureBootUEFI } catch { $null }
$o.admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$o | ConvertTo-Json -Depth 5 -Compress
"#;

const LIVE: &str = r#"
function InsiderLive {
$o = [ordered]@{}
$o.cpu = P (Q Win32_PerfFormattedData_Counters_ProcessorInformation) Name,PercentProcessorTime,PercentProcessorUtility,PercentProcessorPerformance,ProcessorFrequency
$o.os = P (Q Win32_OperatingSystem) FreePhysicalMemory,TotalVisibleMemorySize,@{n='Boot';e={Unix $_.LastBootUpTime}}
$o.bat = P (Q Win32_Battery) EstimatedChargeRemaining,BatteryStatus,EstimatedRunTime
$o.bs = P (Q BatteryStatus root/wmi) InstanceName,RemainingCapacity,ChargeRate,DischargeRate,Voltage,PowerOnline,Charging,Discharging
$o.disk = P (Q Win32_PerfRawData_PerfDisk_PhysicalDisk) Name,DiskReadBytesPersec,DiskWriteBytesPersec
$o.net = P (Get-NetAdapterStatistics) Name,ReceivedBytes,SentBytes
$o.netst = P (Get-NetAdapter -Physical) Name,Status,LinkSpeed
$o.wlan = (netsh wlan show interfaces) -join "`n"
$o.bright = P (Q WmiMonitorBrightness root/wmi) InstanceName,CurrentBrightness
$o.tzp = P (Q Win32_PerfFormattedData_Counters_ThermalZoneInformation) Name,Temperature,HighPrecisionTemperature
$o.tz = P (Q MSAcpi_ThermalZoneTemperature root/wmi) InstanceName,CurrentTemperature
$o.gpu = P (Q Win32_PerfFormattedData_GPUPerformanceCounters_GPUEngine) Name,UtilizationPercentage
$o.caps = [Console]::CapsLock
$o.numlock = [Console]::NumberLock
$o | ConvertTo-Json -Depth 4 -Compress
}
"#;

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// PowerShell's -EncodedCommand wants base64 of UTF-16LE; it sidesteps all
/// command-line quoting.
fn encoded(script: &str) -> String {
    let b: Vec<u8> = script
        .encode_utf16()
        .flat_map(|u| u.to_le_bytes())
        .collect();
    b64(&b)
}

const PS_ARGS: [&str; 6] = [
    "-NoLogo",
    "-NoProfile",
    "-NonInteractive",
    "-ExecutionPolicy",
    "Bypass",
    "-EncodedCommand",
];

fn inventory() -> Json {
    let out = if !sysroot().is_empty() {
        crate::probe::replay("powershell-inventory", &[])
    } else {
        let script = format!("{PRELUDE}{INVENTORY}");
        let enc = encoded(&script);
        let mut args: Vec<&str> = PS_ARGS.to_vec();
        args.push(&enc);
        let o = run_quiet("powershell", &args);
        if let Some(o) = &o {
            capture("powershell-inventory", o);
        }
        o
    };
    out.and_then(|o| Json::parse(o.trim()))
        .unwrap_or(Json::Null)
}

/// Like `probe::run`, without recording under a (huge) argument-derived name.
fn run_quiet(cmd: &str, args: &[&str]) -> Option<String> {
    let mut c = Command::new(cmd);
    c.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    no_window(&mut c);
    let out = c.output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(windows)]
fn no_window(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn no_window(_: &mut Command) {}

/// A PowerShell kept running, fed one line per request.
struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<String>,
    seq: u64,
}

impl Session {
    fn start() -> Option<Session> {
        let mut c = Command::new("powershell");
        c.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
        no_window(&mut c);
        let mut child = c.spawn().ok()?;
        let mut stdin = child.stdin.take()?;
        let stdout = child.stdout.take()?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines() {
                let Ok(l) = l else { break };
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        // define helpers + InsiderLive in the session, from one line
        let setup = encoded(&format!("{PRELUDE}{LIVE}"));
        writeln!(
            stdin,
            ". ([ScriptBlock]::Create([Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{setup}'))))"
        )
        .ok()?;
        stdin.flush().ok()?;
        Some(Session {
            child,
            stdin,
            rx,
            seq: 0,
        })
    }

    fn ask(&mut self, cmd: &str, timeout: Duration) -> Option<String> {
        self.seq += 1;
        let (b, e) = (
            format!("<<<insider-b{}>>>", self.seq),
            format!("<<<insider-e{}>>>", self.seq),
        );
        writeln!(self.stdin, "'{b}'; {cmd}; '{e}'").ok()?;
        self.stdin.flush().ok()?;
        let until = Instant::now() + timeout;
        let mut out = String::new();
        let mut inside = false;
        loop {
            let left = until.checked_duration_since(Instant::now())?;
            let l = self.rx.recv_timeout(left).ok()?;
            if l.trim() == b {
                inside = true;
            } else if l.trim() == e {
                return Some(out);
            } else if inside {
                out.push_str(&l);
                out.push('\n');
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

// ---------------------------------------------------------------- helpers

fn s(j: &Json, k: &str) -> Option<String> {
    clean(j.get(k).str())
}

fn n(j: &Json, k: &str) -> Option<f64> {
    j.get(k).num()
}

/// Get-NetAdapter's LinkSpeed ("1 Gbps", "866.7 Mbps", "0 bps") in Mb/s.
pub(crate) fn link_mbps(s: &str) -> u32 {
    let n = crate::probe::leading_num(s).unwrap_or(0.0);
    let l = s.to_ascii_lowercase();
    let mbps = if l.contains("gbps") {
        n * 1000.0
    } else if l.contains("mbps") {
        n
    } else if l.contains("kbps") {
        n / 1000.0
    } else {
        n / 1e6
    };
    mbps.round() as u32
}

/// "PCI bus 1, device 0, function 0" -> "0000:01:00.0"
pub(crate) fn pci_address(loc: &str) -> Option<String> {
    let mut nums = loc
        .split(|c: char| !c.is_ascii_digit())
        .filter(|x| !x.is_empty())
        .map(|x| x.parse::<u32>().ok());
    if !loc.to_ascii_lowercase().starts_with("pci bus") {
        return None;
    }
    let (b, d, f) = (nums.next()??, nums.next()??, nums.next()??);
    Some(format!("0000:{b:02x}:{d:02x}.{f:x}"))
}

/// "Port_#0003.Hub_#0001" -> "1-3" (hub-port; not the same numbering as Linux)
pub(crate) fn usb_port(loc: &str) -> Option<String> {
    let port = loc.split("Port_#").nth(1)?.get(..4)?.parse::<u32>().ok()?;
    let hub = loc.split("Hub_#").nth(1)?.get(..4)?.parse::<u32>().ok()?;
    Some(format!("{hub}-{port}"))
}

/// "USB\VID_046D&PID_C52B\..." -> ("046d", "c52b")
fn vid_pid(id: &str) -> Option<(String, String)> {
    let u = id.to_ascii_uppercase();
    let v = u.split("VID_").nth(1)?.get(..4)?.to_ascii_lowercase();
    let p = u.split("PID_").nth(1)?.get(..4)?.to_ascii_lowercase();
    Some((v, p))
}

fn pci_ven_dev(id: &str) -> Option<(u32, u32)> {
    let u = id.to_ascii_uppercase();
    let v = u32::from_str_radix(u.split("VEN_").nth(1)?.get(..4)?, 16).ok()?;
    let d = u32::from_str_radix(u.split("DEV_").nth(1)?.get(..4)?, 16).ok()?;
    Some((v, d))
}

fn mem_type(t: u32) -> Option<&'static str> {
    Some(match t {
        20 => "DDR",
        21 => "DDR2",
        24 => "DDR3",
        26 => "DDR4",
        27 => "LPDDR",
        28 => "LPDDR2",
        29 => "LPDDR3",
        30 => "LPDDR4",
        34 => "DDR5",
        35 => "LPDDR5",
        _ => return None,
    })
}

fn pcie_gen(code: f64) -> String {
    match code as u32 {
        1 => "2.5 GT/s".into(),
        2 => "5.0 GT/s".into(),
        3 => "8.0 GT/s".into(),
        4 => "16.0 GT/s".into(),
        5 => "32.0 GT/s".into(),
        6 => "64.0 GT/s".into(),
        x => format!("gen code {x}"),
    }
}

/// `netsh wlan show interfaces`: per interface name, key -> value.
pub(crate) fn parse_netsh(s: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut cur: Option<HashMap<String, String>> = None;
    let flush = |c: Option<HashMap<String, String>>, out: &mut HashMap<_, _>| {
        if let Some(c) = c {
            if let Some(n) = c.get("Name").cloned() {
                out.insert(n, c);
            }
        }
    };
    for l in s.lines() {
        let Some((k, v)) = l.split_once(" : ").or_else(|| l.split_once(": ")) else {
            continue;
        };
        let (k, v) = (k.trim().to_string(), v.trim().to_string());
        if k == "Name" {
            flush(cur.take(), &mut out);
            cur = Some(HashMap::new());
        }
        if let Some(c) = cur.as_mut() {
            c.insert(k, v);
        }
    }
    flush(cur, &mut out);
    out
}

/// One USB device, merged from its PnP entries (composite parent + functions).
#[derive(Default, Debug)]
struct UsbDev {
    vid: String,
    pid: String,
    names: Vec<String>,
    bus_name: Option<String>,
    classes: Vec<String>,
    maker: Option<String>,
    loc: Option<String>,
    removable: Option<bool>,
    ids: Vec<String>,
}

// ---------------------------------------------------------------- the prober

pub struct Prober {
    opts: Options,
    inv: Refresh<Json>,
    session: Option<Session>,
    session_failures: u32,
    prev: HashMap<String, (f64, Instant)>,
    nvidia: HashMap<String, HashMap<String, String>>,
    nvidia_at: Option<Instant>,
}

impl Prober {
    pub fn new(opts: Options) -> Self {
        let first = inventory();
        Prober {
            opts,
            inv: Refresh::new(first, Duration::from_secs(30), inventory),
            session: None,
            session_failures: 0,
            prev: HashMap::new(),
            nvidia: HashMap::new(),
            nvidia_at: None,
        }
    }

    pub fn settle(&self) {}

    fn rate(&mut self, key: &str, v: f64) -> Option<f64> {
        let now = Instant::now();
        let r = self.prev.get(key).and_then(|(pv, pt)| {
            let dt = now.duration_since(*pt).as_secs_f64();
            (dt > 0.05 && v >= *pv).then(|| (v - pv) / dt)
        });
        self.prev.insert(key.to_string(), (v, now));
        r
    }

    fn live(&mut self) -> Json {
        if !sysroot().is_empty() {
            return crate::probe::replay("powershell-live", &[])
                .and_then(|o| Json::parse(o.trim()))
                .unwrap_or(Json::Null);
        }
        if self.session.is_none() && self.session_failures < 3 {
            self.session = Session::start();
            if self.session.is_none() {
                self.session_failures += 1;
            }
        }
        let Some(sess) = self.session.as_mut() else {
            return Json::Null;
        };
        match sess.ask("InsiderLive", Duration::from_secs(8)) {
            Some(o) => {
                capture("powershell-live", &o);
                Json::parse(o.trim()).unwrap_or(Json::Null)
            }
            None => {
                // hung or died: start over next time
                self.session = None;
                self.session_failures += 1;
                Json::Null
            }
        }
    }

    pub fn gather(&mut self) -> Snapshot {
        let inv = self.inv.get();
        let live = self.live();
        let admin = inv.get("admin").bool().unwrap_or(false);

        // PnP device properties: instance id -> key -> value
        let mut props: HashMap<String, HashMap<String, Json>> = HashMap::new();
        for p in inv.get("props").items() {
            if let (Some(id), Some(k)) = (p.get("InstanceId").str(), p.get("KeyName").str()) {
                props
                    .entry(id.to_ascii_uppercase())
                    .or_default()
                    .insert(k, p.get("Data").clone());
            }
        }
        let prop = |id: &str, k: &str| -> Option<Json> {
            props.get(&id.to_ascii_uppercase())?.get(k).cloned()
        };
        let pci_addr = |id: &str| {
            prop(id, "DEVPKEY_Device_LocationInfo")
                .and_then(|v| v.str())
                .and_then(|l| pci_address(&l))
        };

        let dmi = dmi(&inv);
        let tz = thermal_zones(&live);
        let mut parts = Vec::new();
        parts.push(board(&inv, &live, &dmi, &tz));
        let mut claimed_pci: HashSet<String> = HashSet::new();
        parts.push(self.cpu(&inv, &live, &tz, &pci_addr, &mut claimed_pci));
        parts.extend(self.gpus(&inv, &live, &pci_addr, &prop, &mut claimed_pci));
        parts.extend(dimms(&inv, &live));
        let dio = self.disk_io(&live);
        let mut disks = self.disks(&inv, &dio);
        nvme_controllers(&inv, &mut disks, &pci_addr, &prop, &mut claimed_pci);
        parts.extend(disks);
        parts.extend(self.nets(&inv, &live, &pci_addr, &mut claimed_pci));
        if let Some(c) = chipset(&inv, &pci_addr, &mut claimed_pci) {
            parts.push(c);
        }
        parts.push(bios(&inv, &dmi));
        parts.extend(batteries(&inv, &live));
        parts.extend(inputs(&inv, &live));
        parts.extend(usb(&inv, &prop, &dio));
        parts.extend(audio(&inv));
        parts.extend(displays(&inv, &live));
        parts.extend(other_pci(&inv, &pci_addr, &prop, &claimed_pci));

        let mut notes = Vec::new();
        if inv.is_null() {
            notes.push(
                "PowerShell inventory failed: is Windows PowerShell (powershell.exe) available?"
                    .into(),
            );
        }
        if live.is_null() && sysroot().is_empty() {
            notes.push("Live values unavailable: the PowerShell session did not answer".into());
        }
        if !admin {
            notes.push(
                "Not running as Administrator: ACPI thermal zones, disk temperatures and Secure Boot state need an elevated terminal"
                    .into(),
            );
        }
        notes.push(
            "Windows has no built-in interface for CPU core temperatures; the ACPI thermal zone is shown instead"
                .into(),
        );
        Snapshot { parts, dmi, notes }
    }

    // ------------------------------------------------------------ CPU

    fn cpu(
        &mut self,
        inv: &Json,
        live: &Json,
        tz: &[(String, f64)],
        pci_addr: &dyn Fn(&str) -> Option<String>,
        claimed: &mut HashSet<String>,
    ) -> Part {
        let c = inv
            .get("cpu")
            .items()
            .first()
            .cloned()
            .unwrap_or(Json::Null);
        let model = s(&c, "Name").unwrap_or_else(|| "CPU".into());
        let mut p = Part::new("cpu0", Kind::Cpu, clean_cpu_name(&model));
        p.tag = "CPU".into();
        p.bind = vec!["socket0".into(), "0".into()];
        p.opt("Socket", s(&c, "SocketDesignation"));
        p.prop("Model", &model);
        let cores = n(&c, "NumberOfCores").unwrap_or(0.0) as u32;
        let threads = n(&c, "NumberOfLogicalProcessors").unwrap_or(0.0) as u32;
        if threads > 0 {
            p.prop("Cores / threads", format!("{} / {threads}", cores.max(1)));
        }
        p.opt("Vendor", s(&c, "Manufacturer"));
        p.opt("Identifier", s(&c, "Caption"));
        let cache: Vec<String> = [("L2", "L2CacheSize"), ("L3", "L3CacheSize")]
            .iter()
            .filter_map(|(nm, k)| {
                let kb = n(&c, k)? as u64;
                (kb > 0).then(|| format!("{} {nm}", crate::probe_macos::fmt_cache(kb * 1024)))
            })
            .collect();
        if !cache.is_empty() {
            p.prop("Cache", cache.join(", "));
        }
        p.opt(
            "Base clock",
            n(&c, "MaxClockSpeed").map(|m| format!("{:.2} GHz", m / 1000.0)),
        );
        if c.get("VirtualizationFirmwareEnabled").bool() == Some(true) {
            p.prop("Virtualization", "enabled in firmware");
        }

        // live: per logical CPU ("0,3"), totals ("_Total")
        let mut total: Option<(f64, Option<f64>)> = None;
        let mut lcpus: BTreeMap<u32, (f64, Option<f64>)> = BTreeMap::new();
        for r in live.get("cpu").items() {
            let name = r.get("Name").str().unwrap_or_default();
            let load = n(r, "PercentProcessorUtility")
                .or(n(r, "PercentProcessorTime"))
                .unwrap_or(0.0)
                .min(100.0);
            let mhz = n(r, "ProcessorFrequency")
                .zip(n(r, "PercentProcessorPerformance"))
                .map(|(f, pct)| f * pct / 100.0)
                .filter(|m| *m > 0.0);
            if name == "_Total" {
                total = Some((load, mhz));
            } else if let Some((grp, idx)) = name.split_once(',') {
                // "group,index"; "0,_Total" rows are per-group totals
                if let (Ok(g), Ok(i)) = (grp.parse::<u32>(), idx.parse::<u32>()) {
                    lcpus.insert(g * 1000 + i, (load, mhz));
                }
            }
        }
        let freqs: Vec<f64> = lcpus.values().filter_map(|x| x.1).collect();
        if !freqs.is_empty() {
            let avg = freqs.iter().sum::<f64>() / freqs.len() as f64 / 1000.0;
            let max = freqs.iter().cloned().fold(0.0, f64::max) / 1000.0;
            p.metric(format!("{avg:.2} GHz"));
            p.prop(
                "Clock (avg / fastest core)",
                format!("{avg:.2} / {max:.2} GHz"),
            );
        } else if let Some(m) = total.and_then(|t| t.1) {
            p.metric(format!("{:.2} GHz", m / 1000.0));
        }
        if let Some((u, _)) = total {
            p.metric(format!("{u:.0}% load"));
            p.prop("Utilization", format!("{u:.1}%"));
        }
        // Group SMT siblings (numbered next to each other) when the split is
        // even; hybrid CPUs (P+E cores) are listed per logical CPU.
        let tpc = if cores > 0 && threads % cores == 0 {
            threads / cores
        } else {
            1
        };
        let mut by_core: BTreeMap<u32, Vec<(f64, Option<f64>)>> = BTreeMap::new();
        for (k, v) in lcpus.values().enumerate() {
            by_core.entry(k as u32 / tpc).or_default().push(*v);
        }
        p.cores = by_core
            .into_iter()
            .map(|(id, v)| Core {
                id,
                temp: None,
                ghz: v
                    .iter()
                    .filter_map(|x| x.1)
                    .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))))
                    .map(|m| m / 1000.0),
                load: Some(v.iter().map(|x| x.0).sum::<f64>() / v.len().max(1) as f64),
            })
            .collect();
        if let Some((nm, t)) = tz.iter().find(|(nm, _)| {
            let l = nm.to_ascii_lowercase();
            l.contains("cpu") || l.contains("tcpu")
        }) {
            p.temp = Some(*t);
            p.prop("Sensor", format!("ACPI thermal zone {nm}"));
        }

        // Intel integrated graphics live in the package, as on Linux
        for g in inv.get("gpu").items() {
            let id = g.get("PNPDeviceID").str().unwrap_or_default();
            let Some(addr) = pci_addr(&id) else { continue };
            if pci_ven_dev(&id).map_or(false, |(v, _)| v == 0x8086) && addr.starts_with("0000:00:")
            {
                claimed.insert(addr.clone());
                p.prop("Integrated GPU", s(g, "Name").unwrap_or_default());
                p.prop("iGPU PCI address", &addr);
                p.opt("iGPU driver", s(g, "DriverVersion"));
            }
        }
        p
    }

    // ------------------------------------------------------------ GPU

    fn gpus(
        &mut self,
        inv: &Json,
        live: &Json,
        pci_addr: &dyn Fn(&str) -> Option<String>,
        prop: &dyn Fn(&str, &str) -> Option<Json>,
        claimed: &mut HashSet<String>,
    ) -> Vec<Part> {
        let ctrls = inv.get("gpu").items();
        let has_nv = ctrls.iter().any(|g| {
            g.get("PNPDeviceID")
                .str()
                .unwrap_or_default()
                .to_uppercase()
                .contains("VEN_10DE")
        });
        if self.opts.nvidia_smi
            && has_nv
            && self.nvidia_at.map_or(true, |t| t.elapsed().as_secs() >= 4)
        {
            self.nvidia = query_nvidia();
            self.nvidia_at = Some(Instant::now());
        }
        // 3D engine utilization, summed per adapter LUID
        let mut luid: BTreeMap<String, f64> = BTreeMap::new();
        for e in live.get("gpu").items() {
            let name = e.get("Name").str().unwrap_or_default();
            if !name.ends_with("engtype_3D") {
                continue;
            }
            if let Some(l) = name
                .split("luid_")
                .nth(1)
                .and_then(|r| r.split("_phys").next())
            {
                *luid.entry(l.to_string()).or_default() +=
                    n(e, "UtilizationPercentage").unwrap_or(0.0);
            }
        }
        let vram = inv.get("vram").items();
        let mut out = Vec::new();
        for (i, g) in ctrls.iter().enumerate() {
            let id = g.get("PNPDeviceID").str().unwrap_or_default();
            let addr = pci_addr(&id);
            if addr.as_ref().map_or(false, |a| claimed.contains(a)) {
                continue;
            }
            let name = s(g, "Name").unwrap_or_else(|| "Display adapter".into());
            let mut p = Part::new(
                format!("gpu:{}", addr.clone().unwrap_or_else(|| i.to_string())),
                Kind::Gpu,
                name.clone(),
            );
            p.tag = "GPU".into();
            if let Some(a) = &addr {
                claimed.insert(a.clone());
                p.bind = vec![a.clone()];
                p.prop("PCI address", a);
            }
            if let Some((v, d)) = pci_ven_dev(&id) {
                p.prop("PCI ID", format!("{v:04x}:{d:04x}"));
            }
            p.opt("Vendor", s(g, "AdapterCompatibility"));
            p.opt("Driver version", s(g, "DriverVersion"));
            p.opt("Video processor", s(g, "VideoProcessor"));
            let mem = vram
                .iter()
                .find(|v| s(v, "DriverDesc").as_deref() == Some(name.as_str()))
                .and_then(|v| n(v, "Vram"))
                .or(n(g, "AdapterRAM"))
                .filter(|b| *b > 0.0);
            p.opt("VRAM", mem.map(|b| fmt_bin(b as u64)));
            pcie_link(&mut p, &id, prop);
            if let (Some(w), Some(h)) = (
                n(g, "CurrentHorizontalResolution"),
                n(g, "CurrentVerticalResolution"),
            ) {
                p.prop(
                    "Current mode",
                    format!(
                        "{w}x{h}{}",
                        n(g, "CurrentRefreshRate")
                            .map(|r| format!(" @ {r} Hz"))
                            .unwrap_or_default()
                    ),
                );
            }
            let key = addr.as_deref().map(slot_suffix);
            if let Some(nv) = key.and_then(|k| self.nvidia.get(&k)) {
                nvidia_props(&mut p, nv);
            } else if ctrls.len() == 1 && !luid.is_empty() {
                // with a single adapter the busiest LUID is it
                let u = luid.values().cloned().fold(0.0, f64::max).min(100.0);
                p.metric(format!("{u:.0}%"));
                p.prop("3D utilization", format!("{u:.0}%"));
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ storage

    /// Read/write bytes per second per physical disk number ("0", "1"...),
    /// from the raw (cumulative) disk performance counters.
    fn disk_io(&mut self, live: &Json) -> HashMap<String, (f64, f64)> {
        let mut out = HashMap::new();
        for c in live.get("disk").items() {
            // "0 C:", "1", "_Total"
            let Some(dev) = c
                .get("Name")
                .str()
                .and_then(|nm| nm.split_whitespace().next().map(String::from))
                .filter(|d| d.chars().all(|ch| ch.is_ascii_digit()))
            else {
                continue;
            };
            if let (Some(r), Some(w)) = (n(c, "DiskReadBytesPersec"), n(c, "DiskWriteBytesPersec"))
            {
                if let (Some(r), Some(w)) = (
                    self.rate(&format!("{dev}:r"), r),
                    self.rate(&format!("{dev}:w"), w),
                ) {
                    out.insert(dev, (r, w));
                }
            }
        }
        out
    }

    fn disks(&mut self, inv: &Json, dio: &HashMap<String, (f64, f64)>) -> Vec<Part> {
        let mut out = Vec::new();
        for d in inv.get("disk").items() {
            let bus = n(d, "BusType").unwrap_or(0.0) as u32;
            if bus == 7 {
                continue; // USB: shown with its USB device
            }
            let dev = d.get("DeviceId").str().unwrap_or_default();
            let model = s(d, "FriendlyName")
                .or(s(d, "Model"))
                .unwrap_or_else(|| "Disk".into());
            let mut p = Part::new(format!("disk:{dev}"), Kind::Ssd, model);
            let size = n(d, "Size").map(|v| v as u64);
            let media = n(d, "MediaType").unwrap_or(0.0) as u32;
            let label = match (bus, media) {
                (17, _) => "NVMe",
                (_, 3) => "HDD",
                _ => "SATA",
            };
            p.tag = match size {
                Some(s) => format!("{label} {}", fmt_dec(s)),
                None => label.into(),
            };
            p.bind = vec![
                if bus == 17 { "nvme" } else { "sata" }.into(),
                format!("PhysicalDrive{dev}"),
            ];
            p.prop("Device", format!("\\\\.\\PhysicalDrive{dev}"));
            p.prop(
                "Bus",
                match bus {
                    17 => "NVMe",
                    11 => "SATA",
                    3 => "ATA",
                    8 => "RAID",
                    10 => "SAS",
                    _ => "other",
                },
            );
            if let Some(s) = size {
                p.metric(fmt_dec(s));
                p.prop("Capacity", fmt_dec(s));
            }
            p.prop(
                "Media",
                match media {
                    3 => "rotational (HDD)",
                    4 => "solid state",
                    5 => "storage-class memory",
                    _ => "unspecified",
                },
            );
            p.opt("Firmware", s(d, "FirmwareVersion"));
            p.opt("Serial", s(d, "SerialNumber"));
            p.opt(
                "Health",
                d.get("HealthStatus").str().map(|h| match h.as_str() {
                    "0" => "healthy".to_string(),
                    "1" => "warning".to_string(),
                    "2" => "unhealthy".to_string(),
                    x => x.to_string(),
                }),
            );
            if let Some(r) = inv
                .get("rel")
                .items()
                .iter()
                .find(|r| r.get("DeviceId").str().as_deref() == Some(dev.as_str()))
            {
                p.temp = n(r, "Temperature").filter(|t| *t > 0.0);
                p.opt("Wear", n(r, "Wear").map(|w| format!("{w:.0}%")));
                p.opt("Power-on hours", n(r, "PowerOnHours").map(|h| h as u64));
            }
            if let Some((r, w)) = dio.get(&dev) {
                throughput(&mut p, *r, *w);
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ network

    fn nets(
        &mut self,
        inv: &Json,
        live: &Json,
        pci_addr: &dyn Fn(&str) -> Option<String>,
        claimed: &mut HashSet<String>,
    ) -> Vec<Part> {
        let wlan = parse_netsh(&live.get("wlan").str().unwrap_or_default());
        let mut out = Vec::new();
        for a in inv.get("net").items() {
            let pnp = a.get("PnPDeviceID").str().unwrap_or_default();
            if pnp.to_uppercase().starts_with("USB\\") {
                continue; // shown with its USB device
            }
            let medium = n(a, "NdisPhysicalMedium").unwrap_or(0.0) as u32;
            let pmt = a.get("PhysicalMediaType").str().unwrap_or_default();
            let wifi = medium == 9 || pmt.contains("802.11");
            if !wifi && medium != 14 && !pmt.contains("802.3") {
                continue; // Bluetooth PAN and the like
            }
            let name = s(a, "Name").unwrap_or_default();
            let mut p = Part::new(
                format!("net:{name}"),
                if wifi { Kind::Wifi } else { Kind::Ethernet },
                s(a, "InterfaceDescription").unwrap_or_else(|| name.clone()),
            );
            p.tag = if wifi { "Wi-Fi" } else { "Ethernet" }.into();
            p.bind = vec![name.clone()];
            if let Some(addr) = pci_addr(&pnp) {
                claimed.insert(addr.clone());
                p.bind.push(addr.clone());
                p.prop("PCI address", addr);
            }
            if let Some((v, d)) = pci_ven_dev(&pnp) {
                p.prop("PCI ID", format!("{v:04x}:{d:04x}"));
            }
            p.prop("Interface", &name);
            p.opt(
                "MAC",
                s(a, "MacAddress").map(|m| m.replace('-', ":").to_lowercase()),
            );
            p.opt("MTU", s(a, "MtuSize"));
            p.opt(
                "Driver",
                join(&[s(a, "DriverProvider"), s(a, "DriverVersionString")]),
            );
            let st = live
                .get("netst")
                .items()
                .iter()
                .find(|x| s(x, "Name").as_deref() == Some(name.as_str()))
                .cloned()
                .unwrap_or_else(|| a.clone());
            p.link = Some((
                s(&st, "Status").as_deref() == Some("Up"),
                s(&st, "LinkSpeed").map_or(0, |l| link_mbps(&l)),
            ));
            if let Some(state) = s(&st, "Status") {
                p.metric(state.to_lowercase());
                p.prop("State", state);
            }
            if let Some(ls) = s(&st, "LinkSpeed") {
                if !wifi {
                    p.metric(ls.clone());
                }
                p.prop("Link speed", ls);
            }
            if let Some(w) = wlan.get(&name) {
                if let Some(sig) = w.get("Signal") {
                    p.metric(sig.clone());
                    p.prop("Signal", sig);
                }
                for (k, label) in [
                    ("Radio type", "PHY mode"),
                    ("Channel", "Channel"),
                    ("Band", "Band"),
                    ("Receive rate (Mbps)", "Rx rate (Mb/s)"),
                    ("Transmit rate (Mbps)", "Tx rate (Mb/s)"),
                    ("Authentication", "Security"),
                ] {
                    p.opt(label, w.get(k).cloned());
                }
            }
            if let Some(x) = live
                .get("net")
                .items()
                .iter()
                .find(|x| s(x, "Name").as_deref() == Some(name.as_str()))
            {
                if let (Some(rx), Some(tx)) = (n(x, "ReceivedBytes"), n(x, "SentBytes")) {
                    if let (Some(r), Some(t)) = (
                        self.rate(&format!("{name}:rx"), rx),
                        self.rate(&format!("{name}:tx"), tx),
                    ) {
                        p.io = Some((r, t));
                        p.metric(format!("↓{} ↑{}", fmt_rate(r), fmt_rate(t)));
                        p.prop(
                            "Traffic",
                            format!("down {}  up {}", fmt_rate(r), fmt_rate(t)),
                        );
                    }
                }
            }
            out.push(p);
        }
        out
    }
}

// ---------------------------------------------------------------- free functions

/// NVMe drives don't say which controller they sit on; with one drive per
/// controller the pairing is unambiguous (the usual laptop case).
fn nvme_controllers(
    inv: &Json,
    disks: &mut [Part],
    pci_addr: &dyn Fn(&str) -> Option<String>,
    prop: &dyn Fn(&str, &str) -> Option<Json>,
    claimed: &mut HashSet<String>,
) {
    let ctrls: Vec<String> = inv
        .get("pnp")
        .items()
        .iter()
        .filter(|d| {
            s(d, "FriendlyName").map_or(false, |n| n.contains("NVM Express") || n.contains("NVMe"))
        })
        .filter_map(|d| d.get("InstanceId").str())
        .collect();
    let nvme: Vec<&mut Part> = disks
        .iter_mut()
        .filter(|p| p.bind.first().map_or(false, |b| b == "nvme"))
        .collect();
    if ctrls.len() != 1 || nvme.len() != 1 {
        return;
    }
    let (id, p) = (&ctrls[0], nvme.into_iter().next().unwrap());
    if let Some(a) = pci_addr(id) {
        claimed.insert(a.clone());
        p.prop("PCI address", &a);
        p.bind.push(a);
    }
    pcie_link(p, id, prop);
}

fn pcie_link(p: &mut Part, id: &str, prop: &dyn Fn(&str, &str) -> Option<Json>) {
    let num = |k: &str| prop(id, k).and_then(|v| v.num()).filter(|v| *v > 0.0);
    if let (Some(s), Some(w)) = (
        num("DEVPKEY_PciDevice_CurrentLinkSpeed"),
        num("DEVPKEY_PciDevice_CurrentLinkWidth"),
    ) {
        let max = match (
            num("DEVPKEY_PciDevice_MaxLinkSpeed"),
            num("DEVPKEY_PciDevice_MaxLinkWidth"),
        ) {
            (Some(a), Some(b)) => format!(" (max {} x{b})", pcie_gen(a)),
            _ => String::new(),
        };
        p.prop("PCIe link", format!("{} x{w}{max}", pcie_gen(s)));
    }
}

/// Same keys as Linux's /sys/class/dmi/id, so one layout matches a machine
/// under either OS.
fn dmi(inv: &Json) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let first = |k: &str| inv.get(k).items().first().cloned().unwrap_or(Json::Null);
    let (cs, bb, bios, enc) = (first("cs"), first("bb"), first("bios"), first("enc"));
    for (key, j, f) in [
        ("sys_vendor", &cs, "Manufacturer"),
        ("product_name", &cs, "Model"),
        ("product_family", &cs, "SystemFamily"),
        ("product_sku", &cs, "SystemSKUNumber"),
        ("board_vendor", &bb, "Manufacturer"),
        ("board_name", &bb, "Product"),
        ("board_version", &bb, "Version"),
        ("bios_vendor", &bios, "Manufacturer"),
        ("bios_version", &bios, "SMBIOSBIOSVersion"),
        ("bios_date", &bios, "ReleaseDate"),
        ("chassis_vendor", &enc, "Manufacturer"),
    ] {
        if let Some(v) = s(j, f) {
            m.insert(key.to_string(), v);
        }
    }
    if let (Some(a), Some(b)) = (
        n(&bios, "SystemBiosMajorVersion"),
        n(&bios, "SystemBiosMinorVersion"),
    ) {
        if a < 255.0 {
            m.insert("bios_release".into(), format!("{a}.{b}"));
        }
    }
    if let (Some(a), Some(b)) = (
        n(&bios, "EmbeddedControllerMajorVersion"),
        n(&bios, "EmbeddedControllerMinorVersion"),
    ) {
        if a < 255.0 {
            m.insert("ec_firmware_release".into(), format!("{a}.{b}"));
        }
    }
    if let Some(c) = enc
        .get("ChassisTypes")
        .items()
        .first()
        .and_then(|c| c.num())
    {
        m.insert("chassis_type".into(), (c as u32).to_string());
    }
    m
}

/// (name, °C) from the ACPI thermal zones.
fn thermal_zones(live: &Json) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = Vec::new();
    for z in live.get("tzp").items() {
        let name = s(z, "Name").unwrap_or_default();
        let k = n(z, "HighPrecisionTemperature")
            .map(|t| t / 10.0)
            .or(n(z, "Temperature"));
        if let Some(k) = k.filter(|k| *k > 200.0) {
            v.push((name.trim_start_matches("\\_TZ.").to_string(), k - 273.15));
        }
    }
    if v.is_empty() {
        for z in live.get("tz").items() {
            if let Some(t) = n(z, "CurrentTemperature").filter(|t| *t > 2000.0) {
                let name = s(z, "InstanceName").unwrap_or_default();
                let short = name
                    .rsplit('.')
                    .next()
                    .unwrap_or(&name)
                    .trim_end_matches("_0")
                    .to_string();
                v.push((short, t / 10.0 - 273.15));
            }
        }
    }
    v.retain(|(_, t)| *t > -40.0 && *t < 150.0);
    v
}

fn board(inv: &Json, live: &Json, dmi: &HashMap<String, String>, tz: &[(String, f64)]) -> Part {
    let g = |k: &str| dmi.get(k).cloned();
    let title = join(&[g("sys_vendor"), g("product_name")])
        .or_else(|| join(&[g("board_vendor"), g("board_name")]))
        .unwrap_or_else(|| "Mainboard".into());
    let mut p = Part::new("board", Kind::Board, title);
    p.tag = "Mainboard".into();
    p.opt("System vendor", g("sys_vendor"));
    p.opt("Product", g("product_name"));
    p.opt("Family", g("product_family"));
    p.opt("SKU", g("product_sku"));
    p.opt("Board vendor", g("board_vendor"));
    p.opt("Board name", g("board_name"));
    p.opt("Board version", g("board_version"));
    if let Some(c) = g("chassis_type").and_then(|s| s.parse::<u32>().ok()) {
        p.prop("Chassis", format!("{} ({c})", chassis_name(c)));
    }
    let os = inv.get("os").items().first().cloned().unwrap_or(Json::Null);
    p.opt(
        "Windows",
        s(&os, "Caption").map(|c| {
            format!(
                "{c}{}",
                s(&os, "BuildNumber")
                    .map(|b| format!(" (build {b})"))
                    .unwrap_or_default()
            )
        }),
    );
    p.opt("Hostname", s(&os, "CSName"));
    let lo = live
        .get("os")
        .items()
        .first()
        .cloned()
        .unwrap_or(Json::Null);
    let boot = n(&lo, "Boot").or(n(&os, "Boot"));
    if let (Some(b), Ok(now)) = (boot, SystemTime::now().duration_since(UNIX_EPOCH)) {
        p.prop(
            "Uptime",
            fmt_hours((now.as_secs_f64() - b).max(0.0) / 3600.0),
        );
    }
    if let (Some(t), Some(f)) = (
        n(&lo, "TotalVisibleMemorySize"),
        n(&lo, "FreePhysicalMemory"),
    ) {
        let (t, f) = (t as u64 * 1024, f as u64 * 1024);
        p.prop(
            "Memory in use",
            format!("{} of {}", fmt_bin(t - f.min(t)), fmt_bin(t)),
        );
    }
    for (t, v) in tz {
        p.prop(&format!("Thermal zone {t}"), fmt_temp(*v));
    }
    p.temp = tz.first().map(|x| x.1);
    p
}

fn dimms(inv: &Json, live: &Json) -> Vec<Part> {
    let lo = live
        .get("os")
        .items()
        .first()
        .cloned()
        .unwrap_or(Json::Null);
    let mem_use = n(&lo, "TotalVisibleMemorySize")
        .zip(n(&lo, "FreePhysicalMemory"))
        .map(|(t, f)| ((t - f).max(0.0) as u64 * 1024, t as u64 * 1024));
    let mut out = Vec::new();
    let mods = inv.get("mem").items();
    for (i, m) in mods.iter().enumerate() {
        let loc = s(m, "DeviceLocator");
        let bank = s(m, "BankLabel");
        let size = n(m, "Capacity").map(|c| c as u64).filter(|c| *c > 0);
        let mut p = Part::new(format!("dimm{i}"), Kind::Dimm, "Memory module");
        p.bind = [
            loc.clone(),
            bank.clone(),
            match (&bank, &loc) {
                (Some(b), Some(l)) => Some(format!("{b}/{l}")),
                _ => None,
            },
        ]
        .into_iter()
        .flatten()
        .collect();
        p.opt("Locator", loc.clone());
        p.opt("Bank", bank);
        let ty = n(m, "SMBIOSMemoryType").and_then(|t| mem_type(t as u32));
        p.form = match n(m, "FormFactor").unwrap_or(0.0) as u32 {
            8 => Some("DIMM".into()),
            12 => Some("SODIMM".into()),
            _ => None,
        };
        let maker = s(m, "Manufacturer");
        let part_no = s(m, "PartNumber");
        p.title = join(&[maker.clone(), part_no.clone()])
            .or_else(|| loc.clone())
            .unwrap_or_else(|| "Memory module".into());
        let short = size
            .map(|s| format!("{}G", (s as f64 / (1u64 << 30) as f64).round()))
            .unwrap_or_default();
        p.tag = format!("{short} {}", ty.unwrap_or("")).trim().to_string();
        p.metric(p.tag.clone());
        if let Some((used, total)) = mem_use {
            let pct = 100.0 * used as f64 / total.max(1) as f64;
            p.metric(format!("{pct:.0}% used"));
            p.prop(
                "System RAM in use",
                format!("{} of {} ({pct:.0}%)", fmt_bin(used), fmt_bin(total)),
            );
        }
        p.opt("Size", size.map(fmt_bin));
        p.opt("Type", ty);
        p.opt("Form factor", p.form.clone());
        p.opt(
            "Rated speed",
            n(m, "Speed")
                .filter(|v| *v > 0.0)
                .map(|v| format!("{v} MT/s")),
        );
        p.opt(
            "Configured speed",
            n(m, "ConfiguredClockSpeed")
                .filter(|v| *v > 0.0)
                .map(|v| format!("{v} MT/s")),
        );
        p.opt("Manufacturer", maker);
        p.opt("Part number", part_no);
        p.opt("Serial", s(m, "SerialNumber"));
        p.opt(
            "Rank",
            n(m, "Attributes").filter(|r| *r > 0.0).map(|r| r as u32),
        );
        p.opt(
            "Voltage",
            n(m, "ConfiguredVoltage")
                .filter(|v| *v > 0.0)
                .map(|v| format!("{:.2} V", v / 1000.0)),
        );
        p.opt("Width", n(m, "TotalWidth").map(|w| format!("{w} bit")));
        p.prop("Source", "SMBIOS (via WMI)");
        out.push(p);
    }
    // slots SMBIOS knows about but that hold no module
    let slots: u32 = inv
        .get("memarr")
        .items()
        .iter()
        .filter(|a| n(a, "Use").map_or(true, |u| u == 3.0)) // 3 = system memory
        .filter_map(|a| n(a, "MemoryDevices"))
        .sum::<f64>() as u32;
    for i in out.len() as u32..slots.min(16) {
        let mut p = Part::new(format!("dimm{i}"), Kind::Dimm, "Empty memory slot");
        p.present = false;
        p.tag = "empty".into();
        p.form = out.first().and_then(|d| d.form.clone());
        out.push(p);
    }
    out
}

fn chipset(
    inv: &Json,
    pci_addr: &dyn Fn(&str) -> Option<String>,
    claimed: &mut HashSet<String>,
) -> Option<Part> {
    let pnp = inv.get("pnp").items();
    let bus0: Vec<(&Json, String)> = pnp
        .iter()
        .filter_map(|d| {
            let id = d.get("InstanceId").str()?;
            let a = pci_addr(&id)?;
            a.starts_with("0000:00:").then_some((d, a))
        })
        .collect();
    let isa = bus0.iter().find(|(d, _)| {
        let n = d.get("FriendlyName").str().unwrap_or_default();
        n.contains("LPC") || n.contains("eSPI") || n.contains("ISA")
    });
    let (title, addr) = match isa {
        Some((d, a)) => {
            let mut t = s(d, "FriendlyName").unwrap_or_default();
            for junk in [
                " LPC/eSPI Controller",
                " LPC Controller",
                " eSPI Controller",
                " ISA bridge",
            ] {
                t = t.replace(junk, "");
            }
            (t, Some(a.clone()))
        }
        None if !bus0.is_empty() => ("Chipset".to_string(), None),
        None => return None,
    };
    let mut p = Part::new("chipset", Kind::Chipset, title);
    p.tag = "PCH".into();
    if let Some(a) = addr {
        claimed.insert(a.clone());
        p.prop("PCI address", a);
    }
    for (d, a) in &bus0 {
        if claimed.contains(a) {
            continue;
        }
        let class = s(d, "Class").unwrap_or_default();
        if class == "System"
            && s(d, "FriendlyName").map_or(false, |n| n.contains("PCI Express Root Port"))
        {
            continue;
        }
        p.prop(
            &format!("{} {class}", a.get(5..).unwrap_or(a)),
            s(d, "FriendlyName").unwrap_or_default(),
        );
    }
    Some(p)
}

fn bios(inv: &Json, dmi: &HashMap<String, String>) -> Part {
    let g = |k: &str| dmi.get(k).cloned();
    let fw = inv.get("fw").str().unwrap_or_default();
    let uefi = fw.eq_ignore_ascii_case("UEFI");
    let kind = if uefi { "UEFI BIOS" } else { "BIOS" };
    let mut p = Part::new(
        "bios",
        Kind::Bios,
        match g("bios_version") {
            Some(v) => format!("{kind} {v}"),
            None => kind.to_string(),
        },
    );
    p.tag = "BIOS".into();
    if let Some(v) = g("bios_version") {
        p.metric(v);
    }
    if let Some(d) = g("bios_date") {
        p.metric(d);
    }
    p.opt("Vendor", g("bios_vendor"));
    p.opt("Version", g("bios_version"));
    p.opt("Release date", g("bios_date"));
    p.opt("Revision", g("bios_release"));
    p.opt("EC firmware", g("ec_firmware_release"));
    if !fw.is_empty() {
        p.prop(
            "Firmware type",
            if uefi { "UEFI" } else { "Legacy BIOS / CSM" },
        );
    }
    if let Some(sb) = inv.get("sb").bool() {
        p.prop("Secure Boot", if sb { "enabled" } else { "disabled" });
    }
    p
}

fn batteries(inv: &Json, live: &Json) -> Vec<Part> {
    let mut out = Vec::new();
    let wb = inv.get("bat").items();
    if wb.is_empty() && inv.get("batstatic").items().is_empty() {
        return out;
    }
    let lb = live
        .get("bat")
        .items()
        .first()
        .cloned()
        .unwrap_or(Json::Null);
    let statics = inv.get("batstatic").items();
    let n_bat = statics.len().max(wb.len()).max(1);
    for i in 0..n_bat {
        let st = statics.get(i).cloned().unwrap_or(Json::Null);
        let inst = s(&st, "InstanceName").unwrap_or_default();
        let by_inst = |list: &[Json]| -> Json {
            list.iter()
                .find(|x| s(x, "InstanceName").as_deref() == Some(inst.as_str()))
                .or(list.get(i))
                .cloned()
                .unwrap_or(Json::Null)
        };
        let full_j = by_inst(inv.get("batfull").items());
        let cyc_j = by_inst(inv.get("batcyc").items());
        let bs = by_inst(live.get("bs").items());
        let w = wb.get(i).cloned().unwrap_or(Json::Null);
        let mut p = Part::new(
            format!("bat:BAT{i}"),
            Kind::Battery,
            join(&[s(&st, "ManufactureName"), s(&st, "DeviceName")])
                .or(s(&w, "Name"))
                .unwrap_or_else(|| format!("BAT{i}")),
        );
        p.tag = "Battery".into();
        p.bind = vec![format!("BAT{i}")];
        let now = n(&bs, "RemainingCapacity"); // mWh
        let full = n(&full_j, "FullChargedCapacity");
        let design = n(&st, "DesignedCapacity");
        let pct = n(&lb, "EstimatedChargeRemaining")
            .map(|c| c / 100.0)
            .or_else(|| now.zip(full).filter(|(_, f)| *f > 0.0).map(|(a, f)| a / f));
        let charging = bs.get("Charging").bool() == Some(true);
        let discharging = bs.get("Discharging").bool() == Some(true)
            || (bs.is_null() && n(&lb, "BatteryStatus") == Some(1.0));
        let online = bs.get("PowerOnline").bool();
        let status = if charging {
            "Charging"
        } else if discharging {
            "Discharging"
        } else if pct.map_or(false, |p| p > 0.99) {
            "Full"
        } else {
            "Not charging"
        };
        let v = n(&bs, "Voltage").map(|mv| mv / 1000.0);
        let pw = if discharging {
            n(&bs, "DischargeRate")
        } else {
            n(&bs, "ChargeRate")
        }
        .map(|mw| mw.abs() / 1000.0);
        let cycles = n(&cyc_j, "CycleCount")
            .filter(|c| *c > 0.0)
            .map(|c| c as i64);
        p.level = pct.map(|f| f.clamp(0.0, 1.0));
        if let Some(f) = pct {
            p.metric(format!("{:.0}%", f * 100.0));
            p.prop("Charge", format!("{:.0}%", f * 100.0));
        }
        p.metric(status);
        if let Some(v) = v {
            p.metric(format!("{v:.2} V"));
        }
        if let Some(wt) = pw.filter(|w| *w > 0.05) {
            let sign = if discharging {
                "−"
            } else if charging {
                "+"
            } else {
                ""
            };
            p.metric(format!("{sign}{wt:.1} W"));
        }
        if let Some(c) = cycles {
            p.metric(format!("{c} cycles"));
        }
        p.prop("Status", status);
        p.opt("Voltage", v.map(|v| format!("{v:.3} V")));
        p.opt(
            if discharging {
                "Power draw"
            } else {
                "Charge rate"
            },
            pw.map(|w| format!("{w:.2} W")),
        );
        let wh = |x: Option<f64>| x.map(|x| format!("{:.2} Wh", x / 1000.0));
        p.opt("Now", wh(now));
        p.opt("Full", wh(full));
        p.opt("Design", wh(design));
        if let (Some(f), Some(d)) = (full, design) {
            if d > 0.0 {
                p.prop("Health (full / design)", format!("{:.0}%", 100.0 * f / d));
            }
        }
        p.opt("Cycle count", cycles);
        if let (Some(nw), Some(f), Some(r)) =
            (now, full, pw.map(|w| w * 1000.0).filter(|r| *r > 0.0))
        {
            if discharging {
                p.prop("Time to empty", fmt_hours(nw / r));
            } else if charging {
                p.prop("Time to full", fmt_hours((f - nw).max(0.0) / r));
            }
        }
        p.opt(
            "Chemistry",
            n(&w, "Chemistry").and_then(|c| match c as u32 {
                3 => Some("Lead acid"),
                4 => Some("Nickel cadmium"),
                5 => Some("Nickel metal hydride"),
                6 => Some("Lithium-ion"),
                8 => Some("Lithium polymer"),
                _ => None,
            }),
        );
        p.opt("Manufacturer", s(&st, "ManufactureName"));
        p.opt("Model", s(&st, "DeviceName"));
        if let Some(a) = online {
            p.prop("AC adapter", if a { "connected" } else { "unplugged" });
        }
        out.push(p);
    }
    out
}

fn inputs(inv: &Json, live: &Json) -> Vec<Part> {
    let mut out = Vec::new();
    // internal: PS/2 (ACPI\...) or HID over I2C/SPI (no USB VID, no Bluetooth GUID)
    let internal = |id: &str| {
        let u = id.to_uppercase();
        !u.contains("VID_") && !u.contains('{')
    };
    if let Some(k) = inv
        .get("kbd")
        .items()
        .iter()
        .find(|k| internal(&k.get("PNPDeviceID").str().unwrap_or_default()))
    {
        let id = k.get("PNPDeviceID").str().unwrap_or_default();
        let mut p = Part::new("keyboard", Kind::Keyboard, "Built-in keyboard");
        p.tag = "Keyboard".into();
        p.opt("Name", s(k, "Description").or(s(k, "Name")));
        p.prop(
            "Bus",
            if id.to_uppercase().starts_with("ACPI\\") {
                "PS/2 (i8042)"
            } else {
                "HID (I2C/SPI)"
            },
        );
        p.prop("Device ID", &id);
        for (key, label) in [("caps", "Caps Lock"), ("numlock", "Num Lock")] {
            if let Some(on) = live.get(key).bool() {
                p.prop(label, if on { "on" } else { "off" });
                if on {
                    p.metric(format!("{label} on"));
                }
            }
        }
        out.push(p);
    }
    let tp = inv.get("ptr").items().iter().find(|d| {
        let id = d.get("PNPDeviceID").str().unwrap_or_default();
        let name = d.get("Name").str().unwrap_or_default().to_lowercase();
        internal(&id)
            && (name.contains("touch pad")
                || name.contains("touchpad")
                || touchpad_vendor(&id).is_some())
    });
    if let Some(t) = tp {
        let id = t.get("PNPDeviceID").str().unwrap_or_default();
        let title = match touchpad_vendor(&id) {
            Some(v) => format!("{v} touchpad"),
            None => s(t, "Name").unwrap_or_else(|| "Touchpad".into()),
        };
        let mut p = Part::new("touchpad", Kind::Touchpad, title);
        p.tag = "Touchpad".into();
        p.opt("Name", s(t, "Name"));
        p.opt("Manufacturer", s(t, "Manufacturer"));
        p.prop("Device ID", &id);
        let bus = if id.to_uppercase().starts_with("ACPI\\") {
            "PS/2 (i8042)"
        } else {
            "I2C"
        };
        p.prop("Bus", bus);
        p.metric(bus);
        out.push(p);
    }
    out
}

/// USB disks (MSFT_PhysicalDisk with BusType 7): (disk number, serial, size).
fn usb_disks(inv: &Json) -> Vec<(String, String, Option<u64>)> {
    inv.get("disk")
        .items()
        .iter()
        .filter(|d| n(d, "BusType") == Some(7.0))
        .filter_map(|d| {
            Some((
                d.get("DeviceId").str()?,
                d.get("SerialNumber").str().unwrap_or_default(),
                n(d, "Size").map(|v| v as u64),
            ))
        })
        .collect()
}

fn usb(
    inv: &Json,
    prop: &dyn Fn(&str, &str) -> Option<Json>,
    dio: &HashMap<String, (f64, f64)>,
) -> Vec<Part> {
    let mut devs: BTreeMap<String, UsbDev> = BTreeMap::new();
    for d in inv.get("pnp").items() {
        let id = d.get("InstanceId").str().unwrap_or_default();
        if !id.to_uppercase().starts_with("USB\\VID_") {
            continue;
        }
        let Some((vid, pid)) = vid_pid(&id) else {
            continue;
        };
        let iface = id.to_uppercase().contains("&MI_");
        // composite functions belong to their parent: merge by VID:PID
        let e = devs
            .entry(format!("{vid}:{pid}"))
            .or_insert_with(|| UsbDev {
                vid: vid.clone(),
                pid: pid.clone(),
                ..Default::default()
            });
        e.ids.push(id.clone());
        if let Some(nm) = s(d, "FriendlyName") {
            if !e.names.contains(&nm) {
                e.names.push(nm);
            }
        }
        if let Some(c) = s(d, "Class") {
            if !e.classes.contains(&c) {
                e.classes.push(c);
            }
        }
        if e.maker.is_none() {
            e.maker = s(d, "Manufacturer").filter(|m| !m.starts_with('(') && m != "Microsoft");
        }
        if !iface {
            e.bus_name =
                prop(&id, "DEVPKEY_Device_BusReportedDeviceDesc").and_then(|v| clean(v.str()));
            e.loc = prop(&id, "DEVPKEY_Device_LocationInfo").and_then(|v| v.str());
            e.removable = prop(&id, "DEVPKEY_Device_Capabilities")
                .and_then(|v| v.num())
                .map(|c| c as u32 & 0x4 != 0);
        }
    }
    let generic = |n: &str| {
        let l = n.to_lowercase();
        l.contains("composite")
            || l.starts_with("usb input device")
            || l.starts_with("usb mass storage")
            || l.contains("generic usb")
    };
    let mut out = Vec::new();
    // USB serial number: the last part of the device's instance id, unless
    // Windows made one up (those contain '&')
    let mut serials: Vec<Option<String>> = Vec::new();
    for (key, d) in devs {
        let all = d.names.join(" ").to_lowercase();
        if all.contains("hub") && d.classes.iter().all(|c| c == "USB") {
            continue;
        }
        let title = d
            .bus_name
            .clone()
            .or_else(|| d.names.iter().find(|n| !generic(n)).cloned())
            .or_else(|| d.names.first().cloned())
            .unwrap_or_else(|| format!("USB device {key}"));
        let camera = d.classes.iter().any(|c| c == "Camera" || c == "Image");
        let internal = d.removable == Some(false);
        let mut p = Part::new(
            format!("usb:{key}"),
            if camera && internal {
                Kind::Camera
            } else if internal {
                Kind::UsbInternal
            } else {
                Kind::Usb
            },
            title,
        );
        p.tag = if camera {
            "Camera".into()
        } else if all.contains("mass storage") {
            "Storage".into()
        } else {
            d.classes
                .iter()
                .find(|c| c.as_str() != "USB")
                .map(|c| match c.as_str() {
                    "HIDClass" | "Keyboard" | "Mouse" => "HID".to_string(),
                    "DiskDrive" | "WPD" => "Storage".to_string(),
                    "Bluetooth" => "Bluetooth".to_string(),
                    "MEDIA" | "AudioEndpoint" => "Audio".to_string(),
                    "Biometric" => "Fingerprint".to_string(),
                    other => other.to_string(),
                })
                .unwrap_or_else(|| "USB".into())
        };
        if let Some(port) = d.loc.as_deref().and_then(usb_port) {
            p.bind = vec![port.clone()];
            p.prop("Location", format!("hub {port}"));
        }
        p.prop("ID", format!("{}:{}", d.vid, d.pid));
        p.opt("Manufacturer", d.maker.clone());
        if !d.names.is_empty() {
            p.prop("Windows names", d.names.join(", "));
        }
        if !d.classes.is_empty() {
            p.prop("Classes", d.classes.join(", "));
        }
        p.prop(
            "Connection",
            match d.removable {
                Some(false) => "internal (not removable)",
                Some(true) => "external port",
                None => "unknown",
            },
        );
        serials.push(
            d.ids
                .iter()
                .find(|id| !id.to_uppercase().contains("&MI_"))
                .and_then(|id| id.rsplit('\\').next())
                .filter(|s| !s.contains('&'))
                .map(String::from),
        );
        out.push(p);
    }
    // which USB device each USB disk is: same serial, or the only candidate
    let disks = usb_disks(inv);
    let storage: Vec<usize> = (0..out.len())
        .filter(|&i| out[i].tag == "Storage")
        .collect();
    for (num, serial, size) in &disks {
        let serial = serial.trim();
        let by_serial = storage.iter().copied().find(|&i| {
            !serial.is_empty()
                && serials[i]
                    .as_deref()
                    .map_or(false, |s| s.eq_ignore_ascii_case(serial))
        });
        let only = (disks.len() == 1 && storage.len() == 1).then(|| storage[0]);
        let Some(i) = by_serial.or(only) else {
            continue;
        };
        let p = &mut out[i];
        if let Some(sz) = size {
            p.metric(fmt_dec(*sz));
        }
        p.prop(
            &format!("Disk \\\\.\\PhysicalDrive{num}"),
            size.map(fmt_dec).unwrap_or_default(),
        );
        if let Some((r, w)) = dio.get(num) {
            throughput(p, *r, *w);
        }
    }
    out
}

fn audio(inv: &Json) -> Vec<Part> {
    // built-in codecs: HD Audio / Intel SST / AMD ACP, not USB or Bluetooth
    let codecs: Vec<String> = inv
        .get("snd")
        .items()
        .iter()
        .filter(|d| {
            let id = d
                .get("PNPDeviceID")
                .str()
                .unwrap_or_default()
                .to_uppercase();
            id.starts_with("HDAUDIO\\")
                || id.starts_with("INTELAUDIO\\")
                || id.starts_with("ACPI\\")
        })
        .filter_map(|d| s(d, "Name"))
        .collect();
    let eps: Vec<(String, String)> = inv
        .get("ep")
        .items()
        .iter()
        .filter(|e| s(e, "Status").map_or(true, |st| st == "OK"))
        .filter_map(|e| {
            let n = s(e, "FriendlyName")?;
            let (kind, dev) = n.split_once(" (")?;
            Some((kind.to_string(), dev.trim_end_matches(')').to_string()))
        })
        .filter(|(_, dev)| {
            codecs
                .iter()
                .any(|c| c == dev || c.starts_with(dev.as_str()) || dev.starts_with(c.as_str()))
        })
        .collect();
    let mut out = Vec::new();
    if let Some((_, dev)) = eps
        .iter()
        .find(|(k, _)| k.to_lowercase().starts_with("speaker"))
    {
        for (id, side) in [("speaker:l", "left"), ("speaker:r", "right")] {
            let mut p = Part::new(id, Kind::Speaker, format!("Speaker ({side})"));
            p.tag = "Speaker".into();
            p.prop("Channel", side);
            p.prop("Codec", dev);
            out.push(p);
        }
    }
    if let Some((k, dev)) = eps
        .iter()
        .find(|(k, _)| k.to_lowercase().contains("microphone"))
    {
        let arr = k.to_lowercase().contains("array");
        let mut p = Part::new(
            "mic",
            Kind::Microphone,
            if arr {
                "Digital microphone array"
            } else {
                "Internal microphone"
            },
        );
        p.tag = "Mic".into();
        p.prop("Endpoint", k);
        p.prop("Codec", dev);
        out.push(p);
    }
    out
}

fn displays(inv: &Json, live: &Json) -> Vec<Part> {
    let mut out = Vec::new();
    for (i, m) in inv.get("mon").items().iter().enumerate() {
        let inst = s(m, "InstanceName").unwrap_or_default();
        let tech = inv
            .get("monconn")
            .items()
            .iter()
            .find(|c| s(c, "InstanceName").as_deref() == Some(inst.as_str()))
            .and_then(|c| n(c, "VideoOutputTechnology"))
            .map(|t| t as i64);
        let (ctype, internal) = match tech {
            Some(0x8000_0000) | Some(-2147483648) => ("internal", true),
            Some(6) => ("LVDS", true),
            Some(11) => ("eDP", true),
            Some(13) => ("UDI embedded", true),
            Some(5) => ("HDMI-A", false),
            Some(10) => ("DP", false),
            Some(4) => ("DVI", false),
            Some(0) => ("VGA", false),
            Some(12) => ("UDI", false),
            Some(16) => ("Miracast", false),
            _ => ("external", false),
        };
        let edid = inv
            .get("edid")
            .items()
            .iter()
            .find(|e| s(e, "InstanceName").as_deref() == Some(inst.as_str()))
            .and_then(|e| e.get("Edid").str())
            .and_then(|h| {
                (0..h.len() / 2)
                    .map(|j| u8::from_str_radix(h.get(2 * j..2 * j + 2)?, 16).ok())
                    .collect::<Option<Vec<u8>>>()
            })
            .and_then(|b| parse_edid(&b));
        let title = match &edid {
            Some(e) => edid_title(e),
            None => join(&[
                s(m, "Make").map(|v| panel_vendor(&v).to_string()),
                s(m, "Model").or(s(m, "Code")),
            ])
            .unwrap_or_else(|| {
                if internal {
                    "Built-in display".into()
                } else {
                    "Display".into()
                }
            }),
        };
        let mut p = Part::new(format!("display:{i}"), Kind::Display, title);
        p.tag = if internal {
            "Screen".into()
        } else {
            ctype.to_string()
        };
        p.bind = vec![
            if internal { "eDP" } else { ctype }.to_string(),
            (if internal { "internal" } else { "external" }).into(),
        ];
        p.prop("Connector", ctype);
        p.prop(
            "Type",
            if internal {
                "built-in panel"
            } else {
                "external monitor"
            },
        );
        p.prop("Instance", inst.split('\\').nth(1).unwrap_or(&inst));
        if let Some(e) = &edid {
            edid_props(&mut p, e);
        } else {
            p.opt("Model", s(m, "Model"));
            p.opt(
                "Manufacturer",
                s(m, "Make").map(|v| format!("{} ({v})", panel_vendor(&v))),
            );
            p.opt("Product code", s(m, "Code"));
            p.opt("Manufactured", s(m, "YearOfManufacture"));
        }
        if let Some(b) = live
            .get("bright")
            .items()
            .iter()
            .find(|b| s(b, "InstanceName").as_deref() == Some(inst.as_str()))
            .and_then(|b| n(b, "CurrentBrightness"))
        {
            p.level = Some((b / 100.0).clamp(0.0, 1.0));
            p.metric(format!("{b:.0}% brightness"));
            p.prop("Brightness", format!("{b:.0}%"));
        }
        out.push(p);
    }
    out
}

fn other_pci(
    inv: &Json,
    pci_addr: &dyn Fn(&str) -> Option<String>,
    prop: &dyn Fn(&str, &str) -> Option<Json>,
    claimed: &HashSet<String>,
) -> Vec<Part> {
    let mut out = Vec::new();
    for d in inv.get("pnp").items() {
        let id = d.get("InstanceId").str().unwrap_or_default();
        let Some(addr) = pci_addr(&id) else { continue };
        let class = s(d, "Class").unwrap_or_default();
        if claimed.contains(&addr)
            || addr.starts_with("0000:00:")
            || ["Display", "Net", "System"].contains(&class.as_str())
        {
            continue;
        }
        let mut p = Part::new(
            format!("pci:{addr}"),
            Kind::Pci,
            s(d, "FriendlyName").unwrap_or_else(|| "PCI device".into()),
        );
        p.tag = match class.as_str() {
            "SCSIAdapter" | "HDC" => "Storage controller".into(),
            "USB" => "USB controller".into(),
            "MEDIA" => "Multimedia controller".into(),
            "SDHost" => "SD card reader".into(),
            "" => "PCI device".into(),
            c => c.to_string(),
        };
        p.bind = vec![addr.clone()];
        p.prop("PCI address", &addr);
        if let Some((v, dv)) = pci_ven_dev(&id) {
            p.prop("PCI ID", format!("{v:04x}:{dv:04x}"));
        }
        p.prop("Class", &class);
        p.opt("Manufacturer", s(d, "Manufacturer"));
        pcie_link(&mut p, &id, prop);
        out.push(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plumbing() {
        assert_eq!(b64(b"Man"), "TWFu");
        assert_eq!(b64(b"Ma"), "TWE=");
        assert_eq!(b64(b"M"), "TQ==");
        assert_eq!(encoded("a"), "YQA=");
        assert_eq!(
            pci_address("PCI bus 1, device 0, function 0").as_deref(),
            Some("0000:01:00.0")
        );
        assert_eq!(
            pci_address("PCI bus 0, device 31, function 3").as_deref(),
            Some("0000:00:1f.3")
        );
        assert_eq!(usb_port("Port_#0003.Hub_#0001").as_deref(), Some("1-3"));
        assert_eq!(link_mbps("1 Gbps"), 1000);
        assert_eq!(link_mbps("2.5 Gbps"), 2500);
        assert_eq!(link_mbps("866.7 Mbps"), 867);
        assert_eq!(link_mbps("0 bps"), 0);
        assert_eq!(
            vid_pid("USB\\VID_046D&PID_C52B&MI_00\\7&1"),
            Some(("046d".into(), "c52b".into()))
        );
        let w = parse_netsh("\nThere is 1 interface on the system:\n\n    Name                   : Wi-Fi\n    Description            : Intel(R) Wi-Fi 6 AX201 160MHz\n    State                  : connected\n    Radio type             : 802.11ax\n    Channel                : 44\n    Signal                 : 92%\n");
        assert_eq!(w["Wi-Fi"]["Signal"], "92%");
        assert_eq!(w["Wi-Fi"]["Radio type"], "802.11ax");
    }
}
