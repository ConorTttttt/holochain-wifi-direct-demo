//! The embedded Holochain 0.7 conductor and the two-way DHT exchange test.
//!
//! The conductor's only transport is kitsune2's iroh endpoint in direct mode
//! (`irohTransport.directBindAddr`): one UDP socket bound to this machine's
//! Wi-Fi Direct address, no relay, and no bootstrap server. Peers are
//! introduced by the QR pairing exchange instead.
//!
//! The exchange reuses holochain-ble-demo's probe DNA and its byte pattern:
//! a note's body is its label repeated, and the label ends in the body size,
//! so a corrupted or truncated body is caught. Each node writes its data
//! notes, then a `sent:` manifest listing each note's label, size and
//! SHA-256. A node that has read and checked every note in its peer's
//! manifest writes a `receipt:` note listing what it read. Each node then
//! knows both directions: what it read itself, and what its peer's receipt
//! says it read.

use anyhow::{Context, Result, bail};
use hcwfd_pairing::{DirectPeerUrl, EndpointId};
use holochain::conductor::ConductorHandle;
use holochain::conductor::config::ConductorConfig;
use holochain::prelude::*;
use holochain_conductor_api::conductor::paths::DataRootPath;
use holochain_conductor_api::config::conductor::{KeystoreConfig, NetworkConfig};
use holochain_types::app::{AppBundleSource, InstallAppPayload};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// The probe hApp from holochain-ble-demo, byte for byte. Each session gives
/// it its own network seed, so every pairing starts an empty DHT.
const PROBE_HAPP: &[u8] = include_bytes!("../../../happ/probe.happ");
const ZOME: &str = "probe";

/// Lair passphrase. The keystore holds throwaway per-session keys only.
const DEMO_PASSPHRASE: &[u8] = b"holochain-wifi-direct-demo";

/// A bootstrap URL that refuses connections at once.
const NO_BOOTSTRAP_URL: &str = "http://127.0.0.1:1/";

const POLL_INTERVAL: Duration = Duration::from_secs(3);
const STATS_INTERVAL: Duration = Duration::from_secs(30);

const SENT_PREFIX: &str = "sent:";
const RECEIPT_PREFIX: &str = "receipt:";

/// The `probe_integrity` DNA refuses larger bodies.
pub const MAX_NOTE_BYTES: usize = 4 * 1024 * 1024;

/// Mirrors `probe_integrity::Note`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
struct Note {
    label: String,
    origin: String,
    body: String,
}

/// Mirrors `probe::NoteRecord`.
#[derive(Deserialize, Debug)]
struct NoteRecord {
    #[allow(dead_code)]
    action_hash: ActionHash,
    #[allow(dead_code)]
    author: AgentPubKey,
    note: Note,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
struct NoteDigest {
    label: String,
    bytes: usize,
    sha256: String,
}

/// The body of a `sent:` or `receipt:` note.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Manifest {
    from: String,
    notes: Vec<NoteDigest>,
}

pub struct ConductorOptions<'a> {
    pub data_dir: &'a Path,
    pub bind: SocketAddr,
    pub request_timeout_s: u64,
    pub zero_arc: bool,
}

pub async fn start_conductor(options: &ConductorOptions<'_>) -> Result<ConductorHandle> {
    let mut network = NetworkConfig {
        // No bootstrap server: peers come from the pairing exchange. Outside
        // test builds Holochain can not switch bootstrap off, so point it at
        // a local port that refuses at once: no DNS lookup, nothing leaves
        // the machine. The default would look up a public server.
        bootstrap_url: url2::Url2::parse(NO_BOOTSTRAP_URL),
        request_timeout_s: options.request_timeout_s,
        ..Default::default()
    };
    if options.zero_arc {
        network.target_arc_factor = 0;
    }
    // A peer whose connection failed, say while the Wi-Fi Direct link was
    // down, is skipped until a newer agent info for it arrives. Agent infos
    // last 20 minutes and are normally re-signed with 5 left; re-signing with
    // 18 left brings recovery down to about 2 minutes.
    network.advanced = Some(serde_json::json!({
        "irohTransport": { "directBindAddr": options.bind.to_string() },
        "coreSpace": {
            "reSignFreqMs": 60_000,
            "reSignExpireTimeMs": 18 * 60_000,
        },
    }));

    let config = ConductorConfig {
        data_root_path: Some(DataRootPath::from(options.data_dir.to_path_buf())),
        keystore: KeystoreConfig::LairServerInProc { lair_root: None },
        network,
        ..Default::default()
    };
    let passphrase = Arc::new(std::sync::Mutex::new(sodoken::LockedArray::from(
        DEMO_PASSPHRASE.to_vec(),
    )));
    holochain::conductor::Conductor::builder()
        .config(config)
        .passphrase(Some(passphrase))
        .build()
        .await
        .with_context(|| format!("starting the conductor bound to {}", options.bind))
}

