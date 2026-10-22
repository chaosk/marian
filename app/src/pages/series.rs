use leptos::prelude::*;
use marian_types as dto;

use crate::format::{self, cadence_label, clock_of, status_label};
use crate::widgets::{ActionButton, Finder};
use crate::pages::lists::table_pager;
use crate::cache::{self, when, Ready};
use crate::{base, client, query, title, A};

#[component]
pub fn Recurring() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let series = cache::listen(cache::series_list_scope(), cache::server_key);
    let q = query("q");
    when(move || (server.get(), series.get()).ready(), move |(server, series)| {
        let clock = clock_of(&server);
        let base = base();
        let needle = q.trim().to_lowercase();
        let rows: Vec<_> = series
            .into_iter()
            .filter(|item| item.cadence != "once")
            .filter(|item| needle.is_empty() || item.title.to_lowercase().contains(&needle))
            .collect();
        let searchable = !rows.is_empty() || !q.is_empty();
        let body = if rows.is_empty() && !q.is_empty() {
            view! { <section class="empty"><h2>"No matches."</h2></section> }.into_any()
        } else if rows.is_empty() {
            view! { <section class="empty"><h2>"No series yet."</h2><p>"Set a repeat when you create an event."</p></section> }.into_any()
        } else {
            let lines = rows
                .into_iter()
                .map(|item| {
                    let when = item
                        .next
                        .map(|dt| clock.when(dt))
                        .unwrap_or_else(|| "no upcoming date".into());
                    let meta = if item.active && item.max_occurrences.is_none() && item.end_on.is_none()
                    {
                        format!("{when} · keeps going")
                    } else {
                        format!("{when} · {} dates", item.dates)
                    };
                    let href = format!("{base}/recurring/{}", item.id);
                    view! {
                        <tr>
                            <td><A href=href>{item.title}</A></td>
                            <td class="fit">{meta}</td>
                            <td class="fit">
                                <span class="badge">{if item.active { cadence_label(&item.cadence) } else { "Ended" }}</span>
                            </td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet">
                    <table>
                        <thead><tr><th>"Series"</th><th class="fit">"Next"</th><th class="fit">"Repeat"</th></tr></thead>
                        <tbody>{lines}</tbody>
                    </table>
                </div>
            }
            .into_any()
        };
        title("Recurring");
        view! {
            <div class="page-head">
                <div><h1>"Recurring"</h1></div>
                {searchable.then(|| view! { <Finder q=q.clone()/> })}
            </div>
            {body}
        }
    })
}

#[component]
pub fn SeriesPage() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let series = cache::listen(cache::series_scope(), cache::event_key);
    let events = cache::listen(cache::events_scope(), || {
        let page = leptos_router::hooks::use_query_map()
            .with(|map| map.get("page").and_then(|value| value.parse().ok()));
        cache::events_key(
            cache::server_key(),
            &dto::EventQuery {
                series: Some(cache::event_key().1),
                page,
                view: dto::EventView::All,
                ..Default::default()
            },
        )
    });
    when(move || (server.get(), series.get(), events.get()).ready(), move |(server, series, events)| {
        let clock = clock_of(&server);
        let base = base();
        let server_id = cache::server_now();
        let length = match (series.max_occurrences, series.end_on) {
            (Some(count), _) => format!("{count} dates"),
            (None, Some(end)) => format!("until {}", format::format_day(clock.day(end))),
            (None, None) => "keeps going".into(),
        };
        let summary = if series.active {
            let next = series
                .next
                .map(|at| format!(" · next {}", clock.when(at)))
                .unwrap_or_default();
            format!("{} · {length}{next}", cadence_label(&series.cadence))
        } else {
            "Ended. Dates that hadn't gone out were skipped.".to_string()
        };
        let pager = table_pager(&events, "");
        let rows = events
            .items
            .clone()
            .into_iter()
            .map(|event| {
                let href = format!("{base}/events/{}", event.id);
                // Once a date is out or skipped, its status says it all.
                let posts = match event.status.as_str() {
                    "posted" | "archived" | "skipped" | "cancelled" => "—".to_string(),
                    _ => event
                        .post_at
                        .map(|at| clock.when(at))
                        .unwrap_or_else(|| "With the event".into()),
                };
                let renamed = (event.title != series.title).then(|| event.title.clone());
                view! {
                    <tr>
                        <td>
                            <A href=href>{clock.when(event.starts_at)}</A>
                            {renamed.map(|title| view! { <span class="muted series-renamed">{title}</span> })}
                        </td>
                        <td class="fit">{posts}</td>
                        <td class="fit"><span class=format!("badge status-{}", event.status)>{status_label(&event.status)}</span></td>
                    </tr>
                }
            })
            .collect_view();
        let dates = if events.items.is_empty() {
            view! { <p class="muted">"No dates yet."</p> }.into_any()
        } else {
            view! {
                <div class="sheet">
                    <table>
                        <thead><tr><th>"Date"</th><th class="fit">"Posts"</th><th class="fit">"Status"</th></tr></thead>
                        <tbody>{rows}</tbody>
                    </table>
                </div>
                {pager}
            }
            .into_any()
        };
        title(series.title.clone());
        view! {
            <div class="page-head series-head">
                <div>
                    <p class="kicker">"Series"</p>
                    <h1>{series.title.clone()}</h1>
                    <p class="lede">{summary}</p>
                </div>
                {series.active.then(|| view! {
                    <div class="actions">
                        <ActionButton method="POST" url=crate::api::path::series_end(&server_id, &series.id) label="End series"
                            class="danger" confirm="End the series? Dates that haven't gone out are skipped."/>
                    </div>
                })}
            </div>
            <div class="series-page">
                {series.active.then(|| view! { <RepeatCard series=series.clone() clock/> })}
                <section class="panel settings-group">
                    <h2>"Dates"</h2>
                    {dates}
                </section>
            </div>
        }
    })
}

