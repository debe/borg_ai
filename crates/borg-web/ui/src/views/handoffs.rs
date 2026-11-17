//! Hand-offs: the sessions' work offered to other drones (borg handoff/), who took, declined or was asked
//! for it, and who can take what: per machine of the team its engineers, borg and clones, per open
//! hand-off the engineers with a machine that could take it.

use leptos::prelude::*;

use crate::api::{HandOff, data};
use crate::util::{ago, trunc, who};

/// The badge class of a state.
fn badge(state: &str) -> &'static str {
    match state {
        "open" | "asked" => "open",
        "declined" | "contested" => "idle",
        _ => "answered",
    }
}

fn row(h: HandOff) -> impl IntoView {
    let outcome = match (&h.by, h.state.as_str()) {
        (Some((e, _, s)), _) => format!(
            "{} {} (session {}) · claim {}{}",
            if h.state == "taken back" { "taken back by" } else { "taken by" },
            who(e),
            s.chars().take(8).collect::<String>(),
            h.claim,
            if h.confirmed { ", the remote holds it" } else { ", the remote not asked yet" }
        ),
        (None, "contested") => "several claims cite it; the remote has not said which holds".into(),
        (None, _) if !h.declined.is_empty() => format!("declined by {}", h.declined.iter().map(|d| who(d)).collect::<Vec<_>>().join(", ")),
        (None, _) if !h.to.is_empty() => format!("asks {}: no answer yet", who(&h.to)),
        _ => format!("take it: borg handoff take {}", h.id),
    };
    let age = if h.ms > 0.0 { ago(h.ms) } else { "age unknown".into() };
    view! {
        <div class="feed-row">
            <span class=format!("badge {}", badge(&h.state))>{h.state.clone()}</span>
            <div class="feed-body">
                <div class="feed-head">{format!("{} · {} from {}", h.id, h.repos.join(", "), who(&h.engineer))}</div>
                <div class="feed-detail">{trunc(&h.note, 300)}</div>
                <div class="feed-meta">{format!("{age} · {outcome}")}{h.fenced.then(|| " · the original's edits are refused")}</div>
                {(!h.takers.is_empty() && matches!(h.state.as_str(), "open" | "asked" | "declined")).then(|| view! {
                    <div class="chips">{h.takers.iter().map(|t| view! { <span class="chip g" title="has a machine with a clone of every repo, on a borg that takes hand-offs">{who(t)}</span> }).collect_view()}</div>
                })}
            </div>
        </div>
    }
}

#[component]
pub fn HandOffs() -> impl IntoView {
    let d = data();
    let list = move || d.handoffs.with(|h| h.list.clone());
    let open = move || list().into_iter().filter(|h| matches!(h.state.as_str(), "open" | "asked" | "declined" | "contested")).collect::<Vec<_>>();
    let done = move || list().into_iter().filter(|h| matches!(h.state.as_str(), "taken" | "taken back")).collect::<Vec<_>>();
    view! {
        <section class="panel">
            <h2>"Hand-offs"<small>{move || format!("{} waiting · {} taken", open().len(), done().len())}</small></h2>
            {move || {
                let all = list();
                if all.is_empty() {
                    return view! {
                        <div class="explain">
                            <p class="empty">"No hand-off yet. A session passes its work on with its brief; another drone of the team takes it over in a clone of the repo."</p>
                            <pre>"borg handoff \"next: tests for the parser\" --brief brief.md [--to anna@example.com]
borg handoff                 # what waits, on every machine of the team
borg handoff take <id>       # in a session in a clone"</pre>
                        </div>
                    }.into_any();
                }
                view! {
                    <div class="feed">{open().into_iter().chain(done()).map(row).collect_view()}</div>
                }.into_any()
            }}
        </section>
        <section class="panel">
            <h2>"Who can take what"<small>"machines seen in the last day, from their signed presence"</small></h2>
            {move || {
                let hosts = d.handoffs.with(|h| h.hosts.clone());
                if hosts.is_empty() {
                    return view! { <p class="empty">"No machine seen yet: presence comes with the mesh's sessions."</p> }.into_any();
                }
                let here = d.handoffs.with(|h| h.here.clone());
                view! {
                    <table class="ltable">
                        <tr><th>"Engineers"</th><th title="its hostname, else the node's key, shortened">"Machine"</th><th>"borg"</th><th title="the repos it holds clones of">"Clones"</th></tr>
                        {hosts.into_iter().map(|m| {
                            let node = if m.node == here { "this one".to_string() } else if !m.host.is_empty() { m.host.clone() } else { m.node.chars().take(12).collect() };
                            let borg = if m.borg.is_empty() { "no hand-offs".to_string() } else if m.handoff >= 1.0 { m.borg.clone() } else { format!("{} (no hand-offs)", m.borg) };
                            view! {
                                <tr>
                                    <td>{m.engineers.iter().map(|e| who(e)).collect::<Vec<_>>().join(", ")}</td>
                                    <td class="mono">{node}</td>
                                    <td>{borg}</td>
                                    <td>{if m.repos.is_empty() { "–".to_string() } else { m.repos.join(", ") }}</td>
                                </tr>
                            }
                        }).collect_view()}
                    </table>
                }.into_any()
            }}
        </section>
    }
}
