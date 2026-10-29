//! Onboarding by ticket: `borg mesh invite` prints one string (and a QR code of it), `borg mesh
//! join <ticket>` on the new machine does the rest.
//!
//! - **Invite**: collective, the issuing admin's key, a random id, the role, an expiry; signed by
//!   that admin. The admin's node keeps what it issued in `<store>/mesh/invites.json` (uses left,
//!   who joined with it); the file, not the ticket, decides whether an invite is still good, so an
//!   admin can withdraw one (`borg mesh invites --revoke ID`) after handing it out.
//! - **Ticket**: the invite plus where the admin can be reached: its relay setting (so the joiner
//!   uses the same relays) and hints (direct addresses, its home relay; addr.rs). Encoded compactly
//!   as `BORG1` + base32 (uppercase, so a QR code uses its dense alphanumeric mode).
//! - **Redeem** (the admin's node, sync.rs `admit`): issued by this node, signature good, not
//!   expired, uses left, not withdrawn, the key not revoked. Then the key is added to the signed
//!   membership with the invite's role (membership.rs `invite`: the chain semantics are unchanged)
//!   and one use is spent. The key that spent an invite may present it again (a lost answer) and is
//!   welcomed without spending another use; any other key is refused once it is spent.
//!
//! A ticket is a bearer secret until it is spent: whoever holds it can join with the role it
//! carries. Hence one use and seven days by default.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Serialize};

use crate::addr::{self, Hint, Relay, Target};
use crate::identity::{self, Identity};
use crate::membership::Membership;
use crate::{from_hex, to_hex};

pub mod join_config;

pub const PREFIX: &str = "BORG1";
/// The roles a ticket may carry; admins are made by name (`borg mesh invite <key> --role admin`).
pub const ROLES: [&str; 3] = ["member", "steer", "observe"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    pub collective: String,
    /// The admin that issued it; only that admin's node redeems it.
    pub admin: String,
    /// Random, 16 bytes hex.
    pub id: String,
    pub role: String,
    pub expires_ms: i64,
    pub sig: String,
}

fn body(i: &Invite) -> Vec<u8> {
    let mut b = b"borg-invite-v1\n".to_vec();
    b.extend(serde_json::to_vec(&(&i.collective, &i.admin, &i.id, &i.role, i.expires_ms)).expect("invite serialises"));
    b
}

impl Invite {
    pub fn verify(&self) -> Result<(), String> {
        identity::verify(&self.admin, &body(self), &self.sig).map_err(|e| format!("invite: {e}"))
    }
}

/// What the admin's node keeps about an invite it issued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    pub role: String,
    pub expires_ms: i64,
    pub uses_left: u32,
    pub created_ms: i64,
    /// The keys that joined with it.
    #[serde(default)]
    pub joined: Vec<String>,
}

pub fn path(home: &Path) -> PathBuf {
    identity::dir(home).join("invites.json")
}

