//! Which of a graded check's applies to ask, to hand over free, to judge blindly as a placebo, or to
//! leave for later: the lesson selection policy. It replaces attention.rs, which stays one release as
//! the `Attention` sub-policy.
//!
//! **Policies** (`policy`, the first that applies): `BORG_SELECT=fixed` the fixed rule;
//! `BORG_STOP_ASK_CAP=N` the fixed rule asking only the first `N` 3s (evaluation only);
//! `BORG_STOP_ATTENTION=on` attention.rs's learned cut (evaluation only); `BORG_SELECT=learned` the
//! learned policy (`Learned`, select/policy.rs); otherwise the fixed rule, the default until the
//! pre-registered checks P0–P4 pass (tasks/todo.md, amendment 2026-10-29: P4 failed its first run).
//!
//! **The fixed rule** (conscience.rs `graded_without`, no cap) asks every 3 and 2 and the 1s up to
//! `conscience::CAP` when a check has a 3, else hands over the 2s and 1s free, up to `CAP`; a lesson the
//! session already had delivered (`Ctx::had`) may still be asked but never takes a free slot, which its
//! delivery would drop as given before. It also draws up to `PLACEBO_SLOTS` of the applies it delivers
//! neither way for the placebo arm (`draw_placebo`): judged blindly where a free delivery would be, never
//! shown, so what not delivering is worth gets labels at no cost to quality. Those are the labels the
//! learned policy cannot get from the fixed rule's history, which asks and hands over by grade alone.
//!
//! **The learned policy** (select/policy.rs `choose`) routes by a fitted value model under a
//! conservative gate; cold start (no fit, or a gate that may risk nothing yet) is exactly the fixed
//! rule, and so is a store the monitor reverted (select/summary.rs `REVERTED`). It draws its placebos
//! among the applies it leaves for later, as the fixed rule does. The evaluation switches decide
//! exactly as the hook did before this module (0.5.0's attention.rs `graded`) and draw no placebo.
//!
//! **What it reads.** Only `Ctx`: the summary the hook loaded once (select/summary.rs `load`, beside
//! the lessons file), and for the `Attention` sub-policy the outcomes beside the lessons file
//! (outcome.rs `path_beside`), never the routed collective's: an evaluation that points
//! `BORG_CONSCIENCE` at its own store reads that store's outcomes, not the engineer's.
//!
//! Deterministic: a decision depends on the applies, the policy, the summary and the check id
//! (`check_id`, `seed`), all logged.

pub mod policy;
pub mod replay;
pub mod summary;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::attention;
use crate::conscience::{self, Challenge};
use crate::outcome;

/// The policy in force for a graded check (module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The default, and `BORG_SELECT=fixed` whatever else is set: the fixed rule (module doc).
    Fixed,
    /// `BORG_STOP_ASK_CAP=N`: a block asks only the first `N` 3s (conscience.rs `ask_cap`).
    Cap(usize),
    /// `BORG_STOP_ATTENTION=on`: attention.rs's conformal cut on the model's rank.
    Attention,
    /// `BORG_SELECT=learned`: the learned policy (select/policy.rs), opt-in until P0–P4 pass.
    Learned,
}

impl Policy {
    /// The name logged with each check (`Log::policy`, outcome.rs `policy`).
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Policy::Fixed => "fixed",
            Policy::Cap(_) => "cap",
            Policy::Attention => "attention",
            Policy::Learned => "learned",
        }
    }
}

/// The policy this process runs under, from the environment (module doc).
#[must_use]
pub fn policy() -> Policy {
    precedence(std::env::var("BORG_SELECT").ok().as_deref(), conscience::ask_cap(), attention::on())
}

/// `policy` over its three switches: `select` (`BORG_SELECT`), `cap` (`BORG_STOP_ASK_CAP` as
/// conscience.rs `ask_cap` reads it) and `attention` (`BORG_STOP_ATTENTION=on`). `fixed` wins over
/// both evaluation switches, the cap over attention, both over `learned`; any other `BORG_SELECT`,
/// or none, is the default, the fixed rule. Whatever evaluates the learned policy (select/replay.rs,
/// the monitoring of what it would do) names `Policy::Learned` and never reads this.
#[must_use]
pub fn precedence(select: Option<&str>, cap: Option<usize>, attention: bool) -> Policy {
    match (select, cap) {
        (Some("fixed"), _) => Policy::Fixed,
        (_, Some(n)) => Policy::Cap(n),
        _ if attention => Policy::Attention,
        (Some("learned"), _) => Policy::Learned,
        _ => Policy::Fixed,
    }
}

/// What happens to one graded apply at its check.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    /// Asked: in a block at the stop, as a question with the next prompt or tool call live.
    Ask,
    /// Handed over free, as context at the next prompt or touch (hook.rs `queued`).
    Free,
    /// Not delivered, but judged blindly where a free delivery would be: the judge's floor.
    Placebo,
    /// Not now: stays eligible for a later check.
    #[default]
    Not,
}

impl Route {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Route::Ask => "ask",
            Route::Free => "free",
            Route::Placebo => "placebo",
            Route::Not => "not",
        }
    }

    /// The arm the route delivers by, an index below `ARMS`: ask 0, free 1, not now 2 (a placebo is
    /// not delivered).
    #[must_use]
    pub const fn arm(self) -> usize {
        match self {
            Route::Ask => 0,
            Route::Free => 1,
            Route::Placebo | Route::Not => 2,
        }
    }
}

/// The arms a graded apply can be delivered by (`Route::arm`). Protocol: the length and order of
/// every per-arm array (select/policy.rs `monitor`, select/replay.rs).
pub const ARMS: usize = 3;

/// One graded apply as the policy sees it, aligned with the check's applies.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit<'a> {
    pub lesson: &'a str,
    /// 3 certain, 2 likely, 1 worth a thought.
    pub grade: u8,
    /// Its place in the model's applies, 0 first.
    pub rank: usize,
}

/// The units of `applied`, in its order.
#[must_use]
pub fn units<'a>(applied: &[(&'a Challenge, u8)]) -> Vec<Unit<'a>> {
    applied.iter().enumerate().map(|(rank, (c, g))| Unit { lesson: c.id.as_str(), grade: *g, rank }).collect()
}

/// The check a decision is made for.
pub struct Ctx<'a> {
    pub session: &'a str,
    /// The check's id (`check_id`).
    pub check: &'a str,
    /// `stop` or `live`.
    pub channel: &'static str,
    /// The lessons file the conscience reads (hook.rs `lessons_path`): the store whose outcomes count.
    pub lessons: &'a Path,
    /// The selection summary loaded beside `lessons` (select/summary.rs `load`).
    pub summary: &'a summary::Summary,
    /// The check's pool tokens (`shown_tokens`, the graded record's `tokens`); 0 unknown, the
    /// baseline's. The fit and the gate's ledger value the check at the logged tokens, so the learned
    /// policy decides at them too: the gain the gate admits is then the gain its ledger records.
    pub tokens: u32,
    /// The lessons this session already had delivered (asked, delivered early or handed over free:
    /// hook.rs `had_in` over the state read at the decision, `decide`) or has waiting in its queue to be
    /// asked or handed over by another check (hook.rs `waiting_in`), in any order. Such a lesson may
    /// still be asked, but never takes a free slot (the delivery would drop it as given before or queued
    /// twice, conscience.rs `graded_without`), and is never a placebo (`draw_placebo`). Only those among the
    /// applies count (`nofree`, what the graded record logs), so a replay passes the record's `nofree`;
    /// empty for a record from before it, whose hook decided without.
    pub had: &'a [String],
}

