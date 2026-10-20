use std::sync::{Arc, Mutex};

use leptos::prelude::*;
use marian_types as dto;

use crate::format::spots;
use super::focus_left;
use crate::{api, cache, client};

const MAX_OPTIONS: usize = 8;

/// Which card shows its edit form.
#[derive(Clone, PartialEq)]
enum Edit {
    Off,
    Old(String),
    New,
}

/// The panel's shared state, handed to each card.
#[derive(Clone, Copy)]
struct Panel {
    state: RwSignal<dto::Signups>,
    problem: RwSignal<String>,
    /// The option list being saved; its `pending` keeps a second save waiting.
    saving: Action<(Vec<dto::SignupOption>, Option<Vec<String>>), ()>,
    editing: RwSignal<Edit>,
    picking: RwSignal<Option<String>>,
    /// The card whose handle is held, so only it can be dragged.
    grab: RwSignal<Option<String>>,
    dragging: RwSignal<Option<String>>,
    order_before: StoredValue<Vec<String>>,
    ids: StoredValue<(String, String)>,
    members: StoredValue<Vec<dto::Member>>,
    /// The server's roles, for limiting an option to one.
    roles: StoredValue<Vec<dto::Role>>,
    /// A new event in the browser, which keeps option changes until it is saved.
    local: Option<super::LocalDraft>,
}

impl Panel {
    fn order(self) -> Vec<String> {
        self.state
            .with_untracked(|now| now.options.iter().map(|opt| opt.id.clone()).collect())
    }

    fn wire(self) -> Vec<dto::SignupOption> {
        self.state
            .with_untracked(|now| now.options.iter().map(wire).collect())
    }

    /// Sends the whole option list. The cards already show the change, so only a
    /// failure is reported, and a failed reorder puts the old order back.
    fn save(self, list: Vec<dto::SignupOption>, undo: Option<Vec<String>>) {
        if let Some(local) = self.local {
            let _ = undo;
            self.problem.set(String::new());
            self.editing.set(Edit::Off);
            local.options.set(numbered(list));
            local.changed();
            return;
        }
        let (server, event) = self.ids.get_value();
        // The server tells open pages about the change, possibly before it
        // answers here, so the cache must already read the new options.
        let mut preview = self.state.get_untracked();
        apply_sent(&mut preview, &list);
        cache::apply_signups(cache::client(), &(server, event), &preview);
        self.saving.dispatch((list, undo));
    }
}

/// Gives options without an id a temporary one, `new-1` and on, so a new
/// event's options can be told apart before they are saved. The server takes an
/// id it doesn't know as a new option.
pub fn numbered(list: Vec<dto::SignupOption>) -> Vec<dto::SignupOption> {
    let taken: std::collections::HashSet<String> = list.iter().filter_map(|opt| opt.id.clone()).collect();
    let mut next = 1;
    list.into_iter()
        .map(|mut opt| {
            while opt.id.is_none() {
                let id = format!("new-{next}");
                next += 1;
                if !taken.contains(&id) {
                    opt.id = Some(id);
                }
            }
            opt
        })
        .collect()
}

/// The attendance a new event shows: its options, and nobody on them yet.
pub fn unsaved_signups(options: &[dto::SignupOption]) -> dto::Signups {
    dto::Signups {
        options: options
            .iter()
            .map(|opt| dto::OptionSignups {
                id: opt.id.clone().unwrap_or_default(),
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                role_id: opt.role_id.clone(),
                role: None,
                reminders: opt.reminders,
                confirmed_count: 0,
                waiting_count: 0,
                confirmed: Vec::new(),
                waiting: Vec::new(),
                mine: false,
                open: false,
                reason: String::new(),
                waitlist_next: false,
            })
            .collect(),
        notes: Vec::new(),
        show_names: true,
        can_manage: true,
        editable: true,
        posted: false,
    }
}

