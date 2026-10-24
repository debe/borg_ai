//! Hive: the collective's hive goals (`borg hive`): each goal's board by status (a holder on another
//! node named with it), what stops it here (conflicting definitions, checks awaiting `borg hive
//! approve`), its integration, its reworks, its groups of attempts (winners, losers, the one integrated, what
//! reflection learned), what each drone of `borg hive run` spent of its share, and the newest contracts
//! and messages between drones. Above them, the collective's L1 playbooks: their steps, the belief of
//! each version drones were shown, and the edits waiting for an engineer (a playbook's first included).

use leptos::prelude::*;

use crate::api::{HiveGoal, Playbook, data};
use crate::util::{ago, pct, trunc};

fn playbook_view(p: Playbook) -> impl IntoView {
    // A playbook no edit was promoted to yet shows with its waiting edits alone.
    let meta = format!(
        "{}{}{}{}",
        if p.edits > 0.0 { format!("v{} · ", p.edits) } else { String::new() },
        if p.repo.is_empty() { String::new() } else { format!("{} · ", p.repo) },
        if p.edits > 0.0 { format!("promoted {}", ago(p.promoted_ms)) } else { "nothing promoted yet".into() },
        if p.proposed > 0.0 { format!(" · {} edit(s) waiting in the queen's queue", p.proposed) } else { String::new() }
    );
    view! {
        <div>
            <h3 title=p.key.clone()>{format!("“{}”", p.shape)}<small>{format!(" {} · {meta}", p.id)}</small></h3>
            <ol>{p.steps.iter().map(|(id, text)| view! { <li title=id.clone()>{trunc(text, 240)}</li> }).collect_view()}</ol>
            {(!p.waiting.is_empty()).then(|| view! {
                <ul class="feed-meta" title="proposed edits an engineer has not answered yet (borg queen)">
                    {p.waiting.iter().map(|(op, text)| view! { <li>{format!("waiting: {op} “{}”", trunc(text, 240))}</li> }).collect_view()}
                </ul>
            })}
            {(!p.versions.is_empty()).then(|| view! {
                <table class="mono">
                    <tr><th>"version shown"</th><th>"useful"</th><th>"verified"</th><th>"rejected"</th><th>"hacked"</th><th>"vetoed"</th><th>"regretted"</th></tr>
                    {p.versions.iter().map(|v| view! {
                        <tr>
                            <td>{v.key.clone()}</td><td>{pct(v.mean, 1.0)}</td><td>{v.verified}</td><td>{v.rejected}</td>
                            <td>{v.hacked}</td><td>{v.vetoed}</td><td>{format!("{:.2}", v.regretted)}</td>
                        </tr>
                    }).collect_view()}
                </table>
            })}
        </div>
    }
}

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
            {(!g.groups.is_empty()).then(|| view! {
                <h3>"Groups"</h3>
                <table class="mono">
                    <tr><th>"group"</th><th>"attempts"</th><th>"won"</th><th>"lost"</th><th>"integrates"</th><th>"reflection"</th></tr>
                    {g.groups.iter().map(|r| view! {
                        <tr title=format!("decided by {}", r.decided_by)>
                            <td>{r.group.clone()}</td>
                            <td>{format!("{} of {}", r.attempts, r.members)}</td>
                            <td>{if r.won.is_empty() { "-".to_string() } else { r.won.join(", ") }}</td>
                            <td>{if r.lost.is_empty() { "-".to_string() } else { r.lost.join(", ") }}</td>
                            <td>{r.integrates.clone()}</td>
                            <td>{r.reflection.clone()}</td>
                        </tr>
                    }).collect_view()}
                </table>
            })}
            {(!g.reworks.is_empty()).then(|| view! {
                <h3>"Reworks"</h3>
                <table class="mono">
                    <tr><th>"task"</th><th>"rejected"</th><th>"drones"</th><th>"reflection"</th></tr>
                    {g.reworks.iter().map(|r| view! {
                        <tr title="rejected by the verifier, then verified: a lesson source">
                            <td>{r.task.clone()}</td>
                            <td>{format!("{}×", r.rejections)}</td>
                            <td>{r.drones.join(" → ")}</td>
                            <td>{r.reflection.clone()}</td>
                        </tr>
                    }).collect_view()}
                </table>
            })}
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
                let pbs = d.playbooks.get();
                (!pbs.is_empty()).then(|| view! {
                    <article class="panel">
                        <div class="kn-bar"><h2>"Playbooks"<small>"procedures learned from verified work, promoted by engineers"</small></h2></div>
                        {pbs.into_iter().map(playbook_view).collect_view()}
                    </article>
                })
            }}
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
