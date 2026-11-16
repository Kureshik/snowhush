//! Native X11 desktop overlay. No Cinnamon/Muffin plugin needed: X11 exposes
//! stacking, geometry, desktops and pointer, while XShape makes the overlay click-through.
//! Shares the Wayland renderer, tray, preferences and the platform-neutral simulation.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString, c_char, c_int, c_uchar, c_uint, c_ulong, c_void};
use std::time::{Duration, Instant};
use std::ptr;

use calloop::EventLoop;
use calloop::channel::Event;
use libloading::Library;
use snowhush_core::geometry::V2;
use snowhush_core::simulation::{Borrowed, Mapping, Pace, SnowSimulation, Stray, WindowId, WindowSurface};

use crate::light;
use crate::prefs::Settings;
use crate::render::{Ambient, Gpu, Surface};
use crate::snow::Command;
use crate::source::{Display, Panel, Report, Window};
use crate::tray::Tray;
use crate::windows::{self, Area, Shelf};
use crate::log;

type Xid = c_ulong;

const INPUT_OUTPUT: c_uint = 1;
const CW_BACK_PIXEL: c_ulong = 1 << 1;
const CW_BORDER_PIXEL: c_ulong = 1 << 3;
const CW_OVERRIDE_REDIRECT: c_ulong = 1 << 9;
const CW_EVENT_MASK: c_ulong = 1 << 11;
const CW_COLORMAP: c_ulong = 1 << 13;
const SUBSTRUCTURE_NOTIFY_MASK: CLongAlias = 1 << 19;
const PROPERTY_CHANGE_MASK: CLongAlias = 1 << 22;
const STRUCTURE_NOTIFY_MASK: CLongAlias = 1 << 17;
const IS_VIEWABLE: c_int = 2;
const SHAPE_INPUT: c_int = 2;
const PROP_MODE_REPLACE: c_int = 0;
const VISUAL_ID_MASK: CLongAlias = 1;
const CW_SIBLING: c_uint = 1 << 5;
const CW_STACK_MODE: c_uint = 1 << 6;
const BELOW: c_int = 1;
type CLongAlias = libc::c_long;

#[repr(C)]
#[derive(Default)]
struct SetWindowAttributes {
    background_pixmap: Xid,
    background_pixel: c_ulong,
    border_pixmap: Xid,
    border_pixel: c_ulong,
    bit_gravity: c_int,
    win_gravity: c_int,
    backing_store: c_int,
    backing_planes: c_ulong,
    backing_pixel: c_ulong,
    save_under: c_int,
    event_mask: CLongAlias,
    do_not_propagate_mask: CLongAlias,
    override_redirect: c_int,
    colormap: Xid,
    cursor: Xid,
}

#[repr(C)]
#[derive(Default)]
struct WindowChanges {
    x: c_int, y: c_int, width: c_int, height: c_int, border_width: c_int,
    sibling: Xid, stack_mode: c_int,
}

#[repr(C)]
struct VisualInfo {
    visual: *mut c_void,
    visualid: Xid,
    screen: c_int,
    depth: c_int,
    class: c_int,
    red_mask: c_ulong,
    green_mask: c_ulong,
    blue_mask: c_ulong,
    colormap_size: c_int,
    bits_per_rgb: c_int,
}

#[repr(C)]
struct WindowAttributes {
    x: c_int, y: c_int, width: c_int, height: c_int, border_width: c_int, depth: c_int,
    visual: *mut c_void, root: Xid, class: c_int, bit_gravity: c_int, win_gravity: c_int,
    backing_store: c_int, backing_planes: c_ulong, backing_pixel: c_ulong,
    save_under: c_int, colormap: Xid, map_installed: c_int, map_state: c_int,
    all_event_masks: CLongAlias, your_event_mask: CLongAlias,
    do_not_propagate_mask: CLongAlias, override_redirect: c_int, screen: *mut c_void,
}

#[repr(C)]
struct MonitorInfo {
    name: Xid, primary: c_int, automatic: c_int, noutput: c_int,
    x: c_int, y: c_int, width: c_int, height: c_int,
    mwidth: c_int, mheight: c_int, outputs: *mut Xid,
}

