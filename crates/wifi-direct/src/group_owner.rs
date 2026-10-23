//! Windows as the owner of a Wi-Fi Direct group that legacy clients can join.
//!
//! This is WinRT's `WiFiDirectAdvertisementPublisher` with
//! `LegacySettings.IsEnabled`, the API of Microsoft's WiFiDirectLegacyAP
//! sample. Windows runs the group as an autonomous group owner and lets a
//! device without Wi-Fi Direct support, such as an Ubuntu PC through
//! NetworkManager, join it as an ordinary WPA2-PSK station. It is not Mobile
//! Hotspot or a soft AP, and needs neither; Mobile Hotspot must be off.
//!
//! Every WinRT object lives on one thread that this module owns, so nothing
//! crosses apartments. The group stops when the [`GroupOwner`] is dropped or
//! the process exits.
//!
//! Windows raises `WiFiDirectConnectionListener.ConnectionRequested` when a
//! device connects to the group. A legacy client raises it too: on the first
//! hardware run, the requests came from the Ubuntu PC's own Wi-Fi MAC as it
//! associated, and once more during the run, and the client stayed connected
//! without the request being accepted. This build only records the requests, in
//! [`GroupOwner::events`]; the pairing exchange decides who is let in.

use crate::{Result, WifiError};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Publisher state, as WinRT reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupStatus {
    Created,
    Started,
    Stopped,
    /// The group stopped by itself, with Windows' reason.
    Aborted(String),
}

/// An IPv4 address on one of Windows' Wi-Fi Direct virtual adapters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterAddress {
    pub if_index: u32,
    /// The adapter's name, such as "Local Area Connection* 12".
    pub name: String,
    /// Such as "Microsoft Wi-Fi Direct Virtual Adapter #3".
    pub description: String,
    pub addr: Ipv4Addr,
    pub prefix_len: u8,
}

/// A running group. Dropping it stops the group.
pub struct GroupOwner {
    ssid: String,
    passphrase: String,
    status: Arc<Mutex<GroupStatus>>,
    events: Arc<Mutex<Vec<String>>>,
    stop: Option<std::sync::mpsc::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for GroupOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GroupOwner")
            .field("ssid", &self.ssid)
            .field("passphrase", &"<redacted>")
            .field("status", &self.status())
            .finish()
    }
}

/// How long Windows may take to bring a group up.
#[cfg_attr(not(windows), allow(dead_code))]
const START_TIMEOUT: Duration = Duration::from_secs(20);

impl GroupOwner {
    /// Start a group with this SSID and WPA2 passphrase, and wait until
    /// Windows reports it started.
    ///
    /// The SSID and passphrase actually in use are read back from Windows
    /// afterwards; use [`GroupOwner::ssid`] and [`GroupOwner::passphrase`],
    /// not the values passed in, for the invitation.
    pub fn start(ssid: &str, passphrase: &str) -> Result<Self> {
        start_impl(ssid, passphrase)
    }

    pub fn ssid(&self) -> &str {
        &self.ssid
    }

    pub fn passphrase(&self) -> &str {
        &self.passphrase
    }

    pub fn status(&self) -> GroupStatus {
        self.status.lock().expect("poisoned").clone()
    }

    /// Diagnostic events since the last call, such as P2P connection
    /// requests this build does not accept.
    pub fn events(&self) -> Vec<String> {
        std::mem::take(&mut *self.events.lock().expect("poisoned"))
    }

