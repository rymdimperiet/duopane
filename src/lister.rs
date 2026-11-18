//! The Lister (F3): a full-window viewer for text, code, documents, pictures,
//! PDFs, music and video, in the spirit of Total/Double Commander's lister.
//!
//! Keys: Esc/Q/F3 close · N/P next/previous file · 1 preview, 2 text, 3 hex ·
//! W wrap · Ctrl+F find (Enter = next) · +/-/0 zoom · PgUp/PgDn pages ·
//! Space play/pause · Left/Right seek · Up/Down volume (media).

use crate::lister_load::{self, Loaded, Mode, TextDoc};
use crate::media::{fmt_time, Player};
use crate::pane::human_size;
use crate::theme;
use eframe::egui::{self, pos2, text::LayoutJob, vec2, Align2, Color32, ColorImage, FontId, Key, Modifiers, Rect, Sense, TextFormat, TextureHandle, TextureOptions};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

#[derive(PartialEq, Eq)]
pub enum ListerAction {
    None,
    Close,
    Next,
    Prev,
}

enum Content {
    Loading(Receiver<Loaded>),
    Text(TextDoc),
    Hex(Vec<u8>, bool),
    Image { tex: TextureHandle, orig: [u32; 2] },
    Pdf(PdfView),
    Media(Box<MediaView>),
    Error(String),
}

struct PdfView {
    pages: usize,
    page: usize,
    tex: Option<(usize, TextureHandle)>,
    pending: Option<(usize, Receiver<Result<ColorImage, String>>)>,
    error: Option<String>,
}

struct MediaView {
    player: Player,
    tex: Option<TextureHandle>,
    cover: Option<TextureHandle>,
    cover_rx: Option<Receiver<ColorImage>>,
}

struct Find {
    query: String,
    hit: Option<(usize, usize)>,
    message: String,
    focus: bool,
}

pub struct Lister {
    pub path: PathBuf,
    name: String,
    size: u64,
    pub mode: Mode,
    pub wrap: bool,
    dark: bool,
    content: Content,
    find: Option<Find>,
    /// Pictures/PDF: None = fit to window, otherwise a scale factor.
    zoom: Option<f32>,
    /// Text: scroll here on the next frame.
    want_offset: Option<f32>,
    offset: f32,
    view_h: f32,
    wrap_rows: Option<(usize, Vec<(usize, usize, usize)>)>,
    /// The scale used when fitting to the window (zooming starts from here).
    fit: f32,
    plugins: Vec<crate::settings::ListerPlugin>,
}