type OpenDisplay = unsafe extern "C" fn(*const c_char) -> *mut c_void;
type CloseDisplay = unsafe extern "C" fn(*mut c_void) -> c_int;
type DefaultRoot = unsafe extern "C" fn(*mut c_void) -> Xid;
type DefaultScreen = unsafe extern "C" fn(*mut c_void) -> c_int;
type AtomFn = unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> Xid;
type GetProperty = unsafe extern "C" fn(*mut c_void, Xid, Xid, CLongAlias, CLongAlias, c_int, Xid, *mut Xid, *mut c_int, *mut c_ulong, *mut c_ulong, *mut *mut c_uchar) -> c_int;
type GetGeometry = unsafe extern "C" fn(*mut c_void, Xid, *mut Xid, *mut c_int, *mut c_int, *mut c_uint, *mut c_uint, *mut c_uint, *mut c_uint) -> c_int;
type Translate = unsafe extern "C" fn(*mut c_void, Xid, Xid, c_int, c_int, *mut c_int, *mut c_int, *mut Xid) -> c_int;
type QueryTree = unsafe extern "C" fn(*mut c_void, Xid, *mut Xid, *mut Xid, *mut *mut Xid, *mut c_uint) -> c_int;
type QueryPointer = unsafe extern "C" fn(*mut c_void, Xid, *mut Xid, *mut Xid, *mut c_int, *mut c_int, *mut c_int, *mut c_int, *mut c_uint) -> c_int;
type GetAttributes = unsafe extern "C" fn(*mut c_void, Xid, *mut WindowAttributes) -> c_int;
type GetVisualInfo = unsafe extern "C" fn(*mut c_void, CLongAlias, *mut VisualInfo, *mut c_int) -> *mut VisualInfo;
type CreateColormap = unsafe extern "C" fn(*mut c_void, Xid, *mut c_void, c_int) -> Xid;
type CreateWindow = unsafe extern "C" fn(*mut c_void, Xid, c_int, c_int, c_uint, c_uint, c_uint, c_int, c_uint, *mut c_void, c_ulong, *mut SetWindowAttributes) -> Xid;
type ChangeProperty = unsafe extern "C" fn(*mut c_void, Xid, Xid, Xid, c_int, c_int, *const c_uchar, c_int) -> c_int;
type SelectInput = unsafe extern "C" fn(*mut c_void, Xid, CLongAlias) -> c_int;
type WindowAction = unsafe extern "C" fn(*mut c_void, Xid) -> c_int;
type FreeColor = unsafe extern "C" fn(*mut c_void, Xid) -> c_int;
type FreeMemory = unsafe extern "C" fn(*mut c_void) -> c_int;
type Flush = unsafe extern "C" fn(*mut c_void) -> c_int;
type Pending = unsafe extern "C" fn(*mut c_void) -> c_int;
type NextEvent = unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int;
type ConfigureWindow = unsafe extern "C" fn(*mut c_void, Xid, c_uint, *mut WindowChanges) -> c_int;
type GetSelectionOwner = unsafe extern "C" fn(*mut c_void, Xid) -> Xid;
type SetSelectionOwner = unsafe extern "C" fn(*mut c_void, Xid, Xid, Xid) -> c_int;
type GetAtomName = unsafe extern "C" fn(*mut c_void, Xid) -> *mut c_char;
type ShapeRectangles = unsafe extern "C" fn(*mut c_void, Xid, c_int, c_int, c_int, *const c_void, c_int, c_int, c_int);
type ShapeQuery = unsafe extern "C" fn(*mut c_void, *mut c_int, *mut c_int) -> c_int;
type GetMonitors = unsafe extern "C" fn(*mut c_void, Xid, c_int, *mut c_int) -> *mut MonitorInfo;
type FreeMonitors = unsafe extern "C" fn(*mut MonitorInfo);

/// Keep the dynamic libraries alive for the entire lifetime of their function pointers.
struct Xlib {
    _lib: Library,
    _ext: Library,
    _randr: Option<Library>,
    raw: *mut c_void,
    root: Xid,
    screen: c_int,
    close: CloseDisplay,
    intern: AtomFn,
    get_property: GetProperty,
    geometry: GetGeometry,
    translate: Translate,
    query_tree: QueryTree,
    pointer: QueryPointer,
    attributes: GetAttributes,
    visual_info: GetVisualInfo,
    create_colormap: CreateColormap,
    create_window: CreateWindow,
    change_property: ChangeProperty,
    select: SelectInput,
    map: WindowAction,
    unmap: WindowAction,
    destroy: WindowAction,
    raise: WindowAction,
    configure: ConfigureWindow,
    free_color: FreeColor,
    free: FreeMemory,
    flush: Flush,
    pending: Pending,
    next_event: NextEvent,
    atom_name: GetAtomName,
    selection_owner: GetSelectionOwner,
    set_selection_owner: SetSelectionOwner,
    shape: ShapeRectangles,
    monitors: Option<GetMonitors>,
    free_monitors: Option<FreeMonitors>,
}

unsafe fn symbol<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, String> {
    unsafe { lib.get::<T>(name) }.map(|symbol| *symbol).map_err(|error| format!("X11 {}: {error}", String::from_utf8_lossy(name)))
}

impl Xlib {
    fn connect() -> Result<Self, String> {
        // Xlib libraries are loaded at runtime, so the existing Wayland AppImage stays usable.
        let lib = unsafe { Library::new("libX11.so.6") }.map_err(|error| format!("libX11: {error}"))?;
        let ext = unsafe { Library::new("libXext.so.6") }.map_err(|error| format!("libXext: {error}"))?;
        let randr = unsafe { Library::new("libXrandr.so.2") }.ok();
        unsafe {
            let open: OpenDisplay = symbol(&lib, b"XOpenDisplay\0")?;
            let raw = open(ptr::null());
            if raw.is_null() { return Err("XOpenDisplay failed; check DISPLAY".into()); }
            let root_fn: DefaultRoot = symbol(&lib, b"XDefaultRootWindow\0")?;
            let screen_fn: DefaultScreen = symbol(&lib, b"XDefaultScreen\0")?;
            let shape_query: ShapeQuery = symbol(&ext, b"XShapeQueryExtension\0")?;
            let mut event_base = 0;
            let mut error_base = 0;
            if shape_query(raw, &mut event_base, &mut error_base) == 0 {
                let close: CloseDisplay = symbol(&lib, b"XCloseDisplay\0")?;
                close(raw);
                return Err("XShape extension required for a click-through overlay".into());
            }
            let monitors = randr.as_ref().and_then(|lib| symbol(lib, b"XRRGetMonitors\0").ok());
            let free_monitors = randr.as_ref().and_then(|lib| symbol(lib, b"XRRFreeMonitors\0").ok());
            Ok(Self {
                raw, root: root_fn(raw), screen: screen_fn(raw),
                close: symbol(&lib, b"XCloseDisplay\0")?,
                intern: symbol(&lib, b"XInternAtom\0")?,
                get_property: symbol(&lib, b"XGetWindowProperty\0")?,
                geometry: symbol(&lib, b"XGetGeometry\0")?,
                translate: symbol(&lib, b"XTranslateCoordinates\0")?,
                query_tree: symbol(&lib, b"XQueryTree\0")?,
                pointer: symbol(&lib, b"XQueryPointer\0")?,
                attributes: symbol(&lib, b"XGetWindowAttributes\0")?,
                visual_info: symbol(&lib, b"XGetVisualInfo\0")?,
                create_colormap: symbol(&lib, b"XCreateColormap\0")?,
                create_window: symbol(&lib, b"XCreateWindow\0")?,
                change_property: symbol(&lib, b"XChangeProperty\0")?,
                select: symbol(&lib, b"XSelectInput\0")?,
                map: symbol(&lib, b"XMapWindow\0")?,
                unmap: symbol(&lib, b"XUnmapWindow\0")?,
                destroy: symbol(&lib, b"XDestroyWindow\0")?,
                raise: symbol(&lib, b"XRaiseWindow\0")?,
                configure: symbol(&lib, b"XConfigureWindow\0")?,
                free_color: symbol(&lib, b"XFreeColormap\0")?,
                free: symbol(&lib, b"XFree\0")?,
                flush: symbol(&lib, b"XFlush\0")?,
                pending: symbol(&lib, b"XPending\0")?,
                next_event: symbol(&lib, b"XNextEvent\0")?,
                atom_name: symbol(&lib, b"XGetAtomName\0")?,
                selection_owner: symbol(&lib, b"XGetSelectionOwner\0")?,
                set_selection_owner: symbol(&lib, b"XSetSelectionOwner\0")?,
                shape: symbol(&ext, b"XShapeCombineRectangles\0")?,
                monitors, free_monitors, _randr: randr, _lib: lib, _ext: ext,
            })
        }
    }