pub fn load(home: &Path) -> BTreeMap<String, Pending> {
    std::fs::read_to_string(path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save(home: &Path, all: &BTreeMap<String, Pending>) -> Result<(), String> {
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    let (p, tmp) = (path(home), path(home).with_extension("json.tmp"));
    std::fs::write(&tmp, serde_json::to_string_pretty(all).unwrap()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))
}

/// Issues an invite for `role`, valid for `ttl_ms` and `uses` joins; only an admin of `m` can.
pub fn issue(home: &Path, admin: &Identity, m: &Membership, role: &str, ttl_ms: i64, uses: u32, now: i64) -> Result<Invite, String> {
    m.verify()?;
    if m.role_of(&admin.public()) != Some("admin") {
        return Err(format!("only an admin of {} can invite", m.collective));
    }
    if !ROLES.contains(&role) {
        return Err(format!("role {role:?}: a ticket carries one of {}", ROLES.join(", ")));
    }
    if ttl_ms <= 0 || uses == 0 {
        return Err("an invite needs a positive lifetime and at least one use".into());
    }
    let id = to_hex(&blake3::hash(&iroh::SecretKey::generate().to_bytes()).as_bytes()[..16]);
    let mut inv = Invite { collective: m.collective.clone(), admin: admin.public(), id, role: role.into(), expires_ms: now + ttl_ms, sig: String::new() };
    inv.sig = admin.sign(&body(&inv));
    let mut all = load(home);
    all.insert(inv.id.clone(), Pending { role: role.into(), expires_ms: inv.expires_ms, uses_left: uses, created_ms: now, joined: Vec::new() });
    save(home, &all)?;
    Ok(inv)
}

/// The invites that can still be used, soonest to expire first.
pub fn pending(home: &Path, now: i64) -> Vec<(String, Pending)> {
    let mut v: Vec<(String, Pending)> = load(home).into_iter().filter(|(_, p)| p.uses_left > 0 && p.expires_ms > now).collect();
    v.sort_by_key(|(_, p)| p.expires_ms);
    v
}

/// Withdraws an invite (a unique prefix of its id is enough); a ticket for it is refused from now on.
pub fn withdraw(home: &Path, id: &str) -> Result<String, String> {
    let mut all = load(home);
    let hits: Vec<String> = all.keys().filter(|k| !id.is_empty() && k.starts_with(id)).cloned().collect();
    match hits.as_slice() {
        [one] => {
            all.remove(one);
            save(home, &all)?;
            Ok(one.clone())
        }
        [] => Err(format!("no invite {id}")),
        _ => Err(format!("{id} names {} invites: give more of the id", hits.len())),
    }
}

/// The admin's side of a join (module doc). Returns the new membership, saved.
pub fn redeem(home: &Path, admin: &Identity, collective: &str, inv: &Invite, joiner: &str, now: i64) -> Result<Membership, String> {
    if inv.collective != collective {
        return Err(format!("the invite is for {:?}, this node serves {collective:?}", inv.collective));
    }
    if inv.admin != admin.public() {
        return Err("the invite was issued by another admin's node: ask that admin".into());
    }
    inv.verify()?;
    let mut all = load(home);
    let Some(p) = all.get_mut(&inv.id) else { return Err("unknown or withdrawn invite".into()) };
    if p.role != inv.role || p.expires_ms != inv.expires_ms {
        return Err("the invite does not match the one issued".into());
    }
    let mut m = Membership::load(home)?.ok_or("this node holds no membership")?;
    if p.joined.iter().any(|k| k == joiner) && m.role_of(joiner).is_some() {
        return Ok(m); // the same key again: its first answer was lost
    }
    if inv.expires_ms <= now {
        return Err("the invite has expired: ask the admin for a new one".into());
    }
    if p.uses_left == 0 {
        return Err("the invite was already used: ask the admin for a new one".into());
    }
    if m.revoked.iter().any(|r| r.key == joiner) {
        return Err(format!("{joiner} was revoked: an admin has to re-invite it by key"));
    }
    if m.role_of(joiner).is_none() {
        m.invite(admin, joiner, &inv.role)?;
        m.save(home)?;
    }
    p.uses_left -= 1;
    p.joined.push(joiner.into());
    save(home, &all)?;
    Ok(m)
}

/// An invite and where its admin is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub invite: Invite,
    /// The admin's relay setting (addr.rs): the joiner's config takes it over.
    pub relay: Relay,
    pub hints: Vec<Hint>,
}

impl Ticket {
    /// The admin as a dial target.
    pub fn target(&self) -> Target {
        Target { key: self.invite.admin.clone(), hints: self.hints.clone() }
    }

    pub fn encode(&self) -> String {
        let i = &self.invite;
        let mut b = vec![1u8];
        put(&mut b, i.collective.as_bytes());
        b.extend(from_hex::<32>(&i.admin).expect("admin key"));
        b.extend(from_hex::<16>(&i.id).expect("invite id"));
        put(&mut b, i.role.as_bytes());
        b.extend(i.expires_ms.to_be_bytes());
        b.extend(from_hex::<64>(&i.sig).expect("signature"));
        let relay = if self.relay == Relay::Default { String::new() } else { self.relay.setting() };
        put(&mut b, relay.as_bytes());
        let hints: Vec<String> = self.hints.iter().take(255).map(|h| h.to_string()).collect();
        b.push(hints.len() as u8);
        for h in &hints {
            put(&mut b, h.as_bytes());
        }
        format!("{PREFIX}{}", BASE32_NOPAD.encode(&b))
    }

