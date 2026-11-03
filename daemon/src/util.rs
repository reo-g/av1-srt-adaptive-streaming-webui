//! Small helpers: JST time strings, rounding, the developer log ring and the local address.

use std::collections::VecDeque;
use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Developer log line: to stderr (the journal) and into an in-memory ring that is written
/// next to the children's logs when a session ends. Never shown in the UI.
macro_rules! dlog {
    ($($t:tt)*) => { $crate::util::dlog(format!($($t)*)) };
}

const LOG_LINES: usize = 2000;
static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

pub fn dlog(msg: String) {
    let line = format!("{} {msg}", iso_jst(now_ms()));
    eprintln!("{line}");
    let mut ring = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() >= LOG_LINES {
        ring.pop_front();
    }
    ring.push_back(line);
}

/// The developer log ring as text (oldest first).
pub fn log_dump() -> String {
    let ring = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let mut s = String::new();
    for l in ring.iter() {
        s.push_str(l);
        s.push('\n');
    }
    s
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Wall-clock milliseconds of a monotonic instant. The offset is taken now, so a clock step
/// (NTP at boot) does not shift times recorded before it.
pub fn wall_ms(at: Instant) -> i64 {
    let now = Instant::now();
    if at <= now {
        now_ms() - (now - at).as_millis() as i64
    } else {
        now_ms() + (at - now).as_millis() as i64
    }
}

/// ISO 8601 in JST without milliseconds, e.g. 2026-11-04T10:42:18+09:00.
pub fn iso_jst(ms: i64) -> String {
    let secs = ms.div_euclid(1000) + 9 * 3600;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let sod = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+09:00", sod / 3600, sod % 3600 / 60, sod % 60)
}

pub fn iso_at(at: Instant) -> String {
    iso_jst(wall_ms(at))
}

/// JST date and time for directory names, e.g. 20261104-104218.
pub fn stamp() -> String {
    let s = iso_jst(now_ms());
    format!("{}{}{}-{}{}{}", &s[0..4], &s[5..7], &s[8..10], &s[11..13], &s[14..16], &s[17..19])
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

pub fn round(v: f64, digits: i32) -> f64 {
    let p = 10f64.powi(digits);
    (v * p).round() / p
}

/// One decimal, rounded down (a margin that just fell below 3 s reads 2.9, not 3.0).
pub fn floor1(v: f64) -> f64 {
    ((v * 10.0) + 1e-9).floor() / 10.0
}

/// This device's IPv4 address on the LAN, read at run time (the addresses come from DHCP).
/// The route toward `toward` (the peer, if known) picks the interface; no packet is sent.
pub fn local_ipv4(toward: &str) -> String {
    let target = toward
        .parse::<Ipv4Addr>()
        .ok()
        .filter(|a| !a.is_unspecified() && !a.is_loopback())
        .unwrap_or(Ipv4Addr::new(192, 0, 2, 1));
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
        if sock.connect((target, 9)).is_ok() {
            if let Ok(addr) = sock.local_addr() {
                if !addr.ip().is_unspecified() && !addr.ip().is_loopback() {
                    return addr.ip().to_string();
                }
            }
        }
    }
    first_interface_ipv4().unwrap_or_else(|| "0.0.0.0".into())
}

fn first_interface_ipv4() -> Option<String> {
    let mut out = None;
    // SAFETY: getifaddrs/freeifaddrs on a list we own; every pointer is checked before use.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) != 0 {
            return None;
        }
        let mut p = list;
        while !p.is_null() {
            let ifa = &*p;
            let up = ifa.ifa_flags & libc::IFF_UP as libc::c_uint != 0;
            let lo = ifa.ifa_flags & libc::IFF_LOOPBACK as libc::c_uint != 0;
            if up && !lo && !ifa.ifa_addr.is_null() && i32::from((*ifa.ifa_addr).sa_family) == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                out = Some(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)).to_string());
                break;
            }
            p = ifa.ifa_next;
        }
        libc::freeifaddrs(list);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jst_strings() {
        assert_eq!(iso_jst(0), "1970-01-01T09:00:00+09:00");
        assert_eq!(iso_jst(1_793_756_538_000), "2026-11-04T10:42:18+09:00");
        assert_eq!(iso_jst(1_793_756_538_999), "2026-11-04T10:42:18+09:00");
        // leap day and a year boundary in JST
        assert_eq!(iso_jst(1_709_164_800_000), "2024-02-29T09:00:00+09:00");
        assert_eq!(iso_jst(1_735_657_200_000), "2025-01-01T00:00:00+09:00");
    }

    #[test]
    fn rounding() {
        assert_eq!(floor1(2.99), 2.9);
        assert_eq!(floor1(3.0), 3.0);
        assert_eq!(floor1(0.3), 0.3);
        assert_eq!(floor1(6.43), 6.4);
        assert_eq!(round(3.6992, 2), 3.7);
        assert_eq!(round(2.9411, 1), 2.9);
    }

    #[test]
    fn stamp_shape() {
        let s = stamp();
        assert_eq!(s.len(), 15);
        assert_eq!(&s[8..9], "-");
    }
}
