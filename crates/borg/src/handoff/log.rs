//! The hand-off log: news of offers, asks, declines and outcomes, folded per hand-off.
//!
//! Two sources, read as one text (`read`): `<store>/handoffs.jsonl`, the lines this machine wrote with
//! the offers it took or declined and the peers' outcomes of its own offers (`keep_outcomes`), so none
//! of those rests on the message log's TTL; and `<store>/handoff/mesh.jsonl`, the events of the
//! hand-off lines other members' nodes wrote into the message log (msg.rs `HandoffLine`), which the
//! mesh carries signed by their node (`derive` rebuilds it from that log, so it is bounded as the log
//! is: `MEMBER_BYTES` per member and class, `TTL_MS`).
//!
//! A line is a hint, never the decision: the arbiter repo's `taken` ref decides who holds the work
//! (mod.rs). Lines are content-addressed (a line is its bytes: an outcome line is built from its claim
//! commit alone, so whoever writes it writes the same bytes, and the same news appended twice is one
//! line) and the log is a grow-only set: the fold reads it in any order to the same state, by a
//! monotone precedence: open < asked < declined < claimed (taken, taken back or aborted) < done, and a
//! claimed hand-off stays claimed whatever line comes after. An outcome counts only when it cites a claim
//! commit (the fencing token), so a line that is late or made up cannot reopen an offer; outcomes that
//! cite different claims leave the hand-off contested until the remote says which (`Known`, which
//! fence.rs and every listing read; fence.rs fences meanwhile), and of lines citing one claim only the
//! one naming the claimer its commit names counts. A decline counts only from a line naming whom the
//! offer asked (a self-declared engineer, as in messages, or the signing node), once. An abort after a
//! take cites the take's claim and its own commit on `abort` (a second register, created once); a done
//! line counts only beside the claim the remote was seen holding, from the offering node or the
//! claimer's node (beside a claim only lines cite it is a hint), and the remote seen holding no ref of
//! the hand-off any more (`Known::gone`) is done whatever the lines say.
//! No fold compares
//! clocks of different machines: a line's `ms` is its own node's, display only; whether a peer's line
//! is read at all follows the message protocol's clock rule (msg.rs `fresh_at`, in `derive`).
//!
//! Every line is checked on read as a peer's would be (`well_formed`): ids and names are tokens, texts
//! within the message bound (msg.rs `WIRE_TEXT`) and free of control characters where a listing
//! prints them on a line of their own, a line within `LINE_BYTES`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::git::is_oid;
use super::wal::Verb;

pub(crate) const FILE: &str = "handoffs.jsonl";

pub(crate) fn path(home: &Path) -> PathBuf {
    home.join(FILE)
}

/// The events other nodes' hand-off lines carry (`derive`).
pub(crate) fn mesh_path(home: &Path) -> PathBuf {
    super::wal::dir(home).join("mesh.jsonl")
}

/// What the remote said, per hand-off (`Known`).
fn known_path(home: &Path) -> PathBuf {
    super::wal::dir(home).join("remote.json")
}

/// Guard: the longest line read. Derived from what an offer holds: its note and the head of its brief,
/// each at most msg.rs `WIRE_TEXT` characters of up to 6 bytes as JSON escapes them, with as much again
/// for its repos and the names of the files it left out (`fit` drops names past it).
pub(crate) const LINE_BYTES: usize = 16 * crate::msg::WIRE_TEXT;

/// What a line says happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Kind {
    Offer,
    /// Another engineer's claim won.
    Taken,
    /// The offering engineer's own claim won.
    Back,
    /// Whom the offer asked (its `to`) said no; the offer stays its owner's.
    Declined,
    /// Closed by the offering engineer, an admin or a steerer: on an open offer its claim holds
    /// `taken` (nobody gets the work); after a take it cites the take's claim and its `abort` commit.
    Aborted,
    /// Finished: its refs deleted from the remote; the fences go.
    Done,
}

/// One repo of an offer: where its state is, on which remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Repo<'a> {
    #[serde(borrow)]
    pub name: Cow<'a, str>,
    /// Its push URL, credentials redacted (notes.rs `redact`).
    #[serde(borrow)]
    pub remote: Cow<'a, str>,
    #[serde(borrow)]
    pub branch: Cow<'a, str>,
    #[serde(borrow)]
    pub head: Cow<'a, str>,
    #[serde(borrow)]
    pub state: Cow<'a, str>,
    #[serde(borrow)]
    pub tree: Cow<'a, str>,
}

/// One line of the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Event<'a> {
    pub v: u8,
    #[serde(borrow)]
    pub id: Cow<'a, str>,
    pub kind: Kind,
    /// The node and session that wrote it: the offering one, or the claim's.
    #[serde(borrow)]
    pub node: Cow<'a, str>,
    #[serde(borrow)]
    pub session: Cow<'a, str>,
    #[serde(borrow, default)]
    pub engineer: Cow<'a, str>,
    #[serde(borrow, default)]
    pub harness: Cow<'a, str>,
    /// Whom an offer asks to take it (`--to`): an engineer's email or a node key. An offer that names one
    /// is the ask.
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub to: Option<Cow<'a, str>>,
    /// An offer's nonce: its id is `h` + 12 hex of blake3 over node, session and nonce (mod.rs `id_for`),
    /// so no other node or session can write an offer of the same id.
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub nonce: Option<Cow<'a, str>>,
    /// The writer's clock: shown as an age, never compared across machines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<i64>,
    #[serde(borrow, default, skip_serializing_if = "Vec::is_empty")]
    pub repos: Vec<Repo<'a>>,
    #[serde(borrow, default, skip_serializing_if = "str::is_empty")]
    pub note: Cow<'a, str>,
    /// The brief's first lines (mod.rs `head_of`); the brief itself is in git, on `brief`.
    #[serde(borrow, default, skip_serializing_if = "str::is_empty")]
    pub brief_head: Cow<'a, str>,
    /// An offer's brief commit (`refs/borg/handoff/<id>/brief`, on the arbiter repo's state).
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub brief: Option<Cow<'a, str>>,
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_vec", skip_serializing_if = "Vec::is_empty")]
    pub left_out: Vec<Cow<'a, str>>,
    /// How many left-out names the line had no room for (`fit`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub more_left_out: usize,
    /// An outcome's claim commit: the remote's `taken` ref.
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub claim: Option<Cow<'a, str>>,
    /// The state the claim took (its parent).
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub state: Option<Cow<'a, str>>,
    /// An abort's reason: redacted, one line.
    #[serde(borrow, default, skip_serializing_if = "str::is_empty")]
    pub why: Cow<'a, str>,
    /// An abort after a take: its commit on `refs/borg/handoff/<id>/abort` (its parent: the claim).
    #[serde(borrow, default, deserialize_with = "crate::msg::cow_opt", skip_serializing_if = "Option::is_none")]
    pub abort: Option<Cow<'a, str>>,
}

