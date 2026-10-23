//! Write notes and find the ones other agents wrote, through the DHT.

use hdk::prelude::*;
use probe_integrity::*;

/// A note as read back from the DHT.
#[derive(Serialize, Deserialize, Debug)]
pub struct NoteRecord {
    pub action_hash: ActionHash,
    pub author: AgentPubKey,
    pub note: Note,
}

fn all_notes_base() -> ExternResult<EntryHash> {
    Path::from("all_notes").path_entry_hash()
}

/// Commit a note and link it from the shared `all_notes` path.
#[hdk_extern]
pub fn create_note(note: Note) -> ExternResult<ActionHash> {
    let action_hash = create_entry(EntryTypes::Note(note))?;
    create_link(
        all_notes_base()?,
        action_hash.clone(),
        LinkTypes::AllNotes,
        (),
    )?;
    Ok(action_hash)
}

/// Notes linked from `all_notes`, asking the network for links this node
/// does not hold yet.
#[hdk_extern]
pub fn list_note_hashes(_: ()) -> ExternResult<Vec<ActionHash>> {
    note_hashes(GetStrategy::Network)
}

/// Notes linked from `all_notes` that this node already holds, without asking
/// the network. Links only show up here once publish or gossip delivered them.
#[hdk_extern]
pub fn list_local_note_hashes(_: ()) -> ExternResult<Vec<ActionHash>> {
    note_hashes(GetStrategy::Local)
}

fn note_hashes(strategy: GetStrategy) -> ExternResult<Vec<ActionHash>> {
    let links = get_links(
        LinkQuery::try_new(all_notes_base()?, LinkTypes::AllNotes)?,
        strategy,
    )?;
    Ok(links
        .into_iter()
        .filter_map(|link| link.target.into_action_hash())
        .collect())
}

/// Read a note, from the network if this node does not hold it.
#[hdk_extern]
pub fn get_note(action_hash: ActionHash) -> ExternResult<Option<NoteRecord>> {
    let Some(record) = get(action_hash, GetOptions::network())? else {
        return Ok(None);
    };
    let note: Option<Note> = record
        .entry()
        .to_app_option()
        .map_err(|err| wasm_error!(err))?;
    Ok(note.map(|note| NoteRecord {
        action_hash: record.action_address().clone(),
        author: record.action().author().clone(),
        note,
    }))
}
