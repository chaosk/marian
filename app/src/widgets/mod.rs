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

/// A newer frontend than the one this page runs is being served.
#[derive(Clone, Copy)]
// Only the browser listens for builds; the server just provides the flag.
#[cfg_attr(not(feature = "hydrate"), allow(dead_code))]
pub struct NewBuild(pub RwSignal<bool>);

/// Listens for changes on this server and refetches what each one names. The
/// server also names its frontend build when the stream opens, which after a deploy is
/// the reconnect to the new process; a different build raises `NewBuild`.
#[component]
pub fn Live(server: String) -> impl IntoView {
    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        let stream = std::rc::Rc::new(LiveStream {
            server: server.clone(),
            client: crate::cache::client(),
            new_build: use_context::<NewBuild>().map(|build| build.0),
            source: Default::default(),
            opened: Default::default(),
            failures: Default::default(),
            stopped: Default::default(),
        });
        stream.open();
        let stream = send_wrapper::SendWrapper::new(stream);
        on_cleanup(move || stream.take().stop());
    });
    #[cfg(not(feature = "hydrate"))]
    let _ = server;
}

/// One server's live stream. The browser retries a dropped stream by itself
/// but gives up for good when a retry gets an error status, which is what the
/// ingress answers while a deploy has no pod up. Then this opens it again,
/// waiting longer after each failure, up to half a minute.
#[cfg(feature = "hydrate")]
struct LiveStream {
    server: String,
    client: leptos_fetch::QueryClient,
    new_build: Option<RwSignal<bool>>,
    source: std::cell::RefCell<Option<web_sys::EventSource>>,
    opened: std::cell::Cell<bool>,
    failures: std::cell::Cell<u32>,
    stopped: std::cell::Cell<bool>,
}

#[cfg(feature = "hydrate")]
impl LiveStream {
    fn open(self: &std::rc::Rc<Self>) {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;
        if self.stopped.get() {
            return;
        }
        let Ok(source) = web_sys::EventSource::new(&api::path::live(&self.server)) else {
            return;
        };
        let (here, client) = (self.server.clone(), self.client);
        // A message this page can't read, from a newer or older server, refetches everything.
        let on_change = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let change = event
                .data()
                .as_string()
                .and_then(|text| serde_json::from_str::<dto::Change>(&text).ok())
                .unwrap_or(dto::Change::Server);
            crate::cache::apply_change(client, &here, &change);
        });
        source.set_onmessage(Some(on_change.as_ref().unchecked_ref()));
        on_change.forget();
        let new_build = self.new_build;
        let on_build = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |event: web_sys::MessageEvent| {
            let served = event.data().as_string().unwrap_or_default();
            let mine = api::site().build;
            if !served.is_empty() && !mine.is_empty() && served != mine {
                if let Some(flag) = new_build {
                    flag.try_set(true);
                }
            }
        });
        let _ = source.add_event_listener_with_callback("build", on_build.as_ref().unchecked_ref());
        on_build.forget();
        // Changes made while the stream was down were never announced.
        let this = std::rc::Rc::downgrade(self);
        let on_open = Closure::<dyn FnMut()>::new(move || {
            if let Some(this) = this.upgrade() {
                this.failures.set(0);
                if this.opened.replace(true) {
                    crate::cache::refresh_in(this.client, &this.server);
                }
            }
        });
        source.set_onopen(Some(on_open.as_ref().unchecked_ref()));
        on_open.forget();
        let this = std::rc::Rc::downgrade(self);
        let on_error = Closure::<dyn FnMut()>::new(move || {
            let Some(this) = this.upgrade() else { return };
            let closed = this
                .source
                .borrow()
                .as_ref()
                .is_some_and(|source| source.ready_state() == web_sys::EventSource::CLOSED);
            if closed && !this.stopped.get() {
                this.source.borrow_mut().take();
                let failures = this.failures.get();
                this.failures.set(failures + 1);
                let wait = (1000 << failures.min(5)).min(30_000);
                let again = std::rc::Rc::downgrade(&this);
                let reopen = Closure::once_into_js(move || {
                    if let Some(again) = again.upgrade() {
                        again.open();
                    }
                });
                let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(reopen.unchecked_ref(), wait);
            }
        });
        source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        on_error.forget();
        *self.source.borrow_mut() = Some(source);
    }

    fn stop(&self) {
        self.stopped.set(true);
        if let Some(source) = self.source.borrow_mut().take() {
            source.close();
        }
    }
}

