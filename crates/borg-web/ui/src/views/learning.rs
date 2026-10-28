//! Learning: is learning working in this collective? Week by week, backfilled from the logs Borg keeps
//! (borg-web learning.rs): lessons written per source, the share of asked lessons that changed the work,
//! the share of vetoes that repeated an earlier "no", and the playbook versions with their beliefs. Each panel says
//! whether its numbers are the team's (synced logs) or this machine's.

use leptos::prelude::*;
use serde_json::Value;

use crate::api::data;
use crate::charts::{Row, Series, StackedChart, Tile};
use crate::util::{day_num, num, palette, pct, short_day, trunc};

fn nums(v: &Value) -> Vec<f64> {
    v.as_array().into_iter().flatten().map(|x| x.as_f64().unwrap_or(0.0)).collect()
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

/// `/api/learning`, parsed.
#[derive(Clone, Debug, Default, PartialEq)]
struct Learning {
    /// Week labels ("Oct 12"), Mondays; the newest is the current week, "so far" while it runs.
    weeks: Vec<String>,
    /// The index of the week live counting began on this machine (`weeks.len()`: not yet). Before
    /// it, lesson sources, merges and repeated vetoes are unknown (the API's nulls, read as 0 here).
    live: usize,
    /// Its Monday ("Oct 12"), empty when not yet.
    live_from: String,
    /// (source, weekly counts), most first.
    sources: Vec<(String, Vec<f64>)>,
    sources_scope: String,
    team_new: Vec<f64>,
    team_merged: Vec<f64>,
    team_scope: String,
    changed: Vec<f64>,
    ignored: Vec<f64>,
    corrected: Vec<f64>,
    asked_scope: String,
    veto: Vec<f64>,
    repeat: Vec<f64>,
    veto_scope: String,
    /// (playbook shape, version key, belief mean, corrected, vetoed, regretted).
    versions: Vec<(String, String, f64, f64, f64, f64)>,
    playbooks_scope: String,
}

fn parse(v: &Value) -> Learning {
    let mut weeks: Vec<String> = v["weeks"].as_array().into_iter().flatten().filter_map(|w| day_num(w.as_str()?)).map(short_day).collect();
    let live_from = v["live_from"].as_str().and_then(day_num).map(short_day).unwrap_or_default();
    let live = weeks.iter().position(|w| !live_from.is_empty() && *w == live_from).unwrap_or(weeks.len());
    if v["partial_week"] == true
        && let Some(w) = weeks.last_mut()
    {
        w.push_str(" (so far)");
    }
    let l = &v["lessons"];
    let versions = v["playbooks"]["list"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| {
            let shape = s(&p["shape"]);
            p["versions"].as_array().cloned().unwrap_or_default().into_iter().map(move |x| {
                let n = |k: &str| x[k].as_f64().unwrap_or(0.0);
                (shape.clone(), s(&x["key"]), n("mean"), n("corrected"), n("vetoed"), n("regretted"))
            })
        })
        .collect();
    Learning {
        weeks,
        live,
        live_from,
        sources: l["by_source"].as_array().into_iter().flatten().map(|x| (s(&x["source"]), nums(&x["weeks"]))).collect(),
        sources_scope: s(&l["scope"]),
        team_new: nums(&l["team"]["new"]),
        team_merged: nums(&l["team"]["merged"]),
        team_scope: s(&l["team"]["scope"]),
        changed: nums(&v["asked"]["changed"]),
        ignored: nums(&v["asked"]["ignored"]),
        corrected: nums(&v["asked"]["corrected"]),
        asked_scope: s(&v["asked"]["scope"]),
        veto: nums(&v["vetoes"]["veto"]),
        repeat: nums(&v["vetoes"]["repeat"]),
        veto_scope: s(&v["vetoes"]["scope"]),
        versions,
        playbooks_scope: s(&v["playbooks"]["scope"]),
    }
}

/// Sum of the last `n` entries, skipping the newest `skip`.
fn window(v: &[f64], skip: usize, n: usize) -> f64 {
    v.iter().rev().skip(skip).take(n).sum()
}

/// A rate's relative change over the last four weeks against the four before, when both have data.
fn trend(num: &[f64], den: &[f64]) -> Option<f64> {
    let (a, b) = (window(den, 0, 4), window(den, 4, 4));
    if a <= 0.0 || b <= 0.0 {
        return None;
    }
    let (now, before) = (window(num, 0, 4) / a, window(num, 4, 4) / b);
    (before > 0.0).then(|| now / before - 1.0)
}

/// The note on a series history cannot derive (lesson sources, merges, repeated vetoes): from which
/// week it is counted.
fn since(l: &Learning) -> String {
    match (l.live, l.live_from.as_str()) {
        (0, _) => String::new(),
        (_, "") => " · counted once live counting begins".into(),
        (_, w) => format!(" · counted from {w}, unknown before"),
    }
}

/// Where a panel's numbers come from: synced logs (the team's) or this machine's own.
fn scope(sc: String) -> impl IntoView {
    let team = sc == "team";
    let title = if team { "folded from synced logs: every node of the team" } else { "folded from this machine's logs only" };
    view! { <span class=if team { "chip g" } else { "chip" } title=title>{sc}</span> }
}

/// A line of a rate chart: its series and, per week, (part, whole).
type RateLine = (Series, Vec<(f64, f64)>);

/// A share per week (0–100%) as lines; a week without a denominator leaves a gap. Hover a point for
/// its counts.
#[component]
fn RateChart(#[prop(into)] labels: Signal<Vec<String>>, #[prop(into)] lines: Signal<Vec<RateLine>>) -> impl IntoView {
    let (w, h, left, right, top, bottom) = (1000.0, 200.0, 50.0, 12.0, 12.0, 28.0);
    let (pw, ph) = (w - left - right, h - top - bottom);
    let x = move |i: usize, n: usize| left + (i as f64 + 0.5) * pw / n.max(1) as f64;
    let y = move |r: f64| top + ph - r * ph;
    let grid = (0..=4)
        .map(|k| {
            let r = k as f64 / 4.0;
            view! {
                <line x1=format!("{left}") x2=format!("{}", w - right) y1=format!("{:.1}", y(r)) y2=format!("{:.1}", y(r)) class="grid" />
                <text x=format!("{}", left - 8.0) y=format!("{:.1}", y(r) + 4.0) text-anchor="end" class="axis">{format!("{:.0}%", r * 100.0)}</text>
            }
        })
        .collect_view();
    let xlabels = move || {
        let ls = labels.get();
        let n = ls.len();
        let step = n.div_ceil(10).max(1);
        ls.into_iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0)
            .map(|(i, l)| view! { <text x=format!("{:.1}", x(i, n)) y=format!("{}", h - 8.0) text-anchor="middle" class="axis">{l}</text> })
            .collect_view()
    };
    let paths = move || {
        let ls = labels.get();
        let n = ls.len();
        lines
            .get()
            .into_iter()
            .map(|(sr, pts)| {
                let mut d = String::new();
                let mut pen = false;
                let mut dots = Vec::new();
                for (i, (part, whole)) in pts.iter().enumerate() {
                    if *whole <= 0.0 {
                        pen = false;
                        continue;
                    }
                    // A share is at most 100%: the counts of older data were dated by different events.
                    let (px, py) = (x(i, n), y((part / whole).min(1.0)));
                    d.push_str(&format!("{}{px:.1},{py:.1} ", if pen { "L" } else { "M" }));
                    pen = true;
                    let tip = format!("{} · {}: {} ({} of {})", ls.get(i).cloned().unwrap_or_default(), sr.label, pct(*part, *whole), num(*part), num(*whole));
                    dots.push(
                        view! { <circle cx=format!("{px:.1}") cy=format!("{py:.1}") r="4" style=format!("fill:{}", sr.color)><title>{tip}</title></circle> },
                    );
                }
                view! {
                    <path d=d style=format!("fill:none;stroke:{};stroke-width:2.5", sr.color) />
                    {dots}
                }
            })
            .collect_view()
    };
    let legend = move || {
        lines
            .get()
            .into_iter()
            .map(|(sr, _)| view! { <span class="legend-item"><i style=format!("background:{}", sr.color)></i>{sr.label}</span> })
            .collect_view()
    };
    view! {
        <div class="chart">
            <div class="chart-bar"><div class="legend">{legend}</div></div>
            <div class="chart-plot">
                <svg viewBox=format!("0 0 {w} {h}") role="img">{grid}{paths}{xlabels}</svg>
            </div>
            {move || labels.with(Vec::is_empty).then(|| view! { <p class="empty overlay">"No weeks counted yet."</p> })}
        </div>
    }
}