    fn atom(&self, name: &str) -> Xid {
        let name = CString::new(name).expect("atom without NUL");
        unsafe { (self.intern)(self.raw, name.as_ptr(), 0) }
    }

    fn values(&self, window: Xid, property: Xid, len: CLongAlias) -> Vec<Xid> {
        let (mut kind, mut format, mut count, mut after) = (0, 0, 0, 0);
        let mut data: *mut c_uchar = ptr::null_mut();
        let status = unsafe { (self.get_property)(
            self.raw, window, property, 0, len, 0, 0, &mut kind, &mut format, &mut count, &mut after, &mut data,
        ) };
        if status != 0 || data.is_null() { return Vec::new(); }
        let values = if format == 32 {
            unsafe { std::slice::from_raw_parts(data.cast::<Xid>(), (count as usize).min(65536)).to_vec() }
        } else { Vec::new() };
        unsafe { (self.free)(data.cast()) };
        values
    }

    fn atom_text(&self, atom: Xid) -> String {
        if atom == 0 { return String::new(); }
        unsafe {
            let name = (self.atom_name)(self.raw, atom);
            if name.is_null() { return String::new(); }
            let text = CStr::from_ptr(name).to_string_lossy().into_owned();
            (self.free)(name.cast());
            text
        }
    }

    fn rect(&self, window: Xid) -> Option<Area> {
        unsafe {
            let (mut root, mut x, mut y, mut w, mut h, mut border, mut depth) = (0, 0, 0, 0, 0, 0, 0);
            if (self.geometry)(self.raw, window, &mut root, &mut x, &mut y, &mut w, &mut h, &mut border, &mut depth) == 0 { return None; }
            let (mut gx, mut gy, mut child) = (0, 0, 0);
            if (self.translate)(self.raw, window, self.root, 0, 0, &mut gx, &mut gy, &mut child) == 0 { return None; }
            Some(Area { x: gx as f64, y: gy as f64, w: w as f64, h: h as f64 })
        }
    }

    fn viewable(&self, window: Xid) -> bool {
        unsafe {
            let mut attrs = std::mem::MaybeUninit::<WindowAttributes>::uninit();
            (self.attributes)(self.raw, window, attrs.as_mut_ptr()) != 0
                && attrs.assume_init().map_state == IS_VIEWABLE
        }
    }

    fn top_level(&self, mut window: Xid) -> Xid {
        for _ in 0..12 {
            let (mut root, mut parent, mut children, mut n) = (0, 0, ptr::null_mut(), 0);
            let status = unsafe { (self.query_tree)(self.raw, window, &mut root, &mut parent, &mut children, &mut n) };
            if !children.is_null() { unsafe { (self.free)(children.cast()) }; }
            if status == 0 || parent == 0 || parent == self.root { break; }
            window = parent;
        }
        window
    }

    fn cursor(&self) -> V2 {
        unsafe {
            let (mut root, mut child, mut x, mut y, mut wx, mut wy, mut mask) = (0, 0, 0, 0, 0, 0, 0);
            if (self.pointer)(self.raw, self.root, &mut root, &mut child, &mut x, &mut y, &mut wx, &mut wy, &mut mask) == 0 {
                V2::new(-1e6, -1e6)
            } else {
                V2::new(x as f32, y as f32)
            }
        }
    }

    fn root_size(&self) -> (u32, u32) {
        let r = self.rect(self.root).unwrap_or(Area { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 });
        (r.w as u32, r.h as u32)
    }

    fn displays(&self, work: Option<Area>) -> Vec<Display> {
        let mut screens = Vec::new();
        if let (Some(get), Some(free)) = (self.monitors, self.free_monitors) {
            let mut count = 0;
            let monitors = unsafe { get(self.raw, self.root, 1, &mut count) };
            if !monitors.is_null() {
                for monitor in unsafe { std::slice::from_raw_parts(monitors, count.max(0) as usize) } {
                    if monitor.width < 1 || monitor.height < 1 { continue; }
                    let name = self.atom_text(monitor.name);
                    screens.push((name, Area {
                        x: monitor.x as f64, y: monitor.y as f64,
                        w: monitor.width as f64, h: monitor.height as f64,
                    }));
                }
                unsafe { free(monitors) };
            }
        }
        if screens.is_empty() {
            let (w, h) = self.root_size();
            screens.push(("X11-0".into(), Area { x: 0.0, y: 0.0, w: w as f64, h: h as f64 }));
        }
        screens.into_iter().map(|(name, bounds)| {
            let work = work.map(|work| intersect(bounds, work)).filter(|r| r.w > 0.0 && r.h > 0.0).unwrap_or(bounds);
            Display { name, x: bounds.x, y: bounds.y, w: bounds.w, h: bounds.h, ax: work.x, ay: work.y, aw: work.w, ah: work.h }
        }).collect()
    }

