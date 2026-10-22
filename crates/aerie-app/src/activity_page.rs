//! Sync activity: every change in the current (or last) sync pass, grouped by where it
//! stands: happening now, up next, needing attention, and done.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::engine::{Activity, ActivityItem, ItemState, Way};

/// The most rows a group lists; the rest are counted.
const MOST_ROWS: usize = 150;

pub struct ActivityPage {
    pub root: gtk::Widget,
    title: gtk::Label,
    detail: gtk::Label,
    bar: gtk::ProgressBar,
    groups: gtk::Box,
    stack: gtk::Stack,
    latest: RefCell<Option<Activity>>,
}

impl ActivityPage {
    pub fn new() -> Rc<Self> {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["activity-title"])
            .build();
        let detail = gtk::Label::builder()
            .xalign(0.0)
            .css_classes(["metadata", "numeric"])
            .build();
        let bar = gtk::ProgressBar::builder()
            .css_classes(["sync-bar"])
            .margin_top(6)
            .build();
        let summary = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .css_classes(["card", "activity-summary"])
            .build();
        summary.append(&title);
        summary.append(&detail);
        summary.append(&bar);

        let groups = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .build();
        content.append(&summary);
        content.append(&groups);
        let clamp = adw::Clamp::builder()
            .maximum_size(820)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .child(&content)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();
        let empty = adw::StatusPage::builder()
            .icon_name("aerie-refresh-cw-symbolic")
            .title("No sync activity yet")
            .description("Changes appear here as Aerie syncs your folders.")
            .css_classes(["compact"])
            .build();
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&scroller, Some("list"));

        let page = Rc::new(Self {
            root: stack.clone().upcast(),
            title,
            detail,
            bar,
            groups,
            stack,
            latest: RefCell::default(),
        });
        // Rebuilding a long list is only worth it while the page is on screen.
        page.root.connect_map(glib::clone!(
            #[weak]
            page,
            move |_| page.render()
        ));
        page
    }

    pub fn show(&self, activity: Activity) {
        self.latest.replace(Some(activity));
        if self.root.is_mapped() {
            self.render();
        }
    }

    fn render(&self) {
        let latest = self.latest.borrow();
        let Some(activity) = latest.as_ref() else {
            self.stack.set_visible_child_name("empty");
            return;
        };
        self.stack.set_visible_child_name("list");

        let finished = activity.count(|s| !matches!(s, ItemState::Waiting | ItemState::Running));
        let failed = activity.count(|s| matches!(s, ItemState::Failed(_)));
        let when = started(activity.started);
        if activity.finished {
            self.title.set_label("Last sync");
            let changes = plural(activity.total, "change");
            self.detail.set_label(&match failed {
                0 => format!("{changes} · {when}"),
                n => format!("{changes}, {} · {when}", plural(n, "failed")),
            });
            self.bar.set_visible(false);
        } else {
            self.title.set_label("Syncing");
            self.detail.set_label(&format!(
                "{} of {} · started {when}",
                finished.min(activity.total),
                plural(activity.total, "change")
            ));
            self.bar.set_visible(true);
            self.bar
                .set_fraction(finished as f64 / activity.total.max(1) as f64);
        }

        while let Some(child) = self.groups.first_child() {
            self.groups.remove(&child);
        }
        let pick = |matches: &dyn Fn(&ItemState) -> bool| -> Vec<&ActivityItem> {
            activity
                .items
                .iter()
                .filter(|item| matches(&item.state))
                .collect()
        };
        let now = pick(&|s| matches!(s, ItemState::Running));
        let attention = pick(&|s| matches!(s, ItemState::Failed(_) | ItemState::Skipped(_)));
        let next = pick(&|s| matches!(s, ItemState::Waiting));
        let mut done = pick(&|s| matches!(s, ItemState::Done | ItemState::KeptBoth(_)));
        done.reverse();
        let unlisted = activity.total.saturating_sub(activity.items.len());

        self.group("Now", &now, 0);
        self.group("Needs attention", &attention, 0);
        let next_title = if activity.finished {
            "Not reached"
        } else {
            "Up next"
        };
        self.group(next_title, &next, unlisted);
        self.group("Done", &done, 0);
    }

    fn group(&self, title: &str, items: &[&ActivityItem], also: usize) {
        if items.is_empty() && also == 0 {
            return;
        }
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        for item in items.iter().take(MOST_ROWS) {
            list.append(&row(item));
        }
        let more = items.len().saturating_sub(MOST_ROWS) + also;
        if more > 0 {
            list.append(
                &adw::ActionRow::builder()
                    .title(format!("…and {}", plural(more, "more")))
                    .css_classes(["dim-label"])
                    .build(),
            );
        }
        let heading = gtk::Box::builder().spacing(8).build();
        heading.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["heading"])
                .build(),
        );
        heading.append(
            &gtk::Label::builder()
                .label((items.len() + also).to_string())
                .css_classes(["activity-count", "numeric"])
                .build(),
        );
        let section = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .build();
        section.append(&heading);
        section.append(&list);
        self.groups.append(&section);
    }
}

