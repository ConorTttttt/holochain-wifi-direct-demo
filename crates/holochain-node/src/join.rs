//! `hc-wfd join`: the Ubuntu side.
//!
//! 1. Read the invitation: webcam, QR image, or pasted text.
//! 2. Join the Windows group as a legacy WPA2 client with NetworkManager,
//!    after warning that the current Wi-Fi network will drop.
//! 3. Find the host: the DHCP router of the joined group, and check that the
//!    route to it is the Wi-Fi interface with no gateway.
//! 4. Start the conductor bound to this machine's address on the link.
//! 5. Pair over the authenticated channel, and register each other.
//! 6. Run the two-way DHT exchange, keeping the link up, then leave the
//!    group and reconnect the previous Wi-Fi network.

use crate::cli::JoinArgs;
use crate::node::{self, ConductorOptions, ExchangeOptions, Node};
use crate::ui::{self, LinkKind, Stage};
use crate::{SessionPaths, unix_now};
use anyhow::{Context, Result, bail};
use hcwfd_pairing::invite::check_name;
use hcwfd_pairing::{Invite, PeerHello};
use hcwfd_wifi::nm::{self, Ipv4Info, Ipv4Mode};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub async fn run(args: JoinArgs) -> Result<bool> {
    let name = args.node.name.clone().unwrap_or_else(|| "ubuntu".into());
    check_name(&name).map_err(anyhow::Error::msg)?;
    crate::check_node_args(&args.node)?;

    // 1. The invitation.
    ui::stage(Stage::WaitingForQr, "");
    let text = read_invitation(&args).await?;
    let invite = Invite::decode(&text, unix_now()).context("reading the invitation")?;
    let remaining = invite.expires_at.saturating_sub(unix_now());
    ui::stage(
        Stage::QrRecognized,
        format!(
            "network {}, session {}, expires in {}m {}s{}",
            invite.wifi.ssid,
            invite.session_id.short(),
            remaining / 60,
            remaining % 60,
            match &invite.host {
                Some(host) => format!(
                    ", host {} (endpoint {}…) at {}",
                    host.name,
                    host.endpoint_id.short(),
                    host.addr
                ),
                None => ", host identity to come over the pairing channel".into(),
            }
        ),
    );
    let paths = SessionPaths::new(args.node.data_dir.as_deref(), &name, &invite)?;

    // 2. The group.
    let (local_ip, host_ip, link, joined) = if args.no_wifi {
        let local_ip = args.link_ip.context("--no-wifi needs --link-ip")?;
        let host_ip = args
            .host_ip
            .or(invite.host.as_ref().map(|h| h.addr.ip()))
            .context("--no-wifi needs --host-ip, or an invitation that names the host's address")?;
        ui::stage(
            Stage::JoiningWifi,
            "skipped (--no-wifi): this run joins no Wi-Fi network and is NOT a Wi-Fi Direct test",
        );
        (
            local_ip,
            host_ip,
            LinkKind::NotWifiDirect(format!("{local_ip} to {host_ip}")),
            None,
        )
    } else {
        let joined = tokio::task::spawn_blocking({
            let args = args.clone();
            let invite = invite.clone();
            move || join_group(&args, &invite)
        })
        .await??;
        let host_ip = find_host(&args, &invite, &joined.ipv4)?;
        let route = check_route(&joined.iface, host_ip)?;
        let bssid = nm::iw_link(&joined.iface)
            .map(|link| {
                format!(
                    ", BSSID {}{}",
                    link.bssid,
                    link.freq.map(|f| format!(", {f} MHz")).unwrap_or_default()
                )
            })
            .unwrap_or_default();
        ui::stage(
            Stage::WifiConnected,
            format!(
                "{} {}/{} on {}{bssid}; host {host_ip} {route}",
                joined.iface, joined.ipv4.addr, joined.ipv4.prefix_len, invite.wifi.ssid
            ),
        );
        if std::fs::read_to_string("/etc/ufw/ufw.conf").is_ok_and(|c| c.contains("ENABLED=yes")) {
            println!(
                "WARNING   ufw is enabled and may drop Windows' QUIC packets to this machine. In \
                 another terminal, run: scripts/ubuntu-firewall.sh open {}",
                args.node.port
            );
        }
        let link = LinkKind::WifiDirect(format!(
            "{} {}/{}, SSID {}{bssid}, host {host_ip}",
            joined.iface, joined.ipv4.addr, joined.ipv4.prefix_len, invite.wifi.ssid
        ));
        (IpAddr::V4(joined.ipv4.addr), host_ip, link, Some(joined))
    };
    let joined = joined.map(Arc::new);

    // 4. Holochain.
    let bind = SocketAddr::new(local_ip, args.node.port);
    ui::stage(
        Stage::StartingHolochain,
        format!("conductor bound to {bind}, no relay, no bootstrap"),
    );
    let conductor = node::start_conductor(&ConductorOptions {
        data_dir: &paths.session_dir,
        bind,
        request_timeout_s: args.node.request_timeout_s,
        zero_arc: args.node.zero_arc,
    })
    .await?;
    let cell_id = node::install_probe(&conductor, &crate::network_seed(&invite)).await?;
    let node = Node {
        conductor,
        cell_id,
        name: name.clone(),
    };
    let identity = node.identity(Duration::from_secs(60)).await?;
    println!("node      {name}");
    println!("agent     {}", node.cell_id.agent_pubkey());
    println!("dna       {}", node.cell_id.dna_hash());
    println!("endpoint  {}", identity.url);

    // 5. Pairing.
    let pair_addr = SocketAddr::new(host_ip, invite.pairing.port);
    ui::stage(
        Stage::Pairing,
        format!("authenticating with {pair_addr} using the QR code's secret"),
    );
    let own = PeerHello {
        name: name.clone(),
        endpoint_id: identity.url.endpoint_id,
        addr: identity.url.addr,
        agent_infos: identity.agent_infos,
    };
    let peer = crate::pair::join(
        pair_addr,
        local_ip,
        invite.clone(),
        own,
        node.clone(),
        Duration::from_secs(args.pair_timeout_s),
    )
    .await?;
    println!(
        "pairing   paired with {} (endpoint {}…, node at {})",
        peer.name,
        peer.endpoint_id.short(),
        peer.addr
    );

    // 6. The exchange, keeping the link up.
    let done = Arc::new(AtomicBool::new(false));
    let watchdog = joined
        .clone()
        .map(|joined| tokio::spawn(keep_joined(joined, done.clone())));
    ui::stage(Stage::Synchronizing, "writing notes and reading the peer's");
    let outcome = node::run_exchange(
        &node,
        &peer.name,
        &peer.endpoint_id,
        &ExchangeOptions {
            notes: args.node.notes,
            note_bytes: args.node.note_bytes,
            note_interval: Duration::from_secs(args.node.note_interval_s),
            timeout: Duration::from_secs(args.node.timeout_s),
        },
        || {
            ui::stage(Stage::HolochainConnected, "");
            ui::peer_connected(&peer.name);
        },
    )
    .await;
    node.print_transport().await;
    let passed = ui::report(&name, &outcome, &link);
    ui::stage(Stage::Ready, if passed { "PASS" } else { "FAIL" });

    if args.node.linger_s > 0 {
        println!(
            "serving the DHT for {}s more so the peer can finish (Ctrl-C to stop)",
            args.node.linger_s
        );
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(args.node.linger_s)) => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    done.store(true, Ordering::Relaxed);
    if let Some(watchdog) = watchdog {
        let _ = watchdog.await;
    }
    let _ = node.conductor.shutdown().await;
    if let Some(joined) = joined {
        if args.stay_joined {
            println!(
                "wifi      staying on {} (--stay-joined); `hc-wfd leave` reconnects your Wi-Fi",
                invite.wifi.ssid
            );
            joined.keep();
        }
        drop(joined);
    }
    Ok(passed)
}

