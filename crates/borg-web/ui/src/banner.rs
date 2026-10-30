//! The banner over every tab when borg itself needs the engineer (`/api/update`): a newer release
//! (borg update.rs), or a member whose mesh protocol differs from this node's, with what it misses
//! (borg protocol.rs `NOTES`). Nothing shows otherwise. The command is the one the drones offer: `borg
//! update` installs the release and refreshes borg's hooks.

use leptos::prelude::*;

use crate::api::data;
use crate::util::ago;

#[component]
pub fn Banner() -> impl IntoView {
    let d = data();
    move || {
        let u = d.update.get();
        if !u.shown() {
            return None;
        }
        let release = u.update.then(|| {
            let checked = if u.checked_ms > 0.0 { format!(" (checked {})", ago(u.checked_ms)) } else { String::new() };
            view! {
                <p>
                    <b>{format!("borg {} is released", u.latest)}</b>
                    {format!("; this machine runs {}{checked}. Run ", u.borg)}
                    <code>"borg update"</code>
                    ": it installs the release and refreshes borg's hooks."
                </p>
            }
        });
        let members: Vec<_> = u
            .differ()
            .into_iter()
            .map(|m| {
                let runs = if m.borg.is_empty() { "borg older than 0.7".to_string() } else { format!("borg {}", m.borg) };
                let (head, tail) = if m.syncs {
                    (
                        format!("{} runs {runs} on borg/mesh/{}", m.name, m.protocol),
                        if m.note.is_empty() { String::new() } else { format!(": misses {}", m.note) },
                    )
                } else {
                    // The API's note names the member again (borg protocol.rs `Mismatch::line`): said here from the protocols.
                    let who = if m.protocol > u.protocol { "update this node" } else { "it has to update" };
                    (format!("Nothing syncs with {}", m.name), format!(": it speaks borg/mesh/{}, this node borg/mesh/{}; {who}", m.protocol, u.protocol))
                };
                view! { <li><b>{head}</b>{tail}</li> }
            })
            .collect();
        let list = (!members.is_empty()).then(|| view! { <ul>{members}</ul> });
        Some(view! {
            <aside class="update-banner" role="status">
                {release}
                {list}
            </aside>
        })
    }
}
