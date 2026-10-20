//! Main window: welcome screen, sidebar and the My Drive file browser.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::rc::Rc;

use adw::prelude::*;
use aerie_core::{
    BrowseState, DriveFileId, FileEntry, FileKind, MemoryCatalog, compare_names, format_size,
};
use aerie_store::Account;
use gtk::{gio, glib};

use crate::catalog::{AppCatalog, StoreCatalog};
use crate::engine::{Command, Engine, Event};
use crate::fixtures::{self, ROOT_ID};

pub fn build_window(app: &adw::Application) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Aerie")
        .default_width(1080)
        .default_height(700)
        .width_request(360)
        .height_request(320)
        .build();

    let (events, inbox) = async_channel::unbounded();
    let engine = Rc::new(Engine::start(events));
    let browser = Browser::new();
    let welcome = Welcome::new(engine.clone());
    let split = browser.split_view();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(180)
        .build();
    stack.add_named(&welcome.root, Some("welcome"));
    stack.add_named(&split, Some("browser"));
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&stack));
    window.set_content(Some(&toasts));

    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 640sp").expect("valid breakpoint"),
    );
    narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
    window.add_breakpoint(narrow);

    install_actions(&window, &stack, &browser, &welcome, &engine);
    welcome.explore.connect_clicked(glib::clone!(
        #[weak]
        stack,
        move |_| stack.set_visible_child_name("browser")
    ));

    glib::spawn_future_local(glib::clone!(
        #[weak]
        window,
        #[weak]
        stack,
        #[weak]
        toasts,
        #[strong]
        browser,
        #[strong]
        welcome,
        async move {
            while let Ok(event) = inbox.recv().await {
                handle_event(event, &window, &stack, &toasts, &browser, &welcome);
            }
        }
    ));

    window.present();
}

fn install_actions(
    window: &adw::ApplicationWindow,
    stack: &gtk::Stack,
    browser: &Rc<Browser>,
    welcome: &Rc<Welcome>,
    engine: &Rc<Engine>,
) {
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
    // Creating folders on Drive arrives with file actions; until then only sample data
    // offers it.
    let new_folder = gio::SimpleAction::new("new-folder", None);
    new_folder.connect_activate(glib::clone!(
        #[strong]
        browser,
        move |_, _| browser.show_new_folder_dialog()
    ));
    let sign_in = gio::SimpleAction::new("sign-in", None);
    sign_in.connect_activate(glib::clone!(
        #[strong]
        engine,
        #[strong]
        welcome,
        #[weak]
        stack,
        move |_, _| {
            welcome.set_waiting(true);
            stack.set_visible_child_name("welcome");
            engine.send(Command::SignIn);
        }
    ));
    let sign_out = gio::SimpleAction::new("sign-out", None);
    sign_out.set_enabled(false);
    sign_out.connect_activate(glib::clone!(
        #[strong]
        engine,
        move |_, _| engine.send(Command::SignOut)
    ));
    let refresh = gio::SimpleAction::new("refresh", None);
    refresh.connect_activate(glib::clone!(
        #[strong]
        engine,
        move |_, _| engine.send(Command::Refresh)
    ));
    for action in [&search, &new_folder, &sign_in, &sign_out, &refresh] {
        window.add_action(action);
    }
}

fn set_action_enabled(window: &adw::ApplicationWindow, name: &str, enabled: bool) {
    if let Some(action) = window
        .lookup_action(name)
        .and_downcast::<gio::SimpleAction>()
    {
        action.set_enabled(enabled);
    }
}

fn handle_event(
    event: Event,
    window: &adw::ApplicationWindow,
    stack: &gtk::Stack,
    toasts: &adw::ToastOverlay,
    browser: &Rc<Browser>,
    welcome: &Rc<Welcome>,
) {
    match event {
        Event::OpenUri(uri) => open_uri(window, &uri),
        Event::SignInStarted => welcome.set_waiting(true),
        Event::SignInFailed(message) => {
            welcome.set_waiting(false);
            welcome.show_error(&message);
            stack.set_visible_child_name("welcome");
        }
        Event::SignedIn(account) => {
            welcome.set_waiting(false);
            browser.show_drive(&account);
            set_action_enabled(window, "sign-out", true);
            set_action_enabled(window, "new-folder", false);
            stack.set_visible_child_name("browser");
        }
        Event::NeedsSignIn { expired } => {
            welcome.set_waiting(false);
            if expired && browser.is_drive() {
                // Keep the cached files on screen and ask for a fresh sign-in.
                browser.set_status(Status::NeedsSignIn);
            } else {
                stack.set_visible_child_name("welcome");
            }
        }
        Event::SignedOut => {
            browser.show_samples();
            set_action_enabled(window, "sign-out", false);
            set_action_enabled(window, "new-folder", true);
            welcome.show_error("");
            stack.set_visible_child_name("welcome");
        }
        Event::Indexing { files } => browser.set_status(Status::Indexing(files)),
        Event::UpToDate => browser.set_status(Status::UpToDate),
        Event::CatalogChanged => browser.reload(),
        Event::Offline => browser.set_status(Status::Offline),
        Event::Problem(message) => toasts.add_toast(adw::Toast::new(&message)),
    }
}

