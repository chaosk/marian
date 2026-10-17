use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, clock_of, spot_count, status_label};
use crate::{api, base, border, load, md, param};

#[component]
pub fn Schedule() -> impl IntoView {
    let server = param("server");
    let data = async move {
        let here = api::server(&server).await?;
        let clock = clock_of(&here);
        let now = format::now();
        let (from, to) = format::week_of(&clock, now);
        let week = api::events(
            &server,
            &dto::EventQuery {
                view: dto::EventView::Upcoming,
                from: Some(from),
                to: Some(to),
                limit: Some(20),
                ..Default::default()
            },
        )
        .await?;
        let waiting = api::events(
            &server,
            &dto::EventQuery {
                view: dto::EventView::Queued,
                limit: Some(4),
                ..Default::default()
            },
        )
        .await?;
        let wrapped = api::events(
            &server,
            &dto::EventQuery {
                view: dto::EventView::Wrapped,
                limit: Some(8),
                ..Default::default()
            },
        )
        .await?;
        let reminders = api::pending_reminders(&server, 6).await?;
        let activity = api::activity(&server, 8).await?;
        Ok((here, week, waiting, wrapped, reminders, activity, now))
    };
    load(data, |(server, week, waiting, wrapped, reminders, activity, now)| {
        let clock = clock_of(&server);
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
                        .map(|event| card(&base, &clock, now, event, false))
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
                <section class="hero" style=border(&event.color)>
                    <div>
                        <p class="kicker">"Next up · "{format::format_rel(event.starts_at, now)}</p>
                        <h2><a href=format!("{base}/events/{}", event.id)>{event.title}</a></h2>
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
        let wrapped = (!wrapped.items.is_empty()).then(|| {
            let cards = wrapped
                .items
                .into_iter()
                .map(|event| {
                    view! {
                        <a class="card" style=border(&event.color) href=format!("{base}/events/{}", event.id)>
                            {event.title}" · "{clock.when(event.starts_at)}
                        </a>
                    }
                })
                .collect_view();
            view! { <section class="day"><h2>"Wrapped and waiting to archive"</h2><div class="stack">{cards}</div></section> }
        });
        view! {
            <div class="page-head">
                <div><h1>"This week"</h1></div>
                <a class="primary" href=format!("{base}/events/new")>"New event"</a>
            </div>
            {hero}
            {days}
            {wrapped}
            <div class="columns">
                <section class="panel">
                    <h3>"Waiting to post"</h3>
                    {links(&base, &waiting.items, &clock, true)}
                </section>
                <section class="panel">
                    <h3>"Reminders still out"</h3>
                    {if reminders.is_empty() {
                        view! { <p class="muted">"No reminders are waiting."</p> }.into_any()
                    } else {
                        reminders
                            .into_iter()
                            .map(|rem| {
                                view! {
                                    <p>
                                        <a href=format!("{base}/events/{}", rem.event_id)>{rem.title}</a>
                                        <br/><span class="muted">{format::reminder_offset(rem.offset_minutes)}</span>
                                    </p>
                                }
                            })
                            .collect_view()
                            .into_any()
                    }}
                </section>
                <section class="panel">
                    <h3>"Activity"</h3>
                    {if activity.is_empty() {
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
                    }}
                </section>
            </div>
        }
    })
}

fn card(
    base: &str,
    clock: &format::Clock,
    now: chrono::DateTime<chrono::Utc>,
    event: dto::EventSummary,
    _hero: bool,
) -> impl IntoView {
    let pills = event
        .spots
        .iter()
        .map(|spot| {
            view! { <span class="pill"><span>{spot.emoji.clone()}</span><span>{spot_count(spot)}</span></span> }
        })
        .collect_view();
    view! {
        <a class="card" style=border(&event.color) href=format!("{base}/events/{}", event.id)>
            <div class="card-top">
                <h3>{event.title}</h3>
                <span class="badge">{status_label(&event.status)}</span>
            </div>
            <p class="meta">
                {clock.when(event.starts_at)}"–"{clock.end(event.starts_at, event.duration_minutes)}
                " · #"{event.channel}" · "{format::format_rel(event.starts_at, now)}
            </p>
            <div class="pills">{pills}</div>
        </a>
    }
}

fn links(base: &str, items: &[dto::EventSummary], clock: &format::Clock, post: bool) -> impl IntoView {
    if items.is_empty() {
        return view! { <p class="muted">"Nothing is queued."</p> }.into_any();
    }
    items
        .iter()
        .map(|event| {
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
                    <a href=format!("{base}/events/{}", event.id)>{event.title.clone()}</a>
                    <br/><span class="muted">{detail}</span>
                </p>
            }
        })
        .collect_view()
        .into_any()
}
