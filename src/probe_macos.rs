//! macOS hardware probing (Intel and Apple silicon).
//!
//! No private frameworks, no FFI: everything comes from tools every Mac has.
//!
//! * `system_profiler -json` — the inventory (model, RAM, SSD, GPUs, displays,
//!   Wi-Fi, USB tree, audio, camera, battery health). Slow, so it is refreshed
//!   on a background thread every 30 s.
//! * `sysctl`, `vm_stat`, `ps`, `netstat`, `ifconfig` — CPU, memory, load,
//!   network traffic.
//! * `ioreg` — battery live values, GPU utilization (and on AMD GPUs their
//!   temperature/clocks/power), disk throughput, backlight, display EDIDs.
//! * `hidutil list` — the built-in keyboard and trackpad.
//! * `powermetrics` (root only) — CPU/GPU temperatures and fan on Intel,
//!   per-core clocks and load, package/CPU/GPU power, thermal pressure.
//!
//! `INSIDER_CAPTURE=/dir` records every tool's output; replay it anywhere with
//! `INSIDER_SYSROOT=/dir INSIDER_PLATFORM=macos`.

use crate::json::Json;
use crate::model::*;
use crate::platform::{Options, Refresh};
use crate::probe::{
    chassis_name, clean, clean_cpu_name, edid_props, edid_title, join, leading_num, panel_vendor,
    parse_edid, run, usb_speed,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------- tool output parsers

/// `sysctl -a` style "key: value" lines.
pub(crate) fn parse_sysctl(s: &str) -> HashMap<String, String> {
    s.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(": ").or_else(|| l.split_once(" = "))?;
            (!k.contains(' ')).then(|| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// One registry object from `ioreg` text output.
#[derive(Debug, Default, Clone)]
pub(crate) struct IoObj {
    pub name: String,
    pub class: String,
    pub props: Vec<(String, String)>,
}

impl IoObj {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.props
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        io_num(self.get(k)?)
    }
}

pub(crate) fn parse_ioreg(s: &str) -> Vec<IoObj> {
    let mut out: Vec<IoObj> = Vec::new();
    for l in s.lines() {
        let t = l.trim_start_matches(|c: char| c == ' ' || c == '|');
        if let Some(rest) = t.strip_prefix("+-o ") {
            let (name, tail) = rest.split_once("  <").unwrap_or((rest, ""));
            let class = tail
                .strip_prefix("class ")
                .and_then(|c| c.split([',', '>']).next())
                .unwrap_or("")
                .to_string();
            out.push(IoObj {
                name: name.trim().to_string(),
                class,
                props: Vec::new(),
            });
        } else if let Some(rest) = t.strip_prefix('"') {
            let Some((k, v)) = rest.split_once("\" = ") else {
                continue;
            };
            if out.is_empty() {
                out.push(IoObj::default());
            }
            if let Some(o) = out.last_mut() {
                o.props.push((k.to_string(), v.trim().to_string()));
            }
        }
    }
    out
}

/// ioreg integer; negative values are printed as unsigned 64-bit.
pub(crate) fn io_num(v: &str) -> Option<f64> {
    let v = v.trim().trim_matches('"');
    match v {
        "Yes" => return Some(1.0),
        "No" => return Some(0.0),
        _ => {}
    }
    if let Some(h) = v.strip_prefix("0x") {
        return u64::from_str_radix(h, 16).ok().map(|x| x as i64 as f64);
    }
    if let Ok(u) = v.parse::<u64>() {
        return Some(u as i64 as f64);
    }
    v.parse::<f64>().ok()
}

/// Value of `"key"=...` inside an inline ioreg dictionary such as
/// `{"Device Utilization %"=12,"In use system memory"=12345}`.
pub(crate) fn io_inline(v: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"=");
    let i = v.find(&pat)? + pat.len();
    let rest = &v[i..];
    let mut depth = 0i32;
    let mut in_str = false;
    for (j, c) in rest.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '{' | '(' if !in_str => depth += 1,
            '}' | ')' if !in_str => {
                if depth == 0 {
                    return Some(rest[..j].trim().to_string());
                }
                depth -= 1;
            }
            ',' if !in_str && depth == 0 => return Some(rest[..j].trim().to_string()),
            _ => {}
        }
    }
    Some(rest.trim().to_string())
}

fn io_data(v: &str) -> Option<Vec<u8>> {
    let h = v.trim().strip_prefix('<')?.strip_suffix('>')?;
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(h.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

/// `vm_stat`: (page size, "Pages free" -> count, ...).
pub(crate) fn parse_vm_stat(s: &str) -> (u64, HashMap<String, u64>) {
    let mut page = 4096;
    let mut m = HashMap::new();
    for l in s.lines() {
        if let Some(i) = l.find("page size of ") {
            page = leading_num(&l[i + 13..]).map(|v| v as u64).unwrap_or(4096);
        } else if let Some((k, v)) = l.split_once(':') {
            if let Some(n) = leading_num(v) {
                m.insert(k.trim().trim_matches('"').to_string(), n as u64);
            }
        }
    }
    (page, m)
}

/// `netstat -ibn`: interface -> (rx bytes, tx bytes), link-level rows only.
pub(crate) fn parse_netstat(s: &str) -> HashMap<String, (f64, f64)> {
    let mut m = HashMap::new();
    for l in s.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 10 || !f[2].starts_with("<Link#") {
            continue;
        }
        // the Address column is empty for interfaces without a MAC
        let (ib, ob) = if f.len() >= 11 { (6, 9) } else { (5, 8) };
        if let (Ok(r), Ok(t)) = (f[ib].parse::<f64>(), f[ob].parse::<f64>()) {
            m.entry(f[0].trim_end_matches('*').to_string())
                .or_insert((r, t));
        }
    }
    m
}

/// A row of `hidutil list`.
#[derive(Debug, Default, Clone)]
pub(crate) struct Hid {
    pub vendor: u32,
    pub product_id: u32,
    pub usage_page: u32,
    pub usage: u32,
    pub transport: String,
    pub product: String,
    pub built_in: bool,
}

/// `hidutil list` prints tables whose columns widen with long values, so rows
/// are split into words: only the Product column may contain spaces, every
/// other column is one word.
pub(crate) fn parse_hidutil(s: &str) -> Vec<Hid> {
    let mut out = Vec::new();
    let mut cols: Vec<String> = Vec::new();
    for l in s.lines() {
        if l.contains("VendorID") && l.contains("Product") {
            cols = l.split_whitespace().map(String::from).collect();
            continue;
        }
        let words: Vec<&str> = l.split_whitespace().collect();
        let Some(pi) = cols.iter().position(|c| c == "Product") else {
            continue;
        };
        let after = cols.len() - pi - 1;
        if words.len() < cols.len() || l.ends_with(':') {
            continue;
        }
        let field = |name: &str| -> String {
            let Some(ci) = cols.iter().position(|c| c == name) else {
                return String::new();
            };
            if ci < pi {
                words[ci].to_string()
            } else if ci == pi {
                words[pi..words.len() - after].join(" ")
            } else {
                words[words.len() - (cols.len() - ci)].to_string()
            }
        };
        let hex = |s: String| u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0);
        let h = Hid {
            vendor: hex(field("VendorID")),
            product_id: hex(field("ProductID")),
            usage_page: hex(field("UsagePage")),
            usage: hex(field("Usage")),
            transport: field("Transport"),
            product: field("Product"),
            built_in: field("Built-In") == "1",
        };
        if !h.product.is_empty() && h.product != "(null)" {
            out.push(h);
        }
    }
    out
}

/// What one `powermetrics` sample told us.
#[derive(Debug, Default, Clone)]
pub(crate) struct Pm {
    pub cpu_temp: Option<f64>,
    pub gpu_temp: Option<f64>,
    pub fans: Vec<f64>,
    pub pkg_w: Option<f64>,
    pub cpu_w: Option<f64>,
    pub gpu_w: Option<f64>,
    pub ane_w: Option<f64>,
    pub avg_mhz: Option<f64>,
    /// logical CPU -> (MHz, active %)
    pub cpus: BTreeMap<u32, (Option<f64>, Option<f64>)>,
    /// physical core -> C-state residency % (Intel)
    pub core_idle: BTreeMap<u32, f64>,
    pub clusters: Vec<(String, f64)>,
    pub gpu_mhz: Option<f64>,
    pub gpu_active: Option<f64>,
    pub pressure: Option<String>,
    pub cpu_thermal_level: Option<String>,
}

fn after_colon(l: &str) -> &str {
    l.split_once(':').map(|x| x.1.trim()).unwrap_or("")
}

/// Number inside the last "(... Mhz)" of a line.
fn paren_mhz(l: &str) -> Option<f64> {
    let i = l.rfind('(')?;
    leading_num(&l[i + 1..])
}

pub(crate) fn parse_powermetrics(s: &str) -> Pm {
    let mut pm = Pm::default();
    let mut last_cpu: Option<u32> = None;
    let mut section = "";
    for l in s.lines() {
        let l = l.trim();
        if l.starts_with("****") {
            section = if l.contains("GPU") { "gpu" } else { "" };
            continue;
        }
        let v = after_colon(l);
        let watts = || {
            let n = leading_num(v)?;
            Some(if v.contains("mW") { n / 1000.0 } else { n })
        };
        if l.starts_with("CPU die temperature") {
            pm.cpu_temp = leading_num(v);
        } else if l.starts_with("GPU die temperature") {
            pm.gpu_temp = leading_num(v);
        } else if l.starts_with("Fan") && l.contains("rpm") {
            pm.fans.extend(leading_num(v));
        } else if l.starts_with("CPU Thermal level") {
            pm.cpu_thermal_level = Some(v.to_string());
        } else if l.starts_with("Current pressure level") {
            pm.pressure = Some(v.to_string());
        } else if l.to_ascii_lowercase().contains("package power") {
            pm.pkg_w = watts();
        } else if l.starts_with("CPU Power") {
            pm.cpu_w = watts();
        } else if l.starts_with("GPU Power") {
            pm.gpu_w = watts();
        } else if l.starts_with("ANE Power") {
            pm.ane_w = watts();
        } else if l.starts_with("System Average frequency") {
            pm.avg_mhz = paren_mhz(l);
        } else if let Some(r) = l.strip_prefix("CPU ") {
            // "CPU 3 frequency: 1200 MHz" / "CPU 3 active residency: 12.5% (...)"
            // Intel: "CPU 3 duty cycles/s: ..." then
            //        "CPU Average frequency as fraction of nominal: 60% (1563 Mhz)"
            let num: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(n) = num.parse::<u32>() {
                let e = pm.cpus.entry(n).or_default();
                if r.contains("frequency") {
                    e.0 = leading_num(v);
                } else if r.contains("active residency") {
                    e.1 = leading_num(v);
                } else if r.contains("duty cycles") {
                    last_cpu = Some(n);
                }
            } else if r.starts_with("Average frequency") {
                if let Some(c) = last_cpu.take() {
                    pm.cpus.entry(c).or_default().0 = paren_mhz(l);
                }
            }
        } else if let Some(r) = l.strip_prefix("Core ") {
            if let Some((n, rest)) = r.split_once(' ') {
                if rest.starts_with("C-state residency") {
                    if let (Ok(n), Some(x)) = (n.parse::<u32>(), leading_num(after_colon(rest))) {
                        pm.core_idle.insert(n, x);
                    }
                }
            }
        } else if l.contains("Cluster HW active frequency") {
            if let Some(f) = leading_num(v) {
                let name = l.split(" HW").next().unwrap_or("").to_string();
                pm.clusters.push((name, f));
            }
        } else if section == "gpu" || l.starts_with("GPU") {
            if l.contains("active frequency") {
                pm.gpu_mhz = leading_num(v);
            } else if l.contains("active residency") {
                pm.gpu_active = leading_num(v);
            }
        }
    }
    pm
}

