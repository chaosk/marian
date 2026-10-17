use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, clock_of, status_label, window};
use crate::islands::Finder;
use crate::{api, base, border, head, load, param, query};

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
    let server = param("server");
    let q = query("q");
    let page = query("page").parse().ok();
    let data = async move {
        let here = api::server(&server).await?;
        let rows = api::events(
            &server,
            &dto::EventQuery {
                view,
                q: q.clone(),
                page,
                ..Default::default()
            },
        )
        .await?;
        Ok((here, rows, q))
    };
    load(data, move |(server, rows, q)| {
        let clock = clock_of(&server);
        let base = base();
        let searchable = !rows.items.is_empty() || !q.is_empty();
        let body = if !rows.items.is_empty() {
            let lines = rows
                .items
                .iter()
                .map(|event| {
                    view! {
                        <tr>
                            <td class="event-title" style=border(&event.color)>
                                <a href=format!("{base}/events/{}", event.id)>{event.title.clone()}</a>
                            </td>
                            <td class="fit">{clock.when(event.starts_at)}</td>
                            <td class="fit"><span class="badge">{status_label(&event.status)}</span></td>
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
    let server = param("server");
    let data = async move { api::activity(&server, 80).await };
    load(data, |rows| {
        let now = format::now();
        let base = base();
        let body = if rows.is_empty() {
            view! { <section class="empty"><h2>"The log is empty."</h2><p>"Signups and posts will show up here."</p></section> }.into_any()
        } else {
            let items = rows
                .into_iter()
                .map(|row| {
                    let href = row
                        .event_id
                        .map(|id| format!("{base}/events/{id}"))
                        .unwrap_or_else(|| format!("{base}/activity"));
                    view! {
                        <li>
                            <a href=href>{row.body}</a>
                            <time>{format::format_rel(row.created_at, now)}</time>
                        </li>
                    }
                })
                .collect_view();
            view! { <ul class="log-list">{items}</ul> }.into_any()
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
                {prev.map(|href| view! { <a class="ghost" href=href>"Previous"</a> })}
                {next.map(|href| view! { <a class="ghost" href=href>"Next"</a> })}
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
