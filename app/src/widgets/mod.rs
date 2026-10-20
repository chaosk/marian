//! Interactive pieces pages share: forms, live updates, menus.

use leptos::prelude::*;
use marian_types as dto;
use serde_json::Value;

use crate::{api, client};

mod inline;
mod routine;
mod signups;

pub use inline::{
    EventAttendeeRole, LeaveGuard, LocalDraft, EventColor, EventCutoff, EventDescription, EventLocation, EventMentions, EventReminders, EventRepeat, PostButtons, EventChannel, ThreadToggle, ApplyToSeries,
    EventRestrictions, EventTitle, EventWhen, MultipleToggle,
};
pub use routine::{
    routine_problems, template_event, PollMoment, RoutineChannel, RoutineDescription, RoutineEdit, RoutineTime, RoutineTitle, PollTally, UsualDays, tally, TallyRow,
};
pub use signups::{numbered, unsaved_signups, SignupPanel};

/// Listens for a change on this server and reloads the page data.
#[component]
pub fn Live(server: String) -> impl IntoView {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;
        let url = format!("/api/servers/{server}/live");
        let here = server.clone();
        if let Ok(source) = web_sys::EventSource::new(&url) {
            let on_change = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| {
                crate::cache::refresh(&here);
            });
            source.set_onmessage(Some(on_change.as_ref().unchecked_ref()));
            on_change.forget();
            on_cleanup(move || source.close());
        }
    });
    #[cfg(not(feature = "hydrate"))]
    let _ = server;
}

/// A search box that keeps its query in the address bar.
#[component]
pub fn Finder(q: String) -> impl IntoView {
    let needle = RwSignal::new(q);
    view! {
        <form class="finder" method="get" on:submit={
            let go = client::use_go();
            move |ev| {
                ev.prevent_default();
                let q = needle.get();
                let path = client::path();
                let to = if q.trim().is_empty() {
                    path
                } else {
                    format!("{path}?q={}", api::encode(q.trim()))
                };
                go(&to);
            }
        }>
            <input type="search" name="q" prop:value=move || needle.get() placeholder="Search" aria-label="Search"
                on:input=move |ev| needle.set(event_target_value(&ev))/>
        </form>
    }
}

/// The command desk: a log and a box that sends a line without leaving the page.
#[component]
pub fn CommandDesk(server: String, log: Vec<dto::CommandLog>) -> impl IntoView {
    let rows = RwSignal::new(log);
    let line = RwSignal::new(String::new());
    let problem = RwSignal::new(String::new());
    let url = StoredValue::new(format!("/api/servers/{server}/commands"));
    let run = client::action(move |text: &String| {
        let text = text.clone();
        let url = url.get_value();
        async move {
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
        }
    });
    let send = move |_| {
        let text = line.get();
        if !text.trim().is_empty() {
            run.dispatch(text);
        }
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
                <button class="primary" type="submit" disabled=move || run.pending().get()>"Send"</button>
            </form>
        </section>
    }
}

/// Where a change sent the person, when it moved them.
fn reply_done(reply: &Value) -> dto::Done {
    dto::Done {
        location: reply.get("location").and_then(Value::as_str).map(str::to_string),
    }
}

