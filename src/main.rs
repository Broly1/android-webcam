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

mod about;
mod donate;
mod i18n;
mod links;
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
    /// Already relaunched once at this resolution after failing right at startup.
    quick_retried: bool,
    /// When this scrcpy was started, to tell a startup failure from a stream that broke later.
    started: Instant,
    /// Already reloaded v4l2loopback once because /dev/video9 stopped accepting video.
    sink_reset: bool,
}

/// Last lines scrcpy printed (its own log plus the phone server's), used to tell why it crashed.
type ScrcpyOutput = Arc<Mutex<VecDeque<String>>>;
const SCRCPY_OUTPUT_LINES: usize = 200;

/// Set ANDROID_WEBCAM_DEBUG=1 to print scrcpy's output and what the app decides
/// (launch, stop, auto-restart, crash recovery) to stderr.
fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| env::var_os("ANDROID_WEBCAM_DEBUG").is_some_and(|v| !v.is_empty() && v != "0"))
}

fn debug_log(msg: &str) {
    if debug_enabled() {
        // Wall-clock time, so lines can be matched against the system journal.
        let t = glib::DateTime::now_local()
            .and_then(|d| d.format("%H:%M:%S.%f"))
            .map(|s| s.to_string())
            .unwrap_or_default();
        eprintln!("[{}] {}", t, msg);
    }
}

/// Why scrcpy exited, judged from its output.
#[derive(Debug, PartialEq, Eq)]
pub enum CrashCause {
    /// Another app (or a stream that hasn't shut down yet) holds the camera.
    CameraBusy,
    /// Camera privacy toggle or device policy.
    CameraBlocked,
    /// This PC has no audio output to play the phone's mic on (no sound server or card).
    NoAudioOutput,
    /// /dev/video9 refuses new video (v4l2loopback before 0.13 after a stream ended with
    /// keep_format off); only reloading the module fixes it.
    SinkBroken,
    /// Anything else, most likely the phone can't encode this resolution.
    Other,
}

const APP_ID: &str = "com.android.webcam";
const APP_ICON_PATH: &str = "/usr/share/icons/hicolor/256x256/apps/android-webcam.png";

struct AppState {
    current_process: Option<Child>,
    current_launch: Option<LaunchParams>,
    current_output: ScrcpyOutput,
    active_serial: Option<String>,
    devices: Vec<DeviceInfo>,
    /// When to next compare /dev/video9's format with the stream's size (None once done).
    format_check_due: Option<Instant>,
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
        // Fixed size: the window always fits its content (GTK sizes a non-resizable window to
        // it), growing for the camera lock note and shrinking back when it goes.
        .resizable(false)
        .build();

    install_theme_workarounds();
    install_css();
    donate::install_css();

    // Header bar with the main menu (About, License).
    about::install_actions(&window);
    let header = gtk4::HeaderBar::new();
    header.pack_end(&about::menu_button());
    window.set_titlebar(Some(&header));

