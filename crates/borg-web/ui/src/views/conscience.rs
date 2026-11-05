//! Conscience: one page that answers a developer's questions, top-down. Is the conscience working (turn
//! ends, checks, checks that did not finish)? Did the agent act on what borg said? What does it cost?
//! Which lessons matter? How are lessons picked, and is the learned policy safe? The statistical
//! internals sit last, folded, for the author. Every card names its question, its unit, how to read it
//! (hover help) and, in the help only, its raw names (stats.rs kinds, borg top.rs `Selection`).

use std::collections::BTreeMap;

use leptos::prelude::*;

use crate::api::{Count, Estimate, Lesson, Selection, Totals, data};
use crate::charts::{Donut, Help, Row, Series, StackBar, StackedChart, Tile};
use crate::util::{duration, num, pct, short_day, trunc, usd};
use crate::views::overview::delta;

/// The judged placebos needed before leaving a lesson for later is learned, for a server that names
/// none (borg select/policy.rs `NOT_LEARNED_UNTIL`, next to `NOT_PRIOR`).
const PLACEBOS_NEEDED: f64 = 100.0;
/// Below this many, a share is marked "few": too few to read.
const FEW: f64 = 20.0;
/// Judged sessions a lesson needs before its rate is shown.
const RATE_FROM: f64 = 5.0;

fn ratio(a: f64, b: f64) -> f64 {
    if b > 0.0 { a / b } else { 0.0 }
}

fn sigmoid(x: f64) -> f64 {
    1.0 / (1.0 + (-x).exp())
}

fn text(s: &'static str) -> Signal<String> {
    Signal::derive(move || s.to_string())
}

fn fixed(t: String) -> Signal<String> {
    Signal::derive(move || t.clone())
}

/// A verdict line's pieces: plain text, a number in bold, a warning, a good state.
fn plain(s: impl Into<String>) -> AnyView {
    let s: String = s.into();
    view! { <span>{s}</span> }.into_any()
}

fn strong(s: impl Into<String>) -> AnyView {
    let s: String = s.into();
    view! { <b>{s}</b> }.into_any()
}

fn tone(class: &'static str, parts: Vec<AnyView>) -> AnyView {
    view! { <span class=class>{parts}</span> }.into_any()
}

/// A section's title and its computed verdict line.
#[component]
fn SectionHead(title: &'static str, children: Children) -> impl IntoView {
    view! {
        <div class="section-head">
            <h1>{title}</h1>
            <p class="verdict">{children()}</p>
        </div>
    }
}

/// A panel heading: its question, hover help and the unit.
fn head(title: &'static str, help: &'static str, sub: &'static str) -> impl IntoView {
    view! { <h2>{title}<Help text=text(help) />{(!sub.is_empty()).then(|| view! { <small>{sub}</small> })}</h2> }
}

fn sub_head(title: &'static str, help: &'static str) -> impl IntoView {
    view! { <h3>{title}<Help text=text(help) /></h3> }
}

/// One bar row in `HBars`' style, in the given order: a label, a fill share (0 to 1), its value, a hover title.
fn bar(label: String, share: f64, value: String, title: String, color: &'static str) -> impl IntoView {
    bar_few(label, share, value, title, color, false)
}

fn bar_few(label: String, share: f64, value: String, title: String, color: &'static str, few: bool) -> impl IntoView {
    view! {
        <div class="hbar wrap" class:few=few title=title>
            <span class="hbar-l">{label}</span>
            <span class="hbar-t"><span class="hbar-f" style=format!("width:{:.1}%;background:{color}", share.clamp(0.0, 1.0) * 100.0)></span></span>
            <span class="hbar-v">{value}</span>
        </div>
    }
}

/// Verdicts per lesson delivery: the sum of the lessons' `usage` per verdict kind (one per lesson per
/// verdict, unlike the stats log's verdict events, which can cover several lessons each). Not
/// `not_corrected`: the session-end settle writes it for every asked lesson without a correction, judged
/// or not (borg outcome.rs `settle_with`), so it says nothing about coverage.
#[derive(Clone, Copy, Default, PartialEq)]
struct Verdicts {
    acted: f64,
    not: f64,
    corr: f64,
}

impl Verdicts {
    fn of(ls: &[Lesson]) -> Self {
        let u = |l: &Lesson, k: &str| l.usage.get(k).copied().unwrap_or(0.0);
        ls.iter().fold(Verdicts::default(), |v, l| Verdicts {
            acted: v.acted + u(l, "changed"),
            not: v.not + u(l, "ignored"),
            corr: v.corr + u(l, "corrected"),
        })
    }

    fn judged(&self) -> f64 {
        self.acted + self.not + self.corr
    }
}

/// Verdict coverage of the graded checks' asked and passed-along lessons (borg top.rs labels
/// `judged, asked` and `judged, free`): (judged, delivered). A verdict there includes 'never came up'.
fn coverage_of(s: &Selection) -> (f64, f64) {
    s.labels.iter().filter(|c| c.name == "judged, asked" || c.name == "judged, free").fold((0.0, 0.0), |(p, w), c| (p + c.part, w + c.whole))
}

/// A lesson's judged sessions (strongest verdict per session).
fn judged_sessions(l: &Lesson) -> f64 {
    l.changed + l.ignored + l.corrected
}

/// The lessons with the highest `value`, clickable.
#[component]
pub fn LessonRank(
    #[prop(into)] items: Signal<Vec<(Lesson, f64)>>,
    unit: &'static str,
    color: &'static str,
    #[prop(default = "None yet.")] empty: &'static str,
) -> impl IntoView {
    let d = data();
    move || {
        let mut it = items.get();
        it.retain(|x| x.1 > 0.0);
        it.sort_by(|a, b| b.1.total_cmp(&a.1));
        if it.is_empty() {
            return view! { <p class="empty">{empty}</p> }.into_any();
        }
        let mx = it[0].1;
        it.into_iter()
            .take(8)
            .map(|(l, v)| {
                let id = l.id.clone();
                view! {
                    <button class="rank" title=format!("{}\nConsequence: {}", l.question, l.consequence) on:click=move |_| d.open.set(Some(id.clone()))>
                        <span class="rank-q">{trunc(&l.question, 120)}</span>
                        <span class="rank-bar"><span style=format!("width:{:.0}%;background:{color}", v / mx * 100.0)></span></span>
                        <span class="rank-v">{format!("{} {unit}", num(v))}</span>
                    </button>
                }
            })
            .collect_view()
            .into_any()
    }
}

#[component]
pub fn Conscience() -> impl IntoView {
    view! {
        <p class="how">
            "At every turn end borg looks at the agent's work. Turns whose words match no lesson well enough pass free (no model call; with BORG_STOP_GATE=edits, turns without edits pass free instead). "
            "The rest get one model check, which grades the lessons that apply; a policy then decides which to ask as questions and which to pass along as side notes. "
            "A judge model (at later checks and at session end) records whether the agent acted on each lesson."
        </p>
        <Working />
        <Landed />
        <Costs />
        <Lessons />
        <LessonSelection />
    }
}