/// Apple's "0x80CE"-style JEDEC ids and hex-encoded part numbers.
fn jedec(s: &str) -> String {
    match s.to_ascii_uppercase().as_str() {
        "0X80CE" | "0XCE00" => "Samsung".into(),
        "0X80AD" | "0XAD00" => "SK hynix".into(),
        "0X802C" | "0X2C00" => "Micron".into(),
        "0X0198" | "0X9801" => "Kingston".into(),
        "0X859B" | "0X9B05" => "Crucial".into(),
        _ => s.to_string(),
    }
}

fn hex_ascii(s: &str) -> String {
    match s.strip_prefix("0x") {
        Some(h) if h.len() % 2 == 0 && h.chars().all(|c| c.is_ascii_hexdigit()) => {
            let t: String = (0..h.len() / 2)
                .filter_map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).ok())
                .filter(|b| (0x20..0x7f).contains(b))
                .map(|b| b as char)
                .collect();
            t.trim().to_string()
        }
        _ => s.to_string(),
    }
}

/// "spdisplays_yes" -> "yes", "sppci_vendor_Apple" -> "Apple".
fn sp_word(s: &str) -> String {
    let mut t = s;
    for p in [
        "spdisplays_",
        "sppci_vendor_",
        "sppci_",
        "spairport_status_",
        "spairport_security_mode_",
        "spairport_",
        "spaudio_",
        "coreaudio_device_type_",
        "spnvme_",
        "attrib_",
        "ibridge_",
    ] {
        if let Some(r) = t.strip_prefix(p) {
            t = r;
            break;
        }
    }
    t.replace('_', " ")
}

/// Look up a key in a system_profiler object, also trying keys that end with
/// the given name (newer macOS uses e.g. `USBDeviceKeyVendorID`).
fn spk<'a>(o: &'a Json, names: &[&str]) -> Option<&'a Json> {
    for n in names {
        let v = o.get(n);
        if !v.is_null() {
            return Some(v);
        }
    }
    let lc: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    o.entries()
        .into_iter()
        .find(|(k, _)| {
            let k = k.to_ascii_lowercase();
            lc.iter().any(|n| k.ends_with(n.as_str()))
        })
        .map(|(_, v)| v)
}

fn sps(o: &Json, names: &[&str]) -> Option<String> {
    clean(spk(o, names).and_then(|v| v.str()))
}

/// Every object below `j` (depth-first) that satisfies `f`, not descending
/// into matches.
fn walk<'a>(j: &'a Json, f: &dyn Fn(&Json) -> bool, out: &mut Vec<&'a Json>) {
    match j {
        Json::Arr(v) => v.iter().for_each(|x| walk(x, f, out)),
        Json::Obj(m) => {
            if f(j) {
                out.push(j);
                return;
            }
            m.values().for_each(|x| walk(x, f, out));
        }
        _ => {}
    }
}

/// "0x14120000 / 3" -> (bus 0x14, ports [1, 2]) -> "20-1.2"
pub(crate) fn usb_location(loc: &str) -> Option<(String, String)> {
    let h = loc.split_whitespace().next()?.trim_start_matches("0x");
    let v = u32::from_str_radix(h, 16).ok()?;
    let bus = v >> 24;
    let mut ports = Vec::new();
    for i in (0..6).rev() {
        let n = (v >> (i * 4)) & 0xf;
        if n == 0 {
            break;
        }
        ports.push(n.to_string());
    }
    if ports.is_empty() {
        return None;
    }
    let root = format!("{bus}-{}", ports[0]);
    Some((root, format!("{bus}-{}", ports.join("."))))
}

fn edid_vendor(id: u32) -> String {
    [(id >> 10) & 31, (id >> 5) & 31, id & 31]
        .iter()
        .map(|c| (*c as u8 + 64) as char)
        .collect()
}

// ---------------------------------------------------------------- slow inventory

const SP_FAST: [&str; 13] = [
    "SPHardwareDataType",
    "SPMemoryDataType",
    "SPNVMeDataType",
    "SPSerialATADataType",
    "SPDisplaysDataType",
    "SPPowerDataType",
    "SPUSBDataType",
    "SPAudioDataType",
    "SPCameraDataType",
    "SPiBridgeDataType",
    "SPSPIDataType",
    "SPEthernetDataType",
    "SPPCIDataType",
];
/// Slow ones (Wi-Fi scan, Bluetooth) and ones older macOS may not know.
const SP_SLOW: [&str; 3] = [
    "SPAirPortDataType",
    "SPBluetoothDataType",
    "SPUSBHostDataType",
];

#[derive(Default)]
struct Inventory {
    /// "SPHardwareDataType" -> its array
    sp: BTreeMap<String, Json>,
    sysctl: HashMap<String, String>,
    hid: Vec<Hid>,
    edids: Vec<Vec<u8>>,
    /// (hardware port name, device), from networksetup
    ports: Vec<(String, String)>,
    complete: bool,
}

fn system_profiler(types: &[&str]) -> BTreeMap<String, Json> {
    let mut args = vec!["-json"];
    args.extend_from_slice(types);
    let mut m = BTreeMap::new();
    let parsed = run("system_profiler", &args).and_then(|o| Json::parse(&o));
    match parsed {
        Some(Json::Obj(o)) => m.extend(o),
        _ if types.len() > 1 => {
            // an unknown data type on this macOS version? ask one at a time
            for t in types {
                if let Some(Json::Obj(o)) =
                    run("system_profiler", &["-json", t]).and_then(|o| Json::parse(&o))
                {
                    m.extend(o);
                }
            }
        }
        _ => {}
    }
    m
}

fn inventory(complete: bool) -> Inventory {
    let mut sp = system_profiler(&SP_FAST);
    if complete {
        sp.extend(system_profiler(&SP_SLOW));
    }
    let edids = run("ioreg", &["-l", "-w0", "-r", "-k", "IODisplayEDID"])
        .map(|o| {
            parse_ioreg(&o)
                .iter()
                .filter_map(|x| io_data(x.get("IODisplayEDID")?))
                .collect()
        })
        .unwrap_or_default();
    let mut ports = Vec::new();
    let mut port = String::new();
    for l in run("networksetup", &["-listallhardwareports"])
        .unwrap_or_default()
        .lines()
    {
        if let Some(p) = l.strip_prefix("Hardware Port: ") {
            port = p.trim().to_string();
        } else if let Some(d) = l.strip_prefix("Device: ") {
            ports.push((port.clone(), d.trim().to_string()));
        }
    }
    Inventory {
        sp,
        sysctl: run("sysctl", &["-a"])
            .map(|o| parse_sysctl(&o))
            .unwrap_or_default(),
        hid: run("hidutil", &["list"])
            .map(|o| parse_hidutil(&o))
            .unwrap_or_default(),
        edids,
        ports,
        complete,
    }
}

impl Inventory {
    fn sp(&self, t: &str) -> &[Json] {
        self.sp.get(t).map(|j| j.items()).unwrap_or(&[])
    }
    fn hw(&self) -> &Json {
        self.sp("SPHardwareDataType").first().unwrap_or(&Json::Null)
    }
    fn ctl(&self, k: &str) -> Option<String> {
        clean(self.sysctl.get(k).cloned())
    }
    fn ctln(&self, k: &str) -> Option<f64> {
        self.sysctl.get(k).and_then(|v| leading_num(v))
    }
    fn apple_silicon(&self) -> bool {
        self.ctln("hw.optional.arm64") == Some(1.0)
            || self
                .ctl("machdep.cpu.brand_string")
                .map_or(false, |b| b.starts_with("Apple"))
            || self.hw().get("chip_type").str().is_some()
    }
}

// ---------------------------------------------------------------- the prober

pub struct Prober {
    opts: Options,
    inv: Refresh<Inventory>,
    root: bool,
    notes: Vec<String>,
    prev: HashMap<String, (f64, Instant)>,
    pm_ok: bool,
    vol: Option<(Instant, Option<String>)>,
    nvidia: HashMap<String, HashMap<String, String>>,
}

impl Prober {
    pub fn new(opts: Options) -> Self {
        let root = run("id", &["-u"]).map_or(false, |s| s.trim() == "0");
        let first = inventory(false);
        let inv = Refresh::new(first, Duration::from_secs(30), || inventory(true));
        Prober {
            opts,
            inv,
            root,
            notes: Vec::new(),
            prev: HashMap::new(),
            pm_ok: true,
            vol: None,
            nvidia: HashMap::new(),
        }
    }

    pub fn settle(&self) {
        self.inv.wait(Duration::from_secs(20));
    }

    fn rate(&mut self, key: &str, v: f64) -> Option<f64> {
        let now = Instant::now();
        let r = self.prev.get(key).and_then(|(pv, pt)| {
            let dt = now.duration_since(*pt).as_secs_f64();
            (dt > 0.05 && v >= *pv).then(|| (v - pv) / dt)
        });
        self.prev.insert(key.to_string(), (v, now));
        r
    }

    fn powermetrics(&mut self, inv: &Inventory) -> Pm {
        if !self.root || !self.pm_ok {
            return Pm::default();
        }
        let samplers = if inv.apple_silicon() {
            "cpu_power,gpu_power,thermal"
        } else {
            "smc,cpu_power,gpu_power,thermal"
        };
        match run(
            "powermetrics",
            &["-n", "1", "-i", "200", "--samplers", samplers],
        ) {
            Some(o) => parse_powermetrics(&o),
            None => {
                self.pm_ok = false;
                self.notes
                    .push("powermetrics failed: no temperatures, clocks or power".into());
                Pm::default()
            }
        }
    }