    /// Stop the group and wait for Windows to tear it down.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        // Dropping the sender wakes the publisher thread.
        self.stop.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for GroupOwner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Start a group for a few seconds to see whether this adapter and driver can
/// host one, then stop it.
pub fn probe() -> Result<()> {
    let group = GroupOwner::start("DIRECT-hc-probe", &probe_passphrase())?;
    std::thread::sleep(Duration::from_secs(2));
    let status = group.status();
    group.stop();
    match status {
        GroupStatus::Started => Ok(()),
        other => Err(WifiError::Failed(format!(
            "the group started, then went to {other:?}"
        ))),
    }
}

fn probe_passphrase() -> String {
    // Throwaway: the probe group lives for two seconds.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("probe-{nanos:x}")
}

/// IPv4 addresses on Wi-Fi Direct virtual adapters that are up.
pub fn wifi_direct_adapters() -> Result<Vec<AdapterAddress>> {
    adapters_impl()
}

/// Wait for the group's adapter to come up with an IPv4 address.
///
/// `before` holds the interface indexes of Wi-Fi Direct adapters that were
/// already up before the group started, which are not the group's. A
/// link-local (169.254/16) address is returned only if no other address
/// appears within 15 seconds of it, since Windows may show one briefly
/// before the group's own. Returns `None` at the timeout, or as soon as
/// `keep_waiting` returns false.
pub fn wait_for_group_address(
    before: &[u32],
    timeout: Duration,
    mut keep_waiting: impl FnMut() -> bool,
) -> Result<Option<AdapterAddress>> {
    let deadline = Instant::now() + timeout;
    let mut link_local_since: Option<Instant> = None;
    loop {
        let candidates: Vec<AdapterAddress> = wifi_direct_adapters()?
            .into_iter()
            .filter(|a| !before.contains(&a.if_index))
            .collect();
        if let Some(found) = choose_address(&candidates, &mut link_local_since) {
            return Ok(Some(found));
        }
        if Instant::now() >= deadline || !keep_waiting() {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn choose_address(
    candidates: &[AdapterAddress],
    link_local_since: &mut Option<Instant>,
) -> Option<AdapterAddress> {
    if let Some(routable) = candidates.iter().find(|a| !a.addr.is_link_local()) {
        return Some(routable.clone());
    }
    let link_local = candidates.iter().find(|a| a.addr.is_link_local())?;
    let since = *link_local_since.get_or_insert_with(Instant::now);
    (since.elapsed() >= Duration::from_secs(15)).then(|| link_local.clone())
}

#[cfg(not(windows))]
const NEEDS_WINDOWS: &str = "creating a Wi-Fi Direct group owner needs Windows (WinRT \
     Windows.Devices.WiFiDirect); on Ubuntu, use `hc-wfd join`";

#[cfg(not(windows))]
fn start_impl(_ssid: &str, _passphrase: &str) -> Result<GroupOwner> {
    Err(WifiError::Unsupported(NEEDS_WINDOWS))
}

#[cfg(not(windows))]
fn adapters_impl() -> Result<Vec<AdapterAddress>> {
    Err(WifiError::Unsupported(NEEDS_WINDOWS))
}

#[cfg(windows)]
fn start_impl(ssid: &str, passphrase: &str) -> Result<GroupOwner> {
    let status = Arc::new(Mutex::new(GroupStatus::Created));
    let events = Arc::new(Mutex::new(Vec::new()));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = {
        let (ssid, passphrase) = (ssid.to_string(), passphrase.to_string());
        let (status, events) = (status.clone(), events.clone());
        std::thread::Builder::new()
            .name("wifi-direct-group".into())
            .spawn(move || {
                windows_impl::run_group(ssid, passphrase, status, events, ready_tx, stop_rx)
            })
            .map_err(|err| WifiError::Failed(format!("starting the group thread: {err}")))?
    };
    let started = ready_rx
        .recv_timeout(START_TIMEOUT + Duration::from_secs(5))
        .map_err(|_| WifiError::Failed("Windows did not report the group's state in time".into()))
        .and_then(|result| result);
    match started {
        Ok((ssid, passphrase)) => Ok(GroupOwner {
            ssid,
            passphrase,
            status,
            events,
            stop: Some(stop_tx),
            thread: Some(thread),
        }),
        Err(err) => {
            drop(stop_tx);
            let _ = thread.join();
            Err(err)
        }
    }
}

#[cfg(windows)]
fn adapters_impl() -> Result<Vec<AdapterAddress>> {
    windows_impl::adapters()
}

#[cfg(windows)]
mod windows_impl {
    use super::{AdapterAddress, GroupStatus, START_TIMEOUT};
    use crate::{Result, WifiError};
    use std::net::Ipv4Addr;
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Instant;
    use windows::Devices::WiFiDirect::{
        WiFiDirectAdvertisementListenStateDiscoverability, WiFiDirectAdvertisementPublisher,
        WiFiDirectAdvertisementPublisherStatus,
        WiFiDirectAdvertisementPublisherStatusChangedEventArgs, WiFiDirectConnectionListener,
        WiFiDirectConnectionRequestedEventArgs, WiFiDirectError,
    };
    use windows::Foundation::TypedEventHandler;
    use windows::Security::Credentials::PasswordCredential;
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::{HSTRING, Ref};

    fn error_name(error: WiFiDirectError) -> &'static str {
        match error {
            WiFiDirectError::Success => "Success",
            WiFiDirectError::RadioNotAvailable => {
                "RadioNotAvailable: Wi-Fi is off, or the adapter or driver lacks Wi-Fi Direct"
            }
            WiFiDirectError::ResourceInUse => {
                "ResourceInUse: Mobile Hotspot or another Wi-Fi Direct group is running"
            }
            _ => "an unknown WiFiDirectError",
        }
    }

    fn to_status(
        status: WiFiDirectAdvertisementPublisherStatus,
        error: WiFiDirectError,
    ) -> GroupStatus {
        match status {
            WiFiDirectAdvertisementPublisherStatus::Created => GroupStatus::Created,
            WiFiDirectAdvertisementPublisherStatus::Started => GroupStatus::Started,
            WiFiDirectAdvertisementPublisherStatus::Stopped => GroupStatus::Stopped,
            WiFiDirectAdvertisementPublisherStatus::Aborted => {
                GroupStatus::Aborted(error_name(error).to_string())
            }
            _ => GroupStatus::Aborted(format!("unknown publisher status {}", status.0)),
        }
    }

    fn winrt(context: &str) -> impl Fn(windows::core::Error) -> WifiError + '_ {
        move |err| WifiError::Windows(format!("{context}: {err}"))
    }

    /// Own the publisher for the life of the group, on this thread.
    pub(super) fn run_group(
        ssid: String,
        passphrase: String,
        status: Arc<Mutex<GroupStatus>>,
        events: Arc<Mutex<Vec<String>>>,
        ready: Sender<Result<(String, String)>>,
        stop: Receiver<()>,
    ) {
        if let Err(err) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            let _ = ready.send(Err(WifiError::Windows(format!("RoInitialize: {err}"))));
            return;
        }
        let (publisher, _listener) = match start(&ssid, &passphrase, &status, &events) {
            Ok(started) => started,
            Err(err) => {
                let _ = ready.send(Err(err));
                return;
            }
        };
        let read_back = wait_started(&publisher).and_then(|()| read_back(&publisher));
        let failed = read_back.is_err();
        let _ = ready.send(read_back);
        if !failed {
            // Until the GroupOwner is stopped or dropped.
            let _ = stop.recv();
        }
        let _ = publisher.Stop();
        let mut status = status.lock().expect("poisoned");
        if !matches!(*status, GroupStatus::Aborted(_)) {
            *status = GroupStatus::Stopped;
        }
    }

    fn start(
        ssid: &str,
        passphrase: &str,
        status: &Arc<Mutex<GroupStatus>>,
        events: &Arc<Mutex<Vec<String>>>,
    ) -> Result<(
        WiFiDirectAdvertisementPublisher,
        WiFiDirectConnectionListener,
    )> {
        let publisher =
            WiFiDirectAdvertisementPublisher::new().map_err(winrt("creating the publisher"))?;
        let advertisement = publisher
            .Advertisement()
            .map_err(winrt("reading the advertisement"))?;
        advertisement
            .SetListenStateDiscoverability(
                WiFiDirectAdvertisementListenStateDiscoverability::Normal,
            )
            .map_err(winrt("setting discoverability"))?;
        advertisement
            .SetIsAutonomousGroupOwnerEnabled(true)
            .map_err(winrt("enabling autonomous group owner"))?;

        let legacy = advertisement
            .LegacySettings()
            .map_err(winrt("reading the legacy settings"))?;
        legacy
            .SetIsEnabled(true)
            .map_err(winrt("enabling legacy clients"))?;
        legacy
            .SetSsid(&HSTRING::from(ssid))
            .map_err(winrt("setting the SSID"))?;
        let credential = PasswordCredential::new().map_err(winrt("creating the credential"))?;
        credential
            .SetPassword(&HSTRING::from(passphrase))
            .map_err(winrt("setting the passphrase"))?;
        legacy
            .SetPassphrase(&credential)
            .map_err(winrt("setting the passphrase"))?;

        let status_slot = status.clone();
        publisher
            .StatusChanged(&TypedEventHandler::new(
                move |_publisher: Ref<WiFiDirectAdvertisementPublisher>,
                      args: Ref<WiFiDirectAdvertisementPublisherStatusChangedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        *status_slot.lock().expect("poisoned") =
                            to_status(args.Status()?, args.Error()?);
                    }
                    Ok(())
                },
            ))
            .map_err(winrt("watching the publisher status"))?;