/// Whether `url` can be a repo's remote in a line: not empty, no whitespace or control character, not
/// starting with `-` (a listing prints it in a command to run).
pub(crate) fn url_shaped(url: &str) -> bool {
    !url.is_empty() && !url.starts_with('-') && !url.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

impl<'a> Event<'a> {
    /// A line of `kind` of hand-off `id` written by `node`'s session, every other field empty.
    pub(crate) fn of(id: &'a str, kind: Kind, node: &'a str, session: &'a str, engineer: &'a str, harness: &'a str) -> Event<'a> {
        Event {
            v: 1,
            id: id.into(),
            kind,
            node: node.into(),
            session: session.into(),
            engineer: engineer.into(),
            harness: harness.into(),
            to: None,
            nonce: None,
            ms: None,
            repos: Vec::new(),
            note: "".into(),
            brief_head: "".into(),
            brief: None,
            left_out: Vec::new(),
            more_left_out: 0,
            claim: None,
            state: None,
            why: "".into(),
            abort: None,
        }
    }

    /// Whether a line is one this version writes: its names tokens, its shas full ids, an offer with a
    /// repo and its brief and an outcome with its claim and state, texts within `WIRE_TEXT` characters,
    /// and what a listing prints on a line of its own (names of left-out files, branches, remotes, the
    /// note) without a control character, so a peer's line cannot add lines to what the agent reads.
    pub(crate) fn well_formed(&self) -> bool {
        let text = |t: &str| t.chars().count() <= crate::msg::WIRE_TEXT;
        let plain = |t: &str| !t.is_empty() && !t.starts_with('-') && !t.chars().any(char::is_control);
        let repo =
            |r: &Repo| crate::msg::token(&r.name) && [&r.head, &r.state, &r.tree].into_iter().all(|s| is_oid(s)) && plain(&r.branch) && url_shaped(&r.remote);
        self.v == 1
            && super::is_id(&self.id)
            && crate::msg::token(&self.node)
            && crate::msg::token(&self.session)
            && (self.engineer.is_empty() || crate::msg::token(&self.engineer))
            && (self.harness.is_empty() || crate::msg::token(&self.harness))
            && text(&self.note)
            && !self.note.chars().any(char::is_control)
            && text(&self.why)
            && !self.why.chars().any(char::is_control)
            && (self.abort.is_none() || self.kind == Kind::Aborted)
            && text(&self.brief_head)
            && self.left_out.iter().all(|p| plain(p))
            && self.to.as_deref().is_none_or(crate::msg::token)
            && match self.kind {
                Kind::Offer => {
                    !self.repos.is_empty()
                        && self.repos.iter().all(repo)
                        && self.brief.as_deref().is_some_and(is_oid)
                        && self.nonce.as_deref().is_some_and(|n| super::id_for(&self.node, &self.session, n) == self.id)
                }
                Kind::Taken | Kind::Back => self.claim.as_deref().is_some_and(is_oid) && self.state.as_deref().is_some_and(is_oid),
                Kind::Aborted => {
                    self.claim.as_deref().is_some_and(is_oid) && self.state.as_deref().is_some_and(is_oid) && self.abort.as_deref().is_none_or(is_oid)
                }
                Kind::Declined => self.claim.is_none() && self.repos.is_empty(),
                Kind::Done => self.claim.as_deref().is_some_and(is_oid) && self.repos.is_empty(),
            }
    }

    /// Whether this line names `who` as its writer: their email (without case, as collective.rs reads
    /// emails), or their node.
    pub(crate) fn by(&self, who: &str) -> bool {
        self.engineer.eq_ignore_ascii_case(who) || self.node == who
    }

    /// The line as it is written, dropping left-out names until it fits `LINE_BYTES` (each counted in
    /// `more_left_out`).
    pub(crate) fn fit(mut self) -> String {
        loop {
            let line = serde_json::to_string(&self).unwrap_or_default();
            if line.len() <= LINE_BYTES || self.left_out.is_empty() {
                return line;
            }
            // Drop at least as many names as their quoted bytes exceed the bound, then measure again.
            let mut over = line.len() - LINE_BYTES;
            while over > 0
                && let Some(p) = self.left_out.pop()
            {
                over = over.saturating_sub(p.len() + 3);
                self.more_left_out += 1;
            }
        }
    }
}

/// One hand-off as the log tells it.
#[derive(Debug, Clone)]
pub(crate) struct HandOff<'a> {
    pub offer: Event<'a>,
    /// The outcome lines, one per claim and writer they name (in the order of the claims): one claim
    /// is the outcome; more than one, only the remote can say which holds. Lines that cite one claim but
    /// name different writers all stay: the claim commit says whose it is (`end_of`), never an order.
    pub ends: Vec<Event<'a>>,
    /// The decline of whom the offer asks (its `to`): once, whichever of their sessions wrote it.
    pub decline: Option<Event<'a>>,
    /// Aborts after a take (an `abort` commit each), in a total order.
    pub aborts: Vec<Event<'a>>,
    /// Done lines, in a total order; which count is `closed`'s rule.
    pub dones: Vec<Event<'a>>,
}

/// An abort after a take that holds (`HandOff::post_abort`): its commit, its line when one names it,
/// and whether the remote's `abort` ref was seen holding it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PostAbort<'h, 'a> {
    pub sha: &'h str,
    pub by: Option<&'h Event<'a>>,
    pub confirmed: bool,
}

