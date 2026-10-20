//! Dialogs for file actions: rename, move, and asking for write access.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use aerie_core::{DriveFileId, FileEntry, compare_names};
use gtk::glib;

/// Asks for a new name, preselecting the part before the extension.
pub fn rename(parent: &impl IsA<gtk::Widget>, entry: &FileEntry, done: impl Fn(String) + 'static) {
    let field = gtk::Entry::builder()
        .text(&entry.name)
        .activates_default(true)
        .build();
    let dialog = adw::AlertDialog::builder()
        .heading("Rename")
        .extra_child(&field)
        .close_response("cancel")
        .default_response("rename")
        .build();
    dialog.add_responses(&[("cancel", "Cancel"), ("rename", "Rename")]);
    dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);

    let original = entry.name.clone();
    field.connect_changed(glib::clone!(
        #[weak]
        dialog,
        move |field| {
            let name = field.text();
            let name = name.trim();
            dialog.set_response_enabled("rename", !name.is_empty() && name != original);
        }
    ));
    dialog.set_response_enabled("rename", false);
    dialog.connect_response(
        Some("rename"),
        glib::clone!(
            #[weak]
            field,
            move |_, _| done(field.text().trim().to_owned())
        ),
    );
    dialog.present(Some(parent));
    field.grab_focus();
    let stem = match entry.name.rfind('.') {
        Some(dot) if dot > 0 && !entry.is_folder() => dot,
        _ => entry.name.chars().count(),
    };
    field.select_region(0, stem as i32);
}

/// Lists a folder's children, for browsing destinations.
pub type Children = Rc<dyn Fn(&DriveFileId) -> Vec<FileEntry>>;

/// Lets the user browse folders and pick a destination for `item`.
pub fn move_to(
    parent: &impl IsA<gtk::Widget>,
    children: Children,
    root: DriveFileId,
    item: FileEntry,
    done: impl Fn(DriveFileId) + 'static,
) {
    let path: Rc<RefCell<Vec<FileEntry>>> = Rc::default();
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    let location = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["heading"])
        .build();
    let up = gtk::Button::builder()
        .icon_name("aerie-corner-left-up-symbolic")
        .tooltip_text("Up one folder")
        .css_classes(["flat"])
        .build();

    let current = {
        let path = path.clone();
        let root = root.clone();
        move || {
            path.borrow()
                .last()
                .map_or_else(|| root.clone(), |f| f.id.clone())
        }
    };

    // Rebuilds the folder list for the current location.
    let render: Rc<RefCell<Box<dyn Fn()>>> = Rc::new(RefCell::new(Box::new(|| {})));
    *render.borrow_mut() = Box::new({
        let render = Rc::downgrade(&render);
        let (list, location, up, path, children, item, current) = (
            list.clone(),
            location.clone(),
            up.clone(),
            path.clone(),
            children.clone(),
            item.clone(),
            current.clone(),
        );
        move || {
            while let Some(row) = list.first_child() {
                list.remove(&row);
            }
            let names: Vec<_> = path.borrow().iter().map(|f| f.name.clone()).collect();
            location.set_label(&if names.is_empty() {
                "My Drive".to_owned()
            } else {
                format!("My Drive › {}", names.join(" › "))
            });
            up.set_sensitive(!names.is_empty());

            let mut folders: Vec<_> = children(&current())
                .into_iter()
                // An item can't move into itself.
                .filter(|f| f.is_folder() && f.id != item.id)
                .collect();
            folders.sort_by(|a, b| compare_names(&a.name, &b.name));
            if folders.is_empty() {
                list.append(
                    &adw::ActionRow::builder()
                        .title("No folders here")
                        .sensitive(false)
                        .build(),
                );
            }
            for folder in folders {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&folder.name).as_str())
                    .activatable(true)
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("aerie-folder-symbolic"));
                row.add_suffix(&gtk::Image::from_icon_name("aerie-chevron-right-symbolic"));
                row.connect_activated(glib::clone!(
                    #[strong]
                    path,
                    #[strong]
                    render,
                    move |_| {
                        path.borrow_mut().push(folder.clone());
                        if let Some(render) = render.upgrade() {
                            (render.borrow())();
                        }
                    }
                ));
                list.append(&row);
            }
        }
    });
    up.connect_clicked(glib::clone!(
        #[strong]
        path,
        #[strong]
        render,
        move |_| {
            path.borrow_mut().pop();
            (render.borrow())();
        }
    ));
    (render.borrow())();

    let move_here = gtk::Button::builder()
        .label("Move Here")
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::new();
    header.pack_start(&up);
    header.pack_end(&move_here);

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    body.append(&location);
    body.append(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build(),
    );
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&body));

    let dialog = adw::Dialog::builder()
        .title(format!("Move “{}”", item.name))
        .content_width(420)
        .content_height(480)
        .child(&view)
        .build();
    let from = item.parent.clone();
    move_here.connect_clicked(glib::clone!(
        #[weak]
        dialog,
        move |_| {
            let to = current();
            if from.as_ref() != Some(&to) {
                done(to);
            }
            dialog.close();
        }
    ));
    dialog.present(Some(parent));
}

