//! Messages: what sessions told and asked each other (borg msg.rs), as a chat. The left pane lists
//! the conversations (a pair of participants either way, or one broadcast address), the right pane
//! the open one's thread: day separators, grouped bubbles, quoted replies and the command that
//! answers each message. Messages from this machine sit on the right. Filters narrow both panes;
//! refusals sit in a list under the tiles. The messages to the engineer the open thread shows are
//! acknowledged as theirs, as `borg inbox` acknowledges what it lists. Read-only: nothing is sent.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::time::Duration;

use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::json;

use crate::api::{Message, data, post};
use crate::charts::{StackedChart, Tile};
use crate::feed::FeedList;
use crate::util::{ago, clock, day_label, hue, initials, local_day_key, num, trunc};
use crate::views::overview::{day_rows, kind_series};

const KINDS: [&str; 3] = ["msg_sent", "msg_delivered", "msg_dropped"];
/// Display: a new bubble group starts after this much silence from the same sender (10 min).
const GROUP_GAP_MS: f64 = 600_000.0;

/// One participant of a conversation, from the newest message that names it.
#[derive(Clone, Debug, PartialEq)]
struct Side {
    key: String,
    name: String,
    engineer: String,
    repo: String,
    harness: String,
    state: String,
    /// Whether it sends from this machine; `None` when it never sent in this conversation.
    mine: Option<bool>,
}

/// A conversation: the messages of one pair of participants, or of one broadcast address.
#[derive(Clone, Debug, PartialEq)]
struct Conv {
    key: String,
    title: String,
    broadcast: bool,
    /// Ascending by `ms`.
    msgs: Vec<Message>,
    last_ms: f64,
    unread: usize,
    asks: usize,
    repos: Vec<String>,
    people: Vec<Side>,
}

/// The side `key` as its newest message names it: as a sender when it sent, else as a recipient.
fn side(msgs: &[Message], key: &str) -> Side {
    if let Some(m) = msgs.iter().rev().find(|m| m.from_key == key) {
        return Side {
            key: key.to_string(),
            name: m.from.clone(),
            engineer: m.from_engineer.clone(),
            repo: m.from_repo.clone(),
            harness: m.harness.clone(),
            state: m.from_state.clone(),
            mine: Some(m.mine),
        };
    }
    let m = msgs.iter().rev().find(|m| m.to_key == key);
    Side {
        key: key.to_string(),
        name: m.map(|m| m.to.clone()).unwrap_or_else(|| key.to_string()),
        engineer: m.map(|m| m.to_engineer.clone()).unwrap_or_default(),
        repo: m.map(|m| m.to_repo.clone()).unwrap_or_default(),
        harness: String::new(),
        state: m.map(|m| m.to_state.clone()).unwrap_or_default(),
        mine: None,
    }
}

fn conv(key: String, mut msgs: Vec<Message>) -> Conv {
    msgs.sort_by(|a, b| a.ms.total_cmp(&b.ms));
    let first = &msgs[0];
    let broadcast = first.to_kind == "all";
    let title = if broadcast { format!("# {}", first.to) } else { format!("{} ↔ {}", first.from, first.to) };
    let people = if broadcast {
        let senders: BTreeSet<&str> = msgs.iter().map(|m| m.from_key.as_str()).collect();
        senders.into_iter().map(|k| side(&msgs, k)).collect()
    } else {
        vec![side(&msgs, &first.from_key), side(&msgs, &first.to_key)]
    };
    let repos: BTreeSet<&str> = msgs.iter().flat_map(|m| [m.from_repo.as_str(), m.to_repo.as_str()]).filter(|r| !r.is_empty()).collect();
    Conv {
        title,
        broadcast,
        last_ms: msgs.last().map_or(0.0, |m| m.ms),
        unread: msgs.iter().filter(|m| m.unread > 0.0).count(),
        asks: msgs.iter().filter(|m| m.kind == "ask").count(),
        repos: repos.into_iter().map(str::to_string).collect(),
        people,
        key,
        msgs,
    }
}

fn status_label(m: &Message) -> String {
    match m.status.as_str() {
        "broadcast" if m.unread > 0.0 => format!("shown to {} here, {} still unread", num(m.shown_to), num(m.unread)),
        "broadcast" => format!("shown to {} session{} here", num(m.shown_to), if m.shown_to == 1.0 { "" } else { "s" }),
        "engineer" => "to the engineer".into(),
        "read" => "read".into(),
        "unread" => "unread".into(),
        "missed" => "never read: the session ended".into(),
        "elsewhere" => "on their machine".into(),
        _ => String::new(),
    }
}