fn row(item: &ActivityItem) -> adw::ActionRow {
    let path = std::path::Path::new(&item.path);
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| item.path.clone());
    let folder = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    let subtitle = match &item.state {
        ItemState::Failed(message) => message.clone(),
        ItemState::Skipped(reason) => reason.clone(),
        ItemState::KeptBoth(copy) => format!("Both kept; Drive's copy is {copy}"),
        _ if folder.is_empty() => item.verb.clone(),
        _ => format!("{} · {folder}", item.verb),
    };
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&name).as_str())
        .subtitle(glib::markup_escape_text(&subtitle).as_str())
        .subtitle_lines(2)
        .build();
    let (icon, class) = match item.way {
        Way::Down => ("aerie-arrow-down-symbolic", "way-down"),
        Way::Up => ("aerie-arrow-up-symbolic", "way-up"),
        Way::Trash => ("aerie-trash-symbolic", "way-trash"),
        Way::Both => ("aerie-arrow-down-up-symbolic", "way-both"),
    };
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name(icon)
            .pixel_size(16)
            .valign(gtk::Align::Center)
            .css_classes(["activity-way", class])
            .tooltip_text(&item.verb)
            .build(),
    );
    row.add_suffix(&state_widget(&item.state));
    row
}

fn state_widget(state: &ItemState) -> gtk::Widget {
    let chip = |text: &str, class: &str| -> gtk::Widget {
        gtk::Label::builder()
            .label(text)
            .valign(gtk::Align::Center)
            .css_classes(["activity-state", class])
            .build()
            .upcast()
    };
    match state {
        ItemState::Waiting => chip("Waiting", "waiting"),
        ItemState::Running => adw::Spinner::builder()
            .width_request(16)
            .height_request(16)
            .valign(gtk::Align::Center)
            .build()
            .upcast(),
        ItemState::Done => gtk::Image::builder()
            .icon_name("aerie-cloud-check-symbolic")
            .valign(gtk::Align::Center)
            .css_classes(["activity-done"])
            .tooltip_text("Done")
            .build()
            .upcast(),
        ItemState::KeptBoth(_) => chip("Kept both", "kept"),
        ItemState::Skipped(_) => chip("Skipped", "skipped"),
        ItemState::Failed(_) => chip("Failed", "failed"),
    }
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, _) => format!("1 {word}"),
        (_, "failed" | "more") => format!("{n} {word}"),
        _ => format!("{n} {word}s"),
    }
}

/// "just now", "5 minutes ago", or the time of day.
fn started(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    match now - unix {
        ..60 => "just now".to_owned(),
        seconds @ 60..3600 => match seconds / 60 {
            1 => "a minute ago".to_owned(),
            minutes => format!("{minutes} minutes ago"),
        },
        _ => glib::DateTime::from_unix_local(unix)
            .and_then(|time| time.format("at %-l:%M %p"))
            .map_or_else(|_| String::new(), String::from),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_read_naturally() {
        assert_eq!(plural(1, "change"), "1 change");
        assert_eq!(plural(3, "change"), "3 changes");
        assert_eq!(plural(2, "failed"), "2 failed");
        assert_eq!(plural(40, "more"), "40 more");
    }
}
