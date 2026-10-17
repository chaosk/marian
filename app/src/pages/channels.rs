use leptos::prelude::*;

use crate::format::clock_of;
use crate::islands::JsonForm;
use crate::{api, base, load, param};

#[component]
pub fn Channels() -> impl IntoView {
    let server = param("server");
    let data = async move {
        let here = api::server(&server).await?;
        let channels = api::channels(&server).await?;
        Ok((here, channels))
    };
    load(data, |(server, channels)| {
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
        view! {
            <div class="page-head">
                <div>
                    <h1>"Channels"</h1>
                    <p class="lede">"An event channel is a Discord channel Marian creates for events. With purge on, posting or deleting an event removes other messages, and pinned messages stay. Other Discord channels from this server are listed as text channels."</p>
                </div>
            </div>
            {table}
            <JsonForm method="POST".into() url=format!("/api{}/channels", base()) class="panel channel-create".into()>
                <label class="field">"Name "<input name="name" placeholder="race-night" required/></label>
                <button class="primary" type="submit">"Create event channel"</button>
            </JsonForm>
        }
    })
}
