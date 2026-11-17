//! Closing a hand-off: abort, done, and the collective's switch.
//!
//! **Abort** (`borg handoff abort <id> [--why "<text>"]`), by the offering engineer or a member whose
//! role in the held membership is admin or steer (msg.rs `steer_right`, as steering checks it). On an
//! open or asked offer it claims the same `taken` ref (verb `abort`), so an abort and a take race
//! through one compare-and-swap register and exactly one wins; the work refs (`state`, `brief`) are
//! deleted then, the claim stays as the record until `done`, whom it asks and its owner are told, and
//! nobody is fenced: the owner keeps the work. After a take it creates the hand-off's second register,
//! `abort` (its commit's parent: the take's claim, the fencing token it cites), once; both sides are
//! told and fenced on the hand-off (fence.rs) until `done`. Nothing is deleted from anyone's tree.
//! Every abort names who and why, in its commit and its line.
//!
//! **Done** (`borg handoff done <id>`), by whoever holds the work (the claimer), the offering engineer,
//! an admin or a steerer, once a claim holds: deletes every `refs/borg/handoff/<id>/*` ref from each
//! repo's remote it has a clone of (each only while it holds what was read), writes the done line, and
//! removes what this machine's take left: a work tree only when it is clean and holds no ignored file,
//! the local refs; branches and the rest are listed. Each step is logged ahead and idempotent (wal.rs).
//!
//! **The switch**: an admin's `handoffs_off` in the signed membership (borg-mesh membership.rs, set by
//! `borg handoff off`). While it is there, offers and new claims are refused with the reason and
//! the admin, and each node aborts its own open offers at its next `borg handoff` (or a stop's); taken
//! ones stay with their takers. Switched on again, nothing reopens. borg reads the document as the mesh
//! saved it (verified there), as steering does.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::wal::{Halt, Op, Step, Verb, Who};
use super::{Claimer, Ctx, Decided, Refs, Target, git, label, log, short, wal};
use crate::{msg, notes, presence, stats};

/// The fields of borg-mesh membership.rs `Membership` the switch needs.
#[derive(Deserialize)]
struct Held<'a> {
    #[serde(borrow, default)]
    handoffs_off: Option<Off<'a>>,
}

#[derive(Deserialize)]
struct Off<'a> {
    #[serde(borrow)]
    by: Cow<'a, str>,
}

/// The admin who switched hand-offs off in the collective of `home` (as much of the key as a label
/// shows); none while they are on, or without a membership.
fn off_by(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("mesh").join("membership.json")).ok()?;
    let held: Held = serde_json::from_str(&text).ok()?;
    Some(msg::short_node(&held.handoffs_off?.by))
}

/// Why hand-offs are refused in the collective of `home`: switched off in its membership, by which
/// admin; none while they are on (or there is no membership).
pub(crate) fn switched_off(home: &Path) -> Option<String> {
    off_by(home).map(|by| {
        format!("hand-offs are switched off in this collective by admin {by} (its membership): no new offer or take; an admin's `borg handoff on` switches them on again")
    })
}

/// Whether `me` may close `h` (`what`: abort, or finish it as done): its offering engineer or session,
/// the claimer when `claimer` names one, or a member the held membership makes admin or steer; else
/// why not.
fn may_close(ctx: &Ctx, h: &log::HandOff, claimer: Option<&log::Event>, what: &str) -> Result<(), String> {
    let me = &ctx.me;
    let o = &h.offer;
    let owner = (!me.engineer.is_empty() && o.engineer.eq_ignore_ascii_case(&me.engineer)) || (o.node == me.node && o.session == me.session);
    let holder =
        claimer.is_some_and(|c| (!me.engineer.is_empty() && c.engineer.eq_ignore_ascii_case(&me.engineer)) || (c.node == me.node && c.session == me.session));
    if owner || holder {
        return Ok(());
    }
    match msg::steer_right(&ctx.home, &me.node) {
        Ok(_) => Ok(()),
        Err(why) => {
            let who = if claimer.is_some() { "the offering engineer, whoever took it," } else { "the offering engineer" };
            Err(format!("only {who} or a member with the admin or steer role may {what} {}; {why}", o.id))
        }
    }
}

/// The reason an abort gives: one line, redacted, cut as a note is (presence.rs `first_line`).
fn reason(why: Option<&str>) -> Result<String, String> {
    match why.map(str::trim) {
        Some(w) if w.contains('\n') => Err("--why is one line".into()),
        Some(w) => Ok(notes::redact(&presence::first_line(w))),
        None => Ok(String::new()),
    }
}

