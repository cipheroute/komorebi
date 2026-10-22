//! Workspace switch animations
//!
//! Real windows are never moved during a workspace switch animation. Instead, a
//! topmost overlay window is placed over the work area of the monitor, and live DWM
//! thumbnails of the desktop wallpaper, the outgoing windows and the incoming windows
//! are composited onto it by DWM on the GPU. Thumbnails keep rendering even for cloaked
//! windows, so the real workspace switch happens instantly underneath the overlay, and
//! the overlay is removed once the thumbnails have reached the real window positions.
//!
//! Window moves and resizes within a workspace use the same overlay: the real windows are
//! placed at their new positions once, underneath it, and their thumbnails are stretched
//! from the old rects to the new ones, so that applications don't have to redraw at every
//! intermediate size.

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
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::OnceLock;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::AC_SRC_ALPHA;
use windows::Win32::Graphics::Gdi::AC_SRC_OVER;
use windows::Win32::Graphics::Gdi::BI_RGB;
use windows::Win32::Graphics::Gdi::BITMAPINFO;
use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
use windows::Win32::Graphics::Gdi::BLENDFUNCTION;
use windows::Win32::Graphics::Gdi::CreateCompatibleDC;
use windows::Win32::Graphics::Gdi::CreateDIBSection;
use windows::Win32::Graphics::Gdi::DIB_RGB_COLORS;
use windows::Win32::Graphics::Gdi::DeleteDC;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::Graphics::Gdi::GetDC;
use windows::Win32::Graphics::Gdi::HALFTONE;
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::Gdi::ReleaseDC;
use windows::Win32::Graphics::Gdi::SRCCOPY;
use windows::Win32::Graphics::Gdi::SelectObject;
use windows::Win32::Graphics::Gdi::SetBrushOrgEx;
use windows::Win32::Graphics::Gdi::SetStretchBltMode;
use windows::Win32::Graphics::Gdi::StretchBlt;
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
use windows::Win32::Graphics::Dwm::DWMWA_EXTENDED_FRAME_BOUNDS;
use windows::Win32::Graphics::Dwm::DwmFlush;
use windows::Win32::Graphics::Dwm::DwmGetWindowAttribute;
use windows::Win32::Graphics::Dwm::DwmRegisterThumbnail;
use windows::Win32::Graphics::Dwm::DwmUnregisterThumbnail;
use windows::Win32::Graphics::Dwm::DwmUpdateThumbnailProperties;
use windows::Win32::UI::WindowsAndMessaging::CreateWindowExW;
use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
use windows::Win32::UI::WindowsAndMessaging::DispatchMessageW;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT;
use windows::Win32::UI::WindowsAndMessaging::GWL_EXSTYLE;
use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
use windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW;
use windows::Win32::UI::WindowsAndMessaging::GetTopWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
use windows::Win32::UI::WindowsAndMessaging::IsIconic;
use windows::Win32::UI::WindowsAndMessaging::SC_MINIMIZE;
use windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE;
use windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE;
use windows::Win32::UI::WindowsAndMessaging::WINDOWPOS;
use windows::Win32::UI::WindowsAndMessaging::WM_SYSCOMMAND;
use windows::Win32::UI::WindowsAndMessaging::WM_WINDOWPOSCHANGING;
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::Win32::UI::WindowsAndMessaging::PM_REMOVE;
use windows::Win32::UI::WindowsAndMessaging::PeekMessageW;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::ULW_ALPHA;
use windows::Win32::UI::WindowsAndMessaging::UpdateLayeredWindow;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
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
/// The wallpaper snapshot is shrunk by this factor before being blurred; DWM upscales
/// thumbnails with a coarse filter, so larger factors make the blur visibly blocky
const BLUR_DOWNSCALE: i32 = 2;
/// The system acrylic (as used by Zed's "blurred" background, ACCENT_ENABLE_ACRYLICBLURBEHIND
/// with a transparent tint) measured against the plain wallpaper on 2026-10-20: a gaussian
/// blur of ~30px, then per channel `0.83 * blurred + 7`, ie. ~17% of a dark grey mixed in
const ACRYLIC_GAIN: [f64; 3] = [0.829, 0.828, 0.839];
const ACRYLIC_OFFSET: [f64; 3] = [6.8, 7.3, 6.2];

/// How often the blurred wallpaper snapshot is retaken. Taking it makes live wallpaper
/// engines render, so it is kept rare; the blur hides how old the snapshot is.
const BLUR_MAX_AGE: Duration = Duration::from_secs(600);
/// Source windows for thumbnails of our own content live far outside of any monitor
const OFFSCREEN: i32 = -20000;

#[link(name = "user32")]
unsafe extern "system" {
    fn PrintWindow(hwnd: HWND, hdc: HDC, flags: u32) -> windows::core::BOOL;
}

const PW_RENDERFULLCONTENT: u32 = 2;

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
    /// Show the blurred wallpaper behind windows during the animation, so that windows with
    /// acrylic or mica backdrops don't turn see-through while they move (default: true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop_blur: Option<bool>,
    /// Blur strength (gaussian sigma) in pixels of the backdrop (default: 30, like system acrylic)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop_blur_radius: Option<u32>,
    /// Focus border drawn around the focused window during the animation (default: none)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<WorkspaceAnimationBorder>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// Window move and resize animation configuration. When enabled, windows are moved and resized
