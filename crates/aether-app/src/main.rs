//! Aether desktop application.

use adw::prelude::*;

const APP_ID: &str = "io.github.last_refuge.Aether";

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_window);
    app.run()
}

fn build_window(app: &adw::Application) {
    let status = adw::StatusPage::builder()
        .title("Aether")
        .description("Your files, everywhere.")
        .build();

    let content = adw::ToolbarView::new();
    content.add_top_bar(&adw::HeaderBar::new());
    content.set_content(Some(&status));

    adw::ApplicationWindow::builder()
        .application(app)
        .title("Aether")
        .default_width(960)
        .default_height(640)
        .content(&content)
        .build()
        .present();
}
