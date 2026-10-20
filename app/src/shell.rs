use leptos::prelude::*;
use leptos_router::components::Outlet;
use leptos_router::hooks::{use_location, use_params_map};

use crate::widgets::{AccountMenu, Flash, Live};
use crate::cache::{self, when, Ready};
use crate::{api, A};

/// The sidebar: a path under the server, its label, and whether it sits under Events.
const NAV: &[(&str, &str, bool)] = &[
    ("", "Schedule", false),
    ("/events", "Events", false),
    ("/upcoming", "Upcoming", true),
    ("/drafts", "Drafts", true),
    ("/scheduled", "Scheduled", true),
    ("/recurring", "Recurring", false),
    ("/routines", "Routines", false),
    ("/archive", "Archive", false),
    ("/presets", "Presets", false),
    ("/channels", "Channels", false),
    ("/commands", "Command desk", false),
    ("/activity", "Activity", false),
    ("/settings", "Settings", false),
];

#[component]
pub fn Brand() -> impl IntoView {
    view! {
        <div class="brand">
            <img class="mark" src=api::site().mark alt=""/>
            <p>"Marian"<em>"the discord scheduler"</em></p>
        </div>
    }
}

/// The frame around every server page: sidebar, notices, then the page.
#[component]
pub fn ServerShell() -> impl IntoView {
    let me = cache::listen(cache::me_scope(), || ());
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let membership = cache::listen(cache::membership_scope(), cache::server_key);
    let params = use_params_map();
    let server_id = Memo::new(move |_| params.with(|params| params.get("server").unwrap_or_default()));
    let server_title = use_context::<crate::ServerTitle>().map(|title| title.0);
    on_cleanup(move || {
        if let Some(title) = server_title {
            title.set(String::new());
        }
    });
    let new_build = RwSignal::new(false);
    provide_context(crate::widgets::NewBuild(new_build));
    view! {
        <a class="skip" href="#content">"Skip to content"</a>
        <div class="app">
            <aside class="sidebar">
                <Brand/>
                <div class="sidebar-body">
                    {when(
                        move || (me.get(), server.get(), membership.get()).ready(),
                        move |(me, server, membership)| {
                            if let Some(title) = server_title {
                                title.set(server.name.clone());
                            }
                            let base = format!("/servers/{}", server.id);
                            view! {
                                <Switcher me server=server.clone()/>
                                <Nav base=base.clone() counts=server.counts/>
                                <AccountMenu base membership/>
                            }
                        },
                    )}
                </div>
            </aside>
            <main class="main" id="content">
                {move || new_build.get().then(|| view! {
                    <div class="update-banner" role="status">
                        <span>"Marian has been updated. Refresh to use the new version."</span>
                        <button type="button" class="primary"
                            on:click=|_| { let _ = leptos::prelude::window().location().reload(); }>"Refresh"</button>
                        <button type="button" class="ghost" on:click=move |_| new_build.set(false)>"Later"</button>
                    </div>
                })}
                <Flash/>
                {move || view! { <Live server=server_id.get()/> }}
                <div class="page">
                    <Outlet/>
                </div>
            </main>
        </div>
    }
}

#[component]
fn Switcher(me: marian_types::Me, server: marian_types::Server) -> impl IntoView {
    let invite = api::site().invite_url;
    let now = chrono::Utc::now();
    let zone = server
        .timezone
        .parse::<chrono_tz::Tz>()
        .map(|tz| marian_types::time::zone_label(tz, now))
        .unwrap_or_else(|_| server.timezone.clone());
    let servers = me
        .servers
        .into_iter()
        .map(|other| {
            let current = other.id == server.id;
            let initial = other.name.chars().next().unwrap_or('?').to_uppercase().to_string();
            view! {
                <A href=format!("/servers/{}", other.id) attr:class=if current { "current" } else { "" }>
                    <span class="switcher-mark">{initial}</span>
                    <span class="switcher-label">{other.name}</span>
                    {current.then(|| view! { <span class="switcher-check" aria-hidden="true">"✓"</span> })}
                </A>
            }
        })
        .collect_view();
    view! {
        <details class="switcher">
            <summary>
                <span>
                    <strong>{server.name}</strong>
                    <span class="switcher-zone">{zone}</span>
                </span>
                <span class="switcher-chevron" aria-hidden="true"></span>
            </summary>
            <div class="switcher-menu">
                {servers}
                {(!invite.is_empty()).then(|| view! { <a class="switcher-add" href=invite.clone()>"+ Add server"</a> })}
            </div>
        </details>
    }
}

#[component]
fn Nav(base: String, counts: marian_types::Counts) -> impl IntoView {
    let pathname = use_location().pathname;
    let links = NAV
        .iter()
        .map(|(path, label, sub)| {
            let href = format!("{base}{path}");
            let nested = format!("{href}/");
            let here_href = href.clone();
            let schedule = path.is_empty();
            let count = match *path {
                "/upcoming" => Some(counts.upcoming),
                "/drafts" => Some(counts.drafts),
                "/scheduled" => Some(counts.scheduled),
                "/recurring" => Some(counts.series),
                "/routines" => Some(counts.routines),
                _ => None,
            };
            let sub = *sub;
            view! {
                <A href=href exact=schedule attr:class=move || {
                    let here = pathname.get();
                    // Schedule is the server's front page; every event page sits under Events.
                    let on = here == here_href || (!schedule && here.starts_with(&nested));
                    match (on, sub) {
                        (true, true) => "nav-sub active",
                        (false, true) => "nav-sub",
                        (true, false) => "active",
                        (false, false) => "",
                    }
                }>
                    {*label}" "{count.map(|n| view! { <span class="count">{n}</span> })}
                </A>
            }
        })
        .collect_view();
    view! { <nav class="nav">{links}</nav> }
}
