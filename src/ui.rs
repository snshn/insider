//! Drawing the board, assembled and exploded, plus input handling.

use crate::board::{assign, Assignment, BoardDef, SlotDef};
use crate::model::{fmt_temp, Kind, Part, Snapshot};
use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use std::time::Instant;

// ---------------------------------------------------------------- palette

/// Borders, arrows, leader lines.
const LG: Color = Color::Rgb(165, 165, 172);
/// Selected / active.
const HI: Color = Color::White;
const DIMTXT: Color = Color::Rgb(140, 140, 150);
/// Gold is reserved for contacts and circuitry.
const GOLD: Color = Color::Rgb(212, 175, 55);
const GREEN_PCB: Color = Color::Rgb(22, 92, 54);
const BLACK_PCB: Color = Color::Rgb(26, 26, 30);
const METAL: Color = Color::Rgb(160, 162, 168);
const SHIELD: Color = Color::Rgb(178, 180, 186);
/// SoC package substrate (a slot with slots inside it)
const PACKAGE: Color = Color::Rgb(30, 31, 34);
/// Width of the details panel; entries stack key over value to fit
const PANEL_W: u16 = 32;
const CHIP: Color = Color::Rgb(18, 18, 21);
/// Drive activity LED: blue, like the activity light on many modern drives.
const LED_DISK: Color = Color::Rgb(40, 140, 255);
/// RJ45 link LED: gigabit and faster / slower links.
const LED_LINK: Color = Color::Rgb(40, 225, 80);
const LED_LINK_SLOW: Color = Color::Rgb(255, 160, 20);
/// RJ45 activity LED.
const LED_ACT: Color = Color::Rgb(255, 205, 40);

// ---------------------------------------------------------------- app state

pub struct App {
    pub boards: Vec<BoardDef>,
    pub board_idx: usize,
    pub load_errors: Vec<String>,
    pub snap: Snapshot,
    asg: Assignment,
    pub exploded: bool,
    details: bool,
    help: bool,
    selected: Option<String>,
    scroll: u16,
    max_scroll: u16,
    hits: Vec<(Rect, String)>,
    /// where the details panel is, so clicks in it don't count as "outside"
    panel: Option<Rect>,
    pub quit: bool,
    start: Instant,
}

impl App {
    pub fn new(
        boards: Vec<BoardDef>,
        board_idx: usize,
        load_errors: Vec<String>,
        snap: Snapshot,
        exploded: bool,
    ) -> Self {
        let asg = assign(&boards[board_idx], &snap.parts);
        App {
            boards,
            board_idx,
            load_errors,
            snap,
            asg,
            exploded,
            details: false,
            help: false,
            selected: None,
            scroll: 0,
            max_scroll: 0,
            hits: Vec::new(),
            panel: None,
            quit: false,
            start: Instant::now(),
        }
    }

    pub fn set_snapshot(&mut self, s: Snapshot) {
        self.asg = assign(self.board(), &s.parts);
        self.snap = s;
    }

    fn board(&self) -> &BoardDef {
        &self.boards[self.board_idx]
    }

    fn select(&mut self, key: String) {
        if self.selected.as_deref() != Some(key.as_str()) {
            self.scroll = 0;
        }
        self.selected = Some(key);
    }

    fn is_sel(&self, key: &str) -> bool {
        self.selected.as_deref() == Some(key)
    }

    fn secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Selectable keys in drawing order, without duplicates.
    fn order(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for (_, k) in &self.hits {
            if !v.contains(k) {
                v.push(k.clone());
            }
        }
        v
    }

    pub fn on_key(&mut self, code: KeyCode, mods: KeyModifiers) {
        match code {
            KeyCode::Char('c') if mods.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => {
                if self.help {
                    self.help = false
                } else if self.details {
                    self.details = false
                } else {
                    self.selected = None
                }
            }
            KeyCode::Char('?') | KeyCode::F(1) => self.help = !self.help,
            KeyCode::Char('e') | KeyCode::Char('x') | KeyCode::Char(' ') => {
                self.exploded = !self.exploded
            }
            KeyCode::Enter | KeyCode::Char('d') | KeyCode::Char('i') => {
                if self.selected.is_none() {
                    self.select("board".into());
                }
                self.details = !self.details;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let o = self.order();
                if o.is_empty() {
                    return;
                }
                let cur = self
                    .selected
                    .as_ref()
                    .and_then(|s| o.iter().position(|k| k == s));
                let n = o.len();
                let next = match (cur, code == KeyCode::BackTab) {
                    (None, false) => 0,
                    (None, true) => n - 1,
                    (Some(i), false) => (i + 1) % n,
                    (Some(i), true) => (i + n - 1) % n,
                };
                self.select(o[next].clone());
            }
            KeyCode::Left | KeyCode::Char('h') => self.nav(-1, 0),
            KeyCode::Right | KeyCode::Char('l') => self.nav(1, 0),
            KeyCode::Up | KeyCode::Char('k') => self.nav(0, -1),
            KeyCode::Down | KeyCode::Char('j') => self.nav(0, 1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(5).min(self.max_scroll),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            _ => {}
        }
    }

    pub fn on_click(&mut self, x: u16, y: u16) {
        let hit = self
            .hits
            .iter()
            .rev()
            .find(|(r, _)| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
            .map(|(_, k)| k.clone());
        if let Some(k) = hit {
            self.select(k);
            self.details = true;
            return;
        }
        let in_panel = self
            .panel
            .is_some_and(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        if !in_panel {
            // clicking empty space works like Esc (blur)
            self.on_key(KeyCode::Esc, KeyModifiers::NONE);
        }
    }

    pub fn on_scroll(&mut self, down: bool) {
        if self.details {
            self.scroll = if down {
                self.scroll.saturating_add(2).min(self.max_scroll)
            } else {
                self.scroll.saturating_sub(2)
            };
        }
    }

    /// Spatial navigation with arrow keys.
    fn nav(&mut self, dx: i32, dy: i32) {
        let center = |r: &Rect| {
            (
                r.x as i32 + r.width as i32 / 2,
                (r.y as i32 + r.height as i32 / 2) * 2,
            )
        };
        let cur = self
            .selected
            .as_ref()
            .and_then(|s| self.hits.iter().rev().find(|(_, k)| k == s))
            .map(|(r, k)| (center(r), k.clone()));
        let Some(((cx, cy), ck)) = cur else {
            if let Some(k) = self.order().into_iter().find(|k| k != "board") {
                self.select(k);
            }
            return;
        };
        let best = self
            .hits
            .iter()
            .filter(|(_, k)| *k != ck && k != "board")
            .filter_map(|(r, k)| {
                let (x, y) = center(r);
                let along = (x - cx) * dx + (y - cy) * dy;
                if along <= 0 {
                    return None;
                }
                let across = ((x - cx) * dy).abs() + ((y - cy) * dx).abs();
                Some((along + across * 2, k.clone()))
            })
            .min_by_key(|(d, _)| *d);
        if let Some((_, k)) = best {
            self.select(k);
        }
    }
}

// ---------------------------------------------------------------- canvas helpers

struct Bset {
    tl: char,
    tr: char,
    bl: char,
    br: char,
    h: char,
    v: char,
}

const ROUND: Bset = Bset {
    tl: '╭',
    tr: '╮',
    bl: '╰',
    br: '╯',
    h: '─',
    v: '│',
};
const PLAIN: Bset = Bset {
    tl: '┌',
    tr: '┐',
    bl: '└',
    br: '┘',
    h: '─',
    v: '│',
};
const HEAVY: Bset = Bset {
    tl: '┏',
    tr: '┓',
    bl: '┗',
    br: '┛',
    h: '━',
    v: '┃',
};
const DASH: Bset = Bset {
    tl: '┌',
    tr: '┐',
    bl: '└',
    br: '┘',
    h: '╌',
    v: '╎',
};
const CHIPB: Bset = Bset {
    tl: '╒',
    tr: '╕',
    bl: '╘',
    br: '╛',
    h: '═',
    v: '│',
};

struct Cv<'a> {
    buf: &'a mut Buffer,
    clip: Rect,
}

impl Cv<'_> {
    fn inside(&self, x: i32, y: i32) -> bool {
        let c = self.clip;
        x >= c.x as i32
            && y >= c.y as i32
            && x < c.x as i32 + c.width as i32
            && y < c.y as i32 + c.height as i32
    }

    fn put(&mut self, x: i32, y: i32, ch: char, st: Style) {
        if !self.inside(x, y) {
            return;
        }
        if let Some(cell) = self.buf.cell_mut((x as u16, y as u16)) {
            cell.set_char(ch);
            cell.set_style(st);
        }
    }

    fn restyle(&mut self, r: Rect, st: Style) {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                if self.inside(x as i32, y as i32) {
                    if let Some(cell) = self.buf.cell_mut((x, y)) {
                        cell.set_style(st);
                    }
                }
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, maxw: i32, st: Style) {
        for (i, ch) in trunc(s, maxw.max(0) as usize).chars().enumerate() {
            self.put(x + i as i32, y, ch, st);
        }
    }

    fn ctext(&mut self, r: Rect, y: i32, s: &str, st: Style) {
        let w = r.width as usize;
        let t = trunc(s, w);
        let n = t.chars().count();
        self.text(r.x as i32 + ((w - n) / 2) as i32, y, &t, w as i32, st);
    }

    fn rtext(&mut self, r: Rect, y: i32, s: &str, st: Style) {
        let n = s.chars().count() as i32;
        self.text(r.x as i32 + r.width as i32 - n, y, s, n, st);
    }

    fn fill(&mut self, r: Rect, ch: char, st: Style) {
        for y in r.y..r.y + r.height {
            for x in r.x..r.x + r.width {
                self.put(x as i32, y as i32, ch, st);
            }
        }
    }

    fn bg(&mut self, r: Rect, c: Color) {
        self.fill(r, ' ', Style::new().bg(c));
    }

    fn frame(&mut self, r: Rect, b: &Bset, st: Style) {
        if r.width < 2 || r.height < 2 {
            return;
        }
        let (x0, y0) = (r.x as i32, r.y as i32);
        let (x1, y1) = (x0 + r.width as i32 - 1, y0 + r.height as i32 - 1);
        for x in x0 + 1..x1 {
            self.put(x, y0, b.h, st);
            self.put(x, y1, b.h, st);
        }
        for y in y0 + 1..y1 {
            self.put(x0, y, b.v, st);
            self.put(x1, y, b.v, st);
        }
        self.put(x0, y0, b.tl, st);
        self.put(x1, y0, b.tr, st);
        self.put(x0, y1, b.bl, st);
        self.put(x1, y1, b.br, st);
    }

    /// Selection marker for shapes without a border: white corner brackets.
    fn corners(&mut self, r: Rect) {
        let st = Style::new().fg(HI).add_modifier(Modifier::BOLD);
        if r.width < 2 || r.height < 2 {
            self.restyle(r, Style::new().add_modifier(Modifier::REVERSED));
            return;
        }
        let (x0, y0) = (r.x as i32, r.y as i32);
        let (x1, y1) = (x0 + r.width as i32 - 1, y0 + r.height as i32 - 1);
        self.put(x0, y0, '┌', st);
        self.put(x1, y0, '┐', st);
        self.put(x0, y1, '└', st);
        self.put(x1, y1, '┘', st);
    }
}

fn trunc(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w {
        s.to_string()
    } else if w == 0 {
        String::new()
    } else {
        let mut t: String = s.chars().take(w - 1).collect();
        t.push('…');
        t
    }
}

fn shrink(r: Rect) -> Rect {
    Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    )
}

/// Greedy-pack short metrics into lines of width w.
fn pack(items: &[String], w: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for it in items {
        match out.last_mut() {
            Some(l) if l.chars().count() + 3 + it.chars().count() <= w => {
                l.push_str(" · ");
                l.push_str(it);
            }
            _ => out.push(it.clone()),
        }
    }
    out
}

pub fn parse_color(s: &str) -> Option<Color> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn lighten(c: Color, n: u8) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            r.saturating_add(n),
            g.saturating_add(n),
            b.saturating_add(n),
        ),
        c => c,
    }
}

fn tcolor(t: f64) -> Color {
    if t < 50.0 {
        Color::Rgb(110, 220, 140)
    } else if t < 70.0 {
        Color::Rgb(235, 210, 90)
    } else if t < 85.0 {
        Color::Rgb(255, 150, 50)
    } else {
        Color::Rgb(255, 70, 60)
    }
}

fn edge_style(sel: bool) -> Style {
    if sel {
        Style::new().fg(HI).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(LG)
    }
}

// ---------------------------------------------------------------- scaling

/// A slot's layout geometry (x, y, w, h), the key for where it's placed.
type Geom = (u16, u16, u16, u16);

struct Xf {
    ox: i32,
    oy: i32,
    s: f32,
    /// Where each slot really goes, by its layout geometry: scaled, with
    /// connectors at their natural size and a gap around every component.
    placed: Vec<(Geom, Rect)>,
}

impl Xf {
    fn rect(&self, x: u16, y: u16, w: u16, h: u16) -> Rect {
        let f = |v: u16| (v as f32 * self.s).round() as i32;
        let (x0, y0) = (self.ox + f(x), self.oy + f(y));
        let (x1, y1) = (self.ox + f(x + w), self.oy + f(y + h));
        Rect::new(
            x0.max(0) as u16,
            y0.max(0) as u16,
            (x1 - x0).max(1) as u16,
            (y1 - y0).max(1) as u16,
        )
    }
    fn slot(&self, s: &SlotDef) -> Rect {
        let key = (s.x, s.y, s.w, s.h);
        self.placed
            .iter()
            .find(|(k, _)| *k == key)
            .map_or_else(|| self.rect(s.x, s.y, s.w, s.h), |(_, r)| *r)
    }
}

/// Whether slot `t` sits inside another slot (a package: iGPU in the CPU).
fn nested_in<'a>(b: &'a BoardDef, t: &SlotDef) -> Option<&'a SlotDef> {
    b.slots.iter().find(|s| {
        !std::ptr::eq(t, *s)
            && (s.x, s.y, s.w, s.h) != (t.x, t.y, t.w, t.h)
            && t.x >= s.x
            && t.y >= s.y
            && t.x + t.w <= s.x + s.w
            && t.y + t.h <= s.y + s.h
    })
}

/// Cells between two spans (negative when they overlap).
fn span_gap(a0: i32, a1: i32, b0: i32, b1: i32) -> i32 {
    (b0 - a1).max(a0 - b1)
}

/// Lays out the slots on screen. Connectors are as small as their label
/// (they don't grow with the scale), and no two components touch: where
/// rounding (or the layout) leaves them flush, the one that's bigger along
/// that side gives up a cell, so a RAM module never reaches into the next
/// slot and an SSD never brushes the CPU.
fn place(b: &BoardDef, xf: &Xf) -> Vec<(Geom, Rect)> {
    let mut out: Vec<(Geom, Rect, bool)> = Vec::new();
    for s in &b.slots {
        let key = (s.x, s.y, s.w, s.h);
        if out.iter().any(|o| o.0 == key) {
            continue;
        }
        let mut r = xf.rect(s.x, s.y, s.w, s.h);
        if s.kind() == Kind::Connector {
            let w = (s.label().chars().count() as u16 + 2).max(4);
            let (cx, cy) = (
                r.x as i32 + r.width as i32 / 2,
                r.y as i32 + r.height as i32 / 2,
            );
            r = Rect::new(
                (cx - w as i32 / 2).max(0) as u16,
                (cy - 1).max(0) as u16,
                w,
                2,
            );
        }
        out.push((key, r, nested_in(b, s).is_some()));
    }
    let n = out.len();
    for i in 0..n {
        for j in i + 1..n {
            if out[i].2 || out[j].2 {
                continue;
            }
            let (a, z) = (out[i].1, out[j].1);
            let e = |r: Rect| {
                (
                    r.x as i32,
                    r.y as i32,
                    (r.x + r.width) as i32,
                    (r.y + r.height) as i32,
                )
            };
            let ((ax0, ay0, ax1, ay1), (zx0, zy0, zx1, zy1)) = (e(a), e(z));
            let (gx, gy) = (span_gap(ax0, ax1, zx0, zx1), span_gap(ay0, ay1, zy0, zy1));
            // apart already, or overlapping on purpose (deeper than rounding)
            if gx >= 1 || gy >= 1 || gx.max(gy) < -2 {
                continue;
            }
            let horiz = gx >= gy;
            let need = (1 - if horiz { gx } else { gy }) as u16;
            // shrink the one that's bigger along that side
            let (big, other) = if horiz {
                if a.width > z.width {
                    (i, j)
                } else {
                    (j, i)
                }
            } else if a.height > z.height {
                (i, j)
            } else {
                (j, i)
            };
            let (o, r) = (out[other].1, &mut out[big].1);
            if horiz {
                if r.width <= need {
                    continue;
                }
                if r.x < o.x {
                    r.width -= need;
                } else {
                    r.x += need;
                    r.width -= need;
                }
            } else {
                if r.height <= need {
                    continue;
                }
                if r.y < o.y {
                    r.height -= need;
                } else {
                    r.y += need;
                    r.height -= need;
                }
            }
        }
    }
    // packages: what's inside stays inside its (possibly smaller) parent
    let mut placed: Vec<(Geom, Rect)> = out.iter().map(|(k, r, _)| (*k, *r)).collect();
    for s in &b.slots {
        if let Some(p) = nested_in(b, s) {
            let pr = placed
                .iter()
                .find(|(k, _)| *k == (p.x, p.y, p.w, p.h))
                .map(|(_, r)| *r);
            if let (Some(pr), Some(c)) = (
                pr,
                placed.iter_mut().find(|(k, _)| *k == (s.x, s.y, s.w, s.h)),
            ) {
                let i = c.1.intersection(pr);
                if !i.is_empty() {
                    c.1 = i;
                }
            }
        }
    }
    placed
}

