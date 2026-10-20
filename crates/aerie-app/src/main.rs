//! Aerie desktop application.

mod actions;
mod app_menu;
mod browser;
mod catalog;
mod diagnostics;
mod engine;
mod fixtures;
mod preferences;
mod sync_page;
mod transfers;
mod transfers_page;

use adw::prelude::*;
use gtk::{gdk, glib};

pub(crate) const APP_ID: &str = "ai.lastrefuge.Aerie";

fn main() -> glib::ExitCode {
    let _logs = diagnostics::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Aerie");
    glib::set_application_name("Aerie");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.add_main_option(
        "background",
        glib::Char::from(b'b'),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Start without a window and keep syncing",
        None,
    );
    let background = std::rc::Rc::new(std::cell::Cell::new(false));
    app.connect_handle_local_options(glib::clone!(
        #[strong]
        background,
        move |_, options| {
            background.set(options.contains("background"));
            // Continue with normal startup.
            std::ops::ControlFlow::Continue(())
        }
    ));
    app.connect_startup(|app| {
        // Dark-first unless the system or the user prefers otherwise.
        preferences::apply_appearance(preferences::Settings::load().appearance);
        load_css();
        add_dev_icons();
        app_menu::install(app);
        app.set_accels_for_action("win.search", &["<Control>k", "<Control>f"]);
        app.set_accels_for_action("win.new-folder", &["<Control><Shift>n"]);
        app.set_accels_for_action("win.rename", &["F2"]);
        app.set_accels_for_action("win.move", &["<Control>m"]);
        app.set_accels_for_action("win.trash", &["Delete"]);
        app.set_accels_for_action("win.upload", &["<Control>u"]);
        app.set_accels_for_action("win.download", &["<Control>d"]);
    });
    app.connect_activate(move |app| browser::build_window(app, !background.replace(false)));
    app.run()
}

/// Lets a build run from the source tree find its own icons before they're installed.
fn add_dev_icons() {
    if cfg!(debug_assertions) {
        let display = gdk::Display::default().expect("a display is available");
        gtk::IconTheme::for_display(&display)
            .add_search_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/icons"));
    }
}

/// Loads the base styles, then layers the dark and high-contrast tokens to follow the
/// style manager. GTK media queries don't track libadwaita's color scheme for app providers.
fn load_css() {
    let display = gdk::Display::default().expect("a display is available");
    gtk::style_context_add_provider_for_display(
        &display,
        &css_provider(include_str!("style.css")),
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let dark = css_provider(include_str!("style-dark.css"));
    let high_contrast = css_provider(include_str!("style-hc.css"));
    let update = move |manager: &adw::StyleManager| {
        for (provider, active) in [
            (&dark, manager.is_dark()),
            (&high_contrast, manager.is_high_contrast()),
        ] {
            gtk::style_context_remove_provider_for_display(&display, provider);
            if active {
                gtk::style_context_add_provider_for_display(
                    &display,
                    provider,
                    gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
                );
            }
        }
    };

    let manager = adw::StyleManager::default();
    update(&manager);
    manager.connect_dark_notify(update.clone());
    manager.connect_high_contrast_notify(update);
}

fn css_provider(css: &str) -> gtk::CssProvider {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(css);
    provider
}
