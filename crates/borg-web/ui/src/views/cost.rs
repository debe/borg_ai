//! Cost: what the model calls cost, per day and purpose, and what a used lesson costs.

use leptos::prelude::*;

use crate::api::data;
use crate::charts::{HBars, Row, Series, StackedChart, Tile};
use crate::util::{kind, palette, short_day, usd, who};
use crate::views::overview::delta;

#[component]
pub fn Cost() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    // Purposes, most expensive first; one "all" series when the server does not split cost.
    let keys = Memo::new(move |_| {
        let mut k: Vec<(String, f64)> = t().usd_by.into_iter().filter(|x| x.1 > 0.0).collect();
        k.sort_by(|a, b| b.1.total_cmp(&a.1));
        if k.is_empty() { vec!["all".to_string()] } else { k.into_iter().map(|x| x.0).collect() }
    });
    let series = Signal::derive(move || {
        keys.get()
            .into_iter()
            .enumerate()
            .map(|(i, k)| {
                let label = if k == "all" { "cost".to_string() } else { kind(&k).0.to_string() };
                Series::new(&k, &label, palette(i))
            })
            .collect::<Vec<_>>()
    });
    let rows = Signal::derive(move || {
        let ks = keys.get();
        st.with(|s| {
            s.days
                .iter()
                .map(|day| Row {
                    label: short_day(day.day),
                    vals: ks.iter().map(|k| if k == "all" { day.t.usd } else { day.t.usd_by.get(k).copied().unwrap_or(0.0) }).collect(),
                })
                .collect::<Vec<_>>()
        })
    });
    let by_purpose = Signal::derive(move || t().usd_by.into_iter().map(|(k, v)| (kind(&k).0.to_string(), v)).filter(|x| x.1 > 0.0).collect::<Vec<_>>());
    let by_engineer = Signal::derive(move || st.with(|s| s.engineers.iter().map(|e| (who(&e.name), e.t.usd)).filter(|x| x.1 > 0.0).collect::<Vec<_>>()));
    let per = move |n: f64| if n > 0.0 { usd(t().usd / n) } else { "–".into() };

    view! {
        <section class="tiles">
            <Tile label="Total" value=Signal::derive(move || usd(t().usd)) color="var(--c-amber)" />
            <Tile label="Last 7 days" value=Signal::derive(move || st.with(|s| usd(s.last(7).usd)))
                delta=Signal::derive(move || st.with(|s| delta(s, |t| t.usd))) up_is_good=false color="var(--c-amber)" />
            <Tile label="Per model check" value=Signal::derive(move || {
                let t = t();
                let c = t.n("check");
                if c > 0.0 { usd(t.usd_by.get("check").copied().unwrap_or(t.usd) / c) } else { "–".into() }
            }) />
            <Tile label="Per used lesson" value=Signal::derive(move || per(t().n("changed"))) sub=Signal::derive(|| "all cost over lessons that changed the work".to_string()) color="var(--c-good)" />
            <Tile label="Per question" value=Signal::derive(move || per(t().n("block_n"))) color="var(--c-violet)" />
        </section>
        <section class="panel">
            <h2>"Cost per day"<small>"by purpose, with the running total"</small></h2>
            <StackedChart rows=rows series=series money=true cumulative=true ranges=true />
        </section>
        <section class="two">
            <div class="panel"><h2>"By purpose"</h2><HBars items=by_purpose color="var(--c-amber)" money=true /></div>
            <div class="panel"><h2>"By engineer"</h2><HBars items=by_engineer color="var(--c-accent)" money=true /></div>
        </section>
    }
}
