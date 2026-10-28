//! The lesson drawer: one lesson in full, with what happened when it was asked.

use leptos::prelude::*;

use crate::api::data;
use crate::util::{day_of_ms, long_day, num, pct, who};

#[component]
pub fn Drawer() -> impl IntoView {
    let d = data();
    let _esc = window_event_listener(leptos::ev::keydown, move |e| {
        if e.key() == "Escape" {
            d.open.set(None);
        }
    });
    // Activation is log-scale and can be negative: the meter shows where a lesson sits between the
    // collective's least and most active.
    let range = Memo::new(move |_| {
        d.lessons.with(|ls| {
            let lo = ls.iter().map(|l| l.activation).fold(f64::MAX, f64::min);
            let hi = ls.iter().map(|l| l.activation).fold(f64::MIN, f64::max);
            (lo, (hi - lo).max(1e-9))
        })
    });
    move || {
        let id = d.open.get()?;
        let l = d.lesson(&id)?;
        let usage = [
            ("asked", l.asked(), "var(--c-violet)"),
            ("used", l.changed, "var(--c-good)"),
            ("ignored", l.ignored, "var(--c-slate)"),
            ("corrected anyway", l.corrected, "var(--c-heat)"),
            ("vetoed", l.vetoed, "var(--c-heat)"),
            ("regretted later", l.regretted, "var(--c-pink)"),
            ("tests green after", l.tested, "var(--c-good)"),
            ("early, free", l.usage.get("early").copied().unwrap_or(0.0), "var(--c-teal)"),
        ];
        let umax = usage.iter().map(|u| u.1).fold(0.0, f64::max).max(1.0);
        let (lo, span) = range.get();
        let act = ((l.activation - lo) / span * 100.0).clamp(2.0, 100.0);
        Some(view! {
            <div class="scrim" on:click=move |_| d.open.set(None)></div>
            <aside class="drawer" role="dialog" aria-label="Lesson">
                <div class="drawer-top">
                    <div class="chips">
                        {l.scope.iter().map(|s| view! { <span class="chip a">{s.clone()}</span> }).collect_view()}
                        <span class="chip">{l.area()}</span>
                        {l.consolidated.then(|| view! { <span class="chip g">"long-term"</span> })}
                        {l.dormant.then(|| view! { <span class="chip">"dormant"</span> })}
                    </div>
                    <button class="x" on:click=move |_| d.open.set(None) aria-label="Close">"×"</button>
                </div>
                <h3 class="drawer-q">{l.question.clone()}</h3>
                {(!l.situations.is_empty()).then(|| view! {
                    <div class="drawer-sec">
                        <div class="k">"When it applies"</div>
                        <ul>{l.situations.iter().map(|s| view! { <li>{s.clone()}</li> }).collect_view()}</ul>
                    </div>
                })}
                {(!l.consequence.is_empty()).then(|| view! {
                    <div class="drawer-sec">
                        <div class="k">"What it prevents"</div>
                        <p class="conseq">{l.consequence.clone()}</p>
                    </div>
                })}
                <div class="drawer-sec">
                    <div class="k">"Memory"</div>
                    <div class="meter"><span style=format!("width:{act:.0}%")></span></div>
                    <div class="meta">
                        {format!("activation {:.2} · usefulness {} · {}", l.activation, pct(l.usefulness(), 1.0), if l.dormant { "dormant" } else if l.consolidated { "long-term" } else { "active" })}
                    </div>
                </div>
                <div class="drawer-sec">
                    <div class="k">"What happened when it was asked"</div>
                    {usage.into_iter().map(|(label, v, c)| view! {
                        <div class="hbar">
                            <span class="hbar-l">{label}</span>
                            <span class="hbar-t"><span class="hbar-f" style=format!("width:{:.0}%;background:{c}", v / umax * 100.0)></span></span>
                            <span class="hbar-v">{num(v)}</span>
                        </div>
                    }).collect_view()}
                </div>
                <dl class="facts-dl">
                    <dt>"Source"</dt><dd>{l.source_label()}</dd>
                    <dt>"Heard by"</dt><dd>{if l.reach == "repo" { format!("only sessions in {}", l.repo) } else { "the whole team".to_string() }}</dd>
                    <dt>"First learned"</dt><dd>{if l.origin_ms > 0.0 { long_day(day_of_ms(l.origin_ms)) } else { "unknown".into() }}</dd>
                    <dt>"Version"</dt><dd>{format!("{} (sharpened {} time{})", l.version, l.version.saturating_sub(1), if l.version == 2 { "" } else { "s" })}</dd>
                    <dt>"Taught by"</dt><dd>{if l.engineers.is_empty() { "—".to_string() } else { l.engineers.iter().map(|e| who(e)).collect::<Vec<_>>().join(", ") }}</dd>
                    {(!l.repo.is_empty()).then(|| view! { <dt>"Repo"</dt><dd>{l.repo.clone()}</dd> })}
                    <dt>"Id"</dt><dd class="mono">{l.id.clone()}</dd>
                </dl>
            </aside>
        })
    }
}
