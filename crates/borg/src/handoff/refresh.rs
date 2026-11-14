//! The owner's refresh of an open offer: at each stop of the offering session (mod.rs `at_stop`, in a
//! `borg handoff` nobody waits for) and at its own `borg handoff`, every repo of the offer is
//! snapshotted again; when a tree or HEAD moved, the new states of the repos that moved are pushed over
//! the ones the offer published last, each only while the remote still holds that one
//! (`--force-with-lease`), the arbiter last with a new brief (the same HANDOFF.md, borg's metadata of
//! now, naming exactly what each remote holds) in one atomic push, and the offer line is published
//! again. None runs once `taken` exists.
//!
//! The race with a take is benign: a claim pins its parent state, and the taker fetched every state
//! it claims before it claimed (mod.rs `claim`). A refresh that finds `taken` after its pushes, or that
//! was interrupted before every repo it pushed was read back, puts each ref it moved back to what it
//! held (`restore`). A claim that lands after a refresh published (it asked for `taken` before the claim
//! got there) is put right after the decision: by the taker in every repo it fetched, by the owner in
//! every repo once it learns the claim (mod.rs `put_claimed`). So the remote's refs end as the claim names
//! them; the original learns which of its changes came after the state taken (mod.rs `learned`). Every
//! step is logged ahead (wal.rs).

use std::path::Path;

use super::wal::{Halt, Op, Step};
use super::{BRIEF_FILE, Ctx, Refs, STATE_FILE, git, log, wal};

/// Refreshes the open offer `h` of the session `ctx` runs as, when one of its repos moved; says
/// nothing (it runs in the background) unless the remote put it back after a take.
pub(super) fn run(ctx: &Ctx, h: &log::HandOff) {
    let id = h.id();
    let Ok(op) = Op::begin(&ctx.home, id) else { return };
    let text = wal::read(&ctx.home, id);
    let Some(o) = super::offered(&text, None) else { return };
    let Step::Offer { repos: plans, by, .. } = &o.intent else { return };
    if by.session != ctx.me.session || by.node != ctx.me.node {
        return;
    }
    let refs = Refs::of(id);
    let arb = &plans[0];
    if !matches!(git::ls_remote(Path::new(arb.root.as_ref()), &arb.url, &refs.taken), Ok(None)) {
        return;
    }
    // Each repo as it is now; one mid-rebase or detached waits for a later stop. A repo whose tree and
    // HEAD did not move keeps what the offer published: a snapshot of it again would be a new commit no
    // remote holds, and the brief and state.json name only what the remotes hold.
    let mut now: Vec<Now> = Vec::new();
    for (i, (p, l)) in plans.iter().zip(&o.repos).enumerate() {
        let root = Path::new(p.root.as_ref());
        let index = wal::dir(&ctx.home).join(format!("{id}.{i}.refresh.index"));
        let fresh = git::base(root).ok().and_then(|b| {
            let s = git::snapshot(root, &b.head, &index, &format!("borg hand-off {id}: the working tree of {}\n", b.branch)).ok()?;
            (s.tree != l.tree || b.branch != l.branch || b.head != l.head).then_some(Now { snap: s, branch: b.branch, head: b.head, moved: true })
        });
        now.push(fresh.unwrap_or_else(|| Now { snap: as_snapshot(l), branch: l.branch.to_string(), head: l.head.to_string(), moved: false }));
    }
    if !now.iter().any(|n| n.moved) {
        return;
    }
    match steps(ctx, id, &op, &o, &now) {
        Ok(()) => {}
        Err(Halt::Failed(why)) => {
            if let Some(held) = why.strip_prefix(TAKEN) {
                println!("borg handoff: {id} was taken while it was refreshed: the remote holds the state the claim names again");
                if let Some((root, url)) = super::arbiter_clone(ctx, h) {
                    super::learned(ctx, h, &root, &url, log::Heard { taken: Some(held), ..log::Heard::default() });
                    super::fence::refresh(&ctx.home);
                }
            }
        }
        Err(_) => {}
    }
}

/// The prefix of the failure of a refresh that found `taken` after its pushes; the claim follows.
const TAKEN: &str = "taken while refreshing: ";

/// One repo of a refresh: its snapshot, the branch and HEAD it stands on, and whether it moved since
/// the offer published it last (else `snap` is what was published, `as_snapshot`).
struct Now {
    snap: git::Snapshot,
    branch: String,
    head: String,
    moved: bool,
}

