//! How a node is reached: by its key, with optional hints, through the relay its collective uses.
//!
//! - **Relay** (the collective's `relay` in collectives.json, `BORG_RELAY` overrides it):
//!   `default` uses n0's public relays and n0's DNS address lookup (pkarr), so two laptops behind
//!   different NATs find each other by key alone and iroh hole-punches a direct path, falling back
//!   to the relay when it cannot. A URL (or several, comma-separated) uses a self-hosted
//!   `iroh-relay` and no public lookup. `disabled` dials direct addresses only (LAN or VPN). Traffic
//!   is end-to-end encrypted either way: a relay forwards QUIC packets it cannot read.
//! - **Target** (`<key>[@<hint>,<hint>...]`): what `sync`, `peer` and peers.json name. A hint is a
//!   direct address (`ip:port`, the old `<key>@<ip:port>` form) or a relay URL (`https://...`). The
//!   key is what counts: hints only save a lookup.

use std::net::SocketAddr;

use iroh::{EndpointAddr, PublicKey, RelayMode, RelayUrl};

use crate::from_hex;

pub const ENV: &str = "BORG_RELAY";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relay {
    /// n0's public relays and DNS address lookup.
    Default,
    /// Direct addresses only.
    Disabled,
    /// Self-hosted relays, no public address lookup.
    Custom(Vec<RelayUrl>),
}

impl Relay {
    /// `BORG_RELAY` if set, else the collective's setting, else `default`.
    pub fn choose(configured: Option<&str>) -> Result<Relay, String> {
        let env = std::env::var(ENV).ok().filter(|v| !v.trim().is_empty());
        Relay::parse(env.as_deref().or(configured).unwrap_or("default"))
    }

    pub fn parse(s: &str) -> Result<Relay, String> {
        match s.trim() {
            "" | "default" | "n0" => Ok(Relay::Default),
            "disabled" | "off" | "none" => Ok(Relay::Disabled),
            urls => urls.split(',').map(|u| relay_url(u.trim())).collect::<Result<Vec<_>, _>>().map(Relay::Custom),
        }
    }

    /// The setting as collectives.json spells it.
    pub fn setting(&self) -> String {
        match self {
            Relay::Default => "default".into(),
            Relay::Disabled => "disabled".into(),
            Relay::Custom(urls) => urls.iter().map(|u| u.to_string()).collect::<Vec<_>>().join(","),
        }
    }

    pub fn mode(&self) -> RelayMode {
        match self {
            Relay::Default => iroh::endpoint::default_relay_mode(),
            Relay::Disabled => RelayMode::Disabled,
            Relay::Custom(urls) => RelayMode::custom(urls.iter().cloned()),
        }
    }
}

fn relay_url(u: &str) -> Result<RelayUrl, String> {
    if !(u.starts_with("https://") || u.starts_with("http://")) {
        return Err(format!("relay {u:?}: expected \"default\", \"disabled\" or an http(s) URL"));
    }
    u.parse().map_err(|e| format!("relay {u:?}: {e}"))
}

/// One hint: a direct address or a relay URL. `None` for something nobody could dial.
fn hint(h: &str) -> Option<Result<Hint, String>> {
    let h = h.trim();
    if h.is_empty() {
        return None;
    }
    if h.starts_with("https://") || h.starts_with("http://") {
        return Some(relay_url(h).map(Hint::Relay));
    }
    Some(match h.parse::<SocketAddr>() {
        Ok(a) if a.ip().is_unspecified() || a.port() == 0 => Err(format!("{h}: not an address a peer can dial")),
        Ok(a) => Ok(Hint::Direct(a)),
        Err(e) => Err(format!("bad address {h}: {e}")),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hint {
    Direct(SocketAddr),
    Relay(RelayUrl),
}

impl std::fmt::Display for Hint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Hint::Direct(a) => write!(f, "{a}"),
            Hint::Relay(u) => write!(f, "{u}"),
        }
    }
}

