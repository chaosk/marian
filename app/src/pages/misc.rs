use leptos::prelude::*;
use marian_types as dto;

use crate::cache::{self, when};
use crate::client;
use crate::widgets::Flash;
use crate::shell::Brand;
use crate::{api, query, status, title, A};

const GUIDE: &str = include_str!("../guide.html");

/// The front page: the servers to open for someone signed in, and the way
/// in for everyone else.
#[component]
pub fn Front() -> impl IntoView {
    let next = query("next");
    let me = cache::listen(cache::me_scope(), || ());
    // Not signed in is not a failure here; it is the landing page.
    let gather = move || me.get().map(|answer| Ok::<_, api::Failure>(answer.ok()));
    when(gather, move |me| match me {
        Some(me) => server_picker(me).into_any(),
        None => landing(next.clone()).into_any(),
    })
}

fn landing(next: String) -> impl IntoView {
    let ready = api::site().oauth_ready;
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
}

/// Every server the person signed into. When this browser opened one last and
/// it is still theirs to open, the page goes straight there instead.
fn server_picker(me: dto::Me) -> impl IntoView {
    // Only the browser knows the last server, so the list waits for it to look:
    // shown first, it would flash on the way to the server.
    let decided = RwSignal::new(false);
    let invite = api::site().invite_url;
    let empty = me.servers.is_empty();
    let servers = StoredValue::new(me.servers);
    let navigate = leptos_router::hooks::use_navigate();
    Effect::new(move |_| {
        let remembered = client::last_server();
        let open = remembered
            .as_ref()
            .filter(|id| servers.with_value(|list| list.iter().any(|server| &server.id == *id)));
        match open {
            Some(id) => navigate(
                &format!("/servers/{id}"),
                leptos_router::NavigateOptions { replace: true, ..Default::default() },
            ),
            None => {
                // Gone from Marian, or no longer theirs: no reason to keep it.
                if remembered.is_some() {
                    client::forget_server();
                }
                decided.set(true);
            }
        }
    });
    title("");
    let list = move || {
        servers
            .get_value()
            .into_iter()
            .map(|server| {
                let initial = server.name.chars().next().unwrap_or('?').to_uppercase().to_string();
                view! {
                    <li>
                        <A href=format!("/servers/{}", server.id) attr:class="server-pick">
                            <span class="switcher-mark">{initial}</span>
                            <span class="server-pick-name">{server.name}</span>
                        </A>
                    </li>
                }
            })
            .collect_view()
    };
    view! {
        <div class="gate">
            <main class="gate-card" style:visibility=move || if decided.get() { "visible" } else { "hidden" }>
                <Brand/>
                <Flash/>
                <h1>"Pick a server."</h1>
                <p class="lede">{format!("Signed in as {}.", me.name)}</p>
                {if empty {
                    view! { <p class="notice">"Marian isn't in any server you signed into yet."</p> }.into_any()
                } else {
                    view! { <ul class="server-picks">{list}</ul> }.into_any()
                }}
                <div class="gate-actions">
                    {(!invite.is_empty()).then(|| view! { <a class="ghost" href=invite.clone()>"Add Marian to a server"</a> })}
                    <form method="post" action="/logout"><button type="submit" class="ghost">"Sign out"</button></form>
                </div>
            </main>
        </div>
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

/// An address no page answers: a full page of its own.
#[component]
pub fn NotFound() -> impl IntoView {
    status(404);
    title("Missing");
    crate::shell::lost("That page isn't here.", "The address may be mistyped, or the page has moved.", false)
}

/// Something a page asked for that isn't there, said where the page would have shown it.
#[component]
pub fn MissingHere() -> impl IntoView {
    status(404);
    title("Missing");
    view! {
        <section class="empty">
            <h1>"That isn't here."</h1>
            <p>"It may have been deleted, or the link is old."</p>
        </section>
    }
}
