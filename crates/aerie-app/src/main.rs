//! Aerie desktop application.

mod browser;
mod fixtures;

use adw::prelude::*;
use gtk::{gdk, glib};

const APP_ID: &str = "io.github.last_refuge.Aerie";

fn main() -> glib::ExitCode {
    glib::set_application_name("Aerie");
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_startup(|app| {
        // The brand is dark-first; an explicit system preference still wins.
        adw::StyleManager::default().set_color_scheme(adw::ColorScheme::PreferDark);
        load_css();
        app.set_accels_for_action("win.search", &["<Control>k", "<Control>f"]);
        app.set_accels_for_action("win.new-folder", &["<Control><Shift>n"]);
    });
    app.connect_activate(browser::build_window);
    app.run()
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