        // Legacy clients joining show up here too. Recorded only: a legacy
        // client needs no acceptance, and pairing authorizes the peer.
        let listener = WiFiDirectConnectionListener::new()
            .map_err(winrt("creating the connection listener"))?;
        let event_log = events.clone();
        listener
            .ConnectionRequested(&TypedEventHandler::new(
                move |_listener: Ref<WiFiDirectConnectionListener>,
                      args: Ref<WiFiDirectConnectionRequestedEventArgs>| {
                    if let Some(args) = args.as_ref() {
                        let name = args
                            .GetConnectionRequest()
                            .and_then(|request| request.DeviceInformation())
                            .and_then(|info| info.Name())
                            .map(|name| name.to_string())
                            .unwrap_or_else(|_| "an unnamed device".into());
                        event_log.lock().expect("poisoned").push(format!(
                            "device {name} is connecting to the group (legacy clients need no \
                             acceptance; pairing decides who is let in)"
                        ));
                    }
                    Ok(())
                },
            ))
            .map_err(winrt("watching connection requests"))?;

        publisher.Start().map_err(winrt("starting the group"))?;
        Ok((publisher, listener))
    }

    fn wait_started(publisher: &WiFiDirectAdvertisementPublisher) -> Result<()> {
        let deadline = Instant::now() + START_TIMEOUT;
        loop {
            let status = publisher.Status().map_err(winrt("reading the status"))?;
            match status {
                WiFiDirectAdvertisementPublisherStatus::Started => return Ok(()),
                WiFiDirectAdvertisementPublisherStatus::Created if Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(200))
                }
                WiFiDirectAdvertisementPublisherStatus::Created => {
                    return Err(WifiError::Failed(format!(
                        "Windows did not start the group within {}s",
                        START_TIMEOUT.as_secs()
                    )));
                }
                other => {
                    return Err(WifiError::Failed(format!(
                        "Windows did not start the group: status {:?}. Usual causes: Wi-Fi is \
                         off, Mobile Hotspot is on, or the driver lacks Wi-Fi Direct",
                        other.0
                    )));
                }
            }
        }
    }

    /// The SSID and passphrase Windows is actually using.
    fn read_back(publisher: &WiFiDirectAdvertisementPublisher) -> Result<(String, String)> {
        let legacy = publisher
            .Advertisement()
            .and_then(|advertisement| advertisement.LegacySettings())
            .map_err(winrt("reading back the legacy settings"))?;
        let ssid = legacy
            .Ssid()
            .map_err(winrt("reading back the SSID"))?
            .to_string();
        let passphrase = legacy
            .Passphrase()
            .and_then(|credential| credential.Password())
            .map_err(winrt("reading back the passphrase"))?
            .to_string();
        Ok((ssid, passphrase))
    }

    pub(super) fn adapters() -> Result<Vec<AdapterAddress>> {
        use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_NO_DATA, ERROR_SUCCESS};
        use windows::Win32::NetworkManagement::IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        };
        use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
        use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

        let mut size: u32 = 32 * 1024;
        for _ in 0..4 {
            // u64 words keep the buffer aligned for the structures in it.
            let mut buf: Vec<u64> = vec![0; (size as usize).div_ceil(8)];
            let first = buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
            let ret = unsafe {
                GetAdaptersAddresses(
                    AF_INET.0 as u32,
                    GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER,
                    None,
                    Some(first),
                    &mut size,
                )
            };
            if ret == ERROR_BUFFER_OVERFLOW.0 {
                continue;
            }
            if ret == ERROR_NO_DATA.0 {
                return Ok(Vec::new());
            }
            if ret != ERROR_SUCCESS.0 {
                return Err(WifiError::Windows(format!(
                    "GetAdaptersAddresses failed: {ret}"
                )));
            }

            let mut out = Vec::new();
            let mut adapter = first.cast_const();
            while !adapter.is_null() {
                // SAFETY: the list lives in `buf`, which outlives this loop.
                let a = unsafe { &*adapter };
                let description = unsafe { a.Description.to_string() }.unwrap_or_default();
                if a.OperStatus == IfOperStatusUp && description.contains("Wi-Fi Direct") {
                    let name = unsafe { a.FriendlyName.to_string() }.unwrap_or_default();
                    let if_index = unsafe { a.Anonymous1.Anonymous.IfIndex };
                    let mut unicast = a.FirstUnicastAddress;
                    while !unicast.is_null() {
                        let u = unsafe { &*unicast };
                        let sockaddr = u.Address.lpSockaddr;
                        if !sockaddr.is_null() && unsafe { (*sockaddr).sa_family } == AF_INET {
                            let sin = unsafe { &*sockaddr.cast::<SOCKADDR_IN>() };
                            let raw = unsafe { sin.sin_addr.S_un.S_addr };
                            out.push(AdapterAddress {
                                if_index,
                                name: name.clone(),
                                description: description.clone(),
                                addr: Ipv4Addr::from(u32::from_be(raw)),
                                prefix_len: u.OnLinkPrefixLength,
                            });
                        }
                        unicast = u.Next;
                    }
                }
                adapter = a.Next;
            }
            return Ok(out);
        }
        Err(WifiError::Windows(
            "GetAdaptersAddresses kept asking for a larger buffer".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(addr: &str) -> AdapterAddress {
        AdapterAddress {
            if_index: 7,
            name: "Local Area Connection* 12".into(),
            description: "Microsoft Wi-Fi Direct Virtual Adapter #3".into(),
            addr: addr.parse().unwrap(),
            prefix_len: 24,
        }
    }

    #[test]
    fn a_routable_address_is_chosen_at_once() {
        let mut since = None;
        let chosen = choose_address(
            &[adapter("169.254.3.4"), adapter("192.168.137.1")],
            &mut since,
        );
        assert_eq!(chosen.unwrap().addr.to_string(), "192.168.137.1");
    }

    #[test]
    fn a_link_local_address_is_chosen_only_after_waiting() {
        let mut since = None;
        assert_eq!(choose_address(&[adapter("169.254.3.4")], &mut since), None);
        since = Some(Instant::now() - Duration::from_secs(16));
        assert_eq!(
            choose_address(&[adapter("169.254.3.4")], &mut since)
                .unwrap()
                .addr
                .to_string(),
            "169.254.3.4"
        );
        assert_eq!(choose_address(&[], &mut since), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn starting_a_group_off_windows_says_why() {
        let err = GroupOwner::start("DIRECT-hc-x", "passphrase1").unwrap_err();
        assert!(matches!(err, WifiError::Unsupported(_)));
        assert!(err.to_string().contains("needs Windows"), "{err}");
        assert!(matches!(
            wifi_direct_adapters(),
            Err(WifiError::Unsupported(_))
        ));
    }
}
