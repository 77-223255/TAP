//! X11 + tiny-skia 界面。纯平（flat）：没有渐变、没有高光、没有倒角，
//! 只有黑底、细线、一个旋钮、两条电平、一颗状态灯，其余都在右键菜单里。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Stroke, Transform};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::shm::ConnectionExt as ShmExt;
use x11rb::protocol::xproto::{
    ConnectionExt, CreateGCAux, CreateWindowAux, EventMask, ImageFormat, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_FROM_PARENT;

use crate::capture::Capture;
use crate::ctl::{zoom_index, Ctl, ZOOMS};

/// 100% 缩放下的逻辑尺寸
pub const BASE_W: f32 = 360.0;
pub const BASE_H: f32 = 300.0;

const KNOB_RANGE: f32 = 24.0;
const KNOB_CX: f32 = BASE_W / 2.0;
const KNOB_CY: f32 = BASE_H / 2.0;
const KNOB_R: f32 = 92.0;
/// 电平圈（单圈，取两声道里大的那个），和旋钮同心
const RING: f32 = KNOB_R + 15.0;
/// 增益位置的小刻度，在电平圈外面
const GAIN_MARK: f32 = KNOB_R + 25.0;
/// 行程角：±135°
const A0: f32 = -2.356;
const A1: f32 = 2.356;
const ROW_H: f32 = 20.0;
const SEP_H: f32 = 9.0;

pub fn size_for(scale: f32) -> (u32, u32) {
    (
        (BASE_W * scale).round().max(1.0) as u32,
        (BASE_H * scale).round().max(1.0) as u32,
    )
}

// ------------------------------------------------------------------ 配色
fn bg() -> Color {
    Color::from_rgba8(0x0a, 0x0a, 0x0b, 255)
}
fn ink() -> Color {
    Color::from_rgba8(0xe9, 0xe9, 0xea, 255)
}
fn line() -> Color {
    Color::from_rgba8(0x24, 0x24, 0x26, 255)
}
fn line_hi() -> Color {
    Color::from_rgba8(0x38, 0x38, 0x3b, 255)
}
fn muted() -> Color {
    Color::from_rgba8(0x6d, 0x6d, 0x72, 255)
}
fn accent() -> Color {
    Color::from_rgba8(0xe8, 0x7a, 0x28, 255)
}
fn green() -> Color {
    Color::from_rgba8(0x5c, 0xc9, 0x4a, 255)
}
fn menu_bg() -> Color {
    Color::from_rgba8(0x12, 0x12, 0x14, 255)
}
fn menu_hover() -> Color {
    Color::from_rgba8(0x1e, 0x1e, 0x21, 255)
}

// ------------------------------------------------------------------ 字体
struct Fonts {
    regular: fontdue::Font,
}

impl Fonts {
    fn load() -> Option<Self> {
        let settings = fontdue::FontSettings {
            collection_index: 0,
            ..Default::default()
        };
        let paths = [
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        ];
        let f = paths
            .iter()
            .find_map(|p| fontdue::Font::from_bytes(std::fs::read(p).ok()?, settings).ok())?;
        Some(Self { regular: f })
    }
}

type GlyphKey = (u32, u32);
struct GlyphCache(HashMap<GlyphKey, (fontdue::Metrics, Vec<u8>)>);

impl GlyphCache {
    fn new() -> Self {
        Self(HashMap::with_capacity(512))
    }
    fn get(&mut self, font: &fontdue::Font, ch: char, px: f32) -> &(fontdue::Metrics, Vec<u8>) {
        let key = (ch as u32, (px * 8.0).max(1.0) as u32);
        if !self.0.contains_key(&key) {
            if self.0.len() > 4096 {
                self.0.clear();
            }
            let (m, b) = font.rasterize(ch, px);
            self.0.insert(key, (m, b));
        }
        &self.0[&key]
    }
}

// ------------------------------------------------------------------ 绘制
struct Painter {
    pix: Pixmap,
    fonts: Fonts,
    glyphs: GlyphCache,
    s: f32,
}

impl Painter {
    fn new(w: u32, h: u32, s: f32) -> Option<Self> {
        Some(Self {
            pix: Pixmap::new(w, h)?,
            fonts: Fonts::load()?,
            glyphs: GlyphCache::new(),
            s,
        })
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color) {
        let s = self.s;
        if let Some(r) = tiny_skia::Rect::from_xywh(x * s, y * s, w * s, h * s) {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = false;
            self.pix.fill_rect(r, &p, Transform::identity(), None);
        }
    }

    fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color, width: f32) {
        let s = self.s;
        let mut pb = PathBuilder::new();
        pb.move_to(x0 * s, y0 * s);
        pb.line_to(x1 * s, y1 * s);
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            let st = Stroke {
                width: width * s,
                line_cap: tiny_skia::LineCap::Butt,
                ..Default::default()
            };
            self.pix
                .stroke_path(&path, &p, &st, Transform::identity(), None);
        }
    }

    fn circle(&mut self, cx: f32, cy: f32, r: f32, color: Color) {
        let s = self.s;
        let mut pb = PathBuilder::new();
        pb.push_circle(cx * s, cy * s, r * s);
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            self.pix
                .fill_path(&path, &p, tiny_skia::FillRule::Winding, Transform::identity(), None);
        }
    }

    fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, color: Color, width: f32) {
        let s = self.s;
        let steps = 72;
        let mut pb = PathBuilder::new();
        for i in 0..=steps {
            let t = a0 + (a1 - a0) * (i as f32 / steps as f32);
            let (sx, sy) = (cx * s + t.sin() * r * s, cy * s - t.cos() * r * s);
            if i == 0 {
                pb.move_to(sx, sy);
            } else {
                pb.line_to(sx, sy);
            }
        }
        if let Some(path) = pb.finish() {
            let mut p = Paint::default();
            p.set_color(color);
            p.anti_alias = true;
            let st = Stroke {
                width: width * s,
                line_cap: tiny_skia::LineCap::Butt,
                ..Default::default()
            };
            self.pix
                .stroke_path(&path, &p, &st, Transform::identity(), None);
        }
    }

    fn text(&mut self, x: f32, baseline: f32, size: f32, spacing: f32, s: &str, color: Color) {
        let sc = self.s;
        let px = size * sc;
        let Self {
            pix,
            fonts,
            glyphs,
            ..
        } = self;
        let pw = pix.width() as i32;
        let ph = pix.height() as i32;
        let data = pix.data_mut();
        let cr = (color.red() * 255.0) as u8;
        let cg = (color.green() * 255.0) as u8;
        let cb = (color.blue() * 255.0) as u8;

        let mut pen = x * sc;
        let base = baseline * sc;
        for ch in s.chars() {
            if ch == ' ' {
                pen += fonts.regular.metrics(ch, px).advance_width + spacing * sc;
                continue;
            }
            let (m, bitmap) = glyphs.get(&fonts.regular, ch, px);
            let gx = pen.round() as i32 + m.xmin;
            let gy = base.round() as i32 - (m.ymin + m.height as i32);
            for row in 0..m.height {
                let py = gy + row as i32;
                if py < 0 || py >= ph {
                    continue;
                }
                for col in 0..m.width {
                    let a = bitmap[row * m.width + col];
                    if a == 0 {
                        continue;
                    }
                    let pxx = gx + col as i32;
                    if pxx < 0 || pxx >= pw {
                        continue;
                    }
                    let idx = ((py * pw + pxx) * 4) as usize;
                    let alpha = a as f32 / 255.0;
                    let blend = |dst: u8, src: u8| -> u8 {
                        (dst as f32 * (1.0 - alpha) + src as f32 * alpha).round() as u8
                    };
                    data[idx] = blend(data[idx], cr);
                    data[idx + 1] = blend(data[idx + 1], cg);
                    data[idx + 2] = blend(data[idx + 2], cb);
                    data[idx + 3] = 255;
                }
            }
            pen += m.advance_width + spacing * sc;
        }
    }

    fn text_w_of(&self, s: &str, size: f32, spacing: f32) -> f32 {
        let px = size * self.s;
        let mut w = 0.0;
        for ch in s.chars() {
            w += self.fonts.regular.metrics(ch, px).advance_width + spacing * self.s;
        }
        w / self.s
    }
}