fn fit(b: &BoardDef, area: Rect, max_s: f32) -> Xf {
    let s = (area.width as f32 / b.width as f32)
        .min(area.height as f32 / b.height as f32)
        .min(max_s)
        .max(0.05);
    let w = (b.width as f32 * s).round() as i32;
    let h = (b.height as f32 * s).round() as i32;
    let mut xf = Xf {
        ox: area.x as i32 + (area.width as i32 - w) / 2,
        oy: area.y as i32 + (area.height as i32 - h) / 2,
        s,
        placed: Vec::new(),
    };
    xf.placed = place(b, &xf);
    xf
}

// ---------------------------------------------------------------- shapes

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    /// On the board: label, temperature, title, metrics.
    Board,
    /// Exploded picture of a part: shape plus its tag; details go underneath.
    Pic,
}

#[derive(PartialEq, Clone, Copy)]
enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// How to draw one thing.
struct Ctx<'a> {
    mode: Mode,
    sel: bool,
    label: &'a str,
    edge: Edge,
    /// Horizontal alignment of aspect-fitted shapes: -1 left, 0 center, 1 right.
    align: i8,
    t: f64,
    /// e.g. "2 devices" for multi slots.
    extra: Option<String>,
    /// Drawn inside a package outline (the CPU around its iGPU): no frame of
    /// its own, label and temperature on the package's border.
    packaged: bool,
}

fn edge_of(slot: Option<&SlotDef>, kind: Kind, r: Rect) -> Edge {
    match slot.and_then(|s| s.edge.as_deref()) {
        Some("left") => Edge::Left,
        Some("right") => Edge::Right,
        Some("top") => Edge::Top,
        Some("bottom") => Edge::Bottom,
        _ => {
            let tall = r.height as u32 * 2 > r.width as u32;
            match (kind, tall) {
                (Kind::Dimm, false) => Edge::Bottom,
                (Kind::Dimm, true) => Edge::Left,
                (_, false) => Edge::Left,
                (_, true) => Edge::Bottom,
            }
        }
    }
}

/// Largest rect inside r with the given width/height ratio (in cells, for a
/// horizontally lying part; rotated when the slot is taller than wide).
fn aspect(r: Rect, ratio: f32, align: i8) -> Rect {
    let tall = r.height as f32 * 2.0 > r.width as f32 * 1.0 && r.height > r.width;
    let a = if tall { 4.0 / ratio } else { ratio };
    let mut w = r.width as f32;
    let mut h = (w / a).round();
    if h > r.height as f32 {
        h = r.height as f32;
        w = (h * a).round().min(r.width as f32);
    }
    let (w, h) = (w.max(1.0) as u16, h.max(1.0) as u16);
    let x = match align {
        -1 => r.x,
        1 => r.x + r.width - w,
        _ => r.x + (r.width - w) / 2,
    };
    Rect::new(x, r.y + (r.height - h) / 2, w, h)
}

/// Splits a module into (contact strip, body).
fn contacts(m: Rect, e: Edge) -> (Rect, Rect) {
    match e {
        Edge::Left => (
            Rect::new(m.x, m.y, 1, m.height),
            Rect::new(m.x + 1, m.y, m.width.saturating_sub(1), m.height),
        ),
        Edge::Right => (
            Rect::new(m.x + m.width - 1, m.y, 1, m.height),
            Rect::new(m.x, m.y, m.width.saturating_sub(1), m.height),
        ),
        Edge::Top => (
            Rect::new(m.x, m.y, m.width, 1),
            Rect::new(m.x, m.y + 1, m.width, m.height.saturating_sub(1)),
        ),
        Edge::Bottom => (
            Rect::new(m.x, m.y + m.height - 1, m.width, 1),
            Rect::new(m.x, m.y, m.width, m.height.saturating_sub(1)),
        ),
    }
}

/// Gold edge-connector fingers, with an optional key notch.
fn pads(cv: &mut Cv, strip: Rect, bg: Color, notch: bool) {
    let gold = Style::new().fg(GOLD).bg(bg);
    if strip.height == 1 {
        let nx = strip.x + strip.width * 2 / 5;
        for x in strip.x..strip.x + strip.width {
            let ch = if notch && x == nx && strip.width >= 6 {
                ' '
            } else {
                '▮'
            };
            cv.put(x as i32, strip.y as i32, ch, gold);
        }
    } else {
        for y in strip.y..strip.y + strip.height {
            cv.put(strip.x as i32, y as i32, '≡', gold);
        }
    }
}

/// Text lines centered in a rect, first line bold.
fn text_block(cv: &mut Cv, r: Rect, lines: &[String], fg: Color) {
    if r.width < 3 || r.height == 0 {
        return;
    }
    let n = lines.len().min(r.height as usize);
    let y0 = r.y as i32 + (r.height as i32 - n as i32) / 2;
    for (i, l) in lines.iter().take(n).enumerate() {
        let st = if i == 0 {
            Style::new().fg(fg).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(fg)
        };
        cv.ctext(r, y0 + i as i32, l, st);
    }
}

fn temp_tag(cv: &mut Cv, r: Rect, t: Option<f64>) {
    if let Some(t) = t {
        let s = fmt_temp(t);
        if r.width as usize > s.chars().count() + 1 {
            cv.rtext(
                r,
                r.y as i32,
                &s,
                Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
            );
        }
    }
}

/// Short lines for shaped modules: tag first, then the metrics not already in it.
fn module_lines(p: &Part, ctx: &Ctx, w: usize) -> Vec<String> {
    let mut v = vec![if p.tag.is_empty() {
        ctx.label.to_string()
    } else {
        p.tag.clone()
    }];
    if ctx.mode == Mode::Board {
        let m: Vec<String> = p
            .metrics
            .iter()
            .filter(|m| !p.tag.contains(m.as_str()))
            .cloned()
            .collect();
        v.extend(pack(&m, w));
    }
    v
}

fn draw_dimm(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx) {
    let form = p.form.clone().unwrap_or_default();
    let camm = form.contains("CAMM");
    let ratio = if camm {
        3.4
    } else if form.contains("SO") {
        4.5
    } else if form.is_empty() {
        r.width as f32 / r.height.max(1) as f32
    } else {
        8.5
    };
    let m = aspect(r, ratio, ctx.align);
    cv.bg(m, GREEN_PCB);
    let (strip, mut body) = contacts(m, ctx.edge);
    let horiz = strip.height == 1;
    // seated in its slot the contacts are hidden: only the exploded view
    // (a module pulled out) shows them
    let seated = ctx.mode == Mode::Board;
    if seated {
        body = m;
    }
    if camm {
        // compression connector: a pad field along the edge, screw holes at its ends
        let band = if horiz {
            Rect::new(m.x + 2, strip.y, m.width.saturating_sub(4), 1)
        } else {
            Rect::new(strip.x, m.y + 1, 1, m.height.saturating_sub(2))
        };
        if !seated {
            cv.fill(band, '∷', Style::new().fg(GOLD).bg(GREEN_PCB));
        }
        let hole = Style::new().fg(METAL).bg(GREEN_PCB);
        if horiz && !seated {
            cv.put(m.x as i32, strip.y as i32, '◉', hole);
            cv.put((m.x + m.width - 1) as i32, strip.y as i32, '◉', hole);
        }
        // memory packages, flat in a row
        let rows = if horiz { body } else { body };
        for y in rows.y..rows.y + rows.height {
            for x in (rows.x + 1..rows.x + rows.width.saturating_sub(1)).step_by(5) {
                let w = 4.min(rows.x + rows.width - 1 - x);
                cv.bg(Rect::new(x, y, w, 1), CHIP);
            }
        }
    } else {
        if !seated {
            pads(cv, strip, GREEN_PCB, true);
        }
        // DRAM packages
        if horiz {
            for y in body.y..body.y + body.height {
                for x in (body.x + 1..body.x + body.width.saturating_sub(1)).step_by(4) {
                    let w = 3.min(body.x + body.width - 1 - x);
                    cv.bg(Rect::new(x, y, w, 1), CHIP);
                }
            }
        } else {
            for y in (body.y + 1..body.y + body.height.saturating_sub(1)).step_by(3) {
                let h = 2.min(body.y + body.height - 1 - y);
                cv.bg(
                    Rect::new(
                        body.x + 1.min(body.width / 3),
                        y,
                        body.width.saturating_sub(1).max(1),
                        h,
                    ),
                    CHIP,
                );
            }
        }
    }
    let mut text_r = body;
    if ctx.mode == Mode::Board && text_r.width >= 8 {
        text_r = with_bars(cv, text_r, p, 1);
    }
    let mut lines = module_lines(p, ctx, text_r.width as usize);
    if ctx.mode == Mode::Board && p.temp.is_some() && text_r.height as usize > lines.len() {
        temp_tag(cv, text_r, p.temp);
        let tr = Rect {
            y: text_r.y + 1,
            height: text_r.height - 1,
            ..text_r
        };
        lines.truncate(tr.height as usize);
        text_block(cv, tr, &lines, Color::White);
    } else {
        if let (Mode::Board, Some(t)) = (ctx.mode, p.temp) {
            lines.insert(1.min(lines.len()), fmt_temp(t));
        }
        text_block(cv, text_r, &lines, Color::White);
    }
    if ctx.sel {
        cv.corners(m);
    }
}

fn draw_m2(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx, wifi: bool) {
    // seated, a card fills its slot (the layout sized it); out of the board
    // it keeps an M.2 card's proportions
    let m = if ctx.mode == Mode::Board {
        r
    } else {
        aspect(r, if wifi { 2.8 } else { 7.3 }, ctx.align)
    };
    let pcb = if wifi { GREEN_PCB } else { BLACK_PCB };
    cv.bg(m, pcb);
    // contacts only show when the card is out of its slot (exploded view)
    let (strip, body) = if ctx.mode == Mode::Board {
        (Rect::default(), m)
    } else {
        contacts(m, ctx.edge)
    };
    if strip.width > 0 {
        pads(cv, strip, pcb, false);
    }
    // the far end: mounting half-moon (SSD) or antenna connectors (Wi-Fi)
    let (far, inner) = match ctx.edge {
        Edge::Left => (
            Rect::new(body.x + body.width - 1, body.y, 1, body.height),
            Rect::new(body.x, body.y, body.width.saturating_sub(1), body.height),
        ),
        Edge::Right => (
            Rect::new(body.x, body.y, 1, body.height),
            Rect::new(
                body.x + 1,
                body.y,
                body.width.saturating_sub(1),
                body.height,
            ),
        ),
        Edge::Top => (
            Rect::new(body.x, body.y + body.height - 1, body.width, 1),
            Rect::new(body.x, body.y, body.width, body.height.saturating_sub(1)),
        ),
        Edge::Bottom => (
            Rect::new(body.x, body.y, body.width, 1),
            Rect::new(
                body.x,
                body.y + 1,
                body.width,
                body.height.saturating_sub(1),
            ),
        ),
    };
    let silver = Style::new().fg(Color::Rgb(215, 215, 222)).bg(pcb);
    if wifi {
        // two U.FL antenna sockets
        let pts: Vec<(u16, u16)> = if far.width == 1 {
            let ys = [far.y, far.y + far.height.saturating_sub(1)];
            ys.iter().map(|y| (far.x, *y)).collect()
        } else {
            let xs = [far.x + far.width / 3, far.x + far.width * 2 / 3];
            xs.iter().map(|x| (*x, far.y)).collect()
        };
        for (x, y) in pts {
            cv.put(x as i32, y as i32, '◉', silver);
        }
        // RF shield over the chips
        let sh = if inner.height >= 4 {
            Rect::new(
                inner.x + 1,
                inner.y + 1,
                inner.width.saturating_sub(2),
                inner.height - 2,
            )
        } else {
            Rect::new(
                inner.x + 1,
                inner.y,
                inner.width.saturating_sub(1),
                inner.height,
            )
        };
        cv.bg(sh, SHIELD);
        let sh = if ctx.mode == Mode::Board {
            with_bars(cv, sh, p, 1)
        } else {
            sh
        };
        let mut lines = module_lines(p, ctx, sh.width as usize);
        if ctx.mode == Mode::Board && p.temp.is_some() && sh.height as usize > lines.len() {
            temp_tag(cv, sh, p.temp);
            lines.truncate(sh.height as usize - 1);
            text_block(
                cv,
                Rect {
                    y: sh.y + 1,
                    height: sh.height - 1,
                    ..sh
                },
                &lines,
                CHIP,
            );
        } else {
            if let (Mode::Board, Some(t)) = (ctx.mode, p.temp) {
                lines.insert(1.min(lines.len()), fmt_temp(t));
            }
            text_block(cv, sh, &lines, CHIP);
        }
    } else {
        let mid = far.y + far.height / 2;
        if far.width == 1 {
            cv.put(
                far.x as i32,
                mid as i32,
                if ctx.edge == Edge::Left { '◗' } else { '◖' },
                silver,
            );
        } else {
            cv.put((far.x + far.width / 2) as i32, far.y as i32, '◡', silver);
        }
        // controller + NAND packages
        let pkg = Color::Rgb(52, 52, 58);
        if inner.width > inner.height {
            for x in (inner.x + 1..inner.x + inner.width.saturating_sub(1)).step_by(6) {
                let w = 5.min(inner.x + inner.width - 1 - x);
                cv.bg(Rect::new(x, inner.y, w, inner.height), pkg);
            }
        } else {
            cv.bg(inner, pkg);
        }
        let text_r = if ctx.mode == Mode::Board {
            with_bars(cv, inner, p, 1)
        } else {
            inner
        };
        let inner = text_r;
        let mut lines = module_lines(p, ctx, inner.width as usize);
        if ctx.mode == Mode::Board && p.temp.is_some() && inner.height as usize > lines.len() {
            temp_tag(cv, inner, p.temp);
            lines.truncate(inner.height as usize - 1);
            text_block(
                cv,
                Rect {
                    y: inner.y + 1,
                    height: inner.height - 1,
                    ..inner
                },
                &lines,
                Color::White,
            );
        } else {
            if ctx.mode == Mode::Board {
                if let Some(t) = p.temp {
                    lines.insert(1, fmt_temp(t));
                }
            }
            text_block(cv, inner, &lines, Color::White);
        }
        // activity LED at the far end, clear of the mounting screw
        let (lx, ly) = if far.width == 1 && far.height >= 2 {
            let y = if mid == far.y {
                far.y + far.height - 1
            } else {
                far.y
            };
            (far.x, y)
        } else if far.width >= 2 {
            (far.x, far.y)
        } else {
            (inner.x + inner.width.saturating_sub(1), inner.y)
        };
        disk_led(cv, lx as i32, ly as i32, p, ctx.t);
    }
    if ctx.sel {
        cv.corners(m);
    }
}

fn draw_camera(cv: &mut Cv, r: Rect, p: Option<&Part>, ctx: &Ctx) {
    let m = aspect(r, 6.0, ctx.align);
    cv.bg(m, GREEN_PCB);
    let (cx, cy) = (m.x + m.width / 2, m.y + m.height / 2);
    let sensor = if m.width >= 9 && m.height >= 3 {
        Rect::new(cx - 2, cy - 1, 5, 3)
    } else if m.width >= 5 {
        Rect::new(cx - 1, cy, 3, 1)
    } else {
        Rect::new(cx, cy, 1, 1)
    };
    cv.bg(sensor, CHIP);
    cv.put(
        cx as i32,
        cy as i32,
        '◉',
        Style::new().fg(Color::Rgb(70, 85, 140)).bg(CHIP),
    );
    if ctx.mode == Mode::Pic && m.height >= 3 && m.width >= 14 {
        let label = p
            .map(|p| p.tag.clone())
            .unwrap_or_else(|| ctx.label.to_string());
        cv.text(
            m.x as i32 + 1,
            cy as i32,
            &label,
            (sensor.x - m.x - 1) as i32,
            Style::new().fg(Color::White),
        );
    }
    if ctx.sel {
        cv.corners(m);
    }
}

// ---------------------------------------------------------------- LEDs

/// A pseudo-random number in 0..1 for (seed, n): the same inputs give the
/// same value, so every LED flickers in its own pattern.
fn noise(seed: &str, n: u64) -> f64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in seed.bytes() {
        h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
    }
    // splitmix64
    let mut z = h ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64
}

/// Whether an activity LED is lit at time `t`, given `bps` bytes/s of I/O.
/// Real LEDs light up with every burst of reads and writes; insider only
/// learns a rate once per refresh, so the LED flickers at random with a duty
/// cycle that grows with the rate (log scale: a few KB/s is the odd blip,
/// hundreds of MB/s is nearly solid).
fn blink(seed: &str, t: f64, bps: f64) -> bool {
    if bps.is_nan() || bps < 1.0 {
        return false;
    }
    let duty = ((bps.log10() - 2.0) / 7.0).clamp(0.12, 0.9);
    noise(seed, (t * 15.0) as u64) < duty
}