/// `borg handoff abort <id> [--why "<text>"]`.
pub(super) fn abort(ctx: &Ctx, id: &str, why: Option<&str>) -> i32 {
    if !super::is_id(id) {
        eprintln!("borg handoff abort: {id:?} is no hand-off id (h and 12 hex; `borg handoff` lists them)");
        return 2;
    }
    let why = match reason(why) {
        Ok(w) => w,
        Err(e) => return super::usage(&e),
    };
    super::recover(ctx, Some(id));
    log::derive(&ctx.home, ctx.now);
    let text = log::read(&ctx.home);
    let all = log::fold(&text);
    let Some(h) = all.iter().find(|h| h.id() == id) else {
        eprintln!("borg handoff abort: no hand-off {id} is known on this machine (`borg handoff` lists them)");
        return 1;
    };
    if let Err(e) = may_close(ctx, h, None, "abort") {
        eprintln!("borg handoff abort: refused: {e}");
        return 1;
    }
    said(abort_steps(ctx, h, &why), "abort")
}

/// The exit of a closing command, with what it did or why not.
fn said(r: Result<String, Halt>, what: &str) -> i32 {
    match r {
        Ok(s) => {
            println!("{s}");
            0
        }
        Err(Halt::Stopped(s)) => {
            eprintln!("borg handoff {what}: stopped after {s}");
            3
        }
        Err(e) => {
            eprintln!("borg handoff {what}: {e}");
            1
        }
    }
}

/// While hand-offs are switched off: this machine's open offer `h` is aborted by its own offering
/// session, whose node may always close its offer, naming the admin who switched them off.
pub(super) fn abort_switched(ctx: &Ctx, h: &log::HandOff) {
    let Some(by) = off_by(&ctx.home) else { return };
    let o = &h.offer;
    let owner = Ctx {
        home: ctx.home.clone(),
        cwd: ctx.cwd.clone(),
        me: Who {
            node: o.node.to_string().into(),
            session: o.session.to_string().into(),
            engineer: o.engineer.to_string().into(),
            harness: o.harness.to_string().into(),
        },
        now: ctx.now,
    };
    match abort_steps(&owner, h, &format!("hand-offs were switched off for the collective by admin {by}")) {
        Ok(s) => println!("borg handoff: {s}"),
        Err(e) => eprintln!("borg handoff: {} stays open for now: {e}", h.id()),
    }
}

