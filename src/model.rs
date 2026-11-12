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
}

impl Kind {
    pub const ALL: [Kind; 23] = [
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
