//! `borg setup`'s questions about collectives: personal (private, `share: local`, also `personal`), team
//! or company (founded here, or joined with a ticket), the repo directories of each, off paths and
//! `default`. Flags answer their question without asking; `--yes` takes the defaults of the rest.
//! Answers become a [`Want`] (config.rs) and the mesh commands a team needs ([`Mesh`]).

use crate::collective::{Entry, Share};

use super::Opts;
use super::config::{self, Want};
use super::prompt::Prompt;

/// The personal collective's name.
pub const PRIVATE: &str = "private";

/// What the mesh (borg-mesh) does for a team collective once the config is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mesh {
    /// Found `name`: this machine becomes its admin.
    Init(String),
    /// Join with a ticket; `paths` replace join's placeholder.
    Join { ticket: String, paths: Vec<String> },
}

impl Mesh {
    /// The `borg mesh` arguments.
    #[must_use]
    pub fn args(&self) -> Vec<String> {
        match self {
            Mesh::Init(name) => vec!["init".into(), "--collective".into(), name.clone()],
            Mesh::Join { ticket, paths } => {
                let paths = if paths.is_empty() { Vec::new() } else { std::iter::once("--paths".to_string()).chain(paths.iter().cloned()).collect() };
                [vec!["join".to_string(), ticket.clone()], paths, vec!["--apply".into()]].concat()
            }
        }
    }
}

/// The collectives to add (none when the engineer keeps an existing config as it is), and the mesh
/// command for a team.
///
/// # Errors
/// A flag's directory that does not exist, or an invalid relay.
pub fn choose(o: &Opts, ask: &mut Prompt, existing: bool) -> Result<(Want, Option<Mesh>), String> {
    let (private, team) = kinds(o, ask, existing);
    let mut want = Want { default: "off".into(), ..Want::default() };
    if private {
        personal(o, ask, &mut want)?;
    }
    let mesh = if team { self::team(o, ask, &mut want)? } else { None };
    if private || team {
        boundaries(ask, &mut want, existing)?;
    }
    Ok((want, mesh))
}

/// Off paths, and for a new config where every other repo goes.
fn boundaries(ask: &mut Prompt, want: &mut Want, existing: bool) -> Result<(), String> {
    want.off = dirs(&ask.list("Repos no collective may ever see (client or confidential work)", &[]), false)?;
    if !existing && !want.entries.is_empty() {
        want.default = default_of(ask, &want.entries);
    }
    Ok(())
}

/// Personal, team, both, or (with a config already there) neither. Collective flags decide alone.
fn kinds(o: &Opts, ask: &mut Prompt, existing: bool) -> (bool, bool) {
    let team = o.team.is_some() || o.join.is_some();
    if o.private.is_some() || team {
        return (o.private.is_some(), team);
    }
    let mut options = vec!["personal: a private collective, never leaves this machine", "team or company: shared through the mesh", "both"];
    if existing {
        options.push("keep the existing config as it is");
    }
    match ask.choose("Which collectives?", &options, if existing { 3 } else { 0 }) {
        0 => (true, false),
        1 => (false, true),
        2 => (true, true),
        _ => (false, false),
    }
}

/// The personal collective over the chosen directories, also `personal`; none without any.
fn personal(o: &Opts, ask: &mut Prompt, want: &mut Want) -> Result<(), String> {
    let paths = match &o.private {
        Some(p) if !p.is_empty() => dirs(p, true)?,
        _ => dirs(&ask.list("Repo directories for your personal collective", &config::default_roots()), ask.yes())?,
    };
    if paths.is_empty() {
        println!("no repo directories: no personal collective (borg setup --private DIR...)");
        return Ok(());
    }
    want.personal = Some(PRIVATE.into());
    want.entries.push(entry(PRIVATE, paths, Share::Local));
    Ok(())
}

fn entry(name: &str, paths: Vec<String>, share: Share) -> Entry {
    Entry { name: name.into(), paths, share, experts: Vec::new(), relay: None, mode: None }
}

/// A team collective: founded here (its entry goes into the config, then `mesh init`) or joined with a
/// ticket (`mesh join` writes its entry, the ticket names it). Join chosen without a ticket: none.
fn team(o: &Opts, ask: &mut Prompt, want: &mut Want) -> Result<Option<Mesh>, String> {
    let given = ticket_of(o, ask);
    if let Some(e) = given.as_deref().and_then(|t| ticket(t).err()) {
        println!("{e}: no team collective; later: borg setup --join BORG1... --paths DIR...");
        return Ok(None);
    }
    let paths = team_paths(o, ask)?;
    if let Some(ticket) = given {
        return Ok(Some(Mesh::Join { ticket, paths }));
    }
    let name = o.team.clone().unwrap_or_else(|| ask.line("Team collective name", "team"));
    let mut e = entry(&name, paths, Share::Team);
    e.relay = relay(o, ask)?;
    e.mode = (ask.choose("When the conscience judges the work", &["live: during the turn and at its end (default)", "hook: at the turn end only"], 0) == 1)
        .then(|| "hook".to_string());
    want.entries.push(e);
    Ok(Some(Mesh::Init(name)))
}