/// with stretched thumbnails on an overlay instead of the per-frame movement animation.
pub struct WindowAnimationConfig {
    /// Enable or disable overlay window animations (default: true when this section is present)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Animation duration in ms (default: follows the movement animation duration)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    /// A CSS-style cubic bezier curve `[x1, y1, x2, y2]` which overrides the animation style
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bezier: Option<[f64; 4]>,
}

lazy_static! {
    static ref CONFIG: Mutex<WorkspaceAnimationConfig> =
        Mutex::new(WorkspaceAnimationConfig::default());
    static ref WINDOW_CONFIG: Mutex<Option<WindowAnimationConfig>> = Mutex::new(None);
    /// Monitors (by hmonitor) on which a workspace switch is currently animating
    static ref SWITCHING_MONITORS: Mutex<HashSet<isize>> = Mutex::new(HashSet::new());
    /// The workspace index currently on screen for each monitor (by hmonitor)
    static ref SHOWN_WORKSPACES: Mutex<HashMap<isize, usize>> = Mutex::new(HashMap::new());
}

static ANIMATOR: OnceLock<Sender<Job>> = OnceLock::new();
static ANIMATION_RUNNING: AtomicCell<bool> = AtomicCell::new(false);

pub fn set_config(config: Option<WorkspaceAnimationConfig>) {
    *CONFIG.lock() = config.unwrap_or_default();

    // start the animator now, so that the blurred backdrop is ready for the first animation
    animator();
}

pub fn set_window_config(config: Option<WindowAnimationConfig>) {
    *WINDOW_CONFIG.lock() = config;
}

fn window_animations_enabled() -> bool {
    let Some(enabled) = WINDOW_CONFIG.lock().as_ref().map(|c| c.enabled.unwrap_or(true)) else {
        return false;
    };

    enabled
        && ANIMATION_ENABLED_PER_ANIMATION
            .lock()
            .get(&AnimationPrefix::Movement)
            .copied()
            .unwrap_or_else(|| ANIMATION_ENABLED_GLOBAL.load(Ordering::SeqCst))
}

/// Called by a workspace right before it moves its windows to `moves` (window, layout rect).
/// Returns true when an overlay animation now covers the work area, in which case the caller
/// must place the windows at their targets right away instead of animating them itself.
pub fn windows_moving(workspace: &Workspace, moves: &[(isize, Rect)]) -> bool {
    if !window_animations_enabled() || crate::animation::movement_animations_suppressed() {
        return false;
    }

    let shown = |hwnd: isize| {
        WindowsApi::is_window(hwnd) && !WindowsApi::is_window_cloaked(hwnd).unwrap_or(true)
    };

    // hidden workspaces are laid out too; only the one on screen is animated
    let changed = moves.iter().any(|(hwnd, target)| {
        shown(*hwnd) && WindowsApi::window_rect(*hwnd).is_ok_and(|rect| rect != *target)
    });
    if !changed {
        return false;
    }

    let wa = workspace.globals.work_area;
    let hmonitor = WindowsApi::monitor_from_point(POINT {
        x: wa.left + wa.right / 2,
        y: wa.top + wa.bottom / 2,
    });

    // a workspace switch overlay already shows these windows at their live positions
    if SWITCHING_MONITORS.lock().contains(&hmonitor) {
        return false;
    }

    let managed = visible_windows(workspace)
        .into_iter()
        .filter(|hwnd| shown(*hwnd))
        .collect::<Vec<_>>();

    // unmanaged windows on the monitor (eg. desktop pets, apps komorebi ignores) are put on
    // the overlay too, as they would otherwise vanish while it covers the work area
    let windows = managed
        .iter()
        .map(|hwnd| (*hwnd, true))
        .chain(unmanaged_windows(wa, &managed).into_iter().map(|hwnd| (hwnd, false)))
        .filter_map(|(hwnd, managed)| {
            let target = moves.iter().find(|(h, _)| *h == hwnd).map(|(_, layout)| RECT {
                left: layout.left,
                top: layout.top,
                right: layout.left + layout.right,
                bottom: layout.top + layout.bottom,
            });

            Some(WindowMove {
                hwnd,
                frame: frame_rect(hwnd).or_else(|| raw_window_rect(hwnd))?,
                target,
                managed,
            })
        })
        .collect::<Vec<_>>();

    let focused = workspace
        .monocle_container
        .as_ref()
        .and_then(|c| c.focused_window())
        .or_else(|| workspace.focused_container().and_then(|c| c.focused_window()))
        .map(|window| window.hwnd);

    let (ack_sender, ack_receiver) = crossbeam_channel::bounded(1);
    let job = Job::Windows(WindowsCommand {
        hmonitor,
        work_area: wa,
        windows,
        focused,
        ack: ack_sender,
    });

    if animator().send(job).is_err() {
        return false;
    }

    // even if the overlay is late, the thumbnails track the live window positions, so the
    // windows still have to be placed directly
    let _ = ack_receiver.recv_timeout(OVERLAY_READY_TIMEOUT);
    true
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
    let focused = [previous_idx, shown_idx].map(|idx| {
        workspaces.get(idx).and_then(|workspace| {
            workspace
                .monocle_container
                .as_ref()
                .and_then(|c| c.focused_window())
                .or(workspace.maximized_window.as_ref())
                .or_else(|| workspace.focused_container().and_then(|c| c.focused_window()))
                .map(|window| window.hwnd)
        })
    });

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
        focused,
        ack: ack_sender,
    };

    SWITCHING_MONITORS.lock().insert(hmonitor);
    if animator().send(Job::Workspace(command)).is_ok() {
        let _ = ack_receiver.recv_timeout(OVERLAY_READY_TIMEOUT);
    } else {
        SWITCHING_MONITORS.lock().remove(&hmonitor);
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
    /// The focused windows of the outgoing and the incoming workspace
    focused: [Option<isize>; 2],
    ack: Sender<()>,
}