/// The pool tokens of a check whose model call was shown `shown` (hook.rs `Call::order`): whole.rs
/// `cost` summed over them, saturating. What the graded record logs (outcome.rs `tokens`) and the
/// decision values (`Ctx::tokens`), from one definition so the two never part.
#[must_use]
pub fn shown_tokens(shown: &[&Challenge]) -> u32 {
    let sum: usize = shown.iter().map(|c| crate::whole::cost(c)).sum();
    sum.try_into().unwrap_or(u32::MAX)
}

/// The applies the session already had (`Ctx::had`), each once, in the applies' order: what the graded
/// record logs as `nofree` (outcome.rs), all a replay needs of `had` to decide as the hook did.
#[must_use]
pub fn nofree(applied: &[(&Challenge, u8)], had: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (c, _) in applied {
        if had.contains(&c.id) && !out.contains(&c.id) {
            out.push(c.id.clone());
        }
    }
    out
}

/// What a decision logs: the graded record's `policy` and `fit`, and the `#borg-select` line.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Log {
    /// `Policy::name`.
    pub policy: &'static str,
    /// What set this check's routes: `fixed` (the fixed rule: under `Fixed`, `Attention` on a check
    /// without a 3, and `Learned` at cold start), `cap`, attention's cut source (`prior`, `crc`,
    /// `explore`; an `explore` check's outcomes label the cut, outcome.rs `explore`), or the learned
    /// policy's (select/policy.rs `Decision::source`: `learned`, `gate` when the gate kept the fixed
    /// rule, `size` past `policy::SOLVE_MAX`; `reverted` when the monitor reverted the store).
    pub source: &'static str,
    /// The fit the decision used (select/policy.rs `Fit::key`); empty for the prior.
    pub fit: String,
    /// The conservative gate's budget at this check: how much replayed quality (mistakes avoided) the
    /// learned policy may still risk below the fixed rule. Not money: spend is only measured, unless an
    /// engineer sets a budget for the collective. 0 at cold start and under every other policy.
    pub gate: f64,
    /// How much of `gate` this decision takes (select/policy.rs `Decision::risk`): 0 when it keeps the
    /// fixed rule. Replayed, not logged: `borg debug select` P3 checks it never exceeds `gate`.
    pub risk: f64,
    /// `Attention` on a check with a 3: today's `#borg-attention` line (attention.rs `line`), which the
    /// hook keeps writing while attention.rs is a sub-policy.
    pub attention: Option<String>,
    /// Each placebo candidate's chance of the draw (`draw_placebo`, outcome.rs `psi`); 0 without a candidate
    /// and under the evaluation switches, which draw none.
    pub psi: f64,
}

impl Log {
    /// `#borg-select policy=<p> source=<s> fit=<key|-> gate=<B> applied=<m> @<check>`: one check's
    /// decision, beside its `#borg-graded` line; `@<check>` ties it to the check's other lines.
    #[must_use]
    pub fn line(&self, applied: usize, check: &str) -> String {
        let fit = if self.fit.is_empty() { "-" } else { self.fit.as_str() };
        format!("#borg-select policy={} source={} fit={fit} gate={:.4} applied={applied} @{check}", self.policy, self.source, self.gate)
    }
}

/// A graded check's decision: what to ask (in the order to ask it), to hand over free (in the order
/// to deliver it), and to judge as placebo; per apply (aligned with the applies) its probability of
/// being asked (`p`) and of being handed over free (`pf`) under the policy in force; and what to log.
#[derive(Debug, Clone)]
pub struct Routed<'a> {
    pub ask: Vec<&'a Challenge>,
    pub free: Vec<&'a Challenge>,
    pub placebo: Vec<&'a Challenge>,
    pub p: Vec<f64>,
    pub pf: Vec<f64>,
    pub log: Log,
}

impl Routed<'_> {
    /// The route of lesson `id`: `Not` for a lesson the check neither asks, hands over nor judges.
    #[must_use]
    pub fn route(&self, id: &str) -> Route {
        let has = |v: &[&Challenge]| v.iter().any(|c| c.id == id);
        if has(&self.ask) {
            Route::Ask
        } else if has(&self.free) {
            Route::Free
        } else if has(&self.placebo) {
            Route::Placebo
        } else {
            Route::Not
        }
    }
}

/// A graded check's decision under `policy` (the hook passes the policy in force, `policy`, through
/// hook.rs `decide`): replaces 0.5.0's attention.rs `graded`. `Fixed` and `Cap` decide by the fixed
/// rule (conscience.rs `graded_without` with `ctx.had`, `Cap` with its cap), whose propensities are
/// its choice (attention.rs `fixed_p`); `Attention` on a check with a 3 by the learned cut on the
/// outcomes beside `ctx.lessons`, with the explore mixture's propensities, and by the fixed rule
/// otherwise; `Learned` on every check by select/policy.rs `choose` (the fixed rule at cold start or
/// while the store is reverted), with the shares of its draws as the propensities (`learned`). `Fixed`
/// and `Learned` draw placebos among the applies they deliver neither way (`draw_placebo`); the asked
/// and free lists never depend on that draw, so the fixed rule's decision is the same with or without
/// it (P0).
#[must_use]
pub fn graded_under<'a>(policy: Policy, applied: &[(&'a Challenge, u8)], ctx: &Ctx) -> Routed<'a> {
    let three = applied.iter().any(|(_, g)| *g == 3);
    let fixed = |source: &'static str, placebos: bool, (ask, free): (Vec<&'a Challenge>, Vec<&'a Challenge>)| {
        let (placebo, psi) = if placebos { draw_placebo(applied, &ask, &free, ctx) } else { (Vec::new(), 0.0) };
        Routed {
            p: attention::fixed_p(applied, &ask),
            pf: attention::fixed_p(applied, &free),
            ask,
            free,
            placebo,
            log: Log { policy: policy.name(), source, fit: ctx.summary.fit.key.clone(), psi, ..Log::default() },
        }
    };
    match policy {
        Policy::Attention if three => by_cut(applied, ctx),
        Policy::Cap(n) => fixed("cap", false, conscience::graded_without(applied, Some(n), ctx.had)),
        Policy::Learned => learned(applied, ctx),
        Policy::Fixed => fixed("fixed", true, conscience::graded_without(applied, None, ctx.had)),
        Policy::Attention => fixed("fixed", false, conscience::graded_without(applied, None, ctx.had)),
    }
}

/// Placebo slots per check: of the applies a check delivers neither way, at most this many are judged
/// blindly (`Routed::placebo`). Guard: bounds the judge's extra work per check.
pub const PLACEBO_SLOTS: usize = 2;