// ------------------------------------------------------------------ 视图数据
pub struct View {
    pub enabled: bool,
    pub running: bool,
    pub gain_db: f32,
    pub meters: [f32; 2],
    pub holds: [f32; 2],
    pub zoom: i32,
    pub active: bool,
    pub error: Option<String>,
}

impl View {
    pub fn from_state(ctl: &Ctl, cap: &Capture) -> Self {
        let err = cap.error();
        Self {
            enabled: ctl.enabled(),
            running: cap.is_running(),
            gain_db: ctl.gain_db(),
            meters: [ctl.meter(0), ctl.meter(1)],
            holds: [ctl.hold(0), ctl.hold(1)],
            zoom: ctl.zoom(),
            active: false,
            error: if err.is_empty() { None } else { Some(err) },
        }
    }
}

// ------------------------------------------------------------------ 右键菜单
#[derive(Clone, Copy, PartialEq)]
pub enum Action {
    ToggleCapture,
    /// 打开 Zoom 的子菜单
    OpenZoom,
    SetZoom(i32),
}

pub struct Row {
    label: String,
    action: Option<Action>,
    on: bool,
}

pub struct Menu {
    x: f32,
    y: f32,
    w: f32,
    rows: Vec<Row>,
    hover: Option<usize>,
    /// Zoom 的子菜单，只在指针停在主菜单的 Zoom 行上时出现
    sub: Option<SubMenu>,
}

