//! `<state_dir>/link_filter.json`: what the radio line (the WRAN port) may carry, kept up to date
//! for a root-owned helper on the device (not part of this repo) that watches the file and
//! reloads the port's packet filter from it. The daemon never needs root for this and never
//! touches the filter itself.
//!
//! ```json
//! {"version": 1, "role": "tx", "peer": "192.168.0.210", "video_port": 60021, "test_port": 60022,
//!  "hosts": ["192.168.0.65", "192.168.0.56"]}
//! ```
//! - `peer`: tx the transport settings' peer host (an IPv4 literal, else looked up once with the
//!   system resolver; until that answers, or when it fails, the previous file's peer stays, and a
//!   failed lookup is tried again at an update a minute or more later, e.g. a resolver that was
//!   not ready at boot); rx `rx.peer_host` from device.toml (a name looked up the same way),
//!   else the last transmitter that connected (after a restart the previous file's, until one
//!   connects), else null.
//! - `video_port`: tx the peer port of the transport settings, rx `rx.listen_port`.
//! - `test_port`: `network_test.port` (the line-speed test).
//! - `hosts`: the addresses of 途中の無線機 (the applied list, else device.toml's).
//!
//! Written at start and whenever the content changes (after PUT /settings/network, PUT
//! /settings/transport, a new transmitter on rx, a lookup that answered): to
//! `link_filter.json.tmp` in the same directory, mode 0644, fsync, then renamed over the file, so
//! a reader never sees half a file (a watcher should react to the rename).

use crate::config::Role;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions, Permissions};
use std::future::Future;
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const FILE_NAME: &str = "link_filter.json";
/// The file's own format version (`version`).
pub const FORMAT: u32 = 1;
/// A lookup that has not answered by then counts as failed.
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(10);
/// A failed lookup is tried again at the first update this long after it (not at every update:
/// rx updates at every change of the link).
const RETRY_AFTER: Duration = Duration::from_secs(60);

/// The file's content (fields in this order).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterFile {
    pub version: u32,
    pub role: Role,
    pub peer: Option<String>,
    pub video_port: u16,
    pub test_port: u16,
    pub hosts: Vec<String>,
}

/// What the file is made of, as the daemon knows it now (App::filter_inputs).
#[derive(Debug, Clone, PartialEq)]
pub struct Inputs {
    pub role: Role,
    /// tx: the transport settings' peer host; rx: the configured or the last connected
    /// transmitter (None: not known). An address or a host name.
    pub peer: Option<String>,
    pub video_port: u16,
    pub test_port: u16,
    pub hosts: Vec<String>,
}

pub type Lookup = Pin<Box<dyn Future<Output = Option<Ipv4Addr>> + Send>>;
/// Looks a host name up (the system resolver; a fake in the tests).
pub type Resolver = fn(String) -> Lookup;

/// The first IPv4 address the system resolver gives for `host` (getaddrinfo, off the runtime thread).
pub fn system_resolver(host: String) -> Lookup {
    Box::pin(async move {
        let found = tokio::time::timeout(LOOKUP_TIMEOUT, tokio::net::lookup_host((host.as_str(), 0))).await.ok()?.ok()?;
        found
            .filter_map(|a| match a.ip() {
                IpAddr::V4(v4) => Some(v4),
                IpAddr::V6(v6) => v6.to_ipv4_mapped(),
            })
            .next()
    })
}

/// An address as the peer's (`::ffff:a.b.c.d` from a dual-stack socket is the IPv4 one).
fn ipv4_of(s: &str) -> Option<Ipv4Addr> {
    s.strip_prefix("::ffff:").unwrap_or(s).parse().ok()
}

#[derive(Default)]
struct St {
    inputs: Option<Inputs>,
    /// Host names looked up: their address (kept), or None and when the lookup failed (tried
    /// again after RETRY_AFTER).
    names: HashMap<String, (Option<Ipv4Addr>, Instant)>,
    pending: HashSet<String>,
    /// The peer in the file: the one on disk at start (written for the same role), then the last written.
    peer: Option<String>,
    /// What this daemon last wrote (None: nothing yet, so the first update always writes).
    written: Option<FilterFile>,
}

/// The file's writer (in App). Without a path (tests without a state_dir) nothing is written.
pub struct LinkFilter {
    path: Option<PathBuf>,
    resolver: Resolver,
    retry_after: Duration,
    st: Mutex<St>,
}

