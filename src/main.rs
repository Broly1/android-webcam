use gtk4::prelude::*;
use gtk4::{
    gdk, glib, Application, ApplicationWindow, Box, Button, CheckButton, ComboBoxText, CssProvider,
    DrawingArea, DropDown, Entry, Label, Orientation, PolicyType, ScrolledWindow, Separator, Stack,
    Window,
};
use mdns_sd::{RecvTimeoutError, ServiceDaemon, ServiceEvent};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

mod i18n;
use i18n::{fill, tr, trf};

const ADB_PAIRING_SERVICE: &str = "_adb-tls-pairing._tcp.local.";
const ADB_CONNECT_SERVICE: &str = "_adb-tls-connect._tcp.local.";
const QR_PAIR_TIMEOUT: Duration = Duration::from_secs(120);
const QR_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub camera_facing: u32,  // 0 = Back, 1 = Front
    pub resolution: String,  // e.g. "1920x1080"
    pub fps_index: u32,      // 0 = 30, 1 = 60
    pub block_mic: bool,     // true = mic blocked, false = mic on
    pub wireless_ip: String, // e.g. "192.168.1.50:5555"
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            camera_facing: 0,
            resolution: "1920x1080".to_string(),
            fps_index: 0,
            block_mic: false,
            wireless_ip: String::new(),
        }
    }
}

fn get_config_path() -> PathBuf {
    if let Ok(config_home) = env::var("XDG_CONFIG_HOME") {
        if !config_home.trim().is_empty() {
            return PathBuf::from(config_home)
                .join("android-webcam")
                .join("settings.json");
        }
    }
    let home = env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home)
        .join(".config")
        .join("android-webcam")
        .join("settings.json")
}

fn load_config() -> AppConfig {
    let path = get_config_path();
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(config) = serde_json::from_str::<AppConfig>(&data) {
            return config;
        }
    }
    AppConfig::default()
}