pub struct SubMenu {
    x: f32,
    y: f32,
    w: f32,
    rows: Vec<Row>,
    hover: Option<usize>,
}

impl SubMenu {
    fn height(&self) -> f32 {
        8.0 + ROW_H * self.rows.len() as f32
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.height()
    }
    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.contains(x, y) {
            return None;
        }
        let mut cy = self.y + 4.0;
        for (i, _) in self.rows.iter().enumerate() {
            if y >= cy && y < cy + ROW_H {
                return Some(i);
            }
            cy += ROW_H;
        }
        None
    }
}

impl Menu {
    fn height(&self) -> f32 {
        let mut h = 8.0;
        for r in &self.rows {
            h += if r.action.is_some() { ROW_H } else { SEP_H };
        }
        h
    }
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.w && y >= self.y && y <= self.y + self.height()
    }
    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        if !self.contains(x, y) {
            return None;
        }
        let mut cy = self.y + 4.0;
        for (i, r) in self.rows.iter().enumerate() {
            let h = if r.action.is_some() { ROW_H } else { SEP_H };
            if y >= cy && y < cy + h {
                return r.action.map(|_| i);
            }
            cy += h;
        }
        None
    }
}

// ------------------------------------------------------------------ 布局 / 状态
pub struct Layout {
    knob: (f32, f32, f32),
    drag: Option<(f32, f32)>,
    menu: Option<Menu>,
    last_press: Option<(Instant, f32, f32)>,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            knob: (0.0, 0.0, 0.0),
            drag: None,
            menu: None,
            last_press: None,
        }
    }
}

fn knob_hit(l: &Layout, x: f32, y: f32) -> bool {
    let (cx, cy, r) = l.knob;
    (x - cx) * (x - cx) + (y - cy) * (y - cy) <= (r + 10.0) * (r + 10.0)
}

// ------------------------------------------------------------------ GUI 本体
pub struct Gui {
    ctl: Arc<Ctl>,
    cap: Arc<Capture>,
    host: usize,
    quit: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    attached: bool,
}

impl Gui {
    pub fn new(ctl: Arc<Ctl>, cap: Arc<Capture>, host: *const clap_sys::host::clap_host) -> Self {
        Self {
            ctl,
            cap,
            host: host as usize,
            quit: Arc::new(AtomicBool::new(false)),
            thread: None,
            attached: false,
        }
    }

    pub fn attach(&mut self, parent: u32) -> bool {
        if self.attached {
            return true;
        }
        let ctl = self.ctl.clone();
        let cap = self.cap.clone();
        let quit = self.quit.clone();
        let host = self.host;
        let handle = thread::Builder::new()
            .name("tap-gui".into())
            .spawn(move || run_ui(ctl, cap, quit, host, parent));
        match handle {
            Ok(h) => {
                self.thread = Some(h);
                self.attached = true;
                true
            }
            Err(_) => false,
        }
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.ctl.set_zoom_quiet(zoom_index(w as f32 / BASE_W));
    }
}