impl LinkFilter {
    /// Reads the previous file (its peer, an IPv4 address, stays until a new one is known).
    pub fn new(path: Option<PathBuf>, role: Role, resolver: Resolver) -> LinkFilter {
        let peer = path
            .as_deref()
            .and_then(read)
            .filter(|f| f.role == role)
            .and_then(|f| f.peer)
            .filter(|p| p.parse::<Ipv4Addr>().is_ok());
        LinkFilter { path, resolver, retry_after: RETRY_AFTER, st: Mutex::new(St { peer, ..St::default() }) }
    }

    fn st(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The peer address the file has now (None: none yet).
    pub fn peer(&self) -> Option<String> {
        self.st().peer.clone()
    }

    /// New inputs: the file is written if its content changes (always the first time). A peer
    /// given by a host name is looked up in the background, once; the file is written again
    /// when the answer comes. Call from the runtime thread (the lookup is spawned on it).
    pub fn update(self: &Arc<Self>, inputs: Inputs) {
        if self.path.is_none() {
            return;
        }
        let mut st = self.st();
        if let Some(name) = inputs.peer.as_deref().filter(|p| ipv4_of(p).is_none()) {
            let due = match st.names.get(name) {
                None => true,
                Some((Some(_), _)) => false,
                Some((None, failed_at)) => failed_at.elapsed() >= self.retry_after,
            };
            if due && !st.pending.contains(name) {
                self.lookup(&mut st, name.to_string());
            }
        }
        st.inputs = Some(inputs);
        self.write(&mut st);
    }

    fn lookup(self: &Arc<Self>, st: &mut St, name: String) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            dlog!("link filter: cannot look up {name} here; keeping the previous peer");
            st.names.insert(name, (None, Instant::now()));
            return;
        };
        dlog!("link filter: looking up the peer {name}");
        st.pending.insert(name.clone());
        let lookup = (self.resolver)(name.clone());
        let me = self.clone();
        rt.spawn(async move {
            let found = lookup.await;
            let mut st = me.st();
            st.pending.remove(&name);
            match found {
                Some(a) => dlog!("link filter: the peer {name} is {a}"),
                None => {
                    let prev = st.peer.clone().unwrap_or_else(|| "none".into());
                    dlog!("link filter: cannot resolve the peer {name} to an IPv4 address; keeping the previous peer ({prev})");
                }
            }
            st.names.insert(name, (found, Instant::now()));
            me.write(&mut st);
        });
    }

    fn content(st: &St) -> Option<FilterFile> {
        let i = st.inputs.as_ref()?;
        let peer = match i.peer.as_deref() {
            Some(p) => match (ipv4_of(p), st.names.get(p)) {
                (Some(a), _) => Some(a.to_string()),
                (None, Some((Some(a), _))) => Some(a.to_string()),
                // not looked up yet, or it failed
                (None, _) => st.peer.clone(),
            },
            // rx before a transmitter connected: the one of the previous file (from before a restart)
            None => st.peer.clone(),
        };
        let mut hosts = Vec::new();
        for h in &i.hosts {
            match h.parse::<Ipv4Addr>() {
                Ok(a) => hosts.push(a.to_string()),
                Err(_) => dlog!("link filter: {h:?} is not an IPv4 address; left out"),
            }
        }
        Some(FilterFile { version: FORMAT, role: i.role, peer, video_port: i.video_port, test_port: i.test_port, hosts })
    }

    fn write(&self, st: &mut St) {
        let (Some(path), Some(file)) = (self.path.as_deref(), Self::content(st)) else { return };
        if st.written.as_ref() == Some(&file) {
            return;
        }
        match write_atomic(path, &file) {
            Ok(()) => {
                dlog!(
                    "link filter: wrote {} ({:?}, peer {}, video port {}, test port {}, hosts [{}])",
                    path.display(),
                    file.role,
                    file.peer.as_deref().unwrap_or("none"),
                    file.video_port,
                    file.test_port,
                    file.hosts.join(", ")
                );
                st.peer = file.peer.clone();
                st.written = Some(file);
            }
            // tried again at the next change
            Err(e) => dlog!("link filter: cannot write {}: {e}", path.display()),
        }
    }
}