/// A: what happens at turn ends, whether it costs anything, whether checks finish.
#[component]
fn Working() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let l = move || st.with(|s| s.last(7));
    let spark = move |k: &'static str| Signal::derive(move || st.with(|s| s.series(k, 30)));
    let verdict = move || {
        let l = l();
        let (turns, skip, check, dropped, failed) = (l.n("turn_end"), l.n("gate_skip"), l.n("check"), l.n("check_dropped"), l.n("check:failed"));
        let mut v =
            vec![plain("Last 7 days: "), strong(num(turns)), plain(" turn ends, "), strong(pct(skip, turns)), plain(" passed free, "), strong(num(check))];
        v.push(plain(" paid checks, "));
        let drop = vec![strong(num(dropped)), plain(" dropped")];
        v.push(if dropped > 0.25 * turns && turns > 0.0 { tone("warn", drop) } else { tone("", drop) });
        if failed > 0.0 {
            v.push(plain(", "));
            let f = vec![strong(num(failed)), plain(" failed")];
            v.push(if failed > 0.05 * check { tone("warn", f) } else { tone("", f) });
        }
        v.push(plain("."));
        v
    };
    let failed_warn = Signal::derive(move || {
        let l = l();
        l.n("check:failed") > 0.05 * l.n("check") && l.n("check") > 0.0
    });
    let skip_sub = Signal::derive(move || {
        let l = l();
        let (noedit, score, empty, all) = (l.n("gate_skip:noedit"), l.n("gate_skip:score"), l.n("gate_skip:empty"), l.n("gate_skip"));
        let (live, other) = (l.n("gate_skip:live_covered"), l.n("gate_skip:other"));
        let known = noedit + score + empty + live + other;
        let mut s = format!("{} nothing matched well enough · {} no lessons", num(score), num(empty));
        if noedit > 0.0 {
            s.push_str(&format!(" · {} no edits", num(noedit)));
        }
        if live > 0.0 {
            s.push_str(&format!(" · {} covered by live mode (before 0.7.6)", num(live)));
        }
        if other > 0.0 {
            s.push_str(&format!(" · {} other", num(other)));
        }
        if known < all {
            s.push_str(&format!(" · reasons known for {} of {}", num(known), num(all)));
        }
        s
    });
    let check_sub = Signal::derive(move || {
        let l = l();
        let check = l.n("check");
        let each = l.usd_by.get("check").copied().unwrap_or(0.0) / check.max(1.0);
        let avg = if l.n("check_timed") > 0.0 { format!(" · avg {}", duration(l.n("check_ms") / l.n("check_timed"))) } else { String::new() };
        let live = if l.n("live_check") > 0.0 { format!(" · + {} live-mode calls (borg < 0.7.6)", num(l.n("live_check"))) } else { String::new() };
        format!("{} each · {} failed{avg}{live}", usd(each), num(l.n("check:failed")))
    });
    let lessons = move || d.lessons.with(Vec::len) as f64;

    // Per day: the stack adds up to the turn ends. A detached check's asks are `live_ask:async` (borg
    // 0.7.9 on); other `live_ask` came from live mode's checks during the turn (before 0.7.6) or from a
    // 0.7.7-0.7.8 detached check: those count only on days without `live_check`. A failed check asked
    // nothing: it joins the dropped layer.
    let rows = Signal::derive(move || {
        st.with(|s| {
            s.days
                .iter()
                .map(|day| {
                    let t = &day.t;
                    let detached = t.n("live_ask:async");
                    let ask = detached + if t.n("live_check") == 0.0 { (t.n("live_ask") - detached).max(0.0) } else { 0.0 };
                    let (check, block, failed) = (t.n("check"), t.n("block"), t.n("check:failed"));
                    Row {
                        label: short_day(day.day),
                        vals: vec![t.n("gate_skip"), (check - failed - block - ask).max(0.0), ask, block, t.n("check_dropped") + failed],
                    }
                })
                .collect::<Vec<_>>()
        })
    });
    let series = vec![
        Series::new("skip", "passed free", "var(--c-slate)"),
        Series::new("quiet", "checked, nothing to ask", "var(--c-accent)"),
        Series::new("next", "asked with the next prompt", "var(--c-teal)"),
        Series::new("block", "asked in an extra round", "var(--c-violet)"),
        Series::new("dropped", "check did not finish (failed or dropped)", "color-mix(in srgb, var(--c-heat) 60%, transparent)"),
    ];

    // Checks that never delivered, last 7 days and all time; a reason older borgs did not record is its own row.
    let unfinished = move || {
        let (l, t) = (l(), t());
        let rows = |x: &Totals| {
            let r = [
                x.n("check:failed"),
                x.n("check_dropped:superseded"),
                x.n("check_dropped:gone"),
                x.n("check_dropped:unstarted") + x.n("check_dropped:unplaced"),
            ];
            let unknown = (x.n("check_dropped") - r[1] - r[2] - r[3]).max(0.0);
            (r, unknown)
        };
        let ((week, week_unknown), (all, all_unknown)) = (rows(&l), rows(&t));
        let whole = t.n("check") + t.n("check_dropped");
        let labels = [
            ("failed (no answer)", "the model call errored or ran out of time; nothing was asked that turn"),
            ("replaced by a newer turn", "the agent finished another turn before this check started; the newer turn is checked instead"),
            ("session ended first", "the session ended before the check could run"),
            ("never started", "the check was queued but never placed or started"),
        ];
        let mut out: Vec<AnyView> = Vec::new();
        if week.iter().sum::<f64>() + week_unknown == 0.0 {
            out.push(view! { <p class="empty">"None in the last 7 days."</p> }.into_any());
        }
        if all.iter().sum::<f64>() + all_unknown > 0.0 {
            for (i, (label, why)) in labels.iter().enumerate() {
                out.push(
                    bar(
                        label.to_string(),
                        ratio(all[i], whole),
                        format!("{} · {}", num(week[i]), num(all[i])),
                        format!("{why}. Last 7 days {}, all time {} of {} checks.", num(week[i]), num(all[i]), num(whole)),
                        "var(--c-heat)",
                    )
                    .into_any(),
                );
            }
            if all_unknown > 0.0 {
                out.push(
                    bar(
                        "dropped, reason not recorded".into(),
                        ratio(all_unknown, whole),
                        format!("{} · {}", num(week_unknown), num(all_unknown)),
                        "Dropped checks counted by a borg older than 0.7.9, which did not fold the reason.".into(),
                        "var(--c-slate)",
                    )
                    .into_any(),
                );
            }
            out.push(view! { <p class="how">"Value: last 7 days · all time. Bars: share of all checks, run or dropped. Failed checks are counted by borg 0.7.9 and later."</p> }.into_any());
        }
        out
    };

    view! {
        <SectionHead title="Is the conscience working?">{verdict}</SectionHead>
        <section class="tiles">
            <Tile label="Turn ends seen" value=Signal::derive(move || num(l().n("turn_end")))
                help_dyn=Signal::derive(move || format!("Each time an agent finished a turn, borg looked at it. Turns whose words match no lesson well enough pass free with no model call (with BORG_STOP_GATE=edits: turns without edits). The rest get one paid check. A few checks are dropped because a newer turn replaced them or the session ended first. Neither high nor low is good; the free share is a cost lever. Last 7 days; lifetime: {}. (raw: turn_end, gate_skip, check_dropped)", num(t().n("turn_end"))))
                sub=Signal::derive(move || { let l = l(); format!("{} passed free · {} check dropped", pct(l.n("gate_skip"), l.n("turn_end")), pct(l.n("check_dropped"), l.n("turn_end"))) })
                spark=spark("turn_end") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("turn_end")))) color="var(--c-muted)" />
            <Tile label="Passed free" value=Signal::derive(move || { let l = l(); pct(l.n("gate_skip"), l.n("turn_end")) })
                help="A turn end passes free when the word match between the turn and the lessons scores below the bar, or when the collective has no candidate lessons (with BORG_STOP_GATE=edits: also when the agent made no edits). Free means no model call and no tokens. A falling share while most turns are small means the filter lets too much through. Reasons are counted by borg 0.7.9 and later; teammates on an older borg add skips without a reason. Last 7 days. (raw: gate_skip, why score / empty / noedit / live_covered)"
                sub=skip_sub spark=spark("gate_skip") color="var(--c-slate)" />
            <Tile label="Paid checks" value=Signal::derive(move || num(l().n("check")))
                help="One model call per turn end that was not passed free. The call reads the work and the lessons allowed here and grades the ones that apply; a policy then decides what to ask and what to pass along. Cost and time are the last 7 days' average. A failed check asked nothing that turn and should stay near 0; failures are counted only on machines with borg 0.7.9 or later. Live mode's mid-turn calls (teammates on borg before 0.7.6) are not counted here (see 'What does it cost?'). (raw: check, check:failed, check_ms)"
                sub=check_sub warn=failed_warn spark=spark("check") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("check")))) up_is_good=false />
            <Tile label="Lessons read per check" value=Signal::derive(move || { let l = l(); num(ratio(l.n("check_n"), l.n("check"))) })
                help="Each paid check shows the model every lesson allowed here (in scope for where the session works, not superseded, not already done) up to a token budget (12k by default, or the learned pool size); the model picks the ones that apply. The share of the collective is set by scope and that budget. More lessons cost more tokens per check. Last 7 days. (raw: check_n / check)"
                sub=Signal::derive(move || { let l = l(); let per = ratio(l.n("check_n"), l.n("check")); format!("of {} lessons in the collective ({})", num(lessons()), pct(per, lessons())) })
                color="var(--c-violet)" />
        </section>
        <section class="panel">
            {head(
                "What happened at each turn end",
                "Each bar is one day's turn ends. Passed free: no model call. Checked, nothing to ask: one call; every lesson passed along, or none applied. Asked with the next prompt: the detached check (0.7.7+) had questions, which reached the agent with its next prompt. Asked in an extra round: the agent was stopped once to answer questions (before 0.7.7, or blocking checks). Check did not finish: the model call failed, a newer turn replaced the check, or the session ended first. (raw: gate_skip, check, check:failed, live_ask, live_ask:async, block, check_dropped)",
                "per day; the stack adds up to the turn ends",
            )}
            <StackedChart rows=rows series=series ranges=true />
            <p class="how">"Healthy: mostly free and checked bars with a thin asked layer. Warning: a growing did-not-finish layer, or an asked layer near the whole bar."</p>
        </section>
        <section class="panel">
            {head(
                "Checks that did not finish",
                "Detached checks (the default since 0.7.7) run after the turn. These are the ones that never delivered: the call failed, or the check was dropped before it ran. (raw: check:failed, check_dropped:superseded / gone / unstarted / unplaced)",
                "checks that never delivered, by reason",
            )}
            {unfinished}
        </section>
    }
}

