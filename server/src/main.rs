use axum::Router;
use leptos::prelude::*;
use leptos_axum::{generate_route_list, LeptosRoutes};

fn main() {
    let result = marian::serve::run(|web| {
        let options = leptos::config::get_configuration(None)
            .expect("Leptos settings")
            .leptos_options;
        let routes = generate_route_list(marian_app::App);
        let pkg = format!("{}/{}", options.site_root, options.site_pkg_dir);
        Router::new()
            .nest_service(&format!("/{}", options.site_pkg_dir), marian::serve::site_files(pkg))
            .leptos_routes_with_context(
                &options,
                routes,
                move || provide_context(web.clone()),
                {
                    let options = options.clone();
                    move || marian_app::shell(options.clone())
                },
            )
            .fallback(leptos_axum::file_and_error_handler(marian_app::shell))
            .with_state(options)
    });
    if let Err(err) = result {
        eprintln!("Marian stopped: {err}");
        std::process::exit(1);
    }
}
