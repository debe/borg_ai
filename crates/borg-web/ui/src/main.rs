//! The borg web page: the collective's knowledge and what it did, live. Data comes from borg-web's
//! JSON API; a server-sent event stream names the views whose files changed, and only those are
//! fetched again. Tabs follow the URL hash, so a view can be linked. A banner over the tabs says when
//! borg itself has news: a newer release, or a member on another mesh protocol (banner.rs).

mod api;
mod banner;
mod charts;
mod drawer;
mod feed;
mod util;
mod views;

use leptos::prelude::*;

use crate::banner::Banner;
use crate::drawer::Drawer;
use crate::views::{
    conscience::Conscience, cost::Cost, drones::Drones, knowledge::Knowledge, learning::Learning, messages::Messages, overview::Overview, queen::Queen,
    steering::Steering,
};

const TABS: [(&str, &str); 9] = [
    ("overview", "Overview"),
    ("knowledge", "Knowledge"),
    ("conscience", "Conscience"),
    ("learning", "Learning"),
    ("steering", "Steering"),
    ("messages", "Messages"),
    ("queen", "Queen"),
    ("cost", "Cost"),
    ("drones", "Drones"),
];

fn tab_now() -> String {
    let h = location_hash().unwrap_or_default();
    if TABS.iter().any(|(k, _)| *k == h) { h } else { "overview".into() }
}

#[component]
fn App() -> impl IntoView {
    let d = api::provide();
    let tab = RwSignal::new(tab_now());
    // A new tab closes the lesson drawer.
    let _route = window_event_listener(leptos::ev::hashchange, move |_| {
        tab.set(tab_now());
        d.open.set(None);
    });
    let working = move || d.drones.with(|ds| ds.iter().filter(|x| x.state == "working").count());

    view! {
        <header class="top">
            <div class="brand">
                <svg viewBox="0 0 24 24" class="logo" aria-hidden="true">
                    <path d="M12 2 21 7v10l-9 5-9-5V7z" class="logo-cube" />
                    <path d="M12 12 21 7M12 12v10M12 12 3 7" class="logo-edge" />
                    <circle cx="12" cy="12" r="2.2" class="logo-core" />
                </svg>
                <h1>"Borg Collective"</h1>
            </div>
            <select on:change=move |e| d.collective.set(event_target_value(&e)) prop:value=move || d.collective.get()>
                {move || d.collectives.get().into_iter().map(|(n, s, _)| view! { <option value=n.clone()>{format!("{n} · {s}")}</option> }).collect_view()}
            </select>
            <span class="grow"></span>
            <span class=move || format!("mode-badge {}", d.mode()) title="delivery mode: hook judges the work at the turn end, live also during the turn">
                {move || format!("{} mode", d.mode())}
            </span>
            <span class="drones-now" title="drones working right now">{move || format!("{} working", working())}</span>
            <span class=move || if d.live.get() { "live on" } else { "live" } title="live updates from the store">
                <i></i>{move || if d.live.get() { "live" } else { "offline" }}
            </span>
        </header>
        <Banner />
        <nav class="tabs">
            {TABS.iter().map(|(k, label)| {
                let k = *k;
                view! { <a href=format!("#{k}") class:on=move || tab.get() == k>{*label}</a> }
            }).collect_view()}
        </nav>
        <main>
            {move || match tab.get().as_str() {
                "knowledge" => view! { <Knowledge /> }.into_any(),
                "conscience" => view! { <Conscience /> }.into_any(),
                "learning" => view! { <Learning /> }.into_any(),
                "steering" => view! { <Steering /> }.into_any(),
                "messages" => view! { <Messages /> }.into_any(),
                "queen" => view! { <Queen /> }.into_any(),
                "cost" => view! { <Cost /> }.into_any(),
                "drones" => view! { <Drones /> }.into_any(),
                _ => view! { <Overview /> }.into_any(),
            }}
        </main>
        <Drawer />
    }
}

fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
