//! Storage backends for the node's private signing key.
//!
//! The [`KeyStore`] trait is the seam that lets SecureMesh move from a
//! software-only key file to hardware-backed key storage without touching the
//! rest of the core. Planned future implementations:
//!
//! - `TpmKeyStore` — key sealed to a TPM 2.0 PCR policy, signing performed by
//!   the TPM so the private key never enters process memory.
//! - `SecureElementKeyStore` — key generated inside and non-extractable from a
//!   discrete secure element.
//! - `TeeKeyStore` — key held inside a trusted execution environment.
//!
//! Only [`FileKeyStore`] exists today. Its security properties are limited and
//! documented honestly on the type itself and in `docs/security/SECURITY.md`.

use crate::error::{CoreError, CoreResult};
use crate::security::Secret;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

/// Length of an Ed25519 secret scalar seed.
pub const SECRET_KEY_LEN: usize = 32;

/// On-disk schema version, so a future migration can recognise old key files.
const KEYFILE_VERSION: u32 = 1;
const KEYFILE_ALGORITHM: &str = "ed25519";

/// A private key together with the moment its identity was created.
pub struct StoredKey {
    pub secret: Secret<SECRET_KEY_LEN>,
    pub created_at: DateTime<Utc>,
}

/// Written by hand rather than derived, so that the key stays redacted even if
/// `Secret`'s own formatting is ever changed.
impl fmt::Debug for StoredKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredKey")
            .field("secret", &"<redacted>")
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// A place the node's private key can live.
///
/// Implementations must never log, display, or transmit the secret.
pub trait KeyStore: Send + Sync {
    /// Returns the stored key, or `Ok(None)` if this node has no identity yet.
    fn load(&self) -> CoreResult<Option<StoredKey>>;

    /// Persists a newly generated key. Must fail rather than overwrite an
    /// identity that already exists.
    fn store(&self, key: &StoredKey) -> CoreResult<()>;

    /// Short backend name, surfaced in the UI so an operator can tell at a
    /// glance whether keys are software-held or hardware-held.
    fn backend_name(&self) -> &'static str;

    /// Whether the private key is protected by dedicated security hardware.
    /// `FileKeyStore` answers `false`; this is what stops the UI from ever
    /// claiming hardware protection that does not exist.
    fn is_hardware_backed(&self) -> bool;
}

/// The serialised form of the key file.
///
/// `Drop` scrubs the hex-encoded secret, which is otherwise a second plaintext
/// copy of the key sitting in the heap.
#[derive(Serialize, Deserialize)]
struct KeyFile {
    version: u32,
    algorithm: String,
    created_at: DateTime<Utc>,
    secret_key: String,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        self.secret_key.zeroize();
    }
}