/// B: does what borg says land?
#[component]
fn Landed() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let sel = d.select;
    let t = move || st.with(|s| s.totals.clone());
    let v = Memo::new(move |_| d.lessons.with(|ls| Verdicts::of(ls)));
    let cov = Memo::new(move |_| sel.with(coverage_of));
    let verdict = move || {
        let v = v.get();
        let (judged, whole) = cov.get();
        let mut out = vec![
            strong(pct(v.acted, v.judged())),
            plain(format!(" of judged lessons whose situation came up were acted on ({} of {})", num(v.acted), num(v.judged()))),
        ];
        if whole > 0.0 {
            out.push(plain(" · the judge gave a verdict on "));
            out.push(strong(pct(judged, whole)));
            out.push(plain(" of asked or passed-along lessons"));
        }
        if v.corr > 0.0 {
            out.push(plain(" · "));
            out.push(tone("warn", vec![strong(num(v.corr)), plain(" were said and still corrected")]));
        } else {
            out.push(plain(" · "));
            out.push(tone("ok", vec![plain("none was corrected anyway")]));
        }
        out.push(plain("."));
        out
    };
    let parts = Signal::derive(move || {
        let v = v.get();
        vec![
            ("acted on".to_string(), v.acted, "var(--c-good)".to_string(), "the agent changed its work on that point".to_string()),
            ("not acted on".into(), v.not, "var(--c-slate)".into(), "delivered; the agent carried on unchanged".into()),
            ("corrected anyway".into(), v.corr, "var(--c-heat)".into(), "said, and the engineer still had to correct it".into()),
        ]
    });
    let center = Signal::derive(move || {
        let v = v.get();
        pct(v.acted, v.judged())
    });
    let coverage = Signal::derive(move || {
        let (judged, whole) = cov.get();
        vec![
            (
                "judged".to_string(),
                judged,
                "var(--c-accent)".to_string(),
                "the judge gave a verdict: acted on, not acted on, or the situation never came up (raw: labels judged, asked / judged, free)".to_string(),
            ),
            (
                "not judged".into(),
                (whole - judged).max(0.0),
                "var(--c-slate)".into(),
                "The judge gave no verdict: its tries or its prompt room ran out, or the verdict is still to come. Unknown, not good or bad. The more lessons are not judged, the less certain the rate above.".into(),
            ),
        ]
    });
    let relevance = move || {
        let c = sel.with(|s| s.labels.iter().find(|c| c.name == "not arisen").cloned())?;
        (c.whole > 0.0).then(|| {
            view! {
                <p class="how" title="Answers of judge version 3 (borg 0.7.5) on, pending ones included. (raw: label not arisen)">
                    "Of the judge's answers since borg 0.7.5, "<b>{pct(c.part, c.whole)}</b>" ("
                    {format!("{} of {}", num(c.part), num(c.whole))}") found the situation never came up in the session; those are not in the ring. A high share means lessons are delivered more broadly than needed."
                </p>
            }
        })
    };
    // Verdict events per delivery path: the only per-channel split there is.
    let by_channel = move || {
        let t = t();
        let paths = [
            ("stop", "asked in an extra round"),
            ("live", "asked with the next prompt (live mode before 0.7.6: during the turn)"),
            ("queued", "side note with the next prompt"),
            ("early", "up front, when the prompt or a file matched"),
        ];
        let kinds = ["changed", "ignored", "corrected"];
        let mut rows: Vec<(String, f64, f64, String)> = paths
            .iter()
            .map(|(c, label)| (label.to_string(), t.via("changed", c), kinds.iter().map(|k| t.via(k, c)).sum::<f64>(), format!("raw channel: {c}")))
            .collect();
        let rest = |k: &str| t.n(k) - paths.iter().map(|(c, _)| t.via(k, c)).sum::<f64>();
        let rest_n: f64 = kinds.iter().map(|k| rest(k).max(0.0)).sum();
        if rest_n > 0.0 {
            rows.push((
                "delivery path not recorded (older)".into(),
                rest("changed").max(0.0),
                rest_n,
                "verdicts from before delivery paths were recorded".into(),
            ));
        }
        rows.into_iter()
            .filter(|r| r.2 > 0.0)
            .map(|(label, changed, n, raw)| {
                let few = n < FEW;
                let label = if few { format!("{label} (few)") } else { label };
                bar_few(
                    label,
                    ratio(changed, n),
                    format!("{} · {} of {}", pct(changed, n), num(changed), num(n)),
                    format!("{} verdicts ({raw})", num(n)),
                    "var(--c-good)",
                    few,
                )
            })
            .collect_view()
    };
    let reach = Signal::derive(move || {
        let t = t();
        vec![
            (
                "asked in an extra round".to_string(),
                t.n("block_n"),
                "var(--c-violet)".to_string(),
                "the agent was stopped once to answer them (raw: block)".to_string(),
            ),
            (
                "asked with the next prompt".into(),
                t.n("live_ask_n"),
                "var(--c-teal)".into(),
                "a detached check's questions, delivered with the agent's next prompt; before 0.7.6 also live mode's questions during the turn (raw: live_ask)"
                    .into(),
            ),
            (
                "side note".into(),
                t.n("queued_free_n"),
                "var(--c-accent)".into(),
                "passed along free with the next prompt (a check without a grade 3) (raw: queued_free)".into(),
            ),
            (
                "up front".into(),
                t.n("early_n"),
                "var(--c-good)".into(),
                "handed over without a model call when the prompt or a touched file clearly matched (raw: early)".into(),
            ),
        ]
    });
    let reach_facts = move || {
        let t = t();
        view! {
            <p class="how">
                "Questions at once: "<b>{num(ratio(t.n("block_n"), t.n("block")))}</b>{format!(" per extra round ({} rounds) · ", num(t.n("block")))}
                <b>{num(ratio(t.n("live_ask_n"), t.n("live_ask")))}</b>{format!(" with the next prompt ({} deliveries).", num(t.n("live_ask")))}
            </p>
        }
    };
    let rank = Signal::derive(move || d.lessons.with(|ls| ls.iter().map(|l| (l.clone(), l.corrected)).collect::<Vec<_>>()));

    view! {
        <SectionHead title="Did the agent act on what borg said?">{verdict}</SectionHead>
        <section class="panel">
            {head(
                "Did the agent act on it?",
                "A judge model (at later checks and at session end) reads what happened after each delivered lesson. Acted on: the agent changed its work on that point, in a way that avoids the lesson's consequence. Not acted on: it carried on unchanged. Corrected anyway: it carried on, and the engineer then corrected exactly that. Lessons whose situation never came up are left out. A model's verdict, not ground truth. Counted per lesson delivery, every way of delivering. (raw: usage changed, ignored, corrected)",
                "judged lesson deliveries whose situation came up, all ways of delivering",
            )}
            <Donut parts=parts center=center caption="of judged lessons whose situation came up, acted on" />
            {move || {
                let (judged, whole) = cov.get();
                (whole > 0.0).then(|| view! {
                    <p class="how" title="Lessons asked or passed along by the graded checks in the lesson-selection summary, lifetime; up-front lessons are not in it.">
                        {format!("Verdict coverage: the judge gave a verdict on {} of {} asked or passed-along lessons", num(judged), num(whole))}
                    </p>
                    <StackBar parts=coverage />
                })
            }}
            {relevance}
            {sub_head(
                "Acted on, by how it was delivered",
                "Verdict events per delivery path; one verdict can cover several lessons, so these counts differ from the ring above. (raw: changed@channel, ignored@channel, corrected@channel)",
            )}
            {by_channel}
            <p class="how">"If side notes land about as often as questions, asking costs flow for nothing. If questions land far more often, asking is worth it."</p>
        </section>
        <section class="two">
            <div class="panel">
                {head(
                    "How lessons reached agents",
                    "Every check grades the lessons it read: 3 = must ask, 2 = worth knowing, 1 = minor. When a check holds a grade 3, every lesson it graded is asked (1s up to a cap); otherwise all are passed along as side notes. Questions arrive with the agent's next prompt since 0.7.7 (before, the agent was stopped for one extra round). Side notes ride along with the next prompt for free. Up-front lessons are handed over without a model call when the prompt or a touched file clearly matches. Whether many questions at once dilute each other is what the question-count experiment measures. (raw: block_n, live_ask_n, queued_free_n, early_n)",
                    "lessons, lifetime, by delivery path",
                )}
                <StackBar parts=reach />
                {reach_facts}
            </div>
            <div class="panel">
                {head(
                    "Said, and still corrected",
                    "Borg delivered this lesson, the agent went ahead anyway, and the engineer corrected the same point. Read these: the lesson may be too vague or too long, or it arrived as a side note when it should have been a question. (raw: corrected)",
                    "the lesson was delivered, and the engineer then had to correct exactly that (sessions)",
                )}
                <LessonRank items=rank unit="sessions" color="var(--c-heat)" empty="None: nothing borg said was corrected afterwards." />
            </div>
        </section>
    }
}

/// C: money and interruptions, now and per lesson that landed.
#[component]
fn Costs() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let sel = d.select;
    let t = move || st.with(|s| s.totals.clone());
    let l = move || st.with(|s| s.last(7));
    let by = |t: &Totals, k: &str| t.usd_by.get(k).copied().unwrap_or(0.0);
    let spend = move |t: &Totals| by(t, "check");
    // Live mode's calls in the window: teammates still on a borg before 0.7.6.
    let live_note = move |t: &Totals| {
        (t.n("live_check") > 0.0).then(|| format!("+ {} for {} live-mode calls from teammates on borg < 0.7.6", usd(by(t, "live_check")), num(t.n("live_check"))))
    };
    let verdict = move || {
        let l = l();
        let check = by(&l, "check");
        let mut v = vec![
            plain("Last 7 days: "),
            strong(usd(check)),
            plain(" for "),
            strong(num(l.n("check"))),
            plain(" checks ("),
            strong(usd(ratio(check, l.n("check")))),
            plain(" each)"),
        ];
        if l.n("changed") > 0.0 {
            v.push(plain(", "));
            v.push(strong(usd(check / l.n("changed"))));
            v.push(plain(" per acted-on verdict"));
        }
        if let Some(n) = live_note(&l) {
            v.push(plain(" · "));
            v.push(tone("warn", vec![plain(n)]));
        }
        v.push(plain("."));
        v
    };
    let tiles = move || {
        let s = sel.get();
        let (spent, rounds) = s.spend.iter().fold((0.0, 0.0), |(u, r), x| (u + x.usd, r + x.rounds));
        let per = if s.sessions > 0.0 && !s.spend.is_empty() { usd(spent / s.sessions) } else { "–".into() };
        let per_sub = if s.sessions > 0.0 {
            format!("{} extra agent rounds · {} sessions", num(rounds / s.sessions), num(s.sessions))
        } else {
            "not measured yet".into()
        };
        let budget = s.budget.clone().map(|b| {
            view! {
                <Tile label="Lesson-reading limit" value=fixed(format!("{} of {}", num(b.spent), num(b.tokens)))
                    help_dyn=fixed(format!("An engineer set a limit on how many tokens of lessons checks may read per session. When it binds, borg shows fewer lessons per check. The 'price' {:.1e} is how strongly the limit pushes back (a shadow price per token), not dollars. (raw: budget.tokens, lambda)", b.price))
                    sub=fixed(format!("lesson tokens per session · {}", if b.price > 0.0 { "binding" } else { "not binding" }))
                    color="var(--c-amber)" />
            }
        });
        view! {
            <Tile label="Per session" value=fixed(per)
                help="Average over the sessions the lesson-selection summary holds (graded checks only), so it can differ from the tiles beside it. An extra round is a turn end where borg's questions made the agent continue. (raw: selection spend, rounds)"
                sub=fixed(per_sub) color="var(--c-amber)" />
            {budget}
        }
    };
    let rows = move || {
        let s = sel.get();
        let max = s.spend.iter().map(|x| x.usd).fold(0.0, f64::max);
        s.spend
            .iter()
            .map(|x| {
                let label = match x.source.as_str() {
                    "stop" => "at the turn end (blocking)".to_string(),
                    "live" => "detached after the turn (0.7.7+) / live mode before 0.7.6".to_string(),
                    other => other.to_string(),
                };
                bar(
                    label,
                    ratio(x.usd, max),
                    usd(x.usd / s.sessions.max(1.0)),
                    format!("{} calls, {} per check, {} in all (raw source: {})", num(x.calls), usd(ratio(x.usd, x.calls)), usd(x.usd), x.source),
                    "var(--c-amber)",
                )
            })
            .collect_view()
    };
    let reconcile = move || {
        let s = sel.get();
        format!(
            "The tiles count the last 7 days of turn-end checks from the stats log ({} checks); this panel and 'Per session' count the {} graded checks in the selection summary ({} sessions), lifetime. Different scopes, so the numbers differ.",
            num(l().n("check")),
            num(s.checks),
            num(s.sessions)
        )
    };
    let history = move || {
        let t = t();
        (t.n("live_check") > 0.0).then(|| {
            view! {
                <p class="how">
                    "Live mode (borg before 0.7.6; teammates still on it keep adding calls) called the model during turns, "<b>{num(t.n("live_check"))}</b>" times for "
                    <b>{usd(by(&t, "live_check"))}</b>{format!(" ({} of all borg spend).", pct(by(&t, "live_check"), t.usd))}
                </p>
            }
        })
    };

    view! {
        <SectionHead title="What does it cost?">{verdict}</SectionHead>
        <section class="tiles">
            <Tile label="Checking spend, 7 days" value=Signal::derive(move || usd(spend(&l())))
                help_dyn=Signal::derive(move || format!("Model cost of the conscience's turn-end checks. A check also judges earlier questions of the session. Live mode's calls (teammates on borg before 0.7.6) are not in it; the note below names them when there are any. Lifetime: {}. (raw: usd_by.check)", usd(by(&t(), "check"))))
                sub=Signal::derive(move || { let l = l(); let each = format!("{} per check", usd(ratio(by(&l, "check"), l.n("check")))); live_note(&l).map_or(each.clone(), |n| format!("{each} · {n}")) })
                warn=Signal::derive(move || l().n("live_check") > 0.0)
                spark=Signal::derive(move || st.with(|s| s.days.iter().rev().take(30).rev().map(|d| spend(&d.t)).collect::<Vec<_>>()))
                delta=Signal::derive(move || st.with(|s| delta(s, spend))) color="var(--c-amber)" up_is_good=false />
            <Tile label="Cost per acted-on verdict" value=Signal::derive(move || { let l = l(); if l.n("changed") > 0.0 { usd(by(&l, "check") / l.n("changed")) } else { "–".into() } })
                help="Turn-end check spend divided by the judge's 'acted on' verdict events in the same period. One verdict event can cover several lessons, so this is per verdict, not per lesson. Lower is better. A rising cost per verdict with steady volume means checks are paying for nothing. Live mode's spend (before 0.7.6) is left out. (raw: usd_by.check / changed)"
                sub=Signal::derive(move || { let t = t(); format!("last 7 days; lifetime {} (turn-end checks only)", if t.n("changed") > 0.0 { usd(by(&t, "check") / t.n("changed")) } else { "–".into() }) })
                color="var(--c-good)" />
            {tiles}
        </section>
        <section class="panel">
            {head(
                "Where checking money goes",
                "Spend per session of the graded checks in the lesson-selection summary, by when the check ran. Hover a bar for its calls and the cost per check. (raw: selection spend stop / live)",
                "per session, by when the check ran",
            )}
            {rows}
            <p class="how">{reconcile}</p>
            {history}
        </section>
    }
}

