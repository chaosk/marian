use chrono::NaiveDateTime;
use leptos::prelude::*;
use marian_types as dto;
use marian_types::labels::PALETTE;

use crate::{api, client, markdown};

const MAX_OPTIONS: usize = 8;
const MAX_REMINDERS: usize = 5;

#[derive(Clone)]
struct OptionRow {
    n: usize,
    id: Option<i64>,
    emoji: RwSignal<String>,
    name: RwSignal<String>,
    capacity: RwSignal<String>,
    role_id: RwSignal<String>,
    waitlist: RwSignal<bool>,
    reminders: RwSignal<bool>,
}

#[derive(Clone)]
struct ReminderRow {
    n: usize,
    offset: RwSignal<String>,
    message: RwSignal<String>,
}

/// The create and edit form, with a live Discord preview.
#[island]
pub fn EventForm(
    server: String,
    event_id: String,
    heading: String,
    series_note: String,
    status: String,
    series_locked: bool,
    channels: Vec<dto::Channel>,
    seed: dto::EventSeed,
) -> impl IntoView {
    let lock_options = status == "posted";
    let title = RwSignal::new(seed.title);
    let description = RwSignal::new(seed.description);
    let location = RwSignal::new(seed.location);
    let channel_id = RwSignal::new(seed.channel_id);
    let color = RwSignal::new(seed.color);
    let start = RwSignal::new(seed.starts_local);
    let duration = RwSignal::new(seed.duration_minutes.to_string());
    let cutoff = RwSignal::new(seed.cutoff_local);
    let post_at = RwSignal::new(seed.post_local);
    let thread_enabled = RwSignal::new(seed.thread_enabled);
    let multiple_signups = RwSignal::new(seed.multiple_signups);
    let mention_everyone = RwSignal::new(seed.mention_everyone);
    let attendee_role_id = RwSignal::new(
        seed.attendee_role_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
    );
    let mention_ids = RwSignal::new(seed.mention_role_ids);
    let restrict_ids = RwSignal::new(seed.restrict_role_ids);
    let cadence = RwSignal::new(
        seed.repeat
            .as_ref()
            .map(|r| r.cadence.clone())
            .unwrap_or_else(|| "none".into()),
    );
    let repeat_count = RwSignal::new(
        seed.repeat
            .as_ref()
            .and_then(|r| r.count)
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let repeat_until = RwSignal::new(
        seed.repeat
            .as_ref()
            .and_then(|r| r.until)
            .map(|d| d.to_string())
            .unwrap_or_default(),
    );
    let apply_series = RwSignal::new(false);
    let options = RwSignal::new(
        seed.options
            .into_iter()
            .enumerate()
            .map(|(n, opt)| OptionRow {
                n,
                id: opt.id,
                emoji: RwSignal::new(opt.emoji),
                name: RwSignal::new(opt.name),
                capacity: RwSignal::new(opt.capacity.map(|n| n.to_string()).unwrap_or_default()),
                role_id: RwSignal::new(opt.role_id.map(|id| id.to_string()).unwrap_or_default()),
                waitlist: RwSignal::new(opt.waitlist),
                reminders: RwSignal::new(opt.reminders),
            })
            .collect::<Vec<_>>(),
    );
    let reminders = RwSignal::new(
        seed.reminders
            .into_iter()
            .enumerate()
            .map(|(n, rem)| ReminderRow {
                n,
                offset: RwSignal::new(rem.offset_minutes.to_string()),
                message: RwSignal::new(rem.message),
            })
            .collect::<Vec<_>>(),
    );
    let roles = RwSignal::new(Vec::<dto::Role>::new());
    let presets = RwSignal::new(Vec::<dto::Preset>::new());
    let problem = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let ids = StoredValue::new((server.clone(), event_id.clone()));

    Effect::new({
        let server = server.clone();
        move |_| {
            let server = server.clone();
            client::spawn(async move {
                if let Ok(list) = api::roles(&server).await {
                    roles.set(list);
                }
            });
        }
    });
    if event_id.is_empty() {
        Effect::new({
            let server = server.clone();
            move |_| {
                let server = server.clone();
                client::spawn(async move {
                    if let Ok(list) = api::presets(&server).await {
                        presets.set(list);
                    }
                });
            }
        });
    }

    let add_option = move |_| {
        options.update(|rows| {
            if rows.len() < MAX_OPTIONS {
                let n = rows.iter().map(|r| r.n).max().map_or(0, |n| n + 1);
                rows.push(OptionRow {
                    n,
                    id: None,
                    emoji: RwSignal::new(String::new()),
                    name: RwSignal::new(String::new()),
                    capacity: RwSignal::new(String::new()),
                    role_id: RwSignal::new(String::new()),
                    waitlist: RwSignal::new(false),
                    reminders: RwSignal::new(true),
                });
            }
        })
    };
    let add_reminder = move |_| {
        reminders.update(|rows| {
            if rows.len() < MAX_REMINDERS {
                let n = rows.iter().map(|r| r.n).max().map_or(0, |n| n + 1);
                rows.push(ReminderRow {
                    n,
                    offset: RwSignal::new(String::new()),
                    message: RwSignal::new(String::new()),
                });
            }
        })
    };

    let save = move |action: dto::SaveAction| {
        let (server, event_id) = ids.get_value();
        let draft = match collect(
            &title.get(),
            &description.get(),
            &location.get(),
            channel_id.get(),
            &color.get(),
            &start.get(),
            &duration.get(),
            &cutoff.get(),
            &post_at.get(),
            &attendee_role_id.get(),
            thread_enabled.get(),
            multiple_signups.get(),
            mention_everyone.get(),
            mention_ids.get(),
            restrict_ids.get(),
            &options.get(),
            &reminders.get(),
            series_locked,
            &cadence.get(),
            &repeat_count.get(),
            &repeat_until.get(),
            apply_series.get(),
            action,
        ) {
            Ok(draft) => draft,
            Err(message) => {
                problem.set(message);
                return;
            }
        };
        let url = if event_id.is_empty() {
            format!("/api/servers/{server}/events")
        } else {
            format!("/api/servers/{server}/events/{event_id}")
        };
        let method = if event_id.is_empty() { "POST" } else { "PUT" };
        busy.set(true);
        problem.set(String::new());
        client::spawn(async move {
            match api::send::<dto::Done, _>(method, &url, Some(&draft)).await {
                Ok(done) => client::follow(&done),
                Err(failure) => {
                    problem.set(failure.message);
                    busy.set(false);
                }
            }
        });
    };

    let channel_options = channels
        .into_iter()
        .map(|channel| {
            view! { <option value=channel.id selected=channel.id == channel_id.get_untracked()>"#"{channel.name}</option> }
        })
        .collect_view();
    let colors = PALETTE
        .iter()
        .map(|(value, label)| {
            view! { <option value=*value selected=*value == color.get_untracked()>{*label}</option> }
        })
        .collect_view();
    let posted = status == "posted";
    let queued = matches!(status.as_str(), "scheduled" | "failed");
    let show_draft = !posted;
    let show_schedule = !posted;
    let show_publish = !posted;
    let show_save = posted || queued;

    view! {
        <div class="page-head"><div><h1>{heading}</h1></div></div>
        {move || {
            let text = problem.get();
            (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
        }}
        {(!series_note.is_empty()).then(|| view! { <p class="notice">{series_note.clone()}</p> })}
        {event_id.is_empty().then(|| view! {
            {move || {
                let list = presets.get();
                (!list.is_empty()).then(|| {
                    let choices = list.into_iter().map(|preset| {
                        view! { <option value=preset.id>{preset.name}</option> }
                    }).collect_view();
                    view! {
                        <label class="field">"Start from a preset"
                            <select on:change=move |ev| {
                                let id = event_target_value(&ev);
                                if id.is_empty() { return; }
                                let server = ids.get_value().0;
                                client::spawn(async move {
                                    if let Ok(preset) = api::preset(&server, &id).await {
                                        multiple_signups.set(preset.multiple_signups);
                                        options.set(preset.options.into_iter().enumerate().map(|(n, opt)| OptionRow {
                                            n,
                                            id: None,
                                            emoji: RwSignal::new(opt.emoji),
                                            name: RwSignal::new(opt.name),
                                            capacity: RwSignal::new(opt.capacity.map(|n| n.to_string()).unwrap_or_default()),
                                            role_id: RwSignal::new(opt.role_id.map(|id| id.to_string()).unwrap_or_default()),
                                            waitlist: RwSignal::new(opt.waitlist),
                                            reminders: RwSignal::new(opt.reminders),
                                        }).collect());
                                    }
                                });
                            }>
                                <option value="">"Keep the current options"</option>
                                {choices}
                            </select>
                        </label>
                    }
                })
            }}
        })}
        <div class="form-layout">
            <fieldset disabled=move || busy.get()>
                <div class="grid">
                    <label class="field span-2">"Title"
                        <input required prop:value=move || title.get() on:input=move |ev| title.set(event_target_value(&ev))/>
                    </label>
                    <label class="field span-2">"Description"
                        <textarea on:input=move |ev| description.set(event_target_value(&ev))>{description.get_untracked()}</textarea>
                        <span class="meta">"Use {date} for the event date."</span>
                    </label>
                    <label class="field span-2">"Location"
                        <input maxlength="200" prop:value=move || location.get() on:input=move |ev| location.set(event_target_value(&ev))/>
                    </label>
                    <label class="field">"Channel"
                        <select on:change=move |ev| channel_id.set(event_target_value(&ev).parse().unwrap_or_default())>{channel_options}</select>
                    </label>
                    <label class="field">"Color"
                        <select on:change=move |ev| color.set(event_target_value(&ev))>{colors}</select>
                    </label>
                    <label class="field">"Starts"
                        <input type="datetime-local" required prop:value=move || start.get() on:input=move |ev| start.set(event_target_value(&ev))/>
                    </label>
                    <label class="field">"Duration in minutes"
                        <input inputmode="numeric" prop:value=move || duration.get() on:input=move |ev| duration.set(event_target_value(&ev))/>
                    </label>
                    <label class="field">"Signup cutoff"
                        <input type="datetime-local" prop:value=move || cutoff.get() on:input=move |ev| cutoff.set(event_target_value(&ev))/>
                    </label>
                    <label class="field">"Post at"
                        <input type="datetime-local" prop:value=move || post_at.get() on:input=move |ev| post_at.set(event_target_value(&ev))/>
                    </label>
                </div>
                <h2>"Signup options"</h2>
                <p class="muted">"Capacity blank means unlimited. A waitlist needs a capacity."</p>
                <div>
                    <For each=move || options.get() key=|row| row.n children=move |row| {
                        let n = row.n;
                        let role_list = move || {
                            let chosen = row.role_id.get();
                            roles.get().into_iter().map(|role| {
                                view! { <option value=role.id selected=chosen == role.id.to_string()>{role.name}</option> }
                            }).collect_view()
                        };
                        view! {
                            <div class="option">
                                <div class="option-fields">
                                    <input prop:value=move || row.emoji.get() aria-label="Emoji" maxlength="8"
                                        on:input=move |ev| row.emoji.set(event_target_value(&ev))/>
                                    <input prop:value=move || row.name.get() aria-label="Option name" maxlength="40" placeholder="Name"
                                        on:input=move |ev| row.name.set(event_target_value(&ev))/>
                                    <input prop:value=move || row.capacity.get() aria-label="Capacity" placeholder="No limit" inputmode="numeric"
                                        on:input=move |ev| row.capacity.set(event_target_value(&ev))/>
                                    <select aria-label="Role limit" on:change=move |ev| row.role_id.set(event_target_value(&ev))>
                                        <option value="">"Any role"</option>
                                        {role_list}
                                    </select>
                                    {(!lock_options).then(|| view! {
                                        <button class="ghost option-remove" type="button"
                                            on:click=move |_| options.update(|rows| rows.retain(|r| r.n != n))>"Remove"</button>
                                    })}
                                </div>
                                <div class="checks">
                                    <label>
                                        <input type="checkbox" prop:checked=move || row.waitlist.get()
                                            on:change=move |ev| row.waitlist.set(event_target_checked(&ev))/>
                                        " "<span>"Waitlist"</span>
                                    </label>
                                    <label>
                                        <input type="checkbox" prop:checked=move || row.reminders.get()
                                            on:change=move |ev| row.reminders.set(event_target_checked(&ev))/>
                                        " "<span>"Reminders and attendee role"</span>
                                    </label>
                                </div>
                            </div>
                        }
                    }/>
                </div>
                {(!lock_options).then(|| view! {
                    <button class="ghost option-add" type="button" hidden=move || options.with(|r| r.len() >= MAX_OPTIONS)
                        on:click=add_option>"Add option"</button>
                })}
                <h2>"Reminders"</h2>
                <div>
                    <For each=move || reminders.get() key=|row| row.n children=move |row| {
                        let n = row.n;
                        view! {
                            <div class="rem-row">
                                <input prop:value=move || row.offset.get() placeholder="minutes" inputmode="numeric" aria-label="Minutes before"
                                    on:input=move |ev| row.offset.set(event_target_value(&ev))/>
                                <input prop:value=move || row.message.get() placeholder="Optional note, such as flasks and food" aria-label="Reminder note"
                                    on:input=move |ev| row.message.set(event_target_value(&ev))/>
                                <button class="ghost reminder-remove" type="button"
                                    on:click=move |_| reminders.update(|rows| rows.retain(|r| r.n != n))>"Remove"</button>
                            </div>
                        }
                    }/>
                </div>
                <button class="ghost option-add" type="button" hidden=move || reminders.with(|r| r.len() >= MAX_REMINDERS)
                    on:click=add_reminder>"Add reminder"</button>
                <h2>"Access"</h2>
                <div class="settings">
                    <label class="setting">
                        <input type="checkbox" prop:checked=move || thread_enabled.get()
                            on:change=move |ev| thread_enabled.set(event_target_checked(&ev))/>
                        <span><strong>"Event thread"</strong><span class="muted">"This event opens a Discord thread."</span></span>
                    </label>
                    <label class="setting">
                        <input type="checkbox" prop:checked=move || multiple_signups.get()
                            on:change=move |ev| multiple_signups.set(event_target_checked(&ev))/>
                        <span><strong>"Allow several options"</strong><span class="muted">"A person can choose more than one option at once."</span></span>
                    </label>
                </div>
                <label class="field">"Attendee role"
                    <select on:change=move |ev| attendee_role_id.set(event_target_value(&ev))>
                        <option value="">"None"</option>
                        {move || {
                            let chosen = attendee_role_id.get();
                            roles.get().into_iter().map(|role| {
                                view! { <option value=role.id selected=chosen == role.id.to_string()>{role.name}</option> }
                            }).collect_view()
                        }}
                    </select>
                    <span class="muted">"People who accept on a reminding option receive this role."</span>
                </label>
                <p>"Mention"<br/><span class="muted">"The Discord post mentions the roles you check."</span></p>
                <div class="checks">
                    <label>
                        <input type="checkbox" prop:checked=move || mention_everyone.get()
                            on:change=move |ev| mention_everyone.set(event_target_checked(&ev))/>
                        " "<span>"@everyone"</span>
                    </label>
                    {move || {
                        let chosen = mention_ids.get();
                        roles.get().into_iter().map(|role| {
                            let id = role.id;
                            view! {
                                <label>
                                    <input type="checkbox" prop:checked=chosen.contains(&id)
                                        on:change=move |ev| mention_ids.update(|ids| {
                                            if event_target_checked(&ev) { ids.push(id); } else { ids.retain(|n| *n != id); }
                                        })/>
                                    " "<span>{role.name}</span>
                                </label>
                            }
                        }).collect_view()
                    }}
                </div>
                <p>"Limit the whole event to"<br/><span class="muted">"Only people with a checked role can sign up."</span></p>
                <div class="checks">
                    {move || {
                        let chosen = restrict_ids.get();
                        roles.get().into_iter().map(|role| {
                            let id = role.id;
                            view! {
                                <label>
                                    <input type="checkbox" prop:checked=chosen.contains(&id)
                                        on:change=move |ev| restrict_ids.update(|ids| {
                                            if event_target_checked(&ev) { ids.push(id); } else { ids.retain(|n| *n != id); }
                                        })/>
                                    " "<span>{role.name}</span>
                                </label>
                            }
                        }).collect_view()
                    }}
                </div>
                {(!series_locked).then(|| view! {
                    <h2>"Repeat"</h2>
                    <div class="grid">
                        <label class="field">"Cadence"
                            <select on:change=move |ev| cadence.set(event_target_value(&ev))>
                                <option value="none" selected=cadence.get_untracked() == "none">"Does not repeat"</option>
                                <option value="daily" selected=cadence.get_untracked() == "daily">"Daily"</option>
                                <option value="weekly" selected=cadence.get_untracked() == "weekly">"Weekly"</option>
                                <option value="biweekly" selected=cadence.get_untracked() == "biweekly">"Every two weeks"</option>
                                <option value="monthly" selected=cadence.get_untracked() == "monthly">"Monthly"</option>
                            </select>
                        </label>
                        <label class="field">"Stop after"
                            <input prop:value=move || repeat_count.get() placeholder="Leave empty to continue"
                                on:input=move |ev| repeat_count.set(event_target_value(&ev))/>
                        </label>
                        <label class="field">"Or end on"
                            <input type="date" prop:value=move || repeat_until.get()
                                on:input=move |ev| repeat_until.set(event_target_value(&ev))/>
                        </label>
                    </div>
                    <p>"Leave both empty and the series keeps going."</p>
                })}
                {series_locked.then(|| view! {
                    <label class="checks">
                        <input type="checkbox" prop:checked=move || apply_series.get()
                            on:change=move |ev| apply_series.set(event_target_checked(&ev))/>
                        " "<span>"Also update upcoming dates in this series. This date's start time stays its own."</span>
                    </label>
                })}
                <div class="actions">
                    {show_draft.then(|| view! { <button class="ghost" type="button" on:click=move |_| save(dto::SaveAction::Draft)>"Save draft"</button> })}
                    {show_schedule.then(|| view! { <button class="ghost" type="button" on:click=move |_| save(dto::SaveAction::Schedule)>"Schedule post"</button> })}
                    {show_publish.then(|| view! { <button class="primary" type="button" on:click=move |_| save(dto::SaveAction::Publish)>"Post now"</button> })}
                    {show_save.then(|| view! { <button class="primary" type="button" on:click=move |_| save(dto::SaveAction::Save)>"Save changes"</button> })}
                </div>
            </fieldset>
            <aside class="form-preview">
                <h2>"Discord preview"</h2>
                <Preview title description location start duration color options/>
            </aside>
        </div>
    }
}

fn collect(
    title: &str,
    description: &str,
    location: &str,
    channel_id: i64,
    color: &str,
    start: &str,
    duration: &str,
    cutoff: &str,
    post_at: &str,
    attendee_role_id: &str,
    thread_enabled: bool,
    multiple_signups: bool,
    mention_everyone: bool,
    mention_role_ids: Vec<i64>,
    restrict_role_ids: Vec<i64>,
    options: &[OptionRow],
    reminders: &[ReminderRow],
    series_locked: bool,
    cadence: &str,
    repeat_count: &str,
    repeat_until: &str,
    apply_series: bool,
    action: dto::SaveAction,
) -> Result<dto::EventDraft, String> {
    let starts_local = parse_local(start).ok_or_else(|| "Use a date and a time.".to_string())?;
    let duration_minutes = duration
        .trim()
        .parse()
        .map_err(|_| "Duration needs to be a number of minutes.".to_string())?;
    let repeat = if series_locked || cadence == "none" || cadence.is_empty() {
        None
    } else {
        Some(dto::Repeat {
            cadence: cadence.to_string(),
            count: if repeat_count.trim().is_empty() {
                None
            } else {
                Some(
                    repeat_count
                        .trim()
                        .parse()
                        .map_err(|_| "The date count needs to be a number.".to_string())?,
                )
            },
            until: if repeat_until.trim().is_empty() {
                None
            } else {
                Some(
                    chrono::NaiveDate::parse_from_str(repeat_until.trim(), "%Y-%m-%d")
                        .map_err(|_| "Use a calendar date.".to_string())?,
                )
            },
        })
    };
    Ok(dto::EventDraft {
        title: title.to_string(),
        description: description.to_string(),
        location: location.to_string(),
        channel_id,
        color: color.to_string(),
        starts_local,
        duration_minutes,
        cutoff_local: parse_local(cutoff),
        post_local: parse_local(post_at),
        attendee_role_id: attendee_role_id.parse().ok(),
        thread_enabled,
        multiple_signups,
        mention_everyone,
        mention_role_ids,
        restrict_role_ids,
        options: options
            .iter()
            .filter(|row| !row.name.get().trim().is_empty())
            .map(|row| dto::SignupOption {
                id: row.id,
                name: row.name.get(),
                emoji: row.emoji.get(),
                capacity: row.capacity.get().trim().parse().ok(),
                waitlist: row.waitlist.get(),
                role_id: row.role_id.get().parse().ok(),
                reminders: row.reminders.get(),
            })
            .collect(),
        reminders: reminders
            .iter()
            .filter(|row| !row.offset.get().trim().is_empty())
            .map(|row| dto::Reminder {
                offset_minutes: row.offset.get().trim().parse().unwrap_or(0),
                message: row.message.get(),
                sent: false,
            })
            .collect(),
        repeat,
        action,
        apply_series,
    })
}

fn parse_local(value: &str) -> Option<NaiveDateTime> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S"))
        .ok()
}

#[component]
fn Preview(
    title: RwSignal<String>,
    description: RwSignal<String>,
    location: RwSignal<String>,
    start: RwSignal<String>,
    duration: RwSignal<String>,
    color: RwSignal<String>,
    options: RwSignal<Vec<OptionRow>>,
) -> impl IntoView {
    view! {
        <div class="discord">
            <div class="embed" style=move || format!("border-left-color: {}", color.get())>
                <h3>{move || {
                    let t = title.get();
                    if t.trim().is_empty() { "Untitled event".into() } else { t }
                }}</h3>
                {move || {
                    let text = description.get();
                    (!text.trim().is_empty()).then(|| view! { <div class="d-desc md" inner_html=markdown::render(&text)></div> })
                }}
                {move || {
                    let place = location.get();
                    (!place.trim().is_empty()).then(|| view! {
                        <p class="d-kicker">"Location"</p>
                        <p class="d-desc">"📍 "{place}</p>
                    })
                }}
                <p class="d-kicker">"Time"</p>
                <p class="d-desc">{move || time_span(&start.get(), &duration.get())}</p>
                <div class="d-fields">
                    {move || {
                        options.get().into_iter().filter(|row| !row.name.get().trim().is_empty()).map(|row| {
                            let capacity = row.capacity.get();
                            let spots = if capacity.trim().is_empty() { "0".into() } else { format!("0/{}", capacity.trim()) };
                            view! {
                                <div class="d-field">
                                    <p class="d-field-name">{row.emoji.get()}" "<strong>{row.name.get()}</strong>" ("{spots}")"</p>
                                    <p class="d-field-value">"-"</p>
                                </div>
                            }
                        }).collect_view()
                    }}
                </div>
            </div>
            <div class="btn-row">
                {move || {
                    options.get().into_iter().filter(|row| !row.name.get().trim().is_empty()).map(|row| {
                        let emoji = row.emoji.get();
                        let label = if emoji.trim().is_empty() { row.name.get() } else { emoji };
                        view! { <button class="d-btn" type="button" disabled>{label}</button> }
                    }).collect_view()
                }}
            </div>
        </div>
    }
}

fn time_span(start: &str, minutes: &str) -> String {
    let Some(begin) = parse_local(start) else {
        return "Pick a start time".into();
    };
    let length: i64 = minutes.trim().parse().unwrap_or(0).max(0);
    let end = begin + chrono::Duration::minutes(length);
    let fmt = |dt: NaiveDateTime| dt.format("%a %-d %b, %H:%M").to_string();
    if length == 0 {
        return fmt(begin);
    }
    if begin.date() == end.date() {
        format!("{} - {}", fmt(begin), end.format("%H:%M"))
    } else {
        format!("{} - {}", fmt(begin), fmt(end))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_spells_out_the_time() {
        assert_eq!(time_span("2026-10-23T19:00", "120"), "Fri 23 Oct, 19:00 - 21:00");
        assert_eq!(
            time_span("2026-12-31T23:30", "60"),
            "Thu 31 Dec, 23:30 - Fri 1 Jan, 00:30"
        );
        assert_eq!(time_span("", "60"), "Pick a start time");
    }
}