/// Parses comma-separated hints; an empty string is no hint.
pub fn hints(s: &str) -> Result<Vec<Hint>, String> {
    s.split(',').filter_map(hint).collect()
}

pub fn join_hints(h: &[Hint]) -> String {
    h.iter().map(|h| h.to_string()).collect::<Vec<_>>().join(",")
}

/// A member to dial: its key and what is known about where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub key: String,
    pub hints: Vec<Hint>,
}

impl Target {
    /// `<key>`, `<key>@<ip:port>` or `<key>@<hint>,<hint>...`.
    pub fn parse(s: &str) -> Result<Target, String> {
        let (key, rest) = s.split_once('@').unwrap_or((s, ""));
        crate::identity::check_key(key)?;
        Ok(Target { key: key.into(), hints: hints(rest)? })
    }

    pub fn id(&self) -> Result<PublicKey, String> {
        PublicKey::from_bytes(&from_hex::<32>(&self.key)?).map_err(|e| e.to_string())
    }

    /// The iroh address: the key, the hints, and the relays a self-hosted setting names (members of
    /// one company share them). Refused when relays are off and no direct address is known.
    pub fn endpoint_addr(&self, relay: &Relay) -> Result<EndpointAddr, String> {
        let mut a = EndpointAddr::new(self.id()?);
        for h in &self.hints {
            a = match h {
                Hint::Direct(d) => a.with_ip_addr(*d),
                Hint::Relay(u) if *relay != Relay::Disabled => a.with_relay_url(u.clone()),
                Hint::Relay(_) => a,
            };
        }
        if let Relay::Custom(urls) = relay {
            for u in urls {
                a = a.with_relay_url(u.clone());
            }
        }
        if *relay == Relay::Disabled && a.ip_addrs().next().is_none() {
            return Err(format!("no direct address for {}: relays are disabled, give <key>@<ip:port>", self.key));
        }
        Ok(a)
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.hints.is_empty() { write!(f, "{}", self.key) } else { write!(f, "{}@{}", self.key, join_hints(&self.hints)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "3b6a27bcceb6a42d62a3a8d02a6f0d73653215771de243a63ac048a18b59da29";

    #[test]
    fn relay_settings_parse() {
        assert_eq!(Relay::parse("").unwrap(), Relay::Default);
        assert_eq!(Relay::parse("disabled").unwrap(), Relay::Disabled);
        let Relay::Custom(u) = Relay::parse("https://relay.corp.example").unwrap() else { panic!() };
        assert_eq!(u.len(), 1);
        assert!(Relay::parse("relay.corp.example").is_err(), "a bare host is a typo, not a relay");
        assert_eq!(
            Relay::parse(&Relay::parse("https://a.example,https://b.example").unwrap().setting()).unwrap().setting(),
            "https://a.example/,https://b.example/"
        );
    }

    #[test]
    fn targets_by_key_with_old_and_new_hints() {
        let t = Target::parse(KEY).unwrap();
        assert!(t.hints.is_empty());
        assert!(t.endpoint_addr(&Relay::Default).is_ok(), "the key alone: address lookup finds it");
        assert!(t.endpoint_addr(&Relay::Disabled).unwrap_err().contains("relays are disabled"));
        let old = Target::parse(&format!("{KEY}@10.0.0.5:7878")).unwrap();
        assert_eq!(old.hints, vec![Hint::Direct("10.0.0.5:7878".parse().unwrap())]);
        assert_eq!(old.to_string(), format!("{KEY}@10.0.0.5:7878"));
        let both = Target::parse(&format!("{KEY}@10.0.0.5:7878,https://relay.corp.example")).unwrap();
        let a = both.endpoint_addr(&Relay::Default).unwrap();
        assert_eq!((a.ip_addrs().count(), a.relay_urls().count()), (1, 1));
        assert_eq!(Target::parse(&both.to_string()).unwrap(), both);
        assert!(Target::parse(&format!("{KEY}@0.0.0.0:7878")).is_err());
        assert!(Target::parse("nokey@10.0.0.5:1").is_err());
    }
}
