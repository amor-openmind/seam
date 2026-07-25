//! Pairing: turning "two machines that have never met" into a trusted link, with one
//! confirmation and nothing to configure.
//!
//! # Why a 6-digit code and not a fingerprint dialog
//!
//! Every tool in this category shows a 64-hex-character certificate fingerprint and asks
//! the user to verify it. Nobody does. They click accept — or, as observed on this
//! machine, they give up and pass `--disable-crypto`, which is how a KVM ends up sending
//! keystrokes in plaintext over the LAN.
//!
//! Reading a 64-character hex string is a *transcription* task, which humans are bad at.
//! Checking that two 6-digit numbers match is a *comparison* task, which humans are good
//! at. Same guarantee, radically better completion rate.
//!
//! # Why it is secure
//!
//! The code is derived from the TLS 1.3 exporter (RFC 5705) of the live connection, not
//! from anything transmitted. A man-in-the-middle must terminate **two** separate TLS
//! sessions — one to each side — and those sessions have different exporter secrets, so
//! the two machines display **different codes** and the user sees the mismatch.
//!
//! This is the same construction as ZRTP's SAS and Bluetooth Secure Simple Pairing's
//! numeric comparison. It needs no PAKE, and specifically avoids depending on the
//! `spake2` crate, whose own documentation states it has never received an independent
//! security audit.

use sha2::{Digest, Sha256};

/// Length of the exporter output the pairing code is derived from.
const EXPORTER_LEN: usize = 32;

/// RFC 5705 exporter label. Versioned so a future change to the pairing scheme cannot be
/// downgraded into by an old peer: the labels differ, so the codes never match.
pub const EXPORTER_LABEL: &[u8] = b"seam-pair-v1";

/// Number of digits shown to the user.
///
/// Six digits is 20 bits ≈ 1-in-a-million per attempt. That is the right amount of
/// security *because the code is one-shot*: an attacker gets a single guess at an
/// interactive confirmation the user is actively watching, not an offline search.
pub const CODE_DIGITS: u32 = 6;

const CODE_MODULUS: u32 = 1_000_000;

/// A short authentication string, shown to the user on both machines.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PairingCode(u32);

impl PairingCode {
    /// Derive from a connection's exporter output.
    ///
    /// Both peers call this with the *same* label and their own connection's exporter,
    /// and get the same value if and only if they are talking directly to each other.
    #[must_use]
    pub fn from_exporter(exporter: &[u8; EXPORTER_LEN]) -> Self {
        // Hash rather than truncating the exporter directly: this value is displayed, and
        // displaying raw key-schedule output — even 20 bits of it — is a habit worth not
        // forming.
        let mut hasher = Sha256::new();
        hasher.update(b"seam-pairing-code-v1");
        hasher.update(exporter);
        let digest = hasher.finalize();

        let raw = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
        // Modulo bias here is negligible: 2^32 is not an exact multiple of 10^6, so some
        // codes are ~0.0002% likelier. That is irrelevant against a one-shot online guess.
        Self(raw % CODE_MODULUS)
    }

    #[must_use]
    pub const fn value(self) -> u32 {
        self.0
    }

    /// Zero-padded, grouped as `123 456`.
    ///
    /// Grouping matters: three-digit chunks are inside working-memory limits, so users
    /// compare two groups rather than six loose digits, which is where misreads happen.
    #[must_use]
    pub fn to_display_string(self) -> String {
        format!("{:03} {:03}", self.0 / 1000, self.0 % 1000)
    }

    /// Compare against digits the user typed or read out, ignoring spaces and dashes.
    ///
    /// Constant-time is deliberate. It is very probably unnecessary — an attacker who
    /// could time this already has code execution — but a 20-bit secret compared with
    /// `==` is the kind of thing that gets flagged in review, and the cost is nothing.
    #[must_use]
    pub fn matches_input(self, input: &str) -> bool {
        let Some(parsed) = parse_code(input) else { return false };
        // Fold to a single accumulator so the comparison does not short-circuit.
        let diff = parsed ^ self.0;
        diff == 0
    }
}

impl core::fmt::Display for PairingCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_display_string())
    }
}

impl core::fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "PairingCode({})", self.to_display_string())
    }
}

