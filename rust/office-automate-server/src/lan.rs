//! Find a LAN device's current IPv4 address by its MAC (#175).
//!
//! DHCP can move a device between leases, which silently breaks any config
//! that pins its IP. The kernel ARP table maps IP to MAC for every host this
//! Mac has recently talked to, so the lookup is: check the table, and if the
//! MAC is not there, send one UDP datagram to every host on the local subnet
//! that contains the last known IP (which makes the kernel ARP each of them)
//! and check again.
//!
//! The sweep's UDP sends are subject to macOS Local Network privacy, so this
//! only finds anything when run from a process holding that grant -- the
//! signed server binary (#168), not an agent shell.

use std::{
    net::{Ipv4Addr, SocketAddrV4},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use ipnet::Ipv4Net;
use tokio::{net::UdpSocket, process::Command, time};

const ARP_PROGRAM: &str = "/usr/sbin/arp";
const IFCONFIG_PROGRAM: &str = "/sbin/ifconfig";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
/// Refuse to sweep anything larger than a /22 (1022 hosts).
const MAX_SWEEP_PREFIX_LEN: u8 = 22;
/// Discard protocol; nothing needs to answer, the datagram only triggers ARP.
const SWEEP_PORT: u16 = 9;
const ARP_SETTLE_POLLS: u32 = 6;
const ARP_SETTLE_INTERVAL: Duration = Duration::from_millis(500);

pub type MacAddress = [u8; 6];

/// Parses `c0:f8:53:75:1f:cf`, `C0-F8-53-75-1F-CF`, or the zero-stripped
/// form `arp` prints (`a:4d:c6:4a:b1:37`).
pub fn parse_mac(value: &str) -> Result<MacAddress> {
    let parts: Vec<&str> = value.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        bail!("invalid MAC address {value:?}: expected 6 octets");
    }
    let mut mac = [0u8; 6];
    for (octet, part) in mac.iter_mut().zip(parts) {
        if part.is_empty() || part.len() > 2 {
            bail!("invalid MAC address {value:?}: bad octet {part:?}");
        }
        *octet = u8::from_str_radix(part, 16)
            .with_context(|| format!("invalid MAC address {value:?}: bad octet {part:?}"))?;
    }
    Ok(mac)
}

pub fn format_mac(mac: &MacAddress) -> String {
    mac.iter()
        .map(|octet| format!("{octet:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Parses `arp -an` output lines such as
/// `? (192.168.4.68) at c0:f8:53:75:1f:cf on en1 ifscope [ethernet]`,
/// skipping `(incomplete)` entries.
pub fn parse_arp_table(output: &str) -> Vec<(Ipv4Addr, MacAddress)> {
    output
        .lines()
        .filter_map(|line| {
            let ip = line.split_once('(')?.1.split_once(')')?.0.parse().ok()?;
            let mac = line.split_once(" at ")?.1.split_whitespace().next()?;
            Some((ip, parse_mac(mac).ok()?))
        })
        .collect()
}

/// Parses `ifconfig` output for IPv4 networks, from lines such as
/// `inet 192.168.5.10 netmask 0xfffffc00 broadcast 192.168.7.255`.
pub fn parse_ifconfig_networks(output: &str) -> Vec<Ipv4Net> {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if words.next()? != "inet" {
                return None;
            }
            let address: Ipv4Addr = words.next()?.parse().ok()?;
            if words.next()? != "netmask" {
                return None;
            }
            let mask = u32::from_str_radix(words.next()?.trim_start_matches("0x"), 16).ok()?;
            Ipv4Net::with_netmask(address, Ipv4Addr::from(mask))
                .ok()
                .map(|net| net.trunc())
        })
        .collect()
}

/// The local network containing `near`, if it is small enough to sweep.
pub fn sweep_network(networks: &[Ipv4Net], near: Ipv4Addr) -> Option<Ipv4Net> {
    networks
        .iter()
        .filter(|net| !net.addr().is_loopback() && net.contains(&near))
        .find(|net| net.prefix_len() >= MAX_SWEEP_PREFIX_LEN)
        .copied()
}

/// The IP holding `mac`, ignoring `exclude`. After a lease change the ARP
/// cache can still map the MAC to the address that just failed, alongside
/// (or instead of) the new one; that stale entry must not end the search.
pub fn lookup_mac(
    table: &[(Ipv4Addr, MacAddress)],
    mac: &MacAddress,
    exclude: Ipv4Addr,
) -> Option<Ipv4Addr> {
    table
        .iter()
        .find(|(ip, entry)| entry == mac && *ip != exclude)
        .map(|(ip, _)| *ip)
}

/// Finds `mac` on the local network around `near`, the address that just
/// failed. Returns another address if the MAC moved, `near` itself only if
/// the sweep still shows the MAC nowhere else, and `Ok(None)` if it is not
/// seen at all or `near` is not on a sweepable local network.
pub async fn locate_mac_near(mac: &MacAddress, near: Ipv4Addr) -> Result<Option<Ipv4Addr>> {
    if let Some(ip) = lookup_mac(&arp_table().await?, mac, near) {
        return Ok(Some(ip));
    }

    let networks = parse_ifconfig_networks(&run(IFCONFIG_PROGRAM, &[]).await?);
    let Some(network) = sweep_network(&networks, near) else {
        tracing::debug!("no sweepable local network contains {near}; skipping MAC sweep");
        return Ok(None);
    };
    sweep(network).await?;

    let mut table = Vec::new();
    for _ in 0..ARP_SETTLE_POLLS {
        time::sleep(ARP_SETTLE_INTERVAL).await;
        table = arp_table().await?;
        if let Some(ip) = lookup_mac(&table, mac, near) {
            return Ok(Some(ip));
        }
    }
    Ok(table
        .iter()
        .any(|(ip, entry)| entry == mac && *ip == near)
        .then_some(near))
}

async fn sweep(network: Ipv4Net) -> Result<()> {
    let socket = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))
        .await
        .context("failed to bind LAN sweep socket")?;
    for host in network.hosts() {
        // Unreachable or unanswered hosts are the point of the sweep, not errors.
        let _ = socket
            .send_to(&[0], SocketAddrV4::new(host, SWEEP_PORT))
            .await;
    }
    Ok(())
}