async fn read_invitation(args: &JoinArgs) -> Result<String> {
    if let Some(path) = &args.qr_image {
        return crate::qr::decode_image(path);
    }
    if let Some(path) = &args.payload_file {
        return read_payload_file(path);
    }
    if let Some(text) = &args.payload {
        return Ok(text.clone());
    }
    if args.camera {
        let device = args.camera_device.clone();
        return tokio::task::spawn_blocking(move || crate::qr::scan_camera(device.as_deref()))
            .await?;
    }
    if !ui::interactive() {
        bail!("no invitation: pass --qr-image, --payload-file, --payload or --camera");
    }
    tokio::task::spawn_blocking(|| -> Result<String> {
        println!("How do you want to read the QR code shown on Windows?");
        println!("  1) Scan it with a webcam (needs zbar-tools)");
        println!("  2) Import a QR image file (the PNG hc-wfd saved, or a photo of the screen)");
        println!("  3) Paste the invitation text (starts with HCWFD1:)");
        match ui::ask("Choice [2]: ")?.as_str() {
            "1" => crate::qr::scan_camera(None),
            "3" => Ok(ui::ask("Invitation: ")?),
            _ => {
                let path = ui::ask("Image file: ")?;
                crate::qr::decode_image(&PathBuf::from(path.trim_matches(['\'', '"'])))
            }
        }
    })
    .await?
}

