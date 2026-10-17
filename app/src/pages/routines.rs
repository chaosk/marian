use leptos::prelude::*;
use marian_types as dto;

use crate::format::weekday_name;
use crate::islands::{ActionButton, JsonForm};
use crate::{api, base, head, load, param};

#[component]
pub fn Routines() -> impl IntoView {
    let server = param("server");
    let data = async move { api::routines(&server).await };
    load(data, |rows| {
        let base = base();
        let cards = if rows.is_empty() {
            view! { <p class="empty">"No routines yet."</p> }.into_any()
        } else {
            rows.into_iter()
                .map(|row| {
                    let pills = (!row.choices.is_empty()).then(|| {
                        let pills = row
                            .choices
                            .iter()
                            .map(|choice| {
                                view! { <span class="pill"><span>{choice.label.clone()}</span><span>{choice.votes}</span></span> }
                            })
                            .collect_view();
                        view! { <div class="pills">{pills}</div> }
                    });
                    view! {
                        <a class="card" href=format!("{base}/routines/{}", row.id)>
                            <div class="card-top">
                                <h3>{row.title}</h3>
                                <span class="badge">{row.badge}</span>
                            </div>
                            <p class="meta">"#"{row.channel}</p>
                            {(!row.poll.is_empty()).then(|| view! { <p class="meta">{row.poll.clone()}</p> })}
                            {(!row.event.is_empty()).then(|| view! { <p class="meta">{row.event.clone()}</p> })}
                            {pills}
                        </a>
                    }
                })
                .collect_view()
                .into_any()
        };
        view! {
            <div class="page-head">
                <div><h1>"Routines"</h1></div>
                <a class="primary" href=format!("{base}/routines/new")>"New routine"</a>
            </div>
            <div class="stack">{cards}</div>
        }
    })
}

#[component]
pub fn NewRoutine() -> impl IntoView {
    let server = param("server");
    let data = async move { api::channels(&server).await };
    load(data, |channels| {
        let days: Vec<dto::DayCheck> = (0..7)
            .map(|n| dto::DayCheck {
                n,
                name: weekday_name(n).to_string(),
                on: true,
                votes: 0,
            })
            .collect();
        view! {
            {head("New routine")}
            {routine_form("/api".to_string() + &base() + "/routines", "POST".into(), None, channels, days)}
        }
    })
}

#[component]
pub fn RoutinePage() -> impl IntoView {
    let server = param("server");
    let id = param("id");
    let data = async move {
        let routine = api::routine(&server, &id).await?;
        let channels = api::channels(&server).await?;
        Ok((routine, channels))
    };
    load(data, |(routine, channels)| {
        let base = base();
        let here = format!("{base}/routines/{}", routine.id);
        let api_here = format!("/api{here}");
        let run = routine.run.clone().map(|run| {
            let open = run.open;
            let vote = open.then(|| {
                let choices = run
                    .days
                    .iter()
                    .filter(|day| day.on)
                    .map(|day| {
                        view! {
                            <ActionButton
                                method="POST".into()
                                url=format!("{api_here}/vote")
                                label=day.name.clone()
                                class={if day.votes > 0 { "primary".to_string() } else { "ghost".to_string() }}
                                body=format!("{{\"day\":{}}}", day.n)
                            />
                        }
                    })
                    .collect_view();
                view! { <div class="vote-choices">{choices}</div> }
            });
            view! {
                <section class="panel">
                    <h3>{if open { "Days people can pick now" } else { "Days for the next poll" }}</h3>
                    <JsonForm method="PUT".into() url=format!("{api_here}/days")>
                        <input type="hidden" name="scope" value="run"/>
                        <div class="checks">{day_checks(&run.days, open)}</div>
                        <div class="actions"><button class="primary" type="submit">"Save these days"</button></div>
                    </JsonForm>
                    {vote}
                </section>
            }
        });
        let history = (!routine.history.is_empty()).then(|| {
            let cards = routine
                .history
                .iter()
                .map(|row| {
                    view! {
                        <a class="card" href=format!("{base}/events/{}", row.event_id)>
                            {row.winner.clone()}" · "{row.when.clone()}
                        </a>
                    }
                })
                .collect_view();
            view! { <div class="stack">{cards}</div> }
        });
        view! {
            <div class="page-head">
                <div>
                    <p class="kicker">
                        "#"{routine.channel.clone()}
                        {if routine.active { " · active" } else { " · stopped" }}
                    </p>
                    <h1>{routine.title.clone()}</h1>
                </div>
                {routine.active.then(|| view! {
                    <ActionButton method="POST".into() url=format!("{api_here}/stop") label="Stop".into()
                        class="danger".into() confirm="Stop this routine?".into()/>
                })}
            </div>
            {run}
            {routine_form(api_here, "PUT".into(), Some(routine.clone()), channels.clone(), routine.usual.clone())}
            {history}
        }
    })
}