fn save_config(config: &AppConfig) {
    let path = get_config_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(data) = serde_json::to_string_pretty(config) {
        let _ = fs::write(path, data);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub serial: String,
    pub model: String,
    pub is_wireless: bool,
}

enum AppEvent {
    DevicesChanged(Vec<DeviceInfo>),
    WirelessConnectStatus {
        success: bool,
        message: String,
        ip: Option<String>,
    },
    SwitchToWirelessStatus {
        success: bool,
        message: String,
        ip: Option<String>,
    },
    QrPairStatus {
        session: u64,
        message: String,
        done: bool,
        connected_addr: Option<String>,
    },
}

/// Settings of the running stream, kept so it can be relaunched after a crash.
#[derive(Clone)]
struct LaunchParams {
    serial: Option<String>,
    fps: String,
    facing: String,
    block_mic: bool,
    res: String,
    /// No scrcpy window: only feeds the virtual webcam (for OBS, Zoom, ...).
    background: bool,
    /// Already relaunched once because the camera was busy.
    busy_retried: bool,
}

/// Last lines scrcpy printed (its own log plus the phone server's), used to tell why it crashed.
type ScrcpyOutput = Arc<Mutex<VecDeque<String>>>;
const SCRCPY_OUTPUT_LINES: usize = 200;

/// Why scrcpy exited, judged from its output.
#[derive(Debug, PartialEq, Eq)]
pub enum CrashCause {
    /// Another app (or a stream that hasn't shut down yet) holds the camera.
    CameraBusy,
    /// Camera privacy toggle or device policy.
    CameraBlocked,
    /// Anything else, most likely the phone can't encode this resolution.
    Other,
}

const APP_ID: &str = "com.android.webcam";
const APP_ICON_PATH: &str = "/usr/share/icons/hicolor/256x256/apps/android-webcam.png";
const USB_ICON: &str = "android-webcam-usb-symbolic";
const DONATE_URL: &str = "https://www.paypal.com/paypalme/bmacffe";

struct AppState {
    current_process: Option<Child>,
    current_launch: Option<LaunchParams>,
    current_output: ScrcpyOutput,
    active_serial: Option<String>,
    devices: Vec<DeviceInfo>,
}

fn main() {
    env::set_var("G_LOG_LEVELS", "critical");
    env::set_var("GDK_BACKEND", "wayland,x11,*");

    let app = Application::builder()
        .application_id(APP_ID)
        .build();
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &Application) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title(tr("app.title"))
        .default_width(380)
        .default_height(640)
        .build();

    install_theme_workarounds();
    install_donate_css();
    install_bundled_icons();

    let config = Arc::new(Mutex::new(load_config()));
    let state = Arc::new(Mutex::new(AppState {
        current_process: None,
        current_launch: None,
        current_output: ScrcpyOutput::default(),
        active_serial: None,
        devices: Vec::new(),
    }));

    let is_updating_res = Arc::new(AtomicBool::new(false));

    #[allow(deprecated)]
    let (tx, rx) = glib::MainContext::channel::<AppEvent>(glib::Priority::default());

    let stack = Stack::builder()
        .transition_type(gtk4::StackTransitionType::SlideLeftRight)
        .build();

    // ==========================================
    // CONTROLS SCREEN
    // ==========================================
    let controls_box = Box::new(Orientation::Vertical, 10);
    controls_box.set_margin_all(18);

    let device_label = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();

    let device_combo = ComboBoxText::new();
    device_combo.set_visible(false);

    let wifi_actions_box = Box::new(Orientation::Horizontal, 8);
    wifi_actions_box.set_homogeneous(true);

    let switch_wifi_btn = Button::builder()
        .label(tr("wifi.switch"))
        .tooltip_text(tr("wifi.switch_tooltip"))
        .build();

    let disconnect_box = Box::new(Orientation::Horizontal, 6);
    disconnect_box.set_halign(gtk4::Align::Center);
    disconnect_box.append(&gtk4::Image::from_icon_name(USB_ICON));
    disconnect_box.append(&Label::new(Some(tr("wifi.disconnect"))));
    let disconnect_wifi_btn = Button::builder()
        .child(&disconnect_box)
        .css_classes(["destructive-action"])
        .build();
    disconnect_wifi_btn.set_visible(false);

    wifi_actions_box.append(&switch_wifi_btn);
    wifi_actions_box.append(&disconnect_wifi_btn);

    let wifi_action_status = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Start)
        .wrap(true)
        .build();

    let facing_dropdown = DropDown::from_strings(&[tr("camera.back"), tr("camera.front")]);
    facing_dropdown.set_selected(config.lock().unwrap().camera_facing);

    let camera_warning = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Center)
        .build();

    if config.lock().unwrap().camera_facing == 1 {
        camera_warning.set_markup(&format!(
            "<span foreground='#ffa500' size='small'>{}</span>",
            esc(tr("camera.front_warning"))
        ));
    }

    let res_combo = ComboBoxText::new();

    let warning_label = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Center)
        .wrap(true)
        .build();

    let fps_dropdown = DropDown::from_strings(&["30", "60"]);
    fps_dropdown.set_selected(config.lock().unwrap().fps_index);

    let mic_check = CheckButton::with_label(tr("mic.block"));
    mic_check.set_active(config.lock().unwrap().block_mic);

    let button_box = Box::new(Orientation::Vertical, 10);
    let launch_row = Box::new(Orientation::Horizontal, 10);
    launch_row.set_homogeneous(true);

    let start_btn = Button::builder()
        .label(tr("stream.launch"))
        .css_classes(["suggested-action"])
        .build();
    let background_btn = Button::builder()
        .label(tr("stream.launch_background"))
        .tooltip_text(tr("stream.launch_background_tooltip"))
        .build();
    let stop_btn = Button::builder()
        .label(tr("stream.stop"))
        .css_classes(["destructive-action"])
        .build();

    launch_row.append(&start_btn);
    launch_row.append(&background_btn);
    button_box.append(&launch_row);
    button_box.append(&stop_btn);

    let status_label = Label::builder()
        .label(tr("status.ready"))
        .css_classes(["caption"])
        .wrap(true)
        .halign(gtk4::Align::Center)
        .build();

    controls_box.append(&device_label);
    controls_box.append(&device_combo);
    controls_box.append(&wifi_actions_box);
    controls_box.append(&wifi_action_status);
    controls_box.append(&Separator::new(Orientation::Horizontal));

    controls_box.append(
        &Label::builder()
            .label(tr("camera.label"))
            .halign(gtk4::Align::Start)
            .build(),
    );
    controls_box.append(&facing_dropdown);
    controls_box.append(&camera_warning);

    controls_box.append(
        &Label::builder()
            .label(tr("resolution.label"))
            .halign(gtk4::Align::Start)
            .build(),
    );
    controls_box.append(&res_combo);
    controls_box.append(&warning_label);

    controls_box.append(
        &Label::builder()
            .label(tr("fps.label"))
            .halign(gtk4::Align::Start)
            .build(),
    );
    controls_box.append(&fps_dropdown);
    controls_box.append(&mic_check);
    controls_box.append(&button_box);
    controls_box.append(&status_label);

    let scrolled_controls = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .child(&controls_box)
        .build();

    // ==========================================
    // WAITING SCREEN (with Wireless Option)
    // ==========================================
    let waiting_box = Box::new(Orientation::Vertical, 14);
    waiting_box.set_valign(gtk4::Align::Center);
    waiting_box.set_margin_all(24);

    let wait_icon = Label::builder()
        .use_markup(true)
        .label("<span size='xx-large'>📱</span>")
        .build();
    let wait_title = Label::builder()
        .use_markup(true)
        .label(format!("<b>{}</b>", esc(tr("waiting.title"))))
        .build();
    let wait_hint = Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .label(format!("<span size='small' foreground='gray'>{}</span>", esc(tr("waiting.hint"))))
        .build();

    let wifi_frame = Box::new(Orientation::Vertical, 8);
    let wifi_title = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Start)
        .label(format!("<b>{}</b>", esc(tr("waiting.wireless_title"))))
        .build();

    let wifi_entry_box = Box::new(Orientation::Horizontal, 8);
    let initial_ip = config.lock().unwrap().wireless_ip.clone();
    let ip_entry = Entry::builder()
        .placeholder_text(tr("waiting.ip_placeholder"))
        .hexpand(true)
        .text(&initial_ip)
        .build();
    let connect_btn = Button::builder()
        .label(tr("waiting.connect"))
        .css_classes(["suggested-action"])
        .build();

    wifi_entry_box.append(&ip_entry);
    wifi_entry_box.append(&connect_btn);

    let wifi_status_label = Label::builder()
        .use_markup(true)
        .wrap(true)
        .halign(gtk4::Align::Center)
        .build();

    let qr_pair_btn = Button::builder()
        .label(tr("waiting.qr_button"))
        .tooltip_text(tr("waiting.qr_tooltip"))
        .build();

    let wifi_note = Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .label(format!("<span size='x-small' foreground='gray'>{}</span>", esc(tr("waiting.tip"))))
        .build();

    wifi_frame.append(&wifi_title);
    wifi_frame.append(&wifi_entry_box);
    wifi_frame.append(&qr_pair_btn);
    wifi_frame.append(&wifi_status_label);
    wifi_frame.append(&wifi_note);

    waiting_box.append(&wait_icon);
    waiting_box.append(&wait_title);
    waiting_box.append(&wait_hint);
    waiting_box.append(&Separator::new(Orientation::Horizontal));
    waiting_box.append(&wifi_frame);

    // ==========================================
    // QR PAIRING DIALOG (Android 11+ Wireless Debugging)
    // ==========================================
    let qr_window = Window::builder()
        .title(tr("qr.title"))
        .transient_for(&window)
        .modal(true)
        .hide_on_close(true)
        .resizable(false)
        .default_width(340)
        .build();

    let qr_box = Box::new(Orientation::Vertical, 12);
    qr_box.set_margin_all(18);

    let qr_steps = Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .max_width_chars(36)
        .label(fill(
            &esc(tr("qr.steps")),
            &[("path", &format!("<b>{}</b>", esc(tr("qr.steps_path"))))],
        ))
        .build();

    let qr_code: Rc<RefCell<Option<QrCode>>> = Rc::new(RefCell::new(None));
    let qr_area = DrawingArea::builder()
        .content_width(260)
        .content_height(260)
        .halign(gtk4::Align::Center)
        .build();
    qr_area.set_draw_func(glib::clone!(@strong qr_code => move |_, cr, w, h| {
        draw_qr_code(cr, w, h, qr_code.borrow().as_ref());
    }));

    let qr_status_label = Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .max_width_chars(36)
        .build();

    let qr_note = Label::builder()
        .use_markup(true)
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .max_width_chars(40)
        .label(format!("<span size='x-small' foreground='gray'>{}</span>", esc(tr("qr.note"))))
        .build();

    let qr_close_btn = Button::builder().label(tr("qr.close")).build();

    qr_box.append(&qr_steps);
    qr_box.append(&qr_area);
    qr_box.append(&qr_status_label);
    qr_box.append(&qr_note);
    qr_box.append(&qr_close_btn);
    qr_window.set_child(Some(&qr_box));

    // Incremented whenever a pairing attempt starts or the dialog closes, so stale workers stop
    // and their late events are ignored.
    let qr_session = Arc::new(AtomicU64::new(0));

    stack.add_named(&waiting_box, Some("waiting"));
    stack.add_named(&scrolled_controls, Some("controls"));

    // ==========================================
    // EVENT HANDLERS & SIGNALS
    // ==========================================

    // Launch (with or without the scrcpy window)
    let apply_changes = glib::clone!(
        @weak facing_dropdown, @weak res_combo, @weak fps_dropdown, @weak mic_check, @weak status_label, @strong state, @strong config => move |background: bool| {
            let mut s = state.lock().unwrap();

            stop_stream(&mut s);
            thread::sleep(Duration::from_millis(500));

            if !Path::new("/dev/video9").exists() {
                let _ = Command::new("pkexec")
                    .args(["modprobe", "v4l2loopback", "video_nr=9", "card_label=Android-Webcam", "exclusive_caps=1"])
                    .status();
            }

            let facing = if facing_dropdown.selected() == 1 { "front" } else { "back" };
            let res = res_combo.active_text().map(|s| s.to_string()).unwrap_or_else(|| "1920x1080".into());
            let fps = if fps_dropdown.selected() == 1 { "60" } else { "30" };
            let mic_blocked = mic_check.is_active();

            // Save settings automatically on Launch. Not the resolution: picking one in the combo
            // already saves it, and the combo may be showing a crash fallback or a substitute for
            // a preferred size this phone lacks, neither of which should replace the preference.
            {
                let mut cfg = config.lock().unwrap();
                cfg.camera_facing = facing_dropdown.selected();
                cfg.fps_index = fps_dropdown.selected();
                cfg.block_mic = mic_blocked;
                save_config(&cfg);
            }

            let launch = LaunchParams {
                serial: s.active_serial.clone(),
                fps: fps.to_string(),
                facing: facing.to_string(),
                block_mic: mic_blocked,
                res,
                background,
                busy_retried: false,
            };
            let serial = launch.serial.as_deref();
            if let Some((child, output)) = run_scrcpy(&launch) {
                let mic_txt = if mic_blocked { tr("status.mic_off") } else { tr("status.mic_on") };
                let mode_txt = if let Some(ser) = serial {
                    if is_wireless_serial(ser) { " [Wi-Fi]" } else { " [USB]" }
                } else { "" };
                let bg_txt = if background { format!(" {}", tr("status.background")) } else { String::new() };
                status_label.set_markup(&format!("<span foreground='green'>{}{} {}{}</span>", esc(tr("status.active")), mode_txt, esc(mic_txt), esc(&bg_txt)));
                s.current_process = Some(child);
                s.current_output = output;
                s.current_launch = Some(launch);
            } else {
                status_label.set_markup(&format!("<span foreground='red'>{}</span>", esc(tr("status.failed"))));
            }
        }
    );

    let apply_shared = Rc::new(apply_changes);
    start_btn.connect_clicked(glib::clone!(@strong apply_shared => move |_| { (apply_shared)(false); }));
    background_btn.connect_clicked(glib::clone!(@strong apply_shared => move |_| { (apply_shared)(true); }));

    // Settings apply on their own: if a stream is running, relaunch it (same mode) shortly
    // after a change. The delay merges related changes, e.g. switching camera also reloads
    // the resolution list.
    let pending_restart: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let schedule_restart = Rc::new(glib::clone!(@strong state, @strong apply_shared, @strong pending_restart => move || {
        let Some(background) = state.lock().unwrap().current_launch.as_ref().map(|l| l.background) else {
            return; // nothing running: the new settings are just saved for next launch
        };
        if let Some(id) = pending_restart.borrow_mut().take() {
            id.remove();
        }
        let apply = apply_shared.clone();
        let pending = pending_restart.clone();
        let id = glib::timeout_add_local_once(Duration::from_millis(400), move || {
            pending.borrow_mut().take();
            (apply)(background);
        });
        *pending_restart.borrow_mut() = Some(id);
    }));


    // Camera facing changed
    facing_dropdown.connect_selected_notify(glib::clone!(
        @weak res_combo, @weak camera_warning, @strong config, @strong state, @strong is_updating_res, @strong schedule_restart => move |dd| {
            let facing = if dd.selected() == 1 {
                camera_warning.set_markup(&format!("<span foreground='#ffa500' size='small'>{}</span>", esc(tr("camera.front_warning"))));
                "front"
            } else {
                camera_warning.set_text("");
                "back"
            };

            {
                let mut cfg = config.lock().unwrap();
                cfg.camera_facing = dd.selected();
                save_config(&cfg);
            }

            let serial = state.lock().unwrap().active_serial.clone();
            let pref_res = config.lock().unwrap().resolution.clone();
            refresh_resolutions(&res_combo, facing, serial.as_deref(), Some(&pref_res), &is_updating_res);
            schedule_restart();
        }
    ));

    // Resolution changed
    res_combo.connect_changed(glib::clone!(
        @weak warning_label, @strong config, @strong is_updating_res, @strong schedule_restart => move |cb| {
            if let Some(res_str) = cb.active_text() {
                let res_s = res_str.to_string();
                if let Some(width_str) = res_s.split('x').next() {
                    if let Ok(width) = width_str.parse::<u32>() {
                        if width > 1920 {
                            warning_label.set_markup(&format!("<span foreground='#ffa500' size='small'>{}</span>", esc(tr("resolution.high_warning"))));
                        } else {
                            warning_label.set_text("");
                        }
                    }
                }

                // Only user picks: programmatic changes (list reloads, crash fallbacks) neither
                // save nor relaunch.
                if !is_updating_res.load(Ordering::SeqCst) && !res_s.is_empty() {
                    {
                        let mut cfg = config.lock().unwrap();
                        cfg.resolution = res_s;
                        save_config(&cfg);
                    }
                    schedule_restart();
                }
            }
        }
    ));

    // FPS changed
    fps_dropdown.connect_selected_notify(glib::clone!(@strong config, @strong schedule_restart => move |dd| {
        {
            let mut cfg = config.lock().unwrap();
            cfg.fps_index = dd.selected();
            save_config(&cfg);
        }
        schedule_restart();
    }));

    // Mic checkbox changed
    mic_check.connect_toggled(glib::clone!(@strong config, @strong schedule_restart => move |btn| {
        {
            let mut cfg = config.lock().unwrap();
            cfg.block_mic = btn.is_active();
            save_config(&cfg);
        }
        schedule_restart();
    }));

    // Multiple devices selection dropdown changed
    device_combo.connect_changed(glib::clone!(
        @strong state, @strong config, @strong is_updating_res, @weak device_label, @weak facing_dropdown, @weak res_combo, @weak switch_wifi_btn, @weak disconnect_wifi_btn => move |cb| {
            if let Some(serial) = cb.active_id() {
                let serial_str = serial.to_string();
                let mut s = state.lock().unwrap();
                s.active_serial = Some(serial_str.clone());

                let is_wireless = is_wireless_serial(&serial_str);
                switch_wifi_btn.set_visible(!is_wireless);
                disconnect_wifi_btn.set_visible(is_wireless);

                let model = s.devices.iter().find(|d| d.serial == serial_str).map(|d| d.model.clone()).unwrap_or_else(|| serial_str.clone());
                let badge = if is_wireless {
                    wifi_badge(&serial_str)
                } else {
                    "<span foreground='#2ec27e'>[USB]</span>".to_string()
                };
                device_label.set_markup(&format!("<b>{}</b> {} {}", esc(tr("device.label")), esc(&model), badge));

                let facing = if facing_dropdown.selected() == 1 { "front" } else { "back" };
                let pref_res = config.lock().unwrap().resolution.clone();
                refresh_resolutions(&res_combo, facing, Some(&serial_str), Some(&pref_res), &is_updating_res);
            }
        }
    ));

    // Connect Wi-Fi button in waiting screen
    let do_connect_wifi = glib::clone!(
        @strong tx, @weak ip_entry, @weak connect_btn, @weak wifi_status_label => move || {
            let ip_text = ip_entry.text().trim().to_string();
            if ip_text.is_empty() {
                wifi_status_label.set_markup(&format!("<span foreground='#ffa500'>{}</span>", esc(tr("waiting.enter_ip"))));
                return;
            }
            connect_btn.set_sensitive(false);
            wifi_status_label.set_markup(&format!("<span foreground='#3584e4'>{}</span>", esc(tr("waiting.connecting"))));
            let tx_clone = tx.clone();
            thread::spawn(move || {
                match adb_connect_wireless(&ip_text) {
                    Ok(connected_addr) => {
                        let _ = tx_clone.send(AppEvent::WirelessConnectStatus {
                            success: true,
                            message: format!("<span foreground='green'>{}</span>", esc(&trf("connect.success", &[("addr", &connected_addr)]))),
                            ip: Some(connected_addr),
                        });
                    }
                    Err(err) => {
                        let _ = tx_clone.send(AppEvent::WirelessConnectStatus {
                            success: false,
                            message: format!("<span foreground='red'>Failed: {}</span>", esc(&err)),
                            ip: None,
                        });
                    }
                }
            });
        }
    );

    connect_btn
        .connect_clicked(glib::clone!(@strong do_connect_wifi => move |_| { do_connect_wifi(); }));
    ip_entry
        .connect_activate(glib::clone!(@strong do_connect_wifi => move |_| { do_connect_wifi(); }));

    // Pair with QR code button in waiting screen
    qr_pair_btn.connect_clicked(glib::clone!(
        @strong tx, @strong qr_session, @strong qr_code, @weak qr_window, @weak qr_area, @weak qr_status_label => move |_| {
            let session = qr_session.fetch_add(1, Ordering::SeqCst) + 1;
            let name = format!("android-webcam-{}", random_token(6));
            let password = random_token(10);

            *qr_code.borrow_mut() = QrCode::new(adb_qr_payload(&name, &password)).ok();
            qr_area.queue_draw();
            qr_status_label.set_markup(&format!("<span foreground='#3584e4'>{}</span>", esc(tr("qr.waiting_scan"))));
            qr_window.present();

            let tx_clone = tx.clone();
            let qr_session = qr_session.clone();
            thread::spawn(move || {
                let send = |message: String, done: bool, connected_addr: Option<String>| {
                    let _ = tx_clone.send(AppEvent::QrPairStatus { session, message, done, connected_addr });
                };
                let result = qr_pair_and_connect(
                    &name,
                    &password,
                    || qr_session.load(Ordering::SeqCst) != session,
                    |msg| send(format!("<span foreground='#3584e4'>{}</span>", esc(msg)), false, None),
                );
                match result {
                    Ok(addr) => send(
                        format!("<span foreground='green'>{}</span>", esc(&trf("connect.success", &[("addr", &addr)]))),
                        true,
                        Some(addr),
                    ),
                    Err(err) => send(format!("<span foreground='red'>Failed: {}</span>", esc(&err)), true, None),
                }
            });
        }
    ));

    qr_close_btn.connect_clicked(glib::clone!(@weak qr_window => move |_| qr_window.close()));

    qr_window.connect_close_request(glib::clone!(@strong qr_session => move |_| {
        qr_session.fetch_add(1, Ordering::SeqCst);
        glib::Propagation::Proceed
    }));

    // Switch to Wi-Fi button in controls screen
    switch_wifi_btn.connect_clicked(glib::clone!(
        @strong tx, @strong state, @weak switch_wifi_btn, @weak wifi_action_status => move |_| {
            let active_serial = {
                let s = state.lock().unwrap();
                s.active_serial.clone()
            };
            if let Some(serial) = active_serial {
                switch_wifi_btn.set_sensitive(false);
                wifi_action_status.set_markup(&format!("<span foreground='#3584e4'>{}</span>", esc(tr("wifi.switching"))));
                let tx_clone = tx.clone();
                thread::spawn(move || {
                    match switch_device_to_wifi(&serial) {
                        Ok(ip_port) => {
                            let _ = tx_clone.send(AppEvent::SwitchToWirelessStatus {
                                success: true,
                                message: format!("<span foreground='green'>{}</span>", esc(&trf("wifi.switched", &[("addr", &ip_port)]))),
                                ip: Some(ip_port),
                            });
                        }
                        Err(err) => {
                            let _ = tx_clone.send(AppEvent::SwitchToWirelessStatus {
                                success: false,
                                message: format!("<span foreground='red'>Failed: {}</span>", esc(&err)),
                                ip: None,
                            });
                        }
                    }
                });
            }
        }
    ));

    // Disconnect Wi-Fi button in controls screen
    disconnect_wifi_btn.connect_clicked(glib::clone!(
        @strong state, @weak wifi_action_status, @weak status_label => move |_| {
            let active_serial = {
                let mut s = state.lock().unwrap();
                stop_stream(&mut s);
                status_label.set_text(tr("status.stopped"));
                s.active_serial.clone()
            };
            if let Some(serial) = active_serial {
                if is_wireless_serial(&serial) {
                    adb_disconnect(&serial);
                    wifi_action_status.set_markup(&format!("<span foreground='gray'>{}</span>", esc(tr("wifi.disconnected"))));
                }
            }
        }
    ));

    // Stop button
    stop_btn.connect_clicked(glib::clone!(@strong state, @weak status_label => move |_| {
        if let Ok(mut s) = state.lock() {
            stop_stream(&mut s);
            status_label.set_text(tr("status.stopped"));
        }
    }));

    // Clean exit on window close
    window.connect_close_request(glib::clone!(@strong state => move |_| {
        if let Ok(mut s) = state.lock() {
            stop_stream(&mut s);
        }
        glib::Propagation::Proceed
    }));

    // Crash watchdog: if scrcpy exits on its own, find out why from its output. A busy camera
    // gets one retry at the same resolution; anything else that isn't a camera error (e.g. the
    // phone can't encode 4K) relaunches at 1080p, then 720p. A user Stop takes current_process
    // first, so it isn't seen as a crash.
    glib::timeout_add_local(Duration::from_millis(500), glib::clone!(
        @weak status_label, @weak res_combo, @strong state, @strong is_updating_res
            => @default-return glib::ControlFlow::Break, move || {
            let mut s = state.lock().unwrap();
            let exit = match s.current_process.as_mut().map(|child| child.try_wait()) {
                Some(Ok(Some(status))) => Some(status.success()),
                Some(Err(_)) => Some(false),
                _ => None,
            };
            let Some(clean_exit) = exit else { return glib::ControlFlow::Continue };
            s.current_process = None;
            let Some(launch) = s.current_launch.take() else { return glib::ControlFlow::Continue };

            // Exit code 0 means the user closed the scrcpy window: that's a stop, not a crash.
            if clean_exit {
                status_label.set_text(tr("status.stopped"));
                return glib::ControlFlow::Continue;
            }

            // Phone unplugged or disconnected: the device monitor handles that, don't retry.
            if let Some(ser) = launch.serial.as_deref() {
                let online = Command::new("adb")
                    .args(["-s", ser, "get-state"])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "device")
                    .unwrap_or(false);
                if !online {
                    return glib::ControlFlow::Continue;
                }
            }

            let output = s.current_output.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
            let fail = |msg: &str| status_label.set_markup(&format!("<span foreground='red'>{}</span>", esc(msg)));

            let (retry, notice) = match crash_cause(&output) {
                CrashCause::CameraBlocked => {
                    fail(tr("status.camera_blocked"));
                    return glib::ControlFlow::Continue;
                }
                CrashCause::CameraBusy if launch.busy_retried => {
                    fail(tr("status.camera_busy"));
                    return glib::ControlFlow::Continue;
                }
                CrashCause::CameraBusy => {
                    // Often our own previous server still holding the camera: make sure it's gone.
                    stop_phone_server(launch.serial.as_deref());
                    (LaunchParams { busy_retried: true, ..launch.clone() }, tr("status.camera_busy_retry").to_string())
                }
                CrashCause::Other => {
                    let Some(fallback) = next_fallback_resolution(&launch.res) else {
                        fail(&trf("status.crashed", &[("res", &launch.res)]));
                        return glib::ControlFlow::Continue;
                    };
                    let notice = trf("status.relaunched", &[("res", &launch.res), ("fallback", fallback)]);
                    (LaunchParams { res: fallback.to_string(), ..launch.clone() }, notice)
                }
            };

            let fallback = (retry.res != launch.res).then(|| retry.res.clone());
            match run_scrcpy(&retry) {
                Some((child, output)) => {
                    s.current_process = Some(child);
                    s.current_output = output;
                    s.current_launch = Some(retry);
                    status_label.set_markup(&format!("<span foreground='#ffa500'>{}</span>", esc(&notice)));
                }
                None => fail(tr("status.failed")),
            }
            drop(s);

            let Some(fallback) = fallback else { return glib::ControlFlow::Continue };

            // Show the resolution actually in use, without saving it as the preferred one.
            is_updating_res.store(true, Ordering::SeqCst);
            res_combo.set_active_id(Some(&fallback));
            is_updating_res.store(false, Ordering::SeqCst);
            glib::ControlFlow::Continue
        }
    ));

    // Background ADB Device Detection Thread
    let tx_monitor = tx.clone();
    thread::spawn(move || {
        let mut last_devices: Vec<DeviceInfo> = Vec::new();
        loop {
            let current_devices = list_connected_devices();
            if current_devices != last_devices {
                let _ = tx_monitor.send(AppEvent::DevicesChanged(current_devices.clone()));
                last_devices = current_devices;
            }
            thread::sleep(Duration::from_millis(1500));
        }
    });

    // Main context receiver for background events
    rx.attach(
        None,
        glib::clone!(
            @weak stack,
            @weak device_label,
            @weak device_combo,
            @weak switch_wifi_btn,
            @weak disconnect_wifi_btn,
            @weak wifi_action_status,
            @weak wifi_status_label,
            @weak connect_btn,
            @weak ip_entry,
            @weak qr_window,
            @weak qr_status_label,
            @strong qr_session,
            @weak facing_dropdown,
            @weak res_combo,
            @weak status_label,
            @strong state,
            @strong config,
            @strong is_updating_res => @default-return glib::ControlFlow::Continue, move |event| {
                match event {
                    AppEvent::DevicesChanged(devices) => {
                        let mut s = state.lock().unwrap();
                        s.devices = devices.clone();

                        if devices.is_empty() {
                            if let Some(mut child) = s.current_process.take() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                            s.active_serial = None;
                            stack.set_visible_child_name("waiting");
                            status_label.set_text(tr("status.waiting_device"));
                        } else {
                            // Pick active device: keep previous if still present, otherwise choose first
                            let active = if let Some(ref current) = s.active_serial {
                                devices.iter().find(|d| &d.serial == current).cloned()
                            } else {
                                None
                            };

                            let selected = active.unwrap_or_else(|| devices[0].clone());
                            let serial_changed = s.active_serial.as_ref() != Some(&selected.serial);
                            s.active_serial = Some(selected.serial.clone());
                            // Release before touching device_combo: set_active_id fires its
                            // "changed" handler, which locks `state` again and would deadlock.
                            drop(s);

                            // Populate device selector if multiple devices
                            if devices.len() > 1 {
                                device_combo.remove_all();
                                for d in &devices {
                                    let tag = if d.is_wireless { " [Wi-Fi]" } else { " [USB]" };
                                    device_combo.append(Some(&d.serial), &format!("{}{}", d.model, tag));
                                }
                                device_combo.set_active_id(Some(&selected.serial));
                                device_combo.set_visible(true);
                            } else {
                                device_combo.set_visible(false);
                            }

                            // Update device label badge
                            let badge = if selected.is_wireless {
                                wifi_badge(&selected.serial)
                            } else {
                                "<span foreground='#2ec27e'>[USB]</span>".to_string()
                            };
                            device_label.set_markup(&format!("<b>{}</b> {} {}", esc(tr("device.label")), esc(&selected.model), badge));

                            // Controls visibility
                            switch_wifi_btn.set_visible(!selected.is_wireless);
                            disconnect_wifi_btn.set_visible(selected.is_wireless);

                            if stack.visible_child_name().as_deref() != Some("controls") {
                                stack.set_visible_child_name("controls");
                                status_label.set_text(tr("status.stopped"));
                            }

                            // Refresh resolutions if device changed or combo is empty
                            if serial_changed || res_combo.active_text().is_none() {
                                let facing = if facing_dropdown.selected() == 1 { "front" } else { "back" };
                                let pref_res = config.lock().unwrap().resolution.clone();
                                refresh_resolutions(&res_combo, facing, Some(&selected.serial), Some(&pref_res), &is_updating_res);
                            }
                        }
                    }
                    AppEvent::WirelessConnectStatus { success, message, ip } => {
                        connect_btn.set_sensitive(true);
                        wifi_status_label.set_markup(&message);
                        if success {
                            if let Some(ip_addr) = ip {
                                let mut cfg = config.lock().unwrap();
                                cfg.wireless_ip = ip_addr;
                                save_config(&cfg);
                            }
                        }
                    }
                    AppEvent::QrPairStatus { session, message, done, connected_addr } => {
                        if session != qr_session.load(Ordering::SeqCst) {
                            return glib::ControlFlow::Continue;
                        }
                        qr_status_label.set_markup(&message);
                        if done {
                            if let Some(addr) = connected_addr {
                                ip_entry.set_text(&addr);
                                wifi_status_label.set_markup(&message);
                                {
                                    let mut cfg = config.lock().unwrap();
                                    cfg.wireless_ip = addr;
                                    save_config(&cfg);
                                }
                                qr_window.close();
                            }
                        }
                    }
                    AppEvent::SwitchToWirelessStatus { success, message, ip } => {
                        switch_wifi_btn.set_sensitive(true);
                        wifi_action_status.set_markup(&message);
                        if success {
                            if let Some(ip_addr) = ip {
                                let mut cfg = config.lock().unwrap();
                                cfg.wireless_ip = ip_addr;
                                save_config(&cfg);
                            }
                        }
                    }
                }
                glib::ControlFlow::Continue
            }
        ),
    );

    // Discreet donate link, bottom left on every screen.
    let donate_box = Box::new(Orientation::Horizontal, 6);
    // A text heart instead of an icon name: always renders, whatever icon theme the host has.
    let heart = Label::new(Some("♥"));
    heart.add_css_class("donate-heart");
    donate_box.append(&heart);
    donate_box.append(&Label::new(Some(tr("donate.button"))));
    let donate_btn = Button::builder()
        .child(&donate_box)
        .halign(gtk4::Align::Start)
        .tooltip_text(DONATE_URL)
        .css_classes(["flat", "donate-btn"])
        .build();
    let w_c = window.clone();
    donate_btn.connect_clicked(move |_| open_url(&w_c, DONATE_URL));
    let footer = Box::new(Orientation::Horizontal, 0);
    footer.set_margin_start(8);
    footer.set_margin_bottom(8);
    footer.append(&donate_btn);

    stack.set_vexpand(true);
    let root = Box::new(Orientation::Vertical, 0);
    root.append(&stack);
    root.append(&footer);
    window.set_child(Some(&root));
    window.show();
}

