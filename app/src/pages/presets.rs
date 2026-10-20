use leptos::prelude::*;

use crate::widgets::{ActionButton, JsonForm};
use crate::cache::{self, when, Ready};
use crate::{base, head, A};

#[component]
pub fn Presets() -> impl IntoView {
    let presets = cache::listen(cache::presets_scope(), cache::server_key);
    let library = cache::listen(cache::library_scope(), cache::server_key);
    when(move || (presets.get(), library.get()).ready(), |(presets, library)| {
        let base = base();
        let cards = if presets.is_empty() {
            view! { <section class="empty"><h2>"No presets yet."</h2><p>"Add one from the library names, separated by commas."</p></section> }.into_any()
        } else {
            presets
                .iter()
                .cloned()
                .into_iter()
                .map(|preset| {
                    let summary = preset
                        .options
                        .iter()
                        .map(|opt| opt.name.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    view! {
                        <div class="card">
                            <h3>{preset.name}</h3>
                            <p class="meta">{summary}</p>
                            <div class="actions">
                                <A href=format!("{base}/events/new?preset={}", preset.id) attr:class="ghost">"Use on a new event"</A>
                                <ActionButton method="DELETE" url=format!("/api{base}/presets/{}", preset.id)
                                    label="Remove" class="danger"/>
                            </div>
                        </div>
                    }
                })
                .collect_view()
                .into_any()
        };
        let library_list = if library.is_empty() {
            view! { <p class="muted">"Empty."</p> }.into_any()
        } else {
            library
                .into_iter()
                .map(|item| view! { <p>{item.emoji}" "{item.name}</p> })
                .collect_view()
                .into_any()
        };
        view! {
            {head("Presets")}
            <div class="split">
                <section class="stack">
                    {cards}
                    <section class="panel">
                        <h3>"Option library"</h3>
                        {library_list}
                        <JsonForm method="POST" url=format!("/api{base}/library") class="grid" done="Added.">
                            <input name="emoji" placeholder="🛡️"/>
                            <input name="name" placeholder="Tank"/>
                            <button class="ghost" type="submit">"Add option"</button>
                        </JsonForm>
                    </section>
                </section>
                <JsonForm method="POST" url=format!("/api{base}/presets") class="panel" done="Saved the preset.">
                    <h3>"New preset"</h3>
                    <label class="field">"Name "<input name="name" required/></label>
                    <label class="field">
                        "Options, comma separated "
                        <input name="options" data-kind="csv" placeholder="Tank, Healer, DPS, Bench"/>
                    </label>
                    <label class="checks">
                        <input type="checkbox" name="multiple_signups"/>
                        " "<span>"People may pick more than one"</span>
                    </label>
                    <button class="primary" type="submit">"Save preset"</button>
                </JsonForm>
            </div>
        }
    })
}
