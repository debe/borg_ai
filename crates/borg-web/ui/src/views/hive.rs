//! Hive: the collective's hive goals (`borg hive`): each goal's board by status (a holder on another
//! node named with it), what stops it here (conflicting definitions, checks awaiting `borg hive
//! approve`), its integration, what each drone of `borg hive run` spent of its share, and the newest
//! contracts and messages between drones.

use leptos::prelude::*;

use crate::api::{HiveGoal, data};
use crate::util::trunc;

/// Board groups in the order a reader scans them: what moves first.
const GROUPS: [&str; 6] = ["claimed", "open", "expired", "blocked", "done", "verified"];

fn goal_view(g: HiveGoal) -> impl IntoView {
    let pct = |a: f64, b: f64| if b > 0.0 { (a / b * 100.0).clamp(0.0, 100.0) } else { 0.0 };
    let spent = pct(g.spent_usd, g.budget_usd);
    view! {
        <article class="panel">
            <div class="kn-bar">
                <h2>{format!("Goal {}", g.goal)}<small>{format!(
                    "${:.2} of ${:.2}{}{}",
                    g.spent_usd,
                    g.budget_usd,
                    if g.nodes > 0.0 { format!(" · {} other node(s)", g.nodes) } else { String::new() },
                    if g.finished { " · finished" } else { "" }
                )}</small></h2>
            </div>
            <p class="feed-meta">{trunc(&g.spec, 200)}</p>
            {g.alerts.iter().map(|a| view! { <p class="feed-meta mono" title="stops this goal on this node">{format!("! {}", trunc(a, 300))}</p> }).collect_view()}
            <div class="bar" title="budget spent"><div style=format!("width:{spent:.0}%;height:6px;background:currentColor;opacity:.6")></div></div>
            {(!g.integration.is_empty()).then(|| view! { <p class="feed-meta mono" title="the combined tree of the accepted work">{trunc(&g.integration, 200)}</p> })}
            {GROUPS.iter().filter_map(|grp| {
                let rows: Vec<_> = g.tasks.iter().filter(|t| t.status == *grp).cloned().collect();
                (!rows.is_empty()).then(|| view! {
                    <h3>{format!("{grp} ({})", rows.len())}</h3>
                    <table class="mono">
                        {rows.into_iter().map(|t| view! {
                            <tr>
                                <td>{t.id}</td><td>{t.role}</td><td>{trunc(&t.title, 60)}</td>
                                <td title=if t.node.is_empty() { "on this node".to_string() } else { format!("on node {}", t.node) }>
                                    {t.holder.chars().take(8).collect::<String>()}
                                    {(!t.node.is_empty()).then(|| view! { <small>{format!(" @{}", t.node)}</small> })}
                                </td>
                                <td>{if t.lease_left_ms > 0.0 { format!("{}m", (t.lease_left_ms / 60_000.0).floor()) } else { String::new() }}</td>
                                <td>{t.files.join(", ")}</td>
                            </tr>
                        }).collect_view()}
                    </table>
                })
            }).collect_view()}
            {(!g.drones.is_empty()).then(|| view! {
                <h3>"Drones"</h3>
                {g.drones.iter().map(|d| {
                    let w = pct(d.usd, d.share);
                    view! {
                        <div class="feed-meta" title=format!("{} iteration(s), last {}", d.iters, d.last_exit)>
                            <span class="mono">{format!("{}  ${:.2}/${:.2}{}  {} iter  {}", d.drone, d.usd, d.share, if d.unmetered { "+" } else { "" }, d.iters, d.last_exit)}</span>
                            <div style=format!("width:{w:.0}%;height:4px;background:currentColor;opacity:.5")></div>
                        </div>
                    }
                }).collect_view()}
            })}
            {(!g.contracts.is_empty()).then(|| view! {
                <h3>"Contracts"</h3>
                <ul>
                    {g.contracts.iter().rev().map(|(task, text, scope)| view! {
                        <li><span class="mono">{format!("{task} [{}]", scope.join(", "))}</span>" "{trunc(text, 160)}</li>
                    }).collect_view()}
                </ul>
            })}
            {(!g.messages.is_empty()).then(|| view! {
                <h3>"Messages"</h3>
                <ul>
                    {g.messages.iter().rev().map(|(kind, who, node, to, text)| {
                        let to = if to.is_empty() { "all".to_string() } else { to.clone() };
                        let at = if node.is_empty() { String::new() } else { format!(" @{}", node.chars().take(8).collect::<String>()) };
                        view! {
                            <li><span class="mono">{format!("{who}{at} {kind} → {to}")}</span>" "{trunc(text, 160)}</li>
                        }
                    }).collect_view()}
                </ul>
            })}
        </article>
    }
}

#[component]
pub fn Hive() -> impl IntoView {
    let d = data();
    view! {
        <section>
            {move || {
                let goals = d.hive.get();
                if goals.is_empty() {
                    return view! { <p class="empty">"No hive goal in this collective (`borg hive init`)."</p> }.into_any();
                }
                goals.into_iter().map(goal_view).collect_view().into_any()
            }}
        </section>
    }
}
