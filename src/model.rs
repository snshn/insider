//! Hardware data model shared by probes, board layouts and the UI.

use std::collections::HashMap;
use std::fmt::Display;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Board,
    Cpu,
    Gpu,
    Dimm,
    Ssd,
    Wifi,
    Ethernet,
    Chipset,
    Bios,
    Battery,
    Usb,
    UsbInternal,
    Fan,
    Pci,
    Display,
    Camera,
    Keyboard,
    Touchpad,
    Speaker,
    Microphone,
    /// Slot-only kind: a cable connector on the board (eDP, battery...).
    Connector,
    /// Slot-only kind: a PCIe expansion slot (accepts GPUs, add-in cards, ...).
    Pcie,
    /// Slot-only kind: an Ethernet jack on the board's edge, showing the link
    /// and activity of the LAN controller it is wired to (`link`).
    Rj45,
    /// What powers the machine: a power source (laptops, mini PCs, dev
    /// boards) or a power supply unit (desktops, servers).
    Power,
}

impl Kind {
    pub const ALL: [Kind; 24] = [
        Kind::Board,
        Kind::Cpu,
        Kind::Gpu,
        Kind::Dimm,
        Kind::Ssd,
        Kind::Wifi,
        Kind::Ethernet,
        Kind::Chipset,
        Kind::Bios,
        Kind::Battery,
        Kind::Usb,
        Kind::UsbInternal,
        Kind::Fan,
        Kind::Pci,
        Kind::Display,
        Kind::Camera,
        Kind::Keyboard,
        Kind::Touchpad,
        Kind::Speaker,
        Kind::Microphone,
        Kind::Connector,
        Kind::Pcie,
        Kind::Rj45,
        Kind::Power,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Board => "board",
            Kind::Cpu => "cpu",
            Kind::Gpu => "gpu",
            Kind::Dimm => "dimm",
            Kind::Ssd => "ssd",
            Kind::Wifi => "wifi",
            Kind::Ethernet => "ethernet",
            Kind::Chipset => "chipset",
            Kind::Bios => "bios",
            Kind::Battery => "battery",
            Kind::Usb => "usb",
            Kind::UsbInternal => "usb-internal",
            Kind::Fan => "fan",
            Kind::Pci => "pci",
            Kind::Display => "display",
            Kind::Camera => "camera",
            Kind::Keyboard => "keyboard",
            Kind::Touchpad => "touchpad",
            Kind::Speaker => "speaker",
            Kind::Microphone => "microphone",
            Kind::Connector => "connector",
            Kind::Pcie => "pcie",
            Kind::Rj45 => "rj45",
            Kind::Power => "power",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL
            .iter()
            .copied()
            .find(|k| k.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// One detected (or expected-but-empty) piece of hardware.
#[derive(Clone, Debug)]
pub struct Part {
    /// Stable key, e.g. "cpu0", "dimm1", "usb:1-2", "nvme:nvme0".
    pub id: String,
    pub kind: Kind,
    /// Human name, e.g. "Intel Core i7-9750H".
    pub title: String,
    /// Very short label for tiny drawings, e.g. "16G DDR4".
    pub tag: String,
    /// False for things like empty DIMM slots reported by SMBIOS.
    pub present: bool,
    pub temp: Option<f64>,
    /// Short live values shown on/under the drawing.
    pub metrics: Vec<String>,
    /// Everything we know, shown in the detail panel.
    pub props: Vec<(String, String)>,
    /// Keys a board layout slot can bind to (DIMM locator, USB root port, PCI address...).
    pub bind: Vec<String>,
    /// Fill level 0..1: battery charge, screen brightness.
    pub level: Option<f64>,
    pub rpm: Option<u32>,
    /// Physical form factor when known: "SODIMM", "DIMM", "CAMM2"...
    pub form: Option<String>,
    /// Per physical core (CPUs).
    pub cores: Vec<Core>,
    /// Live I/O in bytes/s, for activity LEDs: (read, write) for drives,
    /// (received, sent) for network cards.
    pub io: Option<(f64, f64)>,
    /// Network link: (up, speed in Mb/s or 0 when unknown), for port LEDs.
    pub link: Option<(bool, u32)>,
    /// Power input: plugged in (true) or not; None when unknown.
    pub online: Option<bool>,
    /// How busy it is, 0..1: CPU and GPU utilization.
    pub load: Option<f64>,
    /// (used, total) in bytes: RAM in use, VRAM, drive space.
    pub usage: Option<(f64, f64)>,
    /// What the interface can move, (in, out) bytes/s, for `io` bars: a
    /// drive's PCIe link, SATA or USB speed (the same both ways); a Wi-Fi
    /// link's receive and send rates. (Ethernet uses `link`.)
    pub io_max: Option<(f64, f64)>,
    /// The size people know it by, bytes, for the total in usage bars: a
    /// drive's capacity (128 GB, not the 116 GiB of its filesystem), the RAM
    /// installed (32 GB, not the 30.x GiB the OS can use).
    pub capacity: Option<f64>,
    /// Fan PWM duty cycle, 0..1.
    pub pwm: Option<f64>,
    /// Memory (configured, rated) speed in MT/s.
    pub speed: Option<(f64, f64)>,
}

#[derive(Clone, Debug, Default)]
pub struct Core {
    pub id: u32,
    pub temp: Option<f64>,
    pub ghz: Option<f64>,
    pub load: Option<f64>,
}

impl Part {
    pub fn new(id: impl Into<String>, kind: Kind, title: impl Into<String>) -> Self {
        Part {
            id: id.into(),
            kind,
            title: title.into(),
            tag: String::new(),
            present: true,
            temp: None,
            metrics: Vec::new(),
            props: Vec::new(),
            bind: Vec::new(),
            level: None,
            rpm: None,
            form: None,
            cores: Vec::new(),
            io: None,
            link: None,
            online: None,
            load: None,
            usage: None,
            io_max: None,
            capacity: None,
            pwm: None,
            speed: None,
        }
    }

    pub fn prop(&mut self, k: &str, v: impl Display) {
        let v = v.to_string();
        if !v.trim().is_empty() {
            self.props.push((k.to_string(), v));
        }
    }

    pub fn opt<T: Display>(&mut self, k: &str, v: Option<T>) {
        if let Some(v) = v {
            self.prop(k, v);
        }
    }

    pub fn metric(&mut self, s: impl Into<String>) {
        let s = s.into();
        if !s.is_empty() {
            self.metrics.push(s);
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub parts: Vec<Part>,
    pub dmi: HashMap<String, String>,
    pub notes: Vec<String>,
}

impl Snapshot {
    pub fn is_laptop(&self) -> bool {
        let chassis: u32 = self
            .dmi
            .get("chassis_type")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let small_ram = self.parts.iter().any(|p| {
            p.kind == Kind::Dimm
                && p.form.as_deref().map_or(false, |f| {
                    let f = f.to_uppercase();
                    f.contains("SODIMM") || f.contains("CAMM")
                })
        });
        matches!(chassis, 8 | 9 | 10 | 14 | 30 | 31 | 32)
            || small_ram
            || self.parts.iter().any(|p| p.kind == Kind::Battery)
    }
}

pub fn fmt_temp(t: f64) -> String {
    format!("{:.0}°C", t)
}

/// Binary units (RAM).
pub fn fmt_bin(b: u64) -> String {
    let g = b as f64 / (1u64 << 30) as f64;
    if g >= 1.0 {
        if (g - g.round()).abs() < 0.05 {
            format!("{:.0} GiB", g)
        } else {
            format!("{:.1} GiB", g)
        }
    } else {
        format!("{:.0} MiB", b as f64 / (1u64 << 20) as f64)
    }
}

/// Decimal units (disks).
pub fn fmt_dec(b: u64) -> String {
    let b = b as f64;
    if b >= 1e12 {
        format!("{:.1} TB", b / 1e12)
    } else if b >= 1e9 {
        format!("{:.0} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

pub fn fmt_rate(bps: f64) -> String {
    if bps >= 1e9 {
        format!("{:.1}G/s", bps / 1e9)
    } else if bps >= 1e6 {
        format!("{:.1}M/s", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.0}K/s", bps / 1e3)
    } else {
        format!("{:.0}B/s", bps)
    }
}

pub fn fmt_hours(h: f64) -> String {
    let m = (h * 60.0).round() as u64;
    format!("{}h {:02}m", m / 60, m % 60)
}

/// A version as people write it: "1.04" -> "v1.04"; "F.42" stays.
pub fn vee(v: &str) -> String {
    if v.starts_with(|c: char| c.is_ascii_digit()) {
        format!("v{v}")
    } else {
        v.to_string()
    }
}

/// Phones and tablets by USB vendor:product ID, for drawing them as what
/// they are (`form` "phone"/"tablet") and naming them. Apple has used one ID
/// (05ac:12a8) for every iPhone since the 5, so it can't say which model.
pub fn known_usb(vid: &str, pid: &str) -> Option<(&'static str, &'static str)> {
    let (vid, pid) = (vid.to_ascii_lowercase(), pid.to_ascii_lowercase());
    let phone = |n| Some((n, "phone"));
    let tablet = |n| Some((n, "tablet"));
    match (vid.as_str(), pid.as_str()) {
        ("05ac", "1290") => phone("iPhone"),
        ("05ac", "1292") => phone("iPhone 3G"),
        ("05ac", "1294") => phone("iPhone 3GS"),
        ("05ac", "1297" | "129c") => phone("iPhone 4"),
        ("05ac", "12a0") => phone("iPhone 4S"),
        ("05ac", "12a8") => phone("iPhone"),
        ("05ac", "1291" | "1293" | "1299" | "129e" | "12aa") => phone("iPod touch"),
        (
            "05ac",
            "129a" | "129f" | "12a2" | "12a3" | "12a4" | "12a5" | "12a6" | "12a9" | "12ab",
        ) => tablet("iPad"),
        // Pixel / Nexus: MTP, PTP, with or without ADB
        ("18d1", "4ee1" | "4ee2" | "4ee5" | "4ee6" | "4ee7" | "4ee9") => phone("Pixel"),
        ("04e8", "6860" | "6865" | "6866" | "685c" | "685d") => phone("Galaxy"),
        ("2a70", _) => phone("OnePlus"),
        ("2717", "ff08" | "ff10" | "ff18" | "ff40" | "ff48" | "ff68") => phone("Xiaomi"),
        ("22b8", "2e81" | "2e82" | "2e83" | "2e84" | "2e76") => phone("Motorola"),
        ("12d1", "107e" | "1082" | "1083") => phone("Huawei"),
        ("0fce", _) => phone("Xperia"),
        ("1004", "631c" | "631d" | "633e" | "6344") => phone("LG"),
        ("2a45", _) => phone("Meizu"),
        ("0bb4", "0c02" | "0c87" | "0ff9") => phone("HTC"),
        _ => None,
    }
}

/// A graphics card's memory type, by model name: drivers don't report it
/// (NVML and amdgpu sysfs have no field for it), but each model ships with
/// one. None when unknown, or for integrated GPUs (they use system RAM).
pub fn vram_type(name: &str) -> Option<&'static str> {
    let n = name.to_ascii_uppercase();
    let laptop = ["LAPTOP", "MAX-Q", "MOBILE", "MAX-P"]
        .iter()
        .any(|w| n.contains(w));
    // the model number after a prefix: "RTX 3080 TI" -> (3080, true)
    let num = |prefix: &str| -> Option<(u32, bool)> {
        let rest = n.split(prefix).nth(1)?.trim_start();
        let d: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let ti = rest[d.len()..].trim_start().starts_with("TI");
        Some((d.parse().ok()?, ti))
    };
    if let Some((m, ti)) = num("RTX ").or_else(|| num("RTX")) {
        return Some(match m {
            5000.. => "GDDR7",
            // the desktop high end got GDDR6X; laptops kept GDDR6
            3080 | 3090 | 4070 | 4080 | 4090 if !laptop => "GDDR6X",
            3070 if ti && !laptop => "GDDR6X",
            _ => "GDDR6",
        });
    }
    if let Some((m, ti)) = num("GTX ") {
        return Some(match m {
            1080 => "GDDR5X",
            1660 if ti => "GDDR6",
            1650 | 1660 if n.contains("SUPER") => "GDDR6",
            _ => "GDDR5",
        });
    }
    if n.contains("VEGA") || n.contains("RADEON VII") || n.contains("FURY") {
        return Some("HBM2");
    }
    if let Some((m, _)) = num("RX ") {
        return Some(match m {
            400..=599 => "GDDR5",
            5000..=9999 => "GDDR6",
            _ => return None,
        });
    }
    if n.contains("ARC A") || n.contains("ARC B") {
        return Some("GDDR6");
    }
    if let Some((m, _)) = num("MX") {
        return Some(if m == 450 { "GDDR6" } else { "GDDR5" });
    }
    None
}

#[cfg(test)]
mod usb_tests {
    #[test]
    fn iphones_and_pixels_are_phones() {
        assert_eq!(super::known_usb("05ac", "12a8"), Some(("iPhone", "phone")));
        assert_eq!(super::known_usb("05AC", "12AB"), Some(("iPad", "tablet")));
        assert_eq!(super::known_usb("18d1", "4ee2").map(|k| k.1), Some("phone"));
        assert_eq!(super::known_usb("046d", "c52b"), None);
    }

    #[test]
    fn vram_types_by_model() {
        use super::vram_type as v;
        assert_eq!(
            v("NVIDIA GeForce RTX 2080 with Max-Q Design"),
            Some("GDDR6")
        );
        assert_eq!(v("NVIDIA GeForce RTX 3080"), Some("GDDR6X"));
        assert_eq!(v("NVIDIA GeForce RTX 3080 Laptop GPU"), Some("GDDR6"));
        assert_eq!(v("NVIDIA GeForce GTX 1060 6GB"), Some("GDDR5"));
        assert_eq!(v("NVIDIA GeForce GTX 1660 Ti"), Some("GDDR6"));
        assert_eq!(v("NVIDIA GeForce RTX 5090"), Some("GDDR7"));
        assert_eq!(v("AMD Radeon RX 6800 XT"), Some("GDDR6"));
        assert_eq!(v("Intel UHD Graphics 630"), None);
    }
}
