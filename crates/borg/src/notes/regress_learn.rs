//! The end of `borg notes attach` for a session's landed commits (notes.rs `After`): link them to
//! the earlier decisions they fixed (regress.rs), record the links, and with `BORG_REGRESS=learn`
//! turn each strong link into a regret episode in the collective's store, team or local (regret.rs). One git
//! deadline covers it all (`regress_git.rs`). Once learned as a lesson, `borg learn` credits each regret
//! back to the lessons asked in the deciding session before the decision (learn.rs `credit_regrets`).

use std::fmt::Write as _;
use std::path::Path;

use crate::learn;
use crate::notes::links::Link;
use crate::notes::reasoning;
use crate::notes::regress::{self, Cfg, Git, Mode, Recorded, Scope};
use crate::notes::regret::{self, FixContext};
use crate::notes::{self, Landed};

/// What the fixing session said about it: the end of its last proposal (in the fix's note,
/// written moments earlier) and the engineer's prompts.
fn fix_words(l: &Landed, fix: &str) -> String {
    let proposal = notes::note_of(l.repo, fix).map(|e| e.proposal).unwrap_or_default();
    let prompts = regress::session_file(l, "prompts").unwrap_or_default();
    format!("{}\n{}", prompts.trim(), proposal.trim()).trim().to_string()
}

/// The fix's diff on the decision's files, its author, and the fixing session's task and words.
fn fix_context(git: &Git, l: &Landed, link: &Link, task: &str) -> FixContext {
    let hunks = git.hunks(&format!("{}^", link.fix), &link.fix).unwrap_or_default();
    let email = git.run(&["show", "-s", "--format=%ae", &link.fix]).unwrap_or_default();
    FixContext { task: task.into(), email, diff: regret::diff_summary(&hunks, &link.files), why: fix_words(l, &link.fix) }
}

/// Lists episode `id` in the borg note of `commit`, if it has one.
fn list_episode(repo: &Path, commit: &str, id: &str) -> Result<(), String> {
    let Some(mut e) = notes::note_of(repo, commit) else { return Ok(()) };
    if e.episodes.iter().any(|x| x == id) {
        return Ok(());
    }
    e.episodes.push(id.into());
    notes::write_note(repo, &e)
}

/// Each strong link not learned from yet becomes a regret episode (notes/regret.rs) in the store,
/// listed in the fix's note so `borg why` shows the lesson learned from it.
fn learn_regrets(l: &Landed, git: &Git, scope: &Scope, found: &[Link], done: &mut Recorded) {
    let task = regress::session_file(l, "task").unwrap_or_default();
    for link in found.iter().filter(|x| x.strong()) {
        let id = regret::id_of(link);
        if regret::known(&scope.store, &id) {
            continue;
        }
        let decision = reasoning::reasoning_for(l.repo, &scope.store, &scope.here, std::slice::from_ref(&link.commit)).remove(&link.commit);
        let e = regret::episode(link, decision.as_ref(), &fix_context(git, l, link, &task));
        match learn::record_episode_in(&scope.store, &e) {
            Ok(()) => done.episodes += 1,
            Err(err) => done.errors.push(format!("{}: {err}", scope.store.display())),
        }
        done.errors.extend(list_episode(l.repo, &link.fix, &id).err());
    }
}

/// A strong link is a "no" after merge: the fix changed a committed decision. One whose deciding session
/// the queen never escalated is a missed escalation (escalation.rs), once per link, whether or not the
/// link is learned from.
fn missed_escalations(scope: &Scope, found: &[Link]) {
    let now = learn::now_ms();
    for link in found.iter().filter(|x| x.strong()) {
        crate::escalation::missed(&scope.store, &link.decision.session, "regret", &regret::id_of(link), &link.repo, now);
    }
}

/// The end of `borg notes attach`'s message: what `record` and `learn_regrets` wrote.
fn summary(done: &Recorded) -> String {
    let mut s = if done.links == 0 { String::new() } else { format!("; {} link(s) to earlier decisions ({} fix)", done.links, done.strong) };
    if done.episodes > 0 {
        let _ = write!(s, ", {} regret episode(s)", done.episodes);
    }
    if let Some(e) = done.errors.first() {
        let _ = write!(s, "; link errors: {e}");
    }
    s
}

/// notes.rs `After`: links the session's landed commits to the decisions they fixed and records
/// them (a strong link without an escalation counts against the queen, `missed_escalations`); with `BORG_REGRESS=learn`, strong links also become regret episodes in the collective's
/// store (a local one's never leave the machine). The text ends `borg notes attach`'s message; silent when there is nothing.
#[must_use]
pub fn after_notes(l: &Landed) -> String {
    let cfg = Cfg::from_env();
    let Some(scope) = Scope::of(l.route, l.store) else { return String::new() };
    if cfg.mode == Mode::Off || l.commits.is_empty() {
        return String::new();
    }
    let git = cfg.git(l.repo);
    let found = regress::links_with(&git, l, &scope, &cfg);
    let mut done = regress::record(l.repo, &scope, &found.links);
    missed_escalations(&scope, &found.links);
    if cfg.mode == Mode::Learn {
        learn_regrets(l, &git, &scope, &found.links, &mut done);
    }
    summary(&done)
}