/// One button that sends one change.
#[component]
pub fn ActionButton(
    #[prop(into)] method: String,
    #[prop(into)] url: String,
    #[prop(into)] label: String,
    #[prop(optional, into)] class: Option<String>,
    #[prop(optional, into)] confirm: Option<String>,
    /// A JSON body.
    #[prop(optional, into)]
    body: Option<String>,
    /// What the toast says after a successful change. Empty means no toast.
    #[prop(optional, into)]
    done: Option<String>,
) -> impl IntoView {
    let problem = RwSignal::new(String::new());
    let said = done.unwrap_or_else(|| {
        let label = label.trim();
        if label.is_empty() {
            String::new()
        } else if label.ends_with('.') {
            label.to_string()
        } else {
            format!("{label}.")
        }
    });
    let request = StoredValue::new((
        method,
        url,
        body.unwrap_or_default(),
        confirm.unwrap_or_default(),
        said,
    ));
    let class = class.unwrap_or_default();
    let follow = client::use_follow();
    let asking = RwSignal::new(false);
    let run = client::action(move |_: &()| {
        let (method, url, body, _, said) = request.get_value();
        let body: Option<Value> = serde_json::from_str(&body).ok();
        let follow = follow.clone();
        let toasts = client::toast_list();
        async move {
            match api::send::<Value, Value>(&method, &url, body.as_ref()).await {
                Ok(reply) => {
                    if let Some(list) = toasts {
                        client::push_toast(list, said);
                    }
                    follow(reply_done(&reply));
                }
                Err(failure) => problem.set(failure.message),
            }
        }
    });
    let send = move || {
        run.dispatch(());
    };
    let class = if class.is_empty() {
        "ghost".to_string()
    } else {
        class
    };
    // A change that needs a yes asks in place of the button, not in a browser dialog.
    let question = StoredValue::new(request.with_value(|(_, _, _, confirm, _)| confirm.clone()));
    let send = StoredValue::new(send);
    let press = move |_| {
        if question.with_value(String::is_empty) {
            send.with_value(|send| send());
        } else {
            asking.set(true);
        }
    };
    let confirm_class = format!("{class} confirm-yes");
    view! {
        {move || if asking.get() {
            view! {
                <InlineConfirm question=question.get_value() yes=label.clone() class=confirm_class.clone()
                    on_yes=Callback::new(move |_| {
                        asking.set(false);
                        send.with_value(|send| send());
                    })
                    on_no=Callback::new(move |_| asking.set(false))/>
            }
            .into_any()
        } else {
            view! { <button type="button" class=class.clone() disabled=move || run.pending().get() on:click=press>{label.clone()}</button> }.into_any()
        }}
        {move || {
            let text = problem.get();
            (!text.is_empty()).then(|| view! { <span class="error inline">{text}</span> })
        }}
    }
}

/// A question asked in place: the confirm button and Cancel, with the confirm button
/// focused so Enter answers yes and Escape backs out.
#[component]
pub fn InlineConfirm(
    #[prop(into)] question: String,
    #[prop(into)] yes: String,
    #[prop(optional, into)] class: Option<String>,
    on_yes: Callback<()>,
    on_no: Callback<()>,
) -> impl IntoView {
    let button = NodeRef::<leptos::html::Button>::new();
    Effect::new(move |_| {
        if let Some(button) = button.get() {
            let _ = button.focus();
        }
    });
    let class = class.unwrap_or_else(|| "danger".into());
    let label = question.clone();
    view! {
        <span class="confirm-inline" role="alertdialog" aria-label=label
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    // Escape backs out of the question only, not the form around it.
                    ev.prevent_default();
                    ev.stop_propagation();
                    on_no.run(());
                }
            }>
            <span class="confirm-question">{question}</span>
            <button type="button" class=class node_ref=button on:click=move |_| on_yes.run(())>{yes}</button>
            <button type="button" class="ghost" on:click=move |_| on_no.run(())>"Cancel"</button>
        </span>
    }
}

