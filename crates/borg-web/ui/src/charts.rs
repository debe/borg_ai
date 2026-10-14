//! Charts drawn as SVG by Rust: stacked bars over time with hover and legend toggles, sparklines,
//! a funnel, a donut and horizontal bars. No JavaScript chart library.

use leptos::prelude::*;

use crate::util::{nice_max, num, pct, usd};

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub key: String,
    pub label: String,
    pub color: String,
}

impl Series {
    pub fn new(key: &str, label: &str, color: &str) -> Self {
        Series { key: key.into(), label: label.into(), color: color.into() }
    }
}

/// One x position: its label and a value per series (in series order).
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub label: String,
    pub vals: Vec<f64>,
}

/// Stacked bars over time. Hover a column for its values; click a legend entry to hide a series;
/// `ranges` adds a 7/30/90/all selector; `cumulative` draws the running total on its own scale.
#[component]
pub fn StackedChart(
    #[prop(into)] rows: Signal<Vec<Row>>,
    #[prop(into)] series: Signal<Vec<Series>>,
    #[prop(optional)] money: bool,
    #[prop(optional)] cumulative: bool,
    #[prop(optional)] ranges: bool,
    #[prop(default = 240.0)] height: f64,
    /// viewBox width: smaller for narrow panels, so labels stay readable.
    #[prop(default = 1000.0)]
    width: f64,
) -> impl IntoView {
    let cw = width;
    let hidden = RwSignal::new(Vec::<String>::new());
    let range = RwSignal::new(if ranges { 30usize } else { 0 });
    let hover = RwSignal::new(None::<usize>);
    let (left, right, top, bottom) = (50.0, if cumulative { 58.0 } else { 12.0 }, 12.0, 28.0);
    let plot_h = height - top - bottom;
    let plot_w = cw - left - right;
    let fmt = move |v: f64| if money { usd(v) } else { num(v) };

    let shown = Memo::new(move |_| {
        let r = rows.get();
        let n = range.get();
        if n == 0 || r.len() <= n { r } else { r[r.len() - n..].to_vec() }
    });
    // Which series are on, as a mask over series order.
    let mask = Memo::new(move |_| {
        let h = hidden.get();
        series.get().iter().map(|s| !h.contains(&s.key)).collect::<Vec<bool>>()
    });
    let stack = move |r: &Row, m: &[bool]| r.vals.iter().zip(m).filter(|(_, on)| **on).map(|(v, _)| *v).sum::<f64>();
    let max = Memo::new(move |_| {
        let m = mask.get();
        nice_max(shown.with(|rs| rs.iter().map(|r| stack(r, &m)).fold(0.0, f64::max)))
    });
    let band = move || cw.min(plot_w / shown.with(Vec::len).max(1) as f64);

    let bars = move || {
        let rs = shown.get();
        let m = mask.get();
        let colors: Vec<String> = series.get().iter().map(|s| s.color.clone()).collect();
        let (bw, mx) = (band(), max.get());
        let mut rects = Vec::new();
        for (i, r) in rs.iter().enumerate() {
            let x = left + i as f64 * bw + bw * 0.14;
            let w = (bw * 0.72).max(1.0);
            let mut y = top + plot_h;
            for (j, v) in r.vals.iter().enumerate() {
                if !m.get(j).copied().unwrap_or(false) || *v <= 0.0 {
                    continue;
                }
                let h = (v / mx * plot_h).max(0.8);
                y -= h;
                rects.push((x, y, w, h, colors.get(j).cloned().unwrap_or_default()));
            }
        }
        rects
            .into_iter()
            .map(|(x, y, w, h, c)| {
                view! { <rect x=format!("{x:.1}") y=format!("{y:.1}") width=format!("{w:.1}") height=format!("{h:.1}") rx="1.5" style=format!("fill:{c}") /> }
            })
            .collect_view()
    };

    let grid = move || {
        let mx = max.get();
        (0..=4)
            .map(|k| {
                let v = mx * k as f64 / 4.0;
                let y = top + plot_h - plot_h * k as f64 / 4.0;
                view! {
                    <line x1=format!("{left}") x2=format!("{}", cw - right) y1=format!("{y:.1}") y2=format!("{y:.1}") class="grid" />
                    <text x=format!("{}", left - 8.0) y=format!("{:.1}", y + 4.0) text-anchor="end" class="axis">{fmt(v)}</text>
                }
            })
            .collect_view()
    };

    let xlabels = move || {
        let rs = shown.get();
        let n = rs.len();
        let step = n.div_ceil(8).max(1);
        let bw = band();
        rs.into_iter()
            .enumerate()
            .filter(|(i, _)| i % step == 0)
            .map(|(i, r)| {
                let x = left + (i as f64 + 0.5) * bw;
                view! { <text x=format!("{x:.1}") y=format!("{}", height - 8.0) text-anchor="middle" class="axis">{r.label}</text> }
            })
            .collect_view()
    };

    let line = move || {
        if !cumulative {
            return None;
        }
        let rs = shown.get();
        let m = mask.get();
        let mut run = 0.0;
        let pts: Vec<f64> = rs
            .iter()
            .map(|r| {
                run += stack(r, &m);
                run
            })
            .collect();
        let total = pts.last().copied().unwrap_or(0.0);
        let top_v = nice_max(total);
        let bw = band();
        let path: String = pts
            .iter()
            .enumerate()
            .map(|(i, v)| format!("{}{:.1},{:.1}", if i == 0 { "M" } else { " L" }, left + (i as f64 + 0.5) * bw, top + plot_h - v / top_v * plot_h))
            .collect();
        Some(view! {
            <path d=path class="cumline" />
            <text x=format!("{}", cw - right + 6.0) y=format!("{:.1}", top + plot_h - total / top_v * plot_h - 4.0) class="axis strong">{fmt(total)}</text>
            <text x=format!("{}", cw - right + 6.0) y=format!("{:.1}", top + plot_h - total / top_v * plot_h + 10.0) class="axis">"total"</text>
        })
    };

    let hit = move || {
        let n = shown.with(Vec::len);
        let bw = band();
        (0..n)
            .map(|i| {
                let x = left + i as f64 * bw;
                view! {
                    <rect x=format!("{x:.1}") y=format!("{top}") width=format!("{bw:.1}") height=format!("{plot_h}") class="hit"
                        on:mouseenter=move |_| hover.set(Some(i)) />
                }
            })
            .collect_view()
    };

    let focus = move || {
        hover.get().map(|i| {
            let bw = band();
            let x = left + i as f64 * bw;
            view! { <rect x=format!("{x:.1}") y=format!("{top}") width=format!("{bw:.1}") height=format!("{plot_h}") class="focus" /> }
        })
    };

    let tip = move || {
        let i = hover.get()?;
        let r = shown.with(|rs| rs.get(i).cloned())?;
        let m = mask.get();
        let ser = series.get();
        let x = (left + (i as f64 + 0.5) * band()) / cw * 100.0;
        let total = stack(&r, &m);
        let lines = ser
            .into_iter()
            .zip(r.vals.clone())
            .zip(m)
            .filter(|((_, v), on)| *on && *v > 0.0)
            .map(|((s, v), _)| view! { <div class="tip-row"><i style=format!("background:{}", s.color)></i><span>{s.label}</span><b>{fmt(v)}</b></div> })
            .collect_view();
        let side = if x > 70.0 { "right" } else { "left" };
        let pos = if side == "left" { format!("left:{x:.1}%") } else { format!("right:{:.1}%", 100.0 - x) };
        Some(view! {
            <div class="tip" style=pos>
                <div class="tip-head">{r.label.clone()}<span>{fmt(total)}</span></div>
                {lines}
            </div>
        })
    };

    let legend = move || {
        series
            .get()
            .into_iter()
            .map(|s| {
                let key = s.key.clone();
                let k2 = s.key.clone();
                view! {
                    <button class="legend-item" class:off=move || hidden.with(|h| h.contains(&k2))
                        on:click=move |_| hidden.update(|h| if let Some(p) = h.iter().position(|x| *x == key) { h.remove(p); } else { h.push(key.clone()); })>
                        <i style=format!("background:{}", s.color)></i>{s.label}
                    </button>
                }
            })
            .collect_view()
    };

    let range_picker = move || {
        ranges.then(|| {
            view! {
                <div class="seg">
                    {[(7usize, "7d"), (30, "30d"), (90, "90d"), (0, "all")]
                        .into_iter()
                        .map(|(n, l)| view! { <button class:on=move || range.get() == n on:click=move |_| range.set(n)>{l}</button> })
                        .collect_view()}
                </div>
            }
        })
    };

    view! {
        <div class="chart">
            <div class="chart-bar">
                <div class="legend">{legend}</div>
                {range_picker}
            </div>
            <div class="chart-plot" on:mouseleave=move |_| hover.set(None)>
                <svg viewBox=format!("0 0 {cw} {height}") role="img">
                    {grid}
                    {focus}
                    {bars}
                    {line}
                    {xlabels}
                    {hit}
                </svg>
                {tip}
            </div>
            {move || shown.with(Vec::is_empty).then(|| view! { <p class="empty overlay">"No data in this range yet."</p> })}
        </div>
    }
}

