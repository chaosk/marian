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
                type_id: opt.type_id.clone(),
                name: opt.name.clone(),
                emoji: opt.emoji.clone(),
                capacity: opt.capacity,
                role_id: opt.role_id.clone(),
                role: None,
                attending: opt.attending,
                max_guests: opt.max_guests,
                my_guests: 0,
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
        opt.capacity = sent.capacity;
        opt.max_guests = sent.max_guests;
        opt.role_id = sent.role_id.clone();
    }
    now.options
        .sort_by_key(|opt| list.iter().position(|sent| sent.id.as_ref() == Some(&opt.id)));
}

fn wire(opt: &dto::OptionSignups) -> dto::SignupOption {
    dto::SignupOption {
        id: Some(opt.id.clone()),
        type_id: opt.type_id.clone(),
        name: opt.name.clone(),
        emoji: opt.emoji.clone(),
        attending: opt.attending,
        capacity: opt.capacity,
        max_guests: opt.max_guests,
        role_id: opt.role_id.clone(),
    }
}

/// A library type as a new option on the event.
fn from_library(item: &dto::LibraryItem) -> dto::SignupOption {
    dto::SignupOption {
        id: None,
        type_id: item.id.clone(),
        name: item.name.clone(),
        emoji: item.emoji.clone(),
        attending: item.attending,
        capacity: None,
        max_guests: 0,
        role_id: None,
    }
}

