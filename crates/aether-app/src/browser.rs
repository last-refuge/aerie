//! Main window: welcome screen, sidebar and the My Drive file browser.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::rc::Rc;

use adw::prelude::*;
use aether_core::{BrowseState, DriveFileId, FileEntry, FileKind, compare_names, format_size};
use gtk::{gio, glib};

use crate::fixtures::{self, ROOT_ID};

pub fn build_window(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Aether")
        .default_width(1080)
        .default_height(700)
        .width_request(360)
        .height_request(320)
        .build();

    let browser = Browser::new();
    let split = browser.split_view();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(180)
        .build();
    stack.add_named(&welcome_page(&stack), Some("welcome"));
    stack.add_named(&split, Some("browser"));
    window.set_content(Some(&stack));

    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 640sp").expect("valid breakpoint"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    let search = gio::SimpleAction::new("search", None);
    search.connect_activate(glib::clone!(
        #[strong]
        browser,
        #[weak]
        stack,
        move |_, _| {
            if stack.visible_child_name().as_deref() == Some("browser") {
                browser.search.grab_focus();
            }
        }
    ));
    let new_folder = gio::SimpleAction::new("new-folder", None);
    new_folder.connect_activate(glib::clone!(
        #[strong]
        browser,
        #[weak]
        stack,
        move |_, _| {
            if stack.visible_child_name().as_deref() == Some("browser") {
                browser.show_new_folder_dialog();
            }
        }
    ));
    window.add_action(&search);
    window.add_action(&new_folder);

    window.present();
}

fn welcome_page(stack: &gtk::Stack) -> gtk::Widget {
    let connect = gtk::Button::builder()
        .label("Explore sample files")
        .halign(gtk::Align::Center)
        .css_classes(["aether-primary"])
        .build();
    connect.connect_clicked(glib::clone!(
        #[weak]
        stack,
        move |_| stack.set_visible_child_name("browser")
    ));

    let note = gtk::Label::builder()
        .label("Google Drive sign-in isn’t available yet.")
        .wrap(true)
        .justify(gtk::Justification::Center)
        .css_classes(["metadata"])
        .build();

    let actions = gtk::Box::new(gtk::Orientation::Vertical, 16);
    actions.append(&connect);
    actions.append(&note);

    let status = adw::StatusPage::builder()
        .title("Aether")
        .description("Your cloud, at home on Linux.")
        .child(&actions)
        .build();

    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    view.set_content(Some(&status));
    view.add_css_class("aether-welcome");
    connect.grab_focus();
    view.upcast()
}

/// Widgets over a [`BrowseState`]. Navigation and search rebuild the visible list.
struct Browser {
    state: RefCell<BrowseState>,
    next_local_id: RefCell<u32>,
    store: gio::ListStore,
    view: gtk::ColumnView,
    search: gtk::SearchEntry,
    breadcrumbs: gtk::Box,
    content: gtk::Stack,
    empty: adw::StatusPage,
}

impl Browser {
    fn new() -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        let selection = gtk::SingleSelection::new(Some(sorted.clone()));
        let view = gtk::ColumnView::builder()
            .model(&selection)
            .reorderable(false)
            .css_classes(["aether-files"])
            .build();

        let name = column(
            "Name",
            name_factory(),
            by(|a, b| compare_names(&a.name, &b.name)),
        );
        name.set_expand(true);
        view.append_column(&name);
        view.append_column(&column(
            "Modified",
            text_factory(|entry| format_modified(entry.modified)),
            by(|a, b| a.modified.cmp(&b.modified)),
        ));
        view.append_column(&column(
            "Size",
            text_factory(|entry| entry.size.map_or_else(|| "—".to_owned(), format_size)),
            by(|a, b| a.size.cmp(&b.size)),
        ));
        // Folders stay first whichever column or direction the user sorts by.
        let order = gtk::MultiSorter::new();
        order.append(gtk::CustomSorter::new(|a, b| {
            entry_of(b).is_folder().cmp(&entry_of(a).is_folder()).into()
        }));
        if let Some(columns) = view.sorter() {
            order.append(columns);
        }
        sorted.set_sorter(Some(&order));
        view.sort_by_column(Some(&name), gtk::SortType::Ascending);

        let search = gtk::SearchEntry::builder()
            .placeholder_text("Search My Drive")
            .hexpand(true)
            .build();
        search.update_property(&[gtk::accessible::Property::KeyShortcuts("Control+K")]);

