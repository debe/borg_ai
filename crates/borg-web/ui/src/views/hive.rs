//! Hive: the collective's hive goals (`borg hive`): each goal's board by status (a holder on another
//! node named with it), what stops it here (conflicting definitions, checks awaiting `borg hive
//! approve`), its integration, its reworks, its groups of attempts (winners, losers, the one integrated, what
//! reflection learned), the attempts' commits carried between nodes, the plan's intent cards and the
//! engineer's approval of them, what each drone
//! spent of its share, and the newest contracts and messages between drones. A goal of interactive drones
//! (phase g G6, borg hive/monitor.rs) shows its tasks by state over time from its first line, metered
//! against unmetered spend, its nodes (membership, last beat, the node's worker), this node's drones by
//! state with their wake latency (each opens to its wakes), the stuck and declines lines with their
//! reasons, and its integrations with the node's integration log. Above them, the collective's L1 playbooks: their steps, the belief of
//! each version drones were shown, and the edits waiting for an engineer (a playbook's first included).

use leptos::prelude::*;

use crate::api::{HiveGoal, HiveMember, HiveMonitor, Playbook, data};
use crate::charts::{Row, Series, Sparkline, StackedChart};
use crate::util::{ago, clock, duration, pct, trunc, usd};

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

/// The board's states in the order a goal moves through them, each with its colour (the history chart).
const STATES: [(&str, &str); 6] = [
    ("open", "var(--c-slate)"),
    ("claimed", "var(--c-accent)"),
    ("done", "var(--c-amber)"),
    ("verified", "var(--c-good)"),
    ("blocked", "var(--c-pink)"),
    ("stuck", "var(--c-heat)"),
];

/// Interactive drones by state, in the order a reader scans them: what moves first, the left and gone last.
const DRONE_STATES: [&str; 6] = ["working", "busy", "waiting", "not waiting", "left", "gone"];

/// A drone state's badge class (index.html `.badge`).
fn badge(state: &str) -> &'static str {
    match state {
        "working" => "badge working",
        "waiting" => "badge waiting",
        "busy" => "badge paused",
        "not waiting" => "badge idle",
        _ => "badge",
    }
}

/// The middle of `v` (sorted in place); 0 for none.
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

/// One interactive drone: its line (harness, state, task, work, wake latency trend, spend), opening to
/// its wakes.
fn member_view(d: HiveMember) -> impl IntoView {
    let taken: Vec<f64> = d.wakes.iter().filter(|w| w.taken).map(|w| w.latency_ms).collect();
    let missed = d.wakes.iter().filter(|w| !w.taken).count();
    let last = taken.last().map_or_else(|| "no wake yet".to_string(), |l| format!("last wake {}", duration(*l)));
    let state = if d.task.is_empty() { d.state.clone() } else { format!("{} {}", d.state, d.task) };
    let spent = format!("{}{}", usd(d.usd), if d.unmetered { "+ (unmetered)" } else { "" });
    view! {
        <details class="group">
            <summary title=format!("session {} · since {}", d.session, ago(d.since_ms))>
                <span class="group-name mono">{d.drone.clone()}</span>
                <span class="badge">{d.harness.clone()}</span>
                <span class=badge(&d.state)>{state}</span>
                <span class="feed-meta">{format!("{} done · {} judged · {last}{}", d.done, d.judged, if missed > 0 { format!(" · {missed} missed") } else { String::new() })}</span>
                {(taken.len() > 1).then(|| view! { <span title="wake latency, hand to take-up"><Sparkline values=taken.clone() color="var(--c-teal)" /></span> })}
                <span class="group-meta">{spent}</span>
            </summary>
            {if d.wakes.is_empty() {
                view! { <p class="empty">"No wake yet: its tasks came at its own stops."</p> }.into_any()
            } else {
                view! {
                    <table class="mono">
                        <tr><th>"when"</th><th>"task"</th><th>"hand to take-up"</th></tr>
                        {d.wakes.iter().rev().map(|w| view! {
                            <tr>
                                <td>{clock(w.ms)}</td>
                                <td>{w.task.clone()}</td>
                                <td>{if w.taken { duration(w.latency_ms) } else { "not taken up: released".to_string() }}</td>
                            </tr>
                        }).collect_view()}
                    </table>
                }.into_any()
            }}
        </details>
    }
}