struct WindowMove {
    hwnd: isize,
    /// The visible frame before the move
    frame: RECT,
    /// The visible frame komorebi lays the window out at, if it is being placed
    target: Option<RECT>,
    /// Managed by the workspace, as opposed to some other window over the work area
    managed: bool,
}

/// Windows of the workspace on screen are about to be moved or resized
struct WindowsCommand {
    hmonitor: isize,
    work_area: Rect,
    /// Every visible window over the work area
    windows: Vec<WindowMove>,
    focused: Option<isize>,
    ack: Sender<()>,
}

enum Job {
    Workspace(Command),
    Windows(WindowsCommand),
}

struct Thumbnail {
    id: isize,
    hwnd: isize,
    /// Blurred wallpaper drawn right underneath the window, standing in for its backdrop
    blur_patch: Option<isize>,
    incoming: bool,
    /// Rect of an outgoing window when the animation started, or of any window when a window
    /// animation started (screen coordinates)
    start_rect: RECT,
    /// Where the thumbnail was last drawn (screen coordinates)
    current: RECT,
    /// Window animations: the frame of the window before it was placed, and where komorebi
    /// placed it. Some windows report their new frame a few frames late, so the layout target
    /// is used until the window has visibly moved, and the live frame (which is where windows
    /// with a minimum size really end up) after that.
    placed_from: RECT,
    target: Option<RECT>,
}

/// A copy of a thumbnail's fields, so that rendering can update borders while iterating
struct ThumbnailView {
    id: isize,
    hwnd: isize,
    incoming: bool,
    blur_patch: Option<isize>,
    start_rect: RECT,
}

struct Animation {
    overlay: isize,
    work_area: Rect,
    wallpaper: Option<isize>,
    thumbnails: Vec<Thumbnail>,
    /// Desktop rect and pixel size of the blur snapshot the patches are cut from
    blur: Option<(RECT, i32, i32)>,
    /// Focus borders: thumbnail, surface, and whether it belongs to the incoming workspace
    borders: Vec<(isize, BlurSurface, bool)>,
    forward: bool,
    kind: WorkspaceAnimationKind,
    started: Instant,
    duration: Duration,
    focused: [Option<isize>; 2],
    /// Moves and resizes windows in place instead of switching workspaces
    windows: bool,
}

fn animator() -> &'static Sender<Job> {
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

