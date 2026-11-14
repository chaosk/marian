use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, clock_of, spot_count, status_label};
use crate::cache::{self, show, when, Ready};
use crate::{base, md, title, A};

#[component]
pub fn Schedule() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let week = cache::listen(cache::events_scope(), {
        let server = server;
        move || week_key(server.get())
    });
    let wrapped = cache::listen(cache::events_scope(), || {
        cache::events_key(
            cache::server_key(),
            &dto::EventQuery {
                view: dto::EventView::Wrapped,
                limit: Some(8),
                ..Default::default()
            },
        )
    });
    let waiting = cache::listen(cache::events_scope(), || {
        cache::events_key(
            cache::server_key(),
            &dto::EventQuery {
                view: dto::EventView::Queued,
                limit: Some(4),
                ..Default::default()
            },
        )
    });
    let pending = cache::listen(cache::pending_reminders_scope(), cache::server_key);
    let membership = cache::listen(cache::membership_scope(), cache::server_key);
    let now = format::now();
    title("This week");
    view! {
        <div class="page-head">
            <div><h1>"This week"</h1></div>
            <A href=format!("{}/events/new", base()) attr:class="primary">"New event"</A>
        </div>
        {when(move || (server.get(), week.get()).ready(), move |(server, week)| week_cell(&server, week, now))}
        {when(move || (server.get(), wrapped.get()).ready(), move |(server, wrapped)| wrapped_cell(&server, wrapped, now))}
        <div class="columns">
            <section class="panel">
                <h3>"Waiting to post"</h3>
                {when(move || (server.get(), waiting.get()).ready(), move |(server, waiting)| {
                    links(&base(), &waiting.items, &clock_of(&server), true)
                })}
            </section>
            <section class="panel">
                <h3>"Reminders still out"</h3>
                {show(pending, pending_cell)}
            </section>
            // The server's log is for those who manage it, so only they ask for
            // it. Read inside the Transition, so the server waits for the answer
            // and draws what the browser will.
            <Transition fallback=|| ()>
                {move || membership.get().and_then(Result::ok).filter(|m| m.server_manager).map(|_| {
                    let activity = cache::listen(cache::activity_scope(), || (cache::server_key(), 1, 8));
                    view! {
                        <section class="panel">
                            <h3>"Activity"</h3>
                            {show(activity, move |rows| activity_cell(rows.items, now))}
                        </section>
                    }
                })}
            </Transition>
        </div>
    }
}

fn week_key(server: Option<Result<dto::Server, crate::api::Failure>>) -> (String, String) {
    let (id, clock) = match server {
        Some(Ok(server)) => (server.id.clone(), clock_of(&server)),
        _ => (cache::server_key(), format::Clock::new("UTC", true)),
    };
    let (from, to) = format::week_of(&clock, format::now());
    cache::events_key(
        id,
        &dto::EventQuery {
            view: dto::EventView::Upcoming,
            from: Some(from),
            to: Some(to),
            limit: Some(20),
            ..Default::default()
        },
    )
}

fn week_cell(server: &dto::Server, week: dto::Paged<dto::EventSummary>, now: chrono::DateTime<chrono::Utc>) -> impl IntoView {
    let clock = clock_of(server);
    let base = base();
    let hero = week
        .items
        .iter()
        .filter(|event| event.status == "posted")
        .min_by_key(|event| event.starts_at)
        .cloned();
    let mut days: Vec<(String, bool, Vec<dto::EventSummary>)> = Vec::new();
    let today = clock.day(now);
    for event in week.items.iter().take(5) {
        let day = clock.day(event.starts_at);
        let label = format::format_day(day);
        if days.last().map(|(last, _, _)| last != &label).unwrap_or(true) {
            days.push((label, day == today, Vec::new()));
        }
        days.last_mut().unwrap().2.push(event.clone());
    }
    let days = if days.is_empty() {
        view! { <section class="empty"><h2>"Nothing scheduled."</h2></section> }.into_any()
    } else {
        days.into_iter()
            .map(|(label, is_today, events)| {
                let cards = events
                    .into_iter()
                    .map(|event| card(&base, &clock, now, event))
                    .collect_view();
                view! {
                    <section class=if is_today { "day today" } else { "day" }>
                        <h2>{label}</h2>
                        <div class="stack">{cards}</div>
                    </section>
                }
            })
            .collect_view()
            .into_any()
    };
    let hero = hero.map(|event| {
        let description = event.description.clone();
        let spots = event
            .spots
            .iter()
            .map(|spot| format!("{} {}", spot.name, spot.confirmed))
            .collect::<Vec<_>>()
            .join(" · ");
        view! {
            <section class="hero">
                {crate::paint("stripe", event.color.clone())}
                <div>
                    <p class="kicker">"Next up · "{format::format_rel(event.starts_at, now)}</p>
                    <h2><A href=format!("{base}/events/{}", event.id.clone())>{event.title.clone()}</A></h2>
                    <p>{clock.when(event.starts_at)}" · #"{event.channel}</p>
                    {(!description.is_empty()).then(|| md(&description))}
                </div>
                <div>
                    <p class="kicker">"Responses"</p>
                    <p>{spots}</p>
                </div>
            </section>
        }
    });
    view! { {hero} {days} }
}

