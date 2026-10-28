//! Playbooks: the collective's L1 playbooks (borg playbook.rs `view`): their steps, the belief of each
//! version drones were shown, and the edits waiting for an engineer (a playbook's first included).

use leptos::prelude::*;

use crate::api::{Playbook, data};
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
                    <tr><th>"version shown"</th><th>"useful"</th><th>"corrected"</th><th>"vetoed"</th><th>"regretted"</th></tr>
                    {p.versions.iter().map(|v| view! {
                        <tr>
                            <td>{v.key.clone()}</td><td>{pct(v.mean, 1.0)}</td><td>{v.corrected}</td><td>{v.vetoed}</td>
                            <td>{format!("{:.2}", v.regretted)}</td>
                        </tr>
                    }).collect_view()}
                </table>
            })}
        </div>
    }
}

#[component]
pub fn Playbooks() -> impl IntoView {
    let d = data();
    view! {
        <section>
            {move || {
                let pbs = d.playbooks.get();
                if pbs.is_empty() {
                    return view! { <p class="empty">"No playbook in this collective yet."</p> }.into_any();
                }
                view! {
                    <article class="panel">
                        <div class="kn-bar"><h2>"Playbooks"<small>"procedures learned from work whose tests went green, promoted by engineers"</small></h2></div>
                        {pbs.into_iter().map(playbook_view).collect_view()}
                    </article>
                }.into_any()
            }}
        </section>
    }
}