/// How often a series repeats and when it stops, saved together.
#[component]
fn RepeatCard(series: dto::Series, clock: format::Clock) -> impl IntoView {
    let cadence = RwSignal::new(series.cadence.clone());
    let ends = RwSignal::new(
        match (series.max_occurrences, series.end_on) {
            (Some(_), _) => "count",
            (None, Some(_)) => "date",
            (None, None) => "never",
        }
        .to_string(),
    );
    let count = RwSignal::new(series.max_occurrences.unwrap_or(10).to_string());
    let until = RwSignal::new(
        series
            .end_on
            .map(|end| clock.date_input(end))
            .unwrap_or_else(|| clock.date_input(format::now() + chrono::Duration::days(60))),
    );
    let problem = RwSignal::new(String::new());

    let server = cache::server_now();
    let api = StoredValue::new(crate::api::path::series(&server, &series.id));
    let saving = client::action(move |repeat: &dto::Repeat| {
        let repeat = repeat.clone();
        let url = api.get_value();
        let queries = cache::client();
        let server = server.clone();
        let toasts = client::toast_list();
        async move {
            match crate::api::send::<dto::Done, _>("PATCH", &url, Some(&repeat)).await {
                Ok(_) => {
                    if let Some(list) = toasts {
                        client::push_toast(list, "Saved the repeat.");
                    }
                    cache::refresh_in(queries, &server);
                }
                Err(failure) => problem.set(failure.message),
            }
        }
    });
    let save = move || {
        let count = if ends.get_untracked() == "count" {
            match count.get_untracked().trim().parse::<i64>() {
                Ok(n) if n >= 2 => Some(n),
                _ => return problem.set("A series needs at least 2 dates.".into()),
            }
        } else {
            None
        };
        let until = if ends.get_untracked() == "date" {
            match chrono::NaiveDate::parse_from_str(until.get_untracked().trim(), "%Y-%m-%d") {
                Ok(day) => Some(day),
                Err(_) => return problem.set("Pick the last day.".into()),
            }
        } else {
            None
        };
        let repeat = dto::Repeat {
            cadence: cadence.get_untracked(),
            count,
            until,
        };
        problem.set(String::new());
        saving.dispatch(repeat);
    };
    let cadences = ["daily", "weekly", "biweekly", "monthly"]
        .into_iter()
        .map(|value| view! { <option value=value selected=series.cadence == value>{cadence_label(value)}</option> })
        .collect_view();
    let choice = move |value: &'static str, label: &'static str| {
        view! {
            <label class="ends-choice">
                <input type="radio" name="ends" value=value prop:checked=move || ends.get() == value
                    on:change=move |_| ends.set(value.to_string())/>
                <span>{label}</span>
            </label>
        }
    };
    view! {
        <form class="panel settings-group" on:submit=move |ev| { ev.prevent_default(); save(); }>
            <h2>"Repeat"</h2>
            <label class="setting-row">
                <span class="setting-text"><strong>"Repeats"</strong></span>
                <select class="series-select" on:change=move |ev| cadence.set(event_target_value(&ev))>{cadences}</select>
            </label>
            <div class="setting-row ends-row">
                <span class="setting-text"><strong>"Ends"</strong></span>
                <div class="ends-choices">
                    {choice("never", "Never")}
                    <div class="ends-line">
                        {choice("count", "After")}
                        <input type="number" min="2" inputmode="numeric" aria-label="Number of dates"
                            prop:value=move || count.get() disabled=move || ends.get() != "count"
                            on:input=move |ev| count.set(event_target_value(&ev))/>
                        <span class="muted">"dates"</span>
                    </div>
                    <div class="ends-line">
                        {choice("date", "On")}
                        <input type="date" aria-label="Last day"
                            prop:value=move || until.get() disabled=move || ends.get() != "date"
                            on:input=move |ev| until.set(event_target_value(&ev))/>
                    </div>
                </div>
            </div>
            <p class="settings-note">"Upcoming dates move to the new rhythm. Posted and skipped dates stay as they are."</p>
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            <div class="inline-actions series-actions">
                <button type="submit" class="primary" disabled=move || saving.pending().get()>"Save repeat"</button>
            </div>
        </form>
    }
}