impl Lister {
    pub fn open(path: &Path, mode: Mode, wrap: bool, plugins: Vec<crate::settings::ListerPlugin>) -> Lister {
        let dark = !theme::c().light;
        let (tx, rx) = mpsc::channel();
        let p = path.to_path_buf();
        let plug = plugins.clone();
        std::thread::spawn(move || {
            let _ = tx.send(lister_load::load(&p, mode, dark, &plug));
        });
        Lister {
            path: path.to_path_buf(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            mode,
            wrap,
            dark,
            content: Content::Loading(rx),
            find: None,
            zoom: None,
            want_offset: Some(0.0),
            offset: 0.0,
            view_h: 400.0,
            wrap_rows: None,
            fit: 1.0,
            plugins,
        }
    }

    fn reopen(&mut self, mode: Mode) {
        *self = Lister::open(&self.path.clone(), mode, self.wrap, self.plugins.clone());
    }

    /// Draws the lister; returns what the app should do next.
    pub fn ui(&mut self, ctx: &egui::Context, volume: &mut f32) -> ListerAction {
        self.poll(ctx, *volume);
        if self.dark == theme::c().light {
            // Colour scheme switched between light and dark: re-colour the code.
            let mode = self.mode;
            self.reopen(mode);
        }
        let action = self.keys(ctx, volume);
        if action != ListerAction::None {
            return action;
        }
        let th = theme::c();
        let mut action = ListerAction::None;

        egui::TopBottomPanel::top("lister_top")
            .frame(egui::Frame::default().fill(th.header).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&self.name).strong());
                    ui.label(egui::RichText::new(self.describe()).color(th.dim).small());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close  Esc").clicked() {
                            action = ListerAction::Close;
                        }
                        if ui.button("Open in app").on_hover_text("Open with the system's usual app").clicked() {
                            let _ = open::that_detached(&self.path);
                        }
                        if matches!(self.content, Content::Text(_)) && ui.button("Find  Ctrl+F").clicked() {
                            self.open_find();
                        }
                        if matches!(self.content, Content::Text(_)) {
                            let mut w = self.wrap;
                            if ui.checkbox(&mut w, "Wrap  W").changed() {
                                self.wrap = w;
                                self.wrap_rows = None;
                            }
                        }
                        if matches!(self.content, Content::Image { .. } | Content::Pdf(_)) {
                            if ui.button("Fit  0").clicked() {
                                self.zoom = None;
                            }
                            if ui.button("+").clicked() {
                                self.zoom_by(1.25);
                            }
                            if ui.button("−").clicked() {
                                self.zoom_by(0.8);
                            }
                        }
                        ui.separator();
                        for (m, label) in [(Mode::Hex, "Hex  3"), (Mode::Text, "Text  2"), (Mode::Auto, "Preview  1")] {
                            if ui.selectable_label(self.mode == m, label).clicked() && self.mode != m {
                                self.reopen(m);
                            }
                        }
                    });
                });
            });

        egui::TopBottomPanel::bottom("lister_bottom")
            .frame(egui::Frame::default().fill(th.bg).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(f) = &mut self.find {
                        ui.label("Find:");
                        let r = ui.add(egui::TextEdit::singleline(&mut f.query).id(egui::Id::new("lister_find")).desired_width(240.0));
                        if f.focus {
                            r.request_focus();
                            f.focus = false;
                        }
                        let go = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        ui.label(egui::RichText::new(&f.message).color(th.dim).small());
                        if go {
                            self.find_next();
                            ctx.memory_mut(|m| m.request_focus(egui::Id::new("lister_find")));
                        }
                    } else {
                        let hint = match &self.content {
                            Content::Media(_) => "Space play/pause · Left/Right seek 5 s · Up/Down volume · N/P next/previous file · Esc close",
                            Content::Pdf(_) => "PgUp/PgDn page · +/−/0 zoom · N/P next/previous file · Esc close",
                            Content::Image { .. } => "+/−/0 zoom · N/P next/previous file · Esc close",
                            _ => "Ctrl+F find · W wrap · 1/2/3 preview/text/hex · N/P next/previous file · Esc close",
                        };
                        ui.label(egui::RichText::new(hint).color(th.dim).small());
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::default().fill(th.pane).inner_margin(0.0))
            .show(ctx, |ui| match &mut self.content {
                Content::Loading(_) => {
                    ui.centered_and_justified(|ui| ui.label(egui::RichText::new("Loading…").color(th.dim)));
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                Content::Error(e) => {
                    ui.centered_and_justified(|ui| ui.label(egui::RichText::new(e.as_str()).color(th.error)));
                }
                Content::Text(_) | Content::Hex(..) => self.text_ui(ui),
                Content::Image { tex, .. } => image_ui(ui, tex, &mut self.zoom, &mut self.fit),
                Content::Pdf(pdf) => pdf_ui(ui, ctx, pdf, &self.path, &mut self.zoom, &mut self.fit),
                Content::Media(m) => {
                    if let Some(v) = media_ui(ui, ctx, m, *volume) {
                        *volume = v;
                    }
                }
            });
        action
    }

    fn describe(&self) -> String {
        let size = human_size(self.size);
        match &self.content {
            Content::Loading(_) => size,
            Content::Error(_) => size,
            Content::Text(d) => {
                let mut s = format!("{} · {} lines · {size}", d.kind, d.lines.len());
                if !d.note.is_empty() {
                    s.push_str(" · ");
                    s.push_str(&d.note);
                }
                s
            }
            Content::Hex(_, cut) => format!("Hex · {size}{}", if *cut { " · showing the first 16 MB" } else { "" }),
            Content::Image { orig, .. } => {
                let z = self.zoom.map(|z| format!(" · {:.0}%", z * 100.0)).unwrap_or_default();
                format!("Picture · {}×{} · {size}{z}", orig[0], orig[1])
            }
            Content::Pdf(p) => format!("PDF · page {} of {} · {size}", p.page, p.pages),
            Content::Media(m) => {
                let kind = if m.player.video { "Video" } else { "Audio" };
                let len = m.player.duration().map(|d| format!(" · {}", fmt_time(d))).unwrap_or_default();
                format!("{kind}{len} · {size} · {}", m.player.info.summary)
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context, volume: f32) {
        if let Content::Loading(rx) = &self.content {
            let Ok(loaded) = rx.try_recv() else { return };
            self.content = match loaded {
                Loaded::Text(d) => Content::Text(d),
                Loaded::Hex(b, cut) => Content::Hex(b, cut),
                Loaded::Error(e) => Content::Error(e),
                Loaded::Image(img, orig) => {
                    Content::Image { tex: ctx.load_texture("lister-image", img, TextureOptions::LINEAR), orig }
                }
                Loaded::Pdf { pages } => Content::Pdf(PdfView { pages, page: 1, tex: None, pending: None, error: None }),
                Loaded::Media { video } => {
                    let player = Player::new(&self.path, video, volume);
                    if !player.usable() {
                        Content::Error(player.error.clone().unwrap_or_else(|| "Can't play this file.".into()))
                    } else {
                        let cover_rx = (!video && player.info.has_cover).then(|| {
                            let (tx, rx) = mpsc::channel();
                            let p = self.path.clone();
                            std::thread::spawn(move || {
                                if let Some(png) = crate::media::cover_art(&p) {
                                    if let Ok(img) = image::load_from_memory(&png) {
                                        let img = img.thumbnail(800, 800).to_rgba8();
                                        let size = [img.width() as usize, img.height() as usize];
                                        let _ = tx.send(ColorImage::from_rgba_unmultiplied(size, img.as_raw()));
                                    }
                                }
                            });
                            rx
                        });
                        Content::Media(Box::new(MediaView { player, tex: None, cover: None, cover_rx }))
                    }
                }
            };
            self.want_offset = Some(0.0);
        }
    }

    fn zoom_by(&mut self, f: f32) {
        let base = self.zoom.unwrap_or(self.fit);
        self.zoom = Some((base * f).clamp(0.05, 16.0));
    }

    fn open_find(&mut self) {
        match &mut self.find {
            Some(f) => f.focus = true,
            None => self.find = Some(Find { query: String::new(), hit: None, message: String::new(), focus: true }),
        }
    }

    fn keys(&mut self, ctx: &egui::Context, volume: &mut f32) -> ListerAction {
        let typing = ctx.wants_keyboard_input();
        // Keep focus off buttons so Space and Enter stay lister keys.
        if !typing {
            if let Some(id) = ctx.memory(|m| m.focused()) {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
        }
        let pressed = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            if self.find.is_some() {
                self.find = None;
                return ListerAction::None;
            }
            return ListerAction::Close;
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::F)) {
            if matches!(self.content, Content::Text(_)) {
                self.open_find();
            }
            return ListerAction::None;
        }
        if typing {
            return ListerAction::None;
        }
        if pressed(Key::F3) {
            if self.find.as_ref().is_some_and(|f| !f.query.is_empty()) {
                self.find_next();
                return ListerAction::None;
            }
            return ListerAction::Close;
        }
        if pressed(Key::Q) {
            return ListerAction::Close;
        }
        if pressed(Key::N) {
            return ListerAction::Next;
        }
        if pressed(Key::P) {
            return ListerAction::Prev;
        }
        for (k, m) in [(Key::Num1, Mode::Auto), (Key::Num2, Mode::Text), (Key::Num3, Mode::Hex)] {
            if pressed(k) && self.mode != m {
                self.reopen(m);
                return ListerAction::None;
            }
        }
        if pressed(Key::W) {
            self.wrap = !self.wrap;
            self.wrap_rows = None;
        }
        let zoomable = matches!(self.content, Content::Image { .. } | Content::Pdf(_));
        if zoomable {
            if pressed(Key::Plus) || pressed(Key::Equals) {
                self.zoom_by(1.25);
            }
            if pressed(Key::Minus) {
                self.zoom_by(0.8);
            }
            if pressed(Key::Num0) {
                self.zoom = None;
            }
        }
        match &mut self.content {
            Content::Media(m) => {
                if pressed(Key::Space) {
                    m.player.toggle();
                }
                let p = m.player.position();
                if pressed(Key::ArrowLeft) {
                    m.player.seek(p - 5.0);
                }
                if pressed(Key::ArrowRight) {
                    m.player.seek(p + 5.0);
                }
                if pressed(Key::ArrowUp) {
                    *volume = (*volume + 0.1).min(1.0);
                    m.player.set_volume(*volume);
                }
                if pressed(Key::ArrowDown) {
                    *volume = (*volume - 0.1).max(0.0);
                    m.player.set_volume(*volume);
                }
            }
            Content::Pdf(pdf) => {
                if (pressed(Key::PageDown) || pressed(Key::ArrowRight) || pressed(Key::Space)) && pdf.page < pdf.pages {
                    pdf.page += 1;
                }
                if (pressed(Key::PageUp) || pressed(Key::ArrowLeft)) && pdf.page > 1 {
                    pdf.page -= 1;
                }
                if pressed(Key::Home) {
                    pdf.page = 1;
                }
                if pressed(Key::End) {
                    pdf.page = pdf.pages;
                }
            }
            Content::Text(_) | Content::Hex(..) => {
                let row = 18.0;
                let page = (self.view_h - row).max(row);
                let mut target = None;
                if pressed(Key::ArrowDown) {
                    target = Some(self.offset + row * 3.0);
                }
                if pressed(Key::ArrowUp) {
                    target = Some(self.offset - row * 3.0);
                }
                if pressed(Key::PageDown) || pressed(Key::Space) {
                    target = Some(self.offset + page);
                }
                if pressed(Key::PageUp) {
                    target = Some(self.offset - page);
                }
                if pressed(Key::Home) {
                    target = Some(0.0);
                }
                if pressed(Key::End) {
                    target = Some(f32::MAX);
                }
                if let Some(t) = target {
                    self.want_offset = Some(t.max(0.0));
                }
            }
            _ => {}
        }
        ListerAction::None
    }

    fn find_next(&mut self) {
        let (Some(f), Content::Text(doc)) = (&mut self.find, &self.content) else { return };
        let q = f.query.to_lowercase();
        if q.is_empty() {
            return;
        }
        let (start_line, start_col) = match f.hit {
            Some((l, c)) => (l, c + 1),
            None => (0, 0),
        };
        let n = doc.lines.len();
        for k in 0..=n {
            let l = (start_line + k) % n.max(1);
            let line = doc.lines[l].to_lowercase();
            let from = if k == 0 { start_col } else { 0 };
            let from_byte = line.char_indices().nth(from).map(|(b, _)| b).unwrap_or(line.len());
            if let Some(b) = line[from_byte..].find(&q) {
                let col = line[..from_byte + b].chars().count();
                f.hit = Some((l, col));
                f.message = format!("Line {}", l + 1);
                let row = self.row_of(l, col);
                self.want_offset = Some((row as f32 * self.row_h_guess() - self.view_h / 3.0).max(0.0));
                return;
            }
            if n == 0 {
                break;
            }
        }
        f.hit = None;
        f.message = "Not found".into();
    }

    fn row_h_guess(&self) -> f32 {
        // Matches text_ui's row height closely enough for jumping.
        LAST_ROW_H.with(|r| r.get())
    }

    fn row_of(&self, line: usize, col: usize) -> usize {
        match &self.wrap_rows {
            Some((_, rows)) if self.wrap => {
                rows.iter().position(|&(l, a, b)| l == line && col >= a && (col < b || a == b)).unwrap_or(line)
            }
            _ => line,
        }
    }

    fn text_ui(&mut self, ui: &mut egui::Ui) {
        let th = theme::c();
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let row_h = ui.fonts(|f| f.row_height(&font)) + 2.0;
        LAST_ROW_H.with(|r| r.set(row_h));
        let char_w = ui.fonts(|f| f.glyph_width(&font, 'M')).max(1.0);
        let avail_w = ui.available_width();
        let hit = self.find.as_ref().and_then(|f| f.hit.map(|h| (h, f.query.chars().count())));

        let mut area = egui::ScrollArea::new([!self.wrap, true]).auto_shrink([false, false]);
        if let Some(o) = self.want_offset.take() {
            area = area.vertical_scroll_offset(o);
        }
        let out = match &self.content {
            Content::Hex(bytes, _) => {
                let rows = bytes.len().div_ceil(16);
                area.show_rows(ui, row_h, rows, |ui, range| {
                    for r in range {
                        let chunk = &bytes[r * 16..(r * 16 + 16).min(bytes.len())];
                        let mut s = format!("{:08x}  ", r * 16);
                        for i in 0..16 {
                            match chunk.get(i) {
                                Some(b) => s.push_str(&format!("{b:02x} ")),
                                None => s.push_str("   "),
                            }
                            if i == 7 {
                                s.push(' ');
                            }
                        }
                        s.push(' ');
                        s.extend(chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '·' }));
                        let (rect, _) = ui.allocate_exact_size(vec2(char_w * 80.0, row_h), Sense::hover());
                        ui.painter().text(pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, s, font.clone(), th.text);
                    }
                })
            }
            Content::Text(doc) => {
                let digits = doc.lines.len().max(1).to_string().len();
                let gutter = (digits as f32 + 1.0) * char_w + 12.0;
                let cols = (((avail_w - gutter - 24.0) / char_w) as usize).max(10);
                if self.wrap && self.wrap_rows.as_ref().is_none_or(|(c, _)| *c != cols) {
                    let mut rows = Vec::new();
                    for (i, l) in doc.lines.iter().enumerate() {
                        let n = l.chars().count();
                        if n == 0 {
                            rows.push((i, 0, 0));
                        }
                        let mut a = 0;
                        while a < n {
                            rows.push((i, a, (a + cols).min(n)));
                            a += cols;
                        }
                    }
                    self.wrap_rows = Some((cols, rows));
                }
                let n_rows = if self.wrap { self.wrap_rows.as_ref().map_or(0, |(_, r)| r.len()) } else { doc.lines.len() };
                let wrap_rows = if self.wrap { self.wrap_rows.as_ref().map(|(_, r)| r) } else { None };
                area.show_rows(ui, row_h, n_rows, |ui, range| {
                    for r in range {
                        let (line, a, b) = match wrap_rows {
                            Some(rows) => rows[r],
                            None => (r, 0, usize::MAX),
                        };
                        let job = line_job(doc, line, a, b, &font, th.text, hit);
                        let galley = ui.fonts(|f| f.layout_job(job));
                        let w = gutter + galley.size().x + 16.0;
                        let (rect, _) = ui.allocate_exact_size(vec2(w.max(avail_w), row_h), Sense::hover());
                        if hit.is_some_and(|((l, _), _)| l == line) {
                            ui.painter().rect_filled(rect, 0.0, th.accent.gamma_multiply(0.15));
                        }
                        if a == 0 {
                            ui.painter().text(
                                pos2(rect.left() + gutter - 12.0, rect.center().y),
                                Align2::RIGHT_CENTER,
                                (line + 1).to_string(),
                                font.clone(),
                                th.dim.gamma_multiply(0.7),
                            );
                        }
                        ui.painter().galley(pos2(rect.left() + gutter, rect.top() + 1.0), galley, th.text);
                    }
                })
            }
            _ => return,
        };
        self.offset = out.state.offset.y;
        self.view_h = out.inner_rect.height();
    }
}

