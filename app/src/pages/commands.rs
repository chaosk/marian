use leptos::prelude::*;

use crate::islands::CommandDesk;
use crate::{api, head, load, param};

const GUIDE: &str = include_str!("../guide.html");

#[component]
pub fn Commands() -> impl IntoView {
    let server = param("server");
    let data = async move { api::commands(&server).await };
    load(data, |log| {
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