/// The abort of `h` by `ctx.me`: the claim of `taken` while it is open, else the commit of `abort`
/// after its take (module doc).
fn abort_steps(ctx: &Ctx, h: &log::HandOff, why: &str) -> Result<String, Halt> {
    let id = h.id();
    let arb = &h.offer.repos[0];
    let offering = super::offered_from(&ctx.home, h).map(PathBuf::from).filter(|r| r.is_dir());
    let super::Reach { root, remote, url } =
        super::reach(ctx, id, arb, offering.as_deref(), super::Holds::Arbiter, super::Look::Proven).map_err(Halt::Failed)?;
    let op = Op::begin(&ctx.home, id).map_err(Halt::Io)?;
    let held = git::ls_remote_all(&root, &url, id).map_err(|e| Halt::Failed(format!("cannot reach {remote} ({e}): nothing aborted")))?;
    let leaf = |l: &str| held.iter().find(|(k, _)| k == l).map(|(_, v)| v.clone());
    let (taken, aborted) = (leaf("taken"), leaf("abort"));
    if held.is_empty() {
        log::note(&ctx.home, h, log::Heard { gone: true, ..log::Heard::default() });
        return super::fail(format!("{remote} holds no ref of {id} any more: it is done"));
    }
    if taken.is_some() {
        super::learned(ctx, h, &root, &url, log::Heard { taken: taken.as_deref(), abort: aborted.as_deref(), ..log::Heard::default() });
        super::fence::refresh(&ctx.home);
    }
    let after = match (&taken, &aborted) {
        (None, _) => None,
        (Some(_), Some(a)) => return super::fail(format!("{id} was aborted after its take already (abort {})", short(a))),
        (Some(v), None) => {
            let c = super::claimer(&root, &url, id, v).ok_or_else(|| Halt::Failed(format!("{id} is claimed by {}, a commit borg cannot read", short(v))))?;
            if c.verb == Verb::Abort {
                return super::fail(format!("{id} was aborted already by {}", label(&c.engineer, &c.session, &c.node)));
            }
            Some(v.clone())
        }
    };
    let refs = Refs::of(id);
    let (parent, brief, leaf) = match &after {
        None => {
            git::fetch(&root, &url, &[&refs.state, &refs.brief])
                .map_err(|e| Halt::Failed(format!("cannot fetch the offer from {remote} ({e}): nothing aborted")))?;
            (git::rev_parse(&root, &refs.state).unwrap_or_default(), git::rev_parse(&root, &refs.brief).unwrap_or_default(), "taken")
        }
        Some(v) => (v.clone(), super::claimer(&root, &url, id, v).map(|c| c.brief).unwrap_or_default(), "abort"),
    };
    if !git::is_oid(&parent) || !git::is_oid(&brief) {
        return super::fail(format!("{remote} holds no state and brief of {id} to abort"));
    }
    op.log(Step::Abort {
        by: ctx.me.clone(),
        why: why.into(),
        root: root.to_string_lossy().into_owned().into(),
        remote: remote.clone().into(),
        url: url.clone().into(),
        after: after.clone().map(Into::into),
    })?;
    let mut c = Claimer::of(&ctx.me, Verb::Abort, false, &brief, if after.is_some() { Vec::new() } else { vec![parent.clone()] });
    c.why = why.to_string();
    c.after = after.clone();
    let tree = git::tree_of(&root, &parent).unwrap_or_default();
    let sha = match git::commit_tree(&root, &tree, &parent, &super::claim_message(id, &c)) {
        Ok(sha) => sha,
        Err(e) => return super::end(&op, format!("no abort commit ({e}): nothing aborted")),
    };
    op.log(Step::Claim { sha: sha.clone().into() })?;
    let pushed = git::push(&root, &url, &[(&sha, &git::remote_ref(id, leaf))]);
    op.log(Step::Sent)?;
    let target = Target { root: root.clone(), remote: remote.clone(), url: url.clone(), verb: Verb::Abort, in_place: false };
    match super::decide(&ctx.me, &op, &target, id, &sha, leaf)? {
        Decided::Won(v) => finish(ctx, h, &op, &root, &url, &v),
        Decided::Lost { why, held } => {
            let heard = if leaf == "taken" {
                log::Heard { taken: Some(&held), ..log::Heard::default() }
            } else {
                log::Heard { taken: after.as_deref(), abort: Some(&held), ..log::Heard::default() }
            };
            super::learned(ctx, h, &root, &url, heard);
            super::fence::refresh(&ctx.home);
            super::end(&op, why)
        }
        Decided::Missing => super::end(&op, format!("{remote} refused the abort ({}): nothing aborted", pushed.err().unwrap_or_default())),
    }
}

/// The line of the abort commit `sha` after the take whose claim is `after` (its parent): built from
/// the commit alone, so every writer writes the same bytes.
fn abort_line(id: &str, sha: &str, after: &str, state: &str, c: &Claimer) -> String {
    let mut e = log::Event::of(id, log::Kind::Aborted, &c.node, &c.session, &c.engineer, &c.harness);
    e.claim = Some(after.into());
    e.state = Some(state.into());
    e.abort = Some(sha.into());
    e.why = c.why.as_str().into();
    e.fit()
}

/// The abort commit `a` on the `abort` ref of `h` (after the take `after`) as this machine learned it
/// from the remote at `url`: its line, when none here names it.
pub(super) fn learned_abort(ctx: &Ctx, h: &log::HandOff, root: &Path, url: &str, a: &str, after: &str) {
    if h.aborts.iter().any(|e| e.abort.as_deref() == Some(a)) {
        return;
    }
    if !git::has_commit(root, a) && git::fetch(root, url, &[&git::remote_ref(h.id(), "abort")]).is_err() {
        return;
    }
    let Some(c) = git::message(root, a).and_then(|m| super::parse_claim(&m, h.id())) else { return };
    let state = git::rev_parse(root, &format!("{after}^")).unwrap_or_default();
    if c.verb == Verb::Abort && c.after.as_deref() == Some(after) && git::is_oid(&state) {
        let _ = log::append(&ctx.home, &abort_line(h.id(), a, after, &state, &c));
    }
}