    /// EGL_NATIVE_VISUAL_ID must resolve to an X11 Visual with alpha.
    /// Merely requesting EGL_ALPHA_SIZE=8 is insufficient on Mesa: the
    /// first matching config can be RGB depth 24.
    fn visual_depth(&self, visual_id: u32) -> Option<c_int> {
        unsafe {
            let mut template = VisualInfo {
                visual: ptr::null_mut(), visualid: visual_id as Xid, screen: self.screen,
                depth: 0, class: 0, red_mask: 0, green_mask: 0,
                blue_mask: 0, colormap_size: 0, bits_per_rgb: 0,
            };
            let mut count = 0;
            let list = (self.visual_info)(self.raw, VISUAL_ID_MASK, &mut template, &mut count);
            if list.is_null() || count == 0 {
                if !list.is_null() { (self.free)(list.cast()); }
                return None;
            }
            let depth = (*list).depth;
            (self.free)(list.cast());
            Some(depth)
        }
    }

    fn make_overlay(&self, gpu: &Gpu, bounds: &Area, atoms: &Atoms) -> Result<(Xid, Xid, Surface), String> {
        unsafe {
            let mut template = VisualInfo {
                visual: ptr::null_mut(), visualid: gpu.x11_visual_id()? as Xid, screen: self.screen,
                depth: 0, class: 0, red_mask: 0, green_mask: 0, blue_mask: 0, colormap_size: 0, bits_per_rgb: 0,
            };
            let mut count = 0;
            let list = (self.visual_info)(self.raw, VISUAL_ID_MASK, &mut template, &mut count);
            if list.is_null() || count == 0 { return Err("EGL selected a visual not present on this X screen".into()); }
            let visual = (*list).visual;
            let depth = (*list).depth;
            (self.free)(list.cast());
            if depth != 32 {
                return Err(format!("X11 compositor needs a 32-bit ARGB visual; EGL selected {depth} bits"));
            }
            let colormap = (self.create_colormap)(self.raw, self.root, visual, 0);
            let mut attrs = SetWindowAttributes {
                background_pixel: 0, border_pixel: 0, override_redirect: 1, colormap,
                event_mask: STRUCTURE_NOTIFY_MASK, ..Default::default()
            };
            let mask = CW_BACK_PIXEL | CW_BORDER_PIXEL | CW_OVERRIDE_REDIRECT | CW_COLORMAP | CW_EVENT_MASK;
            let window = (self.create_window)(
                self.raw, self.root, bounds.x as c_int, bounds.y as c_int, bounds.w as c_uint,
                bounds.h as c_uint, 0, depth, INPUT_OUTPUT, visual, mask, &mut attrs,
            );
            if window == 0 { (self.free_color)(self.raw, colormap); return Err("XCreateWindow failed".into()); }
            // Empty INPUT shape is essential: otherwise a full-screen overlay eats every click.
            (self.shape)(self.raw, window, SHAPE_INPUT, 0, 0, ptr::null(), 0, 0, 0);
            let role = atoms.notification;
            (self.change_property)(self.raw, window, atoms.window_type, atoms.atom_type, 32, PROP_MODE_REPLACE,
                (&role as *const Xid).cast::<c_uchar>(), 1);
            let name = b"Snowhush X11\0";
            (self.change_property)(self.raw, window, atoms.net_name, atoms.utf8, 8, PROP_MODE_REPLACE, name.as_ptr(), (name.len() - 1) as c_int);
            (self.map)(self.raw, window);
            (self.flush)(self.raw);
            match gpu.surface_x11(window as u32, bounds.w as i32, bounds.h as i32) {
                Ok(surface) => Ok((window, colormap, surface)),
                Err(error) => {
                    (self.destroy)(self.raw, window);
                    (self.free_color)(self.raw, colormap);
                    Err(error)
                }
            }
        }
    }

    fn restack(&self, window: Xid, report: &Report, behind: bool, atoms: &Atoms) {
        unsafe {
            if behind {
                // Clients are usually reparented by Muffin. Stack relative to the WM FRAME,
                // not the client, because XConfigureWindow requires siblings of the overlay.
                if let Some(lowest) = self.values(self.root, atoms.stacking, 8192).into_iter()
                    .find(|&id| self.viewable(id) && !self.values(id, atoms.window_type, 16).contains(&atoms.desktop))
                {
                    let sibling = self.top_level(lowest);
                    let mut changes = WindowChanges { sibling, stack_mode: BELOW, ..Default::default() };
                    (self.configure)(self.raw, window, CW_SIBLING | CW_STACK_MODE, &mut changes);
                }
            } else {
                let _ = report;
                (self.raise)(self.raw, window);
            }
            (self.flush)(self.raw);
        }
    }
}

impl Drop for Xlib {
    fn drop(&mut self) { unsafe { (self.close)(self.raw); } }
}