/// D: which lessons to keep, reword or retire.
#[component]
fn Lessons() -> impl IntoView {
    let d = data();
    let sort = RwSignal::new(0u8);
    let limit = RwSignal::new(12usize);
    let verdict = move || {
        d.lessons.with(|ls| {
            let k = ls.iter().filter(|l| l.changed >= 3.0).count();
            let m = ls.iter().filter(|l| l.asked() >= 5.0 && judged_sessions(l) >= RATE_FROM && l.changed / judged_sessions(l) < 0.2).count();
            let m_view = vec![strong(num(m as f64)), plain(" asked 5+ times and acted on in under 20% of judged sessions (reword or narrow them).")];
            vec![strong(num(k as f64)), plain(" lessons acted on in 3+ sessions · "), if m > 0 { tone("warn", m_view) } else { tone("", m_view) }]
        })
    };
    let sorts: [(u8, &str, &str); 4] = [
        (0, "Asked most", "Times delivered as a question, most first."),
        (1, "Acted on most", "Sessions in which the agent acted on it, most first."),
        (
            2,
            "Rarely acted on",
            "Asked often, rarely acted on (lowest acted-on rate first): candidates to reword, narrow their situations, or retire. Asked 5+ times and judged in 5+ sessions.",
        ),
        (3, "Best rate", "Acted on, of judged sessions, highest first; 5+ judged sessions."),
    ];
    let rows = Memo::new(move |_| {
        let mut ls: Vec<Lesson> = d.lessons.get();
        match sort.get() {
            1 => ls.sort_by(|a, b| b.changed.total_cmp(&a.changed)),
            2 => {
                ls.retain(|l| l.asked() >= 5.0 && judged_sessions(l) >= RATE_FROM);
                ls.sort_by(|a, b| (a.changed / judged_sessions(a)).total_cmp(&(b.changed / judged_sessions(b))).then(b.asked().total_cmp(&a.asked())));
            }
            3 => {
                ls.retain(|l| judged_sessions(l) >= RATE_FROM);
                ls.sort_by(|a, b| (b.changed / judged_sessions(b)).total_cmp(&(a.changed / judged_sessions(a))));
            }
            _ => ls.sort_by(|a, b| b.asked().total_cmp(&a.asked())),
        }
        let u = |l: &Lesson, k: &str| l.usage.get(k).copied().unwrap_or(0.0);
        ls.retain(|l| l.asked() + u(l, "queued_free") + u(l, "early") + judged_sessions(l) > 0.0);
        ls
    });
    let table = move || {
        let ls = rows.get();
        if ls.is_empty() {
            return view! { <p class="empty">"No lesson delivered yet."</p> }.into_any();
        }
        let more = ls.len() > limit.get();
        let body = ls
            .into_iter()
            .take(limit.get())
            .map(|l| {
                let id = l.id.clone();
                let u = |k: &str| l.usage.get(k).copied().unwrap_or(0.0);
                let judged = judged_sessions(&l);
                let rate = if judged >= RATE_FROM { pct(l.changed, judged) } else { "–".into() };
                let outcome = vec![
                    ("acted on".to_string(), l.changed, "var(--c-good)".to_string(), String::new()),
                    ("not acted on".into(), l.ignored, "var(--c-slate)".into(), String::new()),
                    ("corrected anyway".into(), l.corrected, "var(--c-heat)".into(), String::new()),
                ];
                let title = format!("{}\nConsequence: {}\nArea: {}", l.question, l.consequence, l.area());
                view! {
                    <tr class="pick" class:few=l.question.starts_with('(') on:click=move |_| d.open.set(Some(id.clone()))>
                        <td title=title>{trunc(&l.question, 90)}</td>
                        <td>{num(l.asked())}</td>
                        <td>{num(u("queued_free") + u("early"))}</td>
                        <td class="outcome"><StackBar parts=outcome compact=true /></td>
                        <td>{rate}</td>
                    </tr>
                }
            })
            .collect_view();
        view! {
            <table class="ltable">
                <thead>
                    <tr>
                        <th>"Lesson"</th>
                        <th title="times delivered as a question">"Asked"</th>
                        <th title="times passed along without asking: side notes and up front">"Passed along"</th>
                        <th title="sessions: acted on (green), not acted on (grey), corrected anyway (red)">"Outcome"</th>
                        <th title="acted on, of judged sessions; from 5 judged sessions on">"Rate"</th>
                    </tr>
                </thead>
                <tbody>{body}</tbody>
            </table>
            {more.then(|| view! { <button class="more" on:click=move |_| limit.update(|n| *n += 12)>"show 12 more"</button> })}
        }
        .into_any()
    };

    view! {
        <SectionHead title="Which lessons matter?">{verdict}</SectionHead>
        <section class="panel">
            {head(
                "Lessons and how they went",
                "Per lesson, team-wide, lifetime. Asked = times delivered as a question. Passed along = side notes and up-front deliveries, without asking. Outcome counts sessions, judged by a model (strongest verdict per session): acted on, not acted on, corrected anyway. Rate = acted on of judged sessions, shown from 5 judged sessions on. (raw: usage.block + usage.live_ask, usage.queued_free + usage.early, changed, ignored, corrected)",
                "click a lesson to open it",
            )}
            <div class="sorts">
                {sorts.into_iter().map(|(k, label, title)| view! {
                    <button class:on=move || sort.get() == k title=title on:click=move |_| { sort.set(k); limit.set(12); }>{label}</button>
                }).collect_view()}
            </div>
            {table}
        </section>
    }
}

/// What decides, as a tile's value and note.
fn decides(s: &Selection) -> (String, String) {
    let default = s.source == s.default;
    let opt = if default { "" } else { "opt-in (BORG_SELECT=learned) · " };
    let (v, note) = match s.source.as_str() {
        "reverted" => ("fixed rule (reverted)", "harm detected; stays on the fixed rule until reset".to_string()),
        "learned" if s.gate > 0.0 => ("learned policy", format!("{opt}may differ from the fixed rule while the safety budget lasts")),
        "learned" if !s.fit.is_empty() && s.allowance > 0.0 => {
            ("learned policy", format!("{opt}safety budget spent: behaves as the fixed rule until it grows"))
        }
        "learned" => ("learned policy", format!("{opt}cold start: behaves as the fixed rule until it has evidence")),
        "fixed" if default => ("fixed rule", "the default · the learned policy is opt-in: BORG_SELECT=learned".into()),
        "fixed" => ("fixed rule", "set by BORG_SELECT=fixed".into()),
        "cap" => ("fixed rule, capped (eval)", "BORG_STOP_ASK_CAP (evaluation only)".into()),
        "attention" => ("attention cut (eval)", "BORG_STOP_ATTENTION=on (evaluation only)".into()),
        _ => ("–", "this server does not say".into()),
    };
    (v.to_string(), note)
}

/// Why a learned decision fell back to the fixed rule (borg outcome.rs `DECIDED_BUSY`,
/// `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD`): a plain label and what it means.
fn fell_back(reason: &str) -> (&'static str, &'static str) {
    match reason {
        "lock_busy" => ("decision lock busy", "another learned check held the store's decision lock past the wait"),
        "lock_failed" => ("decision lock failed", "the store's decision lock could not be taken"),
        "unrecorded" => (
            "not written to the log",
            "its record could not reach the log its safety budget is folded from (learning off, BORG_CONSCIENCE elsewhere, a failed append)",
        ),
        _ => ("other", "a reason this page does not know"),
    }
}