/// What a hand-off's outcome is, as far as this machine can tell (`HandOff::end_with`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum End<'h, 'a> {
    /// No claim holds that this machine knows of.
    Open,
    /// The claim that holds: one the remote's `taken` ref was seen holding (`confirmed`), or else the one
    /// claim the lines cite. Its line, when one tells who made it.
    Claim { claim: &'h str, by: Option<&'h Event<'a>>, confirmed: bool },
    /// The lines cite this many claims, and the remote has not said which holds.
    Contested(usize),
}

impl<'a> HandOff<'a> {
    pub(crate) fn id(&self) -> &str {
        &self.offer.id
    }

    /// How many outcome lines cite `claim`, each naming another writer.
    pub(crate) fn named(&self, claim: &str) -> usize {
        self.ends.iter().filter(|e| e.claim.as_deref() == Some(claim)).count()
    }

    /// The outcome line of `claim`: the one naming its claimer (`by`, as the claim commit names them),
    /// else the one line citing it; none when the lines that cite it name different writers, so a line
    /// another member made up beside the real one never picks who is named.
    pub(crate) fn end_of(&self, claim: &str, by: Option<&Claimant>) -> Option<&Event<'a>> {
        let mut lines = self.ends.iter().filter(|e| e.claim.as_deref() == Some(claim));
        match by {
            Some(c) => lines.find(|e| c.names(e)),
            None => lines.next().filter(|_| lines.next().is_none()),
        }
    }

    /// The outcome, given what the remote said of it (`known`): the claim it holds, else the claims the
    /// lines cite that it was not seen without.
    pub(crate) fn end_with<'h>(&'h self, known: Option<&'h Known>) -> End<'h, 'a> {
        if let Some(k) = known
            && let Some(c) = k.holds.as_deref()
        {
            return End::Claim { claim: c, by: self.end_of(c, k.by.as_ref()), confirmed: true };
        }
        let refuted = |c: &str| known.is_some_and(|k| k.refuted.iter().any(|r| r == c));
        // `ends` is sorted by claim first: one claim's lines are adjacent.
        let mut cited: Vec<&'h str> = self.ends.iter().filter_map(|e| e.claim.as_deref()).filter(|c| !refuted(c)).collect();
        cited.dedup();
        match cited.as_slice() {
            [] => End::Open,
            [c] => End::Claim { claim: c, by: self.end_of(c, None), confirmed: false },
            more => End::Contested(more.len()),
        }
    }

    /// The verb of the claim that holds: as its commit names it (`Known::by`), else as its line does.
    pub(crate) fn verb(&self, known: Option<&Known>) -> Option<Verb> {
        match self.end_with(known) {
            End::Claim { by, confirmed, .. } => {
                let k = known.and_then(|k| k.by.as_ref()).filter(|_| confirmed).map(|c| c.verb);
                k.or_else(|| by.and_then(|e| verb_of(e.kind)))
            }
            _ => None,
        }
    }

    /// The abort after the take that holds, if any: the commit the remote's `abort` ref was seen
    /// holding, else the first a line cites on that take's claim that the remote did not refute (a hint:
    /// fence.rs fences both sides on it until the remote is asked).
    pub(crate) fn post_abort<'h>(&'h self, known: Option<&'h Known>) -> Option<PostAbort<'h, 'a>> {
        let End::Claim { claim, .. } = self.end_with(known) else { return None };
        if matches!(self.verb(known), Some(Verb::Abort) | None) {
            return None;
        }
        let on = |e: &&Event| e.claim.as_deref() == Some(claim);
        if let Some(sha) = known.and_then(|k| k.abort.as_deref()) {
            let by = self.aborts.iter().filter(on).find(|e| e.abort.as_deref() == Some(sha));
            return Some(PostAbort { sha, by, confirmed: true });
        }
        let refuted = |sha: &str| known.is_some_and(|k| k.refuted.iter().any(|r| r == sha));
        self.aborts.iter().filter(on).find_map(|e| e.abort.as_deref().filter(|a| !refuted(a)).map(|sha| PostAbort { sha, by: Some(e), confirmed: false }))
    }

    /// Whether it is done: the remote was seen holding none of its refs (`Known::gone`), or a done line
    /// cites the claim the remote was seen holding and was written by the offering node or by the
    /// claimer's (as the claim commit names it). A done line beside a claim only lines cite is a hint:
    /// two made-up lines would otherwise close an open offer with nothing ever asking the remote.
    pub(crate) fn closed(&self, known: Option<&Known>) -> bool {
        if known.is_some_and(|k| k.gone) {
            return true;
        }
        let Some(k) = known else { return false };
        let Some(claim) = k.holds.as_deref() else { return false };
        let claimer = k.by.as_ref().map(|c| c.node.as_str()).or_else(|| self.end_of(claim, None).map(|e| e.node.as_ref()));
        self.dones.iter().any(|d| d.claim.as_deref() == Some(claim) && (d.node == self.offer.node || Some(d.node.as_ref()) == claimer))
    }

    /// The hand-off's state by the precedence of the module doc, as listings name it.
    pub(crate) fn state(&self, known: Option<&Known>) -> &'static str {
        if self.closed(known) {
            return "done";
        }
        if self.post_abort(known).is_some() {
            return "aborted";
        }
        match (self.end_with(known), self.verb(known)) {
            (End::Claim { .. }, Some(Verb::Abort)) => "aborted",
            (End::Claim { .. }, Some(Verb::Back)) => "taken back",
            (End::Claim { .. }, _) => "taken",
            (End::Contested(_), _) => "contested",
            (End::Open, _) if self.decline.is_some() => "declined",
            (End::Open, _) if self.offer.to.is_some() => "asked",
            (End::Open, _) => "open",
        }
    }
}

/// The verb of a claim an outcome line of `kind` names.
fn verb_of(kind: Kind) -> Option<Verb> {
    match kind {
        Kind::Taken => Some(Verb::Take),
        Kind::Back => Some(Verb::Back),
        Kind::Aborted => Some(Verb::Abort),
        Kind::Offer | Kind::Declined | Kind::Done => None,
    }
}