pub fn read(path: &Path) -> Option<FilterFile> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// tmp file in the same directory (0644 whatever the umask) + fsync + rename + fsync of the directory.
pub fn write_atomic(path: &Path, f: &FilterFile) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(f).map_err(io::Error::other)?;
    text.push('\n');
    let tmp = path.with_extension("json.tmp");
    {
        let mut out = OpenOptions::new().write(true).create(true).truncate(true).mode(0o644).open(&tmp)?;
        out.set_permissions(Permissions::from_mode(0o644))?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ctrld-filter-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn tx(peer: &str, hosts: &[&str]) -> Inputs {
        Inputs {
            role: Role::Tx,
            peer: Some(peer.into()),
            video_port: 60021,
            test_port: 60022,
            hosts: hosts.iter().map(|h| h.to_string()).collect(),
        }
    }

    static LOOKUPS: AtomicUsize = AtomicUsize::new(0);

    /// rx.example answers 198.51.100.7 after 50 ms, everything else fails after 50 ms.
    fn fake(host: String) -> Lookup {
        LOOKUPS.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            (host == "rx.example").then(|| Ipv4Addr::new(198, 51, 100, 7))
        })
    }

    fn raw(path: &Path) -> serde_json::Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn the_file_for_tx() {
        let d = dir("tx");
        let path = d.join(FILE_NAME);
        let f = Arc::new(LinkFilter::new(Some(path.clone()), Role::Tx, fake));
        assert_eq!(f.peer(), None);
        f.update(tx("192.168.0.210", &["192.168.0.65", "192.168.0.56"]));
        // the fields in the documented order, one object
        let text = fs::read_to_string(&path).unwrap();
        let keys: Vec<usize> =
            ["\"version\"", "\"role\"", "\"peer\"", "\"video_port\"", "\"test_port\"", "\"hosts\""].iter().map(|k| text.find(k).unwrap()).collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]), "{text}");
        assert_eq!(
            raw(&path),
            serde_json::json!({ "version": 1, "role": "tx", "peer": "192.168.0.210", "video_port": 60021, "test_port": 60022,
                                "hosts": ["192.168.0.65", "192.168.0.56"] })
        );
        assert_eq!(f.peer().as_deref(), Some("192.168.0.210"));
        let meta = fs::metadata(&path).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o644);
        assert!(!path.with_extension("json.tmp").exists());

        // the same content: not written again (the helper is not woken up for nothing)
        let ino = meta.ino();
        f.update(tx("192.168.0.210", &["192.168.0.65", "192.168.0.56"]));
        assert_eq!(fs::metadata(&path).unwrap().ino(), ino);
        // a change: replaced by a rename (a reader holding the old file still reads it whole)
        let held = fs::read_to_string(&path).unwrap();
        let old = File::open(&path).unwrap();
        f.update(Inputs { video_port: 60030, ..tx("192.168.0.211", &[]) });
        assert_ne!(fs::metadata(&path).unwrap().ino(), ino);
        assert_eq!(io::read_to_string(old).unwrap(), held);
        let v = raw(&path);
        assert_eq!((v["peer"].as_str(), v["video_port"].as_u64(), v["hosts"].clone()), (Some("192.168.0.211"), Some(60030), serde_json::json!([])));
        // a leftover tmp file (a crash between the write and the rename) is taken over
        fs::write(path.with_extension("json.tmp"), "garbage").unwrap();
        f.update(tx("192.168.0.212", &["10.0.0.1", "radio.local"]));
        let v = raw(&path);
        assert_eq!((v["peer"].as_str(), v["hosts"].clone()), (Some("192.168.0.212"), serde_json::json!(["10.0.0.1"])), "names are left out");
        assert!(!path.with_extension("json.tmp").exists());
        fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn written_whatever_the_umask() {
        let d = dir("umask");
        let path = d.join(FILE_NAME);
        // SAFETY: umask only changes this process's file creation mask (restored below).
        let old = unsafe { libc::umask(0o077) };
        let r = write_atomic(&path, &FilterFile { version: 1, role: Role::Rx, peer: None, video_port: 1, test_port: 2, hosts: vec![] });
        unsafe { libc::umask(old) };
        r.unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o644);
        fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn the_file_for_rx() {
        let d = dir("rx");
        let path = d.join(FILE_NAME);
        let rx = |peer: Option<&str>| Inputs { role: Role::Rx, peer: peer.map(String::from), video_port: 60021, test_port: 60022, hosts: vec!["192.168.0.56".into()] };
        // a fresh device, no transmitter yet: null
        let f = Arc::new(LinkFilter::new(Some(path.clone()), Role::Rx, fake));
        f.update(rx(None));
        assert_eq!(raw(&path)["peer"], serde_json::Value::Null);
        assert_eq!(raw(&path)["role"], "rx");
        // one connects (a dual-stack socket's form too)
        f.update(rx(Some("::ffff:192.168.0.220")));
        assert_eq!(raw(&path)["peer"], "192.168.0.220");
        // after a restart: the last one stays until another connects
        let f = Arc::new(LinkFilter::new(Some(path.clone()), Role::Rx, fake));
        assert_eq!(f.peer().as_deref(), Some("192.168.0.220"));
        f.update(rx(None));
        assert_eq!(raw(&path)["peer"], "192.168.0.220");
        f.update(rx(Some("192.168.0.221")));
        assert_eq!(raw(&path)["peer"], "192.168.0.221");
        // a file written for the other role says nothing about this one's peer
        let f = Arc::new(LinkFilter::new(Some(path.clone()), Role::Tx, fake));
        assert_eq!(f.peer(), None);
        // without a path nothing is written
        let none = Arc::new(LinkFilter::new(None, Role::Rx, fake));
        none.update(rx(Some("192.168.0.9")));
        assert_eq!(raw(&path)["peer"], "192.168.0.221");
        fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn a_peer_host_name_is_looked_up_once() {
        let d = dir("names");
        let path = d.join(FILE_NAME);
        write_atomic(&path, &FilterFile { version: 1, role: Role::Tx, peer: Some("192.168.0.210".into()), video_port: 60021, test_port: 60022, hosts: vec![] }).unwrap();
        let f = Arc::new(LinkFilter::new(Some(path.clone()), Role::Tx, fake));
        let before = LOOKUPS.load(Ordering::SeqCst);
        // until the answer: the previous file's peer
        f.update(tx("rx.example", &["192.168.0.65"]));
        f.update(tx("rx.example", &["192.168.0.65", "192.168.0.56"]));
        assert_eq!(raw(&path)["peer"], "192.168.0.210");
        assert_eq!(raw(&path)["hosts"], serde_json::json!(["192.168.0.65", "192.168.0.56"]), "the rest is up to date");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(raw(&path)["peer"], "198.51.100.7", "written again when it answered");
        f.update(tx("rx.example", &[]));
        assert_eq!(raw(&path)["peer"], "198.51.100.7");
        // a name that does not resolve: the previous peer stays
        f.update(tx("nowhere.example", &[]));
        tokio::time::sleep(Duration::from_millis(200)).await;
        f.update(tx("nowhere.example", &["10.0.0.1"]));
        assert_eq!((raw(&path)["peer"].as_str(), raw(&path)["hosts"].clone()), (Some("198.51.100.7"), serde_json::json!(["10.0.0.1"])));
        assert_eq!(LOOKUPS.load(Ordering::SeqCst) - before, 2, "each name once");
        // back to an address: no lookup
        f.update(tx("192.168.0.210", &[]));
        assert_eq!(raw(&path)["peer"], "192.168.0.210");
        assert_eq!(LOOKUPS.load(Ordering::SeqCst) - before, 2);
        fs::remove_dir_all(&d).unwrap();
    }

    static BOOT: AtomicUsize = AtomicUsize::new(0);

    /// boot.example: the first lookup fails (a resolver not ready yet at boot), later ones answer.
    fn fake_boot(host: String) -> Lookup {
        let n = BOOT.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { (host == "boot.example" && n > 0).then(|| Ipv4Addr::new(198, 51, 100, 8)) })
    }

    #[tokio::test]
    async fn a_failed_lookup_is_tried_again_later() {
        let d = dir("retry");
        let path = d.join(FILE_NAME);
        // a previous file whose peer is not an address says nothing
        write_atomic(&path, &FilterFile { version: 1, role: Role::Rx, peer: Some("rx.example".into()), video_port: 60021, test_port: 60022, hosts: vec![] }).unwrap();
        let mut f = LinkFilter::new(Some(path.clone()), Role::Rx, fake_boot);
        assert_eq!(f.peer(), None);
        f.retry_after = Duration::from_millis(300);
        let f = Arc::new(f);
        let rx = || Inputs { role: Role::Rx, peer: Some("boot.example".into()), video_port: 60021, test_port: 60022, hosts: vec![] };
        f.update(rx());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(raw(&path)["peer"], serde_json::Value::Null);
        // updates soon after (rx: at every change of the link) do not look it up again
        f.update(rx());
        f.update(rx());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(BOOT.load(Ordering::SeqCst), 1);
        // one later does, and the answer is written
        tokio::time::sleep(Duration::from_millis(300)).await;
        f.update(rx());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!((raw(&path)["peer"].as_str(), BOOT.load(Ordering::SeqCst)), (Some("198.51.100.8"), 2));
        // an answer is kept
        tokio::time::sleep(Duration::from_millis(350)).await;
        f.update(rx());
        assert_eq!(BOOT.load(Ordering::SeqCst), 2);
        fs::remove_dir_all(&d).unwrap();
    }

    #[tokio::test]
    async fn the_system_resolver() {
        assert_eq!(system_resolver("localhost".into()).await, Some(Ipv4Addr::LOCALHOST));
        assert_eq!(system_resolver("10.1.2.3".into()).await, Some(Ipv4Addr::new(10, 1, 2, 3)));
    }
}