/// A small trend line with its area.
#[component]
pub fn Sparkline(values: Vec<f64>, #[prop(default = "var(--c-accent)")] color: &'static str) -> impl IntoView {
    let (w, h) = (120.0, 32.0);
    let mx = values.iter().copied().fold(0.0, f64::max).max(1e-9);
    let n = values.len().max(2) - 1;
    let pts: Vec<(f64, f64)> = values.iter().enumerate().map(|(i, v)| (i as f64 / n as f64 * w, h - 2.0 - v / mx * (h - 4.0))).collect();
    let line: String = pts.iter().enumerate().map(|(i, (x, y))| format!("{}{x:.1},{y:.1}", if i == 0 { "M" } else { " L" })).collect();
    let area = if pts.is_empty() { String::new() } else { format!("{line} L{w},{h} L0,{h} Z") };
    view! {
        <svg class="spark" viewBox=format!("0 0 {w} {h}") preserveAspectRatio="none">
            <path d=area style=format!("fill:{color};opacity:.14") />
            <path d=line style=format!("stroke:{color}") class="spark-line" />
        </svg>
    }
}

/// A headline number with its trend and the change against the week before.
#[component]
pub fn Tile(
    label: &'static str,
    #[prop(into)] value: Signal<String>,
    #[prop(into, optional)] sub: Option<Signal<String>>,
    #[prop(into, optional)] spark: Option<Signal<Vec<f64>>>,
    /// Relative change, e.g. 0.25 for +25%.
    #[prop(into, optional)]
    delta: Option<Signal<Option<f64>>>,
    #[prop(default = "var(--c-accent)")] color: &'static str,
    /// Whether a rise is good news (green) or bad (red).
    #[prop(default = true)]
    up_is_good: bool,
) -> impl IntoView {
    view! {
        <div class="tile">
            <div class="tile-top">
                <span class="l">{label}</span>
                {move || delta.and_then(|d| d.get()).map(|d| {
                    let good = (d >= 0.0) == up_is_good;
                    let cls = if d.abs() < 0.005 { "delta" } else if good { "delta good" } else { "delta bad" };
                    view! { <span class=cls title="last 7 days against the 7 before">{format!("{}{:.0}%", if d >= 0.0 { "+" } else { "" }, d * 100.0)}</span> }
                })}
            </div>
            <span class="v">{move || value.get()}</span>
            {sub.map(|s| view! { <span class="s">{move || s.get()}</span> })}
            {spark.map(|sp| view! { <div class="tile-spark">{move || view! { <Sparkline values=sp.get() color=color /> }}</div> })}
        </div>
    }
}

