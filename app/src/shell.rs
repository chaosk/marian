use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::components::Outlet;
use leptos_router::hooks::use_location;

use crate::islands::{AccountMenu, Flash};
use crate::{api, base, load, param};

const NAV: &[(&str, &str)] = &[
    ("", "Schedule"),
    ("/drafts", "Drafts"),
    ("/scheduled", "Scheduled"),
    ("/recurring", "Recurring"),
    ("/routines", "Routines"),
    ("/archive", "Archive"),
    ("/presets", "Presets"),
    ("/channels", "Channels"),
    ("/commands", "Command desk"),
    ("/activity", "Activity"),
    ("/settings", "Settings"),
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
    let server = param("server");
    let notice = api::take_notice();
    let data = async move {
        let me = api::me().await?;
        let here = api::server(&server).await?;
        let membership = api::membership(&server).await?;
        Ok((me, here, membership))
    };
    load(data, move |(me, server, membership)| {
        view! {
            <Title text=format!("{} · Marian", server.name)/>
            <a class="skip" href="#content">"Skip to content"</a>
            <div class="app">
                <aside class="sidebar">
                    <Brand/>
                    <Switcher me server=server.clone()/>
                    <Nav counts=server.counts/>
                    <AccountMenu base=base() membership/>
                </aside>
                <main class="main" id="content">
                    <Flash notice/>
                    <Outlet/>
                </main>
            </div>
        }
    })
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
                <a href=format!("/servers/{}", other.id) class=if current { "current" } else { "" }>
                    <span class="switcher-mark">{initial}</span>
                    <span class="switcher-label">{other.name}</span>
                    {current.then(|| view! { <span class="switcher-check" aria-hidden="true">"✓"</span> })}
                </a>
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
fn Nav(counts: marian_types::Counts) -> impl IntoView {
    let base = base();
    let here = use_location().pathname.get_untracked();
    let links = NAV
        .iter()
        .map(|(path, label)| {
            let href = format!("{base}{path}");
            let active = if path.is_empty() {
                here == href || here.starts_with(&format!("{href}/events"))
            } else {
                here == href || here.starts_with(&format!("{href}/"))
            };
            let count = match *path {
                "/drafts" => Some(counts.drafts),
                "/scheduled" => Some(counts.scheduled),
                "/recurring" => Some(counts.series),
                "/routines" => Some(counts.routines),
                _ => None,
            };
            view! {
                <a href=href class=if active { "active" } else { "" }>
                    {*label}" "{count.map(|n| view! { <span class="count">{n}</span> })}
                </a>
            }
        })
        .collect_view();
    view! { <nav class="nav">{links}</nav> }
}