fn wrapped_cell(server: &dto::Server, wrapped: dto::Paged<dto::EventSummary>, _now: chrono::DateTime<chrono::Utc>) -> impl IntoView {
    let clock = clock_of(server);
    let base = base();
    (!wrapped.items.is_empty()).then(|| {
        let cards = wrapped
            .items
            .into_iter()
            .map(|event| {
                let href = format!("{base}/events/{}", event.id);
                let stripe = crate::paint("stripe", event.color.clone());
                let label = format!("{} · {}", event.title, clock.when(event.starts_at));
                view! {
                    <A href=href attr:class="card">{stripe}{label}</A>
                }
            })
            .collect_view();
        view! { <section class="day"><h2>"Wrapped and waiting to archive"</h2><div class="stack">{cards}</div></section> }
    })
}

fn pending_cell(reminders: Vec<dto::PendingReminder>) -> impl IntoView {
    let base = base();
    if reminders.is_empty() {
        view! { <p class="muted">"No reminders are waiting."</p> }.into_any()
    } else {
        reminders
            .into_iter()
            .map(|rem| {
                view! {
                    <p>
                        <A href=format!("{base}/events/{}", rem.event_id)>{rem.title}</A>
                        <br/><span class="muted">{format::reminder_offset(rem.offset_minutes)}</span>
                    </p>
                }
            })
            .collect_view()
            .into_any()
    }
}

fn activity_cell(activity: Vec<dto::Activity>, now: chrono::DateTime<chrono::Utc>) -> impl IntoView {
    if activity.is_empty() {
        view! { <p class="muted">"The log is quiet."</p> }.into_any()
    } else {
        activity
            .into_iter()
            .map(|item| {
                view! {
                    <p>
                        {item.body}
                        <br/><span class="muted">{format::format_rel(item.created_at, now)}</span>
                    </p>
                }
            })
            .collect_view()
            .into_any()
    }
}

fn card(
    base: &str,
    clock: &format::Clock,
    now: chrono::DateTime<chrono::Utc>,
    event: dto::EventSummary,
) -> impl IntoView {
    let pills = event
        .spots
        .iter()
        .map(|spot| {
            view! { <span class="pill"><span>{spot.emoji.clone()}</span><span>{spot_count(spot)}</span></span> }
        })
        .collect_view();
    let href = format!("{base}/events/{}", event.id);
    let stripe = crate::paint("stripe", event.color.clone());
    let when = clock.when(event.starts_at);
    let end = clock.end(event.starts_at, event.duration_minutes);
    let rel = format::format_rel(event.starts_at, now);
    let status = status_label(&event.status);
    view! {
        <A href=href attr:class="card">
            {stripe}
            <div class="card-top">
                <h3>{event.title}</h3>
                <span class="badge">{status}</span>
            </div>
            <p class="meta">
                {when}"–"{end}
                " · #"{event.channel}" · "{rel}
            </p>
            <div class="pills">{pills}</div>
        </A>
    }
}

fn links(base: &str, items: &[dto::EventSummary], clock: &format::Clock, post: bool) -> impl IntoView {
    if items.is_empty() {
        return view! { <p class="muted">"Nothing is queued."</p> }.into_any();
    }
    items
        .iter()
        .map(|event| {
            let href = format!("{base}/events/{}", event.id);
            let title = event.title.clone();
            let detail = if post {
                event
                    .post_at
                    .map(|dt| clock.when(dt))
                    .unwrap_or_else(|| "when you publish it".into())
            } else {
                clock.when(event.starts_at)
            };
            view! {
                <p>
                    <A href=href>{title}</A>
                    <br/><span class="muted">{detail}</span>
                </p>
            }
        })
        .collect_view()
        .into_any()
}
