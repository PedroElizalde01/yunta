//! Wake-on-LAN. The other computer's hardware address is never typed: it is read from this
//! computer's ARP table right after the two connect, when the entry is fresh, and saved.

use std::io;
use std::net::{IpAddr, UdpSocket};

/// The magic packet: six 0xFF bytes, then the address sixteen times.
pub fn packet(mac: &[u8; 6]) -> Vec<u8> {
    let mut p = vec![0xFF; 6];
    for _ in 0..16 {
        p.extend_from_slice(mac);
    }
    p
}

/// Broadcasts the magic packet on the usual ports.
pub fn send(mac: &[u8; 6]) -> io::Result<()> {
    let socket = UdpSocket::bind(("0.0.0.0", 0))?;
    socket.set_broadcast(true)?;
    let p = packet(mac);
    for port in [9, 7] {
        socket.send_to(&p, ("255.255.255.255", port))?;
    }
    Ok(())
}

/// The hardware address this computer last saw for `ip`.
pub fn mac_of(ip: IpAddr) -> Option<[u8; 6]> {
    #[cfg(target_os = "linux")]
    let table = std::fs::read_to_string("/proc/net/arp").ok()?;
    #[cfg(windows)]
    let table = {
        use std::os::windows::process::CommandExt;
        // Without a console window flashing up.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("arp").args(["-a", &ip.to_string()]).creation_flags(CREATE_NO_WINDOW).output().ok()?;
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    find(&table, ip)
}

/// Finds `ip`'s address in an ARP listing: Linux's /proc/net/arp or Windows' `arp -a`. Both put
/// the address first on its line and the hardware address further along.
fn find(table: &str, ip: IpAddr) -> Option<[u8; 6]> {
    let ip = ip.to_string();
    table
        .lines()
        .filter(|l| l.split_whitespace().next() == Some(ip.as_str()))
        .flat_map(|l| l.split_whitespace().skip(1).filter_map(crate::config::parse_mac).collect::<Vec<_>>())
        .find(|mac| mac != &[0; 6])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_addresses_in_both_listings() {
        let linux = "IP address       HW type     Flags       HW address            Mask     Device\n\
                     192.168.1.20     0x1         0x2         aa:bb:cc:00:11:22     *        wlp1s0\n\
                     192.168.1.30     0x1         0x0         00:00:00:00:00:00     *        wlp1s0\n";
        let windows = "\nInterface: 192.168.1.10 --- 0x5\n  Internet Address      Physical Address      Type\n  \
                       192.168.1.20          aa-bb-cc-00-11-22     dynamic\n";
        let ip: IpAddr = "192.168.1.20".parse().unwrap();
        for table in [linux, windows] {
            assert_eq!(find(table, ip), Some([0xAA, 0xBB, 0xCC, 0x00, 0x11, 0x22]));
        }
        assert_eq!(find(linux, "192.168.1.30".parse().unwrap()), None); // incomplete entry
        let p = packet(&[1, 2, 3, 4, 5, 6]);
        assert_eq!((p.len(), &p[..7]), (102, &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 1][..]));
    }
}
