use chrono::NaiveDate;
use leptos::prelude::*;
use leptos_router::components::Redirect;
use leptos_router::params::ParamsMap;
use marian_types as dto;

use crate::format::{self, clock_of, status_label, window};
use crate::widgets::Finder;
use crate::cache::{self, show, when, Ready};
use crate::{api, base, border, head, query, title, A};

/// Statuses an event can have, in the order the Status filter lists them.
const STATUSES: &[&str] = &["draft", "scheduled", "failed", "posted", "archived", "cancelled", "skipped"];

/// The Events page's filters, as the address holds them:
/// `?status=draft,scheduled&when=upcoming&from=2026-10-01&to=2026-10-31`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Filters {
    pub status: Vec<&'static str>,
    pub when: Option<dto::EventWhen>,
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
}

impl Filters {
    pub fn read(map: &ParamsMap) -> Self {
        let listed = map.get("status").unwrap_or_default();
        let day = |key: &str| map.get(key).and_then(|value| NaiveDate::parse_from_str(&value, "%Y-%m-%d").ok());
        Self {
            status: STATUSES
                .iter()
                .copied()
                .filter(|status| listed.split(',').any(|named| named.trim() == *status))
                .collect(),
            when: match map.get("when").as_deref() {
                Some("upcoming") => Some(dto::EventWhen::Upcoming),
                Some("past") => Some(dto::EventWhen::Past),
                _ => None,
            },
            from: day("from"),
            to: day("to"),
        }
    }

    /// Posted or queued, and not over yet.
    pub fn upcoming() -> Self {
        Self {
            status: vec!["scheduled", "posted"],
            when: Some(dto::EventWhen::Upcoming),
            ..Self::default()
        }
    }

    pub fn drafts() -> Self {
        Self { status: vec!["draft"], ..Self::default() }
    }

    /// Queued posts, including ones that failed to post.
    pub fn scheduled() -> Self {
        Self { status: vec!["scheduled", "failed"], ..Self::default() }
    }