/// Open a link in the user's default browser. Asks the desktop portal first:
/// it runs in the real desktop session, so it finds the browser even when GTK's
/// own lookup can't (it can end up opening the page in a text editor). Falls
/// back to xdg-open, and as a last resort shows the link so it can be copied.
fn open_url(window: &ApplicationWindow, url: &'static str) {
    let bus = match gtk4::gio::bus_get_sync(gtk4::gio::BusType::Session, None::<&gtk4::gio::Cancellable>) {
        Ok(b) => b,
        Err(_) => {
            open_url_xdg(window, url);
            return;
        }
    };
    // OpenURI(s parent_window, s uri, a{sv} options)
    let params = glib::Variant::tuple_from_iter([
        "".to_variant(),
        url.to_variant(),
        glib::VariantDict::new(None).end(),
    ]);
    let w = window.clone();
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
                open_url_xdg(&w, url);
            }
        },
    );
}

fn open_url_xdg(window: &ApplicationWindow, url: &'static str) {
    let w = window.clone();
    let spawned = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawned {
        Ok(c) => Some(c),
        Err(_) => {
            show_link_window(&w, url);
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
                    show_link_window(&w, url);
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
fn show_link_window(parent: &ApplicationWindow, url: &str) {
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

/// Network devices show up in `adb devices` either as `ip:port` (adb connect)
/// or, when Android 11+ wireless debugging reconnects on its own via mDNS, as
/// `adb-<serial>-<id>._adb-tls-connect._tcp` (no ':' at all).
pub fn is_wireless_serial(serial: &str) -> bool {
    serial.contains(':')
        || serial.ends_with("._adb-tls-connect._tcp")
        || serial.ends_with("._adb._tcp")
}

/// Badge for a wireless device: shows the address when there is one; mDNS
/// names are long and meaningless to users, so those get a plain [Wi-Fi].
fn wifi_badge(serial: &str) -> String {
    if serial.contains(':') {
        format!("<span foreground='#3584e4'>[Wi-Fi: {}]</span>", esc(serial))
    } else {
        "<span foreground='#3584e4'>[Wi-Fi]</span>".to_string()
    }
}

pub fn parse_devices_text(text: &str) -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("List of devices") || line.starts_with("*") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] == "device" {
            let serial = parts[0].to_string();
            let is_wireless = is_wireless_serial(&serial);
            let mut model = String::new();
            for p in &parts[2..] {
                if let Some(m) = p.strip_prefix("model:") {
                    model = m.replace('_', " ");
                    break;
                }
            }
            devices.push(DeviceInfo {
                serial,
                model,
                is_wireless,
            });
        }
    }
    devices
}

fn list_connected_devices() -> Vec<DeviceInfo> {
    let output = Command::new("adb").args(["devices", "-l"]).output();
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut devices = parse_devices_text(&text);
        // Fallback for models if not populated by adb devices -l
        for d in &mut devices {
            if d.model.is_empty() {
                if let Ok(m_out) = Command::new("adb")
                    .args(["-s", &d.serial, "shell", "getprop", "ro.product.model"])
                    .output()
                {
                    d.model = String::from_utf8_lossy(&m_out.stdout).trim().to_string();
                }
            }
            if d.model.is_empty() {
                let key = if d.is_wireless { "device.wireless_fallback" } else { "device.usb_fallback" };
                d.model = trf(key, &[("serial", &d.serial)]);
            }
        }
        devices
    } else {
        Vec::new()
    }
}