/// The steps after a won abort, each idempotent: its line, what the remote said kept, the fences;
/// an open offer's work refs deleted in every repo it has a clone of; whom it concerns told; done.
fn finish(ctx: &Ctx, h: &log::HandOff, op: &Op, root: &Path, url: &str, sha: &str) -> Result<String, Halt> {
    let id = h.id();
    let c = git::message(root, sha).and_then(|m| super::parse_claim(&m, id)).ok_or_else(|| Halt::Failed(format!("the abort {} cannot be read", short(sha))))?;
    super::keep_offer(ctx, h).map_err(Halt::Io)?;
    let why = if c.why.is_empty() { String::new() } else { format!(" ({})", c.why) };
    let by = label(&c.engineer, &c.session, &c.node);
    let (line, heard, mut said) = match &c.after {
        None => {
            let state = git::rev_parse(root, &format!("{sha}^")).unwrap_or_default();
            let heard = log::Heard { taken: Some(sha), by: Some(super::claimant(&c)), ..log::Heard::default() };
            (super::outcome_line(id, sha, &c, &state), heard, format!("aborted hand-off {id}{why}: nobody takes it; the offering tree keeps the work"))
        }
        Some(after) => {
            let state = git::rev_parse(root, &format!("{after}^")).unwrap_or_default();
            let heard = log::Heard { taken: Some(after), abort: Some(sha), ..log::Heard::default() };
            let said =
                format!("aborted hand-off {id} after its take{why}: both sides' edits on it are refused until `borg handoff done {id}`; nothing was deleted");
            (abort_line(id, sha, after, &state, &c), heard, said)
        }
    };
    log::append(&ctx.home, &line).map_err(Halt::Io)?;
    log::note(&ctx.home, h, heard);
    let announced = super::announce(&ctx.home, &line, ctx.now);
    op.log(Step::Logged)?;
    super::fence::refresh(&ctx.home);
    if c.after.is_none() {
        let left = clear(ctx, h, op, &["state", "brief", "probe"])?;
        if !left.is_empty() {
            said.push_str(&format!("; its state stays on {} until `borg handoff done {id}` runs in a clone of it", left.join(", ")));
        }
    }
    let mut to: Vec<String> = Vec::new();
    let o = &h.offer;
    if !(o.node == c.node && o.session == c.session) {
        to.push(format!("{}@{}", o.session, o.node));
    }
    if let Some(email) = o.to.as_deref().filter(|t| t.contains('@') && c.after.is_none()) {
        to.push(format!("{}:{email}", msg::ENGINEER));
    }
    if let Some(take) = c.after.as_deref().and_then(|a| super::claimer(root, url, id, a)) {
        to.push(format!("{}@{}", take.session, take.node));
    }
    let text = match &c.after {
        None => format!("Hand-off {id} was aborted by {by}{why} (abort {}): nobody takes it; the offering tree keeps its work.", short(sha)),
        Some(_) => format!(
            "Hand-off {id} was aborted after its take by {by}{why} (abort {}): edits on it are refused on both sides until it is resolved with `borg handoff done {id}`; nothing was deleted.",
            short(sha)
        ),
    };
    let from = msg::Sender {
        by: c.session.clone(),
        name: if c.engineer.is_empty() { msg::short_session(&c.session) } else { c.engineer.clone() },
        harness: c.harness.clone(),
        node: c.node.clone(),
    };
    let sent = msg::read(&ctx.home);
    for t in to.iter().filter(|t| **t != format!("{}@{}", c.session, c.node)) {
        if sent.iter().any(|m| m.to == *t && m.text.contains(short(sha))) {
            continue;
        }
        if let Err(e) = msg::send(&ctx.home, msg::Message::new(msg::TELL, &from, t, None, ctx.now, &text), ctx.now) {
            eprintln!("borg handoff: {t} is not told in a message ({e}); `borg handoff` shows the abort wherever they look");
        }
    }
    op.log(Step::Told)?;
    if announced {
        op.log(Step::Done)?;
    }
    stats::count(&ctx.home, stats::HANDOFF, &c.session, |ev| ev.why = "abort".into());
    Ok(said)
}

/// Deletes the refs `leaves` of `h` (all of them when empty) from each repo's remote this machine has
/// a clone of, each only while it holds what `ls-remote` read, logging each repo cleared; the remotes
/// it has no clone of (whose refs stay).
fn clear(ctx: &Ctx, h: &log::HandOff, op: &Op, leaves: &[&str]) -> Result<Vec<String>, Halt> {
    let id = h.id();
    let roots = super::offered_roots(&ctx.home, h);
    let mut left = Vec::new();
    for (i, r) in h.offer.repos.iter().enumerate() {
        let first = roots.get(i).map(PathBuf::from).filter(|p| p.is_dir());
        let holds = if i == 0 { super::Holds::Arbiter } else { super::Holds::State(&[r.state.as_ref()]) };
        let Ok(super::Reach { root, remote, url }) = super::reach(ctx, id, r, first.as_deref(), holds, super::Look::Proven) else {
            left.push(notes::redact(&r.remote));
            continue;
        };
        let held = git::ls_remote_all(&root, &url, id)
            .map_err(|e| Halt::Failed(format!("cannot reach {remote} of {} ({e}): run it again once it answers", r.name)))?;
        let refs: Vec<(String, String)> = held
            .into_iter()
            .filter(|(leaf, _)| leaves.is_empty() || leaves.contains(&leaf.as_str()))
            .map(|(leaf, sha)| (git::remote_ref(id, &leaf), sha))
            .collect();
        if !refs.is_empty() {
            let moves: Vec<(&str, &str, &str)> = refs.iter().map(|(r, s)| ("", r.as_str(), s.as_str())).collect();
            git::leased(&root, &url, &moves).map_err(|e| Halt::Failed(format!("{remote} of {} refused deleting the hand-off refs ({e})", r.name)))?;
        }
        op.log(Step::Cleared { repo: i })?;
    }
    Ok(left)
}

