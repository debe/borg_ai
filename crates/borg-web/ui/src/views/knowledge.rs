//! Knowledge: a map of the lessons by system and concern area (drill in by clicking), the lessons of
//! the current selection grouped under headings, and how the knowledge grew.

use std::collections::BTreeMap;

use leptos::prelude::*;

use crate::api::{Lesson, data};
use crate::charts::{HBars, Row, Series, StackedChart};
use crate::util::{day_of_ms, num, palette, pct, short_day, trunc};

/// Squarified treemap (Bruls et al.): rectangles for `areas` (largest first) filling `w`×`h`.
pub fn squarify(areas: &[f64], w: f64, h: f64) -> Vec<(f64, f64, f64, f64)> {
    fn worst(row: &[f64], side: f64) -> f64 {
        let s: f64 = row.iter().sum();
        let mx = row.iter().copied().fold(f64::MIN, f64::max);
        let mn = row.iter().copied().fold(f64::MAX, f64::min);
        f64::max(side * side * mx / (s * s), s * s / (side * side * mn))
    }
    let (mut x, mut y, mut w, mut h) = (0.0, 0.0, w, h);
    let mut out = Vec::with_capacity(areas.len());
    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h).max(1e-9);
        let mut row = vec![areas[i]];
        let mut j = i + 1;
        while j < areas.len() {
            let mut next = row.clone();
            next.push(areas[j]);
            if worst(&next, side) <= worst(&row, side) {
                row = next;
                j += 1;
            } else {
                break;
            }
        }
        let s: f64 = row.iter().sum();
        if w >= h {
            let cw = s / h.max(1e-9);
            let mut yy = y;
            for a in &row {
                let rh = a / cw.max(1e-9);
                out.push((x, yy, cw, rh));
                yy += rh;
            }
            x += cw;
            w -= cw;
        } else {
            let rh = s / w.max(1e-9);
            let mut xx = x;
            for a in &row {
                let cw = a / rh.max(1e-9);
                out.push((xx, y, cw, rh));
                xx += cw;
            }
            y += rh;
            h -= rh;
        }
        i = j;
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum ColorBy {
    Activation,
    Usefulness,
    Asked,
}

impl ColorBy {
    fn score(self, ls: &[&Lesson]) -> f64 {
        let n = ls.len().max(1) as f64;
        match self {
            ColorBy::Activation => ls.iter().map(|l| l.activation).sum::<f64>() / n,
            ColorBy::Usefulness => ls.iter().map(|l| l.usefulness()).sum::<f64>() / n,
            ColorBy::Asked => ls.iter().map(|l| l.asked()).sum::<f64>(),
        }
    }

    fn label(self) -> &'static str {
        match self {
            ColorBy::Activation => "activation",
            ColorBy::Usefulness => "usefulness",
            ColorBy::Asked => "times asked",
        }
    }
}

#[derive(Clone, PartialEq)]
struct Group {
    key: String,
    count: usize,
    score: f64,
    used: f64,
    dormant: usize,
}

fn groups(ls: &[Lesson], key: impl Fn(&Lesson) -> String, by: ColorBy) -> Vec<Group> {
    let mut m: BTreeMap<String, Vec<&Lesson>> = BTreeMap::new();
    for l in ls {
        m.entry(key(l)).or_default().push(l);
    }
    let mut g: Vec<Group> = m
        .into_iter()
        .map(|(k, v)| Group {
            key: k,
            count: v.len(),
            score: by.score(&v),
            used: v.iter().map(|l| l.changed).sum(),
            dormant: v.iter().filter(|l| l.dormant).count(),
        })
        .collect();
    g.sort_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
    g
}