/// Stores the private key in a JSON file inside the node's data directory.
///
/// # Security properties — read before relying on this
///
/// **What it does protect against:** other *unprivileged local users*. On Unix
/// the file is created with mode `0600`. On Windows it inherits the ACL of the
/// per-user application data directory, which by default grants access only to
/// the owning user, SYSTEM, and Administrators.
///
/// **What it does NOT protect against:** the key is stored *unencrypted at
/// rest*. Anyone who can read the file as that user — malware running in the
/// user's session, an attacker with the disk image and no full-disk
/// encryption, or a backup process — recovers the private key. There is no
/// passphrase and no hardware binding.
///
/// This is a deliberate, documented Phase 1 limitation rather than a
/// security control being claimed. It is superseded by the TPM- and TEE-backed
/// implementations described in `docs/architecture/ROADMAP.md`.
pub struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl KeyStore for FileKeyStore {
    fn load(&self) -> CoreResult<Option<StoredKey>> {
        if !self.path.exists() {
            return Ok(None);
        }

        // The file contents are secret; scrub the buffer before returning.
        let mut raw = std::fs::read_to_string(&self.path)
            .map_err(|e| CoreError::identity(format!("could not read keystore ({})", e.kind())))?;
        let parsed = serde_json::from_str::<KeyFile>(&raw);
        raw.zeroize();

        let key_file =
            parsed.map_err(|_| CoreError::identity("keystore file is malformed or corrupt"))?;

        if key_file.version != KEYFILE_VERSION {
            return Err(CoreError::identity(format!(
                "unsupported keystore version {}",
                key_file.version
            )));
        }
        if key_file.algorithm != KEYFILE_ALGORITHM {
            return Err(CoreError::identity(
                "keystore holds a key of an unsupported algorithm",
            ));
        }

        let mut decoded = hex::decode(&key_file.secret_key)
            .map_err(|_| CoreError::identity("keystore key is not valid hex"))?;
        if decoded.len() != SECRET_KEY_LEN {
            decoded.zeroize();
            return Err(CoreError::identity("keystore key has an invalid length"));
        }

        let mut secret = [0u8; SECRET_KEY_LEN];
        secret.copy_from_slice(&decoded);
        decoded.zeroize();

        Ok(Some(StoredKey {
            secret: Secret::new(secret),
            created_at: key_file.created_at,
        }))
    }

    fn store(&self, key: &StoredKey) -> CoreResult<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::identity(format!("could not create data directory ({})", e.kind()))
            })?;
        }

        let key_file = KeyFile {
            version: KEYFILE_VERSION,
            algorithm: KEYFILE_ALGORITHM.to_string(),
            created_at: key.created_at,
            secret_key: hex::encode(key.secret.expose()),
        };
        let mut serialized = serde_json::to_string_pretty(&key_file)?;

        // `create_new` makes this fail if an identity already exists, so a bug
        // elsewhere can never silently destroy this node's identity.
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let result = options.open(&self.path).and_then(|mut file| {
            file.write_all(serialized.as_bytes())?;
            file.sync_all()
        });
        serialized.zeroize();

        result.map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => {
                CoreError::identity("refusing to overwrite an existing node identity")
            }
            kind => CoreError::identity(format!("could not write keystore ({})", kind)),
        })
    }

    fn backend_name(&self) -> &'static str {
        "software-file"
    }

    fn is_hardware_backed(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_key() -> StoredKey {
        StoredKey {
            secret: Secret::new([0x42u8; SECRET_KEY_LEN]),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn load_returns_none_when_no_identity_exists() {
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("identity.json"));
        assert!(store.load().unwrap().is_none());
    }

    #[test]
    fn stored_key_round_trips() {
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("identity.json"));
        let original = sample_key();
        store.store(&original).unwrap();

        let loaded = store.load().unwrap().expect("identity should exist");
        assert_eq!(loaded.secret.expose(), original.secret.expose());
        assert_eq!(
            loaded.created_at.timestamp_millis(),
            original.created_at.timestamp_millis()
        );
    }

    #[test]
    fn store_creates_missing_parent_directories() {
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("nested").join("deep").join("identity.json"));
        store.store(&sample_key()).unwrap();
        assert!(store.path().exists());
    }

    #[test]
    fn store_refuses_to_overwrite_an_existing_identity() {
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("identity.json"));
        store.store(&sample_key()).unwrap();

        let err = store.store(&sample_key()).unwrap_err();
        assert_eq!(err.code(), "IDENTITY_ERROR");
        assert!(err.message().contains("refusing to overwrite"));
    }

    #[test]
    fn corrupt_keystore_is_rejected_without_echoing_contents() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.json");
        std::fs::write(&path, "{ not valid json 'deadbeefsecret'").unwrap();

        let err = FileKeyStore::new(&path).load().unwrap_err();
        assert_eq!(err.code(), "IDENTITY_ERROR");
        assert!(!err.message().contains("deadbeefsecret"));
    }

    #[test]
    fn keystore_with_wrong_key_length_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.json");
        std::fs::write(
            &path,
            r#"{"version":1,"algorithm":"ed25519","created_at":"2026-01-01T00:00:00Z","secret_key":"aabb"}"#,
        )
        .unwrap();

        let err = FileKeyStore::new(&path).load().unwrap_err();
        assert!(err.message().contains("invalid length"));
    }

    #[test]
    fn keystore_from_a_future_version_is_rejected() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("identity.json");
        let secret = hex::encode([1u8; SECRET_KEY_LEN]);
        std::fs::write(
            &path,
            format!(
                r#"{{"version":99,"algorithm":"ed25519","created_at":"2026-01-01T00:00:00Z","secret_key":"{secret}"}}"#
            ),
        )
        .unwrap();

        let err = FileKeyStore::new(&path).load().unwrap_err();
        assert!(err.message().contains("unsupported keystore version"));
    }

    #[test]
    fn file_backend_never_claims_hardware_protection() {
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("identity.json"));
        assert!(!store.is_hardware_backed());
        assert_eq!(store.backend_name(), "software-file");
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_not_readable_by_other_users() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let store = FileKeyStore::new(dir.path().join("identity.json"));
        store.store(&sample_key()).unwrap();

        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "group/other bits must be clear");
    }
}
