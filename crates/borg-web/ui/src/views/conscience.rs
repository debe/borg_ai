//! Conscience: what happens at a turn end, what the questions led to, which lessons carry the weight,
//! and how lessons are selected (borg select.rs, `/api/select`): what decides and whether it works.

use leptos::prelude::*;

use crate::api::{Count, Estimate, Lesson, Selection, data};
use crate::charts::{Donut, HBars, Row, Series, StackedChart, Tile};
use crate::util::{duration, num, pct, short_day, trunc, usd};

/// The lessons with the highest `value`, clickable.
#[component]
pub fn LessonRank(#[prop(into)] items: Signal<Vec<(Lesson, f64)>>, unit: &'static str, color: &'static str) -> impl IntoView {
    let d = data();
    move || {
        let mut it = items.get();
        it.retain(|x| x.1 > 0.0);
        it.sort_by(|a, b| b.1.total_cmp(&a.1));
        if it.is_empty() {
            return view! { <p class="empty">"None yet."</p> }.into_any();
        }
        let mx = it[0].1;
        it.into_iter()
            .take(8)
            .map(|(l, v)| {
                let id = l.id.clone();
                view! {
                    <button class="rank" on:click=move |_| d.open.set(Some(id.clone()))>
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
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let rows = Signal::derive(move || {
        st.with(|s| {
            s.days
                .iter()
                .map(|day| {
                    // `check` counts turn-end checks only; live mode's checks during the turn are `live_check`.
                    let (skip, check, block) = (day.t.n("gate_skip"), day.t.n("check"), day.t.n("block"));
                    Row { label: short_day(day.day), vals: vec![skip, (check - block).max(0.0), block, day.t.n("live_check")] }
                })
                .collect::<Vec<_>>()
        })
    });
    let series = vec![
        Series::new("skip", "skipped free (0 tokens)", "var(--c-slate)"),
        Series::new("quiet", "checked, nothing to ask", "var(--c-accent)"),
        Series::new("block", "extra round with questions", "var(--c-violet)"),
        Series::new("live", "live check during the turn (no extra round)", "var(--c-teal)"),
    ];
    // Only verdicts that say whether a question helped: used, ignored or corrected anyway. "No
    // correction followed" says nothing either way, so it is neither a ring segment nor in the
    // denominator: ring, legend shares and centre all count the same questions.
    let outcomes = Signal::derive(move || {
        let t = t();
        vec![
            ("used".to_string(), t.n("changed"), "var(--c-good)".to_string()),
            ("ignored".into(), t.n("ignored"), "var(--c-slate)".into()),
            ("corrected anyway".into(), t.n("corrected"), "var(--c-heat)".into()),
        ]
    });
    let unjudged = move || t().n("not_corrected");
    // The same verdicts by how the question was asked: by a live check during the turn, or at the turn end
    // (results from before delivery modes carry no channel; they were all asked at the turn end).
    let by_channel = move || {
        let t = t();
        let (used, ignored) = (t.via("changed", "live"), t.via("ignored", "live"));
        let (stop_used, stop_ignored) = (t.n("changed") - used, t.n("ignored") - ignored);
        format!(
            "Asked by a live check: {} of {} judged changed the work. Asked at the turn end: {} of {}.",
            num(used),
            num(used + ignored),
            num(stop_used),
            num(stop_used + stop_ignored)
        )
    };
    let useful = Signal::derive(move || {
        let t = t();
        format!("{} of {}", t.n("changed"), t.n("changed") + t.n("ignored") + t.n("corrected"))
    });
    let rank = move |f: fn(&Lesson) -> f64| Signal::derive(move || d.lessons.with(|ls| ls.iter().map(|l| (l.clone(), f(l))).collect::<Vec<_>>()));
    let check_cost = move || {
        let t = t();
        t.usd_by.get("check").copied().unwrap_or(t.usd) / t.n("check").max(1.0)
    };

    view! {
        <section class="tiles">
            <Tile label="Turn ends" value=Signal::derive(move || num(t().n("turn_end"))) sub=Signal::derive(move || format!("{} skipped free", pct(t().n("gate_skip"), t().n("turn_end")))) color="var(--c-muted)" />
            <Tile label="Model checks" value=Signal::derive(move || num(t().n("check"))) sub=Signal::derive(move || format!("{} per check · {} live during the turn", usd(check_cost()), num(t().n("live_check")))) />
            <Tile label="Lessons per check" value=Signal::derive(move || num(t().n("check_n") / t().n("check").max(1.0))) sub=Signal::derive(|| "read by the model, one line each".to_string()) />
            <Tile label="Questions per round" value=Signal::derive(move || num(t().n("block_n") / t().n("block").max(1.0))) sub=Signal::derive(move || format!("{} rounds in total", num(t().n("block")))) color="var(--c-violet)" />
            <Tile label="Early, free" value=Signal::derive(move || num(t().n("early"))) sub=Signal::derive(move || format!("{} free hand-overs of grade 2/1", num(t().n("queued_free")))) color="var(--c-teal)" />
        </section>
        <section class="panel">
            <h2>"Every turn end"<small>"most cost nothing; a block costs one extra round; live mode also checks during the turn"</small></h2>
            <StackedChart rows=rows series=series ranges=true />
        </section>
        <section class="two">
            <div class="panel">
                <h2>"What the questions led to"</h2>
                <Donut parts=outcomes center=useful caption="judged questions changed the work" />
                <p class="empty">{by_channel}</p>
                <p class="empty">{move || format!("{} more asked with no verdict: no correction followed, which says nothing either way.", num(unjudged()))}</p>
                <p class="empty">{move || st.with(|s| format!("Off-policy support (borg debug ope): {} logged decisions with a pool ({} asks without p).", s.ope.0, s.ope.1))}</p>
            </div>
            <div class="panel">
                <h2>"Asked most"</h2>
                <LessonRank items=rank(Lesson::asked) unit="asked" color="var(--c-violet)" />
            </div>
        </section>
        <section class="two">
            <div class="panel">
                <h2>"Changed the work most"</h2>
                <LessonRank items=rank(|l| l.changed) unit="used" color="var(--c-good)" />
            </div>
            <div class="panel">
                <h2>"Corrected anyway"<small>"asked, and the engineer still had to correct"</small></h2>
                <LessonRank items=rank(|l| l.corrected) unit="times" color="var(--c-heat)" />
            </div>
        </section>
        <LessonSelection />
    }
}

/// What decides, as a tile's value and note: the default named as such, a policy opted into by its
/// switch.
fn decides(s: &Selection) -> (String, String) {
    let default = s.source == s.default;
    let opt = if default { "" } else { "opt-in (BORG_SELECT=learned) · " };
    let (v, note) = match s.source.as_str() {
        "reverted" => ("reverted", "the monitor saw harm beyond the gate's share: the fixed rule decides (sticky)".to_string()),
        "learned" if s.gate > 0.0 => ("learned", format!("{opt}deviates on evidence, and tests the question count, where the gate admits it · fit {}", if s.fit.is_empty() { "prior" } else { &s.fit })),
        "learned" => ("learned", format!("{opt}cold start: every decision the fixed rule's until the gate has budget")),
        "fixed" if default => ("fixed rule", "the default · the learned policy is opt-in: BORG_SELECT=learned".into()),
        "fixed" => ("fixed rule", "BORG_SELECT=fixed".into()),
        "cap" => ("fixed rule, capped", "BORG_STOP_ASK_CAP (evaluation only)".into()),
        "attention" => ("attention cut", "BORG_STOP_ATTENTION=on (evaluation only)".into()),
        _ => ("–", "this server does not say".into()),
    };
    (v.to_string(), note)
}

/// What a cost source is: the checks' model calls, which judge their session's pending questions and
/// placebos too (borg top.rs `Selection::spend`).
fn spent_on(source: &str) -> String {
    match source {
        "stop" => "stop checks".into(),
        "live" => "live checks".into(),
        other => other.into(),
    }
}

/// Why a learned decision fell back to the fixed rule, as its record's `reason` names it (borg
/// outcome.rs `DECIDED_BUSY`, `DECIDED_NO_LOCK`, `DECIDED_NO_RECORD`).
fn fell_back_why(reason: &str) -> &'static str {
    match reason {
        "lock_busy" => "another learned check held the store's decision lock past the wait",
        "lock_failed" => "the store's decision lock could not be taken",
        "unrecorded" => "its record could not reach the log its gate is folded from (learning off, BORG_CONSCIENCE elsewhere, a failed append)",
        _ => "a reason this page does not know",
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
        "points" => "percentage points of P(changed and avoids)".into(),
        "logit" => "standard deviation, logit scale".into(),
        "coefficient" => "the value model's coefficients, logit scale".into(),
        "percent changed" => "share of the judged questions that changed the work, by how many its check asked".into(),
        "points changed" => "percentage points of the judged share that changed the work".into(),
        other => other.into(),
    }
}

/// One bar row in `HBars`' style, in the given order: a label, a fill share (0 to 1), its value, a hover title.
fn bar(label: String, share: f64, value: String, title: String, color: &'static str) -> impl IntoView {
    view! {
        <div class="hbar" title=title>
            <span class="hbar-l">{label}</span>
            <span class="hbar-t"><span class="hbar-f" style=format!("width:{:.1}%;background:{color}", share.clamp(0.0, 1.0) * 100.0)></span></span>
            <span class="hbar-v">{value}</span>
        </div>
    }
}

/// Named shares as bars, in the summary's order.
#[component]
fn Shares(#[prop(into)] rows: Signal<Vec<Count>>, color: &'static str) -> impl IntoView {
    move || {
        let rs = rows.get();
        if rs.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        rs.into_iter()
            .map(|c| {
                bar(
                    c.name.clone(),
                    if c.whole > 0.0 { c.part / c.whole } else { 0.0 },
                    pct(c.part, c.whole),
                    format!("{} of {}", num(c.part), num(c.whole)),
                    color,
                )
            })
            .collect_view()
            .into_any()
    }
}

/// Estimates of one unit as intervals around 0: a dot at the estimate, a line over its interval. Hover a
/// row for its numbers.
#[component]
fn Intervals(#[prop(into)] rows: Signal<Vec<Estimate>>) -> impl IntoView {
    // Display: viewBox width, the label column, the right margin, one row's height and the axis below.
    let (w, left, right, row, axis) = (600.0, 200.0, 16.0, 26.0, 22.0);
    move || {
        let rs = rows.get();
        let Some(unit) = rs.first().map(|e| e.unit.clone()) else {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        };
        let lo = rs.iter().map(|e| e.lo).fold(0.0, f64::min);
        let hi = rs.iter().map(|e| e.hi).fold(0.0, f64::max);
        let span = (hi - lo).max(1e-9);
        let x = move |v: f64| left + (v - lo) / span * (w - left - right);
        let h = row * rs.len() as f64 + axis;
        let zero = x(0.0);
        let marks = rs
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let y = row * i as f64 + row / 2.0;
                let tip = format!("{}: {} [{}, {}] · {} labelled units", e.name, est(e.mean, &unit), est(e.lo, &unit), est(e.hi, &unit), num(e.n));
                view! {
                    <g>
                        <title>{tip}</title>
                        <text x=format!("{}", left - 8.0) y=format!("{:.1}", y + 4.0) text-anchor="end" class="axis">{trunc(&e.name, 30)}</text>
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

/// How lessons are selected: what decides and the gate's budget, how often it differs from the fixed
/// rule and how often a guard gave a learned decision to it, what it spends, and whether its effects,
/// labels, estimates, pool and latency hold up. Every number is the store's selection summary as borg
/// reads it; an empty section is not measured yet.
#[component]
fn LessonSelection() -> impl IntoView {
    let d = data();
    let sel = d.select;
    let fixed = |t: String| Signal::derive(move || t.clone());
    let tiles = move || {
        let s = sel.get();
        let (who, note) = decides(&s);
        let gate = if s.policy.is_empty() { "–".to_string() } else { format!("{:.3}", s.gate) };
        let (part, whole) = s.deviations.iter().fold((0.0, 0.0), |(p, w), c| (p + c.part, w + c.whole));
        let deviated = if whole > 0.0 { format!("{} of {} decisions differed from the fixed rule", num(part), num(whole)) } else { "not measured yet".into() };
        let (spent, rounds) = s.spend.iter().fold((0.0, 0.0), |(u, r), x| (u + x.usd, r + x.rounds));
        let per = if s.sessions > 0.0 && !s.spend.is_empty() { usd(spent / s.sessions) } else { "–".into() };
        let rounds_note = if s.sessions > 0.0 {
            format!("{} extra rounds per session · measured, never capped", num(rounds / s.sessions))
        } else {
            "not measured yet".into()
        };
        let budget = s.budget.clone().map(|b| {
            view! {
                <Tile label="Budget" value=fixed(format!("{} pool tokens per session", num(b.tokens))) sub=fixed(format!("{} read per session · price {:.2e} per token", num(b.spent), b.price)) color="var(--c-amber)" />
            }
        });
        // The gate and the deviations are the learned policy's; under the fixed rule, what it measures for it.
        let learned = s.policy == "learned";
        let gated = learned.then(|| {
            view! {
                <Tile label="Gate budget" value=fixed(gate) sub=fixed("quality it may still risk below the fixed rule".into()) color="var(--c-violet)" />
                <Tile label="Deviations" value=fixed(pct(part, whole)) sub=fixed(deviated) color="var(--c-violet)" />
            }
        });
        let placebos = (!learned).then(|| {
            let (v, note) = match &s.measurement {
                Some(m) => (num(m.placebos), format!("placebos drawn, {} judged: the learned policy's not-now labels", num(m.judged))),
                None => ("–".to_string(), "not measured yet".to_string()),
            };
            view! { <Tile label="Placebos" value=fixed(v) sub=fixed(note) color="var(--c-good)" /> }
        });
        view! {
            <Tile label="Lesson selection" value=fixed(who) sub=fixed(note) color="var(--c-violet)" />
            {gated}
            {placebos}
            <Tile label="Spend per session" value=fixed(per) sub=fixed(rounds_note) />
            {budget}
        }
    };
    // One axis per unit: effects in points, spreads as standard deviations.
    let units = Memo::new(move |_| {
        sel.with(|s| {
            let mut us: Vec<String> = Vec::new();
            for e in &s.effects {
                if !us.contains(&e.unit) {
                    us.push(e.unit.clone());
                }
            }
            us
        })
    });
    let effects = move || {
        let us = units.get();
        if us.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        us.into_iter()
            .map(|u| {
                let rows = Signal::derive(move || sel.with(|s| s.effects.iter().filter(|e| e.unit == u).cloned().collect::<Vec<_>>()));
                view! { <Intervals rows=rows /> }
            })
            .collect_view()
            .into_any()
    };
    let spend = Signal::derive(move || sel.with(|s| s.spend.iter().map(|x| (spent_on(&x.source), x.usd / s.sessions.max(1.0))).collect::<Vec<_>>()));
    let half_life = move || match sel.with(|s| s.half_life) {
        None => "Forgetting half-life: not chosen yet.".to_string(),
        Some(h) if h <= 0.0 => "Forgetting half-life: none, every label counts alike.".into(),
        Some(h) => format!("Forgetting half-life: {} session ticks.", num(h)),
    };
    let declaration = move || match sel.with(|s| s.declaration.clone()) {
        None => "Declaration effect: not measured yet.".to_string(),
        Some(e) => format!(
            "Declaration effect: {} [{}, {}] {} on the judge, a random half judged without the apply/skip declaration.",
            est(e.mean, &e.unit),
            est(e.lo, &e.unit),
            est(e.hi, &e.unit),
            unit_label(&e.unit)
        ),
    };
    let evaluation = move || match sel.with(|s| s.evaluation.clone()) {
        None => view! { <p class="empty">"Not measured yet: the monitor reads the learned policy's sessions only."</p> }.into_any(),
        Some(e) => view! {
            <dl class="facts-dl">
                <dt>"monitor"</dt><dd>{match e.ucb {
                    Some(u) => format!("{:+.3}, upper bound {u:+.3} over {} sessions (below 0 reverts)", e.mean, num(e.sessions)),
                    None => format!("{:+.3} over {} sessions, no bound yet", e.mean, num(e.sessions)),
                }}</dd>
                <dt>"labelled units"</dt><dd>{num(e.units)}</dd>
                <dt>"effective sample size"</dt><dd>{format!("{} ({} of the units)", num(e.ess), pct(e.ess, e.units))}</dd>
                <dt>"largest weight"</dt><dd>{format!("{:.1} × the mean", e.max_weight)}</dd>
                <dt>"weights truncated"</dt><dd>{num(e.truncated)}</dd>
            </dl>
        }
        .into_any(),
    };
    let pool = move || {
        let ys = sel.with(|s| s.pool.clone());
        if ys.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        ys.into_iter()
            .map(|y| {
                let label = format!("positions {}–{}", num(y.from + 1.0), num(y.to));
                bar(label, y.rate, pct(y.rate, 1.0), format!("{} applied of {} shown", num(y.applied), num(y.shown)), "var(--c-teal)")
            })
            .collect_view()
            .into_any()
    };
    let latency = move || {
        let ls = sel.with(|s| s.latency.clone());
        if ls.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        let top = ls.iter().map(|l| l.budget_ms.unwrap_or(l.p95_ms)).fold(0.0, f64::max).max(1.0);
        ls.into_iter()
            .map(|l| {
                let (share, against) = match l.budget_ms {
                    Some(b) => (l.p95_ms / b.max(1.0), format!(" of a {} stop budget; {} failed (ran out of time or erred)", duration(b), num(l.over))),
                    None => (l.p95_ms / top, "; the harness names no stop budget".to_string()),
                };
                let title = format!("{} checks: median {}, 95th percentile {}{against}", num(l.checks), duration(l.p50_ms), duration(l.p95_ms));
                let color = if l.over > 0.0 { "var(--c-heat)" } else { "var(--c-sky)" };
                bar(l.harness, share, duration(l.p95_ms), title, color)
            })
            .collect_view()
            .into_any()
    };
    let delivery = move || match sel.with(|s| s.delivery.clone()) {
        None => view! { <p class="empty">"Not fitted yet: borg learn fits them."</p> }.into_any(),
        Some(r) => [
            ("free at the stop", r.free_stop, "a stop's free lesson reached the next prompt"),
            ("free during the turn", r.free_live, "a live check's free lesson was delivered"),
            ("live question delivered", r.ask_live, "a live question of a lesson the session lacked reached the drone, during the turn or asked at the stop"),
            ("left for later", r.later, "a lesson a live check left was delivered later in the session (shown only)"),
            ("waiting at the stop", r.wait, "a live question still waiting was asked at the stop (shown only)"),
        ]
        .into_iter()
        .map(|(label, rate, title)| bar(label.into(), rate, pct(rate, 1.0), title.into(), "var(--c-sky)"))
        .collect_view()
        .into_any(),
    };
    let measurement = move || {
        let Some(m) = sel.with(|s| s.measurement.clone()) else {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        };
        let share = if m.placebos > 0.0 { m.judged / m.placebos } else { 0.0 };
        let title = format!("{} placebos drawn, {} judged changed or ignored", num(m.placebos), num(m.judged));
        let judged = bar("placebos judged".into(), share, format!("{} of {}", num(m.judged), num(m.placebos)), title, "var(--c-good)");
        // The tail sample: lessons the pool budget dropped, shown past it so the yield there is learned.
        let tail = (m.tail_shown > 0.0).then(|| {
            let title = format!("{} lessons a tail sample showed past the pool budget, {} applied", num(m.tail_shown), num(m.tail_applied));
            bar("tail applied".into(), m.tail_applied / m.tail_shown, format!("{} of {}", num(m.tail_applied), num(m.tail_shown)), title, "var(--c-sky)")
        });
        view! {
            {judged}
            {tail}
            <p class="empty">"A placebo is a graded lesson no route delivered, judged blindly where a free delivery would be: the not-now labels the fixed rule's own history cannot give, at no cost to its quality."</p>
        }
        .into_any()
    };
    // The learned decisions the fixed rule took over, per reason, of the graded checks folded: a guard
    // that binds often shows here.
    let fallbacks = move || {
        let rows = sel.with(|s| s.fallbacks.clone());
        if rows.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        rows.into_iter()
            .map(|c| {
                let title = format!("{} of {} graded checks: {}", num(c.part), num(c.whole), fell_back_why(&c.name));
                let share = if c.whole > 0.0 { c.part / c.whole } else { 0.0 };
                bar(c.name.clone(), share, format!("{} of {}", num(c.part), num(c.whole)), title, "var(--c-amber)")
            })
            .collect_view()
            .into_any()
    };
    // The question-count test: the checks it drew a count for, those the gate refused, those the summary
    // still holds once compaction dropped some, and the risk share in force.
    let test_facts = move || {
        let Some(t) = sel.with(|s| s.test.clone()) else {
            return view! { <p class="empty">"Not running: the test is the learned policy's (BORG_SELECT=learned), on live checks only."</p> }.into_any();
        };
        let risk = if t.risk <= 0.0 {
            "0: the test is off".to_string()
        } else {
            format!("{} of the fixed rule's quality{}", pct(t.risk, 1.0), if t.set { ", the collective's risk" } else { ", the default" })
        };
        // Once compaction dropped some, the drawn checks the readout's estimates still read.
        let kept = (t.kept < t.ran).then(|| {
            view! {
                <dt>"of them still in the summary"</dt>
                <dd>{format!("{} (compaction drops the oldest checks, never the counts; the readout reads these)", num(t.kept))}</dd>
            }
        });
        view! {
            <dl class="facts-dl">
                <dt>"risk share in force"</dt><dd>{risk}</dd>
                <dt>"checks drawn"</dt><dd>{format!("{} in {} sessions (played or refused by the gate)", num(t.ran), num(t.sessions))}</dd>
                <dt>"of them controls"</dt><dd>{format!("{} (s the fixed rule's own count)", num(t.control))}</dd>
                <dt>"of them refused by the gate"</dt><dd>{format!("{} (the draw's risk did not fit what the gate may still risk)", num(t.refused))}</dd>
                {kept}
            </dl>
            <p class="empty">"Of the lessons below grade 3 the fixed rule delivers on a live check, s drawn uniformly from 0 to k are asked and the rest handed over: nothing is withheld."</p>
        }
        .into_any()
    };
    // Per k, each s's share of its draws against the uniform 1 / (k + 1) the test draws by.
    let test_draws = move || {
        let ks = sel.with(|s| s.test.as_ref().map(|t| t.by_k.clone()).unwrap_or_default());
        // A refused draw is counted in `draws` too: a k with none drew nothing.
        let ks: Vec<_> = ks.into_iter().filter(|d| d.draws.iter().sum::<f64>() > 0.0).collect();
        if ks.is_empty() {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        }
        ks.into_iter()
            .flat_map(|d| {
                let n: f64 = d.draws.iter().sum();
                let uniform = 1.0 / (d.k + 1.0);
                d.draws
                    .iter()
                    .enumerate()
                    .map(|(s, &c)| {
                        let share = c / n;
                        let title = format!("k = {}: s = {s} drawn {} of {} times (uniform: {}); {} refused at this k", num(d.k), num(c), num(n), pct(uniform, 1.0), num(d.refused));
                        bar(format!("k {} · s {s}", num(d.k)), share, pct(c, n), title, "var(--c-violet)").into_any()
                    })
                    .collect::<Vec<_>>()
            })
            .collect_view()
            .into_any()
    };
    // The readout: when it is due, and once due its estimates with their session-bootstrap intervals.
    let test_readout = move || {
        let Some(t) = sel.with(|s| s.test.clone()) else {
            return view! { <p class="empty">"Not measured yet."</p> }.into_any();
        };
        if !t.due {
            return view! {
                <p class="empty">{format!(
                    "Due after {} checks drawn or {} sessions with one: {} and {} so far.",
                    num(t.due_checks), num(t.due_sessions), num(t.ran), num(t.sessions)
                )}</p>
            }
            .into_any();
        }
        let ask_free = match t.ask_free.clone() {
            None => "Grade 2 asked against handed over: no judged 2 on both routes yet.".to_string(),
            Some(e) => format!(
                "Grade 2 asked against handed over: {} [{}, {}] {} ({} 2s).",
                est(e.mean, &e.unit),
                est(e.lo, &e.unit),
                est(e.hi, &e.unit),
                unit_label(&e.unit),
                num(e.n)
            ),
        };
        let dilution = Signal::derive(move || sel.with(|s| s.test.as_ref().map(|t| t.dilution.clone()).unwrap_or_default()));
        view! {
            <Intervals rows=dilution />
            <p class="empty">{ask_free}</p>
        }
        .into_any()
    };
    let measured = move || sel.with(|s| s.checks > 0.0);
    let state = move || {
        let s = sel.get();
        match s.policy.as_str() {
            "" => "This server has no selection view yet: update borg-web.".to_string(),
            _ => format!(
                "Nothing measured yet: the selection summary has folded no graded check{}. Until it does, every decision is the fixed rule's.",
                if s.policy == s.default { "" } else { " (and a switch decides)" }
            ),
        }
    };

    view! {
        <section class="tiles">{tiles}</section>
        {move || if !measured() {
            view! {
                <section class="panel">
                    <h2>"Lesson selection"<small>"which graded lessons are asked, handed over free or left for later"</small></h2>
                    <p class="empty">{state}</p>
                </section>
            }.into_any()
        } else {
            view! {
                <section class="two">
                    <div class="panel">
                        <h2>"Effects"<small>"with intervals: grade × route, ask vs free, free vs placebo, dilution, the spread between sessions and between lessons"</small></h2>
                        {effects}
                        <p class="empty">{half_life}</p>
                    </div>
                    <div class="panel">
                        <h2>"Spend per session"<small>"per cost source; measured, priced only under a budget an engineer set"</small></h2>
                        <HBars items=spend money=true />
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        <h2>"Deviations from the fixed rule"<small>"per decision type, under the current fit: on evidence and by the question-count test's draws"</small></h2>
                        {move || if sel.with(|s| s.policy == "learned") {
                            view! { <Shares rows=Signal::derive(move || sel.with(|s| s.deviations.clone())) color="var(--c-violet)" /> }.into_any()
                        } else {
                            view! { <p class="empty">"None: the learned policy does not decide here, so nothing deviates from the fixed rule."</p> }.into_any()
                        }}
                        <h2>"Fell back to the fixed rule"<small>"learned decisions a guard gave the fixed rule, per reason, of the graded checks"</small></h2>
                        {fallbacks}
                    </div>
                    <div class="panel">
                        <h2>"Label health"<small>"judged share per route, unclear share, placebo rate"</small></h2>
                        <Shares rows=Signal::derive(move || sel.with(|s| s.labels.clone())) color="var(--c-good)" />
                        <p class="empty">{declaration}</p>
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        <h2>"Question-count test"<small>"how many lessons a live check asks, randomised, rationed by the gate"</small></h2>
                        {test_facts}
                        <h2>"Readout"<small>"dilution and asking against handing over, session-bootstrap 95% intervals"</small></h2>
                        {test_readout}
                    </div>
                    <div class="panel">
                        <h2>"Draws of s"<small>"per k, each count's share of its checks; uniform by design"</small></h2>
                        {test_draws}
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        <h2>"Measured for the learned policy"<small>"under any policy, the fixed default too"</small></h2>
                        {measurement}
                    </div>
                    <div class="panel">
                        <h2>"Delivery rates"<small>"what the fit learned a route's value from"</small></h2>
                        {delivery}
                    </div>
                </section>
                <section class="two">
                    <div class="panel">
                        <h2>"Pool yield by position"<small>"applied of shown at each fill position, fitted to fall with position"</small></h2>
                        {pool}
                    </div>
                    <div class="panel">
                        <h2>"Latency"<small>"95th percentile of a check against the harness's stop budget"</small></h2>
                        {latency}
                        <h2>"Evaluation health"<small>"of the monitor's weighted estimate"</small></h2>
                        {evaluation}
                    </div>
                </section>
            }.into_any()
        }}
    }
}