pub fn parse_ip_from_ip_output(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("inet ") {
            if let Some(ip_with_mask) = trimmed.split_whitespace().nth(1) {
                let ip = ip_with_mask.split('/').next().unwrap_or("");
                if !ip.is_empty() && ip != "127.0.0.1" {
                    return Some(ip.to_string());
                }
            }
        }
        if trimmed.contains("src ") {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if let Some(pos) = parts.iter().position(|&p| p == "src") {
                if let Some(ip) = parts.get(pos + 1) {
                    if !ip.is_empty() && *ip != "127.0.0.1" && ip.contains('.') {
                        return Some(ip.to_string());
                    }
                }
            }
        }
    }
    None
}

fn get_device_wlan_ip(serial: &str) -> Option<String> {
    if let Ok(out) = Command::new("adb")
        .args([
            "-s", serial, "shell", "ip", "-f", "inet", "addr", "show", "wlan0",
        ])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(ip) = parse_ip_from_ip_output(&text) {
            return Some(ip);
        }
    }
    if let Ok(out) = Command::new("adb")
        .args(["-s", serial, "shell", "ip", "route"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        if let Some(ip) = parse_ip_from_ip_output(&text) {
            return Some(ip);
        }
    }
    if let Ok(out) = Command::new("adb")
        .args(["-s", serial, "shell", "getprop", "dhcp.wlan0.ipaddress"])
        .output()
    {
        let ip = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !ip.is_empty() && ip.contains('.') {
            return Some(ip);
        }
    }
    None
}