impl Drop for Gui {
    fn drop(&mut self) {
        self.quit.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn request_host_callback(host: usize) {
    if host == 0 {
        return;
    }
    let h = host as *const clap_sys::host::clap_host;
    unsafe {
        if let Some(f) = (*h).request_callback {
            f(h);
        }
    }
}

/// MIT-SHM 的一段映射内存
struct Shm {
    seg: u32,
    addr: *mut u8,
    len: usize,
}

unsafe impl Send for Shm {}

impl Shm {
    fn new(conn: &RustConnection, len: usize) -> Option<Self> {
        let seg = conn.generate_id().ok()?;
        let shmid = unsafe { libc::shmget(libc::IPC_PRIVATE, len, libc::IPC_CREAT | 0o600) };
        if shmid < 0 {
            return None;
        }
        let addr = unsafe { libc::shmat(shmid, std::ptr::null(), 0) };
        if addr as isize == -1 {
            unsafe { libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut()) };
            return None;
        }
        if conn.shm_attach(seg, shmid as u32, false).ok()?.check().is_err() {
            unsafe {
                libc::shmdt(addr);
                libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut());
            }
            return None;
        }
        unsafe { libc::shmctl(shmid, libc::IPC_RMID, std::ptr::null_mut()) };
        Some(Self {
            seg,
            addr: addr as *mut u8,
            len,
        })
    }

    fn slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.addr, self.len) }
    }
}

impl Drop for Shm {
    fn drop(&mut self) {
        unsafe { libc::shmdt(self.addr as *const libc::c_void) };
    }
}

fn build_menu(ctl: &Ctl, p: &Painter, x: f32, y: f32) -> Menu {
    let rows = vec![
        Row {
            label: "Capture".into(),
            action: Some(Action::ToggleCapture),
            on: ctl.enabled(),
        },
        Row {
            label: format!(
                "Zoom   {:.0}%",
                crate::ctl::zoom_scale(ctl.zoom()) * 100.0
            ),
            action: Some(Action::OpenZoom),
            on: false,
        },
    ];
    let mut w = 0.0f32;
    for r in &rows {
        w = w.max(p.text_w_of(&r.label, 10.5, 0.2));
    }
    let w = w + 40.0;
    let mut menu = Menu {
        x,
        y,
        w,
        rows,
        hover: None,
        sub: None,
    };
    let h = menu.height();
    menu.x = menu.x.clamp(4.0, (BASE_W - w - 4.0).max(4.0));
    menu.y = menu.y.clamp(4.0, (BASE_H - h - 4.0).max(4.0));
    menu
}

/// The Zoom levels of the submenu, anchored on the row of the `Zoom` item.
fn build_submenu(ctl: &Ctl, p: &Painter, menu: &Menu) -> SubMenu {
    let rows: Vec<Row> = ZOOMS
        .iter()
        .enumerate()
        .map(|(index, level)| Row {
            label: format!("{:.0}%", level * 100.0),
            action: Some(Action::SetZoom(index as i32)),
            on: ctl.zoom() == index as i32,
        })
        .collect();
    let mut w = 0.0f32;
    for r in &rows {
        w = w.max(p.text_w_of(&r.label, 10.5, 0.2));
    }
    let w = w + 44.0;
    let row_y = menu.y + 4.0 + ROW_H;
    let mut sub = SubMenu {
        x: menu.x + menu.w,
        y: row_y,
        w,
        rows,
        hover: None,
    };
    let h = sub.height();
    if sub.x + w > BASE_W - 4.0 {
        sub.x = (menu.x - w).max(4.0);
    }
    sub.y = sub.y.clamp(4.0, (BASE_H - h - 4.0).max(4.0));
    sub
}