/// An abort that was interrupted: ended before its claim; decided by the remote's ref after it was
/// sent; finished once won.
pub(super) fn recover_abort(ctx: &Ctx, id: &str, op: &Op, steps: &[Step]) -> Result<String, Halt> {
    let Some(Step::Abort { by, root, remote, url, after, .. }) = steps.first() else { return super::fail("no abort".into()) };
    let text = log::read(&ctx.home);
    let all = log::fold(&text);
    let h = all.iter().find(|h| h.id() == id).ok_or_else(|| Halt::Failed("its offer is not in the log".into()))?;
    let root = PathBuf::from(root.as_ref());
    let won = steps.iter().rev().find_map(|s| match s {
        Step::Won { sha } => Some((sha.to_string(), true)),
        Step::Claim { sha } => Some((sha.to_string(), false)),
        _ => None,
    });
    let sha = match won {
        None => {
            op.log(Step::Ended { why: "interrupted before its commit was pushed".into() })?;
            return Ok("ended the abort: interrupted before anything was pushed".into());
        }
        Some((sha, true)) => sha,
        Some((sha, false)) => {
            let leaf = if after.is_some() { "abort" } else { "taken" };
            let target = Target { root: root.clone(), remote: remote.to_string(), url: url.to_string(), verb: Verb::Abort, in_place: false };
            match super::decide(by, op, &target, id, &sha, leaf)? {
                Decided::Won(v) => v,
                Decided::Lost { why, .. } => {
                    op.log(Step::Ended { why: why.as_str().into() })?;
                    return Ok(format!("ended the abort: {why}"));
                }
                Decided::Missing => {
                    op.log(Step::Ended { why: "the abort never reached the remote".into() })?;
                    return Ok("ended the abort: it never reached the remote".into());
                }
            }
        }
    };
    let me = Ctx { home: ctx.home.clone(), cwd: ctx.cwd.clone(), me: owned(by), now: ctx.now };
    finish(&me, h, op, &root, url, &sha)
}

/// `w` with owned names, so it outlives the log it was read from.
fn owned(w: &Who) -> Who<'static> {
    Who {
        node: w.node.to_string().into(),
        session: w.session.to_string().into(),
        engineer: w.engineer.to_string().into(),
        harness: w.harness.to_string().into(),
    }
}

/// `borg handoff done <id>`.
pub(super) fn done(ctx: &Ctx, id: &str) -> i32 {
    if !super::is_id(id) {
        eprintln!("borg handoff done: {id:?} is no hand-off id (h and 12 hex; `borg handoff` lists them)");
        return 2;
    }
    super::recover(ctx, Some(id));
    log::derive(&ctx.home, ctx.now);
    {
        let text = log::read(&ctx.home);
        if let Some(h) = log::fold(&text).iter().find(|h| h.id() == id) {
            super::ask_remote(ctx, h);
        }
    }
    let text = log::read(&ctx.home);
    let all = log::fold(&text);
    let Some(h) = all.iter().find(|h| h.id() == id) else {
        eprintln!("borg handoff done: no hand-off {id} is known on this machine (`borg handoff` lists them)");
        return 1;
    };
    let known = log::known(&ctx.home);
    let k = known.get(id);
    let claimer = match h.end_with(k) {
        log::End::Claim { by, .. } => by,
        log::End::Open if !h.closed(k) => {
            eprintln!("borg handoff done: {id} is open: nobody holds it; take it back (`borg handoff take {id}`) or abort it first");
            return 1;
        }
        log::End::Contested(_) => {
            eprintln!("borg handoff done: claims of {id} disagree and its remote could not be asked: run it again in a clone of {}", h.offer.repos[0].name);
            return 1;
        }
        log::End::Open => None,
    };
    if let Err(e) = may_close(ctx, h, claimer, "finish") {
        eprintln!("borg handoff done: refused: {e}");
        return 1;
    }
    let run = || -> Result<String, Halt> {
        let op = Op::begin(&ctx.home, id).map_err(Halt::Io)?;
        op.log(Step::Close { by: ctx.me.clone() })?;
        close_steps(ctx, h, &op)
    };
    said(run(), "done")
}