fn switch_device_to_wifi(serial: &str) -> Result<String, String> {
    let ip = get_device_wlan_ip(serial).ok_or_else(|| {
        tr("error.no_wifi_ip").to_string()
    })?;

    let tcp_out = Command::new("adb")
        .args(["-s", serial, "tcpip", "5555"])
        .output()
        .map_err(|e| trf("error.tcpip_run", &[("error", &e.to_string())]))?;

    if !tcp_out.status.success() {
        let err = String::from_utf8_lossy(&tcp_out.stderr);
        return Err(trf("error.tcpip", &[("error", &err)]));
    }

    thread::sleep(Duration::from_millis(1500));

    let target = format!("{}:5555", ip);
    adb_connect_wireless(&target)
}

fn adb_connect_wireless(target: &str) -> Result<String, String> {
    let mut address = target.trim().to_string();
    if !address.contains(':') {
        address.push_str(":5555");
    }

    let out = Command::new("adb")
        .args(["connect", &address])
        .output()
        .map_err(|e| trf("error.connect_run", &[("error", &e.to_string())]))?;

    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    let combined = format!("{} {}", stdout, stderr).trim().to_string();

    if combined.contains("connected to")
        && !combined.contains("failed")
        && !combined.contains("unable")
    {
        Ok(address)
    } else {
        Err(if combined.is_empty() {
            tr("error.connect").to_string()
        } else {
            combined
        })
    }
}