#[derive(Clone)]
struct Atoms {
    stacking: Xid, workarea: Xid, current: Xid, desktop_of: Xid,
    state: Xid, fullscreen: Xid, maximized_h: Xid, maximized_v: Xid,
    above: Xid, hidden: Xid, frame: Xid, window_type: Xid, desktop: Xid,
    dock: Xid, notification: Xid, atom_type: Xid, utf8: Xid, net_name: Xid,
}
impl Atoms {
    fn new(x: &Xlib) -> Atoms {
        let a = |n| x.atom(n);
        Atoms {
            stacking: a("_NET_CLIENT_LIST_STACKING"), workarea: a("_NET_WORKAREA"),
            current: a("_NET_CURRENT_DESKTOP"), desktop_of: a("_NET_WM_DESKTOP"),
            state: a("_NET_WM_STATE"), fullscreen: a("_NET_WM_STATE_FULLSCREEN"),
            maximized_h: a("_NET_WM_STATE_MAXIMIZED_HORZ"), maximized_v: a("_NET_WM_STATE_MAXIMIZED_VERT"),
            above: a("_NET_WM_STATE_ABOVE"), hidden: a("_NET_WM_STATE_HIDDEN"), frame: a("_NET_FRAME_EXTENTS"),
            window_type: a("_NET_WM_WINDOW_TYPE"), desktop: a("_NET_WM_WINDOW_TYPE_DESKTOP"),
            dock: a("_NET_WM_WINDOW_TYPE_DOCK"), notification: a("_NET_WM_WINDOW_TYPE_NOTIFICATION"),
            atom_type: a("ATOM"), utf8: a("UTF8_STRING"), net_name: a("_NET_WM_NAME"),
        }
    }
}

fn intersect(a: Area, b: Area) -> Area {
    let x = a.x.max(b.x);
    let y = a.y.max(b.y);
    Area { x, y, w: (a.right().min(b.right()) - x).max(0.0), h: (a.bottom().min(b.bottom()) - y).max(0.0) }
}

fn snapshot(x: &Xlib, atoms: &Atoms) -> Report {
    let current = x.values(x.root, atoms.current, 1).first().copied().unwrap_or(0);
    let work = {
        let all = x.values(x.root, atoms.workarea, 128);
        let slice = all.get(current as usize * 4..current as usize * 4 + 4).or_else(|| all.get(0..4));
        slice.map(|s| Area { x: s[0] as i32 as f64, y: s[1] as i32 as f64, w: s[2] as f64, h: s[3] as f64 })
    };
    let screens = x.displays(work);
    let mut report = Report { screens, desktop: current.to_string(), ..Default::default() };
    let clients = x.values(x.root, atoms.stacking, 8192);
    // EWMH stacking order is bottom-to-top; physics wants the frontmost first.
    for id in clients.into_iter().rev() {
        let wid = format!("x11:{id}");
        let desk = x.values(id, atoms.desktop_of, 1).first().copied().unwrap_or(current);
        if desk != current && desk != 0xffff_ffff {
            report.elsewhere.push(wid);
            continue;
        }
        if !x.viewable(id) { continue; }
        let states = x.values(id, atoms.state, 32);
        if states.contains(&atoms.hidden) { continue; }
        let types = x.values(id, atoms.window_type, 16);
        if types.contains(&atoms.desktop) { continue; }
        let Some(mut rect) = x.rect(id) else { continue; };
        // XGetGeometry gives the client rect. The simulation needs the outside of
        // Muffin's frame to put snow on the titlebar rather than in its middle.
        let extents = x.values(id, atoms.frame, 4);
        if extents.len() == 4 {
            rect.x -= extents[0] as f64;
            rect.y -= extents[2] as f64;
            rect.w += (extents[0] + extents[1]) as f64;
            rect.h += (extents[2] + extents[3]) as f64;
        }
        if rect.w <= 0.0 || rect.h <= 0.0 { continue; }
        if types.contains(&atoms.dock) {
            report.panels.push(Panel { id: wid, x: rect.x, y: rect.y, w: rect.w, h: rect.h });
            continue;
        }
        let fullscreen = states.contains(&atoms.fullscreen);
        report.windows.push(Window {
            id: wid, class: "X11".into(), x: rect.x, y: rect.y, w: rect.w, h: rect.h,
            solid: true, full: fullscreen, above: states.contains(&atoms.above),
            decorated: extents.len() == 4 && extents.iter().any(|n| *n > 0),
            maximized: states.contains(&atoms.maximized_v) && states.contains(&atoms.maximized_h),
            radius: None,
        });
    }
    // Cinnamon's panel is often drawn by its shell rather than a separate
    // EWMH client. A reserved bottom workarea is still a physical shelf.
    for display in &report.screens {
        let bounds = Area::bounds(display);
        let work = Area::work(display);
        let height = bounds.bottom() - work.bottom();
        if height >= 4.0 && height < bounds.h * 0.4
            && !report.panels.iter().any(|panel| {
                (panel.y + panel.h - bounds.bottom()).abs() < 2.0
                    && panel.x < bounds.right() && panel.x + panel.w > bounds.x
            })
        {
            report.panels.push(Panel {
                id: format!("x11:panel:{}", display.name),
                x: bounds.x, y: work.bottom(), w: bounds.w, h: height,
            });
        }
    }
    report
}

struct Screen {
    name: String,
    bounds: Area,
    window: Xid,
    colormap: Xid,
    surface: Surface,
    simulation: SnowSimulation,
    surfaces: Vec<WindowSurface>,
    shelf: Option<Shelf>,
    shown: bool,
}

impl Screen {
    fn new(x: &Xlib, gpu: &Gpu, display: &Display, atoms: &Atoms, settings: Settings) -> Result<Self, String> {
        let bounds = Area::bounds(display);
        let (window, colormap, surface) = x.make_overlay(gpu, &bounds, atoms)?;
        let mut simulation = SnowSimulation::new(bounds.w as f32, bounds.h as f32, settings.intensity as f32 / 100.0);
        simulation.thaws = settings.melt;
        simulation.accumulates = settings.accumulates();
        simulation.ground_settles = settings.ground;
        simulation.falling_opacity = settings.opacity as f32 / 100.0;
        simulation.set_modes(&settings.picked.modes());
        simulation.set_halloween(settings.picked.modes() == [snowhush_core::simulation::Mode::Snow] && settings.halloween.wanted(light::today()));
        Ok(Screen { name: display.name.clone(), bounds, window, colormap, surface, simulation, surfaces: Vec::new(), shelf: None, shown: true })
    }

