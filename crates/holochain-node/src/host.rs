//! `hc-wfd create`: the Windows side.
//!
//! 1. Start a Wi-Fi Direct group owner with fresh credentials, and read back
//!    the SSID and passphrase Windows actually uses.
//! 2. Find the address Windows gives the group's adapter. It may only appear
//!    once a client associates; then the QR code goes out without the host's
//!    node identity, and the joiner finds the host at its DHCP gateway.
//! 3. Start the conductor bound to that address, with no relay and no
//!    bootstrap server.
//! 4. Show the QR code, add scoped firewall rules, and serve the pairing port
//!    on that address until one peer pairs or the invitation expires.
//! 5. Run the two-way DHT exchange and report both directions.

use crate::cli::{CreateArgs, FirewallMode};
use crate::node::{self, ConductorOptions, ExchangeOptions, Node};
use crate::ui::{self, LinkKind, Stage};
use crate::{SessionPaths, unix_now};
use anyhow::{Context, Result};
use hcwfd_pairing::invite::{HostNode, check_name};
use hcwfd_pairing::{HostSession, Invite, PeerHello};
use hcwfd_wifi::firewall::{InboundRules, RULE_PREFIX};
use hcwfd_wifi::group_owner::{self, AdapterAddress, GroupOwner, GroupStatus};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub async fn run(args: CreateArgs) -> Result<bool> {
    let name = args.node.name.clone().unwrap_or_else(|| "windows".into());
    check_name(&name).map_err(anyhow::Error::msg)?;
    crate::check_node_args(&args.node)?;
    let lifetime_s = args.expires_min.max(1) * 60;
    let mut invite = Invite::generate(args.pair_port, lifetime_s, unix_now());
    if let Some(ssid) = &args.ssid {
        invite.wifi.ssid = ssid.clone();
    }
    invite.check(unix_now()).context("the requested settings")?;
    let paths = SessionPaths::new(args.node.data_dir.as_deref(), &name, &invite)?;

    // 1. The group.
    let mut group: Option<Arc<GroupOwner>> = None;
    let mut adapter: Option<AdapterAddress> = None;
    let mut before: Vec<u32> = Vec::new();
    if args.no_wifi {
        ui::stage(
            Stage::StartingGroup,
            "skipped (--no-wifi): this run uses no Wi-Fi Direct group and is NOT a Wi-Fi Direct test",
        );
    } else {
        ui::stage(
            Stage::StartingGroup,
            "legacy-client mode, as WiFiDirectLegacyAP does",
        );
        before = group_owner::wifi_direct_adapters()?
            .iter()
            .map(|a| a.if_index)
            .collect();
        let (ssid, passphrase) = (invite.wifi.ssid.clone(), invite.wifi.passphrase.clone());
        let started = tokio::task::spawn_blocking(move || GroupOwner::start(&ssid, &passphrase))
            .await?
            .context("starting the Wi-Fi Direct group owner")?;
        if started.ssid() != invite.wifi.ssid {
            println!(
                "note      Windows uses SSID {} instead of the requested {}",
                started.ssid(),
                invite.wifi.ssid
            );
        }
        if started.passphrase() != invite.wifi.passphrase {
            println!("note      Windows uses its own passphrase; the QR code carries it");
        }
        invite.wifi.ssid = started.ssid().to_string();
        invite.wifi.passphrase = started.passphrase().to_string();
        invite
            .check(unix_now())
            .context("the SSID and passphrase Windows reported")?;
        ui::stage(Stage::GroupStarted, format!("SSID {}", invite.wifi.ssid));
        group = Some(Arc::new(started));
    }

    // 2. The address.
    let mut link_ip = args.link_ip;
    if link_ip.is_none() {
        println!(
            "address   waiting up to {}s for the group adapter's address",
            args.address_wait_s
        );
        let found =
            wait_for_address(&before, Duration::from_secs(args.address_wait_s), None).await?;
        if let Some(found) = found {
            report_adapter(&found);
            link_ip = Some(found.addr);
            adapter = Some(found);
        } else {
            println!(
                "address   not assigned yet: Windows may assign it when the first client joins. \
                 The QR code will not carry this node's identity; the joiner learns it over the \
                 authenticated pairing channel instead"
            );
        }
    }

    let stop = Arc::new(AtomicBool::new(false));
    let monitor = group
        .clone()
        .map(|group| tokio::spawn(monitor_group(group, stop.clone())));

    // 3. Holochain, if the address is known now. The firewall rules go in
    // first: a program that listens with no rule of its own makes Windows
    // show its broader "allow access" prompt.
    let mut node_and_hello = None;
    let mut rules = None;
    if let Some(ip) = link_ip {
        rules = add_firewall_rules(&args, &invite, IpAddr::V4(ip))?;
        node_and_hello = Some(start_node(&args, &name, &invite, &paths, ip).await?);
        let (_, hello) = node_and_hello.as_ref().expect("just set");
        invite.host = Some(HostNode {
            name: name.clone(),
            endpoint_id: hello.endpoint_id,
            addr: hello.addr,
        });
    }

    // 4. The QR code.
    let text = invite.encode();
    let qr_path = args
        .qr_out
        .clone()
        .unwrap_or_else(|| paths.session_dir.join("invite-qr.png"));
    let qr = crate::qr::QrImage::write(&text, &qr_path)?;
    ui::stage(
        Stage::ShowingQr,
        format!(
            "network {}, expires in {} min",
            invite.wifi.ssid,
            lifetime_s / 60
        ),
    );
    println!("{}", crate::qr::render_terminal(&text)?);
    println!("qr image  {}", qr.path().display());
    println!(
        "          Copy this PNG to the Ubuntu PC (USB stick, or a phone photo of the screen) and \
         run: hc-wfd join --qr-image <file>"
    );
    println!(
        "          Anyone who reads this code can join the Wi-Fi Direct group and pair once \
         until it expires. It is deleted when this program exits."
    );
    if args.print_payload {
        println!("payload   {text}");
    }
    if !args.no_open_qr
        && cfg!(windows)
        && let Err(err) = qr.open()
    {
        println!("note      could not open the QR image: {err:#}");
    }

    // The address, now, if it only comes with the first client.
    let (node, hello) = match node_and_hello {
        Some(started) => started,
        None => {
            ui::stage(Stage::WaitingForPeer, "for a client to join the group");
            let found = wait_for_address(
                &before,
                Duration::from_secs(invite.expires_at.saturating_sub(unix_now())),
                Some(stop.clone()),
            )
            .await?
            .context("the group adapter never got an address before the invitation expired")?;
            report_adapter(&found);
            rules = add_firewall_rules(&args, &invite, IpAddr::V4(found.addr))?;
            let started = start_node(&args, &name, &invite, &paths, found.addr).await?;
            adapter = Some(found);
            started
        }
    };
    let link_ip = hello.addr.ip();

    ui::stage(
        Stage::WaitingForPeer,
        format!(
            "pairing service on {}:{}; Ctrl-C revokes the invitation",
            link_ip, args.pair_port
        ),
    );
    let session = HostSession::new(&invite);
    let paired = crate::pair::serve(
        SocketAddr::new(link_ip, args.pair_port),
        session,
        hello.clone(),
        node.clone(),
        stop.clone(),
    )
    .await?;
    // The invitation is used up; nothing else may read it.
    drop(qr);
    let peer = paired.hello;
    ui::stage(
        Stage::Pairing,
        format!(
            "paired with {} (endpoint {}…, node at {}, connected from {})",
            peer.name,
            peer.endpoint_id.short(),
            peer.addr,
            paired.from
        ),
    );

    // 5. The exchange.
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
    let link = match (&adapter, args.no_wifi) {
        (_, true) => LinkKind::NotWifiDirect(format!("{link_ip}")),
        (Some(a), false) => LinkKind::WifiDirect(format!(
            "adapter '{}' ({}), {}/{}, SSID {}",
            a.name, a.description, a.addr, a.prefix_len, invite.wifi.ssid
        )),
        (None, false) => LinkKind::WifiDirect(format!(
            "{link_ip} given with --link-ip, SSID {}",
            invite.wifi.ssid
        )),
    };
    node.print_transport().await;
    let passed = ui::report(&name, &outcome, &link);
    ui::stage(Stage::Ready, if passed { "PASS" } else { "FAIL" });

    linger(args.node.linger_s).await;
    stop.store(true, Ordering::Relaxed);
    if let Some(monitor) = monitor {
        monitor.abort();
    }
    let _ = node.conductor.shutdown().await;
    drop(rules);
    if let Some(group) = group {
        println!("group     stopping {}", invite.wifi.ssid);
        drop(group);
    }
    Ok(passed)
}

