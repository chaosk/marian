use leptos::prelude::*;
use marian_types as dto;

use crate::format::{cadence_label, clock_of, status_label};
use crate::islands::{ActionButton, Finder, JsonForm};
use crate::pages::lists::table_pager;
use crate::{api, base, load, param, query};

#[component]
pub fn Recurring() -> impl IntoView {
    let server = param("server");
    let q = query("q");
    let data = async move {
        let here = api::server(&server).await?;
        let series = api::series_list(&server).await?;
        Ok((here, series, q))
    };
    load(data, |(server, series, q)| {
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
                    view! {
                        <tr>
                            <td><a href=format!("{base}/recurring/{}", item.id)>{item.title}</a></td>
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
    let server_id = param("server");
    let id = param("id");
    let q = query("q");
    let page = query("page").parse().ok();
    let data = async move {
        let server = api::server(&server_id).await?;
        let series = api::series(&server_id, &id).await?;
        let events = api::events(
            &server_id,
            &dto::EventQuery {
                series: Some(id.clone()),
                q: q.clone(),
                page,
                view: dto::EventView::All,
                ..Default::default()
            },
        )
        .await?;
        Ok((server, series, events, q))
    };
    load(data, |(server, series, events, q)| {
        let clock = clock_of(&server);
        let base = base();
        let api_here = format!("/api{base}/series/{}", series.id);
        let until = series.end_on.map(|dt| clock.date_input(dt)).unwrap_or_default();
        let count = series
            .max_occurrences
            .map(|n| n.to_string())
            .unwrap_or_default();
        let searchable = events.total > 0 || !q.is_empty();
        let body = if !events.items.is_empty() {
            let lines = events
                .items
                .iter()
                .map(|event| {
                    let post = event
                        .post_at
                        .map(|dt| clock.when(dt))
                        .unwrap_or_else(|| "with the event".into());
                    view! {
                        <tr>
                            <td><a href=format!("{base}/events/{}", event.id)>{event.title.clone()}</a></td>
                            <td class="fit">{clock.when(event.starts_at)}" · posts "{post}</td>
                            <td class="fit"><span class="badge">{status_label(&event.status)}</span></td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet">
                    <table>
                        <thead><tr><th>"Title"</th><th class="fit">"When"</th><th class="fit">"Status"</th></tr></thead>
                        <tbody>{lines}</tbody>
                    </table>
                </div>
                {table_pager(&events, &q)}
            }
            .into_any()
        } else if !q.is_empty() {
            view! { <section class="empty"><h2>"No matches."</h2></section> }.into_any()
        } else {
            view! { <section class="empty"><h2>"No dates yet."</h2></section> }.into_any()
        };
        let cadences = ["daily", "weekly", "biweekly", "monthly"].into_iter().map(|value| {
            view! { <option value=value selected=series.cadence == value>{cadence_label(value)}</option> }
        }).collect_view();
        view! {
            <div class="page-head">
                <div>
                    <p class="kicker">
                        {cadence_label(&series.cadence)}
                        {if series.active { " · active" } else { " · ended" }}
                    </p>
                    <h1>{series.title.clone()}</h1>
                </div>
                {series.active.then(|| view! {
                    <ActionButton method="POST".into() url=format!("{api_here}/end") label="End series".into()
                        class="danger".into() confirm="Skip every date that has not been posted?".into()/>
                })}
            </div>
            {series.active.then(|| view! {
                <JsonForm method="PATCH".into() url=api_here.clone()>
                    <h2>"Repeat"</h2>
                    <div class="grid">
                        <label class="field">"Cadence"<select name="cadence">{cadences}</select></label>
                        <label class="field">"Stop after"
                            <input name="count" data-kind="int" value=count placeholder="Leave empty to continue"/>
                        </label>
                        <label class="field">"Or end on"<input type="date" name="until" data-kind="opt" value=until/></label>
                    </div>
                    <p>"Leave both empty and the series keeps going. Changing the cadence moves upcoming dates onto the new rhythm. A shorter series drops upcoming dates that no longer fit, and a longer one adds them back. Posted dates stay, and a skipped date stays skipped."</p>
                    <div class="actions"><button class="primary" type="submit">"Save repeat"</button></div>
                </JsonForm>
            })}
            {searchable.then(|| view! { <Finder q=q.clone()/> })}
            {body}
        }
    })
}
