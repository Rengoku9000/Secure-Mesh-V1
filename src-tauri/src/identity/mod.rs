//! The cryptographic identity of a SecureMesh node.
//!
//! Every node owns an Ed25519 keypair generated on first launch and reused for
//! the lifetime of the node. The identity is *derived from* the key material
//! rather than assigned: both the node ID and the human-readable node name are
//! functions of the public key, so a node cannot claim an identity it does not
//! hold the private key for.
//!
//! ```text
//!   Ed25519 public key (32 bytes)
//!            |
//!         SHA-256
//!            |
//!   node_id  = 64-char lowercase hex   e.g. "a7f32c9e..."
//!            |
//!   node_name = "SM-" + first 5 hex chars, uppercased   e.g. "SM-A7F32"
//! ```
//!
//! # Private key handling
//!
//! The [`SigningKey`] lives inside [`NodeIdentity`] and never leaves it:
//!
//! - `NodeIdentity` does not implement `Serialize`, so it cannot cross the
//!   Tauri IPC boundary. Commands return [`PublicIdentity`] instead.
//! - `Debug` is implemented by hand and omits the key.
//! - Signing happens *inside* this module; callers pass a message and receive
//!   a signature, never the key.

pub mod keystore;
mod transport;

use crate::error::{CoreError, CoreResult};
use crate::security::{audit, AuditEvent, AuditOutcome, Secret};
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use keystore::{KeyStore, StoredKey, SECRET_KEY_LEN};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fmt;

/// Number of hex characters of the fingerprint used in the display name.
const NODE_NAME_HEX_CHARS: usize = 5;

/// The algorithm label reported to the UI and written to the keystore.
pub const IDENTITY_ALGORITHM: &str = "Ed25519";

/// A node's full identity, including the private signing key.
///
/// Intentionally **not** `Serialize` and **not** `Clone`.
pub struct NodeIdentity {
    node_id: String,
    node_name: String,
    signing_key: SigningKey,
    verifying_key: VerifyingKey,
    created_at: DateTime<Utc>,
    key_backend: &'static str,
    hardware_backed: bool,
}

impl NodeIdentity {
    /// Loads this node's identity, generating one on first launch.
    ///
    /// A freshly generated key is persisted before it is used, so a crash
    /// between generation and first use cannot leave the node with an identity
    /// it will not recognise next time.
    pub fn load_or_create(store: &dyn KeyStore) -> CoreResult<Self> {
        match store.load() {
            Ok(Some(stored)) => {
                let identity = Self::from_stored(stored, store)?;
                audit(
                    AuditEvent::IdentityLoaded,
                    AuditOutcome::Success,
                    &format!(
                        "node={} backend={}",
                        identity.node_name,
                        store.backend_name()
                    ),
                );
                Ok(identity)
            }
            Ok(None) => {
                let identity = Self::generate(store)?;
                audit(
                    AuditEvent::IdentityCreated,
                    AuditOutcome::Success,
                    &format!(
                        "node={} backend={}",
                        identity.node_name,
                        store.backend_name()
                    ),
                );
                Ok(identity)
            }
            Err(err) => {
                audit(
                    AuditEvent::IdentityLoaded,
                    AuditOutcome::Failure,
                    err.message(),
                );
                Err(err)
            }
        }
    }

    /// Generates a new keypair from the operating system CSPRNG and persists it.
    fn generate(store: &dyn KeyStore) -> CoreResult<Self> {
        let mut seed = [0u8; SECRET_KEY_LEN];
        getrandom::fill(&mut seed).map_err(|_| {
            CoreError::identity("operating system random number generator unavailable")
        })?;

        let stored = StoredKey {
            secret: Secret::new(seed),
            created_at: Utc::now(),
        };
        store.store(&stored)?;
        Self::from_stored(stored, store)
    }

    fn from_stored(stored: StoredKey, store: &dyn KeyStore) -> CoreResult<Self> {
        let signing_key = SigningKey::from_bytes(stored.secret.expose());
        let verifying_key = signing_key.verifying_key();
        let node_id = fingerprint(verifying_key.as_bytes());
        let node_name = node_name_from_fingerprint(&node_id);

        Ok(Self {
            node_id,
            node_name,
            signing_key,
            verifying_key,
            created_at: stored.created_at,
            key_backend: store.backend_name(),
            hardware_backed: store.is_hardware_backed(),
        })
    }

    /// Stable, cryptographically derived node identifier (64 hex characters).
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Short operator-facing name, e.g. `SM-A7F32`.
    pub fn node_name(&self) -> &str {
        &self.node_name
    }

