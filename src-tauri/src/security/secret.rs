//! A container for byte strings that must never be printed, logged, or
//! serialised.
//!
//! `Secret` provides three guarantees:
//!
//! 1. **No accidental disclosure via formatting.** `Debug` and `Display` both
//!    render a fixed redaction marker, so a stray `println!("{:?}", ..)` or a
//!    `#[derive(Debug)]` on an enclosing struct cannot leak the contents.
//! 2. **No accidental disclosure via serialisation.** `Secret` intentionally
//!    does *not* implement `serde::Serialize`. Any struct holding a `Secret`
//!    therefore fails to compile if someone tries to derive `Serialize` on it,
//!    which is what keeps private keys out of Tauri command responses.
//! 3. **Best-effort erasure.** The buffer is overwritten on drop via `zeroize`.
//!
//! What it does *not* do: protect against an attacker who can already read this
//! process's memory, or prevent the OS from paging the buffer to disk. Those
//! require the hardware-backed protections tracked in Phase 5.

use std::fmt;
use zeroize::Zeroize;

/// Owned secret bytes with redacted formatting and zero-on-drop.
pub struct Secret<const N: usize> {
    bytes: [u8; N],
}

impl<const N: usize> Secret<N> {
    /// Takes ownership of `bytes`. The caller should avoid keeping any other
    /// copy of the material alive.
    pub fn new(bytes: [u8; N]) -> Self {
        Self { bytes }
    }

    /// Borrows the raw material. Every call site is a disclosure risk, so this
    /// is intentionally verbose and should stay confined to the identity layer.
    pub fn expose(&self) -> &[u8; N] {
        &self.bytes
    }

    pub const fn len(&self) -> usize {
        N
    }

    pub const fn is_empty(&self) -> bool {
        N == 0
    }
}

impl<const N: usize> fmt::Debug for Secret<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secret<{}>(<redacted>)", N)
    }
}

impl<const N: usize> fmt::Display for Secret<N> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl<const N: usize> Drop for Secret<N> {
    fn drop(&mut self) {
        self.bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_reveals_contents() {
        let secret = Secret::new([0xABu8; 32]);
        let rendered = format!("{:?}", secret);
        assert_eq!(rendered, "Secret<32>(<redacted>)");
        assert!(!rendered.contains("ab"));
        assert!(!rendered.contains("171"));
    }

    #[test]
    fn display_never_reveals_contents() {
        let secret = Secret::new([0x7Fu8; 32]);
        assert_eq!(format!("{}", secret), "<redacted>");
    }

    #[test]
    fn debug_of_enclosing_struct_is_also_redacted() {
        #[derive(Debug)]
        #[allow(dead_code)]
        struct Wrapper {
            label: &'static str,
            key: Secret<32>,
        }

        let wrapper = Wrapper {
            label: "signing",
            key: Secret::new([0x11u8; 32]),
        };
        let rendered = format!("{:?}", wrapper);
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("17"));
    }

    #[test]
    fn expose_returns_the_original_material() {
        let secret = Secret::new([9u8; 32]);
        assert_eq!(secret.expose(), &[9u8; 32]);
        assert_eq!(secret.len(), 32);
        assert!(!secret.is_empty());
    }
}