/// A repo as the offer last published it, as a snapshot (nothing taken anew).
fn as_snapshot(l: &super::Latest) -> git::Snapshot {
    git::Snapshot {
        state: l.state.to_string(),
        tree: l.tree.to_string(),
        left_out: l.left_out.iter().map(|s| s.to_string()).collect(),
        ..git::Snapshot::default()
    }
}

/// The logged steps of one refresh: intent, the snapshots that moved, the new brief, then per repo
/// that moved (the arbiter last, with the brief) the leased push and its read-back, then `taken` asked
/// again; published when it is still absent, else everything put back.
fn steps(ctx: &Ctx, id: &str, op: &Op, o: &super::Offered, now: &[Now]) -> Result<(), Halt> {
    let Step::Offer { repos: plans, .. } = &o.intent else { return Err(Halt::Failed("no offer".into())) };
    let refs = Refs::of(id);
    op.log(Step::Refresh { ms: ctx.now })?;
    let moved: Vec<usize> = (0..plans.len()).filter(|&i| now[i].moved).collect();
    for &i in &moved {
        op.log(super::snapshot_step(i, &now[i].snap, Some((&now[i].branch, &now[i].head))))?;
    }
    let arb = Path::new(plans[0].root.as_ref());
    let brief_text = git::file_at(arb, &o.brief, BRIEF_FILE).ok_or_else(|| Halt::Failed("the brief cannot be read".into()))?;
    let snaps: Vec<(&git::Snapshot, Option<(&str, &str)>)> = now.iter().map(|n| (&n.snap, Some((n.branch.as_str(), n.head.as_str())))).collect();
    let meta = super::metadata(ctx, id, plans, &snaps);
    let files: [(&str, &[u8]); 2] = [(BRIEF_FILE, &brief_text), (STATE_FILE, &meta)];
    // On the arbiter's state as the remote will hold it: its new one when it moved, else the published one.
    let brief = git::brief_commit(arb, &now[0].snap.state, &files, &format!("borg hand-off {id}: the brief\n")).map_err(Halt::Failed)?;
    op.log(Step::Brief { sha: brief.as_str().into() })?;
    // The arbiter last: until its state and brief move, a take sees the offer as it was.
    let mut order: Vec<usize> = moved.iter().copied().filter(|&i| i != 0).collect();
    order.push(0);
    for i in order {
        let (p, l) = (&plans[i], &o.repos[i]);
        let root = Path::new(p.root.as_ref());
        op.log(Step::Pushing { repo: i })?;
        let mut moves = Vec::new();
        if moved.contains(&i) {
            moves.push((now[i].snap.state.as_str(), refs.state.as_str(), l.state.as_ref()));
        }
        if i == 0 {
            moves.push((brief.as_str(), refs.brief.as_str(), o.brief.as_ref()));
        }
        let pushed = git::leased(root, &p.url, &moves);
        if let Err(e) = pushed {
            restore(ctx, id, op)?;
            return super::end(op, format!("the refresh of {} was refused ({e}): put back", p.name));
        }
        op.log(Step::Pushed { repo: i })?;
        for (want, r, _) in &moves {
            if git::ls_remote(root, &p.url, r).ok().flatten().as_deref() != Some(*want) {
                restore(ctx, id, op)?;
                return super::end(op, format!("{} does not read back the refresh: put back", p.name));
            }
        }
        op.log(Step::Verified { repo: i })?;
    }
    match git::ls_remote(arb, &plans[0].url, &refs.taken) {
        Ok(None) => super::publish(ctx, id, op),
        Ok(Some(held)) => {
            restore(ctx, id, op)?;
            super::end(op, format!("{TAKEN}{held}"))
        }
        Err(e) => {
            restore(ctx, id, op)?;
            super::end(op, format!("the remote could not say whether it was taken ({e}): put back"))
        }
    }
}