/// A form sent as JSON. Fields say how they read with `data-kind`: `int` is a number
/// or null, `opt` is text or null, `csv` splits on commas, `json` is parsed as is, and checkboxes with
/// `ints` gather the checked numbers into a list. Other checkboxes are true or false.
#[component]
pub fn JsonForm(
    #[prop(into)] method: String,
    #[prop(into)] url: String,
    #[prop(optional, into)] class: Option<String>,
    /// Save on every change and stay on the page.
    #[prop(optional)]
    autosave: Option<bool>,
    /// What the toast says after a successful change.
    #[prop(optional, into)]
    done: Option<String>,
    children: Children,
) -> impl IntoView {
    let form = NodeRef::<leptos::html::Form>::new();
    let problem = RwSignal::new(String::new());
    let autosave = autosave.unwrap_or(false);
    let class = class.unwrap_or_default();
    let said = done.unwrap_or_else(|| "Saved.".into());
    let request = StoredValue::new((method, url, said));
    let follow = client::use_follow();
    let save = client::action(move |body: &Value| {
        let body = body.clone();
        let (method, url, said) = request.get_value();
        let follow = follow.clone();
        let toasts = client::toast_list();
        let queries = crate::cache::client();
        let server = crate::cache::server_now();
        async move {
            match api::send::<Value, Value>(&method, &url, Some(&body)).await {
                Ok(_reply) if autosave => {
                    if let Some(list) = toasts {
                        client::push_toast(list, said.clone());
                    }
                    if let Ok(next) = serde_json::from_value::<dto::Settings>(body) {
                        crate::cache::apply_settings(queries, &server, &next);
                    }
                }
                Ok(reply) => {
                    if let Some(list) = toasts {
                        client::push_toast(list, said);
                    }
                    follow(reply_done(&reply));
                }
                Err(failure) => problem.set(failure.message),
            }
        }
    });
    let send = move || {
        if let Some(element) = form.get_untracked() {
            problem.set(String::new());
            save.dispatch(read_form(&element));
        }
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
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
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
        "json" => serde_json::from_str(text).unwrap_or(Value::Null),
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

/// Toasts the UI pushed after a change.
#[component]
pub fn Flash() -> impl IntoView {
    let extra = client::toast_list().unwrap_or_else(|| RwSignal::new(Vec::new()));
    Effect::new(move |_| {
        let left = client::take_flash();
        if !left.is_empty() {
            client::toast(left);
        }
    });
    view! {
        <div class="toasts" aria-live="polite">
            <For
                each=move || extra.get()
                key=|row| row.id
                children=move |row| {
                    let leaving = row.leaving;
                    view! {
                        <p class=move || if leaving.get() { "toast out" } else { "toast" }>{row.text}</p>
                    }
                }
            />
        </div>
    }
}

fn membership_said(change: &dto::MembershipPatch) -> String {
    if change.reminders_enabled.is_some() {
        return String::new();
    }
    if let Some(on) = change.google_auto_sync {
        return if on {
            "Google Calendar will stay updated.".into()
        } else {
            "Google Calendar will not update automatically.".into()
        };
    }
    "Saved.".into()
}

/// The signed-in person's menu: reminders, Google Calendar, the feed, and sign out.
#[component]
pub fn AccountMenu(base: String, membership: dto::Membership) -> impl IntoView {
    let url = StoredValue::new(format!("/api{base}/me"));
    let server = StoredValue::new(
        base.strip_prefix("/servers/")
            .unwrap_or(base.as_str())
            .to_string(),
    );
    let reminders = RwSignal::new(membership.reminders_enabled);
    let auto_sync = RwSignal::new(membership.google.auto_sync);
    let problem = RwSignal::new(String::new());
    let save = client::action(move |change: &dto::MembershipPatch| {
        let change = change.clone();
        let url = url.get_value();
        let server = server.get_value();
        let queries = crate::cache::client();
        let toasts = client::toast_list();
        async move {
            match api::send::<dto::Done, _>("PATCH", &url, Some(&change)).await {
                Ok(_) => {
                    if let Some(list) = toasts {
                        client::push_toast(list, membership_said(&change));
                    }
                    crate::cache::apply_membership(queries, &server, &change);
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
        }
    });
    let patch = move |change: dto::MembershipPatch| {
        save.dispatch(change);
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
            <a href=connect rel="external">"Google Calendar"</a>
            <p class="account-note">{note}</p>
        }
        .into_any()
    } else if google.ready {
        view! { <a href=connect rel="external">"Google Calendar"</a> }.into_any()
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
                <a class="calendar" href=membership.calendar_url rel="external">"Calendar feed"</a>
                <form method="post" action="/logout">
                    <button type="submit">"Sign out"</button>
                </form>
            </div>
        </details>
    }
}

/// Several roles for one permission: a button naming the chosen ones that opens
/// a searchable list. The choice sits in a hidden field as a JSON list, and the
/// form hears about it when the list closes with something changed, so one save
/// covers a run of picks.
#[component]
pub fn RolePicker(
    name: &'static str,
    /// What the list is for, read out with the button.
    label: &'static str,
    roles: Vec<dto::Role>,
    chosen: Vec<String>,
    enabled: bool,
    /// What no role chosen means for this permission.
    empty: &'static str,
) -> impl IntoView {
    let start = serde_json::to_string(&chosen).unwrap_or_else(|_| "[]".into());
    let chosen = RwSignal::new(chosen);
    let at_open = StoredValue::new(Vec::<String>::new());
    let open = RwSignal::new(false);
    let needle = RwSignal::new(String::new());
    let roles = StoredValue::new(roles);
    let root = NodeRef::<leptos::html::Div>::new();
    let field = NodeRef::<leptos::html::Input>::new();
    let search = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(input) = search.get() {
            let _ = input.focus();
        }
    });

    let names = move || {
        let ids = chosen.get();
        roles.with_value(|roles| {
            roles
                .iter()
                .filter(|role| ids.contains(&role.id))
                .map(|role| role.name.clone())
                .collect::<Vec<_>>()
        })
    };
    let face = move || {
        let names = names();
        if names.is_empty() {
            view! { <span class="muted">{empty}</span> }.into_any()
        } else {
            view! { <span class="combo-face">{names.join(", ")}</span> }.into_any()
        }
    };
    let begin = move || {
        at_open.set_value(chosen.get_untracked());
        open.set(true);
    };
    let close = move || {
        open.set(false);
        needle.set(String::new());
        let now = chosen.get_untracked();
        if now != at_open.get_value() {
            announce(field, &serde_json::to_string(&now).unwrap_or_else(|_| "[]".into()));
        }
    };
    let toggle = move |id: String| {
        chosen.update(|ids| match ids.iter().position(|other| *other == id) {
            Some(at) => {
                ids.remove(at);
            }
            None => ids.push(id),
        });
    };
    let shown = move || {
        let needle = needle.get().trim().to_lowercase();
        roles.with_value(|roles| {
            roles
                .iter()
                .filter(|role| needle.is_empty() || role.name.to_lowercase().contains(&needle))
                .map(|role| (role.id.clone(), role.name.clone()))
                .collect::<Vec<_>>()
        })
    };
    let rows = move || {
        let picked = chosen.get();
        let list = shown();
        if list.is_empty() {
            let text = if roles.with_value(|roles| roles.is_empty()) {
                "This server has no roles Marian can see."
            } else {
                "No role matches."
            };
            return view! { <p class="pick-empty">{text}</p> }.into_any();
        }
        list.into_iter()
            .map(|(id, role)| {
                let on = picked.contains(&id);
                view! {
                    <button type="button" role="option" class=if on { "combo-option on" } else { "combo-option" }
                        aria-selected=if on { "true" } else { "false" }
                        on:click=move |_| toggle(id.clone())>
                        <span>{role}</span>
                        {on.then(|| view! {
                            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
                        })}
                    </button>
                }
            })
            .collect_view()
            .into_any()
    };

    view! {
        <div class="combo" node_ref=root
            on:focusout=move |ev| {
                if open.get_untracked() && focus_left(&ev, root) {
                    close();
                }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" && open.get_untracked() {
                    ev.prevent_default();
                    close();
                }
            }>
            <input type="hidden" name=name data-kind="json" node_ref=field value=start/>
            <button type="button" class="combo-trigger" disabled=!enabled aria-haspopup="listbox"
                aria-label=label
                title=move || names().join(", ")
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { close() } else { begin() }>
                {face}
                <span class="switcher-chevron" aria-hidden="true"></span>
            </button>
            {move || open.get().then(|| view! {
                // Clicks inside keep focus in the search box, so several picks stay in one go.
                <div class="combo-panel" on:mousedown=|ev| ev.prevent_default()>
                    <input node_ref=search type="search" placeholder="Search roles"
                        aria-label="Search roles" autocomplete="off"
                        on:mousedown=|ev| ev.stop_propagation()
                        prop:value=move || needle.get()
                        on:input=move |ev| needle.set(event_target_value(&ev))
                        on:change=|ev| ev.stop_propagation()
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                if let Some((id, _)) = shown().into_iter().next() {
                                    toggle(id);
                                }
                            }
                        }/>
                    <div class="combo-list" role="listbox" aria-multiselectable="true" aria-label=label>{rows}</div>
                </div>
            })}
        </div>
    }
}