    let config = Arc::new(Mutex::new(load_config()));
    let state = Arc::new(Mutex::new(AppState {
        current_process: None,
        current_launch: None,
        current_output: ScrcpyOutput::default(),
        active_serial: None,
        devices: Vec::new(),
        format_check_due: None,
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
    controls_box.set_margin_bottom(4); // the buttons follow right below, outside the scroll

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
    disconnect_box.append(&usb_symbol());
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

    // Shown while another app (OBS, a browser, Zoom) has the virtual camera open.
    let camera_lock_label = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Center)
        .wrap(true)
        .build();

    let warning_label = Label::builder()
        .use_markup(true)
        .halign(gtk4::Align::Center)
        .wrap(true)
        .build();

    let fps_dropdown = DropDown::from_strings(&["30", "60"]);
    fps_dropdown.set_halign(gtk4::Align::Start); // just wide enough for "60", not the full row
    fps_dropdown.set_selected(config.lock().unwrap().fps_index);

    let mic_check = CheckButton::with_label(tr("mic.block"));
    mic_check.set_active(config.lock().unwrap().block_mic);

    // Launch, Launch in Background and Stop share one compact row.
    let button_box = Box::new(Orientation::Vertical, 0);
    let launch_row = Box::new(Orientation::Horizontal, 6);
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
    launch_row.append(&stop_btn);
    button_box.append(&launch_row);
    for btn in [&start_btn, &background_btn, &stop_btn] {
        btn.add_css_class("stream-btn");
    }
    stop_btn.set_sensitive(false); // lit only while a stream runs (see the crash watchdog)

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
    controls_box.append(&camera_lock_label);

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

    // Warning/status labels that are usually empty: hide them while empty so the box's
    // spacing doesn't leave blank gaps (themes with taller widgets need the room).
    for label in [&wifi_action_status, &camera_lock_label, &camera_warning, &warning_label] {
        label.set_visible(!label.label().is_empty());
        label.connect_label_notify(|l| l.set_visible(!l.label().is_empty()));
    }

    // Scrolling is only a fallback for screens shorter than the window.
    let scrolled_controls = ScrolledWindow::builder()
        .hscrollbar_policy(PolicyType::Never)
        .vscrollbar_policy(PolicyType::Automatic)
        .propagate_natural_height(true)
        .child(&controls_box)
        .build();

    // The buttons and the status line sit outside the scrolled area so they're always fully
    // visible, even when a theme with taller widgets makes the settings overflow the window.
    button_box.set_margin_start(18);
    button_box.set_margin_end(18);
    button_box.set_margin_top(2);
    status_label.set_margin_start(18);
    status_label.set_margin_end(18);
    status_label.set_margin_top(6);
    // Room for three lines from the start ("Stream Active", the stream details, a notice), so
    // the status growing when a stream starts doesn't shrink the controls and hide Stop for a
    // moment. Measured once realized, with the theme's caption font.
    status_label.connect_realize(|label| {
        let (_, three_lines) = label.create_pango_layout(Some("A\nA\nA")).pixel_size();
        label.set_size_request(-1, three_lines);
    });
    // The settings take the height they need and the buttons follow right below; spare height
    // goes under the status line rather than between the settings and the buttons.
    status_label.set_vexpand(true);
    status_label.set_valign(gtk4::Align::Start);
    let controls_page = Box::new(Orientation::Vertical, 0);
    controls_page.append(&scrolled_controls);
    controls_page.append(&button_box);
    controls_page.append(&status_label);

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
    stack.add_named(&controls_page, Some("controls"));

    // ==========================================
    // EVENT HANDLERS & SIGNALS
    // ==========================================

    // Launch (with or without the scrcpy window)
    let apply_changes = glib::clone!(
        @weak facing_dropdown, @weak res_combo, @weak fps_dropdown, @weak mic_check, @weak status_label, @weak stop_btn, @strong state, @strong config => move |background: bool| {
            let mut s = state.lock().unwrap();

            stop_stream(&mut s);
            thread::sleep(Duration::from_millis(500));

            if !Path::new("/dev/video9").exists() {
                let _ = Command::new("pkexec")
                    .args(["modprobe", "v4l2loopback", "video_nr=9", "card_label=Android-Webcam", "exclusive_caps=1"])
                    .status();
            }

            if let Some(problem) = scrcpy_problem() {
                debug_log(&format!("scrcpy check: {}", problem));
                status_label.set_markup(&format!("<span foreground='red'>{}</span>", esc(&problem)));
                return;
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
                quick_retried: false,
                started: Instant::now(),
                sink_reset: false,
            };
            if let Some((child, output)) = run_scrcpy(&launch) {
                status_label.set_markup(&format!(
                    "<span foreground='green'><b>{}</b>\n{}</span>",
                    esc(tr("status.active")),
                    esc(&stream_details(&launch))
                ));
                s.current_process = Some(child);
                s.current_output = output;
                s.current_launch = Some(launch);
                start_format_check(&mut s);
                stop_btn.set_sensitive(true);
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
            debug_log("setting changed, nothing running: saved only");
            return; // nothing running: the new settings are just saved for next launch
        };
        debug_log(&format!("setting changed: restarting stream in 400ms (background={})", background));
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
                    debug_log(&format!("qr pairing: {} (done={})", message, done));
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
    stop_btn.connect_clicked(glib::clone!(@strong state, @weak status_label => move |btn| {
        if let Ok(mut s) = state.lock() {
            stop_stream(&mut s);
            status_label.set_text(tr("status.stopped"));
        }
        btn.set_sensitive(false);
    }));

    // Clean exit on window close
    window.connect_close_request(glib::clone!(@strong state => move |_| {
        if let Ok(mut s) = state.lock() {
            stop_stream(&mut s);
        }
        glib::Propagation::Proceed
    }));

    // Crash watchdog: if scrcpy exits on its own, find out why from its output. A busy camera
    // gets one retry at the same resolution; anything else that isn't a camera error is retried
    // at the same resolution, and only steps down (1080p, then 720p) if it keeps failing right
    // at startup, e.g. the phone can't encode 4K. A user Stop takes current_process first, so
    // it isn't seen as a crash. While the stream runs it also checks the virtual camera's format.
    let mut watchdog_ticks = 0u32;
    glib::timeout_add_local(Duration::from_millis(500), glib::clone!(
        @weak status_label, @weak res_combo, @weak facing_dropdown, @weak fps_dropdown, @weak mic_check,
        @weak start_btn, @weak background_btn, @weak stop_btn, @weak camera_lock_label, @strong state, @strong is_updating_res
            => @default-return glib::ControlFlow::Break, move || {
            stop_btn.set_sensitive(state.lock().unwrap().current_launch.is_some());
            if watchdog_ticks == 0 {
                let streaming = state.lock().unwrap().current_launch.is_some();
                let restarts: [&gtk4::Widget; 4] = [fps_dropdown.upcast_ref(), mic_check.upcast_ref(), start_btn.upcast_ref(), background_btn.upcast_ref()];
                update_camera_lock(&res_combo, &facing_dropdown, &restarts, streaming, &camera_lock_label);
                watchdog_ticks = 4; // every 2 s
            }
            watchdog_ticks -= 1;
            let mut s = state.lock().unwrap();
            let exit = match s.current_process.as_mut().map(|child| child.try_wait()) {
                Some(Ok(Some(status))) => Some(status.success()),
                Some(Err(_)) => Some(false),
                _ => None,
            };
            let Some(clean_exit) = exit else {
                if s.current_process.is_none() {
                    return glib::ControlFlow::Continue;
                }
                let output = s.current_output.lock().unwrap().iter().cloned().collect::<Vec<_>>().join("\n");
                if crash_cause(&output) == CrashCause::SinkBroken {
                    debug_log("v4l2 sink broken while running");
                    if let Some(launch) = s.current_launch.clone() {
                        recover_broken_sink(&mut s, &launch, &status_label);
                    }
                } else {
                    check_sink_format(&mut s, &status_label);
                }
                return glib::ControlFlow::Continue;
            };
            debug_log(&format!("scrcpy exited ({})", if clean_exit { "clean" } else { "error" }));
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

            debug_log(&format!("crash cause: {:?}", crash_cause(&output)));
            let (retry, notice) = match crash_cause(&output) {
                CrashCause::CameraBlocked => {
                    fail(tr("status.camera_blocked"));
                    return glib::ControlFlow::Continue;
                }
                // Not the resolution's fault: retrying lower would fail the same way.
                CrashCause::NoAudioOutput => {
                    fail(tr("status.no_audio_output"));
                    return glib::ControlFlow::Continue;
                }
                CrashCause::SinkBroken => {
                    recover_broken_sink(&mut s, &launch, &status_label);
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
                    let ran_for = launch.started.elapsed();
                    debug_log(&format!("ran for {:.1}s, quick_retried={}", ran_for.as_secs_f32(), launch.quick_retried));
                    match after_other_crash(&launch.res, ran_for, launch.quick_retried) {
                        OtherCrashAction::RetrySame => {
                            let notice = trf("status.restarting", &[("res", &launch.res)]);
                            (LaunchParams { quick_retried: ran_for < STARTUP_WINDOW, ..launch.clone() }, notice)
                        }
                        OtherCrashAction::Lower(fallback) => {
                            let notice = trf("status.relaunched", &[("res", &launch.res), ("fallback", fallback)]);
                            (LaunchParams { res: fallback.to_string(), quick_retried: false, ..launch.clone() }, notice)
                        }
                        OtherCrashAction::GiveUp => {
                            fail(&trf("status.crashed", &[("res", &launch.res)]));
                            return glib::ControlFlow::Continue;
                        }
                    }
                }
            };
            let retry = LaunchParams { started: Instant::now(), ..retry };

            let fallback = (retry.res != launch.res).then(|| retry.res.clone());
            match run_scrcpy(&retry) {
                Some((child, output)) => {
                    s.current_process = Some(child);
                    s.current_output = output;
                    status_label.set_markup(&format!(
                        "<span foreground='#ffa500'>{}</span>\n<span foreground='green'>{}</span>",
                        esc(&notice),
                        esc(&stream_details(&retry))
                    ));
                    s.current_launch = Some(retry);
                    start_format_check(&mut s);
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

    stack.set_vexpand(true);
    let root = Box::new(Orientation::Vertical, 0);
    root.append(&stack);
    root.append(&donate::footer(&window));
    window.set_child(Some(&root));
    window.show();
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

/// A failure this soon after launch is a startup failure (e.g. the phone can't encode that
/// size); later, the stream was running and something interrupted it.
const STARTUP_WINDOW: Duration = Duration::from_secs(10);

/// What to do after scrcpy fails for a reason crash_cause doesn't recognise.
#[derive(Debug, PartialEq, Eq)]
pub enum OtherCrashAction {
    RetrySame,
    Lower(&'static str),
    GiveUp,
}

/// A stream that ran for a while and then broke was interrupted (the phone's camera or
/// encoder hiccuped), so it's restarted as is rather than losing resolution for the session.
/// A startup failure gets one more try at the same size, then steps down.
pub fn after_other_crash(res: &str, ran_for: Duration, quick_retried: bool) -> OtherCrashAction {
    if ran_for >= STARTUP_WINDOW || !quick_retried {
        OtherCrashAction::RetrySame
    } else {
        next_fallback_resolution(res).map_or(OtherCrashAction::GiveUp, OtherCrashAction::Lower)
    }
}

/// First check of /dev/video9's format after a launch, then every FORMAT_CHECK_INTERVAL until
/// FORMAT_CHECK_TIMEOUT. A stuck format is only reported after FORMAT_STUCK_AFTER, so scrcpy
/// has had time to set its own.
const FORMAT_CHECK_DELAY: Duration = Duration::from_secs(1);
const FORMAT_CHECK_INTERVAL: Duration = Duration::from_secs(1);
const FORMAT_STUCK_AFTER: Duration = Duration::from_secs(8);
const FORMAT_CHECK_TIMEOUT: Duration = Duration::from_secs(30);

/// Starts the format checks for a stream that was just launched.
fn start_format_check(s: &mut AppState) {
    s.format_check_due = Some(Instant::now() + FORMAT_CHECK_DELAY);
}

/// v4l2loopback can't change /dev/video9's format while another app (OBS, a browser) has it
/// open, so after a resolution change that app gets frames of the wrong size: a green or
/// scrambled picture. Once the stream is up, compare the device's size with ours and, if it's
/// stuck, say which app is holding it.
fn check_sink_format(s: &mut AppState, status_label: &Label) {
    let Some(due) = s.format_check_due else { return };
    if Instant::now() < due {
        return;
    }
    let Some(launch) = s.current_launch.as_ref() else {
        s.format_check_due = None;
        return;
    };
    let ran_for = launch.started.elapsed();
    let size = v4l2_sink_size();
    if size.as_deref() == Some(launch.res.as_str()) {
        debug_log(&format!("v4l2 format {} matches the stream", launch.res));
        s.format_check_due = None;
        return;
    }
    if let Some(current) = size.filter(|_| ran_for >= FORMAT_STUCK_AFTER) {
        let holders = v4l2_sink_holders();
        debug_log(&format!("v4l2 format stuck at {} (stream {}), held by {:?}", current, launch.res, holders));
        if holders.is_empty() {
            // Nobody else has it open, so the module itself kept the old size (v4l2loopback
            // before 0.13, e.g. the app that held it closed just before this launch).
            s.format_check_due = None;
            let version = fs::read_to_string("/sys/module/v4l2loopback/version").ok();
            if v4l2loopback_is_old(version.as_deref()) && !launch.sink_reset {
                let launch = launch.clone();
                recover_broken_sink(s, &launch, status_label);
            }
            return;
        }
        let msg = trf("status.format_stuck", &[("apps", &holders.join(", ")), ("current", &current), ("res", &launch.res)]);
        status_label.set_markup(&format!(
            "<span foreground='#ffa500'>{}</span>\n<span foreground='green'>{}</span>",
            esc(&msg),
            esc(&stream_details(launch))
        ));
        s.format_check_due = None;
        return;
    }
    s.format_check_due = (ran_for < FORMAT_CHECK_TIMEOUT).then(|| Instant::now() + FORMAT_CHECK_INTERVAL);
}

/// /dev/video9 stopped accepting video (see CrashCause::SinkBroken). Reload v4l2loopback (one
/// password prompt) and relaunch, once. The module can't be unloaded while another app has
/// the camera open, so then just say which app to close.
fn recover_broken_sink(s: &mut AppState, launch: &LaunchParams, status_label: &Label) {
    let fail = |msg: &str| status_label.set_markup(&format!("<span foreground='red'>{}</span>", esc(msg)));
    stop_stream(s);
    if launch.sink_reset {
        fail(tr("status.sink_reset_failed"));
        return;
    }
    thread::sleep(Duration::from_millis(300)); // let the killed scrcpy release the device
    let holders = v4l2_sink_holders();
    if !holders.is_empty() {
        debug_log(&format!("v4l2 sink broken, can't reload: held by {:?}", holders));
        fail(&trf("status.sink_busy", &[("apps", &holders.join(", "))]));
        return;
    }
    if !reload_v4l2loopback() {
        fail(tr("status.sink_reset_failed"));
        return;
    }
    let retry = LaunchParams { sink_reset: true, started: Instant::now(), ..launch.clone() };
    match run_scrcpy(&retry) {
        Some((child, output)) => {
            status_label.set_markup(&format!(
                "<span foreground='#ffa500'>{}</span>\n<span foreground='green'>{}</span>",
                esc(tr("status.sink_reset_done")),
                esc(&stream_details(&retry))
            ));
            s.current_process = Some(child);
            s.current_output = output;
            s.current_launch = Some(retry);
            start_format_check(s);
        }
        None => fail(tr("status.failed")),
    }
}

/// /dev/video9's current capture size as "WIDTHxHEIGHT", or None if it has no format yet.
fn v4l2_sink_size() -> Option<String> {
    let out = Command::new("v4l2-ctl").args(["-d", "/dev/video9", "--get-fmt-video"]).output().ok()?;
    parse_v4l2_size(&String::from_utf8_lossy(&out.stdout))
}

/// Reads "Width/Height      : 1920/1080" from `v4l2-ctl --get-fmt-video`.
pub fn parse_v4l2_size(text: &str) -> Option<String> {
    let value = text.lines().find_map(|l| l.trim().strip_prefix("Width/Height"))?;
    let (w, h) = value.trim_start_matches([' ', ':']).trim().split_once('/')?;
    let (w, h): (u32, u32) = (w.trim().parse().ok()?, h.trim().parse().ok()?);
    Some(format!("{}x{}", w, h))
}

/// v4l2loopback can't change the camera's size while another app has it open, so while one
/// does, resolution and camera (front/back, whose sizes differ) are locked with a note. While
/// that app is receiving our stream, everything that restarts the stream (FPS, mic, Start,
/// Launch in Background) is locked too: a restart cuts its video (a browser drops the camera).
/// Stop stays available.
fn update_camera_lock(
    res_combo: &ComboBoxText,
    facing_dropdown: &DropDown,
    restarts: &[&gtk4::Widget],
    streaming: bool,
    lock_label: &Label,
) {
    let holders = v4l2_sink_holders();
    let locked = !holders.is_empty();
    let lock_restarts = locked && streaming;
    if res_combo.is_sensitive() == locked || restarts.first().is_some_and(|w| w.is_sensitive() == lock_restarts) {
        debug_log(&format!("camera lock: {}", match (locked, lock_restarts) {
            (false, _) => "off".to_string(),
            (true, false) => format!("resolution/camera, held by {:?}", holders),
            (true, true) => format!("all settings and launch, streaming to {:?}", holders),
        }));
    }
    res_combo.set_sensitive(!locked);
    facing_dropdown.set_sensitive(!locked);
    for widget in restarts {
        widget.set_sensitive(!lock_restarts);
    }
    if locked {
        let key = if lock_restarts { "camera.locked_streaming" } else { "camera.locked" };
        let msg = trf(key, &[("apps", &holders.join(", "))]);
        lock_label.set_markup(&format!("<span foreground='#ffa500' size='small'>{}</span>", esc(&msg)));
    } else {
        lock_label.set_text("");
    }
}

/// Names of the apps (other than scrcpy and us) that have /dev/video9 open, from /proc.
fn v4l2_sink_holders() -> Vec<String> {
    let me = std::process::id().to_string();
    let mut names = Vec::new();
    let Ok(procs) = fs::read_dir("/proc") else { return names };
    for proc_dir in procs.flatten() {
        let pid = proc_dir.file_name().to_string_lossy().to_string();
        if pid == me || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(fds) = fs::read_dir(proc_dir.path().join("fd")) else { continue };
        let holds = fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|t| t == Path::new("/dev/video9")));
        if !holds {
            continue;
        }
        let status = fs::read_to_string(proc_dir.path().join("status")).unwrap_or_default();
        if status.lines().any(|l| l.strip_prefix("PPid:").is_some_and(|p| p.trim() == me)) {
            continue; // scrcpy and v4l2-ctl, run by us
        }
        let comm = fs::read_to_string(proc_dir.path().join("comm")).unwrap_or_default().trim().to_string();
        let name = match comm.as_str() {
            "scrcpy" | "" => continue,
            "obs" => "OBS".to_string(),
            other => other.to_string(),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Camera mode needs scrcpy 2.2+. Returns what to tell the user if scrcpy is missing or older
/// (e.g. Ubuntu 24.04 packages 1.25), instead of letting every launch fail as a "crash".
fn scrcpy_problem() -> Option<String> {
    let Ok(out) = Command::new("scrcpy").arg("--version").output() else {
        return Some(tr("status.scrcpy_missing").to_string());
    };
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    match parse_scrcpy_version(&text) {
        Some((major, minor)) if (major, minor) < (2, 2) => {
            Some(trf("status.scrcpy_too_old", &[("version", &format!("{}.{}", major, minor))]))
        }
        _ => None,
    }
}

/// Reads "scrcpy 5.0.1 <https://github.com/Genymobile/scrcpy>" as (5, 0).
pub fn parse_scrcpy_version(text: &str) -> Option<(u32, u32)> {
    let version = text.lines().find_map(|l| l.trim().strip_prefix("scrcpy "))?.split_whitespace().next()?;
    let mut parts = version.split('.').map(|p| p.parse::<u32>().ok());
    Some((parts.next()??, parts.next().flatten().unwrap_or(0)))
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
    // SDL's error when scrcpy can't open a playback device for the mic.
    const NO_AUDIO: &[&str] = &["could not open audio device"];
    // FFmpeg's v4l2 output failing on the device itself (scrcpy keeps running, with no video).
    const SINK_BROKEN: &[&str] = &["failed to write header to /dev/video9"];
    let out = output.to_lowercase();
    if BUSY.iter().any(|p| out.contains(p)) {
        CrashCause::CameraBusy
    } else if BLOCKED.iter().any(|p| out.contains(p)) {
        CrashCause::CameraBlocked
    } else if NO_AUDIO.iter().any(|p| out.contains(p)) {
        CrashCause::NoAudioOutput
    } else if SINK_BROKEN.iter().any(|p| out.contains(p)) {
        CrashCause::SinkBroken
    } else {
        CrashCause::Other
    }
}

/// Keeps the last SCRCPY_OUTPUT_LINES lines of `r`. Also keeps the pipe drained so scrcpy never
/// blocks on a full buffer.
fn collect_output(r: impl Read + Send + 'static, output: ScrcpyOutput) {
    thread::spawn(move || {
        for line in BufReader::new(r).lines().map_while(Result::ok) {
            debug_log(&format!("[scrcpy] {}", line));
            let mut out = output.lock().unwrap();
            if out.len() >= SCRCPY_OUTPUT_LINES {
                out.pop_front();
            }
            out.push_back(line);
        }
    });
}

/// What a running stream is using, for the status line, e.g.
/// "1920x1080 · 60 FPS · Back Camera · Mic On · Wi-Fi · Background".
fn stream_details(launch: &LaunchParams) -> String {
    let mut parts = vec![
        launch.res.clone(),
        trf("status.fps", &[("fps", &launch.fps)]),
        tr(if launch.facing == "front" { "camera.front" } else { "camera.back" }).to_string(),
        tr(if launch.block_mic { "status.mic_off" } else { "status.mic_on" }).to_string(),
    ];
    if let Some(serial) = launch.serial.as_deref().filter(|s| !s.is_empty()) {
        parts.push(if is_wireless_serial(serial) { "Wi-Fi" } else { "USB" }.to_string());
    }
    if launch.background {
        parts.push(tr("status.background").to_string());
    }
    parts.join(" · ")
}

fn run_scrcpy(launch: &LaunchParams) -> Option<(Child, ScrcpyOutput)> {
    prepare_v4l2_sink(&launch.res);
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
    debug_log(&format!("launch: scrcpy {}", args.join(" ")));
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
    debug_log("stop stream");
    if let Some(mut child) = s.current_process.take() {
        terminate_gracefully(&mut child);
    }
    s.current_launch = None;
    s.format_check_due = None;
    s.current_output = ScrcpyOutput::default();
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

/// Ask scrcpy to quit (SIGTERM) so it closes its window properly, and only SIGKILL it if it
/// hasn't exited after a moment. Killing a scrcpy that has a window outright crashes
/// GNOME Shell 46 (Ubuntu 24.04): mutter trips over the vanished Wayland client.
fn terminate_gracefully(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    debug_log("scrcpy ignored SIGTERM, killing it");
    let _ = child.kill();
    let _ = child.wait();
}

/// v4l2loopback's keep_format control makes the device keep its format after the writer
/// leaves, and the right setting depends on the module version:
/// - before 0.13 (Ubuntu 24.04's 0.12.7, built-in or DKMS; it reports no version): with it
///   off, the device refuses every stream after the first ("VIDIOC_G_FMT: Invalid argument")
///   until the module is reloaded. Turning it off between streams breaks it the same way. So
///   it stays on, and since the kept size can't change, picking another resolution reloads the
///   module first (one password prompt). Restarts at the same size need nothing.
/// - 0.13 and later: with it on, the device keeps a stale format (640x480 BGR4 by default)
///   that the next stream can't replace, so scrcpy's frames are read with the wrong layout. Off.
fn prepare_v4l2_sink(res: &str) {
    let version = fs::read_to_string("/sys/module/v4l2loopback/version").ok();
    if !v4l2loopback_is_old(version.as_deref()) {
        set_v4l2_keep_format(false);
        return;
    }
    if let Some(current) = v4l2_sink_size().filter(|size| size != res) {
        let holders = v4l2_sink_holders();
        if holders.is_empty() {
            debug_log(&format!("old v4l2loopback keeps {}, reloading it for {}", current, res));
            reload_v4l2loopback();
        } else {
            debug_log(&format!("old v4l2loopback keeps {}, can't reload: held by {:?}", current, holders));
        }
    }
    set_v4l2_keep_format(true);
}

/// v4l2loopback before 0.13, from /sys/module/v4l2loopback/version. Ubuntu's 0.12.7 builds
/// don't report a version, while 0.13+ do, so no version counts as old.
pub fn v4l2loopback_is_old(version: Option<&str>) -> bool {
    let Some(version) = version else { return true };
    let mut parts = version.trim().split('.').map(|p| p.parse::<u32>().ok());
    match (parts.next().flatten(), parts.next().flatten()) {
        (Some(major), Some(minor)) => (major, minor) < (0, 13),
        _ => true,
    }
}

/// Unloads and reloads v4l2loopback (one password prompt), which resets /dev/video9. Fails if
/// an app still has the camera open.
fn reload_v4l2loopback() -> bool {
    let ok = Command::new("pkexec")
        .args(["sh", "-c", "modprobe -r v4l2loopback && modprobe v4l2loopback video_nr=9 card_label=Android-Webcam exclusive_caps=1"])
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    debug_log(&format!("v4l2loopback reload: {}", if ok { "done" } else { "failed or cancelled" }));
    // The new /dev/video9 only becomes usable once udev has set its permissions.
    let deadline = Instant::now() + Duration::from_secs(5);
    while ok && Instant::now() < deadline {
        let ready = Command::new("v4l2-ctl")
            .args(["-d", "/dev/video9", "--get-ctrl", "keep_format"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|st| st.success());
        if ready {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    ok
}

fn set_v4l2_keep_format(on: bool) {
    let arg = if on { "keep_format=1" } else { "keep_format=0" };
    let ok = Command::new("v4l2-ctl")
        .args(["-d", "/dev/video9", "-c", arg])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    debug_log(&format!("v4l2 {}: {}", arg, if ok { "set" } else { "not set (v4l2-ctl missing or failed)" }));
}

/// Smaller stream buttons, so the three fit in one row.
fn install_css() {
    let Some(display) = gdk::Display::default() else { return };
    let provider = CssProvider::new();
    provider.load_from_data(
        "button.stream-btn { min-height: 0; padding: 4px 6px; }
         window.csd { border-radius: 12px; }",
    );
    gtk4::style_context_add_provider_for_display(&display, &provider, gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION);
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
    // Only when Breeze's GTK 4 theme is really installed: otherwise GTK falls back to
    // Adwaita, whose sliders these negative margins would push below zero instead.
    let mut theme_dirs = vec![glib::user_data_dir().join("themes")];
    if let Some(home) = env::var_os("HOME") {
        theme_dirs.push(PathBuf::from(home).join(".themes"));
    }
    theme_dirs.extend(glib::system_data_dirs().into_iter().map(|d| d.join("themes")));
    if !theme_dirs.iter().any(|d| d.join(&theme).join("gtk-4.0").is_dir()) {
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

/// The USB trident, drawn with cairo in the widget's current text color. No icon theme has
/// a USB symbol everywhere, and a bundled SVG needs an SVG loader and isn't recolored by
/// every GTK version (GTK 4.18 left it dark grey on a red button), so draw it ourselves.
fn usb_symbol() -> DrawingArea {
    let area = DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk4::Align::Center)
        .build();
    area.set_draw_func(|area, cr, width, height| {
        #[allow(deprecated)]
        let c = area.style_context().color();
        cr.set_source_rgba(c.red().into(), c.green().into(), c.blue().into(), c.alpha().into());
        // Designed on a 16x16 grid.
        let s = f64::from(width.min(height)) / 16.0;
        cr.translate((f64::from(width) - 16.0 * s) / 2.0, (f64::from(height) - 16.0 * s) / 2.0);
        cr.scale(s, s);
        let poly = |pts: &[(f64, f64)]| {
            cr.move_to(pts[0].0, pts[0].1);
            for &(x, y) in &pts[1..] {
                cr.line_to(x, y);
            }
            cr.close_path();
        };
        poly(&[(8.0, 0.0), (11.0, 3.6), (5.0, 3.6)]); // arrow head
        cr.rectangle(7.25, 3.0, 1.5, 9.0); // trunk
        poly(&[(7.25, 10.2), (4.75, 8.6), (4.75, 6.2), (3.25, 6.2), (3.25, 9.4), (7.25, 11.9)]); // left arm
        poly(&[(8.75, 8.6), (11.75, 6.7), (11.75, 5.0), (10.25, 5.0), (10.25, 5.9), (8.75, 6.85)]); // right arm
        cr.rectangle(9.9, 2.6, 2.2, 2.2); // square tip
        let _ = cr.fill();
        for (x, y, r) in [(8.0, 13.5, 2.0), (4.0, 5.3, 1.4)] {
            cr.new_sub_path();
            cr.arc(x, y, r, 0.0, std::f64::consts::TAU); // base and round tip
        }
        let _ = cr.fill();
    });
    area
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
        // Ubuntu 26.04 VM with no sound card, mic forwarding on.
        assert_eq!(
            crash_cause("ERROR: Could not open audio device: ALSA: Couldn't open audio device: No such file or directory\nERROR: Demuxer error"),
            CrashCause::NoAudioOutput
        );
        // Ubuntu 24.04 (v4l2loopback 0.12.7) after a stream ended with keep_format off.
        assert_eq!(
            crash_cause("[scrcpy] ERROR: [FFmpeg] ioctl(VIDIOC_G_FMT): Invalid argument\n[scrcpy] ERROR: Failed to write header to /dev/video9\nWARN: Could not send packet to v4l2 sink"),
            CrashCause::SinkBroken
        );
        assert_eq!(crash_cause(""), CrashCause::Other);
    }

    #[test]
    fn test_v4l2loopback_is_old() {
        assert!(!v4l2loopback_is_old(Some("0.15.4\n"))); // Arch
        assert!(!v4l2loopback_is_old(Some("0.15.0"))); // Debian 13
        assert!(!v4l2loopback_is_old(Some("0.13.2"))); // bookworm-backports
        assert!(v4l2loopback_is_old(Some("0.12.7")));
        assert!(v4l2loopback_is_old(None)); // Ubuntu 24.04's 0.12.7 reports no version
        assert!(v4l2loopback_is_old(Some("")));
    }

    #[test]
    fn test_after_other_crash() {
        let quick = Duration::from_secs(1);
        let long = Duration::from_secs(62);
        // First startup failure: one more try at the same size.
        assert_eq!(after_other_crash("1920x1080", quick, false), OtherCrashAction::RetrySame);
        // Fails at startup again: step down.
        assert_eq!(after_other_crash("1920x1080", quick, true), OtherCrashAction::Lower("1280x720"));
        assert_eq!(after_other_crash("3840x2160", quick, true), OtherCrashAction::Lower("1920x1080"));
        assert_eq!(after_other_crash("1280x720", quick, true), OtherCrashAction::GiveUp);
        // Ran fine for a minute, then broke (OnePlus 8T at 720p): restart as is.
        assert_eq!(after_other_crash("1280x720", long, true), OtherCrashAction::RetrySame);
        assert_eq!(after_other_crash("1920x1080", long, false), OtherCrashAction::RetrySame);
    }

    #[test]
    fn test_parse_v4l2_size() {
        let fmt = "Format Video Capture:\n\tWidth/Height      : 1920/1080\n\tPixel Format      : 'YU12' (Planar YUV 4:2:0)";
        assert_eq!(parse_v4l2_size(fmt).as_deref(), Some("1920x1080"));
        assert_eq!(parse_v4l2_size("VIDIOC_G_FMT: failed: Invalid argument"), None);
        assert_eq!(parse_v4l2_size(""), None);
    }

    #[test]
    fn test_parse_scrcpy_version() {
        assert_eq!(parse_scrcpy_version("scrcpy 5.0.1 <https://github.com/Genymobile/scrcpy>\n\nDependencies (compiled / linked):"), Some((5, 0)));
        assert_eq!(parse_scrcpy_version("scrcpy 3.3.4 <https://github.com/Genymobile/scrcpy>"), Some((3, 3)));
        // Ubuntu 24.04's package: too old for --video-source=camera.
        assert_eq!(parse_scrcpy_version("scrcpy 1.25 <https://github.com/Genymobile/scrcpy>"), Some((1, 25)));
        assert_eq!(parse_scrcpy_version("bash: scrcpy: command not found"), None);
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
