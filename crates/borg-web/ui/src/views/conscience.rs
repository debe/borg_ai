//! Conscience: what happens at a turn end, what the questions led to, and which lessons carry the weight.

use leptos::prelude::*;

use crate::api::{Lesson, data};
use crate::charts::{Donut, Row, Series, StackedChart, Tile};
use crate::util::{num, pct, short_day, trunc, usd};

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
                    let (skip, check, block) = (day.t.n("gate_skip"), day.t.n("check"), day.t.n("block"));
                    Row { label: short_day(day.day), vals: vec![skip, (check - block).max(0.0), block] }
                })
                .collect::<Vec<_>>()
        })
    });
    let series = vec![
        Series::new("skip", "skipped free (0 tokens)", "var(--c-slate)"),
        Series::new("quiet", "checked, nothing to ask", "var(--c-accent)"),
        Series::new("block", "extra round with questions", "var(--c-violet)"),
    ];
    let outcomes = Signal::derive(move || {
        let t = t();
        vec![
            ("used".to_string(), t.n("changed"), "var(--c-good)".to_string()),
            ("no correction followed".into(), t.n("not_corrected"), "var(--c-sky)".into()),
            ("ignored".into(), t.n("ignored"), "var(--c-slate)".into()),
            ("corrected anyway".into(), t.n("corrected"), "var(--c-heat)".into()),
        ]
    });
    let useful = Signal::derive(move || {
        let t = t();
        pct(t.n("changed"), t.n("changed") + t.n("ignored") + t.n("corrected"))
    });
    let rank = move |f: fn(&Lesson) -> f64| Signal::derive(move || d.lessons.with(|ls| ls.iter().map(|l| (l.clone(), f(l))).collect::<Vec<_>>()));
    let check_cost = move || {
        let t = t();
        t.usd_by.get("check").copied().unwrap_or(t.usd) / t.n("check").max(1.0)
    };

    view! {
        <section class="tiles">
            <Tile label="Turn ends" value=Signal::derive(move || num(t().n("turn_end"))) sub=Signal::derive(move || format!("{} skipped free", pct(t().n("gate_skip"), t().n("turn_end")))) color="var(--c-muted)" />
            <Tile label="Model checks" value=Signal::derive(move || num(t().n("check"))) sub=Signal::derive(move || format!("{} per check", usd(check_cost()))) />
            <Tile label="Lessons per check" value=Signal::derive(move || num(t().n("check_n") / t().n("check").max(1.0))) sub=Signal::derive(|| "read by the model, one line each".to_string()) />
            <Tile label="Questions per round" value=Signal::derive(move || num(t().n("block_n") / t().n("block").max(1.0))) sub=Signal::derive(move || format!("{} rounds in total", num(t().n("block")))) color="var(--c-violet)" />
            <Tile label="Early, free" value=Signal::derive(move || num(t().n("early"))) sub=Signal::derive(move || format!("{} free hand-overs of grade 2/1", num(t().n("queued_free")))) color="var(--c-teal)" />
        </section>
        <section class="panel">
            <h2>"Every turn end"<small>"most cost nothing; a block costs one extra round"</small></h2>
            <StackedChart rows=rows series=series ranges=true />
        </section>
        <section class="two">
            <div class="panel">
                <h2>"What the questions led to"</h2>
                <Donut parts=outcomes center=useful caption="used when judged" />
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
    }
}
