//! Workspace switch animations
//!
//! Real windows are never moved during a workspace switch animation. Instead, a
//! topmost overlay window is placed over the work area of the monitor, and live DWM
//! thumbnails are composited onto it by DWM on the GPU, from bottom to top:
//!
//! 1. a blurred snapshot of the desktop wallpaper (a tiny pre-blurred image which DWM
//!    upscales), which shows through translucent windows like acrylic or mica would
//! 2. the live, sharp desktop wallpaper, tiled over the area not covered by windows
//! 3. the outgoing and incoming windows
//! 4. a focus border around the focused window of each workspace
//!
//! Thumbnails keep rendering even for cloaked windows, so the real workspace switch
//! happens instantly underneath the overlay, which fades away once the thumbnails have
//! reached the real window positions.

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
use crate::workspace::WorkspaceLayer;

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
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Foundation::HWND;
use windows::Win32::Foundation::LPARAM;
use windows::Win32::Foundation::LRESULT;
use windows::Win32::Foundation::POINT;
use windows::Win32::Foundation::RECT;
use windows::Win32::Foundation::SIZE;
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
use windows::Win32::UI::WindowsAndMessaging::CreateWindowExW;
use windows::Win32::UI::WindowsAndMessaging::DefWindowProcW;
use windows::Win32::UI::WindowsAndMessaging::DestroyWindow;
use windows::Win32::UI::WindowsAndMessaging::DispatchMessageW;
use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
use windows::Win32::UI::WindowsAndMessaging::GW_HWNDNEXT;
use windows::Win32::UI::WindowsAndMessaging::GetTopWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindow;
use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
use windows::Win32::UI::WindowsAndMessaging::HWND_TOPMOST;
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::Win32::UI::WindowsAndMessaging::GetMessageW;
use windows::Win32::UI::WindowsAndMessaging::SendMessageW;
use windows::Win32::UI::WindowsAndMessaging::WM_APP;
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::SWP_SHOWWINDOW;
use windows::Win32::UI::WindowsAndMessaging::SetWindowPos;
use windows::Win32::UI::WindowsAndMessaging::ShowWindow;
use windows::Win32::UI::WindowsAndMessaging::TranslateMessage;
use windows::Win32::UI::WindowsAndMessaging::ULW_ALPHA;
use windows::Win32::UI::WindowsAndMessaging::UpdateLayeredWindow;
use windows::Win32::UI::WindowsAndMessaging::WNDCLASSW;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW;
use windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST;
use windows::Win32::UI::WindowsAndMessaging::WS_POPUP;
use windows::core::PCWSTR;
use windows::core::w;

/// How long the main thread waits for the overlay to be on screen before switching
const OVERLAY_READY_TIMEOUT: Duration = Duration::from_millis(60);
/// Source windows for thumbnails of our own content live far outside of any monitor
const OFFSCREEN: i32 = -20000;
/// The wallpaper snapshot is shrunk by this factor before being blurred; DWM upscales
/// thumbnails with a coarse filter, so larger factors make the blur visibly blocky
const BLUR_DOWNSCALE: i32 = 2;
/// Maximum number of sharp wallpaper tiles between windows
const MAX_WALLPAPER_TILES: usize = 64;
/// The blurred wallpaper snapshot is refreshed when it is older than this while idle
const BLUR_MAX_AGE: Duration = Duration::from_secs(60);

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
    /// Duration in ms of the fade from the animated thumbnails to the real windows once the
    /// animation is over, which hides any difference between them (default: 90)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fade_out: Option<u64>,
    /// Show a blurred wallpaper behind windows, so that translucent windows look like they do
    /// with acrylic or mica backdrops (default: true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop_blur: Option<bool>,
    /// Blur radius in pixels of the backdrop (default: 40)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backdrop_blur_radius: Option<u32>,
    /// Draw drop shadows like the Windows 11 ones underneath windows (default: true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow: Option<bool>,
    /// Focus border drawn around the focused window during the animation (default: none)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border: Option<WorkspaceAnimationBorder>,
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

    if !enabled() {
        return;
    }

    // start the animator early so that the blurred wallpaper is ready for the first switch
    let animator = animator();

    let Some(previous_idx) = previous_idx else {
        return;
    };

    if previous_idx == shown_idx {
        return;
    }

    let outgoing = workspaces
        .get(previous_idx)
        .map(WorkspaceWindows::from)
        .unwrap_or_default();
    let incoming = workspaces
        .get(shown_idx)
        .map(WorkspaceWindows::from)
        .unwrap_or_default();

    if outgoing.hwnds.is_empty() && incoming.hwnds.is_empty() {
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

    if animator.send(command).is_ok() {
        let _ = ack_receiver.recv_timeout(OVERLAY_READY_TIMEOUT);
    }
}

#[derive(Default)]
struct WorkspaceWindows {
    /// The windows of a workspace which are visible when it is on screen
    hwnds: Vec<isize>,
    /// The window which has focus when the workspace is on screen
    focused: Option<isize>,
}