/// "1 response goes" or "3 responses go", for a confirm that says what goes.
fn responses_go(count: i64) -> String {
    if count == 1 { "1 response goes".into() } else { format!("{count} responses go") }
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
        let url = api::path::event_options(&server, &event);
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
                <button type="button" class="icon-btn" title="Add an attendance type" aria-label="Add an attendance type"
                    on:click=move |_| panel.editing.set(Edit::New)>
                    <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M19 13h-6v6h-2v-6H5v-2h6V5h2v6h6v2z"/></svg>
                </button>
            }
        })
    };
    let presets = move || manage().then(|| view! { <PresetChooser panel/> });

    view! {
        <section>
            <div class="section-head attend-section-head">
                <h2>"Attendance"</h2>
                {add}
                {presets}
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
                    <article class="attend-card editing"><TypePicker panel/></article>
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
                    if let Some(opt) = opt.get_untracked() {
                        return edit_form(panel, opt).into_any();
                    }
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
                        // Guests can be changed here only where the option takes them,
                        // and only once the event is posted, as on Discord.
                        let stepper = manage() && opt.max_guests > 0 && panel.state.with_untracked(|now| now.posted);
                        let most = opt.max_guests;
                        let confirmed = opt.confirmed.into_iter().map(|person| {
                            person_row(panel, id.get_value(), person, false, stepper.then_some(most))
                        }).collect_view();
                        let waiting = opt.waiting.into_iter().map(|person| {
                            person_row(panel, id.get_value(), person, true, stepper.then_some(most))
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

/// One person on an option: their name, the guests they bring, and for someone
/// who can edit the event, a control to change that count up to `most`.
fn person_row(panel: Panel, option: String, person: dto::Person, waiting: bool, most: Option<i64>) -> impl IntoView {
    let label = match (person.guests, waiting) {
        (0, false) => person.name.clone(),
        (0, true) => format!("{} · waiting", person.name),
        (guests, false) => format!("{} (+{guests})", person.name),
        (guests, true) => format!("{} (+{guests}) · waiting", person.name),
    };
    let stepper = most.map(|most| {
        let guests = person.guests;
        let name = person.name.clone();
        let set = guest_setter(panel, option, person.id.clone());
        let (fewer, more) = (set.clone(), set);
        view! {
            <span class="guest-step" role="group" aria-label=format!("Guests {name} brings")>
                <button type="button" class="icon-btn" title="One guest fewer" aria-label="One guest fewer"
                    disabled={guests == 0} on:click=move |_| fewer(guests - 1)>"−"</button>
                <button type="button" class="icon-btn" title="One guest more" aria-label="One guest more"
                    disabled={guests >= most} on:click=move |_| more(guests + 1)>"+"</button>
            </span>
        }
    });
    view! {
        <div class="person">
            <span class="person-name">
                <super::Avatar name=person.name.clone() color=person.color.clone() avatar=person.avatar.clone() small=true/>
                <span>{label}</span>
            </span>
            {stepper}
        </div>
    }
}

/// Sets how many guests someone brings on an option. The card shows the new
/// count at once; the server's answer, and any refusal, follow with the refetch.
fn guest_setter(panel: Panel, option: String, member: String) -> impl Fn(i64) + Clone + 'static {
    let toasts = client::toast_list();
    let queries = cache::client();
    let (server, event) = panel.ids.get_value();
    move |guests: i64| {
        panel.state.update(|now| {
            if let Some(opt) = now.options.iter_mut().find(|opt| opt.id == option) {
                let mut change = 0;
                for person in opt.confirmed.iter_mut().filter(|person| person.id == member) {
                    change = guests - person.guests;
                    person.guests = guests;
                }
                for person in opt.waiting.iter_mut().filter(|person| person.id == member) {
                    person.guests = guests;
                }
                opt.confirmed_count += change;
            }
        });
        let url = format!("{}?guests={guests}", api::path::member_signup(&server, &event, &member, &option));
        let server = server.clone();
        client::spawn(async move {
            if let Err(failure) = api::send::<dto::Done, ()>("PUT", &url, None).await {
                if let Some(list) = toasts {
                    client::push_toast(list, failure.message);
                }
            }
            cache::refresh_in(queries, &server);
        });
    }
}

/// The edit face of a card: what this event sets for the type. Its name, emoji,
/// and attending belong to the library, on the Presets page.
fn edit_form(panel: Panel, opt: dto::OptionSignups) -> impl IntoView {
    let id = StoredValue::new(opt.id.clone());
    let capacity = RwSignal::new(opt.capacity.map(|n| n.to_string()).unwrap_or_default());
    let guests_on = RwSignal::new(opt.max_guests > 0);
    let guests_most = RwSignal::new(if opt.max_guests > 0 { opt.max_guests } else { 3 }.to_string());
    let limit = RwSignal::new(opt.role_id.clone());
    let removable = panel.state.with_untracked(|now| now.options.len() > 1);
    let held = opt.confirmed_count + opt.waiting_count;
    let question = if held > 0 {
        format!("Remove {}? {} with it.", opt.name, responses_go(held))
    } else {
        format!("Remove {}?", opt.name)
    };
    let base = crate::base();
    let cancel = move || {
        panel.problem.set(String::new());
        panel.editing.set(Edit::Off);
    };
    let save = move || {
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
        let most = if guests_on.get_untracked() {
            match guests_most.get_untracked().trim().parse::<i64>() {
                Ok(n) if (1..=10).contains(&n) => n,
                _ => {
                    panel.problem.set("Guests each is a number from 1 to 10.".into());
                    return;
                }
            }
        } else {
            0
        };
        let id = id.get_value();
        let mut list = panel.wire();
        if let Some(slot) = list.iter_mut().find(|opt| opt.id.as_ref() == Some(&id)) {
            slot.capacity = cap;
            slot.max_guests = most;
            slot.role_id = limit.get_untracked();
        }
        panel.save(list, None);
    };
    let asking = RwSignal::new(false);
    let remove = move || {
        let id = id.get_value();
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
        <form class="attend-edit" data-editor=""
            on:submit=move |ev| {
                ev.prevent_default();
                save();
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    cancel();
                }
            }>
            <p class="attend-name">{opt.emoji.clone()}" "<strong>{opt.name.clone()}</strong></p>
            <p class="attend-edit-note">
                {if opt.attending { "Counts as attending. " } else { "" }}
                "Rename it in the "<crate::A href=format!("{base}/presets")>"library"</crate::A>"."
            </p>
            <label class="attend-edit-row">
                <span>"Capacity"</span>
                <input node_ref=input type="number" min="1" max="500" inputmode="numeric" placeholder="Unlimited"
                    prop:value=move || capacity.get() on:input=move |ev| capacity.set(event_target_value(&ev))/>
            </label>
            <div class="attend-edit-row">
                <span>"Guests"</span>
                <span class="attend-guests">
                    <label class="checks">
                        <input type="checkbox" prop:checked=move || guests_on.get()
                            on:change=move |ev| guests_on.set(event_target_checked(&ev))/>
                        " "<span>"Allow guests"</span>
                    </label>
                    {move || guests_on.get().then(|| view! {
                        <label class="attend-guests-most">
                            "up to "
                            <input type="number" min="1" max="10" inputmode="numeric" aria-label="Guests each person may bring"
                                prop:value=move || guests_most.get() on:input=move |ev| guests_most.set(event_target_value(&ev))/>
                            " each"
                        </label>
                    })}
                </span>
            </div>
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
            {move || if asking.get() {
                view! {
                    <div class="attend-edit-actions">
                        <super::InlineConfirm question=question.clone() yes="Remove"
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

/// Adds a type to the event: one from the library, found by name, or a new one
/// made in the library on the spot.
#[component]
fn TypePicker(panel: Panel) -> impl IntoView {
    let library = cache::listen(cache::library_scope(), cache::server_key);
    let needle = RwSignal::new(String::new());
    let making = RwSignal::new(false);
    let emoji = RwSignal::new(String::new());
    let attending = RwSignal::new(true);
    let input = NodeRef::<leptos::html::Input>::new();
    Effect::new(move |_| {
        if let Some(field) = input.get() {
            let _ = field.focus();
        }
    });
    let close = move || {
        panel.problem.set(String::new());
        panel.editing.set(Edit::Off);
    };
    let add = move |item: dto::LibraryItem| {
        let mut list = panel.wire();
        list.push(from_library(&item));
        panel.save(list, None);
    };
    let (server, event) = panel.ids.get_value();
    let local = panel.local;
    // Making a type tells open pages the server changed, which can redraw this
    // panel before the answer comes back. So the list goes along with the
    // request, and the new type joins it without the panel: in the page's draft,
    // or saved straight to the event.
    let create = client::action(move |(draft, list): &(dto::TypeDraft, Vec<dto::SignupOption>)| {
        let (draft, mut list) = (draft.clone(), list.clone());
        let library_url = api::path::library(&server);
        let options_url = api::path::event_options(&server, &event);
        let queries = cache::client();
        let server = server.clone();
        async move {
            let made = match api::send::<dto::LibraryItem, _>("POST", &library_url, Some(&draft)).await {
                Ok(made) => made,
                Err(failure) => {
                    panel.problem.try_set(failure.message);
                    return;
                }
            };
            list.push(from_library(&made));
            match local {
                Some(local) => {
                    local.options.try_set(numbered(list));
                    local.changed();
                }
                None => {
                    if let Err(failure) = api::send::<dto::Done, _>("PUT", &options_url, Some(&list)).await {
                        panel.problem.try_set(failure.message);
                    }
                }
            }
            panel.editing.try_set(Edit::Off);
            cache::refresh_in(queries, &server);
        }
    });
    let make = move || {
        create.dispatch((
            dto::TypeDraft {
                name: needle.get_untracked().trim().to_string(),
                emoji: emoji.get_untracked(),
                attending: attending.get_untracked(),
            },
            panel.wire(),
        ));
    };
    let rows = move || {
        let on_event: Vec<String> = panel.state.with(|now| now.options.iter().map(|opt| opt.type_id.clone()).collect());
        let text = needle.get().trim().to_lowercase();
        let Some(Ok(items)) = library.get() else {
            return view! { <p class="pick-empty">"Loading the library…"</p> }.into_any();
        };
        let exact = items.iter().any(|item| item.name.to_lowercase() == text);
        let shown: Vec<dto::LibraryItem> = items
            .into_iter()
            .filter(|item| !on_event.contains(&item.id))
            .filter(|item| text.is_empty() || item.name.to_lowercase().contains(&text))
            .collect();
        let list = shown
            .into_iter()
            .map(|item| {
                let label = format!("{} {}", item.emoji, item.name);
                let attending = item.attending;
                view! {
                    <button type="button" class="pick-row" on:click=move |_| add(item.clone())>
                        <span class="pick-name">{label}</span>
                        {attending.then(|| view! { <span class="pick-note">"attending"</span> })}
                    </button>
                }
            })
            .collect_view();
        let offer = (!text.is_empty() && !exact).then(|| {
            let name = needle.get_untracked().trim().to_string();
            view! {
                <button type="button" class="pick-row pick-create" on:click=move |_| making.set(true)>
                    <span class="pick-name">{format!("Create “{name}”")}</span>
                    <span class="pick-note">"adds it to the library"</span>
                </button>
            }
        });
        view! { {list}{offer} }.into_any()
    };
    view! {
        <form class="attend-edit type-picker" data-editor=""
            on:submit=move |ev| {
                ev.prevent_default();
                if making.get_untracked() { make(); }
            }
            on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                if ev.key() == "Escape" {
                    ev.stop_propagation();
                    close();
                }
            }>
            <input node_ref=input type="search" placeholder="Find or create a type" aria-label="Attendance type"
                autocomplete="off"
                prop:value=move || needle.get()
                on:input=move |ev| { needle.set(event_target_value(&ev)); making.set(false); }/>
            {move || if making.get() {
                view! {
                    <div class="attend-edit-name">
                        <input class="emoji-input" maxlength="8" aria-label="Emoji" placeholder="🙂"
                            prop:value=move || emoji.get() on:input=move |ev| emoji.set(event_target_value(&ev))/>
                        <span class="attend-new-name">{move || needle.get().trim().to_string()}</span>
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
                    <div class="attend-edit-actions">
                        <button type="submit" class="primary" disabled=move || create.pending().get()>"Create and add"</button>
                        <button type="button" class="ghost" on:click=move |_| making.set(false)>"Back"</button>
                    </div>
                }
                .into_any()
            } else {
                view! {
                    <div class="pick-list" role="listbox" aria-label="Attendance types">{rows}</div>
                    <div class="attend-edit-actions">
                        <button type="button" class="ghost" on:click=move |_| close()>"Cancel"</button>
                    </div>
                }
                .into_any()
            }}
        </form>
    }
}

/// Swaps the event's types for a preset's. Types in both keep their responses;
/// when responses would go, it says how many first.
#[component]
fn PresetChooser(panel: Panel) -> impl IntoView {
    let presets = cache::listen(cache::presets_scope(), cache::server_key);
    let open = RwSignal::new(false);
    let asking = RwSignal::new(None::<(dto::Preset, String)>);
    let root = NodeRef::<leptos::html::Div>::new();
    let (server, event) = panel.ids.get_value();
    let applying = client::action(move |preset: &dto::Preset| {
        let url = api::path::event_preset(&server, &event);
        let body = dto::ApplyPreset { preset: preset.id.clone() };
        let queries = cache::client();
        let server = server.clone();
        async move {
            match api::send::<dto::Done, _>("POST", &url, Some(&body)).await {
                Ok(_) => panel.problem.try_set(String::new()),
                Err(failure) => panel.problem.try_set(failure.message),
            };
            cache::refresh_in(queries, &server);
        }
    });
    let apply = move |preset: dto::Preset| {
        open.set(false);
        asking.set(None);
        panel.editing.set(Edit::Off);
        match panel.local {
            Some(local) => {
                local.options.set(numbered(preset.options.clone()));
                local.event.update(|event| event.multiple_signups = preset.multiple_signups);
                local.changed();
            }
            None => {
                applying.dispatch(preset);
            }
        }
    };
    // Responses on types the preset doesn't have, which would go with them.
    let choose = move |preset: dto::Preset| {
        let dropped: Vec<(String, i64)> = panel.state.with_untracked(|now| {
            now.options
                .iter()
                .filter(|opt| !preset.options.iter().any(|kept| kept.type_id == opt.type_id))
                .map(|opt| (opt.name.clone(), opt.confirmed_count + opt.waiting_count))
                .filter(|(_, held)| *held > 0)
                .collect()
        });
        if dropped.is_empty() {
            apply(preset);
        } else {
            let held: i64 = dropped.iter().map(|(_, held)| held).sum();
            let names: Vec<String> = dropped.into_iter().map(|(name, _)| name).collect();
            let question = format!("Use {}? {} from {}.", preset.name, responses_go(held), names.join(", "));
            asking.set(Some((preset, question)));
        }
    };
    // A question about dropped responses waits for its answer.
    super::close_on_outside_press(root, move || open.get_untracked() && asking.get_untracked().is_none(), move || open.set(false));
    view! {
        <div class="inline-anchor preset-chooser" node_ref=root
            on:focusout=move |ev| {
                if open.get_untracked() && asking.get_untracked().is_none() && super::focus_left(&ev, root) {
                    open.set(false);
                }
            }>
            <button type="button" class="ghost preset-button" aria-haspopup="listbox"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                disabled=move || applying.pending().get()
                on:click=move |_| { asking.set(None); open.update(|on| *on = !*on); }>
                "Change preset"
            </button>
            {move || open.get().then(|| view! {
                <div class="popover preset-popover" on:keydown=move |ev: leptos::ev::KeyboardEvent| {
                    if ev.key() == "Escape" { ev.stop_propagation(); open.set(false); asking.set(None); }
                }>
                    {move || match asking.get() {
                        Some((preset, question)) => view! {
                            <super::InlineConfirm question yes="Use it"
                                on_yes=Callback::new(move |_| apply(preset.clone()))
                                on_no=Callback::new(move |_| asking.set(None))/>
                        }
                        .into_any(),
                        None => match presets.get() {
                            Some(Ok(list)) => list
                                .into_iter()
                                .map(|preset| {
                                    let names = preset.options.iter().map(|opt| opt.name.clone()).collect::<Vec<_>>().join(", ");
                                    let label = preset.name.clone();
                                    let default = preset.default;
                                    view! {
                                        <button type="button" class="pick-row" on:click=move |_| choose(preset.clone())>
                                            <span class="pick-name">{label}{default.then_some(" · default")}</span>
                                            <span class="pick-note">{names}</span>
                                        </button>
                                    }
                                })
                                .collect_view()
                                .into_any(),
                            _ => view! { <p class="pick-empty">"Loading presets…"</p> }.into_any(),
                        },
                    }}
                </div>
            })}
        </div>
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
            avatar: member.avatar.clone(),
            guests: 0,
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
            let url = api::path::member_search(&server, text.trim());
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
                            avatar: member.avatar,
                            guests: 0,
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
                    let url = api::path::member_signup(&server, &event, &member, &option);
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
                        view! {
                            <button type="button" class="pick-row" role="option"
                                aria-selected=if on { "true" } else { "false" }
                                on:click=move |_| toggle(id.clone())>
                                <super::Avatar name=person.name.clone() color=person.color.clone() avatar=person.avatar.clone()/>
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

    super::close_on_outside_press(root, move || panel.picking.with_untracked(Option::is_some), move || panel.picking.set(None));
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
            type_id: format!("type-{id}"),
            name: format!("Option {id}"),
            emoji: String::new(),
            capacity: None,
            role_id: None,
            role: None,
            attending: true,
            max_guests: 0,
            my_guests: 0,
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
            type_id: "type".into(),
            name: "x".into(),
            emoji: String::new(),
            attending: true,
            capacity: None,
            max_guests: 0,
            role_id: None,
        };
        let list = numbered(vec![named(None), named(Some("new-1")), named(None)]);
        let ids: Vec<_> = list.iter().map(|opt| opt.id.clone().unwrap()).collect();
        assert_eq!(ids, ["new-2", "new-1", "new-3"]);
    }
}