/// The fields an outcome line's order is total over: every field it has.
fn outcome_key<'b>(e: &'b Event) -> (Option<&'b str>, Option<&'b str>, Kind, &'b str, &'b str, &'b str, &'b str) {
    (e.claim.as_deref(), e.state.as_deref(), e.kind, &e.node, &e.session, &e.engineer, &e.harness)
}

/// The hand-offs of a log `text`, in the order of their first line: every well-formed line within
/// `LINE_BYTES`, in any order. Of several offers of one hand-off (one per state the owner published)
/// the latest by its own node's clock counts; an outcome or a decline with no offer known yet waits
/// for it, and a decline counts only from whom that offer asks, once.
pub(crate) fn fold(text: &str) -> Vec<HandOff<'_>> {
    let mut at: HashMap<&str, usize> = HashMap::new();
    let mut offers: Vec<Vec<Event>> = Vec::new();
    let mut ends: Vec<Vec<Event>> = Vec::new();
    let mut declines: Vec<Vec<Event>> = Vec::new();
    let mut aborts: Vec<Vec<Event>> = Vec::new();
    let mut dones: Vec<Vec<Event>> = Vec::new();
    for e in text.lines().filter(|l| l.len() <= LINE_BYTES).filter_map(|l| serde_json::from_str::<Event>(l).ok()).filter(Event::well_formed) {
        let id: &str = match &e.id {
            Cow::Borrowed(s) => s,
            // An id is hex: never escaped, so always borrowed; a line that escapes one is no line of ours.
            Cow::Owned(_) => continue,
        };
        let i = *at.entry(id).or_insert_with(|| {
            offers.push(Vec::new());
            ends.push(Vec::new());
            declines.push(Vec::new());
            aborts.push(Vec::new());
            dones.push(Vec::new());
            offers.len() - 1
        });
        match e.kind {
            Kind::Offer => offers[i].push(e),
            Kind::Aborted if e.abort.is_some() => aborts[i].push(e),
            Kind::Taken | Kind::Back | Kind::Aborted => ends[i].push(e),
            Kind::Declined => declines[i].push(e),
            Kind::Done => dones[i].push(e),
        }
    }
    let mut order: Vec<(&str, usize)> = at.into_iter().collect();
    order.sort_by_key(|(_, i)| *i);
    order
        .into_iter()
        .filter_map(|(_, i)| {
            // The id names its owner (`well_formed`): one node writes every offer of it, so that node's
            // clock orders them.
            let offer = std::mem::take(&mut offers[i]).into_iter().max_by(|a, b| (a.ms, &a.repos[0].state).cmp(&(b.ms, &b.repos[0].state)))?;
            // One per claim and writer, in a total order over what they name, so nothing depends on an
            // order of arrival.
            let mut ends = std::mem::take(&mut ends[i]);
            ends.sort_by(|a, b| outcome_key(a).cmp(&outcome_key(b)));
            ends.dedup_by(|a, b| outcome_key(a) == outcome_key(b));
            // Every decline kept names whom the offer asks: one of them, the first by a total order.
            let decline = std::mem::take(&mut declines[i])
                .into_iter()
                .filter(|d| offer.to.as_deref().is_some_and(|t| d.by(t)))
                .min_by(|a, b| (&a.node, &a.session, a.ms).cmp(&(&b.node, &b.session, b.ms)));
            let order = |v: &mut Vec<Event>| {
                v.sort_by(|a, b| (outcome_key(a), &a.abort).cmp(&(outcome_key(b), &b.abort)));
                v.dedup_by(|a, b| (outcome_key(a), &a.abort) == (outcome_key(b), &b.abort));
            };
            let (mut aborts, mut dones) = (std::mem::take(&mut aborts[i]), std::mem::take(&mut dones[i]));
            order(&mut aborts);
            order(&mut dones);
            Some(HandOff { offer, ends, decline, aborts, dones })
        })
        .collect()
}

/// Appends `line` to the log unless it holds it already (a line is its bytes), under the log's lock.
///
/// # Errors
/// When the log cannot be read or written.
pub(crate) fn append(home: &Path, line: &str) -> std::io::Result<bool> {
    let p = path(home);
    std::fs::create_dir_all(home)?;
    crate::live::locked(&p, || {
        let text = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        if text.lines().any(|l| l == line) {
            return Ok(false);
        }
        crate::learn::append_line(&p, line).map(|()| true)
    })
}

/// The log's text: this machine's lines, then the events other nodes' hand-off lines carry (empty
/// without either).
pub(crate) fn read(home: &Path) -> String {
    let mut text = std::fs::read_to_string(path(home)).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&std::fs::read_to_string(mesh_path(home)).unwrap_or_default());
    text
}

/// Rebuilds `mesh_path` from the message log at `now`: the event of each fresh hand-off line another
/// node wrote (msg.rs `HandoffLine::well_formed`: its event is its node's own; fresh by the message
/// protocol's clock rule, msg.rs `fresh_at`), as the bytes `fit` writes, once each. Written whole when
/// it changed (facts.rs `write_atomic`), removed when there is none; the message log is read under the
/// file's lock, so the run that writes last derived from the newest log. Then `keep_outcomes`. Returns
/// whether it changed.
pub(crate) fn derive(home: &Path, now: i64) -> bool {
    let here = crate::msg::node(home);
    let p = mesh_path(home);
    let _ = std::fs::create_dir_all(super::wal::dir(home));
    let (changed, body) = crate::live::locked(&p, || {
        let log = std::fs::read_to_string(crate::msg::path(home)).unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        let body: String = log
            .lines()
            .filter_map(crate::msg::LineRef::parse)
            .filter_map(|l| match l {
                crate::msg::LineRef::Handoff(h) if h.node != here && crate::msg::fresh_at(h.ms, now) => serde_json::to_string(h.event()).ok(),
                _ => None,
            })
            .filter(|l| seen.insert(l.clone()))
            .map(|l| l + "\n")
            .collect();
        let changed = if std::fs::read_to_string(&p).unwrap_or_default() == body {
            false
        } else if body.is_empty() {
            std::fs::remove_file(&p).is_ok()
        } else {
            crate::facts::write_atomic(&p, body.as_bytes())
        };
        (changed, body)
    });
    keep_outcomes(home, &here, &body);
    changed
}