/// Install the probe app for this session's network seed, enable it, and
/// return its cell.
pub async fn install_probe(conductor: &ConductorHandle, network_seed: &str) -> Result<CellId> {
    let app_id = format!("probe-{network_seed}");
    if conductor.get_app_info(&app_id).await?.is_none() {
        conductor
            .clone()
            .install_app_bundle(InstallAppPayload {
                source: AppBundleSource::Bytes(bytes::Bytes::from_static(PROBE_HAPP)),
                agent_key: None,
                installed_app_id: Some(app_id.clone()),
                network_seed: Some(network_seed.to_string()),
                roles_settings: None,
                ignore_genesis_failure: false,
                restore_from_dht: false,
            })
            .await
            .context("installing the probe app")?;
    }
    conductor
        .clone()
        .enable_app(app_id.clone())
        .await
        .context("enabling the probe app")?;
    // Each session has its own data directory, so this is the only app.
    conductor
        .running_cell_ids()
        .into_iter()
        .next()
        .context("the probe app has no running cell")
}

#[derive(Clone)]
pub struct Node {
    pub conductor: ConductorHandle,
    pub cell_id: CellId,
    pub name: String,
}

/// This node as the network sees it.
#[derive(Clone, Debug)]
pub struct Identity {
    pub url: DirectPeerUrl,
    /// This node's own signed agent infos, encoded.
    pub agent_infos: Vec<String>,
}

impl Node {
    async fn call<I, O>(&self, fn_name: &str, payload: I) -> Result<O>
    where
        I: Serialize + std::fmt::Debug,
        O: DeserializeOwned + std::fmt::Debug,
    {
        self.conductor
            .easy_call_zome(
                self.cell_id.agent_pubkey(),
                None,
                self.cell_id.clone(),
                ZOME,
                fn_name,
                payload,
            )
            .await
            .with_context(|| format!("calling {fn_name}"))
    }

