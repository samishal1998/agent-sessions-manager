//! jcode over the hub: backed up, not yet restorable.
//!
//! jcode rewrites its whole snapshot every turn, so no copy is ever a prefix
//! of another. The identity is the snapshot's content with `last_pid`
//! removed — that field is a process id on whichever machine last opened it.

use serde_json::Value;

use super::{JCodeAdapter, write};
use crate::hub::bundle::{Bundle, Staged};
use crate::model::Session;
use crate::{CoreError, fsutil};

pub(crate) fn collect(_adapter: &JCodeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let mut files = Vec::new();
    let mut canonical = None;
    for (name, path) in write::session_files(session)? {
        // Read once and upload exactly what was read: a snapshot rewritten
        // between hashing and uploading would otherwise fail its checksum.
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && name != "snapshot.json" => continue,
            Err(e) => return Err(CoreError::io(&path, e)),
        };
        if name == "snapshot.json" {
            let mut value: Value = serde_json::from_slice(&bytes).map_err(|e| CoreError::Invalid {
                msg: format!("{} is not readable JSON yet: {e}", path.display()),
            })?;
            if let Some(object) = value.as_object_mut() {
                object.remove("last_pid");
            }
            // serde_json's map is ordered by key, so this is stable however
            // jcode laid the file out.
            canonical = Some(fsutil::sha256_hex(&serde_json::to_vec(&value).unwrap()));
        }
        files.push(Staged::bytes(&name, bytes));
    }
    let canonical = canonical
        .ok_or_else(|| CoreError::Invalid { msg: "jcode session has no snapshot".into() })?;
    Ok(Bundle { files, canonical, extra: Value::Null })
}