/// Puts a saved option list into the panel: new names, limits, and order.
/// Options the server hasn't numbered yet arrive with the refetch.
fn apply_sent(now: &mut dto::Signups, list: &[dto::SignupOption]) {
    for opt in &mut now.options {
        let Some(sent) = list.iter().find(|sent| sent.id.as_ref() == Some(&opt.id)) else {
            continue;
        };
        opt.name = sent.name.trim().to_string();
        opt.emoji = match sent.emoji.trim() {
            "" => "•".to_string(),
            emoji => emoji.to_string(),
        };
        opt.capacity = sent.capacity;
        opt.role_id = sent.role_id.clone();
        opt.reminders = sent.reminders;
    }
    now.options
        .sort_by_key(|opt| list.iter().position(|sent| sent.id.as_ref() == Some(&opt.id)));
}

fn wire(opt: &dto::OptionSignups) -> dto::SignupOption {
    dto::SignupOption {
        id: Some(opt.id.clone()),
        name: opt.name.clone(),
        emoji: opt.emoji.clone(),
        capacity: opt.capacity,
        role_id: opt.role_id.clone(),
        reminders: opt.reminders,
    }
}

/// Moves option `from` to where option `to` is. False when nothing moved.
fn move_to(options: &mut Vec<dto::OptionSignups>, from: &str, to: &str) -> bool {
    let at = |id: &str| options.iter().position(|opt| opt.id == id);
    let (Some(a), Some(b)) = (at(from), at(to)) else {
        return false;
    };
    if a == b {
        return false;
    }
    let item = options.remove(a);
    options.insert(b, item);
    true
}