    fn update_windows(&mut self, report: &Report, settings: Settings, elapsed: f32) {
        self.simulation.forget_closed(|id| windows::exists(report, id));
        let shelf = report.screens.iter().find(|d| d.name == self.name).and_then(|d| windows::shelf(report, d)).filter(|_| settings.panel);
        self.simulation.shelf = shelf.as_ref().map(|s| s.id);
        self.shelf = shelf;
        let listed = if settings.snow_on_windows { report } else { &Report::default() };
        let surfaces = windows::surfaces(listed, &self.bounds, self.shelf.as_ref(), &self.name);
        self.simulation.covers = windows::covers(listed, &self.bounds);
        if surfaces != self.surfaces {
            let old = &self.surfaces;
            let elsewhere: HashSet<_> = old.iter().map(|surface| surface.id)
                .filter(|id| !surfaces.iter().any(|surface| surface.id == *id)
                    && (windows::elsewhere(report, *id) || windows::fills(report, *id, &self.bounds)))
                .collect();
            self.simulation.update_windows(&surfaces, elapsed, &HashSet::new(), &elsewhere, false);
            self.surfaces = surfaces;
        }
    }

    fn visible(&mut self, x: &Xlib, on: bool) {
        if on == self.shown { return; }
        unsafe {
            if on { (x.map)(x.raw, self.window); } else { (x.unmap)(x.raw, self.window); }
            (x.flush)(x.raw);
        }
        self.shown = on;
    }

    fn destroy(self, x: &Xlib, gpu: &Gpu) {
        gpu.release(self.surface);
        unsafe {
            (x.destroy)(x.raw, self.window);
            (x.free_color)(x.raw, self.colormap);
        }
    }
}

fn synchronize(x: &Xlib, gpu: &Gpu, screens: &mut Vec<Screen>, report: &Report, atoms: &Atoms, settings: Settings) -> Result<(), String> {
    let changed = screens.len() != report.screens.len() || report.screens.iter().any(|d| {
        !screens.iter().any(|s| s.name == d.name && s.bounds == Area::bounds(d))
    });
    if !changed { return Ok(()); }
    for screen in screens.drain(..) { screen.destroy(x, gpu); }
    for display in &report.screens {
        match Screen::new(x, gpu, display, atoms, settings) {
            Ok(screen) => screens.push(screen),
            Err(error) => {
                for screen in screens.drain(..) { screen.destroy(x, gpu); }
                return Err(error);
            }
        }
    }
    log!("X11 screens: {}", screens.len());
    Ok(())
}

fn command(command: Command, settings: &mut Settings, screens: &mut [Screen], x: &Xlib, report: &Report, atoms: &Atoms, quit: &mut bool) {
    match command {
        Command::Quit => *quit = true,
        Command::Intensity(value) => { settings.intensity = (value * 100.0) as u32; for s in screens { s.simulation.set_intensity(value); } }
        Command::FallingOpacity(value) => { settings.opacity = (value * 100.0) as u32; for s in screens { s.simulation.falling_opacity = value; } }
        Command::Ground(on) => { settings.ground = on; for s in screens { s.simulation.ground_settles = on; s.simulation.accumulates = settings.accumulates(); } }
        Command::Panel(on) => { settings.panel = on; for s in screens { s.simulation.accumulates = settings.accumulates(); } }
        Command::Melt(on) => { settings.melt = on; for s in screens { s.simulation.thaws = on; } }
        Command::SnowOnWindows(on) => {
            settings.snow_on_windows = on;
            for s in screens { s.simulation.accumulates = settings.accumulates(); s.surfaces.clear(); }
        }
        Command::BehindWindows(on) => {
            settings.behind_windows = on;
            for s in screens { x.restack(s.window, report, on, atoms); }
        }
        Command::OverFullScreen(on) => settings.over_full_screen = on,
        Command::Performance(on) => settings.max_performance = on,
        Command::ShowInScreenshots(on) => {
            settings.show_in_screenshots = on;
            log!("X11 screenshots are managed by the screen capture app; X11 has no KWin exclusion hook");
        }
        Command::ClearSnow(calm) => for s in screens {
            let direction = s.simulation.breeze_direction();
            s.simulation.blow_away(direction, calm);
        },
        Command::Halloween(choice) => {
            settings.halloween = choice;
            let halloween = settings.picked.modes() == [snowhush_core::simulation::Mode::Snow] && choice.wanted(light::today());
            for s in screens { s.simulation.set_halloween(halloween); }
        }
        Command::Mode { picked, calm } => {
            settings.picked = picked;
            let modes = picked.modes();
            for s in screens {
                let direction = s.simulation.breeze_direction();
                s.simulation.switch_modes(&modes, direction, calm);
                s.simulation.set_halloween(modes == [snowhush_core::simulation::Mode::Snow] && settings.halloween.wanted(light::today()));
            }
        }
        Command::Locked(_) | Command::Power(_) | Command::Backdrops(_) | Command::Light(_) => {}
    }
}


