//! Holochain over Wi-Fi Direct with QR-code pairing.
//!
//! See the README for the procedure, and docs/architecture.md for how the
//! pieces fit.

mod cli;
mod host;
mod join;
mod node;
mod pair;
mod qr;
mod ui;

use anyhow::{Context, Result, bail};
use clap::Parser;
use cli::{Cli, Command, NodeArgs};
use hcwfd_pairing::Invite;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const DEFAULT_LOG_FILTER: &str = "warn,hc_wfd=info,kitsune2_transport_iroh=info";

fn main() -> ExitCode {
    // As the stock holochain binary does.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    if std::env::var_os("RUST_LOG").is_none() {
        // SAFETY: no other threads exist yet.
        unsafe { std::env::set_var("RUST_LOG", DEFAULT_LOG_FILTER) };
    }
    if let Err(err) = holochain_trace::init_fmt(holochain_trace::Output::Compact) {
        eprintln!("logging: {err}");
    }
    let from_menu = cli.command.is_none();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: starting the async runtime: {err}");
            return ExitCode::from(2);
        }
    };
    let code = runtime.block_on(async {
        let command = match cli.command {
            Some(command) => command,
            None => match menu().await {
                Ok(Some(command)) => command,
                Ok(None) => return ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("error: {err:#}");
                    return ExitCode::from(2);
                }
            },
        };
        tokio::select! {
            result = dispatch(command) => match result {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
                Err(err) => {
                    match ui::current() {
                        Some(stage) => println!("\nFAILED at '{}': {err:#}", stage.title()),
                        None => println!("\nFAILED: {err:#}"),
                    }
                    ExitCode::from(2)
                }
            },
            _ = tokio::signal::ctrl_c() => {
                println!("\ninterrupted: stopping, revoking the invitation and cleaning up");
                ExitCode::from(130)
            }
        }
    });
    // Let the conductor's tasks stop with the runtime, not mid-print.
    runtime.shutdown_timeout(std::time::Duration::from_secs(5));
    if from_menu && ui::interactive() {
        let _ = ui::ask("\nPress Enter to close. ");
    }
    code
}

async fn dispatch(command: Command) -> Result<bool> {
    match command {
        Command::Create(args) => host::run(args).await,
        Command::Join(args) => join::run(args).await,
        Command::Leave => tokio::task::spawn_blocking(leave).await?,
        Command::Probe => tokio::task::spawn_blocking(probe).await?,
        Command::Decode { input } => decode(&input),
    }
}

async fn menu() -> Result<Option<Command>> {
    if !ui::interactive() {
        bail!("no command given; run `hc-wfd --help`");
    }
    tokio::task::spawn_blocking(|| {
        println!("Holochain over Wi-Fi Direct");
        println!();
        println!(
            "  1) Create Holochain Network   (Windows: start a Wi-Fi Direct group, show a QR code)"
        );
        println!("  2) Join Holochain Network     (Ubuntu: read the QR code and join)");
        println!("  3) Leave the Wi-Fi Direct group and reconnect Wi-Fi (Ubuntu)");
        println!("  4) Check this machine");
        println!("  q) Quit");
        let default = if cfg!(windows) { "1" } else { "2" };
        let choice = ui::ask(&format!("\nChoice [{default}]: "))?;
        let choice = if choice.is_empty() {
            default
        } else {
            choice.as_str()
        };
        let argv: &[&str] = match choice {
            "1" => &["hc-wfd", "create"],
            "2" => &["hc-wfd", "join"],
            "3" => &["hc-wfd", "leave"],
            "4" => &["hc-wfd", "probe"],
            _ => return Ok(None),
        };
        Ok(Cli::parse_from(argv).command)
    })
    .await?
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Every session is its own DHT: the probe DNA with the session ID as its
/// network seed. Nothing from an earlier run is counted, and a node that did
/// not read this session's QR code is in another DNA altogether.
pub fn network_seed(invite: &Invite) -> String {
    format!("hc-wfd-{}", invite.session_id)
}

pub fn check_node_args(args: &NodeArgs) -> Result<()> {
    if args.note_bytes > node::MAX_NOTE_BYTES {
        bail!("--note-bytes can be at most {}", node::MAX_NOTE_BYTES);
    }
    if args.port == 0 {
        bail!("--port must not be 0: the peer is told this port");
    }
    Ok(())
}

/// Where a session keeps its conductor data.
pub struct SessionPaths {
    pub session_dir: PathBuf,
}

impl SessionPaths {
    pub fn new(base: Option<&Path>, name: &str, invite: &Invite) -> Result<Self> {
        let base = match base {
            Some(base) => base.to_path_buf(),
            // Per-user: a folder under C:\ would let other local users read
            // the live QR image.
            None if cfg!(windows) => std::env::var_os("LOCALAPPDATA")
                .map(|dir| PathBuf::from(dir).join("hc-wfd"))
                .unwrap_or_else(|| PathBuf::from("hc-wfd-data")),
            None => std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join("hcw"))
                .unwrap_or_else(|| PathBuf::from("hcw")),
        };
        let session_dir = base.join(format!("{name}-{}", invite.session_id.short()));
        std::fs::create_dir_all(&session_dir)
            .with_context(|| format!("creating {}", session_dir.display()))?;
        println!("data      {}", session_dir.display());
        Ok(Self { session_dir })
    }
}