fn avatar(key: &str, name: &str) -> impl IntoView + use<> {
    view! { <span class="avatar" style=format!("background:{}", hue(key)) title=name.to_string()>{initials(name)}</span> }
}

/// A `.seg` button group over a `&'static str` signal.
fn seg(sig: RwSignal<&'static str>, opts: &'static [(&'static str, &'static str)]) -> impl IntoView {
    view! {
        <div class="seg">
            {opts.iter().map(|&(v, l)| view! { <button class:on=move || sig.get() == v on:click=move |_| sig.set(v)>{l}</button> }).collect_view()}
        </div>
    }
}

#[component]
pub fn Messages() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let refused = Memo::new(move |_| d.feed.with(|f| f.iter().filter(|e| e.kind == "msg_dropped").cloned().collect::<Vec<_>>()));
    // Gates for the sections built once: they rebuild only when these flip, not on every update.
    let has_refused = Memo::new(move |_| refused.with(|r| !r.is_empty()));
    let has_msgs = Memo::new(move |_| d.messages.with(|ms| !ms.is_empty()));
    let asks = move || d.messages.with(|ms| ms.iter().filter(|m| m.kind == "ask").count());

    let query = RwSignal::new(String::new());
    let kind = RwSignal::new("all");
    let rcpt = RwSignal::new("all");
    let origin = RwSignal::new("all");
    let repo = RwSignal::new(String::new());
    let unread_only = RwSignal::new(false);
    let selected = RwSignal::new(None::<String>);
    let focus = RwSignal::new(None::<String>);
    let copied = RwSignal::new(None::<String>);
    let body = NodeRef::<leptos::html::Div>::new();

    let filtering = move || {
        query.with(|q| !q.trim().is_empty())
            || kind.get() != "all"
            || rcpt.get() != "all"
            || origin.get() != "all"
            || repo.with(|r| !r.is_empty())
            || unread_only.get()
    };
    let repos = Memo::new(move |_| {
        d.messages.with(|ms| {
            let set: BTreeSet<&str> = ms.iter().flat_map(|m| [m.from_repo.as_str(), m.to_repo.as_str()]).filter(|r| !r.is_empty()).collect();
            set.into_iter().map(str::to_string).collect::<Vec<_>>()
        })
    });
    let filtered = Memo::new(move |_| {
        let words: Vec<String> = query.with(|q| q.to_lowercase().split_whitespace().map(str::to_string).collect());
        let (k, r, o, u) = (kind.get(), rcpt.get(), origin.get(), unread_only.get());
        repo.with(|rp| {
            d.messages.with(|ms| {
                ms.iter()
                    .filter(|m| k == "all" || m.kind == k)
                    .filter(|m| match r {
                        "all" => true,
                        "broadcast" => m.to_kind == "all",
                        r => m.to_kind == r,
                    })
                    .filter(|m| o == "all" || m.mine == (o == "mine"))
                    .filter(|m| rp.is_empty() || m.from_repo == *rp || m.to_repo == *rp)
                    .filter(|m| !u || m.unread > 0.0)
                    .filter(|m| {
                        words.is_empty() || {
                            let hay = [&m.text, &m.from, &m.to, &m.from_engineer, &m.to_engineer, &m.from_repo, &m.to_repo, &m.harness, &m.id]
                                .map(|s| s.as_str())
                                .join(" ")
                                .to_lowercase();
                            words.iter().all(|w| hay.contains(w.as_str()))
                        }
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    });
    let convs = Memo::new(move |_| {
        let mut groups: BTreeMap<String, Vec<Message>> = BTreeMap::new();
        for m in filtered.get() {
            groups.entry(m.conversation.clone()).or_default().push(m);
        }
        let mut cs: Vec<Conv> = groups.into_iter().map(|(k, ms)| conv(k, ms)).collect();
        cs.sort_by(|a, b| b.last_ms.total_cmp(&a.last_ms));
        cs
    });
    // The open conversation: the selected one while the filters keep it, else the newest.
    let open = Memo::new(move |_| convs.with(|cs| selected.with(|s| s.as_ref().and_then(|s| cs.iter().find(|c| &c.key == s))).or_else(|| cs.first()).cloned()));
    let no_match = Memo::new(move |_| convs.with(Vec::is_empty));
    // What auto-scroll follows: the open conversation and its length.
    let tail = Memo::new(move |_| open.with(|c| c.as_ref().map(|c| (c.key.clone(), c.msgs.len()))));

    // The thread shows the engineer the messages to them: acknowledged once each (borg-web
    // `/api/messages/seen`), which opens their senders' windows again. Only what the open thread renders.
    let told = StoredValue::new(HashSet::<String>::new());
    Effect::new(move |_| {
        let ids: Vec<String> = open.with(|c| c.iter().flat_map(|c| &c.msgs).filter(|m| m.to_kind == "engineer").map(|m| m.id.clone()).collect());
        let new: Vec<String> = told.with_value(|t| ids.into_iter().filter(|i| !t.contains(i)).collect());
        if new.is_empty() {
            return;
        }
        told.update_value(|t| t.extend(new.iter().cloned()));
        // The collective these messages came from, which the picker may already have left.
        let c = d.source("messages").raw.with_untracked(|v| v["collective"].as_str().unwrap_or_default().to_string());
        spawn_local(async move {
            let _ = post(format!("/api/messages/seen?c={c}"), &json!({"ids": new})).await;
        });
    });
    // The newest message shows on open, on live updates and when a thread body mounts; a quote
    // jump (`focus` set) scrolls to its parent instead.
    Effect::new(move |_| {
        // Read, not `track()`: a Memo is lazy and subscribes to `open` only once computed, and
        // `track()` neither computes it nor would ever be notified.
        tail.with(|_| ());
        let Some(el) = body.get() else { return };
        request_animation_frame(move || {
            if focus.try_with_untracked(Option::is_none) == Some(true) {
                el.set_scroll_top(el.scroll_height());
            }
        });
    });

    // Highlights message `id`, first opening its `conversation` when it is not the open one and
    // the filters keep it.
    let jump = move |id: String, conversation: String| {
        let elsewhere = open.with_untracked(|o| o.as_ref().is_none_or(|o| o.key != conversation));
        if elsewhere && convs.with_untracked(|cs| cs.iter().any(|c| c.key == conversation)) {
            selected.set(Some(conversation));
        }
        focus.set(Some(id.clone()));
        // After the thread has rendered the parent.
        let target = format!("msg-{id}");
        request_animation_frame(move || {
            if let Some(el) = document().get_element_by_id(&target) {
                el.scroll_into_view();
            }
        });
        // `try_update`: harmless when the view is gone by then.
        set_timeout(
            move || {
                _ = focus.try_update(|f| {
                    if f.as_deref() == Some(id.as_str()) {
                        *f = None
                    }
                })
            },
            Duration::from_secs(2),
        );
    };
    let copy = move |by: String, id: String| {
        let cmd = format!("borg tell {by} --re {id} \"…\"");
        let _ = window().navigator().clipboard().write_text(&cmd);
        copied.set(Some(id.clone()));
        set_timeout(
            move || {
                _ = copied.try_update(|c| {
                    if c.as_deref() == Some(id.as_str()) {
                        *c = None
                    }
                })
            },
            Duration::from_millis(1500),
        );
    };

    let conv_row = move |c: Conv| {
        let key = c.key.clone();
        let on_key = c.key.clone();
        let senders: BTreeSet<&str> = c.msgs.iter().map(|m| m.from_key.as_str()).collect();
        let last =
            c.msgs.last().map(|m| if senders.len() > 1 { format!("{}: {}", m.from, trunc(&m.text, 90)) } else { trunc(&m.text, 90) }).unwrap_or_default();
        let engineers: BTreeSet<&str> = c.msgs.iter().flat_map(|m| [m.from_engineer.as_str(), m.to_engineer.as_str()]).filter(|e| !e.is_empty()).collect();
        let sub = c.repos.iter().map(String::as_str).chain(engineers).collect::<Vec<_>>().join(" · ");
        let avatars = if c.broadcast {
            view! { <span class="avatar" style="background:var(--c-slate)">"#"</span> }.into_any()
        } else {
            c.people.iter().take(2).map(|p| avatar(&p.key, &p.name)).collect_view().into_any()
        };
        view! {
            <button class="conv" class:on=move || open.with(|o| o.as_ref().is_some_and(|o| o.key == on_key)) on:click=move |_| selected.set(Some(key.clone()))>
                <div class="avatars">{avatars}</div>
                <div style="min-width:0;text-align:left">
                    <div class="conv-title">{c.title}</div>
                    <div class="conv-sub" style="white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{sub}</div>
                    <div class="conv-last">{last}</div>
                </div>
                <div class="conv-side">
                    <span>{ago(c.last_ms)}</span>
                    <span class="count">{c.msgs.len()}</span>
                    {(c.unread > 0).then(|| view! { <span class="unread">{c.unread}</span> })}
                    {(c.asks > 0).then(|| view! { <span class="chip v">"ask"</span> })}
                </div>
            </button>
        }
    };

    let thread_head = move |c: &Conv| {
        let head = c
            .people
            .iter()
            .map(|p| {
                let meta = [p.engineer.as_str(), p.repo.as_str(), p.harness.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let state = (!p.state.is_empty()).then(|| view! { <span class=format!("badge {}", p.state)>{p.state.clone()}</span> });
                let place = p.mine.map(|mine| {
                    if mine {
                        view! { <span class="chip k">"this machine"</span> }.into_any()
                    } else {
                        view! { <span class="chip">"teammate"</span> }.into_any()
                    }
                });
                view! {
                    <div class="who">
                        {avatar(&p.key, &p.name)}
                        <div>
                            <b>{p.name.clone()}</b>
                            <div class="conv-sub">{meta}</div>
                        </div>
                        {state}
                        {place}
                    </div>
                }
            })
            .collect_view();
        let n = c.msgs.len();
        view! { <div class="thread-head">{head}<small style="margin-left:auto">{format!("{n} message{}", if n == 1 { "" } else { "s" })}</small></div> }
    };

    let thread_rows = move |c: &Conv| {
        let mut prev: Option<&Message> = None;
        c.msgs
            .iter()
            .map(|m| {
                let new_day = prev.is_none_or(|p| local_day_key(p.ms) != local_day_key(m.ms));
                let start = new_day || prev.is_some_and(|p| p.from_key != m.from_key || m.ms - p.ms > GROUP_GAP_MS);
                prev = Some(m);
                let sep = new_day.then(|| view! { <div class="day-sep"><span>{day_label(m.ms)}</span></div> });
                let quote = (!m.re.is_empty()).then(|| {
                    let parent = d.messages.with(|ms| {
                        ms.iter().find(|p| p.id == m.re).map(|p| (format!("↩ {}: {}", p.from, trunc(&p.text, 140)), p.conversation.clone()))
                    });
                    match parent {
                        Some((label, conversation)) => {
                            let re = m.re.clone();
                            view! { <button class="quote" on:click=move |_| jump(re.clone(), conversation.clone())>{label}</button> }.into_any()
                        }
                        None => view! { <span class="quote">{format!("↩ reply to {}", m.re)}</span> }.into_any(),
                    }
                });
                let ask = m.kind == "ask";
                let meta = [clock(m.ms), m.harness.clone(), status_label(m)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                let (hl_id, copied_id, by, copy_id) = (m.id.clone(), m.id.clone(), m.by.clone(), m.id.clone());
                // A drone's label names its engineer already.
                let who = if !m.from_engineer.is_empty() && !m.from.starts_with(&m.from_engineer) { format!("{} · {}", m.from, m.from_engineer) } else { m.from.clone() };
                view! {
                    {sep}
                    <div class="msg" class:mine=m.mine class:start=start id=format!("msg-{}", m.id) class:hl=move || focus.with(|f| f.as_deref() == Some(hl_id.as_str()))>
                        {if start { avatar(&m.from_key, &m.from).into_any() } else { view! { <span class="avatar-gap"></span> }.into_any() }}
                        <div class="bubble" class:ask=ask>
                            {start.then(|| view! { <div class="bubble-who">{who}</div> })}
                            {quote}
                            {ask.then(|| view! { <span class="chip v">"asks for a reply"</span> })}
                            <div class="bubble-text">{m.text.clone()}</div>
                            <div class="bubble-meta">
                                <span>{meta}</span>
                                <button class="copy" title="copy the reply command" on:click=move |_| copy(by.clone(), copy_id.clone())>
                                    {move || if copied.with(|c| c.as_deref() == Some(copied_id.as_str())) { "copied" } else { "copy" }}
                                </button>
                            </div>
                        </div>
                    </div>
                }
            })
            .collect_view()
    };

    view! {
        <section class="tiles">
            <Tile label="Messages sent" value=Signal::derive(move || num(t().n("msg_sent"))) sub=Signal::derive(move || {
                let n = asks();
                format!("{n} ask{} in the last 7 days", if n == 1 { "" } else { "s" })
            }) color="var(--c-sky)" />
            <Tile label="Shown to sessions" value=Signal::derive(move || num(t().n("msg_delivered"))) sub=Signal::derive(|| "each once, at its next prompt or tool call".to_string()) color="var(--c-teal)" />
            <Tile label="Unread now" value=Signal::derive(move || num(d.unread.get())) sub=Signal::derive(|| "waiting for a running session here".to_string()) color="var(--c-violet)" />
            <Tile label="Dropped" value=Signal::derive(move || num(t().n("msg_dropped"))) sub=Signal::derive(|| "refused while their recipient had not read the ones before, or cut by the log's bound".to_string()) color="var(--c-heat)" up_is_good=false />
        </section>
        {move || has_refused.get().then(|| view! {
            <details class="group refused">
                <summary>
                    <span class="group-name">"Refused messages"</span>
                    <span class="group-count">{move || refused.with(Vec::len)}</span>
                    <span class="group-meta">"refused while their recipient had unread ones, or cut by the log's bound"</span>
                </summary>
                <div class="feed compact"><FeedList events=refused limit=40 empty="No refusal." /></div>
            </details>
        })}
        {move || {
            if d.source("messages").missing.get() {
                return Some(view! { <section class="panel"><p class="empty">"This borg web is older than messages: reinstall borg-web."</p></section> }.into_any());
            }
            (!has_msgs.get()).then(|| view! {
                <section class="panel explain">
                    <h2>"No session has sent a message yet"</h2>
                    <p>"Sessions on any harness tell and ask each other. A message is shown once to its recipient, at its next prompt or tool call, labelled as peer data with the command that answers it."</p>
                    <pre>"borg top                               # find the session
borg tell 2 \"the parser test is flaky\"
borg ask all \"who touches the mesh today?\"
borg inbox                             # what was sent, and whom it was shown to"</pre>
                </section>
            }.into_any())
        }}
        {move || has_msgs.get().then(|| view! {
            <section class="panel chat-panel">
                <h2>"Conversations"<small>{move || format!("{} of {} messages · the last 7 days", filtered.with(Vec::len), d.messages.with(Vec::len))}</small></h2>
                <div class="chat-filters">
                    <input class="search" aria-label="search messages" placeholder="search text, people, repos" prop:value=move || query.get() on:input=move |e| query.set(event_target_value(&e)) />
                    {seg(kind, &[("all", "All"), ("tell", "Tells"), ("ask", "Asks")])}
                    {seg(rcpt, &[("all", "Anyone"), ("session", "Sessions"), ("engineer", "Engineers"), ("broadcast", "Broadcasts")])}
                    {seg(origin, &[("all", "Everyone"), ("mine", "This machine"), ("team", "Teammates")])}
                    <select aria-label="repo" on:change=move |e| repo.set(event_target_value(&e)) prop:value=move || repo.get()>
                        <option value="" prop:selected=move || repo.with(String::is_empty)>"All repos"</option>
                        {move || {
                            repos
                                .get()
                                .into_iter()
                                .map(|r| {
                                    let (value, sel) = (r.clone(), r.clone());
                                    view! { <option value=value prop:selected=move || repo.with(|x| *x == sel)>{r}</option> }
                                })
                                .collect_view()
                        }}
                    </select>
                    <label class="toggle"><input type="checkbox" prop:checked=move || unread_only.get() on:change=move |e| unread_only.set(event_target_checked(&e)) />"Unread only"</label>
                    {move || filtering().then(|| view! {
                        <button class="copy" on:click=move |_| {
                            query.set(String::new());
                            kind.set("all");
                            rcpt.set("all");
                            origin.set("all");
                            repo.set(String::new());
                            unread_only.set(false);
                        }>"Clear"</button>
                    })}
                </div>
                {move || {
                    if no_match.get() {
                        return view! { <p class="empty">"No message matches these filters."</p> }.into_any();
                    }
                    view! {
                        <div class="chat">
                            <div class="chat-list">
                                <For each=move || convs.get() key=|c| (c.key.clone(), c.msgs.len(), c.last_ms.to_bits(), c.unread, c.title.clone()) children=conv_row />
                            </div>
                            <div class="chat-thread">
                                {move || open.with(|c| c.as_ref().map(thread_head))}
                                <div class="thread-body" node_ref=body>{move || open.with(|c| c.as_ref().map(thread_rows))}</div>
                            </div>
                        </div>
                    }
                    .into_any()
                }}
            </section>
        })}
        <section class="panel">
            <h2>"Messages"<small>"per day"</small></h2>
            <StackedChart rows=Signal::derive(move || st.with(|s| day_rows(s, &KINDS))) series=kind_series(&KINDS) ranges=true />
        </section>
    }
}
