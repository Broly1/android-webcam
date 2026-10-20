// Donations: the discreet footer link and the dialog it opens (PayPal and Pix).

use gtk4::prelude::*;
use gtk4::{gdk, glib, Box, Button, CssProvider, DrawingArea, Label, Orientation, Separator, Window};
use qrcode::QrCode;
use std::time::Duration;

use crate::i18n::tr;
use crate::{draw_qr_code, links, WidgetExtFixed};

const PAYPAL_URL: &str = "https://www.paypal.com/paypalme/bmacffe";

/// Static Pix code ("Pix copia e cola"): random key, no fixed amount.
const PIX_CODE: &str = "00020126990014BR.GOV.BCB.PIX0136db63b95a-3189-4f4f-b8b1-944775a630d80237Muito obrigado por apoiar o projeto!!5204000053039865802BR5920ELIERMI CUNHA JUNIOR6008SOBRALIA62070503***6304F396";

pub fn install_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = CssProvider::new();
    provider.load_from_data(
        "button.donate-btn { min-height: 0; padding: 2px 8px; font-size: 0.85em; font-weight: normal; opacity: 0.6; }
         button.donate-btn:hover { opacity: 1; }
         .donate-heart { color: #e01b24; }",
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Footer row with the "♥ Donate" link, to append below the page content.
pub fn footer(window: &impl IsA<Window>) -> Box {
    let content = Box::new(Orientation::Horizontal, 6);
    // A text heart instead of an icon name: always renders, whatever icon theme the host has.
    let heart = Label::new(Some("♥"));
    heart.add_css_class("donate-heart");
    content.append(&heart);
    content.append(&Label::new(Some(tr("donate.button"))));

    let button = Button::builder()
        .child(&content)
        .halign(gtk4::Align::Start)
        .css_classes(["flat", "donate-btn"])
        .build();
    let w: Window = window.clone().upcast();
    button.connect_clicked(move |_| show_dialog(&w));

    let footer = Box::new(Orientation::Horizontal, 0);
    footer.set_margin_start(8);
    footer.set_margin_bottom(8);
    footer.append(&button);
    footer
}

/// "Support Android Webcam" dialog: PayPal (opens the browser) and Pix (QR code + copy button).
pub fn show_dialog(parent: &impl IsA<Window>) {
    let body = Box::new(Orientation::Vertical, 12);
    body.set_margin_all(24);

    body.append(
        &Label::builder()
            .label(tr("donate.intro"))
            .wrap(true)
            .max_width_chars(40)
            .justify(gtk4::Justification::Center)
            .build(),
    );

    let paypal_btn = Button::builder()
        .label(tr("donate.paypal"))
        .css_classes(["suggested-action"])
        .halign(gtk4::Align::Center)
        .build();
    body.append(&paypal_btn);

    let separator = Separator::new(Orientation::Horizontal);
    separator.set_margin_top(6);
    separator.set_margin_bottom(6);
    body.append(&separator);

    body.append(&Label::builder().label(tr("donate.pix_title")).css_classes(["heading"]).build());
    body.append(
        &Label::builder()
            .label(tr("donate.pix_hint"))
            .wrap(true)
            .max_width_chars(40)
            .justify(gtk4::Justification::Center)
            .css_classes(["dim-label"])
            .build(),
    );

    // Dark on white whatever the theme: bank apps need the usual contrast to scan it.
    let code = QrCode::new(PIX_CODE).ok();
    let qr = DrawingArea::builder()
        .content_width(200)
        .content_height(200)
        .halign(gtk4::Align::Center)
        .build();
    qr.set_draw_func(move |_, cr, w, h| draw_qr_code(cr, w, h, code.as_ref()));
    body.append(&qr);

    let copy_btn = Button::builder()
        .label(tr("donate.copy_pix"))
        .halign(gtk4::Align::Center)
        .build();
    copy_btn.connect_clicked(|btn| {
        btn.clipboard().set_text(PIX_CODE);
        btn.set_label(tr("donate.copied"));
        let b = btn.clone();
        glib::timeout_add_local_once(Duration::from_secs(2), move || {
            b.set_label(tr("donate.copy_pix"));
        });
    });
    body.append(&copy_btn);

    let window = Window::builder()
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .title(tr("donate.title"))
        .child(&body)
        .build();
    let w = window.clone();
    paypal_btn.connect_clicked(move |_| links::open_url(&w, PAYPAL_URL));
    window.present();
}
