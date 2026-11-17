//! Keyboard layouts: the legends printed on the drawn keyboard, and how to
//! tell which layout is active on each OS. Only the legend set matters here
//! (QWERTY vs Colemak vs AZERTY ...), not the national variant.

use crate::probe::{rd, run, sysroot};

pub struct Keymap {
    pub name: &'static str,
    /// Number row (13 keys), top row (13), home row (11), bottom row (10),
    /// space-separated, as they sit on an ANSI board.
    pub rows: [&'static str; 4],
}

const NUM: &str = "` 1 2 3 4 5 6 7 8 9 0 - =";

pub static ALL: &[Keymap] = &[
    Keymap {
        name: "QWERTY",
        rows: [
            NUM,
            "Q W E R T Y U I O P [ ] \\",
            "A S D F G H J K L ; '",
            "Z X C V B N M , . /",
        ],
    },
    Keymap {
        name: "Colemak",
        rows: [
            NUM,
            "Q W F P G J L U Y ; [ ] \\",
            "A R S T D H N E I O '",
            "Z X C V B K M , . /",
        ],
    },
    Keymap {
        name: "Colemak-DH",
        rows: [
            NUM,
            "Q W F P B J L U Y ; [ ] \\",
            "A R S T G M N E I O '",
            "Z X C D V K H , . /",
        ],
    },
    Keymap {
        name: "Dvorak",
        rows: [
            "` 1 2 3 4 5 6 7 8 9 0 [ ]",
            "' , . P Y F G C R L / = \\",
            "A O E U I D H T N S -",
            "; Q J K X B M W V Z",
        ],
    },
    Keymap {
        name: "Workman",
        rows: [
            NUM,
            "Q D R W B J F U P ; [ ] \\",
            "A S H T G Y N E O I '",
            "Z X M C V K L , . /",
        ],
    },
    Keymap {
        name: "AZERTY",
        rows: [
            "² & É \" ' ( - È _ Ç À ) =",
            "A Z E R T Y U I O P ^ $ *",
            "Q S D F G H J K L M Ù",
            "W X C V B N , ; : !",
        ],
    },
    Keymap {
        name: "QWERTZ",
        rows: [
            "^ 1 2 3 4 5 6 7 8 9 0 ß ´",
            "Q W E R T Z U I O P Ü + #",
            "A S D F G H J K L Ö Ä",
            "Y X C V B N M , . -",
        ],
    },
];

/// A layout by name; QWERTY for anything unknown.
pub fn by_name(name: &str) -> &'static Keymap {
    ALL.iter()
        .find(|k| k.name.eq_ignore_ascii_case(name))
        .unwrap_or(&ALL[0])
}

/// Which legend set a layout description means: a macOS input source
/// ("com.apple.keylayout.Colemak"), an XKB layout and variant ("us colemak_dh",
/// "de"), or a console keymap ("de-latin1").
pub fn classify(desc: &str) -> &'static str {
    let d = desc.to_lowercase();
    let words: Vec<&str> = d.split(|c: char| !c.is_ascii_alphanumeric()).collect();
    let has = |w: &[&str]| words.iter().any(|x| w.contains(x));
    if d.contains("colemak") {
        if d.contains("dh") {
            "Colemak-DH"
        } else {
            "Colemak"
        }
    } else if d.contains("workman") {
        "Workman"
    } else if d.contains("dvorak") {
        "Dvorak"
    } else if d.contains("canadian") {
        "QWERTY" // Canadian French is QWERTY
    } else if d.contains("swiss") || has(&["ch"]) {
        "QWERTZ" // Swiss French too
    } else if d.contains("french") || d.contains("belgian") || has(&["fr", "be"]) {
        "AZERTY"
    } else if [
        "german",
        "austrian",
        "czech",
        "hungarian",
        "slovak",
        "sloven",
        "croatian",
    ]
    .iter()
    .any(|w| d.contains(w))
        || has(&["de", "at", "cz", "hu", "sk", "si", "hr"])
    {
        "QWERTZ"
    } else {
        "QWERTY"
    }
}

/// Record the detected layout on the keyboard part ("Layout", read by the
/// drawing code; QWERTY when unknown) and what it was derived from.
pub fn set_layout(p: &mut crate::model::Part, source: Option<String>) {
    if let Some(src) = source {
        p.prop("Layout", classify(&src));
        p.prop("Input source", src.trim());
    }
}