/// A search box that keeps its query in the address bar, beside any filters
/// already there. A new search starts from the first page.
#[component]
pub fn Finder(q: String) -> impl IntoView {
    let needle = RwSignal::new(q);
    let query = leptos_router::hooks::use_query_map();
    view! {
        <form class="finder" method="get" on:submit={
            let go = client::use_go();
            move |ev| {
                ev.prevent_default();
                let q = needle.get();
                let mut parts: Vec<String> = query.with_untracked(|map| {
                    map.clone()
                        .into_iter()
                        .filter(|(key, _)| !matches!(key.as_ref(), "q" | "page"))
                        .map(|(key, value)| format!("{key}={}", api::encode(&value)))
                        .collect()
                });
                if !q.trim().is_empty() {
                    parts.push(format!("q={}", api::encode(q.trim())));
                }
                let path = client::path();
                let to = if parts.is_empty() { path } else { format!("{path}?{}", parts.join("&")) };
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
    let url = StoredValue::new(api::path::commands(&server));
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
    let url = StoredValue::new(api::path::membership(&crate::cache::server_now()));
    let server = StoredValue::new(crate::cache::server_now());
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
        // One row like Reminders: the switch keeps the calendar updated. The
        // account and a way out sit underneath on one small line.
        let account = if google.email.is_empty() { "Connected".to_string() } else { google.email.clone() };
        view! {
            <button type="button" class="reminder-toggle" title="Keep your Google Calendar updated"
                aria-pressed=move || if auto_sync.get() { "true" } else { "false" }
                on:click=move |_| patch(dto::MembershipPatch {
                    google_auto_sync: Some(!auto_sync.get_untracked()),
                    ..Default::default()
                })>
                <span>"Google Calendar"</span>
                <span class=move || if auto_sync.get() { "toggle on" } else { "toggle" } aria-hidden="true"><span></span></span>
            </button>
            <form class="account-google" method="post" action="/auth/google/disconnect">
                <span class="account-email" title=account.clone()>{account.clone()}</span>
                <input type="hidden" name="next" value=base.clone()/>
                <button type="submit" class="account-link">"Disconnect"</button>
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

    close_on_outside_press(root, move || open.get_untracked(), close);
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

/// One choice from a searchable list: a button naming the current pick that
/// opens a list with a search box, as the time zone and role pickers do. `none`
/// adds a first row that clears the choice, like "Anyone".
#[component]
pub fn Choice(
    /// The choices, as value and label.
    #[prop(into)]
    options: Signal<Vec<(String, String)>>,
    #[prop(into)] chosen: Signal<Option<String>>,
    /// What the button says while nothing is chosen.
    #[prop(into)]
    placeholder: String,
    #[prop(optional, into)] none: Option<String>,
    /// What the list is for, read out with the button.
    #[prop(into)]
    label: String,
    on_pick: Callback<Option<String>>,
) -> impl IntoView {
    let open = RwSignal::new(false);
    let needle = RwSignal::new(String::new());
    let root = NodeRef::<leptos::html::Div>::new();
    let search = NodeRef::<leptos::html::Input>::new();
    let list = NodeRef::<leptos::html::Div>::new();
    let placeholder = StoredValue::new(placeholder);
    let none = StoredValue::new(none);
    let label = StoredValue::new(label);
    Effect::new(move |_| {
        if let Some(input) = search.get() {
            let _ = input.focus();
            reveal_chosen(list);
        }
    });
    let face = move || {
        let now = chosen.get();
        now.as_ref()
            .and_then(|value| options.with(|list| list.iter().find(|(id, _)| id == value).map(|(_, name)| name.clone())))
            .or_else(|| none.get_value().filter(|_| now.is_none()))
            .unwrap_or_else(|| placeholder.get_value())
    };
    let close = move || {
        open.set(false);
        needle.set(String::new());
    };
    let pick = move |value: Option<String>| {
        close();
        on_pick.run(value);
    };
    let shown = move || {
        let needle = needle.get().trim().to_lowercase();
        options.with(|list| {
            list.iter()
                .filter(|(_, name)| needle.is_empty() || name.to_lowercase().contains(&needle))
                .cloned()
                .collect::<Vec<_>>()
        })
    };
    let rows = move || {
        let now = chosen.get();
        let clear = none.get_value().filter(|_| needle.with(|text| text.trim().is_empty())).map(|name| {
            let on = now.is_none();
            view! {
                <button type="button" role="option" class=if on { "combo-option on" } else { "combo-option" }
                    aria-selected=if on { "true" } else { "false" }
                    on:click=move |_| pick(None)>
                    <span>{name}</span>
                </button>
            }
        });
        let items = shown();
        let empty = items.is_empty() && clear.is_none();
        let items = items
            .into_iter()
            .map(|(value, name)| {
                let on = now.as_ref() == Some(&value);
                view! {
                    <button type="button" role="option" class=if on { "combo-option on" } else { "combo-option" }
                        aria-selected=if on { "true" } else { "false" }
                        on:click=move |_| pick(Some(value.clone()))>
                        <span>{name}</span>
                        {on.then(|| view! {
                            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
                        })}
                    </button>
                }
            })
            .collect_view();
        view! {
            {clear}
            {items}
            {empty.then(|| view! { <p class="pick-empty">"Nothing matches."</p> })}
        }
    };
    close_on_outside_press(root, move || open.get_untracked(), close);
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
                    ev.stop_propagation();
                    close();
                }
            }>
            <button type="button" class="combo-trigger" aria-haspopup="listbox"
                aria-label=move || label.get_value()
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| if open.get_untracked() { close() } else { open.set(true) }>
                <span class=move || if chosen.with(Option::is_none) && none.with_value(Option::is_none) { "muted" } else { "" }>{face}</span>
                <span class="switcher-chevron" aria-hidden="true"></span>
            </button>
            {move || open.get().then(|| view! {
                <div class="combo-panel" on:mousedown=|ev| ev.prevent_default()>
                    <input node_ref=search type="search" placeholder="Search"
                        aria-label=move || label.get_value() autocomplete="off"
                        on:mousedown=|ev| ev.stop_propagation()
                        prop:value=move || needle.get()
                        on:input=move |ev| needle.set(event_target_value(&ev))
                        on:change=|ev| ev.stop_propagation()
                        on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                            if ev.key() == "Enter" {
                                ev.prevent_default();
                                if let Some((value, _)) = shown().into_iter().next() {
                                    pick(Some(value));
                                }
                            }
                        }/>
                    <div class="combo-list" role="listbox" node_ref=list aria-label=move || label.get_value()>{rows}</div>
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

    close_on_outside_press(root, move || open.get_untracked(), close);
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
pub(crate) fn focus_left<T>(_: &T, _: NodeRef<leptos::html::Div>) -> bool {
    false
}

/// Closes a menu when a press lands outside `root`. Focus leaving covers the
/// keyboard; this covers a click on the page, which in Safari and Firefox on a
/// Mac never moved focus into the menu to begin with.
#[cfg(feature = "hydrate")]
pub(crate) fn close_on_outside_press(
    root: NodeRef<leptos::html::Div>,
    open: impl Fn() -> bool + 'static,
    close: impl Fn() + 'static,
) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    let on_press = Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
        if !open() {
            return;
        }
        let Some(root) = root.try_get_untracked().flatten() else {
            return;
        };
        let target = ev.target().and_then(|target| target.dyn_into::<web_sys::Node>().ok());
        if !root.contains(target.as_ref()) {
            close();
        }
    })
    .into_js_value();
    let _ = document().add_event_listener_with_callback_and_bool("pointerdown", on_press.unchecked_ref(), true);
    let handle = send_wrapper::SendWrapper::new(on_press);
    on_cleanup(move || {
        let on_press = handle.take();
        let _ = document().remove_event_listener_with_callback_and_bool("pointerdown", on_press.unchecked_ref(), true);
    });
}

#[cfg(not(feature = "hydrate"))]
pub(crate) fn close_on_outside_press(
    _: NodeRef<leptos::html::Div>,
    _: impl Fn() -> bool + 'static,
    _: impl Fn() + 'static,
) {
}

/// Watches every press on the page, once for the app.
///
/// - A menu built on `<details>` closes when a press lands outside it.
/// - An open editor, anything marked `data-editor` with its own Save and
///   Cancel, holds every press outside it until one of those is chosen. The
///   press does nothing, and the editor shakes so the eye finds it. Its own
///   anchor, where the button that opened it sits, still works, as does a
///   dialog on top of the page. An editor nobody has changed yet just closes,
///   and the press goes on to whatever it landed on.
/// - A popover that would run off the bottom of the window opens upward
///   instead, when there is room above it.
#[cfg(feature = "hydrate")]
pub(crate) fn watch_presses() {
    use std::cell::Cell;
    use std::rc::Rc;
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    // Whether the press under way was held, so its click is held too.
    let held = Rc::new(Cell::new(false));
    let target_of = |ev: &web_sys::Event| ev.target().and_then(|target| target.dyn_into::<web_sys::Element>().ok());
    let outside_editor = |target: &web_sys::Element| -> Option<web_sys::Element> {
        let editor = document().query_selector("[data-editor]").ok().flatten()?;
        let inside = |area: &web_sys::Element| area.contains(Some(target));
        let anchor = editor.closest(".inline-anchor").ok().flatten();
        let dialog = target.closest(".modal-backdrop").ok().flatten();
        (!inside(&editor) && !anchor.as_ref().is_some_and(inside) && dialog.is_none()).then_some(editor)
    };
    let hold = |ev: &web_sys::Event, editor: &web_sys::Element| {
        ev.prevent_default();
        ev.stop_immediate_propagation();
        nudge(editor);
    };

    let on_press = {
        let held = held.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
            held.set(false);
            let Some(target) = target_of(&ev) else { return };
            if let Ok(open) = document().query_selector_all("details.switcher[open]") {
                for menu in (0..open.length()).filter_map(|n| open.item(n)?.dyn_into::<web_sys::Element>().ok()) {
                    if !menu.contains(Some(&target)) {
                        let _ = menu.remove_attribute("open");
                    }
                }
            }
            if let Some(editor) = outside_editor(&target) {
                if unchanged(&editor) {
                    let_go(&editor);
                } else {
                    held.set(true);
                    hold(&ev, &editor);
                }
            }
        })
        .into_js_value()
    };
    // Holding the press keeps focus where it is; holding the click keeps
    // whatever was pressed from acting on it.
    let on_follow = {
        let held = held.clone();
        Closure::<dyn FnMut(web_sys::Event)>::new(move |ev: web_sys::Event| {
            if held.get() {
                ev.prevent_default();
                ev.stop_immediate_propagation();
            }
            if ev.type_() == "click" {
                held.set(false);
            }
        })
        .into_js_value()
    };
    // Enter or Space on a button elsewhere would open a second editor.
    let on_key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(move |ev: web_sys::KeyboardEvent| {
        if !matches!(ev.key().as_str(), "Enter" | " ") {
            return;
        }
        let Some(target) = target_of(&ev) else { return };
        if target.closest("button, a, summary, [role=button]").ok().flatten().is_none() {
            return;
        }
        if let Some(editor) = outside_editor(&target) {
            if unchanged(&editor) {
                let_go(&editor);
            } else {
                hold(&ev, &editor);
            }
        }
    })
    .into_js_value();
    // Popovers and editors as they appear: where a popover fits, and what an
    // editor held when it opened.
    let on_added = Closure::<dyn FnMut(js_sys::Array, web_sys::MutationObserver)>::new(
        move |records: js_sys::Array, _: web_sys::MutationObserver| {
            for record in records.iter().filter_map(|record| record.dyn_into::<web_sys::MutationRecord>().ok()) {
                let added = record.added_nodes();
                for node in (0..added.length()).filter_map(|n| added.item(n)?.dyn_into::<web_sys::Element>().ok()) {
                    for popover in matching(&node, ".popover") {
                        place(&popover);
                    }
                    for editor in matching(&node, "[data-editor]") {
                        let _ = editor.set_attribute("data-start", &editor_state(&editor));
                    }
                }
            }
        },
    )
    .into_js_value();
    if let (Ok(observer), Some(body)) = (
        web_sys::MutationObserver::new(on_added.unchecked_ref()),
        document().body(),
    ) {
        let options = web_sys::MutationObserverInit::new();
        options.set_child_list(true);
        options.set_subtree(true);
        let _ = observer.observe_with_options(&body, &options);
    }
    let window = window();
    let _ = window.add_event_listener_with_callback_and_bool("pointerdown", on_press.unchecked_ref(), true);
    for kind in ["mousedown", "click"] {
        let _ = window.add_event_listener_with_callback_and_bool(kind, on_follow.unchecked_ref(), true);
    }
    let _ = window.add_event_listener_with_callback_and_bool("keydown", on_key.unchecked_ref(), true);
}

#[cfg(not(feature = "hydrate"))]
pub(crate) fn watch_presses() {}

/// `node` itself and everything inside it that matches `selector`.
#[cfg(feature = "hydrate")]
fn matching(node: &web_sys::Element, selector: &str) -> Vec<web_sys::Element> {
    use wasm_bindgen::JsCast;
    let mut found: Vec<web_sys::Element> = node.matches(selector).unwrap_or(false).then(|| node.clone()).into_iter().collect();
    if let Ok(inside) = node.query_selector_all(selector) {
        found.extend((0..inside.length()).filter_map(|n| inside.item(n)?.dyn_into::<web_sys::Element>().ok()));
    }
    found
}

/// Opens a popover upward when it would run past the bottom of the window
/// and there is room above the thing it opened from.
#[cfg(feature = "hydrate")]
fn place(popover: &web_sys::Element) {
    let room_below = window().inner_height().ok().and_then(|height| height.as_f64()).unwrap_or(f64::MAX);
    let own = popover.get_bounding_client_rect();
    if own.bottom() <= room_below - 8.0 {
        return;
    }
    let Some(anchor) = popover.parent_element() else { return };
    if anchor.get_bounding_client_rect().top() - 6.0 >= own.height() + 8.0 {
        let _ = popover.set_attribute("data-above", "");
    }
}

/// What an editor's fields hold, to tell whether anything changed. A search
/// box only finds things, so what is typed there doesn't count.
#[cfg(feature = "hydrate")]
fn editor_state(editor: &web_sys::Element) -> String {
    use wasm_bindgen::JsCast;
    let Ok(fields) = editor.query_selector_all("input, select, textarea, [role=switch]") else {
        return String::new();
    };
    (0..fields.length())
        .filter_map(|n| fields.item(n))
        .filter_map(|node| {
            if let Some(input) = node.dyn_ref::<web_sys::HtmlInputElement>() {
                let kind = input.type_();
                return (kind != "search").then(|| format!("{kind}:{}:{}", input.value(), input.checked()));
            }
            if let Some(select) = node.dyn_ref::<web_sys::HtmlSelectElement>() {
                return Some(format!("select:{}", select.value()));
            }
            if let Some(text) = node.dyn_ref::<web_sys::HtmlTextAreaElement>() {
                return Some(format!("text:{}", text.value()));
            }
            let element = node.dyn_ref::<web_sys::Element>()?;
            Some(format!("switch:{}", element.get_attribute("aria-checked").unwrap_or_default()))
        })
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

/// Whether an editor still holds what it opened with.
#[cfg(feature = "hydrate")]
fn unchanged(editor: &web_sys::Element) -> bool {
    editor.get_attribute("data-start").is_some_and(|start| start == editor_state(editor))
}

/// Closes an editor the way Escape does, which every editor answers by cancelling.
#[cfg(feature = "hydrate")]
fn let_go(editor: &web_sys::Element) {
    let init = web_sys::KeyboardEventInit::new();
    init.set_key("Escape");
    init.set_bubbles(true);
    if let Ok(event) = web_sys::KeyboardEvent::new_with_keyboard_event_init_dict("keydown", &init) {
        let _ = editor.dispatch_event(&event);
    }
}

/// Shakes an editor that is waiting for Save or Cancel, and brings it into view.
#[cfg(feature = "hydrate")]
fn nudge(editor: &web_sys::Element) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_block(web_sys::ScrollLogicalPosition::Nearest);
    options.set_behavior(web_sys::ScrollBehavior::Smooth);
    editor.scroll_into_view_with_scroll_into_view_options(&options);
    let _ = editor.remove_attribute("data-asking");
    // Read layout so the animation starts again when it was already running.
    let _ = editor.client_width();
    let _ = editor.set_attribute("data-asking", "");
    let editor = editor.clone();
    let done = Closure::once_into_js(move || {
        let _ = editor.remove_attribute("data-asking");
    });
    let _ = window().set_timeout_with_callback_and_timeout_and_arguments_0(done.unchecked_ref(), 600);
}