/// Off-screen thumbnail sources must stay off-screen: "minimise all" (Win+D, Win+M)
/// otherwise parks them in the minimised row above the taskbar where they are visible
extern "system" fn surface_callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SYSCOMMAND if (wparam.0 & 0xFFF0) as u32 == SC_MINIMIZE => LRESULT(0),
        WM_WINDOWPOSCHANGING => {
            if let Some(pos) = unsafe { (lparam.0 as *mut WINDOWPOS).as_mut() }
                && !pos.flags.contains(SWP_NOMOVE)
            {
                pos.x = OFFSCREEN;
                pos.y = OFFSCREEN;
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
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

/// Shows the overlay once its thumbnails are in place; shown any earlier, it can be
/// composited for a frame or two as a plain black window
fn show_overlay(overlay: isize) {
    unsafe {
        let _ = SetWindowPos(
            HWND(overlay as _),
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
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

/// The visible bounds of a window, without the invisible resize borders. Window thumbnails
/// only contain this part of the window, so they have to be placed on it to render 1:1.
fn frame_rect(hwnd: isize) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe {
        DwmGetWindowAttribute(
            HWND(hwnd as _),
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut rect).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    }
    .ok()?;
    Some(rect)
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

/// Visible top-level windows over the work area which are not in `managed`, skipping tool
/// windows (which includes full screen overlays like the NVIDIA one), the desktop and borders
fn unmanaged_windows(work_area: Rect, managed: &[isize]) -> Vec<isize> {
    let mut hwnds = vec![];
    let mut current = unsafe { GetTopWindow(None) }.ok();

    while let Some(hwnd) = current {
        current = unsafe { GetWindow(hwnd, GW_HWNDNEXT) }.ok();
        let raw = hwnd.0 as isize;

        if managed.contains(&raw)
            || !WindowsApi::is_window_visible(raw)
            || WindowsApi::is_iconic(raw)
            || WindowsApi::is_window_cloaked(raw).unwrap_or(true)
        {
            continue;
        }

        let ex_style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
            continue;
        }

        let mut class = [0u16; 64];
        let len = unsafe { GetClassNameW(hwnd, &mut class) } as usize;
        let class = String::from_utf16_lossy(&class[..len]);
        if matches!(
            class.as_str(),
            "Progman" | "WorkerW" | "border" | "Shell_TrayWnd" | "Windows.UI.Core.CoreWindow"
        )
            || class.starts_with("komorebi")
        {
            continue;
        }

        let Some(rect) = raw_window_rect(raw) else {
            continue;
        };
        let overlaps = rect.left < work_area.left + work_area.right
            && rect.right > work_area.left
            && rect.top < work_area.top + work_area.bottom
            && rect.bottom > work_area.top;
        if overlaps && rect.right > rect.left && rect.bottom > rect.top {
            hwnds.push(raw);
        }
    }

    hwnds
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

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // top-down
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Box blurs BGRX pixels in place, three passes approximate a gaussian blur
fn box_blur(pixels: &mut [u32], width: usize, height: usize, radius: usize) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }

    let mut scratch = pixels.to_vec();
    for _ in 0..3 {
        blur_pass(pixels, &mut scratch, width, height, radius, true);
        blur_pass(&scratch, pixels, width, height, radius, false);
    }
}

fn blur_pass(
    source: &[u32],
    target: &mut [u32],
    width: usize,
    height: usize,
    radius: usize,
    horizontal: bool,
) {
    let (lines, length) = if horizontal {
        (height, width)
    } else {
        (width, height)
    };
    let index = |line: usize, i: usize| {
        if horizontal {
            line * width + i
        } else {
            i * width + line
        }
    };

    let window = (radius * 2 + 1) as u32;
    for line in 0..lines {
        let channel = |pixel: u32, shift: u32| (pixel >> shift) & 0xff;
        let mut sums = [0u32; 3];
        for offset in 0..window as usize {
            let i = offset.saturating_sub(radius).min(length - 1);
            let pixel = source[index(line, i)];
            for (c, shift) in [16, 8, 0].into_iter().enumerate() {
                sums[c] += channel(pixel, shift);
            }
        }

        for i in 0..length {
            target[index(line, i)] = 0xff00_0000
                | (sums[0] / window) << 16
                | (sums[1] / window) << 8
                | (sums[2] / window);

            let leaving = source[index(line, i.saturating_sub(radius))];
            let entering = source[index(line, (i + radius + 1).min(length - 1))];
            for (c, shift) in [16, 8, 0].into_iter().enumerate() {
                sums[c] = sums[c] + channel(entering, shift) - channel(leaving, shift);
            }
        }
    }
}

/// A blurred, shrunk snapshot of the desktop window, taken on a thread of its own since
/// PrintWindow waits on explorer
struct BlurSnapshot {
    desktop: RECT,
    width: i32,
    height: i32,
    pixels: Vec<u32>,
}

impl BlurSnapshot {
    fn capture(radius: u32) -> Option<Self> {
        let desktop_hwnd = desktop_window()?;
        let desktop = raw_window_rect(desktop_hwnd)?;
        let (width, height) = (desktop.right - desktop.left, desktop.bottom - desktop.top);
        let (small_w, small_h) = (
            (width / BLUR_DOWNSCALE).max(1),
            (height / BLUR_DOWNSCALE).max(1),
        );

        let mut pixels = vec![0u32; (small_w * small_h) as usize];

        unsafe {
            let screen = GetDC(None);
            let full_dc = CreateCompatibleDC(Some(screen));
            let small_dc = CreateCompatibleDC(Some(screen));
            let mut full_bits = std::ptr::null_mut();
            let mut small_bits = std::ptr::null_mut();

            let full = CreateDIBSection(
                Some(full_dc),
                &bitmap_info(width, height),
                DIB_RGB_COLORS,
                &mut full_bits,
                None,
                0,
            );
            let small = CreateDIBSection(
                Some(small_dc),
                &bitmap_info(small_w, small_h),
                DIB_RGB_COLORS,
                &mut small_bits,
                None,
                0,
            );

            if let (Ok(full), Ok(small)) = (&full, &small) {
                let previous_full = SelectObject(full_dc, (*full).into());
                let previous_small = SelectObject(small_dc, (*small).into());

                let _ = PrintWindow(HWND(desktop_hwnd as _), full_dc, PW_RENDERFULLCONTENT);
                SetStretchBltMode(small_dc, HALFTONE);
                let _ = SetBrushOrgEx(small_dc, 0, 0, None);
                let _ = StretchBlt(
                    small_dc,
                    0,
                    0,
                    small_w,
                    small_h,
                    Some(full_dc),
                    0,
                    0,
                    width,
                    height,
                    SRCCOPY,
                );

                std::ptr::copy_nonoverlapping(
                    small_bits.cast::<u32>(),
                    pixels.as_mut_ptr(),
                    pixels.len(),
                );

                SelectObject(full_dc, previous_full);
                SelectObject(small_dc, previous_small);
            }

            if let Ok(full) = full {
                let _ = DeleteObject(full.into());
            }
            if let Ok(small) = small {
                let _ = DeleteObject(small.into());
            }
            let _ = DeleteDC(full_dc);
            let _ = DeleteDC(small_dc);
            ReleaseDC(None, screen);
        }

        // three box blur passes of radius r make a gaussian with sigma^2 = ((2r+1)^2 - 1) / 4
        let sigma = f64::from(radius) / f64::from(BLUR_DOWNSCALE);
        let box_radius = (((4.0 * sigma * sigma + 1.0).sqrt() - 1.0) / 2.0).round().max(1.0);
        box_blur(
            &mut pixels,
            small_w as usize,
            small_h as usize,
            box_radius as usize,
        );

        // the luminosity layer of the system acrylic
        for pixel in &mut pixels {
            let channel = |shift: u32, c: usize| {
                let value = f64::from((*pixel >> shift) & 0xff);
                ((value * ACRYLIC_GAIN[c] + ACRYLIC_OFFSET[c]).round().clamp(0.0, 255.0)) as u32
            };
            *pixel = 0xff00_0000 | channel(16, 0) << 16 | channel(8, 1) << 8 | channel(0, 2);
        }

        Some(Self {
            desktop,
            width: small_w,
            height: small_h,
            pixels,
        })
    }
}

/// An off-screen layered window holding a bitmap (the blur snapshot or a focus border),
/// used as a thumbnail source
struct BlurSurface {
    hwnd: isize,
    desktop: RECT,
    width: i32,
    height: i32,
    taken: Instant,
}

impl BlurSurface {
    fn new() -> Option<Self> {
        let class_name = w!("komorebi-workspace-animation-blur");
        let h_module = WindowsApi::module_handle_w().ok()?;
        let window_class = WNDCLASSW {
            hInstance: h_module.into(),
            lpszClassName: class_name,
            lpfnWndProc: Some(surface_callback),
            ..Default::default()
        };
        let _ = WindowsApi::register_class_w(&window_class);

        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class_name,
                class_name,
                WS_POPUP,
                OFFSCREEN,
                OFFSCREEN,
                1,
                1,
                None,
                None,
                Some(h_module.into()),
                None,
            )
        }
        .ok()?;

        Some(Self {
            hwnd: hwnd.0 as isize,
            desktop: RECT::default(),
            width: 0,
            height: 0,
            taken: Instant::now(),
        })
    }

    /// A minimised source gives an empty thumbnail, so restore it if something minimised it
    fn ensure_restored(&self) {
        unsafe {
            if IsIconic(HWND(self.hwnd as _)).as_bool() {
                let _ = ShowWindow(HWND(self.hwnd as _), SW_SHOWNOACTIVATE);
            }
        }
    }

    fn upload(&mut self, snapshot: &BlurSnapshot) {
        self.upload_pixels(snapshot.width, snapshot.height, &snapshot.pixels);
        self.desktop = snapshot.desktop;
        self.taken = Instant::now();
    }

    /// Uploads premultiplied BGRA pixels
    fn upload_pixels(&mut self, width: i32, height: i32, pixels: &[u32]) {
        if width <= 0 || height <= 0 || pixels.len() != (width * height) as usize {
            return;
        }

        unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let mut bits = std::ptr::null_mut();
            if let Ok(bitmap) = CreateDIBSection(
                Some(memory),
                &bitmap_info(width, height),
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            ) {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
                let previous = SelectObject(memory, bitmap.into());
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };

                let _ = UpdateLayeredWindow(
                    HWND(self.hwnd as _),
                    Some(screen),
                    Some(&POINT {
                        x: OFFSCREEN,
                        y: OFFSCREEN,
                    }),
                    Some(&SIZE {
                        cx: width,
                        cy: height,
                    }),
                    Some(memory),
                    Some(&POINT { x: 0, y: 0 }),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );

                SelectObject(memory, previous);
                let _ = DeleteObject(bitmap.into());
                let _ = ShowWindow(HWND(self.hwnd as _), SW_SHOWNOACTIVATE);
            }
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);
        }

        self.width = width;
        self.height = height;
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
/// A focus border drawn around the focused window while a workspace switch animates, so that
/// the focus highlight (eg. from komorebi or tacky-borders) doesn't disappear and pop back in
pub struct WorkspaceAnimationBorder {
    /// Border width in pixels (default: 3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
    /// Border offset from the window edge in pixels, negative values move it inwards (default: -1)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<f64>,
    /// Corner radius of the window in pixels (default: 8)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
    /// Border colour as #RRGGBB or #RRGGBBAA (default: #FFFFFFFF)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
}

