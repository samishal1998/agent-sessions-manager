//! Peer transfers inside one process: packing, unpacking and installing
//! against an isolated Claude store, the ssh transport against a fake `ssh`
//! on PATH, and the peers file. One test, because the stores and PATH are
//! process-wide and every step builds on the one before.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use asm_core::adapter::SessionFilter;
use asm_core::adapter::claude::encode_project_dir;
use asm_core::hub::bundle::{InstallOutcome, Installed};
use asm_core::hub::peer::{self, Peer, Transport};
use asm_core::ops;

const ID: &str = "7f3a1c88-2d4e-4b91-9a05-6c7e8f201b43";

/// Every store this process could find, and asm's own data dir, under one
/// scratch directory, plus a `bin/` first on PATH for the fake ssh. Set
/// once, before anything reads them.
fn isolate() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for (var, sub) in [
            ("HOME", "home"),
            ("ASM_DATA_DIR", "asm"),
            ("XDG_DATA_HOME", "xdg-data"),
            ("XDG_CONFIG_HOME", "xdg-config"),
            ("XDG_STATE_HOME", "xdg-state"),
            ("XDG_CACHE_HOME", "xdg-cache"),
            ("CLAUDE_CONFIG_DIR", "claude"),
            ("CODEX_HOME", "codex"),
            ("JCODE_HOME", "jcode"),
            ("ASM_ANTIGRAVITY_ROOT", "antigravity"),
        ] {
            fs::create_dir_all(root.join(sub)).unwrap();
            // Safety: set once per test process, before concurrent readers.
            unsafe { std::env::set_var(var, root.join(sub)) };
        }
        fs::create_dir_all(root.join("bin")).unwrap();
        let path = format!("{}:{}", root.join("bin").display(), std::env::var("PATH").unwrap_or_default());
        unsafe { std::env::set_var("PATH", path) };
        dir
    })
    .path()
}

fn line(project: &Path, i: usize, text: &str) -> String {
    let kind = if i.is_multiple_of(2) { "user" } else { "assistant" };
    let content = if kind == "user" {
        serde_json::json!(text)
    } else {
        serde_json::json!([{ "type": "text", "text": text }])
    };
    format!(
        "{}\n",
        serde_json::json!({ "type": kind, "cwd": project, "sessionId": ID, "uuid": format!("u{i}"),
                            "parentUuid": i.checked_sub(1).map(|p| format!("u{p}")),
                            "timestamp": format!("2026-10-09T10:00:{i:02}Z"),
                            "message": { "role": kind, "content": content } })
    )
}

fn tar_of(dir: &Path, out: &Path) {
    let status = std::process::Command::new("tar").args(["-cf"]).arg(out).arg("-C").arg(dir).arg(".").status().unwrap();
    assert!(status.success());
}

