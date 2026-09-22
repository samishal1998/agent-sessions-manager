//! Claude Code over the hub: what a session uploads as, and how another
//! machine installs it under its original id.
//!
//! The transcript is append-only, which is what makes a cross-machine
//! update safe: if this machine's copy is a byte-prefix of the hub's, the
//! difference can be appended and the result is exactly the hub's copy,
//! without rewriting a byte background jobs may hold an offset into.
//!
//! One thing in the file is not shared history: `relocated` records. asm
//! writes one when it installs a session at a different path, and Claude
//! Code writes one itself when resumed from a directory other than the last
//! record's (verified against 2.1.278). They are this machine's bookkeeping
//! about where the session lives, they appear mid-file, and they carry no
//! timestamp — so they are removed from what is uploaded and compared, and
//! re-derived for this machine on install.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{ClaudeAdapter, store, write};
use crate::hub::bundle::{self, Base, Bundle, InstallOutcome, Installed, Staged};
use crate::hub::manifest::{FileEntry, Manifest};
use crate::model::{Session, SessionLocation};
use crate::{CoreError, fsutil};

/// Root-level sidecars keyed by session id, as `write::uuid_sidecars` has it.
const ROOT_SIDECARS: [&str; 3] = ["file-history", "session-env", "tasks"];

fn is_relocated(line: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\"relocated\"";
    line.windows(NEEDLE.len()).any(|w| w == NEEDLE)
        && serde_json::from_slice::<Value>(line)
            .is_ok_and(|v| v.get("type").and_then(Value::as_str) == Some("relocated"))
}

/// The transcript as another machine should see it: complete lines only,
/// without this machine's `relocated` records.
pub(crate) fn canonical_transcript(raw: &[u8]) -> Vec<u8> {
    let complete = bundle::complete_lines(raw);
    let mut out = Vec::with_capacity(complete.len());
    for line in complete.split_inclusive(|&b| b == b'\n') {
        if !is_relocated(line) {
            out.extend_from_slice(line);
        }
    }
    out
}

fn transcript_of(session: &Session) -> Result<&Path, CoreError> {
    match &session.handle.location {
        SessionLocation::JsonlFile { path } => Ok(path),
        _ => Err(CoreError::Invalid { msg: "session has no transcript file".into() }),
    }
}

pub(crate) fn collect(adapter: &ClaudeAdapter, session: &Session) -> Result<Bundle, CoreError> {
    let id = &session.handle.native_id;
    let transcript = transcript_of(session)?;
    let raw = fs::read(transcript).map_err(|e| CoreError::io(transcript, e))?;
    let canonical_bytes = canonical_transcript(&raw);
    let canonical = fsutil::sha256_hex(&canonical_bytes);

    let mut files = vec![Staged::bytes("transcript.jsonl", canonical_bytes)];
    let sidecar = transcript.parent().map(|p| p.join(id)).unwrap_or_default();
    files.extend(bundle::walk(&sidecar, "session-dir")?);
    for dir in ROOT_SIDECARS {
        files.extend(bundle::walk(&adapter.root().join(dir).join(id), dir)?);
    }
    Ok(Bundle {
        files,
        canonical,
        // Where the sidecar lived on the pushing machine, so symlinks into
        // it can be repointed on the receiving one.
        extra: json!({ "sidecar_dir": sidecar.display().to_string() }),
    })
}

/// Every copy of this id in any project directory. More than one already
/// breaks Claude Code's cross-project resume for it.
fn find_transcripts(root: &Path, id: &str) -> Vec<PathBuf> {
    let Ok(projects) = fs::read_dir(root.join("projects")) else { return Vec::new() };
    let mut found: Vec<PathBuf> = projects
        .flatten()
        .map(|p| p.path().join(format!("{id}.jsonl")))
        .filter(|p| p.is_file())
        .collect();
    found.sort();
    found
}

fn effective_cwd(transcript: &Path, id: &str) -> Option<PathBuf> {
    store::scan_transcript(transcript, id).map(|s| s.project_root)
}

/// Append one `relocated` record if this machine would otherwise read the
/// session as living somewhere else — which it does whenever the last
/// records came from another machine's path, or a large append pushed an
/// earlier marker out of the window `scan_transcript` reads.
fn ensure_cwd(transcript: &Path, id: &str, want: &str) -> Result<(), CoreError> {
    if !Path::new(want).is_absolute() {
        return Err(CoreError::Invalid { msg: format!("refusing to relocate {id} to {want:?}") });
    }
    if effective_cwd(transcript, id).as_deref() != Some(Path::new(want)) {
        fsutil::append_jsonl_line(transcript, &write::relocated_marker(id, want))?;
    }
    Ok(())
}

fn utf8(path: &Path) -> Result<&str, CoreError> {
    path.to_str().ok_or_else(|| CoreError::Invalid { msg: format!("{} is not UTF-8", path.display()) })
}

