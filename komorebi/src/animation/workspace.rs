//! Workspace switch animations
//!
//! Real windows are never moved during a workspace switch animation. Instead, a
//! topmost overlay window is placed over the work area of the monitor, and live DWM
//! thumbnails of the desktop wallpaper, the outgoing windows and the incoming windows
//! are composited onto it by DWM on the GPU. Thumbnails keep rendering even for cloaked
//! windows, so the real workspace switch happens instantly underneath the overlay, and
//! the overlay is removed once the thumbnails have reached the real window positions.

use crate::animation::ANIMATION_DURATION_GLOBAL;
use crate::animation::ANIMATION_DURATION_PER_ANIMATION;
use crate::animation::ANIMATION_ENABLED_GLOBAL;
use crate::animation::ANIMATION_ENABLED_PER_ANIMATION;
use crate::animation::ANIMATION_STYLE_GLOBAL;
use crate::animation::ANIMATION_STYLE_PER_ANIMATION;
use crate::animation::prefix::AnimationPrefix;
use crate::animation::style::apply_ease_func;
use crate::core::Rect;
use crate::windows_api::WindowsApi;
use crate::workspace::Workspace;

use crossbeam_channel::Receiver;
use crossbeam_channel::Sender;
use crossbeam_utils::atomic::AtomicCell;
use lazy_static::lazy_static;
use parking_lot::Mutex;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::LRESULT;
use windows::Win32::Foundation::RECT;
use windows::Win32::Foundation::WPARAM;
use windows::Win32::Graphics::Dwm::DWM_THUMBNAIL_PROPERTIES;
use windows::Win32::Graphics::Dwm::DWM_TNP_OPACITY;
use windows::Win32::Graphics::Dwm::DWM_TNP_RECTDESTINATION;
use windows::Win32::Graphics::Dwm::DWM_TNP_RECTSOURCE;
use windows::Win32::Graphics::Dwm::DWM_TNP_SOURCECLIENTAREAONLY;
use windows::Win32::Graphics::Dwm::DWM_TNP_VISIBLE;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dwm::DwmRegisterThumbnail;
use windows::Win32::Graphics::Dwm::DwmUnregisterThumbnail;
use windows::Win32::Graphics::Dwm::DwmUpdateThumbnailProperties;
use windows::Win32::UI::WindowsAndMessaging::CreateWindowExW;
use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
use windows::Win32::UI::WindowsAndMessaging::DispatchMessageW;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT;
use windows::Win32::UI::WindowsAndMessaging::GetTopWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::Win32::UI::WindowsAndMessaging::PM_REMOVE;
use windows::Win32::UI::WindowsAndMessaging::PeekMessageW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW;
use windows::Win32::UI::WindowsAndMessaging::SetWindowPos;
use windows::Win32::UI::WindowsAndMessaging::ShowWindow;
use windows::Win32::UI::WindowsAndMessaging::TranslateMessage;
use windows::Win32::UI::WindowsAndMessaging::WNDCLASSW;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST;
use windows::Win32::UI::WindowsAndMessaging::WS_POPUP;
use windows::core::PCWSTR;
use windows::core::w;

/// How long the main thread waits for the overlay to be on screen before switching
const OVERLAY_READY_TIMEOUT: Duration = Duration::from_millis(60);