/// Rows of a weekly stacked chart: one value per series per week.
fn rows(weeks: &[String], cols: &[&Vec<f64>]) -> Vec<Row> {
    weeks.iter().enumerate().map(|(i, w)| Row { label: w.clone(), vals: cols.iter().map(|c| c.get(i).copied().unwrap_or(0.0)).collect() }).collect()
}

#[component]
pub fn Learning() -> impl IntoView {
    let d = data();
    let src = d.source("learning");
    let lr = Memo::new(move |_| parse(&src.raw.get()));
    let weeks = Signal::derive(move || lr.with(|l| l.weeks.clone()));

    // Lessons written per source.
    let src_series = Signal::derive(move || lr.with(|l| l.sources.iter().enumerate().map(|(i, (k, _))| Series::new(k, k, palette(i))).collect::<Vec<_>>()));
    let src_rows = Signal::derive(move || lr.with(|l| rows(&l.weeks, &l.sources.iter().map(|x| &x.1).collect::<Vec<_>>())));
    let team_series = vec![Series::new("new", "new lessons", "var(--c-good)"), Series::new("merged", "merged into one", "var(--c-sky)")];
    let team_rows = Signal::derive(move || lr.with(|l| rows(&l.weeks, &[&l.team_new, &l.team_merged])));

    // Asked → changed, and repeated vetoes, as weekly shares. The verdicts on asked lessons: a Stop's
    // changed or ignored, and a later correction anyway, which one lesson can get on top of either
    // (outcome.rs; the Conscience tab's "useful" counts the same way).
    let judged = move |l: &Learning| -> Vec<f64> {
        (0..l.weeks.len()).map(|i| [&l.changed, &l.ignored, &l.corrected].iter().map(|c| c.get(i).copied().unwrap_or(0.0)).sum()).collect()
    };
    let rate_lines = Signal::derive(move || {
        lr.with(|l| {
            let j = judged(l);
            vec![
                (Series::new("changed", "asked → changed the work", "var(--c-good)"), l.changed.iter().copied().zip(j.clone()).collect()),
                (
                    Series::new("repeat", "vetoes that repeated an earlier no", "var(--c-pink)"),
                    // Before live counting began whether a veto repeated is unknown: a gap, never 0%.
                    l.repeat
                        .iter()
                        .copied()
                        .zip(l.veto.clone())
                        .enumerate()
                        .map(|(i, (r, v))| if i < l.live { (0.0, 0.0) } else { (r, v) })
                        .collect::<Vec<_>>(),
                ),
            ]
        })
    });
    let verdict_series = vec![
        Series::new("changed", "changed the work", "var(--c-good)"),
        Series::new("ignored", "ignored", "var(--c-slate)"),
        Series::new("corrected", "corrected anyway", "var(--c-heat)"),
    ];
    let verdict_rows = Signal::derive(move || lr.with(|l| rows(&l.weeks, &[&l.changed, &l.ignored, &l.corrected])));
    let veto_series = vec![Series::new("first", "first no", "var(--c-heat)"), Series::new("repeat", "repeated no", "var(--c-pink)")];
    let veto_rows = Signal::derive(move || {
        lr.with(|l| {
            let first: Vec<f64> = l.veto.iter().zip(l.repeat.iter().chain(std::iter::repeat(&0.0))).map(|(v, r)| (v - r).max(0.0)).collect();
            rows(&l.weeks, &[&first, &l.repeat])
        })
    });

    // Headline tiles: the last four weeks against the four before.
    let last4 = move |v: &[f64]| window(v, 0, 4);
    let tiles = move || {
        let l = lr.get();
        let j = judged(&l);
        let written: f64 = l.team_new.iter().sum();
        let ch = trend(&l.changed, &j);
        let rp = trend(&l.repeat, &l.veto);
        let written_4 = format!("{} in the last 4 weeks · {} merged into one", num(last4(&l.team_new)), num(last4(&l.team_merged)));
        let changed_4 = pct(last4(&l.changed), last4(&j));
        let repeat_4 = pct(last4(&l.repeat), last4(&l.veto));
        let fixed = |t: String| Signal::derive(move || t.clone());
        view! {
            <Tile label="Lessons written" value=fixed(num(written)) sub=fixed(written_4) color="var(--c-good)" />
            <Tile label="Asked → changed" value=fixed(changed_4) sub=fixed("of the verdicts on asked lessons, last 4 weeks".into())
                delta=Signal::derive(move || ch) color="var(--c-good)" />
            <Tile label="Repeated vetoes" value=fixed(repeat_4) sub=fixed("of vetoes, last 4 weeks: should fall".into())
                delta=Signal::derive(move || rp) up_is_good=false color="var(--c-pink)" />
        }
    };

    view! {
        <section class="tiles">{tiles}</section>
        <section class="two">
            <div class="panel">
                <h2>"Lessons written per week"<small>{move || lr.with(|l| format!("by source{} ", since(l)))}</small>{move || scope(lr.with(|l| l.sources_scope.clone()))}</h2>
                <StackedChart rows=src_rows series=src_series width=600.0 />
            </div>
            <div class="panel">
                <h2>"The team's lessons per week"<small>{move || lr.with(|l| format!("written new, or merged into one{} ", since(l)))}</small>{move || scope(lr.with(|l| l.team_scope.clone()))}</h2>
                <StackedChart rows=team_rows series=team_series width=600.0 />
            </div>
        </section>
        <section class="panel">
            <h2>"Is it working?"<small>{move || lr.with(|l| format!(
                "share of the verdicts on asked lessons that were changed the work (should rise; a Stop's verdict and a later correction of one lesson are two), share of vetoes that repeated an earlier no (should fall{}) ",
                since(l)))}</small>
                {move || scope(lr.with(|l| l.asked_scope.clone()))}</h2>
            <RateChart labels=weeks lines=rate_lines />
        </section>
        <section class="two">
            <div class="panel">
                <h2>"Asked lessons' verdicts"<small>"per week; a lesson judged at a Stop can also be corrected later "</small>{move || scope(lr.with(|l| l.asked_scope.clone()))}</h2>
                <StackedChart rows=verdict_rows series=verdict_series width=600.0 />
            </div>
            <div class="panel">
                <h2>"Vetoes"<small>{move || lr.with(|l| format!("per week, a repeat is the same no said again{} ", since(l)))}</small>{move || scope(lr.with(|l| l.veto_scope.clone()))}</h2>
                <StackedChart rows=veto_rows series=veto_series width=600.0 />
            </div>
        </section>
        <section class="panel">
            <h2>"Playbook versions"<small>"each shown version's belief: does following it get the work through without a correction? "</small>{move || scope(lr.with(|l| l.playbooks_scope.clone()))}</h2>
            {move || {
                let vs = lr.with(|l| l.versions.clone());
                if vs.is_empty() {
                    return view! { <p class="empty">"No playbook version shown to a drone yet."</p> }.into_any();
                }
                view! {
                    <table class="mono">
                        <tr><th>"playbook"</th><th>"version"</th><th>"useful"</th><th>"corrected"</th><th>"vetoed"</th><th>"regretted"</th></tr>
                        {vs.into_iter().map(|(shape, key, mean, c, v, r)| view! {
                            <tr><td>{trunc(&shape, 60)}</td><td>{key}</td><td>{pct(mean, 1.0)}</td><td>{num(c)}</td><td>{num(v)}</td><td>{format!("{r:.2}")}</td></tr>
                        }).collect_view()}
                    </table>
                }.into_any()
            }}
        </section>
        {move || src.missing.get().then(|| view! { <p class="hint">"This server has no learning view yet: update borg-web."</p> })}
    }
}
