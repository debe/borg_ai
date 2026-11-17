//! Drones: the agent sessions of the collective, local and remote, live; a drone that offered its work,
//! took a hand-off over, or whose taken work is fenced is badged (borg handoff/).

use leptos::prelude::*;

use crate::api::data;
use crate::util::{ago, num, trunc, who};

#[component]
pub fn Drones() -> impl IntoView {
    let d = data();
    let show_ended = RwSignal::new(false);
    let count = move |s: &'static str| d.drones.with(|ds| ds.iter().filter(|x| x.state == s).count());
    view! {
        <section class="panel">
            <div class="kn-bar">
                <h2>"Drones"<small>{move || format!("{} working · {} waiting · {} idle · {} ended", count("working"), count("waiting"), count("idle"), count("ended"))}</small></h2>
                <label class="toggle"><input type="checkbox" prop:checked=move || show_ended.get() on:change=move |e| show_ended.set(event_target_checked(&e)) />" show ended"</label>
            </div>
            <div class="drones">
                {move || {
                    let ds: Vec<_> = d.drones.with(|ds| ds.iter().filter(|x| show_ended.get() || x.state != "ended").cloned().collect());
                    if ds.is_empty() {
                        return view! { <p class="empty">"No drone is running. Start an agent session in a repo of this collective."</p> }.into_any();
                    }
                    let hs = d.handoffs.with(|h| h.clone());
                    // A drone's node as hand-offs name it: this one's key for a local drone.
                    let node_of = |x: &crate::api::Drone| if x.node.is_empty() { hs.here.clone() } else { x.node.clone() };
                    let badges = |x: &crate::api::Drone| -> Vec<(&'static str, String, String)> {
                        let n = node_of(x);
                        hs.list.iter().filter_map(|h| {
                            let took = h.by.as_ref().is_some_and(|(_, bn, bs)| *bn == n && *bs == x.session);
                            let offered = h.node == n && h.session == x.session;
                            match (offered, took, h.state.as_str()) {
                                (_, true, _) => Some(("answered", format!("took {}", h.id), format!("took hand-off {} over: {}", h.id, h.note))),
                                (true, _, "open" | "asked" | "declined") => Some(("waiting", format!("offers {}", h.id), format!("offers its work as hand-off {}: {}", h.id, h.note))),
                                (true, _, _) if h.fenced => Some(("paused", "handed off".into(), format!("hand-off {} was taken: its edits in {} are refused", h.id, h.repos.join(", ")))),
                                _ => None,
                            }
                        }).collect()
                    };
                    ds.into_iter().map(|x| view! {
                        <article class=format!("drone {}", x.state)>
                            <div class="drone-head">
                                <span class="drone-name">{x.designation.clone()}</span>
                                <span class=format!("badge {}", x.state)>{x.state.clone()}</span>
                                {x.paused.then(|| view! { <span class="badge paused">"paused"</span> })}
                                {x.remote.then(|| view! { <span class="badge remote">"remote"</span> })}
                                {badges(&x).into_iter().map(|(class, label, title)| view! { <span class=format!("badge {class}") title=title>{label}</span> }).collect_view()}
                                {x.unread.filter(|n| *n > 0.0).map(|n| view! { <span class="badge waiting" title="messages its next prompt or tool call shows (borg inbox)">{format!("{n} unread")}</span> })}
                            </div>
                            <div class="drone-who">{format!("#{} · {} · {}", num(x.number), who(&x.engineer), if x.remote { format!("on {}", x.host) } else { "here".to_string() })}</div>
                            {(!x.prompt.is_empty()).then(|| view! { <div class="drone-doing">{trunc(&x.prompt, 140)}</div> })}
                            {(!x.files.is_empty()).then(|| view! {
                                <div class="drone-files">{x.files.iter().map(|f| view! { <span class="mono">{f.rsplit('/').next().unwrap_or(f).to_string()}</span> }).collect_view()}</div>
                            })}
                            <div class="feed-meta">{format!("started {} · last heard {}", ago(x.started), ago(x.ms))}</div>
                        </article>
                    }).collect_view().into_any()
                }}
            </div>
        </section>
    }
}
