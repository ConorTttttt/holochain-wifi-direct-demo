//! Wi-Fi Direct link setup for Holochain over Wi-Fi Direct.
//!
//! - [`group_owner`]: Windows becomes the group owner of a Wi-Fi Direct group
//!   that accepts legacy WPA2 clients, through WinRT's
//!   `WiFiDirectAdvertisementPublisher`, as Microsoft's WiFiDirectLegacyAP
//!   sample does. Also finds the address Windows gives the group's adapter.
//! - [`firewall`]: narrowly scoped, removable Windows Firewall rules.
//! - [`nm`]: Ubuntu joins that group as a legacy client through
//!   NetworkManager (`nmcli`), and finds the route and the group owner's
//!   address.
//!
//! Each module compiles on every platform; on the wrong one its operations
//! return [`WifiError::Unsupported`].

pub mod firewall;
pub mod group_owner;
pub mod nm;

#[derive(Debug, thiserror::Error)]
pub enum WifiError {
    #[error("{0}")]
    Unsupported(&'static str),
    #[error("`{command}` failed ({status}): {stderr}")]
    Command {
        command: String,
        status: String,
        stderr: String,
    },
    #[error("could not run `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("{0}")]
    Windows(String),
    #[error("{0}")]
    Failed(String),
}

pub type Result<T> = std::result::Result<T, WifiError>;

/// Run a command and return its stdout, or its stderr as the error.
///
/// `shown` is how the command appears in errors; pass a version without any
/// secret.
pub(crate) fn run(program: &str, args: &[&str], shown: &str) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .map_err(|source| WifiError::Spawn {
            command: shown.to_string(),
            source,
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        return Err(WifiError::Command {
            command: shown.to_string(),
            status: output.status.to_string(),
            stderr: if stderr.is_empty() { stdout } else { stderr },
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
