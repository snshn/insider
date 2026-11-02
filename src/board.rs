//! Board layout definitions (TOML) and matching detected parts to slots.

use crate::model::{Kind, Part, Snapshot};
use serde::Deserialize;
use std::path::{Path, PathBuf};

const BUILTIN: &[(&str, &str)] = &[
    ("stock-laptop", include_str!("../boards/stock-laptop.toml")),
    ("stock-atx", include_str!("../boards/stock-atx.toml")),
];

#[derive(Deserialize, Clone, Debug)]
pub struct RectDef {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

/// All given fields must match (case-insensitive substring of the DMI value).
#[derive(Deserialize, Clone, Debug, Default)]
pub struct MatchDef {
    pub sys_vendor: Option<String>,
    pub product_name: Option<String>,
    pub board_vendor: Option<String>,
    pub board_name: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
pub struct SlotDef {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub accepts: Vec<String>,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    /// Soldered parts stay on the board in exploded view.
    #[serde(default)]
    pub soldered: bool,
    /// Slot takes every remaining part of its kinds (e.g. internal USB devices).
    #[serde(default)]
    pub multi: bool,
    /// Only accept parts that expose one of these keys (DIMM locator, USB root port, PCI address...).
    #[serde(default)]
    pub bind: Vec<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// Side with the contacts (DIMM, M.2): "left", "right", "top" or "bottom".
    #[serde(default)]
    pub edge: Option<String>,
    /// Id of a connector slot this one is cabled to (screen -> eDP); drawn when exploded.
    #[serde(default)]
    pub link: Option<String>,
    /// Soldered area that takes every part no other slot wanted (drawn as chips).
    #[serde(default)]
    pub catchall: bool,
    #[serde(skip)]
    pub k: Option<Kind>,
    #[serde(skip)]
    pub acc: Vec<Kind>,
}

impl SlotDef {
    pub fn kind(&self) -> Kind {
        self.k.unwrap_or(Kind::Pci)
    }
    pub fn accepts_kind(&self, k: Kind) -> bool {
        self.acc.contains(&k)
    }
    pub fn label(&self) -> String {
        self.label
            .clone()
            .unwrap_or_else(|| self.kind().name().to_uppercase())
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct BoardDef {
    pub name: String,
    /// "laptop" or "desktop".
    #[serde(default = "default_form")]
    pub form: String,
    pub width: u16,
    pub height: u16,
    /// The PCB; peripherals are laid out around it.
    #[serde(default)]
    pub pcb: Option<RectDef>,
    /// PCB color, "#rrggbb".
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub generic: bool,
    #[serde(default, rename = "match")]
    pub matches: Vec<MatchDef>,
    #[serde(default, rename = "slot")]
    pub slots: Vec<SlotDef>,
    #[serde(skip)]
    pub id: String,
    #[serde(skip)]
    pub source: String,
}

fn default_form() -> String {
    "desktop".into()
}

impl BoardDef {
    fn validate(&mut self) -> Result<(), String> {
        if self.width == 0 || self.height == 0 {
            return Err("width and height must be > 0".into());
        }
        for s in &mut self.slots {
            let k = Kind::parse(&s.kind)
                .ok_or_else(|| format!("slot {}: unknown kind {:?}", s.id, s.kind))?;
            s.k = Some(k);
            s.acc = if s.accepts.is_empty() {
                match k {
                    Kind::Pcie => vec![Kind::Gpu, Kind::Pci, Kind::Ssd, Kind::Wifi, Kind::Ethernet],
                    Kind::Connector => vec![],
                    k => vec![k],
                }
            } else {
                s.accepts
                    .iter()
                    .map(|a| {
                        Kind::parse(a)
                            .ok_or_else(|| format!("slot {}: unknown kind {a:?} in accepts", s.id))
                    })
                    .collect::<Result<_, _>>()?
            };
            if s.w == 0 || s.h == 0 || s.x + s.w > self.width || s.y + s.h > self.height {
                return Err(format!(
                    "slot {} lies outside the {}x{} board",
                    s.id, self.width, self.height
                ));
            }
        }
        for s in &self.slots {
            if let Some(l) = &s.link {
                if !self.slots.iter().any(|t| &t.id == l) {
                    return Err(format!("slot {}: link to unknown slot {l:?}", s.id));
                }
            }
        }
        Ok(())
    }

    fn score(&self, dmi: &std::collections::HashMap<String, String>) -> usize {
        let has = |k: &str, want: &Option<String>| -> Option<bool> {
            let w = want.as_ref()?;
            let v = dmi.get(k).map(|s| s.to_lowercase()).unwrap_or_default();
            Some(v.contains(&w.to_lowercase()))
        };
        self.matches
            .iter()
            .filter_map(|m| {
                let checks = [
                    has("sys_vendor", &m.sys_vendor),
                    has("product_name", &m.product_name),
                    has("board_vendor", &m.board_vendor),
                    has("board_name", &m.board_name),
                ];
                let given: Vec<bool> = checks.into_iter().flatten().collect();
                (!given.is_empty() && given.iter().all(|b| *b)).then_some(given.len())
            })
            .max()
            .unwrap_or(0)
    }
}

pub fn parse(id: &str, source: &str, text: &str) -> Result<BoardDef, String> {
    let mut b: BoardDef = toml::from_str(text).map_err(|e| format!("{source}: {e}"))?;
    b.id = id.to_string();
    b.source = source.to_string();
    b.validate().map_err(|e| format!("{source}: {e}"))?;
    Ok(b)
}

pub fn board_dirs() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        v.push(PathBuf::from(x).join("anatomy/boards"));
    }
    if let Ok(h) = std::env::var("HOME") {
        v.push(PathBuf::from(h).join(".config/anatomy/boards"));
    }
    if let Ok(u) = std::env::var("SUDO_USER") {
        v.push(PathBuf::from(format!("/home/{u}/.config/anatomy/boards")));
    }
    v.push(PathBuf::from("/usr/share/anatomy/boards"));
    v.push(PathBuf::from("boards"));
    v.dedup();
    v
}

fn load_file(p: &Path) -> Result<BoardDef, String> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let id = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    parse(&id, &p.display().to_string(), &text)
}

/// Built-ins first; files with the same id override them.
pub fn load_all() -> (Vec<BoardDef>, Vec<String>) {
    let mut boards: Vec<BoardDef> = Vec::new();
    let mut errs = Vec::new();
    let add = |b: BoardDef, boards: &mut Vec<BoardDef>| {
        if let Some(i) = boards.iter().position(|x| x.id == b.id) {
            boards[i] = b;
        } else {
            boards.push(b);
        }
    };
    for (id, text) in BUILTIN {
        match parse(id, "built-in", text) {
            Ok(b) => add(b, &mut boards),
            Err(e) => errs.push(e),
        }
    }
    for d in board_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map_or(false, |x| x == "toml"))
            .collect();
        files.sort();
        for f in files {
            match load_file(&f) {
                Ok(b) => add(b, &mut boards),
                Err(e) => errs.push(e),
            }
        }
    }
    (boards, errs)
}