    /// The sidebar's name for these filters, when they are one of its shortcuts.
    fn shortcut(&self) -> Option<&'static str> {
        [(Self::upcoming(), "Upcoming"), (Self::drafts(), "Drafts"), (Self::scheduled(), "Scheduled posts")]
            .into_iter()
            .find(|(filters, _)| filters == self)
            .map(|(_, name)| name)
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The query string for these filters, a search, and a page.
    pub fn link(&self, q: &str, page: u32) -> String {
        let mut parts = Vec::new();
        if !self.status.is_empty() {
            parts.push(format!("status={}", self.status.join(",")));
        }
        match self.when {
            Some(dto::EventWhen::Upcoming) => parts.push("when=upcoming".into()),
            Some(dto::EventWhen::Past) => parts.push("when=past".into()),
            None => {}
        }
        if let Some(day) = self.from {
            parts.push(format!("from={day}"));
        }
        if let Some(day) = self.to {
            parts.push(format!("to={day}"));
        }
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

    fn when_label(&self) -> Option<String> {
        use chrono::Datelike;
        let this_year = format::now().year();
        let day = |day: NaiveDate| {
            if day.year() == this_year {
                day.format("%b %-d").to_string()
            } else {
                day.format("%b %-d, %Y").to_string()
            }
        };
        let range = match (self.from, self.to) {
            (None, None) => None,
            (Some(from), None) => Some(format!("from {}", day(from))),
            (None, Some(to)) => Some(format!("to {}", day(to))),
            (Some(from), Some(to)) => Some(format!("{} – {}", day(from), day(to))),
        };
        let when = self.when.map(|when| match when {
            dto::EventWhen::Upcoming => "upcoming".to_string(),
            dto::EventWhen::Past => "past".to_string(),
        });
        match (when, range) {
            (None, None) => None,
            (Some(one), None) | (None, Some(one)) => Some(one),
            (Some(when), Some(range)) => Some(format!("{when}, {range}")),
        }
    }

    fn status_label(&self) -> Option<String> {
        match self.status.as_slice() {
            [] => None,
            [one] => Some(status_label(one).to_lowercase()),
            many => Some(format!("{} statuses", many.len())),
        }
    }
}

/// The old Upcoming, Drafts, and Scheduled pages are filters on Events now.
#[component]
pub fn Upcoming() -> impl IntoView {
    view! { <Redirect path=format!("{}/events{}", base(), Filters::upcoming().link("", 1))/> }
}

#[component]
pub fn Drafts() -> impl IntoView {
    view! { <Redirect path=format!("{}/events{}", base(), Filters::drafts().link("", 1))/> }
}

#[component]
pub fn Scheduled() -> impl IntoView {
    view! { <Redirect path=format!("{}/events{}", base(), Filters::scheduled().link("", 1))/> }
}

/// Every event on the server, narrowed by when it is, its status, and a search.
#[component]
pub fn AllEvents() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let events = cache::listen(cache::events_scope(), move || {
        let (q, page, filters) = leptos_router::hooks::use_query_map().with(|map| {
            (
                map.get("q").unwrap_or_default(),
                map.get("page").and_then(|value| value.parse().ok()),
                Filters::read(map),
            )
        });
        cache::events_key(
            cache::server_key(),
            &dto::EventQuery {
                view: dto::EventView::All,
                q,
                page,
                status: filters.status.join(","),
                when: filters.when,
                from_day: filters.from,
                to_day: filters.to,
                ..Default::default()
            },
        )
    });
    when(move || (server.get(), events.get()).ready(), move |(server, rows)| {
        let q = query("q");
        let filters = leptos_router::hooks::use_query_map().with_untracked(Filters::read);
        let clock = clock_of(&server);
        let base = base();
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
        let narrowed = !filters.is_empty() || !q.is_empty();
        let empty = rows.items.is_empty().then(|| {
            let (headline, note) = if narrowed {
                ("No events match.", "Change the filters or the search above.")
            } else {
                ("No events yet.", "Create one with New event on the Schedule.")
            };
            view! {
                <tr><td colspan="3" class="empty-row"><strong>{headline}</strong>" "{note}</td></tr>
            }
        });
        let clear = narrowed.then(|| {
            let href = format!("{base}/events");
            view! { <A href=href attr:class="ghost">"Clear filters"</A> }
        });
        let heading = filters.shortcut().filter(|_| q.is_empty()).unwrap_or("Events");
        title(heading);
        let go_to = {
            let (base, q) = (base.clone(), q.clone());
            move |next: Filters| format!("{base}/events{}", next.link(&q, 1))
        };
        let pages = {
            let (base, q, filters) = (base.clone(), q.clone(), filters.clone());
            move |page: u32| format!("{base}/events{}", filters.link(&q, page))
        };
        view! {
            <div class="page-head">
                <div><h1>{heading}</h1></div>
                <div class="page-tools">{clear}<Finder q=q.clone()/></div>
            </div>
            <div class="sheet filtered">
                <table>
                    <thead>
                        <tr>
                            <th>"Title"</th>
                            <th class="fit">
                                <WhenFilter filters=filters.clone() go_to=Callback::new({
                                    let go_to = go_to.clone();
                                    move |next: Filters| go_to(next)
                                })/>
                            </th>
                            <th class="fit">
                                <StatusFilter filters=filters.clone() go_to=Callback::new(move |next: Filters| go_to(next))/>
                            </th>
                        </tr>
                    </thead>
                    <tbody>{lines}{empty}</tbody>
                </table>
            </div>
            {pager_with(&rows, pages)}
        }
    })
}

/// A column heading that opens a filter. `on_close` runs whenever it closes,
/// so a menu can send what was picked once instead of on every click.
#[component]
fn FilterMenu(
    label: &'static str,
    summary: Option<String>,
    #[prop(optional)] right: bool,
    open: RwSignal<bool>,
    on_close: Callback<()>,
    children: ChildrenFn,
) -> impl IntoView {
    let root = NodeRef::<leptos::html::Div>::new();
    let close = move || {
        if open.get_untracked() {
            open.set(false);
            on_close.run(());
        }
    };
    let on = summary.is_some();
    view! {
        <div class="inline-anchor filter-head" class:right=right node_ref=root
            on:focusout=move |ev| {
                if crate::widgets::focus_left(&ev, root) {
                    close();
                }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    ev.stop_propagation();
                    close();
                }
            }>
            <button type="button" class="filter-button" class:on=on aria-haspopup="true"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { close() } else { open.set(true) }>
                {label}
                {summary.map(|text| view! { <span class="filter-summary">{text}</span> })}
                <span class="filter-caret" aria-hidden="true">"▾"</span>
            </button>
            {move || open.get().then(|| view! { <div class="popover filter-pop">{children()}</div> })}
        </div>
    }
}

