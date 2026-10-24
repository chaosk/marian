//! A server's presets and the library of attendance types they draw from.
//! A preset is an ordered list of library types, each with a capacity and a
//! role limit; the default one is what a new event starts with. Renaming a
//! type, or changing whether it counts as attending, changes it everywhere.

use leptos::prelude::*;
use marian_types as dto;

use crate::cache::{self, when, Ready};
use crate::widgets::{ActionButton, Choice, InlineConfirm};
use crate::api::path;
use crate::{api, base, client, head, A};

#[component]
pub fn Presets() -> impl IntoView {
    let presets = cache::listen(cache::presets_scope(), cache::server_key);
    let library = cache::listen(cache::library_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    // Which preset is open for editing: its id, or "new".
    let editing = RwSignal::new(None::<String>);
    when(move || (presets.get(), library.get(), roles.get()).ready(), move |(presets, library, roles)| {
        let library = StoredValue::new(library);
        let roles = StoredValue::new(roles);
        let cards = presets
            .into_iter()
            .map(|preset| {
                let id = preset.id.clone();
                let shown = preset.clone();
                view! {
                    {move || if editing.get().as_deref() == Some(id.as_str()) {
                        view! { <PresetForm preset=Some(shown.clone()) library roles editing/> }.into_any()
                    } else {
                        preset_card(shown.clone(), editing).into_any()
                    }}
                }
            })
            .collect_view();
        view! {
            {head("Presets")}
            <section class="stack preset-list">
                <div class="section-head">
                    <h2>"Presets"</h2>
                    <button type="button" class="ghost"
                        class:keep-space=move || editing.get().as_deref() == Some("new")
                        on:click=move |_| editing.set(Some("new".into()))>"New preset"</button>
                </div>
                <p class="settings-note">"A new event starts with the default preset. Any event can switch to another from its Attendance section."</p>
                {move || (editing.get().as_deref() == Some("new")).then(|| view! {
                    <PresetForm preset=None library roles editing/>
                })}
                {cards}
            </section>
            <LibraryPanel library=library.get_value()/>
        }
    })
}

/// A preset as a card: its types in order, and what can be done with it.
fn preset_card(preset: dto::Preset, editing: RwSignal<Option<String>>) -> impl IntoView {
    let base = base();
    let id = preset.id.clone();
    let chips = preset
        .options
        .iter()
        .map(|opt| {
            let mut extra = Vec::new();
            if let Some(cap) = opt.capacity {
                extra.push(format!("{cap} spots"));
            }
            if opt.role_id.is_some() {
                extra.push("role only".to_string());
            }
            let extra = (!extra.is_empty()).then(|| format!(" · {}", extra.join(", ")));
            view! { <span class="type-chip">{opt.emoji.clone()}" "{opt.name.clone()}{extra}</span> }
        })
        .collect_view();
    view! {
        <article class="card preset-card">
            <div class="card-top">
                <h3>{preset.name.clone()}</h3>
                {preset.default.then(|| view! { <span class="badge state-voting">"Default"</span> })}
            </div>
            <div class="type-chips">{chips}</div>
            {preset.multiple_signups.then(|| view! { <p class="meta">"People can pick more than one."</p> })}
            <div class="actions">
                <button type="button" class="ghost" on:click={
                    let id = id.clone();
                    move |_| editing.set(Some(id.clone()))
                }>"Edit"</button>
                <A href=format!("{base}/events/new?preset={id}") attr:class="ghost">"Use on a new event"</A>
                {(!preset.default).then(|| view! {
                    <ActionButton method="POST" url=path::preset_default(&cache::server_now(), &id) label="Make default" class="ghost" done=""/>
                    <ActionButton method="DELETE" url=path::preset(&cache::server_now(), &id) label="Delete" class="danger"
                        confirm=format!("Delete the {} preset? Events keep their types.", preset.name)/>
                })}
            </div>
        </article>
    }
}

/// Makes a preset or changes one: its name, its types in order with their
/// capacity and role limit, and whether people may pick more than one. Rows
/// move by dragging their handle, or with the arrow keys on it.
#[component]
fn PresetForm(
    preset: Option<dto::Preset>,
    library: StoredValue<Vec<dto::LibraryItem>>,
    roles: StoredValue<Vec<dto::Role>>,
    editing: RwSignal<Option<String>>,
) -> impl IntoView {
    let id = preset.as_ref().map(|preset| preset.id.clone());
    let name = RwSignal::new(preset.as_ref().map(|preset| preset.name.clone()).unwrap_or_default());
    let multiple = RwSignal::new(preset.as_ref().is_some_and(|preset| preset.multiple_signups));
    let types = RwSignal::new(preset.map(|preset| preset.options).unwrap_or_default());
    let problem = RwSignal::new(String::new());
    let rows = Rows {
        types,
        grab: RwSignal::new(None),
        dragging: RwSignal::new(None),
        roles,
    };
    let saving = client::action(move |draft: &dto::PresetDraft| {
        let draft = draft.clone();
        let server = cache::server_now();
        let (method, url) = match &id {
            Some(id) => ("PUT", path::preset(&server, id)),
            None => ("POST", path::presets(&server)),
        };
        let queries = cache::client();
        async move {
            match api::send::<dto::Done, _>(method, &url, Some(&draft)).await {
                Ok(_) => {
                    editing.try_set(None);
                    cache::refresh_in(queries, &server);
                }
                Err(failure) => {
                    problem.try_set(failure.message);
                }
            }
        }
    });
    let save = move || {
        problem.set(String::new());
        saving.dispatch(dto::PresetDraft {
            name: name.get_untracked(),
            multiple_signups: multiple.get_untracked(),
            options: types.get_untracked(),
        });
    };
    let addable = Signal::derive(move || {
        let chosen: Vec<String> = types.with(|list| list.iter().map(|opt| opt.type_id.clone()).collect());
        library.with_value(|items| {
            items
                .iter()
                .filter(|item| !chosen.contains(&item.id))
                .map(|item| (item.id.clone(), format!("{} {}", item.emoji, item.name)))
                .collect::<Vec<_>>()
        })
    });
    let add = Callback::new(move |picked: Option<String>| {
        let item = picked.and_then(|picked| library.with_value(|items| items.iter().find(|item| item.id == picked).cloned()));
        if let Some(item) = item {
            types.update(|list| list.push(dto::SignupOption {
                id: None,
                type_id: item.id,
                name: item.name,
                emoji: item.emoji,
                attending: item.attending,
                capacity: None,
                role_id: None,
            }));
        }
    });
    view! {
        <form class="panel preset-form" data-editor="" on:submit=move |ev| { ev.prevent_default(); save(); }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| if ev.key() == "Escape" { editing.set(None); }>
            <label class="field">"Name"
                <input required maxlength="60" prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            </label>
            <div class="preset-types" role="table" aria-label="Types in this preset">
                <div class="preset-type preset-types-head" role="row">
                    <span></span>
                    <span role="columnheader">"Type"</span>
                    <span role="columnheader" title="How many people can pick it. Empty means no limit.">"Capacity"</span>
                    <span role="columnheader">"Who can pick"</span>
                    <span></span>
                </div>
                <For
                    each=move || types.with(|list| list.iter().map(|opt| opt.type_id.clone()).collect::<Vec<_>>())
                    key=|type_id| type_id.clone()
                    children=move |type_id| view! { <PresetTypeRow rows type_id/> }
                />
            </div>
            <div class="preset-add">
                <Choice options=addable chosen=Signal::derive(|| None::<String>) placeholder="Add a type from the library"
                    label="Add a type" on_pick=add/>
            </div>
            <label class="checks">
                <input type="checkbox" prop:checked=move || multiple.get() on:change=move |ev| multiple.set(event_target_checked(&ev))/>
                " "<span>"People may pick more than one"</span>
            </label>
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            <div class="actions">
                <button type="submit" class="primary" disabled=move || saving.pending().get()>"Save preset"</button>
                <button type="button" class="ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
            </div>
        </form>
    }
}

/// The preset editor's rows, shared with each row.
#[derive(Clone, Copy)]
struct Rows {
    types: RwSignal<Vec<dto::SignupOption>>,
    /// The row whose handle is held, so only it can be dragged.
    grab: RwSignal<Option<String>>,
    dragging: RwSignal<Option<String>>,
    roles: StoredValue<Vec<dto::Role>>,
}

impl Rows {
    fn at(self, type_id: &str) -> Option<usize> {
        self.types.with_untracked(|list| list.iter().position(|opt| opt.type_id == type_id))
    }

    /// Moves row `from` to where `to` is.
    fn move_to(self, from: &str, to: usize) {
        if let Some(at) = self.at(from) {
            if at != to {
                self.types.update(|list| {
                    let row = list.remove(at);
                    list.insert(to.min(list.len()), row);
                });
            }
        }
    }

    fn change(self, type_id: &str, apply: impl FnOnce(&mut dto::SignupOption)) {
        self.types.update(|list| {
            if let Some(row) = list.iter_mut().find(|opt| opt.type_id == type_id) {
                apply(row);
            }
        });
    }
}

#[component]
fn PresetTypeRow(rows: Rows, type_id: String) -> impl IntoView {
    let key = StoredValue::new(type_id);
    let row = Memo::new(move |_| {
        key.with_value(|key| rows.types.with(|list| list.iter().find(|opt| &opt.type_id == key).cloned()))
    });
    let label = move || row.with(|row| row.as_ref().map(|opt| format!("{} {}", opt.emoji, opt.name)).unwrap_or_default());
    let held = move || rows.grab.get() == Some(key.get_value());
    let role_choices = Signal::derive(move || {
        rows.roles.with_value(|roles| roles.iter().map(|role| (role.id.clone(), role.name.clone())).collect::<Vec<_>>())
    });
    let role = Signal::derive(move || row.with(|row| row.as_ref().and_then(|opt| opt.role_id.clone())));
    let pick_role = Callback::new(move |picked: Option<String>| key.with_value(|key| rows.change(key, |opt| opt.role_id = picked)));
    let nudge = move |ev: leptos::ev::KeyboardEvent| {
        let step: isize = match ev.key().as_str() {
            "ArrowUp" => -1,
            "ArrowDown" => 1,
            _ => return,
        };
        ev.prevent_default();
        let key = key.get_value();
        let Some(at) = rows.at(&key) else { return };
        let len = rows.types.with_untracked(Vec::len);
        if let Some(to) = at.checked_add_signed(step).filter(|to| *to < len) {
            rows.move_to(&key, to);
        }
    };
    view! {
        <div role="row"
            class=move || if rows.dragging.get() == Some(key.get_value()) { "preset-type dragging" } else { "preset-type" }
            draggable=move || if held() { "true" } else { "false" }
            on:dragstart=move |ev: leptos::ev::DragEvent| {
                if !held() {
                    ev.prevent_default();
                    return;
                }
                #[cfg(feature = "hydrate")]
                if let Some(data) = ev.data_transfer() {
                    let _ = data.set_data("text/plain", &key.get_value());
                    data.set_effect_allowed("move");
                }
                rows.dragging.set(Some(key.get_value()));
            }
            on:dragover=move |ev: leptos::ev::DragEvent| {
                let Some(from) = rows.dragging.get_untracked() else { return };
                ev.prevent_default();
                if let Some(to) = rows.at(&key.get_value()) {
                    rows.move_to(&from, to);
                }
            }
            on:drop=|ev| ev.prevent_default()
            on:dragend=move |_| {
                rows.grab.set(None);
                rows.dragging.set(None);
            }>
            <button type="button" class="drag-handle" title="Drag to reorder" aria-label=move || format!("Move {}", label())
                on:pointerdown=move |_| rows.grab.set(Some(key.get_value()))
                on:pointerup=move |_| rows.grab.set(None)
                on:keydown=nudge>
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M11 18c0 1.1-.9 2-2 2s-2-.9-2-2 .9-2 2-2 2 .9 2 2zm-2-8c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0-6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm6 4c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"/></svg>
            </button>
            <span class="preset-type-name" role="cell">{label}</span>
            <input role="cell" type="number" min="1" max="500" inputmode="numeric" placeholder="No limit"
                aria-label=move || format!("Capacity of {}", label())
                prop:value=move || row.with(|row| row.as_ref().and_then(|opt| opt.capacity).map(|n| n.to_string()).unwrap_or_default())
                on:change=move |ev| {
                    let value = event_target_value(&ev).trim().parse::<i64>().ok();
                    key.with_value(|key| rows.change(key, |opt| opt.capacity = value));
                }/>
            <span role="cell">
                <Choice options=role_choices chosen=role placeholder="Anyone" none="Anyone"
                    label="Who can pick" on_pick=pick_role/>
            </span>
            <button type="button" class="icon-btn" title="Take it out" aria-label=move || format!("Take {} out", label())
                on:click=move |_| rows.types.update(|list| list.retain(|opt| key.with_value(|key| &opt.type_id != key)))>
                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M19 6.41 17.59 5 12 10.59 6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 12 13.41 17.59 19 19 17.59 13.41 12z"/></svg>
            </button>
        </div>
    }
}

/// The server's attendance types: rename them, change their emoji or whether
/// they count as attending, add new ones, and remove the ones nothing uses.
#[component]
fn LibraryPanel(library: Vec<dto::LibraryItem>) -> impl IntoView {
    let editing = RwSignal::new(None::<String>);
    let rows = library
        .into_iter()
        .map(|item| {
            let id = item.id.clone();
            let shown = item.clone();
            view! {
                {move || if editing.get().as_deref() == Some(id.as_str()) {
                    view! { <TypeForm item=Some(shown.clone()) editing/> }.into_any()
                } else {
                    type_row(shown.clone(), editing).into_any()
                }}
            }
        })
        .collect_view();
    view! {
        <section class="stack library-list">
            <div class="section-head">
                <h2>"Attendance types"</h2>
                <button type="button" class="ghost"
                    class:keep-space=move || editing.get().as_deref() == Some("new")
                    on:click=move |_| editing.set(Some("new".into()))>"New type"</button>
            </div>
            <p class="settings-note">"Every event and preset uses these. A change here shows on all of them, and on the Discord posts of events still to come. People on an attending type get reminders and the attendee role."</p>
            <div class="panel library-rows">
                {move || (editing.get().as_deref() == Some("new")).then(|| view! { <TypeForm item=None editing/> })}
                {rows}
            </div>
        </section>
    }
}

fn type_row(item: dto::LibraryItem, editing: RwSignal<Option<String>>) -> impl IntoView {
    let id = item.id.clone();
    let unused = item.used_by.is_empty();
    // The badge's cell stays when empty, so the columns line up.
    view! {
        <div class="library-row">
            <span class="library-name">{item.emoji.clone()}" "<strong>{item.name.clone()}</strong></span>
            <span>{item.attending.then(|| view! { <span class="badge">"Attending"</span> })}</span>
            <span class="muted library-use">{if unused { "Not used".to_string() } else { format!("Used by {}", item.used_by) }}</span>
            <span class="library-tools">
                <button type="button" class="ghost" on:click={
                    let id = id.clone();
                    move |_| editing.set(Some(id.clone()))
                }>"Edit"</button>
                {unused.then(|| view! {
                    <ActionButton method="DELETE" url=path::library_type(&cache::server_now(), &id) label="Delete" class="danger"
                        confirm=format!("Delete {} from the library?", item.name)/>
                })}
            </span>
        </div>
    }
}

/// Adds a type, or changes one everywhere it is used.
#[component]
fn TypeForm(item: Option<dto::LibraryItem>, editing: RwSignal<Option<String>>) -> impl IntoView {
    let id = item.as_ref().map(|item| item.id.clone());
    let used = item.as_ref().is_some_and(|item| !item.used_by.is_empty());
    let name = RwSignal::new(item.as_ref().map(|item| item.name.clone()).unwrap_or_default());
    let emoji = RwSignal::new(item.as_ref().map(|item| item.emoji.clone()).unwrap_or_default());
    let attending = RwSignal::new(item.as_ref().is_none_or(|item| item.attending));
    let problem = RwSignal::new(String::new());
    let asking = RwSignal::new(false);
    let saving = client::action(move |draft: &dto::TypeDraft| {
        let draft = draft.clone();
        let server = cache::server_now();
        let queries = cache::client();
        let id = id.clone();
        async move {
            let sent = match &id {
                Some(id) => api::send::<dto::Done, _>("PATCH", &path::library_type(&server, id), Some(&draft))
                    .await
                    .map(|_| ()),
                None => api::send::<dto::LibraryItem, _>("POST", &path::library(&server), Some(&draft))
                    .await
                    .map(|_| ()),
            };
            match sent {
                Ok(()) => {
                    editing.try_set(None);
                    cache::refresh_in(queries, &server);
                }
                Err(failure) => {
                    problem.try_set(failure.message);
                }
            }
        }
    });
    let save = move || {
        asking.set(false);
        problem.set(String::new());
        saving.dispatch(dto::TypeDraft {
            name: name.get_untracked(),
            emoji: emoji.get_untracked(),
            attending: attending.get_untracked(),
        });
    };
    view! {
        <form class="library-form" data-editor="" on:submit=move |ev| {
                ev.prevent_default();
                // A change to a type in use reaches every event with it, so say so first.
                if used { asking.set(true) } else { save() }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| if ev.key() == "Escape" { editing.set(None); }>
            <div class="attend-edit-name">
                <input class="emoji-input" maxlength="8" aria-label="Emoji" placeholder="🙂"
                    prop:value=move || emoji.get() on:input=move |ev| emoji.set(event_target_value(&ev))/>
                <input required maxlength="40" aria-label="Name" placeholder="Name"
                    prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            </div>
            <div class="attend-edit-row">
                <span title="People on an attending type get reminders and the attendee role.">"Attending"</span>
                <button type="button" role="switch" class=move || if attending.get() { "switch on" } else { "switch" }
                    aria-label="Attending"
                    aria-checked=move || if attending.get() { "true" } else { "false" }
                    on:click=move |_| attending.update(|on| *on = !*on)>
                    <span></span>
                </button>
            </div>
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            {move || if asking.get() {
                view! {
                    <InlineConfirm question="This changes every event and preset that uses it." yes="Save"
                        on_yes=Callback::new(move |_| save())
                        on_no=Callback::new(move |_| asking.set(false))/>
                }
                .into_any()
            } else {
                view! {
                    <div class="actions">
                        <button type="submit" class="primary" disabled=move || saving.pending().get()>"Save"</button>
                        <button type="button" class="ghost" on:click=move |_| editing.set(None)>"Cancel"</button>
                    </div>
                }
                .into_any()
            }}
        </form>
    }
}
