//! A node's identity in one collective: an ed25519 key in `<store>/mesh/node.key` (hex seed, 0600).
//!
//! Per collective, not per machine: membership of the work collective says nothing about a
//! private one, and a key never has to be shared between two minds. The same seed is the node's
//! iroh secret, so the id the transport authenticates is the key the membership lists.

use std::io::Write;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::{from_hex, to_hex};

pub struct Identity {
    key: SigningKey,
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("mesh")
}

pub fn path(home: &Path) -> PathBuf {
    dir(home).join("node.key")
}

/// `<store>/mesh/node.pub`: the public key, hex, for borg itself, which signs nothing: its messages
/// name their node by it (borg msg.rs), and the mesh carries a message only under that key's origin
/// (lines.rs `travels`). borg owns the path, since it reads the key.
pub fn public_path(home: &Path) -> PathBuf {
    borg::msg::node_pub_path(home)
}

impl Identity {
    /// The node's key for the collective stored at `home`, created on first use.
    /// Also writes `public_path` when it is missing.
    pub fn load_or_create(home: &Path) -> Result<Identity, String> {
        let p = path(home);
        let me = match std::fs::read_to_string(&p) {
            Ok(t) => Identity { key: SigningKey::from_bytes(&from_hex::<32>(&t).map_err(|e| format!("{}: {e}", p.display()))?) },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let seed = iroh::SecretKey::generate().to_bytes();
                write_private(&p, &to_hex(&seed))?;
                Identity { key: SigningKey::from_bytes(&seed) }
            }
            Err(e) => return Err(format!("{}: {e}", p.display())),
        };
        me.publish(home)?;
        Ok(me)
    }

    /// Writes `public_path` unless it already names this key: whole, through a rename, so a reader
    /// never sees half a key.
    fn publish(&self, home: &Path) -> Result<(), String> {
        let (p, key) = (public_path(home), self.public());
        if std::fs::read_to_string(&p).is_ok_and(|t| t.trim() == key) {
            return Ok(());
        }
        let tmp = p.with_extension(format!("pub.{}.tmp", std::process::id()));
        std::fs::write(&tmp, format!("{key}\n")).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
    }

    /// The public key, hex: what `borg mesh id` prints and an admin invites.
    pub fn public(&self) -> String {
        to_hex(self.key.verifying_key().as_bytes())
    }

    pub fn sign(&self, msg: &[u8]) -> String {
        to_hex(&self.key.sign(msg).to_bytes())
    }

    pub fn iroh_secret(&self) -> iroh::SecretKey {
        iroh::SecretKey::from_bytes(&self.key.to_bytes())
    }
}

/// Whether `key` (hex) is a valid ed25519 public key.
pub fn check_key(key: &str) -> Result<VerifyingKey, String> {
    VerifyingKey::from_bytes(&from_hex::<32>(key)?).map_err(|e| format!("bad key {key}: {e}"))
}

/// Whether `sig` (hex) is `key`'s (hex) signature of `msg`.
pub fn verify(key: &str, msg: &[u8], sig: &str) -> Result<(), String> {
    let k = check_key(key)?;
    let s = Signature::from_bytes(&from_hex::<64>(sig)?);
    k.verify_strict(msg, &s).map_err(|_| "bad signature".to_string())
}

/// Created 0600 inside a 0700 directory, never overwritten: a key that leaks is a member that leaks.
fn write_private(p: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let d = p.parent().unwrap_or(Path::new("."));
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(d).map_err(|e| format!("{}: {e}", d.display()))?;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("{}: {e}", p.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn created_once_private_and_stable() {
        let home = std::env::temp_dir().join(format!("borg-mesh-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let a = Identity::load_or_create(&home).unwrap();
        let b = Identity::load_or_create(&home).unwrap();
        assert_eq!(a.public(), b.public(), "the key is created once");
        assert_eq!(std::fs::read_to_string(public_path(&home)).unwrap().trim(), a.public(), "published for borg");
        assert_eq!(std::fs::metadata(path(&home)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(to_hex(a.iroh_secret().public().as_bytes()), a.public(), "iroh id == membership key");
        let sig = a.sign(b"hello");
        assert!(verify(&a.public(), b"hello", &sig).is_ok());
        assert!(verify(&a.public(), b"hellO", &sig).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }
}
