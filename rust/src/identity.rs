//! Node identity and hardware inventory (FLT-01, FLT-02). Owner: a2.
//! Contract: `node_id()` is stable across reboots, IP changes and reconnects; `host()`; `inventory()`.

use crate::proto::Inventory;
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Stable id: sha256 of the OS machine id (never sent raw), first 16 hex chars; else a random id kept in a file.
pub fn node_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| match machine_id() {
        Some(m) => hash_id(&m),
        None => persisted_random(),
    })
    .clone()
}

fn hash_id(raw: &str) -> String {
    // salted so the id is not the plain hash of the machine id
    let d = Sha256::digest(format!("ck-node-v1:{}", raw.trim().to_lowercase()));
    d[..8].iter().map(|b| format!("{b:02x}")).collect()
}

fn machine_id() -> Option<String> {
    let ok = |s: String| Some(s.trim().to_string()).filter(|s| s.len() >= 8);
    if cfg!(target_os = "linux") {
        return ["/etc/machine-id", "/var/lib/dbus/machine-id"].iter().find_map(|p| ok(std::fs::read_to_string(p).ok()?));
    }
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("ioreg", &["-rd1", "-c", "IOPlatformExpertDevice"])
    } else if cfg!(windows) {
        ("reg", &["query", r"HKLM\SOFTWARE\Microsoft\Cryptography", "/v", "MachineGuid"])
    } else {
        return None;
    };
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    parse_machine_id(&String::from_utf8_lossy(&out.stdout)).and_then(ok)
}

/// macOS: `"IOPlatformUUID" = "XXXX"`; Windows: `MachineGuid    REG_SZ    XXXX`.
fn parse_machine_id(text: &str) -> Option<String> {
    text.lines().find_map(|l| {
        if l.contains("IOPlatformUUID") {
            l.split('"').nth(3).map(String::from)
        } else if l.contains("MachineGuid") {
            l.split_whitespace().last().map(String::from)
        } else {
            None
        }
    })
}

fn persisted_random() -> String {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
        .unwrap_or_else(|| std::env::temp_dir());
    let path = base.join("ck").join("node_id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if s.trim().len() == 16 {
            return s.trim().to_string();
        }
    }
    let mut seed = vec![0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut seed);
    }
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let id = hash_id(&format!("{seed:?}{t}{}", std::process::id()));
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(&path, &id); // ponytail: if unwritable the id changes per run
    id
}

/// Where this PC keeps its node key (SEC-02): `CK_KEY_DIR` (the systemd unit sets /var/lib/ck), else the user
/// config dir + `ck/`. One file per node id, so virtual nodes in one process each have their own key.
pub fn key_path(node_id: &str) -> std::path::PathBuf {
    let dir = std::env::var_os("CK_KEY_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("XDG_CONFIG_HOME")
            .or_else(|| std::env::var_os("APPDATA"))
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")))
            .unwrap_or_else(std::env::temp_dir)
            .join("ck")
    });
    let safe: String = node_id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    dir.join(format!("node-{safe}.key"))
}

pub fn host() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "pc".into())
}

/// "aa:bb:cc:dd:ee:ff" lowercase, or None if not a usable MAC (all-zero, wrong shape).
fn norm_mac(s: &str) -> Option<String> {
    let s = s.trim().replace('-', ":").to_lowercase();
    let ok = s.split(':').count() == 6 && s.split(':').all(|h| h.len() <= 2 && !h.is_empty() && u8::from_str_radix(h, 16).is_ok());
    let s = s.split(':').map(|h| format!("{h:0>2}")).collect::<Vec<_>>().join(":");
    (ok && s != "00:00:00:00:00:00").then_some(s)
}

/// Every token of `text` that looks like a MAC, for ifconfig / getmac output. `after` picks the keyword before it ("ether ") or "" for CSV.
fn parse_macs(text: &str, after: &str) -> Vec<String> {
    let mut v: Vec<String> = text
        .lines()
        .flat_map(|l| {
            let l = l.trim();
            if after.is_empty() { l.split(',').map(|c| c.trim_matches('"').to_string()).collect::<Vec<_>>() }
            else { l.strip_prefix(after).map(|r| vec![r.split_whitespace().next().unwrap_or("").to_string()]).unwrap_or_default() }
        })
        .filter_map(|t| norm_mac(&t))
        .collect();
    v.dedup();
    v
}

/// NICs' MACs for Wake-on-LAN (NET-06). Empty if the OS tools are missing.
pub fn macs() -> Vec<String> {
    let run = |p: &str, a: &[&str]| std::process::Command::new(p).args(a).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut v = if cfg!(target_os = "linux") {
        let mut v = vec![];
        if let Ok(d) = std::fs::read_dir("/sys/class/net") {
            for e in d.flatten() {
                if e.file_name() == "lo" { continue; }
                if let Some(m) = std::fs::read_to_string(e.path().join("address")).ok().and_then(|s| norm_mac(&s)) { v.push(m); }
            }
        }
        v
    } else if cfg!(target_os = "macos") {
        parse_macs(&run("ifconfig", &[]), "ether ")
    } else if cfg!(windows) {
        parse_macs(&run("getmac", &["/fo", "csv", "/nh"]), "")
    } else { vec![] };
    v.sort();
    v.dedup();
    v
}

pub fn inventory() -> Inventory {
    use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()).with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let logical = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(sys.cpus().len().max(1)) as u32;
    let os = [System::name(), System::os_version()].into_iter().flatten().collect::<Vec<_>>().join(" ");
    Inventory {
        cpu_model: sys.cpus().first().map(|c| c.brand().trim().to_string()).unwrap_or_default(),
        cores_physical: System::physical_core_count().map(|n| n as u32).unwrap_or(logical),
        cores_logical: logical,
        ram_bytes: sys.total_memory(),
        os,
        kernel: System::kernel_version().unwrap_or_default(),
        arch: System::cpu_arch(),
        ck_version: env!("CARGO_PKG_VERSION").into(),
        macs: macs(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_short_hex_and_hides_raw() {
        let a = node_id();
        assert_eq!(a, node_id());
        assert_eq!(a.len(), 16);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(hash_id("ABC-123 "), hash_id("abc-123"));
        assert!(!hash_id("0123456789abcdef").contains("0123456789"));
    }

    #[test]
    fn parses_os_outputs() {
        assert_eq!(parse_machine_id("  | \"IOPlatformUUID\" = \"AB-CD\"\n").as_deref(), Some("AB-CD"));
        assert_eq!(parse_machine_id("    MachineGuid    REG_SZ    1234-abcd\n").as_deref(), Some("1234-abcd"));
    }

    #[test]
    fn parses_macs() {
        assert_eq!(parse_macs("lo0: x\n\tether a4:83:E7:1:2:ff \n\tether 00:00:00:00:00:00\n", "ether "), ["a4:83:e7:01:02:ff"]);
        assert_eq!(parse_macs("\"AA-BB-CC-DD-EE-FF\",\"\\Device\\Tcpip_{X}\"\n", ""), ["aa:bb:cc:dd:ee:ff"]);
        assert!(macs().iter().all(|m| norm_mac(m).as_deref() == Some(m)));
    }

    #[test]
    fn inventory_filled() {
        let i = inventory();
        assert!(i.cores_logical >= 1 && i.cores_physical >= 1 && i.ram_bytes > 0);
        assert!(!i.arch.is_empty() && !i.ck_version.is_empty());
    }
}