/// What a goal of interactive drones adds (borg hive/monitor.rs): nothing for a goal with no history.
fn monitor_view(m: HiveMonitor, budget: f64, spent: f64) -> impl IntoView {
    let series: Vec<Series> = STATES.iter().map(|(k, c)| Series::new(k, k, c)).collect();
    let rows: Vec<Row> = m.history.iter().map(|p| Row { label: clock(p.ms), vals: vec![p.open, p.claimed, p.done, p.verified, p.blocked, p.stuck] }).collect();
    let mut lat: Vec<f64> = m.interactive.iter().flat_map(|d| d.wakes.iter().filter(|w| w.taken).map(|w| w.latency_ms)).collect();
    let wakes = lat.len();
    let missed: usize = m.interactive.iter().map(|d| d.wakes.iter().filter(|w| !w.taken).count()).sum();
    let mid = median(&mut lat);
    let by_state: Vec<String> = DRONE_STATES
        .iter()
        .filter_map(|st| {
            let n = m.interactive.iter().filter(|d| d.state == *st).count();
            (n > 0).then(|| format!("{n} {st}"))
        })
        .collect();
    let here = m.members.iter().find(|n| n.here).cloned();
    let missed_class = if missed > 0 { "fact h" } else { "fact m" };
    view! {
        {(!rows.is_empty()).then(|| view! {
            <h3 title="the board's tasks by state, each point the fold of every line stamped up to it">"Tasks over time"</h3>
            <StackedChart rows=rows series=series height=180.0 />
        })}
        <p class="feed-meta" title="metered: what spend lines say they cost; unmetered: claims whose harness reports no usage, iterations charged an estimate">
            {format!(
                "{} of {} spent · {} metered{}",
                usd(spent),
                usd(budget),
                usd(m.metered_usd),
                if m.unmetered > 0.0 { format!(" · {} unmetered line(s), their cost unknown", m.unmetered) } else { String::new() }
            )}
        </p>
        {(!m.interactive.is_empty()).then(|| view! {
            <div class="facts">
                <div class="fact g"><span class="fact-big">{by_state.join(" · ")}</span><span class="fact-text">"drones of this node"</span></div>
                <div class="fact"><span class="fact-big">{if wakes > 0 { duration(mid) } else { "–".to_string() }}</span>
                    <span class="fact-text">{format!("median wake, hand to take-up, of {wakes} wake(s)")}</span></div>
                <div class=missed_class><span class="fact-big">{missed}</span>
                    <span class="fact-text">"wakes no turn took up (released)"</span></div>
                {here.clone().map(|n| {
                    let cls = if n.worker == "none" { "fact h" } else { "fact v" };
                    view! {
                    <div class=cls><span class="fact-big">{n.worker.clone()}</span>
                        <span class="fact-text">"does this node's goal work (beats, stuck, integration)"</span></div>
                    }
                })}
            </div>
        })}
        {(!m.members.is_empty() && !m.interactive.is_empty()).then(|| view! {
            <h3>"Nodes"</h3>
            <table class="mono">
                <tr><th>"node"</th><th>"member"</th><th>"last beat"</th><th>"last line"</th><th>"takers"</th><th>"worker"</th></tr>
                {m.members.iter().map(|n| {
                    let age = |a: Option<f64>| a.map_or_else(|| "-".to_string(), |a| format!("{} ago", duration(a)));
                    let member = match n.state.as_str() {
                        "in" => format!("in since {}", clock(n.joined_ms)),
                        "left" => format!("left {}", clock(n.left_ms)),
                        s => s.to_string(),
                    };
                    view! {
                        <tr title="ages on this node's clock: when it first held the line">
                            <td>{format!("{}{}", n.node, if n.here { " (here)" } else { "" })}</td>
                            <td>{member}</td>
                            <td>{age(n.beat_age_ms)}</td>
                            <td>{age(n.line_age_ms)}</td>
                            <td>{if n.takers.is_empty() { "-".to_string() } else { n.takers.join(", ") }}</td>
                            <td>{if n.here { n.worker.clone() } else { String::new() }}</td>
                        </tr>
                    }
                }).collect_view()}
            </table>
        })}
        {(!m.interactive.is_empty()).then(|| {
            let mut ds = m.interactive.clone();
            ds.sort_by_key(|d| (DRONE_STATES.iter().position(|s| *s == d.state).unwrap_or(DRONE_STATES.len()), d.drone.clone()));
            view! {
                <h3 title="this node's interactive drones: open one for its wakes">"Interactive drones"</h3>
                {ds.into_iter().map(member_view).collect_view()}
            }
        })}
        {(!m.stuck.is_empty() || !m.declines.is_empty()).then(|| view! {
            <h3 title="a stuck task no participant may take; a decline is a node saying it may not take a task">"Stuck and declined"</h3>
            <table class="mono">
                <tr><th>"line"</th><th>"task"</th><th>"node"</th><th>"when"</th><th>"why"</th></tr>
                {m.stuck.iter().map(|l| ("stuck", l)).chain(m.declines.iter().map(|l| ("declines", l))).map(|(k, l)| view! {
                    <tr><td>{k}</td><td>{l.task.clone()}</td><td>{l.node.clone()}</td><td>{clock(l.ms)}</td><td>{trunc(&l.why, 200)}</td></tr>
                }).collect_view()}
            </table>
        })}
        {(!m.integrations.is_empty() || !m.integrate_log.is_empty()).then(|| view! {
            <h3>"Integrations"</h3>
            <table class="mono">
                <tr><th>"task"</th><th>"opened"</th><th>"status"</th><th>"result"</th></tr>
                {m.integrations.iter().map(|i| view! {
                    <tr><td>{i.task.clone()}</td><td>{clock(i.opened_ms)}</td><td>{i.status.clone()}</td><td>{trunc(&i.result, 200)}</td></tr>
                }).collect_view()}
            </table>
            {(!m.integrate_log.is_empty()).then(|| view! {
                <details class="group">
                    <summary><span class="group-name">"Node integration log"</span><span class="group-meta">{format!("newest {} line(s)", m.integrate_log.len())}</span></summary>
                    <pre class="mono">{m.integrate_log.join("\n")}</pre>
                </details>
            })}
        })}
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
            {monitor_view(g.monitor.clone(), g.budget_usd, g.spent_usd)}
            {g.plan.clone().map(|p| {
                let how = match p.state.as_str() {
                    "approved" | "refused" => format!(
                        "{}{}",
                        if p.by.is_empty() { String::new() } else { format!(" by {}", p.by) },
                        match p.via.as_str() {
                            "yes" => " (--yes)".to_string(),
                            "ratify" => " (borg hive ratify)".to_string(),
                            _ if p.question.is_empty() => String::new(),
                            _ => format!(" ({})", p.question),
                        }
                    ),
                    // An interactive goal's plan is answered at the planning session's prompt, never by `borg queen answer`.
                    "waiting" if p.interactive => format!(" for the engineer ({}): yes or no, because ... at the planning session's prompt, or borg hive ratify", p.question),
                    "waiting" => format!(" for the engineer: borg queen answer {} a", p.question),
                    _ => String::new(),
                };
                let title = if p.interactive {
                    "the engineer approves the plan before any drone claims a task of it"
                } else {
                    "the engineer approves the plan before any drone takes a task"
                };
                view! {
                    <h3 title=title>{format!("Plan {}{how}", p.state)}</h3>
                    {(!p.answer.is_empty()).then(|| view! { <p class="feed-meta">{format!("“{}”", trunc(&p.answer, 200))}</p> })}
                    <table class="mono">
                        <tr><th>"task"</th><th>"intent"</th><th>"files"</th><th>"after"</th><th>"risk"</th></tr>
                        {p.cards.iter().map(|c| view! {
                            <tr title=c.why.clone()>
                                <td>{c.task.clone()}</td>
                                <td>{trunc(&c.intent, 80)}</td>
                                <td>{c.files.join(", ")}</td>
                                <td>{c.deps.join(", ")}</td>
                                <td>{if c.risk == "high" { format!("high: {}", c.why) } else { c.risk.clone() }}</td>
                            </tr>
                        }).collect_view()}
                    </table>
                }
            })}
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
            {(!g.commits.is_empty()).then(|| view! {
                <h3>"Commits"</h3>
                <table class="mono">
                    <tr><th>"task"</th><th>"head"</th><th>"from"</th><th>"size"</th><th>"here"</th></tr>
                    {g.commits.iter().map(|c| view! {
                        <tr title="an attempt's commits, carried between nodes as a git bundle">
                            <td>{c.task.clone()}</td>
                            <td>{c.head.clone()}</td>
                            <td>{if c.node.is_empty() { "this node".to_string() } else { format!("node {}", c.node) }}</td>
                            <td>{format!("{:.0} KiB", (c.bytes / 1024.0).ceil())}</td>
                            <td>{trunc(&c.state, 160)}</td>
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
                    let more = if d.unmetered { "+" } else { "" };
                    // An interactive drone has no share: its claims are charged to the goal's budget.
                    let spent = if d.interactive { format!("${:.2}{more}", d.usd) } else { format!("${:.2}/${:.2}{more}", d.usd, d.share) };
                    view! {
                        <div class="feed-meta" title=format!("{} iteration(s), last {}", d.iters, d.last_exit)>
                            <span class="mono">{format!("{}  {spent}  {} iter  {}", d.drone, d.iters, d.last_exit)}</span>
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
