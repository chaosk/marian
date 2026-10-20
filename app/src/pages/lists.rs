use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, clock_of, status_label, window};
use crate::widgets::Finder;
use crate::cache::{self, show, when, Ready};
use crate::{api, base, border, head, query, title, A};

#[component]
pub fn Upcoming() -> impl IntoView {
    event_list(
        dto::EventView::Upcoming,
        "Upcoming",
        "Nothing coming up.",
        "Posted and scheduled events show up here.",
    )
}

/// Every event on the server, whatever its state.
#[component]
pub fn AllEvents() -> impl IntoView {
    event_list(
        dto::EventView::All,
        "Events",
        "No events yet.",
        "Create one with New event on the Schedule.",
    )
}

#[component]
pub fn Drafts() -> impl IntoView {
    event_list(dto::EventView::Drafts, "Drafts", "No drafts.", "Start an event and choose Save draft.")
}

#[component]
pub fn Scheduled() -> impl IntoView {
    event_list(
        dto::EventView::Queued,
        "Scheduled posts",
        "Nothing is queued.",
        "Choose Schedule post on an event to put it here.",
    )
}

#[component]
pub fn Archive() -> impl IntoView {
    event_list(
        dto::EventView::Archive,
        "Archive",
        "The archive is empty.",
        "Posted events land here when they are archived.",
    )
}

fn event_list(
    view: dto::EventView,
    heading: &'static str,
    empty_title: &'static str,
    empty_body: &'static str,
) -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let events = cache::listen(cache::events_scope(), move || {
        let q = leptos_router::hooks::use_query_map().with(|map| map.get("q").unwrap_or_default());
        let page = leptos_router::hooks::use_query_map()
            .with(|map| map.get("page").and_then(|value| value.parse().ok()));
        cache::events_key(
            cache::server_key(),
            &dto::EventQuery {
                view,
                q,
                page,
                ..Default::default()
            },
        )
    });
    when(move || (server.get(), events.get()).ready(), move |(server, rows)| {
            let q = query("q");
        let clock = clock_of(&server);
        let base = base();
        let searchable = !rows.items.is_empty() || !q.is_empty();
        let body = if !rows.items.is_empty() {
            let lines = rows
                .items
                .iter()
                .cloned()
                .map(|event| {
                    let href = format!("{base}/events/{}", event.id);
                    let style = border(&event.color);
                    let when = clock.when(event.starts_at);
                    let status = status_label(&event.status);
                    view! {
                        <tr>
                            <td class="event-title" style=style>
                                <A href=href>{event.title}</A>
                            </td>
                            <td class="fit">{when}</td>
                            <td class="fit"><span class="badge">{status}</span></td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet">
                    <table>
                        <thead>
                            <tr><th>"Title"</th><th class="fit">"When"</th><th class="fit">"Status"</th></tr>
                        </thead>
                        <tbody>{lines}</tbody>
                    </table>
                </div>
                {pager(&rows, &q)}
            }
            .into_any()
        } else if !q.is_empty() {
            view! { <section class="empty"><h2>"No matches."</h2></section> }.into_any()
        } else {
            view! { <section class="empty"><h2>{empty_title}</h2><p>{empty_body}</p></section> }.into_any()
        };
        title(heading);
        view! {
            <div class="page-head">
                <div><h1>{heading}</h1></div>
                {searchable.then(|| view! { <Finder q=q.clone()/> })}
            </div>
            {body}
        }
    })
}

#[component]
pub fn ActivityLog() -> impl IntoView {
    let rows = cache::listen(cache::activity_scope(), || {
        let page = leptos_router::hooks::use_query_map()
            .with(|map| map.get("page").and_then(|value| value.parse().ok()))
            .unwrap_or(1);
        (cache::server_key(), page, 25)
    });
    show(rows, |rows| {
        let now = format::now();
        let base = base();
        let body = if rows.items.is_empty() {
            view! { <section class="empty"><h2>"The log is empty."</h2><p>"Signups and posts will show up here."</p></section> }.into_any()
        } else {
            let items = rows
                .items
                .iter()
                .cloned()
                .map(|row| {
                    let href = row
                        .event_id
                        .map(|id| format!("{base}/events/{id}"))
                        .unwrap_or_else(|| format!("{base}/activity"));
                    view! {
                        <li>
                            <A href=href>{row.body}</A>
                            <time>{format::format_rel(row.created_at, now)}</time>
                        </li>
                    }
                })
                .collect_view();
            view! {
                <ul class="log-list">{items}</ul>
                {pager(&rows, "")}
            }
            .into_any()
        };
        view! { {head("Activity")} {body} }
    })
}

fn pager<T>(page: &dto::Paged<T>, q: &str) -> impl IntoView {
    let prev = (page.page > 1).then(|| list_link(q, page.page - 1));
    let next = (page.page < page.pages).then(|| list_link(q, page.page + 1));
    view! {
        <div class="pager">
            <p class="muted">{window(page)}</p>
            <div>
                {prev.map(|href| view! { <A href=href attr:class="ghost">"Previous"</A> })}
                {next.map(|href| view! { <A href=href attr:class="ghost">"Next"</A> })}
            </div>
        </div>
    }
}

pub(super) fn list_link(q: &str, page: u32) -> String {
    let mut parts = Vec::new();
    if !q.is_empty() {
        parts.push(format!("q={}", api::encode(q)));
    }
    if page > 1 {
        parts.push(format!("page={page}"));
    }
    if parts.is_empty() {
        "?".into()
    } else {
        format!("?{}", parts.join("&"))
    }
}

pub(super) fn table_pager<T>(page: &dto::Paged<T>, q: &str) -> impl IntoView {
    pager(page, q)
}