fn read_payload_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut text = String::new();
    if path.as_os_str() == "-" {
        std::io::stdin()
            .take(hcwfd_pairing::invite::MAX_TEXT_LEN as u64 + 1)
            .read_to_string(&mut text)?;
    } else {
        std::fs::File::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .take(hcwfd_pairing::invite::MAX_TEXT_LEN as u64 + 1)
            .read_to_string(&mut text)?;
    }
    Ok(text)
}

/// The NetworkManager profile for this session, removed on drop.
struct Joined {
    iface: String,
    profile: String,
    passphrase: String,
    previous: Option<String>,
    ipv4: Ipv4Info,
    keep: AtomicBool,
}

impl Joined {
    fn keep(&self) {
        self.keep.store(true, Ordering::Relaxed);
    }
}

impl Drop for Joined {
    fn drop(&mut self) {
        if self.keep.load(Ordering::Relaxed) {
            return;
        }
        match nm::delete_profile(&self.profile) {
            Ok(()) => println!(
                "wifi      left the group and deleted profile {}",
                self.profile
            ),
            Err(err) => println!(
                "wifi      could not delete {}: {err}; run `hc-wfd leave`",
                self.profile
            ),
        }
        if let Some(previous) = &self.previous {
            match nm::restore(previous) {
                Ok(()) => println!("wifi      reconnected {previous}"),
                Err(err) => println!(
                    "wifi      could not reconnect {previous}: {err}; run `nmcli connection up id '{previous}'`"
                ),
            }
        }
        crate::wifi_state::clear();
    }
}

fn join_group(args: &JoinArgs, invite: &Invite) -> Result<Joined> {
    nm::check_ready().context("NetworkManager")?;
    let devices = nm::wifi_devices()?;
    let device = match &args.ifname {
        Some(name) => devices
            .iter()
            .find(|d| &d.name == name)
            .with_context(|| format!("{name} is not a Wi-Fi interface NetworkManager manages"))?,
        None => devices.first().context(
            "no Wi-Fi interface found; is the Wi-Fi adapter present and managed by NetworkManager?",
        )?,
    };
    let iface = device.name.clone();

    // Scanning leaves the current connection alone, so look for the group
    // before asking to give that connection up.
    ui::stage(
        Stage::JoiningWifi,
        format!("scanning on {iface} for {}", invite.wifi.ssid),
    );
    let ap = nm::find_ssid(
        &iface,
        &invite.wifi.ssid,
        Duration::from_secs(args.scan_timeout_s),
    )?;
    println!(
        "wifi      found {} (BSSID {}, {} MHz, signal {}%)",
        ap.ssid,
        ap.bssid,
        ap.freq_mhz
            .map(|f| f.to_string())
            .unwrap_or_else(|| "?".into()),
        ap.signal
            .map(|s| s.to_string())
            .unwrap_or_else(|| "?".into()),
    );

    let previous = device
        .connection
        .clone()
        .filter(|c| !c.starts_with(nm::PROFILE_PREFIX));
    if let Some(previous) = &previous {
        println!(
            "WARNING   {iface} is connected to '{previous}'. Joining the Wi-Fi Direct group \
             disconnects it until this program ends (or `hc-wfd leave`). Wired connections \
             are not touched."
        );
        if !args.yes {
            if !ui::interactive() {
                bail!("{iface} is in use by '{previous}'; pass --yes to switch it to the group");
            }
            if !ui::confirm("Switch this Wi-Fi interface to the Wi-Fi Direct group?")? {
                bail!("cancelled; {iface} stays on '{previous}'");
            }
        }
        crate::wifi_state::save(&iface, previous)?;
    }

    let ipv4 = match args.ipv4.as_str() {
        "dhcp" => Ipv4Mode::Dhcp,
        "link-local" => Ipv4Mode::LinkLocal,
        cidr if cidr.contains('/') => Ipv4Mode::Manual(cidr.to_string()),
        other => bail!("--ipv4 must be dhcp, link-local or <ip>/<prefix>, not {other}"),
    };
    let profile = format!("{}{}", nm::PROFILE_PREFIX, invite.session_id.short());
    nm::delete_profile(&profile)?;
    nm::add_profile(&profile, &iface, &invite.wifi.ssid, &ipv4)?;
    // From here on, dropping `joined` cleans up.
    let mut joined = Joined {
        iface: iface.clone(),
        profile: profile.clone(),
        passphrase: invite.wifi.passphrase.clone(),
        previous,
        ipv4: Ipv4Info {
            addr: std::net::Ipv4Addr::UNSPECIFIED,
            prefix_len: 0,
            gateway: None,
            dhcp_router: None,
            dhcp_server: None,
        },
        keep: AtomicBool::new(false),
    };
    println!("wifi      connecting with temporary profile {profile} (passphrase not stored)");
    nm::activate(&profile, &joined.passphrase, Duration::from_secs(60))
        .with_context(|| format!("joining {}", invite.wifi.ssid))?;
    joined.ipv4 = nm::wait_for_ipv4(&iface, Duration::from_secs(45))?;
    Ok(joined)
}

