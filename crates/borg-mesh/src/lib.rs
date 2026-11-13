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
//! - **Cache** (cache.rs): what a node's sessions read of its store, shared between them: each log read
//!   whole once and then only past where it was read, so a session's reads grow with what changed.
//! - **Live** (live.rs): presence of the members' drones, ephemeral, carried in every session after
//!   the logs; never reconciled into a log.
//! - **Steering** (steer.rs): a pause, resume or veto is a message line, authenticated by its origin;
//!   another node's counts only under its member role `admin` or `steer`, admitted when stored.
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
pub mod cache;
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

/// Lowercase hex, the form keys and hashes take in files, tickets and on the wire. Written into one
/// string sized up front, each byte through a table (a `format!` per byte was a string per byte).
pub fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(2 * bytes.len());
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    out
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

/// The bytes of exactly `2 · N` lowercase hex digits, the form `to_hex` writes; `None` for any other text
/// (which `from_hex` may still read: it trims and takes capitals).
pub(crate) fn from_lower_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let b = s.as_bytes();
    if b.len() != 2 * N {
        return None;
    }
    let nibble = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (nibble(b[2 * i])? << 4) | nibble(b[2 * i + 1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    /// Lowercase hex reads back exactly what `to_hex` wrote, and only that.
    #[test]
    fn lower_hex_reads_only_what_to_hex_writes() {
        let all: Vec<u8> = (0..=255).collect();
        for b in &all {
            assert_eq!(super::from_lower_hex::<1>(&super::to_hex(&[*b])), Some([*b]));
        }
        for bad in ["AB", " ab", "ab ", "a", "abc", "zz", "é"] {
            assert_eq!(super::from_lower_hex::<1>(bad), None, "{bad:?}");
        }
        assert_eq!(super::from_hex::<1>("AB"), Ok([0xab]), "which from_hex reads");
    }

    /// The table writes what `format!("{b:02x}")` wrote, for every byte.
    #[test]
    fn hex_is_what_format_wrote() {
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(super::to_hex(&all), all.iter().map(|b| format!("{b:02x}")).collect::<String>());
        assert_eq!(super::to_hex(&[]), "");
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(super::from_hex::<4>(&super::to_hex(&b)).unwrap(), b);
        assert!(super::from_hex::<4>("00").is_err());
        assert!(super::from_hex::<1>("zz").is_err());
    }
}
