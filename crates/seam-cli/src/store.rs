//! On-disk state: this machine's identity and the peers it has paired with.
//!
//! Two files, both created automatically on first run. There is no configuration file
//! and nothing here is ever hand-edited — see goal Z1.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use seam_transport::{Fingerprint, Identity, TrustStore};

const IDENTITY_CERT: &str = "identity.crt.der";
const IDENTITY_KEY: &str = "identity.key.der";
const PEERS: &str = "peers";

/// Where seam keeps its state, following each platform's own convention.
pub(crate) fn state_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "seam", "seam")
        .context("could not determine this platform's application data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Load this machine's identity, generating and persisting one on first run.
///
/// Generating rather than prompting is the whole point: the machine's identity is
/// something seam can decide for itself, so it does (goal Z2).
pub(crate) fn load_or_create_identity(dir: &Path) -> Result<Identity> {
    let cert_path = dir.join(IDENTITY_CERT);
    let key_path = dir.join(IDENTITY_KEY);

    if cert_path.exists() && key_path.exists() {
        let cert =
            fs::read(&cert_path).with_context(|| format!("reading {}", cert_path.display()))?;
        let key = fs::read(&key_path).with_context(|| format!("reading {}", key_path.display()))?;
        return Identity::from_der(cert, key).context(
            "this machine's stored identity is unreadable; delete it and seam will make a new \
             one, but every peer will then need re-pairing",
        );
    }

    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let identity = Identity::generate().context("generating this machine's identity")?;
    fs::write(&cert_path, identity.certificate_der())
        .with_context(|| format!("writing {}", cert_path.display()))?;
    write_private(&key_path, identity.private_key_der())?;
    Ok(identity)
}

/// Write a file containing key material with owner-only permissions.
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            // 0o600 before any bytes are written, so the key is never briefly readable.
            .mode(0o600)
            .open(path)
            .with_context(|| format!("writing {}", path.display()))?;
        file.write_all(bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // Windows inherits the ACL of the per-user data directory, which is already
        // owner-only. There is no portable mode bit to set here.
        fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Load the set of paired peers.
///
/// The format is one peer per line, `<64-hex fingerprint> <display name>`. Deliberately
/// plain text: it is the one file a user might ever want to inspect or clear by hand,
/// and a line that fails to parse is skipped with a warning rather than taking down the
/// whole file.
pub(crate) fn load_peers(dir: &Path) -> TrustStore {
    let path = dir.join(PEERS);
    let mut store = TrustStore::new();
    let Ok(text) = fs::read_to_string(&path) else {
        // A missing file is a first run, not an error.
        return store;
    };

    for (number, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, name) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        if let Some(fingerprint) = parse_fingerprint(hex) {
            store.trust(fingerprint, name.trim());
        } else {
            tracing::warn!(
                file = %path.display(),
                line = number + 1,
                "skipping an unreadable entry in the paired-peers file"
            );
        }
    }
    store
}

pub(crate) fn save_peers(dir: &Path, store: &TrustStore) -> Result<()> {
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut text = String::from("# seam paired peers: <fingerprint> <name>\n");
    for (_, peer) in store.iter() {
        text.push_str(&peer.fingerprint.to_grouped_hex().replace(' ', ""));
        text.push(' ');
        text.push_str(&peer.name);
        text.push('\n');
    }
    let path = dir.join(PEERS);
    fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn parse_fingerprint(hex: &str) -> Option<Fingerprint> {
    if hex.len() != 64 || !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut raw = [0u8; 32];
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(Fingerprint::from_bytes(raw))
}

/// Resolve a user-typed peer reference against the paired peers.
///
/// Accepts a display name or an id prefix, case-insensitively, and refuses an ambiguous
/// match rather than guessing — silently acting on the wrong machine is worse than asking
/// again.
pub(crate) fn resolve_peer(store: &TrustStore, query: &str) -> Result<Fingerprint> {
    let needle = query.trim().to_lowercase();
    let matches: Vec<_> = store
        .iter()
        .filter(|(id, peer)| {
            peer.name.to_lowercase() == needle
                || id.to_string().starts_with(&needle)
                || peer.fingerprint.to_grouped_hex().replace(' ', "").starts_with(&needle)
        })
        .collect();

    match matches.as_slice() {
        [(_, peer)] => Ok(peer.fingerprint),
        [] => bail!("no paired peer matches {query:?}. Run `seam peers` to see the list."),
        many => {
            let names: Vec<_> = many.iter().map(|(id, p)| format!("{} ({id})", p.name)).collect();
            bail!("{query:?} matches more than one peer: {}", names.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("seam-store-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn identity_is_generated_once_and_then_reused() {
        let dir = temp_dir("identity");
        let first = load_or_create_identity(&dir).unwrap();
        let second = load_or_create_identity(&dir).unwrap();
        assert_eq!(first.fingerprint(), second.fingerprint(), "identity must be stable");
        let _ = fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_private_key_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = temp_dir("perms");
        load_or_create_identity(&dir).unwrap();
        let mode = fs::metadata(dir.join(IDENTITY_KEY)).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "key file is readable by others: {mode:o}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn peers_round_trip_through_disk() {
        let dir = temp_dir("peers");
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();

        let mut store = TrustStore::new();
        store.trust(a.fingerprint(), "Mac-mini");
        store.trust(b.fingerprint(), "amor");
        save_peers(&dir, &store).unwrap();

        let loaded = load_peers(&dir);
        assert_eq!(loaded.len(), 2);
        assert!(loaded.is_trusted(a.fingerprint()));
        assert!(loaded.is_trusted(b.fingerprint()));
        assert_eq!(loaded.get(a.peer_id()).unwrap().name, "Mac-mini");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_line_is_skipped_rather_than_failing_the_whole_file() {
        let dir = temp_dir("corrupt");
        fs::create_dir_all(&dir).unwrap();
        let good = Identity::generate().unwrap();
        fs::write(
            dir.join(PEERS),
            format!(
                "# comment\n\nnot-a-fingerprint some-name\n{} good-peer\n",
                good.fingerprint().to_grouped_hex().replace(' ', "")
            ),
        )
        .unwrap();

        let loaded = load_peers(&dir);
        assert_eq!(loaded.len(), 1, "the readable entry must survive");
        assert!(loaded.is_trusted(good.fingerprint()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_peers_file_is_an_empty_store_not_an_error() {
        // First run must never fail because state does not exist yet.
        let loaded = load_peers(&temp_dir("absent"));
        assert!(loaded.is_empty());
    }

    #[test]
    fn peer_lookup_accepts_a_name_or_an_id_prefix() {
        let id = Identity::generate().unwrap();
        let mut store = TrustStore::new();
        store.trust(id.fingerprint(), "Mac-mini");

        assert_eq!(resolve_peer(&store, "Mac-mini").unwrap(), id.fingerprint());
        assert_eq!(resolve_peer(&store, "mac-mini").unwrap(), id.fingerprint());
        assert_eq!(resolve_peer(&store, &id.peer_id().to_string()).unwrap(), id.fingerprint());
        assert!(resolve_peer(&store, "nonexistent").is_err());
    }

    #[test]
    fn an_ambiguous_reference_is_refused_rather_than_guessed() {
        let mut store = TrustStore::new();
        for _ in 0..2 {
            store.trust(Identity::generate().unwrap().fingerprint(), "duplicate");
        }
        let err = resolve_peer(&store, "duplicate").unwrap_err().to_string();
        assert!(err.contains("more than one"), "{err}");
    }
}