    /// Hex-encoded Ed25519 public key. Safe to publish and to send to peers.
    pub fn public_key_hex(&self) -> String {
        hex::encode(self.verifying_key.as_bytes())
    }

    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// Signs a message with the node's private key.
    ///
    /// This is the only way callers can use the private key, and it is what
    /// will authenticate this node to peers once Phase 2 networking lands.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        self.signing_key.sign(message).to_bytes()
    }

    /// Verifies a signature made by *this* node.
    pub fn verify(&self, message: &[u8], signature: &[u8; 64]) -> bool {
        self.verifying_key
            .verify(message, &Signature::from_bytes(signature))
            .is_ok()
    }

    /// The safe projection of this identity, suitable for the UI and for peers.
    pub fn public(&self) -> PublicIdentity {
        PublicIdentity {
            node_id: self.node_id.clone(),
            node_name: self.node_name.clone(),
            public_key: self.public_key_hex(),
            algorithm: IDENTITY_ALGORITHM,
            created_at: self.created_at,
            key_backend: self.key_backend,
            hardware_backed: self.hardware_backed,
        }
    }
}

/// Hand-written so the signing key can never reach a log line.
impl fmt::Debug for NodeIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeIdentity")
            .field("node_id", &self.node_id)
            .field("node_name", &self.node_name)
            .field("public_key", &self.public_key_hex())
            .field("created_at", &self.created_at)
            .field("signing_key", &"<redacted>")
            .finish()
    }
}

/// The public, shareable half of a node identity.
///
/// This is what Tauri commands return. It contains no secret material.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicIdentity {
    pub node_id: String,
    pub node_name: String,
    /// Hex-encoded Ed25519 public key.
    pub public_key: String,
    pub algorithm: &'static str,
    pub created_at: DateTime<Utc>,
    /// Which keystore backend holds the private key.
    pub key_backend: &'static str,
    /// True only when the private key is held in dedicated security hardware.
    /// Phase 1 always reports `false`.
    pub hardware_backed: bool,
}

/// Verifies a signature against an arbitrary peer's hex-encoded public key.
///
/// Used by the sync layer in Phase 2 to authenticate records that arrive from
/// other nodes. Every input is untrusted, so every failure path returns
/// `false` rather than panicking.
pub fn verify_with_public_key(public_key_hex: &str, message: &[u8], signature: &[u8]) -> bool {
    let Ok(key_bytes) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(signature) else {
        return false;
    };
    verifying_key
        .verify(message, &Signature::from_bytes(&sig_array))
        .is_ok()
}

/// Derives the SecureMesh node ID from a hex-encoded public key.
///
/// Returns an empty string for input that is not a valid key, which cannot
/// match any real node ID and so fails closed at every comparison.
pub fn node_id_for_public_key(public_key_hex: &str) -> String {
    match hex::decode(public_key_hex) {
        Ok(bytes) if bytes.len() == 32 => fingerprint(&bytes),
        _ => String::new(),
    }
}

/// Whether a node ID is genuinely the fingerprint of a public key.
///
/// The binding between the two is what stops a peer attaching someone else's
/// identity to its own key, so it is re-checked wherever an identity crosses a
/// trust boundary rather than being assumed from the caller.
pub fn node_id_matches_key(node_id: &str, public_key_hex: &str) -> bool {
    let Ok(key_bytes) = hex::decode(public_key_hex) else {
        return false;
    };
    if key_bytes.len() != 32 {
        return false;
    }
    fingerprint(&key_bytes) == node_id
}

/// SHA-256 fingerprint of a public key, lowercase hex.
fn fingerprint(public_key: &[u8]) -> String {
    hex::encode(Sha256::digest(public_key))
}

/// Derives the operator-facing node name for any node ID.
///
/// Exposed so that a peer learned over the mesh gets exactly the same name it
/// calls itself — the name is a function of the ID, never something a node
/// announces and could therefore lie about.
pub fn node_name_for(node_id: &str) -> String {
    node_name_from_fingerprint(node_id)
}