/// A conversion funnel: each step's bar is scaled to the first, with its share of the step before.
#[component]
pub fn Funnel(#[prop(into)] steps: Signal<Vec<(String, f64, String, String)>>) -> impl IntoView {
    move || {
        let st = steps.get();
        let first = st.first().map(|s| s.1).unwrap_or(0.0).max(1e-9);
        let mut prev: Option<f64> = None;
        st.into_iter()
            .map(|(label, v, color, note)| {
                let share = prev.map(|p| pct(v, p));
                prev = Some(v);
                let w = (v / first * 100.0).clamp(if v > 0.0 { 1.5 } else { 0.0 }, 100.0);
                view! {
                    <div class="funnel-step">
                        <div class="funnel-label"><b>{num(v)}</b>" "{label}</div>
                        <div class="funnel-track"><div class="funnel-fill" style=format!("width:{w:.1}%;background:{color}")></div></div>
                        <div class="funnel-note">{share.map(|s| format!("{s} of previous")).unwrap_or_default()}{(!note.is_empty()).then(|| format!(" · {note}"))}</div>
                    </div>
                }
            })
            .collect_view()
    }
}

/// A donut of parts, with its legend.
#[component]
pub fn Donut(#[prop(into)] parts: Signal<Vec<(String, f64, String)>>, #[prop(into)] center: Signal<String>, caption: &'static str) -> impl IntoView {
    let hover = RwSignal::new(None::<usize>);
    let r = 40.0;
    let c = 2.0 * std::f64::consts::PI * r;
    let rings = move || {
        let ps = parts.get();
        let total: f64 = ps.iter().map(|p| p.1).sum();
        if total <= 0.0 {
            return view! { <circle cx="50" cy="50" r=format!("{r}") class="donut-empty" /> }.into_any();
        }
        let mut at = 0.0;
        ps.into_iter()
            .enumerate()
            .filter(|(_, p)| p.1 > 0.0)
            .map(|(i, (_, v, color))| {
                let len = v / total * c;
                let off = -at;
                at += len;
                view! {
                    <circle cx="50" cy="50" r=format!("{r}") class="donut-seg" class:dim=move || hover.get().is_some_and(|h| h != i)
                        style=format!("stroke:{color};stroke-dasharray:{len:.2} {:.2};stroke-dashoffset:{off:.2}", c - len)
                        on:mouseenter=move |_| hover.set(Some(i)) on:mouseleave=move |_| hover.set(None) />
                }
            })
            .collect_view()
            .into_any()
    };
    let legend = move || {
        let ps = parts.get();
        let total: f64 = ps.iter().map(|p| p.1).sum();
        ps.into_iter()
            .enumerate()
            .map(|(i, (label, v, color))| {
                view! {
                    <div class="dl-row" class:hl=move || hover.get() == Some(i)
                        on:mouseenter=move |_| hover.set(Some(i)) on:mouseleave=move |_| hover.set(None)>
                        <i style=format!("background:{color}")></i><span>{label}</span><b>{num(v)}</b><em>{pct(v, total)}</em>
                    </div>
                }
            })
            .collect_view()
    };
    view! {
        <div class="donut">
            // The caption is HTML under the ring, not SVG text inside it: SVG text never wraps, and a
            // caption wider than the hole runs across the ring.
            <figure class="donut-fig">
                <svg viewBox="0 0 100 100">
                    <g transform="rotate(-90 50 50)">{rings}</g>
                    <text x="50" y="50" text-anchor="middle" dominant-baseline="central" class="donut-v">{move || center.get()}</text>
                </svg>
                <figcaption class="donut-c">{caption}</figcaption>
            </figure>
            <div class="donut-legend">{legend}</div>
        </div>
    }
}

/// Labelled horizontal bars, largest first; an optional pick callback makes rows clickable.
#[component]
pub fn HBars(
    #[prop(into)] items: Signal<Vec<(String, f64)>>,
    #[prop(default = "var(--c-accent)")] color: &'static str,
    #[prop(optional)] money: bool,
    #[prop(optional)] on_pick: Option<Callback<String>>,
    #[prop(default = 10)] limit: usize,
) -> impl IntoView {
    move || {
        let mut it = items.get();
        it.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mx = it.first().map(|x| x.1).unwrap_or(0.0).max(1e-9);
        if it.is_empty() {
            return view! { <p class="empty">"Nothing yet."</p> }.into_any();
        }
        it.into_iter()
            .take(limit)
            .map(|(label, v)| {
                let l2 = label.clone();
                view! {
                    <div class="hbar" class:pick=on_pick.is_some() on:click=move |_| if let Some(cb) = on_pick { cb.run(l2.clone()) }>
                        <span class="hbar-l" title=label.clone()>{label.clone()}</span>
                        <span class="hbar-t"><span class="hbar-f" style=format!("width:{:.1}%;background:{color}", v / mx * 100.0)></span></span>
                        <span class="hbar-v">{if money { usd(v) } else { num(v) }}</span>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    }
}
