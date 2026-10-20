use leptos::prelude::*;

use crate::widgets::CommandDesk;
use crate::cache::{self, ask};
use crate::{head, param};

const GUIDE: &str = include_str!("../guide.html");

#[component]
pub fn Commands() -> impl IntoView {
    ask(cache::commands_scope(), cache::server_key, |log| {
        view! {
            {head("Command desk")}
            <div class="split">
                <CommandDesk server=param("server") log/>
                <section class="panel guide">
                    <h3>"Commands"</h3>
                    <div inner_html=GUIDE></div>
                </section>
            </div>
        }
    })
}
