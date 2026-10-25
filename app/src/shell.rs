use leptos::prelude::*;
use leptos_router::components::Outlet;
use leptos_router::hooks::{use_location, use_params_map};

use crate::widgets::{AccountMenu, Flash, Live};
use crate::cache::{self, when, Ready};
use crate::{api, A};

/// The sidebar: a path under the server, its label, and whether it sits under
/// Events. Upcoming, Drafts, and Scheduled are filters on the Events page.
const NAV: &[(&str, &str, bool)] = &[
    ("", "Schedule", false),
    ("/events", "Events", false),
    ("upcoming", "Upcoming", true),
    ("drafts", "Drafts", true),
    ("scheduled", "Scheduled", true),
    ("/recurring", "Recurring", false),
    ("/routines", "Routines", false),
    ("/archive", "Archive", false),
    ("/presets", "Presets", false),
    ("/channels", "Channels", false),
    ("/commands", "Command desk", false),
    ("/activity", "Activity", false),
    ("/settings", "Settings", false),
];

/// The logo and name, linking to the server in the address. Away from a
/// server, it links to the one this browser opened last, once the page has
/// read that, and to the front page before then. `front` always links to the
/// front page, for a server that can't be opened.
#[component]
pub fn Brand(#[prop(optional)] front: bool) -> impl IntoView {
    // From the address, not route params: the page for an unknown address
    // matched no route and has none.
    let pathname = use_location().pathname;
    let remembered = RwSignal::new(None::<String>);
    Effect::new(move |_| remembered.set(crate::client::last_server()));
    let href = move || {
        if front {
            return "/".to_string();
        }
        pathname
            .with(|path| path.strip_prefix("/servers/").and_then(|rest| rest.split('/').next()).map(str::to_string))
            .filter(|id| !id.is_empty())
            .or_else(|| remembered.get())
            .map(|id| format!("/servers/{id}"))
            .unwrap_or_else(|| "/".into())
    };
    view! {
        <a class="brand" href=href>
            <img class="mark" src=api::site().mark alt=""/>
            <p>"Marian"<em>"the discord scheduler"</em></p>
        </a>
    }
}

/// The frame around every server page: sidebar, notices, then the page. A
/// server that isn't there, or isn't theirs, gets one page of its own instead
/// of a sidebar and panels that each say so.
#[component]
pub fn ServerShell() -> impl IntoView {
    let params = use_params_map();
    let opened = params.with_untracked(|params| params.get("server").unwrap_or_default());
    if let Some(failure) = server_gate(&opened) {
        return server_gone(&opened, failure).into_any();
    }
    let me = cache::listen(cache::me_scope(), || ());
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let membership = cache::listen(cache::membership_scope(), cache::server_key);
    let server_id = Memo::new(move |_| params.with(|params| params.get("server").unwrap_or_default()));
    let server_title = use_context::<crate::ServerTitle>().map(|title| title.0);
    on_cleanup(move || {
        if let Some(title) = server_title {
            title.set(String::new());
        }
    });
    let new_build = RwSignal::new(false);
    provide_context(crate::widgets::NewBuild(new_build));
    // A server that opened is the one to come back to. One reached by a link
    // that turns out to be missing or not theirs swaps the frame for the page
    // that says so. Effects run only once the page is live, so this starts out
    // the same on the server and in the browser and can't upset hydration.
    let lost_later = RwSignal::new(None::<crate::api::Failure>);
    Effect::new(move |_| match server.get() {
        Some(Ok(server)) => {
            crate::client::remember_server(&server.id);
            lost_later.set(None);
        }
        Some(Err(failure)) if failure.missing() => lost_later.set(Some(failure)),
        _ => {}
    });
    let showing_lost = Memo::new(move |_| lost_later.with(Option::is_some));
    (move || match showing_lost.get() {
        true => lost_later
            .get_untracked()
            .map(|failure| server_gone(&server_id.get_untracked(), failure))
            .into_any(),
        false => shell(me, server, membership, server_id, server_title, new_build).into_any(),
    })
    .into_any()
}

/// Whether this server can't be opened, decided before anything is drawn so
/// the frame never starts. The server asks the database; the browser reads
/// the answer off the page the server sent. Wrapping the frame in a
/// Transition to decide later put every page inside it, and a page whose own
/// read failed then hydrated differently from what was sent.
#[cfg(feature = "ssr")]
fn server_gate(id: &str) -> Option<crate::api::Failure> {
    crate::api::local::with(id, |_, _| Ok(()))
        .err()
        .filter(crate::api::Failure::missing)
}

#[cfg(all(feature = "hydrate", not(feature = "ssr")))]
fn server_gate(id: &str) -> Option<crate::api::Failure> {
    let status = document()
        .query_selector(&format!("[data-server-gone='{id}']"))
        .ok()
        .flatten()?
        .get_attribute("data-status")?
        .parse()
        .ok()?;
    Some(crate::api::Failure { status, message: String::new() })
}

#[cfg(not(any(feature = "ssr", feature = "hydrate")))]
fn server_gate(_: &str) -> Option<crate::api::Failure> {
    None
}