impl From<&Workspace> for WorkspaceWindows {
    fn from(workspace: &Workspace) -> Self {
        let mut hwnds = vec![];
        let mut focused;

        if let Some(window) = workspace
            .monocle_container
            .as_ref()
            .and_then(|c| c.focused_window())
        {
            hwnds.push(window.hwnd);
            focused = Some(window.hwnd);
        } else if let Some(window) = workspace.maximized_window {
            hwnds.push(window.hwnd);
            focused = Some(window.hwnd);
        } else {
            for container in workspace.containers() {
                if let Some(window) = container.focused_window() {
                    hwnds.push(window.hwnd);
                }
            }

            focused = workspace
                .focused_container()
                .and_then(|c| c.focused_window())
                .map(|w| w.hwnd);
        }

        for window in workspace.floating_windows() {
            hwnds.push(window.hwnd);
        }

        if matches!(workspace.layer, WorkspaceLayer::Floating)
            && let Some(window) = workspace.focused_floating_window()
        {
            focused = Some(window.hwnd);
        }

        hwnds.retain(|hwnd| WindowsApi::is_window(*hwnd));
        if focused.is_some_and(|hwnd| !hwnds.contains(&hwnd)) {
            focused = None;
        }

        Self { hwnds, focused }
    }
}

struct Command {
    hmonitor: isize,
    work_area: Rect,
    outgoing: WorkspaceWindows,
    incoming: WorkspaceWindows,
    forward: bool,
    ack: Sender<()>,
}

fn animator() -> &'static Sender<Command> {
    ANIMATOR.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::unbounded();
        std::thread::spawn(move || Animator::new().run(receiver));
        sender
    })
}

/// Runs a closure on the window host thread, see [`on_window_thread`]
const WM_RUN: u32 = WM_APP + 1;

/// The hidden window owned by the window host thread
static WINDOW_HOST: OnceLock<isize> = OnceLock::new();

extern "system" fn window_callback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_RUN {
        // SAFETY: lparam points at a closure on the stack of a thread which is blocked
        // in SendMessageW until this call returns
        let call = unsafe { &mut *(lparam.0 as *mut &mut dyn FnMut()) };
        call();
        return LRESULT(0);
    }

    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// All windows used by the animations are owned by a dedicated thread which does nothing
/// but process messages. Other processes (notably explorer) send messages to every
/// top-level window and block until they are handled, so the owner of these windows
/// must never block on anything itself, least of all on explorer.
fn window_host() -> Option<HWND> {
    let hwnd = *WINDOW_HOST.get_or_init(|| {
        let (sender, receiver) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let host = create_window(w!("komorebi-workspace-animation-host"), false);
            let _ = sender.send(host.unwrap_or(0));

            let mut msg = MSG::default();
            unsafe {
                while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        });

        receiver.recv().unwrap_or(0)
    });

    (hwnd != 0).then_some(HWND(hwnd as _))
}

/// Runs a closure on the window host thread and returns its result
fn on_window_thread<R>(f: impl FnOnce() -> R) -> Option<R> {
    let host = window_host()?;
    let mut f = Some(f);
    let mut result = None;
    let mut run = || {
        if let Some(f) = f.take() {
            result = Some(f());
        }
    };
    let mut call: &mut dyn FnMut() = &mut run;

    unsafe {
        SendMessageW(
            host,
            WM_RUN,
            None,
            Some(LPARAM(&raw mut call as isize)),
        );
    }

    result
}

fn create_window(class_name: PCWSTR, topmost: bool) -> Option<isize> {
    let h_module = WindowsApi::module_handle_w().ok()?;

    let window_class = WNDCLASSW {
        hInstance: h_module.into(),
        lpszClassName: class_name,
        lpfnWndProc: Some(window_callback),
        hbrBackground: WindowsApi::create_solid_brush(0),
        ..Default::default()
    };

    let _ = WindowsApi::register_class_w(&window_class);

    let mut ex_style = WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED;
    if topmost {
        ex_style |= WS_EX_TOPMOST;
    }

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
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

    Some(hwnd.0 as isize)
}

/// An off-screen window holding a per-pixel alpha bitmap, used as a thumbnail source
struct Surface {
    hwnd: isize,
    width: i32,
    height: i32,
}

impl Surface {
    fn new() -> Option<Self> {
        let hwnd =
            on_window_thread(|| create_window(w!("komorebi-workspace-animation-surface"), false))??;
        Some(Self {
            hwnd,
            width: 0,
            height: 0,
        })
    }