fn tar_lists(tar: &Path) -> String {
    let out = std::process::Command::new("tar").arg("-tf").arg(tar).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_session_crosses_to_a_peer_and_back_without_a_hub() {
    let root = isolate();
    let project = root.join("home/code/app");
    fs::create_dir_all(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let store = root.join("claude/projects").join(encode_project_dir(project.to_str().unwrap()));
    fs::create_dir_all(&store).unwrap();
    let transcript = store.join(format!("{ID}.jsonl"));
    let lines: Vec<String> = (0..4).map(|i| line(&project, i, &format!("turn {i}"))).collect();
    fs::write(&transcript, lines[..2].concat()).unwrap();
    let session = || ops::resolve_ref(ID, &SessionFilter::default()).unwrap();

    // Pack → tar → unpack gives the same manifest and the same bytes.
    let packed = peer::pack(&session()).unwrap();
    assert!(packed.tar.is_file());
    assert!(tar_lists(&packed.tar).contains("manifest.json"));
    let unpacked = peer::unpack(&packed.tar).unwrap();
    assert_eq!(unpacked.manifest.id, ID);
    assert_eq!(unpacked.manifest.canonical, packed.manifest.canonical);
    assert!(unpacked.manifest.files.iter().any(|f| f.path == "transcript.jsonl"));
    assert_eq!(unpacked.install(None).unwrap().outcome, InstallOutcome::InSync);

    // The copy here moves on: the old bundle is behind it.
    fs::write(&transcript, lines[..3].concat()).unwrap();
    assert_eq!(unpacked.install(None).unwrap().outcome, InstallOutcome::Ahead);
    let newer = peer::pack(&session()).unwrap();
    // Both sides moved: refused, and nothing written.
    fs::write(&transcript, lines[..2].concat() + &line(&project, 2, "something else")).unwrap();
    let before = fs::read(&transcript).unwrap();
    let diverged = peer::unpack(&newer.tar).unwrap().install(None).unwrap();
    assert_eq!(diverged.outcome, InstallOutcome::Diverged);
    assert_eq!(fs::read(&transcript).unwrap(), before);
    // Behind: fast-forwarded. Gone: installed anew, at the same place.
    fs::write(&transcript, lines[..2].concat()).unwrap();
    let ff = peer::unpack(&newer.tar).unwrap().install(None).unwrap();
    assert!(matches!(ff.outcome, InstallOutcome::FastForward { appended } if appended > 0), "{ff:?}");
    assert!(fs::read_to_string(&transcript).unwrap().contains("turn 2"));
    fs::remove_file(&transcript).unwrap();
    let fresh = peer::unpack(&newer.tar).unwrap().install(None).unwrap();
    assert_eq!((fresh.outcome, fresh.project_root.as_path()), (InstallOutcome::New, project.as_path()));
    assert!(transcript.is_file());

    // What comes out of tar is checked, not trusted.
    let hostile = tempfile::tempdir().unwrap();
    let bundle = hostile.path().join("b");
    let fresh_bundle = || {
        let _ = fs::remove_dir_all(&bundle);
        fs::create_dir_all(bundle.join("blobs")).unwrap();
        fs::write(bundle.join("manifest.json"), serde_json::to_vec(&newer.manifest).unwrap()).unwrap();
        for f in &newer.manifest.files {
            let sha = f.sha256.as_deref().unwrap();
            fs::write(bundle.join("blobs").join(sha), b"wrong\n").unwrap();
        }
    };
    let tar = hostile.path().join("x.tar");
    fresh_bundle();
    tar_of(&bundle, &tar);
    assert!(peer::unpack(&tar).unwrap_err().to_string().contains("does not match its hash"));
    fresh_bundle();
    fs::remove_dir_all(bundle.join("blobs")).unwrap();
    fs::create_dir_all(bundle.join("blobs")).unwrap();
    tar_of(&bundle, &tar);
    assert!(peer::unpack(&tar).unwrap_err().to_string().contains("missing file"));
    fresh_bundle();
    fs::write(bundle.join("evil.sh"), b"x").unwrap();
    tar_of(&bundle, &tar);
    assert!(peer::unpack(&tar).unwrap_err().to_string().contains("\"evil.sh\""));
    fs::write(&tar, b"not a tar at all").unwrap();
    assert!(peer::unpack(&tar).unwrap_err().to_string().contains("could not be unpacked"));
    // Members that try to leave the scratch directory. tar itself strips
    // the leading `../` and `/` (GNU tar; bsdtar refuses them), and
    // whatever lands is then a stray entry: refused either way, and nothing
    // appears beside the scratch directory.
    let escape = hostile.path().join("escape.tar");
    let status = std::process::Command::new("python3")
        .args(["-I", "-c", "import sys,tarfile,io\nt=tarfile.open(sys.argv[1],'w')\nfor n in ['../escaped','/tmp/escaped-abs']:\n    i=tarfile.TarInfo(n); i.size=1; t.addfile(i, io.BytesIO(b'x'))\nt.close()"])
        .arg(&escape)
        .status()
        .unwrap();
    assert!(status.success());
    let data = root.join("asm");
    let before: Vec<_> = fs::read_dir(&data).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(peer::unpack(&escape).is_err());
    let after: Vec<_> = fs::read_dir(&data).unwrap().flatten().map(|e| e.file_name()).collect();
    assert_eq!(before, after, "nothing escaped the scratch directory");
    assert!(!Path::new("/tmp/escaped-abs").exists());

    // A symlink member followed by a file written through it: refused from
    // the listing, before tar extracts anything, whichever tar this is.
    let target = hostile.path().join("outside");
    fs::create_dir_all(&target).unwrap();
    let linked = hostile.path().join("linked.tar");
    let status = std::process::Command::new("python3")
        .args(["-I", "-c", "import sys,tarfile,io\nt=tarfile.open(sys.argv[1],'w')\ni=tarfile.TarInfo('blobs'); i.type=tarfile.SYMTYPE; i.linkname=sys.argv[2]; t.addfile(i)\nj=tarfile.TarInfo('blobs/pwned'); j.size=1; t.addfile(j, io.BytesIO(b'x'))\nt.close()"])
        .arg(&linked)
        .arg(&target)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(peer::unpack(&linked).unwrap_err().to_string().contains("not part of a session bundle"));
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0, "nothing was written through the symlink");

    // The ssh transport, against a fake ssh first on PATH. It records what
    // it was asked, consumes stdin on a receive and answers from files
    // the test lays down; two hosts fail the way real ones do.
    let bin = root.join("bin");
    let script = bin.join("ssh");
    fs::write(
        &script,
        concat!(
            "#!/bin/sh\n",
            "here=$(dirname \"$0\"); host=$3; cmd=$4\n",
            "printf '%s\\n' \"$host\" \"$cmd\" > \"$here/last\"\n",
            "case \"$host\" in\n",
            "  down) echo 'ssh: connect to host down port 22: Connection refused' >&2; exit 255;;\n",
            "  noasm) echo 'sh: 1: asm: command not found' >&2; exit 127;;\n",
            "esac\n",
            "case \"$cmd\" in\n",
            "  'asm list --json') cat \"$here/list.json\";;\n",
            "  'asm hub send '*) cat \"$here/send.tar\";;\n",
            "  'asm hub receive'*) cat > \"$here/received.tar\"; cat \"$here/reply.json\";;\n",
            "  *) echo \"unexpected: $cmd\" >&2; exit 2;;\n",
            "esac\n"
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let last = || fs::read_to_string(bin.join("last")).unwrap();
    let box_ = Peer { name: "box".into(), transport: Transport::Ssh { host: "ok".into() } };
    let reply = Installed { outcome: InstallOutcome::New, project_root: PathBuf::from("/x y"), path: PathBuf::from("/x y/t") };
    fs::write(bin.join("reply.json"), serde_json::to_vec(&reply).unwrap()).unwrap();
    let sent = peer::push_to(&box_, &session(), Some(Path::new("/x y"))).unwrap();
    assert_eq!((sent.installed.outcome, sent.from.as_deref()), (InstallOutcome::New, Some("box")));
    assert_eq!(last(), "ok\nasm hub receive --project-dir '/x y'\n", "quoted for the peer's shell");
    assert!(tar_lists(&bin.join("received.tar")).contains("manifest.json"), "the tar went up stdin");

    fs::write(bin.join("list.json"), serde_json::to_vec(&vec![session()]).unwrap()).unwrap();
    fs::copy(&newer.tar, bin.join("send.tar")).unwrap();
    assert_eq!(peer::sessions(&box_).unwrap().len(), 1);
    let pulled = peer::pull_from(&box_, &ID[..8], None).unwrap();
    assert_eq!((pulled.installed.outcome, pulled.from.as_deref()), (InstallOutcome::InSync, Some("box")));
    assert_eq!(last(), format!("ok\nasm hub send claude-code:{ID}\n"), "resolved here, asked for exactly");
    let e = peer::pull_from(&box_, "nope", None).unwrap_err().to_string();
    assert!(e.contains("no session matches 'nope' on box"), "{e}");

    let down = Peer { name: "down".into(), transport: Transport::Ssh { host: "down".into() } };
    let e = peer::sessions(&down).unwrap_err().to_string();
    assert!(e.contains("could not reach down over ssh") && e.contains("Connection refused"), "{e}");
    let noasm = Peer { name: "noasm".into(), transport: Transport::Ssh { host: "noasm".into() } };
    let e = peer::push_to(&noasm, &session(), None).unwrap_err().to_string();
    assert!(e.contains("asm is not on the PATH of a non-interactive shell on noasm"), "{e}");

    // The peers file: 0600, one name once, addresses checked up front.
    assert!(peer::list().unwrap().is_empty());
    let added = peer::add("box", "ssh://me@box", None, "me", false).unwrap();
    assert_eq!(added.address(), "ssh://me@box");
    assert!(peer::add("box", "ssh://other", None, "me", false).unwrap_err().to_string().contains("exists"));
    assert!(peer::add("bad name", "ssh://x", None, "me", false).is_err());
    assert!(peer::add("x", "box", None, "me", false).unwrap_err().to_string().contains("not a peer address"));
    assert!(peer::add("x", "ssh://-oProxyCommand=evil", None, "me", false).is_err());
    let e = peer::add("h", "http://127.0.0.1:1", None, "me", false).unwrap_err().to_string();
    assert!(e.contains("join token"), "{e}");
    assert_eq!(peer::get("box").unwrap().name, "box");
    assert!(peer::get("nobody").unwrap_err().to_string().contains("asm peer add"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(root.join("asm/peers.json")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "an HTTP peer's credential lives here");
    }
    assert_eq!(peer::remove("box").unwrap().name, "box");
    assert!(peer::list().unwrap().is_empty());
    assert!(peer::remove("box").is_err());
}
