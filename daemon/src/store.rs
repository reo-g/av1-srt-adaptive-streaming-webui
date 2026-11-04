//! Applied settings on disk: settings.json, mode 0600, replaced atomically (tmp + fsync + rename).

use crate::config::Config;
use crate::model::{DelayProfile, InputKind, Mode, SendLimit, Settings, TransportStored, VideoInput, VideoStored};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Settings before anything was applied: the camera, automatic mode, the peer from device.toml.
pub fn defaults(cfg: &Config) -> Settings {
    Settings {
        video: VideoStored {
            version: 1,
            input: VideoInput { kind: InputKind::Camera, camera: cfg.tx.camera.clone(), file: String::new(), loop_: false },
            mode: Mode::Auto,
            manual_mbps: 3.0,
        },
        transport: TransportStored {
            version: 1,
            mode: "outbound".into(),
            peer_host: cfg.tx.peer_host.clone(),
            peer_port: cfg.tx.peer_port,
            listen_port: cfg.rx.listen_port,
            delay: DelayProfile::Standard,
            cipher: "none".into(),
            send_limit: SendLimit::default(),
        },
    }
}

pub fn load(path: &Path) -> Option<Settings> {
    let mut s: Settings = serde_json::from_str(&fs::read_to_string(path).ok()?).ok()?;
    s.transport.send_limit = s.transport.send_limit.sane();
    Some(s)
}

pub fn save(path: &Path, s: &Settings) -> io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(serde_json::to_string_pretty(s).map_err(io::Error::other)?.as_bytes())?;
        f.sync_all()?;
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
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn roundtrip_private_file() {
        let dir = std::env::temp_dir().join(format!("ctrld-store-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let cfg = Config::parse("id = \"TX\"\nrole = \"tx\"\n[tx]\npeer_host = \"10.1.1.1\"\n[media]\nrepo = \"/m\"\n").unwrap();
        let mut s = defaults(&cfg);
        assert_eq!(load(&path), None);
        s.video.mode = Mode::Manual;
        s.video.version = 7;
        save(&path, &s).unwrap();
        assert_eq!(load(&path), Some(s.clone()));
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(!path.with_extension("json.tmp").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn send_limit_on_disk() {
        let dir = std::env::temp_dir().join(format!("ctrld-store-limit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let cfg = Config::parse("id = \"TX\"\nrole = \"tx\"\n[tx]\npeer_host = \"10.1.1.1\"\n[media]\nrepo = \"/m\"\n").unwrap();
        let mut s = defaults(&cfg);
        assert_eq!(s.transport.send_limit, SendLimit::default());
        assert_eq!((s.transport.send_limit.mbps, s.transport.send_limit.kbps()), (5.0, 5000));
        // a file from before the send cap existed: the rest is kept, the cap is the default
        let mut old = serde_json::to_value(&s).unwrap();
        old["transport"].as_object_mut().unwrap().remove("send_limit");
        old["transport"]["peer_host"] = "10.9.9.9".into();
        fs::write(&path, old.to_string()).unwrap();
        let back = load(&path).unwrap();
        assert_eq!((back.transport.peer_host.as_str(), &back.transport.send_limit), ("10.9.9.9", &SendLimit::default()));
        // what is written
        s.transport.send_limit = SendLimit {
            mbps: 5.26,
            source: crate::model::LimitSource::Measured,
            measured_at: Some("2026-11-04T14:50:12+09:00".into()),
            test_id: Some("1793770000-3".into()),
        };
        save(&path, &s).unwrap();
        let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            v["transport"]["send_limit"],
            serde_json::json!({ "mbps": 5.26, "source": "measured", "measured_at": "2026-11-04T14:50:12+09:00", "test_id": "1793770000-3" })
        );
        assert_eq!(load(&path), Some(s.clone()));
        // edited by hand out of the range: the default
        s.transport.send_limit = SendLimit::manual(90.0);
        save(&path, &s).unwrap();
        assert_eq!(load(&path).unwrap().transport.send_limit, SendLimit::default());
        fs::remove_dir_all(&dir).unwrap();
    }
}