fn open_uri(window: &adw::ApplicationWindow, uri: &str) {
    gtk::UriLauncher::new(uri).launch(Some(window), gio::Cancellable::NONE, |result| {
        if let Err(err) = result {
            tracing::warn!(%err, "couldn't open the browser");
        }
    });
}

/// The first screen: one sign-in action, with progress and recoverable errors.
struct Welcome {
    root: gtk::Widget,
    connect: gtk::Button,
    explore: gtk::Button,
    waiting: gtk::Box,
    message: gtk::Label,
}

impl Welcome {
    fn new(engine: Rc<Engine>) -> Rc<Self> {
        let connect = gtk::Button::builder()
            .label("Connect Google Drive")
            .halign(gtk::Align::Center)
            .action_name("win.sign-in")
            .css_classes(["aerie-primary"])
            .build();
        let explore = gtk::Button::builder()
            .label("Explore sample files")
            .halign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();

        let cancel = gtk::Button::builder()
            .label("Cancel")
            .css_classes(["flat"])
            .build();
        cancel.connect_clicked(move |_| engine.send(Command::CancelSignIn));
        let waiting = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        waiting.append(&adw::Spinner::new());
        waiting.append(&gtk::Label::new(Some("Finish signing in in your browser.")));
        waiting.append(&cancel);

        let message = gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["error"])
            .visible(false)
            .build();
        let note = gtk::Label::builder()
            .label(
                "Aerie asks Google for read-only access to your Drive and talks to Google \
                 directly from this computer.",
            )
            .wrap(true)
            .max_width_chars(48)
            .justify(gtk::Justification::Center)
            .css_classes(["metadata"])
            .build();

        let actions = gtk::Box::new(gtk::Orientation::Vertical, 16);
        actions.append(&connect);
        actions.append(&waiting);
        actions.append(&message);
        actions.append(&note);
        actions.append(&explore);

        let status = adw::StatusPage::builder()
            .icon_name(crate::APP_ID)
            .title("Aerie")
            .description("Your cloud, at home on Linux.")
            .child(&actions)
            .build();

        let view = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder().show_title(false).build();
        header.pack_end(&crate::app_menu::button());
        view.add_top_bar(&header);
        view.set_content(Some(&status));
        view.add_css_class("aerie-welcome");
        connect.grab_focus();

        Rc::new(Self {
            root: view.upcast(),
            connect,
            explore,
            waiting,
            message,
        })
    }

    fn set_waiting(&self, waiting: bool) {
        self.waiting.set_visible(waiting);
        self.connect.set_visible(!waiting);
        if waiting {
            self.show_error("");
        }
    }

    fn show_error(&self, message: &str) {
        self.message.set_label(message);
        self.message.set_visible(!message.is_empty());
    }
}

#[derive(Clone, Copy)]
enum Status {
    Samples,
    Indexing(u64),
    UpToDate,
    Offline,
    NeedsSignIn,
}

/// Widgets over a [`BrowseState`]. Navigation and search rebuild the visible list.
struct Browser {
    state: RefCell<BrowseState<AppCatalog>>,
    account: RefCell<Option<Account>>,
    indexing: Cell<bool>,
    next_local_id: Cell<u32>,
    store: gio::ListStore,
    view: gtk::ColumnView,
    search: gtk::SearchEntry,
    breadcrumbs: gtk::Box,
    content: gtk::Stack,
    empty: adw::StatusPage,
    banner: adw::Banner,
    status_icon: gtk::Image,
    status_title: gtk::Label,
    status_detail: gtk::Label,
}

impl Browser {
    fn new() -> Rc<Self> {
        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sorted = gtk::SortListModel::new(Some(store.clone()), None::<gtk::Sorter>);
        let selection = gtk::SingleSelection::new(Some(sorted.clone()));
        let view = gtk::ColumnView::builder()
            .model(&selection)
            .reorderable(false)
            .css_classes(["aerie-files"])
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

        let empty = adw::StatusPage::builder().css_classes(["compact"]).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&view)
            .build();
        let content = gtk::Stack::new();
        content.add_named(&scroller, Some("list"));
        content.add_named(&empty, Some("empty"));