fn run_ui(ctl: Arc<Ctl>, cap: Arc<Capture>, quit: Arc<AtomicBool>, host: usize, parent: u32) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = conn.setup().roots[screen_num].clone();
    let depth = screen.root_depth;
    let has_shm = conn
        .extension_information(x11rb::protocol::shm::X11_EXTENSION_NAME)
        .ok()
        .flatten()
        .is_some();

    let mut scale = ctl.zoom_scale();
    let (mut w, mut h) = size_for(scale);

    let Ok(win) = conn.generate_id() else { return };
    let aux = CreateWindowAux::new()
        .background_pixel(screen.black_pixel)
        .border_pixel(screen.black_pixel)
        .event_mask(
            EventMask::EXPOSURE
                | EventMask::BUTTON_PRESS
                | EventMask::BUTTON_RELEASE
                | EventMask::POINTER_MOTION
                | EventMask::STRUCTURE_NOTIFY,
        );
    if conn
        .create_window(
            COPY_FROM_PARENT as u8,
            win,
            parent,
            0,
            0,
            w as u16,
            h as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            COPY_FROM_PARENT,
            &aux,
        )
        .is_err()
    {
        return;
    }

    let Ok(gc) = conn.generate_id() else { return };
    let _ = conn.create_gc(
        gc,
        win,
        &CreateGCAux::new()
            .foreground(screen.black_pixel)
            .graphics_exposures(0),
    );
    let _ = conn.map_window(win);
    let _ = conn.flush();

    let Some(mut painter) = Painter::new(w, h, scale) else {
        return;
    };

    let mut shm: Option<Shm> = if has_shm {
        Shm::new(&conn, (w as usize) * (h as usize) * 4)
    } else {
        None
    };
    let mut layout = Layout::default();
    let mut last = Instant::now() - Duration::from_secs(1);

    let mut bars = [0f32; 2];
    let mut holds = [0f32; 2];

    while !quit.load(Ordering::Relaxed) {
        let want = ctl.zoom_scale();
        if (want - scale).abs() > 1e-4 {
            scale = want;
            let (nw, nh) = size_for(scale);
            if let Some(p) = Pixmap::new(nw, nh) {
                painter.pix = p;
                painter.s = scale;
                w = nw;
                h = nh;
                let _ = conn.configure_window(
                    win,
                    &x11rb::protocol::xproto::ConfigureWindowAux::new()
                        .width(w)
                        .height(h),
                );
                shm = if has_shm {
                    Shm::new(&conn, (w as usize) * (h as usize) * 4)
                } else {
                    None
                };
                let _ = conn.flush();
                layout.menu = None;
                last = Instant::now() - Duration::from_secs(1);
            }
        }

        while let Ok(Some(ev)) = conn.poll_for_event() {
            match ev {
                x11rb::protocol::Event::Expose(_) => {
                    last = Instant::now() - Duration::from_secs(1);
                }
                x11rb::protocol::Event::ButtonPress(b) => {
                    let (x, y) = (b.event_x as f32 / scale, b.event_y as f32 / scale);
                    match b.detail {
                        1 => {
                            // 菜单开着就只处理菜单
                            if let Some(m) = layout.menu.take() {
                                if let Some(i) = m.sub.as_ref().and_then(|s| s.hit(x, y)) {
                                    if let Some(a) = m.sub.as_ref().unwrap().rows[i].action {
                                        apply_action(&ctl, a);
                                        request_host_callback(host);
                                    }
                                } else if let Some(i) = m.hit(x, y) {
                                    match m.rows[i].action {
                                        Some(Action::OpenZoom) => {
                                            // 点 Zoom 就留着菜单，子菜单本来就开着
                                            let mut menu = m;
                                            if menu.sub.is_none() {
                                                menu.sub =
                                                    Some(build_submenu(&ctl, &painter, &menu));
                                            }
                                            layout.menu = Some(menu);
                                            continue;
                                        }
                                        Some(a) => {
                                            apply_action(&ctl, a);
                                            request_host_callback(host);
                                        }
                                        None => {}
                                    }
                                }
                                continue;
                            }
                            if knob_hit(&layout, x, y) {
                                // 双击回 0 dB
                                let now = Instant::now();
                                let dbl = matches!(layout.last_press, Some((t, px, py))
                                    if now.duration_since(t) < Duration::from_millis(320)
                                        && (px - x).abs() < 14.0
                                        && (py - y).abs() < 14.0);
                                layout.last_press = Some((now, x, y));
                                if dbl {
                                    ctl.set_gain_db(0.0);
                                    layout.drag = None;
                                    layout.last_press = None;
                                } else {
                                    layout.drag = Some((y, ctl.gain_db()));
                                }
                            }
                        }
                        3 => {
                            // 右键：全局调出菜单（带 Zoom 子菜单）
                            layout.menu = if layout.menu.is_some() {
                                None
                            } else {
                                let mut menu = build_menu(&ctl, &painter, x, y);
                                menu.sub = Some(build_submenu(&ctl, &painter, &menu));
                                Some(menu)
                            };
                            layout.drag = None;
                        }
                        _ => {}
                    }
                }
                x11rb::protocol::Event::MotionNotify(m) => {
                    let (x, y) = (m.event_x as f32 / scale, m.event_y as f32 / scale);
                    if let Some(menu) = layout.menu.as_mut() {
                        menu.hover = menu.hit(x, y);
                        if let Some(sub) = menu.sub.as_mut() {
                            sub.hover = sub.hit(x, y);
                        }
                    } else if let Some((y0, db0)) = layout.drag {
                        let db = (db0 - (y - y0) * (KNOB_RANGE / 90.0))
                            .clamp(-KNOB_RANGE, KNOB_RANGE);
                        let db = if db.abs() < 0.5 { 0.0 } else { db };
                        ctl.set_gain_db(db);
                    }
                }
                x11rb::protocol::Event::ButtonRelease(_) => layout.drag = None,
                x11rb::protocol::Event::DestroyNotify(_) => return,
                x11rb::protocol::Event::Error(e) => {
                    if shm.take().is_some() {
                        eprintln!("tap: X 错误 {e:?}，SHM 上屏关闭，改用 PutImage");
                        last = Instant::now() - Duration::from_secs(1);
                    }
                }
                x11rb::protocol::Event::ConfigureNotify(c) => {
                    if c.width as u32 != w || c.height as u32 != h {
                        ctl.set_zoom_quiet(zoom_index(c.width as f32 / BASE_W));
                    }
                }
                _ => {}
            }
        }

        let fps = if scale >= 3.0 {
            40.0
        } else if scale >= 2.0 {
            60.0
        } else {
            120.0
        };
        let frame = Duration::from_secs_f64(1.0 / fps);
        if last.elapsed() >= frame {
            let dt = last.elapsed().as_secs_f32().min(0.2);
            last = Instant::now();

            let mut v = View::from_state(&ctl, &cap);
            let decay = (-dt / 0.30).exp();
            for ch in 0..2 {
                let peak = v.meters[ch];
                bars[ch] *= decay;
                if peak > bars[ch] {
                    bars[ch] = peak;
                }
                if bars[ch] > holds[ch] {
                    holds[ch] = bars[ch];
                } else {
                    holds[ch] *= (-dt / 1.2).exp();
                }
            }
            v.meters = bars;
            v.holds = holds;
            v.active = v.enabled && v.running;

            paint(&mut painter, &v, &mut layout);
            match shm.as_mut() {
                Some(s) => push_shm(&conn, win, gc, &painter.pix, w, h, s, depth),
                None => push_image(&conn, win, gc, &painter.pix, w, h, depth),
            }
            continue;
        }
        thread::sleep(Duration::from_millis(1));
    }

    let _ = conn.destroy_window(win);
    let _ = conn.free_gc(gc);
    let _ = conn.flush();
}

