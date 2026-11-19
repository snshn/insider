//! Wi-Fi link details over nl80211 (generic netlink), the interface `iw`
//! uses. Replaces /proc/net/wireless: reading that file goes through the old
//! wireless extensions, which makes the kernel log "`insider` uses wireless
//! extensions which will stop working for Wi-Fi 7 hardware; use nl80211".
//!
//! Linux only, no root needed, no dependencies: a raw AF_NETLINK socket and
//! a hand-rolled message codec. Two requests per interface:
//!   NL80211_CMD_GET_INTERFACE  → SSID, frequency
//!   NL80211_CMD_GET_STATION    → signal, TX/RX bitrate (dump; the AP when
//!                                connected as a client)

use std::ffi::{c_int, c_long, c_void};

const AF_NETLINK: c_int = 16;
const SOCK_RAW: c_int = 3;
const SOCK_CLOEXEC: c_int = 0o2_000_000;
const NETLINK_GENERIC: c_int = 16;
const SOL_SOCKET: c_int = 1;
const SO_RCVTIMEO: c_int = 20;

const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_ACK: u16 = 0x4;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;

const GENL_ID_CTRL: u16 = 0x10;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;

const NL80211_CMD_GET_INTERFACE: u8 = 5;
const NL80211_CMD_GET_STATION: u8 = 17;
const NL80211_ATTR_IFINDEX: u16 = 3;
const NL80211_ATTR_STA_INFO: u16 = 21;
const NL80211_ATTR_WIPHY_FREQ: u16 = 38;
const NL80211_ATTR_SSID: u16 = 52;
const NL80211_STA_INFO_SIGNAL: u16 = 7;
const NL80211_STA_INFO_TX_BITRATE: u16 = 8;
const NL80211_STA_INFO_RX_BITRATE: u16 = 14;
const NL80211_RATE_INFO_BITRATE: u16 = 1; // u16, 100 kbit/s
const NL80211_RATE_INFO_BITRATE32: u16 = 5; // u32, 100 kbit/s

extern "C" {
    fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int;
    fn send(fd: c_int, buf: *const c_void, len: usize, flags: c_int) -> isize;
    fn recv(fd: c_int, buf: *mut c_void, len: usize, flags: c_int) -> isize;
    fn close(fd: c_int) -> c_int;
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Link {
    pub ssid: Option<String>,
    pub freq_mhz: Option<u32>,
    pub signal_dbm: Option<i32>,
    pub tx_mbps: Option<f64>,
    pub rx_mbps: Option<f64>,
}

/// An open generic-netlink socket with the nl80211 family resolved.
pub struct Nl80211 {
    fd: c_int,
    family: u16,
    seq: u32,
}

impl Drop for Nl80211 {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
    }
}

/// Append a netlink attribute (4-byte aligned).
fn put_attr(buf: &mut Vec<u8>, ty: u16, data: &[u8]) {
    let len = 4 + data.len();
    buf.extend_from_slice(&(len as u16).to_ne_bytes());
    buf.extend_from_slice(&ty.to_ne_bytes());
    buf.extend_from_slice(data);
    buf.resize((buf.len() + 3) & !3, 0);
}

/// Iterate (type, payload) over a run of attributes.
fn attrs(mut b: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        if b.len() < 4 {
            return None;
        }
        let len = u16::from_ne_bytes([b[0], b[1]]) as usize;
        let ty = u16::from_ne_bytes([b[2], b[3]]) & 0x3fff; // strip NESTED/BYTEORDER flags
        if len < 4 || len > b.len() {
            return None;
        }
        let payload = &b[4..len];
        b = &b[((len + 3) & !3).min(b.len())..];
        Some((ty, payload))
    })
}

fn u16_of(p: &[u8]) -> Option<u16> {
    Some(u16::from_ne_bytes(p.get(..2)?.try_into().ok()?))
}

fn u32_of(p: &[u8]) -> Option<u32> {
    Some(u32::from_ne_bytes(p.get(..4)?.try_into().ok()?))
}

/// A rate-info nest → Mbit/s.
fn bitrate(p: &[u8]) -> Option<f64> {
    let mut r16 = None;
    for (t, v) in attrs(p) {
        match t {
            NL80211_RATE_INFO_BITRATE32 => return u32_of(v).map(|x| x as f64 / 10.0),
            NL80211_RATE_INFO_BITRATE => r16 = u16_of(v).map(|x| x as f64 / 10.0),
            _ => {}
        }
    }
    r16
}