/// The placebo draw of a check that asks `ask` and hands over `free` (`Fixed`, `Learned`): its
/// candidates are the applies whose lesson it delivers neither way, in no copy, and the session never
/// had (`Ctx::had`: a placebo measures not delivering, and the delivery drops a placebo of a lesson
/// given before, hook.rs `queued`), each lesson once; up to `PLACEBO_SLOTS` of them drawn uniformly on
/// the placebo's own stream of the check's seed, in the applies' order. With each candidate's chance
/// of the draw, min(1, `PLACEBO_SLOTS` / their number), 0 without one.
fn draw_placebo<'a>(applied: &[(&'a Challenge, u8)], ask: &[&Challenge], free: &[&Challenge], ctx: &Ctx) -> (Vec<&'a Challenge>, f64) {
    let delivered = |id: &str| ask.iter().chain(free).any(|c| c.id == id);
    let mut later: Vec<usize> = Vec::new();
    for (j, (c, _)) in applied.iter().enumerate() {
        if !delivered(&c.id) && !ctx.had.contains(&c.id) && !later.iter().any(|&i| applied[i].0.id == c.id) {
            later.push(j);
        }
    }
    let mut drawn = later.clone();
    // The placebo's own stream of the check's seed (the tag spells "placebo").
    let mut rng = crate::memory::Rng::new(seed(ctx) ^ 0x706c_6163_6562_6f00);
    for i in (1..drawn.len()).rev() {
        drawn.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
    }
    drawn.truncate(PLACEBO_SLOTS);
    drawn.sort_unstable();
    let psi = if later.is_empty() { 0.0 } else { outcome::round_p((PLACEBO_SLOTS as f64 / later.len() as f64).min(1.0)) };
    (drawn.iter().map(|&j| applied[j].0).collect(), psi)
}

/// `Log::source` of a learned check on a store the monitor reverted (select/summary.rs `REVERTED`).
/// Protocol: logged as the graded record's `source`, which `borg debug select` P1 reads back as the
/// revert the hook saw (select/replay.rs).
pub const REVERTED_SOURCE: &str = "reverted";

/// `Learned`: select/policy.rs `choose` on the summary's fit and gate at the check's pool tokens, never
/// handing over free a lesson the session had (`Ctx::had`; the fixed rule while the store is reverted),
/// then the placebo draw among the applies it delivers neither way (`draw_placebo`).
fn learned<'a>(applied: &[(&'a Challenge, u8)], ctx: &Ctx) -> Routed<'a> {
    let us = units(applied);
    let s = ctx.summary;
    let check = policy::Check { session: ctx.session, stop: ctx.channel == "stop", tokens: ctx.tokens };
    let d = match s.monitor.reverted {
        true => {
            let base = policy::fixed_routes(&us, ctx.had);
            let p = |r: Route| base.iter().map(|x| f64::from(u8::from(*x == r))).collect();
            policy::Decision { p: p(Route::Ask), pf: p(Route::Free), routes: base, source: REVERTED_SOURCE, gate: 0.0, risk: 0.0 }
        }
        false => policy::choose(&us, &check, &s.fit, &s.gate, seed(ctx), ctx.had),
    };
    let (ask, free) = ordered(applied, &d.routes);
    let (placebo, psi) = draw_placebo(applied, &ask, &free, ctx);
    Routed {
        ask,
        free,
        placebo,
        p: d.p,
        pf: d.pf,
        log: Log { policy: Policy::Learned.name(), source: d.source, fit: s.fit.key.clone(), gate: d.gate, risk: d.risk, attention: None, psi },
    }
}

/// `Attention` on a check with a 3: attention.rs's cut from the labels beside `ctx.lessons`, every
/// `attention::EXPLORE`th graded check there exploring up to `attention::RAIL`. `p` and `pf` are the
/// explore mixture over the cut before the explore override (attention.rs `propensities`, `free_p`).
/// 0.5.0's sub-policy as it was: it knows nothing of `Ctx::had`.
fn by_cut<'a>(applied: &[(&'a Challenge, u8)], ctx: &Ctx) -> Routed<'a> {
    let outcomes = outcome::read(&outcome::path_beside(ctx.lessons));
    let base = attention::cut(&attention::labels(&outcomes));
    let (p, pf) = (attention::propensities(applied.len(), base.k), free_p(applied.len(), base.k));
    // The cadence counts the checks it counted before 0.6.0 logged every model call: graded ones with
    // applies whose call answered.
    let checks = outcomes.iter().filter(|o| o.kind == outcome::GRADED && !o.lessons.is_empty() && !o.failed).count();
    let cut = match attention::explores(checks) {
        true => attention::Cut { k: attention::RAIL, source: "explore", ..base },
        false => base,
    };
    let (ask, free) = attention::split(applied, &cut);
    let log = Log {
        policy: Policy::Attention.name(),
        source: cut.source,
        fit: ctx.summary.fit.key.clone(),
        attention: Some(attention::line(&cut, applied.len())),
        ..Log::default()
    };
    Routed { ask, free, placebo: Vec::new(), p, pf, log }
}

/// The learned cut's chance that rank r of `n` applies goes free under the base cut `k`: with
/// q = 1/`attention::EXPLORE`, c = min(k, RAIL, n) and e = min(RAIL, n), it is (1 − q)·[c ≤ r < c +
/// CAP] + q·[e ≤ r < e + CAP] (attention.rs `split` hands over at most `conscience::CAP`).
fn free_p(n: usize, k: usize) -> Vec<f64> {
    let q = 1.0 / attention::EXPLORE as f64;
    let (cut, rail) = (k.min(attention::RAIL).min(n), attention::RAIL.min(n));
    let within = |r: usize, from: usize| f64::from(u8::from(r >= from && r < from + conscience::CAP));
    (0..n).map(|r| outcome::round_p((1.0 - q) * within(r, cut) + q * within(r, rail))).collect()
}

/// `routes` (aligned with `applied`) as the ask and free lists, each higher grade first, then in the
/// model's order: the order the fixed rule asks and hands over in (conscience.rs `graded_capped`).
fn ordered<'a>(applied: &[(&'a Challenge, u8)], routes: &[Route]) -> (Vec<&'a Challenge>, Vec<&'a Challenge>) {
    let mut order: Vec<usize> = (0..applied.len().min(routes.len())).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(applied[i].1));
    let of = |r: Route| order.iter().filter(|&&i| routes[i] == r).map(|&i| applied[i].0).collect();
    (of(Route::Ask), of(Route::Free))
}

/// The seed of a check's draws: the first 8 bytes of blake3 over its session, channel and id, so a
/// replay of the logged check draws the same, and a stop and a live check of one session never share
/// draws.
#[must_use]
pub fn seed(ctx: &Ctx) -> u64 {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-select-seed-v1\0");
    for part in [ctx.session, ctx.channel, ctx.check] {
        h.update(part.as_bytes());
        h.update(b"\0");
    }
    let bytes = h.finalize();
    // Derived: the bytes of the u64 seed.
    const WIDTH: usize = std::mem::size_of::<u64>();
    let mut first = [0u8; WIDTH];
    first.copy_from_slice(&bytes.as_bytes()[..WIDTH]);
    u64::from_le_bytes(first)
}

