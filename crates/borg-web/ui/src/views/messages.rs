//! Messages: what sessions told and asked each other (borg msg.rs), whom each was shown to on this
//! node, what waits unread, and the sends, deliveries and refusals per day.

use leptos::prelude::*;

use crate::api::{Message, data};
use crate::charts::{StackedChart, Tile};
use crate::feed::FeedList;
use crate::util::{ago, num, trunc};
use crate::views::overview::{day_rows, kind_series};

const KINDS: [&str; 3] = ["msg_sent", "msg_delivered", "msg_dropped"];

fn message_row(m: Message) -> impl IntoView {
    let ask = m.kind == "ask";
    let seen = match (m.to_kind.as_str(), m.shown_to) {
        ("all", n) => format!("shown to {} session{}", num(n), if n == 1.0 { "" } else { "s" }),
        ("engineer", _) => "to the engineer".into(),
        (_, n) if n > 0.0 => "shown".into(),
        _ => "unread".into(),
    };
    let meta = [Some(ago(m.ms)), (!m.harness.is_empty()).then(|| m.harness.clone()), Some(seen), (!m.re.is_empty()).then(|| format!("re {}", m.re))]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    view! {
        <div class="feed-row">
            <span class="glyph" style=if ask { "background:var(--c-violet)" } else { "background:var(--c-sky)" } title=m.kind.clone()>{if ask { "?" } else { "✉" }}</span>
            <div class="feed-body">
                <div class="feed-head">{format!("{} {} {}", m.from, if ask { "asked" } else { "told" }, m.to)}</div>
                <div class="feed-detail">{trunc(&m.text, 400)}</div>
                <div class="feed-meta" title=m.id.clone()>{meta}</div>
            </div>
        </div>
    }
}

#[component]
pub fn Messages() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let timeline = Memo::new(move |_| d.feed.with(|f| f.iter().filter(|e| KINDS.contains(&e.kind.as_str())).cloned().collect::<Vec<_>>()));
    let asks = move || d.messages.with(|ms| ms.iter().filter(|m| m.kind == "ask").count());

    view! {
        <section class="tiles">
            <Tile label="Messages sent" value=Signal::derive(move || num(t().n("msg_sent"))) sub=Signal::derive(move || {
                let n = asks();
                format!("{n} ask{} in the last 7 days", if n == 1 { "" } else { "s" })
            }) color="var(--c-sky)" />
            <Tile label="Shown to sessions" value=Signal::derive(move || num(t().n("msg_delivered"))) sub=Signal::derive(|| "each once, at its next prompt or tool call".to_string()) color="var(--c-teal)" />
            <Tile label="Unread now" value=Signal::derive(move || num(d.unread.get())) sub=Signal::derive(|| "waiting for a running session here".to_string()) color="var(--c-violet)" />
            <Tile label="Dropped" value=Signal::derive(move || num(t().n("msg_dropped"))) sub=Signal::derive(|| "refused by the rate limit, or cut by the log's bound".to_string()) color="var(--c-heat)" up_is_good=false />
        </section>
        <section class="panel">
            <h2>"Messages"<small>"per day"</small></h2>
            <StackedChart rows=Signal::derive(move || st.with(|s| day_rows(s, &KINDS))) series=kind_series(&KINDS) ranges=true />
        </section>
        {move || d.messages.with(Vec::is_empty).then(|| view! {
            <section class="panel explain">
                <h2>"No session has sent a message yet"</h2>
                <p>"Sessions on any harness tell and ask each other. A message is shown once to its recipient, at its next prompt or tool call, labelled as peer data with the command that answers it."</p>
                <pre>"borg top                               # find the session
borg tell \"Two of Five\" \"the parser test is flaky\"
borg ask all \"who touches the mesh today?\"
borg inbox                             # what was sent, and whom it was shown to"</pre>
            </section>
        })}
        <section class="two">
            <div class="panel">
                <h2>"Conversation"<small>"newest first, the last 7 days"</small></h2>
                <div class="feed">
                    {move || {
                        if d.source("messages").missing.get() {
                            return view! { <p class="empty">"This borg web is older than messages: reinstall borg-web."</p> }.into_any();
                        }
                        let ms = d.messages.get();
                        if ms.is_empty() {
                            return view! { <p class="empty">"No message in the last 7 days."</p> }.into_any();
                        }
                        ms.into_iter().take(60).map(message_row).collect_view().into_any()
                    }}
                </div>
            </div>
            <div class="panel">
                <h2>"Timeline"<small>"sends and refusals on this machine"</small></h2>
                <div class="feed"><FeedList events=timeline limit=40 empty="No message sent yet." /></div>
            </div>
        </section>
    }
}