async fn arp_table() -> Result<Vec<(Ipv4Addr, MacAddress)>> {
    Ok(parse_arp_table(&run(ARP_PROGRAM, &["-an"]).await?))
}

async fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = time::timeout(
        COMMAND_TIMEOUT,
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .with_context(|| format!("{program} timed out"))?
    .with_context(|| format!("failed to run {program}"))?;
    if !output.status.success() {
        bail!("{program} exited with {}", output.status);
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ERV_MAC: MacAddress = [0xc0, 0xf8, 0x53, 0x75, 0x1f, 0xcf];

    #[test]
    fn parses_mac_in_common_and_arp_forms() {
        assert_eq!(parse_mac("c0:f8:53:75:1f:cf").unwrap(), ERV_MAC);
        assert_eq!(parse_mac(" C0-F8-53-75-1F-CF ").unwrap(), ERV_MAC);
        assert_eq!(
            parse_mac("a:4d:c6:4a:b1:7").unwrap(),
            [0x0a, 0x4d, 0xc6, 0x4a, 0xb1, 0x07]
        );
        assert_eq!(format_mac(&ERV_MAC), "c0:f8:53:75:1f:cf");
    }

    #[test]
    fn rejects_malformed_mac() {
        for value in [
            "",
            "c0:f8:53:75:1f",
            "c0:f8:53:75:1f:cf:00",
            "c0:f8:53:75:1f:zz",
            "c0:f8:53:75:1f:100",
        ] {
            assert!(parse_mac(value).is_err(), "{value:?} should be rejected");
        }
    }

    #[test]
    fn parses_macos_arp_table_and_finds_mac() {
        let output = "\
? (192.168.4.1) at d4:3f:32:89:5c:12 on en1 ifscope [ethernet]
? (192.168.4.20) at a:4d:c6:4a:b1:37 on en1 ifscope [ethernet]
? (192.168.4.30) at (incomplete) on en1 ifscope [ethernet]
? (192.168.4.59) at c0:f8:53:75:1f:cf on en1 ifscope [ethernet]
? (192.168.4.68) at c0:f8:53:75:1f:cf on en1 ifscope [ethernet]
? (224.0.0.251) at 1:0:5e:0:0:fb on en1 ifscope permanent [ethernet]
";
        let table = parse_arp_table(output);
        assert_eq!(table.len(), 5);
        let failed = Ipv4Addr::new(192, 168, 4, 59);
        // The stale entry for the address that just failed is skipped.
        assert_eq!(
            lookup_mac(&table, &ERV_MAC, failed),
            Some(Ipv4Addr::new(192, 168, 4, 68))
        );
        assert_eq!(lookup_mac(&table[..3], &ERV_MAC, failed), None);
        assert_eq!(lookup_mac(&table, &[0; 6], failed), None);
    }

    #[test]
    fn picks_the_local_network_containing_the_last_known_ip() {
        let output = "\
lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
en1: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.5.10 netmask 0xfffffc00 broadcast 192.168.7.255
en9: flags=8863<UP,BROADCAST> mtu 1500
\tinet 10.0.0.2 netmask 0xff000000 broadcast 10.255.255.255
";
        let networks = parse_ifconfig_networks(output);
        assert_eq!(networks.len(), 3);
        let network = sweep_network(&networks, Ipv4Addr::new(192, 168, 4, 59)).expect("network");
        assert_eq!(network.to_string(), "192.168.4.0/22");
        assert_eq!(network.hosts().count(), 1022);
        // Too large to sweep, and not local at all.
        assert_eq!(sweep_network(&networks, Ipv4Addr::new(10, 1, 2, 3)), None);
        assert_eq!(sweep_network(&networks, Ipv4Addr::new(172, 16, 0, 5)), None);
    }
}