#[component]
pub fn Knowledge() -> impl IntoView {
    let d = data();
    let system = RwSignal::new(None::<String>);
    let area = RwSignal::new(None::<String>);
    let query = RwSignal::new(String::new());
    let color_by = RwSignal::new(ColorBy::Activation);

    let matching = Memo::new(move |_| {
        let q = query.get().to_lowercase();
        d.lessons.with(|ls| ls.iter().filter(|l| q.is_empty() || q.split_whitespace().all(|w| l.haystack().contains(w))).cloned().collect::<Vec<_>>())
    });
    // The lessons of the current selection.
    let selected = Memo::new(move |_| {
        let (s, a) = (system.get(), area.get());
        matching.with(|ls| {
            ls.iter().filter(|l| s.as_ref().is_none_or(|s| &l.system() == s) && a.as_ref().is_none_or(|a| &l.area() == a)).cloned().collect::<Vec<_>>()
        })
    });
    let tiles = Memo::new(move |_| {
        let by = color_by.get();
        if system.get().is_none() {
            matching.with(|ls| groups(ls, Lesson::system, by))
        } else if area.get().is_none() {
            selected.with(|ls| groups(ls, Lesson::area, by))
        } else {
            Vec::new()
        }
    });

    let (mw, mh) = (1000.0, 440.0);
    // One cell of the map: a group of lessons, shaded by its score between the lowest and highest
    // shown (activation is log-scale and many lessons share its floor, so ties shade alike).
    let cell = move |g: Group, (x, y, w, h): (f64, f64, f64, f64), t: f64, base: &'static str, pick: Callback<()>| {
        let by = color_by.get_untracked();
        let style = format!("fill:{base};fill-opacity:{:.2}", 0.22 + 0.7 * t);
        let ink = if t > 0.55 { "tm-text light" } else { "tm-text" };
        let big = w > 90.0 && h > 42.0;
        let small = !big && w > 40.0 && h > 18.0;
        let title = format!("{}: {} lessons · {} {:.2} · used {}× · {} dormant", g.key, g.count, by.label(), g.score, num(g.used), g.dormant);
        view! {
            <g class="tm-cell" on:click=move |e| { e.stop_propagation(); pick.run(()) }>
                <title>{title}</title>
                <rect x=format!("{:.1}", x + 1.0) y=format!("{:.1}", y + 1.0) width=format!("{:.1}", (w - 2.0).max(0.0)) height=format!("{:.1}", (h - 2.0).max(0.0)) rx="3" style=style />
                {big.then(|| view! {
                    <text x=format!("{:.1}", x + 9.0) y=format!("{:.1}", y + 21.0) class=format!("{ink} tm-label")>{trunc(&g.key, ((w - 16.0) / 8.0) as usize)}</text>
                    <text x=format!("{:.1}", x + 9.0) y=format!("{:.1}", y + 37.0) class=format!("{ink} tm-sub")>{format!("{} lessons", g.count)}</text>
                })}
                {small.then(|| view! {
                    <text x=format!("{:.1}", x + 5.0) y=format!("{:.1}", y + 14.0) class=format!("{ink} tm-sub")>{trunc(&g.key, ((w - 8.0) / 6.6) as usize)}</text>
                })}
            </g>
        }
    };
    let shade = |gs: &[Group]| {
        let hi = gs.iter().map(|g| g.score).fold(f64::MIN, f64::max);
        let lo = gs.iter().map(|g| g.score).fold(f64::MAX, f64::min);
        move |g: &Group| if hi - lo < 1e-6 { 0.75 } else { ((g.score - lo) / (hi - lo)).clamp(0.0, 1.0) }
    };
    let map = move || {
        let gs = tiles.get();
        if gs.is_empty() {
            return None;
        }
        let by = color_by.get();
        let total: f64 = gs.iter().map(|g| g.count as f64).sum();
        let rects = squarify(&gs.iter().map(|g| g.count as f64 / total * mw * mh).collect::<Vec<_>>(), mw, mh);
        let body = if system.get().is_none() {
            // Systems, each subdivided into its concern areas: the whole structure at a glance.
            let all = matching.get();
            let inner: Vec<Vec<Group>> = gs
                .iter()
                .map(|g| {
                    let of: Vec<Lesson> = all.iter().filter(|l| l.system() == g.key).cloned().collect();
                    groups(&of, Lesson::area, by)
                })
                .collect();
            let flat: Vec<Group> = inner.iter().flatten().cloned().collect();
            let t_of = shade(&flat);
            gs.into_iter()
                .zip(rects)
                .zip(inner)
                .enumerate()
                .map(|(i, ((g, (x, y, w, h)), areas))| {
                    let base = palette(i);
                    let head = if h > 60.0 && w > 70.0 { 22.0 } else { 0.0 };
                    let (ix, iy, iw, ih) = (x + 3.0, y + head + 2.0, (w - 6.0).max(1.0), (h - head - 5.0).max(1.0));
                    let n: f64 = areas.iter().map(|a| a.count as f64).sum::<f64>().max(1.0);
                    let sub = squarify(&areas.iter().map(|a| a.count as f64 / n * iw * ih).collect::<Vec<_>>(), iw, ih);
                    let sys = g.key.clone();
                    let sys2 = g.key.clone();
                    let cells = areas
                        .into_iter()
                        .zip(sub)
                        .map(|(a, (ax, ay, aw, ah))| {
                            let (s, ar) = (sys.clone(), a.key.clone());
                            let t = t_of(&a);
                            cell(a, (ix + ax, iy + ay, aw, ah), t, base, Callback::new(move |_| { system.set(Some(s.clone())); area.set(Some(ar.clone())); }))
                        })
                        .collect_view();
                    let label = (head > 0.0).then(|| view! {
                        <text x=format!("{:.1}", x + 6.0) y=format!("{:.1}", y + 16.0) class="tm-sys">{trunc(&format!("{} · {}", g.key, g.count), ((w - 10.0) / 7.5) as usize)}</text>
                    });
                    view! {
                        <g class="tm-system" on:click=move |_| system.set(Some(sys2.clone()))>
                            <title>{format!("{}: {} lessons; click for its areas", g.key, g.count)}</title>
                            <rect x=format!("{:.1}", x + 0.5) y=format!("{:.1}", y + 0.5) width=format!("{:.1}", (w - 1.0).max(0.0)) height=format!("{:.1}", (h - 1.0).max(0.0)) rx="5" class="tm-frame" style=format!("stroke:{base}") />
                            {label}
                            {cells}
                        </g>
                    }
                })
                .collect_view()
                .into_any()
        } else {
            let t_of = shade(&gs);
            gs.into_iter()
                .zip(rects)
                .map(|(g, r)| {
                    let k = g.key.clone();
                    let t = t_of(&g);
                    cell(g, r, t, "var(--c-accent)", Callback::new(move |_| area.set(Some(k.clone()))))
                })
                .collect_view()
                .into_any()
        };
        Some(view! { <svg class="treemap" viewBox=format!("0 0 {mw} {mh}")>{body}</svg> })
    };
    // Every system as a chip, so small ones the map cannot label are one click away.
    let chips = move || {
        if system.get().is_some() {
            return None;
        }
        let gs = tiles.get();
        Some(view! {
            <div class="sys-chips">
                {gs.into_iter().enumerate().map(|(i, g)| {
                    let k = g.key.clone();
                    view! { <button class="sys-chip" on:click=move |_| system.set(Some(k.clone()))><i style=format!("background:{}", palette(i))></i>{g.key.clone()}<b>{g.count}</b></button> }
                }).collect_view()}
            </div>
        })
    };

    let crumbs = move || {
        let (s, a) = (system.get(), area.get());
        view! {
            <nav class="crumbs">
                <button class:on=s.is_none() on:click=move |_| { system.set(None); area.set(None); }>"All systems"</button>
                {s.clone().map(|s| { let s2 = s.clone(); view! { <span>"›"</span><button class:on=a.is_none() on:click=move |_| { system.set(Some(s2.clone())); area.set(None); }>{s}</button> } })}
                {a.map(|a| view! { <span>"›"</span><button class="on">{a}</button> })}
            </nav>
        }
    };

    // Lesson cards grouped under headings: by system at the top level, by area inside a system.
    let list = move || {
        let ls = selected.get();
        if ls.is_empty() {
            return view! { <p class="empty">{if d.lessons.with(Vec::is_empty) { "No lessons in this collective yet." } else { "No lesson matches." }}</p> }
                .into_any();
        }
        let by_area = system.get().is_some();
        let mut m: BTreeMap<String, Vec<Lesson>> = BTreeMap::new();
        for l in ls {
            m.entry(if by_area { l.area() } else { l.system() }).or_default().push(l);
        }
        let mut groups: Vec<(String, Vec<Lesson>)> = m.into_iter().collect();
        groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()));
        let single = groups.len() == 1;
        groups
            .into_iter()
            .enumerate()
            .map(|(i, (name, mut ls))| {
                ls.sort_by(|a, b| b.activation.total_cmp(&a.activation));
                let count = ls.len();
                let used: f64 = ls.iter().map(|l| l.changed).sum();
                let asked: f64 = ls.iter().map(|l| l.asked()).sum();
                let rest: Vec<Lesson> = ls.split_off(ls.len().min(6));
                let more = rest.len();
                let cards = ls.into_iter().map(|l| lesson_card(l, d.open)).collect_view();
                let rest = (more > 0).then(|| {
                    view! {
                        <details class="more-cards">
                            <summary>{format!("Show {more} more")}</summary>
                            <div class="cards inner">{rest.into_iter().map(|l| lesson_card(l, d.open)).collect_view()}</div>
                        </details>
                    }
                });
                view! {
                    <details class="group" open=single || i == 0>
                        <summary>
                            <span class="group-name">{name}</span>
                            <span class="group-count">{count}</span>
                            <span class="group-meta">{format!("asked {} · used {}", num(asked), num(used))}</span>
                        </summary>
                        <div class="cards">{cards}{rest}</div>
                    </details>
                }
            })
            .collect_view()
            .into_any()
    };

    let per_week = Signal::derive(move || {
        d.lessons.with(|ls| {
            let mut m: BTreeMap<i64, [f64; 3]> = BTreeMap::new();
            for l in ls.iter().filter(|l| l.origin_ms > 0.0) {
                let day = day_of_ms(l.origin_ms);
                let week = day - (day + 3).rem_euclid(7);
                let slot = match l.source.as_str() {
                    "import" => 1,
                    "queen" => 2,
                    _ => 0,
                };
                m.entry(week).or_default()[slot] += 1.0;
            }
            let (Some(first), Some(last)) = (m.keys().next().copied(), m.keys().last().copied()) else { return Vec::new() };
            (0..=(last - first) / 7)
                .map(|k| first + 7 * k)
                .map(|w| Row { label: short_day(w), vals: m.get(&w).copied().unwrap_or_default().to_vec() })
                .collect()
        })
    });
    let sources = vec![
        Series::new("session", "learned in session", "var(--c-good)"),
        Series::new("import", "imported", "var(--c-sky)"),
        Series::new("queen", "queen answers", "var(--c-violet)"),
    ];
    let by_area =
        Signal::derive(move || d.lessons.with(|ls| groups(ls, Lesson::area, ColorBy::Asked).into_iter().map(|g| (g.key, g.count as f64)).collect::<Vec<_>>()));
    let pick_area = Callback::new(move |a: String| {
        system.set(None);
        area.set(None);
        query.set(a);
    });

    view! {
        <section class="panel">
            <div class="kn-bar">
                <h2>"Knowledge map"<small>{move || format!("{} of {} lessons", matching.with(Vec::len), d.lessons.with(Vec::len))}</small></h2>
                <input class="search" placeholder="search questions, systems, areas, engineers" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
            </div>
            <div class="kn-bar">
                {crumbs}
                <div class="seg">
                    <span class="seg-l">"shade by"</span>
                    {[ColorBy::Activation, ColorBy::Usefulness, ColorBy::Asked].into_iter().map(|c| view! {
                        <button class:on=move || color_by.get() == c on:click=move |_| color_by.set(c)>{c.label()}</button>
                    }).collect_view()}
                </div>
            </div>
            {map}
            {chips}
            <p class="hint">{move || if system.get().is_none() { "Each block is a system, split into its concern areas; size is the number of lessons. Click an area to list its lessons, a system header to zoom in." } else if area.get().is_none() { "Click an area to list its lessons." } else { "" }}</p>
        </section>
        <section class="kn-grid">
            <div class="kn-list">{list}</div>
            <aside class="kn-side">
                <div class="panel">
                    <h2>"Growth"<small>"lessons per week, by source"</small></h2>
                    <StackedChart rows=per_week series=sources height=240.0 width=440.0 />
                </div>
                <div class="panel">
                    <h2>"Concern areas"<small>"click to search"</small></h2>
                    <HBars items=by_area color="var(--c-violet)" on_pick=pick_area limit=12 />
                </div>
            </aside>
        </section>
    }
}