fn apply_action(ctl: &Arc<Ctl>, a: Action) {
    match a {
        Action::ToggleCapture => ctl.set_enabled(!ctl.enabled()),
        Action::SetZoom(i) => ctl.set_zoom(i),
        // 由菜单自己的点击处理，不会走到这里
        Action::OpenZoom => {}
    }
}

// ------------------------------------------------------------------ 上屏
fn to_bgrx(pix: &Pixmap, dst: &mut [u8]) {
    let src = pix.data();
    for i in 0..(pix.width() * pix.height()) as usize {
        let r = src[i * 4];
        let g = src[i * 4 + 1];
        let b = src[i * 4 + 2];
        let a = src[i * 4 + 3];
        let (r, g, b) = if a == 255 || a == 0 {
            (r, g, b)
        } else {
            let f = 255.0 / a as f32;
            (
                (r as f32 * f).min(255.0) as u8,
                (g as f32 * f).min(255.0) as u8,
                (b as f32 * f).min(255.0) as u8,
            )
        };
        dst[i * 4] = b;
        dst[i * 4 + 1] = g;
        dst[i * 4 + 2] = r;
        dst[i * 4 + 3] = 0;
    }
}

fn push_shm(
    conn: &RustConnection,
    win: u32,
    gc: u32,
    pix: &Pixmap,
    w: u32,
    h: u32,
    shm: &mut Shm,
    depth: u8,
) {
    to_bgrx(pix, shm.slice());
    let _ = conn.shm_put_image(
        win, gc, w as u16, h as u16, 0, 0, w as u16, h as u16, 0, 0, depth, 2, false, shm.seg, 0,
    );
    let _ = conn.flush();
}