/// The ticket to join with: `--join`, or the answer once the engineer chooses to join; None to found one.
fn ticket_of(o: &Opts, ask: &mut Prompt) -> Option<String> {
    if o.join.is_some() || o.team.is_some() {
        return o.join.clone();
    }
    let join = ask.choose("Team collective", &["found one (you become its admin)", "join one with a ticket (BORG1...)"], 0) == 1;
    join.then(|| ask.line("Ticket", "-"))
}

/// The team collective's repo directories; a WARN where there are none, since it then routes no repo.
fn team_paths(o: &Opts, ask: &mut Prompt) -> Result<Vec<String>, String> {
    let paths = if o.paths.is_empty() { dirs(&ask.list("Repo directories for the team collective", &[]), false)? } else { dirs(&o.paths, true)? };
    if paths.is_empty() {
        println!("WARN no repo directories: the team collective routes no repo until collectives.json lists some (or: borg setup ... --paths DIR...)");
    }
    Ok(paths)
}

/// Whether `t` looks like a mesh ticket (`BORG1…`; borg-mesh decodes the rest).
///
/// # Errors
/// Anything else.
pub fn ticket(t: &str) -> Result<(), String> {
    if t.trim().to_ascii_uppercase().starts_with("BORG1") && t.trim().len() > "BORG1".len() {
        Ok(())
    } else {
        Err(format!("{t:?} is not a mesh ticket: they start with BORG1 (`borg mesh invite` prints one)"))
    }
}

/// The team's relay: `None` for n0's public relays (the default), `disabled` (LAN or VPN), or a company
/// `iroh-relay` URL.
fn relay(o: &Opts, ask: &mut Prompt) -> Result<Option<String>, String> {
    let r = match &o.relay {
        Some(r) => r.clone(),
        None => match ask.choose(
            "How members reach each other",
            &["n0's public relays (default)", "disabled: direct addresses only (LAN or VPN)", "your company's iroh-relay"],
            0,
        ) {
            0 => "default".into(),
            1 => "disabled".into(),
            _ => ask.line("Relay URL", "https://"),
        },
    };
    relay_value(&r)
}

/// A `--relay` value as the config stores it.
///
/// # Errors
/// Anything but `default`, `disabled` or an http(s) URL.
pub fn relay_value(r: &str) -> Result<Option<String>, String> {
    match r {
        "default" => Ok(None),
        "disabled" => Ok(Some(r.into())),
        url if url.len() > "https://".len() && (url.starts_with("https://") || url.starts_with("http://")) => Ok(Some(url.into())),
        _ => Err(format!("relay {r:?}: an http(s) URL of an iroh-relay, disabled, or default")),
    }
}

/// Where a repo outside every listed directory goes: off (recommended) or one of `entries`.
fn default_of(ask: &mut Prompt, entries: &[Entry]) -> String {
    let names: Vec<String> = entries.iter().map(|e| format!("{}: every other repo joins it", e.name)).collect();
    let options: Vec<&str> = std::iter::once("off: other repos belong to no collective (recommended)").chain(names.iter().map(String::as_str)).collect();
    match ask.choose("Repos outside these directories", &options, 0) {
        0 => "off".into(),
        n => entries[n - 1].name.clone(),
    }
}

/// Each of `list` as the config writes it (config.rs `dir`). `strict` (flags, `--yes`): a path that is not
/// a directory is an error; else it is skipped with a note.
fn dirs(list: &[String], strict: bool) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for p in list {
        match config::dir(p) {
            Ok(d) => out.push(d),
            Err(e) if strict => return Err(e),
            Err(e) => println!("skipped {e}"),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesh_commands_and_relays() {
        assert_eq!(Mesh::Init("corp".into()).args(), ["init", "--collective", "corp"]);
        let join = Mesh::Join { ticket: "BORG1X".into(), paths: vec!["~/w".into()] };
        assert_eq!(join.args(), ["join", "BORG1X", "--paths", "~/w", "--apply"]);
        assert_eq!(Mesh::Join { ticket: "BORG1X".into(), paths: vec![] }.args(), ["join", "BORG1X", "--apply"]);
        assert_eq!(relay_value("default").unwrap(), None);
        assert_eq!(relay_value("disabled").unwrap().as_deref(), Some("disabled"));
        assert!(relay_value("https://relay.corp.example").is_ok());
        assert!(relay_value("https://").is_err() && relay_value("relay.corp").is_err());
        assert!(ticket("BORG1abc").is_ok() && ticket("-").is_err() && ticket("BORG1").is_err());
    }
}