fn private_dir(dir: &Path) -> Result<(), CoreError> {
    fs::create_dir_all(dir).map_err(|e| CoreError::io(dir, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Where a sidecar entry lands, and the session's own directory it must
/// stay inside. `validate` has already refused `..` and absolute paths; the
/// `starts_with` is there so that a bug elsewhere cannot quietly turn into a
/// write outside that directory.
fn sidecar_dest(root: &Path, project_dir: &Path, id: &str, path: &str) -> Option<(PathBuf, PathBuf)> {
    let (top, rest) = path.split_once('/')?;
    let base = match top {
        "session-dir" => project_dir.join(id),
        dir if ROOT_SIDECARS.contains(&dir) => root.join(dir).join(id),
        _ => return None,
    };
    let dest = base.join(rest);
    dest.starts_with(&base).then_some((base, dest))
}

/// Create `dir` one level at a time from `base`'s parent, refusing to pass
/// through anything that is not a real directory. A symlink planted by an
/// earlier pull, or by a hostile manifest, must never become a way to write
/// outside the session's own directories.
fn real_dirs(base: &Path, dir: &Path) -> Result<(), CoreError> {
    let trusted = base.parent().unwrap_or(base);
    fs::create_dir_all(trusted).map_err(|e| CoreError::io(trusted, e))?;
    let mut at = trusted.to_path_buf();
    for part in dir.strip_prefix(trusted).unwrap_or(Path::new("")).components() {
        at.push(part);
        match fs::symlink_metadata(&at) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(CoreError::Invalid {
                    msg: format!("{} is not a directory; refusing to write through it", at.display()),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&at).map_err(|e| CoreError::io(&at, e))?;
            }
            Err(e) => return Err(CoreError::io(&at, e)),
        }
    }
    Ok(())
}

/// A link is installed only if it stays inside this session's own
/// directories: a relative target that only descends, or an absolute one
/// into the session's sidecar (repointed from the pushing machine's).
/// Anything else named a place on the other machine's disk, which means
/// nothing here — and would be a way out of the store if it were followed.
fn contained_target(target: &str, source: Option<&Path>, local: &Path, dest: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let target = Path::new(target);
    if target.components().all(|c| matches!(c, Component::Normal(_))) {
        return Some(target.to_path_buf());
    }
    let target = match source.and_then(|src| target.strip_prefix(src).ok()) {
        Some(rest) => local.join(rest),
        None => target.to_path_buf(),
    };
    let plain = target.components().all(|c| matches!(c, Component::Normal(_) | Component::RootDir));
    // Nor a directory the link itself sits in: that is a loop to anything
    // that walks the sidecar.
    let above = dest.starts_with(&target);
    (plain && !above && target.is_absolute() && target.starts_with(local)).then_some(target)
}

/// True when `have` is an earlier state of `want`: its bytes a proper
/// prefix of `want`'s, as an appended-to JSONL sidecar's are.
// ponytail: reads both files whole; stream the comparison if sidecars
// (file-history keeps copies of edited files) ever get large enough to matter.
fn behind(have: &Path, want: &Path) -> bool {
    match (fs::read(have), fs::read(want)) {
        (Ok(have), Ok(want)) => want.len() > have.len() && want.starts_with(&have),
        _ => false,
    }
}

/// Write the hub's sidecar files. `newer` is true only when the hub's
/// transcript extends this machine's; even then a local sidecar is replaced
/// only when this machine has not changed it since it last synced (it is
/// what that revision had), or it is an earlier state of the hub's — so
/// nothing written here and not yet pushed is lost, and a file the other
/// machine rewrote whole (tasks) still arrives.
fn write_sidecars(
    root: &Path,
    project_dir: &Path,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    newer: bool,
    synced: Option<&Base>,
) -> Result<(), CoreError> {
    let local_sidecar = project_dir.join(&manifest.id);
    let source_sidecar =
        manifest.extra.get("sidecar_dir").and_then(Value::as_str).map(PathBuf::from);
    for file in &manifest.files {
        if file.path == "transcript.jsonl" {
            continue;
        }
        let Some((base, dest)) = sidecar_dest(root, project_dir, &manifest.id, &file.path) else {
            return Err(CoreError::Invalid { msg: format!("unexpected entry {:?}", file.path) });
        };
        real_dirs(&base, dest.parent().unwrap_or(&base))?;
        let existing = fs::symlink_metadata(&dest).ok();
        match file {
            FileEntry { symlink: Some(target), .. } => {
                let Some(target) =
                    contained_target(target, source_sidecar.as_deref(), &local_sidecar, &dest)
                else {
                    continue;
                };
                match existing {
                    None => {}
                    // Only a link replaces a link; a real file here is data.
                    Some(meta) if meta.is_symlink() && newer => {
                        fs::remove_file(&dest).map_err(|e| CoreError::io(&dest, e))?;
                    }
                    Some(_) => continue,
                }
                #[cfg(unix)]
                std::os::unix::fs::symlink(&target, &dest).map_err(|e| CoreError::io(&dest, e))?;
            }
            FileEntry { sha256: Some(sha), .. } => {
                let src = blob(sha)?;
                // An existing file is replaced only on a fast-forward and only
                // by a later state of itself. A link there is never followed,
                // not even to read it.
                let unchanged_here = || {
                    synced.and_then(|b| b.files.get(&file.path))
                        .is_some_and(|synced| fsutil::sha256_file(&dest).is_ok_and(|have| have == *synced))
                };
                let replace = match existing {
                    None => true,
                    Some(meta) => newer && meta.is_file() && (unchanged_here() || behind(&dest, &src)),
                };
                if replace {
                    fsutil::copy_atomic(&src, &dest)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Held while one pull installs one id, so two pulls at once cannot both see
/// no copy and each file one, or both append the same delta. An flock, so
/// the kernel releases it however the process ends; the file is left in
/// place, since unlinking a lock someone else holds open lets a third
/// process lock a new file beside it.
struct InstallLock(#[allow(dead_code)] fs::File);

impl InstallLock {
    fn take(root: &Path, id: &str) -> Result<InstallLock, CoreError> {
        let projects = root.join("projects");
        private_dir(&projects)?;
        let path = projects.join(format!(".asm-install-{id}.lock"));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| CoreError::io(&path, e))?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // Safety: flock on a descriptor this function owns.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(CoreError::Invalid {
                    msg: format!("another asm pull of session {id} is running"),
                });
            }
        }
        Ok(InstallLock(file))
    }
}

/// Install a pulled session on this machine, under its original id.
///
/// New here: placed under `project_dir` (else the pushing machine's path
/// resolved against this home), with one `relocated` record if that differs
/// from where its records say it ran. Already here: appended to when this
/// copy is a prefix of the hub's, left alone when it is ahead or has
/// diverged. Never rewrites existing transcript bytes, never makes a second
/// copy of the id.
pub(crate) fn install(
    adapter: &ClaudeAdapter,
    manifest: &Manifest,
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<Installed, CoreError> {
    manifest.validate().map_err(|msg| CoreError::Invalid { msg })?;
    let id = &manifest.id;
    // Every download first (`blob` keeps what it fetched), so nothing below
    // waits on the network between the liveness check and the writes.
    for sha in manifest.blob_shas() {
        blob(sha)?;
    }
    let root = adapter.root();
    let _lock = InstallLock::take(root, id)?;
    write::guard_not_live(adapter, id)?;

    let entry = manifest
        .files
        .iter()
        .find(|f| f.path == "transcript.jsonl")
        .and_then(|f| f.sha256.as_deref())
        .ok_or_else(|| CoreError::Invalid { msg: "bundle has no transcript".into() })?;
    let src = blob(entry)?;
    // Canonicalized again: a machine running another version could have
    // uploaded markers, and they must never be installed as history.
    let incoming = canonical_transcript(&fs::read(&src).map_err(|e| CoreError::io(&src, e))?);

    let existing = find_transcripts(root, id);
    match existing.as_slice() {
        [] => install_new(adapter, manifest, &incoming, blob, project_dir),
        [path] => update(adapter, manifest, path, &incoming, blob, project_dir, base),
        many => Err(CoreError::Invalid {
            msg: format!(
                "session {id} already exists in {} project directories here, which breaks \
                 `claude --resume` for it; resolve that first (asm doctor lists them)",
                many.len()
            ),
        }),
    }
}

fn install_new(
    adapter: &ClaudeAdapter,
    manifest: &Manifest,
    incoming: &[u8],
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
) -> Result<Installed, CoreError> {
    let id = &manifest.id;
    let target = bundle::target_dir(manifest, project_dir)?;
    let target_str = utf8(&target)?;

    let root = adapter.root();
    let dest_dir = root.join("projects").join(super::encode_project_dir(target_str));
    private_dir(&root.join("projects"))?;
    private_dir(&dest_dir)?;
    let dest = dest_dir.join(format!("{id}.jsonl"));

    // Sidecars first and the transcript last: until the transcript exists
    // the session does not, so an interrupted install leaves nothing that
    // looks like a session.
    write_sidecars(root, &dest_dir, manifest, blob, true, None)?;
    fsutil::write_atomic(&dest, incoming)?;
    ensure_cwd(&dest, id, target_str)?;
    finish(InstallOutcome::New, dest, id)
}

fn update(
    adapter: &ClaudeAdapter,
    manifest: &Manifest,
    path: &Path,
    incoming: &[u8],
    blob: &dyn Fn(&str) -> Result<PathBuf, CoreError>,
    project_dir: Option<&Path>,
    base: Option<&Base>,
) -> Result<Installed, CoreError> {
    let id = &manifest.id;
    let project = path.parent().unwrap_or(Path::new("/"));
    let here = match effective_cwd(path, id) {
        Some(here) => {
            if let Some(dir) = project_dir {
                let dir = dir.canonicalize().map_err(|e| CoreError::io(dir, e))?;
                if dir != here {
                    return Err(CoreError::Invalid {
                        msg: format!(
                            "session {id} is already here, in {}; pull without --project-dir \
                             to update it there, or `asm move` it first",
                            here.display()
                        ),
                    });
                }
            }
            here
        }
        // No record here says where it ran yet. Take the place a new install
        // would, but only if that is the project directory it is filed in.
        None => {
            let target = bundle::target_dir(manifest, project_dir)?;
            let filed = project.file_name().and_then(|n| n.to_str());
            if filed != Some(super::encode_project_dir(utf8(&target)?).as_str()) {
                return Err(CoreError::Invalid {
                    msg: format!(
                        "cannot tell which directory session {id} belongs to here; pass \
                         --project-dir with the one {} stands for",
                        project.display()
                    ),
                });
            }
            target
        }
    };

    let raw = fs::read(path).map_err(|e| CoreError::io(path, e))?;
    if raw.last().is_some_and(|&b| b != b'\n') {
        return Err(CoreError::Invalid {
            msg: format!(
                "{} ends in an unfinished line, so appending to it would join two records; \
                 resume the session once so Claude Code finishes it",
                path.display()
            ),
        });
    }
    let local = canonical_transcript(&raw);

    let outcome = if local == incoming {
        InstallOutcome::InSync
    } else if incoming.starts_with(&local) {
        let delta = &incoming[local.len()..];
        // One write, so a reader never sees half of the new records.
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(|e| CoreError::io(path, e))?;
        file.write_all(delta).map_err(|e| CoreError::io(path, e))?;
        InstallOutcome::FastForward { appended: delta.len() as u64 }
    } else if local.starts_with(incoming) {
        InstallOutcome::Ahead
    } else {
        return finish(InstallOutcome::Diverged, path.to_path_buf(), id);
    };

    // The marker before the sidecars: if a sidecar fails, the appended
    // foreign records must not already be moving the session elsewhere.
    ensure_cwd(path, id, utf8(&here)?)?;
    let newer = matches!(outcome, InstallOutcome::FastForward { .. });
    write_sidecars(adapter.root(), project, manifest, blob, newer, base)?;
    finish(outcome, path.to_path_buf(), id)
}

fn finish(outcome: InstallOutcome, path: PathBuf, id: &str) -> Result<Installed, CoreError> {
    let project_root = effective_cwd(&path, id).unwrap_or_default();
    Ok(Installed { outcome, project_root, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hub::manifest::SCHEMA;
    use std::collections::HashMap;

    #[test]
    fn relocated_records_and_torn_tails_are_not_history() {
        let raw = concat!(
            "{\"type\":\"user\",\"cwd\":\"/a\"}\n",
            "{\"type\":\"relocated\",\"sessionId\":\"x\",\"relocatedCwd\":\"/b\"}\n",
            "{\"type\":\"assistant\",\"cwd\":\"/a\"}\n",
            "{\"type\":\"user\",\"cw",
        );
        assert_eq!(
            canonical_transcript(raw.as_bytes()),
            b"{\"type\":\"user\",\"cwd\":\"/a\"}\n{\"type\":\"assistant\",\"cwd\":\"/a\"}\n"
        );
    }

    /// A record that merely mentions the word is conversation, not a marker.
    #[test]
    fn a_message_about_relocation_is_kept() {
        let raw = b"{\"type\":\"user\",\"message\":{\"content\":\"why \\\"relocated\\\"?\"}}\n";
        assert_eq!(canonical_transcript(raw), raw);
    }

    const ID: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

    /// One simulated machine: a Claude store and a project directory, both
    /// under a tempdir so two of them stand in for two computers.
    struct Machine {
        _dir: tempfile::TempDir,
        adapter: ClaudeAdapter,
        project: PathBuf,
    }

    impl Machine {
        fn new(project_name: &str) -> Machine {
            let dir = tempfile::tempdir().unwrap();
            let project = dir.path().join(project_name);
            fs::create_dir_all(&project).unwrap();
            let project = project.canonicalize().unwrap();
            let adapter = ClaudeAdapter::with_root(dir.path().join("claude"));
            fs::create_dir_all(adapter.root().join("projects")).unwrap();
            Machine { _dir: dir, adapter, project }
        }

        fn project_dir(&self) -> PathBuf {
            self.adapter
                .root()
                .join("projects")
                .join(super::super::encode_project_dir(self.project.to_str().unwrap()))
        }

        fn transcript(&self) -> PathBuf {
            self.project_dir().join(format!("{ID}.jsonl"))
        }

        /// A real-shaped conversation record from this machine.
        fn record(&self, text: &str) -> String {
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"sessionId\":\"{ID}\",\"timestamp\":\"2026-09-22T10:00:00Z\",\"message\":{{\"role\":\"user\",\"content\":\"{text}\"}}}}\n",
                self.project.display()
            )
        }

        fn start(&self, texts: &[&str]) {
            fs::create_dir_all(self.project_dir()).unwrap();
            let body: String = texts.iter().map(|t| self.record(t)).collect();
            fs::write(self.transcript(), body).unwrap();
        }

        fn append(&self, text: &str) {
            let mut f = fs::OpenOptions::new().append(true).open(self.transcript()).unwrap();
            f.write_all(self.record(text).as_bytes()).unwrap();
        }

        fn session(&self) -> Session {
            let path = super::super::hub::find_transcripts(self.adapter.root(), ID).pop().unwrap();
            store::scan_transcript(&path, ID).unwrap()
        }

        fn project_root(&self) -> PathBuf {
            self.session().project_root
        }
    }

    /// What a push would upload, as a manifest plus a blob lookup — the hub
    /// in between is tested on its own.
    fn push(from: &Machine) -> (Manifest, HashMap<String, Vec<u8>>) {
        let session = from.session();
        let bundle = collect(&from.adapter, &session).unwrap();
        let mut blobs = HashMap::new();
        for f in &bundle.files {
            if let (Some(sha), bundle_source) = (&f.entry.sha256, &f.source) {
                let bytes = match bundle_source {
                    bundle::Source::Bytes(b) => b.clone(),
                    bundle::Source::Path(p) => fs::read(p).unwrap(),
                    bundle::Source::Symlink => continue,
                };
                blobs.insert(sha.clone(), bytes);
            }
        }
        let manifest = Manifest {
            schema: SCHEMA,
            agent: crate::model::AgentKind::ClaudeCode,
            id: ID.into(),
            title: None,
            slug: None,
            project_root: session.project_root.display().to_string(),
            project_root_portable: session.project_root.display().to_string(),
            git_origin: None,
            git_branch: None,
            agent_version: None,
            created: None,
            updated: None,
            machine: None,
            pushed_at: None,
            canonical: bundle.canonical,
            parent_rev: None,
            files: bundle.files.iter().map(|f| f.entry.clone()).collect(),
            extra: bundle.extra,
        };
        (manifest, blobs)
    }

    fn pull(into: &Machine, pushed: &(Manifest, HashMap<String, Vec<u8>>), dir: Option<&Path>)
        -> Result<Installed, CoreError>
    {
        let scratch = tempfile::tempdir().unwrap();
        let blob = |sha: &str| -> Result<PathBuf, CoreError> {
            let path = scratch.path().join(sha);
            fs::write(&path, &pushed.1[sha]).unwrap();
            Ok(path)
        };
        install(&into.adapter, &pushed.0, &blob, dir, None)
    }

    fn transcript_of_b(b: &Machine) -> Vec<u8> {
        fs::read(find_transcripts(b.adapter.root(), ID).pop().unwrap()).unwrap()
    }

    #[test]
    fn a_new_install_elsewhere_lands_under_its_own_id_and_path() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("work/proj-b"));
        a.start(&["remember PELICAN", "second"]);
        let pushed = push(&a);

        let installed = pull(&b, &pushed, Some(&b.project)).unwrap();
        assert_eq!(installed.outcome, InstallOutcome::New);
        assert_eq!(installed.path, b.transcript(), "filed under B's own project directory");
        // The records still say machine A; the marker is what makes B read
        // it as its own.
        assert_eq!(b.project_root(), b.project);
        assert_eq!(canonical_transcript(&transcript_of_b(&b)), fs::read(a.transcript()).unwrap());
    }

    #[test]
    fn a_later_push_fast_forwards_and_stays_where_it_was_put() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        pull(&b, &push(&a), Some(&b.project)).unwrap();
        let before = transcript_of_b(&b);

        a.append("two");
        a.append("three");
        let installed = pull(&b, &push(&a), None).unwrap();
        assert!(matches!(installed.outcome, InstallOutcome::FastForward { appended } if appended > 0));
        let after = transcript_of_b(&b);
        assert!(after.starts_with(&before), "only appended, never rewritten");
        assert_eq!(canonical_transcript(&after), fs::read(a.transcript()).unwrap());
        assert_eq!(b.project_root(), b.project, "foreign cwds after the marker do not move it");
        assert_eq!(find_transcripts(b.adapter.root(), ID).len(), 1, "never a second copy");
    }

    /// asm reads location from the last 256 KiB. A big fast-forward pushes
    /// the first marker out of that window; the install must notice and
    /// write another, or the session silently moves back to machine A's path.
    #[test]
    fn a_marker_pushed_out_of_the_tail_window_is_rewritten() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        pull(&b, &push(&a), Some(&b.project)).unwrap();
        let filler = "x".repeat(4000);
        for _ in 0..100 {
            a.append(&filler);
        }
        pull(&b, &push(&a), None).unwrap();
        assert_eq!(b.project_root(), b.project);
    }

    #[test]
    fn nothing_changes_when_this_machine_is_ahead_or_both_diverged() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let first = push(&a);
        pull(&b, &first, Some(&b.project)).unwrap();

        // B continued; A's old copy arriving again changes nothing.
        let path = find_transcripts(b.adapter.root(), ID).pop().unwrap();
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b.record("b continued").as_bytes()).unwrap();
        let ahead = transcript_of_b(&b);
        assert_eq!(pull(&b, &first, None).unwrap().outcome, InstallOutcome::Ahead);
        assert_eq!(transcript_of_b(&b), ahead);

        // A continued too, differently: neither is a prefix of the other.
        a.append("a continued");
        assert_eq!(pull(&b, &push(&a), None).unwrap().outcome, InstallOutcome::Diverged);
        assert_eq!(transcript_of_b(&b), ahead, "a divergence touches nothing");
    }

    #[test]
    fn sidecars_arrive_and_symlinks_into_them_are_repointed() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let sidecar = a.project_dir().join(ID);
        fs::create_dir_all(sidecar.join("subagents")).unwrap();
        fs::write(sidecar.join("subagents/agent-1.jsonl"), b"{}\n").unwrap();
        fs::create_dir_all(a.adapter.root().join("file-history").join(ID)).unwrap();
        fs::write(a.adapter.root().join("file-history").join(ID).join("f@v1"), b"old").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(sidecar.join("subagents/agent-1.jsonl"), sidecar.join("out"))
            .unwrap();

        pull(&b, &push(&a), Some(&b.project)).unwrap();
        let b_sidecar = b.project_dir().join(ID);
        assert_eq!(fs::read(b_sidecar.join("subagents/agent-1.jsonl")).unwrap(), b"{}\n");
        assert_eq!(fs::read(b.adapter.root().join("file-history").join(ID).join("f@v1")).unwrap(), b"old");
        #[cfg(unix)]
        assert_eq!(
            fs::read_link(b_sidecar.join("out")).unwrap(),
            b_sidecar.join("subagents/agent-1.jsonl"),
            "repointed into B's sidecar, not left pointing at machine A's"
        );
    }

    #[test]
    fn the_refusals() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let mut pushed = push(&a);

        // Nowhere to put it: A's path does not exist here, and none was
        // given. (Both "machines" share this host, so point A's path at
        // somewhere that really is absent.)
        pushed.0.project_root_portable = "/nonexistent/asm-test/proj-a".into();
        let err = pull(&b, &pushed, None).unwrap_err().to_string();
        assert!(err.contains("--project-dir"), "{err}");

        pull(&b, &pushed, Some(&b.project)).unwrap();
        // Already here, somewhere else than asked.
        let other = b.project.parent().unwrap().join("other");
        fs::create_dir_all(&other).unwrap();
        assert!(pull(&b, &pushed, Some(&other)).is_err());

        // A torn tail: appending after it would glue two records together.
        let path = find_transcripts(b.adapter.root(), ID).pop().unwrap();
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"type\":\"user\",\"cw").unwrap();
        drop(f);
        a.append("two");
        assert!(pull(&b, &push(&a), None).unwrap_err().to_string().contains("unfinished line"));
    }

    #[test]
    fn a_live_session_and_a_duplicated_id_are_never_touched() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let pushed = push(&a);

        fs::create_dir_all(b.adapter.root().join("sessions")).unwrap();
        let pid = std::process::id();
        fs::write(
            b.adapter.root().join("sessions").join(format!("{pid}.json")),
            format!("{{\"pid\":{pid},\"sessionId\":\"{ID}\"}}"),
        )
        .unwrap();
        assert!(matches!(pull(&b, &pushed, Some(&b.project)), Err(CoreError::SessionLive { .. })));
        fs::remove_dir_all(b.adapter.root().join("sessions")).unwrap();

        for dir in ["x", "y"] {
            let d = b.adapter.root().join("projects").join(dir);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(format!("{ID}.jsonl")), a.record("dup")).unwrap();
        }
        assert!(pull(&b, &pushed, Some(&b.project)).unwrap_err().to_string().contains("2 project"));
    }

    type Pushed = (Manifest, HashMap<String, Vec<u8>>);

    /// Add an entry to a pushed bundle, as a hostile or buggy machine could.
    fn add_file(pushed: &mut Pushed, path: &str, bytes: &[u8]) {
        let sha = fsutil::sha256_hex(bytes);
        pushed.1.insert(sha.clone(), bytes.to_vec());
        pushed.0.files.retain(|f| f.path != path);
        pushed.0.files.push(FileEntry {
            path: path.into(),
            sha256: Some(sha),
            size: bytes.len() as u64,
            symlink: None,
        });
    }

    /// A symlink planted on disk earlier, by an older asm or a hostile
    /// revision, is never written through — under any of the four sidecar
    /// directories, on a new install or a fast-forward.
    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_symlink_already_on_disk() {
        for top in ["session-dir", "file-history", "session-env", "tasks"] {
            let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
            a.start(&["one"]);
            pull(&b, &push(&a), Some(&b.project)).unwrap();
            let victim = b.project.parent().unwrap().join("victim");
            fs::create_dir_all(&victim).unwrap();
            fs::write(victim.join("planted"), b"original").unwrap();
            let base = match top {
                "session-dir" => b.project_dir().join(ID),
                dir => b.adapter.root().join(dir).join(ID),
            };
            fs::create_dir_all(&base).unwrap();
            std::os::unix::fs::symlink(&victim, base.join("evil")).unwrap();

            a.append("two");
            let mut pushed = push(&a);
            add_file(&mut pushed, &format!("{top}/evil/planted"), b"REPLACED");
            add_file(&mut pushed, &format!("{top}/evil/new"), b"NEW");
            let err = pull(&b, &pushed, None).unwrap_err().to_string();
            assert!(err.contains("not a directory"), "{top}: {err}");
            assert_eq!(fs::read(victim.join("planted")).unwrap(), b"original", "{top}");
            assert!(!victim.join("new").exists(), "{top}");
            // The transcript still moved and still reads as B's own.
            assert_eq!(canonical_transcript(&transcript_of_b(&b)), fs::read(a.transcript()).unwrap());
            assert_eq!(b.project_root(), b.project, "{top}: marker written before the sidecars");
        }
    }

    /// A link is installed only when it points inside the session's own
    /// sidecar directory; one naming any other place is dropped.
    #[cfg(unix)]
    #[test]
    fn a_link_out_of_the_sidecar_is_never_installed() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let mut pushed = push(&a);
        let sidecar = pushed.0.extra["sidecar_dir"].as_str().unwrap().to_string();
        for (path, target) in [
            ("session-dir/ssh", "/home/someone/.ssh".to_string()),
            ("session-dir/up", format!("{sidecar}/../../..")),
            ("tasks/rel", "../../x".to_string()),
            ("session-dir/ok", format!("{sidecar}/subagents")),
            ("session-dir/rel", "subagents/agent-1.jsonl".to_string()),
            // Its own directory: a loop for anything walking the sidecar.
            ("session-dir/subagents/loop", format!("{sidecar}/subagents")),
        ] {
            pushed.0.files.push(FileEntry {
                path: path.into(),
                sha256: None,
                size: 0,
                symlink: Some(target),
            });
        }
        pull(&b, &pushed, Some(&b.project)).unwrap();
        let b_sidecar = b.project_dir().join(ID);
        for gone in [
            b_sidecar.join("ssh"),
            b_sidecar.join("up"),
            b_sidecar.join("subagents/loop"),
            b.adapter.root().join("tasks").join(ID).join("rel"),
        ] {
            assert!(fs::symlink_metadata(&gone).is_err(), "{}", gone.display());
        }
        assert_eq!(fs::read_link(b_sidecar.join("ok")).unwrap(), b_sidecar.join("subagents"));
        assert_eq!(fs::read_link(b_sidecar.join("rel")).unwrap(), Path::new("subagents/agent-1.jsonl"));
    }

    /// On a fast-forward a sidecar is replaced only by a later state of
    /// itself; one this machine changed differently is left alone.
    #[test]
    fn a_fast_forward_keeps_sidecars_changed_here() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let mut first = push(&a);
        add_file(&mut first, "tasks/t.json", b"{\"v\":1}");
        add_file(&mut first, "session-dir/subagents/s.jsonl", b"{}\n");
        pull(&b, &first, Some(&b.project)).unwrap();
        let tasks = b.adapter.root().join("tasks").join(ID).join("t.json");
        fs::write(&tasks, b"{\"v\":2,\"here\":true}").unwrap();

        a.append("two");
        let mut second = push(&a);
        add_file(&mut second, "tasks/t.json", b"{\"v\":1,\"there\":true}");
        add_file(&mut second, "session-dir/subagents/s.jsonl", b"{}\n{}\n");
        let installed = pull(&b, &second, None).unwrap();
        assert!(matches!(installed.outcome, InstallOutcome::FastForward { .. }));
        assert_eq!(fs::read(&tasks).unwrap(), b"{\"v\":2,\"here\":true}");
        let sub = b.project_dir().join(ID).join("subagents/s.jsonl");
        assert_eq!(fs::read(sub).unwrap(), b"{}\n{}\n", "an appended sidecar follows");
    }

    /// A sidecar the other machine rewrote whole (a task file) arrives on a
    /// fast-forward when this machine left it as last synced; one changed
    /// here too is kept.
    #[test]
    fn a_fast_forward_brings_rewritten_sidecars_this_machine_did_not_touch() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let mut first = push(&a);
        add_file(&mut first, "tasks/1.json", b"{\"status\":\"pending\"}");
        add_file(&mut first, "tasks/2.json", b"{\"status\":\"pending\"}");
        pull(&b, &first, Some(&b.project)).unwrap();
        let synced = Base {
            canonical: first.0.canonical.clone(),
            files: first.0.files.iter().filter_map(|f| Some((f.path.clone(), f.sha256.clone()?))).collect(),
        };
        let tasks = b.adapter.root().join("tasks").join(ID);
        fs::write(tasks.join("2.json"), b"{\"status\":\"edited here\"}").unwrap();

        a.append("two");
        let mut second = push(&a);
        add_file(&mut second, "tasks/1.json", b"{\"status\":\"completed\"}");
        add_file(&mut second, "tasks/2.json", b"{\"status\":\"completed\"}");
        let scratch = tempfile::tempdir().unwrap();
        let blob = |sha: &str| -> Result<PathBuf, CoreError> {
            let path = scratch.path().join(sha);
            fs::write(&path, &second.1[sha]).unwrap();
            Ok(path)
        };
        let installed = install(&b.adapter, &second.0, &blob, None, Some(&synced)).unwrap();
        assert!(matches!(installed.outcome, InstallOutcome::FastForward { .. }));
        assert_eq!(fs::read(tasks.join("1.json")).unwrap(), b"{\"status\":\"completed\"}");
        assert_eq!(fs::read(tasks.join("2.json")).unwrap(), b"{\"status\":\"edited here\"}");
    }

    /// A copy here with no record of where it ran is filed only where it
    /// already sits, and never gets a marker with an empty path.
    #[test]
    fn a_copy_with_no_location_is_placed_only_where_it_is_filed() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        fs::create_dir_all(b.project_dir()).unwrap();
        fs::write(b.transcript(), b"").unwrap();
        let pushed = push(&a);

        let err = pull(&b, &pushed, None).unwrap_err().to_string();
        assert!(err.contains("--project-dir"), "{err}");
        assert!(fs::read(b.transcript()).unwrap().is_empty());

        let installed = pull(&b, &pushed, Some(&b.project)).unwrap();
        assert!(matches!(installed.outcome, InstallOutcome::FastForward { .. }));
        assert_eq!(b.project_root(), b.project);
        assert!(!String::from_utf8(transcript_of_b(&b)).unwrap().contains("\"relocatedCwd\":\"\""));
    }

    #[cfg(unix)]
    #[test]
    fn one_pull_of_an_id_at_a_time() {
        use std::os::fd::AsRawFd;
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let pushed = push(&a);
        let lock = b.adapter.root().join("projects").join(format!(".asm-install-{ID}.lock"));
        let held = fs::File::create(&lock).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        assert!(pull(&b, &pushed, Some(&b.project)).unwrap_err().to_string().contains("running"));
        assert!(find_transcripts(b.adapter.root(), ID).is_empty());

        // Released however the holder ended; a lock file left behind is no
        // obstacle.
        drop(held);
        pull(&b, &pushed, Some(&b.project)).unwrap();
    }

    /// Another machine running an older asm could upload markers; they must
    /// not be installed as history.
    #[test]
    fn a_foreign_marker_in_the_bundle_is_not_installed() {
        let (a, b) = (Machine::new("proj-a"), Machine::new("proj-b"));
        a.start(&["one"]);
        let (mut manifest, mut blobs) = push(&a);
        let mut raw = fs::read(a.transcript()).unwrap();
        raw.extend_from_slice(write::relocated_marker(ID, "/elsewhere").as_bytes());
        raw.push(b'\n');
        let sha = fsutil::sha256_hex(&raw);
        blobs.insert(sha.clone(), raw);
        manifest.files.iter_mut().find(|f| f.path == "transcript.jsonl").unwrap().sha256 = Some(sha);
        pull(&b, &(manifest, blobs), Some(&b.project)).unwrap();
        let installed = String::from_utf8(transcript_of_b(&b)).unwrap();
        assert!(!installed.contains("/elsewhere"));
        assert_eq!(b.project_root(), b.project);
    }
}