fn push_image(conn: &RustConnection, win: u32, gc: u32, pix: &Pixmap, w: u32, h: u32, depth: u8) {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    to_bgrx(pix, &mut buf);
    let _ = conn.put_image(
        ImageFormat::Z_PIXMAP,
        win,
        gc,
        w as u16,
        h as u16,
        0,
        0,
        0,
        depth,
        &buf,
    );
    let _ = conn.flush();
}

// ------------------------------------------------------------------ 画面
fn paint(p: &mut Painter, v: &View, l: &mut Layout) {
    p.rect(0.0, 0.0, BASE_W, BASE_H, bg());

    let cx = KNOB_CX;
    let cy = KNOB_CY;
    let kr = KNOB_R;
    l.knob = (cx, cy, kr);

    // ── 一圈电平（两声道取大的那个）
    let level = db_norm(v.meters[0]).max(db_norm(v.meters[1]));
    let hold = db_norm(v.holds[0]).max(db_norm(v.holds[1]));
    draw_level_ring(p, cx, cy, RING, level, hold);

    // ── 旋钮本体
    p.arc(cx, cy, kr, -3.1416, 3.1416, line(), 1.0);
    let t = ((v.gain_db + KNOB_RANGE) / (KNOB_RANGE * 2.0)).clamp(0.0, 1.0);
    let ang = A0 + (A1 - A0) * t;

    // 指针（老样子）
    p.line(
        cx + ang.sin() * kr * 0.18,
        cy - ang.cos() * kr * 0.18,
        cx + ang.sin() * kr * 0.82,
        cy - ang.cos() * kr * 0.82,
        ink(),
        2.0,
    );
    p.circle(cx, cy, 2.0, ink());

    // 增益位置：电平圈外侧的一小段刻度
    p.line(
        cx + ang.sin() * (GAIN_MARK - 4.0),
        cy - ang.cos() * (GAIN_MARK - 4.0),
        cx + ang.sin() * (GAIN_MARK + 3.0),
        cy - ang.cos() * (GAIN_MARK + 3.0),
        if v.active { accent() } else { muted() },
        2.0,
    );

    // ── 右键菜单画在最上面（主菜单 + Zoom 子菜单）
    if let Some(menu) = &l.menu {
        let h = menu.height();
        p.rect(menu.x, menu.y, menu.w, h, menu_bg());
        p.rect(menu.x, menu.y, menu.w, 1.0, line_hi());
        p.rect(menu.x, menu.y + h - 1.0, menu.w, 1.0, line_hi());
        p.rect(menu.x, menu.y, 1.0, h, line_hi());
        p.rect(menu.x + menu.w - 1.0, menu.y, 1.0, h, line_hi());
        let mut y = menu.y + 4.0;
        for (i, row) in menu.rows.iter().enumerate() {
            if row.action.is_none() {
                p.rect(menu.x + 10.0, y + SEP_H / 2.0, menu.w - 20.0, 1.0, line());
                y += SEP_H;
                continue;
            }
            if menu.hover == Some(i) {
                p.rect(menu.x + 1.0, y, menu.w - 2.0, ROW_H, menu_hover());
            }
            if row.on {
                p.circle(menu.x + 14.0, y + ROW_H / 2.0, 2.5, accent());
            }
            p.text(
                menu.x + 26.0,
                y + ROW_H / 2.0 + 4.0,
                10.5,
                0.2,
                &row.label,
                if row.on { ink() } else { muted() },
            );
            // 有子菜单的行在右边画一个小箭头
            if row.action == Some(Action::OpenZoom) {
                let (cx, cy) = (menu.x + menu.w - 13.0, y + ROW_H / 2.0);
                p.line(cx - 2.0, cy - 3.5, cx + 1.0, cy, muted(), 1.2);
                p.line(cx + 1.0, cy, cx - 2.0, cy + 3.5, muted(), 1.2);
            }
            y += ROW_H;
        }

        // 子菜单贴在 Zoom 行右侧
        if let Some(sub) = &menu.sub {
            let sh = sub.height();
            p.rect(sub.x, sub.y, sub.w, sh, menu_bg());
            p.rect(sub.x, sub.y, sub.w, 1.0, line_hi());
            p.rect(sub.x, sub.y + sh - 1.0, sub.w, 1.0, line_hi());
            p.rect(sub.x, sub.y, 1.0, sh, line_hi());
            p.rect(sub.x + sub.w - 1.0, sub.y, 1.0, sh, line_hi());
            let mut sy = sub.y + 4.0;
            for (i, row) in sub.rows.iter().enumerate() {
                if sub.hover == Some(i) {
                    p.rect(sub.x + 1.0, sy, sub.w - 2.0, ROW_H, menu_hover());
                }
                if row.on {
                    p.circle(sub.x + 14.0, sy + ROW_H / 2.0, 2.5, accent());
                }
                p.text(
                    sub.x + 26.0,
                    sy + ROW_H / 2.0 + 4.0,
                    10.5,
                    0.2,
                    &row.label,
                    if row.on { ink() } else { muted() },
                );
                sy += ROW_H;
            }
        }
    }
}