fn routine_form(
    url: String,
    method: String,
    routine: Option<dto::RoutineView>,
    channels: Vec<dto::Channel>,
    usual: Vec<dto::DayCheck>,
) -> impl IntoView {
    let selected = routine.as_ref().map(|r| r.channel_id);
    let options = channels
        .into_iter()
        .map(|channel| {
            view! {
                <option value=channel.id selected=selected == Some(channel.id)>"#"{channel.name}</option>
            }
        })
        .collect_view();
    let weekday = |selected: u8| {
        (0..7)
            .map(|n| view! { <option value=n selected=n == selected>{weekday_name(n)}</option> })
            .collect_view()
    };
    let title = routine.as_ref().map(|r| r.title.clone()).unwrap_or_default();
    let description = routine.as_ref().map(|r| r.description.clone()).unwrap_or_default();
    let poll_weekday = routine.as_ref().and_then(|r| r.poll_weekday.parse().ok()).unwrap_or(6);
    let lock_weekday = routine.as_ref().and_then(|r| r.lock_weekday.parse().ok()).unwrap_or(1);
    let poll_time = routine.as_ref().map(|r| r.poll_time.clone()).unwrap_or_else(|| "10:00".into());
    let lock_time = routine.as_ref().map(|r| r.lock_time.clone()).unwrap_or_else(|| "18:00".into());
    let start_time = routine.as_ref().map(|r| r.start_time.clone()).unwrap_or_else(|| "19:00".into());
    let duration = routine.as_ref().map(|r| r.duration_minutes).unwrap_or(120);
    let legend = if routine.as_ref().and_then(|r| r.run.as_ref()).is_some() {
        "Days later weeks start from"
    } else {
        "Days people can vote for"
    };
    view! {
        <JsonForm method=method.to_string() url class="grid".into()>
            <label class="field"><span>"Event title"</span><input name="title" required maxlength="120" value=title/></label>
            <label class="field"><span>"Channel"</span><select name="channel_id" data-kind="int" required>{options}</select></label>
            <label class="field span-2">
                <span>"Event description"</span>
                <textarea name="description" maxlength="2000" rows="4">{description}</textarea>
                <span class="meta">"Use {date} for the event date."</span>
            </label>
            <label class="field"><span>"Poll opens"</span><select name="poll_weekday" data-kind="int">{weekday(poll_weekday)}</select></label>
            <label class="field"><span>"Poll time"</span><input type="time" name="poll_time" value=poll_time required/></label>
            <label class="field"><span>"Poll locks"</span><select name="lock_weekday" data-kind="int">{weekday(lock_weekday)}</select></label>
            <label class="field"><span>"Lock time"</span><input type="time" name="lock_time" value=lock_time required/></label>
            <label class="field"><span>"Event starts at"</span><input type="time" name="start_time" value=start_time required/></label>
            <label class="field">
                <span>"Length in minutes"</span>
                <input type="number" name="duration_minutes" data-kind="int" min="15" max="1440" value=duration required/>
            </label>
            <fieldset class="span-2">
                <legend>{legend}</legend>
                <div class="checks">{day_checks(&usual, false)}</div>
            </fieldset>
            <div class="actions span-2"><button class="primary" type="submit">"Save routine"</button></div>
        </JsonForm>
    }
}

fn day_checks(days: &[dto::DayCheck], votes: bool) -> impl IntoView {
    days.iter()
        .map(|day| {
            let label = if votes {
                format!("{} · {}", day.name, day.votes)
            } else {
                day.name.clone()
            };
            view! {
                <label>
                    <input type="checkbox" name="days" data-kind="ints" value=day.n checked=day.on/>
                    " "<span>{label}</span>
                </label>
            }
        })
        .collect_view()
}