/// Any time, coming up, or over, and an optional first and last day.
#[component]
fn WhenFilter(filters: Filters, go_to: Callback<Filters, String>) -> impl IntoView {
    let open = RwSignal::new(false);
    let picked = RwSignal::new(filters.clone());
    let go = crate::client::use_go();
    let send = {
        let (go, filters) = (go.clone(), filters.clone());
        move || {
            let next = picked.get_untracked();
            if next != filters {
                go(&go_to.run(next));
            }
        }
    };
    let choices = [(None, "Any time"), (Some(dto::EventWhen::Upcoming), "Upcoming"), (Some(dto::EventWhen::Past), "Past")];
    let day_input = move |label: &'static str, read: fn(&Filters) -> Option<NaiveDate>, write: fn(&mut Filters, Option<NaiveDate>)| {
        view! {
            <label class="field">
                {label}
                <input type="date" prop:value=move || picked.with(|f| read(f).map(|day| day.to_string()).unwrap_or_default())
                    on:change=move |ev| {
                        let day = NaiveDate::parse_from_str(&event_target_value(&ev), "%Y-%m-%d").ok();
                        picked.update(|f| write(f, day));
                    }/>
            </label>
        }
    };
    view! {
        <FilterMenu label="When" summary=filters.when_label() open on_close=Callback::new({
            let send = send.clone();
            move |_| send()
        })>
            <div class="check-list" role="radiogroup" aria-label="When">
                {choices
                    .into_iter()
                    .map(|(when, label)| {
                        let send = send.clone();
                        view! {
                            <label class="check-row">
                                <input type="radio" name="when" prop:checked=move || picked.with(|f| f.when == when)
                                    on:change=move |_| {
                                        picked.update(|f| f.when = when);
                                        open.set(false);
                                        send();
                                    }/>
                                {label}
                            </label>
                        }
                    })
                    .collect_view()}
            </div>
            <div class="filter-days">
                {day_input("From", |f| f.from, |f, day| f.from = day)}
                {day_input("To", |f| f.to, |f, day| f.to = day)}
            </div>
        </FilterMenu>
    }
}

/// Any of the statuses ticked; none ticked is every status.
#[component]
fn StatusFilter(filters: Filters, go_to: Callback<Filters, String>) -> impl IntoView {
    let open = RwSignal::new(false);
    let picked = RwSignal::new(filters.clone());
    let go = crate::client::use_go();
    let send = move || {
        let next = picked.get_untracked();
        if next != filters {
            go(&go_to.run(next));
        }
    };
    view! {
        <FilterMenu label="Status" summary=picked.with_untracked(Filters::status_label) right=true open
            on_close=Callback::new(move |_| send())>
            <div class="check-list">
                {STATUSES
                    .iter()
                    .copied()
                    .map(|status| {
                        view! {
                            <label class="check-row">
                                <input type="checkbox" prop:checked=move || picked.with(|f| f.status.contains(&status))
                                    on:change=move |ev| {
                                        let on = event_target_checked(&ev);
                                        picked.update(|f| {
                                            f.status.retain(|kept| *kept != status);
                                            if on {
                                                f.status.push(status);
                                                f.status.sort_by_key(|kept| STATUSES.iter().position(|s| s == kept));
                                            }
                                        });
                                    }/>
                                {status_label(status)}
                            </label>
                        }
                    })
                    .collect_view()}
            </div>
        </FilterMenu>
    }
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
    let q = q.to_string();
    pager_with(page, move |n| list_link(&q, n))
}

fn pager_with<T>(page: &dto::Paged<T>, link: impl Fn(u32) -> String) -> impl IntoView {
    let prev = (page.page > 1).then(|| link(page.page - 1));
    let next = (page.page < page.pages).then(|| link(page.page + 1));
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