thread_local! {
    static LAST_ROW_H: std::cell::Cell<f32> = const { std::cell::Cell::new(18.0) };
}

/// Builds the coloured text for characters [a, b) of one line.
fn line_job(doc: &TextDoc, line: usize, a: usize, b: usize, font: &FontId, default: Color32, hit: Option<((usize, usize), usize)>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let hl = hit.and_then(|((l, c), len)| (l == line).then_some((c, c + len)));
    let mut push = |text: &str, color: Color32, start_char: usize| {
        // Split further around the find match so it can be underlined.
        let n = text.chars().count();
        let (h0, h1) = hl.unwrap_or((usize::MAX, usize::MAX));
        let mut cuts = vec![0, n];
        for x in [h0, h1] {
            if x > start_char && x < start_char + n {
                cuts.push(x - start_char);
            }
        }
        cuts.sort();
        cuts.dedup();
        for w in cuts.windows(2) {
            let piece = slice_chars(text, w[0], w[1]);
            let abs = start_char + w[0];
            let mut fmt = TextFormat::simple(font.clone(), color);
            if abs >= h0 && abs < h1 {
                fmt.background = theme::c().selected.gamma_multiply(0.45);
            }
            job.append(piece, 0.0, fmt);
        }
    };
    match doc.spans.as_ref().and_then(|s| s.get(line)) {
        Some(pieces) => {
            let mut pos = 0;
            for (color, text) in pieces {
                let n = text.chars().count();
                let (s, e) = (pos.max(a), (pos + n).min(b));
                if s < e {
                    push(slice_chars(text, s - pos, e - pos), *color, s);
                }
                pos += n;
                if pos >= b {
                    break;
                }
            }
        }
        None => {
            let text = &doc.lines[line];
            let n = text.chars().count();
            let e = b.min(n);
            if a < e {
                push(slice_chars(text, a, e), default, a);
            }
        }
    }
    job
}