/// One LED: a lit cell, or a dark one when off.
fn led(cv: &mut Cv, x: i32, y: i32, on: Option<Color>) {
    let c = match on {
        Some(c) => c,
        None => Color::Rgb(48, 44, 40),
    };
    cv.put(x, y, ' ', Style::new().bg(c));
}

/// The drive activity LED of part `p` at (x, y).
fn disk_led(cv: &mut Cv, x: i32, y: i32, p: &Part, t: f64) {
    let bps = p.io.map_or(0.0, |(r, w)| r + w);
    led(cv, x, y, blink(&p.id, t, bps).then_some(LED_DISK));
}

/// An RJ45 jack: a plain metal shell with two LEDs, like on a real one: link (left; green
/// at gigabit and up, amber below) and activity (right; flickers with traffic).
fn draw_rj45(cv: &mut Cv, r: Rect, lan: Option<&Part>, ctx: &Ctx) {
    cv.bg(r, METAL);
    let (x0, y0) = (r.x as i32, r.y as i32);
    let x1 = x0 + r.width as i32 - 1;
    let (up, mbps) = lan.and_then(|p| p.link).unwrap_or((false, 0));
    if ctx.sel {
        cv.restyle(r, Style::new().bg(HI));
    }
    if r.width >= 2 {
        let link = up.then_some(if mbps == 0 || mbps >= 1000 {
            LED_LINK
        } else {
            LED_LINK_SLOW
        });
        let traffic = lan.and_then(|p| p.io).map_or(0.0, |(rx, tx)| rx + tx);
        let seed = lan.map_or("rj45", |p| p.id.as_str());
        let act = (up && blink(seed, ctx.t, traffic)).then_some(LED_ACT);
        led(cv, x0, y0, link);
        led(cv, x1, y0, act);
    }
}

/// A port or pin header on the board: plain metal. What is plugged in shows
/// as a part with an arrow into it, so the port itself needs no opening.
fn draw_port(cv: &mut Cv, r: Rect, ctx: &Ctx, header: bool) {
    cv.bg(r, METAL);
    if header {
        if let Some(e) = &ctx.extra {
            if r.height >= 3 {
                let dark = Color::Rgb(30, 30, 34);
                cv.ctext(r, (r.y + r.height - 1) as i32, e, Style::new().fg(dark));
            }
        }
    }
    if ctx.sel {
        cv.restyle(r, Style::new().bg(HI));
    }
}

/// An exploded USB device: body plus a metal plug facing the board.
fn draw_usb_device(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx) {
    let w = r.width.min(18).max(4);
    let m = Rect::new(
        match ctx.align {
            -1 => r.x,
            1 => r.x + r.width - w,
            _ => r.x + (r.width - w) / 2,
        },
        r.y,
        w,
        r.height,
    );
    let plug_w = 3.min(m.width / 3);
    // plug faces the board: right edge for the left column, left edge otherwise
    let (plug, body) = if ctx.align <= 0 {
        (
            Rect::new(m.x, m.y + m.height / 3, plug_w, (m.height + 2) / 3),
            Rect::new(m.x + plug_w, m.y, m.width - plug_w, m.height),
        )
    } else {
        (
            Rect::new(
                m.x + m.width - plug_w,
                m.y + m.height / 3,
                plug_w,
                (m.height + 2) / 3,
            ),
            Rect::new(m.x, m.y, m.width - plug_w, m.height),
        )
    };
    let internal = p.kind == Kind::UsbInternal;
    cv.bg(
        body,
        if internal {
            GREEN_PCB
        } else {
            Color::Rgb(44, 48, 58)
        },
    );
    if !internal {
        cv.bg(plug, METAL);
    } else {
        cv.bg(plug, Color::Rgb(235, 235, 235));
    }
    // storage sticks get read/write/space bars under the name, when tall enough
    let text_r = if p.io.is_some() && body.height >= 3 {
        let inner = Rect::new(
            body.x + 1,
            body.y,
            body.width.saturating_sub(2),
            body.height,
        );
        Rect {
            width: body.width,
            x: body.x,
            ..with_bars(cv, inner, p, 1)
        }
    } else {
        body
    };
    text_block(cv, text_r, &[p.tag.clone()], Color::White);
    if (p.io.is_some() || p.tag == "Storage") && body.width >= 3 {
        // activity LED at the far end from the plug
        let x = if ctx.align <= 0 {
            body.x + body.width - 1
        } else {
            body.x
        };
        disk_led(cv, x as i32, body.y as i32, p, ctx.t);
    }
    if ctx.sel {
        cv.corners(m);
    }
}