/// The pool budget in tokens for a check of `session` under the policy in force (`policy`,
/// `pool_tokens_under`).
#[must_use]
pub fn pool_tokens(s: &summary::Summary, limit_ms: Option<u64>, session: &str) -> usize {
    pool_tokens_under(policy(), s, limit_ms, session)
}

/// `pool_tokens` under `policy`, never the environment's: whole.rs `TOKENS` unless the pool is the
/// learned policy's (`pool_active`); then the budget its fit chooses (select/policy.rs `pool_tokens`),
/// never past the baseline without the harness's stop budget for the model call (`limit_ms`) and the
/// latency evidence to fit in it. Fixed for a session between fits, so its prompt prefix stays stable.
/// What a replay calls for the policy it replays.
#[must_use]
pub fn pool_tokens_under(policy: Policy, s: &summary::Summary, limit_ms: Option<u64>, session: &str) -> usize {
    policy::pool_tokens(&s.fit, pool_active(policy, s), limit_ms, policy::session_seed(session))
}

/// Whether `policy` chooses the pool by its fit (`pool_tokens_under`): the learned policy on a store
/// the monitor did not revert, once its gate may risk something or an engineer set a budget for the
/// collective. A budget binds from the first fit that knows it, gate open or not: its price replays
/// the pools as in force (select/policy.rs `price`), so a pool kept at the baseline while the gate is
/// closed would spend past it. The fixed rule and a reverted store keep the baseline.
fn pool_active(policy: Policy, s: &summary::Summary) -> bool {
    policy == Policy::Learned && !s.monitor.reverted && (s.gate.allowance > 0.0 || s.fit.budget.is_some())
}

/// The engineer's budget for the collective whose store holds `lessons`: its `budget` in
/// collectives.json (collective.rs `Entry::budget`: pool tokens per session, a positive number), None
/// when unset (the default) or the config does not load.
#[must_use]
pub fn budget_of(lessons: &Path) -> Option<f64> {
    budget_in(&crate::collective::load().ok().flatten()?, lessons)
}

/// `budget_of` in the config `cfg`.
fn budget_in(cfg: &crate::collective::Config, lessons: &Path) -> Option<f64> {
    let store = lessons.parent()?;
    let base = crate::collective::root_of(store);
    cfg.collectives.iter().find(|c| crate::collective::store(&base, &c.name) == store)?.budget()
}

/// Hex digits of a check id. Protocol: the length of the `@<check>` tags hook.rs already writes
/// (`append_p_of`), 48 bits, so two checks of one store collide with negligible chance.
pub const CHECK_HEX: usize = 12;

