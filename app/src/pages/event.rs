use leptos::prelude::*;

use crate::format::{self, clock_of, color_name, spots, status_label};
use crate::islands::{ActionButton, SignupPanel};
use crate::{api, base, border, load, md, param};

#[component]
pub fn EventPage() -> impl IntoView {
    let server_id = param("server");
    let id = param("id");
    let data = async move {
        let server = api::server(&server_id).await?;
        let event = api::event(&server_id, &id).await?;
        let signups = api::signups(&server_id, &id).await?;
        let reminders = api::event_reminders(&server_id, &id).await?;
        let thread = api::thread(&server_id, &id).await?;
        let roles = api::roles(&server_id).await?;
        let members = if signups.can_manage {
            api::members(&server_id).await?
        } else {
            Vec::new()
        };
        Ok((server, event, signups, reminders, thread, roles, members))
    };
    load(data, |(server, event, signups, reminders, thread, roles, members)| {
        let clock = clock_of(&server);
        let now = format::now();
        let base = base();
        let here = format!("{base}/events/{}", event.id);
        let api_here = format!("/api{here}");
        let name_of = |id: i64| {
            roles
                .iter()
                .find(|role| role.id == id)
                .map(|role| role.name.clone())
        };
        let mentions = {
            let mut names = Vec::new();
            if event.mention_everyone {
                names.push("@everyone".to_string());
            }
            names.extend(event.mention_role_ids.iter().filter_map(|id| name_of(*id)));
            names
        };
        let restrictions: Vec<String> = event
            .restrict_role_ids
            .iter()
            .filter_map(|id| name_of(*id))
            .collect();
        let attendee = event.attendee_role_id.and_then(name_of).unwrap_or_default();
        let close = if event.signups_closed {
            "Closed".to_string()
        } else {
            event
                .cutoff_at
                .map(|dt| clock.when(dt))
                .unwrap_or_else(|| "Never".into())
        };
        let series_note = event.series.as_ref().and_then(|series| {
            (series.cadence != "once").then(|| {
                let href = format!("{base}/recurring/{}", series.id);
                view! {
                    <p class="notice">
                        "This is one date in a series. Moving this start time leaves the other dates where they are. "
                        <a href=href>"Change the repeat on the series."</a>
                    </p>
                }
            })
        });
        let localize = if server.localize_time {
            "Members see this time in their own zone on Discord."
        } else {
            "Times stay in the server zone."
        };
        let calendar = if event.google_synced {
            view! { "On your Google Calendar" }.into_any()
        } else if event.calendar_synced {
            view! { "On your calendar feed" }.into_any()
        } else {
            event
                .google_url
                .clone()
                .map(|url| view! { <a href=url>"Add to Google Calendar"</a> }.into_any())
                .unwrap_or_else(|| view! { <span class="muted">"No calendar link"</span> }.into_any())
        };
        let reminder_rows = if reminders.is_empty() {
            view! { <p class="muted">"No reminders"</p> }.into_any()
        } else {
            reminders
                .iter()
                .map(|rem| {
                    let message = rem.message.clone();
                    view! {
                        <p>
                            {format::reminder_offset(rem.offset_minutes)}
                            <br/><span class="muted">{if rem.sent { "Sent" } else { "Waiting" }}</span>
                        </p>
                        {(!message.is_empty()).then(|| md(&message))}
                    }
                })
                .collect_view()
                .into_any()
        };
        let thread_block = (!thread.members.is_empty() || !thread.messages.is_empty()).then(|| {
            let names = thread.members.join(", ");
            let notes = thread
                .messages
                .iter()
                .map(|note| {
                    view! {
                        <p>
                            {note.body.clone()}
                            <br/>
                            <span class="muted">{note.place.clone()}" · "{format::format_rel(note.created_at, now)}</span>
                        </p>
                    }
                })
                .collect_view();
            view! {
                <p class="kicker">"Thread"</p>
                {(!names.is_empty()).then(|| view! { <p class="muted">{names}</p> })}
                {notes}
            }
        });
        let fields = signups
            .options
            .iter()
            .map(|opt| {
                let lines: Vec<String> = opt
                    .confirmed
                    .iter()
                    .map(|person| person.name.clone())
                    .chain((opt.waiting_count > 0).then(|| format!("{} waiting", opt.waiting_count)))
                    .collect();
                let value = if lines.is_empty() {
                    view! { "-" }.into_any()
                } else {
                    lines
                        .iter()
                        .enumerate()
                        .map(|(i, line)| view! { {(i > 0).then(|| view! { <br/> })}{line.clone()} })
                        .collect_view()
                        .into_any()
                };
                view! {
                    <div class="d-field">
                        <p class="d-field-name">
                            {opt.emoji.clone()}" "
                            <strong>{opt.name.clone()}</strong>
                            " ("{spots(opt.confirmed_count, opt.capacity)}")"
                        </p>
                        <p class="d-field-value">{value}</p>
                    </div>
                }
            })
            .collect_view();
        let buttons = signups
            .options
            .iter()
            .map(|opt| {
                view! { <button class="d-btn" type="button" disabled>{opt.emoji.clone()}</button> }
            })
            .collect_view();
        let description = event.description.clone();
        let location = event.location.clone();
        let image = event.image_url.clone();
        let color = event.color.clone();
        let can = event.can.clone();
        view! {
            <div class="event-top">
                <div>
                    <h1>{event.title.clone()}</h1>
                    <p class="event-date">{format::format_long_date(clock.day(event.starts_at))}</p>
                    <p class="event-time">{format::time_line(&clock, event.starts_at, event.duration_minutes)}</p>
                    {(!location.is_empty()).then(|| view! { <p class="event-place">"📍 "{location.clone()}</p> })}
                </div>
                <div class="actions">
                    {can.edit.then(|| view! { <a class="ghost" href=format!("{here}/edit")>"Edit"</a> })}
                    {can.archive.then(|| view! {
                        <ActionButton method="POST".into() url=format!("{api_here}/archive") label="Archive".into() class="ghost".into()/>
                    })}
                    <ActionButton method="POST".into() url=format!("{api_here}/duplicate") label="Duplicate".into() class="ghost".into()/>
                    {can.skip.then(|| view! {
                        <ActionButton method="POST".into() url=format!("{api_here}/skip") label="Skip this date".into() class="ghost".into()/>
                    })}
                    {can.delete.then(|| view! {
                        <ActionButton method="DELETE".into() url=api_here.clone() label="Delete".into() class="danger".into() confirm="Remove this event?".into()/>
                    })}
                </div>
            </div>
            {series_note}
            {image.as_ref().map(|url| view! { <img class="event-banner" src=url.clone() alt=""/> })}
            <div class="event-layout">
                <div class="event-main">
                    <section class="event-desc" style=border(&color)>
                        {if description.is_empty() {
                            view! { <p class="muted">"No description"</p> }.into_any()
                        } else {
                            md(&description).into_any()
                        }}
                    </section>
                    <SignupPanel server=param("server") event=event.id.clone() signups members/>
                    <section class="panel">
                        <h2>"Channel"</h2>
                        <div class="connection-head">
                            <p>"#"{event.channel.clone()}</p>
                            {event.discord_url.clone().map(|url| view! { <a class="ghost" href=url>"Open in Discord"</a> })}
                        </div>
                        <p class="meta">{calendar}</p>
                        <div class="connection-grid">
                            <div>
                                <p class="kicker">"Reminders"</p>
                                {reminder_rows}
                                {(!attendee.is_empty()).then(|| view! {
                                    <p class="muted">"Confirmed people on a reminding option get "{attendee.clone()}"."</p>
                                })}
                            </div>
                            <div><p class="kicker">"Mentions"</p>{names(&mentions, "No mentions")}</div>
                            <div>
                                <p class="kicker">"Attendee role"</p>
                                {if attendee.is_empty() {
                                    view! { <p class="muted">"No attendee role"</p> }.into_any()
                                } else {
                                    view! { <p>{attendee.clone()}</p> }.into_any()
                                }}
                            </div>
                            <div><p class="kicker">"Restrictions"</p>{names(&restrictions, "No restrictions")}</div>
                        </div>
                        {thread_block}
                    </section>
                    <section>
                        <h2>"Discord message"</h2>
                        <div class="discord">
                            <div class="embed" style=border(&color)>
                                <h3>{event.title.clone()}</h3>
                                {(!description.is_empty()).then(|| view! { <div class="d-desc md" inner_html=crate::markdown::render(&description)></div> })}
                                {(!location.is_empty()).then(|| view! {
                                    <p class="d-kicker">"Location"</p>
                                    <p class="d-desc">"📍 "{location.clone()}</p>
                                })}
                                <p class="d-kicker">"Time"</p>
                                <p class="d-desc">{clock.when(event.starts_at)}" - "{clock.end(event.starts_at, event.duration_minutes)}</p>
                                <p class="d-links">
                                    {event.calendar_url.clone().map(|url| view! { <a href=url>"+"</a> })}
                                    {(!here.is_empty()).then(|| view! { " · "<a href=here.clone()>"View on web"</a> })}
                                </p>
                                <p class="d-desc">"🕐 "{format::format_rel(event.starts_at, now)}</p>
                                {(event.status != "posted").then(|| view! { <p class="d-desc"><em>{status_label(&event.status)}</em></p> })}
                                <div class="d-fields">{fields}</div>
                                {image.as_ref().map(|url| view! { <img class="d-image" src=url.clone() alt=""/> })}
                                <p class="d-foot">"Created by "{event.creator.clone()}</p>
                            </div>
                            <div class="btn-row">
                                {buttons}
                                {if can.edit {
                                    view! { <a class="d-btn primary" href=format!("{here}/edit")>"Edit"</a> }.into_any()
                                } else {
                                    view! { <button class="d-btn primary" type="button" disabled>"Edit"</button> }.into_any()
                                }}
                                {if can.delete {
                                    view! {
                                        <ActionButton method="DELETE".into() url=api_here.clone() label="Delete".into() class="d-btn danger".into() confirm="Remove this event?".into()/>
                                    }
                                    .into_any()
                                } else {
                                    view! { <button class="d-btn danger" type="button" disabled>"Delete"</button> }.into_any()
                                }}
                            </div>
                            <p class="why">"Times as they read on this site. "{localize}</p>
                        </div>
                    </section>
                </div>
                <aside class="panel event-facts">
                    <p class="kicker">"Organizer"</p>
                    <p>{event.creator}</p>
                    <p class="kicker">"Color"</p>
                    <p><i class="dot" style=format!("background: {color}")></i>" "{color_name(&color)}</p>
                    <p class="kicker">"Multiple responses"</p>
                    <p>{if event.multiple_signups { "Yes" } else { "No" }}</p>
                    <p class="kicker">"Signups close"</p>
                    <p>{close}</p>
                    <p class="kicker">"Status"</p>
                    <p>{status_label(&event.status)}</p>
                </aside>
            </div>
        }
    })
}

fn names(items: &[String], empty: &'static str) -> impl IntoView {
    if items.is_empty() {
        return view! { <p class="muted">{empty}</p> }.into_any();
    }
    items.iter().map(|name| view! { <p>{name.clone()}</p> }).collect_view().into_any()
}