/// A miss cause (borg pools.rs `CAUSES`): its plain label and what it means.
fn miss(cause: &str) -> (&'static str, &'static str) {
    match cause {
        "scope" => ("outside the session's scope", "out of scope for where the session works"),
        "vector_only" => ("found by meaning only", "no word of the work matched it; the vectors found it"),
        "no_word_match" => ("no word matched", "no word of the work matched it"),
        "demoted" => ("pushed down by weight or trust", "its words matched well; its weight or its author's trust put it low"),
        "weak_words" => ("weak word match", "its words matched less than half the pool's"),
        _ => ("shortlist order, ties or newest first", "words and score high; the shortlist, ties or newest first put it low"),
    }
}

/// A model coefficient's plain name (borg select/policy.rs terms).
fn coefficient(name: &str) -> &str {
    match name {
        "intercept" => "baseline (graded 1, passed along)",
        "grade2" => "graded 2 (worth knowing)",
        "grade3" => "graded 3 (must ask)",
        "rank" => "list position",
        "stop" => "at a blocking turn end",
        "three" => "check holds a must-ask",
        "ask" => "asked",
        "not" => "left for later",
        "ask_dilution" => "dilution: questions per check",
        "free_dilution" => "dilution: side notes per check",
        "ask_stop" => "asked × at a blocking turn end",
        "ask_grade3" => "asked × must-ask",
        "not_grade" => "left for later × grade",
        "pool" => "list size",
        "three_stop" => "must-ask in check × blocking turn end",
        other => other,
    }
}

/// An estimate in its unit: signed points, or a spread's standard deviation.
fn est(v: f64, unit: &str) -> String {
    match unit {
        "points" | "points changed" => format!("{v:+.1}"),
        "percent changed" => format!("{v:.1}%"),
        _ => format!("{v:.2}"),
    }
}

fn unit_label(unit: &str) -> String {
    match unit {
        "points" => "percentage points of the chance a delivered lesson changes the work".into(),
        "logit" => "standard deviation, logit scale".into(),
        "coefficient" => "the model's coefficients, logit scale; 0 = no effect".into(),
        "percent changed" => "share of the judged questions that changed the work, by how many its check asked".into(),
        "points changed" => "percentage points of the judged share that changed the work".into(),
        other => other.into(),
    }
}

/// Estimates of one unit as intervals around 0: a dot at the estimate, a line over its interval. Hover a
/// row for its numbers; each row's note (its raw name) is added to its hover text.
#[component]
fn Intervals(#[prop(into)] rows: Signal<Vec<(Estimate, String)>>, #[prop(default = "Not measured yet.")] empty: &'static str) -> impl IntoView {
    // Display: viewBox width, the label column, the right margin, one row's height and the axis below.
    let (w, left, right, row, axis) = (640.0, 300.0, 16.0, 26.0, 22.0);
    move || {
        let rs = rows.get();
        let Some(unit) = rs.first().map(|e| e.0.unit.clone()) else {
            return view! { <p class="empty">{empty}</p> }.into_any();
        };
        let lo = rs.iter().map(|e| e.0.lo).fold(0.0, f64::min);
        let hi = rs.iter().map(|e| e.0.hi).fold(0.0, f64::max);
        let span = (hi - lo).max(1e-9);
        let x = move |v: f64| left + (v - lo) / span * (w - left - right);
        let h = row * rs.len() as f64 + axis;
        let zero = x(0.0);
        let marks = rs
            .iter()
            .enumerate()
            .map(|(i, (e, note))| {
                let y = row * i as f64 + row / 2.0;
                let mut tip = format!("{}: {} [{}, {}] · {} labelled units", e.name, est(e.mean, &unit), est(e.lo, &unit), est(e.hi, &unit), num(e.n));
                if !note.is_empty() {
                    tip = format!("{tip}\n{note}");
                }
                view! {
                    <g>
                        <title>{tip}</title>
                        <text x=format!("{}", left - 8.0) y=format!("{:.1}", y + 4.0) text-anchor="end" class="axis">{trunc(&e.name, 46)}</text>
                        <line x1=format!("{:.1}", x(e.lo)) x2=format!("{:.1}", x(e.hi)) y1=format!("{y:.1}") y2=format!("{y:.1}") style="stroke:var(--c-accent);stroke-width:3" />
                        <circle cx=format!("{:.1}", x(e.mean)) cy=format!("{y:.1}") r="5" style="fill:var(--c-accent)" />
                    </g>
                }
            })
            .collect_view();
        view! {
            <div class="chart">
                <div class="chart-plot">
                    <svg viewBox=format!("0 0 {w} {h}") role="img">
                        <line x1=format!("{zero:.1}") x2=format!("{zero:.1}") y1="0" y2=format!("{:.1}", h - axis) class="grid" />
                        {marks}
                        <text x=format!("{left}") y=format!("{:.1}", h - 6.0) class="axis">{est(lo, &unit)}</text>
                        // The 0 mark only where it does not sit on an end's label.
                        {(lo < 0.0 && hi > 0.0).then(|| view! { <text x=format!("{zero:.1}") y=format!("{:.1}", h - 6.0) text-anchor="middle" class="axis">"0"</text> })}
                        <text x=format!("{}", w - right) y=format!("{:.1}", h - 6.0) text-anchor="end" class="axis">{est(hi, &unit)}</text>
                    </svg>
                </div>
                <p class="empty">{unit_label(&unit)}</p>
            </div>
        }
        .into_any()
    }
}

/// Whether `not` is still its prior: fewer judged placebos than needed.
fn not_assumed(s: &Selection) -> bool {
    s.measurement.as_ref().is_none_or(|m| m.judged < if m.placebos_needed > 0.0 { m.placebos_needed } else { PLACEBOS_NEEDED })
}

/// A dilution term is constrained to be ≤ 0: its interval is clamped there.
fn clamp_dilution(e: &mut Estimate) {
    if e.name.ends_with("_dilution") {
        e.hi = e.hi.min(0.0);
        e.mean = e.mean.min(0.0);
    }
}