/// The Wi-Fi connection that joining displaced, so `hc-wfd leave` can bring
/// it back even after a crash. Holds a connection name, no secrets.
pub mod wifi_state {
    use anyhow::{Context, Result};
    use std::path::PathBuf;

    fn path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
        Some(base.join("hc-wfd").join("previous-wifi"))
    }

    pub fn save(iface: &str, connection: &str) -> Result<()> {
        let Some(path) = path() else { return Ok(()) };
        std::fs::create_dir_all(path.parent().expect("has a parent"))?;
        std::fs::write(&path, format!("{iface}\t{connection}\n"))
            .with_context(|| format!("writing {}", path.display()))
    }

    pub fn load() -> Option<(String, String)> {
        let text = std::fs::read_to_string(path()?).ok()?;
        let (iface, connection) = text.trim_end().split_once('\t')?;
        Some((iface.to_string(), connection.to_string()))
    }

    pub fn clear() {
        if let Some(path) = path() {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn leave() -> Result<bool> {
    use hcwfd_wifi::nm;
    let profiles = nm::own_profiles()?;
    if profiles.is_empty() {
        println!("wifi      no hc-wfd profiles");
    }
    for profile in profiles {
        nm::delete_profile(&profile)?;
        println!("wifi      deleted {profile}");
    }
    if let Some((iface, connection)) = wifi_state::load() {
        nm::restore(&connection)
            .with_context(|| format!("reconnecting {connection} on {iface}"))?;
        println!("wifi      reconnected {connection} on {iface}");
        wifi_state::clear();
    }
    Ok(true)
}

fn probe() -> Result<bool> {
    let mut ok = true;
    if cfg!(windows) {
        println!("== Wi-Fi Direct group owner");
        match hcwfd_wifi::group_owner::probe() {
            Ok(()) => {
                println!("PASS  this adapter can host a Wi-Fi Direct group with legacy clients")
            }
            Err(err) => {
                println!("FAIL  {err}");
                ok = false;
            }
        }
        match hcwfd_wifi::group_owner::wifi_direct_adapters() {
            Ok(adapters) if adapters.is_empty() => {
                println!("      no Wi-Fi Direct adapter is up now (normal when no group runs)")
            }
            Ok(adapters) => {
                for a in adapters {
                    println!(
                        "      up: '{}' ({}) {}/{}",
                        a.name, a.description, a.addr, a.prefix_len
                    );
                }
            }
            Err(err) => println!("WARN  listing adapters: {err}"),
        }
    } else {
        use hcwfd_wifi::nm;
        println!("== NetworkManager and Wi-Fi");
        match nm::check_ready() {
            Ok(()) => println!("PASS  NetworkManager runs and Wi-Fi is on"),
            Err(err) => {
                println!("FAIL  {err}");
                ok = false;
            }
        }
        match nm::wifi_devices() {
            Ok(devices) if devices.is_empty() => {
                println!("FAIL  no Wi-Fi interface");
                ok = false;
            }
            Ok(devices) => {
                for d in devices {
                    println!(
                        "PASS  Wi-Fi interface {} ({}{})",
                        d.name,
                        d.state,
                        d.connection
                            .map(|c| format!(", on '{c}'"))
                            .unwrap_or_default()
                    );
                }
            }
            Err(err) => {
                println!("FAIL  {err}");
                ok = false;
            }
        }
        match std::process::Command::new("zbarcam")
            .arg("--version")
            .output()
        {
            Ok(_) => println!("PASS  zbarcam is installed, for --camera"),
            Err(_) => println!(
                "INFO  zbarcam is not installed; --camera needs `sudo apt install zbar-tools`"
            ),
        }
        if std::fs::read_to_string("/etc/ufw/ufw.conf").is_ok_and(|c| c.contains("ENABLED=yes")) {
            println!(
                "WARN  ufw is enabled: open the Holochain port on the Wi-Fi interface with \
                 scripts/ubuntu-firewall.sh open after joining"
            );
        }
    }
    println!();
    println!("RESULT  {}", if ok { "PASS" } else { "FAIL" });
    Ok(ok)
}

fn decode(input: &str) -> Result<bool> {
    let path = Path::new(input);
    let text = if path.exists() {
        qr::decode_image(path)?
    } else {
        input.to_string()
    };
    let invite = Invite::decode(&text, unix_now())?;
    // Debug output redacts the passphrase and the pairing secret.
    println!("{invite:#?}");
    let remaining = invite.expires_at.saturating_sub(unix_now());
    println!("expires in {}m {}s", remaining / 60, remaining % 60);
    Ok(true)
}