    /// (used, total) bytes of RAM.
    fn mem_use(&self, inv: &Inventory) -> Option<(u64, u64)> {
        let total = inv.ctln("hw.memsize")? as u64;
        let (page, m) = parse_vm_stat(&run("vm_stat", &[])?);
        let pages = |k: &str| m.get(k).copied().unwrap_or(0);
        let used = (pages("Pages active")
            + pages("Pages wired down")
            + pages("Pages occupied by compressor"))
            * page;
        Some((used.min(total), total))
    }

    pub fn gather(&mut self) -> Snapshot {
        let inv = self.inv.get();
        let pm = self.powermetrics(&inv);
        let mem_use = self.mem_use(&inv);
        let dmi = dmi(&inv);
        let mut parts = Vec::new();
        parts.push(self.board(&inv, &dmi, &pm, mem_use));
        let accels = accels();
        parts.push(self.cpu(&inv, &pm, &accels));
        parts.extend(self.gpus(&inv, &pm, &accels));
        parts.extend(dimms(&inv, mem_use));
        parts.extend(self.disks(&inv));
        parts.extend(self.nets(&inv));
        if let Some(t2) = t2(&inv) {
            parts.push(t2);
        }
        parts.push(bios(&inv));
        parts.extend(battery(&inv));
        parts.extend(pm.fans.iter().enumerate().map(|(i, rpm)| {
            let mut p = Part::new(format!("fan:smc:{i}"), Kind::Fan, format!("Fan {}", i + 1));
            p.tag = "Fan".into();
            p.rpm = Some(*rpm as u32);
            p.metric(format!("{rpm:.0} RPM"));
            p.prop("Speed", format!("{rpm:.0} RPM"));
            p.prop("Sensor", "SMC (powermetrics)");
            p
        }));
        let (inputs, claimed_usb) = inputs(&inv);
        parts.extend(inputs);
        let usb = usb(&inv, &claimed_usb);
        let has_cam = usb.iter().any(|p| p.kind == Kind::Camera);
        parts.extend(usb);
        if !has_cam {
            parts.extend(cameras(&inv));
        }
        let vol = self.volume();
        parts.extend(audio(&inv, vol));
        parts.extend(displays(&inv));
        parts.extend(pci_cards(&inv));

        let mut notes = self.notes.clone();
        if !self.root {
            notes.push(
                "Not running as root: temperatures, fans, per-core clocks and power need `sudo insider` (they come from powermetrics)"
                    .into(),
            );
        }
        if inv.apple_silicon() {
            notes.push(
                "Apple silicon: macOS has no command-line tool for per-sensor temperatures; thermal pressure is shown instead"
                    .into(),
            );
        }
        if !inv.complete {
            notes.push("Wi-Fi and Bluetooth details are still loading…".into());
        }
        Snapshot { parts, dmi, notes }
    }

    // ------------------------------------------------------------ board

    fn board(
        &mut self,
        inv: &Inventory,
        dmi: &HashMap<String, String>,
        pm: &Pm,
        mem_use: Option<(u64, u64)>,
    ) -> Part {
        let hw = inv.hw();
        let g = |k: &str| dmi.get(k).cloned();
        let title = join(&[g("product_name"), g("board_name").map(|b| format!("({b})"))])
            .unwrap_or_else(|| "Mainboard".into());
        let mut p = Part::new("board", Kind::Board, title);
        p.tag = "Logic board".into();
        p.opt("Model name", sps(hw, &["machine_name"]));
        p.opt(
            "Model identifier",
            sps(hw, &["machine_model"]).or(inv.ctl("hw.model")),
        );
        p.opt("Model number", sps(hw, &["model_number"]));
        p.opt("Chip", sps(hw, &["chip_type"]));
        if let Some(c) = g("chassis_type").and_then(|s| s.parse::<u32>().ok()) {
            p.prop("Chassis", format!("{} ({c})", chassis_name(c)));
        }
        p.opt(
            "macOS",
            inv.ctl("kern.osproductversion").map(|v| {
                format!(
                    "{v}{}",
                    inv.ctl("kern.osversion")
                        .map(|b| format!(" ({b})"))
                        .unwrap_or_default()
                )
            }),
        );
        p.opt(
            "Kernel",
            inv.ctl("kern.osrelease").map(|k| format!("Darwin {k}")),
        );
        p.opt("Hostname", inv.ctl("kern.hostname"));
        let boot = inv
            .sysctl
            .get("kern.boottime")
            .and_then(|b| b.split("sec = ").nth(1))
            .and_then(leading_num);
        if let (Some(b), Ok(now)) = (boot, SystemTime::now().duration_since(UNIX_EPOCH)) {
            p.prop(
                "Uptime",
                fmt_hours((now.as_secs_f64() - b).max(0.0) / 3600.0),
            );
        }
        if let Some(l) = run("sysctl", &["-n", "vm.loadavg"]) {
            let l = l.trim().trim_matches(|c| c == '{' || c == '}').trim();
            p.prop(
                "Load average",
                l.split_whitespace().take(3).collect::<Vec<_>>().join(" "),
            );
        }
        if let Some((u, t)) = mem_use {
            p.prop("Memory in use", format!("{} of {}", fmt_bin(u), fmt_bin(t)));
        }
        p.opt("Thermal pressure", pm.pressure.clone());
        if let Some(pr) = &pm.pressure {
            p.metric(format!("thermal: {}", pr.to_lowercase()));
        }
        p.opt("CPU thermal level", pm.cpu_thermal_level.clone());
        p.opt(
            "Activation Lock",
            sps(hw, &["activation_lock_status"]).map(|s| sp_word(&s)),
        );
        p
    }

    // ------------------------------------------------------------ CPU

    fn cpu(&mut self, inv: &Inventory, pm: &Pm, accels: &[(String, String)]) -> Part {
        let brand = inv
            .ctl("machdep.cpu.brand_string")
            .or_else(|| sps(inv.hw(), &["chip_type", "cpu_type"]))
            .unwrap_or_else(|| "CPU".into());
        let mut p = Part::new("cpu0", Kind::Cpu, clean_cpu_name(&brand));
        p.tag = if inv.apple_silicon() { "SoC" } else { "CPU" }.into();
        p.bind = vec!["socket0".into(), "0".into()];
        p.prop("Model", &brand);
        let phys = inv.ctln("hw.physicalcpu").unwrap_or(0.0) as u32;
        let logical = inv.ctln("hw.logicalcpu").unwrap_or(0.0) as u32;
        if logical > 0 {
            p.prop("Cores / threads", format!("{} / {logical}", phys.max(1)));
        }
        // Apple silicon: performance levels (P / E clusters)
        let nlev = inv.ctln("hw.nperflevels").unwrap_or(0.0) as u32;
        for i in 0..nlev {
            let k = |s: &str| inv.ctl(&format!("hw.perflevel{i}.{s}"));
            if let (Some(name), Some(n)) = (k("name"), k("physicalcpu")) {
                let l2 = k("l2cachesize")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|b| format!(", {} L2", fmt_cache(b)))
                    .unwrap_or_default();
                p.prop(&format!("{name} cores"), format!("{n}{l2}"));
            }
        }
        p.opt("Vendor", inv.ctl("machdep.cpu.vendor"));
        if let (Some(f), Some(m), Some(s)) = (
            inv.ctl("machdep.cpu.family"),
            inv.ctl("machdep.cpu.model"),
            inv.ctl("machdep.cpu.stepping"),
        ) {
            p.prop("Family / model / stepping", format!("{f} / {m} / {s}"));
        }
        p.opt("Microcode", inv.ctl("machdep.cpu.microcode_version"));
        let cache: Vec<String> = [("L2", "hw.l2cachesize"), ("L3", "hw.l3cachesize")]
            .iter()
            .filter_map(|(n, k)| {
                let b = inv.ctln(k)? as u64;
                (b > 0).then(|| format!("{} {n}", fmt_cache(b)))
            })
            .collect();
        if !cache.is_empty() {
            p.prop("Cache", cache.join(", "));
        }
        if let Some(hz) = inv.ctln("hw.cpufrequency").filter(|v| *v > 0.0) {
            let max = inv
                .ctln("hw.cpufrequency_max")
                .filter(|m| *m > hz)
                .map(|m| format!(" (max {:.2} GHz)", m / 1e9))
                .unwrap_or_default();
            p.prop("Nominal clock", format!("{:.2} GHz{max}", hz / 1e9));
        }
        if inv.ctln("kern.hv_vmm_present") == Some(1.0) {
            p.prop("Virtualized", "yes (running under a hypervisor)");
        }

        // live: frequency
        let freqs: Vec<f64> = pm.cpus.values().filter_map(|c| c.0).collect();
        if !freqs.is_empty() {
            let avg = freqs.iter().sum::<f64>() / freqs.len() as f64 / 1000.0;
            let max = freqs.iter().cloned().fold(0.0, f64::max) / 1000.0;
            p.metric(format!("{avg:.2} GHz"));
            p.prop(
                "Clock (avg / fastest core)",
                format!("{avg:.2} / {max:.2} GHz"),
            );
        } else if let Some(m) = pm.avg_mhz {
            p.metric(format!("{:.2} GHz", m / 1000.0));
            p.prop("Clock (average)", format!("{:.2} GHz", m / 1000.0));
        }
        for (name, mhz) in &pm.clusters {
            p.prop(&format!("{name} clock"), format!("{mhz:.0} MHz"));
        }
        // live: load (ps %cpu is per logical CPU, summed)
        if let Some(o) = run("ps", &["-A", "-o", "%cpu="]) {
            let sum: f64 = o.lines().filter_map(|l| l.trim().parse::<f64>().ok()).sum();
            let u = (sum / logical.max(1) as f64).clamp(0.0, 100.0);
            p.metric(format!("{u:.0}% load"));
            p.prop("Utilization", format!("{u:.1}%"));
        }
        if let Some(w) = pm.pkg_w.or(pm.cpu_w) {
            p.metric(format!("{w:.1} W"));
            p.prop(
                if pm.pkg_w.is_some() {
                    "Package power"
                } else {
                    "CPU power"
                },
                format!("{w:.2} W"),
            );
        }
        if let Some(w) = pm.ane_w {
            p.prop("Neural Engine power", format!("{w:.2} W"));
        }
        p.temp = pm.cpu_temp;
        if pm.cpu_temp.is_some() {
            p.prop("Sensor", "SMC CPU die (powermetrics)");
        }