fn parse_colour(colour: &str) -> (u8, u8, u8, u8) {
    let hex = colour.trim_start_matches('#');
    let channel = |idx: usize| {
        hex.get(idx..idx + 2)
            .and_then(|c| u8::from_str_radix(c, 16).ok())
    };

    match hex.len() {
        6 | 8 => (
            channel(0).unwrap_or(255),
            channel(2).unwrap_or(255),
            channel(4).unwrap_or(255),
            channel(6).unwrap_or(255),
        ),
        _ => (255, 255, 255, 255),
    }
}

/// Like tacky-borders, the outer edge of the border sits (width + offset) pixels outside of
/// the visible frame of the window
fn border_expansion(border: &WorkspaceAnimationBorder) -> i32 {
    (border.width.unwrap_or(3.0) + border.offset.unwrap_or(-1.0)).round() as i32
}

/// Rasterises an anti-aliased rounded rectangle outline, whose outer edge is the edge of
/// the bitmap, into premultiplied BGRA pixels
fn render_border(width: i32, height: i32, border: &WorkspaceAnimationBorder) -> Vec<u32> {
    let stroke = border.width.unwrap_or(3.0).max(0.5);
    let radius = (border.radius.unwrap_or(8.0) + f64::from(border_expansion(border))).max(0.0);
    let (r, g, b, a) = parse_colour(border.colour.as_deref().unwrap_or("#FFFFFFFF"));

    let (w, h) = (f64::from(width), f64::from(height));
    let (half_w, half_h) = (w / 2.0, h / 2.0);
    let mut pixels = vec![0u32; (width * height) as usize];

    // pixels further than this from every edge can't be touched by the stroke
    let reach = (stroke + radius + 1.0).ceil() as i32;

    for y in 0..height {
        let near_y = y < reach || y >= height - reach;
        for x in 0..width {
            if !near_y && x >= reach && x < width - reach {
                continue;
            }

            let px = (f64::from(x) + 0.5 - half_w).abs() - (half_w - radius);
            let py = (f64::from(y) + 0.5 - half_h).abs() - (half_h - radius);
            let distance = px.max(0.0).hypot(py.max(0.0)) + px.max(py).min(0.0) - radius;

            let coverage =
                (0.5 - distance).clamp(0.0, 1.0) * (0.5 + distance + stroke).clamp(0.0, 1.0);

            if coverage > 0.0 {
                let alpha = coverage * f64::from(a) / 255.0;
                let premultiply = |c: u8| (f64::from(c) * alpha).round() as u32;
                pixels[(y * width + x) as usize] = ((alpha * 255.0).round() as u32) << 24
                    | premultiply(r) << 16
                    | premultiply(g) << 8
                    | premultiply(b);
            }
        }
    }

    pixels
}

