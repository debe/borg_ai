//! Drones: the agent sessions of the collective, local and remote, live.

use leptos::prelude::*;

use crate::api::data;
use crate::util::{ago, trunc, who};

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
                    ds.into_iter().map(|x| view! {
                        <article class=format!("drone {}", x.state)>
                            <div class="drone-head">
                                <span class="drone-name">{x.designation.clone()}</span>
                                <span class=format!("badge {}", x.state)>{x.state.clone()}</span>
                                {x.paused.then(|| view! { <span class="badge paused">"paused"</span> })}
                                {x.remote.then(|| view! { <span class="badge remote">"remote"</span> })}
                                <span class=move || format!("badge mode-{}", d.mode()) title="the collective's delivery mode">{move || d.mode()}</span>
                            </div>
                            <div class="drone-who">{format!("{} · {} · {}", who(&x.engineer), if x.repo.is_empty() { "?" } else { &x.repo }, x.harness)}</div>
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