/// Payload Android's "Pair device with QR code" scanner expects.
pub fn adb_qr_payload(name: &str, password: &str) -> String {
    format!("WIFI:T:ADB;S:{};P:{};;", name, password)
}

/// Random lowercase alphanumeric token. The 32-char alphabet keeps the byte mapping unbiased and
/// contains no characters that need escaping in the QR payload.
fn random_token(len: usize) -> String {
    const CHARSET: &[u8] = b"abcdefghijkmnpqrstuvwxyz23456789";
    let mut bytes = vec![0u8; len];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        use std::hash::{BuildHasher, RandomState};
        let state = RandomState::new();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = state.hash_one(i) as u8;
        }
    }
    bytes
        .iter()
        .map(|b| CHARSET[*b as usize % CHARSET.len()] as char)
        .collect()
}

fn draw_qr_code(cr: &gtk4::cairo::Context, width: i32, height: i32, code: Option<&QrCode>) {
    cr.set_source_rgb(1.0, 1.0, 1.0);
    let _ = cr.paint();
    let Some(code) = code else { return };

    const QUIET_ZONE: usize = 4;
    let modules = code.width();
    let total = (modules + 2 * QUIET_ZONE) as f64;
    let scale = (f64::from(width.min(height)) / total).floor().max(1.0);
    let offset_x = ((f64::from(width) - scale * total) / 2.0).floor();
    let offset_y = ((f64::from(height) - scale * total) / 2.0).floor();

    cr.set_source_rgb(0.0, 0.0, 0.0);
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let x = (i % modules + QUIET_ZONE) as f64;
            let y = (i / modules + QUIET_ZONE) as f64;
            cr.rectangle(offset_x + x * scale, offset_y + y * scale, scale, scale);
        }
    }
    let _ = cr.fill();
}

/// Waits for the phone that scanned our QR code, pairs with it, then connects to it.
/// Discovery is done in-process because many distro adb builds ship without mDNS support.
fn qr_pair_and_connect(
    name: &str,
    password: &str,
    cancelled: impl Fn() -> bool,
    progress: impl Fn(&str),
) -> Result<String, String> {
    let mdns =
        ServiceDaemon::new().map_err(|e| trf("error.mdns_start", &[("error", &e.to_string())]))?;

    let result = (|| {
        let instance_prefix = format!("{}.", name);
        let pair_addr = wait_for_mdns_service(
            &mdns,
            ADB_PAIRING_SERVICE,
            QR_PAIR_TIMEOUT,
            &cancelled,
            |fullname, _| fullname.starts_with(&instance_prefix),
        )
        .map_err(|e| match e {
            MdnsWaitError::TimedOut => tr("error.pair_timeout").to_string(),
            other => other.to_string(),
        })?;

        progress(tr("qr.pairing"));
        adb_pair(&pair_addr.to_string(), password)?;

        progress(tr("qr.paired"));
        let phone_ip = pair_addr.ip();
        let connect_addr = wait_for_mdns_service(
            &mdns,
            ADB_CONNECT_SERVICE,
            QR_CONNECT_TIMEOUT,
            &cancelled,
            |_, addr| addr.ip() == phone_ip,
        )
        .map_err(|e| match e {
            MdnsWaitError::TimedOut => tr("error.connect_port_timeout").to_string(),
            other => other.to_string(),
        })?;

        adb_connect_wireless(&connect_addr.to_string())
    })();

    let _ = mdns.shutdown();
    result
}

enum MdnsWaitError {
    TimedOut,
    Cancelled,
    Failed(String),
}

impl std::fmt::Display for MdnsWaitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MdnsWaitError::TimedOut => write!(f, "{}", tr("error.timed_out")),
            MdnsWaitError::Cancelled => write!(f, "{}", tr("error.cancelled")),
            MdnsWaitError::Failed(e) => write!(f, "{}", e),
        }
    }
}