async fn start_node(
    args: &CreateArgs,
    name: &str,
    invite: &Invite,
    paths: &SessionPaths,
    ip: Ipv4Addr,
) -> Result<(Node, PeerHello)> {
    let bind = SocketAddr::new(IpAddr::V4(ip), args.node.port);
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
    let cell_id = node::install_probe(&conductor, &crate::network_seed(invite)).await?;
    let node = Node {
        conductor,
        cell_id,
        name: name.to_string(),
    };
    let identity = node.identity(Duration::from_secs(60)).await?;
    println!("node      {name}");
    println!("agent     {}", node.cell_id.agent_pubkey());
    println!("dna       {}", node.cell_id.dna_hash());
    println!("endpoint  {}", identity.url);
    let hello = PeerHello {
        name: name.to_string(),
        endpoint_id: identity.url.endpoint_id,
        addr: identity.url.addr,
        agent_infos: identity.agent_infos,
    };
    Ok((node, hello))
}

async fn wait_for_address(
    before: &[u32],
    timeout: Duration,
    stop: Option<Arc<AtomicBool>>,
) -> Result<Option<AdapterAddress>> {
    let before = before.to_vec();
    tokio::task::spawn_blocking(move || {
        group_owner::wait_for_group_address(&before, timeout, || {
            stop.as_ref()
                .is_none_or(|stop| !stop.load(Ordering::Relaxed))
        })
    })
    .await?
    .context("reading the Wi-Fi Direct adapter's address")
}