/// Who is coming, and the controls for the options and responses for people who can edit the event.
#[component]
pub fn SignupPanel(
    server: String,
    event: String,
    signups: dto::Signups,
    /// People who can be added; empty for anyone who can't edit the event.
    #[prop(optional)]
    members: Vec<dto::Member>,
    #[prop(optional)]
    roles: Vec<dto::Role>,
) -> impl IntoView {
    let state = RwSignal::new(signups);
    let problem = RwSignal::new(String::new());
    let editing = RwSignal::new(Edit::Off);
    let ids = StoredValue::new((server, event));
    let saving = client::action(move |(list, undo): &(Vec<dto::SignupOption>, Option<Vec<String>>)| {
        let (list, undo) = (list.clone(), undo.clone());
        let (server, event) = ids.get_value();
        let url = format!("/api/servers/{server}/events/{event}/options");
        let key = (server.clone(), event);
        let queries = cache::client();
        async move {
            match api::send::<dto::Done, _>("PUT", &url, Some(&list)).await {
                Ok(_) => {
                    problem.try_set(String::new());
                    editing.try_set(Edit::Off);
                    state.try_update(|now| apply_sent(now, &list));
                    if let Some(now) = state.try_get_untracked() {
                        cache::apply_signups(queries, &key, &now);
                    }
                    cache::refresh_in(queries, &server);
                }
                Err(failure) => {
                    problem.try_set(failure.message);
                    cache::refresh_in(queries, &server);
                    if let Some(order) = undo {
                        state.try_update(|now| {
                            now.options
                                .sort_by_key(|opt| order.iter().position(|id| *id == opt.id))
                        });
                    }
                }
            }
        }
    });
    let panel = Panel {
        state,
        problem,
        saving,
        editing,
        picking: RwSignal::new(None),
        grab: RwSignal::new(None),
        dragging: RwSignal::new(None),
        order_before: StoredValue::new(Vec::new()),
        ids,
        members: StoredValue::new(members),
        roles: StoredValue::new(roles),
        local: use_context::<super::LocalDraft>(),
    };
    let manage = move || panel.state.with(|now| now.editable);

    let notes = move || {
        panel
            .state
            .get()
            .notes
            .into_iter()
            .map(|note| view! { <p class="notice info">{note}</p> })
            .collect_view()
    };

    let add = move || {
        let room = panel.state.with(|now| now.options.len() < MAX_OPTIONS);
        (manage() && room && panel.editing.get() != Edit::New).then(|| {
            view! {
                <button type="button" class="icon-btn" title="Add an option" aria-label="Add an option"
                    on:click=move |_| panel.editing.set(Edit::New)>
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/></svg>
                </button>
            }
        })
    };

    view! {
        <section>
            <div class="section-head attend-section-head">
                <h2>"Attendance"</h2>
                {add}
            </div>
            {move || {
                let text = panel.problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            {notes}
            <div class="attend-grid">
                <For
                    each=move || panel.state.with(|now| now.options.iter().map(|opt| opt.id.clone()).collect::<Vec<_>>())
                    key=|id| id.clone()
                    children=move |id| view! { <OptionCard panel id/> }
                />
                {move || (panel.editing.get() == Edit::New).then(|| view! {
                    <article class="attend-card editing">{edit_form(panel, None)}</article>
                })}
            </div>
        </section>
    }
}

#[component]
fn OptionCard(panel: Panel, id: String) -> impl IntoView {
    // Kept in a stored value so the closures below can share it.
    let id = StoredValue::new(id);
    let opt = Memo::new(move |_| {
        panel
            .state
            .with(|now| id.with_value(|id| now.options.iter().find(|opt| &opt.id == id).cloned()))
    });
    let manage = move || panel.state.with(|now| now.editable);
    let class = move || {
        let mut class = String::from("attend-card");
        if opt.with(|opt| opt.as_ref().is_some_and(|opt| opt.mine)) {
            class.push_str(" mine");
        }
        if panel.editing.get() == Edit::Old(id.get_value()) {
            class.push_str(" editing");
        }
        if panel.dragging.get() == Some(id.get_value()) {
            class.push_str(" dragging");
        }
        class
    };
    let start_drag = move |ev: leptos::ev::DragEvent| {
        if panel.grab.get_untracked() != Some(id.get_value()) {
            ev.prevent_default();
            return;
        }
        #[cfg(feature = "hydrate")]
        if let Some(data) = ev.data_transfer() {
            let _ = data.set_data("text/plain", &id.get_value());
            data.set_effect_allowed("move");
        }
        panel.order_before.set_value(panel.order());
        panel.picking.set(None);
        panel.dragging.set(Some(id.get_value()));
    };
    let over = move |ev: leptos::ev::DragEvent| {
        let Some(from) = panel.dragging.get_untracked() else {
            return;
        };
        ev.prevent_default();
        if from != id.get_value() {
            panel.state.update(|now| {
                move_to(&mut now.options, &from, &id.get_value());
            });
        }
    };
    let end_drag = move |_| {
        panel.grab.set(None);
        if panel.dragging.get_untracked().is_none() {
            return;
        }
        panel.dragging.set(None);
        let before = panel.order_before.get_value();
        if panel.order() != before {
            panel.save(panel.wire(), Some(before));
        }
    };
    // Arrow keys on the handle move the card one place.
    let nudge = move |ev: leptos::ev::KeyboardEvent| {
        let step: isize = match ev.key().as_str() {
            "ArrowUp" | "ArrowLeft" => -1,
            "ArrowDown" | "ArrowRight" => 1,
            _ => return,
        };
        ev.prevent_default();
        let before = panel.order();
        let Some(at) = before.iter().position(|other| *other == id.get_value()) else {
            return;
        };
        let Some(to) = at.checked_add_signed(step).and_then(|next| before.get(next)).cloned() else {
            return;
        };
        panel.state.update(|now| {
            move_to(&mut now.options, &id.get_value(), &to);
        });
        panel.save(panel.wire(), Some(before));
    };

    view! {
        <article
            class=class
            draggable=move || if panel.grab.get() == Some(id.get_value()) { "true" } else { "false" }
            on:dragstart=start_drag
            on:dragover=over
            on:drop=|ev| ev.prevent_default()
            on:dragend=end_drag
        >
            {move || {
                if panel.editing.get() == Edit::Old(id.get_value()) {
                    return edit_form(panel, opt.get_untracked()).into_any();
                }
                let tools = manage().then(|| view! {
                    <button type="button" class="drag-handle" title="Drag to reorder" aria-label="Move this option"
                        on:pointerdown=move |_| panel.grab.set(Some(id.get_value()))
                        on:pointerup=move |_| panel.grab.set(None)
                        on:keydown=nudge>
                        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M11 18c0 1.1-.9 2-2 2s-2-.9-2-2 .9-2 2-2 2 .9 2 2zm-2-8c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0-6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm6 4c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"/></svg>
                    </button>
                });
                let actions = manage().then(|| {
                    let posted = panel.state.with_untracked(|now| now.posted);
                    view! {
                        <div class="attend-tools">
                            {posted.then(|| view! {
                                <button type="button" class="icon-btn" title="Add or remove people" aria-label="Add or remove people"
                                    aria-expanded=move || if panel.picking.get() == Some(id.get_value()) { "true" } else { "false" }
                                    on:mousedown=|ev| ev.prevent_default()
                                    on:click=move |_| panel.picking.update(|open| {
                                        *open = if *open == Some(id.get_value()) { None } else { Some(id.get_value()) };
                                    })>
                                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M15 12c2.21 0 4-1.79 4-4s-1.79-4-4-4-4 1.79-4 4 1.79 4 4 4zm-9-2V7H4v3H1v2h3v3h2v-3h3v-2H6zm9 4c-2.67 0-8 1.34-8 4v2h16v-2c0-2.66-5.33-4-8-4z"/></svg>
                                </button>
                            })}
                            <button type="button" class="icon-btn" title="Edit this option" aria-label="Edit this option"
                                on:click=move |_| {
                                    panel.picking.set(None);
                                    panel.editing.set(Edit::Old(id.get_value()));
                                }>
                                <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"/></svg>
                            </button>
                        </div>
                    }
                });
                view! {
                    {tools}
                    <div class="attend-head">
                        {move || opt.get().map(|opt| {
                            let waiting = (opt.waiting_count > 0).then(|| format!(" +{} waiting", opt.waiting_count));
                            view! {
                                <p class="attend-name">
                                    {opt.emoji}" "<strong>{opt.name}</strong>
                                    <span class="pill">{spots(opt.confirmed_count, opt.capacity)}{waiting}</span>
                                </p>
                            }
                        })}
                        {actions}
                    </div>
                    {move || (panel.picking.get() == Some(id.get_value())).then(|| view! { <MemberPicker panel option=id.get_value()/> })}
                    {move || opt.get().map(|opt| {
                        let confirmed = opt.confirmed.into_iter().map(|person| view! {
                            <div class="person">
                                <span>{person.name}</span>
                            </div>
                        }).collect_view();
                        let waiting = opt.waiting.into_iter().map(|person| view! {
                            <div class="person"><span>{person.name}" · waiting"</span></div>
                        }).collect_view();
                        view! {
                            <div class="attend-people">
                                {confirmed}
                                {waiting}
                            </div>
                            {opt.role.map(|role| view! { <p class="muted">{role}" only"</p> })}
                        }
                    })}
                }
                .into_any()
            }}
        </article>
    }
}

/// The edit face of a card. `None` is a new option.
fn edit_form(panel: Panel, opt: Option<dto::OptionSignups>) -> impl IntoView {
    let id = StoredValue::new(opt.as_ref().map(|opt| opt.id.clone()));
    let base = opt.as_ref().map(wire);
    let emoji = RwSignal::new(opt.as_ref().map(|opt| opt.emoji.clone()).unwrap_or_default());
    let name = RwSignal::new(opt.as_ref().map(|opt| opt.name.clone()).unwrap_or_default());
    let capacity = RwSignal::new(
        opt.as_ref()
            .and_then(|opt| opt.capacity)
            .map(|n| n.to_string())
            .unwrap_or_default(),
    );
    let attending = RwSignal::new(opt.as_ref().is_none_or(|opt| opt.reminders));
    let limit = RwSignal::new(base.as_ref().and_then(|base| base.role_id.clone()));
    // Before posting nobody has picked an option yet, so one can go.
    let removable = id.with_value(Option::is_some) && panel.state.with_untracked(|now| !now.posted && now.options.len() > 1);
    let cancel = move || {
        panel.problem.set(String::new());
        panel.editing.set(Edit::Off);
    };
    let save = move || {
        let title = name.get_untracked().trim().to_string();
        if title.is_empty() {
            panel.problem.set("Give the option a name.".into());
            return;
        }
        let text = capacity.get_untracked();
        let cap = match text.trim() {
            "" => None,
            text => match text.parse::<i64>() {
                Ok(n) if (1..=500).contains(&n) => Some(n),
                _ => {
                    panel
                        .problem
                        .set("Capacity is a number from 1 to 500, or empty for unlimited.".into());
                    return;
                }
            },
        };
        let id = id.get_value();
        let next = dto::SignupOption {
            id: id.clone(),
            name: title,
            emoji: emoji.get_untracked().trim().to_string(),
            capacity: cap,
            role_id: limit.get_untracked(),
            reminders: attending.get_untracked(),
        };
        let mut list = panel.wire();
        match list.iter_mut().find(|opt| opt.id.is_some() && opt.id == id) {
            Some(slot) => *slot = next,
            None => list.push(next),
        }
        panel.save(list, None);
    };
    let asking = RwSignal::new(false);
    let remove = move || {
        let Some(id) = id.get_value() else {
            return;
        };
        let list: Vec<dto::SignupOption> = panel.wire().into_iter().filter(|opt| opt.id.as_ref() != Some(&id)).collect();
        panel.state.update(|now| now.options.retain(|opt| opt.id != id));
        panel.save(list, None);
    };
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(field) = input.get() {
            let _ = field.focus();
        }
    });
    view! {
        <form class="attend-edit"
            on:submit=move |ev| {
                ev.prevent_default();
                save();
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    cancel();
                }
            }>
            <div class="attend-edit-name">
                <input class="emoji-input" maxlength="8" aria-label="Emoji" placeholder="🙂"
                    prop:value=move || emoji.get() on:input=move |ev| emoji.set(event_target_value(&ev))/>
                <input node_ref=input maxlength="40" aria-label="Option name" placeholder="Name"
                    prop:value=move || name.get() on:input=move |ev| name.set(event_target_value(&ev))/>
            </div>
            <label class="attend-edit-row">
                <span>"Capacity"</span>
                <input type="number" min="1" max="500" inputmode="numeric" placeholder="Unlimited"
                    prop:value=move || capacity.get() on:input=move |ev| capacity.set(event_target_value(&ev))/>
            </label>
            <label class="attend-edit-row">
                <span>"Who can pick"</span>
                <select on:change=move |ev| limit.set(Some(event_target_value(&ev)).filter(|id| !id.is_empty()))>
                    <option value="" selected=limit.get_untracked().is_none()>"Anyone"</option>
                    {move || panel.roles.with_value(|roles| {
                        let now = limit.get_untracked();
                        roles.iter().map(|role| view! {
                            <option value=role.id.clone() selected=now.as_ref() == Some(&role.id)>{role.name.clone()}</option>
                        }).collect_view()
                    })}
                </select>
            </label>
            <div class="attend-edit-row">
                <span title="People on an attending option get reminders and the attendee role.">"Attending"</span>
                <button type="button" role="switch" class=move || if attending.get() { "switch on" } else { "switch" }
                    aria-label="Attending"
                    aria-checked=move || if attending.get() { "true" } else { "false" }
                    on:click=move |_| attending.update(|on| *on = !*on)>
                    <span></span>
                </button>
            </div>
            {move || if asking.get() {
                let title = name.get_untracked();
                let question = if title.trim().is_empty() {
                    "Remove this option?".to_string()
                } else {
                    format!("Remove {}?", title.trim())
                };
                view! {
                    <div class="attend-edit-actions">
                        <super::InlineConfirm question yes="Remove"
                            on_yes=Callback::new(move |_| remove())
                            on_no=Callback::new(move |_| asking.set(false))/>
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="attend-edit-actions">
                        {removable.then(|| view! {
                            <button type="button" class="danger option-remove" on:click=move |_| asking.set(true)>"Remove"</button>
                        })}
                        <button type="submit" class="primary" disabled=move || panel.saving.pending().get()>"Save"</button>
                        <button type="button" class="ghost" on:click=move |_| cancel()>"Cancel"</button>
                    </div>
                }
                .into_any()
            }}
        </form>
    }
}