fn draw_fan(cv: &mut Cv, r: Rect, p: Option<&Part>, ctx: &Ctx) {
    cv.bg(r, Color::Rgb(22, 22, 25));
    cv.frame(r, &ROUND, edge_style(ctx.sel));
    let inner = shrink(r);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let rpm = p.and_then(|p| p.rpm).unwrap_or(0) as f64;
    // A terminal redraws ~15x a second, so real rotation (50 rev/s at 3000 RPM)
    // would alias into a crawl. Rotation speed scales with RPM, capped below
    // the point where the blades start wagon-wheeling.
    let rps = (rpm / 4000.0).min(0.9);
    let rot = ctx.t * rps * std::f64::consts::TAU;
    let has_text = inner.height >= 4;
    let area_h = inner.height - has_text as u16;
    // Braille: each cell is a 2x4 grid of dots, which are roughly square.
    let (sw, sh) = (inner.width as f64 * 2.0, area_h as f64 * 4.0);
    let (cx, cy) = (sw / 2.0, sh / 2.0);
    let rad = (sw.min(sh) / 2.0) - 0.3;
    const N: f64 = 7.0;
    const BITS: [[u32; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
    let seg = std::f64::consts::TAU / N;
    for y in 0..area_h {
        for x in 0..inner.width {
            let mut bits = 0u32;
            let mut hub = false;
            for (dx, col) in BITS.iter().enumerate() {
                for (dy, bit) in col.iter().enumerate() {
                    let px = (x as f64 * 2.0 + dx as f64 + 0.5 - cx) / rad;
                    let py = (y as f64 * 4.0 + dy as f64 + 0.5 - cy) / rad;
                    let d = (px * px + py * py).sqrt();
                    let on = if d < 0.24 {
                        hub = true;
                        true
                    } else if d > 1.0 {
                        false
                    } else if d > 0.9 {
                        true // shroud ring
                    } else {
                        let th = py.atan2(px) - rot - d * 1.3; // swept blades
                        (th.rem_euclid(seg) / seg) < 0.42
                    };
                    if on {
                        bits |= bit;
                    }
                }
            }
            if bits != 0 {
                let ch = char::from_u32(0x2800 + bits).unwrap_or(' ');
                let c = if hub {
                    Color::Rgb(150, 150, 160)
                } else {
                    Color::Rgb(120, 122, 134)
                };
                cv.put(
                    (inner.x + x) as i32,
                    (inner.y + y) as i32,
                    ch,
                    Style::new().fg(c),
                );
            }
        }
    }
    if has_text {
        let y = (inner.y + inner.height - 1) as i32;
        let s = match p.and_then(|p| p.rpm) {
            Some(r) => format!("{r} RPM"),
            None => ctx.label.to_string(),
        };
        match p.and_then(|p| p.pwm) {
            // PWM duty as a bar, the speed on it
            Some(d) if inner.width >= 6 => {
                let mut b = load_bar(d);
                b.text = s;
                draw_bar(cv, Rect::new(inner.x, y as u16, inner.width, 1), &b);
            }
            _ => cv.ctext(inner, y, &s, Style::new().fg(DIMTXT)),
        }
    }
}

fn draw_display(cv: &mut Cv, r: Rect, p: Option<&Part>, ctx: &Ctx) {
    cv.bg(r, Color::Rgb(16, 16, 18));
    cv.frame(r, &ROUND, edge_style(ctx.sel));
    let top = ((r.height as f32 * 0.16).round() as u16).max(2);
    let panel = Rect::new(
        r.x + 2,
        r.y + top,
        r.width.saturating_sub(4),
        r.height.saturating_sub(top + 1),
    );
    let l = p.and_then(|p| p.level).unwrap_or(0.5);
    let glow = |a: f64, b: f64| (a + b * l) as u8;
    let bg = Color::Rgb(glow(8.0, 26.0), glow(12.0, 40.0), glow(22.0, 70.0));
    cv.bg(panel, bg);
    let lines: Vec<String> = match p {
        Some(p) if ctx.mode == Mode::Board || panel.height >= 3 => {
            let mut v = vec![p.title.clone()];
            v.extend(pack(&p.metrics, panel.width.saturating_sub(2) as usize));
            v
        }
        Some(p) => vec![p.tag.clone()],
        None => vec![ctx.label.to_string(), "no data".into()],
    };
    text_block(cv, panel, &lines, Color::White);
}

fn draw_cpu_cores(cv: &mut Cv, g: Rect, p: &Part) {
    let n = p.cores.len();
    if n == 0 || g.width < 4 || g.height == 0 {
        return;
    }
    let rows = if n > 4 && g.height >= 2 { 2 } else { 1 };
    let cols = (n + rows - 1) / rows;
    let bw = g.width / cols as u16;
    // separate dies: a gap column between boxes, and a gap row between rows
    // as long as each box keeps two rows (temperature and load)
    let gap = (rows > 1 && g.height > 2 * rows as u16) as u16;
    let bh = (g.height - gap * (rows as u16 - 1)) / rows as u16;
    if bw < 3 || bh == 0 {
        return;
    }
    for (i, c) in p.cores.iter().enumerate() {
        let (col, row) = ((i % cols) as u16, (i / cols) as u16);
        let b = Rect::new(g.x + col * bw, g.y + row * (bh + gap), bw - 1, bh);
        cv.bg(b, Color::Rgb(44, 47, 58));
        // a frame only when there's still room for temperature and load bar
        let inner = if b.height >= 4 && b.width >= 6 {
            cv.frame(b, &PLAIN, Style::new().fg(Color::Rgb(95, 98, 112)));
            shrink(b)
        } else {
            b
        };
        let mut lines: Vec<(String, Style)> = Vec::new();
        match c.temp {
            Some(t) => lines.push((
                format!("{t:.0}°"),
                Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
            )),
            None => lines.push((format!("#{}", c.id), Style::new().fg(DIMTXT))),
        }
        if let Some(g) = c.ghz {
            lines.push((
                format!("{g:.1}G"),
                Style::new().fg(Color::Rgb(200, 200, 210)),
            ));
        }
        // load: a mini bar on the bottom row when there's room, else text
        let mut inner = inner;
        if let Some(l) = c.load {
            if inner.height >= 2 {
                let row = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
                draw_bar(cv, row, &load_bar(l / 100.0));
                inner.height -= 1;
            } else {
                lines.push((
                    format!("{l:.0}%"),
                    Style::new().fg(Color::Rgb(160, 160, 172)),
                ));
            }
        }
        let k = lines.len().min(inner.height as usize);
        let y0 = inner.y as i32 + (inner.height as i32 - k as i32) / 2;
        for (j, (s, st)) in lines.iter().take(k).enumerate() {
            cv.ctext(inner, y0 + j as i32, s, *st);
        }
    }
}

// ---------------------------------------------------------------- bars

/// One horizontal gauge: filled to `frac`, with `text` on top. `short` is
/// used when `text` doesn't fit.
struct Bar {
    frac: f64,
    text: String,
    short: String,
    color: Color,
}

/// A row of gauges sharing one line, each as wide as its weight: one bar, or
/// receive and send side by side, sized by what each direction can carry.
type Row = Vec<(Bar, f64)>;

const BAR_TRACK: Color = Color::Rgb(30, 30, 36);
const BAR_READ: Color = Color::Rgb(185, 50, 50);
const BAR_WRITE: Color = Color::Rgb(45, 105, 205);
const BAR_USED: Color = Color::Rgb(88, 88, 100);
const BAR_SPEED: Color = Color::Rgb(35, 140, 65);

/// Green below 70%, yellow to 80, orange to 90, red above.
fn load_color(f: f64) -> Color {
    match f {
        f if f < 0.7 => Color::Rgb(35, 140, 65),
        f if f < 0.8 => Color::Rgb(185, 165, 30),
        f if f < 0.9 => Color::Rgb(215, 115, 25),
        _ => Color::Rgb(195, 45, 40),
    }
}

fn load_bar(f: f64) -> Bar {
    let f = f.clamp(0.0, 1.0);
    let text = format!("{:.0}%", f * 100.0);
    Bar {
        frac: f,
        short: text.clone(),
        text,
        color: load_color(f),
    }
}

/// "3.8/32GB", "6.0/8GB" (binary, as memory is sold) or "39/128GB",
/// "0.6/1.0TB" (decimal, as drives are).
fn fmt_used(used: f64, total: f64, binary: bool) -> String {
    let k: f64 = if binary { 1024.0 } else { 1000.0 };
    let (g, t) = (k.powi(3), k.powi(4));
    if total >= t {
        format!("{:.1}/{:.1}TB", used / t, total / t)
    } else if total >= 100.0 * g {
        format!("{:.0}/{:.0}GB", used / g, total / g)
    } else {
        format!("{:.1}/{:.0}GB", used / g, total / g)
    }
}

fn fmt_rate(b: f64) -> String {
    match b {
        b if b >= 1e9 => format!("{:.1} GB/s", b / 1e9),
        b if b >= 1e6 => format!("{:.1} MB/s", b / 1e6),
        b if b >= 1e3 => format!("{:.0} KB/s", b / 1e3),
        b => format!("{b:.0} B/s"),
    }
}

/// "9.5M", "120K": a rate for narrow bars.
fn fmt_rate_short(b: f64) -> String {
    match b {
        b if b >= 1e9 => format!("{:.1}G", b / 1e9),
        b if b >= 1e6 => format!("{:.1}M", b / 1e6),
        b if b >= 1e3 => format!("{:.0}K", b / 1e3),
        b => format!("{b:.0}"),
    }
}

/// A part's volume or input level: `level`, or the "40%" in its metrics.
fn level_of(p: &Part) -> Option<f64> {
    p.level.or_else(|| {
        p.metrics
            .iter()
            .find_map(|m| m.strip_suffix('%')?.trim().parse::<f64>().ok())
            .map(|v| v / 100.0)
    })
}

/// The gauges a part gets, in drawing order, one row each.
fn bars_for(p: &Part) -> Vec<Row> {
    let mut v: Vec<Row> = Vec::new();
    let one = |b: Bar| vec![(b, 1.0)];
    // used / total, the total as people know it (capacity), the share of
    // what the OS can actually use
    let used = |u: f64, t: f64, binary: bool| {
        let label = p.capacity.filter(|c| *c >= t * 0.9).unwrap_or(t);
        let f = (u / t).clamp(0.0, 1.0);
        let text = format!("{} ({:.0}%)", fmt_used(u, label, binary), f * 100.0);
        Bar {
            frac: f,
            short: fmt_used(u, label, binary),
            text,
            color: BAR_USED,
        }
    };
    let io = |max: Option<(f64, f64)>| -> Option<Row> {
        let (rx, tx) = p.io?;
        let (mr, mt) = max.filter(|m| m.0 > 0.0 && m.1 > 0.0).unwrap_or((0.0, 0.0));
        let seg = |val: f64, m: f64, arrow: char, color: Color| Bar {
            frac: if m > 0.0 {
                (val / m).clamp(0.0, 1.0)
            } else {
                0.0
            },
            text: format!("{arrow} {}", fmt_rate(val)),
            short: format!("{arrow}{}", fmt_rate_short(val)),
            color,
        };
        let (wr, wt) = if mr > 0.0 { (mr, mt) } else { (1.0, 1.0) };
        Some(vec![
            (seg(rx, mr, '↓', BAR_READ), wr),
            (seg(tx, mt, '↑', BAR_WRITE), wt),
        ])
    };
    match p.kind {
        Kind::Cpu => {
            let mean = (!p.cores.is_empty()).then(|| {
                p.cores.iter().filter_map(|c| c.load).sum::<f64>() / p.cores.len() as f64 / 100.0
            });
            if let Some(l) = p.load.or(mean) {
                v.push(one(load_bar(l)));
            }
        }
        Kind::Gpu => {
            if let Some(l) = p.load {
                v.push(one(load_bar(l)));
            }
            // shader clock against its maximum
            if let Some((cur, max)) = p.speed.filter(|s| s.1 > 0.0) {
                v.push(one(Bar {
                    frac: (cur / max).clamp(0.0, 1.0),
                    text: format!("{cur:.0}/{max:.0} MHz"),
                    short: format!("{cur:.0}/{max:.0}"),
                    color: BAR_SPEED,
                }));
            }
            if let Some((u, t)) = p.usage.filter(|u| u.1 > 0.0) {
                // say what it is: the GPU has three bars
                let mut b = used(u, t, true);
                b.text = format!("VRAM: {}", b.text);
                v.push(one(b));
            }
        }
        Kind::Dimm => {
            if let Some((cur, max)) = p.speed.filter(|s| s.1 > 0.0) {
                let text = format!("{cur:.0}/{max:.0} MT/s");
                v.push(one(Bar {
                    frac: (cur / max).clamp(0.0, 1.0),
                    short: format!("{cur:.0}/{max:.0}"),
                    text,
                    color: BAR_SPEED,
                }));
            }
            if let Some((u, t)) = p.usage.filter(|u| u.1 > 0.0) {
                v.push(one(used(u, t, true)));
            }
        }
        Kind::Ethernet => {
            let link = p
                .link
                .filter(|l| l.0 && l.1 > 0)
                .map(|l| l.1 as f64 * 1e6 / 8.0)
                .map(|b| (b, b));
            v.extend(io(p.io_max.or(link)));
        }
        Kind::Wifi => v.extend(io(p.io_max)),
        // drives, and USB drives
        Kind::Ssd | Kind::Usb | Kind::UsbInternal if p.io.is_some() || p.usage.is_some() => {
            v.extend(io(p.io_max));
            if let Some((u, t)) = p.usage.filter(|u| u.1 > 0.0) {
                v.push(one(used(u, t, false)));
            }
        }
        Kind::Speaker | Kind::Microphone => {
            if let Some(l) = level_of(p) {
                v.push(one(load_bar(l)));
            }
        }
        Kind::Fan => {
            if let Some(d) = p.pwm {
                v.push(one(load_bar(d)));
            }
        }
        _ => {}
    }
    v
}

/// A gauge on one row: the filled part in its colour, the rest dark, the
/// text (or its short form) centered on top.
fn draw_bar(cv: &mut Cv, row: Rect, b: &Bar) {
    if row.width == 0 || row.height == 0 {
        return;
    }
    let row = Rect { height: 1, ..row };
    cv.bg(row, BAR_TRACK);
    let fill = (row.width as f64 * b.frac).round() as u16;
    cv.bg(Rect { width: fill, ..row }, b.color);
    let text = if b.text.chars().count() as u16 <= row.width {
        &b.text
    } else {
        &b.short
    };
    let w = (text.chars().count() as u16).min(row.width);
    let x = row.x + (row.width - w) / 2;
    cv.text(x as i32, row.y as i32, text, w as i32, Style::new().fg(HI));
}

/// A row of gauges: each gets a share of the width by its weight, with a
/// one-cell gap between them.
fn draw_row(cv: &mut Cv, row: Rect, bars: &Row) {
    let total: f64 = bars.iter().map(|b| b.1).sum();
    let gaps = bars.len().saturating_sub(1) as u16;
    if bars.is_empty() || total <= 0.0 || row.width <= gaps {
        return;
    }
    let avail = (row.width - gaps) as f64;
    let mut x = row.x;
    for (i, (b, wgt)) in bars.iter().enumerate() {
        let w = if i + 1 == bars.len() {
            row.x + row.width - x
        } else {
            ((avail * wgt / total).round() as u16).max(1)
        };
        draw_bar(cv, Rect::new(x, row.y, w, 1), b);
        x += w + 1;
    }
}

/// Draw a part's bars along the bottom of `area`, with `keep` rows above
/// them for text when there is room; when there isn't, the bars win (the
/// slot's position and label still say what it is). Returns what is left
/// for the text.
fn with_bars(cv: &mut Cv, area: Rect, p: &Part, keep: u16) -> Rect {
    let rows = bars_for(p);
    let all = rows.len() as u16;
    let n = if area.height >= all + keep {
        all
    } else {
        all.min(area.height)
    };
    if n == 0 || area.width < 5 {
        return area;
    }
    let top = area.y + area.height - n;
    for (i, r) in rows.iter().take(n as usize).enumerate() {
        draw_row(cv, Rect::new(area.x, top + i as u16, area.width, 1), r);
    }
    Rect {
        height: area.height - n,
        ..area
    }
}

struct Look {
    bg: Color,
    fg: Color,
    set: &'static Bset,
    pins: bool,
}

fn look(k: Kind) -> Look {
    use Color::Rgb;
    let l = |bg, fg, set, pins| Look { bg, fg, set, pins };
    match k {
        Kind::Cpu => l(Rgb(158, 160, 168), Rgb(20, 20, 26), &HEAVY, false),
        Kind::Gpu => l(Rgb(40, 40, 46), Rgb(225, 225, 225), &HEAVY, false),
        Kind::Ethernet | Kind::Chipset | Kind::Bios => l(CHIP, Rgb(215, 215, 215), &CHIPB, true),
        Kind::Battery => l(Rgb(30, 30, 36), Rgb(235, 235, 235), &ROUND, false),
        _ => l(Rgb(34, 40, 38), Rgb(230, 230, 230), &PLAIN, false),
    }
}

/// Charging animation for the empty part of a battery: sectors filling up
/// left to right, over and over, in a dimmer shade of the charge colour so
/// the real level stays readable in every frame.
fn charge_sweep(cv: &mut Cv, empty: Rect, c: Color, t: f64) {
    const PERIOD: f64 = 1.4; // seconds per sweep
    let dim = match c {
        // halfway to the empty cell colour
        Color::Rgb(r, g, b) => Color::Rgb(r / 2 + 15, g / 2 + 15, b / 2 + 18),
        c => c,
    };
    let sector = (empty.width / 8).max(1);
    let sectors = empty.width.div_ceil(sector);
    // sectors + 1 steps, so each sweep also shows an empty and a full frame
    let shown = ((t / PERIOD).fract() * (sectors + 1) as f64) as u16;
    let w = (shown * sector).min(empty.width);
    cv.bg(Rect { width: w, ..empty }, dim);
}

/// Generic chip/box: label + temperature on the first row inside the border,
/// then title and metrics.
fn draw_chip(cv: &mut Cv, r: Rect, kind: Kind, p: Option<&Part>, ctx: &Ctx) {
    let lk = look(kind);
    cv.fill(r, ' ', Style::new().bg(lk.bg).fg(lk.fg));
    let framed = r.height >= 3 && r.width >= 4 && !ctx.packaged;
    let inner = if framed { shrink(r) } else { r };

    if kind == Kind::Battery {
        if let Some(f) = p.and_then(|p| p.level) {
            let w = (inner.width as f64 * f).round() as u16;
            let c = if f < 0.15 {
                Color::Rgb(150, 40, 35)
            } else if f < 0.35 {
                Color::Rgb(140, 110, 30)
            } else {
                Color::Rgb(30, 105, 55)
            };
            cv.bg(Rect { width: w, ..inner }, c);
            let charging = p.is_some_and(|p| {
                p.props
                    .iter()
                    .any(|(k, v)| k == "Status" && v == "Charging")
            });
            if charging && w < inner.width {
                charge_sweep(
                    cv,
                    Rect {
                        x: inner.x + w,
                        width: inner.width - w,
                        ..inner
                    },
                    c,
                    ctx.t,
                );
            }
            let step = (inner.width / 6).max(2) as usize;
            for x in (inner.x..inner.x + inner.width).step_by(step).skip(1) {
                for y in inner.y..inner.y + inner.height {
                    cv.put(
                        x as i32,
                        y as i32,
                        '┊',
                        Style::new().fg(Color::Rgb(70, 70, 80)),
                    );
                }
            }
        }
        let ny = r.y as i32 + r.height as i32 / 2;
        cv.put(r.x as i32 + r.width as i32, ny, '▌', Style::new().fg(LG));
    }

    let es = edge_style(ctx.sel);
    if framed {
        cv.frame(r, lk.set, es);
        if lk.pins {
            let bot = r.y as i32 + r.height as i32 - 1;
            for x in (r.x as i32 + 2..r.x as i32 + r.width as i32 - 2).step_by(2) {
                cv.put(x, r.y as i32, '╤', es);
                cv.put(x, bot, '╧', es);
            }
        }
    }

    let fg = Style::new().fg(lk.fg);
    let w = inner.width as usize;
    let mut body = inner;
    // CPU and GPU: label and temperature sit on the top border, title-bar
    // style, leaving the inside for the readings and bars
    let on_border = ctx.mode == Mode::Board
        && (framed || ctx.packaged)
        && !lk.pins
        && matches!(kind, Kind::Cpu | Kind::Gpu);
    if on_border {
        // inside a package, write on the package's border just above
        let r = if ctx.packaged {
            Rect::new(
                r.x.saturating_sub(1),
                r.y.saturating_sub(1),
                r.width + 2,
                r.height,
            )
        } else {
            r
        };
        let top = r.y as i32;
        let lab = format!(" {} ", ctx.label);
        let temp = p
            .and_then(|p| p.temp)
            .map(|t| (format!(" {} ", fmt_temp(t)), t));
        let tw = temp.as_ref().map_or(0, |(s, _)| s.chars().count() as i32);
        let room = r.width as i32 - 4 - tw;
        if room > 2 {
            let st = if ctx.packaged {
                Style::new().fg(Color::Rgb(220, 220, 228)).bg(PACKAGE)
            } else {
                fg
            };
            cv.text(
                r.x as i32 + 2,
                top,
                &lab,
                room,
                st.add_modifier(Modifier::BOLD),
            );
        }
        if let Some((s, t)) = temp.filter(|_| r.width as i32 > tw + 4) {
            cv.text(
                r.x as i32 + r.width as i32 - 2 - tw,
                top,
                &s,
                tw,
                Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
            );
        }
    }
    // first row: label (left) and temperature (right)
    if ctx.mode == Mode::Board && inner.height >= 3 && !on_border {
        cv.text(
            inner.x as i32,
            inner.y as i32,
            ctx.label,
            w as i32 - 6,
            fg.add_modifier(Modifier::DIM),
        );
        temp_tag(cv, inner, p.and_then(|p| p.temp));
        body = Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        };
    }
    let mut lines: Vec<String> = Vec::new();
    match (ctx.mode, p) {
        (Mode::Pic, Some(p)) => lines.push(if p.tag.is_empty() {
            ctx.label.to_string()
        } else {
            p.tag.clone()
        }),
        (Mode::Pic, None) => lines.push(ctx.label.to_string()),
        (Mode::Board, Some(p)) => {
            let mut metrics = p.metrics.clone();
            if let Some(e) = &ctx.extra {
                metrics.push(e.clone());
            }
            if body.height >= 2 {
                lines.push(p.title.clone());
            } else {
                let tag = if p.tag.is_empty() {
                    ctx.label.to_string()
                } else {
                    p.tag.clone()
                };
                metrics.retain(|m| !tag.contains(m.as_str()));
                metrics.insert(0, tag);
                if inner.height < 3 {
                    if let Some(t) = p.temp {
                        metrics.insert(1, fmt_temp(t));
                    }
                }
            }
            lines.extend(pack(&metrics, w));
        }
        (Mode::Board, None) => {
            lines.push(ctx.label.to_string());
            if body.height >= 2 {
                lines.push("no data".into());
            }
        }
    }

    // CPU: title, a load bar, then a box per physical core
    if kind == Kind::Cpu && ctx.mode == Mode::Board {
        if let Some(p) = p.filter(|p| !p.cores.is_empty()) {
            let text_h = (lines.len() as u16).min(2);
            if body.height >= text_h + 2 {
                let tr = Rect {
                    height: text_h,
                    ..body
                };
                text_block(cv, tr, &lines[..text_h as usize], lk.fg);
                let bar = bars_for(p).into_iter().next();
                let bar_h = (bar.is_some() && body.height >= text_h + 3) as u16;
                if let (Some(b), 1) = (&bar, bar_h) {
                    draw_row(
                        cv,
                        Rect::new(body.x + 1, body.y + text_h, body.width.saturating_sub(2), 1),
                        b,
                    );
                }
                // a blank row between the total and the cores, when it fits
                let gap = (bar_h == 1 && body.height >= text_h + 5) as u16;
                let g = Rect::new(
                    body.x + 1,
                    body.y + text_h + bar_h + gap,
                    body.width.saturating_sub(1),
                    body.height - text_h - bar_h - gap,
                );
                draw_cpu_cores(cv, g, p);
                return;
            }
        }
    }
    if let (Mode::Board, Some(p)) = (ctx.mode, p) {
        if kind != Kind::Cpu {
            body = with_bars(cv, body, p, 1);
        }
    }
    text_block(cv, body, &lines, lk.fg);
    if ctx.sel && !framed {
        cv.restyle(r, Style::new().add_modifier(Modifier::REVERSED));
    }
    if let (Kind::Ssd, Some(p)) = (kind, p) {
        // a drive's activity LED: on the bottom edge, or the last cell when tiny
        let (x, y) = if framed {
            (r.x + r.width - 3, r.y + r.height - 1)
        } else {
            (r.x + r.width - 1, r.y)
        };
        disk_led(cv, x as i32, y as i32, p, ctx.t);
    }
}

fn empty_slot(cv: &mut Cv, r: Rect, label: &str, sel: bool) {
    let st = edge_style(sel);
    if r.height >= 2 && r.width >= 2 {
        cv.frame(r, &DASH, st);
    } else {
        cv.fill(r, '░', st);
    }
    let inner = if r.height >= 3 { shrink(r) } else { r };
    if inner.width >= 4 {
        let my = inner.y as i32 + inner.height.saturating_sub(1) as i32 / 2;
        cv.ctext(
            inner,
            my,
            &fit_label(label, inner.width),
            Style::new().fg(DIMTXT),
        );
    }
}

/// `label` as it fits `width`: "Speaker (left)", or "Speaker L" when tight.
fn fit_label(label: &str, width: u16) -> String {
    if label.chars().count() as u16 <= width {
        return label.to_string();
    }
    label.replace(" (left)", " L").replace(" (right)", " R")
}

/// Draws a slot (Board mode) or a flown-out part (Pic mode).
fn draw(cv: &mut Cv, r: Rect, slot: Option<&SlotDef>, part: Option<&Part>, ctx: &Ctx) {
    let part = part.filter(|p| p.present);
    let slot_kind = slot.map(|s| s.kind());
    let kind = part.map(|p| p.kind).or(slot_kind).unwrap_or(Kind::Pci);
    let soldered = slot.map_or(false, |s| s.soldered);
    // ports and headers are metal whether or not something is plugged in
    if ctx.mode == Mode::Board {
        match slot_kind {
            Some(Kind::Usb) => return draw_port(cv, r, ctx, false),
            Some(Kind::UsbInternal) => return draw_port(cv, r, ctx, true),
            Some(Kind::Connector) => return draw_connector(cv, r, ctx),
            Some(Kind::Rj45) => return draw_rj45(cv, r, None, ctx),
            // soldered RAM, NAND and Wi-Fi are packages on the board, not modules
            Some(k @ (Kind::Dimm | Kind::Ssd | Kind::Wifi)) if soldered => {
                return draw_chip(cv, r, k, part, ctx)
            }
            Some(k @ (Kind::Dimm | Kind::Ssd | Kind::Wifi)) => {
                return draw_module_slot(cv, r, k, part, ctx)
            }
            _ => {}
        }
    }
    match (kind, part) {
        (Kind::Fan, p) => draw_fan(cv, r, p, ctx),
        (Kind::Display, p) => draw_display(cv, r, p, ctx),
        (Kind::Keyboard, p) => draw_keyboard(cv, r, p, ctx),
        (Kind::Speaker, Some(p)) => draw_speaker(cv, r, p, ctx),
        (Kind::Microphone, Some(p)) => draw_mic(cv, r, p, ctx),
        (Kind::Touchpad, Some(p)) => draw_touchpad(cv, r, p, ctx),
        (Kind::Power, Some(p)) => draw_power(cv, r, p, slot, ctx),
        (Kind::Camera, p) if p.is_some() || soldered => draw_camera(cv, r, p, ctx),
        (Kind::Dimm, Some(p)) => draw_dimm(cv, r, p, ctx),
        (Kind::Ssd, Some(p)) if p.tag.starts_with("NVMe") || slot_kind == Some(Kind::Ssd) => {
            draw_m2(cv, r, p, ctx, false)
        }
        (Kind::Wifi, Some(p)) => draw_m2(cv, r, p, ctx, true),
        (Kind::Usb | Kind::UsbInternal, Some(p)) if p.tag == "Camera" => {
            draw_camera(cv, r, Some(p), ctx)
        }
        (Kind::Usb | Kind::UsbInternal, Some(p)) => draw_usb_device(cv, r, p, ctx),
        (k, p) if p.is_some() || soldered => draw_chip(cv, r, k, p, ctx),
        _ => empty_slot(cv, r, ctx.label, ctx.sel),
    }
}

/// One leader line: from (sx, sy) horizontally, then vertically into rect `to`.
/// Returns the arrow head, drawn last so chips don't cover it.
fn leader(
    cv: &mut Cv,
    sx: i32,
    sy: i32,
    from_left: bool,
    to: Rect,
    st: Style,
) -> (i32, i32, char, Style) {
    let (top, bot) = (to.y as i32, to.y as i32 + to.height as i32 - 1);
    if sy >= top && sy <= bot {
        let ex = if from_left {
            to.x as i32 - 1
        } else {
            (to.x + to.width) as i32
        };
        for x in sx.min(ex)..=sx.max(ex) {
            cv.put(x, sy, '─', st);
        }
        (ex, sy, if from_left { '▶' } else { '◀' }, st)
    } else {
        let cx = to.x as i32 + to.width as i32 / 2;
        let (ey, arrow) = if sy < top {
            (top - 1, '▼')
        } else {
            (bot + 1, '▲')
        };
        for x in sx.min(cx)..=sx.max(cx) {
            cv.put(x, sy, '─', st);
        }
        for y in sy.min(ey)..=sy.max(ey) {
            cv.put(cx, y, '│', st);
        }
        let corner = match (from_left, ey > sy) {
            (true, true) => '┐',
            (true, false) => '┘',
            (false, true) => '┌',
            (false, false) => '└',
        };
        cv.put(cx, sy, corner, st);
        (cx, ey, arrow, st)
    }
}

const PLASTIC: Color = Color::Rgb(14, 14, 16);

/// The black plastic socket of a DIMM/M.2 slot. Returns the area for the module.
fn slot_connector(cv: &mut Cv, r: Rect, e: Edge) -> Rect {
    let (strip, rest) = contacts(r, e);
    cv.bg(strip, PLASTIC);
    let st = Style::new().fg(Color::Rgb(62, 62, 68)).bg(PLASTIC);
    if strip.height == 1 {
        for x in strip.x + 1..strip.x + strip.width.saturating_sub(1) {
            cv.put(x as i32, strip.y as i32, '▬', st);
        }
    } else {
        for y in strip.y..strip.y + strip.height {
            cv.put(strip.x as i32, y as i32, '▮', st);
        }
    }
    rest
}

fn draw_module_slot(cv: &mut Cv, r: Rect, kind: Kind, part: Option<&Part>, ctx: &Ctx) {
    let area = slot_connector(cv, r, ctx.edge);
    match part.filter(|p| p.present) {
        Some(p) if p.kind == Kind::Dimm => draw_dimm(cv, area, p, ctx),
        Some(p) => draw_m2(cv, area, p, ctx, p.kind == Kind::Wifi),
        None => {
            empty_slot(cv, area, ctx.label, ctx.sel);
            if kind != Kind::Dimm && area.width > 2 && area.height > 0 {
                // the M.2 standoff the card screws into
                let my = (area.y + area.height / 2) as i32;
                let x = match ctx.edge {
                    Edge::Right => area.x as i32 + 1,
                    _ => (area.x + area.width) as i32 - 2,
                };
                cv.put(x, my, '◉', Style::new().fg(METAL));
            }
        }
    }
}

/// A slot whose part has flown out (exploded view).
fn draw_empty_slot(cv: &mut Cv, r: Rect, s: &SlotDef, ctx: &Ctx) {
    match s.kind() {
        Kind::Usb => draw_port(cv, r, ctx, false),
        Kind::UsbInternal => draw_port(cv, r, ctx, true),
        Kind::Connector => draw_connector(cv, r, ctx),
        Kind::Rj45 => draw_rj45(cv, r, None, ctx),
        k @ (Kind::Dimm | Kind::Ssd | Kind::Wifi) => draw_module_slot(cv, r, k, None, ctx),
        _ => empty_slot(cv, r, ctx.label, ctx.sel),
    }
}

/// A cable connector on the board (eDP, battery...): black plastic, a row of
/// pins over its label, just big enough for the label.
fn draw_connector(cv: &mut Cv, r: Rect, ctx: &Ctx) {
    cv.bg(r, PLASTIC);
    // pins on top, unless squeezed to one row: then just the label
    if r.height >= 2 {
        let pin = Style::new().fg(Color::Rgb(185, 185, 192)).bg(PLASTIC);
        for x in r.x + 1..r.x + r.width.saturating_sub(1).max(1) {
            cv.put(x as i32, r.y as i32, '╷', pin);
        }
    }
    if r.width >= 3 {
        cv.ctext(
            r,
            (r.y + r.height - 1) as i32,
            ctx.label,
            Style::new().fg(Color::Rgb(200, 200, 206)),
        );
    }
    if ctx.sel {
        cv.corners(r);
    }
}

/// Widest a key gets: a two-cell cap and a one-cell gap. Wider slots get a
/// keyboard only as big as its keys, centred.
const KEY_PITCH: f32 = 3.0;

fn draw_keyboard(cv: &mut Cv, r: Rect, p: Option<&Part>, ctx: &Ctx) {
    let deck = Color::Rgb(22, 22, 26);
    let present = p.is_some_and(|p| p.present);
    let r = if present && r.width >= 2 + 27 && r.height >= 7 {
        // 15 key units across, function row + 5 rows down, plus the frame
        let pitch = ((r.width - 2) as f32 / 15.0).min(KEY_PITCH);
        let w = ((pitch * 15.0).ceil() as u16 + 2).min(r.width);
        let h = (if r.height >= 8 { 8 } else { 7 }).min(r.height);
        Rect::new(r.x + (r.width - w) / 2, r.y + (r.height - h) / 2, w, h)
    } else {
        r
    };
    cv.bg(r, deck);
    let framed = r.height >= 3 && r.width >= 8;
    if framed {
        cv.frame(r, &ROUND, edge_style(ctx.sel));
    }
    let k = if framed { shrink(r) } else { r };
    let Some(p) = p.filter(|p| p.present) else {
        let st = Style::new().fg(DIMTXT);
        cv.ctext(k, (k.y + k.height / 2) as i32, ctx.label, st);
        return;
    };
    let l = p.level.unwrap_or(0.0);
    let c = |v: f64| (v + 45.0 * l) as u8;
    let caps_on = p.metrics.iter().any(|m| m.starts_with("Caps"));
    let layout = p
        .props
        .iter()
        .find(|(k, _)| k == "Layout")
        .map_or("QWERTY", |(_, v)| v.as_str());
    let legend_fg = Color::Rgb(c(150.0), c(150.0), c(160.0));
    let cap_bg = Color::Rgb(c(40.0), c(40.0), c(46.0));
    if keycaps(
        cv,
        k,
        crate::keymap::by_name(layout),
        cap_bg,
        legend_fg,
        caps_on,
    ) {
        return;
    }
    let cap = Style::new()
        .fg(Color::Rgb(c(62.0), c(62.0), c(70.0)))
        .bg(deck);
    // keycaps are the top half of a cell, so rows get a gap between them
    for ri in 0..k.height {
        let y = (k.y + ri) as i32;
        let last = ri + 1 == k.height && k.height >= 3;
        if last {
            let space_w = k.width * 2 / 5;
            let sx = k.x + (k.width - space_w) / 2;
            for x in k.x..k.x + k.width {
                let in_space = x >= sx && x < sx + space_w;
                let edge = x + 1 == sx || x == sx + space_w;
                let gap = !in_space && (edge || (x - k.x) % 4 == 3);
                if !gap {
                    cv.put(x as i32, y, '▀', cap);
                }
            }
        } else {
            let off = [0u16, 0, 1, 2, 3][(ri % 5) as usize];
            for x in k.x..k.x + k.width {
                if (x - k.x + off) % 4 != 3 {
                    cv.put(x as i32, y, '▀', cap);
                }
            }
        }
    }
    // Caps Lock LED
    if p.metrics.iter().any(|m| m.starts_with("Caps")) && k.height >= 4 {
        cv.put(
            k.x as i32 + 1,
            (k.y + 2) as i32,
            '▀',
            Style::new().fg(Color::Rgb(90, 235, 125)),
        );
    }
}

/// A keyboard with legends: staggered rows on a 15-unit grid like a laptop
/// board (function row, numbers, three letter rows, modifiers and space).
/// Returns false when the area is too small to tell keys apart, so the
/// caller draws plain caps instead.
fn keycaps(
    cv: &mut Cv,
    k: Rect,
    km: &crate::keymap::Keymap,
    cap_bg: Color,
    fg: Color,
    caps_on: bool,
) -> bool {
    let pitch = (k.width as f32 / 15.0).min(KEY_PITCH);
    if pitch < 1.8 || k.height < 5 {
        return false;
    }
    // centred when the area is wider than the keys need
    let k = Rect {
        x: k.x + (k.width - (pitch * 15.0).ceil() as u16).min(k.width) / 2,
        ..k
    };
    let mac = cfg!(target_os = "macos");
    // (width in key units, legend)
    let letters = |i: usize| {
        km.rows[i]
            .split(' ')
            .map(|l| (1.0f32, l))
            .collect::<Vec<_>>()
    };
    let fkeys = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
    let mut fn_row = vec![(1.5f32, "esc")];
    fn_row.extend(fkeys.iter().map(|f| (1.0, *f)));
    fn_row.push((1.5, ""));
    let mut num = letters(0);
    num.push((2.0, "⌫"));
    let mut top = vec![(1.5, "⇥")];
    top.extend(letters(1));
    top.last_mut().unwrap().0 = 1.5;
    let mut home = vec![(1.75, "⇪")];
    home.extend(letters(2));
    home.push((2.25, "⏎"));
    let mut bottom = vec![(2.25, "⇧")];
    bottom.extend(letters(3));
    bottom.push((2.75, "⇧"));
    let space: Vec<(f32, &str)> = if mac {
        vec![
            (1.0, "fn"),
            (1.0, "⌃"),
            (1.0, "⌥"),
            (1.25, "⌘"),
            (5.5, ""),
            (1.25, "⌘"),
            (1.0, "⌥"),
            (1.0, "←"),
            (1.0, "↕"),
            (1.0, "→"),
        ]
    } else {
        vec![
            (1.0, "ctrl"),
            (1.0, "fn"),
            (1.0, "win"),
            (1.25, "alt"),
            (5.5, ""),
            (1.25, "alt"),
            (1.0, "ctrl"),
            (1.0, "←"),
            (1.0, "↕"),
            (1.0, "→"),
        ]
    };
    let mut rows = vec![fn_row, num, top, home, bottom, space];
    // rows with a gap between them if there is room, else packed; drop the
    // function row before going below one line per row
    let h = k.height as usize;
    if h < rows.len() {
        rows.remove(0);
    }
    let stride = if h >= rows.len() * 2 - 1 { 2 } else { 1 };
    let used = (rows.len() - 1) * stride + 1;
    let y0 = k.y as usize + (h - used) / 2;
    let cap = Style::new().bg(cap_bg).fg(fg);
    for (ri, row) in rows.iter().enumerate() {
        let y = (y0 + ri * stride) as i32;
        let mut u = 0.0f32;
        for &(w, legend) in row {
            let xs = k.x as i32 + (u * pitch).round() as i32;
            let xe = k.x as i32 + ((u + w) * pitch).round() as i32 - 1; // one-cell gap
            u += w;
            let cw = (xe - xs).max(1);
            cv.fill(Rect::new(xs as u16, y as u16, cw as u16, 1), ' ', cap);
            let lw = legend.chars().count() as i32;
            if lw > 0 && lw <= cw {
                let st = if legend == "⇪" && caps_on {
                    cap.fg(Color::Rgb(90, 235, 125))
                } else {
                    cap
                };
                cv.text(xs + (cw - lw) / 2, y, legend, lw, st);
            }
        }
    }
    true
}

/// The power input: a PSU (metal box, fan grille) or an external power
/// source (a plain brick: AC, a car adapter, a power bank, who knows), with
/// a status LED and the readings, or the layout's `Output` when the OS
/// reports none. An unplugged source is drawn dim.
fn draw_power(cv: &mut Cv, r: Rect, p: &Part, slot: Option<&SlotDef>, ctx: &Ctx) {
    if r.width < 6 || r.height < 3 {
        return draw_chip(cv, r, Kind::Power, Some(p), ctx);
    }
    let prop = |k: &str| {
        p.props
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    };
    let unit = slot
        .and_then(|s| s.props.get("Unit"))
        .map(|u| u.to_ascii_lowercase())
        .unwrap_or_else(|| prop("Unit").unwrap_or("").to_ascii_lowercase());
    let psu = unit.contains("psu") || unit.contains("supply");
    let on = p.online != Some(false);
    let (body, ink) = match (psu, on) {
        (true, _) => (Color::Rgb(48, 50, 55), HI),
        (false, true) => (Color::Rgb(30, 30, 33), HI),
        (false, false) => (Color::Rgb(24, 24, 26), DIMTXT),
    };
    cv.bg(r, body);
    cv.frame(
        r,
        &ROUND,
        if ctx.sel {
            edge_style(true)
        } else {
            Style::new().fg(lighten(body, 50))
        },
    );
    let mut inner = Rect::new(
        r.x + 1,
        r.y + 1,
        r.width.saturating_sub(2),
        r.height.saturating_sub(2),
    );
    if psu && inner.height >= 3 && inner.width >= 16 {
        // fan grille: a disc of dots on the left
        let d = inner.height.min(inner.width / 3) as i32;
        let (cx, cy) = (inner.x as i32 + d, inner.y as i32 + d / 2);
        let st = Style::new().fg(lighten(body, 40)).bg(body);
        for y in 0..d.max(1) {
            for x in 0..2 * d {
                let (dx, dy) = ((x - d) as f64 / 2.0, (y - d / 2) as f64);
                let rr = (d as f64 / 2.0).max(1.0);
                if dx * dx + dy * dy <= rr * rr {
                    cv.put(
                        cx - d + x,
                        cy - d / 2 + y,
                        if (x + y) % 2 == 0 { '·' } else { ' ' },
                        st,
                    );
                }
            }
        }
        let used = 2 * d as u16 + 1;
        inner = Rect::new(
            inner.x + used,
            inner.y,
            inner.width.saturating_sub(used),
            inner.height,
        );
    }
    // text: title, readings (or the layout's rating), state with an LED
    let readings = if on && !p.metrics.is_empty() {
        p.metrics.join(" · ")
    } else if on {
        slot.and_then(|s| s.props.get("Output"))
            .cloned()
            .unwrap_or_default()
    } else {
        String::new()
    };
    let (led, state) = match p.online {
        Some(false) => (Color::Rgb(70, 70, 75), "unplugged"),
        _ if psu => (LED_LINK, "on"),
        _ => (LED_LINK, "connected"),
    };
    let title = if psu {
        "PSU".to_string()
    } else {
        p.title.clone()
    };
    let bold = Style::new().fg(ink).add_modifier(Modifier::BOLD);
    // the LED leads the status line, which shows the readings when there's
    // no room for both: title / readings / ● state, or title / ● readings
    let rows = inner.height as usize;
    let mut lines: Vec<(String, Style, bool)> = Vec::new();
    match (rows, readings.is_empty()) {
        (0, _) => {}
        (1, _) => lines.push((format!("● {title}"), bold, true)),
        (2, false) => {
            lines.push((title, bold, false));
            lines.push((format!("● {readings}"), Style::new().fg(ink), true));
        }
        (_, true) => {
            lines.push((title, bold, false));
            lines.push((format!("● {state}"), Style::new().fg(DIMTXT), true));
        }
        _ => {
            lines.push((title, bold, false));
            lines.push((readings, Style::new().fg(ink), false));
            lines.push((format!("● {state}"), Style::new().fg(DIMTXT), true));
        }
    }
    let y0 = inner.y as i32 + (rows as i32 - lines.len() as i32) / 2;
    for (i, (txt, st, has_led)) in lines.iter().enumerate() {
        let w = (txt.chars().count() as i32).min(inner.width as i32);
        let x = inner.x as i32 + (inner.width as i32 - w) / 2;
        let y = y0 + i as i32;
        cv.text(x, y, txt, w, st.bg(body));
        if *has_led {
            cv.put(x, y, '●', Style::new().fg(led).bg(body));
        }
    }
}

fn draw_speaker(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx) {
    let c = Color::Rgb(26, 26, 30);
    cv.bg(r, c);
    let framed = r.height >= 3 && r.width >= 5;
    if framed {
        cv.frame(r, &ROUND, edge_style(ctx.sel));
    }
    let inner = if framed { shrink(r) } else { r };
    // grille
    let hole = Style::new().fg(Color::Rgb(85, 85, 95));
    for y in inner.y..inner.y + inner.height {
        for x in (inner.x + (y % 2)..inner.x + inner.width).step_by(2) {
            cv.put(x as i32, y as i32, '•', hole);
        }
    }
    let muted = p.metrics.first().is_some_and(|m| m == "muted");
    if let (Some(l), false, true) = (level_of(p), muted, inner.width >= 6) {
        let fits = ctx.label.chars().count() as u16 <= inner.width;
        let two = !fits && ctx.label.contains(" (") && inner.height >= 3;
        // the bar mid-way, or low enough for a two-line name above it
        let y = inner.y + (inner.height / 2).max(if two { 2 } else { 0 });
        // which speaker, above its volume: "Speaker (left)", on two lines or
        // as "Speaker L" when that doesn't fit
        let st = Style::new().fg(Color::Rgb(200, 200, 206)).bg(c);
        match ctx.label.split_once(" (") {
            // "Speaker" over "(left)" when one line is too narrow
            Some((a, b)) if !fits && y >= inner.y + 2 => {
                cv.ctext(inner, y as i32 - 2, a, st);
                cv.ctext(inner, y as i32 - 1, &format!("({b}"), st);
            }
            _ if y > inner.y => {
                cv.ctext(inner, y as i32 - 1, &fit_label(ctx.label, inner.width), st);
            }
            _ => {}
        }
        let row = Rect::new(inner.x + 1, y, inner.width - 2, 1);
        draw_bar(cv, row, &load_bar(l));
    } else if let Some(v) = p.metrics.first() {
        let s = format!(" {v} ");
        let st = Style::new()
            .fg(if v == "muted" {
                Color::Rgb(255, 110, 90)
            } else {
                Color::White
            })
            .bg(c);
        cv.ctext(inner, (inner.y + inner.height / 2) as i32, &s, st);
    }
    if ctx.sel && !framed {
        cv.corners(r);
    }
}

fn draw_mic(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx) {
    let c = Color::Rgb(24, 24, 28);
    cv.bg(r, c);
    let muted = p.metrics.first().map_or(false, |m| m == "muted");
    let st = Style::new().fg(if muted {
        Color::Rgb(255, 110, 90)
    } else {
        Color::Rgb(150, 150, 160)
    });
    // the input level as a bar on the bottom row, holes above it
    let bar = level_of(p).filter(|_| !muted && r.width >= 3);
    let holes_y = if bar.is_some() && r.height >= 2 {
        r.y + (r.height - 1) / 2
    } else {
        r.y + r.height / 2
    };
    if bar.is_none() || r.height >= 2 {
        for x in (r.x..r.x + r.width).step_by(2) {
            cv.put(x as i32, holes_y as i32, '●', st);
        }
    }
    if let Some(l) = bar {
        draw_bar(
            cv,
            Rect::new(r.x, r.y + r.height - 1, r.width, 1),
            &load_bar(l),
        );
    }
    if ctx.sel {
        cv.corners(r);
    }
}

fn draw_touchpad(cv: &mut Cv, r: Rect, p: &Part, ctx: &Ctx) {
    let c = Color::Rgb(38, 38, 44);
    cv.bg(r, c);
    cv.frame(r, &ROUND, edge_style(ctx.sel));
    let inner = if r.height >= 3 { shrink(r) } else { r };
    let s = if ctx.mode == Mode::Board {
        &p.title
    } else {
        &p.tag
    };
    cv.ctext(
        inner,
        (inner.y + inner.height / 2) as i32,
        s,
        Style::new().fg(DIMTXT),
    );
}

fn board_bg(cv: &mut Cv, app: &App, xf: &Xf, hits: &mut Vec<(Rect, String)>) {
    let b = app.board();
    let pcb = b
        .pcb
        .as_ref()
        .map(|p| xf.rect(p.x, p.y, p.w, p.h))
        .unwrap_or_else(|| xf.rect(0, 0, b.width, b.height));
    let pc = b
        .color
        .as_deref()
        .and_then(parse_color)
        .unwrap_or(Color::Rgb(18, 53, 36));
    // the PCB is the union of `pcb` and any `pcb_extra` rectangles (tabs, wings)
    let extra: Vec<Rect> = b
        .pcb_extra
        .iter()
        .map(|p| xf.rect(p.x, p.y, p.w, p.h))
        .collect();
    let edge = Style::new().fg(lighten(pc, 45));
    if extra.is_empty() {
        cv.bg(pcb, pc);
        cv.frame(pcb, &ROUND, edge);
    } else {
        let parts: Vec<Rect> = std::iter::once(pcb).chain(extra.iter().copied()).collect();
        for r in &parts {
            cv.bg(*r, pc);
        }
        outline(cv, &parts, edge);
        hits.extend(extra.iter().map(|r| (*r, "board".to_string())));
    }
    hits.push((pcb, "board".into()));
}

/// Copper traces between chips and slots on the board, showing how it is
/// wired: memory, discrete GPUs, the first x16 slot and the display
/// connector to the CPU; the chipset to the CPU (DMI); everything else on the
/// board (drives, Wi-Fi, LAN, USB, BIOS, audio and other chips) to the
/// chipset, or to the CPU/SoC when there's none. Drawn under the parts, one
/// L-shaped run per link, with proper junctions where runs meet.
fn traces(cv: &mut Cv, app: &App, xf: &Xf) {
    let b = app.board();
    let pcb = b
        .pcb
        .as_ref()
        .map(|p| xf.rect(p.x, p.y, p.w, p.h))
        .unwrap_or_else(|| xf.rect(0, 0, b.width, b.height));
    let on_board = |r: Rect| {
        r.x >= pcb.x
            && r.y >= pcb.y
            && r.x + r.width <= pcb.x + pcb.width
            && r.y + r.height <= pcb.y + pcb.height
    };
    let inside = |t: &SlotDef| {
        b.slots.iter().any(|s| {
            !std::ptr::eq(t, s)
                && t.x >= s.x
                && t.y >= s.y
                && t.x + t.w <= s.x + s.w
                && t.y + t.h <= s.y + s.h
        })
    };
    let find = |k: Kind| {
        b.slots
            .iter()
            .find(|s| s.kind() == k && !inside(s))
            .map(|s| xf.slot(s))
    };
    let (Some(cpu), chipset) = (find(Kind::Cpu), find(Kind::Chipset)) else {
        return;
    };
    let south = chipset.unwrap_or(cpu);
    let first_pcie = b
        .slots
        .iter()
        .find(|s| s.kind() == Kind::Pcie)
        .map(|s| s.id.clone());
    let mut runs: Vec<(Rect, Rect)> = Vec::new();
    if let Some(c) = chipset {
        runs.push((c, cpu));
    }
    for (si, s) in b.slots.iter().enumerate() {
        let r = xf.slot(s);
        if inside(s) || !on_board(r) {
            continue;
        }
        // ports and connectors are always there; other slots need something
        // in them, still on the board (exploded, modules fly out)
        let fixed = matches!(s.kind(), Kind::Usb | Kind::Rj45 | Kind::Connector);
        let occupied = app.asg.slots[si].iter().any(|&i| app.snap.parts[i].present)
            && (s.soldered || !app.exploded);
        if !fixed && !occupied {
            continue;
        }
        let to = match s.kind() {
            Kind::Dimm | Kind::Gpu => cpu,
            Kind::Pcie if Some(&s.id) == first_pcie.as_ref() => cpu,
            Kind::Connector if s.label().eq_ignore_ascii_case("edp") => cpu,
            Kind::Pcie
            | Kind::Ssd
            | Kind::Wifi
            | Kind::Ethernet
            | Kind::Rj45
            | Kind::Bios
            | Kind::Usb
            | Kind::UsbInternal
            | Kind::Pci => south,
            _ => continue,
        };
        if to != r {
            runs.push((r, to));
        }
    }
    // directions per cell: up, down, left, right
    let mut cells: std::collections::HashMap<(i32, i32), u8> = Default::default();
    let (up, down, left, right) = (1u8, 2u8, 4u8, 8u8);
    let mut mark = |x: i32, y: i32, bits: u8| *cells.entry((x, y)).or_default() |= bits;
    let centre = |r: Rect| {
        (
            r.x as i32 + r.width as i32 / 2,
            r.y as i32 + r.height as i32 / 2,
        )
    };
    for (a, z) in runs {
        let ((ax, ay), (zx, zy)) = (centre(a), centre(z));
        // across from the source, then up or down into the target
        let (x0, x1) = (ax.min(zx), ax.max(zx));
        for x in x0..=x1 {
            let mut bits = 0;
            if x > x0 {
                bits |= left;
            }
            if x < x1 {
                bits |= right;
            }
            mark(x, ay, bits);
        }
        let (y0, y1) = (ay.min(zy), ay.max(zy));
        for y in y0..=y1 {
            let mut bits = 0;
            if y > y0 {
                bits |= up;
            }
            if y < y1 {
                bits |= down;
            }
            mark(zx, y, bits);
        }
    }
    let st = Style::new().fg(Color::Rgb(150, 112, 45));
    for ((x, y), bits) in cells {
        let ch = match bits {
            b if b == up | down | left | right => '┼',
            b if b == up | down | left => '┤',
            b if b == up | down | right => '├',
            b if b == left | right | up => '┴',
            b if b == left | right | down => '┬',
            b if b == down | right => '┌',
            b if b == down | left => '┐',
            b if b == up | right => '└',
            b if b == up | left => '┘',
            b if b & (up | down) != 0 => '│',
            _ => '─',
        };
        cv.put(x, y, ch, st);
    }
}

/// What an internal USB device is, for where it's drawn.
#[derive(PartialEq, Clone, Copy)]
enum UsbRole {
    /// Bluetooth: on the Wi-Fi card, which carries it (Intel AX2xx & co.)
    Bluetooth,
    /// A touchscreen: inside the built-in display
    Touch,
    Other,
}

fn usb_role(p: &Part) -> UsbRole {
    let t = format!("{} {}", p.title, p.tag).to_lowercase();
    if t.contains("bluetooth") || p.tag == "Wireless" {
        UsbRole::Bluetooth
    } else if t.contains("touchscreen") || t.contains("touch screen") || t.contains("wacom") {
        UsbRole::Touch
    } else {
        UsbRole::Other
    }
}

/// An internal USB device placed on the board: where, which part, which
/// header it hangs off, and its label.
struct UsbPlaced {
    r: Rect,
    pi: usize,
    si: usize,
    label: String,
}

/// Internal USB devices as parts of their own, each with an arrow to the
/// internal USB header it hangs off: Bluetooth as a badge on the Wi-Fi card
/// (top row, clear of its bars), a touchscreen inside the built-in display,
/// anything else in the nearest free spot of the board next to the header.
/// (Exploded, they fly out like the rest instead.)
fn plan_internal_usb(app: &App, xf: &Xf) -> Vec<UsbPlaced> {
    let b = app.board();
    let parts = &app.snap.parts;
    let pcb = b
        .pcb
        .as_ref()
        .map(|p| xf.rect(p.x, p.y, p.w, p.h))
        .unwrap_or_else(|| xf.rect(0, 0, b.width, b.height));
    let mut taken: Vec<Rect> = b.slots.iter().map(|s| xf.slot(s)).collect();
    let host = |k: Kind, bind: Option<&str>| {
        b.slots.iter().enumerate().find_map(|(si, s)| {
            let ok = app.asg.slots[si].iter().any(|&i| {
                parts[i].present
                    && parts[i].kind == k
                    && bind.map_or(true, |bn| parts[i].bind.iter().any(|x| x == bn))
            });
            ok.then(|| xf.slot(s))
        })
    };
    let mut out = Vec::new();
    for (si, s) in b.slots.iter().enumerate() {
        if s.kind() != Kind::UsbInternal {
            continue;
        }
        let hub = xf.slot(s);
        for &pi in &app.asg.slots[si] {
            let p = &parts[pi];
            if !p.present {
                continue;
            }
            let role = usb_role(p);
            let label = match role {
                UsbRole::Bluetooth => "BT".to_string(),
                UsbRole::Touch => "Touchscreen".to_string(),
                UsbRole::Other => p.title.clone(),
            };
            let badge = |h: Rect, top: bool| {
                let w = (label.chars().count() as u16 + 2).min(h.width.saturating_sub(2));
                let y = if top { h.y } else { h.y + h.height - 2 };
                Rect::new(h.x + h.width - 1 - w, y, w, 1)
            };
            let r = match role {
                UsbRole::Bluetooth => host(Kind::Wifi, None)
                    .filter(|h| h.width >= 6 && h.height >= 2)
                    .map(|h| badge(h, true)),
                UsbRole::Touch => host(Kind::Display, Some("internal"))
                    .filter(|h| h.width >= 8 && h.height >= 3)
                    .map(|h| badge(h, false)),
                UsbRole::Other => None,
            };
            let r = match r {
                Some(r) => r,
                None => {
                    let w = (label.chars().count() as u16 + 4).clamp(8, 22);
                    match free_spot(pcb, &taken, hub, w, 3) {
                        Some(r) => r,
                        None => continue, // no room: it stays counted in the header
                    }
                }
            };
            taken.push(r);
            out.push(UsbPlaced { r, pi, si, label });
        }
    }
    out
}

fn draw_internal_usb(cv: &mut Cv, app: &App, u: &UsbPlaced, hits: &mut Vec<(Rect, String)>) {
    let p = &app.snap.parts[u.pi];
    let sel = app.is_sel(&p.id);
    if u.r.height == 1 {
        let st = Style::new().fg(HI).bg(CHIP).add_modifier(Modifier::BOLD);
        cv.fill(u.r, ' ', st);
        cv.ctext(u.r, u.r.y as i32, &u.label, st);
        if sel {
            cv.restyle(u.r, Style::new().add_modifier(Modifier::REVERSED));
        }
    } else {
        // a pinned package with the device's name, like the catch-all chips
        let es = edge_style(sel);
        cv.fill(u.r, ' ', Style::new().bg(CHIP));
        cv.frame(u.r, &CHIPB, es);
        let bot = (u.r.y + u.r.height - 1) as i32;
        for x in (u.r.x as i32 + 2..(u.r.x + u.r.width) as i32 - 2).step_by(2) {
            cv.put(x, u.r.y as i32, '╤', es);
            cv.put(x, bot, '╧', es);
        }
        let inner = shrink(u.r);
        cv.ctext(
            inner,
            (inner.y + inner.height / 2) as i32,
            &u.label,
            Style::new().fg(HI).bg(CHIP).add_modifier(Modifier::BOLD),
        );
    }
    hits.push((u.r, p.id.clone()));
}

/// The nearest place to `near` for a `w`×`h` rectangle inside `pcb` that
/// keeps a cell clear of everything in `taken`.
fn free_spot(pcb: Rect, taken: &[Rect], near: Rect, w: u16, h: u16) -> Option<Rect> {
    let clear = |r: Rect| {
        r.x > pcb.x
            && r.y > pcb.y
            && r.x + r.width < pcb.x + pcb.width
            && r.y + r.height < pcb.y + pcb.height
            && taken.iter().all(|t| {
                r.x + r.width < t.x
                    || t.x + t.width < r.x
                    || r.y + r.height <= t.y
                    || t.y + t.height <= r.y
            })
    };
    let (cx, cy) = (
        near.x as i32 + near.width as i32 / 2,
        near.y as i32 + near.height as i32 / 2,
    );
    let mut best: Option<(i32, Rect)> = None;
    for y in pcb.y as i32..(pcb.y + pcb.height) as i32 - h as i32 {
        for x in pcb.x as i32..(pcb.x + pcb.width) as i32 - w as i32 {
            let r = Rect::new(x as u16, y as u16, w, h);
            if !clear(r) {
                continue;
            }
            // distance between centres, rows counting double (cells are tall)
            let d = (x + w as i32 / 2 - cx).abs() + 2 * (y + h as i32 / 2 - cy).abs();
            if best.map_or(true, |(bd, _)| d < bd) {
                best = Some((d, r));
            }
        }
    }
    best.map(|(_, r)| r)
}

/// The board's name, silkscreened on the PCB where no slot covers it:
/// bottom-right if free, else sliding left, then rows further up. Drawn
/// after slots and cables, so it is never hidden.
fn board_label(cv: &mut Cv, app: &App, xf: &Xf) {
    let b = app.board();
    let pcb = b
        .pcb
        .as_ref()
        .map(|p| xf.rect(p.x, p.y, p.w, p.h))
        .unwrap_or_else(|| xf.rect(0, 0, b.width, b.height));
    let pc = b
        .color
        .as_deref()
        .and_then(parse_color)
        .unwrap_or(Color::Rgb(18, 53, 36));
    let d = &app.snap.dmi;
    let name = [d.get("board_vendor"), d.get("board_name")]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if pcb.width <= 8 || pcb.height <= 3 || name.is_empty() {
        return;
    }
    let n = (name.chars().count() as i32).min(pcb.width as i32 - 4);
    let slots: Vec<Rect> = b.slots.iter().map(|s| xf.slot(s)).collect();
    // free = no slot within the text's row, with a one-cell gap either side
    let free = |x: i32, y: i32| {
        !slots.iter().any(|r| {
            y >= r.y as i32
                && y < (r.y + r.height) as i32
                && x + n + 1 > r.x as i32
                && x - 1 < (r.x + r.width) as i32
        })
    };
    let (x0, x1) = (pcb.x as i32 + 2, pcb.x as i32 + pcb.width as i32 - 2 - n);
    let (top, bottom) = (pcb.y as i32 + 1, (pcb.y + pcb.height) as i32 - 2);
    let (x, y) = (top..=bottom)
        .rev()
        .find_map(|y| (x0..=x1).rev().find(|&x| free(x, y)).map(|x| (x, y)))
        .unwrap_or((x1, bottom));
    cv.text(
        x,
        y,
        &name,
        n,
        Style::new()
            .fg(Color::White)
            .bg(pc)
            .add_modifier(Modifier::BOLD),
    );
}

/// Rounded outline along the inside edge of a union of rectangles.
fn outline(cv: &mut Cv, rects: &[Rect], st: Style) {
    let inside = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && rects.iter().any(|r| {
                (x as u16) >= r.x
                    && (x as u16) < r.x + r.width
                    && (y as u16) >= r.y
                    && (y as u16) < r.y + r.height
            })
    };
    // a border cell is inside with at least one of its 8 neighbours outside
    let border = |x: i32, y: i32| {
        inside(x, y) && (-1..=1).any(|dy| (-1..=1).any(|dx| !inside(x + dx, y + dy)))
    };
    // two neighbouring border cells are joined if they touch the same outside cell
    let joined = |x: i32, y: i32, dx: i32, dy: i32| {
        let (nx, ny) = (x + dx, y + dy);
        border(nx, ny)
            && (-1..=1).any(|oy| {
                (-1..=1).any(|ox| {
                    let (cx, cy) = (x + ox, y + oy);
                    !inside(cx, cy) && (cx - nx).abs() <= 1 && (cy - ny).abs() <= 1
                })
            })
    };
    let x0 = rects.iter().map(|r| r.x).min().unwrap_or(0) as i32;
    let y0 = rects.iter().map(|r| r.y).min().unwrap_or(0) as i32;
    let x1 = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0) as i32;
    let y1 = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0) as i32;
    for y in y0..y1 {
        for x in x0..x1 {
            if !border(x, y) {
                continue;
            }
            let ch = match (
                joined(x, y, 0, -1),
                joined(x, y, 0, 1),
                joined(x, y, -1, 0),
                joined(x, y, 1, 0),
            ) {
                (false, true, false, true) => '╭',
                (false, true, true, false) => '╮',
                (true, false, false, true) => '╰',
                (true, false, true, false) => '╯',
                (true, true, false, false) => '│',
                (false, false, true, true) => '─',
                (u, d, _, _) if u || d => '│',
                _ => '─',
            };
            cv.put(x, y, ch, st);
        }
    }
}