/// Move a window's drift to the monitor owning most of its titlebar. Window IDs
/// stay constant across workspace changes; the physics keeps dormant piles.
fn transfer_windows(screens: &mut [Screen], report: &Report, owners: &mut HashMap<WindowId, String>) {
    let mut next = HashMap::new();
    for window in &report.windows {
        let Some(owner) = windows::owner(report, window) else { continue };
        let id = windows::window_id(&window.id);
        if let Some(old) = owners.get(&id).filter(|old| old.as_str() != owner)
            && let (Some(from), Some(to)) = (
                screens.iter().position(|s| s.name == *old),
                screens.iter().position(|s| s.name == owner),
            )
            && let Some(snow) = screens[from].simulation.take_window(id)
        {
            let a = screens[from].bounds;
            let b = screens[to].bounds;
            screens[to].simulation.adopt_window(
                id, snow, Mapping::shift(V2::new((a.x - b.x) as f32, (b.bottom() - a.bottom()) as f32)),
            );
            screens[from].surfaces.clear();
            screens[to].surfaces.clear();
        }
        next.insert(id, owner.to_owned());
    }
    *owners = next;
}

/// A drift spanning two monitors is drawn and collided with on both, although
/// only one simulation owns it. This also prevents a hard cut at monitor edges.
fn give_borrowed(screens: &mut [Screen], report: &Report, owners: &HashMap<WindowId, String>, on: bool) {
    let all: Vec<Vec<Borrowed>> = (0..screens.len()).map(|index| {
        let here = screens[index].bounds;
        let mut borrowed = Vec::new();
        if !on { return borrowed; }
        for window in &report.windows {
            let id = windows::window_id(&window.id);
            let Some(owner) = owners.get(&id).filter(|owner| **owner != screens[index].name) else { continue };
            let near = window.x < here.right() && window.x + window.w > here.x
                && window.y - 300.0 < here.bottom() && window.y + 20.0 > here.y;
            let Some(home) = screens.iter().find(|screen| screen.name == *owner).filter(|_| near) else { continue };
            let Some(lent) = home.simulation.lend(id) else { continue };
            let there = home.bounds;
            let lent = lent.mapped(Mapping::shift(
                V2::new((there.x - here.x) as f32, (here.bottom() - there.bottom()) as f32),
            ));
            let [left, bottom, right, top] = lent.bounds();
            if right > 0.0 && left < here.w as f32 && top > 0.0 && bottom < here.h as f32 {
                borrowed.push(lent);
            }
        }
        borrowed
    }).collect();
    for (screen, borrowed) in screens.iter_mut().zip(all) {
        screen.simulation.set_borrowed(borrowed);
    }
}

/// Pass airborne particles between adjacent monitors instead of deleting them
/// at a viewport boundary; return snow caught by a borrowed window to its owner.
fn pass_snow(screens: &mut [Screen], owners: &HashMap<WindowId, String>) {
    for index in 0..screens.len() {
        let here = screens[index].bounds;
        let caught = screens[index].simulation.take_caught();
        for hit in caught {
            let Some(owner) = owners.get(&hit.id) else { continue };
            let Some(home) = screens.iter_mut().find(|s| s.name == *owner) else { continue };
            let shift = V2::new((here.x - home.bounds.x) as f32, 0.0);
            home.simulation.catch(&hit, Mapping::shift(shift));
        }
        let mut passed: Vec<(usize, Vec<Stray>)> = Vec::new();
        for stray in screens[index].simulation.take_strays() {
            let point = stray.position();
            let (x, y) = (here.x + point.x as f64, here.bottom() - point.y as f64);
            let Some(next) = screens.iter().enumerate().position(|(other, s)| {
                let there = s.bounds;
                other != index && s.shown && x >= there.x && x < there.right()
                    && y >= there.y && y < there.bottom()
            }) else { continue };
            match passed.iter_mut().find(|(other, _)| *other == next) {
                Some((_, strays)) => strays.push(stray),
                None => passed.push((next, vec![stray])),
            }
        }
        for (next, strays) in passed {
            let there = screens[next].bounds;
            let shift = V2::new((here.x - there.x) as f32, (there.bottom() - here.bottom()) as f32);
            screens[next].simulation.adopt_strays(strays, Mapping::shift(shift));
        }
    }
}