fn slice_chars(s: &str, a: usize, b: usize) -> &str {
    let mut it = s.char_indices().map(|(i, _)| i).chain(std::iter::once(s.len()));
    let start = it.clone().nth(a).unwrap_or(s.len());
    let end = it.nth(b).unwrap_or(s.len());
    &s[start..end.max(start)]
}

fn image_ui(ui: &mut egui::Ui, tex: &TextureHandle, zoom: &mut Option<f32>, fit: &mut f32) {
    let size = tex.size_vec2();
    let avail = ui.available_rect_before_wrap();
    *fit = (avail.width() / size.x).min(avail.height() / size.y).min(1.0);
    // Ctrl + mouse wheel zooms.
    let zd = ui.input(|i| i.zoom_delta());
    if zd != 1.0 && ui.rect_contains_pointer(avail) {
        let fit = (avail.width() / size.x).min(avail.height() / size.y).min(1.0);
        *zoom = Some((zoom.unwrap_or(fit) * zd).clamp(0.05, 16.0));
    }
    match *zoom {
        None => {
            let s = (avail.width() / size.x).min(avail.height() / size.y).min(1.0);
            let rect = Rect::from_center_size(avail.center(), size * s);
            ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            ui.allocate_rect(avail, Sense::hover());
        }
        Some(z) => {
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let want = size * z;
                let w = want.x.max(ui.available_width());
                let h = want.y.max(ui.available_height());
                let (outer, _) = ui.allocate_exact_size(vec2(w, h), Sense::drag());
                let rect = Rect::from_center_size(outer.center(), want);
                ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            });
        }
    }
}