        let browser = Rc::new(Self {
            state: RefCell::new(samples()),
            account: RefCell::default(),
            indexing: Cell::new(false),
            next_local_id: Cell::new(1),
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
            banner: adw::Banner::new(""),
            status_icon: gtk::Image::new(),
            status_title: gtk::Label::builder().xalign(0.0).build(),
            status_detail: gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["metadata"])
                .build(),
        });

        browser.view.connect_activate(glib::clone!(
            #[weak]
            browser,
            move |view, position| {
                let Some(item) = view.model().and_then(|model| model.item(position)) else {
                    return;
                };
                let entry = entry_of(&item).clone();
                browser.activate(&entry);
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
        browser.banner.connect_button_clicked(glib::clone!(
            #[weak]
            browser,
            move |banner| {
                if let Some(window) = banner.root().and_downcast::<gtk::Window>() {
                    let _ = WidgetExt::activate_action(&window, "win.sign-in", None);
                }
                browser.banner.set_revealed(false);
            }
        ));

        browser.set_status(Status::Samples);
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
        // Hidden rather than greyed out while unavailable, as the plan asks.
        new_folder
            .bind_property("sensitive", &new_folder, "visible")
            .sync_create()
            .build();
        header.pack_end(&new_folder);
        content.add_top_bar(&header);
        content.add_top_bar(&self.banner);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&self.breadcrumbs);
        body.append(&self.content);
        content.set_content(Some(&body));

        adw::NavigationSplitView::builder()
            .sidebar(&adw::NavigationPage::new(&self.sidebar(), "Aerie"))
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

        let text = gtk::Box::new(gtk::Orientation::Vertical, 0);
        text.append(&self.status_title);
        text.append(&self.status_detail);
        let status = gtk::Box::builder()
            .spacing(12)
            .css_classes(["sync-status", "card"])
            .accessible_role(gtk::AccessibleRole::Status)
            .build();
        status.append(&self.status_icon);
        status.append(&text);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&places);
        body.append(&status);

        let title = gtk::Label::builder()
            .label("Aerie")
            .css_classes(["brand-title"])
            .build();
        let view = adw::ToolbarView::new();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_end(&crate::app_menu::button());
        view.add_top_bar(&header);
        view.set_content(Some(&body));
        view.upcast()
    }

    fn is_drive(&self) -> bool {
        self.state.borrow().catalog().is_drive()
    }

    fn show_drive(self: &Rc<Self>, account: &Account) {
        let catalog = match StoreCatalog::open(account.id.clone()) {
            Ok(catalog) => catalog,
            Err(err) => {
                tracing::error!(%err, "couldn't open the metadata cache");
                return;
            }
        };
        // Before the first inventory finishes, the root isn't known; the alias works for
        // nothing, so show the indexing state until it arrives.
        let root = catalog.root().unwrap_or_else(|| DriveFileId::new("root"));
        self.indexing.set(!catalog.inventory_complete());
        self.state
            .borrow_mut()
            .replace(root, AppCatalog::Drive(catalog));
        self.account.replace(Some(account.clone()));
        self.banner.set_revealed(false);
        self.set_status(if self.indexing.get() {
            Status::Indexing(0)
        } else {
            Status::UpToDate
        });
        self.show_state();
    }

    fn show_samples(self: &Rc<Self>) {
        self.state.replace(samples());
        self.account.replace(None);
        self.indexing.set(false);
        self.set_status(Status::Samples);
        self.show_state();
    }

    /// Re-reads the catalog after the engine stored new metadata.
    fn reload(self: &Rc<Self>) {
        {
            let mut state = self.state.borrow_mut();
            if let AppCatalog::Drive(catalog) = state.catalog()
                && let Some(root) = catalog.root()
                && &root != state.root()
            {
                // The first inventory just reported the real root ID.
                let AppCatalog::Drive(catalog) = std::mem::replace(
                    state.catalog_mut(),
                    AppCatalog::Samples(MemoryCatalog::default()),
                ) else {
                    unreachable!()
                };
                state.replace(root, AppCatalog::Drive(catalog));
            }
            if let AppCatalog::Drive(catalog) = state.catalog() {
                self.indexing.set(!catalog.inventory_complete());
            }
            state.revalidate();
        }
        self.refresh();
    }

    fn set_status(&self, status: Status) {
        let email = self
            .account
            .borrow()
            .as_ref()
            .map(|account| account.email.clone())
            .unwrap_or_default();
        let (icon, title, detail) = match status {
            Status::Samples => (
                "network-offline-symbolic",
                "Not connected".to_owned(),
                "Sample files only".to_owned(),
            ),
            Status::Indexing(0) => (
                "content-loading-symbolic",
                "Checking for changes".to_owned(),
                email,
            ),
            Status::Indexing(files) => (
                "content-loading-symbolic",
                "Indexing your Drive".to_owned(),
                format!("{files} items so far"),
            ),
            Status::UpToDate => ("emblem-ok-symbolic", "Up to date".to_owned(), email),
            Status::Offline => (
                "network-offline-symbolic",
                "Offline".to_owned(),
                "Showing files from the last update".to_owned(),
            ),
            Status::NeedsSignIn => (
                "dialog-warning-symbolic",
                "Needs attention".to_owned(),
                "Sign in again to stay up to date".to_owned(),
            ),
        };
        self.status_icon.set_icon_name(Some(icon));
        self.status_title.set_label(&title);
        self.status_detail.set_label(&detail);

        match status {
            Status::Samples => {
                self.banner
                    .set_title("Showing sample files. Google Drive isn’t connected.");
                self.banner.set_button_label(Some("Connect"));
                self.banner.set_revealed(true);
            }
            Status::NeedsSignIn => {
                self.banner.set_title("Your Google sign-in has expired.");
                self.banner.set_button_label(Some("Sign In"));
                self.banner.set_revealed(true);
            }
            Status::Offline => {
                self.banner
                    .set_title("You’re offline. Showing files from the last update.");
                self.banner.set_button_label(None);
                self.banner.set_revealed(true);
            }
            Status::Indexing(_) | Status::UpToDate => self.banner.set_revealed(false),
        }
    }

    fn activate(self: &Rc<Self>, entry: &FileEntry) {
        if entry.is_folder() {
            let opened = self.state.borrow_mut().open(&entry.id);
            if opened {
                self.show_state();
            }
        } else if self.is_drive() {
            // Opening and downloading arrive with transfers; for now, open it in Drive.
            if let Some(window) = self.view.root().and_downcast::<adw::ApplicationWindow>() {
                open_uri(
                    &window,
                    &format!("https://drive.google.com/open?id={}", entry.id.as_str()),
                );
            }
        }
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
            .map(glib::BoxedAnyObject::new)
            .collect();
        drop(state);
        self.store.remove_all();
        self.store.extend_from_slice(&items);

        if items.is_empty() {
            self.show_empty(searching);
            self.content.set_visible_child_name("empty");
        } else {
            self.content.set_visible_child_name("list");
        }
        self.rebuild_breadcrumbs(searching);
    }

    /// An unfinished inventory must never look like an empty Drive.
    fn show_empty(&self, searching: bool) {
        if self.indexing.get() {
            self.empty
                .set_paintable(Some(&adw::SpinnerPaintable::new(Some(&self.empty))));
            self.empty.set_title("Indexing your Drive");
            self.empty.set_description(Some(
                "Files appear here as Aerie finds them. This happens once.",
            ));
        } else if searching {
            self.empty.set_icon_name(Some("system-search-symbolic"));
            self.empty.set_title("No results");
            self.empty
                .set_description(Some("Try a different file name."));
        } else {
            self.empty.set_icon_name(Some("folder-symbolic"));
            self.empty.set_title("This folder is empty");
            self.empty.set_description(None);
        }
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
                move |_, _| browser.create_sample_folder(entry.text().trim())
            ),
        );
        dialog.present(Some(&self.view));
        entry.grab_focus();
    }

    fn create_sample_folder(self: &Rc<Self>, name: &str) {
        let id = DriveFileId::new(format!("local-{}", self.next_local_id.get()));
        self.next_local_id.set(self.next_local_id.get() + 1);
        {
            let mut state = self.state.borrow_mut();
            let parent = state.current_folder().clone();
            let AppCatalog::Samples(catalog) = state.catalog_mut() else {
                return;
            };
            catalog.apply(aerie_core::Change::Upsert(FileEntry {
                id,
                parent: Some(parent),
                name: name.to_owned(),
                kind: FileKind::Folder,
                size: None,
                modified: glib::DateTime::now_local().map_or(0, |now| now.to_unix()),
            }));
            let depth = state.depth();
            state.go_to_depth(depth);
        }
        self.show_state();
    }
}

fn samples() -> BrowseState<AppCatalog> {
    BrowseState::new(
        DriveFileId::new(ROOT_ID),
        AppCatalog::Samples(MemoryCatalog::new(fixtures::sample_entries())),
    )
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
