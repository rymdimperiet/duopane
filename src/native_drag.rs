//! Drag and drop with other apps, on Wayland.
//!
//! winit (under eframe) can neither start a drag that leaves the window nor
//! receive files dropped on it under Wayland, so this does both directly with the
//! Wayland protocol, on the window's own connection (the way the clipboard crate
//! shares it).
//!
//! **Dragging out.** It binds its own seat and pointer: the compositor sends
//! button presses to every pointer the app has, which gives the press serial
//! Wayland requires before it lets a drag start. As soon as a file drag starts in
//! a pane it's handed to the compositor, which then decides which window is under
//! the pointer (only it knows when another window overlaps ours). While the drag
//! is over our own window, its position comes back as drag events; those are fed
//! to egui as pointer moves and a release (see `App::raw_input_hook`), so drops on
//! the panes work as before.
//!
//! **Dropping in.** Files dragged in from other apps arrive the same way and are
//! handed to egui as dropped files.
//!
//! Our drags offer copy and move. Duopane never deletes anything when one ends:
//! a file manager that takes a "move" (Shift) moves the files itself, as is usual
//! with file links, and anything else just gets a copy. Over our own window, Shift
//! switches the drag to "move" (`set_move`) so the desktop's pointer shows it.
//! Drags from other apps are only ever accepted as copies, so no app deletes its
//! originals expecting us to have moved them.

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_data_device::{self, WlDataDevice};
use wayland_client::protocol::wl_data_device_manager::{DndAction, WlDataDeviceManager};
use wayland_client::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_client::protocol::wl_data_source::{self, WlDataSource};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{event_created_child, Connection, Dispatch, Proxy, QueueHandle, WEnum};

/// With `DUOPANE_DEBUG_DRAG=1`, says what's happening on stderr (for bug reports).
macro_rules! debug {
    ($($t:tt)*) => {
        if std::env::var_os("DUOPANE_DEBUG_DRAG").is_some() {
            eprintln!("[drag] {}", format!($($t)*));
        }
    };
}

const URI_LIST: &str = "text/uri-list";

/// What happened, for the window to act on. Positions are in Wayland's
/// surface coordinates (logical pixels from the window's top-left).
#[derive(Debug, PartialEq)]
pub enum DragEvent {
    /// A drag is over the window. `ours` is false for files from another app.
    Motion { x: f64, y: f64, ours: bool },
    /// The drag left the window (for another window, maybe one on top of ours).
    Leave,
    /// Our own drag was dropped back on our window.
    DropOurs { x: f64, y: f64 },
    /// Files from another app were dropped on the window.
    DropFiles { paths: Vec<PathBuf>, x: f64, y: f64 },
    /// Our own drag ended somewhere else (dropped in another app, or cancelled).
    Ended,
}

/// What the dispatch thread and the window share.
#[derive(Default)]
struct Shared {
    /// The serial of the mouse press that's still held, if one is.
    press: Option<u32>,
    /// True while a drag we started is under way.
    ours: bool,
    /// What our drag offers: the files as a `text/uri-list`, and as plain paths.
    uri_list: String,
    plain: String,
    /// Our own drag while it's over our window (to switch it between copy and move).
    own_offer: Option<WlDataOffer>,
    /// Shift is held: a drop on our window moves.
    moving: bool,
}

/// The drag currently over our window.
struct Hovering {
    offer: WlDataOffer,
    ours: bool,
    accepted: bool,
    x: f64,
    y: f64,
}

struct State {
    shared: Arc<Mutex<Shared>>,
    qh: QueueHandle<State>,
    conn: Connection,
    pointer: Option<WlPointer>,
    hovering: Option<Hovering>,
    tx: mpsc::Sender<DragEvent>,
    ctx: eframe::egui::Context,
}

impl State {
    fn send(&self, e: DragEvent) {
        let _ = self.tx.send(e);
        self.ctx.request_repaint();
    }
}

/// Drag and drop with other apps. `None` from [`NativeDrag::new`] when not on Wayland.
pub struct NativeDrag {
    conn: Connection,
    qh: QueueHandle<State>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    surface: WlSurface,
    shared: Arc<Mutex<Shared>>,
    rx: mpsc::Receiver<DragEvent>,
}