/// Derives the operator-facing node name from a fingerprint.
fn node_name_from_fingerprint(fingerprint: &str) -> String {
    let suffix: String = fingerprint
        .chars()
        .take(NODE_NAME_HEX_CHARS)
        .flat_map(|c| c.to_uppercase())
        .collect();
    format!("SM-{}", suffix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use keystore::FileKeyStore;
    use tempfile::TempDir;

    fn store_in(dir: &TempDir) -> FileKeyStore {
        FileKeyStore::new(dir.path().join("identity.json"))
    }

    #[test]
    fn generates_an_identity_on_first_launch() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();

        assert_eq!(identity.node_id().len(), 64);
        assert!(identity.node_id().chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(identity.public_key_hex().len(), 64);
    }

    #[test]
    fn identity_persists_across_restart() {
        let dir = TempDir::new().unwrap();

        let first = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();
        let first_id = first.node_id().to_string();
        let first_key = first.public_key_hex();
        let first_name = first.node_name().to_string();
        drop(first);

        // A second `load_or_create` models the next application launch.
        let second = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();
        assert_eq!(second.node_id(), first_id);
        assert_eq!(second.public_key_hex(), first_key);
        assert_eq!(second.node_name(), first_name);
    }

    #[test]
    fn distinct_nodes_get_distinct_identities() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        let a = NodeIdentity::load_or_create(&store_in(&dir_a)).unwrap();
        let b = NodeIdentity::load_or_create(&store_in(&dir_b)).unwrap();

        assert_ne!(a.node_id(), b.node_id());
        assert_ne!(a.public_key_hex(), b.public_key_hex());
    }

    #[test]
    fn node_name_is_derived_from_the_public_key() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();

        let expected = format!("SM-{}", identity.node_id()[..5].to_uppercase());
        assert_eq!(identity.node_name(), expected);
        assert!(identity.node_name().starts_with("SM-"));
        assert_eq!(identity.node_name().len(), 3 + NODE_NAME_HEX_CHARS);
    }

    #[test]
    fn node_id_is_the_sha256_of_the_public_key() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();

        let key_bytes = hex::decode(identity.public_key_hex()).unwrap();
        assert_eq!(identity.node_id(), hex::encode(Sha256::digest(&key_bytes)));
    }

    #[test]
    fn signatures_verify_against_the_node_public_key() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();

        let message = b"incident:flood at sector 7";
        let signature = identity.sign(message);

        assert!(identity.verify(message, &signature));
        assert!(verify_with_public_key(
            &identity.public_key_hex(),
            message,
            &signature
        ));
    }

    #[test]
    fn tampered_messages_fail_verification() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();

        let signature = identity.sign(b"severity:LOW");
        assert!(!identity.verify(b"severity:CRITICAL", &signature));
    }

    #[test]
    fn a_signature_from_one_node_does_not_verify_for_another() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let a = NodeIdentity::load_or_create(&store_in(&dir_a)).unwrap();
        let b = NodeIdentity::load_or_create(&store_in(&dir_b)).unwrap();

        let signature = a.sign(b"report");
        assert!(!b.verify(b"report", &signature));
    }

    #[test]
    fn verification_rejects_malformed_input_without_panicking() {
        assert!(!verify_with_public_key("not-hex", b"m", &[0u8; 64]));
        assert!(!verify_with_public_key("aabb", b"m", &[0u8; 64]));
        assert!(!verify_with_public_key(&hex::encode([0u8; 32]), b"m", &[]));
        assert!(!verify_with_public_key("", b"m", &[0u8; 64]));
    }

    // --- Private key protection -------------------------------------------

    #[test]
    fn debug_output_never_contains_the_private_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.json");
        let identity = NodeIdentity::load_or_create(&FileKeyStore::new(&path)).unwrap();

        // Read the real secret straight off disk, then assert it is absent.
        let raw = std::fs::read_to_string(&path).unwrap();
        let key_file: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let secret_hex = key_file["secret_key"].as_str().unwrap();

        let rendered = format!("{:?}", identity);
        assert!(!rendered.contains(secret_hex));
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains(identity.node_name()));
    }

    #[test]
    fn public_identity_never_contains_the_private_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.json");
        let identity = NodeIdentity::load_or_create(&FileKeyStore::new(&path)).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let key_file: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let secret_hex = key_file["secret_key"].as_str().unwrap();

        let json = serde_json::to_string(&identity.public()).unwrap();
        assert!(!json.contains(secret_hex));
        assert!(json.contains(&identity.public_key_hex()));
    }

    #[test]
    fn public_identity_reports_software_key_storage_in_phase_1() {
        let dir = TempDir::new().unwrap();
        let identity = NodeIdentity::load_or_create(&store_in(&dir)).unwrap();
        let public = identity.public();

        assert!(!public.hardware_backed);
        assert_eq!(public.key_backend, "software-file");
        assert_eq!(public.algorithm, "Ed25519");
    }
}
