//! Sync setup and status: choose a local folder and Drive folders, then watch, pause or
//! stop syncing.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use aerie_core::{DriveFileId, FileEntry, compare_names};
use aerie_store::SyncSettings;
use gtk::{gio, glib};

use crate::engine::{Command, Engine, SyncState};

pub struct SyncPage {
    pub root: gtk::Widget,
    engine: Rc<Engine>,
    stack: gtk::Stack,
    // Setup.
    folder_row: adw::ActionRow,
    setup_folders: adw::PreferencesGroup,
    start: gtk::Button,
    chosen: RefCell<Option<PathBuf>>,
    // Running.
    status_row: adw::ActionRow,
    held_row: adw::ActionRow,
    local_row: adw::ActionRow,
    running_folders: adw::PreferencesGroup,
    pause: gtk::Button,
    // Shared.
    selected: RefCell<BTreeSet<DriveFileId>>,
    folder_rows: RefCell<Vec<(adw::PreferencesGroup, gtk::Widget)>>,
    paused: RefCell<bool>,
}

impl SyncPage {
    pub fn new(engine: Rc<Engine>) -> Rc<Self> {
        let stack = gtk::Stack::new();

        // Setup page.
        let intro = adw::PreferencesGroup::builder()
            .title("Sync folders to this computer")
            .description(
                "Edits, renames and deletions in synced folders go both ways. Deleted files \
                 go to the trash on the other side, never straight to permanent deletion. \
                 Choosing folders limits what Aerie syncs; Google still grants Aerie access \
                 to your whole Drive.",
            )
            .build();
        let folder_row = adw::ActionRow::builder()
            .title("Local folder")
            .subtitle("Choose an empty folder")
            .build();
        let choose = gtk::Button::builder()
            .label("Choose…")
            .valign(gtk::Align::Center)
            .build();
        folder_row.add_suffix(&choose);
        intro.add(&folder_row);
        let setup_folders = adw::PreferencesGroup::builder()
            .title("Folders to sync")
            .description("Choose folders from the top of My Drive, or whole shared drives.")
            .build();
        let start = gtk::Button::builder()
            .label("Start Syncing")
            .halign(gtk::Align::Center)
            .sensitive(false)
            .css_classes(["aerie-primary"])
            .build();
        let start_group = adw::PreferencesGroup::new();
        start_group.add(&start);
        let setup = adw::PreferencesPage::new();
        setup.add(&intro);
        setup.add(&setup_folders);
        setup.add(&start_group);
        stack.add_named(&setup, Some("setup"));

        // Running page.
        let status_row = adw::ActionRow::builder().title("Checking…").build();
        let sync_now = gtk::Button::builder()
            .label("Sync Now")
            .valign(gtk::Align::Center)
            .build();
        status_row.add_suffix(&sync_now);
        let held_row = adw::ActionRow::builder()
            .title("Deletions paused")
            .visible(false)
            .build();
        let keep = gtk::Button::builder()
            .label("Keep Files")
            .valign(gtk::Align::Center)
            .build();
        let delete = gtk::Button::builder()
            .label("Delete Them")
            .valign(gtk::Align::Center)
            .css_classes(["destructive-action"])
            .build();
        held_row.add_suffix(&keep);
        held_row.add_suffix(&delete);
        let local_row = adw::ActionRow::builder().title("Local folder").build();
        let open = gtk::Button::builder()
            .icon_name("aerie-folder-open-symbolic")
            .tooltip_text("Open in Files")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        local_row.add_suffix(&open);
        let status = adw::PreferencesGroup::new();
        status.add(&status_row);
        status.add(&held_row);
        status.add(&local_row);
        let running_folders = adw::PreferencesGroup::builder()
            .title("Synced folders")
            .description("Turning a folder off keeps its local copy and stops syncing it.")
            .build();
        let pause = gtk::Button::builder().label("Pause Syncing").build();
        let stop = gtk::Button::builder()
            .label("Stop Syncing…")
            .css_classes(["destructive-action"])
            .build();
        let controls = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        controls.append(&pause);
        controls.append(&stop);
        let controls_group = adw::PreferencesGroup::new();
        controls_group.add(&controls);
        let running = adw::PreferencesPage::new();
        running.add(&status);
        running.add(&running_folders);
        running.add(&controls_group);
        stack.add_named(&running, Some("running"));

        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Sync", ""))
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));

        let page = Rc::new(Self {
            root: view.upcast(),
            engine,
            stack,
            folder_row,
            setup_folders,
            start,
            chosen: RefCell::default(),
            status_row,
            held_row,
            local_row,
            running_folders,
            pause,
            selected: RefCell::default(),
            folder_rows: RefCell::default(),
            paused: RefCell::new(false),
        });

        choose.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| page.choose_folder()
        ));
        page.start.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| {
                let Some(local_root) = page.chosen.borrow().clone() else {
                    return;
                };
                if let Err(err) = std::fs::create_dir_all(&local_root) {
                    page.folder_row
                        .set_subtitle(&format!("Couldn’t create it: {err}"));
                    return;
                }
                let roots = page.selected.borrow().iter().cloned().collect();
                page.engine.send(Command::StartSync { local_root, roots });
            }
        ));
        sync_now.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| page.engine.send(Command::SyncNow)
        ));
        keep.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| page.engine.send(Command::KeepHeldItems)
        ));
        delete.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| page.engine.send(Command::AllowHeldDeletions)
        ));
        open.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |button| {
                let path = page.local_row.subtitle().unwrap_or_default();
                let file = gio::File::for_path(path.as_str());
                gtk::FileLauncher::new(Some(&file)).launch(
                    button.root().and_downcast_ref::<gtk::Window>(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        ));
        page.pause.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| {
                let paused = *page.paused.borrow();
                page.engine.send(if paused {
                    Command::ResumeSync
                } else {
                    Command::PauseSync
                });
            }
        ));
        stop.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |button| page.confirm_stop(button)
        ));
        page
    }

    /// Shows setup or status for the current settings, listing the top-level folders.
    pub fn show(
        self: &Rc<Self>,
        settings: Option<SyncSettings>,
        roots: Vec<DriveFileId>,
        mut folders: Vec<FileEntry>,
    ) {
        folders.retain(FileEntry::is_folder);
        folders.sort_by(|a, b| compare_names(&a.name, &b.name));
        *self.selected.borrow_mut() = roots.into_iter().collect();
        let running = settings.is_some();
        self.stack
            .set_visible_child_name(if running { "running" } else { "setup" });
        if let Some(settings) = &settings {
            self.local_row
                .set_subtitle(&settings.local_root.to_string_lossy());
            *self.paused.borrow_mut() = settings.paused;
            self.pause.set_label(if settings.paused {
                "Resume Syncing"
            } else {
                "Pause Syncing"
            });
        } else if self.chosen.borrow().is_none() {
            // Suggest ~/Aerie when it's free.
            if let Some(home) = std::env::var_os("HOME") {
                let suggested = PathBuf::from(home).join("Aerie");
                if is_empty_or_missing(&suggested) {
                    self.folder_row.set_subtitle(&suggested.to_string_lossy());
                    *self.chosen.borrow_mut() = Some(suggested);
                }
            }
        }

        let group = if running {
            &self.running_folders
        } else {
            &self.setup_folders
        };
        for (owner, row) in self.folder_rows.borrow_mut().drain(..) {
            owner.remove(&row);
        }
        if folders.is_empty() {
            let row = adw::ActionRow::builder()
                .title("No folders at the top of My Drive yet")
                .sensitive(false)
                .build();
            group.add(&row);
            self.folder_rows
                .borrow_mut()
                .push((group.clone(), row.upcast()));
        }
        for folder in folders {
            let row = adw::SwitchRow::builder()
                .title(glib::markup_escape_text(&folder.name).as_str())
                .subtitle(if folder.mime_type == aerie_drive::api::SHARED_DRIVE_MIME {
                    "Shared drive"
                } else {
                    ""
                })
                .active(self.selected.borrow().contains(&folder.id))
                .build();
            let id = folder.id.clone();
            row.connect_active_notify(glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |row| {
                    {
                        let mut selected = page.selected.borrow_mut();
                        if row.is_active() {
                            selected.insert(id.clone());
                        } else {
                            selected.remove(&id);
                        }
                    }
                    if page.stack.visible_child_name().as_deref() == Some("running") {
                        let roots = page.selected.borrow().iter().cloned().collect();
                        page.engine.send(Command::SetSyncRoots(roots));
                    }
                    page.update_start();
                }
            ));
            group.add(&row);
            self.folder_rows
                .borrow_mut()
                .push((group.clone(), row.upcast()));
        }
        self.update_start();
    }

    pub fn set_state(&self, state: &SyncState) {
        self.held_row.set_visible(false);
        let (title, subtitle) = match state {
            SyncState::Off => ("Sync is off", String::new()),
            SyncState::Paused => ("Paused", "Changes wait until you resume.".to_owned()),
            SyncState::Syncing => ("Syncing", String::new()),
            SyncState::InSync => ("Everything in sync.", String::new()),
            SyncState::Attention(message) => ("Needs attention", message.clone()),
            SyncState::HeldDeletions(count) => {
                self.held_row.set_visible(true);
                self.held_row.set_subtitle(&format!(
                    "Syncing would delete {count} items. Keep them, or delete them on the other side too."
                ));
                (
                    "Needs attention",
                    "Some deletions are waiting for you.".to_owned(),
                )
            }
        };
        self.status_row.set_title(title);
        self.status_row.set_subtitle(&subtitle);
    }

    /// Shows where a running pass is working.
    pub fn set_progress(&self, folder: Option<&str>, done: usize, total: usize) {
        self.status_row.set_title("Syncing");
        let step = format!("{} of {total}", done + 1);
        self.status_row.set_subtitle(&match folder {
            Some(folder) => format!("{folder} · {step}"),
            None => step,
        });
    }

    fn update_start(&self) {
        self.start
            .set_sensitive(self.chosen.borrow().is_some() && !self.selected.borrow().is_empty());
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose an Empty Folder")
            .accept_label("Choose")
            .modal(true)
            .build();
        dialog.select_folder(
            self.root.root().and_downcast_ref::<gtk::Window>(),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |result| {
                    let Some(path) = result.ok().and_then(|file| file.path()) else {
                        return;
                    };
                    if is_empty_or_missing(&path) {
                        page.folder_row.set_subtitle(&path.to_string_lossy());
                        *page.chosen.borrow_mut() = Some(path);
                    } else {
                        // An empty folder avoids mixing unrelated files into Drive.
                        page.folder_row
                            .set_subtitle("That folder isn’t empty. Choose an empty one.");
                        *page.chosen.borrow_mut() = None;
                    }
                    page.update_start();
                }
            ),
        );
    }

    fn confirm_stop(self: &Rc<Self>, anchor: &gtk::Button) {
        let dialog = adw::AlertDialog::builder()
            .heading("Stop syncing?")
            .body(
                "Files stay where they are, both on this computer and in Drive. Changes \
                 stop flowing between them.",
            )
            .close_response("cancel")
            .default_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("stop", "Stop Syncing")]);
        dialog.set_response_appearance("stop", adw::ResponseAppearance::Destructive);
        dialog.connect_response(
            Some("stop"),
            glib::clone!(
                #[weak(rename_to = page)]
                self,
                move |_, _| page.engine.send(Command::StopSync)
            ),
        );
        dialog.present(Some(anchor));
    }
}

fn is_empty_or_missing(path: &std::path::Path) -> bool {
    match std::fs::read_dir(path) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}
