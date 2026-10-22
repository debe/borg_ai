//! Overview: the headline numbers, activity over time, the conscience funnel, facts worth knowing,
//! and the live feed.

use std::collections::BTreeMap;

use leptos::prelude::*;

use crate::api::{Stats, data};
use crate::charts::{Funnel, Row, Series, StackedChart, Tile};
use crate::feed::FeedList;
use crate::util::{DAY_MS, kind, long_day, now_ms, num, pct, short_day, trunc, usd, who};

/// Change of the last 7 days against the 7 before, or None without a base.
pub fn delta(s: &Stats, f: impl Fn(&crate::api::Totals) -> f64) -> Option<f64> {
    let (now, before) = (f(&s.last(7)), f(&s.before_last(7)));
    (before > 0.0).then(|| (now - before) / before)
}

/// Rows per day of the given kinds' counts.
pub fn day_rows(s: &Stats, kinds: &[&str]) -> Vec<Row> {
    s.days.iter().map(|d| Row { label: short_day(d.day), vals: kinds.iter().map(|k| d.t.n(k)).collect() }).collect()
}

pub fn kind_series(kinds: &[&str]) -> Vec<Series> {
    kinds
        .iter()
        .map(|k| {
            let (label, color, _) = kind(k);
            Series::new(k, label, color)
        })
        .collect()
}

const ACTIVITY: [&str; 13] = [
    "correction",
    "veto",
    "review_finding",
    "lesson_new",
    "block",
    "changed",
    "ignored",
    "corrected",
    "early",
    "steer_issued",
    "pause_refused",
    "queen_answered",
    "assimilated",
];

#[derive(Clone, PartialEq)]
struct Fact {
    big: String,
    text: String,
    lesson: Option<String>,
    tone: &'static str,
}

