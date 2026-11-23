//! Port helpers: whether something is listening, and who holds a port.

use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

/// The process that holds a listening port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Holder {
    /// Process id.
    pub pid: u32,
    /// Short process name, e.g. `nginx`.
    pub name: String,
}

impl std::fmt::Display for Holder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (pid {})", self.name, self.pid)
    }
}

/// Whether a TCP connection to `host:port` succeeds within `timeout`.
#[must_use]
pub fn is_listening(host: &str, port: u16, timeout: Duration) -> bool {
    let Ok(addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .any(|addr| TcpStream::connect_timeout(&addr, timeout).is_ok())
}

/// Best-effort lookup of the process listening on a TCP port.
///
/// Uses `/proc` on Linux and falls back to `lsof` everywhere. Returns `None`
/// when the holder cannot be determined, for example when it belongs to
/// another user and proxlet is not privileged.
#[must_use]
pub fn holder(port: u16) -> Option<Holder> {
    #[cfg(target_os = "linux")]
    if let Some(h) = linux::holder(port) {
        return Some(h);
    }
    lsof_holder(port)
}

fn lsof_holder(port: u16) -> Option<Holder> {
    let out = Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-Fpc"])
        .output()
        .ok()?;
    parse_lsof(&String::from_utf8_lossy(&out.stdout))
}

/// Parses `lsof -Fpc` output: `p<pid>` then `c<command>` lines.
fn parse_lsof(text: &str) -> Option<Holder> {
    let mut pid = None;
    for line in text.lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().ok();
        } else if let (Some(name), Some(pid)) = (line.strip_prefix('c'), pid) {
            return Some(Holder {
                pid,
                name: name.to_owned(),
            });
        }
    }
    None
}

#[cfg(target_os = "linux")]
mod linux {
    use super::Holder;
    use std::fs;

    pub(super) fn holder(port: u16) -> Option<Holder> {
        let inodes: Vec<u64> = ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|p| fs::read_to_string(p).ok())
            .flat_map(|t| listening_inodes(&t, port))
            .collect();
        if inodes.is_empty() {
            return None;
        }
        let wanted: Vec<String> = inodes.iter().map(|i| format!("socket:[{i}]")).collect();
        for entry in fs::read_dir("/proc").ok()?.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Ok(fds) = fs::read_dir(entry.path().join("fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(link) = fs::read_link(fd.path()) {
                    if wanted.iter().any(|w| link.as_os_str() == w.as_str()) {
                        let name = fs::read_to_string(entry.path().join("comm"))
                            .map(|s| s.trim().to_owned())
                            .unwrap_or_else(|_| "unknown".into());
                        return Some(Holder { pid, name });
                    }
                }
            }
        }
        None
    }

    /// Inodes of sockets in LISTEN state (`0A`) on `port` from `/proc/net/tcp*`.
    pub(super) fn listening_inodes(table: &str, port: u16) -> Vec<u64> {
        let port_hex = format!("{port:04X}");
        table
            .lines()
            .skip(1)
            .filter_map(|line| {
                let cols: Vec<&str> = line.split_whitespace().collect();
                let local = cols.get(1)?;
                let state = cols.get(3)?;
                let inode = cols.get(9)?;
                (*state == "0A" && local.rsplit(':').next()? == port_hex)
                    .then(|| inode.parse().ok())
                    .flatten()
            })
            .collect()
    }

    #[cfg(test)]
    #[test]
    fn parses_proc_net_tcp() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000:0050 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 12345 1 0000000000000000 100 0 0 10 0\n   1: 0100007F:1F40 00000000:0000 01 00000000:00000000 00:00000000 00000000     0        0 999 1\n";
        assert_eq!(listening_inodes(table, 80), vec![12345]);
        assert_eq!(listening_inodes(table, 8000), Vec::<u64>::new());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn parses_lsof_output() {
        assert_eq!(
            parse_lsof("p4242\ncnginx\nf6\n"),
            Some(Holder {
                pid: 4242,
                name: "nginx".into()
            })
        );
        assert_eq!(parse_lsof(""), None);
    }

    #[test]
    fn detects_listening_and_free_ports() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        assert!(is_listening("127.0.0.1", port, Duration::from_millis(500)));
        drop(l);
        assert!(!is_listening("127.0.0.1", port, Duration::from_millis(200)));
    }

    #[test]
    fn finds_own_process_as_holder() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        if let Some(h) = holder(port) {
            assert_eq!(h.pid, std::process::id());
        }
    }
}