    /// Wait until the transport has its direct URL and this node's agent
    /// info for the probe DNA is signed with it.
    pub async fn identity(&self, timeout: Duration) -> Result<Identity> {
        let start = Instant::now();
        loop {
            if let Ok(identity) = self.try_identity().await {
                return Ok(identity);
            }
            if start.elapsed() > timeout {
                bail!(
                    "the conductor did not publish an agent info with a direct URL within {}s",
                    timeout.as_secs()
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn try_identity(&self) -> Result<Identity> {
        let stats = self.conductor.dump_network_stats().await?.transport_stats;
        let own = stats.peer_urls.first().context("no peer URL yet")?;
        let url = DirectPeerUrl::parse(own.as_str())
            .with_context(|| format!("the transport's URL {own} is not a direct URL"))?;
        let agent_infos: Vec<String> = self
            .conductor
            .get_agent_infos(Some(vec![self.cell_id.dna_hash().clone()]))
            .await?
            .into_iter()
            .filter(|info| info.url.as_ref() == Some(own) && !info.is_tombstone)
            .filter_map(|info| info.encode().ok())
            .collect();
        if agent_infos.is_empty() {
            bail!("no agent info yet");
        }
        Ok(Identity { url, agent_infos })
    }

    /// Check a peer's encoded agent infos as the conductor will, plus that
    /// each is for this DNA and current, and return their URLs in order.
    pub fn agent_info_urls(&self, encoded: &[String]) -> Result<Vec<Option<String>>, String> {
        let space = self.cell_id.dna_hash().to_k2_space();
        let now = kitsune2_api::Timestamp::now();
        encoded
            .iter()
            .map(|info| {
                let info = kitsune2_api::AgentInfoSigned::decode(
                    &kitsune2_core::Ed25519Verifier,
                    info.as_bytes(),
                )
                .map_err(|err| format!("an agent info does not verify: {err}"))?;
                if info.space != space {
                    return Err(
                        "an agent info is for another DNA: the two nodes are not in the same \
                         session"
                            .to_string(),
                    );
                }
                if info.is_tombstone {
                    return Err("an agent info is a tombstone".into());
                }
                if info.expires_at <= now {
                    return Err("an agent info has expired".into());
                }
                Ok(info.url.as_ref().map(|url| url.as_str().to_string()))
            })
            .collect()
    }

    pub async fn add_agent_infos(&self, encoded: Vec<String>) -> Result<()> {
        self.conductor
            .add_agent_infos(encoded)
            .await
            .context("registering the peer's agent infos")
    }

    /// Transport stats for the connection to this endpoint, if one is open.
    pub async fn connection_to(
        &self,
        peer: &EndpointId,
    ) -> Option<kitsune2_api::TransportConnectionStats> {
        let stats = self.conductor.dump_network_stats().await.ok()?;
        let peer = peer.to_string();
        stats
            .transport_stats
            .connections
            .into_iter()
            .find(|c| c.pub_key == peer)
    }

    pub async fn print_transport(&self) {
        let stats = match self.conductor.dump_network_stats().await {
            Ok(stats) => stats.transport_stats,
            Err(err) => {
                warn!(?err, "Reading transport stats failed");
                return;
            }
        };
        println!(
            "transport {} at {}",
            stats.backend,
            stats
                .peer_urls
                .iter()
                .map(|url| url.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        if stats.connections.is_empty() {
            println!("          no peer connections");
        }
        for c in &stats.connections {
            println!(
                "          peer {} sent {} B in {} msgs, received {} B in {} msgs, direct IP path: {}",
                c.pub_key,
                c.send_bytes,
                c.send_message_count,
                c.recv_bytes,
                c.recv_message_count,
                c.is_direct,
            );
        }
    }
}

/// The body a note with this label must have. Labels end in the body size.
fn note_body(label: &str, bytes: usize) -> String {
    format!("{label}|")
        .bytes()
        .cycle()
        .take(bytes)
        .map(char::from)
        .collect()
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn digest(label: &str, body: &str) -> NoteDigest {
    NoteDigest {
        label: label.to_string(),
        bytes: body.len(),
        sha256: sha256_hex(body.as_bytes()),
    }
}

/// Whether a data note's body is exactly what its label says.
fn is_intact(note: &Note) -> bool {
    let Some(bytes) = note
        .label
        .rsplit('-')
        .next()
        .and_then(|size| size.parse::<usize>().ok())
    else {
        return false;
    };
    note.body == note_body(&note.label, bytes)
}

pub struct ExchangeOptions {
    pub notes: usize,
    pub note_bytes: usize,
    pub note_interval: Duration,
    pub timeout: Duration,
}

/// One direction of the exchange.
#[derive(Debug, Clone)]
pub struct Direction {
    pub from: String,
    pub to: String,
    pub passed: bool,
    pub detail: String,
}

pub struct Outcome {
    /// Peer to this node, from what this node read.
    pub inbound: Direction,
    /// This node to the peer, from the peer's receipt.
    pub outbound: Direction,
    /// When the transport first showed a connection to the peer.
    pub connected_after: Option<Duration>,
}

impl Outcome {
    pub fn passed(&self) -> bool {
        self.inbound.passed && self.outbound.passed
    }
}

/// A data note from the peer, as read here.
struct ReadNote {
    digest: NoteDigest,
    intact: bool,
    found_after: Duration,
    held_locally: bool,
}

/// Write this node's notes, read the peer's, and swap receipts.
///
/// `on_connected` runs once, when the transport first shows a connection to
/// the peer's endpoint.
pub async fn run_exchange(
    node: &Node,
    peer_name: &str,
    peer_endpoint: &EndpointId,
    options: &ExchangeOptions,
    mut on_connected: impl FnMut(),
) -> Outcome {
    let start = Instant::now();
    let writer = tokio::spawn(write_notes(
        node.clone(),
        options.notes,
        options.note_bytes,
        options.note_interval,
    ));
    let mut written: Option<Vec<NoteDigest>> = None;
    let mut write_error: Option<String> = None;

    let mut seen: HashSet<ActionHash> = HashSet::new();
    let mut peer_manifest: Option<Manifest> = None;
    let mut peer_receipt: Option<Manifest> = None;
    let mut peer_notes: BTreeMap<String, ReadNote> = BTreeMap::new();
    let mut receipt_written = false;
    let mut connected_after = None;
    let mut last_stats = Instant::now();
    let mut writer = Some(writer);

    loop {
        if connected_after.is_none() && node.connection_to(peer_endpoint).await.is_some() {
            connected_after = Some(start.elapsed());
            on_connected();
        }
        if let Some(handle) = writer.as_ref()
            && handle.is_finished()
        {
            match writer.take().expect("checked").await {
                Ok(Ok(digests)) => written = Some(digests),
                Ok(Err(err)) => write_error = Some(format!("{err:#}")),
                Err(err) => write_error = Some(err.to_string()),
            }
        }

        let mut lines = Vec::new();
        if let Err(err) = poll_notes(
            node,
            peer_name,
            start,
            &mut seen,
            &mut peer_manifest,
            &mut peer_receipt,
            &mut peer_notes,
            &mut lines,
        )
        .await
        {
            debug!(?err, "Polling notes failed");
        }
        // A note from the peer means a connection exists by now; say so
        // before reporting what came over it.
        if connected_after.is_none()
            && !lines.is_empty()
            && node.connection_to(peer_endpoint).await.is_some()
        {
            connected_after = Some(start.elapsed());
            on_connected();
        }
        for line in lines {
            println!("{line}");
        }

        let inbound_complete = peer_manifest
            .as_ref()
            .is_some_and(|m| m.notes.iter().all(|d| peer_notes.contains_key(&d.label)));
        if !receipt_written && inbound_complete && written.is_some() {
            let manifest = peer_manifest.as_ref().expect("checked");
            let receipt = Manifest {
                from: node.name.clone(),
                notes: manifest
                    .notes
                    .iter()
                    .filter_map(|d| peer_notes.get(&d.label).map(|r| r.digest.clone()))
                    .collect(),
            };
            match write_manifest(node, RECEIPT_PREFIX, &receipt).await {
                Ok(()) => {
                    receipt_written = true;
                    println!("receipt   written: read every note {peer_name} listed");
                }
                Err(err) => warn!(?err, "Writing the receipt failed; retrying"),
            }
        }

        if (receipt_written && peer_receipt.is_some()) || write_error.is_some() {
            break;
        }
        if start.elapsed() > options.timeout {
            break;
        }
        if last_stats.elapsed() >= STATS_INTERVAL {
            node.print_transport().await;
            last_stats = Instant::now();
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    if let Some(handle) = writer {
        handle.abort();
    }

    let inbound = judge_inbound(
        peer_name,
        &node.name,
        peer_manifest.as_ref(),
        &peer_notes,
        options.timeout,
    );
    let outbound = judge_outbound(
        &node.name,
        peer_name,
        written.as_deref(),
        write_error.as_deref(),
        peer_receipt.as_ref(),
    );
    Outcome {
        inbound,
        outbound,
        connected_after,
    }
}

/// Write the data notes, then the manifest; return the digests written.
async fn write_notes(
    node: Node,
    count: usize,
    bytes: usize,
    interval: Duration,
) -> Result<Vec<NoteDigest>> {
    let mut digests = Vec::with_capacity(count);
    for n in 0..count {
        if n > 0 {
            tokio::time::sleep(interval).await;
        }
        let label = format!("{}-{n}-{bytes}", node.name);
        let body = note_body(&label, bytes);
        digests.push(digest(&label, &body));
        let note = Note {
            body,
            label: label.clone(),
            origin: node.name.clone(),
        };
        let action_hash: ActionHash = node.call("create_note", note).await?;
        println!("wrote     {label} ({bytes} bytes) as {action_hash}");
    }
    write_manifest(
        &node,
        SENT_PREFIX,
        &Manifest {
            from: node.name.clone(),
            notes: digests.clone(),
        },
    )
    .await?;
    Ok(digests)
}

async fn write_manifest(node: &Node, prefix: &str, manifest: &Manifest) -> Result<()> {
    let note = Note {
        label: format!("{prefix}{}", node.name),
        origin: node.name.clone(),
        body: serde_json::to_string(manifest)?,
    };
    let _: ActionHash = node.call("create_note", note).await?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn poll_notes(
    node: &Node,
    peer_name: &str,
    start: Instant,
    seen: &mut HashSet<ActionHash>,
    peer_manifest: &mut Option<Manifest>,
    peer_receipt: &mut Option<Manifest>,
    peer_notes: &mut BTreeMap<String, ReadNote>,
    out: &mut Vec<String>,
) -> Result<()> {
    let local: HashSet<ActionHash> = node
        .call::<_, Vec<ActionHash>>("list_local_note_hashes", ())
        .await?
        .into_iter()
        .collect();
    let linked = node
        .call::<_, Vec<ActionHash>>("list_note_hashes", ())
        .await
        .unwrap_or_else(|err| {
            debug!(?err, "Listing notes from the network failed");
            Vec::new()
        });
    let unseen: HashSet<ActionHash> = local
        .iter()
        .cloned()
        .chain(linked)
        .filter(|hash| !seen.contains(hash))
        .collect();

    for action_hash in unseen {
        let record = match node
            .call::<_, Option<NoteRecord>>("get_note", action_hash.clone())
            .await
        {
            Ok(Some(record)) => record,
            Ok(None) => continue,
            Err(err) => {
                debug!(%action_hash, ?err, "Reading a note failed");
                continue;
            }
        };
        seen.insert(action_hash.clone());
        let note = record.note;
        if note.origin != peer_name {
            continue;
        }
        if let Some(rest) = note.label.strip_prefix(SENT_PREFIX) {
            if rest == peer_name {
                match serde_json::from_str::<Manifest>(&note.body) {
                    Ok(manifest) => {
                        out.push(format!(
                            "manifest  {peer_name} sent {} note(s)",
                            manifest.notes.len()
                        ));
                        *peer_manifest = Some(manifest);
                    }
                    Err(err) => warn!(?err, "Unreadable manifest from {peer_name}"),
                }
            }
            continue;
        }
        if let Some(rest) = note.label.strip_prefix(RECEIPT_PREFIX) {
            if rest == peer_name {
                match serde_json::from_str::<Manifest>(&note.body) {
                    Ok(receipt) => {
                        out.push(format!(
                            "receipt   {peer_name} confirms it read {} of our note(s)",
                            receipt.notes.len()
                        ));
                        *peer_receipt = Some(receipt);
                    }
                    Err(err) => warn!(?err, "Unreadable receipt from {peer_name}"),
                }
            }
            continue;
        }

        let read = ReadNote {
            intact: is_intact(&note),
            digest: digest(&note.label, &note.body),
            found_after: start.elapsed(),
            held_locally: local.contains(&action_hash),
        };
        out.push(format!(
            "read      {} ({} bytes) from {} after {:.1}s: {}, {}",
            note.label,
            read.digest.bytes,
            peer_name,
            read.found_after.as_secs_f64(),
            if read.intact {
                "every byte matches"
            } else {
                "CORRUPT"
            },
            if read.held_locally {
                "delivered by publish or gossip"
            } else {
                "fetched on demand"
            },
        ));
        peer_notes.insert(note.label.clone(), read);
    }
    Ok(())
}

fn judge_inbound(
    peer: &str,
    me: &str,
    manifest: Option<&Manifest>,
    read: &BTreeMap<String, ReadNote>,
    timeout: Duration,
) -> Direction {
    let fail = |detail: String| Direction {
        from: peer.into(),
        to: me.into(),
        passed: false,
        detail,
    };
    let Some(manifest) = manifest else {
        return fail(format!(
            "no manifest from {peer} within {}s ({} data note(s) read)",
            timeout.as_secs(),
            read.len()
        ));
    };
    let mut bytes = 0;
    for listed in &manifest.notes {
        let Some(note) = read.get(&listed.label) else {
            return fail(format!(
                "{} of {} note(s) read within {}s; missing {}",
                manifest
                    .notes
                    .iter()
                    .filter(|d| read.contains_key(&d.label))
                    .count(),
                manifest.notes.len(),
                timeout.as_secs(),
                listed.label
            ));
        };
        if !note.intact || note.digest != *listed {
            return fail(format!("{} arrived CORRUPT", listed.label));
        }
        bytes += listed.bytes;
    }
    Direction {
        from: peer.into(),
        to: me.into(),
        passed: true,
        detail: format!(
            "{} note(s), {bytes} bytes, every byte matched (read here)",
            manifest.notes.len()
        ),
    }
}

fn judge_outbound(
    me: &str,
    peer: &str,
    written: Option<&[NoteDigest]>,
    write_error: Option<&str>,
    receipt: Option<&Manifest>,
) -> Direction {
    let fail = |detail: String| Direction {
        from: me.into(),
        to: peer.into(),
        passed: false,
        detail,
    };
    if let Some(err) = write_error {
        return fail(format!("writing our notes failed: {err}"));
    }
    let Some(written) = written else {
        return fail("our notes were not all written in time".into());
    };
    let Some(receipt) = receipt else {
        return fail(format!(
            "no receipt from {peer}: it has not confirmed reading our notes"
        ));
    };
    for ours in written {
        match receipt.notes.iter().find(|d| d.label == ours.label) {
            None => return fail(format!("{peer}'s receipt does not list {}", ours.label)),
            Some(theirs) if theirs != ours => {
                return fail(format!(
                    "{peer} read {} with SHA-256 {} instead of {}",
                    ours.label, theirs.sha256, ours.sha256
                ));
            }
            Some(_) => {}
        }
    }
    let bytes: usize = written.iter().map(|d| d.bytes).sum();
    Direction {
        from: me.into(),
        to: peer.into(),
        passed: true,
        detail: format!(
            "{} note(s), {bytes} bytes, every byte matched ({peer}'s receipt)",
            written.len()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_bodies_encode_their_label_and_size() {
        let body = note_body("windows-0-10", 10);
        assert_eq!(body, "windows-0-");
        assert!(is_intact(&Note {
            label: "windows-0-10".into(),
            origin: "windows".into(),
            body,
        }));
        assert!(!is_intact(&Note {
            label: "windows-0-10".into(),
            origin: "windows".into(),
            body: "windows-0".into(),
        }));
    }

    fn read_note(label: &str, bytes: usize) -> ReadNote {
        let body = note_body(label, bytes);
        ReadNote {
            intact: true,
            digest: digest(label, &body),
            found_after: Duration::ZERO,
            held_locally: true,
        }
    }

    #[test]
    fn inbound_passes_only_with_every_listed_note_intact() {
        let notes = vec![read_note("w-0-100", 100), read_note("w-1-100", 100)];
        let manifest = Manifest {
            from: "w".into(),
            notes: notes.iter().map(|n| n.digest.clone()).collect(),
        };
        let mut read: BTreeMap<String, ReadNote> = notes
            .into_iter()
            .map(|n| (n.digest.label.clone(), n))
            .collect();
        let t = Duration::from_secs(1);
        assert!(judge_inbound("w", "u", Some(&manifest), &read, t).passed);

        read.get_mut("w-1-100").unwrap().intact = false;
        let failed = judge_inbound("w", "u", Some(&manifest), &read, t);
        assert!(
            !failed.passed && failed.detail.contains("CORRUPT"),
            "{failed:?}"
        );

        read.remove("w-1-100");
        let missing = judge_inbound("w", "u", Some(&manifest), &read, t);
        assert!(
            !missing.passed && missing.detail.contains("missing w-1-100"),
            "{missing:?}"
        );
        assert!(!judge_inbound("w", "u", None, &read, t).passed);
    }

    #[test]
    fn outbound_passes_only_when_the_receipt_matches_what_was_written() {
        let written = vec![digest("u-0-5", &note_body("u-0-5", 5))];
        let good = Manifest {
            from: "w".into(),
            notes: written.clone(),
        };
        assert!(judge_outbound("u", "w", Some(&written), None, Some(&good)).passed);

        let mut tampered = good.clone();
        tampered.notes[0].sha256 = "00".repeat(32);
        assert!(!judge_outbound("u", "w", Some(&written), None, Some(&tampered)).passed);

        let empty = Manifest {
            from: "w".into(),
            notes: vec![],
        };
        assert!(!judge_outbound("u", "w", Some(&written), None, Some(&empty)).passed);
        assert!(!judge_outbound("u", "w", Some(&written), None, None).passed);
        assert!(!judge_outbound("u", "w", None, Some("boom"), Some(&good)).passed);
    }
}
