//! The queen: decisions waiting for an engineer, and how they were answered.

use leptos::prelude::*;

use crate::api::{QueenEntry, data};
use crate::charts::{Donut, Tile};
use crate::util::{ago, duration, who};

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

fn card(q: QueenEntry) -> impl IntoView {
    let state = q.state();
    let answered = state == "answered";
    let latency = (answered && q.answered_ms > q.ms && q.ms > 0.0).then(|| duration(q.answered_ms - q.ms));
    view! {
        <article class=format!("queen-card {state}")>
            <div class="queen-head">
                <span class=format!("badge {state}")>{state}</span>
                <span class="feed-meta">{format!("{} · {}", if q.repo.is_empty() { "?".to_string() } else { q.repo.clone() }, ago(q.ms))}</span>
            </div>
            <div class="queen-q">{q.question.clone()}</div>
            {(!q.options.is_empty()).then(|| view! {
                <div class="options">
                    {q.options.iter().map(|o| {
                        let chosen = answered && (q.answer == *o || q.answer.starts_with(o.as_str()));
                        let default = !answered && *o == q.default;
                        view! { <span class="opt" class:chosen=chosen class:default=default>{o.clone()}</span> }
                    }).collect_view()}
                </div>
            })}
            {answered.then(|| view! {
                <div class="answer">
                    <b>{format!("{}: ", who(&q.answered_by))}</b>{q.answer.clone()}
                    {latency.map(|l| view! { <span class="feed-meta">{format!(" · answered after {l}")}</span> })}
                </div>
            })}
        </article>
    }
}

/// The queen's attention reward: did its escalations spend the engineer's minutes well?
fn attention(d: crate::api::Data) -> impl IntoView {
    let a = move || d.attention.get();
    let parts = Signal::derive(move || {
        let a = a();
        vec![
            ("ended in no".to_string(), a.no, "var(--c-good)".to_string()),
            ("yes, fine".into(), a.stamps, "var(--c-amber)".into()),
            ("missed".into(), a.missed, "var(--c-heat)".into()),
        ]
    });
    let share = Signal::derive(move || a().share.map_or_else(|| "–".to_string(), |s| format!("{:.0}%", s * 100.0)));
    let reward = Signal::derive(move || format!("{:+}", a().reward));
    let weights = Signal::derive(move || {
        let a = a();
        format!("no − {}·stamp − {}·missed", a.alpha, a.beta)
    });
    view! {
        <section class="panel">
            <h2>"The queen's attention"<small>"the team's escalations, scored; logged and shown, never optimised"</small></h2>
            <section class="tiles">
                <Tile label="Reward" value=reward sub=weights color="var(--c-violet)" />
                <Tile label="Escalation yield" value=share sub=Signal::derive(|| "answered escalations that ended in a no".to_string()) color="var(--c-good)" />
                <Tile label="Rubber stamps" value=Signal::derive(move || format!("{}", a().stamps)) sub=Signal::derive(|| "answered \"yes, fine\"".to_string()) color="var(--c-amber)" />
                <Tile label="Missed escalations" value=Signal::derive(move || format!("{}", a().missed)) sub=Signal::derive(|| "a no after merge, never escalated".to_string()) color="var(--c-heat)" />
            </section>
            <Donut parts=parts center=Signal::derive(move || { let a = a(); format!("{}", a.answered + a.missed) }) caption="scored" />
        </section>
    }
}

#[component]
pub fn Queen() -> impl IntoView {
    let d = data();
    let by = move |s: &'static str| d.queen.with(|q| q.iter().filter(|e| e.state() == s).count() as f64);
    let parts = Signal::derive(move || {
        vec![
            ("open".to_string(), by("open"), "var(--c-violet)".to_string()),
            ("answered".into(), by("answered"), "var(--c-good)".into()),
            ("dismissed".into(), by("dismissed"), "var(--c-slate)".into()),
        ]
    });
    let latency = Signal::derive(move || {
        d.queen
            .with(|q| median(q.iter().filter(|e| e.state() == "answered" && e.answered_ms > e.ms && e.ms > 0.0).map(|e| e.answered_ms - e.ms).collect()))
            .map(duration)
            .unwrap_or_else(|| "–".into())
    });
    let list = move |state: &'static str| {
        move || {
            let mut q: Vec<QueenEntry> = d.queen.with(|q| q.iter().filter(|e| e.state() == state).cloned().collect());
            q.sort_by(|a, b| b.ms.total_cmp(&a.ms));
            if q.is_empty() {
                return view! { <p class="empty">"None."</p> }.into_any();
            }
            q.into_iter().map(card).collect_view().into_any()
        }
    };
    view! {
        {move || d.source("queen").missing.get().then(|| view! { <p class="empty panel">"This borg web is older than the queen view: reinstall borg-web."</p> })}
        <section class="tiles">
            <Tile label="Open" value=Signal::derive(move || format!("{}", by("open"))) sub=Signal::derive(|| "waiting for an engineer".to_string()) color="var(--c-violet)" />
            <Tile label="Answered" value=Signal::derive(move || format!("{}", by("answered"))) sub=Signal::derive(|| "each answer became a lesson".to_string()) color="var(--c-good)" />
            <Tile label="Dismissed" value=Signal::derive(move || format!("{}", by("dismissed"))) color="var(--c-slate)" />
            <Tile label="Median answer time" value=latency />
        </section>
        {move || d.attention.with(|a| a.known).then(|| attention(d))}
        <section class="two">
            <div class="panel">
                <h2>"Decisions"</h2>
                <Donut parts=parts center=Signal::derive(move || format!("{}", by("open") + by("answered") + by("dismissed"))) caption="decisions" />
            </div>
            <div class="panel">
                <h2>"Waiting for you"<small>"answer with borg queen ask, or in your next session"</small></h2>
                {list("open")}
            </div>
        </section>
        <section class="panel">
            <h2>"Answered"</h2>
            <div class="queen-list">{list("answered")}</div>
            <details class="group">
                <summary><span class="group-name">"Dismissed"</span></summary>
                <div class="queen-list">{list("dismissed")}</div>
            </details>
        </section>
    }
}