impl Animation {
    fn start(
        command: &Command,
        overlay: isize,
        blur: Option<&BlurSurface>,
        border_surfaces: &mut Vec<BlurSurface>,
    ) -> Self {
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
                SWP_NOACTIVATE,
            );
        }

        let wallpaper = desktop_window().and_then(|desktop| register(overlay, desktop));

        let mut outgoing = command.outgoing.clone();
        let mut incoming = command.incoming.clone();
        sort_bottom_to_top(&mut outgoing);
        sort_bottom_to_top(&mut incoming);

        let blur = blur.filter(|blur| blur.width > 0 && config.backdrop_blur.unwrap_or(true));
        let fades = !matches!(
            config.kind.unwrap_or_default(),
            WorkspaceAnimationKind::Slide | WorkspaceAnimationKind::SlideVertical
        );
        let blur = blur.filter(|_| !fades);
        if let Some(blur) = blur {
            blur.ensure_restored();
        }

        let mut thumbnails = vec![];
        for (hwnds, is_incoming) in [(outgoing, false), (incoming, true)] {
            for hwnd in hwnds {
                // registration order is drawing order, so each window's blur patch goes
                // right underneath it
                let blur_patch = blur.and_then(|blur| register(overlay, blur.hwnd));
                if let Some(id) = register(overlay, hwnd) {
                    let start_rect = frame_rect(hwnd)
                        .or_else(|| raw_window_rect(hwnd))
                        .unwrap_or_default();
                    thumbnails.push(Thumbnail {
                        id,
                        hwnd,
                        blur_patch,
                        incoming: is_incoming,
                        start_rect,
                        current: start_rect,
                        placed_from: start_rect,
                        target: None,
                    });
                }
            }
        }

        // borders are registered last so that they are drawn on top of every window
        let mut borders = vec![];
        if config.border.is_some() {
            for (incoming, focused) in [(false, command.focused[0]), (true, command.focused[1])] {
                if focused.is_some_and(|f| thumbnails.iter().any(|t| t.hwnd == f))
                    && let Some(surface) = border_surfaces.pop().or_else(BlurSurface::new)
                {
                    surface.ensure_restored();
                    match register(overlay, surface.hwnd) {
                        Some(id) => borders.push((id, surface, incoming)),
                        None => border_surfaces.push(surface),
                    }
                }
            }
        }

        let mut animation = Self {
            overlay,
            work_area: wa,
            wallpaper,
            thumbnails,
            borders,
            blur: blur.map(|blur| (blur.desktop, blur.width, blur.height)),
            forward: command.forward,
            kind: config.kind.unwrap_or_default(),
            started: Instant::now(),
            duration: Duration::from_millis(duration.max(1)),
            focused: command.focused,
            windows: false,
        };

        animation.render(0.0);
        show_overlay(overlay);
        animation
    }

    /// Starts moving and resizing windows in place. Windows which were still animating on
    /// `previous` continue from where they are drawn, so rapid changes chain smoothly.
    fn start_windows(
        command: &WindowsCommand,
        overlay: isize,
        blur: Option<&BlurSurface>,
        border_surfaces: &mut Vec<BlurSurface>,
        previous: Option<&Animation>,
    ) -> Self {
        let config = WINDOW_CONFIG.lock().clone().unwrap_or_default();
        let duration = config.duration.unwrap_or_else(|| {
            ANIMATION_DURATION_PER_ANIMATION
                .lock()
                .get(&AnimationPrefix::Movement)
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
                SWP_NOACTIVATE,
            );
        }

        let wallpaper = desktop_window().and_then(|desktop| register(overlay, desktop));

        let mut hwnds = command.windows.iter().map(|window| window.hwnd).collect::<Vec<_>>();
        sort_bottom_to_top(&mut hwnds);

        let blur = blur.filter(|blur| blur.width > 0 && CONFIG.lock().backdrop_blur.unwrap_or(true));
        if let Some(blur) = blur {
            blur.ensure_restored();
        }

        let mut thumbnails = vec![];
        for hwnd in hwnds {
            let Some(window) = command.windows.iter().find(|window| window.hwnd == hwnd) else {
                continue;
            };
            let start_rect = previous
                .and_then(|previous| previous.thumbnails.iter().find(|t| t.hwnd == hwnd))
                .map_or(window.frame, |thumbnail| thumbnail.current);

            // unmanaged windows can be any shape (eg. a desktop pet), so they get no backdrop
            let blur_patch = blur
                .filter(|_| window.managed)
                .and_then(|blur| register(overlay, blur.hwnd));
            if let Some(id) = register(overlay, hwnd) {
                thumbnails.push(Thumbnail {
                    id,
                    hwnd,
                    blur_patch,
                    incoming: true,
                    start_rect,
                    current: start_rect,
                    placed_from: window.frame,
                    target: window.target,
                });
            }
        }

        let mut borders = vec![];
        if CONFIG.lock().border.is_some()
            && command.focused.is_some_and(|f| thumbnails.iter().any(|t| t.hwnd == f))
            && let Some(surface) = border_surfaces.pop().or_else(BlurSurface::new)
        {
            surface.ensure_restored();
            match register(overlay, surface.hwnd) {
                Some(id) => borders.push((id, surface, true)),
                None => border_surfaces.push(surface),
            }
        }

        let mut animation = Self {
            overlay,
            work_area: wa,
            wallpaper,
            thumbnails,
            borders,
            blur: blur.map(|blur| (blur.desktop, blur.width, blur.height)),
            forward: true,
            kind: WorkspaceAnimationKind::Slide,
            started: Instant::now(),
            duration: Duration::from_millis(duration.max(1)),
            focused: [None, command.focused],
            windows: true,
        };

        animation.render(0.0);
        show_overlay(overlay);
        animation
    }

    fn progress(&self) -> f64 {
        (self.started.elapsed().as_secs_f64() / self.duration.as_secs_f64()).min(1.0)
    }

    fn eased(&self, progress: f64) -> f64 {
        let (curve, prefix) = if self.windows {
            (
                WINDOW_CONFIG.lock().as_ref().and_then(|c| c.bezier),
                AnimationPrefix::Movement,
            )
        } else {
            (CONFIG.lock().bezier, AnimationPrefix::Workspace)
        };

        if let Some(curve) = curve {
            return cubic_bezier(curve, progress);
        }

        let style = ANIMATION_STYLE_PER_ANIMATION
            .lock()
            .get(&prefix)
            .copied()
            .unwrap_or_else(|| *ANIMATION_STYLE_GLOBAL.lock());

        apply_ease_func(progress, style)
    }

    fn render(&mut self, progress: f64) {
        let wa = self.work_area;
        let eased = self.eased(progress);
        let slide_fade_distance = CONFIG.lock().slide_fade_distance.unwrap_or(0.2);
        let config_border = CONFIG.lock().border.clone();

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

        for idx in 0..self.thumbnails.len() {
            let thumbnail = &self.thumbnails[idx];
            let (thumbnail_id, thumbnail_hwnd, thumbnail_incoming, thumbnail_patch, start_rect) = (
                thumbnail.id,
                thumbnail.hwnd,
                thumbnail.incoming,
                thumbnail.blur_patch,
                thumbnail.start_rect,
            );
            let thumbnail = ThumbnailView {
                id: thumbnail_id,
                hwnd: thumbnail_hwnd,
                incoming: thumbnail_incoming,
                blur_patch: thumbnail_patch,
                start_rect,
            };
            // incoming windows track their live position, so that the thumbnails always
            // land exactly where the real windows have been laid out
            let rect = if thumbnail.incoming {
                frame_rect(thumbnail.hwnd).unwrap_or(thumbnail.start_rect)
            } else {
                thumbnail.start_rect
            };

            // window animations stretch each thumbnail from where the window was to where it
            // is now laid out
            let rect = if self.windows {
                let start = thumbnail.start_rect;
                let (placed_from, target) = (
                    self.thumbnails[idx].placed_from,
                    self.thumbnails[idx].target,
                );
                let rect = match target {
                    Some(target) if rect == placed_from => target,
                    _ => rect,
                };
                #[allow(clippy::cast_possible_truncation)]
                let lerp = |from: i32, to: i32| {
                    (f64::from(from) + f64::from(to - from) * eased).round() as i32
                };
                RECT {
                    left: lerp(start.left, rect.left),
                    top: lerp(start.top, rect.top),
                    right: lerp(start.right, rect.right),
                    bottom: lerp(start.bottom, rect.bottom),
                }
            } else {
                rect
            };

            // outgoing windows travel one extent in the direction of travel, while incoming
            // windows start one extent away on the opposite side and arrive in place
            let (offset, opacity) = if self.windows {
                (0.0, 1.0)
            } else if thumbnail.incoming {
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
            self.thumbnails[idx].current = RECT {
                left: destination.left + wa.left,
                top: destination.top + wa.top,
                right: destination.right + wa.left,
                bottom: destination.bottom + wa.top,
            };

            if let Some(border) = &config_border {
                let side = usize::from(thumbnail.incoming);
                if self.focused[side] == Some(thumbnail.hwnd)
                    && let Some((id, surface, _)) =
                        self.borders.iter_mut().find(|(_, _, incoming)| *incoming == thumbnail.incoming)
                {
                    let expand = border_expansion(border);
                    let rect = RECT {
                        left: destination.left - expand,
                        top: destination.top - expand,
                        right: destination.right + expand,
                        bottom: destination.bottom + expand,
                    };
                    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                    if width > 0 && height > 0 {
                        if surface.width != width || surface.height != height {
                            surface.upload_pixels(width, height, &render_border(width, height, border));
                        }
                        update(*id, rect, None, opacity);
                    }
                }
            }

            // the blurred wallpaper which is behind the window at its current position
            if let (Some(patch), Some((desktop, width, height))) = (thumbnail.blur_patch, self.blur)
            {
                let scale = f64::from(BLUR_DOWNSCALE);
                let to_snapshot = |value: i32, origin: i32, limit: i32| {
                    ((f64::from(value - origin) / scale).round() as i32).clamp(0, limit)
                };
                let source = RECT {
                    left: to_snapshot(destination.left + wa.left, desktop.left, width),
                    top: to_snapshot(destination.top + wa.top, desktop.top, height),
                    right: to_snapshot(destination.right + wa.left, desktop.left, width),
                    bottom: to_snapshot(destination.bottom + wa.top, desktop.top, height),
                };
                update(patch, destination, Some(source), opacity);
            }
        }
    }

    fn finish(self, hide_overlay: bool, border_surfaces: &mut Vec<BlurSurface>) {
        // hide the overlay before its thumbnails go, or it can be composited empty (black)
        if hide_overlay {
            unsafe {
                let _ = ShowWindow(HWND(self.overlay as _), SW_HIDE);
            }
        }

        for (id, surface, _) in self.borders {
            let _ = unsafe { DwmUnregisterThumbnail(id) };
            border_surfaces.push(surface);
        }

        for thumbnail in &self.thumbnails {
            let _ = unsafe { DwmUnregisterThumbnail(thumbnail.id) };
            if let Some(patch) = thumbnail.blur_patch {
                let _ = unsafe { DwmUnregisterThumbnail(patch) };
            }
        }

        if let Some(wallpaper) = self.wallpaper {
            let _ = unsafe { DwmUnregisterThumbnail(wallpaper) };
        }
    }
}

