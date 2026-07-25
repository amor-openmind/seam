//! Licence checking.
//!
//! # What this is, and what it honestly is not
//!
//! A licence is a short token the owner signs with a private key that never leaves their
//! machine. seam carries only the matching **public** key, so a build can verify a licence
//! but cannot mint one. There is no licence server and nothing is sent anywhere: a licence
//! works on a machine that has never been online.
//!
//! What it does **not** do is make the software uncopyable. Anyone able to rebuild or patch
//! the binary can remove this check — that is true of every client-side licence ever
//! shipped, and claiming otherwise would be dishonest. What it does is make running seam a
//! deliberate act that requires something only the owner can issue.
//!
//! # Format
//!
//! `seam-<payload-hex>-<signature-hex>`, where the payload is `name|expiry`, expiry being
//! a Unix day number or `0` for perpetual. Everything needed to check a licence is in the
//! licence, so verification is offline and total.

use anyhow::{Context as _, Result, bail};
use ring::signature::{ED25519, UnparsedPublicKey};

/// The owner's public key, baked in at build time.
///
/// Overridable at build time so the owner can cut releases with their own key without
/// editing source: `SEAM_LICENCE_KEY=<hex> cargo build --release`. The placeholder below
/// is not a real key — a build without the variable set refuses every licence, which is
/// the safe direction to fail.
const OWNER_PUBLIC_KEY_HEX: &str = match option_env!("SEAM_LICENCE_KEY") {
    Some(key) => key,
    None => "",
};

/// A verified licence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Licence {
    /// Who it was issued to. Shown in the UI so a machine can say whose licence it runs on.
    pub name: String,
    /// Unix day number after which it stops working; `0` means perpetual.
    pub expires_day: u64,
}

impl Licence {
    /// Is it still valid today?
    pub(crate) fn is_current(&self) -> bool {
        if self.expires_day == 0 {
            return true;
        }
        today_day_number().is_none_or(|today| today <= self.expires_day)
    }
}

/// Days since the Unix epoch, or `None` if the clock is unreadable.
fn today_day_number() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|d| d.as_secs() / 86_400)
}

/// Check a licence string, returning it only if the signature and dates hold.
pub(crate) fn verify(text: &str) -> Result<Licence> {
    if OWNER_PUBLIC_KEY_HEX.is_empty() {
        bail!(
            "this build carries no licence key, so it cannot verify anything — it was not \
             produced by the owner's release process"
        );
    }
    let body = text.trim().strip_prefix("seam-").context("not a seam licence")?;
    let (payload_hex, signature_hex) =
        body.rsplit_once('-').context("this licence is incomplete")?;

    let payload = hex::decode(payload_hex).context("this licence is damaged")?;
    let signature = hex::decode(signature_hex).context("this licence is damaged")?;
    let key = hex::decode(OWNER_PUBLIC_KEY_HEX).context("this build's licence key is damaged")?;

    UnparsedPublicKey::new(&ED25519, &key)
        .verify(&payload, &signature)
        .map_err(|_| anyhow::anyhow!("this licence was not issued for seam"))?;

    let text = String::from_utf8(payload).context("this licence is damaged")?;
    let (name, expiry) = text.split_once('|').context("this licence is damaged")?;
    Ok(Licence {
        name: name.to_owned(),
        expires_day: expiry.parse().context("this licence has an unreadable date")?,
    })
}

/// Where a licence lives: one place per machine, not per copy of the binary.
///
/// This was originally kept beside the identity so it would travel with a portable
/// install — which meant a binary in `Downloads` and the same binary in `~/.seam` looked
/// in different folders and each demanded its own activation. A licence is a fact about
/// the machine, so it belongs in the platform's application-data directory whatever
/// folder seam is run from. `SEAM_HOME` still overrides everything, for testing.
fn licence_home() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("SEAM_HOME")
        && !home.is_empty()
    {
        return Some(std::path::PathBuf::from(home));
    }
    let dirs = directories::ProjectDirs::from("dev", "seam", "seam")?;
    let dir = dirs.data_dir().to_path_buf();
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// The activated licence for this machine, wherever seam was started from.
pub(crate) fn stored(dir: &std::path::Path) -> Option<Licence> {
    // The machine-wide location first, then the state directory — so a licence activated
    // by an older build, which wrote it beside the identity, is still honoured instead of
    // silently asking again after an update.
    let places = [licence_home(), Some(dir.to_path_buf())];
    for place in places.into_iter().flatten() {
        if let Ok(text) = std::fs::read_to_string(place.join("licence"))
            && let Ok(licence) = verify(&text)
            && licence.is_current()
        {
            return Some(licence);
        }
    }
    None
}

/// Store a licence after checking it, so an invalid one is refused at the moment it is
/// entered rather than at the next start.
pub(crate) fn activate(dir: &std::path::Path, text: &str) -> Result<Licence> {
    let licence = verify(text)?;
    if !licence.is_current() {
        bail!("this licence has expired");
    }
    // Written machine-wide, so every copy of seam on this machine finds it. The state
    // directory gets it too: a portable install carried to another machine then has the
    // licence with it, which is what portability was for.
    if let Some(home) = licence_home() {
        std::fs::write(home.join("licence"), text.trim())?;
    }
    std::fs::write(dir.join("licence"), text.trim())?;
    Ok(licence)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_without_a_key_refuses_everything() {
        // The safe direction to fail: a build that cannot verify must not assume valid.
        if OWNER_PUBLIC_KEY_HEX.is_empty() {
            assert!(verify("seam-00-00").is_err());
        }
    }

    #[test]
    fn nonsense_is_refused_without_panicking() {
        for bad in ["", "seam-", "hello", "seam-zz-zz", "seam--", &"seam-".repeat(50)] {
            assert!(verify(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_perpetual_licence_never_expires_and_a_past_one_always_has() {
        assert!(Licence { name: "x".into(), expires_day: 0 }.is_current());
        assert!(!Licence { name: "x".into(), expires_day: 1 }.is_current());
    }
}
