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

/// Development builds register under their own ID, so running one never activates or
/// disturbs an installed Aerie.
const INSTANCE_ID: &str = if cfg!(debug_assertions) {
    "ai.lastrefuge.Aerie.Devel"
} else {
    APP_ID
};

fn main() -> glib::ExitCode {
    let _logs = diagnostics::init();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting Aerie");
    glib::set_application_name("Aerie");
    let app = adw::Application::builder()
        .application_id(INSTANCE_ID)
        .build();
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
        load_fonts();
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
/// Where Aerie's bundled assets live: the source tree in development builds, otherwise
/// the install prefix (`AERIE_DATA_DIR` at build time, `/usr/share/aerie` by default).
fn data_dir() -> std::path::PathBuf {
    if cfg!(debug_assertions) {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets"))
    } else {
        std::path::PathBuf::from(option_env!("AERIE_DATA_DIR").unwrap_or("/usr/share/aerie"))
    }
}

/// Development builds find their icons in the source tree; installed ones use hicolor.
fn add_dev_icons() {
    if cfg!(debug_assertions) {
        let display = gdk::Display::default().expect("a display is available");
        gtk::IconTheme::for_display(&display).add_search_path(data_dir().join("icons"));
    }
}

/// Makes the bundled Inter and JetBrains Mono available to this app only, so the brand
/// typefaces render without being installed system-wide.
fn load_fonts() {
    use pango::prelude::FontMapExt;
    let font_map = gtk::Label::new(None).pango_context().font_map();
    let Some(font_map) = font_map else { return };
    let Ok(entries) = std::fs::read_dir(data_dir().join("fonts")) else {
        tracing::warn!("bundled fonts not found; using system fonts");
        return;
    };
    for path in entries.flatten().map(|entry| entry.path()) {
        if path.extension().is_some_and(|ext| ext == "ttf")
            && let Err(err) = font_map.add_font_file(&path)
        {
            tracing::warn!(%err, "couldn't load a bundled font");
        }
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