        let empty = adw::StatusPage::builder()
            .icon_name("folder-symbolic")
            .title("This folder is empty")
            .css_classes(["compact"])
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&scroller, Some("list"));
        content.add_named(&empty, Some("empty"));

        let browser = Rc::new(Self {
            state: RefCell::new(BrowseState::new(
                DriveFileId::new(ROOT_ID),
                fixtures::sample_entries(),
            )),
            next_local_id: RefCell::new(1),
            store,
            view,
            search,
            breadcrumbs: gtk::Box::builder()
                .spacing(2)
                .css_classes(["breadcrumbs"])
                .accessible_role(gtk::AccessibleRole::Navigation)
                .build(),
            content,
            empty,
        });

        browser.view.connect_activate(glib::clone!(
            #[weak]
            browser,
            move |view, position| {
                let Some(item) = view.model().and_then(|model| model.item(position)) else {
                    return;
                };
                let id = entry_of(&item).id.clone();
                let opened = browser.state.borrow_mut().open(&id);
                if opened {
                    browser.show_state();
                }
            }
        ));
        browser.search.connect_search_changed(glib::clone!(
            #[weak]
            browser,
            move |search| {
                browser.state.borrow_mut().set_query(&search.text());
                browser.refresh();
            }
        ));
        browser
            .search
            .connect_stop_search(|search| search.set_text(""));

        browser.refresh();
        browser
    }

    fn split_view(self: &Rc<Self>) -> adw::NavigationSplitView {
        let content = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder()
            .title_widget(
                &adw::Clamp::builder()
                    .maximum_size(480)
                    .child(&self.search)
                    .build(),
            )
            .build();
        let new_folder = gtk::Button::builder()
            .icon_name("folder-new-symbolic")
            .tooltip_text("New folder (Ctrl+Shift+N)")
            .action_name("win.new-folder")
            .build();
        header.pack_end(&new_folder);
        content.add_top_bar(&header);
        content.add_top_bar(
            &adw::Banner::builder()
                .title("Showing sample files. Google Drive isn’t connected yet.")
                .revealed(true)
                .build(),
        );

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&self.breadcrumbs);
        body.append(&self.content);
        content.set_content(Some(&body));

        adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::new(&self.sidebar(), "Aether"))
            .content(&adw::NavigationPage::new(&content, "My Drive"))
            .min_sidebar_width(200.0)
            .max_sidebar_width(260.0)
            .build()
    }

    fn sidebar(self: &Rc<Self>) -> gtk::Widget {
        let places = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .vexpand(true)
            .build();
        places.append(&sidebar_row("folder-symbolic", "My Drive"));
        places.select_row(places.row_at_index(0).as_ref());
        places.connect_row_activated(glib::clone!(
            #[weak(rename_to = browser)]
            self,
            move |_, _| browser.open_root()
        ));

        let status_icon = gtk::Image::from_icon_name("network-offline-symbolic");
        let status_text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        status_text.append(
            &gtk::Label::builder()
                .label("Not connected")
                .xalign(0.0)
                .build(),
        );
        status_text.append(
            &gtk::Label::builder()
                .label("Sample files only")
                .xalign(0.0)
                .css_classes(["metadata"])
                .build(),
        );
        let status = gtk::Box::builder()
            .spacing(12)
            .css_classes(["sync-status", "card"])
            .build();
        status.append(&status_icon);
        status.append(&status_text);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&places);
        body.append(&status);

        let title = gtk::Label::builder()
            .label("Aether")
            .css_classes(["brand-title"])
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::builder().title_widget(&title).build());
        view.set_content(Some(&body));
        view.upcast()
    }

    fn open_root(self: &Rc<Self>) {
        self.state.borrow_mut().go_to_depth(0);
        self.show_state();
    }

    /// Refreshes after navigation, which ends any search.
    fn show_state(self: &Rc<Self>) {
        if self.search.text().is_empty() {
            self.refresh();
        } else {
            // Emits search-changed, which refreshes.
            self.search.set_text("");
        }
    }

    fn refresh(self: &Rc<Self>) {
        let state = self.state.borrow();
        let searching = state.is_searching();
        let items: Vec<glib::BoxedAnyObject> = state
            .visible()
            .into_iter()
            .cloned()
            .map(glib::BoxedAnyObject::new)
            .collect();
        drop(state);
        self.store.remove_all();
        self.store.extend_from_slice(&items);

        if items.is_empty() {
            if searching {
                self.empty.set_icon_name(Some("system-search-symbolic"));
                self.empty.set_title("No results");
                self.empty
                    .set_description(Some("Try a different file name."));
            } else {
                self.empty.set_icon_name(Some("folder-symbolic"));
                self.empty.set_title("This folder is empty");
                self.empty.set_description(None);
            }
            self.content.set_visible_child_name("empty");
        } else {
            self.content.set_visible_child_name("list");
        }

        self.rebuild_breadcrumbs(searching);
    }

    fn rebuild_breadcrumbs(self: &Rc<Self>, searching: bool) {
        while let Some(child) = self.breadcrumbs.first_child() {
            self.breadcrumbs.remove(&child);
        }

        if searching {
            self.breadcrumbs.append(&crumb_label("Search results"));
            return;
        }

        let state = self.state.borrow();
        let last = state.depth();
        for (depth, name) in std::iter::once("My Drive")
            .chain(state.path_names())
            .enumerate()
        {
            if depth > 0 {
                self.breadcrumbs.append(
                    &gtk::Image::builder()
                        .icon_name("go-next-symbolic")
                        .css_classes(["dim-label"])
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build(),
                );
            }
            if depth == last {
                self.breadcrumbs.append(&crumb_label(name));
                continue;
            }
            let button = gtk::Button::builder()
                .label(name)
                .css_classes(["flat"])
                .build();
            button.connect_clicked(glib::clone!(
                #[weak(rename_to = browser)]
                self,
                move |_| {
                    browser.state.borrow_mut().go_to_depth(depth);
                    browser.show_state();
                }
            ));
            self.breadcrumbs.append(&button);
        }
    }

    fn show_new_folder_dialog(self: &Rc<Self>) {
        let entry = gtk::Entry::builder()
            .placeholder_text("Folder name")
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading("New folder")
            .extra_child(&entry)
            .close_response("cancel")
            .default_response("create")
            .build();
        dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
        dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        dialog.set_response_enabled("create", false);

        entry.connect_changed(glib::clone!(
            #[weak]
            dialog,
            move |entry| dialog.set_response_enabled("create", !entry.text().trim().is_empty())
        ));
        dialog.connect_response(
            Some("create"),
            glib::clone!(
                #[weak(rename_to = browser)]
                self,
                #[weak]
                entry,
                move |_, _| browser.create_folder(entry.text().trim())
            ),
        );
        dialog.present(Some(&self.view));
        entry.grab_focus();
    }

    fn create_folder(self: &Rc<Self>, name: &str) {
        let id = {
            let mut next = self.next_local_id.borrow_mut();
            *next += 1;
            DriveFileId::new(format!("local-{next}"))
        };
        let mut state = self.state.borrow_mut();
        let parent = state.current_folder().clone();
        state.add(FileEntry {
            id,
            parent: Some(parent),
            name: name.to_owned(),
            kind: FileKind::Folder,
            size: None,
            modified: glib::DateTime::now_local().map_or(0, |now| now.to_unix()),
        });
        let depth = state.depth();
        state.go_to_depth(depth);
        drop(state);
        self.show_state();
    }
}

