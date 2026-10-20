//! The transfer list: progress, pause, resume, retry and cancel.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use aerie_core::format_size;
use aerie_store::{Direction, Transfer, TransferState};
use gtk::{gio, glib};

use crate::engine::{Command, Engine};

struct Row {
    progress: gtk::ProgressBar,
    row: adw::ActionRow,
    direction: Direction,
}

pub struct TransfersPage {
    pub root: gtk::Widget,
    engine: Rc<Engine>,
    list: gtk::ListBox,
    stack: gtk::Stack,
    rows: RefCell<HashMap<i64, Row>>,
}

impl TransfersPage {
    pub fn new(engine: Rc<Engine>) -> Rc<Self> {
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .valign(gtk::Align::Start)
            .build();
        let clamp = adw::Clamp::builder()
            .maximum_size(720)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .child(&list)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("aerie-arrow-down-up-symbolic")
            .title("No transfers")
            .description(
                "Uploads and downloads appear here. Drop files on My Drive to upload them.",
            )
            .css_classes(["compact"])
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&scroller, Some("list"));
        stack.add_named(&empty, Some("empty"));

        let clear = gtk::Button::builder()
            .label("Clear Finished")
            .css_classes(["flat"])
            .build();
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new("Transfers", ""))
            .build();
        header.pack_end(&clear);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&stack));

        let page = Rc::new(Self {
            root: view.upcast(),
            engine,
            list,
            stack,
            rows: RefCell::default(),
        });
        clear.connect_clicked(glib::clone!(
            #[weak]
            page,
            move |_| page.engine.send(Command::ClearFinishedTransfers)
        ));
        page.show(Vec::new());
        page
    }

    /// Rebuilds the list from the store's current transfers.
    pub fn show(self: &Rc<Self>, transfers: Vec<Transfer>) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        let mut rows = self.rows.borrow_mut();
        rows.clear();
        self.stack.set_visible_child_name(if transfers.is_empty() {
            "empty"
        } else {
            "list"
        });
        // Newest first.
        for transfer in transfers.into_iter().rev() {
            let row = self.row(&transfer);
            self.list.append(&row.row);
            rows.insert(transfer.id, row);
        }
    }

    /// Updates one row's progress without rebuilding the list.
    pub fn progress(&self, id: i64, done: u64, total: Option<u64>) {
        if let Some(row) = self.rows.borrow().get(&id) {
            row.progress.set_fraction(fraction(done, total));
            row.row
                .set_subtitle(&progress_text(row.direction, done, total));
        }
    }

    /// How many transfers are waiting or running, for the sidebar badge.
    pub fn active_count(transfers: &[Transfer]) -> usize {
        transfers
            .iter()
            .filter(|t| matches!(t.state, TransferState::Queued | TransferState::Running))
            .count()
    }

    fn row(self: &Rc<Self>, transfer: &Transfer) -> Row {
        let row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&transfer.name).as_str())
            .subtitle(status_text(transfer))
            .build();
        row.add_prefix(&gtk::Image::from_icon_name(match transfer.direction {
            Direction::Upload => "aerie-arrow-up-symbolic",
            Direction::Download => "aerie-arrow-down-symbolic",
        }));
        let progress = gtk::ProgressBar::builder()
            .valign(gtk::Align::Center)
            .width_request(120)
            .fraction(fraction(transfer.transferred, transfer.total))
            .visible(!transfer.state.is_finished())
            .build();
        row.add_suffix(&progress);

        let id = transfer.id;
        let button = |icon: &str, tooltip: &str, command: fn(i64) -> Command| {
            let button = gtk::Button::builder()
                .icon_name(icon)
                .tooltip_text(tooltip)
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let engine = self.engine.clone();
            button.connect_clicked(move |_| engine.send(command(id)));
            row.add_suffix(&button);
        };
        match transfer.state {
            TransferState::Queued | TransferState::Running => {
                button("aerie-pause-symbolic", "Pause", Command::PauseTransfer);
                button("aerie-x-symbolic", "Cancel", Command::CancelTransfer);
            }
            TransferState::Paused => {
                button("aerie-play-symbolic", "Resume", Command::ResumeTransfer);
                button("aerie-x-symbolic", "Cancel", Command::CancelTransfer);
            }
            TransferState::Failed => {
                button(
                    "aerie-rotate-cw-symbolic",
                    "Try Again",
                    Command::ResumeTransfer,
                );
                button("aerie-x-symbolic", "Cancel", Command::CancelTransfer);
            }
            TransferState::Done if transfer.direction == Direction::Download => {
                let open = gtk::Button::builder()
                    .icon_name("aerie-external-link-symbolic")
                    .tooltip_text("Open")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let path = transfer.local_path.clone();
                open.connect_clicked(move |button| {
                    let file = gio::File::for_path(&path);
                    gtk::FileLauncher::new(Some(&file)).launch(
                        button.root().and_downcast_ref::<gtk::Window>(),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                });
                row.add_suffix(&open);
                let reveal = gtk::Button::builder()
                    .icon_name("aerie-folder-open-symbolic")
                    .tooltip_text("Show in Folder")
                    .valign(gtk::Align::Center)
                    .css_classes(["flat"])
                    .build();
                let path = transfer.local_path.clone();
                reveal.connect_clicked(move |button| {
                    let file = gio::File::for_path(&path);
                    gtk::FileLauncher::new(Some(&file)).open_containing_folder(
                        button.root().and_downcast_ref::<gtk::Window>(),
                        gio::Cancellable::NONE,
                        |_| {},
                    );
                });
                row.add_suffix(&reveal);
            }
            TransferState::Done | TransferState::Cancelled => {}
        }
        Row {
            progress,
            row,
            direction: transfer.direction,
        }
    }
}

fn fraction(done: u64, total: Option<u64>) -> f64 {
    match total {
        Some(total) if total > 0 => done as f64 / total as f64,
        _ => 0.0,
    }
}

fn progress_text(direction: Direction, done: u64, total: Option<u64>) -> String {
    let verb = match direction {
        Direction::Upload => "Uploading",
        Direction::Download => "Downloading",
    };
    match total {
        Some(total) => format!("{verb} · {} of {}", format_size(done), format_size(total)),
        None => format!("{verb} · {}", format_size(done)),
    }
}

fn status_text(transfer: &Transfer) -> String {
    match transfer.state {
        TransferState::Queued if transfer.transferred > 0 => "Waiting to resume".to_owned(),
        TransferState::Queued => "Waiting".to_owned(),
        TransferState::Running => {
            progress_text(transfer.direction, transfer.transferred, transfer.total)
        }
        TransferState::Paused => "Paused".to_owned(),
        TransferState::Failed => format!(
            "Didn’t finish: {}",
            transfer.error.as_deref().unwrap_or("unknown error")
        ),
        TransferState::Done => match transfer.direction {
            Direction::Upload => "Uploaded".to_owned(),
            Direction::Download => format!("Saved to {}", transfer.local_path.display()),
        },
        TransferState::Cancelled => "Cancelled".to_owned(),
    }
}