/// The LAN controller an RJ45 jack is wired to: the Ethernet part in its
/// `link` slot; without a `link`, the first Ethernet controller found.
fn jack_part<'a>(app: &'a App, s: &SlotDef) -> Option<&'a Part> {
    let parts = &app.snap.parts;
    let eth = |p: &&Part| p.present && p.kind == Kind::Ethernet;
    match &s.link {
        Some(l) => {
            let ti = app.board().slots.iter().position(|t| &t.id == l)?;
            app.asg.slots[ti].iter().map(|&i| &parts[i]).find(eth)
        }
        None => parts.iter().find(eth),
    }
}

fn slot_key(s: &SlotDef) -> String {
    format!("slot:{}", s.id)
}

// ---------------------------------------------------------------- views

/// Several soldered parts sharing one area (catch-all slot): one chip each.
/// A short chip label for a catch-all device ("Audio device" → "Audio").
fn short_label(p: &Part) -> String {
    let t = if p.tag.is_empty() { &p.title } else { &p.tag };
    let l = t.to_lowercase();
    let pick = [
        ("audio", "Audio"),
        ("usb", "USB"),
        ("smbus", "SMBus"),
        ("serial bus", "SMBus"),
        ("system peripheral", "Sys"),
        ("signal processing", "DSP"),
        ("encryption", "Crypto"),
        ("memory controller", "MemCtl"),
        ("bridge", "Bridge"),
        ("communication", "Comm"),
        ("network", "Net"),
        ("ethernet", "LAN"),
        ("thunderbolt", "TB"),
        ("sd host", "SD"),
        ("card reader", "SD"),
        ("non-volatile", "NVMe"),
        ("sata", "SATA"),
    ];
    pick.iter()
        .find(|(k, _)| l.contains(k))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| t.split_whitespace().next().unwrap_or("Chip").to_string())
}