#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// The kind of animation played when switching workspaces
pub enum WorkspaceAnimationKind {
    /// Workspaces slide horizontally, like on a horizontal strip
    #[default]
    Slide,
    /// Workspaces slide vertically, like on a vertical strip
    SlideVertical,
    /// Workspaces cross-fade in place
    Fade,
    /// Workspaces slide a short distance horizontally while cross-fading
    SlideFade,
    /// Workspaces slide a short distance vertically while cross-fading
    SlideFadeVertical,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Workspace switch animation configuration
pub struct WorkspaceAnimationConfig {
    /// Enable or disable workspace switch animations (default: follows `animation.enabled`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The kind of animation (default: Slide)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<WorkspaceAnimationKind>,
    /// Animation duration in ms (default: follows `animation.duration`)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// A CSS-style cubic bezier curve `[x1, y1, x2, y2]` which overrides the animation style,
    /// eg. `[0.05, 0.9, 0.1, 1.05]` for a slight overshoot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bezier: Option<[f64; 4]>,
    /// Distance travelled by the SlideFade kinds, as a fraction of the work area (default: 0.2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slide_fade_distance: Option<f64>,
}

lazy_static! {
    static ref CONFIG: Mutex<WorkspaceAnimationConfig> =
        Mutex::new(WorkspaceAnimationConfig::default());
    /// The workspace index currently on screen for each monitor (by hmonitor)
    static ref SHOWN_WORKSPACES: Mutex<HashMap<isize, usize>> = Mutex::new(HashMap::new());
}

static ANIMATOR: OnceLock<Sender<Command>> = OnceLock::new();
static ANIMATION_RUNNING: AtomicCell<bool> = AtomicCell::new(false);

pub fn set_config(config: Option<WorkspaceAnimationConfig>) {
    *CONFIG.lock() = config.unwrap_or_default();
}

pub fn enabled() -> bool {
    if let Some(enabled) = CONFIG.lock().enabled {
        return enabled;
    }

    ANIMATION_ENABLED_PER_ANIMATION
        .lock()
        .get(&AnimationPrefix::Workspace)
        .copied()
        .unwrap_or_else(|| ANIMATION_ENABLED_GLOBAL.load(Ordering::SeqCst))
}

pub fn is_running() -> bool {
    ANIMATION_RUNNING.load()
}

/// Called whenever a monitor shows one of its workspaces. Starts an animation when the
/// shown workspace changes, and blocks until the overlay covers the work area (or a short
/// timeout elapses) so that the real workspace switch can safely happen underneath it.
pub fn workspace_shown(
    hmonitor: isize,
    work_area: Rect,
    workspaces: &VecDeque<Workspace>,
    shown_idx: usize,
) {
    let previous_idx = SHOWN_WORKSPACES.lock().insert(hmonitor, shown_idx);

    let Some(previous_idx) = previous_idx else {
        return;
    };

    if previous_idx == shown_idx || !enabled() {
        return;
    }

    let outgoing = workspaces
        .get(previous_idx)
        .map(visible_windows)
        .unwrap_or_default();
    let incoming = workspaces
        .get(shown_idx)
        .map(visible_windows)
        .unwrap_or_default();

    if outgoing.is_empty() && incoming.is_empty() {
        return;
    }

    let (ack_sender, ack_receiver) = crossbeam_channel::bounded(1);
    let command = Command {
        hmonitor,
        work_area,
        outgoing,
        incoming,
        forward: shown_idx > previous_idx,
        ack: ack_sender,
    };

    if animator().send(command).is_ok() {
        let _ = ack_receiver.recv_timeout(OVERLAY_READY_TIMEOUT);
    }
}

/// The windows of a workspace which are visible when it is on screen
fn visible_windows(workspace: &Workspace) -> Vec<isize> {
    let mut hwnds = vec![];

    if let Some(window) = workspace
        .monocle_container
        .as_ref()
        .and_then(|c| c.focused_window())
    {
        hwnds.push(window.hwnd);
    } else if let Some(window) = workspace.maximized_window {
        hwnds.push(window.hwnd);
    } else {
        for container in workspace.containers() {
            if let Some(window) = container.focused_window() {
                hwnds.push(window.hwnd);
            }
        }
    }

    for window in workspace.floating_windows() {
        hwnds.push(window.hwnd);
    }

    hwnds.retain(|hwnd| WindowsApi::is_window(*hwnd));
    hwnds
}

struct Command {
    hmonitor: isize,
    work_area: Rect,
    outgoing: Vec<isize>,
    incoming: Vec<isize>,
    forward: bool,
    ack: Sender<()>,
}

struct Thumbnail {
    id: isize,
    hwnd: isize,
    incoming: bool,
    /// Rect of an outgoing window when the animation started (screen coordinates)
    start_rect: RECT,
}

struct Animation {
    overlay: isize,
    work_area: Rect,
    wallpaper: Option<isize>,
    thumbnails: Vec<Thumbnail>,
    forward: bool,
    kind: WorkspaceAnimationKind,
    started: Instant,
    duration: Duration,
}

fn animator() -> &'static Sender<Command> {
    ANIMATOR.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::unbounded();
        std::thread::spawn(move || run(receiver));
        sender
    })
}

extern "system" fn overlay_callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn create_overlay() -> Option<isize> {
    let class_name = w!("komorebi-workspace-animation");
    let h_module = WindowsApi::module_handle_w().ok()?;

    let window_class = WNDCLASSW {
        hInstance: h_module.into(),
        lpszClassName: class_name,
        lpfnWndProc: Some(overlay_callback),
        hbrBackground: WindowsApi::create_solid_brush(0),
        ..Default::default()
    };

    let _ = WindowsApi::register_class_w(&window_class);

    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            class_name,
            class_name,
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(h_module.into()),
            None,
        )
    }
    .ok()?;

    Some(hwnd.0 as isize)
}

fn pump_messages() {
    let mut msg = MSG::default();
    unsafe {
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn raw_window_rect(hwnd: isize) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as _), &mut rect) }.ok()?;
    Some(rect)
}

