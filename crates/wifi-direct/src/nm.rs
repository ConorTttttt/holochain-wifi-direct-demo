//! Ubuntu joins the Windows group as a legacy Wi-Fi client, through
//! NetworkManager's `nmcli`.
//!
//! The joiner creates one temporary profile per session, named
//! `hc-wfd-<session>`, which:
//!
//! - never autoconnects and never becomes the default route
//!   (`ipv4.never-default`), so internet traffic stays where it was;
//! - ignores the group's DNS servers, and has IPv6 off;
//! - does not store the passphrase (`wifi-sec.psk-flags 2`, not saved). The
//!   passphrase reaches NetworkManager through a `passwd-file` that only this
//!   user can read, which is deleted as soon as `nmcli` returns, so it never
//!   appears on a command line or in a stored profile.
//!
//! Joining uses the Wi-Fi interface's one station (managed) slot, so an
//! existing Wi-Fi connection on it drops; [`restore`] reconnects it.

use crate::{Result, WifiError};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Prefix of every NetworkManager profile this program creates.
pub const PROFILE_PREFIX: &str = "hc-wfd-";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiDevice {
    pub name: String,
    pub state: String,
    /// The connection active on it, if any.
    pub connection: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub ssid: String,
    pub bssid: String,
    pub freq_mhz: Option<u32>,
    pub signal: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ipv4Mode {
    /// DHCP from the group owner, which Windows provides.
    Dhcp,
    /// IPv4 link-local (169.254/16), if the group owner runs no DHCP server.
    LinkLocal,
    /// A fixed address, such as `192.168.137.200/24`.
    Manual(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ipv4Info {
    pub addr: Ipv4Addr,
    pub prefix_len: u8,
    pub gateway: Option<Ipv4Addr>,
    /// The `routers` option of the DHCP lease.
    pub dhcp_router: Option<Ipv4Addr>,
    /// The `dhcp_server_identifier` option of the DHCP lease.
    pub dhcp_server: Option<Ipv4Addr>,
}

impl Ipv4Info {
    /// Whether `ip` is on this address's subnet.
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::MAX
            .checked_shl(32 - u32::from(self.prefix_len))
            .unwrap_or(0);
        u32::from(self.addr) & mask == u32::from(ip) & mask
    }

    /// The group owner's address: the DHCP router, else the DHCP server, else
    /// the gateway.
    pub fn group_owner(&self) -> Option<Ipv4Addr> {
        self.dhcp_router.or(self.dhcp_server).or(self.gateway)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub dev: String,
    pub src: Option<IpAddr>,
    /// A gateway, if the route has one. A direct link has none.
    pub via: Option<IpAddr>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IwLink {
    pub bssid: String,
    pub ssid: Option<String>,
    pub freq: Option<String>,
}

fn nmcli(args: &[&str]) -> Result<String> {
    if !cfg!(target_os = "linux") {
        return Err(WifiError::Unsupported(
            "joining with NetworkManager needs Linux",
        ));
    }
    crate::run("nmcli", args, &format!("nmcli {}", args.join(" ")))
}

/// Split one line of `nmcli -t` output on unescaped colons, and unescape.
pub fn split_terse(line: &str) -> Vec<String> {
    let mut fields = vec![String::new()];
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(next) = chars.next() {
                    fields.last_mut().unwrap().push(next);
                }
            }
            ':' => fields.push(String::new()),
            c => fields.last_mut().unwrap().push(c),
        }
    }
    fields
}

fn non_empty(s: &str) -> Option<String> {
    (!s.is_empty() && s != "--").then(|| s.to_string())
}

/// Check that NetworkManager runs and Wi-Fi is switched on.
pub fn check_ready() -> Result<()> {
    let running = nmcli(&["-t", "-f", "RUNNING", "general"])?;
    if running.trim() != "running" {
        return Err(WifiError::Failed("NetworkManager is not running".into()));
    }
    let radio = nmcli(&["radio", "wifi"])?;
    if radio.trim() != "enabled" {
        return Err(WifiError::Failed(
            "Wi-Fi is switched off; run `nmcli radio wifi on`".into(),
        ));
    }
    Ok(())
}

pub fn wifi_devices() -> Result<Vec<WifiDevice>> {
    let out = nmcli(&["-t", "-f", "DEVICE,TYPE,STATE,CONNECTION", "device"])?;
    Ok(parse_wifi_devices(&out))
}

fn parse_wifi_devices(out: &str) -> Vec<WifiDevice> {
    out.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 4 && f[1] == "wifi")
        .map(|f| WifiDevice {
            name: f[0].clone(),
            state: f[2].clone(),
            connection: non_empty(&f[3]),
        })
        .collect()
}

