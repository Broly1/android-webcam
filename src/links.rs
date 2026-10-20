// Opening web links (donate, About dialog) in the user's browser.

use gtk4::prelude::*;
use gtk4::{glib, Box, Button, Label, Orientation, Window};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::i18n::tr;
use crate::{debug_log, WidgetExtFixed};

/// Open a link in the user's default browser. Asks the desktop portal first:
/// it runs in the real desktop session, so it finds the browser even when GTK's
/// own lookup can't (it can end up opening the page in a text editor). Falls
/// back to xdg-open, and as a last resort shows the link so it can be copied.
pub fn open_url(parent: &impl IsA<Window>, url: &str) {
    debug_log(&format!("opening {}", url));
    let parent: Window = parent.clone().upcast();
    let url = url.to_string();
    let bus = match gtk4::gio::bus_get_sync(gtk4::gio::BusType::Session, None::<&gtk4::gio::Cancellable>) {
        Ok(b) => b,
        Err(_) => {
            open_url_xdg(parent, url);
            return;
        }
    };
    // OpenURI(s parent_window, s uri, a{sv} options)
    let params = glib::Variant::tuple_from_iter([
        "".to_variant(),
        url.to_variant(),
        glib::VariantDict::new(None).end(),
    ]);
    bus.call(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.OpenURI",
        "OpenURI",
        Some(&params),
        None,
        gtk4::gio::DBusCallFlags::NONE,
        10_000,
        None::<&gtk4::gio::Cancellable>,
        move |res| {
            if res.is_err() {
                open_url_xdg(parent, url);
            }
        },
    );
}

fn open_url_xdg(parent: Window, url: String) {
    let spawned = Command::new("xdg-open")
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => Some(c),
        Err(_) => {
            show_link_window(&parent, &url);
            return;
        }
    };
    // xdg-open exits quickly on failure, but may keep running while the
    // browser is open, so poll for a few seconds instead of blocking.
    let started = Instant::now();
    glib::timeout_add_local(Duration::from_millis(200), move || {
        let Some(c) = child.as_mut() else { return glib::ControlFlow::Break };
        match c.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    show_link_window(&parent, &url);
                }
            }
            Ok(None) if started.elapsed() < Duration::from_secs(5) => {
                return glib::ControlFlow::Continue;
            }
            Ok(None) => {
                // Still running: assume the browser opened; reap it later.
                if let Some(mut c) = child.take() {
                    thread::spawn(move || {
                        let _ = c.wait();
                    });
                }
            }
            Err(_) => {}
        }
        glib::ControlFlow::Break
    });
}

/// Last resort when no browser could be launched: show the link, selectable.
fn show_link_window(parent: &Window, url: &str) {
    let win = Window::builder()
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .title(tr("donate.button"))
        .build();
    let content = Box::new(Orientation::Vertical, 12);
    content.set_margin_all(18);
    content.append(
        &Label::builder()
            .label(tr("donate.copy_link"))
            .wrap(true)
            .max_width_chars(40)
            .build(),
    );
    content.append(&Label::builder().label(url).selectable(true).build());
    let close_btn = Button::builder()
        .label(tr("donate.close"))
        .halign(gtk4::Align::End)
        .build();
    let w_c = win.clone();
    close_btn.connect_clicked(move |_| w_c.close());
    content.append(&close_btn);
    win.set_child(Some(&content));
    win.present();
}