/// Sorts window handles from the bottom of the z-order to the top, so that thumbnails
/// registered in this order are drawn with the same stacking as the real windows
fn sort_bottom_to_top(hwnds: &mut [isize]) {
    let mut z_order = HashMap::new();
    let mut current = unsafe { GetTopWindow(None) }.ok();
    let mut idx = 0usize;

    while let Some(hwnd) = current {
        z_order.insert(hwnd.0 as isize, idx);
        idx += 1;
        current = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
    }

    hwnds.sort_by_key(|hwnd| std::cmp::Reverse(z_order.get(hwnd).copied().unwrap_or(0)));
}

fn register(overlay: isize, source: isize) -> Option<isize> {
    unsafe { DwmRegisterThumbnail(HWND(overlay as _), HWND(source as _)) }.ok()
}

fn update(id: isize, destination: RECT, source: Option<RECT>, opacity: u8) {
    let mut flags = DWM_TNP_RECTDESTINATION
        | DWM_TNP_OPACITY
        | DWM_TNP_VISIBLE
        | DWM_TNP_SOURCECLIENTAREAONLY;

    if source.is_some() {
        flags |= DWM_TNP_RECTSOURCE;
    }

    let properties = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: flags,
        rcDestination: destination,
        rcSource: source.unwrap_or_default(),
        opacity,
        fVisible: true.into(),
        fSourceClientAreaOnly: false.into(),
    };

    let _ = unsafe { DwmUpdateThumbnailProperties(id, &properties) };
}

fn desktop_window() -> Option<isize> {
    unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }
        .ok()
        .map(|hwnd| hwnd.0 as isize)
}

/// Solves a CSS-style cubic bezier for y at a given x
fn cubic_bezier(curve: [f64; 4], x: f64) -> f64 {
    let [x1, y1, x2, y2] = curve;
    let sample = |a: f64, b: f64, t: f64| {
        3.0 * a * t * (1.0 - t).powi(2) + 3.0 * b * t.powi(2) * (1.0 - t) + t.powi(3)
    };

    // bisection is plenty fast and stable for a handful of thumbnails per frame
    let (mut low, mut high) = (0.0, 1.0);
    let mut t = x;
    for _ in 0..30 {
        let current = sample(x1, x2, t);
        if (current - x).abs() < 1e-5 {
            break;
        }

        if current < x {
            low = t;
        } else {
            high = t;
        }

        t = (low + high) / 2.0;
    }

    sample(y1, y2, t)
}

