//! Progress stages, prompts and the final report.

use crate::node::Outcome;
use std::io::{BufRead, IsTerminal, Write};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    // Host
    StartingGroup,
    GroupStarted,
    StartingHolochain,
    ShowingQr,
    WaitingForPeer,
    // Joiner
    WaitingForQr,
    QrRecognized,
    JoiningWifi,
    WifiConnected,
    // Both
    Pairing,
    HolochainConnected,
    Synchronizing,
    Ready,
}

impl Stage {
    pub fn title(self) -> &'static str {
        match self {
            Stage::StartingGroup => "Starting Wi-Fi Direct group",
            Stage::GroupStarted => "Wi-Fi Direct group started",
            Stage::StartingHolochain => "Starting Holochain",
            Stage::ShowingQr => "Scan this QR code to connect",
            Stage::WaitingForPeer => "Waiting for the other computer",
            Stage::WaitingForQr => "Waiting for QR code",
            Stage::QrRecognized => "QR code recognized",
            Stage::JoiningWifi => "Joining Wi-Fi Direct",
            Stage::WifiConnected => "Wi-Fi connection established",
            Stage::Pairing => "Pairing with peer",
            Stage::HolochainConnected => "Holochain connected",
            Stage::Synchronizing => "Synchronizing",
            Stage::Ready => "Ready",
        }
    }
}

static CURRENT: Mutex<Option<Stage>> = Mutex::new(None);

/// Enter a stage and say so.
pub fn stage(stage: Stage, detail: impl std::fmt::Display) {
    *CURRENT.lock().expect("poisoned") = Some(stage);
    let detail = detail.to_string();
    if detail.is_empty() {
        println!("\n==> {}", stage.title());
    } else {
        println!("\n==> {}: {detail}", stage.title());
    }
}

/// The stage the program was in, for reporting where it failed.
pub fn current() -> Option<Stage> {
    *CURRENT.lock().expect("poisoned")
}

/// `Peer Connected: Ubuntu`, as both screens show it.
pub fn peer_connected(name: &str) {
    let mut chars = name.chars();
    let shown: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    };
    println!();
    println!("    ******************************");
    println!("    *  Peer Connected: {shown:<10}*");
    println!("    ******************************");
}

pub fn interactive() -> bool {
    std::io::stdin().is_terminal()
}

/// Ask a question on the terminal and return the trimmed answer.
pub fn ask(question: &str) -> std::io::Result<String> {
    print!("{question}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

pub fn confirm(question: &str) -> std::io::Result<bool> {
    let answer = ask(&format!("{question} [y/N] "))?;
    Ok(matches!(answer.to_lowercase().as_str(), "y" | "yes"))
}

/// What kind of link the run used, for the RESULT line.
pub enum LinkKind {
    WifiDirect(String),
    /// `--no-wifi`: loopback or an ordinary network, given by hand.
    NotWifiDirect(String),
}

pub fn report(name: &str, outcome: &Outcome, link: &LinkKind) -> bool {
    println!();
    println!("==== {name} report ====");
    match link {
        LinkKind::WifiDirect(detail) => println!("link      Wi-Fi Direct: {detail}"),
        LinkKind::NotWifiDirect(detail) => {
            println!("link      {detail} (--no-wifi: NOT a Wi-Fi Direct link)")
        }
    }
    match outcome.connected_after {
        Some(after) => println!("connected after {:.1}s", after.as_secs_f64()),
        None => println!("connected never: the transport showed no connection to the peer"),
    }
    for d in [&outcome.inbound, &outcome.outbound] {
        println!(
            "{:>9} -> {:<9} {}  {}",
            d.from,
            d.to,
            if d.passed { "PASS" } else { "FAIL" },
            d.detail
        );
    }
    let passed = outcome.passed();
    let what = match link {
        LinkKind::WifiDirect(_) => "both directions over the Wi-Fi Direct link".to_string(),
        LinkKind::NotWifiDirect(_) => {
            "both directions, over a link that is NOT Wi-Fi Direct".to_string()
        }
    };
    println!(
        "RESULT    {} ({what})",
        if passed { "PASS" } else { "FAIL" }
    );
    passed
}