/// Several soldered parts sharing one area (catch-all slot): a grid of
/// chips kept roughly square, each labelled and clickable; pinned packages
/// when there's room, one-line badges when there isn't.
fn draw_chip_group(cv: &mut Cv, r: Rect, ps: &[&Part], app: &App, hits: &mut Vec<(Rect, String)>) {
    let n = ps.len() as u16;
    if n == 0 || r.width == 0 || r.height == 0 {
        return;
    }
    // columns × rows: the layout whose cells come closest to square
    // (two columns per row, as cells are tall), favouring bigger cells
    let (cols, rows) = (1..=n)
        .map(|c| (c, n.div_ceil(c)))
        .filter(|&(c, rw)| rw <= r.height && c <= r.width)
        .max_by_key(|&(c, rw)| {
            let cw = (r.width + 1) / c - 1;
            let ch = (r.height + 1) / rw - 1;
            (cw / 2).min(ch.max(1)) * 100 + cw.min(ch * 2)
        })
        .unwrap_or((n.min(r.width.max(1)), 1));
    let cw = ((r.width + 1) / cols).max(2) - 1;
    let ch = ((r.height + 1) / rows).max(2) - 1;
    for (i, p) in ps.iter().enumerate() {
        let (col, row) = (i as u16 % cols, i as u16 / cols);
        let c = Rect::new(r.x + col * (cw + 1), r.y + row * (ch + 1), cw, ch.max(1));
        if c.y >= r.y + r.height {
            break;
        }
        let sel = app.is_sel(&p.id);
        let label = short_label(p);
        let es = edge_style(sel);
        cv.fill(c, ' ', Style::new().bg(CHIP).fg(Color::Rgb(215, 215, 215)));
        if c.height >= 3 && c.width >= 5 {
            // a package with pins, like the PCH and BIOS chips
            cv.frame(c, &CHIPB, es);
            let bot = (c.y + c.height - 1) as i32;
            for x in (c.x as i32 + 2..(c.x + c.width) as i32 - 2).step_by(2) {
                cv.put(x, c.y as i32, '╤', es);
                cv.put(x, bot, '╧', es);
            }
            let inner = shrink(c);
            cv.ctext(
                inner,
                (inner.y + inner.height / 2) as i32,
                &label,
                Style::new().fg(HI).bg(CHIP).add_modifier(Modifier::BOLD),
            );
        } else {
            let st = Style::new().fg(HI).bg(CHIP).add_modifier(Modifier::BOLD);
            cv.ctext(c, (c.y + c.height / 2) as i32, &label, st);
            if sel {
                cv.restyle(c, Style::new().add_modifier(Modifier::REVERSED));
            }
        }
        hits.push((c, p.id.clone()));
    }
}

