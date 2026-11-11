//! Just enough JSON to read `system_profiler -json` and PowerShell's
//! `ConvertTo-Json` output, without pulling in another crate.

use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(BTreeMap<String, Json>),
}

static NULL: Json = Json::Null;

impl Json {
    pub fn parse(s: &str) -> Option<Json> {
        let mut p = P {
            b: s.as_bytes(),
            i: 0,
        };
        p.ws();
        let v = p.value(0)?;
        p.ws();
        (p.i == p.b.len()).then_some(v)
    }

    /// Member of an object (Null if absent or not an object).
    pub fn get(&self, k: &str) -> &Json {
        match self {
            Json::Obj(m) => m.get(k).unwrap_or(&NULL),
            _ => &NULL,
        }
    }

    /// Array elements; a lone object counts as a one-element array, Null as
    /// none. (PowerShell unwraps single-element arrays.)
    pub fn items(&self) -> &[Json] {
        match self {
            Json::Arr(v) => v,
            Json::Null => &[],
            other => std::slice::from_ref(other),
        }
    }

    pub fn entries(&self) -> Vec<(&str, &Json)> {
        match self {
            Json::Obj(m) => m.iter().map(|(k, v)| (k.as_str(), v)).collect(),
            _ => Vec::new(),
        }
    }

    pub fn is_null(&self) -> bool {
        matches!(self, Json::Null)
    }

    /// String value; numbers and booleans are stringified. Empty strings -> None.
    pub fn str(&self) -> Option<String> {
        match self {
            Json::Str(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
            Json::Num(n) if n.fract() == 0.0 && n.abs() < 1e15 => Some(format!("{}", *n as i64)),
            Json::Num(n) => Some(n.to_string()),
            Json::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    /// Number value; numeric strings ("12", "0x1f") are accepted too.
    pub fn num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            Json::Str(s) => {
                let s = s.trim();
                if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
                    u64::from_str_radix(h, 16).ok().map(|v| v as f64)
                } else {
                    s.parse().ok()
                }
            }
            Json::Bool(b) => Some(*b as u8 as f64),
            _ => None,
        }
    }

    pub fn bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            Json::Num(n) => Some(*n != 0.0),
            Json::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            },
            _ => None,
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
        // PowerShell may emit a UTF-8 BOM
        if self.b[self.i..].starts_with(&[0xef, 0xbb, 0xbf]) {
            self.i += 3;
            self.ws();
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn value(&mut self, depth: usize) -> Option<Json> {
        if depth > 64 {
            return None;
        }
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => {
                self.i += 1;
                let mut m = BTreeMap::new();
                if self.eat(b'}').is_some() {
                    return Some(Json::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.eat(b':')?;
                    let v = self.value(depth + 1)?;
                    m.insert(k, v);
                    if self.eat(b',').is_some() {
                        continue;
                    }
                    self.eat(b'}')?;
                    return Some(Json::Obj(m));
                }
            }
            b'[' => {
                self.i += 1;
                let mut v = Vec::new();
                if self.eat(b']').is_some() {
                    return Some(Json::Arr(v));
                }
                loop {
                    v.push(self.value(depth + 1)?);
                    if self.eat(b',').is_some() {
                        continue;
                    }
                    self.eat(b']')?;
                    return Some(Json::Arr(v));
                }
            }
            b'"' => self.string().map(Json::Str),
            b't' => self.lit("true", Json::Bool(true)),
            b'f' => self.lit("false", Json::Bool(false)),
            b'n' => self.lit("null", Json::Null),
            _ => {
                let st = self.i;
                while self.i < self.b.len()
                    && matches!(
                        self.b[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                std::str::from_utf8(&self.b[st..self.i])
                    .ok()?
                    .parse()
                    .ok()
                    .map(Json::Num)
            }
        }
    }

    fn lit(&mut self, s: &str, v: Json) -> Option<Json> {
        self.b[self.i..].starts_with(s.as_bytes()).then(|| {
            self.i += s.len();
            v
        })
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = std::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return Some(String::from_utf8_lossy(&out).into_owned()),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let mut u = self.hex4()?;
                            if (0xd800..0xdc00).contains(&u) && self.b[self.i..].starts_with(b"\\u")
                            {
                                self.i += 2;
                                let lo = self.hex4()?;
                                u = 0x10000
                                    + ((u - 0xd800) << 10)
                                    + (lo.wrapping_sub(0xdc00) & 0x3ff);
                            }
                            char::from_u32(u).unwrap_or('\u{fffd}')
                        }
                        other => other as char,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                _ => out.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let j = Json::parse(
            "\u{feff}{\"a\": [1, 2.5, -3e2], \"b\": {\"c\": \"x\\u00e9\\\"\"}, \"d\": null, \"e\": true}",
        )
        .unwrap();
        assert_eq!(j.get("a").items().len(), 3);
        assert_eq!(j.get("a").items()[2].num(), Some(-300.0));
        assert_eq!(j.get("b").get("c").str().as_deref(), Some("xé\""));
        assert!(j.get("d").is_null());
        assert_eq!(j.get("e").bool(), Some(true));
        assert_eq!(j.get("b").items().len(), 1);
        assert_eq!(Json::Str("0x5ac".into()).num(), Some(1452.0));
        assert!(Json::parse("{\"a\":1,}").is_none());
    }
}