#[component]
pub fn Overview() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let spark = move |k: &'static str| Signal::derive(move || st.with(|s| s.series(k, 30)));
    let rejected_week = Memo::new(move |_| {
        let since = now_ms() - 7.0 * DAY_MS;
        d.feed.with(|f| f.iter().filter(|e| e.kind == "correction" && e.why == "rejected_tool" && e.ms >= since).count())
    });

    let facts = Memo::new(move |_| {
        let s = st.get();
        let ls = d.lessons.get();
        let tt = &s.totals;
        let mut out = Vec::new();
        let (turns, skips, checks) = (tt.n("turn_end"), tt.n("gate_skip"), tt.n("check"));
        if turns > 0.0 {
            let per = tt.usd_by.get("check").copied().unwrap_or(tt.usd) / checks.max(1.0);
            out.push(Fact {
                big: pct(skips, turns),
                text: format!("of {} turn ends cost nothing: the free gate skipped the model, saving about {}.", num(turns), usd(skips * per)),
                lesson: None,
                tone: "a",
            });
        }
        if let Some(l) = ls.iter().filter(|l| l.changed > 0.0).max_by(|a, b| a.changed.total_cmp(&b.changed)) {
            out.push(Fact {
                big: format!("{}× used", num(l.changed)),
                text: format!("Most useful lesson: “{}”", trunc(&l.question, 110)),
                lesson: Some(l.id.clone()),
                tone: "g",
            });
        }
        let corrected: Vec<_> = ls.iter().filter(|l| l.corrected > 0.0).collect();
        if !corrected.is_empty() {
            let top = corrected.iter().max_by(|a, b| a.corrected.total_cmp(&b.corrected)).unwrap();
            out.push(Fact {
                big: num(corrected.len() as f64),
                text: format!("lessons were asked and the drone was corrected anyway: candidates to sharpen. Worst: “{}”", trunc(&top.question, 80)),
                lesson: Some(top.id.clone()),
                tone: "h",
            });
        }
        if let Some(l) = ls.iter().filter(|l| l.ignored > 0.0).max_by(|a, b| a.ignored.total_cmp(&b.ignored)) {
            out.push(Fact {
                big: format!("{}× ignored", num(l.ignored)),
                text: format!("Most ignored: “{}”", trunc(&l.question, 110)),
                lesson: Some(l.id.clone()),
                tone: "m",
            });
        }
        let corrections = tt.n("correction");
        if corrections > 0.0 {
            let week = rejected_week.get();
            out.push(Fact {
                big: num(corrections),
                text: format!("corrections captured, {} of them rejected tool calls in the last 7 days.", week),
                lesson: None,
                tone: "h",
            });
        }
        let vetoes = tt.n("veto");
        if vetoes > 0.0 {
            out.push(Fact {
                big: pct(tt.n("veto_repeat"), vetoes),
                text: format!("of {} vetoes repeated an earlier one (the same reason said again). The goal is zero.", num(vetoes)),
                lesson: None,
                tone: "h",
            });
        }
        let judged = tt.n("task_verified") + tt.n("task_rejected") + tt.n("task_hacked");
        if judged > 0.0 {
            out.push(Fact {
                big: pct(tt.n("task_hacked"), judged),
                text: format!("of {} judged hive tasks were hacked: they changed protected tests or CI, or touched the oracle. The goal is zero.", num(judged)),
                lesson: None,
                tone: "h",
            });
        }
        if let Some(e) = s.engineers.iter().max_by(|a, b| a.t.n("turn_end").total_cmp(&b.t.n("turn_end"))).filter(|e| e.t.n("turn_end") > 0.0) {
            out.push(Fact {
                big: who(&e.name),
                text: format!(
                    "ran the most drone turns: {} turn ends, {} questions asked.",
                    num(e.t.n("turn_end")),
                    num(e.t.n("block_n") + e.t.n("live_ask_n"))
                ),
                lesson: None,
                tone: "a",
            });
        }
        let mut systems: BTreeMap<String, (f64, f64)> = BTreeMap::new();
        for l in &ls {
            let e = systems.entry(l.system()).or_default();
            e.0 += 1.0;
            e.1 += l.activation;
        }
        if let Some((name, (n, _))) =
            systems.iter().filter(|(k, v)| v.0 >= 3.0 && k.as_str() != "general").max_by(|a, b| (a.1.1 / a.1.0).total_cmp(&(b.1.1 / b.1.0)))
        {
            out.push(Fact {
                big: name.clone(),
                text: format!(
                    "has the most active lessons on average ({} lessons, used {}×).",
                    num(*n),
                    num(ls.iter().filter(|l| &l.system() == name).map(|l| l.changed).sum())
                ),
                lesson: None,
                tone: "v",
            });
        }
        let month = now_ms() - 30.0 * DAY_MS;
        let fresh = ls.iter().filter(|l| l.origin_ms >= month).count();
        let dormant = ls.iter().filter(|l| l.dormant).count();
        if !ls.is_empty() {
            out.push(Fact {
                big: format!("+{fresh}"),
                text: format!("lessons in the last 30 days; {dormant} of {} are dormant, waiting for a strong match.", ls.len()),
                lesson: None,
                tone: "g",
            });
        }
        if let Some(day) = s.days.iter().filter(|d| d.t.usd > 0.0).max_by(|a, b| a.t.usd.total_cmp(&b.t.usd)) {
            out.push(Fact { big: usd(day.t.usd), text: format!("was the most expensive day ({}).", long_day(day.day)), lesson: None, tone: "m" });
        }
        out
    });

    // Questions at a stop block (each block an extra round) plus questions a live check asked during the
    // turn (no extra round): both count as asked, as their verdicts count as used or ignored.
    let questions = |t: &crate::api::Totals| {
        let live = t.n("live_ask_n");
        if live > 0.0 { format!("{} questions, {} more live", num(t.n("block_n")), num(live)) } else { format!("{} questions", num(t.n("block_n"))) }
    };
    let funnel = Signal::derive(move || {
        let t = t();
        vec![
            ("turn ends".to_string(), t.n("turn_end"), "var(--c-muted)".to_string(), String::new()),
            ("model checks".into(), t.n("check"), "var(--c-accent)".into(), format!("{} skipped free", num(t.n("gate_skip")))),
            ("extra rounds".into(), t.n("block"), "var(--c-violet)".into(), questions(&t)),
            ("used".into(), t.n("changed"), "var(--c-good)".into(), format!("{} ignored · {} corrected anyway", num(t.n("ignored")), num(t.n("corrected")))),
        ]
    });

    view! {
        <section class="tiles">
            <Tile label="Turn ends" value=Signal::derive(move || num(t().n("turn_end")))
                sub=Signal::derive(move || format!("{} skipped free", pct(t().n("gate_skip"), t().n("turn_end"))))
                spark=spark("turn_end") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("turn_end")))) color="var(--c-muted)" />
            <Tile label="Questions asked" value=Signal::derive(move || num(t().n("block_n") + t().n("live_ask_n")))
                sub=Signal::derive(move || {
                    let live = t().n("live_ask_n");
                    if live > 0.0 {
                        format!("in {} extra rounds; {} live, without one", num(t().n("block")), num(live))
                    } else {
                        format!("in {} extra rounds", num(t().n("block")))
                    }
                })
                spark=spark("block") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("block_n") + t.n("live_ask_n")))) color="var(--c-violet)" />
            <Tile label="Lessons used" value=Signal::derive(move || num(t().n("changed")))
                sub=Signal::derive(move || format!("{} ignored · {} corrected anyway", num(t().n("ignored")), num(t().n("corrected"))))
                spark=spark("changed") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("changed")))) color="var(--c-good)" />
            <Tile label="Corrections" value=Signal::derive(move || num(t().n("correction")))
                sub=Signal::derive(move || {
                    let review = t().n("review_finding");
                    if review > 0.0 {
                        format!("{} rejected tool calls this week · {} review findings fixed before a commit", rejected_week.get(), num(review))
                    } else {
                        format!("{} rejected tool calls this week", rejected_week.get())
                    }
                })
                spark=spark("correction") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("correction")))) color="var(--c-heat)" up_is_good=false />
            <Tile label="Vetoes" value=Signal::derive(move || num(t().n("veto")))
                sub=Signal::derive(move || format!("{} repeated", num(t().n("veto_repeat"))))
                spark=spark("veto") delta=Signal::derive(move || st.with(|s| delta(s, |t| t.n("veto")))) color="var(--c-heat)" up_is_good=false />
            <Tile label="Knowledge" value=Signal::derive(move || num(d.lessons.with(Vec::len) as f64))
                sub=Signal::derive(move || {
                    let month = now_ms() - 30.0 * DAY_MS;
                    d.lessons.with(|l| format!("+{} in 30 days · {} dormant", l.iter().filter(|x| x.origin_ms >= month).count(), l.iter().filter(|x| x.dormant).count()))
                })
                spark=spark("lesson_new") color="var(--c-good)" />
            <Tile label="Steering" value=Signal::derive(move || num(t().n("steer_issued")))
                sub=Signal::derive(move || format!("{} paused tool calls · {} refused", num(t().n("pause_refused")), num(t().n("steer_dropped"))))
                spark=spark("steer_issued") color="var(--c-amber)" />
            <Tile label="Queen" value=Signal::derive(move || num(d.queen.with(|q| q.iter().filter(|e| e.state() == "open").count()) as f64))
                sub=Signal::derive(move || format!("open · {} answered", d.queen.with(|q| q.iter().filter(|e| e.state() == "answered").count())))
                color="var(--c-violet)" />
            <Tile label="Cost" value=Signal::derive(move || usd(t().usd))
                sub=Signal::derive(move || format!("{} in the last 7 days", st.with(|s| usd(s.last(7).usd))))
                spark=Signal::derive(move || st.with(|s| s.days.iter().rev().take(30).rev().map(|d| d.t.usd).collect::<Vec<_>>()))
                delta=Signal::derive(move || st.with(|s| delta(s, |t| t.usd))) color="var(--c-amber)" up_is_good=false />
        </section>

        <section class="panel">
            <h2>"Activity"<small>"what the collective did, per day"</small></h2>
            <StackedChart rows=Signal::derive(move || st.with(|s| day_rows(s, &ACTIVITY))) series=kind_series(&ACTIVITY) ranges=true />
        </section>

        <section class="two">
            <div class="panel">
                <h2>"The conscience at work"<small>"from a finished turn to a changed decision"</small></h2>
                <Funnel steps=funnel />
            </div>
            <div class="panel">
                <h2>"Live"<small>"newest first"</small></h2>
                <div class="feed compact"><FeedList events=d.feed limit=7 /></div>
                <a class="more" href="#steering">"All corrections and steering →"</a>
            </div>
        </section>

        <section>
            <h2>"Worth knowing"</h2>
            <div class="facts">
                {move || {
                    let fs = facts.get();
                    if fs.is_empty() {
                        return view! { <p class="empty">"Facts appear once the collective has done some work."</p> }.into_any();
                    }
                    fs.into_iter().map(|f| {
                        let id = f.lesson.clone();
                        view! {
                            <button class=format!("fact {}", f.tone) class:pick=f.lesson.is_some()
                                on:click=move |_| if let Some(i) = id.clone() { d.open.set(Some(i)) }>
                                <span class="fact-big">{f.big}</span>
                                <span class="fact-text">{f.text}</span>
                            </button>
                        }
                    }).collect_view().into_any()
                }}
            </div>
        </section>
    }
}