impl NativeDrag {
    /// Hooks into the window's Wayland connection. Returns `None` on X11, Windows,
    /// macOS, or if the compositor lacks what's needed.
    pub fn new(display: RawDisplayHandle, window: RawWindowHandle, ctx: eframe::egui::Context) -> Option<NativeDrag> {
        let (RawDisplayHandle::Wayland(d), RawWindowHandle::Wayland(w)) = (display, window) else { return None };
        // Safety: eframe hands us the live wl_display and wl_surface of its window,
        // which outlive the app.
        let conn = Connection::from_backend(unsafe { Backend::from_foreign_display(d.display.as_ptr().cast()) });
        let surface_id = unsafe { ObjectId::from_ptr(WlSurface::interface(), w.surface.as_ptr().cast()) }.ok()?;
        let surface = WlSurface::from_id(&conn, surface_id).ok()?;

        let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
        let qh = queue.handle();
        // Version 3 brings drag actions (copy / move); older ones still work.
        let manager: WlDataDeviceManager = globals.bind(&qh, 1..=3, ()).ok()?;
        let seat: WlSeat = globals.bind(&qh, 1..=5, ()).ok()?;
        let device = manager.get_data_device(&seat, &qh, ());
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (tx, rx) = mpsc::channel();
        let mut state =
            State { shared: shared.clone(), qh: qh.clone(), conn: conn.clone(), pointer: None, hovering: None, tx, ctx };
        // Learn the seat's capabilities (and so get a pointer) before going on.
        queue.roundtrip(&mut state).ok()?;
        debug!("ready: data device manager v{}, pointer: {}", manager.version(), state.pointer.is_some());

        std::thread::Builder::new()
            .name("wayland-drag".into())
            .spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {})
            .ok()?;
        Some(NativeDrag { conn, qh, manager, device, surface, shared, rx })
    }

    /// True while a mouse button is held over the window (a drag can start).
    pub fn can_start(&self) -> bool {
        self.shared.lock().is_ok_and(|s| s.press.is_some())
    }

    /// Hands `paths` to the desktop as a drag. Must be called while the mouse
    /// button that started it is still held; returns false otherwise.
    pub fn start(&self, paths: &[PathBuf]) -> bool {
        let Ok(mut shared) = self.shared.lock() else { return false };
        let Some(serial) = shared.press else { return false };
        shared.uri_list = paths.iter().map(|p| format!("{}\r\n", file_uri(p))).collect();
        shared.plain = paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
        shared.ours = true;
        drop(shared);
        let source = self.manager.create_data_source(&self.qh, ());
        source.offer(URI_LIST.into());
        source.offer("text/plain;charset=utf-8".into());
        source.offer("text/plain".into());
        if self.manager.version() >= 3 {
            source.set_actions(DndAction::Copy | DndAction::Move);
        }
        self.device.start_drag(Some(&source), &self.surface, None, serial);
        debug!("start_drag with press serial {serial}: {} file(s)", paths.len());
        self.conn.flush().is_ok()
    }

    /// Shift held or not: switches our drag over our own window between copy and
    /// move, so the desktop draws the right pointer (with or without the plus).
    pub fn set_move(&self, moving: bool) {
        let Ok(mut s) = self.shared.lock() else { return };
        if s.moving == moving {
            return;
        }
        s.moving = moving;
        if let Some(offer) = &s.own_offer {
            offer.set_actions(DndAction::Copy | DndAction::Move, preferred(moving));
            let _ = self.conn.flush();
        }
    }

    /// What happened since last asked.
    pub fn events(&self) -> Vec<DragEvent> {
        self.rx.try_iter().collect()
    }
}

fn preferred(moving: bool) -> DndAction {
    if moving { DndAction::Move } else { DndAction::Copy }
}