/// Explains why Google will ask again, then starts a sign-in with write access.
pub fn ask_for_write_access(parent: &impl IsA<gtk::Widget>, allow: impl Fn() + 'static) {
    let dialog = adw::AlertDialog::builder()
        .heading("Allow Aerie to make changes?")
        .body(
            "Aerie can only read your Drive so far. To create, rename, move or trash files, \
             Google needs to grant Aerie access to change your Drive. Your browser will open \
             to confirm.",
        )
        .close_response("cancel")
        .default_response("allow")
        .build();
    dialog.add_responses(&[("cancel", "Not Now"), ("allow", "Continue")]);
    dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
    dialog.connect_response(Some("allow"), move |_, _| allow());
    dialog.present(Some(parent));
}

/// A format a Google-native document can be exported as.
#[derive(Clone, Copy, Debug)]
pub struct ExportFormat {
    pub label: &'static str,
    pub mime_type: &'static str,
    pub extension: &'static str,
}

const fn format(
    label: &'static str,
    mime_type: &'static str,
    extension: &'static str,
) -> ExportFormat {
    ExportFormat {
        label,
        mime_type,
        extension,
    }
}

const PDF: ExportFormat = format("PDF", "application/pdf", "pdf");

/// Export choices for a Google-native type, most useful first.
pub fn export_formats(mime_type: &str) -> Vec<ExportFormat> {
    match mime_type {
        "application/vnd.google-apps.document" => vec![
            format(
                "Word",
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
                "docx",
            ),
            PDF,
            format(
                "OpenDocument",
                "application/vnd.oasis.opendocument.text",
                "odt",
            ),
            format("Plain Text", "text/plain", "txt"),
        ],
        "application/vnd.google-apps.spreadsheet" => vec![
            format(
                "Excel",
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                "xlsx",
            ),
            PDF,
            format(
                "OpenDocument",
                "application/vnd.oasis.opendocument.spreadsheet",
                "ods",
            ),
            format("CSV (first sheet)", "text/csv", "csv"),
        ],
        "application/vnd.google-apps.presentation" => vec![
            format(
                "PowerPoint",
                "application/vnd.openxmlformats-officedocument.presentationml.presentation",
                "pptx",
            ),
            PDF,
            format(
                "OpenDocument",
                "application/vnd.oasis.opendocument.presentation",
                "odp",
            ),
        ],
        "application/vnd.google-apps.drawing" => {
            vec![
                PDF,
                format("PNG", "image/png", "png"),
                format("SVG", "image/svg+xml", "svg"),
            ]
        }
        _ => vec![PDF],
    }
}

/// Asks which format to export a Google-native document as. An export is a snapshot:
/// edits to the downloaded copy don't flow back to the original.
pub fn choose_export(
    parent: &impl IsA<gtk::Widget>,
    entry: &FileEntry,
    formats: Vec<ExportFormat>,
    done: impl Fn(ExportFormat) + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Download “{}” as", entry.name))
        .body(
            "Google documents download as a copy in another format. Changes to the copy \
             don’t update the original in Drive.",
        )
        .close_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    for (index, format) in formats.iter().enumerate() {
        dialog.add_response(&index.to_string(), format.label);
    }
    dialog.set_default_response(Some("0"));
    dialog.set_response_appearance("0", adw::ResponseAppearance::Suggested);
    dialog.connect_response(None, move |_, response| {
        if let Some(format) = response.parse::<usize>().ok().and_then(|i| formats.get(i)) {
            done(*format);
        }
    });
    dialog.present(Some(parent));
}