    /// Uploads premultiplied BGRA pixels
    fn upload(&mut self, width: i32, height: i32, pixels: &[u32]) {
        if width <= 0 || height <= 0 || pixels.len() != (width * height) as usize {
            return;
        }

        let hwnd = self.hwnd;
        on_window_thread(|| unsafe {
            let screen = GetDC(None);
            let memory = CreateCompatibleDC(Some(screen));
            let mut bits = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(
                Some(memory),
                &bitmap_info(width, height),
                DIB_RGB_COLORS,
                &mut bits,
                None,
                0,
            ) else {
                let _ = DeleteDC(memory);
                ReleaseDC(None, screen);
                return;
            };

            std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits.cast::<u32>(), pixels.len());
            let previous = SelectObject(memory, bitmap.into());

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            let _ = UpdateLayeredWindow(
                HWND(hwnd as _),
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
            let _ = DeleteDC(memory);
            ReleaseDC(None, screen);

            let _ = ShowWindow(HWND(hwnd as _), SW_SHOWNOACTIVATE);
        });

        self.width = width;
        self.height = height;
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        let hwnd = self.hwnd;
        on_window_thread(|| unsafe {
            let _ = DestroyWindow(HWND(hwnd as _));
        });
    }
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


fn raw_window_rect(hwnd: isize) -> Option<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as _), &mut rect) }.ok()?;
    Some(rect)
}

/// The visible bounds of a window, without the invisible resize borders
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

fn desktop_window() -> Option<isize> {
    unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }
        .ok()
        .map(|hwnd| hwnd.0 as isize)
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