        // physical cores; Intel threads of one core are numbered next to each other
        let tpc = if phys > 0 { (logical / phys).max(1) } else { 1 };
        let mut by_core: BTreeMap<u32, Vec<(Option<f64>, Option<f64>)>> = BTreeMap::new();
        for c in 0..phys {
            by_core.entry(c).or_default();
        }
        for (cpu, v) in &pm.cpus {
            by_core.entry(cpu / tpc).or_default().push(*v);
        }
        p.cores = by_core
            .into_iter()
            .map(|(id, v)| {
                let ghz = v
                    .iter()
                    .filter_map(|x| x.0)
                    .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))))
                    .map(|m| m / 1000.0);
                let loads: Vec<f64> = v.iter().filter_map(|x| x.1).collect();
                let load = if !loads.is_empty() {
                    Some(loads.iter().sum::<f64>() / loads.len() as f64)
                } else {
                    pm.core_idle.get(&id).map(|idle| (100.0 - idle).max(0.0))
                };
                Core {
                    id,
                    temp: None,
                    ghz,
                    load,
                }
            })
            .collect();

        // integrated graphics live in the package (Intel iGPU, Apple GPU)
        for g in inv
            .sp("SPDisplaysDataType")
            .iter()
            .filter(|g| integrated_gpu(g))
        {
            let name = sps(g, &["sppci_model", "_name"]).unwrap_or_else(|| "GPU".into());
            p.prop("Integrated GPU", &name);
            p.opt("GPU cores", sps(g, &["sppci_cores"]));
            p.opt(
                "Metal",
                sps(g, &["spdisplays_mtlgpufamilysupport", "spdisplays_metal"])
                    .map(|m| sp_word(&m)),
            );
            p.opt(
                "iGPU memory",
                sps(g, &["spdisplays_vram_shared", "spdisplays_vram"]),
            );
            if let Some(a) = accel_for(&name, accels) {
                if let Some(u) = io_inline(a, "Device Utilization %").and_then(|v| io_num(&v)) {
                    p.prop("GPU utilization", format!("{u:.0}%"));
                }
            }
            if inv.apple_silicon() {
                if let Some(m) = pm.gpu_mhz {
                    p.prop("GPU clock", format!("{m:.0} MHz"));
                }
                if let Some(w) = pm.gpu_w {
                    p.prop("GPU power", format!("{w:.2} W"));
                }
            }
        }
        p
    }

    // ------------------------------------------------------------ GPU

    fn gpus(&mut self, inv: &Inventory, pm: &Pm, accels: &[(String, String)]) -> Vec<Part> {
        if self.opts.nvidia_smi {
            // web-driver era Macs only; harmless when absent
            self.nvidia = crate::probe::query_nvidia();
        }
        let mut out = Vec::new();
        for (i, g) in inv
            .sp("SPDisplaysDataType")
            .iter()
            .filter(|g| !integrated_gpu(g))
            .enumerate()
        {
            let name = sps(g, &["sppci_model", "_name"]).unwrap_or_else(|| "GPU".into());
            let mut p = Part::new(format!("gpu:{i}"), Kind::Gpu, name.clone());
            p.tag = "GPU".into();
            p.bind = vec![format!("gpu{i}")];
            p.opt(
                "Vendor",
                sps(g, &["spdisplays_vendor"]).map(|v| {
                    let w = sp_word(&v);
                    if w.len() <= 3 {
                        w.to_uppercase()
                    } else {
                        w
                    }
                }),
            );
            p.opt("Bus", sps(g, &["sppci_bus"]).map(|v| sp_word(&v)));
            p.opt("PCIe width", sps(g, &["spdisplays_pcie_width"]));
            p.opt("VRAM", sps(g, &["spdisplays_vram", "_spdisplays_vram"]));
            p.opt("Device ID", sps(g, &["spdisplays_device-id"]));
            p.opt("Revision", sps(g, &["spdisplays_revision-id"]));
            p.opt("ROM revision", sps(g, &["spdisplays_rom-revision"]));
            p.opt("EFI driver", sps(g, &["spdisplays_efi-version"]));
            p.opt("gMux version", sps(g, &["spdisplays_gmux-version"]));
            p.opt(
                "Metal",
                sps(g, &["spdisplays_mtlgpufamilysupport", "spdisplays_metal"])
                    .map(|m| sp_word(&m)),
            );
            if let Some(a) = accel_for(&name, accels) {
                let f = |k: &str| io_inline(a, k).and_then(|v| io_num(&v));
                if let Some(u) = f("Device Utilization %").or_else(|| f("GPU Activity(%)")) {
                    p.metric(format!("{u:.0}%"));
                    p.prop("Utilization", format!("{u:.0}%"));
                }
                if let Some(c) = f("Core Clock(MHz)") {
                    p.metric(format!("{c:.0} MHz"));
                    p.prop("Core clock", format!("{c:.0} MHz"));
                }
                p.opt(
                    "Memory clock",
                    f("Memory Clock(MHz)").map(|c| format!("{c:.0} MHz")),
                );
                if let Some(w) = f("Total Power(W)") {
                    p.metric(format!("{w:.0} W"));
                    p.prop("Power draw", format!("{w:.1} W"));
                }
                p.opt("Fan", f("Fan Speed(%)").map(|c| format!("{c:.0}%")));
                if let Some(b) = f("vramUsedBytes") {
                    p.prop("VRAM in use", fmt_bin(b as u64));
                }
                p.temp = f("Temperature(C)").filter(|t| *t > 0.0);
                if p.temp.is_none() {
                    p.temp = pm.gpu_temp;
                }
            }
            if let Some(nv) = self.nvidia.values().next() {
                if name.to_ascii_lowercase().contains("nvidia")
                    || name.to_ascii_lowercase().contains("geforce")
                {
                    crate::probe::nvidia_props(&mut p, nv);
                }
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ storage

    fn disks(&mut self, inv: &Inventory) -> Vec<Part> {
        // ioreg: each IOBlockStorageDriver (with Statistics) is followed by its IOMedia (BSD Name)
        let mut stats: HashMap<String, (f64, f64)> = HashMap::new();
        if let Some(o) = run(
            "ioreg",
            &["-r", "-l", "-d", "2", "-w0", "-c", "IOBlockStorageDriver"],
        ) {
            let objs = parse_ioreg(&o);
            let mut pending: Option<(f64, f64)> = None;
            for x in &objs {
                if let Some(s) = x.get("Statistics") {
                    pending = io_inline(s, "Bytes (Read)")
                        .and_then(|v| io_num(&v))
                        .zip(io_inline(s, "Bytes (Write)").and_then(|v| io_num(&v)));
                } else if let (Some(b), Some(st)) = (x.get("BSD Name"), pending) {
                    stats.insert(b.trim_matches('"').to_string(), st);
                    pending = None;
                }
            }
        }
        let mut drives: Vec<(&'static str, &Json, Option<String>)> = Vec::new();
        for (bus, t) in [("nvme", "SPNVMeDataType"), ("sata", "SPSerialATADataType")] {
            for ctl in inv.sp(t) {
                let mut v = Vec::new();
                walk(
                    ctl,
                    &|o| !o.get("device_model").is_null() || !o.get("size_in_bytes").is_null(),
                    &mut v,
                );
                let cname = sps(ctl, &["_name"]);
                for d in v {
                    drives.push((bus, d, cname.clone()));
                }
            }
        }
        let mut out = Vec::new();
        for (i, (bus, d, ctl)) in drives.into_iter().enumerate() {
            let yes = |k: &str| {
                d.get(k)
                    .str()
                    .map_or(false, |v| v.eq_ignore_ascii_case("yes"))
            };
            if yes("removable_media") || yes("detachable_drive") {
                continue; // shown with its USB/Thunderbolt device
            }
            let model = sps(d, &["device_model", "_name"]).unwrap_or_else(|| "SSD".into());
            let mut p = Part::new(format!("{bus}:{i}"), Kind::Ssd, model);
            let size = d.get("size_in_bytes").num().map(|s| s as u64);
            let rot = d
                .get("spsata_medium_type")
                .str()
                .map_or(false, |m| m.to_lowercase().contains("rotational"));
            let label = if bus == "nvme" {
                "NVMe"
            } else if rot {
                "HDD"
            } else {
                "SATA"
            };
            p.tag = match size {
                Some(s) => format!("{label} {}", fmt_dec(s)),
                None => label.into(),
            };
            p.bind = vec![bus.into()];
            let bsd = sps(d, &["bsd_name"]);
            if let Some(b) = &bsd {
                p.bind.push(b.clone());
                p.prop("Device", format!("/dev/{b}"));
            }
            p.opt("Controller", ctl);
            if let Some(s) = size {
                p.metric(fmt_dec(s));
                p.prop("Capacity", fmt_dec(s));
            }
            p.opt("Firmware", sps(d, &["device_revision"]));
            p.opt("Serial", sps(d, &["device_serial"]));
            if let (Some(s), Some(w)) =
                (sps(d, &["spnvme_linkspeed"]), sps(d, &["spnvme_linkwidth"]))
            {
                p.prop("PCIe link", format!("{s} {w}"));
            }
            p.opt("Link", sps(d, &["spsata_negotiated_linkspeed"]));
            p.opt("Medium", sps(d, &["spsata_medium_type"]));
            p.opt(
                "TRIM",
                sps(d, &["spnvme_trim_support", "spsata_trim_support"]),
            );
            p.opt("S.M.A.R.T.", sps(d, &["smart_status"]).map(|s| sp_word(&s)));
            if let Some((r, w)) = bsd.as_ref().and_then(|b| stats.get(b)).copied() {
                let b = bsd.clone().unwrap_or_default();
                if let (Some(r), Some(w)) = (
                    self.rate(&format!("{b}:r"), r),
                    self.rate(&format!("{b}:w"), w),
                ) {
                    p.metric(format!("R {} W {}", fmt_rate(r), fmt_rate(w)));
                    p.prop(
                        "Throughput",
                        format!("read {}  write {}", fmt_rate(r), fmt_rate(w)),
                    );
                }
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ network

    fn nets(&mut self, inv: &Inventory) -> Vec<Part> {
        let traffic = run("netstat", &["-ibn"])
            .map(|o| parse_netstat(&o))
            .unwrap_or_default();
        let mut out = Vec::new();
        let bt = inv
            .sp("SPBluetoothDataType")
            .first()
            .map(|b| b.get("controller_properties").clone())
            .unwrap_or(Json::Null);
        let wifi_ifs: Vec<&Json> = inv
            .sp("SPAirPortDataType")
            .iter()
            .flat_map(|a| a.get("spairport_airport_interfaces").items())
            .filter(|i| !i.get("spairport_wireless_card_type").is_null())
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        let mut ifs: Vec<(String, Kind, Option<&Json>)> = Vec::new();
        for w in &wifi_ifs {
            if let Some(n) = sps(w, &["_name"]) {
                seen.insert(n.clone());
                ifs.push((n, Kind::Wifi, Some(*w)));
            }
        }
        for (port, dev) in &inv.ports {
            if seen.contains(dev) {
                continue;
            }
            let pl = port.to_lowercase();
            if pl == "wi-fi" || pl == "airport" {
                seen.insert(dev.clone());
                ifs.push((dev.clone(), Kind::Wifi, None));
            } else if pl.starts_with("ethernet")
                && !pl.contains("usb")
                && !pl.contains("thunderbolt")
            {
                seen.insert(dev.clone());
                ifs.push((dev.clone(), Kind::Ethernet, None));
            }
        }
        for (ifn, kind, w) in ifs {
            let wifi = kind == Kind::Wifi;
            let card = w.and_then(|w| sps(w, &["spairport_wireless_card_type"]));
            let chip = sps(&bt, &["controller_chipset"]);
            let title = if wifi {
                match (&chip, &card) {
                    (Some(c), _) if c.starts_with("BCM") => {
                        format!("Broadcom {} (Wi-Fi + Bluetooth)", c.replace('_', ""))
                    }
                    (_, Some(c)) if c.contains("0x14E4") || c.contains("0x14e4") => {
                        format!("Broadcom {}", c.replace("  ", " "))
                    }
                    (_, Some(c)) => c.replace("  ", " "),
                    _ => "Wi-Fi".into(),
                }
            } else {
                port_name(inv, &ifn).unwrap_or_else(|| "Ethernet".into())
            };
            let mut p = Part::new(format!("net:{ifn}"), kind, title);
            p.tag = if wifi { "Wi-Fi" } else { "Ethernet" }.into();
            p.bind = vec![ifn.clone()];
            p.prop("Interface", &ifn);
            if let Some(w) = w {
                p.opt("Card type", card.clone());
                p.opt("Firmware", sps(w, &["spairport_wireless_firmware_version"]));
                p.opt("PHY modes", sps(w, &["spairport_supported_phymodes"]));
                p.opt("Locale", sps(w, &["spairport_wireless_locale"]));
                let cur = w.get("spairport_current_network_information");
                if !cur.is_null() {
                    p.opt("PHY mode", sps(cur, &["spairport_network_phymode"]));
                    p.opt("Channel", sps(cur, &["spairport_network_channel"]));
                    p.opt(
                        "Tx rate",
                        sps(cur, &["spairport_network_rate"]).map(|r| format!("{r} Mb/s")),
                    );
                    p.opt(
                        "Security",
                        sps(cur, &["spairport_security_mode"]).map(|s| sp_word(&s)),
                    );
                    if let Some(sn) = sps(cur, &["spairport_signal_noise"]) {
                        if let Some(sig) = sn.split('/').next() {
                            p.metric(sig.trim().to_string());
                        }
                        p.prop("Signal / noise", sn);
                    }
                }
            }
            if wifi && !bt.is_null() {
                p.opt("Bluetooth chipset", chip);
                p.opt(
                    "Bluetooth firmware",
                    sps(&bt, &["controller_firmwareVersion"]),
                );
                p.opt(
                    "Bluetooth state",
                    sps(&bt, &["controller_state"]).map(|s| sp_word(&s)),
                );
                p.opt("Bluetooth transport", sps(&bt, &["controller_transport"]));
            }
            if let Some(o) = run("ifconfig", &[&ifn]) {
                for l in o.lines() {
                    let l = l.trim();
                    if let Some(s) = l.strip_prefix("status: ") {
                        p.metric(s.to_string());
                        p.prop("State", s);
                    } else if let Some(m) = l.strip_prefix("media: ") {
                        p.prop("Media", m);
                    } else if let Some(m) = l.strip_prefix("ether ") {
                        p.prop("MAC", m.trim());
                    } else if let Some(i) = l.find(" mtu ") {
                        p.opt("MTU", leading_num(&l[i + 5..]).map(|m| m as u32));
                    }
                }
            }
            if let Some((rx, tx)) = traffic.get(&ifn).copied() {
                if let (Some(r), Some(t)) = (
                    self.rate(&format!("{ifn}:rx"), rx),
                    self.rate(&format!("{ifn}:tx"), tx),
                ) {
                    p.metric(format!("↓{} ↑{}", fmt_rate(r), fmt_rate(t)));
                    p.prop(
                        "Traffic",
                        format!("down {}  up {}", fmt_rate(r), fmt_rate(t)),
                    );
                }
            }
            out.push(p);
        }
        out
    }

    fn volume(&mut self) -> (Option<String>, Option<String>) {
        // osascript is slowish: refresh every 3 s
        if self
            .vol
            .as_ref()
            .map_or(true, |(t, _)| t.elapsed().as_secs() >= 3)
        {
            self.vol = Some((
                Instant::now(),
                run("osascript", &["-e", "get volume settings"]),
            ));
        }
        let s = self
            .vol
            .as_ref()
            .and_then(|v| v.1.clone())
            .unwrap_or_default();
        let field = |k: &str| {
            s.split(',')
                .find_map(|kv| kv.trim().strip_prefix(k))
                .map(|v| v.trim().to_string())
        };
        let muted = field("output muted:").as_deref() == Some("true");
        let out = field("output volume:")
            .filter(|v| v != "missing value")
            .map(|v| {
                if muted {
                    "muted".into()
                } else {
                    format!("{v}%")
                }
            });
        let inp = field("input volume:")
            .filter(|v| v != "missing value")
            .map(|v| format!("{v}%"));
        (out, inp)
    }
}

// ---------------------------------------------------------------- free functions

/// Cache sizes: KiB below 1 MiB.
pub(crate) fn fmt_cache(b: u64) -> String {
    if b < 1 << 20 {
        format!("{} KiB", b >> 10)
    } else {
        fmt_bin(b)
    }
}

/// (accelerator class, PerformanceStatistics) per GPU, via ioreg.
fn accels() -> Vec<(String, String)> {
    run("ioreg", &["-r", "-d", "1", "-w0", "-c", "IOAccelerator"])
        .map(|o| {
            parse_ioreg(&o)
                .into_iter()
                .filter_map(|x| {
                    let ps = x.get("PerformanceStatistics")?.to_string();
                    Some((x.class.clone(), ps))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn integrated_gpu(g: &Json) -> bool {
    let bus = g.get("sppci_bus").str().unwrap_or_default();
    bus.contains("builtin")
        || g.get("sppci_model").str().map_or(false, |m| {
            m.starts_with("Apple M") || m.starts_with("Intel")
        })
}

/// The IOAccelerator whose class matches this GPU's maker.
fn accel_for<'a>(gpu: &str, accels: &'a [(String, String)]) -> Option<&'a str> {
    let g = gpu.to_ascii_lowercase();
    let want: &[&str] = if g.contains("intel") {
        &["intel"]
    } else if g.contains("amd") || g.contains("radeon") {
        &["amd", "radeon"]
    } else if g.contains("nvidia") || g.contains("geforce") {
        &["nv", "geforce"]
    } else {
        &["agx", "apple"]
    };
    accels
        .iter()
        .find(|(c, _)| {
            let c = c.to_ascii_lowercase();
            want.iter().any(|w| c.contains(w))
        })
        .map(|(_, s)| s.as_str())
}

fn port_name(inv: &Inventory, dev: &str) -> Option<String> {
    inv.ports
        .iter()
        .find(|(_, d)| d == dev)
        .map(|(p, _)| p.clone())
}

/// DMI-like identity, so board layouts can match Macs the same way as PCs:
/// `sys_vendor = "Apple"`, `board_name = "MacBookPro16,1"` (model identifier).
fn dmi(inv: &Inventory) -> HashMap<String, String> {
    let hw = inv.hw();
    let mut m = HashMap::new();
    let name = sps(hw, &["machine_name"]);
    let model = sps(hw, &["machine_model"]).or(inv.ctl("hw.model"));
    m.insert("sys_vendor".into(), "Apple Inc.".into());
    m.insert("board_vendor".into(), "Apple".into());
    if let Some(n) = &name {
        m.insert("product_name".into(), n.clone());
        m.insert("product_family".into(), n.clone());
    }
    if let Some(b) = &model {
        m.insert("board_name".into(), b.clone());
        m.insert("product_version".into(), b.clone());
    }
    if let Some(s) = sps(hw, &["model_number"]) {
        m.insert("product_sku".into(), s);
    }
    if let Some(v) = sps(hw, &["boot_rom_version"]) {
        m.insert("bios_version".into(), v);
    }
    m.insert("bios_vendor".into(), "Apple Inc.".into());
    let n = name.unwrap_or_default().to_lowercase();
    let chassis = if n.contains("macbook") {
        Some(10)
    } else if n.contains("imac") {
        Some(13)
    } else if n.contains("mac mini") {
        Some(35)
    } else if n.contains("mac pro") {
        Some(7)
    } else if n.contains("mac studio") {
        Some(3)
    } else {
        None
    };
    if let Some(c) = chassis {
        m.insert("chassis_type".into(), c.to_string());
    }
    m
}

fn dimms(inv: &Inventory, mem_use: Option<(u64, u64)>) -> Vec<Part> {
    let mut out = Vec::new();
    let usage = |p: &mut Part| {
        if let Some((used, total)) = mem_use {
            let pct = 100.0 * used as f64 / total.max(1) as f64;
            p.metric(format!("{pct:.0}% used"));
            p.prop(
                "System RAM in use",
                format!("{} of {} ({pct:.0}%)", fmt_bin(used), fmt_bin(total)),
            );
        }
    };
    for (bi, bank) in inv.sp("SPMemoryDataType").iter().enumerate() {
        let upgradeable = sps(bank, &["is_memory_upgradeable"]);
        let slots = bank.get("_items").items();
        if slots.is_empty() {
            // Apple silicon (and some Intel Macs): one unified, soldered pool
            let size = sps(bank, &["SPMemoryDataType", "dimm_size"]);
            let ty = sps(bank, &["dimm_type"]);
            let maker = sps(bank, &["dimm_manufacturer"]).map(|m| jedec(&m));
            if size.is_none() && ty.is_none() {
                continue;
            }
            let short = size
                .as_deref()
                .and_then(crate::probe::parse_size)
                .map(|b| format!("{}G", (b as f64 / (1u64 << 30) as f64).round()))
                .unwrap_or_default();
            let mut p = Part::new(
                format!("dimm{bi}"),
                Kind::Dimm,
                join(&[maker.clone(), ty.clone()]).unwrap_or_else(|| "Unified memory".into()),
            );
            p.tag = format!("{short} {}", ty.clone().unwrap_or_default())
                .trim()
                .to_string();
            p.bind = vec!["unified".into()];
            p.metric(p.tag.clone());
            usage(&mut p);
            p.opt("Size", size);
            p.opt("Type", ty);
            p.opt("Manufacturer", maker);
            p.prop(
                "Form factor",
                "soldered (in the SoC package on Apple silicon)",
            );
            p.opt("Upgradeable", upgradeable.clone());
            p.prop("Source", "system_profiler");
            out.push(p);
            continue;
        }
        for (i, s) in slots.iter().enumerate() {
            let full = sps(s, &["_name"]).unwrap_or_else(|| format!("DIMM{i}"));
            let (bankl, loc) = match full.split_once('/') {
                Some((b, l)) => (Some(b.to_string()), Some(l.to_string())),
                None => (None, Some(full.clone())),
            };
            let mut p = Part::new(
                format!("dimm{}", out.len()),
                Kind::Dimm,
                "Empty memory slot",
            );
            p.bind = [loc.clone(), bankl.clone(), Some(full.clone())]
                .into_iter()
                .flatten()
                .collect();
            p.opt("Locator", loc.clone());
            p.opt("Bank", bankl);
            let size = sps(s, &["dimm_size"])
                .filter(|v| !v.eq_ignore_ascii_case("empty"))
                .and_then(|v| crate::probe::parse_size(&v));
            let Some(size) = size.filter(|_| {
                sps(s, &["dimm_status"]).map_or(true, |st| !st.eq_ignore_ascii_case("empty"))
            }) else {
                p.present = false;
                p.tag = "empty".into();
                out.push(p);
                continue;
            };
            let ty = sps(s, &["dimm_type"]);
            let maker = sps(s, &["dimm_manufacturer"]).map(|m| jedec(&m));
            let part_no = sps(s, &["dimm_part_number"]).map(|m| hex_ascii(&m));
            p.title = join(&[maker.clone(), part_no.clone()])
                .or_else(|| loc.clone())
                .unwrap_or_else(|| "Memory module".into());
            p.tag = format!(
                "{}G {}",
                (size as f64 / (1u64 << 30) as f64).round(),
                ty.clone().unwrap_or_default()
            )
            .trim()
            .to_string();
            if upgradeable.as_deref() == Some("Yes") {
                p.form = Some("SODIMM".into());
            }
            p.metric(p.tag.clone());
            usage(&mut p);
            p.prop("Size", fmt_bin(size));
            p.opt("Type", ty);
            p.opt(
                "Speed",
                sps(s, &["dimm_speed"]).map(|v| v.replace("MHz", "MT/s")),
            );
            p.opt("Manufacturer", maker);
            p.opt("Part number", part_no);
            p.opt("Serial", sps(s, &["dimm_serial_number"]));
            p.opt("Status", sps(s, &["dimm_status"]));
            p.opt(
                "ECC",
                sps(bank, &["global_ecc_state"]).map(|e| e.replace('_', " ")),
            );
            p.opt("Upgradeable", upgradeable.clone());
            p.prop("Source", "system_profiler");
            out.push(p);
        }
    }
    if out.is_empty() {
        let mut p = Part::new("dimm0", Kind::Dimm, "System memory");
        p.tag = inv
            .ctln("hw.memsize")
            .map(|b| fmt_bin(b as u64))
            .unwrap_or_else(|| "RAM".into());
        p.metric(p.tag.clone());
        usage(&mut p);
        out.push(p);
    }
    out
}

/// Apple T2 (Intel Macs 2018-2020): the security/SSD/camera controller.
fn t2(inv: &Inventory) -> Option<Part> {
    let b = inv.sp("SPiBridgeDataType").first()?;
    let model = sps(b, &["ibridge_model_name"])?;
    let mut p = Part::new("chipset", Kind::Chipset, model);
    p.tag = "T2".into();
    p.opt("bridgeOS build", sps(b, &["ibridge_build"]));
    for (k, v) in b.entries() {
        if let Some(s) = v.str() {
            if k.starts_with("ibridge_")
                && !["ibridge_model_name", "ibridge_build", "ibridge_boot_uuid"].contains(&k)
            {
                let key = sp_word(k);
                let val = sp_word(&s);
                let val = val
                    .strip_prefix(key.as_str())
                    .unwrap_or(&val)
                    .trim()
                    .to_string();
                p.prop(&key, val);
            }
        }
    }
    Some(p)
}

fn bios(inv: &Inventory) -> Part {
    let hw = inv.hw();
    let v = sps(hw, &["boot_rom_version"]);
    let asi = inv.apple_silicon();
    let fw = if asi { "iBoot" } else { "Apple EFI" };
    let mut p = Part::new(
        "bios",
        Kind::Bios,
        match &v {
            Some(v) => format!("{fw} {}", v.split(" (").next().unwrap_or(v)),
            None => fw.to_string(),
        },
    );
    p.tag = "Boot ROM".into();
    if let Some(v) = &v {
        p.metric(v.split(" (").next().unwrap_or(v).to_string());
    }
    p.prop("Vendor", "Apple Inc.");
    p.opt("Boot ROM version", v);
    p.opt("OS loader version", sps(hw, &["os_loader_version"]));
    p.opt(
        "SMC version",
        sps(hw, &["SMC_version_system", "smc_version_system"]),
    );
    p.prop(
        "Firmware type",
        if asi {
            "iBoot (Apple silicon)"
        } else {
            "UEFI (Apple EFI)"
        },
    );
    p
}

fn battery(inv: &Inventory) -> Vec<Part> {
    let Some(b) = run("ioreg", &["-r", "-w0", "-c", "AppleSmartBattery"]).and_then(|o| {
        parse_ioreg(&o)
            .into_iter()
            .find(|x| x.get("CurrentCapacity").is_some())
    }) else {
        return Vec::new();
    };
    let sp = inv.sp("SPPowerDataType");
    let sp_bat = sp
        .iter()
        .find(|x| !x.get("sppower_battery_health_info").is_null())
        .cloned()
        .unwrap_or(Json::Null);
    let s = |k: &str| {
        b.get(k)
            .map(|v| v.trim_matches('"').to_string())
            .and_then(|v| clean(Some(v)))
    };
    let n = |k: &str| b.num(k);
    let mut p = Part::new(
        "bat:InternalBattery",
        Kind::Battery,
        join(&[s("Manufacturer"), s("DeviceName")]).unwrap_or_else(|| "Internal battery".into()),
    );
    p.tag = "Battery".into();
    p.bind = vec!["InternalBattery".into(), "BAT0".into()];
    if n("BatteryInstalled") == Some(0.0) {
        p.present = false;
        return vec![p];
    }
    let charging = n("IsCharging") == Some(1.0);
    let ac = n("ExternalConnected") == Some(1.0);
    let full_ch = n("FullyCharged") == Some(1.0);
    let status = if charging {
        "Charging"
    } else if ac && full_ch {
        "Full"
    } else if ac {
        "Not charging"
    } else {
        "Discharging"
    };
    let discharging = status == "Discharging";
    // Intel: Current/MaxCapacity in mAh. Apple silicon: they are percentages
    // and the mAh values are AppleRaw*.
    let pct = n("CurrentCapacity")
        .zip(n("MaxCapacity"))
        .filter(|(_, m)| *m > 0.0)
        .map(|(c, m)| c / m);
    let raw = |k: &str, fallback: &str| {
        n(k).or_else(|| n(fallback).filter(|_| n("MaxCapacity").map_or(false, |m| m > 100.0)))
    };
    let now = raw("AppleRawCurrentCapacity", "CurrentCapacity");
    let full = raw("AppleRawMaxCapacity", "MaxCapacity");
    let design = n("DesignCapacity");
    let v = n("Voltage").map(|mv| mv / 1000.0);
    let amps = n("InstantAmperage").or(n("Amperage")).map(|ma| ma / 1000.0);
    let pw = amps.zip(v).map(|(a, v)| (a * v).abs());
    let cycles = n("CycleCount").map(|c| c as i64);
    p.level = pct.map(|f| f.clamp(0.0, 1.0));
    if let Some(f) = pct {
        p.metric(format!("{:.0}%", f * 100.0));
        p.prop("Charge", format!("{:.0}%", f * 100.0));
    }
    p.metric(status);
    if let Some(v) = v {
        p.metric(format!("{v:.2} V"));
    }
    if let Some(w) = pw.filter(|w| *w > 0.05) {
        let sign = if discharging {
            "−"
        } else if charging {
            "+"
        } else {
            ""
        };
        p.metric(format!("{sign}{w:.1} W"));
    }
    if let Some(c) = cycles {
        p.metric(format!("{c} cycles"));
    }
    p.prop("Status", status);
    p.opt("Voltage", v.map(|v| format!("{v:.3} V")));
    p.opt("Current", amps.map(|a| format!("{:.3} A", a.abs())));
    p.opt(
        if discharging {
            "Power draw"
        } else {
            "Charge rate"
        },
        pw.map(|w| format!("{w:.2} W")),
    );
    let mah = |x: Option<f64>| {
        x.map(|x| match v {
            Some(v) => format!("{x:.0} mAh ({:.1} Wh)", x * v / 1000.0),
            None => format!("{x:.0} mAh"),
        })
    };
    p.opt("Now", mah(now));
    p.opt("Full", mah(full));
    p.opt("Design", mah(design));
    if let (Some(f), Some(d)) = (full, design) {
        if d > 0.0 {
            p.prop("Health (full / design)", format!("{:.0}%", 100.0 * f / d));
        }
    }
    p.opt("Cycle count", cycles);
    p.opt(
        "Design cycle count",
        n("DesignCycleCount9C").map(|c| c as i64),
    );
    let health = sp_bat.get("sppower_battery_health_info");
    p.opt("Condition", sps(health, &["sppower_battery_health"]));
    p.opt(
        "Maximum capacity (macOS)",
        sps(health, &["sppower_battery_health_maximum_capacity"]),
    );
    let mins = if discharging {
        n("AvgTimeToEmpty").or(n("TimeRemaining"))
    } else if charging {
        n("AvgTimeToFull").or(n("TimeRemaining"))
    } else {
        None
    };
    if let Some(m) = mins.filter(|m| *m > 0.0 && *m < 65535.0) {
        p.prop(
            if discharging {
                "Time to empty"
            } else {
                "Time to full"
            },
            fmt_hours(m / 60.0),
        );
    }
    p.temp = n("Temperature")
        .or(n("VirtualTemperature"))
        .map(|t| t / 100.0)
        .filter(|t| *t > -20.0 && *t < 90.0);
    p.opt("Chemistry", sps(&sp_bat, &["sppower_battery_chemistry"]));
    p.opt("Manufacturer", s("Manufacturer"));
    p.opt("Model", s("DeviceName"));
    p.opt(
        "Charger",
        sp.iter()
            .find_map(|x| {
                sps(
                    x.get("sppower_ac_charger_information"),
                    &["sppower_charger_watts"],
                )
            })
            .map(|w| format!("{w} W")),
    );
    p.prop("AC adapter", if ac { "connected" } else { "unplugged" });
    vec![p]
}

/// Built-in keyboard and trackpad (from `hidutil list`). Also returns the
/// names of USB devices they are, so they aren't drawn twice.
fn inputs(inv: &Inventory) -> (Vec<Part>, HashSet<String>) {
    let mut out = Vec::new();
    let mut claimed = HashSet::new();
    let bi: Vec<&Hid> = inv.hid.iter().filter(|h| h.built_in).collect();
    let kbd = bi
        .iter()
        .find(|h| h.usage_page == 1 && h.usage == 6)
        .or_else(|| {
            bi.iter()
                .find(|h| h.product.to_lowercase().contains("keyboard"))
        });
    if let Some(k) = kbd {
        let mut p = Part::new(
            "keyboard",
            Kind::Keyboard,
            if k.product.starts_with("Apple Internal") {
                "Apple internal keyboard".to_string()
            } else {
                k.product.clone()
            },
        );
        p.tag = "Keyboard".into();
        p.prop("Name", &k.product);
        p.prop("Bus", &k.transport);
        p.prop("ID", format!("{:04x}:{:04x}", k.vendor, k.product_id));
        p.metric(k.transport.clone());
        claimed.insert(k.product.clone());
        out.push(p);
    }
    let tp = bi
        .iter()
        .find(|h| h.usage_page == 0x0d && h.usage == 5)
        .or_else(|| {
            bi.iter()
                .find(|h| h.product.to_lowercase().contains("trackpad"))
        });
    if let Some(t) = tp {
        let mut p = Part::new(
            "touchpad",
            Kind::Touchpad,
            if t.product.starts_with("Apple Internal") {
                "Apple Force Touch trackpad".to_string()
            } else {
                t.product.clone()
            },
        );
        p.tag = "Trackpad".into();
        p.prop("Name", &t.product);
        p.prop("Bus", &t.transport);
        p.prop("ID", format!("{:04x}:{:04x}", t.vendor, t.product_id));
        p.metric(t.transport.clone());
        claimed.insert(t.product.clone());
        out.push(p);
    }
    (out, claimed)
}

fn usb(inv: &Inventory, claimed: &HashSet<String>) -> Vec<Part> {
    let mut devs: Vec<&Json> = Vec::new();
    for t in ["SPUSBDataType", "SPUSBHostDataType"] {
        for root in inv.sp(t) {
            walk_usb(root, &mut devs);
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for d in devs {
        let name = sps(d, &["_name", "ProductName"]).unwrap_or_else(|| "USB device".into());
        if claimed.contains(&name)
            || name.to_lowercase().contains("hub")
            || !d.get("_items").items().is_empty()
        {
            continue; // hubs (incl. the T2's internal bus) and the built-in keyboard
        }
        let loc = sps(d, &["location_id", "LocationID"]).unwrap_or_default();
        if !loc.is_empty() && !seen.insert(loc.clone()) {
            continue;
        }
        let vid_raw = sps(d, &["vendor_id", "VendorID"]).unwrap_or_default();
        let vid = if vid_raw.starts_with("apple_vendor_id") {
            "05ac".to_string()
        } else {
            vid_raw
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_start_matches("0x")
                .to_string()
        };
        let pid = sps(d, &["product_id", "ProductID"])
            .unwrap_or_default()
            .trim_start_matches("0x")
            .to_string();
        let maker = sps(d, &["manufacturer", "VendorName"])
            .or_else(|| {
                vid_raw
                    .split_once('(')
                    .map(|x| x.1.trim_end_matches(')').trim().to_string())
            })
            .or_else(|| (vid == "05ac").then(|| "Apple Inc.".to_string()));
        let lname = name.to_lowercase();
        let built_in = sps(d, &["Built-in_Device", "BuiltIn"])
            .map_or(false, |v| v.eq_ignore_ascii_case("yes"))
            || [
                "internal",
                "built-in",
                "bluetooth",
                "ibridge",
                "t2 controller",
                "touch bar",
                "ambient light",
                "facetime",
            ]
            .iter()
            .any(|k| lname.contains(k));
        let camera = lname.contains("camera") || lname.contains("facetime");
        let place = usb_location(&loc);
        let mut p = Part::new(
            format!(
                "usb:{}",
                place
                    .as_ref()
                    .map(|x| x.1.clone())
                    .unwrap_or_else(|| name.clone())
            ),
            if camera && built_in {
                Kind::Camera
            } else if built_in {
                Kind::UsbInternal
            } else {
                Kind::Usb
            },
            name.clone(),
        );
        p.tag = if camera {
            "Camera".into()
        } else if !d.get("Media").is_null() {
            "Storage".into()
        } else {
            "USB".into()
        };
        if let Some((root, full)) = place {
            p.bind = vec![root.clone()];
            if full != root {
                p.bind.push(full.clone());
            }
            p.prop(
                "Location",
                format!(
                    "{full}{}",
                    if full.contains('.') {
                        " (behind a hub)"
                    } else {
                        ""
                    }
                ),
            );
        }
        let speed = sps(d, &["device_speed", "LinkSpeed", "Speed"]).unwrap_or_default();
        let mbps = match speed.as_str() {
            "low_speed" => "1.5".to_string(),
            "full_speed" => "12".into(),
            "high_speed" => "480".into(),
            "super_speed" => "5000".into(),
            "super_speed_plus" => "10000".into(),
            s if s.contains("Gb") => leading_num(s)
                .map(|v| format!("{}", (v * 1000.0) as u64))
                .unwrap_or_default(),
            s => leading_num(s).map(|v| format!("{v}")).unwrap_or_default(),
        };
        let (sp_short, sp_long) = usb_speed(&mbps);
        p.metric(sp_short);
        if !vid.is_empty() && !pid.is_empty() {
            p.prop("ID", format!("{vid}:{pid}"));
        }
        p.opt("Manufacturer", maker);
        p.prop("Product", &name);
        p.prop("Speed", sp_long);
        p.opt("Version", sps(d, &["bcd_device"]));
        p.opt(
            "Bus power",
            sps(d, &["bus_power"]).map(|v| format!("{v} mA")),
        );
        p.opt(
            "Power used",
            sps(d, &["bus_power_used"]).map(|v| format!("{v} mA")),
        );
        p.prop(
            "Connection",
            if built_in {
                "internal"
            } else {
                "external port"
            },
        );
        for m in d.get("Media").items() {
            if let Some(s) = m.get("size_in_bytes").num() {
                p.metric(fmt_dec(s as u64));
                p.prop(
                    &format!("Disk /dev/{}", sps(m, &["bsd_name"]).unwrap_or_default()),
                    fmt_dec(s as u64),
                );
            }
        }
        out.push(p);
    }
    out
}

fn walk_usb<'a>(j: &'a Json, out: &mut Vec<&'a Json>) {
    for it in j.get("_items").items() {
        let dev = spk(it, &["location_id", "LocationID"]).is_some()
            || spk(it, &["vendor_id", "VendorID"]).is_some();
        if dev {
            out.push(it);
        }
        walk_usb(it, out);
    }
}

fn cameras(inv: &Inventory) -> Vec<Part> {
    inv.sp("SPCameraDataType")
        .iter()
        .filter_map(|c| {
            let name = sps(c, &["_name"])?;
            name.contains("FaceTime").then(|| {
                let mut p = Part::new("camera", Kind::Camera, name.clone());
                p.tag = "Camera".into();
                p.bind = vec!["internal".into()];
                p.opt("Model ID", sps(c, &["spcamera_model-id"]));
                p.prop("Connection", "internal");
                p
            })
        })
        .take(1)
        .collect()
}

fn audio(inv: &Inventory, vol: (Option<String>, Option<String>)) -> Vec<Part> {
    let devs: Vec<&Json> = inv
        .sp("SPAudioDataType")
        .iter()
        .flat_map(|x| x.get("_items").items())
        .filter(|d| {
            sps(d, &["coreaudio_device_transport"]).map_or(false, |t| t.contains("builtin"))
        })
        .collect();
    let mut out = Vec::new();
    if let Some(s) = devs
        .iter()
        .find(|d| !d.get("coreaudio_device_output").is_null())
    {
        let name = sps(s, &["_name"]).unwrap_or_else(|| "Speakers".into());
        for (id, side) in [("speaker:l", "left"), ("speaker:r", "right")] {
            let mut p = Part::new(id, Kind::Speaker, format!("Speaker ({side})"));
            p.tag = "Speaker".into();
            if let Some(v) = &vol.0 {
                p.metric(v.clone());
                p.prop("Volume", v);
            }
            p.prop("Channel", side);
            p.prop("Device", &name);
            p.opt(
                "Sample rate",
                sps(s, &["coreaudio_device_srate"]).map(|r| format!("{r} Hz")),
            );
            p.opt("Output source", sps(s, &["coreaudio_output_source"]));
            out.push(p);
        }
    }
    if let Some(m) = devs
        .iter()
        .find(|d| !d.get("coreaudio_device_input").is_null())
    {
        let name = sps(m, &["_name"]).unwrap_or_else(|| "Internal microphone".into());
        let mut p = Part::new("mic", Kind::Microphone, name.clone());
        p.tag = "Mic".into();
        if let Some(v) = &vol.1 {
            p.metric(v.clone());
            p.prop("Input level", v);
        }
        p.opt("Channels", sps(m, &["coreaudio_device_input"]));
        p.opt(
            "Sample rate",
            sps(m, &["coreaudio_device_srate"]).map(|r| format!("{r} Hz")),
        );
        p.opt("Input source", sps(m, &["coreaudio_input_source"]));
        out.push(p);
    }
    out
}

fn displays(inv: &Inventory) -> Vec<Part> {
    // Intel Macs expose backlight as IODisplayParameters
    let backlight = run("ioreg", &["-r", "-w0", "-c", "AppleBacklightDisplay"]).and_then(|o| {
        let objs = parse_ioreg(&o);
        let params = objs.iter().find_map(|x| x.get("IODisplayParameters"))?;
        let b = io_inline(params, "brightness")?;
        let val = io_inline(&b, "value").and_then(|v| io_num(&v))?;
        let max = io_inline(&b, "max")
            .and_then(|v| io_num(&v))
            .filter(|m| *m > 0.0)?;
        Some(val / max)
    });
    let mut out = Vec::new();
    let mut n = 0;
    for g in inv.sp("SPDisplaysDataType") {
        for d in g.get("spdisplays_ndrvs").items() {
            let name = sps(d, &["_name"]).unwrap_or_else(|| "Display".into());
            let conn = sps(d, &["spdisplays_connection_type"]).map(|c| sp_word(&c));
            let dtype = sps(d, &["spdisplays_display_type"]).unwrap_or_default();
            let internal = conn.as_deref() == Some("internal")
                || dtype.contains("built-in")
                || dtype.contains("builtin")
                || name == "Color LCD"
                || name.starts_with("Built-in");
            let vend = sps(d, &["_spdisplays_display-vendor-id"])
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
            let prod = sps(d, &["_spdisplays_display-product-id"])
                .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok());
            let edid = inv.edids.iter().filter_map(|b| parse_edid(b)).find(|e| {
                vend.map_or(false, |v| edid_vendor(v) == e.vendor)
                    && prod.map_or(true, |p| p as u16 == e.product)
            });
            let title = match &edid {
                Some(e) => edid_title(e),
                None if internal => format!("Built-in display ({name})"),
                None => name.clone(),
            };
            let id = format!("display:{n}");
            n += 1;
            let mut p = Part::new(id, Kind::Display, title);
            let ctype = conn.clone().unwrap_or_else(|| {
                if internal {
                    "eDP".into()
                } else {
                    "external".into()
                }
            });
            p.tag = if internal {
                "Screen".into()
            } else {
                ctype.clone()
            };
            p.bind = vec![if internal {
                "eDP".into()
            } else {
                ctype.clone()
            }];
            let side = if internal { "internal" } else { "external" };
            if !p.bind.iter().any(|b| b == side) {
                p.bind.push(side.into());
            }
            p.prop("Name", &name);
            p.prop(
                "Type",
                if internal {
                    "built-in panel"
                } else {
                    "external monitor"
                },
            );
            p.opt("Connection", conn);
            if !dtype.is_empty() {
                p.prop("Panel", sp_word(&dtype));
            }
            if let Some(e) = &edid {
                edid_props(&mut p, e);
            } else {
                if let Some(px) = sps(d, &["_spdisplays_pixels", "spdisplays_pixels"]) {
                    p.metric(px.replace(' ', ""));
                    p.prop("Native resolution", &px);
                }
                if let Some(v) = vend {
                    let code = edid_vendor(v);
                    p.prop("Manufacturer", format!("{} ({code})", panel_vendor(&code)));
                }
                p.opt("Product code", prod.map(|p| format!("{p:04x}")));
                p.opt("Manufactured", sps(d, &["_spdisplays_display-year"]));
            }
            if let Some(r) = sps(d, &["_spdisplays_resolution", "spdisplays_resolution"]) {
                if let Some(hz) = r.split('@').nth(1).and_then(leading_num) {
                    if edid.is_none() {
                        p.metric(format!("{hz:.0} Hz"));
                    }
                }
                p.prop("Current mode (UI)", r);
            }
            p.opt(
                "Retina",
                sps(d, &["spdisplays_retina"]).map(|v| sp_word(&v)),
            );
            p.opt(
                "Main display",
                sps(d, &["spdisplays_main"]).map(|v| sp_word(&v)),
            );
            p.opt(
                "Mirror",
                sps(d, &["spdisplays_mirror"]).map(|v| sp_word(&v)),
            );
            p.opt(
                "Ambient brightness",
                sps(d, &["spdisplays_ambient_brightness"]).map(|v| sp_word(&v)),
            );
            p.prop("GPU", sps(g, &["sppci_model", "_name"]).unwrap_or_default());
            if internal {
                if let Some(l) = backlight {
                    p.level = Some(l.clamp(0.0, 1.0));
                    p.metric(format!("{:.0}% brightness", l * 100.0));
                    p.prop("Brightness", format!("{:.0}%", l * 100.0));
                }
            }
            out.push(p);
        }
    }
    out
}

/// Third-party PCIe cards (Mac Pro, Thunderbolt enclosures).
fn pci_cards(inv: &Inventory) -> Vec<Part> {
    inv.sp("SPPCIDataType")
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let name = sps(c, &["_name", "sppci_name"])?;
            let mut p = Part::new(format!("pci:{i}"), Kind::Pci, name);
            p.tag = sps(c, &["sppci_device_type"])
                .map(|t| sp_word(&t))
                .unwrap_or_else(|| "PCI device".into());
            p.opt("Slot", sps(c, &["sppci_slot_name"]));
            p.opt("Vendor ID", sps(c, &["sppci_vendor-id"]));
            p.opt("Device ID", sps(c, &["sppci_device-id"]));
            p.opt("Link width", sps(c, &["sppci_link-width"]));
            p.opt("Link speed", sps(c, &["sppci_link-speed"]));
            p.opt("Driver", sps(c, &["sppci_driver_installed"]));
            if let Some(s) = sps(c, &["sppci_slot_name"]) {
                p.bind = vec![s];
            }
            Some(p)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioreg_battery() {
        let o = r#"+-o AppleSmartBattery  <class AppleSmartBattery, id 0x100000254, registered, matched, active, busy 0 (0 ms), retain 7>
    {
      "CurrentCapacity" = 85
      "Amperage" = 18446744073709550616
      "Voltage" = 12456
      "BatteryData" = {"Voltage"=12456,"CycleCount"=101}
      "IsCharging" = No
      "Manufacturer" = "SMP"
    }
"#;
        let v = parse_ioreg(o);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].class, "AppleSmartBattery");
        assert_eq!(v[0].num("Amperage"), Some(-1000.0));
        assert_eq!(v[0].num("IsCharging"), Some(0.0));
        assert_eq!(
            io_inline(v[0].get("BatteryData").unwrap(), "CycleCount").as_deref(),
            Some("101")
        );
        let ps = r#"{"Device Utilization %"=12,"Nested"={"a"=1,"b"=2},"Temperature(C)"=45}"#;
        assert_eq!(io_inline(ps, "Temperature(C)").as_deref(), Some("45"));
        assert_eq!(
            io_inline(ps, "Nested").as_deref(),
            Some("{\"a\"=1,\"b\"=2}")
        );
    }

    #[test]
    fn powermetrics_apple() {
        let o = "**** Processor usage ****\n\nE-Cluster HW active frequency: 1043 MHz\nCPU 0 frequency: 1150 MHz\nCPU 0 active residency:  13.45% (600 MHz: 50%)\nCPU 4 frequency: 3228 MHz\nCPU 4 active residency:  90.00% (...)\nCPU Power: 1268 mW\nANE Power: 0 mW\n\n**** GPU usage ****\n\nGPU HW active frequency: 389 MHz\nGPU HW active residency:   2.80% (389 MHz: 2.8%)\nGPU Power: 9 mW\n\n**** Thermal pressure ****\n\nCurrent pressure level: Nominal\n";
        let p = parse_powermetrics(o);
        assert_eq!(p.cpus[&4], (Some(3228.0), Some(90.0)));
        assert_eq!(p.cpu_w, Some(1.268));
        assert_eq!(p.gpu_mhz, Some(389.0));
        assert_eq!(p.gpu_w, Some(0.009));
        assert_eq!(p.pressure.as_deref(), Some("Nominal"));
        assert_eq!(p.clusters[0], ("E-Cluster".to_string(), 1043.0));
    }

    #[test]
    fn powermetrics_intel() {
        let o = "**** SMC sensors ****\n\nCPU Thermal level: 0\nFan: 1798.72 rpm\nCPU die temperature: 60.11 C\nGPU die temperature: 54.00 C\n\n**** Processor usage ****\n\nIntel energy model derived package power (CPUs+GT+SA): 2.70W\nSystem Average frequency as fraction of nominal: 69.28% (1801.36 Mhz)\nCore 0 C-state residency: 89.60% (C3: 0.00% C6: 0.00% C7: 89.60% )\nCPU 0 duty cycles/s: active/idle [< 16 us: 63.60/31.80]\nCPU Average frequency as fraction of nominal: 72.82% (1893.41 Mhz)\n";
        let p = parse_powermetrics(o);
        assert_eq!(p.cpu_temp, Some(60.11));
        assert_eq!(p.fans, vec![1798.72]);
        assert_eq!(p.pkg_w, Some(2.70));
        assert_eq!(p.avg_mhz, Some(1801.36));
        assert_eq!(p.cpus[&0].0, Some(1893.41));
        assert_eq!(p.core_idle[&0], 89.60);
    }

    #[test]
    fn hidutil() {
        let o = "Services:\nVendorID ProductID LocationID UsagePage Usage RegistryID  Transport Class                         Product                            UserClass                   Built-In \n0x5ac    0x27e     0x0        0x1       0x6   0x100000a3c SPI       AppleHIDKeyboardEventDriverV2 Apple Internal Keyboard / Trackpad (null)                       1        \n0x5ac    0x27e     0x0        0xd       0x5   0x100000a40 SPI       AppleMultitouchTrackpadHIDEventDriver Apple Internal Keyboard / Trackpad (null)                       1        \n0x46d    0xc52b    0x14100000 0x1       0x2   0x100001234 USB       IOHIDEventDriver              USB Receiver                       (null)                      0        \n";
        let v = parse_hidutil(o);
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].product, "Apple Internal Keyboard / Trackpad");
        assert!(v[0].built_in && !v[2].built_in);
        assert_eq!((v[0].usage_page, v[0].usage), (1, 6));
        assert_eq!(v[2].transport, "USB");
    }

    #[test]
    fn misc() {
        assert_eq!(
            usb_location("0x14120000 / 3"),
            Some(("20-1".to_string(), "20-1.2".to_string()))
        );
        assert_eq!(edid_vendor(0x610), "APP");
        assert_eq!(hex_ascii("0x4D3437314131"), "M471A1");
        let (page, m) = parse_vm_stat(
            "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:      1000.\nPages active:    2000.\n",
        );
        assert_eq!(page, 16384);
        assert_eq!(m["Pages active"], 2000);
        let ns = parse_netstat("Name  Mtu   Network       Address            Ipkts Ierrs     Ibytes    Opkts Oerrs     Obytes  Coll\nlo0   16384 <Link#1>                         1000     0     200000     1000     0     200000     0\nen0   1500  <Link#6>    aa:bb:cc:dd:ee:ff   5000     0   9000000     4000     0    700000     0\n");
        assert_eq!(ns["en0"], (9000000.0, 700000.0));
        assert_eq!(ns["lo0"], (200000.0, 200000.0));
    }
}
