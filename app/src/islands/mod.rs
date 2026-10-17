//! The parts of a page that run in the browser.

use leptos::prelude::*;
use marian_types as dto;
use serde_json::Value;

use crate::{api, client};

mod event_form;
mod signups;

pub use event_form::EventForm;
pub use signups::SignupPanel;

/// A search box that keeps its query in the address bar.
#[island]
pub fn Finder(q: String) -> impl IntoView {
    let needle = RwSignal::new(q);
    view! {
        <form class="finder" method="get" on:submit=move |ev| {
            ev.prevent_default();
            let q = needle.get();
            let path = client::path();
            let to = if q.trim().is_empty() {
                path
            } else {
                format!("{path}?q={}", api::encode(q.trim()))
            };
            client::go(&to);
        }>
            <input type="search" name="q" prop:value=move || needle.get() placeholder="Search" aria-label="Search"
                on:input=move |ev| needle.set(event_target_value(&ev))/>
        </form>
    }
}

/// The command desk: a log and a box that sends a line without leaving the page.
#[island]
pub fn CommandDesk(server: String, log: Vec<dto::CommandLog>) -> impl IntoView {
    let rows = RwSignal::new(log);
    let line = RwSignal::new(String::new());
    let problem = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let url = StoredValue::new(format!("/api/servers/{server}/commands"));
    let send = move |_| {
        let text = line.get();
        if text.trim().is_empty() {
            return;
        }
        let url = url.get_value();
        busy.set(true);
        client::spawn(async move {
            match api::send::<dto::CommandReply, _>("POST", &url, Some(&dto::CommandRun { line: text.clone() })).await {
                Ok(reply) => {
                    rows.update(|log| {
                        log.push(dto::CommandLog {
                            user: "You".into(),
                            line: text,
                            reply: reply.reply,
                        });
                    });
                    line.set(String::new());
                    problem.set(String::new());
                }
                Err(failure) => problem.set(failure.message),
            }
            busy.set(false);
        });
    };
    view! {
        <section class="cmd">
            <div class="log">
                {move || {
                    let log = rows.get();
                    if log.is_empty() {
                        view! { <p>"No commands yet."</p> }.into_any()
                    } else {
                        log.into_iter()
                            .map(|row| {
                                view! {
                                    <div>
                                        <p class="who">{row.user}</p>
                                        <p>{row.line.clone()}</p>
                                        <div class="reply md" inner_html=crate::markdown::render(&row.reply)></div>
                                    </div>
                                }
                            })
                            .collect_view()
                            .into_any()
                    }
                }}
            </div>
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            <form style="margin-top:12px" on:submit=move |ev| { ev.prevent_default(); send(()); }>
                <label class="field">"Command"
                    <input id="command-line" prop:value=move || line.get() placeholder="/event"
                        on:input=move |ev| line.set(event_target_value(&ev))/>
                </label>
                <button class="primary" type="submit" disabled=move || busy.get()>"Send"</button>
            </form>
        </section>
    }
}

/// Follows a change's reply: a notice and a place to go, or the same page again.
fn follow(reply: &Value) {
    let notice = reply.get("notice").and_then(Value::as_str).unwrap_or_default();
    let location = reply.get("location").and_then(Value::as_str).map(str::to_string);
    client::follow(&dto::Done {
        notice: notice.to_string(),
        location,
    });
}

/// One button that sends one change.
#[island]
pub fn ActionButton(
    method: String,
    url: String,
    label: String,
    #[prop(optional)] class: Option<String>,
    #[prop(optional)] confirm: Option<String>,
    /// A JSON body.
    #[prop(optional)]
    body: Option<String>,
) -> impl IntoView {
    let problem = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let request = StoredValue::new((
        method,
        url,
        body.unwrap_or_default(),
        confirm.unwrap_or_default(),
    ));
    let class = class.unwrap_or_default();
    let press = move |_| {
        let (method, url, body, confirm) = request.get_value();
        if !confirm.is_empty() && !client::confirm(&confirm) {
            return;
        }
        let body: Option<Value> = serde_json::from_str(&body).ok();
        busy.set(true);
        client::spawn(async move {
            match api::send::<Value, Value>(&method, &url, body.as_ref()).await {
                Ok(reply) => follow(&reply),
                Err(failure) => {
                    problem.set(failure.message);
                    busy.set(false);
                }
            }
        });
    };
    let class = if class.is_empty() {
        "ghost".to_string()
    } else {
        class
    };
    view! {
        <button type="button" class=class disabled=move || busy.get() on:click=press>{label}</button>
        {move || {
            let text = problem.get();
            (!text.is_empty()).then(|| view! { <span class="error inline">{text}</span> })
        }}
    }
}