/// Keeps, in this machine's own log, the lines of `mesh` (other nodes' events) a fence here rests on:
/// an outcome citing a claim of an offer of this node while the remote was not seen holding one and
/// did not refute it; of an offer of this node or of a hand-off this machine took, an abort after the
/// take while the remote's `abort` ref was not seen and did not refute it, and the done line citing the
/// claim the remote holds (before it answered, the first one). So the message log's TTL and bound
/// neither lift a fence nor set one back, and what a member announces anew in each window does not
/// pile up. One line per claim and writer (`append` keeps a line once); once the remote answered,
/// `Known` decides, and `note` drops what it refuted.
fn keep_outcomes(home: &Path, here: &str, mesh: &str) {
    let ends: Vec<(&str, Event)> = mesh
        .lines()
        .filter_map(|l| serde_json::from_str::<Event>(l).ok().map(|e| (l, e)))
        .filter(|(_, e)| matches!(e.kind, Kind::Taken | Kind::Back | Kind::Aborted | Kind::Done) && e.well_formed())
        .collect();
    if ends.is_empty() {
        return;
    }
    let local = std::fs::read_to_string(path(home)).unwrap_or_default();
    let own: std::collections::HashSet<&str> =
        local.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()).filter(|e| e.kind == Kind::Offer && e.node == here).map(|e| id_of(&e)).collect();
    let took = |id: &str| {
        let text = super::wal::read(home, id);
        super::wal::ops(&text).iter().any(|(_, s)| matches!(s.first(), Some(super::wal::Step::Take { .. })))
    };
    let known = known(home);
    let mut kept_done: std::collections::HashSet<&str> =
        local.lines().filter_map(|l| serde_json::from_str::<Event>(l).ok()).filter(|e| e.kind == Kind::Done).map(|e| id_of(&e)).collect();
    for (line, e) in ends {
        let k = known.get(e.id.as_ref());
        let refuted = |sha: Option<&str>| k.is_some_and(|k| k.refuted.iter().any(|r| Some(r.as_str()) == sha));
        let mine = own.contains(e.id.as_ref()) || took(&e.id);
        let keep = match e.kind {
            // The one citing the claim the remote holds; before it answered, the first only.
            Kind::Done => {
                mine && match k.and_then(|k| k.holds.as_deref()) {
                    Some(holds) => e.claim.as_deref() == Some(holds),
                    None => !k.is_some_and(|k| k.gone) && !kept_done.contains(id_of(&e)),
                }
            }
            // Until the remote's `abort` ref was seen, unless it refuted this one.
            Kind::Aborted if e.abort.is_some() => mine && k.is_none_or(|k| k.abort.is_none()) && !refuted(e.abort.as_deref()),
            _ => own.contains(e.id.as_ref()) && k.is_none_or(|k| k.holds.is_none()) && !refuted(e.claim.as_deref()),
        };
        if keep && !local.lines().any(|l| l == line) && append(home, line).is_ok() && e.kind == Kind::Done {
            kept_done.insert(id_of(&e));
        }
    }
}

/// An event's id as the line spells it (an id is hex, never escaped: borrowed, `fold`).
fn id_of<'a>(e: &Event<'a>) -> &'a str {
    match e.id {
        Cow::Borrowed(s) => s,
        Cow::Owned(_) => "",
    }
}

/// Who made a claim, as its commit names them (mod.rs `parse_claim`): an outcome line names them when
/// its node, session and kind are these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Claimant {
    pub node: String,
    pub session: String,
    pub verb: Verb,
}

impl Claimant {
    fn names(&self, e: &Event) -> bool {
        e.node == self.node && e.session == self.session && verb_of(e.kind) == Some(self.verb)
    }
}

/// What this machine last heard from the arbiter remote of one hand-off: the claim its `taken` ref
/// holds (a claim, once created, is never moved: decisive) and who made it, the commit its `abort` ref
/// holds (as decisive), whether it holds no ref of the hand-off at all any more (`gone`: done), the
/// claims and aborts lines cited while the remote held another or none (forged: one is announced only
/// after the remote holds it), and, for an offer of this machine taken by another session, its changes
/// after the state that was taken (`after`, mod.rs `learned`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Known {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub holds: Option<String>,
    /// Who made `holds`, as its commit names them; none when borg could not read the commit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by: Option<Claimant>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gone: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refuted: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after: Vec<String>,
}

/// What one ask of the arbiter remote heard (`note`): its `taken` ref and who made that claim, its
/// `abort` ref, and whether it holds any ref of the hand-off.
#[derive(Debug, Clone, Default)]
pub(crate) struct Heard<'r> {
    pub taken: Option<&'r str>,
    pub by: Option<Claimant>,
    pub abort: Option<&'r str>,
    pub gone: bool,
}

/// The mark of what the remote said (msg.rs `mark_of`): a notice reads again once it moves.
pub(crate) fn mark_of_known(home: &Path) -> Option<String> {
    crate::msg::mark_of(&known_path(home))
}

