//! The team mind without a server: the members of a collective sync its lessons peer to peer.
//!
//! - **Identity** (identity.rs): one ed25519 key per node and collective, kept in the collective's
//!   store (mode 0600). The same key is the node's iroh id, so the transport authenticates it.
//! - **Membership** (membership.rs): a JSON document listing member keys, roles and revocations,
//!   signed by an admin. A newer version is adopted only if it is the next one, names the held one
//!   as its parent and an admin of the held version signed it; concurrent admins' forks resolve the
//!   same way on every node, keeping every revocation. A node talks only to peers its verified
//!   membership lists.
//! - **Data** (lines.rs, origin.rs): the append-only `lessons.jsonl`, addressed per line by blake3,
//!   each line with a signed origin (who stamped it, when). Merge is a union; the latest version
//!   of a lesson wins, as `conscience::load` expects. Raw episodes stay on the node that captured
//!   them, so each is learned exactly once. Reasoning excerpts (git notes, `borg notes`) go from
//!   their author to two member replicas, and any holder answers a member asking for a commit's.
//! - **Sync** (sync.rs, recon.rs): iroh QUIC between two nodes, trust first (membership and
//!   revocations before any data), then range-based set reconciliation, cost ∝ difference.
//! - **Live** (live.rs, steer.rs, beats.rs): presence of the members' drones, signed steering
//!   directives and each hive goal's newest beat per node, ephemeral, carried in every session after
//!   the logs; never reconciled into a log.
//! - **Daemon** (daemon.rs, peers.rs): `serve` accepts members and dials the known ones every
//!   interval and on local change, with backoff, until Ctrl-C / SIGTERM.
//! - **Reach** (addr.rs): peers are dialled by key; across NAT through iroh's hole punching with
//!   relay fallback (n0's public relays and address lookup by default, a self-hosted relay, or
//!   direct addresses only), set per collective.
//! - **Onboarding** (invite.rs): an admin's signed, expiring, use-limited invite in a ticket string
//!   or QR code; `join <ticket>` dials the admin, which adds the key to the signed membership.
//!
//! The gate: `borg::collective::may_sync` is asked before any byte of a collective leaves, on both
//! sides, on the config as it is at that moment. A local collective or an off repo is refused before
//! a connection is even opened, and so is a store a local collective also maps to.
//!
//! **Internal API, no semver.** The product is the `borg-mesh` binary; this library exists for borg's own
//! crates, which pin it `=` to the same version. Any release may change or remove any item.

pub mod addr;
pub mod beats;
pub mod daemon;
pub mod identity;
pub mod invite;
pub mod lines;
pub mod live;
pub mod membership;
pub mod origin;
pub mod peers;
pub mod recon;
pub mod steer;
pub mod sync;

/// Lowercase hex, the form keys and hashes take in files, tickets and on the wire.
pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex<const N: usize>(s: &str) -> Result<[u8; N], String> {
    let s = s.trim();
    if s.len() != 2 * N || !s.is_ascii() {
        return Err(format!("expected {} hex characters, got {:?}", 2 * N, s));
    }
    let mut out = [0u8; N];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| format!("bad hex {s:?}: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(super::from_hex::<4>(&super::to_hex(&b)).unwrap(), b);
        assert!(super::from_hex::<4>("00").is_err());
        assert!(super::from_hex::<1>("zz").is_err());
    }
}