fn unregister(id: isize) {
    let _ = unsafe { DwmUnregisterThumbnail(id) };
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

fn hide(id: isize) {
    let properties = DWM_THUMBNAIL_PROPERTIES {
        dwFlags: DWM_TNP_VISIBLE,
        fVisible: false.into(),
        ..Default::default()
    };

    let _ = unsafe { DwmUpdateThumbnailProperties(id, &properties) };
}

fn offset_rect(rect: RECT, dx: i32, dy: i32) -> RECT {
    RECT {
        left: rect.left + dx,
        top: rect.top + dy,
        right: rect.right + dx,
        bottom: rect.bottom + dy,
    }
}

/// Splits the area of `bounds` which is not covered by any of `holes` into rectangles
fn uncovered_area(bounds: RECT, holes: &[RECT]) -> Vec<RECT> {
    let holes: Vec<RECT> = holes
        .iter()
        .map(|h| RECT {
            left: h.left.max(bounds.left),
            top: h.top.max(bounds.top),
            right: h.right.min(bounds.right),
            bottom: h.bottom.min(bounds.bottom),
        })
        .filter(|h| h.left < h.right && h.top < h.bottom)
        .collect();

    let mut edges = vec![bounds.top, bounds.bottom];
    for hole in &holes {
        edges.push(hole.top);
        edges.push(hole.bottom);
    }
    edges.sort_unstable();
    edges.dedup();

    // rects which are still growing downwards, keyed by their horizontal extent
    let mut open: Vec<RECT> = vec![];
    let mut done = vec![];

    for band in edges.windows(2) {
        let (top, bottom) = (band[0], band[1]);

        let mut covered: Vec<(i32, i32)> = holes
            .iter()
            .filter(|h| h.top < bottom && h.bottom > top)
            .map(|h| (h.left, h.right))
            .collect();
        covered.sort_unstable();

        let mut gaps = vec![];
        let mut x = bounds.left;
        for (left, right) in covered {
            if left > x {
                gaps.push((x, left));
            }
            x = x.max(right);
        }
        if x < bounds.right {
            gaps.push((x, bounds.right));
        }

        let mut next_open = vec![];
        for (left, right) in gaps {
            if let Some(idx) = open
                .iter()
                .position(|r| r.left == left && r.right == right && r.bottom == top)
            {
                let mut rect = open.remove(idx);
                rect.bottom = bottom;
                next_open.push(rect);
            } else {
                next_open.push(RECT {
                    left,
                    top,
                    right,
                    bottom,
                });
            }
        }

        done.append(&mut open);
        open = next_open;
    }

    done.append(&mut open);
    done
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

/// Rasterises an anti-aliased rounded rectangle outline into premultiplied BGRA pixels
fn border_expansion(border: &WorkspaceAnimationBorder) -> i32 {
    (border.width.unwrap_or(3.0) + border.offset.unwrap_or(-1.0)).round() as i32
}

/// Rasterises a border whose outer edge is the edge of the bitmap
fn render_border(width: i32, height: i32, border: &WorkspaceAnimationBorder) -> Vec<u32> {
    let stroke = border.width.unwrap_or(3.0).max(0.5);
    let inset = 0.0;
    let radius = (border.radius.unwrap_or(8.0) + f64::from(border_expansion(border))).max(0.0);
    let (r, g, b, a) = parse_colour(border.colour.as_deref().unwrap_or("#FFFFFFFF"));

    let (w, h) = (f64::from(width), f64::from(height));
    let half_w = w / 2.0 - inset;
    let half_h = h / 2.0 - inset;
    let mut pixels = vec![0u32; (width * height) as usize];

    // pixels further than this from every edge can't be touched by the stroke
    let reach = (inset + stroke + radius + 1.0).ceil() as i32;

    for y in 0..height {
        let near_y = y < reach || y >= height - reach;
        for x in 0..width {
            if !near_y && x >= reach && x < width - reach {
                continue;
            }

            let px = (f64::from(x) + 0.5 - w / 2.0).abs() - (half_w - radius);
            let py = (f64::from(y) + 0.5 - h / 2.0).abs() - (half_h - radius);
            let outside = px.max(0.0).hypot(py.max(0.0));
            let distance = outside + px.max(py).min(0.0) - radius;

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

/// A blurred, shrunk snapshot of the desktop window
struct BlurredWallpaper {
    surface: Surface,
    /// Screen coordinates of the desktop window when the snapshot was taken
    desktop: RECT,
    taken: Instant,
}

impl BlurredWallpaper {
    fn capture(surface: Surface, radius: u32) -> Option<Self> {
        let mut surface = surface;
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
                let (full, small) = (*full, *small);
                let previous_full = SelectObject(full_dc, full.into());
                let previous_small = SelectObject(small_dc, small.into());

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

        let radius = (radius as i32 / BLUR_DOWNSCALE).max(1) as usize;
        // three box blur passes of radius r approximate a gaussian of radius ~r * 1.7
        box_blur(
            &mut pixels,
            small_w as usize,
            small_h as usize,
            (radius as f64 / 1.7).round().max(1.0) as usize,
        );

        surface.upload(small_w, small_h, &pixels);

        Some(Self {
            surface,
            desktop,
            taken: Instant::now(),
        })
    }

    /// The part of the snapshot behind a work area, in snapshot pixels
    fn source_for(&self, work_area: Rect) -> RECT {
        let scale = f64::from(BLUR_DOWNSCALE);
        let to_snapshot = |value: i32, origin: i32| (f64::from(value - origin) / scale).round() as i32;

        RECT {
            left: to_snapshot(work_area.left, self.desktop.left),
            top: to_snapshot(work_area.top, self.desktop.top),
            right: to_snapshot(work_area.left + work_area.right, self.desktop.left),
            bottom: to_snapshot(work_area.top + work_area.bottom, self.desktop.top),
        }
        .clamp_to(self.surface.width, self.surface.height)
    }
}

trait ClampTo {
    fn clamp_to(self, width: i32, height: i32) -> Self;
}

impl ClampTo for RECT {
    fn clamp_to(self, width: i32, height: i32) -> Self {
        RECT {
            left: self.left.clamp(0, width),
            top: self.top.clamp(0, height),
            right: self.right.clamp(0, width),
            bottom: self.bottom.clamp(0, height),
        }
    }
}

struct WindowThumbnail {
    id: isize,
    hwnd: isize,
    incoming: bool,
    focused: bool,
    /// Visible frame of an outgoing window when the animation started
    start_frame: RECT,
    /// Thumbnail of the drop shadow drawn underneath the window
    shadow: Option<isize>,
}

/// Maximum number of pre-rendered drop shadows kept between animations
const MAX_CACHED_SHADOWS: usize = 12;

/// Distance the drop shadow bitmap extends beyond the window frame on every side
const SHADOW_MARGIN: i32 = 96;

/// Parameters of a blurred rectangle making up part of a drop shadow
struct ShadowLayer {
    sigma: f64,
    offset_y: f64,
    opacity: f64,
}

/// Approximates the Windows 11 drop shadow as measured around a focused window: a tight
/// ambient shadow on every side and a wide key shadow which is shifted downwards
const SHADOW_LAYERS: [ShadowLayer; 2] = [
    ShadowLayer {
        sigma: 6.0,
        offset_y: 0.0,
        opacity: 0.18,
    },
    ShadowLayer {
        sigma: 24.0,
        offset_y: 20.0,
        opacity: 0.55,
    },
];

/// Unfocused windows have a noticeably lighter shadow
const UNFOCUSED_SHADOW_OPACITY: f64 = 0.6;

/// Normal cumulative distribution function (Abramowitz & Stegun 7.1.26 for erf)
fn normal_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.327_591_1 * z);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736 + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let erf = 1.0 - poly * (-z * z).exp();
    if x >= 0.0 {
        0.5 * (1.0 + erf)
    } else {
        0.5 * (1.0 - erf)
    }
}

/// Rasterises the drop shadow of a window frame of the given size into premultiplied
/// BGRA pixels, with the frame placed `SHADOW_MARGIN` pixels in from every edge. A
/// gaussian-blurred rectangle is separable, so each layer is the product of two 1D
/// profiles. Like DWM, nothing is drawn underneath the window itself.
fn render_shadow(frame_width: i32, frame_height: i32, focused: bool, radius: f64) -> (i32, i32, Vec<u32>) {
    let width = frame_width + SHADOW_MARGIN * 2;
    let height = frame_height + SHADOW_MARGIN * 2;
    let margin = f64::from(SHADOW_MARGIN);
    let strength = if focused { 1.0 } else { UNFOCUSED_SHADOW_OPACITY };

    let profile = |length: i32, start: f64, end: f64, sigma: f64| -> Vec<f64> {
        (0..length)
            .map(|i| {
                let p = f64::from(i) + 0.5;
                normal_cdf((p - start) / sigma) - normal_cdf((p - end) / sigma)
            })
            .collect()
    };

    let layers: Vec<(Vec<f64>, Vec<f64>, f64)> = SHADOW_LAYERS
        .iter()
        .map(|layer| {
            (
                profile(width, margin, margin + f64::from(frame_width), layer.sigma),
                profile(
                    height,
                    margin + layer.offset_y,
                    margin + layer.offset_y + f64::from(frame_height),
                    layer.sigma,
                ),
                layer.opacity * strength,
            )
        })
        .collect();

    let (half_w, half_h) = (f64::from(frame_width) / 2.0, f64::from(frame_height) / 2.0);
    let (centre_x, centre_y) = (margin + half_w, margin + half_h);
    let mut pixels = vec![0u32; (width * height) as usize];

    for y in 0..height {
        for x in 0..width {
            // signed distance to the rounded window frame; the shadow fades out at its edge
            let px = (f64::from(x) + 0.5 - centre_x).abs() - (half_w - radius);
            let py = (f64::from(y) + 0.5 - centre_y).abs() - (half_h - radius);
            let distance =
                px.max(0.0).hypot(py.max(0.0)) + px.max(py).min(0.0) - radius;
            let outside = (distance + 0.5).clamp(0.0, 1.0);
            if outside == 0.0 {
                continue;
            }

            let alpha: f64 = layers
                .iter()
                .map(|(xs, ys, opacity)| opacity * xs[x as usize] * ys[y as usize])
                .sum::<f64>()
                .min(1.0)
                * outside;

            pixels[(y * width + x) as usize] = ((alpha * 255.0).round() as u32) << 24;
        }
    }

    (width, height, pixels)
}

struct BorderThumbnail {
    id: isize,
    surface: Surface,
    incoming: bool,
}

struct Animation {
    overlay: isize,
    work_area: Rect,
    blur: Option<isize>,
    blur_source: RECT,
    wallpaper_tiles: Vec<isize>,
    windows: Vec<WindowThumbnail>,
    borders: Vec<BorderThumbnail>,
    forward: bool,
    kind: WorkspaceAnimationKind,
    config: WorkspaceAnimationConfig,
    started: Instant,
    duration: Duration,
    fade_out: Duration,
}

enum Phase {
    Moving(f64),
    FadingOut(u8),
    Done,
}

impl Animation {
    fn start(
        command: &Command,
        overlay: isize,
        blurred: Option<&BlurredWallpaper>,
        border_surfaces: &mut Vec<Surface>,
        shadow_surfaces: &mut HashMap<(i32, i32, bool), Surface>,
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
        // the overlay is shown fully transparent; it is only made opaque once the
        // thumbnails on it have been composited, otherwise a black frame can flash
        on_window_thread(|| {
            if !WindowsApi::is_window_visible(overlay) {
                let _ = WindowsApi::set_transparent(overlay, 0);
            }

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
        });

        let kind = config.kind.unwrap_or_default();
        let fades = matches!(
            kind,
            WorkspaceAnimationKind::Fade
                | WorkspaceAnimationKind::SlideFade
                | WorkspaceAnimationKind::SlideFadeVertical
        );

        // registration order is drawing order: blur, wallpaper, shadows, windows, borders
        let (blur, blur_source) = match blurred {
            Some(blurred) if config.backdrop_blur.unwrap_or(true) && !fades => (
                register(overlay, blurred.surface.hwnd),
                blurred.source_for(wa),
            ),
            _ => (None, RECT::default()),
        };

        let wallpaper_tiles = desktop_window()
            .map(|desktop| {
                let count = if blur.is_some() {
                    MAX_WALLPAPER_TILES
                } else {
                    1
                };
                (0..count)
                    .filter_map(|_| register(overlay, desktop))
                    .collect()
            })
            .unwrap_or_default();

        let mut sources = vec![];
        for (side, incoming) in [(&command.outgoing, false), (&command.incoming, true)] {
            let mut hwnds = side.hwnds.clone();
            sort_bottom_to_top(&mut hwnds);

            for hwnd in hwnds {
                let frame = frame_rect(hwnd)
                    .or_else(|| raw_window_rect(hwnd))
                    .unwrap_or_default();
                sources.push((hwnd, incoming, side.focused == Some(hwnd), frame));
            }
        }

        // shadows go underneath all windows, so they are registered first; the cache only
        // keeps the shadows of the windows in this animation once it grows too large
        if shadow_surfaces.len() > MAX_CACHED_SHADOWS {
            let wanted: Vec<(i32, i32, bool)> = sources
                .iter()
                .map(|(_, _, focused, f)| (f.right - f.left, f.bottom - f.top, *focused))
                .collect();
            shadow_surfaces.retain(|key, _| wanted.contains(key));
        }

        let radius = config
            .border
            .as_ref()
            .and_then(|b| b.radius)
            .unwrap_or(8.0);
        let shadows: Vec<Option<isize>> = sources
            .iter()
            .map(|(_, _, focused, frame)| {
                if !config.shadow.unwrap_or(true) {
                    return None;
                }

                let size = (frame.right - frame.left, frame.bottom - frame.top, *focused);
                if size.0 <= 0 || size.1 <= 0 {
                    return None;
                }

                if !shadow_surfaces.contains_key(&size) {
                    let mut surface = Surface::new()?;
                    let (width, height, pixels) = render_shadow(size.0, size.1, size.2, radius);
                    surface.upload(width, height, &pixels);
                    shadow_surfaces.insert(size, surface);
                }

                register(overlay, shadow_surfaces.get(&size)?.hwnd)
            })
            .collect();

        let mut windows = vec![];
        for ((hwnd, incoming, focused, frame), shadow) in sources.into_iter().zip(shadows) {
            if let Some(id) = register(overlay, hwnd) {
                windows.push(WindowThumbnail {
                    id,
                    hwnd,
                    incoming,
                    focused,
                    start_frame: frame,
                    shadow,
                });
            } else if let Some(shadow) = shadow {
                unregister(shadow);
            }
        }

        let mut borders = vec![];
        if config.border.is_some() {
            for incoming in [false, true] {
                if windows.iter().any(|w| w.focused && w.incoming == incoming) {
                    let surface = border_surfaces
                        .pop()
                        .or_else(Surface::new);

                    if let Some(surface) = surface
                        && let Some(id) = register(overlay, surface.hwnd)
                    {
                        borders.push(BorderThumbnail {
                            id,
                            surface,
                            incoming,
                        });
                    }
                }
            }
        }

        let mut animation = Self {
            overlay,
            work_area: wa,
            blur,
            blur_source,
            wallpaper_tiles,
            windows,
            borders,
            forward: command.forward,
            kind,
            config: config.clone(),
            started: Instant::now(),
            duration: Duration::from_millis(duration.max(1)),
            fade_out: Duration::from_millis(config.fade_out.unwrap_or(90)),
        };

        animation.render(0.0);
        animation
    }

    fn phase(&self) -> Phase {
        let elapsed = self.started.elapsed();
        if elapsed < self.duration {
            return Phase::Moving(elapsed.as_secs_f64() / self.duration.as_secs_f64());
        }

        let fading = elapsed - self.duration;
        if fading >= self.fade_out {
            return Phase::Done;
        }

        let alpha =
            (255.0 * (1.0 - fading.as_secs_f64() / self.fade_out.as_secs_f64())).round() as u8;

        Phase::FadingOut(alpha)
    }

    fn eased(&self, progress: f64) -> f64 {
        if let Some(curve) = self.config.bezier {
            return cubic_bezier(curve, progress);
        }

        let style = ANIMATION_STYLE_PER_ANIMATION
            .lock()
            .get(&AnimationPrefix::Workspace)
            .copied()
            .unwrap_or_else(|| *ANIMATION_STYLE_GLOBAL.lock());

        apply_ease_func(progress, style)
    }

    fn render(&mut self, progress: f64) {
        let wa = self.work_area;
        let eased = self.eased(progress);
        let bounds = RECT {
            left: 0,
            top: 0,
            right: wa.right,
            bottom: wa.bottom,
        };

        let (vertical, distance, fade) = match self.kind {
            WorkspaceAnimationKind::Slide => (false, 1.0, false),
            WorkspaceAnimationKind::SlideVertical => (true, 1.0, false),
            WorkspaceAnimationKind::Fade => (false, 0.0, true),
            WorkspaceAnimationKind::SlideFade => (
                false,
                self.config.slide_fade_distance.unwrap_or(0.2),
                true,
            ),
            WorkspaceAnimationKind::SlideFadeVertical => (
                true,
                self.config.slide_fade_distance.unwrap_or(0.2),
                true,
            ),
        };

        let extent = f64::from(if vertical { wa.bottom } else { wa.right }) * distance;
        let direction = if self.forward { -1.0 } else { 1.0 };

        if let Some(blur) = self.blur {
            update(blur, bounds, Some(self.blur_source), 255);
        }

        let mut holes = vec![];
        let mut frames = vec![];

        for window in &self.windows {
            // incoming windows track their live position, so that the thumbnails always
            // land exactly where the real windows have been laid out
            let frame = if window.incoming {
                frame_rect(window.hwnd).unwrap_or(window.start_frame)
            } else {
                window.start_frame
            };

            // outgoing windows travel one extent in the direction of travel, while incoming
            // windows start one extent away on the opposite side and arrive in place
            let (offset, opacity) = if window.incoming {
                (-direction * extent * (1.0 - eased), eased.clamp(0.0, 1.0))
            } else {
                (direction * extent * eased, (1.0 - eased).clamp(0.0, 1.0))
            };

            let offset = offset.round() as i32;
            let (dx, dy) = if vertical { (0, offset) } else { (offset, 0) };
            let (dx, dy) = (dx - wa.left, dy - wa.top);

            let opacity = if fade {
                (opacity * 255.0).round() as u8
            } else {
                255
            };

            // window thumbnails only contain the visible frame, without the invisible
            // resize borders, so they have to be placed on the frame to render 1:1
            let frame = offset_rect(frame, dx, dy);
            update(window.id, frame, None, opacity);

            if let Some(shadow) = window.shadow {
                let margin = SHADOW_MARGIN;
                update(
                    shadow,
                    RECT {
                        left: frame.left - margin,
                        top: frame.top - margin,
                        right: frame.right + margin,
                        bottom: frame.bottom + margin,
                    },
                    None,
                    opacity,
                );
            }
            holes.push(frame);
            if window.focused {
                frames.push((window.incoming, frame, opacity));
            }
        }

        // sharp wallpaper everywhere except behind windows, where the blur shows through
        let tiles = if self.blur.is_some() {
            uncovered_area(bounds, &holes)
        } else {
            vec![bounds]
        };

        if tiles.len() > self.wallpaper_tiles.len() {
            // too fragmented to tile; fall back to a plain sharp wallpaper
            for (idx, id) in self.wallpaper_tiles.iter().enumerate() {
                if idx == 0 {
                    update(*id, bounds, Some(self.desktop_source(bounds)), 255);
                } else {
                    hide(*id);
                }
            }
        } else {
            for (idx, id) in self.wallpaper_tiles.iter().enumerate() {
                match tiles.get(idx) {
                    Some(tile) => update(*id, *tile, Some(self.desktop_source(*tile)), 255),
                    None => hide(*id),
                }
            }
        }

        if let Some(border) = &self.config.border {
            for thumbnail in &mut self.borders {
                let Some((_, frame, opacity)) = frames
                    .iter()
                    .find(|(incoming, _, _)| *incoming == thumbnail.incoming)
                else {
                    hide(thumbnail.id);
                    continue;
                };

                // like tacky-borders, the outer edge of the border sits (width + offset)
                // pixels outside of the visible frame of the window
                let expand = border_expansion(border);
                let rect = RECT {
                    left: frame.left - expand,
                    top: frame.top - expand,
                    right: frame.right + expand,
                    bottom: frame.bottom + expand,
                };

                let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                if width <= 0 || height <= 0 {
                    hide(thumbnail.id);
                    continue;
                }

                if thumbnail.surface.width != width || thumbnail.surface.height != height {
                    let pixels = render_border(width, height, border);
                    thumbnail.surface.upload(width, height, &pixels);
                }

                update(thumbnail.id, rect, None, *opacity);
            }
        }
    }

    /// The part of the desktop window behind a rect of the overlay
    fn desktop_source(&self, rect: RECT) -> RECT {
        let desktop = desktop_window()
            .and_then(raw_window_rect)
            .unwrap_or_default();
        offset_rect(
            rect,
            self.work_area.left - desktop.left,
            self.work_area.top - desktop.top,
        )
    }

    /// Releases all thumbnails, returning the border surfaces for reuse
    fn finish(self, hide_overlay: bool, border_surfaces: &mut Vec<Surface>) {
        for window in &self.windows {
            unregister(window.id);
            if let Some(shadow) = window.shadow {
                unregister(shadow);
            }
        }

        for id in &self.wallpaper_tiles {
            unregister(*id);
        }

        if let Some(blur) = self.blur {
            unregister(blur);
        }

        for border in self.borders {
            unregister(border.id);
            border_surfaces.push(border.surface);
        }

        if hide_overlay {
            let overlay = self.overlay;
            on_window_thread(|| unsafe {
                let _ = ShowWindow(HWND(overlay as _), SW_HIDE);
            });
        }
    }
}

struct Animator {
    overlays: HashMap<isize, isize>,
    animations: HashMap<isize, Animation>,
    border_surfaces: Vec<Surface>,
    /// Pre-rendered drop shadows, keyed by window frame size and focus
    shadow_surfaces: HashMap<(i32, i32, bool), Surface>,
    blurred: Option<BlurredWallpaper>,
    /// Set once an animation finishes, so the blur snapshot is refreshed while idle
    blur_stale: bool,
    /// A blur snapshot being taken on a separate thread
    blur_job: Option<Receiver<Option<BlurredWallpaper>>>,
}

impl Animator {
    fn new() -> Self {
        Self {
            overlays: HashMap::new(),
            animations: HashMap::new(),
            border_surfaces: vec![],
            shadow_surfaces: HashMap::new(),
            blurred: None,
            blur_stale: true,
            blur_job: None,
        }
    }

    /// Takes a new blur snapshot on a separate thread: PrintWindow waits on explorer,
    /// which must never hold up an animation
    fn refresh_blur(&mut self) {
        if self.blur_job.is_some() {
            return;
        }

        let radius = CONFIG.lock().backdrop_blur_radius.unwrap_or(40);
        let (sender, receiver) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            let blurred = Surface::new().and_then(|surface| BlurredWallpaper::capture(surface, radius));
            let _ = sender.send(blurred);
        });

        self.blur_job = Some(receiver);
        self.blur_stale = false;
    }

    /// Swaps in a finished blur snapshot; only called while no animation uses the old one
    fn collect_blur(&mut self) {
        let Some(job) = &self.blur_job else {
            return;
        };

        match job.try_recv() {
            Ok(blurred) => {
                if blurred.is_some() {
                    self.blurred = blurred;
                }
                self.blur_job = None;
            }
            Err(crossbeam_channel::TryRecvError::Disconnected) => self.blur_job = None,
            Err(crossbeam_channel::TryRecvError::Empty) => {}
        }
    }

    fn run(mut self, receiver: Receiver<Command>) {
        loop {
            let command = if self.animations.is_empty() {
                ANIMATION_RUNNING.store(false);

                let blur_wanted = CONFIG.lock().backdrop_blur.unwrap_or(true);
                let blur_old = self
                    .blurred
                    .as_ref()
                    .is_none_or(|blurred| blurred.taken.elapsed() > BLUR_MAX_AGE);

                self.collect_blur();
                if blur_wanted && (self.blur_stale || blur_old) {
                    self.refresh_blur();
                }

                // wake up often enough to pick up a blur snapshot soon after it is ready
                let timeout = if self.blur_job.is_some() { 20 } else { 100 };
                match receiver.recv_timeout(Duration::from_millis(timeout)) {
                    Ok(command) => Some(command),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                receiver.try_recv().ok()
            };

            if let Some(command) = command {
                self.start(command);
                continue;
            }

            let mut finished = vec![];
            for (hmonitor, animation) in &mut self.animations {
                match animation.phase() {
                    Phase::Moving(progress) => animation.render(progress),
                    Phase::FadingOut(alpha) => {
                        animation.render(1.0);
                        let overlay = animation.overlay;
                        on_window_thread(|| WindowsApi::set_transparent(overlay, alpha));
                    }
                    Phase::Done => {
                        animation.render(1.0);
                        finished.push(*hmonitor);
                    }
                }
            }

            if !self.animations.is_empty() && unsafe { DwmFlush() }.is_err() {
                std::thread::sleep(Duration::from_millis(4));
            }

            for hmonitor in finished {
                if let Some(animation) = self.animations.remove(&hmonitor) {
                    animation.finish(true, &mut self.border_surfaces);
                    self.blur_stale = true;
                }
            }

        }
    }

    fn start(&mut self, command: Command) {
        ANIMATION_RUNNING.store(true);

        // a new switch on a monitor which is still animating replaces that animation;
        // the overlay stays up so there is no flash of the real windows in between
        let replacing = self.animations.remove(&command.hmonitor).map(|previous| {
            previous.finish(false, &mut self.border_surfaces);
        });

        let overlay = match self.overlays.get(&command.hmonitor) {
            Some(overlay) => Some(*overlay),
            None => on_window_thread(|| create_window(w!("komorebi-workspace-animation"), true))
                .flatten()
                .inspect(|overlay| {
                    self.overlays.insert(command.hmonitor, *overlay);
                }),
        };

        if let Some(overlay) = overlay {
            let animation = Animation::start(
                &command,
                overlay,
                self.blurred.as_ref(),
                &mut self.border_surfaces,
                &mut self.shadow_surfaces,
            );
            // wait for the thumbnails to be composited, then reveal the overlay and make
            // sure it is on screen before the real switch happens underneath it
            if replacing.is_none() {
                let _ = unsafe { DwmFlush() };
            }
            on_window_thread(|| WindowsApi::set_transparent(overlay, 255));
            let _ = unsafe { DwmFlush() };
            self.animations.insert(command.hmonitor, animation);
        }

        let _ = command.ack.send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> RECT {
        RECT {
            left,
            top,
            right,
            bottom,
        }
    }

    fn area(rects: &[RECT]) -> i64 {
        rects
            .iter()
            .map(|r| i64::from(r.right - r.left) * i64::from(r.bottom - r.top))
            .sum()
    }

    #[test]
    fn uncovered_area_excludes_holes() {
        let bounds = rect(0, 0, 100, 100);
        let holes = [rect(10, 10, 40, 90), rect(60, 10, 90, 90)];
        let tiles = uncovered_area(bounds, &holes);

        assert_eq!(area(&tiles), 100 * 100 - 2 * 30 * 80);
        for tile in &tiles {
            for hole in &holes {
                let overlaps = tile.left < hole.right
                    && tile.right > hole.left
                    && tile.top < hole.bottom
                    && tile.bottom > hole.top;
                assert!(!overlaps);
            }
        }
    }

    #[test]
    fn uncovered_area_handles_holes_outside_bounds() {
        let tiles = uncovered_area(rect(0, 0, 100, 100), &[rect(-50, 0, 50, 100)]);
        assert_eq!(area(&tiles), 50 * 100);
    }

    #[test]
    fn bezier_endpoints() {
        let curve = [0.05, 0.9, 0.1, 1.05];
        assert!(cubic_bezier(curve, 0.0).abs() < 1e-3);
        assert!((cubic_bezier(curve, 1.0) - 1.0).abs() < 1e-3);
    }
}