/// E and F: how lessons are picked, whether the learned policy is safe, and the author's diagnostics.
#[component]
fn LessonSelection() -> impl IntoView {
    let d = data();
    let sel = d.select;
    let risk = move |s: &Selection| s.test.as_ref().map_or(0.1, |t| t.risk);
    let dev = move |s: &Selection| s.deviations.iter().fold((0.0, 0.0), |(p, w), c| (p + c.part, w + c.whole));
    let verdict = move || {
        let s = sel.get();
        let (who, _) = decides(&s);
        let mut v = vec![strong(who), plain(" decides · ")];
        let ucb = s.evaluation.as_ref().and_then(|e| e.ucb);
        v.push(if s.source == "reverted" {
            tone("warn", vec![plain("reverted to the fixed rule")])
        } else if ucb.is_some_and(|u| u > 0.0) {
            tone("ok", vec![plain("no harm beyond the 10% tolerance detected")])
        } else {
            plain("safety brake not measured yet")
        });
        if s.policy == "learned" {
            let (part, whole) = dev(&s);
            v.push(plain(" · safety budget "));
            v.push(strong(pct(s.gate, s.allowance)));
            v.push(plain(" left · "));
            v.push(strong(format!("{} of {}", num(part), num(whole))));
            v.push(plain(" checks differed from the fixed rule since the last fit"));
        }
        v.push(plain("."));
        v
    };
    let tiles = move || {
        let s = sel.get();
        let (who, note) = decides(&s);
        let learned = s.policy == "learned";
        let who_help = format!(
            "Who chooses, per lesson, between asking, passing along and leaving for later. The fit id ({}) names the outcome-log position the model was fitted from. (raw: source {}, policy {})",
            if s.fit.is_empty() { "none yet" } else { &s.fit },
            s.source,
            s.policy
        );
        let brake = {
            let (v, sub, help) = match &s.evaluation {
                Some(e) => {
                    let v = match e.ucb {
                        _ if s.source == "reverted" => "reverting",
                        Some(u) if u > 0.0 => "in tolerance",
                        Some(_) => "reverting",
                        None => "–",
                    };
                    let sub = match e.ucb {
                        Some(u) => format!("upper bound {u:+.3} {} 0 · {} sessions", if u > 0.0 { ">" } else { "≤" }, num(e.sessions)),
                        None => format!("no bound yet · {} sessions", num(e.sessions)),
                    };
                    let help = format!(
                        "Borg estimates, from the learned policy's own sessions, how its lessons fare against what the fixed rule would have done. The tolerance is losing up to {:.0}% of the fixed rule's quality. The value is the margin over that tolerance, as the chance per lesson that it changed the work: mean {:+.3}, upper bound {}. While the upper bound stays above 0, nothing happens. Below 0, borg reverts to the fixed rule for good. (raw: evaluation mean/ucb, monitor)",
                        risk(&s) * 100.0,
                        e.mean,
                        e.ucb.map_or("–".into(), |u| format!("{u:+.3}"))
                    );
                    (v.to_string(), sub, help)
                }
                None => (
                    "–".into(),
                    "Not measured yet: needs 10+ sessions of the learned policy.".into(),
                    "Borg compares the learned policy's sessions with what the fixed rule would have done, and reverts to the fixed rule if it detects harm. (raw: evaluation, monitor)".into(),
                ),
            };
            view! { <Tile label="Safety brake" value=fixed(v) sub=fixed(sub) help_dyn=fixed(help) color="var(--c-good)" /> }
        };
        let gated = learned.then(|| {
            let (part, whole) = dev(&s);
            let budget_sub = if s.ledger < 0.0 { format!("deviations may have cost up to {:.2}", -s.ledger) } else { format!("deviations so far helped (+{:.2})", s.ledger) };
            let budget_help = format!(
                "Measured in expected mistakes avoided (lessons that change the work). The learned policy may risk up to {:.0}% of what the fixed rule achieves. Allowance {:.2} is that share so far. In the pessimistic case (5% quantile), its deviations used {:.2}, leaving {:.2}. At 0 every decision goes back to the fixed rule until the allowance grows. (raw: gate = allowance + ledger q05)",
                risk(&s) * 100.0,
                s.allowance,
                -s.ledger,
                s.gate
            );
            view! {
                <Tile label="Safety budget left" value=fixed(format!("{:.2} of {:.2}", s.gate, s.allowance)) sub=fixed(budget_sub) help_dyn=fixed(budget_help) color="var(--c-violet)" />
                <Tile label="Different from the fixed rule" value=fixed(pct(part, whole))
                    sub=fixed(if whole > 0.0 { format!("{} of {} checks since the last fit", num(part), num(whole)) } else { "not measured yet".into() })
                    help="Checks since the model was last fitted in which the learned policy routed at least one lesson differently from the fixed rule. This includes the question-count experiment's random draws (below), so it is not all confident decisions. Near 0 means the model has no evidence yet. (raw: deviations / route)"
                    color="var(--c-violet)" />
            }
        });
        view! {
            <Tile label="Who picks the questions" value=fixed(who) sub=fixed(note) help_dyn=fixed(who_help) color="var(--c-violet)" />
            {brake}
            {gated}
        }
    };

    // E2: the coefficients as percentage points for a grade-2 lesson at the reference.
    let effects_pp = Signal::derive(move || {
        sel.with(|s| {
            let get = |n: &str| s.effects.iter().find(|e| e.name == n && e.unit == "coefficient").cloned();
            let (Some(int), Some(g2)) = (get("intercept"), get("grade2")) else { return Vec::new() };
            let b = int.mean + g2.mean;
            let pp = |base: f64, x: f64| 100.0 * (sigmoid(base + x) - sigmoid(base));
            let row = |name: String, base: f64, e: &Estimate, scale: f64, raw: &str| {
                (
                    Estimate { name, unit: "points".into(), mean: pp(base, scale * e.mean), lo: pp(base, scale * e.lo), hi: pp(base, scale * e.hi), n: e.n },
                    format!("(raw: {raw})"),
                )
            };
            let mut out = vec![row("graded 2 (worth knowing) vs 1".into(), int.mean, &g2, 1.0, "grade2")];
            if let Some(g3) = get("grade3") {
                out.push(row("graded 3 (must ask) vs 1".into(), int.mean, &g3, 1.0, "grade3"));
            }
            if let Some(a) = get("ask") {
                out.push(row("asked vs passed along".into(), b, &a, 1.0, "ask"));
            }
            if let Some(n) = get("not") {
                let label = format!("left for later vs passed along{}", if not_assumed(s) { " (assumed)" } else { "" });
                out.push(row(label, b, &n, 1.0, "not"));
            }
            if let Some(mut a) = get("ask_dilution") {
                let assumed = a.mean == 0.0;
                clamp_dilution(&mut a);
                let label = format!("each doubling of questions{}", if assumed { " (assumed)" } else { "" });
                out.push(row(label, b, &a, std::f64::consts::LN_2, "ask_dilution × ln 2"));
            }
            out
        })
    });

    // E3: the question-count experiment.
    let experiment = move || {
        let s = sel.get();
        let Some(t) = s.test.clone() else {
            return view! { <p class="empty">"Not running: the experiment is the learned policy's, on detached checks only."</p> }.into_any();
        };
        let status = if s.policy != "learned" {
            "Not running: the learned policy does not decide here.".to_string()
        } else if t.risk <= 0.0 {
            "Off: the risk ceiling is 0.".to_string()
        } else {
            format!(
                "Running on detached checks · risk ceiling {:.0}% of the fixed rule's quality{}",
                t.risk * 100.0,
                if t.set { " · set by an engineer" } else { "" }
            )
        };
        let changed = (t.ran - t.refused - t.control).max(0.0);
        let draws = vec![
            ("changed the routing".to_string(), changed, "var(--c-violet)".to_string(), "a draw that asked a different number than the fixed rule".to_string()),
            ("same as the fixed rule".into(), t.control, "var(--c-slate)".into(), "the draw happened to match the fixed rule's own count".into()),
            (
                "vetoed by the safety budget".into(),
                t.refused,
                "color-mix(in srgb, var(--c-heat) 60%, transparent)".into(),
                "the safety budget had no room for the draw; the fixed rule decided".into(),
            ),
        ];
        let progress_title = "The result is read only once enough data exist, a rule fixed in advance so that nobody reads noise as an effect.".to_string();
        let progress = view! {
            {bar(format!("draws: {} of {}", num(t.ran), num(t.due_checks)), ratio(t.ran, t.due_checks), pct(t.ran, t.due_checks), progress_title.clone(), "var(--c-accent)")}
            {bar(
                format!("draws: {} sessions of {}", num(t.sessions), num(t.due_min_sessions)),
                ratio(t.sessions, t.due_min_sessions),
                pct(t.sessions, t.due_min_sessions),
                format!("Sessions with a draw: {} of {} needed together with the draws, or {} sessions alone. {progress_title}", num(t.sessions), num(t.due_min_sessions), num(t.due_sessions)),
                "var(--c-accent)",
            )}
        };
        let result = if t.due {
            let mut rows: Vec<(Estimate, String)> = Vec::new();
            if let Some(e) = t.ask_free.clone() {
                rows.push((Estimate { name: "graded 2: asked minus passed along".into(), ..e }, "(raw: ask_free)".into()));
            }
            let dil = t.dilution.clone();
            view! {
                {(!rows.is_empty()).then(|| view! { <Intervals rows=fixed_rows(rows) /> })}
                <Intervals rows=fixed_rows(dil.into_iter().map(|e| { let n = format!("{} questions vs 1: change rate of the first question", e.name); (Estimate { name: n, ..e }, "(raw: dilution)".to_string()) }).collect()) />
                <p class="how">"Above 0 on the first row: asking a grade 2 changes the work more often (ask more). Below 0 on the dilution rows: each extra question lowers the first one's success (ask fewer)."</p>
            }
            .into_any()
        } else {
            view! { <p class="empty">{format!("Result withheld until {} draws in {} sessions, or {} sessions.", num(t.due_checks), num(t.due_min_sessions), num(t.due_sessions))}</p> }.into_any()
        };
        let controls = s.measurement.clone().map(|m| {
            let needed = if m.placebos_needed > 0.0 { m.placebos_needed } else { PLACEBOS_NEEDED };
            let later = s.labels.iter().find(|c| c.name == "placebo").cloned().unwrap_or_default();
            view! {
                <p class="how" title="A placebo is a lesson borg would leave for later anyway: it is withheld and still judged as if delivered. These are the only labels that say what leaving a lesson out costs. Every judged placebo updates the model's starting assumption; with few, it barely moves. (raw: measurement placebos / judged, label placebo)">
                    "Leaving a lesson for later: "<b>{num(m.judged)}</b>" judged placebos so far; until about "<b>{num(needed)}</b>" (a dated decision) its effect is essentially the starting assumption. "
                    {format!("{} placebos drawn = {} of the lessons left for later.", num(m.placebos), pct(later.part, later.whole))}
                </p>
            }
        });
        if t.ran == 0.0 {
            return view! {
                <p class="how">{status}</p>
                <p class="empty">"Waiting: fills when a detached check has grade 1–2 lessons to pass along and the safety budget has room."</p>
                {controls}
            }
            .into_any();
        }
        view! {
            <p class="how">{status}</p>
            <StackBar parts=draws />
            <p class="how">"Vetoed near 100% means the safety budget is spent and the experiment has stalled."</p>
            <h3>"Progress toward the result"</h3>
            {progress}
            {result}
            {controls}
        }
        .into_any()
    };

    // E4: did deliveries arrive?
    let delivery = move || match sel.with(|s| s.delivery.clone()) {
        None => view! { <p class="empty">"Fills after the first sessions with deliveries."</p> }.into_any(),
        Some(r) => [
            ("side note, detached", r.free_live, "a side note from a detached check", "free_live", false),
            ("side note, blocking stop", r.free_stop, "a side note from a blocking turn-end check", "free_stop", false),
            ("question, detached", r.ask_live, "a detached check's question", "ask_live", false),
            ("left for later (info only)", r.later, "a lesson left for later, delivered later in the session", "later", true),
            ("question open at stop (info only)", r.wait, "", "wait", true),
        ]
        .into_iter()
        .map(|(label, rate, what, raw, shown)| {
            let color = if shown { "var(--c-slate)" } else { "var(--c-sky)" };
            let title = if raw == "wait" {
                format!("{} of a detached check's questions were still waiting at the turn end and were asked there in an extra round; shown only, not used by the model (raw: wait)", pct(rate, 1.0))
            } else {
                format!("{}: {} of these reached the agent before the session ended{} (raw: {raw})", what, pct(rate, 1.0), if shown { "; shown only, not used by the model" } else { "" })
            };
            bar(label.into(), rate, pct(rate, 1.0), title, color)
        })
        .collect_view()
        .into_any(),
    };
    let half_life =
        move || sel.with(|s| s.half_life).map_or("not chosen yet".to_string(), |h| if h <= 0.0 { "none".into() } else { format!("{} sessions", num(h)) });

    // E5: where useful lessons sit in the list.
    let pool = move || {
        let s = sel.get();
        if s.pool.is_empty() {
            return view! { <p class="empty">"Fills after the first checks that show the model a list of lessons; nothing to configure."</p> }.into_any();
        }
        let max = s.pool.iter().map(|y| y.rate).fold(0.0, f64::max).max(1e-9);
        let rows = s
            .pool
            .iter()
            .map(|y| {
                let label = if y.from + 1.0 == y.to { format!("position {}", num(y.to)) } else { format!("positions {}–{}", num(y.from + 1.0), num(y.to)) };
                let title = format!("fitted {}; raw {} of {} shown; forced not to rise with position", pct(y.rate, 1.0), num(y.applied), num(y.shown));
                bar(label, y.rate / max, format!("{} · {} applied", pct(y.rate, 1.0), num(y.applied)), title, "var(--c-sky)")
            })
            .collect_view();
        let m = s.measurement.clone().unwrap_or_default();
        let tail_rate = ratio(m.tail_applied, m.tail_shown);
        let tail = (m.tail_shown > 0.0).then(|| {
            bar(
                "past the budget (random sample)".into(),
                tail_rate / max,
                format!("{} · {} applied", pct(m.tail_applied, m.tail_shown), num(m.tail_applied)),
                format!(
                    "lessons the pool budget would have dropped, shown anyway on a random sample of checks: {} of {} applied",
                    num(m.tail_applied),
                    num(m.tail_shown)
                ),
                "var(--c-teal)",
            )
        });
        let applied: f64 = s.pool.iter().map(|y| y.applied).sum();
        let deep: f64 = s.pool.iter().filter(|y| y.from >= 63.0).map(|y| y.applied).sum();
        let last = s.pool.last().map_or(0.0, |y| y.rate);
        let avg = ratio(applied, s.pool.iter().map(|y| y.shown).sum());
        let fact = view! {
            <p class="how">
                <b>{pct(deep, applied)}</b>" of applied lessons came from position 64 or later."
                {(m.tail_shown > 0.0).then(|| view! {
                    " The sample past the budget applies at "<b>{pct(tail_rate, 1.0)}</b>
                    {(tail_rate >= 0.75 * last && last > 0.0).then_some(", about as useful as the last lessons kept")}
                    " (end of the list "<b>{pct(last, 1.0)}</b>"; kept lessons overall "<b>{pct(avg, 1.0)}</b>")."
                })}
            </p>
        };
        view! { {rows}{tail}{fact} }.into_any()
    };
    let misses = move || {
        let Some(m) = sel.with(|s| s.misses.clone()) else {
            return view! { <p class="empty">"Fills after the first detached check logs its list of lessons."</p> }.into_any();
        };
        let few = m.positives < 50.0;
        let causes: BTreeMap<String, f64> = m.causes.iter().cloned().collect();
        let rows = ["scope", "vector_only", "no_word_match", "demoted", "weak_words", "other"]
            .into_iter()
            .map(|c| {
                let n = causes.get(c).copied().unwrap_or(0.0);
                let (label, why) = miss(c);
                bar(label.into(), ratio(n, m.low), num(n), format!("{why}: {} of the {} ranked low (raw: {c})", num(n), num(m.low)), "var(--c-amber)")
            })
            .collect_view();
        view! {
            <p class="how">
                {format!("{} of {} relevant lessons ranked in the bottom half · from {} checks", num(m.low), num(m.positives), num(m.checks))}
                {few.then(|| view! { <span class="few">": too few to act on"</span> })}
            </p>
            {rows}
            <p class="how">"Many 'pushed down' means weight or trust hides good lessons. Many weak or no word matches means retrieval misses them."</p>
        }
        .into_any()
    };

    let measured = move || sel.with(|s| s.checks > 0.0);
    let state = move || {
        let s = sel.get();
        match s.policy.as_str() {
            "" => "This server has no selection view yet: update borg-web.".to_string(),
            _ => "Nothing measured yet: no graded check is folded. Until one is, every decision is the fixed rule's.".to_string(),
        }
    };
    let intro = "Each check grades its lessons. Then a policy decides, per lesson: ask it, pass it along free, or leave it for later. The fixed rule: if any lesson is graded 3, ask every lesson; otherwise pass them all along. The learned policy may decide differently, but only where its model expects a gain and only while a safety budget lasts. It switches back to the fixed rule by itself if it detects harm.";

    view! {
        <SectionHead title="How lessons are picked, and is the learned policy safe?">{verdict}</SectionHead>
        <p class="how">{intro}</p>
        <section class="tiles">{tiles}</section>
        {move || if !measured() {
            view! { <section class="panel"><p class="empty">{state}</p></section> }.into_any()
        } else {
            view! {
                <section class="two">
                    <div class="panel">
                        {head(
                            "What makes a lesson land",
                            "Borg fits a model of how likely a delivered lesson is to change the work. Each row is that model's estimate for one factor, for a grade-2 lesson otherwise at the reference (passed along, first in the list, detached check). An interval crossing 0 means the data cannot tell yet. 'assumed' rows are still the starting assumption, not learned from your sessions.",
                            "how each factor changes the chance a delivered lesson changes the work, in percentage points; 95% intervals",
                        )}
                        <Intervals rows=effects_pp empty="Not fitted yet: borg learn fits it after the first judged sessions." />
                        <p class="how">"Model-based approximation; full coefficients under Diagnostics."</p>
                    </div>
                    <div class="panel">
                        {head(
                            "Question-count experiment",
                            "On a detached check, borg takes the k lessons graded 1–2 it will deliver (the fixed rule asks all of them when the check holds a 3, none otherwise) and picks at random how many, 0 to k, become questions; the rest are passed along. A draw equal to the fixed rule's own count is the control. A draw is vetoed when it would risk more than the safety budget allows. Runs on detached checks only, not on blocking turn-end checks. (raw: test ran / control / refused, k, s)",
                            "borg sometimes asks more or fewer of a check's grade 1–2 lessons, at random, to learn whether asking beats passing along; nothing is withheld",
                        )}
                        {experiment}
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        {head(
                            "Did it arrive?",
                            "A side note reaches the agent only if another prompt or tool call comes. Lower rates make asking relatively more valuable. Recent sessions count more (see the forgetting half-life under Diagnostics). Shown-only rows inform, but are not used by the model. (raw: delivery free_stop / free_live / ask_live / later / wait)",
                            "per path, the share delivered before the session ended (the policy discounts a path by it); the last row is the share of questions still waiting at the turn end",
                        )}
                        {delivery}
                        <p class="how">{move || format!("Recent sessions count more: half-life {}.", half_life())}</p>
                    </div>
                    <div class="panel">
                        {head(
                            "Where useful lessons sit in the list",
                            "Each check shows the model a ranked list of lessons. Per position, the share of shown lessons the model found applying (any grade), fitted so it cannot rise with position. Bars are scaled to the best position. (raw: pool, measurement tail_shown / tail_applied)",
                            "share of shown lessons the model applied, by position in the ranked list",
                        )}
                        {pool}
                        {sub_head(
                            "Why relevant lessons ranked low",
                            "Of the lessons the model found relevant (graded 2–3) for the first time in a session, those in the bottom half of the list (where a smaller list would have dropped them), by cause. (raw: misses low / positives, causes)",
                        )}
                        {misses}
                    </div>
                </section>
                <Diagnostics />
            }.into_any()
        }}
    }
}