fn pdf_ui(ui: &mut egui::Ui, ctx: &egui::Context, pdf: &mut PdfView, path: &Path, zoom: &mut Option<f32>, fit: &mut f32) {
    let th = theme::c();
    // Collect a finished page.
    if let Some((page, rx)) = &pdf.pending {
        if let Ok(res) = rx.try_recv() {
            match res {
                Ok(img) => pdf.tex = Some((*page, ctx.load_texture("lister-pdf", img, TextureOptions::LINEAR))),
                Err(e) => pdf.error = Some(e),
            }
            pdf.pending = None;
        }
    }
    // Ask for the page being looked at.
    let have_page = pdf.tex.as_ref().is_some_and(|(p, _)| *p == pdf.page);
    if !have_page && pdf.pending.as_ref().is_none_or(|(p, _)| *p != pdf.page) {
        let (tx, rx) = mpsc::channel();
        let (p, page) = (path.to_path_buf(), pdf.page);
        let width = (ui.available_width() * ctx.pixels_per_point()).clamp(800.0, 2400.0) as u32;
        std::thread::spawn(move || {
            let _ = tx.send(lister_load::render_pdf_page(&p, page, width));
        });
        pdf.pending = Some((pdf.page, rx));
    }
    if pdf.pending.is_some() {
        ctx.request_repaint_after(Duration::from_millis(50));
    }
    if let Some(e) = &pdf.error {
        ui.colored_label(th.error, e.as_str());
    }
    let Some((_, tex)) = &pdf.tex else {
        ui.centered_and_justified(|ui| ui.label(egui::RichText::new("Rendering page…").color(th.dim)));
        return;
    };
    let size = tex.size_vec2();
    let avail = ui.available_width() - 24.0;
    *fit = avail / size.x;
    let scale = zoom.unwrap_or(*fit);
    egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
        let want = size * scale;
        let (outer, _) = ui.allocate_exact_size(vec2(want.x.max(ui.available_width()), want.y + 24.0), Sense::hover());
        let rect = Rect::from_min_size(pos2(outer.center().x - want.x / 2.0, outer.top() + 12.0), want);
        ui.painter().image(tex.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    });
}