/// The steps of `done`, each idempotent: every repo's hand-off refs deleted, the done line (citing
/// the claim that holds), the remote noted as holding none, the fences gone, this machine's
/// leftovers removed or listed, done.
fn close_steps(ctx: &Ctx, h: &log::HandOff, op: &Op) -> Result<String, Halt> {
    let id = h.id();
    let left = clear(ctx, h, op, &[])?;
    let known = log::known(&ctx.home);
    let claim = match h.end_with(known.get(id)) {
        log::End::Claim { claim, .. } => Some(claim.to_string()),
        _ => known.get(id).and_then(|k| k.holds.clone()),
    };
    if let Some(claim) = &claim {
        let mut e = log::Event::of(id, log::Kind::Done, &ctx.me.node, &ctx.me.session, &ctx.me.engineer, &ctx.me.harness);
        e.claim = Some(claim.as_str().into());
        let line = e.fit();
        log::append(&ctx.home, &line).map_err(Halt::Io)?;
        super::announce(&ctx.home, &line, ctx.now);
    }
    if left.is_empty() {
        log::note(&ctx.home, h, log::Heard { gone: true, ..log::Heard::default() });
    }
    op.log(Step::Logged)?;
    super::fence::refresh(&ctx.home);
    let kept = leftovers(ctx, id);
    op.log(Step::Done)?;
    stats::count(&ctx.home, stats::HANDOFF, &ctx.me.session, |ev| ev.why = "done".into());
    let mut out = format!(
        "hand-off {id} is done: its refs are deleted from the remote and its fences on this machine are lifted; other machines lift theirs once they ask the remote (a stop of a fenced session, or any `borg handoff` there)"
    );
    if !left.is_empty() {
        out.push_str(&format!("\n  refs stay on {} (no clone of it here): run `borg handoff done {id}` in a clone of it", left.join(", ")));
    }
    for k in kept {
        out.push_str(&format!("\n  {k}"));
    }
    Ok(out)
}

/// Removes what a take of `id` left on this machine: each work tree it made (only when clean and holding
/// no ignored file, and not the one `borg handoff` runs in), then in each clone what loses nothing
/// (`left_in`: the local refs, a branch whose commits are merged or pushed); lists the rest (a work tree
/// with changes or ignored files, a branch holding commits nothing else has).
fn leftovers(ctx: &Ctx, id: &str) -> Vec<String> {
    let text = wal::read(&ctx.home, id);
    let mut out = Vec::new();
    let mut clones: Vec<PathBuf> = Vec::new();
    for (_, steps) in wal::ops(&text) {
        let Some(Step::Take { root, more, .. }) = steps.first() else { continue };
        let roots: Vec<PathBuf> =
            std::iter::once(PathBuf::from(root.as_ref())).chain(more.iter().filter(|r| !r.root.is_empty()).map(|r| PathBuf::from(r.root.as_ref()))).collect();
        for s in &steps {
            let Step::Checkout { repo, path, .. } = s else { continue };
            let (Some(clone), path) = (roots.get(*repo), PathBuf::from(path.as_ref())) else { continue };
            if !clones.contains(clone) {
                clones.push(clone.clone());
            }
            if path != *clone && path.is_dir() {
                let shown = super::quote(&path.to_string_lossy()).into_owned();
                // `git worktree remove` refuses changes and untracked files, and deletes ignored ones.
                let ignored = git::ignored(&path);
                if ctx.cwd.starts_with(&path) {
                    out.push(format!("the work tree {} stays: you are in it (`git worktree remove {shown}` once you are out)", path.display()));
                } else if !ignored.is_empty() {
                    out.push(format!(
                        "the work tree {} stays: it holds ignored files ({}); `git worktree remove {shown}` once you have them",
                        path.display(),
                        ignored.join(", ")
                    ));
                } else {
                    match git::worktree_remove(clone, &path) {
                        Ok(()) => out.push(format!("removed the work tree {}", path.display())),
                        Err(e) => out.push(format!("the work tree {} stays: {e}", path.display())),
                    }
                }
            }
        }
    }
    for clone in clones {
        out.extend(remove_left(&clone, id));
    }
    out
}