    /// Decodes and checks the signature (whether it is still good only the admin's node knows).
    pub fn decode(s: &str) -> Result<Ticket, String> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_uppercase();
        let body = s.strip_prefix(PREFIX).ok_or(format!("not a borg ticket (expected it to start with {PREFIX})"))?;
        let b = BASE32_NOPAD.decode(body.as_bytes()).map_err(|e| format!("ticket: {e} (copied completely?)"))?;
        let mut r = Reader { b: &b, at: 0 };
        if r.take(1)? != [1] {
            return Err("ticket: unknown version; update borg".into());
        }
        let collective = r.text()?;
        let admin = to_hex(r.take(32)?);
        let id = to_hex(r.take(16)?);
        let role = r.text()?;
        let expires_ms = i64::from_be_bytes(r.take(8)?.try_into().unwrap());
        let sig = to_hex(r.take(64)?);
        let relay = Relay::parse(&r.text()?)?;
        let n = r.take(1)?[0];
        let hints = (0..n).map(|_| r.text().and_then(|h| addr::hints(&h))).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect();
        if r.at != b.len() {
            return Err("ticket: trailing bytes".into());
        }
        let invite = Invite { collective, admin, id, role, expires_ms, sig };
        invite.verify()?;
        Ok(Ticket { invite, relay, hints })
    }
}

fn put(b: &mut Vec<u8>, s: &[u8]) {
    let s = &s[..s.len().min(255)];
    b.push(s.len() as u8);
    b.extend(s);
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.b.get(self.at..self.at + n).ok_or("ticket: truncated (copied completely?)")?;
        self.at += n;
        Ok(s)
    }

    fn text(&mut self) -> Result<String, String> {
        let n = self.take(1)?[0] as usize;
        String::from_utf8(self.take(n)?.to_vec()).map_err(|e| format!("ticket: {e}"))
    }
}

/// Where `serve` last found this node reachable (`<store>/mesh/addr.json`): what `invite` puts in
/// a ticket, since `invite` runs while `serve` holds the socket.
pub fn reach_path(home: &Path) -> PathBuf {
    identity::dir(home).join("addr.json")
}

pub fn save_reach(home: &Path, hints: &[Hint]) -> Result<(), String> {
    std::fs::create_dir_all(identity::dir(home)).map_err(|e| e.to_string())?;
    let p = reach_path(home);
    std::fs::write(&p, serde_json::json!({ "hints": addr::join_hints(hints) }).to_string()).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn load_reach(home: &Path) -> Vec<Hint> {
    let v: Option<serde_json::Value> = std::fs::read_to_string(reach_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok());
    v.and_then(|v| v.get("hints")?.as_str().map(str::to_string)).and_then(|h| addr::hints(&h).ok()).unwrap_or_default()
}

/// The path `join` writes for a new collective: routes nothing (it does not exist) until replaced.
pub fn placeholder(name: &str) -> String {
    format!("~/CHANGE-ME/repos-of-{name}")
}

/// `7d`, `12h`, `30m`, `45s` (or plain seconds) in ms.
pub fn duration_ms(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let (n, unit) = s.find(|c: char| !c.is_ascii_digit()).map_or((s, "s"), |i| s.split_at(i));
    let n: i64 = n.parse().map_err(|_| format!("duration {s:?}: expected e.g. 7d, 12h, 30m"))?;
    let unit = match unit {
        "d" => borg::clock::DAY_MS,
        "h" => 3_600_000,
        "m" => 60_000,
        "s" => 1000,
        _ => return Err(format!("duration {s:?}: unit must be d, h, m or s")),
    };
    n.checked_mul(unit).filter(|ms| *ms > 0).ok_or(format!("duration {s:?} out of range"))
}

/// The ticket as a QR code in unicode half-blocks, light modules drawn (for a dark terminal).
pub fn qr_text(ticket: &str) -> Result<String, String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), qrcode::EcLevel::L).map_err(|e| format!("qr: {e}"))?;
    Ok(code.render::<Dense1x2>().dark_color(Dense1x2::Light).light_color(Dense1x2::Dark).quiet_zone(true).build())
}