struct Item {
    pi: usize,
    si: Option<usize>,
    left: bool,
}

/// Semi-exploded (default): peripherals sit beside the board with arrows to
/// their connectors; USB devices hang off their ports; RAM, drives and cards
/// are plugged in. Exploded: those plugged-in modules fly out as well.
fn scene(cv: &mut Cv, area: Rect, app: &App, hits: &mut Vec<(Rect, String)>) {
    let b = app.board();
    let parts = &app.snap.parts;
    let t = app.secs();
    let exploded = app.exploded;

    // which parts leave their slot
    let flies = |s: &SlotDef| {
        s.kind() == Kind::Usb || (exploded && !s.soldered && s.link.is_none() && !s.catchall)
    };
    let mut items: Vec<Item> = Vec::new();
    for (si, s) in b.slots.iter().enumerate() {
        if !flies(s) {
            continue;
        }
        for &pi in &app.asg.slots[si] {
            if parts[pi].present {
                let cx = s.x as i32 * 2 + s.w as i32;
                items.push(Item {
                    pi,
                    si: Some(si),
                    left: cx < b.width as i32,
                });
            }
        }
    }
    let mut nl = items.iter().filter(|i| i.left).count();
    let mut nr = items.len() - nl;
    for &pi in &app.asg.unplaced {
        let left = nl < nr;
        if left {
            nl += 1
        } else {
            nr += 1
        }
        items.push(Item { pi, si: None, left });
    }

    // columns only where needed
    let colw: u16 = (area.width / 5).clamp(16, 30);
    let lw = if nl > 0 { colw + 4 } else { 0 };
    let rw = if nr > 0 { colw + 4 } else { 0 };
    if area.width < lw + rw + 24 || area.height < 8 {
        cv.text(
            area.x as i32 + 1,
            area.y as i32,
            "Terminal too small",
            area.width as i32 - 2,
            Style::new().fg(HI),
        );
        return;
    }
    let left = Rect::new(area.x, area.y, colw, area.height);
    let right = Rect::new(area.x + area.width - colw, area.y, colw, area.height);
    let mid = Rect::new(area.x + lw, area.y, area.width - lw - rw, area.height);
    let xf = fit(b, mid, if exploded { 1.4 } else { 1.6 });
    board_bg(cv, app, &xf, hits);
    if b.traces {
        traces(cv, app, &xf);
    }

    struct Placed {
        pi: usize,
        si: Option<usize>,
        pic: Rect,
        info: Rect,
        left: bool,
    }
    let mut placed: Vec<Placed> = Vec::new();
    let mut overflow = [0usize; 2];
    for (side, col) in [(true, left), (false, right)] {
        let mut v: Vec<&Item> = items.iter().filter(|i| i.left == side).collect();
        if v.is_empty() {
            continue;
        }
        // stack in the order of the slots they point at
        v.sort_by_key(|i| {
            i.si.map(|si| xf.slot(&b.slots[si]).y as i32)
                .unwrap_or(i32::MAX)
        });
        let n = v.len() as i32;
        let h = col.height as i32;
        let item_h = (h / n).clamp(2, 8);
        let pic_h = if item_h >= 6 { 3 } else { 1 };
        let info_h = (item_h - pic_h - 1).clamp(0, 4);
        let bottom = col.y as i32 + h;
        let mut cursor = col.y as i32;
        for (k, it) in v.iter().enumerate() {
            let want = match it.si {
                Some(si) => {
                    let r = xf.slot(&b.slots[si]);
                    r.y as i32 + r.height as i32 / 2 - pic_h / 2
                }
                None => cursor,
            };
            let latest = bottom - (n - k as i32) * item_h;
            let y = want.min(latest).max(cursor);
            if y + pic_h + info_h > bottom {
                overflow[if side { 0 } else { 1 }] += 1;
                continue;
            }
            placed.push(Placed {
                pi: it.pi,
                si: it.si,
                pic: Rect::new(col.x, y as u16, col.width, pic_h as u16),
                info: Rect::new(col.x, (y + pic_h) as u16, col.width, info_h as u16),
                left: side,
            });
            cursor = y + pic_h + info_h + 1;
        }
    }

    // leader lines and cables (under the chips)
    let mut markers: Vec<(i32, i32, char, Style)> = Vec::new();
    for p in &placed {
        let Some(si) = p.si else { continue };
        let sr = xf.slot(&b.slots[si]);
        let st = edge_style(app.is_sel(&parts[p.pi].id));
        let sy = p.pic.y as i32 + p.pic.height as i32 / 2;
        let sx = if p.left {
            (p.pic.x + p.pic.width) as i32
        } else {
            p.pic.x as i32 - 1
        };
        markers.push(leader(cv, sx, sy, p.left, sr, st));
    }
    for (si, s) in b.slots.iter().enumerate() {
        let Some(to) = s
            .link
            .as_ref()
            .and_then(|l| b.slots.iter().position(|t| &t.id == l))
        else {
            continue;
        };
        let ps = || app.asg.slots[si].iter().map(|&i| &parts[i]);
        if ps().any(|p| p.online == Some(false)) {
            continue; // an unplugged power source
        }
        // nothing reported (a fan the OS can't see, say): it is still there
        // and still cabled, drawn dimmer; but no phantom power source
        let empty = ps().all(|p| !p.present);
        if empty && s.kind() == Kind::Power {
            continue;
        }
        let (src, dst) = (xf.slot(s), xf.slot(&b.slots[to]));
        let sel = app.asg.slots[si].iter().any(|&i| app.is_sel(&parts[i].id))
            || app.is_sel(&slot_key(s))
            || app.is_sel(&slot_key(&b.slots[to]));
        let cable = if empty && !sel {
            Style::new().fg(Color::Rgb(70, 72, 82))
        } else {
            edge_style(sel)
        };
        let dcx = dst.x as i32 + dst.width as i32 / 2;
        if dcx > src.x as i32 && dcx < (src.x + src.width) as i32 - 1 {
            // straight down (or up) into the connector
            let st = cable;
            let (y0, y1, arrow) = if dst.y >= src.y + src.height {
                ((src.y + src.height) as i32, dst.y as i32 - 1, '▼')
            } else {
                ((dst.y + dst.height) as i32, src.y as i32 - 1, '▲')
            };
            for y in y0..=y1 {
                cv.put(dcx, y, '│', st);
            }
            markers.push((dcx, if arrow == '▼' { y1 } else { y0 }, arrow, st));
            continue;
        }
        let from_left =
            src.x as i32 + (src.width as i32) / 2 < dst.x as i32 + (dst.width as i32) / 2;
        let sx = if from_left {
            (src.x + src.width) as i32
        } else {
            src.x as i32 - 1
        };
        let lo = src.y as i32 + (src.height > 2) as i32;
        let hi = (src.y as i32 + src.height as i32 - 1 - (src.height > 2) as i32).max(lo);
        let sy = (dst.y as i32 + dst.height as i32 / 2).clamp(lo, hi);
        markers.push(leader(cv, sx, sy, from_left, dst, cable));
    }

    // internal USB devices: placed now, their arrows drawn under the parts
    let usb_placed = if exploded {
        Vec::new()
    } else {
        plan_internal_usb(app, &xf)
    };
    for u in &usb_placed {
        let sel = app.is_sel(&parts[u.pi].id) || app.is_sel(&slot_key(&b.slots[u.si]));
        let hub = xf.slot(&b.slots[u.si]);
        let from_left = u.r.x + u.r.width / 2 < hub.x + hub.width / 2;
        let sx = if from_left {
            (u.r.x + u.r.width) as i32
        } else {
            u.r.x as i32 - 1
        };
        let sy = (u.r.y + u.r.height / 2) as i32;
        markers.push(leader(cv, sx, sy, from_left, hub, edge_style(sel)));
    }

    // A slot lying inside another one is part of it (e.g. the GPU on an
    // SoC): the outer slot becomes a package, its own part is drawn above
    // the inner slots. Draw outer slots first so the inner ones end up on top
    // (and win clicks, which look at the last hit first).
    let inside = |t: &SlotDef, s: &SlotDef| {
        !std::ptr::eq(t, s)
            && t.x >= s.x
            && t.y >= s.y
            && t.x + t.w <= s.x + s.w
            && t.y + t.h <= s.y + s.h
    };
    let mut order: Vec<usize> = (0..b.slots.len()).collect();
    order.sort_by_key(|&i| b.slots.iter().filter(|s| inside(&b.slots[i], s)).count());
    // an empty slot inside another isn't drawn (a CPU without integrated
    // graphics keeps its whole area), so it doesn't make a package either
    let hidden = |ti: usize| {
        let t = &b.slots[ti];
        b.slots.iter().any(|s| inside(t, s)) && app.asg.slots[ti].iter().all(|&i| !parts[i].present)
    };

    // slots
    for si in order {
        if hidden(si) {
            continue;
        }
        let s = &b.slots[si];
        let mut r = xf.slot(s);
        let mut packaged = false;
        let inner_top = b
            .slots
            .iter()
            .enumerate()
            .filter(|(ti, t)| inside(t, s) && !hidden(*ti))
            .map(|(_, t)| xf.slot(t).y)
            .min();
        if let (Some(top), false) = (inner_top, s.catchall) {
            // the package: substrate and outline around everything inside
            let sel = app.asg.slots[si].iter().any(|&i| app.is_sel(&parts[i].id));
            cv.bg(r, PACKAGE);
            cv.frame(
                r,
                &ROUND,
                if sel {
                    edge_style(true)
                } else {
                    Style::new().fg(lighten(PACKAGE, 40))
                },
            );
            let key = app.asg.slots[si]
                .iter()
                .map(|&i| &parts[i])
                .find(|p| p.present)
                .map_or_else(|| slot_key(s), |p| p.id.clone());
            hits.push((r, key));
            if top > r.y + 2 && r.width > 2 {
                r = Rect::new(r.x + 1, r.y + 1, r.width - 2, top - r.y - 1);
                packaged = true;
            }
        }
        let ps = &app.asg.slots[si];
        let present: Vec<&Part> = ps
            .iter()
            .map(|&i| &parts[i])
            .filter(|p| p.present)
            .collect();
        if s.catchall {
            draw_chip_group(cv, r, &present, app, hits);
            continue;
        }
        if s.kind() == Kind::Rj45 {
            // selecting the jack selects the LAN controller behind it
            let lan = jack_part(app, s);
            let key = lan.map_or_else(|| slot_key(s), |p| p.id.clone());
            let label = s.label();
            let ctx = Ctx {
                mode: Mode::Board,
                sel: app.is_sel(&key),
                label: &label,
                edge: Edge::Left,
                align: 0,
                t,
                extra: None,
                packaged: false,
            };
            draw_rj45(cv, r, lan, &ctx);
            hits.push((r, key));
            continue;
        }
        let key = match present.len() {
            1 => present[0].id.clone(),
            _ => slot_key(s),
        };
        let label = s.label();
        let ctx = Ctx {
            mode: Mode::Board,
            sel: app.is_sel(&key),
            label: &label,
            edge: edge_of(Some(s), s.kind(), r),
            align: 0,
            t,
            extra: {
                // internal USB devices drawn on their own don't count here
                let n = present
                    .iter()
                    .filter(|p| !usb_placed.iter().any(|u| parts[u.pi].id == p.id))
                    .count();
                (n > 1).then(|| format!("{n} devices"))
            },
            packaged,
        };
        if flies(s) {
            if s.kind() == Kind::Usb {
                // the cable's plug stays in the port
                draw_port(cv, r, &ctx, false);
            } else {
                draw_empty_slot(cv, r, s, &ctx);
            }
        } else {
            let part = present.first().copied().or(ps.first().map(|&i| &parts[i]));
            draw(cv, r, Some(s), part, &ctx);
        }
        hits.push((r, key));
    }
    for (x, y, ch, st) in markers.drain(..) {
        cv.put(x, y, ch, st);
    }
    for u in &usb_placed {
        draw_internal_usb(cv, app, u, hits);
    }
    board_label(cv, app, &xf);

    // detached parts, with their data underneath
    for p in &placed {
        let part = &parts[p.pi];
        let sel = app.is_sel(&part.id);
        let slot = p.si.map(|si| &b.slots[si]);
        let label = slot
            .map(|s| s.label())
            .unwrap_or_else(|| part.kind.name().to_uppercase());
        let ctx = Ctx {
            mode: Mode::Pic,
            sel,
            label: &label,
            edge: pic_edge(p.left, part.kind),
            align: if p.left { 1 } else { -1 },
            t,
            extra: None,
            packaged: false,
        };
        draw(cv, p.pic, None, Some(part), &ctx);
        if sel {
            cv.frame(
                Rect {
                    height: p.pic.height + p.info.height,
                    ..p.pic
                },
                &PLAIN,
                edge_style(true),
            );
        }
        let w = p.info.width as i32;
        let mut y = p.info.y as i32;
        let end = y + p.info.height as i32;
        let title_st = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
        let mut lines: Vec<(String, Style)> = vec![(part.title.clone(), title_st)];
        if let Some(t) = part.temp {
            lines.push((
                format!("{}  {}", fmt_temp(t), label),
                Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
            ));
        }
        for l in pack(&part.metrics, w as usize) {
            lines.push((l, Style::new().fg(Color::Rgb(190, 190, 200))));
        }
        for (l, st) in lines {
            if y >= end {
                break;
            }
            cv.text(p.info.x as i32, y, &l, w, st);
            y += 1;
        }
        hits.push((
            Rect {
                height: p.pic.height + p.info.height,
                ..p.pic
            },
            part.id.clone(),
        ));
    }
    for (i, n) in overflow.iter().enumerate() {
        if *n > 0 {
            let col = if i == 0 { left } else { right };
            cv.text(
                col.x as i32,
                (col.y + col.height - 1) as i32,
                &format!("+{n} more (Tab)"),
                col.width as i32,
                Style::new().fg(LG),
            );
        }
    }
}

