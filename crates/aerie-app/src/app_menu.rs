//! The primary menu: keyboard shortcuts, about and quit.

use adw::prelude::*;
use gtk::{gio, glib};

use crate::APP_ID;

/// Every shortcut the app defines, grouped for the shortcuts dialog.
const SHORTCUTS: &[(&str, &[(&str, &str)])] = &[
    (
        "Files",
        &[
            ("Search My Drive", "<Control>k"),
            ("New folder", "<Control><Shift>n"),
            ("Rename", "F2"),
            ("Move to…", "<Control>m"),
            ("Move to trash", "Delete"),
            ("Upload files", "<Control>u"),
            ("Download", "<Control>d"),
            ("Open the selected folder", "Return"),
        ],
    ),
    (
        "General",
        &[
            ("Preferences", "<Control>comma"),
            ("Keyboard shortcuts", "<Control>question"),
            ("Quit", "<Control>q"),
        ],
    ),
];

pub fn install(app: &adw::Application) {
    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    shortcuts.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| show_shortcuts(&app)
    ));
    let preferences = gio::SimpleAction::new("preferences", None);
    preferences.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| {
            if let Some(window) = app.active_window() {
                crate::preferences::show(&window);
            }
        }
    ));
    app.add_action(&preferences);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);
    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| show_about(&app)
    ));
    let quit = gio::SimpleAction::new("quit", None);
    quit.connect_activate(glib::clone!(
        #[weak]
        app,
        move |_, _| app.quit()
    ));
    app.add_action(&shortcuts);
    app.add_action(&about);
    app.add_action(&quit);
    app.set_accels_for_action("app.shortcuts", &["<Control>question"]);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
}

pub fn button() -> gtk::MenuButton {
    let menu = gio::Menu::new();
    let account = gio::Menu::new();
    account.append(Some("Check for Changes"), Some("win.refresh"));
    account.append(Some("Sign Out"), Some("win.sign-out"));
    menu.append_section(None, &account);
    menu.append(Some("Preferences"), Some("app.preferences"));
    menu.append(Some("Keyboard Shortcuts"), Some("app.shortcuts"));
    menu.append(Some("About Aerie"), Some("app.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    gtk::MenuButton::builder()
        .icon_name("aerie-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&menu)
        .primary(true)
        .build()
}

fn show_shortcuts(app: &adw::Application) {
    let page = adw::PreferencesPage::new();
    for (title, items) in SHORTCUTS {
        let group = adw::PreferencesGroup::builder().title(*title).build();
        for (label, accel) in *items {
            let row = adw::ActionRow::builder().title(*label).build();
            row.add_suffix(&gtk::ShortcutLabel::new(accel));
            group.add(&row);
        }
        page.add(&group);
    }
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&page));
    let dialog = adw::Dialog::builder()
        .title("Keyboard Shortcuts")
        .content_width(420)
        .content_height(420)
        .child(&view)
        .build();
    dialog.present(app.active_window().as_ref());
}

fn show_about(app: &adw::Application) {
    let about = adw::AboutDialog::builder()
        .application_name("Aerie")
        .application_icon(APP_ID)
        .developer_name("Last Refuge")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Your cloud, at home on Linux.")
        .website("https://github.com/last-refuge/aerie")
        .issue_url("https://github.com/last-refuge/aerie/issues")
        .license_type(gtk::License::MitX11)
        .build();
    about.present(app.active_window().as_ref());
}
