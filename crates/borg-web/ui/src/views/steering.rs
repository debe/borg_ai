//! Steering and refusals: engineers correcting drones, drones steered and paused, and what was refused.

use leptos::prelude::*;

use crate::api::data;
use crate::charts::{HBars, StackedChart, Tile};
use crate::feed::FeedList;
use crate::util::{ago, num, trunc, who};
use crate::views::overview::{day_rows, kind_series};

const KINDS: [&str; 5] = ["correction", "steer_issued", "steer_delivered", "steer_dropped", "pause_refused"];

#[component]
pub fn Steering() -> impl IntoView {
    let d = data();
    let st = d.stats;
    let t = move || st.with(|s| s.totals.clone());
    let timeline = Memo::new(move |_| d.feed.with(|f| f.iter().filter(|e| KINDS.contains(&e.kind.as_str())).cloned().collect::<Vec<_>>()));
    let rejected = Memo::new(move |_| d.feed.with(|f| f.iter().filter(|e| e.kind == "correction" && e.why == "rejected_tool").cloned().collect::<Vec<_>>()));
    let by_tool = Signal::derive(move || {
        let mut m: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
        rejected.with(|r| r.iter().for_each(|e| *m.entry(Rejection::parse(if e.text.is_empty() { &e.why } else { &e.text }).tool).or_default() += 1.0));
        m.into_iter().collect::<Vec<_>>()
    });
    let per_engineer =
        Signal::derive(move || st.with(|s| s.engineers.iter().map(|e| (who(&e.name), e.t.n("correction"))).filter(|x| x.1 > 0.0).collect::<Vec<_>>()));
    let steered = move || t().n("steer_issued") + t().n("steer_delivered") + t().n("pause_refused") + t().n("steer_dropped");

    view! {
        <section class="tiles">
            <Tile label="Corrections" value=Signal::derive(move || num(t().n("correction"))) sub=Signal::derive(move || format!("{} rejected tool calls in the feed", rejected.with(Vec::len))) color="var(--c-heat)" up_is_good=false />
            <Tile label="Steers sent" value=Signal::derive(move || num(t().n("steer_issued"))) sub=Signal::derive(move || format!("{} delivered to drones", num(t().n("steer_delivered")))) color="var(--c-amber)" />
            <Tile label="Paused tool calls" value=Signal::derive(move || num(t().n("pause_refused"))) sub=Signal::derive(|| "refused while a drone was paused".to_string()) color="var(--c-pink)" />
            <Tile label="Steers refused" value=Signal::derive(move || num(t().n("steer_dropped"))) sub=Signal::derive(|| "sent by a node without a steering role".to_string()) color="var(--c-heat)" up_is_good=false />
        </section>
        <section class="panel">
            <h2>"Corrections and steering"<small>"per day"</small></h2>
            <StackedChart rows=Signal::derive(move || st.with(|s| day_rows(s, &KINDS))) series=kind_series(&KINDS) ranges=true />
        </section>
        {move || (steered() == 0.0).then(|| view! {
            <section class="panel explain">
                <h2>"No drone has been steered yet"</h2>
                <p>"Steering pauses, resumes or vetoes running sessions, yours or a teammate's. A session sees it at its next prompt or tool call; a pause holds its tool calls until resumed, and every refused call is counted here."</p>
                <pre>"borg top                                # find the drone
borg tell \"Three of Five\" \"use the shared retry helper\"
borg pause all                          # hold every drone
borg resume all"</pre>
            </section>
        })}
        <section class="two">
            <div class="panel">
                <h2>"Timeline"<small>"corrections, steers and refusals"</small></h2>
                <div class="feed"><FeedList events=timeline limit=40 empty="No corrections or steering yet." /></div>
            </div>
            <div class="stack">
                <div class="panel">
                    <h2>"Who corrects most"</h2>
                    <HBars items=per_engineer color="var(--c-heat)" />
                </div>
                <div class="panel">
                    <h2>"Rejected, by tool"<small>"what engineers most often stop"</small></h2>
                    <HBars items=by_tool color="var(--c-pink)" />
                </div>
                <div class="panel">
                    <h2>"Rejected tool calls"<small>"the engineer said no, with a reason"</small></h2>
                    {move || {
                        let r = rejected.get();
                        if r.is_empty() {
                            return view! { <p class="empty">"No rejected tool call in the feed."</p> }.into_any();
                        }
                        r.into_iter().take(15).map(|e| {
                            let rej = Rejection::parse(if e.text.is_empty() { &e.why } else { &e.text });
                            view! {
                                <div class="reject">
                                    <div class="reject-top"><span class="chip h">{rej.tool.clone()}</span><span class="feed-meta">{format!("{} · {} · {}", who(&e.engineer), if e.repo.is_empty() { "?" } else { &e.repo }, ago(e.ms))}</span></div>
                                    {(!rej.reason.is_empty()).then(|| view! { <div class="reject-why">{format!("“{}”", trunc(&rej.reason, 220))}</div> })}
                                    {(!rej.input.is_empty()).then(|| view! { <div class="reject-in">{trunc(&rej.input, 160)}</div> })}
                                </div>
                            }
                        }).collect_view().into_any()
                    }}
                </div>
            </div>
        </section>
    }
}

/// A rejected tool call's text, `<Tool> <input json>: <the engineer's reason>`, taken apart.
struct Rejection {
    tool: String,
    input: String,
    reason: String,
}

impl Rejection {
    fn parse(text: &str) -> Self {
        let text = text.trim();
        let (tool, rest) = text.split_once(' ').unwrap_or((text, ""));
        // The input is one JSON value; the reason follows it after ": ". A feed text cut short inside
        // the JSON has no reason, and its fields are read from the raw text instead.
        let mut it = serde_json::Deserializer::from_str(rest).into_iter::<serde_json::Value>();
        let (v, reason) = match it.next() {
            Some(Ok(v)) => (v, rest[it.byte_offset()..].trim_start().trim_start_matches(':').trim().to_string()),
            _ => (serde_json::Value::Null, String::new()),
        };
        const KEYS: [&str; 8] = ["command", "plan", "file_path", "path", "pattern", "url", "prompt", "description"];
        let first_line = |t: &str| t.lines().map(|l| l.trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or_default().to_string();
        let raw_field = |k: &str| {
            let at = rest.find(&format!("\"{k}\":\""))? + k.len() + 4;
            let tail = &rest[at..];
            let end = [tail.find("\\n"), tail.find('"')].into_iter().flatten().min().unwrap_or(tail.len());
            Some(tail[..end].trim_start_matches('#').trim().to_string())
        };
        let input = KEYS
            .iter()
            .find_map(|k| v[*k].as_str().map(first_line))
            .or_else(|| v["questions"][0]["question"].as_str().map(String::from))
            .or_else(|| KEYS.iter().find_map(|k| raw_field(k)))
            .unwrap_or_default();
        Rejection { tool: if tool.is_empty() { "tool".into() } else { tool.to_string() }, input, reason }
    }
}
