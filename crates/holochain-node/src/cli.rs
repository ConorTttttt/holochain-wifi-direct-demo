//! Command-line arguments.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Holochain over Wi-Fi Direct with QR-code pairing",
    long_about = "Holochain over Wi-Fi Direct with QR-code pairing.\n\n\
        On Windows, `hc-wfd create` starts a Wi-Fi Direct group, a Holochain conductor and a \
        pairing service, and shows a QR code. On Ubuntu, `hc-wfd join` reads that QR code, \
        joins the group, pairs, and the two conductors exchange DHT entries with no router, \
        internet, relay or bootstrap server.\n\nRun with no command for a menu."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Windows: create the Wi-Fi Direct group and Holochain network, and show the QR code.
    Create(CreateArgs),
    /// Ubuntu: read the QR code, join the group, pair, and exchange DHT entries.
    Join(JoinArgs),
    /// Ubuntu: delete hc-wfd Wi-Fi profiles and reconnect the Wi-Fi network joining replaced.
    Leave,
    /// Check that this machine can take its part, without pairing.
    Probe,
    /// Show what a QR code or invitation text holds, with secrets hidden.
    Decode {
        /// An image file, or the invitation text.
        input: String,
    },
}

#[derive(Args, Debug, Clone)]
pub struct NodeArgs {
    /// This node's name, as the peer shows it. Defaults to "windows" for
    /// create and "ubuntu" for join.
    #[arg(long)]
    pub name: Option<String>,

    /// Directory for node data. Each session gets a fresh subdirectory.
    /// Defaults to %LOCALAPPDATA%\hc-wfd on Windows and ~/hcw elsewhere; keep
    /// it short on Linux, where lair's socket path must fit in 107 bytes.
    #[arg(long)]
    pub data_dir: Option<PathBuf>,

    /// UDP port of the Holochain (iroh QUIC) endpoint on the link.
    #[arg(long, default_value_t = 47001)]
    pub port: u16,

    /// Notes to write to the DHT for the other node to read.
    #[arg(long, default_value_t = 1)]
    pub notes: usize,

    /// Body size of each note, in bytes (at most 4194304).
    #[arg(long, default_value_t = 1024)]
    pub note_bytes: usize,

    /// Seconds between notes; spread them out to keep writing across a
    /// disconnect and reconnect.
    #[arg(long, default_value_t = 0)]
    pub note_interval_s: u64,

    /// Seconds to wait for the exchange to complete after pairing.
    #[arg(long, default_value_t = 600)]
    pub timeout_s: u64,

    /// Seconds to keep serving the DHT after the result, so the other node
    /// can finish. Ctrl-C ends it early.
    #[arg(long, default_value_t = 120)]
    pub linger_s: u64,

    /// Holochain network request timeout, in seconds.
    #[arg(long, default_value_t = 60)]
    pub request_timeout_s: u64,

    /// Hold no DHT data, so every read is fetched from the other node.
    #[arg(long)]
    pub zero_arc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum FirewallMode {
    /// Add scoped rules if this terminal is elevated; otherwise warn.
    Auto,
    /// Add scoped rules, or fail.
    On,
    /// Add none.
    Off,
}

#[derive(Args, Debug, Clone)]
pub struct CreateArgs {
    #[command(flatten)]
    pub node: NodeArgs,

    /// TCP port of the pairing service on the link.
    #[arg(long, default_value_t = 47002)]
    pub pair_port: u16,

    /// Minutes until the invitation expires.
    #[arg(long, default_value_t = 10)]
    pub expires_min: u64,

    /// SSID to ask Windows for. Defaults to a fresh DIRECT-hc-<random>.
    #[arg(long)]
    pub ssid: Option<String>,

    /// Where to save the QR code PNG while the invitation is open. It holds
    /// the live passphrase and pairing secret and is deleted on exit.
    #[arg(long)]
    pub qr_out: Option<PathBuf>,

    /// Do not open the QR code in the image viewer.
    #[arg(long)]
    pub no_open_qr: bool,

    /// Also print the invitation text, for pasting on the other machine.
    /// It holds the live Wi-Fi passphrase and pairing secret.
    #[arg(long)]
    pub print_payload: bool,

    /// Windows Firewall rules for the two ports, scoped to this program, the
    /// group address, the local subnet and wireless interfaces.
    #[arg(long, value_enum, default_value_t = FirewallMode::Auto)]
    pub firewall: FirewallMode,

    /// Use this address for the Holochain endpoint and pairing service
    /// instead of detecting the group adapter's.
    #[arg(long)]
    pub link_ip: Option<Ipv4Addr>,

    /// Seconds to wait for the group adapter's address before showing the
    /// QR code without the host's node identity.
    #[arg(long, default_value_t = 15)]
    pub address_wait_s: u64,

    /// Testing only: start no Wi-Fi Direct group, and use --link-ip as the
    /// link (loopback or an ordinary LAN). Such a run is NOT a Wi-Fi Direct
    /// result and says so.
    #[arg(long, requires = "link_ip")]
    pub no_wifi: bool,
}

#[derive(Args, Debug, Clone)]
pub struct JoinArgs {
    #[command(flatten)]
    pub node: NodeArgs,

    /// Read the QR code from this image: the PNG `hc-wfd create` saved, or a
    /// photo of the Windows screen.
    #[arg(long, group = "source")]
    pub qr_image: Option<PathBuf>,

    /// Read the invitation text from this file ("-" for standard input).
    #[arg(long, group = "source")]
    pub payload_file: Option<PathBuf>,

    /// The invitation text itself. Prefer --payload-file: a command line is
    /// visible to other local users and saved in shell history.
    #[arg(long, group = "source")]
    pub payload: Option<String>,

    /// Scan the QR code with a webcam, through zbarcam (zbar-tools).
    #[arg(long, group = "source")]
    pub camera: bool,

    /// Video device for --camera, such as /dev/video0.
    #[arg(long)]
    pub camera_device: Option<String>,

    /// Wi-Fi interface to join with. Defaults to the first one.
    #[arg(long)]
    pub ifname: Option<String>,

    /// Do not ask before leaving the current Wi-Fi network.
    #[arg(long)]
    pub yes: bool,

    /// IPv4 on the group: "dhcp" (Windows' group owner runs a DHCP server),
    /// "link-local", or a fixed "<ip>/<prefix>".
    #[arg(long, default_value = "dhcp")]
    pub ipv4: String,

    /// The host's address on the link, if DHCP does not say.
    #[arg(long)]
    pub host_ip: Option<IpAddr>,

    /// Seconds to scan for the group's SSID before giving up.
    #[arg(long, default_value_t = 60)]
    pub scan_timeout_s: u64,

    /// Seconds to keep trying the host's pairing service.
    #[arg(long, default_value_t = 120)]
    pub pair_timeout_s: u64,

    /// Stay on the group after the run instead of leaving it and
    /// reconnecting the previous Wi-Fi network.
    #[arg(long)]
    pub stay_joined: bool,

    /// Testing only: join no Wi-Fi network, and use --link-ip as this
    /// machine's address (loopback or an ordinary LAN). Such a run is NOT a
    /// Wi-Fi Direct result and says so.
    #[arg(long, requires = "link_ip")]
    pub no_wifi: bool,

    /// With --no-wifi, this machine's address on the link.
    #[arg(long)]
    pub link_ip: Option<IpAddr>,
}