/// Parse user-entered digits, tolerating the separators people naturally type.
fn parse_code(input: &str) -> Option<u32> {
    let mut value: u32 = 0;
    let mut digits = 0u32;
    for ch in input.chars() {
        match ch {
            // Separators people naturally type; ignored rather than rejected.
            ' ' | '-' | '_' | '\t' => {}
            '0'..='9' => {
                digits += 1;
                if digits > CODE_DIGITS {
                    return None;
                }
                // `digits <= 6` so this cannot overflow.
                value = value * 10 + (ch as u32 - '0' as u32);
            }
            _ => return None,
        }
    }
    (digits == CODE_DIGITS).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exporter(seed: u8) -> [u8; EXPORTER_LEN] {
        let mut out = [0u8; EXPORTER_LEN];
        for (i, b) in out.iter_mut().enumerate() {
            // `i` is bounded by 32, so this cannot truncate meaningfully.
            *b = seed.wrapping_add(u8::try_from(i).unwrap_or(0).wrapping_mul(31));
        }
        out
    }

    #[test]
    fn same_exporter_yields_the_same_code_on_both_peers() {
        let secret = exporter(7);
        assert_eq!(PairingCode::from_exporter(&secret), PairingCode::from_exporter(&secret));
    }

    #[test]
    fn a_man_in_the_middle_produces_different_codes() {
        // The MITM necessarily terminates two TLS sessions, so the two sides' exporters
        // differ. This is the whole security argument, so it gets a test.
        let alice_sees = PairingCode::from_exporter(&exporter(1));
        let bob_sees = PairingCode::from_exporter(&exporter(2));
        assert_ne!(alice_sees, bob_sees, "distinct sessions must not display the same code");
    }

    #[test]
    fn code_is_always_six_digits_when_displayed() {
        // Including the cases that would render as "0" or "1 234" if formatted naively.
        for raw in [0u32, 1, 42, 999, 1000, 999_999] {
            let text = PairingCode(raw).to_display_string();
            assert_eq!(text.replace(' ', "").len(), 6, "{raw} rendered as {text:?}");
            assert_eq!(text.chars().nth(3), Some(' '), "{raw} must be grouped");
        }
    }

    #[test]
    fn code_is_in_range() {
        for seed in 0..64u8 {
            assert!(PairingCode::from_exporter(&exporter(seed)).value() < CODE_MODULUS);
        }
    }

    #[test]
    fn user_input_tolerates_the_separators_people_actually_type() {
        let code = PairingCode(123_456);
        for input in ["123456", "123 456", "123-456", " 123 456 ", "1 2 3 4 5 6", "123_456"] {
            assert!(code.matches_input(input), "should accept {input:?}");
        }
    }

    #[test]
    fn user_input_rejects_wrong_or_malformed_codes() {
        let code = PairingCode(123_456);
        for input in [
            "123457",  // one digit off
            "654321",  // transposed
            "12345",   // too short
            "1234567", // too long
            "",        // empty
            "12345a",  // not a digit
            "123.456", // unsupported separator
        ] {
            assert!(!code.matches_input(input), "should reject {input:?}");
        }
    }

    #[test]
    fn leading_zeros_are_preserved_through_input_parsing() {
        // "000042" must not be read as 42-with-two-digits and rejected, nor as a
        // different code. Leading-zero handling is a classic off-by-one in this pattern.
        let code = PairingCode(42);
        assert!(code.matches_input("000042"));
        assert!(code.matches_input("000 042"));
        assert!(!code.matches_input("42"));
        assert_eq!(code.to_display_string(), "000 042");
    }

    #[test]
    fn derivation_is_sensitive_to_every_exporter_byte() {
        let base = exporter(5);
        let baseline = PairingCode::from_exporter(&base);
        let mut differing = 0;
        for i in 0..EXPORTER_LEN {
            let mut flipped = base;
            flipped[i] ^= 0x01;
            if PairingCode::from_exporter(&flipped) != baseline {
                differing += 1;
            }
        }
        // With a 20-bit output, ~1 in a million flips could collide by chance; requiring
        // all 32 to differ is safe and catches a derivation that ignores part of its input.
        assert_eq!(differing, EXPORTER_LEN, "every exporter byte must affect the code");
    }
}
