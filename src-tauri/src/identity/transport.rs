//! The bridge between a SecureMesh identity and the libp2p transport identity.
//!
//! This file exists so that the one place key material leaves the identity
//! module is explicit, isolated, and easy to find. Everything else in the core
//! reaches the private key only through [`NodeIdentity::sign`].
//!
//! # Why the same key
//!
//! libp2p authenticates a peer by proving possession of the private key behind
//! its `PeerId`. If that key is the *same* Ed25519 key the node signs events
//! with, then a completed QUIC session already proves the peer is the
//! SecureMesh node it claims to be, and the node ID follows from the public
//! key by the same SHA-256 derivation used everywhere else.
//!
//! The alternative — a separate transport key bound to the identity key by a
//! signed certificate — would mean designing and implementing custom
//! cryptography to prove something the transport handshake already proves.
//! Reusing the key removes that code, and with it the chance of getting it
//! wrong.
//!
//! # The hazard, and how it is handled
//!
//! Using one key for two protocols is a genuine cross-protocol risk: a
//! signature produced in one context must never be valid in another. Both
//! sides are domain-separated:
//!
//! | Context | Prefix |
//! |---|---|
//! | libp2p handshake | libp2p's own (`noise-libp2p-static-key:`, TLS labels) |
//! | SecureMesh events | [`crate::domain::event::EVENT_SIGNING_DOMAIN`] |
//! | SecureMesh envelopes | [`crate::networking::protocol::ENVELOPE_SIGNING_DOMAIN`] |
//!
//! Since every SecureMesh signature covers a distinct, length-prefixed prefix,
//! no signature is transferable between contexts.

use super::NodeIdentity;
use crate::error::{CoreError, CoreResult};
use zeroize::Zeroize;

impl NodeIdentity {
    /// Derives the libp2p identity keypair for this node.
    ///
    /// This is the **only** sanctioned path by which private key material
    /// leaves the identity module, and it hands the key to a type that keeps
    /// it encapsulated rather than returning raw bytes. The intermediate copy
    /// is zeroised as soon as libp2p has taken ownership.
    ///
    /// A future hardware-backed `KeyStore` will not be able to implement this,
    /// because a non-extractable key cannot be handed to libp2p at all. That is
    /// a known constraint of the Phase 2 design and is recorded in
    /// `docs/security/SECURITY.md`.
    pub fn libp2p_keypair(&self) -> CoreResult<libp2p::identity::Keypair> {
        let mut seed = self.signing_key.to_bytes();
        let keypair = libp2p::identity::Keypair::ed25519_from_bytes(&mut seed);
        // `ed25519_from_bytes` takes the buffer by `AsMut` and libp2p already
        // clears it, but doing so here too makes the guarantee local and
        // independent of that implementation detail.
        seed.zeroize();

        keypair.map_err(|_| CoreError::identity("could not derive the transport identity"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keystore::FileKeyStore;
    use tempfile::TempDir;

    fn identity(dir: &TempDir) -> NodeIdentity {
        NodeIdentity::load_or_create(&FileKeyStore::new(dir.path().join("id.json"))).unwrap()
    }

    #[test]
    fn the_transport_identity_is_derived_from_the_node_key() {
        let dir = TempDir::new().unwrap();
        let node = identity(&dir);

        let keypair = node.libp2p_keypair().unwrap();
        let public = keypair.public().try_into_ed25519().unwrap();

        // This equality is the whole basis of peer authentication: whatever
        // libp2p proves about the PeerId, it proves about the SecureMesh node.
        assert_eq!(hex::encode(public.to_bytes()), node.public_key_hex());
    }

    #[test]
    fn the_peer_id_maps_back_to_the_secure_mesh_node_id() {
        let dir = TempDir::new().unwrap();
        let node = identity(&dir);

        let keypair = node.libp2p_keypair().unwrap();
        let public = keypair.public().try_into_ed25519().unwrap();
        let recovered = crate::identity::node_id_for_public_key(&hex::encode(public.to_bytes()));

        assert_eq!(recovered, node.node_id());
    }

    #[test]
    fn the_transport_identity_is_stable_across_calls() {
        let dir = TempDir::new().unwrap();
        let node = identity(&dir);

        let first = node.libp2p_keypair().unwrap().public().to_peer_id();
        let second = node.libp2p_keypair().unwrap().public().to_peer_id();
        assert_eq!(first, second);
    }

    #[test]
    fn the_transport_identity_survives_a_restart() {
        let dir = TempDir::new().unwrap();

        let first = identity(&dir)
            .libp2p_keypair()
            .unwrap()
            .public()
            .to_peer_id();
        // A reloaded identity must present the same peer ID, or peers would see
        // a different node after every restart.
        let second = identity(&dir)
            .libp2p_keypair()
            .unwrap()
            .public()
            .to_peer_id();
        assert_eq!(first, second);
    }

    #[test]
    fn distinct_nodes_get_distinct_transport_identities() {
        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();

        assert_ne!(
            identity(&dir_a)
                .libp2p_keypair()
                .unwrap()
                .public()
                .to_peer_id(),
            identity(&dir_b)
                .libp2p_keypair()
                .unwrap()
                .public()
                .to_peer_id()
        );
    }
}
