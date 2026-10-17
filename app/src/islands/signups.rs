use leptos::prelude::*;
use marian_types as dto;

use crate::format::spots;
use crate::{api, client};

/// Who is coming, the signed-in person's own buttons, and an organizer's controls.
/// It fetches again whenever the event changes.
#[island]
pub fn SignupPanel(
    server: String,
    event: String,
    signups: dto::Signups,
    /// People an organizer can add; empty when they can't.
    #[prop(optional)]
    members: Option<Vec<dto::Member>>,
) -> impl IntoView {
    let state = RwSignal::new(signups);
    let notice = RwSignal::new(String::new());
    let problem = RwSignal::new(String::new());
    let ids = StoredValue::new((server, event));

    let refresh = move || {
        let (server, event) = ids.get_value();
        client::spawn(async move {
            if let Ok(fresh) = api::signups(&server, &event).await {
                state.set(fresh);
            }
        });
    };

    #[cfg(feature = "hydrate")]
    Effect::new(move |_| {
        use wasm_bindgen::closure::Closure;
        use wasm_bindgen::JsCast;
        let (server, event) = ids.get_value();
        let url = format!("/api/servers/{server}/events/{event}/live");
        if let Ok(source) = web_sys::EventSource::new(&url) {
            let on_change = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |_| refresh());
            source.set_onmessage(Some(on_change.as_ref().unchecked_ref()));
            on_change.forget();
            on_cleanup(move || source.close());
        }
    });

    let change = move |method: &'static str, path: String| {
        let (server, event) = ids.get_value();
        let url = format!("/api/servers/{server}/events/{event}/signups/{path}");
        client::spawn(async move {
            match api::send::<dto::Done, ()>(method, &url, None).await {
                Ok(done) => {
                    notice.set(done.notice);
                    problem.set(String::new());
                }
                Err(failure) => problem.set(failure.message),
            }
            refresh();
        });
    };

    let members = members.unwrap_or_default();
    let picked_member = RwSignal::new(members.first().map(|m| m.id).unwrap_or_default());
    let picked_option = RwSignal::new(0_i64);
    let members = StoredValue::new(members);

    let cards = move || {
        let now = state.get();
        now.options
            .into_iter()
            .map(|opt| {
                let option_id = opt.id;
                let confirmed = opt
                    .confirmed
                    .into_iter()
                    .map(|person| {
                        let remove = now.can_manage.then(|| {
                            let path = format!("{}/{option_id}", person.id);
                            view! {
                                <button class="ghost" type="button"
                                    on:click=move |_| change("DELETE", path.clone())>"Remove"</button>
                            }
                        });
                        view! {
                            <div class="person">
                                <span><i class="dot" style=format!("background: {}", person.color)></i>{person.name}</span>
                                {remove}
                            </div>
                        }
                    })
                    .collect_view();
                let waiting = opt
                    .waiting
                    .into_iter()
                    .map(|person| view! { <div class="person"><span>{person.name}" · waiting"</span></div> })
                    .collect_view();
                let label = if opt.mine {
                    "Leave"
                } else if opt.waitlist_next {
                    "Join the waitlist"
                } else {
                    "Sign up"
                };
                let method = if opt.mine { "DELETE" } else { "PUT" };
                let waiting_count = (opt.waiting_count > 0).then(|| format!(" +{} waiting", opt.waiting_count));
                view! {
                    <article class=if opt.mine { "attend-card mine" } else { "attend-card" }>
                        <p class="attend-name">
                            {opt.emoji}" "<strong>{opt.name}</strong>" "
                            <span class="muted">{spots(opt.confirmed_count, opt.capacity)}{waiting_count}</span>
                        </p>
                        {confirmed}
                        {waiting}
                        {opt.role.map(|role| view! { <p class="muted">{role}" only"</p> })}
                        <button type="button" class=if opt.mine { "ghost" } else { "primary" }
                            disabled=!opt.open title=opt.reason
                            on:click=move |_| change(method, format!("me/{option_id}"))>{label}</button>
                    </article>
                }
            })
            .collect_view()
    };

    let notes = move || {
        let notes = state.get().notes;
        (!notes.is_empty()).then(|| view! {
            <ul class="signup-notes">{notes.into_iter().map(|note| view! { <li>{note}</li> }).collect_view()}</ul>
        })
    };

    let add = move || {
        let now = state.get();
        let people = members.get_value();
        (now.can_manage && !people.is_empty()).then(|| {
            if picked_option.get_untracked() == 0 {
                picked_option.set(now.options.first().map(|o| o.id).unwrap_or_default());
            }
            let people = people
                .into_iter()
                .map(|person| view! { <option value=person.id>{person.name}</option> })
                .collect_view();
            let choices = now
                .options
                .into_iter()
                .map(|opt| view! { <option value=opt.id>{opt.name}</option> })
                .collect_view();
            view! {
                <div class="grid add-response">
                    <label class="field">"Person"
                        <select on:change=move |ev| picked_member.set(event_target_value(&ev).parse().unwrap_or_default())>{people}</select>
                    </label>
                    <label class="field">"Option"
                        <select on:change=move |ev| picked_option.set(event_target_value(&ev).parse().unwrap_or_default())>{choices}</select>
                    </label>
                    <button class="primary" type="button"
                        on:click=move |_| change("PUT", format!("{}/{}", picked_member.get_untracked(), picked_option.get_untracked()))>
                        "Add a response"
                    </button>
                </div>
            }
        })
    };

    view! {
        <section>
            <div class="section-head"><h2>"Attendance"</h2></div>
            {move || {
                let text = notice.get();
                (!text.is_empty()).then(|| view! { <p class="notice">{text}</p> })
            }}
            {move || {
                let text = problem.get();
                (!text.is_empty()).then(|| view! { <p class="error">{text}</p> })
            }}
            <div class="attend-grid">{cards}</div>
            {notes}
            {add}
        </section>
    }
}
