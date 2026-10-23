//! Entries the two-machine BLE test writes on one conductor and reads back on
//! the other.

use hdi::prelude::*;

/// Largest note body accepted. Big enough to need many BLE round trips,
/// small enough to move within a test run over GATT.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// A note written by one test node for the other to find.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Note {
    /// Unique label, `<node name>-<n>`, so each node can tell its own notes
    /// from the other's.
    pub label: String,
    /// Name of the node that wrote the note.
    pub origin: String,
    /// Payload filled with a pattern derived from the label, so the reader can
    /// check that every byte arrived intact.
    pub body: String,
}

#[hdk_entry_types]
#[unit_enum(UnitEntryTypes)]
pub enum EntryTypes {
    Note(Note),
}

#[hdk_link_types]
pub enum LinkTypes {
    /// Base = the `all_notes` path, target = a `Note` create action. Lets each
    /// node discover the notes the other wrote.
    AllNotes,
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::CreateEntry(OpEntry::CreateEntry {
            app_entry: EntryTypes::Note(note),
            ..
        }) => Ok(validate_note(&note)),
        FlatOp::Update(_) => Ok(ValidateCallbackResult::Invalid(
            "Notes can not be updated".to_string(),
        )),
        _ => Ok(ValidateCallbackResult::Valid),
    }
}

fn validate_note(note: &Note) -> ValidateCallbackResult {
    if note.label.is_empty() {
        return ValidateCallbackResult::Invalid("A note needs a label".to_string());
    }
    if note.body.len() > MAX_BODY_BYTES {
        return ValidateCallbackResult::Invalid(format!(
            "A note body can be at most {MAX_BODY_BYTES} bytes"
        ));
    }
    ValidateCallbackResult::Valid
}
