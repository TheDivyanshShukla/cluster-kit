//! `ck install`: run the agent as a system service (OPS-01, SEC-04). Owner: a2.
//! Contract: `unit_text(exe, head, name, user) -> String` (pure, tested) and `run(&Opts)`
//! (`ck install [--head H] [--name N] [--dry-run]`: prints the unit with --dry-run, else writes and enables it).

use crate::Opts;

const EXE: &str = "/opt/ck/ck";
const UNIT: &str = "/etc/systemd/system/ck-agent.service";
const USER: &str = "ck";

/// systemd unit: unprivileged, hardened, idle priority so students' foreground work wins.
pub fn unit_text(exe: &str, head: Option<&str>, name: &str, user: &str) -> String {
    let mut args = String::from("agent");
    if let Some(h) = head {
        args += &format!(" --head {h}");
    }
    if !name.is_empty() {
        args += &format!(" --name {name}");
    }
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    let quota = if threads > 0 { format!("CPUQuota={}%", threads * 100) } else { "# CPUQuota omitted: thread count unknown".into() };
    format!(
        "[Unit]\nDescription=ck agent\nAfter=network-online.target\nWants=network-online.target\n\n\
[Service]\nUser={user}\nExecStart={exe} {args}\nRestart=always\nRestartSec=2\n\
NoNewPrivileges=yes\nProtectSystem=strict\nProtectHome=yes\nPrivateTmp=yes\n\
Nice=19\nCPUSchedulingPolicy=idle\nMemoryMax=80%\n{quota}\n\n[Install]\nWantedBy=multi-user.target\n"
    )
}

fn commands(exe: &str) -> Vec<String> {
    vec![
        "mkdir -p /opt/ck".into(),
        format!("install -m 0755 {exe} {EXE}"),
        format!("id {USER} || useradd --system --no-create-home --shell /usr/sbin/nologin {USER}"),
        format!("write {UNIT}"),
        "systemctl daemon-reload".into(),
        "systemctl enable --now ck-agent.service".into(),
    ]
}

pub fn run(o: &Opts) {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "ck".into());
    let unit = unit_text(EXE, o.head.as_deref(), &o.name, USER);
    if o.dry_run {
        println!("# {UNIT}\n{unit}\n# commands:");
        commands(&exe).iter().for_each(|c| println!("{c}"));
        return;
    }
    if !cfg!(target_os = "linux") {
        crate::die("ck install supports Linux (systemd) only; use --dry-run to print the unit");
    }
    if std::process::Command::new("id").arg("-u").output().map(|o| o.stdout != b"0\n").unwrap_or(true) {
        crate::die("ck install must be run as root (try: sudo ck install ...)");
    }
    let sh = |cmd: &str| {
        if !std::process::Command::new("sh").args(["-c", cmd]).status().map(|s| s.success()).unwrap_or(false) {
            crate::die(&format!("failed: {cmd}"));
        }
    };
    for c in commands(&exe) {
        if let Some(path) = c.strip_prefix("write ") {
            std::fs::write(path, &unit).unwrap_or_else(|e| crate::die(&format!("write {path}: {e}")));
        } else {
            sh(&c);
        }
    }
    println!("installed: {UNIT} (enabled and started)");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_has_sec04_settings() {
        let u = unit_text("/opt/ck/ck", Some("10.0.0.1"), "lab-1", "ck");
        for want in [
            "User=ck", "Restart=always", "RestartSec=2", "After=network-online.target", "Wants=network-online.target",
            "NoNewPrivileges=yes", "ProtectSystem=strict", "ProtectHome=yes", "PrivateTmp=yes", "Nice=19",
            "CPUSchedulingPolicy=idle", "MemoryMax=80%", "CPUQuota=", "ExecStart=/opt/ck/ck agent --head 10.0.0.1 --name lab-1",
        ] {
            assert!(u.contains(want), "missing {want}");
        }
        assert!(!unit_text("/x", None, "", "ck").contains("--head"));
    }
}
