//! Terminal colour capability: detect whether 24-bit ("truecolor") SGR
//! sequences are understood, and if not, map every `Color::Rgb` in the
//! rendered buffer to the nearest xterm-256 palette entry.
//!
//! Why this matters: Terminal.app before macOS 26 does not understand
//! `ESC[38;2;R;G;Bm` / `ESC[48;2;R;G;Bm`. It reads every number as its own SGR
//! parameter, so a component such as 44/45/46/47 paints a blue/magenta/cyan/
//! grey background, 7 turns on reverse video, 8 hides text, and so on. Sending
//! 256-colour sequences (`38;5;N`) instead works everywhere.

use ratatui::buffer::Buffer;
use ratatui::style::Color;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Depth {
    TrueColor,
    Ansi256,
}

/// Parse a user override (`--color` / `INSIDER_COLOR`).
pub fn parse_depth(s: &str) -> Option<Depth> {
    match s.trim().to_ascii_lowercase().as_str() {
        "truecolor" | "24bit" | "24-bit" | "rgb" | "full" => Some(Depth::TrueColor),
        "256" | "ansi256" | "8bit" | "8-bit" => Some(Depth::Ansi256),
        _ => None,
    }
}

/// Work out what the terminal can show. Explicit signals win; when nothing
/// says "truecolor", fall back to 256 colours, which every modern terminal
/// renders correctly (a wrong guess the other way garbles the whole screen).
pub fn detect() -> Depth {
    let var = |k: &str| std::env::var(k).unwrap_or_default();

    if let Some(d) = parse_depth(&var("INSIDER_COLOR")) {
        return d;
    }

    let colorterm = var("COLORTERM").to_ascii_lowercase();
    if colorterm == "truecolor" || colorterm == "24bit" {
        return Depth::TrueColor;
    }
    let term = var("TERM");
    if term.ends_with("-direct") || term.contains("truecolor") {
        return Depth::TrueColor;
    }

    match var("TERM_PROGRAM").as_str() {
        // Terminal.app gained 24-bit colour in macOS 26 (Tahoe).
        "Apple_Terminal" => {
            return if macos_major().is_some_and(|v| v >= 26) {
                Depth::TrueColor
            } else {
                Depth::Ansi256
            }
        }
        "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Hyper" | "rio" | "Tabby"
        | "WarpTerminal" | "kitty" | "alacritty" | "zed" => return Depth::TrueColor,
        _ => {}
    }
    if term == "xterm-kitty" || term == "xterm-ghostty" || term == "alacritty" || term == "foot" {
        return Depth::TrueColor;
    }
    if !var("WT_SESSION").is_empty()
        || !var("KONSOLE_VERSION").is_empty()
        || !var("VTE_VERSION").is_empty()
    {
        return Depth::TrueColor;
    }
    // Windows 10+ console host and Windows Terminal both take 24-bit colour.
    if cfg!(windows) {
        return Depth::TrueColor;
    }
    Depth::Ansi256
}

#[cfg(target_os = "macos")]
fn macos_major() -> Option<u32> {
    let out = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(not(target_os = "macos"))]
fn macos_major() -> Option<u32> {
    None
}

/// Replace every RGB colour in the buffer with its nearest 256-palette index.
pub fn downsample(buf: &mut Buffer) {
    for cell in buf.content.iter_mut() {
        cell.fg = to_256(cell.fg);
        cell.bg = to_256(cell.bg);
    }
}

pub fn to_256(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Indexed(rgb_to_256(r, g, b)),
        c => c,
    }
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn cube_idx(v: u8) -> usize {
    // nearest of the six cube levels
    let mut best = 0;
    for (i, &l) in CUBE.iter().enumerate() {
        if (l as i32 - v as i32).abs() < (CUBE[best] as i32 - v as i32).abs() {
            best = i;
        }
    }
    best
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> i32 {
    // weighted Euclidean: closer to perceived difference than plain RGB
    let dr = a.0 as i32 - b.0 as i32;
    let dg = a.1 as i32 - b.1 as i32;
    let db = a.2 as i32 - b.2 as i32;
    2 * dr * dr + 4 * dg * dg + 3 * db * db
}

/// Nearest xterm-256 index (16..=255; the first 16 are user-themed, so avoid them).
pub fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let (ri, gi, bi) = (cube_idx(r), cube_idx(g), cube_idx(b));
    let cube = (CUBE[ri], CUBE[gi], CUBE[bi]);
    let cube_i = 16 + 36 * ri + 6 * gi + bi;

    // grey ramp 232..=255: 8, 18, ..., 238
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi_ = ((avg as i32 - 8 + 5) / 10).clamp(0, 23) as u8;
    let gv = 8 + 10 * gi_;
    let grey = (gv, gv, gv);

    if dist((r, g, b), grey) < dist((r, g, b), cube) {
        232 + gi_
    } else {
        cube_i as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_colours() {
        assert_eq!(rgb_to_256(0, 0, 0), 16);
        assert_eq!(rgb_to_256(255, 255, 255), 231);
        assert_eq!(rgb_to_256(255, 0, 0), 196);
        assert_eq!(rgb_to_256(128, 128, 128), 244);
        // near-black chip/plastic shades land on the grey ramp, not pure black
        assert!(rgb_to_256(18, 18, 21) >= 232);
    }

    #[test]
    fn override_parsing() {
        assert_eq!(parse_depth("256"), Some(Depth::Ansi256));
        assert_eq!(parse_depth("TrueColor"), Some(Depth::TrueColor));
        assert_eq!(parse_depth("nope"), None);
    }
}
