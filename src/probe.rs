//! Linux hardware probing.
//!
//! Mostly sysfs/procfs (fast, no root needed). A few external tools fill gaps:
//! `lspci` (device names), `udevadm`/`dmidecode` (per-DIMM SMBIOS data),
//! `lsusb` (USB names when the device has no strings), `nvidia-smi` (NVIDIA GPUs).
//!
//! Set `INSIDER_SYSROOT=/some/dir` to read `/sys` and `/proc` from a captured tree.

use crate::model::*;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Instant;

// ---------------------------------------------------------------- helpers

pub(crate) fn sysroot() -> &'static str {
    static R: OnceLock<String> = OnceLock::new();
    R.get_or_init(|| {
        std::env::var("INSIDER_SYSROOT")
            .map(|s| s.trim_end_matches('/').to_string())
            .unwrap_or_default()
    })
}

/// System path, honoring INSIDER_SYSROOT.
fn sp(p: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", sysroot(), p))
}

pub fn rd(p: impl AsRef<Path>) -> Option<String> {
    let s = fs::read_to_string(p).ok()?;
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn rdn<T: std::str::FromStr>(p: impl AsRef<Path>) -> Option<T> {
    rd(p)?.parse().ok()
}

fn rdhex(p: impl AsRef<Path>) -> Option<u32> {
    let s = rd(p)?;
    u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

fn fname(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Natural ordering so hwmon10 sorts after hwmon2.
pub fn natcmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a.first(), b.first()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let la = a.iter().take_while(|c| c.is_ascii_digit()).count();
                let lb = b.iter().take_while(|c| c.is_ascii_digit()).count();
                let na: u128 = std::str::from_utf8(&a[..la.min(30)])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                let nb: u128 = std::str::from_utf8(&b[..lb.min(30)])
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                if na != nb {
                    return na.cmp(&nb);
                }
                a = &a[la..];
                b = &b[lb..];
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(y);
                }
                a = &a[1..];
                b = &b[1..];
            }
        }
    }
}

fn ls(dir: impl AsRef<Path>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|r| r.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| natcmp(&fname(a), &fname(b)));
    v
}

fn canon(p: impl AsRef<Path>) -> Option<PathBuf> {
    fs::canonicalize(p).ok()
}

fn link_base(p: impl AsRef<Path>) -> Option<String> {
    fs::read_link(p).ok().map(|l| fname(&l))
}

/// File name a command's output is recorded under: the tool plus its
/// arguments, e.g. `ioreg_-rn_AppleSmartBattery`.
pub(crate) fn cmd_key(cmd: &str, args: &[&str]) -> String {
    let mut k = String::from(cmd);
    for a in args {
        k.push('_');
        k.push_str(a);
    }
    let k: String = k
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-,=".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    k.chars().take(120).collect()
}

/// `INSIDER_CAPTURE=/dir`: also write every tool's output to
/// `/dir/cmd/<key>`, so a machine can be replayed elsewhere with
/// `INSIDER_SYSROOT=/dir` (and `INSIDER_PLATFORM=...`).
pub(crate) fn capture(key: &str, out: &str) {
    if let Ok(dir) = std::env::var("INSIDER_CAPTURE") {
        let d = PathBuf::from(dir).join("cmd");
        let _ = fs::create_dir_all(&d);
        let _ = fs::write(d.join(key), out);
    }
}

/// Recorded output of a command when replaying a captured tree: the
/// argument-specific file if there is one, else `cmd/<tool>`.
pub(crate) fn replay(cmd: &str, args: &[&str]) -> Option<String> {
    let dir = format!("{}/cmd", sysroot());
    fs::read_to_string(format!("{dir}/{}", cmd_key(cmd, args)))
        .or_else(|_| fs::read_to_string(format!("{dir}/{cmd}")))
        .ok()
}

pub(crate) fn run(cmd: &str, args: &[&str]) -> Option<String> {
    if !sysroot().is_empty() {
        // Replaying a captured tree: use recorded output, never the live system.
        return replay(cmd, args);
    }
    let out = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    capture(&cmd_key(cmd, args), &s);
    Some(s)
}

/// Drop firmware placeholder strings.
pub(crate) fn clean(s: Option<String>) -> Option<String> {
    let s = s?;
    let t = s.trim();
    let l = t.to_ascii_lowercase();
    const JUNK: [&str; 12] = [
        "unknown",
        "not specified",
        "to be filled by o.e.m.",
        "default string",
        "none",
        "n/a",
        "not available",
        "not provided",
        "undefined",
        "no module installed",
        "[n/a]",
        "[not supported]",
    ];
    if t.is_empty() || JUNK.contains(&l.as_str()) || t.chars().all(|c| c == '0' || c == ' ') {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn join(v: &[Option<String>]) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for s in v.iter().flatten() {
        // avoid "Razer Razer Blade"
        if let Some(o) = out.iter_mut().find(|o| s.starts_with(**o)) {
            *o = s.as_str();
            continue;
        }
        if out.iter().any(|o| o.contains(s.as_str())) {
            continue;
        }
        out.push(s.as_str());
    }
    if out.is_empty() {
        None
    } else {
        Some(out.join(" "))
    }
}

fn is_root() -> bool {
    rd("/proc/self/status")
        .map(|s| {
            s.lines()
                .any(|l| l.starts_with("Uid:") && l.split_whitespace().nth(1) == Some("0"))
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------- hwmon / thermal

struct Hwmon {
    id: String,
    name: String,
    /// Canonical path of the hwmon dir itself.
    real: Option<PathBuf>,
    /// Canonical path of the device it belongs to.
    dev: Option<PathBuf>,
    temps: Vec<(String, f64)>,
    fans: Vec<(String, u32)>,
}

fn scan_hwmon() -> Vec<Hwmon> {
    ls(sp("/sys/class/hwmon"))
        .into_iter()
        .map(|p| {
            let mut temps = Vec::new();
            let mut fans = Vec::new();
            for f in ls(&p) {
                let n = fname(&f);
                if let Some(idx) = n
                    .strip_prefix("temp")
                    .and_then(|r| r.strip_suffix("_input"))
                {
                    if let Some(v) = rdn::<f64>(&f) {
                        let v = v / 1000.0;
                        if v > -40.0 && v < 200.0 {
                            let label = rd(p.join(format!("temp{idx}_label")))
                                .unwrap_or_else(|| format!("temp{idx}"));
                            temps.push((label, v));
                        }
                    }
                } else if let Some(idx) =
                    n.strip_prefix("fan").and_then(|r| r.strip_suffix("_input"))
                {
                    if let Some(v) = rdn::<u32>(&f) {
                        let label = rd(p.join(format!("fan{idx}_label")))
                            .unwrap_or_else(|| format!("fan{idx}"));
                        fans.push((label, v));
                    }
                }
            }
            Hwmon {
                id: fname(&p),
                name: rd(p.join("name")).unwrap_or_else(|| "?".into()),
                real: canon(&p),
                dev: canon(p.join("device")),
                temps,
                fans,
            }
        })
        .collect()
}

fn main_temp(h: &Hwmon) -> Option<f64> {
    const PREF: [&str; 6] = ["package", "tctl", "tdie", "composite", "edge", "cpu"];
    for p in PREF {
        if let Some((_, t)) = h
            .temps
            .iter()
            .find(|(l, _)| l.to_ascii_lowercase().starts_with(p))
        {
            return Some(*t);
        }
    }
    h.temps.first().map(|t| t.1)
}

fn thermal_zones() -> Vec<(String, f64)> {
    ls(sp("/sys/class/thermal"))
        .into_iter()
        .filter(|p| fname(p).starts_with("thermal_zone"))
        .filter_map(|p| {
            let t = rdn::<f64>(p.join("temp"))? / 1000.0;
            if t <= -40.0 || t > 200.0 {
                return None;
            }
            Some((rd(p.join("type")).unwrap_or_else(|| fname(&p)), t))
        })
        .collect()
}

// ---------------------------------------------------------------- PCI

struct PciDev {
    slot: String,
    path: PathBuf,
    class: u32,
    vendor: u32,
    device: u32,
    driver: Option<String>,
}

fn scan_pci() -> Vec<PciDev> {
    ls(sp("/sys/bus/pci/devices"))
        .into_iter()
        .map(|p| PciDev {
            slot: fname(&p),
            class: rdhex(p.join("class")).unwrap_or(0),
            vendor: rdhex(p.join("vendor")).unwrap_or(0),
            device: rdhex(p.join("device")).unwrap_or(0),
            driver: link_base(p.join("driver")),
            path: canon(&p).unwrap_or(p),
        })
        .collect()
}

struct PciName {
    class: String,
    vendor: String,
    device: String,
}

fn parse_lspci(out: &str) -> HashMap<String, PciName> {
    let mut m = HashMap::new();
    for line in out.lines() {
        let Some(slot) = line.split_whitespace().next() else {
            continue;
        };
        let f: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
        if f.len() >= 3 {
            m.insert(
                slot.to_string(),
                PciName {
                    class: f[0].to_string(),
                    vendor: f[1].to_string(),
                    device: f[2].to_string(),
                },
            );
        }
    }
    m
}

pub(crate) fn short_vendor(v: &str) -> String {
    if let (Some(a), Some(b)) = (v.find('['), v.rfind(']')) {
        if a < b {
            let inner = &v[a + 1..b];
            return inner.split('/').next().unwrap_or(inner).to_string();
        }
    }
    let mut s = v.to_string();
    for suf in [
        " Corporation",
        " Corp.",
        ", Inc.",
        " Inc.",
        " Co., Ltd.",
        " Co., Ltd",
        " Co Ltd",
        " Technology",
        " Semiconductor",
        " Electronics",
    ] {
        if let Some(i) = s.find(suf) {
            s.truncate(i);
        }
    }
    s.trim().to_string()
}

pub(crate) fn nice_device(d: &str) -> String {
    if let (Some(a), Some(b)) = (d.rfind('['), d.rfind(']')) {
        if a < b && b - a > 3 {
            return d[a + 1..b].to_string();
        }
    }
    d.to_string()
}

fn pci_class_fallback(class: u32) -> &'static str {
    match class >> 16 {
        0x01 => "Storage controller",
        0x02 => "Network controller",
        0x03 => "Display controller",
        0x04 => "Multimedia controller",
        0x05 => "Memory controller",
        0x06 => "Bridge",
        0x07 => "Communication controller",
        0x08 => "System peripheral",
        0x0c => "Serial bus controller",
        0x0d => "Wireless controller",
        0x11 => "Signal processing controller",
        _ => "PCI device",
    }
}

// ---------------------------------------------------------------- memory (SMBIOS / SPD)

#[derive(Default, Clone)]
struct MemDev {
    present: bool,
    size: Option<u64>,
    locator: Option<String>,
    bank: Option<String>,
    mtype: Option<String>,
    type_detail: Option<String>,
    form: Option<String>,
    speed: Option<u32>,
    cfg_speed: Option<u32>,
    manufacturer: Option<String>,
    part_no: Option<String>,
    serial: Option<String>,
    rank: Option<String>,
    voltage_mv: Option<u32>,
    width: Option<String>,
}

pub(crate) fn leading_num(s: &str) -> Option<f64> {
    let n: String = s
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    n.parse().ok()
}

pub(crate) fn parse_size(s: &str) -> Option<u64> {
    let n = leading_num(s)?;
    let mul: u64 = if s.contains("TB") {
        1 << 40
    } else if s.contains("GB") {
        1 << 30
    } else if s.contains("MB") {
        1 << 20
    } else if s.contains("kB") || s.contains("KB") {
        1 << 10
    } else {
        1
    };
    Some((n * mul as f64) as u64)
}

fn memdevs_from_udev(out: &str) -> Vec<MemDev> {
    let mut map: BTreeMap<usize, HashMap<String, String>> = BTreeMap::new();
    for l in out.lines() {
        let Some(rest) = l.strip_prefix("MEMORY_DEVICE_") else {
            continue;
        };
        let Some((k, v)) = rest.split_once('=') else {
            continue;
        };
        let Some((idx, field)) = k.split_once('_') else {
            continue;
        };
        let Ok(idx) = idx.parse::<usize>() else {
            continue;
        };
        map.entry(idx)
            .or_default()
            .insert(field.to_string(), v.trim().to_string());
    }
    map.into_values()
        .map(|m| {
            let g = |k: &str| clean(m.get(k).cloned());
            let size = g("SIZE")
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|s| *s > 0);
            MemDev {
                present: g("PRESENT").map(|v| v != "0").unwrap_or(size.is_some()) && size.is_some(),
                size,
                locator: g("LOCATOR"),
                bank: g("BANK_LOCATOR"),
                mtype: g("TYPE"),
                type_detail: g("TYPE_DETAIL"),
                form: g("FORM_FACTOR"),
                speed: g("SPEED_MTS")
                    .and_then(|s| s.parse().ok())
                    .filter(|v| *v > 0),
                cfg_speed: g("CONFIGURED_SPEED_MTS")
                    .and_then(|s| s.parse().ok())
                    .filter(|v| *v > 0),
                manufacturer: g("MANUFACTURER"),
                part_no: g("PART_NUMBER"),
                serial: g("SERIAL_NUMBER"),
                rank: g("RANK"),
                voltage_mv: g("CONFIGURED_VOLTAGE")
                    .and_then(|s| s.parse().ok())
                    .filter(|v| *v > 0),
                width: g("TOTAL_WIDTH").map(|w| format!("{w} bit")),
            }
        })
        .collect()
}

fn memdevs_from_dmidecode(out: &str) -> Vec<MemDev> {
    let mut v = Vec::new();
    for block in out.split("\n\n") {
        if !block.contains("Memory Device") {
            continue;
        }
        let mut m: HashMap<&str, &str> = HashMap::new();
        for l in block.lines() {
            if let Some((k, val)) = l.trim().split_once(": ") {
                m.insert(k.trim(), val.trim());
            }
        }
        let g = |k: &str| clean(m.get(k).map(|s| s.to_string()));
        let size = m
            .get("Size")
            .filter(|s| !s.contains("No Module"))
            .and_then(|s| parse_size(s))
            .filter(|s| *s > 0);
        v.push(MemDev {
            present: size.is_some(),
            size,
            locator: g("Locator"),
            bank: g("Bank Locator"),
            mtype: g("Type"),
            type_detail: g("Type Detail"),
            form: g("Form Factor"),
            speed: m
                .get("Speed")
                .and_then(|s| leading_num(s))
                .map(|f| f as u32)
                .filter(|v| *v > 0),
            cfg_speed: m
                .get("Configured Memory Speed")
                .or(m.get("Configured Clock Speed"))
                .and_then(|s| leading_num(s))
                .map(|f| f as u32)
                .filter(|v| *v > 0),
            manufacturer: g("Manufacturer"),
            part_no: g("Part Number"),
            serial: g("Serial Number"),
            rank: g("Rank"),
            voltage_mv: m
                .get("Configured Voltage")
                .and_then(|s| leading_num(s))
                .map(|f| (f * 1000.0).round() as u32)
                .filter(|v| *v > 0),
            width: g("Total Width"),
        });
    }
    v
}

struct Spd {
    gen: &'static str,
    tck: i32,
    taa: i32,
    trcd: i32,
    trp: i32,
    part: Option<String>,
}

fn ascii(b: &[u8]) -> Option<String> {
    clean(Some(
        b.iter()
            .map(|&c| {
                if (0x20..0x7f).contains(&c) {
                    c as char
                } else {
                    ' '
                }
            })
            .collect(),
    ))
}

fn parse_spd(b: &[u8]) -> Option<Spd> {
    let s = match *b.get(2)? {
        0x0C if b.len() >= 350 => {
            let fine = |i: usize| b[i] as i8 as i32;
            let mtb = |i: usize| b[i] as i32 * 125;
            Spd {
                gen: "DDR4",
                tck: mtb(18) + fine(125),
                taa: mtb(24) + fine(123),
                trcd: mtb(25) + fine(122),
                trp: mtb(26) + fine(121),
                part: ascii(&b[329..349]),
            }
        }
        0x12 if b.len() >= 552 => {
            let w = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]) as i32;
            Spd {
                gen: "DDR5",
                tck: w(20),
                taa: w(30),
                trcd: w(32),
                trp: w(34),
                part: ascii(&b[521..551]),
            }
        }
        _ => return None,
    };
    (s.tck > 0).then_some(s)
}