/// "/a b/c.txt" -> "file:///a%20b/c.txt"
pub fn file_uri(p: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for b in p.as_os_str().as_encoded_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => out.push(*b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The local files in a `text/uri-list` ("file:///a%20b" -> "/a b"); other
/// kinds of links and comment lines are left out.
pub fn parse_uri_list(text: &str) -> Vec<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.strip_prefix("file://"))
        // "file://host/path": only this machine's files ("" or "localhost").
        .filter_map(|l| l.strip_prefix("localhost").or(Some(l)).filter(|p| p.starts_with('/')))
        .map(|p| {
            let b = p.as_bytes();
            let mut out = Vec::with_capacity(b.len());
            let mut i = 0;
            while i < b.len() {
                let hex = |c: u8| (c as char).to_digit(16);
                match (b[i], b.get(i + 1).copied().and_then(hex), b.get(i + 2).copied().and_then(hex)) {
                    (b'%', Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                    }
                    (c, ..) => {
                        out.push(c);
                        i += 1;
                    }
                }
            }
            PathBuf::from(std::ffi::OsString::from_vec(out))
        })
        .collect()
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(_: &mut Self, _: &WlDataDeviceManager, _: <WlDataDeviceManager as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        // Asking for a pointer on a seat that has none is a protocol error (which
        // would end the whole window's connection), so wait until it says it has one.
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(&state.qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        let Ok(mut shared) = state.shared.lock() else { return };
        match event {
            wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                shared.press = Some(serial);
                debug!("press, serial {serial}");
            }
            wl_pointer::Event::Button { .. } => shared.press = None,
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(state: &mut Self, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_data_device::Event::Enter { serial, x, y, id: Some(offer), .. } => {
                let Ok(mut shared) = state.shared.lock() else { return };
                let ours = shared.ours;
                let accepted = ours || offer_has(&offer, URI_LIST);
                // Say we'd take the files, so the pointer shows a drop is possible:
                // ours as copy or move (Shift), other apps' as a copy only.
                offer.accept(serial, accepted.then(|| URI_LIST.to_string()));
                if accepted && offer.version() >= 3 {
                    if ours {
                        offer.set_actions(DndAction::Copy | DndAction::Move, preferred(shared.moving));
                    } else {
                        offer.set_actions(DndAction::Copy, DndAction::Copy);
                    }
                }
                shared.own_offer = ours.then(|| offer.clone());
                drop(shared);
                debug!("enter at {x:.0},{y:.0}: {}", if ours { "our drag" } else if accepted { "files from another app" } else { "nothing we can use" });
                if let Some(old) = state.hovering.replace(Hovering { offer, ours, accepted, x, y }) {
                    old.offer.destroy();
                }
                if accepted {
                    state.send(DragEvent::Motion { x, y, ours });
                }
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(h) = &mut state.hovering {
                    (h.x, h.y) = (x, y);
                    let (ours, accepted) = (h.ours, h.accepted);
                    if accepted {
                        state.send(DragEvent::Motion { x, y, ours });
                    }
                }
            }
            wl_data_device::Event::Leave => {
                if let Ok(mut s) = state.shared.lock() {
                    s.own_offer = None;
                }
                if let Some(h) = state.hovering.take() {
                    h.offer.destroy();
                    debug!("leave");
                    state.send(DragEvent::Leave);
                }
            }
            wl_data_device::Event::Drop => {
                if let Ok(mut s) = state.shared.lock() {
                    s.own_offer = None;
                }
                let Some(h) = state.hovering.take() else { return };
                debug!("drop at {:.0},{:.0}", h.x, h.y);
                if h.ours {
                    // Our own files back on our window: the panes handle it.
                    state.send(DragEvent::DropOurs { x: h.x, y: h.y });
                    finish(&h.offer, &state.conn);
                } else if h.accepted {
                    receive_files(h, state);
                } else {
                    h.offer.destroy();
                }
            }
            // The clipboard's contents: not used here.
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, Mutex::new(Vec::new())),
    ]);
}

/// Whether the other app offers this format.
fn offer_has(offer: &WlDataOffer, mime: &str) -> bool {
    offer.data::<Mutex<Vec<String>>>().is_some_and(|m| m.lock().is_ok_and(|v| v.iter().any(|t| t == mime)))
}

/// Tells the other app the drop is done (as a copy) and lets the offer go.
fn finish(offer: &WlDataOffer, conn: &Connection) {
    if offer.version() >= 3 {
        offer.finish();
    }
    offer.destroy();
    let _ = conn.flush();
}

/// Reads the dropped file list from the other app, on its own thread (the other
/// app writes it after this request reaches it, so waiting here would stall).
fn receive_files(h: Hovering, state: &State) {
    let Ok((mut reader, writer)) = std::io::pipe() else { return h.offer.destroy() };
    h.offer.receive(URI_LIST.into(), std::os::fd::AsFd::as_fd(&writer));
    drop(writer); // the request carries its own copy; ours must close so reading ends
    let _ = state.conn.flush();
    let (tx, ctx, conn) = (state.tx.clone(), state.ctx.clone(), state.conn.clone());
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = reader.read_to_string(&mut text);
        finish(&h.offer, &conn);
        let paths = parse_uri_list(&text);
        debug!("received {} file(s)", paths.len());
        if !paths.is_empty() {
            let _ = tx.send(DragEvent::DropFiles { paths, x: h.x, y: h.y });
            ctx.request_repaint();
        }
    });
}

impl Dispatch<WlDataOffer, Mutex<Vec<String>>> for State {
    fn event(_: &mut Self, _: &WlDataOffer, event: wl_data_offer::Event, mimes: &Mutex<Vec<String>>, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            if let Ok(mut m) = mimes.lock() {
                m.push(mime_type);
            }
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(state: &mut Self, source: &WlDataSource, event: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            // The app the files were dropped on asks for them in one of our formats.
            wl_data_source::Event::Send { mime_type, fd } => {
                debug!("drop target asked for {mime_type}");
                let text = state.shared.lock().map(|s| if mime_type == URI_LIST { s.uri_list.clone() } else { s.plain.clone() });
                if let Ok(text) = text {
                    let _ = std::fs::File::from(fd).write_all(text.as_bytes());
                }
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                debug!("drag ended: {event:?}");
                if let Ok(mut s) = state.shared.lock() {
                    s.ours = false;
                }
                source.destroy();
                state.send(DragEvent::Ended);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn uris_both_ways() {
        let p = Path::new("/home/me/a b/ö%.txt");
        assert_eq!(file_uri(p), "file:///home/me/a%20b/%C3%B6%25.txt");
        let list = format!("# a comment\r\n{}\r\nfile://localhost/etc/hosts\r\nhttps://example.com/x\r\nfile://otherhost/x\r\n", file_uri(p));
        assert_eq!(parse_uri_list(&list), vec![p.to_path_buf(), PathBuf::from("/etc/hosts")]);
    }
}