/// One lesson as a card; clicking opens the drawer.
pub fn lesson_card(l: Lesson, open: RwSignal<Option<String>>) -> impl IntoView {
    let id = l.id.clone();
    let use_pct = pct(l.usefulness(), 1.0);
    view! {
        <button class="lesson" class:dormant=l.dormant on:click=move |_| open.set(Some(id.clone()))>
            <span class="q">{l.question.clone()}</span>
            {(!l.consequence.is_empty()).then(|| view! { <span class="c">{trunc(&l.consequence, 180)}</span> })}
            <span class="meta">
                {l.scope.iter().take(2).map(|s| view! { <span class="chip a">{s.clone()}</span> }).collect_view()}
                <span class="chip">{l.area()}</span>
                {(l.source == "queen").then(|| view! { <span class="chip v">"queen"</span> })}
                {(l.reach == "repo").then(|| view! { <span class="chip" title="a convention of its repo, delivered only there">{format!("only in {}", l.repo)}</span> })}
                {l.consolidated.then(|| view! { <span class="chip g">"long-term"</span> })}
                {l.dormant.then(|| view! { <span class="chip">"dormant"</span> })}
                <span class="stat" title="asked · used · corrected anyway · vetoed">{format!("? {} · ✓ {} · ! {}{}", num(l.asked()), num(l.changed), num(l.corrected), if l.vetoed > 0.0 { format!(" · ⊘ {}", num(l.vetoed)) } else { String::new() })}</span>
                <span class="stat" title="usefulness belief">{use_pct}</span>
            </span>
        </button>
    }
}