fn wait_for_mdns_service(
    mdns: &ServiceDaemon,
    service_type: &str,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
    matches: impl Fn(&str, &SocketAddr) -> bool,
) -> Result<SocketAddr, MdnsWaitError> {
    let receiver = mdns
        .browse(service_type)
        .map_err(|e| MdnsWaitError::Failed(trf("error.mdns_browse", &[("error", &e.to_string())])))?;
    let deadline = Instant::now() + timeout;

    let result = 'wait: loop {
        if cancelled() {
            break Err(MdnsWaitError::Cancelled);
        }
        if Instant::now() >= deadline {
            break Err(MdnsWaitError::TimedOut);
        }
        match receiver.recv_timeout(Duration::from_millis(300)) {
            Ok(ServiceEvent::ServiceResolved(info)) => {
                for ip in info.get_addresses_v4() {
                    let addr = SocketAddr::from((ip, info.get_port()));
                    if matches(info.get_fullname(), &addr) {
                        break 'wait Ok(addr);
                    }
                }
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err(MdnsWaitError::Failed(tr("error.mdns_stopped").into()));
            }
        }
    };

    let _ = mdns.stop_browse(service_type);
    result
}

fn adb_pair(target: &str, password: &str) -> Result<(), String> {
    let out = Command::new("adb")
        .args(["pair", target, password])
        .output()
        .map_err(|e| trf("error.pair_run", &[("error", &e.to_string())]))?;

    let combined = format!(
        "{} {}",
        String::from_utf8_lossy(&out.stdout).trim(),
        String::from_utf8_lossy(&out.stderr).trim()
    )
    .trim()
    .to_string();

    if combined.contains("Successfully paired") {
        Ok(())
    } else if combined.is_empty() {
        Err(tr("error.pair").to_string())
    } else {
        Err(combined)
    }
}

fn adb_disconnect(target: &str) {
    let _ = Command::new("adb").args(["disconnect", target]).output();
}

fn refresh_resolutions(
    combo: &ComboBoxText,
    facing: &str,
    serial: Option<&str>,
    preferred_res: Option<&str>,
    is_updating: &AtomicBool,
) {
    is_updating.store(true, Ordering::SeqCst);
    combo.remove_all();

    let mut args = vec![
        "--video-source=camera".to_string(),
        format!("--camera-facing={}", facing),
        "--list-camera-sizes".to_string(),
    ];
    if let Some(s) = serial {
        if !s.is_empty() {
            args.push(format!("--serial={}", s));
        }
    }

    let output = Command::new("scrcpy").args(&args).output();

    let standards = ["3840x2160", "2560x1440", "1920x1080", "1280x720", "720x480"];
    let mut found_sizes = Vec::new();

    if let Ok(out) = output {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let target_id = if facing == "back" {
            "--camera-id=0"
        } else {
            "--camera-id=1"
        };
        let mut inside_target_block = false;

        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("--camera-id=") {
                inside_target_block = trimmed.contains(target_id);
                continue;
            }
            if inside_target_block && trimmed.starts_with("- ") {
                let size = trimmed.trim_start_matches("- ").trim();
                if standards.contains(&size) {
                    found_sizes.push(size.to_string());
                }
            }
        }

        if found_sizes.is_empty() {
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("- ") {
                    let size = trimmed.trim_start_matches("- ").trim();
                    if standards.contains(&size) {
                        found_sizes.push(size.to_string());
                    }
                }
            }
        }
    }

    if found_sizes.is_empty() {
        for s in &standards {
            found_sizes.push(s.to_string());
        }
    }

    found_sizes.sort_by_key(|s| {
        s.split('x')
            .next()
            .unwrap_or("0")
            .parse::<u32>()
            .unwrap_or(0)
    });
    found_sizes.reverse();
    found_sizes.dedup();

    for s in &found_sizes {
        combo.append(Some(s), s);
    }

    let target = preferred_res.unwrap_or("1920x1080");
    let default_idx = found_sizes
        .iter()
        .position(|r| r == target)
        .unwrap_or_else(|| {
            found_sizes
                .iter()
                .position(|r| r == "1920x1080")
                .unwrap_or(0)
        });
    if !found_sizes.is_empty() {
        combo.set_active(Some(default_idx as u32));
    }

    is_updating.store(false, Ordering::SeqCst);
}

/// Next resolution to try after a crash: anything above 1080p drops to 1080p, then 720p.
pub fn next_fallback_resolution(res: &str) -> Option<&'static str> {
    let (w, h) = res.split_once('x')?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    if w > 1920 || h > 1080 {
        Some("1920x1080")
    } else if w > 1280 || h > 720 {
        Some("1280x720")
    } else {
        None
    }
}

/// Tells from scrcpy's output why it exited. Matches Android's CameraAccessException messages
/// (and the camera service's log text), which the phone server prints before closing.
pub fn crash_cause(output: &str) -> CrashCause {
    const BUSY: &[&str] = &[
        "too many cameras already open",
        "limit for number of open cameras",
        "higher-priority client using camera",
        "camera device is in use",
        "camera_in_use",
        "max_cameras_in_use",
    ];
    const BLOCKED: &[&str] = &[
        "camera is disabled",
        "camera_disabled",
        "disabled due to a device policy",
        "sensor privacy",
    ];
    let out = output.to_lowercase();
    if BUSY.iter().any(|p| out.contains(p)) {
        CrashCause::CameraBusy
    } else if BLOCKED.iter().any(|p| out.contains(p)) {
        CrashCause::CameraBlocked
    } else {
        CrashCause::Other
    }
}

/// Keeps the last SCRCPY_OUTPUT_LINES lines of `r`. Also keeps the pipe drained so scrcpy never
/// blocks on a full buffer.
fn collect_output(r: impl Read + Send + 'static, output: ScrcpyOutput) {
    thread::spawn(move || {
        for line in BufReader::new(r).lines().map_while(Result::ok) {
            let mut out = output.lock().unwrap();
            if out.len() >= SCRCPY_OUTPUT_LINES {
                out.pop_front();
            }
            out.push_back(line);
        }
    });
}

fn run_scrcpy(launch: &LaunchParams) -> Option<(Child, ScrcpyOutput)> {
    let mut args = vec![
        "--video-source=camera".into(),
        format!("--camera-facing={}", launch.facing),
        format!("--camera-size={}", launch.res),
        format!("--camera-fps={}", launch.fps),
        "--v4l2-sink=/dev/video9".into(),
        "--v4l2-buffer=0".into(),
    ];
    if launch.background {
        args.push("--no-window".into());
    }

    if let Some(s) = launch.serial.as_deref() {
        if !s.is_empty() {
            args.push(format!("--serial={}", s));
        }
    }

    if launch.block_mic {
        args.push("--no-audio".into());
    } else {
        args.push("--audio-source=mic".into());
        args.push("--audio-buffer=50".into());
        args.push("--audio-output-buffer=50".into());
    }

    let mut cmd = Command::new("scrcpy");
    // Give the scrcpy window our app id so the taskbar groups it under our icon instead of
    // showing a separate scrcpy entry (SDL3 uses this for the Wayland app_id / X11 WM_CLASS).
    cmd.env("SDL_APP_ID", APP_ID);
    if Path::new(APP_ICON_PATH).exists() {
        cmd.env("SCRCPY_ICON_PATH", APP_ICON_PATH);
    }
    let mut child = cmd
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // stdout carries the phone server's log (where camera errors appear), stderr the client's.
    let output = ScrcpyOutput::default();
    if let Some(out) = child.stdout.take() {
        collect_output(out, output.clone());
    }
    if let Some(err) = child.stderr.take() {
        collect_output(err, output.clone());
    }
    Some((child, output))
}

/// Stops the running stream: the scrcpy client here and its server on the phone.
fn stop_stream(s: &mut AppState) {
    if let Some(mut child) = s.current_process.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    s.current_launch = None;
    kill_scrcpy();
    stop_phone_server(s.active_serial.as_deref());
}