/// A time zone chooser: a button with the current zone that opens a searchable
/// list grouped by offset. The choice sits in a hidden `timezone` field, and
/// picking one tells the surrounding form it changed, so an autosaving form saves.
#[component]
pub fn ZonePicker(groups: Vec<dto::ZoneGroup>, enabled: bool) -> impl IntoView {
    let start = groups
        .iter()
        .flat_map(|group| group.zones.iter())
        .find(|zone| zone.selected)
        .map(|zone| zone.id.clone())
        .unwrap_or_default();
    let chosen = RwSignal::new(start.clone());
    let open = RwSignal::new(false);
    let needle = RwSignal::new(String::new());
    let groups = StoredValue::new(groups);
    let root = NodeRef::<leptos::html::Div>::new();
    let field = NodeRef::<leptos::html::Input>::new();
    let search = NodeRef::<leptos::html::Input>::new();
    let list = NodeRef::<leptos::html::Div>::new();
    Effect::new(move |_| {
        if let Some(input) = search.get() {
            let _ = input.focus();
            reveal_chosen(list);
        }
    });

    let face = move || {
        let id = chosen.get();
        groups
            .with_value(|groups| {
                groups.iter().find_map(|group| {
                    group
                        .zones
                        .iter()
                        .find(|zone| zone.id == id)
                        .map(|zone| format!("{} · {}", zone.label, group.offset))
                })
            })
            .unwrap_or_else(|| if id.is_empty() { "Pick a time zone".into() } else { id })
    };
    let close = move || {
        open.set(false);
        needle.set(String::new());
    };
    let pick = move |id: String| {
        chosen.set(id.clone());
        close();
        announce(field, &id);
    };
    let first_match = move || {
        let needle = needle.get_untracked().trim().to_lowercase();
        groups.with_value(|groups| {
            groups.iter().find_map(|group| {
                group
                    .zones
                    .iter()
                    .find(|zone| zone_matches(zone, &group.offset, &needle))
                    .map(|zone| zone.id.clone())
            })
        })
    };
    let rows = move || {
        let needle = needle.get().trim().to_lowercase();
        let now = chosen.get();
        groups.with_value(|groups| {
            let shown: Vec<_> = groups
                .iter()
                .filter_map(|group| {
                    let zones: Vec<_> = group
                        .zones
                        .iter()
                        .filter(|zone| zone_matches(zone, &group.offset, &needle))
                        .map(|zone| {
                            let id = zone.id.clone();
                            let on = id == now;
                            view! {
                                <button type="button" role="option"
                                    class=if on { "combo-option on" } else { "combo-option" }
                                    aria-selected=if on { "true" } else { "false" }
                                    on:click=move |_| pick(id.clone())>
                                    <span>{zone.label.clone()}</span>
                                    {on.then(|| view! {
                                        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
                                    })}
                                </button>
                            }
                        })
                        .collect();
                    (!zones.is_empty()).then(|| view! {
                        <div class="combo-group" role="group" aria-label=group.offset.clone()>
                            <p class="combo-group-label">{group.offset.clone()}</p>
                            {zones}
                        </div>
                    })
                })
                .collect();
            if shown.is_empty() {
                view! { <p class="pick-empty">"No city matches."</p> }.into_any()
            } else {
                shown.collect_view().into_any()
            }
        })
    };

    view! {
        <div class="combo" node_ref=root
            on:focusout=move |ev| {
                if open.get_untracked() && focus_left(&ev, root) {
                    close();
                }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" && open.get_untracked() {
                    ev.prevent_default();
                    close();
                }
            }>
            <input type="hidden" name="timezone" node_ref=field value=start/>
            <button type="button" class="combo-trigger" disabled=!enabled aria-haspopup="listbox"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { close() } else { open.set(true) }>
                <span>{face}</span>
                <span class="switcher-chevron" aria-hidden="true"></span>
            </button>
            {move || open.get().then(|| view! {
                // Clicks inside keep focus in the search box, so the list stays open.
                <div class="combo-panel" on:mousedown=|ev| ev.prevent_default()>
                    <input node_ref=search type="search" placeholder="Search cities or offsets"
                        aria-label="Search time zones" autocomplete="off"
                        on:mousedown=|ev| ev.stop_propagation()
                        prop:value=move || needle.get()
                        on:input=move |ev| {
                            needle.set(event_target_value(&ev));
                            if let Some(list) = list.get_untracked() {
                                list.set_scroll_top(0);
                            }
                        }
                        // The search text is not a setting; keep its change from saving the form.
                        on:change=|ev| ev.stop_propagation()
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                if let Some(id) = first_match() {
                                    pick(id);
                                }
                            }
                        }/>
                    <div class="combo-list" role="listbox" aria-label="Time zones" node_ref=list>{rows}</div>
                </div>
            })}
        </div>
    }
}