/// Start after main has selected an X11 session. Only affects the X11 path; the
/// original Wayland runner and its layers remain unchanged.
pub fn run() -> Result<(), String> {
    let x = Xlib::connect()?;
    let atoms = Atoms::new(&x);
    let mut gpu = Gpu::new_x11(x.raw, |visual_id| x.visual_depth(visual_id) == Some(32))?;
    let mut settings = Settings::load();
    let mut report = snapshot(&x, &atoms);
    let mut screens = Vec::new();
    let mut owners: HashMap<WindowId, String> = HashMap::new();
    synchronize(&x, &gpu, &mut screens, &report, &atoms, settings)?;
    let instance = x.atom("_SNOWHUSH_X11_INSTANCE");
    if unsafe { (x.selection_owner)(x.raw, instance) } != 0 {
        for screen in screens.drain(..) { screen.destroy(&x, &gpu); }
        eprintln!("Snowhush is already running on X11.");
        return Ok(());
    }
    if let Some(first) = screens.first() {
        unsafe {
            (x.set_selection_owner)(x.raw, instance, first.window, 0);
            (x.flush)(x.raw);
        }
    }
    crate::log::start();
    unsafe { (x.select)(x.raw, x.root, SUBSTRUCTURE_NOTIFY_MASK | PROPERTY_CHANGE_MASK | STRUCTURE_NOTIFY_MASK); }

    let mut events: EventLoop<Vec<Command>> = EventLoop::try_new().map_err(|e| e.to_string())?;
    let (sender, source) = calloop::channel::channel::<Command>();
    events.handle().insert_source(source, |event, _, commands| {
        if let Event::Msg(command) = event { commands.push(command); }
    }).map_err(|e| e.to_string())?;
    let signals = calloop::signals::Signals::new(&[calloop::signals::Signal::SIGINT, calloop::signals::Signal::SIGTERM])
        .map_err(|e| e.to_string())?;
    events.handle().insert_source(signals, |_, _, commands| commands.push(Command::Quit))
        .map_err(|e| e.to_string())?;

    use ksni::blocking::TrayMethods;
    let mut tray = Tray::new(settings, sender).assume_sni_available(true).spawn().ok();
    let started = Instant::now();
    let mut last_frame = Instant::now();
    let mut last_scan = Instant::now() - Duration::from_secs(1);
    let mut last_monitor = Instant::now();
    let mut last_light = Instant::now() - Duration::from_secs(61);
    let mut last_stacking: Vec<Xid> = Vec::new();
    let mut pending = Vec::new();
    let mut quit = false;
    log!("X11 backend: {} screen(s), visual {}", screens.len(), gpu.x11_visual_id()?);

    while !quit {
        let fps = if settings.max_performance { 120 } else if screens.iter().all(|s| s.simulation.pace() == Pace::Calm) { 30 } else { 60 };
        events.dispatch(Some(Duration::from_millis(1000 / fps)), &mut pending)
            .map_err(|e| e.to_string())?;
        for next in pending.drain(..) { command(next, &mut settings, &mut screens, &x, &report, &atoms, &mut quit); }
        if quit { break; }

        let now = Instant::now();
        // Drain property and configuration events; doing this before the next snapshot
        // avoids showing windows at their previous locations while they're dragged.
        let mut dirty = false;
        unsafe {
            while (x.pending)(x.raw) > 0 {
                let mut event = [0u64; 24]; // XEvent union is 24 longs on 64-bit Linux.
                (x.next_event)(x.raw, event.as_mut_ptr().cast());
                dirty = true;
            }
        }
        if dirty || now.duration_since(last_scan) >= Duration::from_millis(90) {
            let next = snapshot(&x, &atoms);
            let dt = now.duration_since(last_scan).as_secs_f32().min(0.25);
            transfer_windows(&mut screens, &next, &mut owners);
            for s in &mut screens { s.update_windows(&next, settings, dt); }
            report = next;
            last_scan = now;
            let stacking = x.values(x.root, atoms.stacking, 8192);
            if stacking != last_stacking {
                for s in &screens { x.restack(s.window, &report, settings.behind_windows, &atoms); }
                last_stacking = stacking;
            }
        }
        if now.duration_since(last_monitor) >= Duration::from_secs(1) {
            let old: Vec<_> = screens.iter().map(|s| (s.name.clone(), s.bounds)).collect();
            synchronize(&x, &gpu, &mut screens, &report, &atoms, settings)?;
            if old != screens.iter().map(|s| (s.name.clone(), s.bounds)).collect::<Vec<_>>() {
                owners.clear();
                if let Some(first) = screens.first() {
                    unsafe {
                        (x.set_selection_owner)(x.raw, instance, first.window, 0);
                        (x.flush)(x.raw);
                    }
                }
            }
            last_monitor = now;
        }
        if now.duration_since(last_light) >= Duration::from_secs(60) {
            let today = light::today();
            let halloween = settings.picked.modes() == [snowhush_core::simulation::Mode::Snow] && settings.halloween.wanted(today);
            for s in &mut screens { s.simulation.set_halloween(halloween); }
            if let Some(tray) = &tray { tray.update(|tray| tray.today = today); }
            last_light = now;
        }

        give_borrowed(&mut screens, &report, &owners, settings.snow_on_windows);
        let dt = now.duration_since(last_frame).as_secs_f32().clamp(0.001, 0.05);
        last_frame = now;
        let cursor = x.cursor();
        let night = light::night(settings.picked.modes() == [snowhush_core::simulation::Mode::Snow] && settings.halloween.wanted(light::today()));
        for screen in &mut screens {
            let covered = !settings.over_full_screen && windows::covering(&report, &screen.bounds).is_some();
            screen.visible(&x, !covered);
            if covered { continue; }
            screen.simulation.walls = report.screens.iter().find(|d| d.name == screen.name)
                .map(|d| windows::walls(d, &report.screens)).unwrap_or((true, true));
            screen.simulation.neighbours = (!screen.simulation.walls.0, !screen.simulation.walls.1);
            let local = V2::new(cursor.x - screen.bounds.x as f32, screen.bounds.bottom() as f32 - cursor.y);
            screen.simulation.step(dt, local);
            let panel = screen.shelf.as_ref().map(|shelf| {
                let r = windows::local_rect(&shelf.rect, &screen.bounds);
                [r.min_x(), r.min_y(), r.max_x(), r.max_y() - shelf.radius]
            });
            let ambient = Ambient {
                light: light::daylight(),
                shadow_tint: [0.0; 4],
                night, lantern: local,
            };
            gpu.draw(&screen.surface, &screen.simulation, ambient, panel,
                (now - started).as_secs_f32() % 1000.0, 1.0)?;
        }
        pass_snow(&mut screens, &owners);
    }
    if let Some(tray) = tray.take() { tray.shutdown().wait(); }
    for screen in screens.drain(..) { screen.destroy(&x, &gpu); }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workarea_is_clipped_to_monitor() {
        let monitor = Area { x: 1920.0, y: 0.0, w: 1920.0, h: 1200.0 };
        let work = Area { x: 0.0, y: 0.0, w: 3840.0, h: 1160.0 };
        assert_eq!(intersect(monitor, work), Area { x: 1920.0, y: 0.0, w: 1920.0, h: 1160.0 });
    }

    #[test]
    fn non_overlapping_rectangles_have_zero_intersection() {
        let a = Area { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
        let b = Area { x: 1920.0, y: 0.0, w: 1920.0, h: 1080.0 };
        assert_eq!(intersect(a, b).w, 0.0);
    }
}