/// Rescan on `iface` and list what it sees.
pub fn scan(iface: &str) -> Result<Vec<AccessPoint>> {
    let out = nmcli(&[
        "-t",
        "-f",
        "SSID,BSSID,FREQ,SIGNAL",
        "device",
        "wifi",
        "list",
        "ifname",
        iface,
        "--rescan",
        "yes",
    ])?;
    Ok(parse_scan(&out))
}

fn parse_scan(out: &str) -> Vec<AccessPoint> {
    out.lines()
        .map(split_terse)
        .filter(|f| f.len() >= 4 && !f[0].is_empty())
        .map(|f| AccessPoint {
            ssid: f[0].clone(),
            bssid: f[1].clone(),
            freq_mhz: f[2].split_whitespace().next().and_then(|n| n.parse().ok()),
            signal: f[3].parse().ok(),
        })
        .collect()
}

/// Scan until `ssid` shows up, or fail at `timeout`.
pub fn find_ssid(iface: &str, ssid: &str, timeout: Duration) -> Result<AccessPoint> {
    let deadline = Instant::now() + timeout;
    let mut seen = Vec::new();
    loop {
        match scan(iface) {
            Ok(aps) => {
                if let Some(ap) = aps.iter().find(|ap| ap.ssid == ssid) {
                    return Ok(ap.clone());
                }
                seen = aps.into_iter().map(|ap| ap.ssid).collect();
            }
            // A rescan right after another is refused; the next one works.
            Err(WifiError::Command { .. }) => {}
            Err(err) => return Err(err),
        }
        if Instant::now() >= deadline {
            seen.sort();
            seen.dedup();
            return Err(WifiError::Failed(format!(
                "no network named {ssid} found on {iface} within {}s. Is the Windows group \
                 running and in range? Seen: {}",
                timeout.as_secs(),
                if seen.is_empty() {
                    "nothing".to_string()
                } else {
                    seen.join(", ")
                }
            )));
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

/// Create the session's profile. See the module docs for its settings.
pub fn add_profile(profile: &str, iface: &str, ssid: &str, ipv4: &Ipv4Mode) -> Result<()> {
    let mut args = vec![
        "connection",
        "add",
        "type",
        "wifi",
        "con-name",
        profile,
        "ifname",
        iface,
        "ssid",
        ssid,
        "wifi-sec.key-mgmt",
        "wpa-psk",
        "wifi-sec.psk-flags",
        "2",
        "connection.autoconnect",
        "no",
        "ipv4.never-default",
        "yes",
        "ipv4.ignore-auto-dns",
        "yes",
        "ipv6.method",
        "disabled",
    ];
    // Owned by this user: private to them, and creating it needs only the
    // "modify own" permission a desktop session has.
    let owner = std::env::var("USER")
        .ok()
        .map(|user| format!("user:{user}"));
    if let Some(owner) = &owner {
        args.extend(["connection.permissions", owner.as_str()]);
    }
    match ipv4 {
        Ipv4Mode::Dhcp => args.extend(["ipv4.method", "auto"]),
        Ipv4Mode::LinkLocal => args.extend(["ipv4.method", "link-local"]),
        Ipv4Mode::Manual(cidr) => args.extend(["ipv4.method", "manual", "ipv4.addresses", cidr]),
    }
    nmcli(&args).map(|_| ())
}

/// A file only this user can read, deleted on drop.
struct SecretFile(PathBuf);

impl SecretFile {
    fn create(contents: &str) -> Result<Self> {
        use std::io::Write;
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = dir.join(format!("hc-wfd-psk-{}-{nanos}", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options
            .open(&path)
            .map_err(|err| WifiError::Failed(format!("creating {}: {err}", path.display())))?;
        let file_guard = Self(path);
        file.write_all(contents.as_bytes())
            .map_err(|err| WifiError::Failed(format!("writing the passwd-file: {err}")))?;
        Ok(file_guard)
    }
}

impl Drop for SecretFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Bring the profile up with this passphrase, waiting up to `timeout`.
pub fn activate(profile: &str, passphrase: &str, timeout: Duration) -> Result<()> {
    if passphrase.contains('\n') {
        return Err(WifiError::Failed(
            "the passphrase contains a line break".into(),
        ));
    }
    let secret = SecretFile::create(&format!("802-11-wireless-security.psk:{passphrase}\n"))?;
    let wait = timeout.as_secs().max(1).to_string();
    let file = secret.0.to_string_lossy().into_owned();
    nmcli(&[
        "--wait",
        &wait,
        "connection",
        "up",
        "id",
        profile,
        "passwd-file",
        &file,
    ])
    .map(|_| ())
}

/// Whether the profile is active, and on which device.
pub fn active_device(profile: &str) -> Result<Option<String>> {
    let out = nmcli(&["-t", "-f", "NAME,DEVICE", "connection", "show", "--active"])?;
    Ok(out
        .lines()
        .map(split_terse)
        .find(|f| f.len() >= 2 && f[0] == profile)
        .and_then(|f| non_empty(&f[1])))
}

/// Delete a profile, bringing it down first. Deleting one that does not
/// exist is not an error.
pub fn delete_profile(profile: &str) -> Result<()> {
    match nmcli(&["connection", "delete", "id", profile]) {
        Ok(_) => Ok(()),
        Err(WifiError::Command { stderr, .. })
            if stderr.contains("unknown connection") || stderr.contains("not found") =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

/// Profiles this program created, including ones left by a crash.
pub fn own_profiles() -> Result<Vec<String>> {
    let out = nmcli(&["-t", "-f", "NAME", "connection", "show"])?;
    Ok(out
        .lines()
        .map(|line| split_terse(line).remove(0))
        .filter(|name| name.starts_with(PROFILE_PREFIX))
        .collect())
}

/// Reconnect a connection that joining displaced.
pub fn restore(connection: &str) -> Result<()> {
    nmcli(&["--wait", "30", "connection", "up", "id", connection]).map(|_| ())
}

/// The device's IPv4 address and what DHCP said about the network.
pub fn device_ipv4(iface: &str) -> Result<Option<Ipv4Info>> {
    let out = nmcli(&[
        "-t",
        "-f",
        "IP4.ADDRESS,IP4.GATEWAY,DHCP4.OPTION",
        "device",
        "show",
        iface,
    ])?;
    Ok(parse_device_ipv4(&out))
}

fn parse_device_ipv4(out: &str) -> Option<Ipv4Info> {
    let mut addr = None;
    let mut gateway = None;
    let mut dhcp_router = None;
    let mut dhcp_server = None;
    for line in out.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = split_terse(value).join(":");
        if key.starts_with("IP4.ADDRESS") && addr.is_none() {
            if let Some((ip, prefix)) = value.split_once('/')
                && let (Ok(ip), Ok(prefix)) = (ip.parse::<Ipv4Addr>(), prefix.parse::<u8>())
                && prefix <= 32
            {
                addr = Some((ip, prefix));
            }
        } else if key == "IP4.GATEWAY" {
            gateway = value.parse().ok();
        } else if key.starts_with("DHCP4.OPTION")
            && let Some((name, option)) = value.split_once(" = ")
        {
            // `routers` may list several; the first is the group owner.
            let first = option.split_whitespace().next().unwrap_or_default();
            match name.trim() {
                "routers" => dhcp_router = first.parse().ok(),
                "dhcp_server_identifier" => dhcp_server = first.parse().ok(),
                _ => {}
            }
        }
    }
    let (addr, prefix_len) = addr?;
    Some(Ipv4Info {
        addr,
        prefix_len,
        gateway,
        dhcp_router,
        dhcp_server,
    })
}

/// Wait for the device to have an IPv4 address.
pub fn wait_for_ipv4(iface: &str, timeout: Duration) -> Result<Ipv4Info> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(info) = device_ipv4(iface)? {
            return Ok(info);
        }
        if Instant::now() >= deadline {
            return Err(WifiError::Failed(format!(
                "{iface} joined the group but got no IPv4 address within {}s. If the group \
                 owner runs no DHCP server, join with --ipv4 link-local or --ipv4 <ip>/<prefix>",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

/// The route the kernel picks to `ip`.
pub fn route_to(ip: IpAddr) -> Result<Route> {
    if !cfg!(target_os = "linux") {
        return Err(WifiError::Unsupported("route lookup with `ip` needs Linux"));
    }
    let target = ip.to_string();
    let out = crate::run(
        "ip",
        &["-o", "route", "get", &target],
        &format!("ip -o route get {target}"),
    )?;
    parse_route(&out).ok_or_else(|| WifiError::Failed(format!("no route to {ip}: {}", out.trim())))
}

fn parse_route(out: &str) -> Option<Route> {
    let words: Vec<&str> = out.split_whitespace().collect();
    let after = |key: &str| {
        words
            .iter()
            .position(|w| *w == key)
            .and_then(|i| words.get(i + 1))
            .copied()
    };
    Some(Route {
        dev: after("dev")?.to_string(),
        src: after("src").and_then(|s| s.parse().ok()),
        via: after("via").and_then(|s| s.parse().ok()),
    })
}

/// What `iw` says the interface is associated with.
pub fn iw_link(iface: &str) -> Option<IwLink> {
    let out = crate::run("iw", &["dev", iface, "link"], "iw dev link").ok()?;
    parse_iw_link(&out)
}

fn parse_iw_link(out: &str) -> Option<IwLink> {
    let mut lines = out.lines().map(str::trim);
    let first = lines.next()?;
    let bssid = first
        .strip_prefix("Connected to ")?
        .split_whitespace()
        .next()?
        .to_string();
    let mut link = IwLink {
        bssid,
        ssid: None,
        freq: None,
    };
    for line in lines {
        if let Some(ssid) = line.strip_prefix("SSID: ") {
            link.ssid = Some(ssid.to_string());
        } else if let Some(freq) = line.strip_prefix("freq: ") {
            link.freq = Some(freq.to_string());
        }
    }
    Some(link)
}

/// Whether the interface is a Wi-Fi interface.
pub fn is_wireless(iface: &str) -> bool {
    let dir = std::path::Path::new("/sys/class/net").join(iface);
    dir.join("wireless").exists() || dir.join("phy80211").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terse_fields_split_on_unescaped_colons_only() {
        assert_eq!(
            split_terse(r"DIRECT-hc-AbC:AA\:BB\:CC\:DD\:EE\:FF:2437 MHz:70"),
            vec!["DIRECT-hc-AbC", "AA:BB:CC:DD:EE:FF", "2437 MHz", "70"]
        );
        assert_eq!(split_terse(r"a\\b:c"), vec![r"a\b", "c"]);
        assert_eq!(split_terse(""), vec![""]);
    }

    #[test]
    fn wifi_devices_are_found_with_their_connections() {
        let out = "eno1:ethernet:connected:netplan-eno1\n\
                   wlp0s20f3:wifi:connected:HomeWifi\n\
                   p2p-dev-wlp0s20f3:wifi-p2p:disconnected:\n\
                   wlx00:wifi:disconnected:\n";
        assert_eq!(
            parse_wifi_devices(out),
            vec![
                WifiDevice {
                    name: "wlp0s20f3".into(),
                    state: "connected".into(),
                    connection: Some("HomeWifi".into()),
                },
                WifiDevice {
                    name: "wlx00".into(),
                    state: "disconnected".into(),
                    connection: None,
                },
            ]
        );
    }

    #[test]
    fn scan_results_parse_and_hidden_networks_are_skipped() {
        let out = "DIRECT-hc-Xy12ab:12\\:34\\:56\\:78\\:9A\\:BC:5180 MHz:82\n\
                   :DE\\:AD\\:BE\\:EF\\:00\\:01:2412 MHz:30\n\
                   Home:01\\:02\\:03\\:04\\:05\\:06:2437 MHz:55\n";
        let aps = parse_scan(out);
        assert_eq!(aps.len(), 2);
        assert_eq!(
            aps[0],
            AccessPoint {
                ssid: "DIRECT-hc-Xy12ab".into(),
                bssid: "12:34:56:78:9A:BC".into(),
                freq_mhz: Some(5180),
                signal: Some(82),
            }
        );
    }

    #[test]
    fn device_ipv4_prefers_dhcp_routers_for_the_group_owner() {
        let out = "IP4.ADDRESS[1]:192.168.137.42/24\n\
                   IP4.GATEWAY:\n\
                   DHCP4.OPTION[1]:broadcast_address = 192.168.137.255\n\
                   DHCP4.OPTION[2]:dhcp_server_identifier = 192.168.137.1\n\
                   DHCP4.OPTION[3]:routers = 192.168.137.1\n\
                   DHCP4.OPTION[4]:subnet_mask = 255.255.255.0\n";
        let info = parse_device_ipv4(out).unwrap();
        assert_eq!(info.addr, Ipv4Addr::new(192, 168, 137, 42));
        assert_eq!(info.prefix_len, 24);
        assert_eq!(info.gateway, None);
        assert_eq!(info.group_owner(), Some(Ipv4Addr::new(192, 168, 137, 1)));
        assert!(info.contains(Ipv4Addr::new(192, 168, 137, 1)));
        assert!(!info.contains(Ipv4Addr::new(192, 168, 1, 1)));
    }

    #[test]
    fn device_ipv4_works_on_another_subnet_and_without_dhcp() {
        let out = "IP4.ADDRESS[1]:10.21.4.77/16\nIP4.GATEWAY:10.21.0.1\n";
        let info = parse_device_ipv4(out).unwrap();
        assert_eq!(info.group_owner(), Some(Ipv4Addr::new(10, 21, 0, 1)));
        assert!(info.contains(Ipv4Addr::new(10, 21, 200, 3)));

        let link_local = parse_device_ipv4("IP4.ADDRESS[1]:169.254.10.20/16\n").unwrap();
        assert_eq!(link_local.group_owner(), None);

        assert_eq!(parse_device_ipv4("IP4.GATEWAY:\n"), None);
    }

    #[test]
    fn routes_on_a_link_have_no_gateway() {
        let route =
            parse_route("192.168.137.1 dev wlp0s20f3 src 192.168.137.42 uid 1000 \\    cache ")
                .unwrap();
        assert_eq!(route.dev, "wlp0s20f3");
        assert_eq!(route.src, Some("192.168.137.42".parse().unwrap()));
        assert_eq!(route.via, None);

        let routed =
            parse_route("8.8.8.8 via 192.168.1.1 dev eno1 src 192.168.1.5 uid 1000").unwrap();
        assert_eq!(routed.via, Some("192.168.1.1".parse().unwrap()));
        assert_eq!(
            parse_route("RTNETLINK answers: Network is unreachable"),
            None
        );
    }

    #[test]
    fn iw_link_reports_the_group_owner_bssid_and_ssid() {
        let out = "Connected to 12:34:56:78:9a:bc (on wlp0s20f3)\n\
                   \tSSID: DIRECT-hc-Xy12ab\n\
                   \tfreq: 5180.0\n\
                   \tsignal: -41 dBm\n";
        let link = parse_iw_link(out).unwrap();
        assert_eq!(link.bssid, "12:34:56:78:9a:bc");
        assert_eq!(link.ssid.as_deref(), Some("DIRECT-hc-Xy12ab"));
        assert_eq!(link.freq.as_deref(), Some("5180.0"));
        assert_eq!(parse_iw_link("Not connected.\n"), None);
    }
}