/// Flown-out modules lie horizontally with their contacts facing the board.
fn pic_edge(left: bool, kind: Kind) -> Edge {
    match kind {
        Kind::Dimm => Edge::Bottom,
        _ if left => Edge::Right,
        _ => Edge::Left,
    }
}

// ---------------------------------------------------------------- frame

/// Terminal window title, e.g. "Insider - Razer Blade".
pub fn window_title(app: &App) -> String {
    let g = |k: &str| {
        app.snap
            .dmi
            .get(k)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    let (vendor, product) = (g("sys_vendor"), g("product_name"));
    // don't repeat the vendor when the model name already starts with it
    let name = if !vendor.is_empty() && product.to_lowercase().starts_with(&vendor.to_lowercase()) {
        product
    } else {
        format!("{vendor} {product}").trim().to_string()
    };
    if name.is_empty() {
        "Insider".into()
    } else {
        format!("Insider - {name}")
    }
}

pub fn draw_frame(f: &mut Frame, app: &mut App) {
    let area = f.area();
    // no header or footer: the machine name is in the window title
    // (window_title), keys are under `?`, notes in the board's details
    // keep everything off the terminal's edge: a row top and bottom, two
    // columns either side (cells are about twice as tall as wide)
    let body = if area.width > 8 && area.height > 4 {
        Rect::new(area.x + 2, area.y + 1, area.width - 4, area.height - 2)
    } else {
        area
    };
    let (view, side) = if app.details && body.width >= 70 {
        let c = Layout::horizontal([Constraint::Min(40), Constraint::Length(PANEL_W)]).split(body);
        (c[0], Some(c[1]))
    } else if app.details {
        (body, Some(body))
    } else {
        (body, None)
    };

    let mut hits = Vec::new();
    {
        let mut cv = Cv {
            buf: f.buffer_mut(),
            clip: view,
        };
        scene(&mut cv, view, app, &mut hits);
    }
    app.hits = hits;

    app.panel = side;

    if let Some(side) = side {
        let (title, lines) = details(app);
        // only scroll when the text doesn't fit
        let rows = wrapped_rows(&lines, side.width.saturating_sub(2) as usize);
        app.max_scroll = rows.saturating_sub(side.height.saturating_sub(2) as usize) as u16;
        app.scroll = app.scroll.min(app.max_scroll);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(LG))
            .title(Line::from(Span::styled(
                format!(" {} ", trunc(&title, side.width.saturating_sub(4) as usize)),
                Style::new().fg(HI).add_modifier(Modifier::BOLD),
            )));
        f.render_widget(Clear, side);
        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((app.scroll, 0)),
            side,
        );
    }

    if app.help {
        let w = 64.min(area.width);
        let h = 18.min(area.height);
        let r = Rect::new(
            area.x + (area.width - w) / 2,
            area.y + (area.height - h) / 2,
            w,
            h,
        );
        let k = |s: &str| {
            Span::styled(
                format!("{s:<16}"),
                Style::new().fg(HI).add_modifier(Modifier::BOLD),
            )
        };
        let lines = vec![
            Line::from(vec![
                k("e x Space"),
                Span::raw("semi-exploded ⇄ exploded view"),
            ]),
            Line::from(vec![
                k("mouse click"),
                Span::raw("select a part and show details"),
            ]),
            Line::from(vec![k("Tab Shift-Tab"), Span::raw("cycle through parts")]),
            Line::from(vec![
                k("arrows / hjkl"),
                Span::raw("move selection spatially"),
            ]),
            Line::from(vec![k("Enter d i"), Span::raw("toggle the detail panel")]),
            Line::from(vec![k("PgUp PgDn wheel"), Span::raw("scroll details")]),
            Line::from(vec![
                k("Esc, click away"),
                Span::raw("close panel / clear selection"),
            ]),
            Line::from(vec![k("q Ctrl-C"), Span::raw("quit")]),
            Line::raw(""),
            Line::raw("Peripherals (screen, keyboard, battery, fans, USB devices…)"),
            Line::raw("always point at their connector. Exploded view also pulls"),
            Line::raw("RAM, drives and cards out of their slots."),
            Line::raw(""),
            Line::raw(crate::platform::elevate_hint()),
        ];
        f.render_widget(Clear, r);
        f.render_widget(
            Paragraph::new(lines).wrap(Wrap { trim: false }).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(LG))
                    .title(" help "),
            ),
            r,
        );
    }
}

/// Rows a word-wrapped paragraph takes at width w.
fn wrapped_rows(lines: &[Line], w: usize) -> usize {
    if w == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|l| {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            let (mut rows, mut cur) = (1, 0usize);
            for word in text.split_inclusive(' ') {
                let n = word.chars().count();
                if cur + n.min(w) > w && cur > 0 {
                    rows += 1;
                    cur = 0;
                }
                if n > w {
                    rows += (n - 1) / w;
                    cur = n % w;
                } else {
                    cur += n;
                }
            }
            rows
        })
        .sum()
}

/// A detail entry, stacked to keep the panel narrow: the key, its value
/// below, then a blank line.
fn kv<'a>(k: &str, v: &str) -> Vec<Line<'a>> {
    vec![
        Line::styled(format!("{k}:"), Style::new().fg(Color::Rgb(120, 190, 200))),
        Line::raw(v.to_string()),
        Line::raw(""),
    ]
}

fn part_lines(p: &Part) -> Vec<Line<'static>> {
    let mut v = Vec::new();
    v.push(Line::from(Span::styled(
        format!("{}  ·  {}", p.kind.name().to_uppercase(), p.id),
        Style::new().fg(Color::DarkGray),
    )));
    if !p.present {
        v.push(Line::styled("(not present)", Style::new().fg(Color::Gray)));
    }
    if let Some(t) = p.temp {
        v.push(Line::raw(""));
        v.push(Line::styled(
            "Temperature:",
            Style::new().fg(Color::Rgb(120, 190, 200)),
        ));
        v.push(Line::styled(
            fmt_temp(t),
            Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
        ));
    }
    if !p.metrics.is_empty() {
        v.push(Line::raw(""));
        for m in &p.metrics {
            v.push(Line::styled(
                m.clone(),
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
    }
    if !p.cores.is_empty() {
        v.push(Line::raw(""));
        for c in &p.cores {
            let mut spans = vec![Span::styled(
                format!("Core {:<3}", c.id),
                Style::new().fg(Color::Rgb(120, 190, 200)),
            )];
            if let Some(t) = c.temp {
                spans.push(Span::styled(
                    format!("{:>5}", fmt_temp(t)),
                    Style::new().fg(tcolor(t)),
                ));
            }
            if let Some(g) = c.ghz {
                spans.push(Span::raw(format!("  {g:.2} GHz")));
            }
            if let Some(l) = c.load {
                spans.push(Span::raw(format!("  {l:>3.0}%")));
            }
            v.push(Line::from(spans));
        }
    }
    v.push(Line::raw(""));
    for (k, val) in &p.props {
        v.extend(kv(k, val));
    }
    v
}

fn details(app: &App) -> (String, Vec<Line<'static>>) {
    let parts = &app.snap.parts;
    let key = app.selected.clone().unwrap_or_else(|| "board".into());
    if let Some(id) = key.strip_prefix("slot:") {
        let b = app.board();
        let Some((si, s)) = b.slots.iter().enumerate().find(|(_, s)| s.id == id) else {
            return ("?".into(), vec![]);
        };
        let mut v = [
            kv("Slot", &s.label()),
            kv("Kind", s.kind().name()),
            kv("Soldered", if s.soldered { "yes" } else { "no" }),
        ]
        .concat();
        for (k, val) in &s.props {
            v.extend(kv(k, val));
        }
        let mut ps: Vec<&Part> = app.asg.slots[si]
            .iter()
            .map(|&i| &parts[i])
            .filter(|p| p.present)
            .collect();
        if s.kind() == Kind::Connector {
            for (ti, t) in b.slots.iter().enumerate() {
                if t.link.as_deref() == Some(s.id.as_str()) {
                    ps.extend(
                        app.asg.slots[ti]
                            .iter()
                            .map(|&i| &parts[i])
                            .filter(|p| p.present),
                    );
                }
            }
            if !ps.is_empty() {
                v.push(Line::raw(""));
                v.push(Line::styled("Cabled to", Style::new().fg(DIMTXT)));
            }
        }
        v.push(Line::raw(""));
        if ps.is_empty() {
            v.push(Line::styled("Empty", Style::new().fg(Color::Gray)));
            for &i in &app.asg.slots[si] {
                for (k, val) in &parts[i].props {
                    v.extend(kv(k, val));
                }
            }
        }
        for p in ps {
            v.push(Line::styled(
                p.title.clone(),
                Style::new().fg(HI).add_modifier(Modifier::BOLD),
            ));
            v.extend(part_lines(p));
            v.push(Line::raw(""));
        }
        return (s.label(), v);
    }
    let Some(p) = parts.iter().find(|p| p.id == key) else {
        return (
            "(gone)".into(),
            vec![Line::raw("This part disappeared (unplugged?)")],
        );
    };
    let mut v = part_lines(p);
    // what the layout says about the slot this part sits in
    let pi = parts.iter().position(|q| q.id == key);
    let slot = pi.and_then(|pi| {
        let si = app.asg.slots.iter().position(|s| s.contains(&pi))?;
        app.board().slots.get(si)
    });
    if let Some(s) = slot.filter(|s| !s.props.is_empty()) {
        v.push(Line::raw(""));
        v.push(Line::styled("From the layout", Style::new().fg(DIMTXT)));
        v.push(Line::raw(""));
        for (k, val) in &s.props {
            v.extend(kv(k, val));
        }
    }
    if p.kind == Kind::Board && (!app.snap.notes.is_empty() || !app.load_errors.is_empty()) {
        v.push(Line::raw(""));
        v.push(Line::styled(
            "Notes",
            Style::new()
                .fg(Color::Rgb(255, 150, 50))
                .add_modifier(Modifier::BOLD),
        ));
        for n in app.snap.notes.iter().chain(app.load_errors.iter()) {
            v.push(Line::raw(format!("• {n}")));
        }
    }
    (p.title.clone(), v)
}

#[cfg(test)]
mod bar_tests {
    use super::*;

    #[test]
    fn load_colors_follow_thresholds() {
        assert_eq!(load_color(0.69), Color::Rgb(35, 140, 65)); // green
        assert_eq!(load_color(0.75), Color::Rgb(185, 165, 30)); // yellow
        assert_eq!(load_color(0.85), Color::Rgb(215, 115, 25)); // orange
        assert_eq!(load_color(0.95), Color::Rgb(195, 45, 40)); // red
    }

    #[test]
    fn internal_usb_roles() {
        let p = |t: &str, tag: &str| {
            let mut p = Part::new("u", Kind::UsbInternal, t);
            p.tag = tag.into();
            p
        };
        assert!(usb_role(&p("Intel AX200 Bluetooth", "Wireless")) == UsbRole::Bluetooth);
        assert!(usb_role(&p("ELAN Touchscreen", "HID")) == UsbRole::Touch);
        assert!(usb_role(&p("Synaptics Fingerprint", "Vendor")) == UsbRole::Other);
    }

    #[test]
    fn catch_all_labels() {
        let p = |tag: &str| {
            let mut p = Part::new("c", Kind::Pci, "x");
            p.tag = tag.into();
            p
        };
        let labels: Vec<String> = [
            "Audio device",
            "USB controller",
            "Serial bus controller",
            "System peripheral",
        ]
        .iter()
        .map(|t| short_label(&p(t)))
        .collect();
        assert_eq!(labels, ["Audio", "USB", "SMBus", "Sys"]);
    }

    #[test]
    fn components_never_touch_and_connectors_stay_small() {
        let (boards, _) = crate::board::load_all();
        for b in &boards {
            for (w, h) in [(120, 35), (200, 50), (320, 90)] {
                let xf = fit(b, Rect::new(0, 0, w, h), 1.6);
                let top: Vec<(&SlotDef, Rect)> = b
                    .slots
                    .iter()
                    .filter(|s| nested_in(b, s).is_none())
                    .map(|s| (s, xf.slot(s)))
                    .collect();
                for (i, (s, a)) in top.iter().enumerate() {
                    if s.kind() == Kind::Connector {
                        assert!(a.height <= 2, "{} {}", b.name, s.id);
                        assert!(a.width <= s.label().chars().count() as u16 + 2 || a.width == 4);
                    }
                    for (t, z) in &top[i + 1..] {
                        if (s.x, s.y, s.w, s.h) == (t.x, t.y, t.w, t.h) {
                            continue;
                        }
                        let gx = span_gap(
                            a.x as i32,
                            (a.x + a.width) as i32,
                            z.x as i32,
                            (z.x + z.width) as i32,
                        );
                        let gy = span_gap(
                            a.y as i32,
                            (a.y + a.height) as i32,
                            z.y as i32,
                            (z.y + z.height) as i32,
                        );
                        // apart, or overlapping on purpose
                        let ok = gx >= 1 || gy >= 1 || gx.max(gy) < -2;
                        assert!(ok, "{} at {w}x{h}: {} touches {}", b.name, s.id, t.id);
                    }
                }
            }
        }
    }

    #[test]
    fn free_spot_avoids_parts_and_stays_near() {
        let pcb = Rect::new(0, 0, 40, 12);
        let hub = Rect::new(18, 5, 4, 2);
        let taken = [hub, Rect::new(23, 0, 17, 12)];
        let r = free_spot(pcb, &taken, hub, 8, 3).unwrap();
        // inside the board, and a cell clear of everything taken
        assert!(r.x > pcb.x && r.y > pcb.y && r.x + r.width < 23);
        for t in taken {
            let apart = r.x + r.width < t.x
                || t.x + t.width < r.x
                || r.y + r.height <= t.y
                || t.y + t.height <= r.y;
            assert!(apart, "{r:?} touches {t:?}");
        }
        assert!(free_spot(Rect::new(0, 0, 6, 4), &[], hub, 8, 3).is_none());
    }

    #[test]
    fn used_text() {
        let gi = 1024f64.powi(3);
        // memory in binary units, as sold
        assert_eq!(fmt_used(6.0 * gi, 8.0 * gi, true), "6.0/8GB");
        assert_eq!(fmt_used(3.8 * gi, 32.0 * gi, true), "3.8/32GB");
        // drives in decimal units, as sold
        assert_eq!(fmt_used(39e9, 128e9, false), "39/128GB");
        assert_eq!(fmt_used(0.6e12, 1.0e12, false), "0.6/1.0TB");
    }

    #[test]
    fn bars_per_kind() {
        let gi = 1024f64.powi(3);
        let texts = |p: &Part| -> Vec<Vec<String>> {
            bars_for(p)
                .iter()
                .map(|r| r.iter().map(|(b, _)| b.text.clone()).collect())
                .collect()
        };
        // RAM: speed, then one gray bar with the installed total and the share
        // of what the OS can use
        let mut d = Part::new("d", Kind::Dimm, "RAM");
        d.usage = Some((3.8 * gi, 30.6 * gi));
        d.capacity = Some(32.0 * gi);
        d.speed = Some((2666.0, 3200.0));
        assert_eq!(texts(&d), [vec!["2666/3200 MT/s"], vec!["3.8/32GB (12%)"]]);
        // a USB stick: its 128 GB, not its filesystem's 116 GiB
        let mut u = Part::new("u", Kind::Usb, "Stick");
        u.io = Some((0.0, 0.0));
        u.usage = Some((39e9, 124e9));
        u.capacity = Some(128e9);
        assert_eq!(texts(&u)[1], ["39/128GB (31%)"]);
        // receive and send share a row, sized by their maxima (1:4)
        let mut w = Part::new("w", Kind::Wifi, "Wi-Fi");
        w.io = Some((0.0, 0.0));
        w.io_max = Some((1e6, 4e6));
        let r = &bars_for(&w)[0];
        assert_eq!((r[0].1, r[1].1), (1e6, 4e6));
        // Ethernet: half and half, filled against the link speed
        let mut e = Part::new("e", Kind::Ethernet, "LAN");
        e.link = Some((true, 1000));
        e.io = Some((62.5e6, 0.0));
        let r = &bars_for(&e)[0];
        assert!((r[0].0.frac - 0.5).abs() < 1e-9 && r[1].0.frac == 0.0);
        // speaker volume from its "40%" when no level is set; no fan bar without PWM
        let mut s = Part::new("s", Kind::Speaker, "Speaker");
        s.metrics = vec!["40%".into()];
        assert_eq!(texts(&s), [vec!["40%"]]);
        assert!(bars_for(&Part::new("f", Kind::Fan, "Fan")).is_empty());
    }
}