/// A zone shows for a search when its city, its region id, or its offset holds the text.
fn zone_matches(zone: &dto::ZoneRow, offset: &str, needle: &str) -> bool {
    needle.is_empty()
        || zone.label.to_lowercase().contains(needle)
        || zone.id.to_lowercase().contains(needle)
        || offset.to_lowercase().contains(needle)
}

/// Puts the value in the hidden field and lets the form hear about it.
#[cfg(feature = "hydrate")]
fn announce(field: NodeRef<leptos::html::Input>, value: &str) {
    let Some(input) = field.get_untracked() else {
        return;
    };
    input.set_value(value);
    let init = web_sys::EventInit::new();
    init.set_bubbles(true);
    if let Ok(change) = web_sys::Event::new_with_event_init_dict("change", &init) {
        let _ = input.dispatch_event(&change);
    }
}

#[cfg(not(feature = "hydrate"))]
fn announce(_: NodeRef<leptos::html::Input>, _: &str) {}

/// Scrolls the open list so the current zone is in view.
#[cfg(feature = "hydrate")]
fn reveal_chosen(list: NodeRef<leptos::html::Div>) {
    client::later(0, move || {
        use wasm_bindgen::JsCast;
        let Some(list) = list.get_untracked() else {
            return;
        };
        let Ok(Some(on)) = list.query_selector(".combo-option.on") else {
            return;
        };
        if let Ok(on) = on.dyn_into::<web_sys::HtmlElement>() {
            list.set_scroll_top(on.offset_top() - list.offset_top() - 48);
        }
    });
}

#[cfg(not(feature = "hydrate"))]
fn reveal_chosen(_: NodeRef<leptos::html::Div>) {}

/// Whether focus went somewhere outside `root`. Removing the picker blurs its
/// input after `root` is disposed; that is not a reason to close anything.
#[cfg(feature = "hydrate")]
pub(crate) fn focus_left(ev: &leptos::ev::FocusEvent, root: NodeRef<leptos::html::Div>) -> bool {
    use wasm_bindgen::JsCast;
    let Some(root) = root.try_get_untracked().flatten() else {
        return false;
    };
    let next = ev
        .related_target()
        .and_then(|target| target.dyn_into::<web_sys::Node>().ok());
    !root.contains(next.as_ref())
}

#[cfg(not(feature = "hydrate"))]
fn focus_left<T>(_: &T, _: NodeRef<leptos::html::Div>) -> bool {
    false
}