/// Puts back every ref the refresh `op` moved, in each repo it began pushing to and did not put back
/// yet, each only while the remote holds the refresh's value: to the states (and brief) the claim
/// names when `taken` holds one (a taker may have fetched this very refresh before it claimed), else
/// to what the offer published before it. Unreachable: an error, and the operation stays open for the
/// next run.
fn restore(ctx: &Ctx, id: &str, op: &Op) -> Result<(), Halt> {
    let text = wal::read(&ctx.home, id);
    let prev = super::offered(&text, None).ok_or_else(|| Halt::Failed("the offer's log is incomplete".into()))?;
    let Step::Offer { repos: plans, .. } = &prev.intent else { return Err(Halt::Failed("no offer".into())) };
    let mine = super::steps_of(&text, op.id());
    let refs = Refs::of(id);
    let brief = mine.iter().find_map(|s| if let Step::Brief { sha } = s { Some(sha.to_string()) } else { None });
    let (arb, url) = (Path::new(plans[0].root.as_ref()), plans[0].url.as_ref());
    let held =
        git::ls_remote(arb, url, &refs.taken).map_err(|e| Halt::Failed(format!("cannot ask {} yet ({e}): put back once it answers", plans[0].remote)))?;
    // What each ref goes back to: the claim's (its states, its parent for an older claim; its brief).
    let claimed = held.as_deref().and_then(|v| {
        let c = super::claimer(arb, url, id, v)?;
        let states = if c.states.is_empty() { git::rev_parse(arb, &format!("{v}^")).into_iter().collect() } else { c.states };
        Some((states, c.brief))
    });
    let back_state = |i: usize| claimed.as_ref().and_then(|(s, _)| s.get(i).cloned()).unwrap_or_else(|| prev.repos[i].state.to_string());
    let back_brief = claimed.as_ref().map_or_else(|| prev.brief.to_string(), |(_, b)| b.clone());
    for (i, p) in plans.iter().enumerate() {
        let began = mine.iter().any(|s| matches!(s, Step::Pushing { repo } if *repo == i));
        if !began || mine.iter().any(|s| matches!(s, Step::Restored { repo } if *repo == i)) {
            continue;
        }
        let root = Path::new(p.root.as_ref());
        let held = git::ls_remote_all(root, &p.url, id).map_err(|e| Halt::Failed(format!("cannot ask {} yet ({e}): put back once it answers", p.remote)))?;
        let at = |leaf: &str| held.iter().find(|(l, _)| l == leaf).map(|(_, v)| v.clone());
        let new = mine.iter().find_map(|s| match s {
            Step::Snapshot { repo, state, .. } if *repo == i => Some(state.to_string()),
            _ => None,
        });
        let mut moves: Vec<(String, String, String)> = Vec::new();
        if let (Some(new), Some(cur)) = (new, at("state"))
            && new == cur
            && back_state(i) != cur
        {
            moves.push((back_state(i), refs.state.clone(), new));
        }
        if i == 0
            && let (Some(new), Some(cur)) = (&brief, at("brief"))
            && *new == cur
            && back_brief != cur
        {
            moves.push((back_brief.clone(), refs.brief.clone(), cur));
        }
        if !moves.is_empty() {
            let m: Vec<(&str, &str, &str)> = moves.iter().map(|(s, d, e)| (s.as_str(), d.as_str(), e.as_str())).collect();
            git::leased(root, &p.url, &m).map_err(|e| Halt::Failed(format!("cannot put back the refresh of {} ({e})", p.name)))?;
        }
        op.log(Step::Restored { repo: i })?;
    }
    Ok(())
}

/// A refresh that was interrupted: published when every repo it pushed was read back and `taken` is
/// still absent, else put back (`restore`) and ended.
pub(super) fn recover(ctx: &Ctx, id: &str, op: &Op) -> Result<String, Halt> {
    let text = wal::read(&ctx.home, id);
    let prev = super::offered(&text, None).ok_or_else(|| Halt::Failed("the offer's log is incomplete".into()))?;
    let Step::Offer { repos: plans, .. } = &prev.intent else { return Err(Halt::Failed("no offer".into())) };
    let mine = super::steps_of(&text, op.id());
    if mine.iter().any(|s| matches!(s, Step::Logged)) {
        op.log(Step::Done)?;
        return Ok("finished a published refresh".into());
    }
    let began: Vec<usize> = mine.iter().filter_map(|s| if let Step::Pushing { repo } = s { Some(*repo) } else { None }).collect();
    let read_back = began.iter().all(|i| mine.iter().any(|s| matches!(s, Step::Verified { repo } if repo == i)));
    let restored = mine.iter().any(|s| matches!(s, Step::Restored { .. }));
    let arb = &plans[0];
    let held = git::ls_remote(Path::new(arb.root.as_ref()), &arb.url, &git::remote_ref(id, "taken"))
        .map_err(|e| Halt::Failed(format!("cannot ask {} yet ({e}): finished once it answers", arb.remote)))?;
    if held.is_none() && read_back && began.contains(&0) && !restored {
        super::publish(ctx, id, op)?;
        return Ok("published the refresh (every repo it pushed reads back)".into());
    }
    restore(ctx, id, op)?;
    op.log(Step::Ended { why: "interrupted refresh: put back".into() })?;
    Ok("put back an interrupted refresh: the remote holds the offer as it was published".into())
}