/// A full page for a server that can't be opened, with the way back to the
/// front page. One that doesn't exist and one that isn't theirs read the same.
fn server_gone(id: &str, failure: crate::api::Failure) -> impl IntoView {
    crate::status(404);
    crate::title("Missing");
    // The logo would lead back to this server, so it goes to the front page too.
    view! {
        <div data-server-gone=id.to_string() data-status=failure.status.to_string()>
            {lost("That server isn't here.", "It doesn't exist, or it isn't one you can open.", true)}
        </div>
    }
}

/// A full page for something that isn't there, with the way back to the front page.
pub(crate) fn lost(heading: &'static str, note: &'static str, front: bool) -> impl IntoView {
    view! {
        <div class="gate">
            <main class="gate-card">
                <Brand front/>
                <h1>{heading}</h1>
                <p class="lede">{note}</p>
                <div class="gate-actions">
                    <A href="/" attr:class="primary">"Back to your servers"</A>
                </div>
            </main>
        </div>
    }
}

type Read<T> = Resource<Result<T, crate::api::Failure>>;

/// The sidebar, notices, and page of a server that opened.
fn shell(
    me: Read<marian_types::Me>,
    server: Read<marian_types::Server>,
    membership: Read<marian_types::Membership>,
    server_id: Memo<String>,
    server_title: Option<RwSignal<String>>,
    new_build: RwSignal<bool>,
) -> impl IntoView {
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
    let menu = NodeRef::<leptos::html::Details>::new();
    let servers = me
        .servers
        .into_iter()
        .map(|other| {
            let initial = other.name.chars().next().unwrap_or('?').to_uppercase().to_string();
            let href = format!("/servers/{}", other.id);
            if other.id == server.id {
                // Picking the server already open only closes the menu.
                return view! {
                    <a href=href class="current" aria-current="page" on:click=move |ev| {
                        ev.prevent_default();
                        if let Some(menu) = menu.get_untracked() {
                            menu.set_open(false);
                        }
                    }>
                        <span class="switcher-mark">{initial}</span>
                        <span class="switcher-label">{other.name}</span>
                        <span class="switcher-check" aria-hidden="true">"✓"</span>
                    </a>
                }
                .into_any();
            }
            view! {
                <A href=href>
                    <span class="switcher-mark">{initial}</span>
                    <span class="switcher-label">{other.name}</span>
                </A>
            }
            .into_any()
        })
        .collect_view();
    view! {
        <details class="switcher" node_ref=menu>
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
    use crate::pages::lists::Filters;
    let pathname = use_location().pathname;
    let query = leptos_router::hooks::use_query_map();
    let events_href = format!("{base}/events");
    let shortcut = |path: &str| match path {
        "upcoming" => Some(Filters::upcoming()),
        "drafts" => Some(Filters::drafts()),
        "scheduled" => Some(Filters::scheduled()),
        _ => None,
    };
    // The shortcut the Events page shows right now, if it shows one.
    let showing = Memo::new({
        let events_href = events_href.clone();
        move |_| {
            if pathname.get() != events_href {
                return None;
            }
            query.with(|map| {
                let filters = Filters::read(map);
                let searched = map.get("q").is_some_and(|q| !q.is_empty());
                ["upcoming", "drafts", "scheduled"]
                    .into_iter()
                    .find(|path| !searched && shortcut(path).as_ref() == Some(&filters))
            })
        }
    });
    let links = NAV
        .iter()
        .map(|(path, label, sub)| {
            let filters = shortcut(path);
            let href = match &filters {
                Some(filters) => format!("{events_href}{}", filters.link("", 1)),
                None => format!("{base}{path}"),
            };
            let nested = format!("{href}/");
            let here_href = href.clone();
            let schedule = path.is_empty();
            let events = *path == "/events";
            let count = match *path {
                "upcoming" => Some(counts.upcoming),
                "drafts" => Some(counts.drafts),
                "scheduled" => Some(counts.scheduled),
                "/recurring" => Some(counts.series),
                "/routines" => Some(counts.routines),
                _ => None,
            };
            let sub = *sub;
            let path = *path;
            let new_event = events.then(|| format!("{events_href}/new"));
            let link = view! {
                <A href=href exact=schedule attr:class=move || {
                    let here = pathname.get();
                    // Leaving the server runs this once more after `showing` is gone.
                    let shown = showing.try_get().flatten();
                    // Schedule is the server's front page; every event page sits under Events,
                    // unless the Events page is showing one of the shortcuts below it.
                    let on = if filters.is_some() {
                        shown == Some(path)
                    } else if events {
                        (here == here_href || here.starts_with(&nested)) && shown.is_none()
                    } else {
                        here == here_href || (!schedule && here.starts_with(&nested))
                    };
                    match (on, sub) {
                        (true, true) => "nav-sub active",
                        (false, true) => "nav-sub",
                        (true, false) => "active",
                        (false, false) => "",
                    }
                }>
                    {*label}" "{count.map(|n| view! { <span class="count">{n}</span> })}
                </A>
            };
            match new_event {
                Some(new_event) => view! {
                    <div class="nav-row">
                        {link}
                        <A href=new_event attr:class="nav-new" attr:title="New event" attr:aria-label="New event">
                            <span class="nav-plus" aria-hidden="true"></span>
                        </A>
                    </div>
                }
                .into_any(),
                None => link.into_any(),
            }
        })
        .collect_view();
    view! { <nav class="nav">{links}</nav> }
}