/// What the remote said, per hand-off (`known_path`).
pub(crate) fn known(home: &Path) -> BTreeMap<String, Known> {
    std::fs::read_to_string(known_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

/// Records what the remote said of `h` (`heard`): the claim its `taken` ref holds and who made it, the
/// commit its `abort` ref holds, or that it holds nothing of it any more; each, once seen, stays (refs
/// of a hand-off are created once and only deleted when it is done). Every other claim and abort its
/// lines cite now is refuted, unless the remote is past the hand-off (`gone`), where a line's claim may
/// have been real; this machine's log drops the lines refuted (`drop_refuted`). Under the file's lock.
pub(crate) fn note(home: &Path, h: &HandOff, heard: Heard) {
    update(home, h.id(), |k| {
        if k.holds.is_none() {
            k.holds = heard.taken.map(str::to_string);
        }
        if k.by.is_none() && heard.taken.is_some() && k.holds.as_deref() == heard.taken {
            k.by = heard.by;
        }
        if k.abort.is_none() {
            k.abort = heard.abort.map(str::to_string);
        }
        k.gone |= heard.gone;
        if k.gone {
            return;
        }
        let cited = h.ends.iter().filter_map(|e| e.claim.as_deref()).filter(|c| Some(*c) != k.holds.as_deref());
        let aborts = h.aborts.iter().filter_map(|e| e.abort.as_deref()).filter(|a| Some(*a) != k.abort.as_deref());
        let wrong: Vec<String> = cited.chain(aborts).map(str::to_string).collect();
        for c in wrong {
            if !k.refuted.contains(&c) {
                k.refuted.push(c);
            }
        }
    });
    if let Some(k) = known(home).get(h.id()).filter(|k| !k.gone) {
        drop_refuted(home, h.id(), k);
    }
}

/// Drops from this machine's log the lines of hand-off `id` that what the remote said (`k`) refutes:
/// outcomes and aborts citing a commit it refuted, done lines citing a claim other than the one it
/// holds. Under the log's lock; written when something went.
fn drop_refuted(home: &Path, id: &str, k: &Known) {
    let wrong = |e: &Event| {
        let refuted = |sha: Option<&str>| sha.is_some_and(|s| k.refuted.iter().any(|r| r == s));
        e.id == id
            && match e.kind {
                Kind::Offer | Kind::Declined => false,
                Kind::Done => k.holds.as_deref().is_some_and(|h| e.claim.as_deref() != Some(h)),
                Kind::Aborted if e.abort.is_some() => refuted(e.abort.as_deref()),
                Kind::Taken | Kind::Back | Kind::Aborted => refuted(e.claim.as_deref()),
            }
    };
    let p = path(home);
    crate::live::locked(&p, || {
        let Ok(text) = std::fs::read_to_string(&p) else { return };
        let keep: String = text.lines().filter(|l| serde_json::from_str::<Event>(l).ok().is_none_or(|e| !wrong(&e))).map(|l| format!("{l}\n")).collect();
        if keep.len() != text.len() {
            crate::facts::write_atomic(&p, keep.as_bytes());
        }
    });
}

/// Records the changes of the offering tree after the state that was taken (`Known::after`), once.
pub(crate) fn note_after(home: &Path, id: &str, after: Vec<String>) {
    update(home, id, |k| {
        if k.after.is_empty() {
            k.after = after;
        }
    });
}

/// Changes what is known of hand-off `id` with `f`, under the file's lock; written when it changed.
fn update(home: &Path, id: &str, f: impl FnOnce(&mut Known)) {
    let p = known_path(home);
    let _ = std::fs::create_dir_all(super::wal::dir(home));
    crate::live::locked(&p, || {
        let mut all = known(home);
        let k = all.entry(id.to_string()).or_default();
        let before = k.clone();
        f(k);
        if *k != before {
            crate::facts::write_atomic(&p, serde_json::to_string(&all).unwrap_or_default().as_bytes());
        }
    });
}

/// Drops every line of the hand-offs `ids` from this machine's log, under its lock: they are done and
/// the remote holds nothing of them (`Known::gone`, which stays as their tombstone), so the log stays
/// bounded by the hand-offs still open or taken.
pub(crate) fn compact(home: &Path, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    let p = path(home);
    crate::live::locked(&p, || {
        let Ok(text) = std::fs::read_to_string(&p) else { return };
        let keep: String =
            text.lines().filter(|l| serde_json::from_str::<Event>(l).ok().is_none_or(|e| !ids.iter().any(|i| *i == e.id))).map(|l| format!("{l}\n")).collect();
        if keep.len() != text.len() {
            crate::facts::write_atomic(&p, keep.as_bytes());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONCE: &str = "0011223344556677";

    fn id() -> String {
        super::super::id_for("n1", "s1", NONCE)
    }

    fn sha(c: char) -> String {
        c.to_string().repeat(40)
    }

    fn offer_to(ms: i64, state: char, to: Option<&str>) -> String {
        let repo =
            Repo { name: "app".into(), remote: "/r".into(), branch: "main".into(), head: sha('1').into(), state: sha(state).into(), tree: sha('2').into() };
        let e = Event {
            v: 1,
            id: id().into(),
            kind: Kind::Offer,
            node: "n1".into(),
            session: "s1".into(),
            engineer: "a@x".into(),
            harness: "claude".into(),
            to: to.map(Into::into),
            nonce: Some(NONCE.into()),
            ms: Some(ms),
            repos: vec![repo],
            note: "next: tests".into(),
            brief_head: String::new().into(),
            brief: Some(sha('b').into()),
            left_out: vec![],
            more_left_out: 0,
            claim: None,
            state: None,
            why: "".into(),
            abort: None,
        };
        serde_json::to_string(&e).unwrap()
    }

    fn offer(ms: i64, state: char) -> String {
        offer_to(ms, state, None)
    }

    fn end(kind: Kind, claim: char) -> String {
        let base = offer(1, '3');
        let e = Event {
            kind,
            node: "n2".into(),
            session: "s2".into(),
            nonce: None,
            ms: None,
            repos: vec![],
            note: "".into(),
            brief: None,
            claim: Some(sha(claim).into()),
            state: Some(sha('3').into()),
            ..serde_json::from_str(&base).unwrap()
        };
        serde_json::to_string(&e).unwrap()
    }

    fn declined(node: &str, engineer: &str, ms: i64) -> String {
        let base = offer(1, '3');
        let e = Event {
            kind: Kind::Declined,
            node: node.into(),
            session: "s9".into(),
            engineer: engineer.into(),
            nonce: None,
            ms: Some(ms),
            repos: vec![],
            note: "".into(),
            brief: None,
            ..serde_json::from_str(&base).unwrap()
        };
        serde_json::to_string(&e).unwrap()
    }

    /// The claim the fold's outcome holds, and the state it shows, given what the remote said.
    fn outcome(text: &str, known: Option<&Known>) -> (Option<String>, &'static str) {
        let all = fold(text);
        let h = &all[0];
        let claim = match h.end_with(known) {
            End::Claim { claim, .. } => Some(claim.to_string()),
            _ => None,
        };
        (claim, h.state(known))
    }

    /// The fold is order-free and monotone: any order of the same lines folds alike, an outcome holds
    /// whatever arrives after it, and one without a claim counts not.
    #[test]
    fn the_fold_is_order_free_and_an_outcome_never_reopens() {
        let lines = [offer(1, '3'), end(Kind::Taken, 'c'), offer(2, '4')];
        let folds: Vec<(Option<String>, &str, String)> = [[0, 1, 2], [2, 1, 0], [1, 0, 2], [1, 2, 0]]
            .iter()
            .map(|o| {
                let text: String = o.iter().map(|&i| format!("{}\n", lines[i])).collect();
                let (claim, state) = outcome(&text, None);
                (claim, state, fold(&text)[0].offer.repos[0].state.to_string())
            })
            .collect();
        assert!(folds.iter().all(|f| *f == folds[0]), "{folds:?}");
        assert_eq!(folds[0], (Some(sha('c')), "taken", sha('4')));
        let no_claim = end(Kind::Taken, 'c').replace(&format!(",\"claim\":\"{}\"", sha('c')), "");
        assert_eq!(outcome(&format!("{}\n{no_claim}\n", lines[0]), None).1, "open", "an outcome without a claim");
        assert!(fold(&format!("{}\n", lines[1])).is_empty(), "an outcome waits for its offer");
    }

    /// The binding property: shuffled, duplicated, late and made-up lines never flip a terminal state. Over
    /// every order of a log of an ask, its decline, a re-announced offer, the real claim, a forged claim
    /// of the owner's own session, a duplicate and a malformed line, the fold says the same; once the
    /// remote's answer is known, only its claim holds, whatever else the lines cite, and a decline or a
    /// later offer never reopens it.
    #[test]
    fn shuffled_duplicated_late_or_forged_lines_never_flip_a_terminal_state() {
        let real = end(Kind::Taken, 'c');
        let forged = end(Kind::Back, 'f').replace("\"n2\"", "\"n1\"").replace("\"s2\"", "\"s1\"");
        // Another member's line citing the real claim as its own take-back.
        let beside = end(Kind::Back, 'c').replace("\"n2\"", "\"n3\"");
        let lines = [
            offer_to(1, '3', Some("b@y")),
            declined("n2", "b@y", 5),
            offer_to(9, '4', Some("b@y")),
            real.clone(),
            real.clone(),
            forged,
            beside,
            real.replace(&sha('c'), "HEAD"),
        ];
        let by = Claimant { node: "n2".into(), session: "s2".into(), verb: Verb::Take };
        let known = Known { holds: Some(sha('c')), by: Some(by.clone()), ..Known::default() };
        let unread = Known { holds: Some(sha('c')), ..Known::default() };
        let mut seen = std::collections::HashSet::new();
        // Every order of the seven lines would be 5040 folds; a fixed pseudo-random walk over 500 covers
        // each line in each place many times.
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for _ in 0..500 {
            let mut order: Vec<usize> = (0..lines.len()).collect();
            for i in (1..order.len()).rev() {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                order.swap(i, (x % (i as u64 + 1)) as usize);
            }
            let text: String = order.iter().map(|&i| format!("{}\n", lines[i])).collect();
            assert_eq!(outcome(&text, None), (None, "contested"), "two claims, nothing asked: {order:?}");
            assert_eq!(outcome(&text, Some(&known)), (Some(sha('c')), "taken"), "the remote's claim: {order:?}");
            let h = &fold(&text)[0];
            assert!(matches!(h.end_with(Some(&known)), End::Claim { by: Some(e), .. } if by.names(e)), "its claimer's line: {order:?}");
            assert!(matches!(h.end_with(Some(&unread)), End::Claim { by: None, .. }), "lines that disagree name nobody: {order:?}");
            assert_eq!(outcome(&text, Some(&unread)).1, "taken", "never flipped to a take-back: {order:?}");
            let refuted = Known { refuted: vec![sha('f')], ..Known::default() };
            assert_eq!(outcome(&text, Some(&refuted)), (Some(sha('c')), "taken"), "the forged one refuted: {order:?}");
            seen.insert(order);
        }
        assert!(seen.len() > 400, "{}", seen.len());
        let one = format!("{}\n{}\n", lines[0], declined("n2", "b@y", 5)) + &format!("{}\n", declined("n4", "B@y", 3));
        assert_eq!(fold(&one)[0].decline.as_ref().map(|d| d.node.as_ref()), Some("n2"), "one decline of whom it asks, by a total order");
        // Before any claim: asked, then declined by whom it asks; nobody else's decline counts.
        let ask = format!("{}\n", lines[0]);
        assert_eq!(outcome(&ask, None).1, "asked");
        assert_eq!(outcome(&format!("{ask}{}\n", declined("n7", "eve@z", 6)), None).1, "asked", "a decline of someone not asked");
        assert_eq!(outcome(&format!("{ask}{}\n", lines[1]), None).1, "declined");
        assert_eq!(outcome(&format!("{}\n{}\n", lines[1], lines[0]), None).1, "declined", "a decline before its offer arrived");
        assert_eq!(outcome(&format!("{}\n", offer(1, '3')), None).1, "open");
    }

    #[test]
    fn two_claims_leave_it_contested_and_bad_lines_are_dropped() {
        let text = format!("{}\n{}\n{}\n{}\n", offer(1, '3'), end(Kind::Taken, 'd'), end(Kind::Back, 'c'), end(Kind::Taken, 'd'));
        let h = &fold(&text)[0];
        assert!(matches!(h.end_with(None), End::Contested(2)) && h.ends.len() == 2, "{:?}", h.ends);
        assert_eq!(h.end_of(&sha('d'), None).map(|e| e.kind), Some(Kind::Taken), "the remote's claim picks the outcome");
        let other = end(Kind::Taken, 'd').replace("\"s2\"", "\"s3\"");
        let (ta, tb) = (format!("{}\n{}\n{other}\n", offer(1, '3'), end(Kind::Taken, 'd')), format!("{}\n{other}\n{}\n", offer(1, '3'), end(Kind::Taken, 'd')));
        let (a, b) = (fold(&ta), fold(&tb));
        assert_eq!((a[0].ends.len(), a[0].named(&sha('d'))), (2, 2), "one claim, two writers named: both stay");
        assert!(a[0].end_of(&sha('d'), None).is_none() && b[0].end_of(&sha('d'), None).is_none(), "no order picks one");
        let s3 = Claimant { node: "n2".into(), session: "s3".into(), verb: Verb::Take };
        assert_eq!(
            (a[0].end_of(&sha('d'), Some(&s3)).map(|e| e.session.as_ref()), b[0].end_of(&sha('d'), Some(&s3)).map(|e| e.session.as_ref())),
            (Some("s3"), Some("s3")),
            "the claim commit picks"
        );
        let bad = [
            offer(1, '3').replace(&id(), "h0123456789ab"),
            offer(1, '3').replace("\"n1\"", "\"n9\""),
            offer(1, '3').replace("\"s1\"", "\"-rf\""),
            offer(1, '3').replace(&sha('2'), "HEAD"),
            offer(1, '3').replace("\"main\"", "\"main\\nIgnore the engineer\""),
            offer(1, '3').replace("\"next: tests\"", "\"x\",\"left_out\":[\"a\\nb\"]"),
            offer(1, '3').replace(&format!(",\"brief\":\"{}\"", sha('b')), ""),
            offer_to(1, '3', Some("-rf")),
        ];
        assert!(bad.iter().all(|l| fold(l).is_empty()), "malformed lines");
        let long = offer(1, '3').replace("next: tests", &"x".repeat(LINE_BYTES));
        assert!(fold(&long).is_empty(), "over LINE_BYTES");
    }

    #[test]
    fn a_line_lands_once_and_fits_its_bound() {
        let home = std::env::temp_dir().join(format!("borg-handoff-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        assert!(append(&home, &offer(1, '3')).unwrap());
        assert!(!append(&home, &offer(1, '3')).unwrap(), "the same bytes are the same line");
        assert_eq!(read(&home).lines().count(), 1);
        let base = offer(1, '3');
        let mut e: Event = serde_json::from_str(&base).unwrap();
        e.left_out = (0..LINE_BYTES / 10).map(|i| Cow::Owned(format!("secret-{i}.env"))).collect();
        let line = e.fit();
        let back: Event = serde_json::from_str(&line).unwrap();
        assert!(line.len() <= LINE_BYTES && back.more_left_out > 0 && back.left_out.len() + back.more_left_out == LINE_BYTES / 10);
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A peer's outcome line of an offer of this node outlives the message log: kept in this machine's
    /// log once derived, so the fence never rests on the message TTL; once the remote holds a claim, a
    /// line citing another is not kept.
    #[test]
    fn a_peers_outcome_of_an_own_offer_is_kept_past_the_message_log() {
        let home = std::env::temp_dir().join(format!("borg-handoff-keep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let here = crate::msg::node(&home);
        let base = offer(1, '3');
        let mut o: Event = serde_json::from_str(&base).unwrap();
        let own = super::super::id_for(&here, "s1", NONCE);
        (o.node, o.id) = (here.as_str().into(), own.as_str().into());
        append(&home, &serde_json::to_string(&o).unwrap()).unwrap();
        let peer = "b".repeat(64);
        let taken = |c: char| end(Kind::Taken, c).replace(&id(), &own).replace("\"n2\"", &format!("\"{peer}\""));
        let announced = |line: &str, now: i64| serde_json::to_string(&crate::msg::HandoffLine::new(&peer, now, serde_json::from_str(line).unwrap())).unwrap();
        let now = crate::learn::now_ms();
        let line = taken('c');
        std::fs::write(crate::msg::path(&home), format!("{}\n", announced(&line, now))).unwrap();
        assert!(derive(&home, now));
        assert!(std::fs::read_to_string(path(&home)).unwrap().lines().any(|l| l == line), "kept here");
        assert!(derive(&home, now + crate::msg::TTL_MS + 1) && !mesh_path(&home).exists(), "the message log's copy expired");
        assert_eq!(fold(&read(&home))[0].state(None), "taken", "the outcome stays");
        let text = read(&home);
        note(&home, &fold(&text)[0], Heard { taken: Some(&sha('c')), ..Heard::default() });
        std::fs::write(crate::msg::path(&home), format!("{}\n", announced(&taken('f'), now))).unwrap();
        derive(&home, now);
        assert!(!std::fs::read_to_string(path(&home)).unwrap().contains(&sha('f')), "decided: a line citing another claim is not kept");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// What the remote said is kept: a claim seen held stays, and the claims cited meanwhile that it did
    /// not hold are refuted.
    #[test]
    fn what_the_remote_said_is_kept() {
        let home = std::env::temp_dir().join(format!("borg-handoff-known-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let text = format!("{}\n{}\n", offer(1, '3'), end(Kind::Taken, 'f'));
        let h = &fold(&text)[0];
        note(&home, h, Heard::default());
        assert_eq!(known(&home).get(h.id()), Some(&Known { refuted: vec![sha('f')], ..Known::default() }));
        let by = Claimant { node: "n2".into(), session: "s2".into(), verb: Verb::Take };
        note(&home, h, Heard { taken: Some(&sha('c')), by: Some(by.clone()), ..Heard::default() });
        note(&home, h, Heard { taken: Some(&sha('d')), ..Heard::default() });
        let k = known(&home).get(h.id()).cloned().unwrap_or_default();
        assert_eq!((k.holds, k.by), (Some(sha('c')), Some(by)), "a claim is never moved");
        // Past its end the remote holds nothing: lines' claims are no longer refuted, and it is done.
        let other = format!("{}\n{}\n", offer(1, '3'), end(Kind::Taken, 'e'));
        let h2 = &fold(&other)[0];
        let home2 = home.join("gone");
        note(&home2, h2, Heard { gone: true, ..Heard::default() });
        let k2 = known(&home2).get(h2.id()).cloned().unwrap_or_default();
        assert!(k2.gone && k2.refuted.is_empty() && h2.closed(Some(&k2)) && h2.state(Some(&k2)) == "done", "{k2:?}");
        let _ = std::fs::remove_dir_all(&home);
    }
}