fn fixed_rows(rows: Vec<(Estimate, String)>) -> Signal<Vec<(Estimate, String)>> {
    Signal::derive(move || rows.clone())
}

/// Named shares as bars, in the summary's order, each label mapped to plain words.
fn shares(rows: Vec<Count>, label: impl Fn(&str) -> String, color: &'static str) -> impl IntoView {
    if rows.is_empty() {
        return view! { <p class="empty">"Not measured yet."</p> }.into_any();
    }
    rows.into_iter()
        .map(|c| {
            bar(
                label(&c.name),
                ratio(c.part, c.whole),
                format!("{} · {} of {}", pct(c.part, c.whole), num(c.part), num(c.whole)),
                format!("(raw: {})", c.name),
                color,
            )
        })
        .collect_view()
        .into_any()
}

/// F: model internals, label quality, the judge's prompt length, randomisation and fallbacks.
#[component]
fn Diagnostics() -> impl IntoView {
    let d = data();
    let sel = d.select;
    let st = d.stats;
    let coefficients = Signal::derive(move || {
        sel.with(|s| {
            s.effects
                .iter()
                .filter(|e| e.unit == "coefficient")
                .map(|e| {
                    let mut e = e.clone();
                    let prior = (e.name == "not" && not_assumed(s)) || (e.name.ends_with("_dilution") && e.mean == 0.0);
                    clamp_dilution(&mut e);
                    let raw = e.name.clone();
                    let name = format!("{}{}", coefficient(&raw), if prior { " (prior)" } else { "" });
                    (Estimate { name, ..e }, format!("(raw: {raw})"))
                })
                .collect::<Vec<_>>()
        })
    });
    let spreads = move || {
        let parts: Vec<String> = sel.with(|s| {
            s.effects
                .iter()
                .filter(|e| e.unit == "logit")
                .map(|e| {
                    format!(
                        "{} {:.2}",
                        e.name
                            .trim_end_matches(" spread")
                            .replace("session", "between sessions")
                            .replace("lesson", "between lessons")
                            .replace("judge", "between judge models"),
                        e.mean
                    )
                })
                .collect()
        });
        (!parts.is_empty()).then(|| view! { <p class="how">{format!("{} (logit sd)", parts.join(" · "))}</p> })
    };
    let fit_facts = move || {
        let s = sel.get();
        let labels = s.effects.first().map_or(0.0, |e| e.n);
        let hl = match s.half_life {
            None => "not chosen yet".to_string(),
            Some(h) if h <= 0.0 => "none".into(),
            Some(h) => format!("{} sessions", num(h)),
        };
        format!(
            "fit {} · {} labels · forgetting half-life {hl} (chosen automatically from 8/16/32/64/none) · {} graded checks in {} sessions.",
            if s.fit.is_empty() { "none yet" } else { &s.fit },
            num(labels),
            num(s.checks),
            num(s.sessions)
        )
    };
    let coverage = move || {
        let rows: Vec<Count> = sel.with(|s| s.labels.iter().filter(|c| c.name.starts_with("judged, ")).cloned().collect());
        shares(
            rows,
            |n| format!("lessons {}", n.trim_start_matches("judged, ").replace("free", "passed along").replace("placebo", "withheld (control)")),
            "var(--c-good)",
        )
    };
    let answers = move || {
        let rows: Vec<Count> = sel.with(|s| {
            s.labels
                .iter()
                .filter(|c| matches!(c.name.as_str(), "unclear" | "not arisen" | "consequence remains") || c.name.starts_with("unclear, "))
                .cloned()
                .collect()
        });
        shares(
            rows,
            |n| match n {
                "not arisen" => "situation never came up".into(),
                "consequence remains" => "consequence still open".into(),
                other => other.replace("free", "passed along").replace("placebo", "control"),
            },
            "var(--c-violet)",
        )
    };
    let declaration = move || {
        match sel.with(|s| s.declaration.clone()) {
        None => view! { <p class="empty">"Not measured yet."</p> }.into_any(),
        Some(e) => view! {
            <p class="how">
                <b>{format!("{:+.1}", e.mean)}</b>{format!(" pp 'acted on' when the judge sees the agent's note ({:+.1} to {:+.1}), {} questions.", e.lo, e.hi, num(e.n))}
            </p>
            <p class="how">"Near 0 = the judge reads the work, not the claim. Interval treats questions as independent, so it is likely too narrow."</p>
        }
        .into_any(),
    }
    };
    let judge = move || {
        let js = sel.with(|s| s.judge.clone());
        if js.is_empty() {
            return view! { <p class="empty">"No judge runs here."</p> }.into_any();
        }
        js.into_iter()
            .map(|j| {
                let smallest = j.steps.iter().map(|t| t.upto).fold(f64::INFINITY, f64::min);
                let testing = j.explore.map(|up| {
                    let calls = j.steps.iter().find(|t| t.upto == up).map_or(0.0, |t| t.calls);
                    format!(" · testing {} chars: {} of 5 calls", num(up), num(calls))
                });
                let line = format!("{}: budget {} chars ({}){}", j.model, num(j.chars), if j.start { "default" } else { "moved" }, testing.unwrap_or_default());
                let rows = j
                    .steps
                    .iter()
                    .filter(|t| t.sent > 0.0)
                    .map(|t| {
                        let size = if t.upto == smallest { "smallest".to_string() } else { format!("≤ {} chars", num(t.upto)) };
                        let decision = match t.decision.as_str() {
                            "below" => "pooled into baseline".to_string(),
                            "reference" => "baseline".into(),
                            "open" => format!("undecided ({} of 5 calls)", num(t.calls)),
                            "holds" => "holds".into(),
                            "fails" => "too long".into(),
                            other => other.into(),
                        };
                        bar(
                            format!("{size} · {decision}"),
                            ratio(t.answered, t.sent),
                            pct(t.answered, t.sent),
                            format!(
                                "{} of {} answered in {} calls; 95% {}–{} (raw: {})",
                                num(t.answered),
                                num(t.sent),
                                num(t.calls),
                                pct(t.lo, 1.0),
                                pct(t.hi, 1.0),
                                t.decision
                            ),
                            "var(--c-violet)",
                        )
                    })
                    .collect_view();
                view! { <p class="how">{line}</p>{rows} }
            })
            .collect_view()
            .into_any()
    };
    let draws = move || {
        let Some(t) = sel.with(|s| s.test.clone()) else {
            return view! { <p class="empty">"No draws yet."</p> }.into_any();
        };
        let (shown, hidden): (Vec<_>, Vec<_>) =
            t.by_k.into_iter().filter(|d| d.draws.iter().sum::<f64>() > 0.0).partition(|d| d.draws.iter().sum::<f64>() >= 10.0);
        let width = shown.iter().map(|d| d.draws.len()).max().unwrap_or(0);
        let hidden_n: f64 = hidden.iter().map(|d| d.draws.iter().sum::<f64>()).sum();
        view! {
            {(!shown.is_empty()).then(|| view! {
                <table class="ltable">
                    <thead><tr><th title="lessons in the experiment">"k"</th>{(0..width).map(|s| view! { <th title="of them asked">{format!("s={s}")}</th> }).collect_view()}</tr></thead>
                    <tbody>
                        {shown.into_iter().map(|d| {
                            let n: f64 = d.draws.iter().sum();
                            let title = format!("expected {:.1} each if uniform; refused {}", n / (d.k + 1.0), num(d.refused));
                            view! {
                                <tr title=title>
                                    <td>{num(d.k)}</td>
                                    {(0..width).map(|s| view! { <td>{d.draws.get(s).map(|c| num(*c)).unwrap_or_default()}</td> }).collect_view()}
                                </tr>
                            }
                        }).collect_view()}
                    </tbody>
                </table>
            })}
            <p class="how">{format!("{} rows with fewer than 10 draws hidden ({} draws).", hidden.len(), num(hidden_n))}</p>
        }
        .into_any()
    };
    let reliability = move || {
        match sel.with(|s| s.evaluation.clone()) {
        None => view! { <p class="empty">"Not measured yet: needs 10+ sessions of the learned policy."</p> }.into_any(),
        Some(e) => view! {
            <p class="how">{format!("{} labelled lessons · effective sample {} ({}) · largest weight {:.1}× · {} weights cut", num(e.units), num(e.ess), pct(e.ess, e.units), e.max_weight, num(e.truncated))}</p>
            <p class="how">"Trustworthy: effective sample above about 30%, largest weight under about 10×."</p>
        }
        .into_any(),
    }
    };
    let fallbacks = move || {
        let rows = sel.with(|s| s.fallbacks.clone());
        if rows.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        if rows.iter().all(|c| c.part == 0.0) {
            return view! { <p class="how">{format!("None in {} checks.", num(rows[0].whole))}</p> }.into_any();
        }
        rows.into_iter()
            .map(|c| {
                let (label, why) = fell_back(&c.name);
                bar(label.into(), ratio(c.part, c.whole), format!("{} of {}", num(c.part), num(c.whole)), format!("{why} (raw: {})", c.name), "var(--c-amber)")
            })
            .collect_view()
            .into_any()
    };
    let latency = move || {
        let ls = sel.with(|s| s.latency.clone());
        if ls.is_empty() {
            return view! { <p class="empty">"No blocking checks: since 0.7.7 checks run detached (see 'Paid checks')."</p> }.into_any();
        }
        let top = ls.iter().map(|l| l.budget_ms.unwrap_or(l.p95_ms)).fold(0.0, f64::max).max(1.0);
        ls.into_iter()
            .map(|l| {
                let share = l.budget_ms.map_or(l.p95_ms / top, |b| l.p95_ms / b.max(1.0));
                let budget = l.budget_ms.map_or("the harness names no time limit".to_string(), |b| format!("time limit {}", duration(b)));
                let title = format!("{} checks: median {}, 95th percentile {}; {budget}", num(l.checks), duration(l.p50_ms), duration(l.p95_ms));
                let failed = l.over > 0.0;
                view! {
                    <div class="hbar wrap" title=title>
                        <span class="hbar-l">{l.harness.clone()}</span>
                        <span class="hbar-t"><span class="hbar-f" style=format!("width:{:.1}%;background:{}", share.clamp(0.0, 1.0) * 100.0, if failed { "var(--c-heat)" } else { "var(--c-sky)" })></span></span>
                        <span class="hbar-v">{duration(l.p95_ms)}</span>
                    </div>
                    <p class="how">
                        {format!("p50 {} · p95 {} · ", duration(l.p50_ms), duration(l.p95_ms))}
                        <span class:few=!failed style=if failed { "color:var(--c-heat)" } else { "" }><b>{format!("{} of {} failed", num(l.over), num(l.checks))}</b></span>
                    </p>
                }
            })
            .collect_view()
            .into_any()
    };
    let ope = move || {
        let o = st.with(|s| s.ope);
        format!("This machine only: {} usable decisions · {} asks lack a probability · {} lack the candidate list.", o.0, o.1, o.2)
    };
    let pool_log = move || {
        let (m, share) = sel.with(|s| (s.misses.clone(), s.boundary));
        m.map_or("No list of lessons logged yet.".to_string(), |m| {
            let explore =
                if share > 0.0 { format!("exploration {:.0}% of the budget and of the dropped lessons", 100.0 * share) } else { "exploration off".into() };
            format!("{} checks logged · {:.1} MB · {explore}", num(m.checks), m.bytes / 1_048_576.0)
        })
    };

    view! {
        <details class="group diag">
            <summary>"Diagnostics for the author"<span class="group-meta">"Model internals, label quality, the judge's prompt length, randomisation and fallbacks. Not needed to use borg."</span></summary>
            <div class="stack">
                <section class="two">
                    <div class="panel">
                        {head(
                            "Model coefficients (raw)",
                            "The value model's coefficients on the logit scale, with 95% intervals; hover a row for its raw name. '(prior)' rows are still the starting assumption. Dilution terms are constrained to be ≤ 0.",
                            "logit scale; 0 = no effect",
                        )}
                        <Intervals rows=coefficients />
                        {sub_head("Where the variation comes from", "How much the chance of changing the work varies between sessions, lessons and judge models, beyond the factors above (logit standard deviations). Large judge spread means labels depend on which judge ran.")}
                        {spreads}
                        {sub_head("Fit facts", "Which fit is in force and what it was fitted from. (raw: fit, effects n, half_life, checks, sessions)")}
                        <p class="how">{fit_facts}</p>
                    </div>
                    <div class="stack">
                        <div class="panel">
                            {head("Verdict coverage per route", "Share of lessons on each route the judge settled; coverage, not usefulness. Very different coverage biases the route comparison. (raw: labels judged, asked / free / placebo)", "judged share")}
                            {coverage}
                            {sub_head("Judge answers since 0.7.5", "Shares of the judge's answers from judge version 3 (borg 0.7.5) on. (raw: labels unclear, not arisen, consequence remains, unclear per route)")}
                            {answers}
                        </div>
                        <div class="panel">
                            {head("Does the agent's self-report sway the judge?", "A random half of questions is judged without the agent's own note on whether it applied the lesson. The difference in 'acted on' verdicts. (raw: declaration)", "percentage points")}
                            {declaration}
                        </div>
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        {head("How much the session-end judge can read", "Per judge model, the share of lessons it answered (a verdict, not unclear) per prompt size, in ×1.5 steps; the session-end budget is the largest step still at 85% of the baseline's. (raw: judge chars, steps, decision)", "answered share per prompt size")}
                        {judge}
                    </div>
                    <div class="panel">
                        {head("Randomisation check", "Each count of questions 0..k should be drawn about equally often; small samples look uneven by chance. Rows: k lessons in the experiment; columns: s of them asked. (raw: test by_k draws)", "draws per k and s")}
                        {draws}
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        {head("Safety-brake reliability", "How far the safety brake's weighted estimate can be trusted. (raw: evaluation units, ess, max_weight, truncated)", "")}
                        {reliability}
                        {sub_head("Safety fallbacks", "Learned decisions a guard gave to the fixed rule, per reason, of the graded checks. (raw: fallbacks lock_busy / lock_failed / unrecorded)")}
                        {fallbacks}
                    </div>
                    <div class="panel">
                        {head("Blocking turn-end check speed", "Per harness, how long blocking turn-end checks took against the harness's time limit; a failed one ran out of time or erred. (raw: latency p50_ms, p95_ms, budget_ms, over)", "blocking checks only (before 0.7.7 or BORG_ASYNC=off); detached checks' time is in 'Paid checks'")}
                        {latency}
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        {head("Data for testing other selection rules", "Needed to evaluate another selection rule offline (borg debug ope). (raw: ope decisions, no_p, no_pool)", "")}
                        <p class="how">{ope}</p>
                    </div>
                    <div class="panel">
                        {head("Pool log", "The log of the lesson lists checks showed the model, which the miss causes are read from. (raw: misses at, checks; boundary)", "")}
                        <p class="how">{pool_log}</p>
                    </div>
                </section>
            </div>
        </details>
    }
}
