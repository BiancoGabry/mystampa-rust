//! MAC → IP resolution from the system ARP table, used when a printer's
//! DHCP address changed. Linux reads /proc/net/arp, Windows parses `arp -a`.

fn normalize_mac(mac: &str) -> String {
    mac.trim().to_lowercase().replace('-', ":")
}

#[cfg(not(windows))]
fn arp_table() -> Vec<(String, String)> {
    let Ok(content) = std::fs::read_to_string("/proc/net/arp") else {
        return Vec::new();
    };
    // IP address  HW type  Flags  HW address  Mask  Device
    content
        .lines()
        .skip(1)
        .filter_map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            (parts.len() >= 4).then(|| (parts[0].to_string(), normalize_mac(parts[3])))
        })
        .collect()
}

#[cfg(windows)]
fn arp_table() -> Vec<(String, String)> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let Ok(out) = std::process::Command::new("arp").arg("-a").creation_flags(CREATE_NO_WINDOW).output() else {
        return Vec::new();
    };
    // "  192.168.1.20        00-11-22-33-44-55     dinamico"
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let parts: Vec<&str> = l.split_whitespace().collect();
            if parts.len() >= 2 && parts[0].parse::<std::net::Ipv4Addr>().is_ok() && parts[1].len() == 17 {
                Some((parts[0].to_string(), normalize_mac(parts[1])))
            } else {
                None
            }
        })
        .collect()
}

pub fn resolve_ip_from_mac(mac: &str) -> Option<String> {
    let target = normalize_mac(mac);
    if target.is_empty() {
        return None;
    }
    arp_table().into_iter().find(|(_, m)| *m == target).map(|(ip, _)| ip)
}

/// The IP if set, otherwise the MAC resolved through the ARP table.
pub fn resolve_effective_ip(ip: &str, mac: Option<&str>) -> Option<String> {
    let ip = ip.trim();
    if !ip.is_empty() {
        return Some(ip.to_string());
    }
    mac.map(str::trim).filter(|m| !m.is_empty()).and_then(resolve_ip_from_mac)
}