/// The host's address on the link.
fn find_host(args: &JoinArgs, invite: &Invite, ipv4: &Ipv4Info) -> Result<IpAddr> {
    if let Some(ip) = args.host_ip {
        return Ok(ip);
    }
    let named = invite.host.as_ref().map(|h| h.addr.ip());
    if let Some(IpAddr::V4(ip)) = named
        && ipv4.contains(ip)
    {
        return Ok(IpAddr::V4(ip));
    }
    if let Some(ip) = ipv4.group_owner() {
        if let Some(named) = named
            && named != IpAddr::V4(ip)
        {
            println!(
                "note      the QR code named host {named}; the group's DHCP says {ip}; using {ip}"
            );
        }
        return Ok(IpAddr::V4(ip));
    }
    bail!(
        "the group's DHCP lease names no router, and the QR code names no host address on \
         {}/{}. Pass --host-ip <Windows address>",
        ipv4.addr,
        ipv4.prefix_len
    )
}

/// Check that the kernel reaches the host over the Wi-Fi interface itself.
fn check_route(iface: &str, host: IpAddr) -> Result<String> {
    let route = nm::route_to(host)?;
    if route.dev != iface {
        bail!(
            "the route to {host} goes through {} instead of {iface}; another network uses the \
             same subnet. Disconnect it, or pass --host-ip",
            route.dev
        );
    }
    if let Some(via) = route.via {
        bail!("the route to {host} goes through gateway {via}, not directly over {iface}");
    }
    if !nm::is_wireless(&route.dev) {
        bail!("{} is not a Wi-Fi interface", route.dev);
    }
    Ok(format!("directly on {iface}, no gateway"))
}

/// Re-join if the group drops, until `done`.
async fn keep_joined(joined: Arc<Joined>, done: Arc<AtomicBool>) {
    while !done.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_secs(5)).await;
        if done.load(Ordering::Relaxed) {
            break;
        }
        let profile = joined.profile.clone();
        let active = tokio::task::spawn_blocking(move || nm::active_device(&profile)).await;
        if matches!(active, Ok(Ok(Some(_)))) {
            continue;
        }
        println!(
            "wifi      the Wi-Fi Direct link is down; rejoining {}",
            joined.profile
        );
        let j = joined.clone();
        let result = tokio::task::spawn_blocking(move || {
            nm::activate(&j.profile, &j.passphrase, Duration::from_secs(30))?;
            nm::wait_for_ipv4(&j.iface, Duration::from_secs(30))
        })
        .await;
        match result {
            Ok(Ok(ipv4)) if ipv4.addr == joined.ipv4.addr => {
                println!("wifi      rejoined with the same address {}", ipv4.addr)
            }
            Ok(Ok(ipv4)) => println!(
                "wifi      rejoined, but with {} instead of {}: the Holochain endpoint stays bound \
                 to the old address. Restart both sides with a new invitation",
                ipv4.addr, joined.ipv4.addr
            ),
            Ok(Err(err)) => println!("wifi      rejoining failed: {err}; retrying"),
            Err(err) => println!("wifi      rejoining failed: {err}; retrying"),
        }
    }
}
