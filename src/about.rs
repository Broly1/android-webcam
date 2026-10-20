// Main menu (hamburger button in the header bar) with About and License.

use gtk4::prelude::*;
use gtk4::{gio, glib, ApplicationWindow, HeaderBar, MenuButton, ScrolledWindow, TextView, Window};

use crate::i18n::tr;
use crate::links;

const WEBSITE: &str = "https://github.com/Broly1/android-webcam";
const LICENSE_TEXT: &str = include_str!("../LICENSE");

/// Hamburger button for the header bar. Its items run the window actions
/// registered by `install_actions`.
pub fn menu_button() -> MenuButton {
    let menu = gio::Menu::new();
    menu.append(Some(tr("menu.about")), Some("win.about"));
    menu.append(Some(tr("menu.license")), Some("win.license"));
    MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text(tr("menu.tooltip"))
        .menu_model(&menu)
        .build()
}

/// Register the "win.about" and "win.license" actions on the main window.
pub fn install_actions(window: &ApplicationWindow) {
    let about = gio::SimpleAction::new("about", None);
    let w = window.clone();
    about.connect_activate(move |_, _| show_about(&w));
    window.add_action(&about);

    let license = gio::SimpleAction::new("license", None);
    let w = window.clone();
    license.connect_activate(move |_, _| show_license(&w));
    window.add_action(&license);
}

fn show_about(parent: &ApplicationWindow) {
    let about = gtk4::AboutDialog::builder()
        .transient_for(parent)
        .modal(true)
        .program_name(tr("app.title"))
        .logo_icon_name("android-webcam")
        .version(env!("CARGO_PKG_VERSION"))
        .comments(tr("about.comments"))
        .website(WEBSITE)
        .website_label("GitHub")
        .authors(["Broly https://github.com/Broly1"])
        .copyright("© 2026 Broly")
        .license_type(gtk4::License::Gpl30)
        .build();
    about.add_credit_section(
        tr("about.built_on"),
        &[
            "scrcpy https://github.com/Genymobile/scrcpy",
            "v4l2loopback https://github.com/v4l2loopback/v4l2loopback",
            "Android platform-tools (adb) https://developer.android.com/tools/releases/platform-tools",
        ],
    );
    // Open links ourselves: GTK's default handler can pick the wrong app (see links.rs).
    about.connect_activate_link(|w, uri| {
        links::open_url(w, uri);
        glib::Propagation::Stop
    });
    about.present();
}

/// The full GPL text (the repo's LICENSE file, embedded at build time).
fn show_license(parent: &ApplicationWindow) {
    let text = TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .wrap_mode(gtk4::WrapMode::WordChar)
        .left_margin(18)
        .right_margin(18)
        .top_margin(12)
        .bottom_margin(12)
        .build();
    text.buffer().set_text(LICENSE_TEXT);

    let scrolled = ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .child(&text)
        .build();

    let window = Window::builder()
        .transient_for(parent)
        .modal(true)
        .title(tr("menu.license"))
        .default_width(640)
        .default_height(560)
        .child(&scrolled)
        .build();
    window.set_titlebar(Some(&HeaderBar::new()));
    window.present();
}