/// macOS: the current keyboard input source. Under `sudo`, read the invoking
/// user's preferences, not root's.
pub fn macos() -> Option<String> {
    let prefs = match std::env::var("SUDO_USER") {
        Ok(u) if !u.is_empty() && u != "root" => {
            format!("/Users/{u}/Library/Preferences/com.apple.HIToolbox")
        }
        _ => "com.apple.HIToolbox".into(),
    };
    let id = run(
        "defaults",
        &["read", &prefs, "AppleCurrentKeyboardLayoutInputSourceID"],
    )
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
    id.or_else(|| {
        // older systems: the selected sources, as an old-style plist
        //     "KeyboardLayout Name" = Colemak;
        run("defaults", &["read", &prefs, "AppleSelectedInputSources"])?
            .lines()
            .find_map(|l| {
                let v = l.trim().strip_prefix("\"KeyboardLayout Name\" =")?;
                Some(
                    v.trim()
                        .trim_end_matches(';')
                        .trim()
                        .trim_matches('"')
                        .to_string(),
                )
            })
    })
}

/// Linux: the XKB layout from localectl, else /etc/default/keyboard, else
/// the console keymap.
pub fn linux() -> Option<String> {
    let first = |v: &str| {
        v.split(',')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"')
            .to_string()
    };
    if let Some(s) = run("localectl", &["status"]) {
        let field = |k: &str| s.lines().find_map(|l| l.trim().strip_prefix(k).map(&first));
        if let Some(layout) = field("X11 Layout:").filter(|l| !l.is_empty()) {
            return Some(format!(
                "{layout} {}",
                field("X11 Variant:").unwrap_or_default()
            ));
        }
    }
    let conf = |file: &str, key: &str| {
        rd(format!("{}{file}", sysroot()))?
            .lines()
            .find_map(|l| l.trim().strip_prefix(key).map(&first))
            .filter(|v| !v.is_empty())
    };
    if let Some(layout) = conf("/etc/default/keyboard", "XKBLAYOUT=") {
        let variant = conf("/etc/default/keyboard", "XKBVARIANT=").unwrap_or_default();
        return Some(format!("{layout} {variant}"));
    }
    conf("/etc/vconsole.conf", "KEYMAP=")
}

/// Windows: a keyboard layout ID ("00000407") from the first preloaded layout.
pub fn windows_klid(klid: &str) -> &'static str {
    let k = klid.trim().to_ascii_lowercase();
    match k.as_str() {
        "00010409" | "00030409" | "00040409" => "Dvorak",
        _ => match k.get(4..).unwrap_or("") {
            "040c" | "080c" | "0813" => "AZERTY",
            "0407" | "0807" | "0c07" | "100c" | "0405" | "040e" | "041b" | "0424" | "041a" => {
                "QWERTZ"
            }
            _ => "QWERTY",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sources() {
        assert_eq!(classify("com.apple.keylayout.Colemak"), "Colemak");
        assert_eq!(classify("com.apple.keylayout.ABC"), "QWERTY");
        assert_eq!(classify("com.apple.keylayout.Dvorak-QWERTYCMD"), "Dvorak");
        assert_eq!(classify("com.apple.keylayout.French-PC"), "AZERTY");
        assert_eq!(classify("com.apple.keylayout.Canadian-CSA"), "QWERTY");
        assert_eq!(classify("com.apple.keylayout.SwissFrench"), "QWERTZ");
        assert_eq!(classify("com.apple.keylayout.German"), "QWERTZ");
        assert_eq!(classify("us colemak_dh"), "Colemak-DH");
        assert_eq!(classify("us "), "QWERTY");
        assert_eq!(classify("de nodeadkeys"), "QWERTZ");
        assert_eq!(classify("fr "), "AZERTY");
        assert_eq!(classify("de-latin1"), "QWERTZ");
        assert_eq!(classify("gb "), "QWERTY");
    }

    #[test]
    fn klids() {
        assert_eq!(windows_klid("00000409"), "QWERTY");
        assert_eq!(windows_klid("00010409"), "Dvorak");
        assert_eq!(windows_klid("0000040C"), "AZERTY");
        assert_eq!(windows_klid("00000407"), "QWERTZ");
    }

    #[test]
    fn row_lengths() {
        for k in ALL {
            let n: Vec<usize> = k.rows.iter().map(|r| r.split(' ').count()).collect();
            assert_eq!(n, [13, 13, 11, 10], "{}", k.name);
        }
    }
}