/// The ticket as a QR code PNG (black on white, 8 px per module, 4 modules of quiet zone).
pub fn qr_png(ticket: &str) -> Result<Vec<u8>, String> {
    let code = qrcode::QrCode::with_error_correction_level(ticket.as_bytes(), qrcode::EcLevel::L).map_err(|e| format!("qr: {e}"))?;
    let (w, colors) = (code.width(), code.to_colors());
    let (scale, quiet) = (8usize, 4usize);
    let side = (w + 2 * quiet) * scale;
    let mut raw = Vec::with_capacity(side * (side + 1));
    for y in 0..side {
        raw.push(0u8); // filter: none
        for x in 0..side {
            let (mx, my) = ((x / scale).wrapping_sub(quiet), (y / scale).wrapping_sub(quiet));
            let dark = mx < w && my < w && colors[my * w + mx] == qrcode::Color::Dark;
            raw.push(if dark { 0 } else { 255 });
        }
    }
    Ok(png_gray(side as u32, &raw))
}

/// A minimal 8-bit greyscale PNG: `raw` is the filtered scanlines, stored uncompressed (zlib store).
fn png_gray(side: u32, raw: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8], data: &[u8]) {
        out.extend((data.len() as u32).to_be_bytes());
        let mut c = kind.to_vec();
        c.extend(data);
        out.extend(&c);
        out.extend(crc32(&c).to_be_bytes());
    }
    let mut z = vec![0x78, 0x01];
    let blocks: Vec<&[u8]> = raw.chunks(65_535).collect();
    for (i, b) in blocks.iter().enumerate() {
        z.push((i + 1 == blocks.len()) as u8);
        z.extend((b.len() as u16).to_le_bytes());
        z.extend((!(b.len() as u16)).to_le_bytes());
        z.extend(*b);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in raw {
        a = (a + x as u32) % 65_521;
        b = (b + a) % 65_521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = side.to_be_bytes().to_vec();
    ihdr.extend(side.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 0]); // 8 bit, greyscale, deflate, no filter method, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &x in data {
        c ^= x as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admin_store(tag: &str) -> (PathBuf, Identity, Membership) {
        let home = std::env::temp_dir().join(format!("borg-mesh-invite-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let me = Identity::load_or_create(&home).unwrap();
        let m = Membership::found("corp", &me);
        m.save(&home).unwrap();
        (home, me, m)
    }

    fn key(tag: &str) -> String {
        let home = std::env::temp_dir().join(format!("borg-mesh-invite-key-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        Identity::load_or_create(&home).unwrap().public()
    }

    /// Derived: one day (clock.rs `DAY_MS`), the tests' unit.
    const DAY: i64 = borg::clock::DAY_MS;

    #[test]
    fn a_ticket_round_trips_and_a_tampered_one_is_refused() {
        let (home, admin, m) = admin_store("codec");
        let inv = issue(&home, &admin, &m, "steer", 7 * DAY, 1, 1000).unwrap();
        let t = Ticket { invite: inv, relay: Relay::Default, hints: addr::hints("192.168.1.5:7878,https://relay.example.org").unwrap() };
        let s = t.encode();
        assert!(s.starts_with(PREFIX) && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()), "{s}");
        assert!(s.len() < 400, "compact: {} chars", s.len());
        assert_eq!(Ticket::decode(&s).unwrap(), t);
        assert_eq!(Ticket::decode(&format!("  {}\n", s.to_lowercase())).unwrap(), t, "case and whitespace from a copy-paste are fine");
        let custom = Ticket { relay: Relay::parse("https://relay.corp.example").unwrap(), hints: Vec::new(), ..t.clone() };
        assert_eq!(Ticket::decode(&custom.encode()).unwrap(), custom);
        assert!(Ticket::decode(&s[..s.len() - 10]).is_err(), "truncated");
        assert!(Ticket::decode("hello").is_err());

        // Another role under the same signature: the signature no longer verifies.
        let forged = Ticket { invite: Invite { role: "member".into(), ..t.invite.clone() }, ..t.clone() };
        assert!(Ticket::decode(&forged.encode()).unwrap_err().contains("bad signature"));
        let later = Invite { expires_ms: t.invite.expires_ms + DAY, ..t.invite.clone() };
        assert!(later.verify().is_err(), "the expiry is signed too");
    }

    #[test]
    fn only_an_admin_issues_and_only_ticket_roles() {
        let (home, admin, mut m) = admin_store("issue");
        assert!(issue(&home, &admin, &m, "admin", DAY, 1, 0).is_err());
        assert!(issue(&home, &admin, &m, "member", 0, 1, 0).is_err());
        let other = Identity::load_or_create(&home.join("other")).unwrap();
        m.invite(&admin, &other.public(), "member").unwrap();
        assert!(issue(&home, &other, &m, "member", DAY, 1, 0).unwrap_err().contains("only an admin"));
    }

    #[test]
    fn single_use_expiry_and_withdrawal() {
        let (home, admin, m) = admin_store("redeem");
        let (bob, carol) = (key("bob"), key("carol"));
        let inv = issue(&home, &admin, &m, "member", DAY, 1, 1000).unwrap();
        assert_eq!(pending(&home, 1000).len(), 1);

        let joined = redeem(&home, &admin, "corp", &inv, &bob, 2000).unwrap();
        assert_eq!(joined.role_of(&bob), Some("member"));
        assert_eq!(Membership::load(&home).unwrap().unwrap().role_of(&bob), Some("member"), "saved, re-signed");
        assert!(pending(&home, 2000).is_empty(), "spent");
        assert!(redeem(&home, &admin, "corp", &inv, &carol, 3000).unwrap_err().contains("already used"));
        assert!(redeem(&home, &admin, "corp", &inv, &bob, 3000).is_ok(), "bob again (his answer was lost): welcome, no use spent");

        let short = issue(&home, &admin, &m, "observe", 1000, 1, 10_000).unwrap();
        assert!(redeem(&home, &admin, "corp", &short, &carol, 11_000).unwrap_err().contains("expired"));

        let gone = issue(&home, &admin, &m, "member", DAY, 3, 10_000).unwrap();
        assert_eq!(withdraw(&home, &gone.id[..8]).unwrap(), gone.id);
        assert!(redeem(&home, &admin, "corp", &gone, &carol, 11_000).unwrap_err().contains("withdrawn"));

        let other_collective = Invite { collective: "else".into(), ..inv.clone() };
        assert!(redeem(&home, &admin, "corp", &other_collective, &carol, 3000).is_err());
        let (home2, admin2, _) = admin_store("redeem-other-admin");
        assert!(redeem(&home2, &admin2, "corp", &inv, &carol, 3000).unwrap_err().contains("another admin"));
    }

    #[test]
    fn a_revoked_key_cannot_rejoin_with_a_leftover_ticket() {
        let (home, admin, m) = admin_store("revoked");
        let bob = key("revoked-bob");
        let inv = issue(&home, &admin, &m, "member", DAY, 2, 1000).unwrap();
        redeem(&home, &admin, "corp", &inv, &bob, 2000).unwrap();
        let mut m = Membership::load(&home).unwrap().unwrap();
        m.revoke(&admin, &bob, 3000).unwrap();
        m.save(&home).unwrap();
        // His own earlier use does not count: he is no longer listed.
        assert!(redeem(&home, &admin, "corp", &inv, &bob, 4000).unwrap_err().contains("revoked"));
    }

    #[test]
    fn durations_and_qr() {
        assert_eq!(duration_ms("7d").unwrap(), 7 * DAY);
        assert_eq!(duration_ms("90").unwrap(), 90_000);
        assert!(duration_ms("7w").is_err() && duration_ms("0d").is_err() && duration_ms("d").is_err());
        let qr = qr_text("BORG1ABC").unwrap();
        assert!(qr.lines().count() > 10 && qr.contains('█'));
        let png = qr_png("BORG1ABC").unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(crc32(b"IEND"), 0xae42_6082, "the CRC every PNG ends with");
    }
}