/// A form sent as JSON. Fields say how they read with `data-kind`: `int` is a number
/// or null, `opt` is text or null, `csv` splits on commas, and checkboxes with
/// `ints` gather the checked numbers into a list. Other checkboxes are true or false.
#[island]
pub fn JsonForm(
    method: String,
    url: String,
    #[prop(optional)] class: Option<String>,
    /// Save on every change and stay on the page.
    #[prop(optional)]
    autosave: Option<bool>,
    children: Children,
) -> impl IntoView {
    let form = NodeRef::<leptos::html::Form>::new();
    let problem = RwSignal::new(String::new());
    let saved = RwSignal::new(String::new());
    let busy = RwSignal::new(false);
    let autosave = autosave.unwrap_or(false);
    let class = class.unwrap_or_default();
    let request = StoredValue::new((method, url));
    let send = move || {
        let Some(element) = form.get_untracked() else {
            return;
        };
        let body = read_form(&element);
        let (method, url) = request.get_value();
        busy.set(true);
        problem.set(String::new());
        client::spawn(async move {
            match api::send::<Value, Value>(&method, &url, Some(&body)).await {
                Ok(reply) if autosave => {
                    let notice = reply.get("notice").and_then(Value::as_str).unwrap_or_default();
                    saved.set(notice.to_string());
                    busy.set(false);
                }
                Ok(reply) => follow(&reply),
                Err(failure) => {
                    problem.set(failure.message);
                    busy.set(false);
                }
            }
        });
    };
    view! {
        <form
            node_ref=form
            class=class
            on:submit=move |ev| {
                ev.prevent_default();
                send();
            }
            on:change=move |_| {
                if autosave {
                    send();
                }
            }
        >
            {children()}
            <fieldset class="form-status" disabled=move || busy.get()>
                {move || {
                    let text = problem.get();
                    (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
                }}
                {move || {
                    let text = saved.get();
                    (!text.is_empty()).then(|| view! { <p class="notice">{text}</p> })
                }}
            </fieldset>
        </form>
    }
}

#[cfg(feature = "hydrate")]
fn read_form(form: &web_sys::HtmlFormElement) -> Value {
    use wasm_bindgen::JsCast;
    let mut out = serde_json::Map::new();
    let elements = form.elements();
    for i in 0..elements.length() {
        let Some(element) = elements.item(i) else { continue };
        let name = element.get_attribute("name").unwrap_or_default();
        if name.is_empty() {
            continue;
        }
        let kind = element.get_attribute("data-kind").unwrap_or_default();
        if let Some(input) = element.dyn_ref::<web_sys::HtmlInputElement>() {
            match input.type_().as_str() {
                "checkbox" if kind == "ints" => {
                    let list = out.entry(name).or_insert_with(|| Value::Array(Vec::new()));
                    if input.checked() {
                        if let Value::Array(items) = list {
                            items.push(typed(&input.value(), "int"));
                        }
                    }
                }
                "checkbox" => {
                    out.insert(name, Value::Bool(input.checked()));
                }
                "radio" => {
                    if input.checked() {
                        out.insert(name, typed(&input.value(), &kind));
                    }
                }
                "submit" | "button" => {}
                _ => {
                    out.insert(name, typed(&input.value(), &kind));
                }
            }
        } else if let Some(select) = element.dyn_ref::<web_sys::HtmlSelectElement>() {
            out.insert(name, typed(&select.value(), &kind));
        } else if let Some(area) = element.dyn_ref::<web_sys::HtmlTextAreaElement>() {
            out.insert(name, typed(&area.value(), &kind));
        }
    }
    Value::Object(out)
}

#[cfg(not(feature = "hydrate"))]
fn read_form<T>(_: &T) -> Value {
    Value::Null
}

#[cfg(feature = "hydrate")]
fn typed(text: &str, kind: &str) -> Value {
    let text = text.trim();
    match kind {
        "int" => text.parse::<i64>().map(Value::from).unwrap_or(Value::Null),
        "opt" if text.is_empty() => Value::Null,
        "csv" => Value::Array(
            text.split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(|part| Value::String(part.to_string()))
                .collect(),
        ),
        _ => Value::String(text.to_string()),
    }
}

/// A notice from the server, or from the page before.
#[island]
pub fn Flash(notice: String) -> impl IntoView {
    let text = RwSignal::new(notice);
    Effect::new(move |_| {
        let left = client::take_flash();
        if !left.is_empty() {
            text.set(left);
        }
    });
    move || {
        let notice = text.get();
        (!notice.is_empty()).then(|| view! { <p class="notice">{notice}</p> })
    }
}

/// The signed-in person's menu: reminders, Google Calendar, the feed, and sign out.
#[island]
pub fn AccountMenu(base: String, membership: dto::Membership) -> impl IntoView {
    let url = StoredValue::new(format!("/api{base}/me"));
    let reminders = RwSignal::new(membership.reminders_enabled);
    let auto_sync = RwSignal::new(membership.google.auto_sync);
    let problem = RwSignal::new(String::new());
    let patch = move |change: dto::MembershipPatch| {
        let url = url.get_value();
        client::spawn(async move {
            match api::send::<dto::Done, _>("PATCH", &url, Some(&change)).await {
                Ok(_) => {
                    if let Some(on) = change.reminders_enabled {
                        reminders.set(on);
                    }
                    if let Some(on) = change.google_auto_sync {
                        auto_sync.set(on);
                    }
                    problem.set(String::new());
                }
                Err(failure) => problem.set(failure.message),
            }
        });
    };
    let google = membership.google.clone();
    let connect = format!("/auth/google?next={base}");
    let google = if google.connected {
        let note = if google.email.is_empty() {
            "Connected.".to_string()
        } else {
            format!("Connected as {}.", google.email)
        };
        view! {
            <p class="account-label">"Google Calendar"</p>
            <p class="account-note">{note}</p>
            <label class="account-sync">
                <input type="checkbox" prop:checked=move || auto_sync.get()
                    on:change=move |ev| patch(dto::MembershipPatch {
                        google_auto_sync: Some(event_target_checked(&ev)),
                        ..Default::default()
                    })/>
                " Keep your Google Calendar updated"
            </label>
            <form method="post" action="/auth/google/disconnect">
                <input type="hidden" name="next" value=base.clone()/>
                <button type="submit">"Disconnect"</button>
            </form>
        }
        .into_any()
    } else if google.reconnect {
        let note = if google.email.is_empty() {
            "Needs a new connection.".to_string()
        } else {
            format!("Needs a new connection, last used {}.", google.email)
        };
        view! {
            <a href=connect>"Google Calendar"</a>
            <p class="account-note">{note}</p>
        }
        .into_any()
    } else if google.ready {
        view! { <a href=connect>"Google Calendar"</a> }.into_any()
    } else {
        view! {
            <p class="account-label">"Google Calendar"</p>
            <p class="account-note">"Not set up on this server yet."</p>
        }
        .into_any()
    };
    view! {
        <details class="switcher account-menu">
            <summary>
                <strong>{membership.name}</strong>
                <span class="switcher-chevron" aria-hidden="true"></span>
            </summary>
            <div class="switcher-menu">
                <button type="button" class="reminder-toggle"
                    aria-pressed=move || if reminders.get() { "true" } else { "false" }
                    on:click=move |_| patch(dto::MembershipPatch {
                        reminders_enabled: Some(!reminders.get_untracked()),
                        ..Default::default()
                    })>
                    <span>"Reminders"</span>
                    <span class=move || if reminders.get() { "toggle on" } else { "toggle" } aria-hidden="true"><span></span></span>
                </button>
                {move || {
                    let text = problem.get();
                    (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
                }}
                {google}
                <a class="calendar" href=membership.calendar_url>"Calendar feed"</a>
                <form method="post" action="/logout">
                    <button type="submit">"Sign out"</button>
                </form>
            </div>
        </details>
    }
}

/// A time zone list narrowed by a search box. The chosen zone always stays listed.
#[island]
pub fn ZonePicker(groups: Vec<dto::ZoneGroup>, enabled: bool) -> impl IntoView {
    let chosen = groups
        .iter()
        .flat_map(|group| group.zones.iter())
        .find(|zone| zone.selected)
        .map(|zone| zone.id.clone())
        .unwrap_or_default();
    let chosen = RwSignal::new(chosen);
    let needle = RwSignal::new(String::new());
    let groups = StoredValue::new(groups);
    let shown = move || {
        let needle = needle.get().trim().to_lowercase();
        let chosen = chosen.get();
        groups.with_value(|groups| {
            groups
                .iter()
                .filter_map(|group| {
                    let options = group
                        .zones
                        .iter()
                        .filter(|zone| {
                            zone.id == chosen
                                || needle.is_empty()
                                || zone.label.to_lowercase().contains(&needle)
                                || zone.id.to_lowercase().contains(&needle)
                        })
                        .map(|zone| {
                            let selected = zone.id == chosen;
                            view! { <option value=zone.id.clone() selected=selected>{zone.label.clone()}</option> }
                        })
                        .collect::<Vec<_>>();
                    (!options.is_empty()).then(|| view! { <optgroup label=group.offset.clone()>{options}</optgroup> })
                })
                .collect_view()
        })
    };
    view! {
        <input type="search" placeholder="Search cities" autocomplete="off" disabled=!enabled
            on:input=move |ev| needle.set(event_target_value(&ev))/>
        <select class="zone-list" name="timezone" size="8" disabled=!enabled
            on:change=move |ev| chosen.set(event_target_value(&ev))>
            {shown}
        </select>
    }
}