impl Animation {
    fn start(command: &Command, overlay: isize) -> Self {
        let config = CONFIG.lock().clone();
        let duration = config.duration.unwrap_or_else(|| {
            ANIMATION_DURATION_PER_ANIMATION
                .lock()
                .get(&AnimationPrefix::Workspace)
                .copied()
                .unwrap_or_else(|| ANIMATION_DURATION_GLOBAL.load(Ordering::SeqCst))
        });

        let wa = command.work_area;
        unsafe {
            let _ = SetWindowPos(
                HWND(overlay as _),
                Some(HWND_TOPMOST),
                wa.left,
                wa.top,
                wa.right,
                wa.bottom,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        let wallpaper = desktop_window().and_then(|desktop| register(overlay, desktop));

        let mut outgoing = command.outgoing.clone();
        let mut incoming = command.incoming.clone();
        sort_bottom_to_top(&mut outgoing);
        sort_bottom_to_top(&mut incoming);

        let mut thumbnails = vec![];
        for (hwnds, is_incoming) in [(outgoing, false), (incoming, true)] {
            for hwnd in hwnds {
                if let Some(id) = register(overlay, hwnd) {
                    thumbnails.push(Thumbnail {
                        id,
                        hwnd,
                        incoming: is_incoming,
                        start_rect: raw_window_rect(hwnd).unwrap_or_default(),
                    });
                }
            }
        }

        let animation = Self {
            overlay,
            work_area: wa,
            wallpaper,
            thumbnails,
            forward: command.forward,
            kind: config.kind.unwrap_or_default(),
            started: Instant::now(),
            duration: Duration::from_millis(duration.max(1)),
        };

        animation.render(0.0);
        animation
    }

    fn progress(&self) -> f64 {
        (self.started.elapsed().as_secs_f64() / self.duration.as_secs_f64()).min(1.0)
    }

    fn eased(progress: f64) -> f64 {
        if let Some(curve) = CONFIG.lock().bezier {
            return cubic_bezier(curve, progress);
        }

        let style = ANIMATION_STYLE_PER_ANIMATION
            .lock()
            .get(&AnimationPrefix::Workspace)
            .copied()
            .unwrap_or_else(|| *ANIMATION_STYLE_GLOBAL.lock());

        apply_ease_func(progress, style)
    }

    fn render(&self, progress: f64) {
        let wa = self.work_area;
        let eased = Self::eased(progress);
        let slide_fade_distance = CONFIG.lock().slide_fade_distance.unwrap_or(0.2);

        // the wallpaper part of the desktop window which sits behind the work area
        if let Some(wallpaper) = self.wallpaper {
            let desktop = desktop_window().and_then(raw_window_rect).unwrap_or_default();
            let source = RECT {
                left: wa.left - desktop.left,
                top: wa.top - desktop.top,
                right: wa.left - desktop.left + wa.right,
                bottom: wa.top - desktop.top + wa.bottom,
            };

            update(
                wallpaper,
                RECT {
                    left: 0,
                    top: 0,
                    right: wa.right,
                    bottom: wa.bottom,
                },
                Some(source),
                255,
            );
        }

        let (vertical, distance, fade) = match self.kind {
            WorkspaceAnimationKind::Slide => (false, 1.0, false),
            WorkspaceAnimationKind::SlideVertical => (true, 1.0, false),
            WorkspaceAnimationKind::Fade => (false, 0.0, true),
            WorkspaceAnimationKind::SlideFade => (false, slide_fade_distance, true),
            WorkspaceAnimationKind::SlideFadeVertical => (true, slide_fade_distance, true),
        };

        let extent = f64::from(if vertical { wa.bottom } else { wa.right }) * distance;
        let direction = if self.forward { -1.0 } else { 1.0 };

        for thumbnail in &self.thumbnails {
            // incoming windows track their live position, so that the thumbnails always
            // land exactly where the real windows have been laid out
            let rect = if thumbnail.incoming {
                raw_window_rect(thumbnail.hwnd).unwrap_or(thumbnail.start_rect)
            } else {
                thumbnail.start_rect
            };

            // outgoing windows travel one extent in the direction of travel, while incoming
            // windows start one extent away on the opposite side and arrive in place
            let (offset, opacity) = if thumbnail.incoming {
                (-direction * extent * (1.0 - eased), eased.clamp(0.0, 1.0))
            } else {
                (direction * extent * eased, (1.0 - eased).clamp(0.0, 1.0))
            };

            #[allow(clippy::cast_possible_truncation)]
            let offset = offset.round() as i32;
            let (dx, dy) = if vertical { (0, offset) } else { (offset, 0) };

            let destination = RECT {
                left: rect.left - wa.left + dx,
                top: rect.top - wa.top + dy,
                right: rect.right - wa.left + dx,
                bottom: rect.bottom - wa.top + dy,
            };

            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let opacity = if fade {
                (opacity * 255.0).round() as u8
            } else {
                255
            };

            update(thumbnail.id, destination, None, opacity);
        }
    }

    fn finish(self, hide_overlay: bool) {
        for thumbnail in &self.thumbnails {
            let _ = unsafe { DwmUnregisterThumbnail(thumbnail.id) };
        }

        if let Some(wallpaper) = self.wallpaper {
            let _ = unsafe { DwmUnregisterThumbnail(wallpaper) };
        }

        if hide_overlay {
            unsafe {
                let _ = ShowWindow(HWND(self.overlay as _), SW_HIDE);
            }
        }
    }
}

fn run(receiver: Receiver<Command>) {
    let mut overlays: HashMap<isize, isize> = HashMap::new();
    let mut animations: HashMap<isize, Animation> = HashMap::new();

    loop {
        let command = if animations.is_empty() {
            ANIMATION_RUNNING.store(false);
            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => Some(command),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            receiver.try_recv().ok()
        };

        if let Some(command) = command {
            ANIMATION_RUNNING.store(true);

            // a new switch on a monitor which is still animating replaces that animation;
            // the overlay stays up so there is no flash of the real windows in between
            if let Some(previous) = animations.remove(&command.hmonitor) {
                previous.finish(false);
            }

            let overlay = match overlays.get(&command.hmonitor) {
                Some(overlay) => Some(*overlay),
                None => create_overlay().inspect(|overlay| {
                    overlays.insert(command.hmonitor, *overlay);
                }),
            };

            if let Some(overlay) = overlay {
                let animation = Animation::start(&command, overlay);
                // make sure the overlay has been composited before the real switch happens
                let _ = unsafe { DwmFlush() };
                animations.insert(command.hmonitor, animation);
            }

            let _ = command.ack.send(());
            pump_messages();
            continue;
        }

        let mut finished = vec![];
        for (hmonitor, animation) in &animations {
            let progress = animation.progress();
            animation.render(progress);
            if progress >= 1.0 {
                finished.push(*hmonitor);
            }
        }

        if !animations.is_empty() {
            if unsafe { DwmFlush() }.is_err() {
                std::thread::sleep(Duration::from_millis(4));
            }
        }

        for hmonitor in finished {
            if let Some(animation) = animations.remove(&hmonitor) {
                animation.finish(true);
            }
        }

        pump_messages();
    }
}
