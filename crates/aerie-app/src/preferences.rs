//! Preferences: appearance, starting at login, and diagnostics.

use std::path::PathBuf;

use adw::prelude::*;
use gtk::gio;
use serde::{Deserialize, Serialize};

use crate::APP_ID;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    /// Follow the system; dark when the system has no preference, as the brand prefers.
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub appearance: Appearance,
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_default()
}

fn settings_path() -> PathBuf {
    config_dir().join("aerie").join("settings.json")
}

fn autostart_path() -> PathBuf {
    config_dir()
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(settings_path())
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = settings_path();
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::write(
                    &path,
                    serde_json::to_string_pretty(self).expect("serializes"),
                )
            });
        if let Err(err) = result {
            tracing::warn!(%err, "couldn't save preferences");
        }
    }
}

pub fn apply_appearance(appearance: Appearance) {
    adw::StyleManager::default().set_color_scheme(match appearance {
        Appearance::System => adw::ColorScheme::PreferDark,
        Appearance::Light => adw::ColorScheme::ForceLight,
        Appearance::Dark => adw::ColorScheme::ForceDark,
    });
}

pub fn starts_at_login() -> bool {
    autostart_path().exists()
}

/// Adds or removes the login entry. Aerie then starts hidden and keeps syncing.
fn set_start_at_login(enabled: bool) -> std::io::Result<()> {
    let path = autostart_path();
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!(
            "[Desktop Entry]\nType=Application\nName=Aerie\nComment=Keep Google Drive folders in sync\n\
             Exec=aerie --background\nIcon={APP_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
        ),
    )
}

pub fn show(parent: &impl IsA<gtk::Widget>) {
    let settings = Settings::load();

    let appearance = adw::ComboRow::builder()
        .title("Appearance")
        .model(&gtk::StringList::new(&["Follow System", "Light", "Dark"]))
        .selected(match settings.appearance {
            Appearance::System => 0,
            Appearance::Light => 1,
            Appearance::Dark => 2,
        })
        .build();
    appearance.connect_selected_notify(|row| {
        let appearance = match row.selected() {
            1 => Appearance::Light,
            2 => Appearance::Dark,
            _ => Appearance::System,
        };
        apply_appearance(appearance);
        let mut settings = Settings::load();
        settings.appearance = appearance;
        settings.save();
    });
    let look = adw::PreferencesGroup::builder().title("Appearance").build();
    look.add(&appearance);

    let login = adw::SwitchRow::builder()
        .title("Start at login")
        .subtitle("Aerie starts in the background and keeps your folders in sync.")
        .active(starts_at_login())
        .build();
    login.connect_active_notify(|row| {
        if let Err(err) = set_start_at_login(row.is_active()) {
            tracing::warn!(%err, "couldn't change the login entry");
        }
    });
    let general = adw::PreferencesGroup::builder().title("General").build();
    general.add(&login);

    let logs = adw::ActionRow::builder()
        .title("Logs")
        .subtitle(
            "Kept on this computer for a week. They never include file names, file \
             contents or your sign-in.",
        )
        .build();
    let open_logs = gtk::Button::builder()
        .label("Open Folder")
        .valign(gtk::Align::Center)
        .build();
    open_logs.connect_clicked(|button| {
        let dir = crate::diagnostics::log_dir();
        let file = gio::File::for_path(dir);
        gtk::FileLauncher::new(Some(&file)).launch(
            button.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            |_| {},
        );
    });
    logs.add_suffix(&open_logs);
    let diagnostics = adw::PreferencesGroup::builder()
        .title("Diagnostics")
        .build();
    diagnostics.add(&logs);

    let page = adw::PreferencesPage::new();
    page.add(&look);
    page.add(&general);
    page.add(&diagnostics);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.present(Some(parent));
}