fn sidebar_row(icon: &str, label: &str) -> gtk::ListBoxRow {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    row.append(&gtk::Image::from_icon_name(icon));
    row.append(&gtk::Label::new(Some(label)));
    gtk::ListBoxRow::builder().child(&row).build()
}

fn crumb_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .css_classes(["current"])
        .build()
}

fn column(
    title: &str,
    factory: gtk::SignalListItemFactory,
    sorter: gtk::CustomSorter,
) -> gtk::ColumnViewColumn {
    gtk::ColumnViewColumn::builder()
        .title(title)
        .factory(&factory)
        .sorter(&sorter)
        .resizable(true)
        .build()
}

fn by(compare: impl Fn(&FileEntry, &FileEntry) -> Ordering + 'static) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let a = entry_of(a);
        let b = entry_of(b);
        compare(&a, &b).into()
    })
}

fn entry_of(object: &glib::Object) -> std::cell::Ref<'_, FileEntry> {
    object
        .downcast_ref::<glib::BoxedAnyObject>()
        .expect("list items are file entries")
        .borrow::<FileEntry>()
}

fn name_factory() -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        row.append(&gtk::Image::builder().css_classes(["file-icon"]).build());
        row.append(
            &gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .build(),
        );
        list_item(item).set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = list_item(item);
        let object = item.item().expect("bound item");
        let entry = entry_of(&object);
        let row = item.child().expect("row widget");
        let icon = row
            .first_child()
            .and_downcast::<gtk::Image>()
            .expect("icon");
        let label = icon
            .next_sibling()
            .and_downcast::<gtk::Label>()
            .expect("label");
        icon.set_icon_name(Some(icon_name(entry.kind)));
        if entry.is_folder() {
            icon.add_css_class("folder");
        } else {
            icon.remove_css_class("folder");
        }
        label.set_label(&entry.name);
    });
    factory
}

fn text_factory(text: impl Fn(&FileEntry) -> String + 'static) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        list_item(item).set_child(Some(
            &gtk::Label::builder()
                .xalign(0.0)
                .css_classes(["metadata"])
                .build(),
        ));
    });
    factory.connect_bind(move |_, item| {
        let item = list_item(item);
        let object = item.item().expect("bound item");
        let entry = entry_of(&object);
        let label = item.child().and_downcast::<gtk::Label>().expect("label");
        label.set_label(&text(&entry));
    });
    factory
}

fn list_item(object: &glib::Object) -> &gtk::ListItem {
    object
        .downcast_ref::<gtk::ListItem>()
        .expect("column items are list items")
}

fn icon_name(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Folder => "folder-symbolic",
        FileKind::Document | FileKind::Pdf => "x-office-document-symbolic",
        FileKind::Spreadsheet => "x-office-spreadsheet-symbolic",
        FileKind::Presentation => "x-office-presentation-symbolic",
        FileKind::Image => "image-x-generic-symbolic",
        FileKind::Other => "text-x-generic-symbolic",
    }
}

fn format_modified(unix: i64) -> String {
    glib::DateTime::from_unix_local(unix)
        .and_then(|time| time.format("%b %-e, %Y"))
        .map_or_else(|_| "—".to_owned(), String::from)
}