/// Fill `link` from one nl80211 reply's attributes. The reply's command is
/// not the request's (GET_INTERFACE is answered with NEW_INTERFACE,
/// GET_STATION with NEW_STATION), so go by attribute type alone.
fn parse_reply(a: &[u8], link: &mut Link) {
    for (t, v) in attrs(a) {
        match t {
            NL80211_ATTR_SSID => link.ssid = Some(String::from_utf8_lossy(v).into_owned()),
            NL80211_ATTR_WIPHY_FREQ => link.freq_mhz = u32_of(v),
            NL80211_ATTR_STA_INFO if link.signal_dbm.is_none() => {
                for (st, sv) in attrs(v) {
                    match st {
                        NL80211_STA_INFO_SIGNAL => {
                            link.signal_dbm = sv.first().map(|&b| b as i8 as i32)
                        }
                        NL80211_STA_INFO_TX_BITRATE => link.tx_mbps = bitrate(sv),
                        NL80211_STA_INFO_RX_BITRATE => link.rx_mbps = bitrate(sv),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Nl80211 {
    /// None if there is no nl80211 (no cfg80211 driver loaded, containers).
    pub fn open() -> Option<Nl80211> {
        let fd = unsafe { socket(AF_NETLINK, SOCK_RAW | SOCK_CLOEXEC, NETLINK_GENERIC) };
        if fd < 0 {
            return None;
        }
        // never block the prober for long
        let tv: [c_long; 2] = [1, 0];
        unsafe {
            setsockopt(
                fd,
                SOL_SOCKET,
                SO_RCVTIMEO,
                tv.as_ptr().cast(),
                std::mem::size_of_val(&tv) as u32,
            );
        }
        let mut s = Nl80211 {
            fd,
            family: 0,
            seq: 0,
        };
        let mut name = b"nl80211\0".to_vec();
        name.resize(8, 0);
        let mut payload = Vec::new();
        put_attr(&mut payload, CTRL_ATTR_FAMILY_NAME, &name);
        let mut family = None;
        s.request(GENL_ID_CTRL, CTRL_CMD_GETFAMILY, 0, &payload, |_, a| {
            for (t, v) in attrs(a) {
                if t == CTRL_ATTR_FAMILY_ID {
                    family = u16_of(v);
                }
            }
        })?;
        s.family = family?;
        Some(s)
    }

    /// Current link of a Wi-Fi interface (by ifindex). Fields the driver
    /// doesn't report, or that don't apply (not connected), stay None.
    pub fn link(&mut self, ifindex: u32) -> Link {
        let mut payload = Vec::new();
        put_attr(&mut payload, NL80211_ATTR_IFINDEX, &ifindex.to_ne_bytes());
        let mut link = Link::default();
        let fam = self.family;
        let _ = self.request(fam, NL80211_CMD_GET_INTERFACE, 0, &payload, |_, a| {
            parse_reply(a, &mut link)
        });
        let _ = self.request(
            fam,
            NL80211_CMD_GET_STATION,
            NLM_F_DUMP,
            &payload,
            |_, a| parse_reply(a, &mut link),
        );
        link
    }

    /// Send one generic-netlink request and feed every reply's (cmd, attrs)
    /// to `f` until the kernel acks, finishes the dump, or errors.
    fn request(
        &mut self,
        family: u16,
        cmd: u8,
        flags: u16,
        payload: &[u8],
        mut f: impl FnMut(u8, &[u8]),
    ) -> Option<()> {
        self.seq = self.seq.wrapping_add(1);
        let len = 16 + 4 + payload.len();
        let mut msg = Vec::with_capacity(len);
        msg.extend_from_slice(&(len as u32).to_ne_bytes());
        msg.extend_from_slice(&family.to_ne_bytes());
        msg.extend_from_slice(&(NLM_F_REQUEST | NLM_F_ACK | flags).to_ne_bytes());
        msg.extend_from_slice(&self.seq.to_ne_bytes());
        msg.extend_from_slice(&0u32.to_ne_bytes()); // port id: let the kernel assign
        msg.extend_from_slice(&[cmd, 1, 0, 0]); // genlmsghdr: cmd, version, reserved
        msg.extend_from_slice(payload);
        if unsafe { send(self.fd, msg.as_ptr().cast(), msg.len(), 0) } < 0 {
            return None;
        }
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            let n = unsafe { recv(self.fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n <= 0 {
                return None; // timeout or error
            }
            match walk(&buf[..n as usize], self.seq, &mut f) {
                Step::More => continue,
                Step::Done => return Some(()),
                Step::Failed => return None,
            }
        }
    }
}

enum Step {
    More,
    Done,
    Failed,
}

/// Go through one datagram of netlink messages, handing each reply's
/// (genl cmd, attributes) to `f`. Done at NLMSG_DONE (end of a dump) or a
/// zero NLMSG_ERROR (the ack of a plain request).
fn walk(mut b: &[u8], seq: u32, f: &mut impl FnMut(u8, &[u8])) -> Step {
    while b.len() >= 16 {
        let mlen = u32::from_ne_bytes([b[0], b[1], b[2], b[3]]) as usize;
        let mtype = u16::from_ne_bytes([b[4], b[5]]);
        let mseq = u32::from_ne_bytes([b[8], b[9], b[10], b[11]]);
        if mlen < 16 || mlen > b.len() {
            return Step::Failed;
        }
        let body = &b[16..mlen];
        b = &b[((mlen + 3) & !3).min(b.len())..];
        if mseq != seq {
            continue; // stale reply to an earlier, timed-out request
        }
        match mtype {
            NLMSG_DONE => return Step::Done,
            NLMSG_ERROR => {
                let code = body
                    .get(..4)
                    .map(|c| i32::from_ne_bytes([c[0], c[1], c[2], c[3]]));
                return if code == Some(0) {
                    Step::Done
                } else {
                    Step::Failed
                };
            }
            _ if body.len() >= 4 => f(body[0], &body[4..]),
            _ => {}
        }
    }
    Step::More
}

/// Channel number for a frequency (2.4, 5 and 6 GHz bands).
pub fn channel(freq: u32) -> Option<u32> {
    match freq {
        2484 => Some(14),
        2412..=2472 => Some((freq - 2407) / 5),
        5955..=7115 => Some((freq - 5950) / 5),
        5160..=5885 => Some((freq - 5000) / 5),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels() {
        assert_eq!(channel(2412), Some(1));
        assert_eq!(channel(2484), Some(14));
        assert_eq!(channel(5180), Some(36));
        assert_eq!(channel(5955), Some(1));
        assert_eq!(channel(6115), Some(33));
    }

    /// A whole datagram as the kernel sends it for a station dump: the reply
    /// is NEW_STATION (19), not the GET_STATION (17) that was asked for.
    #[test]
    fn station_dump_datagram() {
        let mut sta = Vec::new();
        put_attr(&mut sta, NL80211_STA_INFO_SIGNAL, &[(-61i8) as u8]);
        let mut attrs_ = Vec::new();
        put_attr(&mut attrs_, NL80211_ATTR_STA_INFO, &sta);
        let msg = |ty: u16, seq: u32, body: &[u8]| {
            let mut m = Vec::new();
            m.extend_from_slice(&((16 + body.len()) as u32).to_ne_bytes());
            m.extend_from_slice(&ty.to_ne_bytes());
            m.extend_from_slice(&0x2u16.to_ne_bytes()); // NLM_F_MULTI
            m.extend_from_slice(&seq.to_ne_bytes());
            m.extend_from_slice(&0u32.to_ne_bytes());
            m.extend_from_slice(body);
            m
        };
        let mut genl = vec![19u8, 1, 0, 0];
        genl.extend_from_slice(&attrs_);
        let mut dgram = msg(0x1c, 7, &genl);
        dgram.extend(msg(NLMSG_DONE, 7, &0i32.to_ne_bytes()));
        let mut l = Link::default();
        let step = walk(&dgram, 7, &mut |_, a| parse_reply(a, &mut l));
        assert!(matches!(step, Step::Done));
        assert_eq!(l.signal_dbm, Some(-61));
        // a reply to another request is ignored
        let mut l2 = Link::default();
        assert!(matches!(
            walk(&dgram, 8, &mut |_, a| parse_reply(a, &mut l2)),
            Step::More
        ));
        assert_eq!(l2.signal_dbm, None);
    }

    #[test]
    fn parses_station_and_interface_replies() {
        let mut rate = Vec::new();
        put_attr(
            &mut rate,
            NL80211_RATE_INFO_BITRATE32,
            &8667u32.to_ne_bytes(),
        );
        let mut sta = Vec::new();
        put_attr(&mut sta, NL80211_STA_INFO_SIGNAL, &[(-54i8) as u8]);
        put_attr(&mut sta, NL80211_STA_INFO_TX_BITRATE, &rate);
        let mut station = Vec::new();
        put_attr(&mut station, NL80211_ATTR_IFINDEX, &3u32.to_ne_bytes());
        put_attr(&mut station, NL80211_ATTR_STA_INFO, &sta);

        let mut iface = Vec::new();
        put_attr(&mut iface, NL80211_ATTR_SSID, b"home");
        put_attr(&mut iface, NL80211_ATTR_WIPHY_FREQ, &5180u32.to_ne_bytes());

        let mut l = Link::default();
        parse_reply(&iface, &mut l);
        parse_reply(&station, &mut l);
        assert_eq!(
            l,
            Link {
                ssid: Some("home".into()),
                freq_mhz: Some(5180),
                signal_dbm: Some(-54),
                tx_mbps: Some(866.7),
                rx_mbps: None,
            }
        );
    }
}