/// A graded check's id: `CHECK_HEX` hex digits of blake3 over its session, channel (`stop`, `live`),
/// start (epoch ms) and process id. Not a count of the session's checks: a stop and a live check of
/// one session can run at once and would count the same lines. Computed once per check and logged,
/// never recomputed.
#[must_use]
pub fn check_id(session: &str, channel: &str, start_ms: i64, pid: u32) -> String {
    let mut h = blake3::Hasher::new();
    h.update(b"borg-check-v1\0");
    h.update(format!("{session}\0{channel}\0{start_ms}\0{pid}").as_bytes());
    h.finalize().to_hex()[..CHECK_HEX].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lessons(n: usize) -> Vec<Challenge> {
        (0..n).map(|i| Challenge { id: format!("L{i:02}"), ..Default::default() }).collect()
    }

    fn ids(v: &[&Challenge]) -> Vec<String> {
        v.iter().map(|c| c.id.clone()).collect()
    }

    /// A temp store: its lessons file (never written) and the outcomes beside it.
    fn store(name: &str, outcomes: &[outcome::Outcome]) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("borg-select-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let text: String = outcomes.iter().map(|o| format!("{}\n", serde_json::to_string(o).unwrap())).collect();
        std::fs::write(d.join("outcomes.jsonl"), text).unwrap();
        d.join("lessons.jsonl")
    }

    fn ctx<'a>(lessons: &'a Path, s: &'a summary::Summary) -> Ctx<'a> {
        Ctx { session: "s", check: "0123456789ab", channel: "stop", lessons, summary: s, tokens: 0, had: &[] }
    }

    /// Every grade sequence of up to `n` applies over {1, 2, 3}.
    fn tables(n: usize) -> Vec<Vec<u8>> {
        let mut out = vec![Vec::new()];
        for len in 1..=n {
            out.extend((0..3usize.pow(len as u32)).map(|mut x| {
                (0..len)
                    .map(|_| {
                        let g = (x % 3) as u8 + 1;
                        x /= 3;
                        g
                    })
                    .collect()
            }));
        }
        out
    }

    #[test]
    fn the_switches_take_precedence_in_order() {
        assert_eq!(precedence(None, None, false), Policy::Fixed, "the default is the fixed rule");
        assert_eq!(precedence(Some("learned"), None, false), Policy::Learned, "the learned policy is opt-in");
        assert_eq!(precedence(Some("fixed"), Some(2), true), Policy::Fixed, "fixed wins over both evaluation switches");
        assert_eq!(precedence(Some("learned"), Some(2), true), Policy::Cap(2), "the cap wins over attention and learned");
        assert_eq!(precedence(Some("learned"), None, true), Policy::Attention, "attention wins over learned");
        assert_eq!(precedence(Some("nonsense"), None, false), Policy::Fixed, "an unknown BORG_SELECT is the default, the fixed rule");
        assert_eq!(precedence(Some("Learned"), None, false), Policy::Fixed, "the value is exact");
        assert_eq!([Policy::Fixed, Policy::Cap(1), Policy::Attention, Policy::Learned].map(Policy::name), ["fixed", "cap", "attention", "learned"]);
    }

    /// The placebo candidates of a decision: the applies whose lesson it delivers neither way, in no
    /// copy, and the session never had, each once (`draw_placebo`).
    fn candidates(applied: &[(&Challenge, u8)], r: &Routed, had: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (c, _) in applied {
            if matches!(r.route(&c.id), Route::Not | Route::Placebo) && !had.contains(&c.id) && !out.contains(&c.id) {
                out.push(c.id.clone());
            }
        }
        out
    }

    /// Cold start is today's rule: every policy but attention with a 3 decides exactly as
    /// conscience.rs `graded_without` with the session's `had` (`graded_capped` without one), in its
    /// order, with its choice as the propensities; the fixed rule and the learned policy draw their
    /// placebos among what they deliver neither way, the evaluation switches none.
    #[test]
    fn every_policy_without_a_cut_is_the_fixed_rule_on_every_grade_table() {
        let ls = lessons(14);
        let s = summary::Summary::default();
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let mut tables = tables(7);
        // Past `conscience::CAP` asks and hand-overs.
        tables.extend([vec![3; 12], [vec![1; 6], vec![3], vec![2; 7]].concat(), vec![2; 14], [vec![1; 11], vec![2; 3]].concat()]);
        // No lesson had (a record from before `nofree`), and some: the first, one past the cap, one
        // the model did not name.
        let some: Vec<String> = ["L00", "L03", "L11", "X"].map(String::from).to_vec();
        for had in [&[][..], &some[..]] {
            for grades in &tables {
                let applied: Vec<(&Challenge, u8)> = ls.iter().zip(grades.iter().copied()).collect();
                let at = Ctx { had, ..ctx(&none, &s) };
                for (policy, cap) in [(Policy::Fixed, None), (Policy::Learned, None), (Policy::Cap(2), Some(2))] {
                    let r = graded_under(policy, &applied, &at);
                    let (ask, free) = conscience::graded_without(&applied, cap, had);
                    assert_eq!((ids(&r.ask), ids(&r.free)), (ids(&ask), ids(&free)), "{policy:?} {grades:?} {had:?}");
                    if had.is_empty() {
                        let (ask, free) = conscience::graded_capped(&applied, cap);
                        assert_eq!((ids(&r.ask), ids(&r.free)), (ids(&ask), ids(&free)), "no lesson had: the rule of every older record");
                    }
                    assert_eq!(r.p, attention::fixed_p(&applied, &ask), "{policy:?} {grades:?}");
                    assert_eq!(r.pf, attention::fixed_p(&applied, &free), "{policy:?} {grades:?}");
                    assert!(r.log.attention.is_none() && r.log.gate == 0.0);
                    assert_eq!(r.log.policy, policy.name());
                    let later = candidates(&applied, &r, had);
                    match policy {
                        Policy::Cap(_) => assert!(r.placebo.is_empty() && r.log.psi == 0.0),
                        _ => {
                            assert_eq!(r.placebo.len(), later.len().min(PLACEBO_SLOTS), "{policy:?} {grades:?}");
                            assert!(ids(&r.placebo).iter().all(|p| later.contains(p)), "{policy:?} {grades:?}");
                            let psi = if later.is_empty() { 0.0 } else { outcome::round_p((PLACEBO_SLOTS as f64 / later.len() as f64).min(1.0)) };
                            assert_eq!(r.log.psi, psi);
                        }
                    }
                }
                if !grades.contains(&3) {
                    let r = graded_under(Policy::Attention, &applied, &at);
                    let (ask, free) = conscience::graded_without(&applied, None, had);
                    assert_eq!((ids(&r.ask), ids(&r.free), r.log.source), (ids(&ask), ids(&free), "fixed"), "no 3: no cut");
                    assert!(r.placebo.is_empty() && r.log.psi == 0.0, "an evaluation switch draws no placebo");
                }
            }
        }
    }

    #[test]
    fn attention_with_a_three_is_the_cut_on_the_outcomes_beside_the_lessons() {
        // No outcomes beside the lessons: the prior cut, 0 to 10, the rest handed over free.
        let ls = lessons(14);
        let s = summary::Summary::default();
        let grades: Vec<u8> = std::iter::once(3).chain(std::iter::repeat_n(1, 13)).collect();
        let applied: Vec<(&Challenge, u8)> = ls.iter().zip(grades).collect();
        let cold = store("attention-cold", &[]);
        let r = graded_under(Policy::Attention, &applied, &ctx(&cold, &s));
        assert_eq!((r.ask.len(), ids(&r.free)), (attention::PRIOR_K, ids(&applied[attention::PRIOR_K..].iter().map(|(c, _)| *c).collect::<Vec<_>>())));
        assert_eq!(r.p, attention::propensities(14, attention::PRIOR_K));
        assert_eq!(&r.pf[..attention::PRIOR_K], &[0.0; attention::PRIOR_K]);
        assert_eq!(&r.pf[attention::PRIOR_K..], &[0.9; 3], "free unless the check explores, which asks them");
        assert_eq!((r.log.policy, r.log.source), ("attention", "prior"));
        assert_eq!(r.log.attention.as_deref(), Some("#borg-attention k=11 n=0 alpha=0.0010 source=prior applied=14"));
        // Nine graded checks with applies beside the lessons: this one, the tenth, explores up to the rail.
        let graded = outcome::Outcome { kind: outcome::GRADED.into(), session: "x".into(), lessons: vec!["L0".into()], ..Default::default() };
        let nine = store("attention-explore", &vec![graded.clone(); attention::EXPLORE - 1]);
        let r = graded_under(Policy::Attention, &applied, &ctx(&nine, &s));
        assert_eq!((r.ask.len(), r.free.len(), r.log.source), (14, 0, "explore"));
        assert_eq!(r.p, attention::propensities(14, attention::PRIOR_K), "the propensities are the mixture's, not the override's");
        // A check without applies, or whose call failed, was not counted before 0.6.0 logged one: as the
        // ninth it does not make this check the tenth.
        let mut dirs = vec![cold, nine];
        let (empty, failed) = (outcome::Outcome { lessons: Vec::new(), ..graded.clone() }, outcome::Outcome { failed: true, ..graded.clone() });
        for (tag, ninth) in [("empty", empty), ("failed", failed)] {
            let mut log = vec![graded.clone(); attention::EXPLORE - 2];
            log.push(ninth);
            let unmoved = store(&format!("attention-{tag}"), &log);
            assert_eq!(graded_under(Policy::Attention, &applied, &ctx(&unmoved, &s)).log.source, "prior", "{tag}");
            dirs.push(unmoved);
        }
        for d in dirs {
            let _ = std::fs::remove_dir_all(d.parent().unwrap());
        }
    }

    #[test]
    fn free_propensities_mix_the_cut_and_the_explore_slice() {
        // 35 applies, cut 3: free 3..13 unless exploring (q = 0.1), then 20..30.
        let pf = free_p(35, 3);
        assert_eq!(&pf[..3], &[0.0; 3]);
        assert_eq!(&pf[3..13], &[0.9; 10]);
        assert_eq!(&pf[13..20], &[0.0; 7]);
        assert_eq!(&pf[20..30], &[0.1; 10]);
        assert_eq!(&pf[30..], &[0.0; 5]);
        assert_eq!(free_p(5, 11), [0.0; 5], "the cut and the rail ask all five");
        // Each rank's routes are disjoint: asked and free never add past 1.
        let p = attention::propensities(35, 3);
        assert!(p.iter().zip(&pf).all(|(a, f)| a + f <= 1.0 + 1e-9), "{p:?} {pf:?}");
    }

    #[test]
    fn a_route_names_the_list_that_holds_the_lesson() {
        let ls = lessons(3);
        let applied: Vec<(&Challenge, u8)> = ls.iter().zip([3, 1, 2]).collect();
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let s = summary::Summary::default();
        let mut r = graded_under(Policy::Fixed, &applied, &ctx(&none, &s));
        assert_eq!(ids(&r.ask), ["L00", "L02", "L01"], "3s, then 2s, then 1s");
        r.free = r.ask.split_off(2);
        r.placebo = r.free.split_off(0);
        assert_eq!(["L00", "L01", "L02", "L99"].map(|id| r.route(id)), [Route::Ask, Route::Placebo, Route::Ask, Route::Not]);
    }

    #[test]
    fn the_select_line_carries_the_decision_and_its_check() {
        let mut log = Log { policy: "learned", source: "fixed", ..Log::default() };
        assert_eq!(log.line(4, "0123456789ab"), "#borg-select policy=learned source=fixed fit=- gate=0.0000 applied=4 @0123456789ab");
        log.fit = "120-ab12".into();
        log.gate = 0.25;
        assert!(log.line(0, "ff").contains(" fit=120-ab12 gate=0.2500 applied=0 @ff"), "{}", log.line(0, "ff"));
        // Filtered from what the pool and the model read, like every borg marker.
        assert_eq!(conscience::work_text(&format!("{}\nwork\n", log.line(1, "ab"))), "work\n");
    }

    /// A fit sure that handing over beats asking, and a gate open wide: the learned policy deviates.
    fn deviating() -> summary::Summary {
        let at = |name: &str| policy::NAMES.iter().position(|n| *n == name).unwrap();
        let mut beta = vec![0.0; policy::P];
        beta[at("intercept")] = -0.5;
        beta[at("ask")] = -3.0;
        beta[at("not")] = -1.0;
        let mut chol = vec![0.0; policy::P * policy::P];
        for k in 0..policy::P {
            chol[k * policy::P + k] = 0.05;
        }
        let model = policy::Model { beta, chol, sd: vec![1.0; policy::P], spread: [0.1; 3], ..policy::Model::default() };
        let rates = policy::Rates { free_stop: 1.0, free_live: 1.0, later: 0.0, wait: 0.0, ask_live: 1.0 };
        let fit = policy::Fit { key: "9-abcdef01".into(), model: Some(model), rates, ..policy::Fit::default() };
        let gate = summary::Gate { gain: vec![0.0; policy::DRAWS], allowance: 5.0, ..summary::Gate::default() };
        summary::Summary { fit, gate, ..summary::Summary::default() }
    }

    #[test]
    fn the_learned_policy_routes_by_its_fit_and_logs_it() {
        let ls = lessons(4);
        let applied: Vec<(&Challenge, u8)> = ls.iter().zip([3, 1, 2, 1]).collect();
        let s = deviating();
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let live = Ctx { channel: "live", ..ctx(&none, &s) };
        let r = graded_under(Policy::Learned, &applied, &live);
        assert!(r.ask.is_empty(), "asking loses under this fit: {:?}", ids(&r.ask));
        assert_eq!(ids(&r.free), ["L00", "L02", "L01", "L03"], "higher grade first, then the model's order");
        assert_eq!((r.log.policy, r.log.source, r.log.fit.as_str()), ("learned", "learned", "9-abcdef01"));
        assert!(r.log.gate > 0.0);
        assert!(r.pf.iter().all(|p| *p > 0.9) && r.p.iter().all(|p| *p < 0.1), "{:?} {:?}", r.p, r.pf);
        // At the stop the 3 is asked whatever the fit says.
        let r = graded_under(Policy::Learned, &applied, &ctx(&none, &s));
        assert_eq!((ids(&r.ask), r.p[0]), (vec!["L00".to_string()], 1.0));
    }

    /// Guard: a store the monitor reverted keeps the fixed rule, whatever its fit and gate say.
    #[test]
    fn a_reverted_store_keeps_the_fixed_rule() {
        let ls = lessons(4);
        let applied: Vec<(&Challenge, u8)> = ls.iter().zip([3, 1, 2, 1]).collect();
        let mut s = deviating();
        s.monitor.reverted = true;
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let r = graded_under(Policy::Learned, &applied, &Ctx { channel: "live", ..ctx(&none, &s) });
        let (ask, free) = conscience::graded_capped(&applied, None);
        assert_eq!((ids(&r.ask), ids(&r.free)), (ids(&ask), ids(&free)));
        assert_eq!((r.log.source, r.log.gate), (REVERTED_SOURCE, 0.0));
        assert_eq!(r.p, attention::fixed_p(&applied, &ask));
        // The fixed rule for a session that had a lesson: never handed over free again.
        let had = ["L01".to_string()];
        let r = graded_under(Policy::Learned, &applied, &Ctx { channel: "live", had: &had, ..ctx(&none, &s) });
        assert_eq!(ids(&r.free), ids(&conscience::graded_without(&applied, None, &had).1));
        assert_eq!((r.log.source, r.pf[1]), (REVERTED_SOURCE, 0.0));
    }

    /// A lesson the session already had may be asked again but never takes a free slot: the next 2 or
    /// 1 takes it, under the fixed rule and the learned policy alike, its `pf` 0. What the record logs
    /// (`nofree`) is all a replay needs of `had`.
    #[test]
    fn a_lesson_the_session_had_is_asked_but_never_handed_over_free() {
        let ls = lessons(12);
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let s = summary::Summary::default();
        // A 2 the session had and eleven 1s: without it the cap hands over L00..L09; with it L01..L10.
        let soft: Vec<(&Challenge, u8)> = ls.iter().zip(std::iter::once(2).chain(std::iter::repeat_n(1, 11))).collect();
        let had = ["L00".to_string(), "X9".to_string()];
        let free = |had: &[String]| ids(&conscience::graded_without(&soft, None, had).1);
        assert_eq!(free(&[]), ids(&ls[..10].iter().collect::<Vec<_>>()));
        assert_eq!(free(&had), ids(&ls[1..11].iter().collect::<Vec<_>>()), "a 2 the session had takes no slot; the next 1 does");
        for policy in [Policy::Fixed, Policy::Learned] {
            let r = graded_under(policy, &soft, &Ctx { had: &had, ..ctx(&none, &s) });
            assert_eq!((ids(&r.free), r.pf[0]), (free(&had), 0.0), "{policy:?}");
            assert!(!ids(&r.placebo).contains(&"L00".to_string()), "{policy:?}: a lesson the session had is no placebo");
        }
        // With a 3 it is asked as before.
        let block: Vec<(&Challenge, u8)> = ls.iter().take(3).zip([3, 2, 1]).collect();
        let ask = |had: &[String]| ids(&conscience::graded_without(&block, None, had).0);
        assert_eq!(ask(&["L01".to_string()]), ask(&[]), "may still be asked");
        // The learned policy under a fit that hands everything over during the turn: not the one it had.
        let open = deviating();
        let applied: Vec<(&Challenge, u8)> = ls.iter().take(4).zip([2, 1, 2, 1]).collect();
        let live = Ctx { channel: "live", ..ctx(&none, &open) };
        let all = graded_under(Policy::Learned, &applied, &live);
        assert_eq!((all.log.source, all.route("L01")), ("learned", Route::Free), "the scenario: without it, L01 goes free");
        let had = ["L01".to_string()];
        let r = graded_under(Policy::Learned, &applied, &Ctx { had: &had, ..live });
        assert_eq!((r.route("L01"), r.pf[1]), (Route::Not, 0.0), "{:?}", r.pf);
        assert_eq!(ids(&r.free), ["L00", "L02", "L03"]);
        // `nofree`: the applies among `had`, each once, in the applies' order.
        let twice: Vec<(&Challenge, u8)> = [&ls[3], &ls[1], &ls[3], &ls[2]].into_iter().map(|c| (c, 1)).collect();
        assert_eq!(nofree(&twice, &["L02".to_string(), "L03".to_string(), "L99".to_string()]), ["L03", "L02"]);
        assert!(nofree(&twice, &[]).is_empty());
    }

    /// The fixed rule's placebo: up to `PLACEBO_SLOTS` of the applies it delivers neither way (past
    /// `CAP`), drawn by the check's seed with ψ = min(1, slots / candidates), never a lesson the session
    /// had or one another copy delivers; the evaluation switches draw none. What it asks and hands over
    /// is the same with or without it.
    #[test]
    fn the_fixed_rule_draws_its_placebos_among_what_it_delivers_neither_way() {
        let ls = lessons(14);
        let none = std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl");
        let s = summary::Summary::default();
        fn fixed<'a>(applied: &[(&'a Challenge, u8)], had: &[String], check: &str) -> Routed<'a> {
            let (s, none) = (summary::Summary::default(), std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl"));
            graded_under(Policy::Fixed, applied, &Ctx { check, had, ..ctx(&none, &s) })
        }
        // A 3 and eleven 1s: ten asked, the two left both drawn.
        let block: Vec<(&Challenge, u8)> = ls.iter().take(12).zip(std::iter::once(3).chain(std::iter::repeat_n(1, 11))).collect();
        let r = fixed(&block, &[], "0123456789ab");
        assert_eq!((r.ask.len(), ids(&r.placebo), r.log.psi), (conscience::CAP, vec!["L10".to_string(), "L11".to_string()], 1.0));
        // Fourteen 1s: ten free, two of the four left, each with ψ = 0.5, uniformly over the checks.
        let soft: Vec<(&Challenge, u8)> = ls.iter().map(|c| (c, 1)).collect();
        let mut drawn = std::collections::BTreeMap::<String, u32>::new();
        let checks = 200;
        for c in 0..checks {
            let check = format!("{c:012x}");
            let r = fixed(&soft, &[], &check);
            assert_eq!((r.free.len(), r.placebo.len(), r.log.psi), (conscience::CAP, PLACEBO_SLOTS, 0.5));
            assert_eq!(ids(&r.placebo), ids(&fixed(&soft, &[], &check).placebo), "the check's seed decides");
            assert!(r.placebo.windows(2).all(|w| w[0].id < w[1].id), "in the applies' order");
            for p in &r.placebo {
                *drawn.entry(p.id.clone()).or_default() += 1;
            }
        }
        assert_eq!(drawn.keys().cloned().collect::<Vec<_>>(), ["L10", "L11", "L12", "L13"]);
        assert!(drawn.values().all(|&n| (60..=140).contains(&n)), "each about half the checks: {drawn:?}");
        // A lesson the session had is no candidate: three left, ψ = 2/3.
        let had = ["L12".to_string()];
        for c in 0..20 {
            let r = fixed(&soft, &had, &format!("{c:012x}"));
            assert_eq!(r.log.psi, outcome::round_p(2.0 / 3.0));
            assert!(!ids(&r.placebo).contains(&had[0]), "{:?}", ids(&r.placebo));
        }
        // A lesson the check delivers in another copy, or names twice past the cap, is one candidate at most.
        let named: Vec<(&Challenge, u8)> = ls.iter().take(10).chain([&ls[0], &ls[12], &ls[12]]).map(|c| (c, 1)).collect();
        let r = fixed(&named, &[], "0123456789ab");
        assert_eq!((ids(&r.placebo), r.log.psi), (vec!["L12".to_string()], 1.0));
        // The evaluation switches draw none; the learned policy at cold start draws the same.
        for policy in [Policy::Cap(1), Policy::Attention] {
            let r = graded_under(policy, &soft, &ctx(&none, &s));
            assert!(r.placebo.is_empty() && r.log.psi == 0.0, "{policy:?}");
        }
        let cold = graded_under(Policy::Learned, &soft, &ctx(&none, &s));
        assert_eq!((ids(&cold.placebo), cold.log.psi), (ids(&graded_under(Policy::Fixed, &soft, &ctx(&none, &s)).placebo), 0.5));
    }

    /// Guard: the pool is the learned policy's only once its gate may risk something or an engineer set
    /// a budget, never under the fixed rule or on a reverted store. The same fit grows the pool when
    /// active, so the condition, not the fit, keeps the baseline.
    #[test]
    fn the_pool_is_the_learned_policys_once_its_gate_opens_or_a_budget_binds() {
        let pool = policy::PoolFit {
            yields: vec![(50.0, 10.0); 8],
            per_lesson: 300.0,
            latency: (1000.0, 0.1, 500.0),
            latency_n: policy::LATENCY_MIN as u32,
            not: (-1.0, 0.1),
            ..policy::PoolFit::default()
        };
        let base = crate::whole::TOKENS;
        let limit = Some(60_000);
        let unpriced = policy::Fit { pool: pool.clone(), ..policy::Fit::default() };
        let grows = (0..20).map(|i| format!("s{i}")).find(|x| policy::pool_tokens(&unpriced, true, limit, policy::session_seed(x)) > base);
        let session = grows.expect("the scenario: an active pool grows for some session");
        let at = |policy: Policy, s: &summary::Summary| (pool_active(policy, s), pool_tokens_under(policy, s, limit, &session));
        let mut s = summary::Summary { fit: unpriced, ..summary::Summary::default() };
        assert_eq!(at(Policy::Learned, &s), (false, base), "gate closed, no budget");
        s.gate.allowance = 1.0;
        assert!(at(Policy::Learned, &s).0 && at(Policy::Learned, &s).1 > base, "gate open");
        for policy in [Policy::Fixed, Policy::Cap(2), Policy::Attention] {
            assert_eq!(at(policy, &s), (false, base), "{policy:?}");
        }
        s.monitor.reverted = true;
        assert_eq!(at(Policy::Learned, &s), (false, base), "reverted");
        // A budget priced below the baseline binds with the gate closed.
        let mut s = summary::Summary { fit: policy::Fit { pool, budget: Some(1.0), lambda: 1e-2, ..policy::Fit::default() }, ..summary::Summary::default() };
        let (active, tokens) = at(Policy::Learned, &s);
        assert!(active && tokens < base, "budget, gate closed: {tokens}");
        assert_eq!(at(Policy::Fixed, &s), (false, base), "the fixed rule keeps the baseline under a budget");
        s.monitor.reverted = true;
        assert_eq!(at(Policy::Learned, &s), (false, base), "reverted under a budget");
    }

    /// The gate's guarantee on the hook's path: a learned check decides at its pool tokens, for the
    /// lessons its session had, and the ledger (select/summary.rs fold, select/policy.rs `ledger`)
    /// re-values it at the same logged tokens against the same fixed rule (`ledger_add_without` with the
    /// record's `nofree`), so each check's risk is what its ledger entry takes and the budget never falls
    /// below 0. The fit has a pool slope and every pool is a quarter of the baseline's: deciding at the
    /// baseline's pool instead admits gains the ledger then does not record.
    #[test]
    fn the_gate_admits_at_the_pool_its_ledger_records() {
        let at = |name: &str| policy::NAMES.iter().position(|n| *n == name).unwrap();
        let mut beta = vec![0.0; policy::P];
        for (name, b) in [("intercept", -0.5), ("ask", -0.4), ("not", -0.3), ("ask_dilution", -0.1), ("free_dilution", -0.1), ("pool", -1.0)] {
            beta[at(name)] = b;
        }
        // Wide on asking, so Thompson's draws deviate often; the pool slope drawn ≤ 0.
        let mut chol = vec![0.0; policy::P * policy::P];
        for k in 0..policy::P {
            chol[k * policy::P + k] = if k == at("ask") {
                1.0
            } else if k == at("pool") {
                0.5
            } else {
                0.3
            };
        }
        let model = policy::Model { beta, chol, sd: vec![1.0; policy::P], spread: [0.3; 3], ..policy::Model::default() };
        let rates = policy::Rates { free_stop: 1.0, free_live: 1.0, later: 0.5, wait: 0.0, ask_live: 1.0 };
        let fit = policy::Fit { key: "9-pool0001".into(), model: Some(model), rates, ..policy::Fit::default() };
        let mut s = summary::Summary { fit, ..summary::Summary::default() };
        let tokens = u32::try_from(crate::whole::TOKENS / 4).unwrap();
        let (ls, none) = (lessons(10), std::path::PathBuf::from("/nonexistent/borg-select/lessons.jsonl"));
        let mut rng = crate::memory::Rng::new(77);
        let mut deviated = 0;
        for c in 0..400u64 {
            let (n, from) = (1 + (rng.next_u64() % 3) as usize, (rng.next_u64() % 10) as usize);
            let applied: Vec<(&Challenge, u8)> = (0..n).map(|i| (&ls[(from + i) % 10], 1 + (rng.next_u64() % 3) as u8)).collect();
            let (session, check, stop) = (format!("t{}", c / 10), format!("{c:012x}"), c % 5 == 0);
            // Every other check's session already had one of the ten lessons.
            let had: Vec<String> = if c % 2 == 0 { vec![ls[(rng.next_u64() % 10) as usize].id.clone()] } else { Vec::new() };
            let ctx = Ctx { session: &session, check: &check, channel: if stop { "stop" } else { "live" }, lessons: &none, summary: &s, tokens, had: &had };
            let r = graded_under(Policy::Learned, &applied, &ctx);
            let us = units(&applied);
            let routes: Vec<Route> = applied.iter().map(|(l, _)| r.route(&l.id)).collect();
            assert!(applied.iter().zip(&r.pf).all(|((l, _), pf)| !had.contains(&l.id) || (r.route(&l.id) != Route::Free && *pf == 0.0)), "check {c}");
            let nofree = nofree(&applied, &had);
            deviated += usize::from(routes.iter().map(|x| if *x == Route::Placebo { Route::Not } else { *x }).ne(policy::fixed_routes(&us, &nofree)));
            let held = |g: &summary::Gate| policy::remaining(g) - g.allowance;
            let before = held(&s.gate);
            // The record as the hook logs it (hook.rs `graded_outcome`), folded as every reader folds
            // it (select/summary.rs `fold`): the ledger entry is the fold's own.
            let ids = |v: &[&Challenge]| v.iter().map(|c| c.id.clone()).collect::<Vec<_>>();
            let o = crate::outcome::Outcome {
                kind: crate::outcome::GRADED.into(),
                session: session.clone(),
                check: check.clone(),
                channel: ctx.channel.into(),
                lessons: applied.iter().map(|(c, _)| c.id.clone()).collect(),
                grades: applied.iter().map(|(_, g)| *g).collect(),
                ranks: (0..n as u16).collect(),
                applied: n as u16,
                chosen: ids(&r.ask),
                free: ids(&r.free),
                placebo: ids(&r.placebo),
                p: r.p.clone(),
                pf: r.pf.clone(),
                psi: r.log.psi,
                nofree,
                policy: r.log.policy.into(),
                fit: r.log.fit.clone(),
                source: r.log.source.into(),
                tokens,
                ..Default::default()
            };
            s = summary::fold(&format!("{}\n", serde_json::to_string(&o).unwrap()), s);
            assert!(
                (r.log.risk - (before - held(&s.gate)).max(0.0)).abs() < 1e-9,
                "check {c}: risked {} but the ledger took {}",
                r.log.risk,
                before - held(&s.gate)
            );
            assert!(policy::remaining(&s.gate) >= -1e-9, "check {c}: the gate overspent, {}", policy::remaining(&s.gate));
        }
        assert!(deviated > 0, "the learned policy deviated, so the gate was put to the test");
        // A refit replays the ledger over the summary (select/policy.rs `ledger`) as the folds built it,
        // under each check's `nofree`.
        assert_eq!(policy::ledger(&s), s.gate, "the refit's ledger is the folds'");
    }

    #[test]
    fn a_budget_is_read_for_the_collective_whose_store_it_is() {
        let base = Path::new("/home/x/.local/share/borg");
        let lessons = crate::collective::store(base, "team").join("lessons.jsonl");
        let cfg = |entry: &str| {
            serde_json::from_str::<crate::collective::Config>(&format!(r#"{{"collectives":[{{"name":"other","paths":[],"budget":5}},{entry}]}}"#)).unwrap()
        };
        assert_eq!(budget_in(&cfg(r#"{"name":"team","paths":[],"budget":400000}"#), &lessons), Some(400_000.0));
        assert_eq!(budget_in(&cfg(r#"{"name":"team","paths":[]}"#), &lessons), None, "off by default");
        assert_eq!(budget_in(&cfg(r#"{"name":"team","paths":[],"budget":0}"#), &lessons), None);
        assert_eq!(budget_in(&cfg(r#"{"name":"team","paths":[],"budget":"lots"}"#), &lessons), None);
    }

    #[test]
    fn a_check_id_is_short_stable_hex_and_differs_per_check() {
        let id = check_id("s", "stop", 1_000, 42);
        assert_eq!(id.len(), CHECK_HEX);
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
        assert_eq!(id, check_id("s", "stop", 1_000, 42), "stable");
        let others = [check_id("s", "live", 1_000, 42), check_id("s", "stop", 1_001, 42), check_id("s", "stop", 1_000, 43), check_id("t", "stop", 1_000, 42)];
        assert!(others.iter().all(|o| *o != id), "{others:?}");
        // The separators keep the parts apart: ("ab", "c") is not ("a", "bc").
        assert_ne!(check_id("ab", "c", 1, 1), check_id("a", "bc", 1, 1));
    }

    #[test]
    fn a_seed_depends_on_session_channel_and_check() {
        let (s, none) = (summary::Summary::default(), std::path::PathBuf::new());
        let at = |session, channel, check| seed(&Ctx { session, check, channel, lessons: &none, summary: &s, tokens: 0, had: &[] });
        assert_eq!(at("s", "stop", "a1"), at("s", "stop", "a1"));
        assert_ne!(at("s", "stop", "a1"), at("s", "live", "a1"));
        assert_ne!(at("s", "stop", "a1"), at("s", "stop", "a2"));
        assert_ne!(at("s", "stop", "a1"), at("t", "stop", "a1"));
    }
}