/// 一圈电平：底圈 + 绿色主体 + 超过 -6 dB 转橙 + 白色峰值刻度
fn draw_level_ring(p: &mut Painter, cx: f32, cy: f32, r: f32, level: f32, hold: f32) {
    p.arc(cx, cy, r, A0, A1, line(), 3.0);
    if level > 0.002 {
        let end = A0 + (A1 - A0) * level;
        let green_end = (A0 + (A1 - A0) * 0.90).min(end);
        p.arc(cx, cy, r, A0, green_end, green(), 3.0);
        if end > green_end + 0.001 {
            p.arc(cx, cy, r, green_end, end, accent(), 3.0);
        }
    }
    if hold > 0.004 {
        let a = A0 + (A1 - A0) * hold;
        p.line(
            cx + a.sin() * (r - 3.0),
            cy - a.cos() * (r - 3.0),
            cx + a.sin() * (r + 3.0),
            cy - a.cos() * (r + 3.0),
            ink(),
            1.0,
        );
    }
}

/// 线性电平 → 0..1（-60 dB .. 0 dB）
fn db_norm(v: f32) -> f32 {
    if v <= 0.000_001 {
        return 0.0;
    }
    let db = 20.0 * v.log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// 把界面渲染成 PNG（README 的预览图就是这么来的）
pub fn render_png(path: &std::path::Path, v: &View) -> Option<()> {
    let scale = crate::ctl::zoom_scale(v.zoom).max(1.0);
    let (w, h) = size_for(scale);
    let mut p = Painter::new(w, h, scale)?;
    let mut l = Layout::default();
    paint(&mut p, v, &mut l);
    p.pix.save_png(path).ok()?;
    Some(())
}

// 预览图里画菜单
pub fn render_png_with_menu(path: &std::path::Path, v: &View, ctl: &Ctl, mx: f32, my: f32) -> Option<()> {
    let scale = crate::ctl::zoom_scale(v.zoom).max(1.0);
    let (w, h) = size_for(scale);
    let mut p = Painter::new(w, h, scale)?;
    let mut l = Layout::default();
    let mut menu = build_menu(ctl, &p, mx, my);
    menu.hover = Some(1);
    menu.sub = Some(build_submenu(ctl, &p, &menu));
    if let Some(sub) = menu.sub.as_mut() {
        sub.hover = Some(ctl.zoom().max(0) as usize);
    }
    l.menu = Some(menu);
    paint(&mut p, v, &mut l);
    p.pix.save_png(path).ok()?;
    Some(())
}