/// What a done hand-off `id` left in `clone`: what deleting loses nothing (`gone`: the local
/// `refs/borg/handoff/<id>/*` refs, copies of what `done` deletes from the remote; each branch of the
/// take no work tree has checked out whose commits another branch, a remote-tracking branch or a tag
/// holds), and why each other branch stays (`kept`).
struct Left {
    gone: Vec<(String, String)>,
    kept: Vec<String>,
}

fn left_in(clone: &Path, id: &str) -> Left {
    let mut gone = git::local_refs(clone, id);
    let mut kept = Vec::new();
    for (branch, sha) in git::branches_of(clone, id) {
        if let Some(w) = git::worktree_of(clone, &branch) {
            kept.push(format!("branch {branch} stays in {}: it is checked out in {}", clone.display(), w.display()));
        } else if git::held_elsewhere(clone, &sha, id) {
            gone.push((format!("refs/heads/{branch}"), sha));
        } else {
            kept.push(format!(
                "branch {branch} stays in {}: it holds commits no other branch, remote-tracking branch or tag has; merge them, or drop them with `git branch -D {}`",
                clone.display(),
                super::quote(&branch)
            ));
        }
    }
    Left { gone, kept }
}

/// Deletes what `left_in` finds loses nothing, each only while it holds what was read (a commit made on
/// a branch since keeps it); returns what stays. One a concurrent run (another listing, a background
/// one) deleted first is gone, not kept.
fn remove_left(clone: &Path, id: &str) -> Vec<String> {
    let Left { gone, mut kept } = left_in(clone, id);
    for (name, sha) in gone {
        if let Err(e) = git::delete_local_ref(clone, &name, &sha)
            && git::rev_parse(clone, &name).is_some()
        {
            kept.push(format!("{name} stays in {}: {e}", clone.display()));
        }
    }
    kept
}

/// How a hand-off whose leftovers a clone holds stands here, as `doctor` and `tidy` read it.
#[derive(PartialEq)]
enum Over {
    Done,
    /// Maybe an offer this machine never heard of: never tidied.
    Unknown,
}

/// `None`: the hand-off is not over.
fn over(all: &[log::HandOff], known: &std::collections::BTreeMap<String, log::Known>, id: &str) -> Option<Over> {
    match all.iter().find(|h| h.id() == id) {
        Some(h) if h.closed(known.get(id)) => Some(Over::Done),
        Some(_) => None,
        None if known.get(id).is_some_and(|k| k.gone) => Some(Over::Done),
        None => Some(Over::Unknown),
    }
}

/// At each `borg handoff`: removes from the clones recorded here what hand-offs done here left that
/// loses nothing (`left_in`), wherever `done` ran; `doctor` names the rest.
pub(super) fn tidy(home: &Path) {
    let text = log::read(home);
    let all = log::fold(&text);
    let known = log::known(home);
    for clone in super::clones::recorded(home) {
        for id in git::local_ids(&clone) {
            if over(&all, &known, &id) == Some(Over::Done) {
                remove_left(&clone, &id);
            }
        }
    }
}

/// A `done` that was interrupted: run again (every step is idempotent).
pub(super) fn recover_close(ctx: &Ctx, id: &str, op: &Op, steps: &[Step]) -> Result<String, Halt> {
    let Some(Step::Close { by }) = steps.first() else { return super::fail("no done".into()) };
    let text = log::read(&ctx.home);
    let all = log::fold(&text);
    let h = all.iter().find(|h| h.id() == id).ok_or_else(|| Halt::Failed("its offer is not in the log".into()))?;
    let me = Ctx { home: ctx.home.clone(), cwd: ctx.cwd.clone(), me: owned(by), now: ctx.now };
    close_steps(&me, h, op)
}

/// Drops from this machine's log every line of a hand-off the remote holds nothing of any more
/// (`Known::gone`: its tombstone stays in what the remote said), and its closed operation logs.
pub(super) fn compact(ctx: &Ctx, known: &std::collections::BTreeMap<String, log::Known>) {
    let open = wal::ids(&ctx.home);
    let gone: Vec<String> = known.iter().filter(|(id, k)| k.gone && !open.contains(id)).map(|(id, _)| id.clone()).collect();
    log::compact(&ctx.home, &gone);
    for id in &gone {
        wal::prune(&ctx.home, id);
    }
}

