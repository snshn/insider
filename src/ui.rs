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
const CHIP: Color = Color::Rgb(18, 18, 21);
/// Drive activity LED, like the HDD light on old cases.
const LED_DISK: Color = Color::Rgb(255, 48, 28);
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

struct Xf {
    ox: i32,
    oy: i32,
    s: f32,
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
        self.rect(s.x, s.y, s.w, s.h)
    }
}

fn fit(b: &BoardDef, area: Rect, max_s: f32) -> Xf {
    let s = (area.width as f32 / b.width as f32)
        .min(area.height as f32 / b.height as f32)
        .min(max_s)
        .max(0.05);
    let w = (b.width as f32 * s).round() as i32;
    let h = (b.height as f32 * s).round() as i32;
    Xf {
        ox: area.x as i32 + (area.width as i32 - w) / 2,
        oy: area.y as i32 + (area.height as i32 - h) / 2,
        s,
    }
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
    let (strip, body) = contacts(m, if camm { ctx.edge } else { ctx.edge });
    let horiz = strip.height == 1;
    if camm {
        // compression connector: a pad field along the edge, screw holes at its ends
        let band = if horiz {
            Rect::new(m.x + 2, strip.y, m.width.saturating_sub(4), 1)
        } else {
            Rect::new(strip.x, m.y + 1, 1, m.height.saturating_sub(2))
        };
        cv.fill(band, '∷', Style::new().fg(GOLD).bg(GREEN_PCB));
        let hole = Style::new().fg(METAL).bg(GREEN_PCB);
        if horiz {
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
        pads(cv, strip, GREEN_PCB, true);
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
    let text_r = if camm && horiz { body } else { body };
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
    let m = aspect(r, if wifi { 2.8 } else { 7.3 }, ctx.align);
    let pcb = if wifi { GREEN_PCB } else { BLACK_PCB };
    cv.bg(m, pcb);
    let (strip, body) = contacts(m, ctx.edge);
    pads(cv, strip, pcb, false);
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

/// An RJ45 jack: a metal shell with the opening (holding the cable's plug
/// when there is a link) and two LEDs, like on a real one: link (left; green
/// at gigabit and up, amber below) and activity (right; flickers with traffic).
fn draw_rj45(cv: &mut Cv, r: Rect, lan: Option<&Part>, ctx: &Ctx) {
    cv.bg(r, METAL);
    let (x0, y0) = (r.x as i32, r.y as i32);
    let x1 = x0 + r.width as i32 - 1;
    let (up, mbps) = lan.and_then(|p| p.link).unwrap_or((false, 0));
    if r.width >= 4 && r.height >= 3 {
        let dark = Color::Rgb(30, 30, 34);
        let open = Rect::new(r.x + 1, r.y + 1, r.width - 2, r.height - 2);
        if up {
            // clear plastic plug with its gold contacts
            let plug = Color::Rgb(196, 204, 212);
            cv.bg(open, plug);
            for x in open.x..open.x + open.width {
                cv.put(x as i32, open.y as i32, '╷', Style::new().fg(GOLD).bg(plug));
            }
        } else {
            cv.bg(open, dark);
        }
        // the latch notch below the opening
        let nw = (open.width / 2).max(1);
        let latch = Rect::new(open.x + (open.width - nw) / 2, r.y + r.height - 1, nw, 1);
        cv.bg(latch, if up { Color::Rgb(150, 156, 164) } else { dark });
    }
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

/// A port on the board: a solid metal box with an opening.
fn draw_port(cv: &mut Cv, r: Rect, plugged: Option<&Part>, ctx: &Ctx, header: bool) {
    cv.bg(r, METAL);
    let dark = Color::Rgb(30, 30, 34);
    if header {
        // pin header: two rows of pins
        let rows = [
            r.y + r.height / 2 - (r.height > 2) as u16,
            r.y + r.height / 2,
        ];
        for y in rows {
            for x in (r.x + 1..r.x + r.width.saturating_sub(1)).step_by(2) {
                cv.put(x as i32, y as i32, '•', Style::new().fg(dark));
            }
        }
        if let Some(e) = &ctx.extra {
            if r.height >= 3 {
                cv.ctext(r, (r.y + r.height - 1) as i32, e, Style::new().fg(dark));
            }
        }
    } else {
        let y = r.y + r.height / 2;
        let open = Rect::new(
            r.x + 1.min(r.width / 3),
            y,
            r.width.saturating_sub(2).max(1),
            1,
        );
        cv.bg(open, dark);
        if let Some(p) = plugged {
            let plug = Color::Rgb(90, 150, 220);
            cv.bg(
                Rect {
                    width: open.width.max(1),
                    ..open
                },
                plug,
            );
            if open.width >= 6 {
                cv.ctext(
                    open,
                    y as i32,
                    &p.tag,
                    Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
                );
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
    text_block(cv, body, &[p.tag.clone()], Color::White);
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
        cv.ctext(inner, y, &s, Style::new().fg(DIMTXT));
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
    let bh = g.height / rows as u16;
    if bw < 3 || bh == 0 {
        return;
    }
    for (i, c) in p.cores.iter().enumerate() {
        let (col, row) = ((i % cols) as u16, (i / cols) as u16);
        let b = Rect::new(
            g.x + col * bw,
            g.y + row * bh,
            bw - 1,
            bh.max(1) - (rows > 1 && bh > 2 && row == 0) as u16,
        );
        cv.bg(b, Color::Rgb(44, 47, 58));
        let inner = if b.height >= 3 && b.width >= 6 {
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
        if let Some(l) = c.load {
            lines.push((
                format!("{l:.0}%"),
                Style::new().fg(Color::Rgb(160, 160, 172)),
            ));
        }
        let k = lines.len().min(inner.height as usize);
        let y0 = inner.y as i32 + (inner.height as i32 - k as i32) / 2;
        for (j, (s, st)) in lines.iter().take(k).enumerate() {
            cv.ctext(inner, y0 + j as i32, s, *st);
        }
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

/// Generic chip/box: label + temperature on the first row inside the border,
/// then title and metrics.
fn draw_chip(cv: &mut Cv, r: Rect, kind: Kind, p: Option<&Part>, ctx: &Ctx) {
    let lk = look(kind);
    cv.fill(r, ' ', Style::new().bg(lk.bg).fg(lk.fg));
    let framed = r.height >= 3 && r.width >= 4;
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
    // first row: label (left) and temperature (right)
    if ctx.mode == Mode::Board && inner.height >= 3 {
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

    // CPU: a box per physical core under the title
    if kind == Kind::Cpu && ctx.mode == Mode::Board {
        if let Some(p) = p.filter(|p| !p.cores.is_empty()) {
            let text_h = (lines.len() as u16).min(2);
            if body.height >= text_h + 2 {
                let tr = Rect {
                    height: text_h,
                    ..body
                };
                text_block(cv, tr, &lines[..text_h as usize], lk.fg);
                let g = Rect::new(
                    body.x + 1,
                    body.y + text_h,
                    body.width.saturating_sub(1),
                    body.height - text_h,
                );
                draw_cpu_cores(cv, g, p);
                return;
            }
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
        cv.ctext(inner, my, label, Style::new().fg(DIMTXT));
    }
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
            Some(Kind::Usb) => return draw_port(cv, r, part, ctx, false),
            Some(Kind::UsbInternal) => return draw_port(cv, r, part, ctx, true),
            Some(Kind::Connector) => return draw_connector(cv, r, ctx),
            Some(Kind::Rj45) => return draw_rj45(cv, r, None, ctx),
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
        Kind::Usb => draw_port(cv, r, None, ctx, false),
        Kind::UsbInternal => draw_port(cv, r, None, ctx, true),
        Kind::Connector => draw_connector(cv, r, ctx),
        Kind::Rj45 => draw_rj45(cv, r, None, ctx),
        k @ (Kind::Dimm | Kind::Ssd | Kind::Wifi) => draw_module_slot(cv, r, k, None, ctx),
        _ => empty_slot(cv, r, ctx.label, ctx.sel),
    }
}

/// A cable connector on the board (eDP, battery...): black plastic, a row of pins.
fn draw_connector(cv: &mut Cv, r: Rect, ctx: &Ctx) {
    cv.bg(r, PLASTIC);
    let pin = Style::new().fg(Color::Rgb(185, 185, 192)).bg(PLASTIC);
    for x in r.x + 1..r.x + r.width.saturating_sub(1).max(1) {
        cv.put(x as i32, r.y as i32, '╷', pin);
    }
    if r.height >= 2 && r.width >= 3 {
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

fn draw_keyboard(cv: &mut Cv, r: Rect, p: Option<&Part>, ctx: &Ctx) {
    let deck = Color::Rgb(22, 22, 26);
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
    if let Some(v) = p.metrics.first() {
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
    let y = (r.y + r.height / 2) as i32;
    for x in (r.x..r.x + r.width).step_by(2) {
        cv.put(x as i32, y, '●', st);
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
    cv.bg(pcb, pc);
    cv.frame(pcb, &ROUND, Style::new().fg(lighten(pc, 45)));
    let d = &app.snap.dmi;
    let name = [d.get("board_vendor"), d.get("board_name")]
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    if pcb.width > 8 && pcb.height > 3 && !name.is_empty() {
        let n = (name.chars().count() as i32).min(pcb.width as i32 - 4);
        cv.text(
            pcb.x as i32 + pcb.width as i32 - 2 - n,
            (pcb.y + pcb.height - 2) as i32,
            &name,
            n,
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        );
    }
    hits.push((pcb, "board".into()));
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
fn draw_chip_group(cv: &mut Cv, r: Rect, ps: &[&Part], app: &App, hits: &mut Vec<(Rect, String)>) {
    let n = ps.len() as u16;
    if n == 0 {
        return;
    }
    let vertical = r.height / n >= 3 || r.height >= r.width / 2;
    for (i, p) in ps.iter().enumerate() {
        let i = i as u16;
        let c = if vertical {
            let h = r.height / n;
            Rect::new(r.x, r.y + i * h, r.width, h.saturating_sub(1).max(1))
        } else {
            let w = r.width / n;
            Rect::new(r.x + i * w, r.y, w.saturating_sub(1).max(1), r.height)
        };
        let ctx = Ctx {
            mode: Mode::Board,
            sel: app.is_sel(&p.id),
            label: &p.tag,
            edge: Edge::Left,
            align: 0,
            t: app.secs(),
            extra: None,
        };
        draw_chip(cv, c, Kind::Pci, Some(p), &ctx);
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
        if app.asg.slots[si].iter().all(|&i| !parts[i].present) {
            continue; // nothing there to cable up
        }
        let (src, dst) = (xf.slot(s), xf.slot(&b.slots[to]));
        let sel = app.asg.slots[si].iter().any(|&i| app.is_sel(&parts[i].id))
            || app.is_sel(&slot_key(s))
            || app.is_sel(&slot_key(&b.slots[to]));
        let dcx = dst.x as i32 + dst.width as i32 / 2;
        if dcx > src.x as i32 && dcx < (src.x + src.width) as i32 - 1 {
            // straight down (or up) into the connector
            let st = edge_style(sel);
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
        markers.push(leader(cv, sx, sy, from_left, dst, edge_style(sel)));
    }

    // slots
    for (si, s) in b.slots.iter().enumerate() {
        let r = xf.slot(s);
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
            extra: (present.len() > 1).then(|| format!("{} devices", present.len())),
        };
        if flies(s) {
            if s.kind() == Kind::Usb {
                // the cable's plug stays in the port
                draw_port(cv, r, present.first().copied(), &ctx, false);
            } else {
                draw_empty_slot(cv, r, s, &ctx);
            }
        } else {
            let part = present.first().copied().or(ps.first().map(|&i| &parts[i]));
            draw(cv, r, Some(s), part, &ctx);
        }
        hits.push((r, key));
    }
    for (x, y, ch, st) in markers {
        cv.put(x, y, ch, st);
    }

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
    // no header row: the machine name goes in the window title (window_title)
    let rows = Layout::vertical([Constraint::Min(4), Constraint::Length(1)]).split(area);
    let (body, foot) = (rows[0], rows[1]);
    let (view, side) = if app.details && body.width >= 70 {
        let c = Layout::horizontal([Constraint::Min(40), Constraint::Length(46)]).split(body);
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

    let mode = if app.exploded {
        " EXPLODED "
    } else {
        " SEMI-EXPLODED "
    };

    let notes = app.snap.notes.len() + app.load_errors.len();
    let k = |s: &'static str| Span::styled(s, Style::new().fg(HI).add_modifier(Modifier::BOLD));
    let ft = Line::from(vec![
        Span::styled(mode, Style::new().fg(Color::Black).bg(LG)),
        k(" e"),
        Span::raw(" toggle  "),
        k("click/Tab/arrows"),
        Span::raw(" select  "),
        k("Enter"),
        Span::raw(" details  "),
        k("?"),
        Span::raw(" help  "),
        k("q"),
        Span::raw(" quit"),
        Span::styled(
            if notes > 0 {
                format!("   ⚠ {notes} notes: select the board (Enter) to read")
            } else {
                String::new()
            },
            Style::new().fg(Color::Rgb(255, 150, 50)),
        ),
    ]);
    f.render_widget(Paragraph::new(ft).style(Style::new().fg(DIMTXT)), foot);

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
            Line::from(vec![k("Esc"), Span::raw("close panel / clear selection")]),
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

fn kv<'a>(k: &str, v: &str) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{k}: "), Style::new().fg(Color::Rgb(120, 190, 200))),
        Span::raw(v.to_string()),
    ])
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
        v.push(Line::from(vec![
            Span::styled("Temperature: ", Style::new().fg(Color::Rgb(120, 190, 200))),
            Span::styled(
                fmt_temp(t),
                Style::new().fg(tcolor(t)).add_modifier(Modifier::BOLD),
            ),
        ]));
    }
    if !p.metrics.is_empty() {
        v.push(Line::styled(
            p.metrics.join(" · "),
            Style::new().add_modifier(Modifier::BOLD),
        ));
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
        v.push(kv(k, val));
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
        let mut v = vec![
            kv("Slot", &s.label()),
            kv("Kind", s.kind().name()),
            kv("Soldered", if s.soldered { "yes" } else { "no" }),
        ];
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
                    v.push(kv(k, val));
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