/// Everyone the picker can name: the server's members and anyone already responding.
fn people(panel: Panel) -> Vec<dto::Person> {
    let mut list: Vec<dto::Person> = panel.state.with_untracked(|now| {
        now.options
            .iter()
            .flat_map(|opt| opt.confirmed.iter().chain(opt.waiting.iter()))
            .cloned()
            .collect()
    });
    panel.members.with_value(|members| {
        list.extend(members.iter().map(|member| dto::Person {
            id: member.id.clone(),
            name: member.name.clone(),
            color: member.color.clone(),
        }));
    });
    let mut seen = std::collections::HashSet::new();
    list.retain(|person| seen.insert(person.id.clone()));
    list
}

/// A searchable list of people for one option. Clicks toggle them; the picks are
/// sent when the list closes, whether by losing focus, Escape, or the page moving on.
#[component]
fn MemberPicker(panel: Panel, option: String) -> impl IntoView {
    let (here, others): (Vec<String>, Vec<(String, String)>) = panel.state.with_untracked(|now| {
        let mut here = Vec::new();
        let mut others = Vec::new();
        for opt in &now.options {
            for person in opt.confirmed.iter().chain(opt.waiting.iter()) {
                if opt.id == option {
                    here.push(person.id.clone());
                } else {
                    others.push((person.id.clone(), opt.name.clone()));
                }
            }
        }
        (here, others)
    });
    let start = people(panel);
    let known = Arc::new(Mutex::new(start.clone()));
    let everyone = RwSignal::new(start);
    let others = StoredValue::new(others);
    let chosen = RwSignal::new(here.clone());
    let picks = Arc::new(Mutex::new(here.clone()));
    let needle = RwSignal::new(String::new());
    // Who Discord returned for the current text, and whether it is still looking.
    let found = RwSignal::new(Vec::<String>::new());
    let searching = RwSignal::new(false);
    let asked = StoredValue::new(0_u64);
    let root = NodeRef::<leptos::html::Div>::new();
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(field) = input.get() {
            let _ = field.focus();
        }
    });

    let toggle = {
        let picks = Arc::clone(&picks);
        move |id: String| {
            chosen.update(|list| match list.iter().position(|other| *other == id) {
                Some(at) => {
                    list.remove(at);
                }
                None => list.push(id),
            });
            if let Ok(mut mirror) = picks.lock() {
                *mirror = chosen.get_untracked();
            }
        }
    };

    let search = {
        let known = Arc::clone(&known);
        let server = panel.ids.get_value().0;
        move |text: String| {
            let turn = asked.get_value() + 1;
            asked.set_value(turn);
            found.set(Vec::new());
            if text.trim().is_empty() {
                searching.set(false);
                return;
            }
            searching.set(true);
            let known = Arc::clone(&known);
            let url = format!("/api/servers/{server}/members/search?q={}", api::encode(text.trim()));
            // Waits for a pause in typing; a newer search makes this one moot.
            client::later(250, move || {
                if asked.try_get_value() != Some(turn) {
                    return;
                }
                client::spawn(async move {
                    let result = api::send::<Vec<dto::Member>, ()>("GET", &url, None).await;
                    if asked.try_get_value() != Some(turn) {
                        return;
                    }
                    let people: Vec<dto::Person> = result
                        .unwrap_or_default()
                        .into_iter()
                        .map(|member| dto::Person {
                            id: member.id,
                            name: member.name,
                            color: member.color,
                        })
                        .collect();
                    found.try_set(people.iter().map(|person| person.id.clone()).collect());
                    everyone.try_update(|list| {
                        for person in &people {
                            if !list.iter().any(|known| known.id == person.id) {
                                list.push(person.clone());
                            }
                        }
                        if let Ok(mut mirror) = known.lock() {
                            *mirror = list.clone();
                        }
                    });
                    searching.try_set(false);
                });
            });
        }
    };

    on_cleanup({
        let (server, event) = panel.ids.get_value();
        let toasts = client::toast_list();
        let queries = cache::client();
        let state = panel.state;
        move || {
            let now = picks.lock().map(|list| list.clone()).unwrap_or_default();
            let everyone = known.lock().map(|list| list.clone()).unwrap_or_default();
            let adds: Vec<String> = now.iter().filter(|id| !here.contains(id)).cloned().collect();
            let drops: Vec<String> = here.iter().filter(|id| !now.contains(id)).cloned().collect();
            if adds.is_empty() && drops.is_empty() {
                return;
            }
            state.try_update(|now| {
                if let Some(opt) = now.options.iter_mut().find(|opt| opt.id == option) {
                    opt.confirmed.retain(|person| !drops.contains(&person.id));
                    opt.waiting.retain(|person| !drops.contains(&person.id));
                    opt.confirmed.extend(
                        everyone
                            .iter()
                            .filter(|person| adds.contains(&person.id))
                            .cloned(),
                    );
                    opt.confirmed_count = opt.confirmed.len() as i64;
                }
            });
            if let Some(now) = state.try_get_untracked() {
                cache::apply_signups(queries, &(server.clone(), event.clone()), &now);
            }
            client::spawn(async move {
                let mut failed = None;
                let changes = adds
                    .iter()
                    .map(|id| ("PUT", id.clone()))
                    .chain(drops.iter().map(|id| ("DELETE", id.clone())));
                for (method, member) in changes {
                    let url = format!("/api/servers/{server}/events/{event}/signups/{member}/{option}");
                    if let Err(failure) = api::send::<dto::Done, ()>(method, &url, None).await {
                        failed = Some(failure.message);
                    }
                }
                // The card already shows the picks; the picker is gone, so a
                // failure can only be said in a toast.
                if let (Some(list), Some(message)) = (toasts, failed) {
                    client::push_toast(list, message);
                }
                cache::refresh_in(queries, &server);
            });
        }
    });

    let rows = move || {
        let needle = needle.get().trim().to_lowercase();
        let picked = chosen.get();
        let found = found.get();
        everyone.with(|everyone| {
            others.with_value(|others| {
                everyone
                    .iter()
                    .filter(|person| {
                        if needle.is_empty() {
                            picked.contains(&person.id) || others.iter().any(|(id, _)| *id == person.id)
                        } else {
                            found.contains(&person.id) || person.name.to_lowercase().contains(&needle)
                        }
                    })
                    .take(50)
                    .map(|person| {
                        let id = person.id.clone();
                        let on = picked.contains(&id);
                        let elsewhere: Vec<&str> = others
                            .iter()
                            .filter(|(other, _)| *other == id)
                            .map(|(_, name)| name.as_str())
                            .collect();
                        let toggle = toggle.clone();
                        let initial = person.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                        view! {
                            <button type="button" class="pick-row" role="option"
                                aria-selected=if on { "true" } else { "false" }
                                on:click=move |_| toggle(id.clone())>
                                <span class="avatar" style=format!("background: {}", person.color) aria-hidden="true">{initial}</span>
                                <span class="pick-name">{person.name.clone()}</span>
                                {(!on && !elsewhere.is_empty()).then(|| view! { <span class="pick-note">{elsewhere.join(", ")}</span> })}
                                {on.then(|| view! {
                                    <svg class="pick-check" viewBox="0 0 24 24" aria-hidden="true"><path d="M9 16.17 4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
                                })}
                            </button>
                        }
                    })
                    .collect_view()
            })
        })
    };
    let empty = move || {
        let text = needle.with(|text| text.trim().to_lowercase());
        if searching.get() {
            return Some(view! { <p class="pick-empty">"Searching the server…"</p> }.into_any());
        }
        if text.is_empty() {
            let none = chosen.with(|list| list.is_empty()) && others.with_value(|list| list.is_empty());
            return none.then(|| view! { <p class="pick-empty">"No one has responded yet. Search to add someone."</p> }.into_any());
        }
        let any = found.with(|found| !found.is_empty())
            || everyone.with(|list| list.iter().any(|person| person.name.to_lowercase().contains(&text)));
        (!any).then(|| view! { <p class="pick-empty">"No one in this server matches."</p> }.into_any())
    };

    view! {
        <div class="picker" node_ref=root
            on:mousedown=|ev| ev.prevent_default()
            on:focusout=move |ev| {
                if focus_left(&ev, root) {
                    panel.picking.set(None);
                }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    panel.picking.set(None);
                }
            }>
            <input node_ref=input type="search" placeholder="Search for members" aria-label="Search for members"
                autocomplete="off"
                on:mousedown=|ev| ev.stop_propagation()
                prop:value=move || needle.get()
                on:input=move |ev| {
                    let text = event_target_value(&ev);
                    needle.set(text.clone());
                    search(text);
                }/>
            <div class="pick-list" role="listbox" aria-multiselectable="true">
                {rows}
                {empty}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(id: &str) -> dto::OptionSignups {
        dto::OptionSignups {
            id: id.to_string(),
            name: format!("Option {id}"),
            emoji: String::new(),
            capacity: None,
            role_id: None,
            role: None,
            reminders: true,
            confirmed_count: 0,
            waiting_count: 0,
            confirmed: Vec::new(),
            waiting: Vec::new(),
            mine: false,
            open: true,
            reason: String::new(),
            waitlist_next: false,
        }
    }

    fn ids(options: &[dto::OptionSignups]) -> Vec<&str> {
        options.iter().map(|opt| opt.id.as_str()).collect()
    }

    #[test]
    fn a_dragged_option_takes_the_place_it_is_over() {
        let mut options = vec![option("a"), option("b"), option("c")];
        assert!(move_to(&mut options, "a", "c"));
        assert_eq!(ids(&options), ["b", "c", "a"]);
        assert!(move_to(&mut options, "a", "b"));
        assert_eq!(ids(&options), ["a", "b", "c"]);
        assert!(!move_to(&mut options, "b", "b"));
        assert!(!move_to(&mut options, "z", "b"));
    }

    #[test]
    fn unsaved_options_get_temporary_ids_that_do_not_clash() {
        let named = |id: Option<&str>| dto::SignupOption {
            id: id.map(str::to_string),
            name: "x".into(),
            emoji: String::new(),
            capacity: None,
            role_id: None,
            reminders: true,
        };
        let list = numbered(vec![named(None), named(Some("new-1")), named(None)]);
        let ids: Vec<_> = list.iter().map(|opt| opt.id.clone().unwrap()).collect();
        assert_eq!(ids, ["new-2", "new-1", "new-3"]);
    }
}