fn run(receiver: Receiver<Job>) {
    let mut overlays: HashMap<isize, isize> = HashMap::new();
    let mut animations: HashMap<isize, Animation> = HashMap::new();
    let mut blur: Option<BlurSurface> = None;
    let mut border_surfaces: Vec<BlurSurface> = vec![];
    let mut blur_job: Option<Receiver<Option<BlurSnapshot>>> = None;

    loop {
        let command = if animations.is_empty() {
            ANIMATION_RUNNING.store(false);

            // the blur snapshot is taken on its own thread and uploaded here while idle,
            // so no animation is using the surface while it changes
            if let Some(job) = &blur_job
                && let Ok(snapshot) = job.try_recv()
            {
                blur_job = None;
                if let Some(snapshot) = snapshot {
                    if blur.is_none() {
                        blur = BlurSurface::new();
                    }
                    if let Some(blur) = &mut blur {
                        blur.upload(&snapshot);
                    }
                }
            }

            let config = CONFIG.lock().clone();
            let blur_due = blur.as_ref().is_none_or(|blur| blur.taken.elapsed() > BLUR_MAX_AGE);
            if config.backdrop_blur.unwrap_or(true) && blur_due && blur_job.is_none() {
                let radius = config.backdrop_blur_radius.unwrap_or(30);
                let (sender, receiver) = crossbeam_channel::bounded(1);
                std::thread::spawn(move || {
                    let _ = sender.send(BlurSnapshot::capture(radius));
                });
                blur_job = Some(receiver);
            }

            match receiver.recv_timeout(Duration::from_millis(100)) {
                Ok(command) => Some(command),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        } else {
            receiver.try_recv().ok()
        };

        if let Some(job) = command {
            ANIMATION_RUNNING.store(true);

            let hmonitor = match &job {
                Job::Workspace(command) => command.hmonitor,
                Job::Windows(command) => command.hmonitor,
            };

            // a new animation on a monitor which is still animating replaces that animation;
            // the overlay stays up so there is no flash of the real windows in between
            let mut previous = animations.remove(&hmonitor);

            let overlay = match overlays.get(&hmonitor) {
                Some(overlay) => Some(*overlay),
                None => create_overlay().inspect(|overlay| {
                    overlays.insert(hmonitor, *overlay);
                }),
            };

            let animation = overlay.map(|overlay| match &job {
                Job::Workspace(command) => {
                    if let Some(previous) = previous.take() {
                        previous.finish(false, &mut border_surfaces);
                    }
                    Animation::start(command, overlay, blur.as_ref(), &mut border_surfaces)
                }
                Job::Windows(command) => Animation::start_windows(
                    command,
                    overlay,
                    blur.as_ref(),
                    &mut border_surfaces,
                    previous.as_ref(),
                ),
            });

            if let Some(previous) = previous {
                previous.finish(false, &mut border_surfaces);
            }

            if let Some(animation) = animation {
                // make sure the overlay has been composited before the real windows change
                let _ = unsafe { DwmFlush() };
                animations.insert(hmonitor, animation);
            } else if matches!(job, Job::Workspace(_)) {
                SWITCHING_MONITORS.lock().remove(&hmonitor);
            }

            let _ = match &job {
                Job::Workspace(command) => command.ack.send(()),
                Job::Windows(command) => command.ack.send(()),
            };
            pump_messages();
            continue;
        }

        let mut finished = vec![];
        for (hmonitor, animation) in &mut animations {
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
                if !animation.windows {
                    SWITCHING_MONITORS.lock().remove(&hmonitor);
                }
                animation.finish(true, &mut border_surfaces);
            }
        }

        pump_messages();
    }
}