/// The hand-off refs the remote of the repo `borg handoff` runs in still holds of hand-offs that are
/// closed here or unknown here: listed with what deletes them (`done` for a closed one; by hand for
/// an unknown one, which may be an offer this machine never heard of).
pub(super) fn stray_refs(ctx: &Ctx, all: &[log::HandOff], known: &std::collections::BTreeMap<String, log::Known>) {
    let Some(root) = git::toplevel(&ctx.cwd) else { return };
    let Some(branch) = git::current_branch(&root) else { return };
    let Ok((remote, url)) = git::push_remote(&root, &branch) else { return };
    let Ok(held) = git::remote_refs(&root, &url) else { return };
    for (id, names) in held {
        match all.iter().find(|h| h.id() == id) {
            Some(h) if h.closed(known.get(&id)) || h.state(known.get(&id)) == "aborted" => {
                println!("{id}: its refs remain on {remote} ({}): `borg handoff done {id}` deletes them", names.len());
            }
            Some(_) => {}
            None => println!(
                "{id}: {remote} holds refs of a hand-off this machine does not know (it may be an offer it never heard of); once you know it is over: git push {} --delete {}",
                super::quote(&remote),
                names.iter().map(|n| super::quote(n)).collect::<Vec<_>>().join(" ")
            ),
        }
    }
}

/// What `borg doctor` reports of hand-offs in the collective of `home`: hand-offs switched off, and
/// what takes left in the clones recorded here (branches `borg/handoff/<id>`, local hand-off refs)
/// whose hand-off is done or unknown here. Of a done one: what `borg handoff` removes (`tidy`), and each
/// branch that stays, with why.
pub fn doctor(home: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(why) = switched_off(home) {
        out.push(format!("hand-off: {why}"));
    }
    let text = log::read(home);
    let all = log::fold(&text);
    let known = log::known(home);
    for clone in super::clones::recorded(home) {
        for id in git::local_ids(&clone) {
            match over(&all, &known, &id) {
                None => {}
                Some(Over::Done) => {
                    let Left { gone, kept } = left_in(&clone, &id);
                    if !gone.is_empty() {
                        out.push(format!(
                            "hand-off {id} is done, but {} still holds {} of its refs or merged branches: `borg handoff` deletes them",
                            clone.display(),
                            gone.len()
                        ));
                    }
                    out.extend(kept.into_iter().map(|k| format!("hand-off {id} is done: {k}")));
                }
                Some(Over::Unknown) => out.push(format!(
                    "hand-off {id} is unknown here, but {} still holds its branch or refs: delete them once you know it is over (`git branch -D borg/handoff/{id}`; `git update-ref -d refs/borg/handoff/{id}/<ref>`)",
                    clone.display()
                )),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
            .args(args)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("HOME", dir)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// Of what a done hand-off left: the local refs go; a branch goes only once another branch holds
    /// its commits and no work tree has it checked out; an unmerged one stays with the `git branch -D`
    /// that drops it, a checked-out one with its work tree.
    #[test]
    fn only_what_loses_nothing_goes() {
        let root = std::env::temp_dir().join(format!("borg-close-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let clone = root.join("app");
        std::fs::create_dir_all(&clone).unwrap();
        let id = "h0123456789ab";
        let base = git::branch_of(id);
        git(&clone, &["init", "-q", "-b", "main"]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "a"]);
        git(&clone, &["update-ref", &git::remote_ref(id, "state"), "HEAD"]);
        // borg/handoff/<id>: a commit nothing else has; -2: merged, but checked out in a work tree.
        git(&clone, &["switch", "-q", "-c", &base]);
        git(&clone, &["commit", "-q", "--allow-empty", "-m", "work"]);
        git(&clone, &["switch", "-q", "main"]);
        git(&clone, &["branch", &format!("{base}-2")]);
        let wt = root.join("wt");
        git(&clone, &["worktree", "add", "-q", &wt.to_string_lossy(), &format!("{base}-2")]);

        let kept = remove_left(&clone, id);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(kept[0].contains(&format!("branch {base} stays")) && kept[0].contains("`git branch -D"), "{kept:?}");
        assert!(kept[1].contains(&format!("branch {base}-2 stays")) && kept[1].contains(&format!("checked out in {}", wt.display())), "{kept:?}");
        assert!(git::local_refs(&clone, id).is_empty(), "the local refs go");

        git(&clone, &["worktree", "remove", &wt.to_string_lossy()]);
        git(&clone, &["merge", "-q", "--ff-only", &base]);
        assert_eq!(remove_left(&clone, id), Vec::<String>::new());
        assert!(git::branches_of(&clone, id).is_empty(), "merged and checked out nowhere: both go");
        assert!(remove_left(&clone, id).is_empty(), "nothing left, nothing kept");
        let _ = std::fs::remove_dir_all(&root);
    }
}
