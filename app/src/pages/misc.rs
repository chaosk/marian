use leptos::prelude::*;
use crate::widgets::Flash;
use crate::shell::Brand;
use crate::{api, query, redirect, status, title, A};

const GUIDE: &str = include_str!("../guide.html");

#[component]
pub fn Front() -> impl IntoView {
    let next = query("next");
    match api::front(&next) {
        api::Front::Home(server) => {
            redirect(&format!("/servers/{server}"));
            view! { <p class="muted">"Opening your server…"</p> }.into_any()
        }
        api::Front::Landing { ready, next } => {
            let notice = api::take_notice();
            let invite = api::site().invite_url;
            let sign_in = if next.is_empty() {
                "/auth/discord".to_string()
            } else {
                format!("/auth/discord?next={}", api::encode(&next))
            };
            title("");
            view! {
                <div class="gate">
                    <main class="gate-card">
                        <Brand/>
                        <Flash/>
                        <h1>"Events for your server."</h1>
                        <p class="lede">"The only human-made thing in here is the logo."</p>
                        {(!notice.is_empty()).then(|| view! { <p class="notice">{notice.clone()}</p> })}
                        {(!ready).then(|| view! { <p class="notice">"Sign-in is not configured yet."</p> })}
                        {(ready || !invite.is_empty()).then(move || view! {
                            <div class="gate-actions">
                                {ready.then(|| view! { <a class="discord" href=sign_in.clone() rel="external">"Sign in with Discord"</a> })}
                                {(!invite.is_empty()).then(|| view! { <a class="ghost" href=invite.clone()>"Invite Marian"</a> })}
                            </div>
                        })}
                    </main>
                </div>
            }
            .into_any()
        }
    }
}

#[component]
pub fn Help() -> impl IntoView {
    title("Commands");
    view! {
        <div class="doc">
            <main>
                <Brand/>
                <h1>"Commands"</h1>
                <p class="lede">
                    "Each Discord command asks for its own fields. "
                    <code>"/event"</code>
                    " is the one that starts a conversation."
                </p>
                <article class="guide" inner_html=GUIDE></article>
                <p class="legal"><A href="/">"Back to the schedule"</A></p>
            </main>
        </div>
    }
}

#[component]
pub fn NotFound() -> impl IntoView {
    status(404);
    title("Missing");
    view! {
        <section class="empty">
            <h1>"That page isn't here."</h1>
            <p><A href="/">"Back to the schedule"</A></p>
        </section>
    }
}
