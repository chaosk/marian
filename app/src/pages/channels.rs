use leptos::prelude::*;
use marian_types as dto;

use crate::cache::{self, Ready, when};
use crate::format::clock_of;
use crate::widgets::{JsonForm, mention_search, role_names};
use crate::{api, client, title};
use marian_types::ChannelKind;

#[component]
pub fn Channels() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let channels = cache::listen(cache::channels_scope(), cache::server_key);
    let roles = cache::listen(cache::roles_scope(), cache::server_key);
    // The one channel whose mention editor is open: opening another closes it.
    let editing = RwSignal::new(None::<String>);
    when(
        move || (server.get(), channels.get(), roles.get()).ready(),
        move |(server, channels, roles)| {
            let clock = clock_of(&server);
            let table = (!channels.is_empty()).then(|| {
            let rows = channels
                .into_iter()
                .map(|channel| {
                    let kind = match channel.kind {
                        ChannelKind::Event => "Event",
                        ChannelKind::Text => "Text",
                    };
                    let purged = channel
                        .purged_at
                        .map(|dt| clock.when(dt))
                        .unwrap_or_default();
                    let name = channel.name.clone();
                    let events = channel.events;
                    view! {
                        <tr>
                            <td>"#"{name}</td>
                            <td class="fit">{kind}</td>
                            <td class="fit">{events}</td>
                            <td><ChannelMentions channel roles=roles.clone() editing/></td>
                            <td class="fit">{purged}</td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet compact spills">
                    <table>
                        <thead>
                            <tr><th>"Channel"</th><th class="fit">"Kind"</th><th class="fit">"Events"</th><th>"New events mention"</th><th class="fit">"Cleared"</th></tr>
                        </thead>
                        <tbody>{rows}</tbody>
                    </table>
                </div>
            }
        });
            title("Channels");
            view! {
                <div class="page-head">
                    <div>
                        <h1>"Channels"</h1>
                        <p class="lede">"An event channel is a Discord channel Marian creates for events. With purge on, posting or deleting an event removes other messages, and pinned messages stay. Other Discord channels from this server are listed as text channels."</p>
                        <p class="lede">"Who a new event mentions starts from its channel. A channel that follows the server uses the default on the Settings page; one with its own setting can mention other roles, or nobody. The event's creator can still change it."</p>
                    </div>
                </div>
                {table}
                <JsonForm method="POST" url=crate::api::path::channels(&cache::server_now()) class="panel channel-create" done="Created the channel.">
                    <label class="field">"Name "<input name="name" placeholder="race-night" required/></label>
                    <button class="primary" type="submit">"Create event channel"</button>
                </JsonForm>
            }
        },
    )
}

/// "@everyone, Raiders", or "Nobody".
fn mention_line(notify: &dto::Notify, roles: &[dto::Role]) -> String {
    let mut names = role_names(roles, &notify.role_ids);
    if notify.everyone {
        names.insert(0, "@everyone".into());
    }
    if names.is_empty() {
        "Nobody".into()
    } else {
        names.join(", ")
    }
}

/// Who a new event in the channel mentions, and a popover to have it follow the
/// server or name its own: some roles, @everyone, or nobody.
#[component]
fn ChannelMentions(
    channel: dto::Channel,
    roles: Vec<dto::Role>,
    /// The channel whose editor is open, shared by the table's rows.
    editing: RwSignal<Option<String>>,
) -> impl IntoView {
    let face = match &channel.own_notify {
        Some(own) => mention_line(own, &roles),
        None => format!("{} (server default)", mention_line(&channel.notify, &roles)),
    };
    let start = channel
        .own_notify
        .clone()
        .unwrap_or_else(|| channel.notify.clone());
    let follow = RwSignal::new(channel.own_notify.is_none());
    let everyone = RwSignal::new(start.everyone);
    let picked = RwSignal::new(start.role_ids.clone());
    let problem = RwSignal::new(String::new());
    let roles = StoredValue::new(roles);
    let start = StoredValue::new((channel.own_notify.is_none(), start));
    let url = StoredValue::new(api::path::channel_notify(&cache::server_now(), &channel.id));
    let server = StoredValue::new(cache::server_now());
    let id = StoredValue::new(channel.id.clone());
    let open =
        Memo::new(move |_| editing.with(|now| now.as_deref() == Some(id.get_value().as_str())));
    let close = move || {
        if open.get_untracked() {
            editing.set(None);
        }
    };
    // What the editor shows on opening: what's saved, not what a closed one left.
    let reset = move || {
        let (follows, notify) = start.get_value();
        follow.set(follows);
        everyone.set(notify.everyone);
        picked.set(notify.role_ids);
        problem.set(String::new());
    };
    let cancel = move || {
        reset();
        close();
    };
    let saving = client::action(move |body: &dto::ChannelNotify| {
        let body = body.clone();
        async move {
            match api::send::<dto::Done, _>("PUT", &url.get_value(), Some(&body)).await {
                Ok(_) => {
                    close();
                    cache::refresh(&server.get_value());
                }
                Err(failure) => problem.set(failure.message),
            }
        }
    });
    let save = move || {
        let notify = (!follow.get_untracked()).then(|| dto::Notify {
            everyone: everyone.get_untracked(),
            role_ids: picked.get_untracked(),
        });
        saving.dispatch(dto::ChannelNotify { notify });
    };
    let name = format!("follow-{}", channel.id);
    view! {
        <div class="inline-anchor">
            <button type="button" class="inline-edit" title="Change who a new event here mentions"
                aria-expanded=move || if open.get() { "true" } else { "false" }
                on:click=move |_| {
                    if open.get_untracked() {
                        cancel();
                    } else {
                        reset();
                        editing.set(Some(id.get_value()));
                    }
                }>
                <span class="face">{face}</span>
            </button>
            {move || open.get().then(|| {
                let name = name.clone();
                view! {
                    <form class="popover" on:submit=move |ev| { ev.prevent_default(); save(); }
                        on:keydown=move |ev| if ev.key() == "Escape" { cancel(); }>
                        <p class="popover-title">"New events mention"</p>
                        <label class="check-row">
                            <input type="radio" name=name.clone() prop:checked=move || follow.get()
                                on:change=move |_| follow.set(true)/>
                            <span>"The server's default"</span>
                        </label>
                        <label class="check-row">
                            <input type="radio" name=name prop:checked=move || !follow.get()
                                on:change=move |_| follow.set(false)/>
                            <span>"This channel's own"</span>
                        </label>
                        {move || (!follow.get()).then(|| view! {
                            <p class="popover-note left">"Pick nothing to mention nobody, whatever the server's default."</p>
                            {mention_search(roles, picked, everyone)}
                        })}
                        {move || {
                            let text = problem.get();
                            (!text.is_empty()).then(|| view! { <p class="error inline-error" role="alert">{text}</p> })
                        }}
                        <div class="inline-actions">
                            <button type="submit" class="primary" disabled=move || saving.pending().get()>"Save"</button>
                            <button type="button" class="ghost" on:click=move |_| cancel()>"Cancel"</button>
                        </div>
                    </form>
                }
            })}
        </div>
    }
}
