use leptos::prelude::*;

use crate::format::clock_of;
use crate::widgets::JsonForm;
use crate::cache::{self, when, Ready};
use crate::title;

#[component]
pub fn Channels() -> impl IntoView {
    let server = cache::listen(cache::server_scope(), cache::server_key);
    let channels = cache::listen(cache::channels_scope(), cache::server_key);
    when(move || (server.get(), channels.get()).ready(), move |(server, channels)| {
        let clock = clock_of(&server);
        let table = (!channels.is_empty()).then(|| {
            let rows = channels
                .into_iter()
                .map(|channel| {
                    let kind = if channel.kind == "event" { "Event" } else { "Text" };
                    let purged = channel
                        .purged_at
                        .map(|dt| clock.when(dt))
                        .unwrap_or_default();
                    view! {
                        <tr>
                            <td>"#"{channel.name}</td>
                            <td class="fit">{kind}</td>
                            <td class="fit">{channel.events}</td>
                            <td class="fit">{purged}</td>
                        </tr>
                    }
                })
                .collect_view();
            view! {
                <div class="sheet compact">
                    <table>
                        <thead>
                            <tr><th>"Channel"</th><th class="fit">"Kind"</th><th class="fit">"Events"</th><th class="fit">"Cleared"</th></tr>
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
                </div>
            </div>
            {table}
            <JsonForm method="POST" url=crate::api::path::channels(&cache::server_now()) class="panel channel-create" done="Created the channel.">
                <label class="field">"Name "<input name="name" placeholder="race-night" required/></label>
                <button class="primary" type="submit">"Create event channel"</button>
            </JsonForm>
        }
    })
}