/// Kills scrcpy's server on the phone and waits (up to 2 s) for it to exit, so the camera is
/// free for the next launch. The server runs as the shell user through app_process, not as an
/// app, so `am force-stop` can't reach it. `[c]om` keeps the pattern from matching the
/// `sh -c` running this very command.
fn stop_phone_server(serial: Option<&str>) {
    const PATTERN: &str = "'[c]om.genymobile.scrcpy.Server'";
    let script = format!(
        "pkill -f {p}; for i in 1 2 3 4 5 6 7 8 9 10; do pgrep -f {p} >/dev/null || break; sleep 0.2; done",
        p = PATTERN
    );
    let mut cmd = Command::new("adb");
    if let Some(s) = serial {
        cmd.args(["-s", s]);
    }
    let _ = cmd
        .args(["shell", &script])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Text interpolated into Pango markup must be escaped, or '&', '<' etc. break parsing.
fn esc(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}

/// `killall` prints "scrcpy: no process found" when nothing is running, so silence it.
fn kill_scrcpy() {
    let _ = Command::new("killall")
        .args(["-9", "scrcpy"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Breeze's GTK4 theme gives scrollbar sliders -9px margins around a 6px+10px box, so GTK
/// measures them at -2px and warns ("GtkGizmo (slider) reported min width -2"). Trim the
/// margins just enough to measure 0px; the slider looks the same.
fn install_theme_workarounds() {
    let Some(display) = gdk::Display::default() else { return };
    let theme = gtk4::Settings::for_display(&display)
        .gtk_theme_name()
        .map(|t| t.to_string())
        .unwrap_or_default();
    if !theme.to_lowercase().starts_with("breeze") {
        return;
    }
    let provider = CssProvider::new();
    provider.load_from_data(
        "scrollbar.vertical slider { margin-left: -8px; margin-right: -8px; }
         scrollbar.horizontal slider { margin-top: -8px; margin-bottom: -8px; }",
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Icons we draw ourselves (no theme has a USB symbol everywhere). Written to the cache dir
/// and added to GTK's icon search path; the -symbolic name makes GTK recolor it to match
/// the theme's text color, light or dark.
fn install_bundled_icons() {
    let Some(display) = gdk::Display::default() else { return };
    let dir = glib::user_cache_dir().join("android-webcam").join("icons");
    let file = dir.join(format!("{}.svg", USB_ICON));
    let svg = include_str!("../resources/usb-symbolic.svg");
    if fs::read_to_string(&file).ok().as_deref() != Some(svg) {
        if fs::create_dir_all(&dir).and_then(|_| fs::write(&file, svg)).is_err() {
            return;
        }
    }
    gtk4::IconTheme::for_display(&display).add_search_path(&dir);
}

fn install_donate_css() {
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

trait WidgetExtFixed {
    fn set_margin_all(&self, m: i32);
}
impl<T: IsA<gtk4::Widget>> WidgetExtFixed for T {
    fn set_margin_all(&self, m: i32) {
        self.set_margin_start(m);
        self.set_margin_end(m);
        self.set_margin_top(m);
        self.set_margin_bottom(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.camera_facing, 0);
        assert_eq!(cfg.resolution, "1920x1080");
        assert_eq!(cfg.fps_index, 0);
        assert!(!cfg.block_mic);
        assert_eq!(cfg.wireless_ip, "");
    }

    #[test]
    fn test_config_serde_roundtrip() {
        let cfg = AppConfig {
            camera_facing: 1,
            resolution: "1280x720".to_string(),
            fps_index: 1,
            block_mic: true,
            wireless_ip: "192.168.1.55:5555".to_string(),
        };

        let json = serde_json::to_string(&cfg).expect("serialize");
        let deserialized: AppConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(cfg, deserialized);
    }

    #[test]
    fn test_adb_qr_payload() {
        assert_eq!(
            adb_qr_payload("android-webcam-abc123", "s3cr3tpass"),
            "WIFI:T:ADB;S:android-webcam-abc123;P:s3cr3tpass;;"
        );
    }

    #[test]
    fn test_random_token() {
        let a = random_token(10);
        let b = random_token(10);
        assert_eq!(a.len(), 10);
        assert!(a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        assert_ne!(a, b);
    }

    #[test]
    fn test_crash_cause() {
        // Seen on a Pixel 8 Pro when another camera client was open.
        let busy = "[server] ERROR: Video encoding error\njava.io.IOException: android.hardware.camera2.CameraAccessException: The system-wide limit for number of open cameras has been reached, and more camera devices cannot be opened until previous instances are closed.\nERROR: Demuxer error";
        assert_eq!(crash_cause(busy), CrashCause::CameraBusy);
        assert_eq!(
            crash_cause("CameraAccessException: CAMERA_IN_USE (4): The camera device is in use already"),
            CrashCause::CameraBusy
        );
        // Android 17 camera service, when a foreground app holds the camera.
        assert_eq!(
            crash_cause("connectHelper:2628: Higher-priority client using camera, ID \"0\" currently unavailable"),
            CrashCause::CameraBusy
        );
        assert_eq!(
            crash_cause("CameraAccessException: The camera is disabled due to a device policy, and cannot be opened."),
            CrashCause::CameraBlocked
        );
        assert_eq!(
            crash_cause("[server] ERROR: Encoding error: android.media.MediaCodec$CodecException: Error 0xfffffc0e"),
            CrashCause::Other
        );
        assert_eq!(crash_cause(""), CrashCause::Other);
    }

    #[test]
    fn test_next_fallback_resolution() {
        assert_eq!(next_fallback_resolution("3840x2160"), Some("1920x1080"));
        assert_eq!(next_fallback_resolution("2560x1440"), Some("1920x1080"));
        assert_eq!(next_fallback_resolution("1920x1080"), Some("1280x720"));
        assert_eq!(next_fallback_resolution("1280x720"), None);
        assert_eq!(next_fallback_resolution("720x480"), None);
        assert_eq!(next_fallback_resolution("bogus"), None);
    }

    #[test]
    fn test_parse_ip_addr_show() {
        let text = "30: wlan0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc mq state UP\n    inet 192.168.1.120/24 brd 192.168.1.255 scope global wlan0\n";
        assert_eq!(
            parse_ip_from_ip_output(text),
            Some("192.168.1.120".to_string())
        );
    }

    #[test]
    fn test_parse_ip_route() {
        let text = "192.168.1.0/24 dev wlan0 proto kernel scope link src 192.168.1.88\n";
        assert_eq!(
            parse_ip_from_ip_output(text),
            Some("192.168.1.88".to_string())
        );
    }

    #[test]
    fn test_parse_devices_mixed() {
        let output = r#"List of devices attached
ZY2234ABCD	device usb:1-1 product:raven model:Pixel_6_Pro device:raven transport_id:1
192.168.1.50:5555	device product:oriole model:Pixel_6 device:oriole transport_id:2
"#;
        let devices = parse_devices_text(output);
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[0].serial, "ZY2234ABCD");
        assert_eq!(devices[0].model, "Pixel 6 Pro");
        assert!(!devices[0].is_wireless);

        assert_eq!(devices[1].serial, "192.168.1.50:5555");
        assert_eq!(devices[1].model, "Pixel 6");
        assert!(devices[1].is_wireless);
    }

    #[test]
    fn test_parse_devices_mdns_wireless() {
        // Android 11+ wireless debugging, auto-reconnected by adb over mDNS
        let output = "List of devices attached\n\
adb-0A1B2C3D4E5F6G-AbCdEf._adb-tls-connect._tcp\tdevice product:husky model:Pixel_8_Pro device:husky transport_id:1\n";
        let devices = parse_devices_text(output);
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].model, "Pixel 8 Pro");
        assert!(devices[0].is_wireless);

        assert!(is_wireless_serial("adb-ABC-xyz._adb._tcp"));
        assert!(is_wireless_serial("192.168.1.50:5555"));
        assert!(!is_wireless_serial("0A1B2C3D4E5F6G"));
    }
}