/// Pick a layout: explicit request, best DMI match, or a stock board.
pub fn choose(
    boards: &mut Vec<BoardDef>,
    snap: &Snapshot,
    requested: Option<&str>,
) -> Result<(usize, String), String> {
    if let Some(r) = requested {
        // For authoring layouts: load a file directly.
        let b = load_file(Path::new(r))?;
        boards.push(b);
        return Ok((boards.len() - 1, "loaded with --board".into()));
    }
    let best = boards
        .iter()
        .enumerate()
        .map(|(i, b)| (i, b.score(&snap.dmi)))
        .filter(|(_, s)| *s > 0)
        .max_by_key(|(_, s)| *s);
    if let Some((i, _)) = best {
        return Ok((i, "matched by DMI".into()));
    }
    let want = if snap.is_laptop() {
        "stock-laptop"
    } else {
        "stock-atx"
    };
    let i = boards.iter().position(|b| b.id == want).unwrap_or(0);
    let who = [snap.dmi.get("board_vendor"), snap.dmi.get("board_name")]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    Ok((
        i,
        format!(
            "generic stand-in: no layout for \"{}\" yet",
            if who.is_empty() {
                "this board".into()
            } else {
                who
            }
        ),
    ))
}

#[derive(Clone, Debug, Default)]
pub struct Assignment {
    /// Part indices per slot (same order as BoardDef::slots).
    pub slots: Vec<Vec<usize>>,
    /// Present parts that found no slot.
    pub unplaced: Vec<usize>,
}

pub fn assign(b: &BoardDef, parts: &[Part]) -> Assignment {
    let mut used: Vec<bool> = parts.iter().map(|p| p.kind == Kind::Board).collect();
    let mut slots = vec![Vec::new(); b.slots.len()];
    // 1. explicit bindings
    for (si, s) in b.slots.iter().enumerate() {
        if s.bind.is_empty() {
            continue;
        }
        for (pi, p) in parts.iter().enumerate() {
            if used[pi] || !s.accepts_kind(p.kind) {
                continue;
            }
            if p.bind
                .iter()
                .any(|k| s.bind.iter().any(|b| b.eq_ignore_ascii_case(k)))
            {
                slots[si].push(pi);
                used[pi] = true;
                if !s.multi {
                    break;
                }
            }
        }
    }
    // 2. single slots, first come first served in definition order
    for (si, s) in b.slots.iter().enumerate() {
        if !s.bind.is_empty() || s.multi {
            continue;
        }
        if let Some(pi) = (0..parts.len()).find(|&pi| !used[pi] && s.accepts_kind(parts[pi].kind)) {
            slots[si].push(pi);
            used[pi] = true;
        }
    }
    // 3. multi slots soak up the rest
    for (si, s) in b.slots.iter().enumerate() {
        if !s.bind.is_empty() || !s.multi || s.catchall {
            continue;
        }
        for pi in 0..parts.len() {
            if !used[pi] && s.accepts_kind(parts[pi].kind) {
                slots[si].push(pi);
                used[pi] = true;
            }
        }
    }
    // 4. leftovers get soldered onto the board, if the layout has room for them
    for (si, s) in b.slots.iter().enumerate() {
        if !s.catchall {
            continue;
        }
        for pi in 0..parts.len() {
            let k = parts[pi].kind;
            if !used[pi] && parts[pi].present && !matches!(k, Kind::Usb | Kind::Display) {
                slots[si].push(pi);
                used[pi] = true;
            }
        }
    }
    let unplaced = (0..parts.len())
        .filter(|&i| !used[i] && parts[i].present)
        .collect();
    Assignment { slots, unplaced }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_parse() {
        for (id, text) in BUILTIN {
            parse(id, "built-in", text).unwrap();
        }
    }
}
