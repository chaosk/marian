use leptos::prelude::*;

use crate::islands::{ActionButton, JsonForm};
use crate::{api, base, head, load, param};

#[component]
pub fn Presets() -> impl IntoView {
    let server = param("server");
    let data = async move {
        let presets = api::presets(&server).await?;
        let library = api::library(&server).await?;
        Ok((presets, library))
    };
    load(data, |(presets, library)| {
        let base = base();
        let cards = if presets.is_empty() {
            view! { <section class="empty"><h2>"No presets yet."</h2><p>"Add one from the library names, separated by commas."</p></section> }.into_any()
        } else {
            presets
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
                                <a class="ghost" href=format!("{base}/events/new?preset={}", preset.id)>"Use on a new event"</a>
                                <ActionButton method="DELETE".into() url=format!("/api{base}/presets/{}", preset.id)
                                    label="Remove".into() class="danger".into()/>
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
                        <JsonForm method="POST".into() url=format!("/api{base}/library") class="grid".into()>
                            <input name="emoji" placeholder="🛡️"/>
                            <input name="name" placeholder="Tank"/>
                            <button class="ghost" type="submit">"Add option"</button>
                        </JsonForm>
                    </section>
                </section>
                <JsonForm method="POST".into() url=format!("/api{base}/presets") class="panel".into()>
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