/// Draws the player; returns a new volume when the slider moved.
fn media_ui(ui: &mut egui::Ui, ctx: &egui::Context, m: &mut MediaView, volume: f32) -> Option<f32> {
    let th = theme::c();
    if let Some(frame) = m.player.poll() {
        let img = ColorImage::from_rgba_unmultiplied(m.player.frame_size, &frame);
        match &mut m.tex {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => m.tex = Some(ctx.load_texture("lister-video", img, TextureOptions::LINEAR)),
        }
    }
    if let Some(rx) = &m.cover_rx {
        if let Ok(img) = rx.try_recv() {
            m.cover = Some(ctx.load_texture("lister-cover", img, TextureOptions::LINEAR));
            m.cover_rx = None;
        }
    }
    if m.player.playing || m.cover_rx.is_some() {
        ctx.request_repaint_after(Duration::from_millis(15));
    }
    let mut new_volume = None;
    let controls_h = 64.0;
    let full = ui.available_rect_before_wrap();
    let stage = Rect::from_min_max(full.min, pos2(full.max.x, full.max.y - controls_h));
    let painter = ui.painter_at(full);
    if m.player.video {
        painter.rect_filled(stage, 0.0, Color32::BLACK);
        if let Some(t) = &m.tex {
            let size = t.size_vec2();
            let s = (stage.width() / size.x).min(stage.height() / size.y);
            let r = Rect::from_center_size(stage.center(), size * s);
            painter.image(t.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
    } else {
        // Music: cover art (or a note symbol) and the tags.
        let left = Rect::from_min_size(stage.min + vec2(40.0, 40.0), vec2(280.0, 280.0).min(stage.size() - vec2(80.0, 80.0)).max(vec2(80.0, 80.0)));
        match &m.cover {
            Some(t) => {
                let size = t.size_vec2();
                let s = (left.width() / size.x).min(left.height() / size.y);
                painter.image(t.id(), Rect::from_min_size(left.min, size * s), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                painter.rect_filled(left, 8.0, th.header);
                painter.text(left.center(), Align2::CENTER_CENTER, "♫", FontId::proportional(96.0), th.dim);
            }
        }
        let mut y = left.top();
        let x = left.right() + 32.0;
        for (k, v) in &m.player.info.tags {
            painter.text(pos2(x, y), Align2::LEFT_TOP, k, FontId::proportional(13.0), th.dim);
            painter.text(pos2(x + 110.0, y), Align2::LEFT_TOP, v, FontId::proportional(16.0), th.text);
            y += 28.0;
        }
    }
    // Controls.
    let bar = Rect::from_min_max(pos2(full.min.x, full.max.y - controls_h), full.max);
    ui.painter().rect_filled(bar, 0.0, th.header);
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(12.0, 10.0))).layout(egui::Layout::left_to_right(egui::Align::Center)));
    let label = if m.player.playing { "⏸" } else { "▶" };
    if cui.add(egui::Button::new(egui::RichText::new(label).size(20.0)).min_size(vec2(40.0, 32.0))).clicked() {
        m.player.toggle();
    }
    let pos = m.player.position();
    let dur = m.player.duration();
    cui.label(format!("{} / {}", fmt_time(pos), dur.map(fmt_time).unwrap_or_else(|| "–:––".into())));
    let vol_w = 150.0;
    let track_w = (cui.available_width() - vol_w - 40.0).max(60.0);
    let (track, resp) = cui.allocate_exact_size(vec2(track_w, 20.0), Sense::click_and_drag());
    let line = Rect::from_center_size(track.center(), vec2(track.width(), 6.0));
    cui.painter().rect_filled(line, 3.0, th.border);
    if let Some(d) = dur.filter(|d| *d > 0.0) {
        let frac = (pos / d).clamp(0.0, 1.0) as f32;
        let done = Rect::from_min_max(line.min, pos2(line.left() + line.width() * frac, line.bottom()));
        cui.painter().rect_filled(done, 3.0, th.accent);
        cui.painter().circle_filled(pos2(done.right(), line.center().y), 7.0, th.accent);
        if resp.clicked() || resp.drag_stopped() {
            if let Some(p) = resp.interact_pointer_pos() {
                let f = ((p.x - line.left()) / line.width()).clamp(0.0, 1.0) as f64;
                m.player.seek(f * d);
            }
        }
    }
    cui.add_space(16.0);
    cui.label("🔊");
    let mut v = volume;
    if cui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false)).changed() {
        m.player.set_volume(v);
        new_volume = Some(v);
    }
    if let Some(e) = &m.player.error {
        let p = ui.painter_at(full);
        p.text(pos2(stage.left() + 12.0, stage.bottom() - 12.0), Align2::LEFT_BOTTOM, e, FontId::proportional(13.0), th.error);
    }
    ui.allocate_rect(full, Sense::hover());
    new_volume
}

