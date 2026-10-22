//! The event feed: what happened, newest first, in words.

use leptos::prelude::*;

use crate::api::{FeedEvent, data};
use crate::util::{ago, duration, kind, num, trunc, usd, who};

/// A one-line headline and a detail line for an event.
pub fn describe(e: &FeedEvent) -> (String, String) {
    let w = who(&e.engineer);
    let detail = e.text.clone();
    let head = match e.kind.as_str() {
        "correction" if e.why == "rejected_tool" => format!("{w} rejected a tool call"),
        "correction" => format!("{w} corrected the drone"),
        "veto" => format!("{w} said no to a drone's work"),
        "veto_repeat" => "A veto repeated an earlier one: the lesson did not hold".into(),
        "lesson_new" => match e.why.as_str() {
            "import" => "A lesson was imported from team documents".into(),
            "queen" => "A queen answer became a lesson".into(),
            "review" => "Borg learned a new lesson from a review finding".into(),
            "regret" => "Borg learned a new lesson from a later fix".into(),
            "veto" => "Borg learned a new lesson from a veto".into(),
            _ => "Borg learned a new lesson from a correction".into(),
        },
        "lesson_merged" => "A new correction sharpened a lesson".into(),
        "review_finding" => "A review step found a bug and a fix step fixed it, before any commit".into(),
        "not_a_lesson" => "A correction was judged not to be a lesson".into(),
        "block" => format!("Borg asked {} question{} before the drone finished", num(e.n.max(1.0)), if e.n == 1.0 { "" } else { "s" }),
        "changed" => "A question changed the drone's work".into(),
        "ignored" => "The drone ignored a question".into(),
        "corrected" => "Asked, and still corrected afterwards".into(),
        "not_corrected" => "Asked, and no correction followed".into(),
        "regretted" => "A later fix counted against a question asked before the decision".into(),
        "early" => format!("{} lesson{} handed over early, free", num(e.n.max(1.0)), if e.n == 1.0 { "" } else { "s" }),
        "steer_issued" => format!("{w} steered drones"),
        "steer_delivered" => format!("A drone received a steer ({})", if e.why.is_empty() { "say" } else { &e.why }),
        "steer_dropped" => "A steer was refused".into(),
        "pause_refused" => format!("A paused drone was stopped from running {}", if e.why.is_empty() { "a tool" } else { &e.why }),
        "queen_queued" => "The queen queued a decision".into(),
        "queen_delivered" => "A drone asked the queen's question".into(),
        "queen_answered" => {
            if e.dur_ms > 0.0 {
                format!("{w} answered the queen after {}", duration(e.dur_ms))
            } else {
                format!("{w} answered the queen")
            }
        }
        "queen_skipped" => format!("{w} skipped a queen question"),
        "queen_dismissed" => format!("{w} dismissed a queen question"),
        "assimilated" => format!("Imported {} lesson{}", num(e.n), if e.n == 1.0 { "" } else { "s" }),
        "check" if e.why == "failed" => "A turn-end check got no answer".into(),
        "check" => format!("Turn-end check over {} lessons", num(e.n)),
        "gate_skip" => "A turn end passed the free gate".into(),
        "model_call" => format!("{} model call", if e.why.is_empty() { "A" } else { &e.why }),
        "turn_end" => "A drone finished a turn".into(),
        other => kind(other).0.to_string(),
    };
    let detail = if detail.is_empty() && e.kind == "steer_dropped" { e.why.clone() } else { detail };
    (head, detail)
}

/// Feed rows; items newer than the last refresh glow briefly.
#[component]
pub fn FeedList(
    #[prop(into)] events: Signal<Vec<FeedEvent>>,
    #[prop(default = 12)] limit: usize,
    #[prop(default = "Nothing has happened yet.")] empty: &'static str,
) -> impl IntoView {
    let d = data();
    move || {
        let evs = events.get();
        if evs.is_empty() {
            let msg = if d.source("feed").missing.get() { "This borg web is older than the feed: reinstall borg-web." } else { empty };
            return view! { <p class="empty">{msg}</p> }.into_any();
        }
        let seen = d.seen.get();
        evs.into_iter()
            .take(limit)
            .map(|e| {
                let (label, color, glyph) = kind(&e.kind);
                let (head, detail) = describe(&e);
                let lesson = e.lessons.first().and_then(|id| d.lesson(id));
                let fresh = seen > 0.0 && e.ms > seen;
                let meta = [
                    (!e.repo.is_empty()).then(|| e.repo.clone()),
                    Some(ago(e.ms)),
                    (e.usd > 0.0).then(|| usd(e.usd)),
                    (e.lessons.len() > 1).then(|| format!("{} lessons", e.lessons.len())),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                view! {
                    <div class="feed-row" class:new=fresh>
                        <span class="glyph" style=format!("background:{color}") title=label>{glyph}</span>
                        <div class="feed-body">
                            <div class="feed-head">{head}</div>
                            {(!detail.is_empty()).then(|| view! { <div class="feed-detail">{trunc(&detail, 220)}</div> })}
                            {lesson.map(|l| {
                                let id = l.id.clone();
                                view! { <button class="feed-lesson" on:click=move |_| d.open.set(Some(id.clone()))>{format!("“{}”", trunc(&l.question, 140))}</button> }
                            })}
                            <div class="feed-meta">{meta}</div>
                        </div>
                    </div>
                }
            })
            .collect_view()
            .into_any()
    }
}