/// Clock cycles needed for a timing at a given clock period (JEDEC-style rounding).
fn nck(t_ps: i32, tck_ps: f64) -> u32 {
    ((t_ps as f64 / tck_ps) - 0.025).ceil().max(1.0) as u32
}

// ---------------------------------------------------------------- the prober

pub struct Options {
    pub nvidia_smi: bool,
}

pub struct Prober {
    opts: Options,
    pci_names: HashMap<String, PciName>,
    usb_names: HashMap<String, Option<String>>,
    mem: Vec<MemDev>,
    mem_source: String,
    spd: Vec<Spd>,
    notes: Vec<String>,
    prev: HashMap<String, (f64, Instant)>,
    prev_cpu: HashMap<String, (u64, u64)>,
    nvidia: HashMap<String, HashMap<String, String>>,
    nvidia_at: Option<Instant>,
}

const NV_FIELDS: [&str; 12] = [
    "pci.bus_id",
    "name",
    "temperature.gpu",
    "utilization.gpu",
    "memory.used",
    "memory.total",
    "clocks.gr",
    "clocks.mem",
    "power.draw",
    "pstate",
    "fan.speed",
    "driver_version",
];

impl Prober {
    pub fn new(opts: Options) -> Self {
        let mut notes = Vec::new();
        let pci_names = match run("lspci", &["-D", "-mm"]) {
            Some(o) => parse_lspci(&o),
            None => {
                if sysroot().is_empty() {
                    notes.push(
                        "lspci not found: PCI devices shown by ID (apt install pciutils)".into(),
                    );
                }
                HashMap::new()
            }
        };

        let mut mem = run(
            "udevadm",
            &[
                "info",
                "--query=property",
                "--path=/sys/devices/virtual/dmi/id",
            ],
        )
        .map(|o| memdevs_from_udev(&o))
        .unwrap_or_default();
        let mut mem_source = "SMBIOS (via udev)".to_string();
        if mem.is_empty() {
            mem = run("dmidecode", &["-t", "17"])
                .map(|o| memdevs_from_dmidecode(&o))
                .unwrap_or_default();
            mem_source = "SMBIOS (via dmidecode)".into();
        }
        if mem.is_empty() {
            notes.push(
                "Per-DIMM details unavailable: needs systemd-udev's dmi_memory_id, or run with sudo (dmidecode)"
                    .into(),
            );
        }

        // SPD EEPROMs (CAS latency etc.)
        let mut spd = Vec::new();
        let mut paths = Vec::new();
        for drv in ["ee1004", "spd5118"] {
            for d in ls(sp(&format!("/sys/bus/i2c/drivers/{drv}"))) {
                if fname(&d).contains('-') && d.join("eeprom").exists() {
                    paths.push(d.join("eeprom"));
                }
            }
        }
        if paths.is_empty() {
            notes.push(
                "No SPD EEPROMs visible. For CAS latency: `sudo modprobe ee1004` (DDR4) or `spd5118` (DDR5), then run with sudo"
                    .into(),
            );
        } else {
            let mut denied = false;
            for p in &paths {
                match fs::read(p) {
                    Ok(b) => {
                        if let Some(s) = parse_spd(&b) {
                            spd.push(s)
                        }
                    }
                    Err(_) => denied = true,
                }
            }
            if denied && spd.is_empty() {
                notes.push("SPD EEPROMs found but need root: run with sudo for RAM timings".into());
            }
        }
        if !is_root() {
            notes.push(
                "Not running as root: some details (SPD timings, CPU package power) are hidden"
                    .into(),
            );
        }

        Prober {
            opts,
            pci_names,
            usb_names: HashMap::new(),
            mem,
            mem_source,
            spd,
            notes,
            prev: HashMap::new(),
            prev_cpu: HashMap::new(),
            nvidia: HashMap::new(),
            nvidia_at: None,
        }
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

    fn pci_title(&self, d: &PciDev) -> String {
        match self.pci_names.get(&d.slot) {
            Some(n) => format!("{} {}", short_vendor(&n.vendor), nice_device(&n.device)),
            None => format!("PCI device {:04x}:{:04x}", d.vendor, d.device),
        }
    }

    fn pci_class(&self, d: &PciDev) -> String {
        self.pci_names
            .get(&d.slot)
            .map(|n| n.class.clone())
            .unwrap_or_else(|| pci_class_fallback(d.class).to_string())
    }

    fn pci_props(&self, p: &mut Part, d: &PciDev) {
        p.prop("PCI address", &d.slot);
        p.prop("PCI ID", format!("{:04x}:{:04x}", d.vendor, d.device));
        p.prop("Class", self.pci_class(d));
        p.opt("Driver", d.driver.clone());
        let ls_ = rd(d.path.join("current_link_speed")).map(|s| s.replace(" PCIe", ""));
        let lw = rd(d.path.join("current_link_width"));
        let ms = rd(d.path.join("max_link_speed")).map(|s| s.replace(" PCIe", ""));
        let mw = rd(d.path.join("max_link_width"));
        if let (Some(s), Some(w)) = (ls_, lw) {
            if !s.contains("Unknown") {
                let max = match (ms, mw) {
                    (Some(a), Some(b)) => format!(" (max {a} x{b})"),
                    _ => String::new(),
                };
                p.prop("PCIe link", format!("{s} x{w}{max}"));
            }
        }
    }

    pub fn gather(&mut self) -> Snapshot {
        let hw = scan_hwmon();
        let tz = thermal_zones();
        let pci = scan_pci();
        let dmi = read_dmi();
        let mut claimed: HashSet<String> = HashSet::new();
        let mut parts = Vec::new();
        parts.push(self.board(&dmi, &tz));
        parts.extend(self.cpus(&hw, &tz, &pci, &mut claimed));
        parts.extend(self.gpus(&hw, &pci, &mut claimed));
        parts.extend(self.dimms(&hw));
        parts.extend(self.disks(&hw, &mut claimed));
        parts.extend(self.nets(&hw, &pci, &mut claimed));
        if let Some(p) = self.chipset(&hw, &tz, &pci, &mut claimed) {
            parts.push(p);
        }
        parts.push(bios(&dmi));
        parts.extend(self.batteries());
        parts.extend(fans(&hw));
        let (inputs, built_in) = inputs(&dmi);
        parts.extend(inputs);
        parts.extend(self.usb(&built_in));
        parts.extend(audio());
        parts.extend(displays());
        parts.extend(self.other_pci(&pci, &claimed));
        Snapshot {
            parts,
            dmi,
            notes: self.notes.clone(),
        }
    }

    // ------------------------------------------------------------ board

    fn board(&mut self, dmi: &HashMap<String, String>, tz: &[(String, f64)]) -> Part {
        let g = |k: &str| dmi.get(k).cloned();
        let title = join(&[g("sys_vendor"), g("product_name")])
            .or_else(|| join(&[g("board_vendor"), g("board_name")]))
            .unwrap_or_else(|| "Mainboard".into());
        let mut p = Part::new("board", Kind::Board, title);
        p.tag = "Mainboard".into();
        p.opt("System vendor", g("sys_vendor"));
        p.opt("Product", g("product_name"));
        p.opt("Product version", g("product_version"));
        p.opt("Family", g("product_family"));
        p.opt("SKU", g("product_sku"));
        p.opt("Board vendor", g("board_vendor"));
        p.opt("Board name", g("board_name"));
        p.opt("Board version", g("board_version"));
        if let Some(c) = g("chassis_type").and_then(|s| s.parse::<u32>().ok()) {
            p.prop("Chassis", format!("{} ({c})", chassis_name(c)));
        }
        p.opt("Kernel", rd(sp("/proc/sys/kernel/osrelease")));
        p.opt("Hostname", rd(sp("/proc/sys/kernel/hostname")));
        if let Some(up) = rd(sp("/proc/uptime")).and_then(|s| leading_num(&s)) {
            p.prop("Uptime", fmt_hours(up / 3600.0));
        }
        if let Some(l) = rd(sp("/proc/loadavg")) {
            p.prop(
                "Load average",
                l.split_whitespace().take(3).collect::<Vec<_>>().join(" "),
            );
        }
        if let Some(mi) = rd(sp("/proc/meminfo")) {
            let kb = |k: &str| {
                mi.lines()
                    .find(|l| l.starts_with(k))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|v| v * 1024)
            };
            if let (Some(t), Some(a)) = (kb("MemTotal:"), kb("MemAvailable:")) {
                p.prop(
                    "Memory in use",
                    format!("{} of {}", fmt_bin(t - a.min(t)), fmt_bin(t)),
                );
            }
        }
        for (t, v) in tz {
            p.prop(&format!("Thermal zone {t}"), fmt_temp(*v));
        }
        p.temp = tz.iter().find(|(t, _)| t == "acpitz").map(|x| x.1);
        p
    }