fn report_adapter(adapter: &AdapterAddress) {
    println!(
        "address   {}/{} on '{}' ({})",
        adapter.addr, adapter.prefix_len, adapter.name, adapter.description
    );
    if adapter.addr.is_link_local() {
        println!(
            "note      this is a link-local address: Windows is not running DHCP for the \
             group. On Ubuntu, join with --ipv4 link-local --host-ip {}",
            adapter.addr
        );
    }
}

/// Only this program, these two ports, this address.
fn add_firewall_rules(
    args: &CreateArgs,
    invite: &Invite,
    ip: IpAddr,
) -> Result<Option<InboundRules>> {
    let (mode, udp_port, tcp_port) = (args.firewall, args.node.port, args.pair_port);
    if mode == FirewallMode::Off || !cfg!(windows) {
        return Ok(None);
    }
    let name = format!("{RULE_PREFIX}-{}", invite.session_id.short());
    let program = std::env::current_exe().context("finding this program's path")?;
    match InboundRules::add(&name, &program, ip, udp_port, tcp_port) {
        Ok(rules) => {
            println!(
                "firewall  added '{name}': UDP {udp_port} and TCP {tcp_port} to {ip}, from the local \
                 subnet, wireless only, this program only; removed on exit"
            );
            Ok(Some(rules))
        }
        Err(err) if mode == FirewallMode::Auto => {
            println!("firewall  not added: {err}");
            println!(
                "          If Windows asks whether to allow hc-wfd, allow it; or rerun in an \
                 elevated terminal; or add the rules with scripts\\windows-firewall.ps1"
            );
            Ok(None)
        }
        Err(err) => Err(err).context("adding firewall rules (--firewall on)"),
    }
}

/// Watch the group and report what Windows says about it. Stops the run if
/// the group goes down.
async fn monitor_group(group: Arc<GroupOwner>, stop: Arc<AtomicBool>) {
    let mut last = group.status();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        for event in group.events() {
            println!("group     {event}");
        }
        let status = group.status();
        if status != last {
            println!("group     status {status:?}");
            if matches!(status, GroupStatus::Aborted(_) | GroupStatus::Stopped)
                && !stop.load(Ordering::Relaxed)
            {
                println!("group     the Wi-Fi Direct group is down; the link to the peer is gone");
            }
            last = status;
        }
    }
}

async fn linger(seconds: u64) {
    if seconds == 0 {
        return;
    }
    println!("serving the DHT for {seconds}s more so the peer can finish (Ctrl-C to stop)");
    tokio::select! {
        _ = tokio::time::sleep(Duration::from_secs(seconds)) => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}