    // ------------------------------------------------------------ CPU

    /// Utilization since last call: overall ("cpu") and per logical CPU ("cpuN").
    fn cpu_util(&mut self) -> HashMap<String, f64> {
        let mut out = HashMap::new();
        let Some(s) = rd(sp("/proc/stat")) else {
            return out;
        };
        for l in s.lines().filter(|l| l.starts_with("cpu")) {
            let mut it = l.split_whitespace();
            let Some(name) = it.next() else { continue };
            let v: Vec<u64> = it.filter_map(|x| x.parse().ok()).collect();
            if v.len() < 4 {
                continue;
            }
            let total: u64 = v.iter().take(8).sum();
            let idle = v[3] + v.get(4).copied().unwrap_or(0);
            if let Some((pt, pi)) = self.prev_cpu.get(name) {
                let dt = total.saturating_sub(*pt);
                let di = idle.saturating_sub(*pi).min(dt);
                if dt > 0 {
                    out.insert(name.to_string(), 100.0 * (dt - di) as f64 / dt as f64);
                }
            }
            self.prev_cpu.insert(name.to_string(), (total, idle));
        }
        out
    }

    fn cpus(
        &mut self,
        hw: &[Hwmon],
        tz: &[(String, f64)],
        pci: &[PciDev],
        claimed: &mut HashSet<String>,
    ) -> Vec<Part> {
        let info = rd(sp("/proc/cpuinfo")).unwrap_or_default();
        let mut sockets: BTreeMap<String, Vec<HashMap<String, String>>> = BTreeMap::new();
        for block in info.split("\n\n") {
            let mut m = HashMap::new();
            for l in block.lines() {
                if let Some((k, v)) = l.split_once(':') {
                    m.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            if m.contains_key("processor") {
                let id = m.get("physical id").cloned().unwrap_or_else(|| "0".into());
                sockets.entry(id).or_default().push(m);
            }
        }
        if sockets.is_empty() {
            sockets.insert("0".into(), Vec::new());
        }
        let utils = self.cpu_util();
        let util = utils.get("cpu").copied();

        // logical CPUs: (package, core id, ghz, load)
        let mut freqs: HashMap<String, Vec<f64>> = HashMap::new();
        let mut lcpus: Vec<(String, u32, Option<f64>, Option<f64>)> = Vec::new();
        for c in ls(sp("/sys/devices/system/cpu")) {
            let n = fname(&c);
            let Some(num) = n.strip_prefix("cpu") else {
                continue;
            };
            if num.is_empty() || !num.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            let pkg = rd(c.join("topology/physical_package_id")).unwrap_or_else(|| "0".into());
            let ghz = rdn::<f64>(c.join("cpufreq/scaling_cur_freq")).map(|f| f / 1e6);
            if let Some(f) = ghz {
                freqs.entry(pkg.clone()).or_default().push(f);
            }
            if let Some(core) = rdn::<u32>(c.join("topology/core_id")) {
                lcpus.push((pkg, core, ghz, utils.get(&n).copied()));
            }
        }
        let cpu_hw: Vec<&Hwmon> = hw
            .iter()
            .filter(|h| {
                ["coretemp", "k10temp", "zenpower", "cpu_thermal"].contains(&h.name.as_str())
            })
            .collect();
        let cpufreq0 = sp("/sys/devices/system/cpu/cpu0/cpufreq");

        let mut out = Vec::new();
        for (i, (pid, procs)) in sockets.iter().enumerate() {
            let first = procs.first();
            let get = |k: &str| first.and_then(|m| m.get(k)).cloned();
            let model = get("model name")
                .or_else(|| get("Hardware"))
                .or_else(|| get("cpu model"))
                .unwrap_or_else(|| "CPU".into());
            let mut p = Part::new(format!("cpu{i}"), Kind::Cpu, clean_cpu_name(&model));
            p.tag = "CPU".into();
            p.bind = vec![format!("socket{i}"), pid.clone()];
            let threads = procs.len();
            let cores = procs
                .iter()
                .filter_map(|m| m.get("core id"))
                .collect::<HashSet<_>>()
                .len();
            p.prop("Model", &model);
            if threads > 0 {
                p.prop(
                    "Cores / threads",
                    format!("{} / {}", if cores > 0 { cores } else { threads }, threads),
                );
            }
            p.opt("Vendor", get("vendor_id"));
            if let (Some(f), Some(m), Some(s)) = (get("cpu family"), get("model"), get("stepping"))
            {
                p.prop("Family / model / stepping", format!("{f} / {m} / {s}"));
            }
            p.opt("Microcode", get("microcode"));
            p.opt("Cache", get("cache size"));
            if get("flags").map_or(false, |f| f.split(' ').any(|x| x == "hypervisor")) {
                p.prop("Virtualized", "yes (running under a hypervisor)");
            }

            let mut fs_: Vec<f64> = freqs.get(pid).cloned().unwrap_or_default();
            if fs_.is_empty() {
                fs_ = procs
                    .iter()
                    .filter_map(|m| m.get("cpu MHz").and_then(|v| v.parse::<f64>().ok()))
                    .map(|mhz| mhz / 1000.0)
                    .collect();
            }
            if !fs_.is_empty() {
                let avg = fs_.iter().sum::<f64>() / fs_.len() as f64;
                let max = fs_.iter().cloned().fold(0.0, f64::max);
                p.metric(format!("{avg:.2} GHz"));
                p.prop(
                    "Clock (avg / fastest core)",
                    format!("{avg:.2} / {max:.2} GHz"),
                );
            }
            if let (Some(lo), Some(hi)) = (
                rdn::<f64>(cpufreq0.join("cpuinfo_min_freq")),
                rdn::<f64>(cpufreq0.join("cpuinfo_max_freq")),
            ) {
                p.prop(
                    "Clock range",
                    format!("{:.2} – {:.2} GHz", lo / 1e6, hi / 1e6),
                );
            }
            p.opt("Frequency driver", rd(cpufreq0.join("scaling_driver")));
            p.opt("Governor", rd(cpufreq0.join("scaling_governor")));
            p.opt(
                "Energy preference",
                rd(cpufreq0.join("energy_performance_preference")),
            );
            if let Some(u) = util {
                p.metric(format!("{u:.0}% load"));
                p.prop("Utilization", format!("{u:.1}%"));
            }

            // physical cores, with per-core temperature/clock/load
            let mut by_core: BTreeMap<u32, Vec<(Option<f64>, Option<f64>)>> = BTreeMap::new();
            for (pkg, core, ghz, load) in &lcpus {
                if pkg == pid {
                    by_core.entry(*core).or_default().push((*ghz, *load));
                }
            }
            let core_temp = |id: u32| {
                cpu_hw.get(i).and_then(|h| {
                    h.temps
                        .iter()
                        .find(|(l, _)| l == &format!("Core {id}"))
                        .map(|(_, t)| *t)
                })
            };
            p.cores = by_core
                .into_iter()
                .map(|(id, v)| {
                    let ghz = v
                        .iter()
                        .filter_map(|x| x.0)
                        .fold(None, |a: Option<f64>, b| Some(a.map_or(b, |a| a.max(b))));
                    let loads: Vec<f64> = v.iter().filter_map(|x| x.1).collect();
                    let load =
                        (!loads.is_empty()).then(|| loads.iter().sum::<f64>() / loads.len() as f64);
                    Core {
                        id,
                        temp: core_temp(id),
                        ghz,
                        load,
                    }
                })
                .collect();

            if let Some(h) = cpu_hw.get(i) {
                p.temp = main_temp(h);
                let cores: Vec<String> = h
                    .temps
                    .iter()
                    .filter(|(l, _)| l.starts_with("Core"))
                    .map(|(_, t)| format!("{t:.0}"))
                    .collect();
                if !cores.is_empty() {
                    p.prop("Core temps (°C)", cores.join(" "));
                }
                p.prop("Sensor", &h.name);
            } else if let Some((n, t)) = tz.iter().find(|(t, _)| {
                ["x86_pkg_temp", "TCPU", "cpu-thermal", "cpu_thermal"].contains(&t.as_str())
            }) {
                p.temp = Some(*t);
                p.prop("Sensor", format!("thermal zone {n}"));
            }

            if i == 0 {
                // Package power (RAPL). Readable by root only on modern kernels.
                if let Some(uj) = rdn::<f64>(sp("/sys/class/powercap/intel-rapl:0/energy_uj")) {
                    if let Some(r) = self.rate("rapl0", uj) {
                        p.metric(format!("{:.1} W", r / 1e6));
                        p.prop("Package power", format!("{:.2} W", r / 1e6));
                    }
                }
                // Intel integrated graphics live inside the CPU package.
                if let Some(ig) = pci.iter().find(|d| {
                    d.class >> 8 == 0x0300 && d.vendor == 0x8086 && d.slot.starts_with("0000:00:")
                }) {
                    claimed.insert(ig.slot.clone());
                    p.prop("Integrated GPU", self.pci_title(ig));
                    p.prop("iGPU PCI address", &ig.slot);
                    p.opt("iGPU driver", ig.driver.clone());
                    for c in ls(sp("/sys/class/drm")) {
                        let n = fname(&c);
                        if !n.starts_with("card") || n.contains('-') {
                            continue;
                        }
                        if canon(c.join("device")).as_ref() != Some(&ig.path) {
                            continue;
                        }
                        let cur =
                            rd(c.join("gt_act_freq_mhz")).or_else(|| rd(c.join("gt_cur_freq_mhz")));
                        let max = rd(c.join("gt_max_freq_mhz"));
                        if let Some(cur) = cur {
                            p.prop(
                                "iGPU clock",
                                format!(
                                    "{cur} MHz{}",
                                    max.map(|m| format!(" (max {m})")).unwrap_or_default()
                                ),
                            );
                        }
                    }
                }
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ GPU

    fn gpus(&mut self, hw: &[Hwmon], pci: &[PciDev], claimed: &mut HashSet<String>) -> Vec<Part> {
        let awake_nv = pci.iter().any(|d| {
            d.vendor == 0x10de
                && d.class >> 16 == 0x03
                && rd(d.path.join("power/runtime_status")).as_deref() != Some("suspended")
        });
        if self.opts.nvidia_smi
            && awake_nv
            && self.nvidia_at.map_or(true, |t| t.elapsed().as_secs() >= 4)
        {
            self.nvidia = query_nvidia();
            self.nvidia_at = Some(Instant::now());
        }

        let mut out = Vec::new();
        let gpus: Vec<&PciDev> = pci
            .iter()
            .filter(|d| d.class >> 16 == 0x03 && !claimed.contains(&d.slot))
            .collect();
        for d in gpus {
            claimed.insert(d.slot.clone());
            let mut p = Part::new(format!("gpu:{}", d.slot), Kind::Gpu, self.pci_title(d));
            p.tag = "GPU".into();
            p.bind = vec![d.slot.clone()];
            self.pci_props(&mut p, d);
            let rt = rd(d.path.join("power/runtime_status"));
            p.opt("Runtime PM", rt.clone());
            p.opt("Power state", rd(d.path.join("power_state")));
            if rd(d.path.join("boot_vga")).as_deref() == Some("1") {
                p.prop("Boot VGA", "yes");
            }
            let asleep = rt.as_deref() == Some("suspended");
            if asleep {
                p.metric("asleep (D3)");
                p.prop(
                    "Note",
                    "GPU is runtime-suspended; not polled so it can stay asleep",
                );
            } else if d.vendor == 0x10de {
                let key = slot_suffix(&d.slot);
                if let Some(nv) = self.nvidia.get(&key) {
                    nvidia_props(&mut p, nv);
                } else if !self.opts.nvidia_smi {
                    p.prop("Note", "nvidia-smi polling disabled (--no-nvidia-smi)");
                }
            }
            if !asleep {
                if let Some(h) = hw.iter().find(|h| h.dev.as_ref() == Some(&d.path)) {
                    if p.temp.is_none() {
                        p.temp = main_temp(h);
                    }
                    p.prop("Sensor", &h.name);
                }
                // amdgpu exposes VRAM in sysfs
                if let (Some(u), Some(t)) = (
                    rdn::<u64>(d.path.join("mem_info_vram_used")),
                    rdn::<u64>(d.path.join("mem_info_vram_total")),
                ) {
                    p.prop("VRAM", format!("{} / {}", fmt_bin(u), fmt_bin(t)));
                }
                if let Some(b) = rdn::<f64>(d.path.join("gpu_busy_percent")) {
                    p.metric(format!("{b:.0}%"));
                }
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ RAM

    fn dimms(&mut self, hw: &[Hwmon]) -> Vec<Part> {
        let temps: Vec<f64> = hw
            .iter()
            .filter(|h| h.name == "jc42" || h.name == "spd5118")
            .filter_map(main_temp)
            .collect();
        let mut out = Vec::new();
        if self.mem.is_empty() {
            let total = rd(sp("/proc/meminfo")).and_then(|mi| {
                mi.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
            });
            let mut p = Part::new("dimm0", Kind::Dimm, "System memory");
            p.tag = total
                .map(|t| fmt_bin(t * 1024))
                .unwrap_or_else(|| "RAM".into());
            p.metric(p.tag.clone());
            p.prop(
                "Note",
                "Per-module details unavailable (see notes on the board panel)",
            );
            out.push(p);
            return out;
        }
        let mem_use = rd(sp("/proc/meminfo")).and_then(|mi| {
            let kb = |k: &str| {
                mi.lines()
                    .find(|l| l.starts_with(k))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|v| v * 1024)
            };
            let (t, a) = (kb("MemTotal:")?, kb("MemAvailable:")?);
            Some((t - a.min(t), t))
        });
        let mut k = 0usize; // index among present modules
        for (i, m) in self.mem.iter().enumerate() {
            let mut p = Part::new(format!("dimm{i}"), Kind::Dimm, "Empty memory slot");
            p.bind = [
                m.locator.clone(),
                m.bank.clone(),
                match (&m.bank, &m.locator) {
                    (Some(b), Some(l)) => Some(format!("{b}/{l}")),
                    _ => None,
                },
            ]
            .into_iter()
            .flatten()
            .collect();
            p.opt("Locator", m.locator.clone());
            p.opt("Bank", m.bank.clone());
            p.form = m.form.clone().map(|f| {
                let u = f.to_uppercase();
                if u.contains("CAMM") {
                    "CAMM2".to_string()
                } else {
                    u
                }
            });
            if !m.present {
                p.present = false;
                p.tag = "empty".into();
                out.push(p);
                continue;
            }
            let size = m.size.map(fmt_bin);
            let short = m
                .size
                .map(|s| format!("{}G", (s as f64 / (1u64 << 30) as f64).round()))
                .unwrap_or_default();
            p.title = join(&[m.manufacturer.clone(), m.part_no.clone()]).unwrap_or_else(|| {
                join(&[m.form.clone(), m.locator.clone()]).unwrap_or_else(|| "Memory module".into())
            });
            p.tag = format!("{short} {}", m.mtype.clone().unwrap_or_default())
                .trim()
                .to_string();
            p.metric(p.tag.clone());
            let speed = m.cfg_speed.or(m.speed);
            if let Some((used, total)) = mem_use {
                let pct = 100.0 * used as f64 / total.max(1) as f64;
                p.metric(format!("{pct:.0}% used"));
                p.prop(
                    "System RAM in use",
                    format!("{} of {} ({pct:.0}%)", fmt_bin(used), fmt_bin(total)),
                );
            }
            p.opt("Size", size);
            p.opt(
                "Type",
                join(&[m.mtype.clone(), m.type_detail.clone()]).or_else(|| m.mtype.clone()),
            );
            p.opt("Form factor", m.form.clone());
            p.opt("Rated speed", m.speed.map(|s| format!("{s} MT/s")));
            p.opt("Configured speed", m.cfg_speed.map(|s| format!("{s} MT/s")));
            p.opt("Manufacturer", m.manufacturer.clone());
            p.opt("Part number", m.part_no.clone());
            p.opt("Serial", m.serial.clone());
            p.opt("Rank", m.rank.clone());
            p.opt(
                "Voltage",
                m.voltage_mv.map(|v| format!("{:.2} V", v as f64 / 1000.0)),
            );
            p.opt("Width", m.width.clone());
            p.prop("Source", &self.mem_source);
            if let Some(s) = self.spd.get(k) {
                let max_mts = 2_000_000.0 / s.tck as f64;
                let mts = speed.map(|v| v as f64).unwrap_or(max_mts);
                let tck = 2_000_000.0 / mts;
                let (cl, rcd, rp) = (nck(s.taa, tck), nck(s.trcd, tck), nck(s.trp, tck));
                p.metric(format!("CL{cl}"));
                p.prop("CAS latency", format!("CL{cl} at {mts:.0} MT/s"));
                p.prop(
                    "Timings (CL-RCD-RP)",
                    format!("{cl}-{rcd}-{rp}  (JEDEC, from SPD)"),
                );
                p.prop(
                    "tAA / tRCD / tRP",
                    format!(
                        "{:.2} / {:.2} / {:.2} ns",
                        s.taa as f64 / 1000.0,
                        s.trcd as f64 / 1000.0,
                        s.trp as f64 / 1000.0
                    ),
                );
                p.prop("SPD max rate", format!("{:.0} MT/s ({})", max_mts, s.gen));
                p.opt("SPD part number", s.part.clone());
            }
            p.temp = temps.get(k).copied();
            k += 1;
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ storage

    fn disks(&mut self, hw: &[Hwmon], claimed: &mut HashSet<String>) -> Vec<Part> {
        let mut out = Vec::new();
        let blocks = ls(sp("/sys/block"));
        for c in ls(sp("/sys/class/nvme")) {
            let n = fname(&c);
            let mut p = Part::new(
                format!("nvme:{n}"),
                Kind::Ssd,
                clean(rd(c.join("model"))).unwrap_or_else(|| "NVMe SSD".into()),
            );
            p.tag = "NVMe".into();
            p.bind = vec!["nvme".into(), n.clone()];
            p.prop("Controller", &n);
            p.opt("Firmware", rd(c.join("firmware_rev")));
            p.opt("Serial", clean(rd(c.join("serial"))));
            p.opt("Transport", rd(c.join("transport")));
            let cc = canon(&c);
            let dev = canon(c.join("device"));
            if let Some(dev) = &dev {
                let slot = fname(dev);
                if slot.contains(':') {
                    claimed.insert(slot.clone());
                    p.prop("PCI address", &slot);
                    p.bind.push(slot);
                    let s = rd(dev.join("current_link_speed")).map(|s| s.replace(" PCIe", ""));
                    let w = rd(dev.join("current_link_width"));
                    if let (Some(s), Some(w)) = (s, w) {
                        p.prop("PCIe link", format!("{s} x{w}"));
                    }
                }
            }
            let mut size = 0u64;
            let (mut rs, mut ws, mut have_rate) = (0.0, 0.0, false);
            for b in &blocks {
                let bn = fname(b);
                if !bn.starts_with(&format!("{n}n")) {
                    continue;
                }
                size += rdn::<u64>(b.join("size")).unwrap_or(0) * 512;
                if let Some(st) = rd(b.join("stat")) {
                    let f: Vec<f64> = st
                        .split_whitespace()
                        .filter_map(|x| x.parse().ok())
                        .collect();
                    if f.len() > 6 {
                        if let (Some(r), Some(w)) = (
                            self.rate(&format!("{bn}:r"), f[2] * 512.0),
                            self.rate(&format!("{bn}:w"), f[6] * 512.0),
                        ) {
                            rs += r;
                            ws += w;
                            have_rate = true;
                        }
                    }
                }
                p.prop("Namespace", &bn);
            }
            if size > 0 {
                p.tag = format!("NVMe {}", fmt_dec(size));
                p.metric(fmt_dec(size));
                p.prop("Capacity", fmt_dec(size));
            }
            if have_rate {
                p.metric(format!("R {} W {}", fmt_rate(rs), fmt_rate(ws)));
                p.prop(
                    "Throughput",
                    format!("read {}  write {}", fmt_rate(rs), fmt_rate(ws)),
                );
            }
            if let Some(h) = hw.iter().find(|h| {
                h.name == "nvme"
                    && ((cc.is_some()
                        && h.real
                            .as_ref()
                            .zip(cc.as_ref())
                            .map_or(false, |(r, c)| r.starts_with(c)))
                        || (h.dev.is_some() && (h.dev == dev || h.dev == cc)))
            }) {
                p.temp = main_temp(h);
                for (l, t) in &h.temps {
                    if !l.starts_with("Composite") {
                        p.prop(&format!("Temp {l}"), fmt_temp(*t));
                    }
                }
            }
            out.push(p);
        }

        for b in &blocks {
            let bn = fname(b);
            if !bn.starts_with("sd") {
                continue;
            }
            let cp = canon(b)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            if cp.contains("/usb") {
                continue; // shown with its USB device
            }
            let vendor = clean(rd(b.join("device/vendor"))).filter(|v| v != "ATA");
            let model = clean(rd(b.join("device/model")));
            let rot = rd(b.join("queue/rotational")).as_deref() == Some("1");
            let size = rdn::<u64>(b.join("size")).unwrap_or(0) * 512;
            let mut p = Part::new(
                format!("disk:{bn}"),
                Kind::Ssd,
                join(&[vendor, model]).unwrap_or_else(|| bn.clone()),
            );
            p.tag = format!("{} {}", if rot { "HDD" } else { "SATA" }, fmt_dec(size));
            p.bind = vec!["sata".into(), bn.clone()];
            if let Some(ata) = cp.split('/').find(|s| s.starts_with("ata")) {
                p.bind.push(ata.to_string());
                p.prop("Port", ata);
            }
            p.metric(fmt_dec(size));
            p.prop("Device", format!("/dev/{bn}"));
            p.prop("Capacity", fmt_dec(size));
            p.prop(
                "Media",
                if rot {
                    "rotational (HDD)"
                } else {
                    "solid state"
                },
            );
            p.opt("Firmware", rd(b.join("device/rev")));
            let bdev = canon(b.join("device"));
            if let Some(h) = hw
                .iter()
                .find(|h| h.name == "drivetemp" && h.dev.is_some() && h.dev == bdev)
            {
                p.temp = main_temp(h);
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ network

    fn nets(&mut self, hw: &[Hwmon], pci: &[PciDev], claimed: &mut HashSet<String>) -> Vec<Part> {
        let wl = rd(sp("/proc/net/wireless")).unwrap_or_default();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for n in ls(sp("/sys/class/net")) {
            let ifn = fname(&n);
            if ifn == "lo" {
                continue;
            }
            let Some(dev) = canon(n.join("device")) else {
                continue;
            };
            if dev.to_string_lossy().contains("/usb") {
                continue;
            }
            if !seen.insert(dev.clone()) {
                continue;
            }
            let wireless = n.join("wireless").exists() || n.join("phy80211").exists();
            let pd = pci.iter().find(|d| d.path == dev);
            let title = pd.map(|d| self.pci_title(d)).unwrap_or_else(|| ifn.clone());
            let mut p = Part::new(
                format!("net:{ifn}"),
                if wireless { Kind::Wifi } else { Kind::Ethernet },
                title,
            );
            p.tag = if wireless { "Wi-Fi" } else { "Ethernet" }.into();
            p.bind = vec![ifn.clone(), fname(&dev)];
            if let Some(d) = pd {
                claimed.insert(d.slot.clone());
                self.pci_props(&mut p, d);
            }
            p.prop("Interface", &ifn);
            p.opt("MAC", rd(n.join("address")));
            let state = rd(n.join("operstate"));
            p.opt("State", state.clone());
            p.opt("MTU", rd(n.join("mtu")));
            if let Some(s) = state {
                p.metric(s);
            }
            if !wireless {
                if let Some(sp_) = rdn::<i64>(n.join("speed")).filter(|s| *s > 0) {
                    p.metric(format!("{sp_} Mb/s"));
                    p.prop("Link speed", format!("{sp_} Mb/s"));
                }
            } else if let Some(line) = wl
                .lines()
                .find(|l| l.trim_start().starts_with(&format!("{ifn}:")))
            {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() >= 4 {
                    let q = f[2].trim_end_matches('.');
                    let lvl = f[3].trim_end_matches('.');
                    p.metric(format!("{lvl} dBm"));
                    p.prop("Link quality", format!("{q}/70"));
                    p.prop("Signal", format!("{lvl} dBm"));
                }
            }
            if let (Some(rx), Some(tx)) = (
                rdn::<f64>(n.join("statistics/rx_bytes")),
                rdn::<f64>(n.join("statistics/tx_bytes")),
            ) {
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
            if let Some(h) = hw.iter().find(|h| h.dev.as_ref() == Some(&dev)) {
                p.temp = main_temp(h);
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ chipset

    fn chipset(
        &mut self,
        hw: &[Hwmon],
        tz: &[(String, f64)],
        pci: &[PciDev],
        claimed: &mut HashSet<String>,
    ) -> Option<Part> {
        let isa = pci
            .iter()
            .find(|d| d.class >> 8 == 0x0601 && d.slot.starts_with("0000:00:"));
        let t = hw
            .iter()
            .find(|h| h.name.starts_with("pch_"))
            .and_then(main_temp)
            .or_else(|| tz.iter().find(|(t, _)| t.starts_with("pch_")).map(|x| x.1));
        if isa.is_none() && t.is_none() {
            return None;
        }
        let title = isa
            .map(|d| {
                let mut s = self.pci_title(d);
                for junk in [
                    " LPC/eSPI Controller",
                    " LPC Controller",
                    " eSPI Controller",
                    " ISA bridge",
                ] {
                    s = s.replace(junk, "");
                }
                s
            })
            .unwrap_or_else(|| "Chipset (PCH)".into());
        let mut p = Part::new("chipset", Kind::Chipset, title);
        p.tag = "PCH".into();
        p.temp = t;
        if let Some(d) = isa {
            claimed.insert(d.slot.clone());
            p.prop("PCI address", &d.slot);
        }
        for d in pci.iter().filter(|d| {
            d.slot.starts_with("0000:00:") && !claimed.contains(&d.slot) && d.class >> 8 != 0x0604
        }) {
            let short = d.slot.get(5..).unwrap_or(&d.slot).to_string();
            p.prop(&format!("{short} {}", self.pci_class(d)), self.pci_title(d));
        }
        Some(p)
    }

    // ------------------------------------------------------------ battery

    fn batteries(&mut self) -> Vec<Part> {
        let ps = ls(sp("/sys/class/power_supply"));
        let ac: Option<bool> = ps
            .iter()
            .filter(|p| rd(p.join("type")).as_deref() == Some("Mains"))
            .filter_map(|p| rdn::<u8>(p.join("online")))
            .map(|v| v == 1)
            .reduce(|a, b| a || b);
        let mut out = Vec::new();
        for b in ps.iter().filter(|p| {
            rd(p.join("type")).as_deref() == Some("Battery")
                && rd(p.join("scope")).as_deref() != Some("Device")
        }) {
            let n = fname(b);
            let num = |k: &str| rdn::<f64>(b.join(k));
            let mut p = Part::new(
                format!("bat:{n}"),
                Kind::Battery,
                join(&[
                    clean(rd(b.join("manufacturer"))),
                    clean(rd(b.join("model_name"))),
                ])
                .unwrap_or_else(|| n.clone()),
            );
            p.tag = "Battery".into();
            p.bind = vec![n.clone()];
            if rd(b.join("present")).as_deref() == Some("0") {
                p.present = false;
                out.push(p);
                continue;
            }
            let status = rd(b.join("status"));
            let cap = num("capacity");
            let wh = b.join("energy_now").exists();
            let (now, full, design) = if wh {
                (
                    num("energy_now"),
                    num("energy_full"),
                    num("energy_full_design"),
                )
            } else {
                (
                    num("charge_now"),
                    num("charge_full"),
                    num("charge_full_design"),
                )
            };
            let frac = cap.map(|c| c / 100.0).or_else(|| {
                now.zip(full)
                    .map(|(n, f)| if f > 0.0 { n / f } else { 0.0 })
            });
            p.level = frac.map(|f| f.clamp(0.0, 1.0));
            let v = num("voltage_now").map(|x| x / 1e6);
            let cur = num("current_now").map(|x| x.abs() / 1e6);
            let pw = num("power_now")
                .map(|x| x.abs() / 1e6)
                .or_else(|| cur.zip(v).map(|(a, b)| a * b));
            let cycles = rdn::<i64>(b.join("cycle_count")).filter(|c| *c > 0);
            let charging = status.as_deref() == Some("Charging");
            let discharging = status.as_deref() == Some("Discharging");

            if let Some(f) = frac {
                p.metric(format!("{:.0}%", f * 100.0));
                p.prop("Charge", format!("{:.0}%", f * 100.0));
            }
            if let Some(s) = &status {
                p.metric(s.clone());
            }
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
            p.opt("Status", status.clone());
            p.opt("Capacity level", rd(b.join("capacity_level")));
            p.opt("Voltage", v.map(|v| format!("{v:.3} V")));
            p.opt(
                "Voltage (min design)",
                num("voltage_min_design").map(|x| format!("{:.2} V", x / 1e6)),
            );
            p.opt("Current", cur.map(|a| format!("{a:.3} A")));
            p.opt(
                if discharging {
                    "Power draw"
                } else {
                    "Charge rate"
                },
                pw.map(|w| format!("{w:.2} W")),
            );
            let unit = if wh { "Wh" } else { "Ah" };
            let f2 = |x: Option<f64>| x.map(|x| format!("{:.2} {unit}", x / 1e6));
            p.opt("Now", f2(now));
            p.opt("Full", f2(full));
            p.opt("Design", f2(design));
            if let (Some(f), Some(d)) = (full, design) {
                if d > 0.0 {
                    p.prop("Health (full / design)", format!("{:.0}%", 100.0 * f / d));
                }
            }
            p.opt("Cycle count", cycles);
            // hours = stored / rate; both µWh/µW or µAh/µA
            let rate = if wh {
                num("power_now")
            } else {
                num("current_now")
            }
            .map(f64::abs);
            if let (Some(n), Some(f), Some(r)) = (now, full, rate.filter(|r| *r > 0.0)) {
                if discharging {
                    p.prop("Time to empty", fmt_hours(n / r));
                } else if charging {
                    p.prop("Time to full", fmt_hours((f - n).max(0.0) / r));
                }
            }
            if let Some(t) = num("temp") {
                p.temp = Some(t / 10.0);
            }
            p.opt("Technology", rd(b.join("technology")));
            p.opt("Manufacturer", clean(rd(b.join("manufacturer"))));
            p.opt("Model", clean(rd(b.join("model_name"))));
            if let Some(a) = ac {
                p.prop("AC adapter", if a { "connected" } else { "unplugged" });
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ USB

    fn usb_lookup(&mut self, vid: &str, pid: &str) -> Option<String> {
        let key = format!("{vid}:{pid}");
        if let Some(v) = self.usb_names.get(&key) {
            return v.clone();
        }
        let v = run("lsusb", &["-d", &key]).and_then(|o| {
            o.lines()
                .next()
                .and_then(|l| {
                    l.split_once(key.as_str())
                        .map(|(_, r)| r.trim().to_string())
                })
                .filter(|s| !s.is_empty())
        });
        self.usb_names.insert(key, v.clone());
        v
    }

    fn usb(&mut self, built_in: &HashSet<String>) -> Vec<Part> {
        let blocks: Vec<(PathBuf, PathBuf)> = ls(sp("/sys/block"))
            .into_iter()
            .filter_map(|b| Some((canon(&b)?, b)))
            .filter(|(c, _)| c.to_string_lossy().contains("/usb"))
            .collect();
        let mut out = Vec::new();
        for d in ls(sp("/sys/bus/usb/devices")) {
            let n = fname(&d);
            if !n.contains('-') || n.contains(':') || built_in.contains(&n) {
                continue;
            }
            let class = rd(d.join("bDeviceClass")).unwrap_or_default();
            if class == "09" {
                continue; // hubs
            }
            let vid = rd(d.join("idVendor")).unwrap_or_default();
            let pid = rd(d.join("idProduct")).unwrap_or_default();
            let mut classes: Vec<&'static str> = Vec::new();
            let mut drivers: Vec<String> = Vec::new();
            if class != "00" && class != "ef" && !class.is_empty() {
                classes.push(usb_class(&class));
            }
            for e in ls(&d) {
                if !fname(&e).starts_with(&format!("{n}:")) {
                    continue;
                }
                let c = usb_class(&rd(e.join("bInterfaceClass")).unwrap_or_default());
                if !classes.contains(&c) {
                    classes.push(c);
                }
                if let Some(dr) = link_base(e.join("driver")) {
                    if !drivers.contains(&dr) {
                        drivers.push(dr);
                    }
                }
            }
            let product = clean(rd(d.join("product")));
            let maker = clean(rd(d.join("manufacturer")));
            let looked = if product.is_none() {
                self.usb_lookup(&vid, &pid)
            } else {
                None
            };
            let title = product
                .clone()
                .or(looked)
                .unwrap_or_else(|| format!("USB device {vid}:{pid}"));
            let ct = rd(d.join("port/connect_type"));
            let removable = rd(d.join("removable"));
            let internal = match ct.as_deref() {
                Some("hardwired") => true,
                Some("hotplug") => false,
                _ => match removable.as_deref() {
                    Some("fixed") => true,
                    Some("removable") => false,
                    _ => classes.contains(&"Wireless") && !n.contains('.'),
                },
            };
            let mut p = Part::new(
                format!("usb:{n}"),
                if classes.contains(&"Video") && internal {
                    Kind::Camera
                } else if internal {
                    Kind::UsbInternal
                } else {
                    Kind::Usb
                },
                title,
            );
            p.tag = classes.first().copied().unwrap_or("USB").to_string();
            if classes.contains(&"Video") {
                p.tag = "Camera".into();
            }
            let root = n.split('.').next().unwrap_or(&n).to_string();
            p.bind = vec![root, n.clone()];
            let speed = rd(d.join("speed")).unwrap_or_default();
            let (sp_short, sp_long) = usb_speed(&speed);
            p.metric(sp_short);
            p.prop("ID", format!("{vid}:{pid}"));
            p.opt("Manufacturer", maker);
            p.opt("Product", product);
            p.prop(
                "Location",
                format!(
                    "bus {} port {}{}",
                    rd(d.join("busnum")).unwrap_or_default(),
                    n.split_once('-').map(|x| x.1).unwrap_or(&n),
                    if n.contains('.') {
                        " (behind a hub)"
                    } else {
                        ""
                    }
                ),
            );
            p.prop("Speed", sp_long);
            p.opt("USB version", rd(d.join("version")));
            p.opt("Max power", rd(d.join("bMaxPower")));
            if !classes.is_empty() {
                p.prop("Classes", classes.join(", "));
            }
            if !drivers.is_empty() {
                p.prop("Drivers", drivers.join(", "));
            }
            p.prop(
                "Connection",
                format!(
                    "{}{}",
                    if internal {
                        "internal"
                    } else {
                        "external port"
                    },
                    ct.or(removable)
                        .map(|s| format!(" ({s})"))
                        .unwrap_or_default()
                ),
            );
            if let Some(dc) = canon(&d) {
                for (bc, b) in &blocks {
                    if bc.starts_with(&dc) {
                        let bn = fname(b);
                        let size = rdn::<u64>(b.join("size")).unwrap_or(0) * 512;
                        if size > 0 {
                            p.metric(fmt_dec(size));
                        }
                        p.prop(&format!("Disk /dev/{bn}"), fmt_dec(size));
                    }
                }
            }
            out.push(p);
        }
        out
    }

    // ------------------------------------------------------------ the rest of PCI

    fn other_pci(&self, pci: &[PciDev], claimed: &HashSet<String>) -> Vec<Part> {
        pci.iter()
            .filter(|d| {
                !claimed.contains(&d.slot)
                    && !d.slot.starts_with("0000:00:")
                    && ![0x0600, 0x0604].contains(&(d.class >> 8))
            })
            .map(|d| {
                let mut p = Part::new(format!("pci:{}", d.slot), Kind::Pci, self.pci_title(d));
                p.tag = self.pci_class(d);
                p.bind = vec![d.slot.clone()];
                self.pci_props(&mut p, d);
                p
            })
            .collect()
    }
}

// ---------------------------------------------------------------- displays

pub(crate) struct Edid {
    pub vendor: String,
    pub product: u16,
    pub year: Option<u32>,
    pub name: Option<String>,
    pub text: Vec<String>,
    pub diag_in: Option<f64>,
    pub native: Option<(u32, u32, f64)>,
    pub version: String,
}

pub(crate) fn parse_edid(b: &[u8]) -> Option<Edid> {
    if b.len() < 128 || b[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return None;
    }
    let v = u16::from_be_bytes([b[8], b[9]]);
    let vendor: String = [(v >> 10) & 31, (v >> 5) & 31, v & 31]
        .iter()
        .map(|c| (*c as u8 + 64) as char)
        .collect();
    let (w, h) = (b[21] as f64, b[22] as f64);
    let mut e = Edid {
        vendor,
        product: u16::from_le_bytes([b[10], b[11]]),
        year: (b[17] > 0).then(|| b[17] as u32 + 1990),
        name: None,
        text: Vec::new(),
        diag_in: (w > 0.0 && h > 0.0).then(|| (w * w + h * h).sqrt() / 2.54),
        native: None,
        version: format!("{}.{}", b[18], b[19]),
    };
    for d in [54usize, 72, 90, 108] {
        let x = &b[d..d + 18];
        let clock = u16::from_le_bytes([x[0], x[1]]) as f64 * 10_000.0;
        if clock > 0.0 {
            if e.native.is_none() {
                let ha = x[2] as u32 | ((x[4] as u32 >> 4) << 8);
                let hb = x[3] as u32 | ((x[4] as u32 & 15) << 8);
                let va = x[5] as u32 | ((x[7] as u32 >> 4) << 8);
                let vb = x[6] as u32 | ((x[7] as u32 & 15) << 8);
                let total = ((ha + hb) * (va + vb)) as f64;
                if total > 0.0 {
                    e.native = Some((ha, va, clock / total));
                }
            }
            continue;
        }
        let txt = || {
            clean(Some(
                x[5..18]
                    .iter()
                    .take_while(|c| **c != b'\n')
                    .map(|&c| c as char)
                    .collect(),
            ))
        };
        match x[3] {
            0xfc => e.name = txt(),
            0xfe => e.text.extend(txt()),
            _ => {}
        }
    }
    Some(e)
}

pub(crate) fn panel_vendor(id: &str) -> &str {
    match id {
        "AUO" => "AU Optronics",
        "BOE" => "BOE",
        "CMN" => "Innolux",
        "IVO" => "InfoVision",
        "LGD" => "LG Display",
        "SDC" => "Samsung Display",
        "SHP" => "Sharp",
        "CSO" => "CSOT",
        "NCP" => "Nanchang",
        "APP" => "Apple",
        "DEL" => "Dell",
        "GSM" => "LG",
        "SAM" => "Samsung",
        "ACR" => "Acer",
        "AUS" => "ASUS",
        "HWP" => "HP",
        "LEN" => "Lenovo",
        "BNQ" => "BenQ",
        "VSC" => "ViewSonic",
        _ => id,
    }
}

pub(crate) fn edid_title(e: &Edid) -> String {
    let model = e.name.clone().or_else(|| e.text.last().cloned());
    format!(
        "{}{}",
        panel_vendor(&e.vendor),
        model.map(|m| format!(" {m}")).unwrap_or_default()
    )
}

pub(crate) fn edid_props(p: &mut Part, e: &Edid) {
    if let Some((w, h, hz)) = e.native {
        p.metric(format!("{w}x{h}"));
        p.metric(format!("{hz:.0} Hz"));
        p.prop("Native mode", format!("{w}x{h} @ {hz:.2} Hz"));
    }
    if let Some(d) = e.diag_in {
        p.metric(format!("{d:.1}\""));
        p.prop("Size", format!("{d:.1}\""));
    }
    p.prop(
        "Manufacturer",
        format!("{} ({})", panel_vendor(&e.vendor), e.vendor),
    );
    p.prop("Product code", format!("{:04x}", e.product));
    p.opt("Model", e.name.clone());
    if !e.text.is_empty() {
        p.prop("Panel strings", e.text.join(", "));
    }
    p.opt("Manufactured", e.year);
    p.prop("EDID version", &e.version);
}

fn displays() -> Vec<Part> {
    let backlight = ls(sp("/sys/class/backlight")).into_iter().find_map(|b| {
        let cur = rdn::<f64>(b.join("actual_brightness")).or_else(|| rdn(b.join("brightness")))?;
        let max = rdn::<f64>(b.join("max_brightness")).filter(|m| *m > 0.0)?;
        Some((cur / max, fname(&b)))
    });
    let mut out = Vec::new();
    for c in ls(sp("/sys/class/drm")) {
        let n = fname(&c);
        let Some((_, conn)) = n.split_once('-') else {
            continue;
        };
        if rd(c.join("status")).as_deref() != Some("connected") {
            continue;
        }
        // "eDP-1" -> "eDP", "HDMI-A-1" -> "HDMI-A"
        let ctype = conn
            .rsplit_once('-')
            .map(|x| x.0)
            .unwrap_or(conn)
            .to_string();
        let internal = ["eDP", "LVDS", "DSI"].contains(&ctype.as_str());
        let edid = fs::read(c.join("edid")).ok().and_then(|b| parse_edid(&b));
        let title = edid.as_ref().map(edid_title).unwrap_or_else(|| {
            if internal {
                "Built-in display".into()
            } else {
                format!("{conn} display")
            }
        });
        let mut p = Part::new(format!("display:{n}"), Kind::Display, title);
        p.tag = if internal {
            "Screen".into()
        } else {
            conn.to_string()
        };
        p.bind = vec![
            ctype.clone(),
            conn.to_string(),
            (if internal { "internal" } else { "external" }).into(),
        ];
        p.prop("Connector", conn);
        p.prop(
            "Type",
            if internal {
                "built-in panel"
            } else {
                "external monitor"
            },
        );
        p.opt("Enabled", rd(c.join("enabled")));
        let mode = rd(c.join("modes")).and_then(|m| m.lines().next().map(|s| s.to_string()));
        if let Some(e) = &edid {
            edid_props(&mut p, e);
        } else if let Some(m) = &mode {
            p.metric(m.clone());
        }
        p.opt("Preferred mode", mode);
        if internal {
            if let Some((lvl, dev)) = &backlight {
                p.level = Some(lvl.clamp(0.0, 1.0));
                p.metric(format!("{:.0}% brightness", lvl * 100.0));
                p.prop("Brightness", format!("{:.0}% ({dev})", lvl * 100.0));
            }
        }
        out.push(p);
    }
    out
}

// ---------------------------------------------------------------- keyboard & touchpad

struct InputDev {
    bus: u32,
    name: String,
    phys: String,
    sysfs: String,
    handlers: Vec<String>,
    prop: u64,
}

fn parse_input_devices(s: &str) -> Vec<InputDev> {
    s.split("\n\n")
        .filter_map(|b| {
            let mut d = InputDev {
                bus: 0,
                name: String::new(),
                phys: String::new(),
                sysfs: String::new(),
                handlers: vec![],
                prop: 0,
            };
            for l in b.lines() {
                let Some((tag, rest)) = l.split_once(": ") else {
                    continue;
                };
                match tag {
                    "I" => {
                        d.bus = rest
                            .split_whitespace()
                            .find_map(|kv| kv.strip_prefix("Bus="))
                            .and_then(|v| u32::from_str_radix(v, 16).ok())
                            .unwrap_or(0)
                    }
                    "N" => {
                        d.name = rest
                            .trim_start_matches("Name=")
                            .trim_matches('"')
                            .to_string()
                    }
                    "P" => d.phys = rest.trim_start_matches("Phys=").to_string(),
                    "S" => d.sysfs = rest.trim_start_matches("Sysfs=").to_string(),
                    "H" => {
                        d.handlers = rest
                            .trim_start_matches("Handlers=")
                            .split_whitespace()
                            .map(String::from)
                            .collect()
                    }
                    "B" => {
                        if let Some(v) = rest.strip_prefix("PROP=") {
                            d.prop = u64::from_str_radix(v.trim(), 16).unwrap_or(0)
                        }
                    }
                    _ => {}
                }
            }
            (!d.name.is_empty()).then_some(d)
        })
        .collect()
}

fn bus_name(b: u32) -> &'static str {
    match b {
        0x03 => "USB",
        0x05 => "Bluetooth",
        0x11 => "PS/2 (i8042)",
        0x18 => "I2C",
        0x19 => "Host",
        0x1c => "SPI",
        _ => "other",
    }
}

/// USB device name ("1-8") an input node hangs off, if any.
fn usb_dev_of(sysfs: &str) -> Option<String> {
    sysfs
        .split('/')
        .filter(|seg| {
            let mut it = seg.splitn(2, '-');
            matches!((it.next(), it.next()), (Some(a), Some(b))
                if !a.is_empty() && a.chars().all(|c| c.is_ascii_digit())
                    && !b.is_empty() && b.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
        .last()
        .map(String::from)
}

pub(crate) fn touchpad_vendor(name: &str) -> Option<&'static str> {
    let n = name.to_uppercase();
    [
        ("06CB", "Synaptics"),
        ("04F3", "ELAN"),
        ("093A", "PixArt"),
        ("27C6", "Goodix"),
        ("2808", "FocalTech"),
        ("SYNA", "Synaptics"),
        ("ELAN", "ELAN"),
        ("ALPS", "Alps"),
    ]
    .iter()
    .find(|(k, _)| n.contains(k))
    .map(|(_, v)| *v)
}

/// Built-in keyboard and touchpad. Also returns USB devices they live on,
/// so those aren't drawn again as plugged-in USB gadgets.
fn inputs(dmi: &HashMap<String, String>) -> (Vec<Part>, HashSet<String>) {
    let devs = parse_input_devices(&rd(sp("/proc/bus/input/devices")).unwrap_or_default());
    let vendor = dmi
        .get("sys_vendor")
        .map(|v| v.to_lowercase())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut usb_claimed = HashSet::new();
    let is_internal = |d: &InputDev| -> bool {
        match d.bus {
            0x11 | 0x18 | 0x19 | 0x1c => true,
            0x03 => {
                let hardwired = usb_dev_of(&d.sysfs).map_or(false, |u| {
                    rd(sp(&format!("/sys/bus/usb/devices/{u}/port/connect_type"))).as_deref()
                        == Some("hardwired")
                });
                hardwired || (!vendor.is_empty() && d.name.to_lowercase().contains(&vendor))
            }
            _ => false,
        }
    };
    let has = |d: &InputDev, h: &str| d.handlers.iter().any(|x| x == h);

    // keyboard: a node with key + LED handlers
    if let Some(k) = devs
        .iter()
        .find(|d| has(d, "kbd") && has(d, "leds") && is_internal(d))
    {
        let title = if k.name == "AT Translated Set 2 keyboard" {
            "Built-in keyboard".to_string()
        } else {
            k.name.clone()
        };
        let mut p = Part::new("keyboard", Kind::Keyboard, title);
        p.tag = "Keyboard".into();
        p.prop("Name", &k.name);
        p.prop("Bus", bus_name(k.bus));
        p.prop("Phys", &k.phys);
        p.prop("Handlers", k.handlers.join(" "));
        let input = k.sysfs.rsplit('/').next().unwrap_or("").to_string();
        for (led, label) in [
            ("capslock", "Caps Lock"),
            ("numlock", "Num Lock"),
            ("scrolllock", "Scroll Lock"),
        ] {
            if let Some(v) = rdn::<u32>(sp(&format!("/sys/class/leds/{input}::{led}/brightness"))) {
                p.prop(label, if v > 0 { "on" } else { "off" });
                if v > 0 {
                    p.metric(format!("{label} on"));
                }
            }
        }
        if let Some(b) = ls(sp("/sys/class/leds"))
            .into_iter()
            .find(|l| fname(l).contains("kbd_backlight"))
        {
            if let (Some(c), Some(m)) = (
                rdn::<f64>(b.join("brightness")),
                rdn::<f64>(b.join("max_brightness")),
            ) {
                if m > 0.0 {
                    p.level = Some(c / m);
                    p.prop("Backlight", format!("{:.0}%", 100.0 * c / m));
                }
            }
        }
        if let Some(u) = usb_dev_of(&k.sysfs) {
            p.prop("USB device", &u);
            usb_claimed.insert(u);
        }
        out.push(p);
    }

    // touchpad: by name, else a pointer device with the touchpad property
    let tp = devs
        .iter()
        .find(|d| d.name.to_lowercase().contains("touchpad") && is_internal(d))
        .or_else(|| {
            devs.iter()
                .find(|d| d.prop & 1 != 0 && has(d, "mouse") && is_internal(d))
        });
    if let Some(t) = tp {
        let title = match touchpad_vendor(&t.name) {
            Some(v) => format!("{v} touchpad"),
            None => t.name.clone(),
        };
        let mut p = Part::new("touchpad", Kind::Touchpad, title);
        p.tag = "Touchpad".into();
        p.prop("Name", &t.name);
        p.prop("Bus", bus_name(t.bus));
        p.prop("Phys", &t.phys);
        if t.prop & 4 != 0 {
            p.prop("Type", "clickpad (button under the surface)");
        }
        p.metric(bus_name(t.bus));
        if let Some(u) = usb_dev_of(&t.sysfs) {
            usb_claimed.insert(u);
        }
        out.push(p);
    }
    (out, usb_claimed)
}

// ---------------------------------------------------------------- speakers & microphone

/// "45%" / "muted" from wpctl (PipeWire) or amixer (ALSA).
fn volume(source: bool) -> Option<String> {
    let target = if source {
        "@DEFAULT_AUDIO_SOURCE@"
    } else {
        "@DEFAULT_AUDIO_SINK@"
    };
    if let Some(o) = run("wpctl", &["get-volume", target]) {
        let v = o
            .split_whitespace()
            .nth(1)
            .and_then(|v| v.parse::<f64>().ok());
        if let Some(v) = v {
            return Some(if o.contains("MUTED") {
                "muted".into()
            } else {
                format!("{:.0}%", v * 100.0)
            });
        }
    }
    let ctl = if source { "Capture" } else { "Master" };
    let o = run("amixer", &["-c", "0", "sget", ctl])?;
    let l = o.lines().find(|l| l.contains('%'))?;
    let pct = l.split('[').nth(1)?.split(']').next()?.to_string();
    Some(if l.contains("[off]") {
        "muted".into()
    } else {
        pct
    })
}

fn audio() -> Vec<Part> {
    let mut codecs = Vec::new();
    let (mut spk, mut mic, mut dmic) = (0, 0, false);
    for card in ls(sp("/proc/asound")) {
        let n = fname(&card);
        if !n.starts_with("card") || !n[4..].chars().all(|c| c.is_ascii_digit()) || n.len() == 4 {
            continue;
        }
        for f in ls(&card) {
            let fnm = fname(&f);
            if fnm.starts_with("codec#") {
                let t = rd(&f).unwrap_or_default();
                if let Some(c) = t.lines().find_map(|l| l.strip_prefix("Codec: ")) {
                    codecs.push(c.trim().to_string());
                }
                // internal (fixed) pins only: jacks are "[Jack]"
                spk += t.lines().filter(|l| l.contains("[Fixed] Speaker")).count();
                mic += t.lines().filter(|l| l.contains("[Fixed] Mic")).count();
            } else if fnm.starts_with("pcm") && fnm.ends_with('c') {
                if rd(f.join("info")).map_or(false, |i| i.contains("DMIC")) {
                    dmic = true;
                }
            }
        }
    }
    let card = rd(sp("/proc/asound/cards")).and_then(|c| {
        c.lines()
            .next()
            .and_then(|l| l.split(" - ").nth(1))
            .map(|s| s.trim().to_string())
    });
    let codec = codecs.first().cloned();
    let mut out = Vec::new();
    if spk > 0 {
        let vol = volume(false);
        for (id, side) in [("speaker:l", "left"), ("speaker:r", "right")] {
            let mut p = Part::new(id, Kind::Speaker, format!("Speaker ({side})"));
            p.tag = "Speaker".into();
            if let Some(v) = &vol {
                p.metric(v.clone());
                p.prop("Volume", v);
            }
            p.prop("Channel", side);
            p.opt("Codec", codec.clone());
            p.opt("Sound card", card.clone());
            out.push(p);
        }
    }
    if mic > 0 || dmic {
        let mut p = Part::new(
            "mic",
            Kind::Microphone,
            if dmic {
                "Digital microphone array"
            } else {
                "Internal microphone"
            },
        );
        p.tag = "Mic".into();
        if let Some(v) = volume(true) {
            p.metric(v.clone());
            p.prop("Input level", v);
        }
        p.prop(
            "Type",
            if dmic {
                "digital (DMIC)"
            } else {
                "analog, on the codec"
            },
        );
        p.opt("Codec", codec);
        p.opt("Sound card", card);
        out.push(p);
    }
    out
}

/// Live values from one `nvidia-smi` row (see `query_nvidia`).
pub(crate) fn nvidia_props(p: &mut Part, nv: &HashMap<String, String>) {
    let f = |k: &str| clean(nv.get(k).cloned());
    if let Some(c) = f("clocks.gr") {
        p.metric(format!("{c} MHz"));
        p.prop("Core clock", format!("{c} MHz"));
    }
    if let Some(u) = f("utilization.gpu") {
        p.metric(format!("{u}%"));
        p.prop("Utilization", format!("{u}%"));
    }
    if let (Some(u), Some(t)) = (
        f("memory.used").and_then(|v| v.parse::<f64>().ok()),
        f("memory.total").and_then(|v| v.parse::<f64>().ok()),
    ) {
        p.metric(format!("{:.1}/{:.0}G", u / 1024.0, t / 1024.0));
        p.prop("VRAM", format!("{u:.0} / {t:.0} MiB"));
    }
    if let Some(w) = f("power.draw").and_then(|v| v.parse::<f64>().ok()) {
        p.metric(format!("{w:.0} W"));
        p.prop("Power draw", format!("{w:.1} W"));
    }
    p.opt("Memory clock", f("clocks.mem").map(|c| format!("{c} MHz")));
    p.opt("P-state", f("pstate"));
    p.opt("Fan", f("fan.speed").map(|c| format!("{c}%")));
    p.opt("NVIDIA driver", f("driver_version"));
    p.temp = f("temperature.gpu").and_then(|v| v.parse().ok());
}

pub(crate) fn query_nvidia() -> HashMap<String, HashMap<String, String>> {
    let mut m = HashMap::new();
    let q = format!("--query-gpu={}", NV_FIELDS.join(","));
    if let Some(o) = run("nvidia-smi", &[&q, "--format=csv,noheader,nounits"]) {
        for line in o.lines() {
            let vals: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
            if vals.len() < NV_FIELDS.len() {
                continue;
            }
            let row: HashMap<String, String> = NV_FIELDS
                .iter()
                .zip(vals.iter())
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            m.insert(slot_suffix(vals[0]), row);
        }
    }
    m
}

pub(crate) fn slot_suffix(s: &str) -> String {
    let s = s.to_ascii_lowercase();
    s[s.len().saturating_sub(7)..].to_string()
}

fn read_dmi() -> HashMap<String, String> {
    let mut m = HashMap::new();
    for k in [
        "sys_vendor",
        "product_name",
        "product_version",
        "product_family",
        "product_sku",
        "board_vendor",
        "board_name",
        "board_version",
        "bios_vendor",
        "bios_version",
        "bios_date",
        "bios_release",
        "ec_firmware_release",
        "chassis_type",
        "chassis_vendor",
    ] {
        if let Some(v) = clean(rd(sp(&format!("/sys/class/dmi/id/{k}")))) {
            m.insert(k.to_string(), v);
        }
    }
    m
}

fn bios(dmi: &HashMap<String, String>) -> Part {
    let g = |k: &str| dmi.get(k).cloned();
    let efi = sp("/sys/firmware/efi");
    let fw = if efi.exists() { "UEFI BIOS" } else { "BIOS" };
    let mut p = Part::new(
        "bios",
        Kind::Bios,
        match g("bios_version") {
            Some(v) => format!("{fw} {v}"),
            None => fw.to_string(),
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
    p.prop(
        "Firmware type",
        if efi.exists() {
            "UEFI"
        } else {
            "Legacy BIOS / CSM"
        },
    );
    if let Ok(b) = fs::read(efi.join("efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-e39e1e2a2b8c")) {
        if let Some(last) = b.last() {
            p.prop(
                "Secure Boot",
                if *last == 1 { "enabled" } else { "disabled" },
            );
        }
    }
    p
}

fn fans(hw: &[Hwmon]) -> Vec<Part> {
    let mut out = Vec::new();
    for h in hw {
        for (i, (label, rpm)) in h.fans.iter().enumerate() {
            let title = if label.starts_with("fan") {
                format!("Fan {} ({})", i + 1, h.name)
            } else {
                format!("{label} ({})", h.name)
            };
            let mut p = Part::new(format!("fan:{}:{i}", h.id), Kind::Fan, title);
            p.tag = "Fan".into();
            p.rpm = Some(*rpm);
            p.metric(format!("{rpm} RPM"));
            p.prop("Speed", format!("{rpm} RPM"));
            p.prop("Sensor", format!("{} ({})", h.name, h.id));
            out.push(p);
        }
    }
    out
}

pub(crate) fn clean_cpu_name(model: &str) -> String {
    let mut s = model
        .replace("(R)", "")
        .replace("(TM)", "")
        .replace("(tm)", "");
    if let Some(i) = s.find(" @ ") {
        s.truncate(i);
    }
    let s = s.replace(" CPU", "").replace(" Processor", "");
    s.split_whitespace()
        .filter(|w| !w.ends_with("-Core"))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn chassis_name(c: u32) -> &'static str {
    match c {
        3 => "Desktop",
        4 => "Low-profile desktop",
        5 => "Pizza box",
        6 => "Mini tower",
        7 => "Tower",
        8 => "Portable",
        9 => "Laptop",
        10 => "Notebook",
        11 => "Handheld",
        13 => "All-in-one",
        14 => "Sub-notebook",
        17 => "Main server chassis",
        23 => "Rack mount",
        30 => "Tablet",
        31 => "Convertible",
        32 => "Detachable",
        35 => "Mini PC",
        36 => "Stick PC",
        _ => "Other",
    }
}

pub(crate) fn usb_class(c: &str) -> &'static str {
    match c.to_ascii_lowercase().as_str() {
        "01" => "Audio",
        "02" => "Comms",
        "03" => "HID",
        "05" => "Physical",
        "06" => "Imaging",
        "07" => "Printer",
        "08" => "Storage",
        "09" => "Hub",
        "0a" => "CDC data",
        "0b" => "Smart card",
        "0d" => "Security",
        "0e" => "Video",
        "0f" => "Healthcare",
        "10" => "AV",
        "11" => "Billboard",
        "dc" => "Diagnostic",
        "e0" => "Wireless",
        "ef" => "Misc",
        "fe" => "App-specific",
        "ff" => "Vendor",
        _ => "USB",
    }
}

pub(crate) fn usb_speed(s: &str) -> (String, String) {
    match s {
        "1.5" => ("1.5M".into(), "USB 1.x Low Speed (1.5 Mb/s)".into()),
        "12" => ("12M".into(), "USB 1.x Full Speed (12 Mb/s)".into()),
        "480" => ("480M".into(), "USB 2.0 High Speed (480 Mb/s)".into()),
        "5000" => ("5G".into(), "USB 3 SuperSpeed (5 Gb/s)".into()),
        "10000" => ("10G".into(), "USB 3 SuperSpeed+ (10 Gb/s)".into()),
        "20000" => ("20G".into(), "USB 3 SuperSpeed+ 2x2 (20 Gb/s)".into()),
        "" => (String::new(), "unknown".into()),
        x => (format!("{x}M"), format!("{x} Mb/s")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spd_ddr4_cl() {
        let mut b = vec![0u8; 512];
        b[2] = 0x0C;
        b[18] = 6; // 750ps  -> DDR4-2666
        b[125] = 0;
        b[24] = 110; // 13.75ns
        b[123] = 0;
        b[25] = 110;
        b[26] = 110;
        let s = parse_spd(&b).unwrap();
        assert_eq!(s.tck, 750);
        assert_eq!(nck(s.taa, 750.0), 19);
    }

    #[test]
    fn lspci_parse() {
        let o = "0000:01:00.0 \"VGA compatible controller\" \"NVIDIA Corporation\" \"TU106M [GeForce RTX 2060 Mobile]\" -ra1 \"Razer USA Ltd.\" \"Device 1234\"\n";
        let m = parse_lspci(o);
        let n = &m["0000:01:00.0"];
        assert_eq!(short_vendor(&n.vendor), "NVIDIA");
        assert_eq!(nice_device(&n.device), "GeForce RTX 2060 Mobile");
    }

    #[test]
    fn udev_mem() {
        let o = "MEMORY_ARRAY_NUM_DEVICES=2\nMEMORY_DEVICE_0_SIZE=17179869184\nMEMORY_DEVICE_0_PRESENT=1\nMEMORY_DEVICE_0_LOCATOR=ChannelA-DIMM0\nMEMORY_DEVICE_0_CONFIGURED_SPEED_MTS=2667\nMEMORY_DEVICE_1_PRESENT=0\nMEMORY_DEVICE_1_LOCATOR=ChannelB-DIMM0\n";
        let v = memdevs_from_udev(o);
        assert_eq!(v.len(), 2);
        assert!(v[0].present && !v[1].present);
        assert_eq!(v[0].cfg_speed, Some(2667));
    }

    #[test]
    fn names() {
        assert_eq!(
            clean_cpu_name("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz"),
            "Intel Core i7-9750H"
        );
        assert_eq!(natcmp("hwmon10", "hwmon2"), Ordering::Greater);
        assert_eq!(
            join(&[Some("Razer".into()), Some("Razer Blade 15".into())]).unwrap(),
            "Razer Blade 15"
        );
    }
}
